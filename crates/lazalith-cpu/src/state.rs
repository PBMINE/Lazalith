use crate::{InvalidStatus, Privilege, RegisterFile, StatusRegister};
use core::{error::Error, fmt};
use lazalith_types::{
    ArchitectureConfig, InstructionAddress, InvalidRegisterIndex, RegisterIndex, VirtualAddress,
    WidthError, WordWidth,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchitecturalState {
    config: ArchitectureConfig,
    registers: RegisterFile,
    pc: InstructionAddress,
    sp: VirtualAddress,
    status: StatusRegister,
}

impl ArchitecturalState {
    pub fn new(
        config: ArchitectureConfig,
        pc: InstructionAddress,
        sp: VirtualAddress,
        status: u64,
    ) -> Result<Self, ControlStateError> {
        validate_pc(config, pc)?;
        validate_sp(config, sp)?;
        let status = StatusRegister::try_from_bits(config.word_width(), status)
            .map_err(ControlStateError::Status)?;
        Ok(Self {
            config,
            registers: RegisterFile::new(config),
            pc,
            sp,
            status,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn registers(&self) -> &RegisterFile {
        &self.registers
    }

    pub fn write_register(&mut self, index: RegisterIndex, value: u64) {
        self.registers.write(index, value);
    }

    pub fn write_register_raw(
        &mut self,
        index: u8,
        value: u64,
    ) -> Result<(), InvalidRegisterIndex> {
        self.registers.write_raw(index, value)
    }

    pub const fn pc(&self) -> InstructionAddress {
        self.pc
    }

    pub const fn sp(&self) -> VirtualAddress {
        self.sp
    }

    pub const fn status(&self) -> StatusRegister {
        self.status
    }

    pub const fn privilege(&self) -> Privilege {
        self.status.privilege()
    }

    pub fn update_arithmetic(&mut self, result: lazalith_types::ArithmeticResult) {
        self.status.update_arithmetic(result);
    }

    pub fn set_interrupts_enabled(&mut self, enabled: bool) {
        self.status.set_interrupts_enabled(enabled);
    }

    pub fn set_pc(&mut self, pc: InstructionAddress) -> Result<(), ControlStateError> {
        validate_pc(self.config, pc)?;
        self.pc = pc;
        Ok(())
    }

    pub fn set_sp(&mut self, sp: VirtualAddress) -> Result<(), ControlStateError> {
        validate_sp(self.config, sp)?;
        self.sp = sp;
        Ok(())
    }

    pub(crate) fn commit_outcome_control(&mut self, pc: InstructionAddress, sp: VirtualAddress) {
        self.pc = pc;
        self.sp = sp;
    }

    pub fn restore_control(
        &mut self,
        pc: InstructionAddress,
        sp: VirtualAddress,
        status: u64,
    ) -> Result<(), ControlStateError> {
        validate_pc(self.config, pc)?;
        validate_sp(self.config, sp)?;
        let status = StatusRegister::try_from_bits(self.config.word_width(), status)
            .map_err(ControlStateError::Status)?;
        self.pc = pc;
        self.sp = sp;
        self.status = status;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    Running,
    Halted,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DebugState {
    pub single_step: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpecialRegister {
    Pc,
    Sp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlStateError {
    Range {
        register: SpecialRegister,
        input: u64,
        source: WidthError,
    },
    Alignment {
        register: SpecialRegister,
        input: u64,
        alignment: u8,
        width: WordWidth,
    },
    Status(InvalidStatus),
}

impl fmt::Display for ControlStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Range {
                register,
                input,
                source,
            } => write!(f, "invalid {register:?} {input:#x}: {source}"),
            Self::Alignment {
                register,
                input,
                alignment,
                width,
            } => write!(
                f,
                "invalid {register:?} {input:#x} at {} bits: requires {alignment}-byte alignment",
                width.bits()
            ),
            Self::Status(source) => source.fmt(f),
        }
    }
}

impl Error for ControlStateError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Range { source, .. } => Some(source),
            Self::Status(source) => Some(source),
            Self::Alignment { .. } => None,
        }
    }
}

pub fn validate_pc(
    config: ArchitectureConfig,
    pc: InstructionAddress,
) -> Result<(), ControlStateError> {
    validate_address(
        config,
        SpecialRegister::Pc,
        pc.as_u64(),
        config.instruction_alignment(),
    )
}

pub fn validate_sp(
    config: ArchitectureConfig,
    sp: VirtualAddress,
) -> Result<(), ControlStateError> {
    validate_address(
        config,
        SpecialRegister::Sp,
        sp.as_u64(),
        config.stack_alignment(),
    )
}

fn validate_address(
    config: ArchitectureConfig,
    register: SpecialRegister,
    input: u64,
    alignment: u8,
) -> Result<(), ControlStateError> {
    config
        .word_width()
        .validate_address(input)
        .map_err(|source| ControlStateError::Range {
            register,
            input,
            source,
        })?;
    if !input.is_multiple_of(u64::from(alignment)) {
        return Err(ControlStateError::Alignment {
            register,
            input,
            alignment,
            width: config.word_width(),
        });
    }
    Ok(())
}
