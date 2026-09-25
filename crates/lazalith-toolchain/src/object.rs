use crate::assembler::assemble_named;
use alloc::{
    boxed::Box,
    collections::{BTreeMap, TryReserveError},
    string::String,
    vec::Vec,
};
use core::{error::Error, fmt};
use lazalith_isa::{DecodeError, ISA_VERSION, InstructionError, Opcode, decode, encode};
use lazalith_os::{LzxArchitecture, LzxError, LzxImage, LzxSection, USER_STACK_LENGTH};
use lazalith_os_abi::ABI_VERSION;
use lazalith_types::{ArchitectureConfig, WordWidth};

pub const OBJECT_MAGIC: [u8; 8] = *b"LZOBJ01\0";
pub const OBJECT_FORMAT_VERSION: u16 = 1;
pub const OBJECT_ISA_VERSION: u16 = 1;
pub const OBJECT_HEADER_SIZE: usize = 128;
pub const OBJECT_SECTION_ENTRY_SIZE: usize = 64;
pub const OBJECT_SYMBOL_ENTRY_SIZE: usize = 32;
pub const OBJECT_RELOCATION_ENTRY_SIZE: usize = 32;
pub const OBJECT_DEBUG_SOURCE_ENTRY_SIZE: usize = 16;
pub const OBJECT_DEBUG_MAPPING_ENTRY_SIZE: usize = 24;
pub const OBJECT_MAX_FILE_SIZE: usize = 16 * 1024 * 1024;
pub const OBJECT_MAX_MATERIALIZED_NAME_BYTES: usize = OBJECT_MAX_FILE_SIZE;
const _: () = assert!(OBJECT_ISA_VERSION == ISA_VERSION);
pub const OBJECT_MAX_SECTIONS: usize = 4096;
pub const OBJECT_MAX_SYMBOLS: usize = u32::MAX as usize;
pub const OBJECT_MAX_RELOCATIONS: usize = u32::MAX as usize;
pub const OBJECT_MAX_DEBUG_SOURCES: usize = u32::MAX as usize;
pub const OBJECT_MAX_DEBUG_MAPPINGS: usize = u32::MAX as usize;
pub const OBJECT_NO_ENTRY: u32 = u32::MAX;

#[derive(Debug)]
pub enum ObjectError {
    InvalidMagic,
    UnsupportedFormat(u16),
    InvalidHeaderSize(u16),
    InvalidArchitecture(u8),
    InvalidFlags(u8),
    UnsupportedIsa(u16),
    UnsupportedAbi(u16),
    ReservedField {
        offset: u64,
    },
    InvalidCount {
        table: &'static str,
        value: u64,
    },
    TableOffset {
        table: &'static str,
        value: u64,
        expected: u64,
    },
    InvalidString {
        offset: u32,
    },
    InvalidSection {
        index: usize,
        reason: &'static str,
    },
    DuplicateSection {
        index: usize,
        previous: usize,
    },
    InvalidSymbol {
        index: usize,
        reason: &'static str,
    },
    InvalidRelocation {
        index: usize,
        reason: &'static str,
    },
    InvalidDebug {
        index: usize,
        reason: &'static str,
    },
    InvalidEntry {
        reason: &'static str,
    },
    Truncated {
        offset: usize,
        needed: usize,
        available: usize,
    },
    Decode {
        section: usize,
        offset: u64,
        source: DecodeError,
    },
    Instruction {
        section: usize,
        offset: u64,
        source: InstructionError,
    },
    NonCanonical {
        section: usize,
        offset: u64,
    },
    UnsupportedLink(&'static str),
    Allocation(TryReserveError),
}

impl fmt::Display for ObjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => f.write_str("object magic is invalid"),
            Self::UnsupportedFormat(value) => {
                write!(f, "object format version {value} is unsupported")
            }
            Self::InvalidHeaderSize(value) => write!(f, "object header size {value} is invalid"),
            Self::InvalidArchitecture(value) => write!(f, "object architecture {value} is invalid"),
            Self::InvalidFlags(value) => write!(f, "object flags {value} are invalid"),
            Self::UnsupportedIsa(value) => write!(f, "object ISA version {value} is unsupported"),
            Self::UnsupportedAbi(value) => write!(f, "object ABI version {value} is unsupported"),
            Self::ReservedField { offset } => {
                write!(f, "reserved object field at {offset} is nonzero")
            }
            Self::InvalidCount { table, value } => {
                write!(f, "object {table} count {value} is invalid")
            }
            Self::TableOffset {
                table,
                value,
                expected,
            } => {
                write!(
                    f,
                    "object {table} offset {value} is invalid; expected {expected}"
                )
            }
            Self::InvalidString { offset } => write!(f, "object string offset {offset} is invalid"),
            Self::InvalidSection { index, reason } => {
                write!(f, "object section {index} is invalid: {reason}")
            }
            Self::DuplicateSection { index, previous } => {
                write!(f, "object section {index} duplicates section {previous}")
            }
            Self::InvalidSymbol { index, reason } => {
                write!(f, "object symbol {index} is invalid: {reason}")
            }
            Self::InvalidRelocation { index, reason } => {
                write!(f, "object relocation {index} is invalid: {reason}")
            }
            Self::InvalidDebug { index, reason } => {
                write!(f, "object debug record {index} is invalid: {reason}")
            }
            Self::InvalidEntry { reason } => write!(f, "object entry is invalid: {reason}"),
            Self::Truncated {
                offset,
                needed,
                available,
            } => write!(
                f,
                "object is truncated at {offset}: need {needed} bytes, have {available}"
            ),
            Self::Decode {
                section,
                offset,
                source,
            } => {
                write!(
                    f,
                    "object section {section} instruction {offset} cannot be decoded: {source}"
                )
            }
            Self::Instruction {
                section,
                offset,
                source,
            } => {
                write!(
                    f,
                    "object section {section} instruction {offset} cannot be encoded: {source}"
                )
            }
            Self::NonCanonical { section, offset } => {
                write!(
                    f,
                    "object section {section} instruction {offset} is not canonical"
                )
            }
            Self::UnsupportedLink(reason) => write!(f, "object cannot be linked: {reason}"),
            Self::Allocation(source) => write!(f, "object allocation failed: {source}"),
        }
    }
}

impl Error for ObjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Decode { source, .. } => Some(source),
            Self::Instruction { source, .. } => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ToolchainError {
    Assembly(Box<crate::AssemblyError>),
    Object(ObjectError),
    Image(LzxError),
    Allocation(TryReserveError),
}

impl fmt::Display for ToolchainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assembly(source) => write!(f, "assembly failed: {source}"),
            Self::Object(source) => write!(f, "object validation failed: {source}"),
            Self::Image(source) => write!(f, "executable construction failed: {source}"),
            Self::Allocation(source) => write!(f, "toolchain allocation failed: {source}"),
        }
    }
}

impl Error for ToolchainError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Assembly(source) => Some(source.as_ref()),
            Self::Object(source) => Some(source),
            Self::Image(source) => Some(source),
            Self::Allocation(source) => Some(source),
        }
    }
}

impl From<crate::AssemblyError> for ToolchainError {
    fn from(source: crate::AssemblyError) -> Self {
        Self::Assembly(Box::new(source))
    }
}

impl From<ObjectError> for ToolchainError {
    fn from(source: ObjectError) -> Self {
        Self::Object(source)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SectionIndex(u16);
impl SectionIndex {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u16 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SymbolIndex(u32);
impl SymbolIndex {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DebugSourceIndex(u32);
impl DebugSourceIndex {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionKind {
    Text = 1,
    ReadOnlyData = 2,
    Data = 3,
    Bss = 4,
}
impl TryFrom<u8> for SectionKind {
    type Error = ObjectError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Text),
            2 => Ok(Self::ReadOnlyData),
            3 => Ok(Self::Data),
            4 => Ok(Self::Bss),
            _ => Err(ObjectError::InvalidSection {
                index: 0,
                reason: "unknown section kind",
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    Undefined = 0,
    Absolute = 1,
    Section = 2,
}
impl TryFrom<u8> for SymbolKind {
    type Error = ObjectError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Undefined),
            1 => Ok(Self::Absolute),
            2 => Ok(Self::Section),
            _ => Err(ObjectError::InvalidSymbol {
                index: 0,
                reason: "unknown symbol kind",
            }),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolBinding {
    Local = 0,
    Global = 1,
}
impl TryFrom<u8> for SymbolBinding {
    type Error = ObjectError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Local),
            1 => Ok(Self::Global),
            _ => Err(ObjectError::InvalidSymbol {
                index: 0,
                reason: "unknown symbol binding",
            }),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelocationKind {
    AbsoluteWord32 = 1,
    AbsoluteWord64 = 2,
    PcRelativeWord32 = 3,
    PcRelativeBranch = 4,
    LiImmediate = 5,
    MemoryDisplacement32 = 6,
}
impl TryFrom<u8> for RelocationKind {
    type Error = ObjectError;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::AbsoluteWord32),
            2 => Ok(Self::AbsoluteWord64),
            3 => Ok(Self::PcRelativeWord32),
            4 => Ok(Self::PcRelativeBranch),
            5 => Ok(Self::LiImmediate),
            6 => Ok(Self::MemoryDisplacement32),
            _ => Err(ObjectError::InvalidRelocation {
                index: 0,
                reason: "unknown relocation kind",
            }),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    name: String,
    kind: SectionKind,
    alignment: u64,
    size: u64,
    bytes: Vec<u8>,
}
impl Section {
    pub fn text(
        name: impl Into<String>,
        config: ArchitectureConfig,
        bytes: &[u8],
    ) -> Result<Self, ObjectError> {
        let mut stored = Vec::new();
        stored
            .try_reserve_exact(bytes.len())
            .map_err(ObjectError::Allocation)?;
        stored.extend_from_slice(bytes);
        let section = Self {
            name: name.into(),
            kind: SectionKind::Text,
            alignment: u64::from(config.instruction_alignment()),
            size: bytes.len() as u64,
            bytes: stored,
        };
        section.validate(config, 0)?;
        Ok(section)
    }
    pub fn read_only_data(
        name: impl Into<String>,
        alignment: u64,
        bytes: &[u8],
    ) -> Result<Self, ObjectError> {
        Self::file_backed(name, SectionKind::ReadOnlyData, alignment, bytes)
    }
    pub fn data(
        name: impl Into<String>,
        alignment: u64,
        bytes: &[u8],
    ) -> Result<Self, ObjectError> {
        Self::file_backed(name, SectionKind::Data, alignment, bytes)
    }
    pub fn bss(name: impl Into<String>, alignment: u64, size: u64) -> Result<Self, ObjectError> {
        let section = Self {
            name: name.into(),
            kind: SectionKind::Bss,
            alignment,
            size,
            bytes: Vec::new(),
        };
        if size == 0 {
            return Err(ObjectError::InvalidSection {
                index: 0,
                reason: "BSS size must be nonzero",
            });
        }
        Ok(section)
    }
    fn file_backed(
        name: impl Into<String>,
        kind: SectionKind,
        alignment: u64,
        bytes: &[u8],
    ) -> Result<Self, ObjectError> {
        let mut stored = Vec::new();
        stored
            .try_reserve_exact(bytes.len())
            .map_err(ObjectError::Allocation)?;
        stored.extend_from_slice(bytes);
        Ok(Self {
            name: name.into(),
            kind,
            alignment,
            size: bytes.len() as u64,
            bytes: stored,
        })
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn kind(&self) -> SectionKind {
        self.kind
    }
    pub const fn alignment(&self) -> u64 {
        self.alignment
    }
    pub const fn size(&self) -> u64 {
        self.size
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn file_size(&self) -> u64 {
        self.bytes.len() as u64
    }
    fn validate(&self, config: ArchitectureConfig, index: usize) -> Result<(), ObjectError> {
        if self.name.is_empty() || self.name.as_bytes().contains(&0) {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "name is empty or contains NUL",
            });
        }
        if self.alignment == 0 || !self.alignment.is_power_of_two() {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "alignment is not a nonzero power of two",
            });
        }
        match self.kind {
            SectionKind::Text => {
                if self.size == 0
                    || self.size != self.file_size()
                    || !self.size.is_multiple_of(8)
                    || self.alignment < u64::from(config.instruction_alignment())
                {
                    return Err(ObjectError::InvalidSection {
                        index,
                        reason: "text size or alignment is invalid",
                    });
                }
                for (offset, bytes) in self.bytes.chunks(8).enumerate() {
                    let offset = (offset as u64) * 8;
                    let instruction =
                        decode(config, bytes).map_err(|source| ObjectError::Decode {
                            section: index,
                            offset,
                            source,
                        })?;
                    let encoded = encode(config, &instruction).map_err(|source| {
                        ObjectError::Instruction {
                            section: index,
                            offset,
                            source,
                        }
                    })?;
                    if encoded != bytes {
                        return Err(ObjectError::NonCanonical {
                            section: index,
                            offset,
                        });
                    }
                }
            }
            SectionKind::ReadOnlyData | SectionKind::Data => {
                if self.size != self.file_size() {
                    return Err(ObjectError::InvalidSection {
                        index,
                        reason: "file-backed size does not match payload",
                    });
                }
            }
            SectionKind::Bss => {
                if self.size == 0 || !self.bytes.is_empty() {
                    return Err(ObjectError::InvalidSection {
                        index,
                        reason: "BSS must have size and no bytes",
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    name: String,
    kind: SymbolKind,
    binding: SymbolBinding,
    section: Option<SectionIndex>,
    value: u64,
    size: u64,
}
impl Symbol {
    pub fn undefined(name: impl Into<String>, binding: SymbolBinding) -> Self {
        Self {
            name: name.into(),
            kind: SymbolKind::Undefined,
            binding,
            section: None,
            value: 0,
            size: 0,
        }
    }
    pub fn absolute(name: impl Into<String>, binding: SymbolBinding, value: u64) -> Self {
        Self {
            name: name.into(),
            kind: SymbolKind::Absolute,
            binding,
            section: None,
            value,
            size: 0,
        }
    }
    pub fn section_defined(
        name: impl Into<String>,
        binding: SymbolBinding,
        section: SectionIndex,
        value: u64,
        size: u64,
    ) -> Self {
        Self {
            name: name.into(),
            kind: SymbolKind::Section,
            binding,
            section: Some(section),
            value,
            size,
        }
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub const fn kind(&self) -> SymbolKind {
        self.kind
    }
    pub const fn binding(&self) -> SymbolBinding {
        self.binding
    }
    pub const fn section(&self) -> Option<SectionIndex> {
        self.section
    }
    pub const fn value(&self) -> u64 {
        self.value
    }
    pub const fn size(&self) -> u64 {
        self.size
    }
    pub(crate) fn from_wire(
        name: String,
        kind: SymbolKind,
        binding: SymbolBinding,
        section: Option<SectionIndex>,
        value: u64,
        size: u64,
    ) -> Self {
        Self {
            name,
            kind,
            binding,
            section,
            value,
            size,
        }
    }
    fn validate(
        &self,
        index: usize,
        config: ArchitectureConfig,
        sections: &[Section],
    ) -> Result<(), ObjectError> {
        if self.name.is_empty() || self.name.as_bytes().contains(&0) {
            return Err(ObjectError::InvalidSymbol {
                index,
                reason: "name is empty or contains NUL",
            });
        }
        match self.kind {
            SymbolKind::Undefined => {
                if self.binding != SymbolBinding::Global
                    || self.section.is_some()
                    || self.value != 0
                    || self.size != 0
                {
                    return Err(ObjectError::InvalidSymbol {
                        index,
                        reason: "undefined symbol has invalid fields",
                    });
                }
            }
            SymbolKind::Absolute => {
                if self.section.is_some()
                    || (config.word_width() == WordWidth::W32 && self.value > u64::from(u32::MAX))
                {
                    return Err(ObjectError::InvalidSymbol {
                        index,
                        reason: "absolute symbol is invalid for target",
                    });
                }
            }
            SymbolKind::Section => {
                let section = self.section.ok_or(ObjectError::InvalidSymbol {
                    index,
                    reason: "section symbol has no section",
                })?;
                let section =
                    sections
                        .get(section.get() as usize)
                        .ok_or(ObjectError::InvalidSymbol {
                            index,
                            reason: "section index is out of range",
                        })?;
                let end = self
                    .value
                    .checked_add(self.size)
                    .ok_or(ObjectError::InvalidSymbol {
                        index,
                        reason: "symbol range overflows",
                    })?;
                if end > section.size {
                    return Err(ObjectError::InvalidSymbol {
                        index,
                        reason: "symbol exceeds section",
                    });
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Relocation {
    symbol: SymbolIndex,
    section: SectionIndex,
    kind: RelocationKind,
    offset: u64,
    addend: i64,
}
impl Relocation {
    pub const fn new(
        symbol: SymbolIndex,
        section: SectionIndex,
        kind: RelocationKind,
        offset: u64,
        addend: i64,
    ) -> Self {
        Self {
            symbol,
            section,
            kind,
            offset,
            addend,
        }
    }
    pub const fn symbol(&self) -> SymbolIndex {
        self.symbol
    }
    pub const fn section(&self) -> SectionIndex {
        self.section
    }
    pub const fn kind(&self) -> RelocationKind {
        self.kind
    }
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    pub const fn addend(&self) -> i64 {
        self.addend
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugSource {
    path: String,
    length: u32,
}
impl DebugSource {
    pub fn new(path: impl Into<String>, length: u32) -> Self {
        Self {
            path: path.into(),
            length,
        }
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub const fn length(&self) -> u32 {
        self.length
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodeMapping {
    section: SectionIndex,
    offset: u64,
    source: DebugSourceIndex,
    source_offset: u32,
    source_length: u32,
}
impl CodeMapping {
    pub const fn new(
        section: SectionIndex,
        offset: u64,
        source: DebugSourceIndex,
        source_offset: u32,
        source_length: u32,
    ) -> Self {
        Self {
            section,
            offset,
            source,
            source_offset,
            source_length,
        }
    }
    pub const fn section(&self) -> SectionIndex {
        self.section
    }
    pub const fn offset(&self) -> u64 {
        self.offset
    }
    pub const fn source(&self) -> DebugSourceIndex {
        self.source
    }
    pub const fn source_offset(&self) -> u32 {
        self.source_offset
    }
    pub const fn source_length(&self) -> u32 {
        self.source_length
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObjectTarget {
    architecture: ArchitectureConfig,
    isa_version: u16,
    abi_version: u16,
}
impl ObjectTarget {
    pub const fn current(architecture: ArchitectureConfig) -> Self {
        Self {
            architecture,
            isa_version: OBJECT_ISA_VERSION,
            abi_version: ABI_VERSION,
        }
    }
    pub fn from_wire(
        architecture: u8,
        isa_version: u16,
        abi_version: u16,
    ) -> Result<Self, ObjectError> {
        let architecture = match architecture {
            1 => ArchitectureConfig::lz32(),
            2 => ArchitectureConfig::lz64(),
            value => return Err(ObjectError::InvalidArchitecture(value)),
        };
        if isa_version != OBJECT_ISA_VERSION {
            return Err(ObjectError::UnsupportedIsa(isa_version));
        }
        if abi_version != ABI_VERSION {
            return Err(ObjectError::UnsupportedAbi(abi_version));
        }
        Ok(Self {
            architecture,
            isa_version,
            abi_version,
        })
    }
    pub const fn architecture(self) -> ArchitectureConfig {
        self.architecture
    }
    pub const fn isa_version(self) -> u16 {
        self.isa_version
    }
    pub const fn abi_version(self) -> u16 {
        self.abi_version
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectFile {
    target: ObjectTarget,
    entry: Option<SymbolIndex>,
    sections: Vec<Section>,
    symbols: Vec<Symbol>,
    relocations: Vec<Relocation>,
    debug_sources: Vec<DebugSource>,
    debug_mappings: Vec<CodeMapping>,
}

impl ObjectFile {
    pub fn new(
        architecture: LzxArchitecture,
        code: Vec<u8>,
        entry_offset: u64,
    ) -> Result<Self, ToolchainError> {
        let config = architecture.config();
        let section = Section::text("text", config, &code).map_err(ToolchainError::Object)?;
        let mut builder = ObjectBuilder::new(config);
        let section = builder
            .add_section(section)
            .map_err(ToolchainError::Object)?;
        let symbol = builder
            .add_symbol(Symbol::section_defined(
                "entry",
                SymbolBinding::Local,
                section,
                entry_offset,
                0,
            ))
            .map_err(ToolchainError::Object)?;
        builder.set_entry(symbol).map_err(ToolchainError::Object)?;
        builder.build().map_err(ToolchainError::Object)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ObjectError> {
        crate::lzo::decode(bytes)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, ObjectError> {
        crate::lzo::encode(self)
    }
    pub fn validate(&self) -> Result<(), ObjectError> {
        if self.target.isa_version != OBJECT_ISA_VERSION {
            return Err(ObjectError::UnsupportedIsa(self.target.isa_version));
        }
        if self.target.abi_version != ABI_VERSION {
            return Err(ObjectError::UnsupportedAbi(self.target.abi_version));
        }
        let mut name_bytes = 0usize;
        for name in self
            .sections
            .iter()
            .map(Section::name)
            .chain(self.symbols.iter().map(Symbol::name))
            .chain(self.debug_sources.iter().map(DebugSource::path))
        {
            name_bytes = name_bytes
                .checked_add(name.len())
                .ok_or(ObjectError::InvalidCount {
                    table: "materialized names",
                    value: u64::MAX,
                })?;
            if name_bytes > OBJECT_MAX_MATERIALIZED_NAME_BYTES {
                return Err(ObjectError::InvalidCount {
                    table: "materialized names",
                    value: name_bytes as u64,
                });
            }
        }
        if self.sections.is_empty() || self.sections.len() > OBJECT_MAX_SECTIONS {
            return Err(ObjectError::InvalidCount {
                table: "section",
                value: self.sections.len() as u64,
            });
        }
        for (index, section) in self.sections.iter().enumerate() {
            section.validate(self.target.architecture, index)?;
        }
        let mut section_names = BTreeMap::new();
        for (index, section) in self.sections.iter().enumerate() {
            if let Some(previous) = section_names.insert(section.name(), index) {
                return Err(ObjectError::DuplicateSection { index, previous });
            }
        }
        for (index, symbol) in self.symbols.iter().enumerate() {
            symbol.validate(index, self.target.architecture, &self.sections)?;
        }
        let mut global_names = BTreeMap::new();
        for (index, symbol) in self.symbols.iter().enumerate() {
            if symbol.binding() == SymbolBinding::Global
                && global_names.insert(symbol.name(), index).is_some()
            {
                return Err(ObjectError::InvalidSymbol {
                    index,
                    reason: "duplicate global name",
                });
            }
        }
        if let Some(entry) = self.entry {
            let symbol =
                self.symbols
                    .get(entry.get() as usize)
                    .ok_or(ObjectError::InvalidEntry {
                        reason: "entry index is out of range",
                    })?;
            let section_index = symbol.section().ok_or(ObjectError::InvalidEntry {
                reason: "entry is not section-defined",
            })?;
            let section = self.sections.get(section_index.get() as usize).ok_or(
                ObjectError::InvalidEntry {
                    reason: "entry section is out of range",
                },
            )?;
            if section.kind() != SectionKind::Text
                || symbol.value() % 4 != 0
                || symbol
                    .value()
                    .checked_add(8)
                    .is_none_or(|end| end > section.size)
            {
                return Err(ObjectError::InvalidEntry {
                    reason: "entry is not a complete text instruction",
                });
            }
        }
        for (index, relocation) in self.relocations.iter().enumerate() {
            let symbol = self.symbols.get(relocation.symbol().get() as usize).ok_or(
                ObjectError::InvalidRelocation {
                    index,
                    reason: "symbol index is out of range",
                },
            )?;
            let section = self
                .sections
                .get(relocation.section().get() as usize)
                .ok_or(ObjectError::InvalidRelocation {
                    index,
                    reason: "section index is out of range",
                })?;
            if section.kind() == SectionKind::Bss {
                return Err(ObjectError::InvalidRelocation {
                    index,
                    reason: "BSS cannot be relocated",
                });
            }
            if symbol.kind() == SymbolKind::Undefined && symbol.binding() != SymbolBinding::Global {
                return Err(ObjectError::InvalidRelocation {
                    index,
                    reason: "local undefined symbol",
                });
            }
            if relocation.offset() >= section.size {
                return Err(ObjectError::InvalidRelocation {
                    index,
                    reason: "offset is out of range",
                });
            }
            if matches!(
                relocation.kind(),
                RelocationKind::PcRelativeBranch
                    | RelocationKind::LiImmediate
                    | RelocationKind::MemoryDisplacement32
            ) {
                if section.kind() != SectionKind::Text
                    || relocation.offset() % 8 != 0
                    || relocation
                        .offset()
                        .checked_add(8)
                        .is_none_or(|end| end > section.size)
                {
                    return Err(ObjectError::InvalidRelocation {
                        index,
                        reason: "code relocation is not an instruction",
                    });
                }
                let start = usize::try_from(relocation.offset()).map_err(|_| {
                    ObjectError::InvalidRelocation {
                        index,
                        reason: "offset does not fit the host index domain",
                    }
                })?;
                let bytes = section.bytes().get(start..start + 8).ok_or(
                    ObjectError::InvalidRelocation {
                        index,
                        reason: "instruction bytes are out of range",
                    },
                )?;
                let instruction = decode(self.target.architecture, bytes).map_err(|source| {
                    ObjectError::Decode {
                        section: relocation.section().get() as usize,
                        offset: relocation.offset(),
                        source,
                    }
                })?;
                let opcode = instruction.opcode();
                let valid = match relocation.kind() {
                    RelocationKind::PcRelativeBranch => {
                        matches!(opcode, Opcode::Br | Opcode::Call)
                    }
                    RelocationKind::LiImmediate => opcode == Opcode::Li,
                    RelocationKind::MemoryDisplacement32 => {
                        matches!(opcode, Opcode::Ldz | Opcode::Lds | Opcode::St)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(ObjectError::InvalidRelocation {
                        index,
                        reason: "relocation kind does not match target opcode",
                    });
                }
            } else {
                let width: u64 = match relocation.kind() {
                    RelocationKind::AbsoluteWord32 | RelocationKind::PcRelativeWord32 => 4,
                    RelocationKind::AbsoluteWord64 => 8,
                    _ => {
                        return Err(ObjectError::InvalidRelocation {
                            index,
                            reason: "unknown relocation kind",
                        });
                    }
                };
                if section.kind() == SectionKind::Text
                    || relocation.kind() == RelocationKind::PcRelativeWord32
                        && section.kind() == SectionKind::Bss
                    || relocation.offset() % width != 0
                    || relocation
                        .offset()
                        .checked_add(width)
                        .is_none_or(|end| end > section.size())
                    || relocation.kind() == RelocationKind::AbsoluteWord64
                        && self.target.architecture.word_width() != WordWidth::W64
                {
                    return Err(ObjectError::InvalidRelocation {
                        index,
                        reason: "data relocation has an invalid target",
                    });
                }
            }
            if index > 0 {
                let previous = &self.relocations[index - 1];
                if previous.section() == relocation.section() {
                    let previous_width = match previous.kind() {
                        RelocationKind::PcRelativeBranch
                        | RelocationKind::LiImmediate
                        | RelocationKind::MemoryDisplacement32 => 8,
                        RelocationKind::AbsoluteWord32 | RelocationKind::PcRelativeWord32 => 4,
                        RelocationKind::AbsoluteWord64 => 8,
                    };
                    let previous_end = previous.offset().saturating_add(previous_width);
                    if previous_end > relocation.offset() {
                        return Err(ObjectError::InvalidRelocation {
                            index,
                            reason: "relocation patch fields overlap",
                        });
                    }
                }
                if previous.section() > relocation.section()
                    || (previous.section() == relocation.section()
                        && previous.offset() >= relocation.offset())
                {
                    return Err(ObjectError::InvalidRelocation {
                        index,
                        reason: "relocations are not strictly ordered",
                    });
                }
            }
        }
        for (index, source) in self.debug_sources.iter().enumerate() {
            if source.path().is_empty() || source.path().as_bytes().contains(&0) {
                return Err(ObjectError::InvalidDebug {
                    index,
                    reason: "source path is empty or contains NUL",
                });
            }
        }
        for (index, mapping) in self.debug_mappings.iter().enumerate() {
            let section = self.sections.get(mapping.section().get() as usize).ok_or(
                ObjectError::InvalidDebug {
                    index,
                    reason: "section index is out of range",
                },
            )?;
            let source = self
                .debug_sources
                .get(mapping.source().get() as usize)
                .ok_or(ObjectError::InvalidDebug {
                    index,
                    reason: "source index is out of range",
                })?;
            if section.kind() != SectionKind::Text
                || mapping.offset() % 8 != 0
                || mapping
                    .offset()
                    .checked_add(8)
                    .is_none_or(|end| end > section.size)
            {
                return Err(ObjectError::InvalidDebug {
                    index,
                    reason: "mapping is not a complete text instruction",
                });
            }
            if u64::from(mapping.source_offset())
                .checked_add(u64::from(mapping.source_length()))
                .is_none_or(|end| end > u64::from(source.length()))
            {
                return Err(ObjectError::InvalidDebug {
                    index,
                    reason: "source span is out of range",
                });
            }
            if index > 0 {
                let previous = self.debug_mappings[index - 1];
                if previous.section() > mapping.section()
                    || (previous.section() == mapping.section()
                        && previous.offset() >= mapping.offset())
                {
                    return Err(ObjectError::InvalidDebug {
                        index,
                        reason: "mappings are not strictly ordered",
                    });
                }
            }
        }
        Ok(())
    }
    pub const fn target(&self) -> ObjectTarget {
        self.target
    }
    pub const fn config(&self) -> ArchitectureConfig {
        self.target.architecture
    }
    pub const fn architecture(&self) -> LzxArchitecture {
        LzxArchitecture::from_config(self.target.architecture)
    }
    pub const fn isa_version(&self) -> u16 {
        self.target.isa_version
    }
    pub const fn abi_version(&self) -> u16 {
        self.target.abi_version
    }
    pub const fn entry(&self) -> Option<SymbolIndex> {
        self.entry
    }
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }
    pub fn symbols(&self) -> &[Symbol] {
        &self.symbols
    }
    pub fn relocations(&self) -> &[Relocation] {
        &self.relocations
    }
    pub fn debug_sources(&self) -> &[DebugSource] {
        &self.debug_sources
    }
    pub fn debug_mappings(&self) -> &[CodeMapping] {
        &self.debug_mappings
    }
    pub fn code(&self) -> &[u8] {
        self.sections
            .iter()
            .find(|section| section.kind() == SectionKind::Text)
            .map_or(&[], Section::bytes)
    }
    pub fn entry_offset(&self) -> u64 {
        self.entry
            .and_then(|index| self.symbols.get(index.get() as usize))
            .map_or(0, Symbol::value)
    }
    pub(crate) fn from_wire(
        target: ObjectTarget,
        entry: Option<SymbolIndex>,
        sections: Vec<Section>,
        symbols: Vec<Symbol>,
        relocations: Vec<Relocation>,
        debug_sources: Vec<DebugSource>,
        debug_mappings: Vec<CodeMapping>,
    ) -> Self {
        Self {
            target,
            entry,
            sections,
            symbols,
            relocations,
            debug_sources,
            debug_mappings,
        }
    }
}

pub struct ObjectBuilder {
    target: ObjectTarget,
    entry: Option<SymbolIndex>,
    sections: Vec<Section>,
    symbols: Vec<Symbol>,
    relocations: Vec<Relocation>,
    debug_sources: Vec<DebugSource>,
    debug_mappings: Vec<CodeMapping>,
}
impl ObjectBuilder {
    pub fn new(architecture: ArchitectureConfig) -> Self {
        Self {
            target: ObjectTarget::current(architecture),
            entry: None,
            sections: Vec::new(),
            symbols: Vec::new(),
            relocations: Vec::new(),
            debug_sources: Vec::new(),
            debug_mappings: Vec::new(),
        }
    }
    pub fn add_section(&mut self, section: Section) -> Result<SectionIndex, ObjectError> {
        self.sections
            .try_reserve(1)
            .map_err(ObjectError::Allocation)?;
        let index = SectionIndex::new(u16::try_from(self.sections.len()).map_err(|_| {
            ObjectError::InvalidCount {
                table: "section",
                value: self.sections.len() as u64,
            }
        })?);
        self.sections.push(section);
        Ok(index)
    }
    pub fn add_symbol(&mut self, symbol: Symbol) -> Result<SymbolIndex, ObjectError> {
        self.symbols
            .try_reserve(1)
            .map_err(ObjectError::Allocation)?;
        let index = SymbolIndex::new(u32::try_from(self.symbols.len()).map_err(|_| {
            ObjectError::InvalidCount {
                table: "symbol",
                value: self.symbols.len() as u64,
            }
        })?);
        self.symbols.push(symbol);
        Ok(index)
    }
    pub fn add_relocation(&mut self, relocation: Relocation) -> Result<(), ObjectError> {
        self.relocations
            .try_reserve(1)
            .map_err(ObjectError::Allocation)?;
        self.relocations.push(relocation);
        Ok(())
    }
    pub fn set_entry(&mut self, entry: SymbolIndex) -> Result<(), ObjectError> {
        self.entry = Some(entry);
        Ok(())
    }
    pub fn add_debug_source(
        &mut self,
        source: DebugSource,
    ) -> Result<DebugSourceIndex, ObjectError> {
        self.debug_sources
            .try_reserve(1)
            .map_err(ObjectError::Allocation)?;
        let index =
            DebugSourceIndex::new(u32::try_from(self.debug_sources.len()).map_err(|_| {
                ObjectError::InvalidCount {
                    table: "debug source",
                    value: self.debug_sources.len() as u64,
                }
            })?);
        self.debug_sources.push(source);
        Ok(index)
    }
    pub fn add_debug_mapping(&mut self, mapping: CodeMapping) -> Result<(), ObjectError> {
        self.debug_mappings
            .try_reserve(1)
            .map_err(ObjectError::Allocation)?;
        self.debug_mappings.push(mapping);
        Ok(())
    }
    pub fn build(self) -> Result<ObjectFile, ObjectError> {
        let object = ObjectFile::from_wire(
            self.target,
            self.entry,
            self.sections,
            self.symbols,
            self.relocations,
            self.debug_sources,
            self.debug_mappings,
        );
        object.validate()?;
        Ok(object)
    }
}

pub fn link_object(object: &ObjectFile) -> Result<LzxImage, ToolchainError> {
    object.validate().map_err(ToolchainError::Object)?;
    if object.sections().len() != 1 || object.sections()[0].kind() != SectionKind::Text {
        return Err(ToolchainError::Object(ObjectError::UnsupportedLink(
            "only one text section is supported by the Step 44 bridge",
        )));
    }
    if !object.relocations().is_empty() {
        return Err(ToolchainError::Object(ObjectError::UnsupportedLink(
            "relocations require the Step 48 linker",
        )));
    }
    let section = LzxSection::code(object.code()).map_err(ToolchainError::Image)?;
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(1)
        .map_err(ToolchainError::Allocation)?;
    sections.push(section);
    LzxImage::new(
        object.architecture(),
        0,
        object.entry_offset(),
        0,
        USER_STACK_LENGTH,
        sections,
    )
    .map_err(ToolchainError::Image)
}

pub fn assemble_and_link(source: &str) -> Result<LzxImage, ToolchainError> {
    let object = assemble_named("input.lzs", source)?;
    link_object(&object)
}
