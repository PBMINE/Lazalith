use crate::{
    MemoryError, Process, ProcessError, ProcessId, ProgramError, ProgramImage, ThreadId,
    USER_CODE_LENGTH, USER_DATA_LENGTH, USER_STACK_LENGTH,
};
use alloc::{collections::TryReserveError, format, string::String, vec::Vec};
use core::{error::Error, fmt};
use lazalith_os_abi::ABI_VERSION;
use lazalith_types::ArchitectureConfig;

pub const LZX_MAGIC: [u8; 8] = *b"LZXLOAD1";
pub const LZX_FORMAT_VERSION: u16 = 2;
pub const LZX_ISA_VERSION: u16 = 1;
pub const LZX_ABI_VERSION: u16 = ABI_VERSION;
pub const LZX_HEADER_SIZE: usize = 64;
pub const LZX_SECTION_ENTRY_SIZE: usize = 48;
pub const LZX_MAX_SECTIONS: usize = 3;
pub const LZX_MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
pub const LZX_CODE_PERMISSIONS: u8 = 0x0d;
pub const LZX_DATA_PERMISSIONS: u8 = 0x0b;
const _: () = assert!(LZX_ISA_VERSION == lazalith_isa::ISA_VERSION);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LzxArchitecture {
    Lz32 = 1,
    Lz64 = 2,
}

impl LzxArchitecture {
    pub const fn config(self) -> ArchitectureConfig {
        match self {
            Self::Lz32 => ArchitectureConfig::lz32(),
            Self::Lz64 => ArchitectureConfig::lz64(),
        }
    }

    pub const fn from_config(config: ArchitectureConfig) -> Self {
        match config.word_width() {
            lazalith_types::WordWidth::W32 => Self::Lz32,
            lazalith_types::WordWidth::W64 => Self::Lz64,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LzxSectionKind {
    Code = 1,
    Data = 2,
    Bss = 3,
}

impl TryFrom<u8> for LzxSectionKind {
    type Error = LzxError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Code),
            2 => Ok(Self::Data),
            3 => Ok(Self::Bss),
            _ => Err(LzxError::InvalidSectionKind { value }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LzxSection {
    kind: LzxSectionKind,
    permissions: u8,
    virtual_offset: u64,
    virtual_size: u64,
    alignment: u64,
    bytes: Vec<u8>,
}

impl LzxSection {
    pub fn new(
        kind: LzxSectionKind,
        permissions: u8,
        virtual_offset: u64,
        virtual_size: u64,
        alignment: u64,
        bytes: &[u8],
    ) -> Result<Self, LzxError> {
        let mut stored = Vec::new();
        stored
            .try_reserve_exact(bytes.len())
            .map_err(LzxError::Allocation)?;
        stored.extend_from_slice(bytes);
        Ok(Self {
            kind,
            permissions,
            virtual_offset,
            virtual_size,
            alignment,
            bytes: stored,
        })
    }

    pub fn code(bytes: &[u8]) -> Result<Self, LzxError> {
        Self::new(
            LzxSectionKind::Code,
            LZX_CODE_PERMISSIONS,
            0,
            bytes.len() as u64,
            4,
            bytes,
        )
    }

    pub fn data(bytes: &[u8], alignment: u64) -> Result<Self, LzxError> {
        Self::new(
            LzxSectionKind::Data,
            LZX_DATA_PERMISSIONS,
            0,
            bytes.len() as u64,
            alignment,
            bytes,
        )
    }

    pub fn bss(size: u64, alignment: u64) -> Result<Self, LzxError> {
        Self::new(
            LzxSectionKind::Bss,
            LZX_DATA_PERMISSIONS,
            0,
            size,
            alignment,
            &[],
        )
    }

    pub const fn kind(&self) -> LzxSectionKind {
        self.kind
    }

    pub const fn permissions(&self) -> u8 {
        self.permissions
    }

    pub const fn virtual_offset(&self) -> u64 {
        self.virtual_offset
    }

    pub const fn virtual_size(&self) -> u64 {
        self.virtual_size
    }

    pub const fn alignment(&self) -> u64 {
        self.alignment
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LzxImage {
    architecture: LzxArchitecture,
    isa_version: u16,
    abi_version: u16,
    entry_section: u16,
    entry_offset: u64,
    required_data: u64,
    required_stack: u64,
    sections: Vec<LzxSection>,
    /// The program's source-level debug information, when it was built with any.
    ///
    /// `None` and an empty block mean the same thing to a reader — no mappings — but
    /// only `Some` writes a table, so an image built without debug information does
    /// not carry the block's eight-byte header for nothing.
    debug: Option<crate::debug::DebugBlock>,
}

impl LzxImage {
    pub fn new(
        architecture: LzxArchitecture,
        entry_section: u16,
        entry_offset: u64,
        required_data: u64,
        required_stack: u64,
        sections: Vec<LzxSection>,
    ) -> Result<Self, LzxError> {
        Self::with_debug(
            architecture,
            entry_section,
            entry_offset,
            required_data,
            required_stack,
            sections,
            None,
        )
    }

    /// An image that also carries source-level debug information.
    ///
    /// The block is taken rather than built here: it is the linker's table, already
    /// fixed up to this image's addresses, and an image that rebuilt it would be a
    /// second derivation of the same addresses.
    pub fn with_debug(
        architecture: LzxArchitecture,
        entry_section: u16,
        entry_offset: u64,
        required_data: u64,
        required_stack: u64,
        sections: Vec<LzxSection>,
        debug: Option<crate::debug::DebugBlock>,
    ) -> Result<Self, LzxError> {
        let image = Self {
            architecture,
            isa_version: LZX_ISA_VERSION,
            abi_version: ABI_VERSION,
            entry_section,
            entry_offset,
            required_data,
            required_stack,
            sections,
            debug,
        };
        image.validate()?;
        Ok(image)
    }

    /// The image's source-level debug information, if it has any.
    pub const fn debug(&self) -> Option<&crate::debug::DebugBlock> {
        self.debug.as_ref()
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LzxError> {
        parse(bytes)
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, LzxError> {
        self.encode()
    }

    pub const fn architecture(&self) -> LzxArchitecture {
        self.architecture
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.architecture.config()
    }

    pub const fn isa_version(&self) -> u16 {
        self.isa_version
    }

    pub const fn abi_version(&self) -> u16 {
        self.abi_version
    }

    pub const fn entry_section(&self) -> u16 {
        self.entry_section
    }

    pub const fn entry_offset(&self) -> u64 {
        self.entry_offset
    }

    pub const fn required_data(&self) -> u64 {
        self.required_data
    }

    pub const fn required_stack(&self) -> u64 {
        self.required_stack
    }

    pub fn sections(&self) -> &[LzxSection] {
        &self.sections
    }

    pub fn load_process(
        &self,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<Process, LzxError> {
        self.validate()?;
        let code = self.sections.first().ok_or(LzxError::MissingCode)?.bytes();
        let program =
            ProgramImage::new(self.config(), self.entry_offset, code).map_err(LzxError::Program)?;
        let mut process =
            Process::new(process_id, thread_id, program).map_err(LzxError::Process)?;
        for section in &self.sections {
            match section.kind {
                LzxSectionKind::Code => {}
                LzxSectionKind::Data => process
                    .memory_mut()
                    .load_data(section.virtual_offset, &section.bytes)
                    .map_err(LzxError::Memory)?,
                LzxSectionKind::Bss => process
                    .memory_mut()
                    .zero_data(section.virtual_offset, section.virtual_size)
                    .map_err(LzxError::Memory)?,
            }
        }
        process
            .memory_mut()
            .reserve_data(self.required_data)
            .map_err(LzxError::Memory)?;
        Ok(process)
    }

    fn validate(&self) -> Result<(), LzxError> {
        if self.sections.is_empty() || self.sections.len() > LZX_MAX_SECTIONS {
            return Err(LzxError::InvalidSectionCount {
                value: u16::try_from(self.sections.len()).unwrap_or(u16::MAX),
            });
        }
        if self.isa_version != LZX_ISA_VERSION {
            return Err(LzxError::UnsupportedIsaVersion {
                value: self.isa_version,
            });
        }
        if self.abi_version != ABI_VERSION {
            return Err(LzxError::UnsupportedAbiVersion {
                value: self.abi_version,
            });
        }
        if self.entry_section != 0 {
            return Err(LzxError::InvalidEntrySection {
                value: self.entry_section,
            });
        }
        if self.required_stack != USER_STACK_LENGTH {
            return Err(LzxError::InvalidStackRequirement {
                required: self.required_stack,
                expected: USER_STACK_LENGTH,
            });
        }
        let config = self.config();
        let mut data_end = 0;
        for (index, section) in self.sections.iter().enumerate() {
            let expected_kind = match index {
                0 => LzxSectionKind::Code,
                1 => LzxSectionKind::Data,
                2 => LzxSectionKind::Bss,
                _ => {
                    return Err(LzxError::InvalidSectionCount {
                        value: u16::try_from(index).unwrap_or(u16::MAX),
                    });
                }
            };
            if section.kind != expected_kind {
                return Err(LzxError::InvalidSectionOrder {
                    index,
                    expected: expected_kind,
                    actual: section.kind,
                });
            }
            let expected_permissions = match section.kind {
                LzxSectionKind::Code => LZX_CODE_PERMISSIONS,
                LzxSectionKind::Data | LzxSectionKind::Bss => LZX_DATA_PERMISSIONS,
            };
            if section.permissions != expected_permissions {
                return Err(LzxError::InvalidSectionPermissions {
                    index,
                    value: section.permissions,
                    expected: expected_permissions,
                });
            }
            if !valid_alignment(section.alignment) {
                return Err(LzxError::InvalidSectionAlignment {
                    index,
                    value: section.alignment,
                });
            }
            let end = section
                .virtual_offset
                .checked_add(section.virtual_size)
                .ok_or(LzxError::AddressOverflow)?;
            match section.kind {
                LzxSectionKind::Code => {
                    if section.virtual_offset != 0
                        || section.alignment != u64::from(config.instruction_alignment())
                    {
                        return Err(LzxError::InvalidCodeSection { index });
                    }
                    if section.bytes.is_empty()
                        || section.virtual_size != section.bytes.len() as u64
                    {
                        return Err(LzxError::InvalidSectionSize {
                            index,
                            expected: section.bytes.len() as u64,
                            actual: section.virtual_size,
                        });
                    }
                    if section.virtual_size > USER_CODE_LENGTH {
                        return Err(LzxError::SectionTooLarge {
                            index,
                            length: section.virtual_size,
                            maximum: USER_CODE_LENGTH,
                        });
                    }
                    let entry_end = self
                        .entry_offset
                        .checked_add(8)
                        .ok_or(LzxError::AddressOverflow)?;
                    if !self
                        .entry_offset
                        .is_multiple_of(u64::from(config.instruction_alignment()))
                        || entry_end > section.virtual_size
                    {
                        return Err(LzxError::InvalidEntryOffset {
                            offset: self.entry_offset,
                            code_size: section.virtual_size,
                        });
                    }
                }
                LzxSectionKind::Data => {
                    if section.virtual_size != section.bytes.len() as u64 {
                        return Err(LzxError::InvalidSectionSize {
                            index,
                            expected: section.bytes.len() as u64,
                            actual: section.virtual_size,
                        });
                    }
                    if !aligned_user_data(section.virtual_offset, section.alignment)? {
                        return Err(LzxError::InvalidSectionAlignment {
                            index,
                            value: section.alignment,
                        });
                    }
                    data_end = data_end.max(end);
                }
                LzxSectionKind::Bss => {
                    if section.virtual_size == 0 || !section.bytes.is_empty() {
                        return Err(LzxError::InvalidSectionSize {
                            index,
                            expected: 0,
                            actual: section.virtual_size,
                        });
                    }
                    if !aligned_user_data(section.virtual_offset, section.alignment)? {
                        return Err(LzxError::InvalidSectionAlignment {
                            index,
                            value: section.alignment,
                        });
                    }
                    data_end = data_end.max(end);
                }
            }
        }
        for (first, section) in self.sections.iter().enumerate() {
            if !matches!(section.kind, LzxSectionKind::Data | LzxSectionKind::Bss) {
                continue;
            }
            let first_end = section
                .virtual_offset
                .checked_add(section.virtual_size)
                .ok_or(LzxError::AddressOverflow)?;
            for (second, other) in self.sections.iter().enumerate().skip(first + 1) {
                if !matches!(other.kind, LzxSectionKind::Data | LzxSectionKind::Bss) {
                    continue;
                }
                let second_end = other
                    .virtual_offset
                    .checked_add(other.virtual_size)
                    .ok_or(LzxError::AddressOverflow)?;
                if section.virtual_offset < second_end && other.virtual_offset < first_end {
                    return Err(LzxError::SectionVirtualOverlap {
                        first,
                        second,
                        first_start: section.virtual_offset,
                        first_end,
                        second_start: other.virtual_offset,
                        second_end,
                    });
                }
            }
        }
        if data_end > USER_DATA_LENGTH || self.required_data > USER_DATA_LENGTH {
            return Err(LzxError::InvalidMemoryRequirement {
                required: self.required_data.max(data_end),
                maximum: USER_DATA_LENGTH,
            });
        }
        if self.required_data < data_end {
            return Err(LzxError::InvalidMemoryRequirement {
                required: data_end,
                maximum: USER_DATA_LENGTH,
            });
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, LzxError> {
        self.validate()?;
        let section_count = u16::try_from(self.sections.len())
            .map_err(|_| LzxError::InvalidSectionCount { value: u16::MAX })?;
        let table_size = self
            .sections
            .len()
            .checked_mul(LZX_SECTION_ENTRY_SIZE)
            .ok_or(LzxError::AddressOverflow)?;
        let table_size_u32 = u32::try_from(table_size).map_err(|_| LzxError::AddressOverflow)?;
        let table_end = u64::try_from(LZX_HEADER_SIZE)
            .ok()
            .and_then(|value| value.checked_add(u64::try_from(table_size).ok()?))
            .ok_or(LzxError::AddressOverflow)?;
        let payload_offset = align_up(table_end, 8).ok_or(LzxError::AddressOverflow)?;
        // The debug block goes after the sections' bytes, and the last two words
        // of the header — which used to hold a section-table offset the header
        // already implied — carry where it is and how long it is. An image with
        // none writes a zero length *and* a zero offset, and a reader treats that
        // as "there is none" rather than as an empty table at some address.
        let mut debug = Vec::new();
        if let Some(block) = &self.debug {
            debug
                .try_reserve(block.encoded_length())
                .map_err(LzxError::Allocation)?;
            debug.extend_from_slice(&block.encode());
        }
        let mut file_offsets = Vec::new();
        file_offsets
            .try_reserve_exact(self.sections.len())
            .map_err(LzxError::Allocation)?;
        let mut next_offset = payload_offset;
        for section in &self.sections {
            file_offsets.push(next_offset);
            next_offset = next_offset
                .checked_add(section.bytes.len() as u64)
                .ok_or(LzxError::AddressOverflow)?;
        }
        // The debug block follows the sections, so it is measured in before the
        // file-size check rather than after: an image that carried a huge table
        // would otherwise be written and only then found to be too big.
        let debug_offset = next_offset;
        next_offset = next_offset
            .checked_add(debug.len() as u64)
            .ok_or(LzxError::AddressOverflow)?;
        let total = usize::try_from(next_offset).map_err(|_| LzxError::FileTooLarge {
            length: next_offset,
            maximum: LZX_MAX_FILE_SIZE,
        })?;
        if next_offset > LZX_MAX_FILE_SIZE {
            return Err(LzxError::FileTooLarge {
                length: next_offset,
                maximum: LZX_MAX_FILE_SIZE,
            });
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(LzxError::Allocation)?;
        bytes.extend_from_slice(&LZX_MAGIC);
        put_u16(&mut bytes, LZX_FORMAT_VERSION);
        put_u16(&mut bytes, LZX_HEADER_SIZE as u16);
        bytes.push(architecture_byte(self.architecture));
        bytes.push(0);
        put_u16(&mut bytes, LZX_ISA_VERSION);
        put_u16(&mut bytes, ABI_VERSION);
        put_u16(&mut bytes, section_count);
        put_u16(&mut bytes, self.entry_section);
        put_u32(
            &mut bytes,
            u32::try_from(self.entry_offset).map_err(|_| LzxError::AddressOverflow)?,
        );
        put_u16(&mut bytes, 0);
        put_u64(&mut bytes, self.required_data);
        put_u64(&mut bytes, self.required_stack);
        // Bytes 44..64 of the header carry where the section table is, where the
        // sections' bytes begin, and where the debug block went. An image with no
        // debug information zeroes the last two words rather than leaving an
        // offset pointing at the end of the file: "none" then reads the same in
        // the file as it does in memory, and a reader never has to consult a
        // length of zero to learn that an offset is meaningless.
        // The words are 32-bit because the header is a fixed 64 bytes and the
        // file is capped far below what a 32-bit offset cannot address.
        let (debug_offset, debug_length) = if debug.is_empty() {
            (0, 0)
        } else {
            (
                u32::try_from(debug_offset).map_err(|_| LzxError::AddressOverflow)?,
                u32::try_from(debug.len()).map_err(|_| LzxError::AddressOverflow)?,
            )
        };
        put_u32(&mut bytes, table_size_u32);
        put_u64(&mut bytes, payload_offset);
        put_u32(&mut bytes, debug_offset);
        put_u32(&mut bytes, debug_length);
        for (index, section) in self.sections.iter().enumerate() {
            bytes.push(section.kind as u8);
            bytes.push(section.permissions);
            put_u16(&mut bytes, 0);
            put_u64(&mut bytes, section.virtual_offset);
            put_u64(&mut bytes, section.virtual_size);
            put_u64(&mut bytes, file_offsets[index]);
            put_u64(&mut bytes, section.bytes.len() as u64);
            put_u64(&mut bytes, section.alignment);
            put_u32(&mut bytes, 0);
        }
        while bytes.len() < payload_offset as usize {
            bytes.push(0);
        }
        for section in &self.sections {
            bytes.extend_from_slice(&section.bytes);
        }
        while bytes.len() < debug_offset as usize {
            bytes.push(0);
        }
        bytes.extend_from_slice(&debug);
        Ok(bytes)
    }
}

#[derive(Debug)]
pub enum LzxError {
    Truncated {
        offset: u64,
        needed: usize,
        available: usize,
    },
    InvalidMagic,
    UnsupportedFormat {
        version: u16,
    },
    InvalidHeaderSize {
        expected: u16,
        actual: u16,
    },
    UnsupportedArchitecture {
        value: u8,
    },
    UnsupportedIsaVersion {
        value: u16,
    },
    UnsupportedAbiVersion {
        value: u16,
    },
    InvalidHeaderFlags {
        value: u8,
    },
    InvalidReserved {
        offset: u64,
    },
    InvalidSectionCount {
        value: u16,
    },
    InvalidEntrySection {
        value: u16,
    },
    InvalidEntryOffset {
        offset: u64,
        code_size: u64,
    },
    InvalidPayloadOffset {
        expected: u64,
        actual: u64,
    },
    InvalidSectionTableSize {
        expected: u32,
        actual: u32,
    },
    InvalidSectionKind {
        value: u8,
    },
    InvalidSectionOrder {
        index: usize,
        expected: LzxSectionKind,
        actual: LzxSectionKind,
    },
    InvalidSectionPermissions {
        index: usize,
        value: u8,
        expected: u8,
    },
    InvalidSectionSize {
        index: usize,
        expected: u64,
        actual: u64,
    },
    InvalidSectionAlignment {
        index: usize,
        value: u64,
    },
    InvalidCodeSection {
        index: usize,
    },
    SectionTooLarge {
        index: usize,
        length: u64,
        maximum: u64,
    },
    SectionRange {
        index: usize,
        offset: u64,
        size: u64,
        file_size: u64,
    },
    SectionFileOverlap {
        first: usize,
        second: usize,
        first_start: u64,
        first_end: u64,
        second_start: u64,
        second_end: u64,
    },
    SectionVirtualOverlap {
        first: usize,
        second: usize,
        first_start: u64,
        first_end: u64,
        second_start: u64,
        second_end: u64,
    },
    InvalidMemoryRequirement {
        required: u64,
        maximum: u64,
    },
    InvalidStackRequirement {
        required: u64,
        expected: u64,
    },
    FileTooLarge {
        length: u64,
        maximum: u64,
    },
    TrailingBytes {
        offset: u64,
        length: u64,
    },
    /// The image's debug block did not read.
    ///
    /// A separate case from a truncated file: the bytes are all there and they are
    /// not a table, which means the file is not the image it claims to be. The
    /// reason is carried as text because a caller that has to report this wants to
    /// say what was wrong, and a debugger wants to know whether to offer to open
    /// the image at all.
    DebugBlock {
        /// What was wrong, as the reader saw it.
        reason: String,
    },
    AddressOverflow,
    MissingCode,
    Allocation(TryReserveError),
    Program(ProgramError),
    Process(ProcessError),
    Memory(MemoryError),
}

impl fmt::Display for LzxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated {
                offset,
                needed,
                available,
            } => write!(
                f,
                "truncated .lzx input at {offset:#x}: need {needed}, have {available}"
            ),
            Self::InvalidMagic => f.write_str("invalid .lzx magic"),
            Self::UnsupportedFormat { version } => {
                write!(f, "unsupported .lzx format version {version}")
            }
            Self::InvalidHeaderSize { expected, actual } => {
                write!(f, "invalid .lzx header size {actual}, expected {expected}")
            }
            Self::UnsupportedArchitecture { value } => {
                write!(f, "unsupported .lzx architecture {value}")
            }
            Self::UnsupportedIsaVersion { value } => {
                write!(f, "unsupported .lzx ISA version {value}")
            }
            Self::UnsupportedAbiVersion { value } => {
                write!(f, "unsupported .lzx ABI version {value}")
            }
            Self::InvalidHeaderFlags { value } => {
                write!(f, "invalid .lzx header flags {value:#04x}")
            }
            Self::InvalidReserved { offset } => {
                write!(f, "nonzero reserved .lzx field at {offset:#x}")
            }
            Self::InvalidSectionCount { value } => write!(f, "invalid .lzx section count {value}"),
            Self::InvalidEntrySection { value } => write!(f, "invalid .lzx entry section {value}"),
            Self::InvalidEntryOffset { offset, code_size } => {
                write!(
                    f,
                    "invalid .lzx entry offset {offset:#x} for code size {code_size:#x}"
                )
            }
            Self::InvalidPayloadOffset { expected, actual } => {
                write!(
                    f,
                    "invalid .lzx payload offset {actual:#x}, expected {expected:#x}"
                )
            }
            Self::InvalidSectionTableSize { expected, actual } => {
                write!(
                    f,
                    "invalid .lzx section table size {actual}, expected {expected}"
                )
            }
            Self::InvalidSectionKind { value } => write!(f, "invalid .lzx section kind {value}"),
            Self::InvalidSectionOrder {
                index,
                expected,
                actual,
            } => write!(
                f,
                "invalid .lzx section order at {index}: expected {expected:?}, got {actual:?}"
            ),
            Self::InvalidSectionPermissions {
                index,
                value,
                expected,
            } => write!(
                f,
                "invalid .lzx permissions {value:#04x} at section {index}, expected {expected:#04x}"
            ),
            Self::InvalidSectionSize {
                index,
                expected,
                actual,
            } => write!(
                f,
                "invalid .lzx section size {actual} at section {index}, expected {expected}"
            ),
            Self::InvalidSectionAlignment { index, value } => {
                write!(f, "invalid .lzx alignment {value} at section {index}")
            }
            Self::InvalidCodeSection { index } => {
                write!(f, "invalid .lzx code section at index {index}")
            }
            Self::SectionTooLarge {
                index,
                length,
                maximum,
            } => write!(f, ".lzx section {index} length {length} exceeds {maximum}"),
            Self::SectionRange {
                index,
                offset,
                size,
                file_size,
            } => write!(
                f,
                ".lzx section {index} range {offset:#x}+{size:#x} exceeds file size {file_size:#x}"
            ),
            Self::SectionFileOverlap {
                first,
                second,
                first_start,
                first_end,
                second_start,
                second_end,
            } => write!(
                f,
                ".lzx file sections {first} and {second} overlap: {first_start:#x}..{first_end:#x} and {second_start:#x}..{second_end:#x}"
            ),
            Self::SectionVirtualOverlap {
                first,
                second,
                first_start,
                first_end,
                second_start,
                second_end,
            } => write!(
                f,
                ".lzx virtual sections {first} and {second} overlap: {first_start:#x}..{first_end:#x} and {second_start:#x}..{second_end:#x}"
            ),
            Self::InvalidMemoryRequirement { required, maximum } => {
                write!(
                    f,
                    "invalid .lzx data requirement {required}, maximum {maximum}"
                )
            }
            Self::InvalidStackRequirement { required, expected } => {
                write!(
                    f,
                    "invalid .lzx stack requirement {required}, expected {expected}"
                )
            }
            Self::FileTooLarge { length, maximum } => {
                write!(f, ".lzx file length {length} exceeds {maximum}")
            }
            Self::TrailingBytes { offset, length } => {
                write!(f, ".lzx file has {length} trailing bytes at {offset:#x}")
            }
            Self::DebugBlock { reason } => {
                write!(f, ".lzx debug block did not read: {reason}")
            }
            Self::AddressOverflow => f.write_str(".lzx arithmetic overflowed"),
            Self::MissingCode => f.write_str(".lzx image has no code section"),
            Self::Allocation(source) => write!(f, ".lzx allocation failed: {source}"),
            Self::Program(source) => write!(f, ".lzx program image failed: {source}"),
            Self::Process(source) => write!(f, ".lzx process construction failed: {source}"),
            Self::Memory(source) => write!(f, ".lzx memory load failed: {source}"),
        }
    }
}

impl Error for LzxError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Program(source) => Some(source),
            Self::Process(source) => Some(source),
            Self::Memory(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
struct RawSection {
    kind: LzxSectionKind,
    permissions: u8,
    virtual_offset: u64,
    virtual_size: u64,
    file_offset: u64,
    file_size: u64,
    alignment: u64,
}

#[derive(Clone, Copy)]
struct SectionSpec {
    kind: LzxSectionKind,
    permissions: u8,
    virtual_offset: u64,
    virtual_size: u64,
    file_size: u64,
    alignment: u64,
}

fn validate_semantics(
    architecture: LzxArchitecture,
    entry_section: u16,
    entry_offset: u64,
    required_data: u64,
    required_stack: u64,
    specs: &[SectionSpec],
) -> Result<(), LzxError> {
    if specs.is_empty() || specs.len() > LZX_MAX_SECTIONS {
        return Err(LzxError::InvalidSectionCount {
            value: u16::try_from(specs.len()).unwrap_or(u16::MAX),
        });
    }
    if entry_section != 0 {
        return Err(LzxError::InvalidEntrySection {
            value: entry_section,
        });
    }
    if required_stack != USER_STACK_LENGTH {
        return Err(LzxError::InvalidStackRequirement {
            required: required_stack,
            expected: USER_STACK_LENGTH,
        });
    }
    let config = architecture.config();
    let mut data_end = 0;
    for (index, spec) in specs.iter().enumerate() {
        let expected_kind = match index {
            0 => LzxSectionKind::Code,
            1 => LzxSectionKind::Data,
            2 => LzxSectionKind::Bss,
            _ => {
                return Err(LzxError::InvalidSectionCount {
                    value: u16::try_from(index).unwrap_or(u16::MAX),
                });
            }
        };
        if spec.kind != expected_kind {
            return Err(LzxError::InvalidSectionOrder {
                index,
                expected: expected_kind,
                actual: spec.kind,
            });
        }
        let expected_permissions = match spec.kind {
            LzxSectionKind::Code => LZX_CODE_PERMISSIONS,
            LzxSectionKind::Data | LzxSectionKind::Bss => LZX_DATA_PERMISSIONS,
        };
        if spec.permissions != expected_permissions {
            return Err(LzxError::InvalidSectionPermissions {
                index,
                value: spec.permissions,
                expected: expected_permissions,
            });
        }
        if !valid_alignment(spec.alignment) {
            return Err(LzxError::InvalidSectionAlignment {
                index,
                value: spec.alignment,
            });
        }
        let end = spec
            .virtual_offset
            .checked_add(spec.virtual_size)
            .ok_or(LzxError::AddressOverflow)?;
        match spec.kind {
            LzxSectionKind::Code => {
                if spec.virtual_offset != 0
                    || spec.alignment != u64::from(config.instruction_alignment())
                {
                    return Err(LzxError::InvalidCodeSection { index });
                }
                if spec.file_size == 0 || spec.file_size != spec.virtual_size {
                    return Err(LzxError::InvalidSectionSize {
                        index,
                        expected: spec.file_size,
                        actual: spec.virtual_size,
                    });
                }
                if spec.virtual_size > USER_CODE_LENGTH {
                    return Err(LzxError::SectionTooLarge {
                        index,
                        length: spec.virtual_size,
                        maximum: USER_CODE_LENGTH,
                    });
                }
                let entry_end = entry_offset
                    .checked_add(8)
                    .ok_or(LzxError::AddressOverflow)?;
                if !entry_offset.is_multiple_of(u64::from(config.instruction_alignment()))
                    || entry_end > spec.virtual_size
                {
                    return Err(LzxError::InvalidEntryOffset {
                        offset: entry_offset,
                        code_size: spec.virtual_size,
                    });
                }
            }
            LzxSectionKind::Data => {
                if spec.file_size != spec.virtual_size {
                    return Err(LzxError::InvalidSectionSize {
                        index,
                        expected: spec.file_size,
                        actual: spec.virtual_size,
                    });
                }
                if !aligned_user_data(spec.virtual_offset, spec.alignment)? {
                    return Err(LzxError::InvalidSectionAlignment {
                        index,
                        value: spec.alignment,
                    });
                }
                data_end = data_end.max(end);
            }
            LzxSectionKind::Bss => {
                if spec.virtual_size == 0 || spec.file_size != 0 {
                    return Err(LzxError::InvalidSectionSize {
                        index,
                        expected: 0,
                        actual: spec.virtual_size,
                    });
                }
                if !aligned_user_data(spec.virtual_offset, spec.alignment)? {
                    return Err(LzxError::InvalidSectionAlignment {
                        index,
                        value: spec.alignment,
                    });
                }
                data_end = data_end.max(end);
            }
        }
    }
    for (first, spec) in specs.iter().enumerate() {
        if !matches!(spec.kind, LzxSectionKind::Data | LzxSectionKind::Bss) {
            continue;
        }
        let first_end = spec
            .virtual_offset
            .checked_add(spec.virtual_size)
            .ok_or(LzxError::AddressOverflow)?;
        for (second, other) in specs.iter().enumerate().skip(first + 1) {
            if !matches!(other.kind, LzxSectionKind::Data | LzxSectionKind::Bss) {
                continue;
            }
            let second_end = other
                .virtual_offset
                .checked_add(other.virtual_size)
                .ok_or(LzxError::AddressOverflow)?;
            if spec.virtual_offset < second_end && other.virtual_offset < first_end {
                return Err(LzxError::SectionVirtualOverlap {
                    first,
                    second,
                    first_start: spec.virtual_offset,
                    first_end,
                    second_start: other.virtual_offset,
                    second_end,
                });
            }
        }
    }
    if data_end > USER_DATA_LENGTH || required_data > USER_DATA_LENGTH {
        return Err(LzxError::InvalidMemoryRequirement {
            required: required_data.max(data_end),
            maximum: USER_DATA_LENGTH,
        });
    }
    if required_data < data_end {
        return Err(LzxError::InvalidMemoryRequirement {
            required: data_end,
            maximum: USER_DATA_LENGTH,
        });
    }
    Ok(())
}

fn parse(bytes: &[u8]) -> Result<LzxImage, LzxError> {
    let input_size = u64::try_from(bytes.len()).map_err(|_| LzxError::FileTooLarge {
        length: u64::MAX,
        maximum: LZX_MAX_FILE_SIZE,
    })?;
    if input_size > LZX_MAX_FILE_SIZE {
        return Err(LzxError::FileTooLarge {
            length: input_size,
            maximum: LZX_MAX_FILE_SIZE,
        });
    }
    let mut cursor = Cursor::new(bytes);
    if cursor.read_exact(8)? != LZX_MAGIC {
        return Err(LzxError::InvalidMagic);
    }
    let version = cursor.read_u16()?;
    if version != LZX_FORMAT_VERSION {
        return Err(LzxError::UnsupportedFormat { version });
    }
    let header_size = cursor.read_u16()?;
    if usize::from(header_size) != LZX_HEADER_SIZE {
        return Err(LzxError::InvalidHeaderSize {
            expected: LZX_HEADER_SIZE as u16,
            actual: header_size,
        });
    }
    let architecture = match cursor.read_u8()? {
        1 => LzxArchitecture::Lz32,
        2 => LzxArchitecture::Lz64,
        value => return Err(LzxError::UnsupportedArchitecture { value }),
    };
    let flags = cursor.read_u8()?;
    if flags != 0 {
        return Err(LzxError::InvalidHeaderFlags { value: flags });
    }
    let isa_version = cursor.read_u16()?;
    if isa_version != LZX_ISA_VERSION {
        return Err(LzxError::UnsupportedIsaVersion { value: isa_version });
    }
    let abi_version = cursor.read_u16()?;
    if abi_version != ABI_VERSION {
        return Err(LzxError::UnsupportedAbiVersion { value: abi_version });
    }
    let section_count = cursor.read_u16()?;
    if section_count == 0 || usize::from(section_count) > LZX_MAX_SECTIONS {
        return Err(LzxError::InvalidSectionCount {
            value: section_count,
        });
    }
    let entry_section = cursor.read_u16()?;
    if entry_section != 0 {
        return Err(LzxError::InvalidEntrySection {
            value: entry_section,
        });
    }
    let entry_offset = u64::from(cursor.read_u32()?);
    if cursor.read_u16()? != 0 {
        return Err(LzxError::InvalidReserved { offset: 26 });
    }
    let required_data = cursor.read_u64()?;
    let required_stack = cursor.read_u64()?;
    let table_size = cursor.read_u32()?;
    let payload_offset = cursor.read_u64()?;
    // Bytes 56..64 of the header were padding and now say where the debug block
    // went. A zero length means the image carries none, which is the normal case
    // for a program built without debug information and not an error.
    let debug_offset = u64::from(cursor.read_u32()?);
    let debug_length = u64::from(cursor.read_u32()?);
    let expected_table_size = u32::try_from(usize::from(section_count) * LZX_SECTION_ENTRY_SIZE)
        .map_err(|_| LzxError::AddressOverflow)?;
    if table_size != expected_table_size {
        return Err(LzxError::InvalidSectionTableSize {
            expected: expected_table_size,
            actual: table_size,
        });
    }
    let expected_payload = align_up(LZX_HEADER_SIZE as u64 + u64::from(expected_table_size), 8)
        .ok_or(LzxError::AddressOverflow)?;
    if payload_offset != expected_payload {
        return Err(LzxError::InvalidPayloadOffset {
            expected: expected_payload,
            actual: payload_offset,
        });
    }
    if payload_offset > input_size {
        return Err(LzxError::Truncated {
            offset: payload_offset,
            needed: 0,
            available: bytes.len(),
        });
    }
    let mut raw_sections: [Option<RawSection>; LZX_MAX_SECTIONS] = [None; LZX_MAX_SECTIONS];
    for (index, slot) in raw_sections
        .iter_mut()
        .take(usize::from(section_count))
        .enumerate()
    {
        let kind_value = cursor.read_u8()?;
        let kind = LzxSectionKind::try_from(kind_value)
            .map_err(|_| LzxError::InvalidSectionKind { value: kind_value })?;
        let permissions = cursor.read_u8()?;
        if cursor.read_u16()? != 0 {
            return Err(LzxError::InvalidReserved {
                offset: LZX_HEADER_SIZE as u64 + (index * LZX_SECTION_ENTRY_SIZE + 2) as u64,
            });
        }
        let virtual_offset = cursor.read_u64()?;
        let virtual_size = cursor.read_u64()?;
        let file_offset = cursor.read_u64()?;
        let section_file_size = cursor.read_u64()?;
        let alignment = cursor.read_u64()?;
        if cursor.read_u32()? != 0 {
            return Err(LzxError::InvalidReserved {
                offset: LZX_HEADER_SIZE as u64 + (index * LZX_SECTION_ENTRY_SIZE + 44) as u64,
            });
        }
        let section_end = file_offset
            .checked_add(section_file_size)
            .ok_or(LzxError::AddressOverflow)?;
        if file_offset < payload_offset || section_end > input_size {
            return Err(LzxError::SectionRange {
                index,
                offset: file_offset,
                size: section_file_size,
                file_size: input_size,
            });
        }
        *slot = Some(RawSection {
            kind,
            permissions,
            virtual_offset,
            virtual_size,
            file_offset,
            file_size: section_file_size,
            alignment,
        });
    }
    for (first, first_slot) in raw_sections
        .iter()
        .take(usize::from(section_count))
        .enumerate()
    {
        let Some(first_section) = *first_slot else {
            continue;
        };
        for (second, second_slot) in raw_sections
            .iter()
            .take(usize::from(section_count))
            .enumerate()
            .skip(first + 1)
        {
            let Some(second_section) = *second_slot else {
                continue;
            };
            let first_end = first_section
                .file_offset
                .checked_add(first_section.file_size)
                .ok_or(LzxError::AddressOverflow)?;
            let second_end = second_section
                .file_offset
                .checked_add(second_section.file_size)
                .ok_or(LzxError::AddressOverflow)?;
            if first_section.file_size != 0
                && second_section.file_size != 0
                && first_section.file_offset < second_end
                && second_section.file_offset < first_end
            {
                return Err(LzxError::SectionFileOverlap {
                    first,
                    second,
                    first_start: first_section.file_offset,
                    first_end,
                    second_start: second_section.file_offset,
                    second_end,
                });
            }
        }
    }
    let mut specs = [SectionSpec {
        kind: LzxSectionKind::Code,
        permissions: 0,
        virtual_offset: 0,
        virtual_size: 0,
        file_size: 0,
        alignment: 0,
    }; LZX_MAX_SECTIONS];
    let mut max_file_end = payload_offset;
    for (index, raw_slot) in raw_sections
        .iter()
        .take(usize::from(section_count))
        .enumerate()
    {
        let raw = (*raw_slot).ok_or(LzxError::InvalidSectionCount {
            value: section_count,
        })?;
        specs[index] = SectionSpec {
            kind: raw.kind,
            permissions: raw.permissions,
            virtual_offset: raw.virtual_offset,
            virtual_size: raw.virtual_size,
            file_size: raw.file_size,
            alignment: raw.alignment,
        };
        let section_end = raw
            .file_offset
            .checked_add(raw.file_size)
            .ok_or(LzxError::AddressOverflow)?;
        max_file_end = max_file_end.max(section_end);
    }
    // The debug block sits after the sections, so the file ends after it.
    let debug_end = if debug_length == 0 {
        0
    } else {
        let end = debug_offset
            .checked_add(debug_length)
            .ok_or(LzxError::AddressOverflow)?;
        if debug_offset < max_file_end || end > input_size {
            return Err(LzxError::SectionRange {
                index: usize::MAX,
                offset: debug_offset,
                size: debug_length,
                file_size: input_size,
            });
        }
        end
    };
    if max_file_end.max(debug_end) != input_size {
        let end = max_file_end.max(debug_end);
        return Err(LzxError::TrailingBytes {
            offset: end,
            length: input_size - end,
        });
    }
    let debug = if debug_length == 0 {
        None
    } else {
        let from = usize::try_from(debug_offset).map_err(|_| LzxError::AddressOverflow)?;
        let length = usize::try_from(debug_length).map_err(|_| LzxError::AddressOverflow)?;
        let to = from.checked_add(length).ok_or(LzxError::AddressOverflow)?;
        let block = bytes.get(from..to).ok_or(LzxError::Truncated {
            offset: from as u64,
            needed: length,
            available: bytes.len().saturating_sub(from),
        })?;
        Some(
            crate::debug::DebugBlock::decode(block).map_err(|error| LzxError::DebugBlock {
                reason: format!("{error:?}"),
            })?,
        )
    };
    validate_semantics(
        architecture,
        entry_section,
        entry_offset,
        required_data,
        required_stack,
        &specs[..usize::from(section_count)],
    )?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(usize::from(section_count))
        .map_err(LzxError::Allocation)?;
    for (index, raw_slot) in raw_sections
        .iter()
        .take(usize::from(section_count))
        .enumerate()
    {
        let raw = (*raw_slot).ok_or(LzxError::InvalidSectionCount {
            value: section_count,
        })?;
        let start = usize::try_from(raw.file_offset).map_err(|_| LzxError::AddressOverflow)?;
        let size = usize::try_from(raw.file_size).map_err(|_| LzxError::AddressOverflow)?;
        let end = start.checked_add(size).ok_or(LzxError::AddressOverflow)?;
        let mut section_bytes = Vec::new();
        section_bytes
            .try_reserve_exact(size)
            .map_err(LzxError::Allocation)?;
        let section_slice = bytes.get(start..end).ok_or(LzxError::SectionRange {
            index,
            offset: raw.file_offset,
            size: raw.file_size,
            file_size: input_size,
        })?;
        section_bytes.extend_from_slice(section_slice);
        sections.push(LzxSection {
            kind: raw.kind,
            permissions: raw.permissions,
            virtual_offset: raw.virtual_offset,
            virtual_size: raw.virtual_size,
            alignment: raw.alignment,
            bytes: section_bytes,
        });
    }
    LzxImage::with_debug(
        architecture,
        entry_section,
        entry_offset,
        required_data,
        required_stack,
        sections,
        debug,
    )
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_exact(&mut self, length: usize) -> Result<&'a [u8], LzxError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(LzxError::AddressOverflow)?;
        if end > self.bytes.len() {
            return Err(LzxError::Truncated {
                offset: self.position as u64,
                needed: length,
                available: self.bytes.len().saturating_sub(self.position),
            });
        }
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or(LzxError::Truncated {
                offset: self.position as u64,
                needed: length,
                available: self.bytes.len().saturating_sub(self.position),
            })?;
        self.position = end;
        Ok(result)
    }

    fn read_u8(&mut self) -> Result<u8, LzxError> {
        Ok(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Result<u16, LzxError> {
        let bytes = self.read_exact(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, LzxError> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, LzxError> {
        let bytes = self.read_exact(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

fn architecture_byte(architecture: LzxArchitecture) -> u8 {
    architecture as u8
}

fn valid_alignment(alignment: u64) -> bool {
    alignment != 0 && alignment.is_power_of_two()
}

fn aligned_user_data(offset: u64, alignment: u64) -> Result<bool, LzxError> {
    if !valid_alignment(alignment) {
        return Ok(false);
    }
    let address = crate::USER_DATA_START
        .checked_add(offset)
        .ok_or(LzxError::AddressOverflow)?;
    Ok(address.is_multiple_of(alignment))
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return None;
    }
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
