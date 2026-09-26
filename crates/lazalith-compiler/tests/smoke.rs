//! End-to-end tests for the frontend as a whole.
//!
//! `documented_examples.rs` pins the compiler to the programs in
//! `docs/lazen-syntax.md`. This file covers the pipeline itself: the entry
//! point, the shape of a checked program, the target-dependent frame layout,
//! and the promise that no input can make the compiler panic.

use lazalith_compiler::frontend::compile;
use lazalith_compiler::resolve::resolve_only_for_tests;
use lazalith_compiler::types::{Target, Type, check_for};
use lazalith_types::{SourceManager, WordWidth};

fn compile_ok(source: &str) -> lazalith_compiler::types::CheckedProgram {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "test.lazen", source) {
        Ok((_, program)) => program,
        Err(error) => panic!(
            "expected this program to compile, but it failed with {}:\n{}",
            error.code().as_str(),
            error.render()
        ),
    }
}

#[test]
fn the_frontend_returns_a_typed_program_with_no_instructions() {
    let program = compile_ok(
        r#"
extern "syscall" fn write(fd: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    let message = "Hello, Lazalith\n";
    let bytes = message.as_bytes();
    write(1, bytes, message.len() as u64, bytes.as_ptr());
    0
}
"#,
    );
    // The interned string is the only literal in the program.
    assert_eq!(program.strings.len(), 1);
    assert_eq!(program.strings[0].text, "Hello, Lazalith\n");
    // One extern, mapped to a real syscall number.
    assert_eq!(program.externs.len(), 1);
    assert_eq!(program.externs[0].name, "write");
    assert!(program.externs[0].syscall.is_some());
    // The call is recorded as an extern call with its four arguments.
    let lazalith_compiler::types::CheckedStmt::Expression { expression, .. } =
        &program.entry().expect("a main function").body.statements[2]
    else {
        panic!("expected a call statement");
    };
    let lazalith_compiler::types::CheckedExpr::Call {
        callee,
        is_extern,
        arguments,
        ..
    } = &**expression
    else {
        panic!("expected a call");
    };
    assert_eq!(callee, "write");
    assert!(is_extern);
    assert_eq!(arguments.len(), 4);
}

#[test]
fn every_local_gets_a_frame_slot_and_an_offset() {
    let program = compile_ok(
        r#"
fn add(a: i32, b: i64) -> i64 {
    let mut total: i64 = 0;
    total = total + b;
    let flag: bool = true;
    if flag {
        total = total + a as i64;
    }
    total
}

fn main() -> i32 {
    add(1, 2) as i32
}
"#,
    );
    let add = program.function("add").expect("an add function");
    let slots: Vec<(&str, u32, u32)> = add
        .locals
        .iter()
        .map(|local| (local.name.as_str(), local.slot, local.offset))
        .collect();
    // Slots are handed out in declaration order; offsets follow the alignment
    // rule: `a` is four bytes, then `b` needs eight-byte alignment.
    assert_eq!(
        slots,
        vec![("a", 0, 0), ("b", 1, 8), ("total", 2, 16), ("flag", 3, 24)]
    );
    // The frame is rounded up to the machine word.
    assert_eq!(add.frame_size, 32);
    assert_eq!(add.result, Type::I64);
}

#[test]
fn the_frame_layout_depends_on_the_target_word() {
    let source = "fn f(n: usize) -> usize { n } fn main() -> i32 { f(1) as i32 }";
    let mut sources = SourceManager::new();
    let source_id = sources.add_file("t.lazen", source).expect("a source");
    let program = lazalith_compiler::parse_file(source_id, &sources).expect("parses");
    let resolved = resolve_only_for_tests(source_id, &sources, program).expect("resolves");

    let wide = check_for(
        source_id,
        &sources,
        &resolved,
        Target {
            word: WordWidth::W64,
        },
    )
    .expect("checks");
    let narrow = check_for(
        source_id,
        &sources,
        &resolved,
        Target {
            word: WordWidth::W32,
        },
    )
    .expect("checks");
    let wide_f = wide.function("f").expect("f");
    let narrow_f = narrow.function("f").expect("f");
    // A `usize` is one word, so the parameter and the frame differ by target.
    assert_eq!(wide_f.parameters[0].ty.size_in_bytes(wide.word), 8);
    assert_eq!(narrow_f.parameters[0].ty.size_in_bytes(narrow.word), 4);
    assert_eq!(wide_f.frame_size, 8);
    assert_eq!(narrow_f.frame_size, 4);
}

#[test]
fn an_empty_program_is_valid_and_contains_nothing() {
    let program = compile_ok("// nothing but a comment\n");
    assert!(program.functions.is_empty());
    assert!(program.externs.is_empty());
    assert!(program.constants.is_empty());
    assert!(program.strings.is_empty());
    assert!(program.entry().is_none());
}

#[test]
fn malformed_programs_never_panic() {
    // Every one of these is a prefix, a truncation, or a stray delimiter. The
    // requirement is a diagnostic, not a crash.
    let cases = [
        "",
        "fn",
        "fn f(",
        "fn f() -> {",
        "fn f() -> i32 {",
        "fn f() -> i32 { let",
        "fn f() -> i32 { let x",
        "fn f() -> i32 { let x =",
        "fn f() -> i32 { let x = 1",
        "fn f() -> i32 { let x = [1, 2",
        "fn f() -> i32 { let x = (1",
        "fn f() -> i32 { let x = \"unterminated",
        "fn f() -> i32 { if",
        "fn f() -> i32 { if x",
        "fn f() -> i32 { if x {",
        "fn f() -> i32 { while",
        "fn f() -> i32 { for",
        "fn f() -> i32 { for x in",
        "fn f() -> i32 { match",
        "fn f() -> i32 { x[",
        "fn f() -> i32 { x[1",
        "fn f() -> i32 { x.",
        "fn f() -> i32 { x as",
        "fn f() -> i32 { x as ptr<",
        "fn f() -> i32 { x as ptr<u8",
        "mod",
        "mod m {",
        "mod m { fn",
        "extern",
        "extern \"",
        "extern \"syscall\"",
        "extern \"syscall\" fn",
        "const",
        "use",
        "use a::",
        "}",
        ")",
        "]",
        "fn f() -> i32 { } }",
        "fn f() -> i32 { { } }",
        "\u{0}",
        "fn f() -> i32 { let x = 1; x = ; 0 }",
        "fn f() -> i32 { let x = 1; x[ = 1; 0 }",
        "fn f() -> i32 { 1 + 0 }",
        "fn f() -> i32 { && 1 }",
        "fn f() -> i32 { 1 as as i32 }",
        "fn f() -> i32 { let x = &1; 0 }",
        "fn f() -> i32 { *1 }",
        "fn f() -> i32 { 0..1 }",
        "fn f() -> i32 { [0u8; -1] }",
        "fn f() -> usize { -1 }",
        "fn f() -> i32 { 0x }",
        "fn f() -> i32 { 0b1010 }",
        "fn f() -> i32 { let x = 1e5; 0 }",
    ];
    for case in cases {
        let mut sources = SourceManager::new();
        // The only requirement is that this returns rather than unwinding.
        let _ = compile(&mut sources, "t.lazen", case);
    }
}

#[test]
fn a_very_large_program_is_still_only_diagnosed() {
    // Deeply nested but syntactically valid input must not overflow the stack.
    let mut source = String::new();
    for _ in 0..500 {
        source.push_str("if true { ");
    }
    source.push('1');
    for _ in 0..500 {
        source.push_str(" } else { 0 }");
    }
    let mut sources = SourceManager::new();
    let _ = compile(
        &mut sources,
        "t.lazen",
        &format!("fn f() -> i32 {{ {source} }}"),
    );
}
