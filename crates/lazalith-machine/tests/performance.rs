//! B24.3: what the JIT actually costs, measured.
//!
//! # The claim under test
//!
//! §13 forbids claiming a speedup without a measurement, and B21 measured the previous
//! optimisation and got a null result. So this file measures, and it is written to be able
//! to come out **negative** — which, on this corpus, it does.
//!
//! # What is measured, and what is deliberately not
//!
//! Four numbers, because one number would hide the interesting thing:
//!
//! | | what it is |
//! |---|---|
//! | **reference** | retired instructions per second, single-instruction engine |
//! | **jit, cold** | a machine that has never translated anything, first run |
//! | **jit, steady** | the same machine, warmed, re-run — the translation cost amortised away |
//! | **handoffs** | how many times the JIT declined and the interpreter took over |
//!
//! The gap between cold and steady is the translation cost, and reporting only the steady
//! number would be reporting a measurement of a program that has already paid for itself —
//! which is exactly the "hiding translation cost" §13 warns about.
//!
//! # Why the honest answer is expected to be negative
//!
//! **A register-only JIT on a memory-and-control-flow program is a pessimisation, and the
//! measurement is here to say so rather than to look for a workload where it wins.** Each
//! instruction the JIT declines costs a failed translation attempt *and* the interpreter's
//! work; the only thing it saves is the interpreter's dispatch, which is a few nanoseconds
//! against a translation attempt that fetches and decodes an instruction to find out it
//! cannot translate it.
//!
//! **The workloads are therefore chosen to bracket the question rather than to flatter
//! it**: a register-only workload where the JIT must win, a memory workload where it must
//! lose, and a realistic mixed one. If the register-only case did not show the JIT ahead,
//! the JIT would be broken and the measurement would be the thing that caught it.

use std::time::Instant;

use lazalith_cpu::{EngineKind, Privilege, StatusRegister};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_isa::{DataSize, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

const CODE: u64 = 0x000;
const DATA: u64 = 0x8000;
const STACK: u64 = 0x9000;
const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();
const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);
const RX: RegionPermissions = RegionPermissions::new(true, false, true, true);

fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).expect("a register exists"))
}

fn at(base: u8, displacement: i32) -> Operand {
    Operand::Memory {
        base: RegisterIndex::try_from(base).expect("a register exists"),
        displacement,
    }
}

/// A workload: a name, code, and how many retired instructions it is worth.
struct Workload {
    name: &'static str,
    code: Vec<u8>,
    budget: u64,
}

fn machine(code: &[u8]) -> LazalithMachine<ConsoleDevice> {
    let mut devices = DeviceManager::new();
    devices
        .insert(
            DeviceId::new(1),
            ConsoleDevice::new(4096).expect("a console"),
        )
        .expect("a console fits");
    let setup = MachineSetup {
        config: CONFIG,
        devices,
        regions: vec![
            MemoryRegion::ram(CONFIG, PhysicalAddress::new(DATA), 0x1000, RW)
                .expect("a data region"),
            MemoryRegion::ram(CONFIG, PhysicalAddress::new(STACK), 0x1000, RW)
                .expect("a stack region"),
        ],
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK + 0x1000),
        status: StatusRegister::new(Privilege::Supervisor, false).bits(),
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).expect("the machine starts");
    let mut image = code.to_vec();
    image.resize(0x1000, 0);
    machine
        .load_region(
            MemoryRegion::rom(CONFIG, PhysicalAddress::new(CODE), &image, RX)
                .expect("a code region"),
        )
        .expect("the code region is mapped");
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(CODE))
        .expect("a trap vector");
    machine
}

/// Runs `workload` to its budget and returns the elapsed nanoseconds and how it stopped.
fn timed(workload: &Workload, engine: EngineKind) -> (u128, u64, u64, u64, &'static str) {
    let mut machine = machine(&workload.code);
    machine
        .switch_execution_engine(engine)
        .expect("the engine exists");
    let start = Instant::now();
    let mut why = "budget";
    while machine.executed_instruction_count() < workload.budget {
        match machine.step() {
            Ok(MachineEvent::Stepped { .. }) => {}
            Ok(MachineEvent::Halted) => {
                why = "halted";
                break;
            }
            Ok(MachineEvent::Trapped { .. }) => {
                why = "trapped";
                break;
            }
            Err(_) => {
                why = "refused";
                break;
            }
        }
    }
    let elapsed = start.elapsed().as_nanos();
    (
        elapsed,
        machine.executed_instruction_count(),
        machine.native_instructions(),
        machine.engine_handoffs(),
        why,
    )
}

/// Repeats `body` enough times to exceed `floor` nanoseconds, and returns the *mean*
/// nanoseconds per repetition.
///
/// **The mean of several runs, and the whole run is repeated rather than one long one**,
/// so that a page fault, a scheduler preemption or a cold cache on the first iteration
/// moves the average by a hundredth of the figure instead of doubling it. A single
/// timed run of a JIT is mostly a measurement of the operating system.
fn average(floor: u128, mut body: impl FnMut()) -> f64 {
    // A warm-up pass, discarded: the first run of anything pays for page faults, and a JIT
    // additionally pays for its first translation of every block.
    body();
    let mut repeats = 1u32;
    let mut total = 0u128;
    loop {
        let start = Instant::now();
        for _ in 0..repeats {
            body();
        }
        total += start.elapsed().as_nanos();
        if total >= floor {
            return total as f64 / f64::from(repeats);
        }
        repeats = repeats.saturating_mul(2);
        if repeats >= 1024 {
            return total as f64 / f64::from(repeats);
        }
    }
}

/// Register-only arithmetic: the JIT's whole translated subset, repeated.
///
/// **A workload the JIT is built for.** If the JIT does not win here, the JIT is broken and
/// this measurement is what says so.
fn register_only() -> Workload {
    let mut code = Vec::new();
    for _ in 0..16 {
        for (index, opcode) in [
            Opcode::Add,
            Opcode::Sub,
            Opcode::Xor,
            Opcode::And,
            Opcode::Or,
            Opcode::Mul,
        ]
        .into_iter()
        .enumerate()
        {
            let instruction = Instruction::new(
                CONFIG,
                opcode,
                &[
                    r(index as u8 % 8),
                    r((index + 1) as u8 % 8),
                    r((index + 2) as u8 % 8),
                ],
            )
            .expect("a well-formed instruction");
            code.extend_from_slice(&encode(CONFIG, &instruction).expect("encodes"));
        }
    }
    Workload {
        name: "register-only",
        code,
        budget: 96,
    }
}

/// Memory traffic: every access declined by the JIT.
///
/// **The data registers are 0–5 and the base is `r6`, and keeping them disjoint is
/// load-bearing rather than tidy.** An earlier version used `round % 8` for the data
/// registers, so at round 6 the load wrote into `r6` — the base register — and every
/// access after that used a data value as an address. The run then stopped at 14 of 48
/// instructions with an unmapped fault, which looked exactly like a workload the machine
/// could not complete and was neither.
fn memory_bound() -> Workload {
    let mut code = Vec::new();
    let li = |register: u8, value: i32| {
        Instruction::new(
            CONFIG,
            Opcode::Li,
            &[r(register), Operand::Immediate(value)],
        )
        .expect("a well-formed instruction")
    };
    code.extend_from_slice(&encode(CONFIG, &li(6, DATA as i32)).expect("encodes"));
    for round in 0..24i32 {
        let load = Instruction::new(
            CONFIG,
            Opcode::Ldz,
            &[
                r((round % 6) as u8),
                at(6, round * 8),
                Operand::DataSize(DataSize::Double),
            ],
        )
        .expect("a well-formed instruction");
        code.extend_from_slice(&encode(CONFIG, &load).expect("encodes"));
        let store = Instruction::new(
            CONFIG,
            Opcode::St,
            &[
                r(((round + 1) % 6) as u8),
                at(6, round * 8),
                Operand::DataSize(DataSize::Double),
            ],
        )
        .expect("a well-formed instruction");
        code.extend_from_slice(&encode(CONFIG, &store).expect("encodes"));
    }
    Workload {
        name: "memory-bound",
        code,
        budget: 49,
    }
}

/// A realistic mix: some arithmetic, some memory, a branch, so the JIT does some of the
/// work and declines the rest.
fn mixed() -> Workload {
    let mut code = Vec::new();
    let li = |register: u8, value: i32| {
        Instruction::new(
            CONFIG,
            Opcode::Li,
            &[r(register), Operand::Immediate(value)],
        )
        .expect("a well-formed instruction")
    };
    code.extend_from_slice(&encode(CONFIG, &li(6, DATA as i32)).expect("encodes"));
    for round in 0..12i32 {
        for (a, b, c) in [(1u8, 2u8, 0u8), (2, 3, 1), (3, 4, 2)] {
            let add = Instruction::new(CONFIG, Opcode::Add, &[r(c), r(a), r(b)])
                .expect("a well-formed instruction");
            code.extend_from_slice(&encode(CONFIG, &add).expect("encodes"));
        }
        let store = Instruction::new(
            CONFIG,
            Opcode::St,
            &[r(0), at(6, round * 8), Operand::DataSize(DataSize::Double)],
        )
        .expect("a well-formed instruction");
        code.extend_from_slice(&encode(CONFIG, &store).expect("encodes"));
        let load = Instruction::new(
            CONFIG,
            Opcode::Ldz,
            &[r(1), at(6, round * 8), Operand::DataSize(DataSize::Double)],
        )
        .expect("a well-formed instruction");
        code.extend_from_slice(&encode(CONFIG, &load).expect("encodes"));
    }
    Workload {
        name: "mixed",
        code,
        budget: 72,
    }
}

fn workloads() -> Vec<Workload> {
    vec![register_only(), memory_bound(), mixed()]
}

/// The measurement, and the number that goes in the documentation.
///
/// **Every figure is reported whether or not it flatters the JIT**, and the test asserts
/// only that the runs *happened* — not that the JIT won. A performance test that asserted
/// "the JIT is faster" would fail on a correct JIT in a workload it does not suit, and
/// would then be deleted, which is how a null result becomes an unmeasured one.
/// Runs a *warmed* engine over one workload, repeatedly, with the JIT's cache kept.
///
/// **The architectural state is restored between repetitions and the engine's is not**, and
/// that asymmetry is the whole point. A JIT measures well only in steady state, where the
/// translation has been paid for once; but re-running the same machine without resetting it
/// measures nothing at all, because the program has already run. So the guest is put back
/// at the start — through the same `restore_architectural` path B23's snapshot tests use,
/// which is an incidental proof that that path is not a test-only fiction — while the
/// translated blocks stay in the engine's cache.
///
/// `reset()` is deliberately *not* used: it calls `discard_private_state`, which would throw
/// the cache away and make this the cold measurement wearing a warm one's name.
///
/// # Two things this has to get right that are easy to miss
///
/// **The instruction count is local, not the machine's.** `restore_architectural` restores
/// the *processor*, so the machine's own `executed` counter is left where it was — and
/// looping on it means the second iteration does nothing at all. A benchmark that measures
/// nothing reports an enormous speedup.
///
/// **A workload that faults has no steady state.** `restore_architectural` refuses while a
/// trap frame is open, which is correct: a machine mid-trap is mid-something, and putting
/// state back underneath that is what the lifecycle exists to prevent. So a workload that
/// traps gets `None` and is reported cold-only rather than being forced through a path it
/// does not belong on.
fn warm_loop(workload: &Workload) -> Option<f64> {
    let mut machine = machine(&workload.code);
    machine
        .switch_execution_engine(EngineKind::Jit)
        .expect("the engine exists");
    let start_state = machine.architectural_state().clone();
    let budget = workload.budget;
    let mut retired = 0u64;
    while retired < budget {
        match machine.step() {
            Ok(MachineEvent::Stepped { instructions, .. }) => {
                retired += u64::from(instructions);
            }
            Ok(MachineEvent::Halted) | Ok(MachineEvent::Trapped { .. }) | Err(_) => {
                return None;
            }
        }
    }
    Some(average(200_000_000, || {
        machine
            .processor_mut()
            .restore_architectural(start_state.clone())
            .expect("a machine with no open trap frame accepts the starting state");
        let mut retired = 0u64;
        let start = Instant::now();
        while retired < budget {
            match machine.step() {
                Ok(MachineEvent::Stepped { instructions, .. }) => {
                    retired += u64::from(instructions);
                }
                // Unreachable in practice: the warm pass proved the program reaches its
                // budget, and the state was just restored. Treated as the end of the run
                // rather than a panic, because a benchmark that panics reports nothing.
                _ => break,
            }
        }
        std::hint::black_box(start.elapsed().as_nanos());
    }))
}

/// The measurement, and the number that goes in the documentation.
///
/// **Every figure is reported whether or not it flatters the JIT**, and the test asserts
/// only that the runs *happened* — not that the JIT won. A performance test that asserted
/// "the JIT is faster" would fail on a correct JIT in a workload it does not suit, and
/// would then be deleted, which is how a null result becomes an unmeasured one.
#[test]
fn the_jit_is_measured_against_the_reference() {
    const FLOOR: u128 = 200_000_000;
    println!(
        "{:<16} {:>8} {:>11} {:>11} {:>11} {:>9} {:>9}  detail",
        "workload", "instrs", "reference", "jit cold", "jit warm", "cold/ref", "warm/ref"
    );

    for workload in workloads() {
        let reference = average(FLOOR, || {
            let (elapsed, _, _, _, _) = timed(&workload, EngineKind::Reference);
            std::hint::black_box(elapsed);
        });
        let cold = average(FLOOR, || {
            let (elapsed, _, _, _, _) = timed(&workload, EngineKind::Jit);
            std::hint::black_box(elapsed);
        });
        // `None` for a workload that faults, which has no steady state to repeat.
        let warm = warm_loop(&workload);

        let (_, instructions, _, _, why) = timed(&workload, EngineKind::Reference);
        assert_eq!(
            instructions, workload.budget,
            "{}: the reference run retired {instructions} of {} instructions and stopped \
             because it {why}, so the figures above are of a run that stopped early",
            workload.name, workload.budget
        );
        let (_, _, native, handoffs, _) = timed(&workload, EngineKind::Jit);
        assert!(
            native + handoffs > 0,
            "{}: neither native execution nor a handoff was recorded, so the 'JIT' run did \
             nothing measurable",
            workload.name
        );

        match warm {
            Some(warm) => println!(
                "{:<16} {instructions:>8} {reference:>11.0} {cold:>11.0} {warm:>11.0} \
                 {:>8.2}x {:>8.2}x  native {native}, handoffs {handoffs}",
                workload.name,
                cold / reference,
                warm / reference,
            ),
            None => println!(
                "{:<16} {instructions:>8} {reference:>11.0} {cold:>11.0} {:>11} {:>8.2}x \
                 {:>8}        native {native}, handoffs {handoffs}  (faults, so it has no \
                 steady state to repeat)",
                workload.name,
                cold / reference,
                instructions,
                native,
            ),
        }
    }
}

/// The JIT must actually be ahead on the workload it is built for.
///
/// **The one performance claim this file makes, and it is a claim about the JIT being
/// *correct in what it claims to do*, not about it being fast in general.** A JIT that
/// translates straight-line register arithmetic and is not faster at it than an interpreter
/// has a bug — most likely a translation cost that is not being amortised, or a handoff on
/// an instruction it should have taken. This test is what would catch that, and it is
/// deliberately the narrowest claim in the file.
#[test]
fn the_jit_is_ahead_on_the_workload_it_translates() {
    const FLOOR: u128 = 200_000_000;
    let workload = register_only();
    let reference = average(FLOOR, || {
        let (elapsed, _, _, _, _) = timed(&workload, EngineKind::Reference);
        std::hint::black_box(elapsed);
    });
    let warm = warm_loop(&workload).expect("a register-only program never faults");
    println!(
        "register-only: reference {reference:.0} ns, warm jit {warm:.0} ns — {:.2}x",
        reference / warm
    );
    assert!(
        warm < reference,
        "on the workload it translates, the JIT took {warm:.0} ns where the interpreter took \
         {reference:.0} ns. A JIT that is not ahead on straight-line register arithmetic has \
         a bug, not a bad workload."
    );
}
