use crate::{LzxArchitecture, LzxError, LzxImage, LzxSection, USER_STACK_LENGTH};
use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_isa::{Instruction, InstructionError, Opcode, Operand, encode};
use lazalith_os_abi::Syscall;
use lazalith_types::{InvalidRegisterIndex, RegisterIndex};

pub const INIT_EXIT_CODE: u32 = 0;
pub const INIT_SYSCALL_NUMBER: u32 = Syscall::Exit.as_u16() as u32;
pub const INIT_CODE_LENGTH: usize = 16;
const _: () = assert!(INIT_EXIT_CODE == 0);

#[derive(Debug)]
pub enum InitImageError {
    Register(InvalidRegisterIndex),
    ImmediateOutOfRange,
    Instruction(InstructionError),
    Image(LzxError),
    Allocation(TryReserveError),
}

impl fmt::Display for InitImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(source) => write!(f, "init register selection failed: {source}"),
            Self::ImmediateOutOfRange => f.write_str("init immediate is outside the LI range"),
            Self::Instruction(source) => {
                write!(f, "init instruction construction failed: {source}")
            }
            Self::Image(source) => write!(f, "init image construction failed: {source}"),
            Self::Allocation(source) => write!(f, "init image allocation failed: {source}"),
        }
    }
}

impl Error for InitImageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Register(source) => Some(source),
            Self::ImmediateOutOfRange => None,
            Self::Instruction(source) => Some(source),
            Self::Image(source) => Some(source),
            Self::Allocation(source) => Some(source),
        }
    }
}

pub fn build_init_image(architecture: LzxArchitecture) -> Result<LzxImage, InitImageError> {
    let config = architecture.config();
    let r0 = RegisterIndex::try_from(0).map_err(InitImageError::Register)?;
    let syscall_number =
        i32::try_from(INIT_SYSCALL_NUMBER).map_err(|_| InitImageError::ImmediateOutOfRange)?;
    let load_exit = Instruction::new(
        config,
        Opcode::Li,
        &[Operand::Register(r0), Operand::Immediate(syscall_number)],
    )
    .map_err(InitImageError::Instruction)?;
    let syscall =
        Instruction::new(config, Opcode::Syscall, &[]).map_err(InitImageError::Instruction)?;
    let mut code = [0u8; INIT_CODE_LENGTH];
    code[..8].copy_from_slice(&encode(config, &load_exit).map_err(InitImageError::Instruction)?);
    code[8..].copy_from_slice(&encode(config, &syscall).map_err(InitImageError::Instruction)?);
    let section = LzxSection::code(&code).map_err(InitImageError::Image)?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(1)
        .map_err(InitImageError::Allocation)?;
    sections.push(section);
    LzxImage::new(architecture, 0, 0, 0, USER_STACK_LENGTH, sections).map_err(InitImageError::Image)
}
