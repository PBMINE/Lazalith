use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::{Privilege, TrapCause};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::MachineEvent;
use lazalith_os::{
    DispatchOutcome, INIT_CODE_LENGTH, KernelService, LzxArchitecture, ProcessId,
    RoundRobinScheduler, ServiceOutcome, ThreadId, USER_CODE_START, UserMemoryContext,
    ValidatedSyscall, ValidatedSyscallKind, build_init_image,
};
use lazalith_types::{ArchitectureConfig as C, InstructionAddress};

struct ExitService;

impl KernelService for ExitService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        _memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::Exit { exit_code: 0 } => ServiceOutcome::Exit,
            _ => ServiceOutcome::Return(lazalith_os::abi::TaggedOutcome::failure(
                lazalith_os::abi::SyscallError::Internal,
                0,
            )),
        }
    }
}

#[test]
fn bootloader_handoff_runs_init_to_nonreturning_exit() {
    for config in [C::lz32(), C::lz64()] {
        let kernel = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();
        let boot = BootImage::new(config, kernel.to_vec(), 0).unwrap();
        let mut machine = boot.start(DeviceManager::<NoDevice>::new()).unwrap();
        machine.set_trap_vector(boot.entry()).unwrap();
        assert_eq!(
            machine.architectural_state().pc().as_u64(),
            KERNEL_LOAD_ADDRESS
        );
        assert_eq!(
            machine.architectural_state().status().privilege(),
            Privilege::Supervisor
        );

        let init = build_init_image(LzxArchitecture::from_config(config)).unwrap();
        let process = init
            .load_process(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
            .unwrap();
        let mut scheduler = RoundRobinScheduler::new(1).unwrap();
        scheduler.add_process(process).unwrap();

        let first = scheduler.step(&mut machine).unwrap();
        assert!(matches!(first.event, MachineEvent::Stepped { .. }));
        assert_eq!(
            machine.architectural_state().pc().as_u64(),
            USER_CODE_START + 8
        );
        assert_eq!(machine.architectural_state().privilege(), Privilege::User);
        assert_eq!(
            machine
                .architectural_state()
                .registers()
                .read_raw(0)
                .unwrap(),
            1
        );

        let second = scheduler.step(&mut machine).unwrap();
        let MachineEvent::Trapped { event } = second.event else {
            panic!("init syscall did not trap");
        };
        assert_eq!(event.cause, TrapCause::Syscall);
        assert_eq!(
            event.resume_pc,
            InstructionAddress::new(USER_CODE_START + INIT_CODE_LENGTH as u64)
        );
        assert!(machine.trap_controller().has_active_frame());

        let mut service = ExitService;
        let outcome = scheduler
            .dispatch_syscall(&mut machine, &mut service)
            .unwrap();
        assert_eq!(outcome, DispatchOutcome::Exit { exit_code: 0 });
        let process = scheduler.process(ProcessId::new(1).unwrap()).unwrap();
        assert_eq!(process.state(), lazalith_os::ProcessState::Exited);
        assert_eq!(process.exit_code(), Some(0));
        assert!(process.execution_context().is_none());
        assert!(scheduler.active().is_none());
        assert!(!machine.trap_controller().has_active_frame());
    }
}
