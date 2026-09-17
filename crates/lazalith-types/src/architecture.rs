use core::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PhysicalAddress(u64);

impl PhysicalAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, offset: u64) -> Option<Self> {
        self.0.checked_add(offset).map(Self)
    }

    pub fn checked_sub(self, offset: u64) -> Option<Self> {
        self.0.checked_sub(offset).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VirtualAddress(u64);

impl VirtualAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, offset: u64) -> Option<Self> {
        self.0.checked_add(offset).map(Self)
    }

    pub fn checked_sub(self, offset: u64) -> Option<Self> {
        self.0.checked_sub(offset).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstructionAddress(u64);

impl InstructionAddress {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, offset: u64) -> Option<Self> {
        self.0.checked_add(offset).map(Self)
    }

    pub fn checked_sub(self, offset: u64) -> Option<Self> {
        self.0.checked_sub(offset).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(u32);

impl DeviceId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceOffset(u64);

impl DeviceOffset {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CycleCount(u64);

impl CycleCount {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct InstructionCount(u64);

impl InstructionCount {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegisterIndex(u8);

impl RegisterIndex {
    pub const COUNT: u8 = 16;

    pub const fn as_u8(self) -> u8 {
        self.0
    }

    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl TryFrom<u8> for RegisterIndex {
    type Error = InvalidRegisterIndex;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::COUNT {
            Ok(Self(value))
        } else {
            Err(InvalidRegisterIndex { input: value })
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRegisterIndex {
    input: u8,
}

impl InvalidRegisterIndex {
    pub const fn input(&self) -> u8 {
        self.input
    }
}

impl fmt::Display for InvalidRegisterIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "register index {} is outside 0..{}",
            self.input,
            RegisterIndex::COUNT
        )
    }
}

impl Error for InvalidRegisterIndex {}

#[cfg(test)]
mod tests {
    use crate::{
        CycleCount, DeviceId, DeviceOffset, InstructionAddress, InstructionCount,
        InvalidRegisterIndex, PhysicalAddress, RegisterIndex, VirtualAddress,
    };
    use alloc::{format, string::ToString};
    use core::error::Error;

    macro_rules! address_boundaries {
        ($name:ident, $ty:ty) => {
            #[test]
            fn $name() {
                for value in [0, 1, u32::MAX as u64, u32::MAX as u64 + 1, u64::MAX] {
                    assert_eq!(<$ty>::new(value).as_u64(), value);
                }
                for (value, offset, sum, difference) in [
                    (0, 0, Some(0), Some(0)),
                    (0, 1, Some(1), None),
                    (0, u64::MAX, Some(u64::MAX), None),
                    (1, u64::MAX, None, None),
                    (42, 7, Some(49), Some(35)),
                    (u64::MAX - 1, 1, Some(u64::MAX), Some(u64::MAX - 2)),
                    (u64::MAX, 0, Some(u64::MAX), Some(u64::MAX)),
                    (u64::MAX, 1, None, Some(u64::MAX - 1)),
                    (u64::MAX, u64::MAX, None, Some(0)),
                ] {
                    let address = <$ty>::new(value);
                    assert_eq!(address.checked_add(offset), sum.map(<$ty>::new));
                    assert_eq!(address.checked_sub(offset), difference.map(<$ty>::new));
                    assert_eq!(address.as_u64(), value);
                }
            }
        };
    }

    address_boundaries!(physical_address_boundaries, PhysicalAddress);
    address_boundaries!(virtual_address_boundaries, VirtualAddress);
    address_boundaries!(instruction_address_boundaries, InstructionAddress);

    macro_rules! quantity_boundaries {
        ($name:ident, $ty:ty) => {
            #[test]
            fn $name() {
                for value in [0, 1, u32::MAX as u64, u32::MAX as u64 + 1, u64::MAX] {
                    assert_eq!(<$ty>::new(value).as_u64(), value);
                }
                for (left, right, sum, difference) in [
                    (0, 0, Some(0), Some(0)),
                    (0, 1, Some(1), None),
                    (0, u64::MAX, Some(u64::MAX), None),
                    (1, u64::MAX, None, None),
                    (42, 7, Some(49), Some(35)),
                    (u64::MAX - 1, 1, Some(u64::MAX), Some(u64::MAX - 2)),
                    (u64::MAX, 0, Some(u64::MAX), Some(u64::MAX)),
                    (u64::MAX, 1, None, Some(u64::MAX - 1)),
                    (u64::MAX, u64::MAX, None, Some(0)),
                ] {
                    let left = <$ty>::new(left);
                    let right = <$ty>::new(right);
                    assert_eq!(left.checked_add(right), sum.map(<$ty>::new));
                    assert_eq!(left.checked_sub(right), difference.map(<$ty>::new));
                }
            }
        };
    }

    quantity_boundaries!(device_offset_boundaries, DeviceOffset);
    quantity_boundaries!(cycle_count_boundaries, CycleCount);
    quantity_boundaries!(instruction_count_boundaries, InstructionCount);

    #[test]
    fn device_id_preserves_full_u32_range() {
        for value in [0, 1, u32::MAX] {
            assert_eq!(DeviceId::new(value).as_u32(), value);
        }
        assert!(DeviceId::new(0) < DeviceId::new(u32::MAX));
    }

    #[test]
    fn register_index_accepts_exactly_sixteen_encodings() {
        assert_eq!(RegisterIndex::COUNT, 16);
        for value in 0..16 {
            let index = RegisterIndex::try_from(value).unwrap();
            assert_eq!(index.as_u8(), value);
            assert_eq!(index.as_usize(), usize::from(value));
        }
    }

    #[test]
    fn register_index_rejects_and_retains_every_invalid_encoding() {
        for value in 16..=u8::MAX {
            let error: InvalidRegisterIndex = RegisterIndex::try_from(value).unwrap_err();
            assert_eq!(error.input(), value);
            assert_eq!(
                error.to_string(),
                format!("register index {value} is outside 0..16")
            );
            assert!(error.source().is_none());
        }
    }
}
