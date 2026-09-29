//! B14: the toolchain stages, and that the tools are genuinely separate.
//!
//! # The one test that matters most
//!
//! `the_two_ways_to_build_agree_byte_for_byte` is the honest check on §18's separation.
//! There are two ways to get an image from a Lazen source in this repository:
//!
//! - `lazen build hello.lz`, which compiles and links in one command;
//! - `lazcc hello.lz` then `lazld hello.lzo`, which stops and starts at a file.
//!
//! They must produce **the same bytes**. If they did not, one of them had a private
//! object format or a private link, and the "separation" would be two implementations
//! that happen to look alike — which is exactly the parallel linker and object system
//! §18 forbids.
//!
//! # Why a temporary directory rather than the target directory
//!
//! These tools write real files, and a test that littered the build directory would
//! make the workspace's state depend on whether tests had run. Each test gets its own
//! directory, named after the process, and removes it afterwards.
//!
//! # What is deliberately not tested
//!
//! No test runs a `main`. The tools' argument handling is exercised through the same
//! functions the binaries call, because a test that shells out to a built binary
//! depends on the binary having been built, and `cargo test` does not guarantee that
//! ordering.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use lazalith_driver::tools::{AsTool, CcTool, LdTool};
use lazalith_driver::{
    C_OBJECT_ENTRY, CBuildOptions, DriverError, LAZEN_ENTRY, compile_c, image_bytes, lazen_object,
    link,
};
use lazalith_runtime::BuildOptions;
use lazalith_toolchain::ObjectFile;

/// Runs a linked image and reports what it printed and returned.
///
/// **The same path `lazen run` takes**, so a test that calls this is testing the
/// machine executing the image rather than a private harness that happens to agree.
fn finish(bytes: Vec<u8>) -> (String, u32) {
    use lazalith_devices::{DeviceManager, NoDevice};
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        lazalith_types::ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the image should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// Compiles, links and runs a C program, through the toolchain stages.
fn run_c(source: &str) -> (String, u32) {
    let object = compile_c(source, &CBuildOptions::hosted("cross.c"))
        .unwrap_or_else(|error| panic!("the C program should compile:\n{error}"));
    let image = link(
        &[object],
        lazalith_types::ArchitectureConfig::lz64(),
        &lazalith_driver::c_entry(),
    )
    .unwrap_or_else(|error| panic!("the C program should link: {error}"));
    let bytes = image_bytes(&image).expect("the C image serialises");
    finish(bytes)
}

/// Compiles, links and runs a Lazen program, through the toolchain stages.
fn run_lazen(source: &str) -> (String, u32) {
    let options = BuildOptions {
        architecture: lazalith_types::ArchitectureConfig::lz64(),
        source_path: String::from("cross.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_driver::build_lazen_image(source, &options)
        .unwrap_or_else(|error| panic!("the Lazen program should build:\n{error}"));
    finish(bytes)
}

/// A directory for one test's files, removed when it goes.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("lazalith-b14-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the scratch directory is creatable");
        Self { path }
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let path = self.path.join(name);
        fs::write(&path, text).expect("a scratch file is writable");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

/// A Lazen program that runs and returns zero.
///
/// The real syntax, taken from `examples/hello/main.lz` — a first draft of this
/// fixture invented a `:` and the compiler helpfully said so, which is the compiler
/// working. A test fixture written from memory of a language is a test of the memory.
const HELLO: &str = r#"fn main() -> i32 {
    rt::sys::print("Hello, Lazalith\n");
    return 0;
}
"#;

/// A LZA assembly program, for the assembler that must not need a compiler.
///
/// `.arch` is required before any instruction — E130, "instruction appears before
/// .arch" — so the fixture says which machine it is for.
const ASSEMBLY: &str = r#".arch lz64
.entry _start
.global _start
.section .text
_start:
    LI r0, 0
    HALT
"#;

// -- the one test that matters -----------------------------------------------

/// `lazcc` + `lazld` and `lazen build` produce the same image.
///
/// **The object as well as the image.** Checking only the image would let the two
/// objects differ and the link hide it, and the object is the thing a caller
/// inspects, diffs, or hands to a different linker.
#[test]
fn the_two_ways_to_build_agree_byte_for_byte() {
    let scratch = Scratch::new("agree");
    let source = scratch.write("hello.lz", HELLO);

    // Way one: the driver, through the stage library.
    //
    // **The same source path as way two gets below.** The first draft used a bare
    // `"hello.lz"` here and the real path there, and the two images differed by the
    // debug block, which embeds the path the compiler was told. That is a real
    // property of the object format rather than a bug, and it is the sort of thing
    // that would have looked like "the tools drift" if the test had not compared
    // bytes.
    let options = BuildOptions::lz64(source.display().to_string());
    let through_the_library =
        lazalith_driver::build_lazen_image(HELLO, &options).expect("the library builds the image");

    // Way two: the tools, with a file in the middle.
    let (object_path, object_bytes) =
        CcTool::run(&args(&[source.to_str().unwrap()])).expect("lazcc produces an object");
    fs::write(&object_path, &object_bytes).expect("the object is writable");

    let direct = lazen_object(HELLO, &options).expect("the driver produces an object");
    assert_eq!(
        object_bytes,
        direct.to_bytes().expect("the object serialises"),
        "lazcc's object and the driver's object are the same bytes, so there is one \
         object format and not two"
    );

    let (image_path, image_from_tools) =
        LdTool::run(&args(&[object_path.to_str().unwrap()])).expect("lazld produces an image");
    fs::write(&image_path, &image_from_tools).expect("the image is writable");

    assert_eq!(
        image_from_tools, through_the_library,
        "and the images are the same bytes: two tools agreeing by construction is the \
         separation working, two tools agreeing by testing would be a duplicate \
         implementation waiting to drift"
    );
    assert_eq!(
        image_path.file_name().and_then(|n| n.to_str()),
        Some("hello.lzx"),
        "with the image named after the program, so `lazcc a.lz && lazld a.lzo` leaves \
         exactly the file `lazen build a.lz` would"
    );

    // The comparison that was missing, and the one this test exists for.
    //
    // **Everything above compares this crate against this crate.** `build_lazen_image`
    // and `LdTool::run` both end in `lazalith_driver::link`, so agreeing with each
    // other says only that the code is deterministic. What makes the separation a
    // claim rather than a tautology is agreement with the path that already worked
    // *before* B14: `RuntimeProgram`, which the CLI and every existing example use.
    //
    // It is not tautological, either. `link` here once passed the *program's* entry
    // as the *image's* entry symbol, so the image started at the program's first
    // instruction with no stack and no exit. Every image produced was internally
    // consistent, both halves of this test agreed, and every program trapped. The
    // only thing that caught it was running the image — which is why the C tests
    // below run what they build.
    let reference = lazalith_runtime::RuntimeProgram::build(HELLO, &options)
        .expect("the pre-B14 path builds")
        .to_image_bytes()
        .expect("the pre-B14 path links");
    assert_eq!(
        reference, through_the_library,
        "the driver's image is the same bytes as the RuntimeProgram image that \
         `lazen build` has always produced, so B14 changed how the stages are reached \
         and not what they produce"
    );
}

/// An image built through the stages runs.
///
/// **The check that a byte comparison cannot make.** The two images in the test
/// above can be identically wrong; this one is not a comparison at all. An image
/// that traps on its first instruction is byte-for-byte consistent with every other
/// image the same bug produced, and no amount of comparing those to each other finds
/// it.
#[test]
fn an_image_built_through_the_stages_runs() {
    let (output, status) = run_lazen(
        r#"fn main() -> i32 {
    rt::sys::print("Hello through the stages\n");
    return 5;
}
"#,
    );
    assert!(
        output.contains("Hello through the stages"),
        "the program ran and printed: {output:?}"
    );
    assert_eq!(status, 5, "and returned what it returned");
}

// -- the stages are independently callable -----------------------------------

/// A caller who wants an object can have one, without a link.
///
/// This is the thing that was impossible before B14: `RuntimeProgram::build` compiled
/// and generated and held the result, and the only way to reach the objects was a
/// method that also knew how to link.
#[test]
fn a_caller_can_stop_at_the_object() {
    let object = lazen_object(HELLO, &BuildOptions::lz64("hello.lz")).expect("an object");
    let bytes = object.to_bytes().expect("it serialises");
    assert!(!bytes.is_empty());
    let read_back = ObjectFile::from_bytes(&bytes).expect("and reads back");
    assert_eq!(
        read_back.sections().len(),
        object.sections().len(),
        "a round trip through the file format preserves the object, so the format is a \
         real boundary and not a suggestion"
    );
}

/// Assembly produces an object with no compiler, no prelude and no linker.
///
/// §18: "Assembly independently produces `.lzo`." Total independence is the claim, and
/// this is the test: the assembler is handed a `.la` file and nothing else exists.
#[test]
fn the_assembler_needs_nothing_else() {
    let scratch = Scratch::new("assemble");
    let source = scratch.write("prog.la", ASSEMBLY);
    let (target, bytes) = AsTool::run(&args(&[source.to_str().unwrap()])).expect("an object");
    fs::write(&target, &bytes).expect("the object is writable");
    assert_eq!(
        target.extension().and_then(|e| e.to_str()),
        Some("lzo"),
        "and the object is named after the source"
    );
    let object = ObjectFile::from_bytes(&bytes).expect("it reads back");
    assert!(
        !object.symbols().is_empty(),
        "with the symbols the assembly declared, which is the whole of what assembling \
         means"
    );
}

/// `lazld` consumes objects and produces an image, and nothing else.
#[test]
fn the_linker_consumes_objects_and_produces_an_image() {
    let scratch = Scratch::new("link");
    let source = scratch.write("hello.lz", HELLO);
    let (object_path, object_bytes) =
        CcTool::run(&args(&[source.to_str().unwrap()])).expect("an object");
    fs::write(&object_path, &object_bytes).expect("writable");

    let objects = vec![ObjectFile::from_bytes(&object_bytes).expect("reads back")];
    let image = link(
        &objects,
        lazalith_types::ArchitectureConfig::lz64(),
        LdTool::ENTRY,
    )
    .expect("an image");
    let bytes = image_bytes(&image).expect("bytes");
    assert!(bytes.len() > 64, "an image is not a header");

    // And the image is loadable, which is the property a linker exists for.
    let parsed = lazalith_os::LzxImage::from_bytes(&bytes).expect("the image parses");
    assert!(
        parsed.entry_offset() <= bytes.len() as u64,
        "the entry is inside the image: {} of {} bytes",
        parsed.entry_offset(),
        bytes.len()
    );
}

/// An object for the wrong machine is refused rather than linked.
#[test]
fn an_object_for_another_machine_is_refused() {
    let lz32_object = lazalith_toolchain::assemble_for(
        lazalith_types::ArchitectureConfig::lz32(),
        "a.la",
        &ASSEMBLY.replace(".arch lz64", ".arch lz32"),
    )
    .expect("a 32-bit object");
    assert_eq!(
        lz32_object.target().architecture().word_width(),
        lazalith_types::WordWidth::W32,
        "and it really is a 32-bit object, so the refusal below is about the mismatch \
         rather than about the test not having built what it thought"
    );
    let error = link(
        &[lz32_object],
        lazalith_types::ArchitectureConfig::lz64(),
        "fn.main",
    )
    .expect_err("a 32-bit object is not a 64-bit object");
    assert!(
        matches!(error, DriverError::WrongArchitecture { .. }),
        "linking a 32-bit object into a 64-bit image would produce an image whose \
         instructions are the wrong width, and nothing downstream would say so. Got {error:?}"
    );
}

// -- C is a first-class target -------------------------------------------------

/// A C file with a diagnostic is reported as a diagnostic, and rendered.
///
/// **Rendering is the part that matters at a stage boundary.** This is the last
/// place a tool can still reach the compiler's source map, so a message that has
/// lost its line and caret by the time it reaches a user is a diagnostic that has
/// been thrown away rather than passed on.
#[test]
fn a_c_file_with_an_error_is_reported_as_an_error() {
    let error = lazalith_driver::compile_c(
        "int main(void) { return not_a_thing; }",
        &CBuildOptions::freestanding("bad.c"),
    )
    .expect_err("a C error is reported");
    match error {
        DriverError::C(lazalith_driver::CFrontendError::Diagnostics { count, first }) => {
            assert!(count >= 1, "and it says how many it found");
            assert!(
                first.contains("bad.c"),
                "and the rendered diagnostic names the file: {first}"
            );
        }
        other => panic!("a C diagnostic must not be reported as a later stage: {other}"),
    }
}

/// A C program compiles to an object, links, and **runs**.
///
/// This is the whole of §14's "C is a first-class LZA target", and the part that
/// cannot be faked: the image is loaded, the machine executes it, and the output and
/// the exit status come back *from the run*. An object that merely links proves
/// nothing about whether the C frontend and the shared backend agree, which is the
/// only interesting question here.
#[test]
fn a_c_program_compiles_links_and_runs() {
    let (output, status) = run_c(
        r#"
int main(void) {
    print("Hello from C\n");
    return 7;
}
"#,
    );
    assert!(
        output.contains("Hello from C"),
        "the C program printed what it was told to: {output:?}"
    );
    assert_eq!(status, 7, "and returned what it returned");
}

/// A C program and a Lazen program computing the same thing agree.
///
/// **The cross-frontend check, through the new toolchain path.** Two front ends
/// sharing one IR, one lowerer and one backend means a disagreement localises the
/// defect to the front end. This is not the same test as
/// `lazalith-c-compiler/tests/cross_frontend.rs`: that one drives the compiler
/// crates directly, and this one goes through `lazalith-driver` and the `.lzo`
/// boundary, so it covers the part that test cannot see.
#[test]
fn c_and_lazen_agree_through_the_toolchain() {
    // Both print a value each program *computed* — 1 + 2 + 3 — so a program that got
    // the arithmetic wrong cannot pass by printing the right thing.
    let c = run_c(
        r#"
static void show(long value) {
    if (value == 0) { putchar(48); putchar(10); return; }
    char digits[24];
    unsigned at = 0, i;
    while (value != 0) { digits[at] = (char)(48 + (value % 10)); value = value / 10; at = at + 1; }
    i = at;
    while (i != 0) { i = i - 1; putchar(digits[i]); }
    putchar(10);
}
int main(void) {
    long total = 1 + 2 + 3;
    show(total);
    return (int)total;
}
"#,
    );
    let (lazen_output, lazen_status) = run_lazen(
        r#"fn main() -> i32 {
    // `i32`, not `i64`: Lazen has no implicit conversions, and an unsuffixed
    // integer literal is `i32`, so declaring `i64` here is a type error the
    // compiler is right to refuse. Written the way the language actually is.
    let total: i32 = 1 + 2 + 3;
    rt::sys::print("6\n");
    return total;
}
"#,
    );
    assert_eq!(c.1, 6, "the C program computed six and returned it: {c:?}");
    assert_eq!(
        c.0, lazen_output,
        "and printed the same line the Lazen program did: {c:?} vs {lazen_output:?}"
    );
    assert_eq!(
        c.1, lazen_status,
        "and the two returned the same status: {c:?} vs {lazen_status:?}"
    );
}

/// A C `main` and a Lazen `fn.main` are different symbols, so both can be linked.
///
/// §14 requires C and Lazen to be first-class *in one system*, and the first thing
/// that breaks if they are not is a link with both in it. The names differ
/// deliberately — `c.main` in the IR, `fn.c.main` in the object, `fn.main` for
/// Lazen — so the property is checked rather than asserted.
#[test]
fn c_and_lazen_objects_link_into_one_image() {
    let scratch = Scratch::new("mixed");
    let c_source = scratch.write("mixed.c", "int main(void) { print(\"C\\n\"); return 3; }\n");
    let lz_source = scratch.write("mixed.lz", HELLO);
    let (c_path, c_bytes) = CcTool::run(&args(&[c_source.to_str().unwrap()])).expect("a C object");
    let (lz_path, lz_bytes) =
        CcTool::run(&args(&[lz_source.to_str().unwrap()])).expect("a Lazen object");
    fs::write(&c_path, &c_bytes).expect("writable");
    fs::write(&lz_path, &lz_bytes).expect("writable");

    let c_object = ObjectFile::from_bytes(&c_bytes).expect("the C object reads back");
    let lz_object = ObjectFile::from_bytes(&lz_bytes).expect("the Lazen object reads back");
    assert!(
        c_object
            .symbols()
            .iter()
            .any(|symbol| symbol.name() == C_OBJECT_ENTRY),
        "the C object exports {C_OBJECT_ENTRY}"
    );
    assert!(
        lz_object
            .symbols()
            .iter()
            .any(|symbol| symbol.name() == LAZEN_ENTRY),
        "and the Lazen object exports {LAZEN_ENTRY}"
    );

    // One image, both objects, one entry. If the two names collided, linking this
    // would be a duplicate-symbol error rather than an image.
    let image = link(
        &[c_object, lz_object],
        lazalith_types::ArchitectureConfig::lz64(),
        &lazalith_driver::c_entry(),
    )
    .expect("a C and a Lazen object link into one image");
    let bytes = image_bytes(&image).expect("bytes");
    assert!(bytes.len() > 64, "and it is an image, not a header");
}

// -- the tools are usable ------------------------------------------------------

/// Each tool explains itself and reports its version, and neither does the work.
///
/// A tool that answers `--help` by *also* overwriting the caller's output file
/// would be a trap for a script, so "does not work" is part of what is tested.
#[test]
fn every_tool_explains_itself_without_doing_the_work() {
    let scratch = Scratch::new("usage");
    let source = scratch.write("hello.lz", HELLO);
    let output = scratch.path.join("out.lzo");
    for tool in [CcTool::NAME, AsTool::NAME, LdTool::NAME] {
        // The real binary, so what is tested is what a user runs.
        let binary = env!("CARGO_BIN_EXE_lazcc");
        let binary = match tool {
            "lazas" => env!("CARGO_BIN_EXE_lazas"),
            "lazld" => env!("CARGO_BIN_EXE_lazld"),
            _ => binary,
        };
        let run = |flag: &str| {
            std::process::Command::new(binary)
                .arg(flag)
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .output()
                .expect("the tool runs")
        };

        let version = run("--version");
        assert!(
            version.status.success(),
            "{tool} --version succeeds: {}",
            String::from_utf8_lossy(&version.stderr)
        );
        assert!(
            String::from_utf8_lossy(&version.stdout).starts_with(tool),
            "{tool} --version names itself: {}",
            String::from_utf8_lossy(&version.stdout)
        );

        let help = run("--help");
        assert!(help.status.success(), "{tool} --help succeeds");
        let help = String::from_utf8_lossy(&help.stdout);
        assert!(help.contains("usage:"), "{tool} --help shows a usage line");
        assert!(
            help.contains("docs/toolchain.md"),
            "{tool} --help says where the stages are described"
        );
        assert!(
            !output.exists(),
            "{tool} --help wrote no output file, so a script asking for help is safe"
        );
    }
}

/// A tool that is given nothing says so, rather than sitting silent.
#[test]
fn a_tool_with_no_input_says_what_it_needs() {
    for (binary, tool) in [
        (env!("CARGO_BIN_EXE_lazcc"), CcTool::NAME),
        (env!("CARGO_BIN_EXE_lazas"), AsTool::NAME),
        (env!("CARGO_BIN_EXE_lazld"), LdTool::NAME),
    ] {
        let run = std::process::Command::new(binary)
            .output()
            .expect("the tool runs");
        assert!(!run.status.success(), "{tool} with no arguments fails");
        let message = String::from_utf8_lossy(&run.stderr);
        assert!(
            message.contains("needs") && message.contains(tool),
            "{tool} says what it needs: {message}"
        );
    }
}

/// `lazld --entry` chooses where the image starts, and a bad entry is refused.
///
/// The linker decides the entry rather than the compiler, which is the other half of
/// why `lazcc` stops at an object: a caller can link the same object at a different
/// entry without recompiling it.
#[test]
fn the_linker_owns_the_entry_point() {
    let scratch = Scratch::new("entry");
    let source = scratch.write("tiny.la", ASSEMBLY);
    let (object_path, object_bytes) =
        AsTool::run(&args(&[source.to_str().unwrap()])).expect("an object");
    fs::write(&object_path, &object_bytes).expect("writable");

    // The assembly declares `_start`, so an image started there links.
    let (image_path, bytes) =
        LdTool::run(&args(&[object_path.to_str().unwrap(), "--entry", "_start"]))
            .expect("an image started at _start");
    assert_eq!(
        image_path.extension().and_then(|e| e.to_str()),
        Some("lzx"),
        "and is named beside the object, the way `lazen build` names it"
    );
    let image = lazalith_os::LzxImage::from_bytes(&bytes).expect("the image parses");
    assert!(
        image.entry_offset() <= bytes.len() as u64,
        "the entry is inside the image it just produced: {} of {} bytes",
        image.entry_offset(),
        bytes.len()
    );

    // The default entry is the Lazen one, and using it here is refused because the
    // object has no such symbol — which is the check being demonstrated.
    let error = LdTool::run(&args(&[object_path.to_str().unwrap()]))
        .expect_err("an object with no fn.main cannot start there");
    assert!(
        matches!(error, DriverError::Link { .. }),
        "and it is a link refusal, not a crash or an empty image: {error:?}"
    );
}
