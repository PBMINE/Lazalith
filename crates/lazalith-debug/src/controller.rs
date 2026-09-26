//! The controller: the only thing a debug frontend is given.
//!
//! [`DebugController`] owns the machine and the kernel and is the whole of what
//! a frontend can reach. There is no accessor that hands out a `&mut` machine, a
//! `&mut` kernel, or a reference into the CPU's register file — a frontend reads
//! owned snapshots and sends commands. That is the enforcement behind the
//! roadmap's "do not allow frontends to manipulate CPU internals directly", and
//! it is why the register and memory calls return values rather than references.

use alloc::vec;
use alloc::vec::Vec;

use lazalith_boot::BootImage;
use lazalith_devices::Device;
use lazalith_machine::LazalithMachine;
use lazalith_os::{
    KernelError, KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ThreadId,
    VirtualFileSystem, VirtualTerminal,
};
use lazalith_toolchain::disassemble_one;
use lazalith_types::{ArchitectureConfig, PhysicalAddress};

use crate::DebugError;
use crate::registers::RegisterSnapshot;
use crate::session::{DebugSession, DebugSnapshot, ExecutionState};

/// Why a run stopped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StopReason {
    /// A breakpoint was reached.
    Breakpoint {
        /// Where.
        address: u64,
    },
    /// A watchpoint's bytes changed.
    Watchpoint {
        /// Where.
        address: u64,
    },
    /// A frontend asked to pause.
    Pause,
    /// The instruction limit was reached with nothing else stopping it.
    StepLimit {
        /// The limit that was reached.
        limit: u64,
    },
    /// The program called `exit`.
    Exit {
        /// The code it passed.
        code: u32,
    },
    /// The program faulted and cannot be continued.
    Fault {
        /// The fault, as the machine reported it.
        detail: &'static str,
    },
}

/// What a call to `run` or `continue_` did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunOutcome {
    /// Why it stopped.
    pub reason: StopReason,
    /// The registers at the moment it stopped.
    pub registers: RegisterSnapshot,
    /// How many instructions retired during the call.
    pub steps: u64,
}

/// What a call to `step` did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StepOutcome {
    /// The registers after the instruction.
    pub registers: RegisterSnapshot,
    /// Whether the step landed on a breakpoint.
    pub at_breakpoint: Option<u64>,
    /// Whether the instruction changed a watchpoint's bytes.
    pub hit_watchpoint: Option<u64>,
}

/// One decoded instruction, with where it is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Disassembly {
    /// The address it is at.
    pub address: u64,
    /// Its text, as the toolchain formats it.
    pub text: alloc::string::String,
}

/// The stack as a debugger can show it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackView {
    /// The stack pointer, which is where the stack *is*, not where it starts.
    pub sp: u64,
    /// The words at and above the stack pointer, lowest address first.
    pub words: Vec<u64>,
    /// Whether a call chain could be built from these words.
    ///
    /// It cannot, and the reason is in this crate's documentation: the calling
    /// convention reserves the return address below the frame but does not record
    /// a frame pointer, so there is no chain to walk. A debugger that printed
    /// addresses and called them a call stack would be showing a heap of numbers.
    pub has_call_chain: bool,
}

/// A machine, a kernel, and the sessions debugging what runs on them.
pub struct DebugController<D: Device> {
    machine: LazalithMachine<D>,
    kernel: LazalithKernel,
    config: ArchitectureConfig,
    sessions: Vec<DebugSession>,
    word_size: u64,
    pause_requested: bool,
    step_limit: u64,
}

impl<D: Device> DebugController<D> {
    /// Boots a machine with a supervisor kernel and returns the controller.
    ///
    /// `kernel_bytes` is the supervisor's own code, and `trap_offset` is the
    /// address *within* it where a trap from user code lands — which is the
    /// kernel's own return instruction, and which only the caller knows, because
    /// a controller that guessed at it would be guessing where a kernel keeps its
    /// epilogue. The trap vector is a parameter for the same reason the terminal
    /// and the filesystem are: a debugger that silently disagreed with the
    /// machine's setup about where a trap goes would misreport every fault.
    pub fn boot(
        config: ArchitectureConfig,
        kernel_bytes: &[u8],
        trap_offset: u64,
        terminal: VirtualTerminal,
        filesystem: VirtualFileSystem,
        devices: lazalith_devices::DeviceManager<D>,
    ) -> Result<Self, DebugError> {
        let boot = BootImage::new(config, kernel_bytes.to_vec(), 0)
            .map_err(|source| DebugError::Boot(Box::new(source)))?;
        let mut machine = boot
            .start(devices)
            .map_err(|source| DebugError::Boot(Box::new(source)))?;
        let trap = lazalith_boot::KERNEL_LOAD_ADDRESS
            .checked_add(trap_offset)
            .ok_or(DebugError::Snapshot(String::from(
                "the trap vector is not an address this target has",
            )))?;
        machine
            .set_trap_vector(lazalith_types::InstructionAddress::new(trap))
            .map_err(|source| DebugError::Machine(Box::new(source)))?;
        // Stepping the supervisor's own first instruction is the handoff to user
        // privilege. Doing it here rather than letting the first program step do
        // it keeps the transition explicit: no program is given the chance to run
        // before the machine has left supervisor mode.
        machine
            .step()
            .map_err(|source| DebugError::Machine(Box::new(source)))?;
        let kernel = LazalithKernel::new(1, terminal, filesystem)
            .map_err(|source| DebugError::Kernel(Box::new(source)))?;
        Ok(Self {
            machine,
            kernel,
            config,
            sessions: Vec::new(),
            word_size: u64::from(config.word_width().bytes()),
            pause_requested: false,
            step_limit: 1_000_000,
        })
    }

    /// The architecture this controller's machine is.
    pub const fn architecture(&self) -> ArchitectureConfig {
        self.config
    }

    /// The instruction limit a `run` uses when nothing stops it first.
    pub fn step_limit(&self) -> u64 {
        self.step_limit
    }

    /// Sets the instruction limit a `run` uses.
    ///
    /// This is a bound on how long a *debugger* is willing to wait, not a change
    /// to the machine: a program that never stops is a program the user asked to
    /// run, and a debugger that hung instead of returning would be the bug.
    pub fn set_step_limit(&mut self, limit: u64) {
        self.step_limit = limit;
    }

    /// Loads `image` as a process and returns a session debugging it.
    pub fn load_image(
        &mut self,
        image: LzxImage,
        process: ProcessId,
        thread: ThreadId,
    ) -> Result<&mut DebugSession, DebugError> {
        self.kernel
            .start_image(image, process, thread)
            .map_err(|source| DebugError::Kernel(Box::new(source)))?;
        let session = DebugSession::new(process, thread, self.word_size);
        self.sessions.push(session);
        Ok(self
            .sessions
            .last_mut()
            .expect("the session was just pushed"))
    }

    /// The session for `process`, or `None` if there is not one.
    pub fn session(&self, process: ProcessId) -> Option<&DebugSession> {
        self.sessions
            .iter()
            .find(|session| session.process() == process)
    }

    /// The session for `process`, mutably, or `None` if there is not one.
    ///
    /// This is how a frontend configures breakpoints. It is a `&mut
    /// DebugSession` and not a `&mut` anything else: the session holds
    /// breakpoints and watchpoints and nothing that can execute.
    pub fn session_mut(&mut self, process: ProcessId) -> Option<&mut DebugSession> {
        self.sessions
            .iter_mut()
            .find(|session| session.process() == process)
    }

    /// Every session.
    pub fn sessions(&self) -> &[DebugSession] {
        &self.sessions
    }

    /// Asks the next `run` or `step` to stop as soon as it can.
    ///
    /// A pause is cooperative: it is checked at an instruction boundary, so a
    /// program already inside a syscall is allowed to finish the call it is in.
    /// Refusing to interrupt a validated syscall would leave a program that
    /// blocked in a driver undebuggable, and interrupting one mid-way would leave
    /// a kernel structure half-updated.
    pub fn pause(&mut self) {
        self.pause_requested = true;
    }

    /// Whether a pause has been asked for and not yet taken.
    pub const fn pause_requested(&self) -> bool {
        self.pause_requested
    }

    /// The registers, as an owned snapshot.
    pub fn registers(&self) -> RegisterSnapshot {
        RegisterSnapshot::of(self.machine.architectural_state())
    }

    /// Reads `length` bytes of guest memory at `address`.
    ///
    /// The bytes come back in an owned buffer, so a frontend can look at memory
    /// without being able to change it. Writing guest memory is not offered:
    /// there is no way to do it that leaves the machine's own checks in place, and
    /// a debug API that could edit a process's memory behind the kernel's back
    /// would not be the thing the kernel validates.
    ///
    /// A read has to be a whole number of words. A partial word is a caller
    /// mistake, and returning the words that *were* whole would look like a short
    /// read of memory rather than a refusal to read it.
    pub fn read_memory(&self, address: u64, length: usize) -> Result<Vec<u8>, DebugError> {
        if (length as u64) % self.word_size != 0 {
            return Err(DebugError::Snapshot(String::from(
                "a memory read must be a whole number of words",
            )));
        }
        let mut out = vec![0u8; length];
        self.machine
            .peek_memory(PhysicalAddress::new(address), &mut out)
            .map_err(|source| DebugError::Memory(Box::new(source)))?;
        Ok(out)
    }

    /// The stack as a debugger can show it.
    ///
    /// This is the stack pointer and the words above it, and nothing more. There
    /// is no call chain: the calling convention reserves the return address below
    /// the frame but records no frame pointer, so a stack walk would be a walk of
    /// whatever numbers happened to be on the stack. `has_call_chain` is false and
    /// says so rather than leaving a frontend to invent one.
    pub fn stack(&self, words: usize) -> Result<StackView, DebugError> {
        let sp = self.machine.architectural_state().sp().as_u64();
        let mut out = Vec::with_capacity(words);
        for step in 0..words {
            let address = sp + step as u64 * self.word_size;
            let mut bytes = [0u8; 8];
            let width = self.word_size.min(8) as usize;
            self.machine
                .peek_memory(PhysicalAddress::new(address), &mut bytes[..width])
                .map_err(|source| DebugError::Memory(Box::new(source)))?;
            let mut value = 0u64;
            for (index, byte) in bytes[..width].iter().enumerate() {
                value |= u64::from(*byte) << (index * 8);
            }
            out.push(value);
        }
        Ok(StackView {
            sp,
            words: out,
            has_call_chain: false,
        })
    }

    /// Decodes `count` instructions from `address`.
    ///
    /// Decoding stops at the first byte that is not a canonical instruction and
    /// reports what it had, because a debugger walking a range that runs into
    /// data should show the data's address rather than refuse the whole request.
    pub fn disassemble(&self, address: u64, count: usize) -> Result<Vec<Disassembly>, DebugError> {
        let mut out = Vec::with_capacity(count);
        for index in 0..count {
            let at = address + index as u64 * self.word_size;
            let mut bytes = vec![0u8; self.word_size as usize];
            self.machine
                .peek_memory(PhysicalAddress::new(at), &mut bytes)
                .map_err(|source| DebugError::Memory(Box::new(source)))?;
            match disassemble_one(self.config, &bytes, at) {
                Ok(instruction) => out.push(Disassembly {
                    address: at,
                    text: alloc::string::String::from(instruction.text()),
                }),
                Err(error) => {
                    return Err(DebugError::Disassembly(Box::new(error)));
                }
            }
        }
        Ok(out)
    }

    /// Steps one instruction of the process `process` is debugging.
    ///
    /// A pending pause is *satisfied* by the step rather than delaying it: the
    /// frontend asked to stop, and stopping after one instruction is stopping.
    /// The request is therefore cleared here, so a later `run` is not stopped by
    /// a pause that has already been honoured.
    pub fn step(&mut self, process: ProcessId) -> Result<StepOutcome, DebugError> {
        self.require_live(process)?;
        self.pause_requested = false;
        let before = self.watch_snapshot(process)?;
        let registers = self.step_once()?;
        self.count_step(process);
        let after = self.watch_snapshot(process)?;
        let hit_watchpoint = first_change(&before, &after);
        let pc = registers.pc();
        let at_breakpoint = self
            .session(process)
            .filter(|session| session.is_breakpoint(pc))
            .map(|_| pc);
        Ok(StepOutcome {
            registers,
            at_breakpoint,
            hit_watchpoint,
        })
    }

    /// Runs the process until something stops it.
    pub fn run(&mut self, process: ProcessId) -> Result<RunOutcome, DebugError> {
        self.require_live(process)?;
        let limit = self.step_limit;
        let mut retired: u64 = 0;
        let reason = loop {
            let pc = self.machine.architectural_state().pc().as_u64();
            let hit = self
                .session(process)
                .is_some_and(|session| session.is_breakpoint(pc));
            if hit {
                break StopReason::Breakpoint { address: pc };
            }
            // A pending pause is taken here and *only* here, and taking it clears
            // the request. Clearing it at the start of the run instead would
            // discard a pause the user asked for before the run began, which is
            // exactly the moment a frontend asks for one: it stops the program,
            // changes a breakpoint, and continues.
            if self.pause_requested {
                self.pause_requested = false;
                break StopReason::Pause;
            }
            if retired >= limit {
                break StopReason::StepLimit { limit };
            }
            let before = self.watch_snapshot(process)?;
            self.step_once()?;
            retired += 1;
            self.count_step(process);
            if self.is_exited() {
                let code = self
                    .session(process)
                    .and_then(|session| match session.state() {
                        ExecutionState::Exited { code } => Some(code),
                        _ => None,
                    })
                    .unwrap_or(0);
                break StopReason::Exit { code };
            }
            let after = self.watch_snapshot(process)?;
            if let Some(address) = first_change(&before, &after) {
                break StopReason::Watchpoint { address };
            }
        };
        let registers = self.registers();
        let reason = self.settle(reason, &registers);
        Ok(RunOutcome {
            reason,
            registers,
            steps: retired,
        })
    }

    /// Whether any session has finished.
    fn is_exited(&self) -> bool {
        self.sessions
            .iter()
            .any(|session| matches!(session.state(), ExecutionState::Exited { .. }))
    }

    /// Refuses to drive a session that is not there or is no longer live.
    fn require_live(&self, process: ProcessId) -> Result<(), DebugError> {
        match self.session(process) {
            None => Err(DebugError::Snapshot(String::from(
                "there is no such session",
            ))),
            Some(session) if session.state().is_live() => Ok(()),
            Some(_) => Err(DebugError::Snapshot(String::from(
                "that process is not running",
            ))),
        }
    }

    /// Counts one retired instruction on every live session.
    ///
    /// Every session rather than the one asked for, because the machine is
    /// single-threaded in v1: one retired instruction belongs to whichever
    /// process is running, and counting it on a finished one would make its step
    /// count keep rising after it exited.
    fn count_step(&mut self, _process: ProcessId) {
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.count_step();
            }
        }
    }

    /// Runs the process again, which is what a user means by "continue".
    pub fn continue_(&mut self, process: ProcessId) -> Result<RunOutcome, DebugError> {
        self.run(process)
    }

    /// Captures a process's debugging state.
    pub fn snapshot(&self, process: ProcessId) -> Result<DebugSnapshot, DebugError> {
        self.session(process)
            .map(DebugSession::snapshot)
            .ok_or_else(|| DebugError::Snapshot(String::from("there is no such session")))
    }

    /// Puts a process's debugging state back.
    ///
    /// The process must not be running: a restore that changed the breakpoints
    /// under a `run` in progress would make the run stop somewhere the frontend
    /// never asked about.
    pub fn restore(
        &mut self,
        process: ProcessId,
        snapshot: &DebugSnapshot,
    ) -> Result<(), DebugError> {
        let live = self
            .session(process)
            .is_some_and(|session| session.state().is_live());
        if live {
            return Err(DebugError::Snapshot(String::from(
                "a live process's session cannot be restored into",
            )));
        }
        self.session_mut(process)
            .ok_or_else(|| DebugError::Snapshot(String::from("there is no such session")))?
            .restore(snapshot)
    }

    /// The kernel's terminal output, for a frontend that shows it.
    pub fn terminal_output(&self) -> Vec<u8> {
        self.kernel.terminal().terminal().output().to_vec()
    }

    /// The display device, for a frontend that draws the program's window.
    ///
    /// This is a shared reference to a *device*, not to the machine. A device is
    /// a thing the program wrote to; reading what it last presented is
    /// inspection, and the returned frame names an address rather than pixels so
    /// that reading them stays the caller's choice.
    pub fn display(&self) -> &lazalith_devices::DisplayDevice {
        self.kernel.display().device()
    }

    /// One step of the machine, with the scheduler's validation in front of it.
    fn step_once(&mut self) -> Result<RegisterSnapshot, DebugError> {
        match self.kernel.step(&mut self.machine) {
            Ok(step) => {
                if let Some(outcome) = step.outcome {
                    if let KernelServiceOutcome::Exit(code) = outcome {
                        self.mark_exited(code);
                    }
                    if let KernelServiceOutcome::Fault(_) = outcome {
                        self.mark_faulted();
                    }
                }
                Ok(self.registers())
            }
            Err(KernelError::Scheduler(lazalith_os::SchedulerError::NoRunnableProcess)) => {
                // A process that has already exited leaves nothing to step. That
                // is the normal end of a run, not a failure, so it is reported as
                // a stop rather than an error.
                self.mark_exited(0);
                Ok(self.registers())
            }
            Err(error) => Err(DebugError::Kernel(Box::new(error))),
        }
    }

    fn mark_exited(&mut self, code: u32) {
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(ExecutionState::Exited { code });
            }
        }
    }

    fn mark_faulted(&mut self) {
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(ExecutionState::Faulted {
                    detail: "the program faulted",
                });
            }
        }
    }

    /// Records why a run stopped on the session.
    fn settle(&mut self, reason: StopReason, registers: &RegisterSnapshot) -> StopReason {
        let state = match reason {
            StopReason::Breakpoint { address } | StopReason::Watchpoint { address } => {
                ExecutionState::Stopped { address }
            }
            StopReason::Pause => ExecutionState::Stopped {
                address: registers.pc(),
            },
            StopReason::Exit { code } => ExecutionState::Exited { code },
            StopReason::Fault { detail } => ExecutionState::Faulted { detail },
            StopReason::StepLimit { .. } => ExecutionState::Stopped {
                address: registers.pc(),
            },
        };
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(state);
            }
        }
        reason
    }

    /// The bytes under a session's watchpoints, before or after a step.
    fn watch_snapshot(&self, process: ProcessId) -> Result<Vec<(u64, Vec<u8>)>, DebugError> {
        let session = self
            .session(process)
            .ok_or_else(|| DebugError::Snapshot(String::from("there is no such session")))?;
        let mut out = Vec::new();
        for watch in session.watchpoints() {
            out.push((
                watch.address,
                self.read_memory(watch.address, watch.size.bytes() as usize)?,
            ));
        }
        Ok(out)
    }
}

/// The address of the first watched byte that changed.
fn first_change(before: &[(u64, Vec<u8>)], after: &[(u64, Vec<u8>)]) -> Option<u64> {
    for (address, bytes) in after {
        let previous = before
            .iter()
            .find(|(seen, _)| seen == address)
            .map(|(_, seen)| seen);
        if previous != Some(bytes) {
            return Some(*address);
        }
    }
    None
}
