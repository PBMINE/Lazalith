use crate::Privilege;
use core::{error::Error, fmt};
use lazalith_isa::DataSize;
use lazalith_types::{ArchitectureConfig, InstructionAddress, VirtualAddress, WidthError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataAccessKind {
    Read,
    Write,
    StackRead,
    StackWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DataAccess {
    config: ArchitectureConfig,
    address: VirtualAddress,
    size: DataSize,
    kind: DataAccessKind,
    privilege: Privilege,
}

impl DataAccess {
    pub fn new(
        config: ArchitectureConfig,
        base: VirtualAddress,
        displacement: i32,
        size: DataSize,
        kind: DataAccessKind,
        privilege: Privilege,
    ) -> Result<Self, DataAccessError> {
        if !config.supports_data_size(size.bytes()) {
            return Err(DataAccessError::InvalidWidth { config, size });
        }
        let width = config.word_width();
        let address = width
            .checked_address_offset(base.as_u64(), i64::from(displacement))
            .map_err(DataAccessError::Width)?;
        width
            .checked_access_end(address, u64::from(size.bytes()))
            .map_err(DataAccessError::Width)?;
        if !address.is_multiple_of(u64::from(size.bytes())) {
            return Err(DataAccessError::Alignment {
                address: VirtualAddress::new(address),
                size,
            });
        }
        Ok(Self {
            config,
            address: VirtualAddress::new(address),
            size,
            kind,
            privilege,
        })
    }

    pub const fn config(self) -> ArchitectureConfig {
        self.config
    }
    pub const fn address(self) -> VirtualAddress {
        self.address
    }
    pub const fn size(self) -> DataSize {
        self.size
    }
    pub const fn kind(self) -> DataAccessKind {
        self.kind
    }
    pub const fn privilege(self) -> Privilege {
        self.privilege
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataAccessError {
    InvalidWidth {
        config: ArchitectureConfig,
        size: DataSize,
    },
    Width(WidthError),
    Alignment {
        address: VirtualAddress,
        size: DataSize,
    },
}

impl fmt::Display for DataAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidWidth { config, size } => {
                write!(f, "unsupported {size:?} access in {config:?}")
            }
            Self::Width(source) => source.fmt(f),
            Self::Alignment { address, size } => {
                write!(f, "unaligned {size:?} access at {:#x}", address.as_u64())
            }
        }
    }
}

impl Error for DataAccessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Width(source) => Some(source),
            _ => None,
        }
    }
}

pub trait CpuMemory {
    type Error: Error + 'static;

    fn fetch_instruction(
        &self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: Privilege,
    ) -> Result<[u8; 8], Self::Error>;

    fn read_data(&mut self, access: DataAccess) -> Result<u64, Self::Error>;
    fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), Self::Error>;
    fn peek_stack(&self, access: DataAccess) -> Result<u64, Self::Error>;

    /// Fetches the instruction at `pc`, from a cache when the memory has one.
    ///
    /// The default implementation *is* the reference behaviour: it fetches the
    /// bytes and hands them back undecoded, so a memory with no cache behaves
    /// exactly as it did before this method existed. An implementation that adds
    /// a decode cache overrides this, and the only thing it may skip is the work
    /// of turning bytes into an instruction — never a check.
    ///
    /// That is why the return type carries the bytes on a miss rather than
    /// `Option`: on a miss the caller has the bytes already, so a caching memory
    /// does the fetch checks once and the caller does not do them again. A cache
    /// that skipped the fetch checks would be faster and wrong in a way that only
    /// shows up after a program does something the fast path did not expect.
    fn fetch_instruction_cached(
        &mut self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: Privilege,
    ) -> Result<FetchedInstruction, Self::Error> {
        self.fetch_instruction(config, pc, privilege)
            .map(FetchedInstruction::Bytes)
    }

    /// Remembers a decoded instruction, if this memory caches them.
    ///
    /// The default does nothing, which is correct for a memory that does not
    /// cache: the caller has decoded the instruction and is about to execute it,
    /// and there is nothing to remember it for.
    fn cache_instruction(
        &mut self,
        _config: ArchitectureConfig,
        _pc: InstructionAddress,
        _instruction: lazalith_isa::Instruction,
    ) {
    }
}

/// What a fetch produced: an instruction the memory had already decoded, or the
/// bytes the caller must decode itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FetchedInstruction {
    /// Already decoded, by a memory that caches.
    Decoded(lazalith_isa::Instruction),
    /// The eight bytes at the address, not yet decoded.
    Bytes([u8; 8]),
}
