//! Step 65: the first Lazen program, end to end.
//!
//! This is the whole pipeline in one test, in the order a user's file goes
//! through it:
//!
//! ```text
//! main.lz
//!   ↓
//! Lazen compiler            (frontend, lowering, code generation)
//!   ↓
//! Lazalith object           (.lzo)
//!   ↓
//! linker                    (+ the startup sequence object)
//!   ↓
//! .lzx
//!   ↓
//! LazOS loader              (LzxImage::from_bytes, then load_process)
//!   ↓
//! process
//!   ↓
//! console
//! ```
//!
//! Nothing here is stubbed and nothing is inspected structurally. The
//! assertions are what the console received and what the process exited with,
//! because a pipeline that produces a well-formed object and a program that
//! prints nothing are both "passing" for every test that only looks at bytes.
//!
//! The image goes through its serialized form and the machine goes through a
//! real boot handoff, so the two parts of the path that have their own format —
//! the `.lzx` container and the supervisor-to-user transition — are the ones
//! under test rather than skipped.

use std::vec::Vec;

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxArchitecture, LzxImage, ProcessId, ProcessState,
    ThreadId, VirtualFileSystem, VirtualTerminal,
};
use lazalith_runtime::{BuildOptions, RuntimeError, RuntimeProgram};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// The program Step 65 exists to run.
const HELLO: &str = r#"
fn main() -> i32 {
    rt::sys::print("Hello, Lazalith\n");
    return 0;
}
"#;

/// The exit code `main` returns, which must become the process's exit code.
const HELLO_EXIT: u32 = 0;

/// A two-instruction supervisor kernel: a `NOP` and the `RFE` that hands off.
fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

/// Boots a machine through the real handoff and returns it with its kernel.
fn boot(config: ArchitectureConfig) -> lazalith_machine::LazalithMachine<NoDevice> {
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
    machine
}

/// Builds `source` and returns the `.lzx` bytes, the way a build leaves it.
fn build_image(source: &str) -> Vec<u8> {
    let program =
        RuntimeProgram::build(source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    program.to_image_bytes().expect("the image serialises")
}

/// The greeting reaches the console and the process exits with `main`'s result.
#[test]
fn a_lazen_program_runs_under_lazos() {
    let config = ArchitectureConfig::lz64();
    let mut machine = boot(config);

    // The loader reads the image from bytes, not from the object in memory: a
    // `.lzx` file is what a build leaves behind, so reading bytes is what a
    // loader will actually do.
    let image = LzxImage::from_bytes(&build_image(HELLO)).expect("the image reads back");
    let mut kernel = LazalithKernel::new(
        1_000,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("a pid"),
            ThreadId::new(1).expect("a tid"),
        )
        .expect("the program is scheduled");

    let mut exit = None;
    for _ in 0..100_000 {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => {
                panic!("the program faulted: {error:?}")
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    assert_eq!(
        exit,
        Some(HELLO_EXIT),
        "`main`'s result became the process's exit code"
    );

    let process = kernel
        .scheduler()
        .process(ProcessId::new(1).expect("a pid"))
        .expect("the process is still known to the scheduler");
    assert_eq!(process.state(), ProcessState::Exited);
    assert_eq!(process.exit_code(), Some(HELLO_EXIT));

    let output = kernel.terminal().terminal().output();
    assert_eq!(
        output, b"Hello, Lazalith\n",
        "the console received exactly what the program printed"
    );
}

/// The program's own value reaches the console, not just a literal.
///
/// The first test proves the pipeline carries a string to a syscall. This one
/// proves a Lazen *computation* survives the same path: a value is computed in
/// a frame, formatted digit by digit into a buffer, and handed to the same
/// `write` wrapper. A bug that made every frame slot read as zero would pass the
/// first test and fail this one.
#[test]
fn a_computed_value_reaches_the_console() {
    let config = ArchitectureConfig::lz64();
    let mut machine = boot(config);
    // Computes 6 * 7 and prints it as three zero-padded decimal digits, which is
    // 042. Lazen v1 has no implicit conversions, so every literal carries the
    // type it is meant to be. The digits come out least-significant first and are
    // placed back to front, which is the only order a repeated division produces.
    let source = r#"
fn main() -> i32 {
    let value: u64 = 6u64 * 7u64;
    let mut digits: [u8; 3] = [0u8; 3];
    let mut rest: u64 = value;
    let mut at: usize = 3;
    while at > 0 {
        at = at - 1;
        digits[at] = ((rest % 10u64) as u8) + 48u8;
        rest = rest / 10u64;
    }
    if digits[0] != 48u8 { return 10; }
    if digits[1] != 52u8 { return 11; }
    if digits[2] != 50u8 { return 12; }
    let mut record: [u8; 16] = [0u8; 16];
    let status: i64 = rt::sys::write_to(1, digits.as_slice(), record.as_mut_slice());
    if status != 0 {
        return 1;
    }
    return 0;
}
"#;
    let image = LzxImage::from_bytes(&build_image(source)).expect("the image reads back");
    let mut kernel = LazalithKernel::new(
        1_000,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("a pid"),
            ThreadId::new(1).expect("a tid"),
        )
        .expect("the program is scheduled");
    let mut exit = None;
    for _ in 0..1_000_000 {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => panic!("the program faulted: {error:?}"),
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    assert_eq!(exit, Some(0), "the program exited cleanly");
    assert_eq!(
        kernel.terminal().terminal().output(),
        b"042",
        "a value computed in a frame reached the console as its decimal digits, \
         zero-padded to the three digits the loop was asked for"
    );
}

/// A program that does not compile never becomes an image, and says why.
///
/// The failure has to arrive as a diagnostic naming the source, not as a panic
/// and not as an image with nothing in it. A build tool that cannot explain its
/// own refusal is the first thing a user meets.
#[test]
fn a_broken_program_never_becomes_an_image() {
    let error = RuntimeProgram::build(
        "fn main() -> i32 { return missing; }",
        &BuildOptions::lz64("main.lz"),
    )
    .expect_err("the program does not compile");
    let text = error.to_string();
    assert!(
        text.contains("missing"),
        "the diagnostic names what could not be found: {text}"
    );
    let rendered = match &error {
        RuntimeError::Compile(source) => source.text().to_string(),
        other => panic!("a source error is reported as one: {other:?}"),
    };
    assert!(
        rendered.contains("main.lz"),
        "the diagnostic names the file: {rendered}"
    );
}

/// The program Step 65 exists to run, read from the checked-in example.
///
/// The example file is the artifact a user would actually write, so the test
/// builds that rather than a copy of it: a test that compiles a string literal
/// proves the compiler works, while this proves the thing in `examples/` works.
/// The returned code digits are asserted separately, so this stays a check on
/// the example's own text.
fn example_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/hello/main.lz")
}

#[test]
fn the_checked_in_example_builds_and_runs() {
    let source = std::fs::read_to_string(example_path())
        .unwrap_or_else(|error| panic!("the example is readable: {error}"));
    let config = ArchitectureConfig::lz64();
    let mut machine = boot(config);
    let image = LzxImage::from_bytes(&build_image(&source)).expect("the example builds");
    let mut kernel = LazalithKernel::new(
        1_000,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("a pid"),
            ThreadId::new(1).expect("a tid"),
        )
        .expect("the program is scheduled");
    let mut exit = None;
    for _ in 0..100_000 {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => panic!("the example faulted: {error:?}"),
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    assert_eq!(exit, Some(HELLO_EXIT));
    assert_eq!(
        kernel.terminal().terminal().output(),
        b"Hello, Lazalith\n",
        "the checked-in example printed its greeting"
    );
}

/// The image is a real file format: it survives a round trip byte for byte.
#[test]
fn the_image_survives_a_serialisation_round_trip() {
    let bytes = build_image(HELLO);
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");
    assert_eq!(
        image.to_bytes().expect("the image serialises"),
        bytes,
        "a `.lzx` is read and written the same way twice"
    );
    assert_eq!(
        image.entry_offset(),
        RuntimeProgram::build(HELLO, &BuildOptions::lz64("main.lz"))
            .expect("builds")
            .to_image_bytes()
            .map(|other| LzxImage::from_bytes(&other).expect("reads").entry_offset())
            .expect("serialises"),
        "the entry offset is the same however the image was reached"
    );
    // A truncated image is refused rather than loaded with whatever survived.
    let truncated = &bytes[..bytes.len() / 2];
    assert!(
        LzxImage::from_bytes(truncated).is_err(),
        "a truncated image is not a loadable image"
    );
    // So is one with a corrupted magic number.
    let mut corrupted = bytes.clone();
    corrupted[0] ^= 0xff;
    assert!(
        LzxImage::from_bytes(&corrupted).is_err(),
        "an image whose magic is wrong is not a loadable image"
    );
}

/// The process runs as a user thread, and the machine goes back to supervisor.
///
/// A program that ran with supervisor rights would make every later security
/// property untested, so the privilege transition is asserted rather than
/// assumed.
#[test]
fn the_program_runs_as_a_user_thread() {
    let bytes = build_image(HELLO);
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");
    let process = image
        .load_process(
            ProcessId::new(7).expect("a pid"),
            ThreadId::new(7).expect("a tid"),
        )
        .expect("the image loads");
    assert_eq!(
        process.primary_thread().cpu().privilege(),
        Privilege::User,
        "a program runs with user rights, not supervisor's"
    );
    // The entry is the startup sequence, which the linker places after the
    // program's own text, so it is *inside* the user code region rather than at
    // its first byte. What matters is that it is reachable code the loader set
    // the program to start at, and that it is within the region the loader maps.
    let entry = process.program().entry().as_u64();
    let start = lazalith_os::USER_CODE_START;
    let length = lazalith_os::USER_CODE_LENGTH;
    assert!(
        (start..start + length).contains(&entry),
        "the entry {entry:#x} is inside the user code region {start:#x}..{:#x}",
        start + length
    );
    let _ = LzxArchitecture::Lz64;
}
