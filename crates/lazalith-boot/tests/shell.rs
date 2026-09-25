use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::MachineEvent;
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxArchitecture, ProcessId, ProcessState, ThreadId,
    VirtualFileSystem, VirtualTerminal, build_init_shell_image,
};
use lazalith_types::{ArchitectureConfig as C, InstructionAddress, PhysicalAddress};

fn kernel(config: C) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

#[test]
fn boot_handoff_runs_native_init_shell_in_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let boot = BootImage::new(config, kernel(config), 0).unwrap();
        let mut machine = boot.start(DeviceManager::<NoDevice>::new()).unwrap();
        machine
            .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
            .unwrap();
        assert_eq!(
            machine.architectural_state().privilege(),
            Privilege::Supervisor
        );
        assert!(matches!(
            machine.step().unwrap(),
            MachineEvent::Stepped { .. }
        ));

        let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
        filesystem.insert_file(b"/hello.txt", b"hello\n").unwrap();
        filesystem.insert_directory(b"/dir").unwrap();
        filesystem.insert_file(b"/dir/a.txt", b"a").unwrap();
        filesystem.insert_file(b"/dir/z.txt", b"z").unwrap();
        let mut kernel = LazalithKernel::new(
            1000,
            VirtualTerminal::new(b"help\necho hi\nls\ncat\n").unwrap(),
            filesystem,
        )
        .unwrap();
        kernel
            .start_init_shell(
                LzxArchitecture::from_config(config),
                ProcessId::new(1).unwrap(),
                ThreadId::new(1).unwrap(),
            )
            .unwrap();

        let mut exited = false;
        for _ in 0..10000 {
            let step = kernel.step(&mut machine).unwrap();
            if let Some(KernelServiceOutcome::Exit(0)) = step.outcome {
                exited = true;
                break;
            }
            if let Some(KernelServiceOutcome::Exit(_) | KernelServiceOutcome::Fault(_)) =
                step.outcome
            {
                panic!("native shell did not exit cleanly");
            }
        }
        assert!(exited);
        let process = kernel
            .scheduler()
            .process(ProcessId::new(1).unwrap())
            .unwrap();
        assert_eq!(process.state(), ProcessState::Exited);
        assert_eq!(process.exit_code(), Some(0));
        assert!(process.execution_context().is_none());
        assert!(kernel.scheduler().active().is_none());
        assert!(!machine.trap_controller().has_active_frame());
        let output = kernel.terminal().terminal().output();
        assert!(output.windows(4).any(|window| window == b"help"));
        assert!(output.windows(3).any(|window| window == b"hi\n"));
        assert!(output.windows(4).any(|window| window == b"dir\n"));
        assert!(output.windows(10).any(|window| window == b"hello.txt\n"));
        assert!(output.windows(5).any(|window| window == b"hello"));
    }
}

#[test]
fn native_shell_image_uses_the_fixed_boot_user_layout() {
    let image = build_init_shell_image(LzxArchitecture::Lz64).unwrap();
    let process = image
        .load_process(ProcessId::new(2).unwrap(), ThreadId::new(2).unwrap())
        .unwrap();
    assert_eq!(process.primary_thread().cpu().privilege(), Privilege::User);
    assert_eq!(
        process.program().entry(),
        InstructionAddress::new(lazalith_os::USER_CODE_START)
    );
    assert_eq!(
        process
            .memory()
            .address_space()
            .regions()
            .iter()
            .find(|region| region.start() == PhysicalAddress::new(lazalith_os::USER_CODE_START))
            .unwrap()
            .length(),
        lazalith_os::USER_CODE_LENGTH
    );
}
