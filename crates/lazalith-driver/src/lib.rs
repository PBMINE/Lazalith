//! B14: the toolchain as independent stages.
//!
//! # §18's chain, and what this crate is for
//!
//! ```text
//! source
//!  ↓
//! frontend
//!  ↓
//! Lazalith IR
//!  ↓
//! optimizer
//!  ↓
//! LZA backend
//!  ↓
//! .lzo
//!  ↓
//! lazld
//!  ↓
//! .lzx
//! ```
//!
//! Before this crate that chain existed only inside `RuntimeProgram::build`, which
//! compiled, lowered, generated *and* held the result, and the only way to get an
//! object out of it was to call a method that also knew how to link. A caller who
//! wanted a `.lzo` — to inspect it, to diff it, to hand it to a separate linker — had
//! no way to stop there.
//!
//! So the stages are functions here, each producing a **serialisable artifact**, and the
//! artifacts are the two formats the toolchain already has: `.lzo` for an
//! [`ObjectFile`], `.lzx` for an [`LzxImage`]. The separation is real because the
//! boundary is a file format, not a function call.
//!
//! # The driver coordinates; it does not reimplement
//!
//! §18: "The compiler driver coordinates these stages instead of implementing a parallel
//! linker or object system." Every function in this crate calls the crate that already
//! owns the work — `lazalith-compiler` for the frontend, `lazalith-codegen` for the
//! backend, `lazalith-toolchain` for the object and link. There is no second
//! assembler and no second linker here, and `tests/stages.rs` checks the strongest
//! version of that: the object `lazcc` writes and the object `lazen build` produces
//! are **byte-for-byte the same**.
//!
//! # Which frontends exist
//!
//! §18 lists Lazen, assembly, and §14 makes C a first-class target. All three have
//! frontends:
//!
//! | Frontend | Stage | Produces |
//! | --- | --- | --- |
//! | `lazcc` | [`compile_lazen`] / [`compile_c`] | `.lzo` |
//! | `lazas` | [`assemble`] | `.lzo` |
//! | `lazld` | [`link`] | `.lzx` |
//!
//! C is at the *frontend* stage only. There is no C-to-object backend yet — the C
//! compiler's job ends at a checked program and emitting an object from it is
//! unbuilt, which B25's freestanding-C work needs. [`compile_c`] therefore **refuses**
//! with a named error rather than pretending, for the same reason B13 refused two
//! firmwares and B7 refused VGA.

use core::fmt;

use lazalith_os::LzxImage;
use lazalith_runtime::{BuildOptions, RuntimeError, RuntimeProgram, STARTUP_LABEL};
use lazalith_toolchain::{LinkOptions, ObjectError, ObjectFile, link_objects};
use lazalith_types::ArchitectureConfig;

mod c_frontend;
mod lazen_frontend;
mod target;
pub mod tools;

pub use c_frontend::{C_OBJECT_ENTRY, CBuildOptions, CFrontendError, compile_c};
pub use lazen_frontend::{LazenFrontendError, compile_lazen};
pub use target::BuildTarget;

/// A stage of the toolchain, for a tool that reports which one it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Stage {
    /// Source to IR: lex, parse, resolve, check.
    Frontend,
    /// IR to an object: lower, optimise, emit.
    Backend,
    /// An assembly source to an object.
    Assembler,
    /// Objects to a linked image.
    Link,
    /// A linked image to bytes.
    Image,
}

impl Stage {
    /// The stage's name, for a diagnostic and for a `--verbose` line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::Backend => "backend",
            Self::Assembler => "assembler",
            Self::Link => "link",
            Self::Image => "image",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The suffix an object file has.
pub const OBJECT_SUFFIX: &str = "lzo";

/// The suffix an image file has.
pub const IMAGE_SUFFIX: &str = "lzx";

/// A refusal from the toolchain stages.
///
/// **One error type for the whole crate**, and that is a decision rather than a
/// convenience: a tool that ran four stages and failed in the third has to report one
/// error, and four error types would mean four shapes for a caller to handle and a
/// `?` at every stage boundary. The [`stage`](Self::stage) field says which stage
/// refused, so the information is not lost.
#[derive(Debug)]
pub enum DriverError {
    /// The Lazen frontend refused.
    Lazen {
        /// Which stage.
        stage: Stage,
        /// What it said, already rendered.
        message: String,
    },
    /// The C frontend refused, or is not built yet.
    C(CFrontendError),
    /// The assembler refused.
    Assembler {
        /// What it said, already rendered.
        message: String,
    },
    /// An object could not be written or read.
    Object(ObjectError),
    /// The linker refused.
    Link {
        /// What it said, already rendered.
        message: String,
    },
    /// The image could not be serialised.
    Image(String),
    /// A file named by the caller could not be read or written.
    Io {
        /// Which file.
        path: String,
        /// What the host said.
        message: String,
    },
    /// An object given to the linker was not for this machine.
    WrongArchitecture {
        /// The machine the object was built for.
        found: ArchitectureConfig,
        /// The machine the link is for.
        wanted: ArchitectureConfig,
    },
    /// A build stage failed inside `RuntimeProgram`, which owns its own error type.
    Runtime(RuntimeError),
    /// The sysroot was missing, incomplete, or of the wrong flavour.
    Sysroot {
        /// What it said, already rendered.
        error: lazalith_sysroot::SysrootError,
    },
}

impl DriverError {
    /// Which stage refused, when the error knows.
    ///
    /// An I/O error has no stage, because which stage was running is the caller's
    /// knowledge and not the error's. Returning `Option` rather than guessing.
    pub const fn stage(&self) -> Option<Stage> {
        match self {
            Self::Lazen { stage, .. } => Some(*stage),
            // A C diagnostic is the frontend saying no; a C lowering failure is the
            // frontend stage as well, since the IR is still the frontend's output;
            // and a codegen failure is the backend. An allocation is not a stage.
            Self::C(CFrontendError::Diagnostics { .. }) | Self::C(CFrontendError::Lower { .. }) => {
                Some(Stage::Frontend)
            }
            Self::C(CFrontendError::Codegen { .. }) => Some(Stage::Backend),
            Self::C(CFrontendError::Allocation) => None,
            Self::Assembler { .. } => Some(Stage::Assembler),
            Self::Object(_) => Some(Stage::Backend),
            Self::Link { .. } => Some(Stage::Link),
            Self::Image(_) => Some(Stage::Image),
            Self::Io { .. }
            | Self::WrongArchitecture { .. }
            | Self::Runtime(_)
            | Self::Sysroot { .. } => None,
        }
    }
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lazen { stage, message } => {
                write!(f, "the {stage} stage refused: {message}")
            }
            Self::C(source) => write!(f, "the C frontend refused: {source}"),
            Self::Assembler { message } => write!(f, "the assembler refused: {message}"),
            Self::Object(source) => write!(f, "{source}"),
            Self::Link { message } => write!(f, "the linker refused: {message}"),
            Self::Image(message) => write!(f, "the image could not be written: {message}"),
            Self::Io { path, message } => {
                write!(f, "{path}: {message}")
            }
            Self::WrongArchitecture { found, wanted } => write!(
                f,
                "an object was built for a {:?} machine and this link is for {:?}",
                found.word_width(),
                wanted.word_width()
            ),
            Self::Runtime(source) => write!(f, "{source}"),
            Self::Sysroot { error } => write!(f, "{error}"),
        }
    }
}

impl core::error::Error for DriverError {}

impl From<ObjectError> for DriverError {
    fn from(source: ObjectError) -> Self {
        Self::Object(source)
    }
}

/// Assembles LZA assembly into an object.
///
/// **Independence is the point.** This is §18's "Assembly independently produces
/// `.lzo`" — it does not need a C compiler, a Lazen compiler, a prelude, or an entry
/// sequence. A caller with a `.la` file has a `.lzo` and nothing else to know.
pub fn assemble(name: &str, source: &str) -> Result<ObjectFile, DriverError> {
    lazalith_toolchain::assemble_named(name, source).map_err(|error| DriverError::Assembler {
        message: error.to_string(),
    })
}

/// Compiles a Lazen program to an object, stopping before the link.
///
/// The runtime library is composed in front of the program by
/// [`BuildOptions`], because that composition is what makes a `.lz` program runnable
/// and is the same composition `lazen build` performs — `tests/stages.rs` checks the
/// two produce identical objects.
pub fn lazen_object(source: &str, options: &BuildOptions) -> Result<ObjectFile, DriverError> {
    let program = RuntimeProgram::build(source, options).map_err(DriverError::Runtime)?;
    Ok(program.objects()[0].clone())
}

/// Links objects into an image.
///
/// `entry_symbol` is the **program's** entry — the function the startup sequence
/// calls — and the image's own entry is [`STARTUP_LABEL`], the startup code itself.
/// Those are two different symbols and swapping them produces an image that starts
/// at a function with no stack and no exit, so the distinction is made here rather
/// than left to each caller.
///
/// The entry sequence is added here rather than at compile time, for the reason
/// `RuntimeProgram::link` gives: a caller who only wants an object does not pay to
/// assemble code it will not link, and the link is the one place that decides what an
/// image starts at. `lazld` therefore produces the same image `lazen build` does from
/// the same objects, which `tests/stages.rs` checks byte-for-byte.
pub fn link(
    objects: &[ObjectFile],
    architecture: ArchitectureConfig,
    entry_symbol: &str,
) -> Result<LzxImage, DriverError> {
    let mut all = Vec::new();
    all.try_reserve(objects.len() + 1)
        .map_err(|_| DriverError::Image(String::from("out of memory")))?;
    all.extend_from_slice(objects);
    all.push(
        lazalith_runtime::startup_object_for(architecture, entry_symbol).map_err(|error| {
            DriverError::Link {
                message: error.to_string(),
            }
        })?,
    );
    for object in objects {
        if object.target().architecture() != architecture {
            return Err(DriverError::WrongArchitecture {
                found: object.target().architecture(),
                wanted: architecture,
            });
        }
    }
    let linked = link_objects(
        &all,
        &LinkOptions {
            // The startup, **not** `entry_symbol`. See above: the image starts at
            // the code that establishes the machine, and that code calls
            // `entry_symbol`.
            entry_symbol: Some(String::from(STARTUP_LABEL)),
        },
    )
    .map_err(|error| DriverError::Link {
        message: error.to_string(),
    })?;
    Ok(linked.into_image())
}

/// Serialises an image to the bytes a file holds.
pub fn image_bytes(image: &LzxImage) -> Result<Vec<u8>, DriverError> {
    image
        .to_bytes()
        .map_err(|error| DriverError::Image(error.to_string()))
}

/// The entry a C program is started at, as a caller must name it to a linker.
///
/// **The object symbol, not the IR name.** A C `main` is `c.main` in the IR and
/// `fn.c.main` in the object, because code generation mangles every function as
/// `fn.<ir-name>`. A caller asking the linker for `c.main` gets "global symbol
/// undefined", which is the same class of mistake as asking for a Rust field
/// instead of a method: the name exists, just one layer earlier than you think.
/// [`C_OBJECT_ENTRY`] is the constant; this is the accessor, so the two cannot
/// drift.
pub fn c_entry() -> String {
    String::from(C_OBJECT_ENTRY)
}

/// The entry a Lazen program is started at.
///
/// Named once, here, because the link stage needs it and the tools that report a
/// build need it, and two spellings of the same entry symbol is one of them being
/// wrong.
pub const LAZEN_ENTRY: &str = "fn.main";

/// What a whole Lazen build produced, at every stage boundary.
///
/// `lazen build` needs more than bytes: it prints the entry symbol, and a caller
/// that wants to know *why* an image is a particular size needs the object it came
/// from. Returning all three keeps the stages reachable without a caller
/// re-deriving them by hand.
#[derive(Clone, Debug)]
pub struct LazenBuild {
    /// The compiled program, before the link.
    pub object: ObjectFile,
    /// The linked image.
    pub image: LzxImage,
    /// The image's bytes, as a file holds them.
    pub bytes: Vec<u8>,
}

impl LazenBuild {
    /// The entry this build starts at.
    pub const fn entry_symbol(&self) -> &'static str {
        LAZEN_ENTRY
    }
}

/// The whole chain from Lazen source to a linked image, in the stages' own order.
///
/// **This is what `lazen build` calls**, so the driver and the separate tools cannot
/// drift: they are the same code path with a file written in the middle.
pub fn build_lazen(source: &str, options: &BuildOptions) -> Result<LazenBuild, DriverError> {
    let object = lazen_object(source, options)?;
    let image = link(
        core::slice::from_ref(&object),
        options.architecture,
        LAZEN_ENTRY,
    )?;
    let bytes = image_bytes(&image)?;
    Ok(LazenBuild {
        object,
        image,
        bytes,
    })
}

/// The whole chain from Lazen source to image bytes.
///
/// [`build_lazen`] for a caller who wants only the file.
pub fn build_lazen_image(source: &str, options: &BuildOptions) -> Result<Vec<u8>, DriverError> {
    build_lazen(source, options).map(|build| build.bytes)
}
