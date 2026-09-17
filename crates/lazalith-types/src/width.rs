use crate::WordWidth;
use core::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArithmeticResult {
    pub value: u64,
    pub negative: bool,
    pub zero: bool,
    pub carry: bool,
    pub overflow: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WidthError {
    InvalidSourceWidth {
        value: u64,
        source_bits: u8,
        width: WordWidth,
    },
    DivisionByZero {
        left: u64,
        right: u64,
        width: WordWidth,
    },
    SignedDivisionOverflow {
        left: u64,
        right: u64,
        width: WordWidth,
    },
    AddressOutOfRange {
        value: u64,
        width: WordWidth,
    },
    InvalidOffsetBase {
        base: u64,
        delta: i64,
        width: WordWidth,
    },
    AddressOffsetOutOfRange {
        base: u64,
        delta: i64,
        width: WordWidth,
    },
    InvalidAccessBase {
        base: u64,
        size: u64,
        width: WordWidth,
    },
    ZeroAccessSize {
        base: u64,
        size: u64,
        width: WordWidth,
    },
    AccessEndOutOfRange {
        base: u64,
        size: u64,
        width: WordWidth,
    },
}

impl fmt::Display for WidthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourceWidth {
                value,
                source_bits,
                width,
            } => write!(
                f,
                "invalid source width {source_bits} for value {value:#x} at {} bits",
                width.bits()
            ),
            Self::DivisionByZero { left, right, width } => write!(
                f,
                "division by zero for operands {left:#x}, {right:#x} at {} bits",
                width.bits()
            ),
            Self::SignedDivisionOverflow { left, right, width } => write!(
                f,
                "signed division overflow for operands {left:#x}, {right:#x} at {} bits",
                width.bits()
            ),
            Self::AddressOutOfRange { value, width } => write!(
                f,
                "address {value:#x} is outside the {}-bit range",
                width.bits()
            ),
            Self::InvalidOffsetBase { base, delta, width } => write!(
                f,
                "invalid offset base {base:#x} with delta {delta} at {} bits",
                width.bits()
            ),
            Self::AddressOffsetOutOfRange { base, delta, width } => write!(
                f,
                "address offset from {base:#x} by {delta} is outside the {}-bit range",
                width.bits()
            ),
            Self::InvalidAccessBase { base, size, width } => write!(
                f,
                "invalid access base {base:#x} with size {size} at {} bits",
                width.bits()
            ),
            Self::ZeroAccessSize { base, size, width } => write!(
                f,
                "access at {base:#x} has nonpositive size {size} at {} bits",
                width.bits()
            ),
            Self::AccessEndOutOfRange { base, size, width } => write!(
                f,
                "access at {base:#x} with size {size} ends outside the {}-bit range",
                width.bits()
            ),
        }
    }
}

impl Error for WidthError {}

impl WordWidth {
    pub const fn mask(self) -> u64 {
        match self {
            Self::W32 => u32::MAX as u64,
            Self::W64 => u64::MAX,
        }
    }

    pub const fn truncate(self, value: u64) -> u64 {
        value & self.mask()
    }

    pub fn zero_extend(self, value: u64, source_bits: u8) -> Result<u64, WidthError> {
        if !matches!(source_bits, 8 | 16 | 32 | 64) || source_bits > self.bits() {
            return Err(WidthError::InvalidSourceWidth {
                value,
                source_bits,
                width: self,
            });
        }
        Ok(value & (u64::MAX >> (64 - source_bits)))
    }

    pub fn sign_extend(self, value: u64, source_bits: u8) -> Result<u64, WidthError> {
        let low = self.zero_extend(value, source_bits)?;
        let sign = 1u64 << (source_bits - 1);
        Ok(self.truncate((low ^ sign).wrapping_sub(sign)))
    }

    pub const fn mask_address_bits(self, value: u64) -> u64 {
        self.truncate(value)
    }

    pub fn wrapping_add(self, left: u64, right: u64) -> u64 {
        self.truncate(self.truncate(left).wrapping_add(self.truncate(right)))
    }

    pub fn wrapping_sub(self, left: u64, right: u64) -> u64 {
        self.truncate(self.truncate(left).wrapping_sub(self.truncate(right)))
    }

    pub fn wrapping_mul(self, left: u64, right: u64) -> u64 {
        self.truncate(self.truncate(left).wrapping_mul(self.truncate(right)))
    }

    fn result(self, value: u64) -> ArithmeticResult {
        let value = self.truncate(value);
        ArithmeticResult {
            value,
            negative: value & self.sign_bit() != 0,
            zero: value == 0,
            carry: false,
            overflow: false,
        }
    }

    fn sign_bit(self) -> u64 {
        1u64 << (self.bits() - 1)
    }

    fn signed(self, value: u64) -> i128 {
        let value = self.truncate(value);
        if value & self.sign_bit() == 0 {
            i128::from(value)
        } else {
            i128::from(value) - (1i128 << self.bits())
        }
    }

    pub fn add(self, left: u64, right: u64) -> ArithmeticResult {
        let left = self.truncate(left);
        let right = self.truncate(right);
        let mut result = self.result(self.wrapping_add(left, right));
        result.carry = u128::from(left) + u128::from(right) > u128::from(self.mask());
        result.overflow = (!(left ^ right) & (left ^ result.value) & self.sign_bit()) != 0;
        result
    }

    pub fn sub(self, left: u64, right: u64) -> ArithmeticResult {
        let left = self.truncate(left);
        let right = self.truncate(right);
        let mut result = self.result(self.wrapping_sub(left, right));
        result.carry = left < right;
        result.overflow = ((left ^ right) & (left ^ result.value) & self.sign_bit()) != 0;
        result
    }

    pub fn mul(self, left: u64, right: u64) -> ArithmeticResult {
        self.result(self.wrapping_mul(left, right))
    }

    pub fn bitand(self, left: u64, right: u64) -> ArithmeticResult {
        self.result(self.truncate(left) & self.truncate(right))
    }

    pub fn bitor(self, left: u64, right: u64) -> ArithmeticResult {
        self.result(self.truncate(left) | self.truncate(right))
    }

    pub fn bitxor(self, left: u64, right: u64) -> ArithmeticResult {
        self.result(self.truncate(left) ^ self.truncate(right))
    }

    pub fn not(self, value: u64) -> ArithmeticResult {
        self.result(!self.truncate(value))
    }

    pub const fn shift_amount(self, value: u64) -> u8 {
        (self.truncate(value) % self.bits() as u64) as u8
    }

    pub fn shl(self, value: u64, amount: u64) -> ArithmeticResult {
        self.result(self.truncate(value) << self.shift_amount(amount))
    }

    pub fn shr(self, value: u64, amount: u64) -> ArithmeticResult {
        self.result(self.truncate(value) >> self.shift_amount(amount))
    }

    pub fn sar(self, value: u64, amount: u64) -> ArithmeticResult {
        self.result((self.signed(value) >> self.shift_amount(amount)) as u64)
    }

    fn divide(
        self,
        left: u64,
        right: u64,
        signed: bool,
        remainder: bool,
    ) -> Result<ArithmeticResult, WidthError> {
        let numerator = self.truncate(left);
        let denominator = self.truncate(right);
        if denominator == 0 {
            return Err(WidthError::DivisionByZero {
                left,
                right,
                width: self,
            });
        }
        let value = if signed {
            if numerator == self.sign_bit() && denominator == self.mask() {
                return Err(WidthError::SignedDivisionOverflow {
                    left,
                    right,
                    width: self,
                });
            }
            let numerator = self.signed(numerator);
            let denominator = self.signed(denominator);
            (if remainder {
                numerator % denominator
            } else {
                numerator / denominator
            }) as u64
        } else if remainder {
            numerator % denominator
        } else {
            numerator / denominator
        };
        Ok(self.result(value))
    }

    pub fn div_unsigned(self, left: u64, right: u64) -> Result<ArithmeticResult, WidthError> {
        self.divide(left, right, false, false)
    }

    pub fn rem_unsigned(self, left: u64, right: u64) -> Result<ArithmeticResult, WidthError> {
        self.divide(left, right, false, true)
    }

    pub fn div_signed(self, left: u64, right: u64) -> Result<ArithmeticResult, WidthError> {
        self.divide(left, right, true, false)
    }

    pub fn rem_signed(self, left: u64, right: u64) -> Result<ArithmeticResult, WidthError> {
        self.divide(left, right, true, true)
    }

    pub fn validate_address(self, value: u64) -> Result<u64, WidthError> {
        if value > self.mask() {
            Err(WidthError::AddressOutOfRange { value, width: self })
        } else {
            Ok(value)
        }
    }

    pub fn checked_address_offset(self, base: u64, delta: i64) -> Result<u64, WidthError> {
        self.validate_address(base)
            .map_err(|_| WidthError::InvalidOffsetBase {
                base,
                delta,
                width: self,
            })?;
        let address = i128::from(base) + i128::from(delta);
        if address < 0 || address > i128::from(self.mask()) {
            Err(WidthError::AddressOffsetOutOfRange {
                base,
                delta,
                width: self,
            })
        } else {
            Ok(address as u64)
        }
    }

    pub fn checked_access_end(self, base: u64, size: u64) -> Result<u64, WidthError> {
        self.validate_address(base)
            .map_err(|_| WidthError::InvalidAccessBase {
                base,
                size,
                width: self,
            })?;
        if size == 0 {
            return Err(WidthError::ZeroAccessSize {
                base,
                size,
                width: self,
            });
        }
        let last = u128::from(base) + u128::from(size) - 1;
        if last > u128::from(self.mask()) {
            Err(WidthError::AccessEndOutOfRange {
                base,
                size,
                width: self,
            })
        } else {
            Ok(last as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ArithmeticResult, WidthError, WordWidth};
    use alloc::{format, string::ToString, vec::Vec};
    use core::error::Error;

    const WIDTHS: [WordWidth; 2] = [WordWidth::W32, WordWidth::W64];

    fn modulus(width: WordWidth) -> u128 {
        2u128.pow(u32::from(width.bits()))
    }

    fn signed_reference(value: u64, width: WordWidth) -> i128 {
        let modulus = modulus(width) as i128;
        let value = i128::from(value) % modulus;
        if value >= modulus / 2 {
            value - modulus
        } else {
            value
        }
    }

    fn expected(width: WordWidth, value: u64, carry: bool, overflow: bool) -> ArithmeticResult {
        assert!(u128::from(value) < modulus(width));
        ArithmeticResult {
            value,
            negative: u128::from(value) >= modulus(width) / 2,
            zero: value == 0,
            carry,
            overflow,
        }
    }

    fn grid(width: WordWidth) -> Vec<u64> {
        let m = (modulus(width) - 1) as u64;
        let s = (modulus(width) / 2) as u64;
        let mut values = alloc::vec![
            0,
            1,
            2,
            3,
            7,
            8,
            15,
            16,
            s - 2,
            s - 1,
            s,
            s + 1,
            m - 2,
            m - 1,
            m,
            u32::MAX as u64,
            1u64 << 32,
            0xffff_ffff_0000_0000,
            0xaaaa_aaaa_aaaa_aaaa,
            0x5555_5555_5555_5555,
            u64::MAX,
        ];
        for bit in 0..64 {
            let power = 1u64 << bit;
            values.extend([power - 1, power, power + 1, !power]);
        }
        values.sort_unstable();
        values.dedup();
        values
    }

    #[test]
    fn mask_truncate_and_address_mask_tables() {
        for (width, m) in [(WordWidth::W32, 0xffff_ffff), (WordWidth::W64, u64::MAX)] {
            assert_eq!(width.mask(), m);
            for (input, low32) in [
                (0, 0),
                (1, 1),
                (0x8000_0000, 0x8000_0000),
                (0xffff_ffff, 0xffff_ffff),
                (0x1_0000_0000, 0),
                (0x1234_5678_9abc_def0, 0x9abc_def0),
                (u64::MAX, 0xffff_ffff),
            ] {
                let value = if width == WordWidth::W32 {
                    low32
                } else {
                    input
                };
                assert_eq!(width.truncate(input), value);
                assert_eq!(width.mask_address_bits(input), value);
                assert_eq!(width.truncate(value), value);
            }
        }
    }

    #[test]
    fn every_source_width_is_validated_and_errors_retain_inputs() {
        for width in WIDTHS {
            for source_bits in 0..=u8::MAX {
                for value in [0, 0x180, u64::MAX] {
                    let valid =
                        matches!(source_bits, 8 | 16 | 32 | 64) && source_bits <= width.bits();
                    if valid {
                        assert!(width.zero_extend(value, source_bits).is_ok());
                        assert!(width.sign_extend(value, source_bits).is_ok());
                    } else {
                        let error = WidthError::InvalidSourceWidth {
                            value,
                            source_bits,
                            width,
                        };
                        assert_eq!(width.zero_extend(value, source_bits), Err(error));
                        assert_eq!(width.sign_extend(value, source_bits), Err(error));
                        assert!(error.source().is_none());
                    }
                }
            }
        }
    }

    #[test]
    fn extension_boundaries_ignore_bits_above_source() {
        for width in WIDTHS {
            for source_bits in [8, 16, 32, 64] {
                if source_bits > width.bits() {
                    continue;
                }
                let source_modulus = 2u128.pow(u32::from(source_bits));
                let sign = (source_modulus / 2) as u64;
                let mask = (source_modulus - 1) as u64;
                for value in [0, 1, sign - 1, sign, sign + 1, mask] {
                    for input in [value, value | !mask] {
                        let signed = if value >= sign {
                            i128::from(value) - source_modulus as i128
                        } else {
                            i128::from(value)
                        };
                        let extended = signed.rem_euclid(modulus(width) as i128) as u64;
                        assert_eq!(width.zero_extend(input, source_bits), Ok(value));
                        assert_eq!(width.sign_extend(input, source_bits), Ok(extended));
                    }
                }
            }
            assert_eq!(width.sign_extend(0x180, 8), Ok(width.mask() - 127));
            assert_eq!(width.zero_extend(0x180, 8), Ok(0x80));
            assert_eq!(
                width.sign_extend(0x8000_0000, 32),
                Ok(width.mask() - 0x7fff_ffff)
            );
            assert_eq!(width.sign_extend(u64::MAX, width.bits()), Ok(width.mask()));
            assert_eq!(width.zero_extend(u64::MAX, width.bits()), Ok(width.mask()));
        }
    }

    #[test]
    fn add_carry_and_signed_overflow_boundary_table() {
        for width in WIDTHS {
            let m = width.mask();
            let s = m / 2 + 1;
            for (left, right, value, carry, overflow) in [
                (0, 0, 0, false, false),
                (m, 1, 0, true, false),
                (s - 1, 1, s, false, true),
                (s, s, 0, true, true),
                (s, m, s - 1, true, true),
                (m, m, m - 1, true, false),
                (s, s - 1, m, false, false),
                (s, 0, s, false, false),
            ] {
                assert_eq!(
                    width.add(left, right),
                    expected(width, value, carry, overflow)
                );
            }
        }
    }

    #[test]
    fn sub_carry_is_borrow_boundary_table() {
        for width in WIDTHS {
            let m = width.mask();
            let s = m / 2 + 1;
            for (left, right, value, carry, overflow) in [
                (0, 0, 0, false, false),
                (0, 1, m, true, false),
                (1, 0, 1, false, false),
                (m, m, 0, false, false),
                (s, 1, s - 1, false, true),
                (s - 1, m, s, true, true),
                (0, s, s, true, true),
                (s, m, s + 1, true, false),
                (m, s, s - 1, false, false),
            ] {
                assert_eq!(
                    width.sub(left, right),
                    expected(width, value, carry, overflow)
                );
            }
        }
    }

    fn arithmetic_reference_grid(width: WordWidth) {
        let modulus = modulus(width);
        let min = -(modulus as i128 / 2);
        let max = modulus as i128 / 2 - 1;
        let values = grid(width);
        for &left in &values {
            for &right in &values {
                let a = u128::from(left) % modulus;
                let b = u128::from(right) % modulus;
                let sa = signed_reference(left, width);
                let sb = signed_reference(right, width);
                let sum = ((a + b) % modulus) as u64;
                let difference = (a as i128 - b as i128).rem_euclid(modulus as i128) as u64;
                let product = ((a * b) % modulus) as u64;
                assert_eq!(width.wrapping_add(left, right), sum);
                assert_eq!(width.wrapping_sub(left, right), difference);
                assert_eq!(width.wrapping_mul(left, right), product);
                assert_eq!(
                    width.add(left, right),
                    expected(
                        width,
                        sum,
                        a + b >= modulus,
                        !(min..=max).contains(&(sa + sb))
                    ),
                    "add {width:?} {left:#x} {right:#x}"
                );
                assert_eq!(
                    width.sub(left, right),
                    expected(width, difference, a < b, !(min..=max).contains(&(sa - sb))),
                    "sub {width:?} {left:#x} {right:#x}"
                );
                assert_eq!(
                    width.mul(left, right),
                    expected(width, product, false, false)
                );
            }
        }
    }

    #[test]
    fn lz32_arithmetic_matches_independent_wide_reference_grid() {
        arithmetic_reference_grid(WordWidth::W32);
    }

    #[test]
    fn lz64_arithmetic_matches_independent_wide_reference_grid() {
        arithmetic_reference_grid(WordWidth::W64);
    }

    #[test]
    fn logic_recomputes_nz_and_clears_cv() {
        for width in WIDTHS {
            let modulus = modulus(width);
            let values = grid(width);
            for &left in &values {
                let a = u128::from(left) % modulus;
                assert_eq!(
                    width.not(left),
                    expected(width, (modulus - 1 - a) as u64, false, false)
                );
                for &right in &values {
                    let b = u128::from(right) % modulus;
                    assert_eq!(
                        width.bitand(left, right),
                        expected(width, (a & b) as u64, false, false)
                    );
                    assert_eq!(
                        width.bitor(left, right),
                        expected(width, (a | b) as u64, false, false)
                    );
                    assert_eq!(
                        width.bitxor(left, right),
                        expected(width, (a ^ b) as u64, false, false)
                    );
                }
            }
        }
    }

    #[test]
    fn normalized_shifts_match_multiplication_and_floor_division() {
        for width in WIDTHS {
            let w = u64::from(width.bits());
            let modulus = modulus(width);
            let amounts = [0, 1, w - 1, w, w + 1, 2 * w, u32::MAX as u64 + 1, u64::MAX];
            for value in grid(width) {
                for amount in amounts.into_iter().chain(0..=2 * w + 1) {
                    let normalized = (u128::from(amount) % modulus) % u128::from(w);
                    let factor = 2u128.pow(normalized as u32);
                    let unsigned = u128::from(value) % modulus;
                    let signed = signed_reference(value, width);
                    let left = ((unsigned * factor) % modulus) as u64;
                    let right = (unsigned / factor) as u64;
                    let arithmetic = signed
                        .div_euclid(factor as i128)
                        .rem_euclid(modulus as i128) as u64;
                    assert_eq!(width.shift_amount(amount), normalized as u8);
                    assert_eq!(
                        width.shl(value, amount),
                        expected(width, left, false, false)
                    );
                    assert_eq!(
                        width.shr(value, amount),
                        expected(width, right, false, false)
                    );
                    assert_eq!(
                        width.sar(value, amount),
                        expected(width, arithmetic, false, false),
                        "sar {width:?} {value:#x} {amount}"
                    );
                }
            }
        }
    }

    #[test]
    fn signed_division_sign_and_truncation_table() {
        for width in WIDTHS {
            for (left, right, quotient, remainder) in [
                (7i64, 3i64, 2i64, 1i64),
                (-7, 3, -2, -1),
                (7, -3, -2, 1),
                (-7, -3, 2, -1),
                (1, 3, 0, 1),
                (-1, 3, 0, -1),
                (1, -3, 0, 1),
                (-1, -3, 0, -1),
                (6, 3, 2, 0),
                (-6, 3, -2, 0),
                (0, -1, 0, 0),
            ] {
                assert_eq!(
                    width.div_signed(left as u64, right as u64),
                    Ok(expected(
                        width,
                        width.truncate(quotient as u64),
                        false,
                        false
                    ))
                );
                assert_eq!(
                    width.rem_signed(left as u64, right as u64),
                    Ok(expected(
                        width,
                        width.truncate(remainder as u64),
                        false,
                        false
                    ))
                );
            }
        }
    }

    #[test]
    fn division_errors_retain_unmasked_operands_for_div_and_rem() {
        for width in WIDTHS {
            for left in grid(width) {
                for right in [0, !width.mask()] {
                    let error = WidthError::DivisionByZero { left, right, width };
                    assert_eq!(width.div_unsigned(left, right), Err(error));
                    assert_eq!(width.rem_unsigned(left, right), Err(error));
                    assert_eq!(width.div_signed(left, right), Err(error));
                    assert_eq!(width.rem_signed(left, right), Err(error));
                }
            }
            for left in [width.mask() / 2 + 1, (width.mask() / 2 + 1) | !width.mask()] {
                for right in [width.mask(), u64::MAX] {
                    let error = WidthError::SignedDivisionOverflow { left, right, width };
                    assert_eq!(width.div_signed(left, right), Err(error));
                    assert_eq!(width.rem_signed(left, right), Err(error));
                    assert_eq!(
                        width.div_unsigned(left, right),
                        Ok(expected(width, 0, false, false))
                    );
                    assert_eq!(
                        width.rem_unsigned(left, right),
                        Ok(expected(width, width.mask() / 2 + 1, false, false))
                    );
                }
            }
        }
    }

    fn division_reference_grid(width: WordWidth) {
        let modulus = modulus(width);
        let values = grid(width);
        for &left in &values {
            for &right in &values {
                let a = u128::from(left) % modulus;
                let b = u128::from(right) % modulus;
                if b == 0 {
                    continue;
                }
                assert_eq!(
                    width.div_unsigned(left, right),
                    Ok(expected(width, (a / b) as u64, false, false))
                );
                assert_eq!(
                    width.rem_unsigned(left, right),
                    Ok(expected(width, (a % b) as u64, false, false))
                );
                let a = signed_reference(left, width);
                let b = signed_reference(right, width);
                if a == -(modulus as i128 / 2) && b == -1 {
                    continue;
                }
                let magnitude = a.abs() / b.abs();
                let quotient = if (a < 0) != (b < 0) {
                    -magnitude
                } else {
                    magnitude
                };
                let remainder = a - quotient * b;
                assert!(remainder.abs() < b.abs());
                assert_eq!(
                    width.div_signed(left, right),
                    Ok(expected(
                        width,
                        quotient.rem_euclid(modulus as i128) as u64,
                        false,
                        false
                    ))
                );
                assert_eq!(
                    width.rem_signed(left, right),
                    Ok(expected(
                        width,
                        remainder.rem_euclid(modulus as i128) as u64,
                        false,
                        false
                    ))
                );
            }
        }
    }

    #[test]
    fn lz32_division_matches_independent_wide_reference_grid() {
        division_reference_grid(WordWidth::W32);
    }

    #[test]
    fn lz64_division_matches_independent_wide_reference_grid() {
        division_reference_grid(WordWidth::W64);
    }

    #[test]
    fn address_validation_never_masks_or_changes_domains() {
        for width in WIDTHS {
            for value in grid(width) {
                let expected = if u128::from(value) < modulus(width) {
                    Ok(value)
                } else {
                    Err(WidthError::AddressOutOfRange { value, width })
                };
                assert_eq!(width.validate_address(value), expected);
            }
        }
        let value = 1u64 << 32;
        assert_eq!(WordWidth::W32.mask_address_bits(value), 0);
        assert!(WordWidth::W32.validate_address(value).is_err());
        assert_eq!(crate::PhysicalAddress::new(value).as_u64(), value);
        assert_eq!(crate::VirtualAddress::new(value).as_u64(), value);
        assert_eq!(crate::InstructionAddress::new(value).as_u64(), value);
    }

    #[test]
    fn offset_boundaries_include_high_addresses_and_scaled_displacements() {
        for width in WIDTHS {
            let m = width.mask();
            for (base, delta, result) in [
                (0, 0, Some(0)),
                (0, 1, Some(1)),
                (0, -1, None),
                (m, 0, Some(m)),
                (m, 1, None),
                (m, -1, Some(m - 1)),
                (m - 1, 1, Some(m)),
                (1, -1, Some(0)),
            ] {
                assert_eq!(
                    width.checked_address_offset(base, delta),
                    result.ok_or(WidthError::AddressOffsetOutOfRange { base, delta, width })
                );
            }
            for base in grid(width) {
                for delta in [
                    i64::MIN,
                    i64::MIN + 1,
                    i64::MAX,
                    i64::from(i32::MIN) * 4,
                    i64::from(i32::MAX) * 4,
                    -8,
                    -1,
                    0,
                    1,
                    8,
                ] {
                    let result = if delta < 0 {
                        base.checked_sub(delta.unsigned_abs())
                    } else {
                        base.checked_add(delta as u64)
                    };
                    let expected = if base > m {
                        Err(WidthError::InvalidOffsetBase { base, delta, width })
                    } else {
                        result
                            .filter(|&address| address <= m)
                            .ok_or(WidthError::AddressOffsetOutOfRange { base, delta, width })
                    };
                    assert_eq!(width.checked_address_offset(base, delta), expected);
                }
            }
        }
        let width = WordWidth::W64;
        assert_eq!(width.checked_address_offset(1u64 << 63, i64::MIN), Ok(0));
        assert_eq!(
            width.checked_address_offset(u64::MAX, i64::MIN),
            Ok(i64::MAX as u64)
        );
        assert_eq!(
            width.checked_address_offset(1u64 << 63, i64::MAX),
            Ok(u64::MAX)
        );
        assert_eq!(
            width.checked_address_offset(0x8000_0000_0000_0010, -8),
            Ok(0x8000_0000_0000_0008)
        );
    }

    #[test]
    fn access_end_is_last_address_with_positive_u64_size() {
        for width in WIDTHS {
            let m = width.mask();
            for (base, size, result) in [
                (0, 1, Some(0)),
                (0, m, Some(m - 1)),
                (1, m, Some(m)),
                (2, m, None),
                (m, 1, Some(m)),
                (m, 2, None),
                (m - 7, 8, Some(m)),
                (m - 7, 9, None),
            ] {
                assert_eq!(
                    width.checked_access_end(base, size),
                    result.ok_or(WidthError::AccessEndOutOfRange { base, size, width })
                );
            }
            for base in grid(width) {
                for size in [
                    0,
                    1,
                    2,
                    3,
                    4,
                    8,
                    m,
                    u32::MAX as u64 + 1,
                    u64::MAX - 1,
                    u64::MAX,
                ] {
                    let expected = if base > m {
                        Err(WidthError::InvalidAccessBase { base, size, width })
                    } else if size == 0 {
                        Err(WidthError::ZeroAccessSize { base, size, width })
                    } else {
                        base.checked_add(size - 1)
                            .filter(|&last| last <= m)
                            .ok_or(WidthError::AccessEndOutOfRange { base, size, width })
                    };
                    assert_eq!(width.checked_access_end(base, size), expected);
                }
            }
        }
        assert_eq!(
            WordWidth::W32.checked_access_end(0, 1u64 << 32),
            Ok(u32::MAX as u64)
        );
        assert_eq!(WordWidth::W64.checked_access_end(1, u64::MAX), Ok(u64::MAX));
        assert_eq!(
            WordWidth::W64.checked_access_end(1u64 << 63, 1u64 << 63),
            Ok(u64::MAX)
        );
    }

    #[test]
    fn invalid_address_base_wins_even_when_offset_repairs_it_or_size_is_zero() {
        let width = WordWidth::W32;
        let base = 1u64 << 32;
        assert_eq!(
            width.checked_address_offset(base, -1),
            Err(WidthError::InvalidOffsetBase {
                base,
                delta: -1,
                width
            })
        );
        assert_eq!(
            width.checked_access_end(base, 0),
            Err(WidthError::InvalidAccessBase {
                base,
                size: 0,
                width
            })
        );
    }

    #[test]
    fn all_error_variants_display_context_and_implement_error() {
        for width in WIDTHS {
            let bits = width.bits();
            let cases = [
                (
                    WidthError::InvalidSourceWidth {
                        value: 0x180,
                        source_bits: 7,
                        width,
                    },
                    format!("invalid source width 7 for value 0x180 at {bits} bits"),
                ),
                (
                    WidthError::DivisionByZero {
                        left: 1,
                        right: 0,
                        width,
                    },
                    format!("division by zero for operands 0x1, 0x0 at {bits} bits"),
                ),
                (
                    WidthError::SignedDivisionOverflow {
                        left: 2,
                        right: 3,
                        width,
                    },
                    format!("signed division overflow for operands 0x2, 0x3 at {bits} bits"),
                ),
                (
                    WidthError::AddressOutOfRange { value: 4, width },
                    format!("address 0x4 is outside the {bits}-bit range"),
                ),
                (
                    WidthError::InvalidOffsetBase {
                        base: 5,
                        delta: -6,
                        width,
                    },
                    format!("invalid offset base 0x5 with delta -6 at {bits} bits"),
                ),
                (
                    WidthError::AddressOffsetOutOfRange {
                        base: 7,
                        delta: -8,
                        width,
                    },
                    format!("address offset from 0x7 by -8 is outside the {bits}-bit range"),
                ),
                (
                    WidthError::InvalidAccessBase {
                        base: 9,
                        size: 10,
                        width,
                    },
                    format!("invalid access base 0x9 with size 10 at {bits} bits"),
                ),
                (
                    WidthError::ZeroAccessSize {
                        base: 11,
                        size: 0,
                        width,
                    },
                    format!("access at 0xb has nonpositive size 0 at {bits} bits"),
                ),
                (
                    WidthError::AccessEndOutOfRange {
                        base: 12,
                        size: 13,
                        width,
                    },
                    format!("access at 0xc with size 13 ends outside the {bits}-bit range"),
                ),
            ];
            for (error, message) in cases {
                assert_eq!(error.to_string(), message);
                assert!(error.source().is_none());
            }
        }
    }
}
