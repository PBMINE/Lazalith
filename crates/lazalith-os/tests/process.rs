use lazalith_cpu::{ExecutionContextId, Privilege};
use lazalith_os::{
    HandleError, Process, ProcessExecutionError, ProcessHandleEntry, ProcessId, ProcessState,
    ProcessStateError, ProgramError, ProgramImage, Thread, ThreadError, ThreadId, USER_CODE_LENGTH,
    USER_CODE_START, USER_DATA_START, USER_INITIAL_SP, UserMemory,
};
use lazalith_os_abi::{FileHandle, ProcessHandle};
use lazalith_types::{ArchitectureConfig as C, PhysicalAddress};

fn image(config: C) -> ProgramImage {
    ProgramImage::new(config, 0, &[0; 8]).unwrap()
}

#[test]
fn process_and_thread_ids_reject_zero() {
    assert!(ProcessId::new(0).is_none());
    assert!(ThreadId::new(0).is_none());
    assert_eq!(ProcessId::new(7).unwrap().get(), 7);
    assert_eq!(ThreadId::new(9).unwrap().get(), 9);
}

#[test]
fn program_images_validate_entry_size_alignment_and_ownership() {
    for config in [C::lz32(), C::lz64()] {
        let program = image(config);
        assert_eq!(program.config(), config);
        assert_eq!(program.entry().as_u64(), USER_CODE_START);
        assert_eq!(program.bytes(), &[0; 8]);
        assert!(matches!(
            ProgramImage::new(config, 0, &[]),
            Err(ProgramError::Empty)
        ));
        assert!(matches!(
            ProgramImage::new(config, 1, &[0; 8]),
            Err(ProgramError::EntryMisaligned { offset: 1, .. })
        ));
        assert!(matches!(
            ProgramImage::new(config, 8, &[0; 8]),
            Err(ProgramError::EntryOutsideImage { offset: 8, .. })
        ));
        let oversized = vec![0; usize::try_from(USER_CODE_LENGTH).unwrap() + 1];
        assert!(matches!(
            ProgramImage::new(config, 0, &oversized),
            Err(ProgramError::ImageTooLarge { .. })
        ));
    }
}

#[test]
fn process_owns_memory_image_stack_thread_cpu_state_and_handles() {
    for config in [C::lz32(), C::lz64()] {
        let process = Process::new(
            ProcessId::new(1).unwrap(),
            ThreadId::new(1).unwrap(),
            image(config),
        )
        .unwrap();
        assert_eq!(process.id().get(), 1);
        assert_eq!(process.state(), ProcessState::Created);
        assert_eq!(process.exit_code(), None);
        assert_eq!(process.stack().initial_sp().as_u64(), USER_INITIAL_SP);
        assert_eq!(process.program().entry().as_u64(), USER_CODE_START);
        assert_eq!(process.primary_thread().id().get(), 1);
        assert_eq!(process.primary_thread().cpu().privilege(), Privilege::User);
        assert_eq!(
            process.primary_thread().cpu().pc().as_u64(),
            USER_CODE_START
        );
        assert_eq!(
            process.primary_thread().cpu().sp().as_u64(),
            USER_INITIAL_SP
        );
        assert!(!process.primary_thread().cpu().status().interrupts_enabled());
        assert!(process.handles().is_empty());
        let mut code = [0u8; 8];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(USER_CODE_START), &mut code)
            .unwrap();
        assert_eq!(code, [0; 8]);
    }
}

#[test]
fn process_context_lends_owned_memory_and_handles_only_to_its_process() {
    let mut process = Process::new(
        ProcessId::new(8).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let mut context = process.memory_context().unwrap();
    assert_eq!(context.process_id(), ProcessId::new(8).unwrap());
    assert_eq!(context.thread_id(), ThreadId::new(1).unwrap());
    assert_eq!(context.state(), ProcessState::Created);
    assert_eq!(context.thread().id(), ThreadId::new(1).unwrap());
    assert_eq!(context.program().entry().as_u64(), USER_CODE_START);
    assert_eq!(context.stack().initial_sp().as_u64(), USER_INITIAL_SP);
    assert!(context.allocate(16, 8).is_ok());
    assert!(matches!(
        context.transition(ProcessState::Exited),
        Err(ProcessStateError::InvalidTransition { .. })
    ));
    context.transition(ProcessState::Ready).unwrap();
    assert_eq!(context.state(), ProcessState::Ready);
    assert!(matches!(
        context.exit(7),
        Err(ProcessStateError::NotInSyscall { .. })
    ));
    assert_eq!(context.exit_code(), None);
    assert_eq!(context.state(), ProcessState::Ready);
    context
        .write_bytes(
            lazalith_types::VirtualAddress::new(USER_DATA_START),
            b"private",
        )
        .unwrap();
    context
        .handles()
        .insert(ProcessHandleEntry::File(FileHandle::new(2).unwrap()))
        .unwrap();
    drop(context);
    let mut bytes = [0u8; 7];
    process
        .memory()
        .address_space()
        .peek(PhysicalAddress::new(USER_DATA_START), &mut bytes)
        .unwrap();
    assert_eq!(&bytes, b"private");
    assert_eq!(process.handles().len(), 1);
    assert_eq!(process.state(), ProcessState::Ready);
    assert_eq!(process.exit_code(), None);
}

#[test]
fn process_spaces_are_isolated_and_images_reject_cross_mode_loading() {
    let mut first = Process::new(
        ProcessId::new(9).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let second = Process::new(
        ProcessId::new(10).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let mut context = first.memory_context().unwrap();
    context
        .write_bytes(
            lazalith_types::VirtualAddress::new(USER_DATA_START),
            b"first",
        )
        .unwrap();
    drop(context);
    assert_ne!(first.memory().identity(), second.memory().identity());
    let mut bytes = [0u8; 5];
    second
        .memory()
        .address_space()
        .peek(PhysicalAddress::new(USER_DATA_START), &mut bytes)
        .unwrap();
    assert_eq!(&bytes, b"\0\0\0\0\0");

    let image = image(C::lz32());
    let mut memory = UserMemory::new(C::lz64()).unwrap();
    assert!(matches!(
        image.load_into(&mut memory),
        Err(ProgramError::ArchitectureMismatch { .. })
    ));
}

#[test]
fn process_state_transitions_are_explicit_and_terminal() {
    let mut process = Process::new(
        ProcessId::new(2).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    assert_eq!(
        process.mark_running(),
        Err(ProcessStateError::InvalidTransition {
            process_id: ProcessId::new(2).unwrap(),
            from: ProcessState::Created,
            to: ProcessState::Running
        })
    );
    assert_eq!(process.state(), ProcessState::Created);
    process.mark_ready().unwrap();
    process.mark_running().unwrap();
    process.preempt().unwrap();
    assert_eq!(process.state(), ProcessState::Ready);
    process.mark_running().unwrap();
    process.block().unwrap();
    assert_eq!(process.state(), ProcessState::Blocked);
    process.mark_ready().unwrap();
    process.exit(7).unwrap();
    assert_eq!(process.state(), ProcessState::Exited);
    assert_eq!(process.exit_code(), Some(7));
    assert_eq!(
        process.mark_ready(),
        Err(ProcessStateError::Terminal {
            process_id: ProcessId::new(2).unwrap(),
            state: ProcessState::Exited
        })
    );

    let mut faulted = Process::new(
        ProcessId::new(3).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    faulted.fault().unwrap();
    assert_eq!(faulted.state(), ProcessState::Faulted);
    assert_eq!(
        faulted.exit(1),
        Err(ProcessStateError::Terminal {
            process_id: ProcessId::new(3).unwrap(),
            state: ProcessState::Faulted
        })
    );
}

#[test]
fn process_handle_table_is_explicit_and_typed() {
    let mut process = Process::new(
        ProcessId::new(4).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let file = FileHandle::new(2).unwrap();
    let child = ProcessHandle::new(2).unwrap();
    process
        .handles_mut()
        .insert(ProcessHandleEntry::File(file))
        .unwrap();
    process
        .handles_mut()
        .insert(ProcessHandleEntry::Child(child))
        .unwrap();
    assert_eq!(process.handles().len(), 2);
    assert!(process.handles().contains(ProcessHandleEntry::File(file)));
    assert!(matches!(
        process.handles_mut().insert(ProcessHandleEntry::File(file)),
        Err(HandleError::Duplicate { .. })
    ));
    process
        .handles_mut()
        .remove(ProcessHandleEntry::File(file))
        .unwrap();
    assert!(matches!(
        process.handles_mut().remove(ProcessHandleEntry::File(file)),
        Err(HandleError::NotFound { .. })
    ));
    assert_eq!(
        process.handles().entries(),
        &[ProcessHandleEntry::Child(child)]
    );
}

#[test]
fn thread_rejects_a_stack_pointer_outside_its_owned_stack() {
    let process = Process::new(
        ProcessId::new(11).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    assert!(matches!(
        Thread::for_process(
            ThreadId::new(2).unwrap(),
            &process,
            lazalith_types::InstructionAddress::new(USER_CODE_START),
            lazalith_types::VirtualAddress::new(0x1000),
        ),
        Err(ThreadError::StackOutside { .. })
    ));
    assert!(matches!(
        Thread::for_process(
            ThreadId::new(2).unwrap(),
            &process,
            lazalith_types::InstructionAddress::new(USER_CODE_START + 1),
            process.stack().initial_sp(),
        ),
        Err(ThreadError::EntryOutsideImage { .. })
    ));
    assert!(
        Thread::for_process(
            ThreadId::new(2).unwrap(),
            &process,
            process.program().entry(),
            process.stack().initial_sp(),
        )
        .is_ok()
    );
}

#[test]
fn process_execution_context_is_explicit_and_reversible() {
    let mut process = Process::new(
        ProcessId::new(14).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let context = ExecutionContextId::new(42).unwrap();
    assert!(matches!(
        process.activate(context),
        Err(ProcessExecutionError::NotRunning { .. })
    ));
    process.mark_ready().unwrap();
    process.mark_running().unwrap();
    process.activate(context).unwrap();
    assert_eq!(process.execution_context(), Some(context));
    assert!(matches!(
        process.activate(ExecutionContextId::new(43).unwrap()),
        Err(ProcessExecutionError::AlreadyBound { .. })
    ));
    assert!(matches!(
        process.deactivate(ExecutionContextId::new(43).unwrap()),
        Err(ProcessExecutionError::ContextMismatch { .. })
    ));
    process.deactivate(context).unwrap();
    assert_eq!(process.execution_context(), None);
    assert!(matches!(
        process.deactivate(context),
        Err(ProcessExecutionError::NotBound { .. })
    ));
}

#[test]
fn additional_threads_are_checked_and_attached_to_one_process() {
    let mut process = Process::new(
        ProcessId::new(12).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let second = Thread::for_process(
        ThreadId::new(2).unwrap(),
        &process,
        process.program().entry(),
        process.stack().initial_sp(),
    )
    .unwrap();
    assert_eq!(second.process_id(), ProcessId::new(12).unwrap());
    process.attach_thread(second).unwrap();
    assert_eq!(process.threads().len(), 2);
    let duplicate = Thread::for_process(
        ThreadId::new(2).unwrap(),
        &process,
        process.program().entry(),
        process.stack().initial_sp(),
    )
    .unwrap();
    assert!(matches!(
        process.attach_thread(duplicate),
        Err(ThreadError::DuplicateThread { .. })
    ));

    let other = Process::new(
        ProcessId::new(13).unwrap(),
        ThreadId::new(1).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let foreign = Thread::for_process(
        ThreadId::new(3).unwrap(),
        &other,
        other.program().entry(),
        other.stack().initial_sp(),
    )
    .unwrap();
    assert!(matches!(
        process.attach_thread(foreign),
        Err(ThreadError::ForeignProcess { .. })
    ));
    let context = process
        .memory_context_for_thread(ThreadId::new(2).unwrap())
        .unwrap();
    assert_eq!(context.thread_id(), ThreadId::new(2).unwrap());
}

#[test]
fn process_into_parts_transfers_complete_ownership() {
    let process = Process::new(
        ProcessId::new(5).unwrap(),
        ThreadId::new(6).unwrap(),
        image(C::lz64()),
    )
    .unwrap();
    let parts = process.into_parts();
    assert_eq!(parts.id.get(), 5);
    assert_eq!(parts.state, ProcessState::Created);
    assert_eq!(parts.exit_code, None);
    assert_eq!(parts.memory.config(), C::lz64());
    assert_eq!(parts.stack.initial_sp().as_u64(), USER_INITIAL_SP);
    assert_eq!(parts.program.entry().as_u64(), USER_CODE_START);
    assert_eq!(parts.threads.len(), 1);
    assert_eq!(parts.threads[0].id().get(), 6);
    assert!(parts.handles.is_empty());
    assert!(parts.execution_context.is_none());
}
