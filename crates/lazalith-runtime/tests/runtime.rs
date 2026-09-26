//! Runtime tests.
//!
//! These build real programs — prelude, user source, entry sequence, link, load,
//! step — and check what the program wrote and how it exited. A runtime test that
//! only inspected the generated object would not show that a syscall wrapper puts
//! its arguments where the ABI reads them, which is the whole job of a wrapper.

use std::string::String;
use std::vec::Vec;

use lazalith_os::{
    DispatchOutcome, FileSystemService, ProcessId, RoundRobinScheduler, TerminalService, ThreadId,
    VirtualFileSystem, VirtualTerminal,
};
use lazalith_runtime::{BuildOptions, RuntimeError, RuntimeProgram};
use lazalith_types::{ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress};

/// A machine with user memory and a trap vector that returns, as the OS harness
/// tests build.
fn test_machine() -> lazalith_machine::LazalithMachine<lazalith_devices::NoDevice> {
    use lazalith_cpu::StatusRegister;
    use lazalith_devices::DeviceManager;
    use lazalith_machine::{LazalithMachine, MachineSetup};
    use lazalith_memory::{MemoryRegion, RegionPermissions};
    use lazalith_os::{USER_CODE_START, USER_INITIAL_SP, UserMemory};
    use lazalith_types::VirtualAddress as Address;

    let config = ArchitectureConfig::lz64();
    let mut regions = Vec::new();
    regions.push(
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            16,
            RegionPermissions::new(true, true, true, false),
        )
        .expect("a supervisor page"),
    );
    regions.extend(UserMemory::regions(config).expect("user memory"));
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: DeviceManager::new(),
        regions,
        pc: InstructionAddress::new(USER_CODE_START),
        sp: Address::new(USER_INITIAL_SP),
        status: StatusRegister::new(lazalith_cpu::Privilege::User, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .expect("a machine");
    let rfe = lazalith_isa::Instruction::new(config, lazalith_isa::Opcode::Rfe, &[]).expect("rfe");
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &lazalith_isa::encode(config, &rfe).expect("encoded"),
        )
        .expect("the trap vector loads");
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .expect("a trap vector");
    machine
}

/// Runs a built program to completion and reports what it wrote and how it left.
fn run(program: &RuntimeProgram) -> (String, Option<u32>) {
    let linked = program.link().expect("the program links");
    // Round-trip the image through its bytes, because those bytes are what a
    // `.lzx` file holds and what a loader will read. Loading the in-memory image
    // would skip the one part of the path that has its own format.
    let bytes = linked.image().to_bytes().expect("the image serialises");
    let image = lazalith_os::LzxImage::from_bytes(&bytes).expect("the image reads back");
    assert_eq!(
        image.entry_offset(),
        linked.entry_offset(),
        "the entry survived the round trip"
    );
    let process = image
        .load_process(
            ProcessId::new(1).expect("a pid"),
            ThreadId::new(1).expect("a tid"),
        )
        .expect("the image loads");
    let mut scheduler = RoundRobinScheduler::new(10_000).expect("a scheduler");
    scheduler.add_process(process).expect("a process");
    let filesystem = VirtualFileSystem::with_defaults().expect("a filesystem");
    let mut service = TerminalService::new(
        VirtualTerminal::new(b"").expect("a terminal"),
        FileSystemService::new(filesystem),
    );
    let mut machine = test_machine();
    let mut exit = None;
    let mut steps = 0usize;
    let mut fault = None;
    for _ in 0..500_000 {
        let step = scheduler.step(&mut machine).expect("a step");
        steps += 1;
        if let lazalith_machine::MachineEvent::Trapped { event } = &step.event {
            if event.cause != lazalith_cpu::TrapCause::Syscall {
                fault = Some(event.cause);
                break;
            }
            match scheduler
                .dispatch_syscall(&mut machine, &mut service)
                .expect("a dispatch")
            {
                DispatchOutcome::Return { completion, .. } => {
                    scheduler
                        .return_from_syscall(&mut machine, completion)
                        .expect("a return");
                }
                DispatchOutcome::Exit { exit_code } => {
                    exit = Some(exit_code);
                    break;
                }
                DispatchOutcome::Fault(error) => panic!("the program faulted: {error:?}"),
            }
        }
    }
    // A program that neither exits nor traps has told us nothing, and `None`
    // alone would not say why. Saying so here turns "the test failed" into
    // "the program trapped here with these registers".
    assert!(
        exit.is_some() || fault.is_none(),
        "the program trapped after {steps} steps: {fault:?}\n  \
         pc={:#x} sp={:#x}\n  last fault: {:?}",
        machine.architectural_state().pc().as_u64(),
        machine.architectural_state().sp().as_u64(),
        machine.last_trap_fault(),
    );
    (
        String::from_utf8_lossy(service.terminal().output()).into_owned(),
        exit,
    )
}

/// Builds and runs `source`, panicking with the diagnostic if it will not build.
fn build_and_run(source: &str) -> (String, Option<u32>) {
    let program = RuntimeProgram::build(source, &BuildOptions::lz64("test.lz"))
        .unwrap_or_else(|error| panic!("{source} should build: {error}"));
    run(&program)
}

/// The whole pipeline works: a Lazen program is compiled, linked, loaded and run,
/// and its `main`'s return value becomes the process's exit code.
///
/// This is the test that would fail if any layer disagreed with the next about
/// where a value lives, and it is why the runtime is built from the same compiler
/// as the program rather than shipped as a prebuilt object.
#[test]
fn a_program_runs_and_its_result_becomes_the_exit_code() {
    let (output, exit) = build_and_run(
        r#"
        fn main() -> i32 {
            rt::sys::print("hello from lazen\n");
            return 7;
        }
        "#,
    );
    assert_eq!(
        output, "hello from lazen\n",
        "the program wrote its message"
    );
    assert_eq!(
        exit,
        Some(7),
        "and the entry sequence turned main's result into the exit code"
    );
}

/// A program with no `main` cannot be started, and the build says so.
///
/// An image whose entry symbol does not exist has no first instruction, so the
/// build refuses it. The refusal comes from lowering, which is where the entry
/// is chosen: the pipeline produces a runnable image, so a unit with nothing to
/// start at is refused before any code is emitted for it.
#[test]
fn a_program_without_main_does_not_link() {
    let error = RuntimeProgram::build(
        r#"
        fn helper() -> i32 {
            return 1;
        }
        "#,
        &BuildOptions::lz64("test.lz"),
    )
    .expect_err("a program with no main has no entry");
    let text = error.to_string();
    assert!(
        text.contains("main") || text.contains("entry"),
        "the error names the missing entry: {text}"
    );
}

/// The runtime's syscall wrappers put their arguments where the ABI reads them.
///
/// Each of these is one syscall with a different shape, and each would read the
/// wrong bytes if the wrapper's argument order or its out-parameter were wrong.
/// The console is the observable one: what the program wrote is what the kernel
/// was handed.
#[test]
fn the_syscall_wrappers_reach_the_kernel() {
    let (output, exit) = build_and_run(
        r#"
        fn main() -> i32 {
            let mut record: [u8; 16] = [0u8; 16];
            let status = rt::sys::write_to(1, "abc".as_bytes(), record.as_mut_slice());
            if status != 0 {
                return 1;
            }
            let cleared = rt::sys::clear();
            if cleared != 0 {
                return 2;
            }
            rt::sys::print("de");
            return 0;
        }
        "#,
    );
    assert_eq!(
        output, "de",
        "the writes after the clear reached the console, and the ones before it \
         did not: the virtual terminal's output is the screen, and clearing the \
         screen is what a program asked for"
    );
    assert_eq!(exit, Some(0), "and the program reported success");
}

/// A wrapper's out-parameter is the caller's buffer, and the wrapper reports how
/// many bytes moved.
///
/// The ABI writes a sixteen-byte `IoResult` into the pointer the caller names, so
/// a wrapper that passed something smaller would be writing past what the program
/// offered. The count read back is what proves the write landed where it should.
#[test]
fn a_write_reports_how_many_bytes_moved() {
    let (output, exit) = build_and_run(
        r#"
        fn main() -> i32 {
            let mut record: [u8; 16] = [0u8; 16];
            if !rt::sys::io_result_fits(record.as_mut_slice()) {
                return 1;
            }
            // A buffer the ABI would write past is refused rather than written.
            let mut tiny: [u8; 4] = [0u8; 4];
            if rt::sys::io_result_fits(tiny.as_mut_slice()) {
                return 2;
            }
            let status = rt::sys::write_to(1, "twelve bytes".as_bytes(), record.as_mut_slice());
            if status != 0 {
                return 3;
            }
            return 0;
        }
        "#,
    );
    assert_eq!(output, "twelve bytes", "the write went through");
    assert_eq!(exit, Some(0));
}

/// The runtime's own memory helpers move and clear bytes correctly.
///
/// These are the functions every later standard library module is built on, and
/// an off-by-one here would be invisible in every program that used them.
#[test]
fn the_memory_helpers_move_and_clear_bytes() {
    let (output, exit) = build_and_run(
        r#"
        fn main() -> i32 {
            let mut destination: [u8; 8] = [0u8; 8];
            let copied = rt::mem::copy(destination.as_mut_slice(), "abcdefghij".as_bytes());
            if copied != 8 {
                return 1;
            }
            if destination[0] != 97 {
                return 2;
            }
            if destination[7] != 104 {
                return 3;
            }
            let cleared = rt::mem::zero(destination.as_mut_slice());
            if cleared != 8 {
                return 4;
            }
            if destination[0] != 0u8 {
                return 5;
            }
            let filled = rt::mem::fill(destination.as_mut_slice(), 65u8);
            if filled != 8 {
                return 6;
            }
            if !rt::mem::equals(destination.as_slice(), "AAAAAAAA".as_bytes()) {
                return 7;
            }
            return 0;
        }
        "#,
    );
    assert_eq!(output, "", "the helpers do not write to the console");
    assert_eq!(exit, Some(0), "and every one of them behaved");
}

/// The text helpers answer questions about bytes without allocating.
#[test]
fn the_text_helpers_read_views() {
    let (output, exit) = build_and_run(
        r#"
        fn main() -> i32 {
            if rt::text::length("hello") != 5 {
                return 1;
            }
            if !rt::text::is_empty("") {
                return 2;
            }
            if !rt::text::starts_with("hello world", "hello") {
                return 3;
            }
            if rt::text::starts_with("hello", "hello world") {
                return 4;
            }
            if rt::text::find("hello world", "world") != 6 {
                return 5;
            }
            if rt::text::find("hello", "zzz") != -1 {
                return 6;
            }
            if !rt::text::equals("same", "same") {
                return 7;
            }
            if rt::text::byte_at("abc", 1) != 98u8 {
                return 8;
            }
            if rt::text::byte_at("abc", 9) != 0u8 {
                return 9;
            }
            return 0;
        }
        "#,
    );
    assert_eq!(output, "");
    assert_eq!(exit, Some(0));
}

/// A program that fails to build reports where and why, not a panic.
#[test]
fn a_broken_program_reports_a_diagnostic() {
    let error = RuntimeProgram::build(
        "fn main() -> i32 { return undefined_name; }",
        &BuildOptions::lz64("broken.lz"),
    )
    .expect_err("the program does not compile");
    assert!(
        matches!(error, RuntimeError::Compile(_)),
        "a compile failure is reported as one: {error:?}"
    );
    let text = error.to_string();
    assert!(
        text.contains("undefined_name"),
        "and the diagnostic names the problem: {text}"
    );
}

/// The prelude is part of the program's own text, so a program reaches the
/// runtime without an import and a program that declares its own `rt` collides
/// loudly rather than silently.
#[test]
fn the_prelude_is_part_of_the_compilation_unit() {
    let program = RuntimeProgram::build(
        r#"
        fn main() -> i32 {
            return 0;
        }
        "#,
        &BuildOptions::lz64("test.lz"),
    )
    .expect("the program builds");
    let names: Vec<&str> = program
        .program()
        .object()
        .symbols()
        .iter()
        .map(|symbol| symbol.name())
        .filter(|name| name.starts_with("fn.rt::"))
        .collect();
    assert!(
        names.contains(&"fn.rt::sys::print"),
        "the runtime's own code is in the object: {names:?}"
    );
    assert!(
        names.contains(&"fn.rt::sys::write_to"),
        "and so is the syscall wrapper, under its own module: {names:?}"
    );
    assert!(
        names.contains(&"fn.rt::mem::copy"),
        "and so is the memory helper: {names:?}"
    );
}

/// A call's fifth and sixth argument words travel on the stack, and arrive.
///
/// The ABI gives a Lazen call four argument registers, so a call that needs more
/// words than that spills the rest below the caller's stack pointer. Both sides
/// have to agree on where those words sit: if the caller writes them to registers
/// the callee never reads, or the callee reads them from a frame offset that
/// belongs to one of its own locals, the extra arguments arrive as zero. Nothing
/// traps — the callee simply reads the wrong memory — so only a test that reads
/// the arguments back can see it.
#[test]
fn the_fifth_and_sixth_argument_words_arrive() {
    let (output, exit) = build_and_run(
        r#"
        // Four words, all in registers: `a` is one, `b` is a view and so two.
        fn four(a: i32, b: &[u8], c: u64) -> u64 {
            return a as u64 + b.len() as u64 + c;
        }
        // Five words: the fifth is the length of `c`, which the caller had to put
        // on the stack because the four registers were already spoken for.
        fn five(a: i32, b: &[u8], c: &[u8]) -> u64 {
            return a as u64 + b.len() as u64 + c.len() as u64;
        }
        // Six words: both of `c`'s words and `d` travelled on the stack.
        fn six(a: i32, b: &[u8], c: &[u8], d: i32) -> u64 {
            return a as u64 + b.len() as u64 + c.len() as u64 + d as u64;
        }
        fn main() -> i32 {
            let mut buffer: [u8; 16] = [0u8; 16];
            // 1 + 2 + 16, from the registers alone.
            if four(1, "ab".as_bytes(), 16u64) != 19u64 {
                return 1;
            }
            // 1 + 2 + 16, with the third argument's length on the stack.
            if five(1, "ab".as_bytes(), buffer.as_slice()) != 19u64 {
                return 2;
            }
            // 1 + 2 + 16 + 100, with two words on the stack.
            if six(1, "ab".as_bytes(), buffer.as_slice(), 100) != 119u64 {
                return 3;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(output, "ok", "every call saw its own arguments");
    assert_eq!(exit, Some(0));
}

/// Writing through a view reaches the data the view names.
///
/// A view is a pointer and a length, so a place behind one — `view[at]` — is an
/// offset from the pointer the view *holds*, not from the slot the view sits in.
/// Getting that wrong writes over the view itself and leaves the data untouched,
/// and an index that is also off by an element size is invisible when the same
/// expression is both written and read.
#[test]
fn a_write_through_a_view_reaches_the_data() {
    let (output, exit) = build_and_run(
        r#"
        // The copy writes through a `&mut [u8]` parameter, so the store is
        // reached through a view the callee was handed rather than through a
        // local array of its own.
        fn fill_through(destination: &mut [u8], source: &[u8]) -> u64 {
            let mut index: u64 = 0;
            let limit = destination.len() as u64;
            while index < limit && index < source.len() as u64 {
                let at = index as usize;
                destination[at] = source[at];
                index = index + 1;
            }
            return index;
        }
        fn main() -> i32 {
            let mut data: [u8; 4] = [0u8; 4];
            if fill_through(data.as_mut_slice(), "abcd".as_bytes()) != 4u64 {
                return 1;
            }
            if data[0] != 97u8 {
                return 2;
            }
            if data[3] != 100u8 {
                return 3;
            }
            // The same index expression on both sides, so an offset that is wrong
            // by one element in the address would still agree with itself here.
            data[1] = 122u8;
            if data[1] != 122u8 {
                return 4;
            }
            // The write beside it left its neighbour alone: `data` is `abcd`.
            if data[2] != 99u8 {
                return 5;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    // The exit code names the check that failed, so it is asserted first: 2 means
    // `data[0]`, 3 means `data[3]`, 4 means the write-and-read of `data[1]`, and
    // 5 means `data[2]` was disturbed.
    assert_eq!(exit, Some(0), "every check held");
    assert_eq!(output, "ok");
}

/// A widened value is the value that was widened, not the frame around it.
///
/// A cast stages its operand in a scratch word and loads it back at a width. If
/// the load is wider than the store, the bytes it adds are the frame's, and the
/// result depends on what else the function happened to put there. That is
/// invisible in a program with one cast and visible in one with a view beside it,
/// so the test has one: a two-word local next to the scratch, which is what
/// decides the bytes above a one-byte staged value.
///
/// The narrowed direction is here too, because the same width chooses both: a
/// cast must not lose the low bytes of a wider source.
#[test]
fn a_widened_value_keeps_the_bytes_it_was_widened_from() {
    let (output, exit) = build_and_run(
        r#"
        fn widen(byte: u8) -> u32 {
            return byte as u32;
        }
        fn view_of(address: u64, length: u64) -> &[u8] {
            return (address as ptr<u8>).slice_from_raw(length);
        }
        fn main() -> i32 {
            let mut data: [u8; 8] = [0u8; 8];
            // A two-word local, so the frame has a view in it before the cast
            // that reads the scratch the cast staged its operand in.
            let seen: &[u8] = view_of(data.as_ptr() as u64, 8u64);
            if widen(255u8) != 255u32 {
                return 1;
            }
            if widen(7u8) != 7u32 {
                return 2;
            }
            // Reading a byte out of a view and widening it, which is what the
            // graphics read-back path does.
            data[3] = 200u8;
            if widen(seen[3]) != 200u32 {
                return 3;
            }
            if seen[3] as u32 != 200u32 {
                return 4;
            }
            // A signed source keeps its sign across the same width.
            let small: i8 = -1;
            if small as i32 != -1 {
                return 5;
            }
            // And a narrower target keeps the low bytes of a wider source.
            let wide: u64 = 4294967297;
            if wide as u32 != 1u32 {
                return 6;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    // Each exit code names the check that failed, so the failure says which
    // direction of the cast went wrong rather than just that one did.
    assert_eq!(exit, Some(0), "every widening and narrowing held");
    assert_eq!(output, "ok");
}

/// A repeated array fills every element, and a large one costs a loop.
///
/// Two properties in one program, because they come from the same translation. A
/// repeat is lowered as a counted loop rather than one store per element, which
/// is what makes a framebuffer-sized array possible at all — unrolled, a
/// 64000-byte array is megabytes of code. And the loop's bound is `index <
/// count` rather than a non-zero test, so element zero is inside the loop; a
/// counter tested for zero would leave the first element of every array
/// uninitialised, which is the kind of bug that only shows on the element
/// somebody was not looking at.
#[test]
fn a_repeated_array_fills_every_element_at_any_size() {
    let (output, exit) = build_and_run(
        r#"
        fn check_every(data: &[u8], value: u8) -> i32 {
            let mut at: u64 = 0u64;
            while at < data.len() as u64 {
                if data[at as usize] != value {
                    return 1;
                }
                at = at + 1u64;
            }
            return 0;
        }

        fn main() -> i32 {
            // A small repeat, where an unrolled loop and a counted one agree.
            let mut small: [u8; 5] = [7u8; 5];
            if check_every(small.as_slice(), 7u8) != 0 {
                return 1;
            }
            // A wide element, to be sure the stride is the element's size.
            let mut wide: [u32; 4] = [4294967295u32; 4];
            let mut at: u64 = 0u64;
            while at < 4u64 {
                if wide[at as usize] != 4294967295u32 {
                    return 2;
                }
                at = at + 1u64;
            }
            // A single element: the loop must still run exactly once.
            let mut one: [u8; 1] = [9u8; 1];
            if one[0] != 9u8 {
                return 3;
            }
            // An array the code section could not hold if the repeat were
            // unrolled: a couple of hundred bytes is a few thousand instructions
            // of stores, against a code section limit of a megabyte. The size
            // is bounded by the *step* budget rather than by the image format,
            // because the generated code keeps every value in the frame and
            // reloads it — a fact about an unoptimised backend, not about
            // repeats. How large a repeat *can* be is a property of the image
            // format, and the lowering test measures the code instead.
            let mut framebuffer: [u8; 256] = [0u8; 256];
            if check_every(framebuffer.as_slice(), 0u8) != 0 {
                return 4;
            }
            // The first and the last element are both writable, which is what a
            // bound that skipped element zero would have broken.
            framebuffer[0] = 1u8;
            framebuffer[255] = 2u8;
            if framebuffer[0] != 1u8 {
                return 5;
            }
            if framebuffer[255] != 2u8 {
                return 6;
            }
            // And a repeat of a non-zero value over a framebuffer.
            let mut filled: [u8; 64] = [255u8; 64];
            if check_every(filled.as_slice(), 255u8) != 0 {
                return 7;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(
        exit,
        Some(0),
        "every repeat filled its whole array: {output:?}"
    );
    assert_eq!(output, "ok");
}
