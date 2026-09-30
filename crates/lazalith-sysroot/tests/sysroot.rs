//! B15: the sysroot is a real directory a real build reads from.
//!
//! # What these tests are actually for
//!
//! A sysroot is a directory, and a directory is easy to write tests about without
//! testing anything: check four folders exist, call it a day. That would pass for a
//! sysroot that no build ever reads, and the fact that B14's whole toolchain worked
//! without one is the reason that test would be worthless on its own.
//!
//! So the tests here are the three that would fail for a fake:
//!
//! - `a_c_program_built_against_a_sysroot_runs` — the sysroot's C library is compiled
//!   in front of a program, linked with the sysroot's startup object, and *executed*.
//! - `the_generated_declarations_round_trip_through_the_c_front_end` — every syscall
//!   declaration the header contains is parsed by the C front end and compared with
//!   the ABI's own signature. A header can only be "generated" in a way that means
//!   something if parsing it back gives the same answer.
//! - `a_freestanding_sysroot_refuses_the_hosted_library` — the hosted/freestanding
//!   distinction is a refusal, not a comment.
//!
//! # Temporary directories
//!
//! Same rule as the driver's tests: each test gets its own directory, named after the
//! process, and removes it. A sysroot test that littered the build directory would
//! make the workspace's state depend on whether tests had run.

use std::fs;
use std::path::PathBuf;

use lazalith_sysroot::{
    C_LIBRARY_FILE, Directory, EntryLanguage, RUNTIME_FILE, Sysroot, SysrootError, SysrootFlavour,
    abi_header, startup_name, syscall_header,
};
use lazalith_types::ArchitectureConfig;

/// A directory for one test's files, removed when it goes.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("lazalith-b15-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the scratch directory is creatable");
        Self { path }
    }

    /// Somewhere inside this test's directory.
    fn at(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn hosted(name: &str) -> (Scratch, Sysroot) {
    let scratch = Scratch::new(name);
    let sysroot = Sysroot::create(
        scratch.at("hosted"),
        SysrootFlavour::Hosted,
        ArchitectureConfig::lz64(),
    )
    .expect("a hosted sysroot is creatable");
    (scratch, sysroot)
}

// -- the layout ---------------------------------------------------------------

/// A hosted sysroot has §19's four directories, and a startup object in one of them.
#[test]
fn a_hosted_sysroot_has_the_four_directories_and_a_startup_object() {
    let (_scratch, sysroot) = hosted("layout");
    for directory in Directory::ALL {
        assert!(
            sysroot.root().join(directory.as_str()).is_dir(),
            "§19 names {directory}/, and a sysroot without it is not a sysroot"
        );
    }
    // The startup objects are the part that makes this a sysroot rather than a
    // layout: an image has to start somewhere, and where is the sysroot's business.
    for language in EntryLanguage::ALL {
        let object = sysroot
            .startup_object(ArchitectureConfig::lz64(), language)
            .expect("a startup object");
        let names: Vec<String> = object
            .symbols()
            .iter()
            .map(|symbol| String::from(symbol.name()))
            .collect();
        assert!(
            names
                .iter()
                .any(|name| name == lazalith_runtime::STARTUP_LABEL),
            "a {:?} startup object declares the label an image starts at, and it \
             declares {names:?}",
            language
        );
        assert!(
            names.contains(&language.symbol()),
            "and it calls that language's entry, {}. One object cannot do both: a C \
             `main` is `fn.c.main` and a Lazen `main` is `fn.main`",
            language.symbol()
        );
    }
}

/// A directory with some of §19's directories is refused, and says which is missing.
#[test]
fn a_directory_that_is_not_a_sysroot_is_refused() {
    let scratch = Scratch::new("not-a-sysroot");
    fs::create_dir_all(scratch.at("halfway/include")).expect("creatable");
    fs::create_dir_all(scratch.at("halfway/lib")).expect("creatable");

    let error =
        Sysroot::open(scratch.at("halfway")).expect_err("two of four directories is not a sysroot");
    match error {
        SysrootError::MissingDirectory { directory, .. } => {
            assert_eq!(
                directory,
                Directory::Crt,
                "and the refusal names the first directory it could not find, rather than \
                 saying 'invalid sysroot' and leaving the reader to guess"
            );
        }
        other => panic!("a missing directory is a missing directory: {other}"),
    }
    assert!(
        error.to_string().contains("not a sysroot"),
        "and says plainly what is wrong: {error}"
    );
}

/// A sysroot that opens is the one that was created, and says what it is for.
#[test]
fn a_sysroot_opens_and_reports_its_flavour() {
    let (_scratch, sysroot) = hosted("open");
    let opened = Sysroot::open_as(sysroot.root(), SysrootFlavour::Hosted)
        .expect("a sysroot it just created opens");
    assert_eq!(
        opened.flavour(),
        SysrootFlavour::Hosted,
        "and the flavour is **read from the directory**, not believed from the caller: a \\
         sysroot with lib/libc.c in it is hosted"
    );

    // And a caller who needs hosted and finds freestanding is told both sides, not
    // just that the requirement failed.
    let scratch = Scratch::new("open-wrong");
    let kernel = Sysroot::create(
        scratch.at("kernel"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("a freestanding sysroot");
    let error = Sysroot::open_as(kernel.root(), SysrootFlavour::Hosted)
        .expect_err("a freestanding sysroot is not a hosted one");
    assert!(
        error.to_string().contains("freestanding") && error.to_string().contains("hosted"),
        "and the message names what it found and what was needed: {error}"
    );
    assert_eq!(
        opened.flavour(),
        SysrootFlavour::Hosted,
        "and a reopened sysroot knows it is hosted"
    );
    assert_eq!(opened.root(), sysroot.root(), "at the same root");
}

// -- hosted and freestanding, which is the point ------------------------------

/// A freestanding sysroot has no C library and no hosted runtime, and says so.
///
/// **The distinction is a refusal, not a flag.** §19's last sentence asks for "a clear
/// distinction between hosted programs and freestanding kernel builds", and the way a
/// distinction is real is that crossing it produces an error. A freestanding build
/// that reached for a hosted library gets told which flavour it opened — so the
/// failure is "wrong sysroot" rather than a link error ten steps later, or worse, a
/// successful link of a hosted `rt::sys::print` into a kernel.
#[test]
fn a_freestanding_sysroot_refuses_the_hosted_library() {
    let scratch = Scratch::new("freestanding");
    let sysroot = Sysroot::create(
        scratch.at("kernel"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("a freestanding sysroot is creatable");

    for (what, refusal) in [
        ("C library", sysroot.c_library()),
        ("hosted runtime", sysroot.lazen_runtime()),
    ] {
        let error = refusal.expect_err(&format!("a freestanding sysroot has no {what}"));
        match error {
            SysrootError::NotInFlavour { flavour, .. } => assert_eq!(
                flavour,
                SysrootFlavour::Freestanding,
                "and the refusal names the flavour, so the fix is to open the other one"
            ),
            other => panic!("expected a flavour refusal, got {other}"),
        }
        assert!(
            error.to_string().contains("freestanding"),
            "and the message says what freestanding means: {error}"
        );
    }

    // A freestanding sysroot still has the startup objects: a kernel needs to start.
    for language in EntryLanguage::ALL {
        sysroot
            .startup_object(ArchitectureConfig::lz64(), language)
            .expect("a freestanding sysroot has a startup object");
    }
    // And it still has the headers, because a kernel's own code includes the ABI.
    assert!(
        sysroot.include().join("lazos/syscall.h").is_file(),
        "and the ABI headers, which are the one hosted thing a kernel genuinely needs"
    );
}

// -- the headers are generated from the ABI, and provably so -------------------

/// The header declares every syscall the ABI has, and nothing the ABI does not.
///
/// Counted both ways on purpose. A header that declared *some* of the ABI would pass
/// a "does it mention write?" test; counting catches a syscall that was added to the
/// table and never reached the header, which is exactly the drift §19 is about.
#[test]
fn the_headers_declare_exactly_the_abi() {
    let header = syscall_header();
    let abi = lazalith_os_abi::ABI_SYSCALLS;
    for (name, _) in abi {
        assert!(
            header.contains(&format!("long {name}(")),
            "the header declares {name}, because the ABI has it"
        );
    }
    let declared = header
        .lines()
        .filter(|line| line.starts_with("long "))
        .count();
    assert_eq!(
        declared,
        abi.len(),
        "and the header declares exactly the ABI's syscalls: no more, no fewer"
    );
    assert!(
        header.contains("GENERATED"),
        "and the header says it is generated, so nobody edits it by hand"
    );
}

/// Every generated declaration parses back to the ABI's own signature.
///
/// **This is the test that makes "generated" mean something.** A header is a string,
/// and a string can agree with the ABI by coincidence. Here each declaration is
/// extracted, handed to the C front end's own parser and resolver, and the resulting
/// function's type is compared with `abi_signature` — the same table the C compiler
/// checks calls against. If the header and the compiler ever disagree, this fails
/// rather than a C program silently reading the wrong register.
#[test]
fn the_generated_declarations_round_trip_through_the_c_front_end() {
    let header = syscall_header();
    let declarations: String = header
        .lines()
        .filter(|line| line.starts_with("long "))
        .map(|line| format!("{line};\n"))
        .collect();
    assert!(
        !declarations.is_empty(),
        "the header contributes at least one declaration to parse"
    );

    let mut sources = lazalith_types::SourceManager::new();
    let (source, checked) =
        lazalith_c_compiler::compile(&mut sources, "lazos/syscall.h", &declarations)
            .unwrap_or_else(|error| panic!("the generated header must parse:\n{}", error.render()));
    let _ = source;
    // `function_types`, not `functions`: a prototype has no body, so it is not a
    // *function* to this front end — which is exactly the fact that makes a
    // declaration-only header the right shape to test.
    for (name, _) in lazalith_os_abi::ABI_SYSCALLS {
        let expected = lazalith_c_compiler::types::abi_signature(name)
            .unwrap_or_else(|| panic!("the ABI has a type for {name}"));
        let found = checked
            .function_types
            .get(*name)
            .unwrap_or_else(|| panic!("the header declares {name} and the front end typed it"));
        assert_eq!(
            found, &expected,
            "the header's declaration for {name} and the ABI's own signature are the same \
             type. A C program that includes this header and calls {name} therefore \
             passes what the kernel will read"
        );
    }
}

/// The ABI header carries the record sizes and flags from the ABI's own constants.
#[test]
fn the_abi_header_carries_the_abi_constants() {
    let header = abi_header();
    for (name, value) in [
        ("IO_RESULT_SIZE", lazalith_os_abi::IO_RESULT_SIZE),
        ("FILE_STAT_SIZE", lazalith_os_abi::FILE_STAT_SIZE),
        (
            "DIRECTORY_RECORD_SIZE",
            lazalith_os_abi::DIRECTORY_RECORD_SIZE,
        ),
        (
            "EXIT_STATUS_RECORD_SIZE",
            lazalith_os_abi::EXIT_STATUS_RECORD_SIZE,
        ),
    ] {
        assert!(
            header.contains(&format!("LAZOS_{name} {value}")),
            "a C program needs {name} to size the buffer it passes to the ABI, and it \
             comes from the ABI's own constant: {header}"
        );
    }
    assert!(
        header.contains(&format!(
            "LAZOS_OPEN_ALL 0x{:08x}u",
            lazalith_os_abi::OPEN_ALL
        )),
        "and the `open` flags come from the ABI too"
    );
}

// -- and it is read by a real build -------------------------------------------

/// A C program built against a sysroot's C library, linked with its startup object,
/// and **run**.
///
/// **The test that a fake sysroot fails.** Everything above can be true of a directory
/// that no build reads. This one opens the sysroot, takes the C library text from
/// `lib/`, the startup object from `crt/`, compiles them, links, and executes the
/// image — and checks the program printed and returned what it was told to. A sysroot
/// that was written to disk and never read would fail here and nowhere else above.
#[test]
fn a_c_program_built_against_a_sysroot_runs() {
    use lazalith_driver::{CBuildOptions, c_entry, image_bytes};
    let (_scratch, sysroot) = hosted("builds");

    let library = sysroot
        .c_library()
        .expect("the hosted sysroot has a C library");
    assert!(
        library.contains("void print("),
        "and it is the C library the toolchain already had, read off disk rather than \
         than from a constant inside the compiler"
    );

    let object = lazalith_driver::compile_c(
        "int main(void) { print(\"from the sysroot\\n\"); return 9; }",
        &CBuildOptions {
            architecture: ArchitectureConfig::lz64(),
            source_path: String::from("sysroot.c"),
            runtime: library,
            library: false,
            headers: sysroot_headers(&sysroot),
        },
    )
    .expect("a C program builds against the sysroot's library");

    // Link the sysroot's *own* startup object rather than asking the linker to
    // generate one, which is the point of §19 asking for startup objects to be
    // separate: the image's start is a file in the sysroot, not a `format!` at the
    // moment of linking.
    let startup = sysroot
        .startup_object(ArchitectureConfig::lz64(), EntryLanguage::C)
        .expect("a startup object");
    let image = lazalith_toolchain::link_objects(
        &[object, startup],
        &lazalith_toolchain::LinkOptions {
            entry_symbol: Some(String::from(lazalith_runtime::STARTUP_LABEL)),
        },
    )
    .expect("the program links against the sysroot's startup object");
    let bytes = image_bytes(image.image()).expect("the image serialises");

    let finished = lazalith_runtime::run_image_with(
        &bytes,
        ArchitectureConfig::lz64(),
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program built against a sysroot should run: {error}"));
    let output = String::from_utf8_lossy(&finished.output);
    assert!(
        output.contains("from the sysroot"),
        "and it printed what it was told to: {output:?}"
    );
    assert_eq!(finished.exit_code, 9, "and returned what it returned");
    // `c_entry` is the symbol the *driver's* link would have used, named here so the
    // two cannot drift: the sysroot's startup calls the C entry, and that is the same
    // entry.
    assert_eq!(
        c_entry(),
        EntryLanguage::C.symbol(),
        "and the sysroot agrees with the driver about where a C image starts, because both \
         ask codegen for the mangling"
    );
}

/// The Lazen runtime a sysroot hands out is the runtime the toolchain already had.
#[test]
fn the_sysroot_runtime_is_the_runtime_the_toolchain_had() {
    let (_scratch, sysroot) = hosted("runtime");
    let from_disk = sysroot.lazen_runtime().expect("a runtime");
    assert_eq!(
        from_disk,
        lazalith_runtime::library_text(),
        "a sysroot that hands out a *different* standard library would be a second one, \
         and the whole point is that there is one"
    );
    assert!(
        sysroot.runtime().join(RUNTIME_FILE).is_file(),
        "and it is a file in runtime/, not a constant inside the compiler"
    );
    assert!(
        sysroot.lib().join(C_LIBRARY_FILE).is_file(),
        "the C library likewise, in lib/"
    );
}

/// The startup object's name says which machine it is for.
#[test]
fn the_startup_object_is_named_for_its_machine() {
    assert_eq!(
        startup_name(ArchitectureConfig::lz64(), EntryLanguage::Lazen),
        "crt1-lz64-lazen.lzo",
        "a startup object says which machine and which language it starts"
    );
    assert_eq!(
        startup_name(ArchitectureConfig::lz32(), EntryLanguage::C),
        "crt1-lz32-c.lzo",
        "and neither the machine nor the language collides"
    );
    assert_ne!(
        EntryLanguage::Lazen.symbol(),
        EntryLanguage::C.symbol(),
        "the two entries really are different symbols, which is why one startup object \
         could not serve both"
    );
}

// -- and the toolchain reads it -------------------------------------------------

/// `lazcc --sysroot` builds the same object as `lazcc` with no sysroot.
///
/// **The claim §19 makes is that a sysroot is the same target, described somewhere a
/// build can name it** — not a second library, not a different language, not a
/// different answer. The test is byte equality, and it is the strictest thing that
/// could be asked: if the sysroot's C library were a *copy* that had drifted, or if
/// reading it from disk changed a line number in the debug block, these bytes would
/// differ.
#[test]
fn the_toolchain_reads_a_sysroot_and_gets_the_same_object() {
    use lazalith_driver::tools::CcTool;
    let (_scratch, sysroot) = hosted("toolchain");

    let scratch = Scratch::new("toolchain-files");
    let source = scratch.at("prog.c");
    fs::write(
        &source,
        "int main(void) { print(\"same object\\n\"); return 4; }\n",
    )
    .expect("writable");

    let built_in = CcTool::run(&[std::ffi::OsString::from(source.display().to_string())])
        .expect("the built-in target builds");
    let from_sysroot = CcTool::run(&[
        std::ffi::OsString::from(source.display().to_string()),
        std::ffi::OsString::from("--sysroot"),
        std::ffi::OsString::from(sysroot.root().display().to_string()),
    ])
    .expect("the sysroot target builds");
    assert_eq!(
        built_in.1, from_sysroot.1,
        "the same C program, built against the built-in target and against a sysroot, \
         is the same object. A sysroot that changed the answer would be a second target"
    );
}

/// A build naming a sysroot that is not there is refused, not silently given the
/// built-in target instead.
///
/// This is the failure mode `--sysroot` exists to prevent. A `--sysroot` that was
/// quietly ignored would produce a working binary from a *different* library than the
/// one the user asked for, and nothing would say so — which is the same class of bug
/// as a linker quietly resolving a symbol it was not given.
#[test]
fn a_sysroot_that_is_not_there_is_refused() {
    use lazalith_driver::tools::CcTool;
    let scratch = Scratch::new("absent");
    let source = scratch.at("prog.c");
    fs::write(&source, "int main(void) { return 0; }\n").expect("writable");

    let error = CcTool::run(&[
        std::ffi::OsString::from(source.display().to_string()),
        std::ffi::OsString::from("--sysroot"),
        std::ffi::OsString::from(scratch.at("nowhere").display().to_string()),
    ])
    .expect_err("a named sysroot that is not there is an error");
    assert!(
        error.to_string().contains("not a sysroot"),
        "and it says the directory is not a sysroot, rather than building anyway: {error}"
    );

    // And a directory that has some of the layout is refused for the same reason.
    fs::create_dir_all(scratch.at("halfway/include")).expect("creatable");
    let error = CcTool::run(&[
        std::ffi::OsString::from(source.display().to_string()),
        std::ffi::OsString::from("--sysroot"),
        std::ffi::OsString::from(scratch.at("halfway").display().to_string()),
    ])
    .expect_err("a half-populated directory is not a sysroot");
    assert!(
        error.to_string().contains("not a sysroot"),
        "and says the same: {error}"
    );
}

/// A hosted build against a freestanding sysroot is refused by name.
#[test]
fn a_hosted_build_against_a_freestanding_sysroot_is_refused() {
    use lazalith_driver::tools::CcTool;
    let scratch = Scratch::new("wrong-flavour");
    lazalith_sysroot::Sysroot::create(
        scratch.at("kernel"),
        lazalith_sysroot::SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("a freestanding sysroot");
    let source = scratch.at("prog.c");
    fs::write(&source, "int main(void) { return 0; }\n").expect("writable");

    let error = CcTool::run(&[
        std::ffi::OsString::from(source.display().to_string()),
        std::ffi::OsString::from("--sysroot"),
        std::ffi::OsString::from(scratch.at("kernel").display().to_string()),
    ])
    .expect_err("a hosted build needs a C library, and a freestanding sysroot has none");
    assert!(
        error.to_string().contains("freestanding") && error.to_string().contains("C library"),
        "and the refusal explains which sysroot it was and what it lacks: {error}"
    );
}

/// The headers a sysroot has, as the driver would read them.
///
/// **A copy of what `BuildTarget::headers` does, and a copy is a smell** — so this is the
/// smallest possible one: it exists because `lazalith-sysroot`'s tests must not depend on
/// `lazalith-driver`'s view of a sysroot, since the whole point of `verify_c_library` is
/// that the sysroot crate does not know about the driver. The driver reads the same files
/// off the same disk, and `the_driver_reads_a_sysroots_headers_off_disk` is the test that
/// says so.
fn sysroot_headers(sysroot: &Sysroot) -> lazalith_driver::Headers {
    let mut headers = lazalith_driver::Headers::new();
    for (name, text) in sysroot.headers(ArchitectureConfig::lz64()) {
        headers.insert(&name, &text);
    }
    headers
}

/// A freestanding sysroot has the things §22 lists, and they are not empty.
///
/// **The test that says "freestanding" is a place, not a refusal.** Until now
/// `Sysroot::create` with `Freestanding` wrote two headers and a `lib/` with nothing in
/// it, and `a_freestanding_sysroot_refuses_the_hosted_library` was true for the wrong
/// reason: it was refusing a library that was never there in the first place. A
/// freestanding environment with nothing in it cannot build anything, so the claim that
/// it is a freestanding *environment* was not yet earned.
#[test]
fn a_freestanding_sysroot_has_headers_and_a_library() {
    let scratch = Scratch::new("freestanding-full");
    let sysroot = Sysroot::create(
        &scratch.at("sysroot"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("a freestanding sysroot is created");

    // The three headers C says a freestanding implementation must provide. All three, by
    // name, because "has some headers" is not the claim.
    for header in ["stddef.h", "stdint.h", "stdbool.h"] {
        let path = sysroot.root().join("include").join(header);
        assert!(
            path.is_file(),
            "a freestanding environment must provide <{header}>: C calls these the \
             freestanding headers, and an implementation without them has not \
             implemented freestanding"
        );
    }

    // The kernel library, under a name that is not the hosted one.
    let kernel = sysroot
        .kernel_library()
        .expect("a freestanding sysroot has a kernel library");
    assert!(
        kernel.contains("void *memcpy(") && kernel.contains("void *memset("),
        "a kernel cannot boot without the byte functions, and this is the low-level \
         runtime §22 asks for"
    );
    assert!(
        kernel.contains("int laz_puts("),
        "and a kernel that cannot print is a kernel whose bugs are invisible"
    );
    assert!(
        !kernel.contains("printf") && !kernel.contains("fopen(") && !kernel.contains("malloc"),
        "and none of the hosted library: a freestanding environment has no heap, no files \
         and no buffered output, and a library that declared them would be promising what \
         the environment cannot deliver"
    );

    // And the hosted library is still refused, which is the distinction §19 and §22 are
    // about. This is the assertion that means something now that there is a library here.
    assert!(
        sysroot.c_library().is_err(),
        "a freestanding sysroot still has no C library, and now that it has a kernel \
         library the refusal is about the *right* library"
    );
}

/// The freestanding headers are generated from the target, so the two targets differ.
///
/// **And they differ in the way that matters.** A 32-bit target gets a 32-bit `size_t` and
/// a `SIZE_MAX` it can actually represent. If both targets got the same text, the
/// generated-header argument — that a hand-written copy is a second place for the answer
/// to live — would be true of these headers too, and the whole reason they are generated
/// would be missing.
#[test]
fn the_freestanding_headers_follow_the_target() {
    let scratch_wide = Scratch::new("freestanding-lz64");
    let scratch_narrow = Scratch::new("freestanding-lz32");
    let wide = Sysroot::create(
        &scratch_wide.at("sysroot"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("an lz64 sysroot");
    let narrow = Sysroot::create(
        &scratch_narrow.at("sysroot"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz32(),
    )
    .expect("an lz32 sysroot");

    let stdint_of = |sysroot: &Sysroot| {
        std::fs::read_to_string(sysroot.root().join("include").join("stdint.h"))
            .expect("the header is on disk")
    };
    let wide_text = stdint_of(&wide);
    let narrow_text = stdint_of(&narrow);
    assert_ne!(
        wide_text, narrow_text,
        "the two targets get different <stdint.h>, because the widths in it are the \
         target's and not the header's"
    );
    assert!(
        wide_text.contains("#define SIZE_MAX 0xffffffffffffffffu"),
        "and lz64 says so"
    );
    assert!(
        narrow_text.contains("#define SIZE_MAX 0xffffffffu"),
        "and lz32 says so, rather than claiming a size it cannot address"
    );
    assert!(
        narrow_text.contains("typedef unsigned int uint32_t;"),
        "a 32-bit target's uint32_t is its own word"
    );
    assert!(
        wide_text.contains("typedef unsigned long uint32_t;"),
        "and a 64-bit target's uint32_t is half its word"
    );
}

/// The generated headers go through the C front end, because that is what a kernel does.
///
/// **A header that does not compile is not a header.** Both freestanding headers and both
/// LazOS headers are fed to the front end, and a program that includes them is compiled.
/// This is the test that would catch a `#define` with a typo in it, which is otherwise
/// invisible until a kernel is halfway through booting.
#[test]
fn the_freestanding_headers_compile_and_are_usable() {
    let scratch = Scratch::new("freestanding-compile");
    let sysroot = Sysroot::create(
        &scratch.at("sysroot"),
        SysrootFlavour::Freestanding,
        ArchitectureConfig::lz64(),
    )
    .expect("a freestanding sysroot");

    // Every header, on its own, must be includable. A header that needs another one to be
    // included first is a header that a program will get wrong.
    for (name, text) in sysroot.headers(ArchitectureConfig::lz64()) {
        let mut sources = lazalith_types::SourceManager::new();
        let analysis = lazalith_c_compiler::frontend::analyse(
            &mut sources,
            &format!("{name}.probe"),
            &text,
        );
        assert!(
            analysis.diagnostics.is_empty(),
            "the generated header {name} is not valid C on its own: {}",
            analysis
                .diagnostics
                .first()
                .map(|error| error.render())
                .unwrap_or_default()
        );
    }

    // And a program that uses them the way a kernel would.
    let program = "#include <stdint.h>\n\
                   #include <stddef.h>\n\
                   #include <stdbool.h>\n\
                   #include <lazos/syscall.h>\n\
                   unsigned long main(void) {\n\
                     uint32_t small = 7u;\n\
                     size_t width = sizeof(uint64_t);\n\
                     bool set = true;\n\
                     if (!set) { return 1; }\n\
                     return small + width;\n\
                   }";
    let object = lazalith_driver::compile_c(
        program,
        &lazalith_driver::CBuildOptions {
            architecture: ArchitectureConfig::lz64(),
            source_path: String::from("kernel.c"),
            runtime: sysroot.kernel_library().expect("the kernel library"),
            library: false,
            headers: sysroot_headers(&sysroot),
        },
    );
    assert!(
        object.is_ok(),
        "a freestanding program that includes a sysroot's headers builds: {}",
        object.err().map(|error| error.to_string()).unwrap_or_default()
    );
}
