//! # B6 — the common VM lifecycle, reset and boot contract
//!
//! `binstruction.md` §6, §9 and §34. This crate is the place where a machine's
//! *lifecycle* lives, as opposed to the machine itself: what `lazalith-machine` owns
//! when it owns a processor, a bus and a clock, and what this owns when it owns the
//! questions of whether firmware has run and what a reset undoes.
//!
//! ## The shape
//!
//! ```text
//! Vm<D>                          this crate: the lifecycle
//!  ├── LazalithMachine<D>        §9: the architectural machine
//!  ├── BootStage                 has firmware run?
//  ├── MachineProfile             what the machine is, to re-check against
//!  └── VmSnapshot                §40: the guest's state
//!
//! BootImage                      §34: the firmware, loaded on request
//!  ↓
//! Bootloader → LazOS
//! ```
//!
//! ## What B6 unified
//!
//! Before this crate there were two ways to make a machine and no way to check them
//! against each other. `BootImage::start` built *and* booted in one call, in the boot
//! crate, and refused a non-empty device manager because a boot image does not know
//! what devices a machine has. `ProfiledMachine::from_profile` built a machine with
//! devices from a profile and knew nothing about booting. A caller wanting a described
//! machine that boots had to pick one and lose the other's half.
//!
//! The contract now is: **each is the authority for what it knows, and the overlap is
//! checked.** See [`boot`] for the fields and [`boot::BootAgreement`] for the check.
//!
//! ## What it did not do
//!
//! - No firmware. §34: "Do not implement them during this architecture pass." The
//!   firmware is a ROM a caller loads; the *contract* for loading one is what this
//!   stage builds.
//! - No VM manager. That is B19. A manager creates, configures, persists and runs
//!   machines for a human, and none of that belongs to a machine.
//! - No rewindable clock and no replay. B18. [`Vm::restore`] is forward-only and says
//!   so rather than pretending.
//! - No firmware *architectures*. §34's minimal-Lazalith, BIOS-like and UEFI-like
//!   layers stay unimplemented, and `MachineLayout` has one ROM window rather than
//!   three nested firmware volumes.
//!
//! ## Preserved
//!
//! Everything. [`Vm`] is additive: `LazalithMachine` is unchanged in what it can do,
//! `BootImage::start` still builds and boots exactly as it did — now by calling the
//! same [`BootImage::boot_into`] this crate uses — and Phase-I's `NoDevice` machines
//! are adopted by [`Vm::from_machine`] rather than replaced.
//!
//! ## The naming
//!
//! §9 proposes **LVMI** (Lazalith Virtual Machine Interface) as a candidate for the
//! VM abstraction's name and says not to finalize it without research. B6 does not
//! claim it. This crate is `lazalith-vm` because that is what it is — the VM's
//! lifecycle — and the interface question is a separate, later decision that should
//! not be settled by a crate name chosen while writing code.

extern crate alloc;

mod boot;
mod boot_profile;
mod error;
mod snapshot;
mod state;
mod vm;

pub use boot::{BootAgreement, BootHandoff};
pub use boot_profile::{
    BootChain, BootProfile, BootProfileError, ChainStage, FirmwareProfile, MAX_FIRMWARE_BYTES,
};
pub use error::VmError;
pub use snapshot::VmSnapshot;
pub use state::BootStage;
pub use vm::Vm;

/// The lifecycle's own vocabulary, gathered so a caller imports one thing.
pub mod lifecycle {
    pub use crate::boot::{BootAgreement, BootHandoff};
    pub use crate::snapshot::VmSnapshot;
    pub use crate::state::BootStage;
    pub use crate::vm::Vm;
    pub use lazalith_machine::{MachineRun, MachineState};
}
