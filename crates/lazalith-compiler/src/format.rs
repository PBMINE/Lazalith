//! The canonical formatting style, and the one function that produces it.
//!
//! # What a formatter is allowed to do
//!
//! Rewrite whitespace and nothing else. That is not a limitation being apologised
//! for — it is the *property* that makes a formatter safe to run on someone else's
//! file, and it is the property the tests assert: the formatted text lexes to the
//! same token stream as the input, with the same comments, in the same order. A
//! formatter that changed a token would be a compiler, and this repository has one.
//!
//! Working from the token stream rather than from the source text is what makes that
//! cheap to guarantee. There is no "did I mean to rewrite this literal" question,
//! because literals are tokens and tokens are copied through. A formatter built on
//! regular expressions over source text has to answer that question for every
//! construct in the language, forever; one built on the lexer answers it once, by
//! construction.
//!
//! # The one thing tokens cannot say
//!
//! `-` is subtraction in `a - b` and negation in `return -x;`, and the two are the
//! same token. A formatter built on tokens alone has three bad options: guess (and
//! produce `a - x` for a negation or `a-b` for a subtraction), refuse to format
//! anything with a sign in it, or parse the file — which would mean the formatter
//! only works on a file that already compiles, at exactly the moment a person most
//! wants to format one.
//!
//! So the formatter reads the *source* for this one decision, and preserves what the
//! author wrote:
//!
//! ```text
//! no space before, space after   -> binary     a - b
//! space before, no space after   -> unary      return -x;
//! ```
//!
//! when the author was clear, and falls back to "binary if the previous token can end
//! an expression" when they were not. Every other decision is made from the token
//! alone, and this one is a *preservation*, not a choice — which is also why the
//! style is idempotent: the output's own text disambiguates the next run the same
//! way.
//!
//! # The style
//!
//! `docs/lazen-formatting.md` has the rules with examples. The short form:
//!
//! - **Four spaces per indent level, never tabs.** A tab is a request for a width the
//!   reader's editor chooses, and the repository already forbids `unsafe`; this is
//!   the same instinct applied to files.
//! - **One space around a binary operator**, none inside brackets: `a + b`, `f(x)`,
//!   `xs[i]`.
//! - **No space before `(`, `,`, `;` or `)`; one space after `,`**.
//! - **A space before `{`, and a block's contents on their own lines.** `{}` stays
//!   `{}`: an empty body means the author meant "nothing here", and a formatter that
//!   turns it into two lines is making a claim the author did not make.
//! - **One blank line between top-level items, at most one inside a block.** More is
//!   collapsed, because two blank lines are a layout artefact and this is a layout
//!   tool.
//! - **Exactly one trailing newline**, and no trailing whitespace on any line.

use alloc::{format, string::String};

use lazalith_types::SourceManager;

use crate::lexer::{self, Comment, Token, TokenKind};

/// How deep one indent level is.
const INDENT: &str = "    ";
/// The most indent levels the formatter will produce.
///
/// Unbounded indentation would let a pathological file allocate a line proportional
/// to its brace depth for every line in it, and a file that nests this deeply has a
/// problem formatting should not make worse.
const MAX_DEPTH: usize = 64;

/// Why a file could not be formatted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    /// The file did not lex, so there is nothing to format.
    NotLexable {
        /// The lexer's first diagnostic.
        message: String,
    },
}

impl core::fmt::Display for FormatError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotLexable { message } => {
                write!(
                    f,
                    "the file did not lex, so it cannot be formatted: {message}"
                )
            }
        }
    }
}

/// Formats one file's text.
///
/// The whole of the command: lex, re-emit, return. Nothing is written and nothing is
/// parsed, so a file that does not compile still formats — which is the point, since
/// the usual moment to run a formatter is on a file that does not yet compile.
pub fn format(text: &str) -> Result<String, FormatError> {
    let mut sources = SourceManager::default();
    let source = sources
        .add_file("format.lz", text)
        .map_err(|error| FormatError::NotLexable {
            message: format!("{error:?}"),
        })?;
    let lexed = lexer::lex(source, &sources);
    if let Some(error) = lexed.diagnostics.first() {
        return Err(FormatError::NotLexable {
            message: format!("{error:?}"),
        });
    }
    Ok(emit(text, &lexed.tokens, &lexed.comments))
}

/// Whether a file is already formatted, for `--check`.
///
/// A separate function rather than a flag on [`format`], because "is it formatted"
/// and "format it" want to answer differently: one is a question for a script and
/// the other is a rewrite of a person's file, and a script must not rewrite anything.
pub fn is_formatted(text: &str) -> Result<bool, FormatError> {
    Ok(format(text)? == text)
}

/// What separates two tokens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Gap {
    /// Nothing at all.
    None,
    /// One space.
    Space,
    /// End the line.
    Line,
}

/// Re-emits the token stream in the canonical style.
fn emit(text: &str, tokens: &[Token], comments: &[Comment]) -> String {
    let mut out = String::new();
    let mut writer = Writer {
        text,
        out: &mut out,
        comments,
        next_comment: 0,
        depth: 0,
        brackets: 0,
        continuation: 0,
        parens: 0,
        flat_blocks: 0,
        pending_brace: false,
        at_line_start: true,
        previous: None,
        tight_next: false,
        previous_end: 0,
        source_base: 0,
        pending_start: 0,
    };
    for token in tokens {
        if matches!(token.kind, TokenKind::Eof) {
            break;
        }
        writer.token(token);
    }
    // A comment after the last token still belongs in the file: a note at the end is
    // a note to whoever reads it next, and dropping it because it came after the
    // last semicolon would be the formatter eating a person's writing.
    writer.flush_comments_before(u32::MAX);
    writer.finish();
    out
}

/// The emission state.
struct Writer<'a> {
    /// The original text, read for the one decision tokens cannot make.
    text: &'a str,
    out: &'a mut String,
    comments: &'a [Comment],
    next_comment: usize,
    depth: usize,
    /// How many open brackets there are, which is what tells a statement`s `;`
    /// from a separator inside an array length or a record literal.
    brackets: usize,
    /// How many `(` and `[` are open, which is how much deeper a broken line inside
    /// a call is written than the line that opened it.
    continuation: usize,
    /// How many round and square brackets are open, which is what tells a
    /// statement semicolon from an array length or a call argument list.
    parens: usize,
    /// How many open braces the author wrote flat, so a semicolon inside one of them
    /// is a separator rather than the end of a statement.
    flat_blocks: usize,
    /// Whether a brace was just opened and its first statement has not been read yet.
    pending_brace: bool,
    at_line_start: bool,
    previous: Option<TokenKind>,
    /// Whether the token just written was a prefix sign, so the next one is tight.
    tight_next: bool,
    /// Where the last token ended, so the gap can ask the source what was between.
    previous_end: u32,
    /// The end of the last thing read from the source — a token or a comment — so
    /// the gap measures the text between two *visible* things rather than across one.
    source_base: u32,
    /// Where the token being emitted starts.
    pending_start: u32,
}

impl Writer<'_> {
    /// Emits one token, with whatever whitespace it needs before it.
    fn token(&mut self, token: &Token) {
        self.pending_start = token.span.start().as_u32();
        self.settle_block();
        self.flush_comments_before(self.pending_start);
        // The gap is decided *before* the dedents below, because a `}` that closes a
        // block the author wrote on one line is the token that tells whether that block
        // is flat, and the count it tests is only decremented afterwards.
        //
        // A prefix sign makes the token after it tight as well, and that has to be
        // remembered rather than re-derived: the next token's own gap comes from its own
        // kind, and a literal has no way to know it follows a sign.
        let was_tight = self.tight_next;
        let gap = if was_tight {
            Gap::None
        } else {
            self.gap_before(token)
        };
        // A `}` returns to the opening level *before* the indent for its line is
        // written, because `apply` writes the indent. Dedenting afterwards would close a
        // brace at the depth of the block it just left, which is the bug every
        // hand-written emitter has once.
        if matches!(token.kind, TokenKind::CloseBrace) {
            self.depth = self.depth.saturating_sub(1);
            self.flat_blocks = self.flat_blocks.saturating_sub(1);
        }
        // The same for a round or square bracket, so the line a closer ends is written
        // at the level of the line that opened it rather than one level in.
        if matches!(token.kind, TokenKind::CloseParen | TokenKind::CloseBracket) {
            self.continuation = self.continuation.saturating_sub(1);
        }
        // A blank line the author wrote is *kept*, one of them. A blank line is
        // grouping, and a formatter that deletes every blank line in a file deletes
        // its structure — which is the difference between a formatter and a
        // refactoring. Runs collapse to one, so the output cannot grow.
        if gap == Gap::Line && self.author_left_blank_lines() {
            self.blank_line();
        }
        self.apply(gap);
        self.push(&token.text);
        self.tight_next = !was_tight && self.is_unary(token);
        if matches!(token.kind, TokenKind::OpenBrace) {
            self.depth += 1;
            // A block the author wrote on one line -- `if a { b(); }` -- stays on one
            // line, and a `;` inside one does not end a statement, which is what keeps
            // a record literal.s field separator from splitting a line in half. Whether
            // it *is* one line is only knowable from the token after the brace, so the
            // question is left open here and settled in `settle_block`.
            self.flat_blocks += 1;
            self.pending_brace = true;
            self.source_base = token.span.end().as_u32();
        } else if matches!(token.kind, TokenKind::OpenParen | TokenKind::OpenBracket) {
            self.brackets += 1;
            self.continuation += 1;
            self.parens += 1;
        } else if matches!(token.kind, TokenKind::CloseParen | TokenKind::CloseBracket) {
            self.brackets = self.brackets.saturating_sub(1);
            self.parens = self.parens.saturating_sub(1);
        }
        self.previous = Some(token.kind.clone());
        self.previous_end = token.span.end().as_u32();
        self.source_base = self.previous_end;
        if self.ends_a_statement(token) {
            self.newline();
        }
    }

    /// Settles whether the block just opened is one the author wrote on one line.
    ///
    /// Called with `pending_start` already set and before the comments are flushed,
    /// because a comment between the brace and the first statement settles it the other
    /// way: a block with a comment in it is a block that was broken.
    fn settle_block(&mut self) {
        if !self.pending_brace {
            return;
        }
        self.pending_brace = false;
        if self.author_broke_the_line() {
            self.flat_blocks = self.flat_blocks.saturating_sub(1);
        }
    }

    /// Whether the author left a blank line before this token.
    ///
    /// Measured from the last token *or comment* to this one, so a comment followed by
    /// a blank line does not have its own newlines counted as the gap.s — which is
    /// what would make a commented statement look deliberately spaced when it is not.
    ///
    /// Read from the source rather than remembered, so the formatter's own output
    /// answers the same question the input did — which is what makes preserving a
    /// blank line idempotent rather than a rule that grows a line every pass.
    fn author_left_blank_lines(&self) -> bool {
        let from = usize::try_from(self.source_base).unwrap_or(usize::MAX);
        let to = usize::try_from(self.pending_start).unwrap_or(0);
        let Some(between) = self.text.get(from..to) else {
            return false;
        };
        between.matches('\n').count() >= 2
    }

    /// Whether the author broke this token onto its own line inside brackets.
    ///
    /// A call whose arguments were written one per line stays that way. A token
    /// stream has no width and no notion of where a line was, so *keeping* the
    /// author's choice is the only thing a formatter built on tokens can do here —
    /// and it is also the only thing worth doing, because the alternative is a
    /// formatter that joins a 40-line call into one unreadable 400-character line.
    fn author_broke_the_line(&self) -> bool {
        let from = usize::try_from(self.source_base).unwrap_or(usize::MAX);
        let to = usize::try_from(self.pending_start).unwrap_or(0);
        let Some(between) = self.text.get(from..to) else {
            return false;
        };
        between.contains('\n')
    }

    /// Whether this token ends a statement.
    ///
    /// A `;` ends a statement only at the top level of the file. Inside an
    /// array's length — `[u8; 16]` — or a record literal's field list — `point { x: 1,
    /// y: 2; }` — it separates things, and ending the line there would split a
    /// statement in half. A `}` ends one whenever the thing it closed was a block,
    /// which is every `}` in this language: there are no record *expressions* with a
    /// trailing `}` and nothing after them on the same line.
    fn ends_a_statement(&self, token: &Token) -> bool {
        match &token.kind {
            TokenKind::Semi => self.parens == 0 && self.flat_blocks == 0,
            TokenKind::CloseBrace => true,
            _ => false,
        }
    }

    /// What goes between the previous token and this one.
    ///
    /// One total function, with the rules in a fixed order, because a formatter whose
    /// spacing comes out of a pile of interacting special cases is a formatter nobody
    /// can change. Each rule says what it *overrides*, and the order is the priority:
    ///
    /// 1. **the author broke the line here** — inside brackets, a line they broke
    ///    stays broken. A token stream has no width and no notion of where a line
    ///    was, so keeping their choice is the only thing possible, and it is also the
    ///    only thing worth doing: the alternative is a formatter that joins a 40-line
    ///    call into one unreadable 400-character line. It comes first because it is
    ///    the author saying *no* to every default below.
    /// 2. **nothing hugs a closer or a separator** — `)`, `]`, `,`, `;`, `:`, `.`,
    ///    `..`, `::`.
    /// 3. **nothing follows an opener or a path separator**, and one space follows a
    ///    `,`, a `:` or an `->`.
    /// 4. **a block boundary is a line break** — after `{`, after `;` at the top
    ///    level, after `}`, and before `}`. The bracket count is what tells a
    ///    statement's `;` from an array's length or a record's field list.
    /// 5. **`{}` stays `{}`** — an empty body means the author meant "nothing here",
    ///    and a brace on its own line is a claim about the body they did not make.
    /// 6. **a call and a subscript are tight**, and a parenthesis after a *keyword* is
    ///    punctuation and takes a space. The lexer has keyword tokens of its own, so
    ///    this is a fact about the token rather than a guess about the text.
    /// 7. **`) {` stays `) {`** — the author put the closer and its brace together,
    ///    and separating them splits one construct into two halves.
    /// 8. **one space** — everything else, including a prefix sign, which keeps the
    ///    space in front of it and takes the space after it away.
    fn gap_before(&self, token: &Token) -> Gap {
        let Some(previous) = &self.previous else {
            return Gap::Line;
        };
        // 1
        if self.brackets > 0 && !self.at_line_start && self.author_broke_the_line() {
            return Gap::Line;
        }
        // 2
        if matches!(
            &token.kind,
            TokenKind::CloseParen
                | TokenKind::CloseBracket
                | TokenKind::Comma
                | TokenKind::Semi
                | TokenKind::Colon
                | TokenKind::Dot
                | TokenKind::DotDot
                | TokenKind::PathSep
        ) {
            return Gap::None;
        }
        // 5
        if matches!(token.kind, TokenKind::CloseBrace) && matches!(previous, TokenKind::OpenBrace) {
            return Gap::None;
        }
        // 3: nothing after an opener or a path separator.
        if matches!(
            previous,
            TokenKind::OpenParen | TokenKind::OpenBracket | TokenKind::PathSep | TokenKind::Dot
        ) {
            return Gap::None;
        }
        // 3: one space after these.
        if matches!(
            previous,
            TokenKind::Comma | TokenKind::Colon | TokenKind::Arrow
        ) {
            return Gap::Space;
        }
        // 4
        if matches!(token.kind, TokenKind::CloseBrace)
            && self.flat_blocks == 0
            && !self.closer_stays_beside_its_brace()
        {
            return Gap::Line;
        }
        if (matches!(previous, TokenKind::OpenBrace | TokenKind::CloseBrace)
            || (matches!(previous, TokenKind::Semi) && self.parens == 0))
            && self.flat_blocks == 0
        {
            return Gap::Line;
        }
        // 4: a brace opens a block, and takes a space before it.
        if matches!(token.kind, TokenKind::OpenBrace) {
            return if self.at_line_start {
                Gap::Line
            } else {
                Gap::Space
            };
        }
        // 6
        if matches!(token.kind, TokenKind::OpenParen | TokenKind::OpenBracket)
            && is_value_end(previous)
        {
            return Gap::None;
        }
        // 7
        if matches!(token.kind, TokenKind::CloseBrace) {
            return Gap::Space;
        }
        // 8
        Gap::Space
    }
    ///
    /// `) {` is how a multi-line call ends and its block begins, and putting the
    /// brace on its own line separates the two halves of one construct. The author
    /// wrote them together, so they stay together.
    fn closer_stays_beside_its_brace(&self) -> bool {
        matches!(
            self.previous,
            Some(TokenKind::CloseParen) | Some(TokenKind::CloseBracket)
        ) && !self.author_broke_the_line()
    }

    /// Whether this token is a prefix operator rather than an infix one.
    ///
    /// The author's own spacing decides it when they were clear, and the previous
    /// token decides it when they were not. See the module documentation for why
    /// this is a preservation and not a guess.
    /// this is a preservation and not a guess.
    fn is_unary(&self, token: &Token) -> bool {
        let Some(previous) = &self.previous else {
            return false;
        };
        if !is_prefix_capable(&token.kind) {
            return false;
        }
        let before = token.span.start().as_u32();
        let after = self
            .text
            .as_bytes()
            .get(usize::try_from(token.span.end().as_u32()).unwrap_or(usize::MAX));
        let space_before = self
            .text
            .as_bytes()
            .get(usize::try_from(before.saturating_sub(1)).unwrap_or(usize::MAX))
            .map(|byte| byte.is_ascii_whitespace())
            .unwrap_or(true);
        let space_after = after
            .map(|byte| byte.is_ascii_whitespace())
            .unwrap_or(false);
        match (space_before, space_after) {
            (true, false) => return true,
            (false, true) => return false,
            _ => {}
        }
        // The author was not clear, so the conservative reading wins: a token after
        // something that can end an expression is infix, and a token after a keyword
        // is a prefix.
        !is_value_end(previous)
    }

    /// Writes out every comment that starts before `offset`.
    ///
    /// A comment goes on its own line at the current indent, after any blank line
    /// the author left above it. That is the only placement that is always right: the
    /// token stream says where a comment sits relative to the *next* token, and
    /// nothing about how far the line it was written on was indented — which is the
    /// one thing a formatter exists to fix.
    fn flush_comments_before(&mut self, offset: u32) {
        while let Some(comment) = self.comments.get(self.next_comment) {
            if comment.span.start().as_u32() >= offset {
                return;
            }
            let start = comment.span.start().as_u32();
            let end = comment.span.end().as_u32();
            let text = comment.text.clone();
            self.next_comment += 1;
            // The measurement base moves *first* and unconditionally, so no early
            // return below can leave it pointing at a comment that has already been
            // written — which is how a comment followed by one newline came to be
            // counted as two, and why every commented statement looked spaced.
            // A blank line *before* a comment is grouping too, and it is the one
            // place a comment-led layout loses it: the comment is written before the
            // token that would have asked for the line break. Measured *before* the
            // base moves past it, or the range is the comment.s own length.
            if self.blank_line_between(self.source_base, start) {
                self.blank_line();
            } else {
                self.newline();
            }
            self.source_base = self.source_base.max(end);
            // The text keeps its own leading space, because a space after the `//` is
            // content — a comment holding an indented code sample means it. One is
            // added only when the author left none, so a bare `//` stays bare and
            // `//     example` does not become `//      example`.
            self.push("//");
            if !text.is_empty() && !text.starts_with(' ') {
                self.push(" ");
            }
            self.push(&text);
            self.newline();
        }
    }

    /// Whether the author left a blank line between two offsets in the source.
    fn blank_line_between(&self, from: u32, to: u32) -> bool {
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let to = usize::try_from(to).unwrap_or(0);
        let Some(between) = self.text.get(from..to) else {
            return false;
        };
        between.matches('\n').count() >= 2
    }

    /// Applies a gap.
    fn apply(&mut self, gap: Gap) {
        match gap {
            Gap::None => {}
            Gap::Space => {
                if !self.at_line_start && !self.out.ends_with(' ') {
                    self.out.push(' ');
                }
            }
            Gap::Line => {
                self.newline();
                self.indent();
            }
        }
    }

    /// Appends text at the current position.
    fn push(&mut self, text: &str) {
        if self.at_line_start {
            self.indent();
        }
        self.out.push_str(text);
        self.at_line_start = false;
    }

    /// Ends the current line.
    ///
    /// Idempotent, and that is not a detail: a `;` ends its line and then the next
    /// token's gap asks for a line as well, so a `newline` that was not a no-op at
    /// line start would put a blank line between every statement. Collapsing a run of
    /// newlines to one is also what makes "at most one blank line" true without a
    /// second pass, and a second pass is what would make the output's own text
    /// different from its input's.
    fn newline(&mut self) {
        if self.at_line_start || self.out.is_empty() {
            return;
        }
        self.trim_line_end();
        self.out.push('\n');
        self.at_line_start = true;
    }

    /// Ends the current line and leaves a blank one behind, unless there already is
    /// one. Collapsing a run of author blank lines to one is what keeps the output
    /// from growing on every pass.
    fn blank_line(&mut self) {
        if !self.at_line_start || self.out.is_empty() {
            self.newline();
        }
        if self.out.is_empty() || self.out.ends_with("\n\n") {
            return;
        }
        self.out.push('\n');
    }

    fn indent(&mut self) {
        if !self.at_line_start {
            return;
        }
        let levels = self.depth.saturating_add(self.continuation).min(MAX_DEPTH);
        for _ in 0..levels {
            self.out.push_str(INDENT);
        }
        // The indent *is* the start of a line's content, so writing it ends the
        // "line start" state. Leaving it set would let `push` write a second indent
        // on the same line, and a double-indented body is the exact bug this
        // arrangement invites.
        self.at_line_start = false;
    }

    /// Removes trailing spaces from the line being written.
    fn trim_line_end(&mut self) {
        while self.out.ends_with(' ') || self.out.ends_with('\t') {
            self.out.pop();
        }
    }

    /// Closes out the file: no trailing spaces, and exactly one trailing newline.
    fn finish(&mut self) {
        while self.out.ends_with(' ') || self.out.ends_with('\t') {
            self.out.pop();
        }
        while self.out.ends_with('\n') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
    }
}

/// Whether a token can be a prefix operator as well as an infix one.
fn is_prefix_capable(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Amp
            | TokenKind::Bang
            | TokenKind::Lt
            | TokenKind::Gt
    )
}

/// Whether a token can end an expression: a value, and nothing else.
///
/// A keyword cannot, which is the whole reason `if (a)` keeps its space and `f(x)`
/// does not. The lexer has keyword tokens of their own, so this is a fact about the
/// kind rather than a string comparison.
fn is_value_end(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident(_)
            | TokenKind::Int { .. }
            | TokenKind::Str(_)
            | TokenKind::CloseParen
            | TokenKind::CloseBracket
    )
}
