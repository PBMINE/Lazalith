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
    ArchitecturalState, ControlStateError, CpuFault, CpuFaultCause, DataAccessError, EngineDecline,
    EngineError, EngineFault, EngineKind, ExecutionContextId, ExecutionEngine, FaultOrigin,
    OutcomeApplication, OutcomeErrorKind, Processor, ReferenceInterpreter, StepResult,
    SyscallCompletion, TrapAttempt, TrapCause, TrapController, TrapRequest,
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

/// Why a step retired no instruction.
///
/// **Separate from [`MachineError`] because one of these is a normal outcome.** A trap is
/// an ordinary event a guest can be written to handle; an engine handoff is an internal
/// detail that must never be visible. Both are "this step did not retire", but only one
/// of them is something the caller asked for.
#[derive(Debug)]
enum StepStop {
    /// A trap was entered, and this is it.
    Trapped(TrapEvent),
    /// No engine on this machine can execute the instruction at the program counter.
    ///
    /// **A machine fact, and never a guest fault.** The interpreter declines nothing, so
    /// reaching this means the guest is standing at an address the machine cannot
    /// execute at all — which is a defect in this build's coverage or a misconfigured
    /// yield boundary, and blaming the guest for it with a trap would be exactly the
    /// mistake B23 removed.
    NoEngine(EngineDecline),
    /// The machine itself refused: trap entry failed, the clock rejected an advance, or
    /// an engine's report was inconsistent.
    ///
    /// **The one case that can leave the machine `Faulted`,** because `enter_trap` sets
    /// that when it cannot enter a trap. That is unchanged from B6 and is correct: a
    /// machine that cannot enter a trap the guest caused has genuinely lost the ability
    /// to continue, and §11's handoff rules do not apply because there is no guest state
    /// left to hand over.
    Failed(MachineError),
}

impl From<MachineError> for StepStop {
    fn from(error: MachineError) -> Self {
        Self::Failed(error)
    }
}

/// A trap that ended a step, or the reason no engine could run it.
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
    /// Restoring a machine's lifecycle state from a snapshot.
    ///
    /// B19 added this. `VmSnapshot` had no lifecycle state to restore, so a machine
    /// that had halted stayed halted after a restore of a snapshot taken before it
    /// halted — the processor's registers and the machine's `Halted` disagreeing, and
    /// the management layer is where that showed up.
    RestoreState,
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
    /// A step that retired normally. `instructions` is how many guest instructions
    /// retired, which is one for an interpreter and more for a JIT that executed a
    /// block; the caller needs it because the machine's own instruction count and the
    /// virtual time it charges both come from it.
    Stepped {
        application: OutcomeApplication,
        instructions: u16,
    },
    Trapped {
        event: TrapEvent,
    },
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
    /// No engine on this machine can execute the instruction at the program counter.
    ///
    /// **Not a guest fault and not a trap, and that is the point.** The active engine
    /// declined, the Reference Interpreter was handed the instruction, and it declined
    /// too — which cannot happen for a well-formed guest and means this build cannot
    /// execute this address at all.
    ///
    /// It is an error rather than a fault because a fault would put the guest in a trap
    /// frame, and a trap is a thing the guest observes: its program counter moves, its
    /// privilege changes, and a handler it wrote runs. Making a machine's own coverage
    /// gap look like a guest error is the exact conflation B23 exists to remove, so this
    /// is reported to the caller — a debugger, a manager, a test — and never entered into
    /// the guest's control flow.
    NoExecutionEngine {
        /// What no engine on this machine could do.
        decline: EngineDecline,
    },
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
            Self::NoExecutionEngine { decline } => {
                write!(f, "no engine on this machine can execute here: {decline}")
            }
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
            // A decline is not a wrapped error — it is this build's coverage, and there
            // is nothing underneath it to point at.
            Self::NoExecutionEngine { .. } => None,
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
    /// The engine a decline is handed to.
    ///
    /// **A second engine *value*, never a second architectural state.** It borrows the
    /// same `processor` for the length of one step, exactly as `engine` does, so running
    /// an instruction here instead of there has nothing to reconcile — which is the whole
    /// reason a JIT → interpreter handoff can be precise without a sync point. It is
    /// always a [`ReferenceInterpreter`]: the semantic authority, and the one engine that
    /// never declines, so the handoff cannot fail to find a destination.
    ///
    /// It is held rather than constructed per decline because constructing one is a
    /// `Box` allocation on a path taken by every instruction the JIT cannot translate,
    /// which for a register-only JIT is most of them.
    fallback: Box<dyn ExecutionEngine<Bus<D>>>,
    /// Guest addresses an engine must not execute an instruction at, sorted.
    ///
    /// **Set from the debugger's breakpoints, and pushed into the engine before every
    /// step.** This is the mechanism that stops a translated block running straight over
    /// a breakpoint, and it lives on the machine rather than in the debugger because the
    /// engine is what has to honour it and the engine cannot see the debugger.
    ///
    /// An engine that retires one instruction at a time has no use for it; one that
    /// retires a block has no other way to know.
    yield_points: Vec<InstructionAddress>,
    /// How many times an engine declined and this machine ran the instruction elsewhere.
    ///
    /// **Observability, and deliberately not a [`MachineEvent`].** The guest must not be
    /// able to tell a handoff happened, which means it must not appear in the stream of
    /// step results the guest's own code path observes — but a debugger and a benchmark
    /// absolutely need to count them, because "the JIT ran nothing" and "the JIT ran
    /// everything" are both invisible without this number. B24 measures it.
    handoffs: u64,
    /// Native instructions retired by engines this machine has already discarded.
    ///
    /// **Banked at every switch**, because the outgoing engine object is dropped and its
    /// own count goes with it. See [`LazalithMachine::native_instructions`].
    native_total: u64,
    /// Why the last engine handoff happened, if one has.
    last_decline: Option<EngineDecline>,
    /// Retired instructions still to run on the current engine before switching to the
    /// JIT, or `None` when no automatic switch is armed.
    ///
    /// **A countdown rather than a hotness score, and the field is named for what it
    /// does.** See [`LazalithMachine::use_jit_after`] for why the automatic transition
    /// is opt-in and why this is not a real hot-code detector.
    jit_warmup: Option<u64>,
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
    /// What the last retired instruction cost, in virtual cycles.
    ///
    /// **A report, not state a snapshot needs.** The clock itself is architectural and
    /// is restored (B18); this is a per-instruction accounting kept so a machine can
    /// answer "what did that instruction cost", which is what a differential test
    /// between two engines (B24) compares and what a `disassembly` view shows. It
    /// derives from the clock, so it is not part of what a snapshot must carry.
    last_cycles: u8,
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
            // Two separate values, because they are two separate roles: `engine` is
            // whichever the caller chose and `fallback` is the one a decline goes to.
            // Handing a decline to the *active* engine would be a no-op, and handing it
            // to a freshly built interpreter would allocate on every declined
            // instruction.
            fallback: Box::new(ReferenceInterpreter::new()),
            yield_points: Vec::new(),
            handoffs: 0,
            native_total: 0,
            last_decline: None,
            jit_warmup: None,
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
            last_cycles: 0,
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

    /// Guest addresses an engine must not execute an instruction at.
    ///
    /// # What this is for
    ///
    /// **It is how a debugger's breakpoints survive an engine that retires several
    /// instructions per step.** A JIT block can cover thirty-one instructions, and a
    /// breakpoint at the third of them would be stepped over: the guest's arithmetic
    /// would be entirely correct and the debugger would be reporting a stop the guest
    /// never took. The debugger owns the breakpoints — it is the thing that knows where
    /// the user's code is — and this is the one piece of that knowledge the machine needs,
    /// because the engine is what has to stop.
    ///
    /// # Why the machine filters it
    ///
    /// **An address at or below the program counter is dropped, and that filtering is the
    /// method's most important behaviour.** A caller that has already been told to stop
    /// at an address has, by definition, already done so; re-arming it would exclude the
    /// instruction about to run, and an engine asked to exclude the instruction it was
    /// about to run has nothing to execute and declines — every step, forever. The
    /// machine therefore keeps only addresses strictly ahead of the program counter, so
    /// the set cannot be poisoned by a stale breakpoint.
    ///
    /// A no-op for an engine that retires one instruction at a time, and harmless.
    pub fn set_yield_points(&mut self, points: impl IntoIterator<Item = u64>) {
        let mut points: Vec<InstructionAddress> =
            points.into_iter().map(InstructionAddress::new).collect();
        points.sort_unstable();
        points.dedup();
        if points != self.yield_points {
            // The engine's cache holds blocks translated under the *old* set, and a
            // block translated with a boundary is different code from one without, so
            // both engines are told. The interpreter ignores it; the JIT clears its
            // cache, which is the point.
            self.engine.set_yield_boundary(None);
            self.yield_points = points;
        }
    }

    /// The guest addresses an engine has been told not to execute at.
    pub fn yield_points(&self) -> Vec<u64> {
        self.yield_points.iter().map(|at| at.as_u64()).collect()
    }

    /// How many times an engine declined and this machine ran the instruction elsewhere.
    ///
    /// **A count, not an event, and the difference is the requirement.** §11 says a mode
    /// switch must not be observable to the guest, so this cannot appear in the
    /// [`MachineEvent`] stream the guest's own execution path sees. A debugger and a
    /// benchmark need it anyway: without it, "the JIT ran every instruction" and "the JIT
    /// ran none and the interpreter did all the work" are the same machine state, and
    /// B24's performance numbers would be uninterpretable.
    pub const fn engine_handoffs(&self) -> u64 {
        self.handoffs
    }

    /// How many guest instructions have retired on this machine, ever.
    ///
    /// **The count is of *retired* instructions, and a translated block contributes all
    /// of them.** Before B23 this was advanced by one per `step`, so a machine running a
    /// JIT block of thirty-one instructions under-counted by thirty — and `run(limit)`,
    /// which counts retired instructions against its limit, would have run roughly thirty
    /// times further than asked. It is exposed because B24 measures against it and
    /// because a debugger reporting a program counter with no idea how far the program has
    /// got is not much of a debugger.
    pub const fn executed_instruction_count(&self) -> u64 {
        self.executed
    }

    /// How many guest instructions have retired as host-native code, across every engine
    /// this machine has ever run.
    ///
    /// **The answer to "did the JIT actually do anything", and the only one available
    /// through a machine.** An engine-switching differential test compares architectural
    /// state, and a JIT that never ran a single instruction natively would match the
    /// reference on every field of it — because the interpreter is correct and is the
    /// oracle. This is the number that distinguishes "the JIT ran the program" from "the
    /// JIT was installed and declined everything", and B23's handoff tests are worthless
    /// without it.
    ///
    /// **Cumulative across engine switches, and the accumulation is the point.** A switch
    /// drops the outgoing engine object, and with it that engine's own count — so a naive
    /// implementation of this accessor reports zero the moment a JIT is switched away
    /// from, and a test that switches `interpreter → JIT → interpreter` and then asks
    /// how much the JIT did would be told "none" about the one segment that used it.
    /// [`switch_execution_engine`] therefore banks the outgoing count into `native_total`
    /// before the object goes, and a run that used a JIT at any point reports it however
    /// many times the engine changed afterwards.
    ///
    /// Both engines are counted, so a decline handed to the fallback is visible as a
    /// handoff rather than as native work.
    pub fn native_instructions(&self) -> u64 {
        self.native_total
            .saturating_add(self.engine.native_instruction_count().unwrap_or(0))
            .saturating_add(self.fallback.native_instruction_count().unwrap_or(0))
    }

    /// Why the last engine handoff happened, if one has.
    pub const fn last_engine_decline(&self) -> Option<EngineDecline> {
        self.last_decline
    }

    /// Switches to the JIT once this many instructions have retired on another engine.
    ///
    /// # What this is for
    ///
    /// §11 lists "interpreter execution → hot code identified → JIT compiles → JIT
    /// continues from the same architectural state" as a required transition. This is
    /// that transition, and it is **opt-in because the honest version of "hot code" is
    /// not this.** A real hotness counter decides per code region which blocks are worth
    /// compiling; this decides once, for the whole machine, after a warm-up.
    ///
    /// It is offered anyway, for two reasons. It proves the transition at an instruction
    /// boundary with no restart, which is the part of §11 that can be wrong; and it gives
    /// a caller that has *already* decided this machine should be fast a way to say so
    /// without reaching past the machine's API to poke its engine.
    ///
    /// # Why it is not the default
    ///
    /// **Because for most guest code it would make things slower, and a default that
    /// makes things slower is not a default.** The JIT translates straight-line register
    /// operations; real programs are memory traffic and control flow, which it declines.
    /// Enabling it unconditionally would pay a failed translation on every instruction
    /// and run nothing natively. That is a measurement (B24) rather than a guess, and
    /// the measurement is the reason this is opt-in.
    ///
    /// The switch happens *at* an instruction boundary, between steps, with the
    /// architectural state untouched — the same operation as a manual
    /// [`switch_execution_engine`](Self::switch_execution_engine), reached automatically.
    /// A count of zero disables it again.
    pub fn use_jit_after(&mut self, instructions: u64) -> Result<(), MachineError> {
        self.jit_warmup = Some(instructions);
        if instructions == 0 {
            self.switch_execution_engine(EngineKind::Jit)
        } else {
            Ok(())
        }
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
    /// # What exists to switch between
    ///
    /// **Two engines since B21**: the [`ReferenceInterpreter`], which is the semantic
    /// authority, and [`lazalith_cpu::FastInterpreter`], which executes the same
    /// semantics with some redundant validation removed. B21 measured that optimisation
    /// as worth nothing — which is the argument for a JIT rather than a better
    /// interpreter. What the second engine is *for* is that B23's handoff can be tested
    /// against two engines that both actually run, rather than one engine switching to
    /// itself and proving that assignment works.
    ///
    /// There is no JIT in this repository and this does not pretend otherwise.
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
        // **Bank the outgoing engine's native count before dropping it.** The object goes
        // with the assignment on the next line, and its own counter with it — so a
        // machine that switched away from a JIT would report zero native instructions
        // afterwards, which is the one number B23's tests and B24's measurements exist to
        // obtain. Architectural state is untouched by any of this; the count is
        // observability of how the work was done, and it survives the switch on purpose.
        self.native_total = self
            .native_total
            .saturating_add(self.engine.native_instruction_count().unwrap_or(0));
        self.engine = engine_for::<Bus<D>>(kind);
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

    /// Puts the machine's lifecycle state back, as a snapshot restore does.
    ///
    /// **This exists because `VmSnapshot` did not record it, and a snapshot that does
    /// not record a machine's halt state restores a halted machine.** The processor
    /// comes back exactly as it was saved — not halted, pointing at the next
    /// instruction — while `state` still said `Halted`, so `is_halted()` reported a
    /// stopped machine whose registers were mid-flight. Two sources of truth, disagreeing,
    /// and the manager layer is where it showed up: a restore of a pre-halt snapshot
    /// onto a halted VM left the VM reporting halted.
    ///
    /// The write is guarded the same way every other machine mutation is: an active
    /// execution context or a trap frame means the machine is mid-something, and putting
    /// a lifecycle state back underneath that is the kind of change the lifecycle
    /// exists to refuse. `Created` is refused too, because a created machine is one
    /// that has not been described yet, and a snapshot of one describes a different
    /// thing than a snapshot of a running machine.
    pub fn restore_state(&mut self, state: MachineState) -> Result<(), MachineError> {
        if self.active_execution_context().is_some() {
            return Err(MachineError::InvalidUserContext {
                reason: "the machine's state cannot be restored under an active execution context",
            });
        }
        if self.processor.traps().has_active_frame() {
            return Err(MachineError::InvalidUserContext {
                reason: "the machine's state cannot be restored while a trap frame is active",
            });
        }
        if state == MachineState::Created {
            return Err(MachineError::InvalidTransition {
                operation: MachineOperation::RestoreState,
                state: self.state,
            });
        }
        self.state = state;
        Ok(())
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
        // The fallback engine's private state goes too, for the same reason: a reset that
        // left one engine's cache and dropped the other's would be a reset that depended
        // on which engine had been active.
        self.fallback.discard_private_state();
        self.clock = VirtualClock::at(self.initial_time);
        self.interrupts.reset();
        self.last_trap_fault = None;
        // Handoff and decline counters are *observability of this run*, not machine state,
        // so a reset clears them. Keeping them would make a benchmark's second run
        // report the first run's handoffs, which is worse than starting from zero.
        self.handoffs = 0;
        self.last_decline = None;
        self.native_total = 0;
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

    /// Runs the active engine, handing a decline to the fallback.
    ///
    /// **This is B23's handoff, and the `match` on [`EngineFault`] is the whole
    /// design.** There are two arms because there are exactly two things that can go
    /// wrong, and they want opposite responses:
    ///
    /// - a **guest fault** is the guest's problem and becomes a trap, exactly as it did
    ///   before any JIT existed;
    /// - a **decline** is this engine's problem and is answered by running the
    ///   instruction on another engine, so the guest sees an ordinary retired
    ///   instruction and cannot tell.
    ///
    /// Before B23 the two were the same type — a `CpuFault` with a `JitDeclined` variant
    /// — and the `Err` arm trapped. That trapped programs for using instructions the JIT
    /// had not learned, and if the trap could not be entered the machine went terminally
    /// `Faulted`. **The types are what make that unrepeatable**: a decline cannot reach
    /// `enter_trap` without an arm here that says so.
    ///
    /// # Why the handoff is precise
    ///
    /// **Because a decline must have changed nothing, the interpreter runs exactly the
    /// instruction the JIT declined.** [`ExecutionEngine`] requires it, and the JIT's own
    /// `step` asserts it, but the guarantee is what makes this correct rather than
    /// merely plausible:
    ///
    /// - the program counter still names the instruction that was declined, so the
    ///   interpreter executes *that* instruction — not a re-execution of one the JIT
    ///   already retired, and not a skip past one it could not;
    /// - no register, no flag, no clock tick and no memory byte was written, so the
    ///   interpreter starts from the state the guest left and produces the result the
    ///   interpreter alone would have;
    /// - the machine's own `executed` count and `last_cycles` are advanced by the
    ///   interpreter's answer, not by the JIT's attempt, so a run that spends most of
    ///   its time declining costs what the guest did and not what the engine tried.
    ///
    /// # Why there is a second engine object
    ///
    /// **Two engines, one architectural state.** `fallback` is a second
    /// [`ExecutionEngine`] *value*, not a second copy of the guest — it borrows the same
    /// `&mut self.processor` for the length of one step, which is the arrangement B3
    /// built and the reason the invariant holds. Running it for one instruction instead
    /// of installing it permanently is cheaper than any reconciliation, because there is
    /// nothing to reconcile.
    fn run_engine(&mut self) -> Result<StepResult, StepStop> {
        match self.engine.step(&mut self.processor, &mut self.bus) {
            Ok(result) => Ok(result),
            Err(EngineFault::Declined(decline)) => {
                self.handoffs += 1;
                self.last_decline = Some(decline);
                // **The fallback is the Reference Interpreter, always.** It is the
                // semantic authority and it declines nothing, so this arm cannot loop.
                match self.fallback.step(&mut self.processor, &mut self.bus) {
                    Ok(result) => Ok(result),
                    Err(EngineFault::Guest(fault)) => {
                        // The declined instruction turned out to fault when it was
                        // executed properly, which is a perfectly ordinary guest fault and
                        // is reported as one. This is the case a JIT must get right: a
                        // null dereference the JIT cannot even translate still has to
                        // trap identically to the interpreter's.
                        let cause = trap_cause(&fault);
                        let resume_pc = fault.pc;
                        let event = self.enter_trap(PendingTrap::Fault {
                            cause,
                            resume_pc,
                            original: fault,
                        })?;
                        Err(StepStop::Trapped(event))
                    }
                    Err(EngineFault::Declined(second)) => {
                        // Two engines in a row declining the same instruction means the
                        // machine cannot execute it at all. That is a fact about this
                        // build, not a guest fault and not an engine switch, and there is
                        // no third place to send it — so it is surfaced as an error
                        // rather than turned into a trap, which would blame the guest for
                        // this machine's coverage.
                        Err(StepStop::NoEngine(EngineDecline::UnsupportedInstruction {
                            reason: match second {
                                EngineDecline::UnsupportedInstruction { reason } => reason,
                                _ => "the reference interpreter declined, which is a bug",
                            },
                        }))
                    }
                }
            }
            Err(EngineFault::Guest(fault)) => {
                let cause = trap_cause(&fault);
                let resume_pc = fault.pc;
                let event = self.enter_trap(PendingTrap::Fault {
                    cause,
                    resume_pc,
                    original: fault,
                })?;
                Err(StepStop::Trapped(event))
            }
        }
    }

    /// The guest address the engine must not execute an instruction at, or `None`.
    ///
    /// **Strictly after the current program counter, which is what makes the JIT's
    /// boundary well-formed.** The set is kept sorted so this is the first address at or
    /// after here. Addresses at or before here are excluded on purpose: the caller has
    /// already been told to stop there — a debugger checks a breakpoint *before*
    /// stepping — and re-arming one behind the program counter would exclude the very
    /// instruction about to run, leaving the JIT with nothing to translate and the
    /// machine declining every step forever.
    fn yield_boundary(&self) -> Option<InstructionAddress> {
        let here = self.architectural_state().pc();
        self.yield_points.iter().copied().find(|at| *at > here)
    }

    fn step_inner(&mut self) -> Result<MachineEvent, MachineError> {
        self.ensure_executable(MachineOperation::Step)?;
        if let Some(event) = self.try_external_interrupt()? {
            return Ok(MachineEvent::Trapped { event });
        }
        // The boundary a block must not run past, recomputed each step because it is a
        // function of the current program counter: the nearest address at or after here
        // that a caller has asked not to execute.
        let boundary = self.yield_boundary();
        self.engine.set_yield_boundary(boundary);
        self.fallback.set_yield_boundary(boundary);
        let had_frame = self.processor.traps().has_active_frame();
        let result = match self.run_engine() {
            Ok(result) => result,
            // A trap was entered, so this step retired nothing: no clock, no count, no
            // outcome to apply. The event is the step's result and it already carries the
            // resume address, so returning here is what keeps the trap from being
            // applied twice.
            Err(StepStop::Trapped(event)) => return Ok(MachineEvent::Trapped { event }),
            Err(StepStop::NoEngine(decline)) => {
                return Err(MachineError::NoExecutionEngine { decline });
            }
            Err(StepStop::Failed(error)) => return Err(error),
        };
        // **Time is charged here: after the instruction retired, before the outcome is
        // applied.** Three things make this the only place that works.
        //
        // - It is after the engine's step, so the cost is the cost of the instruction
        //   that actually ran, reported by whichever engine ran it. A faulted fetch
        //   retires nothing and is charged nothing, which is what a guest measuring its
        //   own work expects. Charging beforehand would mean fetching and decoding the
        //   instruction twice and would bill a guest for an instruction that never ran.
        // - It is before the outcome is applied, so a `Halt` and a `Trap` are both
        //   charged. Both executed an instruction, and a machine where trapping was free
        //   would let a guest spend unbounded time for nothing.
        // - It goes through `advance_clock`, so devices are ticked and any interrupt a
        //   device raises in response to the tick is delivered. The interrupt is then
        //   taken at the *next* step's boundary by `try_external_interrupt`, which is
        //   why a timer works without the step loop knowing that timers exist.
        self.last_cycles = result.cycles;
        self.advance_clock(CycleCount::new(u64::from(result.cycles)))?;
        self.executed = self
            .executed
            .checked_add(u64::from(result.instructions))
            .ok_or(MachineError::InstructionCountOverflow)?;
        if had_frame && !self.processor.traps().has_active_frame() {
            self.last_trap_fault = None;
        }
        // The automatic interpreter → JIT transition, taken at this instruction boundary.
        //
        // **Here, after the instruction retired and the clock was charged**, because that
        // is the only point at which the architectural state is complete and consistent.
        // A switch anywhere earlier would be a switch in the middle of a step, which is
        // the thing §11 says must not happen — and switching is free here precisely
        // because the state is whole.
        if let Some(remaining) = self.jit_warmup {
            let remaining = remaining.saturating_sub(u64::from(result.instructions));
            if remaining == 0 {
                self.jit_warmup = None;
                self.switch_execution_engine(EngineKind::Jit)?;
            } else {
                self.jit_warmup = Some(remaining);
            }
        }
        match result.application {
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
            OutcomeApplication::Continue => Ok(MachineEvent::Stepped {
                application: result.application,
                instructions: result.instructions,
            }),
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
        let before = self.clock.elapsed();
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
                MachineEvent::Stepped { instructions, .. } => {
                    executed += u64::from(instructions);
                }
            }
        };
        Ok(MachineRun {
            executed,
            halted_at,
            trap,
            cycles: self.clock.elapsed().as_u64() - before.as_u64(),
        })
    }

    /// What the last retired instruction cost, in virtual cycles.
    ///
    /// A report rather than state a snapshot carries: the clock is architectural and is
    /// restored, and this is a per-instruction accounting derived from it.
    pub const fn last_instruction_cycles(&self) -> u8 {
        self.last_cycles
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

/// Classifies a guest fault into the trap the guest will see.
///
/// **Every arm of this function is a fault the guest caused**, and that is now enforced by
/// the type rather than by care: an engine decline is an [`EngineDecline`] and arrives as
/// [`EngineFault::Declined`], which [`LazalithMachine::run_engine`] answers with a handoff
/// and never passes here.
///
/// In B22 this function had an arm for a `CpuFaultCause::JitDeclined`, with a comment
/// explaining that reaching it would be a bug — and the machine reached it, on every
/// instruction the JIT could not translate, trapping programs for a compiler limitation
/// and going terminally `Faulted` when the trap could not be entered. **A comment saying
/// "this is unreachable" is a wish; a type that cannot carry the value is a fact.**
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
/// The engine a machine starts on, and the one a switch installs.
///
/// **A `Box<dyn ExecutionEngine>`, so a switch is an assignment and not a rebuild.** B3
/// made the field a trait object precisely so a second engine could be adopted; B21 adds
/// the second engine, and the only thing that had to change is this function.
///
/// The return type is the trait object rather than an enum of engines because an enum
/// would make every future engine a variant here *and* in the machine's field, and the
/// day a JIT arrives that is three places to edit for one engine.
fn engine_for<M: lazalith_memory::CpuMemory>(kind: EngineKind) -> Box<dyn ExecutionEngine<M>> {
    match kind {
        EngineKind::Reference => Box::new(ReferenceInterpreter::new()),
        EngineKind::Optimized => Box::new(lazalith_cpu::FastInterpreter::new()),
        EngineKind::Jit => Box::new(lazalith_jit::Jit::new()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineRun {
    /// How many instructions retired.
    pub executed: u64,
    /// The instruction count at which the guest halted, if it did.
    pub halted_at: Option<u64>,
    /// The trap that ended the run, if one did.
    pub trap: Option<TrapEvent>,
    /// The virtual cycles this run cost.
    ///
    /// **Reported as well as derivable, because it is the cheaper half to ask for and
    /// the harder half to reconstruct.** The clock says where virtual time *is*; this
    /// says what the run *spent*, which is a different question a caller asking "was
    /// that worth it" (B21) and a differential test asking "did the two engines agree
    /// on what this cost" (B24) both want answered without a snapshot in between.
    pub cycles: u64,
}
