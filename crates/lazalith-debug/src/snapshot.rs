//! Machine snapshots: the four types, and what each one is allowed to contain.
//!
//! # The rule this file exists to enforce
//!
//! **Only guest-visible state belongs in a machine snapshot.** That is the
//! roadmap's sentence and it is the whole design constraint, so it is worth being
//! precise about what it excludes, because the exclusions are the interesting
//! part:
//!
//! | Excluded | Why |
//! | --- | --- |
//! | a device's elapsed clock | a device ticks against it, but nothing in the register file reports it and no guest instruction can observe it |
//! | a console's emitted output | a console hands bytes to a *host*; its registers are write-only from the guest's side, so its output is not the guest's to see again |
//! | the machine's instruction count and its virtual clock | host bookkeeping about how the machine got here |
//! | a framebuffer's pixels | those belong to the guest, and they are in the process's memory — a snapshot that copied them would hold a second copy of every one, which is the thing the display device's design refuses to be |
//!
//! What *is* included is everything a guest can observe or affect: the
//! architectural registers, the trap frame a program is stopped in, each device's
//! registers, and each process's state, memory, threads and handles.
//!
//! # Why these are clones and not encodings
//!
//! Every type here holds a clone of the state it names, rather than a byte
//! encoding of it that has to be decoded again. An encoding has to be kept in step
//! with the thing it encodes, and a snapshot that silently omits a field is a
//! snapshot that restores a machine which is *almost* the one that was saved —
//! which is the failure this step exists to prevent. A clone cannot forget a
//! field, and a field added to a process without a decision here is a field a
//! snapshot carries.
//!
//! The one exception is a device, whose state is its own encoding: a device's
//! state is a device's business, and a common encoding would have to be a lowest
//! common denominator that lost whatever made each device different. `restore`
//! checks the bytes are that device's own, so a mismatch is a refusal.

use alloc::vec::Vec;

use lazalith_cpu::{ArchitecturalState, ExecutionState as CpuExecution, Processor};
use lazalith_devices::{DeviceId, DeviceManager};
use lazalith_os::{Process, ProcessId, ProcessState};
use lazalith_types::RegisterIndex;

/// The processor's state, with the trap frames it was stopped in.
///
/// The frames are not an extra. A program stopped in a syscall has its return
/// address and saved registers in the trap frame, so a snapshot of the
/// architectural state alone is not resumable: restoring it would leave a frame
/// the program never returns through.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuSnapshot {
    architectural: ArchitecturalState,
    execution: CpuExecution,
    traps: lazalith_cpu::TrapController,
}

impl CpuSnapshot {
    /// Captures `processor`.
    pub fn of(processor: &Processor) -> Self {
        Self {
            architectural: processor.architectural().clone(),
            execution: processor.execution(),
            traps: processor.traps().clone(),
        }
    }

    /// The program counter at the time of the snapshot.
    pub fn pc(&self) -> u64 {
        self.architectural.pc().as_u64()
    }

    /// The stack pointer at the time of the snapshot.
    pub fn sp(&self) -> u64 {
        self.architectural.sp().as_u64()
    }

    /// The value of general register `index`, or `None` if there is no such
    /// register.
    pub fn register(&self, index: u8) -> Option<u64> {
        let register = RegisterIndex::try_from(index).ok()?;
        Some(self.architectural.registers().read(register))
    }

    /// Whether the processor was stopped in a trap at the time.
    pub fn in_trap(&self) -> bool {
        self.traps.has_active_frame()
    }

    /// Puts the state back.
    ///
    /// The architectural state goes in first, through the same validation a normal
    /// step uses, so a restore is not a way to smuggle an inconsistent processor
    /// past the checks the machine makes every step.
    pub fn restore(&self, processor: &mut Processor) -> Result<(), String> {
        let restored = Processor::from_parts(
            self.architectural.clone(),
            self.execution,
            self.traps.clone(),
        );
        processor
            .restore(restored.map_err(|error| {
                format!("the processor would not take the state back: {error:?}")
            })?)
            .map_err(|error| format!("the processor would not take the state back: {error:?}"))
    }
}

/// One device's guest-visible state, as that device encoded it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSnapshot {
    id: DeviceId,
    bytes: Vec<u8>,
}

impl DeviceSnapshot {
    /// Captures every device in `devices`, in device order.
    pub fn of<D: lazalith_devices::Device>(devices: &DeviceManager<D>) -> Vec<Self> {
        devices
            .snapshot()
            .into_iter()
            .map(|(id, bytes)| Self { id, bytes })
            .collect()
    }

    /// Which device this is.
    pub const fn id(&self) -> DeviceId {
        self.id
    }

    /// The device's own encoding of its state.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A process's state, memory, threads and handles.
///
/// All four, because a process is all four: a snapshot that kept the memory and
/// dropped the handles would restore a program whose files had quietly closed,
/// and one that kept the handles and dropped the memory would restore a program
/// reading whatever the next program left behind.
#[derive(Clone, Debug)]
pub struct ProcessSnapshot {
    id: ProcessId,
    process: Process,
}

impl ProcessSnapshot {
    /// Captures `process`.
    pub fn of(process: &Process) -> Self {
        Self {
            id: process.id(),
            process: process.clone(),
        }
    }

    /// Which process this is.
    pub const fn id(&self) -> ProcessId {
        self.id
    }

    /// Whether the process had finished at the time.
    pub fn finished(&self) -> bool {
        matches!(self.process.state(), ProcessState::Exited)
    }

    /// The code it exited with, if it had.
    pub fn exit_code(&self) -> Option<u32> {
        self.process.exit_code()
    }

    /// The captured process, for a controller putting it back.
    pub const fn process(&self) -> &Process {
        &self.process
    }
}

/// A whole machine: its processor, its devices, and its processes.
///
/// The type a debug session saves and restores. It holds no clock, no terminal
/// output and no filesystem, because none of those is a guest's to see; see the
/// module documentation for the whole list and why each is excluded.
#[derive(Clone, Debug)]
pub struct MachineSnapshot {
    cpu: CpuSnapshot,
    devices: Vec<DeviceSnapshot>,
    processes: Vec<ProcessSnapshot>,
}

impl MachineSnapshot {
    /// Captures a machine and the processes on it.
    pub fn of<D: lazalith_devices::Device>(
        processor: &Processor,
        devices: &DeviceManager<D>,
        processes: impl IntoIterator<Item = Process>,
    ) -> Self {
        Self {
            cpu: CpuSnapshot::of(processor),
            devices: DeviceSnapshot::of(devices),
            processes: processes
                .into_iter()
                .map(|process| ProcessSnapshot::of(&process))
                .collect(),
        }
    }

    /// The processor's state.
    pub const fn cpu(&self) -> &CpuSnapshot {
        &self.cpu
    }

    /// The devices' states, in device order.
    pub fn devices(&self) -> &[DeviceSnapshot] {
        &self.devices
    }

    /// The processes' states, in the order they were given.
    pub fn processes(&self) -> &[ProcessSnapshot] {
        &self.processes
    }

    /// The number of processes captured.
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }
}
