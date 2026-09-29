//! Hardening: the same algorithms in Lazen and in C, compared.
//!
//! Two front ends that share one IR, one lowerer and one code generator. That makes
//! their agreement meaningful in a way that neither one's own tests can be: if the
//! two languages disagree about a program's output, then at least one of them is
//! wrong, and the disagreement localises the defect to the front end rather than to
//! the machinery underneath.
//!
//! This is the closest thing to an oracle this project has. There is no reference
//! implementation of Lazen and no reference implementation of C; there are two
//! independent implementations of *overlapping* semantics, and a mismatch is a bug
//! in one of them that no amount of testing either alone would have found.
//!
//! Every program prints a value **it computed**, never a literal: a program that got
//! the arithmetic wrong cannot pass by printing the right thing. The expected values
//! were computed by hand and written into the table before the programs were run, so
//! agreement between the two front ends is evidence but a shared misreading of the
//! specification is not ruled out by agreement alone — which is why the second test
//! exists.

use lazalith_types::ArchitectureConfig;

/// Builds, links and runs a Lazen program, and reports what it printed and returned.
fn run_lazen(source: &str) -> (String, u32) {
    let options = lazalith_runtime::BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("diff.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the Lazen program should build:\n{error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the Lazen program should link: {error}"));
    finish(bytes)
}

/// Builds, links and runs a C program, and reports what it printed and returned.
fn run_c(source: &str) -> (String, u32) {
    use lazalith_codegen::{CodegenOptions, generate};
    use lazalith_toolchain::{LinkOptions, link_objects};

    let config = ArchitectureConfig::lz64();
    // The C runtime is one translation unit *in front of* the program, exactly as the
    // Lazen standard library is, so a C program may call `print_line_number` the
    // same way a Lazen program may call `rt::sys::print`.
    let mut unit = String::from(lazalith_c_runtime::C_RUNTIME);
    unit.push(char::from(10u8));
    unit.push_str(source);
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = lazalith_c_compiler::compile(&mut sources, "diff.c", &unit)
        .unwrap_or_else(|error| panic!("the C program should compile:\n{}", error.render()));
    let lowered = lazalith_c_compiler::lower(&checked)
        .unwrap_or_else(|error| panic!("the C program should lower: {error}"));
    let program = generate(
        &lowered.module,
        &lowered.frames,
        lowered.entry.as_deref(),
        &CodegenOptions::lz64("diff.c"),
        source,
    )
    .unwrap_or_else(|error| panic!("the C program should generate: {error}"));
    let startup = lazalith_runtime::startup_object_for(config, "fn.c.main")
        .expect("the C entry sequence assembles");
    let linked = link_objects(
        &[program.object().clone(), startup],
        &LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("the C program should link: {error}"));
    let bytes = linked
        .image()
        .to_bytes()
        .unwrap_or_else(|error| panic!("the C image should serialise: {error}"));
    finish(bytes)
}

fn finish(bytes: Vec<u8>) -> (String, u32) {
    use lazalith_devices::{DeviceManager, NoDevice};
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// A C helper that prints a signed number, because the C half of the runtime has
/// `putchar` and no number formatter — the `print_line_number` in
/// `lazalith_c_runtime::VARIADIC` is the *Lazen* half. So the digits are computed
/// here, which is also a better test: the C program is doing the formatting rather
/// than calling a library that would do it identically to the Lazen one.
const C_SHOW: &str = r#"
static void show(long value) {
    unsigned long magnitude;
    char digits[24];
    unsigned at = 0, i;
    if (value < 0) { putchar(45); magnitude = (unsigned long)(0 - value); }
    else { magnitude = (unsigned long)value; }
    if (magnitude == 0) { putchar(48); putchar(10); return; }
    while (magnitude != 0) {
        digits[at] = (char)(48 + (magnitude % 10));
        magnitude = magnitude / 10;
        at = at + 1;
    }
    i = at;
    while (i != 0) { i = i - 1; putchar(digits[i]); }
    putchar(10);
}
"#;
/// A Lazen prelude that can print a number, so no case has to print a literal.
///
/// The digits are produced least-significant-first and the buffer is filled back to
/// front, so the caller prints the *last* `n` bytes — a detail the standard library's
/// own `write_u64` documents and this has to honour.
const PRINT: &str = r#"
fn show(value: i64) {
    let mut negative: bool = value < 0i64;
    let mut magnitude: i64 = value;
    if negative {
        magnitude = 0i64 - value;
    }
    let mut digits: [u8; 24] = [0u8; 24];
    let written: u64 = std::text::write_u64(magnitude as u64, digits.as_mut_slice());
    let mut text: [u8; 24] = [0u8; 24];
    // The sign goes in *first* and the digits start after it, or the first digit
    // overwrites the `-` and a negative number prints as `-0`.
    let mut head: u64 = 0u64;
    if negative {
        text[0] = 45u8;
        head = 1u64;
    }
    let mut at: u64 = 0u64;
    while at < written {
        text[(head + at) as usize] = digits[(24u64 - written + at) as usize];
        at = at + 1u64;
    }
    let total: u64 = written + head;
    // Two writes, not one: the newline was being written into the *result* buffer
    // rather than printed, and a missing newline is exactly the kind of small
    // difference that makes two implementations look like they disagree.
    rt::sys::write(1, text.as_mut_slice().as_ptr(), total, text.as_mut_slice().as_ptr());
    rt::sys::print("\n");
}
"#;

/// One algorithm, written twice, each printing what it computed.
struct Case {
    name: &'static str,
    lazen_body: &'static str,
    c_body: &'static str,
    /// What both must print, computed by hand and written down first.
    expect: &'static str,
    /// What both must return as their status, also by hand.
    status: u32,
}

const CASES: &[Case] = &[
    Case {
        name: "a_sum_of_a_loop",
        lazen_body: r#"
    let mut total: i32 = 0i32;
    let mut i: i32 = 1i32;
    while i <= 100i32 {
        total = total + i;
        i = i + 1i32;
    }
    show(total as i64);
    return total;
"#,
        c_body: r#"
    int total = 0, i = 1;
    while (i <= 100) { total = total + i; i = i + 1; }
    show(total);
    return total;
"#,
        // 100 * 101 / 2
        expect: "5050\n",
        status: 5050,
    },
    Case {
        name: "a_fibonacci_sequence",
        lazen_body: r#"
    let mut a: i32 = 0i32;
    let mut b: i32 = 1i32;
    let mut i: i32 = 0i32;
    let mut last: i32 = 0i32;
    while i < 20i32 {
        last = a + b;
        a = b;
        b = last;
        i = i + 1i32;
    }
    show(last as i64);
    return last;
"#,
        c_body: r#"
    int a = 0, b = 1, i = 0, last = 0;
    while (i < 20) { last = a + b; a = b; b = last; i = i + 1; }
    show(last);
    return last;
"#,
        // After twenty iterations of `last = a + b`, `last` is the twenty-first
        // Fibonacci number, 10946. (F(20) is 6765 and the twenty-first iteration
        // produces F(22)'s predecessor; counting by hand is the only way to be
        // sure, and a first draft of this file said 6765 and was wrong.)
        expect: "10946\n",
        status: 10946,
    },
    Case {
        name: "a_string_built_byte_by_byte_and_measured",
        lazen_body: r#"
    let mut buffer: [u8; 8] = [0u8; 8];
    let word: &str = "abcdefgh";
    let mut at: u32 = 0u32;
    let bytes: &[u8] = word.as_bytes();
    let mut index: u64 = 0u64;
    while index < word.len() as u64 {
        buffer[at as usize] = bytes[index as usize];
        at = at + 1u32;
        index = index + 1u64;
    }
    show(at as i64);
    return at as i32;
"#,
        c_body: r#"
    char buffer[8];
    const char *word = "abcdefgh";
    unsigned at = 0, index = 0;
    while (index < 8) { buffer[at] = word[index]; at = at + 1; index = index + 1; }
    show((int)at);
    return (int)at;
"#,
        expect: "8\n",
        status: 8,
    },
    Case {
        name: "a_primality_test_over_a_range",
        lazen_body: r#"
    let mut found: i32 = 0i32;
    let mut candidate: i32 = 2i32;
    while candidate < 200i32 {
        if is_prime(candidate) {
            found = found + 1i32;
        }
        candidate = candidate + 1i32;
    }
    show(found as i64);
    return found;
"#,
        c_body: r#"
    int found = 0, candidate = 2;
    while (candidate < 200) {
        if (is_prime(candidate)) { found = found + 1; }
        candidate = candidate + 1;
    }
    show(found);
    return found;
"#,
        // There are 46 primes below 200.
        expect: "46\n",
        status: 46,
    },
    Case {
        name: "unsigned_wraparound",
        lazen_body: r#"
    let mut value: u32 = 4294967295u32;
    value = value + 1u32;
    show(value as i64);
    return value as i32;
"#,
        c_body: r#"
    unsigned int value = 4294967295u;
    value = value + 1u;
    show((int)value);
    return (int)value;
"#,
        // 0xFFFFFFFF + 1 wraps to zero, in both languages, at 32 bits.
        expect: "0\n",
        status: 0,
    },
    Case {
        name: "signed_division_truncating_toward_zero",
        lazen_body: r#"
    let a: i32 = -7i32;
    let b: i32 = 2i32;
    let quotient: i32 = a / b;
    show(quotient as i64);
    return quotient;
"#,
        c_body: r#"
    int a = -7, b = 2;
    int quotient = a / b;
    show(quotient);
    return quotient;
"#,
        expect: "-3\n",
        // A negative status is the low 32 bits of the value; the loader does not
        // sign-extend it and neither front end asks it to.
        status: 0xFFFF_FFFD,
    },
    Case {
        name: "a_signed_remainder_keeping_the_dividend_sign",
        lazen_body: r#"
    let a: i32 = -7i32;
    let b: i32 = 2i32;
    let rest: i32 = a % b;
    show(rest as i64);
    return rest;
"#,
        c_body: r#"
    int a = -7, b = 2;
    int rest = a % b;
    show(rest);
    return rest;
"#,
        // Truncating division gives -3, and the remainder takes the dividend's sign.
        expect: "-1\n",
        status: 0xFFFF_FFFF,
    },
    Case {
        name: "a_recursive_factorial",
        lazen_body: r#"
    let value: i32 = factorial(10i32);
    show(value as i64);
    return value;
"#,
        c_body: r#"
    int value = factorial(10);
    show(value);
    return value;
"#,
        // 10!
        expect: "3628800\n",
        status: 3628800,
    },
    Case {
        name: "short_circuit_evaluation",
        // The second operand must not be evaluated when the first decides the
        // answer, or the program divides by zero. A front end that evaluated both
        // would fault; one that inverted the test would also fault — so "it ran" is
        // itself the assertion, and the value proves the right arm was taken.
        lazen_body: r#"
    let zero: i32 = 0i32;
    let mut taken: i32 = 0i32;
    if zero != 0i32 && (100i32 / zero) > 0i32 {
        taken = 1i32;
    } else if zero == 0i32 || (100i32 / zero) > 0i32 {
        taken = 2i32;
    }
    show(taken as i64);
    return taken;
"#,
        c_body: r#"
    int zero = 0, taken = 0;
    if (zero != 0 && (100 / zero) > 0) { taken = 1; }
    else if (zero == 0 || (100 / zero) > 0) { taken = 2; }
    show(taken);
    return taken;
"#,
        expect: "2\n",
        status: 2,
    },
];

/// The Lazen program for a case: the printing helper, the helper it needs, and the
/// body.
fn lazen_program(case: &Case) -> String {
    let mut text = String::from(PRINT);
    if case.name == "a_primality_test_over_a_range" {
        text.push_str(
            r#"
fn is_prime(value: i32) -> bool {
    if value < 2i32 {
        return false;
    }
    let mut divisor: i32 = 2i32;
    while divisor * divisor <= value {
        if value % divisor == 0i32 {
            return false;
        }
        divisor = divisor + 1i32;
    }
    return true;
}
"#,
        );
    }
    if case.name == "a_recursive_factorial" {
        text.push_str(
            r#"
fn factorial(value: i32) -> i32 {
    if value <= 1i32 {
        return 1i32;
    }
    return value * factorial(value - 1i32);
}
"#,
        );
    }
    text.push_str("\nfn main() -> i32 {\n");
    text.push_str(case.lazen_body);
    text.push_str("\n}\n");
    text
}

/// The C program for a case.
fn c_program(case: &Case) -> String {
    let mut text = String::from(C_SHOW);
    if case.name == "a_primality_test_over_a_range" {
        text.push_str(
            r#"
static int is_prime(int value) {
    int divisor;
    if (value < 2) { return 0; }
    divisor = 2;
    while (divisor * divisor <= value) {
        if (value % divisor == 0) { return 0; }
        divisor = divisor + 1;
    }
    return 1;
}
"#,
        );
    }
    if case.name == "a_recursive_factorial" {
        text.push_str(
            r#"
static int factorial(int value) {
    if (value <= 1) { return 1; }
    return value * factorial(value - 1);
}
"#,
        );
    }
    text.push_str("\nint main(void) {\n");
    text.push_str(case.c_body);
    text.push_str("\n}\n");
    text
}

#[test]
fn the_two_front_ends_agree_on_every_algorithm() {
    let mut failures: Vec<String> = Vec::new();
    for case in CASES {
        let (lazen_output, lazen_status) = run_lazen(&lazen_program(case));
        let (c_output, c_status) = run_c(&c_program(case));
        if lazen_output != c_output || lazen_status != c_status {
            failures.push(format!(
                "{}: Lazen printed {lazen_output:?} and returned {lazen_status}, \
                 C printed {c_output:?} and returned {c_status}",
                case.name
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the two front ends disagree:\n{}",
        failures.join("\n")
    );
}

#[test]
fn both_front_ends_produce_the_hand_computed_answers() {
    // The half that matters more: agreement between two implementations of the same
    // specification is evidence, but a *shared* misreading would make both wrong
    // together. These values were computed by hand.
    let mut failures: Vec<String> = Vec::new();
    for case in CASES {
        let (lazen_output, lazen_status) = run_lazen(&lazen_program(case));
        if lazen_output != case.expect || lazen_status != case.status {
            failures.push(format!(
                "Lazen/{}: printed {lazen_output:?} and returned {lazen_status}, \
                 expected {:?} and {}",
                case.name, case.expect, case.status
            ));
        }
        let (c_output, c_status) = run_c(&c_program(case));
        if c_output != case.expect || c_status != case.status {
            failures.push(format!(
                "C/{}: printed {c_output:?} and returned {c_status}, expected {:?} and {}",
                case.name, case.expect, case.status
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a program disagrees with the hand-computed answer:\n{}",
        failures.join("\n")
    );
}
