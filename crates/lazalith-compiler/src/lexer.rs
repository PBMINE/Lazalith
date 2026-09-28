//! The Lazen v1 lexer.
//!
//! Turns source text into a flat token vector. Every token carries an exact
//! `SourceSpan`, so every parser and type-checker diagnostic can point at the
//! characters that caused it.
//!
//! The token set is exactly what `docs/lazen-syntax.md` specifies: no block
//! comments, no preprocessor, no nested strings, no interpolation. An unknown
//! byte is an error, never a skipped character.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use lazalith_types::{SourceId, SourceManager, SourceSpan};

use crate::diagnostic::{FileDiagnostic, StageError};

/// An integer type name that may be written as a literal suffix.
///
/// This is the closed set of Lazen v1 integer types. A suffix must be one of
/// these exactly: `12ab` is an error, not an identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntSuffix {
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `usize`
    Usize,
}

impl IntSuffix {
    /// The type's source name.
    pub fn name(self) -> &'static str {
        match self {
            IntSuffix::I8 => "i8",
            IntSuffix::I16 => "i16",
            IntSuffix::I32 => "i32",
            IntSuffix::I64 => "i64",
            IntSuffix::U8 => "u8",
            IntSuffix::U16 => "u16",
            IntSuffix::U32 => "u32",
            IntSuffix::U64 => "u64",
            IntSuffix::Usize => "usize",
        }
    }

    /// Every valid suffix, for a diagnostic.
    pub fn all() -> [IntSuffix; 9] {
        [
            IntSuffix::I8,
            IntSuffix::I16,
            IntSuffix::I32,
            IntSuffix::I64,
            IntSuffix::U8,
            IntSuffix::U16,
            IntSuffix::U32,
            IntSuffix::U64,
            IntSuffix::Usize,
        ]
    }

    fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "i8" => IntSuffix::I8,
            "i16" => IntSuffix::I16,
            "i32" => IntSuffix::I32,
            "i64" => IntSuffix::I64,
            "u8" => IntSuffix::U8,
            "u16" => IntSuffix::U16,
            "u32" => IntSuffix::U32,
            "u64" => IntSuffix::U64,
            "usize" => IntSuffix::Usize,
            _ => return None,
        })
    }

    /// Whether this type is signed.
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntSuffix::I8 | IntSuffix::I16 | IntSuffix::I32 | IntSuffix::I64
        )
    }

    /// The type's width in bits.
    pub fn bits(self) -> u16 {
        match self {
            IntSuffix::I8 | IntSuffix::U8 => 8,
            IntSuffix::I16 | IntSuffix::U16 => 16,
            IntSuffix::I32 | IntSuffix::U32 => 32,
            IntSuffix::I64 | IntSuffix::U64 => 64,
            IntSuffix::Usize => 0,
        }
    }
}

/// A lexical token kind.
///
/// Every operator the grammar needs is its own variant, so the parser never
/// inspects token text to decide what to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenKind {
    /// An identifier or keyword; `text` says which.
    Ident(String),
    /// An integer literal, exactly as written, plus its base and any type
    /// suffix.
    Int {
        /// Digits without the base prefix.
        digits: String,
        /// 10 or 16.
        radix: u32,
        /// The type named by a suffix such as `0u8`, if present.
        suffix: Option<IntSuffix>,
    },
    /// A string literal's contents, unescaped. Newlines are rejected.
    Str(String),
    /// End of input.
    Eof,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `=`
    Eq,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `&&`
    AndAnd,
    /// `||`
    OrOr,
    /// `!`
    Bang,
    /// `&`
    Amp,
    /// `&mut`
    AmpMut,
    /// `&[`
    AmpOpenBracket,
    /// `->`
    Arrow,
    /// `::`
    PathSep,
    /// `:`
    Colon,
    /// `;`
    Semi,
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `(`
    OpenParen,
    /// `)`
    CloseParen,
    /// `{`
    OpenBrace,
    /// `[`
    OpenBracket,
    /// `]`
    CloseBracket,
    /// `}`
    CloseBrace,
    /// `?` is not in Lazen v1 and is reported by its own code.
    Question,
    /// `let`
    Let,
    /// `mut`
    Mut,
    /// `fn`
    Fn,
    /// `extern`
    Extern,
    /// `mod`
    Mod,
    /// `use`
    Use,
    /// `pub`
    Pub,
    /// `const`
    Const,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `for`
    For,
    /// `in`
    In,
    /// `loop`
    Loop,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `return`
    Return,
    /// `true`
    True,
    /// `false`
    False,
    /// `as`
    As,
}

impl TokenKind {
    /// The fixed text of this token, for a single-byte or keyword token.
    pub fn fixed_text(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Eq => "=",
            TokenKind::EqEq => "==",
            TokenKind::BangEq => "!=",
            TokenKind::Lt => "<",
            TokenKind::LtEq => "<=",
            TokenKind::Gt => ">",
            TokenKind::GtEq => ">=",
            TokenKind::AndAnd => "&&",
            TokenKind::OrOr => "||",
            TokenKind::Bang => "!",
            TokenKind::Amp => "&",
            TokenKind::AmpMut => "&mut",
            TokenKind::AmpOpenBracket => "&[",
            TokenKind::Arrow => "->",
            TokenKind::PathSep => "::",
            TokenKind::Colon => ":",
            TokenKind::Semi => ";",
            TokenKind::Comma => ",",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::OpenParen => "(",
            TokenKind::CloseParen => ")",
            TokenKind::OpenBrace => "{",
            TokenKind::OpenBracket => "[",
            TokenKind::CloseBracket => "]",
            TokenKind::CloseBrace => "}",
            TokenKind::Question => "?",
            TokenKind::Let => "let",
            TokenKind::Mut => "mut",
            TokenKind::Fn => "fn",
            TokenKind::Extern => "extern",
            TokenKind::Mod => "mod",
            TokenKind::Use => "use",
            TokenKind::Pub => "pub",
            TokenKind::Const => "const",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::While => "while",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::Loop => "loop",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Return => "return",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::As => "as",
            TokenKind::Eof => "<end of input>",
            TokenKind::Ident(_) | TokenKind::Int { .. } | TokenKind::Str(_) => return None,
        })
    }

    /// Whether this token is a keyword, that is, an identifier-shaped token
    /// that may not be used as a name.
    pub fn is_keyword(&self) -> bool {
        matches!(self.fixed_text(), Some(text) if !text.is_empty() && is_keyword_text(text))
    }
}

fn is_keyword_text(text: &str) -> bool {
    matches!(
        text,
        "let"
            | "mut"
            | "fn"
            | "extern"
            | "mod"
            | "use"
            | "pub"
            | "const"
            | "if"
            | "else"
            | "while"
            | "for"
            | "in"
            | "loop"
            | "break"
            | "continue"
            | "return"
            | "true"
            | "false"
            | "as"
    )
}

/// A token and where it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// The exact source range it covers.
    pub span: SourceSpan,
    /// The raw text of the token, for diagnostics.
    pub text: String,
}

impl Token {
    /// Whether this token is the given fixed token.
    pub fn is(&self, kind: &TokenKind) -> bool {
        &self.kind == kind
    }

    /// The identifier's text, if this token is an identifier.
    pub fn ident(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Ident(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// A short human name for this token, used in "expected X, found Y".
    pub fn describe(&self) -> String {
        match &self.kind {
            TokenKind::Ident(name) => alloc::format!("identifier `{name}`"),
            TokenKind::Int {
                digits,
                radix,
                suffix,
            } => {
                if let Some(suffix) = suffix {
                    return alloc::format!(
                        "{} literal of type {}",
                        if *radix == 16 {
                            "hexadecimal"
                        } else {
                            "integer"
                        },
                        suffix.name()
                    );
                }
                if *radix == 16 {
                    alloc::format!("hexadecimal literal `0x{digits}`")
                } else {
                    alloc::format!("integer literal `{digits}`")
                }
            }
            TokenKind::Str(_) => "string literal".to_string(),
            TokenKind::Eof => "end of input".to_string(),
            other => {
                let text = other.fixed_text().unwrap_or("token");
                alloc::format!("`{text}`")
            }
        }
    }
}

/// The result of lexing: tokens, or the first error.
pub struct Lexed {
    /// The tokens, always ending with `Eof` on success.
    pub tokens: Vec<Token>,
    /// Every `//` comment, in the order they appear.
    ///
    /// Comments are trivia, so nothing in the compiler reads this and nothing in the
    /// compiler needs to. The formatter does, and it cannot find them any other way:
    /// a formatter that re-derived comments from the source text would be a second
    /// lexer, and a second lexer is a second thing to get wrong. Recording them here
    /// costs one vector and means there is exactly one answer to "where are the
    /// comments".
    pub comments: Vec<Comment>,
    /// Every diagnostic, sorted by position. The compiler stops at the first
    /// error per file so later stages never see a broken token stream.
    pub diagnostics: Vec<StageError>,
}

/// A `//` comment, and where it was.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Comment {
    /// The exact source range it covers, not counting the `//`.
    pub span: SourceSpan,
    /// The text after the `//`, with no trailing whitespace and *with* its leading
    /// whitespace intact.
    ///
    /// The trailing end is trimmed because a comment runs to the end of its line and
    /// the newline is not part of it. The leading end is not, because a space after
    /// the `//` is content: a comment holding an indented code sample
    ///
    /// ```text
    /// //     lazen run main.lz
    /// ```
    ///
    /// means it, and trimming it would delete the example. The indent *before* the
    /// `//` is not in this text at all, so a formatter that re-indents the `//`
    /// cannot disturb what is inside it.
    pub text: String,
}

impl Lexed {
    /// The tokens, or `None` if lexing failed.
    pub fn into_tokens(self) -> Option<Vec<Token>> {
        if self.diagnostics.is_empty() {
            Some(self.tokens)
        } else {
            None
        }
    }
}

/// Lexes one file.
///
/// Errors are reported with precise spans and the lexer never panics on any
/// input, including non-UTF-8-safe boundaries, lone `\r`, and a truncated
/// comment or string.
pub fn lex(source: SourceId, sources: &SourceManager) -> Lexed {
    let Some(file) = sources.file(source) else {
        return Lexed {
            tokens: Vec::new(),
            comments: Vec::new(),
            diagnostics: alloc::vec![FileDiagnostic::new(source, sources, 0, 0).build(
                "L0001",
                "the source file is not registered",
                &[],
                None,
            )],
        };
    };
    Lexer::new(source, sources, file.text()).run()
}

struct Lexer<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    text: &'a str,
    bytes: &'a [u8],
    position: u32,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    diagnostics: Vec<StageError>,
}

impl<'a> Lexer<'a> {
    fn new(source: SourceId, sources: &'a SourceManager, text: &'a str) -> Self {
        Self {
            source,
            sources,
            text,
            bytes: text.as_bytes(),
            position: 0,
            comments: Vec::new(),
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> Lexed {
        loop {
            // Trivia is skipped before every token, not just the first, so a
            // token is never mistaken for an unknown character.
            self.skip_trivia();
            if !self.diagnostics.is_empty() || self.position >= self.bytes.len() as u32 {
                break;
            }
            let start = self.position;
            if let Err(error) = self.next_token() {
                self.diagnostics.push(error);
                // Stop at the first lexical error: a partially understood token
                // stream would only produce misleading parser errors.
                break;
            }
            debug_assert!(
                self.position > start,
                "the lexer must make progress on every token"
            );
        }
        if self.diagnostics.is_empty() {
            let eof = self.make(TokenKind::Eof, self.position, self.position);
            self.tokens.push(eof);
        }
        Lexed {
            tokens: self.tokens,
            comments: self.comments,
            diagnostics: self.diagnostics,
        }
    }

    fn span(&self, start: u32, end: u32) -> SourceSpan {
        self.sources
            .source_span(
                self.source,
                lazalith_types::ByteOffset::new(start),
                lazalith_types::ByteOffset::new(end),
            )
            .expect("the lexer only builds spans inside the file")
    }

    fn make(&self, kind: TokenKind, start: u32, end: u32) -> Token {
        Token {
            kind,
            span: self.span(start, end),
            text: String::from(&self.text[start as usize..end as usize]),
        }
    }

    fn error(
        &self,
        start: u32,
        end: u32,
        raw_code: &str,
        message: impl Into<String>,
        notes: &[&str],
        help: Option<&str>,
    ) -> StageError {
        FileDiagnostic::new(self.source, self.sources, start, end)
            .build(raw_code, message, notes, help)
    }

    fn peek(&self, offset: u32) -> Option<u8> {
        self.bytes.get((self.position + offset) as usize).copied()
    }

    fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek(0) {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.position += 1,
                b'/' if self.peek(1) == Some(b'/') => {
                    let start = self.position + 2;
                    while let Some(next) = self.peek(0) {
                        if next == b'\n' {
                            break;
                        }
                        self.position += 1;
                    }
                    // The comment's text is recorded rather than discarded, for the
                    // formatter. A `//` at the very end of the file and a `//`
                    // followed by `\r\n` both work, because the loop stops at any
                    // byte that is not a newline and the slice is taken from the
                    // text rather than from the bytes.
                    let end = self.position;
                    let slice = self
                        .text
                        .get(
                            usize::try_from(start).unwrap_or(usize::MAX)
                                ..usize::try_from(end).unwrap_or(usize::MAX),
                        )
                        .unwrap_or("");
                    self.comments.push(Comment {
                        span: self.span(start, end),
                        text: alloc::string::String::from(slice.trim_end()),
                    });
                }
                b'/' if self.peek(1) == Some(b'*') => {
                    // `/* */` is deliberately not in v1, so it gets a code of
                    // its own instead of a generic "unexpected character".
                    let start = self.position;
                    self.position += 2;
                    let mut closed = false;
                    while let Some(next) = self.peek(0) {
                        if next == b'*' && self.peek(1) == Some(b'/') {
                            self.position += 2;
                            closed = true;
                            break;
                        }
                        self.position += 1;
                    }
                    // The span covers the whole comment, including the `*/` when
                    // it is present.
                    let end = self.position;
                    let (code, message, help) = if closed {
                        (
                            "L0101",
                            "block comments are not part of Lazen v1",
                            Some("use `//` for a line comment; see docs/lazen-syntax.md"),
                        )
                    } else {
                        (
                            "L0102",
                            "this block comment is never closed",
                            Some("Lazen v1 has no block comments at all; use `//`"),
                        )
                    };
                    self.diagnostics
                        .push(self.error(start, end, code, message, &[], help));
                    return;
                }
                _ => return,
            }
        }
    }

    fn next_token(&mut self) -> Result<(), StageError> {
        let start = self.position;
        let byte = self.peek(0).ok_or_else(|| {
            self.error(start, start, "L0002", "unexpected end of input", &[], None)
        })?;
        match byte {
            b'0'..=b'9' => self.lex_number(),
            b'"' => self.lex_string(),
            b'_' | b'a'..=b'z' | b'A'..=b'Z' => self.lex_word(),
            _ => self.lex_punctuation(),
        }
    }

    fn lex_number(&mut self) -> Result<(), StageError> {
        let start = self.position;
        let radix = if self.peek(0) == Some(b'0') && matches!(self.peek(1), Some(b'x') | Some(b'X'))
        {
            self.position += 2;
            16
        } else {
            10
        };
        while let Some(byte) = self.peek(0) {
            let is_digit = match radix {
                10 => byte.is_ascii_digit(),
                16 => byte.is_ascii_hexdigit(),
                _ => false,
            };
            if is_digit || byte == b'_' {
                self.position += 1;
            } else {
                break;
            }
        }
        let digits_end = self.position;
        // An optional type suffix, such as the `u8` in `[0u8; 16]`.
        let mut suffix = None;
        if self
            .peek(0)
            .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        {
            let suffix_start = self.position;
            while let Some(next) = self.peek(0) {
                if next == b'_' || next.is_ascii_alphanumeric() {
                    self.position += 1;
                } else {
                    break;
                }
            }
            let word = &self.text[suffix_start as usize..self.position as usize];
            match IntSuffix::from_text(word) {
                Some(found) => suffix = Some(found),
                None => {
                    let note = alloc::format!(
                        "`{word}` is not an integer type; a literal suffix must be one of i8, i16, i32, i64, u8, u16, u32, u64, usize"
                    );
                    return Err(self.error(
                        start,
                        self.position,
                        "L0104",
                        "this is not a valid Lazen integer literal",
                        &[note.as_str()],
                        Some("write the digits, or add a type suffix such as `0u8`"),
                    ));
                }
            }
        }
        let end = self.position;
        if let Some(byte) = self.peek(0) {
            // Any identifier character directly after the digits means this is
            // one malformed literal rather than a literal followed by a name.
            // `0x1e` is fine, because `e` was consumed as a hex digit.
            if byte == b'_' || byte.is_ascii_alphanumeric() {
                let stop = self.next_char_end(self.position);
                return Err(self.error(
                    start,
                    stop,
                    "L0104",
                    "this is not a valid Lazen integer literal",
                    &[alloc::format!(
                        "`{}` continues the literal's digits",
                        &self.text[self.position as usize..stop as usize]
                    )
                    .as_str()],
                    Some("write digits, optional `0x` hex digits, and an optional type suffix"),
                ));
            }
            if byte == b'.' {
                // `0..10` is a range and `1.next()` is a member access; only a
                // digit after the dot makes this a float, which v1 rejects.
                let float = match self.peek(1) {
                    Some(next) => next.is_ascii_digit(),
                    None => false,
                };
                if float {
                    return Err(self.error(
                        start,
                        self.position + 1,
                        "L0103",
                        "float literals are not part of Lazen v1",
                        &["Lazen v1 has no floating-point type; see docs/lazen-types.md"],
                        Some("use integer arithmetic with an explicit `as` cast"),
                    ));
                }
            }
        }
        let raw: String = self.text[start as usize..digits_end as usize].replace('_', "");
        let digits = match raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            Some(rest) => rest.to_string(),
            None => raw,
        };
        if digits.is_empty() {
            return Err(self.error(
                start,
                digits_end,
                "L0105",
                "a hexadecimal literal needs at least one digit",
                &[],
                Some("write `0x1` or more digits"),
            ));
        }
        let token = self.make(
            TokenKind::Int {
                digits,
                radix,
                suffix,
            },
            start,
            end,
        );
        self.tokens.push(token);
        Ok(())
    }

    fn lex_string(&mut self) -> Result<(), StageError> {
        let start = self.position;
        self.position += 1;
        let mut value = String::new();
        loop {
            let Some(byte) = self.peek(0) else {
                return Err(self.error(
                    start,
                    self.position,
                    "L0110",
                    "this string literal is never closed",
                    &[],
                    Some("add the closing `\"` before the end of the line"),
                ));
            };
            match byte {
                b'"' => {
                    self.position += 1;
                    let token = self.make(TokenKind::Str(value), start, self.position);
                    self.tokens.push(token);
                    return Ok(());
                }
                b'\n' => {
                    return Err(self.error(
                        start,
                        self.position,
                        "L0111",
                        "a string literal cannot span a line",
                        &[],
                        Some("Lazen v1 has no line-continuation or escape syntax in strings"),
                    ));
                }
                b'\\' => {
                    let escape_start = self.position;
                    self.position += 1;
                    let Some(escaped) = self.peek(0) else {
                        return Err(self.error(
                            escape_start,
                            self.position,
                            "L0112",
                            "this escape sequence is never completed",
                            &["the file ends in the middle of a string literal"],
                            Some("Lazen v1 strings support `\\n`, `\\r`, `\\t`, `\\0`, `\\\\`, and `\\\"`"),
                        ));
                    };
                    let decoded = match escaped {
                        b'n' => Some('\n'),
                        b'r' => Some('\r'),
                        b't' => Some('\t'),
                        b'0' => Some('\0'),
                        b'\\' => Some('\\'),
                        b'"' => Some('"'),
                        _ => None,
                    };
                    match decoded {
                        Some(byte) => {
                            value.push(byte);
                            self.position += 1;
                        }
                        None => {
                            let end = self.next_char_end(self.position);
                            return Err(self.error(
                                escape_start,
                                end,
                                "L0113",
                                "this is not a Lazen v1 escape sequence",
                                &[alloc::format!(
                                    "`\\{}` is not one of `\\n`, `\\r`, `\\t`, `\\0`, `\\\\`, `\\\"`",
                                    &self.text[(self.position) as usize..end as usize]
                                )
                                .as_str()],
                                Some("v1 has no `\\u`, `\\x`, `\\0NNN`, or `\\e` escapes"),
                            ));
                        }
                    }
                }
                _ => {
                    // A whole character, not one byte: a multi-byte UTF-8
                    // sequence must never be split by a slice.
                    let next = self.next_char_end(self.position);
                    value.push_str(&self.text[self.position as usize..next as usize]);
                    self.position = next;
                }
            }
        }
    }

    fn lex_word(&mut self) -> Result<(), StageError> {
        let start = self.position;
        while let Some(byte) = self.peek(0) {
            if byte == b'_' || byte.is_ascii_alphanumeric() {
                self.position += 1;
            } else {
                break;
            }
        }
        let end = self.position;
        let word = &self.text[start as usize..end as usize];
        let kind = match word {
            "let" => TokenKind::Let,
            "mut" => TokenKind::Mut,
            "fn" => TokenKind::Fn,
            "extern" => TokenKind::Extern,
            "mod" => TokenKind::Mod,
            "use" => TokenKind::Use,
            "pub" => TokenKind::Pub,
            "const" => TokenKind::Const,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "as" => TokenKind::As,
            other if other.len() > 32 => {
                let note = alloc::format!("`{}...` is 33 characters or more", &other[..32]);
                return Err(self.error(
                    start,
                    end,
                    "L0120",
                    "this identifier is longer than Lazen v1 allows",
                    &[note.as_str()],
                    Some("Lazen v1 identifiers are at most 32 characters of ASCII letters, digits, and `_`"),
                ));
            }
            other => TokenKind::Ident(String::from(other)),
        };
        let token = self.make(kind, start, end);
        self.tokens.push(token);
        Ok(())
    }

    fn lex_punctuation(&mut self) -> Result<(), StageError> {
        let start = self.position;
        // `&mut` is a single token, but only when it is the whole word: in
        // `&mutate` the `&` is an address-of and `mutate` is a name, so the
        // match must respect an identifier boundary.
        if self.peek(0) == Some(b'&')
            && self.peek(1) == Some(b'm')
            && self
                .peek(4)
                .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
        {
            return Err(self.error(
                start,
                start + 1,
                "L0131",
                "`&mut` must be followed by whitespace or a delimiter",
                &["`&mut` is a single token and cannot be a prefix of a name"],
                Some("write `&mut value` or `&value`, not `&mutate`"),
            ));
        }
        let second = self.peek(1);
        let (kind, width) = match (self.peek(0), second) {
            (Some(b'+'), _) => (TokenKind::Plus, 1),
            (Some(b'-'), Some(b'>')) => (TokenKind::Arrow, 2),
            (Some(b'-'), _) => (TokenKind::Minus, 1),
            (Some(b'*'), _) => (TokenKind::Star, 1),
            (Some(b'/'), _) => (TokenKind::Slash, 1),
            (Some(b'%'), _) => (TokenKind::Percent, 1),
            (Some(b'='), Some(b'=')) => (TokenKind::EqEq, 2),
            (Some(b'='), _) => (TokenKind::Eq, 1),
            (Some(b'!'), Some(b'=')) => (TokenKind::BangEq, 2),
            (Some(b'!'), _) => (TokenKind::Bang, 1),
            (Some(b'<'), Some(b'=')) => (TokenKind::LtEq, 2),
            (Some(b'<'), _) => (TokenKind::Lt, 1),
            (Some(b'>'), Some(b'=')) => (TokenKind::GtEq, 2),
            (Some(b'>'), _) => (TokenKind::Gt, 1),
            (Some(b'&'), Some(b'&')) => (TokenKind::AndAnd, 2),
            (Some(b'&'), Some(b'm')) => (TokenKind::AmpMut, 4),
            (Some(b'&'), Some(b'[')) => (TokenKind::AmpOpenBracket, 2),
            (Some(b'&'), _) => (TokenKind::Amp, 1),
            (Some(b'|'), Some(b'|')) => (TokenKind::OrOr, 2),
            (Some(b'|'), _) => {
                return Err(self.error(
                    start,
                    start + 1,
                    "L0130",
                    "Lazen v1 has no single `|` operator",
                    &["`||` is the logical-or operator"],
                    Some("use `||` for logical or"),
                ));
            }
            (Some(b':'), Some(b':')) => (TokenKind::PathSep, 2),
            (Some(b':'), _) => (TokenKind::Colon, 1),
            (Some(b';'), _) => (TokenKind::Semi, 1),
            (Some(b','), _) => (TokenKind::Comma, 1),
            (Some(b'.'), Some(b'.')) => (TokenKind::DotDot, 2),
            (Some(b'.'), _) => (TokenKind::Dot, 1),
            (Some(b'('), _) => (TokenKind::OpenParen, 1),
            (Some(b')'), _) => (TokenKind::CloseParen, 1),
            (Some(b'{'), _) => (TokenKind::OpenBrace, 1),
            (Some(b'['), _) => (TokenKind::OpenBracket, 1),
            (Some(b']'), _) => (TokenKind::CloseBracket, 1),
            (Some(b'}'), _) => (TokenKind::CloseBrace, 1),
            (Some(b'?'), _) => (TokenKind::Question, 1),
            _ => {
                let end = self.next_char_end(start);
                let found = &self.text[start as usize..end as usize];
                return Err(self.error(
                    start,
                    end,
                    "L0199",
                    alloc::format!("this character is not part of Lazen v1 syntax: `{found}`"),
                    &["Lazen v1 has no preprocessor, no attributes, and no block comments"],
                    Some("see the notation section of docs/lazen-syntax.md"),
                ));
            }
        };
        self.position += width;
        let token = self.make(kind, start, self.position);
        self.tokens.push(token);
        Ok(())
    }

    fn next_char_end(&self, start: u32) -> u32 {
        let mut end = start + 1;
        while end < self.bytes.len() as u32 && !self.text.is_char_boundary(end as usize) {
            end += 1;
        }
        end
    }
}

/// The diagnostic kinds the lexer itself emits, exported so tests and the
/// documentation can name them without duplicating string literals.
pub mod codes {
    /// The source file is not registered with the source manager.
    pub const UNKNOWN_SOURCE: &str = "L0001";
    /// An unknown byte is not Lazen v1 syntax.
    pub const UNKNOWN_CHARACTER: &str = "L0199";
    /// A block comment.
    pub const BLOCK_COMMENT: &str = "L0101";
    /// An unclosed block comment.
    pub const UNCLOSED_BLOCK_COMMENT: &str = "L0102";
    /// A floating-point literal.
    pub const FLOAT_LITERAL: &str = "L0103";
    /// A malformed integer literal.
    pub const MALFORMED_INT: &str = "L0104";
    /// A hexadecimal literal with no digits.
    pub const EMPTY_HEX: &str = "L0105";
    /// An unclosed string literal.
    pub const UNCLOSED_STRING: &str = "L0110";
    /// A string literal that spans a line.
    pub const MULTILINE_STRING: &str = "L0111";
    /// An escape sequence.
    pub const ESCAPE_SEQUENCE: &str = "L0113";
    /// An escape sequence cut off by the end of the file.
    pub const INCOMPLETE_ESCAPE: &str = "L0112";
    /// `&mut` used as a name prefix.
    pub const AMP_MUT_PREFIX: &str = "L0131";
    /// An over-long identifier.
    pub const LONG_IDENTIFIER: &str = "L0120";
    /// A single `|`.
    pub const SINGLE_PIPE: &str = "L0130";
}

/// Builds a `SourceSpan` for a byte range, used by tests and the frontend.
pub fn span_of(
    sources: &SourceManager,
    source: SourceId,
    start: u32,
    end: u32,
) -> Option<SourceSpan> {
    sources
        .source_span(
            source,
            lazalith_types::ByteOffset::new(start),
            lazalith_types::ByteOffset::new(end),
        )
        .ok()
}
