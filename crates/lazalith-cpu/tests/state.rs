use core::error::Error;
use lazalith_cpu::{
    ArchitecturalState, ControlStateError, DebugState, ExecutionState, Privilege, SpecialRegister,
    validate_pc, validate_sp,
};
use lazalith_types::{ArchitectureConfig, InstructionAddress as Pc, VirtualAddress as Sp};

fn state(config: ArchitectureConfig) -> ArchitecturalState {
    ArchitecturalState::new(config, Pc::new(0x104), Sp::new(0x1000), 0x3f).unwrap()
}

#[test]
fn state_keeps_general_special_execution_and_debug_domains_separate() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut state = state(config);
        assert_eq!(state.config(), config);
        assert_eq!(state.privilege(), Privilege::User);
        assert_eq!(state.status().bits(), 0x3f);
        for raw in 0..16 {
            state.write_register_raw(raw, u64::MAX).unwrap();
        }
        assert_eq!(state.pc(), Pc::new(0x104));
        assert_eq!(state.sp(), Sp::new(0x1000));
        let registers = state.registers().clone();
        state.restore_control(Pc::new(4), Sp::new(0), 0).unwrap();
        assert_eq!(state.registers(), &registers);
        assert_eq!(state.privilege(), Privilege::Supervisor);
        let snapshot = state.clone();
        let execution = ExecutionState::Halted;
        let debug = DebugState { single_step: true };
        assert_ne!(execution, ExecutionState::Running);
        assert_ne!(debug, DebugState::default());
        assert_eq!(state, snapshot);
        assert_eq!(state.write_register_raw(255, 0).unwrap_err().input(), 255);
        assert_eq!(state, snapshot);
    }
}

#[test]
fn pc_and_sp_accept_exact_width_and_alignment_boundaries_without_fetch_policy() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let max = config.word_width().mask();
        for input in [
            0,
            4,
            8,
            0x100000000,
            0x8000000000000000,
            max - 7,
            max - 3,
            max,
            u64::MAX,
        ] {
            assert_eq!(
                validate_pc(config, Pc::new(input)).is_ok(),
                input <= max && input % 4 == 0
            );
            assert_eq!(
                validate_sp(config, Sp::new(input)).is_ok(),
                input <= max && input % u64::from(config.word_bytes()) == 0
            );
        }
        let mut state = state(config);
        state.set_pc(Pc::new(max - 3)).unwrap();
        state
            .set_sp(Sp::new(max - u64::from(config.word_bytes()) + 1))
            .unwrap();
        assert_eq!(state.pc().as_u64(), max - 3);
    }
}

#[test]
fn every_invalid_control_update_is_atomic_and_retains_rejected_value() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut state = state(config);
        state.write_register_raw(0, 42).unwrap();
        let before = state.clone();
        for input in [1, 2, 3, 5, 7, 0x100000001, u64::MAX] {
            for register in [SpecialRegister::Pc, SpecialRegister::Sp] {
                let error = match register {
                    SpecialRegister::Pc => state.set_pc(Pc::new(input)).unwrap_err(),
                    SpecialRegister::Sp => state.set_sp(Sp::new(input)).unwrap_err(),
                };
                match error {
                    ControlStateError::Range {
                        register: actual,
                        input: retained,
                        ..
                    } => {
                        assert_eq!(actual, register);
                        assert_eq!(retained, input);
                        assert!(input > config.word_width().mask());
                        assert!(error.source().is_some());
                    }
                    ControlStateError::Alignment {
                        register: actual,
                        input: retained,
                        width,
                        ..
                    } => {
                        assert_eq!(actual, register);
                        assert_eq!(retained, input);
                        assert_eq!(width, config.word_width());
                        assert!(error.source().is_none());
                    }
                    _ => panic!("unexpected error"),
                }
                assert!(!error.to_string().is_empty());
                assert_eq!(state, before);
            }
        }
        for (pc, sp, status) in [(1, 0, 0), (4, 1, 0), (4, 0, 64), (4, 0, u64::MAX)] {
            assert!(
                state
                    .restore_control(Pc::new(pc), Sp::new(sp), status)
                    .is_err()
            );
            assert!(ArchitecturalState::new(config, Pc::new(pc), Sp::new(sp), status).is_err());
            assert_eq!(state, before);
        }
    }
}

#[test]
fn status_scaffold_rejects_all_reserved_bits_without_masking() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        for bits in 0..64 {
            let state = ArchitecturalState::new(config, Pc::new(0), Sp::new(0), bits).unwrap();
            assert_eq!(state.status().bits(), bits);
        }
        for bit in 6..64 {
            let input = (1u64 << bit) | 0x3f;
            let error = ArchitecturalState::new(config, Pc::new(0), Sp::new(0), input).unwrap_err();
            let ControlStateError::Status(source) = error else {
                panic!("unexpected error")
            };
            assert_eq!(source.input, input);
            assert_eq!(source.width, config.word_width());
            assert!(
                error
                    .source()
                    .unwrap()
                    .downcast_ref::<lazalith_cpu::InvalidStatus>()
                    .is_some()
            );
            assert!(error.to_string().contains("reserved bits"));
        }
    }
}
