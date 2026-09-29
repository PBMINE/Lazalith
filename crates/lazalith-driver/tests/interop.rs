//! B16: C, Lazen and assembly in one object pipeline (§16).
//!
//! # What §16 actually asks for
//!
//! ```text
//! C ──────┐
//! Lazen ──┼──→ .lzo → lazld → .lzx
//! ASM ────┘
//! ```
//!
//! "There must NOT be separate executable ecosystems for C, Lazen, and assembly."
//!
//! **Linking together is the easy half, and it already worked.** Three objects, one
//! `lazld`, one `.lzx` — B14 proved that, and nothing here re-proves it. The hard half
//! is that the three can *call* each other, because a link is not interoperability: two
//! objects that sit in the same image and never reference each other are two
//! executables sharing a file format.
//!
//! # What these tests found
//!
//! Three separate things had to be true, and none of them was, so none of them could
//! have been assumed:
//!
//! 1. **A Lazen `extern "c"` declaration has to name the same symbol a C definition
//!    does.** It now does, because both ask `lazalith_ir::c_ir_name`. Before B16 each
//!    front end had its own copy of that prefix, which is a namespace collision waiting
//!    for the first time either changed.
//! 2. **The IR refused every cross-module call.** `CallTarget::Imported` existed and
//!    `verify` rejected all of them, so cross-language calls were not *expressible*. The
//!    verifier now checks an import against a local declaration when there is one and
//!    lets it stand when there is not, because an import is by definition something
//!    this module does not define.
//! 3. **An object that references a symbol it does not define has to record it.** The
//!    first attempt emitted the `extern "c"` declaration as a bodiless function, which
//!    codegen emits as a `TRAP` — so the program *linked* and then called its own trap.
//!    The declaration is now not emitted at all, and codegen records an **undefined
//!    symbol** for a name it calls and does not define.
//!
//! The trap in (3) is why every test here runs its image. Two of the three defects were
//! invisible to every check except execution.

use lazalith_driver::CBuildOptions;
use lazalith_runtime::BuildOptions;
use lazalith_toolchain::ObjectFile;
use lazalith_types::ArchitectureConfig;

/// Links objects and runs the image, reporting what it printed and returned.
fn run(objects: Vec<ObjectFile>, entry: &str) -> (String, u32) {
    let image = lazalith_driver::link(&objects, ArchitectureConfig::lz64(), entry)
        .expect("the objects link into one image");
    let bytes = lazalith_driver::image_bytes(&image).expect("the image serialises");
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        ArchitectureConfig::lz64(),
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the image should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// A C library: definitions with no `main`, because it is not a program.
fn c_library(source: &str) -> ObjectFile {
    lazalith_driver::compile_c(source, &CBuildOptions::hosted("lib.c").as_library())
        .unwrap_or_else(|error| panic!("the C library should compile:\n{error}"))
}

/// A Lazen program.
fn lazen(source: &str) -> ObjectFile {
    lazalith_driver::compile_lazen(source, &BuildOptions::lz64("main.lz"))
        .unwrap_or_else(|error| panic!("the Lazen program should compile:\n{error}"))
}

/// A Lazen program calls a C function, through one linker, and the answer is right.
///
/// The arithmetic is the point: `triple(14)` must be 42, so a program that got the
/// call wrong — wrong register, wrong stack, wrong target — cannot pass by returning
/// something plausible.
#[test]
fn lazen_calls_c() {
    let (_, status) = run(
        vec![
            c_library("int triple(int value) { return value * 3; }\n"),
            lazen(
                "extern \"c\" fn triple(value: i32) -> i32;\n\nfn main() -> i32 {\n    return triple(14);\n}\n",
            ),
        ],
        "fn.main",
    );
    assert_eq!(
        status, 42,
        "a Lazen `main` returned what the C function computed"
    );
}

/// A cross-language call carries a Lazen *view* — a pointer and a length — into C.
///
/// A single-integer call fits entirely in registers. A view is two words, and the
/// second one is not a length the callee may ignore: C reads the bytes the length says
/// are there. The checksum below is of **exactly `len` bytes**, so a call that passed
/// the pointer but the wrong length, or the length but the wrong pointer, cannot come
/// out right.
///
/// **The first draft of this test passed a Lazen `str` to a C `const char *` and had C
/// walk to a NUL.** A Lazen `str` is a pointer and a length; it is *not* NUL
/// terminated, so the C function read past the end of the string and counted 17 — one
/// byte of whatever followed. The count was wrong, the memory access was out of
/// bounds, and the test would have passed had the expected number been 17.
///
/// That is worth recording: the two languages agree about how to pass a pointer, and
/// they do *not* agree about what a string is. §16's "same ABI" is about the calling
/// convention; it is not a claim that the two languages' types are interchangeable.
#[test]
fn a_cross_language_call_carries_a_pointer_and_a_length() {
    let (output, status) = run(
        vec![
            c_library(
                "unsigned long sum_bytes(const char *text, unsigned long length) {\n    unsigned long total = 0;\n    unsigned long i;\n    for (i = 0; i < length; i = i + 1) { total = total + (unsigned long)text[i]; }\n    return total;\n}\n",
            ),
            lazen(
                "extern \"c\" fn sum_bytes(text: str, length: u64) -> u64;\n\nfn main() -> i32 {\n    let greeting: str = \"Hello from Lazen\";\n    return sum_bytes(greeting, greeting.len() as u64) as i32;\n}\n",
            ),
        ],
        "fn.main",
    );
    assert!(
        output.is_empty(),
        "and nothing was printed, because the program only asked for a sum: {output:?}"
    );
    // The byte sum of the 16 characters, computed by hand so it cannot drift with
    // whatever the code happens to do.
    let expected: u32 = "Hello from Lazen".bytes().map(u32::from).sum();
    assert_eq!(
        status, expected,
        "a C function read {expected} bytes through a pointer a Lazen function passed, and \
         summed them to {expected}. A wrong pointer, or a length word in the wrong \
         register, would not land on the same sum"
    );
    // A fixed number, so the test does not merely agree with itself. The first draft
    // of this line said 1062, which is what a plausible-sounding mental sum produced and
    // is wrong: the bytes add to 1506. A constant nobody checked is a constant that
    // will be wrong, and the failure it caused here was the useful kind 2014 it forced the
    // sum to be done by something other than by hand.
    assert_eq!(
        expected, 1506,
        "and the hand-computed sum of the 16 bytes is 1506"
    );
}

/// All three front ends' objects go into one image, and one linker produces it.
///
/// **§16's diagram, executed.** The C object and the assembly object are linked
/// alongside the Lazen one, and the program runs — which is the claim that there is no
/// separate executable ecosystem per language.
#[test]
fn all_three_front_ends_share_one_linker_and_one_image() {
    let assembly = lazalith_toolchain::assemble_named(
        "helpers.la",
        ".arch lz64\n.entry _start\n.global _start\n.global fn.helper_double\n.section .text\n_start:\n    LI r0, 7\n    RET\nfn.helper_double:\n    LI r0, 100\n    RET\n",
    )
    .expect("the assembly assembles");
    let (output, status) = run(
        vec![
            c_library("int triple(int value) { return value * 3; }\n"),
            assembly,
            lazen(
                "extern \"c\" fn triple(value: i32) -> i32;\n\nfn main() -> i32 {\n    return triple(14);\n}\n",
            ),
        ],
        "fn.main",
    );
    assert_eq!(
        status, 42,
        "C, assembly and Lazen in one image, and the C function still computed the \
         answer: {output:?}"
    );
}

/// A C file with no `main` is a library, and is refused only when asked to be a program.
///
/// This is the difference B16 had to introduce, and it is worth being explicit about
/// because the refusal was *correct* and the question was wrong: before `-c`, `lazcc`
/// built only programs, so a C file with no `main` failed with `there is no main to
/// start at`. A library has no entry point because it is not entered; it is linked
/// into something that is.
#[test]
fn a_c_file_with_no_main_is_a_library_and_says_so() {
    let source = "int helper(void) { return 1; }\n";

    // Without `-c` it is a program, and a program needs an entry point.
    let error = lazalith_driver::compile_c(source, &CBuildOptions::hosted("lib.c"))
        .expect_err("a C file with no main is not a program");
    assert!(
        error.to_string().contains("main"),
        "and says what is missing: {error}"
    );

    // With it, the same file is a library, and compiles.
    let object = lazalith_driver::compile_c(source, &CBuildOptions::hosted("lib.c").as_library())
        .expect("the same file, built as a library");
    assert!(
        object
            .symbols()
            .iter()
            .any(|symbol| symbol.name() == "fn.c.helper"),
        "and its definition is a global the linker can bind: {:?}",
        object
            .symbols()
            .iter()
            .map(|symbol| symbol.name())
            .collect::<Vec<_>>()
    );
}

/// A call to a C function that nothing defines is a link error, not a silent trap.
///
/// The two are the same defect seen from opposite ends, and a build that cannot tell
/// them apart will accept one of them. A program whose `extern "c"` declaration names
/// a function no object defines must be **refused at link time**, saying which symbol
/// is missing.
#[test]
fn a_call_to_a_missing_c_function_is_a_link_error() {
    let error = lazalith_driver::link(
        &[
            lazen("extern \"c\" fn nowhere(value: i32) -> i32;\n\nfn main() -> i32 {\n    return nowhere(1);\n}\n"),
        ],
        ArchitectureConfig::lz64(),
        "fn.main",
    )
    .expect_err("a call to a function nothing defines does not link");
    assert!(
        error.to_string().contains("fn.c.nowhere"),
        "and names the symbol that is missing, which is the one thing a linker error \
         has to say: {error}"
    );
}
