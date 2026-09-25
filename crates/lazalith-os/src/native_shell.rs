use crate::{
    LZX_DATA_PERMISSIONS, LzxArchitecture, LzxError, LzxImage, LzxSection, LzxSectionKind,
    SHELL_PROMPT, USER_DATA_LENGTH, USER_DATA_START, USER_STACK_LENGTH,
};
use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_isa::{
    Condition, DataSize, DecodeError, Instruction, InstructionError, Opcode, Operand, decode,
    encode,
};
use lazalith_os_abi::{OPEN_READ, Syscall};
use lazalith_types::{ArchitectureConfig, InvalidRegisterIndex, RegisterIndex, WordWidth};

pub const NATIVE_SHELL_PROMPT: &[u8] = SHELL_PROMPT;
pub const NATIVE_SHELL_HELP: &[u8] = b"commands: help echo ls cat run (guest deferred) clear\n";
pub const NATIVE_SHELL_CAT_PATH: &[u8] = b"/hello.txt";
pub const NATIVE_SHELL_LS_PATH: &[u8] = b"/";
pub const NATIVE_SHELL_RUN_DEFERRED: &[u8] = b"run: deferred; process launch is unsupported\n";
pub const NATIVE_SHELL_IO_ERROR: &[u8] = b"syscall error\n";
pub const NATIVE_SHELL_UNKNOWN_COMMAND: &[u8] = b"unknown command\n";
pub const NATIVE_SHELL_CAT_UNAVAILABLE: &[u8] = b"cat: /hello.txt unavailable\n";
pub const NATIVE_SHELL_LS_HEADER: &[u8] = b"directory records:\n";
pub const NATIVE_SHELL_PROCESS_LAUNCH_SUPPORTED: bool = false;
pub const NATIVE_SHELL_LINE_CAPACITY: u64 = 0xff;
pub const NATIVE_SHELL_FILE_CAPACITY: u64 = 0x100;
pub const NATIVE_SHELL_DIRECTORY_CAPACITY: u64 = 0x400;
pub const NATIVE_SHELL_DATA_OFFSET: u64 = 0x100;
pub const NATIVE_SHELL_DATA_LENGTH: u64 = 0x400;
pub const NATIVE_SHELL_BSS_OFFSET: u64 = 0x1000;
pub const NATIVE_SHELL_BSS_LENGTH: u64 = 0x800;
pub const NATIVE_SHELL_REQUIRED_DATA: u64 = NATIVE_SHELL_BSS_OFFSET + NATIVE_SHELL_BSS_LENGTH;
pub const NATIVE_SHELL_LINE_BUFFER_OFFSET: u64 = NATIVE_SHELL_BSS_OFFSET;
pub const NATIVE_SHELL_IO_RESULT_OFFSET: u64 = NATIVE_SHELL_BSS_OFFSET + 0x100;
pub const NATIVE_SHELL_DIRECTORY_OFFSET: u64 = NATIVE_SHELL_BSS_OFFSET + 0x200;
pub const NATIVE_SHELL_FILE_BUFFER_OFFSET: u64 = NATIVE_SHELL_BSS_OFFSET + 0x600;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeShellLayout {
    pub data_offset: u64,
    pub data_length: u64,
    pub bss_offset: u64,
    pub bss_length: u64,
    pub required_data: u64,
    pub line_buffer_offset: u64,
    pub io_result_offset: u64,
    pub directory_offset: u64,
    pub file_buffer_offset: u64,
}

pub const NATIVE_SHELL_LAYOUT: NativeShellLayout = NativeShellLayout {
    data_offset: NATIVE_SHELL_DATA_OFFSET,
    data_length: NATIVE_SHELL_DATA_LENGTH,
    bss_offset: NATIVE_SHELL_BSS_OFFSET,
    bss_length: NATIVE_SHELL_BSS_LENGTH,
    required_data: NATIVE_SHELL_REQUIRED_DATA,
    line_buffer_offset: NATIVE_SHELL_LINE_BUFFER_OFFSET,
    io_result_offset: NATIVE_SHELL_IO_RESULT_OFFSET,
    directory_offset: NATIVE_SHELL_DIRECTORY_OFFSET,
    file_buffer_offset: NATIVE_SHELL_FILE_BUFFER_OFFSET,
};

#[derive(Debug)]
pub enum NativeShellLayoutError {
    InvalidDataRange {
        offset: u64,
        length: u64,
        capacity: u64,
    },
    OffsetConversion {
        offset: u64,
    },
    AddressOverflow,
    MisalignedOffset {
        offset: u64,
        alignment: u64,
    },
    SectionOverlap {
        first_start: u64,
        first_end: u64,
        second_start: u64,
        second_end: u64,
    },
    OutsideUserData {
        start: u64,
        end: u64,
        maximum: u64,
    },
    RequiredDataOverflow {
        end: u64,
    },
}

impl fmt::Display for NativeShellLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDataRange {
                offset,
                length,
                capacity,
            } => write!(
                f,
                "native shell data range {offset:#x}+{length:#x} exceeds {capacity:#x}"
            ),
            Self::OffsetConversion { offset } => {
                write!(
                    f,
                    "native shell offset {offset:#x} does not fit the host index type"
                )
            }
            Self::AddressOverflow => f.write_str("native shell layout arithmetic overflowed"),
            Self::MisalignedOffset { offset, alignment } => write!(
                f,
                "native shell offset {offset:#x} is not aligned to {alignment}"
            ),
            Self::SectionOverlap {
                first_start,
                first_end,
                second_start,
                second_end,
            } => write!(
                f,
                "native shell sections overlap: {first_start:#x}..{first_end:#x} and {second_start:#x}..{second_end:#x}"
            ),
            Self::OutsideUserData {
                start,
                end,
                maximum,
            } => write!(
                f,
                "native shell data range {start:#x}..{end:#x} exceeds User data length {maximum:#x}"
            ),
            Self::RequiredDataOverflow { end } => {
                write!(f, "native shell required data end {end:#x} overflowed")
            }
        }
    }
}

impl Error for NativeShellLayoutError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LabelId(u32);

#[derive(Debug)]
pub enum NativeShellEmissionError {
    Instruction(InstructionError),
    Decode(DecodeError),
    Allocation(TryReserveError),
    LabelLimit {
        maximum: u32,
    },
    LabelNotFound {
        label: u32,
    },
    LabelAlreadyBound {
        label: u32,
    },
    UnboundLabel {
        label: u32,
    },
    UnalignedLabel {
        label: u32,
        offset: u64,
    },
    BranchOutOfRange {
        label: u32,
        instruction_offset: u64,
        next_pc: u64,
        target: u64,
        displacement: i128,
    },
    BranchUnaligned {
        label: u32,
        instruction_offset: u64,
        next_pc: u64,
        target: u64,
    },
    CanonicalEncoding {
        opcode: Opcode,
        offset: u64,
    },
    CodeLength {
        length: u64,
        alignment: u64,
    },
    CodeOffsetOverflow {
        offset: u64,
    },
    AddressOverflow,
}

impl fmt::Display for NativeShellEmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Instruction(source) => {
                write!(f, "native shell instruction construction failed: {source}")
            }
            Self::Decode(source) => write!(f, "native shell instruction decode failed: {source}"),
            Self::Allocation(source) => {
                write!(f, "native shell code allocation failed: {source}")
            }
            Self::LabelLimit { maximum } => {
                write!(f, "native shell label count exceeds {maximum}")
            }
            Self::LabelNotFound { label } => write!(f, "native shell label {label} is missing"),
            Self::LabelAlreadyBound { label } => {
                write!(f, "native shell label {label} is already bound")
            }
            Self::UnboundLabel { label } => write!(f, "native shell label {label} is unbound"),
            Self::UnalignedLabel { label, offset } => {
                write!(f, "native shell label {label} is unaligned at {offset:#x}")
            }
            Self::BranchOutOfRange {
                label,
                instruction_offset,
                next_pc,
                target,
                displacement,
            } => write!(
                f,
                "native shell branch {label} at {instruction_offset:#x} to {target:#x} from {next_pc:#x} has displacement {displacement}"
            ),
            Self::BranchUnaligned {
                label,
                instruction_offset,
                next_pc,
                target,
            } => write!(
                f,
                "native shell branch {label} at {instruction_offset:#x} from {next_pc:#x} to {target:#x} is not four-byte aligned"
            ),
            Self::CanonicalEncoding { opcode, offset } => write!(
                f,
                "native shell {} at {offset:#x} did not round-trip canonically",
                opcode.definition().mnemonic
            ),
            Self::CodeLength { length, alignment } => write!(
                f,
                "native shell code length {length} is not aligned to {alignment}"
            ),
            Self::CodeOffsetOverflow { offset } => {
                write!(f, "native shell code offset {offset} overflowed")
            }
            Self::AddressOverflow => f.write_str("native shell code arithmetic overflowed"),
        }
    }
}

impl Error for NativeShellEmissionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Instruction(source) => Some(source),
            Self::Decode(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum NativeShellImageError {
    Register(InvalidRegisterIndex),
    ImmediateOutOfRange { value: i64 },
    Emission(NativeShellEmissionError),
    Layout(NativeShellLayoutError),
    Image(LzxError),
    Allocation(TryReserveError),
}

impl fmt::Display for NativeShellImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Register(source) => {
                write!(f, "native shell register selection failed: {source}")
            }
            Self::ImmediateOutOfRange { value } => {
                write!(f, "native shell immediate {value} is outside the LI range")
            }
            Self::Emission(source) => write!(f, "native shell emission failed: {source}"),
            Self::Layout(source) => write!(f, "native shell layout failed: {source}"),
            Self::Image(source) => write!(f, "native shell image construction failed: {source}"),
            Self::Allocation(source) => {
                write!(f, "native shell image allocation failed: {source}")
            }
        }
    }
}

impl Error for NativeShellImageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Register(source) => Some(source),
            Self::Emission(source) => Some(source),
            Self::Image(source) => Some(source),
            Self::Allocation(source) => Some(source),
            Self::ImmediateOutOfRange { .. } | Self::Layout(_) => None,
        }
    }
}

impl From<NativeShellEmissionError> for NativeShellImageError {
    fn from(source: NativeShellEmissionError) -> Self {
        Self::Emission(source)
    }
}

struct DataBuilder {
    bytes: Vec<u8>,
    capacity: u64,
}

impl DataBuilder {
    fn new(capacity: u64) -> Result<Self, NativeShellImageError> {
        let host_capacity = usize::try_from(capacity).map_err(|_| {
            NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion {
                offset: capacity,
            })
        })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(host_capacity)
            .map_err(NativeShellImageError::Allocation)?;
        bytes.resize(host_capacity, 0);
        Ok(Self { bytes, capacity })
    }

    fn put(&mut self, offset: u64, input: &[u8]) -> Result<(), NativeShellImageError> {
        let start = usize::try_from(offset).map_err(|_| {
            NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion { offset })
        })?;
        let length = u64::try_from(input.len()).map_err(|_| {
            NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion {
                offset: u64::MAX,
            })
        })?;
        let end = start
            .checked_add(usize::try_from(length).map_err(|_| {
                NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion {
                    offset: length,
                })
            })?)
            .ok_or(NativeShellImageError::Layout(
                NativeShellLayoutError::AddressOverflow,
            ))?;
        let target = self
            .bytes
            .get_mut(start..end)
            .ok_or(NativeShellImageError::Layout(
                NativeShellLayoutError::InvalidDataRange {
                    offset,
                    length,
                    capacity: self.capacity,
                },
            ))?;
        target.copy_from_slice(input);
        Ok(())
    }
}

struct LabelState {
    offset: Option<u64>,
}

struct BranchPatch {
    instruction_offset: usize,
    condition: Condition,
    label: LabelId,
}

struct Emitter {
    config: ArchitectureConfig,
    code: Vec<u8>,
    labels: Vec<LabelState>,
    branches: Vec<BranchPatch>,
}

fn label_index(label: LabelId) -> Result<usize, NativeShellEmissionError> {
    usize::try_from(label.0).map_err(|_| NativeShellEmissionError::LabelNotFound { label: label.0 })
}

impl Emitter {
    fn new(config: ArchitectureConfig) -> Result<Self, NativeShellEmissionError> {
        let mut code = Vec::new();
        code.try_reserve_exact(512)
            .map_err(NativeShellEmissionError::Allocation)?;
        let mut labels = Vec::new();
        labels
            .try_reserve_exact(32)
            .map_err(NativeShellEmissionError::Allocation)?;
        let mut branches = Vec::new();
        branches
            .try_reserve_exact(32)
            .map_err(NativeShellEmissionError::Allocation)?;
        Ok(Self {
            config,
            code,
            labels,
            branches,
        })
    }

    fn new_label(&mut self) -> Result<LabelId, NativeShellEmissionError> {
        let value = u32::try_from(self.labels.len())
            .map_err(|_| NativeShellEmissionError::LabelLimit { maximum: u32::MAX })?;
        self.labels
            .try_reserve(1)
            .map_err(NativeShellEmissionError::Allocation)?;
        self.labels.push(LabelState { offset: None });
        Ok(LabelId(value))
    }

    fn bind(&mut self, label: LabelId) -> Result<(), NativeShellEmissionError> {
        let offset = code_offset(self.code.len())?;
        if !offset.is_multiple_of(8) {
            return Err(NativeShellEmissionError::UnalignedLabel {
                label: label.0,
                offset,
            });
        }
        let state = self
            .labels
            .get_mut(label_index(label)?)
            .ok_or(NativeShellEmissionError::LabelNotFound { label: label.0 })?;
        if state.offset.is_some() {
            return Err(NativeShellEmissionError::LabelAlreadyBound { label: label.0 });
        }
        state.offset = Some(offset);
        Ok(())
    }

    fn label_offset(&self, label: LabelId) -> Result<u64, NativeShellEmissionError> {
        self.labels
            .get(label_index(label)?)
            .and_then(|state| state.offset)
            .ok_or(NativeShellEmissionError::UnboundLabel { label: label.0 })
    }

    fn emit(
        &mut self,
        opcode: Opcode,
        operands: &[Operand],
    ) -> Result<(), NativeShellEmissionError> {
        let offset = code_offset(self.code.len())?;
        let bytes = checked_encoding(self.config, opcode, operands, offset)?;
        self.code
            .try_reserve(bytes.len())
            .map_err(NativeShellEmissionError::Allocation)?;
        self.code.extend_from_slice(&bytes);
        Ok(())
    }

    fn branch(
        &mut self,
        condition: Condition,
        label: LabelId,
    ) -> Result<(), NativeShellEmissionError> {
        self.branches
            .try_reserve(1)
            .map_err(NativeShellEmissionError::Allocation)?;
        self.emit(
            Opcode::Br,
            &[Operand::Condition(condition), Operand::Immediate(0)],
        )?;
        let instruction_offset = self
            .code
            .len()
            .checked_sub(8)
            .ok_or(NativeShellEmissionError::AddressOverflow)?;
        self.branches.push(BranchPatch {
            instruction_offset,
            condition,
            label,
        });
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, NativeShellEmissionError> {
        for (index, state) in self.labels.iter().enumerate() {
            if state.offset.is_none() {
                return Err(NativeShellEmissionError::UnboundLabel {
                    label: u32::try_from(index)
                        .map_err(|_| NativeShellEmissionError::LabelLimit { maximum: u32::MAX })?,
                });
            }
        }
        let branches = core::mem::take(&mut self.branches);
        for patch in branches {
            let instruction_offset = code_offset(patch.instruction_offset)?;
            let next_pc = instruction_offset
                .checked_add(8)
                .ok_or(NativeShellEmissionError::AddressOverflow)?;
            let target = self.label_offset(patch.label)?;
            let displacement = i128::from(target) - i128::from(next_pc);
            if displacement % 4 != 0 {
                return Err(NativeShellEmissionError::BranchUnaligned {
                    label: patch.label.0,
                    instruction_offset,
                    next_pc,
                    target,
                });
            }
            let immediate = i32::try_from(displacement / 4).map_err(|_| {
                NativeShellEmissionError::BranchOutOfRange {
                    label: patch.label.0,
                    instruction_offset,
                    next_pc,
                    target,
                    displacement,
                }
            })?;
            let bytes = checked_encoding(
                self.config,
                Opcode::Br,
                &[
                    Operand::Condition(patch.condition),
                    Operand::Immediate(immediate),
                ],
                instruction_offset,
            )?;
            let end = patch
                .instruction_offset
                .checked_add(8)
                .ok_or(NativeShellEmissionError::AddressOverflow)?;
            let slot = self.code.get_mut(patch.instruction_offset..end).ok_or(
                NativeShellEmissionError::CodeOffsetOverflow {
                    offset: instruction_offset,
                },
            )?;
            slot.copy_from_slice(&bytes);
        }
        let length = code_offset(self.code.len())?;
        if !length.is_multiple_of(8) {
            return Err(NativeShellEmissionError::CodeLength {
                length,
                alignment: 8,
            });
        }
        let mut offset = 0;
        while offset < self.code.len() {
            let end = offset
                .checked_add(8)
                .ok_or(NativeShellEmissionError::AddressOverflow)?;
            let bytes =
                self.code
                    .get(offset..end)
                    .ok_or(NativeShellEmissionError::CodeOffsetOverflow {
                        offset: code_offset(offset)?,
                    })?;
            let decoded = decode(self.config, bytes).map_err(NativeShellEmissionError::Decode)?;
            let canonical =
                checked_instruction_encoding(self.config, &decoded, code_offset(offset)?)?;
            if canonical.as_slice() != bytes {
                return Err(NativeShellEmissionError::CanonicalEncoding {
                    opcode: decoded.opcode(),
                    offset: code_offset(offset)?,
                });
            }
            offset = end;
        }
        Ok(self.code)
    }
}

fn code_offset(offset: usize) -> Result<u64, NativeShellEmissionError> {
    u64::try_from(offset)
        .map_err(|_| NativeShellEmissionError::CodeOffsetOverflow { offset: u64::MAX })
}

fn checked_encoding(
    config: ArchitectureConfig,
    opcode: Opcode,
    operands: &[Operand],
    offset: u64,
) -> Result<[u8; 8], NativeShellEmissionError> {
    let instruction = Instruction::new(config, opcode, operands)
        .map_err(NativeShellEmissionError::Instruction)?;
    checked_instruction_encoding(config, &instruction, offset)
}

fn checked_instruction_encoding(
    config: ArchitectureConfig,
    instruction: &Instruction,
    offset: u64,
) -> Result<[u8; 8], NativeShellEmissionError> {
    let bytes = encode(config, instruction).map_err(NativeShellEmissionError::Instruction)?;
    let decoded = decode(config, &bytes).map_err(NativeShellEmissionError::Decode)?;
    if decoded != *instruction {
        return Err(NativeShellEmissionError::CanonicalEncoding {
            opcode: instruction.opcode(),
            offset,
        });
    }
    let canonical = encode(config, &decoded).map_err(NativeShellEmissionError::Instruction)?;
    if canonical != bytes {
        return Err(NativeShellEmissionError::CanonicalEncoding {
            opcode: instruction.opcode(),
            offset,
        });
    }
    Ok(bytes)
}

struct Registers {
    r0: RegisterIndex,
    r1: RegisterIndex,
    r2: RegisterIndex,
    r3: RegisterIndex,
    r4: RegisterIndex,
    r5: RegisterIndex,
    r6: RegisterIndex,
    r7: RegisterIndex,
    r8: RegisterIndex,
    r9: RegisterIndex,
    r10: RegisterIndex,
    r11: RegisterIndex,
    r12: RegisterIndex,
    r13: RegisterIndex,
    r14: RegisterIndex,
    r15: RegisterIndex,
}

impl Registers {
    fn new() -> Result<Self, NativeShellImageError> {
        Ok(Self {
            r0: register(0)?,
            r1: register(1)?,
            r2: register(2)?,
            r3: register(3)?,
            r4: register(4)?,
            r5: register(5)?,
            r6: register(6)?,
            r7: register(7)?,
            r8: register(8)?,
            r9: register(9)?,
            r10: register(10)?,
            r11: register(11)?,
            r12: register(12)?,
            r13: register(13)?,
            r14: register(14)?,
            r15: register(15)?,
        })
    }
}

fn register(index: u8) -> Result<RegisterIndex, NativeShellImageError> {
    RegisterIndex::try_from(index).map_err(NativeShellImageError::Register)
}

#[derive(Clone, Copy)]
enum SyscallArgument {
    Immediate(i64),
    Register(RegisterIndex),
}

fn immediate(value: i64) -> Result<i32, NativeShellImageError> {
    i32::try_from(value).map_err(|_| NativeShellImageError::ImmediateOutOfRange { value })
}

fn unsigned_immediate(value: u64) -> Result<i64, NativeShellImageError> {
    i64::try_from(value).map_err(|_| NativeShellImageError::ImmediateOutOfRange { value: i64::MAX })
}

fn length_immediate(bytes: &[u8]) -> Result<i64, NativeShellImageError> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| NativeShellImageError::ImmediateOutOfRange { value: i64::MAX })?;
    unsigned_immediate(length)
}

fn data_address(offset: u64) -> Result<i64, NativeShellImageError> {
    let address = USER_DATA_START
        .checked_add(offset)
        .ok_or(NativeShellImageError::Layout(
            NativeShellLayoutError::AddressOverflow,
        ))?;
    unsigned_immediate(address)
}

fn data_symbol_address(offset: u64) -> Result<i64, NativeShellImageError> {
    let absolute_offset =
        NATIVE_SHELL_DATA_OFFSET
            .checked_add(offset)
            .ok_or(NativeShellImageError::Layout(
                NativeShellLayoutError::AddressOverflow,
            ))?;
    data_address(absolute_offset)
}

fn emit_li(
    emitter: &mut Emitter,
    target: RegisterIndex,
    value: i64,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Li,
            &[
                Operand::Register(target),
                Operand::Immediate(immediate(value)?),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_mov(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    source: RegisterIndex,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Mov,
            &[Operand::Register(destination), Operand::Register(source)],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_add(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    left: RegisterIndex,
    right: RegisterIndex,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Add,
            &[
                Operand::Register(destination),
                Operand::Register(left),
                Operand::Register(right),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_addi(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    source: RegisterIndex,
    value: i64,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Addi,
            &[
                Operand::Register(destination),
                Operand::Register(source),
                Operand::Immediate(immediate(value)?),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_subi(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    source: RegisterIndex,
    value: i64,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Subi,
            &[
                Operand::Register(destination),
                Operand::Register(source),
                Operand::Immediate(immediate(value)?),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_cmp(
    emitter: &mut Emitter,
    left: RegisterIndex,
    right: RegisterIndex,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Cmp,
            &[Operand::Register(left), Operand::Register(right)],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_ldz(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    base: RegisterIndex,
    displacement: i32,
    size: DataSize,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::Ldz,
            &[
                Operand::Register(destination),
                Operand::Memory { base, displacement },
                Operand::DataSize(size),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_st(
    emitter: &mut Emitter,
    source: RegisterIndex,
    base: RegisterIndex,
    displacement: i32,
    size: DataSize,
) -> Result<(), NativeShellImageError> {
    emitter
        .emit(
            Opcode::St,
            &[
                Operand::Register(source),
                Operand::Memory { base, displacement },
                Operand::DataSize(size),
            ],
        )
        .map_err(NativeShellImageError::Emission)
}

fn emit_branch(
    emitter: &mut Emitter,
    condition: Condition,
    label: LabelId,
) -> Result<(), NativeShellImageError> {
    emitter
        .branch(condition, label)
        .map_err(NativeShellImageError::Emission)
}

fn emit_syscall(
    emitter: &mut Emitter,
    registers: &Registers,
    call: Syscall,
    arguments: [SyscallArgument; 6],
) -> Result<(), NativeShellImageError> {
    emit_li(emitter, registers.r0, i64::from(call.as_u16()))?;
    let targets = [
        registers.r1,
        registers.r2,
        registers.r3,
        registers.r4,
        registers.r5,
        registers.r6,
    ];
    for (target, argument) in targets.iter().zip(arguments) {
        match argument {
            SyscallArgument::Immediate(value) => emit_li(emitter, *target, value)?,
            SyscallArgument::Register(source) => emit_mov(emitter, *target, source)?,
        }
    }
    emit_li(emitter, registers.r7, 0)?;
    emitter
        .emit(Opcode::Syscall, &[])
        .map_err(NativeShellImageError::Emission)
}

fn emit_status_guard(
    emitter: &mut Emitter,
    registers: &Registers,
    error: LabelId,
) -> Result<(), NativeShellImageError> {
    emit_li(emitter, registers.r9, 0)?;
    emit_cmp(emitter, registers.r0, registers.r9)?;
    emit_branch(emitter, Condition::Ne, error)
}

fn emit_fixed_write(
    emitter: &mut Emitter,
    registers: &Registers,
    buffer: i64,
    length: i64,
    result: i64,
) -> Result<(), NativeShellImageError> {
    emit_syscall(
        emitter,
        registers,
        Syscall::Write,
        [
            SyscallArgument::Immediate(1),
            SyscallArgument::Immediate(buffer),
            SyscallArgument::Immediate(length),
            SyscallArgument::Immediate(result),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )
}

fn emit_dynamic_write(
    emitter: &mut Emitter,
    registers: &Registers,
    buffer: RegisterIndex,
    length: RegisterIndex,
    result: i64,
) -> Result<(), NativeShellImageError> {
    emit_syscall(
        emitter,
        registers,
        Syscall::Write,
        [
            SyscallArgument::Immediate(1),
            SyscallArgument::Register(buffer),
            SyscallArgument::Register(length),
            SyscallArgument::Immediate(result),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )
}

fn emit_io_count(
    emitter: &mut Emitter,
    destination: RegisterIndex,
    base: RegisterIndex,
    config: ArchitectureConfig,
) -> Result<(), NativeShellImageError> {
    let size = match config.word_width() {
        WordWidth::W32 => DataSize::Word,
        WordWidth::W64 => DataSize::Double,
    };
    emit_ldz(emitter, destination, base, 0, size)
}

fn emit_command_test(
    emitter: &mut Emitter,
    registers: &Registers,
    line_base: RegisterIndex,
    command: &[u8],
    strict: bool,
    next: LabelId,
    handler: LabelId,
) -> Result<(), NativeShellImageError> {
    for (index, byte) in command.iter().enumerate() {
        let displacement = i32::try_from(index)
            .map_err(|_| NativeShellImageError::ImmediateOutOfRange { value: i64::MAX })?;
        emit_ldz(
            emitter,
            registers.r10,
            line_base,
            displacement,
            DataSize::Byte,
        )?;
        emit_li(emitter, registers.r11, i64::from(*byte))?;
        emit_cmp(emitter, registers.r10, registers.r11)?;
        emit_branch(emitter, Condition::Ne, next)?;
    }
    if !strict {
        emit_branch(emitter, Condition::Al, handler)?;
    }
    if strict {
        let displacement = i32::try_from(command.len())
            .map_err(|_| NativeShellImageError::ImmediateOutOfRange { value: i64::MAX })?;
        for terminator in [0, b'\n', b'\r'] {
            emit_ldz(
                emitter,
                registers.r10,
                line_base,
                displacement,
                DataSize::Byte,
            )?;
            emit_li(emitter, registers.r11, i64::from(terminator))?;
            emit_cmp(emitter, registers.r10, registers.r11)?;
            emit_branch(emitter, Condition::Eq, handler)?;
        }
    }
    Ok(())
}

fn build_data() -> Result<Vec<u8>, NativeShellImageError> {
    let mut data = DataBuilder::new(NATIVE_SHELL_DATA_LENGTH)?;
    data.put(0x000, NATIVE_SHELL_PROMPT)?;
    data.put(0x040, NATIVE_SHELL_HELP)?;
    data.put(0x080, b"echo ")?;
    data.put(0x0c0, NATIVE_SHELL_CAT_PATH)?;
    data.put(0x0e0, NATIVE_SHELL_LS_PATH)?;
    data.put(0x100, NATIVE_SHELL_RUN_DEFERRED)?;
    data.put(0x180, NATIVE_SHELL_UNKNOWN_COMMAND)?;
    data.put(0x1c0, NATIVE_SHELL_LS_HEADER)?;
    data.put(0x200, NATIVE_SHELL_CAT_UNAVAILABLE)?;
    data.put(0x220, NATIVE_SHELL_IO_ERROR)?;
    data.put(0x240, b"help")?;
    data.put(0x248, b"echo")?;
    data.put(0x250, b"ls")?;
    data.put(0x258, b"cat")?;
    data.put(0x260, b"run")?;
    data.put(0x268, b"clear")?;
    Ok(data.bytes)
}

fn validate_layout(data_length: usize) -> Result<NativeShellLayout, NativeShellImageError> {
    let data_length = u64::try_from(data_length).map_err(|_| {
        NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion { offset: u64::MAX })
    })?;
    if data_length != NATIVE_SHELL_DATA_LENGTH {
        return Err(NativeShellImageError::Layout(
            NativeShellLayoutError::InvalidDataRange {
                offset: NATIVE_SHELL_DATA_OFFSET,
                length: data_length,
                capacity: NATIVE_SHELL_DATA_LENGTH,
            },
        ));
    }
    let data_end =
        NATIVE_SHELL_DATA_OFFSET
            .checked_add(data_length)
            .ok_or(NativeShellImageError::Layout(
                NativeShellLayoutError::AddressOverflow,
            ))?;
    let bss_end = NATIVE_SHELL_BSS_OFFSET
        .checked_add(NATIVE_SHELL_BSS_LENGTH)
        .ok_or(NativeShellImageError::Layout(
            NativeShellLayoutError::AddressOverflow,
        ))?;
    if data_end > NATIVE_SHELL_BSS_OFFSET {
        return Err(NativeShellImageError::Layout(
            NativeShellLayoutError::SectionOverlap {
                first_start: NATIVE_SHELL_DATA_OFFSET,
                first_end: data_end,
                second_start: NATIVE_SHELL_BSS_OFFSET,
                second_end: bss_end,
            },
        ));
    }
    if bss_end > NATIVE_SHELL_REQUIRED_DATA {
        return Err(NativeShellImageError::Layout(
            NativeShellLayoutError::RequiredDataOverflow { end: bss_end },
        ));
    }
    if bss_end > USER_DATA_LENGTH {
        return Err(NativeShellImageError::Layout(
            NativeShellLayoutError::OutsideUserData {
                start: NATIVE_SHELL_BSS_OFFSET,
                end: bss_end,
                maximum: USER_DATA_LENGTH,
            },
        ));
    }
    for offset in [
        NATIVE_SHELL_DATA_OFFSET,
        NATIVE_SHELL_BSS_OFFSET,
        NATIVE_SHELL_LINE_BUFFER_OFFSET,
        NATIVE_SHELL_IO_RESULT_OFFSET,
        NATIVE_SHELL_DIRECTORY_OFFSET,
        NATIVE_SHELL_FILE_BUFFER_OFFSET,
    ] {
        let address = USER_DATA_START
            .checked_add(offset)
            .ok_or(NativeShellImageError::Layout(
                NativeShellLayoutError::AddressOverflow,
            ))?;
        if !address.is_multiple_of(8) {
            return Err(NativeShellImageError::Layout(
                NativeShellLayoutError::MisalignedOffset {
                    offset,
                    alignment: 8,
                },
            ));
        }
    }
    Ok(NATIVE_SHELL_LAYOUT)
}

fn build_code(architecture: LzxArchitecture) -> Result<Vec<u8>, NativeShellImageError> {
    let config = architecture.config();
    let registers = Registers::new()?;
    let mut emitter = Emitter::new(config).map_err(NativeShellImageError::Emission)?;

    let start = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let loop_label = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let help_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let echo_bare_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let echo_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let ls_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let cat_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let run_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let clear_test = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let unknown_label = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let help_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let echo_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let echo_empty = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let ls_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let ls_records_loop = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let ls_records_end = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let cat_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let run_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let clear_handler = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let cat_error = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let io_error = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;
    let eof = emitter
        .new_label()
        .map_err(NativeShellImageError::Emission)?;

    let line = data_address(NATIVE_SHELL_LINE_BUFFER_OFFSET)?;
    let io_result = data_address(NATIVE_SHELL_IO_RESULT_OFFSET)?;
    let directory = data_address(NATIVE_SHELL_DIRECTORY_OFFSET)?;
    let file = data_address(NATIVE_SHELL_FILE_BUFFER_OFFSET)?;
    let prompt = data_symbol_address(0x000)?;
    let help = data_symbol_address(0x040)?;
    let run_message = data_symbol_address(0x100)?;
    let unknown_message = data_symbol_address(0x180)?;
    let ls_header = data_symbol_address(0x1c0)?;
    let ls_newline = data_symbol_address(
        0x1c0
            + u64::try_from(NATIVE_SHELL_LS_HEADER.len() - 1).map_err(|_| {
                NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion {
                    offset: u64::MAX,
                })
            })?,
    )?;
    let cat_message = data_symbol_address(0x200)?;
    let io_error_message = data_symbol_address(0x220)?;
    let cat_path = data_symbol_address(0x0c0)?;
    let ls_path = data_symbol_address(0x0e0)?;
    let line_length = unsigned_immediate(NATIVE_SHELL_LINE_CAPACITY)?;
    let file_length = unsigned_immediate(NATIVE_SHELL_FILE_CAPACITY)?;
    let directory_capacity = unsigned_immediate(NATIVE_SHELL_DIRECTORY_CAPACITY)?;
    let prompt_length = length_immediate(NATIVE_SHELL_PROMPT)?;
    let help_length = length_immediate(NATIVE_SHELL_HELP)?;
    let run_length = length_immediate(NATIVE_SHELL_RUN_DEFERRED)?;
    let unknown_length = length_immediate(NATIVE_SHELL_UNKNOWN_COMMAND)?;
    let ls_header_length = length_immediate(NATIVE_SHELL_LS_HEADER)?;
    let cat_message_length = length_immediate(NATIVE_SHELL_CAT_UNAVAILABLE)?;
    let io_error_length = length_immediate(NATIVE_SHELL_IO_ERROR)?;
    let cat_path_length = length_immediate(NATIVE_SHELL_CAT_PATH)?;
    let ls_path_length = length_immediate(NATIVE_SHELL_LS_PATH)?;

    emitter
        .bind(start)
        .map_err(NativeShellImageError::Emission)?;
    emit_li(&mut emitter, registers.r7, 0)?;
    emit_li(&mut emitter, registers.r8, line)?;
    emit_li(&mut emitter, registers.r12, file)?;
    emit_li(&mut emitter, registers.r13, io_result)?;
    emit_li(&mut emitter, registers.r14, directory)?;
    emitter
        .bind(loop_label)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(&mut emitter, &registers, prompt, prompt_length, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Read,
        [
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(line),
            SyscallArgument::Immediate(line_length),
            SyscallArgument::Immediate(io_result),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_io_count(&mut emitter, registers.r9, registers.r13, config)?;
    emit_li(&mut emitter, registers.r10, 0)?;
    emit_add(&mut emitter, registers.r14, registers.r8, registers.r9)?;
    emit_st(
        &mut emitter,
        registers.r10,
        registers.r14,
        0,
        DataSize::Byte,
    )?;
    emit_cmp(&mut emitter, registers.r9, registers.r10)?;
    emit_branch(&mut emitter, Condition::Eq, eof)?;

    emitter
        .bind(help_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"help",
        true,
        echo_bare_test,
        help_handler,
    )?;
    emitter
        .bind(echo_bare_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"echo",
        true,
        echo_test,
        echo_handler,
    )?;
    emitter
        .bind(echo_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"echo ",
        false,
        ls_test,
        echo_handler,
    )?;
    emitter
        .bind(ls_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"ls",
        true,
        cat_test,
        ls_handler,
    )?;
    emitter
        .bind(cat_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"cat",
        true,
        run_test,
        cat_handler,
    )?;
    emitter
        .bind(run_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"run",
        true,
        clear_test,
        run_handler,
    )?;
    emitter
        .bind(clear_test)
        .map_err(NativeShellImageError::Emission)?;
    emit_command_test(
        &mut emitter,
        &registers,
        registers.r8,
        b"clear",
        true,
        unknown_label,
        clear_handler,
    )?;

    emitter
        .bind(unknown_label)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(
        &mut emitter,
        &registers,
        unknown_message,
        unknown_length,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(help_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(&mut emitter, &registers, help, help_length, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(echo_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_li(&mut emitter, registers.r10, 5)?;
    emit_cmp(&mut emitter, registers.r9, registers.r10)?;
    emit_branch(&mut emitter, Condition::Ult, echo_empty)?;
    emit_addi(&mut emitter, registers.r10, registers.r8, 5)?;
    emit_subi(&mut emitter, registers.r11, registers.r9, 5)?;
    emit_dynamic_write(
        &mut emitter,
        &registers,
        registers.r10,
        registers.r11,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_fixed_write(&mut emitter, &registers, ls_newline, 1, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;
    emitter
        .bind(echo_empty)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(&mut emitter, &registers, ls_newline, 1, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(ls_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_li(&mut emitter, registers.r14, directory)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::ListDirectory,
        [
            SyscallArgument::Immediate(ls_path),
            SyscallArgument::Immediate(ls_path_length),
            SyscallArgument::Register(registers.r14),
            SyscallArgument::Immediate(directory_capacity),
            SyscallArgument::Immediate(io_result),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_io_count(&mut emitter, registers.r10, registers.r13, config)?;
    emit_li(&mut emitter, registers.r11, 0)?;
    emit_mov(&mut emitter, registers.r15, registers.r14)?;
    emit_fixed_write(
        &mut emitter,
        &registers,
        ls_header,
        ls_header_length,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emitter
        .bind(ls_records_loop)
        .map_err(NativeShellImageError::Emission)?;
    emit_cmp(&mut emitter, registers.r11, registers.r10)?;
    emit_branch(&mut emitter, Condition::Uge, ls_records_end)?;
    emit_ldz(&mut emitter, registers.r9, registers.r15, 0, DataSize::Half)?;
    emit_addi(&mut emitter, registers.r14, registers.r15, 4)?;
    emit_dynamic_write(
        &mut emitter,
        &registers,
        registers.r14,
        registers.r9,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_fixed_write(&mut emitter, &registers, ls_newline, 1, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_addi(&mut emitter, registers.r15, registers.r15, 256)?;
    emit_addi(&mut emitter, registers.r11, registers.r11, 1)?;
    emit_branch(&mut emitter, Condition::Al, ls_records_loop)?;
    emitter
        .bind(ls_records_end)
        .map_err(NativeShellImageError::Emission)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(cat_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Open,
        [
            SyscallArgument::Immediate(cat_path),
            SyscallArgument::Immediate(cat_path_length),
            SyscallArgument::Immediate(i64::from(OPEN_READ)),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_mov(&mut emitter, registers.r15, registers.r1)?;
    emit_li(&mut emitter, registers.r10, 0)?;
    emit_cmp(&mut emitter, registers.r0, registers.r10)?;
    emit_branch(&mut emitter, Condition::Ne, cat_error)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Read,
        [
            SyscallArgument::Register(registers.r15),
            SyscallArgument::Immediate(file),
            SyscallArgument::Immediate(file_length),
            SyscallArgument::Immediate(io_result),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_io_count(&mut emitter, registers.r10, registers.r13, config)?;
    emit_dynamic_write(
        &mut emitter,
        &registers,
        registers.r12,
        registers.r10,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Close,
        [
            SyscallArgument::Register(registers.r15),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(cat_error)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(
        &mut emitter,
        &registers,
        cat_message,
        cat_message_length,
        io_result,
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(run_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(&mut emitter, &registers, run_message, run_length, io_result)?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(clear_handler)
        .map_err(NativeShellImageError::Emission)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::ClearScreen,
        [
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;
    emit_status_guard(&mut emitter, &registers, io_error)?;
    emit_branch(&mut emitter, Condition::Al, loop_label)?;

    emitter
        .bind(io_error)
        .map_err(NativeShellImageError::Emission)?;
    emit_fixed_write(
        &mut emitter,
        &registers,
        io_error_message,
        io_error_length,
        io_result,
    )?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Exit,
        [
            SyscallArgument::Immediate(1),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;

    emitter.bind(eof).map_err(NativeShellImageError::Emission)?;
    emit_syscall(
        &mut emitter,
        &registers,
        Syscall::Exit,
        [
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
            SyscallArgument::Immediate(0),
        ],
    )?;

    emitter.finish().map_err(NativeShellImageError::Emission)
}

pub fn build_init_shell_image(
    architecture: LzxArchitecture,
) -> Result<LzxImage, NativeShellImageError> {
    let data = build_data()?;
    let code = build_code(architecture)?;
    let layout = validate_layout(data.len())?;
    let data_length = u64::try_from(data.len()).map_err(|_| {
        NativeShellImageError::Layout(NativeShellLayoutError::OffsetConversion { offset: u64::MAX })
    })?;
    let code_section = LzxSection::code(&code).map_err(NativeShellImageError::Image)?;
    let data_section = LzxSection::new(
        LzxSectionKind::Data,
        LZX_DATA_PERMISSIONS,
        layout.data_offset,
        data_length,
        8,
        &data,
    )
    .map_err(NativeShellImageError::Image)?;
    let bss_section = LzxSection::new(
        LzxSectionKind::Bss,
        LZX_DATA_PERMISSIONS,
        layout.bss_offset,
        layout.bss_length,
        8,
        &[],
    )
    .map_err(NativeShellImageError::Image)?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(3)
        .map_err(NativeShellImageError::Allocation)?;
    sections.push(code_section);
    sections.push(data_section);
    sections.push(bss_section);
    LzxImage::new(
        architecture,
        0,
        0,
        layout.required_data,
        USER_STACK_LENGTH,
        sections,
    )
    .map_err(NativeShellImageError::Image)
}
