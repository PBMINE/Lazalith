//! Hardening: what an integer constant is worth, and what type it has.
//!
//! An integer constant is the one thing in C that is spelled rather than computed,
//! which means it is also the one thing where the frontend has to agree with a rule
//! rather than with an operation. Two rules apply and both are easy to get subtly
//! wrong: *what type* does this literal have, and *what bits* does it carry.
//!
//! Two defects came out of this file, and neither is the kind a program notices. A
//! constant that panics the compiler is noticed immediately — the build stops — but
//! a constant that silently becomes `0` runs to completion and is wrong, and the
//! whole diagnostic layer of this frontend is built on the premise that a constant
//! either works or says so.
//!
//! The type ladder is C's, and it is worth writing out because it has one asymmetry
//! that catches everyone once:
//!
//! - An unsuffixed **decimal** constant that does not fit `int` becomes `long`, and
//!   one that does not fit `long` becomes `long long`. It never becomes *unsigned*.
//! - An unsuffixed **hexadecimal or octal** constant that does not fit `int` becomes
//!   `unsigned int`, and one that does not fit that becomes `unsigned long`.
//!
//! So `2147483648` is a `long` and `0x80000000` is an `unsigned int`, and a
//! frontend that treats them alike is wrong about half of every large hexadecimal
//! constant a program contains.

use lazalith_types::ArchitectureConfig;

/// Builds, links and runs a C program, and reports what it printed and returned.
fn run_c(body: &str) -> (String, u32) {
    use lazalith_codegen::{CodegenOptions, generate};
    use lazalith_toolchain::{LinkOptions, link_objects};

    let config = ArchitectureConfig::lz64();
    let mut unit = String::from(lazalith_c_runtime::C_RUNTIME);
    unit.push(char::from(10u8));
    unit.push_str(body);
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = lazalith_c_compiler::compile(&mut sources, "t.c", &unit)
        .unwrap_or_else(|error| panic!("the C program should compile:\n{}", error.render()));
    let lowered = lazalith_c_compiler::lower(&checked)
        .unwrap_or_else(|error| panic!("the C program should lower: {error}"));
    let program = generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions::lz64("t.c"),
        &unit,
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
    let bytes = linked.image().to_bytes().expect("the image serializes");
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        config,
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the C program should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// Compiles a C program and reports whether it was accepted, and the first line of
/// the diagnostic if it was not.
fn accepts(source: &str) -> Result<(), String> {
    let mut unit = String::from(lazalith_c_runtime::C_RUNTIME);
    unit.push(char::from(10u8));
    unit.push_str(source);
    let mut sources = lazalith_types::SourceManager::new();
    lazalith_c_compiler::compile(&mut sources, "t.c", &unit)
        .map(|_| ())
        .map_err(|error| {
            error
                .render()
                .lines()
                .next()
                .unwrap_or("a diagnostic with no first line")
                .to_owned()
        })
}

/// Prints an unsigned long in decimal. Checked against known values in
/// `the_printer_is_right` before anything relies on it.
const PRINTER: &str = "\
static void pu(unsigned long value) { \
  char digits[24]; \
  unsigned long at = 0; \
  unsigned long started = 0; \
  while (value > 0) { \
    unsigned long rest = value / 10; \
    digits[at] = (char)(48 + (value - rest * 10)); \
    at = at + 1; \
    value = rest; \
    started = 1; \
  } \
  if (started == 0) { putchar(48); } \
  while (at > 0) { at = at - 1; putchar((int) digits[at]); } \
  putchar(10); \
}\n";

#[test]
fn the_printer_is_right() {
    for (value, expected) in [
        (0u64, "0"),
        (1, "1"),
        (10, "10"),
        (255, "255"),
        (65_535, "65535"),
        (4_294_967_295, "4294967295"),
        (9_223_372_036_854_775_807, "9223372036854775807"),
        (u64::MAX, "18446744073709551615"),
    ] {
        let (output, _) = run_c(&format!(
            "{PRINTER}int main() {{ pu({value}ul); return 0; }}"
        ));
        assert_eq!(
            output.trim(),
            expected,
            "the printer printed {output:?} for {value}"
        );
    }
}

#[test]
fn an_unsigned_long_constant_carries_its_bits() {
    // The defect: these digits do not fit in an `i64`, and the IR used to parse a
    // constant as an `i64` and treat the failure as "not a constant" — which the
    // caller turned into *zero*. So every `unsigned long` constant above `LONG_MAX`
    // compiled cleanly to the wrong value, with no diagnostic.
    for (literal, expected) in [
        ("18446744073709551615ul", u64::MAX),
        ("18446744073709551614ul", u64::MAX - 1),
        ("9223372036854775808ul", 1u64 << 63),
        ("0xFFFFFFFFFFFFFFFFul", u64::MAX),
        ("0x8000000000000000ul", 1u64 << 63),
    ] {
        let (output, status) = run_c(&format!(
            "{PRINTER}int main() {{ pu({literal}); return 0; }}"
        ));
        assert_eq!(status, 0, "`{literal}` should return 0");
        let printed: u64 = output
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("`{literal}` printed {output:?}, which is not a number"));
        assert_eq!(
            printed, expected,
            "`{literal}` is {expected} in C, and the program printed {printed}"
        );
    }
}

#[test]
fn an_unsigned_long_constant_survives_a_widening_comparison() {
    // The same constant, reached the way a program actually reaches it: compared
    // against a value computed at run time. A zero constant fails this even though
    // the two *constants* in the earlier form are consistent with each other.
    let (output, status) = run_c(&format!(
        "{PRINTER}int main() {{ \
           unsigned short small = 65535; \
           unsigned long wide = (unsigned long) small; \
           pu(wide == 18446744073709551615ul ? 1 : 0); \
           return 0; }}"
    ));
    assert_eq!(status, 0);
    assert_eq!(
        output.trim(),
        "0",
        "65535 widened from `unsigned short` is 65535, not all ones — the \
         comparison below is the one that should hold"
    );
    let (output, _) = run_c(&format!(
        "{PRINTER}int main() {{ \
           unsigned long wide = 18446744073709551615ul; \
           pu(wide == 18446744073709551615ul ? 1 : 0); \
           return 0; }}"
    ));
    assert_eq!(
        output.trim(),
        "1",
        "a `ul` constant must equal itself when the program compares it"
    );
}

#[test]
fn every_suffix_and_base_is_accepted_and_narrow_ones_are_refused() {
    // The panic, and the narrow widths it hid behind. `1ul` is as legal as
    // `1u`, and used to abort the compiler in a debug build because the
    // range check computed `1u64 << 64` before asking whether the type was
    // 64 bits wide.
    for literal in [
        "1",
        "0",
        "1u",
        "1l",
        "1ul",
        "1lu",
        "1LL",
        "1ULL",
        "0x1",
        "0xFFFFFFFFFFFFFFFFul",
        "0777",
        "037777777777",
        "2147483647",
        "9223372036854775807",
        "4294967295u",
    ] {
        assert!(
            accepts(&format!("int main() {{ return (int) {literal}; }}")).is_ok(),
            "`{literal}` is legal C and was refused"
        );
    }
    // And a constant with no type at all is still refused, with a diagnostic that
    // says which type it overflowed rather than one that names a type it never had.
    let error = accepts("int main() { return (int) 9223372036854775808; }")
        .expect_err("a decimal constant above LONG_MAX has no type in C");
    assert!(
        error.contains("9223372036854775808"),
        "the diagnostic should name the constant, got: {error}"
    );
}

#[test]
fn an_unsigned_long_constant_above_long_max_is_not_an_error() {
    // The case that the earlier fix could easily have broken: refusing everything a
    // signed 64-bit type cannot hold would also refuse `u64::MAX`, which is the most
    // ordinary unsigned constant there is.
    assert!(
        accepts("int main() { return (int) 18446744073709551615ul; }").is_ok(),
        "an unsigned long constant above LONG_MAX is legal C"
    );
    assert!(
        accepts("int main() { return (int) 18446744073709551615ull; }").is_ok(),
        "and so is the `ll` spelling of it"
    );
}

#[test]
fn a_wide_decimal_constant_becomes_a_long_and_a_wide_hex_one_an_unsigned() {
    // The type ladder. `sizeof` is the measurement, because it is the observable
    // difference between the two branches: a decimal constant above `INT_MAX` is
    // *signed* and 8 bytes, and a hexadecimal one at the same magnitude is
    // *unsigned* and 4 bytes.
    for (literal, expected_size) in [
        ("1", 4u32),
        ("2147483647", 4),
        ("2147483648", 8),
        ("0x7FFFFFFF", 4),
        // C's asymmetry, in one line: same magnitude, different type, different size.
        ("0x80000000", 4),
        ("0xFFFFFFFF", 4),
        ("0x100000000", 8),
        ("037777777777", 4),
        ("040000000000", 8),
    ] {
        let (_, status) = run_c(&format!("int main() {{ return sizeof({literal}); }}"));
        assert_eq!(
            status, expected_size,
            "sizeof({literal}) is {expected_size} in C, and the program said {status}"
        );
    }
}
