#![no_std]

extern crate alloc;

mod bootloader;
mod error;
mod image;

pub use error::BootError;
pub use image::{BootImage, BootImageHeader};

use core::fmt;
use lazalith_types::{ArchitectureConfig, InstructionAddress, PhysicalAddress, WordWidth};

pub const BOOT_MAGIC: [u8; 8] = *b"LZBOOT01";
pub const BOOT_FORMAT_VERSION: u32 = 1;
pub const BOOT_HEADER_SIZE: u16 = 48;
pub const BOOT_ROM_START: u64 = 0x0000_0000;
pub const BOOT_ROM_LENGTH: u64 = 0x0008_0000;
pub const BOOT_HEADER_ADDRESS: u64 = 0x0000_0400;
pub const KERNEL_PAYLOAD_ADDRESS: u64 = 0x0000_1000;
pub const MAX_BOOT_ROM_PAYLOAD: u64 = 0x0007_f000;
pub const KERNEL_LOAD_ADDRESS: u64 = 0x0010_0000;
pub const KERNEL_IMAGE_LENGTH: u64 = 0x0008_0000;
pub const KERNEL_STACK_START: u64 = 0x0018_0000;
pub const KERNEL_STACK_LENGTH: u64 = 0x0001_0000;
pub const KERNEL_INITIAL_SP: u64 = 0x0018_f000;
pub const RESET_VECTOR: InstructionAddress = InstructionAddress::new(BOOT_ROM_START);
pub const BOOT_ADDRESS: InstructionAddress = RESET_VECTOR;
pub const BOOT_ROM_PHYSICAL_START: PhysicalAddress = PhysicalAddress::new(BOOT_ROM_START);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BootArchitecture {
    Lz32 = 1,
    Lz64 = 2,
}

impl BootArchitecture {
    pub const fn from_config(config: ArchitectureConfig) -> Self {
        match config.word_width() {
            WordWidth::W32 => Self::Lz32,
            WordWidth::W64 => Self::Lz64,
        }
    }

    pub const fn config(self) -> ArchitectureConfig {
        match self {
            Self::Lz32 => ArchitectureConfig::lz32(),
            Self::Lz64 => ArchitectureConfig::lz64(),
        }
    }

    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    pub(crate) const fn from_code(input: u8) -> Option<Self> {
        match input {
            1 => Some(Self::Lz32),
            2 => Some(Self::Lz64),
            _ => None,
        }
    }
}

impl fmt::Display for BootArchitecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lz32 => f.write_str("LZ32"),
            Self::Lz64 => f.write_str("LZ64"),
        }
    }
}
