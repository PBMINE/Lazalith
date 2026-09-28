//! How much the instruction cache is actually worth.
//!
//! Step 93 added a decode cache, and the honest way to report an optimization is a
//! number measured on the machine that will run it. So: the same program, the same
//! number of instructions, on a bus that caches and on a reference bus that does
//! not, timed with `Instant` and reported as instructions per second.
//!
//! Two things this deliberately does not do. It does not assert a threshold, because
//! a timing assertion fails on a busy machine and teaches a team to ignore the test
//! that matters. And it does not use a benchmarking framework, because the project
//! takes no third-party Rust dependencies and a dependency is a worse thing to add
//! than a number printed by a program anyone can read.
//!
//! Run it with `cargo run -p lazalith-memory --example decode_speed --release`.

use std::time::Instant;

use lazalith_cpu::{ArchitecturalState, ReferenceInterpreter};
use lazalith_memory::{AddressSpace, Bus, MemoryRegion, RegionPermissions};
use lazalith_types::{ArchitectureConfig, InstructionAddress, PhysicalAddress, VirtualAddress};

/// How many instructions to run per measurement.
///
/// Large enough that the timer is not the measurement, small enough that the
/// example finishes while somebody is watching it.
const STEPS: usize = 400_000;

/// `li r1, 1`
fn li(register: u8, value: i32) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0] = 0x02;
    bytes[1] = register;
    bytes[4..8].copy_from_slice(&value.to_le_bytes());
    bytes
}

/// A straight-line body of arithmetic: the work a real program does between
/// branches, which is where a fetch happens every single instruction.
fn program() -> Vec<[u8; 8]> {
    let mut code = Vec::new();
    code.push(li(1, 1));
    code.push(li(2, 1));
    for _ in 0..14 {
        // `add r3, r3, r1`
        let mut add = [0; 8];
        add[0] = 0x10;
        add[1] = 0x31;
        code.push(add);
        // `sub r4, r4, r2`
        let mut sub = [0; 8];
        sub[0] = 0x13;
        sub[1] = 0x42;
        code.push(sub);
    }
    code
}

fn run(config: ArchitectureConfig, steps: usize, reference: bool) -> f64 {
    let mut bus = if reference {
        Bus::reference(AddressSpace::new(config))
    } else {
        Bus::new(AddressSpace::new(config))
    };
    bus.map(
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0),
            65_536,
            RegionPermissions::new(true, true, true, true),
        )
        .unwrap(),
    )
    .unwrap();
    for (index, bytes) in program().iter().enumerate() {
        bus.initialize(PhysicalAddress::new(index as u64 * 8), bytes)
            .unwrap();
    }
    let mut cpu = ReferenceInterpreter::new(
        ArchitecturalState::new(
            config,
            InstructionAddress::new(0),
            VirtualAddress::new(16_384),
            0,
        )
        .unwrap(),
    );
    let end = (program().len() * 8) as u64;
    let mut executed = 0usize;
    let start = Instant::now();
    while executed < steps {
        // The body is straight-line, so it runs off the end; start over. The cache
        // survives, which is the point: a real loop hits the same addresses again
        // and again, and the loop-back is the one cost this example does not
        // measure.
        if cpu.architectural_state().pc().as_u64() >= end {
            cpu = ReferenceInterpreter::new(
                ArchitecturalState::new(
                    config,
                    InstructionAddress::new(0),
                    VirtualAddress::new(16_384),
                    0,
                )
                .unwrap(),
            );
        }
        cpu.step(&mut bus).unwrap();
        executed += 1;
    }
    let elapsed = start.elapsed();
    executed as f64 / elapsed.as_secs_f64()
}

fn main() {
    println!(
        "{STEPS} instructions, straight-line arithmetic, release build\n\
         {:<24} {:>14}  {:>10}",
        "bus", "instructions/s", "relative"
    );
    for (name, config) in [
        ("lz32", ArchitectureConfig::lz32()),
        ("lz64", ArchitectureConfig::lz64()),
    ] {
        // Warm the machine up, then measure: the first run pays for the code and
        // the pages, and charging that to the interpreter measures the loader.
        let _ = run(config, 50_000, true);
        let _ = run(config, 50_000, false);
        let reference = run(config, STEPS, true);
        let cached = run(config, STEPS, false);
        println!("{name:<24} {cached:>14.0}  {:>9.2}x", cached / reference);
    }
}
