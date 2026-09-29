//! Step 83: the three front ends converge on one object.
//!
//! # What this is checking
//!
//! There are three ways to write a program for this machine — assembly, C and
//! Lazen — and there is supposed to be exactly one way to *run* one:
//!
//! ```text
//! Assembly ──┐
//! C ─────────┼──→ Lazalith Object ──→ Linker ──→ Executable ──→ LazOS
//! Lazen ─────┘
//! ```
//!
//! Every arrow after "Lazalith Object" is shared, and that is the claim. A second
//! executable format for C, or a linker that treats C objects differently, or a
//! runtime that boots a C program by another route, would each be a separate
//! ecosystem wearing the same machine — and this step's instruction is explicit
//! that they should not exist.
//!
//! So this file does not test the three front ends. Step 81 and steps 65 to 72
//! do that. It tests the *convergence*: the same program, written three ways,
//! produces three objects that the same linker turns into the same kind of
//! executable, and the real machine runs all three to the same answer.
//!
//! # Why the program is dull
//!
//! It returns a number and writes nothing. A convergence test that failed for an
//! interesting reason would be a test of something else, and the interesting
//! reasons all live elsewhere: the C runtime has sixteen tests of its own and the
//! Lazen standard library has the SDK suites. What belongs here is only the
//! claim that the *route* is shared, and the shortest possible program is the
//! one that isolates it.
//!
//! No library is involved, deliberately. A program that printed something would
//! need the C runtime or the Lazen standard library linked in, and then a failure
//! would be a failure of one of those rather than of the route.

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_codegen::{CodegenOptions, generate};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ThreadId, VirtualFileSystem,
    VirtualTerminal,
};
use lazalith_toolchain::{LinkOptions, ObjectFile, assemble_named, link_objects};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// How many instructions a program may retire before it is called a runaway.
const STEP_BUDGET: u64 = 1_000_000;

/// The number every version of this program returns.
const ANSWER: u32 = 42;

/// The entry symbol each front end names its `main`.
///
/// Three different names for the same function is the one place the three front
/// ends are *allowed* to differ, because the IR has a single flat symbol
/// namespace and each front end has to keep its own functions out of the others.
/// way: a C function called `write` must not be the ABI's `write`, and a Lazen
/// one must not be a C one. The prefixes are the backend's `fn.` and, for C, the
/// module name the C compiler chose. Everything after the entry symbol is shared,
/// and the last test in this file says so.
const ASSEMBLY_MAIN: &str = "program_main";
const C_MAIN: &str = "fn.c.main";
const LAZEN_MAIN: &str = "fn.main";

/// The program in C. `main` returns an `int`, and the status the process reports
/// is the low 32 bits of it.
const C: &str = "int main(void) { return 42; }";

/// The program in Lazen. `main` returns an `i64`, and the startup passes it
/// straight to `exit`.
fn lazen() -> String {
    format!("pub fn main() -> i64 {{\n    return {ANSWER};\n}}\n")
}

/// The program in assembly: the body, and the entry sequence that calls it.
///
/// The entry sequence is written out rather than generated, because an assembly
/// program is allowed to write its own. That the generated one assembles to the
/// same instructions is a separate test below.
const ASSEMBLY_BODY: &str = r#"
.arch lz64
.entry program_main
.global program_main
program_main:
         LI r0, 42
         RET
"#;

const ASSEMBLY_ENTRY: &str = r#"
.arch lz64
.entry entry
.extern program_main
entry:
         CALL program_main
         MOV r1, r0
         LI r0, 1
         LI r7, 0
         SYSCALL
"#;

/// What a finished run produced.
struct Finished {
    exit_code: u32,
    output: Vec<u8>,
}

/// The C object, built through the C front end.
fn c_object() -> ObjectFile {
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = lazalith_c_compiler::compile(&mut sources, "t.c", C)
        .unwrap_or_else(|error| panic!("the C program should compile:\n{}", error.render()));
    let lowered = lazalith_c_compiler::ir::lower(&checked)
        .unwrap_or_else(|error| panic!("the C program should lower: {error}"));
    assert_eq!(
        lowered.entry.as_deref(),
        Some("c.main"),
        "the entry is the C `main`, under the C compiler's own prefix"
    );
    generate(
        &lowered.module,
        &lowered.frames,
        lowered.entry.as_deref(),
        &CodegenOptions::lz64("t.c"),
        C,
    )
    .unwrap_or_else(|error| panic!("the C program should generate code: {error}"))
    .object()
    .clone()
}

/// The Lazen object, built through the Lazen front end.
///
/// This is the whole of the Lazen side of the claim: the Lazen compiler lowers to
/// the same `lazalith_ir` module, the same backend turns it into an object, and
/// nothing in between knows what language the module came from.
fn lazen_object() -> ObjectFile {
    let source = lazen();
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = lazalith_compiler::compile(&mut sources, "t.lz", &source)
        .unwrap_or_else(|error| panic!("the Lazen program should compile:\n{}", error.render()));
    let lowered = lazalith_compiler::lower::lower(&checked)
        .unwrap_or_else(|error| panic!("the Lazen program should lower: {error}"));
    assert_eq!(
        lowered.entry, "main",
        "the entry is the Lazen `main`; Lazen qualifies names with the module, and \
         the module here is the default one"
    );
    generate(
        &lowered.module,
        &lowered.frames,
        Some(&lowered.entry),
        &CodegenOptions::lz64("t.lz"),
        &source,
    )
    .unwrap_or_else(|error| panic!("the Lazen program should generate code: {error}"))
    .object()
    .clone()
}

/// The assembly objects: the body, and the entry sequence that calls it.
fn assembly_objects() -> Vec<ObjectFile> {
    vec![
        assemble_named("program.body", ASSEMBLY_BODY)
            .unwrap_or_else(|error| panic!("the assembly body should assemble: {error}")),
        assemble_named("program.entry", ASSEMBLY_ENTRY)
            .unwrap_or_else(|error| panic!("the entry sequence should assemble: {error}")),
    ]
}

/// The C objects: the program, and the shared startup that calls its `main`.
fn c_objects() -> Vec<ObjectFile> {
    vec![
        c_object(),
        lazalith_runtime::startup_object_for(ArchitectureConfig::lz64(), C_MAIN)
            .expect("the entry sequence assembles for the C entry symbol"),
    ]
}

/// The Lazen objects, the same way.
fn lazen_objects() -> Vec<ObjectFile> {
    vec![
        lazen_object(),
        lazalith_runtime::startup_object_for(ArchitectureConfig::lz64(), LAZEN_MAIN)
            .expect("the entry sequence assembles for the Lazen entry symbol"),
    ]
}

/// Two instructions of supervisor kernel: a `NOP` and the `RFE` that hands off.
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

/// Links the given objects into an image and runs it, reporting what it did.
///
/// The linker, the image format, the kernel and the machine are the same four
/// things for all three front ends, and that is the whole of the test: this
/// function knows nothing about the language of anything it is given.
fn run(objects: Vec<ObjectFile>) -> Finished {
    let config = ArchitectureConfig::lz64();
    let linked = link_objects(
        &objects,
        &LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("the objects should link: {error}"));
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

    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    kernel
        .start_image(
            image,
            ProcessId::new(1).expect("a process id"),
            ThreadId::new(1).expect("a thread id"),
        )
        .expect("the program is scheduled");

    for _ in 0..STEP_BUDGET {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                return Finished {
                    exit_code: code,
                    output: kernel.terminal().terminal().output().to_vec(),
                };
            }
            Some(KernelServiceOutcome::Fault(error)) => {
                panic!("the program faulted in the kernel: {error:?}")
            }
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                panic!("the program trapped ({cause:?}, payload {payload})")
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    panic!("the program did not finish within {STEP_BUDGET} steps");
}

/// All three are the same object format, stated as a fact about the bytes.
///
/// A second object format for one language would fail here, which is the earliest
/// point at which it could be caught. The magic is the first thing in the file
/// and the section table is what a linker reads, so between them they say
/// "Lazalith object" without depending on any of the rest.
#[test]
fn all_three_front_ends_produce_lazalith_objects() {
    let assembly = assembly_objects()
        .into_iter()
        .next()
        .expect("the assembly body assembles");
    let c = c_object();
    let lazen = lazen_object();

    for (name, object) in [("assembly", &assembly), ("c", &c), ("lazen", &lazen)] {
        let bytes = object.to_bytes().expect("the object serialises");
        assert_eq!(
            &bytes[..lazalith_toolchain::OBJECT_MAGIC.len()],
            &lazalith_toolchain::OBJECT_MAGIC,
            "the {name} object does not begin with the Lazalith object magic, so it \
             is a different object format and the front ends have not converged"
        );
        let sections = object.sections();
        // The section names are bare words -- `text`, `data` -- because a section
        // is identified by its kind and the name is only a label on it. The kind is
        // what the linker reads.
        assert!(
            sections
                .iter()
                .any(|section| section.kind() == lazalith_toolchain::SectionKind::Text),
            "the {name} object has no code section, so it could not run"
        );
    }
}

/// The real claim: one linker, one image, one machine, three front ends.
///
/// Each program is run to completion and its status checked, and then all three
/// results are compared. A test that only ran them would pass with one of them
/// wrong; comparing is what makes this a statement about convergence rather than
/// about three programs.
#[test]
fn all_three_front_ends_run_the_same_program_to_the_same_answer() {
    let results = [
        ("assembly", run(assembly_objects())),
        ("c", run(c_objects())),
        ("lazen", run(lazen_objects())),
    ];

    for (name, finished) in &results {
        assert_eq!(
            finished.output, b"",
            "the {name} program writes nothing, and this one did"
        );
        assert_eq!(
            finished.exit_code, ANSWER,
            "the {name} program should have returned {ANSWER}"
        );
    }

    let (_, first) = &results[0];
    for (name, finished) in &results[1..] {
        assert_eq!(
            finished.exit_code, first.exit_code,
            "the {name} program's status differs from the assembly one's"
        );
    }
}

/// The three versions are separate objects and one image, not three images.
///
/// Convergence is not the same as sameness. The three programs are three objects
/// with three different symbol names, and the linker merges them into one image
/// with one code section — which is the shape a loader can boot, and the reason
/// a C object and a Lazen object can end up in the *same* program.
#[test]
fn three_objects_link_into_one_image() {
    let mut objects = c_objects();
    objects.extend(lazen_objects());
    objects.extend(assembly_objects());
    let linked = link_objects(
        &objects,
        &LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("objects from three front ends should link: {error}"));
    let sections = linked.image().sections();
    let code = sections
        .iter()
        .filter(|section| section.kind() == lazalith_os::LzxSectionKind::Code)
        .count();
    assert_eq!(
        code, 1,
        "one image has one code section however many objects went into it"
    );
}

/// The generated entry sequence, for three different entry names.
///
/// This is the smallest possible version of the convergence claim and it is worth
/// having on its own. The sequence is generated by *one* function and the only
/// thing that varies is the name it calls, so if two of the three are not the
/// same instructions with a different name, one language has grown its own way in.
#[test]
fn one_entry_sequence_serves_three_entry_names() {
    let config = ArchitectureConfig::lz64();
    let mut code = Vec::new();
    for name in [ASSEMBLY_MAIN, C_MAIN, LAZEN_MAIN] {
        let source = lazalith_runtime::startup_source_for(config, name);
        assert!(
            source.contains(&format!("CALL {name}")),
            "the entry sequence for {name} should call {name}, and is:\n{source}"
        );
        let object = lazalith_runtime::startup_object_for(config, name)
            .unwrap_or_else(|error| panic!("the sequence should assemble for {name}: {error}"));
        code.push(object.code().to_vec());
    }
    // The only difference between the three is the immediate in the `CALL`, and an
    // immediate is a relocation rather than a byte of code — so the code sections
    // are identical. That is the convergence claim in its smallest form.
    assert_eq!(
        code[0], code[1],
        "the assembly and C entry sequences should be the same code"
    );
    assert_eq!(
        code[1], code[2],
        "the C and Lazen entry sequences should be the same code"
    );
}
