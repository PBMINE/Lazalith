//! Integrated tests over the programs in `docs/lazen-syntax.md`.
//!
//! The syntax document says its examples are the specification, so every
//! example is a fixture here. If the grammar, the type system, and the
//! document ever disagree, one of these tests fails.
//!
//! Each example is tested twice where it can be: once as a program that must
//! compile, and, for the rules the document states, once as a program that must
//! be rejected with a specific code.

use lazalith_compiler::frontend::compile;
use lazalith_compiler::lexer::codes as lexer_codes;
use lazalith_compiler::resolve::codes as resolve_codes;
use lazalith_compiler::types::codes as type_codes;
use lazalith_types::SourceManager;

fn accepts(source: &str) -> lazalith_compiler::types::CheckedProgram {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "example.lazen", source) {
        Ok((_, program)) => program,
        Err(error) => panic!(
            "this program should compile, but it failed with {}:\n{}",
            error.code().as_str(),
            error.render()
        ),
    }
}

fn rejects(source: &str) -> (String, String) {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "example.lazen", source) {
        Ok(_) => panic!("this program should have been rejected, but it compiled"),
        Err(error) => (error.code().as_str().to_string(), error.render()),
    }
}

fn rejects_with(source: &str, code: &str) -> String {
    let (actual, rendered) = rejects(source);
    assert_eq!(actual, code, "wrong diagnostic:\n{rendered}");
    rendered
}

/// The line and column a diagnostic's primary label points at, as `(line, 1-based
/// column)`.
fn label_position(rendered: &str) -> (u32, u32) {
    // The shared renderer prints ` --> file:line:column`.
    let marker = rendered
        .lines()
        .find(|line| line.trim_start().starts_with("-->"))
        .expect("a rendered diagnostic has a source marker");
    // `--> file:line:column`, so the last two colon-separated fields are the
    // position.
    let fields: Vec<&str> = marker.split(':').collect();
    let (line, column) = (fields[fields.len() - 2], fields[fields.len() - 1]);
    (
        line.trim().parse().expect("a line number"),
        column.trim().parse().expect("a column number"),
    )
}

// ---------------------------------------------------------------- section 1

#[test]
fn section_1_hello_world() {
    let program = accepts(
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
    assert_eq!(program.strings[0].text, "Hello, Lazalith\n");
    assert_eq!(program.externs[0].name, "write");
    // A str is two words: a pointer and a length.
    let main = program.entry().expect("a main function");
    let message = main
        .locals
        .iter()
        .find(|local| local.name == "message")
        .expect("a `message` local");
    assert_eq!(lazalith_compiler::types::ty_name(&message.ty), "str");
    assert_eq!(message.offset, 0);
}

#[test]
fn section_1_a_call_must_pass_every_argument() {
    let rendered = rejects_with(
        r#"
extern "syscall" fn write(fd: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    write(1, 0);
    0
}
"#,
        type_codes::ARITY,
    );
    assert!(rendered.contains("takes 4 arguments"), "{rendered}");
    assert_eq!(label_position(&rendered), (5, 10));
}

// ---------------------------------------------------------------- section 2

#[test]
fn section_2_variables() {
    accepts(
        r#"
fn main() -> i32 {
    let answer = 42;
    let mut total: i64 = 0;
    let label = "sum";
    let flag = true;
    total = total + answer as i64;
    if flag && total > 0 {
        total = total + 1;
    }
    0
}
"#,
    );
}

#[test]
fn section_2_a_float_literal_is_rejected() {
    // The document states this exact rejection.
    let rendered = rejects_with(
        "fn main() -> i32 { let ratio = 1.5; 0 }",
        lexer_codes::FLOAT_LITERAL,
    );
    assert_eq!(label_position(&rendered), (1, 32));
}

#[test]
fn section_2_assigning_without_mut_is_rejected() {
    let rendered = rejects_with(
        "fn main() -> i32 { let total: i32 = 0; total = 1; 0 }",
        type_codes::IMMUTABLE,
    );
    // The diagnostic points at the assignment and labels the `let`.
    assert!(rendered.contains("declared here"), "{rendered}");
    assert_eq!(label_position(&rendered), (1, 40));
}

#[test]
fn section_2_types_are_inferred_and_never_converted() {
    // `answer` is an unannotated `42`, so it is an `i32`; adding it to an `i64`
    // needs a cast, and the document supplies one.
    accepts("fn main() -> i64 { let answer = 42; answer as i64 }");
    rejects_with(
        "fn main() -> i64 { let answer = 42; let t: i64 = 0; t + answer }",
        type_codes::MISMATCH,
    );
}

// ---------------------------------------------------------------- section 3

#[test]
fn section_3_functions() {
    let program = accepts(
        r#"
fn square(value: i32) -> i32 {
    value * value
}

fn clamp(value: i32, low: i32, high: i32) -> i32 {
    if value < low {
        return low;
    }
    if value > high {
        return high;
    }
    value
}

pub fn main() -> i32 {
    let result = square(7);
    let bounded = clamp(result, 0, 40);
    bounded as i32
}
"#,
    );
    assert_eq!(program.functions.len(), 3);
    assert!(program.entry().expect("main").is_public);
    // `square` has one parameter at offset zero. The frame is rounded up to the
    // machine word, so an `i32` parameter in a four-byte slot still leaves an
    // eight-byte frame: the stack stays word-aligned.
    let square = program.function("square").expect("a square function");
    assert_eq!(square.parameters.len(), 1);
    assert_eq!(square.parameters[0].offset, 0);
    assert_eq!(square.frame_size, 8);
}

#[test]
fn section_3_a_call_arity_and_argument_types_are_checked() {
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { f() }",
        type_codes::ARITY,
    );
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { f(1, 2) }",
        type_codes::ARITY,
    );
    // A variable of the wrong type is reported against the parameter it should
    // have matched, with the declaration labelled.
    let rendered = rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { let b = true; f(b) }",
        type_codes::ARGUMENT,
    );
    assert!(rendered.contains("this parameter is `i32`"), "{rendered}");
    // A literal of the wrong type is reported where the literal is.
    rejects_with(
        "fn f(a: i32) -> i32 { a } fn main() -> i32 { f(true) }",
        type_codes::MISMATCH,
    );
}

#[test]
fn section_3_a_function_name_is_not_a_value() {
    // Lazen v1 has no function types as values.
    rejects_with(
        "fn f() -> i32 { 0 } fn main() -> i32 { let g = f; 0 }",
        type_codes::NOT_A_VALUE,
    );
}

// ---------------------------------------------------------------- section 4

#[test]
fn section_4_conditionals() {
    let program = accepts(
        r#"
fn classify(value: i32) -> i32 {
    if value < 0 {
        -1
    } else if value == 0 {
        0
    } else {
        1
    }
}

fn main() -> i32 {
    let first = classify(-5);
    let second = classify(0);
    let third = classify(9);
    (first + second + third) as i32
}
"#,
    );
    let classify = program.function("classify").expect("classify");
    // The body is one `if` expression used as the function's result.
    assert!(classify.body.statements.is_empty(), "an if used as a value");
    assert!(classify.body.tail.is_some(), "a tail expression");
}

#[test]
fn section_4_a_condition_must_be_a_bool() {
    let rendered = rejects_with(
        "fn main() -> i32 { let n = 1; if n { 0 } else { 1 } 0 }",
        type_codes::CONDITION,
    );
    assert!(rendered.contains("no truthiness"), "{rendered}");
    rejects_with(
        "fn main() -> i32 { let n = 1; if n > 0 && n { 0 } 0 }",
        type_codes::CONDITION,
    );
}

#[test]
fn section_4_a_conditional_used_as_a_value_needs_every_arm() {
    // An `if` with no `else` is a statement, and a statement must not end in a
    // value.
    rejects_with(
        "fn main() -> i32 { let n = 1; if n > 0 { 1 } 0 }",
        type_codes::DISCARDED_VALUE,
    );
    // A conditional used as a value must have the same type in every arm.
    rejects_with(
        "fn main() -> i32 { let n = 1; if n > 0 { 1 } else { \"two\" } }",
        type_codes::MISMATCH,
    );
}

// ---------------------------------------------------------------- section 5

#[test]
fn section_5_loops() {
    let program = accepts(
        r#"
fn main() -> i32 {
    let mut total: i32 = 0;
    let mut index: i32 = 0;
    while index < 10 {
        total = total + index;
        index = index + 1;
    }
    for value in 0..10 {
        total = total + value;
    }
    let mut countdown: i32 = 3;
    loop {
        if countdown == 0 {
            break;
        }
        countdown = countdown - 1;
        if countdown == 1 {
            continue;
        }
    }
    total
}
"#,
    );
    let main = program.entry().expect("a main function");
    // The loop variable gets its own frame slot.
    assert!(main.locals.iter().any(|local| local.name == "value"));
}

#[test]
fn section_5_break_outside_a_loop_is_rejected() {
    let rendered = rejects_with("fn main() -> i32 { break; 0 }", type_codes::OUTSIDE_LOOP);
    assert_eq!(label_position(&rendered), (1, 20));
    // A `loop` with no `break` never falls through, so it satisfies a result
    // type; a body that can simply end does not.
    accepts("fn main() -> i32 { loop { continue; } }");
    rejects_with(
        "fn main() -> i32 { let x = 1; }",
        type_codes::MISSING_RESULT,
    );
}

#[test]
fn section_5_a_loop_over_a_collection_is_not_in_v1() {
    // The document lists `for x in collection` as a deliberate omission.
    let rendered = rejects_with(
        r#"
fn main() -> i32 {
    let values = [1, 2, 3];
    let mut total = 0;
    for value in values { total = total + value; }
    total
}
"#,
        type_codes::NOT_A_RANGE,
    );
    assert!(rendered.contains("integer range"), "{rendered}");
}

#[test]
fn section_5_range_bounds_must_agree() {
    // A range's two bounds must have the same integer type.
    rejects_with(
        "fn main() -> i32 { for v in 0..(10u8) { } 0 }",
        type_codes::MISMATCH,
    );
    // A range whose end is not an integer is a type error, not a syntax error.
    rejects_with(
        "fn main() -> i32 { for v in 0..\"x\" { } 0 }",
        type_codes::MISMATCH,
    );
}

// ---------------------------------------------------------------- section 6

#[test]
fn section_6_arrays() {
    let program = accepts(
        r#"
fn main() -> i32 {
    let mut values = [1, 2, 3, 4];
    let mut index: usize = 0;
    let mut total: i32 = 0;
    while index < values.len() {
        total = total + values[index];
        index = index + 1;
    }
    values[0] = 10;
    let first = values[0];
    let slice = values.as_slice();
    total + first + slice.len() as i32
}
"#,
    );
    let main = program.entry().expect("a main function");
    let values = main
        .locals
        .iter()
        .find(|local| local.name == "values")
        .expect("a `values` local");
    assert_eq!(lazalith_compiler::types::ty_name(&values.ty), "[i32; 4]");
    // Four i32 elements, then the rest of the frame.
    assert_eq!(values.offset, 0);
}

#[test]
fn section_6_an_index_must_be_usize() {
    let rendered = rejects_with(
        "fn main() -> i32 { let values = [1, 2]; let i = 0; values[i] }",
        type_codes::INDEX_TYPE,
    );
    assert!(rendered.contains("usize"), "{rendered}");
    // An explicit cast is how a program says so.
    accepts("fn main() -> i32 { let values = [1, 2]; let i = 0; values[i as usize] }");
}

#[test]
fn section_6_array_repeat_and_len() {
    let program = accepts(
        r#"
fn main() -> i32 {
    let mut scratch = [0u8; 16];
    scratch[0] = 65;
    scratch.len() as i32
}
"#,
    );
    let main = program.entry().expect("a main function");
    let scratch = main
        .locals
        .iter()
        .find(|local| local.name == "scratch")
        .expect("a `scratch` local");
    assert_eq!(lazalith_compiler::types::ty_name(&scratch.ty), "[u8; 16]");
    assert_eq!(scratch.ty.size_in_bytes(lazalith_types::WordWidth::W64), 16);
}

#[test]
fn section_6_mixed_array_element_types_are_rejected() {
    rejects_with(
        "fn main() -> i32 { let values = [1, \"two\"]; 0 }",
        type_codes::MISMATCH,
    );
    rejects_with(
        "fn main() -> i32 { let values = [1, true]; 0 }",
        type_codes::MISMATCH,
    );
}

// ---------------------------------------------------------------- section 7

#[test]
fn section_7_modules() {
    let program = accepts(
        r#"
mod geometry {
    pub fn area(width: u32, height: u32) -> u32 {
        width * height
    }

    fn unused() -> i32 {
        0
    }
}

fn main() -> i32 {
    let size: u32 = 4;
    geometry::area(size, 5) as i32
}
"#,
    );
    assert!(program.function("geometry::area").is_some());
    assert!(program.function("geometry::unused").is_some());
    assert!(program.function("unused").is_none());
}

#[test]
fn section_7_a_private_item_is_not_reachable_from_outside() {
    let rendered = rejects_with(
        r#"
mod geometry {
    fn area(width: u32, height: u32) -> u32 {
        width * height
    }
}

fn main() -> i32 {
    geometry::area(4, 5) as i32
}
"#,
        resolve_codes::PRIVATE,
    );
    assert!(rendered.contains("`pub`"), "{rendered}");
}

#[test]
fn section_7_a_module_is_not_a_value() {
    rejects_with(
        "mod m { pub fn f() -> i32 { 0 } } fn main() -> i32 { let x = m; 0 }",
        resolve_codes::NOT_A_MODULE,
    );
}

#[test]
fn section_7_duplicate_items_are_rejected() {
    let rendered = rejects_with(
        "fn f() -> i32 { 0 } fn f() -> i32 { 1 } fn main() -> i32 { 0 }",
        resolve_codes::DUPLICATE_ITEM,
    );
    assert!(rendered.contains("first defined here"), "{rendered}");
}

// ---------------------------------------------------------------- section 8

#[test]
fn section_8_pointers() {
    let program = accepts(
        r#"
extern "syscall" fn write(handle: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    let message = "Hello, Lazalith\n";
    let bytes = message.as_bytes();
    let address = bytes.as_ptr() as u64;
    if address == 0 {
        return 1;
    }
    let mut scratch = [0u8; 16];
    scratch[0] = 65;
    write(1, bytes, message.len() as u64, scratch.as_mut_slice().as_ptr());
    0
}
"#,
    );
    let main = program.entry().expect("a main function");
    assert!(main.locals.iter().any(|local| local.name == "address"));
}

#[test]
fn section_8_a_raw_pointer_is_not_dereferenced() {
    // The document says a dereference is always through a typed view, because
    // `ptr<T>` carries no length and there is no `unsafe`.
    let rendered = rejects_with(
        r#"
extern "syscall" fn write(handle: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    let message = "hi";
    let address = message.as_ptr();
    let value = *address;
    0
}
"#,
        type_codes::RAW_DEREF,
    );
    assert!(rendered.contains("bounds checked"), "{rendered}");
}

#[test]
fn section_8_a_mutable_reference_needs_a_mutable_binding() {
    rejects_with(
        "fn main() -> i32 { let value = 1; let r = &mut value; 0 }",
        type_codes::BAD_BORROW,
    );
    accepts("fn main() -> i32 { let mut value = 1; let r = &mut value; 0 }");
}

#[test]
fn section_8_a_reference_to_an_array_points_at_as_slice() {
    let rendered = rejects_with(
        "fn main() -> i32 { let values = [1, 2]; let r = &values; 0 }",
        type_codes::BAD_BORROW,
    );
    assert!(rendered.contains("as_slice"), "{rendered}");
}

// ---------------------------------------------------------------- section 9

#[test]
fn section_9_errors_are_integer_statuses() {
    let program = accepts(
        r#"
extern "syscall" fn open(path: &[u8], path_length: u64, flags: u32, handle: ptr<u32>) -> i64;
extern "syscall" fn read(handle: u32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;
extern "syscall" fn close(handle: u32) -> i64;

fn read_first_byte(path: &str) -> i32 {
    let mut handle: u32 = 0;
    let status = open(path.as_bytes(), path.len() as u64, 1, &mut handle as ptr<u32>);
    if status != 0 {
        return -1;
    }
    let mut buffer = [0u8; 256];
    let mut result = [0u8; 16];
    let read_status = read(handle, buffer.as_slice(), 256, result.as_ptr());
    let _ = close(handle);
    if read_status != 0 {
        return -2;
    }
    buffer[0] as i32
}

fn main() -> i32 {
    read_first_byte("/hello.txt")
}
"#,
    );
    let read = program
        .externs
        .iter()
        .find(|declaration| declaration.name == "read")
        .expect("a read declaration");
    assert!(read.syscall.is_some(), "read maps to a real syscall");
    // `let _` discards the status without a binding.
    let main = program.entry().expect("a main function");
    assert!(main.locals.iter().all(|local| local.name != "_"));
}

#[test]
fn section_9_a_statement_may_not_discard_a_value() {
    // The document states this rule explicitly.
    let rendered = rejects_with("fn main() -> i32 { 1 + 2; 0 }", type_codes::DISCARDED_VALUE);
    assert!(rendered.contains("only a call"), "{rendered}");
    // A call may be a statement.
    accepts("extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { close(1); 0 }");
}

#[test]
fn section_9_a_mut_cannot_be_a_wildcard() {
    rejects_with(
        "extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { let mut _ = close(1); 0 }",
        type_codes::BAD_BORROW,
    );
}

// --------------------------------------------------------------- section 10

#[test]
fn section_10_the_sdk_module_shape_parses_and_checks() {
    accepts(
        r#"
mod fs {
    pub fn read_file(path: &str, buffer: &mut [u8]) -> i64 {
        0
    }
}
"#,
    );
}

// --------------------------------------------------------------- section 11

#[test]
fn section_11_graphics() {
    let program = accepts(
        r#"
extern "syscall" fn display_open(width: u32, height: u32, framebuffer: ptr<u64>) -> i64;
extern "syscall" fn display_present() -> i64;

mod ui {
    pub fn open(width: u32, height: u32) -> i64 {
        let mut framebuffer: u64 = 0;
        display_open(width, height, &mut framebuffer as ptr<u64>)
    }

    pub fn present() -> bool {
        display_present() == 0
    }
}
"#,
    );
    // The design reserves these names for a later ABI step, so they are
    // accepted but carry no syscall number yet.
    let display = program
        .externs
        .iter()
        .find(|declaration| declaration.name == "display_open")
        .expect("a display_open declaration");
    assert!(display.syscall.is_none(), "no number is invented");
}

// --------------------------------------------------------------- section 12

#[test]
fn section_12_input() {
    accepts(
        r#"
extern "syscall" fn input_poll(events: ptr<u32>, capacity: u32) -> i64;

fn drain(buffer: &mut [u32]) -> u32 {
    let count = input_poll(buffer.as_mut_slice().as_ptr() as ptr<u32>, 32);
    if count < 0 {
        return 0;
    }
    count as u32
}
"#,
    );
}

// --------------------------------------------------------------- section 13

#[test]
fn section_13_every_documented_omission_is_rejected_by_name() {
    // records
    rejects_with(
        "struct Point { x: i32 } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::STRUCT,
    );
    // enums
    rejects_with(
        "enum Colour { Red } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::ENUM,
    );
    // match
    rejects_with(
        "fn main() -> i32 { match 1 { } 0 }",
        lazalith_compiler::parser::codes::MATCH,
    );
    // optional
    rejects_with(
        "fn f() -> optional<i32> { } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::TYPE_ALIAS,
    );
    // `some` and `none`
    rejects_with(
        "fn main() -> i32 { let x = some; 0 }",
        lazalith_compiler::parser::codes::OPTIONAL,
    );
    // traits and generics
    rejects_with(
        "trait T { } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::TRAIT,
    );
    // impl blocks
    rejects_with(
        "impl S { } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::IMPL,
    );
    // type aliases
    rejects_with(
        "type Count = i32; fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::TYPE_ALIAS,
    );
    // unsafe blocks
    rejects_with(
        "fn main() -> i32 { unsafe { 0 } }",
        lazalith_compiler::parser::codes::UNSAFE,
    );
    // the `?` operator
    let rendered = rejects_with(
        "fn f() -> i32 { 0 } fn main() -> i32 { let x = f()?; 0 }",
        lazalith_compiler::parser::codes::QUESTION,
    );
    assert!(rendered.contains("no `optional`"), "{rendered}");
    // block comments
    rejects_with(
        "fn main() -> i32 { /* no */ 0 }",
        lexer_codes::BLOCK_COMMENT,
    );
    // floats
    rejects_with("fn main() -> i32 { 1.0 }", lexer_codes::FLOAT_LITERAL);
}

#[test]
fn section_13_a_rejection_explains_itself() {
    let rendered = rejects_with(
        "struct Point { x: i32 } fn main() -> i32 { 0 }",
        lazalith_compiler::parser::codes::STRUCT,
    );
    assert!(rendered.contains("no records"), "{rendered}");
    assert!(rendered.contains("docs/lazen-syntax.md"), "{rendered}");
}
