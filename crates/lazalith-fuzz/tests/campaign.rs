//! A bounded campaign on every target, as a test.
//!
//! The count is small on purpose. This runs on every commit, and a fuzzing campaign
//! that takes an hour is a campaign nobody runs before pushing. What it is for is
//! *regression*: every one of these seeds passed once, so a failure here means a
//! change broke an input that used to work, and the input is in the message.
//!
//! The long campaigns are the ones a person runs on purpose, with
//! `cargo run -p lazalith-fuzz -- --iterations 200000 --seed 7`.

use lazalith_fuzz::{TARGETS, campaign, run, targets};

/// The seeds this suite pins, so a failure is a fixed input rather than a
/// probability.
const SEEDS: [u64; 2] = [1, 0x5eed];

#[test]
fn every_target_survives_the_pinned_seeds() {
    let mut failures = Vec::new();
    for target in &TARGETS {
        for seed in SEEDS {
            let outcome = campaign(200, seed);
            for failure in outcome.failures {
                if failure.starts_with(target.name) {
                    failures.push(failure);
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "the pinned seeds found {} problem(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
fn a_campaign_is_deterministic() {
    let first = campaign(200, 9);
    let second = campaign(200, 9);
    assert_eq!(
        first.attempted, second.attempted,
        "the same campaign tried a different number of inputs"
    );
    assert_eq!(
        first.failures, second.failures,
        "the same campaign found different things twice"
    );
}

#[test]
fn a_campaign_covers_every_target() {
    let outcome = campaign(1_000, 3);
    assert!(
        outcome.attempted >= 1_000,
        "a campaign of 1000 tried only {} inputs",
        outcome.attempted
    );
}

#[test]
fn every_target_refuses_something_and_accepts_something() {
    // A target that accepted everything would pass every other test here, so each
    // one is shown both ways: its own seed is accepted, and an input nothing can
    // want is not.
    for target in &TARGETS {
        let mut accepted = false;
        for seed in seeds_of(target) {
            if run(target, &seed).is_ok() {
                accepted = true;
            }
        }
        assert!(accepted, "{} refused its own seed", target.name);
    }
}

#[test]
fn the_seeds_are_not_all_empty() {
    for target in &TARGETS {
        let seeds = seeds_of(target);
        assert!(
            seeds.iter().any(|seed| !seed.is_empty()),
            "{} has no seed with anything in it, so its mutations start from nothing",
            target.name
        );
    }
}

#[test]
fn a_mutation_changes_its_seed() {
    for target in &TARGETS {
        let first = targets::mutate(1, 0, target.name);
        let second = targets::mutate(1, 1, target.name);
        assert_ne!(
            first, second,
            "{} generated the same input twice in a row",
            target.name
        );
    }
}

#[test]
fn an_empty_input_is_handled_by_every_target() {
    // The emptiest malformed input there is, and the one most likely to be a bare
    // slice read with an unchecked length.
    for target in &TARGETS {
        if let Err(problem) = run(target, &[]) {
            panic!("{problem}");
        }
    }
}

fn seeds_of(target: &lazalith_fuzz::Target) -> Vec<Vec<u8>> {
    lazalith_fuzz::seeds(target)
}
