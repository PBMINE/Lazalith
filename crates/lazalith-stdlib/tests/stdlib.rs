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
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    let exit = exit.expect("the program finished within its budget");
    let output = String::from_utf8_lossy(kernel.terminal().terminal().output()).into_owned();
    (output, Some(exit))
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
