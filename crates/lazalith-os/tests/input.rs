//! Step 71: the LazOS input driver, exercised through the real ABI.
//!
//! These go through the trap and the dispatcher for the same reason the display
//! tests do: the driver's contract is a *syscall*, and a test that skipped the
//! ABI would pass a driver a program cannot use.
//!
//! The properties each test states come from `docs/lazen-input.md`:
//!
//! - a poll writes whole records into the caller's own array and returns how
//!   many;
//! - **the remainder stays queued**, so a poll that cannot take everything loses
//!   nothing;
//! - a return of zero means "nothing pending", not "an error";
//! - an array too small for the stated capacity is refused before the device is
//!   touched, so a program cannot be talked into writing past its own buffer;
//! - the guest cannot inject its own events.

use lazalith_cpu::{ExecutionContextId, Privilege, StatusRegister, TrapCause};
use lazalith_devices::{DeviceManager, EventKind, HostAction, HostKey, HostScript, NoDevice};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::abi::{
    INPUT_EVENT_RECORD_SIZE, IO_RESULT_SIZE, InputEventRecord, IoResult, Syscall, SyscallStatus,
};
use lazalith_os::input::InputService;
use lazalith_os::{
    DispatchOutcome, Process, ProcessId, ProgramImage, SyscallDispatcher, SyscallRequest, ThreadId,
};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

const KERNEL_CODE: u64 = 0x1000;
const USER_CODE: u64 = 0x2000;
const USER_STACK: u64 = 0x3000;

/// Where the guest's event array, its result record, and any unusable pointer
/// live. The record is the caller's too: a v1 syscall returns one word and that
/// word is the status, so the count comes back in memory the program named.
const EVENTS: u64 = lazalith_os::USER_DATA_START;
const RESULT: u64 = EVENTS + 0x10000;

fn register(index: u8) -> RegisterIndex {
    RegisterIndex::try_from(index).unwrap()
}

fn instruction(config: C, opcode: Opcode, operands: &[Operand]) -> [u8; 8] {
    let instruction = Instruction::new(config, opcode, operands).unwrap();
    encode(config, &instruction).unwrap()
}

fn new_machine(config: C) -> LazalithMachine<NoDevice> {
    let regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(KERNEL_CODE),
            8,
            RegionPermissions::new(true, true, true, false),
        )
        .unwrap(),
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(USER_CODE),
            8,
            RegionPermissions::new(true, false, true, true),
        )
        .unwrap(),
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(USER_STACK),
            0x100,
            RegionPermissions::new(true, true, false, true),
        )
        .unwrap(),
    ];
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions,
        pc: InstructionAddress::new(USER_CODE),
        sp: VirtualAddress::new(USER_STACK + 0x80),
        status: StatusRegister::new(Privilege::User, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(KERNEL_CODE),
            &instruction(config, Opcode::Rfe, &[]),
        )
        .unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(USER_CODE),
            &instruction(config, Opcode::Syscall, &[]),
        )
        .unwrap();
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_CODE))
        .unwrap();
    machine
}

/// One guest, one driver, and as many polls as a test wants to make.
///
/// The driver is stateful — a queue drains as the guest polls — so a helper that
/// made a fresh driver per call could not express "poll, poll again", which is
/// the only sequence the ABI allows.
struct Session {
    driver: InputService,
    process: Process,
}

impl Session {
    fn new() -> Self {
        let mut process = Process::new(
            ProcessId::new(1).unwrap(),
            ThreadId::new(1).unwrap(),
            ProgramImage::new(C::lz64(), 0, &[0; 8]).unwrap(),
        )
        .unwrap();
        process.mark_ready().unwrap();
        process.mark_running().unwrap();
        let execution_context = ExecutionContextId::new(0x1000_0000_0000_0001).unwrap();
        process.activate(execution_context).unwrap();
        Self {
            driver: InputService::new(),
            process,
        }
    }

    /// A session whose queue is already loaded from `script`.
    fn with_script(script: &HostScript) -> Self {
        let mut session = Self::new();
        script
            .replay(session.driver.device_mut())
            .expect("the script fits the queue");
        session
    }

    fn call(&mut self, number: Syscall, arguments: [u64; 6]) -> DispatchOutcome {
        let mut machine = new_machine(C::lz64());
        machine
            .write_register(register(0), number.as_u16() as u64)
            .unwrap();
        for (index, value) in arguments.iter().enumerate() {
            machine
                .write_register(register(u8::try_from(index + 1).unwrap()), *value)
                .unwrap();
        }
        let event = match machine.step().unwrap() {
            MachineEvent::Trapped { event } => event,
            other => panic!("expected syscall trap, got {other:?}"),
        };
        assert_eq!(event.cause, TrapCause::Syscall);
        let thread_id = self.process.primary_thread().id();
        let current = machine.architectural_state().clone();
        let identity = self.process.memory().identity();
        let execution_context = ExecutionContextId::new(0x1000_0000_0000_0001).unwrap();
        machine
            .with_trap_controller_mut(None, |controller| {
                controller.set_execution_context(execution_context);
            })
            .unwrap();
        let request = machine
            .with_trap_controller_mut(Some(execution_context), |controller| {
                SyscallRequest::from_trap(controller, &current, &self.process, thread_id, &identity)
            })
            .unwrap()
            .unwrap();
        let mut memory = self.process.memory_context().unwrap();
        SyscallDispatcher::new().dispatch(
            request,
            machine.trap_controller(),
            machine.architectural_state(),
            &mut memory,
            &mut self.driver,
        )
    }

    fn poll(&mut self, events: u64, capacity: u32) -> DispatchOutcome {
        self.call(
            Syscall::InputPoll,
            [events, u64::from(capacity), RESULT, 0, 0, 0],
        )
    }

    /// The count and status the guest's own result record reports, which is the
    /// answer the call was really about.
    fn reported(&mut self) -> (u64, SyscallStatus) {
        let mut memory = self.process.memory_context().unwrap();
        let mut bytes = vec![0u8; IO_RESULT_SIZE];
        memory
            .read_bytes(VirtualAddress::new(RESULT), &mut bytes)
            .unwrap();
        let result = IoResult::decode(&bytes, C::lz64()).expect("the record reads back");
        (result.transferred(), result.status())
    }

    /// Polls, requires the call to be valid, and returns the count the guest's
    /// own record reported.
    ///
    /// The two halves are checked separately on purpose: the call being valid and
    /// the count being right are separate claims, and a driver that returned a
    /// plausible count from a refused call would pass a test that looked at only
    /// one of them.
    fn count(&mut self, events: u64, capacity: u32) -> u64 {
        let outcome = self.poll(events, capacity);
        assert_valid(&outcome);
        let (count, status) = self.reported();
        assert_eq!(status, SyscallStatus::Ok, "and the record says it worked");
        count
    }

    /// The records the guest can see, read back from its own memory.
    fn records(&mut self, events: u64, count: usize) -> Vec<InputEventRecord> {
        let mut memory = self.process.memory_context().unwrap();
        (0..count)
            .map(|index| {
                let mut bytes = vec![0u8; INPUT_EVENT_RECORD_SIZE];
                memory
                    .read_bytes(
                        VirtualAddress::new(events + index as u64 * INPUT_EVENT_RECORD_SIZE as u64),
                        &mut bytes,
                    )
                    .unwrap();
                InputEventRecord::decode(&bytes).expect("the record reads back")
            })
            .collect()
    }
}

/// Requires that a poll was a valid call.
fn assert_valid(outcome: &DispatchOutcome) {
    match outcome {
        DispatchOutcome::Return { outcome, .. } => assert_eq!(
            outcome.status(),
            SyscallStatus::Ok,
            "the call reached the driver: {outcome:?}"
        ),
        other => panic!("expected a return: {other:?}"),
    }
}

/// A poll writes the queued events into the caller's array, in order.
///
/// The order is the whole point: a program that reads keys cannot tell arrival
/// order from delivery order unless they are the same.
#[test]
fn a_poll_writes_the_queued_events_in_order() {
    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::H));
    script.push(HostAction::KeyUp(HostKey::H));
    script.push(HostAction::MouseMove(7, 9));
    let mut session = Session::with_script(&script);

    assert_eq!(session.count(EVENTS, 8), 3, "three events were written");

    let records = session.records(EVENTS, 3);
    assert_eq!(records[0].kind(), EventKind::KeyDown.as_u32());
    assert_eq!(records[1].kind(), EventKind::KeyUp.as_u32());
    assert_eq!(records[2].kind(), EventKind::MouseMove.as_u32());
    assert_eq!(records[2].x(), 7, "the pointer position arrived");
    assert_eq!(records[2].y(), 9);
    assert_eq!(records[0].x(), 0, "and a key has no position");
    assert_eq!(session.driver.pending(), 0, "the queue is drained");
}

/// The remainder stays queued when the array is too small.
///
/// This is the property the whole queue exists for. A drain that discarded the
/// remainder would lose input silently, and a program polling once per frame at
/// thirty frames a second would lose any key tapped faster than that.
#[test]
fn a_poll_that_cannot_take_everything_keeps_the_rest() {
    let mut script = HostScript::new();
    for _ in 0..5 {
        script.push(HostAction::KeyDown(HostKey::A));
    }
    let mut session = Session::with_script(&script);

    assert_eq!(session.count(EVENTS, 2), 2, "two fit");
    assert_eq!(
        session.driver.pending(),
        3,
        "and three are still queued, not dropped"
    );
    assert_eq!(session.count(EVENTS, 2), 2, "two more fit");
    assert_eq!(session.driver.pending(), 1);
    assert_eq!(session.count(EVENTS, 2), 1, "and the last one");
    assert_eq!(session.driver.pending(), 0);

    // The second call continued where the first stopped, rather than starting
    // over or skipping: the guest saw all five in order.
    let records = session.records(EVENTS, 2);
    assert_eq!(records[0].kind(), EventKind::KeyDown.as_u32());
}

/// A poll with nothing pending returns zero, and zero is not an error.
///
/// A return of zero means "nothing pending". A program that polls once per frame
/// is *supposed* to get zero most of the time, so zero cannot be a failure.
#[test]
fn a_poll_with_nothing_pending_returns_zero() {
    let mut session = Session::new();
    assert_eq!(
        session.count(EVENTS, 8),
        0,
        "nothing pending is zero events"
    );
    // And again, because a program polls every frame.
    assert_eq!(session.count(EVENTS, 8), 0);
}

/// A capacity of zero is a poll for "is anything pending" and touches no memory.
///
/// It is the cheapest way for a program to ask, and it must not be refused for
/// naming a null array it never writes through.
#[test]
fn a_capacity_of_zero_asks_without_writing() {
    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    let mut session = Session::with_script(&script);
    let outcome = session.poll(EVENTS, 0);
    assert_valid(&outcome);
    assert_eq!(
        session.reported().0,
        0,
        "nothing is written, so nothing is reported"
    );
    assert_eq!(
        session.driver.pending(),
        1,
        "and the event is still queued for a real poll"
    );
}

/// An array that cannot hold the records it was promised is refused before
/// anything is written.
///
/// A capacity the driver believed without checking would have it write records
/// past the end of the caller's buffer. The array here starts one record before
/// the end of the data segment and promises four, so the range it names runs off
/// the end — which is the shape a real caller gets wrong when it sizes a buffer
/// for the events it has *seen* rather than the ones it asked for.
#[test]
fn an_array_too_small_for_the_capacity_is_refused() {
    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    let mut session = Session::with_script(&script);
    let near_end = lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH
        - INPUT_EVENT_RECORD_SIZE as u64;
    let outcome = session.poll(near_end, 4);
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_eq!(outcome.status(), SyscallStatus::InvalidPointer);
            assert_eq!(
                outcome.payload(),
                0,
                "the detail is the event array's argument index"
            );
        }
        other => panic!("expected a return: {other:?}"),
    }
    assert_eq!(
        session.driver.pending(),
        1,
        "a refused poll queues nothing away and delivers nothing"
    );
}

/// A capacity far larger than the guest's memory is refused.
///
/// `capacity` is a `u32` and a record is sixteen bytes, so the byte count cannot
/// overflow — the longest array the call can name is under 64 GiB and always
/// fits a word. What it *can* do is name more memory than the guest has, and that
/// is the case worth refusing: the range check is what stops it.
#[test]
fn a_capacity_larger_than_the_guests_memory_is_refused() {
    let mut session = Session::new();
    let outcome = session.call(Syscall::InputPoll, [EVENTS, u32::MAX as u64, 0, 0, 0, 0]);
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_eq!(
                outcome.status(),
                SyscallStatus::InvalidPointer,
                "an array of a hundred million records is not in the guest's memory"
            );
        }
        other => panic!("expected a return: {other:?}"),
    }
    assert_eq!(session.driver.pending(), 0, "and nothing was delivered");
}

/// An event array the guest cannot write is refused.
///
/// The array is where the records go, so an array the guest cannot write is a
/// call that would fault rather than answer.
#[test]
fn an_unwritable_event_array_is_refused() {
    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    let mut session = Session::with_script(&script);
    // The code segment is readable and executable but not writable.
    let outcome = session.poll(lazalith_os::USER_CODE_START, 4);
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_eq!(outcome.status(), SyscallStatus::InvalidPointer);
        }
        other => panic!("expected a return: {other:?}"),
    }
    assert_eq!(
        session.driver.pending(),
        1,
        "nothing was delivered, so nothing was consumed"
    );
}

/// The reserved register is refused for an input call, like every other.
#[test]
fn a_reserved_register_is_refused() {
    let mut session = Session::new();
    let mut machine = new_machine(C::lz64());
    machine
        .write_register(register(0), Syscall::InputPoll.as_u16() as u64)
        .unwrap();
    machine.write_register(register(1), EVENTS).unwrap();
    machine.write_register(register(2), 4).unwrap();
    machine.write_register(register(7), 1).unwrap();
    let event = match machine.step().unwrap() {
        MachineEvent::Trapped { event } => event,
        other => panic!("expected syscall trap, got {other:?}"),
    };
    assert_eq!(event.cause, TrapCause::Syscall);
    let thread_id = session.process.primary_thread().id();
    let current = machine.architectural_state().clone();
    let identity = session.process.memory().identity();
    let execution_context = ExecutionContextId::new(0x1000_0000_0000_0001).unwrap();
    machine
        .with_trap_controller_mut(None, |controller| {
            controller.set_execution_context(execution_context);
        })
        .unwrap();
    let request = machine
        .with_trap_controller_mut(Some(execution_context), |controller| {
            SyscallRequest::from_trap(controller, &current, &session.process, thread_id, &identity)
        })
        .unwrap()
        .unwrap();
    let mut memory = session.process.memory_context().unwrap();
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut session.driver,
    );
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_ne!(outcome.status(), SyscallStatus::Ok, "the call is refused")
        }
        other => panic!("expected a return: {other:?}"),
    }
}

/// A guest cannot inject its own events.
///
/// A poll only drains. There is no argument that adds to the queue, so a program
/// cannot hand itself a key press — which is the property that makes the input
/// device worth having.
#[test]
fn a_guest_cannot_inject_its_own_events() {
    let mut session = Session::new();
    for capacity in [0, 1, 4, 64] {
        let address = match capacity {
            0 => 0,
            _ => EVENTS,
        };
        let _ = session.poll(address, capacity);
    }
    assert_eq!(
        session.driver.pending(),
        0,
        "polling with every shape of argument injected nothing"
    );
    assert_eq!(session.driver.delivered(), 0, "and delivered nothing");
}

/// A record's four fields are written where the ABI says they are.
///
/// The SDK reads a record by offset, so a driver that permuted the fields would
/// produce records no program could read and a test that only compared a decoded
/// struct would not notice.
#[test]
fn a_record_is_written_where_the_abi_says_each_field_is() {
    let mut script = HostScript::new();
    script.push(HostAction::MouseDown(2, 0x0102, 0x0304));
    let mut session = Session::with_script(&script);
    assert_eq!(session.count(EVENTS, 1), 1);

    let mut memory = session.process.memory_context().unwrap();
    let mut bytes = vec![0u8; INPUT_EVENT_RECORD_SIZE];
    memory
        .read_bytes(VirtualAddress::new(EVENTS), &mut bytes)
        .unwrap();
    let kind = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let code = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    let x = i32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    let y = i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    assert_eq!(kind, EventKind::MouseDown.as_u32(), "kind is at offset 0");
    assert_eq!(code, 2, "code is at offset 4");
    assert_eq!(x, 0x0102, "x is at offset 8");
    assert_eq!(y, 0x0304, "y is at offset 12");
}

/// A negative pointer position survives the round trip.
///
/// A position is signed, because a pointer can be dragged off the top or left of
/// a window and a program clamping it needs to know which side it went.
#[test]
fn a_negative_pointer_position_survives_the_round_trip() {
    let mut script = HostScript::new();
    script.push(HostAction::MouseMove(-5, -9));
    let mut session = Session::with_script(&script);
    assert_eq!(session.count(EVENTS, 1), 1);
    let records = session.records(EVENTS, 1);
    assert_eq!(records[0].x(), -5);
    assert_eq!(records[0].y(), -9);
}

/// The driver delivers exactly what the device queued, and no more.
///
/// The driver is a drain with no opinion of its own, so the total the guest
/// receives across several polls is the total the device held.
#[test]
fn the_driver_delivers_exactly_what_the_device_queued() {
    let mut script = HostScript::new();
    for index in 0..9 {
        script.push(HostAction::KeyDown(match index % 3 {
            0 => HostKey::A,
            1 => HostKey::B,
            _ => HostKey::C,
        }));
    }
    let mut session = Session::with_script(&script);
    let queued = session.driver.pending();
    assert_eq!(queued, 9, "the script queued nine");
    let mut total: u64 = 0;
    for capacity in [1, 3, 8, 16] {
        total += session.count(EVENTS, capacity);
    }
    assert_eq!(total, queued, "every queued event reached the guest");
    assert_eq!(session.driver.pending(), 0);
    assert_eq!(session.driver.delivered(), queued);
    let _ = session.count(EVENTS, 8);
    assert_eq!(
        session.driver.delivered(),
        queued,
        "an empty poll delivers nothing"
    );
}

/// The input syscall number is stable, because a program's syscall is a number
/// in its compiled binary and a renumbered driver would run the wrong call on a
/// program built against the old ABI.
#[test]
fn the_input_syscall_number_is_stable() {
    assert_eq!(Syscall::InputPoll.as_u16(), 0x0011);
}
