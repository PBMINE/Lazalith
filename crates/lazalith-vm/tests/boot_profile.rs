//! B13: the boot chain as a profile.
//!
//! # What is being held here
//!
//! **No firmware is implemented, and that is §34's instruction, not an omission.** What
//! is held is the *shape* of the chain: which firmware architecture a machine has, where
//! it lives, where it hands off, and what the two architectures that do not exist are
//! called. `a_firmware_that_is_not_implemented_is_refused_by_name` is the test.
//!
//! **The layout is not duplicated.** This file once defined a `LazLayout` of its own, so a
//! boot profile could be checked without loading a machine, and B4.s
//! `the_machine_geometry_is_defined_once` correctly refused it: `lazalith-vm` already
//! depends on `lazalith-machine`, so the layering argument bought nothing and the cost was
//! a second copy of the machine.s numbers. It now uses `LZA64_LAYOUT`, and that
//! architecture test is the reason it cannot drift again.
//!
//! **An entry outside the ROM is refused, not clamped.** Clamping would make the firmware
//! execute from the start of its ROM while a profile claimed otherwise, and the machine
//! would run without anyone noticing which entry it used.
//!
//! **A step limit of zero is refused.** It would declare the firmware stuck before
//! running a single instruction, which is a profile that cannot boot rather than one that
//! boots quickly.

use lazalith_machine::LZA64_LAYOUT;
use lazalith_types::InstructionAddress;
use lazalith_vm::{
    BootChain, BootProfile, BootProfileError, ChainStage, FirmwareProfile, MAX_FIRMWARE_BYTES,
};

fn profile() -> BootProfile {
    BootProfile::for_layout(&LZA64_LAYOUT)
}

// -- the architectures --------------------------------------------------------

#[test]
fn only_the_minimal_firmware_is_constructible() {
    assert!(FirmwareProfile::Minimal.is_constructible());
    for firmware in [FirmwareProfile::BiosLike, FirmwareProfile::UefiLike] {
        assert!(
            !firmware.is_constructible(),
            "{firmware} is named by §34 and not built"
        );
        assert!(!firmware.as_str().is_empty());
    }
    assert_eq!(
        FirmwareProfile::ALL.len(),
        3,
        "and the list is exhaustive, so a fourth architecture would fail this"
    );
}

#[test]
fn a_firmware_that_is_not_implemented_is_refused_by_name() {
    for firmware in [FirmwareProfile::BiosLike, FirmwareProfile::UefiLike] {
        let profile = profile().with_firmware(firmware);
        let error = profile
            .validate()
            .expect_err("an unimplemented firmware is refused");
        assert_eq!(
            error,
            BootProfileError::UnconstructibleFirmware { firmware },
            "the refusal names *which* firmware is missing: a caller who wrote BiosLike \
             needs to know it is the BIOS firmware that does not exist, because that is a \
             different piece of work from any other firmware question"
        );
    }
    assert!(profile().validate().is_ok(), "and the minimal one is fine");
}

#[test]
fn no_firmware_has_writable_variables_and_that_is_the_definition() {
    for firmware in FirmwareProfile::ALL {
        assert!(
            !firmware.has_writable_variables(),
            "{firmware} has writable variables"
        );
    }
    // A writable variable store is what distinguishes a BIOS-like or UEFI-like
    // firmware from a ROM: both offer a guest a place to write settings that survive a
    // reset. `Minimal` has a ROM, and a ROM is not writable -- the same reason B4's
    // profile maps the boot ROM read-only. So the answer is a fact about the
    // architecture rather than a missing feature, and a caller needing one is asking
    // for a firmware this platform does not have.
}

// -- the profile's own checks --------------------------------------------------

#[test]
fn a_profile_from_the_layout_is_valid() {
    let profile = profile();
    assert!(profile.validate().is_ok());
    assert_eq!(profile.firmware, FirmwareProfile::Minimal);
    assert_eq!(
        profile.entry,
        InstructionAddress::new(LZA64_LAYOUT.boot_rom_start),
        "the entry is the reset vector"
    );
    assert_eq!(
        profile.handoff,
        InstructionAddress::new(LZA64_LAYOUT.kernel_load_address),
        "and the handoff is where a kernel is loaded"
    );
    assert!(
        profile.step_limit > 0,
        "with a bound that would not refuse it"
    );
}

#[test]
fn an_entry_outside_the_rom_is_refused_rather_than_clamped() {
    let outside = BootProfile {
        entry: InstructionAddress::new(LZA64_LAYOUT.physical_ram_start),
        ..profile()
    };
    assert_eq!(
        outside.validate(),
        Err(BootProfileError::EntryOutsideRom {
            entry: LZA64_LAYOUT.physical_ram_start,
            start: LZA64_LAYOUT.boot_rom_start,
            end: LZA64_LAYOUT.boot_rom_start + LZA64_LAYOUT.boot_rom_length - 1,
        }),
        "clamping would make the firmware execute from the start of its ROM while the \
         profile claimed another entry, and nothing would notice which one ran"
    );
}

#[test]
fn an_image_larger_than_the_payload_limit_is_refused() {
    // A window may be as large as the layout allows; an *image* may not exceed the
    // payload limit. The first version checked the window against the payload limit, so
    // every default-layout machine was refused for a ROM one byte over its allowance.
    assert!(profile().image_fits(MAX_FIRMWARE_BYTES).is_ok());
    assert_eq!(
        profile().image_fits(MAX_FIRMWARE_BYTES + 1),
        Err(BootProfileError::FirmwareTooLarge {
            length: MAX_FIRMWARE_BYTES + 1,
            limit: MAX_FIRMWARE_BYTES,
        })
    );
    assert!(
        profile().validate().is_ok(),
        "and a default window, which is larger than the payload limit, is not a refusal"
    );
}

#[test]
fn an_empty_rom_and_a_zero_step_limit_are_refused() {
    let empty = BootProfile {
        rom_length: 0,
        ..profile()
    };
    assert_eq!(empty.validate(), Err(BootProfileError::EmptyRom));

    let no_limit = profile().with_step_limit(0);
    assert_eq!(
        no_limit.validate(),
        Err(BootProfileError::NoStepLimit),
        "a step limit of zero would declare the firmware stuck before running a single \
         instruction: a profile that cannot boot rather than one that boots quickly"
    );
}

#[test]
fn a_rom_window_past_the_address_space_is_refused() {
    let runaway = BootProfile {
        rom: lazalith_types::PhysicalAddress::new(u64::MAX - 4),
        rom_length: 16,
        ..profile()
    };
    assert!(matches!(
        runaway.validate(),
        Err(BootProfileError::RomOutOfRange { .. })
    ));
}

#[test]
fn the_rom_window_answers_exactly_its_own_addresses() {
    let profile = profile();
    assert!(profile.rom_contains(lazalith_types::PhysicalAddress::new(
        LZA64_LAYOUT.boot_rom_start
    )));
    assert!(!profile.rom_contains(lazalith_types::PhysicalAddress::new(
        LZA64_LAYOUT.boot_rom_start + LZA64_LAYOUT.boot_rom_length
    )));
}

// -- the chain ----------------------------------------------------------------

#[test]
fn the_chain_names_the_three_stages_in_order() {
    let chain = BootChain::for_profile(profile());
    assert_eq!(chain.stages.len(), 3);
    assert_eq!(
        chain.stages,
        alloc_vec_of_stages(),
        "firmware, then the bootloader inside it, then LazOS -- §34's chain, in order"
    );
    let described = chain.describe();
    assert!(described.contains("firmware"));
    assert!(described.contains("bootloader"));
    assert!(described.contains("LazOS"));
    assert_eq!(chain.to_string(), described);
}

fn alloc_vec_of_stages() -> Vec<ChainStage> {
    vec![
        ChainStage::Firmware,
        ChainStage::Bootloader,
        ChainStage::LazOs,
    ]
}

#[test]
fn the_bootloader_is_a_stage_and_not_a_second_firmware() {
    // Phase-I's `BootImage` is a ROM containing a bootloader. Splitting them in the
    // chain makes it legible without inventing a second image format, and the stage
    // list is the place that shows the split happened.
    let chain = BootChain::for_profile(profile());
    let firmware = chain
        .stages
        .iter()
        .position(|stage| *stage == ChainStage::Firmware)
        .expect("a firmware stage");
    let bootloader = chain
        .stages
        .iter()
        .position(|stage| *stage == ChainStage::Bootloader)
        .expect("a bootloader stage");
    assert!(
        firmware < bootloader,
        "the firmware is entered first and the bootloader is code inside it"
    );
}
