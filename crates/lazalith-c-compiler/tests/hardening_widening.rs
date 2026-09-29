//! Hardening: does the C frontend sign-extend when it widens?
//!
//! An `int` is 32 bits and a `long` is 64 on this platform, so moving a negative
//! `int` into a `long` has to *extend* it, not zero-fill it. Getting that wrong is
//! the quietest kind of C bug there is: the program runs, prints a plausible number,
//! and every value is wrong by 2^32.
//!
//! This file asks the question the smallest way it can be asked, four times over, so
//! that a fix cannot pass by correcting one of them.

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
        lowered.entry.as_deref(),
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
    let bytes = linked
        .image()
        .to_bytes()
        .unwrap_or_else(|error| panic!("the image should serialise: {error}"));
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

const SHOW: &str = r#"
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

#[test]
fn a_negative_int_assigned_to_a_long_stays_negative() {
    let source = format!("{SHOW}\nint main(void) {{ long value = -3; show(value); return 0; }}\n");
    let (output, _) = run_c(&source);
    assert_eq!(
        output, "-3\n",
        "a negative int widened to a long must keep its sign"
    );
}

#[test]
fn a_negative_int_passed_to_a_long_parameter_stays_negative() {
    let source = format!(
        "{SHOW}\nstatic void take(long value) {{ show(value); }}\n\
         int main(void) {{ take(-3); return 0; }}\n"
    );
    let (output, _) = run_c(&source);
    assert_eq!(
        output, "-3\n",
        "a negative int passed to a long parameter must keep its sign"
    );
}

#[test]
fn a_negative_int_compared_against_zero_is_negative() {
    // The same defect seen through a comparison rather than through a print, so a
    // fix that only repaired the printing path would not pass this.
    let source = "int main(void) { int value = -3; long widened = value; \
                  if (widened < 0) { return 1; } return 0; }\n"
        .to_string();
    let (_, status) = run_c(&source);
    assert_eq!(
        status, 1,
        "a widened negative int must still be less than zero"
    );
}

#[test]
fn a_negative_int_widened_and_halved_is_still_negative() {
    // And through arithmetic, which is where a zero-fill would be visible as a sign
    // flip: -3 / 2 is -1 as a signed division and 2147483645 as an unsigned one.
    let source = format!(
        "{SHOW}\nint main(void) {{ int value = -3; long half = value / 2; \
         show(half); return 0; }}\n"
    );
    let (output, _) = run_c(&source);
    assert_eq!(
        output, "-1\n",
        "signed division must happen at the widened type, where -3/2 is -1"
    );
}

#[test]
fn an_unsigned_int_widened_to_a_long_does_not_become_negative() {
    // The other direction, so a fix cannot simply "make everything negative": an
    // `unsigned int` whose top bit is set is a large positive number, and widening
    // it must not borrow a sign it never had.
    let source = format!(
        "{SHOW}\nint main(void) {{ unsigned int value = 4294967293u; \
         long widened = value; show(widened); return 0; }}\n"
    );
    let (output, _) = run_c(&source);
    assert_eq!(
        output, "4294967293\n",
        "an unsigned int with the top bit set is a large positive number"
    );
}
