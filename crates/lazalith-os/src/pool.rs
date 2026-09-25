use crate::MemoryError;
use lazalith_types::{ArchitectureConfig, PhysicalAddress};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryBlock {
    address: PhysicalAddress,
    length: u64,
}

impl MemoryBlock {
    pub const fn address(&self) -> PhysicalAddress {
        self.address
    }

    pub const fn length(&self) -> u64 {
        self.length
    }

    pub const fn end_exclusive(&self) -> u64 {
        self.address.as_u64() + self.length
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BumpPool {
    config: ArchitectureConfig,
    start: u64,
    limit: u64,
    cursor: u64,
    default_alignment: u64,
}

impl BumpPool {
    pub fn new(
        config: ArchitectureConfig,
        start: PhysicalAddress,
        length: u64,
        default_alignment: u64,
    ) -> Result<Self, MemoryError> {
        validate_alignment(default_alignment)?;
        if length == 0 {
            return Err(MemoryError::InvalidLayout);
        }
        config
            .word_width()
            .checked_access_end(start.as_u64(), length)
            .map_err(|_| MemoryError::InvalidLayout)?;
        let limit = start
            .as_u64()
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        Ok(Self {
            config,
            start: start.as_u64(),
            limit,
            cursor: start.as_u64(),
            default_alignment,
        })
    }

    pub fn allocate(&mut self, length: u64, alignment: u64) -> Result<MemoryBlock, MemoryError> {
        if length == 0 {
            return Err(MemoryError::ZeroSize);
        }
        validate_alignment(alignment)?;
        let aligned = self
            .cursor
            .checked_add(alignment - 1)
            .ok_or(MemoryError::AddressOverflow)?
            & !(alignment - 1);
        let address = if aligned < self.start {
            self.start
                .checked_add(alignment - 1)
                .ok_or(MemoryError::AddressOverflow)?
                & !(alignment - 1)
        } else {
            aligned
        };
        let end = address
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > self.limit {
            return Err(MemoryError::Exhausted {
                requested: length,
                remaining: self.limit.saturating_sub(address),
            });
        }
        self.config
            .word_width()
            .checked_access_end(address, length)
            .map_err(|_| MemoryError::AddressOverflow)?;
        self.cursor = end;
        Ok(MemoryBlock {
            address: PhysicalAddress::new(address),
            length,
        })
    }

    pub fn allocate_default(&mut self, length: u64) -> Result<MemoryBlock, MemoryError> {
        self.allocate(length, self.default_alignment)
    }

    pub(crate) fn advance_to(&mut self, address: u64) -> Result<(), MemoryError> {
        if address < self.cursor || address > self.limit {
            return Err(MemoryError::InvalidLayout);
        }
        self.cursor = address;
        Ok(())
    }

    pub const fn start(&self) -> PhysicalAddress {
        PhysicalAddress::new(self.start)
    }

    pub const fn limit(&self) -> u64 {
        self.limit
    }

    pub fn cursor(&self) -> Option<PhysicalAddress> {
        (self.cursor < self.limit).then(|| PhysicalAddress::new(self.cursor))
    }

    pub const fn remaining(&self) -> u64 {
        self.limit - self.cursor
    }

    pub const fn default_alignment(&self) -> u64 {
        self.default_alignment
    }
}

fn validate_alignment(alignment: u64) -> Result<(), MemoryError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(MemoryError::InvalidAlignment { alignment });
    }
    Ok(())
}
