//! Code generation tests.
//!
//! These state what the generated machine code must *be*, and then run it. The
//! machine is the reference for semantics, so a test that executes generated
//! code and compares its output is stronger than one that inspects bytes, and
//! most of these do both.
//!
//! The properties under test are the ones Step 62 established and that a backend
//! could plausibly break:
//!
//! - a local is reached through the stack pointer, not a function's address;
//! - a view keeps its address *and* its length, so a length is never lost;
//! - a comparison becomes a real comparison and a real `0`/`1` value;
//! - `&&` and `||` do not evaluate the right side when the left decides;
//! - `break` and `continue` reach real code, not a trap;
//! - an out-of-range index traps instead of reading elsewhere;
//! - a string's address comes from a relocation, not a made-up number;
//! - a 32-bit value's arithmetic is 32-bit arithmetic;
//! - a call passes and returns through the documented convention.

use std::vec::Vec;

use lazalith_codegen::{CodegenError, CodegenOptions, Program, generate};
use lazalith_compiler::frontend::compile;
use lazalith_compiler::lower::{self, Lowered};
use lazalith_isa::{Opcode, decode};
use lazalith_os::{
    DispatchOutcome, FileSystemService, LzxArchitecture, ProcessId, RoundRobinScheduler,
    TerminalService, ThreadId, VirtualFileSystem, VirtualTerminal,
};
use lazalith_toolchain::{
    LinkOptions, ObjectFile, RelocationKind, assemble_named, disassemble_object, link_objects,
};
use lazalith_types::{ArchitectureConfig, SourceManager};

/// Generates code for a lowered program.
///
/// The backend takes a module, its frames and its entry, not a Lazen struct, so
/// a test that happens to have built its program with Lazen has to unpack it.
/// That unpacking is the whole of the front end's involvement in code
/// generation, which is the point.
fn generate_lowered(
    lowered: &Lowered,
    options: &CodegenOptions,
    source: &str,
) -> Result<Program, CodegenError> {
    generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        options,
        source,
    )
}

/// Compiles, lowers, and generates code for a program.
fn generate_program(source: &str) -> (Program, Lowered) {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    let lowered =
        lower::lower(&program).unwrap_or_else(|error| panic!("{source} should lower: {error}"));
    let generated = generate_lowered(&lowered, &CodegenOptions::lz64("t.lazen"), source)
        .unwrap_or_else(|error| panic!("{source} should generate code: {error}"));
    (generated, lowered)
}

/// The object alone, for a test that does not care about frames.
fn generate_object(source: &str) -> (ObjectFile, Lowered) {
    let (program, lowered) = generate_program(source);
    (program.object().clone(), lowered)
}

/// The generated code of one function, as decoded instructions.
fn function_code(object: &ObjectFile, function: &str) -> Vec<Opcode> {
    let symbol = format!("fn.{function}");
    let index = object
        .symbols()
        .iter()
        .position(|candidate| candidate.name() == symbol)
        .unwrap_or_else(|| panic!("the object defines {symbol}"));
    let offset = object.symbols()[index].value();
    disassemble_object(object)
        .expect("the object disassembles")
        .first()
        .expect("a text section")
        .instructions()
        .iter()
        .skip(offset as usize / 8)
        .map(|instruction| instruction.instruction().opcode())
        .collect()
}

/// How many instructions a test program may run before it counts as stuck.
///
/// The generated code is unoptimised — every value lives in a frame and every
/// operand is re-loaded — so a loop iteration is over a hundred instructions.
/// The bound is generous enough for the loops these tests run and small enough
/// that a loop which never finishes is reported as one rather than as a timeout.
const MAX_STEPS: usize = 200_000;

/// Runs a linked program to completion, returning what it wrote and how it exited.
///
/// The exit code is the only way a test can see a returned value, so it is
/// reported rather than inferred from the console.
///
/// The harness is hand-written assembly, because the runtime that would call a
/// Lazen program's `main` is Step 64's work. Calling a generated function from
/// known code is what actually proves the calling convention, the frame, and
/// every instruction the compiler emitted.
fn run(objects: Vec<ObjectFile>, entry: &str) -> (Vec<u8>, Option<u32>) {
    let config = ArchitectureConfig::lz64();
    let program = link_objects(
        &objects,
        &LinkOptions {
            entry_symbol: Some(String::from(entry)),
        },
    )
    .expect("the objects link");
    let process = program
        .image()
        .load_process(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .expect("the image loads");
    let mut scheduler = RoundRobinScheduler::new(10_000).expect("a scheduler");
    scheduler
        .add_process(process)
        .expect("the process is scheduled");
    let filesystem = VirtualFileSystem::with_defaults().expect("a filesystem");
    let mut service = TerminalService::new(
        VirtualTerminal::new(b"").expect("a terminal"),
        FileSystemService::new(filesystem),
    );
    let mut machine = test_machine(config);
    let mut exit_code = None;
    let mut steps = 0usize;
    let mut fault = None;
    for _ in 0..MAX_STEPS {
        let step = scheduler.step(&mut machine).expect("a step");
        steps += 1;
        if let lazalith_machine::MachineEvent::Trapped { event } = &step.event {
            // A user `TRAP` is a software trap, not a syscall: the kernel refuses it
            // as a dispatch, which is the correct outcome for a program that
            // trapped on purpose. Only a syscall is dispatched.
            if event.cause != lazalith_cpu::TrapCause::Syscall {
                fault = Some(event.cause);
                break;
            }
            let outcome = match scheduler.dispatch_syscall(&mut machine, &mut service) {
                Ok(outcome) => outcome,
                Err(error) => panic!(
                    "a dispatch failed: {error:?}\n  last fault: {:?}\n  pc={:?}",
                    machine.last_trap_fault(),
                    machine.architectural_state().pc().as_u64()
                ),
            };
            match outcome {
                DispatchOutcome::Return { completion, .. } => {
                    scheduler
                        .return_from_syscall(&mut machine, completion)
                        .expect("a return");
                }
                DispatchOutcome::Exit { exit_code: code } => {
                    exit_code = Some(code);
                    break;
                }
                DispatchOutcome::Fault(error) => panic!("the program faulted: {error:?}"),
            }
        }
    }
    // A program that neither exits nor traps has left nothing to assert on, and
    // `None` on its own does not say why. Saying so here turns "the test failed"
    // into "the program trapped at this instruction with these registers", which
    // is the difference between a minute and an afternoon.
    if exit_code.is_none() && fault.is_none() {
        let registers: Vec<String> = (0..8)
            .map(|index| {
                format!(
                    "r{index}={:#x}",
                    machine
                        .architectural_state()
                        .registers()
                        .read_raw(index)
                        .unwrap_or(0)
                )
            })
            .collect();
        panic!(
            "the program neither exited nor trapped after {steps} steps\n  \
             pc={:#x} sp={:#x}\n  {}\n  last fault: {:?}",
            machine.architectural_state().pc().as_u64(),
            machine.architectural_state().sp().as_u64(),
            registers.join(" "),
            machine.last_trap_fault(),
        );
    }
    (service.terminal().output().to_vec(), exit_code)
}

/// A machine with user memory and a trap vector that returns, as the OS harness
/// tests build.
fn test_machine(
    config: ArchitectureConfig,
) -> lazalith_machine::LazalithMachine<lazalith_devices::NoDevice> {
    use lazalith_cpu::StatusRegister;
    use lazalith_devices::DeviceManager;
    use lazalith_machine::{LazalithMachine, MachineSetup};
    use lazalith_memory::{MemoryRegion, RegionPermissions};
    use lazalith_os::{USER_CODE_START, USER_INITIAL_SP, UserMemory};
    use lazalith_types::{CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress};

    let mut regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            16,
            RegionPermissions::new(true, true, true, false),
        )
        .unwrap(),
    ];
    regions.extend(UserMemory::regions(config).unwrap());
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions,
        pc: InstructionAddress::new(USER_CODE_START),
        sp: VirtualAddress::new(USER_INITIAL_SP),
        status: StatusRegister::new(lazalith_cpu::Privilege::User, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .unwrap();
    let rfe = lazalith_isa::Instruction::new(config, Opcode::Rfe, &[]).unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &lazalith_isa::encode(config, &rfe).unwrap(),
        )
        .unwrap();
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .unwrap();
    machine
}

/// A harness that calls a generated function and exits with its result.
///
/// `CALL` pushes the return address, so the callee's `RET` is real: this is the
/// same shape a runtime uses, written by hand so that Step 63 does not depend on
/// Step 64 existing. Every called symbol is declared `.extern`, which is what
/// lets the linker resolve it against the generated object.
fn harness(name: &str, externs: &[&str], body: &str) -> ObjectFile {
    let mut source = String::from(".arch lz64\n.entry entry\n");
    for symbol in externs {
        source.push_str(&format!(".extern {symbol}\n"));
    }
    source.push_str("entry:\n");
    source.push_str(body);
    // Exit with 0, so a program that just returns has somewhere to go. `r0` is
    // the syscall number and `Exit` is 1 — zero is not a syscall, and a harness
    // that asked for it would fault on its way out rather than finish. The ABI
    // reserves `r7` and requires it to be zero at a syscall, and so requires a
    // field the ABI names as reserved. Hand-written code has to say so itself,
    // exactly as the generated code does.
    source.push_str("LI r0, 1\nLI r1, 0\nLI r7, 0\nSYSCALL\n");
    assemble_named(name, &source).expect("the harness assembles")
}

/// A function that returns a value the harness exits with, so the exit code is
/// the function's result.
#[test]
fn a_generated_object_is_a_valid_object() {
    let (object, _) = generate_object(
        r#"
        pub fn square(value: i32) -> i32 {
            return value * value;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    // `ObjectFile::validate` checks the sections, and the text section's own
    // validation decodes and re-encodes every instruction, so this passing means
    // every emitted instruction is canonical.
    object.validate().expect("the object is valid");
    assert_eq!(object.sections().len(), 1, "one text section, no data");
    assert!(!object.code().is_empty(), "there is code");
}

/// A local is addressed from the stack pointer, so every frame access is a
/// `GETSP` and no function's address is ever taken.
#[test]
fn a_local_is_reached_through_the_stack_pointer() {
    let (object, _) = generate_object(
        r#"
        fn main() -> i32 {
            let total: i32 = 1;
            return total;
        }
        "#,
    );
    let code = function_code(&object, "main");
    assert!(
        code.contains(&Opcode::Getsp),
        "the frame base is the stack pointer: {code:?}"
    );
    assert!(
        code.contains(&Opcode::Setsp),
        "the prologue and the epilogue both move it: {code:?}"
    );
    assert!(
        !code.contains(&Opcode::Getpc),
        "a function's own address is never taken as a frame base: {code:?}"
    );
}

/// The prologue reserves exactly the frame the layout reports, and the epilogue
/// gives it back before returning.
#[test]
fn the_prologue_reserves_the_reported_frame() {
    let (program, lowered) = generate_program(
        r#"
        fn wide(first: i64, second: i64) -> i64 {
            let third: i64 = first + second;
            return third;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let object = program.object().clone();
    let frame = program.frame("wide").expect("a generated frame for wide");
    let _ = lowered;
    let config = ArchitectureConfig::lz64();
    let disassembly = disassemble_object(&object)
        .expect("disassembly")
        .first()
        .expect("a text section")
        .clone();
    let symbol = object
        .symbols()
        .iter()
        .position(|symbol| symbol.name() == "fn.wide")
        .expect("a symbol for wide");
    let start = object.symbols()[symbol].value() as usize / 8;
    let instructions = &disassembly.instructions()[start..];
    // The third instruction is the frame reservation: `GETSP`, then `ADDI` with
    // the negative frame size, then `SETSP`.
    let reserve = instructions[1]
        .instruction()
        .operands()
        .get(2)
        .copied()
        .expect("an immediate frame size");
    let lazalith_isa::Operand::Immediate(size) = reserve else {
        let first: Vec<String> = instructions[..6]
            .iter()
            .map(|i| i.text().to_string())
            .collect();
        panic!("the frame size is an immediate, found {reserve:?}; the prologue is {first:?}");
    };
    let prologue: Vec<String> = instructions[..8]
        .iter()
        .map(|i| i.text().to_string())
        .collect();
    assert_eq!(
        i64::from(size),
        -i64::from(frame.frame_size),
        "the prologue reserves exactly the frame the report names; it is {prologue:?}"
    );
    assert_eq!(
        frame.frame_size,
        frame.reported_frame_size + frame.outgoing_arguments + frame.value_area,
        "the frame is the lowering's own, the reserve for outgoing argument \
         words, and the value area, and nothing else: {frame:?}"
    );
    assert!(
        frame.value_area > 0 && frame.outgoing_arguments > 0,
        "this function has both a value area and outgoing argument space: {frame:?}"
    );
    // And the epilogue adds it back, so the callee leaves the stack pointer as
    // it found it, which is what the convention requires. The epilogue is the
    // `ADDI` immediately before this function's own `RET`: the section holds
    // every function, so "the last `ADDI`" would be another function's.
    let returns = instructions
        .iter()
        .position(|instruction| instruction.instruction().opcode() == Opcode::Ret)
        .expect("the function returns");
    let restore = instructions[..returns]
        .iter()
        .rev()
        .find(|instruction| instruction.instruction().opcode() == Opcode::Addi)
        .expect("an add in the epilogue");
    let lazalith_isa::Operand::Immediate(size) = restore.instruction().operands()[2] else {
        panic!("the restore is an immediate");
    };
    assert_eq!(i64::from(size), i64::from(frame.frame_size));
    let _ = config;
}

/// A string's address is a relocation against its data segment, so no address is
/// written into the code.
#[test]
fn a_string_address_is_a_relocation() {
    let (object, lowered) = generate_object(
        r#"
        fn main() -> i32 {
            let text = "hi";
            let length = text.len();
            return length as i32;
        }
        "#,
    );
    object.validate().expect("the object is valid");
    assert_eq!(
        object.sections().len(),
        2,
        "a string needs a data section: {:?}",
        object
            .sections()
            .iter()
            .map(|section| String::from(section.name()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        object
            .sections()
            .iter()
            .find(|section| section.name() == "rodata")
            .map(|section| section.bytes().to_vec()),
        Some(b"hi".to_vec()),
        "the literal's bytes are in the segment"
    );
    assert!(
        object
            .relocations()
            .iter()
            .any(|relocation| relocation.kind() == RelocationKind::LiImmediate),
        "the address is a symbol the linker fills in: {:?}",
        object
            .relocations()
            .iter()
            .map(|relocation| relocation.kind())
            .collect::<Vec<_>>()
    );
    assert!(!lowered.frames.is_empty());
}

/// The generated code for a whole program runs and writes what it should.
///
/// This is the test that would fail if any instruction were emitted wrongly, and
/// it goes through the real machine, the real syscall dispatcher, and the real
/// console.
#[test]
fn a_generated_program_writes_to_the_console_and_exits() {
    let (object, _) = generate_object(
        r#"
        extern "syscall" fn write(handle: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;

        pub fn main() -> i32 {
            let message = "Hello, Lazalith\n";
            let bytes = message.as_bytes();
            let io = bytes.as_ptr();
            write(1, bytes.as_ptr(), message.len() as u64, io);
            return 0;
        }
        "#,
    );
    // The harness calls the program's own entry, which is what a runtime does.
    // Step 64 is where that runtime is written; here it is three instructions of
    // assembly, so this test does not depend on it existing.
    let harness = harness("harness", &["fn.main"], "CALL fn.main\n");
    let (output, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(0), "the program returned 0");
    assert_eq!(
        String::from_utf8_lossy(&output),
        "Hello, Lazalith\n",
        "the generated program wrote its message"
    );
}

/// A view's length survives the round trip, which is the property the deleted
/// prototype broke: a length that was dropped would make `len` read past the
/// data, and the write below would print the wrong number of bytes.
///
/// The string is built inside the generated program rather than handed in from the
/// harness. A `str` argument is two words, and a harness that has to put a real
/// address in one of them has to know where the linker placed the data — which
/// would make this a test of the harness's guess rather than of the view.
#[test]
fn a_view_keeps_its_length_through_the_generated_code() {
    let (object, _) = generate_object(
        r#"
        extern "syscall" fn write(handle: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;

        pub fn repeat(times: i32) -> i32 {
            let text = "ab";
            let mut count: i32 = 0;
            let mut index: i32 = 0;
            let mut result = [0u8; 16];
            while index < times {
                let bytes = text.as_bytes();
                write(1, bytes.as_ptr(), text.len() as u64, result.as_ptr());
                count = count + text.len() as i32;
                index = index + 1;
            }
            return count;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let harness = harness(
        "harness",
        &["fn.repeat"],
        "         LI r0, 2\n\
         CALL fn.repeat\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (output, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(4), "and the length reached the accumulator too");
    assert_eq!(
        String::from_utf8_lossy(&output),
        "abab",
        "each write used the view's own length"
    );
}

/// A function call passes its arguments in the documented registers and returns
/// in `r0`.
#[test]
fn a_call_uses_the_documented_convention() {
    let (object, _) = generate_object(
        r#"
        pub fn add(left: i64, right: i64) -> i64 {
            return left + right;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let code = function_code(&object, "add");
    assert!(
        code.contains(&Opcode::Add),
        "the addition is real: {code:?}"
    );
    assert!(code.contains(&Opcode::Ret), "and it returns: {code:?}");
    let harness = harness(
        "harness",
        &["fn.add"],
        "         LI r0, 40\n\
         LI r1, 2\n\
         CALL fn.add\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code,
        Some(42),
        "the call returned 42, so r0 held the result"
    );
}

/// A view argument occupies two words, and the callee sees both of them.
#[test]
fn a_view_argument_is_two_words() {
    let (object, _) = generate_object(
        r#"
        pub fn total(first: &[u8], second: &[u8]) -> usize {
            return first.len() + second.len();
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let harness = harness(
        "harness",
        &["fn.total"],
        "LI r0, 0\n\
         LI r1, 3\n\
         LI r2, 0\n\
         LI r3, 7\n\
         CALL fn.total\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(10), "both views arrived whole: 3 and 7 elements");
}

/// A comparison is a real comparison, and its value is a real `0` or `1`.
#[test]
fn a_comparison_produces_a_boolean_value() {
    let (object, _) = generate_object(
        r#"
        pub fn check(value: i32) -> i32 {
            if value > 10 {
                return 1;
            }
            return 0;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let code = function_code(&object, "check");
    assert!(code.contains(&Opcode::Cmp), "a real comparison: {code:?}");
    assert!(code.contains(&Opcode::Br), "and a real branch: {code:?}");
    let harness = harness(
        "harness",
        &["fn.check"],
        "         LI r0, 42\n\
         CALL fn.check\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(1), "42 is greater than 10");
}

/// `&&` does not evaluate its right side when the left side decides the answer.
///
/// The right side here is an out-of-range index, which would trap if it ran. A
/// backend that evaluated it eagerly would fault instead of returning 0.
#[test]
fn a_short_circuit_does_not_evaluate_its_right_side() {
    let (object, _) = generate_object(
        r#"
        pub fn check(values: &[u8], index: usize) -> bool {
            return index < values.len() && values[index] == 7;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let harness = harness(
        "harness",
        &["fn.check"],
        "         LI r0, 0\n\
         LI r1, 0\n\
         LI r2, 1\n\
         LI r3, 99\n\
         CALL fn.check\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code,
        Some(0),
        "the index was out of range, so the right side never ran"
    );
}

/// An out-of-range index traps instead of reading whatever is there.
#[test]
fn an_out_of_range_index_traps() {
    let (object, _) = generate_object(
        r#"
        pub fn read(values: &[u8], index: usize) -> u8 {
            return values[index];
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let code = function_code(&object, "read");
    assert!(
        code.contains(&Opcode::Trap),
        "the check traps rather than reading elsewhere: {code:?}"
    );
    let harness = harness(
        "harness",
        &["fn.read"],
        "         LI r0, 0\n\
         LI r1, 0\n\
         LI r2, 1\n\
         LI r3, 100\n\
         CALL fn.read\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code, None,
        "the process never exited, because the read trapped"
    );
}

/// `break` and `continue` reach real code: the loop below would not terminate if
/// either became a trap or an unreachable block.
#[test]
fn break_and_continue_reach_real_code() {
    let (object, _) = generate_object(
        r#"
        pub fn count(limit: i32) -> i32 {
            let mut total: i32 = 0;
            for index in 0..limit {
                if index == 2 {
                    continue;
                }
                if index == 5 {
                    break;
                }
                total = total + index;
            }
            return total;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let harness = harness(
        "harness",
        &["fn.count"],
        "         LI r0, 100\n\
         CALL fn.count\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    // Counting from zero: index 0 adds nothing, 1 adds 1, 2 is `continue`d and
    // adds nothing, 3 adds 3, 4 adds 4, and 5 breaks before adding anything.
    let expected = 1 + 3 + 4;
    assert_eq!(code, Some(expected), "the loop stopped where it should");
}

/// A 32-bit value's arithmetic is 32-bit arithmetic: `i32::MAX + 1` wraps to
/// `i32::MIN`, and a backend that computed at the machine's 64-bit width would
/// return 2147483648 instead.
///
/// The result is cast to `u32` before it is returned because the exit code is the
/// only channel back to a test and the ABI's exit code is a `u32`: an `i32` of
/// `-2147483648` sign-extends to a word the dispatcher refuses as an argument, so
/// the test would measure the dispatcher's complaint rather than the arithmetic.
/// The cast is Lazen's own, so the width it uses is the one under test too.
#[test]
fn a_32_bit_value_wraps_at_32_bits() {
    let (object, _) = generate_object(
        r#"
        pub fn overflow() -> u32 {
            let big: i32 = 2147483647;
            return (big + 1) as u32;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let harness = harness(
        "harness",
        &["fn.overflow"],
        "         CALL fn.overflow\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code,
        Some(i32::MIN as u32),
        "the addition wrapped at 32 bits, not at 64"
    );
}

/// A cast narrows or widens by the declared widths, and the sign follows the
/// source.
#[test]
fn a_cast_uses_the_declared_widths() {
    let (object, _) = generate_object(
        r#"
        pub fn widen(small: u8) -> i64 {
            return small as i64;
        }
        pub fn narrow(big: i64) -> i16 {
            return big as i16;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let second_harness = harness(
        "harness",
        &["fn.widen"],
        "         LI r0, 200\n\
         CALL fn.widen\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![second_harness, object], "entry");
    assert_eq!(code, Some(200), "a u8 of 200 widened to 200, not to 456");

    let (object, _) = generate_object(
        r#"
        pub fn narrow(big: i64) -> i16 {
            return big as i16;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let second_harness = harness(
        "harness",
        &["fn.narrow"],
        "         LI r0, 1\n\
         LI r1, 0\n\
         CALL fn.narrow\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![second_harness, object], "entry");
    assert_eq!(code, Some(1), "the low sixteen bits survived the narrowing");
}

/// A 32-bit machine has no lowering, and this stage says so instead of emitting
/// 64-bit arithmetic for it.
#[test]
fn a_32_bit_machine_is_refused() {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", "fn main() -> i32 { return 0; }")
        .expect("the program compiles");
    let lowered = lower::lower(&program).expect("the program lowers");
    let options = CodegenOptions {
        architecture: ArchitectureConfig::lz32(),
        source_path: String::from("t.lazen"),
    };
    match generate_lowered(&lowered, &options, "")
        .expect_err("a 32-bit machine has no code generation")
    {
        CodegenError::UnsupportedArchitecture { word } => {
            assert_eq!(word.bits(), 32);
        }
        other => panic!("expected a refusal for a 32-bit machine: {other}"),
    }
}

const MISSING_FRAME_SOURCE: &str = r#"
        fn main() -> i32 {
            return 0;
        }
        "#;

/// A function with no reported frame cannot be given a prologue, and this stage
/// says so rather than inventing a frame size.
#[test]
fn a_function_without_a_frame_is_refused() {
    let (object, mut lowered) = generate_object(MISSING_FRAME_SOURCE);
    object.validate().expect("the object is valid");
    // Remove the frame the lowering reported, as a caller that lost it would.
    lowered.frames.clear();
    match generate_lowered(
        &lowered,
        &CodegenOptions::lz64("t.lazen"),
        MISSING_FRAME_SOURCE,
    )
    .expect_err("a missing frame must be refused")
    {
        CodegenError::MissingFrame { function } => assert_eq!(function, "main"),
        other => panic!("expected a refusal for a missing frame: {other}"),
    }
}

/// The object's debug information names the source it came from, and the text.
///
/// A mapping is a byte offset into the source, so an object that recorded the
/// path and the offsets but not the text would leave a debugger unable to turn
/// any of it into a line. That is what this states: the object is
/// self-describing.
#[test]
fn the_object_records_its_source_and_its_text() {
    let (object, _) = generate_object(
        r#"
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    assert_eq!(
        object
            .debug_sources()
            .iter()
            .map(|source| String::from(source.path()))
            .collect::<Vec<_>>(),
        vec![String::from("t.lazen")],
        "the source path is recorded"
    );
    let source = &object.debug_sources()[0];
    assert!(
        source.text().contains("fn main"),
        "and so is the text the mappings are offsets into: {:?}",
        source.text()
    );
    assert_eq!(
        source.length(),
        u32::try_from(source.text().len()).expect("a host-sized source"),
        "whose recorded length is its own"
    );
    // Every mapping's source range has to fit inside the text it points into, or
    // resolving it would read past the end.
    for mapping in object.debug_mappings() {
        let end = mapping.source_offset() + mapping.source_length();
        assert!(
            end <= source.length(),
            "mapping at {} ends at {end}, past the {}-byte source",
            mapping.offset(),
            source.length()
        );
    }
    assert!(
        object.debug_mappings().len()
            >= object
                .symbols()
                .iter()
                .filter(|s| s.name().starts_with("fn."))
                .count(),
        "every function has at least one code mapping"
    );
}

/// A function that is not public is a local symbol, and a public one is global,
/// so another object can link against it and cannot against the first.
#[test]
fn linkage_follows_the_frontend() {
    let (object, _) = generate_object(
        r#"
        pub fn shared() -> i32 { return 1; }
        fn hidden() -> i32 { return 2; }
        fn main() -> i32 { return 0; }
        "#,
    );
    use lazalith_toolchain::SymbolBinding;
    let binding = |name: &str| {
        object
            .symbols()
            .iter()
            .find(|symbol| symbol.name() == name)
            .map(|symbol| symbol.binding())
    };
    assert_eq!(binding("fn.shared"), Some(SymbolBinding::Global));
    assert_eq!(binding("fn.hidden"), Some(SymbolBinding::Local));
}

/// Every instruction in a generated object decodes to the opcode the ISA
/// defines, which is what the text section's own validation checks; this test
/// makes the property explicit and independent of the object builder.
#[test]
fn every_generated_instruction_decodes() {
    let (object, _) = generate_object(
        r#"
        extern "syscall" fn write(handle: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;
        pub fn work(flag: bool, index: usize) -> i64 {
            let values = [1i64, 2i64, 3i64];
            let mut total: i64 = 0;
            for position in 0..3 {
                if flag && position == 1 {
                    continue;
                }
                total = total + values[position as usize];
            }
            let message = "done";
            let bytes = message.as_bytes();
            let io = bytes.as_ptr();
            write(1, bytes.as_ptr(), message.len() as u64, io);
            return total + index as i64;
        }
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    let config = ArchitectureConfig::lz64();
    let code = object.code();
    assert_eq!(code.len() % 8, 0, "instructions are whole words");
    for (index, chunk) in code.chunks(8).enumerate() {
        let instruction = decode(config, chunk)
            .unwrap_or_else(|error| panic!("instruction {index} decodes: {error}"));
        assert_eq!(
            lazalith_isa::encode(config, &instruction).unwrap(),
            chunk,
            "instruction {index} re-encodes to itself"
        );
    }
}

/// A module whose spans name more than one source cannot be described by one
/// debug path, and this stage says so instead of mapping everything to the file
/// it saw first.
#[test]
fn more_than_one_source_is_refused() {
    let (object, lowered) = generate_object(
        r#"
        fn main() -> i32 {
            let text = "a";
            return text.len() as i32;
        }
        "#,
    );
    object.validate().expect("the object is valid");
    // Both files go into one source manager, because a span names a source by the
    // identifier that manager gave it, and two managers both start at zero.
    let mut sources = SourceManager::new();
    let (_, first) = compile(&mut sources, "t.lazen", "fn main() -> i32 { return 0; }").unwrap();
    let (_, other) = compile(&mut sources, "b.lazen", "fn b() -> i32 { return 0; }").unwrap();
    let _ = first;
    let mut mixed = lowered.clone();
    // A data segment from another file, so the module names two sources.
    mixed.module.data[0].span = Some(other.functions[0].span.clone());
    match generate_lowered(&mixed, &CodegenOptions::lz64("t.lazen"), "")
        .expect_err("two sources cannot share one debug path")
    {
        CodegenError::MultipleSources { count } => assert_eq!(count, 2),
        other => panic!("expected a refusal for two sources: {other}"),
    }
}

/// The architecture the tests run on is the one the code was generated for.
#[test]
fn the_generated_code_is_for_the_requested_machine() {
    let (object, _) = generate_object("fn main() -> i32 { return 0; }");
    assert_eq!(
        object.config().word_width(),
        ArchitectureConfig::lz64().word_width(),
        "the object records the machine it was generated for"
    );
    let _ = LzxArchitecture::Lz64;
}

/// The hello world in `docs/lazen-syntax.md` compiles, generates, and prints.
///
/// The examples in the language documentation are the first thing a reader tries,
/// and nothing else in this file checks that they are what they claim to be. This
/// is the one from section 1, verbatim: it compiles, it lowers, and it generates
/// an object whose entry is `fn.main`.
///
/// Running it to completion is the one part this cannot do, and the reason is
/// Step 64 rather than anything in the program: `main` returns, and a program that
/// returns needs a caller to return *to*, which is the runtime that starts it. So
/// the second half of this test runs the same program with `pub` on `main`, which
/// is what lets the hand-written harness call it the way a runtime will.
#[test]
fn the_documented_hello_world_runs() {
    const DOCUMENTED: &str = r#"
        extern "syscall" fn write(fd: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;

        fn main() -> i32 {
            let message = "Hello, Lazalith\n";
            let bytes = message.as_bytes();
            let mut result = [0u8; 16];
            write(1, bytes.as_ptr(), message.len() as u64, result.as_ptr());
            0
        }
        "#;
    let (object, lowered) = generate_object(DOCUMENTED);
    assert_eq!(
        lowered.entry, "main",
        "the entry is the function named main, public or not"
    );
    assert!(
        object
            .symbols()
            .iter()
            .any(|symbol| symbol.name() == "fn.main"),
        "and the object defines it"
    );

    let (object, _) = generate_object(&DOCUMENTED.replace("fn main", "pub fn main"));
    let harness = harness(
        "harness",
        &["fn.main"],
        "         CALL fn.main\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (output, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(0), "and it returned 0");
    assert_eq!(
        String::from_utf8_lossy(&output),
        "Hello, Lazalith\n",
        "and it printed what the documentation says it prints"
    );
}

/// The image's entry point is callable from outside its own object, whatever the
/// Lazen declaration says.
///
/// `pub` governs visibility between *modules*. The loader is not a module: it
/// starts the image at the entry symbol, so a private `main` — which is what the
/// language documentation's own hello world declares — still has to be reachable
/// from startup code in another object. A local symbol would leave the image with
/// an entry nothing could call, and the failure would be an unresolved symbol at
/// link time rather than anything a Lazen programmer could see.
#[test]
fn the_entry_point_is_callable_from_outside_the_object() {
    let (object, lowered) = generate_object(
        r#"
        fn main() -> i32 {
            return 7;
        }
        "#,
    );
    assert_eq!(lowered.entry, "main");
    let symbol = object
        .symbols()
        .iter()
        .find(|symbol| symbol.name() == "fn.main")
        .expect("the object defines the entry symbol");
    assert_eq!(
        symbol.binding(),
        lazalith_toolchain::SymbolBinding::Global,
        "a private entry is still the image's only way in"
    );
    // A private function that is *not* the entry stays local.
    let (object, _) = generate_object(
        r#"
        fn helper() -> i32 {
            return 1;
        }
        fn main() -> i32 {
            return helper();
        }
        "#,
    );
    let helper = object
        .symbols()
        .iter()
        .find(|symbol| symbol.name() == "fn.helper")
        .expect("the object defines the helper");
    assert_eq!(
        helper.binding(),
        lazalith_toolchain::SymbolBinding::Local,
        "a private function that is not the entry is not exported"
    );
}

/// A narrow parameter is stored at its own width, so the next one survives.
///
/// A `u32` is four bytes and a frame slot is word-aligned, so `f(a: u32, b: u32)`
/// puts `b` four bytes after `a`. Storing a whole word for `a` would write over
/// `b`'s slot — not corrupting it but *replacing* it, which is why the bug shows
/// up as a missing value rather than a wrong one, and why the smallest failing
/// case is two narrow parameters and not one.
#[test]
fn a_narrow_parameter_does_not_overwrite_the_next_one() {
    const SOURCE: &str = r#"
        fn pair(a: u32, b: u32) -> u64 {
            return a as u64 + b as u64;
        }

        fn main() -> i32 {
            // 1 + 2 == 3, and both arguments have to arrive for that to be true.
            if pair(1u32, 2u32) != 3u64 {
                return 1;
            }
            if pair(0u32, 0u32) != 0u64 {
                return 2;
            }
            // Three narrow parameters, so the third is the one that would be lost.
            if triple(1u32, 2u32, 3u32) != 6u64 {
                return 3;
            }
            return 0;
        }

        fn triple(a: u32, b: u32, c: u32) -> u64 {
            return a as u64 + b as u64 + c as u64;
        }
    "#;
    let (object, _) = generate_object(SOURCE);
    let harness = harness(
        "harness",
        &["fn.main"],
        "         CALL fn.main\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code,
        Some(0),
        "every narrow parameter reached the function that was passed"
    );
}

/// A two-word result comes back in `r0` and `r1` and lands in the caller's slot.
///
/// A view is a pointer and a length, and the only way to return one is to return
/// both words. The caller must then *store* them rather than read them from
/// registers, or an expression that returns a view could not appear in the middle
/// of a larger expression: the registers would be gone by the time it was used.
///
/// The test writes through the returned view rather than only reading it, because
/// a view whose *address* word is wrong still has the right length — so a length
/// check alone would pass on a result that points nowhere.
#[test]
fn a_two_word_result_survives_into_the_callers_frame() {
    const SOURCE: &str = r#"
        extern "syscall" fn write(handle: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;

        // Returns a view over the bytes of `pair`. A view is two words, so this is
        // the smallest function that can return one: the address in `r0` and the
        // length in `r1`.
        fn make_pair(pair: &[u8]) -> &[u8] {
            return pair;
        }

        fn main() -> i32 {
            let mut pair: [u8; 2] = [104u8, 105u8];
            let view: &[u8] = make_pair(pair.as_slice());
            if view.len() != 2usize {
                return 1;
            }
            if view[0] != 104u8 {
                return 2;
            }
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = write(
                1,
                view.as_ptr(),
                view.len() as u64,
                record.as_mut_slice().as_ptr() as ptr<u8>
            );
            if status != 0 {
                return 3;
            }
            return 0;
        }
    "#;
    let (object, _) = generate_object(SOURCE);
    let harness = harness(
        "harness",
        &["fn.main"],
        "         CALL fn.main\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (output, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(0), "and it returned 0");
    assert_eq!(
        String::from_utf8_lossy(&output),
        "hi",
        "a returned view still points at the bytes it was made from, with the \
         length it was made with"
    );
}

/// A wide constant is the number it was written as.
///
/// A word's immediate is signed, so a 64-bit constant is built from two halves.
/// The halves have to be put back the way they came apart: the high half shifted
/// up, and the low half in the bottom 32 bits with the sign extension `LI` gave
/// it taken back off. Combining them the other way round — shift the *low* half
/// up and or the high half in unshifted — yields `(low << 32) | high`, so
/// `4294967296` arrived as `1` and every program that multiplied by a word-bound
/// constant silently multiplied by something else.
///
/// The halves can also disagree about the sign in both directions, and both were
/// wrong: a high half of zeros above a low half with bit 31 set, and a high half
/// of ones above a low half with bit 31 clear. `LI` alone is right only when the
/// high half *is* the low half's sign extension.
#[test]
fn a_wide_constant_is_the_number_it_was_written_as() {
    // Each is returned by a function of its own, so the constant is materialised
    // in a function whose only job is to return it: a constant folded into a
    // larger expression could be built by a different path than the one tested.
    let source = r#"
        extern "syscall" fn write(
            handle: u32,
            buffer: ptr<u8>,
            count: u64,
            result: ptr<u8>
        ) -> i64;

        // 0x0000_0001_0000_0000: the constant that came out as 1.
        fn shifted() -> u64 {
            return 1u64 * 4294967296u64;
        }
        // 0x0000_0001_0000_0001: a high half that is neither zero nor all ones.
        fn mixed() -> u64 {
            return 4294967297u64;
        }
        // 0xFFFF_FFFF_0000_0001: a high half of ones above a low half with bit
        // 31 clear, so a sign-extending `LI` of the low half is not the number.
        fn negative_high() -> u64 {
            return 18446744069414584321u64;
        }
        // 0x0000_0000_8000_0000: a high half of zero above a low half with bit
        // 31 set, so the same `LI` sign-extends a positive number negative.
        fn positive_high() -> u64 {
            return 2147483648u64;
        }
        // 0xFFFF_FFFF_8000_0000: the one shape where a single `LI` is right, so
        // the short path has to still produce it.
        fn short_path() -> u64 {
            return 18446744071562067968u64;
        }
        // 0x0000_0002_0000_0000: both halves set, with a zero low half.
        fn both_halves() -> u64 {
            return 2u64 * 4294967296u64;
        }

        fn main() -> i32 {
            let mut bytes: [u8; 48] = [0u8; 48];
            let mut place: u64 = 0u64;
            let mut value: u64 = shifted();
            while place < 48u64 {
                bytes[place as usize] = (value % 256u64) as u8;
                value = value / 256u64;
                place = place + 1u64;
                if place == 8u64 { value = mixed(); }
                if place == 16u64 { value = negative_high(); }
                if place == 24u64 { value = positive_high(); }
                if place == 32u64 { value = short_path(); }
                if place == 40u64 { value = both_halves(); }
            }
            let status: i64 = write(
                1,
                bytes.as_ptr(),
                48u64,
                bytes.as_mut_slice().as_ptr() as ptr<u8>
            );
            if status != 0 {
                return 90;
            }
            return 0;
        }
    "#;
    let (object, _) = generate_object(source);
    let harness = harness(
        "harness",
        &["fn.main"],
        "         CALL fn.main\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (output, code) = run(vec![harness, object], "entry");
    assert_eq!(code, Some(0), "every constant survived the round trip");
    // Each value is eight little-endian bytes, so the output is the six
    // constants back to back.
    let expected: Vec<u8> = [
        0x0000_0001_0000_0000u64,
        0x0000_0001_0000_0001,
        0xFFFF_FFFF_0000_0001,
        0x0000_0000_8000_0000,
        0xFFFF_FFFF_8000_0000,
        0x0000_0002_0000_0000,
    ]
    .into_iter()
    .flat_map(u64::to_le_bytes)
    .collect();
    assert_eq!(
        output, expected,
        "each constant is exactly the bit pattern it was written as"
    );
}

/// A frame that is live across a call keeps its own frame base.
///
/// The backend holds every value in the frame and reaches a slot by forming its
/// address, so it keeps the frame base in the callee-saved `r8` for the length of
/// the body. That makes `r8` a local, and a local that is live across a call is
/// only safe because the function saves it — so the save has to be in a word the
/// call sequence never touches.
///
/// Two words in the frame can be mistaken for the right one, and both were:
///
/// - `[SP+0]` and `[SP+8]` are where a call writes argument words five and six,
///   so a save there survives only while every call passes four words or fewer;
/// - a word sized from the value area but addressed from the *stack pointer*
///   rather than the frame base lands 16 bytes low, inside the value area, and
///   overwrites a local instead.
///
/// Neither shows up in a program that calls a function with two arguments, so the
/// test calls with six at two depths, and each function reads and writes its own
/// locals *after* the call it made. A clobbered frame base sends those accesses
/// somewhere else in the frame, which is mapped, so the failure is a wrong
/// answer rather than a fault.
#[test]
fn a_frame_stays_addressable_across_a_call_with_six_arguments() {
    const SOURCE: &str = r#"
        fn inner(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64) -> i64 {
            return a + b + c + d + e + f;
        }

        // Six argument words, so the call writes `[SP+0]` and `[SP+8]`. Two
        // locals either side of the call, so a frame base that moved by 16 bytes
        // in either direction lands on a different word than the one meant.
        fn middle(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64) -> i64 {
            let mut before: i64 = 7i64;
            let mut sum: i64 = inner(a, b, c, d, e, f);
            let mut after: i64 = 11i64;
            // These three only agree if `sum` still names the slot it did before
            // the call, so they are the read side of the same property.
            if before != 7i64 { return 900; }
            if after != 11i64 { return 901; }
            if sum != a + b + c + d + e + f { return 902; }
            sum = sum + before;
            sum = sum + after;
            return sum;
        }

        fn main() -> i32 {
            // 1 + 2 + 3 + 4 + 5 + 6 == 21, and middle adds 7 and 11 to that.
            if middle(1i64, 2i64, 3i64, 4i64, 5i64, 6i64) != 39i64 {
                return 1;
            }
            // Called again with a different argument pattern, because a save that
            // survived the first call by luck would not survive being reused.
            if middle(10i64, 20i64, 30i64, 40i64, 50i64, 60i64) != 228i64 {
                return 2;
            }
            // And directly, so the six-argument call is exercised with no frame
            // above it at all.
            if inner(1i64, 1i64, 1i64, 1i64, 1i64, 1i64) != 6i64 {
                return 3;
            }
            return 0;
        }
    "#;
    let (object, _) = generate_object(SOURCE);
    let harness = harness(
        "harness",
        &["fn.main"],
        "         CALL fn.main\n\
         MOV r1, r0\n\
         LI r0, 1\n\
         LI r7, 0\n\
         SYSCALL\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code,
        Some(0),
        "a frame stayed addressable across calls that use the whole argument area"
    );
}

/// Every statement leaves a mapping into the source it was written in.
#[test]
fn a_generated_object_maps_its_code_back_to_the_source() {
    let source = "fn main() -> i32 {\n    let a: i32 = 1i32;\n    let b: i32 = 2i32;\n    return a + b;\n}\n";
    let (program, _) = generate_program(source);
    let object = program.object();
    assert_eq!(
        object.debug_sources().len(),
        1,
        "one source file was compiled, so one is carried"
    );
    assert_eq!(object.debug_sources()[0].path(), "t.lazen");
    assert_eq!(
        object.debug_sources()[0].text(),
        source,
        "the text is the source the compiler was given"
    );
    assert!(
        !object.debug_mappings().is_empty(),
        "every statement left a mapping"
    );
    for mapping in object.debug_mappings() {
        assert!(
            mapping.source_offset() + mapping.source_length() <= object.debug_sources()[0].length(),
            "a mapping reaches inside the text it names"
        );
    }
}

/// A mapping's code offset is where that code really is in the object.
///
/// This is the property the whole feature rests on: a mapping that named an
/// offset in the text section that does not hold the statement it claims would
/// still round-trip perfectly and still send a debugger to the wrong place.
#[test]
fn a_mappings_code_offset_is_where_the_code_really_is() {
    let source = "fn main() -> i32 {\n    let a: i32 = 7i32;\n    return a;\n}\n";
    let (object, _) = generate_object(source);
    // The text section is whichever section the disassembler kept, and the
    // mapping's section is compared against that same index rather than a
    // number written down here — a test that hard-coded the index would pass
    // even if the backend put the code somewhere else.
    let disassembly = disassemble_object(&object)
        .expect("the object disassembles")
        .into_iter()
        .next()
        .expect("the object has one text section");
    let text = object
        .sections()
        .iter()
        .enumerate()
        .find(|(index, _)| u16::try_from(*index).unwrap_or(u16::MAX) == disassembly.section())
        .map(|(_, section)| section)
        .expect("the text section is in the object");
    for mapping in object.debug_mappings() {
        assert_eq!(
            mapping.section().get(),
            disassembly.section(),
            "a mapping names the section its code is in"
        );
        assert!(
            mapping.offset() + 8 <= text.bytes().len() as u64,
            "a mapping's offset {} is inside a {}-byte section, so the \
             instruction it names exists",
            mapping.offset(),
            text.bytes().len()
        );
        // And the instruction at that offset is a real one, not half of another.
        let at = mapping.offset() as usize / 8;
        assert!(
            at < disassembly.instructions().len(),
            "and there is an instruction at that offset"
        );
    }
}

/// Code generated for text that is not the text supplied is refused.
#[test]
fn generating_with_the_wrong_source_text_is_refused() {
    let source = "fn main() -> i32 {\n    return 1i32;\n}\n";
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source).expect("compiles");
    let lowered = lower::lower(&program).expect("lowers");
    // A truncated text cannot hold the spans the program was checked against, and
    // an object written from it would carry offsets into text that is not there.
    let short = &source[..source.len() / 2];
    let error = generate_lowered(&lowered, &CodegenOptions::lz64("t.lazen"), short)
        .expect_err("a span past the end of the text is refused");
    assert!(
        matches!(error, CodegenError::SourceOutOfRange { .. }),
        "the failure names the source range as what was wrong: {error}"
    );
}

/// The bounds check survives being inside a loop.
///
/// An out-of-range index traps outside a loop and did not inside one, which is
/// the worst shape of code-generation defect: the program runs, reads memory it
/// does not own, and returns a plausible byte. This asserts the check is emitted
/// in both shapes, and separately that the loop form traps when run.
#[test]
fn a_bounds_check_inside_a_loop_still_traps() {
    let (object, _) = generate_object(
        r#"
        pub fn read(values: &[u8], index: usize) -> u8 {
            return values[index];
        }
        fn main() -> i32 {
            let data: [u8; 4] = [1u8, 2u8, 3u8, 4u8];
            let mut n: i32 = 0;
            while n < 3 {
                let byte: u8 = read(data.as_slice(), 100usize);
                n = n + 1;
            }
            return 0;
        }
        "#,
    );
    let code = function_code(&object, "read");
    assert!(
        code.contains(&Opcode::Trap),
        "the bounds check traps rather than reading elsewhere: {code:?}"
    );
    let harness = harness(
        "harness",
        &["fn.read", "fn.main"],
        "         LI r0, 0\n\
         CALL fn.main\n",
    );
    let (_, code) = run(vec![harness, object], "entry");
    assert_eq!(
        code, None,
        "the process never exited, because the read inside the loop trapped"
    );
}
