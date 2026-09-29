//! The Lazalith VM management layer.
//!
//! # What this is
//!
//! `binstruction.md` §35 asks for a dedicated VM management layer that creates,
//! configures, and runs VMs for a person, and that the GUI and the CLI both consume
//! **without either of them manipulating CPU internals**. This crate is that layer, and
//! it sits above [`lazalith_vm`] the way a hypervisor's management plane sits above its
//! virtual hardware.
//!
//! The three-layer split, and why it is three:
//!
//! ```text
//! CLI / GUI              ← a person, or a script
//!     ↓
//! lazalith-manager       ← this crate: what a VM *is* and what it is *doing*
//!     ↓
//! lazalith-vm            ← the lifecycle: stage, boot, reset, snapshot
//!     ↓
//! lazalith-machine       ← the machine: CPU, memory, devices, clock
//! ```
//!
//! # The rule this crate exists to hold
//!
//! §35's last two lines are the constraint everything here is shaped by: the GUI and
//! the CLI consume this API, and **neither directly manipulates CPU internals**.
//!
//! That is checked as a fact about the source, not by reviewing diffs. The test
//! `no_management_crate_touches_cpu_internals` in `crates/lazalith-cli/tests/`
//! fails if this crate names `machine_mut`, `processor_mut`, `devices_mut`,
//! `architectural_mut` or `traps_mut` anywhere. The reason it has to be a test is that
//! the failure is otherwise invisible: a manager that reached through
//! `machine_mut().processor_mut().architectural_mut()` to "just poke a register for
//! the UI" would compile, would pass every test that only starts and stops a VM, and
//! would be the first place guest state could change without the lifecycle knowing —
//! which is the one property [`lazalith_vm::Vm`] exists to guarantee.
//!
//! So `Manager` keeps its `Vm` private, with no accessor that hands it out. Everything a
//! client can do is a method here, and each one is expressed in terms the lifecycle
//! already understands: a stage, a run limit, a profile, a boot image.
//!
//! # Two vocabularies, kept apart on purpose
//!
//! §35 says to manage "machine profile, CPU configuration, RAM configuration". This
//! crate has [`VmConfig`] for the first and third, and **deliberately has no CPU
//! configuration at all** — no core count, no feature flags, no clock ratio. That is
//! not an omission to be filled in later; the shipped machine profile is single-core
//! with one fixed configuration, and a knob that adjusts nothing is worse than no knob,
//! because a GUI would show it and a user would believe it.
//!
//! The other separation is [`ManagerState`] versus the two lifecycles below it.
//! `lazalith_machine::MachineState` answers "would this machine execute" and
//! `lazalith_vm::BootStage` answers "has firmware run"; `ManagerState` answers "is a
//! person looking at a running, paused or shut-off VM", which is what a title bar
//! shows. `ShutDown` in particular is a manager state with no counterpart below, and
//! it is the reason the enum exists.

#![deny(missing_docs)]

extern crate alloc;

mod config;
mod manager;

pub use config::{ConfigError, DeviceClass, DeviceSpec, MemorySpec, VmConfig};
pub use manager::{DebugAttachment, Manager, ManagerError, ManagerState, VmStatus};
