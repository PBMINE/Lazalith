//! Shared test support for the `lazalith-cpu` tests.
//!
//! `dead_code` is allowed because each test file compiles this module *separately*
//! and uses a different part of it, so a helper one binary never calls is a warning
//! in that binary and not a defect. Removing the helpers that a sibling test needs
//! would be the only way to silence it honestly, and the alternative is a support
//! module per test file.
#![allow(dead_code)]

use lazalith_cpu::{
    ArchitecturalState, CpuMemory, DataAccess, DataAccessKind, Privilege, Processor,
};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_types::{ArchitectureConfig, InstructionAddress, RegisterIndex, VirtualAddress};
use std::{error::Error, fmt};

pub const MODES: [ArchitectureConfig; 2] = [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()];

pub fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).unwrap())
}
pub fn instruction(
    config: ArchitectureConfig,
    opcode: Opcode,
    operands: &[Operand],
) -> Instruction {
    Instruction::new(config, opcode, operands).unwrap()
}
pub fn cpu(
    config: ArchitectureConfig,
    pc: u64,
    sp: u64,
    status: u64,
    registers: &[(u8, u64)],
) -> Processor {
    let mut state = ArchitecturalState::new(
        config,
        InstructionAddress::new(pc),
        VirtualAddress::new(sp),
        status,
    )
    .unwrap();
    for &(index, value) in registers {
        state.write_register_raw(index, value).unwrap();
    }
    Processor::new(state)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryError {
    Unmapped,
    Permission,
    Transaction,
    Policy,
}
impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl Error for MemoryError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ram {
    /// Public so a differential test can compare two memories after every step.
    ///
    /// **Not a shortcut.** The point of that comparison is that the two engines wrote
    /// the same bytes; a private field would mean asking `Ram` to report its own
    /// equality, which is a different and weaker claim.
    pub bytes: Vec<u8>,
    /// Decoded instructions, by address — the cache `lazalith_memory::Bus` keeps.
    ///
    /// **Here so a benchmark is representative rather than pessimistic.** The real bus
    /// caches decoded instructions and a memory without a cache makes every engine
    /// decode every instruction every step, which measures the decoder rather than the
    /// thing being compared. A benchmark run against a memory the system never uses
    /// produces a number that is true and useless.
    pub decoded: std::collections::HashMap<usize, lazalith_isa::Instruction>,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
    pub user: bool,
    pub device: bool,
    pub fail: bool,
    pub reads: usize,
    pub writes: usize,
}
impl Default for Ram {
    fn default() -> Self {
        Self {
            bytes: vec![0; 512],
            decoded: std::collections::HashMap::new(),
            readable: true,
            writable: true,
            executable: true,
            user: true,
            device: false,
            fail: false,
            reads: 0,
            writes: 0,
        }
    }
}
impl Ram {
    pub fn put(&mut self, address: usize, bytes: &[u8]) {
        self.bytes[address..address + bytes.len()].copy_from_slice(bytes);
    }
    pub fn word(&self, address: usize, size: usize) -> u64 {
        let mut bytes = [0; 8];
        bytes[..size].copy_from_slice(&self.bytes[address..address + size]);
        u64::from_le_bytes(bytes)
    }
    pub fn code(
        &mut self,
        address: usize,
        config: ArchitectureConfig,
        opcode: Opcode,
        operands: &[Operand],
    ) {
        self.put(
            address,
            &encode(config, &instruction(config, opcode, operands)).unwrap(),
        );
    }
    fn range(&self, address: u64, size: u8) -> Result<std::ops::Range<usize>, MemoryError> {
        let start = usize::try_from(address).map_err(|_| MemoryError::Unmapped)?;
        let end = start
            .checked_add(usize::from(size))
            .ok_or(MemoryError::Unmapped)?;
        if end > self.bytes.len() {
            return Err(MemoryError::Unmapped);
        }
        Ok(start..end)
    }
    fn validate(
        &self,
        access: DataAccess,
        write: bool,
    ) -> Result<std::ops::Range<usize>, MemoryError> {
        let range = self.range(access.address().as_u64(), access.size().bytes())?;
        if (write && !self.writable)
            || (!write && !self.readable)
            || (access.privilege() == Privilege::User && !self.user)
        {
            return Err(MemoryError::Permission);
        }
        if self.device
            && matches!(
                access.kind(),
                DataAccessKind::StackRead | DataAccessKind::StackWrite
            )
        {
            return Err(MemoryError::Policy);
        }
        if self.fail {
            return Err(MemoryError::Transaction);
        }
        Ok(range)
    }
}
impl CpuMemory for Ram {
    type Error = MemoryError;
    fn fetch_instruction(
        &self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: Privilege,
    ) -> Result<[u8; 8], Self::Error> {
        assert!(pc.as_u64().is_multiple_of(4));
        assert!(
            config
                .word_width()
                .checked_access_end(pc.as_u64(), 8)
                .is_ok()
        );
        let range = self.range(pc.as_u64(), 8)?;
        if !self.executable || (privilege == Privilege::User && !self.user) {
            return Err(MemoryError::Permission);
        }
        if self.device {
            return Err(MemoryError::Policy);
        }
        Ok(self.bytes[range].try_into().unwrap())
    }
    fn read_data(&mut self, access: DataAccess) -> Result<u64, Self::Error> {
        assert_eq!(access.kind(), DataAccessKind::Read);
        let range = self.validate(access, false)?;
        let value = self.word(range.start, range.len());
        self.reads += 1;
        Ok(value)
    }
    fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), Self::Error> {
        assert!(matches!(
            access.kind(),
            DataAccessKind::Write | DataAccessKind::StackWrite
        ));
        let range = self.validate(access, true)?;
        let len = range.len();
        self.bytes[range].copy_from_slice(&value.to_le_bytes()[..len]);
        self.writes += 1;
        Ok(())
    }
    fn peek_stack(&self, access: DataAccess) -> Result<u64, Self::Error> {
        assert_eq!(access.kind(), DataAccessKind::StackRead);
        let range = self.validate(access, false)?;
        Ok(self.word(range.start, range.len()))
    }
}

/// Asserts what an instruction *did*, without its cost.
///
/// **Named so a reader knows which of the two is being checked.** A `step` answers two
/// questions — what happened, and what it cost — and almost every instruction test is
/// only asking the first. Spelling the cost into each of those expectations would couple
/// them all to the timing model, so a change to the model would rewrite thirteen
/// assertions that were never about it. The cost has its own tests
/// (`instruction_costs_follow_the_isa_model`) which check it exactly.
#[macro_export]
macro_rules! assert_outcome {
    ($actual:expr, $expected:expr $(,)?) => {
        assert_eq!(
            $actual.map(lazalith_cpu::StepResult::outcome),
            ::core::result::Result::Ok($expected)
        )
    };
}
