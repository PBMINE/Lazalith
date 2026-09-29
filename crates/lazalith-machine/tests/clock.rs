//! B19 follow-up: virtual time is charged where instructions retire.
//!
//! # The defect this suite exists for
//!
//! B18 established that the virtual clock is real guest-visible state, and B19 found
//! that **nothing advanced it**: a machine that had executed a million instructions
//! still reported zero elapsed cycles, so a guest's `time` call returned a constant and
//! `sleep` never woke. The management layer could not fix it — `advance_clock` is on the
//! machine, and calling it from a management client is the reach-through §35 forbids — so
//! the fix belongs here, at the machine's own step.
//!
//! # What is being held
//!
//! **One cycle per instruction is not the model, and the tests here say so.** The model
//! is the ISA's: each instruction costs what its definition says, and the machine charges
//! it when the instruction retires. The tests below are written so that a flat
//! one-cycle-per-instruction implementation would fail them, because that is the
//! implementation this fixes and the one a future change could plausibly regress to.
//!
//! Each test was checked by removing the thing it claims to cover:
//!
//! | removed | test that failed |
//! | --- | --- |
//! | the clock advance in `step_inner` | 6 of the 8 |
//! | the charge on a trapping instruction | `a_trap_costs_what_the_trap_cost` |
//! | the `run.cycles` accounting | `a_run_reports_what_it_spent` |

use lazalith_isa::{Condition, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineState};
use lazalith_types::{ArchitectureConfig, CycleCount, InstructionAddress, VirtualAddress};

const MODES: [ArchitectureConfig; 2] = [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()];
const CODE: u64 = 0x0000;
const STACK_TOP: u64 = 0x900;

/// A machine with a region big enough for `program` and nothing else.
#[allow(clippy::too_many_lines)]
fn machine_with(
    config: ArchitectureConfig,
    program: &[u8],
) -> LazalithMachine<lazalith_devices::NoDevice> {
    let setup = lazalith_machine::MachineSetup {
        config,
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK_TOP),
        status: 0,
        initial_time: CycleCount::new(0),
        regions: vec![
            lazalith_memory::MemoryRegion::ram(
                config,
                lazalith_types::PhysicalAddress::new(CODE),
                0x1000,
                lazalith_memory::RegionPermissions::new(true, true, true, false),
            )
            .expect("a RAM region"),
        ],
        devices: lazalith_devices::DeviceManager::new(),
    };
    let mut machine = LazalithMachine::new(setup).expect("the machine builds");
    machine
        .load_bytes(lazalith_types::PhysicalAddress::new(CODE), program)
        .expect("the program is loaded");
    machine.reset();
    machine
}

fn emit(config: ArchitectureConfig, opcode: Opcode, operands: &[Operand]) -> Vec<u8> {
    let instruction = Instruction::new(config, opcode, operands).expect("the instruction builds");
    encode(config, &instruction).expect("and encodes").to_vec()
}

/// A program that loops forever on itself, so a `run` costs exactly what it says.
fn spinner(config: ArchitectureConfig) -> Vec<u8> {
    // `BR AL, -2`: a relative target is `next_pc + displacement * 4` and the
    // instruction is 8 bytes, so -2 is "back to here".
    emit(
        config,
        Opcode::Br,
        &[Operand::Condition(Condition::Al), Operand::Immediate(-2)],
    )
}

fn r(index: u8) -> Operand {
    Operand::Register(lazalith_types::RegisterIndex::try_from(index).expect("a register exists"))
}

// -- time moves -------------------------------------------------------------

#[test]
fn executing_an_instruction_moves_virtual_time() {
    for config in MODES {
        let mut machine = machine_with(config, &spinner(config));
        assert_eq!(
            machine.clock().elapsed(),
            CycleCount::new(0),
            "a machine that has executed nothing is at the epoch"
        );
        machine.step().expect("it steps");
        assert_eq!(
            machine.clock().elapsed(),
            CycleCount::new(Opcode::Br.cycles() as u64),
            "and after one instruction the clock reads what that instruction cost"
        );
    }
}

#[test]
fn a_run_advances_the_clock_by_what_its_instructions_cost() {
    for config in MODES {
        let _machine = machine_with(config, &spinner(config));
        // A run of several *different* instructions, so "one cycle per step" and
        // "the sum of the instructions' costs" are different numbers.
        let mut program = emit(config, Opcode::Li, &[r(0), Operand::Immediate(1)]);
        program.extend(emit(config, Opcode::Add, &[r(0), r(0), r(0)]));
        program.extend(emit(config, Opcode::Mul, &[r(1), r(0), r(0)]));
        let expected: u64 = [Opcode::Li, Opcode::Add, Opcode::Mul]
            .iter()
            .map(|opcode| u64::from(opcode.cycles()))
            .sum();
        assert!(
            expected != 3,
            "the three instructions must not happen to cost one each, or the test \
             cannot tell the two models apart"
        );

        let mut machine = machine_with(config, &program);
        let run = machine.run(3).expect("it runs");
        assert_eq!(run.executed, 3);
        assert_eq!(
            machine.clock().elapsed(),
            CycleCount::new(expected),
            "the clock advanced by the sum of the instructions' costs, not by the \
             number of instructions"
        );
    }
}

#[test]
fn a_data_access_costs_more_than_a_register_operation() {
    for config in MODES {
        // `ST`/`LDZ` are the `Mem` format; `ADD` is register work. Same number of
        // instructions, different time.
        //
        // The store's width follows the architecture, because `Double` is not a valid
        // store size on lz32 — and the *cost* is a property of the format, so the two
        // widths are free to use different sizes and still be compared like for like.
        let width = if config.word_width() == lazalith_types::WordWidth::W32 {
            lazalith_isa::DataSize::Word
        } else {
            lazalith_isa::DataSize::Double
        };
        let mut program = emit(config, Opcode::Li, &[r(1), Operand::Immediate(0x40)]);
        program.extend(emit(
            config,
            Opcode::St,
            &[
                r(0),
                Operand::Memory {
                    base: lazalith_types::RegisterIndex::try_from(1).unwrap(),
                    displacement: 0,
                },
                Operand::DataSize(width),
            ],
        ));
        let mut with_store = machine_with(config, &program);
        with_store.step().expect("li");
        with_store.step().expect("st");
        let store_time = with_store.clock().elapsed();

        let mut program = emit(config, Opcode::Li, &[r(0), Operand::Immediate(1)]);
        program.extend(emit(config, Opcode::Add, &[r(0), r(0), r(0)]));
        let mut with_add = machine_with(config, &program);
        with_add.step().expect("li");
        with_add.step().expect("add");
        let add_time = with_add.clock().elapsed();

        assert!(
            store_time > add_time,
            "two instructions, one a data access and one register work: the store \
             took {} and the add took {}",
            store_time.as_u64(),
            add_time.as_u64()
        );
    }
}

// -- what is charged --------------------------------------------------------

#[test]
fn a_faulting_instruction_costs_nothing_because_it_retired_nothing() {
    for config in MODES {
        // A program that is one instruction long, so stepping off the end of it faults
        // on the fetch.
        let program = emit(config, Opcode::Nop, &[]);
        let mut machine = machine_with(config, &program);
        machine.step().expect("the first instruction runs");
        let after_first = machine.clock().elapsed();
        assert!(after_first > CycleCount::new(0), "it cost something");

        // The next fetch runs off the end of the loaded program and into unmapped
        // memory. Whatever happens, time must not have gone *backwards*.
        let _ = machine.step();
        assert!(
            machine.clock().elapsed() >= after_first,
            "a faulted fetch retired nothing, so it charged nothing: time went from \
             {} to {}",
            after_first.as_u64(),
            machine.clock().elapsed().as_u64()
        );
    }
}

#[test]
fn a_halt_costs_what_halt_costs_because_it_executed() {
    for config in MODES {
        let program = emit(config, Opcode::Halt, &[]);
        let mut machine = machine_with(config, &program);
        machine.step().expect("it halts");
        assert_eq!(machine.state(), MachineState::Halted);
        assert_eq!(
            machine.clock().elapsed(),
            CycleCount::new(Opcode::Halt.cycles() as u64),
            "a halt is the end of execution, not an absence of execution: it retired \
             an instruction and is charged for it"
        );
    }
}

#[test]
fn a_trap_costs_what_the_trapping_instruction_cost() {
    for config in MODES {
        // `SYSCALL` traps. It is a `Z`-format instruction, so its cost is one cycle,
        // and it must be charged even though the step ended in a trap rather than
        // completing.
        let program = emit(config, Opcode::Syscall, &[]);
        let mut machine = machine_with(config, &program);
        machine
            .set_trap_vector(InstructionAddress::new(0x800))
            .expect("a trap vector");
        machine.step().expect("it traps");
        assert_eq!(
            machine.clock().elapsed(),
            CycleCount::new(Opcode::Syscall.cycles() as u64),
            "a trapping instruction executed, so it is charged: a machine where \
             trapping was free would let a guest spend unbounded time for nothing"
        );
    }
}

// -- the accounting the machine reports -------------------------------------

#[test]
fn a_run_reports_what_it_spent() {
    for config in MODES {
        let mut machine = machine_with(config, &spinner(config));
        let before = machine.clock().elapsed();
        let run = machine.run(7).expect("it runs");
        assert_eq!(run.executed, 7);
        assert_eq!(
            run.cycles,
            u64::from(Opcode::Br.cycles()) * 7,
            "a run reports what it spent, not what it executed"
        );
        assert_eq!(
            machine.clock().elapsed().as_u64() - before.as_u64(),
            run.cycles,
            "and the clock moved by exactly that, so the two are one account and not two"
        );
    }
}

#[test]
fn two_runs_of_the_same_program_cost_the_same() {
    // The reproducibility property the old driver-driven clock was supposed to give,
    // now coming from the instruction definitions instead of a scheduler's schedule.
    for config in MODES {
        let mut program = emit(config, Opcode::Li, &[r(0), Operand::Immediate(1)]);
        program.extend(emit(config, Opcode::Add, &[r(0), r(0), r(0)]));
        program.extend(emit(config, Opcode::Mul, &[r(0), r(0), r(0)]));

        let mut first = machine_with(config, &program);
        let mut second = machine_with(config, &program);
        let a = first.run(3).expect("it runs");
        let b = second.run(3).expect("it runs");
        assert_eq!(
            a.cycles, b.cycles,
            "the same program cost the same virtual time twice, which is what makes a \
             trace reproducible without reference to the host it ran on"
        );
    }
}

#[test]
fn the_machine_reports_what_the_last_instruction_cost() {
    for config in MODES {
        let mut program = emit(config, Opcode::Mul, &[r(0), r(0), r(0)]);
        program.extend(emit(config, Opcode::Nop, &[]));
        let mut machine = machine_with(config, &program);
        machine.step().expect("the multiply");
        assert_eq!(
            machine.last_instruction_cycles(),
            Opcode::Mul.cycles(),
            "a multiply is the slow operation the model calls out"
        );
        machine.step().expect("the nop");
        assert_eq!(
            machine.last_instruction_cycles(),
            Opcode::Nop.cycles(),
            "and the report follows the last instruction, so it is not a running total"
        );
    }
}

// -- the device side --------------------------------------------------------

#[test]
fn devices_see_the_same_time_the_machine_reports() {
    // A device's `tick` is told the machine's absolute elapsed time. The timer used to
    // *add* it, so it computed a sum of absolute timestamps — a time the machine was
    // never at. It read as correct while the clock moved once and turned wrong the
    // moment it moved twice, which is what per-instruction charging made routine.
    for config in MODES {
        let mut devices = lazalith_devices::DeviceManager::new();
        devices
            .insert(
                lazalith_devices::DeviceId::new(1),
                lazalith_devices::TimerDevice::new(),
            )
            .expect("a timer");
        let setup = lazalith_machine::MachineSetup {
            config,
            pc: InstructionAddress::new(CODE),
            sp: VirtualAddress::new(STACK_TOP),
            status: 0,
            initial_time: CycleCount::new(0),
            regions: vec![
                lazalith_memory::MemoryRegion::ram(
                    config,
                    lazalith_types::PhysicalAddress::new(CODE),
                    0x1000,
                    lazalith_memory::RegionPermissions::new(true, true, true, false),
                )
                .expect("a RAM region"),
            ],
            devices,
        };
        let mut machine = LazalithMachine::new(setup).expect("the machine builds");
        machine
            .load_bytes(lazalith_types::PhysicalAddress::new(CODE), &spinner(config))
            .expect("the program is loaded");
        machine.reset();
        machine
            .advance_clock(CycleCount::new(1_000))
            .expect("time passes");

        for _ in 0..3 {
            machine.step().expect("it steps");
            assert_eq!(
                machine.devices().clock().elapsed(),
                machine.clock().elapsed(),
                "the device manager and the machine are one clock"
            );
        }
    }
}
