//! Reporting a genuine emulator bug.
//!
//! # The distinction this file is about
//!
//! When something goes wrong in this machine there are two completely different
//! questions, and answering them the same way is the worst thing a debugger can
//! do:
//!
//! - **A guest fault** is the *program's* mistake. It read an address it does not
//!   own, executed a byte that is not an instruction, trapped on its own check. The
//!   user is looking for a bug in their Lazen.
//! - **An emulator bug** is *ours*. A register file was indexed outside itself, a
//!   state machine was in a state its own code says is impossible, a decoded
//!   instruction had an operand layout the ISA forbids. There is no user program
//!   to blame, and the person who needs to look is whoever wrote the emulator.
//!
//! Every `EmulatorBug` carries both the machine's state and the place in *this*
//! source where the impossibility was noticed, because an emulator bug report
//! without a line number is a report someone has to reproduce before they can
//! act on it.
//!
//! # Source locations are captured, not typed in
//!
//! [`EmulatorBug::new`] takes a `core::panic::Location::caller()`, so the file,
//! line and column are whatever the compiler recorded at the call site. A hand-
//! written `"src/lib.rs:412"` goes stale the moment the file is edited, and a
//! stale line number in a bug report is worse than none: it sends someone to the
//! right file and the wrong line.
//!
//! # Nothing here panics
//!
//! Reporting a bug and crashing on the way out would destroy the machine state the
//! report is about. Every constructor here returns a value; none of them assert.

use alloc::string::String;
use core::fmt;
use core::panic::Location;

/// Which part of the machine noticed the impossibility.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Subsystem(&'static str);

impl Subsystem {
    /// The instruction decoder.
    pub const DECODER: Self = Self("decoder");
    /// The interpreter.
    pub const INTERPRETER: Self = Self("interpreter");
    /// The register file.
    pub const REGISTERS: Self = Self("registers");
    /// The trap controller.
    pub const TRAPS: Self = Self("traps");
    /// The machine's own bookkeeping.
    pub const MACHINE: Self = Self("machine");
    /// The memory subsystem.
    pub const MEMORY: Self = Self("memory");
    /// The operating system on top of the machine.
    pub const KERNEL: Self = Self("kernel");
    /// The linker or loader.
    pub const LOADER: Self = Self("loader");
    /// A host device.
    pub const DEVICE: Self = Self("device");
    /// The toolchain.
    pub const TOOLCHAIN: Self = Self("toolchain");

    /// The subsystem's name.
    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for Subsystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// A state the machine was in when a bug was noticed.
///
/// A description rather than a dump: what matters is *which* state, and a `Debug`
/// print of a whole machine is unreadable in a report. The caller writes one line
/// per fact, and a state is identified by the line that says so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineStateSummary(&'static str);

impl MachineStateSummary {
    /// Halted, and asked to do something.
    pub const HALTED: Self = Self("halted");
    /// Entering a trap with a frame already active.
    pub const DOUBLE_TRAP: Self = Self("a trap frame was already active");
    /// The trap controller had already failed to enter.
    pub const TERMINAL_TRAP: Self = Self("the trap controller is terminally failed");
    /// An interrupt arrived while a frame was active.
    pub const DEFERRED_INTERRUPT: Self = Self("an interrupt was deferred by a frame");
    /// An instruction that needs Supervisor ran in User.
    pub const PRIVILEGE: Self = Self("privilege");
    /// A state machine in a state its own code says is unreachable.
    pub const UNREACHABLE_STATE: Self = Self("an unreachable state");

    /// The summary.
    pub const fn summary(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for MachineStateSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// What the emulator found impossible.
///
/// Every field here is a fact about *this* machine, taken at the moment the
/// impossibility was noticed, so a report can be acted on without reproducing it.
#[derive(Clone, Debug)]
pub struct EmulatorBug {
    /// Which part of the machine noticed.
    pub subsystem: Subsystem,
    /// What it was doing.
    pub operation: &'static str,
    /// The guest's program counter, when one is meaningful.
    pub guest_pc: Option<u64>,
    /// The guest's instruction there, as text.
    pub instruction: Option<String>,
    /// The address involved, when the bug is about one.
    pub address: Option<u64>,
    /// The state the machine was in.
    pub machine_state: Option<MachineStateSummary>,
    /// The invariant that was violated, in the words of the code that holds it.
    pub invariant: String,
    /// Where in the emulator this was noticed.
    pub site: &'static Location<'static>,
}

impl EmulatorBug {
    /// A bug report with the machine's context filled in later.
    ///
    /// `operation` and `invariant` are the two halves of "what it was doing" and
    /// "what should have been true", and they are separate because they are
    /// written at different places: the operation belongs to the function that
    /// failed and the invariant to the rule it was checking.
    ///
    /// `#[track_caller]` is what makes the location the caller's: without it a
    /// report would point at this constructor, which is the one line in the whole
    /// project that every emulator bug shares.
    #[must_use]
    #[track_caller]
    pub fn new(
        subsystem: Subsystem,
        operation: &'static str,
        invariant: impl Into<String>,
    ) -> Self {
        Self {
            subsystem,
            operation,
            guest_pc: None,
            instruction: None,
            address: None,
            machine_state: None,
            invariant: invariant.into(),
            site: Location::caller(),
        }
    }

    /// Records the guest's program counter.
    #[must_use]
    pub fn at(mut self, pc: u64) -> Self {
        self.guest_pc = Some(pc);
        self
    }

    /// Records the guest's instruction there.
    #[must_use]
    pub fn executing(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Records the address the bug is about.
    #[must_use]
    pub fn concerning(mut self, address: u64) -> Self {
        self.address = Some(address);
        self
    }

    /// Records the state the machine was in.
    #[must_use]
    pub fn while_in(mut self, state: MachineStateSummary) -> Self {
        self.machine_state = Some(state);
        self
    }

    /// A bug report that keeps the site it was given.
    ///
    /// For a fault that was noticed in one crate and is reported by another: the
    /// machine records what the *CPU* found, and re-deriving the location here would
    /// point at the machine rather than at the interpreter that decided the state was
    /// impossible.
    #[must_use]
    pub fn with_site(
        subsystem: Subsystem,
        operation: &'static str,
        invariant: impl Into<String>,
        site: &'static Location<'static>,
    ) -> Self {
        Self {
            subsystem,
            operation,
            guest_pc: None,
            instruction: None,
            address: None,
            machine_state: None,
            invariant: invariant.into(),
            site,
        }
    }

    /// The report as a `Diagnostic`, for a consumer that shows diagnostics.
    ///
    /// The code is [`Self::CODE`] and the severity is an *error*: an emulator bug is
    /// never a note or a warning, whatever else is true about it. The message is the
    /// whole report, because a report that had to be reassembled from parts is a
    /// report someone will not read.
    #[must_use]
    pub fn as_diagnostic(&self) -> crate::Diagnostic {
        crate::Diagnostic::new(
            crate::Severity::Error,
            crate::DiagnosticCode::new(Self::CODE).expect("the code is a literal"),
            self.report(),
        )
    }

    /// The report as one block of text.
    ///
    /// For a log line or a panic message. The fields are `key: value` pairs, one
    /// per line, because a report that runs together is one nobody can grep.
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = format_into(&mut out, format_args!("emulator bug in {}", self.subsystem));
        let _ = format_into(&mut out, format_args!("  operation: {}", self.operation));
        let _ = format_into(&mut out, format_args!("  invariant: {}", self.invariant));
        if let Some(state) = self.machine_state {
            let _ = format_into(&mut out, format_args!("  machine state: {state}"));
        }
        if let Some(pc) = self.guest_pc {
            let _ = format_into(&mut out, format_args!("  guest pc: {pc:#x}"));
        }
        if let Some(instruction) = &self.instruction {
            let _ = format_into(&mut out, format_args!("  instruction: {instruction}"));
        }
        if let Some(address) = self.address {
            let _ = format_into(&mut out, format_args!("  address: {address:#x}"));
        }
        let _ = format_into(
            &mut out,
            format_args!(
                "  noticed at: {}:{}:{}",
                self.site.file(),
                self.site.line(),
                self.site.column()
            ),
        );
        out
    }
}

/// Writes `arguments` into `out`.
///
/// A `write!` that cannot fail, so the `Result` is dropped here rather than at
/// every call site. A `String` push does not fail, and pretending otherwise by
/// unwrapping at each site would be the panic this crate exists to avoid.
fn format_into(out: &mut String, arguments: fmt::Arguments<'_>) -> fmt::Result {
    use core::fmt::Write;
    out.write_fmt(arguments)
}

impl fmt::Display for EmulatorBug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report())
    }
}

impl core::error::Error for EmulatorBug {}

impl EmulatorBug {
    /// The stable code an emulator bug is reported under.
    ///
    /// `R0005` and not a `T`/`L`/`P`/`N` code, because a frontend that filtered
    /// on the letter would otherwise put "Lazalith is broken" in the same bucket
    /// as "this program is broken", and the whole point of the distinction is that
    /// they are not the same.
    pub const CODE: &'static str = "R0005";
}
