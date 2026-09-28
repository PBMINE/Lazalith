//! Step 64: the runtime a Lazen program links against.
//!
//! # What a runtime is here
//!
//! Two things, and it is worth being exact about which is which.
//!
//! The **entry sequence** is machine code: it calls the program's `main` through
//! the documented convention and turns the result into an exit status. It cannot
//! be Lazen, because Lazen v1 has no function pointers and a function's name is
//! not a value, so nothing written in Lazen can call `main` by name.
//!
//! The **library** is Lazen: syscall wrappers, byte moves, and text helpers,
//! in [`source::PRELUDE`]. It goes through the same frontend, lowering and code
//! generation as a user program, so it is not a privileged component that can
//! disagree with the compiler about what the language means.
//!
//! # The build
//!
//! A program is one compilation unit: the prelude, then the program's own text.
//! [`build`] does that composition, lowers it, generates an object, links it with
//! the entry sequence, and hands back a `.lzx` image. Nothing in that path is
//! special-cased for the runtime: the prelude is text in front of the user's text,
//! and the entry sequence is an object beside the generated one.
//!
//! # What the runtime does not do
//!
//! - It does not decide anything the ABI decides. A wrapper builds the arguments
//!   the ABI names and returns the ABI's own status; the standard library in a
//!   later step is where a status becomes a value a program tests.
//! - It does not allocate. Lazen v1 has no heap, so a wrapper that needs an
//!   `IoResult` takes a caller-provided buffer, and a wrapper that needs memory
//!   asks the OS for it and returns an address the program cannot yet name.
//! - It does not set up the stack. The OS establishes `USER_INITIAL_SP` when it
//!   loads the image, and every generated prologue reserves its own frame,
//!   including the outgoing argument space, so the entry sequence's only stack
//!   obligation is to leave SP alone.

#![no_std]

extern crate alloc;

mod run;
mod source;
mod startup;

pub use run::{
    Finished, RunError, STEP_BUDGET, architecture_for, boot, run_image, run_image_on,
    run_image_with, run_loaded, supervisor_kernel,
};
pub use source::PRELUDE;
pub use startup::{
    ENTRY_SYMBOL, StartupError, startup_object, startup_object_for, startup_source,
    startup_source_for,
};

/// The whole of what a program is compiled against: the runtime's own text, then
/// the standard library's, then the GUI library's.
///
/// The three are separate crates and separate constants because they answer
/// different questions. `PRELUDE` is the minimum a program links: the syscall
/// wrappers, the entry sequence's requirements, and the memory and text helpers
/// those wrappers need. `STDLIB` is what a program *chooses* to use — `core`,
/// `io`, `text`, `math`, `collections`, `fs`, `time`, `process` — and it is
/// written on top of the prelude rather than beside it, so every standard library
/// call is a call through the same wrappers a raw program would use. `GUI` is the
/// first-party widget set, written on top of *both*, so a widget is a call
/// through the standard library rather than a second path to the hardware.
///
/// Keeping them apart is what lets a program opt out of the standard library. A
/// freestanding program that wants one syscall and nothing else builds with the
/// prelude alone, and pays for nothing it did not use.
///
/// The GUI library is appended for the same reason the standard library is, and
/// with the same cost: v1 resolves names only within one unit, so there is no
/// import machinery and every program carries the code for all of it. That is a
/// price worth paying once, and it is a price to revisit when v1 grows a way to
/// import a module rather than when it grows another module.
pub fn library_text() -> String {
    use alloc::string::String as StdString;
    let mut text = StdString::from(PRELUDE);
    for part in [lazalith_stdlib::STDLIB, lazalith_ui::UI] {
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
        text.push_str(part);
    }
    text
}

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::{error::Error, fmt};

use lazalith_codegen::{CodegenError, CodegenOptions, Program, generate};
use lazalith_compiler::lower::{self, LowerError};
use lazalith_toolchain::{LinkError, LinkOptions, LinkedProgram, ObjectFile, link_objects};
use lazalith_types::{ArchitectureConfig, SourceManager};

/// The name a program is compiled under when the caller does not choose one.
pub const DEFAULT_PROGRAM_NAME: &str = "main.lz";

/// A built program: the object the compiler produced, and the image it links
/// into.
///
/// Both are kept because a caller sometimes wants one and sometimes the other: a
/// test wants the object to look inside, and a loader wants the image.
#[derive(Clone, Debug)]
pub struct RuntimeProgram {
    architecture: ArchitectureConfig,
    program: Program,
    objects: Vec<ObjectFile>,
    linked: Option<LinkedProgram>,
}

/// How to build a program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildOptions {
    /// The machine to build for.
    pub architecture: ArchitectureConfig,
    /// The source name diagnostics point at.
    pub source_path: String,
    /// The library text compiled in front of the program.
    ///
    /// This is one string holding both the runtime prelude and, by default, the
    /// standard library. It is a field rather than a fixed choice so a program can
    /// be built freestanding — [`BuildOptions::freestanding`] — and so a caller
    /// that wants to see exactly what is linked can set it.
    pub prelude: String,
}

impl BuildOptions {
    /// Default options for a 64-bit machine: the prelude and the standard library.
    pub fn lz64(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            prelude: library_text(),
        }
    }

    /// Options for a 64-bit machine with the runtime alone.
    ///
    /// A program built this way has the syscall wrappers and the byte and text
    /// helpers, and none of `std`. That is the right build for something that wants
    /// one syscall and nothing else, and it is the build the runtime's own tests
    /// use, so the two libraries stay independently honest — a bug in `std` cannot
    /// make a runtime test pass, and a bug in the runtime cannot make a `std` test
    /// pass.
    pub fn freestanding(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            prelude: String::from(PRELUDE),
        }
    }
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self::lz64(DEFAULT_PROGRAM_NAME)
    }
}

impl RuntimeProgram {
    /// Compiles, lowers and generates `source` with the runtime in front of it.
    ///
    /// The result is not yet linked: a caller that only wants to inspect the
    /// generated object does not need a startup sequence or an image.
    pub fn build(source: &str, options: &BuildOptions) -> Result<Self, RuntimeError> {
        let unit = compose(options.prelude.as_str(), source);
        let mut sources = SourceManager::new();
        let (_, checked) =
            lazalith_compiler::compile(&mut sources, options.source_path.as_str(), &unit)
                .map_err(|error| RuntimeError::Compile(Rendered(error.render())))?;
        let lowered = lower::lower(&checked).map_err(RuntimeError::Lower)?;
        let program = generate(
            &lowered.module,
            &lowered.frames,
            &lowered.entry,
            &CodegenOptions {
                architecture: options.architecture,
                source_path: options.source_path.clone(),
            },
            &unit,
        )
        .map_err(RuntimeError::Codegen)?;
        let mut objects = Vec::new();
        objects
            .try_reserve(2)
            .map_err(|_| RuntimeError::Allocation)?;
        objects.push(program.object().clone());
        Ok(Self {
            architecture: options.architecture,
            program,
            objects,
            linked: None,
        })
    }

    /// The generated program: its object and the frames a loader needs.
    pub const fn program(&self) -> &Program {
        &self.program
    }

    /// The objects that would be linked, without the entry sequence.
    pub fn objects(&self) -> &[ObjectFile] {
        &self.objects
    }

    /// The machine this was built for.
    pub const fn architecture(&self) -> ArchitectureConfig {
        self.architecture
    }

    /// Links the program with the entry sequence and returns the image.
    ///
    /// The entry sequence is added here rather than at build time so that a
    /// caller who only wants the object never pays for assembling code it will
    /// not link, and so the link is the one place that decides what an image
    /// starts at.
    pub fn link(&self) -> Result<LinkedProgram, RuntimeError> {
        if let Some(linked) = &self.linked {
            return Ok(linked.clone());
        }
        let startup = startup_object(self.architecture).map_err(RuntimeError::Startup)?;
        let mut objects = Vec::new();
        objects
            .try_reserve(self.objects.len() + 1)
            .map_err(|_| RuntimeError::Allocation)?;
        objects.extend_from_slice(&self.objects);
        objects.push(startup);
        let linked = link_objects(
            &objects,
            &LinkOptions {
                entry_symbol: Some(String::from("entry")),
            },
        )
        .map_err(RuntimeError::Link)?;
        Ok(linked)
    }

    /// The linked program, once [`link`](Self::link) has run.
    pub fn linked(&self) -> Option<&LinkedProgram> {
        self.linked.as_ref()
    }

    /// Links and serialises the image, which is what a file on disk holds.
    pub fn to_image_bytes(&self) -> Result<Vec<u8>, RuntimeError> {
        let linked = self.link()?;
        linked.image().to_bytes().map_err(RuntimeError::Image)
    }

    /// The name of the entry function the image starts at.
    pub fn entry_symbol(&self) -> String {
        String::from("fn.main")
    }
}

/// The compilation unit a program is built from: the runtime, then the program.
///
/// Joins the runtime library and a program into one compilation unit.
///
/// The *program* comes first, and that ordering is load-bearing rather than
/// cosmetic. A diagnostic reports a line number into the unit's text, so a
/// prelude placed first would push every one of the user's own lines up by the
/// prelude's length — a one-line program reported an error at line 410, which
/// sends a user looking in a file that has four hundred lines. Putting the
/// program's text first keeps every line the user wrote at the line they wrote
/// it on, and the prelude's lines are the ones that shift, which is the correct
/// way round: the prelude is not a file the user is editing.
///
/// This is only sound because Lazen resolves names independent of order, so a
/// program may call a function the library declares below it. The resolver
/// already allows that, and `compose` depends on it — so if that ever changes,
/// this has to become a real line-number mapping rather than an ordering.
pub fn compose(prelude: &str, program: &str) -> String {
    let mut unit = String::with_capacity(program.len() + prelude.len() + 2);
    unit.push_str(program);
    if !program.ends_with('\n') {
        unit.push('\n');
    }
    unit.push('\n');
    unit.push_str(prelude);
    unit
}

/// A diagnostic that has already been rendered, so a caller can print it without
/// the compiler's own types.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rendered(String);

impl Rendered {
    /// The rendered diagnostic.
    pub fn text(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Rendered {}

/// Why a program could not be built.
#[derive(Debug)]
pub enum RuntimeError {
    /// The program did not compile.
    Compile(Rendered),
    /// The program did not lower.
    Lower(LowerError),
    /// The program did not generate code.
    Codegen(CodegenError),
    /// The entry sequence did not assemble.
    Startup(StartupError),
    /// The objects did not link.
    Link(LinkError),
    /// The image could not be serialised.
    Image(lazalith_os::LzxError),
    /// An allocation failed.
    Allocation,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compile(rendered) => write!(f, "{rendered}"),
            Self::Lower(error) => write!(f, "the program did not lower: {error}"),
            Self::Codegen(error) => write!(f, "the program did not generate code: {error}"),
            Self::Startup(error) => write!(f, "{error}"),
            Self::Link(error) => write!(f, "the program did not link: {error}"),
            Self::Image(error) => write!(f, "the image could not be built: {error}"),
            Self::Allocation => f.write_str("the toolchain ran out of memory"),
        }
    }
}

impl Error for RuntimeError {}

/// Whether the prelude declares every syscall the OS ABI numbers.
///
/// A wrapper for a syscall the ABI has not numbered would be a declaration the
/// frontend accepts and no call can resolve, so the count is checked rather than
/// assumed. This is a test-facing assertion, not a runtime check: the prelude is
/// a constant in this crate, so a mismatch is a bug here rather than a bad build.
pub fn prelude_is_complete(named: &[&str]) -> bool {
    let mut found = 0usize;
    for name in named {
        if PRELUDE.contains(&format!("fn {name}(")) {
            found += 1;
        }
    }
    found == named.len()
}
