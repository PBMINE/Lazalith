#![no_std]

extern crate alloc;

mod console;
pub use console::ConsoleDevice;

use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_isa::DataSize;
use lazalith_types::{ClockOverflow, VirtualClock};
pub use lazalith_types::{CycleCount, DeviceId, DeviceOffset};

#[derive(Debug)]
pub enum DeviceError {
    UnknownDevice(DeviceId),
    DuplicateDevice(DeviceId),
    EmptyDevice,
    InvalidRange { offset: DeviceOffset, bytes: u64 },
    UnsupportedSize(DataSize),
    ReadUnsupported,
    WriteUnsupported,
    Unpeekable,
    Capacity,
    Allocation(TryReserveError),
    Clock(ClockOverflow),
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
    fn tick(&mut self, elapsed: CycleCount);
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
}
