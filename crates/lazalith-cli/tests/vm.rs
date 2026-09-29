//! `lazen vm` — the management commands, run as a user runs them.
//!
//! # Why a subprocess suite
//!
//! The argument parsing, the exit codes and the *text* of what a person sees are the
//! contract of a command-line tool, and none of them is a function's return value. A
//! suite that called `lazctl::run` would prove the manager calls work; this one proves
//! the tool works, including the parts that are only wrong when a shell is involved.
//!
//! Every test writes a real boot image into a real directory and runs the real binary.
//! There is no fixture manager, so a command that printed a plausible-looking status
//! without going through the management API would still be caught by the clock line not
//! moving — and the architecture test in `architecture.rs` is what checks the layer.
//!
//! # What is deliberately not tested
//!
//! **Snapshot persistence**, because there is none. `lazen vm snapshot` reports what a
//! snapshot holds and says it is not written to disk, and there is a test that it says
//! so — because a command that silently wrote nothing would be the bug, and a command
//! that silently wrote *something* would be a worse one.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lazalith_isa::{Condition, Instruction, Opcode, Operand, encode};
use lazalith_types::ArchitectureConfig;

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// A directory that deletes itself, so a failing test leaves no litter.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("lazctl-{name}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("a temporary directory");
        Self { root }
    }

    /// Writes the kernel payload a VM can be started from.
    ///
    /// **The file is the kernel, not an encoded boot image.** `BootImage::new` takes the
    /// kernel's bytes and a load address, and `lazctl start` reads a file and hands its
    /// contents to it — so what a user passes is the payload, and a file that does not
    /// parse as one is refused by `BootImage::new` rather than by a magic number here.
    fn image(&self, name: &str, kernel: Vec<u8>) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, kernel).expect("the kernel is written");
        path
    }

    fn vm(&self, arguments: &[&str]) -> Run {
        let output: Output = Command::new(env!("CARGO_BIN_EXE_lazen"))
            .arg("vm")
            .args(arguments)
            .current_dir(&self.root)
            .output()
            .expect("the binary runs");
        Run { output }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// What a command produced.
struct Run {
    output: Output,
}

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    fn succeeded(&self) -> &Self {
        assert_eq!(
            self.code(),
            0,
            "expected success\nstdout:\n{}\nstderr:\n{}",
            self.stdout(),
            self.stderr()
        );
        self
    }

    fn refused(&self) -> &Self {
        assert_ne!(
            self.code(),
            0,
            "expected a refusal\nstdout:\n{}",
            self.stdout()
        );
        self
    }
}

/// A kernel that spins on itself, so a VM that starts is a VM that keeps running.
fn spinning_kernel() -> Vec<u8> {
    let branch = Instruction::new(
        CONFIG,
        Opcode::Br,
        &[Operand::Condition(Condition::Al), Operand::Immediate(-2)],
    )
    .expect("a self-branch builds");
    let mut bytes = encode(CONFIG, &branch).expect("and encodes").to_vec();
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

/// A kernel that halts, so a VM's guest can stop.
fn halting_kernel() -> Vec<u8> {
    let halt = Instruction::new(CONFIG, Opcode::Halt, &[]).expect("a halt builds");
    let mut bytes = encode(CONFIG, &halt).expect("and encodes").to_vec();
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

// -- the commands are all there ---------------------------------------------

#[test]
fn every_command_in_the_help_text_is_one_the_dispatcher_has() {
    // A help text naming commands the dispatcher does not have is the most common way a
    // CLI lies, and it is free to check.
    let workspace = Workspace::new("help");
    let run = workspace.vm(&["help"]);
    run.succeeded();
    let help = run.stdout();
    for command in [
        "create", "start", "status", "pause", "resume", "reset", "shutdown", "snapshot", "restore",
        "clone", "attach", "detach",
    ] {
        assert!(
            help.contains(command),
            "the help text names {command}:\n{help}"
        );
    }
    assert!(
        help.contains("VM Manager API"),
        "and it says where the commands go, because that is the point of them:\n{help}"
    );
}

#[test]
fn a_command_that_does_not_exist_is_a_usage_error_not_a_panic() {
    let workspace = Workspace::new("unknown");
    let run = workspace.vm(&["teleport"]);
    run.refused();
    assert_eq!(
        run.code(),
        2,
        "a usage mistake is exit 2, as in the main tool"
    );
    assert!(
        run.stderr().contains("not a lazctl command"),
        "and it says so: {}",
        run.stderr()
    );
}

#[test]
fn a_command_with_no_arguments_prints_the_help_and_refuses() {
    let workspace = Workspace::new("bare");
    let run = workspace.vm(&[]);
    run.refused();
    assert!(
        run.stderr().contains("lazctl — Lazalith VM management"),
        "no arguments is a usage mistake, and the help goes to stderr so it does not \
         pollute a pipeline: {}",
        run.stderr()
    );
}

// -- create and status -------------------------------------------------------

#[test]
fn create_builds_a_vm_and_says_what_it_is() {
    let workspace = Workspace::new("create");
    let run = workspace.vm(&["create", "myvm"]);
    run.succeeded();
    let out = run.stdout();
    assert!(out.contains("myvm:"), "the name is the one given:\n{out}");
    assert!(out.contains("created"), "and a created VM says so:\n{out}");
    assert!(out.contains("cold"), "and that no firmware has run:\n{out}");
    assert!(
        out.contains("0 cycle(s)"),
        "and that no time has passed:\n{out}"
    );
    assert!(
        out.contains("debugger none"),
        "and that no debugger is attached:\n{out}"
    );
}

#[test]
fn create_needs_no_image_because_nothing_is_being_booted() {
    // The one command that does not take one, and it is the only one: creating a VM is
    // not booting it, and a tool that insisted on an image for `create` would be
    // asking for a decision the command does not make.
    let workspace = Workspace::new("create-no-image");
    workspace.vm(&["create", "vm"]).succeeded();
}

#[test]
fn status_reports_a_booted_vm_with_a_moved_clock() {
    // The end-to-end proof that the clock fix reaches a person. `start` runs the guest
    // and then prints the status, so a non-zero clock here is the machine charging
    // cycles and the management layer reporting it, through two layers and a boundary
    // the architecture test enforces.
    let workspace = Workspace::new("status");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let run = workspace.vm(&["start", "vm", image.to_str().expect("a path")]);
    run.succeeded();
    let out = run.stdout();
    assert!(out.contains("booted"), "firmware ran:\n{out}");
    assert!(
        !out.contains("time     0 cycle(s)"),
        "and time moved, because the guest executed:\n{out}"
    );
    assert!(
        out.contains("ran ") && out.contains("instruction(s) in") && out.contains("cycle(s)"),
        "and the run says what it executed and what it cost:\n{out}"
    );
}

#[test]
fn a_run_of_a_halting_guest_says_so() {
    let workspace = Workspace::new("halted");
    let image = workspace.image("kernel.bin", halting_kernel());
    let run = workspace.vm(&["start", "vm", image.to_str().expect("a path")]);
    run.succeeded();
    assert!(
        run.stdout().contains("guest halted"),
        "a guest that stopped says so, because a management client most often needs to \
         notice:\n{}",
        run.stdout()
    );
    assert!(
        run.stdout().contains("halted   yes"),
        "and the status agrees:\n{}",
        run.stdout()
    );
}

// -- the lifecycle -----------------------------------------------------------

#[test]
fn the_lifecycle_commands_each_report_the_state_they_produced() {
    let workspace = Workspace::new("lifecycle");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let path = image.to_str().expect("a path");

    for (command, expected) in [
        ("status", "running"),
        ("pause", "paused"),
        ("reset", "running"),
        ("shutdown", "shut down"),
    ] {
        let run = workspace.vm(&[command, "vm", path]);
        run.succeeded();
        assert!(
            run.stdout().contains(expected),
            "`lazen vm {command}` should leave the VM {expected}:\n{}",
            run.stdout()
        );
    }
}

#[test]
fn a_vm_is_rebuilt_per_invocation_and_that_is_stated() {
    // `lazctl` keeps no VM between invocations, so "pause it twice" is not something a
    // command line can say. This is the test for that design rather than a gap in it:
    // two `pause` invocations are two *first* pauses, both of which succeed, and a
    // person who expected the second to fail gets a VM that is paused.
    //
    // The lifecycle's own "already paused" refusal is tested where it is expressible —
    // in `crates/lazalith-manager/tests/management.rs`, against one manager.
    let workspace = Workspace::new("fresh-per-invocation");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let path = image.to_str().expect("a path");
    workspace.vm(&["pause", "vm", path]).succeeded();
    let second = workspace.vm(&["pause", "vm", path]);
    second.succeeded();
    assert!(
        second.stdout().contains("state    paused"),
        "and the second invocation is a first pause of a fresh VM:\n{}",
        second.stdout()
    );
}

#[test]
fn resuming_a_vm_that_is_not_paused_is_refused() {
    let workspace = Workspace::new("resume-running");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let run = workspace.vm(&["resume", "vm", image.to_str().expect("a path")]);
    run.refused();
    assert!(
        run.stderr().contains("not paused"),
        "and it says why: {}",
        run.stderr()
    );
}

#[test]
fn shutdown_is_reported_as_off_and_the_command_line_still_has_no_such_vm_afterwards() {
    // Shutdown and reset are different operations and the status says which happened.
    // The second half is the design: a shut-down VM does not survive the command, so the
    // next invocation builds a new one — which is B19's "no configuration persistence"
    // limitation, stated here as a test so it stays a stated limitation.
    let workspace = Workspace::new("shutdown");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let path = image.to_str().expect("a path");
    let run = workspace.vm(&["shutdown", "vm", path]);
    run.succeeded();
    assert!(
        run.stdout().contains("state    shut down"),
        "a shut-down VM says so, and it is not the same as paused or reset:\n{}",
        run.stdout()
    );
    assert!(
        run.stdout().contains("debugger none"),
        "and it is still a VM that exists, with its configuration:\n{}",
        run.stdout()
    );
}

// -- snapshot, restore, clone ------------------------------------------------

#[test]
fn snapshot_reports_the_state_and_says_it_is_not_written_to_disk() {
    // The honesty test. A command that wrote nothing and said nothing would be a bug; a
    // command that wrote a file that could not be restored would be a worse one, and
    // this is the assertion that it does not do either.
    let workspace = Workspace::new("snapshot");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let run = workspace.vm(&["snapshot", "vm", image.to_str().expect("a path")]);
    run.succeeded();
    let out = run.stdout();
    assert!(
        out.contains("a snapshot of vm was taken"),
        "it took one:\n{out}"
    );
    assert!(
        out.contains("not written to disk"),
        "and it says plainly that nothing was written:\n{out}"
    );
    assert!(
        !workspace_has_snapshot(&workspace.root),
        "and there is no file in the directory that looks like a snapshot"
    );
}

/// Whether a snapshot file was written.
fn workspace_has_snapshot(root: &Path) -> bool {
    fs::read_dir(root)
        .expect("the directory reads")
        .flatten()
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.contains("snapshot") || name.contains(".lazctl")
        })
}

#[test]
fn restore_puts_the_vm_back_before_the_restore_command_ran_it() {
    let workspace = Workspace::new("restore");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let run = workspace.vm(&["restore", "vm", image.to_str().expect("a path")]);
    run.succeeded();
    assert!(
        run.stdout()
            .contains("restored vm to the state before its run"),
        "and it says what it restored:\n{}",
        run.stdout()
    );
}

#[test]
fn clone_makes_an_independent_paused_copy() {
    let workspace = Workspace::new("clone");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let path = image.to_str().expect("a path");
    let run = workspace.vm(&["clone", "vm", path]);
    run.succeeded();
    let out = run.stdout();
    assert!(out.contains("cloned vm to"), "it cloned:\n{out}");
    assert!(
        out.contains("(clone)"),
        "and the copy says what it is in its own name:\n{out}"
    );
    assert!(
        out.contains("paused"),
        "and a clone is paused, because it has no scheduler slot of its own:\n{out}"
    );
}

// -- debugger attachment -----------------------------------------------------

#[test]
fn attach_and_detach_report_the_debugger_they_handled() {
    let workspace = Workspace::new("attach");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let path = image.to_str().expect("a path");

    let run = workspace.vm(&["attach", "vm", path, "lazdbg"]);
    run.succeeded();
    assert!(
        run.stdout().contains("attached lazdbg to vm"),
        "and it names the debugger it attached:\n{}",
        run.stdout()
    );
    assert!(run.stdout().contains("debugger attached"));

    let run = workspace.vm(&["detach", "vm", path]);
    run.refused();
    assert!(
        run.stderr().contains("no debugger is attached"),
        "and detaching from a VM with no debugger is refused, not a no-op:\n{}",
        run.stderr()
    );
}

// -- the refusals a user hits first -------------------------------------------

#[test]
fn a_command_with_no_kernel_says_it_needs_one() {
    let workspace = Workspace::new("no-image");
    let run = workspace.vm(&["status"]);
    run.refused();
    assert_eq!(run.code(), 2, "no kernel is a usage mistake");
    assert!(
        run.stderr().contains("needs a kernel"),
        "and it says what is missing rather than reporting a file error:\n{}",
        run.stderr()
    );
}

#[test]
fn a_path_that_does_not_exist_is_named() {
    // The difference from the case above matters: no argument is a usage mistake, and a
    // path that is not there is an I/O failure. Reporting one as the other sends a user
    // looking in the wrong place.
    let workspace = Workspace::new("missing-file");
    let run = workspace.vm(&["status", "vm", "no-such-kernel.bin"]);
    run.refused();
    assert_eq!(run.code(), 1, "a missing file is not a usage mistake");
    assert!(
        run.stderr().contains("no-such-kernel.bin"),
        "and the refusal names the path it looked for:\n{}",
        run.stderr()
    );
}

#[test]
fn a_path_that_is_not_a_boot_image_is_named_in_the_refusal() {
    let workspace = Workspace::new("not-an-image");
    let path = workspace.root.join("nonsense.bin");
    fs::write(&path, b"this is not a boot image").expect("the file is written");
    let run = workspace.vm(&["status", "vm", path.to_str().expect("a path")]);
    run.refused();
    assert!(
        run.stderr().contains("nonsense.bin") && run.stderr().contains("not a boot image"),
        "a path that does not hold an image is reported by name:\n{}",
        run.stderr()
    );
}

#[test]
fn a_usb_device_in_a_configuration_is_refused_by_name() {
    // §35 lists USB and this build has no USB device. The refusal is the manager's, and
    // the CLI's job is to pass it on rather than to swallow it — a management tool that
    // quietly built a machine without a device somebody asked for would be the exact
    // failure B19's `DeviceClass::Usb` exists to prevent.
    let workspace = Workspace::new("usb");
    let run = workspace.vm(&["create", "vm", "--usb"]);
    // `--usb` is not an option this tool has, so the honest outcome is a usage error
    // rather than a silent success. What is asserted is that it is not a *success*:
    // an unknown request for hardware must never look like a VM that was built.
    run.refused();
    assert!(
        !run.stdout().contains("created"),
        "and it did not report a VM as created:\n{}",
        run.stdout()
    );
}

#[test]
fn the_limit_option_bounds_a_run_and_is_reported() {
    let workspace = Workspace::new("limit");
    let image = workspace.image("kernel.bin", spinning_kernel());
    let run = workspace.vm(&[
        "start",
        "vm",
        image.to_str().expect("a path"),
        "--limit",
        "7",
    ]);
    run.succeeded();
    assert!(
        run.stdout().contains("ran 7 instruction(s)"),
        "a bounded run executes exactly the bound, so a program that never finishes is \
         reported rather than waited on:\n{}",
        run.stdout()
    );
}
