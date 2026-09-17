use crate::OperandKind;
use core::{error::Error, fmt, num::TryFromIntError};
use lazalith_types::RegisterIndex;

macro_rules! selectors {
    ($name:ident, $kind:ident, $( $variant:ident = $value:literal; )*) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(u8)]
        pub enum $name { $( $variant = $value, )* }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn as_u8(self) -> u8 { self as u8 }
        }

        impl TryFrom<u8> for $name {
            type Error = OperandError;

            fn try_from(input: u8) -> Result<Self, Self::Error> {
                match input {
                    $($value => Ok(Self::$variant),)*
                    _ => Err(OperandError::InvalidSelector { kind: OperandKind::$kind, input }),
                }
            }
        }
    };
}

selectors! { DataSize, DataSize,
    Byte = 0; Half = 1; Word = 2; Double = 3;
}

impl DataSize {
    pub const fn bytes(self) -> u8 {
        1 << self.as_u8()
    }
}

selectors! { Condition, Condition,
    Al = 0; Eq = 1; Ne = 2; Ult = 3; Uge = 4; Ule = 5; Ugt = 6;
    Slt = 7; Sge = 8; Sle = 9; Sgt = 10; Vs = 11; Vc = 12; Mi = 13; Pl = 14;
}

selectors! { ControlRegister, Control,
    Tvec = 0; Epc = 1; Esp = 2; Estatus = 3; Tcause = 4; Tpayload = 5;
}

impl ControlRegister {
    pub const fn is_writable(self) -> bool {
        !matches!(self, Self::Tcause | Self::Tpayload)
    }

    pub const fn requires_active_frame(self) -> bool {
        !matches!(self, Self::Tvec)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Operand {
    Register(RegisterIndex),
    Immediate(i32),
    Memory {
        base: RegisterIndex,
        displacement: i32,
    },
    DataSize(DataSize),
    Condition(Condition),
    Control(ControlRegister),
}

impl Operand {
    pub const fn kind(self) -> OperandKind {
        match self {
            Self::Register(_) => OperandKind::Register,
            Self::Immediate(_) => OperandKind::Immediate,
            Self::Memory { .. } => OperandKind::Memory,
            Self::DataSize(_) => OperandKind::DataSize,
            Self::Condition(_) => OperandKind::Condition,
            Self::Control(_) => OperandKind::Control,
        }
    }

    pub fn try_immediate(input: i64) -> Result<Self, OperandError> {
        checked_immediate(input).map(Self::Immediate)
    }

    pub fn try_memory(base: RegisterIndex, displacement: i64) -> Result<Self, OperandError> {
        Ok(Self::Memory {
            base,
            displacement: checked_immediate(displacement)?,
        })
    }
}

fn checked_immediate(input: i64) -> Result<i32, OperandError> {
    i32::try_from(input).map_err(|source| OperandError::ImmediateOutOfRange { input, source })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandError {
    InvalidSelector { kind: OperandKind, input: u8 },
    ImmediateOutOfRange { input: i64, source: TryFromIntError },
}

impl fmt::Display for OperandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSelector { kind, input } => write!(f, "invalid {kind:?} selector {input}"),
            Self::ImmediateOutOfRange { input, .. } => {
                write!(f, "immediate {input} is outside signed 32-bit range")
            }
        }
    }
}

impl Error for OperandError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ImmediateOutOfRange { source, .. } => Some(source),
            Self::InvalidSelector { .. } => None,
        }
    }
}
