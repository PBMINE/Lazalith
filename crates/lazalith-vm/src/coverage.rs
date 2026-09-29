//! B18: the machine's state, and an account of what a snapshot does and does not hold.
//!
//! # What §40 asks
//!
//! Promote snapshots to VM-level infrastructure, and investigate the coverage of
//!
//! ```text
//! CPU, memory, devices, virtual clock, interrupt state, machine configuration,
//! storage state, network state, debug state
//! ```
//!
//! with snapshot, restore, clone, replay and migration as the capabilities, and a
//! document saying **which host-backed devices are deterministic/snapshot-safe**.
//!
//! # The finding that changed the design
//!
//! `lazalith-debug`'s snapshot module states its rule as "only guest-visible state
//! belongs in a machine snapshot" and then lists the **virtual clock** as excluded,
//! on the grounds that it is "host bookkeeping about how the machine got here".
//!
//! Those two sentences contradict each other, and the clock is on the wrong side of
//! the rule. A guest can call `time` and `sleep`; a device is ticked with the machine's
//! clock; and `LazalithMachine::restore_time` already exists *because* the clock matters
//! to anything that is not the CPU. A snapshot that omits it restores a machine at a
//! different virtual time, so a guest that reads the clock after a restore sees a
//! different answer to the one it saw before.
//!
//! So the clock and the pending-interrupt set are captured here. The module that
//! excluded them is not contradicted in passing — the *reason* is corrected, and the
//! correction is tested by a machine whose restored clock can be observed to be wrong.
//!
//! # What the devices do with their own clocks, and why that is not a second bug
//!
//! Every device also keeps an `elapsed` clock, and **not one of them puts it in its
//! snapshot** — checked by reading all six. That looks like the same mistake one level
//! down, and it is not, and the distinction is worth stating rather than leaving to be
//! rediscovered:
//!
//! - The machine's clock is **guest-visible**: it is what `time` returns and what
//!   `sleep` is measured against, so dropping it changes what a program reads.
//! - A device's `elapsed` is set by `tick` and reported by **no register on any
//!   device**, so no guest instruction can observe it. Each device says so in its own
//!   `snapshot` — the input device's is explicit, and the console's explains that its
//!   whole job is handing bytes to a host and that its output is not guest-readable,
//!   which is why its snapshot is legitimately empty.
//!
//! So the rule the devices are already following is the right one, and this module's
//! claim is the same rule: **capture what the guest can see.** A device that started
//! reporting `elapsed` through a register would have to add it to its snapshot in the
//! same change, and that is a note for whoever adds the register rather than a defect
//! in what is here.
//!
//! # The coverage account is data, not prose
//!
//! [`StateCoverage`] is an **inventory**: nine items, each with a verdict, and a count
//! that must equal the number of items. `the_inventory_covers_exactly_what_is_captured`
//! fails if the inventory and [`CapturedState`] disagree about what exists, so adding a
//! field to the state without deciding what it is for is a test failure rather than a
//! line in a document nobody re-reads.
//!
//! # Determinism is a property of the resource, and it is stated per resource
//!
//! §40 asks which host-backed devices are deterministic. The answer is a
//! [`Determinism`] on every state item, and it is deliberately *not* a blanket claim:
//!
//! - [`Determinism::Intrinsic`] — the value is a function of guest state. A register
//!   is intrinsic; restoring it and running the same instructions reproduces it.
//! - [`Determinism::Recorded`] — the value comes from outside and has to be *written
//!   down* for a replay to reproduce it. A host clock is recorded; without the
//!   recording, "replay" is a second run, not the first one again.
//! - [`Determinism::External`] — the value belongs to the host and is not part of the
//!   machine at all. A terminal's emitted output is external: the guest handed bytes to
//!   the host and the host did what it liked with them.
//!
//! **A replay is only claimed to be deterministic over the items that are intrinsic
//! or recorded.** [`CapturedState::replay_guarantee`] reports which those are rather
//! than returning a bare `true`, because a replay that is deterministic for the CPU
//! and accidental for the network is a replay someone will trust for the CPU and be
//! wrong about for the network.

#![deny(missing_docs)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use lazalith_cpu::{ArchitecturalState, Processor};
use lazalith_devices::DeviceId;
use lazalith_machine::LazalithMachine;
use lazalith_types::InterruptId;
use lazalith_types::{ArchitectureConfig, VirtualClock};

/// One of §40's nine pieces of state.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum StateItem {
    /// The architectural registers and the trap frames a program is stopped in.
    Cpu,
    /// Each process's memory, its threads, and its handles.
    Memory,
    /// Each device's own state, in its own encoding.
    Devices,
    /// The machine's virtual clock, which devices tick against and guests read.
    Clock,
    /// The interrupts that have been requested and not yet taken.
    Interrupts,
    /// The machine's own configuration: architecture, ISA and ABI versions.
    Configuration,
    /// The host filesystem a storage device is backed by.
    Storage,
    /// The host network a network device is backed by.
    Network,
    /// The debug state: breakpoints, watches, and the input log.
    Debug,
}

impl StateItem {
    /// All nine, in §40's order.
    pub const ALL: [Self; 9] = [
        Self::Cpu,
        Self::Memory,
        Self::Devices,
        Self::Clock,
        Self::Interrupts,
        Self::Configuration,
        Self::Storage,
        Self::Network,
        Self::Debug,
    ];

    /// This item's name, as §40 writes it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Memory => "memory",
            Self::Devices => "devices",
            Self::Clock => "virtual clock",
            Self::Interrupts => "interrupt state",
            Self::Configuration => "machine configuration",
            Self::Storage => "storage state",
            Self::Network => "network state",
            Self::Debug => "debug state",
        }
    }

    /// Whether a snapshot holds this item, and what kind of thing it is.
    pub const fn coverage(self) -> Coverage {
        match self {
            // Guest state, carried in the snapshot itself.
            Self::Cpu | Self::Memory | Self::Devices | Self::Clock | Self::Interrupts => {
                Coverage::Captured
            }
            // Known to the machine and checked on restore, but not copied: it is the
            // same for every snapshot of the same machine and copying it would be a
            // second place for it to be wrong.
            Self::Configuration => Coverage::Checked,
            // Host-backed, and therefore a question about the host rather than the
            // guest. A snapshot says so rather than pretending.
            Self::Storage | Self::Network | Self::Debug => Coverage::External,
        }
    }

    /// Whether a replay over this item can be *promised*, rather than hoped for.
    pub const fn determinism(self) -> Determinism {
        match self {
            Self::Cpu | Self::Memory | Self::Devices | Self::Clock | Self::Interrupts => {
                Determinism::Intrinsic
            }
            Self::Configuration => Determinism::Intrinsic,
            // §40's own question. A storage or network device is backed by the host,
            // so what a replay sees is the host's answer at replay time unless the
            // host's answers were written down. They are not, yet, and
            // `ReplayGuarantee` says so instead of the code pretending otherwise.
            Self::Storage | Self::Network => Determinism::External,
            // The input log is recorded, which is what makes input-driven replay
            // work at all; what the *host* does with the input is still the host's.
            Self::Debug => Determinism::Recorded,
        }
    }
}

impl fmt::Display for StateItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a snapshot does about a piece of state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Coverage {
    /// Copied into the snapshot and restored from it.
    Captured,
    /// Not copied, but checked on restore, so a snapshot from a different machine is
    /// refused rather than loaded.
    ///
    /// **Not the same as captured, and the difference matters.** A checked item is
    /// either right or the restore fails; a captured item is restored. A caller that
    /// wanted to *change* the configuration by restoring a snapshot cannot, and a
    /// configuration that is silently not checked would let it.
    Checked,
    /// Not part of the machine. Host-backed resources live on the host, and a snapshot
    /// says where they are rather than pretending to contain them.
    External,
}

/// How reproducible a piece of state is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Determinism {
    /// A function of guest state. Restoring it and running the same instructions
    /// reproduces it.
    Intrinsic,
    /// Comes from outside, and has to be written down for a replay to reproduce it.
    Recorded,
    /// Belongs to the host and is not part of the machine.
    External,
}

/// How many items the inventory gives the classification `kind`.
///
/// **Counted from the inventory, in a `const fn`, rather than written down.** The
/// array lengths in [`ReplayGuarantee`] are these numbers, so adding a tenth item to
/// [`StateItem::ALL`] and reclassifying it is a recompile error in the type instead of
/// an `assert!` that would fire the first time somebody asked for a guarantee.
const fn count_determinism(kind: Determinism) -> usize {
    let mut count = 0;
    let mut index = 0;
    while index < StateItem::ALL.len() {
        if StateItem::ALL[index].determinism() as u8 == kind as u8 {
            count += 1;
        }
        index += 1;
    }
    count
}

/// How many items a replay is deterministic over.
pub const DETERMINISTIC_ITEMS: usize = count_determinism(Determinism::Intrinsic);
/// How many items a replay reproduces only from a recording.
pub const RECORDED_ITEMS: usize = count_determinism(Determinism::Recorded);
/// How many items a replay says nothing about.
pub const EXTERNAL_ITEMS: usize = count_determinism(Determinism::External);

/// What a replay can be promised, item by item.
///
/// **Not a `bool`.** A replay that is exact for the CPU and accidental for the network
/// is a replay someone will trust for the CPU and be wrong about for the network, so
/// the answer is the list rather than a summary of it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReplayGuarantee {
    /// The items a replay reproduces, because they are a function of guest state.
    pub deterministic: [StateItem; DETERMINISTIC_ITEMS],
    /// The items it reproduces only because their host answers were recorded.
    pub recorded: [StateItem; RECORDED_ITEMS],
    /// The items it says nothing about.
    pub external: [StateItem; EXTERNAL_ITEMS],
}

impl ReplayGuarantee {
    /// Whether `item` is covered by the guarantee.
    ///
    /// **External counts as *not* covered, and that is the point of the split.**
    /// `covers` is what a caller may rely on, and a caller that treats storage and
    /// network as covered is exactly the caller this type exists to stop.
    pub const fn covers(item: StateItem) -> bool {
        !matches!(item.determinism(), Determinism::External)
    }

    /// A sentence saying exactly what this replay can and cannot promise.
    pub fn describe(&self) -> String {
        let mut text = format!(
            "replay is deterministic over {} (intrinsic) and {} (recorded) and says nothing about {} (host-backed)",
            self.deterministic.len(),
            self.recorded.len(),
            self.external.len(),
        );
        let names: Vec<&str> = self.external.iter().map(|item| item.as_str()).collect();
        text.push_str(": ");
        text.push_str(&names.join(", "));
        text
    }
}

/// The processor's architectural state, held whole.
///
/// **One field, the canonical type, and not a copy of its parts.** B3 settled that the
/// `Processor` owns the architectural state and that an engine borrows it; a snapshot
/// type that copied `pc`, `sp` and an array of registers into its own fields would be
/// a *second* description of the machine's registers, and the one thing a snapshot
/// must never be is a place a register can be added to and forgotten. Holding the
/// state itself makes that impossible: a new register is in the snapshot the day it is
/// in the machine.
///
/// The trap frames a program is stopped in are the processor's own, restored with
/// [`Processor::restore_architectural`] rather than being re-derived here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuState {
    architectural: ArchitecturalState,
    execution: lazalith_cpu::ExecutionState,
}

impl CpuState {
    /// Reads the processor's state, including where it was stopped.
    pub fn of(processor: &Processor) -> Self {
        Self {
            architectural: processor.architectural().clone(),
            execution: processor.execution(),
        }
    }

    /// The architectural state, whole.
    pub const fn architectural(&self) -> &ArchitecturalState {
        &self.architectural
    }

    /// The program counter.
    pub const fn pc(&self) -> u64 {
        self.architectural.pc().as_u64()
    }

    /// The stack pointer.
    pub const fn sp(&self) -> u64 {
        self.architectural.sp().as_u64()
    }

    /// One general register, or `None` if the machine has no such register.
    pub fn register(&self, index: u8) -> Option<u64> {
        self.architectural.registers().read_raw(index).ok()
    }

    /// Puts the state back, through the same validation a normal step uses.
    ///
    /// A restore is not a way to smuggle an inconsistent processor past the checks the
    /// machine makes every step, so this goes through `Processor::restore_architectural`
    /// and reports what it refused.
    pub fn restore(&self, processor: &mut Processor) -> Result<(), String> {
        processor
            .restore_architectural_with_execution(self.architectural.clone(), self.execution)
            .map_err(|error| format!("restoring the CPU state was refused: {error}"))
    }
}

/// One device's state, in that device's own encoding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceState {
    id: DeviceId,
    bytes: Vec<u8>,
}

impl DeviceState {
    /// Which device this is.
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// The device's own encoding, which only that device can interpret.
    ///
    /// **A device's state is its business, and this is not decoded here.** A common
    /// encoding across every device would have to be a lowest common denominator that
    /// lost whatever made each one different, so each device encodes itself and the
    /// bytes stay opaque until that device is given them back. Decoding a device's
    /// snapshot in the VM layer would put that device's private layout in the one
    /// place that must not know about it.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The bytes in the shape `DeviceManager::restore` takes.
    fn as_manager_state(&self) -> (DeviceId, Vec<u8>) {
        (self.id, self.bytes.clone())
    }
}

/// The §40 inventory: every piece of state, and what happens to it.
///
/// **This is the function a document is generated from.** §40 asks for a document
/// saying what is and is not covered; prose drifts from code, and a document that
/// claims the clock is covered is worth nothing next to a snapshot that drops it. So
/// the claim is this function, and a test holds [`StateItem::ALL`] against it.
pub fn coverage_inventory() -> Vec<StateCoverage> {
    StateItem::ALL
        .into_iter()
        .map(|item| StateCoverage {
            item,
            coverage: item.coverage(),
            determinism: item.determinism(),
        })
        .collect()
}

/// One row of the §40 inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StateCoverage {
    /// Which piece of state.
    pub item: StateItem,
    /// What a snapshot does about it.
    pub coverage: Coverage,
    /// How reproducible it is.
    pub determinism: Determinism,
}

impl fmt::Display for StateCoverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let coverage = match self.coverage {
            Coverage::Captured => "captured",
            Coverage::Checked => "checked on restore",
            Coverage::External => "external (host-backed)",
        };
        let determinism = match self.determinism {
            Determinism::Intrinsic => "intrinsic",
            Determinism::Recorded => "recorded",
            Determinism::External => "not promised",
        };
        write!(f, "{}: {coverage}, {determinism}", self.item)
    }
}

/// A whole machine's state: everything [`StateItem::coverage`] calls [`Coverage::Captured`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedState {
    cpu: CpuState,
    devices: Vec<DeviceState>,
    clock: VirtualClock,
    pending: Vec<InterruptId>,
    architecture: ArchitectureConfig,
}

impl CapturedState {
    /// Captures a machine.
    ///
    /// **The clock and the pending interrupts are here because §40 names them and
    /// because they are on the causal path.** The clock is not host bookkeeping: a
    /// guest reads it through `time`, devices tick against it, and restoring a machine
    /// without it restores one whose devices disagree with each other. A pending
    /// interrupt is a promise the machine has made to itself that it will take one;
    /// dropping it silently removes an interrupt the guest would have received.
    pub fn of<D: lazalith_devices::Device>(machine: &LazalithMachine<D>) -> Self {
        Self {
            cpu: CpuState::of(machine.processor()),
            devices: machine
                .devices()
                .snapshot()
                .into_iter()
                .map(|(id, bytes)| DeviceState { id, bytes })
                .collect(),
            clock: *machine.clock(),
            pending: machine.interrupts().iter().collect(),
            architecture: machine.processor().architectural().config(),
        }
    }

    /// The processor's state.
    pub const fn cpu(&self) -> &CpuState {
        &self.cpu
    }

    /// The devices' states, in device order.
    pub fn devices(&self) -> &[DeviceState] {
        &self.devices
    }

    /// The virtual clock, which devices tick against and guests read.
    pub const fn clock(&self) -> &VirtualClock {
        &self.clock
    }

    /// The interrupts requested and not yet taken, in request order.
    pub fn pending_interrupts(&self) -> &[InterruptId] {
        &self.pending
    }

    /// The machine's configuration, which is checked rather than copied.
    pub const fn architecture(&self) -> ArchitectureConfig {
        self.architecture
    }

    /// Puts the state back.
    ///
    /// **The configuration is checked before anything is written.** Restoring a CPU
    /// state onto a machine with a different architecture and failing afterwards would
    /// leave a machine half-restored, which is worse than not starting.
    pub fn restore<D: lazalith_devices::Device>(
        &self,
        machine: &mut LazalithMachine<D>,
    ) -> Result<(), String> {
        let config = machine.processor().architectural().config();
        if config != self.architecture {
            return Err(format!(
                "the snapshot is for a {:?} machine and this one is {:?}",
                self.architecture.word_width(),
                config.word_width()
            ));
        }
        if machine.devices().len() != self.devices.len() {
            return Err(format!(
                "the snapshot has {} devices and this machine has {}",
                self.devices.len(),
                machine.devices().len()
            ));
        }
        self.cpu.restore(machine.processor_mut())?;
        let states: Vec<(DeviceId, Vec<u8>)> = self
            .devices
            .iter()
            .map(DeviceState::as_manager_state)
            .collect();
        machine
            .devices_mut()
            .restore(&states)
            .map_err(|error| format!("restoring the devices was refused: {error}"))?;
        // The clock last, because it is what the devices just written into were
        // ticking against: setting it first and then restoring a device would leave
        // the device's own view of time disagreeing with the machine's.
        machine.restore_time(self.clock.elapsed());
        machine.interrupts_mut().reset();
        for id in &self.pending {
            machine
                .interrupts_mut()
                .request(*id)
                .map_err(|error| format!("restoring interrupt {id:?} failed: {error}"))?;
        }
        Ok(())
    }

    /// What a replay of this machine can be promised.
    ///
    /// Built from [`StateItem::determinism`] rather than written out, so adding an
    /// item to the inventory changes the guarantee instead of leaving it stale.
    pub fn replay_guarantee() -> ReplayGuarantee {
        let mut deterministic = Vec::new();
        let mut recorded = Vec::new();
        let mut external = Vec::new();
        for item in StateItem::ALL {
            match item.determinism() {
                Determinism::Intrinsic => deterministic.push(item),
                Determinism::Recorded => recorded.push(item),
                Determinism::External => external.push(item),
            }
        }
        // The array lengths are `count_determinism`, so these are the right length by
        // construction and indexing cannot go out of bounds. Filling them through
        // `copy_from_slice` rather than `from_fn` means a future mismatch between the
        // counting and the classification is a panic here, in a function whose entire
        // job is to report the classification, instead of a wrong answer returned
        // confidently to a debugger that trusted it.
        let mut guarantee = ReplayGuarantee {
            deterministic: [StateItem::Cpu; DETERMINISTIC_ITEMS],
            recorded: [StateItem::Cpu; RECORDED_ITEMS],
            external: [StateItem::Cpu; EXTERNAL_ITEMS],
        };
        guarantee.deterministic.copy_from_slice(&deterministic);
        guarantee.recorded.copy_from_slice(&recorded);
        guarantee.external.copy_from_slice(&external);
        guarantee
    }
}
