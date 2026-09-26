use crate::{AbiError, WordValueOutOfRange};
use core::{error::Error, fmt};
use lazalith_types::{ArchitectureConfig, VirtualAddress, WordWidth};

pub const ABI_VERSION: u16 = 1;
pub const MAX_PATH_BYTES: u64 = 4096;
pub const MAX_ARGUMENT_COUNT: u32 = 1024;
pub const MAX_ARGUMENT_BYTES: u64 = 65_536;
pub const MAX_ARGUMENT_TOTAL_BYTES: u64 = 1_048_576;
pub const SYSCALL_NUMBER_REGISTER: u8 = 0;
pub const SYSCALL_FIRST_ARGUMENT_REGISTER: u8 = 1;
pub const SYSCALL_ARGUMENT_COUNT: usize = 6;
pub const SYSCALL_RESERVED_REGISTER: u8 = 7;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u16)]
pub enum Syscall {
    Exit = 0x0001,
    Write = 0x0002,
    Read = 0x0003,
    Open = 0x0004,
    Close = 0x0005,
    Seek = 0x0006,
    Stat = 0x0007,
    ListDirectory = 0x0008,
    Time = 0x0009,
    Sleep = 0x000a,
    AllocateMemory = 0x000b,
    SpawnProcess = 0x000c,
    WaitProcess = 0x000d,
    ClearScreen = 0x000e,
    /// Opens a window and reports the framebuffer the guest owns.
    ///
    /// Step 70. The guest is handed an *address*, not a copy: the display device
    /// shares the guest's memory, so a call that returned pixels would be a
    /// transfer the design explicitly does not make.
    DisplayOpen = 0x000f,
    /// Presents the framebuffer at the address the guest supplied.
    ///
    /// Step 70. A present is a synchronisation point, not an upload.
    DisplayPresent = 0x0010,
}

impl Syscall {
    pub const ALL: &'static [Self] = &[
        Self::Exit,
        Self::Write,
        Self::Read,
        Self::Open,
        Self::Close,
        Self::Seek,
        Self::Stat,
        Self::ListDirectory,
        Self::Time,
        Self::Sleep,
        Self::AllocateMemory,
        Self::SpawnProcess,
        Self::WaitProcess,
        Self::ClearScreen,
        Self::DisplayOpen,
        Self::DisplayPresent,
    ];

    pub const fn as_u16(self) -> u16 {
        self as u16
    }

    pub const fn is_reserved(raw: u64) -> bool {
        raw >= 0x0100 && raw <= u16::MAX as u64
    }

    pub const fn argument_count(self) -> usize {
        match self {
            Self::Exit | Self::Close | Self::Time | Self::Sleep => 1,
            Self::WaitProcess => 3,
            Self::Open | Self::Seek | Self::Stat | Self::AllocateMemory => 4,
            Self::Write | Self::Read | Self::ListDirectory | Self::SpawnProcess => 5,
            Self::ClearScreen => 0,
            // `display_open` takes the geometry, the framebuffer address and a
            // record; `display_present` takes only the framebuffer address.
            Self::DisplayOpen => 4,
            Self::DisplayPresent => 2,
        }
    }

    pub const fn required_zero_argument_mask(self) -> u64 {
        match self {
            Self::Write | Self::Read => 0x0010,
            Self::Open | Self::Stat | Self::AllocateMemory => 0x0008,
            Self::WaitProcess => 0x0004,
            Self::ClearScreen => 0x003f,
            Self::Exit
            | Self::Close
            | Self::Seek
            | Self::ListDirectory
            | Self::Time
            | Self::Sleep
            | Self::SpawnProcess
            | Self::DisplayOpen
            | Self::DisplayPresent => 0,
        }
    }

    pub const fn ignored_argument_mask(self) -> u64 {
        match self {
            Self::Exit | Self::Close | Self::Time | Self::Sleep => 0x003e,
            Self::Write | Self::Read | Self::ListDirectory | Self::SpawnProcess => 0x0020,
            Self::Open | Self::Seek | Self::Stat | Self::AllocateMemory => 0x0030,
            Self::WaitProcess => 0x0038,
            // `display_open` names four arguments and `display_present` two, and
            // the registers past the ones each uses must be zero for the kernel
            // to accept the call.
            Self::DisplayOpen => 0x0030,
            Self::DisplayPresent => 0x003c,
            Self::ClearScreen => 0,
        }
    }

    pub const fn returns(self) -> bool {
        !matches!(self, Self::Exit)
    }

    pub fn from_word(config: ArchitectureConfig, raw: u64) -> Result<Self, AbiError> {
        if raw > config.word_width().mask() {
            return Err(AbiError::UnknownSyscall(raw));
        }
        let raw = u16::try_from(raw).map_err(|_| AbiError::UnknownSyscall(raw))?;
        Self::try_from(raw)
    }
}

impl TryFrom<u16> for Syscall {
    type Error = AbiError;

    fn try_from(input: u16) -> Result<Self, Self::Error> {
        match input {
            0x0001 => Ok(Self::Exit),
            0x0002 => Ok(Self::Write),
            0x0003 => Ok(Self::Read),
            0x0004 => Ok(Self::Open),
            0x0005 => Ok(Self::Close),
            0x0006 => Ok(Self::Seek),
            0x0007 => Ok(Self::Stat),
            0x0008 => Ok(Self::ListDirectory),
            0x0009 => Ok(Self::Time),
            0x000a => Ok(Self::Sleep),
            0x000b => Ok(Self::AllocateMemory),
            0x000c => Ok(Self::SpawnProcess),
            0x000d => Ok(Self::WaitProcess),
            0x000e => Ok(Self::ClearScreen),
            0x000f => Ok(Self::DisplayOpen),
            0x0010 => Ok(Self::DisplayPresent),
            _ => Err(AbiError::UnknownSyscall(u64::from(input))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u32)]
pub enum SyscallError {
    UnknownSyscall = 1,
    InvalidArgument = 2,
    InvalidPointer = 3,
    RangeOverflow = 4,
    Misaligned = 5,
    NotFound = 6,
    AlreadyExists = 7,
    PermissionDenied = 8,
    InvalidHandle = 9,
    NotDirectory = 10,
    IsDirectory = 11,
    NotSupported = 12,
    ResourceExhausted = 13,
    InvalidState = 14,
    IoFailure = 15,
    DeviceFailure = 16,
    ProcessFailure = 17,
    Faulted = 18,
    Internal = 19,
}

impl SyscallError {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl From<SyscallError> for SyscallStatus {
    fn from(error: SyscallError) -> Self {
        match error {
            SyscallError::UnknownSyscall => Self::UnknownSyscall,
            SyscallError::InvalidArgument => Self::InvalidArgument,
            SyscallError::InvalidPointer => Self::InvalidPointer,
            SyscallError::RangeOverflow => Self::RangeOverflow,
            SyscallError::Misaligned => Self::Misaligned,
            SyscallError::NotFound => Self::NotFound,
            SyscallError::AlreadyExists => Self::AlreadyExists,
            SyscallError::PermissionDenied => Self::PermissionDenied,
            SyscallError::InvalidHandle => Self::InvalidHandle,
            SyscallError::NotDirectory => Self::NotDirectory,
            SyscallError::IsDirectory => Self::IsDirectory,
            SyscallError::NotSupported => Self::NotSupported,
            SyscallError::ResourceExhausted => Self::ResourceExhausted,
            SyscallError::InvalidState => Self::InvalidState,
            SyscallError::IoFailure => Self::IoFailure,
            SyscallError::DeviceFailure => Self::DeviceFailure,
            SyscallError::ProcessFailure => Self::ProcessFailure,
            SyscallError::Faulted => Self::Faulted,
            SyscallError::Internal => Self::Internal,
        }
    }
}

impl fmt::Display for SyscallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "syscall error {:?}", self.as_u32())
    }
}

impl Error for SyscallError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SyscallStatus {
    Ok = 0,
    UnknownSyscall = 1,
    InvalidArgument = 2,
    InvalidPointer = 3,
    RangeOverflow = 4,
    Misaligned = 5,
    NotFound = 6,
    AlreadyExists = 7,
    PermissionDenied = 8,
    InvalidHandle = 9,
    NotDirectory = 10,
    IsDirectory = 11,
    NotSupported = 12,
    ResourceExhausted = 13,
    InvalidState = 14,
    IoFailure = 15,
    DeviceFailure = 16,
    ProcessFailure = 17,
    Faulted = 18,
    Internal = 19,
}

impl SyscallStatus {
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl TryFrom<u32> for SyscallStatus {
    type Error = AbiError;

    fn try_from(input: u32) -> Result<Self, Self::Error> {
        match input {
            0 => Ok(Self::Ok),
            1 => Ok(Self::UnknownSyscall),
            2 => Ok(Self::InvalidArgument),
            3 => Ok(Self::InvalidPointer),
            4 => Ok(Self::RangeOverflow),
            5 => Ok(Self::Misaligned),
            6 => Ok(Self::NotFound),
            7 => Ok(Self::AlreadyExists),
            8 => Ok(Self::PermissionDenied),
            9 => Ok(Self::InvalidHandle),
            10 => Ok(Self::NotDirectory),
            11 => Ok(Self::IsDirectory),
            12 => Ok(Self::NotSupported),
            13 => Ok(Self::ResourceExhausted),
            14 => Ok(Self::InvalidState),
            15 => Ok(Self::IoFailure),
            16 => Ok(Self::DeviceFailure),
            17 => Ok(Self::ProcessFailure),
            18 => Ok(Self::Faulted),
            19 => Ok(Self::Internal),
            _ => Err(AbiError::InvalidStatus(u64::from(input))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaggedOutcome {
    status: SyscallStatus,
    payload: u32,
}

impl TaggedOutcome {
    pub const fn success(payload: u32) -> Self {
        Self {
            status: SyscallStatus::Ok,
            payload,
        }
    }

    pub fn failure(error: SyscallError, detail: u32) -> Self {
        Self {
            status: error.into(),
            payload: detail,
        }
    }

    pub const fn status(self) -> SyscallStatus {
        self.status
    }

    pub const fn payload(self) -> u32 {
        self.payload
    }

    pub const fn registers(self) -> [u64; 2] {
        [self.status.as_u32() as u64, self.payload as u64]
    }

    pub fn decode(registers: [u64; 2]) -> Result<Self, AbiError> {
        let status = SyscallStatus::try_from(
            u32::try_from(registers[0]).map_err(|_| AbiError::InvalidStatus(registers[0]))?,
        )?;
        let payload =
            u32::try_from(registers[1]).map_err(|_| AbiError::InvalidArgument { index: 1 })?;
        Ok(Self { status, payload })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyscallArguments {
    values: [u64; SYSCALL_ARGUMENT_COUNT],
}

impl SyscallArguments {
    pub const fn new(values: [u64; SYSCALL_ARGUMENT_COUNT]) -> Self {
        Self { values }
    }

    pub fn get(&self, index: usize) -> Option<u64> {
        self.values.get(index).copied()
    }

    fn argument(&self, index: usize) -> Result<u64, AbiError> {
        self.get(index).ok_or(AbiError::InvalidArgument {
            index: u8::try_from(index).unwrap_or(u8::MAX),
        })
    }

    pub fn pointer(
        &self,
        config: ArchitectureConfig,
        index: usize,
    ) -> Result<VirtualAddress, AbiError> {
        let value = self.argument(index)?;
        let width = config.word_width();
        if value > width.mask() {
            return Err(AbiError::InvalidPointerWidth {
                index: u8::try_from(index).unwrap_or(u8::MAX),
                source: WordValueOutOfRange::new(value, width),
            });
        }
        Ok(VirtualAddress::new(value))
    }

    pub fn u32(&self, index: usize) -> Result<u32, AbiError> {
        u32::try_from(self.argument(index)?).map_err(|_| AbiError::InvalidArgument {
            index: u8::try_from(index).unwrap_or(u8::MAX),
        })
    }

    pub fn word(&self, config: ArchitectureConfig, index: usize) -> Result<u64, AbiError> {
        let value = self.argument(index)?;
        let width = config.word_width();
        if value > width.mask() {
            return Err(AbiError::InvalidArgumentWidth {
                index: u8::try_from(index).unwrap_or(u8::MAX),
                source: WordValueOutOfRange::new(value, width),
            });
        }
        Ok(value)
    }

    pub fn signed_word(&self, config: ArchitectureConfig, index: usize) -> Result<i64, AbiError> {
        let value = self.word(config, index)?;
        Ok(match config.word_width() {
            lazalith_types::WordWidth::W32 => i64::from(value as u32 as i32),
            lazalith_types::WordWidth::W64 => value as i64,
        })
    }

    pub fn usize(&self, config: ArchitectureConfig, index: usize) -> Result<usize, AbiError> {
        usize::try_from(self.word(config, index)?).map_err(|_| AbiError::InvalidArgument {
            index: u8::try_from(index).unwrap_or(u8::MAX),
        })
    }
    pub fn validate_required_zero(&self, syscall: Syscall) -> Result<(), AbiError> {
        let mask = syscall.required_zero_argument_mask();
        for index in 0..SYSCALL_ARGUMENT_COUNT {
            if mask & (1_u64 << index) != 0 && self.get(index) != Some(0) {
                return Err(AbiError::InvalidArgument {
                    index: SYSCALL_FIRST_ARGUMENT_REGISTER + u8::try_from(index).unwrap_or(u8::MAX),
                });
            }
        }
        Ok(())
    }
}

pub fn validate_reserved_register(value: u64) -> Result<(), AbiError> {
    if value == 0 {
        Ok(())
    } else {
        Err(AbiError::InvalidArgument {
            index: SYSCALL_RESERVED_REGISTER,
        })
    }
}

pub(crate) fn validate_word_value(width: WordWidth, value: u64) -> Result<(), AbiError> {
    if value > width.mask() {
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            value, width,
        )))
    } else {
        Ok(())
    }
}

pub fn validate_range(
    config: ArchitectureConfig,
    address: u64,
    length: u64,
    alignment: u64,
) -> Result<(), AbiError> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(AbiError::InvalidArgument { index: 0 });
    }
    let width = config.word_width();
    width.validate_address(address).map_err(AbiError::Width)?;
    if !address.is_multiple_of(alignment) {
        return Err(AbiError::Misaligned { address, alignment });
    }
    validate_word_value(width, length)?;
    if length == 0 {
        return Ok(());
    }
    width
        .checked_access_end(address, length)
        .map(|_| ())
        .map_err(AbiError::Width)
}
