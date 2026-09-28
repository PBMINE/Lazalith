//! The C parser: tokens to a syntax tree.
//!
//! # The declaration ambiguity, and how it is settled
//!
//! C's declaration grammar is hard for one specific reason: an identifier's
//! meaning depends on whether it is being *declared* or *used*, and the only
//! way to know which is to look at where you are. `x * y;` is "declare `y` as
//! a pointer to `x`" at file scope and "multiply `x` by `y`" inside a
//! function, and no amount of lookahead settles it.
//!
//! C settled it in 1973 with two grammars and one rule: a statement cannot
//! begin with a type, so anything that *looks* like a type is a declaration.
//! This parser does the same, with one addition it cannot avoid — a bare name is
//! a type only if something already declared it a `typedef`. So the parser
//! keeps its own `typedef` table, extended as it parses, and
//! [`Parser::is_declaration`] consults it. A parser that guessed here would
//! misparse `T (x);` for a `T` that is a variable rather than a type, and it
//! would do so silently.
//!
//! # Declarators
//!
//! A declarator becomes a list of derivations applied outward from the name,
//! stored in the order the source wrote them. `int *a[3]` is `Pointer` then
//! `Array(3)`, which C folds to "array of 3 pointers to `int`". Reading it the
//! other way round is the famous C bug, and keeping the list in source order
//! means the type checker cannot get it wrong by accident: the fold is written
//! once, in [`crate::types`], and documented there.
//!
//! # What this parser refuses
//!
//! Floating-point types are reported by name the moment they are spelled, and
//! the parse carries on so the rest of the program is still checked. A `case`
//! outside a `switch` and a `break` outside a loop are reported for the same
//! reason: a person fixing a program wants every mistake in it, not the first
//! one and a re-run.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use lazalith_types::{ByteOffset, SourceId, SourceManager, SourceSpan};

use crate::ast::*;
use crate::diagnostic::StageError;
use crate::lexer::{Token, TokenKind, is_keyword};

/// C's diagnostic codes for this stage.
pub mod codes {
    /// Something was expected and something else was found.
    pub const EXPECTED: &str = "C0201";
    /// A declaration without its `;`.
    pub const EXPECTED_SEMICOLON: &str = "C0202";
    /// A construct without its `}`.
    pub const UNCLOSED_BRACE: &str = "C0203";
    /// A parenthesised expression without its `)`.
    pub const UNCLOSED_PAREN: &str = "C0204";
    /// A bracketed subscript without its `]`.
    pub const UNCLOSED_BRACKET: &str = "C0205";
    /// A statement that cannot begin here.
    pub const UNEXPECTED_STATEMENT: &str = "C0210";
    /// A function body that is not a compound statement.
    pub const EXPECTED_FUNCTION_BODY: &str = "C0211";
    /// A C construct this compiler does not implement.
    pub const UNSUPPORTED: &str = "C0220";
    /// A keyword in a place the grammar has no use for it.
    pub const UNEXPECTED_KEYWORD: &str = "C0221";
    /// A declarator with no name where one is required.
    pub const EXPECTED_NAME: &str = "C0222";
    /// A `case` outside a `switch`.
    pub const CASE_OUTSIDE_SWITCH: &str = "C0230";
    /// A `break` or `continue` outside a loop.
    pub const BREAK_OUTSIDE_LOOP: &str = "C0231";
    /// A file's item that is not a declaration.
    pub const EXPECTED_DECLARATION: &str = "C0240";
    /// A `typedef` whose definition is not a type.
    pub const EXPECTED_TYPEDEF: &str = "C0242";
}

/// What one parse produced, and everything it found wrong with it.
#[derive(Debug)]
pub struct Parsed {
    /// The syntax tree.
    pub unit: TranslationUnit,
    /// Every failure, in the order they were found.
    pub diagnostics: Vec<StageError>,
}

/// Parses a token stream into a translation unit.
pub fn parse(source: SourceId, sources: &SourceManager, tokens: Vec<Token>) -> Parsed {
    let mut parser = Parser {
        source,
        sources,
        tokens,
        end: empty_token(source, sources),
        at: 0,
        errors: Vec::new(),
        typedefs: Vec::new(),
        loop_depth: 0,
        switch_depth: 0,
    };
    let unit = parser.translation_unit();
    Parsed {
        unit,
        diagnostics: parser.errors,
    }
}

struct Parser<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    tokens: Vec<Token>,
    /// The end-of-file token every `peek` past the end returns.
    end: Token,
    at: usize,
    errors: Vec<StageError>,
    /// The `typedef` names visible here.
    ///
    /// The parser needs these and only these; the type checker keeps the
    /// authoritative table and is told about the names the parser accepted.
    typedefs: Vec<String>,
    loop_depth: u32,
    switch_depth: u32,
}

impl<'a> Parser<'a> {
    // -- token access --

    fn peek(&self) -> &Token {
        self.tokens.get(self.at).unwrap_or_else(|| self.last())
    }

    fn peek_at(&self, ahead: usize) -> &Token {
        self.tokens
            .get(self.at + ahead)
            .unwrap_or_else(|| self.last())
    }

    /// The token to use when the index is past the end.
    ///
    /// The lexer always ends its stream with exactly one end-of-file token, so the
    /// only case this covers is a stream a caller built by hand with no
    /// end-of-file at all. Rather than fabricate a token on every `peek` past
    /// the end, the end token is added once when the stream is taken, and every
    /// `peek` past the end returns it — so all of them agree.
    fn last(&self) -> &Token {
        &self.end
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.at + 1 < self.tokens.len() {
            self.at += 1;
        }
        token
    }

    fn at_eof(&self) -> bool {
        self.peek().kind == TokenKind::EndOfFile
    }

    /// A span from a pair of byte offsets, clamped into the file.
    fn span_raw(&self, start: u32, end: u32) -> SourceSpan {
        let length = self
            .sources
            .file(self.source)
            .map(|file| file.text().len() as u32)
            .unwrap_or(0);
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

    /// A span from a token through to the end of the token at `at`.
    fn span_from(&self, start: &Token, at: usize) -> SourceSpan {
        let end = self
            .tokens
            .get(at)
            .or_else(|| self.tokens.last())
            .map(|token| token.span.end().as_u32())
            .unwrap_or_else(|| start.span.end().as_u32());
        self.span_raw(start.span.start().as_u32(), end)
    }

    // -- recognition --

    fn is_punctuator(&self, ahead: usize, text: &str) -> bool {
        let token = self.peek_at(ahead);
        token.kind == TokenKind::Punctuator && token.text == text
    }

    fn is_keyword_at(&self, ahead: usize, word: &str) -> bool {
        let token = self.peek_at(ahead);
        token.kind == TokenKind::Identifier && token.value == word
    }

    /// Whether the token at `ahead` is a name that is not a keyword.
    fn is_name_at(&self, ahead: usize) -> bool {
        let token = self.peek_at(ahead);
        token.kind == TokenKind::Identifier && !is_keyword(&token.value)
    }

    /// Whether the name at `ahead` has been declared a `typedef`.
    fn is_typedef_name_at(&self, ahead: usize) -> bool {
        let token = self.peek_at(ahead);
        token.kind == TokenKind::Identifier && self.typedefs.contains(&token.value)
    }

    fn eat_punctuator(&mut self, text: &str) -> bool {
        if self.is_punctuator(0, text) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_keyword(&mut self, word: &str) -> bool {
        if self.is_keyword_at(0, word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_punctuator(&mut self, text: &str) {
        if self.eat_punctuator(text) {
            return;
        }
        let token = self.peek().clone();
        let message = alloc::format!("expected `{text}`, found {}", describe(&token));
        self.error_at(
            token.span,
            codes::EXPECTED,
            message,
            alloc::format!("`{text}` is part of this construct's syntax"),
        );
    }

    /// Records a refusal at a span.
    ///
    /// The message and the help are both `Into<String>` so a caller can write a
    /// literal or a formatted string without a `.into()` at every call site, and the
    /// help is required rather than optional: a refusal here always has a reason,
    /// because a refusal without one is not something a reader can act on.
    fn error_at(
        &mut self,
        span: SourceSpan,
        code: &str,
        message: impl Into<String>,
        help: impl Into<String>,
    ) {
        let help = help.into();
        self.errors.push(crate::diagnostic::at(
            self.source,
            self.sources,
            span,
            code,
            message,
            &[],
            Some(&help),
        ));
    }

    fn error_here(&mut self, code: &str, message: impl Into<String>, help: impl Into<String>) {
        let span = self.peek().span.clone();
        self.error_at(span, code, message, help);
    }

    /// Skips a preprocessor directive or a header name.
    ///
    /// Macro expansion is not implemented, and a directive is not C tokens, so
    /// the only honest thing to do with one is to step over it. A `#define`d
    /// name then fails in the resolver as an undeclared name, which is where a
    /// name with no definition belongs.
    fn skip_directive(&mut self) -> bool {
        match self.peek().kind {
            TokenKind::Directive | TokenKind::HeaderName => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    // -- the translation unit --

    fn translation_unit(&mut self) -> TranslationUnit {
        let mut declarations = Vec::new();
        while !self.at_eof() {
            let before = self.at;
            if self.skip_directive() {
                continue;
            }
            if let Some(declaration) = self.external_declaration() {
                declarations.push(declaration);
            }
            if self.at == before {
                // Every branch either consumes a token or reports. A branch
                // that reported *without* consuming would loop forever, so the
                // one-token of progress is made here, once, where it is visible.
                self.advance();
            }
        }
        TranslationUnit { declarations }
    }

    fn external_declaration(&mut self) -> Option<Declaration> {
        if self.is_keyword_at(0, "_Static_assert") {
            self.advance();
            return Some(Declaration::StaticAssert(self.static_assertion()));
        }
        let start = self.peek().clone();
        let (storage, extern_, static_, typedef, base) = self.declaration_specifiers();
        let Some(base) = base else {
            self.error_here(
                codes::EXPECTED_DECLARATION,
                String::from("expected a declaration"),
                String::from(
                    "a file's items are declarations, and a declaration begins with a type",
                ),
            );
            return None;
        };
        if self.eat_punctuator(";") {
            return self.bare_record_declaration(&base);
        }
        let Some(mut first) = self.declarator() else {
            self.error_at(
                self.span_from(&start, self.at),
                codes::EXPECTED_NAME,
                String::from("expected a name"),
                String::from("a declaration names what it declares"),
            );
            return None;
        };
        if self.is_punctuator(0, "{") {
            return self.function_definition(base, first, &start);
        }
        // A file-scope object's initialiser is part of its declaration, and the
        // tree keeps it on the declarator because a declaration may declare
        // several objects and each has its own.
        if self.eat_punctuator("=") {
            first.initial = Some(self.initializer());
        }
        // A `typedef` may declare several names for one type, so this does not stop
        // at the first declarator. Every name is recorded as a type name as it
        // goes, because the *next* declaration may use any of them.
        let mut declarators = vec![first];
        if let (true, Some(name)) = (typedef, &declarators[0].name) {
            self.typedefs.push(name.clone());
        }
        while self.eat_punctuator(",") {
            if let Some(mut next) = self.declarator() {
                if self.eat_punctuator("=") {
                    next.initial = Some(self.initializer());
                }
                if let (true, Some(name)) = (typedef, &next.name) {
                    self.typedefs.push(name.clone());
                }
                declarators.push(next);
            }
        }
        self.expect_punctuator(";");
        Some(Declaration::Declaration(Box::new(VarDecl {
            storage,
            extern_,
            static_,
            typedef,
            base,
            declarators,
        })))
    }

    /// A `struct S { ... };` with no declarator: the definition is the item.
    fn bare_record_declaration(&mut self, base: &TypeSpecifier) -> Option<Declaration> {
        match &base.base {
            BaseType::Record(reference) => reference
                .definition
                .as_ref()
                .map(|definition| Declaration::Record(definition.clone())),
            BaseType::Enum(reference) => reference
                .definition
                .as_ref()
                .map(|definition| Declaration::Enum(definition.clone())),
            _ => None,
        }
    }

    fn function_definition(
        &mut self,
        base: TypeSpecifier,
        declarator: Declarator,
        start: &Token,
    ) -> Option<Declaration> {
        let Some(name) = declarator.name.clone() else {
            self.error_at(
                self.span_from(start, self.at),
                codes::EXPECTED_NAME,
                String::from("a function definition needs a name"),
                String::from("`int (void) { }` defines nothing"),
            );
            return None;
        };
        // A function definition's declarator must end in a function type. One
        // that does not — `int (*f)(void) { }` — is C's rule that it defines a
        // variable holding a pointer, and there is no body for one here.
        if !matches!(
            declarator.derivation.last(),
            Some(Derivation::Function(_, _, _))
        ) {
            self.error_at(
                self.span_from(start, self.at),
                codes::EXPECTED_FUNCTION_BODY,
                String::from("expected a function body"),
                String::from(
                    "a definition is a function declarator followed by `{ ... }`; \
                     `int (*f)(void) { }` would define a variable, and a variable \
                     is declared and then assigned, not defined",
                ),
            );
            return None;
        }
        if !self.is_punctuator(0, "{") {
            self.error_here(
                codes::EXPECTED_FUNCTION_BODY,
                String::from("expected a function body"),
                String::from("a definition is a declaration followed by `{ ... }`"),
            );
            return None;
        }
        let body = self.compound_statement();
        Some(Declaration::Function(Box::new(FunctionDefinition {
            base,
            declarator,
            name,
            span: self.span_from(start, self.at),
            body: Box::new(body),
        })))
    }

    // -- declaration specifiers --

    /// Reads storage classes, qualifiers and a base type, in any order.
    ///
    /// C allows `static const int` and `const static int` equally, so this loops
    /// until nothing more can be taken. `unsigned` and `signed` are counted
    /// rather than set for the same reason, and `long` may be written twice,
    /// which is the whole difference between `long` and `long long` in C.
    fn declaration_specifiers(&mut self) -> (Storage, bool, bool, bool, Option<TypeSpecifier>) {
        let start = self.peek().clone();
        let mut storage = Storage::default();
        let mut extern_ = false;
        let mut static_ = false;
        let mut typedef = false;
        let mut base: Option<BaseType> = None;
        let mut signed = false;
        let mut unsigned = false;
        let mut longs = 0usize;
        let mut shorts = 0usize;
        // A width keyword alone *is* a type: `long` is `long int`, and C does not
        // let a second type specifier follow it. Without this a name that happens
        // to be a `typedef` is taken as the base of the declaration that follows a
        // width keyword, which is how `typedef long number;` becomes a declaration
        // of something with no name at all.
        let mut width_seen = false;
        loop {
            if self.is_keyword_at(0, "const") {
                storage.constant = true;
                self.advance();
            } else if self.is_keyword_at(0, "volatile") {
                storage.volatile = true;
                self.advance();
            } else if self.is_keyword_at(0, "restrict") {
                storage.restrict_ = true;
                self.advance();
            } else if self.is_keyword_at(0, "inline") {
                storage.inline = true;
                self.advance();
            } else if self.is_keyword_at(0, "extern") {
                extern_ = true;
                self.advance();
            } else if self.is_keyword_at(0, "static") {
                static_ = true;
                self.advance();
            } else if self.is_keyword_at(0, "typedef") {
                typedef = true;
                self.advance();
            } else if self.is_keyword_at(0, "register") || self.is_keyword_at(0, "auto") {
                // Accepted and ignored. Every local is already in a frame, and
                // this machine has no volatile-access ordering to promise.
                self.advance();
            } else if self.is_keyword_at(0, "_Noreturn") {
                self.advance();
            } else if self.is_keyword_at(0, "signed") && !signed && base.is_none() {
                signed = true;
                width_seen = true;
                self.advance();
            } else if self.is_keyword_at(0, "unsigned") && !unsigned && base.is_none() {
                unsigned = true;
                width_seen = true;
                self.advance();
            } else if self.is_keyword_at(0, "long") && base.is_none() && longs < 2 {
                longs += 1;
                width_seen = true;
                self.advance();
            } else if self.is_keyword_at(0, "short") && base.is_none() && shorts < 1 {
                shorts += 1;
                width_seen = true;
                self.advance();
            } else if base.is_none()
                // A *keyword* type may follow a width keyword — `unsigned char`
                // and `long int` are both C. A bare *name* may not, and allowing
                // it is how `typedef long number;` became a declaration of
                // something with no name.
                && (self.is_keyword_base_type()
                    || (!width_seen && self.is_typedef_name_at(0)))
            {
                base = Some(self.base_type());
            } else {
                break;
            }
        }
        let base = base
            .or_else(|| {
                if signed || unsigned || longs > 0 || shorts > 0 {
                    Some(integer_base(unsigned, longs, shorts))
                } else {
                    None
                }
            })
            .map(|base| {
                // `char` is the one base type a signedness keyword may reach, because
                // `signed char` and `unsigned char` are both C and are *different
                // types*. Applying the flags here rather than inside `base_type` is
                // what keeps `unsigned char` from being a plain `char` — and a plain
                // `char` is signed, so a `strcmp` that skips its `unsigned char`
                // comparison sees a high bit as a negative and calls `'c'` less than
                // `'a'`.
                //
                // `int` is the other, and for the opposite reason. A bare `int` after
                // a width or signedness keyword must not **undo** that keyword: C
                // says `unsigned int` is an unsigned `int`, `long int` is a `long`,
                // and `unsigned long int` is an unsigned `long`. Taking the bare
                // `int` as the whole answer made all three 32-bit signed `int`s, which
                // is how `unsigned int` came to be a signed type with nothing wrong
                // anywhere in the type checker.
                match base {
                    BaseType::Char { .. } => BaseType::Char { signed: !unsigned },
                    BaseType::Int { .. } if signed || unsigned || longs > 0 || shorts > 0 => {
                        integer_base(unsigned, longs, shorts)
                    }
                    other => other,
                }
            });
        let span = self.span_from(&start, self.at);
        let qualifiers = storage;
        (
            storage,
            extern_,
            static_,
            typedef,
            base.map(|base| TypeSpecifier {
                base,
                qualifiers,
                span,
            }),
        )
    }

    /// Whether a base type starts here.
    ///
    /// A name is a base type only if it is a `typedef`, which is the one piece
    /// of type knowledge a C parser needs and the only reason it needs any.
    fn starts_base_type(&self) -> bool {
        self.is_keyword_base_type()
            // The width and signedness keywords are a base type on their own, and
            // leaving them out is what makes `short b = 0;` inside a function look
            // like a statement: a statement cannot begin with `short`, so this
            // list is the whole of the "a statement cannot begin with a type" rule.
            || self.is_keyword_at(0, "short")
            || self.is_keyword_at(0, "long")
            || self.is_keyword_at(0, "signed")
            || self.is_keyword_at(0, "unsigned")
            || self.is_typedef_name_at(0)
    }

    /// Whether a *keyword* base type starts here.
    ///
    /// A keyword type is a base type on its own and may follow a width keyword,
    /// because `unsigned char` and `long int` are both C. A bare *name* is not,
    /// because C does not let a second type specifier that is a typedef follow a
    /// width keyword — and allowing it is how `typedef long number;` becomes a
    /// declaration of something with no name at all.
    fn is_keyword_base_type(&self) -> bool {
        self.is_keyword_at(0, "void")
            || self.is_keyword_at(0, "char")
            || self.is_keyword_at(0, "int")
            || self.is_keyword_at(0, "float")
            || self.is_keyword_at(0, "double")
            || self.is_keyword_at(0, "_Bool")
            || self.is_keyword_at(0, "struct")
            || self.is_keyword_at(0, "union")
            || self.is_keyword_at(0, "enum")
    }

    fn base_type(&mut self) -> BaseType {
        if self.eat_keyword("void") {
            return BaseType::Void;
        }
        if self.eat_keyword("_Bool") {
            return BaseType::Bool;
        }
        if self.is_keyword_at(0, "float") || self.is_keyword_at(0, "double") {
            let token = self.advance();
            self.floating_point(&token.value);
            // The compile has already failed, so this placeholder is never seen
            // by the type checker. It is a *placeholder* and not a translation,
            // and saying so is better than a silent `int` that would look like a
            // decision the compiler made on purpose.
            return BaseType::Int { unsigned: false };
        }
        if self.eat_keyword("char") {
            // The signedness is applied by the caller, which is the only place
            // that knows whether `signed` or `unsigned` was written.
            return BaseType::Char { signed: true };
        }
        if self.eat_keyword("int") {
            return BaseType::Int { unsigned: false };
        }
        if self.is_keyword_at(0, "struct") || self.is_keyword_at(0, "union") {
            return BaseType::Record(Box::new(self.record_reference()));
        }
        if self.eat_keyword("enum") {
            return BaseType::Enum(Box::new(self.enum_reference()));
        }
        if self.is_name_at(0) {
            let token = self.advance();
            return BaseType::Named(token.value);
        }
        BaseType::Int { unsigned: false }
    }

    /// Reports that a floating-point type is not representable.
    ///
    /// The parse continues, so every other mistake in the program is reported in
    /// the same run. Stopping here would mean a person fixing `float` to `int`
    /// and then discovering the next mistake, one run at a time.
    fn floating_point(&mut self, spelled: &str) {
        self.error_here(
            codes::UNSUPPORTED,
            alloc::format!("this machine has no `{spelled}`"),
            "the ISA has no floating-point instruction, so a float or a double has no \
             representation; an integer is not a substitute, because it changes what the \
             arithmetic means and what the value can hold",
        );
    }

    fn record_reference(&mut self) -> RecordReference {
        let start = self.peek().clone();
        let union = self.is_keyword_at(0, "union");
        self.advance();
        let tag = if self.is_name_at(0) {
            Some(self.advance().value)
        } else {
            None
        };
        let definition = if self.is_punctuator(0, "{") {
            Some(Box::new(self.record_body(union, tag.clone(), &start)))
        } else {
            None
        };
        RecordReference {
            union,
            tag,
            definition,
            span: self.span_from(&start, self.at),
        }
    }

    fn record_body(&mut self, union: bool, tag: Option<String>, start: &Token) -> RecordDefinition {
        self.expect_punctuator("{");
        let mut members = Vec::new();
        while !self.is_punctuator(0, "}") && !self.at_eof() {
            let before = self.at;
            if self.is_keyword_at(0, "_Static_assert") {
                self.advance();
                self.static_assertion();
                continue;
            }
            if self.skip_directive() {
                continue;
            }
            let (_, _, _, _, base) = self.declaration_specifiers();
            if self.eat_punctuator(";") {
                // A member with no declarator: C11's anonymous struct or
                // union member, whose members belong to the enclosing record.
                if let Some(base) = base {
                    members.push(Member {
                        base,
                        declarators: Vec::new(),
                        span: self.span_from(start, self.at),
                    });
                }
            } else {
                let mut declarators = Vec::new();
                if base.is_some() {
                    loop {
                        if let Some(mut declarator) = self.declarator() {
                            if self.eat_punctuator("=") {
                                declarator.initial = Some(self.initializer());
                            }
                            declarators.push(declarator);
                        }
                        if !self.eat_punctuator(",") {
                            break;
                        }
                    }
                }
                self.expect_punctuator(";");
                if let Some(base) = base {
                    members.push(Member {
                        base,
                        declarators,
                        span: self.span_from(start, self.at),
                    });
                }
            }
            if self.at == before {
                self.advance();
            }
        }
        self.expect_punctuator("}");
        RecordDefinition {
            union,
            tag,
            members: Some(members),
            span: self.span_from(start, self.at),
        }
    }

    fn enum_reference(&mut self) -> EnumReference {
        let start = self.peek().clone();
        let tag = if self.is_name_at(0) {
            Some(self.advance().value)
        } else {
            None
        };
        let definition = if self.is_punctuator(0, "{") {
            Some(Box::new(self.enum_body(tag.clone(), &start)))
        } else {
            None
        };
        EnumReference {
            tag,
            definition,
            span: self.span_from(&start, self.at),
        }
    }

    fn enum_body(&mut self, tag: Option<String>, start: &Token) -> EnumDefinition {
        self.expect_punctuator("{");
        let mut members = Vec::new();
        while !self.is_punctuator(0, "}") && !self.at_eof() {
            if !self.is_name_at(0) {
                self.error_here(
                    codes::EXPECTED,
                    String::from("expected an enumerator's name"),
                    String::from("an enum's items are names"),
                );
                self.advance();
                continue;
            }
            let name = self.advance();
            let value = if self.eat_punctuator("=") {
                Some(self.conditional_expression())
            } else {
                None
            };
            members.push(EnumeratorDefinition {
                name: name.value,
                value,
                span: name.span,
            });
            if !self.eat_punctuator(",") {
                break;
            }
        }
        self.expect_punctuator("}");
        EnumDefinition {
            tag,
            members: Some(members),
            span: self.span_from(start, self.at),
        }
    }

    fn static_assertion(&mut self) -> StaticAssert {
        let start = self.peek().clone();
        self.expect_punctuator("(");
        let condition = self.conditional_expression();
        let message = if self.eat_punctuator(",") {
            match self.peek().kind {
                TokenKind::String => Some(self.advance().value),
                _ => {
                    self.error_here(
                        codes::EXPECTED,
                        String::from("expected a string"),
                        String::from("a static assertion's message is a string literal"),
                    );
                    None
                }
            }
        } else {
            None
        };
        self.expect_punctuator(")");
        self.expect_punctuator(";");
        StaticAssert {
            condition,
            message,
            span: self.span_from(&start, self.at),
        }
    }

    // -- declarators --

    /// Parses a declarator: derivations applied outward from the name.
    fn declarator(&mut self) -> Option<Declarator> {
        let start = self.peek().clone();
        let mut derivation = self.pointer_prefix();
        let (name, tail) = self.direct_declarator();
        derivation.extend(tail);
        Some(Declarator {
            name,
            derivation,
            initial: None,
            span: self.span_from(&start, self.at),
        })
    }

    /// The `*`s and their qualifiers, which are written before the name.
    fn pointer_prefix(&mut self) -> Vec<Derivation> {
        let mut derivation = Vec::new();
        while self.is_punctuator(0, "*") {
            self.advance();
            let mut storage = Storage::default();
            loop {
                if self.eat_keyword("const") {
                    storage.constant = true;
                } else if self.eat_keyword("volatile") {
                    storage.volatile = true;
                } else if self.eat_keyword("restrict") || self.eat_keyword("__restrict") {
                    storage.restrict_ = true;
                } else {
                    break;
                }
            }
            derivation.push(Derivation::Pointer(storage));
        }
        derivation
    }

    /// The part of a declarator that touches the name.
    fn direct_declarator(&mut self) -> (Option<String>, Vec<Derivation>) {
        if self.is_punctuator(0, "(") && self.paren_is_a_declarator() {
            // A parenthesised declarator. Its derivations bind tighter than
            // whatever follows the `)`, so they go *first* in the list — which
            // is what makes `int (*f)(void)` a pointer to a function and not a
            // function returning a pointer.
            self.advance();
            let mut derivation = self.pointer_prefix();
            let (name, tail) = self.direct_declarator();
            derivation.extend(tail);
            self.expect_punctuator(")");
            while let Some(suffix) = self.derivation_suffix(true) {
                derivation.push(suffix);
            }
            return (name, derivation);
        }
        let name = if self.is_name_at(0) {
            Some(self.advance().value)
        } else {
            None
        };
        let mut tail = Vec::new();
        while let Some(derivation) = self.derivation_suffix(false) {
            tail.push(derivation);
        }
        (name, tail)
    }

    /// Whether the `(` here starts a parenthesised declarator.
    ///
    /// `int f(int)` is a function; `int (*f)(int)` is a pointer to a function;
    /// and `int (x)` is `x` in parentheses. The difference is what follows the
    /// `(`: a `*` or another `(` means a declarator, a type means a parameter
    /// list, and a bare name followed by `)` is a parenthesised *name*, which is
    /// the one case where that name is the declarator's own.
    fn paren_is_a_declarator(&self) -> bool {
        self.is_punctuator(1, "*")
            || self.is_punctuator(1, "(")
            || (self.is_name_at(1) && self.is_punctuator(2, ")") && !self.is_typedef_name_at(1))
    }

    /// One `[...]` or `(...)` after a declarator's name.
    ///
    /// `after_parens` says whether the suffix followed a *parenthesised
    /// declarator*, which is the difference between `int *f(void)` and
    /// `int (*f)(void)`. A function's own parameter list is not a parenthesised
    /// declarator and passes `false`.
    fn derivation_suffix(&mut self, after_parens: bool) -> Option<Derivation> {
        if self.eat_punctuator("[") {
            // `static`, `const`, `volatile` and `restrict` inside the brackets
            // describe the *parameter* an array type becomes, not the array
            // itself, and every one of them is accepted and ignored.
            loop {
                if self.is_keyword_at(0, "static")
                    || self.is_keyword_at(0, "const")
                    || self.is_keyword_at(0, "volatile")
                    || self.is_keyword_at(0, "restrict")
                    || self.is_keyword_at(0, "__restrict")
                {
                    self.advance();
                } else {
                    break;
                }
            }
            let length = if self.is_punctuator(0, "]") {
                None
            } else {
                Some(self.conditional_expression())
            };
            self.expect_punctuator("]");
            return Some(Derivation::Array(length));
        }
        if self.eat_punctuator("(") {
            let mut parameters = Vec::new();
            let mut variadic = false;
            if self.is_punctuator(0, "void") && self.is_punctuator(1, ")") {
                // `f(void)` is a prototype of a function that takes nothing,
                // and it is not the same as `f()` — which says nothing at all
                // about the parameters. Only in C, and only for the truth: `f`
                // and `T` are the two names this language spells the same way.
                self.advance();
                self.advance();
                // `f(void)` — no parentheses were written, so the base type is the return
                // type and everything to the left of these parentheses belongs there.
                return Some(Derivation::Function(parameters, false, false));
            }
            if !self.is_punctuator(0, ")") {
                if self.is_punctuator(0, "...") {
                    self.advance();
                    variadic = true;
                } else {
                    loop {
                        if self.is_punctuator(0, "...") {
                            self.advance();
                            variadic = true;
                            break;
                        }
                        parameters.push(self.parameter());
                        if !self.eat_punctuator(",") {
                            break;
                        }
                    }
                }
            }
            self.expect_punctuator(")");
            // The parentheses were written, so what came before them is a pointer to
            // this function rather than a return type of it.
            return Some(Derivation::Function(parameters, variadic, after_parens));
        }
        None
    }

    /// One parameter in a function declarator.
    fn parameter(&mut self) -> ParameterDeclaration {
        let start = self.peek().clone();
        let (storage, _, _, _, base) = self.declaration_specifiers();
        let base = base.unwrap_or(TypeSpecifier {
            base: BaseType::Int { unsigned: false },
            qualifiers: storage,
            span: self.span_from(&start, self.at),
        });
        if self.is_punctuator(0, ")") || self.is_punctuator(0, ",") {
            // A parameter with no name: `int f(int)`, and `int f(int *)`. Both
            // are prototypes and neither needs a name to be legal.
            return ParameterDeclaration {
                storage,
                base,
                declarator: None,
                span: self.span_from(&start, self.at),
            };
        }
        let declarator = self.declarator();
        ParameterDeclaration {
            storage,
            base,
            declarator,
            span: self.span_from(&start, self.at),
        }
    }

    // -- statements --

    fn compound_statement(&mut self) -> Block {
        let start = self.peek().clone();
        self.expect_punctuator("{");
        let mut items = Vec::new();
        while !self.is_punctuator(0, "}") && !self.at_eof() {
            let before = self.at;
            if self.skip_directive() {
                continue;
            }
            if let Some(item) = self.block_item() {
                items.push(item);
            }
            if self.at == before {
                self.advance();
            }
        }
        self.expect_punctuator("}");
        Block {
            items,
            span: self.span_from(&start, self.at),
        }
    }

    fn block_item(&mut self) -> Option<BlockItem> {
        if self.is_keyword_at(0, "_Static_assert") {
            self.advance();
            return Some(BlockItem::Statement(Statement::StaticAssert(Box::new(
                self.static_assertion(),
            ))));
        }
        if !self.is_declaration() {
            return Some(BlockItem::Statement(self.statement()));
        }
        let (storage, extern_, static_, typedef, base) = self.declaration_specifiers();
        let base = base?;
        if self.eat_punctuator(";") {
            return None;
        }
        let mut declarators = Vec::new();
        while let Some(mut declarator) = self.declarator() {
            if self.eat_punctuator("=") {
                declarator.initial = Some(self.initializer());
            }
            if let (true, Some(name)) = (typedef, &declarator.name) {
                self.typedefs.push(name.clone());
            }
            declarators.push(declarator);
            if !self.eat_punctuator(",") {
                break;
            }
        }
        self.expect_punctuator(";");
        // A `typedef` is a declaration whose declarators are type names, so it
        // is one node rather than two: a separate `TypeAlias` would need every
        // consumer to remember that it is a declaration too.
        Some(BlockItem::Declaration(Box::new(VarDecl {
            storage,
            extern_,
            static_,
            typedef,
            base,
            declarators,
        })))
    }

    /// Whether a declaration begins here rather than a statement.
    ///
    /// This is C's own ambiguity and its own answer: a statement cannot begin
    /// with a type, so anything that looks like one is a declaration. A name
    /// that is not a `typedef` is an expression, which is why the check asks the
    /// table rather than only the keyword list.
    fn is_declaration(&self) -> bool {
        self.starts_base_type()
            || self.is_keyword_at(0, "const")
            || self.is_keyword_at(0, "volatile")
            || self.is_keyword_at(0, "static")
            || self.is_keyword_at(0, "extern")
            || self.is_keyword_at(0, "register")
            || self.is_keyword_at(0, "auto")
            || self.is_keyword_at(0, "inline")
            || self.is_keyword_at(0, "restrict")
            || self.is_keyword_at(0, "__restrict")
            || self.is_keyword_at(0, "typedef")
    }

    /// Parses an initialiser, keeping its shape.
    fn initializer(&mut self) -> Initializer {
        if !self.is_punctuator(0, "{") {
            return Initializer::Scalar(self.assignment_expression());
        }
        let start = self.peek().clone();
        self.advance();
        let mut items = Vec::new();
        while !self.is_punctuator(0, "}") && !self.at_eof() {
            let before = self.at;
            let designator = if self.eat_punctuator(".") {
                let name = self.member_name();
                Some(Designator::Field {
                    name: name.0,
                    span: name.1,
                })
            } else if self.eat_punctuator("[") {
                let index = self.conditional_expression();
                let span = self.span_from(&self.peek().clone(), self.at);
                self.expect_punctuator("]");
                Some(Designator::Index { index, span })
            } else {
                None
            };
            if designator.is_some() {
                self.expect_punctuator("=");
            }
            items.push(InitItem {
                designator,
                value: Box::new(self.initializer()),
            });
            if !self.eat_punctuator(",") {
                break;
            }
            if self.at == before {
                self.advance();
            }
        }
        self.expect_punctuator("}");
        Initializer::List {
            items,
            span: self.span_from(&start, self.at),
        }
    }

    fn statement(&mut self) -> Statement {
        if self.is_punctuator(0, "{") {
            return Statement::Block(Box::new(self.compound_statement()));
        }
        if self.eat_punctuator(";") {
            return Statement::Empty;
        }
        if self.is_keyword_at(0, "if") {
            self.advance();
            self.expect_punctuator("(");
            let condition = self.expression();
            self.expect_punctuator(")");
            let then_branch = Box::new(self.statement());
            let else_branch = if self.eat_keyword("else") {
                Some(Box::new(self.statement()))
            } else {
                None
            };
            return Statement::If {
                condition,
                then_branch,
                else_branch,
            };
        }
        if self.is_keyword_at(0, "while") {
            self.advance();
            self.expect_punctuator("(");
            let condition = self.expression();
            self.expect_punctuator(")");
            self.loop_depth += 1;
            let body = Box::new(self.statement());
            self.loop_depth -= 1;
            return Statement::While { condition, body };
        }
        if self.is_keyword_at(0, "do") {
            self.advance();
            self.loop_depth += 1;
            let body = Box::new(self.statement());
            self.loop_depth -= 1;
            if !self.eat_keyword("while") {
                self.error_here(
                    codes::EXPECTED,
                    String::from("expected `while`"),
                    String::from("a `do` statement ends with `while (condition);`"),
                );
            }
            self.expect_punctuator("(");
            let condition = self.expression();
            self.expect_punctuator(")");
            self.expect_punctuator(";");
            return Statement::DoWhile { body, condition };
        }
        if self.is_keyword_at(0, "for") {
            self.advance();
            self.expect_punctuator("(");
            let initialiser = self.for_initialiser();
            let condition = if self.is_punctuator(0, ";") {
                None
            } else {
                Some(self.expression())
            };
            self.expect_punctuator(";");
            let step = if self.is_punctuator(0, ")") {
                None
            } else {
                Some(self.expression())
            };
            self.expect_punctuator(")");
            self.loop_depth += 1;
            let body = Box::new(self.statement());
            self.loop_depth -= 1;
            return Statement::For {
                initialiser,
                condition,
                step,
                body,
            };
        }
        if self.is_keyword_at(0, "switch") {
            self.advance();
            self.expect_punctuator("(");
            let condition = self.expression();
            self.expect_punctuator(")");
            self.switch_depth += 1;
            let body = Box::new(self.statement());
            self.switch_depth -= 1;
            return Statement::Switch { condition, body };
        }
        if self.is_keyword_at(0, "case") {
            self.advance();
            if self.switch_depth == 0 {
                self.error_here(
                    codes::CASE_OUTSIDE_SWITCH,
                    String::from("a `case` label is only meaningful inside a `switch`"),
                    String::from(
                        "a `case` outside a switch is never reached, so it labels nothing",
                    ),
                );
            }
            let value = self.conditional_expression();
            self.expect_punctuator(":");
            let statement = Box::new(self.statement());
            return Statement::Case { value, statement };
        }
        if self.is_keyword_at(0, "default") {
            self.advance();
            if self.switch_depth == 0 {
                self.error_here(
                    codes::CASE_OUTSIDE_SWITCH,
                    String::from("a `default` label is only meaningful inside a `switch`"),
                    String::from("a `default` outside a switch is never reached"),
                );
            }
            self.expect_punctuator(":");
            let statement = Box::new(self.statement());
            return Statement::Default { statement };
        }
        if self.eat_keyword("break") {
            if self.loop_depth == 0 && self.switch_depth == 0 {
                self.error_here(
                    codes::BREAK_OUTSIDE_LOOP,
                    String::from("there is no loop or `switch` to break out of"),
                    String::from("`break` applies to the innermost enclosing loop or switch"),
                );
            }
            let span = self.span_from(&self.tokens[self.at.saturating_sub(1)].clone(), self.at);
            self.expect_punctuator(";");
            return Statement::Break(span);
        }
        if self.eat_keyword("continue") {
            if self.loop_depth == 0 {
                self.error_here(
                    codes::BREAK_OUTSIDE_LOOP,
                    String::from("there is no loop to continue"),
                    String::from("`continue` applies to the innermost enclosing loop"),
                );
            }
            let span = self.span_from(&self.tokens[self.at.saturating_sub(1)].clone(), self.at);
            self.expect_punctuator(";");
            return Statement::Continue(span);
        }
        if self.eat_keyword("return") {
            let start = self.tokens[self.at.saturating_sub(1)].clone();
            let value = if self.is_punctuator(0, ";") {
                None
            } else {
                Some(self.expression())
            };
            self.expect_punctuator(";");
            return Statement::Return {
                value,
                span: self.span_from(&start, self.at),
            };
        }
        if self.eat_keyword("goto") {
            let start = self.tokens[self.at.saturating_sub(1)].clone();
            let name = if self.is_name_at(0) {
                self.advance().value
            } else {
                self.error_here(
                    codes::EXPECTED,
                    String::from("expected a label's name"),
                    String::from("`goto` is followed by the label it jumps to"),
                );
                String::new()
            };
            self.expect_punctuator(";");
            return Statement::Goto {
                name,
                span: self.span_from(&start, self.at),
            };
        }
        if self.is_name_at(0) && self.is_punctuator(1, ":") {
            let name = self.advance();
            let span = name.span;
            let name = name.value;
            self.advance();
            let statement = Box::new(self.statement());
            return Statement::Label {
                name,
                statement,
                span,
            };
        }
        let expression = self.expression();
        self.expect_punctuator(";");
        Statement::Expression(expression)
    }

    /// A `for`'s initialiser, which is either a declaration or an expression.
    fn for_initialiser(&mut self) -> Option<Box<ForInit>> {
        if self.is_punctuator(0, ";") {
            self.advance();
            return None;
        }
        if self.is_declaration() {
            let (storage, extern_, static_, typedef, base) = self.declaration_specifiers();
            let base = base.unwrap_or(TypeSpecifier {
                base: BaseType::Int { unsigned: false },
                qualifiers: storage,
                span: self.peek().span.clone(),
            });
            let mut declarators = Vec::new();
            while let Some(mut declarator) = self.declarator() {
                if self.eat_punctuator("=") {
                    declarator.initial = Some(self.initializer());
                }
                if let (true, Some(name)) = (typedef, &declarator.name) {
                    self.typedefs.push(name.clone());
                }
                declarators.push(declarator);
                if !self.eat_punctuator(",") {
                    break;
                }
            }
            self.expect_punctuator(";");
            if typedef {
                // A `typedef` in a `for`'s initialiser is a declaration, and C
                // says it is scoped to the loop. There is nowhere to put a
                // statement in this tree, so it is reported rather than
                // silently hoisted into the enclosing block.
                self.error_here(
                    codes::UNSUPPORTED,
                    String::from("a `typedef` cannot be declared in a `for`'s initialiser"),
                    String::from("declare the type before the loop"),
                );
                return None;
            }
            return Some(Box::new(ForInit::Declaration(Box::new(VarDecl {
                storage,
                extern_,
                static_,
                typedef,
                base,
                declarators,
            }))));
        }
        let expression = self.expression();
        self.expect_punctuator(";");
        Some(Box::new(ForInit::Expression(expression)))
    }

    // -- expressions --

    /// A full expression, comma included.
    fn expression(&mut self) -> Expression {
        let mut left = self.assignment_expression();
        while self.is_punctuator(0, ",") {
            self.advance();
            let right = self.assignment_expression();
            left = Expression::Comma {
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        left
    }

    /// An assignment, which is right-associative.
    fn assignment_expression(&mut self) -> Expression {
        let left = self.conditional_expression();
        // Each compound assignment is its own operator, because `a += b`
        // converts back to `a`'s type and `a = a + b` does not.
        const COMPOUND: &[(&str, BinaryOp)] = &[
            ("+=", BinaryOp::Add),
            ("-=", BinaryOp::Subtract),
            ("*=", BinaryOp::Multiply),
            ("/=", BinaryOp::Divide),
            ("%=", BinaryOp::Remainder),
            ("<<=", BinaryOp::ShiftLeft),
            (">>=", BinaryOp::ShiftRight),
            ("&=", BinaryOp::BitAnd),
            ("|=", BinaryOp::BitOr),
            ("^=", BinaryOp::BitXor),
        ];
        if self.eat_punctuator("=") {
            let value = self.assignment_expression();
            return Expression::Assign {
                target: Box::new(left),
                value: Box::new(value),
            };
        }
        for (text, op) in COMPOUND {
            if !self.is_punctuator(0, text) {
                continue;
            }
            self.advance();
            let value = self.assignment_expression();
            return Expression::CompoundAssign {
                op: *op,
                target: Box::new(left),
                value: Box::new(value),
            };
        }
        left
    }

    fn conditional_expression(&mut self) -> Expression {
        let condition = self.binary_expression(0);
        if !self.is_punctuator(0, "?") {
            return condition;
        }
        self.advance();
        let then_value = self.expression();
        self.expect_punctuator(":");
        let else_value = self.conditional_expression();
        Expression::Conditional {
            condition: Box::new(condition),
            then_value: Box::new(then_value),
            else_value: Box::new(else_value),
        }
    }

    /// The binary operators, by precedence level, lowest first.
    ///
    /// `&&` and `||` are handled at level 0 before the table, because they are
    /// short-circuiting and produce an `int`, which makes them different kinds
    /// of node rather than different precedences of the same one.
    fn binary_expression(&mut self, level: usize) -> Expression {
        const LEVELS: &[&[(&str, BinaryOp)]] = &[
            &[("|", BinaryOp::BitOr)],
            &[("^", BinaryOp::BitXor)],
            &[("&", BinaryOp::BitAnd)],
            &[("==", BinaryOp::Equal), ("!=", BinaryOp::NotEqual)],
            &[
                ("<", BinaryOp::Less),
                (">", BinaryOp::Greater),
                ("<=", BinaryOp::LessEqual),
                (">=", BinaryOp::GreaterEqual),
            ],
            &[("<<", BinaryOp::ShiftLeft), (">>", BinaryOp::ShiftRight)],
            &[("+", BinaryOp::Add), ("-", BinaryOp::Subtract)],
            &[
                ("*", BinaryOp::Multiply),
                ("/", BinaryOp::Divide),
                ("%", BinaryOp::Remainder),
            ],
        ];
        if level >= LEVELS.len() {
            return self.unary_expression();
        }
        // Level zero is `|`, and it sits *above* `&&` and `||` in C's table even
        // though it is in this table: the logical operators are handled by their
        // own functions because they short-circuit, so the operands of `|` are
        // `logical_or` and the operands of `&&` are this level's successor. Every
        // other level recurses in the ordinary way.
        let mut left = if level == 0 {
            self.logical_or()
        } else {
            self.binary_expression(level + 1)
        };
        loop {
            let mut matched = None;
            for (text, op) in LEVELS[level] {
                if self.is_punctuator(0, text) {
                    matched = Some(*op);
                    break;
                }
            }
            let Some(op) = matched else { return left };
            self.advance();
            let right = if level == 0 {
                self.logical_or()
            } else {
                self.binary_expression(level + 1)
            };
            left = Expression::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    /// `&&` and `||`, which short-circuit and produce an `int`.
    fn logical_and(&mut self) -> Expression {
        let mut left = self.binary_expression(1);
        while self.is_punctuator(0, "&&") {
            self.advance();
            let right = self.binary_expression(1);
            left = Expression::Logical {
                and: true,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        left
    }

    /// The `||` level, which is left-associative over `&&`.
    fn logical_or(&mut self) -> Expression {
        let mut left = self.logical_and();
        while self.is_punctuator(0, "||") {
            self.advance();
            let right = self.logical_and();
            left = Expression::Logical {
                and: false,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        left
    }

    fn unary_expression(&mut self) -> Expression {
        if self.eat_punctuator("+") {
            return Expression::Plus(Box::new(self.unary_expression()));
        }
        if self.eat_punctuator("-") {
            return Expression::Minus(Box::new(self.unary_expression()));
        }
        if self.eat_punctuator("~") {
            return Expression::BitNot(Box::new(self.unary_expression()));
        }
        if self.eat_punctuator("!") {
            return Expression::Not(Box::new(self.unary_expression()));
        }
        if self.eat_punctuator("*") {
            return Expression::Dereference(Box::new(self.unary_expression()));
        }
        if self.eat_punctuator("&") {
            return Expression::Address(Box::new(self.unary_expression()));
        }
        if self.is_punctuator(0, "++") || self.is_punctuator(0, "--") {
            let increment = self.is_punctuator(0, "++");
            self.advance();
            return Expression::Increment {
                operand: Box::new(self.unary_expression()),
                increment,
                prefix: true,
            };
        }
        if self.is_keyword_at(0, "sizeof") {
            self.advance();
            if self.is_punctuator(0, "(") && self.starts_base_type_at(1) {
                self.advance();
                let name = self.type_name();
                self.expect_punctuator(")");
                return Expression::SizeofType(Box::new(name));
            }
            // `sizeof (x)` is `sizeof x` when `x` is an expression, and the
            // parentheses are the ambiguity C has and resolves with the type
            // table. The operand is *not* evaluated, so it is parsed and
            // discarded rather than turned into code.
            let operand = self.unary_expression();
            return Expression::SizeofExpression(Box::new(operand));
        }
        self.postfix_expression()
    }

    /// Whether a type starts at `ahead`, for `sizeof (T)` and casts.
    fn starts_base_type_at(&self, ahead: usize) -> bool {
        let token = self.peek_at(ahead);
        if token.kind != TokenKind::Identifier {
            return false;
        }
        match token.value.as_str() {
            "void" | "char" | "int" | "float" | "double" | "struct" | "union" | "enum"
            | "_Bool" | "const" | "volatile" | "signed" | "unsigned" | "long" | "short" => true,
            _ => self.typedefs.contains(&token.value),
        }
    }

    fn postfix_expression(&mut self) -> Expression {
        let mut expression = self.primary_expression();
        loop {
            if self.eat_punctuator("[") {
                let index = self.expression();
                self.expect_punctuator("]");
                expression = Expression::Subscript {
                    array: Box::new(expression),
                    index: Box::new(index),
                };
                continue;
            }
            if self.eat_punctuator("(") {
                let mut arguments = Vec::new();
                if !self.is_punctuator(0, ")") {
                    loop {
                        arguments.push(self.assignment_expression());
                        if !self.eat_punctuator(",") {
                            break;
                        }
                    }
                }
                self.expect_punctuator(")");
                expression = Expression::Call {
                    callee: Box::new(expression),
                    arguments,
                };
                continue;
            }
            if self.eat_punctuator(".") {
                let (name, span) = self.member_name();
                expression = Expression::Member {
                    record: Box::new(expression),
                    member: name,
                    arrow: false,
                    span,
                };
                continue;
            }
            if self.eat_punctuator("->") {
                let (name, span) = self.member_name();
                expression = Expression::Member {
                    record: Box::new(expression),
                    member: name,
                    arrow: true,
                    span,
                };
                continue;
            }
            if self.is_punctuator(0, "++") || self.is_punctuator(0, "--") {
                let increment = self.is_punctuator(0, "++");
                self.advance();
                expression = Expression::Increment {
                    operand: Box::new(expression),
                    increment,
                    prefix: false,
                };
                continue;
            }
            return expression;
        }
    }

    fn member_name(&mut self) -> (String, SourceSpan) {
        if self.is_name_at(0) {
            let token = self.advance();
            (token.value, token.span)
        } else {
            self.error_here(
                codes::EXPECTED,
                String::from("expected a member's name"),
                String::from("`.` and `->` are followed by a member's name"),
            );
            (String::new(), self.peek().span.clone())
        }
    }

    fn primary_expression(&mut self) -> Expression {
        let token = self.peek().clone();
        match token.kind {
            TokenKind::Integer => {
                self.advance();
                Expression::Integer {
                    number: token.number,
                    span: token.span,
                }
            }
            TokenKind::Character => {
                self.advance();
                Expression::Character {
                    value: token.character,
                    span: token.span,
                }
            }
            TokenKind::String => {
                self.advance();
                Expression::String {
                    value: token.value,
                    span: token.span,
                }
            }
            TokenKind::Identifier if is_keyword(&token.value) => {
                self.error_here(
                    codes::UNEXPECTED_KEYWORD,
                    alloc::format!(
                        "`{}` is a keyword and cannot be used as a value",
                        token.value
                    ),
                    "if you wanted a name that happens to be spelled like a keyword, rename it",
                );
                self.advance();
                Expression::Name {
                    name: token.value,
                    span: token.span,
                }
            }
            TokenKind::Identifier => {
                self.advance();
                Expression::Name {
                    name: token.value,
                    span: token.span,
                }
            }
            TokenKind::Punctuator if token.text == "(" => {
                // A cast if a type starts inside the parentheses, and a
                // parenthesised expression otherwise. `(x + 1) * 2` and
                // `(int) 1` differ only in what is inside, which is why C needs
                // the typedef table to tell them apart.
                if self.starts_base_type_at(1) {
                    self.advance();
                    let name = self.type_name();
                    self.expect_punctuator(")");
                    return Expression::Cast {
                        ty: Box::new(name),
                        operand: Box::new(self.unary_expression()),
                    };
                }
                self.advance();
                let inner = self.expression();
                self.expect_punctuator(")");
                Expression::Group(Box::new(inner))
            }
            _ => {
                self.error_here(
                    codes::UNEXPECTED_STATEMENT,
                    alloc::format!("expected an expression, found {}", describe(&token)),
                    String::from(
                        "an expression is a value, a name, a call, or an operator applied to one",
                    ),
                );
                self.advance();
                Expression::Name {
                    name: String::new(),
                    span: token.span,
                }
            }
        }
    }

    /// A type name, as `sizeof (T)` and a cast both need.
    fn type_name(&mut self) -> TypeName {
        let start = self.peek().clone();
        let (_, _, _, _, base) = self.declaration_specifiers();
        let mut derivation = self.pointer_prefix();
        // An abstract declarator may still have an array or function suffix, and
        // those bind *outside* the pointers: `int *[3]` is an array of pointers.
        while let Some(suffix) = self.derivation_suffix(false) {
            derivation.push(suffix);
        }
        let span = self.span_from(&start, self.at);
        let base = base.unwrap_or(TypeSpecifier {
            base: BaseType::Int { unsigned: false },
            qualifiers: Storage::default(),
            span: span.clone(),
        });
        TypeName {
            base,
            derivation,
            span,
        }
    }
}

/// A zero-width end-of-file token, for a stream that has none.
fn empty_token(source: SourceId, sources: &SourceManager) -> Token {
    let span = sources
        .source_span(source, ByteOffset::new(0), ByteOffset::new(0))
        .expect("a zero-length span is always valid");
    Token {
        kind: TokenKind::EndOfFile,
        text: String::new(),
        value: String::new(),
        number: crate::lexer::Number::default(),
        character: 0,
        span,
    }
}

/// The integer base type a set of signedness and width keywords describes.
///
/// C's rules for the no-width case: `int` and `signed` are `int`, and `unsigned` is
/// `unsigned int`. Neither of those was right here — `unsigned` became a signed
/// `int` because `BaseType::Int` had nowhere to put the flag, and a bare `signed`
/// became a **char**, which is a different width and not merely a different sign.
/// Both are silent: a program using `unsigned int` ran and produced wrong answers.
fn integer_base(unsigned: bool, longs: usize, shorts: usize) -> BaseType {
    match (longs, shorts) {
        (0, 0) => BaseType::Int { unsigned },
        (1 | 2, 0) => BaseType::Long {
            doubled: longs == 2,
            unsigned,
        },
        (0, 1) => BaseType::Short { unsigned },
        // Unreachable for a well-formed specifier list: the caller only calls this
        // when no base keyword was seen, so `long` and `short` were never both
        // written. Answering with an `int` rather than panicking is right anyway,
        // because the checker will report the conflicting specifiers.
        _ => BaseType::Int { unsigned },
    }
}

/// How a token is described in a diagnostic.
fn describe(token: &Token) -> String {
    match token.kind {
        TokenKind::EndOfFile => String::from("the end of the file"),
        TokenKind::String | TokenKind::Character => {
            alloc::format!("`{}`", token.text)
        }
        TokenKind::Integer | TokenKind::Float => alloc::format!("`{}`", token.text),
        TokenKind::Identifier => alloc::format!("`{}`", token.value),
        TokenKind::Punctuator | TokenKind::Directive | TokenKind::HeaderName => {
            if token.text.is_empty() {
                String::from("nothing")
            } else {
                alloc::format!("`{}`", token.text)
            }
        }
    }
}
