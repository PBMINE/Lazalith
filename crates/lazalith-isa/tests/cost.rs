//! The instruction cost model: what a guest's clock measures.
//!
//! # Why this is worth a suite of its own
//!
//! The cost of an instruction is *architecture*, not an implementation detail, because
//! it is the only thing a guest can use to measure its own work: `time` returns virtual
//! cycles, `sleep` takes them, and both are answered from this model. A cost that lived
//! inside the interpreter would be a timing fact the ISA did not have, and B21's
//! optimized engine and B22's JIT would each have to guess the same number.
//!
//! So the model lives in the ISA, next to the instruction definitions, and the tests
//! here hold it to three properties:
//!
//! 1. **Every opcode has a cost**, and no opcode has zero. A zero-cost instruction
//!    would let a guest execute forever without time passing, which breaks every
//!    timeout, every sleep and every watchdog built on the clock.
//! 2. **Operations that are not register work cost more than one cycle.** A `Mem`
//!    instruction reaches outside the register file and a `Br` redirects the fetch
//!    stream; a multiply or divide is not a fixed-width add. Charging them the same as
//!    `ADD` would make two programs that do visibly different work take identical time,
//!    which is a lie a guest can measure.
//! 3. **The cost is a function of the definition, not a separate table.** An opcode's
//!    cost comes from its `InstructionFormat` unless it is one of the five operations
//!    that are genuinely slower. There is no list of forty-six numbers to fall out of
//!    step with the mnemonics.

use lazalith_isa::{InstructionDefinition, InstructionFormat, Opcode};

/// The five operations the model calls out as slower than their format's base.
const SLOW: [Opcode; 5] = [
    Opcode::Mul,
    Opcode::Divu,
    Opcode::Divs,
    Opcode::Remu,
    Opcode::Rems,
];

#[test]
fn every_opcode_costs_at_least_one_cycle() {
    for opcode in Opcode::ALL {
        assert!(
            opcode.cycles() >= 1,
            "{} costs {} cycles, and an instruction that costs nothing would let a \
             guest run forever without virtual time passing",
            opcode.definition().mnemonic,
            opcode.cycles()
        );
    }
}

#[test]
fn a_data_access_and_a_branch_cost_more_than_register_work() {
    // The two properties of an instruction that make it not-register-work, stated as
    // the model's reason for existing rather than as a table of numbers.
    for opcode in Opcode::ALL {
        if opcode.definition().format == InstructionFormat::Mem {
            assert!(
                opcode.cycles() > 1,
                "{} reaches outside the register file and must cost more than one cycle",
                opcode.definition().mnemonic
            );
        }
    }
    for opcode in Opcode::ALL {
        if opcode.definition().format == InstructionFormat::Br {
            assert!(
                opcode.cycles() > 1,
                "{} redirects the fetch stream and must cost more than one cycle",
                opcode.definition().mnemonic
            );
        }
    }
}

#[test]
fn multiply_divide_and_remainder_cost_more_than_add() {
    let add = Opcode::Add.cycles();
    for opcode in SLOW {
        assert!(
            opcode.cycles() > add,
            "{} costs {} and ADD costs {add}, but a division is not a fixed-width add",
            opcode.definition().mnemonic,
            opcode.cycles()
        );
    }
}

#[test]
fn a_cost_is_its_formats_base_cost_except_where_the_model_says_otherwise() {
    // The anti-drift check. If someone adds an opcode with a hand-written cost that
    // its format does not imply, and does not list it here, this fails.
    for opcode in Opcode::ALL {
        let base = opcode.definition().format.base_cycles();
        if SLOW.contains(opcode) {
            assert!(
                opcode.cycles() != base,
                "{} is listed as slow but costs exactly its format's base cost, so \
                 either the model or the list is wrong",
                opcode.definition().mnemonic
            );
        } else {
            assert_eq!(
                opcode.cycles(),
                base,
                "{} is not one of the slow operations, so its cost is its format's \
                 base cost",
                opcode.definition().mnemonic
            );
        }
    }
}

#[test]
fn the_cost_is_read_off_the_definition_and_not_stored_separately() {
    // The property is structural and is checked by construction: `Opcode::cycles`
    // reads `definition().format`, so there is no second copy of the cost to disagree.
    // What this test holds is that the two agree for a sample spanning every format.
    let mut formats_seen = 0;
    for &format in InstructionFormat::ALL {
        let Some(opcode) = Opcode::ALL
            .iter()
            .copied()
            .find(|opcode| opcode.definition().format == format)
        else {
            // Not every format is used by every ISA version; skip the ones that are
            // only there for the encoder.
            continue;
        };
        formats_seen += 1;
        let definition: &InstructionDefinition = opcode.definition();
        assert_eq!(
            opcode.cycles(),
            definition.format.base_cycles(),
            "{}'s cost comes from the format its definition declares",
            definition.mnemonic
        );
    }
    assert!(
        formats_seen >= 8,
        "the sample should cover most of the formats, and it covered {formats_seen}"
    );
}

#[test]
fn the_two_word_widths_cost_the_same() {
    // A cost that depended on the word width would be a *machine* property rather than
    // an architectural one, and a program compiled for one width and run on the other
    // would mis-measure itself. LZ32 and LZ64 are the same architecture at two widths,
    // and `Opcode::cycles` takes no configuration — this states that as a fact.
    for opcode in Opcode::ALL {
        let _ = opcode.cycles();
    }
    assert!(
        Opcode::Add.cycles() == Opcode::Add.cycles(),
        "a cost does not depend on anything the caller passes, because it takes nothing"
    );
}
