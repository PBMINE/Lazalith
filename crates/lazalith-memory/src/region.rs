use crate::{AccessSize, AccessType, MemoryAddress, MemoryFault, MemoryFaultKind};
use alloc::vec::Vec;
use lazalith_types::{ArchitectureConfig, PhysicalAddress};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionPermissions {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
}

impl RegionPermissions {
    pub const fn new(read: bool, write: bool, execute: bool, user: bool) -> Self {
        Self {
            read,
            write,
            execute,
            user,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegionKind {
    Ram,
    Rom,
}

#[derive(Clone, Debug)]
pub struct MemoryRegion {
    start: PhysicalAddress,
    end: PhysicalAddress,
    permissions: RegionPermissions,
    kind: RegionKind,
    bytes: Vec<u8>,
}

impl MemoryRegion {
    pub fn ram(
        config: ArchitectureConfig,
        start: PhysicalAddress,
        length: u64,
        permissions: RegionPermissions,
    ) -> Result<Self, MemoryFault> {
        Self::allocate(config, start, length, permissions, RegionKind::Ram, None)
    }

    pub fn rom(
        config: ArchitectureConfig,
        start: PhysicalAddress,
        contents: &[u8],
        permissions: RegionPermissions,
    ) -> Result<Self, MemoryFault> {
        if permissions.write {
            return Err(MemoryFault::new(
                MemoryAddress::Physical(start),
                AccessType::Map,
                AccessSize::Bytes(contents.len() as u64),
                MemoryFaultKind::WritableRom { permissions },
            ));
        }
        Self::allocate(
            config,
            start,
            contents.len() as u64,
            permissions,
            RegionKind::Rom,
            Some(contents),
        )
    }

    fn allocate(
        config: ArchitectureConfig,
        start: PhysicalAddress,
        length: u64,
        permissions: RegionPermissions,
        kind: RegionKind,
        contents: Option<&[u8]>,
    ) -> Result<Self, MemoryFault> {
        let fault = |kind: MemoryFaultKind| {
            MemoryFault::new(
                MemoryAddress::Physical(start),
                AccessType::Map,
                AccessSize::Bytes(length),
                kind,
            )
        };
        let end = config
            .word_width()
            .checked_access_end(start.as_u64(), length)
            .map_err(|e| fault(MemoryFaultKind::Width(e)))?;
        let length = usize::try_from(length).map_err(|e| fault(MemoryFaultKind::HostSize(e)))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|e| fault(MemoryFaultKind::Allocation(e)))?;
        match contents {
            Some(data) => bytes.extend_from_slice(data),
            None => bytes.resize(length, 0),
        }
        Ok(Self {
            start,
            end: PhysicalAddress::new(end),
            permissions,
            kind,
            bytes,
        })
    }

    pub const fn start(&self) -> PhysicalAddress {
        self.start
    }
    pub const fn end(&self) -> PhysicalAddress {
        self.end
    }
    pub const fn permissions(&self) -> RegionPermissions {
        self.permissions
    }
    pub const fn kind(&self) -> RegionKind {
        self.kind
    }
    pub fn length(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub(crate) fn contains(&self, address: PhysicalAddress) -> bool {
        self.start <= address && address <= self.end
    }

    pub(crate) fn bytes(&self, offset: usize, length: usize) -> &[u8] {
        &self.bytes[offset..offset + length]
    }

    #[cfg(test)]
    pub(crate) fn bytes_for_test(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn initialize(&mut self, offset: usize, bytes: &[u8]) {
        self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    pub(crate) fn allows_initialization(&self) -> bool {
        self.kind == RegionKind::Ram
    }
}
