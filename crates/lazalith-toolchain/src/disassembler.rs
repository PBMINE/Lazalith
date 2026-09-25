use crate::{ObjectFile, SectionKind};
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use core::{error::Error, fmt};
use lazalith_isa::{
    Condition, ControlRegister, DataSize, DecodeError, Instruction, InstructionError, Operand,
    decode, encode,
};
use lazalith_types::ArchitectureConfig;

#[derive(Debug)]
pub enum DisassemblyError {
    IncompleteInstruction {
        offset: usize,
        remaining: usize,
    },
    Decode {
        offset: usize,
        source: DecodeError,
    },
    Encode {
        offset: usize,
        source: InstructionError,
    },
    NonCanonical {
        offset: usize,
    },
}

impl fmt::Display for DisassemblyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IncompleteInstruction { offset, remaining } => {
                write!(
                    f,
                    "incomplete instruction at {offset}: {remaining} bytes remain"
                )
            }
            Self::Decode { offset, source } => {
                write!(f, "instruction at {offset} cannot be decoded: {source}")
            }
            Self::Encode { offset, source } => {
                write!(f, "instruction at {offset} cannot be encoded: {source}")
            }
            Self::NonCanonical { offset } => {
                write!(f, "instruction at {offset} is not canonically encoded")
            }
        }
    }
}

impl Error for DisassemblyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode { source, .. } => Some(source),
            Self::Encode { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisassembledInstruction {
    offset: u64,
    instruction: Instruction,
    text: String,
}

impl DisassembledInstruction {
    pub const fn offset(&self) -> u64 {
        self.offset
    }

    pub const fn instruction(&self) -> Instruction {
        self.instruction
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectDisassembly {
    section: u16,
    instructions: Vec<DisassembledInstruction>,
}

impl ObjectDisassembly {
    pub const fn section(&self) -> u16 {
        self.section
    }

    pub fn instructions(&self) -> &[DisassembledInstruction] {
        &self.instructions
    }
}

pub fn disassemble(
    config: ArchitectureConfig,
    bytes: &[u8],
) -> Result<Vec<DisassembledInstruction>, DisassemblyError> {
    if !bytes.len().is_multiple_of(8) {
        return Err(DisassemblyError::IncompleteInstruction {
            offset: bytes.len() / 8 * 8,
            remaining: bytes.len() % 8,
        });
    }
    let mut instructions = Vec::new();
    for (index, chunk) in bytes.chunks(8).enumerate() {
        let offset = index * 8;
        let instruction =
            decode(config, chunk).map_err(|source| DisassemblyError::Decode { offset, source })?;
        let canonical = encode(config, &instruction)
            .map_err(|source| DisassemblyError::Encode { offset, source })?;
        if canonical != chunk {
            return Err(DisassemblyError::NonCanonical { offset });
        }
        let text = format_instruction(&instruction);
        instructions.push(DisassembledInstruction {
            offset: offset as u64,
            instruction,
            text,
        });
    }
    Ok(instructions)
}

pub fn disassemble_object(object: &ObjectFile) -> Result<Vec<ObjectDisassembly>, DisassemblyError> {
    let mut result = Vec::new();
    for (index, section) in object.sections().iter().enumerate() {
        if section.kind() == SectionKind::Text {
            result.push(ObjectDisassembly {
                section: u16::try_from(index).unwrap_or(u16::MAX),
                instructions: disassemble(object.config(), section.bytes())?,
            });
        }
    }
    Ok(result)
}

fn format_instruction(instruction: &Instruction) -> String {
    let mut text = String::from(instruction.definition().mnemonic);
    for (index, operand) in instruction.operands().iter().enumerate() {
        if index == 0 {
            text.push(' ');
        } else {
            text.push_str(", ");
        }
        match operand {
            Operand::Register(register) => {
                text.push('r');
                text.push_str(&register.as_u8().to_string());
            }
            Operand::Immediate(value) => text.push_str(&value.to_string()),
            Operand::Memory { base, displacement } => {
                text.push_str(&format!("[r{}", base.as_u8()));
                if *displacement < 0 {
                    text.push_str(&format!("-{}", displacement.unsigned_abs()));
                } else if *displacement != 0 {
                    text.push_str(&format!("+{displacement}"));
                }
                text.push(']');
            }
            Operand::DataSize(size) => text.push_str(data_size_name(*size)),
            Operand::Condition(condition) => text.push_str(condition_name(*condition)),
            Operand::Control(control) => text.push_str(control_name(*control)),
        }
    }
    text
}

const fn data_size_name(size: DataSize) -> &'static str {
    match size {
        DataSize::Byte => "BYTE",
        DataSize::Half => "HALF",
        DataSize::Word => "WORD",
        DataSize::Double => "DOUBLE",
    }
}

const fn condition_name(condition: Condition) -> &'static str {
    match condition {
        Condition::Al => "AL",
        Condition::Eq => "EQ",
        Condition::Ne => "NE",
        Condition::Ult => "ULT",
        Condition::Uge => "UGE",
        Condition::Ule => "ULE",
        Condition::Ugt => "UGT",
        Condition::Slt => "SLT",
        Condition::Sge => "SGE",
        Condition::Sle => "SLE",
        Condition::Sgt => "SGT",
        Condition::Vs => "VS",
        Condition::Vc => "VC",
        Condition::Mi => "MI",
        Condition::Pl => "PL",
    }
}

const fn control_name(control: ControlRegister) -> &'static str {
    match control {
        ControlRegister::Tvec => "TVEC",
        ControlRegister::Epc => "EPC",
        ControlRegister::Esp => "ESP",
        ControlRegister::Estatus => "ESTATUS",
        ControlRegister::Tcause => "TCAUSE",
        ControlRegister::Tpayload => "TPAYLOAD",
    }
}
