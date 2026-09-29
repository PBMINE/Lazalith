use crate::{
    AccessSize, AccessType, AddressSpace, AddressSpaceSwapError, DataAccess, DataAccessKind,
    InstructionCache, MemoryAddress, MemoryFault, MemoryFaultKind, RegionPermissions,
};
use alloc::vec::Vec;
use lazalith_devices::{Device, DeviceId, DeviceManager, DeviceOffset, NoDevice};
use lazalith_types::{InstructionAddress, PhysicalAddress, VirtualAddress};

#[derive(Debug)]
struct Mapping {
    id: DeviceId,
    start: PhysicalAddress,
    end: PhysicalAddress,
    permissions: RegionPermissions,
}

#[derive(Debug)]
pub struct Bus<D: Device = NoDevice> {
    space: AddressSpace,
    devices: DeviceManager<D>,
    mappings: Vec<Mapping>,
    /// `None` on a reference bus, which decodes every instruction every time.
    cache: Option<InstructionCache>,
}

impl Bus<NoDevice> {
    pub const fn new(space: AddressSpace) -> Self {
        Self::with_devices(space, DeviceManager::new())
    }

    /// A bus that decodes every instruction every time.
    ///
    /// This is the reference path, and it exists so the promise step 93 makes —
    /// *every optimized implementation must preserve reference behavior* — can be
    /// checked rather than asserted. A test that runs one program through a
    /// caching bus and the same program through this one, and compares every
    /// register, has tested the optimization against the thing it is optimizing.
    /// A time comparison alone would not: a program that got faster and wrong
    /// passes it.
    pub const fn reference(space: AddressSpace) -> Self {
        Self {
            space,
            devices: DeviceManager::new(),
            mappings: Vec::new(),
            cache: None,
        }
    }
}

impl<D: Device> Bus<D> {
    pub const fn with_devices(space: AddressSpace, devices: DeviceManager<D>) -> Self {
        Self {
            space,
            devices,
            mappings: Vec::new(),
            cache: Some(InstructionCache::new()),
        }
    }

    /// A device bus that decodes every instruction every time.
    pub const fn reference_with_devices(space: AddressSpace, devices: DeviceManager<D>) -> Self {
        Self {
            space,
            devices,
            mappings: Vec::new(),
            cache: None,
        }
    }

    /// Whether this bus caches decoded instructions.
    pub const fn caches_instructions(&self) -> bool {
        self.cache.is_some()
    }

    /// The decode cache, for a test that wants to know whether it is being used.
    pub const fn instruction_cache(&self) -> Option<&InstructionCache> {
        self.cache.as_ref()
    }

    pub const fn address_space(&self) -> &AddressSpace {
        &self.space
    }
    pub fn swap_user_address_space(
        &mut self,
        other: &mut AddressSpace,
    ) -> Result<(), AddressSpaceSwapError> {
        self.space.swap_user_regions(other)
    }
    pub fn user_address_space_mut(&mut self) -> &mut AddressSpace {
        &mut self.space
    }
    pub fn has_device_mappings(&self) -> bool {
        !self.mappings.is_empty()
    }
    pub const fn devices(&self) -> &DeviceManager<D> {
        &self.devices
    }
    pub fn devices_mut(&mut self) -> &mut DeviceManager<D> {
        &mut self.devices
    }
    pub fn reset_devices(&mut self, elapsed: lazalith_types::CycleCount) {
        self.devices.reset_at(elapsed);
    }
    pub fn tick_devices(
        &mut self,
        delta: lazalith_types::CycleCount,
    ) -> Result<(), lazalith_devices::DeviceError> {
        self.devices.tick(delta)
    }

    /// Sets the device manager's virtual time outright.
    ///
    /// The restore path, and the reason it lives here rather than being reached
    /// through `LazalithMachine`'s device accessor: a machine restoring a snapshot has
    /// to move the processor's clock and the devices' clock together, and a bus that
    /// offered only `tick` would make that two calls a caller could get wrong.
    pub fn set_device_clock(&mut self, elapsed: lazalith_types::CycleCount) {
        self.devices.set_clock(elapsed);
    }

    /// Takes every interrupt the devices have raised.
    ///
    /// A passthrough for the same reason `set_device_clock` is: the machine has to move
    /// the devices. clock and deliver their interrupts in one place, and a bus that offered
    /// only one of the two would make that two calls a caller could get wrong.
    pub fn take_device_interrupts(&mut self) -> Vec<lazalith_types::InterruptId> {
        self.devices.take_interrupts()
    }

    fn overlap(&self, start: PhysicalAddress, end: PhysicalAddress) -> Result<(), MemoryFaultKind> {
        for mapping in &self.mappings {
            if start <= mapping.end && mapping.start <= end {
                return Err(MemoryFaultKind::Overlap {
                    existing_start: mapping.start,
                    existing_end: mapping.end,
                });
            }
        }
        for region in self.space.regions() {
            if start <= region.end() && region.start() <= end {
                return Err(MemoryFaultKind::Overlap {
                    existing_start: region.start(),
                    existing_end: region.end(),
                });
            }
        }
        Ok(())
    }

    pub fn map(&mut self, region: crate::MemoryRegion) -> Result<(), MemoryFault> {
        self.overlap(region.start(), region.end()).map_err(|kind| {
            MemoryFault::new(
                MemoryAddress::Physical(region.start()),
                AccessType::Map,
                AccessSize::Bytes(region.length()),
                kind,
            )
        })?;
        self.space.map(region)
    }

    pub fn map_device(
        &mut self,
        id: DeviceId,
        start: PhysicalAddress,
        permissions: RegionPermissions,
    ) -> Result<(), MemoryFault> {
        let fault = |bytes, kind| {
            MemoryFault::new(
                MemoryAddress::Physical(start),
                AccessType::Map,
                AccessSize::Bytes(bytes),
                kind,
            )
        };
        let length = self
            .devices
            .address_len(id)
            .map_err(|e| fault(0, MemoryFaultKind::Device(e)))?;
        let end = self
            .space
            .config()
            .word_width()
            .checked_access_end(start.as_u64(), length)
            .map_err(|e| fault(length, MemoryFaultKind::Width(e)))?;
        if permissions.execute {
            return Err(fault(length, MemoryFaultKind::AccessPolicy));
        }
        if self.mappings.iter().any(|mapping| mapping.id == id) {
            return Err(fault(length, MemoryFaultKind::DeviceAlreadyMapped(id)));
        }
        let end = PhysicalAddress::new(end);
        self.overlap(start, end)
            .map_err(|kind| fault(length, kind))?;
        self.mappings
            .try_reserve(1)
            .map_err(|e| fault(length, MemoryFaultKind::Allocation(e)))?;
        self.mappings.push(Mapping {
            id,
            start,
            end,
            permissions,
        });
        Ok(())
    }

    fn mapping(
        &self,
        address: PhysicalAddress,
        bytes: u64,
    ) -> Result<Option<&Mapping>, MemoryFaultKind> {
        let end = self
            .space
            .config()
            .word_width()
            .checked_access_end(address.as_u64(), bytes)
            .map_err(MemoryFaultKind::Width)?;
        if let Some(mapping) = self
            .mappings
            .iter()
            .find(|mapping| address >= mapping.start && address <= mapping.end)
        {
            if end > mapping.end.as_u64() {
                return Err(MemoryFaultKind::CrossRegion {
                    region_start: mapping.start,
                    region_end: mapping.end,
                });
            }
            return Ok(Some(mapping));
        }
        Ok(None)
    }

    pub fn initialize(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MemoryFault> {
        self.space.initialize(address, bytes)
    }

    pub fn peek(&self, address: PhysicalAddress, output: &mut [u8]) -> Result<(), MemoryFault> {
        let length = output.len() as u64;
        let fault = |kind| {
            MemoryFault::new(
                MemoryAddress::Physical(address),
                AccessType::Peek,
                AccessSize::Bytes(length),
                kind,
            )
        };
        if let Some(mapping) = self.mapping(address, output.len() as u64).map_err(fault)? {
            return self
                .devices
                .peek(
                    mapping.id,
                    DeviceOffset::new(address.as_u64() - mapping.start.as_u64()),
                    output,
                )
                .map_err(|e| fault(MemoryFaultKind::Device(e)));
        }
        self.space.peek(address, output)
    }

    pub fn data_access(
        &self,
        base: VirtualAddress,
        displacement: i32,
        size: crate::DataSize,
        kind: DataAccessKind,
        privilege: crate::Privilege,
    ) -> Result<DataAccess, MemoryFault> {
        self.space
            .data_access(base, displacement, size, kind, privilege)
    }

    fn data_fault(access: DataAccess, kind: MemoryFaultKind) -> MemoryFault {
        MemoryFault::new(
            MemoryAddress::Virtual(access.address()),
            AccessType::Data(access.kind()),
            AccessSize::Data(access.size()),
            kind,
        )
        .with_privilege(access.privilege())
    }

    fn route_data(
        &self,
        access: DataAccess,
        accepted: DataAccessKind,
    ) -> Result<Option<(DeviceId, DeviceOffset)>, MemoryFault> {
        let fault = |kind| Self::data_fault(access, kind);
        if access.config() != self.space.config() {
            return Err(fault(MemoryFaultKind::Configuration {
                expected: self.space.config(),
                actual: access.config(),
            }));
        }
        let mapping = self
            .mapping(
                PhysicalAddress::new(access.address().as_u64()),
                u64::from(access.size().bytes()),
            )
            .map_err(fault)?;
        let Some(mapping) = mapping else {
            return Ok(None);
        };
        if access.kind() != accepted
            || !matches!(access.kind(), DataAccessKind::Read | DataAccessKind::Write)
        {
            return Err(fault(MemoryFaultKind::AccessPolicy));
        }
        let permissions = mapping.permissions;
        let allowed = if accepted == DataAccessKind::Read {
            permissions.read
        } else {
            permissions.write
        };
        if !allowed || (access.privilege() == crate::Privilege::User && !permissions.user) {
            return Err(fault(MemoryFaultKind::Permission { permissions }));
        }
        Ok(Some((
            mapping.id,
            DeviceOffset::new(access.address().as_u64() - mapping.start.as_u64()),
        )))
    }

    pub fn read_data(&mut self, access: DataAccess) -> Result<u64, MemoryFault> {
        if let Some((id, offset)) = self.route_data(access, DataAccessKind::Read)? {
            return self
                .devices
                .read(id, offset, access.size())
                .map(|value| value & (u64::MAX >> (64 - u32::from(access.size().bytes()) * 8)))
                .map_err(|e| Self::data_fault(access, MemoryFaultKind::Device(e)));
        }
        self.space.read_data(access)
    }

    pub fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), MemoryFault> {
        if let Some((id, offset)) = self.route_data(access, DataAccessKind::Write)? {
            return self
                .devices
                .write(id, offset, access.size(), value)
                .map_err(|e| Self::data_fault(access, MemoryFaultKind::Device(e)));
        }
        // Forget any instruction the bytes being written are part of, *before*
        // they are written. Doing it first means a store that faults leaves the
        // cache merely colder rather than holding an instruction that disagrees
        // with memory: forgetting too much costs one decode, and a stale entry
        // costs correctness.
        if let Some(cache) = self.cache.as_mut() {
            cache.invalidate_range(
                PhysicalAddress::new(access.address().as_u64()),
                access.size().bytes() as u64,
            );
        }
        self.space.write_data(access, value)
    }

    pub fn peek_stack(&self, access: DataAccess) -> Result<u64, MemoryFault> {
        self.route_data(access, DataAccessKind::StackRead)?;
        self.space.peek_stack(access)
    }

    /// The bytes of the instruction at `pc`, after every fetch check.
    ///
    /// This is the reference path and it is unchanged by the cache: it reads
    /// memory and returns eight bytes. [`Bus::fetch_instruction_cached`] asks the
    /// same question and is allowed to answer it from the cache.
    pub fn fetch_instruction(
        &self,
        config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<[u8; 8], MemoryFault> {
        self.checked_fetch(config, pc, privilege)
    }

    /// The instruction at `pc`, from the cache when it is there.
    ///
    /// Every check is done first, in [`Bus::checked_fetch`], and the cache is
    /// consulted only after they have passed. That is the whole of the fast
    /// path's safety argument, and it is why the cache cannot make a program that
    /// should have faulted succeed.
    pub fn fetch_instruction_cached(
        &mut self,
        config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<lazalith_cpu::FetchedInstruction, MemoryFault> {
        let bytes = self.checked_fetch(config, pc, privilege)?;
        let physical = PhysicalAddress::new(pc.as_u64());
        if let Some(instruction) = self.cache.as_ref().and_then(|cache| cache.lookup(physical)) {
            return Ok(lazalith_cpu::FetchedInstruction::Decoded(instruction));
        }
        Ok(lazalith_cpu::FetchedInstruction::Bytes(bytes))
    }

    /// Every check an instruction fetch owes, and then the bytes.
    ///
    /// One function so that the cached and uncached paths cannot disagree about
    /// what a check is: there is one copy, and both paths call it.
    fn checked_fetch(
        &self,
        config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<[u8; 8], MemoryFault> {
        let fault = |kind| {
            MemoryFault::new(
                MemoryAddress::Instruction(pc),
                AccessType::Fetch,
                AccessSize::Instruction,
                kind,
            )
            .with_pc(pc)
            .with_privilege(privilege)
        };
        if config != self.space.config() {
            return Err(fault(MemoryFaultKind::Configuration {
                expected: self.space.config(),
                actual: config,
            }));
        }
        lazalith_cpu::validate_pc(config, pc).map_err(|e| fault(MemoryFaultKind::Control(e)))?;
        if self
            .mapping(PhysicalAddress::new(pc.as_u64()), 8)
            .map_err(fault)?
            .is_some()
        {
            return Err(fault(MemoryFaultKind::AccessPolicy));
        }
        self.space.fetch_instruction(config, pc, privilege)
    }
}

impl<D: Device> lazalith_cpu::CpuMemory for Bus<D> {
    type Error = MemoryFault;
    fn fetch_instruction(
        &self,
        config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<[u8; 8], Self::Error> {
        Bus::fetch_instruction(self, config, pc, privilege)
    }
    fn fetch_instruction_cached(
        &mut self,
        config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        privilege: crate::Privilege,
    ) -> Result<lazalith_cpu::FetchedInstruction, Self::Error> {
        Bus::fetch_instruction_cached(self, config, pc, privilege)
    }
    fn cache_instruction(
        &mut self,
        _config: lazalith_types::ArchitectureConfig,
        pc: InstructionAddress,
        instruction: lazalith_isa::Instruction,
    ) {
        if let Some(cache) = self.cache.as_mut() {
            cache.insert(PhysicalAddress::new(pc.as_u64()), instruction);
        }
    }
    fn read_data(&mut self, access: DataAccess) -> Result<u64, Self::Error> {
        Bus::read_data(self, access)
    }
    fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), Self::Error> {
        Bus::write_data(self, access, value)
    }
    fn peek_stack(&self, access: DataAccess) -> Result<u64, Self::Error> {
        Bus::peek_stack(self, access)
    }
}
