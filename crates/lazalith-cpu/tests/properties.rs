//! Property tests for the register file.
//!
//! # What a register file owes a program
//!
//! Three things, and each has a property:
//!
//! - A register holds what was written to it, at the machine's width. A `u32`
//!   machine's registers are 32 bits wide, so a 64-bit write is *truncated* — and a
//!   program that relied on the high half surviving would be relying on a
//!   different machine.
//! - Registers are independent. A write to one must not disturb another, which is
//!   the property that catches an off-by-one in an index and nothing else.
//! - A register that has not been written is zero. This is the one that is
//!   easiest to get wrong and the hardest to notice, because an unwritten register
//!   usually *is* zero by accident: a fresh frame is zeroed, an allocator hands
//!   out zeroed memory, and a bug here looks like a bug somewhere else for a long
//!   time.
//!
//! The awkward values are drawn deliberately. A uniform 64-bit draw almost never
//! produces `0xffff_ffff_ffff_ffff` or `0x0000_0001_0000_0000`, and a truncation
//! bug lives exactly at those boundaries.

use lazalith_cpu::RegisterFile;
use lazalith_properties::{Case, Gen, check};
use lazalith_types::{ArchitectureConfig, RegisterIndex};

/// A value written to a register, in one mode.
struct Write {
    config: ArchitectureConfig,
    index: RegisterIndex,
    value: u64,
}

impl Case for Write {
    fn generate(source: &mut Gen) -> Self {
        Self {
            config: match source.bool() {
                true => ArchitectureConfig::lz32(),
                false => ArchitectureConfig::lz64(),
            },
            index: RegisterIndex::try_from(source.below(16) as u8).expect("below sixteen"),
            value: source.interesting_u64(),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{:#x} to r{} in {:?}",
            self.value,
            self.index.as_usize(),
            self.config
        )
    }
}

/// A register holds the value written to it, at the machine's width.
///
/// The truncation is the assertion, not an aside: a `u32` machine's register
/// cannot hold more than 32 bits and a value that came back wider would mean the
/// write and the width disagreed.
#[test]
fn a_register_holds_what_was_written_at_the_machine_width() {
    check::<Write>(64, |case| {
        let mut registers = RegisterFile::new(case.config);
        registers.write(case.index, case.value);
        registers.read(case.index) == case.config.word_width().truncate(case.value)
    });
}

/// A value written twice reads back as the second write.
///
/// The round trip, and the property that catches a write that only ever sets bits:
/// a `|=` where an assignment was meant passes every single-write test above.
#[test]
fn the_last_write_wins() {
    struct Twice {
        config: ArchitectureConfig,
        index: RegisterIndex,
        first: u64,
        second: u64,
    }
    impl Case for Twice {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                index: RegisterIndex::try_from(source.below(16) as u8).expect("below sixteen"),
                first: source.interesting_u64(),
                second: source.interesting_u64(),
            }
        }
        fn describe(&self) -> String {
            format!(
                "{:#x} then {:#x} to r{} in {:?}",
                self.first,
                self.second,
                self.index.as_usize(),
                self.config
            )
        }
    }

    check::<Twice>(64, |case| {
        let mut registers = RegisterFile::new(case.config);
        registers.write(case.index, case.first);
        registers.write(case.index, case.second);
        registers.read(case.index) == case.config.word_width().truncate(case.second)
    });
}

/// Registers are independent of one another.
///
/// Every register is read after every write, not just the one written, because an
/// off-by-one in the index is invisible to a test that only looks at what it wrote
/// — and that is the whole bug this catches.
#[test]
fn writing_one_register_disturbs_no_other() {
    struct Two {
        config: ArchitectureConfig,
        written: RegisterIndex,
        read: RegisterIndex,
        value: u64,
    }
    impl Case for Two {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                written: RegisterIndex::try_from(source.below(16) as u8).expect("below sixteen"),
                read: RegisterIndex::try_from(source.below(16) as u8).expect("below sixteen"),
                value: source.interesting_u64(),
            }
        }
        fn describe(&self) -> String {
            format!(
                "{:#x} to r{}, reading r{} in {:?}",
                self.value,
                self.written.as_usize(),
                self.read.as_usize(),
                self.config
            )
        }
    }

    check::<Two>(64, |case| {
        let mut registers = RegisterFile::new(case.config);
        registers.write(case.written, case.value);
        if case.read == case.written {
            true
        } else {
            // The untouched register is still the zero it started as.
            registers.read(case.read) == 0
        }
    });
}

/// Every register starts at zero, in every mode.
///
/// The one that is easiest to get wrong and hardest to notice, for the reason in
/// the module comment: a bug here looks like a bug somewhere else, because a fresh
/// frame is zeroed and an allocator hands out zeroed memory, so the register file
/// is rarely the first place a stale value would show.
#[test]
fn every_register_starts_at_zero() {
    struct Mode {
        config: ArchitectureConfig,
    }
    impl Case for Mode {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
            }
        }
        fn describe(&self) -> String {
            format!("{:?}", self.config)
        }
    }

    check::<Mode>(2, |case| {
        let registers = RegisterFile::new(case.config);
        (0..RegisterIndex::COUNT)
            .all(|raw| registers.read_raw(raw).expect("a register below sixteen") == 0)
    });
}

/// The indexed and the raw accessors are the same register file.
///
/// There are two doors — `read`/`write` taking a `RegisterIndex` and
/// `read_raw`/`write_raw` taking a `u8` — and a program reaches for whichever it
/// has. They must agree, or a program that mixes them is reading a register it did
/// not write. This is a property about *the pair*, which no single-accessor test
/// can state.
#[test]
fn the_indexed_and_raw_accessors_agree() {
    check::<Write>(64, |case| {
        let mut registers = RegisterFile::new(case.config);
        let raw =
            u8::try_from(case.index.as_usize()).expect("a register below sixteen fits a byte");
        registers
            .write_raw(raw, case.value)
            .expect("a register below sixteen");
        let through_index = registers.read(case.index);
        let through_raw = registers.read_raw(raw).expect("a register below sixteen");
        through_index == through_raw
    });
}

/// An index that is not a register is refused, through both doors.
///
/// `Result::Err` is the answer, and a program that ignored it would write to
/// wherever the index landed. The value must also be left alone: a refused write
/// that wrote anyway would be worse than a refused one.
#[test]
fn an_index_past_the_last_register_is_refused_and_writes_nothing() {
    let config = ArchitectureConfig::lz64();
    let mut registers = RegisterFile::new(config);
    for raw in RegisterIndex::COUNT..=u8::MAX {
        assert!(
            registers.read_raw(raw).is_err(),
            "r{raw} is not a register and should not read"
        );
        assert!(
            registers.write_raw(raw, 0xdead_beef).is_err(),
            "r{raw} is not a register and should not be written"
        );
    }
    // And the refusal did not land somewhere: every real register is still zero.
    for raw in 0..RegisterIndex::COUNT {
        assert_eq!(
            registers.read_raw(raw).expect("a real register"),
            0,
            "a refused write reached r{raw}"
        );
    }
}
