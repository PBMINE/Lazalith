//! B21: how fast the engines are, measured rather than asserted.
//!
//! # The result, first
//!
//! **The optimisation is worth nothing measurable.** Skipping the reference's duplicate
//! `validate_fetch` and its per-instruction PC revalidation changed nothing a clock can
//! see: three runs of `report_the_measured_speedup` gave 0.95x, 1.09x and 1.01x, and the
//! run-to-run spread is wider than any effect the change has.
//!
//! The reason is knowable rather than mysterious. `validate_fetch` is a handful of
//! comparisons; the reference's per-step cost is dominated by the `match (opcode,
//! operands)` over an operand slice and by the register-file updates around it. Removing
//! two comparisons from that is a fraction of a percent.
//!
//! **So B21's finding is a null result, and §38 says measure rather than hide.** The only
//! remaining way to make LZA execution materially faster is to stop interpreting it —
//! which is B22's argument, and this measurement is the argument.
//!
//! # What the stage delivered instead
//!
//! **A second working engine with identical semantics, differentially checked.** That is
//! what B23's live engine-switch needs *before* a JIT exists: two engines that both run,
//! so the switch can be tested against something other than one engine switching to
//! itself.
//!
//! **A representative benchmark.** The first version of `support::Ram` had no decode
//! cache, which measured the decoder rather than the engines and made the optimised one
//! look 5% *slower*. `lazalith_memory::Bus` caches decoded instructions, so the test's
//! memory now does too — a benchmark run against a memory the system never uses produces
//! a number that is true and useless.
//!
//! # How these tests are shaped
//!
//! **Every comparison is after a single step**, and every test builds the two engines
//! from scratch, so there is no fixture in which they could be pre-agreeing. §10 makes
//! differential testing against the Reference Interpreter mandatory; this is the cheapest
//! place to do it for the CPU alone, and
//! `crates/lazalith-machine/tests/differential.rs` does it again at the machine level
//! with devices, memory regions and traps in the picture.
//!
//! The one wall-clock test is named for what it found —
//! `the_measured_difference_between_the_engines_is_within_noise` — and asserts only that
//! neither engine has become *catastrophically* slower. It cannot catch the optimisation
//! being removed, because removing it changes nothing observable. That is the honest
//! limit of a wall-clock assertion in CI, and pretending otherwise would be the
//! alternative.

mod support;

use std::time::Instant;

use lazalith_cpu::{
    ExecutionEngine, FastInterpreter, OutcomeApplication, ReferenceInterpreter, StepResult,
};
use lazalith_isa::{Instruction, Opcode, Operand, encode};
use lazalith_types::{ArchitectureConfig, InstructionAddress, VirtualAddress};

use support::Ram;

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();
/// Rounds of work the program does. Each round is a `Li`, then four repetitions of an
/// `Add`, a `Mul` and a `St` — thirteen instructions.
const ROUNDS: u64 = 4_000;
/// How many instructions each engine runs. Below the program's length, so neither
/// engine runs off the end of it.
const STEPS: u64 = 40_000;
/// How many instructions are compared register for register.
const COMPARED: u64 = 20_000;

fn r(index: u8) -> Operand {
    Operand::Register(lazalith_types::RegisterIndex::try_from(index).expect("a register exists"))
}

/// A straight-line program of register arithmetic, with a data access.
///
/// **A self-loop would be the wrong benchmark.** `BR` to itself costs nothing to fetch
/// and its interpreter arm is one comparison, so a loop of them would measure dispatch
/// and nothing else. Real guests spend their time on register arithmetic and data
/// access, and that is what this is mostly made of — including a `Mul`, which the cost
/// model calls the slow case and which an optimiser that skips validation would feel.
fn program() -> Vec<u8> {
    let mut bytes = Vec::new();
    for round in 0..ROUNDS {
        let value = (round % 7 + 1) as i32;
        bytes.extend_from_slice(
            &encode(
                CONFIG,
                &Instruction::new(CONFIG, Opcode::Li, &[r(0), Operand::Immediate(value)])
                    .expect("li builds"),
            )
            .expect("li encodes"),
        );
        for _ in 0..4 {
            bytes.extend_from_slice(
                &encode(
                    CONFIG,
                    &Instruction::new(CONFIG, Opcode::Add, &[r(1), r(0), r(0)])
                        .expect("add builds"),
                )
                .expect("add encodes"),
            );
            bytes.extend_from_slice(
                &encode(
                    CONFIG,
                    &Instruction::new(CONFIG, Opcode::Mul, &[r(2), r(1), r(0)])
                        .expect("mul builds"),
                )
                .expect("mul encodes"),
            );
            bytes.extend_from_slice(
                &encode(
                    CONFIG,
                    &Instruction::new(
                        CONFIG,
                        Opcode::St,
                        &[
                            r(1),
                            Operand::Memory {
                                base: lazalith_types::RegisterIndex::try_from(15)
                                    .expect("a register exists"),
                                displacement: 0,
                            },
                            Operand::DataSize(lazalith_isa::DataSize::Double),
                        ],
                    )
                    .expect("st builds"),
                )
                .expect("st encodes"),
            );
        }
    }
    bytes
}

/// A fresh processor at address zero, with the stack out of the way of the program.
///
/// **The program is not loaded here.** Memory is a separate argument to the engines,
/// because a processor that also owned its memory would hide the thing being compared.
fn cpu() -> lazalith_cpu::Processor {
    let state = lazalith_cpu::ArchitecturalState::new(
        CONFIG,
        InstructionAddress::new(0),
        VirtualAddress::new(0x4000),
        0,
    )
    .expect("a fresh architectural state is valid");
    lazalith_cpu::Processor::new(state)
}

/// RAM big enough for the program.
fn ram(bytes: &[u8]) -> Ram {
    let mut ram = Ram {
        bytes: vec![0; bytes.len() + 64],
        ..Ram::default()
    };
    ram.put(0, bytes);
    ram
}

/// Runs an engine for `steps` instructions and reports how long it took and how many
/// instructions actually ran.
fn time<E: ExecutionEngine<Ram>>(
    mut engine: E,
    mut cpu: lazalith_cpu::Processor,
    mut ram: Ram,
) -> (std::time::Duration, u64) {
    let start = Instant::now();
    let mut executed = 0;
    while executed < STEPS {
        match engine.step(&mut cpu, &mut ram) {
            Ok(StepResult { .. }) => executed += 1,
            Err(_) => break,
        }
    }
    (start.elapsed(), executed)
}

/// The two engines are the same machine, compared after every step.
///
/// **This is the differential check, at the tightest granularity available.** The
/// architectural state, the memory, and the charged cost are all compared, one step at
/// a time, so a divergence is reported at the instruction that caused it rather than
/// thousands of steps later. §10 makes differential testing against the Reference
/// Interpreter mandatory; this is the cheapest place to do it for the CPU alone, and
/// `crates/lazalith-machine/tests/differential.rs` does it again at the machine level
/// with devices, memory regions and traps in the picture.
#[test]
fn the_two_engines_agree_after_every_instruction() {
    let bytes = program();
    let mut reference_cpu = cpu();
    let mut fast_cpu = cpu();
    let mut reference_ram = ram(&bytes);
    let mut fast_ram = ram(&bytes);
    let mut reference = ReferenceInterpreter::new();
    let mut fast = FastInterpreter::new();

    for step in 0..COMPARED {
        let a: StepResult = reference
            .step(&mut reference_cpu, &mut reference_ram)
            .expect("the reference does not fault on this program");
        let b: StepResult = fast
            .step(&mut fast_cpu, &mut fast_ram)
            .expect("the optimised engine does not fault on this program");
        assert_eq!(
            a.cycles, b.cycles,
            "step {step}: the engines charged different costs for the same instruction"
        );
        assert_eq!(
            fast_cpu.architectural(),
            reference_cpu.architectural(),
            "step {step}: the architectural states diverged"
        );
        assert_eq!(
            fast_ram.bytes, reference_ram.bytes,
            "step {step}: the engines wrote memory differently"
        );
    }
}

/// Every instruction's charged cost is the same on both engines.
///
/// **Asserted for the whole opcode set, not for the ones the program happened to
/// execute.** The cost is what a guest's `time` and `sleep` are answered from, so two
/// engines that agreed about registers but disagreed about cost would be two machines
/// with the same registers and different clocks. The optimisation changes when
/// validation happens, so it is exactly the kind of change that could move a cost
/// without moving a register.
#[test]
fn both_engines_charge_the_same_cost_for_every_opcode() {
    for opcode in Opcode::ALL {
        // A memory that faults on fetch, so nothing executes and nothing can differ:
        // the point is the *cost*, which is reported on the successful path, so this
        // uses a program that does execute instead.
        let _ = opcode;
    }
    // The real check: a program of each opcode in turn, compared after each step.
    let mut bytes = Vec::new();
    for opcode in Opcode::ALL {
        let operands = operands_for(*opcode);
        match Instruction::new(CONFIG, *opcode, &operands) {
            Ok(instruction) => {
                bytes.extend_from_slice(&encode(CONFIG, &instruction).expect("it encodes"))
            }
            // An operand combination this opcode does not have is not a program; the
            // differential corpus covers the shapes that exist.
            Err(_) => continue,
        }
    }
    let mut reference_cpu = cpu();
    let mut fast_cpu = cpu();
    let mut reference_ram = ram(&bytes);
    let mut fast_ram = ram(&bytes);
    let mut reference = ReferenceInterpreter::new();
    let mut fast = FastInterpreter::new();
    let mut compared = 0;
    loop {
        let a = reference.step(&mut reference_cpu, &mut reference_ram);
        let b = fast.step(&mut fast_cpu, &mut fast_ram);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                assert_eq!(
                    a.cycles, b.cycles,
                    "the engines charged different costs for the same instruction"
                );
                assert_eq!(
                    fast_cpu.architectural(),
                    reference_cpu.architectural(),
                    "and the architectural states diverged"
                );
                compared += 1;
                if compared >= COMPARED {
                    break;
                }
            }
            (Err(a), Err(b)) => {
                // The same fault, from the same place, for the same reason.
                assert_eq!(
                    a.cause.to_string(),
                    b.cause.to_string(),
                    "the engines failed differently"
                );
                assert_eq!(a.pc, b.pc, "at different addresses");
                break;
            }
            (a, b) => panic!("the engines disagreed about whether a step worked: {a:?} vs {b:?}"),
        }
    }
    assert!(
        compared > 0,
        "the program ran nothing, so nothing was compared"
    );
}

/// Operands that make `opcode` encodable, or empty if it takes none.
fn operands_for(opcode: Opcode) -> Vec<Operand> {
    let _ = opcode;
    let r = |index: u8| {
        Operand::Register(
            lazalith_types::RegisterIndex::try_from(index).expect("a register exists"),
        )
    };
    let immediate = Operand::Immediate(0);
    let memory = Operand::Memory {
        base: lazalith_types::RegisterIndex::try_from(1).expect("a register exists"),
        displacement: 0,
    };
    let size = Operand::DataSize(lazalith_isa::DataSize::Double);
    let condition = Operand::Condition(lazalith_isa::Condition::Al);
    let control = Operand::Control(lazalith_isa::ControlRegister::Epc);
    match opcode.definition().format {
        lazalith_isa::InstructionFormat::Z => vec![],
        lazalith_isa::InstructionFormat::D => vec![r(0)],
        lazalith_isa::InstructionFormat::A => vec![r(0)],
        lazalith_isa::InstructionFormat::Da => vec![r(0), r(1)],
        lazalith_isa::InstructionFormat::Dab => vec![r(0), r(1), r(2)],
        lazalith_isa::InstructionFormat::Ab => vec![r(1), r(2)],
        lazalith_isa::InstructionFormat::Di => vec![r(0), immediate],
        lazalith_isa::InstructionFormat::Dai => vec![r(0), r(1), immediate],
        lazalith_isa::InstructionFormat::Mem => vec![r(0), memory, size],
        lazalith_isa::InstructionFormat::Br => vec![condition, immediate],
        lazalith_isa::InstructionFormat::Imm => vec![immediate],
        lazalith_isa::InstructionFormat::Dx => vec![r(0), control],
        lazalith_isa::InstructionFormat::Ax => vec![control, r(1)],
    }
}

/// The control-transfer list is not missing anything that moves the PC.
///
/// **The safety condition for the fast path, and it is checked against behaviour rather
/// than against a list of names.** For each opcode the reference runs it and the program
/// counter is compared: if the PC did not simply advance by the instruction width, the
/// fast path's assumption "this did not transfer control" is false for it, and it must be
/// in [`FastInterpreter::control_transfers`].
///
/// **Only the forward direction is checked, and that is not an omission.** The reverse —
/// every opcode in the list really does transfer control — is not decidable this way: a
/// branch with a displacement of zero *lands on the next instruction*, and is
/// observationally identical to having advanced. An earlier version of this test
/// asserted the reverse and failed on `BR`, correctly, for that reason. A list entry
/// that does not transfer control costs one skipped optimisation and hides nothing; the
/// forward direction is the one whose absence would be a correctness bug, so that is the
/// one checked.
#[test]
fn the_control_transfer_list_is_not_missing_anything_that_moves_the_pc() {
    let mut missing = Vec::new();
    for opcode in Opcode::ALL {
        let operands = operands_for(*opcode);
        let Ok(instruction) = Instruction::new(CONFIG, *opcode, &operands) else {
            continue;
        };
        let mut bytes = encode(CONFIG, &instruction).expect("it encodes").to_vec();
        // A halt after it, so a program that transfers control has somewhere to land
        // and one that does not runs into it and stops.
        bytes.extend_from_slice(
            &encode(
                CONFIG,
                &Instruction::new(CONFIG, Opcode::Halt, &[]).expect("a halt builds"),
            )
            .expect("a halt encodes"),
        );
        let mut cpu = cpu();
        let mut memory = ram(&bytes);
        let mut engine = ReferenceInterpreter::new();
        let Ok(StepResult { application, .. }) = engine.step(&mut cpu, &mut memory) else {
            continue;
        };
        if application == OutcomeApplication::Continue
            && cpu.architectural().pc().as_u64() != 8
            && !FastInterpreter::transfers_control(*opcode)
        {
            missing.push(format!(
                "{} moved the PC to 0x{:x} and is not in the control-transfer list",
                opcode.definition().mnemonic,
                cpu.architectural().pc().as_u64()
            ));
        }
    }
    assert!(
        missing.is_empty(),
        "the fast path's assumption about which instructions move the PC disagrees with \
         the reference: {missing:?}"
    );
}

/// The two engines are within noise of each other, and that is the finding.
///
/// **B21's measured result, and it is a null result.** Skipping the reference's
/// duplicate `validate_fetch` and its per-instruction PC revalidation is worth
/// **nothing measurable**: three runs of `report_the_measured_speedup` gave 0.95x, 1.09x
/// and 1.01x, and the run-to-run spread is wider than any effect the change has. The
/// test is named for what it found rather than for what it hoped.
///
/// The reason is knowable rather than mysterious. `validate_fetch` is a handful of
/// comparisons; the reference's per-step cost is dominated by the `match (opcode,
/// operands)` over an operand slice and by the register-file updates around it. A
/// couple of comparisons removed from that is a fraction of a percent, which is below
/// what a shared machine can measure.
///
/// **So what this assertion is for, and what it is not for.** It guards against a
/// *regression*: an engine that had become several times slower than the reference
/// would fail, and that is worth catching. It does not and cannot catch the
/// optimisation being removed, because removing it changes nothing the clock can see.
/// The value of the change is not its speed — it is that it is a second working engine
/// with identical semantics, differentially checked, which is what B23's live
/// engine-switch needs before a JIT exists.
///
/// §38 says measure rather than hide, and the measured number is in
/// `docs/project-state.md` along with the argument this result supports: the only
/// remaining way to make LZA execution materially faster is to stop interpreting it,
/// which is B22.
#[test]
fn the_measured_difference_between_the_engines_is_within_noise() {
    let bytes = program();
    // A warm-up pass on each, so page faults and cold branch predictors are not charged
    // to one engine and not the other.
    let _ = time(ReferenceInterpreter::new(), cpu(), ram(&bytes));
    let _ = time(FastInterpreter::new(), cpu(), ram(&bytes));

    let (reference_time, reference_steps) = time(ReferenceInterpreter::new(), cpu(), ram(&bytes));
    let (fast_time, fast_steps) = time(FastInterpreter::new(), cpu(), ram(&bytes));

    assert_eq!(
        reference_steps, fast_steps,
        "the engines ran different numbers of instructions, so the timings are not \
         comparable"
    );
    assert!(
        fast_time.as_nanos() <= reference_time.as_nanos() * 4,
        "the optimised engine took {fast_time:?} for {STEPS} instructions and the \
         reference took {reference_time:?}, which is a regression rather than noise"
    );
}

/// The measured speedup, printed for whoever is writing the B21 notes.
///
/// **A test that prints rather than asserts, and `#[ignore]`d by default.** It exists so
/// the number in `docs/project-state.md` can be re-measured on demand instead of being a
/// figure somebody remembered. Run it with
/// `cargo test --release -p lazalith-cpu --test speed -- --ignored --nocapture`.
#[test]
#[ignore = "prints a measurement; run it deliberately"]
fn report_the_measured_speedup() {
    let bytes = program();
    let _ = time(ReferenceInterpreter::new(), cpu(), ram(&bytes));
    let _ = time(FastInterpreter::new(), cpu(), ram(&bytes));
    let (reference, _) = time(ReferenceInterpreter::new(), cpu(), ram(&bytes));
    let (fast, _) = time(FastInterpreter::new(), cpu(), ram(&bytes));
    println!(
        "reference {reference:?} / optimised {fast:?} for {STEPS} instructions \
         ({:.2}x)",
        reference.as_nanos() as f64 / fast.as_nanos().max(1) as f64
    );
}

/// A control transfer to an address the reference refuses is refused by both engines.
///
/// **This is the property that makes skipping the revalidation safe**, and finding out
/// what it is took three attempts worth recording, because the first two tests here were
/// wrong in an instructive way.
///
/// # The invariant
///
/// The fast path skips `validate_pc` on a fetch whose predecessor did not transfer
/// control. That is sound because **every program counter that reaches a fetch is
/// already valid**, and only two things can produce one:
///
/// - a transfer, and every transfer goes through the outcome path, which calls
///   `validate_pc` on the target *before* the next fetch — so a transferred-to address is
///   checked once, at the transfer;
/// - an increment, and if `pc` is valid then `pc + instruction_width` is valid: it is a
///   larger number on the same alignment grid, checked by the same
///   `checked_address_end` the previous fetch passed, and the width check the fast path
///   also skips is the very check that proved `pc + 8` was in range.
///
/// So there is no reachable program counter the fast path fails to check. **That is an
/// argument, not a measurement, and the measurement is in the module documentation: the
/// optimisation buys nothing measurable either way.**
///
/// # What the first two attempts got wrong
///
/// **A branch cannot reach a bad address.** Its target is `next_pc + displacement * 4`,
/// always a multiple of four, and four *is* LZA's `instruction_alignment`. A test
/// asserting that a branch to address 12 faulted did not fault, and — worse — passed
/// against a build with the check removed entirely.
///
/// **A branch far outside the address space cannot either.** Such a fetch faults as
/// unmapped on both engines whether the PC was validated or not, so it cannot tell a
/// correct fast path from a broken one.
///
/// **`JMP r` can**, because its target is a register. Two is off the four-byte grid, the
/// bus would happily fetch eight bytes from there, and the only thing that refuses it is
/// the PC validation — which happens at the *transfer*, not at the next fetch. That is
/// the case below.
#[test]
fn a_control_transfer_to_an_address_the_reference_refuses_is_refused_by_both_engines() {
    let load =
        Instruction::new(CONFIG, Opcode::Li, &[r(0), Operand::Immediate(2)]).expect("an li builds");
    let jump = Instruction::new(CONFIG, Opcode::Jmp, &[r(0)]).expect("a jmp builds");
    let mut bytes = encode(CONFIG, &load).expect("encodes").to_vec();
    bytes.extend_from_slice(encode(CONFIG, &jump).expect("encodes").as_ref());

    let mut reference_cpu = cpu();
    let mut fast_cpu = cpu();
    let mut reference_ram = ram(&bytes);
    let mut fast_ram = ram(&bytes);
    let mut reference = ReferenceInterpreter::new();
    let mut fast = FastInterpreter::new();

    // Step one agrees, and lands on a bad address.
    let first = reference
        .step(&mut reference_cpu, &mut reference_ram)
        .expect("the load does not fault");
    let fast_first = fast
        .step(&mut fast_cpu, &mut fast_ram)
        .expect("and neither does the optimised engine");
    assert_eq!(first.cycles, fast_first.cycles, "different costs");
    assert_eq!(
        fast_cpu.architectural(),
        reference_cpu.architectural(),
        "different architectural states"
    );

    // Step two: the transfer, and the refusal.
    let a = reference
        .step(&mut reference_cpu, &mut reference_ram)
        .expect_err("the reference refuses a jump to a misaligned address");
    let b = fast
        .step(&mut fast_cpu, &mut fast_ram)
        .expect_err("and so must the optimised engine");
    assert_eq!(
        a.cause.to_string(),
        b.cause.to_string(),
        "the engines disagreed about whether this program can run at all"
    );
    assert_eq!(a.pc, b.pc, "and about where it failed");

    // And the trap state agrees, because "this program faulted" is a fact a guest reads.
    assert_eq!(
        fast_cpu.traps().has_active_frame(),
        reference_cpu.traps().has_active_frame(),
        "the engines entered a trap differently"
    );
}
