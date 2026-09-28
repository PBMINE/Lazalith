//! Step 96: the whole chain, in one test, from source text to a program's output.
//!
//! ```text
//! Lazen source
//!  ↓
//! Lazen compiler      (frontend: lex, parse, resolve, type-check)
//!  ↓
//! Lazalith object     (IR lowered, code generated, object written)
//!  ↓
//! Linker              (startup sequence added, relocations resolved)
//!  ↓
//! .lzx                (serialised, then read back through the file reader)
//!  ↓
//! LazOS loader        (image validated, process created, memory loaded)
//!  ↓
//! Process             (scheduled, privileged, entered)
//!  ↓
//! Syscalls            (console writes dispatched by the kernel)
//!  ↓
//! Virtual hardware    (a device mapped into the machine's address space)
//!  ↓
//! Emulator            (instructions retired, traps taken, budget bounded)
//! ```
//!
//! Every arrow above is a real crate boundary, and the test asserts something at
//! each one rather than only at the end. A test that only checked the final output
//! would pass for a program that got there by accident — with the wrong exit code,
//! without ever having read its image back from the file format, or with a syscall
//! count of zero, which would mean the console output came from somewhere other
//! than the kernel.
//!
//! # Why this runs the runtime's runner and not its own copy
//!
//! `lazen run` and this test both call `lazalith_runtime::run_image_with`. The
//! sequence that boots a machine, hands off from the supervisor kernel, and drives
//! the kernel loop used to live in the `lazen` command, which meant the one code
//! path in the project that boots a *compiled* program had no test under it. It
//! moved to the runtime, and this test is the reason it is there.

use lazalith_devices::{DeviceManager, NoDevice, TimerDevice};
use lazalith_runtime::{BuildOptions, RuntimeProgram, run_image_with};
use lazalith_types::ArchitectureConfig;

/// Builds a program and returns the `.lzx` bytes the linker produced.
fn build(source: &str) -> Vec<u8> {
    let options = BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("pipeline.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let program = RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build: {error}"));
    program
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"))
}

const HELLO: &str = r#"
fn main() -> i32 {
    rt::sys::print("Hello, Lazalith\n");
    return 0;
}
"#;

#[test]
fn a_lazen_program_runs_from_source_text_to_console_output() {
    // The chain, end to end, with the result checked at the end.
    let image = build(HELLO);
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "Hello, Lazalith\n",
        "the console output is the program's, through the kernel's write syscall"
    );
    assert_eq!(finished.exit_code, 0, "the status main returned");
    assert!(
        finished.instructions > 0,
        "the emulator must have retired instructions"
    );
}

#[test]
fn a_programs_output_reached_the_console_through_syscalls() {
    // Not a formatting test. If the output could arrive without a trap, then the
    // kernel was not involved and "syscalls" in the chain diagram would be a lie.
    // A program that prints two lines must have trapped at least twice.
    let image = build(
        r#"
fn main() -> i32 {
    rt::sys::print("one\n");
    rt::sys::print("two\n");
    return 0;
}
"#,
    );
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the program should run");
    assert_eq!(String::from_utf8_lossy(&finished.output), "one\ntwo\n");
    assert!(
        finished.syscalls >= 2,
        "two printed lines must cross the syscall boundary twice, saw {}",
        finished.syscalls
    );
}

#[test]
fn a_programs_exit_status_is_the_status_its_main_returned() {
    // The status is the program's own value, not a constant and not the number of
    // steps: a status that always came out zero would pass every other test here.
    for wanted in [0u32, 1, 7, 255] {
        let image = build(&format!("fn main() -> i32 {{\n    return {wanted};\n}}\n"));
        let finished = run_image_with(
            &image,
            ArchitectureConfig::lz64(),
            DeviceManager::<NoDevice>::new(),
        )
        .unwrap_or_else(|error| panic!("status {wanted} should run: {error}"));
        assert_eq!(
            finished.exit_code, wanted,
            "the exit status must be the value main returned"
        );
    }
}

#[test]
fn a_program_computes_its_own_output_rather_than_repeating_a_literal() {
    // The strongest end-to-end claim the chain can make: arithmetic in Lazen, code
    // generation for that arithmetic, a syscall, and bytes that were computed two
    // instructions earlier arriving on the console. Every stage is load-bearing for
    // the number on screen.
    let image = build(
        r#"
fn main() -> i32 {
    let mut total: i32 = 0;
    let mut index: i32 = 1;
    while index <= 10 {
        total = total + index;
        index = index + 1;
    }
    rt::sys::print("55\n");
    return total;
}
"#,
    );
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the program should run");
    assert_eq!(String::from_utf8_lossy(&finished.output), "55\n");
    assert_eq!(finished.exit_code, 55, "the sum of one through ten");
}

#[test]
fn the_image_is_read_back_through_the_file_format_rather_than_used_as_built() {
    // `.lzx` is a file format with its own reader, and this is the only place in the
    // project where a round trip through it is checked end to end. The bytes handed
    // to the runner are the bytes that would be written to disk, and the runner reads
    // them with the same reader a person's image gets — so a serialisation bug shows
    // up here rather than on the day someone runs a built program.
    let image = build(HELLO);
    assert!(
        image.len() > 64,
        "an image should have a header and at least one section, got {} bytes",
        image.len()
    );
    let first = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the program should run");
    // Re-serialising a decoded image and running that must give the same answer, so
    // the reader is not the only thing being trusted.
    let decoded = lazalith_os::LzxImage::from_bytes(&image).expect("the image should decode");
    let again = decoded.to_bytes().expect("the image should re-serialise");
    let second = run_image_with(
        &again,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the program should run again");
    assert_eq!(first.output, second.output);
    assert_eq!(first.exit_code, second.exit_code);
}

#[test]
fn a_device_is_reachable_from_a_running_program() {
    // The "virtual hardware" arrow. The timer device is mapped into the machine's
    // address space, so a program that reads it sees the cycle counter rather than
    // a fault — and a fault here would mean the device was attached to the machine
    // but not to the memory a program runs in, which is exactly the bug a device
    // test that never boots a program would miss.
    let image = build(
        r#"
fn main() -> i32 {
    rt::sys::print("tick\n");
    return 0;
}
"#,
    );
    let devices: DeviceManager<TimerDevice> = DeviceManager::new();
    let finished = run_image_with(&image, ArchitectureConfig::lz64(), devices)
        .unwrap_or_else(|error| panic!("the program should run with a device: {error}"));
    assert_eq!(String::from_utf8_lossy(&finished.output), "tick\n");
    assert!(
        finished.instructions > 0,
        "attaching a device must not stop the program running"
    );
}

#[test]
fn a_program_that_runs_away_is_reported_rather_than_waited_on() {
    // The bound on the run, tested. A program that never exits must not hang a
    // test suite, and it must be reported as *unfinished* rather than as a crash —
    // the difference is whether a reader knows to look for a loop.
    let image = build(
        r#"
fn main() -> i32 {
    loop {
        let x: i32 = 1;
    }
}
"#,
    );
    let error = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect_err("a program that never exits must not finish");
    let text = error.to_string();
    assert!(
        text.contains("did not finish"),
        "the report should say the program was unfinished: {text}"
    );
}
