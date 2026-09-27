//! A generator for the property tests.
//!
//! # Why this exists rather than a framework
//!
//! Property testing is a habit, not a dependency, and this repository has no
//! third-party Rust dependencies at all: every crate here is `no_std`, `unsafe`-free
//! and built by a Nix expression that vendors nothing. Adding a framework would
//! have been the fastest route and the wrong one — it would have put a crate in
//! the build graph that the project's own style argues against, for about two
//! hundred lines of generator.
//!
//! So this is those two hundred lines, and they are deliberately small. What a
//! property test needs from a framework is four things, and this has all four:
//!
//! - **Generation.** [`Gen`] produces values of the shapes a machine is made of.
//! - **Repetition.** [`check`] runs a property over many cases.
//! - **Reproducibility.** Every case comes from a named seed, and the seed is in
//!   the failure message, so a failing property is re-run with one line.
//! - **A seed corpus that is *fixed*.** [`SEEDS`] is a constant list, not a
//!   clock. A property suite that generates a different 500 cases on every run
//!   finds a different bug every time and is never the same test twice; a fixed
//!   list means a failure found today is still found tomorrow, and a fix is
//!   visible as a test that stopped failing rather than as a quiet change of
//!   inputs.
//!
//! # What this does not do
//!
//! **It does not shrink.** A shrunk counterexample is worth a great deal, and
//! this does not produce one. What it does instead is report the *whole* failing
//! case and the seed that made it, and the generators here all produce values
//! whose fields are printable, so the failing case is usually readable as it
//! stands. Shrinking is the piece worth having and the piece worth having *well*;
//! a bad shrink is worse than none, because it reports a case that does not fail.
//!
//! # The randomness
//!
//! `xorshift64*`, chosen because it is four lines, has no state to initialise
//! beyond the seed, and is *the same sequence on every platform* — which matters
//! when a failure has to be reproducible on somebody else's machine. It is not a
//! good generator and it is not trying to be: the inputs are not adversarial, and
//! a fixed corpus plus a plain counter-mix is enough to get past the cases a
//! hand-written list thinks of.

/// The seeds every property runs.
///
/// Fixed, so the suite is the same suite on every run. Adding a seed is how a new
/// corner is covered; the numbers are otherwise arbitrary and the property is
/// what matters, not the order.
pub const SEEDS: [u64; 64] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_0002,
    0x0000_0000_0000_0003,
    0x0000_0000_0000_0005,
    0x0000_0000_0000_0008,
    0x0000_0000_0000_000d,
    0x0000_0000_0000_0015,
    0x0000_0000_0000_0022,
    0x0000_0000_0000_0037,
    0x0000_0000_0000_0059,
    0x0000_0000_0000_0090,
    0x0000_0000_0000_00e9,
    0x0000_0000_0000_0179,
    0x0000_0000_0000_0262,
    0x0000_0000_0000_03db,
    0x0000_0000_0000_063d,
    0x0000_0000_0000_0a18,
    0x0000_0000_0000_1063,
    0x0000_0000_0000_197b,
    0x0000_0000_0000_23de,
    0x0000_0000_0000_35b9,
    0x0000_0000_0000_4f97,
    0x0000_0000_0000_6b50,
    0x0000_0000_0000_9609,
    0x0000_0000_0000_c159,
    0x0000_0000_0001_0162,
    0x0000_0000_0001_4c9b,
    0x0000_0000_0001_b1d4,
    0x0000_0000_0002_6a69,
    0x0000_0000_0003_38fe,
    0x0000_0000_0004_4393,
    0x0000_0000_0005_a228,
    0x0000_0000_0007_64bd,
    0x0000_0000_0009_9552,
    0x0000_0000_000c_36e7,
    0x0000_0000_000f_6f7c,
    0x0000_0000_0013_dc11,
    0x0000_0000_0019_90a6,
    0x0000_0000_0021_053b,
    0x0000_0000_002a_3fd0,
    0x0000_0000_0035_a165,
    0x0000_0000_0043_83fa,
    0x0000_0000_0054_9b8f,
    0x0000_0000_0069_c324,
    0x0000_0000_0083_35b9,
    0x0000_0000_00a2_9fee,
    0x0000_0000_00ca_7c83,
    0x0000_0000_00fa_c218,
    0x0000_0000_0137_7f4b,
    0x0000_0000_0181_b07e,
    0x0000_0000_01dc_11b1,
    0x0000_0000_0247_32e4,
    0x0000_0000_02c4_5517,
    0x0000_0000_0355_7c4a,
    0x0000_0000_03fa_a97d,
    0x0000_0000_04b6_c0b0,
    0x0000_0000_0594_91e3,
    0x0000_0000_0698_0316,
    0x0000_0000_07c6_a949,
    0x0000_0000_092b_d17c,
    0x0000_0000_0aa8_8caf,
    0x0000_0000_0c60_18e2,
    0x0000_0000_0e61_5115,
    0x0000_0000_1090_8348,
];

/// A generator for one test case.
///
/// Cheap to make and cheap to clone, and the state is a single word so a failing
/// case can be replayed by constructing a `Gen` from the seed in the message.
#[derive(Clone, Debug)]
pub struct Gen {
    state: u64,
}

impl Gen {
    /// A generator for a named seed.
    ///
    /// Seed 0 would make `xorshift` stuck at 0 forever, so it is nudged to a
    /// value that is not: a generator that always returns the same case is worse
    /// than no generator, because it looks like it is trying.
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9e37_79b9_7f4a_7c15
            } else {
                seed
            },
        }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        // xorshift64*. The multiply is the `*` of the star variant, and it is
        // what makes the low bits depend on the high ones — without it a
        // generator that takes low bits is a very poor one.
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// The next 32 bits.
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    /// The next 16 bits.
    pub fn next_u16(&mut self) -> u16 {
        (self.next_u64() >> 48) as u16
    }

    /// The next 8 bits.
    pub fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    /// A `bool`, with a one-in-four chance of being `true` because a property
    /// that only ever sees `false` is not a property test.
    pub fn bool(&mut self) -> bool {
        self.next_u8() % 4 == 0
    }

    /// A value in `low..=high`, or `low` when the range is empty.
    pub fn range(&mut self, low: u64, high: u64) -> u64 {
        if high <= low {
            return low;
        }
        low + self.next_u64() % (high - low + 1)
    }

    /// A value below `bound`.
    pub fn below(&mut self, bound: u64) -> u64 {
        self.range(0, bound.saturating_sub(1))
    }

    /// One element of `choices`, or `None` when it is empty.
    pub fn choice<T: Copy>(&mut self, choices: &[T]) -> Option<T> {
        if choices.is_empty() {
            return None;
        }
        choices
            .get(self.below(choices.len() as u64) as usize)
            .copied()
    }

    /// A vector of `count` values from `make`.
    pub fn vector<T>(&mut self, count: usize, mut make: impl FnMut(&mut Self) -> T) -> Vec<T> {
        let mut output = Vec::with_capacity(count);
        for _ in 0..count {
            output.push(make(self));
        }
        output
    }

    /// A byte string of up to `max` bytes, never empty.
    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let count = 1 + self.below(max as u64) as usize;
        self.vector(count, |source| source.next_u8())
    }

    /// A `u32` that is likely to be interesting: small, near a boundary, or
    /// random. Half the cases are drawn from the awkward values, because a
    /// uniform 32-bit sample almost never lands on one and the awkward values are
    /// where conversion bugs live.
    pub fn interesting_u32(&mut self) -> u32 {
        const AWKWARD: [u32; 12] = [
            0,
            1,
            2,
            0x7fff_ffff,
            0x8000_0000,
            0x8000_0001,
            0xffff_ffff,
            0xffff_fffe,
            0x0000_00ff,
            0x0000_ff00,
            0x00ff_0000,
            0x0100_0000,
        ];
        match self.below(4) {
            0 => self.choice(&AWKWARD).unwrap_or(0),
            1 => self.next_u32(),
            2 => self.next_u32() >> self.below(32),
            _ => self.next_u32() << self.below(32),
        }
    }

    /// A `u64` that is likely to be interesting, by the same argument as
    /// [`Gen::interesting_u32`].
    pub fn interesting_u64(&mut self) -> u64 {
        const AWKWARD: [u64; 12] = [
            0,
            1,
            2,
            0x7fff_ffff_ffff_ffff,
            0x8000_0000_0000_0000,
            0x8000_0000_0000_0001,
            0xffff_ffff_ffff_ffff,
            0xffff_ffff_ffff_fffe,
            0x0000_0000_ffff_ffff,
            0x0000_0000_ffff_ff00,
            0xffff_ffff_0000_0000,
            0xffff_ffff_0000_0001,
        ];
        match self.below(4) {
            0 => self.choice(&AWKWARD).unwrap_or(0),
            1 => self.next_u64(),
            2 => self.next_u64() >> self.below(64),
            _ => self.next_u64() << self.below(64),
        }
    }
}

/// A case a [`check`] property runs over.
///
/// A case knows how to build itself and how to describe itself, so every property
/// in the repository reports a failure the same way: the seed that made it, the
/// case in a readable form, and the one line that re-runs it.
pub trait Case: Sized {
    /// Builds one case from a generator.
    fn generate(source: &mut Gen) -> Self;

    /// A readable form of the case, for a failure message.
    fn describe(&self) -> String;
}

/// Runs `property` over `cases` generated values, one per seed.
///
/// `cases` is how many of [`SEEDS`] to run, so a slow property can be given fewer
/// and a fast one more. The seeds are the *first* `cases`, so raising the count
/// extends the suite rather than replacing it -- a case that passed yesterday
/// is still run today.
pub fn check<T: Case>(cases: usize, mut property: impl FnMut(&T) -> bool) {
    for seed in SEEDS.iter().take(cases) {
        let mut random = Gen::seeded(*seed);
        let case = T::generate(&mut random);
        if !property(&case) {
            panic!(
                "property failed for seed {seed:#018x}\n  case: {}\n  \
                 re-run it with Gen::seeded({seed:#018x})",
                case.describe()
            );
        }
    }
}
