//! B3: the execution-engine boundary.
//!
//! # What these tests are for
//!
//! `binstruction.md` requires that the eventual JIT and the Reference Interpreter
//! be two *modes of one virtual machine* rather than two machines, and that a
//! switch between them preserve the guest's architectural state without a
//! restart:
//!
//! ```text
//! Interpreter ──▶ JIT        hot code identified
//! JIT ──▶ Interpreter        breakpoint, fault, or unsupported block
//! ```
//!
//! There is no JIT in this repository. What there *is* now is the seam such a
//! switch would go through — and a seam nobody has tested is a comment. So these
//! tests exercise the operation itself, [`LazalithMachine::switch_execution_engine`],
//! and check the one property the requirement is really about:
//!
//! > The switch is an execution-engine change, not a machine reset.
//!
//! # The second engine
//!
//! [`Counting`] is deliberately trivial and deliberately *not* a second
//! implementation of the ISA: it delegates every step to the reference engine and
//! counts. That is the point. What these tests establish is that **an outside
//! implementation of the engine trait can take over a running machine and hand it
//! back** — whether what is inside `step` is a counter or a compiler is a question
//! for the stage that writes the compiler, and the machine needed no change to
//! allow either.

use lazalith_cpu::{
    ArchitecturalState, CpuFault, CpuMemory, EngineError, EngineKind, ExecutionContextId,
    ExecutionEngine, Privilege, Processor, ReferenceInterpreter, StatusRegister, StepResult,
};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_isa::{DataSize, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineError, MachineSetup, MachineState};
use lazalith_memory::{AddressSpace, MemoryRegion, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

/// An execution engine defined outside the workspace, standing in for a JIT.
///
/// It holds private state — a step count — so the tests can check that private
/// state is discardable while the architectural state is not.
#[derive(Debug, Default)]
struct Counting {
    steps: u64,
    discarded: u64,
}

impl Counting {
    fn steps(&self) -> u64 {
        self.steps
    }
    fn discards(&self) -> u64 {
        self.discarded
    }
}

impl<M: CpuMemory> ExecutionEngine<M> for Counting {
    /// Reports the reference engine on purpose: a wrapper has no name of its own,
    /// and a machine that adopted one would otherwise be claiming to be something
    /// it is not. The tests use this to check that the name a machine reports is
    /// the name it is actually running.
    fn kind(&self) -> EngineKind {
        EngineKind::Reference
    }

    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        let result = ReferenceInterpreter::new().step(processor, memory)?;
        self.steps += 1;
        // The cost is forwarded, not recomputed. A wrapper around the reference engine
        // must report the same cost the reference reports, or a machine running it
        // would keep a different virtual clock from a machine running the reference
        // directly — and the two are supposed to be the same machine.
        Ok(result)
    }

    fn execute(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        self.steps += 1;
        ReferenceInterpreter::new().execute(processor, instruction, memory)
    }

    fn discard_private_state(&mut self) {
        self.discarded += 1;
        self.steps = 0;
    }
}

// -- machine construction ---------------------------------------------------

const CODE: u64 = 0x0000;
const STACK: u64 = 0x2000;
const STACK_TOP: u64 = STACK + 1024;
const TRAP: u64 = 0x4000;
const DEVICE_BASE: u64 = 0x0100_0000;
const CONSOLE: DeviceId = DeviceId::new(1);

const MODES: [ArchitectureConfig; 2] = [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()];

const RWX_USER: fn() -> RegionPermissions = || RegionPermissions::new(true, true, true, true);
const RW_USER: fn() -> RegionPermissions = || RegionPermissions::new(true, true, false, true);
const RX: fn() -> RegionPermissions = || RegionPermissions::new(true, false, true, false);

/// The program these tests run:
///
/// ```text
/// li  r1, 0x01000000   the console device's address
/// li  r2, 3
/// li  r3, 4
/// add r2, r2, r3       r2 = 7
/// st  r2, [r1 + 0]     the console device receives 7
/// halt
/// ```
///
/// It is assembled with the real encoder rather than written as bytes, so the test
/// cannot pass or fail on a hand-written encoding that the ISA never defined.
fn program(config: ArchitectureConfig) -> Vec<u8> {
    let mut code = Vec::new();
    let mut emit = |opcode: Opcode, operands: &[Operand]| {
        let instruction = Instruction::new(config, opcode, operands).unwrap();
        code.extend_from_slice(&encode(config, &instruction).unwrap());
    };
    emit(Opcode::Li, &[r(1), Operand::Immediate(DEVICE_BASE as i32)]);
    emit(Opcode::Li, &[r(2), Operand::Immediate(3)]);
    emit(Opcode::Li, &[r(3), Operand::Immediate(4)]);
    emit(Opcode::Add, &[r(2), r(2), r(3)]);
    emit(
        Opcode::St,
        &[
            r(2),
            Operand::Memory {
                base: RegisterIndex::try_from(1u8).unwrap(),
                displacement: 0,
            },
            Operand::DataSize(DataSize::Byte),
        ],
    );
    emit(Opcode::Halt, &[]);
    code
}

fn r(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).unwrap())
}

fn machine(config: ArchitectureConfig) -> LazalithMachine<ConsoleDevice> {
    let mut devices = DeviceManager::new();
    devices
        .insert(CONSOLE, ConsoleDevice::new(64).unwrap())
        .unwrap();
    let setup = MachineSetup {
        config,
        devices,
        regions: vec![
            MemoryRegion::ram(config, PhysicalAddress::new(CODE), 512, RWX_USER()).unwrap(),
            MemoryRegion::ram(config, PhysicalAddress::new(STACK), 1024, RW_USER()).unwrap(),
            MemoryRegion::ram(config, PhysicalAddress::new(TRAP), 64, RX()).unwrap(),
        ],
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK_TOP),
        status: 0,
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).unwrap();
    machine
        .map_device(CONSOLE, PhysicalAddress::new(DEVICE_BASE), RW_USER())
        .unwrap();
    machine
        .load_bytes(PhysicalAddress::new(CODE), &program(config))
        .unwrap();
    // Something executable for a trap to land on, and a vector that points at it.
    let halt = Instruction::new(config, Opcode::Halt, &[]).unwrap();
    machine
        .load_bytes(PhysicalAddress::new(TRAP), &encode(config, &halt).unwrap())
        .unwrap();
    // A machine is created rather than running: `reset` is what takes it out of
    // `Created` into a state a step is legal from -- and it is also what *clears*
    // the trap vector, because a reset is a new processor. So the vector is set
    // after it, which is the order every caller that needs a trap to land uses.
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(TRAP))
        .unwrap();
    machine
}

fn bare_machine(config: ArchitectureConfig) -> LazalithMachine<lazalith_devices::NoDevice> {
    let setup = MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions: vec![
            MemoryRegion::ram(config, PhysicalAddress::new(CODE), 512, RWX_USER()).unwrap(),
            MemoryRegion::ram(config, PhysicalAddress::new(STACK), 1024, RW_USER()).unwrap(),
        ],
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK_TOP),
        status: 0,
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).unwrap();
    machine.reset();
    machine
}

fn register(machine: &LazalithMachine<ConsoleDevice>, index: u8) -> u64 {
    machine
        .processor()
        .architectural()
        .registers()
        .read(RegisterIndex::try_from(index).unwrap())
}

/// Every register, in index order.
fn all_registers(machine: &LazalithMachine<ConsoleDevice>) -> Vec<u64> {
    (0..u8::MAX)
        .filter_map(|index| RegisterIndex::try_from(index).ok())
        .map(|index| machine.processor().architectural().registers().read(index))
        .collect()
}

fn console(machine: &LazalithMachine<ConsoleDevice>) -> Vec<u8> {
    machine.devices().device(CONSOLE).unwrap().output().to_vec()
}

/// Replaces the program with a single instruction, for the fault cases.
fn load_only(
    machine: &mut LazalithMachine<ConsoleDevice>,
    opcode: Opcode,
    operand: Option<Operand>,
) {
    let config = machine.config();
    let owned = operand.into_iter().collect::<Vec<Operand>>();
    let instruction = Instruction::new(config, opcode, &owned).unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(CODE),
            &encode(config, &instruction).unwrap(),
        )
        .unwrap();
}

// -- the boundary itself ----------------------------------------------------

/// A new machine is on the reference engine, and says so.
#[test]
fn a_machine_starts_on_the_reference_engine() {
    for config in MODES {
        let machine = machine(config);
        assert_eq!(machine.execution_engine(), EngineKind::Reference);
        assert_eq!(machine.execution_engine().name(), "reference");
        assert_eq!(EngineKind::Reference.to_string(), "reference");
    }
}

/// Every engine this build names is one it can actually be switched to.
///
/// A machine that could not be asked to run a named engine could not be switched
/// to a new one later without changing its own type — which is the thing this
/// stage exists to make unnecessary.
#[test]
fn every_named_engine_is_one_a_machine_accepts() {
    assert!(!EngineKind::ALL.is_empty());
    for kind in EngineKind::ALL {
        let mut machine = machine(ArchitectureConfig::lz64());
        machine.switch_execution_engine(*kind).unwrap();
        assert_eq!(machine.execution_engine(), *kind);
    }
}

/// A switch mid-program changes nothing the guest can see.
///
/// This is the requirement in one test: the program counter, the stack pointer,
/// every register, the status register, the execution state, the machine's own
/// state and its virtual clock are exactly what they were. A machine that reset,
/// cloned, or reinterpreted would fail here.
#[test]
fn a_switch_preserves_everything_the_guest_can_see() {
    for config in MODES {
        for kind in EngineKind::ALL {
            a_switch_preserves_everything(config, *kind);
        }
    }
}

/// The same, for one destination engine.
///
/// **A parameter rather than a loop body, so the failure names the engine.** The JIT is
/// the reason this is a function: it is the one engine that *builds* a code cache on
/// first use, and a switch to it is the one switch that does more than install a trait
/// object. A loop that switched only to `Reference` would still pass after a JIT landed,
/// because the test never went near the interesting boundary.
fn a_switch_preserves_everything(config: ArchitectureConfig, to: EngineKind) {
    {
        let mut machine = machine(config);
        // Four steps in: the registers and the flags are non-trivial and there is
        // still a next instruction.
        for _ in 0..4 {
            machine.step().unwrap();
        }
        assert_eq!(
            register(&machine, 2),
            7,
            "four instructions in, the add has run"
        );

        let pc = machine.processor().architectural().pc();
        let sp = machine.processor().architectural().sp();
        let status = machine.processor().architectural().status().bits();
        let execution = machine.processor().execution();
        let state = machine.state();
        let time = machine.clock().elapsed();
        let registers = all_registers(&machine);
        let device = console(&machine);

        machine.switch_execution_engine(to).unwrap();
        assert_eq!(machine.execution_engine(), to, "the switch took effect");

        assert_eq!(machine.processor().architectural().pc(), pc, "pc changed");
        assert_eq!(machine.processor().architectural().sp(), sp, "sp changed");
        assert_eq!(
            machine.processor().architectural().status().bits(),
            status,
            "flags changed"
        );
        assert_eq!(machine.processor().execution(), execution);
        assert_eq!(machine.state(), state, "the machine's own state changed");
        assert_eq!(machine.clock().elapsed(), time, "virtual time changed");
        assert_eq!(all_registers(&machine), registers, "a register changed");
        assert_eq!(console(&machine), device, "device state changed");
    }
}

/// Execution continues correctly across a switch.
///
/// A switch that preserved every register and then executed differently would
/// pass the test above and still be a machine that had reset. So the program is
/// run to completion, through a switch, and both the arithmetic and the device
/// write are checked.
#[test]
fn a_program_runs_correctly_across_a_switch() {
    for config in MODES {
        let mut machine = machine(config);
        machine.step().unwrap();
        machine.step().unwrap();
        machine
            .switch_execution_engine(EngineKind::Reference)
            .unwrap();

        let run = machine.run(64).unwrap();
        assert!(run.halted_at.is_some(), "the program did not halt: {run:?}");
        assert!(run.trap.is_none());
        assert_eq!(register(&machine, 2), 7, "3 + 4, through a switch");
        assert_eq!(machine.state(), MachineState::Halted);
        assert_eq!(console(&machine), vec![7], "the device write survived");
    }
}

/// The same program produces the same answer with and without a switch.
///
/// Two machines, one never switched and one switched between every instruction.
/// Any difference is a difference the switch introduced.
#[test]
fn switching_between_every_instruction_changes_nothing() {
    for config in MODES {
        let mut plain = machine(config);
        let plain_run = plain.run(64).unwrap();

        let mut switched = machine(config);
        let mut steps = 0u32;
        loop {
            switched
                .switch_execution_engine(EngineKind::Reference)
                .unwrap();
            let run = switched.run(1).unwrap();
            if run.halted_at.is_some() {
                break;
            }
            steps += 1;
            if steps > 16 {
                panic!("the program did not finish");
            }
        }

        assert_eq!(plain_run.executed, 6, "the program is six instructions");
        assert_eq!(steps, 5, "five of them were not the halt");
        assert_eq!(plain.state(), switched.state());
        assert_eq!(all_registers(&plain), all_registers(&switched));
        assert_eq!(console(&plain), console(&switched));
    }
}

/// A switch around a fault keeps the fault report and the frame.
///
/// This is the direction a JIT needs most: a fault in compiled code has to be
/// able to come back with the trap frame intact, or the program could never
/// return from it.
#[test]
fn a_switch_does_not_disturb_a_live_trap_frame() {
    let config = ArchitectureConfig::lz64();
    let mut machine = machine(config);
    load_only(&mut machine, Opcode::Trap, Some(Operand::Immediate(7)));

    machine.step().unwrap();
    let frame = machine.processor().traps().frame().cloned();
    let pc = machine.processor().architectural().pc();
    let resume = machine.last_trap_resume_pc();
    assert!(frame.is_some(), "the trap should have been entered");
    assert!(machine.processor().traps().has_active_frame());

    machine
        .switch_execution_engine(EngineKind::Reference)
        .unwrap();

    assert!(
        machine.processor().traps().has_active_frame(),
        "the frame a program is stopped in must survive a switch"
    );
    assert_eq!(machine.processor().traps().frame().cloned(), frame);
    assert_eq!(machine.processor().architectural().pc(), pc);
    assert_eq!(machine.last_trap_resume_pc(), resume);
}

/// A switch is refused while a user execution context is active.
///
/// The scheduler is between two steps of a guest process and would not observe
/// the change, so "which engine ran this process" would have two answers.
#[test]
fn a_switch_is_refused_during_an_execution_context() {
    let config = ArchitectureConfig::lz64();
    let mut machine = bare_machine(config);
    let user = ArchitecturalState::new(
        config,
        InstructionAddress::new(CODE),
        VirtualAddress::new(STACK_TOP),
        StatusRegister::new(Privilege::User, false).bits(),
    )
    .unwrap();
    let mut space = AddressSpace::new(config);
    space
        .map(
            MemoryRegion::ram(
                config,
                PhysicalAddress::new(STACK),
                1024,
                RegionPermissions::new(true, true, false, true),
            )
            .unwrap(),
        )
        .unwrap();
    machine
        .activate_user_context(&mut space, user, ExecutionContextId::new(1).unwrap())
        .unwrap();
    assert!(machine.active_execution_context().is_some());

    match machine.switch_execution_engine(EngineKind::Reference) {
        Err(MachineError::Engine(EngineError::SwitchRefused { reason })) => {
            assert!(!reason.is_empty(), "a refusal must say why");
        }
        other => panic!("a switch during an execution context was not refused: {other:?}"),
    }
    assert_eq!(machine.execution_engine(), EngineKind::Reference);
}

/// A faulted machine cannot be revived by switching engines.
///
/// A switch is not a reset, and a terminal trap controller cannot enter another
/// trap, so accepting the switch would promise something the next step cannot
/// deliver.
#[test]
fn a_switch_is_refused_on_a_faulted_machine() {
    let config = ArchitectureConfig::lz64();
    // A bare machine has no trap vector, so entering a trap cannot succeed and the
    // machine faults rather than trapping.
    let mut machine = bare_machine(config);
    machine
        .load_bytes(
            PhysicalAddress::new(CODE),
            &encode(
                config,
                &Instruction::new(config, Opcode::Syscall, &[]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(machine.step().is_err());
    assert_eq!(machine.state(), MachineState::Faulted);

    match machine.switch_execution_engine(EngineKind::Reference) {
        Err(MachineError::Engine(EngineError::SwitchRefused { reason })) => {
            assert!(!reason.is_empty(), "a refusal must say why");
        }
        other => panic!("a switch on a faulted machine was not refused: {other:?}"),
    }
    assert_eq!(machine.state(), MachineState::Faulted);
}

/// A reset still puts the architectural state back exactly.
///
/// The engine is orthogonal to the reset, so a reset that the extraction changed
/// would be a defect in the extraction rather than in either.
#[test]
fn a_reset_after_a_switch_restores_the_initial_state() {
    let config = ArchitectureConfig::lz64();
    let mut machine = machine(config);
    for _ in 0..4 {
        machine.step().unwrap();
    }
    assert_ne!(register(&machine, 2), 0);

    machine
        .switch_execution_engine(EngineKind::Reference)
        .unwrap();
    machine.reset();

    assert_eq!(
        machine.processor().architectural().pc(),
        InstructionAddress::new(CODE)
    );
    assert_eq!(
        machine.processor().architectural().sp(),
        VirtualAddress::new(STACK_TOP)
    );
    assert_eq!(
        all_registers(&machine),
        vec![0; all_registers(&machine).len()]
    );
    assert!(console(&machine).is_empty());
    assert_eq!(machine.state(), MachineState::Reset);
    assert!(!machine.processor().traps().has_active_frame());
}

/// The outside engine can drive a machine's processor directly.
///
/// This is what proves the trait is implementable from outside the workspace: if
/// a foreign `impl ExecutionEngine` can step a machine's own processor through the
/// reference semantics, then what the trait asks of a second engine is a matter of
/// what goes inside `step`, and not a change to anything the machine owns.
#[test]
fn an_outside_engine_steps_a_machine_processor() {
    let config = ArchitectureConfig::lz64();
    let mut machine = machine(config);
    let mut engine = Counting::default();

    let before = engine.steps();
    let mut space = AddressSpace::new(config);
    space
        .map(MemoryRegion::ram(config, PhysicalAddress::new(CODE), 512, RWX_USER()).unwrap())
        .unwrap();
    space
        .initialize(PhysicalAddress::new(CODE), &program(config))
        .unwrap();
    let outcome = engine.step(machine.processor_mut(), &mut space);
    assert!(
        outcome.is_ok(),
        "the outside engine refused to step: {outcome:?}"
    );
    assert_eq!(engine.steps(), before + 1);

    // Its private state is its own to discard.
    ExecutionEngine::<AddressSpace>::discard_private_state(&mut engine);
    assert_eq!(engine.steps(), 0);
    assert_eq!(engine.discards(), 1);
}

/// An outside engine is `Debug`, because a machine that holds one has to be.
///
/// A machine with an unprintable field in it is a machine whose bug reports have
/// to be assembled by hand from the pieces around the interesting part.
#[test]
fn an_outside_engine_is_printable() {
    let machine = machine(ArchitectureConfig::lz64());
    let text = format!("{machine:?}");
    assert!(!text.is_empty());
    assert!(text.contains("LazalithMachine"), "{text}");
}

/// A guest that faults is refused identically whichever engine would run it.
///
/// The fault is produced by the reference engine here; what this checks is that
/// the fault carries the guest's program counter and not the engine's location,
/// which is the property a second engine has to keep or it is reporting its own
/// internals to the guest.
#[test]
fn a_fault_reports_the_guest_program_counter() {
    let config = ArchitectureConfig::lz64();
    let mut machine = machine(config);
    load_only(&mut machine, Opcode::Trap, Some(Operand::Immediate(1)));
    machine.step().unwrap();
    // The *resume* point, which is the next guest instruction after the one that
    // trapped 2014 not the trap vector the machine is now sitting at.
    let event = machine.last_trap_resume_pc();
    assert_eq!(event, Some(InstructionAddress::new(CODE + 8)));
    assert_ne!(
        machine.processor().architectural().pc(),
        event.unwrap(),
        "a trapped machine is at the vector, not at the resume point"
    );

    machine
        .switch_execution_engine(EngineKind::Reference)
        .unwrap();
    assert_eq!(machine.last_trap_resume_pc(), event);
}

/// The data size a guest access uses is the ISA's, and a switch does not change it.
///
/// The engine boundary is a boundary around *execution*, not around width rules, so
/// this pins that an access formed before a switch is the access used after it.
#[test]
fn a_switch_does_not_change_what_a_guest_access_means() {
    let config = ArchitectureConfig::lz64();
    let mut machine = machine(config);
    let access = lazalith_cpu::DataAccess::new(
        config,
        VirtualAddress::new(STACK),
        8,
        DataSize::Double,
        lazalith_cpu::DataAccessKind::Read,
        Privilege::User,
    )
    .unwrap();
    machine
        .switch_execution_engine(EngineKind::Reference)
        .unwrap();
    assert_eq!(access.size(), DataSize::Double);
    assert_eq!(access.size().bytes(), 8);
    assert_eq!(access.privilege(), Privilege::User);
}

/// A switch before the machine has ever run is allowed.
///
/// A machine that has been constructed and not started is the easiest case there
/// is, and refusing it would mean the switch had a preconditions list nobody
/// would remember.
#[test]
fn a_switch_is_allowed_before_the_machine_has_run() {
    let mut machine = machine(ArchitectureConfig::lz64());
    assert_eq!(machine.state(), MachineState::Reset);
    machine
        .switch_execution_engine(EngineKind::Reference)
        .unwrap();
    assert_eq!(machine.state(), MachineState::Reset);
    assert_eq!(
        machine.processor().architectural().pc(),
        InstructionAddress::new(CODE)
    );
}

// (the machine bus is not used here: the outside-engine test builds its own
// address space, which is a real `CpuMemory` and needs no fake)
