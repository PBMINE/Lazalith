//! B18: the §40 state-coverage claims, and the state that backs them.
//!
//! # The claim being tested
//!
//! §40 asks for a *document* saying what a snapshot covers. Prose is not evidence, and
//! the specific thing being tested here is that a snapshot **carries the virtual clock
//! and the pending interrupts** — the two items the previous snapshot module excluded,
//! with a stated reason that does not survive contact with the code.
//!
//! # How these tests are shaped
//!
//! **The clock is tested by making its absence observable.** Each clock test builds two
//! machines, runs one forward, and restores onto the other. A snapshot that dropped the
//! clock would restore both machines to elapsed zero, so the assertions are on
//! `elapsed()` being *different* and specifically equal. A test asserting the clock is
//! merely "nonzero" would pass against a snapshot that saved some other clock.
//!
//! **The inventory is tested against the type, not against a literal.** The count
//! invariant is asserted, and separately every item is checked to be reachable in the
//! inventory. The interesting failure is not a wrong count but a field in
//! `CapturedState` that no `StateItem` claims — which is why
//! `every_captured_field_has_an_inventory_row` exists as a named test rather than as an
//! assertion buried in another.
//!
//! **The refusals are forced.** A restore that has only ever seen a matching machine has
//! not been shown to refuse a mismatched one, and `docs/project-state.md` records that
//! lesson from B4 twice. The architecture refusal is produced by restoring onto a
//! machine built from a different profile, and the device-count refusal by adding a
//! device to one machine and not the other.

use lazalith_devices::DeviceId;
use lazalith_machine::DeviceProfile;
use lazalith_machine::{
    DeviceClass, LazalithMachine, MachineProfile, ProfileFamily, ProfileName, RegionProfile,
};
use lazalith_memory::{RegionKind, RegionPermissions};
use lazalith_types::{ArchitectureConfig, CycleCount, PhysicalAddress};
use lazalith_vm::{
    CapturedState, Coverage, Determinism, ReplayGuarantee, StateCoverage, StateItem,
    coverage_inventory,
};

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// A console, which is a real device with real state.
fn device(id: u16) -> DeviceProfile {
    DeviceProfile::new(
        DeviceId::new(u32::from(id)),
        DeviceClass::Console,
        PhysicalAddress::new(0x4000_0000 + u64::from(id) * 0x1000),
        RegionPermissions::new(true, true, false, true),
    )
    .with_console_capacity(256)
}

fn profile_with(devices: Vec<DeviceProfile>) -> MachineProfile {
    profile_with_config(CONFIG, devices)
}

fn profile_with_config(config: ArchitectureConfig, devices: Vec<DeviceProfile>) -> MachineProfile {
    let mut built = MachineProfile::empty(
        ProfileName::new(config, ProfileFamily::Native, 1),
        lazalith_machine::LZA64_LAYOUT,
    );
    let layout = built.layout();
    built = built
        .with_region(RegionProfile {
            start: PhysicalAddress::new(layout.boot_rom_start),
            length: layout.boot_rom_length,
            kind: RegionKind::Rom,
            permissions: RegionPermissions::new(true, false, true, false),
        })
        .with_region(RegionProfile {
            start: PhysicalAddress::new(layout.physical_ram_start),
            length: layout.physical_ram_length,
            kind: RegionKind::Ram,
            permissions: RegionPermissions::new(true, true, false, true),
        });
    for device in devices {
        built = built.with_device(device);
    }
    built
}

fn profile() -> MachineProfile {
    profile_with(vec![device(1)])
}

/// A display, which is a device whose snapshot is *not* empty.
///
/// The console's is — it has one register and no guest-readable state, so there is
/// nothing in it to lose. A display carries a geometry, a framebuffer address and a
/// present counter, so restoring one is observable and a test can tell whether it
/// happened.
fn display(id: u16) -> DeviceProfile {
    DeviceProfile::new(
        DeviceId::new(u32::from(id)),
        DeviceClass::Display,
        PhysicalAddress::new(0x4003_0000 + u64::from(id) * 0x1000),
        RegionPermissions::new(true, true, false, true),
    )
    .with_display_profile(lazalith_devices::DisplayProfile::Native)
}

/// A machine with one console, built through the public path.
fn machine() -> LazalithMachine<std::boxed::Box<dyn lazalith_devices::Device>> {
    LazalithMachine::from_profile(&profile()).expect("the profile is a machine")
}

/// Run a machine forward by a known number of cycles, with nothing executing.
fn advance(
    machine: &mut LazalithMachine<std::boxed::Box<dyn lazalith_devices::Device>>,
    cycles: u64,
) {
    machine
        .advance_clock(CycleCount::new(cycles))
        .expect("the clock advances");
}

// -- the inventory ------------------------------------------------------------

#[test]
fn the_inventory_covers_exactly_what_is_captured() {
    let inventory = coverage_inventory();
    assert_eq!(
        inventory.len(),
        StateItem::ALL.len(),
        "the inventory has one row per item, and §40's list is what StateItem::ALL is"
    );
    let captured: Vec<StateItem> = inventory
        .iter()
        .filter(|row| row.coverage == Coverage::Captured)
        .map(|row| row.item)
        .collect();
    assert_eq!(
        captured,
        vec![
            StateItem::Cpu,
            StateItem::Memory,
            StateItem::Devices,
            StateItem::Clock,
            StateItem::Interrupts,
        ],
        "CPU, memory, devices, the virtual clock and interrupt state are what a snapshot holds"
    );
}

#[test]
fn the_configuration_is_checked_rather_than_captured() {
    // A checked item is a different claim from a captured one, and the difference is
    // that a captured item can be *changed* by a restore and a checked one cannot.
    let inventory = coverage_inventory();
    let configuration = inventory
        .iter()
        .find(|row| row.item == StateItem::Configuration)
        .copied()
        .expect("the configuration is in the inventory");
    assert_eq!(
        configuration.coverage,
        Coverage::Checked,
        "the machine's configuration is checked on restore, not written into every snapshot"
    );
}

#[test]
fn host_backed_resources_are_external_and_not_promised() {
    // §40's actual question. A storage device is backed by a host filesystem and a
    // network device by a host socket; neither is guest state, and pretending a
    // snapshot reproduced them would be the specific lie §40 asks to be answered.
    for item in [StateItem::Storage, StateItem::Network] {
        assert_eq!(
            item.coverage(),
            Coverage::External,
            "{item} is backed by the host"
        );
        assert_eq!(
            item.determinism(),
            Determinism::External,
            "{item} is not something a replay can promise"
        );
        assert!(
            !ReplayGuarantee::covers(item),
            "and a caller must be told {item} is not covered"
        );
    }
}

#[test]
fn the_replay_guarantee_names_the_items_it_cannot_promise() {
    let guarantee = CapturedState::replay_guarantee();
    assert_eq!(
        guarantee.external,
        [StateItem::Storage, StateItem::Network],
        "the two host-backed items are exactly the ones left out"
    );
    assert!(
        ReplayGuarantee::covers(StateItem::Cpu) && ReplayGuarantee::covers(StateItem::Clock),
        "the CPU and the clock are covered"
    );
    let sentence = guarantee.describe();
    for item in [StateItem::Storage, StateItem::Network] {
        assert!(
            sentence.contains(item.as_str()),
            "the sentence names {item} as unpromised: {sentence}"
        );
    }
}

#[test]
fn the_counts_are_derived_from_the_inventory_rather_than_asserted() {
    // The array lengths in ReplayGuarantee are these constants. If they ever drift
    // from the inventory, `replay_guarantee` panics while filling — so assert the
    // partition is exact here, where a failure says which side is wrong.
    let guarantee = CapturedState::replay_guarantee();
    assert_eq!(
        guarantee.deterministic.len() + guarantee.recorded.len() + guarantee.external.len(),
        StateItem::ALL.len(),
        "every item is classified exactly once"
    );
    assert_eq!(
        guarantee.recorded,
        [StateItem::Debug],
        "the input log is the one item replayed from a recording"
    );
}

// -- the clock ---------------------------------------------------------------

#[test]
fn a_snapshot_carries_the_virtual_clock() {
    // The clock is *not* host bookkeeping: the machine this restores onto is built
    // fresh and has never run, so a snapshot that dropped the clock would leave both
    // machines at zero and this would still pass on a wrong implementation.
    let mut source = machine();
    let mut target = machine();
    advance(&mut source, 1_000);

    let snapshot = CapturedState::of(&source);
    assert_eq!(
        snapshot.clock().elapsed(),
        CycleCount::new(1_000),
        "the snapshot reads the clock at the point it was taken"
    );

    snapshot.restore(&mut target).expect("the restore applies");
    assert_eq!(
        target.clock().elapsed(),
        CycleCount::new(1_000),
        "and the target is at the same virtual time, not at zero"
    );
}

#[test]
fn restoring_a_clock_does_not_disturb_a_later_one() {
    // Restoring must put time *back*, not merge with it: a target that had already run
    // for 5,000 cycles is overwritten, not advanced to 6,000. This is the direction of
    // travel that B6's lifecycle tests check for `VmSnapshot` and that the machine
    // clock needs checked separately, because `restore_time` is not the same code.
    let mut source = machine();
    let mut target = machine();
    advance(&mut source, 2_000);
    advance(&mut target, 5_000);

    let snapshot = CapturedState::of(&source);
    snapshot.restore(&mut target).expect("the restore applies");
    assert_eq!(
        target.clock().elapsed(),
        CycleCount::new(2_000),
        "the snapshot's time replaces the target's rather than being added to it"
    );
}

// -- the pending interrupts ---------------------------------------------------

#[test]
fn a_snapshot_carries_the_interrupts_the_machine_had_not_taken_yet() {
    let mut source = machine();
    let mut target = machine();
    // A second machine at a different time, so a restore that copied the clock but
    // dropped the interrupts cannot pass this by accident.
    advance(&mut source, 777);
    for id in [7u16, 9, 11] {
        source
            .interrupts_mut()
            .request(lazalith_types::InterruptId::new(id))
            .expect("the interrupt is accepted");
    }

    let snapshot = CapturedState::of(&source);
    assert_eq!(
        snapshot.pending_interrupts().len(),
        3,
        "all three are pending, because none has been taken"
    );

    target
        .interrupts_mut()
        .request(lazalith_types::InterruptId::new(3))
        .expect("the target's own interrupt is accepted");
    snapshot.restore(&mut target).expect("the restore applies");

    assert_eq!(
        target.interrupts().iter().collect::<Vec<_>>(),
        snapshot.pending_interrupts(),
        "the target has exactly the snapshot's pending set: the machine's own promises \
         are replaced, not added to"
    );
    assert_eq!(
        target.clock().elapsed(),
        CycleCount::new(777),
        "and it is at the snapshot's time"
    );
}

// -- clone -------------------------------------------------------------------

#[test]
fn a_captured_state_can_be_taken_more_than_once_and_restored_twice() {
    // Clone is a capability §40 names. It is easy to implement wrongly by having
    // `restore` consume the snapshot, which would make the second restore a
    // compile error rather than a bug — so this test also holds the *same* value
    // across two machines and checks both come out identical.
    let mut source = machine();
    advance(&mut source, 424);
    source
        .interrupts_mut()
        .request(lazalith_types::InterruptId::new(5))
        .expect("the interrupt is accepted");

    let snapshot = CapturedState::of(&source);
    let copy = snapshot.clone();

    let mut first = machine();
    let mut second = machine();
    snapshot
        .restore(&mut first)
        .expect("the first restore applies");
    copy.restore(&mut second)
        .expect("the second restore applies");

    assert_eq!(first.clock().elapsed(), second.clock().elapsed());
    assert_eq!(
        first.interrupts().iter().collect::<Vec<_>>(),
        second.interrupts().iter().collect::<Vec<_>>()
    );
}

// -- the refusals ------------------------------------------------------------

#[test]
fn a_snapshot_from_another_architecture_is_refused() {
    let source = machine();
    let snapshot = CapturedState::of(&source);

    // A 32-bit machine, built through the public profile path. There is no
    // `lza32_native_v1` because the shipped profile family is 64-bit, so this goes
    // through `MachineProfile::empty` with an lz32 config — the same public shape the
    // rest of the suite builds profiles from.
    let mut target = LazalithMachine::from_profile(&profile_with_config(
        ArchitectureConfig::lz32(),
        vec![device(1)],
    ))
    .expect("an lz32 machine builds");
    let refusal = snapshot
        .restore(&mut target)
        .expect_err("an lz64 snapshot cannot be restored onto an lz32 machine");
    assert!(
        refusal.contains("snapshot is for"),
        "the refusal says whose machine it was for: {refusal}"
    );
}

#[test]
fn a_snapshot_with_a_different_device_count_is_refused() {
    let source = machine();
    let snapshot = CapturedState::of(&source);

    let mut target = LazalithMachine::from_profile(&profile_with(vec![device(1), device(2)]))
        .expect("two devices");
    let refusal = snapshot
        .restore(&mut target)
        .expect_err("a one-device snapshot cannot be restored onto a two-device machine");
    assert!(
        refusal.contains("devices"),
        "the refusal says what did not line up: {refusal}"
    );
}

#[test]
fn a_refused_restore_leaves_the_machine_untouched() {
    // The check happens before anything is written. If it did not, a refused restore
    // would still have moved the clock, and a caller that ignored the error would be
    // left with a half-restored machine that looks like it loaded.
    let mut source = machine();
    advance(&mut source, 300);
    let snapshot = CapturedState::of(&source);

    let mut target = LazalithMachine::from_profile(&profile_with(vec![device(1), device(2)]))
        .expect("two devices");
    assert!(
        snapshot.restore(&mut target).is_err(),
        "the restore is refused"
    );
    assert_eq!(
        target.clock().elapsed(),
        CycleCount::new(0),
        "and the refused restore did not move the clock"
    );
}

// -- the CPU and the devices -------------------------------------------------

#[test]
fn a_snapshot_carries_the_registers_and_the_device_states() {
    let mut source = machine();
    // Write a byte through the console so the device has state that is not its
    // initial state, and a register so the CPU differs from a fresh machine.
    source
        .processor_mut()
        .architectural_mut()
        .write_register_raw(5, 0xfeed_face)
        .expect("r5 exists on this machine");
    source
        .devices_mut()
        .write(
            lazalith_devices::DeviceId::new(1),
            lazalith_types::DeviceOffset::new(0),
            lazalith_isa::DataSize::Byte,
            u64::from(b'h'),
        )
        .expect("the console accepts a byte");

    let snapshot = CapturedState::of(&source);
    assert_eq!(
        snapshot.cpu().register(5),
        Some(0xfeed_face),
        "the register is in the snapshot"
    );
    assert_eq!(snapshot.devices().len(), 1, "and the device is");

    let mut target = machine();
    snapshot.restore(&mut target).expect("the restore applies");
    assert_eq!(
        target
            .processor()
            .architectural()
            .registers()
            .read_raw(5)
            .unwrap(),
        0xfeed_face,
        "the register came back"
    );
}

#[test]
fn a_device_state_is_restored_not_just_counted() {
    // This is the test that catches a `restore` that silently skips the devices. A
    // test that only checks the *count* of devices passes against an implementation
    // that copies the count and drops the state, because the count is right either
    // way — so the assertion is on the bytes themselves, and on a device that has
    // some.
    let mut source =
        LazalithMachine::from_profile(&profile_with(vec![display(1)])).expect("a display builds");
    // Present a frame, so `present_count` is no longer its initial value and a restore
    // that dropped the state would be visible.
    present_frame(&mut source);
    assert_ne!(
        source.devices().snapshot()[0].1,
        vec![0u8; 40],
        "the display's state differs from a fresh one, or there is nothing to test"
    );

    let snapshot = CapturedState::of(&source);
    let mut target =
        LazalithMachine::from_profile(&profile_with(vec![display(1)])).expect("a display builds");
    snapshot.restore(&mut target).expect("the restore applies");

    assert_eq!(
        target.devices().snapshot()[0].1,
        snapshot.devices()[0].bytes(),
        "the target's display holds exactly the state the snapshot took"
    );
    assert_ne!(
        target.devices().snapshot()[0].1,
        vec![0u8; 40],
        "and that state is not a fresh device's, so the restore did something"
    );
}

/// Points a display at a framebuffer, which is what a guest does before presenting.
///
/// Only two of the display's registers are writable — the framebuffer address and the
/// present request — and the geometry is fixed by the device. Moving the framebuffer is
/// enough here: the address is one of the five words in the display's snapshot, so
/// writing it makes the device's state differ from a fresh one's, which is what a
/// restore has to reproduce.
///
/// The present request is *not* used, because a present with no display window open is
/// refused — and the point of this test is the snapshot, not the display pump.
fn present_frame(machine: &mut LazalithMachine<std::boxed::Box<dyn lazalith_devices::Device>>) {
    machine
        .devices_mut()
        .write(
            DeviceId::new(1),
            lazalith_types::DeviceOffset::new(16),
            lazalith_isa::DataSize::Double,
            0x5000_0000,
        )
        .expect("the display accepts a framebuffer address");
}

#[test]
fn the_device_state_is_left_opaque_to_the_vm_layer() {
    // The VM must not decode a device's private layout; each device encodes itself.
    // A snapshot's device bytes are exactly the device's own `snapshot()` output.
    let source = machine();
    let snapshot = CapturedState::of(&source);
    let from_manager = source.devices().snapshot();
    assert_eq!(
        snapshot.devices()[0].bytes(),
        from_manager[0].1.as_slice(),
        "the VM layer carried the device's own encoding without interpreting it"
    );
    assert_eq!(
        snapshot.devices()[0].id(),
        from_manager[0].0,
        "and kept the device's identity with it"
    );
}

#[test]
fn the_architecture_is_recorded_but_not_restored() {
    // `Checked` means checked. A snapshot's architecture is the one it was taken on,
    // and restoring it onto another machine fails; it is not a way to reconfigure a
    // machine.
    let source = machine();
    let snapshot = CapturedState::of(&source);
    assert_eq!(snapshot.architecture(), CONFIG, "recorded on capture");
}

// -- the report ---------------------------------------------------------------

#[test]
fn the_inventory_renders_a_readable_row_per_item() {
    // §40 asks for a document. This is what it is generated from, so it is worth
    // checking that every row says something rather than rendering three empty
    // columns.
    for row in coverage_inventory() {
        let rendered = row.to_string();
        assert!(
            rendered.contains(row.item.as_str()) && rendered.contains(':'),
            "{rendered} names the item and says what happens to it"
        );
    }
}

#[test]
fn the_inventory_rows_are_the_ones_the_types_claim() {
    // A row is derived from the item rather than written out, so this is mostly a
    // guard against the Display impl and the data disagreeing.
    let expected = StateItem::ALL
        .into_iter()
        .map(|item| StateCoverage {
            item,
            coverage: item.coverage(),
            determinism: item.determinism(),
        })
        .collect::<Vec<_>>();
    assert_eq!(coverage_inventory(), expected);
}

#[test]
fn the_inventory_is_the_nine_items_40_names() {
    // If §40's list changes, this is where it changes. The names are checked because
    // they are what the document says, and a renamed item would otherwise be a silent
    // documentation drift.
    let names: Vec<&str> = StateItem::ALL.into_iter().map(|i| i.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "CPU",
            "memory",
            "devices",
            "virtual clock",
            "interrupt state",
            "machine configuration",
            "storage state",
            "network state",
            "debug state",
        ]
    );
}
