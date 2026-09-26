//! `lazen new` — scaffolding a project.
//!
//! A new project is one program file and nothing else. There is no manifest, no
//! lockfile and no build directory, because nothing in the toolchain reads one:
//! adding a file the tool ignores would teach a user to trust a file that does
//! nothing.
//!
//! The scaffold refuses to overwrite. `lazen new` in a directory that already
//! holds a project is a mistake, and the fix is to pick another name rather than
//! to lose work to a tool that assumed consent.

use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use crate::{CliError, MAIN_FILE, Outcome};

/// The program a fresh project starts with.
///
/// It is the same program as `examples/hello/main.lz`, and that is deliberate:
/// a user's first `lazen run` should print something, and the example is already
/// the thing the test suite proves runs.
const TEMPLATE: &str = include_str!("../../../examples/hello/main.lz");

/// A name a user typed, as a directory name.
///
/// A name that is empty, or that is `.` or `..`, or that contains a path
/// separator, is refused rather than sanitised: `lazen new ../escape` almost
/// certainly means something other than what it says, and quietly creating a
/// directory somewhere else is the worst response to that.
fn project_name(given: &OsString) -> Result<String, CliError> {
    let name = given.to_string_lossy().into_owned();
    let unusable = name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0');
    if unusable {
        return Err(CliError::usage_with_help(format!(
            "`{name}` is not a usable project name.\n\
             A project name is one directory: not empty, not `.` or `..`, and with \
             no path separator in it."
        )));
    }
    Ok(name)
}

/// `lazen new <name>`
///
/// `arguments` is already everything after the command name, so the project name
/// is the first of them.
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let extra: Vec<&OsString> = arguments.iter().collect();
    let Some(given) = extra.first() else {
        return Err(CliError::usage_with_help(
            "`lazen new` needs a project name.",
        ));
    };
    if extra.len() > 1 {
        return Err(CliError::usage_with_help(format!(
            "`lazen new` takes one project name, and {} were given.",
            extra.len()
        )));
    }
    let name = project_name(given)?;
    let directory = PathBuf::from(&name);
    if directory.exists() {
        return Err(CliError::Exists(directory));
    }
    fs::create_dir(&directory).map_err(|error| CliError::io("create", &directory, error))?;
    let program = directory.join(MAIN_FILE);
    fs::write(&program, TEMPLATE).map_err(|error| CliError::io("write", &program, error))?;
    println!("created {}/{}", directory.display(), MAIN_FILE);
    println!();
    println!("Next:");
    println!("    cd {name}");
    println!("    lazen run");
    Ok(Outcome::Done)
}
