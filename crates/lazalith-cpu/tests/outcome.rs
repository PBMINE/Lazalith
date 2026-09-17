use core::{convert::Infallible, error::Error};
use lazalith_cpu::{
    ArchitecturalState, ControlTarget as Target, ExecutionOutcome as Outcome, ExecutionState,
    OutcomeApplication as Applied, OutcomeErrorKind, StackEffect, TrapRequest, checked_next_pc,
    checked_return_sp, prepare_outcome,
};
use lazalith_types::{ArchitectureConfig, InstructionAddress as Pc, VirtualAddress as Sp};

fn state(config: ArchitectureConfig, pc: u64, sp: u64) -> ArchitecturalState {
    let mut state = ArchitecturalState::new(config, Pc::new(pc), Sp::new(sp), 31).unwrap();
    for raw in 0..16 {
        state
            .write_register_raw(raw, u64::MAX - u64::from(raw))
            .unwrap();
    }
    state
}

#[test]
fn continue_jump_and_relative_targets_have_one_checked_pc_policy() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        for (outcome, target) in [
            (Outcome::Continue, 0x108),
            (Outcome::Jump(Target::Relative(0)), 0x108),
            (Outcome::Jump(Target::Relative(-2)), 0x100),
            (Outcome::Jump(Target::Relative(-1)), 0x104),
            (Outcome::Jump(Target::Absolute(Pc::new(4))), 4),
            (
                Outcome::Jump(Target::Absolute(Pc::new(config.word_width().mask() - 3))),
                config.word_width().mask() - 3,
            ),
        ] {
            let mut state = state(config, 0x100, 0x1000);
            let before = state.clone();
            let mut execution = ExecutionState::Running;
            let plan = prepare_outcome(&mut state, &mut execution, outcome).unwrap();
            assert_eq!(plan.next_pc(), Pc::new(0x108));
            assert_eq!(plan.destination(), Pc::new(target));
            assert_eq!(plan.stack_effect(), None);
            assert_eq!(
                plan.commit::<Infallible>(|_| panic!("unexpected stack effect")),
                Ok(Applied::Continue)
            );
            assert_eq!(state.pc(), Pc::new(target));
            assert_eq!(state.sp(), before.sp());
            assert_eq!(state.status(), before.status());
            assert_eq!(state.registers(), before.registers());
        }
    }
}

#[test]
fn call_and_return_use_mode_sized_stack_and_commit_only_after_transaction() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        for target in [Target::Relative(2), Target::Absolute(Pc::new(0x110))] {
            let mut state = state(config, 0x100, 0x1000);
            let before = state.clone();
            let mut execution = ExecutionState::Running;
            let mut bytes = [0xa5; 8];
            let size = usize::from(config.word_bytes());
            let plan = prepare_outcome(&mut state, &mut execution, Outcome::Call(target)).unwrap();
            assert_eq!(plan.destination(), Pc::new(0x110));
            plan.commit::<Infallible>(|effect| {
                let StackEffect::Push {
                    address,
                    return_pc,
                    size: data_size,
                } = effect
                else {
                    panic!("expected push")
                };
                assert_eq!(address.as_u64(), 0x1000 - size as u64);
                assert_eq!(return_pc.as_u64(), 0x108);
                assert_eq!(usize::from(data_size.bytes()), size);
                bytes[..size].copy_from_slice(&return_pc.as_u64().to_le_bytes()[..size]);
                Ok(())
            })
            .unwrap();
            assert_eq!(state.sp().as_u64(), 0x1000 - size as u64);
            assert_eq!(state.pc().as_u64(), 0x110);
            assert_eq!(&bytes[..size], &0x108u64.to_le_bytes()[..size]);
            assert_eq!(&bytes[size..], &before_bytes()[size..]);
            let pushed = bytes;
            let mut loaded = [0; 8];
            loaded[..size].copy_from_slice(&bytes[..size]);
            let target = Pc::new(u64::from_le_bytes(loaded));
            assert_eq!(
                checked_return_sp(config, state.sp()).unwrap(),
                Sp::new(0x1000)
            );
            prepare_outcome(&mut state, &mut execution, Outcome::Return(target)).unwrap().commit::<Infallible>(|effect| {
                assert!(matches!(effect, StackEffect::Pop { address, return_pc, size: data_size } if address.as_u64() == 0x1000 - size as u64 && return_pc == target && usize::from(data_size.bytes()) == size));
                Ok(())
            }).unwrap();
            assert_eq!(bytes, pushed);
            assert_eq!(state.pc(), Pc::new(0x108));
            assert_eq!(state.sp(), before.sp());
            assert_eq!(state.status(), before.status());
            assert_eq!(state.registers(), before.registers());
        }
    }
}

fn before_bytes() -> [u8; 8] {
    [0xa5; 8]
}

#[test]
fn dropped_plans_and_failed_transactions_leave_all_cpu_state_unchanged() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        for outcome in [
            Outcome::Continue,
            Outcome::Call(Target::Relative(2)),
            Outcome::Return(Pc::new(8)),
            Outcome::Trap(TrapRequest::Syscall),
            Outcome::Halt,
        ] {
            let mut state = state(config, 0x100, 0x1000);
            let before = state.clone();
            let mut execution = ExecutionState::Running;
            {
                let _plan = prepare_outcome(&mut state, &mut execution, outcome).unwrap();
            }
            assert_eq!(state, before);
            assert_eq!(execution, ExecutionState::Running);
            if matches!(outcome, Outcome::Call(_) | Outcome::Return(_)) {
                let result = prepare_outcome(&mut state, &mut execution, outcome)
                    .unwrap()
                    .commit(|_| Err(42u8));
                assert_eq!(result, Err(42));
                assert_eq!(state, before);
                assert_eq!(execution, ExecutionState::Running);
            }
        }
    }
}

#[test]
fn syscall_and_software_traps_retain_exact_pre_state_and_typed_resume_payload() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        for request in [
            TrapRequest::Syscall,
            TrapRequest::Software(i32::MIN),
            TrapRequest::Software(-1),
            TrapRequest::Software(0),
            TrapRequest::Software(i32::MAX),
        ] {
            let mut state = state(config, 0x100, 0);
            state
                .restore_control(Pc::new(0x100), Sp::new(0), 63)
                .unwrap();
            let before = state.clone();
            let mut execution = ExecutionState::Running;
            let plan = prepare_outcome(&mut state, &mut execution, Outcome::Trap(request)).unwrap();
            assert_eq!(plan.destination(), before.pc());
            assert_eq!(
                plan.commit::<Infallible>(|_| panic!("trap must not push")),
                Ok(Applied::Trap {
                    request,
                    resume_pc: Pc::new(0x108)
                })
            );
            assert_eq!(state, before);
            assert_eq!(execution, ExecutionState::Running);
        }
    }
}

#[test]
fn halt_advances_once_is_privileged_and_blocks_all_later_outcomes() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut state = state(config, 4, 0);
        let mut execution = ExecutionState::Running;
        assert_eq!(
            prepare_outcome(&mut state, &mut execution, Outcome::Halt)
                .unwrap()
                .commit::<Infallible>(|_| panic!("no stack")),
            Ok(Applied::Halted)
        );
        assert_eq!(state.pc(), Pc::new(12));
        assert_eq!(execution, ExecutionState::Halted);
        let before = state.clone();
        for outcome in [
            Outcome::Continue,
            Outcome::Jump(Target::Relative(0)),
            Outcome::Call(Target::Relative(0)),
            Outcome::Return(Pc::new(0)),
            Outcome::Trap(TrapRequest::Syscall),
            Outcome::Halt,
        ] {
            let error = prepare_outcome(&mut state, &mut execution, outcome)
                .err()
                .unwrap();
            assert_eq!(error.kind, OutcomeErrorKind::Halted);
            assert_eq!(error.outcome, outcome);
            assert_eq!(state, before);
            assert_eq!(execution, ExecutionState::Halted);
        }
        state
            .restore_control(Pc::new(config.word_width().mask() - 7), Sp::new(0), 32)
            .unwrap();
        execution = ExecutionState::Running;
        let before = state.clone();
        let error = prepare_outcome(&mut state, &mut execution, Outcome::Halt)
            .err()
            .unwrap();
        assert_eq!(error.kind, OutcomeErrorKind::PrivilegeViolation);
        assert_eq!(state, before);
    }
}

#[test]
fn next_pc_overflow_precedes_every_instruction_specific_effect() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let max = config.word_width().mask();
        for pc in [max - 7, max - 3] {
            for outcome in [
                Outcome::Continue,
                Outcome::Jump(Target::Absolute(Pc::new(1))),
                Outcome::Call(Target::Relative(i32::MIN)),
                Outcome::Return(Pc::new(1)),
                Outcome::Trap(TrapRequest::Software(-1)),
                Outcome::Halt,
            ] {
                let mut state = state(config, pc, 0);
                let before = state.clone();
                let mut execution = ExecutionState::Running;
                let error = prepare_outcome(&mut state, &mut execution, outcome)
                    .err()
                    .unwrap();
                assert!(
                    matches!(error.kind, OutcomeErrorKind::Width(lazalith_types::WidthError::AddressOffsetOutOfRange { base, delta: 8, .. }) if base == pc)
                );
                assert_eq!(error.pc, Pc::new(pc));
                assert_eq!(error.outcome, outcome);
                assert!(error.source().unwrap().source().is_some());
                assert!(error.to_string().contains("PC"));
                assert_eq!(state, before);
                assert_eq!(execution, ExecutionState::Running);
            }
        }
        assert_eq!(
            checked_next_pc(config, Pc::new(max - 11)).unwrap(),
            Pc::new(max - 3)
        );
        assert!(checked_next_pc(config, Pc::new(1)).is_err());
    }
}

#[test]
fn target_stack_and_relative_boundaries_reject_without_mutation_in_design_order() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let max = config.word_width().mask();
        for (pc, sp, outcome) in [
            (0, 0, Outcome::Call(Target::Absolute(Pc::new(1)))),
            (0, 0, Outcome::Call(Target::Relative(0))),
            (
                0,
                max - u64::from(config.word_bytes()) + 1,
                Outcome::Return(Pc::new(1)),
            ),
            (0, 0, Outcome::Jump(Target::Relative(i32::MIN))),
            (0, 0, Outcome::Return(Pc::new(1))),
            (0, 8, Outcome::Jump(Target::Absolute(Pc::new(u64::MAX)))),
        ] {
            let mut state = state(config, pc, sp);
            let before = state.clone();
            let mut execution = ExecutionState::Running;
            let error = prepare_outcome(&mut state, &mut execution, outcome)
                .err()
                .unwrap();
            assert_eq!(error.outcome, outcome);
            if matches!(outcome, Outcome::Call(Target::Absolute(_))) {
                assert!(matches!(error.kind, OutcomeErrorKind::Control(_)));
            }
            if matches!(outcome, Outcome::Return(_)) && sp != 0 {
                assert!(matches!(error.kind, OutcomeErrorKind::Width(_)));
            }
            assert_eq!(state, before);
            assert_eq!(execution, ExecutionState::Running);
        }
        for pc in [0, 0x100, max - 11, 0x8000000000000000u64 & max] {
            for displacement in [i32::MIN, -2, -1, 0, 1, i32::MAX] {
                let mut state = state(config, pc, 8);
                let before = state.clone();
                let mut execution = ExecutionState::Running;
                let expected = i128::from(pc) + 8 + i128::from(displacement) * 4;
                let result = prepare_outcome(
                    &mut state,
                    &mut execution,
                    Outcome::Jump(Target::Relative(displacement)),
                );
                if expected >= 0 && expected <= i128::from(max) {
                    let plan = result.unwrap();
                    assert_eq!(plan.destination().as_u64(), expected as u64);
                    plan.commit::<Infallible>(|_| panic!("no stack")).unwrap();
                } else {
                    assert!(result.is_err());
                    assert_eq!(state, before);
                }
            }
        }
    }
}

#[test]
fn boundary_stack_slots_and_untaken_branch_continue_do_not_wrap_or_speculate() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut state = state(config, 0, u64::from(config.word_bytes()));
        let mut execution = ExecutionState::Running;
        prepare_outcome(
            &mut state,
            &mut execution,
            Outcome::Call(Target::Relative(0)),
        )
        .unwrap()
        .commit::<Infallible>(|effect| {
            assert!(matches!(effect, StackEffect::Push { address, .. } if address.as_u64() == 0));
            Ok(())
        })
        .unwrap();
        assert_eq!(state.sp().as_u64(), 0);
        prepare_outcome(&mut state, &mut execution, Outcome::Return(Pc::new(0)))
            .unwrap()
            .commit::<Infallible>(|_| Ok(()))
            .unwrap();
        assert_eq!(state.sp().as_u64(), u64::from(config.word_bytes()));
        prepare_outcome(&mut state, &mut execution, Outcome::Continue)
            .unwrap()
            .commit::<Infallible>(|_| panic!("no stack"))
            .unwrap();
        assert_eq!(state.pc().as_u64(), 8);
        assert!(checked_return_sp(config, Sp::new(1)).is_err());
        if config.word_bits() == 32 {
            assert!(checked_return_sp(config, Sp::new(0x100000000)).is_err());
            assert!(checked_next_pc(config, Pc::new(0x100000000)).is_err());
        }
    }
}
