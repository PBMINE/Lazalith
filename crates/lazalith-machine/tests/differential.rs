//! Step 85: differential testing.
//!
//! # What is being compared, and why it is not the comparison the step names
//!
//! The step asks for a `ReferenceInterpreter` checked against *an optimised
//! emulator*. There is no optimised emulator: step 93 introduces one, and until
//! then this repository has exactly one instruction executor —
//! `lazalith_cpu::ReferenceInterpreter`, which `LazalithMachine` calls.
//!
//! Building a second executor here to compare against would be a way of writing
//! step 93 badly, and a differential test between two implementations written in
//! the same sitting catches less than the harness it needs anyway. So what this
//! file does is build the *harness* step 85 asks for, and point it at the two
//! genuinely independent paths that exist:
//!
//! | | what it is | what a disagreement would mean |
//! |---|---|---|
//! | **bare** | `ReferenceInterpreter` against a flat byte array, stepped directly | — |
//! | **machine** | `LazalithMachine` against a real `AddressSpace`, a `Bus`, a region table, a `VirtualClock` and a device | the bus, the address space, the region permissions or the clock disagree with the processor |
//!
//! The processor code is shared, so this cannot find a bug *in* the interpreter —
//! step 93's differential will, once there are two. What it can find, and does
//! find by construction, is a disagreement about anything the machine adds on top:
//! an address translation, a data size, a permission, a fault classification, a
//! flag the machine recomputes, a clock the machine advances. Every one of those is
//! a layer where the same instruction can mean two different things, and none of
//! them is covered by stepping the interpreter in isolation.
//!
//! # What is compared
//!
//! Everything the step lists that both paths can produce, and it is compared
//! **after every step** rather than at the end. Comparing at the end finds programs
//! that end differently; comparing every step finds the *instruction* that made
//! them differ, which is the difference between a bug report and a puzzle.
//!
//! - registers, the program counter, and the status register's flags
//! - the whole of memory both paths can see
//! - the outcome of each step, and the machine's virtual time
//! - device state, for the corpus that touches a device
//!
//! # The corpus
//!
//! Curated programs for the things a random sequence will not reach — a call, a
//! return, a trap, a fault, a device write — and *random* instruction sequences for
//! the rest, drawn from the same fixed seed corpus as step 84. A random sequence
//! that faults is compared too: a fault is an answer, and two paths that disagree
//! about whether an access faulted disagree about the program's behaviour.

use lazalith_cpu::{
    ArchitecturalState, CpuFault, CpuFaultCause, CpuMemory, DataAccess, DataAccessKind, EngineKind,
    ExecutionEngine, OutcomeApplication, Privilege, Processor, ReferenceInterpreter, StepResult,
    TrapCause,
};
use lazalith_devices::{ConsoleDevice, DeviceId, DeviceManager};
use lazalith_isa::{DataSize, Instruction, Opcode, Operand, encode};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup, MachineState};
use lazalith_memory::{AccessType, MemoryFault, MemoryRegion, RegionPermissions};
use lazalith_properties::{Case, Gen, SEEDS};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, RegisterIndex,
    VirtualAddress,
};

/// Where the bare path puts its code and its data.
///
/// Four regions, disjoint and in order, because the machine refuses to map a region
/// that overlaps another and the flat path has no regions at all. A layout that only
/// *nearly* fits is a confusing failure, so the sizes are here rather than spelled
/// into the calls.
const CODE: u64 = 0x000;
const CODE_LENGTH: u64 = 0x400;
const DATA: u64 = 0x400;
const DATA_LENGTH: u64 = 0x400;
/// The stack, for a program that uses one.
const STACK: u64 = 0x800;
const STACK_LENGTH: u64 = 0x200;
/// The console device, for a program that writes to one.
const DEVICE: u64 = 0x1000;

const MODES: [ArchitectureConfig; 2] = [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()];

/// Readable, writable, not executable, not user-accessible.
const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);
/// Readable, not writable, executable, user-accessible.
const RX: RegionPermissions = RegionPermissions::new(true, false, true, true);
/// Not readable, writable, not executable, user-accessible: a device window.
const DEVICE_WINDOW: RegionPermissions = RegionPermissions::new(false, true, false, true);

/// One program: a mode, some bytes, and where it starts.
struct Program {
    config: ArchitectureConfig,
    code: Vec<u8>,
    data: Vec<u8>,
}

/// What the two paths are compared on after each step.
///
/// A snapshot rather than a borrow, because the two paths are different types and
/// the comparison has to be about the *values* rather than the objects. It is also
/// what a failure message prints, so every field here is something a person reads.
#[derive(Debug, Eq, PartialEq)]
struct Observation {
    registers: Vec<u64>,
    pc: u64,
    sp: u64,
    flags: u64,
    halted: bool,
    trapped: bool,
    fault: Option<String>,
    time: u64,
    console: Vec<u8>,
}

impl Observation {
    /// Where the two paths first disagreed, if they did.
    fn difference(&self, other: &Observation) -> Option<String> {
        if self.registers != other.registers {
            return Some(format!(
                "registers: bare {:#x?} machine {:#x?}",
                self.registers, other.registers
            ));
        }
        if self.pc != other.pc {
            return Some(format!("pc: bare {:#x} machine {:#x}", self.pc, other.pc));
        }
        if self.sp != other.sp {
            return Some(format!("sp: bare {:#x} machine {:#x}", self.sp, other.sp));
        }
        if self.flags != other.flags {
            return Some(format!(
                "flags: bare {:#x} machine {:#x}",
                self.flags, other.flags
            ));
        }
        if self.halted != other.halted {
            return Some(format!(
                "halted: bare {} machine {}",
                self.halted, other.halted
            ));
        }
        if self.trapped != other.trapped {
            return Some(format!(
                "trapped: bare {} machine {}",
                self.trapped, other.trapped
            ));
        }
        if self.fault != other.fault {
            return Some(format!(
                "fault: bare {:?} machine {:?}",
                self.fault, other.fault
            ));
        }
        if self.time != other.time {
            return Some(format!("time: bare {} machine {}", self.time, other.time));
        }
        None
    }
}

/// A byte array that behaves like the machine's memory, for the bare path.
///
/// The permissions are the *same region table* the machine is given, and that is
/// the point of this type rather than an accident. A flat array with no permissions
/// would let a program store into the code region, and the machine would refuse —
/// so a program that writes to read-only memory would "differ" on the two paths for
/// a reason that is the machine working correctly. Comparing them would compare a
/// permission model against its absence.
///
/// So the flat path answers the same three questions the region table answers, over
/// the same regions, and a fault here means what a fault there means.
struct Flat {
    bytes: Vec<u8>,
    regions: Vec<(u64, u64, RegionPermissions)>,
}

impl Flat {
    fn of(program: &Program) -> Self {
        Self {
            bytes: program_bytes(program),
            regions: vec![
                (CODE, CODE_LENGTH, RX),
                (DATA, DATA_LENGTH, RW),
                (STACK, STACK_LENGTH, RW),
                (DEVICE, 0x100, DEVICE_WINDOW),
            ],
        }
    }

    /// The permissions at an address, or `None` when nothing is mapped there.
    fn permissions(&self, address: u64) -> Option<RegionPermissions> {
        self.regions
            .iter()
            .find(|(start, length, _)| address >= *start && address < *start + *length)
            .map(|(_, _, permissions)| *permissions)
    }
}
impl CpuMemory for Flat {
    type Error = MemoryFault;

    fn fetch_instruction(
        &self,
        _config: ArchitectureConfig,
        pc: InstructionAddress,
        _privilege: Privilege,
    ) -> Result<[u8; 8], MemoryFault> {
        let address = pc.as_u64();
        match self.permissions(address) {
            Some(permissions) if permissions.read && permissions.execute => {}
            // Nothing mapped, or mapped without the right: both are a fetch the
            // program is not allowed to make, and both are faults.
            _ => return Err(fault(address, AccessType::Fetch, 8)),
        }
        // A fetch is eight bytes wide whatever the mode, because an instruction is,
        // and the trailing bytes of a narrow fetch come from the same array.
        let value = self.read(address, 8)?;
        Ok(value.to_le_bytes())
    }

    fn read_data(&mut self, access: DataAccess) -> Result<u64, MemoryFault> {
        let size = u64::from(access.size().bytes());
        let address = access.address().as_u64();
        match self.permissions(address) {
            Some(permissions) if permissions.read => {}
            _ => return Err(fault(address, AccessType::Data(DataAccessKind::Read), size)),
        }
        self.read(address, size)
    }

    fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), MemoryFault> {
        let address = access.address().as_u64();
        let size = u64::from(access.size().bytes());
        match self.permissions(address) {
            Some(permissions) if permissions.write => {}
            _ => {
                return Err(fault(
                    address,
                    AccessType::Data(DataAccessKind::Write),
                    size,
                ));
            }
        }
        // Bounds-checked rather than sliced: a write past the end of the flat array is
        // a *fault* on this path and a fault is something the differential compares. A
        // panic would be a failure of the harness for a program that simply ran off
        // the end, which is the one thing this corpus is supposed to allow.
        let start = address as usize;
        let end = start.saturating_add(usize::try_from(size).unwrap_or(usize::MAX));
        if end > self.bytes.len() {
            return Err(fault(
                address,
                AccessType::Data(DataAccessKind::Write),
                size,
            ));
        }
        let bytes = value.to_le_bytes();
        self.bytes[start..end].copy_from_slice(&bytes[..usize::try_from(size).unwrap_or(0)]);
        Ok(())
    }

    fn peek_stack(&self, access: DataAccess) -> Result<u64, MemoryFault> {
        self.read(access.address().as_u64(), u64::from(access.size().bytes()))
    }
}

impl Flat {
    fn read(&self, address: u64, size: u64) -> Result<u64, MemoryFault> {
        let start = address as usize;
        let end = start + size as usize;
        if end > self.bytes.len() {
            return Err(fault(address, AccessType::Data(DataAccessKind::Read), size));
        }
        let mut value = 0u64;
        for (index, byte) in self.bytes[start..end].iter().enumerate() {
            value |= u64::from(*byte) << (8 * index);
        }
        Ok(value)
    }
}

/// A fault the flat memory raises, built from its public fields.
///
/// `MemoryFault::new` is crate-private on purpose -- a fault is the memory.s to raise and
/// not a caller.s to invent -- so the fields are public and a test builds one
/// directly. What matters for the comparison is the *kind*, and `Unmapped` is the
/// one a flat array with a short read produces.
fn fault(address: u64, access: AccessType, size: u64) -> MemoryFault {
    MemoryFault {
        address: lazalith_memory::MemoryAddress::Virtual(VirtualAddress::new(address)),
        access,
        size: lazalith_memory::AccessSize::Bytes(size),
        pc: None,
        privilege: None,
        kind: lazalith_memory::MemoryFaultKind::Unmapped,
    }
}

/// Runs a program on the bare interpreter and records what it did, step by step.
///
/// **Virtual time is accumulated here from each retired instruction's own reported
/// cost.** The bare path has no machine and therefore no `VirtualClock`, but the cost
/// is not the machine's — it is the ISA's, and the engine reports it. So the bare path
/// sums the same numbers the machine charges, which is what lets the two be compared on
/// time at all. Before the clock fix this comparison was meaningless in one direction:
/// the machine reported 0 for everything, so "they agree" meant "time is not modelled".
fn run_bare(program: &Program, steps: u64) -> (Vec<Observation>, Vec<Vec<u8>>) {
    let state = ArchitecturalState::new(
        program.config,
        InstructionAddress::new(CODE),
        VirtualAddress::new(STACK),
        0,
    )
    .expect("a fresh architectural state is valid");
    let mut cpu = Processor::new(state);
    let mut memory = Flat::of(program);

    let mut observations = Vec::new();
    let mut memory_watch = Vec::new();
    let mut cycles = 0u64;
    for _ in 0..steps {
        let outcome = ReferenceInterpreter::new().step(&mut cpu, &mut memory);
        cycles += outcome
            .as_ref()
            .map(|step| u64::from(step.cycles))
            .unwrap_or(0);
        observations.push(observe_bare(&cpu, &outcome, cycles));
        memory_watch.push(memory.bytes.clone());
        // A stopped processor is not stepped again. The bare path keeps going and
        // reports a halt as a fault on the next step, while the machine refuses the
        // step outright — two different ways of saying "this program has finished" —
        // so the comparison stops where the program stops, and
        // `both_paths_stop_together` says that they stopped together.
        if !matches!(outcome, Ok(ref step) if step.outcome() == OutcomeApplication::Continue) {
            break;
        }
    }
    (observations, memory_watch)
}

/// Runs a program on the machine and records what it did, step by step.
fn run_machine(program: &Program, steps: u64) -> (Vec<Observation>, Vec<Vec<u8>>) {
    run_machine_on_engine(program, steps, EngineKind::Reference)
}

/// Runs `program` on a machine whose engine is `kind`, switched on every step.
fn run_machine_on_engine(
    program: &Program,
    steps: u64,
    kind: EngineKind,
) -> (Vec<Observation>, Vec<Vec<u8>>) {
    let mut devices = DeviceManager::new();
    devices
        .insert(
            DeviceId::new(1),
            ConsoleDevice::new(4096).expect("a console of 4 KiB"),
        )
        .expect("a console device fits");
    let setup = MachineSetup {
        config: program.config,
        devices,
        regions: vec![
            MemoryRegion::ram(program.config, PhysicalAddress::new(DATA), DATA_LENGTH, RW)
                .expect("a data region"),
            MemoryRegion::ram(
                program.config,
                PhysicalAddress::new(STACK),
                STACK_LENGTH,
                RW,
            )
            .expect("a stack region"),
        ],
        pc: InstructionAddress::new(CODE),
        sp: VirtualAddress::new(STACK),
        status: 0,
        initial_time: CycleCount::new(0),
    };
    let mut machine = LazalithMachine::new(setup).expect("the machine starts");
    machine
        .load_region(
            MemoryRegion::rom(
                program.config,
                PhysicalAddress::new(CODE),
                &code_bytes(program),
                RX,
            )
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
    // A program with no data writes nothing, and `initialize` refuses a zero-length
    // write rather than treating it as a no-op -- correctly, since a zero-byte write
    // has no address to be aligned to.
    if !program.data.is_empty() {
        machine
            .load_bytes(PhysicalAddress::new(DATA), &program.data)
            .expect("the data is loaded");
    }
    // A machine is created rather than running: `reset` is what takes it out of
    // `Created` and into a state a step is legal from, and a step from `Created` is
    // refused. The bare path has no such state -- it is a processor, not a machine --
    // which is one of the differences this test exists to notice.
    machine.reset();
    // A trap vector, because a fault or a trap needs somewhere to go and the
    // machine refuses to enter a trap with no vector. The vector is never reached in
    // this corpus, because nothing returns from one.
    machine
        .set_trap_vector(InstructionAddress::new(CODE))
        .expect("a trap vector inside the code region");
    let mut observations = Vec::new();
    let mut memory_watch = Vec::new();
    for step in 0..steps {
        // Switched on every other step, so the architectural state crosses the engine
        // boundary repeatedly rather than once.
        //
        // **Not while the machine is faulted, and that is a B3 decision this stage
        // leaves alone.** `switch_execution_engine` refuses a `Faulted` machine with
        // "an engine switch is not a reset", and a program that faults into a machine
        // with no way to enter a trap frame is genuinely dead. Relaxing that rule is
        // B23's question, not B21's, and it is recorded as a B23 prerequisite in
        // `docs/project-state.md` — a JIT cannot hand a faulted program back to an
        // interpreter while this rule stands, so B23 will have to answer it.
        if step % 2 == 0 && machine.state() != MachineState::Faulted {
            machine
                .switch_execution_engine(kind)
                .expect("the engine switches between steps");
        }
        let event = machine.step();
        observations.push(observe_machine(&machine, &event));
        let mut seen = Vec::new();
        for address in [DATA, STACK] {
            let mut window = vec![0u8; 64];
            let _ = machine.peek_memory(PhysicalAddress::new(address), &mut window);
            seen.extend_from_slice(&window);
        }
        memory_watch.push(seen);
    }
    (observations, memory_watch)
}

/// The code region.s bytes, padded to the region.s length.
///
/// The machine loads this as a ROM, so it is exactly the region and not one byte
/// more -- a region that ran into the data region would be refused as an overlap,
/// which is a confusing way to learn that a test.s fixture was too large.
///
fn code_bytes(program: &Program) -> Vec<u8> {
    let mut bytes = program.code.clone();
    bytes.resize(CODE_LENGTH as usize, 0);
    bytes
}

/// The whole flat image: the code at zero, the data where the machine puts it,
/// and zeros for everything else.
///
/// The two paths have to start from the same bytes or the comparison is about a
/// difference that was put there on purpose. The flat image is laid out exactly as
/// the machine.s regions are -- code, then data, then stack -- so a program that
/// writes to the stack is writing where the flat array has room for it.
fn program_bytes(program: &Program) -> Vec<u8> {
    // The whole address space the two paths share, gaps included. The device window is
    // at 0x1000 and the stack ends well before it, so the array is longer than the
    // sum of its regions and the gap between them is zeros -- which is also what
    // an unmapped address on the machine looks like from the program.s side.
    let total = (DEVICE + 0x100) as usize;
    let mut bytes = vec![0u8; total];
    bytes[..CODE_LENGTH as usize].copy_from_slice(&code_bytes(program));
    let data = program.data.len().min(DATA_LENGTH as usize);
    let start = DATA as usize;
    bytes[start..start + data].copy_from_slice(&program.data[..data]);
    bytes
}

fn observe_bare(
    cpu: &Processor,
    outcome: &Result<StepResult, CpuFault<MemoryFault>>,
    cycles: u64,
) -> Observation {
    let state = cpu.architectural();
    // The same three-way question the machine's observer asks, in the same words.
    // A trap *request* is not a fault: the processor is asking the machine for
    // something, and the machine's answer to the same request is a trap with a cause.
    // The two are compared on the guest's pc — which for a trap is the resume point
    // the outcome carries, not the instruction that trapped.
    let application = outcome.as_ref().map(|step| step.outcome());
    let (halted, trapped, fault, guest_pc) = match &application {
        Ok(OutcomeApplication::Continue) => (false, false, None, state.pc()),
        Ok(OutcomeApplication::Halted) => (true, false, None, state.pc()),
        Ok(OutcomeApplication::Trap { request, resume_pc }) => {
            let cause = match request {
                lazalith_cpu::TrapRequest::Syscall => "syscall",
                lazalith_cpu::TrapRequest::Software(_) => "software",
            };
            (false, true, Some(String::from(cause)), *resume_pc)
        }
        Err(fault) => (false, true, Some(cause_name(&fault.cause)), state.pc()),
    };
    Observation {
        registers: (0..RegisterIndex::COUNT)
            .map(|raw| state.registers().read_raw(raw).unwrap_or(0))
            .collect(),
        pc: guest_pc.as_u64(),
        sp: state.sp().as_u64(),
        flags: state.status().bits(),
        halted,
        trapped,
        fault,
        time: cycles,
        console: Vec::new(),
    }
}

fn observe_machine(
    machine: &LazalithMachine<ConsoleDevice>,
    event: &Result<MachineEvent, lazalith_machine::MachineError>,
) -> Observation {
    let state = machine.architectural_state();
    // The *guest* program counter, not the architectural one, and it comes from the
    // trap event rather than from the machine: a machine that has entered a trap is in
    // its trap frame with the *vector* in its pc, and the guest program is at the
    // resume point. Comparing the vector against the bare path.s next instruction would
    // report a difference where there is none, and the event already carries the
    // answer.
    let guest_pc = match event {
        Ok(MachineEvent::Trapped { event }) => event.resume_pc,
        _ => state.pc(),
    };
    // The machine *enters* a trap where the bare processor *returns* a fault, so
    // the trap is where the cause has to be read from. Leaving it out would compare
    // "trapped" with "trapped" and miss every fault that the two report in two
    // different places.
    let (halted, trapped, fault) = match event {
        Ok(MachineEvent::Stepped { .. }) => (false, false, None),
        Ok(MachineEvent::Halted) => (true, false, None),
        Ok(MachineEvent::Trapped { event }) => (false, true, Some(trap_cause_name(&event.cause))),
        Err(error) => (false, true, Some(format!("{error:?}"))),
    };
    Observation {
        registers: (0..RegisterIndex::COUNT)
            .map(|raw| state.registers().read_raw(raw).unwrap_or(0))
            .collect(),
        pc: guest_pc.as_u64(),
        sp: state.sp().as_u64(),
        flags: state.status().bits(),
        halted,
        trapped,
        fault,
        time: machine.clock().elapsed().as_u64(),
        console: machine
            .devices()
            .device(DeviceId::new(1))
            .map(|console| console.output().to_vec())
            .unwrap_or_default(),
    }
}

/// A name for a processor fault, matching the machine's vocabulary where it can.
///
/// The two paths describe the same event in different words, and the point of the
/// comparison is the *event*, so the mapping is coarse on purpose. A fetch that hits
/// unmapped memory is `CpuFaultCause::Fetch` here and `TrapCause::Unmapped` on the
/// machine; both are "memory", and both are reported as such.
///
/// Anything that is not a memory event keeps its own name, because a difference in
/// those is a difference worth reading.
fn cause_name(cause: &CpuFaultCause<MemoryFault>) -> String {
    match cause {
        CpuFaultCause::Memory { .. } | CpuFaultCause::Fetch(_) => String::from("memory"),
        // A data access that failed to *build* is a width problem -- an
        // unsupported size, a width the target does not have, a misaligned
        // displacement -- and the machine names all three `width`. Splitting it
        // here rather than lumping it with `memory` is what makes a divide by zero
        // and a misaligned load compare equal to their counterparts.
        CpuFaultCause::DataAccess(_) | CpuFaultCause::Width(_) => String::from("width"),
        // A program counter that could not be advanced is a width problem too: the
        // next instruction address left the address space.
        CpuFaultCause::NextPc(_) => String::from("width"),

        CpuFaultCause::Halted => String::from("halted"),
        CpuFaultCause::PrivilegeViolation => String::from("privilege"),
        other => format!("{other:?}"),
    }
}
/// Compares one program on both paths, step by step.
fn differential(program: &Program, steps: u64) -> Result<(), String> {
    let (bare, bare_memory) = run_bare(program, steps);
    let (machine, machine_memory) = run_machine(program, steps);
    for (step, (left, right)) in bare.iter().zip(machine.iter()).enumerate() {
        if let Some(difference) = left.difference(right) {
            let pc = left.pc;
            return Err(format!(
                "step {step} at pc {pc:#x}: {difference}\n  \
                 bare:    {left:?}\n  machine: {right:?}\n  \
                 program: {}",
                describe(program)
            ));
        }
    }
    // Memory, compared over the *same addresses* on both paths. The bare path is one
    // flat array laid out like the machine's regions and the machine has regions of
    // its own, so both are read at the data and stack addresses — comparing the bare
    // array's first bytes would be comparing code against data and would always
    // differ.
    const WINDOW: usize = 64;
    for (step, (left, right)) in bare_memory.iter().zip(machine_memory.iter()).enumerate() {
        for (index, address) in [DATA, STACK].into_iter().enumerate() {
            let start = address as usize;
            let bare = &left[start..start + WINDOW];
            let machine = &right[index * WINDOW..(index + 1) * WINDOW];
            if bare != machine {
                return Err(format!(
                    "step {step}: memory at {address:#x} differs\n  bare:    {bare:02x?}\n  \
                     machine: {machine:02x?}\n  program: {}",
                    describe(program)
                ));
            }
        }
    }
    Ok(())
}

fn describe(program: &Program) -> String {
    format!(
        "{:?}, {} bytes of code",
        program.config.word_width(),
        program.code.len()
    )
}

/// Assembles a list of instructions into a program's code bytes.
fn assemble(config: ArchitectureConfig, instructions: &[Instruction]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for instruction in instructions {
        bytes.extend_from_slice(&encode(config, instruction).expect("an instruction encodes"));
    }
    bytes
}

fn here(config: ArchitectureConfig, opcode: Opcode, operands: &[Operand]) -> Instruction {
    Instruction::new(config, opcode, operands).expect("the operand list fits the opcode")
}

fn reg(index: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(index).expect("below sixteen"))
}

/// A memory operand: a base register and a displacement.
fn memory(base: u8, displacement: i32) -> Operand {
    Operand::Memory {
        base: RegisterIndex::try_from(base).expect("below sixteen"),
        displacement,
    }
}

/// A `DataSize` the mode can actually load and store.
fn wide(config: ArchitectureConfig) -> DataSize {
    if config.word_width() == lazalith_types::WordWidth::W32 {
        DataSize::Word
    } else {
        DataSize::Double
    }
}

/// The programs a random sequence will not reach.
///
/// A call and a return, a trap, a fault on unmapped memory, a store to a
/// read-only region, a write to the device, and an arithmetic chain that sets and
/// clears every flag. Each is here because a random draw of a few instructions will
/// not produce it, and each is a case where the machine adds something the bare
/// interpreter does not have — a stack, a region table, a device, a clock.
///
/// The operand lists are the ISA's own shapes and are written out rather than
/// generated, because a corpus built by asking the format what it wants is a corpus
/// of the *builder's* opinion rather than of a program somebody meant.
fn curated(config: ArchitectureConfig) -> Vec<Program> {
    let mut programs = Vec::new();
    let step = i32::try_from(8 * u64::from(config.word_bytes())).unwrap_or(8);

    // Arithmetic and flags: a carry out of an add, a borrow from a sub, a zero
    // result, and a comparison that sets the condition a branch reads.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(config, Opcode::Li, &[reg(0), Operand::Immediate(-1)]),
                here(config, Opcode::Add, &[reg(1), reg(0), reg(0)]),
                here(config, Opcode::Sub, &[reg(2), reg(0), reg(0)]),
                here(config, Opcode::Sub, &[reg(3), reg(2), reg(2)]),
                here(config, Opcode::Cmp, &[reg(0), reg(1)]),
                here(
                    config,
                    Opcode::Br,
                    &[
                        Operand::Condition(lazalith_isa::Condition::Eq),
                        Operand::Immediate(step * 2),
                    ],
                ),
                here(config, Opcode::Li, &[reg(4), Operand::Immediate(11)]),
                here(config, Opcode::Halt, &[]),
            ],
        ),
        data: Vec::new(),
    });

    // A call and a return: the stack and the frame are the machine's, and the
    // subroutine is the fifth instruction in, so the displacement is four steps.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(config, Opcode::Li, &[reg(9), Operand::Immediate(5)]),
                here(config, Opcode::Li, &[reg(0), Operand::Immediate(1)]),
                here(config, Opcode::Call, &[Operand::Immediate(step * 4)]),
                here(config, Opcode::Halt, &[]),
                here(config, Opcode::Li, &[reg(1), Operand::Immediate(9)]),
                here(config, Opcode::Ret, &[]),
            ],
        ),
        data: Vec::new(),
    });

    // A fault: a load from an address nothing is mapped at.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(
                    config,
                    Opcode::Ldz,
                    &[reg(0), memory(1, 4096), Operand::DataSize(wide(config))],
                ),
                here(config, Opcode::Halt, &[]),
            ],
        ),
        data: Vec::new(),
    });

    // A store into the code region, which is readable and executable and not
    // writable. The machine refuses it on permissions.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(
                    config,
                    Opcode::St,
                    &[reg(0), memory(1, 0), Operand::DataSize(wide(config))],
                ),
                here(config, Opcode::Halt, &[]),
            ],
        ),
        data: Vec::new(),
    });

    // A write to the device window, so the console is compared too.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(config, Opcode::Li, &[reg(0), Operand::Immediate(0x5a)]),
                here(
                    config,
                    Opcode::St,
                    &[
                        reg(0),
                        memory(1, i32::try_from(DEVICE).unwrap_or(0)),
                        Operand::DataSize(DataSize::Byte),
                    ],
                ),
                here(config, Opcode::Halt, &[]),
            ],
        ),
        data: vec![0x5a],
    });

    // A trap: the program asks for one, and both paths have to enter it.
    programs.push(Program {
        config,
        code: assemble(
            config,
            &[
                here(config, Opcode::Trap, &[Operand::Immediate(3)]),
                here(config, Opcode::Halt, &[]),
            ],
        ),
        data: Vec::new(),
    });

    // Halted at once. Both paths stop, and they stop for different reasons — the
    // machine refuses to step a halted machine and the bare processor reports a
    // halt as a fault — which is the difference `both_paths_stop_together` is about.
    programs.push(Program {
        config,
        code: assemble(config, &[here(config, Opcode::Halt, &[])]),
        data: Vec::new(),
    });

    programs
}

/// A random instruction sequence, for the cases a curated list does not think of.
struct Random {
    config: ArchitectureConfig,
    code: Vec<u8>,
}

impl Case for Random {
    fn generate(source: &mut Gen) -> Self {
        let config = match source.bool() {
            true => ArchitectureConfig::lz32(),
            false => ArchitectureConfig::lz64(),
        };
        let count = 1 + source.below(12) as usize;
        let mut instructions = Vec::with_capacity(count);
        for _ in 0..count {
            // Opcodes that cannot damage the machine's own state and cannot leave
            // the code region. A random control transfer either runs off the end or
            // loops forever, and neither makes the two paths disagree — it makes the
            // *test* slow to notice that they agree. The corpus above is where the
            // control transfers are covered.
            let opcode = source
                .choice(SAFE_OPCODES)
                .expect("the opcode list is never empty");
            let operands: Vec<Operand> = opcode
                .definition()
                .operands()
                .iter()
                .map(|definition| operand_for(source, definition.kind))
                .collect();
            instructions.push(
                Instruction::new(config, opcode, &operands)
                    .unwrap_or_else(|_| here(config, Opcode::Halt, &[])),
            );
        }
        Self {
            config,
            code: assemble(config, &instructions),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{:?}, {} instructions",
            self.config.word_width(),
            self.code.len() / 8
        )
    }
}

/// Opcodes a random program may use.
const SAFE_OPCODES: &[Opcode] = &[
    Opcode::Li,
    Opcode::Addi,
    Opcode::Add,
    Opcode::Sub,
    Opcode::Subi,
    Opcode::Mul,
    Opcode::Divu,
    Opcode::Remu,
    Opcode::And,
    Opcode::Or,
    Opcode::Xor,
    Opcode::Not,
    Opcode::Shl,
    Opcode::Shr,
    Opcode::Sar,
    Opcode::Cmp,
    Opcode::Mov,
    Opcode::Getpc,
    Opcode::Ldz,
    Opcode::Lds,
    Opcode::St,
    Opcode::Halt,
];

fn operand_for(source: &mut Gen, kind: lazalith_isa::OperandKind) -> Operand {
    use lazalith_isa::OperandKind;
    match kind {
        OperandKind::Register => reg(source.next_u8() % 8),
        OperandKind::Immediate => Operand::Immediate(source.interesting_u32() as i32),
        OperandKind::Memory => memory(source.next_u8() % 8, source.interesting_u32() as i32),
        OperandKind::DataSize => {
            Operand::DataSize(source.choice(DataSize::ALL).unwrap_or(DataSize::Byte))
        }
        OperandKind::Condition => Operand::Condition(
            source
                .choice(lazalith_isa::Condition::ALL)
                .unwrap_or(lazalith_isa::Condition::Al),
        ),
        OperandKind::Control => Operand::Control(
            source
                .choice(lazalith_isa::ControlRegister::ALL)
                .unwrap_or(lazalith_isa::ControlRegister::Tvec),
        ),
    }
}

/// Every curated program runs the same on both paths, step for step.
#[test]
fn the_curated_corpus_agrees_step_for_step() {
    for config in MODES {
        for program in curated(config) {
            if let Err(difference) = differential(&program, 32) {
                panic!("{difference}");
            }
        }
    }
}

/// A random program runs the same on both paths, step for step.
///
/// The property form, over the same fixed seed corpus as step 84. A random program
/// that faults is compared too: a fault is an answer, and two paths that disagree
/// about whether an access faulted disagree about what the program does.
#[test]
fn a_random_program_agrees_step_for_step() {
    for (index, seed) in SEEDS.iter().enumerate().take(48) {
        let mut source = Gen::seeded(*seed);
        let random = Random::generate(&mut source);
        let program = Program {
            config: random.config,
            code: random.code,
            data: Vec::new(),
        };
        if let Err(difference) = differential(&program, 24) {
            panic!(
                "seed {index} ({seed:#018x}) disagreed:\n{difference}\n  \
                 re-run with Gen::seeded({seed:#018x})"
            );
        }
    }
}

/// The two paths stop at the same place, not merely with the same registers.
///
/// A run that faults has to fault at the same *step* on both paths. The two stop
/// for different reasons at the end — the machine refuses to step a halted machine
/// and the bare processor reports a halt as a fault on the next step — so the
/// comparison is on *when* each stopped, which is the part a program can observe.
#[test]
fn both_paths_stop_together() {
    for config in MODES {
        for program in curated(config) {
            let (bare, _) = run_bare(&program, 32);
            let (machine, _) = run_machine(&program, 32);
            let bare_stop = bare
                .iter()
                .position(|observation| observation.halted || observation.trapped);
            let machine_stop = machine
                .iter()
                .position(|observation| observation.halted || observation.trapped);
            assert_eq!(
                bare_stop,
                machine_stop,
                "the two paths stopped at different steps for {}",
                describe(&program)
            );
        }
    }
}

/// The machine's virtual time is exactly what its driver advanced it by.
///
/// Stated rather than skipped, because the step lists virtual time among the things
/// to compare and the honest answer is about *who owns the clock*. The clock belongs
/// to the machine and not to the processor: a step does not move it, and the
/// scheduler advances it by a quantum per process it runs. So the property is that
/// the clock is a function of the driver and of nothing else — which is what makes a
/// replay reproducible, and which a step-count property would have got wrong.
/// Virtual time is a function of what the guest executed, and of nothing else.
///
/// **This test asserted the opposite for the whole of B6–B19**, and it was right to
/// assert it at the time. It said: the clock belongs to the machine and not to the
/// processor, a step does not move it, and the *driver* advances it by a quantum per
/// process. The reasoning was good — a clock that moved by a host tick or a wall clock
/// would make a trace depend on the machine it ran on, which is the one thing a
/// reproduction must not do.
///
/// What it got wrong was the conclusion. Refusing to let the step move the clock did
/// not make the clock a function of the program; it made the clock a function of
/// *nothing*. A guest that called `time` read zero forever, and `sleep` never woke,
/// because nothing on the machine ever moved it.
///
/// So the property is restated rather than deleted, and the "reproducible" part of the
/// old reasoning is kept exactly: time now advances by **the ISA's cost for the
/// instruction that retired**, which is part of the instruction's definition and
/// therefore identical on every host, for every engine, at every speed. That is a
/// stronger form of reproducibility than the old one — the old clock was a function of
/// the driver's schedule, which *could* vary; this one is a function of the program.
#[test]
fn virtual_time_is_a_function_of_what_executed_and_of_nothing_else() {
    let mut any_execution_moved_the_clock = false;
    for config in MODES {
        for program in curated(config) {
            let (observations, _) = run_machine(&program, 8);
            for pair in observations.windows(2) {
                // Monotonic, and it moves unless the step faulted.
                //
                // **A faulted instruction is charged nothing, and that is the model
                // rather than a loophole.** A fault is a failed fetch or a rejected
                // operation: the instruction did not retire, so there is nothing to
                // charge for, and charging it anyway would mean a program could make
                // virtual time pass by faulting in a loop. So the property is
                // "non-decreasing, and strictly increasing unless the step faulted",
                // which is two assertions rather than one because one would be false.
                assert!(
                    pair[1].time >= pair[0].time,
                    "stepping {} moved virtual time backwards, from {} to {}",
                    describe(&program),
                    pair[0].time,
                    pair[1].time
                );
                if pair[1].fault.is_none() {
                    assert!(
                        pair[1].time > pair[0].time,
                        "stepping {} executed an instruction for no cycles at all: \
                         time stayed at {}",
                        describe(&program),
                        pair[1].time
                    );
                }
            }
            // Checked across the corpus rather than per program, because a program
            // that faults on its first instruction legitimately never moves the clock
            // — there is nothing to charge. Requiring every program to move would be
            // requiring every program to execute.
            any_execution_moved_the_clock |= observations
                .last()
                .is_some_and(|observation| observation.time > 0);
        }
    }
    assert!(
        any_execution_moved_the_clock,
        "a corpus of programs ran and not one of them moved virtual time"
    );
}

/// The same program costs the same virtual time every time it is run.
///
/// **The half of reproducibility that the old test was really after.** If a clock
/// moved by a host tick this would fail on a busy machine and pass on an idle one,
/// which is the worst way for a test to fail. Running the same program twice on the
/// same machine is a weaker check than running it on two machines, and it is the one
/// available here; the bare-versus-machine comparison in `run_bare` is the other half,
/// since the bare path has no clock at all and still agrees.
#[test]
fn the_same_program_costs_the_same_virtual_time_every_time() {
    for config in MODES {
        for program in curated(config) {
            let (first, _) = run_machine(&program, 8);
            let (second, _) = run_machine(&program, 8);
            assert_eq!(
                first.iter().map(|o| o.time).collect::<Vec<_>>(),
                second.iter().map(|o| o.time).collect::<Vec<_>>(),
                "two runs of {} disagreed about what it cost",
                describe(&program)
            );
        }
    }
}

/// A device write reaches the console, on the machine.
///
/// The bare path has no devices, so this is not a differential: it is the check
/// that the device half of the step's list is *tested* rather than claimed. The byte
/// the program stored is the byte the console received, and the console is the
/// program's only window on the device.
#[test]
fn a_device_write_reaches_the_console() {
    for config in MODES {
        let program = curated(config)
            .into_iter()
            .find(|program| !program.data.is_empty())
            .expect("the corpus has a device program");
        let (observations, _) = run_machine(&program, 8);
        let written = observations
            .iter()
            .rev()
            .find_map(|observation| observation.console.last().copied());
        assert_eq!(
            written,
            Some(0x5a),
            "the console saw {:?} for {}",
            written.map(|byte| format!("{byte:#x}")),
            describe(&program)
        );
    }
}

/// A name for a trap cause, coarse enough to compare and fine enough to be useful.
///
/// Deliberately coarse: the bare path's `CpuFaultCause` and the machine's `TrapCause`
/// describe the same events in different vocabularies, and a comparison that tried to
/// match them field by field would be a translation table rather than a differential.
/// What matters is that *both* paths name the same kind of event: a memory fault is a
/// memory fault on both, and a software trap is a software trap on both.
fn trap_cause_name(cause: &TrapCause) -> String {
    // Coarse on purpose, and the coarseness is the point: the two paths name the same
    // events in different vocabularies, and a comparison that matched them field by
    // field would be a translation table rather than a differential. What matters is
    // that a memory fault is called a memory fault on both sides.
    match cause {
        TrapCause::Unmapped | TrapCause::Permission | TrapCause::DeviceAccess => {
            String::from("memory")
        }
        // The machine.s half of the width category, which the bare processor calls
        // `Width` without splitting it.
        TrapCause::Alignment
        | TrapCause::AddressOverflow
        | TrapCause::DivideByZero
        | TrapCause::DivisionOverflow => String::from("width"),
        TrapCause::SoftwareTrap => String::from("software"),
        TrapCause::Syscall => String::from("syscall"),
        other => format!("{other:?}"),
    }
}

// -- B21: the two engines are interchangeable, and the machine says so -------

/// A fault description with the Rust source location removed.
///
/// **Two engines genuinely differ here, and it is not a guest-visible difference.** A
/// `CpuFault` carries a `Location` naming the `.rs` file and line it was raised at, and
/// the reference raises it in `interpreter.rs` while the optimised engine raises it in
/// `fast.rs`. That is a fact about this repository's source, not about the machine: no
/// guest can observe it, and §10 asks for *guest-visible* behaviour to be preserved.
///
/// So the location is cut before the two are compared, and the cut is written out here
/// rather than hidden inside a helper called `normalize`. Everything else about the fault
/// — its cause, the address, the access, the size, the resume PC, and the double-trap
/// that follows — is still compared exactly.
fn without_site(fault: &Option<String>) -> Option<String> {
    fault
        .as_ref()
        .map(|text| match text.find(", site: Location") {
            Some(at) => text[..at].to_string(),
            None => text.clone(),
        })
}

/// The machine runs the same program identically on either engine.
///
/// **This is §10's "the VM must not care whether execution is performed by the
/// interpreter or JIT", checked for the two engines that exist.** The engine is switched
/// before every step, so the architectural state is handed from one engine to the other
/// and back over and over, and the observations still match a run that never switched.
///
/// Compared against the *reference-only* run rather than against another switched run, so
/// the oracle is the engine §10 makes authoritative and not a second opinion from the
/// same code path.
#[test]
fn the_machine_runs_identically_on_either_engine() {
    for config in MODES {
        for program in curated(config) {
            let (reference, reference_memory) = run_machine(&program, 32);
            let (optimized, optimized_memory) =
                run_machine_on_engine(&program, 32, EngineKind::Optimized);
            assert_eq!(
                reference.len(),
                optimized.len(),
                "the two engines stopped at different steps for {}",
                describe(&program)
            );
            for (index, (a, b)) in reference.iter().zip(optimized.iter()).enumerate() {
                assert_eq!(
                    a.registers,
                    b.registers,
                    "step {index} of {}: the engines left different registers",
                    describe(&program)
                );
                assert_eq!(
                    a.pc,
                    b.pc,
                    "step {index} of {}: different PCs",
                    describe(&program)
                );
                assert_eq!(
                    a.sp,
                    b.sp,
                    "step {index} of {}: different SPs",
                    describe(&program)
                );
                assert_eq!(
                    a.flags,
                    b.flags,
                    "step {index} of {}: different status registers",
                    describe(&program)
                );
                assert_eq!(
                    a.halted,
                    b.halted,
                    "step {index} of {}: one halted and the other did not",
                    describe(&program)
                );
                assert_eq!(
                    a.trapped,
                    b.trapped,
                    "step {index} of {}: one trapped and the other did not",
                    describe(&program)
                );
                assert_eq!(
                    a.time,
                    b.time,
                    "step {index} of {}: the engines charged different virtual time",
                    describe(&program)
                );
                assert_eq!(
                    a.console,
                    b.console,
                    "step {index} of {}: the devices saw different output",
                    describe(&program)
                );
                assert_eq!(
                    without_site(&a.fault),
                    without_site(&b.fault),
                    "step {index} of {}: the engines failed differently",
                    describe(&program)
                );
            }
            assert_eq!(
                reference_memory,
                optimized_memory,
                "and the two engines left memory differently for {}",
                describe(&program)
            );
        }
    }
}
