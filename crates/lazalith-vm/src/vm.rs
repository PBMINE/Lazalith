//! `Vm` — the VM lifecycle, in one type.
//!
//! # What this is for
//!
//! `binstruction.md` §9 lists what a VM core should define, and among the items are
//! `reset`, `boot`, `device lifecycle`, `machine lifecycle` and `snapshot/restore`.
//! Those were five different things in five different places:
//!
//! - `LazalithMachine::reset` reset the machine but knew nothing about booting;
//! - `BootImage::start` built a machine *and* ran a bootloader, in the boot crate, and
//!   could not be told about devices — it refused a non-empty device manager outright;
//! - `ProfiledMachine::from_profile` built a machine from a profile, in the machine
//!   crate, and knew nothing about booting;
//! - `DeviceManager::snapshot` captured device state, and nothing composed it with the
//!   processor, so there was no machine snapshot at all;
//! - `MachineState` recorded whether a machine would execute, and nothing recorded
//!   whether firmware had run.
//!
//! B6's deliverable is that these are now **one contract on one type**. `Vm` owns a
//! machine, owns the stage it is in, and is the only place a lifecycle transition is
//! decided. Everything else still works, because `Vm` is additive:
//! `LazalithMachine` is still a `LazalithMachine`, Phase-I's `BootImage::start` still
//! builds and boots, and `Vm::from_machine` adopts anything.
//!
//! # What this deliberately does not do
//!
//! - It does not implement firmware, a BIOS or UEFI. §34 forbids it in this pass.
//! - It does not implement the VM *manager* — that is B19, and the difference matters:
//!   a manager creates, configures, persists and runs VMs for a human, and none of
//!   that is a machine's business.
//! - It does not change [`MachineState`]. That enum is the authority on whether a
//!   machine executes; [`BootStage`] is orthogonal to it. See [`crate::state`] for why
//!   there are two and not one.
//!
//! # On refusing mutation
//!
//! This file originally refused to map a device or load a region "while the guest is
//! running", which sounds right and is wrong. Execution here is **synchronous**:
//! `run` executes and returns, and what it leaves behind is
//! [`MachineState::Running`] as bookkeeping, not a guest in flight. A rule keyed on
//! that state would refuse `reset()` immediately after a normal `run(1000)`, which is
//! the most ordinary thing a caller does.
//!
//! So the rule is keyed on what is genuinely in flight: an **active execution
//! context**. That is a scheduled context the machine is inside, and mutating memory
//! or a device window under one would change what addresses mean behind a context
//! that does not know. That is the real hazard, and it is the only one this guards.

use alloc::boxed::Box;

use lazalith_boot::BootImage;
use lazalith_devices::Device;
use lazalith_machine::{
    LazalithMachine, MachineError, MachineEvent, MachineProfile, MachineRun, MachineState,
};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_types::{InstructionAddress, PhysicalAddress, VirtualClock};

use crate::boot::{BootAgreement, BootHandoff};
use crate::error::VmError;
use crate::snapshot::VmSnapshot;
use crate::state::BootStage;

/// A machine with a lifecycle.
#[derive(Debug)]
pub struct Vm<D: Device = Box<dyn Device>> {
    machine: LazalithMachine<D>,
    stage: BootStage,
    /// Kept so a caller can re-validate against the profile it was built from, and so
    /// [`Vm::matches_profile`] has something to check without being handed a profile
    /// that might not be the one this machine was built from.
    profile: Option<MachineProfile>,
    /// Where the last boot left execution, kept so a re-boot and an inspector both
    /// have one answer rather than each recomputing it.
    /// Where the last boot left execution.
    ///
    /// A `BootHandoff` rather than a bare address, so "where does this VM resume"
    /// and "what is a handoff" have one answer.
    booted: Option<BootHandoff>,
}

impl Vm<Box<dyn Device>> {
    /// A machine built from a profile, reset, with nothing executed.
    ///
    /// This is B4's `ProfiledMachine::from_profile` with a lifecycle attached: the
    /// same machine, and the profile is now the VM's to re-check rather than the
    /// caller's to remember.
    ///
    /// The machine is [`BootStage::Cold`]: its boot ROM window exists and is empty,
    /// because `lza64_native_v1` says the profile describes the machine and the image
    /// is not the machine. Executing before a boot lands on zeroes, and `stage` says
    /// so before a caller finds out.
    pub fn described(profile: &MachineProfile) -> Result<Self, VmError> {
        let machine = LazalithMachine::from_profile(profile)?;
        Ok(Self {
            machine,
            stage: BootStage::Cold,
            profile: Some(profile.clone()),
            booted: None,
        })
    }

    /// Whether this machine still agrees with the profile it was built from.
    ///
    /// **Only on the erased instantiation**, and that placement is deliberate:
    /// `LazalithMachine::matches_profile` walks the device set, so it is written for a
    /// machine of erased devices. A `NoDevice` machine has no device set to walk and no
    /// profile to disagree with, so it is not given a check that would always pass 2014
    /// that would be a function whose passing meant nothing.
    ///
    /// Refuses with [`VmError::NoProfile`] for an adopted machine rather than reporting
    /// agreement: a caller that believes its Phase-I machine was verified when no check
    /// ran has been told a lie by an `Ok`.
    pub fn matches_profile(&self) -> Result<(), VmError> {
        let profile = self.profile.as_ref().ok_or(VmError::NoProfile)?;
        self.machine.matches_profile(profile)?;
        Ok(())
    }

    // -- boot --------------------------------------------------------------

    /// Loads a boot image's firmware and runs its bootloader to the handoff.
    ///
    /// **This rebuilds the machine, and that is the contract rather than a
    /// convenience.** A profile's RAM and a boot image's RAM are not the same thing: a
    /// profile has one flat region with the profile's permissions, and an image has a
    /// kernel image, a kernel stack and a user region with the OS's — and a profile's
    /// ROM window is mapped **read-only**, because a guest must not be able to write
    /// its own firmware, which also means firmware cannot be installed into it after
    /// the fact. (That is not a nuisance to work around; it is B6's first attempt at
    /// this method, which loaded the ROM with `load_bytes` and was refused by the
    /// memory model with a `ReadOnly` fault. The memory model was right.)
    ///
    /// So the image is the authority on memory and the profile on devices, and a
    /// machine that both boots and has devices is assembled from the two rather than by
    /// patching one into the other.
    ///
    /// Nothing is carried across. A boot is a cold start: the previous machine's RAM,
    /// devices and processor are not preserved, and preserving them would be a way to
    /// get a machine that is half one profile's disk and half another's.
    ///
    /// The agreement check comes **before** anything is built, so a machine with half a
    /// bootloader in its ROM is never produced.
    pub fn boot(&mut self, image: &BootImage) -> Result<InstructionAddress, VmError> {
        self.require_no_active_context("booting")?;
        let Some(profile) = &self.profile else {
            // No profile: there is nothing to check the image against, and the machine
            // is booted on trust. This is the Phase-I case, where `BootImage::start`
            // already did exactly this and the build has one layout.
            let entry = image.boot_into(&mut self.machine)?;
            self.stage = BootStage::Booted;
            self.booted = Some(BootHandoff::new(entry));
            return Ok(entry);
        };

        match BootAgreement::check(profile, image) {
            BootAgreement::Agree => {}
            BootAgreement::Disagree {
                field,
                profile,
                image,
            } => {
                return Err(VmError::LayoutDisagreement {
                    field,
                    profile,
                    image,
                });
            }
            BootAgreement::RomTooLarge { image, window } => {
                return Err(VmError::RomTooLarge { image, window });
            }
            BootAgreement::Architecture { profile, image } => {
                return Err(VmError::ArchitectureDisagreement { profile, image });
            }
        }

        // The image's memory, the profile's devices.
        let mut setup = image.machine_setup(lazalith_devices::DeviceManager::new())?;
        setup.devices = profile.build_devices()?;
        setup.pc = profile.reset_vector();
        setup.sp = profile.kernel_stack_pointer();
        let mut machine = LazalithMachine::new(setup)?;
        for device in profile.devices() {
            machine.map_device(device.id, device.address, device.permissions)?;
        }
        machine.set_trap_vector(profile.trap_vector())?;
        machine.reset();

        let entry = image.boot_into(&mut machine)?;
        self.machine = machine;
        self.stage = BootStage::Booted;
        self.booted = Some(BootHandoff::new(entry));
        Ok(entry)
    }
}

impl<D: Device> Vm<D> {
    /// Adopts a machine that already exists.
    ///
    /// **The escape hatch, and it is deliberate.** Phase-I's boot path builds a
    /// `LazalithMachine<NoDevice>` through `BootImage::start`, and B6 does not replace
    /// that path. This is how such a machine gets a lifecycle.
    ///
    /// `stage` is the caller's to assert, which is the weakest thing in this file. It
    /// is a parameter rather than something computed because there is nothing to
    /// compute it from: a `LazalithMachine` does not record whether a bootloader ran,
    /// and B6 did not add that field to it — the fact belongs to the lifecycle, not
    /// to the machine. A caller that claims `Booted` when nothing booted gets a VM that
    /// believes it, which is a bug in the caller in the same class as every other
    /// precondition a caller meets.
    pub fn from_machine(machine: LazalithMachine<D>, stage: BootStage) -> Self {
        Self {
            machine,
            stage,
            profile: None,
            booted: None,
        }
    }

    /// The machine, borrowed.
    pub const fn machine(&self) -> &LazalithMachine<D> {
        &self.machine
    }

    /// The machine, mutably.
    ///
    /// Present because a debugger (B17) and a manager (B19) will need to reach the
    /// machine for things the lifecycle does not decide. It is a borrow, not a way
    /// out: everything that changes the *lifecycle* still goes through `Vm`.
    pub const fn machine_mut(&mut self) -> &mut LazalithMachine<D> {
        &mut self.machine
    }

    /// Where this machine is in the boot chain.
    pub const fn stage(&self) -> BootStage {
        self.stage
    }

    /// Whether this machine will execute.
    pub const fn machine_state(&self) -> MachineState {
        self.machine.state()
    }

    /// Whether the machine has halted.
    pub const fn is_halted(&self) -> bool {
        self.machine.is_halted()
    }

    /// The profile this VM was built from, if it was built from one.
    pub fn profile(&self) -> Option<&MachineProfile> {
        self.profile.as_ref()
    }

    /// Where the last boot left execution, if it has booted.
    pub const fn booted(&self) -> Option<BootHandoff> {
        self.booted
    }

    /// The virtual clock.
    pub const fn clock(&self) -> &VirtualClock {
        self.machine.clock()
    }

    // -- reset -------------------------------------------------------------

    /// Returns the machine to the state it was described in.
    ///
    /// **A reset always goes back to [`BootStage::Cold`].** That is the part worth
    /// being explicit about: a machine that had booted is not a booted machine after
    /// a reset, because booting ran firmware and loaded a kernel, and neither is undone
    /// by returning the processor to its reset vector. The RAM still holds the old
    /// kernel and the ROM still holds the bootloader, exactly as a real machine's
    /// would; what is gone is the *claim* that the handoff happened.
    ///
    /// its bootloader from its entry — a real and useful thing to do, and one a
    /// bootloader test needs. `stage` says `Cold` throughout, so a caller that meant to
    /// keep the boot has to say so by booting again.
    pub fn reset(&mut self) -> Result<(), VmError> {
        self.require_no_active_context("resetting")?;
        self.machine.reset();
        self.stage = BootStage::Cold;
        self.booted = None;
        Ok(())
    }
    /// One instruction, if the machine will execute.
    pub fn step(&mut self) -> Result<MachineEvent, VmError> {
        Ok(self.machine.step()?)
    }

    /// Runs up to `limit` instructions, or until it halts or traps.
    ///
    /// This is also how a paused machine resumes.
    /// [`lazalith_machine::LazalithMachine`] has `pause` and no separate `resume`,
    /// because pausing sets [`MachineState::Paused`], which is itself an executable
    /// state — so resuming *is* running again. A `resume` here would be a second
    /// spelling of one transition.
    pub fn run(&mut self, limit: u64) -> Result<MachineRun, VmError> {
        Ok(self.machine.run(limit)?)
    }

    /// Pauses a running machine.
    pub fn pause(&mut self) -> Result<(), VmError> {
        self.machine.pause()?;
        Ok(())
    }

    // -- mutation ----------------------------------------------------------

    /// Maps a device window, if no execution context is active.
    pub fn map_device(
        &mut self,
        id: lazalith_devices::DeviceId,
        start: PhysicalAddress,
        permissions: RegionPermissions,
    ) -> Result<(), VmError> {
        self.require_no_active_context("mapping a device window")?;
        self.machine.map_device(id, start, permissions)?;
        Ok(())
    }

    /// Loads a memory region, if no execution context is active.
    pub fn load_region(&mut self, region: MemoryRegion) -> Result<(), VmError> {
        self.require_no_active_context("loading a memory region")?;
        self.machine.load_region(region)?;
        Ok(())
    }

    // -- snapshot ----------------------------------------------------------

    /// Captures the machine.
    pub fn snapshot(&self) -> VmSnapshot {
        VmSnapshot::new(
            self.stage,
            self.machine.processor().clone(),
            self.machine.clock().elapsed(),
            self.machine.devices().snapshot(),
            self.machine.state(),
        )
    }

    /// Puts a snapshot back.
    ///
    /// **Time goes backwards, and getting that right was the hard part.** This method
    /// originally advanced the clock to the snapshot's time and refused a snapshot
    /// taken *earlier* than the machine had reached — which quietly made every snapshot
    /// useless, because you could snapshot, run, and then not restore, and undoing the
    /// running is the only reason to take one. The fix was in the machine, not the rule:
    /// `LazalithMachine::advance_clock` only moves forward, while devices had been able
    /// to rewind all along because each one's `restore` writes its own elapsed count
    /// straight back. The device manager was already rewindable and the machine was
    /// not, and nothing had noticed because nothing had tried.
    ///
    /// B6 added `LazalithMachine::restore_time`, which sets the processor's and the
    /// devices' clocks together, and the snapshot restored.
    ///
    /// Two refusals remain, both about what a restore cannot conjure:
    ///
    /// - **a snapshot from another stage.** A machine cannot become booted by being
    ///   restored, because booting ran firmware. The same reason a block device
    ///   refuses a snapshot taken against different storage: what is behind the
    ///   machine is part of what was saved.
    /// - **a different device count.** Device snapshots are restored by position, so
    ///   applying a two-device snapshot to a one-device machine would put the second
    ///   device's state into the first.
    ///
    /// B18 is where replay and determinism verification arrive. Neither refusal above
    /// is an artefact of this implementation; both are true at any snapshot level.
    pub fn restore(&mut self, snapshot: &VmSnapshot) -> Result<(), VmError> {
        let (stage, processor, clock, devices, state) = snapshot.clone().into_parts();
        if stage != self.stage {
            return Err(VmError::SnapshotStage {
                snapshot: stage,
                machine: self.stage,
            });
        }
        let device_count = self.machine.devices().len();
        if devices.len() != device_count {
            return Err(VmError::DeviceCount {
                snapshot: devices.len(),
                machine: device_count,
            });
        }
        // Everything is checked; now everything is written. `Processor::restore`
        // validates before it writes, so a snapshot that would produce an unreachable
        // processor state is refused rather than half-applied.
        //
        // The order is load-bearing. Devices first, then the clock: a device's elapsed
        // count comes from its own snapshot bytes, and `restore_time` sets the device
        // manager's clock *without* ticking anything — so doing the clock first would
        // be harmless today but reads as though ticking happened, and the day someone
        // adds one it becomes a double-advance.
        self.machine
            .processor_mut()
            .restore(processor)
            .map_err(|source| VmError::Machine(MachineError::Cpu(source)))?;
        self.machine.devices_mut().restore(&devices)?;
        self.machine.restore_time(clock);
        // The lifecycle state last, like the clock: it is the summary of everything
        // above it, and writing it before the processor and devices would leave a
        // machine reporting a state its own contents do not match if a later write
        // failed. B19 added this — without it a restore left a halted machine halted
        // with a running processor's registers.
        self.machine.restore_state(state)?;
        Ok(())
    }

    // -- helpers -----------------------------------------------------------

    fn require_no_active_context(&self, what: &'static str) -> Result<(), VmError> {
        if self.machine.active_execution_context().is_some() {
            return Err(VmError::MutationWhileRunning { what });
        }
        Ok(())
    }
}
