//! Reproducing a run from a file.
//!
//! # The promise
//!
//! Step 87 asks that a bug be reproducible from four things:
//!
//! ```text
//! binary          what the program is
//! architecture    which machine it runs on
//! initial state   where that machine starts
//! input log       what the outside world did to it
//! ```
//!
//! [`ReplaySession`] takes exactly those four and produces a [`Trace`]. Running the
//! same four twice gives the same trace, field for field; a state or a log written
//! down and read back replays to itself; and a trace compared against a *recorded*
//! one says precisely which field differs. That is the whole feature — not "a
//! debugger that can rewind" but "a run that can be shipped".
//!
//! # Why devices are not in the initial state
//!
//! The obvious design puts every device's state in the snapshot. This one does not,
//! and the reason is that a device is a *model of the outside world*, and the
//! outside world is not in the snapshot — the input log is. A device is a function
//! of the input it was given and the virtual time it has been ticked, and the
//! artifact carries both, so snapshotting a device would store a cache of something
//! derivable and create a second source of truth to disagree with the first.
//!
//! What replaces it is stricter rather than weaker: a [`Trace`] records what the
//! devices *did* — how many events reached the program — so a replay that fed a
//! device differently, ticked it differently, or let the program read a different
//! number of events produces a different trace and is caught rather than assumed
//! right.
//!
//! # Where this sits in the machine
//!
//! Below the kernel. A replay here is a machine, a program image, and a log: no
//! supervisor, no filesystem, no process table. That is deliberate. The kernel's own
//! state would have to be in the artifact for the promise to hold, and until it is,
//! a replay that included it would be a promise about something this implementation
//! does not check. What the initial state does carry is everything a program can
//! observe: the processor, the trap vector, and every mapped region's bytes and
//! permissions.

use alloc::vec::Vec;

use lazalith_cpu::EngineKind;
use lazalith_devices::{CycleCount, DeviceId, input::InputDevice};
use lazalith_machine::{LazalithMachine, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_types::PhysicalAddress;
use lazalith_types::{ArchitectureConfig, InstructionAddress, RegisterIndex, VirtualAddress};

use crate::input_log::{InputLog, InputLogError};

/// The magic at the front of an encoded state.
const MAGIC: [u8; 4] = *b"LZST";
/// The format version this build writes.
const VERSION: u16 = 1;
/// How many registers a machine has, in the file and in the trace.
const REGISTERS: usize = 16;
/// The device the replay drives.
const DEVICE_ID: u32 = 1;
/// Where the device window is mapped.
const DEVICE_BASE: u64 = 0x1000;

/// What can be wrong with a replay artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplayError {
    /// The initial state is not one this build reads.
    State(StateError),
    /// The input log is not one this build reads.
    Log(InputLogError),
    /// The binary is not an image this build loads.
    Image(&'static str),
    /// The machine refused to be built as asked.
    Machine(String),
    /// A region in the artifact is not one this machine can map.
    Region {
        /// Which region.
        index: usize,
        /// What was wrong with it.
        reason: String,
    },
}

impl core::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::State(error) => write!(f, "the initial state is not usable: {error}"),
            Self::Log(error) => write!(f, "the input log is not usable: {error}"),
            Self::Image(reason) => write!(f, "the binary did not load: {reason}"),
            Self::Machine(reason) => write!(f, "the machine would not start: {reason}"),
            Self::Region { index, reason } => {
                write!(
                    f,
                    "region {index} in the artifact is not mappable: {reason}"
                )
            }
        }
    }
}

impl From<StateError> for ReplayError {
    fn from(error: StateError) -> Self {
        Self::State(error)
    }
}

impl From<InputLogError> for ReplayError {
    fn from(error: InputLogError) -> Self {
        Self::Log(error)
    }
}

/// What can be wrong with an initial state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateError {
    /// The bytes do not start with the magic.
    NotAState,
    /// The version is one this build does not write.
    Version(u16),
    /// The file ended in the middle of something.
    Truncated {
        /// What was being read.
        part: &'static str,
    },
    /// The file has bytes left over after everything it declared.
    Trailing {
        /// How many bytes were left.
        bytes: usize,
    },
    /// The architecture in the file is not one this build knows.
    Architecture {
        /// The word width byte.
        width: u8,
        /// The feature bits.
        features: u32,
    },
    /// The region count in the file is larger than the bytes can describe.
    Regions {
        /// The count in the header.
        regions: u32,
        /// How many the bytes could hold.
        possible: usize,
    },
    /// The region count is not one a `usize` can hold.
    RegionCount(u32),
}

impl core::fmt::Display for StateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAState => f.write_str("these bytes are not a machine state"),
            Self::Version(version) => {
                write!(
                    f,
                    "machine state version {version} is not one this build reads"
                )
            }
            Self::Truncated { part } => {
                write!(f, "the machine state ends in the middle of its {part}")
            }
            Self::Trailing { bytes } => {
                write!(
                    f,
                    "{bytes} bytes are left over after the machine state ends"
                )
            }
            Self::Architecture { width, features } => write!(
                f,
                "a {width}-bit word with feature bits {features:#x} is not an architecture this build knows"
            ),
            Self::Regions { regions, possible } => write!(
                f,
                "the state claims {regions} regions, which is more than {possible} could hold"
            ),
            Self::RegionCount(regions) => {
                write!(f, "a state with {regions} regions does not fit this host")
            }
        }
    }
}

/// Everything a program can observe about a machine before it runs.
///
/// The configuration lives here rather than beside it, because an initial state for
/// a machine that is not the machine in the artifact describes nothing: the width
/// decides how a register is read and the features decide which instructions are
/// legal, so a state without them is not a state, it is half of one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineState {
    /// The machine this state is for.
    pub config: ArchitectureConfig,
    /// The registers, in index order.
    pub registers: [u64; REGISTERS],
    /// Where it will execute next.
    pub pc: InstructionAddress,
    /// Its stack pointer.
    pub sp: VirtualAddress,
    /// Its status register.
    pub status: u64,
    /// Whether it is halted.
    pub halted: bool,
    /// Where a trap goes.
    pub trap_vector: InstructionAddress,
    /// The regions, in the order they were mapped.
    pub regions: Vec<StateRegion>,
}

/// The fixed part of an encoded state, before the regions.
const STATE_HEADER: usize = 4 + 2 + 1 + 4 + (8 * REGISTERS) + 8 + 8 + 8 + 1 + 8 + 4;
/// The fixed part of one region header.
const REGION_HEADER: usize = 8 + 4 + 4;

/// One region, as the artifact carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateRegion {
    /// Where it starts.
    pub start: u64,
    /// Its permissions.
    pub permissions: RegionPermissions,
    /// Its bytes.
    pub bytes: Vec<u8>,
}

impl MachineState {
    /// A state with no regions and every register zero.
    pub const fn new(config: ArchitectureConfig) -> Self {
        Self {
            config,
            registers: [0; REGISTERS],
            pc: InstructionAddress::new(0),
            sp: VirtualAddress::new(0),
            status: 0,
            halted: false,
            trap_vector: InstructionAddress::new(0),
            regions: Vec::new(),
        }
    }

    /// The state as bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(STATE_HEADER);
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.push(match self.config.word_width() {
            lazalith_types::WordWidth::W32 => 1,
            lazalith_types::WordWidth::W64 => 2,
        });
        bytes.extend_from_slice(&self.config.features().bits().to_le_bytes());
        for register in &self.registers {
            bytes.extend_from_slice(&register.to_le_bytes());
        }
        bytes.extend_from_slice(&self.pc.as_u64().to_le_bytes());
        bytes.extend_from_slice(&self.sp.as_u64().to_le_bytes());
        bytes.extend_from_slice(&self.status.to_le_bytes());
        bytes.push(u8::from(self.halted));
        bytes.extend_from_slice(&self.trap_vector.as_u64().to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(self.regions.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for region in &self.regions {
            bytes.extend_from_slice(&region.start.to_le_bytes());
            bytes.extend_from_slice(&[
                u8::from(region.permissions.read),
                u8::from(region.permissions.write),
                u8::from(region.permissions.execute),
                u8::from(region.permissions.user),
            ]);
            bytes.extend_from_slice(
                &u32::try_from(region.bytes.len())
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            );
            bytes.extend_from_slice(&region.bytes);
        }
        bytes
    }

    /// A state from bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        if bytes.len() < STATE_HEADER || bytes[0..4] != MAGIC {
            return Err(StateError::NotAState);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(StateError::Version(version));
        }
        let width_byte = bytes[6];
        let features = read_u32(bytes, 7);
        let width = match width_byte {
            1 => lazalith_types::WordWidth::W32,
            2 => lazalith_types::WordWidth::W64,
            _ => {
                return Err(StateError::Architecture {
                    width: width_byte,
                    features,
                });
            }
        };
        let config = ArchitectureConfig::try_from_bits(width, features).map_err(|_| {
            StateError::Architecture {
                width: width_byte,
                features,
            }
        })?;
        let mut at = 11;
        let mut registers = [0_u64; REGISTERS];
        for register in &mut registers {
            *register = read_u64(bytes, at);
            at += 8;
        }
        let pc = InstructionAddress::new(read_u64(bytes, at));
        at += 8;
        let sp = VirtualAddress::new(read_u64(bytes, at));
        at += 8;
        let status = read_u64(bytes, at);
        at += 8;
        let halted = bytes.get(at).copied().unwrap_or(0) != 0;
        at += 1;
        let trap_vector = InstructionAddress::new(read_u64(bytes, at));
        at += 8;
        let regions = read_u32(bytes, at);
        at += 4;
        let possible = bytes.len().saturating_sub(at) / REGION_HEADER;
        if usize::try_from(regions).map_or(true, |regions| regions > possible) {
            return Err(StateError::Regions { regions, possible });
        }
        let count = usize::try_from(regions).map_err(|_| StateError::RegionCount(regions))?;
        let mut state = Self {
            config,
            registers,
            pc,
            sp,
            status,
            halted,
            trap_vector,
            regions: Vec::new(),
        };
        state
            .regions
            .try_reserve_exact(count)
            .map_err(|_| StateError::RegionCount(regions))?;
        for _ in 0..count {
            let start = read_u64(bytes, at);
            let flags = bytes
                .get(at + 8..at + 12)
                .ok_or(StateError::Truncated { part: "a region" })?;
            let permissions = RegionPermissions {
                read: flags[0] != 0,
                write: flags[1] != 0,
                execute: flags[2] != 0,
                user: flags[3] != 0,
            };
            let length = read_u32(bytes, at + 12);
            at += REGION_HEADER;
            let length = usize::try_from(length).unwrap_or(usize::MAX);
            let end = at.checked_add(length).ok_or(StateError::Truncated {
                part: "a region's bytes",
            })?;
            let body = bytes.get(at..end).ok_or(StateError::Truncated {
                part: "a region's bytes",
            })?;
            at = end;
            state.regions.push(StateRegion {
                start,
                permissions,
                bytes: body.to_vec(),
            });
        }
        if at != bytes.len() {
            return Err(StateError::Trailing {
                bytes: bytes.len() - at,
            });
        }
        Ok(state)
    }
}

/// Why a run stopped.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    /// The program halted.
    Halted,
    /// The run reached its step limit with the program still running.
    Limit,
    /// The machine faulted, and the fault is the reason it stopped.
    Faulted,
}

impl Stop {
    /// The name, for a report.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Halted => "halted",
            Self::Limit => "step limit",
            Self::Faulted => "faulted",
        }
    }
}

/// What a run did, in enough detail to say whether a second run did the same.
///
/// Not the machine's whole state: a trace is what an *observer* could have watched,
/// which is the point. A trace that recorded every bit would be a snapshot, and a
/// snapshot of a run is not evidence about a run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Trace {
    /// How many times the replay loop called the machine.
    ///
    /// **A host-level count, and it is *not* engine-independent.** A JIT that retires a
    /// block needs fewer calls than an interpreter for the same program, so two replays of
    /// one program on different engines legitimately disagree here. That is not a defect in
    /// replay — it is the JIT doing its job — and it is why [`Trace::instructions`] exists.
    /// Anything that needs "did the same work happen" must compare that field, not this one.
    pub steps: u64,
    /// How many guest instructions retired.
    ///
    /// **The count that is engine-independent**, and therefore the one a determinism claim
    /// is about: a guest can read its own instruction count through the machine, and it
    /// must read the same number however the machine executed it.
    pub instructions: u64,
    /// Why it stopped.
    pub stop: Stop,
    /// The registers at the end, in index order.
    pub registers: [u64; REGISTERS],
    /// Where it stopped.
    pub pc: InstructionAddress,
    /// Its stack pointer at the end.
    pub sp: VirtualAddress,
    /// Its status register at the end.
    pub status: u64,
    /// The virtual time at the end.
    pub time: CycleCount,
    /// How many events the input device had handed to the program.
    pub delivered: u64,
    /// Whether a trap frame was still open at the end.
    pub in_trap: bool,
}

impl Trace {
    /// The first thing two traces disagree about, named.
    ///
    /// A `PartialEq` on a twelve-field struct reports *that* two runs differ, which
    /// is the one thing a person reproducing a bug does not need to be told twice.
    /// The name is a `String` because a register's name depends on its index, and a
    /// table of sixteen `&'static str`s to avoid one small allocation would be the
    /// wrong trade in code whose whole purpose is to be read.
    pub fn difference(&self, other: &Self) -> Option<String> {
        let same = |what: &'static str| Some(String::from(what));
        if self.steps != other.steps {
            return same("the number of steps the replay took");
        }
        if self.instructions != other.instructions {
            return same("the number of guest instructions retired");
        }
        if self.stop != other.stop {
            return same("why it stopped");
        }
        for (index, (left, right)) in self.registers.iter().zip(&other.registers).enumerate() {
            if left != right {
                return Some(format!("register r{index}"));
            }
        }
        if self.pc != other.pc {
            return same("the program counter");
        }
        if self.sp != other.sp {
            return same("the stack pointer");
        }
        if self.status != other.status {
            return same("the status register");
        }
        if self.time != other.time {
            return same("the virtual time");
        }
        if self.delivered != other.delivered {
            return same("how many input events the program was given");
        }
        if self.in_trap != other.in_trap {
            return same("whether a trap frame was open");
        }
        None
    }
}

/// A run that can be repeated.
///
/// Built from the four things the step names and holding nothing else, so a session
/// is *only* as much state as the artifact carries. A replay that quietly remembered
/// something its file did not would be a replay nobody could trust.
#[derive(Debug)]
pub struct ReplaySession {
    machine: LazalithMachine<InputDevice>,
    log: InputLog,
    next: usize,
    /// Engine changes still to apply, as `(instructions retired when due, engine)`.
    ///
    /// **Ordered by the retired count, and the count is retired *instructions* rather than
    /// `run` iterations** — a JIT retires several instructions in one step, so a schedule
    /// counted in steps would fire at a different guest position depending on which engine
    /// was live. Counting retired instructions makes a schedule mean the same thing on
    /// every engine, which is the property the whole test depends on.
    schedule: Vec<(u64, EngineKind)>,
}

impl ReplaySession {
    /// Builds a session from the four things a reproduction is made of.
    pub fn new(image: &[u8], initial: &MachineState, log: &InputLog) -> Result<Self, ReplayError> {
        Self::build(image, initial, log, &[])
    }

    /// The same, with a schedule of engine changes to apply as the run proceeds.
    ///
    /// # What this is for
    ///
    /// **§11 requires "deterministic replay across engine switches" as a listed case, and
    /// this is how that case is stated rather than hoped for.** A replay is a claim that a
    /// recorded run can be reproduced; a claim that is only true when the reproduction
    /// happens to use the same execution engine is a much weaker one, and nobody would know
    /// whether it held until someone replayed on a different host or after a JIT changed
    /// how it chunks blocks.
    ///
    /// The schedule is a list of `(instructions retired before the switch, engine)`. It is
    /// applied *between* steps, which is the only place a switch may happen, and it is
    /// deliberately the caller's to specify rather than a policy: "what if the machine
    /// switched engines at these points" is a question with no single right answer, and a
    /// replay that answered it for you would be hiding the thing being tested.
    ///
    /// The [`Trace`] this produces must be **identical** to one produced with an empty
    /// schedule — that is the assertion `replay_is_engine_independent` makes, and it is
    /// the whole point of the parameter.
    pub fn with_engine_schedule(
        image: &[u8],
        initial: &MachineState,
        log: &InputLog,
        schedule: &[(u64, EngineKind)],
    ) -> Result<Self, ReplayError> {
        Self::build(image, initial, log, schedule)
    }

    fn build(
        image: &[u8],
        initial: &MachineState,
        log: &InputLog,
        schedule: &[(u64, EngineKind)],
    ) -> Result<Self, ReplayError> {
        let mut devices = lazalith_devices::DeviceManager::new();
        devices
            .insert(DeviceId::new(DEVICE_ID), InputDevice::new())
            .map_err(|error| ReplayError::Machine(format!("the input device: {error:?}")))?;
        let mut machine = LazalithMachine::new(MachineSetup {
            config: initial.config,
            devices,
            regions: build_regions(initial)?,
            pc: initial.pc,
            sp: initial.sp,
            status: initial.status,
            initial_time: CycleCount::new(0),
        })
        .map_err(|error| ReplayError::Machine(format!("{error:?}")))?;
        // A machine that has not been reset will not step — it is `Created`, and
        // `Created` is a state where running is refused rather than a state where
        // running does something surprising. Reset also empties the devices, which
        // is what a replay wants: the log is the only thing that should put an event
        // in the input queue, and a device that remembered one from construction
        // would be input the artifact does not describe.
        machine.reset();
        for (index, value) in initial.registers.iter().enumerate() {
            let Ok(raw) = u8::try_from(index) else {
                break;
            };
            if let Ok(register) = RegisterIndex::try_from(raw) {
                machine
                    .write_register(register, *value)
                    .map_err(|error| ReplayError::Machine(format!("register {raw}: {error:?}")))?;
            }
        }
        machine
            .set_trap_vector(initial.trap_vector)
            .map_err(|error| ReplayError::Machine(format!("the trap vector: {error:?}")))?;
        machine
            .map_device(
                DeviceId::new(DEVICE_ID),
                PhysicalAddress::new(DEVICE_BASE),
                RegionPermissions {
                    read: true,
                    write: true,
                    execute: false,
                    user: true,
                },
            )
            .map_err(|error| ReplayError::Machine(format!("the device window: {error:?}")))?;
        // The artifact's own bytes go in first and the binary on top of them, so a
        // state that already holds a program's data is not overwritten by an image
        // that only carries code.
        for (index, region) in initial.regions.iter().enumerate() {
            if region.bytes.is_empty() {
                continue;
            }
            machine
                .load_bytes(PhysicalAddress::new(region.start), &region.bytes)
                .map_err(|error| ReplayError::Region {
                    index,
                    reason: format!("its bytes would not load: {error:?}"),
                })?;
        }
        load_image(&mut machine, image)?;
        // The schedule's first entry is applied before the first step rather than at the
        // first instruction boundary it names, so a session that asks to start on an engine
        // actually starts on it — the alternative is a run that ignores its own schedule for
        // the first N instructions and then is surprised by it.
        if let Some((_, first)) = schedule.first() {
            machine
                .switch_execution_engine(*first)
                .map_err(|error| ReplayError::Machine(format!("the first engine: {error:?}")))?;
        }
        Ok(Self {
            machine,
            log: log.clone(),
            next: 0,
            schedule: schedule
                .iter()
                .skip(1)
                .map(|(at, kind)| (*at, *kind))
                .collect(),
        })
    }

    /// Runs up to `limit` instructions, delivering the log as it goes.
    ///
    /// **This loop does not touch the clock.** It used to, advancing it by exactly one
    /// cycle per instruction, and that was a second time source living in the replay
    /// engine: the machine's own clock was not moving during execution, so the replay
    /// engine had to move it or the log's events would never come due. The machine now
    /// charges each retired instruction its own cost, so a step here advances virtual
    /// time by what the instruction cost — and the replay engine adding a cycle on top
    /// would count every instruction twice.
    ///
    /// That is the whole point of the fix, and it is worth being explicit about why it
    /// was possible: the manual advance was *load-bearing* while the machine's clock was
    /// inert, and removing it is only correct because the machine took the job over. A
    /// replay engine that kept its own clock would be a second account of virtual time,
    /// and two accounts of the same quantity is the defect the whole canonical-state
    /// rule exists to prevent — in a place nobody would have looked for it, because
    /// "the replay engine keeps the log's events on schedule" sounds like a feature.
    ///
    /// What is preserved is the property the manual advance was there for: a trace
    /// depends on the program and the log, never on a host tick or a wall clock. The
    /// machine's cost model is part of the ISA, so it is the same on every host.
    ///
    /// A log's events are delivered on the cycle they were recorded on, so the program
    /// sees the same input at the same point in its own execution.
    pub fn run(&mut self, limit: u64) -> Result<Trace, ReplayError> {
        let mut steps = 0;
        let stop = loop {
            if steps >= limit {
                break Stop::Limit;
            }
            self.deliver_due();
            if self.machine.step().is_err() {
                break Stop::Faulted;
            }
            steps += 1;
            self.apply_schedule();
            if self.machine.is_halted() {
                break Stop::Halted;
            }
        };
        Ok(self.trace(steps, stop))
    }

    /// Applies any engine change that has come due, in retired-instruction order.
    ///
    /// **Between steps, which is the only place a switch may happen.** A switch in the
    /// middle of a step would be a switch in the middle of an instruction, and B3's
    /// machine refuses exactly that for good reason. The switched engine also has to be
    /// one the machine accepts at this moment — a machine that has trapped is not
    /// switchable, and a schedule that asks for it is reporting a fault rather than
    /// failing silently, which is the behaviour a reproduction needs.
    fn apply_schedule(&mut self) {
        while let Some((at, kind)) = self.schedule.first().copied() {
            if self.machine.executed_instruction_count() < at {
                break;
            }
            self.schedule.remove(0);
            if self.machine.execution_engine() == kind {
                continue;
            }
            if let Err(error) = self.machine.switch_execution_engine(kind) {
                // A refused switch is a fact about the run, and the run keeps its
                // correctness by simply not switching — the alternative, propagating the
                // error, would abort a reproduction that is otherwise fine.
                let _ = error;
            }
        }
    }

    /// Injects every event the log says arrived by now.
    fn deliver_due(&mut self) {
        let now = self.machine.clock().elapsed();
        let due = self.log.events_through(now);
        while self.next < due {
            let Some(record) = self.log.records().get(self.next) else {
                break;
            };
            // A device that will not take the event refuses, and refusing is the
            // honest answer: a log with more events queued than the device will hold
            // is a log to investigate, not one to quietly drop the tail of. The
            // cursor still moves, so one full queue cannot wedge the run.
            if let Ok(device) = self
                .machine
                .devices_mut()
                .device_mut(DeviceId::new(DEVICE_ID))
            {
                let _ = device.inject(record.event);
            }
            self.next += 1;
        }
    }

    /// What the run did.
    fn trace(&self, steps: u64, stop: Stop) -> Trace {
        let state = self.machine.architectural_state();
        let mut registers = [0_u64; REGISTERS];
        for (index, register) in registers.iter_mut().enumerate() {
            if let Ok(raw) = u8::try_from(index) {
                if let Ok(raw) = RegisterIndex::try_from(raw) {
                    *register = state.registers().read_raw(raw.as_u8()).unwrap_or(0);
                }
            }
        }
        Trace {
            steps,
            instructions: self.machine.executed_instruction_count(),
            stop,
            registers,
            pc: state.pc(),
            sp: state.sp(),
            status: state.status().bits(),
            time: self.machine.clock().elapsed(),
            delivered: self
                .machine
                .devices()
                .device(DeviceId::new(DEVICE_ID))
                .map_or(0, |device| device.delivered()),
            in_trap: self.machine.processor().traps().has_active_frame(),
        }
    }

    /// The machine, for a caller that wants something the trace does not carry.
    pub const fn machine(&self) -> &LazalithMachine<InputDevice> {
        &self.machine
    }

    /// The machine, mutably.
    ///
    /// For a caller that wants to look at memory or a device *between* steps rather
    /// than only at the end. Stepping through this instead of [`ReplaySession::run`]
    /// makes the run depend on the caller, and a caller that does it gets a run that
    /// is no longer the one the artifact describes — which is worth being able to do
    /// and worth being able to see.
    pub const fn machine_mut(&mut self) -> &mut LazalithMachine<InputDevice> {
        &mut self.machine
    }

    /// The log this session is replaying.
    pub const fn log(&self) -> &InputLog {
        &self.log
    }
}

/// Copies a program image into the machine, over the artifact's own bytes.
fn load_image(machine: &mut LazalithMachine<InputDevice>, image: &[u8]) -> Result<(), ReplayError> {
    let loaded = lazalith_os::LzxImage::from_bytes(image)
        .map_err(|_| ReplayError::Image("these bytes are not an .lzx image"))?;
    let Some(section) = loaded.sections().first() else {
        return Err(ReplayError::Image("the image has no sections"));
    };
    machine
        .load_bytes(
            PhysicalAddress::new(section.virtual_offset()),
            section.bytes(),
        )
        .map_err(|error| ReplayError::Region {
            index: 0,
            reason: format!(
                "the image's code section does not fit the region the state mapped: {error:?}"
            ),
        })?;
    Ok(())
}

/// Maps the artifact's regions onto a machine.
fn build_regions(state: &MachineState) -> Result<Vec<MemoryRegion>, ReplayError> {
    let mut regions = Vec::new();
    regions
        .try_reserve_exact(state.regions.len())
        .map_err(|_| ReplayError::Machine(String::from("not enough memory for the regions")))?;
    for (index, region) in state.regions.iter().enumerate() {
        let length = u64::try_from(region.bytes.len()).map_err(|_| ReplayError::Region {
            index,
            reason: String::from("its length is not an address"),
        })?;
        let mapped = MemoryRegion::ram(
            state.config,
            PhysicalAddress::new(region.start),
            length,
            region.permissions,
        )
        .map_err(|error| ReplayError::Region {
            index,
            reason: format!("{error:?}"),
        })?;
        regions.push(mapped);
    }
    Ok(regions)
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0_u8; 4];
    for (index, slot) in word.iter_mut().enumerate() {
        *slot = bytes.get(at + index).copied().unwrap_or(0);
    }
    u32::from_le_bytes(word)
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0_u8; 8];
    for (index, slot) in word.iter_mut().enumerate() {
        *slot = bytes.get(at + index).copied().unwrap_or(0);
    }
    u64::from_le_bytes(word)
}
