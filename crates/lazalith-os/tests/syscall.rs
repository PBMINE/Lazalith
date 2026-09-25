use lazalith_cpu::{ExecutionContextId, Privilege, StatusRegister, TrapCause};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::abi::{
    DirectoryRecord, FileHandle, FileStat, IoResult, MAX_ARGUMENT_BYTES, MAX_ARGUMENT_TOTAL_BYTES,
    MAX_PATH_BYTES, OPEN_CREATE, OPEN_READ, OPEN_WRITE, OpenFlags, ProcessHandle, SeekOrigin,
    Syscall, SyscallError, SyscallStatus, TaggedOutcome,
};
use lazalith_os::{
    DispatchOutcome, FileAccess, FileSystemService, IoHandle, KernelService, Process, ProcessId,
    ProgramImage, ServiceOutcome, SyscallDispatcher, SyscallRequest, SyscallRequestError,
    TerminalService, ThreadId, UserMemoryContext, ValidatedSyscall, ValidatedSyscallKind,
    VirtualFileSystem, VirtualTerminal,
};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

const KERNEL_CODE: u64 = 0x1000;
const USER_CODE: u64 = 0x2000;
const USER_STACK: u64 = 0x3000;

struct TrapHarness {
    machine: LazalithMachine<NoDevice>,
    request: SyscallRequest,
    process: Process,
}

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

fn test_process(config: C) -> Process {
    let image = ProgramImage::new(config, 0, &[0; 8]).unwrap();
    Process::new(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap(), image).unwrap()
}

fn trapped(config: C, number: u64, arguments: [u64; 6], reserved: u64) -> TrapHarness {
    let mut machine = new_machine(config);
    machine.write_register(register(0), number).unwrap();
    for (index, value) in arguments.iter().enumerate() {
        machine
            .write_register(register(u8::try_from(index + 1).unwrap()), *value)
            .unwrap();
    }
    machine.write_register(register(7), reserved).unwrap();
    let event = match machine.step().unwrap() {
        MachineEvent::Trapped { event } => event,
        other => panic!("expected syscall trap, got {other:?}"),
    };
    assert_eq!(event.cause, TrapCause::Syscall);
    assert_eq!(event.resume_pc, InstructionAddress::new(USER_CODE + 8));
    let mut process = test_process(config);
    process.mark_ready().unwrap();
    process.mark_running().unwrap();
    let execution_context = ExecutionContextId::new(0x1000_0000_0000_0001).unwrap();
    process.activate(execution_context).unwrap();
    machine
        .with_trap_controller_mut(None, |controller| {
            controller.set_execution_context(execution_context);
        })
        .unwrap();
    let thread_id = process.primary_thread().id();
    let current = machine.architectural_state().clone();
    let active_identity = process.memory().identity();
    let request = machine
        .with_trap_controller_mut(Some(execution_context), |controller| {
            SyscallRequest::from_trap(controller, &current, &process, thread_id, &active_identity)
        })
        .unwrap()
        .unwrap();
    assert_eq!(request.resume_pc(), event.resume_pc);
    TrapHarness {
        machine,
        request,
        process,
    }
}

#[derive(Default)]
struct RecordingService {
    calls: Vec<ValidatedSyscallKind>,
}

impl KernelService for RecordingService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        _memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        let kind = syscall.kind();
        self.calls.push(kind);
        match kind {
            ValidatedSyscallKind::Exit { .. } => ServiceOutcome::Exit,
            ValidatedSyscallKind::Time { .. } => ServiceOutcome::Return(TaggedOutcome::success(99)),
            _ => ServiceOutcome::Return(TaggedOutcome::success(0)),
        }
    }
}

fn dispatch_request(
    config: C,
    number: u64,
    arguments: [u64; 6],
    reserved: u64,
    setup: impl FnOnce(&mut UserMemoryContext<'_>),
) -> (DispatchOutcome, RecordingService) {
    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(config, number, arguments, reserved);
    let mut memory = process.memory_context().unwrap();
    setup(&mut memory);
    let mut service = RecordingService::default();
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut service,
    );
    (outcome, service)
}

fn dispatch_filesystem_call(
    config: C,
    number: Syscall,
    arguments: [u64; 6],
    filesystem: VirtualFileSystem,
    setup: impl FnOnce(&mut UserMemoryContext<'_>),
) -> (DispatchOutcome, Process, FileSystemService) {
    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(config, number.as_u16() as u64, arguments, 0);
    let mut memory = process.memory_context().unwrap();
    setup(&mut memory);
    let mut service = FileSystemService::new(filesystem);
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut service,
    );
    (outcome, process, service)
}

fn dispatch_terminal_call(
    config: C,
    number: Syscall,
    arguments: [u64; 6],
    terminal: VirtualTerminal,
    setup: impl FnOnce(&mut UserMemoryContext<'_>),
) -> (DispatchOutcome, Process, TerminalService) {
    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(config, number.as_u16() as u64, arguments, 0);
    let mut memory = process.memory_context().unwrap();
    setup(&mut memory);
    let mut service = TerminalService::new(
        terminal,
        FileSystemService::new(VirtualFileSystem::with_defaults().unwrap()),
    );
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut service,
    );
    (outcome, process, service)
}

fn assert_return(outcome: &DispatchOutcome, status: SyscallStatus, payload: u32) -> TaggedOutcome {
    match outcome {
        DispatchOutcome::Return { outcome, .. } => {
            assert_eq!(outcome.status(), status);
            assert_eq!(outcome.payload(), payload);
            *outcome
        }
        other => panic!("expected returning outcome, got {other:?}"),
    }
}

#[test]
fn trap_snapshot_dispatch_and_rfe_complete_the_syscall_path_in_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let TrapHarness {
            mut machine,
            request,
            mut process,
        } = trapped(
            config,
            Syscall::Time.as_u16() as u64,
            [
                lazalith_os::USER_DATA_START,
                0x1111,
                0x2222,
                0x3333,
                0x4444,
                0x5555,
            ],
            0,
        );
        let mut memory = process.memory_context().unwrap();
        let mut service = RecordingService::default();
        let outcome = SyscallDispatcher::new().dispatch(
            request,
            machine.trap_controller(),
            machine.architectural_state(),
            &mut memory,
            &mut service,
        );
        assert_return(&outcome, SyscallStatus::Ok, 99);
        assert_eq!(
            service.calls,
            [ValidatedSyscallKind::Time {
                result: VirtualAddress::new(lazalith_os::USER_DATA_START)
            }]
        );
        let completion = outcome.into_completion().unwrap();
        let saved_sp = machine.architectural_state().sp();
        let saved_status = machine.trap_controller().frame().unwrap().resume_status();
        assert!(matches!(
            machine.return_from_syscall(completion).unwrap(),
            MachineEvent::Stepped { .. }
        ));
        assert_eq!(
            machine.architectural_state().pc(),
            InstructionAddress::new(USER_CODE + 8)
        );
        assert_eq!(machine.architectural_state().privilege(), Privilege::User);
        assert_eq!(
            machine.architectural_state().registers().read(register(2)),
            0x1111
        );
        assert_eq!(
            machine.architectural_state().registers().read(register(1)),
            99
        );
        assert_eq!(machine.architectural_state().sp(), saved_sp);
        assert_eq!(machine.architectural_state().status().bits(), saved_status);
        let preserved = [0x2222, 0x3333, 0x4444, 0x5555, 0];
        for (offset, expected) in preserved.iter().enumerate() {
            let index = u8::try_from(offset + 3).unwrap();
            assert_eq!(
                machine
                    .architectural_state()
                    .registers()
                    .read(register(index)),
                *expected
            );
        }
        assert!(!machine.trap_controller().has_active_frame());
    }
}

#[test]
fn every_frozen_call_reaches_the_service_with_typed_arguments() {
    for config in [C::lz32(), C::lz64()] {
        let data = VirtualAddress::new(lazalith_os::USER_DATA_START);
        let io_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 16);
        let seek_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 32);
        let stat_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 48);
        let records = VirtualAddress::new(lazalith_os::USER_DATA_START + 512);
        let list_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 1024);
        let buffer = VirtualAddress::new(lazalith_os::USER_DATA_START + 2048);
        let handle_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 2304);
        let allocation_result = VirtualAddress::new(lazalith_os::USER_DATA_START + 2368);
        let path = data;
        let argv = VirtualAddress::new(lazalith_os::USER_DATA_START + 64);
        let argument = VirtualAddress::new(lazalith_os::USER_DATA_START + 128);
        let file = FileHandle::new(2).unwrap();
        let process = ProcessHandle::new(2).unwrap();
        let flags = OpenFlags::new(OPEN_READ).unwrap();
        let cases = vec![
            (
                Syscall::Exit,
                [7, 9, 9, 9, 9, 9],
                ValidatedSyscallKind::Exit { exit_code: 7 },
            ),
            (
                Syscall::Write,
                [2, buffer.as_u64(), 0, io_result.as_u64(), 0, 9],
                ValidatedSyscallKind::Write {
                    handle: IoHandle::File(file),
                    buffer,
                    length: 0,
                    result: io_result,
                },
            ),
            (
                Syscall::Read,
                [2, buffer.as_u64(), 0, io_result.as_u64(), 0, 9],
                ValidatedSyscallKind::Read {
                    handle: IoHandle::File(file),
                    buffer,
                    length: 0,
                    result: io_result,
                },
            ),
            (
                Syscall::Open,
                [path.as_u64(), 1, u64::from(OPEN_READ), 0, 9, 9],
                ValidatedSyscallKind::Open {
                    path,
                    path_length: 1,
                    flags,
                },
            ),
            (
                Syscall::Close,
                [2, 9, 9, 9, 9, 9],
                ValidatedSyscallKind::Close { handle: file },
            ),
            (
                Syscall::Seek,
                [2, (-5_i64) as u64, 1, seek_result.as_u64(), 9, 9],
                ValidatedSyscallKind::Seek {
                    handle: file,
                    offset: -5,
                    origin: SeekOrigin::Current,
                    result: seek_result,
                },
            ),
            (
                Syscall::Stat,
                [path.as_u64(), 1, stat_result.as_u64(), 0, 9, 9],
                ValidatedSyscallKind::Stat {
                    path,
                    path_length: 1,
                    result: stat_result,
                },
            ),
            (
                Syscall::ListDirectory,
                [
                    path.as_u64(),
                    1,
                    records.as_u64(),
                    256,
                    list_result.as_u64(),
                    9,
                ],
                ValidatedSyscallKind::ListDirectory {
                    path,
                    path_length: 1,
                    records,
                    capacity: 256,
                    result: list_result,
                },
            ),
            (
                Syscall::Time,
                [io_result.as_u64(), 9, 9, 9, 9, 9],
                ValidatedSyscallKind::Time { result: io_result },
            ),
            (
                Syscall::Sleep,
                [123, 9, 9, 9, 9, 9],
                ValidatedSyscallKind::Sleep { cycles: 123 },
            ),
            (
                Syscall::AllocateMemory,
                [64, 16, allocation_result.as_u64(), 0, 9, 9],
                ValidatedSyscallKind::AllocateMemory {
                    length: 64,
                    alignment: 16,
                    result: allocation_result,
                },
            ),
            (
                Syscall::SpawnProcess,
                [
                    path.as_u64(),
                    1,
                    argv.as_u64(),
                    1,
                    handle_result.as_u64(),
                    9,
                ],
                ValidatedSyscallKind::SpawnProcess {
                    path,
                    path_length: 1,
                    argv,
                    argc: 1,
                    result: handle_result,
                },
            ),
            (
                Syscall::WaitProcess,
                [2, allocation_result.as_u64(), 0, 9, 9, 9],
                ValidatedSyscallKind::WaitProcess {
                    handle: process,
                    result: allocation_result,
                },
            ),
            (
                Syscall::ClearScreen,
                [0, 0, 0, 0, 0, 0],
                ValidatedSyscallKind::ClearScreen,
            ),
        ];

        for (number, arguments, expected) in cases {
            let (outcome, service) =
                dispatch_request(config, number.as_u16() as u64, arguments, 0, |memory| {
                    memory.write_bytes(path, b"x").unwrap();
                    memory.write_bytes(argument, b"arg\0").unwrap();
                    let mut table = [0u8; 8];
                    let raw = argument.as_u64().to_le_bytes();
                    table[..usize::from(config.word_bytes())]
                        .copy_from_slice(&raw[..usize::from(config.word_bytes())]);
                    memory
                        .write_bytes(argv, &table[..usize::from(config.word_bytes())])
                        .unwrap();
                });
            if matches!(expected, ValidatedSyscallKind::Exit { .. }) {
                assert_eq!(outcome, DispatchOutcome::Exit { exit_code: 7 });
            } else if matches!(expected, ValidatedSyscallKind::Time { .. }) {
                assert_return(&outcome, SyscallStatus::Ok, 99);
            } else {
                assert_return(&outcome, SyscallStatus::Ok, 0);
            }
            assert_eq!(service.calls, [expected]);
        }
    }
}

#[test]
fn identity_reserved_and_required_zero_checks_precede_dispatch() {
    let data = lazalith_os::USER_DATA_START;
    let (outcome, service) = dispatch_request(C::lz64(), 0x0100, [0; 6], 0, |_| {});
    assert_return(&outcome, SyscallStatus::UnknownSyscall, 0x0100);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [data, 0, 0, 0, 0, 0],
        1,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 7);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Write.as_u16() as u64,
        [1, data, 0, data + 16, 1, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 5);
    assert!(service.calls.is_empty());

    for (call, arguments, index) in [
        (Syscall::Open, [data, 1, u64::from(OPEN_READ), 1, 0, 0], 4),
        (Syscall::Stat, [data, 1, data + 16, 1, 0, 0], 4),
        (Syscall::AllocateMemory, [64, 8, data + 16, 1, 0, 0], 4),
        (Syscall::WaitProcess, [2, data + 16, 1, 0, 0, 0], 3),
        (Syscall::ClearScreen, [1, 0, 0, 0, 0, 0], 1),
    ] {
        let (outcome, service) =
            dispatch_request(C::lz64(), call.as_u16() as u64, arguments, 0, |_| {});
        assert_return(&outcome, SyscallStatus::InvalidArgument, index);
        assert!(service.calls.is_empty());
    }

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [data + 8, 99, 99, 99, 99, 99],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::Ok, 99);
    assert_eq!(service.calls.len(), 1);
}

#[test]
fn pointer_path_capacity_and_string_checks_reject_before_service_mutation() {
    let data = lazalith_os::USER_DATA_START;
    let crossing_result = lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH - 8;
    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Write.as_u16() as u64,
        [1, data, 0, crossing_result, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidPointer, 3);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Write.as_u16() as u64,
        [1, data, 0, lazalith_os::USER_CODE_START, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidPointer, 3);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Write.as_u16() as u64,
        [1, 0x4000_0000, 0, data + 16, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::Ok, 0);
    assert_eq!(service.calls.len(), 1);

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Open.as_u16() as u64,
        [data, 0, u64::from(OPEN_READ), 0, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 1);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Open.as_u16() as u64,
        [data, 2, u64::from(OPEN_READ), 0, 0, 0],
        0,
        |memory| {
            memory
                .write_bytes(VirtualAddress::new(data), b"a\0")
                .unwrap()
        },
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 1);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::Open.as_u16() as u64,
        [data, MAX_PATH_BYTES + 1, u64::from(OPEN_READ), 0, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 1);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::ListDirectory.as_u16() as u64,
        [data, 1, data + 512, 255, data + 1024, 0],
        0,
        |memory| memory.write_bytes(VirtualAddress::new(data), b"x").unwrap(),
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 3);
    assert!(service.calls.is_empty());

    let records = lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH - 256;
    let result = lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH - 16;
    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::ListDirectory.as_u16() as u64,
        [data, 1, records, 256, result, 0],
        0,
        |memory| memory.write_bytes(VirtualAddress::new(data), b"x").unwrap(),
    );
    assert_return(&outcome, SyscallStatus::Ok, 0);
    assert_eq!(service.calls.len(), 1);

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::SpawnProcess.as_u16() as u64,
        [data, 1, data + 64, 1, data + 70_000, 0],
        0,
        |memory| {
            let data = VirtualAddress::new(data);
            memory.write_bytes(data, b"x").unwrap();
            let argument = VirtualAddress::new(lazalith_os::USER_DATA_START + 128);
            let table = argument.as_u64().to_le_bytes();
            memory
                .write_bytes(
                    VirtualAddress::new(lazalith_os::USER_DATA_START + 64),
                    &table,
                )
                .unwrap();
            let missing_nul = vec![b'x'; usize::try_from(MAX_ARGUMENT_BYTES).unwrap() + 1];
            memory.write_bytes(argument, &missing_nul).unwrap();
        },
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 2);
    assert!(service.calls.is_empty());

    const { assert!(MAX_ARGUMENT_TOTAL_BYTES < 17 * MAX_ARGUMENT_BYTES) };
    let table = VirtualAddress::new(lazalith_os::USER_DATA_START + 64);
    let argument = VirtualAddress::new(lazalith_os::USER_DATA_START + 512);
    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::SpawnProcess.as_u16() as u64,
        [data, 1, table.as_u64(), 17, data + 70_000, 0],
        0,
        |memory| {
            memory.write_bytes(VirtualAddress::new(data), b"x").unwrap();
            let mut entries = [0u8; 17 * 8];
            for index in 0..17 {
                let start = index * 8;
                entries[start..start + 8].copy_from_slice(&argument.as_u64().to_le_bytes());
            }
            memory.write_bytes(table, &entries).unwrap();
            let mut argument_bytes = vec![b'x'; usize::try_from(MAX_ARGUMENT_BYTES).unwrap()];
            let last = argument_bytes.last_mut().unwrap();
            *last = 0;
            memory.write_bytes(argument, &argument_bytes).unwrap();
        },
    );
    assert_return(&outcome, SyscallStatus::ResourceExhausted, 2);
    assert!(service.calls.is_empty());

    let boundary = lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH - 1;
    let stack = lazalith_os::USER_STACK_START;
    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::SpawnProcess.as_u16() as u64,
        [data, 1, data + 64, 1, data + 70_000, 0],
        0,
        |memory| {
            memory.write_bytes(VirtualAddress::new(data), b"x").unwrap();
            let argument = VirtualAddress::new(boundary);
            let table = argument.as_u64().to_le_bytes();
            memory
                .write_bytes(
                    VirtualAddress::new(lazalith_os::USER_DATA_START + 64),
                    &table,
                )
                .unwrap();
            memory.write_bytes(argument, b"x").unwrap();
            memory
                .write_bytes(VirtualAddress::new(stack), &[0])
                .unwrap();
        },
    );
    assert_return(&outcome, SyscallStatus::InvalidPointer, 2);
    assert!(service.calls.is_empty());

    let (outcome, service) = dispatch_request(
        C::lz64(),
        Syscall::AllocateMemory.as_u16() as u64,
        [0, 8, data + 16, 0, 0, 0],
        0,
        |_| {},
    );
    assert_return(&outcome, SyscallStatus::InvalidArgument, 0);
    assert!(service.calls.is_empty());
}

#[test]
fn user_memory_context_checks_full_ranges_and_permissions_before_copying() {
    let config = C::lz64();
    let mut process = test_process(config);
    let mut memory = process.memory_context().unwrap();
    let data = VirtualAddress::new(lazalith_os::USER_DATA_START);
    let code = VirtualAddress::new(lazalith_os::USER_CODE_START);
    memory.write_bytes(data, b"lazalith").unwrap();
    let mut output = [0u8; 8];
    memory.read_bytes(data, &mut output).unwrap();
    assert_eq!(&output, b"lazalith");
    assert!(memory.read_bytes(code, &mut output).is_ok());
    assert!(memory.write_bytes(code, b"x").is_err());
    let mut empty = [];
    assert!(
        memory
            .read_bytes(VirtualAddress::new(0x4000), &mut empty)
            .is_ok()
    );
    assert!(
        memory
            .validate_range(
                VirtualAddress::new(
                    lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH - 1
                ),
                2,
                1,
                lazalith_os::UserMemoryAccess::Read,
            )
            .is_err()
    );
    assert_eq!(memory.process_id(), ProcessId::new(1).unwrap());
}

struct ReturningService;

impl KernelService for ReturningService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        _memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }
}

struct ExitingService;

impl KernelService for ExitingService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        _memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        ServiceOutcome::Exit
    }
}

#[test]
fn service_return_kind_must_match_the_validated_nonreturning_call() {
    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(C::lz64(), Syscall::Exit.as_u16() as u64, [7; 6], 0);
    let mut memory = process.memory_context().unwrap();
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut ReturningService,
    );
    assert_eq!(outcome, DispatchOutcome::Fault(SyscallError::Internal));
    drop(memory);
    assert_eq!(process.state(), lazalith_os::ProcessState::Running);
    assert_eq!(process.exit_code(), None);

    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(C::lz64(), Syscall::Exit.as_u16() as u64, [7; 6], 0);
    let mut memory = process.memory_context().unwrap();
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut ExitingService,
    );
    assert_eq!(outcome, DispatchOutcome::Exit { exit_code: 7 });
    drop(memory);
    assert_eq!(process.state(), lazalith_os::ProcessState::Exited);
    assert_eq!(process.exit_code(), Some(7));

    let TrapHarness {
        machine,
        request,
        mut process,
    } = trapped(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [lazalith_os::USER_DATA_START + 8; 6],
        0,
    );
    let mut memory = process.memory_context().unwrap();
    assert_eq!(
        SyscallDispatcher::new().dispatch(
            request,
            machine.trap_controller(),
            machine.architectural_state(),
            &mut memory,
            &mut ExitingService,
        ),
        DispatchOutcome::Fault(SyscallError::Internal)
    );
}

#[test]
fn request_memory_binding_rejects_a_different_address_space() {
    let first = trapped(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [lazalith_os::USER_DATA_START + 8; 6],
        0,
    );
    let mut second_process = test_process(C::lz64());
    let mut second_memory = second_process.memory_context().unwrap();
    let mut service = RecordingService::default();
    assert_eq!(
        SyscallDispatcher::new().dispatch(
            first.request,
            first.machine.trap_controller(),
            first.machine.architectural_state(),
            &mut second_memory,
            &mut service,
        ),
        DispatchOutcome::Fault(SyscallError::InvalidState)
    );
    assert!(service.calls.is_empty());

    let first = trapped(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [lazalith_os::USER_DATA_START + 8; 6],
        0,
    );
    let second = trapped(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [lazalith_os::USER_DATA_START + 8; 6],
        0,
    );
    let mut second_process = second.process;
    let mut second_memory = second_process.memory_context().unwrap();
    let mut service = RecordingService::default();
    assert_eq!(
        SyscallDispatcher::new().dispatch(
            first.request,
            second.machine.trap_controller(),
            second.machine.architectural_state(),
            &mut second_memory,
            &mut service,
        ),
        DispatchOutcome::Fault(SyscallError::InvalidState)
    );
    assert!(service.calls.is_empty());
}

#[test]
fn request_rejects_nonrunning_or_cross_architecture_processes() {
    let mut machine = new_machine(C::lz64());
    machine
        .write_register(register(0), Syscall::Time.as_u16() as u64)
        .unwrap();
    let event = match machine.step().unwrap() {
        MachineEvent::Trapped { event } => event,
        other => panic!("expected syscall trap, got {other:?}"),
    };
    assert_eq!(event.cause, TrapCause::Syscall);
    let current = machine.architectural_state().clone();
    let active_identity = machine.memory().identity().clone();
    let process = test_process(C::lz64());
    let thread_id = process.primary_thread().id();
    assert_eq!(
        machine
            .with_trap_controller_mut(None, |controller| {
                SyscallRequest::from_trap(
                    controller,
                    &current,
                    &process,
                    thread_id,
                    &active_identity,
                )
            })
            .unwrap(),
        Err(SyscallRequestError::ProcessNotRunning {
            process_id: ProcessId::new(1).unwrap(),
            state: lazalith_os::ProcessState::Created,
        })
    );
    let wrong = test_process(C::lz32());
    let wrong_thread = wrong.primary_thread().id();
    assert_eq!(
        machine
            .with_trap_controller_mut(None, |controller| {
                SyscallRequest::from_trap(
                    controller,
                    &current,
                    &wrong,
                    wrong_thread,
                    &active_identity,
                )
            })
            .unwrap(),
        Err(SyscallRequestError::ProcessConfiguration {
            process: C::lz32(),
            current: C::lz64(),
        })
    );
}

#[test]
fn request_rejects_a_different_active_execution_context() {
    let mut first = trapped(
        C::lz64(),
        Syscall::Time.as_u16() as u64,
        [lazalith_os::USER_DATA_START + 8; 6],
        0,
    );
    let mut wrong = test_process(C::lz64());
    wrong.mark_ready().unwrap();
    wrong.mark_running().unwrap();
    let wrong_context = ExecutionContextId::new(0x1000_0000_0000_0002).unwrap();
    wrong.activate(wrong_context).unwrap();
    let current = first.machine.architectural_state().clone();
    let active_identity = first.machine.memory().identity().clone();
    let thread_id = wrong.primary_thread().id();
    assert_eq!(
        first
            .machine
            .with_trap_controller_mut(
                Some(ExecutionContextId::new(0x1000_0000_0000_0001).unwrap()),
                |controller| {
                    SyscallRequest::from_trap(
                        controller,
                        &current,
                        &wrong,
                        thread_id,
                        &active_identity,
                    )
                },
            )
            .unwrap(),
        Err(SyscallRequestError::ExecutionContextMismatch {
            expected: ExecutionContextId::new(0x1000_0000_0000_0001).unwrap(),
            actual: wrong_context,
        })
    );
}

#[test]
fn request_admission_requires_the_active_syscall_frame() {
    let TrapHarness {
        mut machine,
        request,
        process,
    } = trapped(C::lz64(), Syscall::Time.as_u16() as u64, [0x3000; 6], 0);
    let thread_id = process.primary_thread().id();
    let current = machine.architectural_state().clone();
    let active_identity = process.memory().identity();
    assert_eq!(
        machine
            .with_trap_controller_mut(
                Some(ExecutionContextId::new(0x1000_0000_0000_0001).unwrap()),
                |controller| {
                    SyscallRequest::from_trap(
                        controller,
                        &current,
                        &process,
                        thread_id,
                        &active_identity,
                    )
                },
            )
            .unwrap(),
        Err(SyscallRequestError::AlreadyAdmitted)
    );
    let mut process = process;
    let mut memory = process.memory_context().unwrap();
    let mut service = RecordingService::default();
    let outcome = SyscallDispatcher::new().dispatch(
        request,
        machine.trap_controller(),
        machine.architectural_state(),
        &mut memory,
        &mut service,
    );
    machine
        .return_from_syscall(outcome.into_completion().unwrap())
        .unwrap();
    let current = machine.architectural_state().clone();
    assert_eq!(
        machine
            .with_trap_controller_mut(
                Some(ExecutionContextId::new(0x1000_0000_0000_0001).unwrap()),
                |controller| {
                    SyscallRequest::from_trap(
                        controller,
                        &current,
                        &process,
                        thread_id,
                        &active_identity,
                    )
                },
            )
            .unwrap(),
        Err(SyscallRequestError::MissingFrame)
    );
}

#[test]
fn filesystem_service_open_creates_a_process_owned_handle() {
    for config in [C::lz32(), C::lz64()] {
        let path = lazalith_os::USER_DATA_START;
        let (outcome, process, service) = dispatch_filesystem_call(
            config,
            Syscall::Open,
            [path, 5, (OPEN_CREATE | OPEN_WRITE) as u64, 0, 0, 0],
            VirtualFileSystem::with_defaults().unwrap(),
            |memory| {
                memory
                    .write_bytes(VirtualAddress::new(path), b"/file")
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 2);
        assert!(process.handles().file(FileHandle::new(2).unwrap()).is_ok());
        assert_eq!(service.filesystem().metadata(b"/file").unwrap().size, 0);
    }
}

#[test]
fn filesystem_service_opens_and_reports_metadata() {
    for config in [C::lz32(), C::lz64()] {
        let path = lazalith_os::USER_DATA_START;
        let stat_result = path + 64;
        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        filesystem.insert_file(b"/file", b"abc").unwrap();
        let (outcome, process, service) = dispatch_filesystem_call(
            config,
            Syscall::Stat,
            [path, 5, stat_result, 0, 0, 0],
            filesystem,
            |memory| {
                memory
                    .write_bytes(VirtualAddress::new(path), b"/file")
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(stat_result), &mut encoded)
            .unwrap();
        let stat = FileStat::decode(&encoded, config).unwrap();
        assert_eq!(stat.size(), 3);
        assert_eq!(stat.kind(), lazalith_os::abi::AbiFileKind::File);
        assert_eq!(service.filesystem().metadata(b"/file").unwrap().size, 3);
    }
}

#[test]
fn filesystem_service_reads_writes_seeks_and_closes_handles() {
    for config in [C::lz32(), C::lz64()] {
        let data = lazalith_os::USER_DATA_START;
        let result = data + 256;
        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/file", b"abc").unwrap();
        let (outcome, process, service) = dispatch_filesystem_call(
            config,
            Syscall::Write,
            [2, data + 32, 2, result, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(false, true))
                    .unwrap();
                memory
                    .write_bytes(VirtualAddress::new(data + 32), b"XY")
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        assert_eq!(
            service
                .filesystem()
                .read_at(node, 0, 99, FileAccess::new(true, true))
                .unwrap(),
            b"XYc"
        );
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(result), &mut encoded)
            .unwrap();
        assert_eq!(IoResult::decode(&encoded, config).unwrap().transferred(), 2);

        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/file", b"abcdef").unwrap();
        let buffer = data + 32;
        let (outcome, process, service) = dispatch_filesystem_call(
            config,
            Syscall::Read,
            [2, buffer, 4, result, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(true, false))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        let mut read = [0u8; 4];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(buffer), &mut read)
            .unwrap();
        assert_eq!(&read, b"abcd");
        assert_eq!(
            service
                .filesystem()
                .read_at(node, 0, 99, FileAccess::new(true, true))
                .unwrap(),
            b"abcdef"
        );

        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/file", b"abcdef").unwrap();
        let (outcome, _process, _service) = dispatch_filesystem_call(
            config,
            Syscall::Seek,
            [2, (-2_i64) as u64, 2, result, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(true, false))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);

        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/file", b"abc").unwrap();
        let (outcome, process, _service) = dispatch_filesystem_call(
            config,
            Syscall::Close,
            [2, 0, 0, 0, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(true, false))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        assert!(process.handles().file(FileHandle::new(2).unwrap()).is_err());
    }
}

#[test]
fn filesystem_service_lists_sorted_directory_records() {
    let data = lazalith_os::USER_DATA_START;
    let records = data + 128;
    let result = data + 1024;
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_file(b"/z", b"z").unwrap();
    filesystem.insert_file(b"/a", b"a").unwrap();
    let (outcome, process, _service) = dispatch_filesystem_call(
        C::lz64(),
        Syscall::ListDirectory,
        [data, 1, records, 512, result, 0],
        filesystem,
        |memory| {
            memory.write_bytes(VirtualAddress::new(data), b"/").unwrap();
        },
    );
    assert_return(&outcome, SyscallStatus::Ok, 0);
    let mut first = [0u8; 256];
    let mut second = [0u8; 256];
    process
        .memory()
        .address_space()
        .peek(PhysicalAddress::new(records), &mut first)
        .unwrap();
    process
        .memory()
        .address_space()
        .peek(PhysicalAddress::new(records + 256), &mut second)
        .unwrap();
    assert_eq!(DirectoryRecord::decode(&first).unwrap().name(), b"a");
    assert_eq!(DirectoryRecord::decode(&second).unwrap().name(), b"z");
    let mut io = [0u8; 16];
    process
        .memory()
        .address_space()
        .peek(PhysicalAddress::new(result), &mut io)
        .unwrap();
    assert_eq!(IoResult::decode(&io, C::lz64()).unwrap().transferred(), 2);
}

#[test]
fn terminal_service_handles_input_output_and_clear_descriptors() {
    for config in [C::lz32(), C::lz64()] {
        let data = lazalith_os::USER_DATA_START;
        let buffer = data + 32;
        let result = data + 64;
        let (outcome, process, service) = dispatch_terminal_call(
            config,
            Syscall::Read,
            [0, buffer, 3, result, 0, 0],
            VirtualTerminal::new(b"abc").unwrap(),
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        let mut input = [0u8; 3];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(buffer), &mut input)
            .unwrap();
        assert_eq!(&input, b"abc");
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(result), &mut encoded)
            .unwrap();
        assert_eq!(IoResult::decode(&encoded, config).unwrap().transferred(), 3);
        assert!(service.terminal().remaining_input().is_empty());

        let source = data + 32;
        let (outcome, process, service) = dispatch_terminal_call(
            config,
            Syscall::Write,
            [1, source, 5, result, 0, 0],
            VirtualTerminal::new(b"").unwrap(),
            |memory| {
                memory
                    .write_bytes(VirtualAddress::new(source), b"hello")
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(result), &mut encoded)
            .unwrap();
        assert_eq!(IoResult::decode(&encoded, config).unwrap().transferred(), 5);
        assert_eq!(service.terminal().output(), b"hello");

        let (outcome, _process, service) = dispatch_terminal_call(
            config,
            Syscall::ClearScreen,
            [0; 6],
            VirtualTerminal::new(b"ignored").unwrap(),
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        assert_eq!(service.terminal().screen_generation(), 1);
        assert!(service.terminal().output().is_empty());
    }
}

#[test]
fn every_syscall_error_status_fits_the_one_shot_completion_range() {
    for error in [
        SyscallError::UnknownSyscall,
        SyscallError::InvalidArgument,
        SyscallError::InvalidPointer,
        SyscallError::RangeOverflow,
        SyscallError::Misaligned,
        SyscallError::NotFound,
        SyscallError::AlreadyExists,
        SyscallError::PermissionDenied,
        SyscallError::InvalidHandle,
        SyscallError::NotDirectory,
        SyscallError::IsDirectory,
        SyscallError::NotSupported,
        SyscallError::ResourceExhausted,
        SyscallError::InvalidState,
        SyscallError::IoFailure,
        SyscallError::DeviceFailure,
        SyscallError::ProcessFailure,
        SyscallError::Faulted,
        SyscallError::Internal,
    ] {
        let status = SyscallStatus::from(error).as_u32();
        assert!(
            status <= 19,
            "status {status} for {error:?} cannot be completed in one shot"
        );
    }
    assert_eq!(SyscallStatus::from(SyscallError::Internal).as_u32(), 19);
    assert_eq!(SyscallStatus::Ok.as_u32(), 0);
}

#[test]
fn stale_foreign_and_closed_handles_are_rejected_end_to_end() {
    for config in [C::lz32(), C::lz64()] {
        let data = lazalith_os::USER_DATA_START;
        let result = data + 256;
        let buffer = data + 32;
        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/file", b"abc").unwrap();
        let (outcome, _process, _service) = dispatch_filesystem_call(
            config,
            Syscall::Close,
            [2, 0, 0, 0, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(true, true))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);

        let (outcome, _process, _service) = dispatch_filesystem_call(
            config,
            Syscall::Close,
            [2, 0, 0, 0, 0, 0],
            VirtualFileSystem::with_defaults().unwrap(),
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::InvalidHandle, 0);

        let (outcome, process, _service) = dispatch_filesystem_call(
            config,
            Syscall::Read,
            [7, buffer, 3, result, 0, 0],
            VirtualFileSystem::with_defaults().unwrap(),
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(true, true))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::InvalidHandle, 0);
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(result), &mut encoded)
            .unwrap();
        assert_eq!(IoResult::decode(&encoded, config).unwrap().transferred(), 0);
    }
}

#[test]
fn word_width_bounds_the_records_and_arguments_in_both_modes() {
    let data = lazalith_os::USER_DATA_START;
    for (config, word_bytes) in [(C::lz32(), 4u64), (C::lz64(), 8u64)] {
        let (outcome, service) = dispatch_request(
            config,
            Syscall::Write.as_u16() as u64,
            [1, data, 4, data + 256, 0, 0],
            0,
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::Ok, 0);
        assert_eq!(service.calls.len(), 1);
        let (outcome, service) = dispatch_request(
            config,
            Syscall::Write.as_u16() as u64,
            [1, data, 4, data + 256 + 1, 0, 0],
            0,
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::Misaligned, 3);
        assert!(service.calls.is_empty());
        let (outcome, service) = dispatch_request(
            config,
            Syscall::Read.as_u16() as u64,
            [0, 0x0080_0000, 4, data + 256, 0, 0],
            0,
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::InvalidPointer, 1);
        assert!(service.calls.is_empty());
        let permissions = lazalith_os::abi::FilePermissions::new(1).unwrap();
        let stat =
            FileStat::new(config, lazalith_os::abi::AbiFileKind::File, permissions, 7).unwrap();
        let encoded = stat.encode();
        assert_eq!(encoded.len(), 16);
        let decoded = FileStat::decode(&encoded, config).unwrap();
        assert_eq!(decoded.size(), 7);
        assert_eq!(decoded.kind(), lazalith_os::abi::AbiFileKind::File);
        let wide = [0x1_0000_0000u64, 0x1_0000_0000, 0, 0, 0, 0];
        let arguments = lazalith_os::abi::SyscallArguments::new(wide);
        if config == C::lz32() {
            assert!(
                arguments.word(config, 0).is_err(),
                "LZ32 arguments must reject a value above the word mask"
            );
            assert!(
                arguments.word(config, 1).is_err(),
                "every LZ32 word argument is bounded by the word mask"
            );
        } else {
            arguments.word(config, 0).unwrap();
        }
        assert_eq!(
            word_bytes as u32,
            u32::from(config.word_bytes()),
            "the record word width follows the architecture"
        );
    }
}

#[test]
fn read_write_guards_reject_out_of_range_lengths_without_partial_effects() {
    let data = lazalith_os::USER_DATA_START;
    let result = data + 256;
    for config in [C::lz32(), C::lz64()] {
        let (outcome, service) = dispatch_request(
            config,
            Syscall::Write.as_u16() as u64,
            [1, data, u64::MAX, result, 0, 0],
            0,
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::RangeOverflow, 1);
        assert!(service.calls.is_empty());
        let (outcome, service) = dispatch_request(
            config,
            Syscall::Read.as_u16() as u64,
            [0, data, u64::MAX, result, 0, 0],
            0,
            |_| {},
        );
        assert_return(&outcome, SyscallStatus::RangeOverflow, 1);
        assert!(service.calls.is_empty());
    }
}

#[test]
fn a_write_only_handle_cannot_read_through_the_dispatcher() {
    for config in [C::lz32(), C::lz64()] {
        let data = lazalith_os::USER_DATA_START;
        let result = data + 256;
        let buffer = data + 32;
        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        let node = filesystem.insert_file(b"/secret", b"classified").unwrap();
        let (outcome, process, service) = dispatch_filesystem_call(
            config,
            Syscall::Read,
            [2, buffer, 4, result, 0, 0],
            filesystem,
            |memory| {
                memory
                    .handles()
                    .open_file(node, FileAccess::new(false, true))
                    .unwrap();
            },
        );
        assert_return(&outcome, SyscallStatus::PermissionDenied, 0);
        let mut encoded = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(result), &mut encoded)
            .unwrap();
        assert_eq!(IoResult::decode(&encoded, config).unwrap().transferred(), 0);
        let mut written = [0u8; 4];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(buffer), &mut written)
            .unwrap();
        assert_eq!(&written, &[0, 0, 0, 0], "no bytes may be copied");
        assert_eq!(
            service
                .filesystem()
                .read_at(node, 0, 99, FileAccess::new(true, true))
                .unwrap(),
            b"classified"
        );
    }
}
