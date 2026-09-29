//! The canonical guest-visible processor state.
//!
//! # Why this type exists
//!
//! Before it existed, [`ReferenceInterpreter`](crate::ReferenceInterpreter) *owned*
//! the architectural state, the execution state and the trap controller. That was
//! correct while there was exactly one engine, and it is the reason there could not
//! be a second one: a second engine either had to reach inside the first — which is
//! precisely the door the debug API is built not to have — or had to hold its own
//! copy, and a second copy of a program's registers is a second architectural
//! truth. Two truths about `pc` is the same defect as no truth about `pc`.
//!
//! So the state moved out of the engine and into [`Processor`], and the machine
//! owns the `Processor`. An engine is now something that *operates on* the
//! processor rather than something that *is* the processor.
//!
//! # The rule this enforces
//!
//! > An engine may hold private state. An engine may never hold architectural
//! > state.
//!
//! A translation cache, a host register file, a compiled block — all of those
//! belong to an engine, and all of them are discardable without the guest being
//! able to tell. The program counter, the registers, the status register, the trap
//! frames and the execution context belong to the [`Processor`], and there is one
//! per machine.
//!
//! # What is deliberately not here
//!
//! Memory, devices, the clock, and the machine's own lifecycle. A processor is
//! what a guest can observe *about itself*; a machine is what a host can observe
//! about the whole system. Keeping them apart is what lets the same processor be
//! stepped by either engine without either engine needing to know what a bus is.

use crate::{
    ArchitecturalState, ControlStateError, CpuFault, CpuFaultCause, CpuMemory, DoubleTrap,
    ExecutionState, Privilege, SyscallCompletion, TrapAttempt, TrapCause, TrapController,
    validate_pc, validate_sp,
};
use lazalith_isa::ControlRegister;
use lazalith_types::{ArchitectureConfig, InstructionAddress, RegisterIndex, VirtualAddress};

/// Everything a guest can observe about its own processor.
///
/// One of these exists per virtual machine. The machine owns it; the execution
/// engine borrows it for the length of a step and gives it back.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Processor {
    architectural: ArchitecturalState,
    execution: ExecutionState,
    traps: TrapController,
}

impl Processor {
    /// A processor at a known architectural state.
    ///
    /// This is the same validation a step performs, done once, at the point where
    /// the state is created rather than every time it is used. A state that could
    /// not have been reached by a reset cannot be restored from one either.
    pub fn new(architectural: ArchitecturalState) -> Self {
        let traps = TrapController::new(architectural.config());
        Self {
            architectural,
            execution: ExecutionState::Running,
            traps,
        }
    }

    /// The guest-visible architectural state: registers, `pc`, `sp`, status.
    pub const fn architectural(&self) -> &ArchitecturalState {
        &self.architectural
    }

    /// The architectural state, mutably.
    ///
    /// Every mutation on this is a *transition* the ISA defines, not an
    /// assignment, and [`Processor::commit`] is how one is applied. An engine
    /// reaches for this to build a candidate and then commits it, so a step that
    /// faults half way through leaves nothing half-applied.
    pub const fn architectural_mut(&mut self) -> &mut ArchitecturalState {
        &mut self.architectural
    }

    /// Applies a candidate architectural state, unchanged.
    ///
    /// The engine validates before it mutates, so this is a commit and not a
    /// check — and it stays the *only* way architectural state changes, which is
    /// what makes "validate, calculate, commit" a property of the type rather than
    /// a habit.
    pub fn commit(&mut self, architectural: ArchitecturalState) {
        self.architectural = architectural;
    }

    /// Whether the processor is running or halted.
    pub const fn execution(&self) -> ExecutionState {
        self.execution
    }

    pub fn halt(&mut self) {
        self.execution = ExecutionState::Halted;
    }

    pub fn resume(&mut self) {
        self.execution = ExecutionState::Running;
    }

    /// Applies a candidate execution state, unchanged.
    ///
    /// An outcome decides whether the processor is still running; this is how that
    /// decision is applied, alongside [`Processor::commit`] for the architectural
    /// half, so a step's two effects stay paired.
    pub fn set_execution(&mut self, execution: ExecutionState) {
        self.execution = execution;
    }

    /// The trap controller: the vector, the frame stack, and the syscall
    /// admission handshake.
    pub const fn traps(&self) -> &TrapController {
        &self.traps
    }

    pub fn traps_mut(&mut self) -> &mut TrapController {
        &mut self.traps
    }

    /// The whole processor, for a snapshot.
    pub fn capture(&self) -> Processor {
        self.clone()
    }

    /// Builds a processor from its three parts, for a snapshot restore.
    ///
    /// The architectural state is validated exactly as `restore` validates it, and
    /// the parts arrive in the same order `Processor::new` establishes them, so
    /// there is one way a processor is created and one way it is checked. A caller
    /// that has the parts in a different shape — a snapshot holding them
    /// separately, say — goes through here rather than assembling a struct.
    pub fn from_parts(
        architectural: ArchitecturalState,
        execution: ExecutionState,
        traps: TrapController,
    ) -> Result<Self, ControlStateError> {
        if architectural.config() != traps.config() {
            return Err(ControlStateError::InvalidControlState {
                operation: "restore execution context",
                selector: 0,
            });
        }
        validate_pc(architectural.config(), architectural.pc())?;
        validate_sp(architectural.config(), architectural.sp())?;
        Ok(Self {
            architectural,
            execution,
            traps,
        })
    }

    /// Puts back a whole processor state, trap frames included.
    ///
    /// A machine snapshot needs this because a program stopped in a syscall is
    /// not resumable from its architectural state alone: the trap frame is where
    /// the return address and the saved registers are, and restoring the registers
    /// without it would leave a frame the program never returns through. The
    /// architectural state is restored *first* and through the same validation a
    /// normal step uses — a restore is not a way to smuggle an inconsistent
    /// processor past the checks the machine makes every step.
    pub fn restore(&mut self, other: Processor) -> Result<(), ControlStateError> {
        self.restore_architectural(other.architectural)?;
        self.traps = other.traps;
        self.execution = other.execution;
        Ok(())
    }

    /// Replaces the architectural state, as a context switch does.
    pub fn replace_architectural(
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

    pub fn restore_architectural(
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

    pub fn restore_architectural_with_execution(
        &mut self,
        state: ArchitecturalState,
        execution: ExecutionState,
    ) -> Result<(), ControlStateError> {
        self.restore_architectural(state)?;
        self.execution = execution;
        Ok(())
    }

    pub fn write_register(&mut self, index: RegisterIndex, value: u64) {
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

    /// The privilege the processor is currently at.
    pub fn privilege(&self) -> Privilege {
        self.architectural.privilege()
    }

    /// The configuration this processor is for.
    pub const fn config(&self) -> ArchitectureConfig {
        self.architectural.config()
    }

    /// The stack pointer, as a host address value.
    pub fn stack_pointer(&self) -> VirtualAddress {
        self.architectural.sp()
    }

    // -- trap entry -----------------------------------------------------------
    //
    // Trap entry is an architectural transition, not an engine one. Every engine
    // has to enter a trap the same way or a fault means two different things
    // depending on which engine noticed it, and a JIT that pushed its own frame
    // would leave a program that the interpreter could not describe.

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
            return Err(CpuFault::at(
                self.architectural.pc(),
                None,
                CpuFaultCause::Control(ControlStateError::InvalidControlState {
                    operation: "invalid fault cause",
                    selector: 0,
                }),
            ));
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
            return Err(CpuFault::at(
                self.architectural.pc(),
                None,
                CpuFaultCause::TerminalTrap,
            ));
        }
        if self.execution == ExecutionState::Halted {
            return Err(CpuFault::at(
                self.architectural.pc(),
                None,
                CpuFaultCause::Halted,
            ));
        }
        if self.traps.has_active_frame() {
            return Err(CpuFault::at(
                self.architectural.pc(),
                None,
                CpuFaultCause::DeferredInterrupt,
            ));
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
        let fault = |cause| CpuFault::at(self.architectural.pc(), None, cause);
        if self.execution == ExecutionState::Halted {
            if !self.traps.is_terminal() {
                self.traps
                    .record_failed_entry(&self.architectural, cause, payload, resume_pc);
            }
            return Err(fault(CpuFaultCause::Halted));
        }
        if self.traps.is_terminal() {
            return Err(fault(CpuFaultCause::TerminalTrap));
        }
        if self.traps.has_active_frame() {
            let snapshot = self.architectural.clone();
            self.traps
                .record_double_trap(&snapshot, cause, payload, resume_pc);
            return Err(fault(CpuFaultCause::DoubleTrap));
        }
        let Some(target) = self.traps.tvec() else {
            self.traps
                .record_failed_entry(&self.architectural, cause, payload, resume_pc);
            return Err(fault(CpuFaultCause::Control(
                ControlStateError::InvalidControlState {
                    operation: "enter trap without TVEC",
                    selector: ControlRegister::Tvec.as_u8(),
                },
            )));
        };
        let candidate = crate::prepare_entry_control(
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
                return Err(fault(CpuFaultCause::TrapEntry(source)));
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

    /// Authorises a syscall return, so the `RFE` that follows is the one this
    /// kernel was answering and not one the guest issued.
    pub fn authorize_syscall_return(&mut self, completion: &SyscallCompletion) -> bool {
        self.traps.authorize_syscall_return(completion)
    }

    /// Withdraws an authorisation, for a syscall return that did not happen.
    pub fn clear_syscall_return_authorization(&mut self) {
        self.traps.clear_syscall_return_authorization();
    }

    /// The record of a trap that failed to enter, if one is kept.
    pub fn failed_entry(&self) -> Option<&TrapAttempt> {
        self.traps.failed_entry()
    }

    /// The record of a double trap, if one is kept.
    pub fn double_trap(&self) -> Option<&DoubleTrap> {
        self.traps.double_trap()
    }
}
