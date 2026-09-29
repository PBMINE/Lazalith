//! The management panel: a VM's own state, as §35 says a management client sees it.
//!
//! # Why a second view exists
//!
//! [`crate::view`] is a *debugger's* view: ten panels of registers, disassembly, memory
//! and the stack, built from the debug API. It answers "what is the processor doing".
//!
//! This answers a different question — "what is this VM doing" — from the management
//! API instead. That is §35's list rather than §17's, and it is the view a person
//! actually wants when they are managing a machine rather than debugging a program: is
//! it running, has firmware run, how much virtual time has passed, has the guest
//! stopped, is a debugger attached.
//!
//! # What it proves, and what it does not
//!
//! **It proves the GUI has a path to the management API that does not go through the
//! debugger.** `lazalith-gui` depends on `lazalith-debug` for the debug view, and a
//! frontend that could *only* reach a machine through the debugger would be a debugger
//! wearing a GUI. This module is built from `lazalith_manager::VmStatus` — an owned
//! value with no machine behind it — and
//! `no_frontend_reaches_cpu_internals` in `crates/lazalith-cli/tests/architecture.rs`
//! fails if this crate names a machine mutator.
//!
//! **It is not a control surface.** Nothing here starts, pauses or resets anything;
//! [`crate::control`] is the GUI's control surface and it drives the debug API. A panel
//! that both displayed and changed a VM would be a second place where a VM's state
//! changes, and the reason `lazalith-manager` exists is that there is one.
//!
//! # Why a VM's name is passed separately from its status
//!
//! Because the manager's `status()` does not include it. That is deliberate: a status
//! is the machine's state, and a name is a label somebody attached to it. Taking the
//! name as a parameter keeps this module honest about the difference instead of
//! inventing a `VmStatus::name` that the manager would have to keep in step.

use lazalith_manager::{Manager, ManagerError, VmStatus};

use crate::view::{Emphasis, Line, Panel, Section};

/// The panel a management view draws into.
///
/// **A panel of its own rather than a fold-into-`Registers`.** The ten debug panels
/// describe a processor; this describes a VM, and putting a VM's state in a panel
/// titled "registers" would claim a relationship that does not exist — a VM can be
/// running with no registers visible at all, which is what a management client
/// normally has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmPanel;

impl VmPanel {
    /// The title the panel is drawn under.
    pub const TITLE: &'static str = "vm";
}

/// A management view: the panels describing a VM.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmView {
    /// The VM's name, as a person labelled it.
    pub name: String,
    /// The sections to draw, in display order.
    pub sections: Vec<Section>,
}

impl VmView {
    /// Builds the view from a VM's status.
    ///
    /// **Every line is a field of [`VmStatus`] or the `Display` of one of its types.**
    /// Nothing here is computed, derived or formatted from anything else, so a panel
    /// cannot show a state from one moment and a clock from another — the same property
    /// the debug view gets from taking an owned snapshot.
    pub fn of(name: impl Into<String>, status: VmStatus) -> Self {
        let name = name.into();
        let mut sections = vec![Section {
            panel: Panel::Screen,
            title: VmPanel::TITLE.to_string(),
            lines: vec![
                line("name", &name, Emphasis::Plain),
                line("state", &status.state.to_string(), Emphasis::Plain),
                line("stage", &status.stage.to_string(), Emphasis::Plain),
                line(
                    "time",
                    &format!("{} cycle(s)", status.elapsed_cycles),
                    Emphasis::Plain,
                ),
                line(
                    "halted",
                    if status.halted { "yes" } else { "no" },
                    // A halted guest is the one line here worth drawing attention to:
                    // everything else in this panel is what a person configured or the
                    // lifecycle is doing, and a guest that stopped is the thing a
                    // management client most often needs to notice.
                    if status.halted {
                        Emphasis::Fault
                    } else {
                        Emphasis::Plain
                    },
                ),
                line(
                    "debugger",
                    if status.debugger_attached {
                        "attached"
                    } else {
                        "none"
                    },
                    Emphasis::Plain,
                ),
            ],
        }];
        sections.shrink_to_fit();
        Self { name, sections }
    }

    /// Builds the view from a [`Manager`], by asking it for its status.
    ///
    /// The manager is taken by reference and only `status()` is called, so this is the
    /// whole of what the GUI's management view needs from a VM: one owned value.
    pub fn of_manager(manager: &Manager) -> Result<Self, ManagerError> {
        Ok(Self::of(manager.name().to_string(), manager.status()))
    }

    /// The section for the VM panel.
    pub fn vm_section(&self) -> &Section {
        &self.sections[0]
    }
}

/// One labelled line.
fn line(label: &str, text: &str, emphasis: Emphasis) -> Line {
    Line::new(label, text, emphasis)
}
