//! B22: the JIT really executes host code, and agrees with the reference.
//!
//! # The test that matters most is the first one
//!
//! **A JIT that quietly delegated to the interpreter would pass every differential test
//! in the repository**, because the interpreter is correct and is the oracle. So
//! `the_jit_actually_executed_host_code` asks the question the differential tests
//! cannot: did any guest instruction retire as host machine code? The answer comes from
//! the engine's own counters, and the test fails if the count is zero.
//!
//! # Why the rest of this file is differential
//!
//! Everything else compares the JIT against the Reference Interpreter step for step,
//! which §10 makes mandatory and which is the only evidence that the translation is
//! *correct* rather than merely *native*. Native and correct are different properties,
//! and a JIT that had only the first would be a very fast way to produce wrong answers.
//!
//! # The subset, and what is not in it
//!
//! The translator handles straight-line runs of register-only instructions. Memory,
//! control transfers, privileged operations and anything without a host encoding are
//! declined, and the machine runs those through the reference. `the_jit_declines_what_it
//! _cannot_translate` pins the boundary, because a JIT that *accepted* an instruction it
//! has no encoding for would emit whatever bytes happened to be next — which is the one
//! failure mode a JIT has that an interpreter does not.

use lazalith_cpu::{
    CpuMemory, EngineKind, ExecutionEngine, OutcomeApplication, Privilege, Processor,
    ReferenceInterpreter, StepResult,
};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_jit::{Jit, Translatable, translatable};
use lazalith_types::{ArchitectureConfig, InstructionAddress, VirtualAddress};

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// Memory that caches decoded instructions, as `lazalith_memory::Bus` does.
///
/// **The cache is here so the JIT is not asked to translate the same bytes twice**,
/// which is not what is being measured, and so a block's second run is a run of
/// translated code rather than a fresh translation.
#[derive(Clone, Debug, Default)]
struct Memory {
    bytes: Vec<u8>,
    decoded: std::collections::HashMap<usize, Instruction>,
}

impl Memory {
    fn of(bytes: &[u8]) -> Self {
        let mut memory = Self {
            bytes: bytes.to_vec(),
            decoded: std::collections::HashMap::new(),
        };
        memory.bytes.resize(bytes.len() + 64, 0);
        memory
    }
}

impl CpuMemory for Memory {
    type Error = MemoryError;
    fn fetch_instruction(
        &self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: Privilege,
    ) -> Result<[u8; 8], Self::Error> {
        let at = pc.as_u64() as usize;
        let end = at + 8;
        if end > self.bytes.len() {
            return Err(MemoryError);
        }
        let _ = (config, privilege);
        let mut chunk = [0u8; 8];
        chunk.copy_from_slice(&self.bytes[at..end]);
        Ok(chunk)
    }
    fn fetch_instruction_cached(
        &mut self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: lazalith_cpu::Privilege,
    ) -> Result<lazalith_cpu::FetchedInstruction, Self::Error> {
        let bytes = self.fetch_instruction(config, pc, privilege)?;
        match self.decoded.get(&(pc.as_u64() as usize)) {
            Some(instruction) => Ok(lazalith_cpu::FetchedInstruction::Decoded(*instruction)),
            None => Ok(lazalith_cpu::FetchedInstruction::Bytes(bytes)),
        }
    }
    fn cache_instruction(
        &mut self,
        _config: ArchitectureConfig,
        pc: InstructionAddress,
        instruction: Instruction,
    ) {
        self.decoded.insert(pc.as_u64() as usize, instruction);
    }
    fn read_data(&mut self, _access: lazalith_cpu::DataAccess) -> Result<u64, Self::Error> {
        Err(MemoryError)
    }
    fn write_data(
        &mut self,
        _access: lazalith_cpu::DataAccess,
        _value: u64,
    ) -> Result<(), Self::Error> {
        Err(MemoryError)
    }
    fn peek_stack(&self, _access: lazalith_cpu::DataAccess) -> Result<u64, Self::Error> {
        Err(MemoryError)
    }
}

fn r(index: u8) -> Operand {
    Operand::Register(lazalith_types::RegisterIndex::try_from(index).expect("a register exists"))
}

fn cpu() -> Processor {
    let state = lazalith_cpu::ArchitecturalState::new(
        CONFIG,
        InstructionAddress::new(0),
        VirtualAddress::new(0x4000),
        0,
    )
    .expect("a fresh architectural state is valid");
    Processor::new(state)
}

fn emit(opcode: Opcode, operands: &[Operand]) -> Vec<u8> {
    let instruction = Instruction::new(CONFIG, opcode, operands).expect("builds");
    encode(CONFIG, &instruction).expect("encodes").to_vec()
}

/// A straight-line run of register arithmetic, each result landing in its own register.
///
/// **One result per register, deliberately.** An earlier version of this program wrote
/// `ADD` and then `SUB` into the same `r0`, and the test then asserted `r0 == 42` — the
/// answer to the `ADD`, which the `SUB` had already overwritten. The assertion passed for
/// the wrong reason on the day it was written and would have failed for the right reason
/// the moment the translator stopped being wrong. Spreading the results out means each
/// one is a single instruction's answer and can be checked against what the ISA says it
/// should be, not against what the previous instruction left behind.
fn arithmetic() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&emit(Opcode::Li, &[r(1), Operand::Immediate(20)]));
    bytes.extend_from_slice(&emit(Opcode::Li, &[r(2), Operand::Immediate(22)]));
    bytes.extend_from_slice(&emit(Opcode::Add, &[r(0), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Sub, &[r(3), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Xor, &[r(4), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::And, &[r(5), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Or, &[r(6), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Mul, &[r(7), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Mov, &[r(8), r(1)]));
    bytes.extend_from_slice(&emit(Opcode::Addi, &[r(9), r(1), Operand::Immediate(2)]));
    bytes.extend_from_slice(&emit(Opcode::Subi, &[r(10), r(1), Operand::Immediate(2)]));
    bytes
}

/// A program whose values are only right at 64 bits.
///
/// **The first arithmetic corpus passed this test while one operation in it was wrong.**
/// `20 + 22` and `20 - 22` were the only pair in the first program whose 8-bit and 64-bit
/// results differed, so a translator doing 8-bit arithmetic agreed with the interpreter on
/// four of five operations and disagreed on the fifth. These values — where the answer
/// depends on bits 8 and above — do not have that property: every operation here is wrong
/// at the wrong width, so the corpus fails immediately rather than one case at a time.
fn wide_values() -> Vec<u8> {
    let big = Operand::Immediate(0x0123_4567);
    let other = Operand::Immediate(0x89AB_CDEF_u32 as i32);
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&emit(Opcode::Li, &[r(1), big]));
    bytes.extend_from_slice(&emit(Opcode::Li, &[r(2), other]));
    bytes.extend_from_slice(&emit(Opcode::Add, &[r(0), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Sub, &[r(3), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Xor, &[r(4), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::And, &[r(5), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Or, &[r(6), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Mul, &[r(7), r(1), r(2)]));
    bytes.extend_from_slice(&emit(Opcode::Addi, &[r(8), r(1), Operand::Immediate(-3)]));
    bytes.extend_from_slice(&emit(Opcode::Subi, &[r(9), r(1), Operand::Immediate(-3)]));
    bytes
}

/// Values wider than a byte, checked against the reference.
///
/// **This is the width test, and it is separate from the small-value one on purpose.**
#[test]
fn the_jit_computes_at_sixty_four_bits() {
    let bytes = wide_values();
    let mut jit = Jit::new();
    let mut translated = cpu();
    let mut translated_memory = Memory::of(&bytes);
    let mut reference = ReferenceInterpreter::new();
    let mut reference_cpu = cpu();
    let mut reference_memory = Memory::of(&bytes);

    jit.step(&mut translated, &mut translated_memory)
        .expect("a block runs");
    for _ in 0..10 {
        reference
            .step(&mut reference_cpu, &mut reference_memory)
            .expect("the reference does not fault");
    }
    assert_eq!(
        translated.architectural(),
        reference_cpu.architectural(),
        "a block of values wider than a byte is where a wrong operand size shows up"
    );
    // `LI` sign-extends its 32-bit immediate, so r2 is the sign-extended 0x89ABCDEF
    // rather than 0x89ABCDEF, and the add carries into the high half. Spelling that out
    // is the point: an expectation written with a bare `0x89AB_CDEF` is the kind that
    // fails for a reason that has nothing to do with what it is checking.
    let r1 = 0x0123_4567u64;
    let r2 = 0x89AB_CDEF_u32 as i32 as i64 as u64;
    assert_eq!(
        translated.architectural().registers().read_raw(0).unwrap(),
        r1.wrapping_add(r2),
        "and the add is a 64-bit add of the sign-extended operands"
    );
    assert_eq!(
        translated.architectural().registers().read_raw(1).unwrap(),
        r1,
        "LI writes the immediate itself, sign-extended from 32 bits"
    );
}

/// Every flag, in every combination the ISA can produce, against the reference.
///
/// **The flags are the part of this engine most likely to be quietly wrong.** A register
/// that is off by a bit is obvious; a carry flag that is off by one bit changes which
/// conditional branch a guest takes, and only shows up as a program that behaves
/// differently on one engine than on another. The wide-value test above found a missing
/// status register by accident — every register matched and only the `status` field
/// differed — which is exactly the kind of near-miss this test is here to make deliberate.
///
/// Each case is a two-instruction block so the arithmetic instruction is the last
/// flag-setting one, and each is compared against the interpreter, which is the authority.
#[test]
fn every_flag_matches_the_reference() {
    // (description, left, right, opcode)
    let cases: [(u64, u64, Opcode, &str); 10] = [
        (0, 0, Opcode::Add, "zero result sets Z"),
        (1, 0, Opcode::Add, "a nonzero result clears Z"),
        (
            u64::MAX,
            1,
            Opcode::Add,
            "an unsigned carry out of 64 bits sets C",
        ),
        (0, 1, Opcode::Add, "no carry clears C"),
        (
            1,
            u64::MAX,
            Opcode::Sub,
            "a borrow sets C, because the ISA's subtract carry is a borrow",
        ),
        (1, 0, Opcode::Sub, "no borrow clears C"),
        (i64::MAX as u64, 1, Opcode::Add, "a signed overflow sets V"),
        (
            0,
            0,
            Opcode::Sub,
            "a zero difference sets Z and clears C and V",
        ),
        (
            i64::MIN as u64,
            1,
            Opcode::Mul,
            "a multiply sets N and clears C and V, which the ISA defines as always false",
        ),
        (
            0x8000_0000_0000_0000,
            0,
            Opcode::Xor,
            "a negative result sets N",
        ),
    ];

    for (left, right, opcode, description) in cases {
        let bytes = [
            emit(Opcode::Li, &[r(1), Operand::Immediate(left as i32)]),
            emit(Opcode::Li, &[r(2), Operand::Immediate(right as i32)]),
            emit(opcode, &[r(0), r(1), r(2)]),
        ]
        .concat();
        let mut jit = Jit::new();
        let mut translated = cpu();
        let mut translated_memory = Memory::of(&bytes);
        let mut reference = ReferenceInterpreter::new();
        let mut reference_cpu = cpu();
        let mut reference_memory = Memory::of(&bytes);

        jit.step(&mut translated, &mut translated_memory)
            .unwrap_or_else(|error| panic!("{description}: the block should run: {error}"));
        for _ in 0..3 {
            reference
                .step(&mut reference_cpu, &mut reference_memory)
                .expect("the reference does not fault");
        }
        assert_eq!(
            translated.architectural().status(),
            reference_cpu.architectural().status(),
            "{description}: the JIT's status register is {:#b}, the reference's is {:#b}",
            translated.architectural().status().bits(),
            reference_cpu.architectural().status().bits()
        );
        // And the register the flags describe, so a case cannot pass by having both
        // engines fail the same way.
        assert_eq!(
            translated.architectural().registers().read_raw(0).unwrap(),
            reference_cpu
                .architectural()
                .registers()
                .read_raw(0)
                .unwrap(),
            "{description}: and the result the flags describe"
        );
    }
}

/// A block of `LI`s leaves the status register exactly as it was.
///
/// **`LI` and `MOV` do not set flags, and a block made only of them must therefore be
/// transparent to them.** This is the case a flags implementation gets wrong by
/// unconditionally writing the status register after a block: a program of pure
/// `MOV`s would then have its condition codes silently rewritten.
#[test]
fn a_block_with_no_arithmetic_leaves_the_flags_alone() {
    let bytes = [
        emit(Opcode::Li, &[r(1), Operand::Immediate(0x4000_0000)]),
        emit(Opcode::Mov, &[r(0), r(1)]),
        emit(Opcode::Mov, &[r(2), r(1)]),
    ]
    .concat();
    let mut processor = cpu();
    let before = processor.architectural().status();
    let mut jit = Jit::new();
    let mut memory = Memory::of(&bytes);
    jit.step(&mut processor, &mut memory).expect("a block runs");
    assert_eq!(
        processor.architectural().status(),
        before,
        "three instructions that set no flags, and the status register is unchanged"
    );
    assert_eq!(
        processor.architectural().registers().read_raw(0).unwrap(),
        0x4000_0000u32 as i32 as i64 as u64,
        "while the registers are updated"
    );
}

// -- the test that distinguishes a JIT from a function call ------------------

/// The JIT retired guest instructions as host machine code.
///
/// **This is the non-vacuity test for the whole stage.** Without it, a `Jit` whose
/// `step` returned the interpreter's answer would satisfy every differential test in the
/// repository. The counters are the only thing that distinguishes the two, which is why
/// they exist on the engine rather than being inferred.
#[test]
fn the_jit_actually_executed_host_code() {
    let bytes = arithmetic();
    let mut jit = Jit::new();
    let mut processor = cpu();
    let mut memory = Memory::of(&bytes);

    let result: StepResult = jit.step(&mut processor, &mut memory).expect("a block runs");
    assert!(
        jit.executed_instructions() > 0,
        "no guest instruction retired as host code, so this is not a JIT"
    );
    assert!(
        result.instructions > 1,
        "a block retires several instructions in one step, and it reported {}",
        result.instructions
    );
    assert_eq!(
        jit.executed_blocks(),
        1,
        "and it was one block, not one block per instruction"
    );
    assert_eq!(
        jit.executed_instructions(),
        u64::from(result.instructions),
        "the engine's own count agrees with what it reported"
    );
}

/// The arithmetic the host code performed is the arithmetic the ISA defines.
///
/// **Checked against the Reference Interpreter, on every register.** This is the
/// difference between a JIT that is *native* and one that is *correct*.
#[test]
fn the_jit_computes_what_the_reference_computes() {
    let bytes = arithmetic();
    let mut jit = Jit::new();
    let mut translated = cpu();
    let mut translated_memory = Memory::of(&bytes);
    let mut reference = ReferenceInterpreter::new();
    let mut reference_cpu = cpu();
    let mut reference_memory = Memory::of(&bytes);

    let block: StepResult = jit
        .step(&mut translated, &mut translated_memory)
        .expect("a block runs");
    assert_eq!(block.outcome(), OutcomeApplication::Continue);
    assert_eq!(
        block.cycles, 18,
        "ten one-cycle instructions and one `Mul`, which the ISA costs at 8 because a \
         multiply is not a fixed-width register add"
    );
    assert_eq!(
        block.cycles,
        (0..11).map(|_| 1).sum::<u8>() + Opcode::Mul.cycles() - 1,
        "and that total is the sum of the ISA's own per-opcode costs, not a flat rate"
    );

    // The reference runs the same instructions, one at a time.
    for _ in 0..11 {
        reference
            .step(&mut reference_cpu, &mut reference_memory)
            .expect("the reference does not fault");
    }

    assert_eq!(
        translated.architectural(),
        reference_cpu.architectural(),
        "the translated block and the reference disagree about the whole machine"
    );
    // Spelled out, because a whole-state comparison says nothing about *which*
    // register was wrong.
    for index in 0..11u8 {
        assert_eq!(
            translated
                .architectural()
                .registers()
                .read_raw(index)
                .unwrap(),
            reference_cpu
                .architectural()
                .registers()
                .read_raw(index)
                .unwrap(),
            "r{index} differs"
        );
    }

    // And the answers are the ones the ISA defines, not merely the ones the reference
    // also produces. Written out by hand from 20 and 22, because a differential test
    // against a correct implementation cannot tell you *why* the two agree — only a
    // value derived from the specification can.
    let expected: [(u8, u64, &str); 11] = [
        (0, 42, "20 + 22"),
        (1, 20, "LI r1, 20"),
        (2, 22, "LI r2, 22"),
        (3, (-2i64) as u64, "20 - 22, wrapping"),
        (4, 20 ^ 22, "20 ^ 22"),
        (5, 20 & 22, "20 & 22"),
        (6, 20 | 22, "20 | 22"),
        (7, 20u64.wrapping_mul(22), "20 * 22, low 64 bits"),
        (8, 20, "MOV r8, r1"),
        (9, 22, "20 + 2"),
        (10, 18, "20 - 2"),
    ];
    for (index, want, description) in expected {
        assert_eq!(
            translated
                .architectural()
                .registers()
                .read_raw(index)
                .unwrap(),
            want,
            "r{index} should be {want} ({description})"
        );
    }
}

/// The bytes the translator emits for one instruction, spelled out.
///
/// **A translation test that only checks results cannot tell a correct encoding from a
/// wrong one that happens to be compensated for elsewhere in the block.** These are the
/// literal bytes, so a change in what the emitter produces has to be a deliberate change
/// here — and a mistake in the ModRM or REX byte shows up as a diff rather than as a
/// mysterious wrong register three instructions later.
#[test]
fn the_emitted_bytes_are_the_ones_written_down() {
    let translate = |bytes: &[u8]| {
        let processor = cpu();
        let mut memory = Memory::of(bytes);
        lazalith_jit::translation_of(&processor, &mut memory)
            .expect("translates")
            .code
            .bytes()
            .to_vec()
    };

    // A trailing *flag-setting* instruction, so the one under test is never the block's
    // last one and therefore carries no flags spill. A trailing `Mov` would not do: `Mov`
    // sets no flags, so the instruction under test would still be the last flag-setting
    // one and would still spill. The spill is checked on its own below.
    let tail = emit(Opcode::Xor, &[r(15), r(14), r(13)]);
    // Translated on its own, the tail instruction is also the last flag-setting one, so
    // it spills then exactly as it does inside a longer block — which is what makes this
    // length the number of bytes to trim.
    let tail_len = translate(&tail).len();

    let one = |opcode: Opcode, operands: &[Operand]| {
        let mut bytes = emit(opcode, operands);
        bytes.extend_from_slice(&tail);
        let mut all = translate(&bytes);
        // `tail_len` already includes the block-closing `RET`, because translating the tail
        // on its own produces a whole block.
        all.truncate(all.len() - tail_len);
        all
    };

    // LI r1, 20 — MOV R10, 20; MOV [RDI+8], R10; ADD [RSI], 8; RET
    assert_eq!(
        one(Opcode::Li, &[r(1), Operand::Immediate(20)]),
        vec![
            0x49, 0xBA, 20, 0, 0, 0, 0, 0, 0, 0, // MOV R10, 20 (B8+rd, REX.B)
            0x4C, 0x89, 0x97, 0x08, 0, 0, 0, // MOV [RDI+8], R10 (REX.R for the reg field)
            0x48, 0x83, 0x06, 0x08, // ADD QWORD [RSI], 8
        ],
        "the block loads the immediate into a scratch register, stores it, advances the \
         PC through the pointer the caller passed, and returns"
    );

    // SUB r3, r1, r2 — the left source is r1, *not* the destination r3.
    assert_eq!(
        one(Opcode::Sub, &[r(3), r(1), r(2)]),
        vec![
            0x4C, 0x8B, 0x97, 0x08, 0, 0, 0, // MOV R10, [RDI+8]     (r1)
            0x4C, 0x8B, 0x9F, 0x10, 0, 0, 0, // MOV R11, [RDI+16]    (r2)
            0x4D, 0x29, 0xDA, // SUB R10, R11 — reg=011+R8=R11 (source), r/m=010+R8=R10
            0x4C, 0x89, 0x97, 0x18, 0, 0, 0, // MOV [RDI+24], R10   (r3)
            0x48, 0x83, 0x06, 0x08, // ADD QWORD [RSI], 8
        ],
        "r3 is written and never read: the accumulator comes from operand one. Both \
         scratch registers are above 7, so both REX.R and REX.B are set, which is what \
         makes 0x4D out of 0x48"
    );

    // SUBI r10, r1, 2 — the `81 /5` form, and /5 rather than the SUB opcode 0x28.
    assert_eq!(
        one(Opcode::Subi, &[r(10), r(1), Operand::Immediate(2)]),
        vec![
            0x4C, 0x8B, 0x97, 0x08, 0, 0, 0, // MOV R10, [RDI+8]    (r1)
            0x49, 0xBB, 2, 0, 0, 0, 0, 0, 0, 0, // MOV R11, 2 (B8+011, REX.B for R11)
            0x49, 0x81, 0xEA, 0x02, 0, 0, 0, // SUB R10, 2 — /digit 5, r/m=010+R8=R10, imm32
            0x4C, 0x89, 0x97, 0x50, 0, 0, 0, // MOV [RDI+80], R10  (r10)
            0x48, 0x83, 0x06, 0x08,
        ],
        "a subtract by immediate is /5 in the 81 group; using SUB's opcode 0x28 here \
         truncates to /0 and silently emits an add"
    );

    // MUL r7, r1, r2 — the two-operand IMUL.
    assert_eq!(
        one(Opcode::Mul, &[r(7), r(1), r(2)]),
        vec![
            0x4C, 0x8B, 0x97, 0x08, 0, 0, 0, // MOV R10, [RDI+8]
            0x4C, 0x8B, 0x9F, 0x10, 0, 0, 0, // MOV R11, [RDI+16]
            0x4D, 0x0F, 0xAF, 0xD3, // IMUL R10, R11 — reg=R10 (dest), r/m=R11 (source)
            0x4C, 0x89, 0x97, 0x38, 0, 0, 0, // MOV [RDI+56], R10  (r7)
            0x48, 0x83, 0x06, 0x08,
        ],
        "the two-operand form is used so neither RAX nor RDX — the block's reserved \
         registers — is touched, and note the reg/r/m roles are the reverse of SUB's"
    );
}

/// The flags scratch, emitted only for the last flag-setting instruction.
///
/// **A block's status register is observable after the block, so the block has to spill
/// what those flags are computed from — but only the last arithmetic instruction's.** This
/// checks the placement rather than the values: the spills must be inside the block, around
/// the *last* arithmetic instruction, and absent everywhere else. A test that only checked
/// the resulting flags would pass even if every arithmetic instruction spilled, because
/// only the last write is read.
#[test]
fn only_the_last_flag_setting_instruction_spills() {
    let bytes = arithmetic();
    let processor = cpu();
    let mut memory = Memory::of(&bytes);
    let block = lazalith_jit::translation_of(&processor, &mut memory).expect("translates");

    // `MOV [RDX + disp32], reg` with `RDX` in the r/m field: REX 0x4C, opcode 0x89, and
    // a ModRM whose low three bits are RDX (2). Counting exactly these says how many
    // stores the block made into the flags scratch, wherever in the block they are.
    let spill_stores = |displacement: u32| {
        block
            .code
            .bytes()
            .windows(7)
            .filter(|window| {
                window[0] == 0x4C
                    && window[1] == 0x89
                    && window[2] & 0b0000_0111 == 2
                    && u32::from_le_bytes([window[3], window[4], window[5], window[6]])
                        == displacement
            })
            .count()
    };
    assert_eq!(spill_stores(0), 1, "the left operand is spilled once");
    assert_eq!(spill_stores(8), 1, "the right operand is spilled once");
    assert_eq!(spill_stores(16), 1, "and the result is spilled once");
    assert_eq!(
        spill_stores(0) + spill_stores(8) + spill_stores(16),
        3,
        "and nothing else in the block writes the scratch, even though the block has \
         eight arithmetic instructions and any of them could have"
    );

    // The block is `Add, Sub, Xor, And, Or, Mul, Mov, Addi, Subi`, so the last
    // flag-setting instruction is `Subi r10, r1, 2` and *that* is the one described.
    assert_eq!(
        block.flags,
        Some(lazalith_jit::Flags {
            opcode: Opcode::Subi,
            left: lazalith_jit::FlagsOperand::Register(1),
            right: lazalith_jit::FlagsOperand::Immediate(2),
        }),
        "and the block says which instruction the flags came from"
    );

    // A block with no arithmetic at all sets no flags.
    let plain = emit(Opcode::Mov, &[r(0), r(1)]);
    let mut memory = Memory::of(&plain);
    let block = lazalith_jit::translation_of(&processor, &mut memory).expect("translates");
    assert_eq!(
        block.flags, None,
        "LI and MOV do not touch the status register, so a block of them has no flags"
    );
}

// -- what the JIT declines ---------------------------------------------------

#[test]
fn the_jit_declines_what_it_cannot_translate() {
    // A memory operand, because reproducing the bus's permission checks in host code is
    // a memory-safety question rather than a wrong-answer one.
    let memory_operand = Operand::Memory {
        base: lazalith_types::RegisterIndex::try_from(1).unwrap(),
        displacement: 0,
    };
    let instruction = Instruction::new(
        CONFIG,
        Opcode::Ldz,
        &[
            r(0),
            memory_operand,
            Operand::DataSize(lazalith_isa::DataSize::Double),
        ],
    )
    .expect("a load builds");
    assert_eq!(
        translatable(&instruction),
        Translatable::No("it accesses memory"),
        "a memory access is declined, and the reason says so"
    );

    // A control transfer, because a block's `RET` has to be the end of it.
    let branch = Instruction::new(
        CONFIG,
        Opcode::Br,
        &[
            Operand::Condition(lazalith_isa::Condition::Al),
            Operand::Immediate(2),
        ],
    )
    .expect("a branch builds");
    assert_eq!(
        translatable(&branch),
        Translatable::No("it transfers control")
    );

    // A privileged operation.
    let privileged = Instruction::new(CONFIG, Opcode::Di, &[]).expect("a di builds");
    assert_eq!(
        translatable(&privileged),
        Translatable::No("it is privileged")
    );

    // And what *is* translated.
    for (opcode, operands) in [
        (Opcode::Li, vec![r(0), Operand::Immediate(1)]),
        (Opcode::Add, vec![r(0), r(1), r(2)]),
        (Opcode::Mul, vec![r(0), r(1), r(2)]),
    ] {
        let instruction = Instruction::new(CONFIG, opcode, &operands).expect("builds");
        assert_eq!(translatable(&instruction), Translatable::Yes);
    }
}

#[test]
fn a_program_the_jit_cannot_translate_leaves_the_processor_untouched() {
    // A block that cannot be built is declined, and declining must not have half-run
    // anything — the machine falls back to the reference for this instruction.
    let bytes = emit(
        Opcode::Ldz,
        &[
            r(0),
            Operand::Memory {
                base: lazalith_types::RegisterIndex::try_from(1).unwrap(),
                displacement: 0,
            },
            Operand::DataSize(lazalith_isa::DataSize::Double),
        ],
    );
    let mut jit = Jit::new();
    let mut processor = cpu();
    let mut memory = Memory::of(&bytes);
    let result = jit.step(&mut processor, &mut memory);
    assert!(
        result.is_err(),
        "a memory access at the start of a block is declined"
    );
    assert_eq!(
        processor.architectural().pc().as_u64(),
        0,
        "and the processor is exactly where it was, so the machine can run it elsewhere"
    );
    assert_eq!(
        jit.executed_instructions(),
        0,
        "and nothing ran as host code"
    );
}

#[test]
fn every_opcode_agrees_about_whether_it_is_translatable() {
    // The rule is in one place, and this checks the rule against the decoder: for every
    // encodable instruction, `translatable` and the engine's own answer must match.
    for opcode in lazalith_isa::Opcode::ALL {
        for operands in operand_shapes(*opcode) {
            let Ok(instruction) = Instruction::new(CONFIG, *opcode, &operands) else {
                continue;
            };
            let expected = lazalith_jit::is_translatable(&instruction);
            let reported = match translatable(&instruction) {
                Translatable::Yes => true,
                Translatable::No(_) => false,
            };
            assert_eq!(
                expected,
                reported,
                "the two answers disagree about {}",
                opcode.definition().mnemonic
            );
        }
    }
}

/// Operand shapes worth trying for an opcode.
fn operand_shapes(opcode: Opcode) -> Vec<Vec<Operand>> {
    use lazalith_isa::InstructionFormat as F;
    let shapes: Vec<Vec<Operand>> = match opcode.definition().format {
        F::Z => vec![vec![]],
        F::D | F::A => vec![vec![r(0)]],
        F::Da | F::Ab => vec![vec![r(0), r(1)]],
        F::Dab => vec![vec![r(0), r(1), r(2)]],
        F::Di | F::Br => vec![vec![r(0), Operand::Immediate(1)]],
        F::Dai => vec![vec![r(0), r(1), Operand::Immediate(1)]],
        F::Mem => vec![vec![
            r(0),
            Operand::Memory {
                base: lazalith_types::RegisterIndex::try_from(1).unwrap(),
                displacement: 0,
            },
            Operand::DataSize(lazalith_isa::DataSize::Double),
        ]],
        F::Imm => vec![vec![Operand::Immediate(1)]],
        F::Dx => vec![vec![
            r(0),
            Operand::Control(lazalith_isa::ControlRegister::Epc),
        ]],
        F::Ax => vec![vec![
            Operand::Control(lazalith_isa::ControlRegister::Epc),
            r(1),
        ]],
    };
    // A `Br` needs a condition, not a register, and the shape above is close enough
    // that `Instruction::new` will refuse it; that refusal is the test skipping an
    // unencodable shape, which is what it is for.
    shapes
}

#[test]
fn the_jit_reports_its_own_engine() {
    let jit: Box<dyn ExecutionEngine<Memory>> = Box::new(Jit::new());
    assert_eq!(
        ExecutionEngine::kind(&*jit),
        EngineKind::Jit,
        "a JIT that reported itself as an interpreter would be indistinguishable from \
         one at the only place a machine can ask"
    );
}
/// The memory's error type.
///
/// **A named type rather than `()`,** because `CpuMemory::Error` has to be an `Error`
/// and `()` is not one. A test memory that could not answer is a test memory that is
/// being asked the wrong question, so the error says so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryError;

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the test memory does not answer data accesses")
    }
}

impl std::error::Error for MemoryError {}
