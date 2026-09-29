//! What a refusal from the VM lifecycle is.
//!
//! Every variant here is a **decision the lifecycle made**, not a fault the guest
//! caused. That is the distinction the whole error type turns on, and it is why
//! `VmError` does not flatten its sources: a caller that has to decide between "this
//! machine cannot do that" and "this guest did something wrong" has to be able to,
//! and a single `anyhow`-shaped error would take that away.

use core::fmt;

use lazalith_boot::BootError;
use lazalith_devices::DeviceError;
use lazalith_machine::{MachineError, ProfileError};

use crate::state::BootStage;
use lazalith_types::WordWidth;

/// Why the VM lifecycle refused.
#[derive(Debug)]
pub enum VmError {
    /// The profile is not a machine this build can produce.
    Profile(ProfileError),
    /// A machine operation was refused, and the machine said why.
    Machine(MachineError),
    /// A device refused, and the device said why.
    Device(DeviceError),
    /// The boot image refused.
    Boot(BootError),
    /// An allocation failed while building the VM.
    Allocation,

    /// A profile-level check was asked of a machine that has no profile.
    ///
    /// A `Vm` adopted by `from_machine` was never built from a profile, so there is
    /// nothing to check it against. Reporting that as agreement 2014 the alternative 2014
    /// would tell a caller its Phase-I machine is verified when no check ran.
    NoProfile,

    // -- lifecycle ---------------------------------------------------------
    /// An operation that is only legal in one stage was attempted in another.
    ///
    /// A refusal rather than a no-op because a silently ignored lifecycle request is
    /// a request the caller believes happened. Booting a machine that is already
    /// booted, or halting one that is not running, is a bug in the *manager* — and
    /// the manager is code, so it deserves an error.
    WrongStage {
        /// What was asked for.
        operation: &'static str,
        /// What the lifecycle requires.
        required: BootStage,
        /// Where the machine actually is.
        found: BootStage,
    },

    /// A mutation that would change a guest-visible machine while the guest is
    /// running.
    ///
    /// Mapping a device under a running program changes what addresses mean without
    /// the program having done anything. A manager that wanted that is a manager with
    /// a bug, and the alternative — a window appearing between two of the guest's own
    /// instructions — is a machine that misbehaves for reasons no fault can explain.
    MutationWhileRunning {
        /// What was being changed.
        what: &'static str,
    },

    // -- boot contract -----------------------------------------------------
    /// A boot image and a profile disagree about the machine they are for.
    ///
    /// **The check B4 could not do.** A profile and a boot image each describe a
    /// machine, and nothing compared them: a profile that said the kernel loads at
    /// `0x0010_0000` and an image built for a layout where it loads elsewhere would
    /// be accepted, and the failure would appear as a fault at the first instruction
    /// of a kernel that was loaded somewhere else. Two authorities, one machine, and a
    /// refusal when they disagree — the architecture names the field it is about so a
    /// caller can say which half to fix.
    LayoutDisagreement {
        /// The geometry field, named as it is called in [`MachineProfile`].
        field: &'static str,
        /// What the profile says.
        profile: u64,
        /// What the boot image was built for.
        image: u64,
    },

    /// A boot image's ROM does not fit the machine it is being booted on.
    RomTooLarge {
        /// How many bytes the image's ROM holds.
        image: u64,
        /// How many bytes the machine's boot ROM window holds.
        window: u64,
    },

    /// A boot image and a profile are for different machine architectures.
    ///
    /// Checked before the geometry, because two machines of different widths have three
    /// fields of meaningless numbers between them and reporting the third is noise.
    ArchitectureDisagreement {
        /// What the profile describes.
        profile: WordWidth,
        /// What the image was built for.
        image: WordWidth,
    },

    // -- snapshot ----------------------------------------------------------
    /// A snapshot taken in a different lifecycle stage.
    ///
    /// A machine cannot become a booted machine by being restored, because booting
    /// ran firmware and that cannot be un-run. The stage is part of the snapshot for
    /// the same reason the storage's identity is part of a block device's: restoring
    /// into a machine that was never booted is a machine whose bootloader has not run.
    SnapshotStage {
        /// The stage the snapshot was taken in.
        snapshot: BootStage,
        /// The stage this machine is in.
        machine: BootStage,
    },

    /// A snapshot for a machine with a different number of devices.
    DeviceCount {
        /// How many devices the snapshot has.
        snapshot: usize,
        /// How many the machine has.
        machine: usize,
    },
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(source) => write!(f, "the profile is not a machine: {source}"),
            Self::Machine(source) => write!(f, "the machine refused: {source}"),
            Self::Device(source) => write!(f, "a device refused: {source}"),
            Self::Boot(source) => write!(f, "the boot image refused: {source}"),
            Self::Allocation => f.write_str("an allocation failed while building the VM"),
            Self::NoProfile => f.write_str(
                "this VM was adopted rather than built from a profile, so it has nothing to \
                 be checked against",
            ),
            Self::WrongStage {
                operation,
                required,
                found,
            } => write!(
                f,
                "{operation} needs a {required:?} machine, and this one is {found:?}"
            ),
            Self::MutationWhileRunning { what } => {
                write!(
                    f,
                    "{what} while the guest is running would change what addresses mean"
                )
            }
            Self::LayoutDisagreement {
                field,
                profile,
                image,
            } => write!(
                f,
                "the profile puts {field} at {profile:#x} and the boot image was built for {image:#x}"
            ),
            Self::RomTooLarge { image, window } => write!(
                f,
                "the boot image's ROM is {image} bytes and the machine's window is {window}"
            ),
            Self::ArchitectureDisagreement { profile, image } => write!(
                f,
                "the profile describes a {profile:?} machine and the image was built for {image:?}"
            ),

            Self::SnapshotStage { snapshot, machine } => write!(
                f,
                "the snapshot is of a {snapshot:?} machine and this one is {machine:?}"
            ),
            Self::DeviceCount { snapshot, machine } => write!(
                f,
                "the snapshot has {snapshot} devices and the machine has {machine}"
            ),
        }
    }
}

impl core::error::Error for VmError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Profile(source) => Some(source),
            Self::Machine(source) => Some(source),
            Self::Device(source) => Some(source),
            Self::Boot(source) => Some(source),
            _ => None,
        }
    }
}

impl From<ProfileError> for VmError {
    fn from(source: ProfileError) -> Self {
        Self::Profile(source)
    }
}

impl From<MachineError> for VmError {
    fn from(source: MachineError) -> Self {
        Self::Machine(source)
    }
}

impl From<DeviceError> for VmError {
    fn from(source: DeviceError) -> Self {
        Self::Device(source)
    }
}

impl From<BootError> for VmError {
    fn from(source: BootError) -> Self {
        Self::Boot(source)
    }
}
