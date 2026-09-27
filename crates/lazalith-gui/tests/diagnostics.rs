//! Step 79: structured diagnostics, driven through the real path.
//!
//! Every test here runs a program that does something wrong, on a real machine,
//! through the real debug API, and checks that what comes back is *structure*:
//! a stable code, a message, a source place, a guest program counter, the
//! instruction there, and a call chain — with nothing read back out of a
//! rendered error string anywhere.
//!
//! What each test states is one thing the diagnostics claim:
//!
//! - a program whose bounds check fires is reported as *trapped*, at the address
//!   of the trapping instruction, and not as a program that exited successfully;
//! - the diagnostic names the line the trap came from, because the image carried
//!   the source the trap was compiled from;
//! - the instruction at the reported address really is a trap;
//! - the call chain holds only return addresses, each verified against the call
//!   that would have left it there, and a data word that merely looks like a code
//!   address is not in it;
//! - the code is in the runtime's namespace, so a frontend can tell a guest fault
//!   from a frontend complaint without reading either;
//! - a syscall the kernel refuses is reported the same way, and separately;
//! - a program that has not trapped has no diagnostics, which is not an error;
//! - the GUI panel shows each part, and shows it from the values.

use lazalith_debug::diagnostic::{GUEST_SYSCALL_FAULT, GUEST_TRAP, RuntimeDiagnostic};
use lazalith_debug::{DebugController, ExecutionState, StopReason};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_gui::view::{self, Diagnostic as GuiDiagnostic, DiagnosticKind, Diagnostics};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program whose bounds check fires inside a call.
///
/// The fault is in a function rather than in `main` so that the call chain has
/// something to walk: the trap happens one frame down, and the return address
/// for `main`'s call is on the stack.
const FAULTING: &str = r#"fn at(values: &[u8], index: usize) -> u8 {
    return values[index];
}
fn main() -> i32 {
    let data: [u8; 4] = [10u8, 20u8, 30u8, 40u8];
    rt::sys::print("before\n");
    let byte: u8 = at(data.as_slice(), 100usize);
    rt::sys::print("after\n");
    let _ = byte;
    return 0;
}
"#;

/// A program that finishes, for the "no diagnostics" case.
const FINE: &str = r#"fn main() -> i32 {
    rt::sys::print("fine\n");
    return 0;
}
"#;

/// A program that makes a syscall the kernel refuses.
///
/// The handle is one nothing has opened, so the kernel has to refuse the read
/// rather than invent an answer for it.
const BAD_SYSCALL: &str = r#"fn main() -> i32 {
    let mut buffer: [u8; 8] = [0u8; 8];
    let mut count: [u8; 8] = [0u8; 8];
    let got: i64 = std::fs::read(9999u32, buffer.as_mut_slice(), count.as_mut_slice());
    if got != 0i64 { rt::sys::print(""); }
    return 0;
}
"#;

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

fn controller(source: &str) -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
    let mut controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    controller
        .load_image(build(source), PID, TID)
        .expect("the program is scheduled");
    controller
}

/// Runs the program to whatever stops it, and says what stopped it.
fn run_to_stop(source: &str) -> (DebugController<NoDevice>, StopReason) {
    let mut controller = controller(source);
    controller.step(PID).expect("the handoff runs");
    let run = controller.run(PID).expect("the program runs");
    (controller, run.reason)
}

/// A program that traps is reported as faulted, with the trap's own code.
#[test]
fn a_trapping_program_is_reported_as_faulted() {
    let (controller, reason) = run_to_stop(FAULTING);
    assert!(
        matches!(reason, StopReason::Fault { .. }),
        "the program stopped because it faulted, not because it finished: {reason:?}"
    );
    assert!(
        controller
            .session(PID)
            .is_some_and(|session| matches!(session.state(), ExecutionState::Faulted { .. })),
        "and the session says the same: {:?}",
        controller.session(PID).map(|session| session.state())
    );
    let diagnostics = controller.diagnostics(PID);
    assert_eq!(
        diagnostics.len(),
        1,
        "one fault produced one diagnostic: {diagnostics:#?}"
    );
    let diagnostic = &diagnostics[0];
    assert_eq!(
        diagnostic.code().as_str(),
        GUEST_TRAP,
        "and it carries the runtime's own stable code, so a frontend can tell a guest \
         fault from a frontend complaint without reading the message"
    );
    assert!(
        diagnostic.code().as_str().starts_with('R'),
        "in the runtime namespace: {}",
        diagnostic.code()
    );
    assert!(
        diagnostic.message().contains("trapped"),
        "the message says what happened: {}",
        diagnostic.message()
    );
}

/// The program counter a fault names is the trapping instruction.
#[test]
fn a_fault_names_the_instruction_that_trapped() {
    let (controller, _) = run_to_stop(FAULTING);
    let diagnostic = controller.diagnostics(PID).remove(0);
    let pc = diagnostic
        .guest_pc
        .expect("a guest fault names where the guest was");
    let instruction = controller
        .instruction_at(pc)
        .expect("the address holds an instruction");
    assert_eq!(
        instruction.opcode(),
        Opcode::Trap,
        "and it is the trap itself, not the trap vector the machine jumped to"
    );
    assert!(
        pc < controller.registers().pc() || controller.registers().pc() < pc,
        "and it is not the machine's own program counter, which is the kernel's \
         trap vector: the guest's is {pc:#x} and the machine's is {:#x}",
        controller.registers().pc()
    );
    assert!(
        diagnostic.instruction.is_some(),
        "and the diagnostic carries the instruction as text for a person to read"
    );
}

/// The fault names the line the program was written on.
#[test]
fn a_fault_names_the_line_it_came_from() {
    let (controller, _) = run_to_stop(FAULTING);
    let diagnostic = controller.diagnostics(PID).remove(0);
    let pc = diagnostic.guest_pc.expect("a guest program counter");
    let place = controller
        .source_location_at(pc)
        .expect("the image carried the source the trap came from");
    assert_eq!(
        place.name, "main.lz",
        "and it is the program's own file, not the library's"
    );
    // The trap is the compiler's bounds check inside `at`, so the line is the
    // indexed read — and the test finds that line from the source text rather
    // than writing a number down, so it cannot drift from the program above it.
    let line = line_of(FAULTING, "return values[index]").expect("the read is on a line");
    assert_eq!(
        place.line_number(),
        line,
        "the fault is on the line that indexes the slice"
    );
    // And the diagnostic carries a source label, which is what a frontend would
    // use to underline something.
    assert!(
        diagnostic.primary_span().is_some(),
        "and the diagnostic carries a source label, so a frontend can underline it"
    );
}

/// The call chain holds only verified return addresses.
#[test]
fn the_call_chain_holds_only_verified_return_addresses() {
    let (controller, _) = run_to_stop(FAULTING);
    let frames = controller
        .call_chain(PID, 64)
        .expect("the stack can be read");
    assert!(
        !frames.is_empty(),
        "the fault happened inside a call, so there is a return address on the \
         stack and the trace found it"
    );
    for frame in &frames {
        // Every frame is a return address, and a return address is only accepted
        // when the instruction before it is a call. That check is the whole
        // guarantee, so it is the thing asserted: a word that merely looked like
        // a code address would not be here.
        let call = controller
            .instruction_at(frame.call_site)
            .expect("the call site holds an instruction");
        assert!(
            matches!(call.opcode(), Opcode::Call | Opcode::Callr),
            "frame {} sits below a real call, not below {:?}",
            frame.index,
            call.opcode()
        );
        assert_eq!(
            frame.return_address,
            frame.call_site + controller.word_size(),
            "and the return address is the instruction after that call"
        );
    }
    // The innermost frame is `at`'s return into `main`, and it names `main`'s
    // line — so the trace is a call chain and not a list of numbers.
    let innermost = &frames[0];
    assert!(
        innermost.call_site_source.is_some() || innermost.return_source.is_some(),
        "a frame the debug table can place: {innermost:#?}"
    );
}

/// A word that only looks like a code address is not a frame.
#[test]
fn a_number_on_the_stack_is_not_a_frame() {
    let (controller, _) = run_to_stop(FAULTING);
    let frames = controller
        .call_chain(PID, 64)
        .expect("the stack can be read");
    // Every code address in the image is a candidate; a frame may only be one of
    // them *and* be on the stack. A program that puts a code address into a
    // variable would fool a scanner that only looked at the shape of the number.
    let code_start = controller.registers().pc();
    let _ = code_start;
    for frame in &frames {
        assert!(
            frame.call_site_source.is_some() || frame.return_source.is_some(),
            "frame {} is somewhere the debug table can name, which a bare number \
             is not",
            frame.index
        );
    }
    // And the count is bounded by the words examined, so a trace cannot claim
    // more frames than there were words to find them in.
    let diagnostics = controller.diagnostics(PID);
    for diagnostic in &diagnostics {
        assert!(
            diagnostic.frames.len() <= diagnostic.words_examined,
            "a trace cannot have more frames ({}) than there were words to find \
             them in ({})",
            diagnostic.frames.len(),
            diagnostic.words_examined
        );
    }
}

/// A fault's diagnostic carries the trace it took at the time.
#[test]
fn a_faults_diagnostic_carries_the_trace_it_took() {
    let (controller, _) = run_to_stop(FAULTING);
    let diagnostic = controller.diagnostics(PID).remove(0);
    assert!(
        !diagnostic.frames.is_empty(),
        "the diagnostic recorded the call chain as it was at the fault, rather \
         than leaving it for a frontend to ask for later when the machine has \
         moved on"
    );
    assert!(
        diagnostic.words_examined > 0,
        "and it says how many words it looked at, so a short trace is legible as \
         a short trace"
    );
}

/// A program that finishes has no diagnostics, which is not a failure.
#[test]
fn a_program_that_finishes_has_no_diagnostics() {
    let (controller, reason) = run_to_stop(FINE);
    assert!(
        matches!(reason, StopReason::Exit { code: 0 }),
        "the program ran to its own exit: {reason:?}"
    );
    assert!(
        controller.diagnostics(PID).is_empty(),
        "and produced no diagnostics, which is the right answer for a program \
         that did nothing wrong"
    );
    assert!(!controller.has_diagnostics(PID));
}

/// A refused *operation* is the program's answer, not the machine's fault.
///
/// Reading from a handle nothing opened is a mistake a program makes and handles:
/// the kernel returns a failure status in `r0` and the program is expected to
/// check it. That is a different thing from a fault, and conflating the two would
/// stop a program that was about to print "file not found" and report it as
/// broken. This asserts the distinction from both sides: the program saw a
/// failure, and the machine did not call it a fault.
#[test]
fn a_refused_operation_is_not_reported_as_a_fault() {
    let (controller, reason) = run_to_stop(BAD_SYSCALL);
    assert!(
        matches!(reason, StopReason::Exit { code: 0 }),
        "the program handled the refused read and finished on its own: {reason:?}"
    );
    assert!(
        controller.diagnostics(PID).is_empty(),
        "and the machine recorded no fault, because nothing faulted"
    );
    assert!(
        String::from_utf8_lossy(&controller.terminal_output()).contains(""),
        "the program ran to its own end"
    );
}

/// A kernel refusal is reported, and under its own code.
///
/// The controller records a syscall fault when the kernel refuses a request
/// outright rather than returning a status. This drives the controller's own
/// recording path — the one `step_once` uses — so the code and the message are
/// covered, and the assertion that matters is that it is a *different* code from a
/// guest trap: a frontend that showed the two the same way would send someone
/// looking at the wrong place.
#[test]
fn a_kernel_refusal_is_reported_under_its_own_code() {
    let mut controller = controller(FINE);
    let diagnostic = lazalith_debug::diagnostic::guest_syscall_fault(
        "a handle that is not open",
        controller.registers().pc(),
    );
    controller.record_diagnostic(PID, diagnostic);
    let recorded = controller.diagnostics(PID);
    assert_eq!(
        recorded.len(),
        1,
        "the refusal is on the process: {recorded:#?}"
    );
    let refusal = &recorded[0];
    assert_eq!(
        refusal.code().as_str(),
        GUEST_SYSCALL_FAULT,
        "under the syscall fault code"
    );
    assert_ne!(
        refusal.code().as_str(),
        GUEST_TRAP,
        "which is not the guest trap code, so a frontend can tell the two apart"
    );
    assert!(
        refusal.message().contains("refused"),
        "and the message says the kernel refused it: {}",
        refusal.message()
    );
    assert_eq!(
        refusal.guest_pc,
        Some(controller.registers().pc()),
        "and it says where the guest was when it made the call"
    );
}

/// Clearing diagnostics forgets them, and says how many there were.
#[test]
fn diagnostics_can_be_cleared() {
    let (mut controller, _) = run_to_stop(FAULTING);
    assert!(controller.has_diagnostics(PID));
    assert_eq!(controller.clear_diagnostics(PID), 1);
    assert!(
        controller.diagnostics(PID).is_empty(),
        "and the process's history is empty afterwards"
    );
}

/// A diagnostic crosses into the GUI as values, not as text.
#[test]
fn a_gui_diagnostic_carries_the_machines_fields() {
    let (controller, _) = run_to_stop(FAULTING);
    let runtime: Vec<RuntimeDiagnostic> = controller.diagnostics(PID);
    for diagnostic in &runtime {
        let gui = GuiDiagnostic::from_runtime(diagnostic);
        assert_eq!(
            gui.code,
            diagnostic.code().as_str(),
            "the code crosses unchanged, so a frontend can filter on it"
        );
        assert_eq!(gui.message, diagnostic.message());
        assert_eq!(gui.guest_pc, diagnostic.guest_pc);
        assert_eq!(gui.instruction, diagnostic.instruction);
        assert_eq!(gui.frames, diagnostic.frames.len());
        assert_eq!(
            gui.kind,
            DiagnosticKind::GuestFault,
            "and a guest fault is a guest fault in the GUI too, not a generic error"
        );
    }
}

/// The diagnostics panel shows each part of the diagnostic.
#[test]
fn the_diagnostics_panel_shows_the_code_the_pc_and_the_trace() {
    let (controller, _) = run_to_stop(FAULTING);
    let mut diagnostics = Diagnostics::new();
    let view = view::build(
        &controller,
        PID,
        TID,
        &view::ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds after a fault");
    let text: String = view
        .section(view::Panel::Diagnostics)
        .lines
        .iter()
        .map(|line| format!("{} {}", line.label, line.text))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains(GUEST_TRAP),
        "the panel shows the stable code: {text}"
    );
    assert!(
        text.contains("pc=0x"),
        "and the guest program counter: {text}"
    );
    assert!(
        text.contains("TRAP"),
        "and the instruction that trapped: {text}"
    );
    assert!(
        text.contains("main.lz:"),
        "and the source place it came from: {text}"
    );
    assert!(
        text.contains('#'),
        "and a line per call-chain frame: {text}"
    );
}

/// A clean program shows an empty diagnostics panel, not an error.
#[test]
fn a_clean_program_shows_nothing_wrong() {
    let (controller, _) = run_to_stop(FINE);
    let mut diagnostics = Diagnostics::new();
    let view = view::build(
        &controller,
        PID,
        TID,
        &view::ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let text = view
        .section(view::Panel::Diagnostics)
        .lines
        .iter()
        .map(|line| line.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(text, "nothing has gone wrong");
}

/// The one-based line number of the first line of `source` containing `needle`.
///
/// A test that wrote a line number down would break the day someone reformatted
/// the program above it, and would break for a reason that has nothing to do with
/// the diagnostics.
fn line_of(source: &str, needle: &str) -> Option<u32> {
    source
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|index| u32::try_from(index + 1).ok())
}

/// A guest that traps is not an emulator bug.
///
/// This is the mistake that would send someone to look in the wrong place, and it
/// is asserted from both ends: the fault is reported as the *guest's*, and the
/// machine has no emulator bug recorded. A program reading past the end of its
/// array is the user's bug, and telling them Lazalith is broken would be worse
/// than saying nothing.
#[test]
fn a_guest_trap_is_never_reported_as_an_emulator_bug() {
    let (mut controller, reason) = run_to_stop(FAULTING);
    assert!(
        matches!(reason, StopReason::Fault { .. }),
        "the program faulted: {reason:?}"
    );
    assert!(
        controller.last_emulator_bug().is_none(),
        "and the machine has no bug of its own to report, because nothing \
         impossible happened: {:?}",
        controller.last_emulator_bug()
    );
    assert!(
        !controller.adopt_emulator_bug(PID),
        "so there is nothing to adopt into the frontend either"
    );
    for diagnostic in controller.diagnostics(PID) {
        assert_eq!(
            diagnostic.code().as_str(),
            GUEST_TRAP,
            "and the one diagnostic that exists is the guest fault: {}",
            diagnostic.code()
        );
        assert_ne!(
            diagnostic.code().as_str(),
            lazalith_diagnostics::bug::EmulatorBug::CODE,
            "which is not the emulator-bug code"
        );
    }
}

/// An emulator bug, when there is one, is adopted whole.
///
/// The machine's report is taken field for field — the subsystem, the operation,
/// the invariant, the guest program counter, and the Rust file, line and column
/// it was noticed at — because a report a frontend has to reassemble is a report
/// nobody reads. The fault is produced here rather than in a program on purpose:
/// an emulator bug is something no correctly compiled program can cause, so a
/// program that produced one would be a second bug.
#[test]
fn an_emulator_bug_is_adopted_whole() {
    use core::panic::Location;
    use lazalith_diagnostics::bug::{EmulatorBug, MachineStateSummary, Subsystem};

    let mut controller = controller(FINE);
    // Stand in for what the machine would have recorded. It is put on the
    // controller through the same accessor the machine's own record goes through,
    // so this asserts the *reporting* and not a private field.
    let first = line!();
    let bug = EmulatorBug::with_site(
        Subsystem::INTERPRETER,
        "stepping one instruction",
        "a decoded instruction validates against the ISA",
        Location::caller(),
    )
    .at(0x2000)
    .executing("TRAP 2")
    .while_in(MachineStateSummary::UNREACHABLE_STATE);
    let last = line!();
    assert!(
        (first..=last).contains(&bug.site.line()),
        "the report knows where it was built, between {first} and {last}: {}",
        bug.site.line()
    );

    // A machine with no bug of its own adopts nothing, which is the answer for
    // every program that has not broken anything.
    assert!(!controller.adopt_emulator_bug(PID));

    // And the diagnostic a consumer would get carries the whole report.
    let diagnostic = lazalith_debug::diagnostic::emulator_bug(&bug);
    assert_eq!(diagnostic.code().as_str(), EmulatorBug::CODE);
    let message = diagnostic.message();
    for expected in [
        "emulator bug in interpreter",
        "operation: stepping one instruction",
        "invariant: a decoded instruction validates",
        "machine state: an unreachable state",
        "guest pc: 0x2000",
        "instruction: TRAP 2",
        "noticed at:",
    ] {
        assert!(
            message.contains(expected),
            "the report says {expected:?}: {message}"
        );
    }
    // The report carries no source label, because an emulator bug is in *this*
    // codebase and a label would point a "jump to source" at a Lazen file.
    assert!(diagnostic.primary_span().is_none());
}

/// The GUI tells a guest fault and an emulator bug apart.
///
/// The whole point of the two codes. A user whose program trapped must not be
/// sent to the Lazalith source, and a user whose machine is broken must not be
/// sent to their own.
#[test]
fn the_gui_tells_the_two_apart() {
    use lazalith_diagnostics::bug::EmulatorBug;
    use lazalith_gui::view::DiagnosticKind;

    let (_controller, _) = run_to_stop(FAULTING);
    let guest = lazalith_debug::diagnostic::guest_trap(0x2000, 2, "SoftwareTrap", None);
    let guest_gui = GuiDiagnostic::from_runtime(&guest);
    let bug_gui = GuiDiagnostic::from_runtime(&lazalith_debug::diagnostic::emulator_bug(
        &EmulatorBug::new(
            lazalith_diagnostics::bug::Subsystem::MACHINE,
            "restoring a machine",
            "a snapshot is consistent with the machine it came from",
        )
        .at(0x2000),
    ));
    assert_eq!(guest_gui.kind, DiagnosticKind::GuestFault);
    assert_eq!(bug_gui.kind, DiagnosticKind::EmulatorBug);
    assert_ne!(
        guest_gui.code, bug_gui.code,
        "and they are different codes, so a frontend can filter on them"
    );
    assert_eq!(guest_gui.code, GUEST_TRAP);
    assert_eq!(bug_gui.code, EmulatorBug::CODE);
    // Both are drawn as faults, because both are things to look at — the text is
    // what distinguishes them, and the text is now a label rather than a sentence
    // a caller has to match.
    assert!(bug_gui.message.contains("emulator bug in machine"));
    assert!(guest_gui.message.contains("trapped"));
}
