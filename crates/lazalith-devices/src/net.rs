//! B11: networking — a guest NIC, a Lazalith NIC, and a host backend.
//!
//! # The fourth shape, and the first two-way one
//!
//! B7's display is pulled. B9's input is pushed. B10's audio has a rate and is drained
//! by the host. A network is **two-way and asynchronous on both sides**: the guest sends
//! without being asked and receives without expecting, and the host may be slow, may
//! refuse, or may be a different machine entirely from the one the guest believes it is
//! on.
//!
//! That is the property the design turns on. A guest that sends a frame gets one of:
//!
//! - **accepted** — a frame left the guest and reached the host;
//! - **refused** — the host would not take it, and the guest is told *why*;
//! - **dropped** — it is gone, and nobody will ever know.
//!
//! Only the first two are honest. **There is no "sent".** A device that reported a
//! transmit as complete when the host had not accepted the frame would be telling the
//! guest a packet was delivered that never left the machine — and a network stack that
//! believed it would retransmit nothing, because nothing looked wrong.
//!
//! So `transmit` returns a [`TransmitOutcome`] and there is no path to `Sent` that does
//! not go through the host.
//!
//! # Frames, not packets
//!
//! §32 says "Guest NIC → Lazalith NIC → host backend" and does not name a link-layer
//! format. A frame here is **an opaque byte string with a length**, deliberately: a NIC
//! that knew about Ethernet headers would be a device model for one particular
//! compatibility machine, and §32's own words are that the host side (NAT, user-mode,
//! bridged, tap/socket) is "host-side implementation detail".
//!
//! An `Ethernet`-shaped frame would be that decision made in the guest-visible device,
//! where every later decision inherits it. `LazFrame` is a length and bytes, and a
//! guest that wants Ethernet framing puts it there itself.
//!
//! # The host backend is host-side, and says so
//!
//! §32 names five host backends and calls them implementation details. So
//! [`NetworkBackend`] has no Lazalith types in its signatures at all — the same rule B10
//! set for audio, and for the same reason: a backend that took a guest-visible struct
//! would be a second guest driver.

use alloc::vec::Vec;
use core::fmt;

use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceOffset, InterruptId};

use crate::{Device, DeviceError, DeviceId};

/// How large the register window is.
pub const NET_REGISTER_BYTES: u64 = 64;

/// The ABI this device implements.
pub const NET_ABI_VERSION: u64 = 1;

/// MAC address word 0, at offset 0. Read-only.
pub const NET_REGISTER_MAC_LOW: DeviceOffset = DeviceOffset::new(0);
/// MAC address word 1, at offset 8. Read-only.
pub const NET_REGISTER_MAC_HIGH: DeviceOffset = DeviceOffset::new(8);
/// Control, at offset 16. Read/write.
pub const NET_REGISTER_CONTROL: DeviceOffset = DeviceOffset::new(16);
/// Status, at offset 24. Read-only.
pub const NET_REGISTER_STATUS: DeviceOffset = DeviceOffset::new(24);
/// The transmit data port, at offset 32. Write.
pub const NET_REGISTER_TX_DATA: DeviceOffset = DeviceOffset::new(32);
/// The receive data port, at offset 40. Read.
pub const NET_REGISTER_RX_DATA: DeviceOffset = DeviceOffset::new(40);
/// The number of bytes the transmit port has taken, at offset 48. Read-only.
pub const NET_REGISTER_TX_LENGTH: DeviceOffset = DeviceOffset::new(48);
/// The number of bytes the receive port can give, at offset 56. Read-only.
pub const NET_REGISTER_RX_LENGTH: DeviceOffset = DeviceOffset::new(56);

/// `CONTROL` bit 0: the link is up.
pub const NET_CONTROL_UP: u64 = 1;
/// `CONTROL` bit 1: a transmit is being assembled.
pub const NET_CONTROL_TX_ACTIVE: u64 = 2;
/// `CONTROL` bit 2: interrupts are enabled.
pub const NET_CONTROL_INTERRUPTS: u64 = 4;
/// `CONTROL` bit 3: the frame in the transmit port is complete; hand it to the host.
///
/// **Added because the register interface could not finish a transmit without it.**
/// `commit_transmit` is a Rust call, so a guest driving the device through MMIO could
/// start a frame and write its bytes and had no way to say "that is all of it" — the
/// only register that ended a transmit also cleared the one that started it, so the
/// guest could assemble a frame forever and never have it leave the machine.
///
/// The host's `pump` looks for this bit. Writing it does not itself transmit, for the
/// same reason nothing else in this device transmits: a register access must not call a
/// host.
pub const NET_CONTROL_TX_COMMIT: u64 = 8;

/// `STATUS` bit 0: a frame is waiting to be read by the guest.
pub const NET_STATUS_RX_READY: u64 = 1;
/// `STATUS` bit 1: a frame is waiting to be sent by the host.
pub const NET_STATUS_TX_READY: u64 = 2;
/// `STATUS` bit 2: the host refused the last frame.
pub const NET_STATUS_TX_REFUSED: u64 = 4;
/// `STATUS` bit 3: the host is unreachable.
pub const NET_STATUS_LINK_DOWN: u64 = 8;

/// The largest frame this device will carry, in bytes.
///
/// 1500 — the standard Ethernet MTU — because a host that receives a larger frame from
/// a bridge will have already dropped it, and a device that accepted one would be
/// accepting a frame the network will never deliver. 1514 (with an Ethernet header) is
/// accepted by [`MAX_FRAME_BYTES`] below.
pub const MAX_FRAME_BYTES: usize = 1514;

/// The MTU this device advertises, excluding any link-layer header.
pub const MTU: usize = 1500;

/// The size of the transmit staging buffer.
///
/// **A power of two, and large enough for one maximum frame.** 2048 holds a 1514-byte
/// frame with room to spare, and being a power of two means a full-buffer test is a
/// mask rather than a division.
pub const TX_BUFFER_BYTES: usize = 2048;

/// The size of the receive queue, in frames.
/// 16. Not a power of two, because it is a *count* and not a buffer, and a
/// `VecDeque` handles it without a mask. A receive queue of one frame would drop
/// everything the host delivered while the guest was descheduled; sixteen is about
/// 120 microseconds of a 1 Gb link, which is longer than a guest scheduling quantum.
pub const RX_QUEUE_FRAMES: usize = 16;

/// A frame: a length and bytes.
///
/// **Opaque on purpose** — see the module documentation. Nothing in this platform knows
/// what is inside a frame, and a guest that wants a particular link-layer format builds
/// one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LazFrame {
    bytes: Vec<u8>,
}

impl LazFrame {
    /// A frame of these bytes.
    pub fn new(bytes: Vec<u8>) -> Result<Self, NetError> {
        if bytes.is_empty() {
            return Err(NetError::EmptyFrame);
        }
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(NetError::FrameTooLarge {
                length: bytes.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        Ok(Self { bytes })
    }

    /// The frame's bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The frame's length.
    pub fn len(&self) -> u64 {
        self.bytes.len() as u64
    }

    /// Always `false`: a frame is non-empty by construction.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Takes the bytes out, for a host that is going to own them.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// What a network backend can tell the guest, and what it refuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetError {
    /// A frame of no bytes.
    EmptyFrame,
    /// A frame longer than [`MAX_FRAME_BYTES`].
    FrameTooLarge {
        /// The length asked for.
        length: usize,
        /// The largest this device carries.
        limit: usize,
    },
    /// The host refused the frame, and said nothing more.
    HostRefused,
    /// The host is not reachable.
    HostUnavailable,
    /// The link is down, so a frame was not sent.
    LinkDown,
    /// The receive queue is full, so a frame the host delivered was dropped.
    ///
    /// **The one genuinely lossy path, and it is reported.** A network drops packets;
    /// that is what a network does. What matters is that the *host* is told its frame
    /// was not stored, because a host that silently fills a queue and discards is a
    /// host nobody can debug.
    ReceiveQueueFull {
        /// The queue's capacity, in frames.
        capacity: usize,
    },
    /// A register was read or written with the wrong shape.
    BadRegister(DeviceOffset),
    /// The transmit port was written with no transmit in progress.
    NoTransmit,
    /// The receive port was read with no frame waiting.
    NoReceive,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFrame => f.write_str("a frame with no bytes is not a frame"),
            Self::FrameTooLarge { length, limit } => {
                write!(f, "a {length} byte frame is over the {limit} byte limit")
            }
            Self::HostRefused => f.write_str("the host would not accept the frame"),
            Self::HostUnavailable => f.write_str("the host is not reachable"),
            Self::LinkDown => f.write_str("the link is down"),
            Self::ReceiveQueueFull { capacity } => {
                write!(f, "the receive queue already holds {capacity} frames")
            }
            Self::BadRegister(offset) => {
                write!(
                    f,
                    "register {} is not part of the network device",
                    offset.as_u64()
                )
            }
            Self::NoTransmit => f.write_str("no transmit is in progress"),
            Self::NoReceive => f.write_str("no frame is waiting to be read"),
        }
    }
}

impl core::error::Error for NetError {}

impl From<NetError> for DeviceError {
    fn from(source: NetError) -> Self {
        match source {
            NetError::BadRegister(offset) => DeviceError::InvalidRange {
                offset,
                bytes: u64::from(DataSize::Double.bytes()),
            },
            NetError::NoReceive => DeviceError::ReadUnsupported,
            _ => DeviceError::Capacity,
        }
    }
}

/// What happened to a frame the guest tried to send.
///
/// **There is no `Sent` variant that the device can produce on its own.** Every
/// successful path goes through the host, and that is the point: see the module
/// documentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransmitOutcome {
    /// The host accepted the frame.
    Accepted {
        /// How many bytes.
        bytes: u64,
    },
    /// The host would not take it, and the guest is told.
    Refused {
        /// Why, as far as the host said.
        reason: NetError,
    },
    /// The frame was handed to the host, which dropped it.
    ///
    /// A host-side delivery that is not the same as acceptance — a user-mode backend
    /// that could not hand the frame to a real interface. The guest is told, because
    /// "sent" would be a lie and silence would be worse.
    Dropped {
        /// Why.
        reason: NetError,
    },
}

/// A host's network.
///
/// **No Lazalith types in any signature.** `&[u8]` in, `Vec<u8>` out, and two facts
/// about the link. §32's five host backends — NAT, user-mode, bridged, host-only,
/// tap/socket — are all implementations of this, and none of them needs to know what
/// the guest is.
pub trait NetworkBackend: fmt::Debug {
    /// Whether the link is up.
    fn is_up(&self) -> bool;

    /// The address bytes, in whatever order the host uses.
    ///
    /// A `Vec<u8>` rather than a `u64` MAC, because §32 does not name a link layer and
    /// a device that had a `u64` MAC would have decided that the link is 48-bit
    /// Ethernet. The register pair still exists, so a guest can read six bytes of
    /// whatever the host calls its address.
    fn address(&self) -> Vec<u8>;

    /// Offers a frame to the host.
    ///
    /// `Ok(true)` is acceptance. `Ok(false)` is the host having declined it without a
    /// reason. `Err` is the host being unable to be asked at all.
    fn send(&mut self, frame: &[u8]) -> Result<bool, NetError>;

    /// Takes the next frame the host has, if there is one.
    fn receive(&mut self) -> Result<Option<Vec<u8>>, NetError>;
}

/// A guest-visible network device.
///
/// The guest writes a frame into the transmit port and reads one out of the receive
/// port, a byte at a time through the register window, because `Device` gives a device
/// its registers and no access to guest memory. That is B5's block device and B10's
/// audio device reaching the same conclusion independently: a frame is a **port**,
/// not a buffer, and a device that could name a guest address would be a device that
/// could be pointed at anything.
#[derive(Debug)]
pub struct NetworkDevice {
    id: DeviceId,
    interrupt: InterruptId,
    control: u64,
    status: u64,
    tx: Vec<u8>,
    rx: alloc::collections::VecDeque<Vec<u8>>,
    rx_cursor: usize,
    pending_tx: Option<Result<TransmitOutcome, NetError>>,
    rx_ready_latch: bool,
    tx_count: u64,
    rx_count: u64,
    dropped: u64,
}

impl NetworkDevice {
    /// A device whose link is down and which has sent nothing.
    pub fn new(id: DeviceId, interrupt: InterruptId) -> Self {
        Self {
            id,
            interrupt,
            control: 0,
            status: NET_STATUS_LINK_DOWN,
            tx: Vec::new(),
            rx: alloc::collections::VecDeque::new(),
            rx_cursor: 0,
            pending_tx: None,
            rx_ready_latch: false,
            tx_count: 0,
            rx_count: 0,
            dropped: 0,
        }
    }

    /// Which device this is, for a diagnostic.
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// How many frames the host has accepted from the guest.
    pub const fn transmitted(&self) -> u64 {
        self.tx_count
    }

    /// How many frames the guest has read from the host.
    pub const fn received(&self) -> u64 {
        self.rx_count
    }

    /// How many frames the host delivered that did not fit.
    ///
    /// The loss counter a network stack would want, and the reason a host that silently
    /// fills a queue is a host nobody can debug.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// The current status bits.
    pub const fn status(&self) -> u64 {
        self.status
    }

    /// How many frames are waiting for the guest.
    pub fn receive_queue_len(&self) -> usize {
        self.rx.len()
    }

    fn recompute(&mut self, link_up: bool) -> u64 {
        let mut status = 0;
        if self.rx_cursor < self.rx.len() {
            status |= NET_STATUS_RX_READY;
        }
        if !self.tx.is_empty() {
            status |= NET_STATUS_TX_READY;
        }
        if self.pending_tx.is_some() {
            status |= NET_STATUS_TX_REFUSED;
        }
        if !link_up {
            status |= NET_STATUS_LINK_DOWN;
        }
        self.status = status;
        status
    }

    /// Starts a transmit. The guest then writes bytes to the transmit port.
    pub fn begin_transmit(&mut self) -> Result<(), NetError> {
        if self.control & NET_CONTROL_TX_ACTIVE != 0 {
            return Err(NetError::HostRefused);
        }
        self.tx.clear();
        self.control |= NET_CONTROL_TX_ACTIVE;
        Ok(())
    }

    /// Adds bytes to the transmit in progress.
    pub fn push_transmit(&mut self, bytes: &[u8]) -> Result<(), NetError> {
        if self.control & NET_CONTROL_TX_ACTIVE == 0 {
            return Err(NetError::NoTransmit);
        }
        if self.tx.len() + bytes.len() > TX_BUFFER_BYTES {
            return Err(NetError::FrameTooLarge {
                length: self.tx.len() + bytes.len(),
                limit: TX_BUFFER_BYTES,
            });
        }
        self.tx.extend_from_slice(bytes);
        Ok(())
    }

    /// Finishes a transmit and offers the frame to a host.
    ///
    /// **The only path to a successful transmit.** There is no method that completes one
    /// without a backend, because a device that could say "sent" on its own would be
    /// telling the guest a packet was delivered that never left the machine.
    pub fn commit_transmit(
        &mut self,
        backend: &mut dyn NetworkBackend,
    ) -> Result<TransmitOutcome, NetError> {
        if self.control & NET_CONTROL_TX_ACTIVE == 0 {
            return Err(NetError::NoTransmit);
        }
        self.control &= !NET_CONTROL_TX_ACTIVE;
        let frame = core::mem::take(&mut self.tx);
        if frame.is_empty() {
            return Err(NetError::EmptyFrame);
        }
        if !backend.is_up() {
            let outcome = TransmitOutcome::Refused {
                reason: NetError::LinkDown,
            };
            self.pending_tx = Some(Err(NetError::LinkDown));
            self.recompute(backend.is_up());
            return Ok(outcome);
        }
        let outcome = match backend.send(&frame) {
            Ok(true) => {
                self.tx_count = self.tx_count.saturating_add(1);
                TransmitOutcome::Accepted {
                    bytes: frame.len() as u64,
                }
            }
            Ok(false) => {
                self.pending_tx = Some(Err(NetError::HostRefused));
                TransmitOutcome::Refused {
                    reason: NetError::HostRefused,
                }
            }
            Err(reason) => {
                self.pending_tx = Some(Err(reason));
                TransmitOutcome::Dropped { reason }
            }
        };
        self.recompute(backend.is_up());
        Ok(outcome)
    }

    /// Hands a committed frame to the host and takes what it has delivered.
    ///
    /// **The one place a transmit reaches a host**, and the reason [`commit_transmit`]
    /// is not called from a register write. A guest sets `NET_CONTROL_TX_COMMIT`, and the
    /// host `pump`s. So the guest-visible device has no method that completes a transmit
    /// without a backend in hand, and there is no code path by which a frame is reported
    /// as delivered that did not go through a host.
    pub fn pump(
        &mut self,
        backend: &mut dyn NetworkBackend,
        budget: u64,
    ) -> Result<PumpOutcome, NetError> {
        let mut outcome = PumpOutcome {
            received: 0,
            transmit: None,
        };
        if self.control & NET_CONTROL_TX_COMMIT != 0 {
            self.control &= !NET_CONTROL_TX_COMMIT;
            outcome.transmit = Some(self.commit_transmit(backend)?);
        }
        outcome.received = self.pump_receive(backend, budget)?;
        Ok(outcome)
    }

    /// Takes whatever the host has delivered, into the receive queue.
    ///
    /// Bounded by `budget` frames, because a host on a fast link can have more queued
    /// than a caller wants to move in one go — and an unbounded drain would let a
    /// 100 Gb link monopolise a pump.
    pub fn pump_receive(
        &mut self,
        backend: &mut dyn NetworkBackend,
        budget: u64,
    ) -> Result<u64, NetError> {
        let mut taken = 0u64;
        for _ in 0..budget {
            if self.rx.len() >= RX_QUEUE_FRAMES {
                // The queue is full. Anything the host has is dropped *and counted*,
                // because a network drops packets and the only unacceptable thing is
                // dropping them silently.
                if let Ok(Some(_)) = backend.receive() {
                    self.dropped = self.dropped.saturating_add(1);
                }
                break;
            }
            match backend.receive() {
                Ok(Some(frame)) => {
                    self.rx.push_back(frame);
                    taken += 1;
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }
        let link_up = backend.is_up();
        if taken > 0 {
            self.rx_ready_latch = true;
        }
        self.recompute(link_up);
        Ok(taken)
    }

    /// Reads a byte of the frame waiting for the guest.
    pub fn pull_receive(&mut self) -> Result<u8, NetError> {
        let Some(frame) = self.rx.front() else {
            return Err(NetError::NoReceive);
        };
        if self.rx_cursor >= frame.len() {
            // The guest has read the whole frame. Dropping it here rather than on the
            // next call means a guest that reads exactly `length` bytes leaves the
            // queue empty, which is what it expects.
            self.rx.pop_front();
            self.rx_cursor = 0;
            self.rx_count = self.rx_count.saturating_add(1);
            return Err(NetError::NoReceive);
        }
        let byte = frame[self.rx_cursor];
        self.rx_cursor += 1;
        Ok(byte)
    }

    /// The length of the frame the guest is reading.
    pub fn receive_length(&self) -> u64 {
        self.rx.front().map_or(0, |frame| frame.len() as u64)
    }

    /// Whether the guest is part-way through reading a frame.
    pub const fn is_reading(&self) -> bool {
        self.rx_cursor != 0
    }
}

impl Device for NetworkDevice {
    fn address_len(&self) -> u64 {
        NET_REGISTER_BYTES
    }

    fn reset(&mut self) {
        // Queues cleared, the transmit buffer dropped, and a partial receive abandoned.
        // A machine that resets mid-frame and then hands the guest the tail of a dead
        // transfer is a machine whose network is a source of nonsense.
        self.tx.clear();
        self.rx.clear();
        self.rx_cursor = 0;
        self.pending_tx = None;
        self.rx_ready_latch = false;
        self.control &= NET_CONTROL_UP | NET_CONTROL_INTERRUPTS;
        self.status = NET_STATUS_LINK_DOWN;
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= NET_REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        match offset {
            NET_REGISTER_TX_DATA => Err(DeviceError::ReadUnsupported),
            _ => Ok(()),
        }
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        _value: u64,
    ) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= NET_REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        match offset {
            NET_REGISTER_TX_DATA => {
                if self.control & NET_CONTROL_TX_ACTIVE == 0 {
                    Err(NetError::NoTransmit.into())
                } else {
                    Ok(())
                }
            }
            NET_REGISTER_CONTROL => Ok(()),
            _ => Err(DeviceError::WriteUnsupported),
        }
    }

    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(match offset {
            NET_REGISTER_MAC_LOW | NET_REGISTER_MAC_HIGH => 0,
            NET_REGISTER_CONTROL => self.control,
            NET_REGISTER_STATUS => self.status,
            NET_REGISTER_TX_LENGTH => self.tx.len() as u64,
            NET_REGISTER_RX_LENGTH => self.receive_length(),
            NET_REGISTER_RX_DATA => {
                // A byte at a time, zero-extended: the port is 8 bytes wide and a frame
                // is a byte stream, so the guest reads eight times per eight bytes and
                // the high bytes are zero. Reading a word would be a different protocol
                // and is not this one.
                let byte = self.pull_receive().map_err(DeviceError::from)?;
                u64::from(byte)
            }
            NET_REGISTER_TX_DATA => return Err(DeviceError::ReadUnsupported),
            _ => return Err(NetError::BadRegister(offset).into()),
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
            NET_REGISTER_CONTROL => {
                // Three cases, written out rather than as clever bit arithmetic, because
                // two of them were got wrong first.
                //
                //   TX_ACTIVE set   -> begin a transmit
                //   TX_COMMIT set  -> the frame is finished; leave the transmit open so
                //                    the host `pump` can still see it
                //   neither         -> abandon the transmit
                //
                // The first version began and then cleared TX_ACTIVE in the same arm, so
                // the transmit was unset before the guest could write a byte into it. The
                // second cleared TX_ACTIVE whenever TX_COMMIT was written, so committing
                // a frame abandoned it. Both looked correct.
                self.control &= !NET_CONTROL_TX_COMMIT;
                if value & NET_CONTROL_TX_ACTIVE != 0 {
                    self.begin_transmit()?;
                } else if value & NET_CONTROL_TX_COMMIT == 0 {
                    self.control &= !NET_CONTROL_TX_ACTIVE;
                }
                self.control |=
                    value & (NET_CONTROL_UP | NET_CONTROL_INTERRUPTS | NET_CONTROL_TX_COMMIT);
            }
            NET_REGISTER_TX_DATA => {
                self.push_transmit(&value.to_le_bytes())?;
            }
            _ => return Err(DeviceError::WriteUnsupported),
        }
        Ok(())
    }

    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        if output.len() != DataSize::Double.bytes() as usize {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: output.len() as u64,
            });
        }
        let value = match offset {
            NET_REGISTER_MAC_LOW | NET_REGISTER_MAC_HIGH => 0,
            NET_REGISTER_CONTROL => self.control,
            NET_REGISTER_STATUS => self.status,
            NET_REGISTER_TX_LENGTH => self.tx.len() as u64,
            NET_REGISTER_RX_LENGTH => self.receive_length(),
            // A peek of the receive port reports the next byte without taking it, so a
            // debugger can show what the guest is about to read.
            NET_REGISTER_RX_DATA => self.rx.front().map_or(0, |frame| {
                u64::from(frame.get(self.rx_cursor).copied().unwrap_or(0))
            }),
            NET_REGISTER_TX_DATA => return Err(DeviceError::Unpeekable),
            _ => return Err(DeviceError::Unpeekable),
        };
        output.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    fn tick(&mut self, _elapsed: CycleCount) {
        // The network is driven by `pump_receive` and `commit_transmit`, both of which
        // the host calls. What `tick` does is latch the "a frame is waiting" condition
        // so the interrupt is an edge rather than a level.
        if self.rx_cursor < self.rx.len() {
            self.rx_ready_latch = true;
        }
    }

    fn take_interrupt(&mut self) -> Option<InterruptId> {
        if self.rx_ready_latch {
            self.rx_ready_latch = false;
            return Some(self.interrupt);
        }
        None
    }

    fn snapshot(&self) -> Vec<u8> {
        // The queues are **not** in here. A network device's snapshot holds its
        // registers and its counters, not a copy of every frame in flight: a machine
        // that had sixteen 1500-byte frames queued would have a 24 KiB snapshot, and a
        // machine snapshot is not a packet capture. The counters are, because a guest
        // can read them and is entitled to see them again after a restore.
        let mut out = Vec::with_capacity(NET_SNAPSHOT_BYTES);
        out.extend_from_slice(&self.control.to_le_bytes());
        out.extend_from_slice(&self.status.to_le_bytes());
        out.extend_from_slice(&self.tx_count.to_le_bytes());
        out.extend_from_slice(&self.rx_count.to_le_bytes());
        out.extend_from_slice(&self.dropped.to_le_bytes());
        out.push(self.rx_ready_latch as u8);
        out
    }

    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        if bytes.len() != NET_SNAPSHOT_BYTES {
            return Err(DeviceError::SnapshotShape {
                expected: NET_SNAPSHOT_BYTES,
                found: bytes.len(),
            });
        }
        self.control = u64::from_le_bytes(bytes[0..8].try_into().unwrap())
            & (NET_CONTROL_UP | NET_CONTROL_INTERRUPTS);
        self.status = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        self.tx_count = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        self.rx_count = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        self.dropped = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        self.rx_ready_latch = bytes[40] != 0;
        // Queues cleared, for the reason the module documentation gives: a restored
        // machine must not hand the guest packets that were in flight when the snapshot
        // was taken, because the host may since have moved them.
        self.tx.clear();
        self.rx.clear();
        self.rx_cursor = 0;
        self.pending_tx = None;
        Ok(())
    }
}

/// What one `pump` did, in both directions.
///
/// A struct rather than two return values because a pump does both and a caller wants to
/// know what happened; a tuple of two `u64`s and an `Option` would be the same
/// information with three positional parts to get wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PumpOutcome {
    /// Frames taken from the host.
    pub received: u64,
    /// What happened to a frame the guest had committed, if it had one.
    pub transmit: Option<TransmitOutcome>,
}

/// How many bytes a `NetworkDevice` snapshot is.
pub const NET_SNAPSHOT_BYTES: usize = 41;

/// A backend that drops everything, for a machine with no network.
///
/// **Not a null object for convenience.** A guest that transmits on a machine with no
/// network must be told the frame went nowhere. A backend that reported acceptance and
/// then discarded would make a guest's retransmit logic believe a packet was delivered.
#[derive(Debug)]
pub struct NullNetworkBackend {
    up: bool,
    accepted: u64,
    offered: u64,
    inbound: alloc::collections::VecDeque<Vec<u8>>,
}

impl NullNetworkBackend {
    /// A backend whose link is down and which accepts nothing.
    pub const fn new() -> Self {
        Self {
            up: false,
            accepted: 0,
            offered: 0,
            inbound: alloc::collections::VecDeque::new(),
        }
    }

    /// A backend whose link is up, which still delivers nothing.
    pub const fn up() -> Self {
        Self {
            up: true,
            accepted: 0,
            offered: 0,
            inbound: alloc::collections::VecDeque::new(),
        }
    }

    /// How many frames were offered.
    pub const fn offered(&self) -> u64 {
        self.offered
    }

    /// How many the host took.
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// Arranges for the host to deliver these frames, in order.
    pub fn with_inbound(mut self, frames: Vec<Vec<u8>>) -> Self {
        self.inbound = frames.into_iter().collect();
        self
    }
}

impl Default for NullNetworkBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for NullNetworkBackend {
    fn is_up(&self) -> bool {
        self.up
    }

    fn address(&self) -> Vec<u8> {
        Vec::new()
    }

    fn send(&mut self, _frame: &[u8]) -> Result<bool, NetError> {
        self.offered = self.offered.saturating_add(1);
        // `false`, not `true`: acceptance is a claim the host cannot make. A machine
        // with no network has nowhere to put the frame, and a guest that believes
        // otherwise will not retransmit it.
        Ok(false)
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        Ok(self.inbound.pop_front())
    }
}

/// A backend that records what it was given and can be told to fail, for tests.
#[derive(Debug)]
pub struct RecordingNetworkBackend {
    up: bool,
    sent: Vec<Vec<u8>>,
    fail: Option<NetError>,
    inbound: alloc::collections::VecDeque<Vec<u8>>,
    address: Vec<u8>,
}

impl RecordingNetworkBackend {
    /// A backend at this link state.
    pub fn new(up: bool) -> Self {
        Self {
            up,
            sent: Vec::new(),
            fail: None,
            inbound: alloc::collections::VecDeque::new(),
            address: Vec::new(),
        }
    }

    /// Every frame the host accepted, in order.
    pub fn sent(&self) -> &[Vec<u8>] {
        &self.sent
    }

    /// Makes every send fail with this reason.
    pub fn failing(mut self, reason: NetError) -> Self {
        self.fail = Some(reason);
        self
    }

    /// Arranges for the host to deliver these frames, in order.
    pub fn with_inbound(mut self, frames: Vec<Vec<u8>>) -> Self {
        self.inbound = frames.into_iter().collect();
        self
    }

    /// Gives the host an address, in whatever order it uses.
    pub fn with_address(mut self, address: Vec<u8>) -> Self {
        self.address = address;
        self
    }
}

impl NetworkBackend for RecordingNetworkBackend {
    fn is_up(&self) -> bool {
        self.up
    }

    fn address(&self) -> Vec<u8> {
        self.address.clone()
    }

    fn send(&mut self, frame: &[u8]) -> Result<bool, NetError> {
        if let Some(reason) = self.fail {
            return Err(reason);
        }
        self.sent.push(frame.to_vec());
        Ok(true)
    }

    fn receive(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        Ok(self.inbound.pop_front())
    }
}
