use crate::{
    ArchitecturalState, ControlStateError, ControlTarget, CpuFault, CpuFaultCause as Cause,
    CpuMemory, DataAccess, DataAccessKind, ExecutionOutcome as Outcome, ExecutionState,
    OutcomeApplication, Privilege, StackEffect, TrapCause, TrapController, TrapRequest,
    checked_next_pc, checked_return_sp, prepare_outcome, trap::prepare_entry_control, validate_pc,
    validate_sp,
};
use lazalith_isa::{ControlRegister, DataSize, Instruction, Opcode, Operand, decode};
use lazalith_types::{InstructionAddress, VirtualAddress, WordWidth};

#[derive(Debug, Eq, PartialEq)]
pub struct ReferenceInterpreter {
    architectural: ArchitecturalState,
    execution: ExecutionState,
    traps: TrapController,
}

impl ReferenceInterpreter {
    pub fn new(architectural: ArchitecturalState) -> Self {
        let traps = TrapController::new(architectural.config());
        Self {
            architectural,
            execution: ExecutionState::Running,
            traps,
        }
    }

    pub const fn architectural_state(&self) -> &ArchitecturalState {
        &self.architectural
    }
    pub const fn execution_state(&self) -> ExecutionState {
        self.execution
    }
    pub const fn trap_controller(&self) -> &TrapController {
        &self.traps
    }
    pub fn trap_controller_mut(&mut self) -> &mut TrapController {
        &mut self.traps
    }
    pub fn replace_architectural_state(
        &mut self,
        state: ArchitecturalState,
    ) -> Result<(), ControlStateError> {
        if self.architectural.config() != state.config()
            || self.traps.has_active_frame()
            || self.traps.is_terminal()
        {
            return Err(ControlStateError::InvalidControlState {
                operation: "replace execution context",
                selector: 0,
            });
        }
        validate_pc(state.config(), state.pc())?;
        validate_sp(state.config(), state.sp())?;
        self.architectural = state;
        self.execution = ExecutionState::Running;
        Ok(())
    }
    pub fn restore_architectural_state(
        &mut self,
        state: ArchitecturalState,
    ) -> Result<(), ControlStateError> {
        if self.architectural.config() != state.config()
            || self.traps.has_active_frame()
            || self.traps.is_terminal()
        {
            return Err(ControlStateError::InvalidControlState {
                operation: "restore execution context",
                selector: 0,
            });
        }
        validate_pc(state.config(), state.pc())?;
        validate_sp(state.config(), state.sp())?;
        self.architectural = state;
        Ok(())
    }
    pub fn restore_architectural_state_with_execution(
        &mut self,
        state: ArchitecturalState,
        execution: ExecutionState,
    ) -> Result<(), ControlStateError> {
        self.restore_architectural_state(state)?;
        self.execution = execution;
        Ok(())
    }
    pub fn write_register(&mut self, index: lazalith_types::RegisterIndex, value: u64) {
        self.architectural.write_register(index, value);
    }
    pub fn set_trap_vector(&mut self, target: InstructionAddress) -> Result<(), ControlStateError> {
        self.write_trap_control(ControlRegister::Tvec, target.as_u64())
    }
    pub fn read_trap_control(&self, control: ControlRegister) -> Result<u64, ControlStateError> {
        self.traps.read_control(control)
    }
    pub fn write_trap_control(
        &mut self,
        control: ControlRegister,
        value: u64,
    ) -> Result<(), ControlStateError> {
        self.traps.write_control(control, value)
    }
    pub fn enter_fault<M: CpuMemory>(
        &mut self,
        memory: &mut M,
        cause: TrapCause,
        resume_pc: InstructionAddress,
    ) -> Result<(), CpuFault<M::Error>> {
        if matches!(
            cause,
            TrapCause::Syscall | TrapCause::SoftwareTrap | TrapCause::ExternalInterrupt
        ) {
            return Err(CpuFault {
                pc: self.architectural.pc(),
                opcode: None,
                cause: Cause::Control(ControlStateError::InvalidControlState {
                    operation: "invalid fault cause",
                    selector: 0,
                }),
            });
        }
        self.enter_event(memory, cause, 0, resume_pc)
    }

    pub fn enter_syscall<M: CpuMemory>(
        &mut self,
        memory: &mut M,
        resume_pc: InstructionAddress,
    ) -> Result<(), CpuFault<M::Error>> {
        self.enter_event(memory, TrapCause::Syscall, 0, resume_pc)
    }

    pub fn enter_software<M: CpuMemory>(
        &mut self,
        memory: &mut M,
        payload: i32,
        resume_pc: InstructionAddress,
    ) -> Result<(), CpuFault<M::Error>> {
        let payload = match self.architectural.config().word_width() {
            lazalith_types::WordWidth::W32 => u64::from(payload as u32),
            lazalith_types::WordWidth::W64 => payload as i64 as u64,
        };
        self.enter_event(memory, TrapCause::SoftwareTrap, payload, resume_pc)
    }

    pub fn enter_external<M: CpuMemory>(
        &mut self,
        memory: &mut M,
        id: u16,
        resume_pc: InstructionAddress,
    ) -> Result<(), CpuFault<M::Error>> {
        if self.traps.is_terminal() {
            return Err(CpuFault {
                pc: self.architectural.pc(),
                opcode: None,
                cause: Cause::TerminalTrap,
            });
        }
        if self.execution == ExecutionState::Halted {
            return Err(CpuFault {
                pc: self.architectural.pc(),
                opcode: None,
                cause: Cause::Halted,
            });
        }
        if self.traps.has_active_frame() {
            return Err(CpuFault {
                pc: self.architectural.pc(),
                opcode: None,
                cause: Cause::DeferredInterrupt,
            });
        }
        self.enter_event(
            memory,
            TrapCause::ExternalInterrupt,
            u64::from(id),
            resume_pc,
        )
    }

    fn enter_event<M: CpuMemory>(
        &mut self,
        memory: &mut M,
        cause: TrapCause,
        payload: u64,
        resume_pc: InstructionAddress,
    ) -> Result<(), CpuFault<M::Error>> {
        let fault = |cause| CpuFault {
            pc: self.architectural.pc(),
            opcode: None,
            cause,
        };
        if self.execution == ExecutionState::Halted {
            if !self.traps.is_terminal() {
                self.traps
                    .record_failed_entry(&self.architectural, cause, payload, resume_pc);
            }
            return Err(fault(Cause::Halted));
        }
        if self.traps.is_terminal() {
            return Err(fault(Cause::TerminalTrap));
        }
        if self.traps.has_active_frame() {
            let snapshot = self.architectural.clone();
            self.traps
                .record_double_trap(&snapshot, cause, payload, resume_pc);
            return Err(fault(Cause::DoubleTrap));
        }
        let Some(target) = self.traps.tvec() else {
            self.traps
                .record_failed_entry(&self.architectural, cause, payload, resume_pc);
            return Err(fault(Cause::Control(
                ControlStateError::InvalidControlState {
                    operation: "enter trap without TVEC",
                    selector: ControlRegister::Tvec.as_u8(),
                },
            )));
        };
        let candidate = prepare_entry_control(
            self.architectural.config(),
            &self.architectural,
            memory,
            target,
        );
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(source) => {
                self.traps
                    .record_failed_entry(&self.architectural, cause, payload, resume_pc);
                return Err(fault(Cause::TrapEntry(source)));
            }
        };
        let snapshot = TrapController::snapshot(&self.architectural);
        let resume_sp = self.architectural.sp();
        let resume_status = self.architectural.status().bits();
        self.traps.install_frame(
            snapshot,
            cause,
            payload,
            resume_pc,
            resume_sp,
            resume_status,
        );
        self.architectural = candidate;
        Ok(())
    }

    fn validate_fetch<E>(&self) -> Result<(), CpuFault<E>> {
        let fault = |cause| CpuFault {
            pc: self.architectural.pc(),
            opcode: None,
            cause,
        };
        if self.traps.is_terminal() {
            return Err(fault(Cause::TerminalTrap));
        }
        if self.execution == ExecutionState::Halted {
            return Err(fault(Cause::Halted));
        }
        let config = self.architectural.config();
        validate_pc(config, self.architectural.pc()).map_err(|e| fault(Cause::Control(e)))?;
        config
            .word_width()
            .checked_access_end(self.architectural.pc().as_u64(), 8)
            .map_err(|e| fault(Cause::Width(e)))?;
        Ok(())
    }

    pub fn step<M: CpuMemory>(
        &mut self,
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>> {
        self.validate_fetch()?;
        let bytes = memory
            .fetch_instruction(
                self.architectural.config(),
                self.architectural.pc(),
                self.architectural.privilege(),
            )
            .map_err(|source| CpuFault {
                pc: self.architectural.pc(),
                opcode: None,
                cause: Cause::Fetch(source),
            })?;
        self.step_bytes(&bytes, memory)
    }

    pub fn step_bytes<M: CpuMemory>(
        &mut self,
        bytes: &[u8],
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>> {
        self.validate_fetch()?;
        let instruction =
            decode(self.architectural.config(), bytes).map_err(|source| CpuFault {
                pc: self.architectural.pc(),
                opcode: bytes.first().copied(),
                cause: Cause::Decode(source),
            })?;
        self.execute(&instruction, memory)
    }

    pub fn execute<M: CpuMemory>(
        &mut self,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>> {
        self.validate_fetch().map_err(|mut error| {
            error.opcode = Some(instruction.opcode().as_u8());
            error
        })?;
        let pc = self.architectural.pc();
        let opcode = instruction.opcode();
        let fault = |cause| CpuFault {
            pc,
            opcode: Some(opcode.as_u8()),
            cause,
        };
        let config = self.architectural.config();
        let width = config.word_width();
        instruction
            .validate(config)
            .map_err(|e| fault(Cause::Instruction(e)))?;
        if instruction.definition().supervisor_only
            && self.architectural.privilege() != Privilege::Supervisor
        {
            return Err(fault(Cause::PrivilegeViolation));
        }
        let next_pc = checked_next_pc(config, pc).map_err(|e| fault(Cause::NextPc(e)))?;
        let read = |index| self.architectural.registers().read(index);
        match (opcode, instruction.operands()) {
            (Opcode::Rfe, []) => {
                let (resume_pc, resume_sp, resume_status) = self
                    .traps
                    .return_control()
                    .map_err(|source| fault(Cause::Control(source)))?;
                if !self.traps.consume_syscall_return_authorization() {
                    return Err(fault(Cause::Control(
                        ControlStateError::InvalidControlState {
                            operation: "return",
                            selector: ControlRegister::Epc.as_u8(),
                        },
                    )));
                }
                let mut candidate = self.architectural.clone();
                candidate
                    .restore_control(resume_pc, resume_sp, resume_status)
                    .map_err(|source| fault(Cause::Control(source)))?;
                self.architectural = candidate;
                self.traps.commit_return();
                return Ok(OutcomeApplication::Continue);
            }
            (Opcode::Csrr, [Operand::Register(d), Operand::Control(control)]) => {
                let value = self
                    .traps
                    .read_control(*control)
                    .map_err(|source| fault(Cause::Control(source)))?;
                let mut candidate = self.architectural.clone();
                candidate.write_register(*d, value);
                candidate
                    .set_pc(next_pc)
                    .map_err(|source| fault(Cause::Control(source)))?;
                self.architectural = candidate;
                return Ok(OutcomeApplication::Continue);
            }
            (Opcode::Csrw, [Operand::Control(control), Operand::Register(source)]) => {
                let value = read(*source);
                self.traps
                    .write_control(*control, value)
                    .map_err(|error| fault(Cause::Control(error)))?;
                self.architectural
                    .set_pc(next_pc)
                    .map_err(|source| fault(Cause::Control(source)))?;
                return Ok(OutcomeApplication::Continue);
            }
            _ => {}
        }
        let mut candidate = self.architectural.clone();
        let mut execution = self.execution;
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
                candidate.write_register(*d, self.architectural.sp().as_u64())
            }
            (Opcode::Getstatus, [Register(d)]) => {
                candidate.write_register(*d, self.architectural.status().bits())
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
                    self.architectural.privilege(),
                )
                .map_err(|e| fault(Cause::DataAccess(e)))?;
                data = Some((*d, access));
            }
            (Opcode::Br, [Condition(condition), Immediate(i)]) => {
                if self.architectural.status().matches(*condition) {
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
                checked_return_sp(config, self.architectural.sp())
                    .map_err(|e| fault(Cause::NextPc(e)))?;
                let size = match width {
                    WordWidth::W32 => DataSize::Word,
                    WordWidth::W64 => DataSize::Double,
                };
                let access = DataAccess::new(
                    config,
                    self.architectural.sp(),
                    0,
                    size,
                    DataAccessKind::StackRead,
                    self.architectural.privilege(),
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
        self.architectural = candidate;
        self.execution = execution;
        Ok(application)
    }
}
