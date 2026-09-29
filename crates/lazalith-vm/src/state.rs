//! Where a machine is in the `binstruction.md` §34 chain.
//!
//! # Why this is not `MachineState`
//!
//! [`lazalith_machine::MachineState`] already exists and is the authority on whether
//! a machine will execute: `Created`, `Reset`, `Running`, `Paused`, `Halted`,
//! `Faulted`. Adding a second enum that also answers "where is this machine" is how a
//! VM ends up with two answers that disagree, and B6's whole job is that there is one
//! contract.
//!
//! So `BootStage` does not overlap it. The question `BootStage` answers is *whether
//! firmware has run*, and `MachineState` does not record that: a machine reset and
//! never booted, and a machine whose bootloader finished, are both
//! `MachineState::Reset`. They are not the same machine, and the difference is
//! observable — one has a kernel loaded and one has an empty ROM — so it has to be
//! somewhere.
//!
//! The two are orthogonal and both owned by [`crate::Vm`], and the combination is
//! well defined:
//!
//! | `BootStage` | `MachineState` | What is true |
//! | --- | --- | --- |
//! | `Cold` | `Reset` | the machine matches its profile; firmware has not run |
//! | `Booted` | `Reset`/`Running`/`Paused`/`Halted` | a bootloader has handed off to LazOS |
//!
//! `Faulted` is the one `MachineState` that `BootStage` says nothing useful about:
//! a faulted machine keeps whatever stage it had, and only a reset or a restore moves
//! it.

/// Whether firmware and bootloader have run on this machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootStage {
    /// Built from a profile, reset, and nothing has executed.
    ///
    /// The boot ROM window exists and is empty unless a boot image filled it, so
    /// executing from here lands on zeroes. That is a fact a caller can check rather
    /// than discover.
    Cold,
    /// A bootloader ran to its handoff point and execution is in LazOS.
    Booted,
}

impl BootStage {
    /// Whether firmware has run.
    pub const fn is_booted(self) -> bool {
        matches!(self, Self::Booted)
    }

    /// The other stage.
    ///
    /// Used by reset, which is the one transition that always returns to `Cold` and
    /// then follows the normal chain again — a reset machine has not booted, whatever
    /// it had done before.
    pub const fn cold(self) -> Self {
        Self::Cold
    }
}

impl core::fmt::Display for BootStage {
    /// The name a status line shows.
    ///
    /// **Added in B20, for a reason that is not cosmetic.** A management client has to
    /// print this — `lazctl status` has a line for it — and before this a client either
    /// wrote its own `match` over the enum, which is a second place for the two stages
    /// to be spelled, or printed `{:?}`, which is a debug rendering in a user-facing
    /// line. The management layer's own types already render (`ManagerState` does), and
    /// this is the stage doing the same.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Cold => "cold",
            Self::Booted => "booted",
        })
    }
}
