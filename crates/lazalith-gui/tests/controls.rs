//! The controls, driven against real machines.
//!
//! Every test here presses a control by calling `dispatch` and checks what the
//! program did. No window and no synthesised key events: the point of keeping
//! [`Controls`] out of the window is that the controls are testable at all, and a
//! test that had to open a display to press a key would not be run.
//!
//! What each test states is one thing the controls claim:
//!
//! - Run stops at a breakpoint, at the end, or at the instruction limit, and says
//!   which;
//! - Continue runs past the breakpoints rather than clearing them;
//! - Step retires exactly one instruction;
//! - Reset puts the machine back where it was before the first step;
//! - the breakpoint control toggles, and a line with no code says so;
//! - a control that cannot do what it was asked refuses with a *reason*, as a
//!   value, and records it;
//! - every function key maps to exactly one control, and a key with no control is
//!   not an error.

use lazalith_debug::{DebugController, ExecutionState, StopReason};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_gui::control::{Control, Controls, Outcome, Refusal};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program with a loop that prints at the end, so it can be stopped inside,
/// run to the end, and stepped through.
const SOURCE: &str = r#"fn main() -> i32 {
    let mut index: i64 = 0i64;
    let mut total: i64 = 0i64;
    while index < 5i64 {
        total = total + index;
        index = index + 1i64;
    }
    rt::sys::print("done\n");
    return 0;
}
"#;

/// The line the loop body is on.
const BODY_LINE: u32 = 5;

fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

fn build(source: &str) -> LzxImage {
    let program =
        RuntimeProgram::build(source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    LzxImage::from_bytes(&bytes).expect("the image reads back")
}

/// Controls with the program loaded and not yet run.
fn loaded() -> Controls<NoDevice> {
    loaded_with(SOURCE)
}

fn loaded_with(source: &str) -> Controls<NoDevice> {
    let config = ArchitectureConfig::lz64();
    let controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    let mut controls = Controls::new(controller);
    controls
        .load(build(source), PID, TID)
        .expect("the program loads");
    controls
}

/// The same, with one more instruction retired, so the program has moved.
///
/// `load` already performs the supervisor handoff, so this is a step of the
/// program's own code.
fn running() -> Controls<NoDevice> {
    let mut controls = loaded();
    controls.dispatch(Control::Step);
    controls
}

/// Run runs the program to its end and says so.
#[test]
fn run_runs_the_program_to_its_end() {
    let mut controls = running();
    let outcome = controls.dispatch(Control::Run);
    let Outcome::Ran { reason, steps } = outcome else {
        panic!("run reported {outcome:?}");
    };
    assert!(
        matches!(reason, StopReason::Exit { code: 0 }),
        "the program ran to its own exit: {reason:?}"
    );
    assert!(steps > 0, "and it retired instructions to get there");
    assert_eq!(
        controls.state(),
        Some(ExecutionState::Exited { code: 0 }),
        "which is the state the session has"
    );
}

/// Run stops at a breakpoint, at the instruction the breakpoint is on.
#[test]
fn run_stops_at_a_breakpoint() {
    let mut controls = running();
    let set = controls.dispatch(Control::BreakAtLine {
        name: "main.lz",
        line: BODY_LINE,
    });
    let Outcome::LineBreakpointsSet { addresses, .. } = set else {
        panic!("setting a line breakpoint reported {set:?}");
    };
    assert!(!addresses.is_empty(), "the line has code");
    let outcome = controls.dispatch(Control::Run);
    let Outcome::Ran { reason, .. } = outcome else {
        panic!("run reported {outcome:?}");
    };
    let StopReason::Breakpoint { address } = reason else {
        panic!("run stopped for {reason:?}, not at a breakpoint");
    };
    assert!(
        addresses.contains(&address),
        "and it stopped at one of the addresses the line resolved to: {address:#x} \
         is not among {addresses:?}"
    );
    assert_eq!(
        controls.state(),
        Some(ExecutionState::Stopped { address }),
        "so the session says the program is stopped there"
    );
}

/// Continue runs past the breakpoints instead of clearing them.
#[test]
fn continue_runs_past_the_breakpoints() {
    let mut controls = running();
    controls.dispatch(Control::BreakAtLine {
        name: "main.lz",
        line: BODY_LINE,
    });
    let outcome = controls.dispatch(Control::ContinueRun);
    let Outcome::Ran { reason, .. } = outcome else {
        panic!("continue reported {outcome:?}");
    };
    assert!(
        matches!(reason, StopReason::Exit { code: 0 }),
        "continue ran past the breakpoint to the end: {reason:?}"
    );
    assert!(
        !controls
            .controller()
            .session(PID)
            .expect("the session")
            .breakpoints()
            .is_empty(),
        "and it did so by ignoring them rather than by deleting them: a \
         Continue that cleared the breakpoints would leave the user with a \
         debugger that had forgotten where they were"
    );
}

/// Step retires exactly one instruction.
#[test]
fn step_retires_exactly_one_instruction() {
    let mut controls = running();
    let before = controls.controller().registers().pc();
    let outcome = controls.dispatch(Control::Step);
    let Outcome::Stepped { address } = outcome else {
        panic!("step reported {outcome:?}");
    };
    assert_ne!(address, before, "the program counter moved");
    let outcome = controls.dispatch(Control::Step);
    let Outcome::Stepped { address: second } = outcome else {
        panic!("the second step reported {outcome:?}");
    };
    assert_ne!(second, address, "and a second step moved it again");
}

/// Reset puts the machine back where it was before the first step.
#[test]
fn reset_puts_the_machine_back() {
    // The entry point is read from a *fresh* load rather than from the controls
    // after stepping, because reset goes back to where the program started and
    // not to wherever it happened to be one step ago.
    let entry = loaded().controller().registers().pc();
    let mut controls = running();
    assert_ne!(
        controls.controller().registers().pc(),
        entry,
        "the program moved away from its entry"
    );
    for _ in 0..40 {
        controls.dispatch(Control::Step);
    }
    assert_ne!(
        controls.controller().registers().pc(),
        entry,
        "and further away still"
    );
    let outcome = controls.dispatch(Control::Reset);
    let Outcome::Reset { address } = outcome else {
        panic!("reset reported {outcome:?}");
    };
    assert_eq!(
        address, entry,
        "and reset put it back at the program's entry point"
    );
    assert_eq!(
        controls.controller().registers().pc(),
        entry,
        "which is what the controller reports too"
    );
}

/// Reset after a run to the end also comes back.
#[test]
fn reset_comes_back_from_a_finished_program() {
    let mut controls = running();
    controls.dispatch(Control::Run);
    assert_eq!(
        controls.state(),
        Some(ExecutionState::Exited { code: 0 }),
        "the program finished"
    );
    let outcome = controls.dispatch(Control::Reset);
    let Outcome::Reset { .. } = outcome else {
        panic!("reset reported {outcome:?}");
    };
    assert!(
        controls.can_advance(),
        "and the program can be run again, because a reset really is a reset \
         and not a 'you may not do that now'"
    );
}

/// The breakpoint control toggles at the program counter.
#[test]
fn the_breakpoint_control_toggles_at_the_program_counter() {
    let mut controls = running();
    let pc = controls.controller().registers().pc();
    let outcome = controls.dispatch(Control::ToggleBreakpoint);
    assert_eq!(
        outcome,
        Outcome::BreakpointSet { address: pc },
        "the first press sets a breakpoint where the program is"
    );
    assert!(
        controls
            .controller()
            .session(PID)
            .expect("the session")
            .is_breakpoint(pc),
        "and the session has it"
    );
    let outcome = controls.dispatch(Control::ToggleBreakpoint);
    assert_eq!(
        outcome,
        Outcome::BreakpointCleared { address: pc },
        "the second press clears it"
    );
    assert!(
        !controls
            .controller()
            .session(PID)
            .expect("the session")
            .is_breakpoint(pc),
        "and the session does not"
    );
}

/// A line with no code is a real answer, not a silent nothing.
#[test]
fn a_line_with_no_code_reports_that_it_has_none() {
    let mut controls = running();
    let outcome = controls.dispatch(Control::BreakAtLine {
        name: "main.lz",
        line: 900,
    });
    assert_eq!(
        outcome,
        Outcome::LineBreakpointsSet {
            line: 900,
            addresses: Vec::new()
        },
        "the control reports the line and that nothing resolved to it"
    );
    assert!(
        controls
            .diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.code == Refusal::NoCodeOnLine { line: 900 }.code()),
        "and records a diagnostic saying why, because a user who pressed a \
         breakpoint key and saw nothing happen would otherwise think it is broken"
    );
}

/// An image with no debug information cannot have a line breakpoint.
#[test]
fn a_line_breakpoint_without_debug_information_is_refused() {
    let config = ArchitectureConfig::lz64();
    let controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    let with_debug = build(SOURCE);
    let stripped = LzxImage::new(
        with_debug.architecture(),
        0,
        0,
        with_debug.required_data(),
        with_debug.required_stack(),
        with_debug.sections().to_vec(),
    )
    .expect("an image without debug information is valid");
    let mut controls = Controls::new(controller);
    controls
        .load(stripped, PID, TID)
        .expect("the program loads");
    let outcome = controls.dispatch(Control::BreakAtLine {
        name: "main.lz",
        line: BODY_LINE,
    });
    assert_eq!(
        outcome,
        Outcome::Refused {
            reason: Refusal::NoDebugInformation
        },
        "a line cannot be found in an image that carries no lines"
    );
    assert!(
        controls
            .diagnostics()
            .entries()
            .iter()
            .any(|entry| entry.code == Refusal::NoDebugInformation.code()),
        "and the diagnostic says so"
    );
}

/// Clearing breakpoints says how many there were.
#[test]
fn clearing_breakpoints_says_how_many_there_were() {
    let mut controls = running();
    controls.dispatch(Control::BreakAtLine {
        name: "main.lz",
        line: BODY_LINE,
    });
    controls.dispatch(Control::ToggleBreakpoint);
    let count = controls
        .controller()
        .session(PID)
        .expect("the session")
        .breakpoints()
        .len();
    assert!(count >= 1, "there were some to clear");
    let outcome = controls.dispatch(Control::ClearBreakpoints);
    assert_eq!(outcome, Outcome::BreakpointsCleared { count });
    assert!(
        controls
            .controller()
            .session(PID)
            .expect("the session")
            .breakpoints()
            .is_empty(),
        "and there are none now"
    );
}

/// A control with nothing loaded refuses, with a reason.
#[test]
fn a_control_with_nothing_loaded_refuses() {
    let config = ArchitectureConfig::lz64();
    let controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    let mut controls = Controls::new(controller);
    assert!(!controls.can_advance());
    for control in [
        Control::Run,
        Control::ContinueRun,
        Control::Step,
        Control::Pause,
        Control::Reset,
        Control::ToggleBreakpoint,
        Control::ClearBreakpoints,
    ] {
        let outcome = controls.dispatch(control);
        assert_eq!(
            outcome,
            Outcome::Refused {
                reason: Refusal::NoProcess
            },
            "{control:?} with nothing loaded refuses rather than pretending"
        );
        assert_eq!(outcome.refusal(), Some(Refusal::NoProcess));
    }
}

/// A finished program refuses to be run again, and says which state it is in.
#[test]
fn a_finished_program_refuses_to_run_again() {
    let mut controls = running();
    controls.dispatch(Control::Run);
    for control in [Control::Run, Control::ContinueRun, Control::Step] {
        let outcome = controls.dispatch(control);
        assert_eq!(
            outcome,
            Outcome::Refused {
                reason: Refusal::AlreadyExited { code: 0 }
            },
            "{control:?} on a program that has exited"
        );
    }
    assert!(
        !controls.can_advance(),
        "and the frontend can tell in advance, so it can grey the controls out"
    );
}

/// A pause is a request, and the next run takes it at once.
#[test]
fn a_pause_is_taken_by_the_next_run() {
    let mut controls = running();
    let outcome = controls.dispatch(Control::Pause);
    let Outcome::PauseRequested { .. } = outcome else {
        panic!("pause reported {outcome:?}");
    };
    let before = controls.controller().registers().pc();
    let outcome = controls.dispatch(Control::Run);
    let Outcome::Ran { reason, steps } = outcome else {
        panic!("run reported {outcome:?}");
    };
    assert_eq!(
        reason,
        StopReason::Pause,
        "the run stopped because of the pause rather than at a breakpoint"
    );
    assert!(
        steps <= 1,
        "and it stopped at the first instruction boundary, after {steps} \
         instructions: a pause that let a thousand instructions run is not a pause"
    );
    assert_eq!(
        controls.controller().registers().pc(),
        before,
        "at the instruction it was asked from"
    );
}

/// Every bound key maps back to the control it was bound to.
#[test]
fn every_key_maps_back_to_its_control() {
    for control in [
        Control::Run,
        Control::ContinueRun,
        Control::Step,
        Control::Pause,
        Control::Reset,
        Control::ToggleBreakpoint,
    ] {
        let scancode = control
            .scancode()
            .unwrap_or_else(|| panic!("{control:?} is a control and is bound to a key"));
        assert_eq!(
            Control::from_scancode(scancode),
            Some(control),
            "{control:?}'s key maps back to it"
        );
    }
}

/// A key with no control is not an error.
#[test]
fn a_key_with_no_control_is_nothing() {
    // Escape is a key the frontend does not bind, and pressing it must be
    // harmless: a debugger that complained about every key it did not use would
    // make the guest's own keyboard unusable through the window.
    assert_eq!(Control::from_scancode(1), None);
}
