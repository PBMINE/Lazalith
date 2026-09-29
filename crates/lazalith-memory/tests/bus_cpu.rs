use lazalith_cpu::{ExecutionEngine, Processor, ReferenceInterpreter};
use lazalith_memory::{
    AddressSpace, Bus, DataAccessKind as K, DataSize as S, MemoryFaultKind as F, MemoryRegion,
    Privilege as U, RegionKind, RegionPermissions as RP, RegisterIndex,
};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress as I, PhysicalAddress as P, VirtualAddress as V,
};

fn bus(config: C) -> Bus {
    Bus::new(AddressSpace::new(config))
}

fn state(config: C, pc: u64, sp: u64) -> lazalith_cpu::ArchitecturalState {
    lazalith_cpu::ArchitecturalState::new(config, I::new(pc), V::new(sp), 0).unwrap()
}

fn reg(opcode: u8, d: u8, a: u8, b: u8) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0] = opcode;
    bytes[1] = d | (a << 4);
    bytes[2] = b;
    bytes
}

fn imm(opcode: u8, d: u8, value: i32) -> [u8; 8] {
    let mut bytes = reg(opcode, d, 0, 0);
    bytes[4..8].copy_from_slice(&value.to_le_bytes());
    bytes
}

fn mem(opcode: u8, d: u8, a: u8, size: u8, displacement: i32) -> [u8; 8] {
    let mut bytes = reg(opcode, d, a, 0);
    bytes[2] = size << 4;
    bytes[4..8].copy_from_slice(&displacement.to_le_bytes());
    bytes
}

#[test]
fn a_tiny_program_executes_through_the_bus_in_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config);
        bus.map(MemoryRegion::ram(config, P::new(0), 64, RP::new(true, true, true, true)).unwrap())
            .unwrap();
        let mut code = Vec::new();
        code.extend_from_slice(&imm(0x02, 1, 44));
        code.extend_from_slice(&imm(0x02, 2, 3));
        code.extend_from_slice(&reg(0x10, 3, 1, 2));
        code.extend_from_slice(&mem(0x32, 3, 1, 2, 4));
        code.extend_from_slice(&mem(0x30, 4, 1, 2, 4));
        code.extend_from_slice(&imm(0x52, 0, 0));
        bus.initialize(P::new(0), &code).unwrap();
        let mut cpu = Processor::new(state(config, 0, 48));
        let mut steps = 0;
        while cpu.execution() == lazalith_cpu::ExecutionState::Running {
            ReferenceInterpreter::new()
                .step(&mut cpu, &mut bus)
                .unwrap();
            steps += 1;
            assert!(steps <= 6, "cpu did not halt within six steps");
        }
        assert_eq!(steps, 6);
        assert_eq!(cpu.execution(), lazalith_cpu::ExecutionState::Halted);
        let registers = cpu.architectural().registers();
        assert_eq!(registers.read(RegisterIndex::try_from(3).unwrap()), 47);
        assert_eq!(registers.read(RegisterIndex::try_from(4).unwrap()), 47);
        let size = usize::from(config.word_bytes());
        let mut slot = [0; 8];
        bus.peek(P::new(48), &mut slot[..size]).unwrap();
        assert_eq!(&slot[..size], &47u64.to_le_bytes()[..size]);
        let mut beyond = [0; 8];
        bus.peek(P::new(56), &mut beyond).unwrap();
        assert_eq!(beyond, [0; 8]);
    }
}

#[test]
fn rom_immutability_and_stack_region_policy_hold_through_the_bus() {
    let config = C::lz64();
    let mut bus = bus(config);
    assert!(
        MemoryRegion::rom(
            config,
            P::new(0),
            &[1, 2, 3],
            RP::new(true, true, true, true)
        )
        .unwrap_err()
        .to_string()
        .contains("ROM constructor rejects writable")
    );
    bus.map(
        MemoryRegion::rom(
            config,
            P::new(0),
            &[1, 2, 3, 4, 5, 6, 7, 8],
            RP::new(true, false, true, true),
        )
        .unwrap(),
    )
    .unwrap();
    bus.map(MemoryRegion::ram(config, P::new(64), 32, RP::new(true, true, false, true)).unwrap())
        .unwrap();
    let mut bytes = [0; 8];
    bus.peek(P::new(0), &mut bytes).unwrap();
    assert_eq!(bytes, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(
        bus.data_access(V::new(0), 0, S::Word, K::Read, U::User)
            .is_ok()
    );
    let write = bus
        .data_access(V::new(0), 0, S::Word, K::Write, U::User)
        .unwrap();
    let error = bus.write_data(write, u64::MAX).unwrap_err();
    assert!(matches!(error.kind, F::Permission { .. }));
    let mut after = [0; 8];
    bus.peek(P::new(0), &mut after).unwrap();
    assert_eq!(after, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(bus.initialize(P::new(0), &[9]).is_err());
    assert_eq!(bus.address_space().regions()[0].kind(), RegionKind::Rom);
    let fetch = bus.fetch_instruction(config, I::new(0), U::User).unwrap();
    assert_eq!(fetch, [1, 2, 3, 4, 5, 6, 7, 8]);
    let stack_in_rom = bus
        .data_access(V::new(0), 0, S::Double, K::StackRead, U::User)
        .unwrap();
    let error = bus.peek_stack(stack_in_rom).unwrap_err();
    assert!(error.to_string().contains("requires RAM"));
    let stack_in_ram = bus
        .data_access(V::new(64), 0, S::Double, K::StackWrite, U::Supervisor)
        .unwrap();
    bus.write_data(stack_in_ram, 0x1234).unwrap();
    let mut slot = [0; 8];
    bus.peek(P::new(64), &mut slot).unwrap();
    assert_eq!(slot, 0x1234u64.to_le_bytes());
}
