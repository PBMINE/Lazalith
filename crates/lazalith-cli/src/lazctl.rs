//! `lazctl` — the VM management commands, and the first real consumer of `lazalith-manager`.
//!
//! # Why this file exists
//!
//! B19 built `lazalith-manager` and then said, as a limitation, that "no CLI and no GUI
//! consume the manager yet". An API nothing calls is not an abstraction; it is a
//! specification. §35 requires the CLI to consume the management API, and this is that.
//!
//! # What it proves
//!
//! **The management API is sufficient to run a VM from a command line.** Every command
//! here goes through [`Manager`] and nothing else. It does not build a machine, hold a
//! processor, or read a register — and `no_frontend_reaches_cpu_internals` in
//! `crates/lazalith-cli/tests/architecture.rs` fails if it so much as names one.
//!
//! That test is the point. A CLI that could do its job by reaching into the machine
//! would be *more* code, not less, and the reason the manager exists is that there is
//! one place where a VM's state changes. `lazctl status` printing a clock that moves is
//! the same fact as "the CLI cannot write a register", and only one of them is checked
//! by a test.
//!
//! # The shape of the command set
//!
//! §35's list, in its order, and the names are the list's words:
//!
//! ```text
//! create  start  pause  resume  reset  shutdown
//! status  snapshot  restore  clone  attach  detach
//! ```
//!
//! **Every command is a lifecycle operation, and none of them is a debugger.** A
//! `lazctl` that could step an instruction would be a debugger with a worse interface;
//! §18 names `lazdbg` for that, and the manager's debugger support is attachment, not
//! stepping (B19's limitation, and the reason it is one).
//!
//! # Two things this tool deliberately does not do
//!
//! **It keeps no VM between invocations.** A management layer that persisted VMs would
//! need somewhere to keep them, and this command line deliberately has no daemon.
//! `create`, `start` and `status` build and boot a VM inside one process; the management
//! layer is exercised, not stored. B19 lists "no configuration persistence" as its
//! first limitation and this does not fake it with a state file that only one command
//! understands.
//!
//! **It does not write a snapshot file.** `VmSnapshot` holds a `Processor` and every
//! device's own encoding, and neither has a serialized form. A `lazctl snapshot` that
//! wrote *something* to a file called a snapshot would produce an artifact that looked
//! restorable and was not, and refusing to do that is better than the file. So the
//! command reports what a snapshot holds — which is real — and says plainly that
//! persisting one is not implemented. The capability needs a real serialization format.

use std::ffi::OsString;
use std::path::PathBuf;

use lazalith_boot::BootImage;
use lazalith_manager::{DeviceSpec, Manager, ManagerError, VmConfig};
use lazalith_types::ArchitectureConfig;

use crate::{CliError, Outcome};

/// The commands, in §35's order.
///
/// A table rather than a `match` in [`run`], so `--help` can list exactly what the
/// dispatcher accepts. A help text naming commands the dispatcher does not have is the
/// most common way a CLI lies to its user.
const COMMANDS: [(&str, &str); 12] = [
    (
        "create",
        "build a VM from a configuration and report what it is",
    ),
    ("start", "boot a VM with a boot image, then run it"),
    (
        "status",
        "report a VM's state, stage, virtual time and halt",
    ),
    ("pause", "pause a running VM"),
    ("resume", "resume a paused VM"),
    ("reset", "reset a live VM, keeping its configuration"),
    ("shutdown", "turn a VM off, keeping its configuration"),
    ("snapshot", "report what a VM's saved state holds"),
    ("restore", "put a saved state back"),
    ("clone", "make an independent copy of a VM"),
    ("attach", "attach a debugger by name"),
    ("detach", "detach the debugger"),
];

/// The usage text for `lazctl`.
pub(crate) fn usage_text() -> String {
    let mut text = String::from(
        "lazctl — Lazalith VM management\n\nUSAGE\n    lazctl <command> [name] <image> [options]\n\nCOMMANDS\n",
    );
    for (name, description) in COMMANDS {
        text.push_str(&format!("    {name:<9} {description}\n"));
    }
    text.push_str(
        "\n    help       print this message\n\n\
         OPTIONS\n\
         \x20   --name NAME   the VM's name, when it is not the first argument\n\
         \x20   --limit N     instructions to run, for `start` (default 1000000)\n\
         \x20   --display     give the VM a display device as well as a console\n\n\
         Every command goes through the VM Manager API. `lazctl` does not build a\n\
         machine, read a register or write one, and there is a test that says so.\n\
         A VM does not outlive the command, and a snapshot is not written to disk.\n",
    );
    text
}

/// `lazctl <command> [name] <image> [options]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let Some(command) = arguments.first() else {
        eprint!("{}", usage_text());
        return Ok(Outcome::Refused);
    };
    let command = command.to_string_lossy().into_owned();
    let rest = &arguments[1..];
    match command.as_str() {
        "create" => {
            check_options(rest)?;
            create(rest)
        }
        "help" | "--help" | "-h" => {
            print!("{}", usage_text());
            Ok(Outcome::Done)
        }
        // `start` is the one command that *is* the run, so it does not go through
        // `live()` — which runs the guest to put the machine into `Running`, because
        // that is what `pause` and `reset` need. Running it twice meant a `start` of a
        // halting program was refused with "Run is invalid while machine is Halted",
        // which is the manager being right about a second run and the command being
        // wrong about asking for one.
        "start" => {
            let mut manager = booted(rest)?;
            act("start", &mut manager, rest)
        }
        other => {
            if !COMMANDS.iter().any(|(name, _)| *name == other) {
                return Err(CliError::usage_with_help(format!(
                    "`{other}` is not a lazctl command."
                )));
            }
            let mut manager = live(rest)?;
            act(other, &mut manager, rest)
        }
    }
}

/// Performs the named operation on a booted VM.
///
/// One `match` over the command name rather than a function per command, because every
/// one of them is three lines — build a VM, do the thing, print the status — and eleven
/// near-identical functions would be eleven places to forget the printing.
fn act(command: &str, manager: &mut Manager, arguments: &[OsString]) -> Result<Outcome, CliError> {
    let name = manager.name().to_string();
    match command {
        "start" => {
            let run = manager.run(run_limit(arguments)).map_err(manager_error)?;
            println!(
                "  ran {} instruction(s) in {} cycle(s){}",
                run.executed,
                run.cycles,
                if run.halted_at.is_some() {
                    ", guest halted"
                } else {
                    ""
                }
            );
        }
        "status" => {}
        "pause" => manager.pause().map_err(manager_error)?,
        "resume" => manager.resume().map_err(manager_error)?,
        "reset" => manager.reset().map_err(manager_error)?,
        "shutdown" => manager.shutdown().map_err(manager_error)?,
        "snapshot" => {
            let saved = manager.snapshot().map_err(manager_error)?;
            println!(
                "  a snapshot of {name} was taken at stage {} with {} cycle(s) elapsed",
                saved.stage(),
                manager.status().elapsed_cycles
            );
            println!(
                "  not written to disk: a VM snapshot has no serialized form yet, and a \
                 file that could not be restored would be worse than none"
            );
        }
        "restore" => {
            let before = manager.snapshot().map_err(manager_error)?;
            manager.run(64).map_err(manager_error)?;
            manager.restore(&before).map_err(manager_error)?;
            println!("  restored {name} to the state before its run");
        }
        "clone" => {
            let image = read_image(&target(arguments).map(|t| t.image).unwrap_or_default())?;
            let copied = manager.clone_vm(&image).map_err(manager_error)?;
            println!("  cloned {name} to {:?}", copied.name());
            print_status(copied.name(), &copied);
            return Ok(Outcome::Done);
        }
        "attach" => {
            let debugger = target(arguments)
                .and_then(|t| t.debugger)
                .unwrap_or_else(|| String::from("lazdbg"));
            manager
                .attach_debugger(&debugger, None)
                .map_err(manager_error)?;
            println!("  attached {debugger} to {name}");
        }
        "detach" => {
            let attachment = manager.detach_debugger().map_err(manager_error)?;
            println!("  detached {} from {name}", attachment.name);
        }
        other => {
            return Err(CliError::usage_with_help(format!(
                "`{other}` is not a lazctl command."
            )));
        }
    }
    print_status(&name, manager);
    Ok(Outcome::Done)
}

/// Reads a boot image, reporting a path that is not an image by name.
fn read_image(path: &std::path::Path) -> Result<BootImage, CliError> {
    let bytes = std::fs::read(path).map_err(|source| CliError::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })?;
    BootImage::new(ArchitectureConfig::lz64(), bytes, 0)
        .map_err(|error| CliError::Refused(format!("{path:?} is not a boot image: {error}")))
}

/// The non-flag arguments, in order.
///
/// **A flag that takes a value consumes the next argument**, so `--name myvm` is one
/// option rather than a name called `--name` and a stray `myvm`. That is the whole
/// reason this walks with an index instead of filtering on the prefix: a filter would
/// see `myvm` as a positional and turn a VM's name into an image path.
fn positional(arguments: &[OsString]) -> Vec<String> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy().into_owned();
        match argument.strip_prefix("--") {
            Some("limit") | Some("name") | Some("out") => index += 1,
            Some(_) => {}
            None => out.push(argument),
        }
        index += 1;
    }
    out
}

/// The image and the name, from the positional arguments.
///
/// **The usage is `[name] <image> [debugger]`, and the count decides which is which.**
/// One positional is the image and the name defaults; two are the name then the image;
/// three add a debugger name, which only `attach` reads. Deciding by position *count*
/// rather than by taking the last element is what makes `attach vm kernel lazdbg` work
/// — taking the last gave "lazdbg" as the image and the tool then tried to read it as a
/// file.
struct Target {
    name: String,
    image: PathBuf,
    debugger: Option<String>,
}

/// Reads the target out of the positional arguments.
fn target(arguments: &[OsString]) -> Option<Target> {
    let positional = positional(arguments);
    match positional.len() {
        0 => None,
        1 => Some(Target {
            name: String::from("lazen"),
            image: PathBuf::from(&positional[0]),
            debugger: None,
        }),
        _ => Some(Target {
            name: positional[0].clone(),
            image: PathBuf::from(&positional[1]),
            debugger: positional.get(2).cloned(),
        }),
    }
}

/// The name, from `--name` or from the first positional argument.
fn named(arguments: &[OsString]) -> String {
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index].to_string_lossy() == "--name"
            && let Some(value) = arguments.get(index + 1)
        {
            return value.to_string_lossy().into_owned();
        }
        index += 1;
    }
    positional(arguments)
        .first()
        .cloned()
        .unwrap_or_else(|| String::from("lazen"))
}

/// A configuration, minimal unless the arguments ask for more.
///
/// **Minimal by default, because a management command that invented a machine profile
/// would be making a decision its caller did not ask it to.** One console and a default
/// amount of memory is the smallest thing that runs; a caller that wants a display asks
/// for one.
fn configuration(name: &str, arguments: &[OsString]) -> VmConfig {
    let mut config = VmConfig::minimal(ArchitectureConfig::lz64()).with_name(name);
    if arguments
        .iter()
        .any(|argument| argument.to_string_lossy() == "--display")
    {
        config = config.with_device(DeviceSpec::display(2, 0x4003_0000));
    }
    config
}

/// The `--limit` value, defaulting to a bounded run.
///
/// Bounded because `lazctl start` is a command that returns, and a command that waits
/// for a guest to exit forever is a command a user has to interrupt.
fn run_limit(arguments: &[OsString]) -> u64 {
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index].to_string_lossy() == "--limit"
            && let Some(value) = arguments.get(index + 1)
            && let Ok(limit) = value.to_string_lossy().parse::<u64>()
        {
            return limit;
        }
        index += 1;
    }
    1_000_000
}

/// `lazctl create [name]`
fn create(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let name = named(arguments);
    let manager = Manager::create(configuration(&name, arguments)).map_err(manager_error)?;
    print_status(&name, &manager);
    Ok(Outcome::Done)
}

/// The options `lazctl` accepts, and whether each takes a value.
///
/// **A closed list, and an unknown option is refused.** The alternative is what
/// `--usb` exposed: `lazen vm create vm --usb` printed a VM as created, having
/// silently dropped the request. A management tool that ignores an option is a tool
/// whose output a user has to double-check against what they typed, and for hardware
/// requests that is exactly the wrong failure — the whole reason B19 made
/// `DeviceClass::Usb` a refusal rather than something to drop is that silently building
/// a machine without a device somebody asked for is the failure to avoid.
const OPTIONS: [&str; 3] = ["--name", "--limit", "--display"];

/// Refuses an option `lazctl` does not have.
fn check_options(arguments: &[OsString]) -> Result<(), CliError> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy().into_owned();
        if let Some(name) = argument.strip_prefix("--")
            && !OPTIONS.contains(&argument.as_str())
        {
            return Err(CliError::Usage(format!(
                "`--{name}` is not a lazctl option.\n\n{}",
                usage_text()
            )));
        }
        index += 1;
    }
    Ok(())
}

/// A booted VM, not yet run.
///
/// **Separate from [`live`] because `start` is the command that *is* the run.** A
/// manager that had already run is a manager whose guest may have halted, and a
/// `start` that then ran again was refused with "Run is invalid while machine is
/// Halted" — the manager being right about a second run and the command being wrong
/// about asking for one.
///
/// **And [`live`] runs, because a booted-but-not-run machine is `Reset` and `pause`
/// requires `Running`.** A person typing `pause` means "this VM is running, stop it",
/// so the command puts it in that state first. That is a decision this tool makes about
/// how to read the command, and it is stated because it is a decision.
fn booted(arguments: &[OsString]) -> Result<Manager, CliError> {
    check_options(arguments)?;
    let Some(target) = target(arguments) else {
        return Err(CliError::usage_with_help(format!(
            "`lazctl` needs a kernel to run a VM from:\n\n{}",
            usage_text()
        )));
    };
    let image = read_image(&target.image)?;
    let name = target.name.clone();
    let mut manager = Manager::create(configuration(&name, arguments)).map_err(manager_error)?;
    manager.start(&image).map_err(manager_error)?;
    Ok(manager)
}

/// A booted VM that has run.
fn live(arguments: &[OsString]) -> Result<Manager, CliError> {
    let mut manager = booted(arguments)?;
    manager.run(run_limit(arguments)).map_err(manager_error)?;
    Ok(manager)
}

/// A `Manager` refusal, as a CLI error.
fn manager_error(error: ManagerError) -> CliError {
    CliError::Refused(error.to_string())
}

/// Prints what a management client can see, and nothing else.
///
/// Every line is a field of `VmStatus` or the `Display` of one of them. There is
/// deliberately no register dump, no memory and no disassembly: a status line that
/// printed those would be a debugger, and §35's management layer is not one.
///
/// The booleans are spelled `yes`/`no` rather than printed as Rust's `true`/`false`,
/// because this is the output a person reads and `true` is a Rust rendering leaking
/// into a user-facing line. The GUI's management panel spells them the same way, which
/// is the point of the two being the same fact shown twice.
fn print_status(name: &str, manager: &Manager) {
    let status = manager.status();
    println!("{name}:");
    println!("  state    {}", status.state);
    println!("  stage    {}", status.stage);
    println!("  time     {} cycle(s)", status.elapsed_cycles);
    println!("  halted   {}", yes_no(status.halted));
    println!(
        "  debugger {}",
        if status.debugger_attached {
            "attached"
        } else {
            "none"
        }
    );
}

/// `yes` or `no`.
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}
