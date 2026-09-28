//! The controls: what a person presses, and what the machine does about it.
//!
//! # Why this is not in the window
//!
//! A key press is an event; a debugger's action is a decision. Between them sits
//! a layer that has to answer questions — is the program running, is this
//! breakpoint already set, what does the user see afterwards — and that layer is
//! where the interesting mistakes live. So it is here, headless, and the window
//! only translates a key into a [`Control`].
//!
//! That is also what makes it testable. Every control is exercised against a real
//! machine in the test suite, with no window and no key events: a test presses
//! "step" by calling [`Controls::dispatch`] and checks what the program did. A
//! frontend whose controls could only be tested by synthesising key events would
//! have its controls untested.
//!
//! # Every outcome is a value
//!
//! [`Outcome`] is a closed set of facts, not a message. A frontend that got back
//! a string would have to read it back to decide whether to un-grey a button,
//! which is a frontend that will get it wrong the day the wording changes. A
//! refusal says *why* as a [`Refusal`] variant, which is a thing to match on.
//!
//! # What a control may do to a program
//!
//! Only what the debug API offers. There is no path from here to a register file
//! or to machine memory: `Controls` holds a `DebugController` and calls its
//! methods. A "reset" is a restore of the machine snapshot taken before the first
//! step, which is the only reset the API has — and which is honest, because it
//! restores the machine rather than pretending to re-run a boot.

use std::string::String;
use std::vec::Vec;

use lazalith_debug::{
    DebugController, DebugError, ExecutionState, MachineSnapshot, RunOutcome, StopReason,
};
use lazalith_devices::Device;
use lazalith_os::{LzxImage, ProcessId, ThreadId};

use crate::view::{Diagnostic, DiagnosticKind, Diagnostics};

/// Something a person asked the debugger to do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Control {
    /// Run until a breakpoint, a watchpoint, a pause, or the end.
    Run,
    /// Keep going from where the program is stopped, as if the breakpoints were
    /// not there.
    ContinueRun,
    /// Retire exactly one instruction.
    Step,
    /// Ask the next run or step to stop as soon as it can.
    Pause,
    /// Put the machine back the way it was before the first step.
    Reset,
    /// Set a breakpoint at the program counter, or clear the one that is there.
    ToggleBreakpoint,
    /// Set breakpoints on a line of a source file.
    BreakAtLine {
        /// The file, as the compiler was given it.
        name: &'static str,
        /// The line, one-based.
        line: u32,
    },
    /// Forget every breakpoint.
    ClearBreakpoints,
}

impl Control {
    /// The label this control is drawn with.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Run => "Run",
            Self::ContinueRun => "Continue",
            Self::Step => "Step",
            Self::Pause => "Pause",
            Self::Reset => "Reset",
            Self::ToggleBreakpoint => "Breakpoint",
            Self::BreakAtLine { .. } => "Break at line",
            Self::ClearBreakpoints => "Clear breakpoints",
        }
    }

    /// The physical key that triggers this control, if one does.
    ///
    /// The keys are function keys because a debugger's other keys belong to the
    /// program: the person is often typing into the guest's own input, and a
    /// frontend that swallowed letters would take them away from it. The value
    /// is a *scancode* rather than a keycode, so the binding is to a physical key
    /// and does not move when someone changes their keyboard layout. The numbers
    /// come from SDL3's headers, measured by the boundary crate's build script
    /// rather than written down here.
    pub const fn scancode(self) -> Option<u32> {
        match self {
            Self::Reset => Some(lazalith_sdl3::SCANCODE_F4),
            Self::Run => Some(lazalith_sdl3::SCANCODE_F5),
            Self::ContinueRun => Some(lazalith_sdl3::SCANCODE_F6),
            Self::Pause => Some(lazalith_sdl3::SCANCODE_F8),
            Self::ToggleBreakpoint => Some(lazalith_sdl3::SCANCODE_F9),
            Self::Step => Some(lazalith_sdl3::SCANCODE_F10),
            Self::BreakAtLine { .. } | Self::ClearBreakpoints => None,
        }
    }

    /// The control a physical key triggers, if any.
    ///
    /// A key with no control is not an error: the person pressed something the
    /// debugger does not use, and a debugger that complained would be wrong.
    pub fn from_scancode(scancode: u32) -> Option<Self> {
        // Listed rather than searched, so adding a key is adding a row and a
        // missing binding is a compiler error rather than a control that silently
        // stopped working.
        const KEYS: [(u32, Control); 6] = [
            (lazalith_sdl3::SCANCODE_F4, Control::Reset),
            (lazalith_sdl3::SCANCODE_F5, Control::Run),
            (lazalith_sdl3::SCANCODE_F6, Control::ContinueRun),
            (lazalith_sdl3::SCANCODE_F8, Control::Pause),
            (lazalith_sdl3::SCANCODE_F9, Control::ToggleBreakpoint),
            (lazalith_sdl3::SCANCODE_F10, Control::Step),
        ];
        KEYS.iter()
            .find(|(code, _)| *code == scancode)
            .map(|(_, control)| *control)
    }
}

/// Why a control did nothing.
///
/// A refusal is a value with a reason, not a message a caller has to read. Each
/// variant is a case a person can be told about and a test can match on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// There is no process loaded to act on.
    NoProcess,
    /// The process has exited and cannot be continued.
    AlreadyExited {
        /// The code it exited with.
        code: u32,
    },
    /// The process faulted and cannot be continued.
    AlreadyFaulted,
    /// A breakpoint on that line resolved to no addresses, because the line has no
    /// code in it.
    NoCodeOnLine {
        /// The line asked for.
        line: u32,
    },
    /// The image carries no debug information, so no line can be found.
    NoDebugInformation,
    /// The debug API refused, for a reason of its own.
    Refused,
}

impl Refusal {
    /// The sentence shown for this refusal.
    pub fn message(self) -> String {
        match self {
            Self::NoProcess => String::from("there is no process to act on"),
            Self::AlreadyExited { code } => {
                format!("the program already exited with {code}")
            }
            Self::AlreadyFaulted => String::from("the program faulted and cannot be continued"),
            Self::NoCodeOnLine { line } => {
                format!("line {line} has no code, so there is nothing to stop on")
            }
            Self::NoDebugInformation => String::from("the image carries no debug information"),
            Self::Refused => String::from("the machine refused"),
        }
    }

    /// The diagnostic code this refusal is recorded under.
    ///
    /// A code rather than the message, so a test can assert on it and a frontend
    /// can filter on it without matching prose.
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoProcess => "gui-no-process",
            Self::AlreadyExited { .. } => "gui-process-exited",
            Self::AlreadyFaulted => "gui-process-faulted",
            Self::NoCodeOnLine { .. } => "gui-line-has-no-code",
            Self::NoDebugInformation => "gui-no-debug-information",
            Self::Refused => "gui-refused",
        }
    }
}

/// What a control did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The program ran, and here is why it stopped.
    Ran {
        /// Why it stopped.
        reason: StopReason,
        /// How many instructions it retired.
        steps: u64,
    },
    /// The program retired one instruction.
    Stepped {
        /// Where it is now.
        address: u64,
    },
    /// A pause was asked for.
    ///
    /// The controller steps a program synchronously, so there is no moment between
    /// two instructions for a pause to interrupt. A pause is therefore a
    /// *request*, and the next `run` or `step` takes it at its first instruction
    /// boundary — which is where a frontend's event loop gets to notice. Calling
    /// this `Paused` would be claiming a stop that has not happened, and a
    /// frontend that showed the program as stopped when it was still running
    /// would be lying about the one thing a debugger is for.
    PauseRequested {
        /// Where the program is now, which is where it will stop unless it is
        /// already at the end of its run.
        address: u64,
    },
    /// A breakpoint was set.
    BreakpointSet {
        /// Where.
        address: u64,
    },
    /// The breakpoint at the program counter was removed.
    BreakpointCleared {
        /// Where it was.
        address: u64,
    },
    /// Breakpoints were set on a line.
    LineBreakpointsSet {
        /// The line.
        line: u32,
        /// Where they are, which is empty when the line has no code.
        addresses: Vec<u64>,
    },
    /// Every breakpoint was removed.
    BreakpointsCleared {
        /// How many there were.
        count: usize,
    },
    /// The machine was put back the way it was.
    Reset {
        /// Where the program counter is now, which is where it was before the
        /// first step.
        address: u64,
    },
    /// Nothing happened, for a stated reason.
    Refused {
        /// Why.
        reason: Refusal,
    },
}

impl Outcome {
    /// Whether the program moved.
    pub const fn advanced(&self) -> bool {
        match self {
            Self::Ran { .. }
            | Self::Stepped { .. }
            | Self::PauseRequested { .. }
            | Self::Reset { .. } => true,
            Self::BreakpointSet { .. }
            | Self::BreakpointCleared { .. }
            | Self::LineBreakpointsSet { .. }
            | Self::BreakpointsCleared { .. } => false,
            Self::Refused { .. } => false,
        }
    }

    /// The refusal, if this outcome is one.
    pub const fn refusal(&self) -> Option<Refusal> {
        match self {
            Self::Refused { reason } => Some(*reason),
            _ => None,
        }
    }
}

/// A machine, the process being debugged, and the state a reset restores.
pub struct Controls<D: Device> {
    controller: DebugController<D>,
    process: Option<ProcessId>,
    thread: Option<ThreadId>,
    reset_point: Option<MachineSnapshot>,
    diagnostics: Diagnostics,
}

impl<D: Device> Controls<D> {
    /// Wraps a booted controller with nothing loaded.
    pub fn new(controller: DebugController<D>) -> Self {
        Self {
            controller,
            process: None,
            thread: None,
            reset_point: None,
            diagnostics: Diagnostics::new(),
        }
    }

    /// The controller, for building a view.
    pub const fn controller(&self) -> &DebugController<D> {
        &self.controller
    }

    /// The frontend's diagnostics, which include what refusals were recorded as.
    pub const fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    /// Forgets the recorded diagnostics.
    pub fn clear_diagnostics(&mut self) {
        self.diagnostics.clear();
    }

    /// Loads `image` and remembers the machine so [`Control::Reset`] can come
    /// back to it.
    ///
    /// Loading *includes the supervisor handoff*. The machine boots in supervisor
    /// mode with the kernel's own first instruction at the program counter, and
    /// the handoff is the step that gives the machine to the guest. Debugging
    /// starts after it, so this performs it and takes the reset point afterwards:
    /// a reset that returned to the kernel's `RFE` would need another step before
    /// the user was back where they were, which is a reset that does not reset.
    pub fn load(
        &mut self,
        image: LzxImage,
        process: ProcessId,
        thread: ThreadId,
    ) -> Result<(), DebugError> {
        if let Err(error) = self.controller.load_image(image, process, thread) {
            // Recorded before the error goes out, so a caller that only logs the
            // error still gets the diagnostic the panel shows.
            self.record(Diagnostic::new(
                DiagnosticKind::Frontend,
                "gui-load-failed",
                format!("the program could not be scheduled: {error}"),
            ));
            return Err(error);
        }
        // The handoff is one step of the kernel's own code. A frontend that could
        // not do it would have no program counter to show at all, so a failure here
        // is the frontend's problem and is reported as one.
        if let Err(error) = self.controller.step(process) {
            self.record(Diagnostic::new(
                DiagnosticKind::Frontend,
                "gui-handoff-failed",
                format!("the supervisor handoff did not run: {error}"),
            ));
            return Err(error);
        }
        self.process = Some(process);
        self.thread = Some(thread);
        // Taken *after* the load and the handoff and before anything else has run, so
        // a reset returns the machine to the state the person started debugging in
        // rather than to some arbitrary point in the middle of a run.
        //
        // A whole-machine snapshot now has to be able to drive the machine, because a
        // process being activated keeps its memory in the machine rather than in the
        // process, and a snapshot that cannot see that cannot capture the program it
        // is a snapshot of. A frontend that cannot take one reports it, the same way
        // it reports a failed handoff.
        match self.controller.snapshot_machine() {
            Ok(snapshot) => self.reset_point = Some(snapshot),
            Err(error) => {
                self.record(Diagnostic::new(
                    DiagnosticKind::Frontend,
                    "gui-snapshot-failed",
                    format!("the reset point could not be captured: {error}"),
                ));
                return Err(error);
            }
        }
        Ok(())
    }

    /// The process being debugged.
    pub const fn process(&self) -> Option<ProcessId> {
        self.process
    }

    /// The thread being debugged.
    pub const fn thread(&self) -> Option<ThreadId> {
        self.thread
    }

    /// The process's state, or `None` if nothing is loaded.
    pub fn state(&self) -> Option<ExecutionState> {
        let process = self.process?;
        self.controller
            .session(process)
            .map(|session| session.state())
    }

    /// Whether the program can still be stepped.
    ///
    /// A frontend uses this to grey out the controls that would be refused, which
    /// is better than letting someone press them and be told afterwards.
    pub fn can_advance(&self) -> bool {
        match self.state() {
            Some(ExecutionState::Exited { .. }) | Some(ExecutionState::Faulted { .. }) => false,
            Some(_) => true,
            None => false,
        }
    }

    /// Runs `control` and reports what happened.
    pub fn dispatch(&mut self, control: Control) -> Outcome {
        let Some(process) = self.process else {
            return self.refuse(Refusal::NoProcess, control);
        };
        match control {
            Control::Run | Control::ContinueRun => self.run(process, control),
            Control::Step => self.step(process),
            Control::Pause => self.pause(process),
            Control::Reset => self.reset(),
            Control::ToggleBreakpoint => self.toggle_breakpoint(process),
            Control::BreakAtLine { name, line } => self.break_at_line(process, name, line),
            Control::ClearBreakpoints => self.clear_breakpoints(process),
        }
    }

    /// Runs until something stops the program.
    fn run(&mut self, process: ProcessId, control: Control) -> Outcome {
        if let Some(ExecutionState::Exited { code }) = self.state() {
            return self.refuse(Refusal::AlreadyExited { code }, control);
        }
        if let Some(ExecutionState::Faulted { .. }) = self.state() {
            return self.refuse(Refusal::AlreadyFaulted, control);
        }
        let result = match control {
            // `Continue` is `run` with the breakpoints ignored, and the debug API
            // spells that as its own call. A frontend that implemented Continue by
            // clearing the breakpoints, running, and setting them again would be
            // wrong in a way nobody would notice until a program set its own
            // breakpoint in the middle.
            Control::ContinueRun => self.controller.continue_(process),
            _ => self.controller.run(process),
        };
        self.report_run(result, control)
    }

    fn report_run(&mut self, result: Result<RunOutcome, DebugError>, control: Control) -> Outcome {
        match result {
            Ok(run) => Outcome::Ran {
                reason: run.reason.clone(),
                steps: run.steps,
            },
            Err(error) => {
                self.record(
                    Diagnostic::new(
                        DiagnosticKind::Frontend,
                        "gui-run-failed",
                        format!("{control:?} could not run the program: {error}"),
                    )
                    .at(self.controller.registers().pc()),
                );
                Outcome::Refused {
                    reason: Refusal::Refused,
                }
            }
        }
    }

    /// Retires one instruction.
    fn step(&mut self, process: ProcessId) -> Outcome {
        if let Some(ExecutionState::Exited { code }) = self.state() {
            return self.refuse(Refusal::AlreadyExited { code }, Control::Step);
        }
        match self.controller.step(process) {
            Ok(step) => Outcome::Stepped {
                address: step.registers.pc(),
            },
            Err(error) => {
                self.record(
                    Diagnostic::new(
                        DiagnosticKind::Frontend,
                        "gui-step-failed",
                        format!("the program could not be stepped: {error}"),
                    )
                    .at(self.controller.registers().pc()),
                );
                Outcome::Refused {
                    reason: Refusal::Refused,
                }
            }
        }
    }

    /// Asks for a pause.
    ///
    /// A pause is cooperative: the controller checks it at an instruction boundary,
    /// so the next `run` or `step` stops as soon as it can, and a program already
    /// inside a syscall is allowed to finish the call it is in. This does not take
    /// the pause — it asks for one, and the next run or step takes it. A frontend
    /// that pressed Pause and saw the program run on would be a debugger where
    /// Pause does not pause, so the run controls here take it on the next press,
    /// which is the only place it can be taken.
    fn pause(&mut self, _process: ProcessId) -> Outcome {
        if !self.can_advance() {
            return self.refuse(Refusal::AlreadyFaulted, Control::Pause);
        }
        self.controller.pause();
        let address = self.controller.registers().pc();
        Outcome::PauseRequested { address }
    }

    /// Puts the machine back where it was before the first step.
    fn reset(&mut self) -> Outcome {
        let Some(snapshot) = self.reset_point.clone() else {
            return self.refuse(Refusal::NoProcess, Control::Reset);
        };
        match self.controller.restore_machine(&snapshot) {
            Ok(()) => Outcome::Reset {
                address: self.controller.registers().pc(),
            },
            Err(error) => {
                self.record(
                    Diagnostic::new(
                        DiagnosticKind::Frontend,
                        "gui-reset-failed",
                        format!("the machine could not be restored: {error}"),
                    )
                    .at(self.controller.registers().pc()),
                );
                Outcome::Refused {
                    reason: Refusal::Refused,
                }
            }
        }
    }

    /// Sets or clears the breakpoint at the program counter.
    fn toggle_breakpoint(&mut self, process: ProcessId) -> Outcome {
        let address = self.controller.registers().pc();
        let Some(session) = self.controller.session_mut(process) else {
            return self.refuse(Refusal::NoProcess, Control::ToggleBreakpoint);
        };
        if session.is_breakpoint(address) {
            let cleared = session.clear_breakpoint(address);
            debug_assert!(
                cleared,
                "a breakpoint that is there clears, and a test covers that"
            );
            return Outcome::BreakpointCleared { address };
        }
        match session.set_breakpoint(address) {
            // The debug API answers whether the address was already set, and this
            // branch has already checked, so `false` here would mean the two
            // disagreed. Treating it as a refusal is right: a breakpoint the
            // frontend believes in and the machine does not is worse than one
            // that is honestly absent.
            Ok(true) => Outcome::BreakpointSet { address },
            Ok(false) => Outcome::Refused {
                reason: Refusal::Refused,
            },
            Err(_) => Outcome::Refused {
                reason: Refusal::Refused,
            },
        }
    }

    /// Sets breakpoints on a line of a source file.
    fn break_at_line(&mut self, process: ProcessId, name: &'static str, line: u32) -> Outcome {
        if self.controller.debug_info().is_none() {
            self.record(
                Diagnostic::new(
                    DiagnosticKind::Frontend,
                    Refusal::NoDebugInformation.code(),
                    Refusal::NoDebugInformation.message(),
                )
                .at(self.controller.registers().pc()),
            );
            return Outcome::Refused {
                reason: Refusal::NoDebugInformation,
            };
        }
        match self.controller.set_source_breakpoint(process, name, line) {
            Ok(addresses) if addresses.is_empty() => {
                // An empty answer is a real answer, not an error: a line can be a
                // comment, a blank, or a branch the backend never emitted. The
                // refusal exists so a frontend can say *that* rather than leaving
                // the user to wonder why nothing happened.
                self.record(
                    Diagnostic::new(
                        DiagnosticKind::Frontend,
                        Refusal::NoCodeOnLine { line }.code(),
                        Refusal::NoCodeOnLine { line }.message(),
                    )
                    .in_source(crate::view::SourcePlace::new(name, line, 1))
                    .at(self.controller.registers().pc()),
                );
                Outcome::LineBreakpointsSet {
                    line,
                    addresses: Vec::new(),
                }
            }
            Ok(addresses) => Outcome::LineBreakpointsSet { line, addresses },
            Err(_) => Outcome::Refused {
                reason: Refusal::Refused,
            },
        }
    }

    /// Forgets every breakpoint.
    fn clear_breakpoints(&mut self, process: ProcessId) -> Outcome {
        let Some(session) = self.controller.session_mut(process) else {
            return self.refuse(Refusal::NoProcess, Control::ClearBreakpoints);
        };
        Outcome::BreakpointsCleared {
            count: session.clear_breakpoints(),
        }
    }

    /// Records a refusal and returns the outcome that says so.
    fn refuse(&mut self, reason: Refusal, control: Control) -> Outcome {
        self.record(
            Diagnostic::new(
                DiagnosticKind::Frontend,
                reason.code(),
                format!("{control:?} did nothing: {}", reason.message()),
            )
            .at(self.controller.registers().pc()),
        );
        Outcome::Refused { reason }
    }

    /// Records a diagnostic.
    fn record(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }
}
