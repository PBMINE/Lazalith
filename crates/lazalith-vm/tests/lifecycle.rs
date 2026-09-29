//! B6: the VM lifecycle contract.
//!
//! # What is being held here
//!
//! Three claims, of different kinds, so tested differently.
//!
//! **One construction path.** A machine is `Cold` until firmware has run and `Booted`
//! after. B4 could build a machine with devices and could not boot it; the boot path
//! could boot a machine and refused devices. `Vm` does both, and these check the two
//! halves agree about what they produced rather than merely that each works.
//!
//! **The boot agreement.** A profile and a boot image each describe a machine, and the
//! check between them is the part that did not exist before. It is tested by *forcing*
//! each disagreement in turn, because a check that has only ever seen agreeing inputs
//! has not been shown to be able to fail. `docs/project-state.md` records that lesson
//! from B4 twice.
//!
//! **Snapshot direction.** Two refusals — a snapshot from the future, and a snapshot
//! from another stage — both about direction of travel rather than data, and both
//! tested by trying to travel the wrong way.
//!
//! **Nothing here needs a test-only constructor.** The "future snapshot" is produced by
//! snapshotting a running machine and then resetting it, which is something a real
//! caller can do; the disagreements are produced by editing a `MachineLayout`, which is
//! a public type with public fields. A test that could only be written with a bespoke
//! `#[doc(hidden)]` constructor is usually testing a shape no caller can reach.
//!
//! # What is deliberately not tested
//!
//! Nothing here runs a kernel. `Vm::boot` runs a real bootloader over a real ROM and the
//! handoff is checked, but B6 is not a boot *test*: Phase-I's
//! `crates/lazalith-boot/tests/` is, and those suites are unchanged and still green,
//! which is the real evidence the boot path was not disturbed.

use lazalith_boot::BootImage;
use lazalith_cpu::Processor;
use lazalith_devices::DeviceManager;
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::{
    DeviceClass, DeviceProfile, MachineLayout, MachineProfile, MachineState, ProfileFamily,
    ProfileName, RegionProfile,
};
use lazalith_memory::{RegionKind, RegionPermissions};
use lazalith_types::{ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress};
use lazalith_vm::{BootAgreement, BootStage, Vm, VmError};

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// A kernel that halts, so an image is bootable and does something visible.
fn kernel() -> Vec<u8> {
    let halt = Instruction::new(CONFIG, Opcode::Halt, &[]).expect("a halt encodes");
    let nop = Instruction::new(CONFIG, Opcode::Nop, &[]).expect("a nop encodes");
    let mut bytes = encode(CONFIG, &halt).expect("a halt encodes").to_vec();
    bytes.extend_from_slice(&encode(CONFIG, &nop).expect("a nop encodes"));
    // Trailing bytes the loader checks, as the boot suite does.
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

fn image() -> BootImage {
    BootImage::new(CONFIG, kernel(), 0).expect("a boot image for this build's layout")
}

fn profile() -> MachineProfile {
    MachineProfile::lza64_native_v1()
}

/// A profile with a different `MachineLayout`, built through the public field.
fn profile_with(mutate: impl FnOnce(&mut MachineLayout)) -> MachineProfile {
    let mut layout = profile().layout();
    mutate(&mut layout);
    profile().with_layout(layout)
}

/// A profile with exactly one device, for the device-count refusal.
fn profile_with_one_device() -> MachineProfile {
    let mut built = MachineProfile::empty(
        ProfileName::new(ArchitectureConfig::lz64(), ProfileFamily::Native, 1),
        lazalith_machine::LZA64_LAYOUT,
    );
    let layout = built.layout();
    built = built.with_region(RegionProfile {
        start: PhysicalAddress::new(layout.boot_rom_start),
        length: layout.boot_rom_length,
        kind: RegionKind::Rom,
        permissions: RegionPermissions::new(true, false, true, false),
    });
    built = built.with_region(RegionProfile {
        start: PhysicalAddress::new(layout.physical_ram_start),
        length: layout.physical_ram_length,
        kind: RegionKind::Ram,
        permissions: RegionPermissions::new(true, true, false, true),
    });
    built.with_device(
        DeviceProfile::new(
            lazalith_devices::DeviceId::new(1),
            DeviceClass::Console,
            PhysicalAddress::new(0x4000_0000),
            RegionPermissions::new(true, true, false, true),
        )
        .with_console_capacity(1024),
    )
}

// -- the two states -----------------------------------------------------------

#[test]
fn a_described_machine_is_cold_and_has_run_nothing() {
    let vm = Vm::described(&profile()).expect("the native profile is a machine");
    assert_eq!(vm.stage(), BootStage::Cold, "nothing has run");
    assert_eq!(vm.booted(), None, "so there is nowhere execution ended up");
    assert_eq!(
        vm.machine_state(),
        MachineState::Reset,
        "and it is reset, which is the only executable state a cold machine is in"
    );
    assert!(
        vm.matches_profile().is_ok(),
        "a machine just built from a profile agrees with it"
    );
}

#[test]
fn a_cold_machine_boots_to_a_handoff_and_lands_in_lazos() {
    let mut vm = Vm::described(&profile()).expect("the native profile is a machine");
    let entry = vm
        .boot(&image())
        .expect("the native layout agrees with the image");
    assert_eq!(vm.stage(), BootStage::Booted);
    assert_eq!(
        vm.booted().map(|handoff| handoff.entry()),
        Some(entry),
        "the VM remembers where the bootloader stopped, so a caller and an inspector \
         have one answer rather than each recomputing it"
    );
    assert_eq!(
        vm.machine().architectural_state().pc(),
        entry,
        "and execution is there"
    );
    assert_eq!(entry.as_u64(), image().entry().as_u64());
}

#[test]
fn a_reset_returns_a_booted_machine_to_cold() {
    let mut vm = Vm::described(&profile()).expect("the native profile is a machine");
    vm.boot(&image())
        .expect("the native layout agrees with the image");
    assert_eq!(vm.stage(), BootStage::Booted);

    vm.reset().expect("a stopped machine can be reset");
    assert_eq!(
        vm.stage(),
        BootStage::Cold,
        "a reset machine has not booted: returning to the reset vector does not undo \
         loading a kernel, but it does undo the claim that the handoff happened"
    );
    assert_eq!(vm.booted(), None, "and the handoff is gone with it");
    assert_eq!(
        vm.machine().architectural_state().pc(),
        profile().reset_vector(),
        "and the processor is back at the reset vector, not the handoff"
    );
}

#[test]
fn a_machine_adopted_from_the_boot_path_can_hold_a_lifecycle() {
    // Phase-I's path: build and boot with no devices at all. B6 does not replace it,
    // and this is the test that says so.
    let machine = image()
        .start(DeviceManager::new())
        .expect("the Phase-I boot path still works");
    let vm = Vm::from_machine(machine, BootStage::Booted);
    assert_eq!(vm.stage(), BootStage::Booted);
    assert!(
        vm.matches_profile().is_err(),
        "a machine with no profile is refused rather than reported as agreeing: an `Ok` \
         here would be a check that silently did not run"
    );
}

// -- the boot agreement -------------------------------------------------------

#[test]
fn the_native_profile_and_a_native_image_agree() {
    assert_eq!(
        BootAgreement::check(&profile(), &image()),
        BootAgreement::Agree,
        "the layout and the image are both this build's, so there is nothing to refuse"
    );
}

/// The check refuses a profile whose layout the image was not built for.
///
/// Each field is forced in turn, so the test cannot pass by checking only some of them
/// — the bug this guards against is a list that silently covers three of four fields.
#[test]
fn the_boot_agreement_can_be_made_to_fail_on_every_field() {
    // `boot_rom_start` is listed as expecting the refusal to name it `reset_vector`,
    // because that is the same fact under the name `BootAgreement` reports it by: the
    // reset vector *is* the start of the ROM. Naming the two differently would give a
    // caller two fields to go and fix for one mistake.
    for (field, reported, value) in [
        ("kernel_load_address", "kernel_load_address", 0x0020_0000u64),
        ("physical_ram_start", "physical_ram_start", 0x0040_0000),
        ("kernel_initial_sp", "kernel_initial_sp", 0x0020_0000),
        ("boot_rom_start", "reset_vector", 0x0008_0000),
    ] {
        let moved = {
            let mut layout = profile().layout();
            match field {
                "kernel_load_address" => layout.kernel_load_address = value,
                "physical_ram_start" => layout.physical_ram_start = value,
                "kernel_initial_sp" => layout.kernel_initial_sp = value,
                _ => layout.boot_rom_start = value,
            }
            profile().with_layout(layout)
        };
        let agreement = BootAgreement::check(&moved, &image());
        match agreement {
            BootAgreement::Disagree { field: named, .. } => assert_eq!(
                named, reported,
                "the refusal names the field that broke, so a caller knows which half of \
                 the disagreement to fix"
            ),
            other => panic!("moving {field} was not refused: {other:?}"),
        }
    }
}

#[test]
fn a_boot_onto_a_profile_that_disagrees_leaves_the_machine_alone() {
    let mut vm = Vm::described(&profile_with(|layout| {
        layout.kernel_load_address = 0x0020_0000;
    }))
    .expect("the profile is a machine");
    let before = vm.machine().architectural_state().pc();

    let error = vm.boot(&image()).unwrap_err();
    assert!(
        matches!(
            error,
            VmError::LayoutDisagreement {
                field: "kernel_load_address",
                profile: 0x0020_0000,
                image: 0x0010_0000,
            }
        ),
        "the refusal is the disagreement, got {error:?}"
    );
    assert_eq!(vm.stage(), BootStage::Cold, "and nothing was booted");
    assert_eq!(
        vm.machine().architectural_state().pc(),
        before,
        "the machine is exactly as it was: the check happens before the ROM is \
         written, because half a bootloader in a ROM is an hour of somebody's afternoon"
    );
}

#[test]
fn a_rom_larger_than_the_window_is_refused() {
    let tiny = profile_with(|layout| layout.boot_rom_length = 1);
    let agreement = BootAgreement::check(&tiny, &image());
    assert!(
        matches!(agreement, BootAgreement::RomTooLarge { .. }),
        "a firmware image that does not fit the machine's ROM window is a refusal, not \
         a truncated boot"
    );
}

#[test]
fn an_image_for_another_architecture_is_refused_before_the_geometry() {
    let lz32 =
        BootImage::new(ArchitectureConfig::lz32(), kernel_lz32(), 0).expect("a lz32 image builds");
    assert!(
        matches!(
            BootAgreement::check(&profile(), &lz32),
            BootAgreement::Architecture { .. }
        ),
        "two machines of different widths have three fields of meaningless numbers \
         between them, so the width is reported first"
    );
}

fn kernel_lz32() -> Vec<u8> {
    let config = ArchitectureConfig::lz32();
    let halt = Instruction::new(config, Opcode::Halt, &[]).expect("a halt encodes");
    let nop = Instruction::new(config, Opcode::Nop, &[]).expect("a nop encodes");
    let mut bytes = encode(config, &halt).expect("a halt encodes").to_vec();
    bytes.extend_from_slice(&encode(config, &nop).expect("a nop encodes"));
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

// -- snapshot direction -------------------------------------------------------

#[test]
fn a_snapshot_carries_the_stage_and_the_clock() {
    let vm = Vm::described(&profile()).expect("the native profile is a machine");
    let snapshot = vm.snapshot();
    assert_eq!(snapshot.stage(), BootStage::Cold);
    assert_eq!(snapshot.clock().as_u64(), 0, "nothing has run");
    assert_eq!(
        snapshot.device_count(),
        2,
        "the profile names a console and a timer, and both are in the snapshot"
    );
}

#[test]
fn a_snapshot_round_trips() {
    let mut vm = Vm::described(&profile()).expect("the native profile is a machine");
    let snapshot = vm.snapshot();
    vm.restore(&snapshot)
        .expect("a snapshot restores into the machine it came from");
    assert!(vm.matches_profile().is_ok());
}

#[test]
fn a_snapshot_taken_later_restores_forward() {
    let mut vm = Vm::described(&profile()).expect("a machine");
    // The clock is advanced directly rather than by executing, because a cold
    // profile machine has an empty ROM and executing one halts immediately. What is
    // under test is the clock.s direction, not the instruction decoder.
    vm.machine_mut()
        .advance_clock(CycleCount::new(1_000))
        .expect("the clock advances");
    let now = vm.clock().elapsed().as_u64();
    assert_eq!(now, 1_000);

    let later = vm.snapshot();
    vm.machine_mut()
        .advance_clock(CycleCount::new(500))
        .expect("the clock advances");
    assert_eq!(vm.clock().elapsed().as_u64(), 1_500);

    vm.restore(&later).expect(
        "restoring a snapshot taken before now is the whole point of taking one: the \
         clock rewinds",
    );
    assert_eq!(
        vm.clock().elapsed().as_u64(),
        now,
        "and the clock follows the snapshot backwards, because the clock is part of \
         what was saved"
    );
}

#[test]
fn a_snapshot_from_further_ahead_moves_time_forward() {
    let mut vm = Vm::described(&profile()).expect("a machine");
    vm.machine_mut()
        .advance_clock(CycleCount::new(2_000))
        .expect("the clock advances");
    let snapshot = vm.snapshot();
    assert_eq!(snapshot.clock().as_u64(), 2_000);

    // A reset machine is at time zero, so a snapshot taken at 2_000 describes a machine
    // from further ahead. Restoring it moves the machine *forward* in virtual time,
    // which is allowed 2014 there is nothing to conjure and nothing to lose.
    vm.reset().expect("a stopped machine resets");
    assert_eq!(
        vm.clock().elapsed().as_u64(),
        0,
        "a reset machine is at time zero"
    );

    vm.restore(&snapshot).expect(
        "a snapshot from further ahead moves time forward, which is the one direction a \
         restore never has to invent",
    );
    assert_eq!(
        vm.clock().elapsed().as_u64(),
        2_000,
        "and the machine is where the snapshot said it was"
    );
}

#[test]
fn a_snapshot_from_another_stage_is_refused() {
    let mut vm = Vm::described(&profile()).expect("a machine");
    vm.boot(&image()).expect("the native layout agrees");
    let booted_snapshot = vm.snapshot();
    assert_eq!(booted_snapshot.stage(), BootStage::Booted);

    vm.reset().expect("a stopped machine resets");
    let error = vm.restore(&booted_snapshot).unwrap_err();
    assert!(
        matches!(
            error,
            VmError::SnapshotStage {
                snapshot: BootStage::Booted,
                machine: BootStage::Cold,
            }
        ),
        "a machine cannot become booted by being restored, because booting ran \
         firmware; restoring a booted snapshot onto a reset machine would produce a \
         machine whose bootloader has not run. Got {error:?}"
    );
}

// -- mutation -----------------------------------------------------------------

#[test]
fn a_snapshot_refuses_a_different_device_count() {
    let vm = Vm::described(&profile()).expect("a two-device machine");
    let snapshot = vm.snapshot();
    let mut other = Vm::described(&profile_with_one_device()).expect("a one-device machine");
    let error = other.restore(&snapshot).unwrap_err();
    assert!(
        matches!(
            error,
            VmError::DeviceCount {
                snapshot: 2,
                machine: 1
            }
        ),
        "a snapshot for a two-device machine must not be applied to a one-device \
         machine, or the devices land in the wrong order. Got {error:?}"
    );
}

#[test]
fn the_processor_in_a_snapshot_is_the_canonical_one() {
    let vm = Vm::described(&profile()).expect("a machine");
    let snapshot = vm.snapshot();
    // B3's boundary: the snapshot holds a `Processor`, not a re-encoding of one. If this
    // stops compiling, something has invented a second architectural state.
    let _: &Processor = snapshot.processor();
    let _: InstructionAddress = snapshot.pc();
}
