use alloc::collections::TryReserveError;
use core::{error::Error, fmt};
use lazalith_memory::MemoryFault;

#[derive(Debug)]
pub enum MemoryError {
    ZeroSize,
    InvalidAlignment { alignment: u64 },
    InvalidLayout,
    AddressOverflow,
    Exhausted { requested: u64, remaining: u64 },
    OutsideCode { start: u64, end: u64 },
    OutsideData { start: u64, end: u64 },
    Space(MemoryFault),
    Allocation(TryReserveError),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSize => f.write_str("memory allocation size must be nonzero"),
            Self::InvalidAlignment { alignment } => {
                write!(
                    f,
                    "memory alignment {alignment} is not a nonzero power of two"
                )
            }
            Self::InvalidLayout => f.write_str("LazOS memory layout is invalid"),
            Self::AddressOverflow => f.write_str("memory range arithmetic overflowed"),
            Self::Exhausted {
                requested,
                remaining,
            } => write!(
                f,
                "memory pool cannot allocate {requested} bytes with {remaining} bytes remaining"
            ),
            Self::OutsideCode { start, end } => {
                write!(f, "code range {start:#x}..{end:#x} is outside User code")
            }
            Self::OutsideData { start, end } => {
                write!(f, "data range {start:#x}..{end:#x} is outside User data")
            }
            Self::Space(source) => source.fmt(f),
            Self::Allocation(source) => write!(f, "LazOS memory allocation failed: {source}"),
        }
    }
}

impl Error for MemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Space(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}
