//! Type-check tests: one positive and one negative case per rule.
//!
//! Every rule in `docs/lazen-types.md` that the compiler enforces appears here
//! twice: once as a program that must compile, and once as a program that must
//! be rejected with a specific code. Where a rule is about a location, the
//! diagnostic's line and column are asserted too.

use lazalith_compiler::frontend::compile;
use lazalith_compiler::resolve::codes as resolve_codes;
use lazalith_compiler::types::ABI_SYSCALLS;
use lazalith_compiler::types::codes;
use lazalith_types::SourceManager;

fn accepts(source: &str) {
    let mut sources = SourceManager::new();
    if let Err(error) = compile(&mut sources, "t.lazen", source) {
        panic!(
            "expected this to type-check, but it failed with {}:\n{}",
            error.code().as_str(),
            error.render()
        );
    }
}

fn rejects_with(source: &str, code: &str) -> String {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "t.lazen", source) {
        Ok(_) => panic!("expected a rejection, but this compiled:\n{source}"),
        Err(error) => {
            let actual = error.code().as_str().to_string();
            assert_eq!(actual, code, "wrong diagnostic:\n{}", error.render());
            error.render()
        }
    }
}

/// The `(line, column)` a diagnostic's primary label points at.
fn label_position(rendered: &str) -> (u32, u32) {
    let marker = rendered
        .lines()
        .find(|line| line.trim_start().starts_with("-->"))
        .expect("a rendered diagnostic has a source marker");
    let fields: Vec<&str> = marker.split(':').collect();
    (
        fields[fields.len() - 2]
            .trim()
            .parse()
            .expect("a line number"),
        fields[fields.len() - 1]
            .trim()
            .parse()
            .expect("a column number"),
    )
}

// --------------------------------------------------------- integer literals

#[test]
fn an_unannotated_integer_literal_is_an_i32() {
    let mut sources = SourceManager::new();
    let (_, program) = compile(
        &mut sources,
        "t.lazen",
        "fn main() -> i32 { let n = 42; n }",
    )
    .expect("compiles");
    let local = &program.entry().expect("main").locals[0];
    assert_eq!(lazalith_compiler::types::ty_name(&local.ty), "i32");
}

#[test]
fn an_integer_literal_takes_the_type_of_its_context() {
    accepts("fn main() -> u8 { let n: u8 = 200; n }");
    accepts("fn main() -> i64 { let n: i64 = 9000000000; n }");
    accepts(
        "extern \"syscall\" fn close(h: u32) -> i64; fn main() -> i32 { let s = close(1); if s == 0 { 0 } else { 1 } }",
    );
}

#[test]
fn a_literal_that_does_not_fit_its_type_is_an_error() {
    let rendered = rejects_with(
        "fn main() -> u8 { let n: u8 = 256; n }",
        codes::LITERAL_RANGE,
    );
    assert!(rendered.contains("outside the range"), "{rendered}");
    rejects_with(
        "fn main() -> i8 { let n: i8 = 200; n }",
        codes::LITERAL_RANGE,
    );
    rejects_with(
        "fn main() -> u8 { let n: u8 = -1; n }",
        codes::LITERAL_RANGE,
    );
}

#[test]
fn a_literal_suffix_fixes_the_type() {
    accepts("fn main() -> u8 { 200u8 }");
    accepts("fn main() -> i64 { 0x1Fi64 }");
    // A literal suffix fixes the type, so a context that disagrees is an error.
    let rendered = rejects_with("fn main() -> u8 { let n: u8 = 200u16; n }", codes::MISMATCH);
    assert!(rendered.contains("suffix"), "{rendered}");
}

#[test]
fn hexadecimal_literals_work_in_both_bases() {
    accepts("fn main() -> i32 { 0x1F }");
    accepts("fn main() -> i32 { 0xff }");
    accepts("fn main() -> i32 { 31 }");
}

// ----------------------------------------------------------------- integers

#[test]
fn arithmetic_requires_matching_integer_types() {
    accepts("fn main() -> i32 { let a: i32 = 1; let b: i32 = 2; a + b }");
    let rendered = rejects_with(
        "fn main() -> i64 { let a: i32 = 1; let b: i64 = 2; a + b }",
        codes::MISMATCH,
    );
    assert!(rendered.contains("no implicit conversions"), "{rendered}");
    assert_eq!(label_position(&rendered), (1, 56));
}

#[test]
fn every_arithmetic_operator_is_checked() {
    for source in [
        "fn main() -> i32 { let a = 1; let b = 2; a + b }",
        "fn main() -> i32 { let a = 1; let b = 2; a - b }",
        "fn main() -> i32 { let a = 1; let b = 2; a * b }",
        "fn main() -> i32 { let a = 1; let b = 2; a / b }",
        "fn main() -> i32 { let a = 1; let b = 2; a % b }",
    ] {
        accepts(source);
    }
    // A non-integer cannot be an operand: v1 has no floats and no operator
    // overloading.
    for source in [
        "fn main() -> i32 { let a = 1; let b = \"x\"; a + b }",
        "fn main() -> i32 { let a = 1; let b = true; a * b }",
        "fn main() -> i32 { let a = 1; let b = [1]; a - b }",
    ] {
        rejects_with(source, codes::MISMATCH);
    }
}

#[test]
fn unsigned_and_signed_types_are_distinct() {
    rejects_with(
        "fn main() -> u32 { let a: u32 = 1; let b: i32 = 2; a + b }",
        codes::MISMATCH,
    );
}

#[test]
fn usize_is_distinct_from_every_fixed_width_integer() {
    accepts("fn main() -> usize { let n: usize = 1; n }");
    rejects_with(
        "fn main() -> u64 { let n: usize = 1; n + 1u64 }",
        codes::MISMATCH,
    );
    // An explicit cast between integer types is always allowed, so a `usize` and an
    // `i32` are distinct but convertible by name.
    accepts("fn main() -> u64 { let n: usize = 1; n as u64 }");
    accepts("fn main() -> i32 { let n: u64 = 1; n as i32 }");
}

#[test]
fn a_bool_is_never_an_integer() {
    accepts("fn main() -> bool { true }");
    rejects_with("fn main() -> i32 { true as i32 }", codes::INVALID_CAST);
    rejects_with(
        "fn main() -> i32 { let flag = true; flag + 1 }",
        codes::MISMATCH,
    );
}

// ------------------------------------------------------------- comparisons

#[test]
fn comparisons_yield_bool_and_compare_integers() {
    accepts("fn main() -> bool { 1 < 2 }");
    accepts("fn main() -> bool { 1u8 <= 2u8 }");
    accepts("fn main() -> bool { 1i64 != 2i64 }");
}

#[test]
fn a_comparison_needs_two_operands_of_the_same_integer_type() {
    rejects_with(
        "fn two() -> u8 { 2 } fn main() -> bool { 1 < two() }",
        codes::MISMATCH,
    );
    // A `str` is not an integer, and saying so is more useful than calling it a
    // bad range.
    let rendered = rejects_with("fn main() -> bool { \"a\" < \"b\" }", codes::MISMATCH);
    assert!(
        rendered.contains("comparison must be an integer"),
        "{rendered}"
    );
}

#[test]
fn a_comparison_result_is_a_bool_and_not_an_integer() {
    // Chaining comparisons is a type error, not a parse error.
    rejects_with("fn main() -> bool { 1 < 2 < 3 }", codes::MISMATCH);
}

#[test]
fn logical_operators_need_bool_operands() {
    accepts("fn main() -> bool { true && false }");
    accepts("fn main() -> bool { true || false }");
    // A literal has no context, so `1` is an `i32`, and a `bool` is what the
    // operator needs.
    let rendered = rejects_with("fn main() -> bool { 1 && true }", codes::CONDITION);
    assert!(rendered.contains("no truthiness"), "{rendered}");
}

#[test]
fn logical_not_needs_a_bool() {
    accepts("fn main() -> bool { !true }");
    rejects_with("fn main() -> bool { !1 }", codes::MISMATCH);
}

#[test]
fn negation_needs_an_integer() {
    accepts("fn main() -> i32 { -1 }");
    rejects_with(
        "fn flag() -> bool { true } fn main() -> i32 { -flag() }",
        codes::MISMATCH,
    );
    // `-x` wraps, so the most negative value is representable.
    accepts("fn main() -> i8 { -128i8 }");
}

// ------------------------------------------------------------------ mutability

#[test]
fn assignment_requires_mut() {
    accepts("fn main() -> i32 { let mut n = 1; n = 2; n }");
    let rendered = rejects_with("fn main() -> i32 { let n = 1; n = 2; n }", codes::IMMUTABLE);
    assert!(rendered.contains("declared here"), "{rendered}");
}

#[test]
fn a_mutable_binding_may_be_assigned_repeatedly() {
    accepts("fn main() -> i32 { let mut n = 1; n = 2; n = 3; n }");
}

#[test]
fn compound_assignment_is_expanded_and_checked() {
    accepts("fn main() -> i32 { let mut n = 1; n += 2; n }");
    rejects_with(
        "fn main() -> i32 { let n = 1; n += 2; n }",
        codes::IMMUTABLE,
    );
    rejects_with(
        "fn main() -> i32 { let mut n: i32 = 1; n += true; n }",
        codes::MISMATCH,
    );
}

// ------------------------------------------------------------------- calls

#[test]
fn a_call_checks_arity_and_every_argument() {
    accepts("fn f(a: i32, b: i32) -> i32 { a + b } fn main() -> i32 { f(1, 2) }");
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { f() }",
        codes::ARITY,
    );
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { f(1, 2) }",
        codes::ARITY,
    );
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { let b = true; f(b) }",
        codes::ARGUMENT,
    );
}

#[test]
fn a_call_result_is_the_callees_result() {
    accepts("fn f() -> i64 { 1 } fn main() -> i64 { f() }");
    rejects_with(
        "fn f() -> i64 { 1 } fn main() -> i32 { f() }",
        codes::MISMATCH,
    );
}

#[test]
fn a_local_is_not_callable() {
    rejects_with("fn main() -> i32 { let f = 1; f() }", codes::NOT_CALLABLE);
}

#[test]
fn a_parenthesised_name_is_still_a_name() {
    // Lazen has no function values, so `(f)()` is `f()`: the parentheses are
    // grouping, not a value.
    accepts("fn f() -> i32 { 0 } fn main() -> i32 { (f)() }");
}

#[test]
fn a_const_may_be_used_where_a_value_is_wanted() {
    accepts("const LIMIT: i32 = 4; fn main() -> i32 { LIMIT }");
    accepts("const NAME: &str = \"x\"; fn main() -> i32 { NAME.len() as i32 }");
}

#[test]
fn a_const_is_not_a_place() {
    rejects_with(
        "const LIMIT: i32 = 4; fn main() -> i32 { LIMIT = 5; 0 }",
        codes::NOT_A_PLACE,
    );
}

// ------------------------------------------------------------------ returns

#[test]
fn a_return_value_must_match_the_result_type() {
    accepts("fn f() -> i32 { return 1; }");
    let rendered = rejects_with(
        "fn flag() -> bool { true } fn f() -> i32 { return flag(); }",
        codes::MISMATCH,
    );
    assert!(rendered.contains("expected"), "{rendered}");
    // The literal takes the result type, so this is correct.
    accepts("fn f() -> i64 { return 1; }");
}

#[test]
fn a_bare_return_needs_a_unit_result() {
    accepts("fn f() { return; }");
    rejects_with("fn f() -> i32 { return; }", codes::RETURN_TYPE);
}

#[test]
fn a_function_must_be_able_to_produce_its_result() {
    accepts("fn f() -> i32 { 1 }");
    let rendered = rejects_with("fn f() -> i32 { }", codes::MISSING_RESULT);
    assert!(rendered.contains("end in a value"), "{rendered}");
    // A loop that never breaks never falls through, so it satisfies the rule.
    accepts("fn f() -> i32 { loop { } }");
    // A loop that *can* break can fall through, so it does not satisfy the rule.
    rejects_with("fn f() -> i32 { loop { break; } }", codes::MISSING_RESULT);
    rejects_with("fn f() -> i32 { let n = 1; }", codes::MISSING_RESULT);
}

#[test]
fn a_tail_expression_is_the_result() {
    accepts("fn f(a: i32) -> i32 { a * 2 }");
    rejects_with(
        "fn g() -> i64 { 1 } fn f(a: i32) -> i32 { g() }",
        codes::MISMATCH,
    );
}

// ------------------------------------------------------------------- control

#[test]
fn a_loop_needs_a_bool_condition() {
    accepts("fn main() -> i32 { let mut n = 0; while n < 3 { n = n + 1; } n }");
    rejects_with(
        "fn main() -> i32 { let mut n = 0; while n { n = 1; } n }",
        codes::CONDITION,
    );
}

#[test]
fn break_and_continue_belong_to_the_innermost_loop() {
    accepts(
        "fn main() -> i32 { let mut n = 0; while n < 3 { if n == 1 { break; } n = n + 1; } n }",
    );
    accepts("fn main() -> i32 { let mut n = 0; loop { n = n + 1; if n > 2 { break; } } n }");
    accepts(
        "fn main() -> i32 { let mut n = 0; loop { n = n + 1; if n > 2 { continue; } break; } n }",
    );
    rejects_with("fn main() -> i32 { break; 0 }", codes::OUTSIDE_LOOP);
    rejects_with("fn main() -> i32 { continue; 0 }", codes::OUTSIDE_LOOP);
    // A loop inside a loop: the inner `break` leaves the inner one.
    accepts(
        "fn main() -> i32 { let mut a = 0; loop { a = a + 1; loop { break; } if a > 2 { break; } } a }",
    );
}

#[test]
fn a_for_range_is_half_open_and_integer_only() {
    accepts("fn main() -> i32 { let mut t = 0; for v in 0..10 { t = t + v; } t }");
    accepts("fn main() -> i64 { let mut t: i64 = 0; for v in 0i64..10i64 { t = t + v; } t }");
    rejects_with(
        "fn main() -> i32 { let values = [1, 2]; for v in values { } 0 }",
        codes::NOT_A_RANGE,
    );
    rejects_with(
        "fn main() -> i32 { let s = \"ab\"; for c in s { } 0 }",
        codes::NOT_A_RANGE,
    );
}

#[test]
fn a_loop_variable_is_scoped_to_its_body() {
    accepts("fn main() -> i32 { for v in 0..3 { } 0 }");
    rejects_with(
        "fn main() -> i32 { for v in 0..3 { } v }",
        resolve_codes::UNRESOLVED,
    );
}

// ------------------------------------------------------------ arrays, slices

#[test]
fn an_array_literal_infers_one_element_type() {
    accepts("fn main() -> i32 { let values = [1, 2, 3]; values[0] }");
    accepts("fn main() -> u8 { let values = [1u8, 2, 3]; values[0] }");
    rejects_with(
        "fn main() -> i32 { let values = [1, true]; 0 }",
        codes::MISMATCH,
    );
    rejects_with(
        "fn main() -> i32 { let values = [1, \"two\"]; 0 }",
        codes::MISMATCH,
    );
}

#[test]
fn a_repeated_array_needs_a_concrete_element_type() {
    accepts("fn main() -> i32 { let mut scratch = [0u8; 16]; scratch[0] = 65; scratch[0] as i32 }");
    // Without a suffix and without a context, `0` is an `i32`, so this is an
    // array of `i32`, which is also fine.
    accepts("fn main() -> i32 { let mut values = [0; 4]; values[0] = 1; values[0] }");
    rejects_with(
        "fn main() -> i32 { let values = [0; 0]; 0 }",
        codes::ARRAY_TOO_LARGE,
    );
}

#[test]
fn an_array_length_must_fit_a_frame() {
    let rendered = rejects_with(
        "fn main() -> i32 { let values = [0u8; 4294967296]; 0 }",
        codes::ARRAY_TOO_LARGE,
    );
    assert!(rendered.contains("too large"), "{rendered}");
}

#[test]
fn indexing_produces_the_element_type() {
    accepts("fn main() -> u8 { let values = [1u8, 2]; values[1] }");
    accepts("fn main() -> i32 { let s = \"abc\"; s[0] as i32 }");
}

#[test]
fn an_index_must_be_usize() {
    let rendered = rejects_with(
        "fn main() -> i32 { let values = [1, 2]; let i = 0; values[i] }",
        codes::INDEX_TYPE,
    );
    assert!(rendered.contains("usize"), "{rendered}");
    // An explicit cast is how a program says so.
    accepts("fn main() -> i32 { let values = [1, 2]; let i = 0; values[i as usize] }");
}

#[test]
fn only_an_array_or_slice_can_be_indexed() {
    rejects_with("fn main() -> i32 { let n = 1; n[0] }", codes::MISMATCH);
    rejects_with(
        "fn main() -> i32 { let s = \"abc\"; s[0] = 1; 0 }",
        codes::IMMUTABLE,
    );
}

#[test]
fn an_immutable_slice_cannot_be_written_through() {
    rejects_with(
        "fn f(values: &[i32]) -> i32 { values[0] = 1; 0 }",
        codes::IMMUTABLE,
    );
    accepts("fn f(values: &mut [i32]) -> i32 { values[0] = 1; values[0] }");
}

#[test]
fn as_slice_and_as_mut_slice_need_the_right_mutability() {
    accepts("fn main() -> i32 { let values = [1, 2]; values.as_slice().len() as i32 }");
    accepts(
        "fn main() -> i32 { let mut values = [1, 2]; values.as_mut_slice()[0] = 1; values[0] }",
    );
    rejects_with(
        "fn main() -> i32 { let values = [1, 2]; values.as_mut_slice(); 0 }",
        codes::BAD_BORROW,
    );
}

#[test]
fn the_builtin_methods_are_the_only_methods() {
    accepts("fn main() -> usize { \"abc\".len() }");
    accepts("fn main() -> usize { \"abc\".as_bytes().len() }");
    let rendered = rejects_with(
        "fn main() -> i32 { \"abc\".capacity() as i32 }",
        codes::UNKNOWN_METHOD,
    );
    assert!(rendered.contains("valid methods"), "{rendered}");
    rejects_with("fn main() -> i32 { 1.len() as i32 }", codes::UNKNOWN_METHOD);
    rejects_with(
        "fn main() -> i32 { \"abc\".len(1) as i32 }",
        codes::UNKNOWN_METHOD,
    );
}

// ----------------------------------------------------------------- pointers

#[test]
fn a_reference_needs_a_place_and_the_right_mutability() {
    accepts("fn main() -> i32 { let mut n = 1; let r = &mut n; *r = 2; *r }");
    accepts("fn main() -> i32 { let n = 1; let r = &n; *r }");
    rejects_with(
        "fn main() -> i32 { let n = 1; let r = &mut n; 0 }",
        codes::BAD_BORROW,
    );
    rejects_with("fn main() -> i32 { let r = &1; 0 }", codes::NOT_A_PLACE);
}

#[test]
fn a_dereference_yields_the_referent() {
    accepts("fn f(n: i32) -> i32 { let r = &n; *r }");
    rejects_with(
        "fn flag() -> bool { true } fn f(n: i32) -> i32 { let r = &n; if flag() { *r } else { 0i64 } }",
        codes::MISMATCH,
    );
    accepts("fn f(n: i32) -> i32 { let r = &n; if true { *r } else { 0 } }");
}

#[test]
fn a_raw_pointer_is_only_a_value_to_pass_to_the_abi() {
    accepts("fn main() -> i32 { let mut n = 0u64; let p = &mut n as ptr<u64>; 0 }");
    accepts("fn main() -> i32 { let n = 0u64; let a = (&n as ptr<u64>) as u64; a as i32 }");
    // No dereference, because `ptr<T>` has no length to check.
    rejects_with(
        "fn main() -> i32 { let n = 0u64; let p = &n as ptr<u64>; let v = *p; v as i32 }",
        codes::RAW_DEREF,
    );
}

#[test]
fn a_cast_is_only_permitted_between_the_types_v1_allows() {
    // integer to integer
    accepts("fn main() -> u16 { 200u16 as u8 as u16 }");
    // A literal suffix fixes the type, so a context that disagrees is an error.
    rejects_with("fn main() -> u8 { let n: u8 = 200u16; n }", codes::MISMATCH);
    // reference to pointer, pointer to integer, integer to pointer
    accepts("fn main() -> i32 { let n = 0u32; (&n as ptr<u32>) as usize as i32 }");
    // the same type is always allowed
    accepts("fn main() -> i32 { let n = 1i32; n as i32 }");
    // and these are not
    rejects_with("fn main() -> i32 { true as i32 }", codes::INVALID_CAST);
    rejects_with(
        "fn main() -> i32 { let s = \"a\"; s as i32 }",
        codes::INVALID_CAST,
    );
    rejects_with(
        "fn main() -> i32 { let v = [1, 2]; v as i32 }",
        codes::INVALID_CAST,
    );
    rejects_with(
        "fn flag() -> bool { true } fn main() -> i32 { flag() as ptr<u8> as i32 as i32 }",
        codes::INVALID_CAST,
    );
}

// ------------------------------------------------------------- extern calls

#[test]
fn an_extern_must_name_an_os_syscall() {
    accepts("extern \"syscall\" fn close(handle: u32) -> i64;");
    let rendered = rejects_with(
        "extern \"syscall\" fn not_a_syscall(x: u32) -> i64;",
        codes::ABI_MISMATCH,
    );
    assert!(rendered.contains("OS ABI syscall"), "{rendered}");
}

#[test]
fn an_extern_may_not_declare_more_arguments_than_the_abi_has() {
    // `close` takes one argument in the shared ABI.
    let rendered = rejects_with(
        "extern \"syscall\" fn close(handle: u32, extra: u32) -> i64;",
        codes::ABI_MISMATCH,
    );
    assert!(rendered.contains("argument"), "{rendered}");
    // Fewer is allowed: the ABI fills its own registers.
    accepts("extern \"syscall\" fn close() -> i64;");
}

#[test]
fn a_call_through_an_extern_is_checked_like_any_other() {
    rejects_with(
        "extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { close(1, 2) }",
        codes::ARITY,
    );
    rejects_with(
        "extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { let h = true; close(h) }",
        codes::ARGUMENT,
    );
}

// ---------------------------------------------------------------- statements

#[test]
fn a_statement_whose_value_is_discarded_must_be_a_call() {
    accepts("extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { close(1); 0 }");
    let rendered = rejects_with("fn main() -> i32 { 1 + 2; 0 }", codes::DISCARDED_VALUE);
    assert!(rendered.contains("only a call"), "{rendered}");
    rejects_with(
        "fn main() -> i32 { let n = 1; n; 0 }",
        codes::DISCARDED_VALUE,
    );
    rejects_with("fn main() -> i32 { \"text\"; 0 }", codes::DISCARDED_VALUE);
}

#[test]
fn a_let_binds_the_value_it_is_given() {
    accepts("fn main() -> i32 { let n: i32 = 1; n }");
    rejects_with(
        "fn flag() -> bool { true } fn main() -> i32 { let n: i32 = flag(); n }",
        codes::MISMATCH,
    );
    rejects_with(
        "fn main() -> i32 { let n: i32 = \"x\"; n }",
        codes::MISMATCH,
    );
}

#[test]
fn let_underscore_evaluates_and_discards() {
    accepts(
        "extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { let _ = close(1); 0 }",
    );
    rejects_with(
        "extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { let mut _ = close(1); 0 }",
        codes::BAD_BORROW,
    );
    rejects_with(
        "fn main() -> i32 { let _ = 1; _ }",
        resolve_codes::UNRESOLVED,
    );
}

// ------------------------------------------------------------------- consts

#[test]
fn a_const_must_be_a_literal_of_its_declared_type() {
    accepts("const LIMIT: u32 = 10; fn main() -> i32 { LIMIT as i32 }");
    accepts("const LIMIT: i32 = 10; fn main() -> i32 { LIMIT }");
    let rendered = rejects_with(
        "const LIMIT: u32 = -1; fn main() -> i32 { 0 }",
        codes::LITERAL_RANGE,
    );
    assert!(rendered.contains("does not fit"), "{rendered}");
    rejects_with(
        "const LIMIT: u32 = \"x\"; fn main() -> i32 { 0 }",
        codes::MISMATCH,
    );
}

#[test]
fn a_const_cannot_be_computed_from_another_call() {
    rejects_with(
        "fn f() -> i32 { 1 } const LIMIT: i32 = f(); fn main() -> i32 { 0 }",
        codes::NOT_CONSTANT,
    );
}

#[test]
fn the_syscall_name_table_covers_the_shared_abi_exactly() {
    // A Lazen `extern` declaration names a syscall by its source name, so this
    // table has to name every syscall the shared ABI has. A new ABI syscall
    // without a name here would make `extern` declarations for it unresolvable,
    // so the test fails instead.
    use lazalith_os_abi::Syscall;
    let mut named: Vec<Syscall> = ABI_SYSCALLS.iter().map(|(_, syscall)| *syscall).collect();
    let mut expected: Vec<Syscall> = Syscall::ALL.to_vec();
    named.sort_by_key(|syscall| syscall.as_u16());
    named.dedup();
    expected.sort_by_key(|syscall| syscall.as_u16());
    assert_eq!(
        named, expected,
        "every `Syscall` variant needs a source name in ABI_SYSCALLS"
    );
    // Names are unique, so one name cannot map to two calls.
    let mut names: Vec<&str> = ABI_SYSCALLS.iter().map(|(name, _)| *name).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    assert_eq!(names.len(), count, "two syscalls share one source name");
}

#[test]
fn a_reserved_design_syscall_is_accepted_without_a_number() {
    // The graphics and input documents specify these calls; the ABI does not
    // number them yet, so the frontend must not invent one.
    for name in lazalith_compiler::types::RESERVED_DESIGN_SYSCALLS {
        let source = format!("extern \"syscall\" fn {name}() -> i64;");
        let mut sources = SourceManager::new();
        let (_, program) = compile(&mut sources, "t.lazen", &source)
            .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
        let declaration = &program.externs[0];
        assert_eq!(declaration.name, *name);
        assert!(
            declaration.syscall.is_none(),
            "a reserved call must carry no number"
        );
    }
}
