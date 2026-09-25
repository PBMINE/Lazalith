use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::MachineEvent;
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxArchitecture, LzxImage, ProcessId, ProcessState,
    ThreadId, VirtualFileSystem, VirtualTerminal, build_init_image,
};
use lazalith_toolchain::{assemble_named, link_object};
use lazalith_types::{ArchitectureConfig as C, InstructionAddress};

fn kernel(config: C) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

fn source(architecture: &str) -> String {
    format!(".arch {architecture}\n.entry _start\n_start:\n    LI r0, 1\n    SYSCALL\n")
}

#[test]
fn assembly_object_executable_process_path_runs_in_both_modes() {
    for (config, architecture, name) in [
        (C::lz32(), LzxArchitecture::Lz32, "lz32"),
        (C::lz64(), LzxArchitecture::Lz64, "lz64"),
    ] {
        let object = assemble_named("hello.lzs", &source(name)).unwrap();
        assert_eq!(object.architecture(), architecture);
        assert_eq!(object.entry_offset(), 0);
        let image = link_object(&object).unwrap();
        let bytes = image.to_bytes().unwrap();
        assert_eq!(
            bytes,
            build_init_image(architecture).unwrap().to_bytes().unwrap()
        );
        let image = LzxImage::from_bytes(&bytes).unwrap();
        let process = image
            .load_process(ProcessId::new(7).unwrap(), ThreadId::new(7).unwrap())
            .unwrap();

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

        let mut kernel = LazalithKernel::new(
            1000,
            VirtualTerminal::new(b"").unwrap(),
            VirtualFileSystem::with_defaults().unwrap(),
        )
        .unwrap();
        kernel.scheduler_mut().add_process(process).unwrap();

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
                panic!("assembled process did not exit cleanly");
            }
        }
        assert!(exited);
        let process = kernel
            .scheduler()
            .process(ProcessId::new(7).unwrap())
            .unwrap();
        assert_eq!(process.state(), ProcessState::Exited);
        assert_eq!(process.exit_code(), Some(0));
        assert!(process.execution_context().is_none());
        assert!(kernel.scheduler().active().is_none());
        assert!(!machine.trap_controller().has_active_frame());
    }
}

#[test]
fn assembly_diagnostics_retain_source_locations() {
    let error = assemble_named(
        "bad.lzs",
        ".arch lz64\n.entry _start\n_start:\n    LI r16, 1\n",
    )
    .unwrap_err();
    let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
        panic!("expected an assembly diagnostic");
    };
    let diagnostic = error.diagnostic().unwrap();
    assert_eq!(diagnostic.code().as_str(), "E139");
    let rendered =
        lazalith_diagnostics::render_plain(diagnostic, error.sources().unwrap()).unwrap();
    assert!(rendered.contains("bad.lzs:4:"));
}

#[test]
fn empty_entry_program_retains_an_assembly_diagnostic() {
    let error = assemble_named("empty.lzs", ".arch lz64\n.entry _start\n_start:\n").unwrap_err();
    let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
        panic!("expected an assembly diagnostic");
    };
    assert_eq!(error.diagnostic().unwrap().code().as_str(), "E104");
}

#[test]
fn object_bridge_rejects_noncanonical_code() {
    let mut code = encode(
        C::lz64(),
        &Instruction::new(C::lz64(), Opcode::Nop, &[]).unwrap(),
    )
    .unwrap()
    .to_vec();
    code[7] |= 1;
    let object = lazalith_toolchain::ObjectFile::new(LzxArchitecture::Lz64, code, 0);
    assert!(object.is_err());
}
