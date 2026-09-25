use lazalith_cpu::{Privilege, StatusRegister};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, decode, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::{
    DispatchOutcome, FileSystemService, HeadlessShell, LzxArchitecture, LzxImage, LzxSectionKind,
    NATIVE_SHELL_BSS_LENGTH, NATIVE_SHELL_BSS_OFFSET, NATIVE_SHELL_CAT_PATH,
    NATIVE_SHELL_DATA_LENGTH, NATIVE_SHELL_DATA_OFFSET, NATIVE_SHELL_DIRECTORY_OFFSET,
    NATIVE_SHELL_FILE_BUFFER_OFFSET, NATIVE_SHELL_HELP, NATIVE_SHELL_IO_RESULT_OFFSET,
    NATIVE_SHELL_LINE_BUFFER_OFFSET, NATIVE_SHELL_LS_HEADER, NATIVE_SHELL_LS_PATH,
    NATIVE_SHELL_PROCESS_LAUNCH_SUPPORTED, NATIVE_SHELL_PROMPT, NATIVE_SHELL_REQUIRED_DATA,
    NATIVE_SHELL_RUN_DEFERRED, ProcessId, RoundRobinScheduler, ShellCommand, ShellError,
    ShellOutcome, TerminalService, ThreadId, USER_CODE_START, USER_DATA_START, USER_INITIAL_SP,
    USER_STACK_LENGTH, UserMemory, VirtualFileSystem, VirtualTerminal, build_init_shell_image,
};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress,
};

fn assert_canonical_code(architecture: LzxArchitecture, code: &[u8]) {
    let config = architecture.config();
    assert!(!code.is_empty());
    assert_eq!(code.len() % 8, 0);
    let mut saw_syscall = false;
    let mut saw_branch = false;
    let mut saw_load = false;
    for index in (0..code.len()).step_by(8) {
        let chunk = &code[index..index + 8];
        let instruction = decode(config, chunk).unwrap();
        assert_eq!(encode(config, &instruction).unwrap(), chunk);
        saw_syscall |= instruction.opcode() == Opcode::Syscall;
        saw_branch |= instruction.opcode() == Opcode::Br;
        saw_load |= instruction.opcode() == Opcode::Ldz;
    }
    assert!(saw_syscall);
    assert!(saw_branch);
    assert!(saw_load);
}

fn machine(config: C) -> LazalithMachine<NoDevice> {
    let mut regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            16,
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
    let rfe = Instruction::new(config, Opcode::Rfe, &[]).unwrap();
    machine
        .load_bytes(PhysicalAddress::new(0x1000), &encode(config, &rfe).unwrap())
        .unwrap();
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .unwrap();
    machine
}

fn run_native_shell(architecture: LzxArchitecture, input: &[u8]) -> (Vec<u8>, u64) {
    let config = architecture.config();
    let image = build_init_shell_image(architecture).unwrap();
    let process = image
        .load_process(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .unwrap();
    let mut scheduler = RoundRobinScheduler::new(1000).unwrap();
    scheduler.add_process(process).unwrap();
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem
        .insert_file(NATIVE_SHELL_CAT_PATH, b"hello\n")
        .unwrap();
    let mut service = TerminalService::new(
        VirtualTerminal::new(input).unwrap(),
        FileSystemService::new(filesystem),
    );
    let mut machine = machine(config);
    for _ in 0..4000 {
        let step = scheduler.step(&mut machine).unwrap();
        if let MachineEvent::Trapped { .. } = step.event {
            let outcome = scheduler
                .dispatch_syscall(&mut machine, &mut service)
                .unwrap();
            match outcome {
                DispatchOutcome::Return { completion, .. } => {
                    scheduler
                        .return_from_syscall(&mut machine, completion)
                        .unwrap();
                }
                DispatchOutcome::Exit { .. } => break,
                DispatchOutcome::Fault(error) => panic!("native shell faulted: {error:?}"),
            }
        }
    }
    (
        service.terminal().output().to_vec(),
        service.terminal().screen_generation(),
    )
}

#[test]
fn native_shell_builder_emits_a_canonical_three_section_lzx_image() {
    for architecture in [LzxArchitecture::Lz32, LzxArchitecture::Lz64] {
        let image = build_init_shell_image(architecture).unwrap();
        assert_eq!(image.architecture(), architecture);
        assert_eq!(image.entry_section(), 0);
        assert_eq!(image.entry_offset(), 0);
        assert_eq!(image.required_stack(), USER_STACK_LENGTH);
        assert_eq!(image.sections().len(), 3);

        let code = &image.sections()[0];
        let data = &image.sections()[1];
        let bss = &image.sections()[2];
        assert_eq!(code.kind(), LzxSectionKind::Code);
        assert_eq!(data.kind(), LzxSectionKind::Data);
        assert_eq!(bss.kind(), LzxSectionKind::Bss);
        assert_eq!(code.virtual_offset(), 0);
        assert_eq!(code.virtual_size(), code.bytes().len() as u64);
        assert_eq!(data.virtual_offset(), NATIVE_SHELL_DATA_OFFSET);
        assert_eq!(data.virtual_size(), NATIVE_SHELL_DATA_LENGTH);
        assert_eq!(data.bytes().len(), NATIVE_SHELL_DATA_LENGTH as usize);
        assert_eq!(bss.virtual_offset(), NATIVE_SHELL_BSS_OFFSET);
        assert_eq!(bss.virtual_size(), NATIVE_SHELL_BSS_LENGTH);
        assert!(bss.bytes().is_empty());
        assert!(data.virtual_offset() + data.virtual_size() <= bss.virtual_offset());
        assert_eq!(image.required_data(), NATIVE_SHELL_REQUIRED_DATA);
        assert!(image.required_data() >= bss.virtual_offset() + bss.virtual_size());
        assert!(
            data.bytes()
                .windows(NATIVE_SHELL_PROMPT.len())
                .any(|window| { window == NATIVE_SHELL_PROMPT })
        );
        assert!(
            data.bytes()
                .windows(NATIVE_SHELL_HELP.len())
                .any(|window| { window == NATIVE_SHELL_HELP })
        );
        assert!(
            data.bytes()
                .windows(NATIVE_SHELL_CAT_PATH.len())
                .any(|window| window == NATIVE_SHELL_CAT_PATH)
        );
        assert!(
            data.bytes()
                .windows(NATIVE_SHELL_LS_PATH.len())
                .any(|window| window == NATIVE_SHELL_LS_PATH)
        );
        assert_canonical_code(architecture, code.bytes());

        let encoded = image.to_bytes().unwrap();
        let parsed = LzxImage::from_bytes(&encoded).unwrap();
        assert_eq!(parsed, image);
    }
}

#[test]
fn native_shell_process_loads_user_code_data_and_zeroed_bss() {
    const { assert!(!NATIVE_SHELL_PROCESS_LAUNCH_SUPPORTED) };
    assert!(!NATIVE_SHELL_RUN_DEFERRED.is_empty());
    for architecture in [LzxArchitecture::Lz32, LzxArchitecture::Lz64] {
        let image = build_init_shell_image(architecture).unwrap();
        let process = image
            .load_process(ProcessId::new(41).unwrap(), ThreadId::new(43).unwrap())
            .unwrap();
        assert_eq!(process.program().config(), architecture.config());
        assert_eq!(process.program().entry().as_u64(), USER_CODE_START);
        assert_eq!(
            process.primary_thread().cpu().sp().as_u64(),
            USER_INITIAL_SP
        );
        assert_eq!(process.program().bytes(), image.sections()[0].bytes());
        assert_eq!(process.primary_thread().cpu().privilege(), Privilege::User);
        let mut code = [0u8; 8];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(USER_CODE_START), &mut code)
            .unwrap();
        assert_eq!(code, image.sections()[0].bytes()[..8]);

        let mut prompt = [0u8; 16];
        process
            .memory()
            .address_space()
            .peek(
                PhysicalAddress::new(USER_DATA_START + NATIVE_SHELL_DATA_OFFSET),
                &mut prompt,
            )
            .unwrap();
        assert_eq!(&prompt[..NATIVE_SHELL_PROMPT.len()], NATIVE_SHELL_PROMPT);

        let mut bss = [0xffu8; 32];
        process
            .memory()
            .address_space()
            .peek(
                PhysicalAddress::new(USER_DATA_START + NATIVE_SHELL_BSS_OFFSET),
                &mut bss,
            )
            .unwrap();
        assert_eq!(bss, [0; 32]);

        let mut line = [0xffu8; 16];
        process
            .memory()
            .address_space()
            .peek(
                PhysicalAddress::new(USER_DATA_START + NATIVE_SHELL_LINE_BUFFER_OFFSET),
                &mut line,
            )
            .unwrap();
        assert_eq!(line, [0; 16]);
        const {
            assert!(NATIVE_SHELL_IO_RESULT_OFFSET >= NATIVE_SHELL_BSS_OFFSET);
            assert!(NATIVE_SHELL_DIRECTORY_OFFSET >= NATIVE_SHELL_BSS_OFFSET);
            assert!(NATIVE_SHELL_FILE_BUFFER_OFFSET >= NATIVE_SHELL_BSS_OFFSET);
        }
    }
}

#[test]
fn native_shell_fixture_executes_bounded_commands() {
    for architecture in [LzxArchitecture::Lz32, LzxArchitecture::Lz64] {
        let (output, _) = run_native_shell(architecture, b"help\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, NATIVE_SHELL_HELP].concat()));
        let (output, _) = run_native_shell(architecture, b"\nhelp\n");
        assert!(
            output
                .windows(NATIVE_SHELL_HELP.len())
                .any(|window| window == NATIVE_SHELL_HELP)
        );
        let (output, _) = run_native_shell(architecture, b"help\necho hi\nls\ncat\n");
        assert!(
            output
                .windows(NATIVE_SHELL_HELP.len())
                .any(|window| window == NATIVE_SHELL_HELP)
        );
        assert!(output.windows(3).any(|window| window == b"hi\n"));

        let (output, _) = run_native_shell(architecture, b"echo\n");
        assert_eq!(
            output,
            [NATIVE_SHELL_PROMPT, b"\n", NATIVE_SHELL_PROMPT].concat()
        );

        let (output, _) = run_native_shell(architecture, b"echo hello\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, b"hello\n"].concat()));

        let (output, _) = run_native_shell(architecture, b"ls\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, NATIVE_SHELL_LS_HEADER].concat()));
        assert!(output.len() > NATIVE_SHELL_PROMPT.len() + NATIVE_SHELL_LS_HEADER.len());
        let (output, _) = run_native_shell(architecture, b"ls\nls\n");
        assert!(
            output
                .windows(10)
                .filter(|window| *window == b"hello.txt\n")
                .count()
                >= 2
        );
        let (output, _) = run_native_shell(architecture, b"echo");
        assert!(output.windows(1).any(|window| window == b"\n"));

        let (output, _) = run_native_shell(architecture, b"cat\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, b"hello\n"].concat()));

        let (output, _) = run_native_shell(architecture, b"run\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, NATIVE_SHELL_RUN_DEFERRED].concat()));

        let (output, _) = run_native_shell(architecture, b"unknown\n");
        assert!(output.starts_with(&[NATIVE_SHELL_PROMPT, b"unknown command\n"].concat()));

        let (_, generation) = run_native_shell(architecture, b"clear\n");
        assert_eq!(generation, 1);
    }
}

#[test]
fn the_two_shells_declare_one_conformance_contract() {
    let architecture = LzxArchitecture::Lz64;
    for input in [&b"help\n"[..], b"echo hello\n", b"echo\n"] {
        let (guest, _) = run_native_shell(architecture, input);
        let mut host = HeadlessShell::new(VirtualFileSystem::with_defaults().unwrap());
        host.print_prompt().unwrap();
        host.execute_line(trim_newline(input)).unwrap();
        assert_eq!(
            guest.starts_with(NATIVE_SHELL_PROMPT),
            host.output().starts_with(NATIVE_SHELL_PROMPT),
            "both shells must emit the same prompt for {input:?}"
        );
    }

    let (guest, _) = run_native_shell(architecture, b"unknown\n");
    assert!(guest.starts_with(&[NATIVE_SHELL_PROMPT, b"unknown command\n"].concat()));
    let mut host = HeadlessShell::new(VirtualFileSystem::with_defaults().unwrap());
    assert!(matches!(
        host.execute_line(b"unknown"),
        Err(ShellError::UnknownCommand { command }) if command == b"unknown"
    ));

    let (guest, _) = run_native_shell(architecture, b"\n");
    assert!(
        guest.starts_with(&[NATIVE_SHELL_PROMPT, b"unknown command\n"].concat()),
        "a blank line is outside the in-image subset and is reported"
    );
    assert!(matches!(host.execute_line(b""), Err(ShellError::EmptyLine)));

    let (guest, _) = run_native_shell(architecture, b"ls /dir\n");
    assert!(
        guest
            .windows(b"unknown command".len())
            .any(|window| window == b"unknown command"),
        "the in-image shell is a documented subset: it takes no ls argument"
    );
    let mut host = HeadlessShell::new(VirtualFileSystem::with_defaults().unwrap());
    host.filesystem_mut().insert_directory(b"/dir").unwrap();
    assert!(matches!(
        host.execute_line(b"ls /dir"),
        Ok(ShellOutcome {
            command: ShellCommand::Ls,
            ..
        })
    ));

    let (guest, generation) = run_native_shell(architecture, b"clear\n");
    assert_eq!(generation, 1);
    let mut host = HeadlessShell::new(VirtualFileSystem::with_defaults().unwrap());
    host.print_prompt().unwrap();
    host.execute_line(b"clear").unwrap();
    assert_eq!(host.screen_generation(), 1);
    assert!(host.output().is_empty(), "clear resets the host transcript");
    let _ = guest;
}

fn trim_newline(input: &[u8]) -> &[u8] {
    match input.strip_suffix(b"\n") {
        Some(trimmed) => trimmed,
        None => input,
    }
}
