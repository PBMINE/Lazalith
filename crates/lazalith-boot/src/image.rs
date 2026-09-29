use crate::{
    BOOT_FORMAT_VERSION, BOOT_HEADER_ADDRESS, BOOT_HEADER_SIZE, BOOT_MAGIC, BOOT_ROM_LENGTH,
    BOOT_ROM_PHYSICAL_START, BootArchitecture, BootError, KERNEL_IMAGE_LENGTH, KERNEL_INITIAL_SP,
    KERNEL_LOAD_ADDRESS, KERNEL_PAYLOAD_ADDRESS, MAX_BOOT_ROM_PAYLOAD, RESET_VECTOR, bootloader,
};
use alloc::{boxed::Box, vec::Vec};
use core::fmt;
use lazalith_devices::{Device, DeviceManager};
use lazalith_isa::decode;
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::{KernelMemory, UserMemory};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BootImageHeader {
    config: ArchitectureConfig,
    image_length: u64,
    entry_offset: u64,
    entry: InstructionAddress,
    checksum: u32,
}

impl BootImageHeader {
    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }
    pub const fn image_length(&self) -> u64 {
        self.image_length
    }
    pub const fn entry_offset(&self) -> u64 {
        self.entry_offset
    }
    pub const fn checksum(&self) -> u32 {
        self.checksum
    }
    pub const fn entry(&self) -> InstructionAddress {
        self.entry
    }
}

struct ParsedHeader {
    image_length: u64,
    entry_offset: u64,
    checksum: u32,
}

pub struct BootImage {
    config: ArchitectureConfig,
    header: BootImageHeader,
    rom: Vec<u8>,
    kernel_start: usize,
    kernel_end: usize,
    boot_instructions: u16,
}

impl fmt::Debug for BootImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BootImage")
            .field("config", &self.config)
            .field("header", &self.header)
            .field("rom_length", &self.rom.len())
            .field("boot_instructions", &self.boot_instructions)
            .finish()
    }
}

impl BootImage {
    pub fn new(
        config: ArchitectureConfig,
        kernel: Vec<u8>,
        entry_offset: u64,
    ) -> Result<Self, BootError> {
        let image_length = u64::try_from(kernel.len()).map_err(BootError::HostSize)?;
        let entry = validate_kernel(config, &kernel, image_length, entry_offset)?;
        let bootloader = bootloader::build(config)?;
        ensure_bootloader_fits(&bootloader.bytes)?;
        let checksum = crc32(&kernel);
        let mut rom = Vec::new();
        let rom_length = usize::try_from(BOOT_ROM_LENGTH).map_err(BootError::HostSize)?;
        rom.try_reserve_exact(rom_length)
            .map_err(BootError::Allocation)?;
        rom.resize(rom_length, 0);
        let code_end = bootloader.bytes.len();
        let header_start = usize::try_from(BOOT_HEADER_ADDRESS).map_err(BootError::HostSize)?;
        let payload_start = usize::try_from(KERNEL_PAYLOAD_ADDRESS).map_err(BootError::HostSize)?;
        let payload_end =
            payload_start
                .checked_add(kernel.len())
                .ok_or(BootError::InvalidImageLength {
                    input: image_length,
                })?;
        if code_end > header_start || payload_end > rom_length {
            return Err(BootError::BootloaderTooLarge {
                length: payload_end,
                limit: rom_length,
            });
        }
        rom.get_mut(..code_end)
            .ok_or(BootError::BootloaderTooLarge {
                length: code_end,
                limit: header_start,
            })?
            .copy_from_slice(&bootloader.bytes);
        let header_output = rom
            .get_mut(header_start..)
            .ok_or(BootError::InvalidHeaderSlice)?;
        write_header(header_output, config, image_length, entry_offset, checksum)?;
        rom.get_mut(payload_start..payload_end)
            .ok_or(BootError::InvalidImageLength {
                input: image_length,
            })?
            .copy_from_slice(&kernel);
        Ok(Self {
            config,
            header: BootImageHeader {
                config,
                image_length,
                entry_offset,
                entry,
                checksum,
            },
            rom,
            kernel_start: payload_start,
            kernel_end: payload_end,
            boot_instructions: bootloader.instructions,
        })
    }

    pub fn from_rom(config: ArchitectureConfig, rom: Vec<u8>) -> Result<Self, BootError> {
        let expected_length = usize::try_from(BOOT_ROM_LENGTH).map_err(BootError::HostSize)?;
        if rom.len() != expected_length {
            return Err(BootError::InvalidBootRomLength {
                expected: BOOT_ROM_LENGTH,
                actual: u64::try_from(rom.len()).map_err(BootError::HostSize)?,
            });
        }
        let prefix_length = usize::try_from(BOOT_HEADER_ADDRESS).map_err(BootError::HostSize)?;
        let header_start = prefix_length;
        let header_end = header_start
            .checked_add(usize::from(BOOT_HEADER_SIZE))
            .ok_or(BootError::InvalidHeaderSlice)?;
        let header_bytes = rom
            .get(header_start..header_end)
            .ok_or(BootError::InvalidHeaderSlice)?;
        let header = parse_header(config, header_bytes)?;

        validate_image_length(config, header.image_length)?;
        let payload_start = usize::try_from(KERNEL_PAYLOAD_ADDRESS).map_err(BootError::HostSize)?;
        let image_length = usize::try_from(header.image_length).map_err(BootError::HostSize)?;
        let payload_end =
            payload_start
                .checked_add(image_length)
                .ok_or(BootError::InvalidImageLength {
                    input: header.image_length,
                })?;
        if payload_end > rom.len() {
            return Err(BootError::PayloadDoesNotFitRom {
                input: header.image_length,
                maximum: MAX_BOOT_ROM_PAYLOAD,
            });
        }
        let payload =
            rom.get(payload_start..payload_end)
                .ok_or(BootError::PayloadDoesNotFitRom {
                    input: header.image_length,
                    maximum: MAX_BOOT_ROM_PAYLOAD,
                })?;
        let entry = validate_kernel(config, payload, header.image_length, header.entry_offset)?;
        let actual_checksum = crc32(payload);
        if actual_checksum != header.checksum {
            return Err(BootError::ChecksumMismatch {
                expected: header.checksum,
                actual: actual_checksum,
            });
        }

        let bootloader = bootloader::build(config)?;
        ensure_bootloader_fits(&bootloader.bytes)?;
        let mut expected_prefix = Vec::new();
        expected_prefix
            .try_reserve_exact(prefix_length)
            .map_err(BootError::Allocation)?;
        expected_prefix.resize(prefix_length, 0);
        expected_prefix
            .get_mut(..bootloader.bytes.len())
            .ok_or(BootError::BootloaderTooLarge {
                length: bootloader.bytes.len(),
                limit: prefix_length,
            })?
            .copy_from_slice(&bootloader.bytes);
        let actual_prefix = rom
            .get(..prefix_length)
            .ok_or(BootError::InvalidBootRomLength {
                expected: BOOT_ROM_LENGTH,
                actual: u64::try_from(rom.len()).map_err(BootError::HostSize)?,
            })?;
        if let Some(offset) = expected_prefix
            .iter()
            .zip(actual_prefix)
            .position(|(expected, actual)| expected != actual)
        {
            let expected = *expected_prefix
                .get(offset)
                .ok_or(BootError::InvalidHeaderSlice)?;
            let actual = *rom.get(offset).ok_or(BootError::InvalidHeaderSlice)?;
            return Err(BootError::BootRomMismatch {
                offset,
                expected,
                actual,
            });
        }
        validate_zero(
            rom.get(header_end..payload_start)
                .ok_or(BootError::InvalidHeaderSlice)?,
            header_end,
        )?;
        validate_zero(
            rom.get(payload_end..)
                .ok_or(BootError::InvalidHeaderSlice)?,
            payload_end,
        )?;
        let header = BootImageHeader {
            config,
            image_length: header.image_length,
            entry_offset: header.entry_offset,
            entry,
            checksum: header.checksum,
        };
        Ok(Self {
            config,
            header,
            rom,
            kernel_start: payload_start,
            kernel_end: payload_end,
            boot_instructions: bootloader.instructions,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }
    pub const fn header(&self) -> BootImageHeader {
        self.header
    }
    pub fn entry(&self) -> InstructionAddress {
        self.header.entry()
    }
    pub fn rom(&self) -> &[u8] {
        &self.rom
    }
    pub fn kernel(&self) -> Result<&[u8], BootError> {
        self.rom
            .get(self.kernel_start..self.kernel_end)
            .ok_or(BootError::InvalidImageLength {
                input: self.header.image_length,
            })
    }

    pub fn machine_setup<D: Device>(
        &self,
        devices: DeviceManager<D>,
    ) -> Result<MachineSetup<D>, BootError> {
        if !devices.is_empty() {
            return Err(BootError::UnexpectedDevices {
                count: devices.len(),
            });
        }
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(7)
            .map_err(BootError::Allocation)?;
        regions.push(
            MemoryRegion::rom(
                self.config,
                BOOT_ROM_PHYSICAL_START,
                &self.rom,
                RegionPermissions::new(true, false, true, false),
            )
            .map_err(BootError::Memory)?,
        );
        regions.extend(KernelMemory::regions(self.config).map_err(BootError::OsMemory)?);
        regions.extend(UserMemory::regions(self.config).map_err(BootError::OsMemory)?);
        Ok(MachineSetup {
            config: self.config,
            devices,
            regions,
            pc: RESET_VECTOR,
            sp: VirtualAddress::new(KERNEL_INITIAL_SP),
            status: 0,
            initial_time: CycleCount::new(0),
        })
    }

    pub fn start<D: Device>(
        &self,
        devices: DeviceManager<D>,
    ) -> Result<LazalithMachine<D>, BootError> {
        let mut machine = LazalithMachine::new(self.machine_setup(devices)?)
            .map_err(|source| BootError::Machine(Box::new(source)))?;
        // The one bootloader implementation, shared with `Vm::boot` since B6. Reset
        // here rather than in `boot_into` because `start` is the path that *is*
        // "build and boot from scratch", and a machine it just built has nothing to
        // preserve.
        machine.reset();
        self.execute_bootloader(&mut machine)?;
        Ok(machine)
    }

    /// Runs this image's bootloader on a machine that is already built.
    ///
    /// **B6's reason this is public.** Before B6, booting a machine and building one
    /// were the same call: `start` built the machine and then ran the bootloader, so a
    /// caller who had already built a machine from a *profile* had no way to run a
    /// bootloader on it — and a profile-built machine has devices, which `start`
    /// refuses outright. Splitting the two makes "build it, then boot it" a sequence
    /// rather than a single privileged call.
    ///
    /// `start` is now exactly `machine_setup` followed by this, so there is one
    /// bootloader implementation and Phase-I is unaffected.
    ///
    /// The machine is expected to have the image's ROM loaded and to be at its reset
    /// vector. It is *not* reset here: resetting a machine a caller has just
    /// deliberately configured would discard that configuration, and which of the two
    /// a caller means is not something this can guess.
    pub fn boot_into<D: Device>(
        &self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<InstructionAddress, BootError> {
        self.execute_bootloader(machine)?;
        Ok(self.entry())
    }

    fn execute_bootloader<D: Device>(
        &self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<(), BootError> {
        machine.reset();
        let entry = self.entry();
        let limit = self.execution_limit()?;
        for _ in 0..limit {
            match machine.step() {
                Ok(MachineEvent::Halted) => {
                    return Err(BootError::BootloaderHalted {
                        pc: machine.architectural_state().pc(),
                    });
                }
                Ok(MachineEvent::Stepped { .. }) => {}
                Ok(MachineEvent::Trapped { event }) => {
                    return Err(BootError::UnexpectedBootTrap { event });
                }
                Err(source) => return Err(BootError::Machine(Box::new(source))),
            }
            if machine.architectural_state().pc() == entry {
                self.verify_handoff(machine, entry)?;
                machine
                    .inspect_instruction(entry)
                    .map_err(BootError::Memory)?;
                return Ok(());
            }
        }
        let remaining = machine
            .architectural_state()
            .registers()
            .read_raw(4)
            .map_err(|_| BootError::InvalidImageLength {
                input: self.header.image_length,
            })?;
        Err(BootError::BootStepLimit {
            limit,
            pc: machine.architectural_state().pc(),
            remaining,
        })
    }

    fn verify_handoff<D: Device>(
        &self,
        machine: &LazalithMachine<D>,
        entry: InstructionAddress,
    ) -> Result<(), BootError> {
        let state = machine.architectural_state();
        if state.pc() != entry {
            return Err(BootError::BootHandoffMismatch { field: "pc" });
        }
        if state.sp() != VirtualAddress::new(KERNEL_INITIAL_SP) {
            return Err(BootError::BootHandoffMismatch { field: "sp" });
        }
        let status = state.status().bits();
        if status & (1 << 5) != 0 || status & (1 << 4) != 0 {
            return Err(BootError::BootHandoffMismatch { field: "status" });
        }
        let registers = state.registers();
        let expected = [
            (0, 0),
            (1, KERNEL_LOAD_ADDRESS),
            (2, self.header.image_length),
            (3, entry.as_u64()),
            (15, entry.as_u64()),
        ];
        for (input, value) in expected {
            if registers
                .read_raw(input)
                .map_err(|_| BootError::BootHandoffMismatch { field: "registers" })?
                != value
            {
                return Err(BootError::BootHandoffMismatch { field: "registers" });
            }
        }
        for input in 4..=14 {
            if registers
                .read_raw(input)
                .map_err(|_| BootError::BootHandoffMismatch { field: "registers" })?
                != 0
            {
                return Err(BootError::BootHandoffMismatch { field: "registers" });
            }
        }
        Ok(())
    }

    fn execution_limit(&self) -> Result<u64, BootError> {
        self.header
            .image_length
            .checked_mul(6)
            .and_then(|copy_steps| copy_steps.checked_add(u64::from(self.boot_instructions)))
            .and_then(|steps| steps.checked_add(1))
            .ok_or(BootError::BootLimitOverflow)
    }
}

fn validate_image_length(config: ArchitectureConfig, image_length: u64) -> Result<(), BootError> {
    if image_length == 0 || image_length > KERNEL_IMAGE_LENGTH {
        return Err(BootError::InvalidImageLength {
            input: image_length,
        });
    }
    if config == ArchitectureConfig::lz32() && image_length > u64::from(u32::MAX) {
        return Err(BootError::InvalidImageLength {
            input: image_length,
        });
    }
    if image_length > MAX_BOOT_ROM_PAYLOAD {
        return Err(BootError::PayloadDoesNotFitRom {
            input: image_length,
            maximum: MAX_BOOT_ROM_PAYLOAD,
        });
    }
    Ok(())
}

fn validate_kernel(
    config: ArchitectureConfig,
    kernel: &[u8],
    image_length: u64,
    entry_offset: u64,
) -> Result<InstructionAddress, BootError> {
    validate_image_length(config, image_length)?;
    if u64::try_from(kernel.len()).map_err(BootError::HostSize)? != image_length {
        return Err(BootError::InvalidImageLength {
            input: image_length,
        });
    }
    if config == ArchitectureConfig::lz32() && entry_offset > u64::from(u32::MAX) {
        return Err(BootError::InvalidEntryOffset {
            offset: entry_offset,
            image_length,
        });
    }
    if !entry_offset.is_multiple_of(4) {
        return Err(BootError::InvalidEntryOffset {
            offset: entry_offset,
            image_length,
        });
    }
    let entry_end = entry_offset
        .checked_add(8)
        .ok_or(BootError::InvalidEntryOffset {
            offset: entry_offset,
            image_length,
        })?;
    if entry_end > image_length {
        return Err(BootError::InvalidEntryOffset {
            offset: entry_offset,
            image_length,
        });
    }
    let entry_offset_usize = usize::try_from(entry_offset).map_err(BootError::HostSize)?;
    let entry_end_usize = usize::try_from(entry_end).map_err(BootError::HostSize)?;
    let entry_bytes =
        kernel
            .get(entry_offset_usize..entry_end_usize)
            .ok_or(BootError::InvalidEntryOffset {
                offset: entry_offset,
                image_length,
            })?;
    decode(config, entry_bytes).map_err(BootError::EntryInstruction)?;
    let entry = entry_address(entry_offset)?;
    let mathematical_end =
        entry
            .as_u64()
            .checked_add(7)
            .ok_or(BootError::EntryAddressOverflow {
                offset: entry_offset,
            })?;
    let end = config
        .word_width()
        .checked_access_end(entry.as_u64(), 8)
        .map_err(|source| BootError::EntryWidth {
            entry,
            end: PhysicalAddress::new(mathematical_end),
            source,
        })?;
    let kernel_end = KERNEL_LOAD_ADDRESS.checked_add(KERNEL_IMAGE_LENGTH).ok_or(
        BootError::EntryAddressOverflow {
            offset: entry_offset,
        },
    )?;
    if PhysicalAddress::new(end) >= PhysicalAddress::new(kernel_end) {
        return Err(BootError::EntryOutsideKernel {
            entry,
            end: PhysicalAddress::new(mathematical_end),
        });
    }
    Ok(entry)
}

fn entry_address(entry_offset: u64) -> Result<InstructionAddress, BootError> {
    KERNEL_LOAD_ADDRESS
        .checked_add(entry_offset)
        .map(InstructionAddress::new)
        .ok_or(BootError::EntryAddressOverflow {
            offset: entry_offset,
        })
}

fn parse_header(config: ArchitectureConfig, input: &[u8]) -> Result<ParsedHeader, BootError> {
    if input.len() < usize::from(BOOT_HEADER_SIZE) {
        return Err(BootError::InvalidHeaderSlice);
    }
    if input.get(..BOOT_MAGIC.len()) != Some(BOOT_MAGIC.as_slice()) {
        return Err(BootError::InvalidMagic);
    }
    let header_size = read_u16(input, 14)?;
    if header_size != BOOT_HEADER_SIZE {
        return Err(BootError::InvalidHeaderSize { input: header_size });
    }
    let version = read_u32(input, 8)?;
    if version != BOOT_FORMAT_VERSION {
        return Err(BootError::UnsupportedVersion { input: version });
    }
    let architecture_input = *input.get(12).ok_or(BootError::InvalidHeaderSlice)?;
    let architecture =
        BootArchitecture::from_code(architecture_input).ok_or(BootError::InvalidArchitecture {
            input: architecture_input,
        })?;
    if architecture.config() != config {
        return Err(BootError::ArchitectureMismatch {
            expected: BootArchitecture::from_config(config),
            actual: architecture,
        });
    }
    let flags = *input.get(13).ok_or(BootError::InvalidHeaderSlice)?;
    if flags != 0 {
        return Err(BootError::UnsupportedFlags { input: flags });
    }
    let load_input = read_u64(input, 16)?;
    let load = PhysicalAddress::new(load_input);
    if load != PhysicalAddress::new(KERNEL_LOAD_ADDRESS) {
        return Err(BootError::InvalidLoadAddress { input: load });
    }
    let image_length = read_u64(input, 24)?;
    let entry_offset = read_u64(input, 32)?;
    let checksum = read_u32(input, 40)?;
    let reserved = read_u32(input, 44)?;
    if reserved != 0 {
        return Err(BootError::NonzeroReserved { input: reserved });
    }
    Ok(ParsedHeader {
        image_length,
        entry_offset,
        checksum,
    })
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16, BootError> {
    let end = offset.checked_add(2).ok_or(BootError::InvalidHeaderSlice)?;
    let bytes = input
        .get(offset..end)
        .ok_or(BootError::InvalidHeaderSlice)?
        .try_into()
        .map_err(|_| BootError::InvalidHeaderSlice)?;
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, BootError> {
    let end = offset.checked_add(4).ok_or(BootError::InvalidHeaderSlice)?;
    let bytes = input
        .get(offset..end)
        .ok_or(BootError::InvalidHeaderSlice)?
        .try_into()
        .map_err(|_| BootError::InvalidHeaderSlice)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, BootError> {
    let end = offset.checked_add(8).ok_or(BootError::InvalidHeaderSlice)?;
    let bytes = input
        .get(offset..end)
        .ok_or(BootError::InvalidHeaderSlice)?
        .try_into()
        .map_err(|_| BootError::InvalidHeaderSlice)?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_header(
    output: &mut [u8],
    config: ArchitectureConfig,
    image_length: u64,
    entry_offset: u64,
    checksum: u32,
) -> Result<(), BootError> {
    let mut header = [0u8; 48];
    let magic_end = BOOT_MAGIC.len();
    header[..magic_end].copy_from_slice(&BOOT_MAGIC);
    header[8..12].copy_from_slice(&BOOT_FORMAT_VERSION.to_le_bytes());
    header[12] = BootArchitecture::from_config(config).as_u8();
    header[13] = 0;
    header[14..16].copy_from_slice(&BOOT_HEADER_SIZE.to_le_bytes());
    header[16..24].copy_from_slice(&KERNEL_LOAD_ADDRESS.to_le_bytes());
    header[24..32].copy_from_slice(&image_length.to_le_bytes());
    header[32..40].copy_from_slice(&entry_offset.to_le_bytes());
    header[40..44].copy_from_slice(&checksum.to_le_bytes());
    header[44..48].copy_from_slice(&0u32.to_le_bytes());
    output
        .get_mut(..header.len())
        .ok_or(BootError::InvalidHeaderSlice)?
        .copy_from_slice(&header);
    Ok(())
}

fn ensure_bootloader_fits(bytes: &[u8]) -> Result<(), BootError> {
    let limit = usize::try_from(BOOT_HEADER_ADDRESS).map_err(BootError::HostSize)?;
    if bytes.len() > limit {
        return Err(BootError::BootloaderTooLarge {
            length: bytes.len(),
            limit,
        });
    }
    Ok(())
}

fn validate_zero(input: &[u8], base: usize) -> Result<(), BootError> {
    if let Some(offset) = input.iter().position(|byte| *byte != 0) {
        let offset = base
            .checked_add(offset)
            .ok_or(BootError::InvalidHeaderSlice)?;
        let input = *input
            .get(offset - base)
            .ok_or(BootError::InvalidHeaderSlice)?;
        return Err(BootError::NonzeroReservedByte { offset, input });
    }
    Ok(())
}

fn crc32(input: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in input {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use lazalith_devices::NoDevice;
    use lazalith_isa::{Instruction, Opcode, encode};
    use lazalith_memory::MemoryFaultKind;
    use lazalith_os::{KERNEL_STACK_LENGTH, KERNEL_STACK_START};

    fn halt_kernel(config: ArchitectureConfig) -> Vec<u8> {
        let instruction = Instruction::new(config, Opcode::Halt, &[]).unwrap();
        encode(config, &instruction).unwrap().to_vec()
    }

    fn forge(valid: &BootImage, rom: Vec<u8>) -> BootImage {
        BootImage {
            config: valid.config,
            header: valid.header,
            rom,
            kernel_start: valid.kernel_start,
            kernel_end: valid.kernel_end,
            boot_instructions: valid.boot_instructions,
        }
    }

    fn put_u64(rom: &mut [u8], offset: usize, value: u64) {
        rom[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn crc32_matches_the_iso_hdlc_reference_vector() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn runtime_guards_halt_before_copy_for_invalid_length_and_lz32_underflow() {
        for (config, entry_offset) in [
            (ArchitectureConfig::lz64(), 0),
            (ArchitectureConfig::lz32(), 0xffff_fffc),
        ] {
            let valid = BootImage::new(config, halt_kernel(config), 0).unwrap();
            let mut rom = valid.rom().to_vec();
            let header = BOOT_HEADER_ADDRESS as usize;
            put_u64(
                &mut rom,
                header + 24,
                if config == ArchitectureConfig::lz32() {
                    8
                } else {
                    0
                },
            );
            put_u64(&mut rom, header + 32, entry_offset);
            let forged = forge(&valid, rom);
            let mut machine = LazalithMachine::new(
                forged
                    .machine_setup(DeviceManager::<NoDevice>::new())
                    .unwrap(),
            )
            .unwrap();
            assert!(matches!(
                forged.execute_bootloader(&mut machine),
                Err(BootError::BootloaderHalted { .. })
            ));
            let mut bytes = [0u8; 8];
            machine
                .peek_memory(PhysicalAddress::new(KERNEL_LOAD_ADDRESS), &mut bytes)
                .unwrap();
            assert_eq!(bytes, [0u8; 8]);
        }
    }

    #[test]
    fn entry_fetch_permission_failure_is_reported_after_exact_copy() {
        let config = ArchitectureConfig::lz64();
        let image = BootImage::new(config, halt_kernel(config), 0).unwrap();
        let regions = vec![
            MemoryRegion::rom(
                config,
                BOOT_ROM_PHYSICAL_START,
                image.rom(),
                RegionPermissions::new(true, false, true, false),
            )
            .unwrap(),
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(KERNEL_LOAD_ADDRESS),
                KERNEL_IMAGE_LENGTH,
                RegionPermissions::new(true, true, false, false),
            )
            .unwrap(),
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(KERNEL_STACK_START),
                KERNEL_STACK_LENGTH,
                RegionPermissions::new(true, true, false, false),
            )
            .unwrap(),
        ];
        let mut machine = LazalithMachine::new(MachineSetup {
            config,
            devices: DeviceManager::<NoDevice>::new(),
            regions,
            pc: RESET_VECTOR,
            sp: VirtualAddress::new(KERNEL_INITIAL_SP),
            status: 0,
            initial_time: CycleCount::new(0),
        })
        .unwrap();
        assert!(matches!(
            image.execute_bootloader(&mut machine),
            Err(BootError::Memory(source))
                if matches!(source.kind, MemoryFaultKind::Permission { .. })
        ));
        assert_eq!(machine.state(), lazalith_machine::MachineState::Reset);
        assert_eq!(machine.architectural_state().pc(), image.entry());
        let mut bytes = [0u8; 8];
        machine
            .peek_memory(PhysicalAddress::new(KERNEL_LOAD_ADDRESS), &mut bytes)
            .unwrap();
        assert_eq!(bytes.as_slice(), halt_kernel(config).as_slice());
    }
}
