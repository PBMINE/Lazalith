use crate::outcome::StepResult;
use crate::{
    ControlStateError, ControlTarget, CpuFault, CpuFaultCause as Cause, CpuMemory, DataAccess,
    DataAccessKind, EngineKind, ExecutionEngine, ExecutionOutcome as Outcome, ExecutionState,
    FetchedInstruction, OutcomeApplication, Privilege, Processor, StackEffect, TrapRequest,
    checked_next_pc, checked_return_sp, prepare_outcome, validate_pc,
};
use lazalith_isa::{ControlRegister, DataSize, Instruction, Opcode, Operand, decode};
use lazalith_types::{InstructionAddress, VirtualAddress, WordWidth};

/// The reference interpreter: the executable statement of what the ISA means.
///
/// It is not an optimisation and not a second implementation written for speed. It
/// is the *definition*, and every other engine is checked against it. Nothing here
/// may be changed to make another engine's output match; a disagreement is a
/// defect in the other engine.
///
/// # It holds no architectural state
///
/// A [`Processor`] is passed in and given back. That is not a convenience: it is
/// what makes an engine switchable, because a machine that owns the state cannot
/// have two copies of it. See [`crate::Processor`] and [`crate::ExecutionEngine`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReferenceInterpreter;

impl ReferenceInterpreter {
    /// The reference engine.
    pub const fn new() -> Self {
        Self
    }

    fn validate_fetch<E>(processor: &Processor) -> Result<(), CpuFault<E>> {
        let fault = |cause| CpuFault::at(processor.architectural().pc(), None, cause);
        if processor.traps().is_terminal() {
            return Err(fault(Cause::TerminalTrap));
        }
        if processor.execution() == ExecutionState::Halted {
            return Err(fault(Cause::Halted));
        }
        let config = processor.config();
        validate_pc(config, processor.architectural().pc())
            .map_err(|e| fault(Cause::Control(e)))?;
        config
            .word_width()
            .checked_access_end(processor.architectural().pc().as_u64(), 8)
            .map_err(|e| fault(Cause::Width(e)))?;
        Ok(())
    }

    fn execute_checked<M: CpuMemory>(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        let application = self.execute_application_body(processor, instruction, memory)?;
        Ok(StepResult::new(application, instruction.opcode().cycles()))
    }
}

impl<M: CpuMemory> ExecutionEngine<M> for ReferenceInterpreter {
    fn kind(&self) -> EngineKind {
        EngineKind::Reference
    }

    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        Self::validate_fetch(processor)?;
        let config = processor.config();
        let pc = processor.architectural().pc();
        // A memory with a decode cache hands back an instruction it decoded
        // earlier, and the only work left is executing it. A memory without one
        // hands back the bytes, and the cost is exactly what it always was —
        // which is what makes the cache an optimisation of this function and not
        // a different function.
        let instruction = match memory
            .fetch_instruction_cached(config, pc, processor.privilege())
            .map_err(|source| CpuFault::at(pc, None, Cause::Fetch(source)))?
        {
            FetchedInstruction::Decoded(instruction) => instruction,
            FetchedInstruction::Bytes(bytes) => {
                let instruction = decode(config, &bytes).map_err(|source| {
                    CpuFault::at(pc, bytes.first().copied(), Cause::Decode(source))
                })?;
                memory.cache_instruction(config, pc, instruction);
                instruction
            }
        };
        self.execute_checked(processor, &instruction, memory)
    }

    fn step_bytes(
        &mut self,
        processor: &mut Processor,
        bytes: &[u8],
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        Self::validate_fetch(processor)?;
        let instruction = decode(processor.config(), bytes).map_err(|source| {
            CpuFault::at(
                processor.architectural().pc(),
                bytes.first().copied(),
                Cause::Decode(source),
            )
        })?;
        self.execute_checked(processor, &instruction, memory)
    }

    fn execute(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        self.execute_checked(processor, instruction, memory)
    }
}

impl ReferenceInterpreter {
    /// The application of one instruction, with no timing.
    ///
    /// **Kept separate from the trait.s `execute` so the fifty-odd `return
    /// Ok(OutcomeApplication::...)` sites in the body below do not each have to know
    /// what the instruction cost.** Every one of them returns the outcome; the cost is
    /// attached once, by `execute_checked`, from the instruction that produced them.
    fn execute_application_body<M: CpuMemory>(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>> {
        Self::validate_fetch(processor).map_err(|mut error| {
            error.opcode = Some(instruction.opcode().as_u8());
            error
        })?;
        let pc = processor.architectural().pc();
        let opcode = instruction.opcode();
        let fault = |cause| CpuFault::at(pc, Some(opcode.as_u8()), cause);
        let config = processor.config();
        let width = config.word_width();
        instruction
            .validate(config)
            .map_err(|e| fault(Cause::Instruction(e)))?;
        if instruction.definition().supervisor_only
            && processor.privilege() != Privilege::Supervisor
        {
            return Err(fault(Cause::PrivilegeViolation));
        }
        let next_pc = checked_next_pc(config, pc).map_err(|e| fault(Cause::NextPc(e)))?;
        let read = |index| processor.architectural().registers().read(index);
        match (opcode, instruction.operands()) {
            (Opcode::Rfe, []) => {
                let (resume_pc, resume_sp, resume_status) = processor
                    .traps_mut()
                    .return_control()
                    .map_err(|source| fault(Cause::Control(source)))?;
                if !processor.traps_mut().consume_syscall_return_authorization() {
                    return Err(fault(Cause::Control(
                        ControlStateError::InvalidControlState {
                            operation: "return",
                            selector: ControlRegister::Epc.as_u8(),
                        },
                    )));
                }
                let mut candidate = processor.architectural().clone();
                candidate
                    .restore_control(resume_pc, resume_sp, resume_status)
                    .map_err(|source| fault(Cause::Control(source)))?;
                processor.commit(candidate);
                processor.traps_mut().commit_return();
                return Ok(OutcomeApplication::Continue);
            }
            (Opcode::Csrr, [Operand::Register(d), Operand::Control(control)]) => {
                let value = processor
                    .traps()
                    .read_control(*control)
                    .map_err(|source| fault(Cause::Control(source)))?;
                let mut candidate = processor.architectural().clone();
                candidate.write_register(*d, value);
                candidate
                    .set_pc(next_pc)
                    .map_err(|source| fault(Cause::Control(source)))?;
                processor.commit(candidate);
                return Ok(OutcomeApplication::Continue);
            }
            (Opcode::Csrw, [Operand::Control(control), Operand::Register(source)]) => {
                let value = read(*source);
                processor
                    .traps_mut()
                    .write_control(*control, value)
                    .map_err(|error| fault(Cause::Control(error)))?;
                processor
                    .architectural_mut()
                    .set_pc(next_pc)
                    .map_err(|source| fault(Cause::Control(source)))?;
                return Ok(OutcomeApplication::Continue);
            }
            _ => {}
        }
        let mut candidate = processor.architectural().clone();
        let mut execution = processor.execution();
        let mut outcome = Outcome::Continue;
        let mut data = None;
        let mut arithmetic = None;
        use Operand::{Condition, DataSize as Size, Immediate, Memory, Register};
        match (opcode, instruction.operands()) {
            (Opcode::Nop, []) => {}
            (Opcode::Mov, [Register(d), Register(a)]) => candidate.write_register(*d, read(*a)),
            (Opcode::Li, [Register(d), Immediate(i)]) => candidate.write_register(
                *d,
                width
                    .sign_extend(*i as u64, 32)
                    .map_err(|e| fault(Cause::Width(e)))?,
            ),
            (Opcode::Getpc, [Register(d)]) => candidate.write_register(*d, pc.as_u64()),
            (Opcode::Getsp, [Register(d)]) => {
                candidate.write_register(*d, processor.architectural().sp().as_u64())
            }
            (Opcode::Getstatus, [Register(d)]) => {
                candidate.write_register(*d, processor.architectural().status().bits())
            }
            (Opcode::Setsp, [Register(a)]) => candidate
                .set_sp(VirtualAddress::new(read(*a)))
                .map_err(|e| fault(Cause::Control(e)))?,
            (Opcode::Addi | Opcode::Subi, [Register(d), Register(a), Immediate(i)]) => {
                let right = width
                    .sign_extend(*i as u64, 32)
                    .map_err(|e| fault(Cause::Width(e)))?;
                arithmetic = Some((
                    Some(*d),
                    if opcode == Opcode::Addi {
                        width.add(read(*a), right)
                    } else {
                        width.sub(read(*a), right)
                    },
                ));
            }
            (Opcode::Cmp, [Register(a), Register(b)]) => {
                arithmetic = Some((None, width.sub(read(*a), read(*b))))
            }
            (Opcode::Not, [Register(d), Register(a)]) => {
                arithmetic = Some((Some(*d), width.not(read(*a))))
            }
            (
                Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Divu
                | Opcode::Divs
                | Opcode::Remu
                | Opcode::Rems
                | Opcode::And
                | Opcode::Or
                | Opcode::Xor
                | Opcode::Shl
                | Opcode::Shr
                | Opcode::Sar,
                [Register(d), Register(a), Register(b)],
            ) => {
                let (a, b) = (read(*a), read(*b));
                let result = match opcode {
                    Opcode::Add => width.add(a, b),
                    Opcode::Sub => width.sub(a, b),
                    Opcode::Mul => width.mul(a, b),
                    Opcode::Divu => width
                        .div_unsigned(a, b)
                        .map_err(|e| fault(Cause::Width(e)))?,
                    Opcode::Divs => width.div_signed(a, b).map_err(|e| fault(Cause::Width(e)))?,
                    Opcode::Remu => width
                        .rem_unsigned(a, b)
                        .map_err(|e| fault(Cause::Width(e)))?,
                    Opcode::Rems => width.rem_signed(a, b).map_err(|e| fault(Cause::Width(e)))?,
                    Opcode::And => width.bitand(a, b),
                    Opcode::Or => width.bitor(a, b),
                    Opcode::Xor => width.bitxor(a, b),
                    Opcode::Shl => width.shl(a, b),
                    Opcode::Shr => width.shr(a, b),
                    Opcode::Sar => width.sar(a, b),
                    _ => return Err(fault(Cause::OperandLayout)),
                };
                arithmetic = Some((Some(*d), result));
            }
            (
                Opcode::Ldz | Opcode::Lds | Opcode::St,
                [Register(d), Memory { base, displacement }, Size(size)],
            ) => {
                let access = DataAccess::new(
                    config,
                    VirtualAddress::new(read(*base)),
                    *displacement,
                    *size,
                    if opcode == Opcode::St {
                        DataAccessKind::Write
                    } else {
                        DataAccessKind::Read
                    },
                    processor.privilege(),
                )
                .map_err(|e| fault(Cause::DataAccess(e)))?;
                data = Some((*d, access));
            }
            (Opcode::Br, [Condition(condition), Immediate(i)]) => {
                if processor.architectural().status().matches(*condition) {
                    outcome = Outcome::Jump(ControlTarget::Relative(*i));
                }
            }
            (Opcode::Jmp | Opcode::Callr, [Register(a)]) => {
                let target = ControlTarget::Absolute(InstructionAddress::new(read(*a)));
                outcome = if opcode == Opcode::Jmp {
                    Outcome::Jump(target)
                } else {
                    Outcome::Call(target)
                };
            }
            (Opcode::Call, [Immediate(i)]) => outcome = Outcome::Call(ControlTarget::Relative(*i)),
            (Opcode::Ret, []) => {
                checked_return_sp(config, processor.architectural().sp())
                    .map_err(|e| fault(Cause::NextPc(e)))?;
                let size = match width {
                    WordWidth::W32 => DataSize::Word,
                    WordWidth::W64 => DataSize::Double,
                };
                let access = DataAccess::new(
                    config,
                    processor.architectural().sp(),
                    0,
                    size,
                    DataAccessKind::StackRead,
                    processor.privilege(),
                )
                .map_err(|e| fault(Cause::DataAccess(e)))?;
                let target = memory
                    .peek_stack(access)
                    .map_err(|source| fault(Cause::Memory { access, source }))?;
                outcome = Outcome::Return(InstructionAddress::new(target));
            }
            (Opcode::Syscall, []) => outcome = Outcome::Trap(TrapRequest::Syscall),
            (Opcode::Trap, [Immediate(i)]) => outcome = Outcome::Trap(TrapRequest::Software(*i)),
            (Opcode::Halt, []) => outcome = Outcome::Halt,
            (Opcode::Ei | Opcode::Di, []) => candidate.set_interrupts_enabled(opcode == Opcode::Ei),
            _ => return Err(fault(Cause::OperandLayout)),
        }
        if let Some((destination, result)) = arithmetic {
            if let Some(d) = destination {
                candidate.write_register(d, result.value);
            }
            candidate.update_arithmetic(result);
        }
        let privilege = candidate.privilege();
        let prepared = prepare_outcome(&mut candidate, &mut execution, outcome)
            .map_err(|e| fault(Cause::Outcome(e)))?;
        let mut loaded = None;
        if let Some((d, access)) = data {
            if opcode == Opcode::St {
                memory
                    .write_data(access, read(d))
                    .map_err(|source| fault(Cause::Memory { access, source }))?;
            } else {
                let value = memory
                    .read_data(access)
                    .map_err(|source| fault(Cause::Memory { access, source }))?;
                let bits = access.size().bytes() * 8;
                let shift = u64::from(width.bits() - bits);
                let value = if opcode == Opcode::Lds {
                    width.sar(width.shl(value, shift).value, shift).value
                } else {
                    value & width.shr(width.mask(), shift).value
                };
                loaded = Some((d, value));
            }
        }
        let application = prepared.commit(|stack| {
            if let StackEffect::Push {
                address,
                return_pc,
                size,
            } = stack
            {
                let access = DataAccess::new(
                    config,
                    address,
                    0,
                    size,
                    DataAccessKind::StackWrite,
                    privilege,
                )
                .map_err(|e| fault(Cause::DataAccess(e)))?;
                memory
                    .write_data(access, return_pc.as_u64())
                    .map_err(|source| fault(Cause::Memory { access, source }))?;
            }
            Ok(())
        })?;
        if let Some((d, value)) = loaded {
            candidate.write_register(d, value);
        }
        processor.commit(candidate);
        processor.set_execution(execution);
        Ok(application)
    }
}
