//! `lazen check` — parse, resolve and type-check, generating nothing.
//!
//! `check` is the command a user runs most often, because it is the fastest one
//! and it is the one that finds mistakes. It exists as a separate command rather
//! than as a flag on `build` for a practical reason: `build` also runs code
//! generation and the linker, and a user who only wants to know whether their
//! program is well-formed should not have to wait for an image to find out.
//!
//! What `check` does *not* do is decide anything the later stages decide. It
//! stops after the type checker, so a program that type-checks but fails to lower
//! is reported by `build`, not here — and a program that type-checks is not
//! thereby known to run.
//!
//! # `check` sees the same program `build` does
//!
//! The runtime library is prepended to the user's text before the frontend runs,
//! because that is what makes a program that calls `rt::sys::print` well-formed.
//! So `check` prepends it too. Checking the user's file *alone* would report that
//! every name the library provides is undefined, and a `check` that fails on a
//! program `build` accepts is worse than no `check` at all.

use std::ffi::OsString;

use lazalith_compiler::frontend::compile;
use lazalith_types::SourceManager;

use crate::{CliError, Outcome, build_options, one_file, read_source};

/// `lazen check [file]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    check(one_file("check", arguments)?)
}

/// Checks one program and reports the outcome.
fn check(explicit: Option<&OsString>) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    let options = build_options(&path);
    let name = options.source_path.as_str();
    // The same composition `build` performs, so the two commands agree on what
    // the program *is*. The only stage skipped is the one after the checker.
    let unit = lazalith_runtime::compose(options.prelude.as_str(), text.as_str());
    let mut sources = SourceManager::new();
    match compile(&mut sources, name, unit.as_str()) {
        // `compile` is the whole frontend: lex, parse, resolve and type-check.
        // Stopping here is what makes this `check` rather than a slower `build`.
        Ok(_) => {
            println!("{name}: ok");
            Ok(Outcome::Done)
        }
        Err(error) => Err(CliError::Refused(error.render())),
    }
}
