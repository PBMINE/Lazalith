//! B5: the guest-visible half of a block device.
//!
//! `binstruction.md` §26's first worked example, end to end:
//!
//! ```text
//! Guest block storage
//!  ↓
//! Lazalith block device      ← this file
//!  ↓
//! host storage backend      ← [`BlockBackend`](crate::BlockBackend)
//! ```
//!
//! # What this file is responsible for, and what it is not
//!
//! This device is the **translation layer**, and the whole of the backend boundary
//! is in that word. A guest register access names an *offset in this device's
//! window*. A backend call names a *sector of storage*. The two are different
//! address spaces, they mean different things, and nothing here lets one become
//! the other:
//!
//! - the guest cannot name a sector in a register — it names a data port and
//!   moves bytes through it;
//! - the backend never sees an offset, a register, a privilege, or a `DataAccess`.
//!   Its signature has nowhere to put one.
//!
//! That is why a block device is a device and a backend is not, and it is why
//! adding a `backend()` accessor here would be the mistake this design is
//! arranged to make hard.
//!
//! # The transfer protocol
//!
//! The register window is a *port*, not a buffer, because [`Device`] gives a
//! device `read`/`write` on registers and no access to guest memory. This is the
//! shape a real port-I/O disk has, chosen here for the same reason: it is the only
//! shape that keeps the transfer inside the device contract.
//!
//! ```text
//! read a sector    write SECTOR = n
//!                  write COMMAND = READ        the device asks the backend now
//!                  read DATA 64 times           each read moves 8 bytes
//!
//! write a sector   write SECTOR = n
//!                  write COMMAND = WRITE       the device starts collecting
//!                  write DATA 64 times
//!                  REMAINING reaching 0        the device hands the sector to
//!                                               the backend
//! ```
//!
//! The data port moves whole accesses and refuses one that would cross the end of
//! the sector. It does not pad a short read, because a padded read hands a guest
//! bytes that were never stored and a program cannot tell them from data.
//!
//! # What a snapshot does and does not carry
//!
//! It carries the **device**: its registers, which sector is selected, which way a
//! transfer is going, and the identity of the storage behind it. It does **not**
//! carry the storage, because the storage is a host resource and a machine snapshot
//! that copied a 64 MiB disk would be holding a second copy of it — the same
//! reason a display's snapshot excludes the framebuffer.
//!
//! Two things make the omission safe rather than silent:
//!
//! - the backend's **identity** travels in the snapshot, so restoring onto a
//!   machine whose disk is a different disk is a **refusal**;
//! - the transfer state travels too, so a snapshot taken mid-transfer is *recorded
//!   as mid-transfer* and a restore of it is refused. A half-moved sector is a
//!   register value and some scratch bytes, and a machine restored into one would
//!   have a data port mid-sector with nobody to finish it.
//!
//! `Device::snapshot` cannot refuse — its signature returns `Vec<u8>` — so the
//! honest shape is a faithful record plus a strict restore, rather than a snapshot
//! that lies about being restorable.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use lazalith_isa::DataSize;
use lazalith_types::CycleCount;

use crate::backend::{BackendError, BackendIdentity, BlockBackend, SECTOR_BYTES};
use crate::{Device, DeviceError, DeviceOffset};

/// How many bytes of register space the device occupies.
pub const BLOCK_REGISTER_BYTES: u64 = 64;

/// Sectors the backing storage holds, at offset 0. Read-only.
pub const BLOCK_REGISTER_CAPACITY: DeviceOffset = DeviceOffset::new(0);
/// The sector the next command applies to, at offset 8. Read/write.
pub const BLOCK_REGISTER_SECTOR: DeviceOffset = DeviceOffset::new(8);
/// The command, at offset 16. Write to start; read to learn what the last one did.
pub const BLOCK_REGISTER_COMMAND: DeviceOffset = DeviceOffset::new(16);
/// Bytes of the current transfer not yet moved, at offset 24. Read-only.
pub const BLOCK_REGISTER_REMAINING: DeviceOffset = DeviceOffset::new(24);
/// The data port, at offset 32. Read to take bytes, write to give them.
pub const BLOCK_REGISTER_DATA: DeviceOffset = DeviceOffset::new(32);
/// The device's own status, at offset 40. Read-only.
pub const BLOCK_REGISTER_STATUS: DeviceOffset = DeviceOffset::new(40);

/// `BLOCK_REGISTER_COMMAND`: fetch the selected sector into the device.
pub const COMMAND_READ: u64 = 1;
/// `BLOCK_REGISTER_COMMAND`: collect a sector and hand it to the backend.
pub const COMMAND_WRITE: u64 = 2;

/// `BLOCK_REGISTER_STATUS` bit 0: the storage can be read.
pub const BLOCK_STATUS_READABLE: u64 = 1;
/// `BLOCK_REGISTER_STATUS` bit 1: the storage accepts writes.
pub const BLOCK_STATUS_WRITABLE: u64 = 2;
/// `BLOCK_REGISTER_STATUS` bit 2: the last command or transfer failed.
pub const BLOCK_STATUS_FAILED: u64 = 4;
/// `BLOCK_REGISTER_STATUS` bit 3: a transfer is in progress.
pub const BLOCK_STATUS_BUSY: u64 = 8;

/// The ABI version this device implements.
pub const BLOCK_ABI_VERSION: u64 = 1;

/// How many bytes a block device's snapshot is.
pub const BLOCK_SNAPSHOT_BYTES: usize = 29;

/// Which way a transfer is going, and how far along it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transfer {
    Idle,
    /// The device holds a sector the guest has not taken yet.
    Reading {
        moved: u32,
    },
    /// The device is collecting a sector the guest has not finished giving.
    Writing {
        taken: u32,
    },
}

impl Transfer {
    fn moved(self) -> u32 {
        match self {
            Self::Idle => 0,
            Self::Reading { moved } => moved,
            Self::Writing { taken } => taken,
        }
    }
    fn in_flight(self) -> bool {
        !matches!(self, Self::Idle)
    }
    /// Bytes of the current transfer still to move, and **zero when there is no
    /// transfer at all**.
    ///
    /// The idle case is not the arithmetic case. Deriving this as
    /// `SECTOR_BYTES - moved` and letting `Idle` contribute a `moved` of zero would
    /// report a full sector outstanding for a device that is waiting for a command —
    /// so a program that polls `BLOCK_REGISTER_REMAINING` before issuing one would
    /// wait for a transfer that was never started.
    fn remaining(self) -> u64 {
        match self {
            Self::Idle => 0,
            Self::Reading { .. } | Self::Writing { .. } => {
                SECTOR_BYTES.saturating_sub(u64::from(self.moved()))
            }
        }
    }
    fn encode(self) -> u8 {
        match self {
            Self::Idle => 0,
            Self::Reading { .. } => 1,
            Self::Writing { .. } => 2,
        }
    }
    fn decode(raw: u8, moved: u32) -> Option<Self> {
        match raw {
            0 => Some(Self::Idle),
            1 => Some(Self::Reading { moved }),
            2 => Some(Self::Writing { taken: moved }),
            _ => None,
        }
    }
}

/// Why a block operation was refused.
///
/// Separate from [`DeviceError`] for the same reason `DisplayError` is: a storage
/// problem is a different fact from a bad register access, and a caller that has
/// to match on one of them should be able to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockError {
    /// The host resource behind the device refused.
    ///
    /// **Carried rather than flattened**, so a caller can tell "the disk is full"
    /// from "you asked for the wrong register" — and so a backend's own reason
    /// survives to whoever has to report it.
    Storage(BackendError),
    /// A command number that is not one this device has.
    UnknownCommand(u64),
    /// A command issued while a transfer was already in progress.
    Busy,
    /// A data-port access that would cross the end of the sector.
    ///
    /// Not padded: a short read hands the guest bytes the storage never produced.
    ShortTransfer {
        /// Bytes the access asked for.
        asked: u64,
        /// Bytes actually left in the sector.
        remaining: u64,
    },
    /// A data-port access with no transfer in progress.
    NoTransfer,
    /// A write to storage that does not accept writes.
    NotWritable,
    /// A sector the storage does not hold.
    OutOfRange {
        /// The sector asked for.
        sector: u64,
        /// How many sectors the storage holds.
        sectors: u64,
    },
    /// A snapshot whose device was mid-transfer.
    TransferInFlight,
    /// A snapshot from a machine whose storage was a different backend.
    DifferentBackend {
        /// The backend this device holds.
        device: BackendIdentity,
        /// The backend the snapshot was taken against.
        snapshot: BackendIdentity,
    },
    /// A snapshot that is not exactly this device's own encoding.
    SnapshotShape {
        /// How many bytes this device's `snapshot` produces.
        expected: usize,
        /// How many bytes were offered.
        found: usize,
    },
}

impl fmt::Display for BlockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(source) => write!(f, "the storage refused: {source}"),
            Self::UnknownCommand(command) => write!(f, "{command} is not a block command"),
            Self::Busy => f.write_str("a transfer is already in progress"),
            Self::ShortTransfer { asked, remaining } => write!(
                f,
                "the data port was asked for {asked} bytes with {remaining} left in the sector"
            ),
            Self::NoTransfer => f.write_str("no transfer is in progress"),
            Self::NotWritable => f.write_str("the storage does not accept writes"),
            Self::OutOfRange { sector, sectors } => {
                write!(f, "sector {sector} of a storage holding {sectors} sectors")
            }
            Self::TransferInFlight => {
                f.write_str("the snapshot was taken mid-transfer, which is not a state to resume")
            }
            Self::DifferentBackend { device, snapshot } => write!(
                f,
                "the snapshot was taken against {snapshot:?} and this device holds {device:?}"
            ),
            Self::SnapshotShape { expected, found } => {
                write!(
                    f,
                    "a block snapshot is {expected} bytes, and {found} were offered"
                )
            }
        }
    }
}

impl core::error::Error for BlockError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Storage(source) => Some(source),
            _ => None,
        }
    }
}

impl From<BackendError> for BlockError {
    fn from(source: BackendError) -> Self {
        Self::Storage(source)
    }
}

impl From<BlockError> for DeviceError {
    fn from(source: BlockError) -> Self {
        DeviceError::Block(source)
    }
}

/// A guest-visible block device over a host backend.
///
/// # It has no accessor for its backend
///
/// There is deliberately no `backend()` method. The backend is the host's; the
/// device is the guest's; and the only place they meet is a sector crossing in one
/// direction or the other. A getter here would be a door, and a door is how a host
/// resource becomes a guest interface one refactor after nobody is looking.
#[derive(Debug)]
pub struct BlockDevice {
    backend: Box<dyn BlockBackend>,
    sector: u64,
    transfer: Transfer,
    failed: bool,
    staging: Vec<u8>,
}

impl BlockDevice {
    /// A block device over `backend`.
    ///
    /// The backend is *moved in*. A host that wants the storage afterwards keeps
    /// whatever handle it already had — the bytes for a memory backend, the file
    /// for a file backend.
    pub fn new(backend: Box<dyn BlockBackend>) -> Self {
        let mut staging = Vec::new();
        // Allocated once. A transfer must not be able to fail halfway through for
        // want of memory and leave the device mid-sector.
        let _ = staging.try_reserve_exact(SECTOR_BYTES as usize);
        staging.resize(SECTOR_BYTES as usize, 0);
        Self {
            backend,
            sector: 0,
            transfer: Transfer::Idle,
            failed: false,
            staging,
        }
    }

    /// How many sectors the storage holds.
    pub fn sectors(&self) -> u64 {
        self.backend.capacity() / SECTOR_BYTES
    }

    /// Whether the storage accepts writes.
    pub fn writable(&self) -> bool {
        self.backend.writable()
    }

    /// What the storage is, for a diagnostic.
    ///
    /// A *kind*, not a handle. The point of the boundary is that a caller can ask
    /// what a device is backed by without being able to reach the backing.
    pub fn backend_kind(&self) -> crate::BackendKind {
        self.backend.kind()
    }

    /// The device's own state, for a machine snapshot.
    ///
    /// Five fields, and every one of them is a word this device would otherwise have
    /// to invent on a restore: the selected sector, the failure flag, the storage's
    /// identity, and the direction and progress of any transfer. **The storage is not
    /// among them**, and neither is the staging buffer — a transfer in flight is
    /// recorded as in-flight and refused on restore rather than half-saved.
    pub fn snapshot_device(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(BLOCK_SNAPSHOT_BYTES);
        out.extend_from_slice(&self.sector.to_le_bytes());
        out.extend_from_slice(&u64::from(self.failed).to_le_bytes());
        out.extend_from_slice(&self.backend.identity().raw().to_le_bytes());
        out.push(self.transfer.encode());
        out.extend_from_slice(&self.transfer.moved().to_le_bytes());
        debug_assert_eq!(out.len(), BLOCK_SNAPSHOT_BYTES);
        out
    }

    /// Puts a device snapshot back.
    ///
    /// Refuses a snapshot taken mid-transfer, and one taken against different storage.
    /// Both are refusals rather than warnings because both produce a machine that is
    /// *almost* the one that was saved, which is the failure a snapshot exists to
    /// prevent.
    ///
    /// The field order is the one `snapshot_device` writes: sector, failure, identity,
    /// then the two transfer bytes. It is a fixed layout rather than a tagged encoding
    /// because a machine snapshot is a machine-internal artefact, not a wire format,
    /// and every word in it is already naturally aligned.
    pub fn restore_device(&mut self, bytes: &[u8]) -> Result<(), BlockError> {
        let malformed = || BlockError::SnapshotShape {
            expected: BLOCK_SNAPSHOT_BYTES,
            found: bytes.len(),
        };
        if bytes.len() != BLOCK_SNAPSHOT_BYTES {
            return Err(malformed());
        }
        let sector = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let failed = bytes[8] != 0;
        let snapshot =
            BackendIdentity::from_raw(u64::from_le_bytes(bytes[16..24].try_into().unwrap()));
        let Some(transfer) = Transfer::decode(
            bytes[24],
            u32::from_le_bytes(bytes[25..29].try_into().unwrap()),
        ) else {
            return Err(malformed());
        };
        if transfer.in_flight() {
            return Err(BlockError::TransferInFlight);
        }
        let device = self.backend.identity();
        if snapshot != device {
            return Err(BlockError::DifferentBackend { device, snapshot });
        }
        if sector >= self.sectors() {
            return Err(BlockError::OutOfRange {
                sector,
                sectors: self.sectors(),
            });
        }
        self.sector = sector;
        self.transfer = Transfer::Idle;
        self.failed = failed;
        Ok(())
    }

    fn status(&self) -> u64 {
        let mut status = 0;
        if self.backend.capacity() >= SECTOR_BYTES {
            status |= BLOCK_STATUS_READABLE;
        }
        if self.backend.writable() {
            status |= BLOCK_STATUS_WRITABLE;
        }
        if self.failed {
            status |= BLOCK_STATUS_FAILED;
        }
        if self.transfer.in_flight() {
            status |= BLOCK_STATUS_BUSY;
        }
        status
    }

    fn validate_register_access(offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= BLOCK_REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        Ok(())
    }

    fn start(&mut self, command: u64) -> Result<(), DeviceError> {
        if self.transfer.in_flight() {
            return Err(BlockError::Busy.into());
        }
        if self.sector >= self.sectors() {
            return Err(BlockError::OutOfRange {
                sector: self.sector,
                sectors: self.sectors(),
            }
            .into());
        }
        self.failed = false;
        match command {
            COMMAND_READ => {
                // The backend is asked *now*, not at the first data-port read. A
                // read that fails then fails here, where the guest can read a
                // status bit, rather than sixty-four register reads deep with a
                // half-filled buffer.
                let Self {
                    backend, staging, ..
                } = self;
                backend
                    .read_sector(self.sector, staging)
                    .map_err(BlockError::from)?;
                self.transfer = Transfer::Reading { moved: 0 };
            }
            COMMAND_WRITE => {
                if !self.backend.writable() {
                    return Err(BlockError::NotWritable.into());
                }
                self.transfer = Transfer::Writing { taken: 0 };
            }
            other => return Err(BlockError::UnknownCommand(other).into()),
        }
        Ok(())
    }

    /// Hands a completed read's last word out, and lets the device go idle.
    ///
    /// A read that never went idle would leave `BLOCK_STATUS_BUSY` set forever and
    /// refuse every further command, so the completion of a read has to be a real
    /// transition and not merely a counter reaching zero. The staging buffer is left
    /// alone: it holds the sector until the next command overwrites it, and clearing
    /// it would be a write nobody asked for.
    fn take(&mut self, size: u64) -> Result<u64, DeviceError> {
        let moved = self.transfer.moved();
        let remaining = self.transfer.remaining();
        if remaining < size {
            // Refused rather than padded: a short read hands the guest bytes that
            // were never in the sector.
            self.failed = true;
            return Err(BlockError::ShortTransfer {
                asked: size,
                remaining,
            }
            .into());
        }
        let start = moved as usize;
        let mut word = [0u8; 8];
        word[..size as usize].copy_from_slice(&self.staging[start..start + size as usize]);
        let moved = moved + size as u32;
        self.transfer = if u64::from(moved) >= SECTOR_BYTES {
            Transfer::Idle
        } else {
            Transfer::Reading { moved }
        };
        Ok(u64::from_le_bytes(word))
    }

    fn give(&mut self, size: u64, value: u64) -> Result<(), DeviceError> {
        let taken = self.transfer.moved();
        let remaining = self.transfer.remaining();
        if remaining < size {
            self.failed = true;
            return Err(BlockError::ShortTransfer {
                asked: size,
                remaining,
            }
            .into());
        }
        let start = taken as usize;
        self.staging[start..start + size as usize]
            .copy_from_slice(&value.to_le_bytes()[..size as usize]);
        self.transfer = Transfer::Writing {
            taken: taken + size as u32,
        };
        self.settle_write();
        Ok(())
    }

    /// Hands a completed write to the backend, and records whether it worked.
    ///
    /// A failure here is recorded in the status register rather than raised,
    /// because the guest is already sixty-four register writes deep and the status
    /// bit is the only way left to tell it. "The write happened" and "the storage
    /// refused it" have to be distinguishable, or a guest will assume the first.
    ///
    /// **The backend's own reason is dropped, deliberately.** There is nowhere to
    /// put it: `Device::write` has returned, and a `u64` in a register cannot carry
    /// a `BackendError`. Carrying it would mean either a second read-only register
    /// the guest has to know to consult or a host-side accessor on the device, and
    /// the second of those is a door. A write that failed is therefore visible as
    /// `BLOCK_STATUS_FAILED` and nothing finer — which is a real limitation of a
    /// one-word status port, and is named here rather than left to be discovered
    /// by whoever needs the reason.
    fn settle_write(&mut self) {
        let Transfer::Writing { taken } = self.transfer else {
            return;
        };
        if u64::from(taken) < SECTOR_BYTES {
            return;
        }
        let sector = self.sector;
        let Self {
            backend, staging, ..
        } = self;
        self.transfer = Transfer::Idle;
        match backend.write_sector(sector, staging) {
            Ok(()) => self.failed = false,
            Err(_) => self.failed = true,
        }
    }
}

impl Device for BlockDevice {
    fn address_len(&self) -> u64 {
        BLOCK_REGISTER_BYTES
    }

    fn reset(&mut self) {
        self.sector = 0;
        self.transfer = Transfer::Idle;
        self.failed = false;
        self.backend.reset();
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        Self::validate_register_access(offset, size)?;
        match offset {
            BLOCK_REGISTER_CAPACITY
            | BLOCK_REGISTER_SECTOR
            | BLOCK_REGISTER_COMMAND
            | BLOCK_REGISTER_REMAINING
            | BLOCK_REGISTER_STATUS => Ok(()),
            BLOCK_REGISTER_DATA => {
                // Readable during a *read* transfer and not during a write. Allowing
                // it either way would be wrong: a read mid-write would consume the
                // port and turn the write into a read, and the bytes the guest had
                // already given would be lost with no fault raised anywhere.
                if matches!(self.transfer, Transfer::Reading { .. }) {
                    Ok(())
                } else {
                    Err(BlockError::NoTransfer.into())
                }
            }
            _ => Err(DeviceError::ReadUnsupported),
        }
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        _value: u64,
    ) -> Result<(), DeviceError> {
        Self::validate_register_access(offset, size)?;
        match offset {
            BLOCK_REGISTER_SECTOR | BLOCK_REGISTER_COMMAND => {
                if self.transfer.in_flight() {
                    Err(BlockError::Busy.into())
                } else {
                    Ok(())
                }
            }
            BLOCK_REGISTER_DATA => match self.transfer {
                Transfer::Writing { .. } => Ok(()),
                Transfer::Idle => Err(BlockError::NoTransfer.into()),
                // A read transfer collects nothing. Refusing is right: a guest that
                // thought it was writing would be silently losing data.
                Transfer::Reading { .. } => Err(DeviceError::WriteUnsupported),
            },
            _ => Err(DeviceError::WriteUnsupported),
        }
    }

    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(match offset {
            BLOCK_REGISTER_CAPACITY => self.sectors(),
            BLOCK_REGISTER_SECTOR => self.sector,
            // The command in flight, so a program can ask what the device is doing
            // without tracking it itself. Zero means idle, which is not a command.
            BLOCK_REGISTER_COMMAND => match self.transfer {
                Transfer::Reading { .. } => COMMAND_READ,
                Transfer::Writing { .. } => COMMAND_WRITE,
                Transfer::Idle => 0,
            },
            BLOCK_REGISTER_REMAINING => self.transfer.remaining(),
            BLOCK_REGISTER_DATA => self.take(u64::from(size.bytes()))?,
            _ => self.status(),
        })
    }

    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_write(offset, size, value)?;
        match offset {
            BLOCK_REGISTER_SECTOR => self.sector = value,
            BLOCK_REGISTER_COMMAND => self.start(value)?,
            BLOCK_REGISTER_DATA => self.give(u64::from(size.bytes()), value)?,
            _ => return Err(DeviceError::WriteUnsupported),
        }
        Ok(())
    }

    /// Reports the device's state without moving a transfer.
    ///
    /// The data port is the interesting one: a peek says what the *next* read would
    /// return and leaves the transfer exactly where it was. A peek that advanced the
    /// transfer would be a hidden mutation, which is the one thing a peek is not
    /// allowed to be — and this device has a visible, counted transfer that a
    /// hidden advance would corrupt.
    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        if output.len() != DataSize::Double.bytes() as usize {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: output.len() as u64,
            });
        }
        self.validate_read(offset, DataSize::Double)?;
        let value = match offset {
            BLOCK_REGISTER_CAPACITY => self.sectors(),
            BLOCK_REGISTER_SECTOR => self.sector,
            BLOCK_REGISTER_COMMAND => match self.transfer {
                Transfer::Reading { .. } => COMMAND_READ,
                Transfer::Writing { .. } => COMMAND_WRITE,
                Transfer::Idle => 0,
            },
            BLOCK_REGISTER_REMAINING => self.transfer.remaining(),
            BLOCK_REGISTER_DATA => {
                let moved = self.transfer.moved() as usize;
                let mut word = [0u8; 8];
                let end = (moved + 8).min(self.staging.len());
                word[..end - moved].copy_from_slice(&self.staging[moved..end]);
                u64::from_le_bytes(word)
            }
            _ => self.status(),
        };
        output.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn tick(&mut self, _elapsed: CycleCount) {
        // A block device has no clock. The storage behind it is the host's, and a
        // device that counted cycles would be reporting a rate a guest could measure
        // a transfer against — which would be measuring the host, not the guest.
    }

    /// The device's own state, for a machine snapshot.
    ///
    /// Records a transfer in flight as in-flight rather than refusing, because
    /// [`Device::snapshot`] cannot refuse. The refusal is in
    /// [`Device::restore`], and it is the strict half of the pair: the snapshot
    /// says what was true, and the restore refuses what cannot be resumed.
    fn snapshot(&self) -> Vec<u8> {
        self.snapshot_device()
    }

    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        if self.transfer.in_flight() {
            // Refused here as well as in `restore_device`, because a device
            // *restoring* into a mid-transfer state is the same defect from the
            // other direction.
            return Err(BlockError::TransferInFlight.into());
        }
        self.restore_device(bytes).map_err(DeviceError::from)
    }
}
