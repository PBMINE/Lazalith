#![no_std]

extern crate alloc;

mod bus;
mod cache;
mod fault;
mod region;
mod space;

pub use bus::Bus;
pub use cache::InstructionCache;
pub use fault::{AccessSize, AccessType, MemoryAddress, MemoryFault, MemoryFaultKind};
pub use lazalith_cpu::{CpuMemory, DataAccess, DataAccessKind, FetchedInstruction, Privilege};
pub use lazalith_isa::DataSize;
pub use lazalith_types::RegisterIndex;
pub use region::{MemoryRegion, RegionKind, RegionPermissions};
pub use space::{AddressSpace, AddressSpaceIdentity, AddressSpaceSwapError, UserSpace};
