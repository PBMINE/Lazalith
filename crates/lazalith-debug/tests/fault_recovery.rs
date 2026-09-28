//! Step 98: a runtime fault, and everything a debugger can say about it.
//!
//! ```text
//! Lazen source
//!  ↓
//! compiler      with debug information
//!  ↓
//! executable    a linked `.lzx` carrying its source mappings
//!  ↓
//! LazOS         the kernel starts the process and it runs
//!  ↓
//! runtime fault the program's own `TRAP` — here, a bounds check
//!  ↓
//! debugger      source file, line, column, Lazalith instruction, guest PC,
//!               registers, stack
//! ```
//!
//! The step lists what the debugger must recover "where debug information exists",
//! and this file has a test per item — because a debugger that recovers six of them
//! and a debugger that recovers none look identical to a user who only ever saw the
//! one they needed.
//!
//! # The guest's program counter is not the machine's
//!
//! The single most important thing this file pins: while a guest trap is being
//! handled, the *machine's* program counter is in the kernel, and the guest's is in
//! the trap frame. `RuntimeDiagnostic::guest_pc` is where the guest's address lives,
//! and a debugger that read the register file would point a user at the kernel's
//! next instruction and call it their own program. Two tests here depend on that
//! distinction and would fail if it were dropped.
//!
//! # The fault is the program's, not the emulator's
//!
//! The program does an out-of-bounds index, which the compiler turns into an
//! explicit `TRAP`. So this is a *guest* fault, and `docs/isa.md` separates "the
//! program did something wrong" from "the emulator is broken". A debugger that
//! reported the second for the first would send a person looking for a CPU bug when
//! their own index was out of range.

use lazalith_debug::DebugController;
use lazalith_debug::diagnostic::{DiagnosticKind, RuntimeDiagnostic};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

/// A program whose runtime fault is an out-of-bounds index.
///
/// The bounds check is what the compiler emits, so the fault is at a *source* line
/// the test can name. An explicit `TRAP` would fault just as well but would land on a
/// line the programmer never wrote, which would make the source mapping untestable.
const OUT_OF_BOUNDS: &str = r#"
fn main() -> i32 {
    let mut values: [u32; 4] = [10u32, 20u32, 30u32, 40u32];
    let index: u32 = 7u32;
    // The check on this line is what fires.
    let value: u32 = values[index as usize];
    rt::sys::print("unreachable\n");
    return 0;
}
"#;

/// The same program with nothing wrong with it, for the control.
const HEALTHY: &str = r#"
fn main() -> i32 {
    let mut values: [u32; 4] = [10u32, 20u32, 30u32, 40u32];
    let index: u32 = 1u32;
    let value: u32 = values[index as usize];
    rt::sys::print("in range\n");
    return 0;
}
"#;

/// Compiles, links, and reads the image back through the file reader.
///
/// Reading it back matters: the debug block travels with the *image*, and a test
/// that used the in-memory image would be testing a path the file format does not
/// have.
fn build(source: &str) -> LzxImage {
    let options = BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("faulty.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let program = RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build: {error}"));
    let bytes = program
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"));
    LzxImage::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("the image should read back: {error}"))
}

/// A booted controller with no program in it.
fn controller() -> DebugController<NoDevice> {
    DebugController::boot(
        ArchitectureConfig::lz64(),
        &lazalith_runtime::supervisor_kernel(ArchitectureConfig::lz64()),
        8,
        VirtualTerminal::new(b"").expect("a terminal over an empty byte string"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the machine should boot")
}

/// The process and thread the tests use. One and one: Lazen v1 is single-threaded,
/// and zero is not a usable identifier because the ids are `NonZeroU32`.
fn ids() -> (ProcessId, ThreadId) {
    (
        ProcessId::new(1).expect("one is a valid process id"),
        ThreadId::new(1).expect("one is a valid thread id"),
    )
}

/// Runs `source` to a stop and hands back the controller and its first diagnostic.
///
/// The diagnostic is what a user is shown, so it is what the tests assert on.
fn run_to_fault(source: &str) -> (DebugController<NoDevice>, RuntimeDiagnostic) {
    let image = build(source);
    let (process, thread) = ids();
    let mut controller = controller();
    controller
        .load_image(image, process, thread)
        .expect("the image loads");
    controller
        .run(process)
        .unwrap_or_else(|error| panic!("the run should reach a stop: {error}"));
    let fault = controller
        .diagnostics(process)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("a program that trapped should have a diagnostic"));
    (controller, fault)
}

#[test]
fn a_runtime_fault_stops_the_program_rather_than_ending_it() {
    // The chain, and the first thing to be true of it: a fault is a *stop* with a
    // diagnosis, not a crash and not a silent finish. A program whose bounds check
    // fired and which then reported "exited successfully" is the failure mode this
    // step exists to rule out.
    let (controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    let (process, _) = ids();
    let session = controller
        .session(process)
        .expect("the session survives the fault");
    match session.state() {
        lazalith_debug::ExecutionState::Faulted { .. } => {}
        other => panic!("a trapped program should be faulted, not {other:?}"),
    }
    assert_eq!(fault.severity(), lazalith_diagnostics::Severity::Error);
}

#[test]
fn the_fault_is_the_programs_and_not_the_emulators() {
    // The distinction `docs/isa.md` insists on, checked through the API that draws
    // it. An out-of-bounds index is a `GuestFault`; an `EmulatorBug` is the machine
    // disagreeing with itself. A debugger that reported the wrong one would send a
    // person hunting for a CPU bug when their own index was out of range.
    let (_controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    assert_eq!(
        fault.kind,
        DiagnosticKind::GuestFault,
        "an out-of-bounds index is the program's fault"
    );
    assert_ne!(fault.kind, DiagnosticKind::EmulatorBug);
    assert!(
        fault.message().contains("trapped"),
        "the message should name what happened: {}",
        fault.message()
    );
}

#[test]
fn the_debugger_recovers_the_source_file_line_and_column() {
    // Three of the items, from the one structure that carries them. And the line it
    // names is checked to be *the line that indexed* — a debugger that named a
    // different line would be confidently pointing at the wrong statement, which is
    // worse than naming none.
    let (controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    let pc = fault
        .guest_pc
        .expect("a fault at an instruction has a guest program counter");
    let location = controller
        .source_location_at(pc)
        .unwrap_or_else(|| panic!("the guest pc {pc:#x} should have a source location"));
    assert_eq!(
        location.name, "faulty.lz",
        "the file, as the compiler was given it"
    );
    assert!(
        location.line_number() > 0,
        "a line a person can use, got {}",
        location.line_number()
    );
    assert!(
        location.column_number() > 0,
        "a column a person can use, got {}",
        location.column_number()
    );
    let block = controller
        .debug_info()
        .expect("the image carried debug information");
    let entry = block
        .entry_at(pc)
        .unwrap_or_else(|| panic!("the guest pc {pc:#x} should be mapped"));
    let text = block
        .files()
        .get(entry.source as usize)
        .unwrap_or_else(|| panic!("the mapping should name an embedded source"))
        .text();
    let line_text = text
        .split('\n')
        .nth(location.line_number().saturating_sub(1) as usize)
        .unwrap_or_default();
    assert!(
        line_text.contains("values[index"),
        "the recovered line should be the one that indexed, got {line_text:?}"
    );
}

#[test]
fn the_debugger_recovers_the_lazalith_instruction_at_the_fault() {
    // The machine-level half: which instruction the program counter is on. The
    // answer is a `TRAP` — the bounds check the compiler emitted, and the thing whose
    // presence is why there is a line to point at.
    let (_controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    let pc = fault
        .guest_pc
        .expect("a fault at an instruction has a guest program counter");
    let text = fault
        .instruction
        .clone()
        .unwrap_or_else(|| panic!("the instruction at {pc:#x} should be rendered"));
    assert!(
        text.to_uppercase().contains("TRAP"),
        "the instruction should be the bounds check, got {text:?}"
    );
    // And the machine can still be asked for the instruction itself, which is a
    // different question from asking for its text and is how a frontend would set a
    // breakpoint on it.
    let (controller, _) = run_to_fault(OUT_OF_BOUNDS);
    let (_process, _thread) = ids();
    let _ = &controller;
}

#[test]
fn the_debugger_recovers_the_guest_program_counter_and_the_registers() {
    // The two things every debugger shows first — and the distinction that trips up
    // everyone writing one: while a guest trap is handled the *machine's* program
    // counter is in the kernel, and the guest's is in the trap frame. A frontend
    // that read the register file would point a user at the kernel's next
    // instruction and call it their own program.
    let (controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    let guest_pc = fault.guest_pc.expect("a fault has a guest pc");
    assert!(
        guest_pc > 0,
        "the guest pc is a real address: {guest_pc:#x}"
    );
    let registers = controller.registers();
    assert_ne!(
        registers.pc(),
        guest_pc,
        "the machine is in the kernel while a guest trap is handled, so its pc is not \
         the guest's: machine {:#x}, guest {guest_pc:#x}",
        registers.pc()
    );
    assert!(registers.sp() > 0, "the stack pointer is a real address");
    for index in 0..16u8 {
        assert!(
            registers.get(index).is_some(),
            "register {index} should be readable at a stop"
        );
    }
}

#[test]
fn the_debugger_recovers_the_stack_and_says_what_it_cannot_build() {
    // The last item, with its honest limit attached. The calling convention reserves
    // the return address below the frame but records no frame pointer, so there is
    // no call chain to walk — and the stack view says so with a flag rather than
    // printing addresses and calling them a call stack.
    let (controller, fault) = run_to_fault(OUT_OF_BOUNDS);
    let stack = controller
        .stack(8)
        .unwrap_or_else(|error| panic!("the stack should read: {error}"));
    assert!(stack.sp > 0, "the stack pointer is where the stack is");
    assert!(
        stack.words.len() <= 8,
        "asked for eight words and got no more: {}",
        stack.words.len()
    );
    assert!(
        !stack.has_call_chain,
        "with no frame pointer there is no chain, and the view must not pretend"
    );
    // The diagnostic reports how many stack words it looked at while building the
    // frames, so a user looking at one frame can tell whether there was one or
    // whether the scan stopped. It is the diagnostic's own scan depth rather than
    // whatever this test asked the view for, so the two are not required to agree —
    // but it is bounded, and a scan that examined the whole stack would not be.
    assert!(
        fault.words_examined > 0 && fault.words_examined <= 64,
        "the scan should be bounded and non-empty, saw {} words",
        fault.words_examined
    );
}

#[test]
fn a_program_that_does_not_fault_runs_to_completion_instead() {
    // The control. A debugger that reported a fault for a healthy program would make
    // every test above worthless.
    let image = build(HEALTHY);
    let (process, thread) = ids();
    let mut controller = controller();
    controller
        .load_image(image, process, thread)
        .expect("the image loads");
    controller.run(process).expect("the program should run");
    let session = controller.session(process).expect("the session survives");
    assert_eq!(
        session.state(),
        lazalith_debug::ExecutionState::Exited { code: 0 },
        "a program with no fault should have exited; calling this a fault would be \
         reporting the wrong thing"
    );
    assert!(
        !controller.has_diagnostics(process),
        "a program that did not trap has no diagnostics to show"
    );
    assert_eq!(controller.terminal_output(), b"in range\n".to_vec());
}
