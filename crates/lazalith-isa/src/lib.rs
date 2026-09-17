#![no_std]

mod codec;
mod metadata;
mod operand;

pub use codec::{DecodeError, Instruction, InstructionError, ValidationError, decode, encode};
pub use metadata::{
    EncodingField, ImmediateMeaning, InstructionDefinition, InstructionFormat, NzcvEffect, Opcode,
    OperandDefinition, OperandKind, UnknownOpcode,
};
pub use operand::{Condition, ControlRegister, DataSize, Operand, OperandError};
