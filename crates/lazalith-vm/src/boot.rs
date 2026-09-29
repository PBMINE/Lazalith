//! The `binstruction.md` §34 boot contract, and the one place it is checked.
//!
//! §34 names a chain:
//!
//! ```text
//! VM Manager        ← B19, not this
//!  ↓
//! Machine Profile   ← what the machine is
//!  ↓
//! Firmware          ← a ROM, supplied as bytes
//!  ↓
//! Bootloader        ← the code in that ROM
//!  ↓
//! LazOS             ← where execution ends up
//! ```
//!
//! B6 builds everything from the profile down and deliberately does not implement
//! firmware: §34 says "Do not implement them during this architecture pass."
//!
//! # The problem this file exists to solve
//!
//! Before B6 there were two ways to make a machine, and they did not know about each
//! other. `BootImage::machine_setup` built one from a boot image and **refused a
//! non-empty device manager** — `BootError::UnexpectedDevices` — because a boot image
//! does not know what devices a machine has. `MachineProfile::machine_setup` built one
//! from a profile, with devices, and never touched a boot image. A caller wanting
//! both had to know which to ask, and nothing compared the two.
//!
//! So the rule is: **each is the authority for what it knows, and the overlap is
//! checked.** The boot image owns memory — a kernel image, a kernel stack and a user
//! region are the OS's business, and their permissions differ from a profile's flat
//! RAM. The profile owns devices, the timer, and the geometry. Where both describe
//! the same fact, they must agree.
//!
//! That check is the point. A profile saying the kernel loads at `0x0010_0000` and an
//! image built for a layout where it loads elsewhere was previously accepted, and
//! failed later as a fault at the first instruction of a kernel loaded somewhere else.
//! A refusal names the field, so a caller knows which half to fix.

use lazalith_boot::{
    BootImage, KERNEL_INITIAL_SP, KERNEL_LOAD_ADDRESS, PHYSICAL_RAM_START, RESET_VECTOR,
};
use lazalith_machine::MachineProfile;
use lazalith_types::{InstructionAddress, WordWidth};

/// The agreement between a profile and a boot image, or the field that broke it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootAgreement {
    /// The image may be booted on this machine.
    Agree,
    /// The two disagree about a field both of them describe.
    Disagree {
        /// The field, named as [`MachineProfile`] names it.
        field: &'static str,
        /// What the profile says.
        profile: u64,
        /// What the image was built for.
        image: u64,
    },
    /// The image's ROM is larger than the machine's ROM window.
    RomTooLarge {
        /// The image's ROM in bytes.
        image: u64,
        /// The machine's window in bytes.
        window: u64,
    },
    /// The two are for different architectures.
    Architecture {
        /// What the profile describes.
        profile: WordWidth,
        /// What the image was built for.
        image: WordWidth,
    },
}

impl BootAgreement {
    /// The first thing wrong, or [`BootAgreement::Agree`].
    ///
    /// Checked in a fixed order — architecture, then size, then geometry — so the
    /// refusal is the *most fundamental* disagreement rather than whichever one
    /// happened to be written first. Two machines that disagree about architecture
    /// have three fields of garbage between them, and reporting the third is noise.
    pub fn check(profile: &MachineProfile, image: &BootImage) -> Self {
        if profile.name().architecture() != image.config() {
            return Self::Architecture {
                profile: profile.name().architecture().word_width(),
                image: image.config().word_width(),
            };
        }
        let rom_bytes = image.rom().len() as u64;
        let window = profile.layout().boot_rom_length;
        if rom_bytes > window {
            return Self::RomTooLarge {
                image: rom_bytes,
                window,
            };
        }
        let layout = profile.layout();
        for (field, from_profile, from_image) in [
            (
                "physical_ram_start",
                layout.physical_ram_start,
                PHYSICAL_RAM_START,
            ),
            (
                "reset_vector",
                profile.reset_vector().as_u64(),
                RESET_VECTOR.as_u64(),
            ),
            (
                "kernel_load_address",
                layout.kernel_load_address,
                KERNEL_LOAD_ADDRESS,
            ),
            (
                "kernel_initial_sp",
                layout.kernel_initial_sp,
                KERNEL_INITIAL_SP,
            ),
        ] {
            if from_profile != from_image {
                return Self::Disagree {
                    field,
                    profile: from_profile,
                    image: from_image,
                };
            }
        }
        Self::Agree
    }
}

/// A boot image and where it handed off, kept together so a re-boot cannot skip the
/// check that got it here.
/// Where a booted machine.s execution ended up.
///
/// A bare address rather than a handle on the image that produced it. `BootImage` owns a
/// ROM buffer and is not `Clone`, so wrapping it here would force every VM to own a
// second copy of the firmware just to remember where it got to 2014 and the address is
/// the only thing a caller can act on. A caller that wants to re-boot holds its own
/// image; `Vm::boot` takes it by reference for exactly that reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootHandoff {
    entry: InstructionAddress,
}

impl BootHandoff {
    pub(crate) const fn new(entry: InstructionAddress) -> Self {
        Self { entry }
    }

    /// Where the bootloader left execution.
    pub const fn entry(self) -> InstructionAddress {
        self.entry
    }
}
