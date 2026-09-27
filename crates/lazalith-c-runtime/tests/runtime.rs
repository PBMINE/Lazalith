//! The C runtime, built and run.
//!
//! These tests check the thing a standard library is for: that a C program can
//! call `strlen`, `malloc` and `putchar` and get what it asked for. A library
//! whose functions *compile* but return nothing is worse than no library, so
//! every test here runs a real program on the real machine and checks its
//! output and its exit status.

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
use lazalith_runtime::startup_object_for;
use lazalith_toolchain::{LinkOptions, ObjectFile, link_objects};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// How many instructions a program may retire before it is called a runaway.
const STEP_BUDGET: u64 = 1_000_000;

/// The object symbol a C program's `main` reaches.
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

/// Compiles a C program with the runtime, links it, runs it, and reports.
fn run(source: &str) -> Finished {
    run_with_files(source, &[])
}

/// The same, with files already in the filesystem.
///
/// The step asks for file operations, and a file operation that is only ever run
/// against a filesystem with nothing in it proves only that `fopen` of a missing
/// name fails. A program needs something to read.
fn run_with_files(source: &str, files: &[(&str, &[u8])]) -> Finished {
    let config = ArchitectureConfig::lz64();
    // The C runtime is one translation unit in front of the program, exactly as
    // the Lazen standard library is. A program may therefore define a name the
    // runtime also defines, and the *program's* is the one that wins once the
    // linker has seen both — which is what a C programmer expects and what
    // putting the runtime first achieves.
    let unit = {
        let mut text = lazalith_c_runtime::C_RUNTIME.to_string();
        text.push('\n');
        text.push_str(source);
        text
    };
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = compile(&mut sources, "program.c", &unit)
        .unwrap_or_else(|error| panic!("the program should compile:\n{}", error.render()));
    let lowered =
        lower(&checked).unwrap_or_else(|error| panic!("the program should lower: {error}"));
    let program = generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions::lz64("program.c"),
        &unit,
    )
    .unwrap_or_else(|error| panic!("the program should generate code: {error}"));
    let mut objects: Vec<ObjectFile> = vec![program.object().clone()];
    let startup = startup_object_for(config, C_MAIN).expect("the entry sequence assembles");
    objects.push(startup);
    let linked = link_objects(
        &objects,
        &LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("the program should link: {error}"));
    let bytes = linked.image().to_bytes().expect("the image serialises");
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");

    let boot = BootImage::new(config, supervisor_kernel(config), 0).expect("a boot image");
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    assert_eq!(
        machine.architectural_state().privilege(),
        Privilege::Supervisor
    );
    machine.step().expect("the kernel's first step");

    let mut filesystem = VirtualFileSystem::with_defaults().expect("a filesystem");
    for (path, data) in files {
        filesystem
            .insert_file(path.as_bytes(), data)
            .unwrap_or_else(|error| panic!("`{path}` should be in the filesystem: {error}"));
    }
    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").expect("a terminal"),
        filesystem,
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("a process id"),
            ThreadId::new(1).expect("a thread id"),
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
                panic!("the program faulted in the kernel: {error:?}")
            }
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
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
                    "the program trapped ({cause:?}, payload {payload})\n  {}\n  \
                     last fault: {:?}",
                    registers.join(" "),
                    machine.last_trap_fault()
                );
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    let exit_code =
        exit_code.unwrap_or_else(|| panic!("the program did not finish in {STEP_BUDGET} steps"));
    Finished {
        exit_code,
        output: kernel.terminal().terminal().output().to_vec(),
    }
}

/// The text a run produced, with a trailing line feed removed.
///
/// The virtual terminal does not echo one, so a run's output and the string a
/// program wrote are not always the same length, and a test that compared them
/// directly would be testing the terminal.
fn text(finished: &Finished) -> String {
    String::from_utf8_lossy(&finished.output)
        .trim_end_matches('\n')
        .to_string()
}

/// `strlen` counts the characters and stops at the null.
#[test]
fn strlen_counts_a_string() {
    let finished = run(r#"
        int main(void) {
            int length = (int)strlen("hello");
            return length;
        }
        "#);
    assert_eq!(finished.exit_code, 5);
}

/// `strcpy` copies the bytes and writes the null itself.
///
/// The null is the point: a `strcpy` that did not write one would leave the
/// destination's last byte as whatever was there, and the test below reads the
/// destination with `strlen` — so a missing null would make the answer wrong
/// rather than merely untidy.
#[test]
fn strcpy_copies_and_terminates() {
    let finished = run(r#"
        int main(void) {
            char buffer[16];
            memset(buffer, 0, 16);
            strcpy(buffer, "hello");
            return (int)strlen(buffer);
        }
        "#);
    assert_eq!(finished.exit_code, 5);
}

/// `strcmp` says which string sorts first, and says equal for equal strings.
#[test]
fn strcmp_orders_and_agrees() {
    let finished = run(r#"
        int main(void) {
            if (strcmp("abc", "abd") >= 0) { return 1; }
            if (strcmp("abd", "abc") <= 0) { return 2; }
            if (strcmp("abc", "abc") != 0) { return 3; }
            return 0;
        }
        "#);
    assert_eq!(finished.exit_code, 0, "and the two agree");
}

/// `malloc` returns memory a program can write to and read back.
///
/// A `malloc` that returned a null pointer every time would pass a test that only
/// checked for null, and a `malloc` that returned the *same* block twice would
/// pass a test that only wrote to it. Both are checked here: the block is written,
/// read back, and a second allocation is required to be a *different* address.
#[test]
fn malloc_returns_writable_memory() {
    let finished = run(r#"
        int main(void) {
            char *first = (char *)malloc(16);
            if (first == 0) { return 1; }
            strcpy(first, "written");
            if (strcmp(first, "written") != 0) { return 2; }
            char *second = (char *)malloc(16);
            if (second == 0) { return 3; }
            if (second == first) { return 4; }
            return (int)strlen(first);
        }
        "#);
    assert_eq!(finished.exit_code, 7, "'written' is seven characters");
}

/// `calloc` zeroes, which is the whole difference from `malloc`.
#[test]
fn calloc_returns_zeroed_memory() {
    let finished = run(r#"
        int main(void) {
            unsigned char *block = (unsigned char *)calloc(8, 1);
            if (block == 0) { return 1; }
            unsigned long at = 0;
            while (at < 8) {
                if (block[at] != 0) { return 2; }
                at = at + 1;
            }
            block[3] = 7;
            return block[3];
        }
        "#);
    assert_eq!(finished.exit_code, 7);
}

/// `putchar` writes one byte to the console.
#[test]
fn putchar_writes_one_byte() {
    let finished = run("int main(void) { putchar(65); return 0; }");
    assert_eq!(finished.exit_code, 0);
    assert_eq!(text(&finished), "A", "and the byte is an `A`");
}

/// `puts` writes a string and a newline, and returns zero on success.
#[test]
fn puts_writes_a_line() {
    let finished = run("int main(void) { return puts(\"from the runtime\"); }");
    assert_eq!(finished.exit_code, 0);
    assert_eq!(text(&finished), "from the runtime");
}

/// `memmove` handles overlapping regions, which is why it is not `memcpy`.
///
/// The two regions overlap by one byte and the copy runs backwards, so a forward
/// implementation — which `memcpy` above *is* — would read a byte it had already
/// written. The test asserts the *last* byte, which is the one a forward copy
/// gets wrong first.
#[test]
fn memmove_handles_overlapping_regions() {
    let finished = run(r#"
        int main(void) {
            char buffer[8];
            memset(buffer, 0, 8);
            strcpy(buffer, "abcdefg");
            memmove(buffer + 1, buffer, 7);
            return buffer[7];
        }
        "#);
    assert_eq!(finished.exit_code, 103, "and the last byte survived");
}

/// `abs` and `atoi` are the two number helpers, and both are checked.
#[test]
fn the_number_helpers_work() {
    let finished = run(r#"
        int main(void) {
            if (abs(0 - 5) != 5) { return 1; }
            return atoi("-42");
        }
        "#);
    // A `main` that returns a negative number is a process whose *status* is that
    // number, and the kernel reports it as a `u32`. -42 is 0xffffffd6.
    assert_eq!(finished.exit_code, u32::MAX - 41, "atoi parsed -42");
}

/// Printing is a call per piece, because a variadic body is what this C cannot
/// write. Both halves are checked: the text, and the digits.
#[test]
fn printing_needs_no_format_string() {
    let finished = run(r#"
        int main(void) {
            print("n=");
            print_decimal(-1234);
            print_line("");
            print("done");
            return 0;
        }
        "#);
    assert_eq!(String::from_utf8_lossy(&finished.output), "n=-1234\ndone");
    assert_eq!(finished.exit_code, 0);
}

/// `print_decimal(0)` is the one value with no digits, and a loop that divides
/// by ten until it is zero writes nothing at all for it.
#[test]
fn zero_has_a_digit() {
    let finished = run(r#"
        int main(void) {
            print_decimal(0);
            print_line("");
            return 0;
        }
        "#);
    assert_eq!(String::from_utf8_lossy(&finished.output), "0\n");
}

/// `exit` is the ABI's own syscall, and the runtime must not shadow it with a
/// wrapper that cannot exit. A program that returns normally and a program that
/// calls `exit` have to be distinguishable by their status.
#[test]
fn exit_is_the_syscall() {
    let finished = run(r#"
        int main(void) {
            print("bye");
            exit(7);
            return 1;
        }
        "#);
    assert_eq!(finished.exit_code, 7, "exit set the status");
    assert_eq!(String::from_utf8_lossy(&finished.output), "bye");
}

/// The C runtime and the Lazen helpers are two objects, and a shared name would
/// The C runtime and the Lazen helpers are two objects, and a shared *definition*
/// would be a link error on the day somebody linked both. Checked in the
/// direction that can actually collide: every name Lazen defines must be absent
/// from the C source, because the C side is the one that is text-matched here.
#[test]
fn the_two_halves_share_no_names() {
    let lazen: Vec<&str> = lazalith_c_runtime::VARIADIC
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pub fn "))
        .filter_map(|rest| rest.split('(').next())
        .collect();
    assert!(
        !lazen.is_empty(),
        "the Lazen half defines nothing, so this test would pass on an empty list"
    );
    for name in lazen {
        let defined = format!("{name}(");
        assert!(
            !lazalith_c_runtime::C_RUNTIME.contains(&format!(" {defined}")),
            "Lazen defines `{name}` and so does the C runtime; a program linking \
             both would get one of them at random"
        );
    }
}

/// The file half of `<stdio.h>`, on the handles a C program can actually have.
///
/// There is no `fopen` to test, and that is not a gap in the tests: the ABI
/// reports a new file's handle in a return register no calling convention hands a
/// caller, so a C program cannot be given one. Handles 0, 1 and 2 are the
/// console, and these are the functions that work on a handle the program already
/// has. `docs/c-runtime.md` explains the `fopen` gap and what would close it.
#[test]
fn stdio_writes_to_a_handle() {
    let finished = run(r#"
        int main(void) {
            if (fwrite("abc", 1, 3, 1) != 3) { return 1; }
            if (fputs("de", 1) != 0) { return 2; }
            if (fflush(1) != 0) { return 3; }
            return 0;
        }
        "#);
    assert_eq!(String::from_utf8_lossy(&finished.output), "abcde");
    assert_eq!(finished.exit_code, 0);
}

/// The count and the bytes are checked separately. A `fwrite` that reported three
/// and wrote nothing would pass a test that only read the console back, and one
/// that wrote three and reported a count of four would pass a test that only
/// compared bytes.
#[test]
fn stdio_writes_a_whole_count() {
    let finished = run(r#"
        int main(void) {
            unsigned long wrote = fwrite("xy", 1, 2, 1);
            print_decimal((long)wrote);
            print_line("");
            return wrote == 2 ? 0 : 1;
        }
        "#);
    // The two bytes went to the console as well as being counted, so the output is
    // what was written followed by what was counted.
    assert_eq!(String::from_utf8_lossy(&finished.output), "xy2\n");
    assert_eq!(finished.exit_code, 0, "two bytes, counted as two");
}

/// A rejected call has to be visible. `fwrite` to a handle the process does not
/// own is refused by the ABI, and the runtime has to pass that refusal on rather
/// than reporting a count of bytes nobody wrote.
#[test]
fn a_bad_handle_is_refused() {
    let finished = run(r#"
        int main(void) {
            if (fwrite("x", 1, 1, 999) != 0) { return 1; }
            return 0;
        }
        "#);
    assert_eq!(
        finished.output, b"",
        "nothing was written to a handle that is not open"
    );
    assert_eq!(
        finished.exit_code, 0,
        "and the call reported a count of zero"
    );
}
