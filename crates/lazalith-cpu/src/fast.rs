//! B21: an optimised engine, and what optimising an interpreter is actually worth.
//!
//! # What this engine does, precisely
//!
//! Two things, both of which are *removals of work that provably repeated itself*:
//!
//! 1. **One fetch validation per instruction instead of two.** The reference calls
//!    `validate_fetch` in `step` and again at the top of `execute_application`, with
//!    nothing in between that could have changed the answer — the same PC, the same
//!    trap state, the same execution state. This engine calls it once.
//!
//! 2. **No per-instruction PC revalidation within a straight-line run.** The address
//!    checks in `validate_fetch` are a function of the PC alone. After an instruction
//!    that did not transfer control, the next PC is the old one plus the instruction
//!    width, and if *that* is in range then the new one is too — a smaller number, in
//!    the same instruction-width grid, in the same region. So the check is made once
//!    when a run begins and skipped for the instructions inside it.
//!
//! The trap-state and halted checks are **not** skipped. They depend on the processor,
//! not on the address, and a guest that takes a trap changes them between one
//! instruction and the next. Skipping them would be the "optimisation" that turns a
//! green test suite into a real bug.
//!
//! # What this engine deliberately does not do
//!
//! **It does not re-implement the instruction semantics.** Every instruction is executed
//! by the reference's own `execute_application`, unchanged. An optimised engine carrying
//! its own copy of the arms would be a second implementation of the ISA, and §10 says the
//! Reference Interpreter is the semantic authority — a second implementation does not
//! become the authority by being faster, it becomes a second thing that can be wrong.
//! The optimisation here is entirely in *how the reference's work is scheduled*, which is
//! what makes it safe by construction rather than by testing.
//!
//! That is also why the win is modest, and §38's "measure rather than hide" applies: the
//! measured number is in `docs/project-state.md`. The remaining per-instruction cost is
//! the `match (opcode, operands)` over an operand slice, and nothing short of compiling
//! that away removes it — which is B22's job, not B21's.
//!
//! # What this engine is not allowed to do
//!
//! **Hold architectural state.** It is a `Copy` struct with one `bool` of private
//! scheduling state, and that `bool` is about *this engine's own work*, not the guest's.
//! A machine that switched engines would find the guest's registers, PC, status and
//! memory exactly as the previous engine left them, which is what B23 tests.

use crate::{
    CpuFault, CpuFaultCause, CpuMemory, EngineFault, EngineKind, ExecutionEngine, ExecutionState,
    FetchedInstruction, OutcomeApplication, Processor, ReferenceInterpreter, StepResult,
    validate_pc,
};
use lazalith_isa::{Instruction, Opcode, decode};

/// The instructions that move the program counter somewhere this engine cannot predict.
///
/// **A closed list, and it is the safety condition for the second optimisation.** An
/// instruction not in it is not a bug — it loses an optimisation — but one added here
/// that does *not* actually transfer control would be a bug, and
/// `the_control_transfer_list_matches_the_reference` checks the list against the
/// reference's own behaviour.
const CONTROL_TRANSFERS: [Opcode; 8] = [
    Opcode::Br,
    Opcode::Jmp,
    Opcode::Call,
    Opcode::Callr,
    Opcode::Ret,
    Opcode::Rfe,
    Opcode::Syscall,
    Opcode::Trap,
];

/// An execution engine that skips work the reference does twice.
///
/// `Copy`, because it holds no guest state — and B3's
/// `no_execution_engine_owns_the_architectural_state` test looks for an `architectural:`
/// field, which this struct does not have.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FastInterpreter {
    /// Whether the PC the previous instruction reached was already validated.
    ///
    /// **Private scheduling state, and it is about this engine's own work.** It answers
    /// "did I already check this address", not anything about the guest. A machine that
    /// dropped it mid-run would re-validate and reach the same answers.
    checked_next: bool,
}

impl FastInterpreter {
    /// The engine.
    pub const fn new() -> Self {
        Self {
            checked_next: false,
        }
    }

    /// Whether `opcode` is one whose PC this engine cannot predict.
    pub fn transfers_control(opcode: Opcode) -> bool {
        CONTROL_TRANSFERS.contains(&opcode)
    }

    /// The list, for the test that checks it against the reference.
    pub const fn control_transfers() -> [Opcode; 8] {
        CONTROL_TRANSFERS
    }
}

impl<M: CpuMemory> ExecutionEngine<M> for FastInterpreter {
    fn kind(&self) -> EngineKind {
        EngineKind::Optimized
    }

    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, EngineFault<M::Error>> {
        self.step_guest(processor, memory)
            .map_err(EngineFault::Guest)
    }

    fn execute(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<StepResult, EngineFault<M::Error>> {
        self.execute_guest(processor, instruction, memory)
            .map_err(EngineFault::Guest)
    }
}

impl FastInterpreter {
    fn step_guest<M: CpuMemory>(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        let config = processor.config();
        let pc = processor.architectural().pc();
        let fault = |cause| CpuFault::at(pc, None, cause);

        // The trap-state and halted checks, always. They depend on the processor, not on
        // the address, and a guest that has just taken a trap has changed them.
        if processor.traps().is_terminal() {
            return Err(fault(CpuFaultCause::TerminalTrap));
        }
        if processor.execution() == ExecutionState::Halted {
            return Err(fault(CpuFaultCause::Halted));
        }
        // The address checks, unless the previous instruction left the PC somewhere this
        // engine already validated.
        if !self.checked_next {
            validate_pc(config, pc).map_err(|e| fault(CpuFaultCause::Control(e)))?;
            config
                .word_width()
                .checked_access_end(pc.as_u64(), 8)
                .map_err(|e| fault(CpuFaultCause::Width(e)))?;
        }

        let instruction = match memory
            .fetch_instruction_cached(config, pc, processor.privilege())
            .map_err(|source| fault(CpuFaultCause::Fetch(source)))?
        {
            FetchedInstruction::Decoded(instruction) => instruction,
            FetchedInstruction::Bytes(bytes) => {
                let instruction = decode(config, &bytes).map_err(|source| {
                    CpuFault::at(pc, bytes.first().copied(), CpuFaultCause::Decode(source))
                })?;
                memory.cache_instruction(config, pc, instruction);
                instruction
            }
        };

        let application = ReferenceInterpreter::execute_application(
            &mut ReferenceInterpreter::new(),
            processor,
            &instruction,
            memory,
        )?;

        // Whether the *next* fetch may skip its address checks: only if this
        // instruction did not move the PC somewhere unpredictable.
        self.checked_next = !Self::transfers_control(instruction.opcode())
            && application == OutcomeApplication::Continue;
        Ok(StepResult::new(application, instruction.opcode().cycles()))
    }

    fn execute_guest<M: CpuMemory>(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        let application = ReferenceInterpreter::execute_application(
            &mut ReferenceInterpreter::new(),
            processor,
            instruction,
            memory,
        )?;
        self.checked_next = false;
        Ok(StepResult::new(application, instruction.opcode().cycles()))
    }
}
