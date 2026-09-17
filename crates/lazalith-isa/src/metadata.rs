use core::{error::Error, fmt};
use lazalith_types::FeatureSet;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OperandKind {
    Register,
    Immediate,
    Memory,
    DataSize,
    Condition,
    Control,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EncodingField {
    D,
    A,
    Breg,
    X,
    I,
}

impl EncodingField {
    pub const fn shift(self) -> u8 {
        match self {
            Self::D => 8,
            Self::A => 12,
            Self::Breg => 16,
            Self::X => 20,
            Self::I => 32,
        }
    }

    pub const fn mask(self) -> u64 {
        match self {
            Self::I => 0xffff_ffff_0000_0000,
            _ => 0xf << self.shift(),
        }
    }

    pub(crate) fn extract(self, bits: u64) -> u32 {
        ((bits & self.mask()) >> self.shift()) as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperandDefinition {
    pub kind: OperandKind,
    pub fields: &'static [EncodingField],
}

const RD: OperandDefinition = OperandDefinition {
    kind: OperandKind::Register,
    fields: &[EncodingField::D],
};
const RA: OperandDefinition = OperandDefinition {
    kind: OperandKind::Register,
    fields: &[EncodingField::A],
};
const RB: OperandDefinition = OperandDefinition {
    kind: OperandKind::Register,
    fields: &[EncodingField::Breg],
};
const IMM: OperandDefinition = OperandDefinition {
    kind: OperandKind::Immediate,
    fields: &[EncodingField::I],
};
const MEMORY: OperandDefinition = OperandDefinition {
    kind: OperandKind::Memory,
    fields: &[EncodingField::A, EncodingField::I],
};
const SIZE: OperandDefinition = OperandDefinition {
    kind: OperandKind::DataSize,
    fields: &[EncodingField::X],
};
const CONDITION: OperandDefinition = OperandDefinition {
    kind: OperandKind::Condition,
    fields: &[EncodingField::X],
};
const CONTROL: OperandDefinition = OperandDefinition {
    kind: OperandKind::Control,
    fields: &[EncodingField::X],
};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InstructionFormat {
    Z,
    D,
    A,
    Da,
    Dab,
    Ab,
    Di,
    Dai,
    Mem,
    Br,
    Imm,
    Dx,
    Ax,
}

impl InstructionFormat {
    pub const ALL: &'static [Self] = &[
        Self::Z,
        Self::D,
        Self::A,
        Self::Da,
        Self::Dab,
        Self::Ab,
        Self::Di,
        Self::Dai,
        Self::Mem,
        Self::Br,
        Self::Imm,
        Self::Dx,
        Self::Ax,
    ];

    pub const fn operands(self) -> &'static [OperandDefinition] {
        match self {
            Self::Z => &[],
            Self::D => &[RD],
            Self::A => &[RA],
            Self::Da => &[RD, RA],
            Self::Dab => &[RD, RA, RB],
            Self::Ab => &[RA, RB],
            Self::Di => &[RD, IMM],
            Self::Dai => &[RD, RA, IMM],
            Self::Mem => &[RD, MEMORY, SIZE],
            Self::Br => &[CONDITION, IMM],
            Self::Imm => &[IMM],
            Self::Dx => &[RD, CONTROL],
            Self::Ax => &[CONTROL, RA],
        }
    }

    pub fn used_mask(self) -> u64 {
        self.operands()
            .iter()
            .flat_map(|operand| operand.fields)
            .fold(0xff, |mask, field| mask | field.mask())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NzcvEffect {
    Preserve,
    Add,
    Subtract,
    ResultNzClearCv,
    Restore,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImmediateMeaning {
    None,
    SignedWord,
    ByteDisplacement,
    RelativeDisplacement,
    TrapPayload,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstructionDefinition {
    pub opcode: Opcode,
    pub mnemonic: &'static str,
    pub format: InstructionFormat,
    pub supervisor_only: bool,
    pub nzcv: NzcvEffect,
    pub immediate: ImmediateMeaning,
}

impl InstructionDefinition {
    pub const fn required_features(&self) -> FeatureSet {
        FeatureSet::base_v1()
    }

    pub const fn operands(&self) -> &'static [OperandDefinition] {
        self.format.operands()
    }
}

macro_rules! opcodes {
    ($( $name:ident = $byte:literal, $mnemonic:literal, $format:ident, $supervisor:literal, $nzcv:ident, $immediate:ident; )*) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        #[repr(u8)]
        pub enum Opcode { $( $name = $byte, )* }

        impl Opcode {
            pub const ALL: &'static [Self] = &[$(Self::$name,)*];

            pub const fn as_u8(self) -> u8 { self as u8 }

            pub const fn definition(self) -> &'static InstructionDefinition {
                match self {
                    $(Self::$name => &InstructionDefinition {
                        opcode: Self::$name,
                        mnemonic: $mnemonic,
                        format: InstructionFormat::$format,
                        supervisor_only: $supervisor,
                        nzcv: NzcvEffect::$nzcv,
                        immediate: ImmediateMeaning::$immediate,
                    },)*
                }
            }
        }

        impl TryFrom<u8> for Opcode {
            type Error = UnknownOpcode;

            fn try_from(input: u8) -> Result<Self, Self::Error> {
                match input {
                    $($byte => Ok(Self::$name),)*
                    _ => Err(UnknownOpcode { input }),
                }
            }
        }
    };
}

opcodes! {
    Nop = 0x00, "NOP", Z, false, Preserve, None;
    Mov = 0x01, "MOV", Da, false, Preserve, None;
    Li = 0x02, "LI", Di, false, Preserve, SignedWord;
    Getpc = 0x03, "GETPC", D, false, Preserve, None;
    Getsp = 0x04, "GETSP", D, false, Preserve, None;
    Setsp = 0x05, "SETSP", A, false, Preserve, None;
    Getstatus = 0x06, "GETSTATUS", D, false, Preserve, None;
    Add = 0x10, "ADD", Dab, false, Add, None;
    Addi = 0x11, "ADDI", Dai, false, Add, SignedWord;
    Sub = 0x12, "SUB", Dab, false, Subtract, None;
    Subi = 0x13, "SUBI", Dai, false, Subtract, SignedWord;
    Mul = 0x14, "MUL", Dab, false, ResultNzClearCv, None;
    Divu = 0x15, "DIVU", Dab, false, ResultNzClearCv, None;
    Divs = 0x16, "DIVS", Dab, false, ResultNzClearCv, None;
    Remu = 0x17, "REMU", Dab, false, ResultNzClearCv, None;
    Rems = 0x18, "REMS", Dab, false, ResultNzClearCv, None;
    Cmp = 0x19, "CMP", Ab, false, Subtract, None;
    And = 0x20, "AND", Dab, false, ResultNzClearCv, None;
    Or = 0x21, "OR", Dab, false, ResultNzClearCv, None;
    Xor = 0x22, "XOR", Dab, false, ResultNzClearCv, None;
    Not = 0x23, "NOT", Da, false, ResultNzClearCv, None;
    Shl = 0x24, "SHL", Dab, false, ResultNzClearCv, None;
    Shr = 0x25, "SHR", Dab, false, ResultNzClearCv, None;
    Sar = 0x26, "SAR", Dab, false, ResultNzClearCv, None;
    Ldz = 0x30, "LDZ", Mem, false, Preserve, ByteDisplacement;
    Lds = 0x31, "LDS", Mem, false, Preserve, ByteDisplacement;
    St = 0x32, "ST", Mem, false, Preserve, ByteDisplacement;
    Br = 0x40, "BR", Br, false, Preserve, RelativeDisplacement;
    Jmp = 0x41, "JMP", A, false, Preserve, None;
    Call = 0x42, "CALL", Imm, false, Preserve, RelativeDisplacement;
    Callr = 0x43, "CALLR", A, false, Preserve, None;
    Ret = 0x44, "RET", Z, false, Preserve, None;
    Syscall = 0x50, "SYSCALL", Z, false, Preserve, None;
    Trap = 0x51, "TRAP", Imm, false, Preserve, TrapPayload;
    Halt = 0x52, "HALT", Z, true, Preserve, None;
    Rfe = 0x53, "RFE", Z, true, Restore, None;
    Ei = 0x54, "EI", Z, true, Preserve, None;
    Di = 0x55, "DI", Z, true, Preserve, None;
    Csrr = 0x56, "CSRR", Dx, true, Preserve, None;
    Csrw = 0x57, "CSRW", Ax, true, Preserve, None;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownOpcode {
    pub input: u8,
}

impl fmt::Display for UnknownOpcode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unallocated opcode {:#04x}", self.input)
    }
}

impl Error for UnknownOpcode {}
