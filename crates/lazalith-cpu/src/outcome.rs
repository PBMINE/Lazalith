use crate::{
    ArchitecturalState, ControlStateError, ExecutionState, Privilege, validate_pc, validate_sp,
};
use core::{error::Error, fmt};
use lazalith_isa::DataSize;
use lazalith_types::{ArchitectureConfig, InstructionAddress, VirtualAddress, WidthError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlTarget {
    Absolute(InstructionAddress),
    Relative(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapRequest {
    Syscall,
    Software(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionOutcome {
    Continue,
    Jump(ControlTarget),
    Call(ControlTarget),
    Return(InstructionAddress),
    Trap(TrapRequest),
    Halt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StackEffect {
    Push {
        address: VirtualAddress,
        return_pc: InstructionAddress,
        size: DataSize,
    },
    Pop {
        address: VirtualAddress,
        return_pc: InstructionAddress,
        size: DataSize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeApplication {
    Continue,
    Trap {
        request: TrapRequest,
        resume_pc: InstructionAddress,
    },
    Halted,
}

/// What one `step` produced, and what it cost.
///
/// **The cycles travel with the outcome because the engine is the only thing that
/// knows which instruction it retired.** The machine cannot ask afterwards: the
/// processor's PC has already moved, so the instruction that just ran is no longer
/// there to be measured. Reading the cost *before* the step would mean fetching and
/// decoding the instruction twice, and — worse — it would let a cost be charged for an
/// instruction that then faulted, which is exactly the accounting a guest measuring its
/// own work would be misled by.
///
/// So the engine reports it. That is also what makes B22's JIT work without a second
/// mechanism: a JIT knows what it translated and retired, and it reports the same
/// number the interpreter would, which is what lets the two engines agree on virtual
/// time (B24 checks exactly that).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StepResult {
    /// What the instruction did.
    pub application: OutcomeApplication,
    /// What it cost, from [`lazalith_isa::Opcode::cycles`], for the instructions retired.
    pub cycles: u8,
    /// How many guest instructions retired.
    ///
    /// **One for every engine except a JIT, and this field is why.** The machine charges
    /// virtual time per retired instruction and counts instructions, and an engine that
    /// executes a *block* natively retires several in one `step`. Without this the
    /// machine would under-count instructions and under-charge time for every block the
    /// JIT ran, and the two engines would disagree about virtual time for the same
    /// program, which is the thing B24 exists to catch.
    ///
    /// A `u16` rather than a `u64` because a block is bounded by how much code the JIT
    /// will translate at once, and a `u16` makes that bound structural: a block retiring
    /// more than 65,535 instructions cannot be described, so the translation loop has to
    /// stop. A `u64` would leave the bound as a comment.
    pub instructions: u16,
}

impl StepResult {
    /// One instruction, retired at this cost. What every interpreter step is.
    pub const fn one(application: OutcomeApplication, cycles: u8) -> Self {
        Self {
            application,
            cycles,
            instructions: 1,
        }
    }

    /// An instruction that retired with this outcome at this cost.
    pub const fn new(application: OutcomeApplication, cycles: u8) -> Self {
        Self::one(application, cycles)
    }

    /// A *block*: `instructions` instructions retired, costing `cycles` in total.
    pub const fn block(application: OutcomeApplication, cycles: u8, instructions: u16) -> Self {
        Self {
            application,
            cycles,
            instructions,
        }
    }

    /// The outcome, without the cost.
    ///
    /// **A named accessor so a test that is about what an instruction did can say so
    /// in one place.** Almost every instruction test asserts on the outcome, and putting
    /// a cost in every one of those expectations would mean re-deriving them whenever
    /// the timing model changes — for no gain, because none of them was ever checking
    /// the cost. `Result::map(StepResult::outcome)` drops it, and the tests that *are*
    /// about cost use [`StepResult::cycles`] directly.
    pub const fn outcome(self) -> OutcomeApplication {
        self.application
    }
}

impl From<OutcomeApplication> for StepResult {
    /// An outcome with no cost, for an engine that did not retire a real instruction.
    ///
    /// **This exists for test engines, and it is deliberately not the default way to
    /// build a `StepResult`.** A test engine that returns `Ok(OutcomeApplication::Halted)`
    /// without saying what it cost is saying "nothing ran", and this conversion says
    /// exactly that. A real engine must name its cost with
    /// [`StepResult::new`], because a real engine's cost is what the guest's clock
    /// measures and there is no sensible default for it.
    fn from(application: OutcomeApplication) -> Self {
        Self {
            application,
            cycles: 0,
            instructions: 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutcomeError {
    pub pc: InstructionAddress,
    pub outcome: ExecutionOutcome,
    pub kind: OutcomeErrorKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutcomeErrorKind {
    Halted,
    PrivilegeViolation,
    Width(WidthError),
    Control(ControlStateError),
}

impl fmt::Display for OutcomeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "outcome {:?} at PC {:#x}: {}",
            self.outcome,
            self.pc.as_u64(),
            self.kind
        )
    }
}

impl fmt::Display for OutcomeErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Halted => f.write_str("execution is halted"),
            Self::PrivilegeViolation => f.write_str("HALT requires Supervisor privilege"),
            Self::Width(source) => source.fmt(f),
            Self::Control(source) => source.fmt(f),
        }
    }
}

impl Error for OutcomeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.kind)
    }
}

impl Error for OutcomeErrorKind {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Width(source) => Some(source),
            Self::Control(source) => Some(source),
            Self::Halted | Self::PrivilegeViolation => None,
        }
    }
}

pub fn checked_next_pc(
    config: ArchitectureConfig,
    pc: InstructionAddress,
) -> Result<InstructionAddress, OutcomeErrorKind> {
    validate_pc(config, pc).map_err(OutcomeErrorKind::Control)?;
    config
        .word_width()
        .checked_address_offset(pc.as_u64(), i64::from(config.instruction_bytes()))
        .map(InstructionAddress::new)
        .map_err(OutcomeErrorKind::Width)
}

pub fn checked_return_sp(
    config: ArchitectureConfig,
    sp: VirtualAddress,
) -> Result<VirtualAddress, OutcomeErrorKind> {
    validate_sp(config, sp).map_err(OutcomeErrorKind::Control)?;
    config
        .word_width()
        .checked_address_offset(sp.as_u64(), i64::from(config.word_bytes()))
        .map(VirtualAddress::new)
        .map_err(OutcomeErrorKind::Width)
}

#[must_use]
pub struct PreparedOutcome<'a> {
    state: &'a mut ArchitecturalState,
    execution: &'a mut ExecutionState,
    next_pc: InstructionAddress,
    destination: InstructionAddress,
    sp: VirtualAddress,
    stack: Option<StackEffect>,
    application: OutcomeApplication,
}

impl PreparedOutcome<'_> {
    pub const fn next_pc(&self) -> InstructionAddress {
        self.next_pc
    }
    pub const fn destination(&self) -> InstructionAddress {
        self.destination
    }
    pub const fn stack_effect(&self) -> Option<StackEffect> {
        self.stack
    }

    pub fn commit<E>(
        self,
        transaction: impl FnOnce(StackEffect) -> Result<(), E>,
    ) -> Result<OutcomeApplication, E> {
        if let Some(stack) = self.stack {
            transaction(stack)?;
        }
        if !matches!(self.application, OutcomeApplication::Trap { .. }) {
            self.state.commit_outcome_control(self.destination, self.sp);
        }
        if self.application == OutcomeApplication::Halted {
            *self.execution = ExecutionState::Halted;
        }
        Ok(self.application)
    }
}

pub fn prepare_outcome<'a>(
    state: &'a mut ArchitecturalState,
    execution: &'a mut ExecutionState,
    outcome: ExecutionOutcome,
) -> Result<PreparedOutcome<'a>, OutcomeError> {
    let pc = state.pc();
    let error = |kind| OutcomeError { pc, outcome, kind };
    if *execution == ExecutionState::Halted {
        return Err(error(OutcomeErrorKind::Halted));
    }
    if outcome == ExecutionOutcome::Halt && state.privilege() != Privilege::Supervisor {
        return Err(error(OutcomeErrorKind::PrivilegeViolation));
    }
    let config = state.config();
    let next_pc = checked_next_pc(config, pc).map_err(error)?;
    let mut destination = next_pc;
    let mut sp = state.sp();
    let mut stack = None;
    let mut application = OutcomeApplication::Continue;
    let size = match config.word_width() {
        lazalith_types::WordWidth::W32 => DataSize::Word,
        lazalith_types::WordWidth::W64 => DataSize::Double,
    };
    match outcome {
        ExecutionOutcome::Continue => {}
        ExecutionOutcome::Jump(target) | ExecutionOutcome::Call(target) => {
            destination = match target {
                ControlTarget::Absolute(target) => target,
                ControlTarget::Relative(displacement) => InstructionAddress::new(
                    config
                        .word_width()
                        .checked_address_offset(next_pc.as_u64(), i64::from(displacement) * 4)
                        .map_err(|source| error(OutcomeErrorKind::Width(source)))?,
                ),
            };
            validate_pc(config, destination)
                .map_err(|source| error(OutcomeErrorKind::Control(source)))?;
            if matches!(outcome, ExecutionOutcome::Call(_)) {
                sp = VirtualAddress::new(
                    config
                        .word_width()
                        .checked_address_offset(sp.as_u64(), -i64::from(config.word_bytes()))
                        .map_err(|source| error(OutcomeErrorKind::Width(source)))?,
                );
                stack = Some(StackEffect::Push {
                    address: sp,
                    return_pc: next_pc,
                    size,
                });
            }
        }
        ExecutionOutcome::Return(target) => {
            sp = checked_return_sp(config, sp).map_err(error)?;
            validate_pc(config, target)
                .map_err(|source| error(OutcomeErrorKind::Control(source)))?;
            destination = target;
            stack = Some(StackEffect::Pop {
                address: state.sp(),
                return_pc: target,
                size,
            });
        }
        ExecutionOutcome::Trap(request) => {
            destination = pc;
            application = OutcomeApplication::Trap {
                request,
                resume_pc: next_pc,
            };
        }
        ExecutionOutcome::Halt => application = OutcomeApplication::Halted,
    }
    Ok(PreparedOutcome {
        state,
        execution,
        next_pc,
        destination,
        sp,
        stack,
        application,
    })
}
