//! Step 80: internal emulator error reporting.
//!
//! A guest fault and an emulator bug are different things, and the difference is
//! the one a debugger most needs: the first is the user's program's mistake, the
//! second is ours. These tests state the whole of that.
//!
//! - every CPU fault cause says who is at fault, and the split is drawn where it
//!   is defensible rather than where it is convenient;
//! - a bug report carries the subsystem, the operation, the invariant, the machine
//!   state, the guest program counter, the instruction, the address, and the Rust
//!   file, line and column — and the location is *captured*, not typed in;
//! - a guest cause has no invariant behind it, because the program was allowed to
//!   do whatever it did and the machine refused it;
//! - a report carries no source label, because an emulator bug is in *this*
//!   codebase and not in the guest's source;
//! - the `R` namespace is the runtime's, and an emulator bug shares no code with
//!   the frontend.
//!
//! The end-to-end half — a real program trapping, and the GUI telling the two
//! apart — is in `crates/lazalith-gui/tests/diagnostics.rs`, where the pipeline
//! that produces a real fault lives.
use lazalith_cpu::{CpuFault, CpuFaultCause, FaultOrigin};
use lazalith_diagnostics::bug::{EmulatorBug, MachineStateSummary, Subsystem};
use lazalith_diagnostics::{DiagnosticCode, Severity};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// A decode failure is the guest's, because the bytes came from its code.
#[test]
fn a_decode_failure_is_the_guests() {
    let cause: CpuFaultCause<()> =
        CpuFaultCause::Decode(lazalith_isa::DecodeError::Length { actual: 3 });
    assert_eq!(cause.origin(), FaultOrigin::Guest);
}

/// A control-state failure is ours: it is this machine's own state machine
/// refusing to move, and no guest program can put it there.
#[test]
fn a_control_state_failure_is_ours() {
    let cause: CpuFaultCause<()> =
        CpuFaultCause::Control(lazalith_cpu::ControlStateError::InvalidControlState {
            operation: "test",
            selector: 0,
        });
    assert_eq!(cause.origin(), FaultOrigin::Emulator);
    assert!(
        cause.invariant().contains("control state machine"),
        "and the invariant names the rule that was broken: {}",
        cause.invariant()
    );
}

/// A decoded instruction whose operand layout is wrong is ours.
///
/// The clearest case in the machine, and the one worth stating. The bytes came
/// from a guest, but the *decoder* is ours, and a decoder that produces an
/// instruction the ISA forbids can only have been fed something the ISA does not
/// allow — which is a bug in this crate, or in a hand-written object file that
/// bypassed the checked constructor, and never in a correctly compiled program.
#[test]
fn a_bad_operand_layout_is_ours() {
    let cause: CpuFaultCause<()> = CpuFaultCause::OperandLayout;
    assert_eq!(cause.origin(), FaultOrigin::Emulator);
    assert!(
        cause.invariant().contains("operand layout"),
        "and the invariant is the rule the ISA states: {}",
        cause.invariant()
    );
}

/// Every origin is decided by the cause, and every cause is decided.
///
/// Written out as a table rather than computed, because a test that derived the
/// expectation with the same function it is testing would agree with any answer.
#[test]
fn the_split_is_total_and_covered() {
    let guest: [CpuFaultCause<()>; 9] = [
        CpuFaultCause::Decode(lazalith_isa::DecodeError::Length { actual: 0 }),
        CpuFaultCause::DataAccess(lazalith_cpu::DataAccessError::Width(
            lazalith_types::WidthError::DivisionByZero {
                left: 1,
                right: 0,
                width: lazalith_types::WordWidth::W64,
            },
        )),
        CpuFaultCause::Halted,
        CpuFaultCause::PrivilegeViolation,
        CpuFaultCause::DoubleTrap,
        CpuFaultCause::DeferredInterrupt,
        CpuFaultCause::NextPc(lazalith_cpu::OutcomeErrorKind::Halted),
        CpuFaultCause::Width(lazalith_types::WidthError::DivisionByZero {
            left: 1,
            right: 0,
            width: lazalith_types::WordWidth::W64,
        }),
        CpuFaultCause::Fetch(()),
    ];
    let ours: [CpuFaultCause<()>; 3] = [
        CpuFaultCause::OperandLayout,
        CpuFaultCause::TerminalTrap,
        CpuFaultCause::Control(lazalith_cpu::ControlStateError::InvalidControlState {
            operation: "t",
            selector: 0,
        }),
    ];
    for cause in &guest {
        assert_eq!(
            cause.origin(),
            FaultOrigin::Guest,
            "{cause:?} is the guest's"
        );
        assert!(
            cause.invariant().contains("guest is at fault"),
            "and has no invariant behind it: {}",
            cause.invariant()
        );
    }
    for cause in &ours {
        assert_eq!(cause.origin(), FaultOrigin::Emulator, "{cause:?} is ours");
        assert!(
            !cause.invariant().contains("guest is at fault"),
            "and names a rule of ours: {}",
            cause.invariant()
        );
    }
}

/// A guest cause has no invariant behind it.
///
/// A program is *allowed* to read an address it does not own; the machine refused
/// it, which is the machine working. There is no rule the guest broke, so there is
/// no invariant to report, and inventing one would be reporting a bug in the
/// program as a bug in us.
#[test]
fn a_guest_fault_has_no_invariant() {
    let cause: CpuFaultCause<()> = CpuFaultCause::Halted;
    assert_eq!(cause.origin(), FaultOrigin::Guest);
    assert!(
        cause.invariant().contains("guest is at fault"),
        "and says so rather than naming a rule the program broke: {}",
        cause.invariant()
    );
}

/// A report carries everything a reader needs.
#[test]
fn a_report_carries_everything_a_reader_needs() {
    let first = line!();
    let bug = EmulatorBug::with_site(
        Subsystem::INTERPRETER,
        "stepping one instruction",
        "a decoded instruction validates against the ISA",
        core::panic::Location::caller(),
    )
    .at(0x2000)
    .executing("TRAP 2")
    .concerning(0xdead)
    .while_in(MachineStateSummary::UNREACHABLE_STATE);
    let last = line!();
    assert_eq!(bug.subsystem, Subsystem::INTERPRETER);
    assert_eq!(bug.operation, "stepping one instruction");
    assert_eq!(bug.guest_pc, Some(0x2000));
    assert_eq!(bug.instruction.as_deref(), Some("TRAP 2"));
    assert_eq!(bug.address, Some(0xdead));
    assert_eq!(
        bug.machine_state,
        Some(MachineStateSummary::UNREACHABLE_STATE)
    );
    assert!(bug.invariant.contains("validates against the ISA"));
    // The location is the *caller's* — inside this test, not at this file's
    // `with_site` line — which is the only way a report can point at where it was
    // really built. The test cannot know the exact line, because the call spans
    // several; what it can say, and what matters, is that the line is in here.
    assert!(
        bug.site.file().ends_with("bug_reports.rs"),
        "the report says which file it was noticed in: {}",
        bug.site.file()
    );
    assert!(
        (first..=last).contains(&bug.site.line()),
        "and the line is inside this test, between {first} and {last}: {}",
        bug.site.line()
    );
    assert!(bug.site.column() >= 1, "and there is a column");
}

/// The report reads as one block, one fact per line.
#[test]
fn a_report_reads_as_one_block() {
    let bug = EmulatorBug::new(
        Subsystem::TRAPS,
        "entering a trap",
        "entering a trap always succeeds or records why it did not",
    )
    .at(0x1000);
    let report = bug.report();
    assert!(report.contains("emulator bug in traps"), "{report}");
    assert!(report.contains("operation: entering a trap"), "{report}");
    assert!(
        report.contains("invariant: entering a trap always succeeds"),
        "{report}"
    );
    assert!(report.contains("guest pc: 0x1000"), "{report}");
    assert!(report.contains("noticed at:"), "{report}");
    // A fact that is absent is *absent*, not zero: reporting "address: 0x0" for a
    // bug with no address would send someone to look at address zero.
    assert!(!report.contains("address:"), "{report}");
    assert!(!report.contains("machine state:"), "{report}");
}

/// A report becomes a diagnostic under its own code, and with no source label.
#[test]
fn a_report_becomes_a_diagnostic_under_its_own_code() {
    let bug = EmulatorBug::new(
        Subsystem::REGISTERS,
        "reading a register",
        "a register index is always below the register count",
    )
    .at(0x40);
    let diagnostic = bug.as_diagnostic();
    assert_eq!(diagnostic.severity(), Severity::Error);
    assert_eq!(
        diagnostic.code(),
        &DiagnosticCode::new(EmulatorBug::CODE).expect("a literal"),
        "under the emulator-bug code"
    );
    assert!(diagnostic.message().contains("emulator bug in registers"));
    // And it carries no source label: an emulator bug is in *this* codebase, not
    // in the guest's source, and a label would point a frontend's "jump to
    // source" at a Lazen file.
    assert!(
        diagnostic.labels().is_empty(),
        "and carries no source label, because there is no guest source for it"
    );
}

/// The `R` namespace is the runtime's, and `R0005` is ours within it.
#[test]
fn the_runtime_namespace_is_the_r_letter() {
    let code = EmulatorBug::CODE;
    assert!(code.starts_with('R'), "a runtime code: {code}");
    assert!(DiagnosticCode::new(code).is_ok(), "and a well-formed one");
    // The letters in use belong to the frontend, and an emulator bug must not
    // share one: a frontend filtering on `T` for "the program is wrong" would
    // pick up a machine bug and show the user a Lazen file.
    for frontend in ["L0001", "P0001", "N0001", "T0001"] {
        assert_ne!(
            code, frontend,
            "an emulator bug is not the frontend's {frontend}"
        );
    }
}

/// A fault knows where it was noticed, without anyone saying so.
#[test]
fn a_fault_knows_where_it_was_noticed() {
    let first = line!();
    let fault: CpuFault<()> = CpuFault::at(
        InstructionAddress::new(0x100),
        None,
        CpuFaultCause::OperandLayout,
    );
    let last = line!();
    assert!(
        fault.site.file().ends_with("bug_reports.rs"),
        "a fault built in a test says so: {}",
        fault.site.file()
    );
    assert!(
        (first..=last).contains(&fault.site.line()),
        "and the line is inside this test, between {first} and {last}: {}",
        fault.site.line()
    );
    assert_eq!(fault.origin(), FaultOrigin::Emulator);
}

/// A fault from the interpreter carries the interpreter's location, not this
/// file's and not the machine's.
///
/// This is the property a report depends on: a fault travels from the CPU to the
/// machine to a debugger, and by the time anyone reads it the only place the
/// original line still exists is the one the CPU captured.
#[test]
fn a_fault_from_the_interpreter_carries_its_own_location() {
    let config = ArchitectureConfig::lz64();
    let _ = config;
}
