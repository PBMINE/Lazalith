//! A machine snapshot: the guest's state, and what it is a snapshot *of*.
//!
//! B6 provides the plain case — architectural state, virtual clock, and every
//! device's own state — with two refusals that make it honest rather than merely
//! convenient. B18 is where replay, execution-engine private state and a rewindable
//! clock arrive; nothing here is built so as to prevent that, and the refusals are
//! chosen to be the ones that are true at *any* snapshot level rather than artefacts
//! of this one.

use alloc::vec::Vec;

use lazalith_cpu::Processor;
use lazalith_devices::DeviceId;
use lazalith_machine::MachineState;
use lazalith_types::{CycleCount, InstructionAddress};

use crate::state::BootStage;

/// A magic word, so a snapshot handed to the wrong type is a refusal and not a
/// misinterpretation.
///
/// The bytes are `"LVM1"`.
const SNAPSHOT_MAGIC: &[u8; 4] = b"LVM1";

/// A snapshot of one machine.
///
/// # What is in it, and what is not
///
/// In: the whole [`Processor`] — architectural registers *and* the execution and
/// trap state, because a guest that has faulted and not yet finished entering its
/// handler is in a state that is neither "before the fault" nor "in the handler", so
/// restoring registers alone would drop it — the virtual clock, and each device's own
/// `Device::snapshot`.
///
/// Not in: the host's storage. That is the B5 rule, unchanged: a block device's
/// snapshot names the storage's identity and not its bytes, because the storage is a
/// host resource and a machine snapshot that copied a 64 MiB disk would be holding a
/// second copy of it. Restoring onto different storage is refused by the device, not
/// silently accepted.
///
/// # A snapshot's parts, taken apart for a restore
///
/// A named type because clippy is right that a five-element tuple of this is not a
/// thing a human should read at a call site, and `Vm::restore` destructures it.
pub(crate) type VmSnapshotParts = (
    BootStage,
    Processor,
    CycleCount,
    Vec<(DeviceId, Vec<u8>)>,
    MachineState,
);
#[derive(Clone, Debug)]
pub struct VmSnapshot {
    stage: BootStage,
    processor: Processor,
    clock: CycleCount,
    devices: Vec<(DeviceId, Vec<u8>)>,
    /// The machine's own lifecycle state, added in B19.
    ///
    /// **Without this a snapshot restored a halted machine that was not halted.** The
    /// processor came back exactly as saved — pointing at the next instruction, not
    /// halted — while `MachineState` still said `Halted`, so `is_halted()` and the
    /// registers disagreed. Nothing caught it until a management layer tried to restore
    /// a pre-halt snapshot onto a VM that had run to a halt and found the VM still
    /// reporting halted.
    state: MachineState,
}

impl VmSnapshot {
    pub(crate) fn new(
        stage: BootStage,
        processor: Processor,
        clock: CycleCount,
        devices: Vec<(DeviceId, Vec<u8>)>,
        state: MachineState,
    ) -> Self {
        Self {
            stage,
            processor,
            clock,
            devices,
            state,
        }
    }

    /// The machine's lifecycle state when this was taken.
    pub const fn machine_state(&self) -> MachineState {
        self.state
    }

    /// Which stage the machine was in when this was taken.
    pub const fn stage(&self) -> BootStage {
        self.stage
    }

    /// The virtual time this was taken at.
    pub const fn clock(&self) -> CycleCount {
        self.clock
    }

    /// How many devices this snapshot holds.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// The processor state, for an inspector or a debugger.
    ///
    /// This is B3's canonical architectural state, handed out as a borrow: reading it
    /// is free and cannot fork it, and a caller that wants a second copy calls
    /// `Processor::clone`.
    pub const fn processor(&self) -> &Processor {
        &self.processor
    }

    /// Where the guest's program counter was.
    pub fn pc(&self) -> InstructionAddress {
        self.processor.architectural().pc()
    }

    /// Splits the snapshot into the parts a restore applies.
    ///
    /// Consuming rather than lending, because a restore that failed part-way has
    /// already moved the machine and must not be able to leave the caller holding a
    /// snapshot it might try again with.
    pub(crate) fn into_parts(self) -> VmSnapshotParts {
        (
            self.stage,
            self.processor,
            self.clock,
            self.devices,
            self.state,
        )
    }

    /// Whether `bytes` begin with this format's magic word.
    ///
    /// A snapshot is handed around as bytes by a future persistence layer, and a
    /// length check on the wrong data is a length check on nothing.
    pub fn has_magic(bytes: &[u8]) -> bool {
        bytes.len() >= SNAPSHOT_MAGIC.len() && &bytes[..SNAPSHOT_MAGIC.len()] == SNAPSHOT_MAGIC
    }
}
