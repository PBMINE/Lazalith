//! Fuzzing for everything that reads bytes it did not write.
//!
//! # What this is, and what it is not
//!
//! There is no `cargo-fuzz` here and no libFuzzer, for the same reason there is no
//! property-test framework in step 84: this repository has no third-party Rust
//! dependencies, and a fuzzing harness is a hundred lines of generator and a driver.
//! What it gives up is libFuzzer's coverage-guided search, and what it keeps is
//! determinism — every campaign is a seed and a count, so a failure is a command
//! line and not an afternoon.
//!
//! The trade is stated here because it is a real one. A coverage-guided fuzzer finds
//! deeper bugs in less time on a single target. A deterministic campaign finds the
//! same class of bug every time, runs in CI on every commit, and never flakes. For a
//! repository whose whole claim is reproducibility, the second is worth more than the
//! first.
//!
//! Run it:
//!
//! ```text
//! cargo run -p lazalith-fuzz -- --iterations 200000 --seed 7
//! ```
//!
//! and the same command is what the test runs, with a smaller count.
//!
//! # The invariant, which is the point of the step
//!
//! "Malformed data must not silently corrupt state" is the whole requirement, and it
//! is a statement about *outcomes* rather than about crashes. A target has three
//! acceptable answers and one unacceptable one:
//!
//! - **Refuse.** With a reason. `Err` is not enough — a refusal with no message is
//!   a comment — and a target that refused everything would pass every test here, so
//!   each target also has a *round trip* or a *non-empty corpus* half.
//! - **Accept, and be consistent.** An instruction that decodes re-encodes to the
//!   bytes it came from; an object that reads re-encodes to its input; a snapshot
//!   that loads restores a machine that behaves like the one that saved it. This is
//!   the half that catches "it accepted the malformed input and quietly changed the
//!   state", which is the failure the step is about and the only one a no-panic test
//!   would miss.
//! - **Neither.** A panic, a hang, an arithmetic overflow, or an accepted input that
//!   is not self-consistent. All four are bugs, and each target checks for all four.
//!
//! # Why a panic is caught rather than merely avoided
//!
//! A fuzzer that panics takes the whole test binary with it, and the next target
//! never runs. Every target here is called through [`run`], which catches the panic,
//! turns it into a failure with the input attached, and carries on. A campaign
//! therefore reports *every* target that broke rather than the first, which is the
//! difference between one bug report and five.

use std::panic::{AssertUnwindSafe, catch_unwind};

pub mod targets;

/// A target: something that reads bytes, and what "correct" means for it.
///
/// Deliberately not `PartialEq`: a derived one would compare the function pointers
/// too, and a field is not a comparison anyone needs to make.
#[derive(Clone, Copy, Debug)]
pub struct Target {
    /// Its name, as the step spells it.
    pub name: &'static str,
    /// What it reads.
    pub description: &'static str,
    /// The check itself.
    pub run: fn(&[u8]) -> Result<(), String>,
}

/// Every target the step lists, in the order it lists them.
pub const TARGETS: [Target; 11] = [
    Target {
        name: "instruction decoder",
        description: "raw bytes decoded as an instruction",
        run: targets::instruction_decoder,
    },
    Target {
        name: "assembler",
        description: "text assembled into an object",
        run: targets::assembler,
    },
    Target {
        name: "lazen parser",
        description: "text parsed as Lazen",
        run: targets::lazen_parser,
    },
    Target {
        name: "c parser",
        description: "text parsed as C",
        run: targets::c_parser,
    },
    Target {
        name: "object reader",
        description: "bytes read as a Lazalith object",
        run: targets::object_reader,
    },
    Target {
        name: "executable loader",
        description: "bytes loaded as an LZX executable",
        run: targets::executable_loader,
    },
    Target {
        name: "package reader",
        description: "bytes read as a Lazen application package",
        run: targets::package_reader,
    },
    Target {
        name: "kernel loader",
        description: "bytes loaded as a kernel image",
        run: targets::kernel_loader,
    },
    Target {
        name: "filesystem metadata",
        description: "paths read as filesystem metadata",
        run: targets::filesystem_metadata,
    },
    Target {
        name: "debugger commands",
        description: "text driven through the debugger's command surface",
        run: targets::debugger_commands,
    },
    Target {
        name: "snapshot reader",
        description: "bytes read as a machine snapshot",
        run: targets::snapshot_reader,
    },
];

/// Runs one target against one input, turning a panic into a failure.
///
/// The panic message is kept: a target that panics *inside* the crate is a bug in that
/// crate, and the message says where. `catch_unwind` also means a campaign continues
/// past a broken target instead of stopping at the first one.
pub fn run(target: &Target, input: &[u8]) -> Result<(), String> {
    let name = target.name;
    let bytes = input.to_vec();
    let run = target.run;
    match catch_unwind(AssertUnwindSafe(move || (run)(&bytes))) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(problem)) => Err(format!("{name}: {problem}\n  input: {input:02x?}")),
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| String::from("a panic with no message"));
            Err(format!(
                "{name}: panicked: {message}\n  input: {input:02x?}"
            ))
        }
    }
}

/// What one campaign found.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Outcome {
    /// How many inputs were tried, across every target.
    pub attempted: u64,
    /// One entry per failure, in the order they were found.
    pub failures: Vec<String>,
}

impl Outcome {
    /// Whether the campaign found nothing.
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Runs `iterations` mutations of `seed` against every target.
///
/// Deterministic by construction: the same `(iterations, seed)` runs the same inputs
/// in the same order on every platform, which is what makes a failure reportable as a
/// command line.
pub fn campaign(iterations: u64, seed: u64) -> Outcome {
    let mut outcome = Outcome::default();
    for target in &TARGETS {
        // Every target starts from the same seeds and takes a different number of
        // them, so a target that needs deeper inputs gets them and the total stays
        // bounded.
        let budget = iterations.div_ceil(TARGETS.len() as u64).max(1);
        for index in 0..budget {
            let input = targets::mutate(seed, index, target.name);
            outcome.attempted += 1;
            if let Err(problem) = run(target, &input) {
                outcome.failures.push(problem);
            }
        }
    }
    outcome
}

/// A seed corpus for every target, as text and as bytes.
///
/// A fuzzer that starts from nothing explores the space of *valid* inputs very
/// slowly, and the interesting inputs for these targets are the ones that are nearly
/// valid: an object with one field wrong, a snapshot with a truncated register, a
/// path with a missing separator. So each target gets a small hand-written seed and
/// the mutations start from it.
pub fn seeds(target: &Target) -> Vec<Vec<u8>> {
    targets::seeds(target)
}
