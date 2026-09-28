//! Hardening: the calling convention and the frame, under the loads that break them.
//!
//! A frame-layout defect does not show up in a program that calls one function once
//! with two arguments. It shows up when a frame is deep enough for the stack pointer
//! to move a long way, when a call's outgoing arguments have to live *beside* the
//! caller's own locals, when a temporary has to survive across a call, or when a
//! register is live across one.
//!
//! So this file builds programs that do all four and checks them against values
//! computed by hand. Every program returns its result as its exit status, because a
//! returned value that is *almost* right is the failure mode that matters and a
//! console comparison could hide it in surrounding output.

use lazalith_cpu::{Privilege, StatusRegister};
use lazalith_machine::{LazalithMachine, MachineEvent, MachineSetup};
use lazalith_memory::{MemoryRegion, RegionPermissions};
use lazalith_os::{
    DispatchOutcome, FileSystemService, LzxImage, ProcessId, RoundRobinScheduler, TerminalService,
    ThreadId, VirtualFileSystem, VirtualTerminal,
};
use lazalith_types::{ArchitectureConfig as C, CycleCount, InstructionAddress, PhysicalAddress};

/// How many instructions a program is given.
const MAX_STEPS: usize = 40_000_000;

fn test_machine(config: C) -> LazalithMachine<lazalith_devices::NoDevice> {
    let regions = vec![
        MemoryRegion::ram(
            config,
            PhysicalAddress::new(0x1000),
            16,
            RegionPermissions::new(true, true, true, false),
        )
        .expect("a trap page"),
    ];
    let mut machine = LazalithMachine::new(MachineSetup {
        config,
        devices: lazalith_devices::DeviceManager::new(),
        regions,
        pc: InstructionAddress::new(0),
        sp: lazalith_types::VirtualAddress::new(0x0080_0000),
        status: StatusRegister::new(Privilege::Supervisor, false).bits(),
        initial_time: CycleCount::new(0),
    })
    .expect("a machine");
    let rfe = lazalith_isa::Instruction::new(config, lazalith_isa::Opcode::Rfe, &[]).unwrap();
    machine
        .load_bytes(
            PhysicalAddress::new(0x1000),
            &lazalith_isa::encode(config, &rfe).unwrap(),
        )
        .expect("a trap handler");
    machine.reset();
    machine
        .set_trap_vector(InstructionAddress::new(0x1000))
        .expect("a trap vector");
    machine
}

/// Compiles, links and runs a Lazen program, reporting its exit status.
///
/// The program is run through the same path a user's program takes: a real image
/// read back from its own bytes, a real loader, a real machine, and the kernel
/// dispatching the syscalls.
fn run(source: &str) -> u32 {
    let config = C::lz64();
    let options = lazalith_runtime::BuildOptions {
        architecture: config,
        source_path: String::from("frame.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build:\n{error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"));
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");
    let process = image
        .load_process(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .expect("the image loads");
    let mut scheduler = RoundRobinScheduler::new(MAX_STEPS as u64).expect("a scheduler");
    scheduler.add_process(process).expect("scheduled");
    let filesystem = VirtualFileSystem::with_defaults().expect("a filesystem");
    let mut service = TerminalService::new(
        VirtualTerminal::new(b"").expect("a terminal"),
        FileSystemService::new(filesystem),
    );
    let mut machine = test_machine(config);
    let mut exit_code = None;
    for _ in 0..MAX_STEPS {
        let step = scheduler.step(&mut machine).expect("a step");
        if let MachineEvent::Trapped { event } = &step.event
            && event.cause == lazalith_cpu::TrapCause::Syscall
        {
            let outcome = scheduler
                .dispatch_syscall(&mut machine, &mut service)
                .expect("a dispatch");
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
                DispatchOutcome::Fault(error) => {
                    panic!("the program faulted: {error:?}")
                }
            }
        }
    }
    exit_code.unwrap_or_else(|| panic!("the program did not finish in {MAX_STEPS} steps"))
}

#[test]
fn a_deep_recursion_keeps_every_frames_temporary_to_itself() {
    // Twenty levels, each adding a live temporary, and the answer depends on every
    // frame being the right size. A frame that is one word short turns the deepest
    // call's live value into the *next* frame's incoming argument, and the answer
    // comes back plausible and wrong.
    let source = r#"
fn descend(level: i32, seed: i32) -> i32 {
    if level <= 0i32 {
        return seed;
    }
    let a: i32 = seed + level;
    let b: i32 = a * 2i32;
    let c: i32 = b - 1i32;
    return descend(level - 1i32, c);
}

fn main() -> i32 {
    return descend(20i32, 0i32);
}
"#;
    // Worked out by hand: seed_{n} = (seed_{n-1} + n) * 2 - 1.
    let mut expected = 0i32;
    for level in (1..=20i32).rev() {
        expected = (expected + level) * 2 - 1;
    }
    assert_eq!(
        run(source),
        expected as u32,
        "every frame's temporaries must belong to that frame"
    );
}

#[test]
fn a_call_inside_an_expression_keeps_the_outgoing_arguments() {
    // The outgoing-argument area sits below the caller's frame, and a call in the
    // middle of an expression has to place its arguments there without disturbing
    // the operands the expression has already computed. Three calls, nested, with
    // more arguments than fit in registers, is where that breaks.
    let source = r#"
fn add3(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {
    return a + b + c + d + e;
}

fn nest(x: i32) -> i32 {
    return add3(x, 1i32, 2i32, 3i32, 4i32) + add3(5i32, x, 6i32, 7i32, 8i32);
}

fn main() -> i32 {
    return nest(10i32) + nest(20i32);
}
"#;
    // Computed here rather than written down, because a hand-written constant is
    // only as reliable as the arithmetic behind it — and the first draft of this
    // file said 140, which is what a careful reader would also have got wrong.
    let add3 = |a: i32, b: i32, c: i32, d: i32, e: i32| a + b + c + d + e;
    let nest = |x: i32| add3(x, 1, 2, 3, 4) + add3(5, x, 6, 7, 8);
    let expected = nest(10) + nest(20);
    assert_eq!(
        run(source),
        expected as u32,
        "a call in the middle of an expression must not disturb what is already \
         computed; add3(nest(10) + nest(20)) is {expected}"
    );
}

#[test]
fn a_recursive_function_with_many_parameters_still_converges() {
    // A recursive call is the same as any other call and also the easiest place for
    // the outgoing area to be clobbered, because every frame does it at once. Ten
    // parameters, depth twenty, and a sum that depends on all of them.
    let source = r#"
fn deep(n: i32, a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {
    if n <= 0i32 {
        return a + b + c + d + e;
    }
    return deep(n - 1i32, a + 1i32, b + 2i32, c + 3i32, d + 4i32, e + 5i32);
}

fn main() -> i32 {
    return deep(20i32, 0i32, 0i32, 0i32, 0i32, 0i32);
}
"#;
    // Computed, not written down: each level adds 1 + 2 + 3 + 4 + 5, twenty times.
    // A hand-written constant here was 900, because the first draft of the program
    // had ten parameters adding 45 each and the parameter count was later reduced to
    // the ABI's six-word limit. The program was right and the constant was stale,
    // which is the argument for computing it.
    let mut expected = 0i32;
    for _ in 0..20 {
        expected += 1 + 2 + 3 + 4 + 5;
    }
    assert_eq!(
        run(source),
        expected as u32,
        "twenty levels of fifteen each"
    );
}

#[test]
fn a_value_live_across_a_call_is_not_clobbered() {
    // The classic register-allocation question: a value computed before a call, still
    // needed after it, and stored in a register the call also wants. The compiler has
    // to spill it, and a frame that forgets to makes the answer depend on which
    // register happened to survive.
    let source = r#"
fn side_effect(x: i32) -> i32 {
    return x * 3i32;
}

fn main() -> i32 {
    let before: i32 = 7i32;
    let wasted: i32 = side_effect(before);
    let after: i32 = before + 1i32;
    return before + after + wasted;
}
"#;
    // 7 + 8 + 21 = 36, and it is only 36 if `before` survived the call.
    assert_eq!(
        run(source),
        36,
        "a value live across a call must survive the call"
    );
}

#[test]
fn a_call_inside_a_loop_reuses_its_outgoing_area_safely() {
    // The same area, used a thousand times, with a value from the previous
    // iteration still live across it. An area that is not reset, or a frame that
    // grows per iteration, shows up here.
    let source = r#"
fn accumulate(value: i32, step: i32) -> i32 {
    return value * 10i32 + step;
}

fn main() -> i32 {
    let mut total: i32 = 0i32;
    let mut i: i32 = 0i32;
    while i < 10i32 {
        total = accumulate(total, i);
        i = i + 1i32;
    }
    return total;
}
"#;
    // total_{n+1} = total_n * 10 + n, starting at 0, for n = 0..9.
    let mut expected = 0i32;
    for step in 0..10i32 {
        expected = expected * 10 + step;
    }
    assert_eq!(
        run(source),
        expected as u32,
        "ten calls in a loop must not accumulate damage in the frame"
    );
}

#[test]
fn a_frame_does_not_survive_its_function() {
    // Two calls in sequence where the second reuses the first's frame space: if the
    // frame is not reset on entry, the second call starts with the first's leftovers
    // in its temporaries. Both calls return the same value from the same code, and
    // the second one must not see the first.
    let source = r#"
fn initialised(value: i32) -> i32 {
    let a: i32 = value;
    let b: i32 = a * 2i32;
    let c: i32 = b + 1i32;
    return c;
}

fn main() -> i32 {
    let first: i32 = initialised(5i32);
    let second: i32 = initialised(5i32);
    return first * 1000i32 + second;
}
"#;
    // Both are 11, so 11 * 1000 + 11 = 11011.
    assert_eq!(
        run(source),
        11_011,
        "a frame must not leak into the next call of the same function"
    );
}
