use crate::BootArchitecture;
use alloc::boxed::Box;
use core::{error::Error, fmt, num::TryFromIntError};
use lazalith_isa::{DecodeError, InstructionError};
use lazalith_machine::{MachineError, TrapEvent};
use lazalith_memory::MemoryFault;
use lazalith_os::MemoryError as OsMemoryError;
use lazalith_types::{InstructionAddress, InvalidRegisterIndex, PhysicalAddress, WidthError};

#[derive(Debug)]
pub enum BootError {
    InvalidBootRomLength {
        expected: u64,
        actual: u64,
    },
    InvalidHeaderSlice,
    InvalidMagic,
    UnsupportedVersion {
        input: u32,
    },
    InvalidArchitecture {
        input: u8,
    },
    ArchitectureMismatch {
        expected: BootArchitecture,
        actual: BootArchitecture,
    },
    UnsupportedFlags {
        input: u8,
    },
    InvalidHeaderSize {
        input: u16,
    },
    InvalidLoadAddress {
        input: PhysicalAddress,
    },
    InvalidImageLength {
        input: u64,
    },
    PayloadDoesNotFitRom {
        input: u64,
        maximum: u64,
    },
    InvalidEntryOffset {
        offset: u64,
        image_length: u64,
    },
    EntryAddressOverflow {
        offset: u64,
    },
    EntryWidth {
        entry: InstructionAddress,
        end: PhysicalAddress,
        source: WidthError,
    },
    EntryOutsideKernel {
        entry: InstructionAddress,
        end: PhysicalAddress,
    },
    NonzeroReserved {
        input: u32,
    },
    NonzeroReservedByte {
        offset: usize,
        input: u8,
    },
    ChecksumMismatch {
        expected: u32,
        actual: u32,
    },
    BootRomMismatch {
        offset: usize,
        expected: u8,
        actual: u8,
    },
    UnresolvedLabel {
        label: u8,
    },
    InvalidBranchOffset,
    BootloaderTooLarge {
        length: usize,
        limit: usize,
    },
    EntryInstruction(DecodeError),
    Register(InvalidRegisterIndex),
    Instruction(InstructionError),
    Allocation(alloc::collections::TryReserveError),
    HostSize(TryFromIntError),
    Memory(MemoryFault),
    OsMemory(OsMemoryError),
    UnexpectedDevices {
        count: usize,
    },
    BootHandoffMismatch {
        field: &'static str,
    },
    Machine(Box<MachineError>),
    UnexpectedBootTrap {
        event: TrapEvent,
    },
    BootloaderHalted {
        pc: InstructionAddress,
    },
    BootLimitOverflow,
    BootStepLimit {
        limit: u64,
        pc: InstructionAddress,
        remaining: u64,
    },
}

impl fmt::Display for BootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBootRomLength { expected, actual } => {
                write!(f, "boot ROM length {actual} does not match {expected}")
            }
            Self::InvalidHeaderSlice => f.write_str("boot header is truncated"),
            Self::InvalidMagic => f.write_str("invalid boot image magic"),
            Self::UnsupportedVersion { input } => write!(f, "unsupported boot version {input}"),
            Self::InvalidArchitecture { input } => write!(f, "invalid boot architecture {input}"),
            Self::ArchitectureMismatch { expected, actual } => {
                write!(
                    f,
                    "boot architecture {actual:?} does not match {expected:?}"
                )
            }
            Self::UnsupportedFlags { input } => write!(f, "unsupported boot flags {input:#04x}"),
            Self::InvalidHeaderSize { input } => write!(f, "invalid boot header size {input}"),
            Self::InvalidLoadAddress { input } => {
                write!(f, "invalid kernel load address {:#x}", input.as_u64())
            }
            Self::InvalidImageLength { input } => write!(f, "invalid kernel image length {input}"),
            Self::PayloadDoesNotFitRom { input, maximum } => {
                write!(
                    f,
                    "kernel payload length {input} exceeds ROM capacity {maximum}"
                )
            }
            Self::InvalidEntryOffset {
                offset,
                image_length,
            } => write!(
                f,
                "kernel entry offset {offset} is invalid for image length {image_length}"
            ),
            Self::EntryAddressOverflow { offset } => {
                write!(f, "kernel entry offset {offset} overflows its load address")
            }
            Self::EntryWidth { entry, end, source } => {
                write!(
                    f,
                    "kernel entry range {:#x}..{:#x} is invalid: {source}",
                    entry.as_u64(),
                    end.as_u64()
                )
            }
            Self::EntryOutsideKernel { entry, end } => {
                write!(
                    f,
                    "kernel entry range {:#x}..{:#x} is outside the kernel image",
                    entry.as_u64(),
                    end.as_u64()
                )
            }
            Self::NonzeroReserved { input } => write!(f, "boot reserved field is {input:#010x}"),
            Self::NonzeroReservedByte { offset, input } => {
                write!(f, "reserved boot ROM byte {offset} is {input:#04x}")
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "boot payload checksum {actual:#010x} does not match {expected:#010x}"
            ),
            Self::BootRomMismatch {
                offset,
                expected,
                actual,
            } => write!(
                f,
                "boot ROM byte {offset} is {actual:#04x}, expected {expected:#04x}"
            ),
            Self::UnresolvedLabel { label } => write!(f, "unresolved bootloader label {label}"),
            Self::InvalidBranchOffset => f.write_str("bootloader branch offset is invalid"),
            Self::BootloaderTooLarge { length, limit } => write!(
                f,
                "bootloader length {length} exceeds reserved limit {limit}"
            ),
            Self::EntryInstruction(source) => source.fmt(f),
            Self::Register(source) => source.fmt(f),
            Self::Instruction(source) => source.fmt(f),
            Self::Allocation(source) => write!(f, "boot image allocation failed: {source}"),
            Self::HostSize(source) => source.fmt(f),
            Self::Memory(source) => source.fmt(f),
            Self::OsMemory(source) => source.fmt(f),
            Self::UnexpectedDevices { count } => {
                write!(
                    f,
                    "Step 28 boot requires an empty device manager, got {count} devices"
                )
            }
            Self::BootHandoffMismatch { field } => {
                write!(
                    f,
                    "bootloader produced invalid kernel handoff field {field}"
                )
            }
            Self::Machine(source) => source.fmt(f),
            Self::UnexpectedBootTrap { event } => {
                write!(f, "bootloader unexpectedly delivered trap {event:?}")
            }
            Self::BootloaderHalted { pc } => {
                write!(
                    f,
                    "bootloader halted before kernel entry at {:#x}",
                    pc.as_u64()
                )
            }
            Self::BootLimitOverflow => f.write_str("bootloader instruction limit overflowed"),
            Self::BootStepLimit {
                limit,
                pc,
                remaining,
            } => {
                write!(
                    f,
                    "bootloader did not reach kernel entry within {limit} instructions at {:#x} with {remaining} bytes remaining",
                    pc.as_u64()
                )
            }
        }
    }
}

impl Error for BootError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::EntryInstruction(source) => Some(source),
            Self::EntryWidth { source, .. } => Some(source),
            Self::Register(source) => Some(source),
            Self::Instruction(source) => Some(source),
            Self::Allocation(source) => Some(source),
            Self::HostSize(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::OsMemory(source) => Some(source),
            Self::Machine(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}
