//! Runs a fuzzing campaign.
//!
//! ```text
//! cargo run -p lazalith-fuzz -- --iterations 200000 --seed 7
//! cargo run -p lazalith-fuzz -- --list
//! ```
//!
//! No arguments, no clap: the whole interface is two numbers and a flag, and a
//! dependency to read two numbers would be a worse trade than four lines of
//! `std::env`. Everything the fuzzer needs is already in the library, and this is
//! the part a person runs by hand rather than the part a test runs.

use std::{env, process::ExitCode};

use lazalith_fuzz::{TARGETS, campaign, run, targets};

fn main() -> ExitCode {
    let mut iterations: u64 = 20_000;
    let mut seed: u64 = 1;
    let mut only: Option<String> = None;
    let mut list = false;
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--iterations" | "-n" => match args.next().and_then(|value| value.parse().ok()) {
                Some(value) => iterations = value,
                None => return usage("--iterations wants a number"),
            },
            "--seed" | "-s" => match args.next().and_then(|value| value.parse().ok()) {
                Some(value) => seed = value,
                None => return usage("--seed wants a number"),
            },
            "--target" | "-t" => match args.next() {
                Some(value) => only = Some(value),
                None => return usage("--target wants a name"),
            },
            "--list" | "-l" => list = true,
            "--help" | "-h" => {
                print_help();
                return ExitCode::SUCCESS;
            }
            other => return usage(&format!("{other} is not an option")),
        }
    }

    if list {
        for target in &TARGETS {
            println!("{}", targets::describe(target));
        }
        return ExitCode::SUCCESS;
    }

    if let Some(name) = only {
        let Some(target) = TARGETS.iter().find(|target| target.name == name) else {
            return usage(&format!("there is no target called {name}"));
        };
        return replay(target, seed, iterations);
    }

    let outcome = campaign(iterations, seed);
    println!(
        "{} inputs across {} targets, seed {seed}, {} failure(s)",
        outcome.attempted,
        TARGETS.len(),
        outcome.failures.len()
    );
    for failure in &outcome.failures {
        println!("\n{failure}");
    }
    if outcome.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Runs one target on a number of its own inputs, for narrowing a failure down.
fn replay(target: &lazalith_fuzz::Target, seed: u64, iterations: u64) -> ExitCode {
    let mut failures = 0_u64;
    for index in 0..iterations {
        let input = targets::mutate(seed, index, target.name);
        if let Err(problem) = run(target, &input) {
            failures += 1;
            println!("\n{problem}");
            if failures == 8 {
                println!("\n(stopped after eight failures; the rest are the same bug)");
                break;
            }
        }
    }
    println!("{iterations} inputs, {failures} failure(s)");
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn usage(problem: &str) -> ExitCode {
    eprintln!("lazalith-fuzz: {problem}");
    print_help();
    ExitCode::from(2)
}

fn print_help() {
    println!(
        "\
lazalith-fuzz -- deterministic campaigns over everything that reads bytes

usage:
  lazalith-fuzz [--iterations N] [--seed S] [--target NAME]
  lazalith-fuzz --list

  -n, --iterations N   how many inputs per target   (default 20000)
  -s, --seed S         the campaign's seed           (default 1)
  -t, --target NAME    one target instead of all of them
  -l, --list           every target and its seeds
  -h, --help           this

A failure is a command line: re-run it with --target and the same seed, and it
happens again."
    );
}
