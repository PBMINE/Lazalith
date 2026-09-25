mod support;

use lazalith_cpu::{
    ArchitecturalState, CpuFault, CpuFaultCause, OutcomeApplication, Privilege, TrapCause,
    TrapSnapshot,
};
use lazalith_isa::{ControlRegister, Opcode, Operand};
use support::{MODES, Ram, cpu, instruction, r};

fn assert_snapshot(snapshot: &TrapSnapshot, state: &ArchitecturalState) {
    assert_eq!(snapshot.pc(), state.pc());
    assert_eq!(snapshot.sp(), state.sp());
    assert_eq!(snapshot.status(), state.status().bits());
    for index in 0..16 {
        assert_eq!(
            snapshot.register(lazalith_types::RegisterIndex::try_from(index).unwrap()),
            state.registers().read_raw(index).unwrap()
        );
    }
}

#[test]
fn entry_snapshots_exact_state_and_forces_supervisor_without_memory_writes() {
    for config in MODES {
        let registers = (0..16)
            .map(|index| (index, 0x1000 + u64::from(index)))
            .collect::<Vec<_>>();
        let mut cpu = cpu(config, 0x20, 0x100, 0x3f, &registers);
        cpu.set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        let before = cpu.architectural_state().clone();
        let mut ram = Ram::default();
        ram.code(0x80, config, Opcode::Nop, &[]);

        cpu.enter_syscall(&mut ram, lazalith_types::InstructionAddress::new(0x28))
            .unwrap();

        assert_eq!(ram.writes, 0);
        assert_eq!(cpu.architectural_state().pc().as_u64(), 0x80);
        assert_eq!(cpu.architectural_state().sp(), before.sp());
        assert_eq!(cpu.architectural_state().status().bits(), 0x0f);
        assert_eq!(
            cpu.architectural_state().status().privilege(),
            Privilege::Supervisor
        );
        for index in 0..16 {
            assert_eq!(
                cpu.architectural_state()
                    .registers()
                    .read_raw(index)
                    .unwrap(),
                before.registers().read_raw(index).unwrap()
            );
        }
        let frame = cpu.trap_controller().frame().unwrap();
        assert_snapshot(frame.snapshot(), &before);
        assert_eq!(frame.cause(), TrapCause::Syscall);
        assert_eq!(frame.payload(), 0);
        assert_eq!(frame.resume_pc().as_u64(), 0x28);
        assert_eq!(frame.resume_sp(), before.sp());
        assert_eq!(frame.resume_status(), before.status().bits());
    }
}

#[test]
fn editable_controls_and_rfe_restore_only_control_state() {
    for config in MODES {
        let mut cpu = cpu(config, 0x40, 0x100, 0, &[(3, 0xabcd)]);
        cpu.set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        let mut ram = Ram::default();
        cpu.enter_software(&mut ram, -1, lazalith_types::InstructionAddress::new(0x48))
            .unwrap();
        cpu.write_trap_control(ControlRegister::Epc, 0x90).unwrap();
        cpu.write_trap_control(ControlRegister::Esp, 0x120).unwrap();
        cpu.write_trap_control(ControlRegister::Estatus, 0x31)
            .unwrap();
        assert_eq!(cpu.read_trap_control(ControlRegister::Tvec).unwrap(), 0x80);
        assert_eq!(cpu.read_trap_control(ControlRegister::Epc).unwrap(), 0x90);
        assert_eq!(cpu.read_trap_control(ControlRegister::Esp).unwrap(), 0x120);
        assert_eq!(
            cpu.read_trap_control(ControlRegister::Estatus).unwrap(),
            0x31
        );
        assert_eq!(cpu.read_trap_control(ControlRegister::Tcause).unwrap(), 17);
        let expected_payload = match config.word_width() {
            lazalith_types::WordWidth::W32 => u64::from(u32::MAX),
            lazalith_types::WordWidth::W64 => u64::MAX,
        };
        assert_eq!(
            cpu.read_trap_control(ControlRegister::Tpayload).unwrap(),
            expected_payload
        );
        let frame = cpu.trap_controller().frame().unwrap().clone();
        for (control, value) in [
            (ControlRegister::Epc, 2),
            (ControlRegister::Esp, 2),
            (ControlRegister::Estatus, 0x40),
        ] {
            assert!(cpu.write_trap_control(control, value).is_err());
            assert_eq!(cpu.trap_controller().frame(), Some(&frame));
        }
        assert!(matches!(
            cpu.write_trap_control(ControlRegister::Tcause, 0),
            Err(lazalith_cpu::ControlStateError::InvalidControlState { .. })
        ));
        assert!(matches!(
            cpu.write_trap_control(ControlRegister::Tpayload, 0),
            Err(lazalith_cpu::ControlStateError::InvalidControlState { .. })
        ));
        assert_eq!(cpu.trap_controller().frame(), Some(&frame));

        assert_eq!(
            cpu.execute(&instruction(config, Opcode::Rfe, &[]), &mut ram),
            Ok(OutcomeApplication::Continue)
        );
        assert_eq!(cpu.architectural_state().pc().as_u64(), 0x90);
        assert_eq!(cpu.architectural_state().sp().as_u64(), 0x120);
        assert_eq!(cpu.architectural_state().status().bits(), 0x31);
        assert_eq!(cpu.architectural_state().privilege(), Privilege::User);
        assert_eq!(
            cpu.architectural_state().registers().read_raw(3).unwrap(),
            0xabcd
        );
        assert!(cpu.trap_controller().frame().is_none());
    }
}

#[test]
fn failed_entry_validation_is_atomic() {
    for config in MODES {
        let mut missing = cpu(config, 0x20, 0x100, 0x20, &[(1, 7)]);
        let before = missing.architectural_state().clone();
        let mut ram = Ram::default();
        let error = missing
            .enter_fault(
                &mut ram,
                TrapCause::Unmapped,
                lazalith_types::InstructionAddress::new(0x20),
            )
            .unwrap_err();
        assert!(matches!(
            error.cause,
            CpuFaultCause::Control(lazalith_cpu::ControlStateError::InvalidControlState { .. })
        ));
        assert_eq!(missing.architectural_state(), &before);
        assert!(missing.trap_controller().frame().is_none());
        let attempt = missing.trap_controller().failed_entry().unwrap().clone();
        assert_eq!(attempt.cause(), TrapCause::Unmapped);
        assert_eq!(attempt.payload(), 0);
        assert_eq!(attempt.resume_pc(), before.pc());
        assert_snapshot(attempt.snapshot(), &before);
        assert!(missing.trap_controller().is_terminal());
        let repeated = missing
            .enter_external(&mut ram, 7, lazalith_types::InstructionAddress::new(0x24))
            .unwrap_err();
        assert_eq!(repeated.cause, CpuFaultCause::TerminalTrap);
        assert_eq!(missing.trap_controller().failed_entry(), Some(&attempt));

        let mut fetch = cpu(config, 0x20, 0x100, 0x20, &[(1, 7)]);
        fetch
            .set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        ram.put(0x80, &[0; 8]);
        ram.executable = false;
        let before = fetch.architectural_state().clone();
        let error = fetch
            .enter_fault(
                &mut ram,
                TrapCause::Permission,
                lazalith_types::InstructionAddress::new(0x20),
            )
            .unwrap_err();
        assert!(matches!(error.cause, CpuFaultCause::TrapEntry(_)));
        assert_eq!(fetch.architectural_state(), &before);
        assert!(fetch.trap_controller().frame().is_none());
        let attempt = fetch.trap_controller().failed_entry().unwrap();
        assert_eq!(attempt.cause(), TrapCause::Permission);
        assert_snapshot(attempt.snapshot(), &before);
    }
}

#[test]
fn double_trap_retains_first_and_second_contexts_without_nesting() {
    for config in MODES {
        let mut cpu = cpu(config, 0x20, 0x100, 0, &[]);
        cpu.set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        let mut ram = Ram::default();
        cpu.enter_syscall(&mut ram, lazalith_types::InstructionAddress::new(0x28))
            .unwrap();
        let first = cpu.trap_controller().frame().unwrap().clone();
        let before_second = cpu.architectural_state().clone();
        let error = cpu
            .enter_software(&mut ram, 9, lazalith_types::InstructionAddress::new(0x88))
            .unwrap_err();
        assert_eq!(error.cause, CpuFaultCause::DoubleTrap);
        assert_eq!(cpu.architectural_state(), &before_second);
        assert_eq!(cpu.trap_controller().frame(), Some(&first));
        let double = cpu.trap_controller().double_trap().unwrap();
        assert_snapshot(double.second(), &before_second);
        assert_eq!(double.cause(), TrapCause::SoftwareTrap);
        assert_eq!(double.payload(), 9);
        assert_eq!(double.resume_pc().as_u64(), 0x88);
        let retained = double.clone();
        let repeated = cpu
            .enter_fault(
                &mut ram,
                TrapCause::Permission,
                lazalith_types::InstructionAddress::new(0x90),
            )
            .unwrap_err();
        assert_eq!(repeated.cause, CpuFaultCause::TerminalTrap);
        assert_eq!(cpu.trap_controller().double_trap(), Some(&retained));
        for (opcode, operands) in [
            (Opcode::Nop, vec![]),
            (Opcode::Li, vec![r(0), Operand::Immediate(0)]),
            (Opcode::Ei, vec![]),
            (Opcode::Halt, vec![]),
        ] {
            let before = cpu.architectural_state().clone();
            assert!(matches!(
                cpu.execute(&instruction(config, opcode, &operands), &mut ram),
                Err(CpuFault {
                    cause: CpuFaultCause::TerminalTrap,
                    ..
                })
            ));
            assert_eq!(cpu.architectural_state(), &before);
        }
        let before_return = cpu.architectural_state().clone();
        assert!(matches!(
            cpu.execute(&instruction(config, Opcode::Rfe, &[]), &mut ram),
            Err(CpuFault {
                cause: CpuFaultCause::TerminalTrap,
                ..
            })
        ));
        assert_eq!(cpu.architectural_state(), &before_return);
        assert_eq!(cpu.trap_controller().frame(), Some(&first));
    }
}

#[test]
fn external_entry_defers_while_framed_and_never_wakes_halted_execution() {
    for config in MODES {
        let mut cpu = cpu(config, 0x20, 0x100, 0, &[]);
        cpu.set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        let mut ram = Ram::default();
        cpu.enter_syscall(&mut ram, lazalith_types::InstructionAddress::new(0x28))
            .unwrap();
        let frame = cpu.trap_controller().frame().unwrap().clone();
        let before = cpu.architectural_state().clone();
        let deferred = cpu
            .enter_external(&mut ram, 9, lazalith_types::InstructionAddress::new(0x28))
            .unwrap_err();
        assert_eq!(deferred.cause, CpuFaultCause::DeferredInterrupt);
        assert_eq!(cpu.architectural_state(), &before);
        assert_eq!(cpu.trap_controller().frame(), Some(&frame));
        cpu.execute(&instruction(config, Opcode::Halt, &[]), &mut ram)
            .unwrap();
        let halted_pc = cpu.architectural_state().pc();
        let halted = cpu.enter_external(&mut ram, 9, halted_pc);
        assert!(matches!(halted.unwrap_err().cause, CpuFaultCause::Halted));
        assert!(cpu.trap_controller().double_trap().is_none());
        assert!(cpu.trap_controller().failed_entry().is_none());
        assert!(!cpu.trap_controller().is_terminal());
    }
}

#[test]
fn frame_controls_require_a_frame_before_value_validation() {
    for config in MODES {
        let mut cpu = cpu(config, 0, 0x100, 0, &[]);
        for (control, value) in [
            (ControlRegister::Epc, 2),
            (ControlRegister::Esp, 2),
            (ControlRegister::Estatus, 0x40),
        ] {
            assert!(matches!(
                cpu.write_trap_control(control, value),
                Err(lazalith_cpu::ControlStateError::InvalidControlState { .. })
            ));
        }
    }
}

#[test]
fn csrr_and_csrw_use_real_controller_state() {
    for config in MODES {
        let mut cpu = cpu(config, 0, 0x100, 0, &[(1, 0x80)]);
        assert_eq!(
            cpu.execute(
                &instruction(
                    config,
                    Opcode::Csrw,
                    &[Operand::Control(ControlRegister::Tvec), r(1)],
                ),
                &mut Ram::default(),
            ),
            Ok(OutcomeApplication::Continue)
        );
        assert_eq!(cpu.trap_controller().tvec().unwrap().as_u64(), 0x80);
        assert_eq!(cpu.architectural_state().pc().as_u64(), 8);
        assert_eq!(
            cpu.execute(
                &instruction(
                    config,
                    Opcode::Csrr,
                    &[r(2), Operand::Control(ControlRegister::Tvec)],
                ),
                &mut Ram::default(),
            ),
            Ok(OutcomeApplication::Continue)
        );
        assert_eq!(
            cpu.architectural_state().registers().read_raw(2).unwrap(),
            0x80
        );
        assert_eq!(cpu.architectural_state().pc().as_u64(), 16);
        cpu.execute(&instruction(config, Opcode::Halt, &[]), &mut Ram::default())
            .unwrap();
        let halted_pc = cpu.architectural_state().pc();
        assert!(matches!(
            cpu.enter_external(&mut Ram::default(), 1, halted_pc)
                .unwrap_err()
                .cause,
            CpuFaultCause::Halted
        ));
    }
}
