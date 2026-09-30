use lazalith_cpu::{ArchitecturalState, ExecutionEngine, OutcomeApplication, Processor};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_memory::{AddressSpace, Bus, MemoryRegion, RegionPermissions as RP};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress as I, PhysicalAddress as P, VirtualAddress as V,
};

fn li(register: u8, value: i32) -> [u8; 8] {
    let mut bytes = [2, register, 0, 0, 0, 0, 0, 0];
    bytes[4..].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn program(text: &[u8]) -> Vec<u8> {
    let mut code = Vec::new();
    code.extend_from_slice(&li(1, 0x1000));
    for byte in text {
        code.extend_from_slice(&li(2, i32::from(*byte)));
        code.extend_from_slice(&[0x32, 0x12, 0, 0, 0, 0, 0, 0]);
    }
    code.extend_from_slice(&[0x52, 0, 0, 0, 0, 0, 0, 0]);
    code
}

fn setup(config: C, capacity: usize, text: &[u8]) -> (Processor, Bus<ConsoleDevice>) {
    let mut devices = DeviceManager::new();
    devices
        .insert(DeviceId::new(1), ConsoleDevice::new(capacity).unwrap())
        .unwrap();
    let mut bus = Bus::with_devices(AddressSpace::new(config), devices);
    bus.map(
        MemoryRegion::rom(
            config,
            P::new(0),
            &program(text),
            RP::new(true, false, true, true),
        )
        .unwrap(),
    )
    .unwrap();
    bus.map(
        MemoryRegion::ram(config, P::new(0x800), 256, RP::new(true, true, false, true)).unwrap(),
    )
    .unwrap();
    bus.map_device(
        DeviceId::new(1),
        P::new(0x1000),
        RP::new(false, true, false, true),
    )
    .unwrap();
    let cpu = Processor::new(ArchitecturalState::new(config, I::new(0), V::new(0x900), 0).unwrap());
    (cpu, bus)
}

#[test]
fn encoded_guest_hello_runs_through_interpreter_rom_and_mmio_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let text = b"Hello, Lazalith";
        let (mut cpu, mut bus) = setup(config, text.len(), text);
        for step in 0..(2 * text.len() + 2) {
            let event = lazalith_cpu::ReferenceInterpreter::new()
                .step(&mut cpu, &mut bus)
                .unwrap();
            assert_eq!(
                event.outcome(),
                if step == 2 * text.len() + 1 {
                    OutcomeApplication::Halted
                } else {
                    OutcomeApplication::Continue
                }
            );
        }
        assert_eq!(
            bus.devices().device(DeviceId::new(1)).unwrap().output(),
            text
        );
        assert!(
            lazalith_cpu::ReferenceInterpreter::new()
                .step(&mut cpu, &mut bus)
                .is_err()
        );
        assert_eq!(
            bus.devices().device(DeviceId::new(1)).unwrap().output(),
            text
        );
    }
}

#[test]
fn console_full_fault_preserves_cpu_ram_and_prior_output() {
    for config in [C::lz32(), C::lz64()] {
        let (mut cpu, mut bus) = setup(config, 1, b"AB");
        for _ in 0..4 {
            lazalith_cpu::ReferenceInterpreter::new()
                .step(&mut cpu, &mut bus)
                .unwrap();
        }
        let before = cpu.architectural().clone();
        let error = lazalith_cpu::ReferenceInterpreter::new()
            .step(&mut cpu, &mut bus)
            .unwrap_err()
            .into_guest()
            .expect("the reference interpreter never declines");
        assert!(matches!(
            error.cause,
            lazalith_cpu::CpuFaultCause::Memory {
                source: lazalith_memory::MemoryFault {
                    kind: lazalith_memory::MemoryFaultKind::Device(
                        lazalith_devices::DeviceError::Capacity
                    ),
                    ..
                },
                ..
            }
        ));
        assert_eq!(cpu.architectural(), &before);
        assert_eq!(
            bus.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
        let mut ram = [9; 256];
        bus.peek(P::new(0x800), &mut ram).unwrap();
        assert_eq!(ram, [0; 256]);
    }
}
