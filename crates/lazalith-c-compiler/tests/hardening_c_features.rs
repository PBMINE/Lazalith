//! Hardening: a sweep of C features, each against a hand-written answer.
//!
//! Seven defects so far in this frontend, and every one was found by asking the C
//! compiler to do something and checking the number it produced. This file asks the
//! questions for the features the rest of the suite does not reach, and it was
//! written to be runnable as a whole: several of them turned out not to be supported,
//! and a sweep that only contains what works is a sweep that cannot report a gap.
//!
//! The answers are written **literally**, not computed in Rust. That is the one
//! departure from the convention this phase has otherwise followed, and it is
//! deliberate: a computed expectation here would be a second implementation of C's
//! semantics written by the same person testing the first, and would share its
//! mistakes. A literal can only be wrong by being *typed* wrong, which is visible.
//!
//! Each supported feature is its own `#[test]`, so a failure names a feature rather
//! than a line number. The features that are *not* supported are in their own module
//! at the end, as the record of what this file found about the language rather than
//! about the implementation.

use lazalith_types::ArchitectureConfig;

/// Builds, links and runs a C program, and reports what it printed and returned.
///
/// The prelude is the C runtime, so a case can use `print`, `print_decimal` and
/// friends without redeclaring them — which keeps each case to the feature under test
/// and nothing else.
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

/// Requires exactly `expected` printed and `expected_status` returned.
///
/// The output is compared whole, including its newline, so a program that computed
/// the right number and also said something else has still failed.
fn check(body: &str, expected: &str, expected_status: i32) {
    let (output, status) = run_c(body);
    assert_eq!(
        output, expected,
        "the program printed {output:?} and C says {expected:?}"
    );
    assert_eq!(
        status, expected_status as u32,
        "the program returned {status} and C says {expected_status}"
    );
}

#[test]
fn a_struct_has_the_size_its_members_imply() {
    // `int a` at 0, then `long b` has to start eight-aligned so there are four
    // padding bytes, then `short c` at 16, and the whole struct is padded up to a
    // multiple of its own alignment — so 8 + 8 + 2, rounded up to 24.
    //
    // The first draft of this asserted 16, having added the members and not the
    // padding. The program was right and the constant was wrong, which is the third
    // time in this phase that a hand-written expected value has been the thing that
    // was broken.
    check(
        r#"
struct S { int a; long b; short c; };
int main() {
    print_decimal((long) sizeof(struct S));
    print(" ");
    print_decimal((long) sizeof(short));
    print(" ");
    print_decimal((long) sizeof(long));
    print("\n");
    return 0;
}
"#,
        "24 2 8\n",
        0,
    );
}

#[test]
fn a_pointer_steps_by_the_size_of_what_it_points_at() {
    check(
        r#"
int main() {
    int values[5] = {10, 20, 30, 40, 50};
    int *at = values;
    print_decimal((long) at[0]);
    print(" ");
    print_decimal((long) *(at + 2));
    print(" ");
    print_decimal((long) at[4]);
    print(" ");
    print_decimal((long) *(at + 1 + 1));
    print(" ");
    print_decimal((long) (at + 1) - (long) at);
    print("\n");
    return 0;
}
"#,
        "10 30 50 30 4\n",
        0,
    );
}

#[test]
fn a_switch_falls_through_and_breaks() {
    // 1 and 2 share a body; 3 breaks; 0 and 4 take the default.
    check(
        r#"
int main() {
    int i;
    int total = 0;
    for (i = 0; i < 5; i = i + 1) {
        switch (i) {
            case 1:
            case 2: total = total + 10; break;
            case 3: total = total + 100; break;
            default: total = total + 1;
        }
    }
    print_decimal((long) total);
    print("\n");
    return 0;
}
"#,
        "122\n",
        0,
    );
}

#[test]
fn a_do_while_runs_at_least_once() {
    // The whole point of `do`/`while`: the body runs before the first test. The count is
    // four because the fourth subtraction takes `n` below zero before the test, and
    // the first draft asserted that `n` stopped at 1 — which is the value *after*
    // the third pass, and would have been the answer for a `while`.
    check(
        r#"
int main() {
    int n = 10;
    int count = 0;
    do {
        count = count + 1;
        n = n - 3;
    } while (n > 0);
    print_decimal((long) count);
    print(" ");
    print_decimal((long) n);
    print("\n");
    return 0;
}
"#,
        "4 -2\n",
        0,
    );
}

#[test]
fn break_and_continue_leave_the_right_loop() {
    // The inner `break` must not leave the outer loop, and `continue` must skip the
    // rest of the inner body *and* still run the inner increment. A `continue` that
    // jumped to the outer step would make this loop not terminate.
    check(
        r#"
int main() {
    int i;
    int j;
    int total = 0;
    for (i = 0; i < 4; i = i + 1) {
        for (j = 0; j < 4; j = j + 1) {
            if (j == 1) { continue; }
            if (j == 3) { break; }
            total = total + 1;
        }
        total = total + 100;
    }
    print_decimal((long) total);
    print("\n");
    return 0;
}
"#,
        "408\n",
        0,
    );
}

#[test]
fn a_global_is_initialised_once_and_keeps_its_value() {
    check(
        r#"
static int counter = 5;
static long wide = 7;
int bump(void) {
    counter = counter + 1;
    return counter;
}
int main() {
    print_decimal((long) bump());
    print(" ");
    print_decimal((long) bump());
    print(" ");
    print_decimal((long) counter);
    print(" ");
    print_decimal(wide);
    print("\n");
    return 0;
}
"#,
        "6 7 7 7\n",
        0,
    );
}

#[test]
fn a_string_is_an_array_of_bytes_with_a_terminator() {
    check(
        r#"
int main() {
    char text[6] = "hello";
    print_decimal((long) strlen(text));
    print(" ");
    print_decimal((long) text[0]);
    print(" ");
    print_decimal((long) text[5]);
    print(" ");
    if (strcmp(text, "hello") == 0) { print("eq"); } else { print("ne"); }
    print("\n");
    return 0;
}
"#,
        "5 104 0 eq\n",
        0,
    );
}

#[test]
fn sizeof_of_an_expression_does_not_evaluate_it() {
    // A `sizeof` of a call must not call it, or the side effect happens once more
    // than the program says it does. The first two numbers are what make this a test
    // of `sizeof` and the last is the control that `touch` *can* be called.
    check(
        r#"
static int calls = 0;
int touch(void) { calls = calls + 1; return 1; }
int main() {
    int size = (int) sizeof(touch());
    print_decimal((long) size);
    print(" ");
    print_decimal((long) calls);
    print(" ");
    touch();
    print_decimal((long) calls);
    print("\n");
    return 0;
}
"#,
        "4 0 1\n",
        0,
    );
}

#[test]
fn an_unsigned_comparison_is_not_a_signed_one() {
    // The comparison a signed `int` gets wrong: as an unsigned value -1 is the
    // *largest* number, not the smallest.
    check(
        r#"
int main() {
    unsigned int big = 4294967295u;
    int minus = -1;
    if (big > 1u) { print("gt "); } else { print("le "); }
    if ((unsigned int) minus > 1u) { print("ugt "); } else { print("ule "); }
    if (minus > 1) { print("sgt"); } else { print("sle"); }
    print("\n");
    return 0;
}
"#,
        "gt ugt sle\n",
        0,
    );
}

#[test]
fn a_shift_is_on_the_value_not_its_type() {
    // `1 << 31` is the sign bit at 32 bits and a perfectly ordinary 2147483648 in a
    // long, which is the distinction a shift defect shows up in.
    check(
        r#"
int main() {
    long wide = 1 << 31;
    unsigned int narrow = 1u << 31;
    print_decimal(wide);
    print(" ");
    print_decimal((long) narrow);
    print(" ");
    print_decimal(1024 >> 3);
    print(" ");
    print_decimal(-1024 >> 2);
    print("\n");
    return 0;
}
"#,
        "2147483648 2147483648 128 -256\n",
        0,
    );
}

#[test]
fn a_compound_assignment_applies_once_and_converts() {
    // Ten operators in a row, so a defect in any one of them is visible and the
    // numbers after it stay right — which is the property that makes the row worth
    // reading rather than the individual answers.
    check(
        r#"
int main() {
    int n = 10;
    n += 5;  print_decimal((long) n); print(" ");
    n -= 3;  print_decimal((long) n); print(" ");
    n *= 2;  print_decimal((long) n); print(" ");
    n /= 4;  print_decimal((long) n); print(" ");
    n %= 4;  print_decimal((long) n); print(" ");
    n <<= 4; print_decimal((long) n); print(" ");
    n >>= 2; print_decimal((long) n); print(" ");
    n |= 1;  print_decimal((long) n); print(" ");
    n &= 7;  print_decimal((long) n); print(" ");
    n ^= 15; print_decimal((long) n); print(" ");
    print("\n");
    return 0;
}
"#,
        "15 12 24 6 2 32 8 9 1 14 \n",
        0,
    );
}

#[test]
fn the_ternary_operator_evaluates_one_arm() {
    // A ternary that evaluated both arms would call `touch` twice per line, and the
    // count at the end is the whole test.
    check(
        r#"
static int calls = 0;
int touch(int v) { calls = calls + 1; return v; }
int main() {
    int chosen = 1 ? touch(10) : touch(20);
    int skipped = 0 ? touch(30) : touch(40);
    print_decimal((long) chosen);
    print(" ");
    print_decimal((long) skipped);
    print(" ");
    print_decimal((long) calls);
    print("\n");
    return 0;
}
"#,
        "10 40 2\n",
        0,
    );
}

/// A struct's members can be read, and its size is right, but they cannot be
/// *written through a pointer* — and a whole-struct assignment is the same gap.
///
/// A struct's members, read and written through a pointer.
///
/// This is here because of what the pointer-arithmetic defect looked like from the
/// outside. `p->a` is an addition by a byte offset from a pointer, and that addition
/// was not being scaled by the pointee — the *same* defect. So a record written when
/// the struct cases were first tried said "a write through a struct pointer is not
/// supported", and the record was wrong about the reason in a way that would have kept
/// the bug alive after it was fixed.
#[test]
fn a_struct_member_can_be_read_and_written_through_a_pointer() {
    check(
        r#"
struct S { int a; long b; short c; };
int main() {
    struct S value;
    struct S *at = &value;
    at->a = 11;
    at->b = 22;
    at->c = 33;
    print_decimal((long) at->a);
    print(" ");
    print_decimal((long) at->b);
    print(" ");
    print_decimal((long) at->c);
    print("\n");
    return 0;
}
"#,
        "11 22 33\n",
        0,
    );
}

/// Four things the sweep found that this frontend cannot do, all of them loudly.
///
/// They are recorded as tests rather than as prose because a limitation record that is
/// only written down goes stale, and this one went stale twice: an earlier version
/// claimed a write through a struct pointer was unsupported when `p->a` had worked all
/// along, and claimed a bare function-pointer *declaration* was unsupported when only
/// *calling through* one is. Both were found by probing rather than by reading, which
/// is the only reliable way to be right about what a compiler can do.
///
/// Each asserts the refusal across the whole pipeline rather than only the front end,
/// so the day a feature lands this file says so.
#[test]
fn four_c_features_are_still_refused() {
    for (what, body) in [
        (
            "a whole-struct assignment",
            r#"
struct S { int a; long b; };
int main() { struct S v; struct S w; v = w; return 0; }
"#,
        ),
        (
            // The *arrow* form works and the dereference form does not, which is the
            // whole of it: `(*p).a` types the place as the record rather than as the
            // field, so the store is a record-sized one. Same address, wrong width.
            "a member access through a dereference",
            r#"
struct S { int a; long b; short c; };
int main() { struct S v; struct S *p = &v; p->a = 7; print_decimal((long) (*p).a); print("\n"); return 0; }
"#,
        ),
        (
            "a call through a function pointer",
            r#"
int add(int a, int b) { return a + b; }
int main() { int (*at)(int, int); at = add; return at(1, 2); }
"#,
        ),
        (
            "reading an element of a two-dimensional array",
            r#"
int main() { int grid[2][3] = {{1, 2, 3}, {4, 5, 6}}; return (int) grid[1][2]; }
"#,
        ),
    ] {
        assert!(
            refuses(body).is_some(),
            "{what} now builds, so this limitation record is stale and the feature \
             belongs in the tests above"
        );
    }
}

/// Builds, lowers and generates `body`, and reports the first line of whatever refused
/// it — or `None` if nothing did.
fn refuses(body: &str) -> Option<String> {
    use lazalith_codegen::{CodegenOptions, generate};
    let mut unit = String::from(lazalith_c_runtime::C_RUNTIME);
    unit.push('\n');
    unit.push_str(body);
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = match lazalith_c_compiler::compile(&mut sources, "t.c", &unit) {
        Ok(compiled) => compiled,
        Err(error) => {
            return Some(
                error
                    .render()
                    .lines()
                    .nth(1)
                    .unwrap_or("")
                    .trim()
                    .to_owned(),
            );
        }
    };
    let lowered = match lazalith_c_compiler::lower(&checked) {
        Ok(lowered) => lowered,
        Err(error) => {
            return Some(error.to_string().lines().next().unwrap_or("").to_owned());
        }
    };
    match generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions::lz64("t.c"),
        &unit,
    ) {
        Ok(_) => None,
        Err(error) => Some(error.to_string().lines().next().unwrap_or("").to_owned()),
    }
}
