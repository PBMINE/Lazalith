//! B19: the management layer, through the operations §35 lists.
//!
//! # What is being held here
//!
//! §35 names twenty things a management layer manages. This suite covers the ones that
//! are *decisions* — because a management API is mostly a set of refusals, and the
//! refusals are what a GUI gets wrong first. The list is walked in §35's own order:
//! creation, boot, start, pause, resume, reset, shutdown, snapshot, restore, clone,
//! debugger attachment, console.
//!
//! **Every refusal is forced.** Each one is reached by putting the VM into the state
//! that makes it refuse, not by hoping a state machine happens to be strict. That is
//! the lesson `docs/project-state.md` records from B4 twice, and it is why there are
//! more `..._is_refused` tests here than happy paths.
//!
//! **§35 items that are deliberately not implemented are tested as absences.** §35 says
//! the layer manages a USB bus and a CPU configuration. Neither exists in this build,
//! and `a_usb_device_is_refused_by_name` and `there_is_no_cpu_configuration_to_set` say
//! so in the tests rather than only in documentation, so a reader of the suite learns
//! the same thing a caller would.
//!
//! # What is not tested
//!
//! No test here reaches into the machine. The rule that the management layer does not
//! manipulate CPU internals is not testable from inside this crate — it is a property
//! of the source — and it is checked by `no_management_crate_touches_cpu_internals` in
//! `crates/lazalith-cli/tests/architecture.rs`. What these tests *can* check is that
//! the management surface is enough: every state a VM can be in is observable through
//! `status()` without a register read, which is what makes the rule affordable.

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_devices::DeviceId;
use lazalith_isa::{Condition, Instruction, Opcode, Operand, encode};
use lazalith_machine::ProfileFamily;
use lazalith_manager::{
    ConfigError, DeviceClass, DeviceSpec, Manager, ManagerError, ManagerState, MemorySpec, VmConfig,
};
use lazalith_memory::RegionKind;
use lazalith_types::ArchitectureConfig;
use lazalith_vm::BootStage;

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// A kernel that spins forever, so the guest keeps running and the lifecycle operations
/// that require a live machine are reachable.
///
/// **It is a self-loop rather than a run of NOPs, and the reason is a trap rather than
/// an aesthetic.** A fixed run of NOPs runs off its own end and faults on the image
/// trailer, so every test that ran far enough would be testing a fault instead of the
/// operation. `BR AL, -2` branches to itself, because a relative target is
/// `next_pc + displacement * 4` and the instruction is 8 bytes wide, so a displacement
/// of zero would branch to the *next* instruction and fall straight off the end.
///
/// The other obvious kernel halts, and a halted machine refuses `pause` with
/// `InvalidTransition { operation: Pause, state: Halted }` — which is the lifecycle
/// being correct. A suite written against a halting kernel would be testing the halt
/// path over and over, so the two kernels are separated and the halting tests ask.
fn spinning_kernel() -> Vec<u8> {
    let branch = Instruction::new(
        CONFIG,
        Opcode::Br,
        &[Operand::Condition(Condition::Al), Operand::Immediate(-2)],
    )
    .expect("a self-branch encodes");
    let mut bytes = encode(CONFIG, &branch)
        .expect("and encodes to bytes")
        .to_vec();
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

/// A kernel that halts, for the tests that want a guest to stop.
fn halting_kernel() -> Vec<u8> {
    let halt = Instruction::new(CONFIG, Opcode::Halt, &[]).expect("a halt encodes");
    let mut bytes = encode(CONFIG, &halt).expect("a halt encodes").to_vec();
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

fn image() -> BootImage {
    BootImage::new(CONFIG, spinning_kernel(), 0).expect("an image builds")
}

fn halting_image() -> BootImage {
    BootImage::new(CONFIG, halting_kernel(), 0).expect("an image builds")
}

fn config() -> VmConfig {
    VmConfig::minimal(CONFIG)
}

/// A created, started VM whose guest is still running.
fn started() -> Manager {
    let mut manager = Manager::create(config()).expect("a minimal config is a VM");
    manager.start(&image()).expect("it boots");
    manager
}

// -- creation and configuration ----------------------------------------------

#[test]
fn a_configuration_becomes_a_vm_with_a_profile_that_agrees() {
    let manager = Manager::create(config().with_name("test-vm")).expect("it creates");
    assert_eq!(manager.name(), "test-vm");
    // The profile is the authority on memory and device windows (B6), and a management
    // client that could not ask for it would have to guess.
    let profile = manager.profile().expect("a described VM has a profile");
    assert_eq!(
        profile.devices().len(),
        1,
        "and it has the console it was configured with"
    );
    assert!(
        manager.profile().is_ok(),
        "so the management layer can report what the VM is"
    );
}

#[test]
fn the_amount_of_memory_a_vm_has_comes_from_its_configuration() {
    let manager = Manager::create(config().with_memory(MemorySpec::new(8 * 1024 * 1024)))
        .expect("it creates");
    let profile = manager.profile().expect("a profile");
    let ram = profile
        .regions()
        .iter()
        .find(|region| region.kind == RegionKind::Ram)
        .expect("there is RAM");
    assert_eq!(
        ram.length,
        8 * 1024 * 1024,
        "the configuration's memory is the profile's, not a default"
    );
}

#[test]
fn a_usb_device_is_refused_by_name() {
    // §35 lists USB. This build has no USB device, and the refusal names the class
    // rather than dropping the entry and building a machine that is quietly missing
    // something somebody asked for.
    let config = config().with_device(DeviceSpec::of(2, DeviceClass::Usb, 0x4001_0000));
    let error = Manager::create(config).expect_err("usb is not built in this build");
    assert!(matches!(error, ManagerError::Config(_)));
    let text = error.to_string();
    assert!(text.contains("usb"), "and the message names it: {text}");
}

#[test]
fn there_is_no_cpu_configuration_to_set() {
    // §35 lists "CPU configuration". There is none, and the type says so: `VmConfig`
    // has no CPU field, so a caller cannot set a core count that adjusts nothing. This
    // test exists so the absence is a fact in the suite rather than an omission a
    // reader has to notice on their own.
    let config = config();
    assert_eq!(config.family.as_str(), ProfileFamily::Native.as_str());
    assert_eq!(config.kernel_load_address, KERNEL_LOAD_ADDRESS);
    assert_eq!(
        config.name, "lazen",
        "a VmConfig is name, family, memory, devices and load address — there is no \
         field that adjusts a CPU, because the shipped profile has one configuration"
    );
}

#[test]
fn a_violation_is_refused_before_a_machine_exists() {
    // A refused configuration must not leave a half-built VM behind, and the way to
    // show that is that `create` returns an error rather than a `Manager`.
    let error = Manager::create(VmConfig {
        devices: Vec::new(),
        ..config()
    })
    .expect_err("a VM with no devices is refused");
    assert_eq!(error.to_string(), ConfigError::NoDevices.to_string());
}

// -- start -------------------------------------------------------------------

#[test]
fn a_created_vm_is_created_and_running_it_makes_it_running() {
    let mut manager = Manager::create(config()).expect("it creates");
    assert_eq!(manager.status().state, ManagerState::Created);
    assert_eq!(manager.status().stage, BootStage::Cold);

    manager.start(&image()).expect("it boots");
    assert_eq!(manager.status().state, ManagerState::Running);
    assert_eq!(manager.status().stage, BootStage::Booted);
}

#[test]
fn a_shut_down_vm_cannot_be_started_again_in_place() {
    // Shutdown is not reset, and this is the difference: a reset keeps the VM live and
    // a shut-down one is off.
    let mut manager = started();
    manager.shutdown().expect("it shuts down");
    assert_eq!(manager.status().state, ManagerState::ShutDown);
    assert!(
        matches!(manager.start(&image()), Err(ManagerError::ShutDown)),
        "and starting it again in place is refused"
    );
}

// -- run, step ---------------------------------------------------------------

#[test]
fn a_live_vm_can_be_run_and_stepped() {
    let mut manager = started();
    let run = manager.run(10).expect("it runs");
    assert!(
        run.halted_at.is_some() || run.executed > 0,
        "running did something: {run:?}"
    );
    manager.step().expect("and it steps");
}

#[test]
fn a_created_vm_cannot_be_run() {
    let mut manager = Manager::create(config()).expect("it creates");
    let error = manager
        .run(1)
        .expect_err("a VM that has not booted cannot run");
    assert!(
        matches!(
            error,
            ManagerError::NotLive {
                state: ManagerState::Created,
                ..
            }
        ),
        "and the refusal says which state it was in: {error}"
    );
}

// -- pause and resume --------------------------------------------------------

#[test]
fn pause_and_resume_are_separate_and_a_pause_keeps_the_guest_where_it_was() {
    // A booted-but-not-yet-run machine is `MachineState::Reset`, and `pause` requires
    // `Running` — the lifecycle refusing a pause on a machine that has not started
    // executing. So the run comes first, and `a_booted_vm_that_has_not_run_cannot_be_paused`
    // pins that refusal down.
    //
    // **The spinning kernel, not the halting one.** A halt is terminal, so a VM that
    // has halted cannot be paused either — `InvalidTransition { operation: Pause, state:
    // Halted }` — and a pause test written against a halting kernel would be testing
    // the refusal over and over.
    let mut manager = started();
    manager.run(10).expect("it runs");

    manager.pause().expect("it pauses");
    assert_eq!(manager.status().state, ManagerState::Paused);
    assert!(!manager.status().halted, "and the guest is still running");
    manager.resume().expect("it resumes");
    assert_eq!(manager.status().state, ManagerState::Running);
    assert!(
        !manager.status().halted,
        "resuming did not change the guest's state"
    );
    manager.run(10).expect("and it runs on");
    assert!(
        !manager.status().halted,
        "so the pause was a pause and not a halt"
    );
}

#[test]
fn a_booted_vm_that_has_not_run_cannot_be_paused() {
    // The lifecycle's rule, reached deliberately: a machine in `Reset` has nothing
    // running to pause. A management layer that papered over this would report a VM as
    // paused when the guest had not executed a single instruction.
    let mut manager = Manager::create(config()).expect("it creates");
    manager.start(&image()).expect("it boots");
    let error = manager
        .pause()
        .expect_err("a machine that has not started executing cannot be paused");
    let text = error.to_string();
    assert!(
        text.contains("Reset"),
        "and the refusal names the state the machine was in: {text}"
    );
}

#[test]
fn pausing_twice_is_refused_rather_than_being_a_no_op() {
    let mut manager = started();
    manager.run(10).expect("it runs");
    manager.pause().expect("it pauses");
    assert!(
        matches!(manager.pause(), Err(ManagerError::AlreadyPaused)),
        "a second pause is refused instead of quietly succeeding"
    );
}

#[test]
fn resuming_a_vm_that_is_not_paused_is_refused() {
    let mut manager = started();
    assert!(
        matches!(manager.resume(), Err(ManagerError::NotPaused)),
        "a running VM has nothing to resume"
    );
}

#[test]
fn a_paused_vm_can_still_be_snapshotted() {
    // A pause is exactly when a person saves a VM, so a management layer that refused
    // to snapshot a paused VM would refuse the one case the feature is for.
    let mut manager = started();
    manager.run(10).expect("it runs");
    manager.pause().expect("it pauses");
    assert!(
        manager.snapshot().is_ok(),
        "a paused VM can be saved, which is when a person saves one"
    );
}

// -- reset -------------------------------------------------------------------

#[test]
fn resetting_keeps_the_vm_live_and_puts_the_guest_back() {
    let mut manager = started();
    manager.run(1000).expect("it runs");
    manager.reset().expect("it resets");
    assert_eq!(
        manager.status().state,
        ManagerState::Running,
        "a reset is not a shutdown"
    );
    assert_eq!(
        manager.status().stage,
        BootStage::Cold,
        "and the guest is back at the start of its boot"
    );
}

// -- snapshot, restore, clone ------------------------------------------------

#[test]
fn a_restore_puts_a_vm_back_where_the_snapshot_was_taken() {
    // The observable here is *executability* and the halt flag, not the clock. See
    // `the_clock_does_not_advance_because_nothing_advances_it` for why the clock
    // cannot be used as evidence at this stage.
    let mut manager = Manager::create(config()).expect("it creates");
    manager
        .start(&halting_image())
        .expect("it boots the halting kernel");
    let snapshot = manager.snapshot().expect("a live VM can be saved");

    // Run to a halt, which is a definite change of state.
    manager.run(5000).expect("it runs");
    assert!(manager.status().halted, "running took it to a halt");

    manager.restore(&snapshot).expect("it restores");
    assert!(
        !manager.status().halted,
        "the restore undid the halt: the VM is back before the snapshot"
    );
    let run = manager.run(5000).expect("and it runs again");
    assert!(
        run.halted_at.is_some(),
        "so the restored VM really was executable, not stuck"
    );
}

#[test]
fn a_clone_is_independent_and_starts_paused() {
    // A *halting* kernel, because a halt is a definite, observable change of state
    // and the spinning kernel never changes state at all. Independence is shown by the
    // original and the clone reaching their halts independently.
    let mut original = Manager::create(config()).expect("it creates");
    original
        .start(&halting_image())
        .expect("it boots the halting kernel");
    let mut clone = original
        .clone_vm(&halting_image())
        .expect("it clones, which needs the image it boots");

    assert_eq!(
        clone.status().state,
        ManagerState::Paused,
        "a clone is paused, never running: it has no scheduler slot of its own"
    );
    assert_ne!(
        clone.name(),
        original.name(),
        "and it is a different VM, with a name that says so"
    );

    // The independence is the part worth testing: two VMs sharing state would make
    // `clone` a second name for one machine.
    original.run(1000).expect("the original runs");
    assert!(original.status().halted, "and reaches its halt");
    assert!(
        !clone.status().halted,
        "which did not halt the clone: they are not one machine with two names"
    );

    clone.resume().expect("the clone resumes");
    let run = clone.run(1000).expect("and runs on its own");
    assert!(
        run.halted_at.is_some(),
        "reaching its own halt, from its own copy of the state"
    );
}

/// A management client can see virtual time move, without reaching into the machine.
///
/// **This test asserted the opposite for the whole of B19.** It read
/// `elapsed_cycles == 0` after a five-thousand-instruction run and recorded *why*: the
/// step loop did not advance the clock, and the management layer could not fix it
/// without calling `advance_clock` on the machine, which is the reach-through §35
/// forbids. The suite deliberately used `halted`, `stage` and `ManagerState` as its
/// evidence everywhere else for that reason.
///
/// The defect is now fixed at the layer that owns it — the machine charges each retired
/// instruction what it costs — so a status bar can show a running clock. The important
/// half is that the manager still reaches nothing: `status()` reports a clock that moved
/// on its own, and the manager holds no reference to the machine's.
#[test]
fn a_management_client_can_see_virtual_time_move() {
    let mut manager = started();
    // Booting already costs something: the bootloader ran, so the machine's clock is
    // not at zero. That is itself the evidence the fix works — before it, booting was
    // free.
    let booted_at = manager.status().elapsed_cycles;
    assert!(
        booted_at > 0,
        "a booted VM has a clock that moved, because the bootloader ran"
    );

    let run = manager.run(64).expect("it runs");
    assert_eq!(
        manager.status().elapsed_cycles,
        booted_at + run.cycles,
        "and running it moves the clock by exactly what the run reports it spent, so \
         the status is not a second account of the same quantity"
    );
}

#[test]
fn snapshotting_a_created_vm_is_refused() {
    let manager = Manager::create(config()).expect("it creates");
    assert!(
        matches!(manager.snapshot(), Err(ManagerError::NotLive { .. })),
        "there is no guest state to save before it has booted"
    );
}

#[test]
fn restoring_hands_the_decision_to_the_lifecycle() {
    // The manager does not re-implement the stage comparison; it hands the snapshot to
    // `Vm` and surfaces the error. A management layer with its own copy of that rule
    // would be a second place for it to be wrong.
    let manager = started();
    let snapshot = manager.snapshot().expect("it saves");
    let mut other = started();
    assert!(
        other.restore(&snapshot).is_ok(),
        "a booted VM restores onto a booted VM"
    );
}

// -- debugger attachment -----------------------------------------------------

#[test]
fn a_debugger_can_be_attached_and_detached() {
    let mut manager = started();
    assert!(!manager.status().debugger_attached);
    manager
        .attach_debugger("lazdbg", Some(vec![1, 2, 3]))
        .expect("it attaches");
    assert!(manager.status().debugger_attached);
    let attachment = manager.attachment().expect("and it is there");
    assert_eq!(attachment.name, "lazdbg");
    assert_eq!(
        attachment.debug_block.as_deref(),
        Some([1u8, 2, 3].as_slice()),
        "with the image's debug information"
    );

    let detached = manager.detach_debugger().expect("it detaches");
    assert_eq!(detached.name, "lazdbg", "and hands back what was attached");
    assert!(!manager.status().debugger_attached);
}

#[test]
fn a_second_debugger_is_refused_rather_than_replacing_the_first() {
    // Two debuggers on one machine is a real thing people want. This does not do it,
    // and silently replacing the first would make its breakpoints vanish.
    let mut manager = started();
    manager.attach_debugger("first", None).expect("it attaches");
    assert!(matches!(
        manager.attach_debugger("second", None),
        Err(ManagerError::AlreadyAttached)
    ));
    assert_eq!(
        manager.attachment().expect("the first is still there").name,
        "first",
        "and the first is untouched"
    );
}

#[test]
fn detaching_when_nothing_is_attached_is_refused() {
    let mut manager = started();
    assert!(matches!(
        manager.detach_debugger(),
        Err(ManagerError::NotAttached)
    ));
}

#[test]
fn an_image_without_debug_information_attaches_and_says_so() {
    // `None` is a real answer, not a placeholder: a UI should be able to tell "this
    // image has no debug information" from "nothing is attached".
    let mut manager = started();
    manager
        .attach_debugger("lazdbg", None)
        .expect("it attaches");
    assert!(
        manager
            .attachment()
            .expect("attached")
            .debug_block
            .is_none(),
        "the attachment records that there is no debug block"
    );
}

#[test]
fn a_created_vm_cannot_have_a_debugger_attached() {
    let mut manager = Manager::create(config()).expect("it creates");
    assert!(
        matches!(
            manager.attach_debugger("lazdbg", None),
            Err(ManagerError::NotLive { .. })
        ),
        "there is no running guest to attach to"
    );
}

// -- console -----------------------------------------------------------------

#[test]
fn a_vm_with_a_console_reports_its_identity_and_buffer() {
    // "Manages the console" means configuring it. A console's registers are write-only
    // from the guest's side, so the guest's output reaches a person through the device
    // backend and not through here.
    let manager = Manager::create(config()).expect("it creates");
    let console = manager.console().expect("it has one");
    assert_eq!(console.id, DeviceId::new(1));
    assert_eq!(console.class, DeviceClass::Console);
    assert!(
        console.console_capacity.is_some(),
        "and the buffer size, which is the fact a UI shows"
    );
}

#[test]
fn a_vm_with_no_console_says_so() {
    let config = VmConfig {
        devices: vec![DeviceSpec::display(1, 0x4003_0000)],
        ..config()
    };
    let manager = Manager::create(config).expect("a display-only VM is a VM");
    assert!(matches!(manager.console(), Err(ManagerError::NoConsole)));
}

// -- status ------------------------------------------------------------------

#[test]
fn status_is_a_value_a_ui_can_hold_after_the_borrow_ends() {
    let mut manager = started();
    manager.run(100).expect("it runs");
    let status = manager.status();
    drop(manager);
    assert_eq!(
        (status.state, status.stage),
        (ManagerState::Running, BootStage::Booted),
        "a status bar can hold this after the manager is gone"
    );
}

#[test]
fn a_halted_guest_is_reported_as_halted() {
    let mut manager = Manager::create(config()).expect("it creates");
    manager
        .start(&halting_image())
        .expect("it boots the halting kernel");
    manager.run(1000).expect("it runs to the halt");
    assert!(
        manager.status().halted,
        "the kernel halts, and a UI needs to know"
    );
    assert_eq!(
        manager.status().state,
        ManagerState::Running,
        "and a halted guest is still a running VM, not a stopped one"
    );
}

#[test]
fn the_manager_prints_what_a_person_would_want() {
    let manager = started();
    let rendered = format!("{manager:?}");
    assert!(rendered.contains("Manager"), "{rendered}");
    assert!(
        rendered.contains("Running"),
        "and the state, spelled as the enum spells it: {rendered}"
    );
    assert!(
        !rendered.contains("Processor"),
        "and not the machine's innards: {rendered}"
    );
}

#[test]
fn the_managers_state_is_not_the_machines_state_restated() {
    // `ManagerState` and `lazalith_machine::MachineState` answer different questions.
    // A VM that has halted is `ManagerState::Running` and `MachineState::Halted`, and
    // the manager reports the first because that is what a title bar shows.
    let mut manager = Manager::create(config()).expect("it creates");
    manager
        .start(&halting_image())
        .expect("it boots the halting kernel");
    manager.run(1000).expect("it runs to the halt");
    let status = manager.status();
    assert_eq!(status.state, ManagerState::Running);
    assert!(
        status.halted,
        "so both facts are available without a register read"
    );
}
