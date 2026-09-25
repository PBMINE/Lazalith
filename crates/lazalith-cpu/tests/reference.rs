mod support;
use lazalith_cpu::{CpuFaultCause as Cause, ExecutionState, OutcomeApplication, TrapRequest};
use lazalith_isa::{DataSize, Opcode, Operand};
use support::*;

#[test]
fn tiny_fetched_call_sequence_runs_both_modes() {
    for config in MODES {
        let mut ram = Ram::default();
        ram.code(0, config, Opcode::Li, &[r(0), Operand::Immediate(7)]);
        ram.code(8, config, Opcode::Call, &[Operand::Immediate(2)]);
        ram.code(16, config, Opcode::Halt, &[]);
        ram.code(
            24,
            config,
            Opcode::Addi,
            &[r(0), r(0), Operand::Immediate(-2)],
        );
        ram.code(32, config, Opcode::Ret, &[]);
        let mut cpu = cpu(config, 0, 256, 0, &[]);
        for pc in [8, 24, 32, 16] {
            assert_eq!(cpu.step(&mut ram), Ok(OutcomeApplication::Continue));
            assert_eq!(cpu.architectural_state().pc().as_u64(), pc);
        }
        assert_eq!(cpu.architectural_state().registers().read_raw(0), Ok(5));
        assert_eq!(cpu.architectural_state().sp().as_u64(), 256);
        assert_eq!(
            ram.word(
                256 - usize::from(config.word_bytes()),
                usize::from(config.word_bytes())
            ),
            16
        );
        assert_eq!(cpu.step(&mut ram), Ok(OutcomeApplication::Halted));
        let before = cpu.architectural_state().clone();
        let memory = ram.clone();
        assert!(matches!(
            cpu.step(&mut ram).unwrap_err().cause,
            Cause::Halted
        ));
        assert_eq!(cpu.architectural_state(), &before);
        assert_eq!(ram, memory);
        assert_eq!(cpu.execution_state(), ExecutionState::Halted);
    }
}

#[test]
fn byte_fetch_arithmetic_and_memory_use_shared_width() {
    for config in MODES {
        let mut ram = Ram::default();
        let mut cpu = cpu(
            config,
            0,
            256,
            31,
            &[(1, config.word_width().mask()), (2, 1), (3, 128)],
        );
        cpu.step_bytes(&[0x10, 0x10, 2, 0, 0, 0, 0, 0], &mut ram)
            .unwrap();
        assert_eq!(cpu.architectural_state().registers().read_raw(0), Ok(0));
        assert_eq!(cpu.architectural_state().status().bits(), 22);
        let mem = Operand::Memory {
            base: lazalith_types::RegisterIndex::try_from(3).unwrap(),
            displacement: -4,
        };
        cpu.execute(
            &instruction(
                config,
                Opcode::St,
                &[r(1), mem, Operand::DataSize(DataSize::Byte)],
            ),
            &mut ram,
        )
        .unwrap();
        cpu.execute(
            &instruction(
                config,
                Opcode::Lds,
                &[r(0), mem, Operand::DataSize(DataSize::Byte)],
            ),
            &mut ram,
        )
        .unwrap();
        assert_eq!(
            cpu.architectural_state().registers().read_raw(0),
            Ok(config.word_width().mask())
        );
        assert_eq!(cpu.architectural_state().pc().as_u64(), 24);
        assert_eq!(cpu.architectural_state().status().bits(), 22);
    }
}

#[test]
fn faults_do_not_publish_candidate_or_memory_effects() {
    for config in MODES {
        for (opcode, operands, registers, sp) in [
            (Opcode::Divu, vec![r(0), r(1), r(2)], vec![(1, 9)], 256),
            (Opcode::Callr, vec![r(1)], vec![(1, 3)], 0),
            (Opcode::Ret, vec![], vec![], 128),
            (Opcode::Setsp, vec![r(1)], vec![(1, 3)], 256),
        ] {
            let mut ram = Ram::default();
            ram.put(128, &3u64.to_le_bytes());
            let mut cpu = cpu(config, 0, sp, 31, &registers);
            let before = cpu.architectural_state().clone();
            let memory = ram.clone();
            let error = cpu
                .execute(&instruction(config, opcode, &operands), &mut ram)
                .unwrap_err();
            assert_eq!(error.pc.as_u64(), 0);
            assert_eq!(error.opcode, Some(opcode.as_u8()));
            assert_eq!(cpu.architectural_state(), &before);
            assert_eq!(ram, memory);
            assert_eq!(cpu.execution_state(), ExecutionState::Running);
        }
        let mut ram = Ram::default();
        ram.fail = true;
        let mut cpu = cpu(config, 0, 256, 31, &[]);
        let before = cpu.architectural_state().clone();
        let memory = ram.clone();
        assert!(matches!(
            cpu.execute(
                &instruction(config, Opcode::Call, &[Operand::Immediate(0)]),
                &mut ram
            )
            .unwrap_err()
            .cause,
            Cause::Memory {
                source: MemoryError::Transaction,
                ..
            }
        ));
        assert_eq!(cpu.architectural_state(), &before);
        assert_eq!(ram, memory);
    }
}

#[test]
fn next_pc_is_checked_before_reads_and_stack_operations() {
    for config in MODES {
        for (opcode, operands) in [
            (
                Opcode::Ldz,
                vec![
                    r(0),
                    Operand::Memory {
                        base: lazalith_types::RegisterIndex::try_from(1).unwrap(),
                        displacement: 0,
                    },
                    Operand::DataSize(DataSize::Byte),
                ],
            ),
            (Opcode::Ret, vec![]),
            (Opcode::Call, vec![Operand::Immediate(0)]),
            (Opcode::Halt, vec![]),
            (Opcode::Rfe, vec![]),
        ] {
            let mut ram = Ram::default();
            ram.device = true;
            let mut cpu = cpu(config, config.word_width().mask() - 7, 256, 31, &[]);
            let before = cpu.architectural_state().clone();
            let memory = ram.clone();
            assert!(matches!(
                cpu.execute(&instruction(config, opcode, &operands), &mut ram)
                    .unwrap_err()
                    .cause,
                Cause::NextPc(_)
            ));
            assert_eq!(cpu.architectural_state(), &before);
            assert_eq!(ram, memory);
        }
    }
}

#[test]
fn traps_are_events_and_controller_instructions_are_not_emulated() {
    for config in MODES {
        for status in [0, 32] {
            for (opcode, operands, request) in [
                (Opcode::Syscall, vec![], TrapRequest::Syscall),
                (
                    Opcode::Trap,
                    vec![Operand::Immediate(i32::MIN)],
                    TrapRequest::Software(i32::MIN),
                ),
            ] {
                let mut cpu = cpu(config, 0, 256, status, &[]);
                let before = cpu.architectural_state().clone();
                let mut ram = Ram::default();
                assert_eq!(
                    cpu.execute(&instruction(config, opcode, &operands), &mut ram),
                    Ok(OutcomeApplication::Trap {
                        request,
                        resume_pc: lazalith_types::InstructionAddress::new(8)
                    })
                );
                assert_eq!(cpu.architectural_state(), &before);
            }
            for (opcode, operands) in [
                (Opcode::Rfe, vec![]),
                (
                    Opcode::Csrr,
                    vec![r(0), Operand::Control(lazalith_isa::ControlRegister::Tvec)],
                ),
            ] {
                let mut cpu = cpu(config, 0, 256, status, &[]);
                let before = cpu.architectural_state().clone();
                let error = cpu
                    .execute(&instruction(config, opcode, &operands), &mut Ram::default())
                    .unwrap_err();
                if status == 32 {
                    assert!(matches!(error.cause, Cause::PrivilegeViolation));
                } else {
                    assert!(matches!(error.cause, Cause::Control(_)));
                }
                assert_eq!(cpu.architectural_state(), &before);
            }
            let operands = vec![Operand::Control(lazalith_isa::ControlRegister::Tvec), r(0)];
            let mut cpu = cpu(config, 0, 256, status, &[(0, 0)]);
            if status == 32 {
                assert!(matches!(
                    cpu.execute(
                        &instruction(config, Opcode::Csrw, &operands),
                        &mut Ram::default()
                    )
                    .unwrap_err()
                    .cause,
                    Cause::PrivilegeViolation
                ));
            } else {
                assert_eq!(
                    cpu.execute(
                        &instruction(config, Opcode::Csrw, &operands),
                        &mut Ram::default()
                    ),
                    Ok(OutcomeApplication::Continue)
                );
                assert_eq!(cpu.trap_controller().tvec().unwrap().as_u64(), 0);
            }
        }
    }
}
