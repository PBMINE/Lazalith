use lazalith_types::{ArchitectureConfig, InvalidRegisterIndex, RegisterIndex, WordWidth};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterFile {
    width: WordWidth,
    values: [u64; RegisterIndex::COUNT as usize],
}

impl RegisterFile {
    pub const fn new(config: ArchitectureConfig) -> Self {
        Self {
            width: config.word_width(),
            values: [0; RegisterIndex::COUNT as usize],
        }
    }

    pub const fn word_width(&self) -> WordWidth {
        self.width
    }

    pub fn read(&self, index: RegisterIndex) -> u64 {
        self.values[index.as_usize()]
    }

    pub fn write(&mut self, index: RegisterIndex, value: u64) {
        self.values[index.as_usize()] = self.width.truncate(value);
    }

    pub fn read_raw(&self, index: u8) -> Result<u64, InvalidRegisterIndex> {
        Ok(self.read(RegisterIndex::try_from(index)?))
    }

    pub fn write_raw(&mut self, index: u8, value: u64) -> Result<(), InvalidRegisterIndex> {
        self.write(RegisterIndex::try_from(index)?, value);
        Ok(())
    }
}
