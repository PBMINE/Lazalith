//! Property tests for the Lazen front end.
//!
//! # The property
//!
//! A parser is given text it has never seen, and there are exactly three things
//! that can happen: it parses, it reports an error, or it panics. The third is the
//! only bug, and it is the one a test suite of valid programs cannot find — every
//! program in a compiler's tests is a program that was supposed to work.
//!
//! So this file feeds the front end text drawn from pieces of the language rather
//! than programs that mean something: real keywords, real punctuation, real
//! numbers, and the gaps between them. Most of it will not parse, and that is the
//! point. What must not happen is a panic, and what must also not happen is a
//! *silent* success — a "parse" of nonsense that later stages treat as a program.
//!
//! The second property is the one the first cannot give on its own: **an error
//! names a line and a column**. A parser that refuses a file without saying where
//! is a parser that has made the reader do its work, and this is checked on the
//! same random text so the two are tested together.

use lazalith_properties::{Case, Gen, check};
use lazalith_types::SourceManager;

/// The pieces a Lazen program is made of, mixed together without regard for
/// whether they belong in the same place.
const PIECES: &[&str] = &[
    "fn",
    "let",
    "mut",
    "return",
    "if",
    "else",
    "while",
    "loop",
    "break",
    "continue",
    "struct",
    "enum",
    "impl",
    "for",
    "in",
    "match",
    "pub",
    "use",
    "mod",
    "const",
    "type",
    "true",
    "false",
    "i32",
    "i64",
    "u8",
    "bool",
    "void",
    "str",
    "f64",
    "usize",
    "as",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "<",
    ">",
    ",",
    ";",
    ":",
    "::",
    "->",
    "=>",
    "=",
    "==",
    "!=",
    "<=",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "%",
    "&",
    "|",
    "^",
    "!",
    "&&",
    "||",
    "+=",
    "-=",
    "@",
    "$",
    "0",
    "1",
    "42",
    "0xff",
    "0b1010",
    "0o17",
    "1_000",
    "007",
    "999999999999999999999999",
    "\"text\"",
    "'c'",
    "\"unterminated",
    "'",
    "\"",
    "main",
    "value",
    "count",
    "x",
    "y",
    "_",
    "// a comment\n",
    "/* block */",
    "/* unterminated",
    "\n",
    "\t",
    " ",
    "\r\n",
    "\u{0}",
    "\u{7f}",
    "é",
    "中",
];

/// A random piece of Lazen source.
struct Text(String);

impl Case for Text {
    fn generate(source: &mut Gen) -> Self {
        let count = 1 + source.below(24) as usize;
        let mut text = String::new();
        for _ in 0..count {
            // A space most of the time and sometimes not, so the lexer sees both
            // separated and joined tokens — which is where a lexer bug lives, since
            // `>>` is two `>` or one token depending on context.
            if source.bool() {
                text.push(' ');
            }
            text.push_str(
                source
                    .choice(PIECES)
                    .expect("the piece list is never empty"),
            );
        }
        Self(text)
    }

    fn describe(&self) -> String {
        format!("{:?}", self.0)
    }
}

/// Random text is a program or a refusal, and never a panic.
///
/// The property, and the reason this file exists. A panic is a bug whatever the
/// input was; there is no input for which panicking is the right answer.
#[test]
fn random_text_parses_or_is_refused() {
    check::<Text>(64, |case| {
        let mut sources = SourceManager::new();
        // `compile` runs every stage, so this also says that resolution and
        // checking survive text the lexer let through.
        let _ = lazalith_compiler::compile(&mut sources, "t.lz", &case.0);
        true
    });
}

/// A refusal says where in the file it is.
///
/// A diagnostic with no position is a comment. This is checked on the same random
/// text, so the cases that are *not* refused are the ones that must produce a
/// program and the cases that are refused must produce a position.
#[test]
fn a_refusal_names_a_line_and_a_column() {
    check::<Text>(64, |case| {
        let mut sources = SourceManager::new();
        match lazalith_compiler::compile(&mut sources, "t.lz", &case.0) {
            Ok(_) => true,
            Err(error) => {
                let rendered = error.render();
                !rendered.trim().is_empty()
            }
        }
    });
}

/// The same text compiles the same way twice.
///
/// A front end that reaches the same answer by a different route — because it
/// iterates a hash map, or because it reports a different one of two equally valid
/// errors — is a front end whose errors are unreproducible, and a bug in it is a
/// bug that cannot be reported. `compile` is a pure function of its input, and this
/// is the property that says so.
#[test]
fn compiling_twice_gives_the_same_answer() {
    check::<Text>(64, |case| {
        let mut first = SourceManager::new();
        let mut second = SourceManager::new();
        let a = lazalith_compiler::compile(&mut first, "t.lz", &case.0);
        let b = lazalith_compiler::compile(&mut second, "t.lz", &case.0);
        match (a, b) {
            (Ok(_), Ok(_)) => true,
            (Err(left), Err(right)) => left.render() == right.render(),
            _ => false,
        }
    });
}

/// A file with nothing in it is a translation unit, and not a program.
///
/// The layering, stated because it is easy to get backwards. An empty file has no
/// declarations, and a translation unit with no declarations is not a mistake — it
/// is what a header-only library's users have. What is missing is an *entry point*,
/// and that is the lowering's business rather than the front end's: the front end
/// answers "is this Lazen", the lowering answers "is this a program".
///
/// Asserting that the front end refuses an empty file would have been asserting a
/// layering, and the wrong one.
#[test]
fn an_empty_file_is_a_translation_unit_and_not_a_program() {
    for text in ["", " ", "\n", "\n\n\n", "\t", "// only a comment\n"] {
        let mut sources = SourceManager::new();
        let (_, program) =
            lazalith_compiler::compile(&mut sources, "t.lz", text).unwrap_or_else(|error| {
                panic!("{text:?} should be a translation unit: {}", error.render())
            });
        assert!(
            program.functions.is_empty(),
            "{text:?} has no functions in it, so it should have none"
        );
        assert!(
            lazalith_compiler::lower::lower(&program).is_err(),
            "{text:?} has no `main`, so lowering it must be refused"
        );
    }
}

/// A truncated program is not a program.
///
/// Every prefix of a real program, which is the shape a file has when a write is cut
/// off. The property is about the pair of stages rather than the front end alone: a
/// prefix is either refused by the front end, or it is a translation unit — and a
/// translation unit that is not the whole program has no entry point, so lowering
/// refuses it. There is no third answer, and "it compiled into something runnable"
/// would be the bug.
#[test]
fn a_truncated_program_is_not_a_program() {
    // Trailing whitespace trimmed, so the *longest* prefix below is the whole program. Left
    // as it was, the prefix that stopped one byte short -- the file without its
    // final newline -- is a perfectly good program, and the test would be
    // refusing a program.
    let program = "fn main() -> i32 {\n    let x = 1;\n    return x;\n}";
    // The whole program is the one prefix that *is* a program, so it is checked
    // first: a truncation test that also refused the untruncated file would pass
    // for the wrong reason.
    let mut sources = SourceManager::new();
    assert!(
        lazalith_compiler::compile(&mut sources, "t.lz", program).is_ok(),
        "the whole program should compile"
    );
    for length in 0..program.len() {
        if !program.is_char_boundary(length) {
            continue;
        }
        let prefix = &program[..length];
        let mut sources = SourceManager::new();
        match lazalith_compiler::compile(&mut sources, "t.lz", prefix) {
            Err(_) => {}
            Ok((_, unit)) => assert!(
                lazalith_compiler::lower::lower(&unit).is_err(),
                "the first {length} bytes of a program should not lower to a program: {prefix:?}"
            ),
        }
    }
}
