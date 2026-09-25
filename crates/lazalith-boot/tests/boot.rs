use lazalith_boot::{
    BOOT_ADDRESS, BOOT_FORMAT_VERSION, BOOT_HEADER_ADDRESS, BOOT_HEADER_SIZE, BOOT_ROM_LENGTH,
    BOOT_ROM_PHYSICAL_START, BOOT_ROM_START, BootArchitecture, BootError, BootImage,
    KERNEL_IMAGE_LENGTH, KERNEL_INITIAL_SP, KERNEL_LOAD_ADDRESS, KERNEL_PAYLOAD_ADDRESS,
    MAX_BOOT_ROM_PAYLOAD, RESET_VECTOR,
};
use lazalith_cpu::Privilege;
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::MachineState;
use lazalith_memory::RegionPermissions;
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, PhysicalAddress as P, VirtualAddress as V,
};
use std::error::Error;

fn kernel(config: C) -> Vec<u8> {
    let halt = Instruction::new(config, Opcode::Halt, &[]).unwrap();
    let nop = Instruction::new(config, Opcode::Nop, &[]).unwrap();
    let mut bytes = encode(config, &halt).unwrap().to_vec();
    bytes.extend_from_slice(&encode(config, &nop).unwrap());
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

fn image(config: C) -> BootImage {
    BootImage::new(config, kernel(config), 0).unwrap()
}

fn put_u16(rom: &mut [u8], offset: usize, value: u16) {
    rom[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(rom: &mut [u8], offset: usize, value: u32) {
    rom[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(rom: &mut [u8], offset: usize, value: u64) {
    rom[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn valid_image_roundtrips_loads_and_transfers_control_in_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let image = image(config);
        assert_eq!(image.config(), config);
        assert_eq!(image.header().config(), config);
        assert_eq!(image.header().image_length(), 24);
        assert_eq!(image.header().entry_offset(), 0);
        assert_eq!(image.entry().as_u64(), KERNEL_LOAD_ADDRESS);
        assert_eq!(image.kernel().unwrap(), kernel(config));
        assert_eq!(image.rom().len() as u64, BOOT_ROM_LENGTH);

        let parsed = BootImage::from_rom(config, image.rom().to_vec()).unwrap();
        assert_eq!(parsed.header(), image.header());
        assert_eq!(parsed.rom(), image.rom());

        let mut machine = image.start(DeviceManager::<NoDevice>::new()).unwrap();
        assert_eq!(machine.state(), MachineState::Reset);
        assert_eq!(machine.architectural_state().pc(), image.entry());
        assert_eq!(
            machine.architectural_state().sp(),
            V::new(KERNEL_INITIAL_SP)
        );
        assert_eq!(
            machine.architectural_state().status().privilege(),
            Privilege::Supervisor
        );
        assert!(!machine.architectural_state().status().interrupts_enabled());
        let regions = machine.memory().regions();
        assert_eq!(regions.len(), 7);
        assert_eq!(regions[0].start().as_u64(), 0);
        assert_eq!(regions[0].length(), BOOT_ROM_LENGTH);
        assert_eq!(
            regions[0].permissions(),
            RegionPermissions::new(true, false, true, false)
        );
        assert_eq!(regions[1].start().as_u64(), KERNEL_LOAD_ADDRESS);
        assert_eq!(regions[1].length(), 0x0008_0000);
        assert_eq!(
            regions[1].permissions(),
            RegionPermissions::new(true, true, true, false)
        );
        assert_eq!(regions[2].start().as_u64(), 0x0018_0000);
        assert_eq!(regions[2].length(), 0x0001_0000);
        assert_eq!(
            regions[2].permissions(),
            RegionPermissions::new(true, true, false, false)
        );
        assert_eq!(regions[3].start().as_u64(), 0x0019_0000);
        assert_eq!(regions[3].length(), 0x0007_0000);
        assert_eq!(
            regions[3].permissions(),
            RegionPermissions::new(true, true, false, false)
        );
        assert_eq!(regions[4].start().as_u64(), 0x0020_0000);
        assert_eq!(regions[4].length(), 0x0010_0000);
        assert_eq!(
            regions[4].permissions(),
            RegionPermissions::new(true, false, true, true)
        );
        assert_eq!(regions[5].start().as_u64(), 0x0030_0000);
        assert_eq!(regions[5].length(), 0x0010_0000);
        assert_eq!(
            regions[5].permissions(),
            RegionPermissions::new(true, true, false, true)
        );
        assert_eq!(regions[6].start().as_u64(), 0x0040_0000);
        assert_eq!(regions[6].length(), 0x0001_0000);
        assert_eq!(
            regions[6].permissions(),
            RegionPermissions::new(true, true, false, true)
        );
        let registers = machine.architectural_state().registers();
        assert_eq!(registers.read_raw(0).unwrap(), 0);
        assert_eq!(registers.read_raw(1).unwrap(), KERNEL_LOAD_ADDRESS);
        assert_eq!(
            registers.read_raw(2).unwrap(),
            image.header().image_length()
        );
        assert_eq!(registers.read_raw(3).unwrap(), image.entry().as_u64());
        for register in 4..=14 {
            assert_eq!(registers.read_raw(register).unwrap(), 0);
        }
        assert_eq!(registers.read_raw(15).unwrap(), image.entry().as_u64());

        let mut loaded = [0u8; 24];
        machine
            .peek_memory(P::new(KERNEL_LOAD_ADDRESS), &mut loaded)
            .unwrap();
        assert_eq!(loaded.as_slice(), kernel(config).as_slice());
        assert_eq!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Halted
        );
        assert_eq!(machine.state(), MachineState::Halted);
        assert_eq!(
            machine.architectural_state().pc().as_u64(),
            KERNEL_LOAD_ADDRESS + 8
        );
    }
}

#[test]
fn nonzero_entry_offsets_are_loaded_and_selected_exactly() {
    for config in [C::lz32(), C::lz64()] {
        let nop = Instruction::new(config, Opcode::Nop, &[]).unwrap();
        let halt = Instruction::new(config, Opcode::Halt, &[]).unwrap();
        let bytes = [
            encode(config, &nop).unwrap().as_slice(),
            encode(config, &halt).unwrap().as_slice(),
        ]
        .concat();
        let image = BootImage::new(config, bytes, 8).unwrap();
        assert_eq!(image.entry().as_u64(), KERNEL_LOAD_ADDRESS + 8);
        let mut machine = image.start(DeviceManager::<NoDevice>::new()).unwrap();
        assert_eq!(machine.architectural_state().pc(), image.entry());
        assert_eq!(
            machine
                .architectural_state()
                .registers()
                .read_raw(3)
                .unwrap(),
            image.entry().as_u64()
        );
        assert_eq!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Halted
        );
        assert_eq!(
            machine.architectural_state().pc().as_u64(),
            KERNEL_LOAD_ADDRESS + 16
        );
    }
}

#[test]
fn constructor_validates_kernel_before_building_a_boot_image() {
    let config = C::lz64();
    assert!(matches!(
        BootImage::new(config, Vec::new(), 0),
        Err(BootError::InvalidImageLength { input: 0 })
    ));
    assert!(matches!(
        BootImage::new(config, kernel(config), 1),
        Err(BootError::InvalidEntryOffset {
            offset: 1,
            image_length: 24
        })
    ));
    assert!(matches!(
        BootImage::new(config, vec![0u8; 8], 4),
        Err(BootError::InvalidEntryOffset {
            offset: 4,
            image_length: 8
        })
    ));
    assert!(matches!(
        BootImage::new(config, vec![0xff; 8], 0),
        Err(BootError::EntryInstruction(_))
    ));
    assert!(matches!(
        BootImage::new(config, vec![0u8; MAX_BOOT_ROM_PAYLOAD as usize + 1], 0),
        Err(BootError::PayloadDoesNotFitRom {
            input: _,
            maximum: MAX_BOOT_ROM_PAYLOAD
        })
    ));
}

#[test]
fn payload_capacity_boundary_is_checked_before_rom_materialization() {
    let config = C::lz64();
    let mut maximum = vec![0u8; MAX_BOOT_ROM_PAYLOAD as usize];
    let nop = Instruction::new(config, Opcode::Nop, &[]).unwrap();
    maximum[..8].copy_from_slice(&encode(config, &nop).unwrap());
    let image = BootImage::new(config, maximum, 0).unwrap();
    assert_eq!(image.header().image_length(), MAX_BOOT_ROM_PAYLOAD);
    assert!(BootImage::from_rom(config, image.rom().to_vec()).is_ok());

    let oversized = vec![0u8; MAX_BOOT_ROM_PAYLOAD as usize + 1];
    assert!(matches!(
        BootImage::new(config, oversized, 0),
        Err(BootError::PayloadDoesNotFitRom {
            input,
            maximum
        }) if input == MAX_BOOT_ROM_PAYLOAD + 1 && maximum == MAX_BOOT_ROM_PAYLOAD
    ));
}

#[test]
fn rom_parser_rejects_every_header_field_and_preserves_validation_order() {
    let config = C::lz64();
    let base = image(config);
    let header = BOOT_HEADER_ADDRESS as usize;
    let payload = KERNEL_PAYLOAD_ADDRESS as usize;

    assert!(matches!(
        BootImage::from_rom(config, vec![0u8; 7]),
        Err(BootError::InvalidBootRomLength {
            expected: BOOT_ROM_LENGTH,
            actual: 7
        })
    ));
    assert!(matches!(
        BootImage::from_rom(config, vec![0u8; BOOT_ROM_LENGTH as usize + 1]),
        Err(BootError::InvalidBootRomLength {
            expected: BOOT_ROM_LENGTH,
            actual
        }) if actual == BOOT_ROM_LENGTH + 1
    ));

    let mut bytes = base.rom().to_vec();
    bytes[header] ^= 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidMagic)
    ));

    let mut bytes = base.rom().to_vec();
    put_u32(&mut bytes, header + 8, BOOT_FORMAT_VERSION + 1);
    put_u16(&mut bytes, header + 14, BOOT_HEADER_SIZE + 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidHeaderSize { input: 49 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u32(&mut bytes, header + 8, BOOT_FORMAT_VERSION + 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::UnsupportedVersion { input: 2 })
    ));

    let mut bytes = base.rom().to_vec();
    bytes[header + 12] = 3;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidArchitecture { input: 3 })
    ));
    assert!(matches!(
        BootImage::from_rom(C::lz32(), base.rom().to_vec()),
        Err(BootError::ArchitectureMismatch {
            expected: BootArchitecture::Lz32,
            actual: BootArchitecture::Lz64
        })
    ));

    let mut bytes = image(C::lz32()).rom().to_vec();
    put_u64(&mut bytes, header + 16, 0x0000_0001_0000_0000);
    assert!(matches!(
        BootImage::from_rom(C::lz32(), bytes),
        Err(BootError::InvalidLoadAddress { .. })
    ));

    let mut bytes = image(C::lz32()).rom().to_vec();
    put_u64(&mut bytes, header + 24, 0x0000_0001_0000_0000);
    assert!(matches!(
        BootImage::from_rom(C::lz32(), bytes),
        Err(BootError::InvalidImageLength { input })
            if input == 0x0000_0001_0000_0000
    ));

    let mut bytes = base.rom().to_vec();
    bytes[header + 13] = 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::UnsupportedFlags { input: 1 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u16(&mut bytes, header + 14, BOOT_HEADER_SIZE + 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidHeaderSize { input: 49 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u64(&mut bytes, header + 16, KERNEL_LOAD_ADDRESS + 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidLoadAddress { .. })
    ));

    let mut bytes = base.rom().to_vec();
    put_u64(&mut bytes, header + 24, 0);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidImageLength { input: 0 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u64(&mut bytes, header + 24, 0);
    put_u64(&mut bytes, header + 32, u64::MAX);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidImageLength { input: 0 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u64(&mut bytes, header + 32, 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidEntryOffset {
            offset: 1,
            image_length: 24
        })
    ));

    let mut bytes = image(C::lz32()).rom().to_vec();
    put_u64(&mut bytes, header + 32, 0x0000_0001_0000_0000);
    assert!(matches!(
        BootImage::from_rom(C::lz32(), bytes),
        Err(BootError::InvalidEntryOffset {
            offset: 0x0000_0001_0000_0000,
            image_length: 24
        })
    ));

    let mut bytes = base.rom().to_vec();
    put_u32(&mut bytes, header + 44, 1);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::NonzeroReserved { input: 1 })
    ));

    let mut bytes = base.rom().to_vec();
    put_u32(&mut bytes, header + 40, 0);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::ChecksumMismatch { .. })
    ));

    let mut bytes = base.rom().to_vec();
    bytes[payload + 8] ^= 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::ChecksumMismatch { .. })
    ));

    let mut bytes = base.rom().to_vec();
    bytes[payload - 1] = 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::NonzeroReservedByte {
            offset,
            input: 1
        }) if offset == payload - 1
    ));

    let mut bytes = base.rom().to_vec();
    bytes[payload + base.header().image_length() as usize] = 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::NonzeroReservedByte {
            offset,
            input: 1
        }) if offset == payload + base.header().image_length() as usize
    ));

    let mut bytes = base.rom().to_vec();
    bytes[0] ^= 1;
    put_u64(&mut bytes, header + 24, 0);
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::InvalidImageLength { input: 0 })
    ));

    let mut bytes = base.rom().to_vec();
    bytes[0] ^= 1;
    assert!(matches!(
        BootImage::from_rom(config, bytes),
        Err(BootError::BootRomMismatch { offset: 0, .. })
    ));
}

#[test]
fn step28_rejects_unmapped_devices_instead_of_silently_ignoring_them() {
    let mut devices = DeviceManager::new();
    devices
        .insert(DeviceId::new(1), ConsoleDevice::new(1).unwrap())
        .unwrap();
    assert!(matches!(
        image(C::lz64()).start(devices),
        Err(BootError::UnexpectedDevices { count: 1 })
    ));
}

#[test]
fn machine_setup_rejects_an_initial_device_clock_ahead_of_boot_epoch() {
    let image = image(C::lz64());
    let mut devices = DeviceManager::<NoDevice>::new();
    devices.tick(CycleCount::new(1)).unwrap();
    let error = image.start(devices).unwrap_err();
    assert!(matches!(
        error,
        BootError::Machine(ref source)
            if matches!(
                source.as_ref(),
                lazalith_machine::MachineError::InitialClock {
                    devices,
                    requested
                } if devices.as_u64() == 1 && requested.as_u64() == 0
            )
    ));
    assert!(Error::source(&error).is_some());
}

#[test]
fn the_documented_reset_vector_is_the_single_boot_source_of_truth() {
    assert_eq!(BOOT_ADDRESS, RESET_VECTOR);
    assert_eq!(RESET_VECTOR.as_u64(), BOOT_ROM_START);
    assert_eq!(BOOT_ROM_PHYSICAL_START.as_u64(), BOOT_ROM_START);
    assert_eq!(
        KERNEL_INITIAL_SP,
        KERNEL_LOAD_ADDRESS + KERNEL_IMAGE_LENGTH + 0xF000
    );
    for config in [C::lz32(), C::lz64()] {
        let setup = image(config)
            .machine_setup(DeviceManager::<NoDevice>::new())
            .unwrap();
        assert_eq!(setup.pc, RESET_VECTOR);
        assert_eq!(setup.sp.as_u64(), KERNEL_INITIAL_SP);
        let machine = image(config)
            .start(DeviceManager::<NoDevice>::new())
            .unwrap();
        assert_eq!(
            machine.architectural_state().pc().as_u64(),
            KERNEL_LOAD_ADDRESS
        );
        assert_eq!(
            machine.architectural_state().sp().as_u64(),
            KERNEL_INITIAL_SP
        );
        assert_eq!(
            machine.architectural_state().sp().as_u64(),
            KERNEL_INITIAL_SP
        );
        let starts: Vec<u64> = machine
            .memory()
            .regions()
            .iter()
            .map(|region| region.start().as_u64())
            .collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), starts.len(), "boot regions must be disjoint");
        assert!(starts.contains(&BOOT_ROM_PHYSICAL_START.as_u64()));
        assert!(starts.contains(&KERNEL_LOAD_ADDRESS));
    }
}
