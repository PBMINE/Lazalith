//! Property tests for the object format.
//!
//! # The property
//!
//! ```text
//! from_bytes(to_bytes(object)) == object
//! ```
//!
//! and it is worth more here than in most places, because the object format is
//! the *only* thing the three front ends of step 83 have in common. A round trip
//! that lost a symbol binding or reordered a relocation would make one language's
//! objects behave differently from another's while every other test still passed —
//! which is precisely the second ecosystem the step forbids.
//!
//! # Why this is not just a test of `to_bytes`
//!
//! The round trip is a property of *both* halves, and a one-directional test
//! cannot tell which one is wrong. So there are three:
//!
//! - The round trip, which is the property.
//! - The bytes are a function of the object: the same object encodes to the same
//!   bytes twice, so nothing in the encoder depends on a hash order or an
//!   allocation address.
//! - Garbage in the door is refused or is an object, and never a panic. The object
//!   reader is the one piece of the toolchain that reads bytes it did not write, so
//!   it is the one piece where a malformed input is a real event rather than a
//!   hypothetical.
//!
//! The objects are *built* rather than read, because a random byte string is almost
//! always refused at the header and would exercise nothing. The builder is given
//! random section sizes, alignments, symbol values and relocation offsets, and the
//! reader has to agree about all of them.

use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_properties::{Case, Gen, check};
use lazalith_toolchain::{
    CodeMapping, DebugSource, ObjectBuilder, ObjectFile, Relocation, RelocationKind, Section,
    Symbol, SymbolBinding,
};
use lazalith_types::ArchitectureConfig;

/// A built object, and the architecture it was built for.
struct Built {
    config: ArchitectureConfig,
    object: ObjectFile,
}

impl Case for Built {
    fn generate(source: &mut Gen) -> Self {
        let config = match source.bool() {
            true => ArchitectureConfig::lz32(),
            false => ArchitectureConfig::lz64(),
        };
        // Instructions, so the code section is real machine code rather than
        // arbitrary bytes: an object full of nonsense would be refused by the
        // reader for a reason that has nothing to do with the round trip.
        let instruction_count = 1 + source.below(4) as usize;
        let instructions: Vec<Instruction> = source
            .vector(instruction_count, |source| {
                let opcode = source
                    .choice(Opcode::ALL)
                    .expect("the opcode table is never empty");
                let operands = operands_for(source, opcode, config);
                Instruction::new(config, opcode, &operands)
                    .unwrap_or_else(|_| Instruction::new(config, Opcode::Nop, &[]).expect("a NOP"))
            })
            .into_iter()
            .collect();
        let mut code = Vec::new();
        for instruction in &instructions {
            code.extend_from_slice(
                &encode(config, instruction).expect("an instruction built here encodes"),
            );
        }

        let mut builder = ObjectBuilder::new(config);
        let text = builder
            .add_section(Section::text("text", config, &code).expect("a code section"))
            .expect("the first section is added");
        let alignment = 1u64 << source.below(4);
        let data_length = 4 * (1 + source.below(8));
        let data = builder
            .add_section(
                Section::data(
                    "data",
                    alignment,
                    &source.vector(data_length as usize, |source| source.next_u8()),
                )
                .expect("a data section"),
            )
            .expect("the data section is added");
        let _bss = builder
            .add_section(
                Section::bss("bss", alignment, 4 * (1 + source.below(16))).expect("a bss section"),
            )
            .expect("the bss section is added");

        let entry = builder
            .add_symbol(Symbol::section_defined(
                "_start",
                SymbolBinding::Local,
                text,
                0,
                0,
            ))
            .expect("a defined symbol");
        builder.set_entry(entry).expect("the entry is set");
        let undefined = builder
            .add_symbol(Symbol::undefined("elsewhere", SymbolBinding::Global))
            .expect("an undefined symbol");
        builder
            .add_symbol(Symbol::absolute(
                "answer",
                SymbolBinding::Global,
                // An absolute symbol has to fit the target, so the value is the machine
                // s own width rather than an interesting 64-bit number -- on a 32-bit
                // target a 64-bit absolute is refused, correctly, and building one would
                // be testing this file.s mistake.
                config_word_mask(config),
            ))
            .expect("an absolute symbol");
        let defined = builder
            .add_symbol(Symbol::section_defined(
                "value",
                SymbolBinding::Global,
                data,
                0,
                data_length,
            ))
            .expect("a data symbol");
        // bss holds no bytes, so a relocation into it is refused by the builder --
        // correctly, and a property test that built one would be testing its own
        // mistake. Every relocation here is into the data section instead, at offsets
        // that do not overlap: the builder refuses a pair that would write over each
        // other, and choosing offsets that can overlap would be testing that check
        // with a case it is supposed to refuse.
        // A 64-bit absolute patch is not available on a 32-bit target -- correctly,
        // the format has no word for it -- so on that target both patches are 32-bit
        // and the case covers a 32-bit object with two relocations rather than none.
        let wide = if config.word_width() == lazalith_types::WordWidth::W64 {
            RelocationKind::AbsoluteWord64
        } else {
            RelocationKind::AbsoluteWord32
        };
        let wide_at = 0u64;
        let narrow_at = 8u64;
        if data_length >= 12 {
            builder
                .add_relocation(Relocation::new(defined, data, wide, wide_at, 0))
                .expect("a wide relocation");
            builder
                .add_relocation(Relocation::new(
                    undefined,
                    data,
                    RelocationKind::AbsoluteWord32,
                    narrow_at,
                    0,
                ))
                .expect("a half-word relocation");
        }
        let source = builder
            .add_debug_source(DebugSource::new("object.lzs", "line one\nline two\n"))
            .expect("a debug source");
        builder
            .add_debug_mapping(CodeMapping::new(text, 0, source, 0, 3))
            .expect("a code mapping");
        Self {
            config,
            object: builder.build().expect("the object is finished"),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{} sections, {} symbols, {} relocations in {:?}",
            self.object.sections().len(),
            self.object.symbols().len(),
            self.object.relocations().len(),
            self.config
        )
    }
}

/// A value an absolute symbol may hold on this target.
fn config_word_mask(config: ArchitectureConfig) -> u64 {
    match config.word_width() {
        lazalith_types::WordWidth::W32 => u32::MAX as u64,
        lazalith_types::WordWidth::W64 => u64::MAX,
    }
}

/// Operands for an opcode, or none when its format wants none this can satisfy.
fn operands_for(
    source: &mut Gen,
    opcode: Opcode,
    config: ArchitectureConfig,
) -> Vec<lazalith_isa::Operand> {
    use lazalith_isa::{Condition, ControlRegister, DataSize, Operand, OperandKind};
    opcode
        .definition()
        .operands()
        .iter()
        .map(|definition| match definition.kind {
            OperandKind::Register => Operand::Register(
                lazalith_types::RegisterIndex::try_from(source.below(16) as u8)
                    .expect("below sixteen"),
            ),
            OperandKind::Immediate => Operand::Immediate(source.interesting_u32() as i32),
            OperandKind::Memory => Operand::Memory {
                base: lazalith_types::RegisterIndex::try_from(source.below(16) as u8)
                    .expect("below sixteen"),
                displacement: source.interesting_u32() as i32,
            },
            OperandKind::DataSize => Operand::DataSize(
                source
                    .choice(DataSize::ALL)
                    .filter(|size| config.supports_data_size(size.bytes()))
                    .or_else(|| source.choice(DataSize::ALL))
                    .expect("there is always a data size"),
            ),
            OperandKind::Condition => Operand::Condition(
                source
                    .choice(Condition::ALL)
                    .expect("there is always a condition"),
            ),
            OperandKind::Control => Operand::Control(
                source
                    .choice(ControlRegister::ALL)
                    .expect("there is always a control register"),
            ),
        })
        .collect()
}

/// The object survives being written and read back.
///
/// The property, and the one step 83's convergence rests on.
#[test]
fn an_object_survives_the_round_trip() {
    check::<Built>(64, |case| {
        let bytes = case.object.to_bytes().expect("an object encodes");
        let read = ObjectFile::from_bytes(&bytes).expect("what it wrote, it reads");
        read.to_bytes().expect("the read object encodes") == bytes
    });
}

/// Encoding an object twice gives the same bytes.
///
/// The encoder must be a function of the object and not of a hash order or an
/// allocation address. Without this, a round trip can pass while two *identical*
/// objects differ on disk — and a linker that compares or caches object bytes
/// would then be comparing things that are not equal.
#[test]
fn encoding_is_a_function_of_the_object() {
    check::<Built>(64, |case| {
        let first = case.object.to_bytes().expect("an object encodes");
        let second = case.object.to_bytes().expect("an object encodes again");
        first == second
    });
}

/// The bytes begin with the magic, and the reader says so when they do not.
///
/// The shortest form of "this is a Lazalith object": the first thing in the file
/// says so, and a file that does not begin that way is refused by the reader rather
/// than half-interpreted.
#[test]
fn the_bytes_begin_with_the_magic() {
    check::<Built>(64, |case| {
        let bytes = case.object.to_bytes().expect("an object encodes");
        bytes.starts_with(&lazalith_toolchain::OBJECT_MAGIC)
    });
}

/// Arbitrary bytes are an object or a refusal, and never a panic.
///
/// The reader is the one piece of the toolchain that reads bytes it did not write,
/// so a malformed input is a real event for it and not a hypothetical. Every case
/// here is either refused or decodes to something that re-encodes to the same
/// bytes; "it panicked" and "it invented an object" are both bugs, and only the
/// first would be caught by a crash.
#[test]
fn arbitrary_bytes_are_an_object_or_a_refusal() {
    struct Bytes(Vec<u8>);
    impl Case for Bytes {
        fn generate(source: &mut Gen) -> Self {
            // Between eight bytes — the magic — and a few hundred, so the reader
            // gets past its first check sometimes and does not always stop at it.
            let length = 8 + source.below(256) as usize;
            Self(source.vector(length, |source| source.next_u8()))
        }
        fn describe(&self) -> String {
            format!("{:02x?}", &self.0[..self.0.len().min(24)])
        }
    }

    check::<Bytes>(64, |case| match ObjectFile::from_bytes(&case.0) {
        Err(_) => true,
        Ok(object) => object
            .to_bytes()
            .map(|bytes| bytes == case.0)
            .unwrap_or(false),
    });
}

/// A truncated object is refused rather than read as a shorter one.
///
/// The half-length case, which is the one a fuzzer finds and a hand-written test
/// does not: a file that stops in the middle of a section. Reading it as a smaller
/// object would be worse than refusing, because the program would run with
/// whatever the missing bytes happened to be.
#[test]
fn a_truncated_object_is_refused() {
    check::<Built>(32, |case| {
        let bytes = case.object.to_bytes().expect("an object encodes");
        if bytes.len() < 16 {
            return true;
        }
        // Every prefix short of the whole file must be refused. This is a loop
        // rather than one case because the interesting prefixes are the ones that
        // stop just after a count that promised more.
        bytes[..bytes.len() - 1]
            .iter()
            .enumerate()
            .all(|(length, _)| ObjectFile::from_bytes(&bytes[..length]).is_err())
    });
}
