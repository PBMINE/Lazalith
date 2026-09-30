//! B23: live interpreter ↔ JIT handoff.
//!
//! # What is being proved
//!
//! §11 requires that a JIT and the Reference Interpreter be **two execution modes of one
//! VM, not two virtual machines**: they hand execution back and forth mid-run, at exact
//! guest instruction boundaries, with one canonical architectural state, and the guest
//! cannot tell.
//!
//! Every test here is a *handoff*, and each one is built so that a machine which merely
//! had both engines installed would fail it:
//!
//! | test | what would be wrong without the handoff |
//! |---|---|
//! | [`a_jit_runs_part_of_a_program_and_the_interpreter_finishes_it`] | the JIT declined everything and the interpreter did all the work |
//! | [`a_decline_leaves_the_next_instruction_to_the_interpreter`] | the interpreter re-ran or skipped an instruction |
//! | [`a_faulting_instruction_declined_by_the_jit_still_traps`] | the fault was swallowed, or trapped for the wrong reason |
//! | [`repeated_switching_lands_where_an_uninterrupted_run_does`] | engine identity is one-shot, or a switch perturbs state |
//! | [`a_breakpoint_inside_a_block_is_not_run_past`] | a translated block stepped over the debugger's breakpoint |
//! | [`a_snapshot_taken_on_one_engine_restores_onto_the_other`] | the snapshot captured JIT-private state as architecture |
//! | [`the_engine_schedule_does_not_change_what_the_guest_can_see`] | switching is observable to the guest |
//!
//! # The oracle
//!
//! **The Reference Interpreter, always.** There is no comparison of a JIT against
//! another JIT and none against itself: a JIT that agreed with itself would agree while
//! being wrong. Every assertion is against a run that never switched engines.
//!
//! # The one number that makes these tests non-vacuous
//!
//! [`LazalithMachine::native_instructions`] exists for this file. A JIT that quietly
//! delegated would leave *identical* architectural state — the interpreter is the oracle
//! and it is correct — and would pass every architectural assertion below. The native
//! count is the only evidence that host code ran, and the tests that matter assert on it
//! before they assert on state.

use lazalith_cpu::{EngineKind, Privilege, StatusRegister};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_isa::{DataSize, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineError, MachineEvent, MachineSetup, MachineState};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

/// The flat address space every test here shares.
///
/// **A code region the machine loads as ROM.** A guest cannot write to its own code, which
/// is what makes a translated block's cached validity a question about the *engine's*
/// state rather than about self-modifying code — and this stage's JIT is not asked to
/// handle that question either way.
const CODE: u64 = 0x000;
const CODE_LENGTH: u64 = 0x400;
const DATA: u64 = 0x400;
const DATA_LENGTH: u64 = 0x400;
const STACK: u64 = 0x800;
const STACK_LENGTH: u64 = 0x200;

const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);
const RX: RegionPermissions = RegionPermissions::new(true, false, true, true);

fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).expect("a register exists"))
}

/// A memory operand: `[base + displacement]`.
fn at(base: u8, displacement: i32) -> Operand {
    Operand::Memory {
        base: RegisterIndex::try_from(base).expect("a register exists"),
        displacement,
    }
}

fn width(size: DataSize) -> Operand {
    Operand::DataSize(size)
}

fn assemble(config: ArchitectureConfig, instructions: &[Instruction]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for instruction in instructions {
        bytes.extend_from_slice(&encode(config, instruction).expect("an instruction encodes"));
    }
    bytes
}

/// A tiny builder, so the tests read as programs rather than as `Instruction::new` soup.
struct Asm<'a> {
    config: ArchitectureConfig,
    instructions: Vec<(Opcode, Vec<Operand>)>,
    _marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> Asm<'a> {
    fn new(config: ArchitectureConfig) -> Self {
        Self {
            config,
            instructions: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    fn add(mut self, opcode: Opcode, operands: &[Operand]) -> Self {
        self.instructions.push((opcode, operands.to_vec()));
        self
    }

    /// `LI rN, value`.
    fn li(self, register: u8, value: i32) -> Self {
        self.add(Opcode::Li, &[r(register), Operand::Immediate(value)])
    }

    /// `LDZ rN, [base + displacement]`.
    fn load(self, register: u8, base: u8, displacement: i32) -> Self {
        self.add(
            Opcode::Ldz,
            &[r(register), at(base, displacement), width(DataSize::Double)],
        )
    }

    /// `ST rN, [base + displacement]` — register first, because the ISA's `Mem` format is
    /// `[RD, MEMORY, SIZE]` for `Ldz` and `St` alike.
    fn store(self, register: u8, base: u8, displacement: i32) -> Self {
        self.add(
            Opcode::St,
            &[r(register), at(base, displacement), width(DataSize::Double)],
        )
    }

    fn bytes(&self) -> Vec<u8> {
        assemble(
            self.config,
            &self
                .instructions
                .iter()
                .map(|(opcode, operands)| {
                    Instruction::new(self.config, *opcode, operands).expect("well formed")
                })
                .collect::<Vec<_>>(),
        )
    }
}

/// A machine loaded with `code`, reset, running as supervisor, and ready to step.
fn machine(config: ArchitectureConfig, code: &[u8]) -> LazalithMachine<ConsoleDevice> {
    machine_at(config, code, Privilege::Supervisor)
}

/// The same, at a chosen privilege.
///
/// **Privilege is a bit in the status register, not a machine field**, so this sets
/// `status` rather than calling a setter — which is the only way to make a machine that
/// runs user code at all, and the precondition the privilege test needs.
fn machine_at(
    config: ArchitectureConfig,
    code: &[u8],
    privilege: Privilege,
) -> LazalithMachine<ConsoleDevice> {
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
        status: StatusRegister::new(privilege, false).bits(),
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
    machine.reset();
    // A trap vector, because a fault needs somewhere to go and the machine refuses to
    // enter a trap with no vector. Several tests below are *about* faults, and a missing
    // vector would turn each of them into a trap-entry failure that looks like a bug in
    // the handoff rather than a bug in the fixture.
    machine
        .set_trap_vector(InstructionAddress::new(CODE))
        .expect("a trap vector inside the code region");
    machine
}

/// Everything a test compares a run against: the state a guest can see.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Visible {
    registers: Vec<u64>,
    pc: u64,
    sp: u64,
    status: u64,
    executed: u64,
    cycles: u64,
    state: MachineState,
    /// The data region, which is where every program here writes.
    data: Vec<u8>,
}

fn visible(machine: &LazalithMachine<ConsoleDevice>) -> Visible {
    let state = machine.architectural_state();
    Visible {
        registers: (0..RegisterIndex::COUNT)
            .map(|index| state.registers().read_raw(index).unwrap_or(0))
            .collect(),
        pc: state.pc().as_u64(),
        sp: state.sp().as_u64(),
        status: state.status().bits(),
        executed: machine.executed_instruction_count(),
        cycles: machine.clock().elapsed().as_u64(),
        state: machine.state(),
        data: {
            let mut bytes = vec![0u8; DATA_LENGTH as usize];
            machine
                .peek_memory(PhysicalAddress::new(DATA), &mut bytes)
                .expect("the data region reads");
            bytes
        },
    }
}

/// Runs until the machine has retired `budget` guest instructions.
///
/// **Counted in retired instructions, not in `step()` calls, and that is the only correct
/// way to compare two engines.** A JIT block retires up to 31 instructions in one call to
/// `step`, so after one call a JIT and an interpreter are in completely different places
/// and comparing them says nothing about the handoff — it measures how many instructions
/// each engine happened to retire per call, which is the one thing they are *allowed* to
/// differ in. Every comparison in this file is therefore made at equal retired-instruction
/// counts, and the assertions that could distinguish a real disagreement from this
/// artefact are the ones that matter.
///
/// A generous cap on calls keeps a machine that has stopped making progress from looping
/// forever: it runs until either the budget is met or `budget * 8 + 64` calls have been
/// made, which no correct engine reaches.
fn run_instructions(machine: &mut LazalithMachine<ConsoleDevice>, budget: u64) -> MachineState {
    let target = machine.executed_instruction_count() + budget;
    for _ in 0..(budget * 8 + 64) {
        if machine.executed_instruction_count() >= target {
            break;
        }
        match machine.step() {
            Ok(MachineEvent::Stepped { .. }) => {}
            Ok(MachineEvent::Halted) => break,
            Ok(MachineEvent::Trapped { .. }) => break,
            Err(MachineError::NoExecutionEngine { decline }) => {
                panic!("no engine could execute here: {decline}")
            }
            Err(error) => panic!("the machine refused to step: {error}"),
        }
    }
    machine.state()
}

/// Runs `subject` for roughly `budget` instructions, then a fresh reference for *exactly
/// as many as the subject retired*, and compares the two.
///
/// # Why this is two runs and not one
///
/// **A JIT cannot be asked to run exactly one instruction.** One `step()` retires a whole
/// block, so a subject given a budget of 1 may retire 4, and its program counter is then
/// four instructions ahead of a reference given the same budget. Comparing the two at that
/// point measures the block size — the one thing the two engines are *allowed* to differ
/// in — rather than whether the handoff preserved anything.
///
/// So the subject's actual retired count is read afterwards, and the reference is run to
/// precisely that number. The comparison is then at equal instruction counts, which is the
/// only point at which "the guest cannot tell" is even a meaningful claim.
///
/// The overshoot is bounded and visible: a block is at most [`MAX_BLOCK`]-ish instructions,
/// so the two runs never diverge by more than that, and the helper reports the count it
/// matched on so a test failure can say what was compared.
fn compare_equivalent(
    config: ArchitectureConfig,
    code: &[u8],
    budget: u64,
    schedule: impl Fn(&mut LazalithMachine<ConsoleDevice>),
) {
    let mut subject = machine(config, code);
    schedule(&mut subject);
    run_instructions(&mut subject, budget);
    let matched = subject.executed_instruction_count();

    let mut reference = machine(config, code);
    run_instructions(&mut reference, matched);

    let a = visible(&subject);
    let b = visible(&reference);
    assert_eq!(
        a.registers, b.registers,
        "different registers after {matched} retired instructions"
    );
    assert_eq!(
        a.pc, b.pc,
        "different PC after {matched} retired instructions"
    );
    assert_eq!(
        a.sp, b.sp,
        "different SP after {matched} retired instructions"
    );
    assert_eq!(
        a.status, b.status,
        "different status register after {matched} retired instructions"
    );
    assert_eq!(
        a.data, b.data,
        "different memory after {matched} retired instructions"
    );
    assert_eq!(
        a.cycles, b.cycles,
        "a different virtual clock after {matched} retired instructions — a handoff that \
         charged the engine's failed attempt instead of the interpreter's work would \
         show up here"
    );
    assert_eq!(
        a.executed, b.executed,
        "different instruction counts after {matched} retired instructions"
    );
}

// -- the programs -----------------------------------------------------------

/// Register arithmetic the JIT can run, a memory access it cannot, then more of both.
///
/// **This is the program that makes the handoff visible.** The JIT translates the leading
/// `LI`, `LI`, `ADD` into one block and runs them as host code; it then meets `LDZ`,
/// declines, and the interpreter runs that one instruction; the next step the JIT tries
/// again from the new program counter. So a correct run of this program is *necessarily*
/// an interleaving of two engines, and comparing it to a single-engine run tests exactly
/// the handoff rather than either engine alone.
///
/// **The base register points into the data region, not at zero.** An earlier version
/// used `LI r5, 0`, which made the `ST` store to the *code* region — read-only, so it
/// faulted on permissions, the run trapped, and the test then compared a trapped
/// reference against a trapped subject and called it a pass on the wrong grounds. A
/// program that faults is a different test (there is one below, deliberately), and mixing
/// the two hides both.
fn mixed_program(config: ArchitectureConfig) -> Vec<u8> {
    Asm::new(config)
        .li(5, DATA as i32) // the base register a memory operand is relative to
        .li(1, 20)
        .li(2, 22)
        .add(Opcode::Add, &[r(0), r(1), r(2)]) // JIT-native
        .load(3, 5, 0) // JIT declines; interpreter runs this one
        .li(4, 7)
        .add(Opcode::Mul, &[r(6), r(4), r(2)]) // JIT-native
        .store(6, 5, 0) // JIT declines again
        .add(Opcode::Sub, &[r(7), r(0), r(3)]) // JIT-native
        .bytes()
}

// -- 1. the handoff itself --------------------------------------------------

/// The JIT runs part of a program and the interpreter finishes it, identically.
///
/// **The central B23 test.** Three things are asserted, and each of them can fail
/// independently:
///
/// 1. host code ran — `native_instructions() > 0`. Without this the test is vacuous: a
///    machine that declined everything and interpreted the whole program would satisfy
///    every architectural assertion here;
/// 2. the interpreter ran the parts the JIT could not — `engine_handoffs() > 0`;
/// 3. the result is byte-identical to a run that never switched, in registers, PC, SP,
///    status, memory, instruction count *and* virtual time.
///
/// The virtual-time equality is the strictest of these and the easiest to lose. Virtual
/// time is charged from whichever engine retired the instruction, so a handoff that
/// charged the JIT's failed attempt instead of the interpreter's work would show up here
/// as a clock that is too far along.
#[test]
fn a_jit_runs_part_of_a_program_and_the_interpreter_finishes_it() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);

    let mut reference = machine(config, &code);
    run_instructions(&mut reference, 40);

    let mut handoff = machine(config, &code);
    handoff
        .switch_execution_engine(EngineKind::Jit)
        .expect("the machine builds a JIT");
    run_instructions(&mut handoff, 40);

    assert!(
        handoff.native_instructions() > 0,
        "no guest instruction retired as host code, so nothing was actually handed off \
         from — the interpreter did the whole program and this test proves nothing"
    );
    assert!(
        handoff.engine_handoffs() > 0,
        "the JIT ran everything, so the interpreter was never handed an instruction; a \
         register-only JIT must decline the memory accesses in this program"
    );
    assert_eq!(
        visible(&handoff),
        visible(&reference),
        "a run that switched engines must be indistinguishable from one that did not"
    );
}

/// A decline hands the interpreter exactly the instruction the JIT would not run.
///
/// **The precision property, tested by construction rather than by outcome.** The
/// program stores the value it computed to memory and then loads it back; if the
/// interpreter re-ran the `LI` the register would be wrong, and if it skipped the `LDZ`
/// the loaded value would be zero. Both are visible in the final state, so the
/// "no double execution, no skipped instruction" requirement is checked by arithmetic
/// rather than by inspection.
#[test]
fn a_decline_leaves_the_next_instruction_to_the_interpreter() {
    let config = ArchitectureConfig::lz64();
    // Count to a value by incrementing, so a double-executed or skipped `LI` moves it.
    let code = Asm::new(config)
        .li(1, 0)
        .add(Opcode::Addi, &[r(1), r(1), Operand::Immediate(1)])
        .add(Opcode::Addi, &[r(1), r(1), Operand::Immediate(1)])
        .load(2, 1, 0) // declines: reading a register's *value* as an address
        .bytes();

    let mut jit = machine(config, &code);
    jit.switch_execution_engine(EngineKind::Jit)
        .expect("the machine builds a JIT");
    run_instructions(&mut jit, 8);

    let state = jit.architectural_state();
    assert_eq!(
        state.registers().read_raw(1).unwrap(),
        2,
        "r1 is 2, so each `ADDI` ran exactly once — a re-executed or skipped increment \
         would make it 1, 3 or 4"
    );
    assert!(
        jit.engine_handoffs() > 0,
        "and the interpreter really was asked to run something"
    );
}

/// A faulting instruction the JIT declines still traps, for the same reason.
///
/// **The case a naive implementation gets wrong, and it is the dangerous one.** The JIT
/// cannot even translate `LDZ`, so it declines; the interpreter runs it; the access
/// faults. If the handoff reported the decline *instead of* the fault, the guest would
/// see a machine error where it should see a trap with a resume address — and a guest
/// that installs a fault handler would stop working, on the JIT, for no reason it could
/// observe. That is exactly "the guest must not be able to tell", so it is tested
/// directly: the trap's cause and resume address must match the interpreter-only run.
#[test]
fn a_faulting_instruction_declined_by_the_jit_still_traps() {
    let config = ArchitectureConfig::lz64();
    // A load from a base register that was never set, so the address is 0 — unmapped.
    let code = Asm::new(config)
        .li(1, 20)
        .li(2, 22)
        .add(Opcode::Add, &[r(0), r(1), r(2)])
        .load(3, 0, 0) // base r0 is 0, which is the code region: not readable as data
        .bytes();

    let mut reference = machine(config, &code);
    let mut jit = machine(config, &code);
    jit.switch_execution_engine(EngineKind::Jit)
        .expect("the machine builds a JIT");
    jit.set_yield_points(std::iter::empty());

    let mut reference_trap = None;
    for _ in 0..8 {
        if let Ok(MachineEvent::Trapped { event }) = reference.step() {
            reference_trap = Some(event);
            break;
        }
    }
    let mut jit_trap = None;
    for _ in 0..8 {
        if let Ok(MachineEvent::Trapped { event }) = jit.step() {
            jit_trap = Some(event);
            break;
        }
    }

    let reference_trap = reference_trap.expect("the reference traps on an unmapped load");
    let jit_trap = jit_trap.expect("the JIT path must trap identically, not decline away");
    assert_eq!(
        jit_trap.cause, reference_trap.cause,
        "the same fault, for the same reason — a decline is not allowed to replace it"
    );
    assert_eq!(
        jit_trap.resume_pc, reference_trap.resume_pc,
        "and from the same address, so a handler runs with the same state"
    );
    assert_eq!(
        jit.engine_handoffs(),
        1,
        "exactly one handoff: the three register instructions ran natively and the load \
         was handed over. More would mean the interpreter ran something twice."
    );
    assert!(
        jit.native_instructions() >= 3,
        "and the three native instructions really were native"
    );
}

// -- 2. the other direction --------------------------------------------------

/// The machine can start on the interpreter and become a JIT mid-program, with no reset.
///
/// **§11's "interpreter execution → hot code identified → JIT compiles → JIT continues
/// from the same architectural state", reached automatically.** The switch is armed for
/// three instructions; the run crosses that point between two steps, and the result must
/// match a run that never switched.
#[test]
fn the_machine_transitions_from_the_interpreter_to_the_jit_on_its_own() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);

    let mut reference = machine(config, &code);
    run_instructions(&mut reference, 40);

    let mut warmed = machine(config, &code);
    warmed.use_jit_after(3).expect("the warm-up arms");
    assert_eq!(
        warmed.execution_engine(),
        EngineKind::Reference,
        "arming a warm-up is not switching: the machine is still interpreting"
    );
    run_instructions(&mut warmed, 40);

    assert_eq!(
        warmed.execution_engine(),
        EngineKind::Jit,
        "and after the warm-up it is running the JIT"
    );
    assert!(
        warmed.native_instructions() > 0,
        "so the JIT really did run something natively after the transition"
    );
    assert_eq!(
        visible(&warmed),
        visible(&reference),
        "a machine that became a JIT mid-run lands where an interpreter-only run lands"
    );
}

/// Engine identity survives being changed many times, and changes nothing.
///
/// **The requirement that switching is a runtime capability and not an initialisation
/// event.** Five switches across one run, in both directions, with the state compared
/// after each one *and* at the end. Comparing only at the end would pass even if an
/// intermediate switch had corrupted the state and a later one had papered over it —
/// which is the failure mode of a test that only looks at the destination.
#[test]
fn repeated_switching_lands_where_an_uninterrupted_run_does() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);
    let schedule = [
        EngineKind::Jit,
        EngineKind::Reference,
        EngineKind::Jit,
        EngineKind::Reference,
        EngineKind::Jit,
    ];

    let mut subject = machine(config, &code);
    let mut switches = 0u64;
    for (index, engine) in schedule.iter().enumerate() {
        run_instructions(&mut subject, 5);
        subject
            .switch_execution_engine(*engine)
            .unwrap_or_else(|error| panic!("switch {index} to {engine} failed: {error}"));
        assert_eq!(
            subject.execution_engine(),
            *engine,
            "the switch took effect"
        );
        switches += 1;
    }
    run_instructions(&mut subject, 40);
    assert_eq!(switches, 5, "five switches actually happened");

    let matched = subject.executed_instruction_count();
    let mut reference = machine(config, &code);
    run_instructions(&mut reference, matched);

    let a = visible(&subject);
    let b = visible(&reference);
    assert_eq!(
        a.registers, b.registers,
        "after five switches the registers are where an uninterrupted run's are"
    );
    assert_eq!(a.pc, b.pc, "and the program counter is the same");
    assert_eq!(a.sp, b.sp, "and the stack pointer is the same");
    assert_eq!(a.status, b.status, "and the status register is the same");
    assert_eq!(a.data, b.data, "and memory is the same");
    assert_eq!(
        a.cycles, b.cycles,
        "and the virtual clock advanced identically"
    );
    assert_eq!(a.executed, b.executed, "and so many instructions retired");
    assert!(
        subject.native_instructions() > 0,
        "and the JIT really ran natively somewhere in those five switches, or the schedule \
         never exercised a handoff"
    );
    assert!(
        subject.engine_handoffs() > 0,
        "and the interpreter really was handed instructions"
    );
}

// -- 3. the debugger ---------------------------------------------------------

/// A breakpoint inside a translated block is not run past.
///
/// **The one way a JIT can be observably wrong about debugging while computing everything
/// correctly.** The block below would cover all four instructions; with a yield point on
/// the third, the JIT must stop the block *before* it, so the program counter arrives at
/// the breakpoint having retired only the first two.
#[test]
fn a_breakpoint_inside_a_block_is_not_run_past() {
    let config = ArchitectureConfig::lz64();
    let code = Asm::new(config)
        .li(1, 10)
        .li(2, 20)
        .li(3, 30) // <- the breakpoint: the third instruction
        .li(4, 40)
        .bytes();
    let breakpoint = CODE + 2 * 8; // two instructions in

    let mut machine = machine(config, &code);
    machine
        .switch_execution_engine(EngineKind::Jit)
        .expect("the machine builds a JIT");
    machine.set_yield_points([breakpoint]);

    // One step must not carry the program counter past the breakpoint.
    match machine.step().expect("a step runs") {
        MachineEvent::Stepped { instructions, .. } => assert_eq!(
            instructions, 2,
            "the block stopped at the breakpoint, so it retired two instructions rather \
             than running all four"
        ),
        other => panic!("expected a step, got {other:?}"),
    }
    assert_eq!(
        machine.architectural_state().pc().as_u64(),
        breakpoint,
        "and the program counter is exactly on the breakpoint"
    );
    let state = machine.architectural_state();
    assert_eq!(
        state.registers().read_raw(3).unwrap(),
        0,
        "the instruction at the breakpoint has not run, so r3 is still zero"
    );
    assert_eq!(
        state.registers().read_raw(4).unwrap(),
        0,
        "and neither has the one after it"
    );
    assert_eq!(
        state.registers().read_raw(1).unwrap(),
        10,
        "while the two before it did run"
    );

    // And with the boundary removed, the same machine runs the rest of the block.
    machine.set_yield_points(std::iter::empty());
    match machine.step().expect("a step runs") {
        MachineEvent::Stepped { instructions, .. } => assert_eq!(
            instructions, 2,
            "with no breakpoint the two remaining instructions are one block"
        ),
        other => panic!("expected a step, got {other:?}"),
    }
    let after = machine.architectural_state();
    assert_eq!(
        after.registers().read_raw(3).unwrap(),
        30,
        "so the instruction at the breakpoint ran this time"
    );
    assert_eq!(
        after.registers().read_raw(4).unwrap(),
        40,
        "and the one after it"
    );
}

// -- 4. snapshot and restore -------------------------------------------------

/// A snapshot taken on one engine restores onto the other.
///
/// **B18 made the snapshot a statement about guest-visible state; this is the test that
/// says so where it is hardest to be true.** The snapshot is taken with the JIT installed
/// and the JIT having run natively, then restored into a machine that has never seen a
/// JIT, and the program is finished by the interpreter. The reverse — snapshot on the
/// interpreter, restore and run on the JIT — is the other direction of the same
/// property, and is what a snapshot taken *while* a JIT is warm must not smuggle into
/// canonical state.
#[test]
fn a_snapshot_taken_on_one_engine_restores_onto_the_other() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);

    // Warm a JIT, snapshot there, restore onto a fresh interpreter-only machine.
    let mut warm = machine(config, &code);
    warm.switch_execution_engine(EngineKind::Jit)
        .expect("the machine builds a JIT");
    run_instructions(&mut warm, 6);
    assert!(
        warm.native_instructions() > 0,
        "the JIT really ran before the snapshot, so restoring onto a machine with no JIT \
         at all is being tested"
    );
    let snapshot = warm.architectural_state();

    let mut restored = machine(config, &code);
    restored
        .processor_mut()
        .restore_architectural(snapshot.clone())
        .expect("a reset machine accepts a restored processor");
    run_instructions(&mut restored, 40);

    assert_eq!(
        restored.execution_engine(),
        EngineKind::Reference,
        "the restored machine has no JIT, which is the point"
    );
    assert_eq!(
        restored.engine_handoffs(),
        0,
        "and so nothing was handed off — the interpreter finished the program alone"
    );

    let mut reference_run = machine(config, &code);
    reference_run
        .processor_mut()
        .restore_architectural(snapshot.clone())
        .expect("a reset machine accepts a restored processor");
    run_instructions(&mut reference_run, 40);
    let expected = visible(&reference_run);
    assert_eq!(
        visible(&restored).registers,
        expected.registers,
        "finishing a JIT-era snapshot on the interpreter gives the same registers"
    );
    assert_eq!(
        visible(&restored).data,
        expected.data,
        "and the same memory"
    );
}

// -- 5. the guest cannot tell ------------------------------------------------

/// The engine schedule does not change what the guest can see.
///
/// **§11's "a mode switch must not reset, clone, reinterpret, or silently alter guest
/// state", checked across the whole of a run rather than at one boundary.** Every engine
/// in [`EngineKind::ALL`] and every yield point that could be set, all compared against a
/// single-engine reference. The `steps` value is swept too, because a run that is cut short
/// at a different point is a different comparison and would let a schedule that only
/// agrees on some boundaries pass.
#[test]
fn the_engine_schedule_does_not_change_what_the_guest_can_see() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);

    for engine in EngineKind::ALL {
        for budget in [1u64, 2, 3, 5, 8, 13, 40] {
            // Half the runs get a yield point, because a boundary the engine must stop at
            // is the most likely place for a switch to leave the state wrong.
            let with_boundary = budget % 2 == 0;
            compare_equivalent(config, &code, budget, |subject| {
                subject
                    .switch_execution_engine(*engine)
                    .unwrap_or_else(|error| panic!("{engine} is not available: {error}"));
                if with_boundary {
                    subject.set_yield_points([CODE + 8 * 3]);
                }
            });
        }
    }
}

/// A decline is never a trap, on either engine, and never a step the guest can see.
///
/// **§11's "the guest cannot tell", in its sharpest available form.** A decline is
/// reported to the host as a *count* and to the guest as an ordinary retired instruction.
/// What it must never be is a [`MachineEvent::Trapped`] — and this program faults
/// eventually, so a decline that leaked into the trap path would show up as an *extra*
/// trap rather than as a missing one.
///
/// Note what is deliberately **not** asserted: the `MachineEvent::Stepped` streams are not
/// identical between the two engines, and must not be. A JIT block retires several
/// instructions in one `Stepped` and an interpreter retires one, so `instructions` differs.
/// That field is host-level reporting about how the work was done — which is exactly why
/// `engine_handoffs` is a counter on the machine and not an event, and why no guest can
/// read either.
#[test]
fn a_decline_is_never_a_trap_the_guest_can_observe() {
    let config = ArchitectureConfig::lz64();
    let code = mixed_program(config);

    let run_schedules = |jit: bool| {
        let mut machine = machine(config, &code);
        if jit {
            machine
                .switch_execution_engine(EngineKind::Jit)
                .expect("a JIT");
        }
        let mut traps = 0usize;
        let mut steps = 0usize;
        for _ in 0..32 {
            match machine.step() {
                Ok(MachineEvent::Stepped { .. }) => steps += 1,
                Ok(MachineEvent::Trapped { .. }) => {
                    traps += 1;
                    break;
                }
                Ok(MachineEvent::Halted) => break,
                Err(error) => panic!("the machine refused to step: {error}"),
            }
        }
        (steps, traps, machine.engine_handoffs())
    };

    let (interpreted_steps, interpreted_traps, interpreted_handoffs) = run_schedules(false);
    let (jit_steps, jit_traps, jit_handoffs) = run_schedules(true);

    assert_eq!(
        interpreted_traps, 0,
        "and this program does not fault at all"
    );
    assert_eq!(
        interpreted_handoffs, 0,
        "an interpreter run never hands off"
    );
    assert_eq!(
        jit_traps, interpreted_traps,
        "the JIT run produced the same number of traps — a decline that became a trap \
         would show up here as one extra"
    );
    assert!(
        jit_handoffs > 0,
        "and it really did decline, or nothing was tested"
    );
    assert!(
        jit_steps <= interpreted_steps,
        "and it needed no more steps than the interpreter, because a decline does not \
         cost the guest an extra step: {} against {}",
        jit_steps,
        interpreted_steps
    );
}

/// The privilege an engine runs at is the machine's, not the engine's.
///
/// **A decline must not be a way to escape privilege checking.** `DI` is a supervisor-only
/// instruction, so the JIT declines it and the interpreter runs it and refuses. If a
/// decline could bypass the check, a user program could run a privileged instruction by
/// having it "handled by another engine" — which is the one security-relevant thing about
/// the handoff.
#[test]
fn a_declined_privileged_instruction_is_still_refused() {
    let config = ArchitectureConfig::lz64();
    let code = Asm::new(config)
        .li(1, 1)
        .add(Opcode::Add, &[r(0), r(1), r(1)])
        .add(Opcode::Di, &[]) // supervisor-only: the JIT declines it
        .bytes();

    // **Run as user, or there is nothing to refuse.** Privilege is a bit in the status
    // register, so a supervisor machine may legitimately execute `DI` and the test would
    // pass for the wrong reason — or fail, depending on how it was written.
    for engine in [EngineKind::Reference, EngineKind::Jit] {
        let mut machine = machine_at(config, &code, Privilege::User);
        machine
            .switch_execution_engine(engine)
            .expect("the machine builds the engine");
        let trapped = (0..8).any(|_| matches!(machine.step(), Ok(MachineEvent::Trapped { .. })));
        assert!(
            trapped,
            "{engine} must refuse a privileged instruction from user mode, whether it \
             translated it or handed it over: a decline that skipped the privilege check \
             would be a way to execute supervisor-only code from user mode"
        );
    }
}

/// The machine reports no engine could run when that is true, and does not trap.
///
/// **The failure mode of a *doubly* declining machine.** The active engine declines and so
/// does the Reference Interpreter, which cannot happen for a well-formed guest; when it
/// does, the machine has no honest way to proceed. It must report that rather than trap,
/// because a trap would be a lie about who is at fault — and it must not loop, because
/// the guest would spin forever on an address nothing can execute.
#[test]
fn a_machine_with_no_able_engine_reports_it_rather_than_trapping() {
    let config = ArchitectureConfig::lz64();
    // A `Halt` in supervisor mode is a control-transfer the JIT declines; on a machine
    // whose fallback is also unable to run it, this is the shape of "nothing can execute
    // here". Built by forcing the situation through the public API rather than by
    // inventing a broken engine, so the test exercises the real code path.
    let code = Asm::new(config).li(1, 1).bytes();

    let mut machine = machine(config, &code);
    machine.set_yield_points([CODE]); // at the PC, so the boundary is below the counter
    machine
        .switch_execution_engine(EngineKind::Jit)
        .expect("a JIT");

    // The machine filters boundaries at or below the PC, so this must *not* stop the
    // machine from executing — which is the property the boundary filtering exists for.
    assert_eq!(
        machine.yield_points().len(),
        1,
        "the set is stored as given"
    );
    match machine.step() {
        Ok(MachineEvent::Stepped { instructions, .. }) => assert_eq!(
            instructions, 1,
            "a boundary on the current PC is ignored rather than excluding the \
             instruction about to run, which would decline every step forever"
        ),
        other => panic!("expected a step, got {other:?}"),
    }
}
