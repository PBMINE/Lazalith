//! B14: the toolchain as separate tools.
//!
//! # What this crate is
//!
//! §18 asks to "move toward distinct user-facing tools" and lists candidate names. Before
//! choosing them I looked at what exists: **one binary, `lazen`, with eight
//! subcommands.** So the names are free, and `lazen` stays — §18 calls the compiler
//! driver a coordinator, and a coordinator is what it already is.
//!
//! The three tools here are the stages §18 says must be *independent*:
//!
//! | Tool | Stage | Consumes | Produces |
//! | --- | --- | --- | --- |
//! | [`lazcc`] | frontend + backend | `.lz`, `.c` | `.lzo` |
//! | [`lazas`] | assembler | `.la` | `.lzo` |
//! | [`lazld`] | link | `.lzo` | `.lzx` |
//!
//! # Why thin, and why that is the point
//!
//! Each `main` is argument handling and a call into `lazalith-driver`. **No tool
//! implements a stage**, because §18 says the driver "coordinates these stages instead
//! of implementing a parallel linker or object system", and a tool that grew its own
//! linker would be exactly the parallel system §18 forbids.
//!
//! `tests/stages.rs` holds the honest check: the object `lazcc` writes is byte-for-byte
//! the object `lazen build` produces, and the image `lazld` writes from it is
//! byte-for-byte the image `lazen build` produces. Two tools agreeing by construction
//! is the separation working; two tools agreeing by testing is a duplicate
//! implementation waiting to drift.
//!
//! # `lazcc` on a C file refuses, and says why
//!
//! The C frontend is complete — lex, parse, resolve, type-check, every diagnostic in a
//! file — and there is **no C-to-object backend**. So `lazcc foo.c` checks the file,
//! reports any diagnostic, and then says the backend is not built.
//!
//! It does not emit an empty object. That would move the failure from the tool to
//! whoever runs the program, and it is the sixth time this project has had the choice:
//! B4's `lza64-at-v1`, B5's unbuilt backends, B7's unimplemented VGA, B8's unbuilt
//! controllers, B9's unimplemented PS/2, B13's two firmwares. §14 makes C a first-class
//! target and B25 needs freestanding C, so the backend is required work — it is a stage
//! of its own, not a side effect of separating the toolchain.

#![deny(missing_docs)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::{BuildTarget, DriverError, IMAGE_SUFFIX, OBJECT_SUFFIX};

/// The exit code a tool uses when it refuses.
const EXIT_REFUSED: u8 = 2;

/// The version every tool reports, from the crate.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a tool prints for `--help`.
///
/// A tool that cannot explain itself is a tool whose refusals are the only
/// documentation, and the refusals are the case nobody has.
fn usage(tool: &str, summary: &str, inputs: &str) -> String {
    format!(
        "{tool} {VERSION} — {summary}\n\n  usage: {tool} {inputs} [-o <output>]\n  \
         the stages are described in docs/toolchain.md"
    )
}

/// What a tool was asked for, once `--help` and `--version` are out of the way.
enum Request {
    /// Print this and stop.
    Say(String),
    /// Do the work.
    Work,
}

/// Answers `--help` and `--version`, once, identically, for all three tools.
///
/// When either is asked for the work is *not* done: a script that asks a tool its
/// version and also passes a file should learn the version, not also have its
/// output silently replaced.
fn asked(arguments: &[OsString], tool: &str, summary: &str, inputs: &str) -> Request {
    let is = |name: &str| {
        arguments
            .iter()
            .any(|argument| argument.to_str() == Some(name))
    };
    if is("--version") || is("-V") {
        Request::Say(format!("{tool} {VERSION}"))
    } else if is("--help") || is("-h") {
        Request::Say(usage(tool, summary, inputs))
    } else {
        Request::Work
    }
}

/// Runs one tool: answer `--help`/`--version`, then work, then write or refuse.
///
/// The single exit path every tool shares, so the three cannot come to disagree
/// about exit codes, or about what lands on stdout versus stderr.
fn run_tool(
    arguments: &[OsString],
    tool: &str,
    summary: &str,
    inputs: &str,
    work: impl FnOnce() -> Result<(PathBuf, Vec<u8>), DriverError>,
) -> ExitCode {
    match asked(arguments, tool, summary, inputs) {
        Request::Say(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Request::Work => match work() {
            Ok((target, bytes)) => {
                if let Err(error) = write(&target, &bytes) {
                    return refuse(&error);
                }
                println!("{} ({} bytes)", target.display(), bytes.len());
                ExitCode::SUCCESS
            }
            Err(error) => refuse(&error),
        },
    }
}

/// A file a tool was asked to work on.
struct Job {
    source: PathBuf,
    /// Where the tool's output goes, when the caller did not say.
    target: PathBuf,
}

impl Job {
    /// The one file, and where its output belongs.
    ///
    /// **The default output is the source's name with the tool's suffix**, so building
    /// two programs in one directory does not have one silently overwrite the other.
    /// That is the same rule `lazen build` uses for images.
    fn from_source(path: &Path, suffix: &str) -> Self {
        let target = path.with_extension(suffix);
        Self {
            source: path.to_path_buf(),
            target,
        }
    }
}

/// Reports a refusal and returns the exit code.
///
/// One function so every tool fails the same way: the message on stderr, the same
/// non-zero code, and nothing printed to stdout that could be mistaken for output.
fn refuse(error: &DriverError) -> ExitCode {
    eprintln!("{error}");
    ExitCode::from(EXIT_REFUSED)
}

/// The one input file, from the command line.
///
/// An explicit `--output` is honoured; otherwise the output sits beside the source.
/// `--target` is *skipped* here and read by [`target_architecture`], so argument
/// parsing has one home per option rather than two that must agree about the value.
fn job(arguments: &[OsString], suffix: &str, tool: &str) -> Result<Job, String> {
    let mut source: Option<PathBuf> = None;
    let mut target: Option<PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy().into_owned();
        match argument.as_str() {
            "-o" | "--output" => {
                let value = arguments
                    .get(index + 1)
                    .ok_or_else(|| format!("{tool} --output needs a path"))?;
                target = Some(PathBuf::from(value));
                index += 1;
            }
            // `--target` and `--sysroot` take a value; `-c` and `--library` do not.
            // Both are skipped here and read by the function that owns them, so
            // argument parsing has one home per option rather than two that must
            // agree about where the value is.
            "--target" | "--sysroot" => {
                let _ = arguments
                    .get(index + 1)
                    .ok_or_else(|| format!("{tool} --target needs a machine"))?;
                index += 1;
            }
            "-c" | "--library" => {}
            other if other.starts_with('-') => {
                return Err(format!("{tool} does not take {other}"));
            }
            other => {
                if source.is_some() {
                    return Err(format!("{tool} takes one input file, and two were given"));
                }
                source = Some(PathBuf::from(other));
            }
        }
        index += 1;
    }
    let source = source.ok_or_else(|| format!("{tool} needs an input file"))?;
    let mut job = Job::from_source(&source, suffix);
    if let Some(target) = target {
        job.target = target;
    }
    Ok(job)
}

/// The machine `--target` asks for, or LZ64.
///
/// **Named, not guessed from the file name.** LZ64 is the default because it is the
/// only machine with a working backend, and a tool that inferred its target from a
/// path would produce output that depends on what a file is called. An unknown target
/// is refused rather than defaulted: a `--target` that is silently ignored is worse
/// than one that is rejected.
fn target_architecture(
    arguments: &[OsString],
    tool: &str,
) -> Result<lazalith_types::ArchitectureConfig, DriverError> {
    let index = match arguments
        .iter()
        .position(|argument| argument.to_str() == Some("--target"))
    {
        Some(index) => index,
        None => return Ok(lazalith_types::ArchitectureConfig::lz64()),
    };
    let value = arguments
        .get(index + 1)
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| DriverError::Io {
            path: tool.to_string(),
            message: format!("{tool} --target needs a machine"),
        })?;
    match value.as_str() {
        "lz64" | "lza64" | "lza64-at-v1" => Ok(lazalith_types::ArchitectureConfig::lz64()),
        "lz32" => Ok(lazalith_types::ArchitectureConfig::lz32()),
        other => Err(DriverError::Io {
            path: tool.to_string(),
            message: format!("{tool} does not know the machine {other}; try lz64 or lz32"),
        }),
    }
}

fn read(path: &Path) -> Result<String, DriverError> {
    fs::read_to_string(path).map_err(|error| DriverError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), DriverError> {
    fs::write(path, bytes).map_err(|error| DriverError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    })
}

// -- lazcc --------------------------------------------------------------------

/// `lazcc` — compile a Lazen or C program to a `.lzo` object.
///
/// Stops at the object. Linking is [`lazld`]'s job, and a compiler that also links
/// cannot be used to hand an object to a *different* linker, which is the whole of
/// §18's separation.
#[derive(Debug)]
pub struct CcTool;

/// Whether `-c` or `--library` was given.
///
/// **The `-c` of a real toolchain, and the same spelling.** A flag meaning "produce
/// an object, do not link" is called `-c` by every compiler a user has met, and
/// inventing a different name for it would be a worse decision than it is small. It
/// carries its long form because the long form says what it does.
fn library_flag(arguments: &[OsString]) -> bool {
    arguments
        .iter()
        .any(|argument| matches!(argument.to_str(), Some("-c") | Some("--library")))
}

/// The sysroot `--sysroot` names, or the built-in target.
///
/// **Absent is not an error.** No `--sysroot` means the target the toolchain was built
/// with, which is what every build did before §19 and what most builds should keep
/// doing: a sysroot is for a build that needs a *different* C library or runtime, and
/// requiring one would make the common case pay for the uncommon one. A `--sysroot`
/// that is *named* and cannot be opened is an error, because a build that asked for a
/// target and silently got another one has been lied to.
fn target_from(arguments: &[OsString], tool: &str) -> Result<BuildTarget, DriverError> {
    let index = match arguments
        .iter()
        .position(|argument| argument.to_str() == Some("--sysroot"))
    {
        Some(index) => index,
        None => return Ok(BuildTarget::BuiltIn),
    };
    let value = arguments
        .get(index + 1)
        .map(|value| value.to_string_lossy().into_owned())
        .ok_or_else(|| DriverError::Io {
            path: tool.to_string(),
            message: format!("{tool} --sysroot needs a directory"),
        })?;
    BuildTarget::open(value)
}

impl CcTool {
    /// The tool's name, for a usage message.
    pub const NAME: &'static str = "lazcc";

    /// Runs one command line and returns the bytes it would write.
    pub fn run(arguments: &[OsString]) -> Result<(PathBuf, Vec<u8>), DriverError> {
        let job = job(arguments, OBJECT_SUFFIX, Self::NAME).map_err(|message| DriverError::Io {
            path: Self::NAME.to_string(),
            message,
        })?;
        let architecture = target_architecture(arguments, Self::NAME)?;
        let target = target_from(arguments, Self::NAME)?;
        let library = library_flag(arguments);
        let text = read(&job.source)?;
        let source_path = job.source.display().to_string();
        let object = match job.source.extension().and_then(|ext| ext.to_str()) {
            // A `.c` file is a C program: the C frontend, the shared IR, the shared
            // backend. There is no branch downstream of this one.
            Some("c") => {
                let mut options = target.c_build_options(architecture, source_path)?;
                options.library = library;
                crate::compile_c(text.as_str(), &options)?
            }
            _ => {
                if library {
                    return Err(DriverError::Io {
                        path: Self::NAME.to_string(),
                        message: String::from(
                            "lazcc --library is for C translation units. A Lazen file with \\
                             no `fn main` is refused by the compiler, which is the right \\
                             answer for a language whose programs are entered at fn.main",
                        ),
                    });
                }
                crate::compile_lazen(
                    text.as_str(),
                    &target.build_options(architecture, source_path)?,
                )?
            }
        };
        let bytes = object.to_bytes()?;
        Ok((job.target, bytes))
    }

    /// Runs one command line and writes what it produced.
    pub fn main(arguments: &[OsString]) -> ExitCode {
        run_tool(
            arguments,
            Self::NAME,
            "compile a Lazen or C program to a `.lzo` object",
            "<file.lz|file.c> [--target lz64] [--sysroot DIR]",
            || Self::run(arguments),
        )
    }
}

// -- lazas --------------------------------------------------------------------

/// `lazas` — assemble LZA assembly into a `.lzo` object.
///
/// §18: "Assembly independently produces `.lzo`." Independence is total: this needs no
/// compiler, no prelude, no entry sequence, and no linker. A caller with a `.la` file
/// has an object and nothing else to know.
#[derive(Debug)]
pub struct AsTool;

impl AsTool {
    /// The tool's name, for a usage message.
    pub const NAME: &'static str = "lazas";

    /// Runs one command line and returns the bytes it would write.
    pub fn run(arguments: &[OsString]) -> Result<(PathBuf, Vec<u8>), DriverError> {
        let job = job(arguments, OBJECT_SUFFIX, Self::NAME).map_err(|message| DriverError::Io {
            path: Self::NAME.to_string(),
            message,
        })?;
        let text = read(&job.source)?;
        let object = crate::assemble(&job.source.display().to_string(), text.as_str())?;
        let bytes = object.to_bytes()?;
        Ok((job.target, bytes))
    }

    /// Runs one command line and writes what it produced.
    pub fn main(arguments: &[OsString]) -> ExitCode {
        run_tool(
            arguments,
            Self::NAME,
            "assemble LZA assembly into a `.lzo` object",
            "<file.la>",
            || Self::run(arguments),
        )
    }
}

// -- lazld --------------------------------------------------------------------

/// `lazld` — link `.lzo` objects into a `.lzx` image.
///
/// §18: "Linking independently consumes `.lzo` and libraries and produces `.lzx`." The
/// entry sequence is added here rather than at compile time, so a caller who only
/// wanted an object did not pay to assemble code that would not be linked, and the link
/// is the one place that decides what an image starts at.
#[derive(Debug)]
pub struct LdTool;

impl LdTool {
    /// The tool's name, for a usage message.
    pub const NAME: &'static str = "lazld";

    /// The entry symbol an image starts at, when the caller does not say.
    ///
    /// The driver's [`LAZEN_ENTRY`](crate::LAZEN_ENTRY), named here once so
    /// `lazld` and `lazen build` cannot end up disagreeing about where a program
    /// begins.
    pub const ENTRY: &'static str = crate::LAZEN_ENTRY;

    /// Runs one command line and returns the bytes it would write.
    pub fn run(arguments: &[OsString]) -> Result<(PathBuf, Vec<u8>), DriverError> {
        let mut sources: Vec<PathBuf> = Vec::new();
        let mut target: Option<PathBuf> = None;
        let mut entry: Option<String> = None;
        let mut index = 0;
        while index < arguments.len() {
            let argument = arguments[index].to_string_lossy().into_owned();
            match argument.as_str() {
                "-o" | "--output" => {
                    let value = arguments.get(index + 1).ok_or_else(|| DriverError::Io {
                        path: Self::NAME.to_string(),
                        message: format!("{} --output needs a path", Self::NAME),
                    })?;
                    target = Some(PathBuf::from(value));
                    index += 1;
                }
                "--entry" => {
                    let value = arguments.get(index + 1).ok_or_else(|| DriverError::Io {
                        path: Self::NAME.to_string(),
                        message: format!("{} --entry needs a symbol", Self::NAME),
                    })?;
                    entry = Some(value.to_string_lossy().into_owned());
                    index += 1;
                }
                other if other.starts_with('-') => {
                    return Err(DriverError::Io {
                        path: Self::NAME.to_string(),
                        message: format!("{} does not take {other}", Self::NAME),
                    });
                }
                other => sources.push(PathBuf::from(other)),
            }
            index += 1;
        }
        if sources.is_empty() {
            return Err(DriverError::Io {
                path: Self::NAME.to_string(),
                message: format!("{} needs at least one object", Self::NAME),
            });
        }
        let mut objects = Vec::new();
        let mut architecture = None;
        for source in &sources {
            let bytes = fs::read(source).map_err(|error| DriverError::Io {
                path: source.display().to_string(),
                message: error.to_string(),
            })?;
            let object = lazalith_toolchain::ObjectFile::from_bytes(&bytes)?;
            architecture.get_or_insert(object.target().architecture());
            objects.push(object);
        }
        let architecture = architecture.unwrap_or(lazalith_types::ArchitectureConfig::lz64());
        let image = crate::link(
            &objects,
            architecture,
            entry.as_deref().unwrap_or(Self::ENTRY),
        )?;
        let bytes = crate::image_bytes(&image)?;
        // The default image name comes from the *first* object, so `lazcc a.lz && lazld
        // a.lzo` produces `a.lzx` — the same file `lazen build a.lz` would.
        let target = target.unwrap_or_else(|| Job::from_source(&sources[0], IMAGE_SUFFIX).target);
        Ok((target, bytes))
    }

    /// Runs one command line and writes what it produced.
    pub fn main(arguments: &[OsString]) -> ExitCode {
        run_tool(
            arguments,
            Self::NAME,
            "link `.lzo` objects into a `.lzx` image",
            "<object.lzo>... [--entry <symbol>]",
            || Self::run(arguments),
        )
    }
}
