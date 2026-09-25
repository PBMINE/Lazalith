use crate::SyscallError;
use core::{error::Error, fmt};
use lazalith_types::{WidthError, WordWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WordValueOutOfRange {
    value: u64,
    width: WordWidth,
}

impl WordValueOutOfRange {
    pub const fn new(value: u64, width: WordWidth) -> Self {
        Self { value, width }
    }

    pub const fn value(self) -> u64 {
        self.value
    }

    pub const fn width(self) -> WordWidth {
        self.width
    }
}

impl fmt::Display for WordValueOutOfRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "value {:#x} does not fit {} bits",
            self.value,
            self.width.bits()
        )
    }
}

impl Error for WordValueOutOfRange {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    UnknownSyscall(u64),
    InvalidStatus(u64),
    InvalidHandle(u32),
    ReservedNonzero {
        field: &'static str,
        value: u32,
    },
    InvalidNameLength {
        length: usize,
        maximum: usize,
    },
    InvalidExitReason(u32),
    InvalidPointerWidth {
        index: u8,
        source: WordValueOutOfRange,
    },
    InvalidArgumentWidth {
        index: u8,
        source: WordValueOutOfRange,
    },
    WordValueOutOfRange(WordValueOutOfRange),
    InvalidPointer {
        index: u8,
    },
    InvalidArgument {
        index: u8,
    },
    ResourceExhausted {
        index: u8,
    },
    Misaligned {
        address: u64,
        alignment: u64,
    },
    RangeOverflow {
        address: u64,
        length: u64,
    },
    Width(WidthError),
}

impl fmt::Display for AbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownSyscall(input) => write!(f, "unknown syscall {input:#06x}"),
            Self::InvalidStatus(input) => write!(f, "invalid syscall status {input:#010x}"),
            Self::InvalidHandle(input) => write!(f, "invalid handle {input}"),
            Self::ReservedNonzero { field, value } => {
                write!(f, "reserved ABI field {field} is {value:#010x}")
            }
            Self::InvalidNameLength { length, maximum } => {
                write!(f, "name length {length} exceeds maximum {maximum}")
            }
            Self::InvalidExitReason(input) => write!(f, "invalid process exit reason {input}"),
            Self::InvalidPointerWidth { index, source } => {
                write!(f, "pointer argument {index} is out of range: {source}")
            }
            Self::InvalidArgumentWidth { index, source } => {
                write!(f, "word argument {index} is out of range: {source}")
            }
            Self::WordValueOutOfRange(source) => write!(f, "{source}"),
            Self::InvalidPointer { index } => write!(f, "argument {index} is not a valid pointer"),
            Self::InvalidArgument { index } => write!(f, "argument {index} is invalid"),
            Self::ResourceExhausted { index } => {
                write!(f, "argument {index} exceeds the ABI resource limit")
            }
            Self::Misaligned { address, alignment } => {
                write!(f, "address {address:#x} is not aligned to {alignment}")
            }
            Self::RangeOverflow { address, length } => {
                write!(f, "ABI range {address:#x}+{length} overflows")
            }
            Self::Width(error) => write!(f, "{error}"),
        }
    }
}

impl Error for AbiError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPointerWidth { source, .. }
            | Self::InvalidArgumentWidth { source, .. }
            | Self::WordValueOutOfRange(source) => Some(source),
            Self::Width(source) => Some(source),
            _ => None,
        }
    }
}

impl From<AbiError> for SyscallError {
    fn from(error: AbiError) -> Self {
        match error {
            AbiError::UnknownSyscall(_) => Self::UnknownSyscall,
            AbiError::InvalidStatus(_) | AbiError::ReservedNonzero { .. } => Self::Internal,
            AbiError::InvalidHandle(_) => Self::InvalidHandle,
            AbiError::InvalidNameLength { .. }
            | AbiError::InvalidExitReason(_)
            | AbiError::InvalidArgument { .. }
            | AbiError::InvalidArgumentWidth { .. } => Self::InvalidArgument,
            AbiError::ResourceExhausted { .. } => Self::ResourceExhausted,
            AbiError::InvalidPointer { .. } | AbiError::InvalidPointerWidth { .. } => {
                Self::InvalidPointer
            }
            AbiError::WordValueOutOfRange(_) => Self::RangeOverflow,
            AbiError::Misaligned { .. } => Self::Misaligned,
            AbiError::RangeOverflow { .. } => Self::RangeOverflow,
            AbiError::Width(
                WidthError::InvalidSourceWidth { .. }
                | WidthError::AddressOutOfRange { .. }
                | WidthError::InvalidAccessBase { .. }
                | WidthError::AccessEndOutOfRange { .. },
            ) => Self::RangeOverflow,
            AbiError::Width(_) => Self::Internal,
        }
    }
}
