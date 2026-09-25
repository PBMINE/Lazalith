use crate::{
    DispatchOutcome, FileSystemService, LzxArchitecture, LzxError, LzxImage, NativeShellImageError,
    ProcessError, ProcessId, RoundRobinScheduler, SchedulerError, SchedulerStep, TerminalService,
    ThreadId, VirtualFileSystem, VirtualTerminal, build_init_shell_image,
};
use core::{error::Error, fmt};
use lazalith_cpu::TrapCause;
use lazalith_devices::Device;
use lazalith_machine::{LazalithMachine, MachineEvent};
use lazalith_os_abi::{SyscallError, TaggedOutcome};

#[derive(Debug)]
pub enum KernelError {
    Image(NativeShellImageError),
    LzxImage(LzxError),
    Process(ProcessError),
    Scheduler(SchedulerError),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Image(source) => write!(f, "kernel init image failed: {source}"),
            Self::LzxImage(source) => write!(f, "kernel executable image failed: {source}"),
            Self::Process(source) => write!(f, "kernel process construction failed: {source}"),
            Self::Scheduler(source) => write!(f, "kernel scheduler failed: {source}"),
        }
    }
}

impl Error for KernelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Image(source) => Some(source),
            Self::LzxImage(source) => Some(source),
            Self::Process(source) => Some(source),
            Self::Scheduler(source) => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KernelServiceOutcome {
    Return(TaggedOutcome),
    Exit(u32),
    Fault(SyscallError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelStep {
    pub scheduler: SchedulerStep,
    pub outcome: Option<KernelServiceOutcome>,
}

pub struct LazalithKernel {
    scheduler: RoundRobinScheduler,
    terminal: TerminalService,
}

impl LazalithKernel {
    pub fn new(
        quantum: u64,
        terminal: VirtualTerminal,
        filesystem: VirtualFileSystem,
    ) -> Result<Self, KernelError> {
        let scheduler = RoundRobinScheduler::new(quantum).map_err(KernelError::Scheduler)?;
        let terminal = TerminalService::new(terminal, FileSystemService::new(filesystem));
        Ok(Self {
            scheduler,
            terminal,
        })
    }

    pub fn start_init_shell(
        &mut self,
        architecture: LzxArchitecture,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<(), KernelError> {
        let image = build_init_shell_image(architecture).map_err(KernelError::Image)?;
        let process = image
            .load_process(process_id, thread_id)
            .map_err(|source| KernelError::Image(NativeShellImageError::Image(source)))?;
        self.scheduler
            .add_process(process)
            .map_err(KernelError::Scheduler)
    }

    pub fn start_image(
        &mut self,
        image: LzxImage,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<(), KernelError> {
        let process = image
            .load_process(process_id, thread_id)
            .map_err(KernelError::LzxImage)?;
        self.scheduler
            .add_process(process)
            .map_err(KernelError::Scheduler)
    }

    pub fn step<D: Device>(
        &mut self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<KernelStep, KernelError> {
        let mut scheduler = self
            .scheduler
            .step(machine)
            .map_err(KernelError::Scheduler)?;
        let outcome = match scheduler.event {
            MachineEvent::Trapped { event } if event.cause == TrapCause::Syscall => {
                let outcome = self
                    .scheduler
                    .dispatch_syscall(machine, &mut self.terminal)
                    .map_err(KernelError::Scheduler)?;
                match outcome {
                    DispatchOutcome::Return {
                        outcome,
                        completion,
                    } => {
                        scheduler = self
                            .scheduler
                            .return_from_syscall(machine, completion)
                            .map_err(KernelError::Scheduler)?;
                        Some(KernelServiceOutcome::Return(outcome))
                    }
                    DispatchOutcome::Exit { exit_code } => {
                        Some(KernelServiceOutcome::Exit(exit_code))
                    }
                    DispatchOutcome::Fault(error) => Some(KernelServiceOutcome::Fault(error)),
                }
            }
            MachineEvent::Trapped { .. } | MachineEvent::Stepped { .. } | MachineEvent::Halted => {
                None
            }
        };
        Ok(KernelStep { scheduler, outcome })
    }

    pub const fn scheduler(&self) -> &RoundRobinScheduler {
        &self.scheduler
    }

    pub fn scheduler_mut(&mut self) -> &mut RoundRobinScheduler {
        &mut self.scheduler
    }

    pub const fn terminal(&self) -> &TerminalService {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut TerminalService {
        &mut self.terminal
    }
}
