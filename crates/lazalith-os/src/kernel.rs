use crate::display::DisplayService;
use crate::input::InputService;
use crate::{
    DispatchOutcome, FileSystemService, KernelService, LzxArchitecture, LzxError, LzxImage,
    NativeShellImageError, ProcessError, ProcessId, RoundRobinScheduler, SchedulerError,
    SchedulerStep, ServiceOutcome, TerminalService, ThreadId, UserMemoryContext, ValidatedSyscall,
    ValidatedSyscallKind, VirtualFileSystem, VirtualTerminal, build_init_shell_image,
};
use core::{error::Error, fmt};
use lazalith_cpu::TrapCause;
use lazalith_devices::Device;
use lazalith_machine::{LazalithMachine, MachineEvent};
use lazalith_os_abi::{SyscallError, TaggedOutcome};
use lazalith_types::ArchitectureConfig;

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
    /// The guest program trapped, and nothing handled it for it.
    ///
    /// A guest trap is not a syscall and not a kernel failure. It is the program
    /// executing a `TRAP` instruction, which is also what the compiler's bounds
    /// check and every runtime check compile down to. The kernel has no handler
    /// for one — the program is meant to be looking at it — so it reports one and
    /// leaves the machine in the trap frame, which is where a debugger stops and
    /// shows the guest's own program counter.
    ///
    /// This variant exists because dropping the event left a trapped program in
    /// limbo: the machine stayed in its trap frame, nothing could return from it,
    /// and the next run found no runnable process and reported the program as
    /// having *exited successfully*. A program whose bounds check fired was
    /// reported as a program that finished.
    GuestTrap {
        /// What kind of trap it was.
        cause: TrapCause,
        /// The payload the guest supplied, sign-extended from the instruction's
        /// immediate.
        payload: i64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelStep {
    pub scheduler: SchedulerStep,
    pub outcome: Option<KernelServiceOutcome>,
}

/// The kernel's services, together.
///
/// Dispatch needs *one* service to hand a validated syscall to, but a kernel has
/// more than one: the terminal owns the console and the filesystem, the display
/// driver owns the window, and the input driver owns the event queue. Routing
/// them through a composite keeps `dispatch`
/// unchanged and keeps each owner responsible for its own calls — a syscall
/// reaching the wrong owner is a routing bug that shows up immediately as an
/// "unknown syscall" rather than as a subtly wrong answer.
pub struct KernelServices {
    terminal: TerminalService,
    display: DisplayService,
    input: InputService,
}

impl KernelServices {
    /// The terminal service, for a caller that wants the console.
    pub const fn terminal(&self) -> &TerminalService {
        &self.terminal
    }

    /// The terminal service, mutably.
    pub fn terminal_mut(&mut self) -> &mut TerminalService {
        &mut self.terminal
    }

    /// The display driver, for a caller that wants the window.
    pub const fn display(&self) -> &DisplayService {
        &self.display
    }

    /// The display driver, mutably.
    pub fn display_mut(&mut self) -> &mut DisplayService {
        &mut self.display
    }

    /// The input driver, for a host adapter that feeds a script.
    pub const fn input(&self) -> &InputService {
        &self.input
    }

    /// The input driver, mutably, for a host adapter that feeds a script.
    pub fn input_mut(&mut self) -> &mut InputService {
        &mut self.input
    }
}

impl KernelService for KernelServices {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::DisplayOpen { .. }
            | ValidatedSyscallKind::DisplayPresent { .. } => self.display.invoke(syscall, memory),
            ValidatedSyscallKind::InputPoll { .. } => self.input.invoke(syscall, memory),
            _ => self.terminal.invoke(syscall, memory),
        }
    }
}

pub struct LazalithKernel {
    scheduler: RoundRobinScheduler,
    services: KernelServices,
}

impl LazalithKernel {
    pub fn new(
        quantum: u64,
        terminal: VirtualTerminal,
        filesystem: VirtualFileSystem,
    ) -> Result<Self, KernelError> {
        Self::with_architecture(quantum, terminal, filesystem, ArchitectureConfig::lz64())
    }

    /// A kernel whose services target `architecture`.
    ///
    /// The display driver's records are sized and word-width checked against the
    /// architecture, so a kernel that has to dispatch a display call needs to know
    /// which one. It defaults to LZ64 because that is the only target the Lazen
    /// pipeline generates today, and a kernel with no display traffic is
    /// unaffected either way.
    pub fn with_architecture(
        quantum: u64,
        terminal: VirtualTerminal,
        filesystem: VirtualFileSystem,
        architecture: ArchitectureConfig,
    ) -> Result<Self, KernelError> {
        let scheduler = RoundRobinScheduler::new(quantum).map_err(KernelError::Scheduler)?;
        Ok(Self {
            scheduler,
            services: KernelServices {
                terminal: TerminalService::new(terminal, FileSystemService::new(filesystem)),
                display: DisplayService::new(architecture),
                input: InputService::new(),
            },
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
                    .dispatch_syscall(machine, &mut self.services)
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
            // Every other trap is the guest's own, and is reported rather than
            // dropped. Dropping it left the machine in a trap frame with nothing
            // able to leave it, which a caller could only see as the program
            // having finished.
            MachineEvent::Trapped { event } => Some(KernelServiceOutcome::GuestTrap {
                cause: event.cause,
                payload: event.payload as i64,
            }),
            MachineEvent::Stepped { .. } | MachineEvent::Halted => None,
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
        self.services.terminal()
    }

    pub fn terminal_mut(&mut self) -> &mut TerminalService {
        self.services.terminal_mut()
    }

    /// The display driver, for a host frontend that wants the presented frame.
    pub const fn display(&self) -> &DisplayService {
        self.services.display()
    }

    /// The display driver, mutably, for a host that sets a window up directly.
    pub fn display_mut(&mut self) -> &mut DisplayService {
        self.services.display_mut()
    }

    /// The input driver, for a host adapter that feeds a script of events.
    pub const fn input(&self) -> &InputService {
        self.services.input()
    }

    /// The input driver, mutably, for a host adapter that feeds a script.
    ///
    /// A Lazen program cannot reach this: the SDK's `poll` goes through
    /// `input_poll`. A graphical host queues its events here, and a test queues
    /// a script here, and the program cannot tell the two apart.
    pub fn input_mut(&mut self) -> &mut InputService {
        self.services.input_mut()
    }
}
