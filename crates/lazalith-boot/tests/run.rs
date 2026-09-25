use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::MachineEvent;
use lazalith_os::{
    HeadlessShell, KernelServiceOutcome, LazalithKernel, ProcessId, ProcessState, ThreadId,
    VirtualFileSystem, VirtualTerminal,
};
use lazalith_toolchain::{LinkOptions, assemble_named, link_objects};
use lazalith_types::{ArchitectureConfig as C, InstructionAddress};

fn kernel(config: C) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

#[test]
fn shell_launches_a_real_assembly_program_and_observes_console_output() {
    let config = C::lz64();
    let object = assemble_named(
        "hello.lzs",
        ".arch lz64\n.entry _start\n.section .rodata\nmessage:\n.asciz \"Hello from assembly\\n\"\n.section .text\n_start:\nLI r0, 2\nLI r1, 1\nLI r2, message\nLI r3, 20\nLI r4, io_result\nLI r5, 0\nLI r6, 0\nLI r7, 0\nSYSCALL\nLI r0, 1\nLI r1, 0\nSYSCALL\n.section .bss\nio_result:\n.zero 16\n",
    )
    .unwrap();
    let program = link_objects(&[object], &LinkOptions::default()).unwrap();
    let image_bytes = program.image().to_bytes().unwrap();
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

    let mut shell_filesystem = VirtualFileSystem::with_defaults().unwrap();
    shell_filesystem
        .insert_file(b"/hello.lzx", &image_bytes)
        .unwrap();
    let mut shell = HeadlessShell::new(shell_filesystem);
    let mut kernel_runtime = LazalithKernel::new(
        1000,
        VirtualTerminal::new(b"").unwrap(),
        VirtualFileSystem::with_defaults().unwrap(),
    )
    .unwrap();
    shell
        .execute_run_command(
            b"run /hello.lzx",
            &mut kernel_runtime,
            ProcessId::new(9).unwrap(),
            ThreadId::new(9).unwrap(),
        )
        .unwrap();

    let mut exited = false;
    for _ in 0..10000 {
        let step = kernel_runtime.step(&mut machine).unwrap();
        if let Some(KernelServiceOutcome::Exit(0)) = step.outcome {
            exited = true;
            break;
        }
        if let Some(KernelServiceOutcome::Exit(_) | KernelServiceOutcome::Fault(_)) = step.outcome {
            panic!("assembly program did not exit cleanly");
        }
    }
    assert!(exited);
    assert!(
        kernel_runtime
            .terminal()
            .terminal()
            .output()
            .windows(20)
            .any(|window| window == b"Hello from assembly\n")
    );
    let process = kernel_runtime
        .scheduler()
        .process(ProcessId::new(9).unwrap())
        .unwrap();
    assert_eq!(process.state(), ProcessState::Exited);
    assert_eq!(process.exit_code(), Some(0));
}
