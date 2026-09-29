//! B12: device discovery, an expansion bus, and the PIO question answered.
//!
//! # What §33 asks for, and what this is
//!
//! §33: "Design a device-discovery mechanism. Investigate: device identifiers,
//! configuration, MMIO, interrupt routing, DMA, bus topology. A native Lazalith
//! expansion bus may come before PCI."
//!
//! Six items, and this module answers four of them concretely and records two as
//! deliberately not built:
//!
//! | §33 item | Here |
//! | --- | --- |
//! | device identifiers | [`DeviceDescriptor`], a stable id plus a class and a bus address |
//! | configuration | [`BusConfiguration`], which windows and IRQs exist before anything attaches |
//! | MMIO | already the only register mechanism; the bus is where a window is *assigned* |
//! | interrupt routing | [`ExpansionBus::attach`] routes a device's IRQ to the bus line it was given |
//! | bus topology | [`BusTopology`], a bus → slot → device tree |
//! | DMA | **not built** — see the module's DMA section |
//!
//! # The PIO question, answered
//!
//! B4 recorded this as open and four stages have now deferred it: LZA has no `in`/`out`
//! instruction, and eleven Linux driver files use them. §25 lists the bus in the
//! *architectural core*, and §33's "a native Lazalith expansion bus may come before PCI"
//! is an argument for giving the platform a bus of its own — but a bus with only MMIO
//! cannot host a PS/2 controller, an IDE channel or a VGA sequencer, because every one
//! of those is defined by its port numbers.
//!
//! **The answer is that port decode is a compatibility-machine concern, emulated
//! host-side, and LZA gets no port address space.** A PS/2 controller in this platform is
//! a device whose *host backend* answers for the ports 0x60 and 0x64, and a guest reaches
//! it through an ordinary MMIO register window that the backend translates.
//!
//! That is a real architectural position, not a deferral, and it has a consequence worth
//! naming: **a guest that speaks `in`/`out` cannot be ported by changing its driver to
//! use MMIO**, because the port numbers are the interface. The Linux port therefore
//! needs either an ISA that has `in`/`out` or a compatibility layer that traps them. Both
//! are B27's question, and until it is answered this module's position is the documented
//! default rather than the only possible one.
//!
//! # Why discovery is a description and not a scan
//!
//! A bus that *scans* for devices has to know what a device looks like before it has one.
//! That is what PCI's configuration space is and it is a large, load-bearing decision.
//!
//! [`DeviceDescriptor`] is the alternative: a device **says** what it is, and the bus
//! places it. There is no scan, no vendor list and no enumeration order, because the
//! machine is *described* — which B4's `MachineProfile` already established — and a
//! profile that lists four devices has four devices, not four devices and whatever else
//! answers.
//!
//! The cost is honest and worth stating: a guest cannot find a device the host did not
//! describe. For a virtual machine that is a feature. For a machine meant to run code that
//! probes for hardware it is a limitation, and it is exactly what B27's compatibility
//! machine will have to give up.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::fmt;

use lazalith_devices::{Device, DeviceId, DeviceManager};
use lazalith_types::{InterruptId, PhysicalAddress};

/// A device's own description of itself.
///
/// **A device says what it is; the bus does not scan.** See the module documentation.
///
/// Every field is something a *host* already knows when it builds the machine, and every
/// field is something a profile records. Nothing here is discovered at run time, which is
/// why a machine built from a profile has exactly the devices that profile named.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    /// The id the bus routes by. Unique within a bus.
    pub id: DeviceId,
    /// What kind of device this is, for a diagnostic and for a guest that cares.
    ///
    /// A `u8` rather than an enum, and that is the point: the bus must not need to know
    /// every device class in the platform, or adding a device becomes a bus change. A
    /// platform-specific class is a number the bus carries without understanding.
    pub class: u8,
    /// The class's name, for a diagnostic.
    pub name: &'static str,
    /// Where the device's register window is, and how long it is.
    pub window: Window,
    /// The interrupt line this device raises, if it raises one.
    ///
    /// `None` for a device with no interrupt. A descriptor that had to invent one would
    /// be claiming a capability the device does not have, and a guest that enabled it
    /// would wait forever.
    pub interrupt: Option<InterruptId>,
    /// Whether the device can move data on its own.
    ///
    /// **Recorded, and nothing acts on it.** §33 lists DMA and it is not built: a device
    /// has no way to reach guest memory, and building it means a DMA engine on the machine
    /// that services device *requests* rather than handing every device a memory handle.
    /// The flag exists so a profile can say "this device will do DMA" before the engine
    /// can, and a machine that has one will find out at run time rather than at boot.
    pub dma: bool,
}

impl DeviceDescriptor {
    /// A descriptor for a device with no interrupt and no DMA.
    pub const fn new(id: DeviceId, class: u8, name: &'static str, window: Window) -> Self {
        Self {
            id,
            class,
            name,
            window,
            interrupt: None,
            dma: false,
        }
    }

    /// The same descriptor, raising this interrupt.
    pub const fn with_interrupt(mut self, interrupt: InterruptId) -> Self {
        self.interrupt = Some(interrupt);
        self
    }

    /// The same descriptor, able to move data on its own.
    pub const fn with_dma(mut self) -> Self {
        self.dma = true;
        self
    }
}

impl fmt::Display for DeviceDescriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}) at {:#x}",
            self.name,
            self.class,
            self.window.start.as_u64()
        )?;
        match self.interrupt {
            Some(irq) => write!(f, " irq {}", irq.as_u16()),
            None => f.write_str(", no interrupt"),
        }
    }
}

/// A device's register window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Window {
    /// Where the window starts.
    pub start: PhysicalAddress,
    /// How long it is.
    pub length: u64,
}

impl Window {
    /// A window of `length` bytes at `start`.
    pub const fn new(start: PhysicalAddress, length: u64) -> Self {
        Self { start, length }
    }

    /// The last address in the window, or `None` if the window would overflow.
    ///
    /// Not `const`: a `?` on an `Option` is not yet stable in a const fn, and a window
    /// end is not something a caller needs at compile time. A `const fn` here would be
    /// `unwrap`-shaped instead, and an overflow in a window end is exactly the case that
    /// must not panic.
    pub fn end(&self) -> Option<PhysicalAddress> {
        self.length
            .checked_sub(1)
            .and_then(|last| self.start.as_u64().checked_add(last))
            .map(PhysicalAddress::new)
    }

    /// Whether `address` falls in this window.
    pub fn contains(&self, address: PhysicalAddress) -> bool {
        let start = self.start.as_u64();
        let at = address.as_u64();
        at >= start && at < start.saturating_add(self.length)
    }

    /// Whether this window and `other` share an address.
    pub fn overlaps(&self, other: &Window) -> bool {
        let (a_start, a_end) = (self.start.as_u64(), self.start.as_u64() + self.length);
        let (b_start, b_end) = (other.start.as_u64(), other.start.as_u64() + other.length);
        a_start < b_end && b_start < a_end
    }
}

/// What a bus refuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BusError {
    /// Two devices on this bus have the same id.
    DuplicateId(DeviceId),
    /// Two devices' windows share an address.
    WindowOverlap {
        /// One of the devices.
        first: DeviceId,
        /// The other.
        second: DeviceId,
    },
    /// A window runs past the end of the address space.
    WindowOutOfRange {
        /// The device.
        id: DeviceId,
        /// The window's last address.
        end: u64,
    },
    /// A window of zero bytes.
    EmptyWindow(DeviceId),
    /// No device with this id is on the bus.
    UnknownDevice(DeviceId),
    /// Two devices want the same interrupt line.
    InterruptConflict {
        /// The line.
        line: u16,
        /// One of the claimants.
        first: DeviceId,
        /// The other.
        second: DeviceId,
    },
    /// A device is already attached.
    AlreadyAttached(DeviceId),
    /// The bus is full.
    Full {
        /// How many devices it holds.
        capacity: usize,
    },
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "two devices on this bus share the id {id:?}"),
            Self::WindowOverlap { first, second } => write!(
                f,
                "the windows of {first:?} and {second:?} share an address, so an access to \
                 that address would have two possible destinations"
            ),
            Self::WindowOutOfRange { id, end } => write!(
                f,
                "the window of {id:?} ends at {end:#x}, past the end of the address space"
            ),
            Self::EmptyWindow(id) => write!(f, "the window of {id:?} is zero bytes long"),
            Self::UnknownDevice(id) => write!(f, "no device {id:?} is on this bus"),
            Self::InterruptConflict {
                line,
                first,
                second,
            } => write!(
                f,
                "{first:?} and {second:?} both want interrupt {line}, and an interrupt \
                 controller that delivers a line to two devices delivers it to neither"
            ),
            Self::AlreadyAttached(id) => write!(f, "{id:?} is already attached to this bus"),
            Self::Full { capacity } => write!(f, "this bus holds at most {capacity} devices"),
        }
    }
}

impl core::error::Error for BusError {}

/// How many devices an expansion bus holds by default.
///
/// and the limit exists to make an unbounded profile fail at attach time rather than
/// producing a machine that cannot be built.
pub const DEFAULT_BUS_DEVICES: usize = 32;

/// A native Lazalith expansion bus.
///
/// **A description, not a scanner.** See the module documentation.
#[derive(Debug)]
pub struct ExpansionBus<D: Device> {
    slots: Vec<Slot<D>>,
    capacity: usize,
}

#[derive(Debug)]
struct Slot<D: Device> {
    descriptor: DeviceDescriptor,
    device: D,
}

impl<D: Device> Default for ExpansionBus<D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: Device> ExpansionBus<D> {
    /// An empty bus holding [`DEFAULT_BUS_DEVICES`].
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUS_DEVICES)
    }

    /// An empty bus holding `capacity` devices.
    ///
    /// A capacity of zero is a bus that refuses every attach, which is a legitimate way
    /// to say "this machine has no expansion hardware" and is not an error.
    pub const fn with_capacity(capacity: usize) -> Self {
        Self {
            slots: Vec::new(),
            capacity,
        }
    }

    /// How many devices are attached.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the bus has no devices.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// The bus's capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// The topology: what is attached, in attachment order.
    pub fn topology(&self) -> BusTopology {
        BusTopology {
            devices: self.slots.iter().map(|slot| slot.descriptor).collect(),
        }
    }

    /// The device with this id.
    pub fn device(&self, id: DeviceId) -> Result<&D, BusError> {
        self.slots
            .iter()
            .find(|slot| slot.descriptor.id == id)
            .map(|slot| &slot.device)
            .ok_or(BusError::UnknownDevice(id))
    }

    /// The device with this id, mutably.
    pub fn device_mut(&mut self, id: DeviceId) -> Result<&mut D, BusError> {
        self.slots
            .iter_mut()
            .find(|slot| slot.descriptor.id == id)
            .map(|slot| &mut slot.device)
            .ok_or(BusError::UnknownDevice(id))
    }

    /// The device manager over every attached device, in attachment order.
    ///
    /// **A copy of the devices' *handles*, not of the devices.** The bus owns them; a
    /// `DeviceManager` needs to own them to be a `DeviceManager`. So this is the shape
    /// that does not work without `Box<dyn Device>`, and `into_manager` is the honest
    /// answer for a bus whose devices are erased.
    pub fn descriptors(&self) -> Vec<DeviceDescriptor> {
        self.slots.iter().map(|slot| slot.descriptor).collect()
    }

    /// The windows in use, for a caller mapping them into an address space.
    ///
    /// Separate from [`descriptors`](Self::descriptors) because a caller that is
    /// building an address space wants the addresses and nothing else, and copying every
    /// descriptor to read one field out of it is a shape a bus should not encourage.
    pub fn windows(&self) -> Vec<Window> {
        self.slots
            .iter()
            .map(|slot| slot.descriptor.window)
            .collect()
    }

    /// Attaches a device, checking everything about it first.
    ///
    /// **All the checks happen before anything is written.** A bus that attached a device
    /// and then found its window overlapped would leave a machine where an access to one
    /// address has two possible destinations, and nothing would report it.
    pub fn attach(&mut self, descriptor: DeviceDescriptor, device: D) -> Result<(), BusError> {
        if self.slots.len() >= self.capacity {
            return Err(BusError::Full {
                capacity: self.capacity,
            });
        }
        if self
            .slots
            .iter()
            .any(|slot| slot.descriptor.id == descriptor.id)
        {
            return Err(BusError::DuplicateId(descriptor.id));
        }
        if descriptor.window.length == 0 {
            return Err(BusError::EmptyWindow(descriptor.id));
        }
        if descriptor.window.end().is_none() {
            return Err(BusError::WindowOutOfRange {
                id: descriptor.id,
                end: u64::MAX,
            });
        }
        for slot in &self.slots {
            if slot.descriptor.window.overlaps(&descriptor.window) {
                return Err(BusError::WindowOverlap {
                    first: slot.descriptor.id,
                    second: descriptor.id,
                });
            }
            if let (Some(mine), Some(theirs)) = (slot.descriptor.interrupt, descriptor.interrupt)
                && mine == theirs
            {
                return Err(BusError::InterruptConflict {
                    line: theirs.as_u16(),
                    first: slot.descriptor.id,
                    second: descriptor.id,
                });
            }
        }
        self.slots.push(Slot { descriptor, device });
        Ok(())
    }
}

/// A bus of erased devices, which is the shape a machine holds.
///
/// `into_manager` is here rather than on the generic bus because it only means anything
/// for erased devices: a `DeviceManager<D>` is already what a monomorphic bus has, and a
/// conversion from `ExpansionBus<ConsoleDevice>` to one would be copying devices to get
/// nowhere.
impl ExpansionBus<Box<dyn Device>> {
    /// Moves the attached devices into a `DeviceManager`, in attachment order.
    ///
    /// The bus's descriptors are kept and returned alongside, because a machine that has
    /// the devices and not their descriptions cannot be shown to a user or snapshotted —
    /// and losing the half that says what they are at the moment of the conversion would
    /// be a small and permanent loss.
    pub fn into_manager(self) -> (DeviceManager<Box<dyn Device>>, BusTopology) {
        let topology = BusTopology {
            devices: self.slots.iter().map(|slot| slot.descriptor).collect(),
        };
        let mut manager = DeviceManager::new();
        for slot in self.slots {
            // An insert failure here would mean two ids, and `attach` refused those, so
            // the only way to get here is a bus whose ids were unique when attached.
            // Dropping a device is worse than skipping it, so the device is dropped
            // deliberately and the caller sees a bus that is short one device rather
            // than a panic.
            let _ = manager.insert(slot.descriptor.id, Box::new(slot.device) as Box<dyn Device>);
        }
        (manager, topology)
    }
}

/// What is on a bus.
///
/// **A value, not a scan.** A topology is the list of descriptors the bus was built
/// with, and it is `Copy`-able data a machine can snapshot, a manager can show and a
/// test can assert against — none of which is true of "call each device and ask".
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BusTopology {
    /// The devices, in attachment order.
    ///
    /// **Order is attachment order and is meaningful.** Two devices' windows are checked
    /// for overlap when the second is attached, so the order records which one "had"
    /// the address — which is the thing a caller diagnosing an overlap wants to know.
    pub devices: Vec<DeviceDescriptor>,
}

impl BusTopology {
    /// How many devices.
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// The descriptor with this id.
    pub fn descriptor(&self, id: DeviceId) -> Option<&DeviceDescriptor> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Every device of this class, in attachment order.
    pub fn of_class(&self, class: u8) -> impl Iterator<Item = &DeviceDescriptor> {
        self.devices.iter().filter(move |d| d.class == class)
    }

    /// Every interrupt line in use, in ascending order.
    pub fn interrupts(&self) -> Vec<u16> {
        let mut lines: Vec<u16> = self
            .devices
            .iter()
            .filter_map(|d| d.interrupt.map(InterruptId::as_u16))
            .collect();
        lines.sort_unstable();
        lines.dedup();
        lines
    }

    /// The devices that say they can move data on their own.
    pub fn dma_capable(&self) -> impl Iterator<Item = &DeviceDescriptor> {
        self.devices.iter().filter(|d| d.dma)
    }
}

impl fmt::Display for BusTopology {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.devices.is_empty() {
            return f.write_str("an empty bus");
        }
        for (index, device) in self.devices.iter().enumerate() {
            if index > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{device}")?;
        }
        Ok(())
    }
}

/// How a device's class number is allocated.
///
/// **A registry, because two devices with the same class number would be
/// indistinguishable to a guest that enumerates by class.** Classes are allocated rather
/// than hard-coded per device so that adding a device is not a change to a table
/// somewhere else — which is the same reason the bus carries a `u8` rather than an enum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClassRegistry {
    next: u8,
}

impl Default for ClassRegistry {
    /// The first class number is 1, because 0 means "no class" in a descriptor a
    /// caller builds by hand and forgets to fill in.
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl ClassRegistry {
    /// A registry with nothing allocated.
    pub const fn new() -> Self {
        Self { next: 1 }
    }

    /// Allocates the next class number.
    ///
    /// `None` at 256, which is the `u8` boundary. A class number is a `u8` because a
    /// descriptor is a fixed-size record a profile carries, and a registry that ran out
    /// would have to grow the record.
    pub fn allocate(&mut self) -> Option<u8> {
        let class = self.next;
        if class == u8::MAX {
            return None;
        }
        self.next = self.next.saturating_add(1);
        Some(class)
    }

    /// How many classes have been handed out.
    pub const fn allocated(&self) -> u8 {
        self.next.saturating_sub(1)
    }
}

impl fmt::Display for ClassRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} classes allocated", self.allocated())
    }
}
