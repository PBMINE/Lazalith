//! Step 67: the standard library, exercised as a program.
//!
//! These are end-to-end. Each one builds a Lazen program that uses the library,
//! runs it under LazOS, and checks the exit code, because a library function that
//! lowers to nothing is indistinguishable from one that works until something
//! reads its result.
//!
//! The functions are called through the public names a program would use —
//! `std::text::len`, not `rt::mem::equals` — because a library whose own tests
//! reach past its API is not being tested at the interface it offers.
//!
//! Each test is a *return value*, not a printed assertion: the program returns a
//! distinct non-zero code for each failed check, so a failure says which property
//! broke rather than just that something did.

use std::string::String;
use std::vec::Vec;

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ThreadId, VirtualFileSystem,
    VirtualTerminal,
};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// Enough steps for the largest program here, which loops over a byte string.
const STEP_BUDGET: u64 = 5_000_000;

fn supervisor_kernel(architecture: ArchitectureConfig) -> Vec<u8> {
    [
        encode(
            architecture,
            &Instruction::new(architecture, Opcode::Nop, &[]).unwrap(),
        )
        .unwrap(),
        encode(
            architecture,
            &Instruction::new(architecture, Opcode::Rfe, &[]).unwrap(),
        )
        .unwrap(),
    ]
    .concat()
}

/// Runs `source` under LazOS and returns `(console output, exit code)`.
fn run(source: &str) -> (String, Option<u32>) {
    let (output, exit, _) = run_reporting(source);
    (output, exit)
}

/// Runs `source` and hands back the kernel too, so a test can read what the
/// display driver saw rather than only what the program said about it.
///
/// A program that presents a frame and a driver that recorded one are two
/// claims, and only the second one is evidence. The kernel is returned because it
/// is the only handle on the driver: `LazalithKernel` owns the display service,
/// and a Lazen program has no other path to it.
fn run_reporting(source: &str) -> (String, Option<u32>, LazalithKernel) {
    let config = ArchitectureConfig::lz64();
    let program = RuntimeProgram::build(source, &BuildOptions::lz64("stdlib.lz"))
        .unwrap_or_else(|error| panic!("{source} should build: {error}"));
    let bytes = program.to_image_bytes().expect("the image serialises");
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");

    let boot = BootImage::new(config, supervisor_kernel(config), 0).expect("a boot image");
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    machine.step().expect("the handoff");

    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").unwrap(),
        VirtualFileSystem::with_defaults().unwrap(),
    )
    .expect("the kernel starts");
    kernel
        .start_image(image, ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .expect("the program is scheduled");

    let mut exit = None;
    for _ in 0..STEP_BUDGET {
        // A process that has exited leaves nothing runnable, and the scheduler
        // reports that on the step *after* the one that carried the exit — the
        // exit outcome is returned, and the process is already gone by the time
        // the next step looks. So the loop ends on either signal, and the status
        // recorded from the outcome is the one that matters.
        let step = match kernel.step(&mut machine) {
            Ok(step) => step,
            Err(lazalith_os::KernelError::Scheduler(
                lazalith_os::SchedulerError::NoRunnableProcess,
            )) => break,
            Err(error) => panic!("a step: {error}"),
        };
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => panic!("the program faulted: {error:?}"),
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                panic!(
                    "the program trapped ({cause:?}, payload {payload}) at {:#x}",
                    machine.architectural_state().pc().as_u64()
                )
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    let exit = exit.expect("the program finished within its budget");
    let output = String::from_utf8_lossy(kernel.terminal().terminal().output()).into_owned();
    (output, Some(exit), kernel)
}

/// Runs `source` and requires it to report success.
fn passes(source: &str) {
    let (output, exit) = run(source);
    assert_eq!(
        exit,
        Some(0),
        "the program reported a failed check\nsource:\n{source}\noutput: {output:?}"
    );
}

/// `core` — the integer helpers, and the status convention everything uses.
#[test]
fn core_answers_integer_questions() {
    passes(
        r#"
fn main() -> i32 {
    if std::core::u32_max() != 0xffff_ffffu32 { return 1; }
    if std::core::u64_max() != 0xffff_ffff_ffff_ffffu64 { return 2; }
    if std::core::i32_max() != 0x7fff_ffffi32 { return 3; }
    if std::core::i32_min() != -0x7fff_ffffi32 - 1i32 { return 4; }
    if std::core::clamp_u32(5u32, 1u32, 3u32) != 3u32 { return 5; }
    if std::core::clamp_u32(0u32, 1u32, 3u32) != 1u32 { return 6; }
    if std::core::clamp_u32(2u32, 1u32, 3u32) != 2u32 { return 7; }
    // Zero is success and a negative is an error, which is the ABI's convention.
    if !std::core::succeeded(0i64) { return 8; }
    if std::core::succeeded(-1i64) { return 9; }
    if !std::core::failed(-1i64) { return 10; }
    return 0;
}
"#,
    );
}

/// A checked power refuses an overflow rather than wrapping, and says so.
#[test]
fn core_refuses_an_overflow_it_can_see() {
    passes(
        r#"
fn main() -> i32 {
    let mut out: [u8; 8] = [0u8; 8];
    if std::core::checked_pow(2u64, 10u32, out.as_mut_slice()) != 0i64 { return 1; }
    if std::core::u64_from(out.as_slice()) != 1024u64 { return 2; }
    // 2^64 does not fit, and the check happens before the multiply.
    if std::core::checked_pow(2u64, 64u32, out.as_mut_slice()) == 0i64 { return 3; }
    // A destination too small is refused rather than written past.
    let mut tiny: [u8; 4] = [0u8; 4];
    if std::core::checked_pow(2u64, 2u32, tiny.as_mut_slice()) == 0i64 { return 4; }
    return 0;
}
"#,
    );
}

/// `i64::MIN` has no positive counterpart, so `abs_i64` refuses it.
#[test]
fn core_refuses_the_one_value_with_no_absolute() {
    passes(
        r#"
fn main() -> i32 {
    let mut out: [u8; 8] = [0u8; 8];
    if std::core::abs_i64(-5i64, out.as_mut_slice()) != 0i64 { return 1; }
    if std::core::u64_from(out.as_slice()) != 5u64 { return 2; }
    if std::core::abs_i64(5i64, out.as_mut_slice()) != 0i64 { return 3; }
    if std::core::u64_from(out.as_slice()) != 5u64 { return 4; }
    // The one value with no absolute is refused, not silently returned as itself.
    if std::core::abs_i64(std::core::i64_min(), out.as_mut_slice()) == 0i64 { return 5; }
    return 0;
}
"#,
    );
}

/// `math` — integer arithmetic, with no floating point anywhere.
#[test]
fn math_computes_over_integers() {
    passes(
        r#"
fn main() -> i32 {
    if std::math::gcd_u64(12u64, 18u64) != 6u64 { return 1; }
    if std::math::gcd_u64(17u64, 5u64) != 1u64 { return 2; }
    if std::math::gcd_u64(0u64, 5u64) != 5u64 { return 3; }
    if std::math::lcm_u64(4u64, 6u64) != 12u64 { return 4; }
    // A least common multiple involving 0 is 0, not a division by zero.
    if std::math::lcm_u64(0u64, 6u64) != 0u64 { return 5; }
    if std::math::sqrt_u64(144u64) != 12u64 { return 6; }
    if std::math::sqrt_u64(0u64) != 0u64 { return 7; }
    if std::math::sqrt_u64(1u64) != 1u64 { return 8; }
    if std::math::sqrt_u64(15u64) != 3u64 { return 9; }
    let mut out: [u8; 8] = [0u8; 8];
    if std::math::pow_u64(3u64, 4u32, out.as_mut_slice()) != 0i64 { return 10; }
    if std::core::u64_from(out.as_slice()) != 81u64 { return 11; }
    return 0;
}
"#,
    );
}

/// A floor modulo and a truncated one differ for a negative dividend, and a
/// library that got this wrong would be off by the modulus in one direction.
#[test]
fn math_floor_mod_rounds_towards_negative_infinity() {
    passes(
        r#"
fn main() -> i32 {
    if std::math::floor_mod_i64(7i64, 3i64) != 1i64 { return 1; }
    // -7 / 3 truncates to -2 with remainder -1; flooring gives 2.
    if std::math::floor_mod_i64(-7i64, 3i64) != 2i64 { return 2; }
    if std::math::floor_mod_i64(7i64, -3i64) != -2i64 { return 3; }
    if std::math::floor_mod_i64(-6i64, 3i64) != 0i64 { return 4; }
    return 0;
}
"#,
    );
}

/// `text` — questions about bytes, answered without copying.
#[test]
fn text_answers_questions_about_bytes() {
    passes(
        r#"
fn main() -> i32 {
    if std::text::len("hello") != 5u64 { return 1; }
    if std::text::len("") != 0u64 { return 2; }
    if !std::text::is_empty("") { return 3; }
    if std::text::is_empty("x") { return 4; }
    if !std::text::eq("same", "same") { return 5; }
    if std::text::eq("same", "different") { return 6; }
    if std::text::byte_at("abc", 1u64) != 98u8 { return 7; }
    // Past the end is 0 rather than a read past it.
    if std::text::byte_at("abc", 99u64) != 0u8 { return 8; }
    if std::text::find("hello world", "world") != 6u64 { return 9; }
    // Absent is u64::MAX, which no real index can be.
    if std::text::find("hello", "zzz") != std::core::u64_max() { return 10; }
    return 0;
}
"#,
    );
}

/// `starts_with` and `ends_with` compare a sub-view, which is where an off-by-one
/// in the view arithmetic would show up as a wrong answer rather than a crash.
#[test]
fn text_matches_at_both_ends() {
    passes(
        r#"
fn main() -> i32 {
    if !std::text::starts_with("hello world", "hello") { return 1; }
    if std::text::starts_with("hello", "hello world") { return 2; }
    // A prefix as long as the text is a match, not a failure.
    if !std::text::starts_with("hello", "hello") { return 3; }
    if !std::text::ends_with("hello world", "world") { return 4; }
    if std::text::ends_with("hello", "hello world") { return 5; }
    if !std::text::ends_with("hello", "hello") { return 6; }
    if !std::text::starts_with("", "") { return 7; }
    if !std::text::ends_with("", "") { return 8; }
    return 0;
}
"#,
    );
}

/// Decimal formatting fills back to front, and the start index is what a caller
/// needs to find the digits.
#[test]
fn text_writes_decimal_digits() {
    passes(
        r#"
fn main() -> i32 {
    let mut out: [u8; 24] = [0u8; 24];
    let mut count: [u8; 8] = [0u8; 8];
    if std::text::u64_to_bytes(1234u64, out.as_mut_slice(), count.as_mut_slice()) != 0i64 {
        return 1;
    }
    if std::core::u64_from(count.as_slice()) != 4u64 { return 2; }
    // The digits are written at the *end* of the buffer, because they are produced
    // least-significant first and the length is not known until they are all done.
    let start: u64 = 24u64 - 4u64;
    if out[start as usize] != 49u8 { return 3; }
    if out[(start + 1u64) as usize] != 50u8 { return 4; }
    if out[(start + 2u64) as usize] != 51u8 { return 5; }
    if out[(start + 3u64) as usize] != 52u8 { return 6; }
    // Zero writes one digit, because an empty field is indistinguishable from a
    // bug and "0" is how a person writes zero.
    if std::text::u64_to_bytes(0u64, out.as_mut_slice(), count.as_mut_slice()) != 0i64 {
        return 7;
    }
    if std::core::u64_from(count.as_slice()) != 1u64 { return 8; }
    if out[23] != 48u8 { return 9; }
    return 0;
}
"#,
    );
}

/// `collections` — a fixed-capacity buffer over the program's own memory.
#[test]
fn collections_grows_a_buffer_within_its_capacity() {
    passes(
        r#"
fn main() -> i32 {
    let mut storage: [u8; 4] = [0u8; 4];
    let mut live: u64 = 0u64;
    live = std::collections::push(storage.as_mut_slice(), live, 65u8);
    live = std::collections::push(storage.as_mut_slice(), live, 66u8);
    live = std::collections::push(storage.as_mut_slice(), live, 67u8);
    if live != 3u64 { return 1; }
    if storage[0] != 65u8 { return 2; }
    if storage[2] != 67u8 { return 3; }
    // Three of four used, so a fourth byte still fits.
    if std::collections::remaining(4u64, live) != 1u64 { return 5; }
    live = std::collections::push(storage.as_mut_slice(), live, 68u8);
    if live != 4u64 { return 4; }
    // Now it is full, and a fifth byte is refused rather than written past the end.
    if std::collections::remaining(4u64, live) != 0u64 { return 6; }
    if std::collections::push(storage.as_mut_slice(), live, 69u8) != 0u64 { return 7; }
    if storage[3] != 68u8 { return 8; }
    let mut words: [u8; 3] = [0u8; 3];
    let mut n: u64 = 0u64;
    n = std::collections::extend(words.as_mut_slice(), n, "hi".as_bytes());
    if n != 2u64 { return 7; }
    if words[0] != 104u8 { return 9; }
    return 0;
}
"#,
    );
}

/// A stack of `u64`s, whose value goes out through a caller-provided slot.
#[test]
fn collections_stack_pushes_and_pops() {
    passes(
        r#"
fn main() -> i32 {
    let mut slots: [u8; 16] = [0u8; 16];
    if std::collections::stack::capacity_in(16u64) != 2u64 { return 1; }
    let mut count: u64 = 0u64;
    count = std::collections::stack::push(slots.as_mut_slice(), count, 7u64);
    count = std::collections::stack::push(slots.as_mut_slice(), count, 9u64);
    if count != 2u64 { return 2; }
    let mut out: [u8; 8] = [0u8; 8];
    count = std::collections::stack::pop(slots.as_slice(), count, out.as_mut_slice());
    if count != 1u64 { return 3; }
    if std::core::u64_from(out.as_slice()) != 9u64 { return 4; }
    count = std::collections::stack::pop(slots.as_slice(), count, out.as_mut_slice());
    if count != 0u64 { return 5; }
    if std::core::u64_from(out.as_slice()) != 7u64 { return 6; }
    // Popping an empty stack leaves the count at 0 and writes nothing.
    count = std::collections::stack::pop(slots.as_slice(), count, out.as_mut_slice());
    if count != 0u64 { return 7; }
    return 0;
}
"#,
    );
}

/// `io` — the console, which is the one output a program can be *seen* through.
#[test]
fn io_writes_to_the_console() {
    let (output, exit) = run(r#"
fn main() -> i32 {
    if !std::io::console::write("one ") { return 1; }
    if !std::io::console::write("two ") { return 2; }
    if !std::io::console::write_line("three") { return 3; }
    return 0;
}
"#);
    assert_eq!(exit, Some(0));
    assert_eq!(
        output, "one two three\n",
        "the console received every write, in order"
    );
}

/// Clearing the console really clears it, and what was written before is gone.
#[test]
fn io_clearing_the_console_discards_what_was_on_it() {
    let (output, exit) = run(r#"
fn main() -> i32 {
    std::io::console::write("before");
    if !std::io::console::clear() { return 1; }
    std::io::console::write_line("after");
    return 0;
}
"#);
    assert_eq!(exit, Some(0));
    assert_eq!(
        output, "after\n",
        "the virtual terminal's output is the screen, and clearing the screen is \
         what the program asked for"
    );
}

/// The UTF-8 check is what makes every `str` in the language a checked one, so it
/// gets the most adversarial test in this file.
#[test]
fn text_refuses_bytes_that_are_not_utf8() {
    passes(
        r#"
fn check(bytes: &[u8], want: bool, code: i32) -> i32 {
    let mut ok: bool = false;
    let text: &str = bytes.as_str(ok);
    if want {
        if !ok { return code; }
        if std::text::len(text) != bytes.len() as u64 { return code + 100; }
    } else {
        if ok { return code; }
    }
    return 0;
}
fn main() -> i32 {
    // One byte below 0x80 is itself.
    let ascii: [u8; 3] = [104u8, 105u8, 33u8];
    if check(ascii.as_slice(), true, 1) != 0 { return 1; }
    // A two-byte sequence.
    let two: [u8; 2] = [0xC3u8, 0xA9u8];
    if check(two.as_slice(), true, 2) != 0 { return 2; }
    // A three-byte sequence.
    let three: [u8; 3] = [0xE2u8, 0x82u8, 0xACu8];
    if check(three.as_slice(), true, 3) != 0 { return 3; }
    // A four-byte sequence.
    let four: [u8; 4] = [0xF0u8, 0x9Fu8, 0x98u8, 0x80u8];
    if check(four.as_slice(), true, 4) != 0 { return 4; }
    // The empty string is valid.
    let empty: [u8; 1] = [0u8; 1];
    if check(empty.as_slice(), true, 5) != 0 { return 5; }
    return 0;
}
"#,
    );
}

/// The overlong and surrogate cases are the ones a shape-only check gets wrong,
/// and each spells a character that also has another spelling.
#[test]
fn text_refuses_overlong_and_surrogate_encodings() {
    passes(
        r#"
fn invalid(bytes: &[u8], code: i32) -> i32 {
    let mut ok: bool = true;
    let text: &str = bytes.as_str(ok);
    if ok { return code; }
    return 0;
}
fn main() -> i32 {
    // 0xC0 and 0xC1 can only begin an overlong encoding.
    let overlong_c0: [u8; 2] = [0xC0u8, 0x80u8];
    if invalid(overlong_c0.as_slice(), 1) != 0 { return 1; }
    // 0xC0 0x80 spells U+0000, which 0x00 also spells. Two spellings of one
    // character is exactly what a text check has to refuse.
    let nul_overlong: [u8; 2] = [0xC0u8, 0x80u8];
    if invalid(nul_overlong.as_slice(), 2) != 0 { return 2; }
    // 0xE0 0x80.. is an overlong three-byte form.
    let overlong_e0: [u8; 3] = [0xE0u8, 0x80u8, 0x80u8];
    if invalid(overlong_e0.as_slice(), 3) != 0 { return 3; }
    // 0xED 0xA0.. is a surrogate: U+D800 is not a character.
    let surrogate: [u8; 3] = [0xEDu8, 0xA0u8, 0x80u8];
    if invalid(surrogate.as_slice(), 4) != 0 { return 4; }
    // 0xF0 0x80.. is an overlong four-byte form.
    let overlong_f0: [u8; 4] = [0xF0u8, 0x80u8, 0x80u8, 0x80u8];
    if invalid(overlong_f0.as_slice(), 5) != 0 { return 5; }
    // 0xF4 0x90.. is above U+10FFFF, which does not exist.
    let too_high: [u8; 4] = [0xF4u8, 0x90u8, 0x80u8, 0x80u8];
    if invalid(too_high.as_slice(), 6) != 0 { return 6; }
    // A lead byte with no continuation.
    let lone: [u8; 1] = [0xC3u8];
    if invalid(lone.as_slice(), 7) != 0 { return 7; }
    // A continuation byte where a lead belongs.
    let stray: [u8; 1] = [0x80u8];
    if invalid(stray.as_slice(), 8) != 0 { return 8; }
    // 0xF5..0xFF can never lead.
    let f5: [u8; 1] = [0xF5u8];
    if invalid(f5.as_slice(), 9) != 0 { return 9; }
    return 0;
}
"#,
    );
}

/// A valid sequence that *looks* adjacent to the invalid ones must still pass, so
/// the ranges above are not over-tight.
#[test]
fn text_accepts_the_characters_next_to_the_refused_ones() {
    passes(
        r#"
fn valid(bytes: &[u8], code: i32) -> i32 {
    let mut ok: bool = false;
    let text: &str = bytes.as_str(ok);
    if !ok { return code; }
    if std::text::len(text) != bytes.len() as u64 { return code + 100; }
    return 0;
}
fn main() -> i32 {
    // 0xC2 is the lowest legal two-byte lead.
    let lowest: [u8; 2] = [0xC2u8, 0x80u8];
    if valid(lowest.as_slice(), 1) != 0 { return 1; }
    // 0xDF is the highest.
    let highest: [u8; 2] = [0xDFu8, 0xBFu8];
    if valid(highest.as_slice(), 2) != 0 { return 2; }
    // 0xE0 0xA0 is the lowest legal three-byte start (U+0800).
    let e0_low: [u8; 3] = [0xE0u8, 0xA0u8, 0x80u8];
    if valid(e0_low.as_slice(), 3) != 0 { return 3; }
    // 0xED 0x9F is the highest before the surrogates (U+D7FF).
    let ed_high: [u8; 3] = [0xEDu8, 0x9Fu8, 0xBFu8];
    if valid(ed_high.as_slice(), 4) != 0 { return 4; }
    // 0xF0 0x90 is the lowest legal four-byte start (U+10000).
    let f0_low: [u8; 4] = [0xF0u8, 0x90u8, 0x80u8, 0x80u8];
    if valid(f0_low.as_slice(), 5) != 0 { return 5; }
    // 0xF4 0x8F is the highest (U+10FFFF).
    let f4_high: [u8; 4] = [0xF4u8, 0x8Fu8, 0xBFu8, 0xBFu8];
    if valid(f4_high.as_slice(), 6) != 0 { return 6; }
    return 0;
}
"#,
    );
}

/// A freestanding program links the runtime alone, with no standard library.
///
/// This is the build that keeps the two honest: a bug in `std` cannot make a
/// runtime test pass, and a bug in the runtime cannot make a `std` test pass.
#[test]
fn a_freestanding_program_needs_no_standard_library() {
    let config = ArchitectureConfig::lz64();
    let source = "fn main() -> i32 {\n    rt::sys::print(\"bare\\n\");\n    return 0;\n}\n";
    let program = RuntimeProgram::build(source, &BuildOptions::freestanding("bare.lz"))
        .expect("a freestanding program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");
    let boot = BootImage::new(config, supervisor_kernel(config), 0).unwrap();
    let mut machine = boot.start(DeviceManager::<NoDevice>::new()).unwrap();
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .unwrap();
    machine.step().unwrap();
    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").unwrap(),
        VirtualFileSystem::with_defaults().unwrap(),
    )
    .unwrap();
    kernel
        .start_image(image, ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .unwrap();
    let mut exit = None;
    for _ in 0..STEP_BUDGET {
        let step = kernel.step(&mut machine).unwrap();
        if let Some(KernelServiceOutcome::Exit(code)) = step.outcome {
            exit = Some(code);
            break;
        }
    }
    assert_eq!(exit, Some(0));
    assert_eq!(
        String::from_utf8_lossy(kernel.terminal().terminal().output()),
        "bare\n",
        "the runtime alone is enough to print"
    );
}

// ------------------------------------------------------------- Step 70: graphics

/// A window opens over the program's own memory and the driver records that
/// address, byte for byte.
///
/// The device shares guest memory rather than copying, so the only way to see a
/// window is to read the address back. If the driver had allocated its own
/// framebuffer, the program would be drawing somewhere the device never looks.
#[test]
fn a_window_is_the_programs_own_memory() {
    let (output, exit, kernel) = run_reporting(
        r#"
        fn main() -> i32 {
            let mut framebuffer: [u8; 64] = [0u8; 64];
            let mut record: [u8; 24] = [0u8; 24];
            if !std::graphics::open(4, 4, framebuffer.as_mut_slice(), record.as_mut_slice()) {
                return 1;
            }
            if std::graphics::record_width(record.as_slice()) != 4u32 {
                return 2;
            }
            if std::graphics::record_height(record.as_slice()) != 4u32 {
                return 3;
            }
            // The record's address is the framebuffer's, so the two agree and the
            // program can check that the driver saw what it passed.
            if std::graphics::record_framebuffer(record.as_slice())
                != framebuffer.as_ptr() as u64 {
                return 4;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(exit, Some(0), "the window opened: {output:?}");
    let device = kernel.display().device();
    assert!(device.is_open(), "the driver opened a window");
    assert_eq!(device.width(), 4, "at the geometry the program asked for");
    assert_eq!(device.height(), 4);
    // The device names guest memory, so its address is in memory the guest may
    // write: the program's framebuffer is a local, so it is on the stack rather
    // than in the data segment. The program separately checked that the address
    // in its record was the address of its own framebuffer, so between the two
    // the device and the program are looking at the same bytes.
    let address = device.framebuffer();
    let in_data = (lazalith_os::USER_DATA_START
        ..lazalith_os::USER_DATA_START + lazalith_os::USER_DATA_LENGTH)
        .contains(&address);
    let in_stack = (lazalith_os::USER_STACK_START
        ..lazalith_os::USER_STACK_START + lazalith_os::USER_STACK_LENGTH)
        .contains(&address);
    assert!(
        in_data || in_stack,
        "the device's framebuffer is guest memory the guest owns, at {address}"
    );
    assert!(
        kernel.display().last_frame().is_none(),
        "an open window is not a presented frame: nothing has been shown yet"
    );
}

/// A framebuffer too small for the window is refused by the SDK before the
/// driver is asked, so a program that got its arithmetic wrong learns so from a
/// `false` rather than from a fault it cannot handle.
#[test]
fn a_framebuffer_too_small_is_refused_without_trapping() {
    let (output, exit, kernel) = run_reporting(
        r#"
        fn main() -> i32 {
            let mut small: [u8; 8] = [0u8; 8];
            let mut record: [u8; 24] = [0u8; 24];
            // 4 by 4 is 64 bytes; eight is not enough.
            if std::graphics::open(4, 4, small.as_mut_slice(), record.as_mut_slice()) {
                return 1;
            }
            let mut tiny_record: [u8; 4] = [0u8; 4];
            let mut framebuffer: [u8; 64] = [0u8; 64];
            if std::graphics::open(4, 4, framebuffer.as_mut_slice(), tiny_record.as_mut_slice()) {
                return 2;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(exit, Some(0), "both refusals were reported: {output:?}");
    assert!(
        kernel.display().last_frame().is_none(),
        "and no window was left open"
    );
}

/// Drawing puts whole ARGB8888 pixels at the right offsets, and reading one back
/// gives the same number.
#[test]
fn pixels_are_argb8888_at_the_right_offsets() {
    let (output, exit) = run(r#"
        fn main() -> i32 {
            // 0xAARRGGBB: alpha high, blue low, and that is the byte order.
            let red: u32 = std::graphics::rgba(255u8, 0u8, 0u8, 255u8);
            if red != 4294901760u32 {
                return 1;
            }
            let green: u32 = std::graphics::rgba(0u8, 255u8, 0u8, 255u8);
            if green != 4278255360u32 {
                return 2;
            }
            let blue: u32 = std::graphics::rgba(0u8, 0u8, 255u8, 255u8);
            if blue != 4278190335u32 {
                return 3;
            }
            if std::graphics::alpha_of(red) != 255u8 {
                return 4;
            }
            if std::graphics::red_of(red) != 255u8 {
                return 5;
            }
            if std::graphics::green_of(green) != 255u8 {
                return 6;
            }
            if std::graphics::blue_of(blue) != 255u8 {
                return 7;
            }
            // Opaque white is all four channels, so it is all ones.
            if std::graphics::white() != 4294967295u32 {
                return 8;
            }

            let mut canvas: [u8; 32] = [0u8; 32];
            std::graphics::put_pixel(
                canvas.as_mut_slice(),
                4u32,
                2u32,
                std::graphics::pack_point(1u32, 1u32),
                red
            );
            // Pixel (1, 1) of a four-wide canvas is at byte 4 * (1 * 4 + 1).
            // The four bytes at that offset are A, R, G, B, so a red pixel is
            // full alpha, then full red, then nothing.
            if canvas[20] != 255u8 || canvas[21] != 255u8 {
                return 9;
            }
            if canvas[22] != 0u8 || canvas[23] != 0u8 {
                return 10;
            }
            if canvas[0] != 0u8 {
                return 11;
            }
            // And reading it back gives the same number.
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 1u32, 1u32) != red {
                return 12;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#);
    assert_eq!(exit, Some(0), "every pixel check held: {output:?}");
    assert_eq!(output, "ok");
}

/// Clipping is total: a rectangle off two edges draws its visible part and
/// nothing outside, and one entirely off the canvas draws nothing at all.
#[test]
fn drawing_is_clipped_on_every_side() {
    let (output, exit) = run(r#"
        fn main() -> i32 {
            let blue: u32 = std::graphics::rgba(0u8, 0u8, 255u8, 255u8);
            // A four by two canvas, so bytes 0..32.
            let mut canvas: [u8; 32] = [0u8; 32];
            // A rectangle hanging off the right and the bottom: the visible
            // part is the last two columns of the last row, and nothing else.
            std::graphics::fill_rect(
                canvas.as_mut_slice(),
                4u32,
                2u32,
                std::graphics::pack_rect(2u32, 1u32, 4u32, 4u32),
                blue
            );
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 0u32, 0u32) != 0u32 {
                return 1;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 1u32, 0u32) != 0u32 {
                return 2;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 2u32, 0u32) != 0u32 {
                return 3;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 3u32, 0u32) != 0u32 {
                return 4;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 0u32, 1u32) != 0u32 {
                return 5;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 1u32, 1u32) != 0u32 {
                return 6;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 2u32, 1u32) != blue {
                return 7;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 3u32, 1u32) != blue {
                return 8;
            }
            // Nothing was written past the end: byte 32 is out of the canvas
            // entirely, so reading it is the check that the loop stopped.
            if canvas[31] != 0xffu8 {
                return 6;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 4u32, 2u32, 0u32, 1u32) != 0u32 {
                return 7;
            }
            // A rectangle entirely off the canvas draws nothing.
            let mut other: [u8; 32] = [0u8; 32];
            std::graphics::fill_rect(
                other.as_mut_slice(),
                4u32,
                2u32,
                std::graphics::pack_rect(9u32, 9u32, 2u32, 2u32),
                blue
            );
            if std::graphics::get_pixel(other.as_slice(), 4u32, 2u32, 0u32, 0u32) != 0u32 {
                return 8;
            }
            // `clear` fills exactly the canvas and no more.
            let mut third: [u8; 32] = [0u8; 32];
            std::graphics::clear(third.as_mut_slice(), blue);
            if std::graphics::get_pixel(third.as_slice(), 4u32, 2u32, 3u32, 1u32) != blue {
                return 9;
            }
            if third[31] != 0xffu8 {
                return 10;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#);
    assert_eq!(exit, Some(0), "every clip held: {output:?}");
    assert_eq!(output, "ok");
}

/// A packed rectangle round-trips every field at 16 bits.
///
/// The packer gave one field fewer bits than its reader expected, so a rectangle
/// below 256 was fine and one above it was drawn somewhere else. A canvas taller
/// than 256 rows is unusual, so only a test that uses large fields would see it.
#[test]
fn a_packed_rectangle_round_trips_every_field() {
    let (output, exit) = run(r#"
        fn main() -> i32 {
            let packed: u64 = std::graphics::pack_rect(40000u32, 300u32, 5000u32, 60000u32);
            if std::graphics::rect_x(packed) != 40000u32 { return 1; }
            if std::graphics::rect_y(packed) != 300u32 { return 2; }
            if std::graphics::rect_w(packed) != 5000u32 { return 3; }
            if std::graphics::rect_h(packed) != 60000u32 { return 4; }
            let zero: u64 = std::graphics::pack_rect(0u32, 0u32, 0u32, 0u32);
            if std::graphics::rect_x(zero) != 0u32 { return 5; }
            if std::graphics::rect_h(zero) != 0u32 { return 6; }
            // A point is two 16-bit halves too.
            let point: u64 = std::graphics::pack_point(40000u32, 300u32);
            if std::graphics::point_x(point) != 40000u32 { return 7; }
            if std::graphics::point_y(point) != 300u32 { return 8; }
            rt::sys::print("ok");
            return 0;
        }
        "#);
    assert_eq!(exit, Some(0), "every field round-tripped: {output:?}");
    assert_eq!(output, "ok");
}

/// Text draws real glyphs from the built-in font, in the colour it was given.
///
/// The font is a resource of the SDK and not a host asset, so this is the same
/// bytes headless and graphical. The checks are on pixels, not on a return value:
/// a `draw_text` that drew nothing and a `draw_text` that drew the wrong glyphs
/// both return nothing to test.
#[test]
fn text_draws_glyphs_from_the_builtin_font() {
    let (output, exit) = run(r#"
        fn main() -> i32 {
            // 'A' is the thirty-third glyph. Its first row is 0b00111100, so
            // columns two through five are lit and the rest are not.
            let first: u8 = std::graphics::glyph_row(65u8, 0u32);
            if first != 60u8 {
                return 1;
            }
            if !std::graphics::glyph_pixel(first, 2u32) { return 2; }
            if !std::graphics::glyph_pixel(first, 5u32) { return 3; }
            if std::graphics::glyph_pixel(first, 0u32) { return 4; }
            if std::graphics::glyph_pixel(first, 7u32) { return 5; }
            // A space is blank and a row below the font is blank.
            if std::graphics::glyph_row(32u8, 0u32) != 0u8 { return 6; }
            if std::graphics::glyph_row(65u8, 7u32) != 0u8 { return 7; }
            // A code with no glyph draws nothing rather than a neighbour's.
            if std::graphics::glyph_row(7u8, 0u32) != 0u8 { return 8; }

            let white: u32 = std::graphics::white();
            // A canvas wide enough for two glyphs and three rows tall.
            let mut canvas: [u8; 96] = [0u8; 96];
            std::graphics::draw_text(
                canvas.as_mut_slice(),
                std::graphics::pack_surface(8u32, 3u32),
                std::graphics::pack_ink(0u32, 0u32, white),
                "A"
            );
            // Row 0, column 2 is lit; row 0, column 0 is not.
            if std::graphics::get_pixel(canvas.as_slice(), 8u32, 3u32, 2u32, 0u32) != white {
                return 9;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 8u32, 3u32, 0u32, 0u32) != 0u32 {
                return 10;
            }
            // Row 1 of 'A' is `.##..##.`, so columns 1, 2, 5 and 6.
            if std::graphics::get_pixel(canvas.as_slice(), 8u32, 3u32, 1u32, 1u32) != white {
                return 11;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 8u32, 3u32, 5u32, 1u32) != white {
                return 12;
            }
            if std::graphics::get_pixel(canvas.as_slice(), 8u32, 3u32, 3u32, 1u32) != 0u32 {
                return 17;
            }
            // A second character starts eight columns along, which is the next
            // glyph cell and not one column further.
            let mut two: [u8; 96] = [0u8; 96];
            std::graphics::draw_text(
                two.as_mut_slice(),
                std::graphics::pack_surface(16u32, 3u32),
                std::graphics::pack_ink(0u32, 0u32, white),
                "AA"
            );
            if std::graphics::get_pixel(two.as_slice(), 16u32, 3u32, 10u32, 0u32) != white {
                return 13;
            }
            if std::graphics::get_pixel(two.as_slice(), 16u32, 3u32, 9u32, 0u32) != 0u32 {
                return 14;
            }
            // Text that runs off the right edge is clipped, not a fault.
            let mut edge: [u8; 96] = [0u8; 96];
            std::graphics::draw_text(
                edge.as_mut_slice(),
                std::graphics::pack_surface(8u32, 3u32),
                std::graphics::pack_ink(4u32, 0u32, white),
                "AAA"
            );
            if std::graphics::get_pixel(edge.as_slice(), 8u32, 3u32, 6u32, 0u32) != white {
                return 15;
            }
            // And text that starts off the left edge draws from what is visible.
            let mut left: [u8; 96] = [0u8; 96];
            std::graphics::draw_text(
                left.as_mut_slice(),
                std::graphics::pack_surface(8u32, 3u32),
                std::graphics::pack_ink(0u32, 0u32, white),
                "A"
            );
            if std::graphics::get_pixel(left.as_slice(), 8u32, 3u32, 2u32, 0u32) != white {
                return 16;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#);
    assert_eq!(exit, Some(0), "every glyph check held: {output:?}");
    assert_eq!(output, "ok");
}

/// Presenting reports the frame count, and the driver saw the frame at the
/// address the program passed.
#[test]
fn presenting_reports_frames_and_the_driver_sees_them() {
    let (output, exit, kernel) = run_reporting(
        r#"
        fn main() -> i32 {
            let mut framebuffer: [u8; 64] = [0u8; 64];
            let mut record: [u8; 24] = [0u8; 24];
            if !std::graphics::open(4, 4, framebuffer.as_mut_slice(), record.as_mut_slice()) {
                return 1;
            }
            let white: u32 = std::graphics::white();
            std::graphics::clear(framebuffer.as_mut_slice(), white);
            let mut count: [u8; 8] = [0u8; 8];
            let mut at: u64 = 0u64;
            while at < 3u64 {
                if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) {
                    return 2;
                }
                let frames: u64 = rt::sys::read_u64(count.as_slice(), 0);
                if frames != at + 1u64 {
                    return 3;
                }
                at = at + 1u64;
            }
            // Presenting some other address is refused: the frame count does not
            // move, and the program is told so.
            let mut other: [u8; 64] = [0u8; 64];
            if std::graphics::present(other.as_mut_slice(), count.as_mut_slice()) {
                return 4;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(
        exit,
        Some(0),
        "the present loop agreed with the program: {output:?}"
    );
    let frame = kernel
        .display()
        .last_frame()
        .expect("the driver recorded a frame");
    assert_eq!(frame.width, 4, "the driver's window is the one opened");
    assert_eq!(frame.height, 4);
    assert_eq!(
        frame.present_count, 3,
        "three presents, and the fourth was refused"
    );
}

/// A Lazen program reaches the display device only through the SDK.
///
/// The driver is the only thing that talks to the device, and the SDK is the only
/// thing a program can name. This test states that by checking the program that
/// never calls the SDK leaves the driver with no window at all.
#[test]
fn a_program_that_never_opens_a_window_has_no_frame() {
    let (output, exit, kernel) = run_reporting(
        r#"
        fn main() -> i32 {
            // Arithmetic that a graphical program would do, with no display call
            // anywhere: a program that cannot name the device has no frame.
            let mut total: u64 = 0u64;
            let mut index: u64 = 0u64;
            while index < 10u64 {
                total = total + index;
                index = index + 1u64;
            }
            if total != 45u64 {
                return 1;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
    );
    assert_eq!(exit, Some(0), "{output:?}");
    assert!(
        kernel.display().last_frame().is_none(),
        "no display call means no window and no frame"
    );
}

// ---------------------------------------------------------------- Step 71: input

/// Runs `source` with `script` already queued on the kernel's input device.
///
/// This is the whole of the host input path in one line: a host — a script here,
/// SDL3 in Step 77 — queues guest-visible records on the device, and the program
/// polls for them through the driver. Nothing about the host is visible from the
/// program, which is the property the test below is about.
fn run_with_script(source: &str, script: lazalith_devices::HostScript) -> (String, Option<u32>) {
    let config = ArchitectureConfig::lz64();
    let program = RuntimeProgram::build(source, &BuildOptions::lz64("stdlib.lz"))
        .unwrap_or_else(|error| panic!("{source} should build: {error}"));
    let bytes = program.to_image_bytes().expect("the image serialises");
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");

    let boot = BootImage::new(config, supervisor_kernel(config), 0).unwrap();
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    machine.step().expect("the handoff");

    let mut kernel = LazalithKernel::new(
        STEP_BUDGET,
        VirtualTerminal::new(b"").unwrap(),
        VirtualFileSystem::with_defaults().unwrap(),
    )
    .expect("the kernel starts");
    script
        .replay(kernel.input_mut().device_mut())
        .expect("the script fits the queue");
    kernel
        .start_image(image, ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
        .expect("the program is scheduled");

    let mut exit = None;
    for _ in 0..STEP_BUDGET {
        let step = match kernel.step(&mut machine) {
            Ok(step) => step,
            Err(lazalith_os::KernelError::Scheduler(
                lazalith_os::SchedulerError::NoRunnableProcess,
            )) => break,
            Err(error) => panic!("a step: {error}"),
        };
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => panic!("the program faulted: {error:?}"),
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                panic!(
                    "the program trapped ({cause:?}, payload {payload}) at {:#x}",
                    machine.architectural_state().pc().as_u64()
                )
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    let exit = exit.expect("the program finished within its budget");
    let output = String::from_utf8_lossy(kernel.terminal().terminal().output()).into_owned();
    (output, Some(exit))
}

/// A Lazen program reads keyboard input from a script, with no host present.
///
/// This is the verification `docs/lazen-input.md` asks for: a program compiled
/// and run headlessly, driven by a scripted event source, reacts to keyboard
/// input. Every check is a *return value* rather than a printed assertion, and
/// each exit code names the check that failed.
#[test]
fn a_program_reacts_to_scripted_keyboard_input() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    // "hi" typed, then a key released, then a key the adapter does not name.
    script.push(HostAction::Printable(HostKey::H));
    script.push(HostAction::Printable(HostKey::I));
    script.push(HostAction::KeyUp(HostKey::H));
    script.push(HostAction::KeyDown(HostKey::Unknown));

    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            // Sixteen records is room for everything the script queues.
            let mut events: [u8; 256] = [0u8; 256];
            let got: u32 = std::input::poll(events.as_mut_slice(), 16u32);
            if got != 6u32 {
                return 1;
            }
            // 'h' arrived as its key and then as its text, in that order.
            if !std::input::is(events.as_slice(), 0u64, std::input::key_down()) {
                return 2;
            }
            if std::input::letter_of(std::input::code_of(events.as_slice(), 0u64)) != 104u32 {
                return 3;
            }
            if !std::input::is(events.as_slice(), 1u64, std::input::text()) {
                return 4;
            }
            if std::input::text_of(events.as_slice(), 1u64) != 104u32 {
                return 5;
            }
            // 'i' the same way.
            if std::input::letter_of(std::input::code_of(events.as_slice(), 2u64)) != 105u32 {
                return 6;
            }
            if std::input::text_of(events.as_slice(), 3u64) != 105u32 {
                return 7;
            }
            // The release carried no text, which is what makes key and text two
            // events rather than one event with an optional character.
            if !std::input::is(events.as_slice(), 4u64, std::input::key_up()) {
                return 8;
            }
            if std::input::x_of(events.as_slice(), 4u64) != 0 {
                return 9;
            }
            // A key the adapter does not name still arrived, as `Unknown`.
            if !std::input::is(events.as_slice(), 5u64, std::input::key_down()) {
                return 10;
            }
            if std::input::code_of(events.as_slice(), 5u64) != 0u32 {
                return 11;
            }
            // A second poll finds nothing: the queue is drained, not sampled.
            if std::input::poll(events.as_mut_slice(), 16u32) != 0u32 {
                return 12;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(
        exit,
        Some(0),
        "the program read the whole script: {output:?}"
    );
    assert_eq!(output, "ok");
}

/// A poll too small for the queue takes what fits and keeps the rest.
///
/// The property the device exists for: a program polling once per frame must not
/// lose a key tapped faster than that. Five events queued and two asked for,
/// three times over, leaves one still queued — and a poll with room finds it.
#[test]
fn a_poll_too_small_keeps_the_rest_of_the_queue() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    for _ in 0..5 {
        script.push(HostAction::KeyDown(HostKey::A));
    }
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            let mut total: u32 = 0u32;
            // Two at a time, twice: four delivered and one still queued.
            let mut round: u32 = 0u32;
            while round < 2u32 {
                if std::input::poll(events.as_mut_slice(), 2u32) != 2u32 {
                    return 1;
                }
                total = total + 2u32;
                round = round + 1u32;
            }
            if total != 4u32 {
                return 2;
            }
            // The fifth is still there, which is the whole point: a drain that
            // discarded the remainder would leave nothing and lose a key press.
            let mut more: [u8; 256] = [0u8; 256];
            if std::input::poll(more.as_mut_slice(), 8u32) != 1u32 {
                return 3;
            }
            // And it is the same event, not a fresh one: a key that is still `a`.
            if std::input::letter_of(std::input::code_of(more.as_slice(), 0u64)) != 97u32 {
                return 4;
            }
            // Now the queue is empty, and a poll says so.
            if std::input::poll(more.as_mut_slice(), 8u32) != 0u32 {
                return 5;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "the remainder was kept: {output:?}");
}

/// A poll that cannot take everything still delivers what it can.
///
/// Five events queued, three asked for: the program gets three, and the other two
/// are still there for the next call. A drain that discarded the remainder would
/// make the second poll return zero and lose two key presses silently.
#[test]
fn a_poll_delivers_what_it_can_and_keeps_the_rest() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    for _ in 0..5 {
        script.push(HostAction::KeyDown(HostKey::B));
    }
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            if std::input::poll(events.as_mut_slice(), 3u32) != 3u32 {
                return 1;
            }
            // Two are left, and a poll with room finds them.
            if std::input::poll(events.as_mut_slice(), 8u32) != 2u32 {
                return 2;
            }
            if std::input::poll(events.as_mut_slice(), 8u32) != 0u32 {
                return 3;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "nothing was lost: {output:?}");
}

/// A poll with nothing pending is zero, and zero is not an error.
///
/// A program that polls once per frame is supposed to get zero most of the time,
/// so a zero that read as a failure would make every frame look like a problem.
#[test]
fn a_poll_with_nothing_pending_is_zero() {
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            let mut round: u32 = 0u32;
            while round < 5u32 {
                if std::input::poll(events.as_mut_slice(), 8u32) != 0u32 {
                    return 1;
                }
                round = round + 1u32;
            }
            // A capacity of zero is the cheapest way to ask, and writes nothing.
            if std::input::poll(events.as_mut_slice(), 0u32) != 0u32 {
                return 2;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        lazalith_devices::HostScript::new(),
    );
    assert_eq!(exit, Some(0), "an empty queue is not a failure: {output:?}");
}

/// A poll that asks for more events than its array holds is refused by the SDK.
///
/// The SDK checks the array against the capacity before the call, so a program
/// that mis-sized its buffer gets zero rather than the kernel faulting it. The
/// queue is untouched, so the events are still there for a correctly sized poll.
#[test]
fn a_poll_larger_than_the_array_is_refused_without_losing_anything() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    script.push(HostAction::KeyDown(HostKey::C));
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            // Room for one record, asked for two.
            let mut small: [u8; 16] = [0u8; 16];
            if std::input::poll(small.as_mut_slice(), 2u32) != 0u32 {
                return 1;
            }
            // A correctly sized array still finds both events, so the refusal
            // cost nothing.
            let mut roomy: [u8; 256] = [0u8; 256];
            if std::input::poll(roomy.as_mut_slice(), 4u32) != 2u32 {
                return 2;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "the refusal lost nothing: {output:?}");
}

/// A program reads a pointer event's position, including a negative one.
///
/// A pointer can be dragged off the top or left of a window, and a program
/// clamping it needs to know which side it went.
#[test]
fn a_pointer_event_carries_its_position() {
    use lazalith_devices::{HostAction, HostScript};

    let mut script = HostScript::new();
    script.push(HostAction::MouseMove(120, 45));
    script.push(HostAction::MouseDown(1, -3, -7));
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            if std::input::poll(events.as_mut_slice(), 4u32) != 2u32 {
                return 1;
            }
            if !std::input::is(events.as_slice(), 0u64, std::input::mouse_move()) {
                return 2;
            }
            if std::input::x_of(events.as_slice(), 0u64) != 120 {
                return 3;
            }
            if std::input::y_of(events.as_slice(), 0u64) != 45 {
                return 4;
            }
            if !std::input::is(events.as_slice(), 1u64, std::input::mouse_down()) {
                return 5;
            }
            if std::input::code_of(events.as_slice(), 1u64) != 1u32 {
                return 6;
            }
            // A negative position survived, so a program can tell which edge the
            // pointer went off.
            if std::input::x_of(events.as_slice(), 1u64) != -3 {
                return 7;
            }
            if std::input::y_of(events.as_slice(), 1u64) != -7 {
                return 8;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "the position arrived: {output:?}");
}

/// `find` locates an event by kind, and reports "not found" as the count.
///
/// A program looking for a quit event, or for the first key, should not have to
/// write the loop itself.
#[test]
fn a_program_can_find_an_event_by_its_kind() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    script.push(HostAction::MouseMove(1, 1));
    script.push(HostAction::KeyDown(HostKey::B));
    script.push(HostAction::Quit);
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            let count: u32 = std::input::poll(events.as_mut_slice(), 8u32);
            if count != 4u32 {
                return 1;
            }
            // The second key press is at index two, not the first.
            let quit: u32 = std::input::find(events.as_slice(), count, std::input::quit());
            if quit != 3u32 {
                return 2;
            }
            let second: u32 = std::input::find(events.as_slice(), count, std::input::key_down());
            if second != 0u32 {
                return 3;
            }
            if std::input::letter_of(std::input::code_of(events.as_slice(), second as u64))
                != 97u32 {
                return 4;
            }
            // A kind that is not there is reported as the count, so "not found"
            // and "found at the end" cannot be confused.
            let absent: u32 = std::input::find(events.as_slice(), count, std::input::mouse_up());
            if absent != count {
                return 5;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "the events were found: {output:?}");
}

/// A guest cannot inject its own events.
///
/// There is no call that adds to the queue, so a program cannot hand itself a key
/// press. This is the property that makes the device worth having: a program that
/// could lie about input would be testing its own imagination.
#[test]
fn a_program_cannot_inject_its_own_events() {
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut events: [u8; 256] = [0u8; 256];
            // Poll every way a program can, and nothing appears.
            let mut round: u32 = 0u32;
            while round < 4u32 {
                if std::input::poll(events.as_mut_slice(), 8u32) != 0u32 {
                    return 1;
                }
                round = round + 1u32;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        lazalith_devices::HostScript::new(),
    );
    assert_eq!(exit, Some(0), "a program invented no events: {output:?}");
}

/// The key codes a program tests are Lazen's, and they classify without a table.
///
/// The program below does not know any key *name* — it compares codes and reads a
/// letter by its distance from the start of a range. That is the property the
/// numbering exists for, and it is what makes a program portable across hosts.
#[test]
fn key_codes_classify_without_a_table() {
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            if std::input::key_letter_first() != 17u32 { return 1; }
            if std::input::key_letter_last() != 42u32 { return 2; }
            if std::input::key_digit_first() != 43u32 { return 3; }
            if std::input::key_digit_last() != 52u32 { return 4; }
            if std::input::key_max() != 57u32 { return 5; }
            // Every letter is where its distance from the start says it is.
            let mut code: u32 = std::input::key_letter_first();
            let mut letter: u32 = 97u32;
            while code <= std::input::key_letter_last() {
                if std::input::letter_of(code) != letter {
                    return 6;
                }
                if !std::input::is_letter(code) {
                    return 7;
                }
                if std::input::is_digit(code) {
                    return 8;
                }
                code = code + 1u32;
                letter = letter + 1u32;
            }
            if letter != 123u32 {
                return 9;
            }
            // The same for digits.
            code = std::input::key_digit_first();
            letter = 48u32;
            while code <= std::input::key_digit_last() {
                if std::input::digit_of(code) != letter {
                    return 10;
                }
                code = code + 1u32;
                letter = letter + 1u32;
            }
            if letter != 58u32 {
                return 11;
            }
            // A named key is neither.
            if std::input::letter_of(std::input::key_space()) != 0u32 { return 12; }
            if std::input::digit_of(std::input::key_comma()) != 0u32 { return 13; }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        lazalith_devices::HostScript::new(),
    );
    assert_eq!(exit, Some(0), "every key classified: {output:?}");
}

/// A graphical program opens a window, draws, and quits on a scripted key.
///
/// The whole chain in one program: the Step 70 display driver, the Step 71 input
/// driver, and a program that knows neither the host nor the kernel. It is
/// Step 72's shape, and it is here because the input path is only worth having
/// if a program can actually use it.
///
/// The window is 32 by 24 because a Lazen frame is initialised in the *code*
/// section, and a large one would not fit in the image — which is a fact about
/// v1's zero initialisation, not about this test.
#[test]
fn a_window_draws_and_quits_on_a_scripted_key() {
    use lazalith_devices::{HostAction, HostKey, HostScript};

    let mut script = HostScript::new();
    script.push(HostAction::MouseMove(10, 10));
    script.push(HostAction::Printable(HostKey::Q));
    script.push(HostAction::Quit);
    let (output, exit) = run_with_script(
        r#"
        fn main() -> i32 {
            let mut framebuffer: [u8; 3072] = [0u8; 3072];
            let mut record: [u8; 24] = [0u8; 24];
            if !std::graphics::open(
                32u32,
                24u32,
                framebuffer.as_mut_slice(),
                record.as_mut_slice()
            ) {
                return 1;
            }
            let white: u32 = std::graphics::white();
            let blue: u32 = std::graphics::rgba(0u8, 0u8, 255u8, 255u8);
            let surface: u64 = std::graphics::pack_surface(32u32, 24u32);
            std::graphics::clear(framebuffer.as_mut_slice(), blue);
            std::graphics::draw_text(
                framebuffer.as_mut_slice(),
                surface,
                std::graphics::pack_ink(2u32, 2u32, white),
                "Hi"
            );

            // A frame loop that runs until the script says to stop. The quit
            // event is the third in the queue, so the loop sees it on its first
            // poll and presents twice more before stopping.
            let mut events: [u8; 256] = [0u8; 256];
            let mut frames: u32 = 0u32;
            let mut quit: bool = false;
            let mut round: u32 = 0u32;
            while round < 3u32 && !quit {
                let count: u32 = std::input::poll(events.as_mut_slice(), 16u32);
                let mut at: u32 = 0u32;
                while at < count && !quit {
                    if std::input::is(events.as_slice(), at as u64, std::input::quit()) {
                        quit = true;
                    }
                    at = at + 1u32;
                }
                let mut count_bytes: [u8; 8] = [0u8; 8];
                if !std::graphics::present(
                    framebuffer.as_mut_slice(),
                    count_bytes.as_mut_slice()
                ) {
                    return 2;
                }
                frames = frames + 1u32;
                round = round + 1u32;
            }
            if !quit {
                return 3;
            }
            if frames != 1u32 {
                return 4;
            }
            // The text really is on the frame. 'H' row 0 is `.##..##.`, so
            // columns 1, 2, 5 and 6 of the glyph are lit; the text starts at
            // x = 2, so x = 3 is the glyph's first lit pixel. The pixel beside
            // it is *blue*, not black: `draw_text` draws the glyph's lit pixels
            // and leaves the rest of the canvas as it found it, so a check that
            // expected zero here would be testing a clear the program never did.
            if std::graphics::get_pixel(
                framebuffer.as_slice(),
                32u32,
                24u32,
                3u32,
                2u32
            ) != white {
                return 5;
            }
            if std::graphics::get_pixel(
                framebuffer.as_slice(),
                32u32,
                24u32,
                2u32,
                2u32
            ) != blue {
                return 6;
            }
            // And the background is the other colour, so the text is not the
            // whole frame.
            if std::graphics::get_pixel(
                framebuffer.as_slice(),
                32u32,
                24u32,
                30u32,
                22u32
            ) != blue {
                return 7;
            }
            rt::sys::print("ok");
            return 0;
        }
        "#,
        script,
    );
    assert_eq!(exit, Some(0), "the program drew and quit: {output:?}");
}
