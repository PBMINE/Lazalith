use lazalith_cpu::RegisterFile;
use lazalith_types::{ArchitectureConfig, RegisterIndex};

#[test]
fn all_registers_start_zero_and_are_independent_ordinary_words() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut registers = RegisterFile::new(config);
        assert_eq!(registers.word_width(), config.word_width());
        for raw in 0..16 {
            assert_eq!(registers.read_raw(raw), Ok(0));
        }
        for raw in 0..16 {
            registers.write(RegisterIndex::try_from(raw).unwrap(), u64::from(raw) + 1);
            for other in 0..16 {
                assert_eq!(
                    registers.read_raw(other),
                    Ok(if other <= raw {
                        u64::from(other) + 1
                    } else {
                        0
                    })
                );
            }
        }
    }
}

#[test]
fn writes_truncate_at_actual_word_boundaries() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut registers = RegisterFile::new(config);
        for raw in 0..16 {
            for value in [
                0,
                1,
                0xffff,
                0x10000,
                0x7fffffff,
                0x80000000,
                0xffffffff,
                0x100000000,
                0x8000000000000000,
                u64::MAX,
            ] {
                registers.write_raw(raw, value).unwrap();
                let expected = if config.word_bits() == 32 {
                    value & 0xffffffff
                } else {
                    value
                };
                assert_eq!(
                    registers.read(RegisterIndex::try_from(raw).unwrap()),
                    expected
                );
            }
        }
    }
}

#[test]
fn every_invalid_raw_access_retains_index_and_changes_nothing() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut registers = RegisterFile::new(config);
        for raw in 0..16 {
            registers.write_raw(raw, u64::MAX - u64::from(raw)).unwrap();
        }
        let before = registers.clone();
        for raw in 16..=u8::MAX {
            assert_eq!(registers.read_raw(raw).unwrap_err().input(), raw);
            assert_eq!(registers.write_raw(raw, 42).unwrap_err().input(), raw);
            assert_eq!(registers, before);
        }
    }
}
