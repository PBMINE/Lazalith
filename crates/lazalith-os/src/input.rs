//! The LazOS input driver.
//!
//! # Where this sits
//!
//! ```text
//! host input source          (SDL3 in a graphical host, a script in a test)
//!     ↓ HostInputAdapter     (the only component that knows what a key is)
//! Virtual Input Device       (the Step 69 queue, in guest-visible records)
//!     ↓ input ABI            (input_poll)
//! LazOS input driver         ← this file
//!     ↓ std::input           (the SDK: pure Lazen)
//! Lazen application
//! ```
//!
//! The driver is a *drain*. `input_poll` takes the address of an array the caller
//! owns and a count of how many records fit in it, and writes whole records
//! there. The driver allocates no per-event memory and owns no queue of its own:
//! the queue is the device's, and the array is the program's.
//!
//! # Why the records are written into the program
//!
//! A v1 call cannot return a length *and* a buffer, so the buffer is an
//! argument. That is not a limitation worked around; it is the property that
//! matters. A program that stops polling simply stops receiving events, and one
//! that is not scheduled cannot lose an event that was already delivered, because
//! nothing was delivered. There is no ring the kernel owns for a program to
//! overflow.
//!
//! # The remainder is the whole design
//!
//! If the queue holds more events than the caller's array can take, the rest
//! **stays queued at the device** and the next call continues from there. A
//! driver that dropped the remainder would lose input silently, and a program
//! polling once per frame at thirty frames a second would lose any key tapped
//! faster than that. The device already implements this; the driver's job is not
//! to break it, and `a_poll_that_cannot_take_everything_keeps_the_rest` in the
//! OS tests holds that through the ABI.
//!
//! # What this driver does not know
//!
//! Nothing about a keyboard. It never sees a host key code, a scancode, or a
//! modifier convention; it sees the device's records, which are already Lazen's
//! own. The host adapter is the only component that translates, so the same
//! program under a graphical host and under a script receives identical records.

use alloc::vec::Vec;

use lazalith_devices::{Event, InputDevice, InputError};
use lazalith_os_abi::{
    INPUT_EVENT_RECORD_SIZE, InputEventRecord, IoResult, SyscallError, SyscallStatus, TaggedOutcome,
};
use lazalith_types::{ArchitectureConfig, VirtualAddress};

use crate::syscall::{
    KernelService, ServiceOutcome, UserMemoryContext, ValidatedSyscall, ValidatedSyscallKind,
};

/// The LazOS input driver, sitting on an input device.
#[derive(Debug)]
pub struct InputService {
    device: InputDevice,
    architecture: ArchitectureConfig,
    /// Records the device hands back, kept between calls so a poll does not
    /// allocate. Its length is the caller's capacity, and it holds no state the
    /// device does not already own.
    drained: Vec<Event>,
}

impl InputService {
    /// A driver over a device with nothing queued.
    pub fn new() -> Self {
        Self::with_architecture(ArchitectureConfig::lz64())
    }

    /// A driver whose result records are sized for `architecture`.
    ///
    /// The record holds a count as a word, and the ABI checks that word against
    /// the target'"'"'s width. LZ64 is the default because it is the only target the
    /// Lazen pipeline generates, and a kernel with no input traffic is unaffected
    /// either way.
    pub const fn with_architecture(architecture: ArchitectureConfig) -> Self {
        Self {
            device: InputDevice::new(),
            architecture,
            drained: Vec::new(),
        }
    }

    /// A driver over an existing device, for a caller that has queued events
    /// already.
    ///
    /// This is how a *test* or a host adapter feeds a script without a guest
    /// having to arrive first. A Lazen program cannot reach it: the SDK's `poll`
    /// goes through `input_poll`, not through here.
    pub fn with_device(device: InputDevice) -> Self {
        Self {
            device,
            architecture: ArchitectureConfig::lz64(),
            drained: Vec::new(),
        }
    }

    /// A driver over an existing device, with records sized for `architecture`.
    pub const fn with_device_for(architecture: ArchitectureConfig, device: InputDevice) -> Self {
        Self {
            device,
            architecture,
            drained: Vec::new(),
        }
    }

    /// The device, so a host adapter can queue events and a debugger can see what
    /// is pending.
    pub const fn device(&self) -> &InputDevice {
        &self.device
    }

    /// The device, mutably, for a caller that injects a script.
    pub fn device_mut(&mut self) -> &mut InputDevice {
        &mut self.device
    }

    /// How many events the device still holds.
    pub fn pending(&self) -> u64 {
        self.device.queued()
    }

    /// How many events have reached the guest across every poll.
    pub const fn delivered(&self) -> u64 {
        self.device.delivered()
    }

    /// `input_poll`: write up to `capacity` records into the caller's array.
    ///
    /// Each record goes in through the validated memory context, so a range that
    /// was checked at validation time is still checked here. A call that became
    /// invalid between the two — a process whose mapping was revoked, say —
    /// therefore writes what it could and reports how far it got, rather than
    /// faulting the machine or claiming a count it did not reach.
    fn poll(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        events: VirtualAddress,
        capacity: u32,
        result: VirtualAddress,
    ) -> ServiceOutcome {
        let wanted = capacity as usize;
        if self.drained.len() < wanted {
            self.drained.resize(wanted, Event::default());
        }
        let count = match self
            .device
            .poll(&mut self.drained[..wanted], u64::from(capacity))
        {
            Ok(count) => count as usize,
            // Validation already checked that the caller's array holds
            // `capacity` records and that the count is a `u32`, so the device can
            // only refuse for a limit this driver did not apply. It is reported
            // rather than approximated, because returning a count the caller did
            // not ask for is worse than a refusal.
            //
            // `QueueFull` is the device refusing an *injection*, and a poll can
            // only ever shrink the queue, so it is unreachable here. It is named
            // rather than folded into a wildcard so that adding a refusal to the
            // device is a compile error in this arm instead of a silently
            // mis-mapped status.
            Err(InputError::CapacityExceedsBuffer { .. } | InputError::CapacityTooLarge { .. }) => {
                return self.report(memory, result, 0, SyscallStatus::InvalidArgument);
            }
            Err(InputError::QueueFull { .. }) => {
                return self.report(memory, result, 0, SyscallStatus::InvalidState);
            }
        };
        let mut written: u64 = 0;
        for (index, event) in self.drained[..count].iter().enumerate() {
            let record =
                InputEventRecord::pointer(event.kind_value(), event.code, event.x, event.y);
            let Some(at) = events.checked_add(index as u64 * INPUT_EVENT_RECORD_SIZE as u64) else {
                // The range validated, so this cannot be reached; a checked add
                // that fails is reported rather than wrapped into an address the
                // program never offered. The events already written stay
                // delivered, and the count says so.
                return self.report(memory, result, index as u64, SyscallStatus::InvalidPointer);
            };
            if memory.write_bytes(at, &record.encode()).is_err() {
                return self.report(memory, result, index as u64, SyscallStatus::InvalidPointer);
            }
            written = index as u64 + 1;
        }
        self.report(memory, result, written, SyscallStatus::Ok)
    }

    /// Writes the `IoResult` saying how many records reached the guest.
    ///
    /// The count goes here rather than in the return value because a v1 call
    /// returns one word and that word is the status — the same reason `write` and
    /// `time` report through a record. It is written on *every* path, including a
    /// refusal, so a program whose array turned out to be unusable can read how
    /// far it got rather than only that it stopped.
    fn report(
        &self,
        memory: &mut UserMemoryContext<'_>,
        result: VirtualAddress,
        transferred: u64,
        status: SyscallStatus,
    ) -> ServiceOutcome {
        let record = match IoResult::new(self.architecture, transferred, status) {
            Ok(record) => record,
            Err(_) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::InvalidArgument,
                    0,
                ));
            }
        };
        if memory.write_bytes(result, &record.encode()).is_err() {
            return ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::InvalidPointer, 0));
        }
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }
}

impl Default for InputService {
    /// An LZ64 driver, which is the only target the Lazen pipeline generates.
    fn default() -> Self {
        Self::new()
    }
}

impl KernelService for InputService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::InputPoll {
                events,
                capacity,
                result,
            } => self.poll(memory, events, capacity, result),
            // A driver refuses a call that is not its own rather than answering
            // something plausible. Routing is the kernel's job, and a service that
            // guessed would hide a routing mistake behind a working answer.
            _ => ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::NotSupported, 0)),
        }
    }
}
