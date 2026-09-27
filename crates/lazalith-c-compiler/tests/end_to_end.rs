//! A C program, all the way to the machine.
//!
//! The other test file checks that the compiler *produces* the right IR. This one
//! checks the thing that matters more: that a C program runs. Everything between
//! the source and the process's exit status is the real pipeline — the same
//! `generate`, the same linker, the same `.lzx` reader, the same boot image, the
//! same kernel, the same interpreter — so a failure here is a failure of the
//! machine's C support and not of a mock.

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_c_compiler::compile;
use lazalith_c_compiler::ir::lower;
use lazalith_codegen::{CodegenOptions, generate};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ThreadId, VirtualFileSystem,
    VirtualTerminal,
};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// How many instructions a program may retire before it is called a runaway.
///
/// A single `return 42` is a few dozen. A million is several orders of magnitude
/// above anything that works and far below "waited too long", and a program that
/// exceeds it is spinning — which is a bug worth failing on rather than waiting
/// for.
const STEP_BUDGET: u64 = 1_000_000;

/// The object symbol a C program's `main` reaches, which the entry sequence calls.
///
/// The C compiler prefixes its functions with `c.` because the IR has one flat
/// namespace, and the native backend prefixes that with `fn.`. Both prefixes are
/// the point: a C function called `write` must not be the ABI's `write`.
const C_MAIN: &str = "fn.c.main";

/// A two-instruction supervisor kernel: a `NOP` and the `RFE` that hands off.
fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(
            config,
            &Instruction::new(config, Opcode::Nop, &[]).expect("a NOP encodes"),
        )
        .expect("the NOP encodes"),
        encode(
            config,
            &Instruction::new(config, Opcode::Rfe, &[]).expect("an RFE encodes"),
        )
        .expect("the RFE encodes"),
    ]
    .concat()
}

/// What a finished run produced.
struct Finished {
    exit_code: u32,
    output: Vec<u8>,
}

/// Compiles, links and runs a C program, and reports what it did.
///
/// The `.lzx` image is serialised and read back before it is run, because a
/// `.lzx` file is what a build leaves on disk and a loader that only works on
/// the in-memory image has not been tested against the one a user gets.
fn run(source: &str) -> Finished {
    let config = ArchitectureConfig::lz64();
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = compile(&mut sources, "t.c", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    let lowered = lower(&checked).unwrap_or_else(|error| panic!("{source} should lower: {error}"));
    assert_eq!(
        lowered.entry, "c.main",
        "the entry is the C `main`, and the C compiler's own prefix is the IR's"
    );
    let program = generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions::lz64("t.c"),
        source,
    )
    .unwrap_or_else(|error| panic!("{source} should generate code: {error}"));
    let startup = lazalith_runtime::startup_object_for(config, C_MAIN)
        .expect("the entry sequence assembles for a C entry symbol");
    let linked = lazalith_toolchain::link_objects(
        &[program.object().clone(), startup],
        &lazalith_toolchain::LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("{source} should link: {error}"));
    let bytes = linked
        .image()
        .to_bytes()
        .unwrap_or_else(|error| panic!("{source} should serialise: {error}"));
    let image = LzxImage::from_bytes(&bytes)
        .unwrap_or_else(|error| panic!("{source} should read back: {error}"));

    let boot = BootImage::new(config, supervisor_kernel(config), 0).expect("a boot image");
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    assert_eq!(
        machine.architectural_state().privilege(),
        Privilege::Supervisor,
        "the machine starts in supervisor mode, before the handoff"
    );
    machine.step().expect("the kernel's first step");

    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("one is a valid process id"),
            ThreadId::new(1).expect("one is a valid thread id"),
        )
        .expect("the program is scheduled");

    let mut exit_code = None;
    for _ in 0..STEP_BUDGET {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit_code = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => {
                panic!("{source} faulted in the kernel: {error:?}")
            }
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                // A fault says *what* kind and nothing about *where*, and a test
                // that fails with "Alignment" alone costs an afternoon. The
                // registers and the machine's own last-fault record say which
                // access and which address.
                let registers: Vec<String> = (0..8)
                    .map(|index| {
                        format!(
                            "r{index}={:#x}",
                            machine
                                .architectural_state()
                                .registers()
                                .read_raw(index)
                                .unwrap_or(0)
                        )
                    })
                    .collect();
                panic!(
                    "{source} trapped ({cause:?}, payload {payload})\n  \
                     pc={:#x} sp={:#x}\n  {}\n  last fault: {:?}",
                    machine.architectural_state().pc().as_u64(),
                    machine.architectural_state().sp().as_u64(),
                    registers.join(" "),
                    machine.last_trap_fault(),
                );
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    let exit_code =
        exit_code.unwrap_or_else(|| panic!("{source} did not finish in {STEP_BUDGET} steps"));
    let output = kernel.terminal().terminal().output().to_vec();
    Finished { exit_code, output }
}

/// A returned constant is the process's exit status.
///
/// This is the shortest program that reaches every stage: the source is lexed,
/// parsed, resolved, checked, lowered, verified, generated, linked, serialised,
/// read back, loaded and executed.
#[test]
fn a_returned_constant_is_the_programs_exit_status() {
    assert_eq!(run("int main(void) { return 42; }").exit_code, 42);
}

/// Arithmetic works, which is the shortest program that proves the pipeline:
/// a load, an add, a store and a return.
#[test]
fn arithmetic_works_end_to_end() {
    let finished = run("int main(void) { int a = 20; int b = 22; return a + b; }");
    assert_eq!(finished.exit_code, 42);
}

/// A `while` loop runs the right number of times.
///
/// A loop one iteration short or long would still pass a test that only checked an
/// addition, so the count is checked directly.
#[test]
fn a_loop_runs_the_right_number_of_times() {
    let finished = run(
        "int main(void) { int n = 0; int i = 0; while (i < 10) { i = i + 1; n = n + i; } return n; }",
    );
    assert_eq!(finished.exit_code, 55, "one through ten is fifty-five");
}

/// A `for` loop's initialiser, condition and step are all reached.
#[test]
fn a_for_loop_runs_its_body() {
    let finished = run(
        "int main(void) { int n = 0; for (int i = 0; i < 5; i = i + 1) { n = n + 2; } return n; }",
    );
    assert_eq!(finished.exit_code, 10);
}

/// A function is called and its result comes back through the ABI.
#[test]
fn a_function_is_called_and_its_result_returns() {
    let finished = run("int twice(int x) { return x + x; } int main(void) { return twice(21); }");
    assert_eq!(finished.exit_code, 42);
}

/// A `break` leaves a loop and a `continue` skips to its step.
#[test]
fn break_and_continue_reach_the_right_places() {
    let broken = run(
        "int main(void) { int n = 0; int i = 0; while (1) { i = i + 1; if (i == 4) { break; } n = n + 1; } return n; }",
    );
    assert_eq!(broken.exit_code, 3, "three iterations before the break");
    let continued = run(
        "int main(void) { int n = 0; int i = 0; while (i < 5) { i = i + 1; if (i == 3) { continue; } n = n + 1; } return n; }",
    );
    assert_eq!(continued.exit_code, 4, "five iterations, one skipped");
}

/// A global is stored once in its own segment and read back.
#[test]
fn a_global_round_trips_through_its_segment() {
    let finished = run("int counter = 7; int main(void) { return counter * 6; }");
    assert_eq!(finished.exit_code, 42);
}

/// A string literal is written to the terminal through the ABI's `write`.
///
/// This is the one test that leaves the machine. A C program's only way to be
/// seen is a syscall, so this proves the whole path: a string in C, a data
/// segment in the object, a relocation the linker filled in, the `SYSCALL`
/// instruction, the kernel's `write`, and a byte in the terminal.
#[test]
fn a_string_literal_reaches_the_terminal() {
    let finished = run(
        "int main(void) { int result; write(1, \"hello from C\\n\", 12, &result, 0); return 7; }",
    );
    assert_eq!(finished.exit_code, 7, "and the program still exits cleanly");
    // The twelve bytes asked for, which are `hello from C` and *not* the
    // newline: the program wrote twelve bytes of a thirteen-byte string, so the
    // line feed stays in the program's own memory. Writing the whole string and
    // expecting the newline would be asserting something about the terminal's
    // line handling, which this test is not about.
    assert!(
        finished
            .output
            .windows(12)
            .any(|window| window == b"hello from C"),
        "the string reaches the terminal\n  bytes: {:?}",
        finished.output
    );
}
/// A byte-by-byte walk of a local array is the one test that would notice a
/// `char` that was four bytes wide: the walk would read past the end of the
/// array and the string it built would have four copies of every character.
#[test]
fn a_char_is_one_byte_at_run_time() {
    let finished = run(r#"
        int main(void) {
            char text[4];
            text[0] = 'a'; text[1] = 'b'; text[2] = 'c'; text[3] = 0;
            int length = 0;
            while (text[length] != 0) { length = length + 1; }
            return length;
        }
        "#);
    assert_eq!(finished.exit_code, 3, "three characters and a null");
}
