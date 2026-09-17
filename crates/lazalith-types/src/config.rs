use crate::RegisterIndex;
use core::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WordWidth {
    W32,
    W64,
}

impl WordWidth {
    pub const fn bits(self) -> u8 {
        match self {
            Self::W32 => 32,
            Self::W64 => 64,
        }
    }

    pub const fn bytes(self) -> u8 {
        self.bits() / 8
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FeatureSet(u32);

impl FeatureSet {
    const BASE_INTEGER: u32 = 1;

    pub const fn base_v1() -> Self {
        Self(Self::BASE_INTEGER)
    }

    pub const fn try_from_bits(input: u32) -> Result<Self, InvalidFeatureSet> {
        let unsupported_bits = input & !Self::BASE_INTEGER;
        if unsupported_bits != 0 {
            Err(InvalidFeatureSet::UnsupportedBits {
                input,
                unsupported_bits,
            })
        } else if input & Self::BASE_INTEGER == 0 {
            Err(InvalidFeatureSet::MissingBaseInteger { input })
        } else {
            Ok(Self(input))
        }
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidFeatureSet {
    UnsupportedBits { input: u32, unsupported_bits: u32 },
    MissingBaseInteger { input: u32 },
}

impl InvalidFeatureSet {
    pub const fn input(&self) -> u32 {
        match *self {
            Self::UnsupportedBits { input, .. } | Self::MissingBaseInteger { input } => input,
        }
    }
}

impl fmt::Display for InvalidFeatureSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBits {
                input,
                unsupported_bits,
            } => write!(
                f,
                "feature bits {input:#010x} contain unsupported bits {unsupported_bits:#010x}"
            ),
            Self::MissingBaseInteger { input } => {
                write!(f, "feature bits {input:#010x} are missing BaseInteger")
            }
        }
    }
}

impl Error for InvalidFeatureSet {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ArchitectureConfig {
    width: WordWidth,
    features: FeatureSet,
}

impl ArchitectureConfig {
    pub const fn new(width: WordWidth, features: FeatureSet) -> Self {
        Self { width, features }
    }

    pub const fn lz32() -> Self {
        Self::new(WordWidth::W32, FeatureSet::base_v1())
    }

    pub const fn lz64() -> Self {
        Self::new(WordWidth::W64, FeatureSet::base_v1())
    }

    pub const fn try_from_bits(
        width: WordWidth,
        feature_bits: u32,
    ) -> Result<Self, InvalidFeatureSet> {
        match FeatureSet::try_from_bits(feature_bits) {
            Ok(features) => Ok(Self::new(width, features)),
            Err(error) => Err(error),
        }
    }

    pub const fn word_width(self) -> WordWidth {
        self.width
    }

    pub const fn word_bits(self) -> u8 {
        self.width.bits()
    }

    pub const fn word_bytes(self) -> u8 {
        self.width.bytes()
    }

    pub const fn pointer_bits(self) -> u8 {
        self.word_bits()
    }

    pub const fn address_bits(self) -> u8 {
        self.word_bits()
    }

    pub const fn register_count(self) -> u8 {
        RegisterIndex::COUNT
    }

    pub const fn instruction_bytes(self) -> u8 {
        8
    }

    pub const fn instruction_alignment(self) -> u8 {
        4
    }

    pub const fn stack_alignment(self) -> u8 {
        self.word_bytes()
    }

    pub const fn supported_data_sizes(self) -> &'static [u8] {
        match self.width {
            WordWidth::W32 => &[1, 2, 4],
            WordWidth::W64 => &[1, 2, 4, 8],
        }
    }

    pub fn supports_data_size(self, bytes: u8) -> bool {
        self.supported_data_sizes().contains(&bytes)
    }

    pub const fn features(self) -> FeatureSet {
        self.features
    }
}

#[cfg(test)]
mod tests {
    use crate::{ArchitectureConfig, FeatureSet, InvalidFeatureSet, RegisterIndex, WordWidth};
    use alloc::{format, string::ToString};
    use core::error::Error;

    #[test]
    fn word_width_queries() {
        for (width, bits, bytes) in [(WordWidth::W32, 32u8, 4u8), (WordWidth::W64, 64, 8)] {
            assert_eq!(width.bits(), bits);
            assert_eq!(width.bytes(), bytes);
        }
    }

    fn assert_queries(
        config: ArchitectureConfig,
        width: WordWidth,
        bits: u8,
        bytes: u8,
        sizes: &[u8],
    ) {
        assert_eq!(config.word_width(), width);
        assert_eq!(config.word_bits(), bits);
        assert_eq!(config.word_bytes(), bytes);
        assert_eq!(config.pointer_bits(), bits);
        assert_eq!(config.address_bits(), bits);
        assert_eq!(config.register_count(), 16u8);
        assert_eq!(config.register_count(), RegisterIndex::COUNT);
        assert_eq!(config.instruction_bytes(), 8u8);
        assert_eq!(config.instruction_alignment(), 4u8);
        assert_eq!(config.stack_alignment(), bytes);
        let supported: &'static [u8] = config.supported_data_sizes();
        assert_eq!(supported, sizes);
        assert_eq!(config.features(), FeatureSet::base_v1());
        assert_eq!(config.features().bits(), 1u32);
        for size in sizes {
            assert!(config.supports_data_size(*size));
        }
    }

    #[test]
    fn lz32_complete_query_table() {
        assert_queries(
            ArchitectureConfig::lz32(),
            WordWidth::W32,
            32,
            4,
            &[1, 2, 4],
        );
    }

    #[test]
    fn lz64_complete_query_table() {
        assert_queries(
            ArchitectureConfig::lz64(),
            WordWidth::W64,
            64,
            8,
            &[1, 2, 4, 8],
        );
    }

    #[test]
    fn both_modes_accept_exactly_their_data_sizes() {
        for bytes in 0..=u8::MAX {
            assert_eq!(
                ArchitectureConfig::lz32().supports_data_size(bytes),
                matches!(bytes, 1 | 2 | 4),
                "LZ32 size {bytes}"
            );
            assert_eq!(
                ArchitectureConfig::lz64().supports_data_size(bytes),
                matches!(bytes, 1 | 2 | 4 | 8),
                "LZ64 size {bytes}"
            );
        }
    }

    #[test]
    fn valid_construction_paths_agree() {
        let features = FeatureSet::try_from_bits(1).unwrap();
        assert_eq!(features, FeatureSet::base_v1());
        assert_eq!(features.bits(), 1u32);
        for (width, named) in [
            (WordWidth::W32, ArchitectureConfig::lz32()),
            (WordWidth::W64, ArchitectureConfig::lz64()),
        ] {
            assert_eq!(ArchitectureConfig::new(width, features), named);
            assert_eq!(ArchitectureConfig::try_from_bits(width, 1), Ok(named));
            assert_eq!(named.word_width(), width);
            assert_eq!(named.features(), features);
        }
    }

    fn assert_rejected(input: u32, expected: InvalidFeatureSet) {
        let error = FeatureSet::try_from_bits(input).unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(error.input(), input);
        assert!(error.source().is_none());
        for width in [WordWidth::W32, WordWidth::W64] {
            let error = ArchitectureConfig::try_from_bits(width, input).unwrap_err();
            assert_eq!(error, expected);
            assert_eq!(error.input(), input);
        }
    }

    #[test]
    fn missing_base_integer_is_rejected() {
        assert_rejected(0, InvalidFeatureSet::MissingBaseInteger { input: 0 });
    }

    #[test]
    fn every_unsupported_bit_wins_over_missing_base() {
        for bit in 1..32 {
            let unsupported_bits = 1u32 << bit;
            for input in [unsupported_bits, unsupported_bits | 1] {
                assert_rejected(
                    input,
                    InvalidFeatureSet::UnsupportedBits {
                        input,
                        unsupported_bits,
                    },
                );
            }
        }
    }

    #[test]
    fn combined_unsupported_bits_are_retained() {
        for unsupported_bits in [0x8000_0002, 0xaaaa_aaaa, 0x5555_5554, 0xffff_fffe] {
            for input in [unsupported_bits, unsupported_bits | 1] {
                assert_rejected(
                    input,
                    InvalidFeatureSet::UnsupportedBits {
                        input,
                        unsupported_bits,
                    },
                );
            }
        }
    }

    #[test]
    fn feature_errors_display_retained_inputs() {
        assert_eq!(
            FeatureSet::try_from_bits(0).unwrap_err().to_string(),
            "feature bits 0x00000000 are missing BaseInteger"
        );
        for input in [2u32, 3, u32::MAX - 1, u32::MAX] {
            assert_eq!(
                FeatureSet::try_from_bits(input).unwrap_err().to_string(),
                format!(
                    "feature bits {input:#010x} contain unsupported bits {:#010x}",
                    input & !1
                )
            );
        }
    }
}
