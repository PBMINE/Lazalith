//! `lazen run` and `lazen test` — executing an image under LazOS.
//!
//! Both commands run the program the same way, through the same boot handoff and
//! the same kernel, because a `run` that took a shortcut a `test` did not would
//! mean a program could pass its tests and fail when a person ran it.
//!
//! # The run itself
//!
//! The boot, the handoff, and the kernel loop live in `lazalith_runtime::run`.
//! They used to live here, for the reason that the runtime crate "deliberately
//! does not own a machine, because a library that started one would be a library
//! with a global" — which was true, and which was the wrong reason: a function
//! that *returns* a machine owns nothing global, and moving it put the one code
//! path in the project that boots a compiled program under a test.
//!
//! # The step budget
//!
//! A program that never exits must not hang the tool, so the run is bounded. The
//! bound is generous enough that a real program finishes and small enough that a
//! runaway is reported rather than waited on. Exceeding it is a *failure*, not a
//! timeout to be retried: a program that cannot finish has told us something.

use std::ffi::OsString;
use std::path::Path;

use lazalith_devices::{DeviceManager, NoDevice};

use crate::{CliError, Outcome, architecture_for, build_options, one_file, read_source};

/// What a finished run produced.
struct Finished {
    exit_code: u32,
    output: Vec<u8>,
}

/// `lazen run [file]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    run_one(one_file("run", arguments)?)
}

fn run_one(explicit: Option<&OsString>) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    execute(text.as_str(), &path).map(|finished| {
        // The program's output goes to this tool's stdout rather than being
        // captured: a user running a program expects to see it as it happens,
        // and buffering it to re-print would also reorder it against stderr.
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&finished.output);
        let _ = out.flush();
        Outcome::Exited {
            code: finished.exit_code,
        }
    })
}

/// `lazen test [file]`
///
/// A project's tests are the functions named `test_*` in its program file. That
/// is the whole convention: there is no test framework, no attribute and no
/// discovery file, because a test suite a user cannot read from the source is a
/// test suite they cannot add to.
pub(crate) fn tests(arguments: &[OsString]) -> Result<Outcome, CliError> {
    test_project(one_file("test", arguments)?)
}

fn test_project(explicit: Option<&OsString>) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    let names = test_names(text.as_str(), &path)?;
    if names.is_empty() {
        println!(
            "no tests in {}: a test is a function named `test_*` that takes no \
             arguments and returns 0",
            path.display()
        );
        return Ok(Outcome::Done);
    }
    let helpers = test_program_body(text.as_str());
    let mut failed = Vec::new();
    for name in &names {
        match run_test(text.as_str(), &path, name, helpers.as_str()) {
            Ok(()) => println!("test {name} ... ok"),
            Err(failure) => {
                println!("test {name} ... FAILED");
                println!("  {failure}");
                failed.push(name.clone());
            }
        }
    }
    let total = names.len();
    let passed = total - failed.len();
    println!();
    if failed.is_empty() {
        println!("{passed} passed; 0 failed");
        Ok(Outcome::Done)
    } else {
        println!("{passed} passed; {} failed", failed.len());
        for name in &failed {
            println!("  failed: {name}");
        }
        Ok(Outcome::Refused)
    }
}

/// The names of a source file's tests, in the order they are written.
///
/// The list comes from the *checked* program rather than from a text search, so a
/// name inside a comment or a string is not a test, a test written across lines is
/// still found, and a `test_*` that does not compile is a compile error rather
/// than a silently missing test. Going through the checker also means the names
/// are known good: nothing downstream has to wonder whether `test_foo` exists.
fn test_names(source: &str, path: &Path) -> Result<Vec<String>, CliError> {
    let name = path.to_string_lossy().into_owned();
    let options = build_options(path);
    let unit = lazalith_runtime::compose(options.prelude.as_str(), source);
    let mut sources = lazalith_types::SourceManager::new();
    let (_, program) = lazalith_compiler::compile(&mut sources, name.as_str(), unit.as_str())
        .map_err(|error| CliError::Refused(error.render()))?;
    Ok(program
        .functions
        .iter()
        // The library's own functions are named for their module, so a top-level
        // `test_*` is the user's. Checking the module is empty is what keeps a
        // library function from ever being mistaken for a test.
        .filter(|function| {
            function.module.is_empty()
                && function.name.starts_with("test_")
                && function.parameters.is_empty()
        })
        .map(|function| function.name.to_string())
        .collect())
}

/// The text of every top-level item except `main`.
///
/// A test may call a helper the project wrote, and a test that cannot call the
/// code next to it is not much of a test. So the program given to the compiler is
/// the project's own items minus `main`, whose place is taken by the generated one
/// that calls the test under examination.
///
/// `main` is removed with the same brace-aware extraction used for the test, not by
/// dropping its first line: dropping the line leaves the body, and a bare `return`
/// where an item is expected is a syntax error that has nothing to do with the test.
fn test_program_body(source: &str) -> String {
    let without_main = match function_source(source, "main") {
        // The spans are byte offsets, so removing the text between them leaves
        // every other item exactly where it was and keeps the newlines that
        // separated them.
        Some(text) => {
            let removed = source.len() - text.len();
            let start = source.find(text.as_str()).unwrap_or(0);
            let mut out = String::with_capacity(source.len() - removed);
            out.push_str(&source[..start]);
            out.push_str(&source[start + text.len()..]);
            out
        }
        // No `main` to remove, which is a perfectly good project: a file of tests
        // on its own is something a user writes.
        None => source.to_string(),
    };
    // The tests themselves are added back by the caller, so they are not
    // duplicated here.
    let _ = without_main;
    without_main
}

/// The text of one function, found by its declaration line.
///
/// The extraction is by brace depth from the line that starts `fn <name>`, which
/// needs no parser and no new crate. It is honest about its own limit: a `fn`
/// signature broken across lines, or a brace inside a string or a comment, would
/// confuse it. That is acceptable *here* and only here, because the text is fed
/// straight back to the compiler — if the extraction is wrong, the compiler says
/// so with a real diagnostic instead of the tool guessing. The alternative, a
/// full-fidelity slice out of the checked tree, would need the compiler to expose
/// item source ranges, which is more coupling than this command justifies.
fn function_source(source: &str, name: &str) -> Option<String> {
    let header = format!("fn {name}");
    let lines: Vec<&str> = source.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with(&header))?;
    let mut depth = 0i64;
    let mut saw_open = false;
    let mut collected = Vec::new();
    for line in &lines[start..] {
        collected.push(*line);
        for character in line.chars() {
            match character {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' => depth -= 1,
                _ => {}
            }
        }
        // The body is closed once a brace has been opened and the count is back
        // at zero: a function whose signature has no body yet is a syntax error
        // the compiler will report, not something to guess at here.
        if saw_open && depth == 0 {
            return Some(collected.join("\n"));
        }
    }
    None
}

/// Builds and runs one test function on its own.
///
/// A test runs as its own program, so a test that corrupts a frame or exits early
/// cannot affect the next one. The program is the test's *own* body with a `main`
/// that returns what the test returned, which is what makes "return 0" the
/// passing convention rather than an arbitrary one.
///
/// The `main` is generated and the user's own `main` is left out. Including it
/// would be a duplicate definition — the compiler is right to refuse one, and
/// `lazen test` refusing to run because a project happens to have a `main` would
/// make the command useless for every real program.
fn run_test(source: &str, path: &Path, test: &str, helpers: &str) -> Result<(), String> {
    // The test is already in `helpers`, which is the project's items minus
    // `main`; prepending its text again would define it twice. The function is
    // located only to give a real diagnostic when the name is not in the file at
    // all, which the checked-program scan should already have prevented.
    if function_source(source, test).is_none() {
        return Err(format!("`{test}` is not in {}.", path.display()));
    }
    let program_source = format!("fn main() -> i32 {{\n    return {test}();\n}}\n{helpers}");
    let finished = execute(&program_source, path).map_err(|error| error.to_string())?;
    // Whatever the test wrote is shown whether it passed or failed. A test that
    // prints is reporting something — a value, a diagnostic, a note to whoever
    // reads the output — and swallowing it because the test happened to return 0
    // would throw that away. It is shown after the verdict line so the reader
    // knows which test it belongs to.
    if !finished.output.is_empty() {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = out.write_all(&finished.output);
        if !finished.output.ends_with(b"\n") {
            let _ = out.write_all(b"\n");
        }
        let _ = out.flush();
    }
    if finished.exit_code == 0 {
        Ok(())
    } else {
        let mut printed = String::from("the test returned ");
        printed.push_str(&finished.exit_code.to_string());
        if !finished.output.is_empty() {
            printed.push_str("; it wrote: ");
            printed.push_str(String::from_utf8_lossy(&finished.output).trim_end());
        }
        Err(printed)
    }
}

/// Builds a program and runs it to completion under LazOS.
///
/// The run itself is the runtime's, so that `lazen run` and the step-96
/// integration test boot a program the *same* way. Two copies of this sequence
/// would drift, and the one that drifted would be the one nobody tested.
fn execute(source: &str, path: &Path) -> Result<Finished, CliError> {
    let architecture = architecture_for(path);
    let options = build_options(path);
    let program = lazalith_runtime::RuntimeProgram::build(source, &options)
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let bytes = program
        .to_image_bytes()
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let finished =
        lazalith_runtime::run_image_with(&bytes, architecture, DeviceManager::<NoDevice>::new())
            .map_err(|error| CliError::Refused(error.to_string()))?;
    Ok(Finished {
        exit_code: finished.exit_code,
        output: finished.output,
    })
}
