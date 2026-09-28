//! Hardening: the Lazen frontend's operator precedence, scoping and shadowing.
//!
//! Precedence and scoping are the two places a compiler produces a *plausible
//! wrong answer* rather than a diagnostic, because the program still type-checks and
//! still runs. A `<<` that binds tighter than `+` does not fail; it computes a
//! different number. A shadowed name that leaks does not fail; it reads the wrong
//! variable.
//!
//! So every case here is a program whose *only* interesting content is how the
//! expression or the name resolved, and whose answer is compared against the answer
//! the operator table in `docs/lazen-syntax.md` says. Where the intent is "these
//! associate the same way", the test says so; where it is "this is left-associative",
//! the test pins which answer that is, because a compiler that changed it would be a
//! compiler that changed every program using it.

use lazalith_types::ArchitectureConfig;

fn run(source: &str) -> u32 {
    use lazalith_devices::{DeviceManager, NoDevice};
    let options = lazalith_runtime::BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("parse.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build:\n{error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"));
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "",
        "the program prints nothing"
    );
    finished.exit_code
}

/// A program that returns one expression, and the value it must produce.
fn value_of(body: &str) -> u32 {
    let source = format!("fn main() -> i32 {{ return {body}; }}\n");
    run(&source)
}

#[test]
fn multiplication_binds_tighter_than_addition() {
    assert_eq!(value_of("1i32 + 2i32 * 3i32"), 7, "1 + (2 * 3)");
    assert_eq!(value_of("2i32 * 3i32 + 1i32"), 7, "(2 * 3) + 1");
    // The distributive reading gives 9 and 10, so both are pinned.
    assert_ne!(value_of("1i32 + 2i32 * 3i32"), 9);
    assert_ne!(value_of("2i32 * 3i32 + 1i32"), 10);
}

#[test]
fn parenthesised_subtraction_binds_tighter_than_a_comparison() {
    assert_eq!(value_of("if 1i32 - 1i32 < 1i32 { 1i32 } else { 0i32 }"), 1);
    // Read as `1 - (1 < 1)` this would be a type error, so the fact that it
    // compiles is itself part of the claim; the value confirms the reading.
    assert_eq!(
        value_of("if (1i32 - 1i32) < 1i32 { 10i32 } else { 20i32 }"),
        10
    );
}

#[test]
fn logical_and_binds_tighter_than_logical_or() {
    // The values have to be chosen so the two readings actually *differ*, and the
    // first draft of this test chose `false, true, false` — where both readings
    // agree, so it would have passed a compiler with either precedence. The
    // distinguishing case is a true left operand with a false right one: `a || (b
    // && c)` is true and `(a || b) && c` is false.
    let source = r#"
fn main() -> i32 {
    let a: bool = true;
    let b: bool = false;
    let c: bool = false;
    if a || b && c { return 1i32; }
    return 0i32;
}
"#;
    assert_eq!(
        run(source),
        1,
        "`&&` binds tighter, so this is `a || (b && c)`, which is true"
    );
    let other = r#"
fn main() -> i32 {
    let a: bool = true;
    let b: bool = false;
    let c: bool = false;
    if (a || b) && c { return 1i32; }
    return 0i32;
}
"#;
    assert_eq!(
        run(other),
        0,
        "and the parenthesised reading is false, so the two are told apart"
    );
}

#[test]
fn addition_is_left_associative() {
    // `1 - 2 - 3` is `(1 - 2) - 3` = -4, not `1 - (2 - 3)` = 2. A compiler that
    // got this wrong would change the meaning of every subtraction chain.
    assert_eq!(value_of("1i32 - 2i32 - 3i32"), (-4i32) as u32);
    assert_ne!(value_of("1i32 - 2i32 - 3i32"), 2);
    assert_eq!(
        value_of("8i32 / 2i32 / 2i32"),
        2,
        "division is left-associative too"
    );
    assert_eq!(value_of("8i32 - 2i32 + 2i32"), 8);
}

#[test]
fn a_unary_minus_applies_before_a_binary_operator() {
    assert_eq!(value_of("0i32 - -3i32"), 3, "a unary minus binds tightest");
    assert_eq!(value_of("-3i32 * 2i32"), (-6i32) as u32);
    // A signed overflow is a wrapping one at 32 bits, and this pins that the
    // negation happens at the width the literal has.
    assert_eq!(value_of("-2147483648i32 - 1i32"), 2_147_483_647u32);
}

#[test]
fn a_comparison_binds_looser_than_arithmetic_and_returns_a_bool() {
    let source = r#"
fn main() -> i32 {
    if 1i32 + 1i32 == 2i32 { return 1i32; }
    return 0i32;
}
"#;
    assert_eq!(run(source), 1, "`+` binds tighter than `==`");
}

#[test]
fn a_name_shadowed_in_an_inner_block_does_not_leak_out() {
    // The classic scope-leak question. If the inner `let` escaped, the outer read
    // would find 99 and the answer would be 99 rather than the outer 1.
    let source = r#"
fn main() -> i32 {
    let value: i32 = 1i32;
    {
        let value: i32 = 99i32;
        if value != 99i32 { return 100i32; }
    }
    return value;
}
"#;
    assert_eq!(
        run(source),
        1,
        "the inner binding must not survive its block"
    );
}

#[test]
fn a_name_shadowed_in_an_inner_block_shadows_for_that_block() {
    // The other half: the inner read must find the *inner* binding, not the outer.
    let source = r#"
fn main() -> i32 {
    let value: i32 = 1i32;
    let mut seen: i32 = 0i32;
    {
        let value: i32 = 42i32;
        seen = value;
    }
    return seen * 100i32 + value;
}
"#;
    assert_eq!(run(source), 4201, "42 inside the block, 1 outside");
}

#[test]
fn a_loop_body_rebinds_each_iteration_rather_than_accumulating() {
    // A `let` inside a loop body is a fresh binding per iteration. If it were hoisted,
    // the second iteration would see the first iteration's value, and this program
    // would double-count.
    let source = r#"
fn main() -> i32 {
    let mut total: i32 = 0i32;
    let mut i: i32 = 0i32;
    while i < 5i32 {
        let doubled: i32 = i * 2i32;
        total = total + doubled;
        i = i + 1i32;
    }
    return total;
}
"#;
    // 0 + 2 + 4 + 6 + 8
    assert_eq!(run(source), 20, "each iteration has its own `doubled`");
}

#[test]
fn a_function_parameter_shadows_a_global_of_the_same_name() {
    // A name resolved from the wrong scope is a wrong answer with no diagnostic, and
    // this is the shape most likely to appear in real code: a helper whose parameter
    // shares a name with a file-scope value.
    let source = r#"
fn scale(factor: i32) -> i32 {
    return factor * 10i32;
}

fn main() -> i32 {
    return scale(3i32) + 1i32;
}
"#;
    assert_eq!(
        run(source),
        31,
        "the parameter is the `factor` that matters"
    );
}

#[test]
fn a_cast_applies_to_the_expression_it_is_written_around() {
    // The value has to be one that *actually* truncates. The first draft used 300,
    // which fits in an `i32` perfectly well — so the test could not tell a cast from
    // no cast, and its "300 truncates to 44" comment was simply wrong. 2^32 + 1
    // truncates to 1 at 32 bits, which no other reading of the expression produces.
    assert_eq!(
        value_of("4294967297i64 as i32 + 1i32"),
        2,
        "the cast takes 2^32 + 1 to 1, and the 1 is added after"
    );
    assert_eq!(
        value_of("4294967297i64 as i32 + 1i32"),
        value_of("(4294967297i64 as i32) + 1i32"),
        "and an explicit parenthesis means the same thing"
    );
    assert_ne!(
        value_of("4294967297i64 as i32 + 1i32"),
        value_of("4294967297i64 as i32"),
        "so the cast is not `(4294967297 + 1)` truncated"
    );
}

#[test]
fn an_index_expression_is_evaluated_before_the_element_is_read() {
    // A subscript whose index has a side effect must run the index, once, for the
    // right element. An implementation that folded the base or the index would
    // read a different element or run the index a different number of times.
    let source = r#"
fn main() -> i32 {
    let mut values: [i32; 4] = [10i32, 20i32, 30i32, 40i32];
    let mut index: i32 = 0i32;
    let mut total: i32 = 0i32;
    let mut round: i32 = 0i32;
    while round < 3i32 {
        total = total + values[index as usize];
        index = index + 1i32;
        round = round + 1i32;
    }
    return total;
}
"#;
    assert_eq!(run(source), 60, "10 + 20 + 30");
}
