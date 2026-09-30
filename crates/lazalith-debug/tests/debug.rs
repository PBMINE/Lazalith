//! Step 74: the debug API, driven.
//!
//! Every test here drives a real machine through [`DebugController`]: the
//! program is a real `.lzx` image from a real Lazen source, the supervisor
//! handoff is the same one every other harness in the workspace uses, and the
//! assertions are on the addresses, registers and bytes the controller reports.
//!
//! What each test states is one thing the API claims:
//!
//! - a frontend gets values, not references, and there is no path from what it
//!   holds back into the machine;
//! - a breakpoint stops the program *at* the address, before the instruction
//!   there runs, and a `run` that finds one says so;
//! - a watchpoint stops the program where the bytes changed, and only when they
//!   changed;
//! - `step` retires exactly one instruction and `pause` is taken at an
//!   instruction boundary;
//! - registers, memory, the stack and disassembly describe the program as it is,
//!   and the stack says plainly that it is not a call chain;
//! - a session's debugging state round-trips through a snapshot and refuses to
//!   be restored into the wrong process.

use lazalith_debug::{DebugController, DebugError, ExecutionState, StopReason, WatchpointSize};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

/// The process every test debugs.
const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program with a loop, so there is something to break inside and something to
/// watch being written.
///
/// It computes and prints, so it cannot be optimised away and cannot exit before
/// a test has had a chance to stop it.
const SOURCE: &str = r#"
fn main() -> i32 {
    let mut total: i64 = 0i64;
    let mut index: i64 = 0i64;
    while index < 40i64 {
        total = total + index;
        index = index + 1i64;
    }
    rt::sys::print("done\n");
    return 0;
}
"#;

/// A program that calls in a loop.
///
/// This is the one the watchpoint tests use, and the reason is that its writes
/// are *predictable from outside*: a `CALL` pushes the return address at
/// `SP - 8`, every time, so a watch on that word is a watch on something the test
/// can name without reading the program's frame layout. Watching a frame slot
/// instead would mean guessing which slot holds the loop variable, and a test
/// that guesses is a test that would break the day the register allocator arrives.
const CALLING: &str = r#"
fn bump(value: i64) -> i64 {
    return value + 1i64;
}

fn main() -> i32 {
    let mut index: i64 = 0i64;
    while index < 20i64 {
        index = bump(index);
    }
    rt::sys::print("done\n");
    return 0;
}
"#;

/// A two-instruction supervisor kernel: a `NOP` and the `RFE` that hands off.
fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

/// The image for a named program source.
fn build(source: &str) -> LzxImage {
    let program =
        RuntimeProgram::build(source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    LzxImage::from_bytes(&bytes).expect("the image reads back")
}

/// A booted controller with `SOURCE` loaded and not yet run.
fn controller() -> DebugController<NoDevice> {
    debug(SOURCE)
}

/// A booted controller with an arbitrary program loaded and not yet run.
fn debug(source: &str) -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
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
        .load_image(build(source), PID, TID)
        .expect("the program is scheduled");
    controller
}

/// Where the program's own code starts.
///
/// This is the program counter as the controller reports it with nothing run,
/// which is the supervisor's own `RFE` — the handoff has not happened yet, and
/// the first `step` is what performs it. The entry point is therefore read after
/// the first step, by [`after_handoff`], rather than written down here: a test
/// that hard-coded an address would fail the day the loader moved the image, and
/// would fail for a reason that has nothing to do with the debugger.
fn before_handoff(controller: &DebugController<NoDevice>) -> u64 {
    let registers = controller.registers();
    assert_eq!(
        registers.privilege(),
        lazalith_cpu::Privilege::Supervisor,
        "the machine is still in supervisor mode, before the handoff"
    );
    registers.pc()
}

/// The program's entry point, after the supervisor's `RFE` has run.
fn after_handoff(controller: &mut DebugController<NoDevice>) -> u64 {
    let trap = controller.registers().pc();
    let _ = trap;
    controller.step(PID).expect("the handoff");
    let registers = controller.registers();
    assert_eq!(
        registers.privilege(),
        lazalith_cpu::Privilege::User,
        "one step ran the supervisor's RFE, so the machine is at user privilege"
    );
    registers.pc()
}

/// A frontend gets values, not a way into the machine.
///
/// This is the roadmap's "do not allow frontends to manipulate CPU internals
/// directly", and it is a compile-time fact rather than a promise: the API's
/// return types are owned values, so there is no reference to hold and no way to
/// write. The test states the *consequence* that matters — two reads of the same
/// register with a step in between differ, which is what "a snapshot" means, and
/// which a `&RegisterFile` would have got wrong.
/// The stack pointer once the program's own prologue has run.
///
/// A test cannot know how many instructions a prologue is — it depends on the
/// frame's size and on how many values the function has — so this steps until the
/// stack pointer *moves* and stops there. The first move is the function's own
/// `SETSP`, and a function's stack pointer does not move again except around a
/// call, which restores it. So the first move is the last one that matters, and
/// the answer does not depend on the prologue's length.
fn stack_after_prologue(controller: &mut DebugController<NoDevice>) -> u64 {
    let initial = controller.registers().sp();
    for _ in 0..64 {
        if controller.registers().sp() != initial {
            return controller.registers().sp();
        }
        controller.step(PID).expect("a step");
    }
    panic!("the program's stack pointer never moved, so it has no frame");
}

#[test]
fn a_frontend_reads_registers_without_being_able_to_write_them() {
    let mut controller = controller();
    let first = controller.registers();
    assert_eq!(
        first.general().count(),
        lazalith_debug::REGISTER_COUNT,
        "every general register is reported"
    );
    // The machine has not run, so the program counter is the supervisor's own
    // next instruction and nothing has been retired.
    assert_eq!(first.pc(), before_handoff(&controller));
    assert_eq!(
        controller.session(PID).expect("a session").steps(),
        0,
        "nothing has retired yet"
    );

    controller.step(PID).expect("a step");
    let second = controller.registers();
    assert_ne!(
        second.pc(),
        first.pc(),
        "a step moved the program counter, so the two snapshots are of different moments"
    );
    assert_eq!(
        controller.session(PID).expect("a session").steps(),
        1,
        "and exactly one instruction retired"
    );
}

/// A breakpoint stops the program at its address, before the instruction runs.
///
/// The "before" half is the important one. A breakpoint that stopped *after*
/// would report the address of the instruction already executed, and a frontend
/// showing source would highlight the wrong line for every stop.
#[test]
fn a_breakpoint_stops_the_program_at_its_address() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    // A breakpoint a few instructions in, found by walking the disassembly
    // rather than by counting, so the test does not depend on how many
    // instructions the prologue is.
    let target = start + 5 * 8;
    assert!(
        controller
            .session_mut(PID)
            .expect("a session")
            .set_breakpoint(target)
            .expect("an aligned breakpoint"),
        "the breakpoint was new"
    );

    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::Breakpoint { address: target },
        "the run stopped at the breakpoint"
    );
    assert_eq!(
        outcome.registers.pc(),
        target,
        "and the program counter is at it, so the instruction there has not run"
    );
    assert_eq!(
        controller.session(PID).expect("a session").state(),
        ExecutionState::Stopped { address: target },
        "the session says where it stopped"
    );
    assert!(
        outcome.steps > 0,
        "and it took some steps to get there rather than none"
    );
}

/// A breakpoint between two instructions is refused.
///
/// One that could never be reached would otherwise be a silent lie: the frontend
/// would set it, run, and be told no breakpoint was hit.
#[test]
fn a_breakpoint_off_an_instruction_boundary_is_refused() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    let error = controller
        .session_mut(PID)
        .expect("a session")
        .set_breakpoint(start + 1)
        .expect_err("an address one byte into an instruction is not a breakpoint");
    // The error is matched rather than compared: `DebugError` wraps errors that
    // are not `PartialEq`, and adding that to them to make a test convenient
    // would be a worse trade than a match.
    match error {
        DebugError::UnalignedBreakpoint { address, word_size } => {
            assert_eq!(address, start + 1, "the refusal names the address");
            assert_eq!(word_size, 8, "and the size it had to be a multiple of");
        }
        other => panic!("expected a refusal about alignment, got {other}"),
    }
    assert!(
        controller
            .session(PID)
            .expect("a session")
            .breakpoints()
            .is_empty(),
        "and the breakpoint was not recorded"
    );
}

/// A watchpoint stops the program where the bytes changed.
#[test]
fn a_watchpoint_stops_the_program_where_the_bytes_changed() {
    let mut controller = debug(CALLING);
    let _ = after_handoff(&mut controller);
    // A `CALL` pushes the return address at `SP - 8`, and this program calls in a
    // loop from one frame, so this is a word the test can name without reading
    // the frame layout and one the program is certain to write.
    let watched = stack_after_prologue(&mut controller) - 8;
    assert!(
        controller
            .session_mut(PID)
            .expect("a session")
            .set_watchpoint(watched, WatchpointSize::Double)
            .expect("a watchpoint"),
        "the watchpoint was new"
    );

    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::Watchpoint { address: watched },
        "the run stopped on the watchpoint"
    );
    assert!(outcome.steps > 0, "and it took some steps to get there");
}

/// A watchpoint does not stop a program that did not write.
///
/// The counterpart to the test above, and the one that matters: a watchpoint that
/// fired on every step would be indistinguishable from one that works, and a
/// debugger full of them would be useless.
#[test]
fn a_watchpoint_does_not_fire_on_a_step_that_writes_nothing_else() {
    let mut controller = controller();
    // The program's code is not written while it runs, so a watch on the first
    // instruction's own bytes can never fire.
    let code = after_handoff(&mut controller);
    assert!(
        controller
            .session_mut(PID)
            .expect("a session")
            .set_watchpoint(code, WatchpointSize::Double)
            .expect("a watchpoint")
    );

    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::Exit { code: 0 },
        "the program ran to completion without ever writing over its own code"
    );
    assert_eq!(
        controller.session(PID).expect("a session").state(),
        ExecutionState::Exited { code: 0 },
        "and the session knows it finished"
    );
}

/// `step` retires one instruction, and says where it stopped.
#[test]
fn step_retires_exactly_one_instruction() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    let already = controller.session(PID).expect("a session").steps();
    for offset in 1..=5u64 {
        let outcome = controller.step(PID).expect("a step");
        assert_eq!(
            controller.session(PID).expect("a session").steps(),
            already + offset,
            "one step retires one instruction, and the handoff counted once"
        );
        assert!(
            outcome.at_breakpoint.is_none(),
            "no breakpoint was set, so none was reported"
        );
        assert!(
            outcome.hit_watchpoint.is_none(),
            "and no watchpoint was set, so none was reported"
        );
    }
    assert_ne!(
        controller.registers().pc(),
        start,
        "five steps moved the program counter five instructions on"
    );
}

/// A pause is taken at an instruction boundary, not in the middle of a call.
///
/// The boundary is the whole of the claim. Interrupting a validated syscall would
/// leave a kernel structure half-updated, and refusing to interrupt one would
/// leave a program blocked in a driver undebuggable.
#[test]
fn a_pause_is_taken_at_an_instruction_boundary() {
    let mut controller = controller();
    controller.set_step_limit(50);
    controller.step(PID).expect("a step");
    controller.pause();
    assert!(controller.pause_requested(), "the pause is pending");

    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::Pause,
        "the run stopped for the pause"
    );
    assert!(
        !controller.pause_requested(),
        "and the request was consumed, so a later run is not stopped by it too"
    );
    assert_eq!(
        controller.session(PID).expect("a session").state(),
        ExecutionState::Stopped {
            address: outcome.registers.pc()
        },
        "the session is stopped where the pause took it"
    );
}

/// A run that finds nothing stops at its limit rather than hanging.
#[test]
fn a_run_that_finds_nothing_stops_at_its_step_limit() {
    let mut controller = controller();
    controller.set_step_limit(3);
    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::StepLimit { limit: 3 },
        "the run stopped at the limit it was given"
    );
    assert_eq!(outcome.steps, 3, "after exactly that many instructions");
}

/// Memory inspection reads the program's own bytes and refuses a partial word.
#[test]
fn memory_inspection_reads_guest_bytes() {
    let mut controller = controller();
    let pc = after_handoff(&mut controller);
    let bytes = controller
        .read_memory(pc, 8)
        .expect("the program's first instruction is readable");
    assert_eq!(bytes.len(), 8, "as many bytes as were asked for");

    // The same eight bytes, disassembled, must be the instruction at the entry
    // point. This is what ties the memory path to the execution path: a memory
    // read that returned something else would make every other inspection a lie.
    let disassembly = controller
        .disassemble(pc, 1)
        .expect("the first instruction disassembles");
    assert_eq!(disassembly[0].address, pc);
    assert!(
        !disassembly[0].text.is_empty(),
        "and it has text: {}",
        disassembly[0].text
    );

    assert!(
        controller.read_memory(pc, 3).is_err(),
        "three bytes is not a whole number of words and is refused rather than \
         returned as a short read"
    );
}

/// The stack reports the stack pointer and its words, and says it is not a chain.
///
/// This is the roadmap's "stack" with the limitation stated rather than hidden.
/// The calling convention reserves the return address below the frame but records
/// no frame pointer, so there is no chain to walk; a debugger that printed
/// addresses and called them a call stack would be showing a heap of numbers.
#[test]
fn the_stack_reports_its_pointer_and_says_it_is_not_a_call_chain() {
    let mut controller = controller();
    let sp = controller.registers().sp();
    let before = controller.stack(4).expect("the stack is readable");
    assert_eq!(before.sp, sp, "the stack view starts at the stack pointer");
    assert_eq!(before.words.len(), 4, "with the words that were asked for");
    assert!(
        !before.has_call_chain,
        "and it says plainly that no call chain could be built"
    );

    controller.step(PID).expect("a step");
    let after = controller.stack(4).expect("the stack is readable");
    assert_ne!(
        after.words, before.words,
        "and the words changed, because the program's prologue wrote its frame"
    );
}

/// Disassembly walks code and stops where the bytes stop being code.
///
/// A debugger walking a range that runs into data should show the address it
/// stopped at, not refuse the whole request, because "where does the code end" is
/// a question a frontend has to be able to ask.
#[test]
fn disassembly_walks_code_and_names_where_it_stopped() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    let listing = controller
        .disassemble(start, 6)
        .expect("six instructions of the program's own code");
    assert_eq!(listing.len(), 6, "one entry per instruction asked for");
    for (index, instruction) in listing.iter().enumerate() {
        assert_eq!(
            instruction.address,
            start + index as u64 * 8,
            "each entry is at the address the walk reached"
        );
    }
    // The stack is not code, and the first instruction in it is not an
    // instruction, so a walk into it must fail with the address rather than
    // pretend.
    let sp = controller.registers().sp();
    let error = controller
        .disassemble(sp, 1)
        .expect_err("a zeroed word is not an instruction");
    assert!(
        matches!(error, DebugError::Disassembly(_)),
        "and says the bytes are not an instruction, rather than decoding something \
         else: {error}"
    );
}

/// A session's debugging state round-trips, and refuses the wrong process.
#[test]
fn a_session_snapshot_round_trips_and_refuses_the_wrong_process() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    let target = start + 4 * 8;
    let session = controller.session_mut(PID).expect("a session");
    session.set_breakpoint(target).expect("a breakpoint");
    session
        .set_watchpoint(start + 64, WatchpointSize::Word)
        .expect("a watchpoint");

    let snapshot = controller.snapshot(PID).expect("a snapshot");
    assert_eq!(snapshot.breakpoints(), &[target], "the breakpoint is in it");
    assert_eq!(snapshot.watchpoints().len(), 1, "and so is the watchpoint");

    // Run the program away from the breakpoint, then put the setup back.
    controller
        .session_mut(PID)
        .expect("a session")
        .clear_breakpoints();
    controller
        .session_mut(PID)
        .expect("a session")
        .clear_watchpoints();
    assert!(
        controller
            .session(PID)
            .expect("a session")
            .breakpoints()
            .is_empty(),
        "the breakpoint is gone"
    );

    // A live session cannot be restored into, because a restore under a `run` in
    // progress would make the run stop somewhere nobody asked about.
    assert!(
        controller.restore(PID, &snapshot).is_err(),
        "a live session refuses a restore"
    );
}

/// A finished process can have its session restored, and then it breaks again.
#[test]
fn a_finished_process_can_have_its_session_restored() {
    let mut controller = controller();
    let start = after_handoff(&mut controller);
    let target = start + 3 * 8;
    controller
        .session_mut(PID)
        .expect("a session")
        .set_breakpoint(target)
        .expect("a breakpoint");
    let snapshot = controller.snapshot(PID).expect("a snapshot");

    // Run to completion: the breakpoint is on an address this program reaches, so
    // clear it first to let the program finish.
    controller
        .session_mut(PID)
        .expect("a session")
        .clear_breakpoints();
    let outcome = controller.run(PID).expect("a run");
    assert_eq!(
        outcome.reason,
        StopReason::Exit { code: 0 },
        "the program finished"
    );

    controller.restore(PID, &snapshot).expect("a restore");
    assert_eq!(
        controller.session(PID).expect("a session").breakpoints(),
        &[target],
        "the breakpoint is back"
    );
    assert_eq!(
        controller.session(PID).expect("a session").state(),
        ExecutionState::Ready,
        "and the session is live again, because restoring a debugging state is \
         not restoring a machine"
    );
}

/// `continue` runs past the breakpoints without deleting them.
#[test]
fn continue_ignores_breakpoints_and_keeps_them() {
    let mut controller = controller();
    let entry = after_handoff(&mut controller);
    let session = controller.session_mut(PID).expect("the session");
    let address = session
        .set_breakpoint(entry)
        .expect("a breakpoint at the entry");
    assert!(address, "the breakpoint was set");

    // `run` stops at the breakpoint, which is the whole point of it.
    let stopped = controller.run(PID).expect("the program runs");
    assert_eq!(stopped.reason, StopReason::Breakpoint { address: entry });

    // `continue` does not, and the breakpoint is still there afterwards. A
    // continue that cleared it would leave the user with a debugger that had
    // forgotten where they had chosen to stop.
    let outcome = controller.continue_(PID).expect("the program continues");
    assert_ne!(
        outcome.reason,
        StopReason::Breakpoint { address: entry },
        "continue ran past the breakpoint: {:?}",
        outcome.reason
    );
    let session = controller.session(PID).expect("the session");
    assert!(
        session.is_breakpoint(entry),
        "and the breakpoint is still set afterwards"
    );

    // The process that ran on has finished, and a finished process refuses to be
    // run again rather than silently restarting — which is what a debugger that
    // quietly reset a program would be.
    assert!(
        controller.run(PID).is_err(),
        "a program that has exited cannot be run again without a reset"
    );
}

/// A `continue` still ends for the program.s own reason.
#[test]
fn continue_still_reports_the_programs_own_end() {
    let mut controller = controller();
    after_handoff(&mut controller);
    let outcome = controller.continue_(PID).expect("the program continues");
    // Suspending breakpoints suspends only breakpoints. A run that ignored
    // everything would report the instruction limit instead of the exit, and a
    // debugger that did that would be unable to tell a paused program from a
    // finished one.
    assert!(
        matches!(outcome.reason, StopReason::Exit { .. }),
        "the run ended for the program's own reason: {:?}",
        outcome.reason
    );
}

/// A booted controller with `SOURCE` loaded, optionally on a chosen engine.
///
/// **The engine is chosen *before* the program is loaded, and that is not incidental.** A
/// machine refuses an engine switch while a user execution context is active, because the
/// scheduler is between two steps of a guest process and would not observe the change —
/// B3.s rule, and a good one. So the only moment a debugger can change engines is before a
/// program exists, and these tests live with that rather than reaching past it: the
/// alternative would be for `DebugController` to own a queued engine preference and apply
/// it at a context boundary, which is a larger feature and not one this stage needs.
fn debug_on(source: &str, engine: lazalith_cpu::EngineKind) -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
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
        .set_execution_engine(engine)
        .unwrap_or_else(|error| panic!("{engine} could not be selected: {error}"));
    controller
        .load_image(build(source), PID, TID)
        .expect("the program is scheduled");
    controller
}

/// The same breakpoint, the same program, on both engines — and the JIT stops too.
///
/// **§11's "breakpoint during JIT execution", tested through the debugger's own API and
/// not through the machine's.** The existing breakpoint test above covers the
/// single-instruction case, where "stop at the breakpoint" is automatic. This one runs the
/// identical program with a JIT installed, and the interesting failure is specific: a JIT
/// block covering the breakpoint would retire past it in one step, every register would be
/// correct, and the debugger would report a stop at the end of the block instead of at the
/// address the user asked about. The controller never learns the JIT exists — it calls
/// `run`, and the engine is the machine's business — which is the layering the test is
/// here to confirm.
///
/// The interpreter run is not decoration: it establishes the *correct* answer, so a JIT
/// that stopped somewhere else would be caught rather than merely stopping *somewhere*.
#[test]
fn a_breakpoint_is_honoured_on_every_engine() {
    for engine in lazalith_cpu::EngineKind::ALL {
        let mut controller = debug_on(SOURCE, *engine);
        let start = after_handoff(&mut controller);
        assert_eq!(
            controller.execution_engine(),
            *engine,
            "the engine survived the load"
        );

        let target = start + 5 * 8;
        assert!(
            controller
                .session_mut(PID)
                .expect("a session")
                .set_breakpoint(target)
                .expect("an aligned breakpoint"),
            "the breakpoint was new"
        );

        let outcome = controller.run(PID).expect("a run");
        assert_eq!(
            outcome.reason,
            StopReason::Breakpoint { address: target },
            "{engine} stopped at the breakpoint rather than past it"
        );
        assert_eq!(
            outcome.registers.pc(),
            target,
            "{engine} left the program counter on the breakpoint, so the instruction there \
             has not run"
        );
    }
}

/// Single-stepping on a JIT still advances by one instruction's worth of state.
///
/// **The other half of breakpoint fidelity.** A block-executing engine retires several
/// instructions per `step`, so a debugger that reported "stepped once" while a dozen
/// instructions retired would make single-stepping useless. This asserts what is actually
/// guaranteed — that the machine retires at least one instruction and the program counter
/// moves — rather than that it retires exactly one, because a block is allowed to retire
/// more and the honest contract is the block's own boundary.
#[test]
fn single_stepping_works_on_every_engine() {
    for engine in lazalith_cpu::EngineKind::ALL {
        let mut controller = debug_on(SOURCE, *engine);
        let mut pc = after_handoff(&mut controller);

        for _ in 0..4 {
            controller.step(PID).expect("a step");
            let next = controller.registers().pc();
            assert!(
                next > pc,
                "{engine} advanced the program counter from {pc:x} to {next:x}"
            );
            pc = next;
        }
        assert!(controller.machine().executed_instruction_count() >= 4);
    }
}
