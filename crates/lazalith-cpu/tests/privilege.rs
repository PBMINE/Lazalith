#[macro_use]
mod support;

use lazalith_cpu::{
    CpuFault, CpuFaultCause, EngineFault, ExecutionContextId, ExecutionEngine, OutcomeApplication,
    Privilege, ReferenceInterpreter, TrapCause,
};
use lazalith_isa::{ControlRegister, Opcode, Operand};
use support::{MODES, Ram, cpu, instruction, r};

#[test]
fn every_privileged_operation_rejects_user_before_any_effect() {
    for config in MODES {
        for status in [0x3f, 0x20] {
            let mut cpu = cpu(config, 0, 0x100, status, &[(1, 0x1234)]);
            let mut ram = Ram::default();
            for (opcode, operands) in [
                (Opcode::Halt, vec![]),
                (Opcode::Ei, vec![]),
                (Opcode::Di, vec![]),
                (Opcode::Rfe, vec![]),
                (
                    Opcode::Csrr,
                    vec![r(0), Operand::Control(ControlRegister::Tvec)],
                ),
                (
                    Opcode::Csrw,
                    vec![Operand::Control(ControlRegister::Tvec), r(1)],
                ),
            ] {
                let before = cpu.architectural().clone();
                let error = ReferenceInterpreter::new()
                    .execute(&mut cpu, &instruction(config, opcode, &operands), &mut ram)
                    .unwrap_err()
                    .into_guest()
                    .expect("the reference interpreter never declines");
                assert_eq!(error.cause, CpuFaultCause::PrivilegeViolation);
                assert_eq!(cpu.architectural(), &before);
                assert_eq!(ram.reads, 0);
                assert_eq!(ram.writes, 0);
            }
        }
    }
}

#[test]
fn syscall_and_software_trap_are_legal_in_both_modes_without_early_transition() {
    for config in MODES {
        for (status, privilege) in [(0x20, Privilege::User), (0, Privilege::Supervisor)] {
            for (opcode, operands, request, resume) in [
                (
                    Opcode::Syscall,
                    vec![],
                    lazalith_cpu::TrapRequest::Syscall,
                    8,
                ),
                (
                    Opcode::Trap,
                    vec![Operand::Immediate(-1)],
                    lazalith_cpu::TrapRequest::Software(-1),
                    8,
                ),
            ] {
                let mut cpu = cpu(config, 0, 0x100, status, &[]);
                let before = cpu.architectural().clone();
                assert_outcome!(
                    ReferenceInterpreter::new().execute(
                        &mut cpu,
                        &instruction(config, opcode, &operands),
                        &mut Ram::default()
                    ),
                    OutcomeApplication::Trap {
                        request,
                        resume_pc: lazalith_types::InstructionAddress::new(resume)
                    }
                );
                assert_eq!(cpu.architectural().privilege(), privilege);
                assert_eq!(cpu.architectural(), &before);
            }
        }
    }
}

#[test]
fn trap_entry_and_rfe_restore_user_and_interrupt_state_without_restoring_registers() {
    for config in MODES {
        let mut cpu = cpu(config, 0x20, 0x100, 0x3f, &[(3, 0xabcd)]);
        cpu.set_trap_vector(lazalith_types::InstructionAddress::new(0x80))
            .unwrap();
        let mut ram = Ram::default();
        ram.put(0x80, &[0; 8]);
        ram.code(0x28, config, Opcode::Halt, &[]);
        let before = cpu.architectural().clone();
        cpu.enter_syscall(&mut ram, lazalith_types::InstructionAddress::new(0x28))
            .unwrap();
        assert_eq!(cpu.architectural().status().bits(), 0x0f);
        assert_eq!(
            cpu.architectural().status().privilege(),
            Privilege::Supervisor
        );
        let frame = cpu.traps().frame().unwrap();
        assert_eq!(frame.snapshot().pc(), before.pc());
        assert_eq!(frame.snapshot().sp(), before.sp());
        assert_eq!(frame.snapshot().status(), before.status().bits());
        assert_eq!(frame.cause(), TrapCause::Syscall);
        let before_return = cpu.architectural().clone();
        assert!(matches!(
            ReferenceInterpreter::new().execute(
                &mut cpu,
                &instruction(config, Opcode::Rfe, &[]),
                &mut ram
            ),
            Err(EngineFault::Guest(CpuFault {
                cause: CpuFaultCause::Control(_),
                ..
            }))
        ));
        assert_eq!(cpu.architectural(), &before_return);
        let context = ExecutionContextId::new(1).unwrap();
        cpu.traps_mut().set_execution_context(context);
        let admission = cpu.traps_mut().take_syscall_admission().unwrap();
        let completion = admission.complete_checked(0, 0).unwrap();
        assert!(cpu.traps_mut().authorize_syscall_return(&completion));
        assert_outcome!(
            ReferenceInterpreter::new().execute(
                &mut cpu,
                &instruction(config, Opcode::Rfe, &[]),
                &mut ram
            ),
            OutcomeApplication::Continue
        );
        assert_eq!(cpu.architectural().privilege(), Privilege::User);
        assert!(cpu.architectural().status().interrupts_enabled());
        assert_eq!(
            cpu.architectural().pc(),
            lazalith_types::InstructionAddress::new(0x28)
        );
        assert_eq!(cpu.architectural().sp(), before.sp());
        assert_eq!(cpu.architectural().registers().read_raw(3).unwrap(), 0xabcd);
    }
}
