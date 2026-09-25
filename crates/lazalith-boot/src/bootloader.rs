use crate::{BootError, KERNEL_PAYLOAD_ADDRESS};
use alloc::vec::Vec;
use lazalith_isa::{Condition, DataSize, Instruction, Opcode, Operand, encode};
use lazalith_types::{ArchitectureConfig, RegisterIndex};

#[derive(Clone, Copy, Eq, PartialEq)]
struct Label(u8);

enum Item {
    Instruction(Instruction),
    Branch { condition: Condition, label: Label },
}

struct Program {
    config: ArchitectureConfig,
    items: Vec<Item>,
    labels: Vec<(Label, usize)>,
}

pub(crate) struct Bootloader {
    pub(crate) bytes: Vec<u8>,
    pub(crate) instructions: u16,
}

impl Program {
    fn new(config: ArchitectureConfig) -> Result<Self, BootError> {
        let mut items = Vec::new();
        items.try_reserve_exact(64).map_err(BootError::Allocation)?;
        let mut labels = Vec::new();
        labels.try_reserve_exact(2).map_err(BootError::Allocation)?;
        Ok(Self {
            config,
            items,
            labels,
        })
    }

    fn emit(&mut self, opcode: Opcode, operands: &[Operand]) -> Result<(), BootError> {
        self.items.try_reserve(1).map_err(BootError::Allocation)?;
        let instruction =
            Instruction::new(self.config, opcode, operands).map_err(BootError::Instruction)?;
        self.items.push(Item::Instruction(instruction));
        Ok(())
    }

    fn branch(&mut self, condition: Condition, label: Label) -> Result<(), BootError> {
        self.items.try_reserve(1).map_err(BootError::Allocation)?;
        self.items.push(Item::Branch { condition, label });
        Ok(())
    }

    fn mark(&mut self, label: Label) -> Result<(), BootError> {
        self.labels.try_reserve(1).map_err(BootError::Allocation)?;
        self.labels.push((label, self.items.len()));
        Ok(())
    }

    fn finish(self) -> Result<Bootloader, BootError> {
        let mut bytes = Vec::new();
        let byte_length = self
            .items
            .len()
            .checked_mul(8)
            .ok_or(BootError::InvalidBranchOffset)?;
        bytes
            .try_reserve_exact(byte_length)
            .map_err(BootError::Allocation)?;
        for (index, item) in self.items.iter().enumerate() {
            let instruction = match item {
                Item::Instruction(instruction) => *instruction,
                Item::Branch { condition, label } => {
                    let target = self
                        .labels
                        .iter()
                        .find(|(candidate, _)| candidate == label)
                        .map(|(_, position)| *position)
                        .ok_or(BootError::UnresolvedLabel { label: label.0 })?;
                    let next = index.checked_add(1).ok_or(BootError::InvalidBranchOffset)?;
                    let displacement = i64::try_from(target)
                        .map_err(BootError::HostSize)?
                        .checked_sub(i64::try_from(next).map_err(BootError::HostSize)?)
                        .and_then(|value| value.checked_mul(2))
                        .ok_or(BootError::InvalidBranchOffset)?;
                    let displacement = i32::try_from(displacement).map_err(BootError::HostSize)?;
                    Instruction::new(
                        self.config,
                        Opcode::Br,
                        &[
                            Operand::Condition(*condition),
                            Operand::Immediate(displacement),
                        ],
                    )
                    .map_err(BootError::Instruction)?
                }
            };
            let encoded = encode(self.config, &instruction).map_err(BootError::Instruction)?;
            bytes.extend_from_slice(&encoded);
        }
        let instructions = u16::try_from(self.items.len()).map_err(BootError::HostSize)?;
        Ok(Bootloader {
            bytes,
            instructions,
        })
    }
}

fn register_index(input: u8) -> Result<RegisterIndex, BootError> {
    RegisterIndex::try_from(input).map_err(BootError::Register)
}

fn register(input: u8) -> Result<Operand, BootError> {
    Ok(Operand::Register(register_index(input)?))
}

fn memory(base: u8, displacement: i32) -> Result<Operand, BootError> {
    Ok(Operand::Memory {
        base: register_index(base)?,
        displacement,
    })
}

pub(crate) fn build(config: ArchitectureConfig) -> Result<Bootloader, BootError> {
    let mut program = Program::new(config)?;
    let copy_loop = Label(0);
    let failure = Label(1);

    program.emit(Opcode::Li, &[register(5)?, Operand::Immediate(0x400)])?;
    program.emit(
        Opcode::Ldz,
        &[
            register(9)?,
            memory(5, 20)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(
        Opcode::Ldz,
        &[
            register(10)?,
            memory(5, 28)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(
        Opcode::Ldz,
        &[
            register(11)?,
            memory(5, 36)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(Opcode::Or, &[register(12)?, register(9)?, register(10)?])?;
    program.emit(Opcode::Or, &[register(12)?, register(12)?, register(11)?])?;
    program.branch(Condition::Ne, failure)?;

    program.emit(
        Opcode::Ldz,
        &[
            register(13)?,
            memory(5, 16)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(Opcode::Li, &[register(12)?, Operand::Immediate(0x1000)])?;
    program.emit(Opcode::Li, &[register(14)?, Operand::Immediate(8)])?;
    program.emit(Opcode::Shl, &[register(12)?, register(12)?, register(14)?])?;
    program.emit(Opcode::Cmp, &[register(13)?, register(12)?])?;
    program.branch(Condition::Ne, failure)?;

    program.emit(
        Opcode::Ldz,
        &[
            register(2)?,
            memory(5, 24)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(Opcode::Cmp, &[register(2)?, register(0)?])?;
    program.branch(Condition::Eq, failure)?;
    program.emit(Opcode::Li, &[register(12)?, Operand::Immediate(0x7f000)])?;
    program.emit(Opcode::Cmp, &[register(12)?, register(2)?])?;
    program.branch(Condition::Ult, failure)?;

    program.emit(
        Opcode::Ldz,
        &[
            register(8)?,
            memory(5, 32)?,
            Operand::DataSize(DataSize::Word),
        ],
    )?;
    program.emit(Opcode::Li, &[register(12)?, Operand::Immediate(3)])?;
    program.emit(Opcode::And, &[register(12)?, register(12)?, register(8)?])?;
    program.branch(Condition::Ne, failure)?;
    program.emit(Opcode::Sub, &[register(12)?, register(2)?, register(8)?])?;
    program.branch(Condition::Ult, failure)?;
    program.emit(Opcode::Li, &[register(6)?, Operand::Immediate(8)])?;
    program.emit(Opcode::Cmp, &[register(12)?, register(6)?])?;
    program.branch(Condition::Ult, failure)?;

    program.emit(Opcode::Mov, &[register(1)?, register(13)?])?;
    program.emit(Opcode::Mov, &[register(4)?, register(2)?])?;
    program.emit(
        Opcode::Li,
        &[
            register(5)?,
            Operand::Immediate(i32::try_from(KERNEL_PAYLOAD_ADDRESS).map_err(BootError::HostSize)?),
        ],
    )?;
    program.emit(Opcode::Mov, &[register(7)?, register(1)?])?;
    program.mark(copy_loop)?;
    program.emit(
        Opcode::Ldz,
        &[
            register(6)?,
            memory(5, 0)?,
            Operand::DataSize(DataSize::Byte),
        ],
    )?;
    program.emit(
        Opcode::St,
        &[
            register(6)?,
            memory(7, 0)?,
            Operand::DataSize(DataSize::Byte),
        ],
    )?;
    program.emit(
        Opcode::Addi,
        &[register(5)?, register(5)?, Operand::Immediate(1)],
    )?;
    program.emit(
        Opcode::Addi,
        &[register(7)?, register(7)?, Operand::Immediate(1)],
    )?;
    program.emit(
        Opcode::Addi,
        &[register(4)?, register(4)?, Operand::Immediate(-1)],
    )?;
    program.branch(Condition::Ne, copy_loop)?;

    program.emit(Opcode::Add, &[register(8)?, register(8)?, register(1)?])?;
    program.emit(Opcode::Mov, &[register(3)?, register(8)?])?;
    program.emit(Opcode::Mov, &[register(15)?, register(8)?])?;
    program.emit(Opcode::Li, &[register(0)?, Operand::Immediate(0)])?;
    for input in 4..=14 {
        program.emit(Opcode::Li, &[register(input)?, Operand::Immediate(0)])?;
    }
    program.emit(Opcode::Jmp, &[register(15)?])?;
    program.mark(failure)?;
    program.emit(Opcode::Halt, &[])?;

    program.finish()
}
