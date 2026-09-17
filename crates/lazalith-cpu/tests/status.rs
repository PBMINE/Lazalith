use lazalith_cpu::{ArchitecturalState, Privilege, StatusRegister};
use lazalith_isa::Condition;
use lazalith_types::{
    ArchitectureConfig, ArithmeticResult, InstructionAddress, VirtualAddress, WordWidth,
};

#[test]
fn every_status_combination_has_exact_bits_and_control_updates_preserve_flags() {
    for width in [WordWidth::W32, WordWidth::W64] {
        for bits in 0..64 {
            let status = StatusRegister::try_from_bits(width, bits).unwrap();
            assert_eq!(status.bits(), bits);
            assert_eq!(status.negative(), bits & 1 != 0);
            assert_eq!(status.zero(), bits & 2 != 0);
            assert_eq!(status.carry(), bits & 4 != 0);
            assert_eq!(status.overflow(), bits & 8 != 0);
            assert_eq!(status.interrupts_enabled(), bits & 16 != 0);
            assert_eq!(
                status.privilege(),
                if bits & 32 != 0 {
                    Privilege::User
                } else {
                    Privilege::Supervisor
                }
            );
            for privilege in [Privilege::Supervisor, Privilege::User] {
                for enabled in [false, true] {
                    let control =
                        u64::from(enabled) * 16 + if privilege == Privilege::User { 32 } else { 0 };
                    assert_eq!(StatusRegister::new(privilege, enabled).bits(), control);
                    let mut changed = status;
                    changed.set_privilege(privilege);
                    changed.set_interrupts_enabled(enabled);
                    assert_eq!(changed.bits(), bits & 15 | control);
                }
            }
        }
    }
}

#[test]
fn all_arithmetic_flag_combinations_replace_only_nzcv() {
    for width in [WordWidth::W32, WordWidth::W64] {
        for before in 0..64 {
            for flags in 0..16 {
                let mut status = StatusRegister::try_from_bits(width, before).unwrap();
                status.update_arithmetic(ArithmeticResult {
                    value: u64::MAX,
                    negative: flags & 1 != 0,
                    zero: flags & 2 != 0,
                    carry: flags & 4 != 0,
                    overflow: flags & 8 != 0,
                });
                assert_eq!(status.bits(), before & 48 | flags);
            }
        }
    }
}

#[test]
fn all_shared_conditions_match_independent_truth_table_for_every_status() {
    for width in [WordWidth::W32, WordWidth::W64] {
        for bits in 0..64 {
            let status = StatusRegister::try_from_bits(width, bits).unwrap();
            let n = bits & 1 != 0;
            let z = bits & 2 != 0;
            let c = bits & 4 != 0;
            let v = bits & 8 != 0;
            let expected = [
                true,
                z,
                !z,
                c,
                !c,
                c || z,
                !(c || z),
                n ^ v,
                !(n ^ v),
                z || (n ^ v),
                !(z || (n ^ v)),
                v,
                !v,
                n,
                !n,
            ];
            for &condition in Condition::ALL {
                assert_eq!(
                    status.matches(condition),
                    expected[condition.as_u8() as usize]
                );
            }
        }
    }
}

#[test]
fn real_width_results_drive_status_without_affecting_other_state() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let w = config.word_width();
        let max = w.mask();
        let sign = 1u64 << (w.bits() - 1);
        let cases = [
            (w.add(max, 1), 6),
            (w.add(sign - 1, 1), 9),
            (w.sub(0, 1), 5),
            (w.sub(sign, 1), 8),
            (w.sub(42, 42), 2),
            (w.mul(max, 2), 1),
            (w.bitand(1, 0), 2),
            (w.bitor(sign, 1), 1),
            (w.bitxor(max, max), 2),
            (w.not(max), 2),
            (w.shl(1, u64::from(w.bits())), 0),
            (w.shr(sign, 1), 0),
            (w.sar(sign, 1), 1),
            (w.div_signed(max - 6, 3).unwrap(), 1),
            (w.rem_signed(max - 6, 3).unwrap(), 1),
            (w.div_unsigned(1, 2).unwrap(), 2),
            (w.rem_unsigned(2, 2).unwrap(), 2),
        ];
        for (result, flags) in cases {
            let mut state = ArchitecturalState::new(
                config,
                InstructionAddress::new(4),
                VirtualAddress::new(8),
                63,
            )
            .unwrap();
            state.write_register_raw(0, 17).unwrap();
            state.update_arithmetic(result);
            assert_eq!(state.status().bits(), 48 | flags);
            assert_eq!(state.pc().as_u64(), 4);
            assert_eq!(state.sp().as_u64(), 8);
            assert_eq!(state.registers().read_raw(0), Ok(17));
            state.set_interrupts_enabled(false);
            assert_eq!(state.status().bits(), 32 | flags);
            let before = state.clone();
            for result in [
                w.div_unsigned(1, 0),
                w.rem_unsigned(1, 0),
                w.div_signed(sign, max),
                w.rem_signed(sign, max),
            ] {
                if let Ok(result) = result {
                    state.update_arithmetic(result);
                }
                assert_eq!(state, before);
            }
        }
    }
}
