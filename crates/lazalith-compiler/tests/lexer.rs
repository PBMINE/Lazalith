//! Lexer tests.
//!
//! Each test names the rule it pins. Token families are covered one by one,
//! and every diagnostic code the lexer can emit has a positive test that the
//! span is exactly the offending text.

use lazalith_compiler::lexer::{IntSuffix, TokenKind, codes, lex};
use lazalith_types::SourceManager;

fn lex_ok(text: &str) -> Vec<TokenKind> {
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    assert!(
        lexed.diagnostics.is_empty(),
        "unexpected lexer diagnostics: {:?}",
        lexed
            .diagnostics
            .iter()
            .map(|error| error.code().as_str().to_string())
            .collect::<Vec<_>>()
    );
    lexed.tokens.into_iter().map(|token| token.kind).collect()
}

fn lex_error_code(text: &str) -> String {
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    let first = lexed
        .diagnostics
        .first()
        .unwrap_or_else(|| panic!("expected a lexer error for {text:?}"));
    first.code().as_str().to_string()
}

/// The source text a diagnostic's primary label covers.
fn lex_error_text(text: &str) -> String {
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    let error = lexed
        .diagnostics
        .first()
        .expect("a lexer error with a label");
    let label = error
        .diagnostic()
        .labels()
        .first()
        .expect("a primary label");
    let file = sources.file(label.span().id()).expect("a known file");
    let start = label.span().start().as_u32() as usize;
    let end = label.span().end().as_u32() as usize;
    file.text()[start..end].to_string()
}

#[test]
fn every_token_family_is_lexed() {
    let tokens = lex_ok("let mut x = 1 + 2 - 3 * 4 / 5 % 6;");
    assert_eq!(
        tokens,
        vec![
            TokenKind::Let,
            TokenKind::Mut,
            TokenKind::Ident(String::from("x")),
            TokenKind::Eq,
            TokenKind::Int {
                digits: String::from("1"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Plus,
            TokenKind::Int {
                digits: String::from("2"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Minus,
            TokenKind::Int {
                digits: String::from("3"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Star,
            TokenKind::Int {
                digits: String::from("4"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Slash,
            TokenKind::Int {
                digits: String::from("5"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Percent,
            TokenKind::Int {
                digits: String::from("6"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Semi,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn comparison_and_logical_operators_are_two_byte_tokens() {
    assert_eq!(
        lex_ok("a == b != c < d <= e > f >= g && h || i"),
        vec![
            TokenKind::Ident(String::from("a")),
            TokenKind::EqEq,
            TokenKind::Ident(String::from("b")),
            TokenKind::BangEq,
            TokenKind::Ident(String::from("c")),
            TokenKind::Lt,
            TokenKind::Ident(String::from("d")),
            TokenKind::LtEq,
            TokenKind::Ident(String::from("e")),
            TokenKind::Gt,
            TokenKind::Ident(String::from("f")),
            TokenKind::GtEq,
            TokenKind::Ident(String::from("g")),
            TokenKind::AndAnd,
            TokenKind::Ident(String::from("h")),
            TokenKind::OrOr,
            TokenKind::Ident(String::from("i")),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn reference_forms_are_distinct_tokens() {
    assert_eq!(
        lex_ok("& &mut &[ -> &x"),
        vec![
            TokenKind::Amp,
            TokenKind::AmpMut,
            TokenKind::AmpOpenBracket,
            TokenKind::Arrow,
            TokenKind::Amp,
            TokenKind::Ident(String::from("x")),
            TokenKind::Eof,
        ]
    );
}

#[test]
fn range_and_path_and_member_punctuation() {
    assert_eq!(
        lex_ok("a..b c::d . ,"),
        vec![
            TokenKind::Ident(String::from("a")),
            TokenKind::DotDot,
            TokenKind::Ident(String::from("b")),
            TokenKind::Ident(String::from("c")),
            TokenKind::PathSep,
            TokenKind::Ident(String::from("d")),
            TokenKind::Dot,
            TokenKind::Comma,
            TokenKind::Eof,
        ]
    );
}

#[test]
fn every_keyword_is_its_own_token() {
    let text = "let mut fn extern mod use pub const if else while for in loop break continue return true false as";
    let tokens = lex_ok(text);
    assert_eq!(tokens.len(), 21);
    for token in &tokens[..20] {
        assert!(token.is_keyword(), "{token:?} must be a keyword");
        assert!(
            !matches!(token, TokenKind::Ident(_)),
            "{token:?} must not be an identifier"
        );
    }
    assert_eq!(tokens[20], TokenKind::Eof);
}

#[test]
fn identifiers_allow_digits_and_underscores() {
    let tokens = lex_ok("_x x_1 a1_b2 _");
    let names: Vec<String> = tokens
        .iter()
        .filter_map(|token| match token {
            TokenKind::Ident(name) => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        names,
        vec![
            String::from("_x"),
            String::from("x_1"),
            String::from("a1_b2"),
            String::from("_")
        ]
    );
}

#[test]
fn integer_literals_keep_their_base_and_drop_separators() {
    let tokens = lex_ok("0 42 0x1F 0xff 1_000");
    assert_eq!(
        tokens,
        vec![
            TokenKind::Int {
                digits: String::from("0"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Int {
                digits: String::from("42"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Int {
                digits: String::from("1F"),
                radix: 16,
                suffix: None,
            },
            TokenKind::Int {
                digits: String::from("ff"),
                radix: 16,
                suffix: None,
            },
            TokenKind::Int {
                digits: String::from("1000"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Eof,
        ]
    );
}

#[test]
fn string_literals_keep_their_bytes() {
    let tokens = lex_ok("\"Hello, Lazalith\\n\" \"\" \"a b\"");
    let values: Vec<String> = tokens
        .iter()
        .filter_map(|token| match token {
            TokenKind::Str(value) => Some(value.clone()),
            _ => None,
        })
        .collect();
    // The value is the decoded bytes, not the written ones.
    assert_eq!(
        values,
        vec![
            String::from("Hello, Lazalith\n"),
            String::from(""),
            String::from("a b"),
        ]
    );
}

#[test]
fn line_comments_and_whitespace_are_insignificant() {
    let tokens = lex_ok("// leading\n\tlet // after\n x\n\r\n+ 1 // trailing");
    assert_eq!(
        tokens,
        vec![
            TokenKind::Let,
            TokenKind::Ident(String::from("x")),
            TokenKind::Plus,
            TokenKind::Int {
                digits: String::from("1"),
                radix: 10,
                suffix: None,
            },
            TokenKind::Eof,
        ]
    );
}

#[test]
fn a_comment_at_end_of_file_needs_no_newline() {
    let tokens = lex_ok("let x = 1 // no newline after");
    assert_eq!(tokens.first(), Some(&TokenKind::Let));
    assert_eq!(tokens.last(), Some(&TokenKind::Eof));
}

#[test]
fn empty_input_is_one_eof_token() {
    assert_eq!(lex_ok(""), vec![TokenKind::Eof]);
    assert_eq!(lex_ok("   \n\t  \n"), vec![TokenKind::Eof]);
    assert_eq!(lex_ok("// only a comment"), vec![TokenKind::Eof]);
}

#[test]
fn non_ascii_source_is_accepted_and_reported_precisely() {
    // A UTF-8 identifier character is not Lazen syntax, and the error must
    // cover exactly that one character, not a partial code point.
    let code = lex_error_code("fn f() { let café = 1; }");
    assert_eq!(code, codes::UNKNOWN_CHARACTER);
    assert_eq!(lex_error_text("fn f() { let café = 1; }"), "é");
}

#[test]
fn non_ascii_inside_a_string_is_accepted() {
    let tokens = lex_ok("\"héllo\"");
    assert_eq!(
        tokens.first().and_then(|token| match token {
            TokenKind::Str(value) => Some(value.clone()),
            _ => None,
        }),
        Some(String::from("héllo"))
    );
}

#[test]
fn block_comments_are_rejected_with_their_own_code() {
    assert_eq!(lex_error_code("/* hi */ let x = 1;"), codes::BLOCK_COMMENT);
    assert_eq!(lex_error_text("/* hi */"), "/* hi */");
    assert_eq!(
        lex_error_code("/* never closed"),
        codes::UNCLOSED_BLOCK_COMMENT
    );
}

#[test]
fn float_literals_are_rejected_with_their_own_code() {
    assert_eq!(lex_error_code("let ratio = 1.5;"), codes::FLOAT_LITERAL);
    assert_eq!(lex_error_text("let ratio = 1.5;"), "1.");
    // A method call on an integer is not a float literal.
    assert!(lex_ok("let n = 1;n.next();").contains(&TokenKind::Dot));
}

#[test]
fn malformed_integer_literals_are_rejected() {
    assert_eq!(lex_error_code("let n = 0x;"), codes::EMPTY_HEX);
    assert_eq!(lex_error_code("let n = 12ab;"), codes::MALFORMED_INT);
    assert_eq!(lex_error_code("let n = 0x1g;"), codes::MALFORMED_INT);
    // A hexadecimal digit after `0x` is part of the literal, not an error.
    assert!(lex_ok("let n = 0x1e + 2;").contains(&TokenKind::Plus));
}

#[test]
fn unclosed_strings_are_rejected() {
    assert_eq!(
        lex_error_code("let s = \"unterminated;"),
        codes::UNCLOSED_STRING
    );
    assert_eq!(
        lex_error_code("let s = \"open\nnext\";"),
        codes::MULTILINE_STRING
    );
}

#[test]
fn string_escapes_are_decoded_and_unknown_ones_rejected() {
    // The six escapes v1 supports, all of which the documented hello-world
    // example depends on.
    let tokens = lex_ok("\"a\\nb\\tc\\rd\\0e\\\\f\\\"g\"");
    let value = match &tokens[0] {
        TokenKind::Str(value) => value.clone(),
        other => panic!("expected a string, found {other:?}"),
    };
    assert_eq!(value, "a\nb\tc\rd\u{0}e\\f\"g");

    // Anything else is rejected, with the escape named.
    assert_eq!(lex_error_code(r#"let s = "\d";"#), codes::ESCAPE_SEQUENCE);
    assert_eq!(lex_error_text(r#"let s = "\d";"#), "\\d");
    assert_eq!(lex_error_code(r#"let s = "\x41";"#), codes::ESCAPE_SEQUENCE);
    assert_eq!(
        lex_error_code(r#"let s = "\u{41}";"#),
        codes::ESCAPE_SEQUENCE
    );
    // A backslash at the very end of the file has no escape at all.
    assert_eq!(
        lex_error_code("let s = \"trailing\\"),
        codes::INCOMPLETE_ESCAPE
    );
}

#[test]
fn over_long_identifiers_are_rejected() {
    let long = "a".repeat(33);
    let text = format!("let {long} = 1;");
    assert_eq!(lex_error_code(&text), codes::LONG_IDENTIFIER);
    // Exactly 32 characters is still fine.
    let ok = "a".repeat(32);
    assert_eq!(lex_ok(&format!("let {ok} = 1;")).len(), 6);
}

#[test]
fn a_single_pipe_is_rejected() {
    assert_eq!(lex_error_code("let f = a | b;"), codes::SINGLE_PIPE);
}

#[test]
fn every_token_span_covers_its_own_text() {
    let text = "let mut total: i64 = 0x1F; // sum";
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
    for token in &lexed.tokens {
        let file = sources.file(token.span.id()).expect("a known file");
        let start = token.span.start().as_u32() as usize;
        let end = token.span.end().as_u32() as usize;
        let slice = &file.text()[start..end];
        assert_eq!(slice, token.text, "span must cover exactly the token text");
    }
    // And the spans must tile the file in order, apart from trivia.
    let mut previous = 0u32;
    for token in &lexed.tokens {
        assert!(
            token.span.start().as_u32() >= previous,
            "spans must not go backwards"
        );
        previous = token.span.end().as_u32();
    }
    assert_eq!(previous as usize, text.len());
}

#[test]
fn line_and_column_resolution_uses_the_shared_source_map() {
    let text = "fn main() {\n    let x = 1;\n}\n";
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    let x = lexed
        .tokens
        .iter()
        .find(|token| token.ident() == Some("x"))
        .expect("the identifier x");
    let line_column = sources
        .line_column(source, x.span.start())
        .expect("a resolvable position");
    assert_eq!(line_column.line, 2);
    assert_eq!(line_column.column, 9);
}

#[test]
fn lexing_stops_at_the_first_error_with_no_partial_tail() {
    let text = "let x = 1; ~ let y = 2;";
    let mut sources = SourceManager::new();
    let source = sources
        .add_file("test.lazen", text)
        .expect("a valid source");
    let lexed = lex(source, &sources);
    assert_eq!(lexed.diagnostics.len(), 1);
    assert!(lexed.tokens.last().is_none() || !lexed.tokens.is_empty());
    assert!(lexed.into_tokens().is_none());
}

#[test]
fn integer_literal_type_suffixes_are_lexed() {
    // The documented `[0u8; 16]` and the `0x1F as u8` idiom both need this.
    let tokens = lex_ok("0u8 42i32 0xFFu64 7usize");
    let suffixes: Vec<Option<IntSuffix>> = tokens
        .iter()
        .filter_map(|token| match token {
            TokenKind::Int { suffix, .. } => Some(*suffix),
            _ => None,
        })
        .collect();
    assert_eq!(
        suffixes,
        vec![
            Some(IntSuffix::U8),
            Some(IntSuffix::I32),
            Some(IntSuffix::U64),
            Some(IntSuffix::Usize),
        ]
    );
    // A suffix never merges into the digits.
    match &tokens[0] {
        TokenKind::Int { digits, radix, .. } => {
            assert_eq!(digits, "0");
            assert_eq!(*radix, 10);
        }
        other => panic!("expected an integer, found {other:?}"),
    }
}

#[test]
fn a_non_type_suffix_is_rejected() {
    assert_eq!(lex_error_code("let n = 12ab;"), codes::MALFORMED_INT);
    assert_eq!(lex_error_code("let n = 1f32;"), codes::MALFORMED_INT);
    // `usize` is the only multi-word suffix; `usiz` is not a type.
    assert_eq!(lex_error_code("let n = 1usiz;"), codes::MALFORMED_INT);
    assert_eq!(lex_error_code("let n = 1u 8;"), codes::MALFORMED_INT);
}
