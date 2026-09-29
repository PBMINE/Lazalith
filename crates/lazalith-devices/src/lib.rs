#![no_std]

extern crate alloc;

mod audio;
mod backend;
mod console;
mod display;
mod graphics;
pub mod host_input;
pub mod input;
mod input_backend;
mod net;
mod storage;
pub mod timer;
pub use audio::{
    AUDIO_ABI_VERSION, AUDIO_CONTROL_CAPTURE, AUDIO_CONTROL_PLAY, AUDIO_REGISTER_BYTES,
    AUDIO_REGISTER_CHANNELS, AUDIO_REGISTER_CONTROL, AUDIO_REGISTER_DATA, AUDIO_REGISTER_FORMAT,
    AUDIO_REGISTER_LEVEL, AUDIO_REGISTER_RATE, AUDIO_REGISTER_STATUS, AUDIO_REGISTER_THRESHOLD,
    AUDIO_SNAPSHOT_BYTES, AUDIO_STATUS_BAD_FORMAT, AUDIO_STATUS_OVERRUN, AUDIO_STATUS_READY,
    AUDIO_STATUS_UNDERRUN, AudioBackend, AudioDevice, AudioDrain, AudioError, AudioFormat,
    AudioFrame, DEFAULT_RING_SAMPLES, MAX_CHANNELS, MAX_RATE, NullAudioBackend,
    RecordingAudioBackend, SampleFormat,
};
pub use backend::{
    AbsentBlockBackend, Backend, BackendError, BackendIdentity, BackendKind, BlockBackend,
    CopyOnWriteBlockBackend, MemoryBlockBackend, SECTOR_BYTES,
};
pub use console::{CONSOLE_REGISTER_BYTES, ConsoleDevice};
pub use display::{
    DISPLAY_ABI_VERSION, DisplayDevice, DisplayError, MAX_DIMENSION, PIXEL_BYTES, PresentedFrame,
    REGISTER_ABI_VERSION, REGISTER_BYTES, REGISTER_BYTES as DISPLAY_REGISTER_BYTES,
    REGISTER_FRAMEBUFFER, REGISTER_HEIGHT, REGISTER_LAST_PRESENT, REGISTER_PRESENT,
    REGISTER_PRESENT_COUNT, REGISTER_STATUS, REGISTER_WIDTH, STATUS_PRESENTED, frame_bytes,
    pixel_at, zeroed_framebuffer,
};
pub use graphics::{
    DisplayBackend, DisplayBackendError, DisplayFrame, DisplayProfile, DisplayPump,
    HeadlessDisplayBackend, frame_len, open_window, presented_geometry, pump_display,
};
pub use host_input::{
    HostAction, HostKey, HostScript, KEY_BACKSLASH, KEY_BACKSPACE, KEY_COMMA, KEY_DIGIT_FIRST,
    KEY_DIGIT_LAST, KEY_ENTER, KEY_EQUALS, KEY_ESCAPE, KEY_LEFT_ALT, KEY_LEFT_CONTROL,
    KEY_LEFT_SHIFT, KEY_LEFT_SUPER, KEY_LETTER_FIRST, KEY_LETTER_LAST, KEY_MAX, KEY_MINUS,
    KEY_PERIOD, KEY_RIGHT_ALT, KEY_RIGHT_CONTROL, KEY_RIGHT_SHIFT, KEY_RIGHT_SUPER, KEY_SEMICOLON,
    KEY_SLASH, KEY_SPACE, KEY_TAB, KEY_UNKNOWN, digit_of, is_digit, is_letter, letter_of,
};
pub use input::{
    EVENT_BYTES, Event, EventKind, EventKindValue, INPUT_ABI_VERSION, InputDevice, InputError,
    MAX_POLL_CAPACITY, REGISTER_ABI_VERSION as INPUT_REGISTER_ABI_VERSION,
    REGISTER_BYTES as INPUT_REGISTER_BYTES, REGISTER_DELIVERED, REGISTER_INJECTED,
    REGISTER_LAST_CAPACITY, REGISTER_LAST_COUNT, REGISTER_PENDING,
    REGISTER_POLL as INPUT_REGISTER_POLL, REGISTER_STATUS as INPUT_REGISTER_STATUS,
    STATUS_INJECTED,
};
pub use input_backend::usb;
pub use input_backend::{
    AbsentInputBackend, InputBackend, InputBackendError, InputProfile, InputPump,
    ScriptedInputBackend, pump_input,
};
pub use net::{
    LazFrame, MAX_FRAME_BYTES, MTU, NET_ABI_VERSION, NET_CONTROL_INTERRUPTS, NET_CONTROL_TX_ACTIVE,
    NET_CONTROL_TX_COMMIT, NET_CONTROL_UP, NET_REGISTER_BYTES, NET_REGISTER_CONTROL,
    NET_REGISTER_MAC_HIGH, NET_REGISTER_MAC_LOW, NET_REGISTER_RX_DATA, NET_REGISTER_RX_LENGTH,
    NET_REGISTER_STATUS, NET_REGISTER_TX_DATA, NET_REGISTER_TX_LENGTH, NET_SNAPSHOT_BYTES,
    NET_STATUS_LINK_DOWN, NET_STATUS_RX_READY, NET_STATUS_TX_READY, NET_STATUS_TX_REFUSED,
    NetError, NetworkBackend, NetworkDevice, NullNetworkBackend, PumpOutcome, RX_QUEUE_FRAMES,
    RecordingNetworkBackend, TX_BUFFER_BYTES, TransmitOutcome,
};
pub use storage::{
    BLOCK_ABI_VERSION, BLOCK_REGISTER_BYTES, BLOCK_REGISTER_CAPACITY, BLOCK_REGISTER_COMMAND,
    BLOCK_REGISTER_DATA, BLOCK_REGISTER_REMAINING, BLOCK_REGISTER_SECTOR, BLOCK_REGISTER_STATUS,
    BLOCK_SNAPSHOT_BYTES, BLOCK_STATUS_BUSY, BLOCK_STATUS_FAILED, BLOCK_STATUS_READABLE,
    BLOCK_STATUS_WRITABLE, BlockDevice, BlockError, COMMAND_READ, COMMAND_WRITE,
};
pub use timer::{REGISTER_CYCLES as TIMER_REGISTER_CYCLES, TIMER_REGISTER_BYTES, TimerDevice};

use alloc::{boxed::Box, collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_isa::DataSize;
use lazalith_types::{ClockOverflow, VirtualClock};
pub use lazalith_types::{CycleCount, DeviceId, DeviceOffset, InterruptId};

#[derive(Debug)]
pub enum DeviceError {
    UnknownDevice(DeviceId),
    DuplicateDevice(DeviceId),
    EmptyDevice,
    InvalidRange {
        offset: DeviceOffset,
        bytes: u64,
    },
    UnsupportedSize(DataSize),
    ReadUnsupported,
    WriteUnsupported,
    Unpeekable,
    Capacity,
    Allocation(TryReserveError),
    Clock(ClockOverflow),
    /// A snapshot's bytes are not the shape this device produces.
    ///
    /// This is a separate case from `InvalidRange` because a *range* is about
    /// where in the device's registers a read or write landed, and a snapshot is
    /// not a register access at all: it is the device's own encoding of its
    /// state. A length that differs is refused rather than padded or truncated.
    SnapshotShape {
        /// How many bytes the device's own `snapshot` produces.
        expected: usize,
        /// How many bytes were offered.
        found: usize,
    },
    /// A block device refused an operation.
    ///
    /// Its own variant, holding a [`BlockError`] rather than a `BackendError`,
    /// because "the sector is past the end of the disk" and "you asked for the wrong
    /// register" are different facts and a caller that has to match on one of them
    /// should be able to. The host storage's own reason is carried *inside*
    /// `BlockError::Storage` rather than flattened away, so a guest-visible fault
    /// still says whether the disk was read-only or the base refused a write.
    Block(crate::BlockError),
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "device operation rejected: {self:?}")
    }
}

impl Error for DeviceError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Clock(source) => Some(source),
            _ => None,
        }
    }
}

pub trait Device: fmt::Debug {
    fn address_len(&self) -> u64;
    fn reset(&mut self);
    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError>;
    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError>;
    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError>;
    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError>;
    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError>;

    /// Tells the device what time it is now.
    ///
    /// **`elapsed` is the machine's new *absolute* virtual time, not a delta, and a
    /// device that stores it must store it rather than add it.**
    ///
    /// That sentence is here because one device got it wrong for the whole of B5–B19
    /// and nothing noticed. `TimerDevice::tick` added the argument to its own counter,
    /// so it computed a sum of absolute timestamps. That reads as correct on a machine
    /// whose clock moves exactly once: the first tick delivers 1_234 and the counter
    /// becomes 1_234. The moment the machine charged cycles per retired instruction, the
    /// second tick delivered 1_235 and the counter became 2_469 — a time the machine was
    /// never at, readable by a guest through a register that is documented as
    /// "the cycle count as the guest sees it".
    ///
    /// The cost of the bug was hidden by the clock defect it was waiting for. A delta
    /// would be a defensible thing for a device to want, and the trait would then need
    /// two methods; it has one, and what it passes is a clock reading.
    fn tick(&mut self, elapsed: CycleCount);

    /// This device's guest-visible state, as bytes.
    ///
    /// A machine snapshot holds one of these per device, so this is the only way
    /// a device's state can be captured. What belongs in the bytes is a rule and
    /// not a type: **only state a guest can observe or affect**. A display's
    /// window geometry and its frame count belong — a program reads both through
    /// MMIO and is entitled to see them again after a restore. The host clock a
    /// device ticks against does not, and neither does a host file handle, a
    /// buffer the host is using to stage a window, or anything else the guest
    /// cannot name.
    ///
    /// The encoding is the device's own, and that is deliberate: a device's state
    /// is a device's business, and a common encoding in this trait would have to
    /// be a lowest common denominator that lost whatever made each device
    /// different. [`Device::restore`] is what checks the bytes are this device's
    /// own, so a mismatch is a refusal rather than a misreading.
    fn snapshot(&self) -> Vec<u8>;

    /// An interrupt this device has raised, if any, taken rather than read.
    ///
    /// **Added by B10**, for §29.s "interrupts". A device that consumes a *rate* 2014 audio
    /// filling a ring, a disk finishing a transfer 2014 learns something in `tick`, and a
    /// guest that has to be told has no other way to learn it. Polling a status register
    /// makes the guest busy-wait at its own rate, which is the wrong rate: the device
    /// knows when its ring is half empty and the guest does not.
    ///
    /// Taken rather than read so that a raised interrupt is delivered exactly once. A
    /// `&self` peek would be re-delivered every cycle, and a guest that cleared its own
    /// interrupt would race with the machine reading it.
    ///
    /// The machine calls this after every `tick`. `None` is the default, so a device that
    /// has no interrupts implements nothing extra 2014 which is why this is additive and
    /// why no existing device needed changing.
    fn take_interrupt(&mut self) -> Option<InterruptId> {
        None
    }

    /// Puts back a state this device produced.
    ///
    /// `bytes` must be exactly what this device's `snapshot` would produce. A
    /// length that differs is refused rather than padded or truncated, because a
    /// restored device that silently kept some of its old state is a device whose
    /// behaviour after a restore depends on what it happened to be doing before.
    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError>;
}

#[derive(Debug)]
pub enum NoDevice {}

impl Device for NoDevice {
    fn address_len(&self) -> u64 {
        match *self {}
    }
    fn reset(&mut self) {
        match *self {}
    }
    fn validate_read(&self, _: DeviceOffset, _: DataSize) -> Result<(), DeviceError> {
        match *self {}
    }
    fn validate_write(&self, _: DeviceOffset, _: DataSize, _: u64) -> Result<(), DeviceError> {
        match *self {}
    }
    fn read(&mut self, _: DeviceOffset, _: DataSize) -> Result<u64, DeviceError> {
        match *self {}
    }
    fn write(&mut self, _: DeviceOffset, _: DataSize, _: u64) -> Result<(), DeviceError> {
        match *self {}
    }
    fn peek(&self, _: DeviceOffset, _: &mut [u8]) -> Result<(), DeviceError> {
        match *self {}
    }
    fn tick(&mut self, _: CycleCount) {
        match *self {}
    }
    fn take_interrupt(&mut self) -> Option<InterruptId> {
        None
    }
    fn snapshot(&self) -> Vec<u8> {
        match *self {}
    }
    fn restore(&mut self, _bytes: &[u8]) -> Result<(), DeviceError> {
        match *self {}
    }
}

/// A *heterogeneous* device, erased.
///
/// A [`DeviceManager`] holds one concrete device type, so a machine built over it
/// can have a console, *or* a timer, *or* a display, *or* an input device — never
/// two of different kinds at once. This impl is what removes that restriction, and
/// it is the reason a machine profile can describe a device *inventory* rather
/// than a device *kind*.
///
/// Nothing above the device layer had to change to get it.
/// `DeviceManager<Box<dyn Device>>` is a `DeviceManager` of some `D: Device`, and
/// every generic in `lazalith-memory` and `lazalith-machine` is already written in
/// terms of `D` — so `LazalithMachine<Box<dyn Device>>` is a machine, built from
/// the same constructor, with the same `map_device`, the same routing by
/// `DeviceId`, and the same per-device snapshot contract. Every existing caller
/// keeps its concrete `D` and is untouched.
///
/// The cost is a dynamic call per register access, which is why a machine with one
/// kind of device still should not do this: `LazalithMachine<ConsoleDevice>` stays
/// monomorphic and stays fast. `NoDevice` also still means something an empty
/// erased list does not — "a machine that can hold no device at all", rather than
/// "a machine holding nothing".
impl Device for Box<dyn Device> {
    fn address_len(&self) -> u64 {
        (**self).address_len()
    }
    fn reset(&mut self) {
        (**self).reset()
    }
    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        (**self).validate_read(offset, size)
    }
    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        (**self).validate_write(offset, size, value)
    }
    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        (**self).read(offset, size)
    }
    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        (**self).write(offset, size, value)
    }
    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        (**self).peek(offset, output)
    }
    fn tick(&mut self, elapsed: CycleCount) {
        (**self).tick(elapsed)
    }
    fn snapshot(&self) -> Vec<u8> {
        (**self).snapshot()
    }
    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        (**self).restore(bytes)
    }
}

#[derive(Debug)]
struct Entry<D> {
    id: DeviceId,
    length: u64,
    device: D,
}

#[derive(Debug)]
pub struct DeviceManager<D: Device> {
    entries: Vec<Entry<D>>,
    clock: VirtualClock,
}

impl<D: Device> Default for DeviceManager<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: Device> DeviceManager<D> {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            clock: VirtualClock::new(),
        }
    }

    pub fn insert(&mut self, id: DeviceId, mut device: D) -> Result<(), DeviceError> {
        if self.entries.iter().any(|entry| entry.id == id) {
            return Err(DeviceError::DuplicateDevice(id));
        }
        let length = device.address_len();
        if length == 0 {
            return Err(DeviceError::EmptyDevice);
        }
        self.entries
            .try_reserve(1)
            .map_err(DeviceError::Allocation)?;
        device.tick(self.clock.elapsed());
        self.entries.push(Entry { id, length, device });
        Ok(())
    }

    fn entry(&self, id: DeviceId) -> Result<&Entry<D>, DeviceError> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .ok_or(DeviceError::UnknownDevice(id))
    }

    fn entry_mut(&mut self, id: DeviceId) -> Result<&mut Entry<D>, DeviceError> {
        self.entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or(DeviceError::UnknownDevice(id))
    }

    pub fn device(&self, id: DeviceId) -> Result<&D, DeviceError> {
        Ok(&self.entry(id)?.device)
    }
    /// A device, mutably.
    ///
    /// The counterpart to [`DeviceManager::device`], and the access a host needs to
    /// feed one: an input device is fed by injection and a display is told what to
    /// draw, and neither is a state restore. Rebuilding a device from its own
    /// snapshot is the way to *put back* a state, not the way to act on a device,
    /// and a host that had to do it that way would be reconstructing a queue in
    /// order to append to it.
    pub fn device_mut(&mut self, id: DeviceId) -> Result<&mut D, DeviceError> {
        Ok(&mut self.entry_mut(id)?.device)
    }
    pub fn address_len(&self, id: DeviceId) -> Result<u64, DeviceError> {
        Ok(self.entry(id)?.length)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub const fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// Every device's guest-visible state, in device order.
    ///
    /// The order is insertion order, which is the order a machine's setup used, so
    /// a snapshot's `entries[i]` belongs to the same device as the manager's
    /// `entries[i]` was. Restoring walks the same order and checks each device's
    /// own encoding, so a snapshot taken from a machine with a different set of
    /// devices is refused rather than applied to the wrong one.
    ///
    /// The manager's `VirtualClock` is **not** here. It is the host's notion of
    /// when devices were ticked, no guest can read it, and a machine snapshot is
    /// about what a guest can see.
    pub fn snapshot(&self) -> Vec<(DeviceId, Vec<u8>)> {
        self.entries
            .iter()
            .map(|entry| (entry.id, entry.device.snapshot()))
            .collect()
    }

    /// Takes every interrupt the devices have raised, in device order.
    ///
    /// **Returns them rather than delivering them**: this crate does not know what an
    /// interrupt controller is, and the machine does. A device that raised twice before
    /// anyone asked loses the first 2014 which is the same "interrupts are edges" rule a
    /// real controller has, and the reason a device keeps at most one pending.
    pub fn take_interrupts(&mut self) -> Vec<InterruptId> {
        let mut raised = Vec::new();
        for entry in &mut self.entries {
            if let Some(id) = entry.device.take_interrupt() {
                raised.push(id);
            }
        }
        raised
    }

    /// Puts every device.s state back, in the order `snapshot` produced them.
    pub fn restore(&mut self, states: &[(DeviceId, Vec<u8>)]) -> Result<(), DeviceError> {
        if states.len() != self.entries.len() {
            return Err(DeviceError::SnapshotShape {
                expected: self.entries.len(),
                found: states.len(),
            });
        }
        // Validated before anything is written, so a snapshot that names the
        // wrong device does not leave half of them restored.
        for (index, (id, _)) in states.iter().enumerate() {
            if self.entries[index].id != *id {
                return Err(DeviceError::UnknownDevice(*id));
            }
        }
        for (index, (_, bytes)) in states.iter().enumerate() {
            self.entries[index].device.restore(bytes)?;
        }
        Ok(())
    }

    fn validate_range(
        &self,
        id: DeviceId,
        offset: DeviceOffset,
        bytes: u64,
    ) -> Result<(), DeviceError> {
        let length = self.address_len(id)?;
        if bytes == 0
            || offset
                .as_u64()
                .checked_add(bytes)
                .is_none_or(|end| end > length)
        {
            return Err(DeviceError::InvalidRange { offset, bytes });
        }
        Ok(())
    }

    pub fn read(
        &mut self,
        id: DeviceId,
        offset: DeviceOffset,
        size: DataSize,
    ) -> Result<u64, DeviceError> {
        self.validate_range(id, offset, u64::from(size.bytes()))?;
        self.device(id)?.validate_read(offset, size)?;
        self.entry_mut(id)?.device.read(offset, size)
    }

    pub fn write(
        &mut self,
        id: DeviceId,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_range(id, offset, u64::from(size.bytes()))?;
        let value = value & (u64::MAX >> (64 - u32::from(size.bytes()) * 8));
        self.device(id)?.validate_write(offset, size, value)?;
        self.entry_mut(id)?.device.write(offset, size, value)
    }

    pub fn peek(
        &self,
        id: DeviceId,
        offset: DeviceOffset,
        output: &mut [u8],
    ) -> Result<(), DeviceError> {
        self.validate_range(id, offset, output.len() as u64)?;
        self.device(id)?.peek(offset, output)
    }

    pub fn reset(&mut self) {
        self.reset_at(CycleCount::new(0));
    }

    pub fn reset_at(&mut self, elapsed: CycleCount) {
        for entry in &mut self.entries {
            entry.device.reset();
        }
        if elapsed != CycleCount::new(0) {
            for entry in &mut self.entries {
                entry.device.tick(elapsed);
            }
        }
        self.clock = VirtualClock::at(elapsed);
    }

    pub fn tick(&mut self, delta: CycleCount) -> Result<(), DeviceError> {
        if delta == CycleCount::new(0) {
            return Ok(());
        }
        let clock = match self.clock.advanced(delta) {
            Ok(clock) => clock,
            Err(source) => return Err(DeviceError::Clock(source)),
        };
        for entry in &mut self.entries {
            entry.device.tick(clock.elapsed());
        }
        self.clock = clock;
        Ok(())
    }

    /// Sets virtual time outright, forwards or backwards.
    ///
    /// **The restore path, and a trap if used alone.** `tick` only moves time
    /// forward, so a machine snapshot could not be put back into a machine that had
    /// moved on since — and a snapshot that can only be restored into a machine at or
    /// before its own time is barely a snapshot. Devices already rewind: each one's
    /// `restore` writes its elapsed count straight back, so `DeviceManager` refusing to
    /// rewind was an inconsistency rather than a policy.
    ///
    /// It does **not** tick any device. A device's own elapsed count is restored by its
    /// `restore`, from its own snapshot bytes, and ticking it here as well would apply
    /// the same interval twice. So the rule is: restore the devices, then set the
    /// clock, and do not do it the other way round.
    pub fn set_clock(&mut self, elapsed: CycleCount) {
        self.clock = VirtualClock::at(elapsed);
    }
}
