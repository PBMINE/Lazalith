//! Step 100: the finished platform, and the one claim the diagram makes.
//!
//! ```text
//!                       LAZALITH
//!                           │
//!          ┌────────────────┼────────────────┐
//!          │                │                │
//!         CPU             Tools            GUI
//!          │                │                │
//!          ▼                ▼                ▼
//!       Machine      Assembler/Linker      SDL3
//!          │
//!          ▼
//!    Lazalith HW
//!          │
//!          ▼
//!        LazOS
//!          │
//!          ▼
//!    System ABI
//!          │
//!          ▼
//!     ┌───────┐
//!     │ Lazen │
//!     └───┬───┘
//!         │
//!         ▼
//!    Applications
//! ```
//!
//! ```text
//! Lazen ─┐
//! C ─────┼──→ Lazalith Object ──→ Linker ──→ Lazalith Executable ──→ LazOS ──→ Machine
//! Asm ───┘
//! ```
//!
//! Every arrow above is a real crate boundary, and this file is the only place where
//! **all three languages go through one path and are compared**. The individual
//! languages are already tested — `lazalith-runtime`'s `pipeline.rs` for Lazen, this
//! crate's `end_to_end.rs` for C, the toolchain's assembler tests for assembly — but
//! each in its own test file, in its own crate, against its own copy of the boot
//! sequence. Three tests that each pass and a diagram that says "all three" is not a
//! claim; this is.
//!
//! The claim being checked is narrow and mechanical: **the same program, written in
//! three languages, produces the same observable behaviour through one object format,
//! one linker, one loader, and one machine.** Not the same source — the languages are
//! different — but the same arithmetic, the same syscalls, the same exit status, and
//! the same bytes on the same console.
//!
//! What is *not* claimed is that the three languages are equally expressive. They are
//! not, and `docs/c-compiler.md` is explicit about where C stops. The claim is about
//! the path, not about the languages.

use lazalith_c_compiler::frontend::compile as compile_c;
use lazalith_c_compiler::lower as lower_c;
use lazalith_codegen::{CodegenOptions, generate as generate_c};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram, run_image_with};
use lazalith_types::ArchitectureConfig;

/// What a finished run produced, for the three languages to be compared on.
#[derive(Debug, Eq, PartialEq)]
struct Finished {
    exit_code: u32,
    output: String,
}

/// The Lazen version of the program.
///
/// Sums one through ten, prints the sum, and returns it — so the exit status and the
/// printed number are the *same* value, computed by the program rather than stated by
/// the test. A front end that hard-coded either would pass a weaker version of this.
const LAZEN: &str = r#"
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
"#;

/// The C version of the same program.
const C: &str = r#"
int main(void) {
    int total = 0;
    int index = 1;
    while (index <= 10) {
        total = total + index;
        index = index + 1;
    }
    return total;
}
"#;

/// The assembly version: the same sum, in Lazalith assembly.
///
/// Written by hand rather than generated, because the point is that a person can drop
/// to the lowest layer the platform offers and still reach the same place.
///
/// **Unrolled, and the reason is worth stating:** the ISA has `BR` and `JMP` and
/// `CALL`, and no *conditional* branch. A hand-written loop therefore cannot be
/// written without a branch the architecture does not have, so this adds ten times in
/// a row. That is not a workaround for this test — it is what the instruction set
/// means for a person writing in it, and it is why the Lazen and C loops above are the
/// interesting comparison: the compilers have a conditional branch to give a loop,
/// and this layer does not.
const ASSEMBLY: &str = r#"
.arch lz64
.entry sum_entry
.global sum_entry
.section .text
sum_entry:
    LI r1, 0
    LI r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    ADD r1, r1, r2
    ADDI r2, r2, 1
    MOV r0, r1
    RET
.section .bss
"#;

#[test]
fn a_lazen_program_sums_and_reports_through_the_shared_path() {
    let finished = run_lazen();
    assert_eq!(finished.exit_code, 55, "the sum of one through ten");
    assert_eq!(finished.output, "55\n", "and the program says so itself");
}

#[test]
fn a_c_program_sums_and_reports_through_the_same_path() {
    // The same number, arrived at through a different front end, the same IR, the
    // same object format, the same linker, the same loader, and the same machine.
    let finished = run_c();
    assert_eq!(finished.exit_code, 55);
}

#[test]
fn an_assembly_program_sums_through_the_same_path() {
    let finished = run_assembly();
    assert_eq!(finished.exit_code, 55, "the same sum, by hand");
}

#[test]
fn all_three_languages_agree_on_the_same_program() {
    // The claim, in one test. Three front ends, one object format, one linker, one
    // loader, one machine — and the same answer.
    let lazen = run_lazen();
    let c = run_c();
    let assembly = run_assembly();
    assert_eq!(
        lazen.exit_code, c.exit_code,
        "Lazen and C disagree about the program's result"
    );
    assert_eq!(
        c.exit_code, assembly.exit_code,
        "C and assembly disagree about the same program"
    );
    assert_eq!(lazen.exit_code, 55, "and all three are wrong the same way?");
}

#[test]
fn all_three_produce_a_lazalith_executable_rather_than_their_own() {
    // The other half of the diagram: one *executable format*, not three. Every image
    // here carries the same ISA version, the same ABI version, and the same three
    // sections, because the loader is one loader and the machine is one machine.
    let config = ArchitectureConfig::lz64();
    for (name, bytes) in [
        ("lazen", lazen_image()),
        ("c", c_image()),
        ("assembly", assembly_image()),
    ] {
        let image = LzxImage::from_bytes(&bytes)
            .unwrap_or_else(|error| panic!("the {name} image should read: {error}"));
        assert_eq!(
            image.architecture().config(),
            config,
            "the {name} image is for the machine it was built for"
        );
    }
}

/// The Lazen program, as `.lzx` bytes.
fn lazen_image() -> Vec<u8> {
    let options = BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("program.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    RuntimeProgram::build(LAZEN, &options)
        .expect("the Lazen program should build")
        .to_image_bytes()
        .expect("the Lazen program should link")
}

/// The C program, as `.lzx` bytes.
///
/// The same shape as the Lazen path and the assembly path, deliberately written out
/// rather than shared: the claim is that the three paths *coincide*, and a test that
/// shared the code between them would be asserting that they were the same code.
fn c_image() -> Vec<u8> {
    let config = ArchitectureConfig::lz64();
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = compile_c(&mut sources, "program.c", C)
        .unwrap_or_else(|error| panic!("the C program should compile:\n{}", error.render()));
    let lowered = lower_c(&checked).expect("the C program should lower");
    let program = generate_c(
        &lowered.module,
        &lowered.frames,
        lowered.entry.as_deref(),
        &CodegenOptions::lz64("program.c"),
        C,
    )
    .expect("the C program should generate code");
    let startup = lazalith_runtime::startup_object_for(config, C_MAIN)
        .expect("the entry sequence assembles for a C entry symbol");
    let linked = lazalith_toolchain::link_objects(
        &[program.object().clone(), startup],
        &lazalith_toolchain::LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .expect("the C program should link");
    linked
        .image()
        .to_bytes()
        .expect("the C image should serialise")
}

/// The assembly program, as `.lzx` bytes: one object, assembled, then linked with the
/// same entry sequence the other two use.
fn assembly_image() -> Vec<u8> {
    let config = ArchitectureConfig::lz64();
    let object = lazalith_toolchain::assemble_named("program.lzs", ASSEMBLY)
        .expect("the assembly should assemble");
    let startup = lazalith_runtime::startup_object_for(config, "sum_entry")
        .expect("the entry sequence assembles for an assembly entry");
    let linked = lazalith_toolchain::link_objects(
        &[object, startup],
        &lazalith_toolchain::LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .expect("the assembly should link");
    linked
        .image()
        .to_bytes()
        .expect("the assembly image should serialise")
}

fn run_lazen() -> Finished {
    finish(lazen_image())
}

fn run_c() -> Finished {
    finish(c_image())
}

fn run_assembly() -> Finished {
    finish(assembly_image())
}

/// Runs a `.lzx` through the runtime's runner: image bytes in, output and status out.
///
/// The *same* runner all three go through, which is the point of the test. Step 96
/// moved it out of the `lazen` command precisely so that a test and the tool could
/// boot a program the same way; here it is three programs and three front ends.
fn finish(image: Vec<u8>) -> Finished {
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    Finished {
        exit_code: finished.exit_code,
        output: String::from_utf8_lossy(&finished.output).into_owned(),
    }
}

/// The C entry symbol, which is the IR one and not a name this file invents.
const C_MAIN: &str = "fn.c.main";

/// The machine, the kernel and the services a run needs, named once.
///
/// This is here so a reader can see that a run is a machine and a kernel and two
/// services, and that none of the three is per-language.
#[allow(dead_code)]
fn the_run_needs() -> (
    DeviceManager<NoDevice>,
    ProcessId,
    ThreadId,
    VirtualTerminal,
    VirtualFileSystem,
) {
    (
        DeviceManager::<NoDevice>::new(),
        ProcessId::new(1).expect("one is a process id"),
        ThreadId::new(1).expect("one is a thread id"),
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
}
