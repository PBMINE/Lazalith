//! Diagnostics for the Lazen frontend.
//!
//! The compiler has no diagnostic architecture of its own. Every failure is a
//! `lazalith_diagnostics::Diagnostic` carrying a stable code and a real
//! `lazalith_types::SourceSpan`, and the sources come from the shared
//! `SourceManager`.
//!
//! Code namespaces, all stable:
//!
//! | Range | Stage |
//! | --- | --- |
//! | `L0xxx` | lexer |
//! | `P0xxx` | parser |
//! | `N0xxx` | name resolution |
//! | `T0xxx` | types and semantics |

use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;
use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Label, Note, Severity};
use lazalith_types::{ByteOffset, SourceId, SourceManager, SourceSpan};

/// Builds a code, falling back to a valid code if a literal is ever malformed.
///
/// A code is a `const`-checked literal at every call site; the fallback exists
/// so a future edit cannot turn a diagnostic typo into a panic.
fn code(raw: &str) -> DiagnosticCode {
    DiagnosticCode::new(raw).unwrap_or_else(|_| {
        DiagnosticCode::new("T9999").expect("the fallback code is a valid literal")
    })
}

/// A frontend failure at one source location.
///
/// The payload is boxed for the same reason `lazalith-toolchain` boxes its
/// assembler errors: this crate compiles for a platform with a small stack, and
/// a diagnostic plus a source map is far too large to sit in every `Err`
/// variant of every parser function.
#[derive(Debug)]
pub struct StageError {
    payload: Box<StageErrorPayload>,
}

#[derive(Debug)]
struct StageErrorPayload {
    diagnostic: Diagnostic,
    sources: SourceManager,
}

impl StageError {
    fn new(diagnostic: Diagnostic, sources: SourceManager) -> Self {
        Self {
            payload: Box::new(StageErrorPayload {
                diagnostic,
                sources,
            }),
        }
    }

    /// The diagnostic.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.payload.diagnostic
    }

    /// The sources the diagnostic refers to.
    pub fn sources(&self) -> &SourceManager {
        &self.payload.sources
    }

    /// The diagnostic's code.
    pub fn code(&self) -> &DiagnosticCode {
        self.payload.diagnostic.code()
    }

    /// Renders the diagnostic with the shared renderer.
    pub fn render(&self) -> String {
        render_one(&self.payload.diagnostic, &self.payload.sources)
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

impl core::error::Error for StageError {}

/// A whole compilation failure: one or more diagnostics, plus their sources.
#[derive(Debug)]
pub struct CompileError {
    /// The diagnostics, ordered by source position.
    pub diagnostics: Vec<Diagnostic>,
    /// The sources they refer to.
    pub sources: SourceManager,
}

impl CompileError {
    /// Creates an error from one stage failure.
    pub fn from_stage(error: StageError) -> Self {
        Self {
            diagnostics: alloc::vec![error.payload.diagnostic],
            sources: error.payload.sources.clone(),
        }
    }

    /// Creates an error from many stage failures, keeping the sources of the
    /// first and merging the diagnostics.
    pub fn from_stages(errors: Vec<StageError>) -> Self {
        let mut diagnostics = Vec::new();
        let mut sources = None;
        for error in errors {
            if sources.is_none() {
                sources = Some(error.payload.sources.clone());
            }
            diagnostics.push(error.payload.diagnostic);
        }
        Self {
            diagnostics,
            sources: sources.unwrap_or_default(),
        }
    }

    /// The first diagnostic's code, for tests and CLI reporting.
    pub fn first_code(&self) -> Option<&DiagnosticCode> {
        self.diagnostics.first().map(Diagnostic::code)
    }

    /// Whether a diagnostic with the given code was reported.
    pub fn has_code(&self, wanted: &str) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == wanted)
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.diagnostics.first() {
            Some(diagnostic) => f.write_str(&render_one(diagnostic, &self.sources)),
            None => f.write_str("compilation failed"),
        }
    }
}

impl core::error::Error for CompileError {}

/// Renders every diagnostic of a failure, separated by a blank line.
pub fn render(error: &CompileError) -> String {
    let mut output = String::new();
    for (index, diagnostic) in error.diagnostics.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&render_one(diagnostic, &error.sources));
    }
    output
}

fn render_one(diagnostic: &Diagnostic, sources: &SourceManager) -> String {
    match lazalith_diagnostics::render_plain(diagnostic, sources) {
        Ok(rendered) => rendered,
        Err(error) => alloc::format!("{diagnostic} (not renderable: {error})"),
    }
}

/// A diagnostic under construction over one byte range of one file.
pub struct FileDiagnostic<'a> {
    source: SourceId,
    manager: &'a SourceManager,
    start: u32,
    end: u32,
}

impl<'a> FileDiagnostic<'a> {
    /// Starts a diagnostic over `[start, end)`.
    pub fn new(source: SourceId, manager: &'a SourceManager, start: u32, end: u32) -> Self {
        Self {
            source,
            manager,
            start,
            end,
        }
    }

    /// Starts a diagnostic over a span.
    pub fn from_span(source: SourceId, manager: &'a SourceManager, span: SourceSpan) -> Self {
        Self {
            source,
            manager,
            start: span.start().as_u32(),
            end: span.end().as_u32(),
        }
    }

    /// The span this diagnostic points at.
    ///
    /// A range is clamped into the file rather than discarded, and a range that
    /// cannot be made valid at all falls back to the start of the file, so a
    /// diagnostic never loses its location.
    pub fn span(&self) -> SourceSpan {
        let length = self
            .manager
            .file(self.source)
            .map(|file| file.text().len() as u32)
            .unwrap_or(0);
        let start = self.start.min(length);
        let end = self.end.clamp(start, length);
        self.manager
            .source_span(self.source, ByteOffset::new(start), ByteOffset::new(end))
            .or_else(|_| {
                self.manager
                    .source_span(self.source, ByteOffset::new(0), ByteOffset::new(0))
            })
            .expect("a zero-length span at offset zero is always valid")
    }

    /// Builds a diagnostic with an optional help line and notes.
    pub fn build(
        self,
        raw_code: &str,
        message: impl Into<String>,
        notes: &[&str],
        help: Option<&str>,
    ) -> StageError {
        let mut diagnostic = Diagnostic::new(Severity::Error, code(raw_code), message)
            .with_label(Label::primary(self.span(), "here"));
        for note in notes {
            diagnostic = diagnostic.with_note(Note::new(*note));
        }
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(lazalith_diagnostics::Help::new(help));
        }
        StageError::new(diagnostic, self.manager.clone())
    }
}

/// Convenience: a diagnostic over a span, with notes and help.
pub fn at(
    source: SourceId,
    manager: &SourceManager,
    span: SourceSpan,
    raw_code: &str,
    message: impl Into<String>,
    notes: &[&str],
    help: Option<&str>,
) -> StageError {
    FileDiagnostic::from_span(source, manager, span).build(raw_code, message, notes, help)
}

impl StageError {
    /// Builds a stage error from an already-built diagnostic and its sources.
    pub fn from_parts(diagnostic: Diagnostic, sources: SourceManager) -> Self {
        Self::new(diagnostic, sources)
    }
}
