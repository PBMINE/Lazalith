//! The management layer's operations: create, start, pause, reset, snapshot, clone.
//!
//! # The one rule this crate is built around
//!
//! §35 says the GUI and CLI consume the management API and that **neither directly
//! manipulates CPU internals**. That is a constraint on this crate's source, not a
//! convention, and it is checked as a fact by
//! `no_management_crate_touches_cpu_internals` in `crates/lazalith-cli/tests/architecture.rs`,
//! which fails if this crate so much as names `machine_mut`, `processor_mut`,
//! `devices_mut`, `architectural_mut` or `traps_mut`.
//!
//! The reason to check the source rather than review the diff is that the failure is
//! silent. A manager that called `machine_mut().processor_mut().architectural_mut()` to
//! "just set up a debug register" would compile, would pass every test that only ever
//! started and stopped a VM, and would be the first place guest state could be changed
//! without the lifecycle knowing — which is the property `Vm` exists to guarantee.
//!
//! Everything here is therefore expressed in terms the lifecycle already understands:
//! a stage, a run limit, a profile, a boot image. A caller that wants a register
//! written cannot ask this layer for it, which is the requirement.
//!
//! # What "a VM is doing" means here
//!
//! [`ManagerState`] is the manager's own lifecycle, and it is deliberately *not*
//! `lazalith_machine::MachineState` or `lazalith_vm::BootStage`. Those two answer
//! "would this machine execute" and "has firmware run"; this one answers "is a person
//! looking at a running, stopped or shut-off VM", which is the question a GUI's title
//! bar asks. `Stopped` and `Running` are different facts from `Running` and `Paused` in
//! the machine, and conflating them would let a UI claim a VM is running when nothing
//! is scheduled to run it.

#![deny(missing_docs)]

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use lazalith_boot::BootImage;
use lazalith_devices::Device;
use lazalith_types::InstructionAddress;
use lazalith_vm::{BootStage, Vm, VmError, VmSnapshot};

use crate::config::{ConfigError, VmConfig};

/// What a person would say about a VM they are looking at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagerState {
    /// Built, not yet started.
    Created,
    /// Started, and nothing is scheduled to run.
    Running,
    /// Started, and a run is not in progress but the guest is not reset either.
    Paused,
    /// Turned off. A shut-down VM keeps its configuration and can be started again,
    /// which is what makes `shutdown` different from `reset`.
    ShutDown,
}

impl ManagerState {
    /// The name a UI shows.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::ShutDown => "shut down",
        }
    }

    /// Whether a start, pause or resume is meaningful.
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Paused)
    }
}

impl fmt::Display for ManagerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A debugger attached to a VM, as far as the management layer is concerned.
///
/// **This is an attachment, not a stepping session, and the difference is the
/// limitation.** `lazalith_debug::DebugController` *owns* the machine it drives — it is
/// constructed by booting one — so it cannot be pointed at a VM this layer already
/// owns. Handing it a borrowed machine is a refactor of the same shape as B23's
/// interpreter↔JIT handoff, and is recorded as the work that would close it.
///
/// So what is attached here is the *debug information* plus a name: enough for a UI to
/// say "a debugger is attached", and enough that a future controller over a borrowed
/// machine has somewhere to hang. See `docs/project-state.md` for the B19 write-up.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugAttachment {
    /// The name the attaching tool gave itself.
    pub name: String,
    /// The image's debug block, if it has one.
    ///
    /// `None` is a real answer, not a placeholder: an image built without debug
    /// information attaches with no mappings, and a UI should say so rather than
    /// showing an empty source view as if the program had no functions.
    pub debug_block: Option<Vec<u8>>,
}

/// What a management client can see about a VM.
///
/// A value, not a reference, so a status bar or a `lazctl status` line can hold it
/// after the borrow ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VmStatus {
    /// Whether the VM is running, paused, created or shut down.
    pub state: ManagerState,
    /// Whether firmware has run.
    pub stage: BootStage,
    /// How much virtual time has passed.
    pub elapsed_cycles: u64,
    /// Whether the guest has halted.
    pub halted: bool,
    /// Whether a debugger is attached.
    pub debugger_attached: bool,
}

/// A managed VM.
///
/// The `Vm` inside is private, and there is no accessor that hands it out, because
/// §35's rule is that a management client does not manipulate CPU internals. What a
/// client can do is everything in this type.
pub struct Manager {
    name: String,
    config: VmConfig,
    vm: Vm<Box<dyn Device>>,
    state: ManagerState,
    attachment: Option<DebugAttachment>,
}

impl Manager {
    /// Builds a VM from a configuration.
    ///
    /// **The configuration is validated before anything is allocated.** A USB entry is
    /// refused here with its class in the message rather than producing a machine that
    /// is quietly missing a device somebody asked for.
    pub fn create(config: VmConfig) -> Result<Self, ManagerError> {
        let profile = config.to_profile().map_err(ManagerError::Config)?;
        let vm = Vm::described(&profile).map_err(ManagerError::Vm)?;
        Ok(Self {
            name: config.name.clone(),
            config,
            vm,
            state: ManagerState::Created,
            attachment: None,
        })
    }

    /// The VM's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The configuration this VM was created from.
    pub const fn config(&self) -> &VmConfig {
        &self.config
    }

    /// The machine profile the configuration produced, which is the authority on the
    /// memory and the device windows.
    pub fn profile(&self) -> Result<&lazalith_machine::MachineProfile, ManagerError> {
        self.vm.profile().ok_or(ManagerError::NotConfigurable)
    }

    /// Boots the VM with an image.
    pub fn start(&mut self, image: &BootImage) -> Result<InstructionAddress, ManagerError> {
        if self.state == ManagerState::ShutDown {
            return Err(ManagerError::ShutDown);
        }
        let entry = self.vm.boot(image).map_err(ManagerError::Vm)?;
        self.state = ManagerState::Running;
        Ok(entry)
    }

    /// Runs the guest for at most `limit` instructions.
    pub fn run(&mut self, limit: u64) -> Result<lazalith_machine::MachineRun, ManagerError> {
        self.require_live("run")?;
        self.vm.run(limit).map_err(ManagerError::Vm)
    }

    /// Steps the guest once.
    pub fn step(&mut self) -> Result<lazalith_machine::MachineEvent, ManagerError> {
        self.require_live("step")?;
        self.vm.step().map_err(ManagerError::Vm)
    }

    /// Pauses a running VM, leaving its state where it is.
    pub fn pause(&mut self) -> Result<(), ManagerError> {
        self.require_live("pause")?;
        if self.state == ManagerState::Paused {
            return Err(ManagerError::AlreadyPaused);
        }
        self.vm.pause().map_err(ManagerError::Vm)?;
        self.state = ManagerState::Paused;
        Ok(())
    }

    /// Resumes a paused VM.
    pub fn resume(&mut self) -> Result<(), ManagerError> {
        if self.state != ManagerState::Paused {
            return Err(ManagerError::NotPaused);
        }
        self.state = ManagerState::Running;
        Ok(())
    }

    /// Resets a live VM, keeping its configuration.
    pub fn reset(&mut self) -> Result<(), ManagerError> {
        self.require_live("reset")?;
        self.vm.reset().map_err(ManagerError::Vm)?;
        self.state = ManagerState::Running;
        Ok(())
    }

    /// Turns the VM off, keeping its configuration so it can be started again.
    ///
    /// **This is not `reset`.** A reset puts a live guest back at the start of its boot
    /// and the VM is still the same running thing; a shut-down VM is off, and starting
    /// it again re-runs the boot. A UI's "power off" is this and not that.
    pub fn shutdown(&mut self) -> Result<(), ManagerError> {
        self.vm.reset().map_err(ManagerError::Vm)?;
        self.state = ManagerState::ShutDown;
        Ok(())
    }

    /// Saves the VM's state.
    pub fn snapshot(&self) -> Result<VmSnapshot, ManagerError> {
        self.require_live("snapshot")?;
        Ok(self.vm.snapshot())
    }

    /// Restores a saved state.
    ///
    /// A restore is refused if the snapshot is from a different stage or another
    /// machine, and the refusal comes from `Vm` rather than being re-implemented here.
    /// A management layer that trusted its own comparison instead of the lifecycle's
    /// would be a second place for that rule to be wrong.
    pub fn restore(&mut self, snapshot: &VmSnapshot) -> Result<(), ManagerError> {
        self.require_live("restore")?;
        self.vm.restore(snapshot).map_err(ManagerError::Vm)
    }

    /// Makes an independent copy of this VM, stopped.
    ///
    /// **The clone is `Paused`, never `Running`.** A clone that reported itself running
    /// would be claiming a scheduler slot it does not have; the copy has the guest's
    /// state and nothing else.
    ///
    /// **It takes the boot image, and that is the lifecycle's stage rule, not a
    /// convenience.** B6 made `restore` refuse a snapshot whose stage does not match the
    /// machine's: a `Booted` snapshot cannot be restored onto a `Cold` machine, because
    /// restoring a booted guest's registers into a machine that has not run firmware
    /// would put a running kernel's state into a machine with no firmware behind it.
    /// A fresh machine is `Cold`, so a clone has to be booted before it can be restored
    /// — and that is a fact about `Vm` that a management layer cannot work around
    /// without re-implementing the stage rule, which is the one thing it must not do.
    ///
    /// A consequence worth stating: **cloning a VM requires the image it boots.** That
    /// is correct, and it is also the reason a management API that stored boot images
    /// per VM would make `clone` a one-argument call. Persisting the boot chain with the
    /// configuration is the natural fix and is not done here.
    pub fn clone_vm(&self, image: &BootImage) -> Result<Self, ManagerError> {
        self.require_live("clone")?;
        let snapshot = self.vm.snapshot();
        let profile = self
            .vm
            .profile()
            .ok_or(ManagerError::NotConfigurable)?
            .clone();
        let mut vm = Vm::described(&profile).map_err(ManagerError::Vm)?;
        // Boot the clone first so the machine is in the same stage as the snapshot;
        // the boot is then overwritten by the restore, and its only job is to make the
        // stage match.
        vm.boot(image).map_err(ManagerError::Vm)?;
        vm.restore(&snapshot).map_err(ManagerError::Vm)?;
        Ok(Self {
            name: format!("{} (clone)", self.name),
            config: self.config.clone(),
            vm,
            state: ManagerState::Paused,
            attachment: None,
        })
    }

    /// Attaches a debugger by name, with the image's debug information if it has any.
    ///
    /// **One attachment at a time**, and a second is refused rather than replacing the
    /// first. Two debuggers on one machine is a real thing people want, and it is not
    /// what this does; silently replacing the first would make the first one's
    /// breakpoints vanish.
    pub fn attach_debugger(
        &mut self,
        name: &str,
        debug_block: Option<Vec<u8>>,
    ) -> Result<(), ManagerError> {
        self.require_live("attach a debugger")?;
        if self.attachment.is_some() {
            return Err(ManagerError::AlreadyAttached);
        }
        self.attachment = Some(DebugAttachment {
            name: String::from(name),
            debug_block,
        });
        Ok(())
    }

    /// Detaches whatever debugger is attached.
    pub fn detach_debugger(&mut self) -> Result<DebugAttachment, ManagerError> {
        self.require_live("detach a debugger")?;
        self.attachment.take().ok_or(ManagerError::NotAttached)
    }

    /// The attachment, if there is one.
    pub const fn attachment(&self) -> Option<&DebugAttachment> {
        self.attachment.as_ref()
    }

    /// The VM's console, as it was configured.
    ///
    /// **"Manages the console" means configuring it, not reading the guest's output
    /// through it.** A console's registers are write-only from the guest's side, so
    /// there is nothing there for a management client to read: what the guest printed
    /// reaches a person through the *device backend* that was given to the machine,
    /// which is a backend question and not a management one. What the management layer
    /// owns is the console's identity and its buffer size, because those are the facts
    /// a UI shows and a configuration file records.
    ///
    /// An earlier draft of this returned the guest's output and always failed. That
    /// was a function whose name promised something no implementation could deliver,
    /// which is worse than not having it: a caller would have written a UI around an
    /// error message.
    pub fn console(&self) -> Result<&crate::config::DeviceSpec, ManagerError> {
        self.config
            .devices
            .iter()
            .find(|spec| spec.class == crate::config::DeviceClass::Console)
            .ok_or(ManagerError::NoConsole)
    }

    /// What a management client can see.
    pub fn status(&self) -> VmStatus {
        VmStatus {
            state: self.state,
            stage: self.vm.stage(),
            elapsed_cycles: self.vm.clock().elapsed().as_u64(),
            halted: self.vm.is_halted(),
            debugger_attached: self.attachment.is_some(),
        }
    }

    /// The lifecycle's own view, for a client that wants the machine's stage.
    pub const fn stage(&self) -> BootStage {
        self.vm.stage()
    }

    fn require_live(&self, what: &str) -> Result<(), ManagerError> {
        if self.state.is_live() {
            return Ok(());
        }
        Err(ManagerError::NotLive {
            state: self.state,
            operation: String::from(what),
        })
    }
}

impl fmt::Debug for Manager {
    /// Prints what a person would want, not the machine.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manager")
            .field("name", &self.name)
            .field("state", &self.state)
            .field("stage", &self.vm.stage())
            .field("elapsed_cycles", &self.vm.clock().elapsed().as_u64())
            .field("debugger", &self.attachment.as_ref().map(|a| &a.name))
            .finish()
    }
}

/// Why the management layer refused.
#[derive(Debug)]
pub enum ManagerError {
    /// The configuration is not a machine.
    Config(ConfigError),
    /// The lifecycle refused.
    Vm(VmError),
    /// A configuration was asked of a VM adopted rather than described.
    NotConfigurable,
    /// The VM has not been started, so the operation has nothing to act on.
    NotLive {
        /// The state it is in.
        state: ManagerState,
        /// What was being attempted.
        operation: String,
    },
    /// The VM is shut down, so it cannot be started again in place.
    ShutDown,
    /// The VM is already paused.
    AlreadyPaused,
    /// The VM is not paused, so there is nothing to resume.
    NotPaused,
    /// A debugger is already attached.
    AlreadyAttached,
    /// No debugger is attached.
    NotAttached,
    /// The configuration has no console.
    NoConsole,
}

impl fmt::Display for ManagerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Vm(error) => write!(f, "{error}"),
            Self::NotConfigurable => {
                f.write_str("this VM was adopted rather than described, so it has no profile")
            }
            Self::NotLive { state, operation } => {
                write!(f, "cannot {operation}: the VM is {state}")
            }
            Self::ShutDown => f.write_str("the VM is shut down"),
            Self::AlreadyPaused => f.write_str("the VM is already paused"),
            Self::NotPaused => f.write_str("the VM is not paused"),
            Self::AlreadyAttached => f.write_str("a debugger is already attached"),
            Self::NotAttached => f.write_str("no debugger is attached"),
            Self::NoConsole => f.write_str("this VM has no console"),
        }
    }
}
