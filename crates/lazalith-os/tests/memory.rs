use lazalith_cpu::{
    ArchitecturalState, CpuFaultCause, ExecutionEngine, Processor, ReferenceInterpreter,
};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{DataSize, Instruction, Opcode, Operand, encode};
use lazalith_memory::{AddressSpace, Bus, RegionPermissions};
use lazalith_os::{
    BumpPool, KERNEL_HEAP_START, KERNEL_IMAGE_START, KERNEL_INITIAL_SP, KernelMemory, MemoryError,
    PHYSICAL_RAM_LENGTH, PHYSICAL_RAM_START, StackRegion, USER_CODE_START, USER_DATA_LENGTH,
    USER_DATA_START, USER_INITIAL_SP, UserMemory,
};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress, PhysicalAddress, VirtualAddress,
};

fn instruction(config: C, opcode: Opcode, operands: &[Operand]) -> Instruction {
    Instruction::new(config, opcode, operands).unwrap()
}

fn register(index: u8) -> Operand {
    Operand::Register(lazalith_types::RegisterIndex::try_from(index).unwrap())
}

#[test]
fn bump_pool_enforces_alignment_capacity_and_atomic_failure() {
    for config in [C::lz32(), C::lz64()] {
        assert!(matches!(
            BumpPool::new(config, PhysicalAddress::new(0x1000), 0, 8),
            Err(MemoryError::InvalidLayout)
        ));
        for alignment in [0, 3, 6] {
            assert!(matches!(
                BumpPool::new(config, PhysicalAddress::new(0x1000), 16, alignment),
                Err(MemoryError::InvalidAlignment { alignment: actual })
                    if actual == alignment
            ));
        }
        let mut pool = BumpPool::new(config, PhysicalAddress::new(0x1000), 32, 8).unwrap();
        let first = pool.allocate(1, 1).unwrap();
        assert_eq!(first.address().as_u64(), 0x1000);
        let second = pool.allocate(3, 4).unwrap();
        assert_eq!(second.address().as_u64(), 0x1004);
        let third = pool.allocate(8, 16).unwrap();
        assert_eq!(third.address().as_u64(), 0x1010);
        let before = pool.clone();
        assert!(matches!(pool.allocate(0, 1), Err(MemoryError::ZeroSize)));
        assert!(matches!(
            pool.allocate(4, 3),
            Err(MemoryError::InvalidAlignment { alignment: 3 })
        ));
        assert_eq!(pool, before);
        assert!(matches!(
            pool.allocate(9, 1),
            Err(MemoryError::Exhausted {
                requested: 9,
                remaining: 8
            })
        ));
        assert_eq!(pool, before);
    }

    let mut bounded = BumpPool::new(
        C::lz64(),
        PhysicalAddress::new(USER_DATA_START),
        USER_DATA_LENGTH,
        8,
    )
    .unwrap();
    assert!(matches!(
        bounded.allocate(1, 0x0040_0000),
        Err(MemoryError::Exhausted { .. })
    ));
    let mut wide = BumpPool::new(
        C::lz64(),
        PhysicalAddress::new(USER_DATA_START),
        USER_DATA_LENGTH * 2,
        8,
    )
    .unwrap();
    let aligned = wide.allocate(1, 0x0040_0000).unwrap();
    assert_eq!(aligned.address().as_u64() % 0x0040_0000, 0);

    let mut pool = BumpPool::new(C::lz32(), PhysicalAddress::new(0xffff_fff0), 16, 1).unwrap();
    let block = pool.allocate(16, 1).unwrap();
    assert_eq!(block.address().as_u64(), 0xffff_fff0);
    assert_eq!(block.end_exclusive(), 0x1_0000_0000);
    assert_eq!(pool.cursor(), None);
    assert_eq!(pool.remaining(), 0);
    assert!(matches!(
        pool.allocate(1, 1),
        Err(MemoryError::Exhausted {
            requested: 1,
            remaining: 0
        })
    ));
}

#[test]
fn kernel_memory_owns_checked_heap_and_stack_ranges() {
    for config in [C::lz32(), C::lz64()] {
        let mut memory = KernelMemory::new(config).unwrap();
        assert_eq!(memory.heap().start().as_u64(), KERNEL_HEAP_START);
        assert_eq!(memory.stack().initial_sp().as_u64(), 0x0018_f000);
        let first = memory.allocate_default(8).unwrap();
        assert_eq!(first.address().as_u64(), KERNEL_HEAP_START);
        assert_eq!(first.length(), 8);
        let before = memory.heap().clone();
        assert!(memory.allocate(usize::MAX as u64, 8).is_err());
        assert_eq!(memory.heap(), &before);
    }

    assert!(matches!(
        StackRegion::new(C::lz64(), 0x1000, 0x100, 0x1001),
        Err(MemoryError::InvalidLayout)
    ));
    assert!(matches!(
        StackRegion::new(C::lz64(), 0x1000, 0x100, 0x1100),
        Err(MemoryError::InvalidLayout)
    ));
    assert!(matches!(
        StackRegion::new(C::lz32(), 0xffff_fff0, 0x10, 0x1_0000_0000),
        Err(MemoryError::InvalidLayout)
    ));
    assert!(matches!(
        StackRegion::new(C::lz32(), 0xffff_fff0, 0x20, 0xffff_fff0),
        Err(MemoryError::InvalidLayout)
    ));
}

#[test]
fn user_memory_builds_disjoint_executable_data_and_stack_regions() {
    for config in [C::lz32(), C::lz64()] {
        let mut memory = UserMemory::new(config).unwrap();
        let regions = memory.address_space().regions();
        assert_eq!(regions.len(), 3);
        assert_eq!(regions[0].start().as_u64(), USER_CODE_START);
        assert_eq!(regions[0].length(), 0x0010_0000);
        assert_eq!(
            regions[0].permissions(),
            RegionPermissions::new(true, false, true, true)
        );
        assert_eq!(regions[1].start().as_u64(), USER_DATA_START);
        assert_eq!(
            regions[1].permissions(),
            RegionPermissions::new(true, true, false, true)
        );
        assert_eq!(regions[2].start().as_u64(), 0x0040_0000);
        assert_eq!(
            memory.layout().stack().initial_sp(),
            VirtualAddress::new(USER_INITIAL_SP)
        );

        let allocation = memory.allocate(24, 16).unwrap();
        assert_eq!(allocation.address().as_u64(), USER_DATA_START);
        assert_eq!(allocation.length(), 24);
        let before = memory.layout().heap().clone();
        memory.load_code(0, &[0xaa; 8]).unwrap();
        memory.load_code(0x0010_0000 - 8, &[0xbb; 8]).unwrap();
        let mut first_code = [0u8; 8];
        memory
            .address_space()
            .peek(PhysicalAddress::new(USER_CODE_START), &mut first_code)
            .unwrap();
        assert_eq!(first_code, [0xaa; 8]);
        assert!(memory.load_code(0x0010_0000 - 4, &[0; 8]).is_err());
        let mut after = [0u8; 8];
        memory
            .address_space()
            .peek(PhysicalAddress::new(USER_CODE_START), &mut after)
            .unwrap();
        assert_eq!(after, first_code);
        let mut tail = [0u8; 8];
        memory
            .address_space()
            .peek(
                PhysicalAddress::new(USER_CODE_START + 0x0010_0000 - 8),
                &mut tail,
            )
            .unwrap();
        assert_eq!(tail, [0xbb; 8]);
        assert_eq!(memory.layout().heap(), &before);
    }
}

#[test]
fn user_code_heap_and_stack_execute_through_real_cpu_memory() {
    for config in [C::lz32(), C::lz64()] {
        let mut memory = UserMemory::new(config).unwrap();
        let call = instruction(config, Opcode::Call, &[Operand::Immediate(2)]);
        let ret = instruction(config, Opcode::Ret, &[]);
        let mut code = encode(config, &call).unwrap().to_vec();
        code.resize(16, 0);
        code.extend_from_slice(&encode(config, &ret).unwrap());
        memory.load_code(0, &code).unwrap();
        let allocation = memory.allocate(64, 8).unwrap();
        let (layout, space) = memory.into_parts();
        let mut bus = Bus::with_devices(space, DeviceManager::<NoDevice>::new());

        let mut engine = ReferenceInterpreter::new();
        let mut cpu = Processor::new(
            ArchitecturalState::new(
                config,
                InstructionAddress::new(USER_CODE_START),
                layout.stack().initial_sp(),
                0x20,
            )
            .unwrap(),
        );
        engine.step(&mut cpu, &mut bus).unwrap();
        assert_eq!(cpu.architectural().pc().as_u64(), USER_CODE_START + 16);
        assert_eq!(
            cpu.architectural().sp().as_u64(),
            USER_INITIAL_SP - u64::from(config.word_bytes())
        );
        engine.step(&mut cpu, &mut bus).unwrap();
        assert_eq!(cpu.architectural().pc().as_u64(), USER_CODE_START + 8);
        assert_eq!(cpu.architectural().sp().as_u64(), USER_INITIAL_SP);

        let store_data = instruction(
            config,
            Opcode::Li,
            &[
                register(0),
                Operand::Immediate(allocation.address().as_u64() as i32),
            ],
        );
        let store_value = instruction(
            config,
            Opcode::Li,
            &[register(1), Operand::Immediate(0x1234)],
        );
        let store = instruction(
            config,
            Opcode::St,
            &[
                register(1),
                Operand::Memory {
                    base: lazalith_types::RegisterIndex::try_from(0).unwrap(),
                    displacement: 0,
                },
                Operand::DataSize(DataSize::Word),
            ],
        );
        let load = instruction(
            config,
            Opcode::Ldz,
            &[
                register(2),
                Operand::Memory {
                    base: lazalith_types::RegisterIndex::try_from(0).unwrap(),
                    displacement: 0,
                },
                Operand::DataSize(DataSize::Word),
            ],
        );
        for item in [&store_data, &store_value, &store, &load] {
            engine.execute(&mut cpu, item, &mut bus).unwrap();
        }
        assert_eq!(cpu.architectural().registers().read_raw(2).unwrap(), 0x1234);
    }
}

#[test]
fn complete_layout_is_backed_and_kernel_code_heap_and_stack_run_through_bus() {
    for config in [C::lz32(), C::lz64()] {
        assert_eq!(PHYSICAL_RAM_START + PHYSICAL_RAM_LENGTH - 1, 0x0040_ffff);
        let mut kernel = KernelMemory::new(config).unwrap();
        let mut regions = KernelMemory::regions(config).unwrap();
        regions.extend(UserMemory::regions(config).unwrap());
        assert_eq!(regions.len(), 6);
        let mut space = AddressSpace::new(config);
        for region in regions {
            space.map(region).unwrap();
        }
        let mut heap = [0u8; 16];
        space
            .peek(PhysicalAddress::new(KERNEL_HEAP_START), &mut heap)
            .unwrap();
        assert_eq!(heap, [0u8; 16]);
        let allocation = kernel.allocate_default(64).unwrap();
        assert_eq!(allocation.address().as_u64(), KERNEL_HEAP_START);

        let call = instruction(config, Opcode::Call, &[Operand::Immediate(2)]);
        let ret = instruction(config, Opcode::Ret, &[]);
        let mut code = encode(config, &call).unwrap().to_vec();
        code.resize(16, 0);
        code.extend_from_slice(&encode(config, &ret).unwrap());
        let items = [
            instruction(
                config,
                Opcode::Li,
                &[
                    register(0),
                    Operand::Immediate(allocation.address().as_u64() as i32),
                ],
            ),
            instruction(
                config,
                Opcode::Li,
                &[register(1), Operand::Immediate(0x1234)],
            ),
            instruction(
                config,
                Opcode::St,
                &[
                    register(1),
                    Operand::Memory {
                        base: lazalith_types::RegisterIndex::try_from(0).unwrap(),
                        displacement: 0,
                    },
                    Operand::DataSize(DataSize::Word),
                ],
            ),
            instruction(
                config,
                Opcode::Ldz,
                &[
                    register(2),
                    Operand::Memory {
                        base: lazalith_types::RegisterIndex::try_from(0).unwrap(),
                        displacement: 0,
                    },
                    Operand::DataSize(DataSize::Word),
                ],
            ),
        ];
        for item in &items {
            code.extend_from_slice(&encode(config, item).unwrap());
        }
        space
            .initialize(PhysicalAddress::new(KERNEL_IMAGE_START), &code)
            .unwrap();
        let mut bus = Bus::with_devices(space, DeviceManager::<NoDevice>::new());
        let mut engine = ReferenceInterpreter::new();
        let mut cpu = Processor::new(
            ArchitecturalState::new(
                config,
                InstructionAddress::new(KERNEL_IMAGE_START),
                kernel.stack().initial_sp(),
                0,
            )
            .unwrap(),
        );
        engine.step(&mut cpu, &mut bus).unwrap();
        engine.step(&mut cpu, &mut bus).unwrap();
        for item in &items {
            engine.execute(&mut cpu, item, &mut bus).unwrap();
        }
        assert_eq!(cpu.architectural().registers().read_raw(2).unwrap(), 0x1234);
        assert_eq!(cpu.architectural().sp().as_u64(), KERNEL_INITIAL_SP);
    }
}

#[test]
fn user_stack_is_not_executable_in_either_mode() {
    for config in [C::lz32(), C::lz64()] {
        let memory = UserMemory::new(config).unwrap();
        let (_, space) = memory.into_parts();
        let mut bus = Bus::with_devices(space, DeviceManager::<NoDevice>::new());
        let mut engine = ReferenceInterpreter::new();
        let mut cpu = Processor::new(
            ArchitecturalState::new(
                config,
                InstructionAddress::new(USER_INITIAL_SP),
                VirtualAddress::new(USER_INITIAL_SP),
                0x20,
            )
            .unwrap(),
        );
        let error = engine
            .step(&mut cpu, &mut bus)
            .unwrap_err()
            .into_guest()
            .expect("the reference interpreter never declines");
        assert!(matches!(
            error.cause,
            CpuFaultCause::Fetch(lazalith_memory::MemoryFault {
                kind: lazalith_memory::MemoryFaultKind::Permission { .. },
                ..
            })
        ));
    }
}

#[test]
fn kernel_consumes_the_shared_os_abi_without_copying_definitions() {
    assert_eq!(lazalith_os::abi::ABI_VERSION, 1);
    assert_eq!(
        lazalith_os::abi::Syscall::try_from(0x000e),
        Ok(lazalith_os::abi::Syscall::ClearScreen)
    );
    assert_eq!(
        lazalith_os::abi::TaggedOutcome::failure(
            lazalith_os::abi::SyscallError::InvalidArgument,
            3
        )
        .registers(),
        [2, 3]
    );
}

#[test]
fn bump_pool_reports_alignment_aware_remaining_capacity() {
    let mut pool = BumpPool::new(C::lz64(), PhysicalAddress::new(0x1000), 0x20, 8).unwrap();
    pool.allocate(1, 1).unwrap();
    assert_eq!(pool.remaining(), 0x1f);
    pool.allocate(3, 1).unwrap();
    assert_eq!(pool.remaining(), 0x1c);
    let mut padded = BumpPool::new(C::lz64(), PhysicalAddress::new(0x1000), 0x10, 1).unwrap();
    padded.allocate(4, 1).unwrap();
    let before = padded.clone();
    assert!(
        matches!(
            padded.allocate(1, 0x10),
            Err(MemoryError::Exhausted { remaining: 0, .. })
        ),
        "alignment padding must not be reported as usable capacity"
    );
    assert_eq!(padded, before);
    let tail = pool.allocate(0x1c, 1).unwrap();
    assert_eq!(tail.address().as_u64(), 0x1004);
    assert_eq!(pool.remaining(), 0);
    assert!(matches!(
        pool.allocate(1, 1),
        Err(MemoryError::Exhausted { remaining: 0, .. })
    ));
}
