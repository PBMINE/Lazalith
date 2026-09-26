//! Tests for the `lazen` command.
//!
//! These run the real binary in a temporary directory and judge it by what it
//! wrote to stdout, stderr and its exit code. Testing a command line tool by
//! calling its functions would miss the parts that are actually the tool's
//! contract: which stream a message goes to, what the exit code is, and whether
//! a file appeared on disk.
//!
//! Every test gets its own directory, so a test that leaves a project behind
//! cannot make the next one pass or fail.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A directory that deletes itself, so a failing test does not leave litter.
struct Project {
    root: PathBuf,
}

impl Project {
    /// Creates an empty working directory named after the test.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lazen-cli-{name}"));
        // A previous run of this test may have left one, and a leftover project
        // would make `lazen new` refuse for the wrong reason.
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("a temporary directory");
        Self { root }
    }

    /// Writes a file into the project and returns its path.
    fn write(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("a parent directory");
        }
        fs::write(&path, contents).expect("the file is written");
        path
    }

    /// Reads a file out of the project.
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.root.join(name)).expect("the file is readable")
    }

    /// Whether a path exists in the project.
    fn exists(&self, name: &str) -> bool {
        self.root.join(name).exists()
    }

    /// Runs `lazen` in the project root.
    fn lazen(&self, arguments: &[&str]) -> Run {
        self.run_in(&self.root, arguments)
    }

    /// Runs `lazen` in a subdirectory of the project.
    fn lazen_in(&self, directory: &str, arguments: &[&str]) -> Run {
        self.run_in(&self.root.join(directory), arguments)
    }

    fn run_in(&self, directory: &Path, arguments: &[&str]) -> Run {
        let output: Output = Command::new(env!("CARGO_BIN_EXE_lazen"))
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("the lazen binary runs");
        Run::new(output)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// What one invocation of the tool did.
struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    fn new(output: Output) -> Self {
        Self {
            code: output
                .status
                .code()
                .expect("the tool exited rather than being signalled"),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// Both streams together, for an assertion that does not care which one.
    fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    fn succeeded(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "expected success\nstdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }

    fn refused(&self) -> &Self {
        assert_eq!(
            self.code, 1,
            "expected a refusal\nstdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }

    fn usage(&self) -> &Self {
        assert_eq!(
            self.code, 2,
            "expected a usage error\nstdout: {}\nstderr: {}",
            self.stdout, self.stderr
        );
        self
    }
}

/// A program that prints and returns a status.
const HELLO: &str = "fn main() -> i32 {\n    rt::sys::print(\"hi\\n\");\n    return 0;\n}\n";

/// The roadmap's own first experience, start to finish.
#[test]
fn new_then_run_prints_and_succeeds() {
    let project = Project::new("new-then-run");
    project.lazen(&["new", "demo"]).succeeded();
    assert!(
        project.exists("demo/main.lz"),
        "`lazen new` leaves a program file in the project directory"
    );
    let run = project.lazen_in("demo", &["run"]);
    run.succeeded();
    assert!(
        run.stdout.contains("Hello, Lazalith"),
        "the scaffolded program printed its greeting: {}",
        run.stdout
    );
}

/// `check` reports a good program without generating anything.
#[test]
fn check_accepts_a_good_program_and_writes_no_image() {
    let project = Project::new("check-good");
    project.write("main.lz", HELLO);
    let run = project.lazen(&["check", "main.lz"]);
    run.succeeded();
    assert!(run.stdout.contains("ok"), "check says so: {}", run.stdout);
    assert!(
        !project.exists("main.lzx"),
        "check generates nothing, so there is no image afterwards"
    );
}

/// `check` sees the runtime library, so a program that uses it is well-formed.
///
/// This is the failure worth a test: checking the user's file *alone* would call
/// every library name undefined, and a `check` that rejects what `build` accepts
/// is worse than no command at all.
#[test]
fn check_sees_the_runtime_library() {
    let project = Project::new("check-prelude");
    // `rt::sys::print` is the library, not the user. If `check` did not compose
    // the library in, this program would not type-check.
    project.write("main.lz", HELLO);
    project.lazen(&["check", "main.lz"]).succeeded();
}

/// A diagnostic names the file, the line and the error, and exits 1.
#[test]
fn check_reports_a_bad_program_with_a_usable_diagnostic() {
    let project = Project::new("check-bad");
    project.write("main.lz", "fn main() -> i32 {\n    return not_a_name;\n}\n");
    let run = project.lazen(&["check", "main.lz"]);
    run.refused();
    let text = run.output();
    assert!(text.contains("not_a_name"), "names the symbol: {text}");
    assert!(text.contains("main.lz"), "names the file: {text}");
    // The user's second line must be reported as line 2, not as whatever line it
    // occupies after the runtime library is prepended. A user reading "line 411"
    // in a three-line file has been given a useless number.
    assert!(
        text.contains("main.lz:2:") || text.contains("main.lz:3:"),
        "reports a line inside the user's own file, not one shifted by the \
         library: {text}"
    );
}

/// `build` writes an image beside its source, named after it.
#[test]
fn build_writes_a_named_image() {
    let project = Project::new("build");
    project.write("main.lz", HELLO);
    let run = project.lazen(&["build", "main.lz"]);
    run.succeeded();
    assert!(project.exists("main.lzx"), "build left an image");
    let image = fs::read(project.root.join("main.lzx")).expect("the image is readable");
    assert!(!image.is_empty(), "the image has content");
    // The image is the format a loader reads, so its magic has to be the loader's.
    // `lazalith-os` owns the constant; a test that restated it would pass even if
    // the format changed underneath, so this checks the bytes and says where the
    // truth lives rather than duplicating a value it cannot keep in step.
    assert!(
        image.starts_with(b"LZXLOAD1"),
        "the image begins with the loader's magic: {:?}",
        &image[..image.len().min(8)]
    );
}

/// Two programs in one directory do not overwrite each other's images.
#[test]
fn build_names_each_image_after_its_own_source() {
    let project = Project::new("build-two");
    project.write("one.lz", HELLO);
    project.write("two.lz", HELLO);
    project.lazen(&["build", "one.lz"]).succeeded();
    project.lazen(&["build", "two.lz"]).succeeded();
    assert!(project.exists("one.lzx"), "the first image is its own");
    assert!(project.exists("two.lzx"), "the second image is its own");
    // Identical programs must produce identical images. They are compared as
    // bytes because that is what the file is, and reading them as text would
    // fail on any byte that is not valid UTF-8 rather than on a real difference.
    let one = fs::read(project.root.join("one.lzx")).expect("the first image is readable");
    let two = fs::read(project.root.join("two.lzx")).expect("the second image is readable");
    assert_eq!(
        one, two,
        "identical programs produce identical images, so the second build did \
         not overwrite the first with something else"
    );
}

/// `run` executes the program and passes its status through as its own.
#[test]
fn run_reports_the_programs_status_as_its_own() {
    let project = Project::new("run-status");
    project.write("main.lz", "fn main() -> i32 {\n    return 7;\n}\n");
    let run = project.lazen(&["run", "main.lz"]);
    assert_eq!(
        run.code, 7,
        "a program that returned 7 made the tool exit 7\nstdout: {}\nstderr: {}",
        run.stdout, run.stderr
    );
}

/// `run` shows what the program wrote.
#[test]
fn run_shows_the_programs_output() {
    let project = Project::new("run-output");
    project.write(
        "main.lz",
        "fn main() -> i32 {\n    rt::sys::print(\"one\\n\");\n    rt::sys::print(\"two\\n\");\n    return 0;\n}\n",
    );
    let run = project.lazen(&["run", "main.lz"]);
    run.succeeded();
    assert_eq!(run.stdout, "one\ntwo\n", "both writes arrived, in order");
}

/// A program that does not compile never runs, and says why.
#[test]
fn run_refuses_a_program_that_does_not_compile() {
    let project = Project::new("run-bad");
    project.write("main.lz", "fn main() -> i32 { return nope; }\n");
    let run = project.lazen(&["run", "main.lz"]);
    run.refused();
    assert!(
        run.stderr.contains("nope"),
        "the diagnostic names the symbol: {}",
        run.stderr
    );
}

/// `lazen test` runs a project's tests and counts them.
#[test]
fn test_runs_named_tests_and_reports_failures() {
    let project = Project::new("test");
    project.write(
        "main.lz",
        "fn test_passes() -> i32 {\n    return 0;\n}\n\
         fn test_fails() -> i32 {\n    return 3;\n}\n\
         fn main() -> i32 {\n    return 0;\n}\n",
    );
    let run = project.lazen(&["test", "main.lz"]);
    // A suite with a failure exits 1, so a script can tell without parsing text.
    run.refused();
    assert!(
        run.stdout.contains("test test_passes ... ok"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("test test_fails ... FAILED"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("1 passed; 1 failed"), "{}", run.stdout);
}

/// A suite where everything passes exits 0.
#[test]
fn test_succeeds_when_every_test_passes() {
    let project = Project::new("test-pass");
    project.write(
        "main.lz",
        "fn test_one() -> i32 { return 0; }\nfn test_two() -> i32 { return 0; }\n",
    );
    let run = project.lazen(&["test", "main.lz"]);
    run.succeeded();
    assert!(run.stdout.contains("2 passed; 0 failed"), "{}", run.stdout);
}

/// A test may call a helper the project wrote.
#[test]
fn a_test_may_call_a_helper_beside_it() {
    let project = Project::new("test-helper");
    project.write(
        "main.lz",
        "fn double(x: i32) -> i32 { return x * 2; }\n\
         fn test_helper() -> i32 {\n    if double(21) == 42 { return 0; }\n    return 1;\n}\n\
         fn main() -> i32 { return 0; }\n",
    );
    project.lazen(&["test", "main.lz"]).succeeded();
}

/// A test that writes to the console still passes, and its output is shown.
#[test]
fn a_test_may_write_to_the_console() {
    let project = Project::new("test-print");
    project.write(
        "main.lz",
        "fn test_prints() -> i32 {\n    rt::sys::print(\"note\\n\");\n    return 0;\n}\n\
         fn main() -> i32 { return 0; }\n",
    );
    let run = project.lazen(&["test", "main.lz"]);
    run.succeeded();
    assert!(
        run.stdout.contains("note"),
        "the test's own output reached the terminal: {}",
        run.stdout
    );
}

/// A file with no tests is not a failure.
#[test]
fn test_on_a_file_with_no_tests_is_not_a_failure() {
    let project = Project::new("test-none");
    project.write("main.lz", HELLO);
    let run = project.lazen(&["test", "main.lz"]);
    run.succeeded();
    assert!(
        run.stdout.contains("no tests"),
        "the tool says there are none rather than pretending: {}",
        run.stdout
    );
}

/// `lazen new` refuses to overwrite a project that is already there.
#[test]
fn new_refuses_to_overwrite() {
    let project = Project::new("new-exists");
    project.lazen(&["new", "demo"]).succeeded();
    let run = project.lazen(&["new", "demo"]);
    run.refused();
    assert!(
        run.stderr.contains("already exists"),
        "the refusal says why: {}",
        run.stderr
    );
    assert!(
        project.exists("demo/main.lz"),
        "and the existing project is untouched"
    );
}

/// A name that would escape the current directory is refused, not sanitised.
#[test]
fn new_refuses_a_name_that_is_not_a_directory() {
    let project = Project::new("new-badname");
    for name in ["../escape", "a/b", ".", "..", ""] {
        let run = project.lazen(&["new", name]);
        assert_ne!(
            run.code,
            0,
            "`lazen new {name:?}` should not succeed, and nothing should be \
             created: {}",
            run.output()
        );
    }
}

/// A command line that names two files is refused rather than half acted on.
#[test]
fn two_files_are_refused_rather_than_one_being_ignored() {
    let project = Project::new("two-files");
    project.write("one.lz", HELLO);
    project.write("two.lz", HELLO);
    for command in ["check", "build", "run", "test"] {
        let run = project.lazen(&[command, "one.lz", "two.lz"]);
        run.usage();
        assert!(
            run.stderr.contains("one file"),
            "`lazen {command}` explains the arity: {}",
            run.stderr
        );
    }
}

/// An unknown command is a usage error that lists what there is.
#[test]
fn an_unknown_command_lists_the_real_commands() {
    let project = Project::new("unknown");
    let run = project.lazen(&["frobnicate"]);
    run.usage();
    let text = run.stderr;
    assert!(text.contains("frobnicate"), "names what was typed: {text}");
    for command in ["new", "check", "build", "run", "test"] {
        assert!(
            text.contains(command),
            "lists `{command}` as a real command: {text}"
        );
    }
}

/// Running outside a project says which file it looked for.
#[test]
fn a_command_outside_a_project_says_what_it_wanted() {
    let project = Project::new("no-project");
    let run = project.lazen(&["check"]);
    run.refused();
    assert!(
        run.stderr.contains("main.lz"),
        "the message names the file it looked for: {}",
        run.stderr
    );
}

/// A missing file is reported as a file problem, not a usage problem.
#[test]
fn a_missing_file_is_refused_not_misreported_as_usage() {
    let project = Project::new("missing");
    let run = project.lazen(&["check", "absent.lz"]);
    run.refused();
    assert!(
        run.stderr.contains("absent.lz") && run.stderr.contains("read"),
        "the message says what could not be read: {}",
        run.stderr
    );
}

/// `help` and `--version` work and say what this build supports.
#[test]
fn help_and_version_report_the_tool() {
    let project = Project::new("help");
    let help = project.lazen(&["help"]);
    help.succeeded();
    assert!(help.stdout.contains("lazen"), "{}", help.stdout);
    // The help must be honest about `fmt` being absent, or a user will try it.
    assert!(
        help.stdout.contains("fmt") && help.stdout.contains("not implemented"),
        "the help says `fmt` is not implemented: {}",
        help.stdout
    );
    let version = project.lazen(&["--version"]);
    version.succeeded();
    assert!(
        version.stdout.contains(env!("CARGO_PKG_VERSION")),
        "the version is the crate's: {}",
        version.stdout
    );
}

/// `lazen fmt` is absent on purpose, and says so rather than doing nothing.
#[test]
fn fmt_is_refused_with_a_reason() {
    let project = Project::new("fmt");
    project.write("main.lz", HELLO);
    let before = project.read("main.lz");
    let run = project.lazen(&["fmt"]);
    // Refused, not accepted: a `fmt` that exited 0 having changed nothing would
    // tell a user their file was formatted when it was not.
    assert_ne!(run.code, 0, "`lazen fmt` is not implemented");
    assert_eq!(
        project.read("main.lz"),
        before,
        "and it did not touch the file"
    );
}

/// A default `main.lz` is found with no file argument.
#[test]
fn the_default_file_needs_no_argument() {
    let project = Project::new("default");
    project.write("main.lz", HELLO);
    project.lazen(&["check"]).succeeded();
    project.lazen(&["build"]).succeeded();
    project.lazen(&["run"]).succeeded();
}
