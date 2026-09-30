use crate::{
    ControlStateError, DataAccess, DataAccessError, OutcomeError, OutcomeErrorKind,
    PrepareEntryError,
};
use core::{error::Error, fmt, panic::Location};
use lazalith_isa::{DecodeError, InstructionError};
use lazalith_types::{InstructionAddress, WidthError};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuFault<E> {
    pub pc: InstructionAddress,
    pub opcode: Option<u8>,
    pub cause: CpuFaultCause<E>,
    /// Where in the emulator this fault was noticed.
    ///
    /// Captured by [`Self::at`] from `Location::caller()`, so it is the line the
    /// compiler recorded rather than one written down by hand. A hand-written file
    /// and line go stale the moment the file is edited, and a stale line number in
    /// a bug report sends someone to the right file and the wrong line.
    pub site: &'static Location<'static>,
}

impl<E> CpuFault<E> {
    /// A fault, noticed here.
    ///
    /// The `site` is this call's caller, which is the line in the emulator that
    /// decided the machine could not continue — which is what a bug report needs
    /// and is not available anywhere else once the fault has travelled.
    ///
    /// `#[track_caller]` is what makes that true: without it `Location::caller()`
    /// reports the line *inside this function*, which is the one place a bug report
    /// must never point at.
    #[must_use]
    #[track_caller]
    pub fn at(pc: InstructionAddress, opcode: Option<u8>, cause: CpuFaultCause<E>) -> Self {
        Self {
            pc,
            opcode,
            cause,
            site: Location::caller(),
        }
    }

    /// The same fault, with `cause` replaced and this fault's own site kept.
    ///
    /// Used where a fault has to be *stored* and something is left behind. A
    /// `CpuFault` cannot be `Clone` because its error type need not be, and the
    /// site is the one field that must not be rewritten when it is copied — a
    /// report pointing at wherever the copy happened to be made is a report
    /// pointing at nothing.
    #[must_use]
    pub fn with_cause(&self, cause: CpuFaultCause<E>) -> Self
    where
        CpuFaultCause<E>: Clone,
    {
        Self {
            pc: self.pc,
            opcode: self.opcode,
            cause,
            site: self.site,
        }
    }

    /// This fault, with only the fields that can be copied.
    ///
    /// For storing a fault whose cause is the emulator's, where the cause carries
    /// no borrow: the error type is behind a reference, and a stored report has to
    /// outlive it.
    #[must_use]
    pub fn shallow(&self) -> Self
    where
        CpuFaultCause<E>: Clone,
    {
        self.with_cause(self.cause.clone())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpuFaultCause<E> {
    Halted,
    PrivilegeViolation,
    DoubleTrap,
    DeferredInterrupt,
    TerminalTrap,
    TrapEntry(PrepareEntryError<E>),
    Decode(DecodeError),
    Instruction(InstructionError),
    OperandLayout,
    Control(ControlStateError),
    Width(WidthError),
    NextPc(OutcomeErrorKind),
    Outcome(OutcomeError),
    DataAccess(DataAccessError),
    Fetch(E),
    Memory {
        access: DataAccess,
        source: E,
    },
    /// This execution engine declined to run the instruction.
    ///
    /// **Not a guest error, and that is why it is a variant here rather than a
    /// `Result::Err` outside the fault type.** Every other cause is something the guest
    /// did — a bad address, a bad operand, a privilege violation. A JIT declining an
    /// instruction is something *the engine* did, and the machine's correct response is
    /// to run it through a different engine rather than to trap the guest.
    ///
    /// B22 added it when the JIT arrived, and the distinction is load-bearing: a
    /// machine that treated a decline as a guest fault would trap a program for using an
    /// instruction the JIT happens not to handle, which is the JIT's limitation and not
    /// the program's.
    JitDeclined,
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
            Self::DoubleTrap => {
                f.write_str("synchronous trap occurred while a trap frame was active")
            }
            Self::DeferredInterrupt => {
                f.write_str("external interrupt is deferred by an active frame")
            }
            Self::TerminalTrap => f.write_str("trap controller is in a terminal failure state"),
            Self::TrapEntry(source) => write!(f, "trap entry failed: {source}"),
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
            Self::JitDeclined => {
                f.write_str("this execution engine declined to run the instruction")
            }
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
            Self::TrapEntry(source) => Some(source),
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

/// Who is at fault.
///
/// This is the distinction the whole debugging story rests on, and getting it
/// wrong sends someone to look in the wrong place: a guest fault is a bug in the
/// program they are debugging, and an emulator bug is a bug in *this* crate with
/// nothing to do with their program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultOrigin {
    /// The guest program did something the machine had to refuse.
    Guest,
    /// The machine found something its own rules say is impossible.
    ///
    /// Nothing the guest did can bring the machine here, so a report of one is a
    /// report about this codebase and carries the file and line it was noticed at.
    Emulator,
}

impl<E> CpuFaultCause<E> {
    /// Whether the guest or the emulator is at fault.
    ///
    /// The split is drawn where it is defensible rather than where it is
    /// convenient:
    ///
    /// - `Decode` is the guest's, because the bytes came from its code.
    /// - `DataAccess`, `Fetch` and `Memory` are the guest's, because it named the
    ///   address.
    /// - `NextPc` and `Width` are the guest's, because an instruction that
    ///   computes a program counter outside the architecture's range is a program
    ///   whose code is wrong.
    /// - `Instruction` and `OperandLayout` are the *emulator's*: a decoded
    ///   instruction whose operand layout the ISA forbids could only have been
    ///   produced by this crate's own decoder, its own validator, or a
    ///   hand-written object file that bypassed both.
    /// - `Control`, `Outcome` and `TrapEntry` are the emulator's, because they are
    ///   this machine's own state machines refusing to move.
    /// - `Halted`, `PrivilegeViolation`, `DoubleTrap`, `DeferredInterrupt` and
    ///   `TerminalTrap` are the guest's, because each is a program doing something
    ///   the architecture says it may not — with `TerminalTrap` the exception,
    ///   since it means *this* machine failed to enter a trap and is now stuck.
    pub const fn origin(&self) -> FaultOrigin {
        match self {
            Self::Decode(_)
            | Self::DataAccess(_)
            | Self::Fetch(_)
            | Self::Memory { .. }
            | Self::NextPc(_)
            | Self::Width(_)
            | Self::Halted
            | Self::PrivilegeViolation
            | Self::DoubleTrap
            | Self::DeferredInterrupt => FaultOrigin::Guest,
            Self::Instruction(_)
            | Self::OperandLayout
            | Self::Control(_)
            | Self::Outcome(_)
            | Self::TrapEntry(_)
            | Self::TerminalTrap => FaultOrigin::Emulator,
            // A decline is the engine's limitation, not the guest's mistake and not an
            // emulator bug: nothing invariant was violated. It is `Guest` because the
            // machine's answer to it is to run the instruction elsewhere, and calling it
            // an emulator cause would make `Emulator` mean "this build is wrong" for a
            // JIT that simply does not handle an instruction yet.
            Self::JitDeclined => FaultOrigin::Guest,
        }
    }

    /// The invariant this cause means was violated, in the words of the code that
    /// holds it.
    ///
    /// Only meaningful when [`Self::origin`] is [`FaultOrigin::Emulator`]; a
    /// guest cause has no invariant behind it, because the program was allowed to
    /// do whatever it did and the machine refused it.
    pub const fn invariant(&self) -> &'static str {
        match self {
            Self::Instruction(_) => "a decoded instruction validates against the ISA",
            Self::OperandLayout => {
                "a decoded instruction has the operand layout its opcode requires"
            }
            Self::Control(_) => "the control state machine is in a state its own transitions allow",
            Self::Outcome(_) => "an instruction produced an outcome the state machine can apply",
            Self::TrapEntry(_) => "entering a trap always succeeds or records why it did not",
            Self::TerminalTrap => {
                "a trap that failed to enter leaves the machine able to enter another"
            }
            _ => "the guest is at fault, and no invariant of ours was violated",
        }
    }
}

impl<E> CpuFault<E> {
    /// Who is at fault for this fault.
    pub const fn origin(&self) -> FaultOrigin {
        self.cause.origin()
    }
}
