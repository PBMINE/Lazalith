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

/// Compiles, lowers, and generates code for a program.
fn generate_program(source: &str) -> (Program, Lowered) {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    let lowered =
        lower::lower(&program).unwrap_or_else(|error| panic!("{source} should lower: {error}"));
    let generated = generate(&lowered, &CodegenOptions::lz64("t.lazen"))
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
    match generate(&lowered, &options).expect_err("a 32-bit machine has no code generation") {
        CodegenError::UnsupportedArchitecture { word } => {
            assert_eq!(word.bits(), 32);
        }
        other => panic!("expected a refusal for a 32-bit machine: {other}"),
    }
}

/// A function with no reported frame cannot be given a prologue, and this stage
/// says so rather than inventing a frame size.
#[test]
fn a_function_without_a_frame_is_refused() {
    let (object, mut lowered) = generate_object(
        r#"
        fn main() -> i32 {
            return 0;
        }
        "#,
    );
    object.validate().expect("the object is valid");
    // Remove the frame the lowering reported, as a caller that lost it would.
    lowered.frames.clear();
    match generate(&lowered, &CodegenOptions::lz64("t.lazen"))
        .expect_err("a missing frame must be refused")
    {
        CodegenError::MissingFrame { function } => assert_eq!(function, "main"),
        other => panic!("expected a refusal for a missing frame: {other}"),
    }
}

/// The object's debug information names the source it came from.
#[test]
fn the_object_records_its_source() {
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
    assert_eq!(
        object.debug_mappings().len(),
        object
            .symbols()
            .iter()
            .filter(|s| s.name().starts_with("fn."))
            .count(),
        "each function has a code mapping"
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
    match generate(&mixed, &CodegenOptions::lz64("t.lazen"))
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
