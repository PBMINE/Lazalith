use lazalith_cpu::{ExecutionContextId, Privilege, StatusRegister};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Condition, ControlRegister, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::{
    DispatchOutcome, KernelService, Process, ProcessId, ProgramImage, RoundRobinScheduler,
    SchedulerError, ServiceOutcome, ThreadId, USER_CODE_START, USER_DATA_START, USER_INITIAL_SP,
    UserMemory, UserMemoryContext, ValidatedSyscall,
};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress,
};

fn instruction(config: C, opcode: Opcode) -> [u8; 8] {
    encode(config, &Instruction::new(config, opcode, &[]).unwrap()).unwrap()
}

fn image(config: C, opcodes: &[Opcode]) -> ProgramImage {
    let mut bytes = Vec::new();
    for opcode in opcodes {
        bytes.extend_from_slice(&instruction(config, *opcode));
    }
    ProgramImage::new(config, 0, &bytes).unwrap()
}

fn syscall_image(config: C, number: u64) -> ProgramImage {
    let register = lazalith_types::RegisterIndex::try_from(0).unwrap();
    let load = Instruction::new(
        config,
        Opcode::Li,
        &[
            Operand::Register(register),
            Operand::Immediate(number as i32),
        ],
    )
    .unwrap();
    let mut bytes = encode(config, &load).unwrap().to_vec();
    bytes.extend_from_slice(&instruction(config, Opcode::Syscall));
    ProgramImage::new(config, 0, &bytes).unwrap()
}

fn time_image(config: C) -> ProgramImage {
    let number = lazalith_os::abi::Syscall::Time.as_u16() as u64;
    let register_zero = lazalith_types::RegisterIndex::try_from(0).unwrap();
    let register_one = lazalith_types::RegisterIndex::try_from(1).unwrap();
    let load_number = Instruction::new(
        config,
        Opcode::Li,
        &[
            Operand::Register(register_zero),
            Operand::Immediate(number as i32),
        ],
    )
    .unwrap();
    let load_result = Instruction::new(
        config,
        Opcode::Li,
        &[
            Operand::Register(register_one),
            Operand::Immediate(USER_DATA_START as i32),
        ],
    )
    .unwrap();
    let mut bytes = encode(config, &load_number).unwrap().to_vec();
    bytes.extend_from_slice(&encode(config, &load_result).unwrap());
    bytes.extend_from_slice(&instruction(config, Opcode::Syscall));
    ProgramImage::new(config, 0, &bytes).unwrap()
}

fn trap_image(config: C) -> ProgramImage {
    let instruction = Instruction::new(config, Opcode::Trap, &[Operand::Immediate(1)]).unwrap();
    ProgramImage::new(config, 0, &encode(config, &instruction).unwrap()).unwrap()
}

fn branch_outside_image(config: C) -> ProgramImage {
    let instruction = Instruction::new(
        config,
        Opcode::Br,
        &[Operand::Condition(Condition::Al), Operand::Immediate(2)],
    )
    .unwrap();
    ProgramImage::new(config, 0, &encode(config, &instruction).unwrap()).unwrap()
}

fn loop_image(config: C) -> ProgramImage {
    let instruction = Instruction::new(
        config,
        Opcode::Br,
        &[Operand::Condition(Condition::Al), Operand::Immediate(-2)],
    )
    .unwrap();
    ProgramImage::new(config, 0, &encode(config, &instruction).unwrap()).unwrap()
}

fn bad_process(id: u32, config: C) -> Process {
    Process::new(
        ProcessId::new(id).unwrap(),
        ThreadId::new(id).unwrap(),
        ProgramImage::new(config, 0, &[0xff; 8]).unwrap(),
    )
    .unwrap()
}

fn process(id: u32, config: C, opcodes: &[Opcode]) -> Process {
    Process::new(
        ProcessId::new(id).unwrap(),
        ThreadId::new(id).unwrap(),
        image(config, opcodes),
    )
    .unwrap()
}

struct ReturningService;

impl KernelService for ReturningService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        _memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        ServiceOutcome::Return(lazalith_os::abi::TaggedOutcome::success(0))
    }
}

struct BlockingService;

impl KernelService for BlockingService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        memory
            .transition(lazalith_os::ProcessState::Blocked)
            .unwrap();
        ServiceOutcome::Return(lazalith_os::abi::TaggedOutcome::success(0))
    }
}

struct ExitService;

impl KernelService for ExitService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        memory.exit(0).unwrap();
        ServiceOutcome::Exit
    }
}

struct ExitThenReturnService;

impl KernelService for ExitThenReturnService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        memory.exit(0).unwrap();
        ServiceOutcome::Return(lazalith_os::abi::TaggedOutcome::success(0))
    }
}

struct TimeExitReturnService;

impl KernelService for TimeExitReturnService {
    fn invoke(
        &mut self,
        _syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        memory.exit(0).unwrap();
        ServiceOutcome::Return(lazalith_os::abi::TaggedOutcome::success(0))
    }
}

fn machine(config: C) -> LazalithMachine<NoDevice> {
    let mut regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            32,
            RegionPermissions::new(true, true, true, false),
        )
        .unwrap(),
    ];
    regions.extend(UserMemory::regions(config).unwrap());
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions,
        pc: InstructionAddress::new(USER_CODE_START),
        sp: VirtualAddress::new(USER_INITIAL_SP),
        status: StatusRegister::new(Privilege::User, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    let mut kernel = Vec::new();
    kernel.extend_from_slice(&instruction(config, Opcode::Nop));
    kernel.extend_from_slice(&instruction(config, Opcode::Nop));
    kernel.extend_from_slice(&instruction(config, Opcode::Rfe));
    machine
        .load_bytes(PhysicalAddress::new(0x1000), &kernel)
        .unwrap();
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .unwrap();
    machine
}

#[test]
fn scheduler_rejects_invalid_configuration_and_duplicate_processes() {
    assert!(matches!(
        RoundRobinScheduler::new(0),
        Err(SchedulerError::ZeroQuantum)
    ));
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Halt]))
        .unwrap();
    assert!(matches!(
        scheduler.add_process(process(1, config, &[Opcode::Halt])),
        Err(SchedulerError::DuplicateProcess(_))
    ));
    assert!(
        scheduler
            .add_process(process(2, C::lz32(), &[Opcode::Halt]))
            .is_err()
    );
    assert_eq!(scheduler.processes().len(), 1);

    let mut bound = process(3, config, &[Opcode::Halt]);
    bound.mark_ready().unwrap();
    bound.mark_running().unwrap();
    let context = ExecutionContextId::new(99).unwrap();
    bound.activate(context).unwrap();
    assert!(matches!(
        scheduler.add_process(bound),
        Err(SchedulerError::ProcessExecution(
            lazalith_os::ProcessExecutionError::AlreadyBound { .. }
        ))
    ));
    assert_eq!(scheduler.processes().len(), 1);

    let mut bounded = RoundRobinScheduler::with_memory_budget(1, Some(1)).unwrap();
    assert!(matches!(
        bounded.add_process(process(3, config, &[Opcode::Nop])),
        Err(SchedulerError::MemoryBudgetExceeded { .. })
    ));
    assert_eq!(bounded.used_memory(), 0);
}

#[test]
fn scheduler_preserves_ready_and_blocked_admission_states() {
    let config = C::lz64();
    let mut ready = process(1, config, &[Opcode::Halt]);
    ready.mark_ready().unwrap();
    let mut blocked = process(2, config, &[Opcode::Halt]);
    blocked.mark_ready().unwrap();
    blocked.block().unwrap();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler.add_process(ready).unwrap();
    scheduler.add_process(blocked).unwrap();
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Ready
    );
    assert_eq!(
        scheduler
            .process(ProcessId::new(2).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Blocked
    );
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::NoRunnableProcess)
    ));
    scheduler.unblock(ProcessId::new(2).unwrap()).unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
}

#[test]
fn scheduler_run_suspends_a_sole_blocked_process() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Nop]))
        .unwrap();
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    scheduler
        .with_active_memory_context(&mut machine, |context| {
            context
                .transition(lazalith_os::ProcessState::Blocked)
                .unwrap();
        })
        .unwrap();
    let result = scheduler.run(&mut machine, 4).unwrap();
    assert_eq!(result.executed, 0);
    assert!(scheduler.active().is_none());
    scheduler.unblock(ProcessId::new(1).unwrap()).unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
}

#[test]
fn scheduler_reaps_terminal_processes_and_reclaims_budget() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    let resident = {
        let process = process(1, config, &[Opcode::Nop]);
        let resident = process.resident_bytes();
        scheduler.add_process(process).unwrap();
        resident
    };
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    scheduler.terminate_current(&mut machine, 0).unwrap();
    assert!(machine.step().is_err());
    assert_eq!(scheduler.used_memory(), resident);
    let removed = scheduler
        .remove_process(ProcessId::new(1).unwrap())
        .unwrap();
    assert_eq!(removed.id().get(), 1);
    assert_eq!(scheduler.used_memory(), 0);
    assert!(scheduler.process(ProcessId::new(1).unwrap()).is_none());
}

#[test]
fn scheduler_runs_multiple_user_programs_to_completion() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);

    let first = scheduler.step(&mut machine).unwrap();
    assert_eq!(first.process_id.get(), 1);
    assert!(matches!(first.event, MachineEvent::Stepped { .. }));
    assert_eq!(first.switched_to, Some(ProcessId::new(2).unwrap()));
    let second = scheduler.step(&mut machine).unwrap();
    assert_eq!(second.process_id.get(), 2);
    assert!(matches!(second.event, MachineEvent::Stepped { .. }));
    assert_eq!(second.switched_to, Some(ProcessId::new(1).unwrap()));
    assert_eq!(
        scheduler.terminate_current(&mut machine, 0).unwrap().get(),
        1
    );
    let third = scheduler.step(&mut machine).unwrap();
    assert_eq!(third.process_id.get(), 2);
    assert!(matches!(third.event, MachineEvent::Stepped { .. }));
    assert_eq!(
        scheduler.terminate_current(&mut machine, 7).unwrap().get(),
        2
    );
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .exit_code(),
        Some(0)
    );
    assert_eq!(
        scheduler
            .process(ProcessId::new(2).unwrap())
            .unwrap()
            .exit_code(),
        Some(7)
    );
    assert!(scheduler.active().is_none());
    let drained = scheduler.run(&mut machine, 4).unwrap();
    assert_eq!(drained.executed, 0);
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::NoRunnableProcess)
    ));
}

#[test]
fn round_robin_quantum_switches_at_a_safe_boundary() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(
            1,
            config,
            &[Opcode::Nop, Opcode::Nop, Opcode::Halt],
        ))
        .unwrap();
    scheduler
        .add_process(process(
            2,
            config,
            &[Opcode::Nop, Opcode::Nop, Opcode::Halt],
        ))
        .unwrap();
    let mut machine = machine(config);

    let first = scheduler.step(&mut machine).unwrap();
    assert_eq!(first.process_id.get(), 1);
    assert_eq!(first.switched_to, Some(ProcessId::new(2).unwrap()));
    let second = scheduler.step(&mut machine).unwrap();
    assert_eq!(second.process_id.get(), 2);
    assert_eq!(second.switched_to, Some(ProcessId::new(1).unwrap()));
    let third = scheduler.step(&mut machine).unwrap();
    assert_eq!(third.process_id.get(), 1);
    assert_eq!(third.switched_to, Some(ProcessId::new(2).unwrap()));
    let fourth = scheduler.step(&mut machine).unwrap();
    assert_eq!(fourth.process_id.get(), 2);
    assert_eq!(fourth.switched_to, Some(ProcessId::new(1).unwrap()));
    assert_eq!(scheduler.active().unwrap().remaining, 1);
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .primary_thread()
            .cpu()
            .pc()
            .as_u64(),
        USER_CODE_START + 16
    );
    assert_eq!(
        scheduler
            .process(ProcessId::new(2).unwrap())
            .unwrap()
            .primary_thread()
            .cpu()
            .pc()
            .as_u64(),
        USER_CODE_START + 16
    );
}

#[test]
fn scheduler_faults_one_process_and_keeps_the_other_runnable() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler.add_process(bad_process(1, config)).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    let fault = scheduler.step(&mut machine).unwrap();
    assert!(matches!(fault.event, MachineEvent::Trapped { .. }));
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Faulted
    );
    let next = scheduler.step(&mut machine).unwrap();
    assert_eq!(next.process_id.get(), 2);
    assert!(matches!(next.event, MachineEvent::Stepped { .. }));
    assert_eq!(
        scheduler.terminate_current(&mut machine, 0).unwrap().get(),
        2
    );
}

#[test]
fn scheduler_run_advances_after_completed_synchronous_fault() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler.add_process(bad_process(1, config)).unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Nop]))
        .unwrap();
    let mut machine = machine(config);
    let result = scheduler.run(&mut machine, 1).unwrap();
    assert_eq!(result.executed, 1);
    assert!(matches!(
        result.last.unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert_eq!(
        scheduler
            .process(ProcessId::new(2).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Running
    );
}

#[test]
fn scheduler_exposes_deterministic_active_thread_identity() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(2).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    let first = scheduler.step(&mut machine).unwrap();
    assert_eq!(first.process_id.get(), 1);
    let active = scheduler.active().unwrap();
    assert_eq!(active.process_id, ProcessId::new(1).unwrap());
    assert_eq!(active.thread_id, ThreadId::new(1).unwrap());
    assert_eq!(active.remaining, 1);
    assert!(active.execution_context.get() > 0);
    let second = scheduler.step(&mut machine).unwrap();
    assert!(matches!(second.event, MachineEvent::Stepped { .. }));
    assert_eq!(second.process_id.get(), 1);
}

#[test]
fn scheduler_context_switch_preserves_process_memory_ownership() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    let first_identity = scheduler.processes()[0].memory().identity();
    let second_identity = scheduler.processes()[1].memory().identity();
    assert_ne!(first_identity, second_identity);
    scheduler.step(&mut machine).unwrap();
    scheduler.step(&mut machine).unwrap();
    let active = scheduler.active().unwrap();
    assert!(machine.active_execution_context().is_some());
    assert_eq!(
        machine.memory().identity().clone(),
        scheduler
            .process(active.process_id)
            .unwrap()
            .memory()
            .identity()
    );
    assert_ne!(
        scheduler.processes()[0].memory().identity(),
        scheduler.processes()[1].memory().identity()
    );
}

#[test]
fn scheduler_reconciles_blocked_processes_before_execution() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(2).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    scheduler
        .with_active_memory_context(&mut machine, |context| {
            context
                .transition(lazalith_os::ProcessState::Blocked)
                .unwrap();
        })
        .unwrap();
    let next = scheduler.step(&mut machine).unwrap();
    assert_eq!(next.process_id.get(), 2);
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Blocked
    );
    scheduler.unblock(ProcessId::new(1).unwrap()).unwrap();
}

#[test]
fn scheduler_suspends_process_blocked_by_syscall_service() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(3).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                time_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Rfe),
        )
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut BlockingService)
        .unwrap();
    let completion = outcome.into_completion().unwrap();
    let returned = scheduler
        .return_from_syscall(&mut machine, completion)
        .unwrap();
    assert_eq!(
        returned.switched_to,
        Some(ProcessId::new(2).unwrap()),
        "active={:?} states={:?}",
        scheduler.active(),
        scheduler.processes()
    );
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Blocked
    );
    assert!(scheduler.active().is_some());
}

#[test]
fn scheduler_releases_exit_syscall_frame_without_returning() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                syscall_image(config, 1),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut ExitService)
        .unwrap();
    assert!(
        matches!(outcome, DispatchOutcome::Exit { exit_code: 0 }),
        "{outcome:?}"
    );
    assert!(scheduler.active().is_none());
    assert!(machine.active_execution_context().is_none());
    assert!(!machine.trap_controller().has_active_frame());
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Exited
    );
}

#[test]
fn scheduler_releases_a_service_fault_after_service_exit_mutation() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                syscall_image(config, 1),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut ExitThenReturnService)
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Fault(_)));
    assert!(scheduler.active().is_none());
    assert!(machine.active_execution_context().is_none());
    assert!(!machine.trap_controller().has_active_frame());
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Exited
    );
}

#[test]
fn scheduler_rejects_returning_service_after_nonexit_exit_mutation() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(3).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                time_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    for _ in 0..2 {
        assert!(matches!(
            scheduler.step(&mut machine).unwrap().event,
            MachineEvent::Stepped { .. }
        ));
    }
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut TimeExitReturnService)
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Fault(_)));
    assert!(scheduler.active().is_none());
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Exited
    );
}

#[test]
fn scheduler_poisoned_after_terminal_machine_error() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                trap_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Syscall),
        )
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    assert!(scheduler.step(&mut machine).is_err());
    assert!(scheduler.poisoned());
    assert!(scheduler.active().is_none());
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::Poisoned)
    ));
    scheduler.reset_after_poison(&mut machine).unwrap();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Halt]))
        .unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
    assert!(!scheduler.poisoned());
}

#[test]
fn scheduler_switches_after_architecturally_valid_out_of_image_control_state() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                branch_outside_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    let switched = scheduler.step(&mut machine).unwrap();
    assert_eq!(switched.process_id.get(), 1);
    assert_eq!(switched.switched_to, Some(ProcessId::new(2).unwrap()));
}

#[test]
fn scheduler_charges_user_trap_but_not_handler_frame() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                trap_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    let returned = scheduler.step(&mut machine).unwrap();
    assert!(matches!(returned.event, MachineEvent::Stepped { .. }));
    assert_eq!(returned.switched_to, Some(ProcessId::new(2).unwrap()));
}

#[test]
fn scheduler_poisons_if_handler_returns_to_supervisor() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                trap_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let register = lazalith_types::RegisterIndex::try_from(0).unwrap();
    let status = StatusRegister::new(Privilege::Supervisor, false).bits();
    let load_status = Instruction::new(
        config,
        Opcode::Li,
        &[
            Operand::Register(register),
            Operand::Immediate(status as i32),
        ],
    )
    .unwrap();
    let write_status = Instruction::new(
        config,
        Opcode::Csrw,
        &[
            Operand::Control(ControlRegister::Estatus),
            Operand::Register(register),
        ],
    )
    .unwrap();
    let mut handler = encode(config, &load_status).unwrap().to_vec();
    handler.extend_from_slice(&encode(config, &write_status).unwrap());
    handler.extend_from_slice(&instruction(config, Opcode::Rfe));
    let mut machine = machine(config);
    machine
        .load_bytes(PhysicalAddress::new(0x1000), &handler)
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
    assert!(scheduler.step(&mut machine).is_err());
    assert!(scheduler.poisoned());
    assert!(scheduler.active().is_none());
}

#[test]
fn scheduler_captures_syscalls_from_the_active_execution() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Syscall]))
        .unwrap();
    let mut machine = machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Rfe),
        )
        .unwrap();
    let _event = scheduler.step(&mut machine).unwrap();
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut ReturningService)
        .unwrap();
    assert!(matches!(outcome, DispatchOutcome::Return { .. }));
    let completion = outcome.into_completion().unwrap();
    let result = scheduler.return_from_syscall(&mut machine, completion);
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn scheduler_syscall_completion_switches_after_user_trap_charge() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Syscall]))
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Rfe),
        )
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    let outcome = scheduler
        .dispatch_syscall(&mut machine, &mut ReturningService)
        .unwrap();
    let completion = match outcome {
        DispatchOutcome::Return { completion, .. } => completion,
        other => panic!("expected returning syscall, got {other:?}"),
    };
    let returned = scheduler
        .return_from_syscall(&mut machine, completion)
        .unwrap();
    assert_eq!(returned.process_id.get(), 1);
    assert_eq!(returned.switched_to, Some(ProcessId::new(2).unwrap()));
}

#[test]
fn scheduler_service_context_uses_the_active_machine_space() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(2).unwrap(),
                ThreadId::new(2).unwrap(),
                loop_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    scheduler
        .with_active_memory_context(&mut machine, |context| {
            context
                .write_bytes(VirtualAddress::new(USER_DATA_START), b"active")
                .unwrap();
        })
        .unwrap();
    scheduler.step(&mut machine).unwrap();
    scheduler.step(&mut machine).unwrap();
    scheduler.step(&mut machine).unwrap();
    let mut bytes = [0u8; 6];
    scheduler
        .process(ProcessId::new(2).unwrap())
        .unwrap()
        .memory()
        .address_space()
        .peek(
            lazalith_types::PhysicalAddress::new(USER_DATA_START),
            &mut bytes,
        )
        .unwrap();
    assert_eq!(&bytes, b"active");
}

#[test]
fn scheduler_poisons_on_activation_failure_and_recovers_after_reset() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut kernel_regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            32,
            RegionPermissions::new(true, true, true, false),
        )
        .unwrap(),
    ];
    kernel_regions.extend(UserMemory::regions(config).unwrap());
    let mut machine: LazalithMachine<NoDevice> = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions: kernel_regions,
        pc: InstructionAddress::new(USER_CODE_START),
        sp: VirtualAddress::new(USER_INITIAL_SP),
        status: StatusRegister::new(Privilege::User, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    assert!(matches!(
        machine.state(),
        lazalith_machine::MachineState::Created
    ));
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::Machine(_))
    ));
    assert!(scheduler.poisoned());
    assert!(scheduler.active().is_none());
    assert!(!scheduler.recovery_required());
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::Poisoned)
    ));
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Ready
    );
    scheduler.reset_after_poison(&mut machine).unwrap();
    assert!(!scheduler.poisoned());
    machine
        .load_bytes(
            PhysicalAddress::new(USER_CODE_START),
            &instruction(config, Opcode::Halt),
        )
        .unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Rfe),
        )
        .unwrap();
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Halt]))
        .unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
}

#[test]
fn scheduler_detects_a_stale_execution_context_binding() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
    let active = scheduler.active().unwrap();
    machine
        .with_trap_controller_mut(Some(active.execution_context), |controller| {
            controller.set_execution_context(ExecutionContextId::new(200).unwrap());
        })
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::ProcessExecution(
            lazalith_os::ProcessExecutionError::ContextMismatch {
                expected,
                actual
            }
        )) if expected == active.execution_context
            && actual == ExecutionContextId::new(200).unwrap()
    ));
    assert!(scheduler.poisoned());
    assert_ne!(
        active.execution_context,
        ExecutionContextId::new(200).unwrap()
    );
}

#[test]
fn scheduler_rejects_host_termination_of_the_running_process() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(4).unwrap();
    scheduler
        .add_process(process(
            1,
            config,
            &[Opcode::Nop, Opcode::Nop, Opcode::Nop, Opcode::Halt],
        ))
        .unwrap();
    let mut machine = machine(config);
    scheduler.step(&mut machine).unwrap();
    let rejected = scheduler
        .with_active_memory_context(&mut machine, |context| context.exit(3))
        .unwrap();
    assert!(matches!(
        rejected,
        Err(lazalith_os::ProcessStateError::NotInSyscall { .. })
    ));
    let rejected = scheduler
        .with_active_memory_context(&mut machine, |context| {
            context.transition(lazalith_os::ProcessState::Ready)
        })
        .unwrap();
    assert!(matches!(
        rejected,
        Err(lazalith_os::ProcessStateError::InvalidTransition { .. })
    ));
    assert!(!scheduler.poisoned());
    assert_eq!(
        scheduler
            .process(ProcessId::new(1).unwrap())
            .unwrap()
            .state(),
        lazalith_os::ProcessState::Running
    );
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Stepped { .. }
    ));
}

#[test]
fn scheduler_resumes_a_blocked_process_with_its_own_cpu_context() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(2).unwrap();
    scheduler
        .add_process(process(
            1,
            config,
            &[Opcode::Nop, Opcode::Nop, Opcode::Halt],
        ))
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
    let after_one = scheduler
        .process(ProcessId::new(1).unwrap())
        .unwrap()
        .primary_thread()
        .cpu()
        .pc();
    scheduler
        .with_active_memory_context(&mut machine, |context| {
            context
                .transition(lazalith_os::ProcessState::Blocked)
                .unwrap();
        })
        .unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
    assert_eq!(scheduler.active(), None);
    scheduler.unblock(ProcessId::new(1).unwrap()).unwrap();
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
    let resumed = scheduler
        .process(ProcessId::new(1).unwrap())
        .unwrap()
        .primary_thread()
        .cpu()
        .pc();
    assert_eq!(
        resumed.as_u64(),
        after_one.as_u64() + 8,
        "a resumed process must continue after its saved program counter"
    );
}

#[test]
fn scheduler_unblock_enforces_its_preconditions_and_keeps_the_cursor() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::with_memory_budget(4, Some(10_000_000)).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    scheduler
        .add_process(process(2, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    scheduler
        .add_process(process(3, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    assert!(matches!(
        scheduler.unblock(ProcessId::new(1).unwrap()),
        Err(SchedulerError::ProcessNotBlocked(_))
    ));
    assert!(matches!(
        scheduler.unblock(ProcessId::new(9).unwrap()),
        Err(SchedulerError::ProcessNotFound(_))
    ));
    scheduler.step(&mut machine).unwrap();
    assert!(matches!(
        scheduler.unblock(ProcessId::new(1).unwrap()),
        Err(SchedulerError::ActiveProcess(_))
    ));
    scheduler
        .with_active_memory_context(&mut machine, |context| {
            context
                .transition(lazalith_os::ProcessState::Blocked)
                .unwrap();
        })
        .unwrap();
    scheduler.step(&mut machine).unwrap();
    scheduler.unblock(ProcessId::new(1).unwrap()).unwrap();
    assert!(matches!(
        scheduler.unblock(ProcessId::new(1).unwrap()),
        Err(SchedulerError::ProcessNotBlocked(_))
    ));
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
}

#[test]
fn scheduler_reaping_rejects_non_terminal_and_keeps_rotation_ordered() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::with_memory_budget(1, Some(10_000_000)).unwrap();
    for id in 1..=3 {
        scheduler
            .add_process(process(id, config, &[Opcode::Nop, Opcode::Halt]))
            .unwrap();
    }
    let mut machine = machine(config);
    assert!(matches!(
        scheduler.remove_process(ProcessId::new(1).unwrap()),
        Err(SchedulerError::ProcessNotTerminal(_))
    ));
    assert_eq!(scheduler.processes().len(), 3);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 2);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 3);
    let order: Vec<u32> = scheduler
        .processes()
        .iter()
        .map(|process| process.id().get())
        .collect();
    assert_eq!(order, vec![1, 2, 3]);
    assert_eq!(scheduler.used_memory(), 6488064);
    assert!(matches!(
        scheduler.remove_process(ProcessId::new(2).unwrap()),
        Err(SchedulerError::ProcessNotTerminal(_))
    ));

    for _ in 0..3 {
        scheduler.step(&mut machine).unwrap();
    }
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::NoRunnableProcess)
    ));
    let finished: Vec<u32> = scheduler
        .processes()
        .iter()
        .filter(|process| process.state().is_terminal())
        .map(|process| process.id().get())
        .collect();
    assert_eq!(
        finished.len(),
        3,
        "every image halts in Supervisor and faults"
    );
    let reaped = scheduler
        .remove_process(ProcessId::new(2).unwrap())
        .unwrap();
    assert_eq!(reaped.id().get(), 2);
    assert_eq!(scheduler.processes().len(), 2);
    assert_eq!(
        scheduler.used_memory(),
        4325376,
        "reaping must release the process memory budget"
    );
    let remaining: Vec<u32> = scheduler
        .processes()
        .iter()
        .map(|process| process.id().get())
        .collect();
    assert_eq!(remaining, vec![1, 3], "rotation order is preserved");
    let reaped = scheduler
        .remove_process(ProcessId::new(1).unwrap())
        .unwrap();
    assert_eq!(reaped.id().get(), 1);
    let reaped = scheduler
        .remove_process(ProcessId::new(3).unwrap())
        .unwrap();
    assert_eq!(reaped.id().get(), 3);
    assert_eq!(scheduler.processes().len(), 0);
    assert_eq!(scheduler.used_memory(), 0);
    assert!(scheduler.active().is_none());
}

#[test]
fn scheduler_preserves_the_faulted_machine_state_while_poisoned() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(1).unwrap();
    scheduler
        .add_process(
            Process::new(
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
                trap_image(config),
            )
            .unwrap(),
        )
        .unwrap();
    let mut machine = machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &instruction(config, Opcode::Syscall),
        )
        .unwrap();
    assert!(matches!(
        scheduler.step(&mut machine).unwrap().event,
        MachineEvent::Trapped { .. }
    ));
    assert!(scheduler.step(&mut machine).is_err());
    assert!(scheduler.poisoned());
    assert_eq!(
        machine.state(),
        lazalith_machine::MachineState::Faulted,
        "poisoning must not rewrite a terminal machine as merely halted"
    );
    assert!(!machine.is_halted());
    assert!(machine.active_execution_context().is_none());
    assert!(machine.trap_controller().is_terminal());
    assert!(matches!(
        scheduler.step(&mut machine),
        Err(SchedulerError::Poisoned)
    ));
}

#[test]
fn scheduler_release_returns_the_machine_to_halted() {
    let config = C::lz64();
    let mut scheduler = RoundRobinScheduler::new(4).unwrap();
    scheduler
        .add_process(process(1, config, &[Opcode::Nop, Opcode::Halt]))
        .unwrap();
    let mut machine = machine(config);
    assert_eq!(scheduler.step(&mut machine).unwrap().process_id.get(), 1);
    assert_eq!(machine.state(), lazalith_machine::MachineState::Running);
    scheduler.terminate_current(&mut machine, 0).unwrap();
    assert_eq!(machine.state(), lazalith_machine::MachineState::Halted);
    assert!(machine.is_halted());
    assert!(machine.active_execution_context().is_none());
}
