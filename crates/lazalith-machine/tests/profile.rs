//! B4: machine profiles, and the heterogeneous device set they need.
//!
//! # What these tests are for
//!
//! `binstruction.md` §27 asks for versioned machine profiles describing a
//! machine's architecture, CPU, RAM, firmware, boot behaviour, interrupt model,
//! timer, device inventory and MMIO map. A profile cannot describe a device
//! *inventory* while `DeviceManager` holds one concrete device type, so this file
//! checks both halves:
//!
//! - one machine really can hold a console, a timer, an input device and a
//!   display at once, each mapped, each window answering for its own device — the
//!   thing that was impossible before, and that everything in §27's device list
//!   depends on;
//! - a profile is accepted, builds a machine, and that machine agrees with it;
//! - a profile describing something impossible is *refused*, with a reason that
//!   names which part of it to change.
//!
//! # What these tests protect
//!
//! The Phase-I path. B4 is additive: it adds a way to hold *more* kinds of device,
//! and moves where the machine's geometry constants are **defined** without
//! changing a value. Two tests in `lazalith-boot/tests/profile_layout.rs` hold that,
//! because only the boot crate can see both the boot and the OS re-exports; the
//! rest of the Phase-I suite — the boot path, LazOS, the debugger, the whole
//! toolchain — is unchanged and still green, which is the stronger evidence.

use lazalith_devices::{Device, DeviceId};
use lazalith_isa::{DataSize, ISA_VERSION, Instruction, Opcode, Operand, encode};
use lazalith_machine::{
    BlockStorage, Compatibility, DeviceClass, DeviceProfile, LZA64_LAYOUT, MachineProfile,
    ProfileError, ProfileFamily, ProfileName, ProfiledMachine, RegionProfile,
};
use lazalith_memory::{AddressSpace, RegionKind, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
};

const CODE: u64 = 0x0050_0000;
const STACK: u64 = 0x0058_0000;
const STACK_TOP: u64 = STACK + 0x1000;

fn native_name(config: ArchitectureConfig, version: u16) -> ProfileName {
    ProfileName::new(config, ProfileFamily::Native, version)
}

fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).unwrap())
}

fn region(start: u64, length: u64, permissions: RegionPermissions) -> RegionProfile {
    RegionProfile {
        start: PhysicalAddress::new(start),
        length,
        kind: RegionKind::Ram,
        permissions,
    }
}

fn bare(config: ArchitectureConfig) -> MachineProfile {
    MachineProfile::empty(native_name(config, 1), LZA64_LAYOUT)
}

// -- Phase-I safety ---------------------------------------------------------

/// A Phase-I machine still cannot hold a device at all.
///
/// `NoDevice` is an uninhabited enum, so `DeviceManager<NoDevice>` is not an empty
/// erased list — it is a machine that could never hold anything. That is a
/// stronger statement than B4's `Box<dyn Device>` makes, and the two staying
/// distinct is why the Phase-I path is untouched rather than merely working.
#[test]
fn a_phase_i_machine_cannot_hold_a_device_at_all() {
    fn accepts<D: lazalith_devices::Device>(_: &lazalith_memory::Bus<D>) {}
    let space = AddressSpace::new(ArchitectureConfig::lz64());
    accepts(&lazalith_memory::Bus::<lazalith_devices::NoDevice>::new(
        space,
    ));

    // And an erased set *can* hold several kinds, which is the difference B4 makes.
    let space = AddressSpace::new(ArchitectureConfig::lz64());
    let erased: lazalith_devices::DeviceManager<Box<dyn lazalith_devices::Device>> =
        lazalith_devices::DeviceManager::new();
    accepts(&lazalith_memory::Bus::with_devices(space, erased));
}

// -- naming and versioning --------------------------------------------------

/// A profile's name is architecture, family and version, and says all three.
///
/// §27 requires versioning "to preserve guest compatibility", which is only a real
/// property if the version is part of the identity rather than a field beside it.
#[test]
fn a_profile_name_carries_its_architecture_family_and_version() {
    let cases = [
        (ProfileFamily::Virt, "lza64-virt-v1"),
        (ProfileFamily::Native, "lza64-native-v1"),
        (ProfileFamily::At, "lza64-at-v1"),
    ];
    for (family, expected) in cases {
        let name = ProfileName::new(ArchitectureConfig::lz64(), family, 1);
        assert_eq!(name.to_string(), expected);
        assert_eq!(name.version(), 1);
        assert_eq!(name.family(), family);
    }
    // The narrow architecture gets its own identity rather than sharing LZ64's,
    // which is the whole reason LZA32 and LZA64 are names and not one name.
    assert_eq!(
        native_name(ArchitectureConfig::lz32(), 1).to_string(),
        "lza32-native-v1"
    );
    // And a later version is a different name, not the same name later.
    assert_ne!(
        native_name(ArchitectureConfig::lz64(), 2).to_string(),
        native_name(ArchitectureConfig::lz64(), 1).to_string()
    );
}

/// A profile with no version is refused.
///
/// This is what makes "versioned" mean something: a profile that could be
/// unversioned could be changed incompatibly and a guest would have no way to tell
/// which machine it was on.
#[test]
fn an_unversioned_profile_is_refused() {
    let profile = bare(ArchitectureConfig::lz64()).with_version(0);
    assert!(matches!(profile.validate(), Err(ProfileError::Unversioned)));
    assert!(matches!(
        ProfiledMachine::from_profile(&profile),
        Err(ProfileError::Unversioned)
    ));
}

/// A profile naming an ISA this build does not implement is refused.
///
/// The compatibility claim in §27 is only checkable if something checks it.
#[test]
fn a_profile_for_another_isa_version_is_refused() {
    let profile = MachineProfile::lza64_native_v1().with_isa_version(ISA_VERSION + 1);
    match profile.validate() {
        Err(ProfileError::IsaVersionMismatch { profile, build }) => {
            assert_eq!(profile, ISA_VERSION + 1);
            assert_eq!(build, ISA_VERSION);
        }
        other => panic!("a profile for another ISA version was not refused: {other:?}"),
    }
}

// -- the first real profile -------------------------------------------------

/// `lza64-native-v1` is valid, and is named what it says.
#[test]
fn lza64_native_v1_is_a_valid_profile() {
    let profile = MachineProfile::lza64_native_v1();
    profile.validate().expect("lza64-native-v1 should be valid");
    assert_eq!(profile.name().to_string(), "lza64-native-v1");
    assert_eq!(profile.isa_version(), ISA_VERSION);
    assert_eq!(profile.compatibility(), Compatibility::Native);
    assert_eq!(profile.reset_vector().as_u64(), LZA64_LAYOUT.boot_rom_start);
    assert_eq!(
        profile.kernel_stack_pointer().as_u64(),
        LZA64_LAYOUT.kernel_initial_sp
    );

    // A boot ROM, executable and not writable, and the machine's RAM.
    assert_eq!(profile.regions().len(), 2);
    let rom = profile.regions()[0];
    assert_eq!(rom.start, PhysicalAddress::new(LZA64_LAYOUT.boot_rom_start));
    assert_eq!(rom.length, LZA64_LAYOUT.boot_rom_length);
    assert!(rom.permissions.execute, "the boot ROM must be executable");
    assert!(!rom.permissions.write, "the boot ROM must not be writable");
    let machine_ram = profile.regions()[1];
    assert_eq!(
        machine_ram.start,
        PhysicalAddress::new(LZA64_LAYOUT.physical_ram_start)
    );
    assert_eq!(machine_ram.length, LZA64_LAYOUT.physical_ram_length);
}

/// The native profile is minimal on purpose.
///
/// §53 puts machine profiles before the target-side OS, and a freestanding kernel
/// (B25, B26) is built against a machine with nothing on it but a CPU and memory.
/// A native profile whose default shape included a display would mean every
/// kernel target had to opt out of a device it never asked for, and opting out is
/// how a guest quietly acquires a dependency on a device that is not always there.
#[test]
fn the_native_profile_carries_only_what_a_kernel_needs() {
    let profile = MachineProfile::lza64_native_v1();
    assert_eq!(profile.devices().len(), 2);
    assert!(profile.device_of(DeviceClass::Console).is_some());
    assert!(profile.device_of(DeviceClass::Timer).is_some());
    for absent in [
        DeviceClass::Display,
        DeviceClass::Input,
        DeviceClass::Block,
        DeviceClass::Network,
        DeviceClass::Audio,
        DeviceClass::Serial,
    ] {
        assert!(
            profile.device_of(absent).is_none(),
            "lza64-native-v1 should not carry a {absent}"
        );
    }
    assert_eq!(profile.timer_period(), Some(CycleCount::new(1_000)));
}

/// A profile builds a machine, and the machine agrees with the profile.
///
/// The second half is the half that is easy to leave out. A profile can be
/// accepted and a machine built from it and the two still disagree — a device
/// mapped where the profile did not say, a reset vector that did not take — and
/// the machine boots and then misbehaves. So this is a round trip, not a
/// construction.
#[test]
fn a_machine_built_from_a_profile_matches_it() {
    let profile = MachineProfile::lza64_native_v1();
    let mut machine = ProfiledMachine::from_profile(&profile).expect("the profile builds");
    machine
        .matches_profile(&profile)
        .expect("the machine is the one the profile describes");

    // And it is a machine rather than a description: past `Created`, resettable,
    // and it knows its own architecture.
    assert!(!machine.is_halted());
    machine.reset();
    assert_eq!(
        machine.processor().architectural().pc(),
        profile.reset_vector()
    );
    assert_eq!(
        machine.processor().architectural().sp(),
        profile.kernel_stack_pointer()
    );
    assert_eq!(machine.config(), ArchitectureConfig::lz64());
}

/// The round trip works at both widths the architecture has.
#[test]
fn a_profile_works_at_both_widths() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let profile = bare(config).with_region(region(
            LZA64_LAYOUT.physical_ram_start,
            LZA64_LAYOUT.physical_ram_length,
            RegionPermissions::new(true, true, false, true),
        ));
        profile
            .validate()
            .unwrap_or_else(|e| panic!("{config:?} should be valid: {e}"));
        let machine = ProfiledMachine::from_profile(&profile)
            .unwrap_or_else(|e| panic!("{config:?} should build: {e}"));
        machine
            .matches_profile(&profile)
            .unwrap_or_else(|e| panic!("{config:?} should agree: {e}"));
        assert_eq!(machine.config(), config);
    }
}

// -- the heterogeneous device set -------------------------------------------

/// Four devices of four kinds, in one machine, each window answering for itself.
///
/// This is what B4's first half exists for, and what a profile's device inventory
/// is impossible without: `DeviceManager` holds one concrete type, so before B4 a
/// machine could have a console *or* a timer *or* an input device.
///
/// The assertions are about *routing*, not about each device working on its own. A
/// bus that sent every window to the first device would pass a test that only
/// checked that four devices exist, so each window is read and checked against
/// what that specific device — and only that device — would answer.
#[test]
fn one_machine_holds_four_kinds_of_device() {
    let profile = MachineProfile::lza64_native_v1()
        .with_device(DeviceProfile::new(
            DeviceId::new(3),
            DeviceClass::Input,
            PhysicalAddress::new(0x4000_2000),
            RegionPermissions::new(true, true, false, true),
        ))
        .with_device(DeviceProfile::new(
            DeviceId::new(4),
            DeviceClass::Display,
            PhysicalAddress::new(0x4003_0000),
            RegionPermissions::new(true, true, false, true),
        ));
    let machine = ProfiledMachine::from_profile(&profile).expect("the profile builds");
    assert_eq!(machine.devices().len(), 4, "all four in one machine");

    let console = profile.address_of(DeviceClass::Console).expect("a console");
    let timer = profile.address_of(DeviceClass::Timer).expect("a timer");
    let input = profile
        .address_of(DeviceClass::Input)
        .expect("an input device");
    let display = profile.address_of(DeviceClass::Display).expect("a display");

    // Four distinct windows, which is the property a monomorphic manager forbade.
    let windows = [console, timer, input, display];
    for (index, first) in windows.iter().enumerate() {
        for second in &windows[index + 1..] {
            assert_ne!(first, second, "two devices share a window");
        }
    }

    // The timer window is the discriminator. It is eight bytes long, so offset 8 is
    // past its end, and a bus that fell through to the next mapping would answer
    // with the input device's first register rather than refusing.
    assert_eq!(peek(&machine, timer, 0), vec![0; 8], "a timer at zero");
    assert!(
        peek_refused(&machine, timer, 8),
        "an offset past the timer window must not fall through to another device"
    );
    assert_eq!(
        peek(&machine, input, 8),
        0u64.to_le_bytes().to_vec(),
        "the input device has nothing queued"
    );
    // The console answers differently again: it is write-only, and refusing to be
    // peeked is a different answer from anything the other three could give. If
    // this window were reaching some other device, the peek would succeed.
    assert!(
        peek_refused(&machine, console, 0),
        "the console is write-only, so its window must refuse a peek"
    );
    // And the display window is readable and reports its own registers.
    assert_eq!(
        peek(&machine, display, 0),
        vec![0; 8],
        "a display with no window open"
    );
}

/// One guest, reading three different device windows in one program.
///
/// The host-side test above checks routing; this checks the thing a guest actually
/// experiences, which is the claim that matters: a program on this machine can
/// name three devices and get three different answers, where before it could name
/// one. The program is assembled with the real encoder, so it cannot pass on a
/// hand-written encoding the ISA never defined.
///
/// It also reads the *timer*, which the host-side test deliberately does not. A
/// host peek is not a guest read: `TimerDevice::peek` validates the register and
/// returns without writing the buffer, so a peek of a timer window hands back the
/// caller's own bytes and can never show the value. The guest's `LDZ` goes through
/// `Device::read`, which does return the cycle count. That difference is a
/// Phase-I device defect recorded in `docs/project-state.md`; the reason this test
/// uses the guest path is simply that the guest path is the one that works.
#[test]
fn one_guest_reads_three_device_windows() {
    let config = ArchitectureConfig::lz64();
    let profile = MachineProfile::lza64_native_v1()
        .with_region(region(
            CODE,
            0x1000,
            RegionPermissions::new(true, true, true, true),
        ))
        .with_region(region(
            STACK,
            0x1000,
            RegionPermissions::new(true, true, false, true),
        ))
        .with_device(DeviceProfile::new(
            DeviceId::new(3),
            DeviceClass::Input,
            PhysicalAddress::new(0x4000_2000),
            RegionPermissions::new(true, true, false, true),
        ))
        .with_device(DeviceProfile::new(
            DeviceId::new(4),
            DeviceClass::Display,
            PhysicalAddress::new(0x4003_0000),
            RegionPermissions::new(true, true, false, true),
        ));

    let timer = profile.address_of(DeviceClass::Timer).expect("a timer");
    let input = profile
        .address_of(DeviceClass::Input)
        .expect("an input device");
    let display = profile.address_of(DeviceClass::Display).expect("a display");

    // li r1, timer      ldz r2, [r1 + 0]      the timer's cycle count
    // li r3, input      ldz r4, [r3 + 8]      the input device's pending count
    // li r5, display    ldz r6, [r5 + 0]      the display's width register
    // halt
    let mut code = Vec::new();
    let emit = |opcode: Opcode, operands: &[Operand], code: &mut Vec<u8>| {
        let instruction = Instruction::new(config, opcode, operands).unwrap();
        code.extend_from_slice(&encode(config, &instruction).unwrap());
    };
    emit(
        Opcode::Li,
        &[r(1), Operand::Immediate(timer.as_u64() as i32)],
        &mut code,
    );
    emit(
        Opcode::Ldz,
        &[r(2), memory(r(1), 0), Operand::DataSize(DataSize::Double)],
        &mut code,
    );
    emit(
        Opcode::Li,
        &[r(3), Operand::Immediate(input.as_u64() as i32)],
        &mut code,
    );
    emit(
        Opcode::Ldz,
        &[r(4), memory(r(3), 8), Operand::DataSize(DataSize::Double)],
        &mut code,
    );
    emit(
        Opcode::Li,
        &[r(5), Operand::Immediate(display.as_u64() as i32)],
        &mut code,
    );
    emit(
        Opcode::Ldz,
        &[r(6), memory(r(5), 0), Operand::DataSize(DataSize::Double)],
        &mut code,
    );
    emit(Opcode::Halt, &[], &mut code);

    // The guest starts at its own code above the profile's RAM, not at its
    // reset vector, and
    // runs as a supervisor program: a device window is supervisor-only in the
    // native profile, and a User program reading one is a different question.
    let mut setup = profile
        .machine_setup()
        .expect("the profile describes a machine");
    setup.pc = InstructionAddress::new(CODE);
    setup.sp = lazalith_types::VirtualAddress::new(STACK_TOP);
    let mut machine = ProfiledMachine::new(setup).expect("the machine builds");
    machine
        .load_bytes(PhysicalAddress::new(CODE), &code)
        .expect("the program is loaded");
    for device in profile.devices() {
        machine
            .map_device(device.id, device.address, device.permissions)
            .expect("the device window maps");
    }
    machine.reset();

    // Virtual time, so the timer has something to report.
    machine
        .advance_clock(CycleCount::new(1_234))
        .expect("the clock advances");

    let run = machine.run(64).expect("the program runs");
    assert!(run.halted_at.is_some(), "the program did not halt: {run:?}");
    assert!(run.trap.is_none(), "the program trapped: {run:?}");

    let register = |index: u8| {
        machine
            .processor()
            .architectural()
            .registers()
            .read(RegisterIndex::try_from(index).unwrap())
    };
    assert_eq!(register(2), 1_234, "the timer window");
    assert_eq!(register(4), 0, "the input window, with nothing queued");
    assert_eq!(register(6), 0, "the display window, with no window open");
    assert_ne!(
        register(2),
        register(6),
        "two different windows must not read the same register"
    );

    // No `matches_profile` here, and the omission is the point: this machine's
    // program counter is the code it was loaded at, not the profile's reset
    // vector, so it is deliberately *not* the machine the profile describes any
    // more. The round trip is `a_machine_built_from_a_profile_matches_it`'s job,
    // and this test would have failed its check by doing its own work.
}

fn memory(base: Operand, displacement: i32) -> Operand {
    match base {
        Operand::Register(base) => Operand::Memory { base, displacement },
        other => panic!("a memory operand needs a register, got {other:?}"),
    }
}

// -- refusals ---------------------------------------------------------------

/// Two devices with one id are refused before a machine exists.
///
/// A profile naming the same id twice describes a machine where one address routes
/// to two devices. `Bus::map_device` would refuse the second mapping, so building
/// first would work — but it would leave a half-built machine and an error about
/// mapping rather than about the profile.
#[test]
fn two_devices_with_one_id_are_refused() {
    let profile = MachineProfile::lza64_native_v1().with_device(DeviceProfile::new(
        DeviceId::new(1),
        DeviceClass::Timer,
        PhysicalAddress::new(0x4000_9000),
        RegionPermissions::new(true, true, false, true),
    ));
    assert!(matches!(
        profile.validate(),
        Err(ProfileError::DuplicateDeviceId { .. })
    ));
    assert!(matches!(
        ProfiledMachine::from_profile(&profile),
        Err(ProfileError::DuplicateDeviceId { .. })
    ));
}

/// Two devices whose windows overlap are refused.
#[test]
fn two_overlapping_device_windows_are_refused() {
    // The native profile's timer is at 0x4000_1000 and is eight bytes long, so a
    // console placed at 0x4000_1004 sits inside it.
    let profile = MachineProfile::lza64_native_v1().with_device(
        DeviceProfile::new(
            DeviceId::new(9),
            DeviceClass::Console,
            PhysicalAddress::new(0x4000_1004),
            RegionPermissions::new(true, true, false, true),
        )
        .with_console_capacity(16),
    );
    assert!(matches!(
        profile.validate(),
        Err(ProfileError::DeviceOverlap { .. })
    ));
}

/// A device window a guest could execute is refused.
///
/// `Bus::map_device` refuses an executable window, so a profile that asked for one
/// describes a machine that cannot be built. The refusal belongs to the profile, so
/// the profile is what is reported as wrong.
#[test]
fn an_executable_device_window_is_refused() {
    let profile = MachineProfile::lza64_native_v1().with_device(DeviceProfile::new(
        DeviceId::new(4),
        DeviceClass::Timer,
        PhysicalAddress::new(0x4000_5000),
        RegionPermissions::new(true, true, true, true),
    ));
    assert!(matches!(
        profile.validate(),
        Err(ProfileError::ExecutableDevice { .. })
    ));
}

/// A device class this build cannot construct is refused, not skipped.
///
/// Skipping would build a machine missing something the profile promised while the
/// profile still validated. A refusal says the class is not available here, which
/// is a fact a caller can act on.
#[test]
fn a_device_class_this_build_cannot_construct_is_refused() {
    for class in [
        DeviceClass::Network,
        DeviceClass::Audio,
        DeviceClass::Serial,
        DeviceClass::Other,
    ] {
        let profile = MachineProfile::lza64_native_v1().with_device(DeviceProfile::new(
            DeviceId::new(7),
            class,
            PhysicalAddress::new(0x4000_7000),
            RegionPermissions::new(true, true, false, true),
        ));
        assert!(
            matches!(
                profile.validate(),
                Err(ProfileError::UnconstructibleDevice { class: c, .. }) if c == class
            ),
            "a {class} device was not refused"
        );
    }
}

/// Overlapping regions are refused; abutting ones are not.
#[test]
fn overlapping_regions_are_refused_and_abutting_ones_are_not() {
    let abutting = bare(ArchitectureConfig::lz64())
        .with_region(region(
            0x1000,
            0x1000,
            RegionPermissions::new(true, true, false, true),
        ))
        .with_region(region(
            0x2000,
            0x1000,
            RegionPermissions::new(true, true, false, true),
        ));
    abutting
        .validate()
        .expect("regions that touch but do not overlap are fine");

    let overlapping = bare(ArchitectureConfig::lz64())
        .with_region(region(
            0x1000,
            0x1000,
            RegionPermissions::new(true, true, false, true),
        ))
        .with_region(region(
            0x1800,
            0x1000,
            RegionPermissions::new(true, true, false, true),
        ));
    assert!(matches!(
        overlapping.validate(),
        Err(ProfileError::RegionOverlap { .. })
    ));

    let empty = bare(ArchitectureConfig::lz64()).with_region(region(
        0x1000,
        0,
        RegionPermissions::new(true, true, false, true),
    ));
    assert!(matches!(
        empty.validate(),
        Err(ProfileError::EmptyRegion { .. })
    ));
}

/// The AT compatibility machine can be named, and building one is refused.
///
/// `lza64-at-v1` is in §27 and §28 requires VGA, which does not exist. The name has
/// to be writable down before the device set exists, and asking for a machine this
/// build cannot produce has to be a refusal rather than an `lza64-at-v1` that
/// quietly has no VGA in it.
#[test]
fn the_at_profile_can_be_named_and_is_refused_here() {
    let at = MachineProfile::empty(
        ProfileName::new(ArchitectureConfig::lz64(), ProfileFamily::At, 1),
        LZA64_LAYOUT,
    )
    .with_compatibility(Compatibility::At);
    assert_eq!(at.name().to_string(), "lza64-at-v1");
    assert!(matches!(
        at.validate(),
        Err(ProfileError::UnimplementedCompatibility { .. })
    ));

    // A family that disagrees with its own compatibility behaviour is refused
    // first: a *native* profile claiming AT hardware is wrong about something more
    // basic than which devices it has.
    let mismatched = bare(ArchitectureConfig::lz64()).with_compatibility(Compatibility::At);
    assert!(matches!(
        mismatched.validate(),
        Err(ProfileError::CompatibilityMismatch { .. })
    ));
}

/// The device inventory is a list, because a machine may have two displays.
///
/// A map keyed by class would have to decide which display is "the" display, and
/// that decision belongs to the guest.
#[test]
fn a_profile_can_describe_two_devices_of_one_class() {
    let profile = MachineProfile::lza64_native_v1()
        .with_device(DeviceProfile::new(
            DeviceId::new(5),
            DeviceClass::Display,
            PhysicalAddress::new(0x4003_0000),
            RegionPermissions::new(true, true, false, true),
        ))
        .with_device(DeviceProfile::new(
            DeviceId::new(6),
            DeviceClass::Display,
            PhysicalAddress::new(0x4004_0000),
            RegionPermissions::new(true, true, false, true),
        ));
    profile
        .validate()
        .expect("two displays is a machine, not a contradiction");
    let displays: Vec<DeviceId> = profile
        .devices_of(DeviceClass::Display)
        .map(|device| device.id)
        .collect();
    assert_eq!(displays, vec![DeviceId::new(5), DeviceId::new(6)]);

    let machine = ProfiledMachine::from_profile(&profile).expect("the profile builds");
    assert_eq!(machine.devices().len(), 4);
    machine.matches_profile(&profile).expect("and it agrees");
}

// -- helpers ----------------------------------------------------------------

/// Reads a device window through the machine, the way a guest would.
///
/// `peek` rather than a data read because a peek does not mutate, so a test can
/// read several windows in a row and be reading the same machine throughout.
fn peek(machine: &ProfiledMachine, window: PhysicalAddress, offset: u64) -> Vec<u8> {
    let mut out = [0u8; 8];
    machine
        .peek_memory(PhysicalAddress::new(window.as_u64() + offset), &mut out)
        .unwrap_or_else(|e| panic!("reading {window:?}+{offset}: {e}"));
    out.to_vec()
}

/// The same, for a window that is expected to refuse.
fn peek_refused(machine: &ProfiledMachine, window: PhysicalAddress, offset: u64) -> bool {
    let mut out = [0u8; 8];
    machine
        .peek_memory(PhysicalAddress::new(window.as_u64() + offset), &mut out)
        .is_err()
}

// -- B5: a block device in the inventory ---------------------------------------

/// A profile can name a block device, and the machine it builds answers on it.
///
/// This is the B4 refusal turned into a construction. `DeviceClass::Block` was
/// `is_constructible() == false` through B4 and a profile naming one was refused; now
/// the profile has to say what is behind the disk, and the machine has to be the
/// machine the profile described.
#[test]
fn a_profile_can_build_a_block_device() {
    let profile = MachineProfile::lza64_native_v1().with_device(
        DeviceProfile::new(
            DeviceId::new(7),
            DeviceClass::Block,
            PhysicalAddress::new(0x4000_7000),
            RegionPermissions::new(true, true, false, true),
        )
        .with_block_storage(BlockStorage::Memory {
            bytes: 8 * lazalith_devices::SECTOR_BYTES,
        }),
    );
    assert!(
        profile.validate().is_ok(),
        "a block device with a disk is buildable"
    );

    let machine = ProfiledMachine::from_profile(&profile).expect("the profile builds a machine");
    machine
        .matches_profile(&profile)
        .expect("the machine is the machine the profile described, window and all");
    let device = machine
        .devices()
        .device(DeviceId::new(7))
        .expect("the block device is in the machine");
    assert_eq!(
        device.address_len(),
        lazalith_devices::BLOCK_REGISTER_BYTES,
        "the mapped window is the size the device reports, so the bus and the device \
         cannot disagree about how big it is"
    );
    let mut machine = ProfiledMachine::from_profile(&profile).expect("rebuilt");
    assert_eq!(
        machine
            .devices_mut()
            .device_mut(DeviceId::new(7))
            .expect("the block device is in the machine")
            .read(lazalith_devices::BLOCK_REGISTER_CAPACITY, DataSize::Double)
            .expect("a capacity read"),
        8,
        "the guest sees the eight sectors the profile promised"
    );
}

/// A block device the profile gave no disk to is refused.
#[test]
fn a_block_device_with_no_storage_is_refused() {
    let profile = MachineProfile::lza64_native_v1().with_device(DeviceProfile::new(
        DeviceId::new(7),
        DeviceClass::Block,
        PhysicalAddress::new(0x4000_7000),
        RegionPermissions::new(true, true, false, true),
    ));
    assert!(
        matches!(
            profile.validate(),
            Err(ProfileError::BlockStorage { id, .. }) if id == DeviceId::new(7)
        ),
        "a block device with nothing behind it is a promise the machine cannot keep, so \
         it is refused rather than built with a dead disk"
    );
}

/// A disk that is not a whole number of sectors is refused, not rounded.
#[test]
fn a_disk_that_is_not_a_whole_number_of_sectors_is_refused() {
    for storage in [
        BlockStorage::Memory { bytes: 513 },
        BlockStorage::Memory { bytes: 0 },
        BlockStorage::CopyOnWrite { base: 100 },
    ] {
        let profile = MachineProfile::lza64_native_v1().with_device(
            DeviceProfile::new(
                DeviceId::new(7),
                DeviceClass::Block,
                PhysicalAddress::new(0x4000_7000),
                RegionPermissions::new(true, true, false, true),
            )
            .with_block_storage(storage),
        );
        assert!(
            matches!(profile.validate(), Err(ProfileError::BlockStorage { .. })),
            "a {storage:?} disk was accepted: rounding it down would give a guest a \
             capacity the profile never promised"
        );
    }
}

/// Storage on a device that is not a block device is refused.
#[test]
fn storage_on_a_device_that_is_not_a_block_device_is_refused() {
    let profile = MachineProfile::lza64_native_v1().with_device(
        DeviceProfile::new(
            DeviceId::new(7),
            DeviceClass::Timer,
            PhysicalAddress::new(0x4000_5000),
            RegionPermissions::new(true, true, false, true),
        )
        .with_block_storage(BlockStorage::Memory {
            bytes: lazalith_devices::SECTOR_BYTES,
        }),
    );
    assert!(
        matches!(profile.validate(), Err(ProfileError::BlockStorage { .. })),
        "a timer with a disk is a profile that does not mean what it says"
    );
}

/// A copy-on-write disk is buildable and presents a writable disk over a base.
#[test]
fn a_copy_on_write_disk_is_buildable() {
    let profile = MachineProfile::lza64_native_v1().with_device(
        DeviceProfile::new(
            DeviceId::new(7),
            DeviceClass::Block,
            PhysicalAddress::new(0x4000_7000),
            RegionPermissions::new(true, true, false, true),
        )
        .with_block_storage(BlockStorage::CopyOnWrite {
            base: 8 * lazalith_devices::SECTOR_BYTES,
        }),
    );
    assert!(profile.validate().is_ok());
    let mut machine =
        ProfiledMachine::from_profile(&profile).expect("a copy-on-write disk is buildable");
    let device = machine
        .devices_mut()
        .device_mut(DeviceId::new(7))
        .expect("the block device is in the machine");
    let status = device
        .read(lazalith_devices::BLOCK_REGISTER_STATUS, DataSize::Double)
        .expect("a status read");
    assert_ne!(
        status & lazalith_devices::BLOCK_STATUS_WRITABLE,
        0,
        "the overlay is writable even though its base is not, and a guest can see that"
    );
}

// -- B7: the display profile ---------------------------------------------------

/// A display device names its architecture, and the native one builds.
#[test]
fn a_native_display_profile_builds() {
    let profile = MachineProfile::lza64_native_v1().with_device(
        DeviceProfile::new(
            DeviceId::new(5),
            DeviceClass::Display,
            PhysicalAddress::new(0x4003_0000),
            RegionPermissions::new(true, true, false, true),
        )
        .with_display_profile(lazalith_devices::DisplayProfile::Native),
    );
    assert!(profile.validate().is_ok());
    let machine = ProfiledMachine::from_profile(&profile).expect("a native display builds");
    machine
        .matches_profile(&profile)
        .expect("and the machine is the one described");
}

/// The two display architectures §28 names but B7 does not build are refused.
#[test]
fn an_unbuilt_display_architecture_is_refused_by_name() {
    for display in [
        lazalith_devices::DisplayProfile::VgaCompatible,
        lazalith_devices::DisplayProfile::ModernFramebuffer,
    ] {
        let profile = MachineProfile::lza64_native_v1().with_device(
            DeviceProfile::new(
                DeviceId::new(5),
                DeviceClass::Display,
                PhysicalAddress::new(0x4003_0000),
                RegionPermissions::new(true, true, false, true),
            )
            .with_display_profile(display),
        );
        assert!(
            matches!(
                profile.validate(),
                Err(ProfileError::UnconstructibleDisplay { profile: p, .. }) if p == display
            ),
            "a {display} display must be refused by name, so the caller knows it is the \
             VGA display that is missing rather than that displays are unavailable"
        );
    }
}

/// A display with no architecture named gets the platform default.
#[test]
fn a_display_with_no_architecture_named_uses_the_native_one() {
    let profile = MachineProfile::lza64_native_v1().with_device(DeviceProfile::new(
        DeviceId::new(5),
        DeviceClass::Display,
        PhysicalAddress::new(0x4003_0000),
        RegionPermissions::new(true, true, false, true),
    ));
    assert!(
        profile.validate().is_ok(),
        "naming no architecture means the native one, and a display that had to name it \
         to be built would be a display most callers could not use"
    );
}
