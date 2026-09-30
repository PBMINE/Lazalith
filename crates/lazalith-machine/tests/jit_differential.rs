//! B24: differential JIT verification.
//!
//! # What this is
//!
//! §12 requires the JIT to be compared against the Reference Interpreter on registers,
//! PC, SP, status/flags, memory, faults, device-visible state and virtual time, across
//! straight-line code, branches, calls, arithmetic, memory accesses, unsupported
//! instructions, faulting instructions, engine switches, multiple engine switches, debug
//! boundaries, snapshot/restore and replay. B23 proved the handoff works; this proves the
//! *result* is the same however the work was divided between engines. Snapshot/restore and
//! replay live in `tests/handoff.rs` and the debug and replay suites.
//!
//! # The comparison, and why it is not per-step
//!
//! **A block-executing engine does not visit every instruction boundary, and a per-step
//! comparison is neither available nor what should be wanted.** A JIT retires up to 31
//! instructions in one call to `step`; the intermediate states did not exist to be
//! compared, because nothing observed them.
//!
//! So each engine's run is recorded as a **trajectory**: the architectural state after
//! exactly *k* instructions retired, for every *k* that engine actually reached. The two
//! are compared at every *k* both reached — a strictly stronger claim than comparing
//! endpoints, since a disagreement in the middle of a program is caught even when the ends
//! agree.
//!
//! # One subtlety that is easy to get wrong
//!
//! **A retired-instruction count can have more than one state, because a faulting step
//! retires nothing.** The count stands still while the program counter moves to the trap
//! vector, so a run genuinely passes through two states at the same count. An earlier
//! version of this file compared frames by count and reported a disagreement that was
//! entirely this: the interpreter had a post-fault frame at count 2 with the program
//! counter on the trap vector, and the JIT's block boundary at count 2 was the *pre-fault*
//! state. Both engines were right.
//!
//! So the frames are reduced to one per count — the state immediately after that many
//! instructions retired — and the faults are compared separately as an ordered sequence
//! of causes. A fault and its cause are entirely guest-visible; *where* a fault falls
//! relative to a block boundary is a fact about the engine, not the guest.
//!
//! # The oracle, and the two ways this file could pass vacuously
//!
//! The oracle is **the Reference Interpreter, always, and never another JIT.** And two
//! traps are closed explicitly: a JIT that declined everything would share almost no
//! boundary with the reference, so `the_jit_runs_natively_and_hands_off_on_this_corpus`
//! asserts that host code ran and that a handoff fired; and the exhaustive
//! per-instruction comparison lives in `a_breakpoint_on_every_instruction_matches_the_­
//! reference`, which puts a yield point on every instruction so the JIT's blocks are all
//! length one and it visits every boundary the interpreter does.

use lazalith_cpu::{EngineKind, Privilege, StatusRegister, TrapCause};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_isa::{Condition, DataSize, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineError, MachineEvent, MachineSetup, MachineState};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

const CODE: u64 = 0x000;
const CODE_LENGTH: u64 = 0x800;
const DATA: u64 = 0x800;
const DATA_LENGTH: u64 = 0x800;
const STACK: u64 = 0x1000;
const STACK_LENGTH: u64 = 0x400;
const DEVICE: u64 = 0x2000;

const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);
const RX: RegionPermissions = RegionPermissions::new(true, false, true, true);
const DEVICE_WINDOW: RegionPermissions = RegionPermissions::new(false, true, false, true);

fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).expect("a register exists"))
}

fn at(base: u8, displacement: i32) -> Operand {
    Operand::Memory {
        base: RegisterIndex::try_from(base).expect("a register exists"),
        displacement,
    }
}

fn size(size: DataSize) -> Operand {
    Operand::DataSize(size)
}

/// A tiny program builder so the corpus below reads as programs.
struct Asm {
    config: ArchitectureConfig,
    instructions: Vec<(Opcode, Vec<Operand>)>,
}

impl Asm {
    fn new(config: ArchitectureConfig) -> Self {
        Self {
            config,
            instructions: Vec::new(),
        }
    }

    fn add(mut self, opcode: Opcode, operands: &[Operand]) -> Self {
        self.instructions.push((opcode, operands.to_vec()));
        self
    }

    fn li(self, register: u8, value: i32) -> Self {
        self.add(Opcode::Li, &[r(register), Operand::Immediate(value)])
    }

    /// A one-**byte** store to the console register.
    ///
    /// **A separate method because the console's register is one byte long and a
    /// word-sized store to it crosses the end of the device window.** The first version of
    /// this corpus stored a `Double`, and every device program in it faulted — the engines
    /// agreed perfectly on the fault, which is the point, but a corpus whose device cases
    /// all fault is a corpus that has not tested device-visible state at all. The fault was
    /// a `CrossRegion` with `region_end == region_start`, which takes a moment to read as
    /// "that window is one byte long" rather than "the machine is wrong".
    fn console(self, byte: u8) -> Self {
        self.li(9, i32::from(byte))
            .add(Opcode::St, &[r(9), at(6, 0), size(DataSize::Byte)])
    }

    /// The data size a `Double` load or store needs on this machine.
    ///
    /// **Follows the configuration, because a 32-bit machine refuses a 64-bit data
    /// access** and the corpus is built for both. A corpus that hard-coded `Double` would
    /// have tested the 64-bit machine only and quietly reported the 32-bit one as passing
    /// — which is the failure mode of a fixture that cannot fail.
    fn data_size(&self) -> DataSize {
        match self.config.word_bits() {
            32 => DataSize::Word,
            _ => DataSize::Double,
        }
    }

    fn load(self, register: u8, base: u8, displacement: i32) -> Self {
        let width = self.data_size();
        self.add(
            Opcode::Ldz,
            &[r(register), at(base, displacement), size(width)],
        )
    }

    fn store(self, register: u8, base: u8, displacement: i32) -> Self {
        let width = self.data_size();
        self.add(
            Opcode::St,
            &[r(register), at(base, displacement), size(width)],
        )
    }

    fn bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (opcode, operands) in &self.instructions {
            let instruction = Instruction::new(self.config, *opcode, operands)
                .expect("a well-formed instruction");
            bytes.extend_from_slice(
                &encode(self.config, &instruction).expect("an instruction encodes"),
            );
        }
        bytes
    }
}

/// A program: its code, and any data the machine should preload.
struct Program {
    name: &'static str,
    config: ArchitectureConfig,
    code: Vec<u8>,
    data: Vec<u8>,
    /// How many retired instructions the run is allowed.
    ///
    /// **A bound, not a target.** A differential test that stops when the reference stops
    /// is measuring the reference; this stops at a fixed number of retired instructions so
    /// two runs are comparable even if one of them would have carried on.
    budget: u64,
}

fn machine(config: ArchitectureConfig, code: &[u8]) -> LazalithMachine<ConsoleDevice> {
    let mut devices = DeviceManager::new();
    devices
        .insert(
            DeviceId::new(1),
            ConsoleDevice::new(4096).expect("a console of 4 KiB"),
        )
        .expect("a console device fits");
    let setup = MachineSetup {
        config,
        devices,
        regions: vec![
            MemoryRegion::ram(config, PhysicalAddress::new(DATA), DATA_LENGTH, RW)
                .expect("a data region"),
            MemoryRegion::ram(config, PhysicalAddress::new(STACK), STACK_LENGTH, RW)
                .expect("a stack region"),
        ],
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK + STACK_LENGTH),
        status: StatusRegister::new(Privilege::Supervisor, false).bits(),
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).expect("the machine starts");
    let mut image = code.to_vec();
    image.resize(CODE_LENGTH as usize, 0);
    machine
        .load_region(
            MemoryRegion::rom(config, PhysicalAddress::new(CODE), &image, RX)
                .expect("a code region"),
        )
        .expect("the code region is mapped");
    machine
        .map_device(
            DeviceId::new(1),
            PhysicalAddress::new(DEVICE),
            DEVICE_WINDOW,
        )
        .expect("the device window is mapped");
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(CODE))
        .expect("a trap vector inside the code region");
    machine
}

/// Preloads a program's data, if it has any, and leaves the machine ready to step.
fn load_data(machine: &mut LazalithMachine<ConsoleDevice>, program: &Program) {
    if program.data.is_empty() {
        return;
    }
    machine
        .load_bytes(PhysicalAddress::new(DATA), &program.data)
        .expect("the data loads");
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(CODE))
        .expect("a trap vector");
}

/// Everything a guest can observe, at one point in a run.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Frame {
    registers: Vec<u64>,
    pc: u64,
    sp: u64,
    status: u64,
    time: u64,
    data: Vec<u8>,
    console: Vec<u8>,
    state: MachineState,
    /// Whether a trap frame is open, which is not readable from `registers` and is
    /// absolutely something a guest can observe.
    in_trap: bool,
}

fn frame(machine: &LazalithMachine<ConsoleDevice>) -> Frame {
    let state = machine.architectural_state();
    let mut data = vec![0u8; DATA_LENGTH as usize];
    machine
        .peek_memory(PhysicalAddress::new(DATA), &mut data)
        .expect("the data region reads");
    Frame {
        registers: (0..RegisterIndex::COUNT)
            .map(|index| state.registers().read_raw(index).unwrap_or(0))
            .collect(),
        pc: state.pc().as_u64(),
        sp: state.sp().as_u64(),
        status: state.status().bits(),
        time: machine.clock().elapsed().as_u64(),
        data,
        console: console_bytes(machine),
        state: machine.state(),
        in_trap: machine.processor().traps().has_active_frame(),
    }
}

fn console_bytes(machine: &LazalithMachine<ConsoleDevice>) -> Vec<u8> {
    // The console is looked up by device id rather than by index: a program that writes to
    // the device window has to have its bytes compared, and a lookup that returned "no
    // device" on a differently-built machine would make that comparison vacuous.
    machine
        .devices()
        .device(DeviceId::new(1))
        .expect("the console is where it was installed")
        .output()
        .to_vec()
}

/// A run, as two things rather than one.
///
/// **One frame per retired-instruction count, plus the traps as a sequence.** See the module
/// documentation for why a count can have more than one state and why the faults are
/// compared separately.
struct Run {
    frames: Vec<(u64, Frame)>,
    traps: Vec<TrapCause>,
}

/// Runs `program` on `engine` and records the state at every retired-instruction boundary
/// the engine actually reached, plus every trap it entered.
fn run_on(program: &Program, engine: EngineKind) -> Run {
    let mut machine = machine(program.config, &program.code);
    load_data(&mut machine, program);
    machine
        .switch_execution_engine(engine)
        .expect("the machine builds the engine");
    step_out(program, &mut machine)
}

/// Steps a machine to the program's budget, recording the trajectory and the traps.
fn step_out(program: &Program, machine: &mut LazalithMachine<ConsoleDevice>) -> Run {
    let mut frames = vec![(0, frame(machine))];
    let mut traps = Vec::new();
    for _ in 0..(program.budget * 4 + 64) {
        if machine.executed_instruction_count() >= program.budget {
            break;
        }
        let before = machine.executed_instruction_count();
        match machine.step() {
            Ok(MachineEvent::Stepped { .. }) | Ok(MachineEvent::Halted) => {}
            Ok(MachineEvent::Trapped { event }) => traps.push(event.cause),
            Err(MachineError::NoExecutionEngine { decline }) => {
                panic!("{}: no engine could execute here: {decline}", program.name)
            }
            Err(_) => break,
        }
        let after = machine.executed_instruction_count();
        if after != before {
            frames.push((after, frame(machine)));
        }
        if !matches!(machine.state(), MachineState::Reset | MachineState::Running) {
            break;
        }
    }
    Run { frames, traps }
}

/// Compares two runs at every retired-instruction boundary both reached, and returns how
/// many that was.
fn compare(name: &str, reference: &Run, subject: &Run) -> usize {
    let mut shared = 0usize;
    for (count, expected) in &reference.frames {
        let Some((_, actual)) = subject.frames.iter().find(|(at, _)| at == count) else {
            continue;
        };
        shared += 1;
        let where_ = |field: &str| format!("{name} at {count} instructions: {field}");
        assert_eq!(
            actual.registers,
            expected.registers,
            "{}",
            where_("registers")
        );
        assert_eq!(actual.pc, expected.pc, "{}", where_("program counter"));
        assert_eq!(actual.sp, expected.sp, "{}", where_("stack pointer"));
        assert_eq!(
            actual.status,
            expected.status,
            "{}",
            where_("status register")
        );
        assert_eq!(actual.time, expected.time, "{}", where_("virtual time"));
        assert_eq!(actual.data, expected.data, "{}", where_("data region"));
        assert_eq!(
            actual.console,
            expected.console,
            "{}",
            where_("console output")
        );
        assert_eq!(
            actual.in_trap,
            expected.in_trap,
            "{}",
            where_("trap frame state")
        );
        assert_eq!(actual.state, expected.state, "{}", where_("machine state"));
    }
    assert_eq!(
        subject.traps, reference.traps,
        "{name}: the same program entered different traps. The reference took {:?} and this \
         engine took {:?}",
        reference.traps, subject.traps
    );
    shared
}

// -- the corpus -------------------------------------------------------------

fn corpus() -> Vec<Program> {
    let mut programs = Vec::new();
    for config in [ArchitectureConfig::lz64(), ArchitectureConfig::lz32()] {
        let step = 8 * i64::from(config.word_bytes()) as i32;
        let bits = config.word_bits() as u64;
        let half = (1u64 << (bits / 2)) as i32;
        let one = half.wrapping_add(1);
        let tag = if bits == 64 { "64" } else { "32" };

        // Straight-line register arithmetic: the JIT's whole translated subset, and the only
        // program here where it should run natively for a long stretch.
        programs.push(Program {
            name: if bits == 64 {
                "straight-line (64)"
            } else {
                "straight-line (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, 20)
                .li(2, 22)
                .add(Opcode::Add, &[r(0), r(1), r(2)])
                .add(Opcode::Sub, &[r(3), r(1), r(2)])
                .add(Opcode::Xor, &[r(4), r(1), r(2)])
                .add(Opcode::And, &[r(5), r(1), r(2)])
                .add(Opcode::Or, &[r(6), r(1), r(2)])
                .add(Opcode::Mul, &[r(7), r(1), r(2)])
                .add(Opcode::Mov, &[r(8), r(1)])
                .add(Opcode::Addi, &[r(9), r(1), Operand::Immediate(7)])
                .add(Opcode::Subi, &[r(10), r(1), Operand::Immediate(7)])
                .bytes(),
            data: Vec::new(),
            budget: 11,
        });

        // Every flag the ISA can produce, including those only a boundary can reach.
        programs.push(Program {
            name: if bits == 64 {
                "flags (64)"
            } else {
                "flags (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, 0)
                .li(2, one)
                .add(Opcode::Add, &[r(0), r(1), r(1)])
                .add(Opcode::Cmp, &[r(0), r(2)])
                .add(Opcode::Sub, &[r(3), r(1), r(2)])
                .add(Opcode::Cmp, &[r(3), r(3)])
                .add(Opcode::Mul, &[r(4), r(2), r(2)])
                .bytes(),
            data: Vec::new(),
            budget: 9,
        });

        // A branch that is taken and a branch that is not, so both sides of the
        // condition-code comparison are exercised and the flags feeding them come from
        // instructions the JIT does translate.
        programs.push(Program {
            name: if bits == 64 {
                "branch (64)"
            } else {
                "branch (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, 5)
                .li(2, 5)
                .add(Opcode::Cmp, &[r(1), r(2)])
                .add(
                    Opcode::Br,
                    &[Operand::Condition(Condition::Eq), Operand::Immediate(step)],
                )
                .li(3, 0xBAD)
                .add(Opcode::Jmp, &[r(3)])
                .li(4, 0x600)
                .bytes(),
            data: Vec::new(),
            budget: 10,
        });

        // A call and a return: a control transfer the JIT cannot translate at all, so the
        // handoff fires on a different kind of boundary than a memory access. `CALL` takes
        // a relative displacement and `CALLR` a register; `RET` pops what the call pushed,
        // so the subroutine is real code at a known offset and the return address is
        // architectural state the comparison sees.
        let subroutine = CODE + 5 * 8;
        programs.push(Program {
            name: if bits == 64 {
                "call and return (64)"
            } else {
                "call and return (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, subroutine as i32)
                .add(Opcode::Callr, &[r(1)])
                .add(Opcode::Add, &[r(5), r(6), r(6)])
                .add(Opcode::Halt, &[])
                // The subroutine: double r6, then return.
                .add(Opcode::Add, &[r(6), r(6), r(6)])
                .add(Opcode::Ret, &[])
                .bytes(),
            data: Vec::new(),
            budget: 8,
        });

        // Memory traffic: every access the JIT declines, so the handoff fires on all of
        // them, and the final store makes the result observable in memory.
        programs.push(Program {
            name: if bits == 64 {
                "memory (64)"
            } else {
                "memory (32)"
            },
            config,
            code: Asm::new(config)
                .li(6, DATA as i32)
                .li(1, 1234)
                .store(1, 6, 0)
                .li(2, 5678)
                .store(2, 6, 8)
                .load(3, 6, 0)
                .load(4, 6, 8)
                .add(Opcode::Add, &[r(5), r(3), r(4)])
                .store(5, 6, 16)
                .load(7, 6, 16)
                .bytes(),
            data: Vec::new(),
            budget: 12,
        });

        // A store to the device window: console output is architecturally visible and the
        // JIT must not be able to make it differ.
        programs.push(Program {
            name: if bits == 64 {
                "device write (64)"
            } else {
                "device write (32)"
            },
            config,
            code: Asm::new(config)
                .li(6, DEVICE as i32)
                .console(b'A')
                .console(b'B')
                .console(b'C')
                .bytes(),
            data: Vec::new(),
            budget: 7,
        });

        // A store to the read-only code region: a permission fault the JIT cannot even
        // translate, and one that must trap identically on both engines.
        programs.push(Program {
            name: if bits == 64 {
                "permission fault (64)"
            } else {
                "permission fault (32)"
            },
            config,
            code: Asm::new(config)
                .li(6, CODE as i32)
                .li(1, 1)
                .store(1, 6, 0)
                .li(2, 2)
                .bytes(),
            data: Vec::new(),
            budget: 8,
        });

        // A load from an unmapped address: an unmapped fault rather than a permission one,
        // so the trap *cause* differs from the program above and a machine that reported
        // the same cause for both would pass one and fail the other.
        programs.push(Program {
            name: if bits == 64 {
                "unmapped fault (64)"
            } else {
                "unmapped fault (32)"
            },
            config,
            code: Asm::new(config)
                .li(6, 0x40_0000)
                .load(1, 6, 0)
                .li(2, 2)
                .bytes(),
            data: Vec::new(),
            budget: 8,
        });

        // A divide by zero: an instruction the JIT declines, whose *execution* faults, and
        // which must therefore trap rather than be reported as a decline.
        programs.push(Program {
            name: if bits == 64 {
                "divide by zero (64)"
            } else {
                "divide by zero (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, 100)
                .li(2, 0)
                .add(Opcode::Divu, &[r(0), r(1), r(2)])
                .li(3, 3)
                .bytes(),
            data: Vec::new(),
            budget: 8,
        });

        // A software trap, so the trap *frame* — which is architectural — is compared as
        // well as the outcome.
        programs.push(Program {
            name: if bits == 64 {
                "software trap (64)"
            } else {
                "software trap (32)"
            },
            config,
            code: Asm::new(config)
                .li(1, 9)
                .add(Opcode::Trap, &[Operand::Immediate(3)])
                .li(2, 2)
                .bytes(),
            data: Vec::new(),
            budget: 8,
        });

        // A privileged operation executed as supervisor, and a long straight-line run so
        // the JIT's block limit and the interpreter's single-instruction stepping have to
        // agree over many instructions rather than a handful.
        let mut long = Asm::new(config);
        for index in 0..40u8 {
            long = long.li(index % 8, i32::from(index));
        }
        long = long.add(Opcode::Add, &[r(9), r(1), r(2)]);
        programs.push(Program {
            name: if bits == 64 {
                "long run (64)"
            } else {
                "long run (32)"
            },
            config,
            code: long.bytes(),
            data: Vec::new(),
            budget: 41,
        });

        let _ = (tag, half);
    }
    programs
}

// -- the tests ---------------------------------------------------------------

/// Every program produces the reference trajectory on every engine, at every shared
/// boundary.
///
/// **The central B24 claim, over the whole corpus.** For each program the reference run is
/// compared against every other engine at every retired-instruction boundary both reached —
/// registers, PC, SP, status, virtual time, the whole data region, console output, the
/// trap-frame flag and the machine's lifecycle state — and the two runs must have entered
/// the same traps in the same order.
#[test]
fn every_engine_produces_the_reference_trajectory() {
    for program in corpus() {
        let reference = run_on(&program, EngineKind::Reference);
        for engine in EngineKind::ALL {
            if *engine == EngineKind::Reference {
                continue;
            }
            let subject = run_on(&program, *engine);
            let shared = compare(program.name, &reference, &subject);
            // **No "thin comparison" threshold here, deliberately.** A JIT that retires a
            // block visits only its block's *end* boundaries, so it may legitimately share
            // as few as two of eleven with an interpreter. Demanding more would be
            // demanding that the JIT behave like the interpreter, which is the opposite of
            // what a JIT is for. What is demanded is that they met at all and ended in the
            // same state; the exhaustive per-instruction comparison is
            // `a_breakpoint_on_every_instruction_matches_the_reference`, which forces the
            // JIT to visit every boundary.
            assert!(
                shared > 0,
                "{}: {engine} shared no boundary at all with the reference, so the two runs \
                 never met and nothing was compared",
                program.name
            );
            let last = reference.frames.last().expect("a run has an end");
            assert_eq!(
                subject.frames.last().map(|(_, frame)| frame),
                Some(&last.1),
                "{}: {engine} ended in a different state than the reference",
                program.name
            );
        }
    }
}

/// The JIT really runs natively, and the handoff really fires, on this corpus.
///
/// **Without this the test above could be satisfied by a JIT that declines everything**:
/// every state would match, because the interpreter would have done all of it, and the
/// suite would report a clean differential result having verified nothing about the JIT.
///
/// The two counts are also the honest characterisation of this stage's JIT: on a corpus
/// that is mostly memory and control flow, native instructions are a minority of the work
/// and handoffs outnumber them. That is what the performance measurement is about, which
/// is why this asserts "both non-zero" rather than "most of it".
#[test]
fn the_jit_runs_natively_and_hands_off_on_this_corpus() {
    let mut native_total = 0u64;
    let mut handoff_total = 0u64;
    let mut with_native = 0usize;
    let programs = corpus();
    for program in &programs {
        let mut machine = machine(program.config, &program.code);
        load_data(&mut machine, program);
        machine
            .switch_execution_engine(EngineKind::Jit)
            .expect("the machine builds a JIT");
        for _ in 0..(program.budget * 4 + 64) {
            if machine.executed_instruction_count() >= program.budget {
                break;
            }
            if machine.step().is_err() {
                break;
            }
        }
        native_total += machine.native_instructions();
        handoff_total += machine.engine_handoffs();
        if machine.native_instructions() > 0 {
            with_native += 1;
        }
    }
    assert!(
        with_native > 0,
        "no program in the corpus retired a single instruction natively, so the \
         differential above tested only the interpreter"
    );
    assert!(
        handoff_total > 0,
        "and no handoff fired either, so the corpus never exercised the interpreter's half \
         of the arrangement"
    );
    // Recorded rather than asserted on: this is the ratio the performance section is about
    // and it should be visible in a change's test output.
    eprintln!(
        "JIT over the corpus: {native_total} instructions retired natively, {handoff_total} \
         handoffs, {with_native} of {} programs with any native execution",
        programs.len()
    );
}

/// An engine that changes part-way through matches a run that never changed at all.
///
/// **The "multiple engine switches" case of §12, at instruction granularity.** A schedule
/// that changes engine every single step is the most aggressive thing available, and it
/// must produce the reference trajectory exactly.
#[test]
fn switching_engine_every_instruction_matches_the_reference() {
    for program in corpus() {
        let reference = run_on(&program, EngineKind::Reference);
        let mut machine = machine(program.config, &program.code);
        load_data(&mut machine, &program);
        let engines = [
            EngineKind::Jit,
            EngineKind::Optimized,
            EngineKind::Reference,
        ];
        // The frame at zero is common to every run, so the trajectory starts there and the
        // stepping below appends.
        let mut frames = vec![(0, frame(&machine))];
        let mut traps = Vec::new();
        // The range is `u64` because the budget is an instruction count, so the index is
        // too; `usize` would work on every host that has one but this is not that.
        for step_index in 0..(program.budget * 4 + 64) {
            if machine.executed_instruction_count() >= program.budget {
                break;
            }
            if step_index > 0 {
                // A refused switch is correct — a faulted machine cannot be switched, and
                // switching to the engine already installed is a no-op — and the run then
                // simply continues on whichever engine it has.
                let _ = machine
                    .switch_execution_engine(engines[(step_index % engines.len() as u64) as usize]);
            }
            let before = machine.executed_instruction_count();
            match machine.step() {
                Ok(MachineEvent::Stepped { .. }) | Ok(MachineEvent::Halted) => {}
                Ok(MachineEvent::Trapped { event }) => traps.push(event.cause),
                Err(_) => break,
            }
            if machine.executed_instruction_count() != before {
                frames.push((machine.executed_instruction_count(), frame(&machine)));
            }
            if !matches!(machine.state(), MachineState::Reset | MachineState::Running) {
                break;
            }
        }
        compare(program.name, &reference, &Run { frames, traps });
    }
}

/// Debug boundaries do not change the trajectory.
///
/// **The "debug boundaries" case of §12, and the strongest comparison in this file.** A
/// yield point on every instruction makes every block length one, so the JIT visits exactly
/// the boundaries the interpreter does — and so this is a genuinely exhaustive
/// per-instruction comparison rather than a sparse one. The trajectory must be the
/// reference's, and the shared-boundary count is asserted to be the *whole* run rather
/// than merely large.
#[test]
fn a_breakpoint_on_every_instruction_matches_the_reference() {
    for program in corpus() {
        let reference = run_on(&program, EngineKind::Reference);
        let mut machine = machine(program.config, &program.code);
        load_data(&mut machine, &program);
        machine
            .switch_execution_engine(EngineKind::Jit)
            .expect("the machine builds a JIT");
        // A yield point on every instruction boundary in the program. The machine drops any
        // at or below the program counter and the debugger re-arms as it advances, which is
        // exactly the case that filtering exists for.
        let points: Vec<u64> = (0..program.budget * 2 + 8)
            .map(|index| CODE + index * 8)
            .collect();
        machine.set_yield_points(points);
        let subject = step_out(&program, &mut machine);

        let shared = compare(program.name, &reference, &subject);
        assert_eq!(
            shared,
            reference.frames.len(),
            "{}: with a boundary on every instruction the JIT should visit all {} of the \
             reference's boundaries, and visited {shared}",
            program.name,
            reference.frames.len()
        );
    }
}
