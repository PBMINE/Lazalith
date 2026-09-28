//! `lazen fmt` — one canonical formatting style, applied.
//!
//! Two flags, and the difference between them is the whole design:
//!
//! - `lazen fmt` **rewrites** the file in place.
//! - `lazen fmt --check` **reports** whether it would, and changes nothing.
//!
//! The second is a separate flag rather than a mode because the first rewrites a
//! person's file and a script must not do that by accident. `is_formatted` is a
//! different function for the same reason: "would this change" and "change this" are
//! different questions, and answering one of them with the other is how a linter
//! ends up editing code in CI.

use std::{ffi::OsString, fs};

use lazalith_compiler::format as format_source;

use crate::{CliError, Outcome, one_file, read_source};

/// `lazen fmt [--check] [file]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let mut check = false;
    let mut rest: Vec<OsString> = Vec::new();
    for argument in arguments {
        match argument.to_string_lossy().as_ref() {
            "--check" => check = true,
            other if other.starts_with('-') => {
                return Err(CliError::usage_with_help(format!(
                    "{other} is not an option"
                )));
            }
            _ => rest.push(argument.clone()),
        }
    }
    let file = one_file("fmt", &rest)?;
    format(file, check)
}

/// Formats one file, or reports whether it is already formatted.
fn format(explicit: Option<&OsString>, check: bool) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    let formatted =
        format_source(text.as_str()).map_err(|error| CliError::Refused(error.to_string()))?;
    if formatted == text {
        // Already formatted. Saying so and exiting 0 is what lets a script run this
        // unconditionally; a file that needs nothing done is not a failure.
        println!("{} is formatted", path.display());
        return Ok(Outcome::Done);
    }
    if check {
        // A file that is not formatted is reported on stdout, not stderr, because
        // `--check` is a question and its answer is the point of the run.
        println!("{} is not formatted", path.display());
        print_difference(text.as_str(), &formatted);
        return Ok(Outcome::Refused);
    }
    fs::write(&path, formatted.as_bytes()).map_err(|error| CliError::io("write", &path, error))?;
    println!("formatted {}", path.display());
    Ok(Outcome::Done)
}

/// The first lines that differ, so a person does not have to run a diff to see why.
///
/// Bounded, because a whole file is not a report: the point is to show the first
/// divergence and say how many there are, not to reprint the file.
fn print_difference(before: &str, after: &str) {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    let mut differences = 0_usize;
    for (index, (left, right)) in before_lines.iter().zip(&after_lines).enumerate() {
        if left != right {
            if differences < 5 {
                println!("  line {}:", index + 1);
                println!("    - {left}");
                println!("    + {right}");
            }
            differences += 1;
        }
    }
    differences +=
        before_lines.len().max(after_lines.len()) - before_lines.len().min(after_lines.len());
    if differences > 5 {
        println!("  ... and {} more line(s)", differences - 5);
    }
}
