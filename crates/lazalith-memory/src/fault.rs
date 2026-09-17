use crate::RegionPermissions;
use alloc::collections::TryReserveError;
use core::{error::Error, fmt, num::TryFromIntError};
use lazalith_cpu::{ControlStateError, DataAccessKind, Privilege};
use lazalith_isa::DataSize;
use lazalith_types::ArchitectureConfig;
use lazalith_types::{InstructionAddress, PhysicalAddress, VirtualAddress, WidthError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessType {
    Map,
    Initialize,
    Translate,
    Fetch,
    Data(DataAccessKind),
    Peek,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessSize {
    Data(DataSize),
    Instruction,
    Bytes(u64),
}

impl AccessSize {
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Data(size) => size.bytes() as u64,
            Self::Instruction => 8,
            Self::Bytes(bytes) => bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryAddress {
    Physical(PhysicalAddress),
    Virtual(VirtualAddress),
    Instruction(InstructionAddress),
}

#[derive(Debug)]
pub struct MemoryFault {
    pub address: MemoryAddress,
    pub access: AccessType,
    pub size: AccessSize,
    pub pc: Option<InstructionAddress>,
    pub privilege: Option<Privilege>,
    pub kind: MemoryFaultKind,
}

impl MemoryFault {
    pub(crate) fn new(
        address: MemoryAddress,
        access: AccessType,
        size: AccessSize,
        kind: MemoryFaultKind,
    ) -> Self {
        Self {
            address,
            access,
            size,
            pc: None,
            privilege: None,
            kind,
        }
    }

    pub fn with_pc(mut self, pc: InstructionAddress) -> Self {
        self.pc = Some(pc);
        self
    }

    pub fn with_privilege(mut self, privilege: Privilege) -> Self {
        self.privilege = Some(privilege);
        self
    }
}

#[derive(Debug)]
pub enum MemoryFaultKind {
    Device(lazalith_devices::DeviceError),
    DeviceAlreadyMapped(lazalith_devices::DeviceId),
    InvalidDataAccess {
        base: VirtualAddress,
        displacement: i32,
        source: lazalith_cpu::DataAccessError,
    },
    Width(WidthError),
    Control(ControlStateError),
    Configuration {
        expected: ArchitectureConfig,
        actual: ArchitectureConfig,
    },
    Permission {
        permissions: RegionPermissions,
    },
    AccessPolicy,
    StackSize,
    StackRegion {
        region_start: PhysicalAddress,
        region_end: PhysicalAddress,
        kind: crate::RegionKind,
    },
    ReadOnly {
        region_start: PhysicalAddress,
        region_end: PhysicalAddress,
        kind: crate::RegionKind,
    },
    WritableRom {
        permissions: RegionPermissions,
    },
    HostSize(TryFromIntError),
    Allocation(TryReserveError),
    Overlap {
        existing_start: PhysicalAddress,
        existing_end: PhysicalAddress,
    },
    Unmapped,
    CrossRegion {
        region_start: PhysicalAddress,
        region_end: PhysicalAddress,
    },
}

impl fmt::Display for MemoryFaultKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDataAccess {
                base,
                displacement,
                source,
            } => write!(
                f,
                "data access from {:#x} with displacement {displacement}: {source}",
                base.as_u64()
            ),
            Self::Device(source) => source.fmt(f),
            Self::DeviceAlreadyMapped(id) => write!(f, "device {id:?} already mapped"),
            Self::Width(source) => source.fmt(f),
            Self::Control(source) => source.fmt(f),
            Self::Configuration { expected, actual } => write!(
                f,
                "memory configuration {expected:?} differs from access {actual:?}"
            ),
            Self::Permission { permissions } => {
                write!(f, "region permissions deny access: {permissions:?}")
            }
            Self::AccessPolicy => f.write_str("operation rejects this access kind or backend"),
            Self::StackSize => f.write_str("stack access requires a complete architectural word"),
            Self::StackRegion {
                region_start,
                region_end,
                kind,
            } => write!(
                f,
                "stack access requires RAM but the {kind:?} region {:#x}..={:#x} is not",
                region_start.as_u64(),
                region_end.as_u64()
            ),
            Self::ReadOnly {
                region_start,
                region_end,
                kind,
            } => write!(
                f,
                "{kind:?} region {:#x}..={:#x} rejects mutation",
                region_start.as_u64(),
                region_end.as_u64()
            ),
            Self::WritableRom { permissions } => write!(
                f,
                "ROM constructor rejects writable permissions {permissions:?}"
            ),
            Self::HostSize(source) => write!(f, "memory size exceeds host capacity: {source}"),
            Self::Allocation(source) => write!(f, "memory allocation failed: {source}"),
            Self::Overlap {
                existing_start,
                existing_end,
            } => write!(
                f,
                "mapping overlaps {:#x}..={:#x}",
                existing_start.as_u64(),
                existing_end.as_u64()
            ),
            Self::Unmapped => f.write_str("unmapped address"),
            Self::CrossRegion {
                region_start,
                region_end,
            } => write!(
                f,
                "access exceeds single region {:#x}..={:#x}",
                region_start.as_u64(),
                region_end.as_u64()
            ),
        }
    }
}

impl Error for MemoryFaultKind {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            Self::InvalidDataAccess { source, .. } => Some(source),
            Self::Width(source) => Some(source),
            Self::Control(source) => Some(source),
            Self::HostSize(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

impl fmt::Display for MemoryFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} {:?} at {:?} (PC {:?}, privilege {:?}): {}",
            self.access, self.size, self.address, self.pc, self.privilege, self.kind
        )
    }
}

impl Error for MemoryFault {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.kind)
    }
}
