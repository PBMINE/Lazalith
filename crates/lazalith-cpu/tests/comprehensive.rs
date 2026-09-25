mod support;

use lazalith_cpu::{
    CpuFaultCause as Cause, ExecutionState, OutcomeApplication, ReferenceInterpreter,
};
use lazalith_isa::{Condition, ControlRegister, DataSize, Opcode, Operand};
use lazalith_types::{ArchitectureConfig, RegisterIndex, WidthError};
use std::error::Error;
use support::*;

fn mem(base: u8, displacement: i32) -> Operand {
    Operand::Memory {
        base: RegisterIndex::try_from(base).unwrap(),
        displacement,
    }
}

fn run(cpu: &mut ReferenceInterpreter, ram: &mut Ram, opcode: Opcode, operands: &[Operand]) {
    let config = cpu.architectural_state().config();
    assert_eq!(
        cpu.execute(&instruction(config, opcode, operands), ram),
        Ok(OutcomeApplication::Continue)
    );
}

fn unchanged(
    cpu: &mut ReferenceInterpreter,
    ram: &mut Ram,
    opcode: Opcode,
    operands: &[Operand],
) -> Cause<MemoryError> {
    let before = cpu.architectural_state().clone();
    let execution = cpu.execution_state();
    let memory = ram.clone();
    let error = cpu
        .execute(&instruction(before.config(), opcode, operands), ram)
        .unwrap_err();
    assert_eq!(error.pc, before.pc());
    assert_eq!(error.opcode, Some(opcode.as_u8()));
    assert_eq!(cpu.architectural_state(), &before);
    assert_eq!(cpu.execution_state(), execution);
    assert_eq!(*ram, memory);
    error.cause
}

#[test]
fn arithmetic_opcodes_match_independent_wide_oracle_with_aliases() {
    for config in MODES {
        let bits = config.word_bits();
        let modulus = 1i128 << bits;
        let mask = config.word_width().mask();
        let sign = 1u64 << (bits - 1);
        let signed = |v: u64| {
            if v & sign == 0 {
                i128::from(v)
            } else {
                i128::from(v) - modulus
            }
        };
        let values = [0, 1, 2, 3, 7, sign - 1, sign, sign + 1, mask - 1, mask];
        for opcode in [
            Opcode::Add,
            Opcode::Sub,
            Opcode::Mul,
            Opcode::Divu,
            Opcode::Divs,
            Opcode::Remu,
            Opcode::Rems,
            Opcode::And,
            Opcode::Or,
            Opcode::Xor,
            Opcode::Shl,
            Opcode::Shr,
            Opcode::Sar,
        ] {
            for a in values {
                for b in values
                    .into_iter()
                    .chain([u64::from(bits), u64::from(bits) + 1])
                {
                    let shift = (b % u64::from(bits)) as u32;
                    let (value, carry, overflow) = match opcode {
                        Opcode::Add => {
                            let sum = u128::from(a) + u128::from(b);
                            let s = signed(a) + signed(b);
                            (
                                sum as u64 & mask,
                                sum > u128::from(mask),
                                s < -(modulus / 2) || s >= modulus / 2,
                            )
                        }
                        Opcode::Sub => {
                            let s = signed(a) - signed(b);
                            (
                                (i128::from(a) - i128::from(b)) as u64 & mask,
                                a < b,
                                s < -(modulus / 2) || s >= modulus / 2,
                            )
                        }
                        Opcode::Mul => {
                            ((u128::from(a) * u128::from(b)) as u64 & mask, false, false)
                        }
                        Opcode::Divu | Opcode::Remu if b == 0 => continue,
                        Opcode::Divs | Opcode::Rems if b == 0 || (a == sign && b == mask) => {
                            continue;
                        }
                        Opcode::Divu => (a / b, false, false),
                        Opcode::Remu => (a % b, false, false),
                        Opcode::Divs => ((signed(a) / signed(b)) as u64 & mask, false, false),
                        Opcode::Rems => ((signed(a) % signed(b)) as u64 & mask, false, false),
                        Opcode::And => (a & b, false, false),
                        Opcode::Or => (a | b, false, false),
                        Opcode::Xor => (a ^ b, false, false),
                        Opcode::Shl => ((u128::from(a) << shift) as u64 & mask, false, false),
                        Opcode::Shr => (a >> shift, false, false),
                        Opcode::Sar => (
                            signed(a).div_euclid(1i128 << shift) as u64 & mask,
                            false,
                            false,
                        ),
                        _ => unreachable!(),
                    };
                    let flags = u64::from(value & sign != 0)
                        | (u64::from(value == 0) << 1)
                        | (u64::from(carry) << 2)
                        | (u64::from(overflow) << 3);
                    for d in [0, 1, 2, 15] {
                        let mut machine =
                            cpu(config, 0x100, 256, 63, &[(0, 99), (1, a), (2, b), (15, 55)]);
                        let mut expected = machine.architectural_state().clone();
                        expected.write_register_raw(d, value).unwrap();
                        expected
                            .restore_control(
                                lazalith_types::InstructionAddress::new(0x108),
                                expected.sp(),
                                48 | flags,
                            )
                            .unwrap();
                        let mut ram = Ram::default();
                        run(&mut machine, &mut ram, opcode, &[r(d), r(1), r(2)]);
                        assert_eq!(
                            machine.architectural_state(),
                            &expected,
                            "{config:?} {opcode:?} {a:#x} {b:#x} d={d}"
                        );
                        assert_eq!(ram.reads + ram.writes, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn immediate_arithmetic_compare_and_not_have_exact_flags() {
    for config in MODES {
        let mask = config.word_width().mask();
        let sign = 1u64 << (config.word_bits() - 1);
        for (opcode, a, immediate, value, flags) in [
            (Opcode::Addi, 0, -1, mask, 1),
            (Opcode::Addi, mask, 1, 0, 6),
            (Opcode::Addi, sign - 1, 1, sign, 9),
            (Opcode::Subi, 0, 1, mask, 5),
            (Opcode::Subi, sign, 1, sign - 1, 8),
            (Opcode::Subi, 0, -1, 1, 4),
        ] {
            let mut machine = cpu(config, 16, 256, 63, &[(15, a)]);
            run(
                &mut machine,
                &mut Ram::default(),
                opcode,
                &[r(15), r(15), Operand::Immediate(immediate)],
            );
            assert_eq!(
                machine.architectural_state().registers().read_raw(15),
                Ok(value)
            );
            assert_eq!(machine.architectural_state().status().bits(), 48 | flags);
            assert_eq!(machine.architectural_state().pc().as_u64(), 24);
        }
        for (a, b, flags) in [(0, 1, 5), (1, 1, 2), (sign, 1, 8), (1, 0, 0)] {
            let mut machine = cpu(config, 0, 256, 63, &[(1, a), (2, b)]);
            let registers = machine.architectural_state().registers().clone();
            run(
                &mut machine,
                &mut Ram::default(),
                Opcode::Cmp,
                &[r(1), r(2)],
            );
            assert_eq!(machine.architectural_state().registers(), &registers);
            assert_eq!(machine.architectural_state().status().bits(), 48 | flags);
            assert_eq!(machine.architectural_state().pc().as_u64(), 8);
        }
        for (a, value, flags) in [(0, mask, 1), (mask, 0, 2), (sign, sign - 1, 0)] {
            let mut machine = cpu(config, 0, 256, 63, &[(0, a)]);
            run(
                &mut machine,
                &mut Ram::default(),
                Opcode::Not,
                &[r(0), r(0)],
            );
            assert_eq!(
                machine.architectural_state().registers().read_raw(0),
                Ok(value)
            );
            assert_eq!(machine.architectural_state().status().bits(), 48 | flags);
            assert_eq!(machine.architectural_state().pc().as_u64(), 8);
        }
    }
}

#[test]
fn division_faults_preserve_every_register_status_pc_and_memory() {
    for config in MODES {
        for opcode in [Opcode::Divu, Opcode::Divs, Opcode::Remu, Opcode::Rems] {
            for a in [0, 7, config.word_width().mask()] {
                let mut machine = cpu(config, 16, 256, 63, &[(0, 123), (1, a)]);
                assert!(matches!(
                    unchanged(
                        &mut machine,
                        &mut Ram::default(),
                        opcode,
                        &[r(0), r(1), r(2)]
                    ),
                    Cause::Width(WidthError::DivisionByZero { .. })
                ));
            }
        }
        for opcode in [Opcode::Divs, Opcode::Rems] {
            let mut machine = cpu(
                config,
                16,
                256,
                63,
                &[
                    (1, 1 << (config.word_bits() - 1)),
                    (2, config.word_width().mask()),
                ],
            );
            assert!(matches!(
                unchanged(
                    &mut machine,
                    &mut Ram::default(),
                    opcode,
                    &[r(1), r(1), r(2)]
                ),
                Cause::Width(WidthError::SignedDivisionOverflow { .. })
            ));
        }
    }
}

#[test]
fn moves_immediates_and_special_registers_preserve_status() {
    for config in MODES {
        let mask = config.word_width().mask();
        for index in 0..16 {
            for immediate in [i32::MIN, -1, 0, 1, i32::MAX] {
                let mut machine = cpu(config, 4, 256, 63, &[]);
                run(
                    &mut machine,
                    &mut Ram::default(),
                    Opcode::Li,
                    &[r(index), Operand::Immediate(immediate)],
                );
                assert_eq!(
                    machine.architectural_state().registers().read_raw(index),
                    Ok((i64::from(immediate) as u64) & mask)
                );
                assert_eq!(machine.architectural_state().status().bits(), 63);
                assert_eq!(machine.architectural_state().pc().as_u64(), 12);
            }
            let mut machine = cpu(config, 4, 256, 63, &[(index, mask)]);
            let mut ram = Ram::default();
            run(&mut machine, &mut ram, Opcode::Mov, &[r(index), r(index)]);
            assert_eq!(
                machine.architectural_state().registers().read_raw(index),
                Ok(mask)
            );
            run(&mut machine, &mut ram, Opcode::Getpc, &[r(index)]);
            assert_eq!(
                machine.architectural_state().registers().read_raw(index),
                Ok(12)
            );
            run(&mut machine, &mut ram, Opcode::Getsp, &[r(index)]);
            assert_eq!(
                machine.architectural_state().registers().read_raw(index),
                Ok(256)
            );
            run(&mut machine, &mut ram, Opcode::Setsp, &[r(index)]);
            assert_eq!(machine.architectural_state().sp().as_u64(), 256);
            run(&mut machine, &mut ram, Opcode::Getstatus, &[r(index)]);
            assert_eq!(
                machine.architectural_state().registers().read_raw(index),
                Ok(63)
            );
            run(&mut machine, &mut ram, Opcode::Nop, &[]);
            assert_eq!(machine.architectural_state().pc().as_u64(), 52);
            assert_eq!(machine.architectural_state().status().bits(), 63);
        }
        let mut machine = cpu(
            config,
            0,
            256,
            63,
            &[(1, mask & !(u64::from(config.word_bytes()) - 1))],
        );
        run(&mut machine, &mut Ram::default(), Opcode::Setsp, &[r(1)]);
        assert_eq!(
            machine.architectural_state().sp().as_u64(),
            mask & !(u64::from(config.word_bytes()) - 1)
        );
        let mut machine = cpu(config, 0, 256, 63, &[(1, 3)]);
        assert!(matches!(
            unchanged(&mut machine, &mut Ram::default(), Opcode::Setsp, &[r(1)]),
            Cause::Control(_)
        ));
    }
}

#[test]
fn every_branch_condition_exhausts_all_flag_patterns() {
    for config in MODES {
        for flags in 0..16u64 {
            let (n, z, c, v) = (
                flags & 1 != 0,
                flags & 2 != 0,
                flags & 4 != 0,
                flags & 8 != 0,
            );
            let expected = [
                true,
                z,
                !z,
                c,
                !c,
                c || z,
                !c && !z,
                n != v,
                n == v,
                z || n != v,
                !z && n == v,
                v,
                !v,
                n,
                !n,
            ];
            for (&condition, taken) in Condition::ALL.iter().zip(expected) {
                for displacement in [-3, -2, -1, 0, 2, i32::MAX] {
                    let mut machine = cpu(config, 0x100, 256, 48 | flags, &[]);
                    let result = machine.execute(
                        &instruction(
                            config,
                            Opcode::Br,
                            &[
                                Operand::Condition(condition),
                                Operand::Immediate(displacement),
                            ],
                        ),
                        &mut Ram::default(),
                    );
                    let target = 0x108i128 + i128::from(displacement) * 4;
                    if taken && target > i128::from(config.word_width().mask()) {
                        assert!(matches!(result.unwrap_err().cause, Cause::Outcome(_)));
                        assert_eq!(machine.architectural_state().pc().as_u64(), 0x100);
                    } else {
                        assert_eq!(result, Ok(OutcomeApplication::Continue));
                        assert_eq!(
                            machine.architectural_state().pc().as_u64(),
                            if taken { target as u64 } else { 0x108 }
                        );
                    }
                    assert_eq!(machine.architectural_state().status().bits(), 48 | flags);
                }
            }
        }
        let mut machine = cpu(config, 0, 256, 0, &[]);
        run(
            &mut machine,
            &mut Ram::default(),
            Opcode::Br,
            &[
                Operand::Condition(Condition::Eq),
                Operand::Immediate(i32::MIN),
            ],
        );
        assert_eq!(machine.architectural_state().pc().as_u64(), 8);
        let mut machine = cpu(config, 0, 256, 2, &[]);
        assert!(matches!(
            unchanged(
                &mut machine,
                &mut Ram::default(),
                Opcode::Br,
                &[
                    Operand::Condition(Condition::Eq),
                    Operand::Immediate(i32::MIN)
                ]
            ),
            Cause::Outcome(_)
        ));
    }
}

#[test]
fn every_load_store_width_extends_truncates_and_handles_aliases() {
    for config in MODES {
        for &size in DataSize::ALL {
            if !config.supports_data_size(size.bytes()) {
                continue;
            }
            let bytes = usize::from(size.bytes());
            let bits = size.bytes() * 8;
            let data_mask = u64::MAX >> (64 - bits);
            for value in [
                0,
                1,
                (1u64 << (bits - 1)) - 1,
                1u64 << (bits - 1),
                data_mask,
            ] {
                let mut ram = Ram::default();
                let mut machine = cpu(config, 0, 256, 63, &[(1, 136), (2, value | !data_mask)]);
                run(
                    &mut machine,
                    &mut ram,
                    Opcode::St,
                    &[r(2), mem(1, -8), Operand::DataSize(size)],
                );
                assert_eq!(ram.word(128, bytes), value);
                assert_eq!(ram.word(120, 8), 0);
                assert_eq!(ram.word(128 + bytes, 8), 0);
                run(
                    &mut machine,
                    &mut ram,
                    Opcode::Ldz,
                    &[r(0), mem(1, -8), Operand::DataSize(size)],
                );
                assert_eq!(
                    machine.architectural_state().registers().read_raw(0),
                    Ok(value)
                );
                run(
                    &mut machine,
                    &mut ram,
                    Opcode::Lds,
                    &[r(1), mem(1, -8), Operand::DataSize(size)],
                );
                let signed = if value & (1 << (bits - 1)) == 0 {
                    value
                } else {
                    value | !data_mask
                } & config.word_width().mask();
                assert_eq!(
                    machine.architectural_state().registers().read_raw(1),
                    Ok(signed)
                );
                assert_eq!(machine.architectural_state().status().bits(), 63);
                assert_eq!(machine.architectural_state().pc().as_u64(), 24);
                assert_eq!((ram.reads, ram.writes), (2, 1));
            }
            let mut ram = Ram::default();
            let mut machine = cpu(config, 0, 256, 63, &[(1, 128)]);
            run(
                &mut machine,
                &mut ram,
                Opcode::St,
                &[r(1), mem(1, 0), Operand::DataSize(size)],
            );
            assert_eq!(ram.word(128, bytes), 128);
        }
    }
}

#[test]
fn data_faults_validate_ranges_permissions_and_transactions_without_effects() {
    for config in MODES {
        for opcode in [Opcode::Ldz, Opcode::Lds, Opcode::St] {
            for (base, displacement, size) in [
                (0, -1, DataSize::Byte),
                (config.word_width().mask(), 1, DataSize::Byte),
                (3, 0, DataSize::Word),
                (512, 0, DataSize::Word),
                (config.word_width().mask(), 0, DataSize::Word),
            ] {
                let mut machine = cpu(config, 0, 256, 63, &[(0, 99), (1, base)]);
                unchanged(
                    &mut machine,
                    &mut Ram::default(),
                    opcode,
                    &[r(0), mem(1, displacement), Operand::DataSize(size)],
                );
            }
            for failure in 0..4 {
                let mut ram = Ram::default();
                match failure {
                    0 => ram.readable = false,
                    1 => ram.writable = false,
                    2 => ram.user = false,
                    3 => ram.fail = true,
                    _ => unreachable!(),
                }
                if (failure == 0 && opcode == Opcode::St) || (failure == 1 && opcode != Opcode::St)
                {
                    continue;
                }
                let mut machine = cpu(config, 0, 256, 63, &[(0, 99), (1, 128)]);
                assert!(matches!(
                    unchanged(
                        &mut machine,
                        &mut ram,
                        opcode,
                        &[r(0), mem(1, 0), Operand::DataSize(DataSize::Word)]
                    ),
                    Cause::Memory { .. }
                ));
            }
        }
        let mut ram = Ram::default();
        ram.device = true;
        let mut machine = cpu(config, 0, 256, 31, &[(0, 128)]);
        run(
            &mut machine,
            &mut ram,
            Opcode::Ldz,
            &[r(0), mem(0, 0), Operand::DataSize(DataSize::Word)],
        );
        assert_eq!(
            (ram.reads, machine.architectural_state().pc().as_u64()),
            (1, 8)
        );
        let mut machine = cpu(config, config.word_width().mask() - 7, 256, 31, &[(0, 128)]);
        assert!(matches!(
            unchanged(
                &mut machine,
                &mut ram,
                Opcode::Ldz,
                &[r(0), mem(0, 0), Operand::DataSize(DataSize::Word)]
            ),
            Cause::NextPc(_)
        ));
    }
}

#[test]
fn calls_returns_and_jumps_obey_stack_and_subsequent_fetch_policy() {
    for config in MODES {
        for status in [31, 63] {
            for opcode in [Opcode::Call, Opcode::Callr] {
                let mut ram = Ram::default();
                let mut machine = cpu(config, 0x100, 256, status, &[(1, 0x110)]);
                let operands = if opcode == Opcode::Call {
                    vec![Operand::Immediate(2)]
                } else {
                    vec![r(1)]
                };
                run(&mut machine, &mut ram, opcode, &operands);
                let slot = 256 - usize::from(config.word_bytes());
                assert_eq!(machine.architectural_state().pc().as_u64(), 0x110);
                assert_eq!(machine.architectural_state().sp().as_u64(), slot as u64);
                assert_eq!(ram.word(slot, usize::from(config.word_bytes())), 0x108);
                let pushed = ram.clone();
                run(&mut machine, &mut ram, Opcode::Ret, &[]);
                assert_eq!(machine.architectural_state().pc().as_u64(), 0x108);
                assert_eq!(machine.architectural_state().sp().as_u64(), 256);
                assert_eq!(machine.architectural_state().status().bits(), status);
                assert_eq!(ram, pushed);
            }
        }
        for opcode in [Opcode::Jmp, Opcode::Callr] {
            let mut ram = Ram::default();
            let mut machine = cpu(config, 0, 256, 31, &[(1, config.word_width().mask() - 3)]);
            run(&mut machine, &mut ram, opcode, &[r(1)]);
            assert_eq!(
                machine.architectural_state().pc().as_u64(),
                config.word_width().mask() - 3
            );
            assert_eq!(ram.writes, usize::from(opcode == Opcode::Callr));
            let before = machine.architectural_state().clone();
            assert!(matches!(
                machine.step(&mut ram).unwrap_err().cause,
                Cause::Width(_)
            ));
            assert_eq!(machine.architectural_state(), &before);
        }
        let mut ram = Ram::default();
        ram.code(4, config, Opcode::Getpc, &[r(0)]);
        let mut machine = cpu(config, 0, 256, 31, &[(1, 4)]);
        run(&mut machine, &mut ram, Opcode::Jmp, &[r(1)]);
        machine.step(&mut ram).unwrap();
        assert_eq!(machine.architectural_state().registers().read_raw(0), Ok(4));
    }
}

#[test]
fn stack_fault_priority_and_pure_return_peek_prevent_partial_effects() {
    for config in MODES {
        let mut ram = Ram::default();
        ram.device = true;
        let mut machine = cpu(config, 0, 0, 31, &[(1, 3)]);
        let cause = unchanged(&mut machine, &mut ram, Opcode::Callr, &[r(1)]);
        assert!(matches!(
            cause,
            Cause::Outcome(lazalith_cpu::OutcomeError {
                kind: lazalith_cpu::OutcomeErrorKind::Control(_),
                ..
            })
        ));
        let mut machine = cpu(config, 0, 0, 31, &[]);
        assert!(matches!(
            unchanged(
                &mut machine,
                &mut ram,
                Opcode::Call,
                &[Operand::Immediate(0)]
            ),
            Cause::Outcome(_)
        ));
        let top = config.word_width().mask() & !(u64::from(config.word_bytes()) - 1);
        let mut machine = cpu(config, 0, top, 31, &[]);
        assert!(matches!(
            unchanged(&mut machine, &mut ram, Opcode::Ret, &[]),
            Cause::NextPc(_)
        ));
        for opcode in [Opcode::Call, Opcode::Ret] {
            let operands = if opcode == Opcode::Call {
                vec![Operand::Immediate(0)]
            } else {
                vec![]
            };
            for failure in 0..5 {
                let mut ram = Ram::default();
                match failure {
                    0 => ram.device = true,
                    1 => ram.readable = false,
                    2 => ram.writable = false,
                    3 => ram.fail = true,
                    4 => ram.user = false,
                    _ => unreachable!(),
                }
                if (failure == 1 && opcode == Opcode::Call)
                    || (failure == 2 && opcode == Opcode::Ret)
                {
                    continue;
                }
                let mut machine = cpu(config, 0, 256, 63, &[]);
                assert!(matches!(
                    unchanged(&mut machine, &mut ram, opcode, &operands),
                    Cause::Memory { .. }
                ));
            }
        }
        let mut ram = Ram::default();
        ram.put(128, &3u64.to_le_bytes());
        let mut machine = cpu(config, 0, 128, 31, &[]);
        assert!(matches!(
            unchanged(&mut machine, &mut ram, Opcode::Ret, &[]),
            Cause::Outcome(_)
        ));
        for (opcode, sp) in [(Opcode::Call, 520), (Opcode::Ret, 512)] {
            let operands = if opcode == Opcode::Call {
                vec![Operand::Immediate(0)]
            } else {
                vec![]
            };
            let mut machine = cpu(config, 0, sp, 31, &[]);
            assert!(matches!(
                unchanged(&mut machine, &mut ram, opcode, &operands),
                Cause::Memory {
                    source: MemoryError::Unmapped,
                    ..
                }
            ));
        }
    }
}

#[test]
fn privilege_halt_and_all_control_selectors_are_explicit() {
    for config in MODES {
        for opcode in [Opcode::Halt, Opcode::Ei, Opcode::Di, Opcode::Rfe] {
            let mut machine = cpu(config, config.word_width().mask() - 7, 256, 63, &[]);
            assert_eq!(
                unchanged(&mut machine, &mut Ram::default(), opcode, &[]),
                Cause::PrivilegeViolation
            );
        }
        for &control in ControlRegister::ALL {
            for opcode in [Opcode::Csrr, Opcode::Csrw] {
                let operands = if opcode == Opcode::Csrr {
                    vec![r(0), Operand::Control(control)]
                } else {
                    vec![Operand::Control(control), r(0)]
                };
                for status in [31, 63] {
                    let mut machine = cpu(config, 0, 256, status, &[(0, u64::MAX)]);
                    let actual = unchanged(&mut machine, &mut Ram::default(), opcode, &operands);
                    if status == 63 {
                        assert_eq!(actual, Cause::PrivilegeViolation);
                    } else {
                        assert!(matches!(actual, Cause::Control(_)));
                    }
                }
            }
        }
        let mut machine = cpu(config, 0, 256, 15, &[]);
        let mut ram = Ram::default();
        run(&mut machine, &mut ram, Opcode::Ei, &[]);
        assert_eq!(machine.architectural_state().status().bits(), 31);
        run(&mut machine, &mut ram, Opcode::Di, &[]);
        assert_eq!(machine.architectural_state().status().bits(), 15);
        assert_eq!(
            machine.execute(&instruction(config, Opcode::Halt, &[]), &mut ram),
            Ok(OutcomeApplication::Halted)
        );
        assert_eq!(machine.architectural_state().pc().as_u64(), 24);
        assert_eq!(machine.execution_state(), ExecutionState::Halted);
        for opcode in [
            Opcode::Nop,
            Opcode::Ei,
            Opcode::Ret,
            Opcode::Halt,
            Opcode::Syscall,
        ] {
            assert_eq!(
                unchanged(&mut machine, &mut ram, opcode, &[]),
                Cause::Halted
            );
        }
        let before = machine.architectural_state().clone();
        assert!(matches!(
            machine.step_bytes(&[], &mut ram).unwrap_err().cause,
            Cause::Halted
        ));
        assert!(matches!(
            machine.step(&mut ram).unwrap_err().cause,
            Cause::Halted
        ));
        assert_eq!(machine.architectural_state(), &before);
    }
}

#[test]
fn decode_mode_fetch_errors_retain_context_and_typed_sources() {
    for config in MODES {
        for bytes in [
            vec![],
            vec![0; 7],
            vec![0; 9],
            vec![0xff; 8],
            vec![0x52, 0, 0, 1, 0, 0, 0, 0],
            vec![0x40, 0, 0xf0, 0, 0, 0, 0, 0],
        ] {
            let mut ram = Ram::default();
            let mut machine = cpu(config, 0, 256, 63, &[]);
            let before = machine.architectural_state().clone();
            let error = machine.step_bytes(&bytes, &mut ram).unwrap_err();
            assert!(matches!(error.cause, Cause::Decode(_)));
            assert_eq!(error.opcode, bytes.first().copied());
            assert_eq!(error.pc.as_u64(), 0);
            assert!(error.source().unwrap().source().is_some());
            assert!(error.to_string().contains("PC"));
            assert_eq!(machine.architectural_state(), &before);
        }
        for failure in 0..4 {
            let mut ram = Ram::default();
            match failure {
                0 => ram.executable = false,
                1 => ram.user = false,
                2 => ram.device = true,
                _ => {}
            }
            let mut machine = cpu(config, if failure == 3 { 508 } else { 0 }, 256, 63, &[]);
            let before = machine.architectural_state().clone();
            let memory = ram.clone();
            let error = machine.step(&mut ram).unwrap_err();
            assert!(matches!(error.cause, Cause::Fetch(_)));
            assert_eq!(error.opcode, None);
            assert!(error.source().unwrap().source().is_some());
            assert_eq!(machine.architectural_state(), &before);
            assert_eq!(ram, memory);
        }
        let mut ram = Ram::default();
        ram.readable = false;
        ram.code(4, config, Opcode::Nop, &[]);
        let mut machine = cpu(config, 4, 256, 63, &[]);
        assert_eq!(machine.step(&mut ram), Ok(OutcomeApplication::Continue));
    }
    let wide = instruction(
        ArchitectureConfig::lz64(),
        Opcode::Ldz,
        &[r(0), mem(1, 0), Operand::DataSize(DataSize::Double)],
    );
    let mut machine = cpu(ArchitectureConfig::lz32(), 0, 256, 31, &[]);
    let before = machine.architectural_state().clone();
    let mut ram = Ram::default();
    assert!(matches!(
        machine.execute(&wide, &mut ram).unwrap_err().cause,
        Cause::Instruction(_)
    ));
    assert!(matches!(
        machine
            .step_bytes(
                &lazalith_isa::encode(ArchitectureConfig::lz64(), &wide).unwrap(),
                &mut ram
            )
            .unwrap_err()
            .cause,
        Cause::Decode(_)
    ));
    assert_eq!(machine.architectural_state(), &before);
    assert_eq!(ram.reads + ram.writes, 0);
}
