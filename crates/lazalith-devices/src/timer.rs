//! A clock the guest can read through memory-mapped I/O.
//!
//! # Why a device at all
//!
//! `Time` and `Sleep` are syscalls, and they are enough for a program that asks the
//! kernel how long it has been running. They are not enough for a program that wants
//! to *measure* something: a syscall costs the program a trap, and a measurement that
//! interrupts what it measures is a measurement of the trap.
//!
//! So the cycle count is also a device, and a program reads it the same way it reads
//! anything else: by loading from an address. That is the whole reason this exists, and
//! it is why the register is 64 bits wide regardless of the target's word size — a
//! cycle count that wrapped at 2³² on a 32-bit target would be a clock that lied
//! after about seven minutes.
//!
//! # What it does not do
//!
//! It does not count down, fire interrupts, or have a programmable period. Those are
//! a timer *controller*, which is a different device with a different job; this one
//! reports a counter, and a program that wants a delay computes one and calls `Sleep`.

use alloc::vec::Vec;

use crate::{Device, DeviceError, DeviceOffset};
use lazalith_isa::DataSize;
use lazalith_types::CycleCount;

/// How many bytes the device's register window occupies.
pub const TIMER_REGISTER_BYTES: u64 = 8;
/// The cycle count, at offset 0.
pub const REGISTER_CYCLES: DeviceOffset = DeviceOffset::new(0);

/// A counter the host ticks and the guest reads.
/// A counter the host ticks and the guest reads.
#[derive(Debug)]
pub struct TimerDevice {
    elapsed: CycleCount,
}

impl Default for TimerDevice {
    /// A counter at zero, because a caller that says `Default` has not said when.
    fn default() -> Self {
        Self::new()
    }
}

impl TimerDevice {
    /// A counter at zero.
    pub const fn new() -> Self {
        Self {
            elapsed: CycleCount::new(0),
        }
    }

    /// The cycle count as the guest sees it.
    pub const fn cycles(&self) -> CycleCount {
        self.elapsed
    }
}

impl Device for TimerDevice {
    fn address_len(&self) -> u64 {
        TIMER_REGISTER_BYTES
    }

    fn reset(&mut self) {
        // A reset puts the counter back to zero rather than leaving it where the
        // previous machine left it: a program that reads the counter twice around a
        // reset must see it go backwards, or a run that was never the same run twice
        // would look like it was.
        self.elapsed = CycleCount::new(0);
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if offset != REGISTER_CYCLES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        Ok(())
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        // Writes are refused rather than ignored. A write to a counter that silently
        // does nothing is a program's idea of "reset the clock" quietly failing, and
        // the program would then measure a span it believes it controlled.
        let _ = (offset, size, value);
        Err(DeviceError::WriteUnsupported)
    }

    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(self.elapsed.as_u64())
    }

    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_write(offset, size, value)
    }

    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        let _ = output;
        self.validate_read(offset, DataSize::Double)
    }

    fn tick(&mut self, elapsed: CycleCount) {
        // **Assigned, not accumulated.** This was the one device out of five that
        // added, and the machine's own `DeviceManager::tick` passes the *new absolute*
        // elapsed time — so a timer that added was computing a sum of absolute
        // timestamps. On a machine whose clock only ever moved once, that read as
        // correct: the first tick delivered 1_234 and the counter became 1_234. The
        // moment the clock started moving during execution, the second tick delivered
        // 1_235 and the counter became 2_469, which is a time the machine was never at.
        //
        // The guest reads this register as "what time is it", so absolute is the only
        // answer that means anything. `elapsed` here is a clock reading, not a delta;
        // a device that wants a delta is a device that wants a different method, and
        // the trait documents which one it is getting.
        self.elapsed = elapsed;
    }

    /// The counter, as eight bytes, for a machine snapshot.
    ///
    /// A snapshot of a program that read the clock, restored onto a machine with a
    /// different one, would otherwise read a time that never happened — so the counter
    /// is guest-visible state and belongs in the snapshot with everything else the
    /// guest can see.
    fn snapshot(&self) -> Vec<u8> {
        self.elapsed.as_u64().to_le_bytes().to_vec()
    }

    /// Puts a counter back, refusing a snapshot that is not exactly one counter.
    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError> {
        let raw: [u8; 8] = bytes.try_into().map_err(|_| DeviceError::SnapshotShape {
            expected: 8,
            found: bytes.len(),
        })?;
        self.elapsed = CycleCount::new(u64::from_le_bytes(raw));
        Ok(())
    }
}
