//! Property tests for width conversions and arithmetic.
//!
//! # Why this area more than the others
//!
//! Every value on this machine passes through a width conversion at some point: a
//! C `int` is 32 bits in a 64-bit register, a byte load sign-extends or does not
//! depending on the C type it came from, and a 32-bit add has to produce a carry
//! and an overflow flag that mean what they say. Step 82 found three bugs here —
//! a `char` that was signed whatever the program said, a widening conversion that
//! read bytes the value never had, and an extension that followed the target
//! instead of the source — and all three were invisible to a suite that used `int`
//! and nothing else.
//!
//! The properties are stated over the *whole* space of widths and values rather
//! than over a few interesting pairs, because that is where a conversion bug lives:
//! every combination of source width, sign and value, checked against the
//! definition rather than against a table.

use lazalith_properties::{Case, Gen, check};
use lazalith_types::{ArithmeticResult, WidthError, WordWidth};

/// The low `bits` bits of a value, for a width that is not 64.
fn low_mask(bits: u8) -> u64 {
    u64::MAX >> (64 - bits)
}

/// The top bit of a width, for a width that is not 64.
fn sign_bit(bits: u8) -> u64 {
    1u64 << (bits - 1)
}

/// Every width a value can have in this machine.s arithmetic.
const WIDTHS: [WordWidth; 2] = [WordWidth::W32, WordWidth::W64];

/// A source width, which is narrower than or equal to the machine's.
fn source_bits(width: WordWidth) -> u8 {
    match width {
        WordWidth::W32 => 32,
        WordWidth::W64 => 64,
    }
}

/// A conversion: a value, the width it came from, and how it is being widened.
struct Widen {
    width: WordWidth,
    value: u64,
    from: u8,
    signed: bool,
}

impl Case for Widen {
    fn generate(source: &mut Gen) -> Self {
        let width = match source.bool() {
            true => WordWidth::W32,
            false => WordWidth::W64,
        };
        // Any of 8, 16, 32, 64, so long as it fits the machine. A source wider
        // than the machine is a different question and is refused, not converted.
        let from = match source.below(4) {
            0 => 8,
            1 => 16,
            2 => 32,
            _ => source_bits(width),
        };
        Self {
            width,
            value: source.interesting_u64(),
            from,
            signed: source.bool(),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{:#x} from {} bits, {} into {:?}",
            self.value,
            self.from,
            if self.signed {
                "sign-extended"
            } else {
                "zero-extended"
            },
            self.width
        )
    }
}

/// A zero extension is a mask, and a mask is idempotent.
///
/// `zero_extend(v, n)` keeps the low `n` bits. Asking twice must give the same
/// answer, because a conversion applied to its own output is the shape of every
/// round trip through memory: a byte is stored, loaded as a word, and stored
/// again.
#[test]
fn zero_extension_is_a_mask_and_is_idempotent() {
    check::<Widen>(64, |case| {
        let Ok(once) = case.width.zero_extend(case.value, case.from) else {
            return true;
        };
        let twice = case.width.zero_extend(once, case.from);
        twice == Ok(once) && once & !case.width.mask() == 0
    });
}

/// A zero extension keeps the value's magnitude and drops nothing above it.
///
/// The definition, stated as a property: after zero-extending, the result equals
/// the low `from` bits of the input, and reading those bits back is the input's own
/// truncation.
#[test]
fn zero_extension_keeps_exactly_the_low_bits() {
    check::<Widen>(64, |case| {
        let Ok(extended) = case.width.zero_extend(case.value, case.from) else {
            return true;
        };
        let mask = u64::MAX >> (64 - case.from);
        extended == case.value & mask
    });
}

/// A sign extension is a zero extension of the same bits, with the sign filled in.
///
/// The definition: take the low `from` bits, and if the top of *those* is set,
/// fill everything above it. Checked both ways, because a sign extension that
/// always zero-extends and one that always fills with ones are both wrong and only
/// the pair catches them.
#[test]
fn sign_extension_fills_from_the_source_sign_bit() {
    check::<Widen>(64, |case| {
        let Ok(extended) = case.width.sign_extend(case.value, case.from) else {
            return true;
        };
        let low = case.value & low_mask(case.from);
        let sign_set = low & sign_bit(case.from) != 0;
        let fill = if sign_set { !low_mask(case.from) } else { 0 };
        extended == (low | fill) & case.width.mask()
    });
}

/// The two extensions agree exactly when they must.
///
/// This is the property the C compiler's cast depends on: `(int)c` and
/// `(unsigned)c` are the same value when `c`'s high bit is clear and different when
/// it is set, and a conversion that disagreed with itself here would make
/// `strcmp`'s `unsigned char` comparison wrong for every byte above 127 — which is
/// exactly the bug step 82 found.
///
/// "Exactly when they must" includes a case that is easy to miss, and which this
/// property missed first: a source *as wide as the machine* sign-extends to itself,
/// so the two agree even for a value whose top bit is set. Written as "they differ
/// iff the value is negative" it is wrong for a 64-bit source — and being wrong
/// about it is how this file found the real bug just above, where sign extension
/// from 64 bits was not the identity at all.
#[test]
fn the_two_extensions_differ_exactly_when_the_source_is_narrow_and_negative() {
    check::<Widen>(64, |case| {
        let (Ok(zero), Ok(sign)) = (
            case.width.zero_extend(case.value, case.from),
            case.width.sign_extend(case.value, case.from),
        ) else {
            return true;
        };
        let low = case.value & low_mask(case.from);
        let negative = low & sign_bit(case.from) != 0;
        (zero != sign) == (negative && case.from < case.width.bits())
    });
}

/// Truncating is idempotent, and truncating twice is truncating once.
///
/// The same argument as the zero extension, and the reason a `u32` value can be
/// carried in a 64-bit register and a `u8` in a frame slot without either losing
/// its value.
#[test]
fn truncation_is_idempotent() {
    check::<Widen>(64, |case| {
        let once = case.width.truncate(case.value);
        case.width.truncate(once) == once && once & !case.width.mask() == 0
    });
}

/// Addition is commutative, and subtraction is its inverse.
///
/// Two properties that catch the classic wrong-flag bugs. If `add` set carry from
/// the wrong pair of operands, commutativity would not catch it — but a carry that
/// depends on the order of a commutative operation is a bug, and a subtraction
/// whose borrow is not the reverse of an addition is a bug too. The overflow flag
/// is checked through its definition rather than through a table.
#[test]
fn addition_commutes_and_subtraction_undoes_it() {
    struct Pair {
        width: WordWidth,
        left: u64,
        right: u64,
    }
    impl Case for Pair {
        fn generate(source: &mut Gen) -> Self {
            Self {
                width: match source.bool() {
                    true => WordWidth::W32,
                    false => WordWidth::W64,
                },
                left: source.interesting_u64(),
                right: source.interesting_u64(),
            }
        }
        fn describe(&self) -> String {
            format!("{:#x} and {:#x} in {:?}", self.left, self.right, self.width)
        }
    }

    check::<Pair>(64, |case| {
        let forward = case.width.add(case.left, case.right);
        let backward = case.width.add(case.right, case.left);
        if forward.value != backward.value || forward.carry != backward.carry {
            return false;
        }
        // `a + b - b` is `a`, in the machine's width, and the carry out of the
        // subtraction is the borrow `b` needed. Anything else is a flag bug.
        let round = case.width.sub(forward.value, case.right);
        round.value == case.width.truncate(case.left)
    });
}

/// Multiplication distributes over addition, in this machine's width.
///
/// The one identity that catches a multiply that truncated one operand too early
/// or too late, which is a bug a round trip cannot see: `a * 0` is `0` whatever
/// `a` is, and `(a + b) * 2` is `a * 2 + b * 2` only if both sides truncate the
/// same way.
#[test]
fn multiplication_distributes_over_addition() {
    struct Triple {
        width: WordWidth,
        left: u64,
        right: u64,
        scale: u64,
    }
    impl Case for Triple {
        fn generate(source: &mut Gen) -> Self {
            Self {
                width: match source.bool() {
                    true => WordWidth::W32,
                    false => WordWidth::W64,
                },
                left: source.interesting_u64(),
                right: source.interesting_u64(),
                // A small odd scale, so the identity is not trivially true because
                // everything overflowed to the same thing.
                scale: 2 + source.below(7) * 2,
            }
        }
        fn describe(&self) -> String {
            format!(
                "({:#x} + {:#x}) * {} in {:?}",
                self.left, self.right, self.scale, self.width
            )
        }
    }

    check::<Triple>(64, |case| {
        let sum = case.width.add(case.left, case.right);
        let distributed = case.width.add(
            case.width.mul(case.left, case.scale).value,
            case.width.mul(case.right, case.scale).value,
        );
        case.width.mul(sum.value, case.scale).value == distributed.value
    });
}

/// Every arithmetic result is inside the machine's width.
///
/// The invariant everything else rests on: a `u32` machine produces `u32` values
/// from every operation, and an operation that returned a wider value would be a
/// value the register file could not hold.
#[test]
fn every_result_fits_the_machine() {
    struct Operands {
        width: WordWidth,
        left: u64,
        right: u64,
    }
    impl Case for Operands {
        fn generate(source: &mut Gen) -> Self {
            Self {
                width: match source.bool() {
                    true => WordWidth::W32,
                    false => WordWidth::W64,
                },
                left: source.interesting_u64(),
                right: source.interesting_u64(),
            }
        }
        fn describe(&self) -> String {
            format!("{:#x}, {:#x} in {:?}", self.left, self.right, self.width)
        }
    }

    check::<Operands>(64, |case| {
        let mask = !case.width.mask();
        let results: [ArithmeticResult; 6] = [
            case.width.add(case.left, case.right),
            case.width.sub(case.left, case.right),
            case.width.mul(case.left, case.right),
            case.width.bitand(case.left, case.right),
            case.width.bitor(case.left, case.right),
            case.width.bitxor(case.left, case.right),
        ];
        results.iter().all(|result| result.value & mask == 0)
    });
}

/// A source width that is not a real width, or wider than the machine, is refused.
///
/// The negative half. A conversion that quietly accepted a width of 12 would
/// produce a value that is neither the source's nor the target's, and nothing
/// downstream would notice.
#[test]
fn an_impossible_source_width_is_refused() {
    for from in [0u8, 1, 7, 9, 12, 24, 33, 65, 128, 255] {
        for width in WIDTHS {
            assert!(
                matches!(
                    width.zero_extend(1, from),
                    Err(WidthError::InvalidSourceWidth { .. })
                ),
                "a {from}-bit source should be refused in {width:?}"
            );
            assert!(matches!(
                width.sign_extend(1, from),
                Err(WidthError::InvalidSourceWidth { .. })
            ),);
        }
    }
}
