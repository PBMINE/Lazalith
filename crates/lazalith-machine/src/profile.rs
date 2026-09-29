//! B4: machine profiles.
//!
//! # What a profile is
//!
//! `binstruction.md` §27 asks for versioned profiles — `lza64-virt-v1`,
//! `lza64-native-v1`, `lza64-at-v1` — that describe a machine's architecture, CPU,
//! RAM, firmware, boot behaviour, interrupt model, timer, device inventory,
//! MMIO/PIO map, display, input, storage, serial, network, audio, and compatibility
//! behaviour. It says profiles "must be versioned to preserve guest
//! compatibility".
//!
//! So a profile is **the statement of what a machine is**, addressed by a name and
//! a version, and it is the only place in the platform that answers that question.
//! Before it, the answer was scattered: the boot ROM's geometry was constants in
//! `lazalith-boot`, the physical RAM extent was a constant in `lazalith-os` that
//! nothing built a region from, the trap vector was set after construction by each
//! caller, and the device set was whatever the caller happened to hand over.
//!
//! # What a profile is not
//!
//! It is not firmware, and it does not contain firmware. A profile says *where*
//! firmware lives and *where the reset vector points*; the bytes are an image, and
//! the boot path supplies them. Keeping those apart is what lets one machine run a
//! different kernel without the machine changing — which is the whole reason a
//! profile is a profile and not a machine.
//!
//! Nor is it an operating system. The LazOS kernel and user region layout stays in
//! `lazalith-os`, layered over the machine the profile describes, exactly as it is
//! today. `lazalith-machine` must not depend on the OS, and a profile that carried
//! the OS's region layout would be that dependency by another name.
//!
//! # The device inventory, and why it is a list
//!
//! §27 lists the MMIO/PIO map *and* display, input, storage, serial, network and
//! audio as separate things. They are not separate: a display is a device at an
//! address. So the profile has one `devices` list, and the class queries
//! ([`MachineProfile::device_of`], [`MachineProfile::devices_of`]) are how a caller
//! asks "where is the display". A profile that also had a `display:` field would
//! be able to describe two different machines, and the disagreement would not be a
//! build error — it would be a machine that boots and then draws nowhere.
//!
//! The list is a list and not a map because a machine may have two displays. Which
//! one is "the" display is a decision for the guest, and a host-side map would have
//! to make it.

use alloc::{boxed::Box, vec::Vec};
use core::{error::Error, fmt};
use lazalith_devices::{
    BackendError, BlockDevice, ConsoleDevice, CopyOnWriteBlockBackend, Device, DeviceError,
    DeviceId, DeviceManager, DisplayDevice, InputDevice, MemoryBlockBackend, TimerDevice,
};
use lazalith_isa::{ControlRegister, ISA_VERSION};
use lazalith_memory::{MemoryFault, MemoryRegion, RegionKind, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress, WordWidth,
};

use crate::{LazalithMachine, MachineError, MachineSetup};

/// The fixed physical layout of an LZA machine.
///
/// These are the values `lazalith-boot` and `lazalith-os` used to declare
/// separately — and in two cases used to declare *twice*. They are the machine's
/// geometry: where firmware lives, where the reset vector points, where a kernel
/// is loaded, and how much RAM there is. A profile names them; it does not
/// compute them, because a layout is a fact about the architecture rather than a
/// choice a profile makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineLayout {
    /// Where the boot ROM starts, and therefore the reset vector.
    pub boot_rom_start: u64,
    /// How large the boot ROM is.
    pub boot_rom_length: u64,
    /// Where inside the ROM the boot header lives.
    pub boot_header_address: u64,
    /// Where inside the ROM a kernel payload is staged.
    pub kernel_payload_address: u64,
    /// The largest payload the ROM format can carry.
    pub max_boot_rom_payload: u64,
    /// Where a kernel is loaded into RAM, and where execution resumes.
    pub kernel_load_address: u64,
    /// The window reserved for a kernel image.
    pub kernel_image_length: u64,
    /// The stack pointer a kernel starts with.
    pub kernel_initial_sp: u64,
    /// Where a trap lands: the supervisor entry point a fault or a syscall goes to.
    ///
    /// Part of the layout rather than a field a caller sets on a built machine,
    /// because B4 recorded that it was the one piece of the interrupt model a
    /// profile could not express 2014 and an interrupt vector that only exists after
    /// construction is an interrupt vector whose value depends on who constructed the
    /// machine. A guest faulting before anyone set it has nowhere to go.
    pub trap_vector: u64,
    /// Where physical RAM starts.
    pub physical_ram_start: u64,
    /// How much physical RAM there is.
    pub physical_ram_length: u64,
}

/// The LZA64 layout, which every LZA64 machine shares.
///
/// The values are exactly the ones `lazalith-boot` declared before B4, and the two
/// that `lazalith-os` also declared — `KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP`
/// — were the same numbers declared twice. `lazalith-boot` and `lazalith-os` now
/// re-export these, so there is one definition rather than two that happen to
/// agree today.
pub const LZA64_LAYOUT: MachineLayout = MachineLayout {
    boot_rom_start: 0x0000_0000,
    boot_rom_length: 0x0008_0000,
    boot_header_address: 0x0000_0400,
    kernel_payload_address: 0x0000_1000,
    max_boot_rom_payload: 0x0007_f000,
    kernel_load_address: 0x0010_0000,
    kernel_image_length: 0x0008_0000,
    kernel_initial_sp: 0x0018_f000,
    trap_vector: 0x0000_0800,
    physical_ram_start: 0x0010_0000,
    physical_ram_length: 0x0031_0000,
};

/// Which family of machine a profile describes.
///
/// The three are `binstruction.md` §27's names, minus the architecture and version
/// that [`ProfileName`] carries.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ProfileFamily {
    /// The general-purpose machine the platform's own software targets.
    Virt,
    /// CPU, memory, console and timer. What a bootloader or a freestanding kernel
    /// is built against.
    Native,
    /// The compatibility machine. Not implemented; reserved so a profile name can
    /// be written down and referred to before the device set exists.
    At,
}

impl ProfileFamily {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Virt => "virt",
            Self::Native => "native",
            Self::At => "at",
        }
    }
}

impl fmt::Display for ProfileFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A profile's name: architecture, family, version.
///
/// The version is part of the identity rather than metadata beside it, because
/// `binstruction.md` §27 requires versioning *to preserve guest compatibility*: a
/// change any guest could observe is a new version, and a name that omitted the
/// version could not say which one it was. Making it a field rather than a string
/// also means a profile cannot be called `lza64-native-v1` and then describe an LZ32
/// machine, which is the kind of disagreement that is otherwise only found by a
/// guest.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ProfileName {
    architecture: ArchitectureConfig,
    family: ProfileFamily,
    version: u16,
}

impl ProfileName {
    pub const fn new(
        architecture: ArchitectureConfig,
        family: ProfileFamily,
        version: u16,
    ) -> Self {
        Self {
            architecture,
            family,
            version,
        }
    }

    pub const fn architecture(self) -> ArchitectureConfig {
        self.architecture
    }
    pub const fn family(self) -> ProfileFamily {
        self.family
    }
    pub const fn version(self) -> u16 {
        self.version
    }
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let width = match self.architecture.word_width() {
            WordWidth::W32 => "lza32",
            WordWidth::W64 => "lza64",
        };
        write!(f, "{width}-{}-v{}", self.family, self.version)
    }
}

/// What kind of device an inventory entry is.
///
/// The class is what a caller asks about ("where is the display"); the address is
/// what the machine routes by. Keeping them in one entry is the point — see the
/// module comment.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DeviceClass {
    Console,
    Timer,
    Display,
    Input,
    Serial,
    Block,
    Network,
    Audio,
    /// A device this build has no constructor for.
    ///
    /// It is here so a profile can *describe* a device a later stage will provide,
    /// and so `from_profile` refuses it clearly instead of skipping it and building
    /// a machine that is missing something the profile promised.
    Other,
}

impl DeviceClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Timer => "timer",
            Self::Display => "display",
            Self::Input => "input",
            Self::Serial => "serial",
            Self::Block => "block",
            Self::Network => "network",
            Self::Audio => "audio",
            Self::Other => "other",
        }
    }

    /// Whether this build can construct a device of this class.
    pub const fn is_constructible(self) -> bool {
        matches!(
            self,
            Self::Console | Self::Timer | Self::Display | Self::Input | Self::Block
        )
    }
}

impl fmt::Display for DeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What is behind a block device, when the profile names one.
///
/// `binstruction.md` §26's example is a *chain*: a guest block device over a host
/// storage backend, and §31 asks for raw, sparse and copy-on-write. A profile
/// says which chain a machine has, because which chain it has is a property of the
/// machine — a machine with a copy-on-write overlay and a machine with a writable
/// flat image behave differently for the same guest program, and a caller
/// assembling a machine has to be able to name the difference.
///
/// It is a field on the device rather than a separate profile section because a
/// profile that listed devices in one place and their storage in another could
/// describe a machine where a device had no storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockStorage {
    /// A flat image of `bytes`, held by the host and writable.
    Memory {
        /// How many bytes the image holds. A whole number of sectors.
        bytes: u64,
    },
    /// A sparse overlay over a read-only image of `base` bytes.
    ///
    /// Reads fall through to the base where a sector has not been written; writes
    /// allocate in the overlay and the base is never modified. The overlay presents
    /// the base's capacity — an overlay is not a way to make a disk bigger, and a
    /// profile that wanted a bigger disk should name a bigger base.
    CopyOnWrite {
        /// How many bytes the read-only base holds. A whole number of sectors.
        base: u64,
    },
}

impl BlockStorage {
    /// The whole number of sectors this storage presents.
    pub const fn sectors(&self) -> u64 {
        match *self {
            Self::Memory { bytes } => bytes / lazalith_devices::SECTOR_BYTES,
            Self::CopyOnWrite { base } => base / lazalith_devices::SECTOR_BYTES,
        }
    }

    fn build(self) -> Result<Box<dyn lazalith_devices::BlockBackend>, BackendError> {
        match self {
            Self::Memory { bytes } => Ok(Box::new(MemoryBlockBackend::new(bytes)?)),
            Self::CopyOnWrite { base } => {
                // The base is read-only *by construction*, and that is what makes
                // the chain copy-on-write rather than a stack of layers that
                // happens not to write through.
                let base = MemoryBlockBackend::new(base)?.read_only();
                Ok(Box::new(CopyOnWriteBlockBackend::new(Box::new(base))?))
            }
        }
    }
}

/// One entry in a machine's device inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceProfile {
    /// The id the machine routes by. Unique within a profile.
    pub id: DeviceId,
    /// What kind of device it is.
    pub class: DeviceClass,
    /// Where it is mapped.
    pub address: PhysicalAddress,
    /// What a guest may do with it.
    ///
    /// Execution is always refused: `Bus::map_device` rejects an executable device
    /// window, and a profile that asked for one would describe a machine that
    /// cannot exist.
    pub permissions: RegionPermissions,
    /// Console output capacity in bytes. Unused by other classes.
    pub console_capacity: usize,
    /// The storage behind a block device. `None` for every other class.
    pub block: Option<BlockStorage>,
}

impl DeviceProfile {
    pub const fn new(
        id: DeviceId,
        class: DeviceClass,
        address: PhysicalAddress,
        permissions: RegionPermissions,
    ) -> Self {
        Self {
            id,
            class,
            address,
            permissions,
            console_capacity: 0,
            block: None,
        }
    }

    /// The same entry, with a console output capacity.
    pub const fn with_console_capacity(mut self, capacity: usize) -> Self {
        self.console_capacity = capacity;
        self
    }

    /// The storage this device names, if it is a block device.
    ///
    /// Returns `None` for every other class, so a caller that has a device entry and
    /// wants its storage has to ask the right question rather than reach for a
    /// field.
    pub const fn block_storage(&self) -> Option<BlockStorage> {
        self.block
    }

    /// The same entry, with the storage behind a block device.
    ///
    /// The counterpart to `with_console_capacity`: the other per-class fact a
    /// device entry can carry that a caller has to be able to name.
    pub const fn with_block_storage(mut self, block: BlockStorage) -> Self {
        self.block = Some(block);
        self
    }
}

/// One region of the machine's physical memory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegionProfile {
    pub start: PhysicalAddress,
    pub length: u64,
    pub kind: RegionKind,
    pub permissions: RegionPermissions,
}

/// What kind of compatibility hardware a machine presents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Compatibility {
    /// Native LZA hardware only. The default, and the only value this build has
    /// machines for.
    #[default]
    Native,
    /// The AT-derived compatibility machine. Described so `lza64-at-v1` can be
    /// written down and referred to; no device of this kind exists.
    At,
}

/// Everything `binstruction.md` §27 asks a profile to describe.
///
/// A profile is data. It is validated before a machine is built from it, and the
/// machine it builds is checked against it, so a profile that describes something
/// the machine cannot be is a refused profile rather than a machine that boots and
/// then misbehaves.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineProfile {
    name: ProfileName,
    isa_version: u16,
    layout: MachineLayout,
    regions: Vec<RegionProfile>,
    devices: Vec<DeviceProfile>,
    timer: Option<CycleCount>,
    compatibility: Compatibility,
}

impl MachineProfile {
    /// A profile with the given name and the LZA64 layout, and nothing else.
    pub fn empty(name: ProfileName, layout: MachineLayout) -> Self {
        Self {
            name,
            isa_version: ISA_VERSION,
            layout,
            regions: Vec::new(),
            devices: Vec::new(),
            timer: None,
            compatibility: Compatibility::Native,
        }
    }

    /// `lza64-native-v1` — CPU, RAM, a boot ROM, a console and a timer.
    ///
    /// This is the first real profile, and it is the minimal one on purpose: §53
    /// places machine profiles before the target-side OS, and a freestanding kernel
    /// (B25, B26) is built against a machine with nothing on it but a CPU and
    /// memory. A profile whose default shape included a display would mean every
    /// kernel target had to opt out of a device it never asked for, and opting out
    /// is how a guest quietly acquires a dependency on a device that is not always
    /// there.
    pub fn lza64_native_v1() -> Self {
        let layout = LZA64_LAYOUT;
        let mut profile = Self::empty(
            ProfileName::new(ArchitectureConfig::lz64(), ProfileFamily::Native, 1),
            layout,
        );
        // The boot ROM: executable, not writable, not user-accessible. It is
        // created empty and filled by the boot path, because the profile describes
        // the machine and the image is not the machine.
        profile.regions.push(RegionProfile {
            start: PhysicalAddress::new(layout.boot_rom_start),
            length: layout.boot_rom_length,
            kind: RegionKind::Rom,
            permissions: RegionPermissions::new(true, false, true, false),
        });
        profile.regions.push(RegionProfile {
            start: PhysicalAddress::new(layout.physical_ram_start),
            length: layout.physical_ram_length,
            permissions: RegionPermissions::new(true, true, false, true),
            kind: RegionKind::Ram,
        });
        profile.devices.push(
            DeviceProfile::new(
                DeviceId::new(1),
                DeviceClass::Console,
                PhysicalAddress::new(0x4000_0000),
                RegionPermissions::new(true, true, false, true),
            )
            .with_console_capacity(64 * 1024),
        );
        profile.devices.push(DeviceProfile::new(
            DeviceId::new(2),
            DeviceClass::Timer,
            PhysicalAddress::new(0x4000_1000),
            RegionPermissions::new(true, true, false, true),
        ));
        profile.timer = Some(CycleCount::new(1_000));
        profile
    }

    /// The profile's name, which carries its version.
    pub const fn name(&self) -> ProfileName {
        self.name
    }
    /// The ISA version this profile describes.
    ///
    /// This is the compatibility claim made concrete. A profile is versioned so a
    /// guest can rely on the machine being the machine it booted on, and the way
    /// that promise is checked at build time is by refusing a profile whose ISA
    /// version this build does not implement. Without it, "versioned" would be a
    /// label and the only thing that would notice a mismatch is a guest that
    /// misbehaves.
    pub const fn isa_version(&self) -> u16 {
        self.isa_version
    }
    /// The same profile, named at a different version.
    ///
    /// Exists so the refusal below can be reached and tested rather than being an
    /// assertion about a value nothing can change.
    pub fn with_version(mut self, version: u16) -> Self {
        self.name = ProfileName::new(self.name.architecture(), self.name.family(), version);
        self
    }

    /// The same profile, built against a different ISA version.
    ///
    /// Same reason as [`Self::with_version`].
    pub fn with_isa_version(mut self, isa_version: u16) -> Self {
        self.isa_version = isa_version;
        self
    }
    pub const fn layout(&self) -> MachineLayout {
        self.layout
    }
    pub const fn compatibility(&self) -> Compatibility {
        self.compatibility
    }
    pub const fn timer_period(&self) -> Option<CycleCount> {
        self.timer
    }
    pub fn regions(&self) -> &[RegionProfile] {
        &self.regions
    }
    pub fn devices(&self) -> &[DeviceProfile] {
        &self.devices
    }

    /// Adds a region.
    pub fn with_region(mut self, region: RegionProfile) -> Self {
        self.regions.push(region);
        self
    }

    /// Adds a device to the inventory.
    pub fn with_device(mut self, device: DeviceProfile) -> Self {
        self.devices.push(device);
        self
    }

    /// The same profile, on a different machine layout.
    ///
    /// The layout is the *shape* of the machine — where its ROM is, where a kernel
    /// loads, where its RAM starts — as opposed to the regions and devices that fill
    /// it. A profile that changes its layout has usually changed what machine it is
    /// for, and the regions added before the change may no longer fit it.
    ///
    /// This exists as a builder rather than a public field because the regions and
    /// devices a profile carries are not re-validated against the new layout. A caller
    /// that swaps the layout under a populated profile gets a profile that
    /// `validate` will refuse if the two disagree, and one it will not refuse if they
    /// happen not to — which is the honest behaviour for a value that is meant to be
    /// built by hand.
    pub fn with_layout(mut self, layout: MachineLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Declares what kind of compatibility hardware this machine presents.
    ///
    /// Declaring `At` does not make a machine buildable. It is here so
    /// `lza64-at-v1` can be written down and referred to before the device set
    /// exists, and so asking for one is a refusal with a reason rather than a
    /// profile that quietly has no VGA in it.
    pub fn with_compatibility(mut self, compatibility: Compatibility) -> Self {
        self.compatibility = compatibility;
        self
    }

    /// The first device of a class, which is what "where is the display" means.
    pub fn device_of(&self, class: DeviceClass) -> Option<&DeviceProfile> {
        self.devices.iter().find(|device| device.class == class)
    }

    /// Every device of a class. A machine may have more than one display.
    pub fn devices_of(&self, class: DeviceClass) -> impl Iterator<Item = &DeviceProfile> {
        self.devices
            .iter()
            .filter(move |device| device.class == class)
    }

    /// Where a class is mapped, or `None` if this machine has no such device.
    pub fn address_of(&self, class: DeviceClass) -> Option<PhysicalAddress> {
        self.device_of(class).map(|device| device.address)
    }

    /// The reset vector this machine boots at.
    pub fn reset_vector(&self) -> InstructionAddress {
        InstructionAddress::new(self.layout.boot_rom_start)
    }

    /// The stack pointer a kernel starts with.
    pub fn kernel_stack_pointer(&self) -> VirtualAddress {
        VirtualAddress::new(self.layout.kernel_initial_sp)
    }

    /// Where a trap lands on this machine.
    ///
    /// Read from the layout rather than stored, so the vector a profile reports and
    /// the vector a machine is handed cannot be two different values from two places.
    pub const fn trap_vector(&self) -> InstructionAddress {
        InstructionAddress::new(self.layout.trap_vector)
    }

    /// Everything wrong with this profile, or `Ok(())`.
    ///
    /// Validated *before* a machine is built, so a bad profile is a refusal rather
    /// than a half-built machine. The checks are the ones whose failure would
    /// otherwise produce a machine that boots and then misbehaves: an unversioned
    /// profile, a profile for an ISA this build does not implement, two devices at
    /// one id, two devices overlapping, a device whose window the architecture cannot
    /// address, an executable device window, and a device class this build cannot
    /// construct.
    ///
    /// **What is deliberately not checked:** whether a region's permissions make sense.
    /// The memory model builds what it is given — writable and executable RAM is
    /// constructible, and the Phase-I OS tests use it for code — so a profile that
    /// refused it would be inventing a policy the platform does not have, and would
    /// refuse to describe machines that genuinely exist.
    pub fn validate(&self) -> Result<(), ProfileError> {
        if self.name.architecture().word_width() == WordWidth::W32 && self.layout != LZA64_LAYOUT {
            return Err(ProfileError::ArchitectureMismatch {
                declared: self.name.architecture(),
                layout_is: "lza64",
            });
        }
        if self.name.version() == 0 {
            return Err(ProfileError::Unversioned);
        }
        if self.isa_version != ISA_VERSION {
            return Err(ProfileError::IsaVersionMismatch {
                profile: self.isa_version,
                build: ISA_VERSION,
            });
        }
        if self.name.family() == ProfileFamily::At && self.compatibility != Compatibility::At {
            return Err(ProfileError::CompatibilityMismatch {
                family: self.name.family(),
                compatibility: self.compatibility,
            });
        }
        if self.name.family() != ProfileFamily::At && self.compatibility == Compatibility::At {
            return Err(ProfileError::CompatibilityMismatch {
                family: self.name.family(),
                compatibility: self.compatibility,
            });
        }
        if self.compatibility == Compatibility::At {
            return Err(ProfileError::UnimplementedCompatibility {
                compatibility: self.compatibility,
            });
        }
        let width = self.name.architecture().word_width();
        let mut previous_end: Option<PhysicalAddress> = None;
        for region in &self.regions {
            if region.length == 0 {
                return Err(ProfileError::EmptyRegion {
                    start: region.start,
                });
            }
            let end = width
                .checked_access_end(region.start.as_u64(), region.length)
                .map_err(|source| ProfileError::RegionOutOfRange {
                    start: region.start,
                    length: region.length,
                    source,
                })?;
            let end = PhysicalAddress::new(end);
            if let Some(earlier) = previous_end
                && region.start <= earlier
            {
                return Err(ProfileError::RegionOverlap {
                    start: region.start,
                    end,
                    conflicts_with: earlier,
                });
            }
            previous_end = Some(end);
        }
        let mut ids: Vec<DeviceId> = Vec::new();
        for device in &self.devices {
            if ids.contains(&device.id) {
                return Err(ProfileError::DuplicateDeviceId { id: device.id });
            }
            ids.push(device.id);
            if !device.class.is_constructible() {
                return Err(ProfileError::UnconstructibleDevice {
                    class: device.class,
                    id: device.id,
                });
            }
            if device.permissions.execute {
                return Err(ProfileError::ExecutableDevice {
                    id: device.id,
                    address: device.address,
                });
            }
            if device.class == DeviceClass::Block {
                let storage = device.block_storage().ok_or(ProfileError::BlockStorage {
                    id: device.id,
                    detail: "a block device with no storage behind it",
                })?;
                let bytes = match storage {
                    BlockStorage::Memory { bytes } | BlockStorage::CopyOnWrite { base: bytes } => {
                        bytes
                    }
                };
                if bytes == 0 {
                    return Err(ProfileError::BlockStorage {
                        id: device.id,
                        detail: "a block device with a zero-byte disk",
                    });
                }
                if bytes % lazalith_devices::SECTOR_BYTES != 0 {
                    return Err(ProfileError::BlockStorage {
                        id: device.id,
                        detail: "a disk that is not a whole number of 512-byte sectors",
                    });
                }
            } else if device.block.is_some() {
                return Err(ProfileError::BlockStorage {
                    id: device.id,
                    detail: "only a block device has storage",
                });
            }
        }
        for (index, device) in self.devices.iter().enumerate() {
            for other in &self.devices[index + 1..] {
                let length = device_window_length(device);
                let Some(end) = width
                    .checked_access_end(device.address.as_u64(), length)
                    .ok()
                else {
                    return Err(ProfileError::DeviceOutOfRange {
                        id: device.id,
                        address: device.address,
                        length,
                    });
                };
                let end = PhysicalAddress::new(end);
                let other_end = width
                    .checked_access_end(other.address.as_u64(), length)
                    .map(PhysicalAddress::new)
                    .unwrap_or(PhysicalAddress::new(u64::MAX));
                if device.address <= other_end && other.address <= end {
                    return Err(ProfileError::DeviceOverlap {
                        first: device.id,
                        second: other.id,
                    });
                }
            }
        }
        Ok(())
    }

    /// The `MachineSetup` this profile describes, with its devices built.
    ///
    /// A profile's devices are built into an erased set, because a profile may name
    /// more than one kind of device and the device manager holds one concrete type.
    /// That is the whole reason `DeviceManager` is used here rather than a single
    /// device: `Box<dyn Device>` is a `Device`, so the rest of the machine is
    /// written once and does not know which is which.
    pub fn machine_setup(&self) -> Result<MachineSetup<Box<dyn Device>>, ProfileError> {
        let config = self.name().architecture();
        let devices = self.build_devices()?;
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(self.regions.len())
            .map_err(|_| ProfileError::Allocation)?;
        for region in &self.regions {
            let length = usize::try_from(region.length).unwrap_or(0);
            let built = match region.kind {
                RegionKind::Rom => {
                    // Empty on purpose: `lza64_native_v1` says the profile describes
                    // the machine and a boot image is not the machine, so the ROM
                    // window exists and holds nothing until firmware is installed.
                    //
                    // Which is also why it is mapped **read-only**: the guest cannot
                    // write its own firmware, and a profile-built machine has no way to
                    // install any. B6 boots a machine by building it from the *image's*
                    // memory and this profile's devices, because a read-only ROM is a
                    // window and not a place firmware arrives.
                    let mut bytes = alloc::vec::Vec::new();
                    bytes
                        .try_reserve_exact(length)
                        .map_err(|_| ProfileError::Allocation)?;
                    bytes.resize(length, 0);
                    MemoryRegion::rom(config, region.start, &bytes, region.permissions)
                }
                RegionKind::Ram => {
                    MemoryRegion::ram(config, region.start, region.length, region.permissions)
                }
            }
            .map_err(|source| ProfileError::Memory {
                start: region.start,
                source,
            })?;
            regions.push(built);
        }
        Ok(MachineSetup {
            config,
            devices,
            regions,
            pc: self.reset_vector(),
            sp: self.kernel_stack_pointer(),
            status: 0,
            initial_time: CycleCount::new(0),
        })
    }

    /// The devices of this profile, and nothing else.
    ///
    /// B6 made this necessary and the reason is the boot contract. A boot image owns a
    /// machine.s *memory* 2014 a kernel image, a kernel stack and a user region with the
    /// OS.s own permissions, which are not the same permissions a profile.s flat RAM
    /// has 2014 and it refuses a device manager outright, because a boot image does not
    /// know what devices a machine has. A profile owns its *devices*.
    ///
    /// So a machine that both boots and has devices is assembled from the two, and this
    /// is the half of it that comes from the profile. `machine_setup` is the other
    /// half: the whole profile-built machine, for a caller that does not need an image.
    ///
    /// Validates first, so a profile that could not be built as a whole is refused here
    /// too rather than producing a device set that a later `map_device` would reject.
    pub fn build_devices(&self) -> Result<DeviceManager<Box<dyn Device>>, ProfileError> {
        self.validate()?;
        let mut devices = DeviceManager::new();
        for device in &self.devices {
            let built: Box<dyn Device> = match device.class {
                DeviceClass::Console => Box::new(
                    ConsoleDevice::new(device.console_capacity).map_err(|source| {
                        ProfileError::Device {
                            id: device.id,
                            source,
                        }
                    })?,
                ),
                DeviceClass::Timer => Box::new(TimerDevice::new()),
                DeviceClass::Display => Box::new(DisplayDevice::new()),
                DeviceClass::Input => Box::new(InputDevice::new()),
                DeviceClass::Block => {
                    // The backend is built here and moved into the device, so the
                    // device is the only thing the machine holds. A machine that
                    // also held the backend would be holding the storage twice:
                    // once behind the device and once beside it, and the copy beside
                    // it would be the one that went stale.
                    let backend = device
                        .block_storage()
                        .ok_or(ProfileError::BlockStorage {
                            id: device.id,
                            detail: "a block device with no storage behind it",
                        })?
                        .build()
                        .map_err(|source| ProfileError::Storage {
                            id: device.id,
                            source,
                        })?;
                    Box::new(BlockDevice::new(backend))
                }
                DeviceClass::Serial
                | DeviceClass::Network
                | DeviceClass::Audio
                | DeviceClass::Other => {
                    return Err(ProfileError::UnconstructibleDevice {
                        class: device.class,
                        id: device.id,
                    });
                }
            };
            devices
                .insert(device.id, built)
                .map_err(|source| ProfileError::Device {
                    id: device.id,
                    source,
                })?;
        }
        Ok(devices)
    }
}

/// How long a device's register window is, from the class.
///
/// A profile does not carry a length because a device's length is a property of
/// the device, not a choice: a profile that named one could disagree with the
/// device it is describing, and the bus would map a window of the wrong size. So
/// the size is read from the same constant the device's own `address_len` returns,
/// and the two cannot drift.
const fn device_window_length(device: &DeviceProfile) -> u64 {
    match device.class {
        DeviceClass::Console => lazalith_devices::CONSOLE_REGISTER_BYTES,
        DeviceClass::Timer => lazalith_devices::TIMER_REGISTER_BYTES,
        DeviceClass::Display => lazalith_devices::DISPLAY_REGISTER_BYTES,
        DeviceClass::Input => lazalith_devices::INPUT_REGISTER_BYTES,
        DeviceClass::Block => lazalith_devices::BLOCK_REGISTER_BYTES,
        DeviceClass::Serial | DeviceClass::Network | DeviceClass::Audio | DeviceClass::Other => 0,
    }
}

/// What a profile refuses.
///
/// Every variant is a *decision* the profile made that the machine cannot honour,
/// named so that a caller building one is told which part of it to change rather
/// than just that something was wrong.
#[derive(Debug)]
pub enum ProfileError {
    /// The profile has no version, so it cannot preserve a guest's compatibility.
    Unversioned,
    /// The profile describes an ISA this build does not implement.
    IsaVersionMismatch {
        /// The version the profile names.
        profile: u16,
        /// The version this build implements.
        build: u16,
    },
    /// The architecture in the name is not the architecture the layout describes.
    ArchitectureMismatch {
        declared: ArchitectureConfig,
        layout_is: &'static str,
    },
    /// The family and the compatibility behaviour disagree.
    CompatibilityMismatch {
        family: ProfileFamily,
        compatibility: Compatibility,
    },
    /// The profile asks for compatibility hardware this build has none of.
    UnimplementedCompatibility { compatibility: Compatibility },
    /// A region with no length.
    EmptyRegion { start: PhysicalAddress },
    /// A region the architecture cannot address.
    RegionOutOfRange {
        start: PhysicalAddress,
        length: u64,
        source: lazalith_types::WidthError,
    },
    /// Two regions that overlap.
    RegionOverlap {
        start: PhysicalAddress,
        end: PhysicalAddress,
        conflicts_with: PhysicalAddress,
    },
    /// Two devices with the same id.
    DuplicateDeviceId { id: DeviceId },
    /// Two devices whose windows overlap.
    DeviceOverlap { first: DeviceId, second: DeviceId },
    /// A device window the architecture cannot address.
    DeviceOutOfRange {
        id: DeviceId,
        address: PhysicalAddress,
        length: u64,
    },
    /// A device window a guest could execute. `Bus::map_device` refuses these.
    ExecutableDevice {
        id: DeviceId,
        address: PhysicalAddress,
    },
    /// A device class this build has no constructor for.
    UnconstructibleDevice { class: DeviceClass, id: DeviceId },
    /// The host storage a block device asked for was refused.
    ///
    /// Distinct from `UnconstructibleDevice` because the class *is* constructible:
    /// this is the storage refusing, which is a fact about the disk and not about
    /// the build.
    Storage {
        /// The device whose storage was refused.
        id: DeviceId,
        /// What the storage said.
        source: BackendError,
    },
    /// A block device with no storage, or with storage that is not a whole number
    /// of sectors.
    ///
    /// A refusal rather than a rounding, because a disk of 513 bytes does not become
    /// a disk of one sector by being rounded down: a guest would see a capacity the
    /// profile never promised.
    BlockStorage { id: DeviceId, detail: &'static str },
    /// The machine was built, but it is not the machine the profile describes.
    ///
    /// A round-trip mismatch, and the reason it is a distinct case is that it is
    /// the only failure that can happen *after* a machine exists. Everything else
    /// is a refusal to build one.
    ProfileMismatch {
        /// Which part of the machine disagreed.
        detail: &'static str,
    },
    /// A device that refused to be created.
    Device { id: DeviceId, source: DeviceError },
    /// A device window the machine refused to map.
    ///
    /// Distinct from a memory region being refused: a region is asked for by
    /// `MemoryRegion::..`, and a window is asked for by the bus, which has its own
    /// reasons to refuse that the region builder does not have.
    DeviceMapping {
        id: DeviceId,
        address: PhysicalAddress,
        source: MachineError,
    },
    /// A region the memory model refused.
    Memory {
        start: PhysicalAddress,
        source: MemoryFault,
    },
    /// The machine refused the profile's own setup.
    Machine(MachineError),
    /// An allocation failed while building the profile.
    Allocation,
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unversioned => {
                f.write_str("a profile with no version cannot preserve guest compatibility")
            }
            Self::IsaVersionMismatch { profile, build } => write!(
                f,
                "the profile describes ISA version {profile}, and this build implements {build}"
            ),
            Self::ArchitectureMismatch {
                declared,
                layout_is,
            } => write!(
                f,
                "the profile is named {declared:?} but its layout is the {layout_is} layout"
            ),
            Self::CompatibilityMismatch {
                family,
                compatibility,
            } => write!(
                f,
                "a {family} profile cannot declare {compatibility:?} compatibility hardware"
            ),
            Self::UnimplementedCompatibility { compatibility } => write!(
                f,
                "{compatibility:?} compatibility hardware is not implemented in this build"
            ),
            Self::EmptyRegion { start } => {
                write!(f, "the region at {start:?} has no length")
            }
            Self::RegionOutOfRange {
                start,
                length,
                source,
            } => write!(f, "the region at {start:?} of {length} bytes: {source}"),
            Self::RegionOverlap {
                start,
                end,
                conflicts_with,
            } => write!(
                f,
                "the region at {start:?}..{end:?} overlaps the region ending at {conflicts_with:?}"
            ),
            Self::DuplicateDeviceId { id } => write!(f, "two devices share the id {id:?}"),
            Self::DeviceOverlap { first, second } => {
                write!(f, "the windows of {first:?} and {second:?} overlap")
            }
            Self::DeviceOutOfRange {
                id,
                address,
                length,
            } => write!(f, "{id:?} at {address:?} of {length} bytes is out of range"),
            Self::ExecutableDevice { id, address } => {
                write!(f, "{id:?} at {address:?} has an executable window")
            }
            Self::UnconstructibleDevice { class, id } => {
                write!(f, "this build cannot construct a {class} device for {id:?}")
            }
            Self::Storage { id, source } => {
                write!(f, "the storage for device {id:?} was refused: {source}")
            }
            Self::BlockStorage { id, detail } => {
                write!(f, "the storage for device {id:?} is not usable: {detail}")
            }
            Self::ProfileMismatch { detail } => {
                write!(
                    f,
                    "the machine is not the one the profile describes: {detail}"
                )
            }
            Self::Device { id, source } => write!(f, "{id:?} could not be created: {source}"),
            Self::DeviceMapping {
                id,
                address,
                source,
            } => write!(f, "{id:?} at {address:?} could not be mapped: {source}"),
            Self::Memory { start, source } => {
                write!(f, "the region at {start:?} was refused: {source}")
            }
            Self::Machine(source) => write!(f, "the machine refused the profile: {source}"),
            Self::Allocation => f.write_str("building the profile ran out of memory"),
        }
    }
}

impl Error for ProfileError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RegionOutOfRange { source, .. } => Some(source),
            Self::Device { source, .. } => Some(source),
            Self::DeviceMapping { source, .. } => Some(source),
            Self::Memory { source, .. } => Some(source),
            Self::Storage { source, .. } => Some(source),
            Self::Machine(source) => Some(source),
            _ => None,
        }
    }
}

impl From<MachineError> for ProfileError {
    fn from(source: MachineError) -> Self {
        Self::Machine(source)
    }
}

impl LazalithMachine<Box<dyn Device>> {
    /// Builds the machine this profile describes.
    ///
    /// Every device in the inventory is created *and mapped*, so a machine built
    /// from a profile is one whose device windows are where the profile said they
    /// were — not one that has the devices but has not connected them. A profile
    /// that mapped cleanly and a machine that agrees with it is the round trip
    /// `crates/lazalith-machine/tests/profile.rs` checks.
    pub fn from_profile(profile: &MachineProfile) -> Result<Self, ProfileError> {
        let setup = profile.machine_setup()?;
        let mut machine = Self::new(setup)?;
        for device in profile.devices() {
            machine
                .map_device(device.id, device.address, device.permissions)
                .map_err(|source| ProfileError::DeviceMapping {
                    id: device.id,
                    address: device.address,
                    source,
                })?;
        }
        machine.reset();
        // The interrupt model is part of the profile, so the vector is applied here
        // where the profile is the authority 2014 not left for each caller to remember.
        // A machine whose trap vector depends on who built it is a machine whose
        // interrupts land somewhere chosen by a caller that may not have known there
        // was a choice.
        machine.set_trap_vector(profile.trap_vector())?;
        Ok(machine)
    }

    /// Whether this machine agrees with a profile.
    ///
    /// The check has to be able to fail, or it is not a check: a machine that has
    /// the right devices at the wrong addresses, or the right addresses and a
    /// device the profile never named, is a machine that booted and then
    /// misbehaved. This walks everything a profile describes and nothing else —
    /// a profile says nothing about, say, the device *contents*, so this says
    /// nothing about them either.
    pub fn matches_profile(&self, profile: &MachineProfile) -> Result<(), ProfileError> {
        let mismatch = |detail| Err(ProfileError::ProfileMismatch { detail });
        if self.config() != profile.name().architecture() {
            return mismatch("the architecture differs");
        }
        if self.processor().architectural().pc() != profile.reset_vector() {
            return mismatch("the reset vector differs");
        }
        if self.processor().architectural().sp() != profile.kernel_stack_pointer() {
            return mismatch("the kernel stack pointer differs");
        }
        let trap_vector = self
            .processor()
            .read_trap_control(ControlRegister::Tvec)
            .map_err(|source| ProfileError::Machine(MachineError::Cpu(source)))?;
        if trap_vector != profile.trap_vector().as_u64() {
            return mismatch("the trap vector differs");
        }
        if self.devices().len() != profile.devices().len() {
            return mismatch("the machine has devices the profile did not name");
        }
        for device in profile.devices() {
            self.devices()
                .device(device.id)
                .map_err(|source| ProfileError::Device {
                    id: device.id,
                    source,
                })?;
        }
        Ok(())
    }
}

/// A machine with no devices at all, built from a profile that has none.
///
/// The Phase-I shape — `NoDevice` — is a machine that *cannot* hold a device, and
/// it is what the boot path and the OS tests use today. This alias exists so the two
/// are not confused: a profile-built machine is `Box<dyn Device>`, and a Phase-I
/// machine is `NoDevice`, and the difference is visible in the type.
pub type ProfiledMachine = LazalithMachine<Box<dyn Device>>;
