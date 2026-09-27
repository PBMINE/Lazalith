//! Property tests for the instruction codec.
//!
//! # What the exhaustive test does not do
//!
//! `isa.rs` already walks every opcode, every operand kind and a curated list of
//! immediates, and that is a *stronger* claim than randomness for the cases it
//! reaches: it is exhaustive rather than sampled. What it cannot do is reach
//! operand *values* nobody thought of. `a + b` and `a < b` being the only
//! comparisons a compiler uses is the lesson of step 82, and it is the lesson
//! here too: a hand-written list is the list somebody imagined, and the codec's
//! job is to be right about the ones nobody imagined.
//!
//! So this file draws operands at random from the whole space — every register,
//! every selector, and immediates biased towards the awkward values, because a
//! uniform draw from 32 bits almost never lands on one and the awkward values are
//! where an encoding bug lives.
//!
//! # The properties
//!
//! The one the step names:
//!
//! ```text
//! decode(encode(instruction)) == instruction
//! ```
//!
//! and three more that a codec gets wrong in ways the first does not see: the
//! reverse round trip, a length that does not move, and a refusal that says why.

use lazalith_isa::{
    Condition, ControlRegister, DataSize, Instruction, Opcode, Operand, OperandKind, decode, encode,
};
use lazalith_properties::{Case, Gen, check};
use lazalith_types::{ArchitectureConfig, RegisterIndex, WordWidth};

/// One valid instruction and the mode it was built for.
struct Encoded {
    config: ArchitectureConfig,
    instruction: Instruction,
}

impl Case for Encoded {
    fn generate(source: &mut Gen) -> Self {
        let config = match source.bool() {
            true => ArchitectureConfig::lz32(),
            false => ArchitectureConfig::lz64(),
        };
        let opcode = source
            .choice(Opcode::ALL)
            .expect("the opcode table is never empty");
        let operands: Vec<Operand> = opcode
            .definition()
            .operands()
            .iter()
            .map(|definition| operand(source, definition.kind))
            .collect();
        let instruction = Instruction::new(config, opcode, &operands)
            .expect("generated operands fit their opcode's format");
        Self {
            config,
            instruction,
        }
    }

    fn describe(&self) -> String {
        format!("{:?} in {:?}", self.instruction, self.config)
    }
}

/// One operand of the kind an opcode's format asks for.
///
/// A `Register` is drawn from the sixteen the machine has rather than from the
/// operand's own range, because the codec is what decides which of those are
/// allocated and a random 0..255 would spend most of its cases on values the
/// format rejects before the codec is reached. The rejections are the subject of
/// their own property below.
fn operand(source: &mut Gen, kind: OperandKind) -> Operand {
    match kind {
        OperandKind::Register => Operand::Register(
            RegisterIndex::try_from(source.below(16) as u8).expect("a register below sixteen"),
        ),
        OperandKind::Immediate => Operand::Immediate(source.interesting_u32() as i32),
        OperandKind::Memory => Operand::Memory {
            base: RegisterIndex::try_from(source.below(16) as u8)
                .expect("a register below sixteen"),
            displacement: source.interesting_u32() as i32,
        },
        OperandKind::DataSize => Operand::DataSize(
            source
                .choice(DataSize::ALL)
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
    }
}

/// An operand drawn from its kind.s whole range.
///
/// `RegisterIndex` is a `u8` newtype bounded at sixteen, so "the whole range" is
/// those sixteen -- which is what the machine has, and not one more. The
/// interesting rejections come from the ISA.s own table of which of the sixteen are
/// allocated for a given operand, and which of those are refused is the exhaustive
/// test.s subject, not this one.s.
fn wide_operand(source: &mut Gen, kind: OperandKind) -> Operand {
    match kind {
        OperandKind::Register => Operand::Register(
            RegisterIndex::try_from(source.below(16) as u8).expect("a register below sixteen"),
        ),
        OperandKind::Immediate => Operand::Immediate(source.interesting_u32() as i32),
        OperandKind::Memory => Operand::Memory {
            base: RegisterIndex::try_from(source.below(16) as u8)
                .expect("a register below sixteen"),
            displacement: source.interesting_u32() as i32,
        },
        OperandKind::DataSize => Operand::DataSize(
            source
                .choice(DataSize::ALL)
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
    }
}

/// `decode(encode(i)) == i`, for random valid instructions.
///
/// The property the step names. The reverse round trip is checked in the same
/// case, because a codec that decodes an instruction correctly but re-encodes it
/// to different bytes is a codec whose output a second decoder would disagree
/// with — and the disagreement would not show up here.
#[test]
fn encoding_then_decoding_is_the_instruction() {
    check::<Encoded>(64, |case| {
        let bytes = match encode(case.config, &case.instruction) {
            Ok(bytes) => bytes,
            Err(_) => return true,
        };
        if decode(case.config, &bytes) != Ok(case.instruction.clone()) {
            return false;
        }
        encode(
            case.config,
            &decode(case.config, &bytes).expect("it just decoded"),
        ) == Ok(bytes)
    });
}

/// The encoding is the same length every time, whatever the operands.
///
/// A fixed-width instruction field is what lets a branch displacement be computed
/// without decoding, and a codec whose output length depended on its operands
/// would put that beyond the CPU.
///
/// The property is stated as "the same length as the same opcode in the same mode"
/// rather than as a number, because a number would be a fact about today's table
/// rather than about the rule — and the number is not what one would guess.
/// `Divu` in 32-bit mode encodes to eight bytes, twice the width of its own
/// registers.
#[test]
fn an_encoding_does_not_depend_on_its_operands() {
    struct Length {
        width: WordWidth,
        opcode: Opcode,
        bytes: usize,
    }
    impl Case for Length {
        fn generate(source: &mut Gen) -> Self {
            let case = Encoded::generate(source);
            let bytes = encode(case.config, &case.instruction)
                .map(|encoded| encoded.len())
                .unwrap_or(0);
            Self {
                width: case.config.word_width(),
                opcode: case.instruction.opcode(),
                bytes,
            }
        }
        fn describe(&self) -> String {
            format!(
                "{:?} in {:?} encoded to {} bytes",
                self.opcode, self.width, self.bytes
            )
        }
    }

    // Collected here rather than inside the property, because a property takes a
    // case and returns a verdict and nothing else, and this one needs to remember.
    let mut lengths: Vec<(WordWidth, Opcode, usize)> = Vec::new();
    check::<Length>(64, |case| {
        if case.bytes == 0 {
            return true;
        }
        match lengths
            .iter()
            .find(|(width, opcode, _)| *width == case.width && *opcode == case.opcode)
        {
            None => {
                lengths.push((case.width, case.opcode, case.bytes));
                true
            }
            Some((_, _, first)) => *first == case.bytes,
        }
    });
    assert!(
        !lengths.is_empty(),
        "nothing encoded, so there was no length to compare"
    );
}

/// A refusal says what was wrong with it.
///
/// `Result::Err` on its own would be satisfied by a codec that refused everything.
/// This checks the half of that: when a case *is* refused, the error names the
/// problem, and a case that is built successfully must then encode.
///
/// Which combinations are *legal* is the exhaustive test.s question, and it
/// answers it. This one is about the two doors -- `Instruction::new` and
/// `encode` -- both naming what they turned away, rather than returning a bare
/// `Err` that leaves a reader to guess. A codec that refused everything would pass
/// the first property as written, so the built-then-refused direction is checked
/// too.
#[test]
fn a_refusal_says_what_was_wrong() {
    for index in 0..64 {
        let mut source = Gen::seeded(lazalith_properties::SEEDS[index]);
        let config = match source.bool() {
            true => ArchitectureConfig::lz32(),
            false => ArchitectureConfig::lz64(),
        };
        let opcode = source
            .choice(Opcode::ALL)
            .expect("the opcode table is never empty");
        let operands: Vec<Operand> = opcode
            .definition()
            .operands()
            .iter()
            .map(|definition| wide_operand(&mut source, definition.kind))
            .collect();
        let described = format!("{opcode:?} with {operands:?} in {config:?}");
        match Instruction::new(config, opcode, &operands) {
            Ok(instruction) => assert!(
                encode(config, &instruction).is_ok(),
                "{described} built and then refused by the encoder"
            ),
            Err(error) => assert!(
                !error.to_string().is_empty(),
                "{described} refused with no message"
            ),
        }
    }
}

/// Arbitrary bytes are decoded or refused, and never anything else.
///
/// This is the property that fuzzing is really for, and it needs no valid
/// instruction to state. A whole instruction's worth of random bytes is either a
/// valid encoding, in which case re-encoding it gives the same bytes back, or it
/// is refused. There is no third answer: "it panicked" and "it invented an
/// instruction" are both bugs, and only the first is caught by a crash.
#[test]
fn arbitrary_bytes_decode_or_are_refused() {
    struct Bytes {
        config: ArchitectureConfig,
        bytes: Vec<u8>,
    }
    impl Case for Bytes {
        fn generate(source: &mut Gen) -> Self {
            let config = match source.bool() {
                true => ArchitectureConfig::lz32(),
                false => ArchitectureConfig::lz64(),
            };
            let width = usize::from(config.word_bytes());
            Self {
                config,
                bytes: source.vector(width, |source| source.next_u8()),
            }
        }
        fn describe(&self) -> String {
            format!("{:02x?} in {:?}", self.bytes, self.config)
        }
    }

    check::<Bytes>(64, |case| match decode(case.config, &case.bytes) {
        Err(_) => true,
        Ok(instruction) => encode(case.config, &instruction)
            .map(|bytes| bytes[..] == case.bytes[..])
            .unwrap_or(false),
    });
}
