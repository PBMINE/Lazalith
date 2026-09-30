//! The preprocessor.
//!
//! Until this stage existed, a `#` line was lexed and then *stepped over*: the parser had
//! `skip_directive`, which consumed a directive and said nothing. That was defensible when
//! there was no header to include and no macro to expand — there was nothing a directive
//! could have said that mattered. It stops being defensible the moment a freestanding
//! kernel needs `<stdint.h>`, because then a directive is the only way to name a type.
//!
//! So this is the whole of C's preprocessing that a kernel actually uses, and it is worth
//! being exact about what that is:
//!
//! | Directive | Here | Why |
//! | --- | --- | --- |
//! | `#include` | yes | the entire reason this stage exists |
//! | `#define`, object-like | yes | constants, feature switches, type names |
//! | `#define`, function-like | yes | `#define MIN(a, b)` and its kind |
//! | `#undef` | yes | the "is this already defined" idiom |
//! | `#ifdef`, `#ifndef`, `#else`, `#endif` | yes | include guards, feature selection |
//! | `#error` | yes | "this configuration cannot work" is worth saying out loud |
//! | `#if`, `#elif` | **no** | see below |
//! | `#pragma` | ignored, on purpose | see below |
//!
//! # Why there is no `#if`
//!
//! `#if` evaluates a constant expression, and that needs an integer expression evaluator
//! with the full C operator set, `defined`, and a defined answer for division by zero.
//! Every include guard in every header in existence is `#ifndef` — a *definedness* test,
//! not an expression — so guards and feature switches are covered without it.
//!
//! What is refused is the alternative to refusing: **an unimplemented `#if` is a
//! diagnostic, not a skipped line.** The old behaviour made `#if 1` and `#if 0` the same
//! program, silently, and a kernel that believed a feature was compiled out would find
//! out at run time. A missing feature reported as a missing feature is worth more than a
//! build that proceeds and is wrong.
//!
//! # `#pragma` is the exception, and it is not a mistake
//!
//! `#pragma` is the language's way of saying *this is for someone else's tool*. Refusing
//! it would make the front end unusable with any real header, and honouring it is
//! impossible without knowing what it means. So it is skipped, and it is the **only**
//! directive that is: an unrecognised `#foobar` is a diagnostic, because that one is a
//! typo, and a typo that is ignored is a bug that is not.
//!
//! # It works on tokens, not on text
//!
//! A text-level preprocessor rewrites the program into a new string and then has to
//! reconstruct a line map for it, because every span in the parser is a byte offset into
//! some file. That reconstruction is where preprocessors grow the bugs that report a type
//! error in the wrong place.
//!
//! This one lexes each file **once, exactly as written**, so every token's span is exact
//! and needs no reconstruction. Directives are the lines the lexer already marked, and
//! macro expansion *replaces tokens with other tokens*.
//!
//! The one thing a caller may want different is a diagnostic *inside* a macro body, which
//! after expansion points at the invocation rather than at the `#define`. That is a
//! deliberate trade, and it is the only one here: GCC and Clang point at the definition,
//! which is better when the macro is wrong and worse when the *call* is wrong. A kernel's
//! headers are read far more often than they are edited, and pointing at the call site
//! answers the question the reader actually has.
//!
//! # What is deliberately not here
//!
//! - **Arguments must be on one line.** `MIN(\n a, b)` does not expand, because expansion
//!   is line-at-a-time to keep conditional groups and line boundaries honest. A kernel
//!   writes its calls on one line; a formatter that wants otherwise wants a real
//!   preprocessor.
//! - **No `##`, no `#`, no stringification.** Nothing in a kernel header needs them, and a
//!   half-implemented paste is a silent miscompile.
//! - **No `#line`, no `_Pragma`, and no `__DATE__`.** `__DATE__` is *refused* rather than
//!   defined: a macro that changes a program's meaning depending on when it was built is
//!   not a fact about the program.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use lazalith_diagnostics::Diagnostic;
use lazalith_types::{ByteOffset, SourceId, SourceManager, SourceSpan};

use crate::lexer::{self, Number, Token, TokenKind};

/// This stage's diagnostic codes.
pub mod codes {
    /// A header that includes itself, directly or through others.
    pub const INCLUDE_CYCLE: &str = "C0601";
    /// A header name that no include path resolved.
    pub const HEADER_NOT_FOUND: &str = "C0602";
    /// A conditional group that was never closed.
    pub const UNCLOSED_CONDITIONAL: &str = "C0603";
    /// A conditional directive with nothing open to act on, or two `#else`s.
    pub const MISPLACED_CONDITIONAL: &str = "C0604";
    /// A directive this preprocessor does not implement.
    pub const UNSUPPORTED_DIRECTIVE: &str = "C0605";
    /// A macro redefined with a different body.
    pub const REDEFINED_MACRO: &str = "C0606";
    /// A function-like macro called with the wrong number of arguments.
    pub const ARGUMENT_COUNT: &str = "C0607";
    /// `#include` nested past the limit.
    pub const INCLUDE_TOO_DEEP: &str = "C0608";
    /// Expansion that did not stop.
    pub const EXPANSION_BUDGET: &str = "C0609";
    /// A header name the directive grammar does not allow.
    pub const MALFORMED_INCLUDE: &str = "C0610";
    /// `#error`, and `#error` with nothing after it.
    pub const ERROR_DIRECTIVE: &str = "C0611";
    /// A `#define` or `#undef` with no name.
    pub const NAMELESS_DEFINE: &str = "C0612";
    /// A function-like macro whose parameter list is not a parameter list.
    pub const MALFORMED_PARAMETERS: &str = "C0613";
}

/// How deep `#include` may nest before the preprocessor gives up.
///
/// **Twenty is not arbitrary: it is the point at which someone should look.** C's own
/// standard requires at least fifteen levels of conditional inclusion, so twenty is above
/// the language's floor; a kernel's headers nest three or four deep. The limit exists so
/// that a cycle which somehow evades [`codes::INCLUDE_CYCLE`] — a resolver that answers
/// differently each time it is asked, which a generated-header resolver quite plausibly
/// does — is a diagnostic rather than an exhausted machine.
const MAX_INCLUDE_DEPTH: usize = 20;

/// The most tokens macro expansion may produce for one line.
///
/// **A bound, because the failure mode without one is the machine stopping.** Object-like
/// expansion is guarded against direct self-reference by painting the substituted name, and
/// that is enough for `#define f(x) f(x)` and for `#define A A`. Mutual recursion —
/// `#define A B` with `#define B A` — has no such local fix, and a preprocessor that loops
/// on it hangs the build rather than reporting a bug in the header.
///
/// The number is far above any real line. A kernel header's most expansive macro is a
/// register accessor, a few hundred tokens. Sixty-four thousand is a line that has already
/// gone wrong, and a caller that reaches it gets [`codes::EXPANSION_BUDGET`] instead of a
/// build that never finishes.
const BUDGET: usize = 65_536;

/// Finds the text of a header.
///
/// **A trait, and not a filesystem call, because this crate is `no_std`.** Deciding *where*
/// headers live is a host policy — a directory, a sysroot, a map of built-in headers, a
/// test with three strings in it — and a front end that reads files itself has made that
/// policy unrepresentable. The caller supplies a resolver; this stage supplies the
/// semantics.
pub trait IncludeResolver {
    /// The text of `name`, or `None` if this resolver does not have it.
    ///
    /// `name` is the header as written, without the `<>` or `""`: `lazos/syscall.h`, not
    /// `"lazos/syscall.h"`. Whether the angle form or the quote form searches a different
    /// set of directories is the resolver's business, and it learns which was used from
    /// `angled`.
    fn resolve(&mut self, name: &str, angled: bool) -> Option<String>;
}

/// A resolver that has no headers.
///
/// **The default, so that "no include path" is a decision rather than a `None` check
/// scattered through the stage.** A program with no `#include` behaves identically with
/// this and with a real resolver; a program with one gets [`codes::HEADER_NOT_FOUND`]
/// pointing at the `#include`, which is the diagnostic a person needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoIncludes;

impl IncludeResolver for NoIncludes {
    fn resolve(&mut self, _name: &str, _angled: bool) -> Option<String> {
        None
    }
}

/// A resolver over a fixed set of headers.
///
/// **This is what a sysroot, a test and an embedded build all want**, and it is a map
/// rather than a trait implementation because the resolver *is* the policy and the policy
/// here is a set of names to texts. A real build wraps its include path in its own type.
#[derive(Clone, Debug, Default)]
pub struct MapIncludes {
    headers: BTreeMap<String, String>,
}

impl MapIncludes {
    /// A resolver with nothing in it.
    pub fn new() -> Self {
        MapIncludes::default()
    }

    /// Adds a header, replacing any header of the same name.
    pub fn insert(&mut self, name: &str, text: &str) {
        self.headers.insert(name.to_string(), text.to_string());
    }

    /// The names here, which is what a diagnostic about a missing one should list: a
    /// person who misspells `stdint.h` needs to see the near misses.
    pub fn names(&self) -> Vec<&str> {
        self.headers.keys().map(String::as_str).collect()
    }
}

impl IncludeResolver for MapIncludes {
    fn resolve(&mut self, name: &str, _angled: bool) -> Option<String> {
        self.headers.get(name).cloned()
    }
}

/// How to preprocess: the target, and where headers come from.
///
/// **No `Default`, and that is a decision rather than an omission.** A default resolver
/// would have to be a `&mut` to something that outlives the `Includes` value, which in
/// practice means a `static` (which cannot be borrowed mutably) or a leaked allocation
/// (which is a leak in a compiler that is supposed to be run in a build). So a caller that
/// has no headers says so by holding a [`NoIncludes`] of its own, which is a line rather
/// than a hidden allocation.
pub struct Includes<'a> {
    /// `"lz64"`, `"lz32"`, or `None` to define neither `__LZ64__` nor `__LZ32__`.
    ///
    /// **A `&str` and not an `ArchitectureConfig`, because the architecture is a header
    /// question here and only a header question.** This crate must not depend on the
    /// machine crate, and `c_goes_through_the_abi_rather_than_around_it` already guards
    /// the dependency that does matter. Passing the name keeps the preprocessor's
    /// question — *which macros are defined* — separable from everything that decides
    /// what a word means.
    pub arch: Option<&'a str>,
    /// Where headers come from.
    pub resolver: &'a mut dyn IncludeResolver,
}

impl<'a> Includes<'a> {
    /// Preprocessing with no headers and no architecture, from a caller that has no better
    /// idea.
    ///
    /// **This is what "no include path" means, written as a constructor** rather than as a
    /// `Default` impl, because it needs a place to put the empty resolver and that place
    /// has to live as long as the borrow.
    pub fn none(resolver: &'a mut dyn IncludeResolver) -> Self {
        Includes {
            arch: None,
            resolver,
        }
    }

    /// The same, for a target.
    pub fn for_arch(arch: &'a str, resolver: &'a mut dyn IncludeResolver) -> Self {
        Includes {
            arch: Some(arch),
            resolver,
        }
    }
}

#[derive(Clone)]
/// One `#define`.
///
/// `Clone` because expansion copies a macro out of the table: `reject` needs a mutable
/// borrow of the preprocessor, and a borrow of the table would still be live across it.
struct Macro {
    /// The parameters, for a function-like macro; `None` for an object-like one.
    parameters: Option<Vec<String>>,
    /// The replacement list, already lexed.
    body: Vec<Token>,
    /// How the body was spelled, for the redefinition diagnostic.
    ///
    /// **Compared as text rather than as tokens, because that is the comparison a reader
    /// of the header is making.** `#define X 1` and `#define X (1)` are the same macro to
    /// any compiler, and a diagnostic that claimed otherwise would send someone looking for
    /// a difference that is not there.
    spelling: String,
}

/// A conditional group: an `#ifdef` and everything it opened.
struct Conditional {
    /// Whether the branch being read is one the program sees.
    active: bool,
    /// Whether any branch so far ran, so `#else` knows to skip.
    taken: bool,
    /// Whether `#else` has been seen, because two of them are a mistake.
    seen_else: bool,
    /// Where the group opened, for the unclosed diagnostic.
    span: SourceSpan,
}

/// The result of preprocessing.
pub struct Preprocessed {
    /// Every token of the program, in order, with the directives gone.
    pub tokens: Vec<Token>,
    /// Every refusal. Empty when nothing was wrong.
    pub diagnostics: Vec<Diagnostic>,
}

/// A preprocessor over one root file.
struct Preprocessor<'a> {
    sources: &'a mut SourceManager,
    macros: BTreeMap<String, Macro>,
    conditionals: Vec<Conditional>,
    /// The names of the files being included, outermost first.
    ///
    /// **Names, not `SourceId`s, and that is the fix rather than a simplification.** Every
    /// read of a header adds a *new* file to the source map, so a cycle re-reads one header
    /// under a fresh id and a check on ids never fires: `a.h` includes `b.h` includes
    /// `a.h`, and the third read is `a.h` again with a different id, and a stack of ids
    /// sees three files that have never met. A stack of names catches the cycle, and it is
    /// also the message that finds the bug: "a.h includes itself" is true and useless,
    /// and "a.h → b.h → a.h" is a bug report.
    stack: Vec<String>,
    diagnostics: Vec<Diagnostic>,
}

/// Preprocesses the file `root`.
///
/// The tokens come back even when there are diagnostics, for the same reason
/// [`lexer::lex`] returns its tokens: a front end that reports one mistake per run makes a
/// person fix them one at a time.
pub fn preprocess(
    sources: &mut SourceManager,
    root: SourceId,
    name: &str,
    includes: &mut Includes<'_>,
) -> Preprocessed {
    let mut preprocessor = Preprocessor {
        sources,
        macros: BTreeMap::new(),
        conditionals: Vec::new(),
        stack: Vec::new(),
        diagnostics: Vec::new(),
    };
    preprocessor.define_builtins(includes.arch);
    let mut tokens = preprocessor.file(root, name, includes.resolver);
    tokens.shrink_to_fit();
    Preprocessed {
        tokens,
        diagnostics: preprocessor.diagnostics,
    }
}

impl Preprocessor<'_> {
    /// Preprocesses one file, and returns its tokens.
    ///
    /// `name` is the header's name as written, which is what the cycle check compares: see
    /// the note on [`Preprocessor::stack`].
    fn file(&mut self, id: SourceId, name: &str, resolver: &mut dyn IncludeResolver) -> Vec<Token> {
        if let Some(depth) = self.stack.iter().position(|open| open == name) {
            let mut chain: Vec<String> = self.stack[depth..].to_vec();
            chain.push(name.to_string());
            self.reject(
                self.whole_file_span(id),
                codes::INCLUDE_CYCLE,
                &format!("`{name}` includes itself"),
                &[
                    "an include guard is missing, or a header includes itself",
                    &format!("the chain is {}", chain.join(" → ")),
                ],
            );
            return Vec::new();
        }
        if self.stack.len() >= MAX_INCLUDE_DEPTH {
            self.reject(
                self.whole_file_span(id),
                codes::INCLUDE_TOO_DEEP,
                &format!("includes are nested more than {MAX_INCLUDE_DEPTH} deep"),
                &["this is nearly always a cycle a guard did not catch"],
            );
            return Vec::new();
        }
        let depth = self.stack.len();
        self.stack.push(name.to_string());
        let lexed = lexer::lex(id, self.sources);
        self.diagnostics.extend(lexed.diagnostics);
        let tokens = self.lines(lexed.tokens, resolver);
        self.stack.truncate(depth);
        // A group left open when a file ends is that file's mistake, and it is reported
        // against the file it was opened in — which needs no bookkeeping, because a span
        // carries the file it came from.
        while self
            .conditionals
            .last()
            .is_some_and(|open| open.span.id() == id)
        {
            if let Some(open) = self.conditionals.pop() {
                self.reject(
                    open.span,
                    codes::UNCLOSED_CONDITIONAL,
                    "a conditional group is never closed",
                    &["every `#ifdef` and `#ifndef` needs an `#endif`"],
                );
            }
        }
        tokens
    }

    /// Walks a file's tokens one logical line at a time.
    ///
    /// **Lines, because a directive is a line and nothing else is.** The lexer has already
    /// handled everything that spans lines — block comments, backslash-spliced strings, a
    /// directive with continued lines — so the boundaries here are simply where the
    /// reported line number changes, which the source map already knows.
    fn lines(&mut self, tokens: Vec<Token>, resolver: &mut dyn IncludeResolver) -> Vec<Token> {
        let mut out = Vec::new();
        let mut line: Vec<Token> = Vec::new();
        let mut line_number = None;
        for token in tokens {
            let number = self.line_of(&token.span);
            if Some(number) != line_number && !line.is_empty() {
                self.text_line(&line, resolver, &mut out);
                line = Vec::new();
            }
            line_number = Some(number);
            line.push(token);
        }
        if !line.is_empty() {
            self.text_line(&line, resolver, &mut out);
        }
        out
    }

    /// Handles one line: a directive, or code.
    ///
    /// **A line beginning with a `HeaderName` is a directive too, and that is the only
    /// signal there is for one.** The lexer treats `#include` specially: its argument is
    /// not C tokens, so it is lexed as a header name and *the word `include` is not
    /// emitted at all*. A preprocessor that looked only for a `Directive` token would find
    /// no directive on any `#include` line in any program, and would hand the header name
    /// to the parser as if it were an identifier — which is a name that is not declared.
    fn text_line(
        &mut self,
        line: &[Token],
        resolver: &mut dyn IncludeResolver,
        out: &mut Vec<Token>,
    ) {
        match line.first() {
            Some(first) if matches!(first.kind, TokenKind::Directive | TokenKind::HeaderName) => {
                self.directive(line, resolver, out);
            }
            _ => {
                if self.emitting() {
                    out.append(&mut self.expand(line.to_vec()));
                }
            }
        }
    }

    // -- the directives --

    fn directive(
        &mut self,
        line: &[Token],
        resolver: &mut dyn IncludeResolver,
        out: &mut Vec<Token>,
    ) {
        // A `HeaderName` first token means `#include`, because that is the only directive
        // whose argument is lexed as a header name — and the lexer's comment says the
        // directive's own name is *not* emitted in that case.
        let name = if line[0].kind == TokenKind::HeaderName {
            String::from("include")
        } else {
            line[0].value.clone()
        };
        let directive = &line[0];
        match name.as_str() {
            "include" => self.include(line, resolver, out),
            "define" => {
                let argument = self.argument(directive);
                self.define(directive, &argument);
            }
            "undef" => self.undef(directive),
            "ifdef" | "ifndef" => self.conditional(directive, &name),
            "else" => self.else_(directive),
            "endif" => self.endif(directive),
            "error" => self.error(directive),
            "if" => self.unimplemented(directive, "if", "ifdef"),
            "elif" => self.unimplemented(directive, "elif", "if"),
            // The only directive that is skipped rather than refused. See the module
            // documentation: `#pragma` is addressed to another tool by design.
            "pragma" => {}
            other => {
                self.reject(
                    directive.span.clone(),
                    codes::UNSUPPORTED_DIRECTIVE,
                    &format!("`#{other}` is not a directive this preprocessor knows"),
                    &[
                        "this preprocessor does `include`, `define`, `undef`, `ifdef`,",
                        "`ifndef`, `else`, `endif` and `error`",
                        "an unknown directive is usually a misspelling, and a misspelling",
                        "that is ignored is a mistake that is not",
                    ],
                );
            }
        }
    }

    /// A directive's argument, re-lexed.
    ///
    /// **The lexer hands a directive over as one token holding the whole line**, because a
    /// directive's argument is not C tokens — `#include <a.h>` and `#define A(x) x` are
    /// both lines rather than expressions. So the name is `token.value` and the rest is
    /// `token.text` after it, and the argument has to be lexed again to be usable.
    ///
    /// Every token it produces points at the directive's span, which is deliberate: the
    /// argument is *of* that line, and a malformed parameter list should be reported on the
    /// `#define` rather than at an offset in a string that exists only for this function.
    fn argument(&self, directive: &Token) -> Vec<Token> {
        let text = directive
            .text
            .get(directive.value.len()..)
            .unwrap_or_default();
        without_end_of_file(lexer::lex_str("<directive-argument>", text).tokens)
            .into_iter()
            .map(|mut token| {
                token.span = directive.span.clone();
                token
            })
            .collect()
    }

    /// `#include`, the reason this stage exists.
    fn include(
        &mut self,
        line: &[Token],
        resolver: &mut dyn IncludeResolver,
        out: &mut Vec<Token>,
    ) {
        if !self.emitting() {
            return;
        }
        let directive = &line[0];
        let Some(header) = line
            .iter()
            .find(|token| token.kind == TokenKind::HeaderName)
        else {
            self.reject(
                directive.span.clone(),
                codes::MALFORMED_INCLUDE,
                "this `#include` has no header name",
                &["it is written `#include \"name.h\"` or `#include <name.h>`"],
            );
            return;
        };
        let Some((name, angled)) = header_name(&header.value) else {
            self.reject(
                header.span.clone(),
                codes::MALFORMED_INCLUDE,
                &format!("`{}` is not a closed header name", header.value),
                &["a header name is written `<name>` or `\"name\"`"],
            );
            return;
        };
        let Some(text) = resolver.resolve(name, angled) else {
            self.reject(
                header.span.clone(),
                codes::HEADER_NOT_FOUND,
                &format!("no header named `{name}`"),
                &["the include path does not have it"],
            );
            return;
        };
        // The header goes into the *same* source map as the program, and that is what lets
        // a diagnostic inside it point at it: a `SourceSpan` carries the file it came from,
        // so a type error in a header renders against the header's own text.
        //
        // A header read twice gets two entries in the map on purpose. C's include guards
        // make the second read produce nothing, and a map that reused the first entry
        // would make a cycle undetectable — the cycle check compares `SourceId`s, and two
        // reads of one header would share one.
        match self.sources.add_file(name, text) {
            Ok(id) => {
                // The header.s own end-of-file is stripped: the program continues after
                // the include, and a terminator in the middle of a token stream is a
                // place the parser stops for no reason.
                let mut tokens = self.file(id, name, resolver);
                tokens = without_end_of_file(tokens);
                out.append(&mut tokens);
            }
            Err(_) => {
                self.reject(
                    header.span.clone(),
                    codes::HEADER_NOT_FOUND,
                    &format!("`{name}` could not be read"),
                    &["a header that cannot be added to the source map cannot be compiled"],
                );
            }
        }
    }

    /// `#define`.
    ///
    /// `argument` is `NAME body…`, with a `(` straight after the name for a function-like
    /// macro and its parameters inside those parentheses.
    fn define(&mut self, directive: &Token, argument: &[Token]) {
        if !self.emitting() {
            return;
        }
        let Some(name_token) = argument.first() else {
            self.unnamed(directive);
            return;
        };
        let name = name_token.value.clone();
        if name.is_empty() {
            self.unnamed(directive);
            return;
        }
        let mut cursor = 1;
        let mut parameters = None;
        if argument.get(1).is_some_and(is_left_paren) {
            let mut names = Vec::new();
            cursor = 2;
            loop {
                // A parameter, then a separator. **The separator is required**, and that is
                // the whole point: `#define M(a b)` is two names with nothing between them,
                // which C refuses, and a preprocessor that accepted it would give a macro a
                // parameter list nobody wrote. A comma with nothing after it is refused for
                // the same reason.
                match argument.get(cursor) {
                    Some(token) if is_right_paren(token) => {
                        cursor += 1;
                        break;
                    }
                    Some(token) if token.kind == TokenKind::Identifier => {
                        names.push(token.value.clone());
                        cursor += 1;
                    }
                    _ => {
                        self.reject(
                            directive.span.clone(),
                            codes::MALFORMED_PARAMETERS,
                            "this function-like macro's parameter list is malformed",
                            &["it is written `#define NAME(a, b) body`"],
                        );
                        return;
                    }
                }
                if argument.get(cursor).is_some_and(is_comma) {
                    cursor += 1;
                    continue;
                }
                if argument.get(cursor).is_some_and(is_right_paren) {
                    cursor += 1;
                    break;
                }
                self.reject(
                    directive.span.clone(),
                    codes::MALFORMED_PARAMETERS,
                    "this function-like macro's parameters are not separated",
                    &[
                        "it is written `#define NAME(a, b) body`",
                        "two names with nothing between them is a missing comma",
                    ],
                );
                return;
            }
            parameters = Some(names);
        }
        let spelling = spelling_of(&argument[cursor..]);
        let body = self.lex_fragment(&spelling, directive.span.clone());
        // C allows a macro to be defined twice if the two definitions agree, and the
        // include-guard-plus-`#undef` dance depends on it. So only a *different* body is an
        // error.
        if let Some(existing) = self.macros.get(&name) {
            let same_parameters = match (&existing.parameters, &parameters) {
                (None, None) => true,
                (Some(one), Some(two)) => one == two,
                _ => false,
            };
            if !same_parameters || existing.spelling != spelling {
                self.reject(
                    directive.span.clone(),
                    codes::REDEFINED_MACRO,
                    &format!("`{name}` is already defined, differently"),
                    &[
                        "redefining a macro with a different body makes the meaning of every",
                        "use depend on which header was read first",
                    ],
                );
            }
            return;
        }
        self.macros.insert(
            name,
            Macro {
                parameters,
                body,
                spelling,
            },
        );
    }

    /// `#undef`.
    fn undef(&mut self, directive: &Token) {
        if !self.emitting() {
            return;
        }
        let argument = self.argument(directive);
        let Some(name) = argument.first() else {
            self.unnamed(directive);
            return;
        };
        self.macros.remove(&name.value);
    }

    /// `#ifdef` and `#ifndef`.
    fn conditional(&mut self, directive: &Token, name: &str) {
        let argument = self.argument(directive);
        let Some(subject) = argument.first() else {
            self.reject(
                directive.span.clone(),
                codes::UNSUPPORTED_DIRECTIVE,
                &format!("this `#{name}` has no name"),
                &[
                    &format!("it is written `#{name} NAME`"),
                    "`#ifdef` asks whether a name is defined, and that is all a header",
                    "guard or a feature switch needs to know",
                ],
            );
            return;
        };
        let defined = self.macros.contains_key(&subject.value) || is_built_in(&subject.value);
        let wants = if name == "ifdef" { defined } else { !defined };
        // The group's own `active` depends on the enclosing ones: a group inside a skipped
        // group is skipped whatever its own condition says, which is what nesting means.
        let active = self.emitting() && wants;
        self.conditionals.push(Conditional {
            active,
            taken: active,
            seen_else: false,
            span: directive.span.clone(),
        });
    }

    /// `#else`.
    ///
    /// **The enclosing groups are counted *excluding* this one, and that exclusion is the
    /// whole function.** A group that was not taken because its `#ifdef` was false still
    /// has to be able to take its `#else`; asking `emitting()` here would ask about this
    /// group too, get `false` from it, and conclude that nothing outside it is running
    /// either. The symptom is a header whose `#else` branch is never read — so
    /// `#ifdef __LZ64__ / #else / typedef unsigned int / #endif` declares nothing at all
    /// for a 32-bit target, and the program fails with "expected a declaration" pointing at
    /// a `word` that the header said it was defining.
    fn else_(&mut self, directive: &Token) {
        let enclosing = self
            .conditionals
            .iter()
            .rev()
            .skip(1)
            .all(|group| group.active);
        let Some(group) = self.conditionals.last_mut() else {
            self.reject(
                directive.span.clone(),
                codes::MISPLACED_CONDITIONAL,
                "this `#else` has nothing to match",
                &["`#else` follows an `#ifdef` or an `#ifndef`"],
            );
            return;
        };
        if group.seen_else {
            self.reject(
                directive.span.clone(),
                codes::MISPLACED_CONDITIONAL,
                "this conditional has two `#else`s",
                &["a group has at most one `#else`"],
            );
            return;
        }
        group.seen_else = true;
        group.active = enclosing && !group.taken;
        group.taken = true;
    }

    /// `#endif`.
    fn endif(&mut self, directive: &Token) {
        if self.conditionals.pop().is_none() {
            self.reject(
                directive.span.clone(),
                codes::MISPLACED_CONDITIONAL,
                "this `#endif` has nothing to close",
                &["`#endif` closes an `#ifdef` or an `#ifndef`"],
            );
        }
    }

    /// `#error`, which is a refusal carrying the programmer's own words.
    fn error(&mut self, directive: &Token) {
        if !self.emitting() {
            return;
        }
        let message = directive
            .text
            .get(directive.value.len()..)
            .unwrap_or_default()
            .trim()
            .to_string();
        if message.is_empty() {
            self.reject(
                directive.span.clone(),
                codes::ERROR_DIRECTIVE,
                "this `#error` says nothing",
                &["`#error` is for a configuration that cannot work, and it says why"],
            );
            return;
        }
        self.reject(
            directive.span.clone(),
            codes::ERROR_DIRECTIVE,
            &message,
            &["`#error` in a build that is being compiled is a deliberate failure"],
        );
    }

    fn unimplemented(&mut self, directive: &Token, found: &str, alternative: &str) {
        if !self.emitting() {
            return;
        }
        self.reject(
            directive.span.clone(),
            codes::UNSUPPORTED_DIRECTIVE,
            &format!("`#{found}` is not implemented by this preprocessor"),
            &[
                &format!("`#{alternative}` is, and a definedness test is all that is needed"),
                "a silently ignored `#if` would make `#if 0` and `#if 1` the same program",
            ],
        );
    }

    fn unnamed(&mut self, directive: &Token) {
        self.reject(
            directive.span.clone(),
            codes::NAMELESS_DEFINE,
            &format!("this `#{}` has no name", directive.value),
            &["it is written `#define NAME replacement` or `#undef NAME`"],
        );
    }

    // -- macro expansion --

    /// Expands the macros in one line's tokens.
    fn expand(&mut self, line: Vec<Token>) -> Vec<Token> {
        // Nothing is defined, so nothing can be substituted. **This check is the
        // difference between a preprocessor that costs nothing and one that walks every
        // token of every program**, and for a program with no `#define` anywhere it is the
        // whole cost.
        if self.macros.is_empty() && !line.iter().any(|token| is_built_in(&token.value)) {
            return line;
        }
        let mut out = Vec::new();
        let mut pending: VecDeque<Entry> = line.into_iter().map(Entry::fresh).collect();
        let mut budget = BUDGET;
        while let Some(entry) = pending.pop_front() {
            if budget == 0 {
                self.reject(
                    entry.token.span.clone(),
                    codes::EXPANSION_BUDGET,
                    "macro expansion did not stop",
                    &["a macro is almost certainly defined in terms of itself"],
                );
                return out;
            }
            budget -= 1;
            let token = entry.token;
            if entry.painted || token.kind != TokenKind::Identifier {
                out.push(token);
                continue;
            }
            let name = token.value.clone();
            if is_built_in(&name) {
                match built_in(&name, &token, self.sources) {
                    Some(replacement) => pending.push_front(Entry::fresh(replacement)),
                    None => out.push(token),
                }
                continue;
            }
            // **Cloned, because `reject` needs `&mut self` and a borrow of the table
            // would still be live across the call.** A macro is a name, a handful of
            // parameters and a token list; copying one to keep the borrow checker happy
            // costs far less than restructuring the table into something that hands out
            // interior references.
            let Some(definition) = self.macros.get(&name).cloned() else {
                out.push(token);
                continue;
            };
            let Some(expanded) = self.substitute(&definition, &name, &token, &mut pending) else {
                // Not a call at all: a function-like macro name with no `(` after it is an
                // identifier, which is C's rule and the one that lets a program have both a
                // macro and a variable of the same name.
                out.push(token);
                continue;
            };
            // **Blue paint.** A token that came out of macro `M` and still reads `M` is
            // not expanded again. That is what makes `#define f(x) f(x)` — the idiom every
            // header uses for a function-like macro that must not expand its own name —
            // terminate, and it is applied to substituted tokens only, so a *different*
            // macro with the same body still expands.
            for produced in expanded.into_iter().rev() {
                let painted = produced.kind == TokenKind::Identifier && produced.value == name;
                pending.push_front(Entry {
                    token: produced,
                    painted,
                });
            }
        }
        out
    }

    /// Produces a macro's replacement, or `None` when this is not a call.
    fn substitute(
        &mut self,
        definition: &Macro,
        name: &str,
        at: &Token,
        pending: &mut VecDeque<Entry>,
    ) -> Option<Vec<Token>> {
        let Some(parameters) = definition.parameters.clone() else {
            return Some(self.respan(&definition.body, None, &[], at.span.clone()));
        };
        let arguments = self.take_arguments(pending)?;
        if arguments.len() != parameters.len() {
            let wanted = parameters.len();
            let given = arguments.len();
            self.reject(
                at.span.clone(),
                codes::ARGUMENT_COUNT,
                &format!(
                    "`{name}` takes {wanted} argument{} but is given {given}",
                    if wanted == 1 { "" } else { "s" }
                ),
                &[
                    "a function-like macro is called with the arguments its parameter list \
                   names",
                ],
            );
            // The call is dropped rather than half-substituted, because a macro invoked
            // with the wrong number of arguments has no meaning: there is no reading of the
            // body that is right.
            return Some(Vec::new());
        }
        Some(self.respan(
            &definition.body,
            Some(&parameters),
            &arguments,
            at.span.clone(),
        ))
    }

    /// Replaces parameters in a body and points every resulting token at the call.
    ///
    /// **A parameter is substituted by name, and only where the body has an identifier.**
    /// That second half is what keeps `#define MSG(x) "x is not a macro argument"` honest:
    /// a string is one token whose text is data, and no name inside it is a name.
    fn respan(
        &mut self,
        body: &[Token],
        parameters: Option<&[String]>,
        arguments: &[Vec<Token>],
        at: SourceSpan,
    ) -> Vec<Token> {
        let mut out = Vec::new();
        for token in body {
            let substituted = parameters.and_then(|parameters| {
                (token.kind == TokenKind::Identifier)
                    .then(|| parameters.iter().position(|name| name == &token.value))
                    .flatten()
            });
            match substituted {
                Some(index) => out.extend(arguments[index].iter().cloned()),
                None => out.push(token.clone()),
            }
        }
        for token in &mut out {
            token.span = at.clone();
        }
        out
    }

    /// Takes a call's arguments off the front of the queue.
    ///
    /// **Returns `None` when the next token is not `(`,** which is the C rule that keeps
    /// `sizeof` — and a variable that happens to share a macro's name — from being read as
    /// a call. The tokens are only removed once the whole argument list has been seen, so
    /// an unbalanced `(` leaves the line as it was and the parser reports the parenthesis
    /// that is actually wrong.
    fn take_arguments(&mut self, pending: &mut VecDeque<Entry>) -> Option<Vec<Vec<Token>>> {
        if !pending
            .front()
            .is_some_and(|entry| is_left_paren(&entry.token))
        {
            return None;
        }
        // Find the closing parenthesis without consuming anything: a macro call whose
        // arguments run off the end of the line is not a call, and the tokens are needed
        // intact for whoever reports it.
        let mut depth = 0usize;
        let mut end = None;
        for (index, entry) in pending.iter().enumerate() {
            if is_left_paren(&entry.token) {
                depth += 1;
            } else if is_right_paren(&entry.token) {
                depth -= 1;
                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
        }
        let end = end?;
        // `make_contiguous` first, because a `VecDeque` has no range `Index` impl: it has
        // `Index<usize>`, so a range is resolved there and reported as a bad index rather
        // than falling through to a slice. Contiguating is free when the queue already is
        // contiguous, which after one `pop_front` it usually is not — and this is a
        // preprocessor, not a hot loop.
        let arguments = split_arguments(&pending.make_contiguous()[1..end]);
        for _ in 0..=end {
            pending.pop_front();
        }
        Some(arguments)
    }

    // -- the conditional state --

    /// Whether the line being read is one the program sees.
    ///
    /// **Every group must be running, not just the innermost,** which is what makes a
    /// skipped group skip the groups inside it.
    fn emitting(&self) -> bool {
        self.conditionals.iter().all(|group| group.active)
    }

    // -- the built-in macros --

    /// The macros every compilation has, before any header is read.
    ///
    /// **Four, and the last is the only interesting one.** `__LAZALITH__` says what compiled
    /// the program, `__STDC__` is the C standard's own claim, and `__LZ64__` / `__LZ32__`
    /// say what it targets — the last of which is how a header chooses a word width with
    /// `#ifdef` instead of with the `#if` this preprocessor does not have.
    fn define_builtins(&mut self, arch: Option<&str>) {
        // The span of a built-in macro's own tokens. **Source 0 is the program's own
        // file**, so a token that somehow reached a diagnostic without being substituted
        // points at the top of the program rather than nowhere — and the file is
        // guaranteed to exist, because the preprocessor is only ever run on a file.
        let here = self.whole_file_span(SourceId::new(0));
        let define = |preprocessor: &mut Self, name: &str, value: &str| {
            let body = preprocessor.lex_fragment(value, here.clone());
            preprocessor.macros.insert(
                name.to_string(),
                Macro {
                    parameters: None,
                    body,
                    spelling: value.to_string(),
                },
            );
        };
        define(self, "__LAZALITH__", "1");
        define(self, "__STDC__", "1");
        match arch {
            Some("lz64") => define(self, "__LZ64__", "1"),
            Some("lz32") => define(self, "__LZ32__", "1"),
            // An architecture this compiler does not know is *not* an error here. The
            // machine is what refuses an unknown architecture, and refusing it in two places
            // would mean two places to keep in step. Defining neither width means a header
            // that needs one fails to find it, which is the right failure.
            _ => {}
        }
    }

    /// Lexes a fragment of text and re-spans its tokens.
    ///
    /// **Every token comes back pointing at `at`,** because a fragment is not a place a
    /// diagnostic can usefully point, and expansion overwrites the span again.
    fn lex_fragment(&mut self, text: &str, at: SourceSpan) -> Vec<Token> {
        without_end_of_file(lexer::lex_str("<macro>", text).tokens)
            .into_iter()
            .map(|mut token| {
                token.span = at.clone();
                token
            })
            .collect()
    }

    // -- small helpers --

    /// Records one refusal, against the file the span came from.
    ///
    /// **The first help line is the help and the rest are notes,** because a diagnostic has
    /// one help and any number of notes, and a reader follows the help first. Two help
    /// lines flattened into one would read as one long sentence with no break in it.
    fn reject(&mut self, span: SourceSpan, code: &str, message: &str, help: &[&str]) {
        let built = crate::diagnostic::at(
            span.id(),
            self.sources,
            span,
            code,
            message,
            &[],
            help.first().copied(),
        );
        let mut diagnostic = built.diagnostic().clone();
        for line in help.iter().skip(1) {
            diagnostic = diagnostic.with_note(lazalith_diagnostics::Note::new(*line));
        }
        self.diagnostics.push(diagnostic);
    }

    fn line_of(&self, span: &SourceSpan) -> u32 {
        self.sources
            .line_column(span.id(), span.start())
            .map_or(0, |position| position.line)
    }

    /// A span covering a whole file, for a diagnostic about the file rather than a line.
    ///
    /// **Clamped to the file's length, because a span past the end of a file is not
    /// renderable** and the zero-length fallback is a real position in the file a reader
    /// can be sent to, whereas an invalid span is a rendering that cannot happen.
    fn whole_file_span(&self, id: SourceId) -> SourceSpan {
        let length = self
            .sources
            .file(id)
            .map_or(0, |file| file.text().len() as u32);
        let clamped = ByteOffset::new(length);
        self.sources
            .source_span(id, ByteOffset::new(0), clamped)
            .or_else(|_| {
                self.sources
                    .source_span(id, ByteOffset::new(0), ByteOffset::new(0))
            })
            .expect("a zero-length span at offset zero is always valid")
    }
}

/// Drops the tokens the lexer ends every file and every fragment with.
///
/// **The lexer appends one `EndOfFile` to whatever it reads, and three of the four things
/// this stage lexes must not carry one.** A macro body is spliced into the middle of a
/// program, so its end-of-file would arrive in the middle of an expression and the parser
/// would stop there: `#define LIMIT 7` followed by `return LIMIT;` produced a stream that
/// ended at the substituted `7`, and the diagnostic pointed at the `;` as if it were the
/// end of the program. An included header's end-of-file has the same problem a third of a
/// way into a file. Only the *root* file's terminator is wanted, because only the root's
/// is where the program ends.
fn without_end_of_file(mut tokens: Vec<Token>) -> Vec<Token> {
    while tokens
        .last()
        .is_some_and(|token| token.kind == TokenKind::EndOfFile)
    {
        tokens.pop();
    }
    tokens
}

/// A token in the expansion queue, and whether it is painted.
struct Entry {
    token: Token,
    painted: bool,
}

impl Entry {
    fn fresh(token: Token) -> Self {
        Entry {
            token,
            painted: false,
        }
    }
}

/// Splits a macro call's argument tokens on top-level commas.
///
/// **A depth counter, because an argument is often a call or a brace initialiser.** A split
/// that ignored nesting would turn `M(f(1, 2), 3)` into three arguments and then report
/// the wrong count — a diagnostic about the preprocessor, caused by the preprocessor.
fn split_arguments(tokens: &[Entry]) -> Vec<Vec<Token>> {
    let mut arguments: Vec<Vec<Token>> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    let mut depth = 0usize;
    for entry in tokens {
        let token = &entry.token;
        if is_open(token) {
            depth += 1;
        } else if is_close(token) {
            depth = depth.saturating_sub(1);
        } else if is_comma(token) && depth == 0 {
            arguments.push(core::mem::take(&mut current));
            continue;
        }
        current.push(token.clone());
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    arguments
}

/// Whether `token` is a punctuator spelled `spelling`.
fn is_punctuator(token: &Token, spelling: &str) -> bool {
    token.kind == TokenKind::Punctuator && token.text == spelling
}

fn is_left_paren(token: &Token) -> bool {
    is_punctuator(token, "(")
}

fn is_right_paren(token: &Token) -> bool {
    is_punctuator(token, ")")
}

fn is_comma(token: &Token) -> bool {
    is_punctuator(token, ",")
}

/// Any bracket that opens, so that a comma inside one is not a separator.
fn is_open(token: &Token) -> bool {
    is_punctuator(token, "(") || is_punctuator(token, "{") || is_punctuator(token, "[")
}

/// Any bracket that closes.
fn is_close(token: &Token) -> bool {
    is_punctuator(token, ")") || is_punctuator(token, "}") || is_punctuator(token, "]")
}

/// Splits a header name into its spelling and whether it was angled.
fn header_name(spelled: &str) -> Option<(&str, bool)> {
    if let Some(inner) = spelled
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Some((inner, true));
    }
    spelled
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .map(|inner| (inner, false))
}

/// A directive's body as written, with a space between tokens.
///
/// **The spelling, not the tokens, because that is what the redefinition check compares
/// and what a message about a redefinition should quote.** Two macros that differ only in
/// spacing are the same macro, and one that differs in a token is not.
fn spelling_of(tokens: &[Token]) -> String {
    let mut out = String::new();
    for token in tokens {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

/// Whether `name` is one of the two names a program may use without a header defining it.
///
/// **A match, not a lookup, so that adding a built-in is one change in one place.**
fn is_built_in(name: &str) -> bool {
    matches!(name, "__FILE__" | "__LINE__")
}

/// The token a built-in macro stands for.
fn built_in(name: &str, at: &Token, sources: &SourceManager) -> Option<Token> {
    let number = |digits: String, span: SourceSpan| Token {
        kind: TokenKind::Integer,
        text: digits.clone(),
        value: digits.clone(),
        number: Number {
            digits,
            base: 10,
            suffix: String::new(),
            floating: false,
        },
        character: 0,
        span,
    };
    match name {
        "__LINE__" => {
            let line = sources
                .line_column(at.span.id(), at.span.start())
                .map_or(0, |position| position.line);
            Some(number(line.to_string(), at.span.clone()))
        }
        "__FILE__" => {
            let file = sources
                .file(at.span.id())
                .map(|file| file.name().to_string())
                .unwrap_or_else(|| "<unknown>".to_string());
            Some(Token {
                kind: TokenKind::String,
                text: format!("\"{file}\""),
                value: file,
                number: Number {
                    digits: String::new(),
                    base: 10,
                    suffix: String::new(),
                    floating: false,
                },
                character: 0,
                span: at.span.clone(),
            })
        }
        _ => None,
    }
}
