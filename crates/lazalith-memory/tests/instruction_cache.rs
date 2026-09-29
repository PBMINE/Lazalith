//! The decode cache, and the claim that makes it safe.
//!
//! A cache is only worth having if it is indistinguishable from no cache. These
//! tests are that claim, taken in the ways it could fail:
//!
//! - the cache might not be *used*, so the "optimization" does nothing;
//! - it might disagree with memory after a program writes an instruction — the
//!   failure mode that executes a stale instruction, which is the worst one this
//!   platform has;
//! - it might make a fetch succeed that should have faulted, which is what the
//!   "checks before the lookup" test is for;
//! - it might change an observable outcome, which the rest of the suite already
//!   checks, since every machine in it runs through a caching bus and every
//!   expected value was written down before the cache existed.

use lazalith_cpu::{
    CpuMemory, DataAccess, DataAccessKind, ExecutionEngine, FetchedInstruction, Privilege,
    Processor, ReferenceInterpreter,
};
use lazalith_isa::{Instruction, Opcode, Operand, decode, encode};
use lazalith_memory::{
    AddressSpace, Bus, DataSize, InstructionCache, MemoryFaultKind, MemoryRegion,
    RegionPermissions as RP,
};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress as I, PhysicalAddress as P, RegisterIndex,
    VirtualAddress as V,
};

/// A bus with 256 bytes of readable, writable, executable RAM.
/// A caching bus, which is what every machine in the suite runs on.
fn bus(config: C) -> Bus {
    let mut bus = Bus::new(AddressSpace::new(config));
    bus.map(MemoryRegion::ram(config, P::new(0), 256, RP::new(true, true, true, true)).unwrap())
        .unwrap();
    bus
}

fn supervisor() -> Privilege {
    Privilege::Supervisor
}

fn write_byte(bus: &mut Bus, address: u64, value: u64, config: C) {
    let access = DataAccess::new(
        config,
        V::new(address),
        0,
        DataSize::Byte,
        DataAccessKind::Write,
        supervisor(),
    )
    .unwrap();
    bus.write_data(access, value).unwrap();
}

/// `li r1, 7`
fn li(register: u8, value: i32) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0] = 0x02;
    bytes[1] = register;
    bytes[4..8].copy_from_slice(&value.to_le_bytes());
    bytes
}

#[test]
fn a_second_fetch_of_the_same_address_is_served_from_the_cache() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config);
        bus.initialize(P::new(0), &li(1, 44)).unwrap();
        assert_eq!(
            bus.instruction_cache().map_or(0, InstructionCache::filled),
            0
        );
        // The first fetch misses, because nothing has been decoded yet.
        let first = bus
            .fetch_instruction_cached(config, I::new(0), supervisor())
            .unwrap();
        assert!(
            matches!(first, FetchedInstruction::Bytes(_)),
            "the first fetch of an address has nothing cached"
        );
        let bytes = match first {
            FetchedInstruction::Bytes(bytes) => bytes,
            FetchedInstruction::Decoded(_) => unreachable!(),
        };
        bus.cache_instruction(config, I::new(0), decode(config, &bytes).unwrap());
        // The second is answered without touching memory, and by walking the
        // program it is answered *without re-decoding*, which is the whole point.
        let second = bus
            .fetch_instruction_cached(config, I::new(0), supervisor())
            .unwrap();
        assert_eq!(
            second,
            FetchedInstruction::Decoded(decode(config, &bytes).unwrap())
        );
        assert_eq!(
            bus.instruction_cache().map_or(0, InstructionCache::filled),
            1
        );
    }
}

#[test]
fn a_warm_cache_does_not_rescue_a_fetch_that_should_have_faulted() {
    // The safety argument in one test: the checks run *before* the lookup, so a
    // warm entry cannot turn a fetch that should have faulted into one that did
    // not. An unaligned program counter is the check — a warm entry is planted
    // there on purpose, because a test that only ever warms valid addresses
    // proves nothing about this.
    let config = C::lz64();
    let mut bus = bus(config);
    let unaligned = I::new(1);
    bus.cache_instruction(config, unaligned, decode(config, &li(1, 1)).unwrap());
    let fetched = bus.fetch_instruction_cached(config, unaligned, supervisor());
    match fetched {
        Err(fault) => {
            assert!(
                matches!(fault.kind, MemoryFaultKind::Control(_)),
                "an unaligned fetch is a control fault, got {:?}",
                fault.kind
            );
        }
        Ok(_) => panic!("an unaligned fetch must fault even with a warm entry"),
    }
}

#[test]
fn a_warm_cache_does_not_hide_a_configuration_mismatch() {
    // The other check that runs before the lookup. A bus built for one width
    // asked about the other must say so, and a warm entry must not make it answer
    // with an instruction decoded under different rules.
    let mut bus = bus(C::lz64());
    bus.cache_instruction(C::lz64(), I::new(0), decode(C::lz64(), &li(1, 1)).unwrap());
    match bus.fetch_instruction_cached(C::lz32(), I::new(0), supervisor()) {
        Err(fault) => assert!(
            matches!(fault.kind, MemoryFaultKind::Configuration { .. }),
            "a width mismatch must be a configuration fault, got {:?}",
            fault.kind
        ),
        Ok(fetched) => {
            panic!("a width mismatch must be reported, not served from the cache: {fetched:?}")
        }
    }
}

#[test]
fn writing_an_instruction_makes_the_cached_copy_be_ignored() {
    // The failure mode this whole file exists to rule out: a program that stores
    // a new instruction over a decoded one and reaches it must execute the *new*
    // instruction. A cache that forgot to invalidate executes the old one, and the
    // program does something its author never wrote.
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config);
        // At 0: `li r1, 7`, and at 8: `li r2, 99`. The first is then overwritten
        // with `li r1, 0` — a different instruction, not a different encoding of
        // the same one, so a stale entry would be visible in a register.
        bus.initialize(P::new(0), &li(1, 7)).unwrap();
        bus.initialize(P::new(8), &li(2, 99)).unwrap();
        let mut cpu = Processor::new(
            lazalith_cpu::ArchitecturalState::new(config, I::new(0), V::new(200), 0).unwrap(),
        );
        ReferenceInterpreter::new()
            .step(&mut cpu, &mut bus)
            .unwrap();
        assert_eq!(
            cpu.architectural()
                .registers()
                .read(RegisterIndex::try_from(1).unwrap()),
            7
        );
        // The second instruction has been decoded by now, and cached.
        ReferenceInterpreter::new()
            .step(&mut cpu, &mut bus)
            .unwrap();
        assert_eq!(
            cpu.architectural()
                .registers()
                .read(RegisterIndex::try_from(2).unwrap()),
            99
        );
        let before = bus.instruction_cache().map_or(0, InstructionCache::filled);
        assert!(before >= 1, "a decoded instruction should be cached");
        for (index, byte) in li(1, 0).iter().enumerate() {
            write_byte(&mut bus, index as u64, u64::from(*byte), config);
        }
        assert!(
            bus.instruction_cache().map_or(0, InstructionCache::filled) < before,
            "a store into a cached instruction must forget it"
        );
        // And the program must now do the new thing.
        let mut cpu = Processor::new(
            lazalith_cpu::ArchitecturalState::new(config, I::new(0), V::new(200), 0).unwrap(),
        );
        ReferenceInterpreter::new()
            .step(&mut cpu, &mut bus)
            .unwrap();
        assert_eq!(
            cpu.architectural()
                .registers()
                .read(RegisterIndex::try_from(1).unwrap()),
            0,
            "the replaced instruction must not have run"
        );
    }
}

#[test]
fn a_store_that_lands_in_the_middle_of_an_instruction_forgets_it_too() {
    // An equality test would get this wrong. A store of one byte at offset 4 of an
    // eight-byte instruction changes that instruction, and the entry has to go.
    let config = C::lz64();
    let mut bus = bus(config);
    bus.initialize(P::new(0), &li(1, 7)).unwrap();
    bus.cache_instruction(config, I::new(0), decode(config, &li(1, 7)).unwrap());
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        1
    );
    write_byte(&mut bus, 4, 0, config);
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        0
    );
}

#[test]
fn a_store_that_does_not_touch_an_instruction_leaves_it_cached() {
    // The other half: invalidation that forgets everything is not a cache, it is a
    // tax. A store to a distant address must not cost the next fetch its hit.
    let config = C::lz64();
    let mut bus = bus(config);
    bus.initialize(P::new(0), &li(1, 7)).unwrap();
    bus.cache_instruction(config, I::new(0), decode(config, &li(1, 7)).unwrap());
    let access = DataAccess::new(
        config,
        V::new(128),
        0,
        DataSize::Word,
        DataAccessKind::Write,
        supervisor(),
    )
    .unwrap();
    bus.write_data(access, 1).unwrap();
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        1
    );
    assert_eq!(
        bus.fetch_instruction_cached(config, I::new(0), supervisor())
            .unwrap(),
        FetchedInstruction::Decoded(decode(config, &li(1, 7)).unwrap()),
        "a store elsewhere must leave the entry usable"
    );
}

#[test]
fn a_conflicting_address_replaces_the_entry_rather_than_probing() {
    // Direct-mapped, so a collision costs a decode and nothing else. What must
    // never happen is a collision returning the *other* address's instruction, so
    // this asserts the miss rather than the eviction policy.
    let config = C::lz64();
    let mut bus = bus(config);
    let conflicting = (4u64..4096)
        .map(I::new)
        .find(|address| ((address.as_u64() / 4) as usize) & 511 == 0)
        .expect("an address that shares the first slot");
    bus.initialize(P::new(0), &li(1, 7)).unwrap();
    bus.cache_instruction(config, I::new(0), decode(config, &li(1, 7)).unwrap());
    bus.cache_instruction(
        config,
        conflicting,
        Instruction::new(config, Opcode::Nop, &[]).unwrap(),
    );
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        1,
        "one slot, one entry"
    );
    assert_eq!(
        bus.fetch_instruction_cached(config, I::new(0), supervisor())
            .unwrap(),
        FetchedInstruction::Bytes(li(1, 7)),
        "the evicted address is a miss, not the other address's instruction"
    );
}

#[test]
fn a_cached_instruction_is_the_instruction_its_own_bytes_decode_to() {
    // The cache stores a decoded instruction and hands back a copy; this is that
    // the copy is what decoding the bytes under it would have produced, for three
    // different operand shapes, so no encoding can be cached wrongly.
    let config = C::lz64();
    let cases = [
        Instruction::new(config, Opcode::Nop, &[]).unwrap(),
        Instruction::new(
            config,
            Opcode::Li,
            &[
                Operand::Register(RegisterIndex::try_from(1).unwrap()),
                Operand::Immediate(42),
            ],
        )
        .unwrap(),
        Instruction::new(
            config,
            Opcode::Mov,
            &[
                Operand::Register(RegisterIndex::try_from(2).unwrap()),
                Operand::Register(RegisterIndex::try_from(3).unwrap()),
            ],
        )
        .unwrap(),
    ];
    for instruction in cases {
        let bytes = encode(config, &instruction).unwrap();
        let mut bus = bus(config);
        bus.initialize(P::new(0), &bytes).unwrap();
        bus.cache_instruction(config, I::new(0), instruction);
        let fetched = bus
            .fetch_instruction_cached(config, I::new(0), supervisor())
            .unwrap();
        assert_eq!(fetched, FetchedInstruction::Decoded(instruction));
        assert_eq!(
            decode(config, &bytes).unwrap(),
            instruction,
            "the entry and the bytes agree"
        );
    }
}

#[test]
fn the_cache_is_bounded_and_a_long_run_does_not_grow_it() {
    // A cache that could grow with the program would be a leak with a performance
    // motive attached. The table is fixed, so the same addresses are the same
    // entries however many times they are fetched.
    let config = C::lz64();
    let mut bus = bus(config);
    for index in 0..32u64 {
        bus.initialize(P::new(index * 8), &li(1, index as i32))
            .unwrap();
    }
    for _ in 0..62 {
        let mut cpu = Processor::new(
            lazalith_cpu::ArchitecturalState::new(config, I::new(0), V::new(200), 0).unwrap(),
        );
        for _ in 0..32 {
            ReferenceInterpreter::new()
                .step(&mut cpu, &mut bus)
                .unwrap();
        }
    }
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        32,
        "the same 32 addresses are 32 entries however many times they are fetched"
    );
}

/// Runs `program` on a bus, and returns every register plus the PC.
fn run(config: C, program: &[[u8; 8]], steps: usize, reference: bool) -> Vec<u64> {
    let mut bus = if reference {
        Bus::reference(AddressSpace::new(config))
    } else {
        Bus::new(AddressSpace::new(config))
    };
    bus.map(MemoryRegion::ram(config, P::new(0), 1024, RP::new(true, true, true, true)).unwrap())
        .unwrap();
    for (index, bytes) in program.iter().enumerate() {
        bus.initialize(P::new(index as u64 * 8), bytes).unwrap();
    }
    let mut cpu = Processor::new(
        lazalith_cpu::ArchitecturalState::new(config, I::new(0), V::new(896), 0).unwrap(),
    );
    let mut trace = Vec::new();
    for _ in 0..steps {
        if cpu.execution() != lazalith_cpu::ExecutionState::Running {
            break;
        }
        ReferenceInterpreter::new()
            .step(&mut cpu, &mut bus)
            .unwrap();
        trace.push(cpu.architectural().pc().as_u64());
        for index in 0..8u8 {
            trace.push(
                cpu.architectural()
                    .registers()
                    .read(RegisterIndex::try_from(index).unwrap()),
            );
        }
    }
    trace
}

#[test]
fn a_caching_bus_and_a_reference_bus_run_a_program_identically() {
    // The promise step 93 makes, as a test rather than a claim: every register and
    // the program counter, after every instruction, must be the same on both. A
    // time comparison could not catch an optimization that changed behaviour; this
    // can, because it compares everything the machine can be observed doing.
    for config in [C::lz32(), C::lz64()] {
        // A loop body: load, add, store, add, add — the shape a real program runs
        // a million times, and the shape a decode cache exists for.
        let program: [[u8; 8]; 6] = [
            li(1, 1),
            li(2, 0),
            li(3, 10),
            {
                // `add r1, r1, r2`
                let mut bytes = [0; 8];
                bytes[0] = 0x10;
                bytes[1] = 0x12;
                bytes
            },
            li(2, 2),
            {
                // `sub r3, r3, r1`
                let mut bytes = [0; 8];
                bytes[0] = 0x13;
                bytes[1] = 0x32;
                bytes
            },
        ];
        let cached = run(config, &program, 40, false);
        let reference = run(config, &program, 40, true);
        assert_eq!(
            cached, reference,
            "the cache changed what the machine did in {config:?}"
        );
        assert!(!cached.is_empty(), "the program should have run");
    }
}

#[test]
fn a_reference_bus_has_no_cache_at_all() {
    // Otherwise the comparison above is between two identical buses and proves
    // nothing, which is worse than having no test.
    let config = C::lz64();
    let reference = Bus::reference(AddressSpace::new(config));
    assert!(!reference.caches_instructions());
    assert!(reference.instruction_cache().is_none());
    let caching = Bus::new(AddressSpace::new(config));
    assert!(caching.caches_instructions());
}

#[test]
fn a_store_forgets_exactly_the_instructions_it_overlaps() {
    // The address walk rather than a table scan, checked by its *result*: if it
    // walked the wrong addresses it would either miss an overlapping entry — a
    // stale instruction, the bug this file exists to prevent — or clear a
    // neighbour it had no business clearing, which would turn the cache into a tax
    // on exactly the programs it should be helping.
    let config = C::lz64();
    let mut bus = bus(config);
    // Four instructions at 0, 8, 16, 24, all decoded and cached.
    for index in 0..4u64 {
        bus.initialize(P::new(index * 8), &li(1, index as i32))
            .unwrap();
        bus.cache_instruction(
            config,
            I::new(index * 8),
            decode(config, &li(1, index as i32)).unwrap(),
        );
    }
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        4
    );
    // A byte store inside the *second* instruction forgets that one and no other.
    write_byte(&mut bus, 12, 0, config);
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        3,
        "one overlapping instruction, one forgotten entry"
    );
    for address in [0u64, 16, 24] {
        // The immediate is the instruction's index, not its address: what is being
        // compared is *which entry survived*, not what the entry says.
        let expected = decode(config, &li(1, (address / 8) as i32)).unwrap();
        assert_eq!(
            bus.fetch_instruction_cached(config, I::new(address), supervisor())
                .unwrap(),
            FetchedInstruction::Decoded(expected),
            "the instruction at {address} was not touched by the store and must still be cached"
        );
    }
}

#[test]
fn a_store_spanning_two_instructions_forgets_both() {
    // An eight-byte store that starts on an instruction boundary covers exactly
    // that one; one that starts four bytes in covers two. Both are cases where a
    // naive "clear the entry at the store's own address" would get it wrong.
    let config = C::lz64();
    let mut bus = bus(config);
    for index in 0..2u64 {
        bus.initialize(P::new(index * 8), &li(1, index as i32))
            .unwrap();
        bus.cache_instruction(
            config,
            I::new(index * 8),
            decode(config, &li(1, index as i32)).unwrap(),
        );
    }
    for (index, byte) in li(2, 0).iter().enumerate() {
        write_byte(&mut bus, 4 + index as u64, u64::from(*byte), config);
    }
    assert_eq!(
        bus.instruction_cache().map_or(0, InstructionCache::filled),
        0,
        "a store starting four bytes into one instruction reaches into the next"
    );
}
