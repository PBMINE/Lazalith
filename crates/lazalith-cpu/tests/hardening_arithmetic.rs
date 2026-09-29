//! Hardening: arithmetic checked against a model written independently of it.
//!
//! The existing CPU tests assert what the interpreter *is*. This file asserts what
//! arithmetic *means*, using a model written from the ISA document rather than from
//! the implementation, and comparing after every instruction. That is the only way a
//! wrong-answer bug in a wide operation is caught at all: a test that computes the
//! expected value by calling the same `WordWidth` method the interpreter calls is a
//! test that agrees with the bug.
//!
//! The model here uses Rust's own `i32`/`i64`/`u32`/`u64` arithmetic through explicit
//! masking, so a defect in `WordWidth` cannot hide behind itself.
//!
//! Every operation is run through a real `ReferenceInterpreter` with a real memory,
//! not by calling the width helpers, so the comparison covers the whole path:
//! operand fetch, width truncation, the operation, and the register write.

mod support;

use lazalith_cpu::ExecutionEngine;
use lazalith_isa::{Instruction, Opcode};
use lazalith_types::{ArchitectureConfig as C, WordWidth};
use support::{MODES, cpu, instruction, r};

/// A deterministic generator, so a failure is reproducible from the seed alone.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*, chosen because it is four lines and has no library.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn value_for(&mut self, width: WordWidth) -> u64 {
        match width {
            // Half the values are "interesting" — zero, one, all ones, the sign
            // bit, one past the sign bit — because those are where a width or sign
            // mistake shows up, and a purely uniform sample almost never hits them.
            WordWidth::W32 => {
                if self.next().is_multiple_of(2) {
                    self.next() as u32 as u64
                } else {
                    let table = [
                        0u64,
                        1,
                        0xFFFF_FFFF,
                        0x8000_0000,
                        0x7FFF_FFFF,
                        0xFFFF_FFFE,
                        2,
                        0xFFFF,
                        0xFFFF_0000,
                        0x0000_FFFF,
                    ];
                    table[(self.next() as usize) % table.len()]
                }
            }
            WordWidth::W64 => {
                if self.next().is_multiple_of(2) {
                    self.next()
                } else {
                    let table = [
                        0u64,
                        1,
                        u64::MAX,
                        0x8000_0000_0000_0000,
                        0x7FFF_FFFF_FFFF_FFFF,
                        u64::MAX - 1,
                        2,
                        0xFFFF_FFFF,
                        0xFFFF_FFFF_0000_0000,
                        0x0000_0000_FFFF_FFFF,
                    ];
                    table[(self.next() as usize) % table.len()]
                }
            }
        }
    }
}

/// The reference model: the meaning of each operation at a width, written with Rust's
/// own integer arithmetic and explicit masking rather than with the crate's helpers.
fn model(opcode: Opcode, width: WordWidth, a: u64, b: u64) -> Result<u64, ModelFault> {
    let bits = match width {
        WordWidth::W32 => 32,
        // v1 has exactly two widths, and a third arm here would be dead code that
        // a future width would silently fall into rather than fail to compile.
        WordWidth::W64 => 64,
    };
    let mask = if bits == 32 { 0xFFFF_FFFFu64 } else { u64::MAX };
    let sign = 1u64 << (bits - 1);
    let a = a & mask;
    let b = b & mask;
    let sa = sign_extend(a, bits);
    let sb = sign_extend(b, bits);
    Ok(match opcode {
        Opcode::Add => a.wrapping_add(b) & mask,
        Opcode::Sub => a.wrapping_sub(b) & mask,
        Opcode::Mul => a.wrapping_mul(b) & mask,
        Opcode::Divu => {
            if b == 0 {
                return Err(ModelFault::DivisionByZero);
            }
            a / b
        }
        Opcode::Remu => {
            if b == 0 {
                return Err(ModelFault::DivisionByZero);
            }
            a % b
        }
        Opcode::Divs => {
            if b == 0 {
                return Err(ModelFault::DivisionByZero);
            }
            if a == sign && b == mask {
                return Err(ModelFault::SignedOverflow);
            }
            (sa / sb) as u64 & mask
        }
        Opcode::Rems => {
            if b == 0 {
                return Err(ModelFault::DivisionByZero);
            }
            if a == sign && b == mask {
                return Err(ModelFault::SignedOverflow);
            }
            (sa % sb) as u64 & mask
        }
        Opcode::And => a & b,
        Opcode::Or => a | b,
        Opcode::Xor => a ^ b,
        // The shift amount is taken modulo the width, which is the documented
        // behaviour; `docs/isa.md` says so and this test is what keeps it true.
        Opcode::Shl => {
            let n = (b % bits) as u32;
            a.wrapping_shl(n) & mask
        }
        Opcode::Shr => {
            let n = (b % bits) as u32;
            a >> n
        }
        Opcode::Sar => {
            let n = (b % bits) as u32;
            ((sa >> n) as u64) & mask
        }
        other => panic!("{other:?} is not a three-register arithmetic operation"),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelFault {
    DivisionByZero,
    SignedOverflow,
}

/// Runs one arithmetic instruction on a real interpreter and reports the result.
fn run(config: C, opcode: Opcode, a: u64, b: u64) -> Result<u64, String> {
    let mut cpu = cpu(config, 0, 0, 0, &[(1, a), (2, b)]);
    let instruction = instruction(config, opcode, &[r(3), r(1), r(2)]);
    let bytes = lazalith_isa::encode(config, &instruction).expect("the instruction encodes");
    let mut memory = support::Ram::default();
    match lazalith_cpu::ReferenceInterpreter::new().step_bytes(&mut cpu, &bytes, &mut memory) {
        Ok(_) => Ok(cpu
            .architectural()
            .registers()
            .read(RegisterIndex::try_from(3).unwrap())),
        Err(error) => Err(format!("{error:?}")),
    }
}

const OPERATIONS: [Opcode; 13] = [
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
];

#[test]
fn every_arithmetic_operation_agrees_with_the_model_over_random_inputs() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let mut checked = 0u32;
    for config in MODES {
        let width = config.word_width();
        for opcode in OPERATIONS {
            for _ in 0..400 {
                let a = rng.value_for(width);
                let b = rng.value_for(width);
                let expected = model(opcode, width, a, b);
                let actual = run(config, opcode, a, b);
                match (expected, actual) {
                    (Ok(want), Ok(got)) => assert_eq!(
                        got, want,
                        "{opcode:?} on {config:?} with a={a:#x} b={b:#x}: model says \
                         {want:#x}, the machine says {got:#x}"
                    ),
                    (Err(_), Err(_)) => {}
                    (Ok(want), Err(error)) => panic!(
                        "{opcode:?} on {config:?} with a={a:#x} b={b:#x} should be {want:#x} \
                         but the machine refused: {error}"
                    ),
                    (Err(fault), Ok(got)) => panic!(
                        "{opcode:?} on {config:?} with a={a:#x} b={b:#x} should have failed \
                         ({fault:?}) but the machine answered {got:#x}"
                    ),
                }
                checked += 1;
            }
        }
    }
    assert!(
        checked > 10_000,
        "the campaign should be large; it ran {checked} cases"
    );
}

#[test]
fn a_signed_division_of_the_most_negative_number_by_negative_one_faults() {
    // The one arithmetic case that has no answer at the width, and the one a wrong
    // implementation answers with the most negative number itself.
    for config in MODES {
        let width = config.word_width();
        let most_negative = match width {
            WordWidth::W32 => 0x8000_0000u64,
            _ => 0x8000_0000_0000_0000,
        };
        let all_ones = match width {
            WordWidth::W32 => 0xFFFF_FFFFu64,
            _ => u64::MAX,
        };
        for opcode in [Opcode::Divs, Opcode::Rems] {
            let error = run(config, opcode, most_negative, all_ones)
                .expect_err("the most negative number divided by minus one has no answer");
            assert!(
                error.contains("Division") || error.contains("overflow") || error.contains("Width"),
                "the fault should name the overflow, got {error}"
            );
        }
    }
}

#[test]
fn a_division_by_zero_faults_rather_than_answering() {
    for config in MODES {
        for opcode in [Opcode::Divu, Opcode::Divs, Opcode::Remu, Opcode::Rems] {
            // Zero, and a value that *becomes* zero at this width. The first is the
            // obvious case; the second is the one a truncation bug would miss, and it
            // is only zero on a 32-bit machine — 0x1_0000_0000 is a real divisor on a
            // 64-bit one, so a test that asserted it faulted everywhere would be
            // asserting a bug.
            let mut divisors = vec![0u64];
            if config.word_width() == WordWidth::W32 {
                divisors.push(0x1_0000_0000);
                divisors.push(0xFFFF_FFFF_0000_0000);
            }
            for divisor in divisors {
                let error =
                    run(config, opcode, 7, divisor).expect_err("a division by zero has no answer");
                assert!(
                    error.contains("Division"),
                    "the fault should name division by zero, got {error}"
                );
            }
        }
    }

    // And the complement, which is the half that would catch a fault where there
    // should not be one: a divisor that is *not* zero after truncation must answer.
    for config in MODES {
        let got = run(config, Opcode::Divu, 7, 0xFFFF_FFFF_FFFF_FFFF)
            .expect("all-ones is not zero at either width");
        assert_eq!(got, 0, "7 divided by the largest value is zero");
    }
}

#[test]
fn an_arithmetic_result_is_truncated_to_the_machines_width() {
    // A 32-bit machine must not keep the upper half of a 64-bit product in the
    // register: the register is 64 bits wide, and "the result fits" is a promise
    // about the *value*, not about the storage. A program that adds two 32-bit
    // numbers and compares against a 64-bit constant must see the wrapped value.
    let config = C::lz32();
    let got = run(config, Opcode::Add, 0xFFFF_FFFF, 2).expect("an add cannot fail");
    assert_eq!(got, 1, "a 32-bit add wraps into 1, not 0x1_0000_0001");
}

#[test]
fn a_shift_by_more_than_the_width_is_the_same_as_the_shift_modulo_it() {
    // Documented behaviour, and the kind of thing a document and an implementation
    // can quietly disagree about. If the ISA ever changes this, *this* test is what
    // should fail.
    for config in MODES {
        let width = config.word_width();
        let bits = config_bits(width);
        let value = match width {
            WordWidth::W32 => 0x0000_0003u64,
            _ => 0x0000_0000_0000_0003,
        };
        let by = bits;
        let wrapped = 0;
        for opcode in [Opcode::Shl, Opcode::Shr, Opcode::Sar] {
            let got = run(config, opcode, value, by).expect("a shift cannot fail");
            assert_eq!(
                got,
                run(config, opcode, value, wrapped).expect("a shift cannot fail"),
                "{opcode:?} by the width must equal {opcode:?} by zero, on {config:?}"
            );
        }
    }
}

#[test]
fn a_register_is_never_wider_than_the_machines_result() {
    // LZ32 and LZ64 are the same ISA with a different width, and the *meaning* of a
    // value must differ by the width and by nothing else. Running the identical
    // operation at both widths must agree exactly where the two agree, and disagree
    // exactly where the widths make them disagree.
    let mut rng = Rng(0xFEED_FACE_CAFE_BEEF);
    for _ in 0..500 {
        let value = rng.value_for(WordWidth::W32);
        let wide = run(C::lz64(), Opcode::Add, value, 1).expect("an add cannot fail");
        let narrow = run(C::lz32(), Opcode::Add, value, 1).expect("an add cannot fail");
        assert_eq!(
            wide as u32 as u64, narrow,
            "a 64-bit add and a 32-bit add of the same values must agree below 2^32"
        );
    }
}

/// Keeps the unused-import checker honest about the `Instruction` import, which the
/// helper above uses only through `support::instruction`.
#[allow(dead_code)]
fn the_instruction_type_is_used() -> Instruction {
    instruction(C::lz64(), Opcode::Nop, &[])
}

/// The register read helper, kept beside the other one so both halves of a step are
/// in one place.
use lazalith_types::RegisterIndex;

/// The width's bit count, from the config rather than from `WordWidth`, so the model
/// does not borrow the implementation's own arithmetic helpers.
const fn config_bits(width: WordWidth) -> u64 {
    match width {
        WordWidth::W32 => 32,
        WordWidth::W64 => 64,
    }
}

/// Sign-extends a masked value to `i64` the way the ISA document says a value is
/// interpreted, written without the crate's own `signed` helper for the same reason
/// as everything else here.
fn sign_extend(value: u64, bits: u64) -> i64 {
    if bits >= 64 {
        // A 64-bit value reinterpreted as signed *is* its own sign extension, and
        // `1i64 << 64` does not exist, which is a good way to find out.
        return value as i64;
    }
    let sign = 1u64 << (bits - 1);
    if value & sign == 0 {
        value as i64
    } else {
        (value as i64) - (1i64 << (bits as u32))
    }
}
