use crate::{Device, DeviceError, DeviceOffset};
use alloc::vec::Vec;
use lazalith_isa::DataSize;
use lazalith_types::CycleCount;

#[derive(Debug)]
pub struct ConsoleDevice {
    output: Vec<u8>,
    capacity: usize,
    elapsed: CycleCount,
}

impl ConsoleDevice {
    pub fn new(capacity: usize) -> Result<Self, DeviceError> {
        let mut output = Vec::new();
        output
            .try_reserve_exact(capacity)
            .map_err(DeviceError::Allocation)?;
        Ok(Self {
            output,
            capacity,
            elapsed: CycleCount::new(0),
        })
    }

    pub fn output(&self) -> &[u8] {
        &self.output
    }
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
    pub const fn elapsed(&self) -> CycleCount {
        self.elapsed
    }

    fn validate_register(offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if offset.as_u64() != 0 {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        if size != DataSize::Byte {
            return Err(DeviceError::UnsupportedSize(size));
        }
        Ok(())
    }
}

impl Device for ConsoleDevice {
    fn address_len(&self) -> u64 {
        1
    }
    fn reset(&mut self) {
        self.output.clear();
        self.elapsed = CycleCount::new(0);
    }
    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        Self::validate_register(offset, size)?;
        Err(DeviceError::ReadUnsupported)
    }
    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        _: u64,
    ) -> Result<(), DeviceError> {
        Self::validate_register(offset, size)?;
        if self.output.len() == self.capacity {
            return Err(DeviceError::Capacity);
        }
        Ok(())
    }
    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Err(DeviceError::ReadUnsupported)
    }
    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_write(offset, size, value)?;
        self.output.push(value as u8);
        Ok(())
    }
    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        if offset.as_u64() != 0 || output.len() != 1 {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: output.len() as u64,
            });
        }
        Err(DeviceError::Unpeekable)
    }
    fn tick(&mut self, elapsed: CycleCount) {
        self.elapsed = elapsed;
    }
}
