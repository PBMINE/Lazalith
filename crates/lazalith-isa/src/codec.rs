use crate::{
    Condition, ControlRegister, DataSize, EncodingField, InstructionDefinition, Opcode, Operand,
    OperandError, OperandKind, UnknownOpcode,
};
use core::{error::Error, fmt};
use lazalith_types::{ArchitectureConfig, InvalidRegisterIndex, RegisterIndex, WordWidth};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    opcode: Opcode,
    operands: [Operand; 3],
}

impl Instruction {
    pub fn new(
        config: ArchitectureConfig,
        opcode: Opcode,
        operands: &[Operand],
    ) -> Result<Self, InstructionError> {
        validate(config, opcode, operands)?;
        let mut stored = [Operand::Immediate(0); 3];
        stored[..operands.len()].copy_from_slice(operands);
        Ok(Self {
            opcode,
            operands: stored,
        })
    }

    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }

    pub const fn definition(&self) -> &'static InstructionDefinition {
        self.opcode.definition()
    }

    pub fn operands(&self) -> &[Operand] {
        &self.operands[..self.definition().operands().len()]
    }

    pub fn validate(&self, config: ArchitectureConfig) -> Result<(), InstructionError> {
        validate(config, self.opcode, self.operands())
    }
}

fn validate(
    config: ArchitectureConfig,
    opcode: Opcode,
    operands: &[Operand],
) -> Result<(), InstructionError> {
    let fail = |source| InstructionError { opcode, source };
    let definitions = opcode.definition().operands();
    if operands.len() != definitions.len() {
        return Err(fail(ValidationError::OperandCount {
            expected: definitions.len(),
            actual: operands.len(),
        }));
    }
    for (index, (operand, definition)) in operands.iter().zip(definitions).enumerate() {
        if operand.kind() != definition.kind {
            return Err(fail(ValidationError::OperandKind {
                index,
                expected: definition.kind,
                actual: *operand,
            }));
        }
        if let Operand::DataSize(size) = operand
            && !config.supports_data_size(size.bytes())
        {
            return Err(fail(ValidationError::InvalidWidth {
                index,
                size: *size,
                width: config.word_width(),
            }));
        }
    }
    Ok(())
}

pub fn encode(
    config: ArchitectureConfig,
    instruction: &Instruction,
) -> Result<[u8; 8], InstructionError> {
    instruction.validate(config)?;
    let mut bits = u64::from(instruction.opcode.as_u8());
    for (operand, definition) in instruction
        .operands()
        .iter()
        .zip(instruction.definition().operands())
    {
        for field in definition.fields {
            let value = match operand {
                Operand::Register(register) => u32::from(register.as_u8()),
                Operand::Immediate(value) => *value as u32,
                Operand::Memory { base, displacement } => match field {
                    EncodingField::I => *displacement as u32,
                    _ => u32::from(base.as_u8()),
                },
                Operand::DataSize(size) => u32::from(size.as_u8()),
                Operand::Condition(condition) => u32::from(condition.as_u8()),
                Operand::Control(control) => u32::from(control.as_u8()),
            };
            bits |= u64::from(value) << field.shift();
        }
    }
    Ok(bits.to_le_bytes())
}

pub fn decode(config: ArchitectureConfig, input: &[u8]) -> Result<Instruction, DecodeError> {
    let bytes: [u8; 8] = input.try_into().map_err(|_| DecodeError::Length {
        actual: input.len(),
    })?;
    let bits = u64::from_le_bytes(bytes);
    let opcode = Opcode::try_from(bytes[0])
        .map_err(|source| DecodeError::UnknownOpcode { bytes, source })?;
    let definition = opcode.definition();
    let nonzero = bits & !definition.format.used_mask();
    if nonzero != 0 {
        return Err(DecodeError::ReservedBits {
            bytes,
            opcode,
            nonzero,
        });
    }
    let mut operands = [Operand::Immediate(0); 3];
    for (index, definition) in definition.operands().iter().enumerate() {
        let value = definition.fields[0].extract(bits);
        let selector_error = |source| DecodeError::Operand {
            bytes,
            index,
            source,
        };
        let register = |value| {
            RegisterIndex::try_from(value).map_err(|source| DecodeError::Register {
                bytes,
                index,
                source,
            })
        };
        operands[index] = match definition.kind {
            OperandKind::Register => Operand::Register(register(value as u8)?),
            OperandKind::Immediate => Operand::Immediate(value as i32),
            OperandKind::Memory => Operand::Memory {
                base: register(value as u8)?,
                displacement: definition.fields[1].extract(bits) as i32,
            },
            OperandKind::DataSize => {
                Operand::DataSize(DataSize::try_from(value as u8).map_err(selector_error)?)
            }
            OperandKind::Condition => {
                Operand::Condition(Condition::try_from(value as u8).map_err(selector_error)?)
            }
            OperandKind::Control => {
                Operand::Control(ControlRegister::try_from(value as u8).map_err(selector_error)?)
            }
        };
    }
    Instruction::new(config, opcode, &operands[..definition.operands().len()])
        .map_err(|source| DecodeError::Validation { bytes, source })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    OperandCount {
        expected: usize,
        actual: usize,
    },
    OperandKind {
        index: usize,
        expected: OperandKind,
        actual: Operand,
    },
    InvalidWidth {
        index: usize,
        size: DataSize,
        width: WordWidth,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OperandCount { expected, actual } => {
                write!(f, "expected {expected} operands, got {actual}")
            }
            Self::OperandKind {
                index,
                expected,
                actual,
            } => write!(f, "operand {index}: expected {expected:?}, got {actual:?}"),
            Self::InvalidWidth { index, size, width } => write!(
                f,
                "operand {index}: {}-byte data size is unsupported at {} bits",
                size.bytes(),
                width.bits()
            ),
        }
    }
}

impl Error for ValidationError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstructionError {
    pub opcode: Opcode,
    pub source: ValidationError,
}

impl fmt::Display for InstructionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.opcode.definition().mnemonic, self.source)
    }
}

impl Error for InstructionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Length {
        actual: usize,
    },
    UnknownOpcode {
        bytes: [u8; 8],
        source: UnknownOpcode,
    },
    ReservedBits {
        bytes: [u8; 8],
        opcode: Opcode,
        nonzero: u64,
    },
    Operand {
        bytes: [u8; 8],
        index: usize,
        source: OperandError,
    },
    Register {
        bytes: [u8; 8],
        index: usize,
        source: InvalidRegisterIndex,
    },
    Validation {
        bytes: [u8; 8],
        source: InstructionError,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Length { actual } => {
                write!(f, "expected exactly 8 instruction bytes, got {actual}")
            }
            Self::UnknownOpcode { source, .. } => write!(f, "instruction decode: {source}"),
            Self::ReservedBits {
                opcode, nonzero, ..
            } => write!(
                f,
                "{}: nonzero reserved bits {nonzero:#018x}",
                opcode.definition().mnemonic
            ),
            Self::Operand { index, source, .. } => {
                write!(f, "instruction decode operand {index}: {source}")
            }
            Self::Register { index, source, .. } => {
                write!(f, "instruction decode operand {index}: {source}")
            }
            Self::Validation { source, .. } => write!(f, "instruction decode: {source}"),
        }
    }
}

impl Error for DecodeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::UnknownOpcode { source, .. } => Some(source),
            Self::Operand { source, .. } => Some(source),
            Self::Register { source, .. } => Some(source),
            Self::Validation { source, .. } => Some(source),
            Self::Length { .. } | Self::ReservedBits { .. } => None,
        }
    }
}
