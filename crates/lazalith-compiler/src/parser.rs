//! The Lazen v1 parser: recursive descent over the token stream.
//!
//! The parser builds a typed [`crate::ast`] tree and nothing else. It assigns
//! no types, resolves no names, and makes no claim about whether a program is
//! meaningful; a program that parses can still be rejected by the resolver or
//! the type checker.
//!
//! Two properties matter more than anything else here:
//!
//! * **No panic is reachable from source text.** Every lookahead is bounded,
//!   every recursion is finite because every construct must consume a token,
//!   and there is no `unwrap` on user data.
//! * **A rejected construct names itself.** `struct`, `enum`, `match`,
//!   `optional`, `trait`, `impl`, `unsafe`, `type`, and `?` each have their own
//!   code and their own help text, because a program that cannot be compiled
//!   should be told why rather than merely that it failed.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use lazalith_diagnostics::Diagnostic;
use lazalith_diagnostics::{DiagnosticCode, Label, Note, Severity};
use lazalith_types::{SourceId, SourceManager, SourceSpan};

use crate::ast::{
    Abi, ArithOp, BinaryOp, Block, CompareOp, ConstDecl, Expr, ExternDecl, Function, IfArm,
    IntLiteral, Item, ModuleDecl, Name, Parameter, Path, Program, Stmt, TypeAnnotation, TypeExpr,
    UnaryOp, UseDecl,
};
use crate::diagnostic::StageError;
use crate::lexer::{Token, TokenKind};

/// The parser's diagnostic codes.
pub mod codes {
    /// An expected token was not found.
    pub const EXPECTED: &str = "P0001";
    /// A `;` was expected.
    pub const EXPECTED_SEMICOLON: &str = "P0002";
    /// A `}` was expected.
    pub const EXPECTED_BRACE: &str = "P0003";
    /// A `struct` declaration.
    pub const STRUCT: &str = "P0100";
    /// An `enum` declaration.
    pub const ENUM: &str = "P0101";
    /// A `match` expression.
    pub const MATCH: &str = "P0102";
    /// `some` or `none`.
    pub const OPTIONAL: &str = "P0103";
    /// A `trait` declaration.
    pub const TRAIT: &str = "P0104";
    /// An `impl` block.
    pub const IMPL: &str = "P0105";
    /// An `unsafe` block.
    pub const UNSAFE: &str = "P0106";
    /// A `type` alias.
    pub const TYPE_ALIAS: &str = "P0107";
    /// The `?` operator.
    pub const QUESTION: &str = "P0108";
    /// A `loop`-shaped construct that is not in v1.
    pub const UNSUPPORTED_KEYWORD: &str = "P0110";
    /// A `+`-style assignment that v1 does not have.
    pub const COMPOUND_ASSIGNMENT: &str = "P0111";
    /// A function type used as a value.
    pub const FUNCTION_POINTER: &str = "P0112";
    /// An empty item body where one is required.
    pub const EMPTY_BODY: &str = "P0120";
    /// A statement that is not a call, a `let`, or an assignment.
    pub const UNEXPECTED_STATEMENT: &str = "P0121";
    /// Too many closing delimiters.
    pub const UNBALANCED: &str = "P0122";
    /// A missing item after `pub`, `const`, or `extern`.
    pub const MISSING_ITEM: &str = "P0123";
}

/// Parses a token stream into a program.
pub fn parse(
    source: SourceId,
    sources: &SourceManager,
    tokens: Vec<Token>,
) -> Result<Program, StageError> {
    Parser::new(source, sources, tokens).parse_program()
}

struct Parser<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    tokens: Vec<Token>,
    position: usize,
    /// Nesting depth, bounded so a pathological file cannot exhaust the stack.
    depth: u32,
}

/// The deepest expression, block, and item nesting the parser accepts.
///
/// A Lazen v1 program nests a handful of levels; a file that needs hundreds is
/// a mistake, and reporting it is better than overflowing the stack.
const MAX_DEPTH: u32 = 128;

impl<'a> Parser<'a> {
    fn new(source: SourceId, sources: &'a SourceManager, tokens: Vec<Token>) -> Self {
        Self {
            source,
            sources,
            tokens,
            position: 0,
            depth: 0,
        }
    }

    // ---------------------------------------------------------------- tokens

    fn peek(&self) -> &Token {
        self.tokens
            .get(self.position)
            .unwrap_or_else(|| self.tokens.last().expect("the lexer always ends with Eof"))
    }

    fn peek_kind(&self) -> &TokenKind {
        &self.peek().kind
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek_kind() == kind
    }

    /// Whether the cursor is on a string literal with exactly this text.
    fn at_str(&self, expected: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Str(value) if value == expected)
    }

    fn at_eof(&self) -> bool {
        self.at(&TokenKind::Eof)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.position + 1 < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    /// Eats the token if it matches, returning its span.
    fn eat_span(&mut self, kind: &TokenKind) -> Option<SourceSpan> {
        if self.at(kind) {
            Some(self.advance().span)
        } else {
            None
        }
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind) -> Result<Token, StageError> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            Err(self.expected(kind.fixed_text().unwrap_or("this token")))
        }
    }

    fn expected(&self, wanted: &str) -> StageError {
        let found = self.peek();
        let span = self.token_span(found);
        self.diagnostic(
            codes::EXPECTED,
            alloc::format!("expected `{wanted}`, found {}", found.describe()),
            span,
            &[alloc::format!("`{wanted}` is required here").as_str()],
            Some("a Lazen v1 statement ends with `;` and a block ends with `}`"),
            &[],
        )
    }

    fn span(&self, start: u32, end: u32) -> SourceSpan {
        self.sources
            .source_span(
                self.source,
                lazalith_types::ByteOffset::new(start),
                lazalith_types::ByteOffset::new(end),
            )
            .expect("the parser only builds spans inside the file")
    }

    fn token_span(&self, token: &Token) -> SourceSpan {
        token.span.clone()
    }

    fn diagnostic(
        &self,
        raw_code: &str,
        message: impl Into<String>,
        span: SourceSpan,
        notes: &[&str],
        help: Option<&str>,
        extra_labels: &[(SourceSpan, String)],
    ) -> StageError {
        let code = DiagnosticCode::new(raw_code)
            .unwrap_or_else(|_| DiagnosticCode::new("P9999").expect("the fallback code is valid"));
        let mut diagnostic = Diagnostic::new(Severity::Error, code, message)
            .with_label(Label::primary(span, "here"));
        for note in notes {
            diagnostic = diagnostic.with_note(Note::new(*note));
        }
        for (label_span, text) in extra_labels {
            diagnostic = diagnostic.with_label(Label::secondary(label_span.clone(), text.clone()));
        }
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(lazalith_diagnostics::Help::new(help));
        }
        StageError::from_parts(diagnostic, self.sources.clone())
    }

    /// Rejects a v1 omission that can appear where an expression is expected.
    ///
    /// `match`, `some`, and `none` are ordinary identifiers as far as the lexer
    /// is concerned, so without this a program naming a variable `match` would
    /// get a confusing parse error instead of the omission it is.
    fn reject_omission_word(&self) -> Option<StageError> {
        let token = self.peek();
        if !matches!(token.kind, TokenKind::Ident(_)) {
            return None;
        }
        let word = token.ident()?;
        let (code, reason) = match word {
            "match" => (
                codes::MATCH,
                "Lazen v1 has no enums, so there is nothing to match",
            ),
            "some" | "none" => (
                codes::OPTIONAL,
                "Lazen v1 has no `optional`; return an integer status and test it, as section 9 of docs/lazen-syntax.md shows",
            ),
            "self" | "Self" | "super" => (
                codes::UNSUPPORTED_KEYWORD,
                "Lazen v1 has no `self`, and no methods to receive one",
            ),
            // The item keywords are reserved words wherever they appear, not only
            // where an item is expected.
            "struct" => (codes::STRUCT, "Lazen v1 has no records"),
            "enum" => (
                codes::ENUM,
                "Lazen v1 has no enums, and no `match` to consume them",
            ),
            "trait" => (codes::TRAIT, "Lazen v1 has no traits or generics"),
            "impl" => (codes::IMPL, "Lazen v1 has no inherent impls or methods"),
            "type" => (
                codes::TYPE_ALIAS,
                "Lazen v1 has a closed type set and no aliases",
            ),
            "unsafe" => (
                codes::UNSAFE,
                "Lazen v1 has no `unsafe`: every dereference is bounds-checked",
            ),
            _ => return None,
        };
        Some(self.reject_keyword(word, code, reason))
    }

    /// Rejects a construct that is not part of Lazen v1, by name.
    fn reject_keyword(&self, word: &str, code: &str, reason: &str) -> StageError {
        let token = self.peek();
        self.diagnostic(
            code,
            alloc::format!("`{word}` is not part of Lazen v1"),
            self.token_span(token),
            &[reason],
            Some("see section 13 of docs/lazen-syntax.md"),
            &[],
        )
    }

    /// Reports a name that cannot begin an item.
    fn reject_item_keyword(&self) -> Option<StageError> {
        let token = self.peek().clone();
        let (code, reason) = match token.kind {
            TokenKind::Ident(ref name) => match name.as_str() {
                "struct" => (
                    codes::STRUCT,
                    "Lazen v1 has no records, so there is nothing to construct",
                ),
                "enum" => (
                    codes::ENUM,
                    "Lazen v1 has no enums, and no `match` to consume them",
                ),
                "trait" => (codes::TRAIT, "Lazen v1 has no traits or generics"),
                "impl" => (codes::IMPL, "Lazen v1 has no inherent impls or methods"),
                "type" => (
                    codes::TYPE_ALIAS,
                    "Lazen v1 has a closed type set and no aliases",
                ),
                "unsafe" => (
                    codes::UNSAFE,
                    "Lazen v1 has no `unsafe`: every dereference is bounds-checked",
                ),
                _ => return None,
            },
            _ => return None,
        };
        Some(self.reject_keyword(token.ident().unwrap_or("?"), code, reason))
    }

    // ----------------------------------------------------------------- items

    fn parse_program(&mut self) -> Result<Program, StageError> {
        let start = self.peek().span.start().as_u32();
        let mut items = Vec::new();
        while !self.at_eof() {
            items.push(self.parse_item()?);
        }
        let end = self.peek().span.start().as_u32();
        Ok(Program {
            items,
            span: self.span(start, end),
        })
    }

    fn parse_item(&mut self) -> Result<Item, StageError> {
        if let Some(error) = self.reject_item_keyword() {
            return Err(error);
        }
        let is_public = self.eat(&TokenKind::Pub);
        if self.at(&TokenKind::Extern) {
            if is_public {
                return Err(self.missing_item_error("an `extern` declaration cannot be `pub`"));
            }
            return Ok(Item::Extern(self.parse_extern()?));
        }
        if self.at(&TokenKind::Const) {
            let item = self.parse_const(is_public)?;
            return Ok(Item::Const(item));
        }
        if self.at(&TokenKind::Mod) {
            let item = self.parse_module(is_public)?;
            return Ok(Item::Module(item));
        }
        if self.at(&TokenKind::Use) {
            if is_public {
                return Err(self.missing_item_error("a `use` declaration cannot be `pub`"));
            }
            return Ok(Item::Use(self.parse_use()?));
        }
        if self.at(&TokenKind::Fn) {
            let mut item = self.parse_function(is_public)?;
            if is_public {
                item.is_public = true;
            }
            return Ok(Item::Function(item));
        }
        if is_public {
            return Err(
                self.missing_item_error("`pub` must be followed by `fn`, `mod`, or `const`")
            );
        }
        Err(self.unexpected_item_error())
    }

    fn missing_item_error(&self, message: &str) -> StageError {
        let token = self.peek();
        self.diagnostic(
            codes::MISSING_ITEM,
            message,
            self.token_span(token),
            &[],
            Some("write `pub fn`, `pub mod`, or `pub const`"),
            &[],
        )
    }

    fn unexpected_item_error(&self) -> StageError {
        let token = self.peek().clone();
        match &token.kind {
            TokenKind::Ident(name) => {
                let (code, message, help) = match name.as_str() {
                    "match" => (
                        codes::MATCH,
                        String::from("`match` is not part of Lazen v1"),
                        String::from(
                            "Lazen v1 has no enums, so there is nothing to match; use `if` and `else`",
                        ),
                    ),
                    "some" | "none" => (
                        codes::OPTIONAL,
                        alloc::format!("`{name}` is not part of Lazen v1"),
                        String::from(
                            "Lazen v1 has no `optional`; return an integer status and test it, as section 9 of docs/lazen-syntax.md shows",
                        ),
                    ),
                    "loop" => (
                        codes::UNSUPPORTED_KEYWORD,
                        String::from("a bare `loop` is not a Lazen v1 statement"),
                        String::from("write `loop { }`"),
                    ),
                    _ => (
                        codes::UNEXPECTED_STATEMENT,
                        alloc::format!("expected an item, found `{name}`"),
                        String::from("an item is `fn`, `extern`, `mod`, `use`, or `const`"),
                    ),
                };
                self.diagnostic(
                    code,
                    message,
                    self.token_span(&token),
                    &[],
                    Some(&help),
                    &[],
                )
            }
            other => self.diagnostic(
                codes::UNEXPECTED_STATEMENT,
                alloc::format!(
                    "expected an item, found {}",
                    other.fixed_text().unwrap_or("this token")
                ),
                self.token_span(&token),
                &[],
                Some("an item is `fn`, `extern`, `mod`, `use`, or `const`"),
                &[],
            ),
        }
    }

    fn parse_function(&mut self, is_public: bool) -> Result<Function, StageError> {
        let start = self.expect(&TokenKind::Fn)?.span.start().as_u32();
        let name = self.parse_name()?;
        let parameters = self.parse_parameters()?;
        let result = self.parse_result_type()?;
        let body = self.parse_block()?;
        let end = body.span.end().as_u32();
        Ok(Function {
            name,
            is_public,
            parameters,
            result,
            body,
            span: self.span(start, end),
        })
    }

    fn parse_extern(&mut self) -> Result<ExternDecl, StageError> {
        let start = self.expect(&TokenKind::Extern)?.span.start().as_u32();
        if !self.at_str("syscall") {
            let abi_token = self.peek().clone();
            return Err(self.diagnostic(
                codes::EXPECTED,
                "the only foreign calling convention in Lazen v1 is `syscall`",
                self.token_span(&abi_token),
                &["`extern \"c\"`, `extern \"system\"`, and inline assembly are not in v1"],
                Some("write `extern \"syscall\"`"),
                &[],
            ));
        }
        self.advance();
        let abi = Abi::Syscall;
        self.expect(&TokenKind::Fn)?;
        let name = self.parse_name()?;
        let parameters = self.parse_parameters()?;
        let result = match self.parse_result_type()? {
            Some(result) => result,
            None => {
                return Err(self.diagnostic(
                    codes::EXPECTED,
                    "an `extern` declaration must state its result type",
                    name.span.clone(),
                    &["an OS ABI call always reports a status"],
                    Some("write `-> i64` or the call's real result type"),
                    &[],
                ));
            }
        };
        let semi = self.expect(&TokenKind::Semi)?;
        Ok(ExternDecl {
            abi,
            name,
            parameters,
            result,
            span: self.span(start, semi.span.end().as_u32()),
        })
    }

    fn parse_const(&mut self, is_public: bool) -> Result<ConstDecl, StageError> {
        let start = self.expect(&TokenKind::Const)?.span.start().as_u32();
        let name = self.parse_name()?;
        let annotation = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi)?;
        Ok(ConstDecl {
            name,
            is_public,
            annotation,
            value,
            span: self.span(start, semi.span.end().as_u32()),
        })
    }

    fn parse_module(&mut self, is_public: bool) -> Result<ModuleDecl, StageError> {
        let start = self.expect(&TokenKind::Mod)?.span.start().as_u32();
        let name = self.parse_name()?;
        self.expect(&TokenKind::OpenBrace)?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.too_deep(name.span.clone()));
        }
        let mut items = Vec::new();
        while !self.at(&TokenKind::CloseBrace) {
            if self.at_eof() {
                return Err(self.expected("}"));
            }
            items.push(self.parse_item()?);
        }
        let close = self.advance();
        self.depth -= 1;
        Ok(ModuleDecl {
            name,
            is_public,
            items,
            span: self.span(start, close.span.end().as_u32()),
        })
    }

    fn parse_use(&mut self) -> Result<UseDecl, StageError> {
        let start = self.expect(&TokenKind::Use)?.span.start().as_u32();
        let path = self.parse_path()?;
        let alias = if self.eat(&TokenKind::As) {
            Some(self.parse_name()?)
        } else {
            None
        };
        let semi = self.expect(&TokenKind::Semi)?;
        Ok(UseDecl {
            path,
            alias,
            span: self.span(start, semi.span.end().as_u32()),
        })
    }

    fn parse_name(&mut self) -> Result<Name, StageError> {
        let token = self.peek().clone();
        let span = self.token_span(&token);
        match &token.kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                self.advance();
                Ok(Name::new(name, span))
            }
            other => {
                let text = other.fixed_text().unwrap_or("this token");
                let found = token.describe();
                Err(self.diagnostic(
                    codes::EXPECTED,
                    alloc::format!("expected a name, found {found}"),
                    span,
                    &[alloc::format!("`{text}` cannot be a name").as_str()],
                    None,
                    &[],
                ))
            }
        }
    }

    fn parse_path(&mut self) -> Result<Path, StageError> {
        let first = self.parse_name()?;
        let mut segments = vec![first];
        while self.eat(&TokenKind::PathSep) {
            segments.push(self.parse_name()?);
        }
        let start = segments[0].span.start().as_u32();
        let end = segments[segments.len() - 1].span.end().as_u32();
        Ok(Path {
            segments,
            span: self.span(start, end),
        })
    }

    fn parse_parameters(&mut self) -> Result<Vec<Parameter>, StageError> {
        self.expect(&TokenKind::OpenParen)?;
        let mut parameters = Vec::new();
        if self.eat(&TokenKind::CloseParen) {
            return Ok(parameters);
        }
        loop {
            let start = self.peek().span.start().as_u32();
            let name = self.parse_name()?;
            self.expect(&TokenKind::Colon)?;
            let annotation = self.parse_type()?;
            let end = annotation.span.end().as_u32();
            parameters.push(Parameter {
                name,
                annotation,
                span: self.span(start, end),
            });
            if self.eat(&TokenKind::Comma) {
                if self.at(&TokenKind::CloseParen) {
                    self.advance();
                    return Ok(parameters);
                }
                continue;
            }
            self.expect(&TokenKind::CloseParen)?;
            return Ok(parameters);
        }
    }

    fn parse_result_type(&mut self) -> Result<Option<TypeAnnotation>, StageError> {
        if self.eat(&TokenKind::Arrow) {
            Ok(Some(self.parse_type()?))
        } else {
            Ok(None)
        }
    }

    // ----------------------------------------------------------------- types

    /// Parses a type, recording the span of everything it consumed.
    fn parse_type(&mut self) -> Result<TypeAnnotation, StageError> {
        let start = self.peek().span.start().as_u32();
        let kind = self.parse_type_expr()?;
        let end = self.previous_end();
        Ok(TypeAnnotation::new(kind, self.span(start, end)))
    }

    /// The end offset of the most recently consumed token.
    fn previous_end(&self) -> u32 {
        self.tokens
            .get(self.position.saturating_sub(1))
            .map(|token| token.span.end().as_u32())
            .unwrap_or_else(|| self.peek().span.start().as_u32())
    }

    fn parse_type_expr(&mut self) -> Result<TypeExpr, StageError> {
        let token = self.peek().clone();
        match &token.kind {
            TokenKind::Ident(name) => {
                let name = name.clone();
                let scalar = match name.as_str() {
                    "bool" => Some(TypeExpr::Bool),
                    "i8" => Some(TypeExpr::I8),
                    "i16" => Some(TypeExpr::I16),
                    "i32" => Some(TypeExpr::I32),
                    "i64" => Some(TypeExpr::I64),
                    "u8" => Some(TypeExpr::U8),
                    "u16" => Some(TypeExpr::U16),
                    "u32" => Some(TypeExpr::U32),
                    "u64" => Some(TypeExpr::U64),
                    "usize" => Some(TypeExpr::Usize),
                    "str" => Some(TypeExpr::Str),
                    _ => None,
                };
                if let Some(scalar) = scalar {
                    self.advance();
                    return Ok(scalar);
                }
                if name == "ptr" {
                    self.advance();
                    self.expect(&TokenKind::Lt)?;
                    let pointee = self.parse_type_expr()?;
                    self.expect(&TokenKind::Gt)?;
                    return Ok(TypeExpr::Ptr(Box::new(pointee)));
                }
                Err(self.unknown_type_error(token))
            }
            TokenKind::AmpOpenBracket => {
                self.advance();
                let element = self.parse_type_expr()?;
                self.expect(&TokenKind::CloseBracket)?;
                Ok(TypeExpr::Slice {
                    element: Box::new(element),
                    mutable: false,
                })
            }
            TokenKind::AmpMut => {
                self.advance();
                self.expect(&TokenKind::OpenBracket)?;
                let element = self.parse_type_expr()?;
                self.expect(&TokenKind::CloseBracket)?;
                Ok(TypeExpr::Slice {
                    element: Box::new(element),
                    mutable: true,
                })
            }
            TokenKind::Amp => {
                self.advance();
                if self.eat(&TokenKind::Mut) {
                    self.expect(&TokenKind::OpenBracket)?;
                    let element = self.parse_type_expr()?;
                    self.expect(&TokenKind::CloseBracket)?;
                    return Ok(TypeExpr::Slice {
                        element: Box::new(element),
                        mutable: true,
                    });
                }
                if self.eat(&TokenKind::OpenBracket) {
                    let element = self.parse_type_expr()?;
                    self.expect(&TokenKind::CloseBracket)?;
                    return Ok(TypeExpr::Slice {
                        element: Box::new(element),
                        mutable: false,
                    });
                }
                if self.at_str_kind("str") {
                    self.advance();
                    return Ok(TypeExpr::StrRef);
                }
                let found = self.peek().clone();
                Err(self.diagnostic(
                    codes::EXPECTED,
                    alloc::format!(
                        "expected `&[T]`, `&mut [T]`, or `&str`, found {}",
                        found.describe()
                    ),
                    self.token_span(&found),
                    &["Lazen v1 has no reference to a single named type other than `&str`"],
                    Some("write `&[u8]`, `&mut [u32]`, or `&str`"),
                    &[],
                ))
            }
            TokenKind::OpenBracket => {
                self.advance();
                let element = self.parse_type_expr()?;
                self.expect(&TokenKind::Semi)?;
                let length = self.parse_array_length()?;
                self.expect(&TokenKind::CloseBracket)?;
                Ok(TypeExpr::Array {
                    element: Box::new(element),
                    length,
                })
            }
            other => {
                let text = other.fixed_text().unwrap_or("this token");
                Err(self.diagnostic(
                    codes::EXPECTED,
                    alloc::format!("expected a type, found `{text}`"),
                    self.token_span(&token),
                    &["Lazen v1 types are bool, i8..i64, u8..u64, usize, str, &str, ptr<T>, &[T], &mut [T], and [T; N]"],
                    Some("see the scalar and compound type tables in docs/lazen-types.md"),
                    &[],
                ))
            }
        }
    }

    /// Whether the cursor is on the identifier with this text.
    fn at_str_kind(&self, expected: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(name) if name == expected)
    }

    fn unknown_type_error(&self, token: Token) -> StageError {
        let name = token.ident().unwrap_or("?").to_string();
        let (message, help) = match name.as_str() {
            "optional" | "Option" => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1 has no `optional`; return an integer status and test it, as section 9 of docs/lazen-syntax.md shows",
            ),
            "f32" | "f64" | "float" => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1 has no floating-point type",
            ),
            "String" => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1's string type is `str`, a checked byte string",
            ),
            "u128" | "i128" | "isize" => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1 integers are i8..i64, u8..u64, and usize",
            ),
            "void" | "never" => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1 has one result type; a function with no result returns `i32`",
            ),
            _ => (
                alloc::format!("`{name}` is not a Lazen v1 type"),
                "Lazen v1 has a closed type set and no aliases",
            ),
        };
        self.diagnostic(
            codes::TYPE_ALIAS,
            message,
            self.token_span(&token),
            &["Lazen v1 types are listed in docs/lazen-types.md"],
            Some(help),
            &[],
        )
    }

    fn parse_array_length(&mut self) -> Result<u64, StageError> {
        let token = self.peek().clone();
        let span = self.token_span(&token);
        match &token.kind {
            TokenKind::Int {
                digits,
                radix,
                suffix,
            } => {
                let has_suffix = suffix.is_some();
                let digits = digits.clone();
                let radix = *radix;
                self.advance();
                if has_suffix {
                    return Err(self.diagnostic(
                        codes::EXPECTED,
                        "an array length is a plain count, not a typed literal",
                        span.clone(),
                        &[],
                        Some("write the count alone, as in `[u8; 16]`"),
                        &[],
                    ));
                }
                let value = u64::from_str_radix(&digits, radix).map_err(|_| {
                    self.diagnostic(
                        codes::EXPECTED,
                        "this array length is too large",
                        span.clone(),
                        &[alloc::format!("`{digits}` does not fit in a length").as_str()],
                        Some("Lazen v1 arrays are limited to a length that fits in u32"),
                        &[],
                    )
                })?;
                Ok(value)
            }
            _ => Err(self.expected("an array length")),
        }
    }

    // ------------------------------------------------------------ statements

    /// Parses a block: `{ statement; ... tail? }`.
    ///
    /// Whether an expression is an assignment, a statement, or the block's tail
    /// is decided by what follows it, never by what precedes it. An expression
    /// followed by `=` is an assignment, one followed by `;` is a statement, and
    /// one followed by `}` is the tail. A bare expression statement still needs
    /// its `;`, so a missing semicolon between two statements is an error rather
    /// than a silently swallowed value.
    fn parse_block(&mut self) -> Result<Block, StageError> {
        let start_token = self.peek().clone();
        if !self.at(&TokenKind::OpenBrace) {
            return Err(self.expected("{"));
        }
        self.advance();
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.too_deep(start_token.span.clone()));
        }
        let mut statements = Vec::new();
        let mut tail = None;
        loop {
            if self.at(&TokenKind::CloseBrace) {
                break;
            }
            if self.at_eof() {
                return Err(self.unclosed_block_error(start_token.span.clone()));
            }
            if self.starts_statement() {
                statements.push(self.parse_statement()?);
                continue;
            }
            if !self.starts_expression() {
                return Err(self.unexpected_statement_error());
            }
            let expression = self.parse_expr()?;
            let start = expression.start();
            if self.at(&TokenKind::Eq) {
                self.advance();
                let value = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi)?;
                statements.push(Stmt::Assign {
                    target: expression,
                    value,
                    span: self.span(start, semi.span.end().as_u32()),
                });
            } else if let Some((operator, operator_span)) = self.compound_assignment() {
                // The operator is two tokens: `+=` is `+` then `=`.
                self.advance();
                self.advance();
                let value = self.parse_expr()?;
                let semi = self.expect(&TokenKind::Semi)?;
                let span = self.span(start, semi.span.end().as_u32());
                let read_span = self.span(start, operator_span.end().as_u32());
                let read = self.read_of(&expression, read_span.clone());
                statements.push(Stmt::Assign {
                    target: expression,
                    value: Expr::Binary {
                        operator,
                        left: Box::new(read),
                        right: Box::new(value),
                        span: read_span,
                    },
                    span,
                });
            } else if self.at(&TokenKind::Semi) {
                let semi = self.advance();
                statements.push(
                    self.statement_from(expression, self.span(start, semi.span.end().as_u32())),
                );
            } else if self.at(&TokenKind::CloseBrace) && tail_capable(&expression) {
                tail = Some(Box::new(expression));
            } else {
                // Only a conditional with an `else` can produce a value, so only
                // that form is a tail expression. Every other `if` is a statement,
                // which is also how the syntax document writes them: with or
                // without a trailing `;`.
                let Expr::If { arms, span } = &expression else {
                    return Err(self.expected(";"));
                };
                let has_else = arms.last().is_some_and(|arm| arm.condition.is_none());
                if has_else && self.at(&TokenKind::CloseBrace) {
                    tail = Some(Box::new(expression));
                } else {
                    statements.push(Stmt::If {
                        arms: arms.clone(),
                        span: span.clone(),
                    });
                }
            }
        }
        let close = self.advance();
        self.depth -= 1;
        Ok(Block {
            statements,
            tail,
            span: self.span(start_token.span.start().as_u32(), close.span.end().as_u32()),
        })
    }

    /// A synthetic read of a place, used to desugar `place += value`. The type
    /// checker sees an ordinary assignment, so it cannot forget the read.
    fn read_of(&self, place: &Expr, span: SourceSpan) -> Expr {
        match place {
            Expr::Path { path, .. } => Expr::Path {
                path: path.clone(),
                span,
            },
            other => other.clone(),
        }
    }

    /// The compound assignment operator at the cursor, with the span of the
    /// operator token.
    fn compound_assignment(&self) -> Option<(BinaryOp, SourceSpan)> {
        let operator = match self.peek_kind() {
            TokenKind::Plus => ArithOp::Add,
            TokenKind::Minus => ArithOp::Sub,
            TokenKind::Star => ArithOp::Mul,
            TokenKind::Slash => ArithOp::Div,
            TokenKind::Percent => ArithOp::Rem,
            _ => return None,
        };
        let next = self.tokens.get(self.position + 1)?;
        if next.kind != TokenKind::Eq {
            return None;
        }
        Some((BinaryOp::Arith(operator), self.token_span(self.peek())))
    }

    /// Whether the token at the cursor must begin a statement.
    fn starts_statement(&self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Let
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Loop
                | TokenKind::Break
                | TokenKind::Continue
                | TokenKind::Return
                | TokenKind::Const
                | TokenKind::OpenBrace
        )
    }

    /// An expression written as a statement becomes an `if` statement when it is
    /// a conditional, and an expression statement otherwise.
    fn statement_from(&self, expression: Expr, span: SourceSpan) -> Stmt {
        match expression {
            Expr::If {
                arms,
                span: if_span,
            } => Stmt::If {
                arms,
                span: if_span,
            },
            other => Stmt::Expression {
                expression: other,
                span,
            },
        }
    }

    fn unexpected_statement_error(&self) -> StageError {
        let token = self.peek().clone();
        self.diagnostic(
            codes::UNEXPECTED_STATEMENT,
            alloc::format!("expected a statement, found {}", token.describe()),
            self.token_span(&token),
            &["a statement is a `let`, a loop, a `return`, a conditional, or a call"],
            Some("statements end with `;`"),
            &[],
        )
    }

    fn too_deep(&self, span: SourceSpan) -> StageError {
        self.diagnostic(
            codes::UNBALANCED,
            alloc::format!("this program nests deeper than {MAX_DEPTH} levels"),
            span,
            &["Lazen v1 blocks and expressions nest a handful of levels at most"],
            Some("split the program into functions"),
            &[],
        )
    }

    fn unclosed_block_error(&self, span: SourceSpan) -> StageError {
        self.diagnostic(
            codes::EXPECTED_BRACE,
            "this block is never closed",
            span,
            &[alloc::format!("found {} instead of `}}`", self.peek().describe()).as_str()],
            Some("add the closing `}`"),
            &[],
        )
    }

    /// Whether the current token can begin an expression.
    fn starts_expression(&self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Int { .. }
                | TokenKind::Str(_)
                | TokenKind::Ident(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::OpenParen
                | TokenKind::OpenBrace
                | TokenKind::OpenBracket
                | TokenKind::Minus
                | TokenKind::Bang
                | TokenKind::Amp
                | TokenKind::AmpMut
                | TokenKind::Star
                | TokenKind::If
        )
    }

    fn parse_statement(&mut self) -> Result<Stmt, StageError> {
        let start_token = self.peek().clone();
        if let Some(error) = self.reject_item_keyword() {
            return Err(error);
        }
        match &start_token.kind {
            TokenKind::Let => self.parse_let(),
            TokenKind::While => self.parse_while(),
            TokenKind::For => self.parse_for(),
            TokenKind::Loop => self.parse_loop(),
            TokenKind::Break => {
                self.advance();
                let semi = self.expect(&TokenKind::Semi)?;
                Ok(Stmt::Break {
                    span: self.span(start_token.span.start().as_u32(), semi.span.end().as_u32()),
                })
            }
            TokenKind::Continue => {
                self.advance();
                let semi = self.expect(&TokenKind::Semi)?;
                Ok(Stmt::Continue {
                    span: self.span(start_token.span.start().as_u32(), semi.span.end().as_u32()),
                })
            }
            TokenKind::Return => {
                self.advance();
                let value = if self.at(&TokenKind::Semi) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                let semi = self.expect(&TokenKind::Semi)?;
                Ok(Stmt::Return {
                    value,
                    span: self.span(start_token.span.start().as_u32(), semi.span.end().as_u32()),
                })
            }
            TokenKind::Const => Err(self.reject_keyword(
                "const",
                codes::UNSUPPORTED_KEYWORD,
                "a `const` is an item, not a statement",
            )),
            TokenKind::OpenBrace => {
                let block = self.parse_block()?;
                let span = block.span.clone();
                Ok(Stmt::Block {
                    block: Box::new(block),
                    span,
                })
            }
            // An expression statement, an assignment, or a bare `if`. The block
            // loop decides which by what follows the expression, so this arm only
            // reports a token that cannot begin a statement at all.
            _ => Err(self.unexpected_statement_error()),
        }
    }

    fn eat_semicolon_if_present(&mut self) -> bool {
        self.eat(&TokenKind::Semi)
    }

    fn parse_let(&mut self) -> Result<Stmt, StageError> {
        let start = self.peek().span.start().as_u32();
        self.advance();
        let mutable = self.eat(&TokenKind::Mut);
        let name = self.parse_name()?;
        let annotation = if self.eat(&TokenKind::Colon) {
            Some(self.parse_type()?)
        } else {
            None
        };
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_expr()?;
        let semi = self.expect(&TokenKind::Semi)?;
        Ok(Stmt::Let {
            name,
            mutable,
            annotation,
            value,
            span: self.span(start, semi.span.end().as_u32()),
        })
    }

    fn parse_while(&mut self) -> Result<Stmt, StageError> {
        let start = self.advance().span.start().as_u32();
        let condition = self.parse_expr()?;
        let body = self.parse_block()?;
        let end = body.span.end().as_u32();
        self.eat_semicolon_if_present();
        Ok(Stmt::While {
            condition,
            body,
            span: self.span(start, end),
        })
    }

    fn parse_for(&mut self) -> Result<Stmt, StageError> {
        let start = self.advance().span.start().as_u32();
        let name = self.parse_name()?;
        self.expect(&TokenKind::In)?;
        let iterated = self.parse_expr()?;
        let range_end = if self.eat(&TokenKind::DotDot) {
            Some(self.parse_expr()?)
        } else {
            None
        };
        let body = self.parse_block()?;
        let end = body.span.end().as_u32();
        self.eat_semicolon_if_present();
        Ok(Stmt::For {
            name,
            iterated,
            end: range_end,
            body,
            span: self.span(start, end),
        })
    }

    fn parse_loop(&mut self) -> Result<Stmt, StageError> {
        let start = self.advance().span.start().as_u32();
        let body = self.parse_block()?;
        let end = body.span.end().as_u32();
        self.eat_semicolon_if_present();
        Ok(Stmt::Loop {
            body,
            span: self.span(start, end),
        })
    }

    // ----------------------------------------------------------- expressions

    pub(crate) fn parse_expr(&mut self) -> Result<Expr, StageError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.too_deep(self.peek().span.clone()));
        }
        let expression = self.parse_binary(0)?;
        self.depth -= 1;
        Ok(expression)
    }

    /// Binary operator precedence, lowest binding first. `&&` and `||` are
    /// separate levels so that `a || b && c` groups as `a || (b && c)`, and
    /// comparison is looser than arithmetic, so `a + 1 < b * 2` groups as
    /// `(a + 1) < (b * 2)`.
    fn binary_precedence(kind: &TokenKind) -> Option<(BinaryOp, u8)> {
        Some(match kind {
            TokenKind::OrOr => (BinaryOp::Or, 1),
            TokenKind::AndAnd => (BinaryOp::And, 2),
            TokenKind::EqEq => (BinaryOp::Compare(CompareOp::Equal), 3),
            TokenKind::BangEq => (BinaryOp::Compare(CompareOp::NotEqual), 3),
            TokenKind::Lt => (BinaryOp::Compare(CompareOp::Less), 4),
            TokenKind::LtEq => (BinaryOp::Compare(CompareOp::LessEqual), 4),
            TokenKind::Gt => (BinaryOp::Compare(CompareOp::Greater), 4),
            TokenKind::GtEq => (BinaryOp::Compare(CompareOp::GreaterEqual), 4),
            TokenKind::Plus => (BinaryOp::Arith(ArithOp::Add), 5),
            TokenKind::Minus => (BinaryOp::Arith(ArithOp::Sub), 5),
            TokenKind::Star => (BinaryOp::Arith(ArithOp::Mul), 6),
            TokenKind::Slash => (BinaryOp::Arith(ArithOp::Div), 6),
            TokenKind::Percent => (BinaryOp::Arith(ArithOp::Rem), 6),
            _ => return None,
        })
    }

    fn parse_binary(&mut self, minimum: u8) -> Result<Expr, StageError> {
        let mut left = self.parse_cast()?;
        while let Some((operator, precedence)) = Self::binary_precedence(self.peek_kind()) {
            // `+=` is an assignment, not an addition followed by an `=`, so the
            // expression parser must stop before it and let the statement decide.
            if self.compound_assignment().is_some() {
                break;
            }
            if precedence < minimum {
                break;
            }
            self.advance();
            // All Lazen binary operators are left-associative.
            let right = self.parse_binary(precedence + 1)?;
            let span = self.span(left.start(), right.end());
            left = Expr::Binary {
                operator,
                left: Box::new(left),
                right: Box::new(right),
                span,
            };
        }
        Ok(left)
    }

    /// Parses a cast.
    ///
    /// A cast binds more tightly than any binary operator and less tightly than
    /// a unary one, so `-x as i64` is `(-x) as i64`, and
    /// `&mut handle as ptr<u32>` takes the address first and casts second.
    fn parse_cast(&mut self) -> Result<Expr, StageError> {
        let mut operand = self.parse_unary()?;
        // Casts chain from left to right, as `a as u8 as u64` does.
        while self.at(&TokenKind::As) {
            self.advance();
            let target = self.parse_type()?;
            let span = self.span(operand.start(), target.span.end().as_u32());
            operand = Expr::Cast {
                operand: Box::new(operand),
                target,
                span,
            };
        }
        Ok(operand)
    }

    fn parse_unary(&mut self) -> Result<Expr, StageError> {
        let token = self.peek().clone();
        let operator = match token.kind {
            TokenKind::Minus => Some(UnaryOp::Negate),
            TokenKind::Bang => Some(UnaryOp::Not),
            TokenKind::Amp => Some(UnaryOp::Address),
            TokenKind::AmpMut => Some(UnaryOp::AddressMut),
            TokenKind::Star => Some(UnaryOp::Deref),
            _ => None,
        };
        let Some(operator) = operator else {
            return self.parse_postfix_expr();
        };
        self.advance();
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.too_deep(token.span.clone()));
        }
        let operand = self.parse_unary()?;
        self.depth -= 1;
        Ok(Expr::Unary {
            operator,
            span: self.span(token.span.start().as_u32(), operand.end()),
            operand: Box::new(operand),
        })
    }

    fn parse_postfix_expr(&mut self) -> Result<Expr, StageError> {
        let mut expression = self.parse_primary()?;
        loop {
            if self.at(&TokenKind::OpenParen) {
                let open = self.advance();
                let (arguments, close) = self.parse_arguments()?;
                let end = close.end().as_u32();
                expression = Expr::Call {
                    callee: Box::new(expression),
                    arguments,
                    span: self.span(open.span.start().as_u32(), end),
                };
            } else if self.at(&TokenKind::OpenBracket) {
                let open = self.advance();
                let index = self.parse_expr()?;
                let close = self.expect(&TokenKind::CloseBracket)?;
                expression = Expr::Index {
                    base: Box::new(expression),
                    index: Box::new(index),
                    span: self.span(open.span.start().as_u32(), close.span.end().as_u32()),
                };
            } else if self.at(&TokenKind::Question) {
                // `?` follows a postfix expression, so it is rejected here
                // rather than in `parse_primary`.
                let token = self.advance();
                return Err(self.diagnostic(
                    codes::QUESTION,
                    "the `?` operator is not part of Lazen v1",
                    self.token_span(&token),
                    &["Lazen v1 has no `optional` to propagate"],
                    Some("test an integer status with `if`, as section 9 of docs/lazen-syntax.md shows"),
                    &[],
                ));
            } else if self.at(&TokenKind::Dot) {
                self.advance();
                let method = self.parse_name()?;
                let (arguments, close) = if self.at(&TokenKind::OpenParen) {
                    let open = self.advance();
                    let (arguments, close) = self.parse_arguments()?;
                    let _ = open;
                    (arguments, close)
                } else {
                    (Vec::new(), method.span.clone())
                };
                let end = close.end().as_u32();
                let start = expression.start();
                expression = Expr::MethodCall {
                    receiver: Box::new(expression),
                    method,
                    arguments,
                    span: self.span(start, end),
                };
            } else {
                break;
            }
        }
        Ok(expression)
    }

    /// Parses an argument list and consumes the closing parenthesis.
    ///
    /// The closing parenthesis is consumed here and its span returned, so a caller
    /// never has to expect it a second time.
    fn parse_arguments(&mut self) -> Result<(Vec<Expr>, SourceSpan), StageError> {
        let mut arguments = Vec::new();
        if let Some(close) = self.eat_span(&TokenKind::CloseParen) {
            return Ok((arguments, close));
        }
        loop {
            arguments.push(self.parse_expr()?);
            if let Some(close) = self.eat_span(&TokenKind::CloseParen) {
                return Ok((arguments, close));
            }
            if self.eat(&TokenKind::Comma) {
                if let Some(close) = self.eat_span(&TokenKind::CloseParen) {
                    return Ok((arguments, close));
                }
                continue;
            }
            return Err(self.expected(")"));
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, StageError> {
        if let Some(error) = self.reject_omission_word() {
            return Err(error);
        }
        let token = self.peek().clone();
        let span = self.token_span(&token);
        match &token.kind {
            TokenKind::Int {
                digits,
                radix,
                suffix,
            } => {
                self.advance();
                let (digits, radix, suffix) = (digits.clone(), *radix, *suffix);
                let value = parse_int_value(&digits, radix).ok_or_else(|| {
                    self.diagnostic(
                        codes::EXPECTED,
                        "this integer literal does not fit in Lazen v1",
                        span.clone(),
                        &[
                            alloc::format!("`{digits}` is larger than any Lazen v1 integer")
                                .as_str(),
                        ],
                        Some("Lazen v1 integers are at most 64 bits wide"),
                        &[],
                    )
                })?;
                Ok(Expr::Int {
                    literal: IntLiteral {
                        value,
                        radix,
                        suffix,
                    },
                    span,
                })
            }
            TokenKind::Str(value) => {
                self.advance();
                Ok(Expr::Str {
                    value: value.clone(),
                    span,
                })
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Bool { value: true, span })
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Bool { value: false, span })
            }
            TokenKind::Ident(_) => {
                let path = self.parse_path()?;
                let span = path.span.clone();
                Ok(Expr::Path { path, span })
            }
            TokenKind::OpenParen => {
                self.advance();
                let inner = self.parse_expr()?;
                self.expect(&TokenKind::CloseParen)?;
                Ok(inner)
            }
            TokenKind::OpenBracket => self.parse_array_literal(),
            TokenKind::If => {
                let (arms, span) = self.parse_if_arms()?;
                Ok(Expr::If { arms, span })
            }
            TokenKind::Question => Err(self.diagnostic(
                codes::QUESTION,
                "the `?` operator is not part of Lazen v1",
                span,
                &["Lazen v1 has no `optional` to propagate"],
                Some(
                    "test an integer status with `if`, as section 9 of docs/lazen-syntax.md shows",
                ),
                &[],
            )),
            TokenKind::PathSep
            | TokenKind::Arrow
            | TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::CloseParen
            | TokenKind::CloseBrace
            | TokenKind::CloseBracket
            | TokenKind::Eof => Err(self.diagnostic(
                codes::EXPECTED,
                alloc::format!("expected an expression, found {}", token.describe()),
                span,
                &[],
                Some("an expression is a literal, a name, a call, or an operation on those"),
                &[],
            )),
            other => {
                let text = other.fixed_text().unwrap_or("this token");
                if let Some(word) = token.ident() {
                    let _ = word;
                }
                Err(self.diagnostic(
                    codes::EXPECTED,
                    alloc::format!("expected an expression, found `{text}`"),
                    span,
                    &[],
                    Some("an expression is a literal, a name, a call, or an operation on those"),
                    &[],
                ))
            }
        }
    }

    fn parse_array_literal(&mut self) -> Result<Expr, StageError> {
        let open = self.advance();
        let start = open.span.start().as_u32();
        if self.eat(&TokenKind::CloseBracket) {
            return Err(self.diagnostic(
                codes::EXPECTED,
                "an array literal needs at least one element or a repeat count",
                self.span(start, open.span.end().as_u32()),
                &["`[]` has no element type to infer"],
                Some("write `[0u8; 16]`, or remove the brackets"),
                &[],
            ));
        }
        let first = self.parse_expr()?;
        if self.eat(&TokenKind::Semi) {
            let count = self.parse_expr()?;
            let close = self.expect(&TokenKind::CloseBracket)?;
            return Ok(Expr::ArrayRepeat {
                value: Box::new(first),
                count: Box::new(count),
                span: self.span(start, close.span.end().as_u32()),
            });
        }
        let mut elements = vec![first];
        while self.eat(&TokenKind::Comma) {
            if self.at(&TokenKind::CloseBracket) {
                break;
            }
            elements.push(self.parse_expr()?);
        }
        let close = self.expect(&TokenKind::CloseBracket)?;
        Ok(Expr::Array {
            elements,
            span: self.span(start, close.span.end().as_u32()),
        })
    }

    /// Parses `if` arms: one or more `if` arms and an optional `else`.
    fn parse_if_arms(&mut self) -> Result<(Vec<IfArm>, SourceSpan), StageError> {
        let start = self.peek().span.start().as_u32();
        let mut arms = Vec::new();
        let mut end = start;
        loop {
            if !self.at(&TokenKind::If) {
                break;
            }
            let if_token = self.advance();
            let condition = self.parse_expr()?;
            let body = self.parse_block()?;
            end = body.span.end().as_u32();
            arms.push(IfArm {
                condition: Some(condition),
                span: self.span(if_token.span.start().as_u32(), end),
                body,
            });
            // `else if` chains without needing braces around the nested `if`.
            let Some(else_token) = self.eat_span(&TokenKind::Else) else {
                break;
            };
            if self.at(&TokenKind::If) {
                continue;
            }
            let body = self.parse_block()?;
            end = body.span.end().as_u32();
            arms.push(IfArm {
                condition: None,
                span: self.span(else_token.start().as_u32(), end),
                body,
            });
            break;
        }
        Ok((arms, self.span(start, end)))
    }
}

/// Parses a literal's digits, returning `None` when they do not fit in 128 bits.
fn parse_int_value(digits: &str, radix: u32) -> Option<u128> {
    u128::from_str_radix(digits, radix).ok()
}

/// Whether an expression can be a block's tail.
///
/// Every expression can, except a conditional with no `else`: it has no value
/// when the condition is false, so it is a statement wherever it appears.
fn tail_capable(expression: &Expr) -> bool {
    match expression {
        Expr::If { arms, .. } => arms.last().is_some_and(|arm| arm.condition.is_none()),
        _ => true,
    }
}
