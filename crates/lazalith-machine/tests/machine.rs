use lazalith_cpu::{CpuFault, CpuFaultCause, TrapCause};
use lazalith_devices::{DeviceId, DeviceManager};
use lazalith_isa::{Opcode, decode};
use lazalith_machine::{
    LazalithMachine, MachineError, MachineOperation, MachineSetup, MachineState,
};
use lazalith_memory::{MemoryFault, MemoryFaultKind, MemoryRegion, RegionPermissions as RP};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress as I, InterruptId,
    PhysicalAddress as P, VirtualAddress as V,
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

fn ready_machine(
    config: C,
    capacity: usize,
    text: &[u8],
) -> LazalithMachine<lazalith_devices::ConsoleDevice> {
    let mut machine = make_machine(config, capacity, text);
    machine.reset();
    machine
}

fn assert_invalid_transition<T: core::fmt::Debug>(
    result: Result<T, MachineError>,
    operation: MachineOperation,
    state: MachineState,
) {
    let error = result.unwrap_err();
    assert!(matches!(
        error,
        MachineError::InvalidTransition {
            operation: actual,
            state: actual_state,
        } if actual == operation && actual_state == state
    ));
    assert_eq!(
        error.to_string(),
        format!("{operation:?} is invalid while machine is {state:?}")
    );
    assert!(std::error::Error::source(&error).is_none());
}

#[test]
fn machine_produces_guest_hello_in_both_modes_with_no_internals_exposed() {
    for config in [C::lz32(), C::lz64()] {
        let text = b"Hello, Lazalith";
        let mut machine = ready_machine(config, text.len(), text);
        let before_inspection = machine.architectural_state().clone();
        let mut expected_instruction = [0u8; 8];
        expected_instruction.copy_from_slice(&text_code(text)[..8]);
        assert_eq!(
            machine.inspect_instruction(I::new(0)).unwrap(),
            expected_instruction
        );
        assert_eq!(machine.architectural_state(), &before_inspection);
        let run = machine.run(2 * text.len() as u64 + 2).unwrap();
        assert_eq!(run.halted_at, Some(2 * text.len() as u64 + 2));
        assert_eq!(run.executed, 2 * text.len() as u64 + 2);
        assert_eq!(machine.state(), MachineState::Halted);
        assert!(machine.is_halted());
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            text
        );
        assert_invalid_transition(machine.step(), MachineOperation::Step, MachineState::Halted);
        assert_eq!(
            machine.architectural_state().pc(),
            I::new(8 * (2 * text.len() + 2) as u64)
        );
    }
}

#[test]
fn machine_clock_is_deterministic_and_fault_preserves_state_and_time() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = ready_machine(config, 1, b"AB");
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
        machine.set_trap_vector(I::new(0)).unwrap();
        let run = machine.run(4).unwrap();
        let before = machine.architectural_state().clone();
        let event = match machine.step().unwrap() {
            lazalith_machine::MachineEvent::Trapped { event } => event,
            other => panic!("expected trap, got {other:?}"),
        };
        assert_eq!(event.cause, TrapCause::DeviceAccess);
        assert_eq!(event.payload, 0);
        assert_eq!(event.resume_pc, before.pc());
        assert!(matches!(
            machine.last_trap_fault(),
            Some(CpuFault { cause: CpuFaultCause::Memory { source, .. }, .. })
                if matches!(
                    source.kind,
                    MemoryFaultKind::Device(lazalith_devices::DeviceError::Capacity)
                )
        ));
        assert_eq!(machine.state(), MachineState::Running);
        assert_eq!(machine.architectural_state().pc(), I::new(0));
        let frame = machine.trap_controller().frame().unwrap();
        assert_eq!(frame.snapshot().pc(), before.pc());
        assert_eq!(frame.snapshot().sp(), before.sp());
        assert_eq!(frame.snapshot().status(), before.status().bits());
        for register in 0..16 {
            assert_eq!(
                frame
                    .snapshot()
                    .register(lazalith_types::RegisterIndex::try_from(register).unwrap()),
                before.registers().read_raw(register).unwrap()
            );
        }
        // The clock is where the driver left it plus what the run cost, and the faulting
        // step that followed cost nothing because it retired nothing.
        //
        // **Asserted against `run.cycles` rather than a literal**, because the run is the
        // thing that knows what it spent and this test is about the arithmetic being
        // consistent, not about what four particular instructions cost. The cost model
        // itself is pinned by `instruction_costs_follow_the_isa_model`.
        let expected = CycleCount::new(7 + run.cycles);
        assert_eq!(machine.clock().elapsed(), expected);
        assert_eq!(
            machine
                .devices()
                .device(DeviceId::new(1))
                .unwrap()
                .elapsed(),
            expected
        );
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
        assert!(!machine.is_halted());
        let retained_fault = machine
            .last_trap_fault()
            .map(|_| machine.last_trap_fault().unwrap() as *const CpuFault<MemoryFault>);
        assert!(retained_fault.is_some());
        assert!(matches!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Stepped { .. }
        ));
        assert_eq!(
            machine
                .last_trap_fault()
                .map(|_| { machine.last_trap_fault().unwrap() as *const CpuFault<MemoryFault> }),
            retained_fault
        );
        // A refused overflow leaves the clock exactly where it was. Captured rather
        // than asserted against a literal, because the successful step above moved the
        // clock and the point of this is that the *refusal* changed nothing — not that
        // the clock happens to be at a particular number.
        let before_overflow = machine.clock().elapsed();
        assert!(
            machine.advance_clock(CycleCount::new(u64::MAX)).is_err(),
            "advancing past the end of the clock is refused"
        );
        assert_eq!(
            machine.clock().elapsed(),
            before_overflow,
            "and a refused advance does not move it"
        );
    }
}

#[test]
fn repeated_construction_is_deterministic_and_run_is_bounded() {
    for config in [C::lz32(), C::lz64()] {
        let text = b"Hello, Lazalith";
        let mut first = ready_machine(config, text.len(), text);
        let mut second = ready_machine(config, text.len(), text);
        let bounded = first.run(3).unwrap();
        assert_eq!(bounded.halted_at, None);
        assert_eq!(bounded.executed, 3);
        assert_eq!(first.state(), MachineState::Running);
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
        let mut machine = ready_machine(config, 0, b"");
        machine.step().unwrap();
        assert_eq!(machine.state(), MachineState::Reset);
        let result = machine.run(10).unwrap();
        assert_eq!(result.executed, 1);
        assert_eq!(result.halted_at, Some(2));
        assert_eq!(result.trap, None);
    }
}

#[test]
fn run_delivers_traps_and_stops_at_the_handler_without_reexecution() {
    for config in [C::lz32(), C::lz64()] {
        for (opcode, cause, payload) in [
            (0x50, TrapCause::Syscall, 0i32),
            (0x51, TrapCause::SoftwareTrap, -7i32),
        ] {
            let mut rom = vec![0u8; 0x108];
            rom[..4].copy_from_slice(&[opcode, 0, 0, 0]);
            rom[4..8].copy_from_slice(&payload.to_le_bytes());
            rom[0x100..0x108].copy_from_slice(&[0x52, 0, 0, 0, 0, 0, 0, 0]);
            let mut machine = LazalithMachine::new(MachineSetup::<lazalith_devices::NoDevice> {
                config,
                devices: DeviceManager::new(),
                regions: vec![
                    MemoryRegion::rom(config, P::new(0), &rom, RP::new(false, false, true, true))
                        .unwrap(),
                ],
                pc: I::new(0),
                sp: V::new(0x100),
                status: 0,
                initial_time: CycleCount::new(0),
            })
            .unwrap();
            machine.reset();
            machine.set_trap_vector(I::new(0x100)).unwrap();
            let before = machine.architectural_state().clone();
            let result = machine.run(100).unwrap();
            let expected_payload = match config.word_width() {
                lazalith_types::WordWidth::W32 => payload as u32 as u64,
                lazalith_types::WordWidth::W64 => payload as i64 as u64,
            };
            assert_eq!(result.executed, 1);
            assert_eq!(
                result.trap,
                Some(lazalith_machine::TrapEvent {
                    cause,
                    payload: expected_payload,
                    resume_pc: I::new(8),
                    interrupt: None,
                })
            );
            assert_eq!(result.halted_at, None);
            assert_eq!(machine.state(), MachineState::Running);
            assert_eq!(machine.architectural_state().pc(), I::new(0x100));
            assert!(!machine.architectural_state().status().interrupts_enabled());
            let frame = machine.trap_controller().frame().unwrap();
            assert_eq!(frame.snapshot().pc(), before.pc());
            assert_eq!(frame.snapshot().sp(), before.sp());
            assert_eq!(frame.snapshot().status(), before.status().bits());
            assert_eq!(frame.cause(), cause);
            assert_eq!(frame.payload(), expected_payload);
            assert_eq!(frame.resume_pc(), I::new(8));
            machine.step().unwrap();
            assert_eq!(machine.state(), MachineState::Halted);
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
            assert_eq!(machine.state(), MachineState::Created);
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
    invalid_machine.reset();
    assert!(matches!(
        invalid_machine.step(),
        Err(lazalith_machine::MachineError::TrapEntry {
            attempt: _,
            original: Some(original),
            failure
        }) if matches!(
            original.cause,
            CpuFaultCause::Fetch(lazalith_memory::MemoryFault {
                kind: MemoryFaultKind::Unmapped,
                ..
            })
        ) && matches!(
            failure.cause,
            CpuFaultCause::Control(lazalith_cpu::ControlStateError::InvalidControlState { .. })
        )
    ));
    assert_eq!(invalid_machine.state(), MachineState::Faulted);
    let mut machine = ready_machine(config, 8, b"Hello, Lazalith");
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
    let mut second_machine = ready_machine(config, 8, b"Hello, Lazalith");
    assert!(matches!(
        second_machine.run(1000),
        Err(lazalith_machine::MachineError::TrapEntry {
            attempt: _,
            original: Some(original),
            failure
        }) if matches!(
            original.cause,
            CpuFaultCause::Memory {
                source: lazalith_memory::MemoryFault {
                    kind: MemoryFaultKind::Device(lazalith_devices::DeviceError::Capacity),
                    ..
                },
                ..
            }
        ) && matches!(
            failure.cause,
            CpuFaultCause::Control(lazalith_cpu::ControlStateError::InvalidControlState { .. })
        )
    ));
    assert_eq!(second_machine.state(), MachineState::Faulted);
    assert!(!second_machine.is_halted());
    assert_invalid_transition(
        second_machine.step(),
        MachineOperation::Step,
        MachineState::Faulted,
    );
}

#[test]
fn created_requires_reset_and_zero_length_runs_honor_lifecycle() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = make_machine(config, 1, b"A");
        assert_eq!(machine.state(), MachineState::Created);
        let before = machine.architectural_state().clone();
        assert_invalid_transition(
            machine.step(),
            MachineOperation::Step,
            MachineState::Created,
        );
        assert_invalid_transition(machine.run(0), MachineOperation::Run, MachineState::Created);
        assert_invalid_transition(
            machine.pause(),
            MachineOperation::Pause,
            MachineState::Created,
        );
        assert_eq!(machine.state(), MachineState::Created);
        assert_eq!(machine.architectural_state(), &before);

        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
        assert!(matches!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Stepped {
                application: lazalith_cpu::OutcomeApplication::Continue,
            }
        ));
        assert_eq!(machine.state(), MachineState::Reset);
        assert_eq!(machine.architectural_state().pc(), I::new(8));
        assert_invalid_transition(
            machine.pause(),
            MachineOperation::Pause,
            MachineState::Reset,
        );

        assert_eq!(machine.run(0).unwrap().executed, 0);
        assert_eq!(machine.state(), MachineState::Running);
        machine.pause().unwrap();
        assert_eq!(machine.state(), MachineState::Paused);
        assert_invalid_transition(
            machine.pause(),
            MachineOperation::Pause,
            MachineState::Paused,
        );
        assert_eq!(machine.run(0).unwrap().executed, 0);
        assert_eq!(machine.state(), MachineState::Running);
        machine.pause().unwrap();
        assert_eq!(machine.state(), MachineState::Paused);
        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
        assert_eq!(machine.run(0).unwrap().executed, 0);
        assert_eq!(machine.state(), MachineState::Running);
        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
    }
}

#[test]
fn running_pause_and_step_transitions_reach_halt_then_reject_execution() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = ready_machine(config, 1, b"A");
        assert_eq!(machine.run(1).unwrap().executed, 1);
        assert_eq!(machine.state(), MachineState::Running);
        assert_eq!(machine.architectural_state().pc(), I::new(8));
        assert!(matches!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Stepped {
                application: lazalith_cpu::OutcomeApplication::Continue,
            }
        ));
        assert_eq!(machine.state(), MachineState::Running);
        assert_eq!(machine.architectural_state().pc(), I::new(16));
        machine.pause().unwrap();
        assert_eq!(machine.state(), MachineState::Paused);
        assert!(matches!(
            machine.step().unwrap(),
            lazalith_machine::MachineEvent::Stepped {
                application: lazalith_cpu::OutcomeApplication::Continue,
            }
        ));
        assert_eq!(machine.state(), MachineState::Paused);
        assert_eq!(machine.architectural_state().pc(), I::new(24));
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
        let halted = machine.run(1).unwrap();
        assert_eq!((halted.executed, halted.halted_at), (1, Some(4)));
        assert_eq!(machine.state(), MachineState::Halted);
        assert_eq!(machine.architectural_state().pc(), I::new(32));

        let halted = machine.architectural_state().clone();
        assert_invalid_transition(machine.step(), MachineOperation::Step, MachineState::Halted);
        assert_invalid_transition(machine.run(0), MachineOperation::Run, MachineState::Halted);
        assert_invalid_transition(
            machine.run(100),
            MachineOperation::Run,
            MachineState::Halted,
        );
        assert_invalid_transition(
            machine.pause(),
            MachineOperation::Pause,
            MachineState::Halted,
        );
        assert_eq!(machine.state(), MachineState::Halted);
        assert_eq!(machine.architectural_state(), &halted);

        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
        assert_eq!(machine.architectural_state().pc(), I::new(0));
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b""
        );
        assert_eq!(machine.run(4).unwrap().halted_at, Some(8));
    }
}

#[test]
fn faulted_is_terminal_until_reset_clears_cpu_and_device_state() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = ready_machine(config, 1, b"AB");
        assert!(machine.run(5).is_err());
        assert_eq!(machine.state(), MachineState::Faulted);
        assert_eq!(machine.architectural_state().pc(), I::new(32));
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
        let faulted = machine.architectural_state().clone();
        assert_invalid_transition(
            machine.step(),
            MachineOperation::Step,
            MachineState::Faulted,
        );
        assert_invalid_transition(machine.run(0), MachineOperation::Run, MachineState::Faulted);
        assert_invalid_transition(
            machine.run(100),
            MachineOperation::Run,
            MachineState::Faulted,
        );
        assert_invalid_transition(
            machine.pause(),
            MachineOperation::Pause,
            MachineState::Faulted,
        );
        assert_eq!(machine.architectural_state(), &faulted);

        machine.reset();
        assert_eq!(machine.state(), MachineState::Reset);
        assert_eq!(machine.architectural_state().pc(), I::new(0));
        assert_eq!(
            machine
                .architectural_state()
                .registers()
                .read_raw(1)
                .unwrap(),
            0
        );
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b""
        );
        assert_eq!(machine.run(3).unwrap().executed, 3);
        assert_eq!(machine.state(), MachineState::Running);
        assert_eq!(
            machine.devices().device(DeviceId::new(1)).unwrap().output(),
            b"A"
        );
    }
}

#[test]
fn reset_restores_cpu_devices_and_epoch_but_preserves_memory_and_count() {
    let config = C::lz64();
    let mut devices = DeviceManager::new();
    devices
        .insert(
            DeviceId::new(1),
            lazalith_devices::ConsoleDevice::new(1).unwrap(),
        )
        .unwrap();
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices,
        regions: vec![
            MemoryRegion::ram(config, P::new(0x800), 256, RP::new(true, true, false, true))
                .unwrap(),
        ],
        pc: I::new(0),
        sp: V::new(0x900),
        status: 4,
        initial_time: CycleCount::new(7),
    })
    .unwrap();
    machine
        .load_region(
            MemoryRegion::rom(
                config,
                P::new(0),
                &text_code(b"X"),
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
    machine.load_bytes(P::new(0x800), &[43]).unwrap();
    machine.reset();
    let initial = machine.architectural_state().clone();

    assert_eq!(machine.state(), MachineState::Reset);
    assert_eq!(machine.clock().elapsed(), CycleCount::new(7));
    assert_eq!(machine.devices().clock().elapsed(), CycleCount::new(7));
    assert_eq!(
        machine
            .devices()
            .device(DeviceId::new(1))
            .unwrap()
            .elapsed(),
        CycleCount::new(7)
    );
    assert_eq!(machine.architectural_state().status().bits(), 4);
    machine.advance_clock(CycleCount::new(3)).unwrap();
    assert_eq!(machine.run(4).unwrap().halted_at, Some(4));
    assert_eq!(machine.state(), MachineState::Halted);
    assert_eq!(
        machine.devices().device(DeviceId::new(1)).unwrap().output(),
        b"X"
    );

    machine.reset();
    assert_eq!(machine.state(), MachineState::Reset);
    assert_eq!(machine.architectural_state(), &initial);
    // A reset restores the machine but does not rewind virtual time: the clock is
    // machine-wide state, and a machine that forgot what time it was after a reset
    // would let a guest read a time that went backwards across one.
    assert_eq!(machine.clock().elapsed(), CycleCount::new(7));
    assert_eq!(machine.devices().clock().elapsed(), CycleCount::new(7));

    assert_eq!(machine.architectural_state().pc(), I::new(0));
    assert_eq!(machine.architectural_state().sp(), V::new(0x900));
    assert_eq!(machine.architectural_state().status().bits(), 4);
    assert_eq!(
        machine
            .architectural_state()
            .registers()
            .read_raw(1)
            .unwrap(),
        0
    );
    assert_eq!(
        machine.devices().device(DeviceId::new(1)).unwrap().output(),
        b""
    );
    let mut ram = [0u8; 1];
    machine.peek_memory(P::new(0x800), &mut ram).unwrap();
    assert_eq!(ram, [43]);
    assert_eq!(machine.run(4).unwrap().halted_at, Some(8));
    assert_eq!(
        machine.devices().device(DeviceId::new(1)).unwrap().output(),
        b"X"
    );
}

fn trap_machine(
    config: C,
    first: [u8; 8],
    handler: [u8; 8],
    status: u64,
) -> LazalithMachine<lazalith_devices::NoDevice> {
    let mut rom = vec![0u8; 0x108];
    rom[..8].copy_from_slice(&first);
    rom[0x100..0x108].copy_from_slice(&handler);
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions: vec![
            MemoryRegion::rom(config, P::new(0), &rom, RP::new(false, false, true, true)).unwrap(),
            MemoryRegion::ram(
                config,
                P::new(0x800),
                0x100,
                RP::new(true, true, false, true),
            )
            .unwrap(),
        ],
        pc: I::new(0),
        sp: V::new(0x880),
        status,
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    machine.reset();
    machine
}

fn trap_program_machine(
    config: C,
    program: &[u8],
    status: u64,
) -> LazalithMachine<lazalith_devices::NoDevice> {
    let mut rom = vec![0u8; 0x108];
    rom[..program.len()].copy_from_slice(program);
    rom[0x100..0x108].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions: vec![
            MemoryRegion::rom(config, P::new(0), &rom, RP::new(false, false, true, true)).unwrap(),
            MemoryRegion::ram(
                config,
                P::new(0x800),
                0x100,
                RP::new(true, true, false, true),
            )
            .unwrap(),
        ],
        pc: I::new(0),
        sp: V::new(0x880),
        status,
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    machine.reset();
    machine.set_trap_vector(I::new(0x100)).unwrap();
    machine
}

fn trap_image_machine(
    config: C,
    program: &[u8],
    handler: &[u8],
    status: u64,
) -> LazalithMachine<lazalith_devices::NoDevice> {
    let mut rom = vec![0u8; 0x110];
    rom[..program.len()].copy_from_slice(program);
    let handler_end = 0x100 + handler.len();
    rom.get_mut(0x100..handler_end)
        .unwrap()
        .copy_from_slice(handler);
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions: vec![
            MemoryRegion::rom(config, P::new(0), &rom, RP::new(false, false, true, true)).unwrap(),
            MemoryRegion::ram(
                config,
                P::new(0x800),
                0x100,
                RP::new(true, true, false, true),
            )
            .unwrap(),
        ],
        pc: I::new(0),
        sp: V::new(0x880),
        status,
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    machine.reset();
    machine.set_trap_vector(I::new(0x100)).unwrap();
    machine
}

#[test]
fn fault_causes_preserve_width_division_address_alignment_and_decode_categories() {
    for config in [C::lz32(), C::lz64()] {
        for (opcode, cause) in [
            (0x15, TrapCause::DivideByZero),
            (0x17, TrapCause::DivideByZero),
        ] {
            let program = [opcode, 0, 0, 0, 0, 0, 0, 0];
            let decoded = decode(config, &program).unwrap();
            assert_eq!(decoded.opcode(), Opcode::try_from(opcode).unwrap());
            let mut machine = trap_program_machine(config, &program, 0);
            let result = machine.run(1).unwrap();
            assert_eq!(result.trap.unwrap().cause, cause);
        }
        for (opcode, cause) in [
            (0x16, TrapCause::DivisionOverflow),
            (0x18, TrapCause::DivisionOverflow),
        ] {
            let mut program = if config.word_width() == lazalith_types::WordWidth::W32 {
                li(0, i32::MIN).to_vec()
            } else {
                let mut program = li(0, 1).to_vec();
                program.extend_from_slice(&li(1, 63));
                program.extend_from_slice(&[0x24, 0, 1, 0, 0, 0, 0, 0]);
                program
            };
            program.extend_from_slice(&li(1, -1));
            program.extend_from_slice(&[opcode, 0, 1, 0, 0, 0, 0, 0]);
            let decoded = decode(config, &program[program.len() - 8..]).unwrap();
            assert_eq!(decoded.opcode(), Opcode::try_from(opcode).unwrap());
            let mut machine = trap_program_machine(config, &program, 0);
            let result = machine.run((program.len() / 8) as u64).unwrap();
            assert_eq!(result.trap.unwrap().cause, cause);
        }

        let address_overflow = [0x30, 0x11, 0, 0, 0xff, 0xff, 0xff, 0xff];
        let mut machine = trap_program_machine(config, &address_overflow, 0);
        assert_eq!(
            machine.run(1).unwrap().trap.unwrap().cause,
            TrapCause::AddressOverflow
        );

        let mut alignment = li(1, 1).to_vec();
        alignment.extend_from_slice(&[0x41, 0x10, 0, 0, 0, 0, 0, 0]);
        let mut machine = trap_program_machine(config, &alignment, 0);
        assert_eq!(
            machine.run(2).unwrap().trap.unwrap().cause,
            TrapCause::Alignment
        );

        let mut control = li(1, 1).to_vec();
        control.extend_from_slice(&[0x05, 0x10, 0, 0, 0, 0, 0, 0]);
        let mut machine = trap_program_machine(config, &control, 0);
        assert_eq!(
            machine.run(2).unwrap().trap.unwrap().cause,
            TrapCause::Alignment
        );
    }

    let mut machine = trap_program_machine(C::lz32(), &[0x30, 0x10, 0x30, 0, 0, 0, 0, 0], 0);
    assert_eq!(
        machine.run(1).unwrap().trap.unwrap().cause,
        TrapCause::InvalidWidth
    );
}

#[test]
fn ei_and_rfe_defer_then_deliver_the_next_lowest_interrupt() {
    for config in [C::lz32(), C::lz64()] {
        let mut program = vec![0x54, 0, 0, 0, 0, 0, 0, 0];
        program.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let handler = [0x54, 0, 0, 0, 0, 0, 0, 0, 0x53, 0, 0, 0, 0, 0, 0, 0];
        let mut machine = trap_image_machine(config, &program, &handler, 0);
        machine.step().unwrap();
        machine.step().unwrap();
        assert!(machine.architectural_state().status().interrupts_enabled());
        machine.request_interrupt(InterruptId::new(7)).unwrap();
        machine.request_interrupt(InterruptId::new(1)).unwrap();

        let first = machine.run(1).unwrap();
        assert_eq!(first.executed, 0);
        assert_eq!(first.trap.unwrap().interrupt, Some(InterruptId::new(1)));
        assert_eq!(machine.architectural_state().pc(), I::new(0x100));
        machine.step().unwrap();
        assert_eq!(machine.architectural_state().pc(), I::new(0x108));
        machine.step().unwrap();
        assert!(!machine.trap_controller().has_active_frame());
        assert_eq!(machine.architectural_state().pc(), I::new(0x10));
        assert!(machine.architectural_state().status().interrupts_enabled());

        let second = machine.run(1).unwrap();
        assert_eq!(second.executed, 0);
        assert_eq!(second.trap.unwrap().interrupt, Some(InterruptId::new(7)));
        assert_eq!(machine.architectural_state().pc(), I::new(0x100));
    }
}

#[test]
fn rfe_releases_retained_fault_diagnostic() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_image_machine(config, &[0xff; 8], &[0x53, 0, 0, 0, 0, 0, 0, 0], 0);
        machine.run(1).unwrap();
        assert!(machine.last_trap_fault().is_some());
        machine.step().unwrap();
        assert!(!machine.trap_controller().has_active_frame());
        assert!(machine.last_trap_fault().is_none());
        assert_eq!(machine.architectural_state().pc(), I::new(0));
    }
}

#[test]
fn interrupts_deliver_at_every_executable_boundary_lowest_first_and_defer_in_frame() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0x52, 0, 0, 0, 0, 0, 0, 0],
            16,
        );
        machine.set_trap_vector(I::new(0x100)).unwrap();
        assert_eq!(machine.state(), MachineState::Reset);
        assert!(machine.request_interrupt(InterruptId::new(7)).unwrap());
        assert!(!machine.request_interrupt(InterruptId::new(7)).unwrap());
        assert!(machine.request_interrupt(InterruptId::new(1)).unwrap());

        let delivered = machine.step().unwrap();
        assert_eq!(
            delivered,
            lazalith_machine::MachineEvent::Trapped {
                event: lazalith_machine::TrapEvent {
                    cause: TrapCause::ExternalInterrupt,
                    payload: 1,
                    resume_pc: I::new(0),
                    interrupt: Some(InterruptId::new(1)),
                },
            }
        );
        assert_eq!(machine.architectural_state().pc(), I::new(0x100));
        assert_eq!(machine.interrupts().peek(), Some(InterruptId::new(7)));
        assert!(machine.trap_controller().has_active_frame());

        let deferred = machine.step().unwrap();
        assert_eq!(deferred, lazalith_machine::MachineEvent::Halted);
        assert_eq!(machine.architectural_state().pc(), I::new(0x108));
        assert_eq!(machine.interrupts().peek(), Some(InterruptId::new(7)));
        assert_eq!(machine.state(), MachineState::Halted);
        assert!(matches!(
            machine.run(1),
            Err(MachineError::InvalidTransition {
                operation: MachineOperation::Run,
                state: MachineState::Halted
            })
        ));
        assert_eq!(machine.interrupts().peek(), Some(InterruptId::new(7)));
        machine.reset();
        assert!(machine.interrupts().is_empty());
        assert!(!machine.trap_controller().has_active_frame());
        assert_eq!(machine.state(), MachineState::Reset);
    }
}

#[test]
fn interrupt_delivery_requires_an_enabled_interrupt_at_an_executable_boundary() {
    for config in [C::lz32(), C::lz64()] {
        let mut masked = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0, 0],
            0,
        );
        masked.set_trap_vector(I::new(0x100)).unwrap();
        masked.request_interrupt(InterruptId::new(3)).unwrap();
        assert!(matches!(
            masked.step().unwrap(),
            lazalith_machine::MachineEvent::Stepped { .. }
        ));
        assert_eq!(masked.interrupts().peek(), Some(InterruptId::new(3)));
        assert!(!masked.trap_controller().has_active_frame());

        let mut running = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0, 0],
            16,
        );
        running.set_trap_vector(I::new(0x100)).unwrap();
        running.request_interrupt(InterruptId::new(3)).unwrap();
        let run = running.run(1).unwrap();
        assert_eq!(run.executed, 0);
        assert_eq!(
            run.trap.map(|event| event.cause),
            Some(TrapCause::ExternalInterrupt)
        );

        let mut paused = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0, 0],
            16,
        );
        paused.set_trap_vector(I::new(0x100)).unwrap();
        assert_eq!(paused.run(1).unwrap().executed, 1);
        paused.pause().unwrap();
        assert_eq!(paused.state(), MachineState::Paused);
        paused.request_interrupt(InterruptId::new(3)).unwrap();
        assert!(matches!(
            paused.step().unwrap(),
            lazalith_machine::MachineEvent::Trapped { .. }
        ));
        assert_eq!(paused.interrupts().len(), 0);
    }
}

#[test]
fn masked_interrupts_remain_pending_without_changing_cpu_state() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0x52, 0, 0, 0, 0, 0, 0, 0],
            0,
        );
        machine.set_trap_vector(I::new(0x100)).unwrap();
        machine.request_interrupt(InterruptId::new(4)).unwrap();
        let result = machine.run(1).unwrap();
        assert_eq!(result.executed, 1);
        assert_eq!(result.trap, None);
        assert_eq!(machine.architectural_state().pc(), I::new(8));
        assert!(!machine.architectural_state().status().interrupts_enabled());
        assert_eq!(machine.interrupts().peek(), Some(InterruptId::new(4)));
    }
}

#[test]
fn double_trap_is_terminal_and_retains_the_first_frame() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_machine(
            config,
            [0x50, 0, 0, 0, 0, 0, 0, 0],
            [0x50, 0, 0, 0, 0, 0, 0, 0],
            0,
        );
        machine.set_trap_vector(I::new(0x100)).unwrap();
        let first = machine.run(1).unwrap();
        assert!(first.trap.is_some());
        let first_frame = machine.trap_controller().frame().unwrap().clone();
        let before = machine.architectural_state().clone();

        assert!(matches!(
            machine.step(),
            Err(MachineError::TrapEntry {
            attempt: _,
                original: None,
                failure
            }) if matches!(failure.cause, CpuFaultCause::DoubleTrap)
        ));
        assert_eq!(machine.state(), MachineState::Faulted);
        assert_eq!(machine.architectural_state(), &before);
        assert_eq!(machine.trap_controller().frame(), Some(&first_frame));
        let double = machine.trap_controller().double_trap().unwrap();
        assert_eq!(double.cause(), TrapCause::Syscall);
        assert_eq!(double.second().pc(), before.pc());
    }
}

#[test]
fn failed_trap_target_fetch_is_terminal_without_installing_a_frame() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_machine(
            config,
            [0x50, 0, 0, 0, 0, 0, 0, 0],
            [0x52, 0, 0, 0, 0, 0, 0, 0],
            0,
        );
        machine.set_trap_vector(I::new(0x200)).unwrap();
        let before = machine.architectural_state().clone();
        assert!(matches!(
            machine.run(1),
            Err(MachineError::TrapEntry {
            attempt: _,
                original: None,
                failure
            }) if matches!(
                failure.cause,
                CpuFaultCause::TrapEntry(_)
            )
        ));
        assert_eq!(machine.state(), MachineState::Faulted);
        assert_eq!(machine.architectural_state(), &before);
        assert!(!machine.trap_controller().has_active_frame());
    }
}

#[test]
fn failed_external_entry_retains_attempt_and_leaves_request_pending() {
    for config in [C::lz32(), C::lz64()] {
        let mut machine = trap_machine(
            config,
            [0, 0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 0, 0],
            16,
        );
        machine.set_trap_vector(I::new(0x200)).unwrap();
        machine.request_interrupt(InterruptId::new(5)).unwrap();
        let before = machine.architectural_state().clone();
        assert!(matches!(
            machine.run(1),
            Err(MachineError::TrapEntry {
                attempt,
                original: None,
                failure
            }) if attempt.cause() == TrapCause::ExternalInterrupt
                && attempt.payload() == 5
                && attempt.resume_pc() == before.pc()
                && matches!(failure.cause, CpuFaultCause::TrapEntry(_))
        ));
        assert_eq!(machine.state(), MachineState::Faulted);
        assert_eq!(machine.architectural_state(), &before);
        assert_eq!(machine.interrupts().peek(), Some(InterruptId::new(5)));
        assert!(!machine.trap_controller().has_active_frame());
    }
}
