#![no_std]

extern crate alloc;

mod interrupts;
pub use interrupts::InterruptController;

use alloc::{boxed::Box, collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_cpu::{
    ArchitecturalState, ControlStateError, CpuFault, CpuFaultCause, DataAccessError,
    ExecutionContextId, OutcomeApplication, OutcomeErrorKind, ReferenceInterpreter,
    SyscallCompletion, TrapAttempt, TrapCause, TrapController, TrapRequest,
};
use lazalith_devices::{Device, DeviceId, DeviceManager};
use lazalith_isa::{DecodeError, InstructionError, Opcode, ValidationError, decode};
use lazalith_memory::{
    AddressSpace, AddressSpaceSwapError, Bus, MemoryFault, MemoryFaultKind, MemoryRegion,
    RegionPermissions, UserSpace,
};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, InterruptId, PhysicalAddress,
    RegisterIndex, VirtualAddress, VirtualClock, WidthError,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineState {
    Created,
    Reset,
    Running,
    Paused,
    Halted,
    Faulted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineOperation {
    Reset,
    Step,
    Run,
    Pause,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TrapEvent {
    pub cause: TrapCause,
    pub payload: u64,
    pub resume_pc: InstructionAddress,
    pub interrupt: Option<InterruptId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineEvent {
    Stepped { application: OutcomeApplication },
    Trapped { event: TrapEvent },
    Halted,
}

enum PendingTrap {
    Fault {
        cause: TrapCause,
        resume_pc: InstructionAddress,
        original: CpuFault<MemoryFault>,
    },
    Syscall {
        resume_pc: InstructionAddress,
    },
    Software {
        payload: i32,
        resume_pc: InstructionAddress,
    },
    External {
        id: InterruptId,
        resume_pc: InstructionAddress,
    },
}

#[derive(Debug)]
pub enum MachineError {
    TrapEntry {
        attempt: Box<TrapAttempt>,
        original: Option<Box<CpuFault<MemoryFault>>>,
        failure: Box<CpuFault<MemoryFault>>,
    },
    TrapEntryContextMissing,
    InterruptAllocation(TryReserveError),
    InterruptAcknowledgement {
        id: InterruptId,
    },
    Clock(lazalith_types::ClockOverflow),
    Device(lazalith_devices::DeviceError),
    Memory(lazalith_memory::MemoryFault),
    AddressSpaceSwap(AddressSpaceSwapError),
    Cpu(lazalith_cpu::ControlStateError),
    Decode(DecodeError),
    InvalidSyscallReturn,
    InvalidUserContext {
        reason: &'static str,
    },
    InvalidTransition {
        operation: MachineOperation,
        state: MachineState,
    },
    InstructionCountOverflow,
    InitialClock {
        devices: CycleCount,
        requested: CycleCount,
    },
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { operation, state } => {
                write!(f, "{operation:?} is invalid while machine is {state:?}")
            }
            Self::TrapEntry { failure, .. } => {
                write!(f, "trap entry failed: {failure}")
            }
            Self::InterruptAcknowledgement { id } => {
                write!(f, "accepted interrupt {id:?} could not be acknowledged")
            }
            Self::Decode(source) => write!(f, "instruction decode failed: {source}"),
            Self::AddressSpaceSwap(source) => source.fmt(f),
            Self::InvalidSyscallReturn => {
                f.write_str("syscall return requires an active RFE handler")
            }
            Self::InvalidUserContext { reason } => {
                write!(f, "invalid User execution context: {reason}")
            }
            _ => write!(f, "machine rejected operation: {self:?}"),
        }
    }
}

impl Error for MachineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TrapEntry { failure, .. } => Some(failure.as_ref()),
            Self::InterruptAllocation(source) => Some(source),
            Self::Clock(source) => Some(source),
            Self::Device(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::AddressSpaceSwap(source) => Some(source),
            Self::Cpu(source) => Some(source),
            Self::Decode(source) => Some(source),
            Self::InvalidTransition { .. }
            | Self::TrapEntryContextMissing
            | Self::InterruptAcknowledgement { .. }
            | Self::InvalidSyscallReturn
            | Self::InvalidUserContext { .. }
            | Self::InstructionCountOverflow
            | Self::InitialClock { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct MachineSetup<D: Device> {
    pub config: ArchitectureConfig,
    pub devices: DeviceManager<D>,
    pub regions: Vec<MemoryRegion>,
    pub pc: InstructionAddress,
    pub sp: VirtualAddress,
    pub status: u64,
    pub initial_time: CycleCount,
}

#[derive(Debug)]
pub struct LazalithMachine<D: Device> {
    cpu: ReferenceInterpreter,
    bus: Bus<D>,
    config: ArchitectureConfig,
    clock: VirtualClock,
    initial_state: ArchitecturalState,
    initial_time: CycleCount,
    state: MachineState,
    interrupts: InterruptController,
    last_trap_fault: Option<Box<CpuFault<MemoryFault>>>,
    executed: u64,
}

impl<D: Device> LazalithMachine<D> {
    pub fn new(setup: MachineSetup<D>) -> Result<Self, MachineError> {
        let MachineSetup {
            config,
            devices,
            regions,
            pc,
            sp,
            status,
            initial_time,
        } = setup;
        let mut space = AddressSpace::new(config);
        for region in regions {
            space.map(region).map_err(MachineError::Memory)?;
        }
        let device_time = devices.clock().elapsed();
        let delta = initial_time
            .checked_sub(device_time)
            .ok_or(MachineError::InitialClock {
                devices: device_time,
                requested: initial_time,
            })?;
        let initial_state =
            ArchitecturalState::new(config, pc, sp, status).map_err(MachineError::Cpu)?;
        let cpu = ReferenceInterpreter::new(initial_state.clone());
        let mut bus = Bus::with_devices(space, devices);
        bus.tick_devices(delta).map_err(MachineError::Device)?;
        let clock = VirtualClock::at(initial_time);
        Ok(Self {
            cpu,
            bus,
            config,
            clock,
            initial_state,
            initial_time,
            state: MachineState::Created,
            interrupts: InterruptController::new(),
            last_trap_fault: None,
            executed: 0,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }
    pub const fn clock(&self) -> &VirtualClock {
        &self.clock
    }
    pub const fn state(&self) -> MachineState {
        self.state
    }
    pub const fn is_halted(&self) -> bool {
        matches!(self.state, MachineState::Halted)
    }
    pub fn architectural_state(&self) -> &ArchitecturalState {
        self.cpu.architectural_state()
    }
    pub fn write_register(&mut self, index: RegisterIndex, value: u64) -> Result<(), MachineError> {
        if self.active_execution_context().is_some()
            || self.cpu.trap_controller().has_active_frame()
        {
            return Err(MachineError::InvalidUserContext {
                reason: "direct register mutation is not allowed during an active context",
            });
        }
        self.cpu.write_register(index, value);
        Ok(())
    }

    fn write_register_unrestricted(&mut self, index: RegisterIndex, value: u64) {
        self.cpu.write_register(index, value);
    }
    pub fn return_from_syscall(
        &mut self,
        completion: SyscallCompletion,
    ) -> Result<MachineEvent, MachineError> {
        self.ensure_executable(MachineOperation::Step)?;
        let frame = self
            .cpu
            .trap_controller()
            .frame()
            .ok_or(MachineError::InvalidSyscallReturn)?;
        if frame.cause() != TrapCause::Syscall
            || !self.cpu.trap_controller().completion_matches(&completion)
            || self.architectural_state().privilege() != lazalith_cpu::Privilege::Supervisor
            || self.architectural_state().status().interrupts_enabled()
        {
            return Err(MachineError::InvalidSyscallReturn);
        }
        let bytes = self
            .inspect_instruction(self.architectural_state().pc())
            .map_err(MachineError::Memory)?;
        let instruction = decode(self.config, &bytes).map_err(MachineError::Decode)?;
        if instruction.opcode() != Opcode::Rfe || !instruction.operands().is_empty() {
            return Err(MachineError::InvalidSyscallReturn);
        }
        if self.executed == u64::MAX {
            return Err(MachineError::InstructionCountOverflow);
        }
        if !self
            .cpu
            .trap_controller_mut()
            .authorize_syscall_return(&completion)
        {
            return Err(MachineError::InvalidSyscallReturn);
        }
        self.write_register_unrestricted(
            RegisterIndex::try_from(0).map_err(|_| MachineError::InvalidSyscallReturn)?,
            u64::from(completion.status()),
        );
        self.write_register_unrestricted(
            RegisterIndex::try_from(1).map_err(|_| MachineError::InvalidSyscallReturn)?,
            u64::from(completion.payload()),
        );
        let result = self.step_managed(completion.execution_context());
        if result.is_err() {
            self.cpu
                .trap_controller_mut()
                .clear_syscall_return_authorization();
        }
        result
    }
    pub fn with_user_context<R>(
        &mut self,
        operation: impl FnOnce(&TrapController, &ArchitecturalState, &mut dyn UserSpace) -> R,
    ) -> R {
        operation(
            self.cpu.trap_controller(),
            self.cpu.architectural_state(),
            self.bus.user_address_space_mut(),
        )
    }

    pub fn release_user_context(
        &mut self,
        address_space: &mut AddressSpace,
        execution_context: ExecutionContextId,
    ) -> Result<(), MachineError> {
        if self.cpu.trap_controller().has_active_frame() {
            return Err(MachineError::InvalidUserContext {
                reason: "cannot release while a trap frame is active",
            });
        }
        if self.active_execution_context() != Some(execution_context) {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context is not active",
            });
        }
        if address_space.config() != self.config {
            return Err(MachineError::InvalidUserContext {
                reason: "address space architecture differs from machine",
            });
        }
        self.bus
            .swap_user_address_space(address_space)
            .map_err(MachineError::AddressSpaceSwap)?;
        if !self
            .cpu
            .trap_controller_mut()
            .clear_execution_context(execution_context)
        {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context changed during context release",
            });
        }
        self.state = MachineState::Halted;
        Ok(())
    }

    pub fn recover_user_context(
        &mut self,
        address_space: &mut AddressSpace,
        execution_context: ExecutionContextId,
    ) -> Result<(), MachineError> {
        if self.active_execution_context() != Some(execution_context) {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context is not active",
            });
        }
        if address_space.config() != self.config {
            return Err(MachineError::InvalidUserContext {
                reason: "address space architecture differs from machine",
            });
        }
        self.bus
            .swap_user_address_space(address_space)
            .map_err(MachineError::AddressSpaceSwap)?;
        let _ = self
            .cpu
            .trap_controller_mut()
            .clear_execution_context(execution_context);
        self.state = MachineState::Halted;
        Ok(())
    }

    pub fn invalidate_user_context(
        &mut self,
        execution_context: ExecutionContextId,
    ) -> Result<(), MachineError> {
        if self.active_execution_context() != Some(execution_context) {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context is not active",
            });
        }
        let _ = self
            .cpu
            .trap_controller_mut()
            .clear_execution_context(execution_context);
        self.state = MachineState::Halted;
        Ok(())
    }

    pub fn active_execution_context(&self) -> Option<ExecutionContextId> {
        self.cpu.trap_controller().execution_context()
    }

    pub fn activate_user_context(
        &mut self,
        address_space: &mut AddressSpace,
        cpu: ArchitecturalState,
        execution_context: ExecutionContextId,
    ) -> Result<(), MachineError> {
        if address_space.config() != self.config {
            return Err(MachineError::InvalidUserContext {
                reason: "address space architecture differs from machine",
            });
        }
        if cpu.config() != self.config || cpu.privilege() != lazalith_cpu::Privilege::User {
            return Err(MachineError::InvalidUserContext {
                reason: "CPU context is not a User state for this machine",
            });
        }
        if self.cpu.trap_controller().has_active_frame() || self.cpu.trap_controller().is_terminal()
        {
            return Err(MachineError::InvalidUserContext {
                reason: "cannot switch while a trap frame is active",
            });
        }
        if self.active_execution_context().is_some() {
            return Err(MachineError::InvalidUserContext {
                reason: "an execution context is already active",
            });
        }
        if !matches!(
            self.state,
            MachineState::Reset
                | MachineState::Running
                | MachineState::Paused
                | MachineState::Halted
        ) {
            return Err(MachineError::InvalidUserContext {
                reason: "machine is not in a context-switchable state",
            });
        }
        if self.bus.has_device_mappings() {
            return Err(MachineError::InvalidUserContext {
                reason: "cannot replace an address space with device mappings",
            });
        }
        let previous = self.cpu.architectural_state().clone();
        let previous_execution = self.cpu.execution_state();
        self.cpu
            .replace_architectural_state(cpu)
            .map_err(MachineError::Cpu)?;
        if let Err(error) = self.bus.swap_user_address_space(address_space) {
            let _ = self
                .cpu
                .restore_architectural_state_with_execution(previous, previous_execution);
            return Err(MachineError::AddressSpaceSwap(error));
        }
        self.last_trap_fault = None;
        self.cpu
            .trap_controller_mut()
            .set_execution_context(execution_context);
        self.state = MachineState::Running;
        Ok(())
    }

    pub fn abort_trap(&mut self) -> Result<(), MachineError> {
        if !self.cpu.trap_controller_mut().abort_frame() {
            return Err(MachineError::InvalidUserContext {
                reason: "no active trap frame to abort",
            });
        }
        Ok(())
    }

    pub fn devices(&self) -> &DeviceManager<D> {
        self.bus.devices()
    }
    pub const fn memory(&self) -> &AddressSpace {
        self.bus.address_space()
    }
    pub const fn trap_controller(&self) -> &TrapController {
        self.cpu.trap_controller()
    }
    pub fn with_trap_controller_mut<R>(
        &mut self,
        execution_context: Option<ExecutionContextId>,
        operation: impl FnOnce(&mut TrapController) -> R,
    ) -> Result<R, MachineError> {
        if self.active_execution_context() != execution_context {
            return Err(MachineError::InvalidUserContext {
                reason: "trap-controller access does not match the active execution context",
            });
        }
        Ok(operation(self.cpu.trap_controller_mut()))
    }
    pub const fn interrupts(&self) -> &InterruptController {
        &self.interrupts
    }
    pub fn last_trap_fault(&self) -> Option<&CpuFault<MemoryFault>> {
        self.last_trap_fault.as_deref()
    }
    pub fn request_interrupt(&mut self, id: InterruptId) -> Result<bool, MachineError> {
        self.interrupts
            .request(id)
            .map_err(MachineError::InterruptAllocation)
    }
    pub fn set_trap_vector(&mut self, target: InstructionAddress) -> Result<(), MachineError> {
        self.ensure_host_mutation_allowed()?;
        self.cpu.set_trap_vector(target).map_err(MachineError::Cpu)
    }

    pub fn peek_memory(
        &self,
        address: PhysicalAddress,
        output: &mut [u8],
    ) -> Result<(), MemoryFault> {
        self.bus.peek(address, output)
    }

    pub fn inspect_instruction(&self, pc: InstructionAddress) -> Result<[u8; 8], MemoryFault> {
        self.bus
            .fetch_instruction(self.config, pc, self.architectural_state().privilege())
    }

    fn ensure_host_mutation_allowed(&self) -> Result<(), MachineError> {
        if self.active_execution_context().is_some()
            || self.cpu.trap_controller().has_active_frame()
        {
            return Err(MachineError::InvalidUserContext {
                reason: "host memory mutation is not allowed during an active execution context",
            });
        }
        Ok(())
    }

    pub fn load_region(&mut self, region: MemoryRegion) -> Result<(), MachineError> {
        self.ensure_host_mutation_allowed()?;
        self.bus.map(region).map_err(MachineError::Memory)
    }

    pub fn load_bytes(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MachineError> {
        self.ensure_host_mutation_allowed()?;
        self.bus
            .initialize(address, bytes)
            .map_err(MachineError::Memory)
    }

    pub fn map_device(
        &mut self,
        id: DeviceId,
        start: PhysicalAddress,
        permissions: RegionPermissions,
    ) -> Result<(), MachineError> {
        self.ensure_host_mutation_allowed()?;
        self.bus
            .map_device(id, start, permissions)
            .map_err(MachineError::Memory)
    }

    pub fn reset(&mut self) {
        self.bus.reset_devices(self.initial_time);
        self.cpu = ReferenceInterpreter::new(self.initial_state.clone());
        self.clock = VirtualClock::at(self.initial_time);
        self.interrupts.reset();
        self.last_trap_fault = None;
        self.state = MachineState::Reset;
    }

    fn ensure_executable(&self, operation: MachineOperation) -> Result<(), MachineError> {
        if matches!(
            self.state,
            MachineState::Reset | MachineState::Running | MachineState::Paused
        ) {
            return Ok(());
        }
        Err(MachineError::InvalidTransition {
            operation,
            state: self.state,
        })
    }

    fn finish_trap(
        &mut self,
        result: Result<(), CpuFault<MemoryFault>>,
        cause: TrapCause,
        payload: u64,
        resume_pc: InstructionAddress,
        interrupt: Option<InterruptId>,
        original: Option<CpuFault<MemoryFault>>,
    ) -> Result<TrapEvent, MachineError> {
        match result {
            Ok(()) => {
                self.last_trap_fault = original.map(Box::new);
                Ok(TrapEvent {
                    cause,
                    payload,
                    resume_pc,
                    interrupt,
                })
            }
            Err(failure) => {
                self.state = MachineState::Faulted;
                let attempt = self
                    .cpu
                    .trap_controller()
                    .failed_entry()
                    .cloned()
                    .or_else(|| {
                        self.cpu
                            .trap_controller()
                            .double_trap()
                            .map(|double| double.attempt())
                    });
                let Some(attempt) = attempt else {
                    return Err(MachineError::TrapEntryContextMissing);
                };
                Err(MachineError::TrapEntry {
                    attempt: Box::new(attempt),
                    original: original.map(Box::new),
                    failure: Box::new(failure),
                })
            }
        }
    }

    fn enter_trap(&mut self, pending: PendingTrap) -> Result<TrapEvent, MachineError> {
        match pending {
            PendingTrap::Fault {
                cause,
                resume_pc,
                original,
            } => {
                let result = self.cpu.enter_fault(&mut self.bus, cause, resume_pc);
                self.finish_trap(result, cause, 0, resume_pc, None, Some(original))
            }
            PendingTrap::Syscall { resume_pc } => {
                let result = self.cpu.enter_syscall(&mut self.bus, resume_pc);
                self.finish_trap(result, TrapCause::Syscall, 0, resume_pc, None, None)
            }
            PendingTrap::Software { payload, resume_pc } => {
                let result = self.cpu.enter_software(&mut self.bus, payload, resume_pc);
                let extended = sign_extended_payload(self.config, payload);
                self.finish_trap(
                    result,
                    TrapCause::SoftwareTrap,
                    extended,
                    resume_pc,
                    None,
                    None,
                )
            }
            PendingTrap::External { id, resume_pc } => {
                let result = self
                    .cpu
                    .enter_external(&mut self.bus, id.as_u16(), resume_pc);
                self.finish_trap(
                    result,
                    TrapCause::ExternalInterrupt,
                    u64::from(id.as_u16()),
                    resume_pc,
                    Some(id),
                    None,
                )
            }
        }
    }

    fn try_external_interrupt(&mut self) -> Result<Option<TrapEvent>, MachineError> {
        if self.state != MachineState::Running
            || !self.architectural_state().status().interrupts_enabled()
            || self.cpu.trap_controller().has_active_frame()
        {
            return Ok(None);
        }
        let Some(id) = self.interrupts.peek() else {
            return Ok(None);
        };
        let resume_pc = self.architectural_state().pc();
        let event = self.enter_trap(PendingTrap::External { id, resume_pc })?;
        if !self.interrupts.acknowledge(id) {
            self.state = MachineState::Faulted;
            return Err(MachineError::InterruptAcknowledgement { id });
        }
        Ok(Some(event))
    }

    pub fn step(&mut self) -> Result<MachineEvent, MachineError> {
        if self.active_execution_context().is_some() {
            return Err(MachineError::InvalidUserContext {
                reason: "direct stepping is not allowed during an active context",
            });
        }
        self.step_inner()
    }

    pub fn step_managed(
        &mut self,
        execution_context: ExecutionContextId,
    ) -> Result<MachineEvent, MachineError> {
        if self.active_execution_context() != Some(execution_context) {
            return Err(MachineError::InvalidUserContext {
                reason: "managed step execution context does not match",
            });
        }
        self.step_inner()
    }

    fn step_inner(&mut self) -> Result<MachineEvent, MachineError> {
        self.ensure_executable(MachineOperation::Step)?;
        if let Some(event) = self.try_external_interrupt()? {
            return Ok(MachineEvent::Trapped { event });
        }
        let executed = self
            .executed
            .checked_add(1)
            .ok_or(MachineError::InstructionCountOverflow)?;
        let had_frame = self.cpu.trap_controller().has_active_frame();
        let application = match self.cpu.step(&mut self.bus) {
            Ok(application) => application,
            Err(fault) => {
                let cause = trap_cause(&fault);
                let resume_pc = fault.pc;
                let event = self.enter_trap(PendingTrap::Fault {
                    cause,
                    resume_pc,
                    original: fault,
                })?;
                return Ok(MachineEvent::Trapped { event });
            }
        };
        self.executed = executed;
        if had_frame && !self.cpu.trap_controller().has_active_frame() {
            self.last_trap_fault = None;
        }
        match application {
            OutcomeApplication::Halted => {
                self.state = MachineState::Halted;
                Ok(MachineEvent::Halted)
            }
            OutcomeApplication::Trap { request, resume_pc } => {
                let pending = match request {
                    TrapRequest::Syscall => PendingTrap::Syscall { resume_pc },
                    TrapRequest::Software(payload) => PendingTrap::Software { payload, resume_pc },
                };
                let event = self.enter_trap(pending)?;
                Ok(MachineEvent::Trapped { event })
            }
            OutcomeApplication::Continue => Ok(MachineEvent::Stepped { application }),
        }
    }

    pub fn run(&mut self, limit: u64) -> Result<MachineRun, MachineError> {
        if self.active_execution_context().is_some() {
            return Err(MachineError::InvalidUserContext {
                reason: "direct run is not allowed during an active context",
            });
        }
        self.run_inner(limit)
    }

    fn run_inner(&mut self, limit: u64) -> Result<MachineRun, MachineError> {
        self.ensure_executable(MachineOperation::Run)?;
        if limit != 0 {
            self.executed
                .checked_add(1)
                .ok_or(MachineError::InstructionCountOverflow)?;
        }
        self.state = MachineState::Running;
        let mut executed = 0u64;
        let mut trap = None;
        let halted_at = loop {
            if executed == limit {
                break None;
            }
            match self.step()? {
                MachineEvent::Halted => {
                    executed += 1;
                    break Some(self.executed);
                }
                MachineEvent::Trapped { event } => {
                    if matches!(event.cause, TrapCause::Syscall | TrapCause::SoftwareTrap) {
                        executed += 1;
                    }
                    trap = Some(event);
                    break None;
                }
                MachineEvent::Stepped { .. } => {
                    executed += 1;
                }
            }
        };
        Ok(MachineRun {
            executed,
            halted_at,
            trap,
        })
    }

    pub fn pause(&mut self) -> Result<(), MachineError> {
        if self.state != MachineState::Running {
            return Err(MachineError::InvalidTransition {
                operation: MachineOperation::Pause,
                state: self.state,
            });
        }
        self.state = MachineState::Paused;
        Ok(())
    }

    pub fn advance_clock(&mut self, delta: CycleCount) -> Result<(), MachineError> {
        let next = self.clock.advanced(delta).map_err(MachineError::Clock)?;
        self.bus.tick_devices(delta).map_err(MachineError::Device)?;
        self.clock = next;
        Ok(())
    }
}

fn trap_cause(fault: &CpuFault<MemoryFault>) -> TrapCause {
    match &fault.cause {
        CpuFaultCause::Halted | CpuFaultCause::OperandLayout => TrapCause::IllegalInstruction,
        CpuFaultCause::Decode(source) => decode_cause(source),
        CpuFaultCause::Instruction(source) => instruction_cause(source),
        CpuFaultCause::PrivilegeViolation => TrapCause::PrivilegeViolation,
        CpuFaultCause::Control(source) => control_cause(source),
        CpuFaultCause::Width(source) => width_cause(source),
        CpuFaultCause::NextPc(source) => outcome_cause(source),
        CpuFaultCause::Outcome(source) => outcome_cause(&source.kind),
        CpuFaultCause::DataAccess(source) => data_access_cause(source),
        CpuFaultCause::Fetch(source) | CpuFaultCause::Memory { source, .. } => {
            memory_fault_cause(source)
        }
        CpuFaultCause::DoubleTrap
        | CpuFaultCause::DeferredInterrupt
        | CpuFaultCause::TerminalTrap
        | CpuFaultCause::TrapEntry(_) => TrapCause::InvalidControlState,
    }
}

fn decode_cause(error: &DecodeError) -> TrapCause {
    match error {
        DecodeError::Validation { source, .. } => instruction_cause(source),
        _ => TrapCause::IllegalInstruction,
    }
}

fn instruction_cause(error: &InstructionError) -> TrapCause {
    match error.source {
        ValidationError::InvalidWidth { .. } => TrapCause::InvalidWidth,
        _ => TrapCause::IllegalInstruction,
    }
}

fn outcome_cause(error: &OutcomeErrorKind) -> TrapCause {
    match error {
        OutcomeErrorKind::Halted => TrapCause::IllegalInstruction,
        OutcomeErrorKind::PrivilegeViolation => TrapCause::PrivilegeViolation,
        OutcomeErrorKind::Control(source) => control_cause(source),
        OutcomeErrorKind::Width(source) => width_cause(source),
    }
}

fn control_cause(error: &ControlStateError) -> TrapCause {
    match error {
        ControlStateError::Range { source, .. } => width_cause(source),
        ControlStateError::Alignment { .. } => TrapCause::Alignment,
        ControlStateError::Status(_) => TrapCause::InvalidStatus,
        ControlStateError::InvalidControlState { .. } => TrapCause::InvalidControlState,
    }
}

fn data_access_cause(error: &DataAccessError) -> TrapCause {
    match error {
        DataAccessError::InvalidWidth { .. } => TrapCause::InvalidWidth,
        DataAccessError::Width(source) => width_cause(source),
        DataAccessError::Alignment { .. } => TrapCause::Alignment,
    }
}

fn width_cause(error: &WidthError) -> TrapCause {
    match error {
        WidthError::DivisionByZero { .. } => TrapCause::DivideByZero,
        WidthError::SignedDivisionOverflow { .. } => TrapCause::DivisionOverflow,
        WidthError::InvalidSourceWidth { .. } | WidthError::ZeroAccessSize { .. } => {
            TrapCause::InvalidWidth
        }
        WidthError::AddressOutOfRange { .. }
        | WidthError::InvalidOffsetBase { .. }
        | WidthError::AddressOffsetOutOfRange { .. }
        | WidthError::InvalidAccessBase { .. }
        | WidthError::AccessEndOutOfRange { .. } => TrapCause::AddressOverflow,
    }
}

fn memory_fault_cause(fault: &MemoryFault) -> TrapCause {
    match &fault.kind {
        MemoryFaultKind::Device(_)
        | MemoryFaultKind::DeviceAlreadyMapped(_)
        | MemoryFaultKind::WritableRom { .. } => TrapCause::DeviceAccess,
        MemoryFaultKind::Width(source) => width_cause(source),
        MemoryFaultKind::InvalidDataAccess { source, .. } => data_access_cause(source),
        MemoryFaultKind::Control(source) => control_cause(source),
        MemoryFaultKind::Permission { .. }
        | MemoryFaultKind::AccessPolicy
        | MemoryFaultKind::ReadOnly { .. }
        | MemoryFaultKind::StackRegion { .. } => TrapCause::Permission,
        MemoryFaultKind::Unmapped | MemoryFaultKind::CrossRegion { .. } => TrapCause::Unmapped,
        MemoryFaultKind::StackSize => TrapCause::InvalidWidth,
        MemoryFaultKind::Configuration { .. }
        | MemoryFaultKind::HostSize(_)
        | MemoryFaultKind::Allocation(_)
        | MemoryFaultKind::Overlap { .. } => TrapCause::InvalidControlState,
    }
}

fn sign_extended_payload(config: ArchitectureConfig, payload: i32) -> u64 {
    match config.word_width() {
        lazalith_types::WordWidth::W32 => u64::from(payload as u32),
        lazalith_types::WordWidth::W64 => payload as i64 as u64,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineRun {
    pub executed: u64,
    pub halted_at: Option<u64>,
    pub trap: Option<TrapEvent>,
}
