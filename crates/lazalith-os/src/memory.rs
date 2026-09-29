use crate::{BumpPool, MemoryBlock, MemoryError};
use alloc::vec::Vec;
use lazalith_memory::{AddressSpace, MemoryRegion, RegionPermissions};
use lazalith_types::{ArchitectureConfig, PhysicalAddress, VirtualAddress};

// The machine's physical geometry is the machine's, and a machine profile is
// where a machine is described — so these four come from there rather than being
// restated. `PHYSICAL_RAM_START` and `PHYSICAL_RAM_LENGTH` were previously
// declared here and then never used to build a region, which is a constant that
// looks like it is doing something and is not; re-exporting makes that visible
// instead of silent.
//
// The kernel and user *region* layout below is a different thing and stays here:
// it is the LazOS address-space design, not the machine's physical geometry, and
// `lazalith-machine` must not depend on the operating system.
pub const PHYSICAL_RAM_START: u64 = lazalith_machine::LZA64_LAYOUT.physical_ram_start;
pub const PHYSICAL_RAM_LENGTH: u64 = lazalith_machine::LZA64_LAYOUT.physical_ram_length;
pub const KERNEL_IMAGE_START: u64 = lazalith_machine::LZA64_LAYOUT.kernel_load_address;
pub const KERNEL_IMAGE_LENGTH: u64 = lazalith_machine::LZA64_LAYOUT.kernel_image_length;
pub const KERNEL_STACK_START: u64 = 0x0018_0000;
pub const KERNEL_STACK_LENGTH: u64 = 0x0001_0000;
pub const KERNEL_INITIAL_SP: u64 = lazalith_machine::LZA64_LAYOUT.kernel_initial_sp;
pub const KERNEL_HEAP_START: u64 = 0x0019_0000;
pub const KERNEL_HEAP_LENGTH: u64 = 0x0007_0000;
pub const USER_CODE_START: u64 = 0x0020_0000;
pub const USER_CODE_LENGTH: u64 = 0x0010_0000;
pub const USER_DATA_START: u64 = 0x0030_0000;
pub const USER_DATA_LENGTH: u64 = 0x0010_0000;
pub const USER_STACK_START: u64 = 0x0040_0000;
pub const USER_STACK_LENGTH: u64 = 0x0001_0000;
pub const USER_INITIAL_SP: u64 = 0x0040_f000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StackRegion {
    config: ArchitectureConfig,
    start: u64,
    end_exclusive: u64,
    initial_sp: u64,
}

impl StackRegion {
    pub fn new(
        config: ArchitectureConfig,
        start: u64,
        length: u64,
        initial_sp: u64,
    ) -> Result<Self, MemoryError> {
        if length == 0 {
            return Err(MemoryError::InvalidLayout);
        }
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        config
            .word_width()
            .checked_access_end(start, length)
            .map_err(|_| MemoryError::InvalidLayout)?;
        if initial_sp < start
            || initial_sp >= end
            || !initial_sp.is_multiple_of(u64::from(config.stack_alignment()))
        {
            return Err(MemoryError::InvalidLayout);
        }
        Ok(Self {
            config,
            start,
            end_exclusive: end,
            initial_sp,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn start(&self) -> PhysicalAddress {
        PhysicalAddress::new(self.start)
    }

    pub const fn end_exclusive(&self) -> u64 {
        self.end_exclusive
    }

    pub const fn length(&self) -> u64 {
        self.end_exclusive - self.start
    }

    pub const fn initial_sp(&self) -> VirtualAddress {
        VirtualAddress::new(self.initial_sp)
    }

    pub fn contains(&self, address: PhysicalAddress) -> bool {
        address.as_u64() >= self.start && address.as_u64() < self.end_exclusive
    }

    pub fn contains_virtual(&self, address: VirtualAddress) -> bool {
        address.as_u64() >= self.start && address.as_u64() < self.end_exclusive
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelMemory {
    config: ArchitectureConfig,
    heap: BumpPool,
    stack: StackRegion,
}

impl KernelMemory {
    pub fn new(config: ArchitectureConfig) -> Result<Self, MemoryError> {
        let alignment = u64::from(config.word_bytes());
        Ok(Self {
            config,
            heap: BumpPool::new(
                config,
                PhysicalAddress::new(KERNEL_HEAP_START),
                KERNEL_HEAP_LENGTH,
                alignment,
            )?,
            stack: StackRegion::new(
                config,
                KERNEL_STACK_START,
                KERNEL_STACK_LENGTH,
                KERNEL_INITIAL_SP,
            )?,
        })
    }

    pub fn regions(config: ArchitectureConfig) -> Result<Vec<MemoryRegion>, MemoryError> {
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(3)
            .map_err(MemoryError::Allocation)?;
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(KERNEL_IMAGE_START),
                KERNEL_IMAGE_LENGTH,
                RegionPermissions::new(true, true, true, false),
            )
            .map_err(MemoryError::Space)?,
        );
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(KERNEL_STACK_START),
                KERNEL_STACK_LENGTH,
                RegionPermissions::new(true, true, false, false),
            )
            .map_err(MemoryError::Space)?,
        );
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(KERNEL_HEAP_START),
                KERNEL_HEAP_LENGTH,
                RegionPermissions::new(true, true, false, false),
            )
            .map_err(MemoryError::Space)?,
        );
        Ok(regions)
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub fn allocate(&mut self, length: u64, alignment: u64) -> Result<MemoryBlock, MemoryError> {
        self.heap.allocate(length, alignment)
    }

    pub fn allocate_default(&mut self, length: u64) -> Result<MemoryBlock, MemoryError> {
        self.heap.allocate_default(length)
    }

    pub const fn heap(&self) -> &BumpPool {
        &self.heap
    }

    pub const fn stack(&self) -> StackRegion {
        self.stack
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UserAllocation {
    address: VirtualAddress,
    length: u64,
}

impl UserAllocation {
    pub const fn address(&self) -> VirtualAddress {
        self.address
    }

    pub const fn length(&self) -> u64 {
        self.length
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserMemoryLayout {
    config: ArchitectureConfig,
    heap: BumpPool,
    stack: StackRegion,
}

impl UserMemoryLayout {
    pub fn new(config: ArchitectureConfig) -> Result<Self, MemoryError> {
        Ok(Self {
            config,
            heap: BumpPool::new(
                config,
                PhysicalAddress::new(USER_DATA_START),
                USER_DATA_LENGTH,
                u64::from(config.word_bytes()),
            )?,
            stack: StackRegion::new(config, USER_STACK_START, USER_STACK_LENGTH, USER_INITIAL_SP)?,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub fn allocate(&mut self, length: u64, alignment: u64) -> Result<UserAllocation, MemoryError> {
        let block = self.heap.allocate(length, alignment)?;
        Ok(UserAllocation {
            address: VirtualAddress::new(block.address().as_u64()),
            length: block.length(),
        })
    }

    pub const fn heap(&self) -> &BumpPool {
        &self.heap
    }

    pub const fn stack(&self) -> StackRegion {
        self.stack
    }
}

#[derive(Debug, Clone)]
pub struct UserMemory {
    layout: UserMemoryLayout,
    space: AddressSpace,
    identity: lazalith_memory::AddressSpaceIdentity,
}

impl UserMemory {
    pub fn new(config: ArchitectureConfig) -> Result<Self, MemoryError> {
        let layout = UserMemoryLayout::new(config)?;
        let space = Self::build_address_space(config)?;
        let identity = space.identity().clone();
        Ok(Self {
            layout,
            space,
            identity,
        })
    }

    pub fn regions(config: ArchitectureConfig) -> Result<Vec<MemoryRegion>, MemoryError> {
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(3)
            .map_err(MemoryError::Allocation)?;
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(USER_CODE_START),
                USER_CODE_LENGTH,
                RegionPermissions::new(true, false, true, true),
            )
            .map_err(MemoryError::Space)?,
        );
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(USER_DATA_START),
                USER_DATA_LENGTH,
                RegionPermissions::new(true, true, false, true),
            )
            .map_err(MemoryError::Space)?,
        );
        regions.push(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(USER_STACK_START),
                USER_STACK_LENGTH,
                RegionPermissions::new(true, true, false, true),
            )
            .map_err(MemoryError::Space)?,
        );
        Ok(regions)
    }

    fn build_address_space(config: ArchitectureConfig) -> Result<AddressSpace, MemoryError> {
        let mut space = AddressSpace::new(config);
        for region in Self::regions(config)? {
            space.map(region).map_err(MemoryError::Space)?;
        }
        Ok(space)
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.layout.config
    }

    pub const fn layout(&self) -> &UserMemoryLayout {
        &self.layout
    }

    pub const fn address_space(&self) -> &AddressSpace {
        &self.space
    }

    /// The address space, for a caller that has to swap it with a machine.s.
    ///
    /// A snapshot is the caller: while a process is activated its regions are in the
    /// machine, so capturing a process means moving them back first, and that is this
    /// accessor. Nothing else may swap them, because a region moved behind the
    /// scheduler.s back would be a region the scheduler does not know about.
    pub fn address_space_mut(&mut self) -> &mut AddressSpace {
        &mut self.space
    }

    pub fn identity(&self) -> lazalith_memory::AddressSpaceIdentity {
        self.identity.clone()
    }

    pub fn load_code(&mut self, offset: u64, bytes: &[u8]) -> Result<(), MemoryError> {
        let start = USER_CODE_START
            .checked_add(offset)
            .ok_or(MemoryError::AddressOverflow)?;
        let length = u64::try_from(bytes.len()).map_err(|_| MemoryError::AddressOverflow)?;
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > USER_CODE_START + USER_CODE_LENGTH {
            return Err(MemoryError::OutsideCode { start, end });
        }
        self.space
            .initialize(PhysicalAddress::new(start), bytes)
            .map_err(MemoryError::Space)
    }

    pub(crate) fn load_data(&mut self, offset: u64, bytes: &[u8]) -> Result<(), MemoryError> {
        let start = USER_DATA_START
            .checked_add(offset)
            .ok_or(MemoryError::AddressOverflow)?;
        let length = u64::try_from(bytes.len()).map_err(|_| MemoryError::AddressOverflow)?;
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > USER_DATA_START + USER_DATA_LENGTH {
            return Err(MemoryError::OutsideData { start, end });
        }
        if bytes.is_empty() {
            return Ok(());
        }
        self.space
            .initialize(PhysicalAddress::new(start), bytes)
            .map_err(MemoryError::Space)
    }

    pub(crate) fn zero_data(&mut self, offset: u64, length: u64) -> Result<(), MemoryError> {
        let start = USER_DATA_START
            .checked_add(offset)
            .ok_or(MemoryError::AddressOverflow)?;
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > USER_DATA_START + USER_DATA_LENGTH {
            return Err(MemoryError::OutsideData { start, end });
        }
        let mut address = start;
        let mut remaining = length;
        let zeros = [0u8; 256];
        while remaining != 0 {
            let chunk = remaining.min(zeros.len() as u64);
            let chunk_len = usize::try_from(chunk).map_err(|_| MemoryError::AddressOverflow)?;
            self.space
                .initialize(PhysicalAddress::new(address), &zeros[..chunk_len])
                .map_err(MemoryError::Space)?;
            address = address
                .checked_add(chunk)
                .ok_or(MemoryError::AddressOverflow)?;
            remaining -= chunk;
        }
        Ok(())
    }

    pub(crate) fn reserve_data(&mut self, length: u64) -> Result<(), MemoryError> {
        let end = USER_DATA_START
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > USER_DATA_START + USER_DATA_LENGTH {
            return Err(MemoryError::OutsideData {
                start: USER_DATA_START,
                end,
            });
        }
        self.layout.heap.advance_to(end)
    }

    pub fn allocate(&mut self, length: u64, alignment: u64) -> Result<UserAllocation, MemoryError> {
        self.layout.allocate(length, alignment)
    }

    pub(crate) fn parts_mut(&mut self) -> (&mut UserMemoryLayout, &mut AddressSpace) {
        (&mut self.layout, &mut self.space)
    }

    pub fn into_parts(self) -> (UserMemoryLayout, AddressSpace) {
        (self.layout, self.space)
    }
}
