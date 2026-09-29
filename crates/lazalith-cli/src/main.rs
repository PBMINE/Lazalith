//! The `lazen` command-line toolchain.
//!
//! # Scope
//!
//! The roadmap says to provide commands *gradually*, and not to implement a
//! command whose underlying functionality does not exist yet. So this step
//! implements five of the six named commands and deliberately leaves `fmt` out:
//!
//! ```text
//! lazen new    scaffold a project
//! lazen check  parse, resolve and type-check, generating no output
//! lazen build  compile and link to a .lzx image
//! lazen run    build, then execute the image under LazOS
//! lazen test   run a project's tests and report pass or fail
//! ```
//!
//! There is no `lazen fmt` here. The formatter is Step 90, and a `fmt` that
//! silently rewrote nothing would be worse than a command that is absent: a user
//! who runs it would believe their file had been formatted.
//!
//! # Exit codes
//!
//! A build tool's exit code is its interface, so these are stated rather than
//! incidental:
//!
//! ```text
//! 0   success
//! 1   the program refused: a diagnostic, or a non-zero exit from `run`
//! 2   the command line was wrong
//! ```
//!
//! # Diagnostics
//!
//! A refused program is reported with the compiler's own rendered diagnostic,
//! which already carries the error code, the file, the line, the column and the
//! source line. This crate does not reformat that text and does not parse it: a
//! frontend that reads error strings back breaks whenever the message improves.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod build;
mod check;
mod deps;
mod format_cmd;
mod lazctl;
mod new;
mod pack;
mod run;
mod sysroot_cmd;

/// The exit code for a program that refused, by diagnostic or by exit status.
const EXIT_REFUSED: u8 = 1;
/// The exit code for a command line the tool could not act on.
const EXIT_USAGE: u8 = 2;

/// The name of a project's program file.
const MAIN_FILE: &str = "main.lz";
/// The name of a project's image, beside its source.
const MAIN_IMAGE: &str = "main.lzx";

/// What went wrong, in a form `main` can turn into an exit code and a message.
#[derive(Debug)]
pub(crate) enum CliError {
    /// The arguments did not name a command this tool has.
    Usage(String),
    /// A file could not be read or written.
    Io {
        /// What the tool was doing, in the user's terms.
        action: &'static str,
        /// The path it was doing it to.
        path: PathBuf,
        /// The error the operating system reported.
        source: std::io::Error,
    },
    /// A program did not compile, and this is the rendered diagnostic.
    Refused(String),
    /// A project directory has no program file in it.
    NotAProject(PathBuf),
    /// The file already exists, and overwriting it was not asked for.
    Exists(PathBuf),
}

impl CliError {
    /// Wraps an I/O error with the path and the action that failed.
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    /// A usage error with the whole usage text appended, so a user who mistyped
    /// a command is told what the commands are in the same breath.
    pub(crate) fn usage_with_help(message: impl Into<String>) -> Self {
        Self::Usage(format!("{}\n\n{}", message.into(), usage_text()))
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => f.write_str(message),
            Self::Io {
                action,
                path,
                source,
            } => write!(f, "could not {action} {}: {source}", path.display()),
            Self::Refused(rendered) => f.write_str(rendered),
            Self::NotAProject(path) => write!(
                f,
                "{} has no {MAIN_FILE}, so it is not a Lazen project.\n\
                 Run `lazen new <name>` to create one.",
                path.display()
            ),
            Self::Exists(path) => write!(
                f,
                "{} already exists. Choose another name, or remove it first.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// What a command decided.
///
/// `Refused` and `Exited` are separate because they mean different things to a
/// caller: `Refused` is the toolchain saying no, and `Exited` is a program that
/// built and ran and reported a status of its own.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    /// The command did what it was asked.
    Done,
    /// The command ran and the answer was no.
    Refused,
    /// A program ran and exited with this status.
    Exited {
        /// The status the program returned.
        code: u32,
    },
}

impl Outcome {
    /// The process exit code this outcome means.
    pub(crate) fn exit_code(&self) -> ExitCode {
        match self {
            Self::Done => ExitCode::SUCCESS,
            Self::Refused => ExitCode::from(EXIT_REFUSED),
            // A program's status is a byte on every platform this runs on, and a
            // status wider than a byte is clamped rather than refused: the program
            // ran, and its status is not this tool's to reject.
            Self::Exited { code } => ExitCode::from(*code as u8),
        }
    }
}

fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match dispatch(&arguments) {
        Ok(outcome) => outcome.exit_code(),
        Err(error) => {
            eprintln!("lazen: {error}");
            match error {
                CliError::Usage(_) => ExitCode::from(EXIT_USAGE),
                _ => ExitCode::from(EXIT_REFUSED),
            }
        }
    }
}

/// Runs one command line and reports what it decided.
fn dispatch(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let Some(command) = arguments.first() else {
        eprint!("{}", usage_text());
        return Ok(Outcome::Refused);
    };
    let command = command.to_string_lossy().into_owned();
    let rest = &arguments[1..];
    match command.as_str() {
        "new" => new::run(rest),
        "check" => check::run(rest),
        "build" => build::run(rest),
        "run" => run::run(rest),
        "test" => run::tests(rest),
        "pack" => pack::run(rest),
        "deps" => deps::run(rest),
        "vm" => lazctl::run(rest),
        "sysroot" => sysroot_cmd::run(rest),
        "fmt" => format_cmd::run(rest),
        "help" | "--help" | "-h" => {
            print!("{}", usage_text());
            Ok(Outcome::Done)
        }
        "--version" | "-V" => {
            println!("lazen {}", env!("CARGO_PKG_VERSION"));
            Ok(Outcome::Done)
        }
        other => Err(CliError::usage_with_help(format!(
            "`{other}` is not a lazen command."
        ))),
    }
}

/// The usage text, which names what this build of the tool can actually do.
pub(crate) fn usage_text() -> String {
    format!(
        "\
lazen {version} — the Lazen toolchain

USAGE
    lazen <command> [arguments]

COMMANDS
    new <name>       create a Lazen project in ./<name>
    check [file]     parse, resolve and type-check, generating nothing
    build [file]     compile and link to a {MAIN_IMAGE} image
    run [file]       build, then execute the program under LazOS
    test [file]      run a project's tests and report pass or fail
    pack [out]       write a .lza from a manifest and a built image
    deps             resolve a manifest's dependencies against local directories
    vm <command>    manage a VM through the VM Manager API: create, start, status, pause,
                     resume, reset, shutdown, snapshot, restore, clone, attach, detach
    sysroot <dir>    write a target sysroot: headers, libraries, runtime, startup objects
    fmt [--check] [file]
                     format a file in the canonical style, or report that it is not

    help             print this message
    --version        print the version

EXIT CODES
    0  success
    1  the program refused, or `run` reported a non-zero status
    2  the command line could not be acted on

`pack` takes `--manifest FILE` and `--image FILE`, and writes `NAME.lza` next to
the manifest by default. `deps` takes `--manifest FILE` and `--path DIR`, and looks
in the manifest's directory and `./packages` by default. There is no registry: a
dependency is a directory on disk, and `docs/lazen-packages.md` is the design.

`fmt` rewrites the file it is given; `fmt --check` changes nothing and exits 1 if a
file would change, which is the form a script wants. The style is in
`docs/lazen-formatting.md`, and the formatter only ever moves whitespace.
",
        version = env!("CARGO_PKG_VERSION"),
    )
}

/// The single optional file a command was pointed at.
///
/// `arguments` is everything *after* the command name, so a command that takes
/// one optional file looks at the first of them. Refusing a second one is not
/// fussiness: silently ignoring `lazen check a.lz b.lz` would check a file the
/// user did not ask about and report success, which is the worst thing a build
/// tool can do.
pub(crate) fn one_file<'a>(
    command: &str,
    arguments: &'a [OsString],
) -> Result<Option<&'a OsString>, CliError> {
    match arguments {
        [] => Ok(None),
        [only] => Ok(Some(only)),
        many => Err(CliError::Usage(format!(
            "`lazen {command}` takes one file, and {} were given: {}\n\n{}",
            many.len(),
            many.iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", "),
            usage_text(),
        ))),
    }
}

/// The source file a command was pointed at, defaulting to the project's.
///
/// The default is looked up in the current directory rather than assumed to
/// exist: a user who runs `lazen check` outside a project gets told which file
/// was looked for, which is more use than a bare "no such file".
pub(crate) fn source_path(explicit: Option<&OsString>) -> Result<PathBuf, CliError> {
    if let Some(given) = explicit {
        return Ok(PathBuf::from(given));
    }
    let default = PathBuf::from(MAIN_FILE);
    if default.exists() {
        return Ok(default);
    }
    Err(CliError::NotAProject(PathBuf::from(".")))
}

/// Reads the program source a command was pointed at.
pub(crate) fn read_source(explicit: Option<&OsString>) -> Result<(PathBuf, String), CliError> {
    let path = source_path(explicit)?;
    let text = fs::read_to_string(&path).map_err(|error| CliError::io("read", &path, error))?;
    Ok((path, text))
}

/// The directory a source file lives in, which is where its image belongs.
pub(crate) fn project_directory(source: &Path) -> PathBuf {
    source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// The image path a build produces beside `source`.
///
/// The stem follows the source, so `hello.lz` builds `hello.lzx` rather than
/// overwriting one image with another. A source with no stem at all still gets a
/// name, because a build that produced no file would be worse than a surprising
/// one.
pub(crate) fn image_path(source: &Path) -> PathBuf {
    let mut name = source.file_stem().map_or_else(
        || MAIN_IMAGE.into(),
        |stem| {
            let mut stem = stem.to_os_string();
            stem.push(".lzx");
            stem
        },
    );
    if name.is_empty() {
        name = MAIN_IMAGE.into();
    }
    project_directory(source).join(name)
}

/// The architecture a source path asks for.
///
/// The extension is the only signal a user gives, so `.lz32` selects the 32-bit
/// target. Lowering owns the decision about whether it can serve that target and
/// reports its own refusal; this only reads the request.
pub(crate) fn architecture_for(path: &Path) -> lazalith_types::ArchitectureConfig {
    if path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lz32"))
    {
        lazalith_types::ArchitectureConfig::lz32()
    } else {
        lazalith_types::ArchitectureConfig::lz64()
    }
}

/// The build options a source path implies.
///
/// The source name in the options is what diagnostics point at, so it is the
/// path the user gave rather than a fixed name: a diagnostic that says `main.lz`
/// when the user asked about `hello.lz` sends them to the wrong file.
pub(crate) fn build_options(path: &Path) -> lazalith_runtime::BuildOptions {
    lazalith_runtime::BuildOptions {
        architecture: architecture_for(path),
        source_path: path.to_string_lossy().into_owned(),
        // The whole library: the runtime's syscall wrappers and the standard
        // library written on top of them. This is the same text every command
        // uses, which is what makes `check` accept exactly what `build` builds.
        // It is not a file the user chooses or edits, so naming it here rather
        // than leaving it to a default keeps the option honest: a caller wanting
        // a different library would be visible in the call.
        prelude: lazalith_runtime::library_text(),
    }
}
