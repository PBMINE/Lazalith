//! Parser tests: the grammar of `docs/lazen-syntax.md`.
//!
//! Each test names the rule it pins. A test asserts both that a construct
//! parses into the expected shape and, where a rule is about rejection, that the
//! diagnostic points at the right characters.

use lazalith_compiler::ast::{BinaryOp, CompareOp, Item, Stmt, TypeExpr, UnaryOp};
use lazalith_compiler::frontend::parse_file;
use lazalith_compiler::parser::codes;
use lazalith_types::SourceManager;

fn parse_ok(source: &str) -> lazalith_compiler::ast::Program {
    let mut sources = SourceManager::new();
    let id = sources.add_file("t.lazen", source).expect("a source");
    match parse_file(id, &sources) {
        Ok(program) => program,
        Err(error) => panic!(
            "expected this to parse, but it failed with {}:\n{}",
            error.code().as_str(),
            error.render()
        ),
    }
}

fn parse_err(source: &str) -> (String, String) {
    let mut sources = SourceManager::new();
    let id = sources.add_file("t.lazen", source).expect("a source");
    match parse_file(id, &sources) {
        Ok(_) => panic!("expected a parse error, but this parsed:\n{source}"),
        Err(error) => (error.code().as_str().to_string(), error.render()),
    }
}

fn parse_err_with(source: &str, code: &str) -> String {
    let (actual, rendered) = parse_err(source);
    assert_eq!(actual, code, "wrong diagnostic:\n{rendered}");
    rendered
}

/// The single function in a parsed program, for tests that only care about one.
fn only_function(source: &str) -> lazalith_compiler::ast::Function {
    let program = parse_ok(source);
    match program.items.as_slice() {
        [Item::Function(function)] => function.clone(),
        other => panic!("expected exactly one function, found {} items", other.len()),
    }
}

#[test]
fn a_function_has_parameters_a_result_and_a_body() {
    let function = only_function("fn f(a: i32, b: &[u8]) -> i64 { 0 }");
    assert_eq!(function.name.text, "f");
    assert_eq!(function.parameters.len(), 2);
    assert_eq!(function.parameters[0].name.text, "a");
    assert_eq!(function.parameters[0].annotation.kind, TypeExpr::I32);
    assert_eq!(
        function.parameters[1].annotation.kind,
        TypeExpr::Slice {
            element: Box::new(TypeExpr::U8),
            mutable: false
        }
    );
    assert_eq!(
        function.result.as_ref().map(|result| &result.kind),
        Some(&TypeExpr::I64)
    );
}

#[test]
fn a_function_with_no_result_type_has_none() {
    let function = only_function("fn f() { }");
    assert!(function.result.is_none());
    assert!(function.parameters.is_empty());
}

#[test]
fn every_type_form_in_the_specification_parses() {
    let function = only_function(
        "fn f(a: bool, b: i8, c: i16, d: i32, e: i64, g: u8, h: u16, i: u32, j: u64, \
         k: usize, l: str, m: &str, n: ptr<u8>, o: &[i32], p: &mut [u32], q: [u8; 16]) -> bool { true }",
    );
    let kinds: Vec<TypeExpr> = function
        .parameters
        .iter()
        .map(|parameter| parameter.annotation.kind.clone())
        .collect();
    assert_eq!(
        kinds,
        vec![
            TypeExpr::Bool,
            TypeExpr::I8,
            TypeExpr::I16,
            TypeExpr::I32,
            TypeExpr::I64,
            TypeExpr::U8,
            TypeExpr::U16,
            TypeExpr::U32,
            TypeExpr::U64,
            TypeExpr::Usize,
            TypeExpr::Str,
            TypeExpr::StrRef,
            TypeExpr::Ptr(Box::new(TypeExpr::U8)),
            TypeExpr::Slice {
                element: Box::new(TypeExpr::I32),
                mutable: false
            },
            TypeExpr::Slice {
                element: Box::new(TypeExpr::U32),
                mutable: true
            },
            TypeExpr::Array {
                element: Box::new(TypeExpr::U8),
                length: 16
            },
        ]
    );
}

#[test]
fn a_nested_type_keeps_its_own_shape() {
    let function =
        only_function("fn f(a: &[&[u8]], b: ptr<ptr<u8>>, c: [[u8; 2]; 3]) -> i32 { 0 }");
    assert_eq!(
        function.parameters[0].annotation.kind,
        TypeExpr::Slice {
            element: Box::new(TypeExpr::Slice {
                element: Box::new(TypeExpr::U8),
                mutable: false
            }),
            mutable: false
        }
    );
    assert_eq!(
        function.parameters[1].annotation.kind,
        TypeExpr::Ptr(Box::new(TypeExpr::Ptr(Box::new(TypeExpr::U8))))
    );
    assert_eq!(
        function.parameters[2].annotation.kind,
        TypeExpr::Array {
            element: Box::new(TypeExpr::Array {
                element: Box::new(TypeExpr::U8),
                length: 2
            }),
            length: 3
        }
    );
}

#[test]
fn a_type_outside_the_closed_set_is_named_in_the_diagnostic() {
    for (type_text, expected) in [
        ("optional<i32>", codes::TYPE_ALIAS),
        ("f32", codes::TYPE_ALIAS),
        ("String", codes::TYPE_ALIAS),
        ("u128", codes::TYPE_ALIAS),
        ("void", codes::TYPE_ALIAS),
        ("&i32", codes::EXPECTED),
        ("&u8", codes::EXPECTED),
    ] {
        let source = format!("fn f(a: {type_text}) -> i32 {{ 0 }}");
        parse_err_with(&source, expected);
    }
}

#[test]
fn multiplication_binds_more_tightly_than_addition() {
    let function = only_function("fn f() -> i32 { 1 + 2 * 3 }");
    let tail = function.body.tail.as_ref().expect("a tail expression");
    let lazalith_compiler::ast::Expr::Binary {
        operator, right, ..
    } = &**tail
    else {
        panic!("expected a binary expression");
    };
    assert_eq!(
        *operator,
        BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Add)
    );
    // The right operand is the multiplication.
    assert!(matches!(
        &**right,
        lazalith_compiler::ast::Expr::Binary {
            operator: BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Mul),
            ..
        }
    ));
}

#[test]
fn subtraction_is_left_associative() {
    let function = only_function("fn f() -> i32 { 1 - 2 - 3 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Binary {
        operator,
        left,
        right,
        ..
    } = &**tail
    else {
        panic!("expected a binary expression");
    };
    assert_eq!(
        *operator,
        BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Sub)
    );
    // `(1 - 2) - 3`, so the left is itself a subtraction.
    assert!(matches!(
        &**left,
        lazalith_compiler::ast::Expr::Binary { .. }
    ));
    assert!(matches!(&**right, lazalith_compiler::ast::Expr::Int { .. }));
}

#[test]
fn comparison_binds_more_tightly_than_logic() {
    let function = only_function("fn f() -> bool { 1 < 2 && 3 > 4 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Binary { operator, left, .. } = &**tail else {
        panic!("expected a binary expression");
    };
    assert_eq!(*operator, BinaryOp::And);
    assert!(matches!(
        &**left,
        lazalith_compiler::ast::Expr::Binary {
            operator: BinaryOp::Compare(CompareOp::Less),
            ..
        }
    ));
}

#[test]
fn logical_and_binds_more_tightly_than_logical_or() {
    let function = only_function("fn f() -> bool { true || false && true }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Binary {
        operator, right, ..
    } = &**tail
    else {
        panic!("expected a binary expression");
    };
    assert_eq!(*operator, BinaryOp::Or);
    assert!(matches!(
        &**right,
        lazalith_compiler::ast::Expr::Binary {
            operator: BinaryOp::And,
            ..
        }
    ));
}

#[test]
fn a_cast_binds_more_tightly_than_a_binary_operator() {
    let function = only_function("fn f() -> u64 { 1 + 2 as u64 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Binary { right, .. } = &**tail else {
        panic!("expected a binary expression");
    };
    // `2 as u64`, not `1 + 2` cast afterwards.
    assert!(matches!(
        &**right,
        lazalith_compiler::ast::Expr::Cast { .. }
    ));
}

#[test]
fn a_cast_binds_more_loosely_than_a_unary_operator() {
    // `-x as i64` is `(-x) as i64`, and `&mut h as ptr<u32>` takes the address
    // first and casts second.
    let function = only_function("fn f(h: i32) -> i64 { -h as i64 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Cast { operand, .. } = &**tail else {
        panic!("expected a cast");
    };
    assert!(matches!(
        &**operand,
        lazalith_compiler::ast::Expr::Unary {
            operator: UnaryOp::Negate,
            ..
        }
    ));
}

#[test]
fn casts_chain_from_left_to_right() {
    let function = only_function("fn f() -> i64 { 1 as u8 as i64 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Cast {
        operand, target, ..
    } = &**tail
    else {
        panic!("expected a cast");
    };
    assert_eq!(target.kind, TypeExpr::I64);
    assert!(matches!(
        &**operand,
        lazalith_compiler::ast::Expr::Cast { .. }
    ));
}

#[test]
fn parentheses_override_precedence() {
    let function = only_function("fn f() -> i32 { (1 + 2) * 3 }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Binary { operator, left, .. } = &**tail else {
        panic!("expected a binary expression");
    };
    assert_eq!(
        *operator,
        BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Mul)
    );
    assert!(matches!(
        &**left,
        lazalith_compiler::ast::Expr::Binary {
            operator: BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Add),
            ..
        }
    ));
}

#[test]
fn nested_calls_and_indexing_are_postfix() {
    let function = only_function("fn f() -> i32 { values[index[0]] }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::Index { base, index, .. } = &**tail else {
        panic!("expected an index");
    };
    assert!(matches!(&**base, lazalith_compiler::ast::Expr::Path { .. }));
    assert!(matches!(
        &**index,
        lazalith_compiler::ast::Expr::Index { .. }
    ));
}

#[test]
fn a_block_that_ends_in_a_semicolon_has_no_tail() {
    let function = only_function("fn f() -> i32 { 1; }");
    assert_eq!(function.body.statements.len(), 1);
    assert!(function.body.tail.is_none());
}

#[test]
fn a_block_that_ends_in_an_expression_has_a_tail() {
    let function = only_function("fn f() -> i32 { 1 }");
    assert!(function.body.statements.is_empty());
    assert!(function.body.tail.is_some());
}

#[test]
fn a_conditional_with_an_else_is_a_value_and_one_without_is_a_statement() {
    let value = only_function("fn f() -> i32 { if true { 1 } else { 2 } }");
    assert!(value.body.tail.is_some(), "an if-else can be a value");

    let statement = only_function("fn f() -> i32 { if true { 1; } }");
    assert!(
        statement.body.tail.is_none(),
        "an if without else can never be a value"
    );
    assert!(matches!(
        statement.body.statements.first(),
        Some(Stmt::If { .. })
    ));
}

#[test]
fn an_else_if_chain_becomes_several_arms() {
    let function = only_function("fn f() -> i32 { if a { 1 } else if b { 2 } else { 3 } }");
    let tail = function.body.tail.as_ref().expect("a tail");
    let lazalith_compiler::ast::Expr::If { arms, .. } = &**tail else {
        panic!("expected a conditional");
    };
    assert_eq!(arms.len(), 3);
    assert!(arms[0].condition.is_some());
    assert!(arms[1].condition.is_some());
    assert!(arms[2].condition.is_none(), "the last arm is the else");
}

#[test]
fn a_for_loop_takes_a_range_and_records_its_end() {
    let function = only_function("fn f() -> i32 { for v in 0..10 { } 0 }");
    let Stmt::For { iterated, end, .. } = &function.body.statements[0] else {
        panic!("expected a for loop");
    };
    assert!(matches!(iterated, lazalith_compiler::ast::Expr::Int { .. }));
    assert!(end.is_some(), "a range loop has an end");
}

#[test]
fn a_for_loop_over_something_else_is_parsed_and_left_to_the_checker() {
    // The parser does not decide what can be iterated; the type checker rejects
    // a collection by name, which is a better diagnostic than a parse error.
    let function = only_function("fn f() -> i32 { for v in values { } 0 }");
    let Stmt::For { end, .. } = &function.body.statements[0] else {
        panic!("expected a for loop");
    };
    assert!(end.is_none(), "no range end, so it is a collection");
}

#[test]
fn a_compound_assignment_is_desugared_into_a_read_and_a_write() {
    let function = only_function("fn f() -> i32 { let mut total = 0; total += 1; total }");
    let Stmt::Assign {
        target,
        value: lazalith_compiler::ast::Expr::Binary { operator, left, .. },
        ..
    } = &function.body.statements[1]
    else {
        panic!("expected a compound assignment to become an assignment");
    };
    assert_eq!(
        *operator,
        BinaryOp::Arith(lazalith_compiler::ast::ArithOp::Add)
    );
    assert!(matches!(&**left, lazalith_compiler::ast::Expr::Path { .. }));
    assert!(matches!(target, lazalith_compiler::ast::Expr::Path { .. }));
}

#[test]
fn extern_declarations_take_a_syscall_abi_and_end_with_a_semicolon() {
    let program = parse_ok("extern \"syscall\" fn write(fd: i32) -> i64;");
    match program.items.as_slice() {
        [Item::Extern(declaration)] => {
            assert_eq!(declaration.name.text, "write");
            assert_eq!(declaration.parameters.len(), 1);
        }
        other => panic!(
            "expected one extern declaration, found {} items",
            other.len()
        ),
    }
}

#[test]
fn an_extern_declaration_may_omit_trailing_arguments() {
    // The ABI table decides the real arity; the checker compares them.
    parse_ok("extern \"syscall\" fn close(handle: u32) -> i64;");
}

#[test]
fn modules_nest_and_use_declares_an_import() {
    let program = parse_ok(
        "use geometry::area as surface; mod geometry { pub fn area(w: u32) -> u32 { w } }",
    );
    assert_eq!(program.items.len(), 2);
    match &program.items[0] {
        Item::Use(declaration) => {
            assert_eq!(declaration.path.segments.len(), 2);
            assert_eq!(
                declaration.alias.as_ref().map(|alias| alias.text.as_str()),
                Some("surface")
            );
        }
        other => panic!("expected a use declaration, found {other:?}"),
    }
    match &program.items[1] {
        Item::Module(module) => {
            assert_eq!(module.name.text, "geometry");
            assert_eq!(module.items.len(), 1);
        }
        other => panic!("expected a module, found {other:?}"),
    }
}

#[test]
fn a_const_declaration_may_annotate_its_type() {
    let program = parse_ok("const LIMIT: u32 = 10;");
    match program.items.as_slice() {
        [Item::Const(constant)] => {
            assert_eq!(constant.name.text, "LIMIT");
            assert!(constant.annotation.is_some());
        }
        other => panic!("expected one const, found {} items", other.len()),
    }
}

#[test]
fn pub_is_allowed_on_functions_modules_and_consts() {
    let program = parse_ok(
        "pub const A: i32 = 1; pub mod m { pub fn f() -> i32 { 0 } } pub fn g() -> i32 { 0 }",
    );
    assert_eq!(program.items.len(), 3);
    assert!(program.items.iter().all(|item| item.is_public()));
}

#[test]
fn a_missing_semicolon_is_reported_where_it_is_missing() {
    // The diagnostic names the token it wanted and the one it found.
    let rendered = parse_err_with("fn f() -> i32 { let x = 1 0 }", codes::EXPECTED);
    assert!(rendered.contains("expected `;`"), "{rendered}");
    assert!(rendered.contains("found integer literal `0`"), "{rendered}");
}

#[test]
fn a_missing_closing_brace_is_reported() {
    let rendered = parse_err_with("fn f() -> i32 { let x = 1;", codes::EXPECTED_BRACE);
    assert!(rendered.contains("never closed"), "{rendered}");
}

#[test]
fn a_stray_closing_delimiter_is_reported() {
    // A stray `}` is not an item.
    parse_err_with("fn f() -> i32 { 0 } }", codes::UNEXPECTED_STATEMENT);
    parse_err_with("fn f() -> i32 { (0 }", codes::EXPECTED);
    parse_err_with("fn f() -> i32 { [0 }", codes::EXPECTED);
}

#[test]
fn an_empty_file_parses_to_no_items() {
    let program = parse_ok("// just a comment\n");
    assert!(program.items.is_empty());
}

#[test]
fn an_empty_array_literal_is_rejected() {
    let rendered = parse_err_with("fn f() -> i32 { let a = []; 0 }", codes::EXPECTED);
    assert!(rendered.contains("at least one element"), "{rendered}");
}

#[test]
fn an_array_length_must_be_a_plain_count() {
    parse_ok("fn f() -> i32 { let a = [u8; 16]; 0 }");
    parse_ok("fn f(a: [u8; 0x10]) -> i32 { 0 }");
    let rendered = parse_err_with("fn f(a: [u8; 16u8]) -> i32 { 0 }", codes::EXPECTED);
    assert!(rendered.contains("plain count"), "{rendered}");
    parse_err_with("fn f(a: [u8; n]) -> i32 { 0 }", codes::EXPECTED);
}

#[test]
fn a_single_pipe_is_rejected_by_the_lexer() {
    let rendered = parse_err_with(
        "fn f() -> i32 { let a = 1 | 2; 0 }",
        lazalith_compiler::lexer::codes::SINGLE_PIPE,
    );
    assert!(rendered.contains("||"), "{rendered}");
}

#[test]
fn deep_nesting_is_rejected_rather_than_overflowing_the_stack() {
    let deep = format!(
        "fn f() -> i32 {{ {}{} 0 {} }}",
        "if true { ".repeat(200),
        "",
        "}".repeat(200)
    );
    let (code, rendered) = parse_err(&deep);
    assert_eq!(code, codes::UNBALANCED);
    assert!(rendered.contains("nests deeper"), "{rendered}");
}

#[test]
fn a_question_mark_after_any_expression_is_rejected() {
    parse_err_with("fn f() -> i32 { let a = x?; 0 }", codes::QUESTION);
    parse_err_with("fn f() -> i32 { let a = f()?; 0 }", codes::QUESTION);
    parse_err_with("fn f() -> i32 { let a = v[0]?; 0 }", codes::QUESTION);
}
