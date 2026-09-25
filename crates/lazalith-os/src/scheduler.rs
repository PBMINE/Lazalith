use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};

use lazalith_cpu::{ExecutionContextId, Privilege, SyscallCompletion, TrapCause};
use lazalith_devices::Device;
use lazalith_machine::{LazalithMachine, MachineError, MachineEvent};
use lazalith_types::ArchitectureConfig;

use crate::{
    DispatchOutcome, KernelService, MemoryError, Process, ProcessExecutionError, ProcessId,
    ProcessState, ProcessStateError, SyscallDispatcher, SyscallRequest, SyscallRequestError,
    ThreadError, ThreadId, USER_CODE_LENGTH, USER_DATA_LENGTH, USER_STACK_LENGTH,
    UserMemoryContext, UserMemoryError,
};

const DEFAULT_USER_MEMORY_BUDGET: u64 =
    2 * (USER_CODE_LENGTH + USER_DATA_LENGTH + USER_STACK_LENGTH);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActiveThread {
    pub process_id: ProcessId,
    pub thread_id: ThreadId,
    pub remaining: u64,
    pub execution_context: ExecutionContextId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerStep {
    pub process_id: ProcessId,
    pub event: MachineEvent,
    pub switched_to: Option<ProcessId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerRun {
    pub executed: u64,
    pub last: Option<SchedulerStep>,
    pub active: Option<ActiveThread>,
}

#[derive(Debug)]
pub enum SchedulerError {
    ZeroQuantum,
    DuplicateProcess(ProcessId),
    ProcessNotFound(ProcessId),
    ProcessStateMismatch {
        process_id: ProcessId,
        expected: ProcessState,
        actual: ProcessState,
    },
    ProcessNotTerminal(ProcessId),
    ProcessConfiguration {
        expected: ArchitectureConfig,
        actual: ArchitectureConfig,
    },
    ProcessState(ProcessStateError),
    ProcessExecution(ProcessExecutionError),
    Thread(ThreadError),
    Memory(MemoryError),
    Context(UserMemoryError),
    Request(SyscallRequestError),
    Machine(MachineError),
    NoRunnableProcess,
    ProcessNotBlocked(ProcessId),
    ActiveProcess(ProcessId),
    Poisoned,
    RecoveryRequired,
    NotPoisoned,
    ContextExhausted,
    MemoryBudgetExceeded {
        requested: u64,
        remaining: u64,
    },
    MemoryAccountingOverflow,
    Allocation(TryReserveError),
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroQuantum => f.write_str("scheduler quantum must be nonzero"),
            Self::DuplicateProcess(id) => {
                write!(f, "process {} is already in the scheduler", id.get())
            }
            Self::ProcessNotFound(id) => write!(f, "process {} is not in the scheduler", id.get()),
            Self::ProcessStateMismatch {
                process_id,
                expected,
                actual,
            } => write!(
                f,
                "process {} is {actual:?}, expected {expected:?}",
                process_id.get()
            ),
            Self::ProcessNotTerminal(id) => {
                write!(f, "process {} is not terminal", id.get())
            }
            Self::ProcessConfiguration { expected, actual } => {
                write!(f, "scheduler uses {expected:?}, process uses {actual:?}")
            }
            Self::ProcessState(source) => source.fmt(f),
            Self::ProcessExecution(source) => source.fmt(f),
            Self::Thread(source) => source.fmt(f),
            Self::Memory(source) => source.fmt(f),
            Self::Context(source) => source.fmt(f),
            Self::Request(source) => source.fmt(f),
            Self::Machine(source) => source.fmt(f),
            Self::NoRunnableProcess => f.write_str("scheduler has no runnable process"),
            Self::ProcessNotBlocked(id) => {
                write!(f, "process {} is not Blocked", id.get())
            }
            Self::ActiveProcess(id) => {
                write!(f, "process {} is currently active", id.get())
            }
            Self::Poisoned => f.write_str("scheduler requires coordinated machine reset"),
            Self::RecoveryRequired => {
                f.write_str("scheduler recovery requires restoring the active User space")
            }
            Self::NotPoisoned => f.write_str("scheduler is not poisoned"),
            Self::ContextExhausted => f.write_str("scheduler execution context IDs exhausted"),
            Self::MemoryBudgetExceeded {
                requested,
                remaining,
            } => write!(
                f,
                "scheduler memory budget cannot reserve {requested} bytes with {remaining} bytes remaining"
            ),
            Self::MemoryAccountingOverflow => f.write_str("scheduler memory accounting overflowed"),
            Self::Allocation(source) => write!(f, "scheduler process allocation failed: {source}"),
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProcessState(source) => Some(source),
            Self::ProcessExecution(source) => Some(source),
            Self::Thread(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::Context(source) => Some(source),
            Self::Request(source) => Some(source),
            Self::Machine(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

pub struct RoundRobinScheduler {
    processes: Vec<Process>,
    config: Option<ArchitectureConfig>,
    quantum: u64,
    cursor: usize,
    current: Option<ActiveThread>,
    next_context: u64,
    memory_budget: Option<u64>,
    used_memory: u64,
    poisoned: bool,
    recovery_required: bool,
}

impl RoundRobinScheduler {
    pub fn new(quantum: u64) -> Result<Self, SchedulerError> {
        Self::with_memory_budget(quantum, Some(DEFAULT_USER_MEMORY_BUDGET))
    }

    pub fn with_memory_budget(
        quantum: u64,
        memory_budget: Option<u64>,
    ) -> Result<Self, SchedulerError> {
        if quantum == 0 {
            return Err(SchedulerError::ZeroQuantum);
        }
        Ok(Self {
            processes: Vec::new(),
            config: None,
            quantum,
            cursor: 0,
            current: None,
            next_context: 1,
            memory_budget,
            used_memory: 0,
            poisoned: false,
            recovery_required: false,
        })
    }

    pub fn quantum(&self) -> u64 {
        self.quantum
    }

    pub const fn memory_budget(&self) -> Option<u64> {
        self.memory_budget
    }

    pub const fn used_memory(&self) -> u64 {
        self.used_memory
    }

    pub const fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub const fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    pub fn processes(&self) -> &[Process] {
        &self.processes
    }

    pub fn active(&self) -> Option<ActiveThread> {
        self.current
    }

    pub fn add_process(&mut self, mut process: Process) -> Result<(), SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        if self
            .processes
            .iter()
            .any(|existing| existing.id() == process.id())
        {
            return Err(SchedulerError::DuplicateProcess(process.id()));
        }
        if let Some(context) = process.execution_context() {
            return Err(SchedulerError::ProcessExecution(
                ProcessExecutionError::AlreadyBound {
                    process_id: process.id(),
                    context,
                },
            ));
        }
        if !matches!(
            process.state(),
            ProcessState::Created | ProcessState::Ready | ProcessState::Blocked
        ) {
            return Err(SchedulerError::ProcessStateMismatch {
                process_id: process.id(),
                expected: ProcessState::Ready,
                actual: process.state(),
            });
        }
        if let Some(config) = self.config
            && process.memory().config() != config
        {
            return Err(SchedulerError::ProcessConfiguration {
                expected: config,
                actual: process.memory().config(),
            });
        }
        let resident_bytes = process.resident_bytes();
        let next_used = self
            .used_memory
            .checked_add(resident_bytes)
            .ok_or(SchedulerError::MemoryAccountingOverflow)?;
        if let Some(budget) = self.memory_budget {
            let remaining = budget.saturating_sub(self.used_memory);
            if resident_bytes > remaining {
                return Err(SchedulerError::MemoryBudgetExceeded {
                    requested: resident_bytes,
                    remaining,
                });
            }
            if next_used > budget {
                return Err(SchedulerError::MemoryBudgetExceeded {
                    requested: resident_bytes,
                    remaining: budget.saturating_sub(self.used_memory),
                });
            }
        }
        self.processes
            .try_reserve(1)
            .map_err(SchedulerError::Allocation)?;
        if process.state() == ProcessState::Created {
            process.mark_ready().map_err(SchedulerError::ProcessState)?;
        }
        if self.config.is_none() {
            self.config = Some(process.memory().config());
        }
        self.processes.push(process);
        self.used_memory = next_used;
        Ok(())
    }

    pub fn process(&self, id: ProcessId) -> Option<&Process> {
        self.processes.iter().find(|process| process.id() == id)
    }

    pub fn remove_process(&mut self, id: ProcessId) -> Result<Process, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        let index = self.process_index(id)?;
        if self.current.is_some_and(|active| active.process_id == id) {
            return Err(SchedulerError::ActiveProcess(id));
        }
        if !self.processes[index].state().is_terminal() {
            return Err(SchedulerError::ProcessNotTerminal(id));
        }
        let resident_bytes = self.processes[index].resident_bytes();
        let next_used = self
            .used_memory
            .checked_sub(resident_bytes)
            .ok_or(SchedulerError::MemoryAccountingOverflow)?;
        let process = self.processes.remove(index);
        self.used_memory = next_used;
        if self.processes.is_empty() {
            self.cursor = 0;
        } else {
            if index < self.cursor {
                self.cursor -= 1;
            }
            self.cursor %= self.processes.len();
        }
        Ok(process)
    }

    pub fn with_active_memory_context<R, D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        operation: impl FnOnce(&mut UserMemoryContext<'_>) -> R,
    ) -> Result<R, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        let (active, index) = self.active_index()?;
        if machine.trap_controller().has_active_frame() {
            self.validate_active_syscall(machine)?;
        } else {
            self.validate_active_user(machine)?;
        }
        machine.with_user_context(|_, _, space| {
            let mut context = self.processes[index]
                .memory_context_for_thread_in_space(active.thread_id, space)
                .map_err(SchedulerError::Context)?;
            Ok(operation(&mut context))
        })
    }

    fn capture_syscall<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<SyscallRequest, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        let (active, index) = self.active_index()?;
        self.validate_active_syscall(machine)?;
        let current = machine.architectural_state().clone();
        let identity = machine.memory().identity().clone();
        let process = &self.processes[index];
        machine
            .with_trap_controller_mut(Some(active.execution_context), |controller| {
                SyscallRequest::from_trap(
                    controller,
                    &current,
                    process,
                    active.thread_id,
                    &identity,
                )
            })
            .map_err(SchedulerError::Machine)?
            .map_err(SchedulerError::Request)
    }

    pub fn dispatch_syscall<S: KernelService + ?Sized>(
        &mut self,
        machine: &mut LazalithMachine<impl lazalith_devices::Device>,
        service: &mut S,
    ) -> Result<DispatchOutcome, SchedulerError> {
        let request = self.capture_syscall(machine)?;
        let (active, index) = self.active_index()?;
        let result: Result<DispatchOutcome, SchedulerError> =
            machine.with_user_context(|controller, current, space| {
                let mut context = self.processes[index]
                    .memory_context_for_thread_in_space(active.thread_id, space)
                    .map_err(SchedulerError::Context)?;
                Ok(SyscallDispatcher::new().dispatch(
                    request,
                    controller,
                    current,
                    &mut context,
                    service,
                ))
            });
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                self.poison_after_machine_error(machine);
                return Err(error);
            }
        };
        match outcome {
            DispatchOutcome::Return { .. } => Ok(outcome),
            DispatchOutcome::Exit { exit_code } => {
                self.finish_nonreturning_syscall(machine, Some(exit_code))?;
                Ok(outcome)
            }
            DispatchOutcome::Fault(error) => {
                self.finish_nonreturning_syscall(machine, None)?;
                Ok(DispatchOutcome::Fault(error))
            }
        }
    }

    fn finish_nonreturning_syscall<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        exit_code: Option<u32>,
    ) -> Result<(), SchedulerError> {
        if machine.trap_controller().has_active_frame() && machine.abort_trap().is_err() {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Poisoned);
        }
        let effective_exit_code = if exit_code.is_some() {
            exit_code
        } else {
            self.current
                .and_then(|active| self.process_index(active.process_id).ok())
                .and_then(|index| {
                    (self.processes[index].state() == ProcessState::Exited)
                        .then(|| self.processes[index].exit_code())
                        .flatten()
                })
        };
        self.release_current(machine, effective_exit_code)
    }

    fn next_index(&self) -> Option<usize> {
        if self.processes.is_empty() {
            return None;
        }
        for offset in 0..self.processes.len() {
            let index = (self.cursor + offset) % self.processes.len();
            if self.processes[index].state() == ProcessState::Ready {
                return Some(index);
            }
        }
        None
    }

    fn next_context(&mut self) -> Result<ExecutionContextId, SchedulerError> {
        let value = self.next_context;
        self.next_context = self
            .next_context
            .checked_add(1)
            .ok_or(SchedulerError::ContextExhausted)?;
        ExecutionContextId::new(value).ok_or(SchedulerError::ContextExhausted)
    }

    fn process_index(&self, id: ProcessId) -> Result<usize, SchedulerError> {
        self.processes
            .iter()
            .position(|process| process.id() == id)
            .ok_or(SchedulerError::ProcessNotFound(id))
    }

    fn active_index(&self) -> Result<(ActiveThread, usize), SchedulerError> {
        let active = self.current.ok_or(SchedulerError::NoRunnableProcess)?;
        let index = self.process_index(active.process_id)?;
        Ok((active, index))
    }

    fn validate_active_binding<D: Device>(
        &self,
        machine: &LazalithMachine<D>,
        active: ActiveThread,
        index: usize,
        expected_state: ProcessState,
        frame: bool,
        privilege: Option<Privilege>,
    ) -> Result<(), SchedulerError> {
        let process = &self.processes[index];
        if process.id() != active.process_id {
            return Err(SchedulerError::ProcessExecution(
                ProcessExecutionError::ContextMismatch {
                    expected: active.execution_context,
                    actual: process
                        .execution_context()
                        .unwrap_or(active.execution_context),
                },
            ));
        }
        if process.state() != expected_state {
            return Err(SchedulerError::ProcessStateMismatch {
                process_id: process.id(),
                expected: expected_state,
                actual: process.state(),
            });
        }
        if process.execution_context() != Some(active.execution_context)
            || machine.active_execution_context() != Some(active.execution_context)
        {
            return Err(SchedulerError::ProcessExecution(
                ProcessExecutionError::ContextMismatch {
                    expected: active.execution_context,
                    actual: process
                        .execution_context()
                        .or(machine.active_execution_context())
                        .unwrap_or(active.execution_context),
                },
            ));
        }
        if machine.trap_controller().is_terminal() {
            return Err(SchedulerError::Machine(MachineError::InvalidUserContext {
                reason: "machine trap controller is terminal",
            }));
        }
        if machine.trap_controller().has_active_frame() != frame {
            return Err(SchedulerError::Machine(MachineError::InvalidUserContext {
                reason: "trap frame presence does not match scheduler boundary",
            }));
        }
        if privilege.is_some_and(|expected| machine.architectural_state().privilege() != expected) {
            return Err(SchedulerError::Machine(MachineError::InvalidUserContext {
                reason: "scheduler privilege boundary does not match machine state",
            }));
        }
        if process.memory().config() != machine.memory().config() {
            return Err(SchedulerError::ProcessConfiguration {
                expected: machine.memory().config(),
                actual: process.memory().config(),
            });
        }
        if machine.memory().identity() != &process.memory().identity() {
            return Err(SchedulerError::Context(UserMemoryError::IdentityMismatch {
                expected: process.memory().identity(),
                actual: machine.memory().identity().clone(),
            }));
        }
        Ok(())
    }

    fn validate_active_user<D: Device>(
        &self,
        machine: &LazalithMachine<D>,
    ) -> Result<(ActiveThread, usize), SchedulerError> {
        let (active, index) = self.active_index()?;
        self.validate_active_binding(
            machine,
            active,
            index,
            ProcessState::Running,
            false,
            Some(Privilege::User),
        )?;
        Ok((active, index))
    }

    fn validate_active_syscall<D: Device>(
        &self,
        machine: &LazalithMachine<D>,
    ) -> Result<(ActiveThread, usize), SchedulerError> {
        let (active, index) = self.active_index()?;
        self.validate_active_binding(
            machine,
            active,
            index,
            ProcessState::Running,
            true,
            Some(Privilege::Supervisor),
        )?;
        Ok((active, index))
    }

    fn activate_next<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<Option<ProcessId>, SchedulerError> {
        let Some(index) = self.next_index() else {
            return Ok(None);
        };
        let process_id = self.processes[index].id();
        let context = self.next_context()?;
        let process = &mut self.processes[index];
        process
            .mark_running()
            .map_err(SchedulerError::ProcessState)?;
        process
            .activate(context)
            .map_err(SchedulerError::ProcessExecution)?;
        let thread_id = process.primary_thread().id();
        let cpu = process.thread_cpu(thread_id).ok_or(SchedulerError::Thread(
            ThreadError::UnknownThread(thread_id),
        ))?;
        if let Err(error) =
            machine.activate_user_context(process.memory_mut().address_space_mut(), cpu, context)
        {
            let _ = process.deactivate(context);
            let _ = process.preempt();
            return Err(SchedulerError::Machine(error));
        }
        if machine.memory().identity() != &process.memory().identity() {
            let _ = machine.recover_user_context(process.memory_mut().address_space_mut(), context);
            let _ = process.deactivate(context);
            let _ = process.preempt();
            return Err(SchedulerError::Machine(MachineError::InvalidUserContext {
                reason: "activated address-space identity does not match process",
            }));
        }
        self.cursor = (index + 1) % self.processes.len();
        self.current = Some(ActiveThread {
            process_id,
            thread_id,
            remaining: self.quantum,
            execution_context: context,
        });
        Ok(Some(process_id))
    }

    fn release_current<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        exit_code: Option<u32>,
    ) -> Result<(), SchedulerError> {
        let Some(active) = self.current else {
            return Ok(());
        };
        let index = self.process_index(active.process_id)?;
        let state = self.processes[index].state();
        self.validate_active_binding(machine, active, index, state, false, None)?;
        self.processes[index]
            .validate_release(exit_code)
            .map_err(SchedulerError::ProcessState)?;
        if let Err(error) = machine.release_user_context(
            self.processes[index].memory_mut().address_space_mut(),
            active.execution_context,
        ) {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Machine(error));
        }
        let process = &mut self.processes[index];
        process.apply_release(exit_code);
        process.clear_execution_context();
        self.current = None;
        Ok(())
    }

    fn yield_current<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<Option<ProcessId>, SchedulerError> {
        let Some(active) = self.current else {
            return Ok(None);
        };
        let index = self.process_index(active.process_id)?;
        self.validate_active_user(machine)?;
        self.processes[index]
            .save_thread_cpu(active.thread_id, machine.architectural_state().clone())
            .map_err(SchedulerError::Thread)?;
        if let Err(error) = machine.release_user_context(
            self.processes[index].memory_mut().address_space_mut(),
            active.execution_context,
        ) {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Machine(error));
        }
        let process = &mut self.processes[index];
        process.preempt_after_release();
        process.clear_execution_context();
        self.current = None;
        self.cursor = (index + 1) % self.processes.len();
        if self
            .processes
            .iter()
            .enumerate()
            .any(|(candidate, process)| {
                candidate != index && process.state() == ProcessState::Ready
            })
        {
            self.activate_next(machine)
        } else {
            Ok(None)
        }
    }

    fn suspend_current<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<(), SchedulerError> {
        let Some(active) = self.current else {
            return Ok(());
        };
        let index = self.process_index(active.process_id)?;
        self.validate_active_binding(
            machine,
            active,
            index,
            ProcessState::Blocked,
            false,
            Some(Privilege::User),
        )?;
        self.processes[index]
            .save_thread_cpu(active.thread_id, machine.architectural_state().clone())
            .map_err(SchedulerError::Thread)?;
        if let Err(error) = machine.release_user_context(
            self.processes[index].memory_mut().address_space_mut(),
            active.execution_context,
        ) {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Machine(error));
        }
        self.processes[index].clear_execution_context();
        self.current = None;
        self.cursor = (index + 1) % self.processes.len();
        Ok(())
    }

    pub fn unblock(&mut self, process_id: ProcessId) -> Result<(), SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        if self
            .current
            .is_some_and(|active| active.process_id == process_id)
        {
            return Err(SchedulerError::ActiveProcess(process_id));
        }
        let index = self.process_index(process_id)?;
        if self.processes[index].state() != ProcessState::Blocked {
            return Err(SchedulerError::ProcessNotBlocked(process_id));
        }
        self.processes[index]
            .mark_ready()
            .map_err(SchedulerError::ProcessState)?;
        self.cursor = index;
        Ok(())
    }

    pub fn return_from_syscall<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        completion: SyscallCompletion,
    ) -> Result<SchedulerStep, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        let (active, index) = self.active_index()?;
        let state = self.processes[index].state();
        if !matches!(state, ProcessState::Running | ProcessState::Blocked)
            || self
                .validate_active_binding(
                    machine,
                    active,
                    index,
                    state,
                    true,
                    Some(Privilege::Supervisor),
                )
                .is_err()
        {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Poisoned);
        }
        let process_id = active.process_id;
        let event = match machine.return_from_syscall(completion) {
            Ok(event) => event,
            Err(error) => {
                self.poison_after_machine_error(machine);
                return Err(SchedulerError::Machine(error));
            }
        };
        if machine.trap_controller().has_active_frame() {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Poisoned);
        }
        if machine.architectural_state().privilege() != Privilege::User {
            self.poison_after_machine_error(machine);
            return Err(SchedulerError::Poisoned);
        }
        let switched_to = match self.processes[index].state() {
            ProcessState::Blocked => {
                self.suspend_current(machine)?;
                self.activate_next(machine)?
            }
            ProcessState::Running => self.yield_if_quantum_exhausted(machine)?,
            _ => {
                self.poison_after_machine_error(machine);
                return Err(SchedulerError::Poisoned);
            }
        };
        Ok(SchedulerStep {
            process_id,
            event,
            switched_to,
        })
    }

    pub fn terminate_current<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        exit_code: u32,
    ) -> Result<ProcessId, SchedulerError> {
        let Some(active) = self.current else {
            return Err(SchedulerError::NoRunnableProcess);
        };
        self.release_current(machine, Some(exit_code))?;
        Ok(active.process_id)
    }

    fn has_other_ready(&self, index: usize) -> bool {
        self.processes
            .iter()
            .enumerate()
            .any(|(candidate, process)| {
                candidate != index && process.state() == ProcessState::Ready
            })
    }

    fn expire_quantum<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<Option<ProcessId>, SchedulerError> {
        let remaining = self
            .current
            .ok_or(SchedulerError::NoRunnableProcess)?
            .remaining;
        if remaining <= 1 {
            let index = self.process_index(
                self.current
                    .ok_or(SchedulerError::NoRunnableProcess)?
                    .process_id,
            )?;
            if self.has_other_ready(index) {
                self.yield_current(machine)
            } else {
                self.current
                    .as_mut()
                    .ok_or(SchedulerError::NoRunnableProcess)?
                    .remaining = self.quantum;
                Ok(None)
            }
        } else {
            self.current
                .as_mut()
                .ok_or(SchedulerError::NoRunnableProcess)?
                .remaining = remaining - 1;
            Ok(None)
        }
    }

    fn charge_user_trap(&mut self) -> Result<(), SchedulerError> {
        let current = self
            .current
            .as_mut()
            .ok_or(SchedulerError::NoRunnableProcess)?;
        current.remaining = current.remaining.saturating_sub(1);
        Ok(())
    }

    fn yield_if_quantum_exhausted<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<Option<ProcessId>, SchedulerError> {
        if self.current.is_some_and(|active| active.remaining == 0) {
            let index = self.process_index(
                self.current
                    .ok_or(SchedulerError::NoRunnableProcess)?
                    .process_id,
            )?;
            if self.has_other_ready(index) {
                self.yield_current(machine)
            } else {
                self.current
                    .as_mut()
                    .ok_or(SchedulerError::NoRunnableProcess)?
                    .remaining = self.quantum;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    fn poison_after_machine_error<D: Device>(&mut self, machine: &mut LazalithMachine<D>) {
        self.poisoned = true;
        self.recovery_required = false;
        let Some(active) = self.current.take() else {
            self.recovery_required = machine.active_execution_context().is_some();
            return;
        };
        if let Ok(index) = self.process_index(active.process_id) {
            let _ = self.processes[index].fault();
            self.processes[index].clear_execution_context();
            if machine.memory().identity() == &self.processes[index].memory().identity() {
                let _ = machine.abort_trap();
                if machine
                    .recover_user_context(
                        self.processes[index].memory_mut().address_space_mut(),
                        active.execution_context,
                    )
                    .is_err()
                {
                    self.recovery_required = true;
                }
            } else {
                let _ = machine.invalidate_user_context(active.execution_context);
                self.recovery_required = true;
            }
        } else {
            let _ = machine.invalidate_user_context(active.execution_context);
            self.recovery_required = true;
        }
    }

    pub fn reset_after_poison<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<(), SchedulerError> {
        if !self.poisoned {
            return Err(SchedulerError::NotPoisoned);
        }
        machine.reset();
        if self.recovery_required {
            return Err(SchedulerError::RecoveryRequired);
        }
        self.current = None;
        self.poisoned = false;
        self.recovery_required = false;
        self.cursor = 0;
        Ok(())
    }

    pub fn step<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<SchedulerStep, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        if self.current.is_none() && self.activate_next(machine)?.is_none() {
            return Err(SchedulerError::NoRunnableProcess);
        }
        if let Some(active) = self.current {
            let index = self.process_index(active.process_id)?;
            if self.processes[index].state() == ProcessState::Blocked {
                if machine.trap_controller().has_active_frame() {
                    self.poison_after_machine_error(machine);
                    return Err(SchedulerError::Poisoned);
                }
                self.suspend_current(machine)?;
                if self.activate_next(machine)?.is_none() {
                    return Err(SchedulerError::NoRunnableProcess);
                }
            }
        }
        let (active, index) = self.active_index()?;
        let validation = if machine.trap_controller().has_active_frame() {
            self.validate_active_syscall(machine)
        } else {
            self.validate_active_user(machine)
        };
        if let Err(error) = validation {
            self.poison_after_machine_error(machine);
            return Err(error);
        }
        let process_id = active.process_id;
        let had_frame = machine.trap_controller().has_active_frame();
        let event = match machine.step_managed(active.execution_context) {
            Ok(event) => event,
            Err(error) => {
                self.poison_after_machine_error(machine);
                return Err(SchedulerError::Machine(error));
            }
        };
        let switched_to = match event {
            MachineEvent::Halted => {
                if machine.architectural_state().privilege() != Privilege::User
                    || machine.trap_controller().has_active_frame()
                {
                    self.poison_after_machine_error(machine);
                    return Err(SchedulerError::Poisoned);
                }
                self.release_current(machine, Some(0))?;
                None
            }
            MachineEvent::Trapped { event } => {
                if matches!(
                    event.cause,
                    TrapCause::Syscall | TrapCause::SoftwareTrap | TrapCause::ExternalInterrupt
                ) {
                    if let Err(error) = self.validate_active_binding(
                        machine,
                        active,
                        index,
                        ProcessState::Running,
                        true,
                        Some(Privilege::Supervisor),
                    ) {
                        self.poison_after_machine_error(machine);
                        return Err(error);
                    }
                    if !had_frame
                        && matches!(event.cause, TrapCause::Syscall | TrapCause::SoftwareTrap)
                    {
                        self.charge_user_trap()?;
                    }
                    None
                } else {
                    if let Err(error) = machine.abort_trap() {
                        self.poison_after_machine_error(machine);
                        return Err(SchedulerError::Machine(error));
                    }
                    self.release_current(machine, None)?;
                    None
                }
            }
            MachineEvent::Stepped { .. } => {
                if had_frame {
                    if machine.trap_controller().has_active_frame() {
                        if let Err(error) = self.validate_active_binding(
                            machine,
                            active,
                            index,
                            ProcessState::Running,
                            true,
                            Some(Privilege::Supervisor),
                        ) {
                            self.poison_after_machine_error(machine);
                            return Err(error);
                        }
                        None
                    } else {
                        if let Err(error) = self.validate_active_user(machine) {
                            self.poison_after_machine_error(machine);
                            return Err(error);
                        }
                        self.yield_if_quantum_exhausted(machine)?
                    }
                } else {
                    self.expire_quantum(machine)?
                }
            }
        };
        Ok(SchedulerStep {
            process_id,
            event,
            switched_to,
        })
    }

    pub fn run<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
        limit: u64,
    ) -> Result<SchedulerRun, SchedulerError> {
        if self.poisoned {
            return Err(SchedulerError::Poisoned);
        }
        if let Some(active) = self.current {
            let index = self.process_index(active.process_id)?;
            if self.processes[index].state() == ProcessState::Blocked
                && !self.has_other_ready(index)
            {
                self.suspend_current(machine)?;
                return Ok(SchedulerRun {
                    executed: 0,
                    last: None,
                    active: self.current,
                });
            }
        }
        if limit == 0 || (self.current.is_none() && self.next_index().is_none()) {
            return Ok(SchedulerRun {
                executed: 0,
                last: None,
                active: self.current,
            });
        }
        let mut executed = 0;
        let mut last = None;
        while executed < limit {
            let step = self.step(machine)?;
            let counted = match step.event {
                MachineEvent::Stepped { .. } | MachineEvent::Halted => true,
                MachineEvent::Trapped { event } => {
                    matches!(event.cause, TrapCause::Syscall | TrapCause::SoftwareTrap)
                }
            };
            if counted {
                executed += 1;
            }
            let stop = matches!(step.event, MachineEvent::Trapped { .. })
                && machine.trap_controller().has_active_frame();
            last = Some(step);
            if stop || (self.current.is_none() && self.next_index().is_none()) {
                break;
            }
        }
        Ok(SchedulerRun {
            executed,
            last,
            active: self.current,
        })
    }
}
