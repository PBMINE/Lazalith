use crate::{ControlStateError, DataAccess, DataAccessError, OutcomeError, OutcomeErrorKind};
use core::{error::Error, fmt};
use lazalith_isa::{DecodeError, InstructionError, Opcode};
use lazalith_types::{InstructionAddress, WidthError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuFault<E> {
    pub pc: InstructionAddress,
    pub opcode: Option<u8>,
    pub cause: CpuFaultCause<E>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpuFaultCause<E> {
    Halted,
    PrivilegeViolation,
    UnsupportedUntilTrapController(Opcode),
    Decode(DecodeError),
    Instruction(InstructionError),
    OperandLayout,
    Control(ControlStateError),
    Width(WidthError),
    NextPc(OutcomeErrorKind),
    Outcome(OutcomeError),
    DataAccess(DataAccessError),
    Fetch(E),
    Memory { access: DataAccess, source: E },
}

impl<E: Error + 'static> fmt::Display for CpuFault<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CPU fault at PC {:#x}, opcode {:?}: {}",
            self.pc.as_u64(),
            self.opcode,
            self.cause
        )
    }
}

impl<E: Error + 'static> fmt::Display for CpuFaultCause<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Halted => f.write_str("execution is halted"),
            Self::PrivilegeViolation => f.write_str("instruction requires Supervisor privilege"),
            Self::UnsupportedUntilTrapController(opcode) => {
                write!(f, "{opcode:?} requires the Step 31 trap controller")
            }
            Self::OperandLayout => {
                f.write_str("instruction operand layout has no execution implementation")
            }
            Self::Decode(source) => source.fmt(f),
            Self::Instruction(source) => source.fmt(f),
            Self::Control(source) => source.fmt(f),
            Self::Width(source) => source.fmt(f),
            Self::NextPc(source) => source.fmt(f),
            Self::Outcome(source) => source.fmt(f),
            Self::DataAccess(source) => source.fmt(f),
            Self::Fetch(source) => write!(f, "instruction fetch: {source}"),
            Self::Memory { access, source } => write!(f, "{access:?}: {source}"),
        }
    }
}

impl<E: Error + 'static> Error for CpuFault<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

impl<E: Error + 'static> Error for CpuFaultCause<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode(source) => Some(source),
            Self::Instruction(source) => Some(source),
            Self::Control(source) => Some(source),
            Self::Width(source) => Some(source),
            Self::NextPc(source) => Some(source),
            Self::Outcome(source) => Some(source),
            Self::DataAccess(source) => Some(source),
            Self::Fetch(source) | Self::Memory { source, .. } => Some(source),
            _ => None,
        }
    }
}
