//! B12: device discovery and the expansion bus.
//!
//! # What is being held here
//!
//! **Discovery is a description, not a scan.** A bus that scans for devices has to know
//! what a device looks like before it has one — that is PCI's configuration space and it
//! is a large decision. Here a device *says* what it is, so a machine built from a
//! profile has exactly the devices the profile named and no others. The cost is stated
//! plainly: a guest cannot find a device the host did not describe, and for a machine
//! meant to run code that probes for hardware that is a limitation, not a feature.
//!
//! **Every refusal happens before anything is written.** A bus that attached a device
//! and *then* found its window overlapped would leave a machine where one address has two
//! possible destinations, and nothing would ever report it. Each test attaches a good
//! device, then a bad one, and checks the good one is untouched.
//!
//! **Interrupt lines are exclusive.** A controller that delivers one line to two devices
//! delivers it to neither, so a conflict is a refusal rather than a resolution order.

use std::boxed::Box;

use lazalith_devices::{ConsoleDevice, Device, DeviceId, TimerDevice};
use lazalith_memory::expansion::{
    BusError, ClassRegistry, DEFAULT_BUS_DEVICES, DeviceDescriptor, ExpansionBus, Window,
};
use lazalith_types::{InterruptId, PhysicalAddress};

const CONSOLE: DeviceId = DeviceId::new(1);
const TIMER: DeviceId = DeviceId::new(2);
const DISK: DeviceId = DeviceId::new(3);

const CONSOLE_WINDOW: Window = Window::new(PhysicalAddress::new(0x4000_0000), 0x1000);
const TIMER_WINDOW: Window = Window::new(PhysicalAddress::new(0x4000_1000), 0x1000);
const DISK_WINDOW: Window = Window::new(PhysicalAddress::new(0x4003_0000), 0x1000);

fn console() -> DeviceDescriptor {
    DeviceDescriptor::new(CONSOLE, 1, "console", CONSOLE_WINDOW)
}

fn timer() -> DeviceDescriptor {
    DeviceDescriptor::new(TIMER, 2, "timer", TIMER_WINDOW).with_interrupt(InterruptId::new(2))
}

fn disk() -> DeviceDescriptor {
    DeviceDescriptor::new(DISK, 3, "block", DISK_WINDOW).with_interrupt(InterruptId::new(3))
}

fn bus() -> ExpansionBus<Box<dyn Device>> {
    let mut bus = ExpansionBus::new();
    bus.attach(
        console(),
        Box::new(ConsoleDevice::new(64).expect("a console")) as Box<dyn Device>,
    )
    .expect("a console attaches");
    bus
}

// -- the window arithmetic -----------------------------------------------------

#[test]
fn a_window_contains_exactly_its_own_addresses() {
    let window = Window::new(PhysicalAddress::new(0x1000), 0x100);
    assert!(
        window.contains(PhysicalAddress::new(0x1000)),
        "the first byte"
    );
    assert!(
        window.contains(PhysicalAddress::new(0x10FF)),
        "the last byte"
    );
    assert!(
        !window.contains(PhysicalAddress::new(0x1100)),
        "and one past the end is outside, which is the boundary an off-by-one gets wrong"
    );
    assert!(!window.contains(PhysicalAddress::new(0xFFF)));
}

#[test]
fn two_windows_overlap_only_when_they_share_an_address() {
    let a = Window::new(PhysicalAddress::new(0x1000), 0x100);
    let adjacent = Window::new(PhysicalAddress::new(0x1100), 0x100);
    let overlapping = Window::new(PhysicalAddress::new(0x1080), 0x100);
    let before = Window::new(PhysicalAddress::new(0x0F00), 0x100);
    assert!(!a.overlaps(&adjacent), "adjacent windows do not overlap");
    assert!(
        a.overlaps(&overlapping),
        "a window that starts inside one does"
    );
    assert!(overlapping.overlaps(&a), "and it is symmetric");
    assert!(!a.overlaps(&before));
}

#[test]
fn a_window_that_would_overflow_the_address_space_has_no_end() {
    let enormous = Window::new(PhysicalAddress::new(u64::MAX - 4), 16);
    assert_eq!(
        enormous.end(),
        None,
        "a window that runs past the end of the address space must be a refusal, not an \
         end address that wrapped to something small and legal"
    );
}

// -- attaching ----------------------------------------------------------------

#[test]
fn a_bus_holds_the_devices_it_was_given() {
    let mut bus = bus();
    bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a timer attaches");
    bus.attach(
        disk(),
        Box::new(ConsoleDevice::new(64).expect("a console")) as Box<dyn Device>,
    )
    .expect("a block device attaches");

    assert_eq!(bus.len(), 3);
    assert!(!bus.is_empty());
    assert!(
        bus.device(TIMER).is_ok(),
        "and a caller can reach one by id"
    );
    assert_eq!(
        bus.device(DeviceId::new(99)).err(),
        Some(BusError::UnknownDevice(DeviceId::new(99)))
    );
}

#[test]
fn two_devices_with_one_id_are_refused() {
    let mut bus = bus();
    let clash = DeviceDescriptor::new(CONSOLE, 9, "clash", DISK_WINDOW);
    assert_eq!(
        bus.attach(clash, Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::DuplicateId(CONSOLE)),
        "two devices with one id means an access routed by id has two destinations"
    );
    assert_eq!(bus.len(), 1, "and the bus is unchanged");
}

#[test]
fn two_devices_whose_windows_overlap_are_refused() {
    let mut bus = bus();
    let overlapping = DeviceDescriptor::new(
        DISK,
        3,
        "overlap",
        Window::new(PhysicalAddress::new(0x4000_0080), 0x1000),
    );
    assert_eq!(
        bus.attach(overlapping, Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::WindowOverlap {
            first: CONSOLE,
            second: DISK,
        }),
        "the refusal names both devices, so a caller fixing it knows which pair to move"
    );
    assert_eq!(
        bus.len(),
        1,
        "and nothing was attached: a bus that attached and *then* found the overlap would \
         leave a machine where one address has two possible destinations and nothing would \
         ever report it"
    );
}

#[test]
fn two_devices_on_one_interrupt_line_are_refused() {
    let mut bus = bus();
    bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a timer attaches first");
    let rival =
        DeviceDescriptor::new(DISK, 3, "rival", DISK_WINDOW).with_interrupt(InterruptId::new(2));
    assert_eq!(
        bus.attach(rival, Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::InterruptConflict {
            line: 2,
            first: TIMER,
            second: DISK,
        }),
        "a controller that delivers one line to two devices delivers it to neither, so this \
         is a refusal rather than a resolution order"
    );
    assert_eq!(bus.len(), 2, "and again nothing more was attached");
}

#[test]
fn a_zero_length_window_is_refused() {
    let mut bus = bus();
    let empty = DeviceDescriptor::new(
        DISK,
        3,
        "empty",
        Window::new(PhysicalAddress::new(0x5000_0000), 0),
    );
    assert_eq!(
        bus.attach(empty, Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::EmptyWindow(DISK))
    );
}

#[test]
fn a_window_past_the_end_of_the_address_space_is_refused() {
    let mut bus = bus();
    let runaway = DeviceDescriptor::new(
        DISK,
        3,
        "runaway",
        Window::new(PhysicalAddress::new(u64::MAX - 4), 16),
    );
    assert!(matches!(
        bus.attach(runaway, Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::WindowOutOfRange { id: DISK, .. })
    ));
}

#[test]
fn a_full_bus_is_refused_rather_than_growing() {
    let mut bus = ExpansionBus::with_capacity(1);
    bus.attach(
        console(),
        Box::new(ConsoleDevice::new(64).expect("a console")) as Box<dyn Device>,
    )
    .expect("the first attaches");
    assert_eq!(
        bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>),
        Err(BusError::Full { capacity: 1 }),
        "a bound the bus advertises is a bound it keeps, so an unbounded profile fails at \
         attach time rather than producing a machine that cannot be built"
    );
}

#[test]
fn a_bus_with_no_capacity_is_a_legitimate_machine() {
    let mut bus: ExpansionBus<TimerDevice> = ExpansionBus::with_capacity(0);
    assert!(
        bus.attach(console(), TimerDevice::new()).is_err(),
        "and refusing every attach is a way to say this machine has no expansion hardware"
    );
    assert!(bus.is_empty());
    assert_eq!(DEFAULT_BUS_DEVICES, 32, "while the default is not that");
}

// -- discovery ----------------------------------------------------------------

#[test]
fn a_topology_is_a_value_rather_than_a_scan() {
    let mut bus = bus();
    bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a timer attaches");
    let topology = bus.topology();

    assert_eq!(topology.len(), 2);
    assert!(!topology.is_empty());
    assert_eq!(
        topology.descriptor(TIMER).map(|d| d.name),
        Some("timer"),
        "a caller can ask what is on the bus by id"
    );
    assert_eq!(topology.of_class(1).count(), 1, "or by class");
    assert_eq!(
        topology.interrupts(),
        vec![2],
        "and can be told which lines are in use without asking any device"
    );
    assert!(
        topology.to_string().contains("timer"),
        "a Display that names the devices, because this is what a manager shows a person"
    );
}

#[test]
fn the_topology_records_attachment_order_because_it_records_who_had_an_address() {
    let mut bus = bus();
    bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a timer attaches");
    let ids: Vec<DeviceId> = bus.topology().devices.iter().map(|d| d.id).collect();
    assert_eq!(ids, vec![CONSOLE, TIMER], "in the order they were attached");
    // Two devices' windows are checked when the *second* is attached, so the order
    // records which one had the address -- which is what a caller diagnosing an overlap
    // wants to know.
}

#[test]
fn a_device_that_says_it_can_dma_is_visible_as_one_that_can() {
    let mut bus = bus();
    let dma = disk().with_dma();
    bus.attach(dma, Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a dma device attaches");
    let topology = bus.topology();
    assert_eq!(
        topology.dma_capable().count(),
        1,
        "§33 lists DMA and it is not built; the flag exists so a profile can say so before \
         the engine can, and a machine that has one finds out at run time rather than at \
         boot. A flag nothing reads would be worse than none."
    );
    assert_eq!(topology.descriptor(DISK).map(|d| d.dma), Some(true));
}

#[test]
fn class_numbers_are_allocated_rather_than_hard_coded() {
    let mut registry = ClassRegistry::new();
    let console = registry.allocate().expect("a class");
    let timer = registry.allocate().expect("a class");
    let disk = registry.allocate().expect("a class");
    assert_ne!(console, timer);
    assert_ne!(timer, disk);
    assert_eq!(registry.allocated(), 3);
    assert!(registry.to_string().contains('3'));
    // A descriptor that a caller builds by hand and forgets to fill in gets 0, and class
    // 0 is never handed out, so an unfilled class is distinguishable from a real one.
    assert_eq!(registry.allocate(), Some(4));
    assert_ne!(console, 0);
}

// -- conversion ---------------------------------------------------------------

#[test]
fn an_erased_bus_converts_to_a_manager_and_keeps_its_descriptions() {
    let mut bus = bus();
    bus.attach(timer(), Box::new(TimerDevice::new()) as Box<dyn Device>)
        .expect("a timer attaches");
    let (manager, topology) = bus.into_manager();
    assert_eq!(manager.len(), 2, "every attached device arrived");
    assert_eq!(
        topology.len(),
        2,
        "and the descriptions came with them: a machine that has the devices and not what \
         they are cannot be shown to a person or snapshotted"
    );
    assert!(manager.device(CONSOLE).is_ok());
    assert!(manager.device(TIMER).is_ok());
}

#[test]
fn a_monomorphic_bus_also_works_because_it_never_needs_converting() {
    // `into_manager` is on the erased bus only, and deliberately: a `DeviceManager<D>` is
    // already what a monomorphic bus has, so converting would copy devices to get
    // nowhere. This test is here so the monomorphic path stays covered rather than
    // untested-because-inconvenient.
    //
    // It uses `TimerDevice` and not `NoDevice`, because `NoDevice` is an *uninhabited*
    // enum: there is no value of it, so a bus of them could not be built and this test
    // could not exist. Phase-I uses `NoDevice` as a type, not as a value.
    let mut bus: ExpansionBus<TimerDevice> = ExpansionBus::new();
    bus.attach(console(), TimerDevice::new())
        .expect("a console slot attaches");
    bus.attach(timer(), TimerDevice::new())
        .expect("a timer slot attaches");
    assert_eq!(bus.len(), 2);
    assert_eq!(bus.topology().len(), 2);
    assert_eq!(bus.windows().len(), 2, "and the windows came with them");
}
