//! What a VM is, described as data, before one exists.
//!
//! # Why a configuration is a value and not a builder
//!
//! §35 lists "VM configuration" next to "VM creation", and a management layer that
//! configures a VM by mutating a half-built object cannot answer a question a GUI needs
//! to ask before it exists: *what would this machine be?* A GUI shows a form, a CLI
//! takes flags, and both want to hold a description, hand it over, and get a VM back.
//!
//! So [`VmConfig`] is a plain value with public fields, and [`Manager::create`] is the
//! only thing that turns one into a machine. Everything a configuration can say is
//! therefore checkable without building anything, which is what
//! `VmConfig::validate` is for and why the manager's tests can exercise a refusal
//! without a machine in existence.
//!
//! # USB is describable and not constructible
//!
//! §35 lists USB among the things the management layer manages, and **this build has
//! no USB device**. Rather than dropping it from the vocabulary or accepting a
//! configuration that silently produces a machine with no USB, [`DeviceClass`] has a
//! `Usb` variant and [`Manager::create`] refuses it by name.
//!
//! This is the same distinction B4 drew for `DisplayProfile::VgaCompatible` and §28
//! drew for VGA: a profile may *describe* hardware this build cannot produce, and the
//! refusal is a fact a caller can act on. A management API that quietly ignored a
//! configuration field would be worse than one that lacked it, because the field
//! would still be there and would still be type-checked.

#![deny(missing_docs)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

use lazalith_devices::{DeviceId, DisplayProfile};
use lazalith_machine::{DeviceProfile, MachineProfile, ProfileFamily, ProfileName, RegionProfile};
use lazalith_memory::{RegionKind, RegionPermissions};
use lazalith_types::{ArchitectureConfig, PhysicalAddress};

/// The kind of hardware a device entry describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DeviceClass {
    /// A terminal the guest writes to.
    Console,
    /// A framebuffer.
    Display,
    /// A block device backed by storage.
    Block,
    /// A network interface.
    Network,
    /// A sound device.
    Audio,
    /// A keyboard or pointer.
    Input,
    /// A USB controller.
    ///
    /// **Describable, and refused on construction.** No USB device exists in this
    /// build; see the module documentation.
    Usb,
}

impl DeviceClass {
    /// The name a configuration file would use.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Display => "display",
            Self::Block => "block",
            Self::Network => "network",
            Self::Audio => "audio",
            Self::Input => "input",
            Self::Usb => "usb",
        }
    }

    /// Whether this build can construct a device of this class.
    pub const fn is_constructible(self) -> bool {
        !matches!(self, Self::Usb)
    }

    /// Every class, so a UI can enumerate them.
    pub const ALL: [Self; 7] = [
        Self::Console,
        Self::Display,
        Self::Block,
        Self::Network,
        Self::Audio,
        Self::Input,
        Self::Usb,
    ];
}

impl fmt::Display for DeviceClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One device in a [`VmConfig`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSpec {
    /// Which device this is, which is also the number a guest programs name.
    pub id: DeviceId,
    /// What kind of hardware it is.
    pub class: DeviceClass,
    /// Where its register window is mapped.
    pub address: PhysicalAddress,
    /// What may be done to that window.
    pub permissions: RegionPermissions,
    /// Bytes of console output to keep, if this is a console.
    pub console_capacity: Option<usize>,
    /// The display architecture, if this is a display.
    pub display: Option<DisplayProfile>,
}

impl DeviceSpec {
    /// A console, which is the device a VM almost always has.
    pub fn console(id: u16, address: u64) -> Self {
        Self {
            id: DeviceId::new(u32::from(id)),
            class: DeviceClass::Console,
            address: PhysicalAddress::new(address),
            permissions: RegionPermissions::new(true, true, false, true),
            console_capacity: Some(64 * 1024),
            display: None,
        }
    }

    /// A display with the platform's own architecture.
    pub fn display(id: u16, address: u64) -> Self {
        Self {
            id: DeviceId::new(u32::from(id)),
            class: DeviceClass::Display,
            address: PhysicalAddress::new(address),
            permissions: RegionPermissions::new(true, true, false, true),
            console_capacity: None,
            display: Some(DisplayProfile::Native),
        }
    }

    /// A device of any other class.
    pub fn of(id: u16, class: DeviceClass, address: u64) -> Self {
        Self {
            id: DeviceId::new(u32::from(id)),
            class,
            address: PhysicalAddress::new(address),
            permissions: RegionPermissions::new(true, true, false, true),
            console_capacity: None,
            display: None,
        }
    }
}

/// How much memory a VM has, and what kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemorySpec {
    /// The length of the RAM region.
    pub length: u64,
}

impl MemorySpec {
    /// This much RAM.
    pub const fn new(length: u64) -> Self {
        Self { length }
    }
}

/// A whole VM, as data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmConfig {
    /// A name for the VM, for a UI to show and a log to mention.
    pub name: String,
    /// The architecture and machine family.
    pub architecture: ArchitectureConfig,
    /// The machine family this VM belongs to.
    pub family: ProfileFamily,
    /// How much memory it has.
    pub memory: MemorySpec,
    /// Its devices, in the order they are created.
    pub devices: Vec<DeviceSpec>,
    /// The address the kernel is loaded at, which is part of the boot agreement.
    pub kernel_load_address: u64,
}

impl VmConfig {
    /// A VM with a console and nothing else, which is the smallest thing that runs.
    pub fn minimal(architecture: ArchitectureConfig) -> Self {
        Self {
            name: String::from("lazen"),
            architecture,
            family: ProfileFamily::Native,
            memory: MemorySpec::new(16 * 1024 * 1024),
            devices: alloc::vec![DeviceSpec::console(1, 0x4000_0000)],
            kernel_load_address: lazalith_boot::KERNEL_LOAD_ADDRESS,
        }
    }

    /// Adds a device.
    pub fn with_device(mut self, spec: DeviceSpec) -> Self {
        self.devices.push(spec);
        self
    }

    /// Gives the VM a different amount of memory.
    pub fn with_memory(mut self, memory: MemorySpec) -> Self {
        self.memory = memory;
        self
    }

    /// Renames the VM.
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    /// Checks the configuration, without building anything.
    ///
    /// **This is where "describable but not constructible" becomes a refusal with a
    /// name in it.** A configuration naming a USB device is not invalid data; it is a
    /// request for hardware this build does not have, and it is reported as that.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.name.is_empty() {
            return Err(ConfigError::EmptyName);
        }
        if self.memory.length == 0 {
            return Err(ConfigError::EmptyMemory);
        }
        if self.devices.is_empty() {
            return Err(ConfigError::NoDevices);
        }
        let mut seen: Vec<DeviceId> = Vec::new();
        for spec in &self.devices {
            if !spec.class.is_constructible() {
                return Err(ConfigError::UnbuildableDevice {
                    class: spec.class,
                    id: spec.id,
                });
            }
            if let Some(display) = spec.display
                && !display.is_constructible()
            {
                return Err(ConfigError::UnbuildableDisplay {
                    profile: display,
                    id: spec.id,
                });
            }
            if spec.class == DeviceClass::Console && spec.console_capacity == Some(0) {
                return Err(ConfigError::EmptyConsole { id: spec.id });
            }
            if seen.contains(&spec.id) {
                return Err(ConfigError::DuplicateDevice { id: spec.id });
            }
            seen.push(spec.id);
        }
        Ok(())
    }

    /// Turns this configuration into a machine profile.
    ///
    /// **The configuration is the only place RAM is decided.** A profile describes the
    /// memory, and `MachineProfile` is the authority on it (B6), so the mapping is
    /// here rather than in the manager's own field, and `matches_profile` afterwards
    /// can still tell a caller that the machine is the one that was described.
    pub fn to_profile(&self) -> Result<MachineProfile, ConfigError> {
        self.validate()?;
        let mut profile = MachineProfile::empty(
            ProfileName::new(self.architecture, self.family, 1),
            lazalith_machine::LZA64_LAYOUT,
        );
        let layout = profile.layout();
        profile = profile
            .with_region(RegionProfile {
                start: PhysicalAddress::new(layout.boot_rom_start),
                length: layout.boot_rom_length,
                kind: RegionKind::Rom,
                permissions: RegionPermissions::new(true, false, true, false),
            })
            .with_region(RegionProfile {
                start: PhysicalAddress::new(layout.physical_ram_start),
                length: self.memory.length,
                kind: RegionKind::Ram,
                permissions: RegionPermissions::new(true, true, false, true),
            });
        for spec in &self.devices {
            let mut entry =
                DeviceProfile::new(spec.id, spec.class.into(), spec.address, spec.permissions);
            if let Some(capacity) = spec.console_capacity {
                entry = entry.with_console_capacity(capacity);
            }
            if let Some(display) = spec.display {
                entry = entry.with_display_profile(display);
            }
            profile = profile.with_device(entry);
        }
        Ok(profile)
    }
}

impl From<DeviceClass> for lazalith_machine::DeviceClass {
    fn from(class: DeviceClass) -> Self {
        match class {
            DeviceClass::Console => Self::Console,
            DeviceClass::Display => Self::Display,
            DeviceClass::Block => Self::Block,
            DeviceClass::Network => Self::Network,
            DeviceClass::Audio => Self::Audio,
            DeviceClass::Input => Self::Input,
            // The machine crate already has the right word for "a device this
            // build has no constructor for", and mapping to it rather than to a
            // near-miss class means a USB request is refused there too. If it were
            // mapped to `Input`, a caller who somehow got past `validate` would get
            // a keyboard and no complaint.
            DeviceClass::Usb => Self::Other,
        }
    }
}

/// Why a configuration is not a machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// The VM has no name.
    EmptyName,
    /// The VM has no memory.
    EmptyMemory,
    /// The VM has no devices at all.
    NoDevices,
    /// A device class this build cannot construct was named.
    UnbuildableDevice {
        /// The class that was named.
        class: DeviceClass,
        /// Which entry named it.
        id: DeviceId,
    },
    /// A display architecture this build cannot construct was named.
    UnbuildableDisplay {
        /// The architecture that was named.
        profile: DisplayProfile,
        /// Which entry named it.
        id: DeviceId,
    },
    /// A console was given a capacity of zero, which is a console that discards
    /// everything written to it.
    EmptyConsole {
        /// Which console.
        id: DeviceId,
    },
    /// Two devices claim the same number, and a guest programs a device by number.
    DuplicateDevice {
        /// The number both claimed.
        id: DeviceId,
    },
    /// This build has no machine layout for the architecture.
    UnbuildableArchitecture {
        /// The architecture that was named.
        architecture: ArchitectureConfig,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => f.write_str("the VM has no name"),
            Self::EmptyMemory => f.write_str("the VM was given no memory"),
            Self::NoDevices => f.write_str("the VM has no devices at all"),
            Self::UnbuildableDevice { class, id } => write!(
                f,
                "device {id:?} is a {class} and this build has no {class} device"
            ),
            Self::UnbuildableDisplay { profile, id } => write!(
                f,
                "device {id:?} names the {} display architecture, which this build \
                 does not construct",
                profile.as_str()
            ),
            Self::EmptyConsole { id } => write!(
                f,
                "console {id:?} was given a capacity of zero, which discards \
                 everything written to it"
            ),
            Self::DuplicateDevice { id } => {
                write!(f, "two devices are both numbered {id:?}")
            }
            Self::UnbuildableArchitecture { architecture } => write!(
                f,
                "this build has no machine layout for {:?}",
                architecture.word_width()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimal_configuration_is_valid() {
        assert!(
            VmConfig::minimal(ArchitectureConfig::lz64())
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn usb_is_describable_and_refused_by_name() {
        let config = VmConfig::minimal(ArchitectureConfig::lz64()).with_device(DeviceSpec::of(
            2,
            DeviceClass::Usb,
            0x4001_0000,
        ));
        assert!(!DeviceClass::Usb.is_constructible());
        let error = config
            .validate()
            .expect_err("usb is not built in this build");
        assert_eq!(
            error,
            ConfigError::UnbuildableDevice {
                class: DeviceClass::Usb,
                id: DeviceId::new(2)
            }
        );
        assert!(
            error.to_string().contains("usb"),
            "and the refusal names it: {error}"
        );
    }

    #[test]
    fn two_devices_cannot_share_a_number() {
        let config = VmConfig::minimal(ArchitectureConfig::lz64())
            .with_device(DeviceSpec::display(1, 0x4003_0000));
        assert_eq!(
            config.validate(),
            Err(ConfigError::DuplicateDevice {
                id: DeviceId::new(1)
            })
        );
    }

    #[test]
    fn every_class_is_named_and_only_usb_is_unbuildable() {
        for class in DeviceClass::ALL {
            assert!(!class.as_str().is_empty());
            assert_eq!(
                class.is_constructible(),
                class != DeviceClass::Usb,
                "{class} is the only unbuildable class"
            );
        }
    }
}
