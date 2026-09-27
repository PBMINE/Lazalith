//! The virtual input device.
//!
//! # A queue, not a sample
//!
//! The device owns a queue of guest-visible events and the guest *drains* it. That
//! is the whole design decision, and everything else follows from it. A device
//! that reported only the *current* key state would lose every press and release
//! that happened between two polls — and a program that polls once per frame at
//! thirty frames a second would lose any key the user tapped faster than that.
//! Losing input is not a detail; it is the difference between a program that works
//! and one that intermittently does not, with no way to tell which.
//!
//! So: events accumulate, `poll` hands out up to `capacity` of them, and **the
//! remainder stays queued**. Events are never dropped to make a buffer fit.
//!
//! # Events are records, not host structures
//!
//! The record is 16 bytes with every field defined for every kind, so a reader
//! never has to ask which fields are meaningful. Nothing here is a host type: the
//! device does not know what a keyboard is, only that something pressed a key
//! with a stable code. The host adapter in Step 71 is the only component that
//! knows the host, and the same program under it must see the same records —
//! which is what makes a graphical program testable headlessly.
//!
//! # No blocking, no timeouts
//!
//! There is no "wait for an event" operation, and that is deliberate. A blocking
//! read would make a program's behaviour depend on when it was scheduled, and two
//! runs with the same injected events would not necessarily agree. A program polls;
//! a poll that finds nothing returns zero, which means "nothing pending" and not
//! "something went wrong".

use alloc::vec::Vec;
use core::fmt;

use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceOffset};

use crate::{Device, DeviceError};

/// The size of one event record, in bytes.
///
/// Fixed, and versioned with the ABI. A driver writes exactly this many bytes per
/// event, and a program that allocated a different size has a bug the size itself
/// makes visible.
pub const EVENT_BYTES: u64 = 16;

/// The first register: writing it drains the queue.
pub const REGISTER_POLL: DeviceOffset = DeviceOffset::new(0);
/// The second register: how many events are waiting.
pub const REGISTER_PENDING: DeviceOffset = DeviceOffset::new(8);
/// The third register: how many events the guest has taken in total.
pub const REGISTER_DELIVERED: DeviceOffset = DeviceOffset::new(16);
/// The fourth register: how many events the host has injected in total.
pub const REGISTER_INJECTED: DeviceOffset = DeviceOffset::new(24);
/// The fifth register: the ABI version the device implements.
pub const REGISTER_ABI_VERSION: DeviceOffset = DeviceOffset::new(32);
/// The sixth register: the device's own status, as a bit set.
///
/// Bit 0 is set once an event has been injected, so a program can tell "the host
/// has said nothing" from "nothing has happened yet".
pub const REGISTER_STATUS: DeviceOffset = DeviceOffset::new(40);
/// The seventh register: the number of events the last poll returned.
pub const REGISTER_LAST_COUNT: DeviceOffset = DeviceOffset::new(48);
/// The eighth register: the capacity the last poll was given.
pub const REGISTER_LAST_CAPACITY: DeviceOffset = DeviceOffset::new(56);

/// The ABI version this device implements.
pub const INPUT_ABI_VERSION: u64 = 1;

/// `REGISTER_STATUS` bit 0: at least one event has been injected.
pub const STATUS_INJECTED: u64 = 1;

/// How many bytes of register space the device occupies.
pub const REGISTER_BYTES: u64 = 64;

/// The most events one `poll` may take.
///
/// A bound rather than an unbounded read, so a caller cannot ask the device to
/// write an arbitrary number of records into a buffer it sized for fewer. It is
/// generous: a program that polls once per frame and a host that injects faster
/// than that will still drain everything within a few polls.
pub const MAX_POLL_CAPACITY: u64 = 1024;

/// What happened, as a guest-visible kind.
///
/// These numbers are the ABI's and are frozen. They are not host codes and must
/// never be renumbered to match one: a program compiled against this table has to
/// keep working when the host adapter changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventKind {
    /// A key went down. `code` is a stable key code.
    KeyDown,
    /// A key came up. `code` is a stable key code.
    KeyUp,
    /// The pointer moved. `x` and `y` are its absolute position.
    MouseMove,
    /// A mouse button went down. `code` is the button index.
    MouseDown,
    /// A mouse button came up. `code` is the button index.
    MouseUp,
    /// A character was typed. `code` is its Unicode scalar value.
    Text,
    /// The program was asked to quit.
    Quit,
}

impl EventKind {
    /// Every kind, in numbering order.
    pub const ALL: &'static [Self] = &[
        Self::KeyDown,
        Self::KeyUp,
        Self::MouseMove,
        Self::MouseDown,
        Self::MouseUp,
        Self::Text,
        Self::Quit,
    ];

    /// The ABI number for this kind.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::KeyDown => 1,
            Self::KeyUp => 2,
            Self::MouseMove => 3,
            Self::MouseDown => 4,
            Self::MouseUp => 5,
            Self::Text => 6,
            Self::Quit => 7,
        }
    }

    /// The kind for an ABI number, if it is one.
    pub const fn from_u32(value: u32) -> Option<Self> {
        Some(match value {
            1 => Self::KeyDown,
            2 => Self::KeyUp,
            3 => Self::MouseMove,
            4 => Self::MouseDown,
            5 => Self::MouseUp,
            6 => Self::Text,
            7 => Self::Quit,
            _ => return None,
        })
    }
}

/// One guest-visible event.
///
/// Every field is defined for every kind, and an unused one is zero. A reader
/// never has to check which fields are meaningful, which is the property that lets
/// a program pattern-match on the kind and read the rest without branching.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Event {
    /// What happened.
    pub kind: EventKindValue,
    /// The key code, button index, or Unicode scalar value, depending on the kind.
    pub code: u32,
    /// The pointer's absolute x, or zero.
    pub x: i32,
    /// The pointer's absolute y, or zero.
    pub y: i32,
}

/// An event's kind as it appears in a record.
///
/// A record's `kind` field is a `u32` that may name a kind this build does not
/// know, and a reader must be able to hold that without pretending to understand
/// it. So the field is the number, and [`Event::kind_value`] and
/// [`Event::is`] are the ways to ask about it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EventKindValue(pub u32);

impl EventKindValue {
    /// The number as written into a record.
    pub const fn as_u32(self) -> u32 {
        self.0
    }

    /// The kind, if this build knows it.
    pub const fn known(self) -> Option<EventKind> {
        EventKind::from_u32(self.0)
    }
}

impl From<EventKind> for EventKindValue {
    fn from(kind: EventKind) -> Self {
        Self(kind.as_u32())
    }
}

impl Event {
    /// An event of `kind` with `code`, and no pointer position.
    pub const fn new(kind: EventKind, code: u32) -> Self {
        Self {
            kind: EventKindValue(kind.as_u32()),
            code,
            x: 0,
            y: 0,
        }
    }

    /// A pointer event of `kind` with `code` at an absolute position.
    pub const fn pointer(kind: EventKind, code: u32, x: i32, y: i32) -> Self {
        Self {
            kind: EventKindValue(kind.as_u32()),
            code,
            x,
            y,
        }
    }

    /// The kind as a number, which is what a record holds.
    pub const fn kind_value(self) -> u32 {
        self.kind.as_u32()
    }

    /// The kind, if this build knows it.
    pub const fn kind(self) -> Option<EventKind> {
        self.kind.known()
    }

    /// Whether this event is of `kind`.
    pub const fn is(self, kind: EventKind) -> bool {
        self.kind.as_u32() == kind.as_u32()
    }

    /// The record's sixteen bytes, little-endian, in the ABI's field order.
    pub fn encode(self) -> [u8; EVENT_BYTES as usize] {
        let mut bytes = [0u8; EVENT_BYTES as usize];
        bytes[0..4].copy_from_slice(&self.kind.as_u32().to_le_bytes());
        bytes[4..8].copy_from_slice(&self.code.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.x.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.y.to_le_bytes());
        bytes
    }

    /// An event from a record's sixteen bytes.
    ///
    /// An unknown `kind` is kept as its number rather than refused: a program
    /// running against a newer device must be able to *hold* an event it does not
    /// understand and skip it, and refusing the record would make that impossible.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < EVENT_BYTES as usize {
            return None;
        }
        Some(Self {
            kind: EventKindValue(read_u32(bytes, 0)),
            code: read_u32(bytes, 4),
            x: read_i32(bytes, 8),
            y: read_i32(bytes, 12),
        })
    }
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_i32(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The virtual input device.
#[derive(Debug)]
pub struct InputDevice {
    queue: Vec<Event>,
    head: usize,
    delivered: u64,
    injected: u64,
    last_count: u64,
    last_capacity: u64,
    elapsed: CycleCount,
}

impl InputDevice {
    /// A device with an empty queue.
    pub const fn new() -> Self {
        Self {
            queue: Vec::new(),
            head: 0,
            delivered: 0,
            injected: 0,
            last_count: 0,
            last_capacity: 0,
            elapsed: CycleCount::new(0),
        }
    }

    /// The most events the device will hold before it refuses injection.
    ///
    /// A bound is needed because the queue is host-fed and guest-drained: a host
    /// that produces faster than a program polls would otherwise grow the queue
    /// without limit. Refusing at the bound is honest — a program that is not
    /// polling has told the device it is not ready — where dropping the oldest
    /// event would silently lose input.
    pub const QUEUE_LIMIT: usize = 4096;

    /// Injects an event from the host.
    ///
    /// This is the *only* way an event enters the device, and it is deliberately
    /// not reachable from a guest register: a guest that could inject its own
    /// events could fabricate input, and an input device a program can lie to is
    /// not an input device.
    pub fn inject(&mut self, event: Event) -> Result<(), InputError> {
        if self.queued() >= Self::QUEUE_LIMIT as u64 {
            return Err(InputError::QueueFull {
                limit: Self::QUEUE_LIMIT,
            });
        }
        self.queue.push(event);
        self.injected = self.injected.saturating_add(1);
        Ok(())
    }

    /// Injects several events, stopping at the first the queue refuses.
    ///
    /// Stopping rather than skipping is the point: a host adapter that skipped the
    /// event that did not fit would deliver a *reordered* stream, and a program
    /// that got its keys in the wrong order would have no way to tell.
    pub fn inject_all(&mut self, events: &[Event]) -> Result<usize, InputError> {
        for (index, event) in events.iter().enumerate() {
            self.inject(*event)?;
            let _ = index;
        }
        Ok(events.len())
    }

    /// How many events are waiting to be delivered.
    pub fn queued(&self) -> u64 {
        (self.queue.len() - self.head) as u64
    }

    /// How many events the guest has taken in total.
    pub const fn delivered(&self) -> u64 {
        self.delivered
    }

    /// How many events the host has injected in total.
    pub const fn injected(&self) -> u64 {
        self.injected
    }

    /// How many events the last `poll` returned.
    pub const fn last_count(&self) -> u64 {
        self.last_count
    }

    /// The capacity the last `poll` was given.
    pub const fn last_capacity(&self) -> u64 {
        self.last_capacity
    }

    /// Takes up to `capacity` events, writing them into `output`.
    ///
    /// Returns how many were written. Anything beyond `capacity` **stays queued**
    /// for the next call — this is the property the whole queue exists for, and a
    /// drain that dropped the remainder would lose input silently.
    pub fn poll(&mut self, output: &mut [Event], capacity: u64) -> Result<u64, InputError> {
        let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
        if capacity > output.len() {
            // A capacity larger than the caller's buffer would have the device
            // write past it. Refusing here means the device never trusts a number
            // it cannot check against something real.
            return Err(InputError::CapacityExceedsBuffer {
                capacity: capacity as u64,
                buffer: output.len() as u64,
            });
        }
        if capacity as u64 > MAX_POLL_CAPACITY {
            return Err(InputError::CapacityTooLarge {
                capacity,
                limit: MAX_POLL_CAPACITY,
            });
        }
        let available = self.queued();
        let take = core::cmp::min(available, capacity as u64) as usize;
        // The taken events are copied out in order, which is what makes delivery
        // order the same as arrival order.
        output[..take].copy_from_slice(&self.queue[self.head..self.head + take]);
        self.head += take;
        self.delivered = self.delivered.saturating_add(take as u64);
        self.last_count = take as u64;
        self.last_capacity = capacity as u64;
        // The consumed prefix is dropped once it is half the queue, so a long run
        // does not keep growing a `Vec` whose front is dead. Half rather than all,
        // so a program that polls one event at a time does not cause a copy on
        // every call.
        if self.head > 0 && self.head * 2 >= self.queue.len() {
            self.queue.drain(..self.head);
            self.head = 0;
        }
        Ok(take as u64)
    }

    /// A copy of the queued events, oldest first, without consuming them.
    ///
    /// For a test or a debugger that wants to see what is pending. The device
    /// itself never needs this, and nothing in the guest path uses it.
    pub fn pending(&self) -> &[Event] {
        &self.queue[self.head..]
    }

    /// A register's value, for a read.
    fn register(&self, offset: DeviceOffset) -> u64 {
        match offset.as_u64() {
            0 => 0,
            8 => self.queued(),
            16 => self.delivered,
            24 => self.injected,
            32 => INPUT_ABI_VERSION,
            40 => {
                if self.injected == 0 {
                    0
                } else {
                    STATUS_INJECTED
                }
            }
            48 => self.last_count,
            56 => self.last_capacity,
            _ => 0,
        }
    }
}

impl Default for InputDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for InputDevice {
    fn address_len(&self) -> u64 {
        REGISTER_BYTES
    }

    fn reset(&mut self) {
        // A reset discards the queue. Events the host injected but the guest never
        // saw are gone, which is correct: a reset is the start of a new machine,
        // and a key that was down before it is not down after it.
        *self = Self::new();
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        validate_register_access(offset, size)
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        _: u64,
    ) -> Result<(), DeviceError> {
        validate_register_access(offset, size)
    }

    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(self.register(offset))
    }

    fn write(&mut self, offset: DeviceOffset, size: DataSize, _: u64) -> Result<(), DeviceError> {
        // Only the poll register is writable, and a write to it *drains nothing*:
        // the driver reads the pending count and then reads events through a
        // different path, because a device that drained on a register write would
        // have nowhere to put the bytes. The register exists so a driver can
        // acknowledge a poll; the events themselves travel through the ABI in
        // Step 71, not through this register.
        self.validate_write(offset, size, 0)?;
        match offset.as_u64() {
            0 => Ok(()),
            8 | 16 | 24 | 32 | 40 | 48 | 56 => Err(DeviceError::WriteUnsupported),
            _ => Err(DeviceError::WriteUnsupported),
        }
    }

    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        self.validate_read(offset, size_of(output))?;
        output.copy_from_slice(&self.register(offset).to_le_bytes());
        Ok(())
    }

    fn tick(&mut self, elapsed: CycleCount) {
        self.elapsed = elapsed;
    }

    /// The queue, as a guest can see it.
    ///
    /// The whole queue, from the head onwards, then the head's position, then
    /// the two counters a program can read through the device's registers. The
    /// queue belongs here because a program *owns* it: it drains it, and a
    /// snapshot that left the drained events out would restore a device that
    /// handed the same keystroke to the program a second time. That is not a
    /// detail of the host's timing — it is the one thing about input a program
    /// can observe, because the program is what drained it.
    ///
    /// The delivered count is included for the same reason. The `elapsed` clock is
    /// not: nothing in the register file reports it and no guest instruction can
    /// observe it.
    ///
    /// The encoding is the events' own sixteen-byte record form, then five words:
    /// the queue's length, the head, the delivered count, the injected count, the
    /// last reported count, and the last reported capacity.
    fn snapshot(&self) -> Vec<u8> {
        let queued = &self.queue[self.head..];
        let mut bytes = Vec::with_capacity(queued.len() * EVENT_BYTES as usize + 6 * 8);
        for event in queued {
            bytes.extend_from_slice(&event.encode());
        }
        for value in [
            queued.len() as u64,
            self.head as u64,
            self.delivered,
            self.injected,
            self.last_count,
            self.last_capacity,
        ] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        const WORDS: usize = 6;
        // The trailer is *appended*, so it begins after the events rather than at
        // a fixed offset: two events make it start at 32, and reading it from 48
        // would run off the end of a snapshot of exactly the right length.
        let trailer_bytes = WORDS * 8;
        let event_bytes = EVENT_BYTES as usize;
        if bytes.len() < trailer_bytes || !(bytes.len() - trailer_bytes).is_multiple_of(event_bytes)
        {
            return Err(DeviceError::SnapshotShape {
                expected: bytes.len(),
                found: bytes.len(),
            });
        }
        let records = (bytes.len() - trailer_bytes) / event_bytes;
        let mut queue = Vec::new();
        for index in 0..records {
            let at = index * event_bytes;
            let event =
                Event::decode(&bytes[at..at + event_bytes]).ok_or(DeviceError::SnapshotShape {
                    expected: event_bytes,
                    found: 0,
                })?;
            queue.push(event);
        }
        let start = records * event_bytes;
        let mut words = [0u64; WORDS];
        for (index, word) in words.iter_mut().enumerate() {
            let at = start + index * 8;
            let mut chunk = [0u8; 8];
            chunk.copy_from_slice(&bytes[at..at + 8]);
            *word = u64::from_le_bytes(chunk);
        }
        self.queue = queue;
        self.head = 0;
        self.delivered = words[2];
        self.injected = words[3];
        self.last_count = words[4];
        self.last_capacity = words[5];
        Ok(())
    }
}

/// The access size a `peek` of `len` bytes is asking for.
///
/// `peek` is given a byte buffer, so the width is checked against what the buffer
/// actually is rather than against a `DataSize` the caller chose.
fn size_of(output: &[u8]) -> DataSize {
    match output.len() {
        1 => DataSize::Byte,
        2 => DataSize::Half,
        4 => DataSize::Word,
        _ => DataSize::Double,
    }
}

fn validate_register_access(offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
    if size != DataSize::Double {
        return Err(DeviceError::UnsupportedSize(size));
    }
    if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= REGISTER_BYTES {
        return Err(DeviceError::InvalidRange {
            offset,
            bytes: u64::from(size.bytes()),
        });
    }
    Ok(())
}

/// Why an input operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    /// The queue is at its limit, so an injection is refused rather than dropping
    /// an event the host already produced.
    QueueFull {
        /// The queue's limit.
        limit: usize,
    },
    /// A poll asked for more events than the caller's buffer holds.
    CapacityExceedsBuffer {
        /// The capacity that was asked for.
        capacity: u64,
        /// The buffer the caller supplied.
        buffer: u64,
    },
    /// A poll asked for more events than the device will write at once.
    CapacityTooLarge {
        /// The capacity that was asked for.
        capacity: usize,
        /// The largest capacity allowed.
        limit: u64,
    },
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull { limit } => {
                write!(f, "the input queue is full at {limit} events")
            }
            Self::CapacityExceedsBuffer { capacity, buffer } => write!(
                f,
                "a poll for {capacity} events needs a buffer of at least {capacity}, \
                 and the caller's holds {buffer}"
            ),
            Self::CapacityTooLarge { capacity, limit } => write!(
                f,
                "a poll for {capacity} events is more than the {limit} event limit"
            ),
        }
    }
}

impl core::error::Error for InputError {}
