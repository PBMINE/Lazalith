use lazalith_cpu::CpuFaultCause;
use lazalith_devices::{DeviceId, DeviceManager};
use lazalith_machine::{LazalithMachine, MachineError, MachineSetup};
use lazalith_memory::{MemoryFaultKind, MemoryRegion, RegionPermissions as RP};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress as I, PhysicalAddress as P,
    VirtualAddress as V,
};

fn li(register: u8, value: i32) -> [u8; 8] {
    let mut bytes = [2, register, 0, 0, 0, 0, 0, 0];
    bytes[4..].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn text_code(text: &[u8]) -> Vec<u8> {
    let mut code = Vec::new();
    code.extend_from_slice(&li(1, 0x1000));
    for byte in text {
        code.extend_from_slice(&li(2, i32::from(*byte)));
        code.extend_from_slice(&[0x32, 0x12, 0, 0, 0, 0, 0, 0]);
    }
    code.extend_from_slice(&[0x52, 0, 0, 0, 0, 0, 0, 0]);
    code
}

fn make_machine(
    config: C,
    capacity: usize,
    text: &[u8],
) -> LazalithMachine<lazalith_devices::ConsoleDevice> {
    let mut devices = DeviceManager::new();
    devices
        .insert(
            DeviceId::new(1),
            lazalith_devices::ConsoleDevice::new(capacity).unwrap(),
        )
        .unwrap();
    let setup = MachineSetup {
        config,
        devices,
        regions: vec![
            MemoryRegion::ram(config, P::new(0x800), 256, RP::new(true, true, false, true))
                .unwrap(),
        ],
        pc: I::new(0),
        sp: V::new(0x900),
        status: 0,
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).unwrap();
    machine
        .load_region(
            MemoryRegion::rom(
                config,
                P::new(0),
                &text_code(text),
                RP::new(true, false, true, true),
            )
            .unwrap(),
        )
        .unwrap();
    machine
        .map_device(
            DeviceId::new(1),
            P::new(0x1000),
            RP::new(false, true, false, true),
        )
        .unwrap();
    machine
}

#[test]
fn machine_produces_guest_hello_in_both_modes_with_no_internals_exposed() {
    for config in [C::lz32(), C::lz64()] {
        let text = b"Hello, Lazalith";
        let mut machine = make_machine(config, text.len(), text);
        let run = machine.run(2 * text.len() as u64 + 2).unwrap();
        assert_eq!(run.halted_at, Some(2 * text.len() as u64 + 2));
        assert_eq!(run.executed, 2 * text.len() as u64 + 2);
        assert!(machine.is_halted());
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            text
        );
        assert!(matches!(machine.step(), Err(MachineError::Halted)));
        assert_eq!(
            machine.architectural_state().pc(),
            I::new(8 * (2 * text.len() + 2) as u64)
        );
    }
}

#[test]
fn machine_clock_is_deterministic_and_fault_preserves_state_and_time() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = make_machine(config, 1, b"AB");
        machine.advance_clock(CycleCount::new(7)).unwrap();
        assert_eq!(machine.clock().elapsed(), CycleCount::new(7));
        assert_eq!(
            machine
                .devices()
                .device(DeviceId::new(1))
                .unwrap()
                .elapsed(),
            CycleCount::new(7)
        );
        machine.run(4).unwrap();
        let before = machine.architectural_state().clone();
        let error = machine.step().unwrap_err();
        assert!(matches!(
            error,
            lazalith_machine::MachineError::Fault(boxed) if matches!(
                boxed.cause,
                CpuFaultCause::Memory {
                    source: lazalith_memory::MemoryFault {
                        kind: MemoryFaultKind::Device(lazalith_devices::DeviceError::Capacity),
                        ..
                    },
                    ..
                }
            )
        ));
        assert_eq!(machine.architectural_state(), &before);
        assert_eq!(machine.clock().elapsed(), CycleCount::new(7));
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
        assert!(!machine.is_halted());
        assert!(machine.advance_clock(CycleCount::new(u64::MAX)).is_err());
        assert_eq!(machine.clock().elapsed(), CycleCount::new(7));
    }
}

#[test]
fn repeated_construction_is_deterministic_and_run_is_bounded() {
    for config in [C::lz32(), C::lz64()] {
        let text = b"Hello, Lazalith";
        let mut first = make_machine(config, text.len(), text);
        let mut second = make_machine(config, text.len(), text);
        let bounded = first.run(3).unwrap();
        assert_eq!(bounded.halted_at, None);
        assert_eq!(bounded.executed, 3);
        assert!(!first.is_halted());
        let full = first.run(100).unwrap();
        assert_eq!(full.executed, 32 - 3);
        assert_eq!(full.halted_at, Some(32));
        let full_other = second.run(100).unwrap();
        assert_eq!(full_other.executed, 32);
        assert_eq!(full.halted_at, full_other.halted_at);
        let mut left = [0u8; 16];
        let mut right = [0u8; 16];
        first.peek_memory(P::new(0), &mut left).unwrap();
        second.peek_memory(P::new(0), &mut right).unwrap();
        assert_eq!(left, right);
        assert_eq!(
            first.devices().device(DeviceId::new(1)).unwrap().output(),
            second.devices().device(DeviceId::new(1)).unwrap().output()
        );
    }
}

#[test]
fn stepping_then_running_counts_every_successful_instruction() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = make_machine(config, 0, b"");
        machine.step().unwrap();
        let result = machine.run(10).unwrap();
        assert_eq!(result.executed, 1);
        assert_eq!(result.halted_at, Some(2));
        assert_eq!(result.trap, None);
    }
}

#[test]
fn run_returns_trap_without_reexecuting_unchanged_pc() {
    for config in [C::lz32(), C::lz64()] {
        for (opcode, request) in [
            (0x50, lazalith_cpu::TrapRequest::Syscall),
            (0x51, lazalith_cpu::TrapRequest::Software(0)),
        ] {
            let mut machine = LazalithMachine::new(MachineSetup::<lazalith_devices::NoDevice> {
                config,
                devices: DeviceManager::new(),
                regions: vec![
                    MemoryRegion::rom(
                        config,
                        P::new(0),
                        &[opcode, 0, 0, 0, 0, 0, 0, 0],
                        RP::new(false, false, true, true),
                    )
                    .unwrap(),
                ],
                pc: I::new(0),
                sp: V::new(0x100),
                status: 0,
                initial_time: CycleCount::new(0),
            })
            .unwrap();
            let before = machine.architectural_state().clone();
            let result = machine.run(100).unwrap();
            assert_eq!(result.executed, 1);
            assert_eq!(result.trap, Some((request, I::new(8))));
            assert_eq!(result.halted_at, None);
            assert_eq!(machine.architectural_state(), &before);
        }
    }
}

#[test]
fn construction_synchronizes_preticked_devices_without_double_advancing() {
    for initial in [4, 5, 9] {
        let mut devices = DeviceManager::new();
        devices
            .insert(
                DeviceId::new(1),
                lazalith_devices::ConsoleDevice::new(1).unwrap(),
            )
            .unwrap();
        devices.tick(CycleCount::new(5)).unwrap();
        let result = LazalithMachine::new(MachineSetup {
            config: C::lz64(),
            devices,
            regions: vec![],
            pc: I::new(0),
            sp: V::new(0x100),
            status: 0,
            initial_time: CycleCount::new(initial),
        });
        if initial < 5 {
            assert!(
                matches!(result, Err(MachineError::InitialClock { devices, requested })
                if devices == CycleCount::new(5) && requested == CycleCount::new(initial))
            );
        } else {
            let machine = result.unwrap();
            assert_eq!(machine.clock().elapsed(), CycleCount::new(initial));
            assert_eq!(
                machine.devices().clock().elapsed(),
                CycleCount::new(initial)
            );
            assert_eq!(
                machine
                    .devices()
                    .device(DeviceId::new(1))
                    .unwrap()
                    .elapsed(),
                CycleCount::new(initial)
            );
        }
    }
}

#[test]
fn construction_and_loader_failures_preserve_explicit_setup_contracts() {
    let config = C::lz64();
    let invalid_setup = MachineSetup::<lazalith_devices::ConsoleDevice> {
        config,
        devices: DeviceManager::new(),
        regions: Vec::new(),
        pc: I::new(0x100),
        sp: V::new(0x900),
        status: 0,
        initial_time: CycleCount::new(0),
    };
    let mut invalid_machine = LazalithMachine::new(invalid_setup).unwrap();
    assert!(matches!(
        invalid_machine.step(),
        Err(lazalith_machine::MachineError::Fault(_))
    ));
    let mut machine = make_machine(config, 8, b"Hello, Lazalith");
    assert!(machine.load_bytes(P::new(0), &[9]).is_err());
    assert!(
        machine
            .load_region(
                MemoryRegion::ram(config, P::new(0x808), 16, RP::new(true, true, false, true))
                    .unwrap()
            )
            .is_err()
    );
    assert!(
        machine
            .map_device(
                DeviceId::new(2),
                P::new(0x1001),
                RP::new(false, true, false, true)
            )
            .is_err()
    );
    assert!(machine.load_bytes(P::new(0x1000), &[1]).is_err());
    let mut second_machine = make_machine(config, 8, b"Hello, Lazalith");
    assert!(second_machine.run(1000).is_err());
    assert!(!second_machine.is_halted());
    assert!(matches!(
        second_machine.step(),
        Err(lazalith_machine::MachineError::Fault(boxed)) if matches!(
            boxed.cause,
            CpuFaultCause::Memory {
                source: lazalith_memory::MemoryFault {
                    kind: MemoryFaultKind::Device(lazalith_devices::DeviceError::Capacity),
                    ..
                },
                ..
            }
        )
    ));
}
