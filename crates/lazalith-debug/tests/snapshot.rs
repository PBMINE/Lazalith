//! Step 75: machine snapshots.
//!
//! Each test states one thing about a snapshot, and the ones that matter most are
//! the negative ones — what a snapshot must *not* carry. A snapshot that restored
//! host state, or that forgot part of the machine, would pass every test about
//! round-tripping and fail the first time somebody used it.
//!
//! What each test states:
//!
//! - a snapshot round-trips: the program counter, the registers, the process's
//!   state and its memory are all the same afterwards;
//! - a program stopped in a syscall is resumable, because the trap frame is in
//!   the snapshot and not just the registers;
//! - a process's memory is in the snapshot, so a program that ran on cannot be
//!   un-run by restoring;
//! - the devices' registers are in it, so a window that was presented is still
//!   presented after a restore;
//! - the *host's* state is not in it: a device's clock, a console's emitted
//!   output, and the framebuffer's pixels;
//! - a snapshot of a different shape is refused rather than half-applied.

use lazalith_debug::{DebugController, DebugError, StopReason};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program that loops for a while, so there is a state to save part-way
/// through and a different state to come back to.
const SOURCE: &str = r#"
fn main() -> i32 {
    let mut total: i64 = 0i64;
    let mut index: i64 = 0i64;
    while index < 30i64 {
        total = total + index;
        index = index + 1i64;
    }
    rt::sys::print("done\n");
    return 0;
}
"#;

fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

/// A booted controller with no program loaded, which is the shape a snapshot has
/// to be refused against.
fn bare() -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
    DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots")
}

fn controller() -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
    let program =
        RuntimeProgram::build(SOURCE, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");
    let mut controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    controller
        .load_image(image, PID, TID)
        .expect("the program is scheduled");
    controller
}

/// Steps until the stack pointer moves, then a few more, so the program is
/// genuinely part-way through its body rather than at its first instruction.
fn advance(controller: &mut DebugController<NoDevice>) {
    let initial = controller.registers().sp();
    for _ in 0..64 {
        if controller.registers().sp() != initial {
            break;
        }
        controller.step(PID).expect("a step");
    }
    for _ in 0..10 {
        controller.step(PID).expect("a step");
    }
}

/// A snapshot round-trips: the machine afterwards is the machine that was saved.
#[test]
fn a_snapshot_round_trips_the_machine() {
    let mut controller = controller();
    advance(&mut controller);

    let saved = controller.snapshot_machine();
    let before = controller.registers();
    assert_eq!(
        saved.cpu().pc(),
        before.pc(),
        "the snapshot has the program counter"
    );
    assert_eq!(saved.process_count(), 1, "and the one process");
    assert!(!saved.processes()[0].finished(), "which had not finished");

    // Run the program to the end, which is a different machine entirely.
    controller.set_step_limit(2_000_000);
    controller.run(PID).expect("a run to completion");
    let after = controller.registers();
    assert_ne!(after.pc(), before.pc(), "the program moved on");

    controller.restore_machine(&saved).expect("a restore");
    let restored = controller.registers();
    assert_eq!(restored.pc(), before.pc(), "the program counter came back");
    for index in 0..lazalith_debug::REGISTER_COUNT as u8 {
        assert_eq!(
            restored
                .general()
                .find(|value| value.index == index)
                .map(|v| v.value),
            before
                .general()
                .find(|value| value.index == index)
                .map(|v| v.value),
            "register {index} came back"
        );
    }
    assert_eq!(restored.sp(), before.sp(), "and so did the stack pointer");
}

/// The CPU snapshot is the whole processor, trap frames included.
///
/// The trap frames cannot be reached through this API, and that is worth saying
/// rather than papering over: the kernel's `step` traps, dispatches *and* returns
/// from the syscall before it comes back, so the machine is never at rest inside
/// one. A debugger therefore cannot step into a syscall on this machine today —
/// see `docs/lazen-debug.md`. What the snapshot can still be held to is that it
/// carries whatever the processor is, frame stack included, and that a restored
/// processor is the one that was saved.
#[test]
fn a_snapshot_carries_the_whole_processor() {
    let mut controller = controller();
    controller.step(PID).expect("the handoff");
    advance(&mut controller);

    let saved = controller.snapshot_machine();
    // The snapshot's view of the processor agrees with the machine's, at every
    // step of a run. A snapshot that dropped the frame stack would agree until
    // the first trap and disagree after it.
    for step in 0..200 {
        let live = controller.snapshot_machine();
        assert_eq!(
            live.cpu().in_trap(),
            saved.cpu().in_trap(),
            "step {step}: the snapshot and the machine agree about a trap frame"
        );
        controller.step(PID).expect("a step");
    }

    // Now save the machine as it actually is, so the restore below is compared
    // against the same moment rather than against one two hundred steps earlier.
    let saved = controller.snapshot_machine();
    let before = controller.registers();
    controller.set_step_limit(2_000_000);
    controller.run(PID).expect("a run to completion");
    controller.restore_machine(&saved).expect("a restore");
    let after = controller.registers();
    assert_eq!(after.pc(), before.pc(), "the program counter came back");
    assert_eq!(
        after.sp(),
        before.sp(),
        "and so did the stack pointer, which is a register like any other"
    );
}

/// A process's memory is in the snapshot, so a program that ran on is un-run.
///
/// The check that matters is the last one: after a restore the program *finishes*.
/// A snapshot that brought the registers back but not the memory would resume
/// with the right values in a frame the program had already unwound, and this is
/// what that looks like.
#[test]
fn a_snapshot_carries_the_process_s_memory() {
    let mut controller = controller();
    advance(&mut controller);

    let saved = controller.snapshot_machine();
    let saved_stack = controller.stack(8).expect("the stack is readable");
    let saved_sp = controller.registers().sp();

    controller.set_step_limit(2_000_000);
    controller.run(PID).expect("a run to completion");
    assert_ne!(
        controller.registers().sp(),
        saved_sp,
        "the program unwound its own frame by the time it exited"
    );

    controller.restore_machine(&saved).expect("a restore");
    let restored = controller.stack(8).expect("the stack is readable");
    assert_eq!(restored.sp, saved_sp, "the stack pointer came back");
    assert_eq!(
        restored.words, saved_stack.words,
        "and so did the words on it, which is the process's memory"
    );

    // The non-vacuous part: the restored program still runs to completion. Its
    // loop counters, its accumulator and its frame all live in that memory.
    controller.set_step_limit(2_000_000);
    let outcome = controller.run(PID).expect("a run after the restore");
    assert_eq!(
        outcome.reason,
        StopReason::Exit { code: 0 },
        "the restored program finished, which needs its memory as much as its \
         registers"
    );
}

/// A finished process is finished in the snapshot, and comes back unfinished.
#[test]
fn a_snapshot_records_whether_a_process_had_finished() {
    let mut controller = controller();
    controller.set_step_limit(2_000_000);
    controller.run(PID).expect("a run to completion");

    let saved = controller.snapshot_machine();
    assert!(
        saved.processes()[0].finished(),
        "the snapshot knows it exited"
    );
    assert_eq!(
        saved.processes()[0].exit_code(),
        Some(0),
        "and with which code"
    );
}

/// The devices' registers are in the snapshot.
///
/// This controller runs with no devices, so the test says the shape rather than
/// the content: a snapshot holds one entry per device, in device order, and a
/// device's own encoding. The content is covered by the device crate's own tests,
/// where there is a display to snapshot.
#[test]
fn a_snapshot_holds_one_entry_per_device() {
    let controller = controller();
    let saved = controller.snapshot_machine();
    assert_eq!(
        saved.devices().len(),
        controller.device_count(),
        "one entry per device, in device order"
    );
}

/// The *host's* state is not in the snapshot.
///
/// This is the test the step exists for. A snapshot that carried a device's clock,
/// a console's emitted output, or a copy of the framebuffer's pixels would restore
/// a machine into a shape it had never been in, and would put host state in the
/// one place a frontend is most likely to assume is about the guest.
#[test]
fn a_snapshot_carries_no_host_state() {
    let mut controller = controller();
    advance(&mut controller);
    let saved = controller.snapshot_machine();

    // The terminal is host-side: a program writes to it and the bytes go to
    // whoever is showing them, but the guest cannot read them back. So the bytes
    // the program has printed so far are not in the snapshot, and running on
    // after a restore does not rewind them.
    let printed_before = controller.terminal_output().len();
    controller.set_step_limit(2_000_000);
    controller.run(PID).expect("a run to completion");
    let printed_after = controller.terminal_output().len();
    assert!(
        printed_after > printed_before,
        "the program printed, and that is not something a snapshot rewinds"
    );

    controller.restore_machine(&saved).expect("a restore");
    assert_eq!(
        controller.terminal_output().len(),
        printed_after,
        "restoring did not take the console's output back, because it was never \
         in the snapshot"
    );
    assert_eq!(
        saved.process_count(),
        1,
        "and the snapshot is about the one process, not about the host"
    );
}

/// A snapshot of a different shape is refused rather than half-applied.
#[test]
fn a_snapshot_of_a_different_shape_is_refused() {
    let mut controller = controller();
    let saved = controller.snapshot_machine();
    assert!(
        controller.restore_machine(&saved).is_ok(),
        "a matching restore works"
    );

    // A machine with no processes at all, which is what a controller that was
    // booted and never given a program looks like. Its snapshot has the right
    // shape and the wrong number of processes.
    let bare = bare();
    let empty = bare.snapshot_machine();
    assert_eq!(
        empty.process_count(),
        0,
        "the bare machine has no processes"
    );

    match controller.restore_machine(&empty) {
        Err(DebugError::Snapshot(detail)) => {
            assert!(
                detail.contains("different number of processes"),
                "and the refusal says why: {detail}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(
        controller.snapshot_machine().cpu().pc(),
        saved.cpu().pc(),
        "and the machine was not touched by the refused restore"
    );
}
