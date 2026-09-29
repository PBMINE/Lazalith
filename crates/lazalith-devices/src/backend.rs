//! B5: the host backend boundary.
//!
//! `binstruction.md` §26 asks for four layers, and for the APIs to be
//! Lazalith-specific even though the shape is inspired by QEMU's:
//!
//! ```text
//! Guest
//!  ↓
//! guest-visible device
//!  ↓
//! Lazalith device model
//!  ↓
//! host backend
//! ```
//!
//! # What a backend is, and what it deliberately is not
//!
//! A backend is a **host resource**. It is the bytes on a file, the sectors of an
//! image, the rings of a socket. It is not a device, it is not mapped anywhere, and
//! it has no registers.
//!
//! The boundary between "host resource" and "guest-visible device state" is not a
//! naming convention. It is in the **signatures**:
//!
//! - [`BlockBackend`] methods take a `u64` sector and a byte buffer. Not a
//!   [`DataAccess`](lazalith_cpu::DataAccess), not a
//!   [`PhysicalAddress`](lazalith_types::PhysicalAddress), not a
//!   [`DeviceOffset`](crate::DeviceOffset), not a
//!   [`Privilege`](lazalith_cpu::Privilege). **A backend cannot be handed a guest
//!   address, a register offset or a privilege, because its interface has nowhere
//!   to put one.** A guest register read becomes, at the device, a sector number and
//!   a buffer; that translation is the device's whole job.
//! - A backend is not a [`Device`](crate::Device), and there is no `impl Device`
//!   for any backend. A guest cannot reach one even by accident, because the only
//!   way a machine reaches a device is `DeviceManager`, and a backend is not one.
//! - A device does not expose its backend. There is no `fn backend(&self)`,
//!   because a getter is a door and a door is how a host resource becomes a
//!   guest interface one refactor later.
//!
//! # What a backend knows about, and what it does not
//!
//! It knows its capacity, whether it can be written, and how to move a sector. It
//! does not know what machine it is in, what device asked, what privilege the ask
//! came from, or whether the guest has been told the transfer happened.
//!
//! The last one is the one that is easy to get wrong, and it is the reason
//! [`BlockBackend::read_sector`] returns `Result` and takes a caller-filled
//! buffer: **a backend reports whether it did the work.** A backend that cannot
//! serve a read says so, and the device turns that into a device fault the guest
//! can see. A backend that failed silently would leave the guest reading a buffer
//! of whatever was in it, and a program would act on data that was never stored.
//!
//! # Why `lazalith-devices` holds the backends at all
//!
//! Because it is `no_std`. That is not an accident of packaging, it is the
//! boundary being enforced by the compiler: this crate **cannot** open a file, make
//! a socket or call into a host audio API, so a backend here is pure computation
//! over a buffer. A backend that *is* a host resource — a file, a socket, a window
//! — belongs in the host layer, and the only thing it has to agree with this crate
//! about is [`BlockBackend`]. `docs/device-model.md` §5 records that, and the
//! absence of a file backend here is the check.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::DeviceId;

/// How many bytes one sector is.
///
/// The hardware sector size, not the operating system's block size. A block
/// device moves sectors; what a filesystem does with them is the operating
/// system's business and is not this device's.
pub const SECTOR_BYTES: u64 = 512;

/// What a host backend is, for a diagnostic and for a machine profile to name.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum BackendKind {
    /// A plain byte buffer the host allocated. The leaf of every chain.
    Memory,
    /// A sparse overlay over a read-only base. Reads fall through; writes allocate.
    CopyOnWrite,
    /// No storage at all, for a machine profile that named a disk and could not
    /// have one.
    ///
    /// Its own kind rather than a `Memory` of zero bytes, because a diagnostic that
    /// says "memory backend, 0 sectors" describes a disk that exists and is empty.
    /// The difference is the whole of what `Absent` is for.
    Absent,
}

impl BackendKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::CopyOnWrite => "cow",
            Self::Absent => "absent",
        }
    }

    /// Whether backends of this kind can be stacked under another backend.
    ///
    /// The rule is about *where a sector can come from*, not about nesting: a
    /// memory backend is a leaf because there is nothing under it, and an overlay
    /// is not a leaf because a read that is not allocated has to go somewhere.
    pub const fn is_stackable(self) -> bool {
        matches!(self, Self::CopyOnWrite)
    }
}

impl fmt::Display for BackendKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A refusal from a host backend.
///
/// Every variant is something a **guest** can be told about, because every one of
/// them is reachable from a register access and a guest that reads a zero because
/// the storage underneath was full has been told a lie.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackendError {
    /// Nothing to hold.
    ///
    /// Its own variant rather than an `OutOfRange`, because a zero-capacity backend
    /// has no end to be past: there is no sector that was out of range, there is no
    /// disk.
    Empty,
    /// A sector or a buffer past the end of what this backend holds.
    OutOfRange {
        /// The sector asked for.
        sector: u64,
        /// How many bytes were asked for.
        bytes: u64,
        /// How many bytes the backend holds.
        capacity: u64,
    },
    /// A write to something that is not writable.
    ///
    /// A read-only base is not a limitation a guest should discover by writing to
    /// it and seeing nothing happen; a device that is not writable is a device
    /// whose status register says so before the guest tries.
    ReadOnly,
    /// A sector buffer that is not exactly [`SECTOR_BYTES`].
    WrongSectorSize {
        /// What a sector is.
        expected: usize,
        /// What was offered.
        found: usize,
    },
    /// A copy-on-write backend was built over a base that can be written.
    ///
    /// This is a construction error and it is deliberate. An overlay whose base
    /// can be written is not copy-on-write, and nothing would notice until the
    /// first change to a backend that flushes through. Refusing at construction
    /// makes the promise checkable instead of aspirational.
    WritableBase,
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("there is no storage to hold anything"),
            Self::OutOfRange {
                sector,
                bytes,
                capacity,
            } => write!(
                f,
                "sector {sector} of {bytes} bytes is past a backend holding {capacity} bytes"
            ),
            Self::ReadOnly => f.write_str("the backend is read-only"),
            Self::WrongSectorSize { expected, found } => {
                write!(f, "a sector is {expected} bytes, and {found} were offered")
            }
            Self::WritableBase => f.write_str("a copy-on-write overlay needs a read-only base"),
        }
    }
}

impl Error for BackendError {}

/// What every host backend has, whatever shape it backs.
pub trait Backend: fmt::Debug {
    /// What kind of backend this is, for a diagnostic and for a profile to name.
    fn kind(&self) -> BackendKind;

    /// An opaque token identifying *this* backend instance.
    ///
    /// It exists for one reason: a machine snapshot restored onto a machine whose
    /// disk is a different disk would otherwise produce a machine that is almost
    /// the one that was saved. The token is compared on restore and a mismatch is
    /// a refusal, so "restored onto different storage" is a fact a caller is told
    /// rather than a difference nobody notices.
    ///
    /// The value is chosen by the backend and only has to be *distinguishing*, not
    /// meaningful. Two different allocations must differ, and the same allocation
    /// must not.
    fn identity(&self) -> BackendIdentity;

    /// Discards volatile state.
    ///
    /// A backend's *contents* are not volatile — they are the storage, and a reset
    /// does not empty a disk. What a reset discards is whatever the backend was
    /// holding that the guest was not entitled to see again, and a backend with
    /// nothing of its own does nothing here.
    fn reset(&mut self);
}

/// A host resource with a capacity, addressed in sectors.
///
/// This is the shape a disk, an image file and a ROM window all have, and it is
/// the smallest one that can carry the §26 separation for storage. A display
/// backend and an audio backend are different traits, because a single erased
/// "backend" trait with a request enum would be the lowest common denominator of
/// four unrelated things — which is the same argument `Device::snapshot` already
/// makes about not sharing one encoding across devices.
pub trait BlockBackend: Backend {
    /// How many bytes this backend holds.
    fn capacity(&self) -> u64;

    /// Whether this backend accepts writes.
    fn writable(&self) -> bool;

    /// Reads one sector into `out`, which must be exactly [`SECTOR_BYTES`].
    ///
    /// `out` is the caller's buffer and the backend fills it or reports that it
    /// could not. A backend that returned `Ok` without filling it would leave the
    /// caller acting on its own uninitialised bytes, so filling it is part of the
    /// contract rather than an expectation.
    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), BackendError>;

    /// Writes one sector from `data`, which must be exactly [`SECTOR_BYTES`].
    fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BackendError>;
}

/// An opaque token identifying one backend instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct BackendIdentity(u64);

impl BackendIdentity {
    /// A token from a snapshot, for a device comparing it with its own.
    ///
    /// Not a constructor in the ordinary sense: it makes no backend. It is how a
    /// device reads the identity a snapshot recorded and asks whether it is the
    /// one it holds.
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw token, for a device encoding one in its own snapshot.
    ///
    /// Public because a device has to be able to put it in a snapshot it owns and
    /// read it back. It is not a handle: it names no backend and reaches none.
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// A fresh identity, different from every other one this process hands out.
    ///
    /// A counter rather than a pointer, because a pointer would make the identity
    /// mean "where this was allocated", which leaks host layout into something a
    /// snapshot compares.
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

// -- the leaf backend --------------------------------------------------------

/// A plain byte buffer the host allocated.
///
/// The leaf of every chain: there is nothing under it, so a read is a copy out of
/// the buffer and a write is a copy into it. It is `no_std` and touches no host
/// resource, which is what makes it the right thing to build a first chain on —
/// and what makes it useless as a model of a real file, which is B8's work.
#[derive(Debug)]
pub struct MemoryBlockBackend {
    identity: BackendIdentity,
    bytes: Vec<u8>,
    writable: bool,
}

impl MemoryBlockBackend {
    /// A backend of `capacity` zero bytes, writable.
    ///
    /// Fails rather than clamping on a capacity of zero: a backend holding nothing
    /// is a backend every access to which faults, and a caller who asked for that
    /// has made a mistake worth naming.
    pub fn new(capacity: u64) -> Result<Self, BackendError> {
        if capacity == 0 {
            // A backend holding nothing is a backend every access to which faults, and
            // a caller who asked for that has made a mistake worth naming. Refused
            // rather than clamped: `Vec::try_reserve_exact(0)` succeeds, so without
            // this a zero-sized disk would be built and then fault on every access.
            return Err(BackendError::Empty);
        }
        let length = usize::try_from(capacity).map_err(|_| BackendError::OutOfRange {
            sector: 0,
            bytes: capacity,
            capacity: 0,
        })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| BackendError::OutOfRange {
                sector: 0,
                bytes: capacity,
                capacity: 0,
            })?;
        bytes.resize(length, 0);
        Ok(Self {
            identity: BackendIdentity::fresh(),
            bytes,
            writable: true,
        })
    }

    /// The same backend, refusing writes.
    ///
    /// This is how a base is made: a raw image, opened read-only, for an overlay
    /// to sit on. It is a *view* of the same bytes rather than a copy, so a write
    /// that somehow reached the base would be refused by the same buffer it would
    /// otherwise have modified.
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    /// A backend holding a copy of `contents`.
    ///
    /// For an image a caller already has — a boot disk built in memory, a fixture
    /// in a test — rather than a zeroed one.
    pub fn from_bytes(contents: &[u8]) -> Result<Self, BackendError> {
        let mut backend = Self::new(contents.len() as u64)?;
        backend.bytes[..contents.len()].copy_from_slice(contents);
        Ok(backend)
    }

    /// The bytes, for a host that wants to keep a handle on its own storage.
    ///
    /// **This is the host's way back in, and it is not a guest interface.** The
    /// backend is moved into the device when the device is built, and the device
    /// exposes no way to reach it — so a host that wants the disk afterwards keeps
    /// what it already had. For a memory backend that is the bytes; for a file
    /// backend (B8) it is the file.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Backend for MemoryBlockBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Memory
    }
    fn identity(&self) -> BackendIdentity {
        self.identity
    }
    fn reset(&mut self) {
        // Nothing. The contents are the storage, and a reset does not empty a
        // disk — that would be a machine that boots into a machine with no disk.
    }
}

impl BlockBackend for MemoryBlockBackend {
    fn capacity(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn writable(&self) -> bool {
        self.writable
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), BackendError> {
        if out.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: out.len(),
            });
        }
        let start = sector
            .checked_mul(SECTOR_BYTES)
            .ok_or(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            })?;
        let end = start
            .checked_add(SECTOR_BYTES)
            .ok_or(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            })?;
        if end > self.capacity() {
            return Err(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            });
        }
        let start = start as usize;
        out.copy_from_slice(&self.bytes[start..start + SECTOR_BYTES as usize]);
        Ok(())
    }

    fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::ReadOnly);
        }
        if data.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: data.len(),
            });
        }
        let start = sector
            .checked_mul(SECTOR_BYTES)
            .ok_or(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            })?;
        let end = start
            .checked_add(SECTOR_BYTES)
            .ok_or(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            })?;
        if end > self.capacity() {
            return Err(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            });
        }
        let start = start as usize;
        self.bytes[start..start + SECTOR_BYTES as usize].copy_from_slice(data);
        Ok(())
    }
}

// -- the stacking backend ----------------------------------------------------

/// A sparse overlay over a read-only base.
///
/// The model `binstruction.md` §26 and §31 ask for, and QEMU's: **read from the
/// overlay where a sector is allocated, otherwise from the base; write only to the
/// overlay.** The base is never modified, which is what makes a machine's writes
/// disposable and a base image shareable.
///
/// **The base must be read-only, and that is checked at construction.** QEMU
/// opens intermediate layers read-only for the same reason. An overlay over a
/// writable base is not copy-on-write — it is a layer that happens not to write
/// through today — and nothing would notice until a future backend that does.
/// [`BackendError::WritableBase`] makes it a refusal instead.
#[derive(Debug)]
pub struct CopyOnWriteBlockBackend {
    identity: BackendIdentity,
    base: Box<dyn BlockBackend>,
    /// Sector number → its bytes. Absent means "ask the base".
    ///
    /// A map and not a vector because the whole point is that an overlay is
    /// sparse: a 64 MiB disk with one sector written holds one sector here, and a
    /// vector would make the overlay the size of the disk, which is the thing
    /// copy-on-write exists to avoid.
    allocated: BTreeMap<u64, Vec<u8>>,
}

impl CopyOnWriteBlockBackend {
    /// An empty overlay over `base`.
    ///
    /// Refuses a base that accepts writes, and refuses a base that is itself not
    /// enough to hold a whole sector, because a sector is the unit and a chain
    /// that cannot hold one cannot be read.
    pub fn new(base: Box<dyn BlockBackend>) -> Result<Self, BackendError> {
        if base.writable() {
            return Err(BackendError::WritableBase);
        }
        if base.capacity() < SECTOR_BYTES {
            return Err(BackendError::OutOfRange {
                sector: 0,
                bytes: SECTOR_BYTES,
                capacity: base.capacity(),
            });
        }
        Ok(Self {
            identity: BackendIdentity::fresh(),
            base,
            allocated: BTreeMap::new(),
        })
    }

    /// How many sectors this overlay holds in its own right.
    ///
    /// The number that makes copy-on-write visible: a machine that has written
    /// three sectors holds three sectors, not a copy of the disk.
    pub fn allocated_sectors(&self) -> u64 {
        self.allocated.len() as u64
    }

    /// Whether a sector is in the overlay rather than the base.
    pub fn is_allocated(&self, sector: u64) -> bool {
        self.allocated.contains_key(&sector)
    }

    /// The base, for a host that wants to reach what the guest has not written.
    ///
    /// The same rule as [`MemoryBlockBackend::as_bytes`]: this is the host's way
    /// in, and the device has no such accessor.
    pub fn base(&self) -> &dyn BlockBackend {
        self.base.as_ref()
    }
}

impl Backend for CopyOnWriteBlockBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::CopyOnWrite
    }
    fn identity(&self) -> BackendIdentity {
        self.identity
    }
    fn reset(&mut self) {
        // The overlay is the *device's* dirty state, not a cache of something else,
        // and a machine reset is not a machine that forgot what it wrote. QEMU
        // behaves the same way: creating a snapshot makes the old image the
        // backing file, and a reset does not undo that.
        self.base.reset();
    }
}

impl BlockBackend for CopyOnWriteBlockBackend {
    fn capacity(&self) -> u64 {
        self.base.capacity()
    }
    fn writable(&self) -> bool {
        true
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), BackendError> {
        if out.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: out.len(),
            });
        }
        if let Some(bytes) = self.allocated.get(&sector) {
            out.copy_from_slice(bytes);
            return Ok(());
        }
        // The fall-through. Not a copy of the base into the overlay — that is what
        // makes an overlay the size of the disk — just this sector, on the way to
        // the guest.
        self.base.read_sector(sector, out)
    }

    fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BackendError> {
        if data.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: data.len(),
            });
        }
        if sector
            .saturating_mul(SECTOR_BYTES)
            .saturating_add(SECTOR_BYTES)
            > self.capacity()
        {
            return Err(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            });
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(SECTOR_BYTES as usize)
            .map_err(|_| BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            })?;
        bytes.extend_from_slice(data);
        self.allocated.insert(sector, bytes);
        Ok(())
    }
}

/// A backend that refuses every operation, for a machine profile that names a
/// device with no storage behind it.
///
/// It exists because "a profile that asks for a block device and gets a machine
/// with a dead disk" is worse than a refusal, and because it makes the third
/// §26 rule testable: a backend that cannot support what the guest asked reports
/// it rather than pretending.
#[derive(Debug)]
pub struct AbsentBlockBackend {
    identity: BackendIdentity,
    reason: DeviceId,
}

impl AbsentBlockBackend {
    /// A backend with no storage, attributable to the device that asked for it.
    pub fn new(reason: DeviceId) -> Self {
        Self {
            identity: BackendIdentity::fresh(),
            reason,
        }
    }
    /// The device whose storage is missing.
    pub const fn missing_for(&self) -> DeviceId {
        self.reason
    }
}

impl Backend for AbsentBlockBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Absent
    }
    fn identity(&self) -> BackendIdentity {
        self.identity
    }
    fn reset(&mut self) {}
}

impl BlockBackend for AbsentBlockBackend {
    fn capacity(&self) -> u64 {
        0
    }
    fn writable(&self) -> bool {
        false
    }
    fn read_sector(&mut self, sector: u64, _out: &mut [u8]) -> Result<(), BackendError> {
        Err(BackendError::OutOfRange {
            sector,
            bytes: SECTOR_BYTES,
            capacity: 0,
        })
    }
    fn write_sector(&mut self, _sector: u64, _data: &[u8]) -> Result<(), BackendError> {
        Err(BackendError::ReadOnly)
    }
}
