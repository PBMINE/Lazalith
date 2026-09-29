//! B13: the boot chain as a profile, without any firmware.
//!
//! # §34, and what "separate" means here
//!
//! §34 draws a chain and then says "Do not implement them during this architecture pass":
//!
//! ```text
//! VM Manager        ← B19
//!  ↓
//! Machine Profile
//!  ↓
//! Firmware          ← this module
//!  ↓
//! Bootloader
//!  ↓
//! LazOS
//! ```
//!
//! So nothing here is firmware. What is here is **the shape of the chain**: a profile
//! records which of the three firmware architectures it uses, what the firmware is
//! allowed to do, and where the handoff lands — and the two that do not exist are
//! *named and refused*, the same treatment B4 gave `lza64-at-v1` and B7 gave
//! `DisplayProfile::VgaCompatible`.
//!
//! # Why a profile and not a type per firmware
//!
//! Three firmware architectures, three machines? That would be three copies of every
//! question asked about booting, and B6 already found the alternative: the firmware is
//! *bytes a caller supplies*, and the machine is a profile plus a check. A profile naming
//! a firmware architecture is a machine that knows what it is booting, which is one fact.
//!
//! # The three architectures, and why two are refused
//!
//! | Architecture | What it is | Here |
//! | --- | --- | --- |
//! | `Minimal` | one boot ROM, the current boot image format | **buildable** |
//! | `BiosLike` | a BIOS-compatible firmware, with services and a boot menu | named, refused |
//! | `UefiLike` | UEFI: variables, services, multiple boot entries | named, refused |
//!
//! `BiosLike` and `UefiLike` are refused for the same reason and in the same shape as
//! §34's "do not implement them": a firmware that claims to be BIOS-compatible and
//! implements a third of it is worse than one that does not exist, because a guest
//! probing for the other two thirds gets wrong answers. A guest that finds a
//! BIOS-compatible firmware and finds none of BIOS behaves like a machine with no
//! firmware and hangs; a guest that finds none at all says so.
//!
//! This is the same argument as B5's read-only boot ROM, B7's unimplemented VGA, B8's
//! unimplemented controllers, B9's unimplemented PS/2 and B11's absent host backends. It
//! is a pattern: **naming an unimplemented compatibility surface is worth doing, and
//! pretending to implement part of it is not.**

use alloc::vec::Vec;
use core::fmt;

use lazalith_machine::{LZA64_LAYOUT, MachineLayout};
use lazalith_types::{CycleCount, InstructionAddress, PhysicalAddress};

/// A firmware architecture, as §34 names them.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum FirmwareProfile {
    /// One boot ROM, in the current boot image format.
    ///
    /// **The only one that builds.** It is what Phase-I's `BootImage` already is: a ROM
    /// with a header and a payload, executed from its first instruction, handing off to
    /// LazOS at a fixed address.
    Minimal,
    /// A BIOS-compatible firmware.
    BiosLike,
    /// A UEFI-like firmware.
    UefiLike,
}

impl FirmwareProfile {
    /// Every profile, for a test that checks all of them.
    pub const ALL: &'static [Self] = &[Self::Minimal, Self::BiosLike, Self::UefiLike];

    /// Whether this build can produce firmware of this architecture.
    pub const fn is_constructible(self) -> bool {
        matches!(self, Self::Minimal)
    }

    /// The name, for a diagnostic and for a profile that names one.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::BiosLike => "bios-like",
            Self::UefiLike => "uefi-like",
        }
    }

    /// Whether this architecture has writable firmware variables.
    ///
    /// **`false` for all three, and that is not a gap — it is the definition.**
    ///
    /// A writable variable store is what distinguishes a BIOS-like or UEFI-like
    /// firmware from a ROM: both offer a guest a place to *write* settings that survive a
    /// reset, and both use that place for boot order, console redirection and secure
    /// boot. `Minimal` has a ROM, and a ROM is not writable — which is the same reason
    /// B4's profile maps the boot ROM read-only.
    ///
    /// So the question "does this machine have writable firmware variables" is answered
    /// `false` and the answer is a fact about the architecture rather than a missing
    /// feature. A caller that needs one is asking for a firmware this platform does not
    /// have, and `FirmwareProfile::is_constructible` is where that is refused.
    pub const fn has_writable_variables(self) -> bool {
        false
    }
}

impl fmt::Display for FirmwareProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The largest firmware ROM this platform's layout leaves room for.
///
/// `LZA64_LAYOUT::max_boot_rom_payload`, repeated here as a bound rather than imported,
/// because a profile must be checkable without the machine crate and a boot profile that
/// cannot be validated without loading a machine is a boot profile that is validated too
/// late. The test in `tests/boot_profile.rs` checks the two agree.
pub const MAX_FIRMWARE_BYTES: u64 = 0x0007_f000;

/// Where the firmware lives and what it hands off to.
///
/// **A description, not firmware.** Nothing here is code. Every field is a fact about the
/// machine that a caller already knows when it builds the profile, and every field is
/// something a check can refuse.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootProfile {
    /// Which firmware architecture this machine has.
    pub firmware: FirmwareProfile,
    /// Where the firmware ROM is mapped.
    pub rom: PhysicalAddress,
    /// How large the ROM window is.
    pub rom_length: u64,
    /// Where the firmware is entered.
    pub entry: InstructionAddress,
    /// Where execution resumes after the firmware.
    pub handoff: InstructionAddress,
    /// The stack pointer the firmware starts with.
    pub stack: u64,
    /// How many instructions the firmware may execute before it is declared stuck.
    ///
    /// **A bound, not a timeout.** Virtual time does not advance for firmware on its own,
    /// so a firmware that loops forever would loop forever. A step limit is what turns
    /// "the firmware hung" from a hang into a refusal, and B6's boot path already relies
    /// on it: `BootError::BootStepLimit`.
    pub step_limit: u64,
    /// The virtual time the machine starts at.
    pub initial_time: CycleCount,
}

impl BootProfile {
    /// A profile for this machine's layout, with the defaults a fresh machine has.
    ///
    /// The entry is the reset vector and the handoff is the kernel load address, which is
    /// what `LZA64_LAYOUT` says — and which B6's `BootAgreement` already checks against a
    /// boot image. This constructor is the one place those two defaults are written down.
    pub const fn for_layout(layout: &MachineLayout) -> Self {
        Self {
            firmware: FirmwareProfile::Minimal,
            rom: PhysicalAddress::new(layout.boot_rom_start),
            rom_length: layout.boot_rom_length,
            entry: InstructionAddress::new(layout.boot_rom_start),
            handoff: InstructionAddress::new(layout.kernel_load_address),
            stack: layout.kernel_initial_sp,
            step_limit: 1_000_000,
            initial_time: CycleCount::new(0),
        }
    }

    /// The same profile, with this firmware architecture.
    pub const fn with_firmware(mut self, firmware: FirmwareProfile) -> Self {
        self.firmware = firmware;
        self
    }

    /// The same profile, with this step limit.
    pub const fn with_step_limit(mut self, step_limit: u64) -> Self {
        self.step_limit = step_limit;
        self
    }

    /// Everything wrong with this profile, or `Ok(())`.
    pub fn validate(&self) -> Result<(), BootProfileError> {
        if !self.firmware.is_constructible() {
            return Err(BootProfileError::UnconstructibleFirmware {
                firmware: self.firmware,
            });
        }
        if self.rom_length == 0 {
            return Err(BootProfileError::EmptyRom);
        }
        let Some(last) = self.rom_length.checked_sub(1) else {
            return Err(BootProfileError::EmptyRom);
        };
        let Some(rom_end) = self.rom.as_u64().checked_add(last) else {
            return Err(BootProfileError::RomOutOfRange {
                start: self.rom.as_u64(),
                length: self.rom_length,
            });
        };
        if !self
            .entry
            .as_u64()
            .checked_sub(self.rom.as_u64())
            .is_some_and(|offset| offset <= rom_end - self.rom.as_u64())
        {
            return Err(BootProfileError::EntryOutsideRom {
                entry: self.entry.as_u64(),
                start: self.rom.as_u64(),
                end: rom_end,
            });
        }
        if self.step_limit == 0 {
            return Err(BootProfileError::NoStepLimit);
        }
        Ok(())
    }

    /// Whether an image of this many bytes fits in this machine.s ROM window.
    ///
    /// **The window and the payload limit are different numbers** and the first version
    /// compared the window against the payload limit, so every machine with a default
    /// layout was refused for a ROM one byte larger than its payload allowance. The
    /// window is where the ROM is mapped; the payload limit is how much of it may hold.
    /// `validate` checks the window; this checks the image.
    pub fn image_fits(&self, bytes: u64) -> Result<(), BootProfileError> {
        if bytes > MAX_FIRMWARE_BYTES {
            return Err(BootProfileError::FirmwareTooLarge {
                length: bytes,
                limit: MAX_FIRMWARE_BYTES,
            });
        }
        Ok(())
    }

    /// Whether `address` falls in this machine.s ROM window.
    pub fn rom_contains(&self, address: PhysicalAddress) -> bool {
        let start = self.rom.as_u64();
        let at = address.as_u64();
        at >= start && at < start.saturating_add(self.rom_length)
    }
}

impl Default for BootProfile {
    fn default() -> Self {
        Self::for_layout(&LZA64_LAYOUT)
    }
}

/// Why a boot profile was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootProfileError {
    /// A firmware architecture this build cannot produce.
    ///
    /// **Named, not folded into a general "unsupported".** A caller who wrote
    /// `BiosLike` needs to know it is *the BIOS firmware* that is missing, because that
    /// is a different piece of work from any other firmware question.
    UnconstructibleFirmware {
        /// Which architecture was asked for.
        firmware: FirmwareProfile,
    },
    /// A ROM window of zero bytes.
    EmptyRom,
    /// A firmware *image* larger than the payload limit.
    ///
    /// About the image, not the window — see [`BootProfile::image_fits`]. The window is
    /// where the ROM is mapped and is as large as the layout allows; the payload limit
    /// is how much of it may hold.
    FirmwareTooLarge {
        /// The image size asked for.
        length: u64,
        /// The largest payload this platform allows.
        limit: u64,
    },
    /// A ROM window that runs past the end of the address space.
    RomOutOfRange {
        /// Where the window starts.
        start: u64,
        /// How long it is.
        length: u64,
    },
    /// A firmware entry point outside its own ROM.
    ///
    /// **Refused rather than clamped.** An entry outside the ROM would fetch from
    /// wherever the entry points — possibly the kernel it is supposed to be loading —
    /// and the machine would execute a kernel that had not been loaded. Clamping would
    /// hide that.
    EntryOutsideRom {
        /// The entry point.
        entry: u64,
        /// Where the ROM starts.
        start: u64,
        /// Where the ROM ends.
        end: u64,
    },
    /// A step limit of zero, which would refuse the firmware before running it.
    NoStepLimit,
}

impl fmt::Display for BootProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnconstructibleFirmware { firmware } => write!(
                f,
                "this build cannot produce a {firmware} firmware; only the minimal \
                 architecture exists, and a firmware that claims BIOS or UEFI \
                 compatibility and implements a third of it is worse than none"
            ),
            Self::EmptyRom => f.write_str("a ROM window of zero bytes holds no firmware"),
            Self::FirmwareTooLarge { length, limit } => {
                write!(
                    f,
                    "a {length} byte firmware image is over the {limit} byte payload limit"
                )
            }
            Self::RomOutOfRange { start, length } => {
                write!(
                    f,
                    "a {length} byte ROM at {start:#x} runs past the address space"
                )
            }
            Self::EntryOutsideRom { entry, start, end } => write!(
                f,
                "the firmware entry {entry:#x} is outside its own ROM, {start:#x}..={end:#x}"
            ),
            Self::NoStepLimit => {
                f.write_str("a step limit of zero would refuse the firmware before running it")
            }
        }
    }
}

impl core::error::Error for BootProfileError {}

/// The chain §34 draws, as a value.
///
/// **A description of the chain, not a run of it.** A `BootChain` says what will run and
/// in what order; executing it is B6's `Vm::boot`, which already exists and already
/// checks the profile against a boot image. This is the part B6 needed and did not have:
/// the *declared* chain, so a machine can be shown what it will do before it does it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootChain {
    /// The firmware profile.
    pub profile: BootProfile,
    /// What the firmware is, by name, for a display.
    pub firmware_name: alloc::string::String,
    /// The stages, in order.
    pub stages: Vec<ChainStage>,
}

/// One step of the chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChainStage {
    /// Firmware: a ROM executed from its entry.
    Firmware,
    /// The bootloader, which is code *in* the firmware's ROM.
    ///
    /// **A separate stage, not a separate thing.** Phase-I's `BootImage` is a ROM
    /// containing a bootloader; splitting them here makes the chain legible without
    /// inventing a second image format.
    Bootloader,
    /// LazOS, where execution ends up.
    LazOs,
}

impl ChainStage {
    /// The name, for a display.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Firmware => "firmware",
            Self::Bootloader => "bootloader",
            Self::LazOs => "LazOS",
        }
    }
}

impl fmt::Display for ChainStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl BootChain {
    /// The chain this platform actually has, for a validated profile.
    pub fn for_profile(profile: BootProfile) -> Self {
        Self {
            firmware_name: alloc::string::String::from(profile.firmware.as_str()),
            profile,
            stages: alloc::vec![
                ChainStage::Firmware,
                ChainStage::Bootloader,
                ChainStage::LazOs
            ],
        }
    }

    /// A one-line description, for a manager showing what a machine will do.
    pub fn describe(&self) -> alloc::string::String {
        let mut out = self.firmware_name.clone();
        for stage in &self.stages {
            out.push_str(" → ");
            out.push_str(stage.as_str());
        }
        out
    }
}

impl fmt::Display for BootChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.describe())
    }
}
