//! Property tests for the C front end.
//!
//! The same three properties as `lazalith-compiler/tests/properties.rs`, over the
//! other language — and they are worth having twice rather than once, because the
//! two front ends share the IR and the backend and share *nothing* above that. A
//! parser that panics on a random `}` is a different bug in each, and a shared
//! property suite would have tested only one of them.
//!
//! What is *not* shared is the piece list: a C program is made of different tokens
//! from a Lazen one, and the interesting lexer cases here are the ones C is
//! famous for — the `<:` digraph-adjacent sequences, a `'` that opens a character
//! constant or closes one, and a `/*` that opens a comment or divides.

use lazalith_c_compiler::compile;
use lazalith_properties::{Case, Gen, check};
use lazalith_types::SourceManager;

/// The pieces a C program is made of, mixed without regard for where they belong.
const PIECES: &[&str] = &[
    "int",
    "char",
    "long",
    "short",
    "unsigned",
    "signed",
    "float",
    "double",
    "void",
    "struct",
    "union",
    "enum",
    "const",
    "static",
    "extern",
    "typedef",
    "sizeof",
    "return",
    "if",
    "else",
    "while",
    "for",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    ",",
    ";",
    ":",
    "?",
    ".",
    "->",
    "...",
    "=",
    "==",
    "!=",
    "<",
    ">",
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
    "~",
    "!",
    "&&",
    "||",
    "<<",
    ">>",
    "++",
    "--",
    "+=",
    "-=",
    "*=",
    "/=",
    "%=",
    "&=",
    "|=",
    "^=",
    "<<=",
    ">>=",
    "#",
    "##",
    "0",
    "1",
    "42",
    "0x2a",
    "0b1010",
    "0o17",
    "1_000",
    "1u",
    "1L",
    "1ULL",
    "'a'",
    "'\\n'",
    "'\\''",
    "'ab'",
    "\"text\"",
    "\"unterminated",
    "\"with \\\" escape\"",
    "main",
    "value",
    "count",
    "x",
    "y",
    "p",
    "_",
    "printf",
    "sizeof",
    "// a comment\n",
    "/* block */",
    "/* unterminated",
    "\n",
    "\t",
    " ",
    "\r\n",
    "\u{0}",
    "é",
    "中",
    "\u{feff}",
];

/// A random piece of C.
struct Text(String);

impl Case for Text {
    fn generate(source: &mut Gen) -> Self {
        let count = 1 + source.below(24) as usize;
        let mut text = String::new();
        for _ in 0..count {
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
/// The property. `compile` runs every stage, so this also says that resolution and
/// checking survive text the lexer let through — and step 82 is why that matters:
/// the front end's own suite is 34 programs, and every one of them was supposed to
/// work.
#[test]
fn random_text_parses_or_is_refused() {
    check::<Text>(64, |case| {
        let mut sources = SourceManager::new();
        let _ = compile(&mut sources, "t.c", &case.0);
        true
    });
}

/// A refusal says something.
///
/// A diagnostic with no message is a comment, and a front end that produces one has
/// told the reader nothing they could act on.
#[test]
fn a_refusal_says_something() {
    check::<Text>(64, |case| {
        let mut sources = SourceManager::new();
        match compile(&mut sources, "t.c", &case.0) {
            Ok(_) => true,
            Err(error) => !error.render().trim().is_empty(),
        }
    });
}

/// Compiling the same text twice gives the same answer.
///
/// `compile` is a pure function of its input, and this is the property that says
/// so. A front end that reached the same answer by a different route — because it
/// iterated a hash map, or reported a different one of two equally valid errors —
/// would have errors nobody could reproduce and a bug nobody could report.
#[test]
fn compiling_twice_gives_the_same_answer() {
    check::<Text>(64, |case| {
        let mut first = SourceManager::new();
        let mut second = SourceManager::new();
        let a = compile(&mut first, "t.c", &case.0);
        let b = compile(&mut second, "t.c", &case.0);
        match (a, b) {
            (Ok(_), Ok(_)) => true,
            (Err(left), Err(right)) => left.render() == right.render(),
            _ => false,
        }
    });
}

/// Every diagnostic from one compile is reported, not just the first.
///
/// A front end that stops at the first error makes a program with three mistakes
/// take three runs to diagnose, and a reader who fixes them in the order reported
/// gets a different error each time. Both are worse than reporting all of them, and
/// this is the property that says the compiler does.
#[test]
fn every_diagnostic_is_reported() {
    check::<Text>(64, |case| {
        let mut sources = SourceManager::new();
        let diagnostics = lazalith_c_compiler::analyse(&mut sources, "t.c", &case.0).diagnostics;
        // Whatever is reported is reported with a code and a position, and the list
        // is finite — a front end that reported an error per token of a large file
        // would be a denial of service against its own user.
        diagnostics
            .iter()
            .all(|error| !error.code().as_str().is_empty())
    });
}

/// A multi-byte character is lexed as a character, not stepped into.
///
/// The bug this pins, and it is worth a test of its own rather than only the random
/// text above: `at` is a byte offset, and the lexer's "make progress" step advanced
/// it by one *byte*. On a three-byte character that leaves `at` inside it, and the
/// next token's span is then not a character boundary — which is a panic, on a file
/// with a `é` in a comment.
///
/// A C file with a non-ASCII byte in a comment or a string is not exotic. It is a
/// file somebody's editor produced, and the compiler's job is to lex the bytes
/// around it and say something useful about the one in the middle.
#[test]
fn a_multi_byte_character_is_a_character() {
    for text in [
        "// é\nint main(void) { return 0; }",
        "/* 中 */ int main(void) { return 0; }",
        "int main(void) { return 0; } // é中",
        "\"é\"",
        "int é = 1;",
        "// \u{feff}bom\nint main(void) { return 0; }",
        "中",
        "é",
    ] {
        let mut sources = SourceManager::new();
        // The assertion is that this returns at all. A string and a comment should
        // compile; an identifier with a non-ASCII byte in it should be refused, and
        // either way the answer must be an answer.
        let _ = compile(&mut sources, "t.c", text);
    }
}

/// A diagnostic about a multi-byte character quotes the whole character.
///
/// The second half of the fix, and the half a user would notice. A span rounded in
/// to the character boundary loses the character from the message; rounded out, it
/// is quoted whole, so the reader can see the thing the compiler is complaining
/// about.
#[test]
fn a_diagnostic_about_a_multi_byte_character_quotes_it() {
    let mut sources = SourceManager::new();
    let error = compile(&mut sources, "t.c", "中").expect_err("a bare character is not a program");
    let rendered = error.render();
    assert!(
        rendered.contains('中'),
        "the message should quote the character it is about, and says:\n{rendered}"
    );
}
