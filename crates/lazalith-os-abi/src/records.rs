use crate::{AbiError, SyscallStatus, syscall::validate_word_value, validate_range};
use core::fmt;
use lazalith_types::{ArchitectureConfig, VirtualAddress};

pub const IO_RESULT_SIZE: usize = 16;
pub const FILE_STAT_SIZE: usize = 16;
pub const DIRECTORY_RECORD_SIZE: usize = 256;
pub const DIRECTORY_NAME_CAPACITY: usize = 252;
pub const MEMORY_ALLOCATION_SIZE: usize = 16;
pub const EXIT_STATUS_RECORD_SIZE: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileHandle(u32);

impl FileHandle {
    pub const fn new(value: u32) -> Result<Self, AbiError> {
        if value < 2 {
            return Err(AbiError::InvalidHandle(value));
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessHandle(u32);

impl ProcessHandle {
    pub const fn new(value: u32) -> Result<Self, AbiError> {
        if value == 0 {
            return Err(AbiError::InvalidHandle(value));
        }
        Ok(Self(value))
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}

pub const OPEN_READ: u32 = 0x0000_0001;
pub const OPEN_WRITE: u32 = 0x0000_0002;
pub const OPEN_CREATE: u32 = 0x0000_0004;
pub const OPEN_TRUNCATE: u32 = 0x0000_0008;
pub const OPEN_ALL: u32 = OPEN_READ | OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OpenFlags(u32);

impl OpenFlags {
    pub const fn new(value: u32) -> Result<Self, AbiError> {
        if value & !OPEN_ALL != 0 {
            return Err(AbiError::InvalidArgument { index: 2 });
        }
        Ok(Self(value))
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

pub fn valid_open_flags(input: u32) -> bool {
    input & !OPEN_ALL == 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SeekOrigin {
    Start = 0,
    Current = 1,
    End = 2,
}

impl TryFrom<u32> for SeekOrigin {
    type Error = AbiError;

    fn try_from(input: u32) -> Result<Self, Self::Error> {
        match input {
            0 => Ok(Self::Start),
            1 => Ok(Self::Current),
            2 => Ok(Self::End),
            _ => Err(AbiError::InvalidArgument { index: 3 }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum AbiFileKind {
    File = 1,
    Directory = 2,
}

impl AbiFileKind {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

impl TryFrom<u16> for AbiFileKind {
    type Error = AbiError;

    fn try_from(input: u16) -> Result<Self, Self::Error> {
        match input {
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            _ => Err(AbiError::InvalidArgument { index: 0 }),
        }
    }
}

impl TryFrom<u32> for AbiFileKind {
    type Error = AbiError;

    fn try_from(input: u32) -> Result<Self, Self::Error> {
        u16::try_from(input)
            .map_err(|_| AbiError::InvalidArgument { index: 0 })
            .and_then(Self::try_from)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilePermissions(u32);

impl FilePermissions {
    pub const READ: u32 = 1;
    pub const WRITE: u32 = 2;
    pub const EXECUTE: u32 = 4;
    pub const fn new(value: u32) -> Result<Self, AbiError> {
        if value & !(Self::READ | Self::WRITE | Self::EXECUTE) != 0 {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        Ok(Self(value))
    }
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IoResult {
    transferred: u64,
    status: SyscallStatus,
}

impl IoResult {
    pub fn new(
        config: ArchitectureConfig,
        transferred: u64,
        status: SyscallStatus,
    ) -> Result<Self, AbiError> {
        validate_word_value(config.word_width(), transferred)?;
        Ok(Self {
            transferred,
            status,
        })
    }
    pub const fn transferred(&self) -> u64 {
        self.transferred
    }
    pub const fn status(&self) -> SyscallStatus {
        self.status
    }
    pub fn encode(&self) -> [u8; IO_RESULT_SIZE] {
        let mut output = [0u8; IO_RESULT_SIZE];
        output[..8].copy_from_slice(&self.transferred.to_le_bytes());
        output[8..12].copy_from_slice(&self.status.as_u32().to_le_bytes());
        output
    }
    pub fn decode(input: &[u8], config: ArchitectureConfig) -> Result<Self, AbiError> {
        if input.len() != IO_RESULT_SIZE {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        let transferred = read_u64(input, 0)?;
        let status = SyscallStatus::try_from(read_u32(input, 8)?)?;
        if read_u32(input, 12)? != 0 {
            return Err(AbiError::ReservedNonzero {
                field: "IoResult.reserved",
                value: read_u32(input, 12)?,
            });
        }
        Self::new(config, transferred, status)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileStat {
    kind: AbiFileKind,
    permissions: FilePermissions,
    size: u64,
}

impl FileStat {
    pub fn new(
        config: ArchitectureConfig,
        kind: AbiFileKind,
        permissions: FilePermissions,
        size: u64,
    ) -> Result<Self, AbiError> {
        validate_word_value(config.word_width(), size)?;
        Ok(Self {
            kind,
            permissions,
            size,
        })
    }
    pub const fn kind(&self) -> AbiFileKind {
        self.kind
    }
    pub const fn permissions(&self) -> FilePermissions {
        self.permissions
    }
    pub const fn size(&self) -> u64 {
        self.size
    }
    pub fn encode(&self) -> [u8; FILE_STAT_SIZE] {
        let mut output = [0u8; FILE_STAT_SIZE];
        output[..4].copy_from_slice(&(self.kind as u32).to_le_bytes());
        output[4..8].copy_from_slice(&self.permissions.bits().to_le_bytes());
        output[8..].copy_from_slice(&self.size.to_le_bytes());
        output
    }
    pub fn decode(input: &[u8], config: ArchitectureConfig) -> Result<Self, AbiError> {
        if input.len() != FILE_STAT_SIZE {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        Self::new(
            config,
            AbiFileKind::try_from(read_u32(input, 0)?)?,
            FilePermissions::new(read_u32(input, 4)?)?,
            read_u64(input, 8)?,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryRecord {
    name: [u8; DIRECTORY_NAME_CAPACITY],
    name_length: u16,
    kind: AbiFileKind,
}

impl DirectoryRecord {
    pub fn new(name: &[u8], kind: AbiFileKind) -> Result<Self, AbiError> {
        if name.len() > DIRECTORY_NAME_CAPACITY {
            return Err(AbiError::InvalidNameLength {
                length: name.len(),
                maximum: DIRECTORY_NAME_CAPACITY,
            });
        }
        let length = match u16::try_from(name.len()) {
            Ok(length) => length,
            Err(_) => {
                return Err(AbiError::InvalidNameLength {
                    length: name.len(),
                    maximum: DIRECTORY_NAME_CAPACITY,
                });
            }
        };
        let mut stored = [0u8; DIRECTORY_NAME_CAPACITY];
        stored[..name.len()].copy_from_slice(name);
        Ok(Self {
            name: stored,
            name_length: length,
            kind,
        })
    }
    pub fn name(&self) -> &[u8] {
        &self.name[..self.name_length as usize]
    }
    pub const fn kind(&self) -> AbiFileKind {
        self.kind
    }
    pub fn encode(&self) -> [u8; DIRECTORY_RECORD_SIZE] {
        let mut output = [0u8; DIRECTORY_RECORD_SIZE];
        output[..2].copy_from_slice(&self.name_length.to_le_bytes());
        output[2..4].copy_from_slice(&self.kind.as_u16().to_le_bytes());
        output[4..4 + self.name_length as usize].copy_from_slice(self.name());
        output
    }
    pub fn decode(input: &[u8]) -> Result<Self, AbiError> {
        if input.len() != DIRECTORY_RECORD_SIZE {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        let name_length = u16::from_le_bytes([input[0], input[1]]);
        if name_length as usize > DIRECTORY_NAME_CAPACITY {
            return Err(AbiError::InvalidNameLength {
                length: name_length as usize,
                maximum: DIRECTORY_NAME_CAPACITY,
            });
        }
        let kind = AbiFileKind::try_from(u16::from_le_bytes([input[2], input[3]]))?;
        let mut name = [0u8; DIRECTORY_NAME_CAPACITY];
        name[..name_length as usize].copy_from_slice(&input[4..4 + name_length as usize]);
        Ok(Self {
            name,
            name_length,
            kind,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryAllocation {
    address: VirtualAddress,
    length: u64,
}

impl MemoryAllocation {
    pub fn new(config: ArchitectureConfig, address: u64, length: u64) -> Result<Self, AbiError> {
        validate_range(config, address, length, 1)?;
        Ok(Self {
            address: VirtualAddress::new(address),
            length,
        })
    }
    pub const fn address(&self) -> VirtualAddress {
        self.address
    }
    pub const fn length(&self) -> u64 {
        self.length
    }
    pub fn encode(&self) -> [u8; MEMORY_ALLOCATION_SIZE] {
        let mut output = [0u8; MEMORY_ALLOCATION_SIZE];
        output[..8].copy_from_slice(&self.address.as_u64().to_le_bytes());
        output[8..].copy_from_slice(&self.length.to_le_bytes());
        output
    }
    pub fn decode(input: &[u8], config: ArchitectureConfig) -> Result<Self, AbiError> {
        if input.len() != MEMORY_ALLOCATION_SIZE {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        Self::new(config, read_u64(input, 0)?, read_u64(input, 8)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum ProcessExitReason {
    Normal = 0,
    Killed = 1,
    Faulted = 2,
    Terminated = 3,
}

impl ProcessExitReason {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for ProcessExitReason {
    type Error = AbiError;

    fn try_from(input: u32) -> Result<Self, Self::Error> {
        match input {
            0 => Ok(Self::Normal),
            1 => Ok(Self::Killed),
            2 => Ok(Self::Faulted),
            3 => Ok(Self::Terminated),
            _ => Err(AbiError::InvalidExitReason(input)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitStatusRecord {
    exit_code: u32,
    reason: ProcessExitReason,
}

impl ExitStatusRecord {
    pub const fn new(exit_code: u32, reason: ProcessExitReason) -> Self {
        Self { exit_code, reason }
    }
    pub const fn exit_code(&self) -> u32 {
        self.exit_code
    }
    pub const fn reason(&self) -> ProcessExitReason {
        self.reason
    }
    pub fn encode(&self) -> [u8; EXIT_STATUS_RECORD_SIZE] {
        let mut output = [0u8; EXIT_STATUS_RECORD_SIZE];
        output[..4].copy_from_slice(&self.exit_code.to_le_bytes());
        output[4..].copy_from_slice(&self.reason.as_u32().to_le_bytes());
        output
    }
    pub fn decode(input: &[u8]) -> Result<Self, AbiError> {
        if input.len() != EXIT_STATUS_RECORD_SIZE {
            return Err(AbiError::InvalidArgument { index: 0 });
        }
        Ok(Self {
            exit_code: read_u32(input, 0)?,
            reason: ProcessExitReason::try_from(read_u32(input, 4)?)?,
        })
    }
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, AbiError> {
    let end = offset
        .checked_add(4)
        .ok_or(AbiError::InvalidArgument { index: 0 })?;
    let bytes = input
        .get(offset..end)
        .ok_or(AbiError::InvalidArgument { index: 0 })?;
    Ok(u32::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| AbiError::InvalidArgument { index: 0 })?,
    ))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, AbiError> {
    let end = offset
        .checked_add(8)
        .ok_or(AbiError::InvalidArgument { index: 0 })?;
    let bytes = input
        .get(offset..end)
        .ok_or(AbiError::InvalidArgument { index: 0 })?;
    Ok(u64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| AbiError::InvalidArgument { index: 0 })?,
    ))
}

impl fmt::Display for AbiFileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => f.write_str("file"),
            Self::Directory => f.write_str("directory"),
        }
    }
}
