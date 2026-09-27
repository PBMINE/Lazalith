//! C tokens.
//!
//! # What this lexer knows about C that a general lexer does not
//!
//! Three things, and each of them is a place where a general lexer gets C
//! wrong:
//!
//! - A **maximal munch** is a rule, not a convenience. `a+++b` is `a++ + b`, not
//!   `a + ++b`, because C's grammar has no `++` operator that could be meant
//!   there, and a lexer that prefers the shorter match produces a program that
//!   does not compile for a reason that is not in the source. Every operator is
//!   therefore spelled out and matched longest first, and `--` is recognised
//!   before `-`.
//! - A **header name** is not a token. `#include <stdio.h>` has a header name
//!   that is neither an identifier nor a string, and treating it as either makes
//!   the include either unresolvable or silently wrong. This lexer never
//!   resolves includes — that is a later stage, which needs a search path — but
//!   it does recognise the line so the lexer can say "no such header" instead
//!   of "unterminated string".
//! - A **preprocessor directive** is a line, and the tokens after `#` are not
//!   C tokens at all. The lexer keeps each directive on its own line so a
//!   directive's arguments are never spliced into the token stream, and so
//!   `#define`d text is not lexed as though it were program text. Macro
//!   *expansion* is not implemented: a `#define`d name is left alone and the
//!   program that uses it fails in the resolver, which is where a name with no
//!   definition belongs.
//!
//! # Numbers
//!
//! An integer constant is read to its end, and the *suffix* is kept in the
//! token rather than resolved here, because a suffix's effect depends on the
//! base: `0x10` is sixteen, and `0x10u` is an `unsigned int` of value sixteen,
//! while `010` is eight. Deciding that in the lexer would mean deciding the
//! type system in the lexer. This lexer records the digits, the base and the
//! suffix, and [`ctypes`](crate::ctypes) works out what the constant is.
//!
//! A floating constant is *recognised* so it can be refused with "this machine
//! has no floating point" rather than with a parse error three tokens later.
//! Reading past the number to find that out is worth the two lines.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lazalith_diagnostics::Diagnostic;
use lazalith_types::{ByteOffset, SourceId, SourceManager, SourceSpan};

/// C's diagnostic codes for this stage.
pub mod codes {
    /// A character that is not part of any token.
    pub const UNKNOWN_CHARACTER: &str = "C0101";
    /// A string that reaches the end of the file before its closing quote.
    pub const UNCLOSED_STRING: &str = "C0102";
    /// A string that runs past the end of a line.
    pub const MULTILINE_STRING: &str = "C0103";
    /// A backslash escape that is not one C defines.
    pub const INCOMPLETE_ESCAPE: &str = "C0104";
    /// An escape sequence with a value that does not fit in a `char`.
    pub const ESCAPE_SEQUENCE: &str = "C0105";
    /// An integer constant with digits that are not valid in its base.
    pub const MALFORMED_INT: &str = "C0110";
    /// A hexadecimal constant with no digits.
    pub const EMPTY_HEX: &str = "C0111";
    /// A floating constant, which this machine cannot represent.
    pub const FLOAT_LITERAL: &str = "C0120";
    /// A number whose exponent has no digits.
    pub const MALFORMED_EXPONENT: &str = "C0121";
    /// A character constant with no characters.
    pub const EMPTY_CHARACTER: &str = "C0130";
    /// A character constant with more than one character in it.
    pub const LONG_CHARACTER: &str = "C0131";
    /// A comment that reaches the end of the file before its `*/`.
    pub const UNCLOSED_COMMENT: &str = "C0140";
    /// A header name with no closing `>`.
    pub const UNCLOSED_HEADER: &str = "C0150";
    /// A backslash-newline inside a string, which splices two lines.
    pub const LINE_SPLICE_IN_STRING: &str = "C0160";
    /// The file ended in the middle of a token.
    pub const UNEXPECTED_EOF: &str = "C0001";
}

/// What a token is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// A name, or a keyword: C spells keywords in the identifier space, so
    /// telling them apart is the parser's job and pretending otherwise here
    /// would make a variable named `int` lex differently from one named `i`.
    Identifier,
    /// An integer constant, with its base and suffix recorded.
    Integer,
    /// A floating constant, recognised only so it can be refused by name.
    Float,
    /// A character constant.
    Character,
    /// A string literal, with the value already unescaped.
    String,
    /// A punctuator, held as the exact source spelling.
    Punctuator,
    /// `#include` and its header name, on one line.
    HeaderName,
    /// A preprocessor directive line, not including the `#`.
    Directive,
    /// End of the file. There is always exactly one.
    EndOfFile,
}

/// One token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    /// What it is.
    pub kind: TokenKind,
    /// The text as written, for a punctuator and a directive.
    pub text: String,
    /// An identifier's name, or a string's unescaped value.
    ///
    /// A string keeps its *value* here rather than its spelling, so a
    /// comparison in the program is a comparison of the bytes the program asked
    /// for and not of how it spelled them.
    pub value: String,
    /// An integer's digits, and an integer's suffix.
    pub number: Number,
    /// A character's value.
    pub character: i64,
    /// Where it is.
    pub span: SourceSpan,
}

/// An integer constant, untyped.
///
/// The base and the suffix are both kept, because C's rule for what `0x10u`
/// means depends on both, and the type checker is the only stage that has the
/// rest of the type system to decide it with.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Number {
    /// The digits, as written, without a base prefix or a suffix.
    pub digits: String,
    /// The base: 10, 8 or 16.
    pub base: u32,
    /// The suffix, lowercased: `u`, `l`, `ul` and so on.
    pub suffix: String,
    /// Whether the value has an `f` or an `l` suffix, which makes it a
    /// floating constant.
    pub floating: bool,
}

/// Everything the lexer produced, including its failures.
#[derive(Clone, Debug)]
pub struct Lexed {
    /// The tokens, in order, ending with exactly one [`TokenKind::EndOfFile`].
    pub tokens: Vec<Token>,
    /// Every refusal, in the order they were found.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lexes one file.
///
/// The tokens are returned even when there are diagnostics, because a caller
/// that stops at the first bad token has to re-lex to find the second one, and
/// a front end that reports one error per run is a front end that makes a person
/// fix a mistake at a time.
pub fn lex(source: SourceId, sources: &SourceManager) -> Lexed {
    let text = sources
        .file(source)
        .map(|file| file.text())
        .unwrap_or_default();
    Lexer {
        source,
        sources,
        text,
        at: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
    }
    .run()
}

struct Lexer<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    text: &'a str,
    at: u32,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn run(mut self) -> Lexed {
        loop {
            self.skip_trivia();
            let start = self.at;
            if start >= self.len() {
                break;
            }
            let byte = self.byte(start);
            if byte == b'"' || byte == b'\'' {
                self.lex_quoted(start, byte);
            } else if byte.is_ascii_digit()
                || (byte == b'.'
                    && self
                        .peek(start + 1)
                        .is_some_and(|byte| byte.is_ascii_digit()))
            {
                self.lex_number(start);
            } else if is_identifier_start(byte) {
                self.lex_word(start);
            } else if byte == b'#' && self.at_line_start(start) {
                self.lex_directive(start);
            } else {
                self.lex_punctuator(start);
            }
            if self.at == start {
                // A one-byte advance is the only way this loop can end: every
                // branch either consumes at least one byte or reports and
                // consumes. Without it an unrecognised character would spin
                // forever, which is a hang a person cannot diagnose.
                self.at += 1;
            }
        }
        let end = self.len();
        self.tokens.push(Token {
            kind: TokenKind::EndOfFile,
            text: String::new(),
            value: String::new(),
            number: Number::default(),
            character: 0,
            span: self.span(end.saturating_sub(1), end),
        });
        Lexed {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn len(&self) -> u32 {
        self.text.len() as u32
    }

    fn byte(&self, at: u32) -> u8 {
        self.text.as_bytes().get(at as usize).copied().unwrap_or(0)
    }

    fn peek(&self, at: u32) -> Option<u8> {
        self.text.as_bytes().get(at as usize).copied()
    }

    /// A span, clamped into the file and taken through the source map.
    ///
    /// A `SourceSpan` is only ever built by the map that owns the file, because that is
    /// what validates it; a range that cannot be made valid falls back to a
    /// zero-length span at the file's start rather than being dropped, so a
    /// diagnostic never loses its location.
    fn span(&self, start: u32, end: u32) -> SourceSpan {
        let length = self.len();
        let start = start.min(length);
        let end = end.clamp(start, length);
        self.sources
            .source_span(self.source, ByteOffset::new(start), ByteOffset::new(end))
            .or_else(|_| {
                self.sources
                    .source_span(self.source, ByteOffset::new(0), ByteOffset::new(0))
            })
            .expect("a zero-length span at offset zero is always valid")
    }

    /// Whether `at` is the first thing on its line, ignoring whitespace.
    fn at_line_start(&self, at: u32) -> bool {
        let mut cursor = at;
        while cursor > 0 {
            cursor -= 1;
            match self.byte(cursor) {
                b' ' | b'\t' | b'\r' => {}
                b'\n' => return true,
                // A line-continued line is still the same line as far as the
                // preprocessor is concerned, which is the whole point of the
                // backslash.
                b'\\' if cursor > 0 && self.byte(cursor - 1) == b'\n' => {}
                _ => return false,
            }
        }
        true
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.byte(self.at) {
                b' ' | b'\t' | b'\r' | b'\n' => self.at += 1,
                b'\\' if self.byte(self.at + 1) == b'\n' => {
                    self.at += 2;
                    if self.byte(self.at) == b'\r' {
                        self.at += 1;
                    }
                }
                b'/' if self.byte(self.at + 1) == b'/' => {
                    while self.at < self.len() && self.byte(self.at) != b'\n' {
                        // A `//` comment can be continued, and then the next
                        // line is part of the comment rather than the program.
                        if self.byte(self.at) == b'\\' && self.byte(self.at + 1) == b'\n' {
                            self.at += 2;
                            if self.byte(self.at) == b'\r' {
                                self.at += 1;
                            }
                            continue;
                        }
                        self.at += 1;
                    }
                }
                b'/' if self.byte(self.at + 1) == b'*' => {
                    let start = self.at;
                    self.at += 2;
                    loop {
                        if self.at >= self.len() {
                            self.reject(
                                start,
                                self.len(),
                                codes::UNCLOSED_COMMENT,
                                "this comment is never closed",
                                "a `/*` comment runs until the first `*/`, and there is not one in the file",
                            );
                            return;
                        }
                        if self.byte(self.at) == b'*' && self.byte(self.at + 1) == b'/' {
                            self.at += 2;
                            break;
                        }
                        self.at += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn lex_word(&mut self, start: u32) {
        while self.at < self.len() && is_identifier_continue(self.byte(self.at)) {
            self.at += 1;
        }
        let value = self.slice(start, self.at).to_string();
        self.push(TokenKind::Identifier, start, self.at, value.clone(), value);
    }

    fn lex_number(&mut self, start: u32) {
        let mut base = 10;
        let mut digits = String::new();
        let mut floating = false;
        if self.byte(start) == b'0' && matches!(self.byte(start + 1), b'x' | b'X') {
            base = 16;
            self.at = start + 2;
            while self.at < self.len() {
                let byte = self.byte(self.at);
                if byte.is_ascii_hexdigit() || byte == b'\'' {
                    if byte != b'\'' {
                        digits.push(char::from(byte));
                    }
                    self.at += 1;
                } else {
                    break;
                }
            }
            if digits.is_empty() {
                self.reject(
                    start,
                    self.at,
                    codes::EMPTY_HEX,
                    "this hexadecimal constant has no digits",
                    "the `0x` prefix is not a number by itself; `0x0` is zero",
                );
            }
        } else if self.byte(start) == b'0'
            && matches!(self.byte(start + 1), b'b' | b'B')
            && self
                .peek(start + 2)
                .is_some_and(|byte| byte == b'0' || byte == b'1')
        {
            base = 2;
            self.at = start + 2;
            while self.at < self.len() && matches!(self.byte(self.at), b'0' | b'1' | b'\'') {
                if self.byte(self.at) != b'\'' {
                    digits.push(char::from(self.byte(self.at)));
                }
                self.at += 1;
            }
        } else {
            while self.at < self.len() {
                let byte = self.byte(self.at);
                if byte.is_ascii_digit() || byte == b'\'' {
                    if byte != b'\'' {
                        digits.push(char::from(byte));
                    }
                    self.at += 1;
                } else {
                    break;
                }
            }
            if self.byte(self.at) == b'.' {
                floating = true;
                self.at += 1;
                while self.at < self.len() && (self.byte(self.at).is_ascii_digit()) {
                    digits.push(char::from(self.byte(self.at)));
                    self.at += 1;
                }
            }
            if matches!(self.byte(self.at), b'e' | b'E')
                && (self
                    .peek(self.at + 1)
                    .is_some_and(|byte| byte.is_ascii_digit())
                    || matches!(
                        (self.peek(self.at + 1), self.peek(self.at + 2)),
                        (Some(b'+' | b'-'), Some(byte)) if byte.is_ascii_digit()
                    ))
            {
                floating = true;
                self.at += 1;
                if matches!(self.byte(self.at), b'+' | b'-') {
                    self.at += 1;
                }
                let exponent = self.at;
                while self.at < self.len() && self.byte(self.at).is_ascii_digit() {
                    self.at += 1;
                }
                if self.at == exponent {
                    self.reject(
                        start,
                        self.at,
                        codes::MALFORMED_EXPONENT,
                        "this exponent has no digits",
                        "an exponent is written with at least one digit: `1e3`, not `1e`",
                    );
                }
            }
            // A digit sequence with no `0` prefix is octal, and a digit that is
            // not an octal digit in an octal constant is not a C constant.
            if digits.len() > 1 && digits.starts_with('0') && !floating {
                base = 8;
                if let Some(bad) = digits.chars().find(|digit| !matches!(digit, '0'..='7')) {
                    let _ = bad;
                    self.reject(
                        start,
                        self.at,
                        codes::MALFORMED_INT,
                        "this octal constant has a digit that is not octal",
                        "a constant that starts with `0` is octal, so its digits are 0 to 7; write `0x` for hexadecimal",
                    );
                }
            }
        }
        let mut suffix = String::new();
        while self.at < self.len()
            && matches!(self.byte(self.at), b'u' | b'U' | b'l' | b'L' | b'f' | b'F')
        {
            let byte = self.byte(self.at);
            suffix.push(char::from(byte.to_ascii_lowercase()));
            if matches!(byte, b'f' | b'F') {
                floating = true;
            }
            self.at += 1;
        }
        if floating {
            self.reject(
                start,
                self.at,
                codes::FLOAT_LITERAL,
                "this machine has no floating point",
                "`float` and `double` are not representable, so this constant has no type; write it as an integer",
            );
        }
        self.tokens.push(Token {
            kind: TokenKind::Integer,
            text: self.slice(start, self.at).to_string(),
            value: String::new(),
            number: Number {
                digits,
                base,
                suffix,
                floating,
            },
            character: 0,
            span: self.span(start, self.at),
        });
    }

    fn lex_quoted(&mut self, start: u32, quote: u8) {
        let string = quote == b'"';
        self.at = start + 1;
        let mut value = String::new();
        let mut closed = false;
        while self.at < self.len() {
            let byte = self.byte(self.at);
            if byte == b'\n' {
                break;
            }
            if byte == quote {
                self.at += 1;
                closed = true;
                break;
            }
            if byte == b'\\' {
                match self.escape(start) {
                    Some(escaped) => value.push_str(&escaped),
                    None => break,
                }
                continue;
            }
            // A byte above 127 is a character in the file's encoding, and C
            // lets a string literal contain one. Taking it a byte at a time
            // keeps the value byte-exact, which is what a `char *` from a string
            // literal is supposed to be.
            if byte < 0x80 {
                value.push(char::from(byte));
                self.at += 1;
            } else {
                let width = utf8_width(byte);
                let end = (self.at + width).min(self.len());
                value.push_str(self.slice(self.at, end));
                self.at = end;
            }
        }
        if !closed {
            self.reject(
                start,
                self.at,
                if string {
                    codes::UNCLOSED_STRING
                } else {
                    codes::EMPTY_CHARACTER
                },
                if string {
                    "this string is never closed"
                } else {
                    "this character constant is never closed"
                },
                if string {
                    "a string literal ends at the closing `\"` on the same line"
                } else {
                    "a character constant ends at the closing `'` on the same line"
                },
            );
            return;
        }
        if string {
            self.push(TokenKind::String, start, self.at, value.clone(), value);
        } else {
            let mut characters = value.chars();
            let first = characters.next();
            let rest = characters.count();
            match (first, rest) {
                (None, _) => self.reject(
                    start,
                    self.at,
                    codes::EMPTY_CHARACTER,
                    "this character constant has no character in it",
                    "a character constant is written `'a'`; `''` is not a character",
                ),
                (Some(character), 0) => {
                    let code = i64::from(character as u32);
                    self.tokens.push(Token {
                        kind: TokenKind::Character,
                        text: self.slice(start, self.at).to_string(),
                        value: String::new(),
                        number: Number::default(),
                        character: code,
                        span: self.span(start, self.at),
                    });
                }
                (Some(_), _) => self.reject(
                    start,
                    self.at,
                    codes::LONG_CHARACTER,
                    "this character constant has more than one character in it",
                    "a character constant holds one character; a string of several is a string literal",
                ),
            }
        }
    }

    /// Reads one escape sequence, reporting it and returning `None` on a
    /// malformed one.
    fn escape(&mut self, start: u32) -> Option<String> {
        let escape_start = self.at;
        self.at += 1;
        let byte = self.byte(self.at);
        self.at += 1;
        let decoded = match byte {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'0'..=b'7' => {
                // An octal escape is up to three digits, and C says the value
                // must fit in a `char`; a fourth digit is not part of it.
                let mut value = i64::from(byte - b'0');
                let mut digits = 1;
                while digits < 3 && matches!(self.byte(self.at), b'0'..=b'7') {
                    value = value * 8 + i64::from(self.byte(self.at) - b'0');
                    self.at += 1;
                    digits += 1;
                }
                if value > 0xff {
                    self.reject(
                        escape_start,
                        self.at,
                        codes::ESCAPE_SEQUENCE,
                        "this escape sequence does not fit in a character",
                        "an octal escape holds up to three digits, and its value must be at most 255",
                    );
                    return None;
                }
                char::from_u32(u32::try_from(value).unwrap_or(0)).unwrap_or(char::from(0))
            }
            b'x' => {
                let mut value: i64 = 0;
                let mut digits = 0;
                while self
                    .peek(self.at)
                    .is_some_and(|byte| byte.is_ascii_hexdigit())
                {
                    let digit = self.byte(self.at);
                    let nibble = i64::from(char::from(digit).to_digit(16).unwrap_or(0));
                    value = value * 16 + nibble;
                    self.at += 1;
                    digits += 1;
                }
                if digits == 0 {
                    self.reject(
                        escape_start,
                        self.at,
                        codes::INCOMPLETE_ESCAPE,
                        "this escape sequence has no hexadecimal digits",
                        "`\\x` must be followed by at least one digit: `\\x41`",
                    );
                    return None;
                }
                // C does not require the value to fit in a `char`, so this is
                // truncated the way a `char` would be, rather than refused.
                char::from_u32(u32::try_from(value & 0xff).unwrap_or(0)).unwrap_or(char::from(0))
            }
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'?' => '?',
            b'a' => '\u{7}',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'v' => '\u{b}',
            b'e' => {
                self.reject(
                    escape_start,
                    self.at,
                    codes::INCOMPLETE_ESCAPE,
                    "`\\e` is not a C escape",
                    "C has no `\\e`; use the ASCII code, `\\x1b`",
                );
                return None;
            }
            b'\n' => {
                self.reject(
                    escape_start,
                    self.at,
                    codes::LINE_SPLICE_IN_STRING,
                    "this line is spliced inside a string",
                    "a backslash-newline removes the newline, so a string cannot be continued this way; put the pieces in two strings",
                );
                return None;
            }
            _ => {
                self.reject(
                    escape_start,
                    self.at,
                    codes::INCOMPLETE_ESCAPE,
                    "this is not a C escape sequence",
                    "the escapes C defines are `\\n` `\\t` `\\r` `\\0` `\\a` `\\b` `\\f` `\\v` `\\\\` `\\'` `\\\"` `\\?` and `\\x` with hexadecimal digits",
                );
                return None;
            }
        };
        let _ = start;
        Some(String::from(decoded))
    }

    fn lex_directive(&mut self, start: u32) {
        let line_start = self.at + 1;
        // A directive is one line, plus any continued lines. `#include` is the
        // one whose argument is not C tokens, so it is lexed on its own.
        let mut end = line_start;
        while end < self.len() && self.byte(end) != b'\n' {
            end += 1;
        }
        let mut cursor = line_start;
        while cursor < end && matches!(self.byte(cursor), b' ' | b'\t') {
            cursor += 1;
        }
        let mut name_end = cursor;
        while name_end < end
            && (self.byte(name_end).is_ascii_alphanumeric() || self.byte(name_end) == b'_')
        {
            name_end += 1;
        }
        // The directive's name and its argument are both sliced *before* the
        // cursor moves, because a borrow of the source text and a write to the
        // cursor cannot overlap. Copying two short strings is cheaper than a
        // second pass over the line.
        let name = self.slice(cursor, name_end).to_string();
        let mut argument = name_end;
        while argument < end && matches!(self.byte(argument), b' ' | b'\t') {
            argument += 1;
        }
        let rest = self.slice(argument, end).to_string();
        self.at = end;
        if name == "include" {
            let header = rest.trim();
            let closed = (header.starts_with('<') && header.ends_with('>'))
                || (header.starts_with('"') && header.ends_with('"'));
            if !closed {
                self.reject(
                    start,
                    end,
                    codes::UNCLOSED_HEADER,
                    "this header name is not closed",
                    "a header name is written `<name>` or `\"name\"`",
                );
            }
            self.push(
                TokenKind::HeaderName,
                start,
                end,
                header.to_string(),
                header.to_string(),
            );
            return;
        }
        let mut text = name.clone();
        text.push_str(&rest);
        self.push(TokenKind::Directive, start, end, text, name);
    }

    fn lex_punctuator(&mut self, start: u32) {
        // Longest first. C's operators share prefixes, and a lexer that matched
        // `+` before `++` would turn `i++` into `i + (+...)` and report a
        // syntax error in a program that is perfectly well formed.
        const PUNCTUATORS: &[&str] = &[
            "...", "<<=", ">>=", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||",
            "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "##", "[", "]", "(", ")", "{", "}",
            ".", "&", "*", "+", "-", "~", "!", "/", "%", "<", ">", "^", "|", "?", ":", ";", "=",
            ",", "#",
        ];
        let rest = self.slice(start, self.len());
        for punctuator in PUNCTUATORS {
            if rest.starts_with(punctuator) {
                let end = start + punctuator.len() as u32;
                self.at = end;
                self.push(
                    TokenKind::Punctuator,
                    start,
                    end,
                    (*punctuator).to_string(),
                    (*punctuator).to_string(),
                );
                return;
            }
        }
        self.reject(
            start,
            start + 1,
            codes::UNKNOWN_CHARACTER,
            "this character is not part of any C token",
            "C's tokens are its names, its numbers, its strings, and the punctuators in the grammar; a stray character is usually a smart quote or a non-breaking space",
        );
    }

    fn push(&mut self, kind: TokenKind, start: u32, end: u32, text: String, value: String) {
        self.tokens.push(Token {
            kind,
            text,
            value,
            number: Number::default(),
            character: 0,
            span: self.span(start, end),
        });
    }

    fn slice(&self, start: u32, end: u32) -> &str {
        let start = (start as usize).min(self.text.len());
        let end = (end as usize).min(self.text.len());
        // A span is always inside the file, and a slice built from one is too.
        // A multi-byte character is only ever reached at a character boundary,
        // because every branch that advances does so by a whole character or by
        // a token the grammar has already accepted.
        &self.text[start..end]
    }

    fn reject(&mut self, start: u32, end: u32, code: &str, message: &str, help: &str) {
        self.diagnostics.push(
            crate::diagnostic::at(
                self.source,
                self.sources,
                self.span(start, end),
                code,
                message,
                &[],
                Some(help),
            )
            .diagnostic()
            .clone(),
        );
    }
}

/// Whether `byte` can start an identifier.
const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Whether `byte` can continue an identifier.
const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

/// How many bytes the character starting with `byte` occupies.
///
/// A byte that is not a valid UTF-8 start is one byte, so a file that is not
/// UTF-8 still lexes to its bytes rather than panicking on a slice.
const fn utf8_width(byte: u8) -> u32 {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Every C keyword, so a caller can tell one from a name.
pub const KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "_Bool",
    "_Static_assert",
    "_Noreturn",
];

/// Whether a name is a C keyword.
pub fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}
