//! B20: the GUI's management view, and the manager it is built from.
//!
//! # Why this is a separate suite from `panels.rs`
//!
//! [`panels`] checks the *debugger's* view — ten panels of registers, memory and
//! disassembly, built from the debug API against a real machine. This checks the
//! *management* view: what §35 says a management client can see about a VM, built from
//! `lazalith_manager::VmStatus` and nothing else.
//!
//! The two must be kept apart because they answer different questions and come from
//! different layers. A frontend that could only reach a machine through the debugger
//! would be a debugger wearing a GUI; one that reaches a VM through the debugger for
//! the *management* panel has made the same mistake in a subtler place, and
//! `both_frontends_depend_on_the_management_api` in `crates/lazalith-cli/tests/`
//! is what catches it.
//!
//! # Every test builds a real `Manager`
//!
//! There is no `VmStatus` fixture constructed by hand anywhere in this file, and that is
//! deliberate. A management view tested against a hand-written status proves it formats
//! the fields it was handed, which is the easy half; testing it against a VM that was
//! created, booted and run proves the fields mean what the manager says they mean — and
//! the clock test in particular only means anything if the clock moved because the guest
//! executed.

use lazalith_boot::BootImage;
use lazalith_gui::{Emphasis, VmView};
use lazalith_isa::{Condition, Instruction, Opcode, Operand, encode};
use lazalith_manager::{Manager, VmConfig};
use lazalith_types::ArchitectureConfig;

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

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

/// A kernel that halts.
fn halting_kernel() -> Vec<u8> {
    let halt = Instruction::new(CONFIG, Opcode::Halt, &[]).expect("a halt builds");
    let mut bytes = encode(CONFIG, &halt).expect("and encodes").to_vec();
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

fn image(bytes: Vec<u8>) -> BootImage {
    BootImage::new(CONFIG, bytes, 0).expect("a boot image builds")
}

/// A created VM that has not been started.
fn created() -> Manager {
    Manager::create(VmConfig::minimal(CONFIG).with_name("panel-vm"))
        .expect("a minimal configuration is a VM")
}

/// A started VM running a spinning kernel.
fn running() -> Manager {
    let mut manager = created();
    manager.start(&image(spinning_kernel())).expect("it boots");
    manager
}

/// A started VM whose guest has halted.
fn halted() -> Manager {
    let mut manager = created();
    manager.start(&image(halting_kernel())).expect("it boots");
    manager.run(64).expect("it runs to the halt");
    manager
}

/// The text of the line with the given label, or a sentence saying it is absent.
fn line<'a>(view: &'a VmView, label: &str) -> &'a str {
    view.vm_section()
        .lines
        .iter()
        .find(|line| line.label == label)
        .map(|line| line.text.as_str())
        .unwrap_or("<no such line>")
}

// -- the panel exists and says the management API's fields -------------------

#[test]
fn a_created_vm_shows_its_own_state_and_has_run_no_cycles() {
    let manager = created();
    let view = VmView::of_manager(&manager).expect("a status is always available");
    assert_eq!(
        view.name, "panel-vm",
        "the name is the one the configuration gave"
    );
    assert_eq!(line(&view, "name"), "panel-vm");
    assert_eq!(line(&view, "state"), "created");
    assert_eq!(line(&view, "stage"), "cold");
    assert_eq!(line(&view, "time"), "0 cycle(s)");
    assert_eq!(line(&view, "halted"), "no");
    assert_eq!(line(&view, "debugger"), "none");
}

#[test]
fn a_started_vm_shows_that_firmware_ran() {
    let view = VmView::of_manager(&running()).expect("a status");
    assert_eq!(
        line(&view, "stage"),
        "booted",
        "booting is the management layer's own fact, and it is not the same fact as \
         the machine having executed anything"
    );
    assert_eq!(line(&view, "state"), "running");
}

#[test]
fn the_clock_line_moves_because_the_guest_ran() {
    // The one that would be vacuous with a hand-written `VmStatus`. The manager is
    // started and run, so the clock it reports moved because instructions retired, and
    // the panel is showing that rather than a number someone typed in.
    let mut manager = running();
    let before = manager.status().elapsed_cycles;
    let run = manager.run(32).expect("it runs");
    let view = VmView::of_manager(&manager).expect("a status");
    assert!(
        before > 0,
        "booting already costs cycles, so a started VM is not at zero"
    );
    assert_eq!(
        line(&view, "time"),
        format!("{} cycle(s)", before + run.cycles),
        "and the panel shows the clock after the run, not before it"
    );
}

#[test]
fn a_halted_guest_is_drawn_as_the_notable_thing_it_is() {
    let view = VmView::of_manager(&halted()).expect("a status");
    assert_eq!(line(&view, "halted"), "yes");
    let halted_line = view
        .vm_section()
        .lines
        .iter()
        .find(|line| line.label == "halted")
        .expect("the halted line is there");
    assert_eq!(
        halted_line.emphasis,
        Emphasis::Fault,
        "and it is the one line in this panel worth drawing attention to, because a \
         guest that stopped is what a management client most needs to notice"
    );
}

#[test]
fn an_attached_debugger_is_shown_and_its_absence_is_not_alarmed() {
    let mut manager = running();
    let before = VmView::of_manager(&manager).expect("a status");
    let none = before
        .vm_section()
        .lines
        .iter()
        .find(|line| line.label == "debugger")
        .expect("the debugger line is there");
    assert_eq!(none.emphasis, Emphasis::Plain, "no debugger is not a fault");

    manager
        .attach_debugger("lazdbg", None)
        .expect("a debugger attaches");
    let after = VmView::of_manager(&manager).expect("a status");
    assert_eq!(line(&after, "debugger"), "attached");
}

#[test]
fn a_paused_vm_says_paused_and_a_shut_down_one_says_shut_down() {
    let mut manager = running();
    manager.run(32).expect("it runs");
    manager.pause().expect("it pauses");
    assert_eq!(
        line(&VmView::of_manager(&manager).expect("a status"), "state"),
        "paused"
    );
    manager.shutdown().expect("it shuts down");
    assert_eq!(
        line(&VmView::of_manager(&manager).expect("a status"), "state"),
        "shut down"
    );
}

// -- what the panel is not ---------------------------------------------------

#[test]
fn the_panel_shows_no_registers_and_no_disassembly() {
    // A management view that printed a register or a disassembly would be a debugger.
    // §35's list has no registers in it, and the reason is that a management client
    // usually cannot see them: the manager does not hand them out, which is the whole
    // point of the rule B19 enforced and this test keeps.
    let view = VmView::of_manager(&halted()).expect("a status");
    let labels: Vec<&str> = view
        .vm_section()
        .lines
        .iter()
        .map(|line| line.label.as_str())
        .collect();
    assert_eq!(
        labels,
        vec!["name", "state", "stage", "time", "halted", "debugger"],
        "and the panel is exactly §35's facts, in that order"
    );
    for forbidden in ["r0", "r1", "pc", "sp", "flags"] {
        assert!(
            !labels.contains(&forbidden),
            "a management panel has no {forbidden} in it"
        );
    }
}

#[test]
fn a_view_outlives_the_manager_it_was_built_from() {
    // The same property the debug view gets from an owned snapshot, and the reason a
    // status bar can hold one: the panel is a value, not a borrow.
    let view = {
        let manager = running();
        VmView::of_manager(&manager).expect("a status")
    };
    assert_eq!(line(&view, "stage"), "booted");
}

#[test]
fn the_view_is_rebuildable_from_a_status_alone() {
    // A GUI that can only build its panel from a live `Manager` would have to keep one
    // borrowed for as long as it drew, which is a borrow a windowing loop cannot hold.
    // `VmView::of` takes the owned status, and the two paths agree.
    let manager = running();
    let from_manager = VmView::of_manager(&manager).expect("a status");
    let from_status = VmView::of(manager.name(), manager.status());
    assert_eq!(from_manager, from_status);
}
