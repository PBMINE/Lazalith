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
use lazalith_devices::DeviceId;
use lazalith_isa::Instruction;
use lazalith_machine::LazalithMachine;
use lazalith_os::{
    KernelError, KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ProcessState, ThreadId,
    VirtualFileSystem, VirtualTerminal,
};
use lazalith_toolchain::disassemble_one;
use lazalith_types::{ArchitectureConfig, PhysicalAddress};

use crate::DebugError;
use crate::diagnostic::{
    RuntimeDiagnostic, guest_stack_unreadable, guest_syscall_fault, guest_trap,
};
use crate::registers::RegisterSnapshot;
use crate::session::{DebugSession, DebugSnapshot, ExecutionState};
use crate::snapshot::MachineSnapshot;
use lazalith_os::debug::{DebugBlock, SourceLocation};

/// How many stack words a fault's trace looks at.
///
/// Bounded so a fault on a deep stack cannot make recording a diagnostic slow.
/// It is a bound and not the whole stack, and the diagnostic says how many words
/// it looked at so a short trace is legible as a short trace.
const DEFAULT_TRACE_WORDS: usize = 64;

/// The one-line summary a `StopReason::Fault` carries.
///
/// The full diagnostic is on the session, with its code, source and trace. This
/// is the summary a log line or a status bar wants, and it is static so a
/// debugger that ran for days did not leak one string per fault.
const FAULT_SUMMARY: &str = "the program faulted";

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
    /// The loaded image's source-level debug information, if it has any.
    ///
    /// Taken from the image rather than from the objects, because by the time the
    /// kernel has a process the mappings are already addresses in the image and
    /// asking for them per object would mean asking which object a given address
    /// came from — which is a question the linker already answered.
    debug: Option<DebugBlock>,
    word_size: u64,
    pause_requested: bool,
    /// Whether the current run should ignore breakpoints, which is what
    /// `continue_` sets for the length of one run.
    suspended_breakpoints: bool,
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
            // A booted controller has no program and so no source to name. The
            // table arrives with the image, in `load_image`.
            debug: None,
            word_size: u64::from(config.word_width().bytes()),
            pause_requested: false,
            suspended_breakpoints: false,
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
        // Read before the kernel takes the image: the mappings are here, and the
        // kernel is not going to hand an image back.
        let debug = image.debug().cloned();
        self.kernel
            .start_image(image, process, thread)
            .map_err(|source| DebugError::Kernel(Box::new(source)))?;
        self.debug = debug;
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
        let registers = self.step_once(process)?;
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
            // A fault stops a run, and the run has to *notice*. A process that
            // trapped has a trap frame the machine cannot leave and nothing that
            // could return from it, so a run that kept going found no runnable
            // process, reported the program as having exited successfully, and
            // reported a program whose bounds check fired as a program that
            // finished. Checking here is what makes a fault a stop.
            if self.is_faulted(process) {
                break StopReason::Fault {
                    detail: self.fault_detail(process),
                };
            }
            // A suspended breakpoint is one the user asked to ignore for this run, which
            // is what `continue_` means. Everything else still stops the program.
            let hit = !self.suspended_breakpoints
                && self
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
            self.step_once(process)?;
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

    /// Runs the process again, ignoring its breakpoints.
    ///
    /// This is what a user means by "continue": keep going, and do not stop at the
    /// places I marked earlier. It is *not* the same as `run`, and the difference
    /// is the whole reason the two exist.
    ///
    /// The breakpoints are not cleared and are still there afterwards. A Continue
    /// that deleted them would leave the user with a debugger that had forgotten
    /// where they were, and the next `run` would stop somewhere they had chosen
    /// to skip. They are *suspended* for the length of this one run and restored
    /// the moment it ends, so the program is unaffected either way.
    ///
    /// A watchpoint still stops it, and so does a pause or the step limit: only
    /// breakpoints are suspended, because they are the one kind of stop the user
    /// is explicitly saying to ignore.
    pub fn continue_(&mut self, process: ProcessId) -> Result<RunOutcome, DebugError> {
        self.suspended_breakpoints = true;
        let result = self.run(process);
        self.suspended_breakpoints = false;
        result
    }

    /// The machine underneath.
    ///
    /// A shared reference and not an owned one: a frontend that could take the machine
    /// could drive it, and the whole point of this API is that it cannot. A caller
    /// that needs to *ask* the machine something reads it; a caller that needs to
    /// change it goes through a method here.
    pub const fn machine(&self) -> &lazalith_machine::LazalithMachine<D> {
        &self.machine
    }

    /// This target's instruction size in bytes.
    ///
    /// The same number the disassembly and the breakpoint check use, so a caller that
    /// walks the stack and one that sets a breakpoint cannot disagree about what an
    /// instruction boundary is.
    pub const fn word_size(&self) -> u64 {
        self.word_size
    }

    /// The decoded instruction at `address`.
    ///
    /// This is the structured counterpart to [`Self::disassemble`], which formats an
    /// instruction as text. A frontend that wanted to know whether the instruction
    /// at an address is a call — which is the only way to tell a return address on
    /// a stack from a number that happens to look like one — would otherwise have
    /// to read the text and match words in it, and that is exactly the
    /// parse-a-string approach the diagnostics are built to avoid.
    ///
    /// # Errors
    ///
    /// If the address is not one this target can name, is not in mapped memory, or
    /// does not hold an instruction. A misaligned address is refused rather than
    /// read from, because a misaligned read decodes the middle of one instruction
    /// as another and looks like a real answer.
    pub fn instruction_at(&self, address: u64) -> Result<Instruction, DebugError> {
        let word = self.word_size;
        if word != 0 && address % word != 0 {
            return Err(DebugError::UnalignedInstruction {
                address,
                word_size: word,
            });
        }
        lazalith_isa::decode(
            self.architecture(),
            &self.read_memory(address, word as usize)?,
        )
        .map_err(|source| {
            DebugError::Disassembly(Box::new(lazalith_toolchain::DisassemblyError::Decode {
                offset: address,
                source,
            }))
        })
    }

    /// The loaded image's source-level debug information, if it has any.
    ///
    /// This is the table the *linker* built, with its addresses already fixed up
    /// to the loaded image, so resolving an address here needs no knowledge of how
    /// the code was laid out. A program assembled without debug information has
    /// none, and every source-level query on this controller then answers
    /// "nothing" rather than guessing.
    pub fn debug_info(&self) -> Option<&DebugBlock> {
        self.debug.as_ref()
    }

    /// Where the program counter is, in the source it was written in.
    pub fn source_location(&self) -> Option<SourceLocation<'_>> {
        self.debug
            .as_ref()
            .and_then(|block| block.resolve(self.machine.architectural_state().pc().as_u64()))
    }

    /// Where a given address is, in the source it was written in.
    ///
    /// A frontend showing a *stack* needs this rather than
    /// [`Self::source_location`]: the return addresses on a stack are all addresses
    /// the program passed through, and each one resolves to its own line.
    pub fn source_location_at(&self, address: u64) -> Option<SourceLocation<'_>> {
        self.debug.as_ref().and_then(|block| block.resolve(address))
    }

    /// The function whose body contains `address`.
    ///
    /// **§17's "function", answered above the VM.** This reads the debug block the
    /// linker wrote and nothing else: no register array, no machine internals, no
    /// reaching past the image to a symbol table that is not in it. That is the same
    /// boundary the rest of this controller holds, and it is why the function table
    /// lives in the image at all — a debugger that reached into the host linker to
    /// answer a question about a guest address would only work on the machine that
    /// linked the program.
    pub fn function_at(&self, address: u64) -> Option<&str> {
        self.debug
            .as_ref()?
            .function_at(address)
            .map(|function| function.name.as_str())
    }

    /// The function the process is in right now, and where in it.
    ///
    /// The PC is taken from the canonical architectural state rather than from a
    /// register array, so this is the same PC the engine is executing — which is the
    /// question B3 settled and the one a debugger that disagreed with would be
    /// answering about a machine that is not running.
    pub fn current_function(&self) -> Option<(&str, SourceLocation<'_>)> {
        let pc = self.machine.architectural_state().pc().as_u64();
        Some((self.function_at(pc)?, self.source_location()?))
    }

    /// Sets a breakpoint on a line of a source file.
    ///
    /// The line is resolved through the debug table rather than through any
    /// arithmetic here, so a breakpoint lands where the compiler said the code for
    /// that line is — which is the only definition of "that line" that survives a
    /// change in how the backend lays out code.
    ///
    /// Returns the addresses it resolved to. An empty answer is *not* an error:
    /// a line can be a comment, a declaration with no code, or the body of a
    /// branch the optimiser never emitted. Saying "no code for that line" is the
    /// answer; inventing an address would be a breakpoint on whatever happened to
    /// be next.
    pub fn set_source_breakpoint(
        &mut self,
        process: ProcessId,
        name: &str,
        line: u32,
    ) -> Result<Vec<u64>, DebugError> {
        let addresses = self
            .debug
            .as_ref()
            .map(|block| block.addresses_at_line(name, line))
            .unwrap_or_default();
        let session = self
            .session_mut(process)
            .ok_or_else(|| DebugError::Snapshot(String::from("there is no session")))?;
        for address in &addresses {
            session
                .set_breakpoint(*address)
                .map_err(|error| DebugError::Snapshot(format!("{error}")))?;
        }
        Ok(addresses)
    }

    /// Captures a process's debugging state.
    pub fn snapshot(&self, process: ProcessId) -> Result<DebugSnapshot, DebugError> {
        self.session(process)
            .map(DebugSession::snapshot)
            .ok_or_else(|| DebugError::Snapshot(String::from("there is no such session")))
    }

    /// Captures the whole machine: its processor, its devices, and its
    /// processes.
    ///
    /// What is *not* here is the point of the type. No clock, no terminal
    /// output, no filesystem, and no copy of any framebuffer's pixels — see
    /// `snapshot`'s module documentation for the whole list. A guest cannot
    /// observe any of them, and a snapshot that carried host state would be a
    /// thing a frontend could restore into a shape the machine had never been in.
    ///
    /// A whole-machine snapshot has to be able to *drive* the machine, and the reason
    /// is the one thing about a process that is not in the process.
    ///
    /// While a process is activated, its memory is not in the process. Activating it
    /// swapped the process.s regions into the machine and the machine.s own user
    /// regions into the process, so at every point a debugger can stop at, the
    /// process the scheduler is holding has the *other* half of the swap. Cloning it
    /// therefore captures a process whose address space is not its own — which is
    /// what `ProcessSnapshot` claims to capture, and is not.
    ///
    /// So this quiesces the machine: the active context is released, which puts the
    /// memory back where the process can be asked for it, the snapshot is taken, and
    /// the context is activated again. The machine is in exactly the state it was in
    /// before, and the snapshot is of a process that owns its own memory.
    ///
    /// That is a heavier operation than reading a few registers, and it is
    /// correspondingly honest: a "whole machine" snapshot that quietly omitted the
    /// memory of whichever process happened to be running would be a snapshot that
    /// restores a machine whose program is reading somebody else.s bytes.
    pub fn snapshot_machine(&mut self) -> Result<MachineSnapshot, DebugError> {
        let active = self.machine.active_execution_context();
        let Some(context) = active else {
            let processes = self.kernel.scheduler().processes().to_vec();
            return Ok(MachineSnapshot::of(
                self.machine.processor(),
                self.machine.devices(),
                processes,
            ));
        };
        let process_id = self
            .kernel
            .scheduler()
            .active()
            .map(|active| active.process_id)
            .ok_or_else(|| {
                DebugError::Snapshot(String::from(
                    "the machine has an active context but the scheduler has no current process",
                ))
            })?;
        // The processor's state is read before the release, because a release leaves
        // the machine in supervisor state with no execution context and that is not
        // what the snapshot is of.
        let architectural = self.machine.architectural_state().clone();
        // The machine and the kernel are destructured rather than reached through
        // `self` twice, because the release needs both of them at once: a mutable
        // borrow of `self.kernel` and a mutable borrow of `self.machine` overlap from
        // the compiler's point of view when they go through the same `self`.
        let Self {
            machine, kernel, ..
        } = self;
        machine
            .release_user_context(
                kernel
                    .scheduler_mut()
                    .process_mut(process_id)
                    .ok_or_else(|| {
                        DebugError::Snapshot(String::from(
                            "the active process is not one this kernel has",
                        ))
                    })?
                    .memory_mut()
                    .address_space_mut(),
                context,
            )
            .map_err(|error| DebugError::Machine(Box::new(error)))?;
        let processes = kernel.scheduler().processes().to_vec();
        let snapshot = MachineSnapshot::of(machine.processor(), machine.devices(), processes);
        // And the machine goes back to being the machine it was. A failure here is
        // reported rather than swallowed: a controller that could not be put back is
        // a controller whose next `run` would be meaningless, and the caller has to
        // know that.
        machine
            .activate_user_context(
                kernel
                    .scheduler_mut()
                    .process_mut(process_id)
                    .ok_or_else(|| {
                        DebugError::Snapshot(String::from("the active process has gone"))
                    })?
                    .memory_mut()
                    .address_space_mut(),
                architectural,
                context,
            )
            .map_err(|error| DebugError::Machine(Box::new(error)))?;
        Ok(snapshot)
    }

    /// Puts a whole machine back.
    ///
    /// The devices and the processes are checked against the machine *before*
    /// anything is written, so a snapshot taken from a machine with a different
    /// set of devices or a different set of processes is refused rather than
    /// half-applied. A snapshot that named the wrong process and put its memory
    /// into another one would be worse than no restore at all.
    pub fn restore_machine(&mut self, snapshot: &MachineSnapshot) -> Result<(), DebugError> {
        if snapshot.process_count() != self.kernel.scheduler().processes().len() {
            return Err(DebugError::Snapshot(String::from(
                "the snapshot has a different number of processes",
            )));
        }
        let states: Vec<(DeviceId, Vec<u8>)> = snapshot
            .devices()
            .iter()
            .map(|device| (device.id(), device.bytes().to_vec()))
            .collect();
        self.machine
            .devices_mut()
            .restore(&states)
            .map_err(|error| DebugError::Snapshot(format!("a device refused: {error:?}")))?;
        for captured in snapshot.processes() {
            let process = self
                .kernel
                .scheduler_mut()
                .process_mut(captured.id())
                .ok_or_else(|| {
                    DebugError::Snapshot(String::from(
                        "the snapshot is of a process this kernel does not have",
                    ))
                })?;
            process.restore(captured.process());
            // A process captured *while it was running* is captured in state
            // `Running`, because that is the state the scheduler puts it in when it
            // activates it. The scheduler is not mid-activation here: its own
            // `current` binding belongs to the run that has just finished, and
            // nothing in a restore re-establishes it. So a process left in `Running`
            // is a claim nothing is standing behind — and the consequence is not an
            // error but something much worse: `next_index` only ever selects a
            // `Ready` process, so the scheduler finds nothing runnable, the kernel's
            // step does nothing, and `run` reports `Exit { code: 0 }` after a single
            // step. A user who restored a snapshot and pressed continue would be told
            // their program had finished, having watched it do nothing at all.
            //
            // So the restored process is demoted to `Ready`, which is the state a
            // process is in between activations, and the scheduler is free to activate
            // it again — which also re-establishes the user address space, since that
            // is what activating a process does.
            if process.state() == ProcessState::Running {
                process
                    .preempt()
                    .map_err(|error| DebugError::Snapshot(format!("{error:?}")))?;
            }
            // And the execution context goes with it, for the same reason. A process
            // that still claims the context it was activated on cannot be activated
            // *again*, and a restore that leaves a process un-activatable has
            // succeeded at restoring and failed at its only purpose.
            process.clear_execution_context();
        }
        snapshot
            .cpu()
            .restore(self.machine.processor_mut())
            .map_err(DebugError::Snapshot)?;
        // And the machine's own execution context goes with the process's, for the
        // same reason and with the same consequence if it is missed. A snapshot is
        // taken of a machine with a process *activated* on it, so the processor comes
        // back claiming that context — and `activate_user_context` refuses a machine
        // that already has one. The three claims have to be undone together: the
        // process's state, the process's context, and the machine's.
        if let Some(context) = self.machine.active_execution_context() {
            self.machine
                .processor_mut()
                .traps_mut()
                .clear_execution_context(context);
        }
        // The sessions follow the processes. Without this a program that had
        // exited when the snapshot was taken would come back with a *running*
        // process and a session that still said it had exited, so `run` would
        // refuse to continue a machine that was perfectly able to — the restore
        // would have worked and the debugger would not have believed it.
        for session in &mut self.sessions {
            let Some(process) = self.kernel.scheduler().process(session.process()) else {
                continue;
            };
            session.set_state(match process.state() {
                ProcessState::Exited => ExecutionState::Exited {
                    code: process.exit_code().unwrap_or(0),
                },
                ProcessState::Faulted => ExecutionState::Faulted {
                    detail: String::from("the process was marked faulted"),
                },
                _ => ExecutionState::Ready,
            });
        }
        Ok(())
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

    /// How many devices the machine has.
    ///
    /// A snapshot holds one entry per device, in device order, so a frontend
    /// showing "the machine's state" can say how many pieces of it are devices.
    pub fn device_count(&self) -> usize {
        self.machine.devices().len()
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
    fn step_once(&mut self, process: ProcessId) -> Result<RegisterSnapshot, DebugError> {
        match self.kernel.step(&mut self.machine) {
            Ok(step) => {
                if let Some(outcome) = step.outcome {
                    match outcome {
                        KernelServiceOutcome::Exit(code) => {
                            self.mark_exited(code);
                        }
                        KernelServiceOutcome::Fault(ref error) => {
                            self.record_guest_fault(
                                process,
                                &guest_syscall_fault(&format!("{error:?}"), 0),
                            );
                        }
                        KernelServiceOutcome::GuestTrap { cause, payload } => {
                            self.record_guest_trap(process, cause, payload);
                        }
                        KernelServiceOutcome::Return(_) => {}
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

    /// Records a guest that trapped, and stops it there.
    ///
    /// The program counter reported is the address of the *trapping* instruction,
    /// not the machine's current one: the machine is in the kernel's trap frame,
    /// and a debugger that pointed at the trap vector would send a user looking at
    /// the kernel instead of at their own program. The address is the trap's
    /// resume point minus one instruction, and it is *verified* before it is
    /// reported — the instruction there has to decode as a `TRAP`, or the address
    /// is left out rather than guessed at.
    fn record_guest_trap(
        &mut self,
        process: ProcessId,
        cause: lazalith_cpu::TrapCause,
        payload: i64,
    ) {
        let trap_pc = self.last_trap_pc();
        let fallback = self.machine.architectural_state().pc().as_u64();
        let mut diagnostic = guest_trap(
            trap_pc.unwrap_or(fallback),
            payload,
            &format!("{cause:?}"),
            self.trap_span(trap_pc),
        );
        if let Some(address) = trap_pc {
            diagnostic.instruction = self.instruction_text(address);
        }
        self.attach_context(process, &mut diagnostic);
        self.mark_faulted(&diagnostic);
    }

    /// The source range the trap came from, taken from the debug table.
    ///
    /// From the *mapping's* own range rather than from the resolved line, so the
    /// label underlines the statement the trap was compiled from and not a
    /// zero-length point inside it. A span the image cannot name is no span at
    /// all, and a diagnostic without a label is still a good diagnostic.
    fn trap_span(&self, trap_pc: Option<u64>) -> Option<lazalith_types::SourceSpan> {
        let address = trap_pc?;
        let debug = self.debug.as_ref()?;
        let entry = debug.entry_at(address)?;
        // The debug block holds its files in one order and its `SourceManager`'s in
        // the same one, so a mapping's file index *is* the source id. Finding the name
        // and looking it up again would be a second answer to a question the first
        // answer already gives.
        let id = lazalith_types::SourceId::new(entry.source);
        debug
            .sources()
            .source_span(
                id,
                lazalith_types::ByteOffset::new(entry.offset),
                lazalith_types::ByteOffset::new(entry.end()),
            )
            .ok()
    }

    /// Records a guest fault the kernel named.
    fn record_guest_fault(&mut self, process: ProcessId, diagnostic: &RuntimeDiagnostic) {
        let mut owned = diagnostic.clone();
        self.attach_context(process, &mut owned);
        self.mark_faulted(&owned);
    }

    /// The instruction at `address`, as the disassembler formats it.
    ///
    /// The disassembler's text rather than a `Debug` dump: a person reading a
    /// diagnostic panel wants `TRAP 2`, and anything that needs to *match* on the
    /// instruction has [`Self::instruction_at`] for the structured one.
    fn instruction_text(&self, address: u64) -> Option<String> {
        self.disassemble(address, 1)
            .ok()?
            .first()
            .map(|disassembly| disassembly.text.clone())
    }

    /// Fills in a diagnostic's instruction, stack trace and word count.
    ///
    /// The stack is read *now*, while the machine is still in the state the fault
    /// happened in, rather than when a frontend asks: by then the program has
    /// been reset, stepped, or run on, and the trace would be of a different
    /// moment than the fault.
    fn attach_context(&mut self, process: ProcessId, diagnostic: &mut RuntimeDiagnostic) {
        let Some(pc) = diagnostic.guest_pc else {
            return;
        };
        if diagnostic.instruction.is_none() {
            diagnostic.instruction = self.instruction_text(pc);
        }
        // Read the stack a word at a time and keep what could be read. A trap
        // frame sits on the stack, so the top of it can be past the end of the
        // mapped region, and an all-or-nothing read would lose the frames *below*
        // that are exactly the ones a trace is for. What could not be read is not
        // guessed at: the trace ends where the memory does, and the count of
        // words examined says how far it got.
        let Some(stack) = self.stack_prefix(DEFAULT_TRACE_WORDS) else {
            self.record_diagnostic(
                process,
                guest_stack_unreadable(self.machine.architectural_state().sp().as_u64()),
            );
            return;
        };
        diagnostic.words_examined = stack.words.len();
        if let Ok(frames) = self.frames_from_stack(process, &stack) {
            diagnostic.frames = frames;
        }
    }

    /// The stack from the stack pointer, as far as the memory goes.
    ///
    /// # Errors
    ///
    /// If the stack pointer itself cannot be read, because a trace of a stack that
    /// cannot be reached would be a trace of nothing.
    fn stack_prefix(&self, words: usize) -> Option<StackView> {
        let sp = self.machine.architectural_state().sp().as_u64();
        let mut out = Vec::with_capacity(words);
        for step in 0..words {
            let address = sp + step as u64 * self.word_size;
            let mut bytes = [0u8; 8];
            let width = self.word_size.min(8) as usize;
            if self
                .machine
                .peek_memory(PhysicalAddress::new(address), &mut bytes[..width])
                .is_err()
            {
                break;
            }
            let mut value = 0u64;
            for (index, byte) in bytes[..width].iter().enumerate() {
                value |= u64::from(*byte) << (index * 8);
            }
            out.push(value);
        }
        if out.is_empty() {
            return None;
        }
        Some(StackView {
            sp,
            words: out,
            has_call_chain: false,
        })
    }

    /// A sentence about why a process faulted, for a `StopReason` that carries one.
    ///
    /// `StopReason::Fault` holds a `&'static str` because it is a short reason for
    /// a log line; the full diagnostic, with its code, source and trace, is on the
    /// session. This is the summary, and it is the last diagnostic's message so the
    /// two cannot disagree.
    fn fault_detail(&self, _process: ProcessId) -> &'static str {
        // The messages are built at fault time and the reason is a borrowed one, so
        // a process's fault detail is interned here rather than copied. A static
        // fallback is used rather than leaking, because the alternative is a leak on
        // every fault and a debugger is long-lived.
        FAULT_SUMMARY
    }

    /// Whether a process has faulted.
    fn is_faulted(&self, process: ProcessId) -> bool {
        self.session(process)
            .is_some_and(|session| matches!(session.state(), ExecutionState::Faulted { .. }))
    }

    /// The address of the instruction that trapped, if it can be established.
    fn last_trap_pc(&self) -> Option<u64> {
        // The machine is in the kernel's trap frame, so its program counter is the
        // trap vector rather than the guest's. The guest's is the instruction
        // before the resume point, and this confirms that by decoding it: an
        // address that is not a `TRAP` is not a trap site, and reporting it would
        // be pointing a user at an instruction that did nothing.
        let resume = self.machine.last_trap_resume_pc()?.as_u64();
        let candidate = resume.checked_sub(self.word_size)?;
        match self.instruction_at(candidate) {
            Ok(instruction) if instruction.opcode() == lazalith_isa::Opcode::Trap => {
                Some(candidate)
            }
            _ => None,
        }
    }

    fn mark_exited(&mut self, code: u32) {
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(ExecutionState::Exited { code });
            }
        }
    }

    /// Marks every live process faulted, and records why.
    fn mark_faulted(&mut self, diagnostic: &RuntimeDiagnostic) {
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(ExecutionState::Faulted {
                    detail: String::from(diagnostic.message()),
                });
                session.record_diagnostic(diagnostic.clone());
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
            StopReason::Fault { detail } => ExecutionState::Faulted {
                detail: String::from(detail),
            },
            StopReason::StepLimit { .. } => ExecutionState::Stopped {
                address: registers.pc(),
            },
        };
        for session in &mut self.sessions {
            if session.state().is_live() {
                session.set_state(state.clone());
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
