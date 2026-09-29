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

// The machine's geometry comes from `lazalith-machine`, which is where a machine
// profile lives and therefore the only place that should be able to answer "where
// does the reset vector point" and "how much RAM is there".
//
// Two of these values — `KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP` — were
// previously declared here *and* in `lazalith-os`, as two independent constants
// with the same numbers. B4 gives the machine one definition and both crates
// re-export it, so the two can no longer agree today and disagree tomorrow. The
// names and the values are unchanged, so nothing above this line moved.
pub const BOOT_ROM_START: u64 = lazalith_machine::LZA64_LAYOUT.boot_rom_start;
pub const BOOT_ROM_LENGTH: u64 = lazalith_machine::LZA64_LAYOUT.boot_rom_length;
pub const BOOT_HEADER_ADDRESS: u64 = lazalith_machine::LZA64_LAYOUT.boot_header_address;
pub const KERNEL_PAYLOAD_ADDRESS: u64 = lazalith_machine::LZA64_LAYOUT.kernel_payload_address;
pub const MAX_BOOT_ROM_PAYLOAD: u64 = lazalith_machine::LZA64_LAYOUT.max_boot_rom_payload;
pub const KERNEL_LOAD_ADDRESS: u64 = lazalith_machine::LZA64_LAYOUT.kernel_load_address;
pub const KERNEL_IMAGE_LENGTH: u64 = lazalith_machine::LZA64_LAYOUT.kernel_image_length;
pub const KERNEL_INITIAL_SP: u64 = lazalith_machine::LZA64_LAYOUT.kernel_initial_sp;
pub const PHYSICAL_RAM_START: u64 = lazalith_machine::LZA64_LAYOUT.physical_ram_start;
pub const PHYSICAL_RAM_LENGTH: u64 = lazalith_machine::LZA64_LAYOUT.physical_ram_length;
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
