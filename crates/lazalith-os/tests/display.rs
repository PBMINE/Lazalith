//! Step 70: the LazOS display driver, exercised through the real ABI.
//!
//! These go through the trap and the dispatcher rather than calling the driver
//! directly, because the driver's contract is a *syscall*: the argument order, the
//! word widths, the record sizes and the memory checks all belong to the ABI, and
//! a test that skipped the ABI would pass a driver that a program cannot use.
//!
//! The property each test states is one the design in `docs/lazen-graphics.md`
//! depends on:
//!
//! - a window is over the guest's *own* memory, and the driver copies nothing;
//! - `display_open` reports the geometry and address back through its record;
//! - a present of an address that is not the open window's is refused, so a bug
//!   in a program shows as a wrong number of frames rather than a wrong picture;
//! - the record and the result are written only after validation, so a refusal
//!   leaves the guest's memory alone;
//! - every refusal is one of the ABI's own error codes.

use lazalith_cpu::{ExecutionContextId, Privilege, StatusRegister, TrapCause};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::abi::{
    DISPLAY_RECORD_SIZE, DisplayRecord, IO_RESULT_SIZE, IoResult, Syscall, SyscallStatus,
};
use lazalith_os::display::DisplayService;
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

/// Where the guest's framebuffer and records live, well inside the data segment.
const FRAMEBUFFER: u64 = lazalith_os::USER_DATA_START;
const RECORD: u64 = FRAMEBUFFER + 0x4000;
const RESULT: u64 = FRAMEBUFFER + 0x8000;

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

/// One guest, one driver, and as many calls as a test wants to make.
///
/// The driver is stateful — a window is opened once and presented into — so a
/// helper that made a fresh driver per call could not express "open, then
/// present", which is the only sequence the ABI allows.
struct Session {
    driver: DisplayService,
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
            driver: DisplayService::new(C::lz64()),
            process,
        }
    }

    /// Makes one call and returns what the ABI returned.
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

    fn open(&mut self, width: u32, height: u32, framebuffer: u64, record: u64) -> DispatchOutcome {
        self.call(
            Syscall::DisplayOpen,
            open_arguments(width, height, framebuffer, record),
        )
    }

    fn present(&mut self, framebuffer: u64) -> DispatchOutcome {
        self.call(Syscall::DisplayPresent, [framebuffer, RESULT, 0, 0, 0, 0])
    }
}

fn assert_return(outcome: &DispatchOutcome, status: SyscallStatus, payload: u32) {
    assert_return_because(
        outcome,
        status,
        payload,
        "the call returned what the ABI promised",
    )
}

/// Asserts the status and payload, saying `because` when it does not hold.
fn assert_return_because(
    outcome: &DispatchOutcome,
    status: SyscallStatus,
    payload: u32,
    because: &str,
) {
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_eq!(outcome.status(), status, "{because}");
            assert_eq!(outcome.payload(), payload, "{because}");
        }
        other => panic!("{because}, but the call did not return: {other:?}"),
    }
}

fn read_record(process: &mut Process, address: u64) -> DisplayRecord {
    let mut memory = process.memory_context().unwrap();
    let mut bytes = vec![0u8; DISPLAY_RECORD_SIZE];
    memory
        .read_bytes(VirtualAddress::new(address), &mut bytes)
        .unwrap();
    DisplayRecord::decode(&bytes, C::lz64()).expect("the record reads back")
}

fn read_result(process: &mut Process, address: u64) -> IoResult {
    let mut memory = process.memory_context().unwrap();
    let mut bytes = vec![0u8; IO_RESULT_SIZE];
    memory
        .read_bytes(VirtualAddress::new(address), &mut bytes)
        .unwrap();
    IoResult::decode(&bytes, C::lz64()).expect("the result reads back")
}

fn open_arguments(width: u32, height: u32, framebuffer: u64, record: u64) -> [u64; 6] {
    [
        u64::from(width),
        u64::from(height),
        framebuffer,
        record,
        0,
        0,
    ]
}

/// A window opens over memory the guest already owns, and the driver records the
/// address rather than copying a pixel.
#[test]
fn opening_a_window_records_the_guests_own_address() {
    let mut session = Session::new();
    let outcome = session.open(64, 32, FRAMEBUFFER, RECORD);
    assert_return(&outcome, SyscallStatus::Ok, 0);
    let record = read_record(&mut session.process, RECORD);
    assert_eq!(record.width(), 64, "the geometry comes back as asked");
    assert_eq!(record.height(), 32);
    assert_eq!(
        record.framebuffer(),
        FRAMEBUFFER,
        "and the address is the guest's own, not a copy"
    );
    assert_eq!(
        record.byte_length(),
        Some(64 * 32 * 4),
        "the record knows how big the window's memory is"
    );
    let device = session.driver.device();
    assert!(device.is_open(), "the window is open");
    assert_eq!(device.width(), 64);
    assert_eq!(device.height(), 32);
    assert_eq!(device.framebuffer(), FRAMEBUFFER);
}

/// A framebuffer too small for the window is refused before the device is
/// touched, so a program that got its arithmetic wrong is told rather than
/// allowed to scribble past its own buffer.
#[test]
fn a_framebuffer_too_small_for_the_window_is_refused() {
    let mut session = Session::new();
    // 4096 by 4096 is 64 MiB of pixels, far more than the data segment holds, so
    // the address is a real address and the *length* is what fails. The status is
    // the ABI's own, the same one every other syscall reports for a range
    // outside the caller's memory, so a program can test it without knowing it
    // was a display call.
    let outcome = session.open(4096, 4096, FRAMEBUFFER, RECORD);
    assert_return_because(
        &outcome,
        SyscallStatus::InvalidPointer,
        2,
        "the framebuffer is argument 2, so the detail names it",
    );
    assert!(
        !session.driver.device().is_open(),
        "a refused open leaves no window behind"
    );
}

/// A geometry whose pixel count would overflow is refused rather than wrapping.
///
/// `width * height * 4` is the framebuffer's length, and a wrapped one is a
/// number small enough to pass a bounds check that should have failed.
#[test]
fn a_geometry_whose_pixel_count_overflows_is_refused() {
    let mut session = Session::new();
    let outcome = session.open(u32::MAX, u32::MAX, FRAMEBUFFER, RECORD);
    assert_return_because(
        &outcome,
        SyscallStatus::ResourceExhausted,
        0,
        "a geometry whose byte count does not fit a word is exhausted, not answered",
    );
    assert!(!session.driver.device().is_open(), "and no window is open");
}

/// A present of the open window's address reports the frame count, and the
/// device sees the frame.
#[test]
fn presenting_the_open_window_counts_frames() {
    let mut session = Session::new();
    assert_return(
        &session.open(64, 32, FRAMEBUFFER, RECORD),
        SyscallStatus::Ok,
        0,
    );
    for expected in 1..=3u64 {
        let outcome = session.present(FRAMEBUFFER);
        assert_return(&outcome, SyscallStatus::Ok, 0);
        let result = read_result(&mut session.process, RESULT);
        assert_eq!(
            result.transferred(),
            expected,
            "the frame count is reported, so a program knows a frame landed"
        );
        assert_eq!(result.status(), SyscallStatus::Ok);
        let frame = session.driver.last_frame().expect("a frame was presented");
        assert_eq!(frame.address, FRAMEBUFFER);
        assert_eq!(frame.width, 64);
        assert_eq!(frame.present_count, expected);
    }
}

/// A present of some *other* address is refused rather than presented anyway.
///
/// Showing something would hide the bug until the picture was wrong. The record
/// is still written, so the program can read how far it got.
#[test]
fn presenting_an_address_that_is_not_the_window_is_refused() {
    let mut session = Session::new();
    assert_return(
        &session.open(64, 32, FRAMEBUFFER, RECORD),
        SyscallStatus::Ok,
        0,
    );
    let outcome = session.present(FRAMEBUFFER + 0x2000);
    assert_return(&outcome, SyscallStatus::Ok, 0);
    let result = read_result(&mut session.process, RESULT);
    assert_eq!(
        result.status(),
        SyscallStatus::InvalidArgument,
        "the record says the present was refused"
    );
    assert_eq!(result.transferred(), 0, "and no frame reached the screen");
    assert!(
        session.driver.last_frame().is_none(),
        "so the device still has nothing to show"
    );
}

/// A present before any window is open is refused, because there is no frame to
/// show.
#[test]
fn presenting_before_a_window_is_open_is_refused() {
    let mut session = Session::new();
    assert_return(&session.present(FRAMEBUFFER), SyscallStatus::Ok, 0);
    assert_eq!(
        read_result(&mut session.process, RESULT).status(),
        SyscallStatus::InvalidHandle
    );
}

/// A record the guest cannot have written to is refused, and no window is left
/// open.
///
/// The window comes back through that record, so a window opened over a record
/// the guest cannot receive is a window the guest cannot know about.
#[test]
fn an_unwritable_record_is_refused_and_leaves_no_window() {
    let mut session = Session::new();
    // The code segment is readable and executable but not writable, so the
    // record lands somewhere the guest cannot receive it.
    let outcome = session.open(64, 32, FRAMEBUFFER, lazalith_os::USER_CODE_START);
    assert_return_because(
        &outcome,
        SyscallStatus::InvalidPointer,
        3,
        "the record is argument 3, so the detail names it",
    );
    assert!(
        !session.driver.device().is_open(),
        "a window whose record was refused is not left open"
    );
}

/// A null framebuffer is refused: a window over address zero is not a window.
#[test]
fn a_null_framebuffer_is_refused() {
    let mut session = Session::new();
    assert_return_because(
        &session.open(64, 32, 0, RECORD),
        SyscallStatus::InvalidPointer,
        2,
        "address zero is caught as the framebuffer argument",
    );
    assert!(!session.driver.device().is_open());
}

/// A zero-sized window is refused rather than opened over nothing.
///
/// The device is what decides this one, and the driver passes its refusal on as
/// an ABI error rather than a display-specific code.
#[test]
fn a_zero_sized_window_is_refused() {
    let mut session = Session::new();
    assert_return_because(
        &session.open(0, 0, FRAMEBUFFER, RECORD),
        SyscallStatus::InvalidArgument,
        0,
        "a window with no pixels is not a window",
    );
    assert!(!session.driver.device().is_open());
}

/// A result record the guest cannot receive is refused too.
///
/// The refusal is reported through the return value here, not through a record
/// the guest cannot read — which is the only way a program could learn of it.
#[test]
fn an_unwritable_result_is_refused() {
    let mut session = Session::new();
    assert_return(
        &session.open(64, 32, FRAMEBUFFER, RECORD),
        SyscallStatus::Ok,
        0,
    );
    let outcome = session.call(
        Syscall::DisplayPresent,
        [FRAMEBUFFER, lazalith_os::USER_CODE_START, 0, 0, 0, 0],
    );
    assert_return_because(
        &outcome,
        SyscallStatus::InvalidPointer,
        1,
        "the result is argument 1, so the detail names it",
    );
}

/// The driver never writes a pixel, so a frame the guest drew survives an open
/// and a present untouched.
#[test]
fn the_driver_never_touches_a_pixel() {
    let mut session = Session::new();
    let pixel = [0xffu8, 0x11, 0x22, 0xff];
    {
        let mut memory = session.process.memory_context().unwrap();
        memory
            .write_bytes(VirtualAddress::new(FRAMEBUFFER), &pixel)
            .unwrap();
    }
    assert_return(
        &session.open(16, 16, FRAMEBUFFER, RECORD),
        SyscallStatus::Ok,
        0,
    );
    assert_return(&session.present(FRAMEBUFFER), SyscallStatus::Ok, 0);
    let mut read_back = [0u8; 4];
    {
        let mut memory = session.process.memory_context().unwrap();
        memory
            .read_bytes(VirtualAddress::new(FRAMEBUFFER), &mut read_back)
            .unwrap();
    }
    assert_eq!(
        read_back, pixel,
        "the driver's window is over the guest's own memory, byte for byte"
    );
}

/// The driver reports what it presented, and never a bitmap: a frontend resolves
/// the address against guest memory itself.
#[test]
fn the_driver_reports_an_address_and_not_pixels() {
    let mut session = Session::new();
    assert_return(
        &session.open(8, 8, FRAMEBUFFER, RECORD),
        SyscallStatus::Ok,
        0,
    );
    assert_return(&session.present(FRAMEBUFFER), SyscallStatus::Ok, 0);
    let frame = session.driver.last_frame().expect("a frame");
    assert_eq!(frame.present_count, 1);
    // The reported frame is Copy and holds four scalars, so there is nowhere for
    // a copy of the framebuffer to be hiding.
    assert_eq!(std::mem::size_of_val(&frame), 4 * 8);
}

/// The display syscall numbers are stable, because a program's syscall is a
/// number in its compiled binary and a renumbered driver would run the wrong
/// call on a program built against the old ABI.
#[test]
fn the_display_syscall_numbers_are_stable() {
    assert_eq!(Syscall::DisplayOpen.as_u16(), 0x000f);
    assert_eq!(Syscall::DisplayPresent.as_u16(), 0x0010);
}
