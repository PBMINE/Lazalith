use core::{error::Error, fmt};
use lazalith_isa::Condition;
use lazalith_types::{ArithmeticResult, WordWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Privilege {
    Supervisor,
    User,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusRegister(u8);

impl StatusRegister {
    const N: u8 = 1 << 0;
    const Z: u8 = 1 << 1;
    const C: u8 = 1 << 2;
    const V: u8 = 1 << 3;
    const IE: u8 = 1 << 4;
    const U: u8 = 1 << 5;
    const NZCV: u8 = Self::N | Self::Z | Self::C | Self::V;

    pub const fn new(privilege: Privilege, interrupts_enabled: bool) -> Self {
        Self(
            (match privilege {
                Privilege::Supervisor => 0,
                Privilege::User => Self::U,
            }) | if interrupts_enabled { Self::IE } else { 0 },
        )
    }

    pub const fn negative(self) -> bool {
        self.0 & Self::N != 0
    }
    pub const fn zero(self) -> bool {
        self.0 & Self::Z != 0
    }
    pub const fn carry(self) -> bool {
        self.0 & Self::C != 0
    }
    pub const fn overflow(self) -> bool {
        self.0 & Self::V != 0
    }
    pub const fn interrupts_enabled(self) -> bool {
        self.0 & Self::IE != 0
    }

    pub fn set_interrupts_enabled(&mut self, enabled: bool) {
        self.0 = (self.0 & !Self::IE) | if enabled { Self::IE } else { 0 };
    }

    pub fn set_privilege(&mut self, privilege: Privilege) {
        self.0 = (self.0 & !Self::U)
            | match privilege {
                Privilege::Supervisor => 0,
                Privilege::User => Self::U,
            };
    }

    pub fn update_arithmetic(&mut self, result: ArithmeticResult) {
        self.0 = (self.0 & !Self::NZCV)
            | (u8::from(result.negative) * Self::N)
            | (u8::from(result.zero) * Self::Z)
            | (u8::from(result.carry) * Self::C)
            | (u8::from(result.overflow) * Self::V);
    }

    pub const fn matches(self, condition: Condition) -> bool {
        match condition {
            Condition::Al => true,
            Condition::Eq => self.zero(),
            Condition::Ne => !self.zero(),
            Condition::Ult => self.carry(),
            Condition::Uge => !self.carry(),
            Condition::Ule => self.carry() || self.zero(),
            Condition::Ugt => !self.carry() && !self.zero(),
            Condition::Slt => self.negative() != self.overflow(),
            Condition::Sge => self.negative() == self.overflow(),
            Condition::Sle => self.zero() || self.negative() != self.overflow(),
            Condition::Sgt => !self.zero() && self.negative() == self.overflow(),
            Condition::Vs => self.overflow(),
            Condition::Vc => !self.overflow(),
            Condition::Mi => self.negative(),
            Condition::Pl => !self.negative(),
        }
    }

    pub fn try_from_bits(width: WordWidth, input: u64) -> Result<Self, InvalidStatus> {
        if input & !0x3f != 0 {
            return Err(InvalidStatus { input, width });
        }
        Ok(Self(input as u8))
    }

    pub const fn bits(self) -> u64 {
        self.0 as u64
    }

    pub const fn privilege(self) -> Privilege {
        if self.0 & 0x20 == 0 {
            Privilege::Supervisor
        } else {
            Privilege::User
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidStatus {
    pub input: u64,
    pub width: WordWidth,
}

impl fmt::Display for InvalidStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid status {:#x} at {} bits: reserved bits must be zero",
            self.input,
            self.width.bits()
        )
    }
}

impl Error for InvalidStatus {}
