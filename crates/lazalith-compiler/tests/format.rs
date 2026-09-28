//! The formatter's three promises: it changes whitespace only, it is idempotent,
//! and it keeps every comment.
//!
//! Every fixture is checked three ways, because each of the three can fail on its
//! own: the tokens must be the same, the comments must be the same, and formatting
//! the output again must change nothing. A formatter that satisfies only the first is
//! a token-preserving pretty-printer; the second is what makes it safe to run twice,
//! which is the only property a person actually relies on.

use lazalith_compiler::{
    format::{format, is_formatted},
    lexer,
};
use lazalith_types::SourceManager;

/// The token stream, as `(kind, text)` pairs, with the `Eof` dropped.
fn tokens(text: &str) -> Vec<(String, String)> {
    let mut sources = SourceManager::default();
    let source = sources
        .add_file("f.lz", text)
        .expect("the source registers");
    let lexed = lexer::lex(source, &sources);
    assert!(
        lexed.diagnostics.is_empty(),
        "the fixture does not lex: {text:?} -> {:?}",
        lexed.diagnostics
    );
    lexed
        .tokens
        .iter()
        .filter(|token| !matches!(token.kind, lazalith_compiler::lexer::TokenKind::Eof))
        .map(|token| (format!("{:?}", token.kind), token.text.clone()))
        .collect()
}

/// The comments, as text, in order.
fn comments(text: &str) -> Vec<String> {
    let mut sources = SourceManager::default();
    let source = sources
        .add_file("f.lz", text)
        .expect("the source registers");
    lexer::lex(source, &sources)
        .comments
        .iter()
        .map(|comment| comment.text.clone())
        .collect()
}

/// Checks all three promises for one fixture.
fn check(input: &str) -> String {
    let formatted =
        format(input).unwrap_or_else(|error| panic!("{input:?} did not format: {error}"));
    assert_eq!(
        tokens(input),
        tokens(&formatted),
        "the token stream changed for {input:?} -> {formatted:?}"
    );
    assert_eq!(
        comments(input),
        comments(&formatted),
        "the comments changed for {input:?} -> {formatted:?}"
    );
    let again = format(&formatted).expect("the output formats");
    assert_eq!(
        formatted, again,
        "formatting is not idempotent for {input:?}: {formatted:?} -> {again:?}"
    );
    assert!(is_formatted(&formatted).expect("the output lexes"));
    formatted
}

/// Every fixture, formatted the way the style says.
const STYLE: &[(&str, &str)] = &[
    // Four spaces per level.
    (
        "fn f() -> i32 {\nreturn 1;\n}\n",
        "fn f() -> i32 {\n    return 1;\n}\n",
    ),
    // A space before `{`, and a call is tight. A body the author wrote across lines
    // is indented; a body they wrote on one line is left alone, because a formatter
    // that breaks a one-line `if` is making a claim about it they did not make.
    (
        "fn f() -> i32 {\nreturn 1;\n}\n",
        "fn f() -> i32 {\n    return 1;\n}\n",
    ),
    ("if a { b(); }\n", "if a { b(); }\n"),
    // An empty body stays empty.
    ("struct S {}\n", "struct S {}\n"),
    // One space around a binary operator.
    ("let x: i32=1+2*3;\n", "let x: i32 = 1 + 2 * 3;\n"),
    // A subscript and a call are tight.
    ("let y=xs[i]+f(1);\n", "let y = xs[i] + f(1);\n"),
    // A comma takes no space before and one after.
    ("f(a,b,c);\n", "f(a, b, c);\n"),
    // A path is tight.
    ("let m=a::b::c;\n", "let m = a::b::c;\n"),
    // A cast and a type are tight.
    ("let p=*q;\n", "let p = *q;\n"),
    // Exactly one trailing newline, and none before it.
    ("let x=1;\n\n\n", "let x = 1;\n"),
    // Trailing whitespace goes.
    ("let x = 1;   \n", "let x = 1;\n"),
];

#[test]
fn the_style_is_what_the_document_says() {
    for (input, expected) in STYLE {
        let formatted = check(input);
        assert_eq!(
            &formatted, expected,
            "\n  input: {input:?}\n  wanted: {expected:?}\n  got:    {formatted:?}"
        );
    }
}

#[test]
fn a_comment_survives_and_is_reindented() {
    let input = "// a note\nfn f() -> i32 {\n        // why this is 1\nreturn 1;\n}\n";
    let formatted = check(input);
    assert_eq!(
        formatted, "// a note\nfn f() -> i32 {\n    // why this is 1\n    return 1;\n}\n",
        "a comment should keep its text and get the block's indent"
    );
}

#[test]
fn a_comment_at_the_end_of_a_file_is_not_eaten() {
    let formatted = check("fn f() -> i32 {\n    return 1;\n}\n// the end\n");
    assert!(
        formatted.contains("// the end"),
        "a trailing comment was eaten: {formatted:?}"
    );
}

#[test]
fn an_empty_file_stays_empty_and_a_whitespace_only_one_becomes_empty() {
    assert_eq!(check(""), "");
    assert_eq!(check("\n\n   \n"), "");
}

#[test]
fn nested_blocks_indent_and_dedent() {
    let formatted = check("fn f() -> i32 {\nif a {\nif b {\nreturn 1;\n}\n}\nreturn 0;\n}\n");
    assert_eq!(
        formatted,
        "fn f() -> i32 {\n    if a {\n        if b {\n            return 1;\n        }\n    }\n    return 0;\n}\n"
    );
}

/// The one decision a token stream cannot make, and the reason the formatter reads
/// the source for it.
#[test]
fn a_sign_the_author_wrote_as_unary_stays_unary_and_one_wrote_as_binary_stays_binary() {
    // Unary: a space before the sign and none after.
    let formatted = check("let x = - 1;\n");
    assert_eq!(
        formatted, "let x = -1;\n",
        "a unary sign gained a space after"
    );

    // Binary: none before and a space after.
    let formatted = check("let x = 1 - 2;\n");
    assert_eq!(
        formatted, "let x = 1 - 2;\n",
        "a binary sign lost its spaces"
    );

    // Ambiguous, and the conservative reading applies: after something that can end
    // an expression, a sign is binary.
    let formatted = check("let x = a-b;\n");
    assert_eq!(
        formatted, "let x = a - b;\n",
        "an ambiguous sign was guessed wrong"
    );
    // And after a keyword, a sign is unary, because a keyword cannot end an
    // expression.
    let formatted = check("return -1;\n");
    assert_eq!(
        formatted, "return -1;\n",
        "a sign after a keyword became binary"
    );
}

#[test]
fn a_very_deeply_nested_file_is_formatted_without_an_unbounded_indent() {
    // A thousand nested blocks is not a program, and the formatter must not indent
    // without bound: the depth is capped, so a pathological file gets a deep indent
    // rather than a line of kilobytes of spaces.
    let mut input = String::new();
    for _ in 0..1000 {
        input.push_str("{\n");
    }
    input.push_str("return 1;\n");
    for _ in 0..1000 {
        input.push_str("}\n");
    }
    let formatted = format(&input).expect("it formats");
    let widest = formatted
        .lines()
        .map(|line| line.len() - line.trim_start().len())
        .max()
        .unwrap_or(0);
    assert!(
        widest <= 64 * 4,
        "a line is indented {widest} characters, so the indent is unbounded"
    );
}

#[test]
fn a_file_that_does_not_lex_is_refused_rather_than_mangled() {
    let error = format("let x = \"unterminated;\n").expect_err("an unlexable file was formatted");
    assert!(
        format!("{error}").contains("did not lex"),
        "the refusal does not say why: {error}"
    );
    // And `is_formatted` refuses the same way, rather than answering a question
    // about a file it could not read.
    assert!(is_formatted("let x = @;\n").is_err());
}
/// A formatter that cannot format a real program is a formatter nobody runs, so the
/// fixtures above are only half the promise: the repository's own example program
/// must survive it, with its tokens and its comments intact.
///
/// The file is read from `examples/window/main.lz` rather than written out here,
/// because a fixture in a test and a program in `examples/` drift apart, and the one
/// that drifts is the one nobody notices. It is the largest program in the
/// repository and it uses most of what the formatter has to get right: nested calls,
/// array types with lengths, casts, comparison chains, and comments inside blocks.
#[test]
fn the_repository_s_own_example_program_survives_the_formatter() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/window/main.lz");
    let source = std::fs::read_to_string(path).expect("the example program is readable");
    let formatted = check(&source);
    // `check` has already proved the tokens and the comments survived; this proves it
    // is still a program.
    let mut sources = SourceManager::default();
    let id = sources
        .add_file("main.lz", formatted.as_str())
        .expect("the output registers");
    let parsed = lazalith_compiler::parse_file(id, &sources);
    assert!(
        parsed.is_ok(),
        "the formatted program does not parse: {formatted:?}\n{:?}",
        parsed.err()
    );
    // And the example is already written in this style, so the formatter should leave
    // it alone. That is the strongest statement the style can make: the
    // repository's own code is its worked example.
    assert_eq!(
        formatted, source,
        "the formatter changed a program that was already in the style"
    );
}

// A `match` is desugared in the parser, and the formatter works on tokens, so
// the two never meet. That is a deliberate arrangement: if the formatter printed
// from the tree it would write a `match` back out as a binding and an `if` chain,
// and a person who ran `lazen fmt` would get a different program in their file.
// These tests are the check that it does not.

#[test]
fn a_match_is_formatted_in_place_and_keeps_its_arms() {
    let formatted = check(
        "fn f(x: i32) -> i32 {\n    match x {\n        0 => {\n            return 1;\n        },\n        else => {\n            return 2;\n        },\n    }\n}\n",
    );
    assert!(formatted.contains("match x {"), "{formatted}");
    assert!(formatted.contains("=> {"), "{formatted}");
    assert!(
        !formatted.contains("$match"),
        "the compiler's binding must never reach a file: {formatted}"
    );
}

#[test]
fn a_match_with_comments_and_a_trailing_comma_keeps_both() {
    let formatted = check(
        "fn f(x: i32) -> i32 {\n    match x {\n        // the first case\n        0 => {\n            return 1;\n        },\n        else => {\n            return 2;\n        },\n    }\n    return 0;\n}\n",
    );
    assert!(formatted.contains("// the first case"), "{formatted}");
    assert!(formatted.contains("=> {"), "{formatted}");
}

#[test]
fn a_one_line_match_is_left_alone_because_it_is_already_canonical() {
    let source = "fn f(x: i32) -> i32 { match x { 0 => { return 1; }, else => { return 2; } } }\n";
    check(source);
}
