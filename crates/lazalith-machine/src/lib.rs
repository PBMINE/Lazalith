#![no_std]

extern crate alloc;

mod interrupts;
pub use interrupts::InterruptController;
mod profile;
pub use profile::{
    BlockStorage, Compatibility, DeviceClass, DeviceProfile, LZA64_LAYOUT, MachineLayout,
    MachineProfile, ProfileError, ProfileFamily, ProfileName, ProfiledMachine, RegionProfile,
};

use alloc::{boxed::Box, collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_cpu::{
    ArchitecturalState, ControlStateError, CpuFault, CpuFaultCause, DataAccessError, EngineError,
    EngineKind, ExecutionContextId, ExecutionEngine, FaultOrigin, OutcomeApplication,
    OutcomeErrorKind, Processor, ReferenceInterpreter, SyscallCompletion, TrapAttempt, TrapCause,
    TrapController, TrapRequest,
};
use lazalith_devices::{Device, DeviceId, DeviceManager};
use lazalith_diagnostics::bug::{EmulatorBug, Subsystem};
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

fn is_executable_state(state: MachineState) -> bool {
    matches!(
        state,
        MachineState::Reset | MachineState::Running | MachineState::Paused
    )
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
    /// The execution-engine boundary refused: an engine this build does not have,
    /// or a switch at a moment when changing engines is not a thing a machine may
    /// do. See [`LazalithMachine::switch_execution_engine`].
    Engine(EngineError),
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
            Self::TrapEntryContextMissing => {
                f.write_str("trap entry requires a valid execution context")
            }
            Self::InterruptAllocation(source) => {
                write!(f, "interrupt request allocation failed: {source}")
            }
            Self::Clock(source) => write!(f, "virtual clock rejected the operation: {source}"),
            Self::Device(source) => write!(f, "device rejected the operation: {source}"),
            Self::Memory(source) => write!(f, "memory rejected the operation: {source}"),
            Self::Cpu(source) => write!(f, "CPU rejected the operation: {source}"),
            Self::Engine(source) => write!(f, "execution engine: {source}"),
            Self::InstructionCountOverflow => f.write_str("executed instruction count overflowed"),
            Self::InitialClock { devices, requested } => write!(
                f,
                "initial clock {} precedes the device epoch {}",
                requested.as_u64(),
                devices.as_u64()
            ),
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
            Self::Engine(source) => Some(source),
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
    /// The canonical guest-visible processor state.
    ///
    /// The machine owns it, not an engine, and that ownership is the whole reason
    /// an engine can be changed without the guest seeing a machine reset: there is
    /// one copy of the program counter, the registers, the status register and the
    /// trap frames, and an engine that goes away takes nothing of it with it.
    processor: Processor,
    /// The engine that is currently executing LZA instructions.
    ///
    /// Whatever this holds beyond the instruction semantics — a translation cache,
    /// host register state — is private and discardable. The machine drops it on a
    /// switch through [`ExecutionEngine::discard_private_state`], and a guest must
    /// not be able to tell that it was dropped.
    engine: Box<dyn ExecutionEngine<Bus<D>>>,
    bus: Bus<D>,
    config: ArchitectureConfig,
    clock: VirtualClock,
    initial_state: ArchitecturalState,
    initial_time: CycleCount,
    state: MachineState,
    interrupts: InterruptController,
    last_trap_fault: Option<Box<CpuFault<MemoryFault>>>,
    /// The resume point of the last trap that was entered, or `None`.
    ///
    /// This is *not* the machine's program counter: a trapped machine is in the
    /// kernel's trap frame, so its `pc` is the trap vector. A caller that wants the
    /// *guest's address needs the point execution would resume at, and the
    /// instruction before that is the one that trapped.
    last_trap_resume_pc: Option<InstructionAddress>,
    /// The last fault that was the *emulator's own, reported, if any.
    ///
    /// A guest fault is the program's own mistake and is not kept here: it belongs to the
    /// guest, and the debugger reports it from the trap that carried it. This is only
    /// for a cause no guest program can reach, where the thing to report is a bug in
    /// this machine.
    ///
    /// A *report* and not the fault: the fault carries a borrowed error that a stored
    /// value cannot outlive, and the report is what a person reads anyway.
    last_emulator_bug: Option<Box<EmulatorBug>>,
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
        let processor = Processor::new(initial_state.clone());
        let mut bus = Bus::with_devices(space, devices);
        bus.tick_devices(delta).map_err(MachineError::Device)?;
        let clock = VirtualClock::at(initial_time);
        Ok(Self {
            processor,
            engine: Box::new(ReferenceInterpreter::new()),
            bus,
            config,
            clock,
            initial_state,
            initial_time,
            state: MachineState::Created,
            interrupts: InterruptController::new(),
            last_trap_fault: None,
            last_trap_resume_pc: None,
            last_emulator_bug: None,
            executed: 0,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    /// Which execution engine this machine is running LZA instructions on.
    ///
    /// The machine does not care which; a caller can ask, and the answer is a
    /// stable name rather than a type, so a debugger can report it and a profile
    /// can bucket by it.
    pub fn execution_engine(&self) -> EngineKind {
        self.engine.kind()
    }

    /// Changes which execution engine runs this machine, without resetting it.
    ///
    /// # What this is for
    ///
    /// The two directions a future JIT needs, and neither of them is a restart:
    ///
    /// ```text
    /// Interpreter ──▶ JIT   hot code identified
    /// JIT ──▶ Interpreter   breakpoint, fault, debug event, or an unsupported block
    /// ```
    ///
    /// Both are the same operation, because both are the same fact: the machine has
    /// one canonical architectural state and the engine is how that state is being
    /// advanced.
    ///
    /// # What it guarantees
    ///
    /// The [`Processor`] is the machine's, so it is not touched. Program counter,
    /// stack pointer, registers, status, the trap frame stack, memory, the device
    /// state, the virtual clock and the machine's own lifecycle are all exactly
    /// what they were. What *is* dropped is the outgoing engine's private state,
    /// through [`ExecutionEngine::discard_private_state`], because a translation
    /// cache describes compiled code and not a program.
    ///
    /// # When it is refused
    ///
    /// In two cases, both named in the error rather than implied:
    ///
    /// - while a user execution context is active, because the scheduler is
    ///   between two steps of a guest process and would not observe the change;
    /// - while the machine is [`MachineState::Faulted`], because a terminal machine
    ///   is over and reviving it through an engine switch is a reset wearing a
    ///   disguise.
    ///
    /// A switch **is** allowed while a trap frame is active, which is the case
    /// that matters: a fault in compiled code has to be able to hand the program
    /// back to the interpreter with the frame intact, or the program could never
    /// return from it.
    ///
    /// # Today
    ///
    /// One engine exists — [`EngineKind::Reference`] — so this currently replaces an
    /// engine with the same semantics and discards its private state. It is the
    /// operation, not the second engine, that this stage establishes; there is no
    /// JIT in this repository and this does not pretend otherwise.
    pub fn switch_execution_engine(&mut self, kind: EngineKind) -> Result<(), MachineError> {
        if !EngineKind::ALL.contains(&kind) {
            return Err(MachineError::Engine(EngineError::UnknownEngine(kind)));
        }
        if self.active_execution_context().is_some() {
            return Err(MachineError::Engine(EngineError::SwitchRefused {
                reason: "a user execution context is active, and the scheduler would not see the change",
            }));
        }
        if self.state == MachineState::Faulted {
            return Err(MachineError::Engine(EngineError::SwitchRefused {
                reason: "the machine is faulted, and an engine switch is not a reset",
            }));
        }
        self.engine.discard_private_state();
        self.engine = Box::new(engine_for(kind));
        Ok(())
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
        self.processor.architectural()
    }
    pub fn write_register(&mut self, index: RegisterIndex, value: u64) -> Result<(), MachineError> {
        if self.active_execution_context().is_some() || self.processor.traps().has_active_frame() {
            return Err(MachineError::InvalidUserContext {
                reason: "direct register mutation is not allowed during an active context",
            });
        }
        self.processor.write_register(index, value);
        Ok(())
    }

    fn write_register_unrestricted(&mut self, index: RegisterIndex, value: u64) {
        self.processor.write_register(index, value);
    }
    pub fn return_from_syscall(
        &mut self,
        completion: SyscallCompletion,
    ) -> Result<MachineEvent, MachineError> {
        self.ensure_executable(MachineOperation::Step)?;
        let frame = self
            .processor
            .traps()
            .frame()
            .ok_or(MachineError::InvalidSyscallReturn)?;
        if frame.cause() != TrapCause::Syscall
            || !self.processor.traps().completion_matches(&completion)
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
            .processor
            .traps_mut()
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
            self.processor
                .traps_mut()
                .clear_syscall_return_authorization();
        }
        result
    }
    pub fn with_user_context<R>(
        &mut self,
        operation: impl FnOnce(&TrapController, &ArchitecturalState, &mut dyn UserSpace) -> R,
    ) -> R {
        operation(
            self.processor.traps(),
            self.processor.architectural(),
            self.bus.user_address_space_mut(),
        )
    }

    pub fn release_user_context(
        &mut self,
        address_space: &mut AddressSpace,
        execution_context: ExecutionContextId,
    ) -> Result<(), MachineError> {
        if self.processor.traps().has_active_frame() {
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
            .processor
            .traps_mut()
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
        if !self
            .processor
            .traps_mut()
            .clear_execution_context(execution_context)
        {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context changed during context release",
            });
        }
        self.settle_after_context_release();
        Ok(())
    }

    fn settle_after_context_release(&mut self) {
        if !matches!(self.state, MachineState::Faulted) {
            self.state = MachineState::Halted;
        }
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
        if !self
            .processor
            .traps_mut()
            .clear_execution_context(execution_context)
        {
            return Err(MachineError::InvalidUserContext {
                reason: "execution context changed during context release",
            });
        }
        self.settle_after_context_release();
        Ok(())
    }

    pub fn active_execution_context(&self) -> Option<ExecutionContextId> {
        self.processor.traps().execution_context()
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
        if self.processor.traps().has_active_frame() || self.processor.traps().is_terminal() {
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
        let previous = self.processor.architectural().clone();
        let previous_execution = self.processor.execution();
        self.processor
            .replace_architectural(cpu)
            .map_err(MachineError::Cpu)?;
        if let Err(error) = self.bus.swap_user_address_space(address_space) {
            let _ = self
                .processor
                .restore_architectural_with_execution(previous, previous_execution);
            return Err(MachineError::AddressSpaceSwap(error));
        }
        self.last_trap_fault = None;
        self.last_trap_resume_pc = None;
        self.last_emulator_bug = None;
        self.processor
            .traps_mut()
            .set_execution_context(execution_context);
        self.state = MachineState::Running;
        Ok(())
    }

    pub fn abort_trap(&mut self) -> Result<(), MachineError> {
        if !self.processor.traps_mut().abort_frame() {
            return Err(MachineError::InvalidUserContext {
                reason: "no active trap frame to abort",
            });
        }
        Ok(())
    }

    pub fn devices(&self) -> &DeviceManager<D> {
        self.bus.devices()
    }

    /// The devices, mutably, for a caller restoring a machine snapshot.
    ///
    /// A device's registers are the guest's to read and write, so restoring them
    /// is restoring guest-visible state rather than reaching past the guest. What
    /// makes this safe is that a restore goes through
    /// [`Device::restore`](lazalith_devices::Device::restore), which checks that
    /// the bytes are the ones that device's own `snapshot` produces — so a
    /// caller cannot put a display's state into an input device, and cannot pad a
    /// snapshot into a shape the device would have refused.
    pub fn devices_mut(&mut self) -> &mut DeviceManager<D> {
        self.bus.devices_mut()
    }

    /// The processor, for a caller that has to save or restore it whole.
    ///
    /// This is the one accessor that hands out a mutable reference to the CPU, and
    /// it exists for exactly two callers: a machine snapshot, which has to capture
    /// the architectural state *and* the trap frame stack together because a
    /// program stopped in a syscall is neither resumable without them nor
    /// describable without them; and this crate's own tests.
    ///
    /// It is not a debugging convenience. A frontend that could reach the
    /// processor through this would be able to forge a trap frame and set the
    /// program counter, which is the thing the debug API deliberately does not
    /// offer — so nothing outside the workspace's own machine-level code uses it,
    /// and `lazalith-debug` reaches the CPU only through the machine's
    /// snapshot methods.
    pub fn processor(&self) -> &Processor {
        &self.processor
    }

    /// The processor, mutably. See [`Self::processor`].
    pub fn processor_mut(&mut self) -> &mut Processor {
        &mut self.processor
    }
    pub const fn memory(&self) -> &AddressSpace {
        self.bus.address_space()
    }
    pub const fn trap_controller(&self) -> &TrapController {
        self.processor.traps()
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
        Ok(operation(self.processor.traps_mut()))
    }
    pub const fn interrupts(&self) -> &InterruptController {
        &self.interrupts
    }

    /// The interrupt controller, mutably.
    ///
    /// **This is a snapshot restore's accessor, and that is the only intended use.**
    ///
    /// Every other mutation of the pending set goes through `raise_interrupt`, which is
    /// the machine's step and is where an interrupt is *delivered*. That distinction
    /// is the reason this accessor exists rather than being folded into the step: a
    /// restore has to put back the promises the machine made to itself — the
    /// interrupts it was about to take and had not — and that is not a delivery, so it
    /// must not go through the path that performs one. Making it a separate, mutable
    /// accessor keeps the fact that only a restore writes this set directly.
    pub const fn interrupts_mut(&mut self) -> &mut InterruptController {
        &mut self.interrupts
    }
    /// The resume point of the last trap that was entered, or `None` if none has.
    ///
    /// See the field for why this is not the machine's program counter.
    pub const fn last_trap_resume_pc(&self) -> Option<InstructionAddress> {
        self.last_trap_resume_pc
    }

    /// The last fault that was the emulator's own, or `None`.
    ///
    /// A fault whose cause is a guest's is deliberately not returned: a program that
    /// read an address it does not own is not an emulator bug, and a debugger that
    /// found one here would report every out-of-bounds read as a bug in Lazalith.
    pub fn last_emulator_bug(&self) -> Option<&EmulatorBug> {
        self.last_emulator_bug.as_deref()
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
        self.processor
            .set_trap_vector(target)
            .map_err(MachineError::Cpu)
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
        if self.active_execution_context().is_some() || self.processor.traps().has_active_frame() {
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
        self.processor = Processor::new(self.initial_state.clone());
        self.engine.discard_private_state();
        self.clock = VirtualClock::at(self.initial_time);
        self.interrupts.reset();
        self.last_trap_fault = None;
        self.state = MachineState::Reset;
    }

    fn ensure_executable(&self, operation: MachineOperation) -> Result<(), MachineError> {
        if is_executable_state(self.state) {
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
                self.last_trap_resume_pc = Some(resume_pc);
                Ok(TrapEvent {
                    cause,
                    payload,
                    resume_pc,
                    interrupt,
                })
            }
            Err(failure) => {
                self.state = MachineState::Faulted;
                // A fault that stopped the trap from being entered is recorded
                // before the attempt is looked for, because a trap that failed to
                // enter *and* failed to be described would leave a debugger with
                // nothing to report at all.
                if failure.origin() == FaultOrigin::Emulator {
                    self.last_emulator_bug = Some(Box::new(
                        EmulatorBug::with_site(
                            Subsystem::MACHINE,
                            "entering a trap",
                            failure.cause.invariant(),
                            failure.site,
                        )
                        .at(failure.pc.as_u64()),
                    ));
                }
                let attempt = self.processor.traps().failed_entry().cloned().or_else(|| {
                    self.processor
                        .traps()
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
                let result = self.processor.enter_fault(&mut self.bus, cause, resume_pc);
                self.finish_trap(result, cause, 0, resume_pc, None, Some(original))
            }
            PendingTrap::Syscall { resume_pc } => {
                let result = self.processor.enter_syscall(&mut self.bus, resume_pc);
                self.finish_trap(result, TrapCause::Syscall, 0, resume_pc, None, None)
            }
            PendingTrap::Software { payload, resume_pc } => {
                let result = self
                    .processor
                    .enter_software(&mut self.bus, payload, resume_pc);
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
                    .processor
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
        if !is_executable_state(self.state)
            || !self.architectural_state().status().interrupts_enabled()
            || self.processor.traps().has_active_frame()
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
        let had_frame = self.processor.traps().has_active_frame();
        let application = match self.engine.step(&mut self.processor, &mut self.bus) {
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
        if had_frame && !self.processor.traps().has_active_frame() {
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
        self.deliver_device_interrupts()?;
        Ok(())
    }

    /// Hands any interrupts the devices raised to the processor.
    ///
    /// **Called after every clock advance, not from inside `tick`.** A device learns
    /// things *in* `tick` — a timer expiring, an audio ring reaching half — and the
    /// machine asks afterwards, so a device never has to know whether an interrupt
    /// controller exists and never has to deliver anything itself. That is what keeps
    /// `Device` free of guest-controller vocabulary.
    ///
    /// The clock is committed first, so a device that raises an interrupt in response to
    /// the tick sees the time that caused it. And a refusal to queue one is propagated
    /// rather than dropped: a device that raised an interrupt the machine could not
    /// deliver has told the guest something, and swallowing that would be worse than the
    /// error.
    fn deliver_device_interrupts(&mut self) -> Result<(), MachineError> {
        for id in self.bus.take_device_interrupts() {
            self.request_interrupt(id)?;
        }
        Ok(())
    }

    /// Sets virtual time outright, forwards or backwards.
    ///
    /// **The restore path.** `advance_clock` only moves forward, which is right for
    /// running a machine and wrong for putting one back: a snapshot taken before the
    /// machine ran on could not be restored after it did, and a snapshot that cannot
    /// undo the running is not a snapshot. B6 added this so the VM lifecycle's
    /// `restore` is real rather than a one-way capture.
    ///
    /// Devices are *not* ticked. A device's elapsed count is part of its own snapshot
    /// and is written back by its `restore`; ticking it here too would apply the same
    /// interval twice, which for a timer means its deadline moves. So the order is:
    /// restore the devices, then set the clock.
    ///
    /// The machine's clock and the device manager's clock are set together, because a
    /// machine whose two clocks disagree is a machine where a device has been told a
    /// time the processor does not believe — and nothing else would report it.
    pub fn restore_time(&mut self, elapsed: CycleCount) {
        self.clock = VirtualClock::at(elapsed);
        self.bus.set_device_clock(elapsed);
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

/// The engine a name refers to.
///
/// This is the only place an [`EngineKind`] becomes an engine. It is deliberately a
/// closed `match` rather than a registry: when a JIT arrives this gains an arm and
/// nothing else has to change, and until then there is exactly one thing a name can
/// mean — which is why a machine can be asked to switch to an engine that does not
/// exist and be told so rather than silently continuing.
fn engine_for(kind: EngineKind) -> ReferenceInterpreter {
    match kind {
        EngineKind::Reference => ReferenceInterpreter::new(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineRun {
    pub executed: u64,
    pub halted_at: Option<u64>,
    pub trap: Option<TrapEvent>,
}
