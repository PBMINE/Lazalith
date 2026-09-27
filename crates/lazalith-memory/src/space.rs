use crate::{AccessSize, AccessType, MemoryAddress, MemoryFault, MemoryFaultKind, MemoryRegion};
use alloc::{collections::TryReserveError, sync::Arc, vec::Vec};
use core::{
    error::Error,
    fmt,
    hash::{Hash, Hasher},
};
use lazalith_types::{ArchitectureConfig, PhysicalAddress, VirtualAddress};

#[derive(Clone, Debug)]
pub struct AddressSpaceIdentity {
    marker: Arc<u8>,
}

impl PartialEq for AddressSpaceIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.marker, &other.marker)
    }
}

impl Eq for AddressSpaceIdentity {}

impl Hash for AddressSpaceIdentity {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.marker).hash(state);
    }
}

#[derive(Debug)]
pub enum AddressSpaceSwapError {
    Configuration {
        expected: ArchitectureConfig,
        actual: ArchitectureConfig,
    },
    Allocation(TryReserveError),
    CapacityOverflow,
    Overlap {
        existing_start: PhysicalAddress,
        existing_end: PhysicalAddress,
        incoming_start: PhysicalAddress,
        incoming_end: PhysicalAddress,
    },
}

impl fmt::Display for AddressSpaceSwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration { expected, actual } => {
                write!(
                    f,
                    "address-space configuration {actual:?} differs from {expected:?}"
                )
            }
            Self::Allocation(source) => write!(f, "address-space swap allocation failed: {source}"),
            Self::CapacityOverflow => f.write_str("address-space swap capacity overflowed"),
            Self::Overlap {
                existing_start,
                existing_end,
                incoming_start,
                incoming_end,
            } => write!(
                f,
                "address-space swap overlaps {existing_start:?}..={existing_end:?} with {incoming_start:?}..={incoming_end:?}"
            ),
        }
    }
}

impl Error for AddressSpaceSwapError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Configuration { .. } | Self::CapacityOverflow | Self::Overlap { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AddressSpace {
    config: ArchitectureConfig,
    identity: AddressSpaceIdentity,
    regions: Vec<MemoryRegion>,
}

pub trait UserSpace {
    fn config(&self) -> ArchitectureConfig;
    fn identity(&self) -> &AddressSpaceIdentity;
    fn regions(&self) -> &[MemoryRegion];
    fn peek_user(&mut self, address: PhysicalAddress, output: &mut [u8])
    -> Result<(), MemoryFault>;
    fn initialize_user(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MemoryFault>;
}

impl UserSpace for AddressSpace {
    fn config(&self) -> ArchitectureConfig {
        self.config
    }

    fn identity(&self) -> &AddressSpaceIdentity {
        &self.identity
    }

    fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    fn peek_user(
        &mut self,
        address: PhysicalAddress,
        output: &mut [u8],
    ) -> Result<(), MemoryFault> {
        self.validate_user_range(address, output.len() as u64, false)?;
        self.peek(address, output)
    }

    fn initialize_user(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MemoryFault> {
        self.validate_user_range(address, bytes.len() as u64, true)?;
        self.initialize(address, bytes)
    }
}

impl AddressSpace {
    pub fn new(config: ArchitectureConfig) -> Self {
        Self {
            config,
            identity: AddressSpaceIdentity {
                marker: Arc::new(0),
            },
            regions: Vec::new(),
        }
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }
    pub const fn identity(&self) -> &AddressSpaceIdentity {
        &self.identity
    }
    pub fn regions(&self) -> &[MemoryRegion] {
        &self.regions
    }

    pub(crate) fn swap_user_regions(
        &mut self,
        other: &mut Self,
    ) -> Result<(), AddressSpaceSwapError> {
        if self.config != other.config {
            return Err(AddressSpaceSwapError::Configuration {
                expected: self.config,
                actual: other.config,
            });
        }
        let self_kernel_count = self
            .regions
            .iter()
            .filter(|region| !region.permissions().user)
            .count();
        let self_user_count = self.regions.len() - self_kernel_count;
        let other_kernel_count = other
            .regions
            .iter()
            .filter(|region| !region.permissions().user)
            .count();
        let other_user_count = other.regions.len() - other_kernel_count;
        for incoming in other
            .regions
            .iter()
            .filter(|region| region.permissions().user)
        {
            for existing in self
                .regions
                .iter()
                .filter(|region| !region.permissions().user)
            {
                if incoming.start() <= existing.end() && existing.start() <= incoming.end() {
                    return Err(AddressSpaceSwapError::Overlap {
                        existing_start: existing.start(),
                        existing_end: existing.end(),
                        incoming_start: incoming.start(),
                        incoming_end: incoming.end(),
                    });
                }
            }
        }
        for incoming in self
            .regions
            .iter()
            .filter(|region| region.permissions().user)
        {
            for existing in other
                .regions
                .iter()
                .filter(|region| !region.permissions().user)
            {
                if incoming.start() <= existing.end() && existing.start() <= incoming.end() {
                    return Err(AddressSpaceSwapError::Overlap {
                        existing_start: existing.start(),
                        existing_end: existing.end(),
                        incoming_start: incoming.start(),
                        incoming_end: incoming.end(),
                    });
                }
            }
        }
        let mut self_kernel = Vec::new();
        let mut self_user = Vec::new();
        let mut other_kernel = Vec::new();
        let mut other_user = Vec::new();
        let self_result_len = self_kernel_count
            .checked_add(other_user_count)
            .ok_or(AddressSpaceSwapError::CapacityOverflow)?;
        let other_result_len = other_kernel_count
            .checked_add(self_user_count)
            .ok_or(AddressSpaceSwapError::CapacityOverflow)?;
        self_kernel
            .try_reserve_exact(self_result_len)
            .map_err(AddressSpaceSwapError::Allocation)?;
        self_user
            .try_reserve_exact(self_user_count)
            .map_err(AddressSpaceSwapError::Allocation)?;
        other_kernel
            .try_reserve_exact(other_result_len)
            .map_err(AddressSpaceSwapError::Allocation)?;
        other_user
            .try_reserve_exact(other_user_count)
            .map_err(AddressSpaceSwapError::Allocation)?;
        for region in self.regions.drain(..) {
            if region.permissions().user {
                self_user.push(region);
            } else {
                self_kernel.push(region);
            }
        }
        for region in other.regions.drain(..) {
            if region.permissions().user {
                other_user.push(region);
            } else {
                other_kernel.push(region);
            }
        }
        self.regions = self_kernel;
        self.regions.extend(other_user);
        other.regions = other_kernel;
        other.regions.extend(self_user);
        core::mem::swap(&mut self.identity, &mut other.identity);
        Ok(())
    }

    fn validate_user_range(
        &self,
        address: PhysicalAddress,
        length: u64,
        write: bool,
    ) -> Result<(), MemoryFault> {
        let size = length;
        if size == 0 {
            return Ok(());
        }
        let end = self
            .config
            .word_width()
            .checked_access_end(address.as_u64(), size)
            .map_err(|error| {
                MemoryFault::new(
                    MemoryAddress::Physical(address),
                    if write {
                        AccessType::Data(crate::DataAccessKind::Write)
                    } else {
                        AccessType::Data(crate::DataAccessKind::Read)
                    },
                    AccessSize::Bytes(size),
                    MemoryFaultKind::Width(error),
                )
            })?;
        let last = PhysicalAddress::new(end);
        let region = self
            .regions
            .iter()
            .find(|region| address >= region.start() && address <= region.end())
            .ok_or_else(|| {
                MemoryFault::new(
                    MemoryAddress::Physical(address),
                    if write {
                        AccessType::Data(crate::DataAccessKind::Write)
                    } else {
                        AccessType::Data(crate::DataAccessKind::Read)
                    },
                    AccessSize::Bytes(size),
                    MemoryFaultKind::Unmapped,
                )
            })?;
        if last > region.end() {
            return Err(MemoryFault::new(
                MemoryAddress::Physical(address),
                if write {
                    AccessType::Data(crate::DataAccessKind::Write)
                } else {
                    AccessType::Data(crate::DataAccessKind::Read)
                },
                AccessSize::Bytes(size),
                MemoryFaultKind::CrossRegion {
                    region_start: region.start(),
                    region_end: region.end(),
                },
            ));
        }
        if !region.permissions().user
            || (if write {
                !region.permissions().write
            } else {
                !region.permissions().read
            })
        {
            return Err(MemoryFault::new(
                MemoryAddress::Physical(address),
                if write {
                    AccessType::Data(crate::DataAccessKind::Write)
                } else {
                    AccessType::Data(crate::DataAccessKind::Read)
                },
                AccessSize::Bytes(size),
                MemoryFaultKind::Permission {
                    permissions: region.permissions(),
                },
            ));
        }
        if write && region.kind() != crate::RegionKind::Ram {
            return Err(MemoryFault::new(
                MemoryAddress::Physical(address),
                AccessType::Data(crate::DataAccessKind::Write),
                AccessSize::Bytes(size),
                MemoryFaultKind::ReadOnly {
                    region_start: region.start(),
                    region_end: region.end(),
                    kind: region.kind(),
                },
            ));
        }
        Ok(())
    }

    pub fn translate_identity(
        &self,
        address: VirtualAddress,
    ) -> Result<PhysicalAddress, MemoryFault> {
        self.config
            .word_width()
            .validate_address(address.as_u64())
            .map_err(|e| {
                MemoryFault::new(
                    MemoryAddress::Virtual(address),
                    AccessType::Translate,
                    AccessSize::Bytes(1),
                    MemoryFaultKind::Width(e),
                )
            })?;
        Ok(PhysicalAddress::new(address.as_u64()))
    }

    pub fn map(&mut self, region: MemoryRegion) -> Result<(), MemoryFault> {
        let fault = |kind| {
            MemoryFault::new(
                MemoryAddress::Physical(region.start()),
                AccessType::Map,
                AccessSize::Bytes(region.length()),
                kind,
            )
        };
        self.config
            .word_width()
            .checked_access_end(region.start().as_u64(), region.length())
            .map_err(|e| fault(MemoryFaultKind::Width(e)))?;
        for existing in &self.regions {
            if region.start() <= existing.end() && existing.start() <= region.end() {
                return Err(fault(MemoryFaultKind::Overlap {
                    existing_start: existing.start(),
                    existing_end: existing.end(),
                }));
            }
        }
        self.regions
            .try_reserve(1)
            .map_err(|e| fault(MemoryFaultKind::Allocation(e)))?;
        self.regions.push(region);
        Ok(())
    }

    pub fn initialize(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MemoryFault> {
        let size = bytes.len() as u64;
        let fault = |kind| {
            MemoryFault::new(
                MemoryAddress::Physical(address),
                AccessType::Initialize,
                AccessSize::Bytes(size),
                kind,
            )
        };
        let (index, offset) = self.locate(address, size).map_err(fault)?;
        if !self.regions[index].allows_initialization() {
            return Err(fault(MemoryFaultKind::ReadOnly {
                region_start: self.regions[index].start(),
                region_end: self.regions[index].end(),
                kind: self.regions[index].kind(),
            }));
        }
        self.regions[index].initialize(offset, bytes);
        Ok(())
    }

    pub fn fetch_instruction(
        &self,
        config: ArchitectureConfig,
        pc: lazalith_types::InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<[u8; 8], MemoryFault> {
        let fault = |kind| {
            let mut error = MemoryFault::new(
                MemoryAddress::Instruction(pc),
                AccessType::Fetch,
                AccessSize::Instruction,
                kind,
            )
            .with_pc(pc);
            error.privilege = Some(privilege);
            error
        };
        self.check_config(config).map_err(fault)?;
        lazalith_cpu::validate_pc(config, pc).map_err(|e| fault(MemoryFaultKind::Control(e)))?;
        let physical = PhysicalAddress::new(pc.as_u64());
        let (index, offset) = self
            .locate(physical, AccessSize::Instruction.bytes())
            .map_err(fault)?;
        self.check_permission(index, AccessType::Fetch, privilege)
            .map_err(fault)?;
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.regions[index].bytes(offset, 8));
        Ok(bytes)
    }

    pub fn data_access(
        &self,
        base: VirtualAddress,
        displacement: i32,
        size: crate::DataSize,
        kind: crate::DataAccessKind,
        privilege: crate::Privilege,
    ) -> Result<crate::DataAccess, MemoryFault> {
        crate::DataAccess::new(self.config, base, displacement, size, kind, privilege).map_err(
            |source| {
                let address = match source {
                    lazalith_cpu::DataAccessError::Alignment { address, .. } => address,
                    lazalith_cpu::DataAccessError::Width(
                        lazalith_types::WidthError::AccessEndOutOfRange { base, .. },
                    ) => VirtualAddress::new(base),
                    _ => base,
                };
                let mut error = MemoryFault::new(
                    MemoryAddress::Virtual(address),
                    AccessType::Data(kind),
                    AccessSize::Data(size),
                    MemoryFaultKind::InvalidDataAccess {
                        base,
                        displacement,
                        source,
                    },
                );
                error.privilege = Some(privilege);
                error
            },
        )
    }

    pub fn read_data(&mut self, access: crate::DataAccess) -> Result<u64, MemoryFault> {
        let (index, offset) = self.validate_data(access, &[crate::DataAccessKind::Read])?;
        Ok(self.read_value(index, offset, access.size()))
    }

    pub fn write_data(&mut self, access: crate::DataAccess, value: u64) -> Result<(), MemoryFault> {
        let (index, offset) = self.validate_data(
            access,
            &[
                crate::DataAccessKind::Write,
                crate::DataAccessKind::StackWrite,
            ],
        )?;
        if !self.regions[index].allows_initialization() {
            return Err(MemoryFault::new(
                MemoryAddress::Virtual(access.address()),
                AccessType::Data(access.kind()),
                AccessSize::Data(access.size()),
                MemoryFaultKind::ReadOnly {
                    region_start: self.regions[index].start(),
                    region_end: self.regions[index].end(),
                    kind: self.regions[index].kind(),
                },
            )
            .with_privilege(access.privilege()));
        }
        self.regions[index].initialize(
            offset,
            &value.to_le_bytes()[..usize::from(access.size().bytes())],
        );
        Ok(())
    }

    pub fn peek_stack(&self, access: crate::DataAccess) -> Result<u64, MemoryFault> {
        let (index, offset) = self.validate_data(access, &[crate::DataAccessKind::StackRead])?;
        Ok(self.read_value(index, offset, access.size()))
    }

    pub fn peek(&self, address: PhysicalAddress, output: &mut [u8]) -> Result<(), MemoryFault> {
        let fault = |kind| {
            MemoryFault::new(
                MemoryAddress::Physical(address),
                AccessType::Peek,
                AccessSize::Bytes(output.len() as u64),
                kind,
            )
        };
        let (index, offset) = self.locate(address, output.len() as u64).map_err(fault)?;
        output.copy_from_slice(self.regions[index].bytes(offset, output.len()));
        Ok(())
    }

    fn read_value(&self, index: usize, offset: usize, size: crate::DataSize) -> u64 {
        let mut bytes = [0; 8];
        let length = usize::from(size.bytes());
        bytes[..length].copy_from_slice(self.regions[index].bytes(offset, length));
        u64::from_le_bytes(bytes)
    }

    fn check_config(&self, actual: ArchitectureConfig) -> Result<(), MemoryFaultKind> {
        if actual != self.config {
            return Err(MemoryFaultKind::Configuration {
                expected: self.config,
                actual,
            });
        }
        Ok(())
    }

    fn validate_data(
        &self,
        access: crate::DataAccess,
        accepted: &[crate::DataAccessKind],
    ) -> Result<(usize, usize), MemoryFault> {
        let fault = |kind| {
            let mut error = MemoryFault::new(
                MemoryAddress::Virtual(access.address()),
                AccessType::Data(access.kind()),
                AccessSize::Data(access.size()),
                kind,
            );
            error.privilege = Some(access.privilege());
            error
        };
        self.check_config(access.config()).map_err(fault)?;
        if !accepted.contains(&access.kind()) {
            return Err(fault(MemoryFaultKind::AccessPolicy));
        }
        let physical = self.translate_identity(access.address())?;
        let location = self
            .locate(physical, u64::from(access.size().bytes()))
            .map_err(fault)?;
        self.check_permission(
            location.0,
            AccessType::Data(access.kind()),
            access.privilege(),
        )
        .map_err(fault)?;
        if matches!(
            access.kind(),
            crate::DataAccessKind::StackRead | crate::DataAccessKind::StackWrite
        ) {
            if access.size().bytes() != self.config.word_bytes() {
                return Err(fault(MemoryFaultKind::StackSize));
            }
            if self.regions[location.0].kind() != crate::RegionKind::Ram {
                return Err(fault(MemoryFaultKind::StackRegion {
                    region_start: self.regions[location.0].start(),
                    region_end: self.regions[location.0].end(),
                    kind: self.regions[location.0].kind(),
                }));
            }
        }
        Ok(location)
    }

    fn check_permission(
        &self,
        index: usize,
        access: AccessType,
        privilege: crate::Privilege,
    ) -> Result<(), MemoryFaultKind> {
        let permissions = self.regions[index].permissions();
        let allowed = match access {
            AccessType::Fetch => permissions.execute,
            AccessType::Data(crate::DataAccessKind::Read | crate::DataAccessKind::StackRead) => {
                permissions.read
            }
            AccessType::Data(crate::DataAccessKind::Write | crate::DataAccessKind::StackWrite) => {
                permissions.write
            }
            _ => false,
        };
        if !allowed || (privilege == crate::Privilege::User && !permissions.user) {
            return Err(MemoryFaultKind::Permission { permissions });
        }
        Ok(())
    }

    pub(crate) fn locate(
        &self,
        address: PhysicalAddress,
        size: u64,
    ) -> Result<(usize, usize), MemoryFaultKind> {
        let end = self
            .config
            .word_width()
            .checked_access_end(address.as_u64(), size)
            .map_err(MemoryFaultKind::Width)?;
        let (index, region) = self
            .regions
            .iter()
            .enumerate()
            .find(|(_, region)| region.contains(address))
            .ok_or(MemoryFaultKind::Unmapped)?;
        if end > region.end().as_u64() {
            return Err(MemoryFaultKind::CrossRegion {
                region_start: region.start(),
                region_end: region.end(),
            });
        }
        let offset = usize::try_from(address.as_u64() - region.start().as_u64())
            .map_err(MemoryFaultKind::HostSize)?;
        Ok((index, offset))
    }
}

impl lazalith_cpu::CpuMemory for AddressSpace {
    type Error = MemoryFault;

    fn fetch_instruction(
        &self,
        config: lazalith_types::ArchitectureConfig,
        pc: lazalith_types::InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<[u8; 8], Self::Error> {
        AddressSpace::fetch_instruction(self, config, pc, privilege)
    }

    fn read_data(&mut self, access: crate::DataAccess) -> Result<u64, Self::Error> {
        AddressSpace::read_data(self, access)
    }

    fn write_data(&mut self, access: crate::DataAccess, value: u64) -> Result<(), Self::Error> {
        AddressSpace::write_data(self, access, value)
    }

    fn peek_stack(&self, access: crate::DataAccess) -> Result<u64, Self::Error> {
        AddressSpace::peek_stack(self, access)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RegionPermissions;

    #[test]
    fn loader_validation_precedes_mutation() {
        let config = ArchitectureConfig::lz64();
        let mut space = AddressSpace::new(config);
        space
            .map(
                MemoryRegion::ram(
                    config,
                    PhysicalAddress::new(8),
                    4,
                    RegionPermissions::new(false, false, false, false),
                )
                .unwrap(),
            )
            .unwrap();
        space
            .initialize(PhysicalAddress::new(8), &[1, 2, 3, 4])
            .unwrap();
        for (base, bytes) in [
            (7, &[9][..]),
            (11, &[9, 9][..]),
            (8, &[][..]),
            (u64::MAX, &[9, 9][..]),
        ] {
            assert!(space.initialize(PhysicalAddress::new(base), bytes).is_err());
        }
        assert_eq!(space.regions[0].bytes_for_test(), &[1, 2, 3, 4]);
    }
}
