//! C's diagnostics.
//!
//! This is the same shape as the Lazen front end's: a small boxed error that
//! carries one diagnostic and the source map that renders it. It is a *separate*
//! type, not a shared one, because the two languages have different codes and a
//! type that could hold either would let a C diagnostic be rendered with a
//! Lazen source map — which renders, and renders nonsense.
//!
//! The code ranges are documented on the crate root. A malformed code falls back
//! to the stage's `C9xxx`, so a diagnostic is never dropped for want of a valid
//! code string: a missing code is a bug here, and losing the error that reports
//! it would be a second one.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Severity};
use lazalith_types::{ByteOffset, SourceId, SourceManager, SourceSpan};

/// The fallback for a code that is not a valid code string.
const FALLBACK: &str = "C9999";

/// A diagnostic code, falling back rather than failing.
///
/// A code is a literal at every call site in this crate, so a fallback is
/// unreachable. It is here anyway because the alternative is `expect` on a
/// value derived from a name, and a compiler that panics while reporting a
/// mistake in the program it is compiling has failed at its one job.
pub(crate) fn raw(value: &str) -> DiagnosticCode {
    DiagnosticCode::new(value.to_owned()).unwrap_or_else(|_| {
        DiagnosticCode::new(FALLBACK).expect("the fallback code is a valid literal")
    })
}

/// A diagnostic with the source map that renders it.
#[derive(Clone)]
pub struct StageError {
    payload: alloc::boxed::Box<Payload>,
}

#[derive(Clone)]
struct Payload {
    diagnostic: Diagnostic,
    sources: SourceManager,
}

impl StageError {
    /// The diagnostic itself.
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.payload.diagnostic
    }

    /// The source map, for rendering.
    pub fn sources(&self) -> &SourceManager {
        &self.payload.sources
    }

    /// The diagnostic's code.
    pub fn code(&self) -> &DiagnosticCode {
        self.payload.diagnostic.code()
    }

    /// Builds one from its parts.
    pub fn from_parts(diagnostic: Diagnostic, sources: SourceManager) -> Self {
        Self {
            payload: alloc::boxed::Box::new(Payload {
                diagnostic,
                sources,
            }),
        }
    }

    /// The diagnostic rendered against its sources.
    ///
    /// A diagnostic that will not render falls back to its code and message,
    /// which need no source. Dropping it instead would report a successful
    /// compile, and a compile that failed is the one thing a caller must hear
    /// about.
    pub fn render(&self) -> String {
        match lazalith_diagnostics::render_plain(&self.payload.diagnostic, &self.payload.sources) {
            Ok(rendered) => rendered,
            Err(error) => alloc::format!("{} (not renderable: {error})", self.payload.diagnostic),
        }
    }
}

impl fmt::Display for StageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

impl fmt::Debug for StageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StageError")
            .field("code", self.payload.diagnostic.code())
            .finish()
    }
}

impl Error for StageError {}

/// Every failure a whole compile produced.
#[derive(Debug)]
pub struct CompileError {
    /// The diagnostics, in the order the stages found them.
    pub diagnostics: Vec<Diagnostic>,
    /// The source map they render against.
    pub sources: SourceManager,
}

impl CompileError {
    /// Wraps one stage failure.
    pub fn from_stage(error: StageError) -> Self {
        Self {
            diagnostics: alloc::vec![error.payload.diagnostic],
            sources: error.payload.sources,
        }
    }

    /// Wraps several stage failures.
    pub fn from_stages(errors: Vec<StageError>) -> Self {
        let mut diagnostics = Vec::new();
        let mut sources = None;
        for error in errors {
            if sources.is_none() {
                sources = Some(error.payload.sources);
            }
            diagnostics.push(error.payload.diagnostic);
        }
        Self {
            diagnostics,
            // Every stage shares one source map, so the first one is the map.
            // A compile with no failures has no error, so this only runs with
            // at least one stage failure.
            sources: sources.unwrap_or_else(SourceManager::new),
        }
    }

    /// The first code, for a caller that only wants to know what to look for.
    pub fn first_code(&self) -> Option<&DiagnosticCode> {
        self.diagnostics.first().map(Diagnostic::code)
    }

    /// Whether any diagnostic carries a code.
    pub fn has_code(&self, wanted: &str) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == wanted)
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&render(self))
    }
}

impl Error for CompileError {}

/// Renders every diagnostic, separated by a blank line.
///
/// A person reading a compile failure wants all of them, and one run's worth.
pub fn render(error: &CompileError) -> String {
    let mut rendered = String::new();
    for (index, diagnostic) in error.diagnostics.iter().enumerate() {
        if index > 0 {
            rendered.push('\n');
        }
        match lazalith_diagnostics::render_plain(diagnostic, &error.sources) {
            Ok(text) => rendered.push_str(&text),
            // A diagnostic that cannot be rendered is still a diagnostic, and
            // dropping it would report a successful compile. The code and the
            // message are the parts that need no source.
            Err(_) => {
                rendered.push_str(&alloc::format!(
                    "{}: {}\n",
                    diagnostic.code(),
                    diagnostic.message()
                ));
            }
        }
    }
    rendered
}

/// A diagnostic tied to one span of one file.
///
/// Constructed with the map it will be rendered against, so no span can be
/// quoted from a file the renderer does not have.
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

    /// Builds the diagnostic.
    pub fn build(
        self,
        code: &str,
        message: impl Into<String>,
        notes: &[&str],
        help: Option<&str>,
    ) -> StageError {
        let span = self.span();
        let mut diagnostic = Diagnostic::new(Severity::Error, raw(code), message)
            .with_label(lazalith_diagnostics::Label::primary(span, "here"));
        for note in notes {
            diagnostic = diagnostic.with_note(lazalith_diagnostics::Note::new(*note));
        }
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(lazalith_diagnostics::Help::new(help));
        }
        StageError::from_parts(diagnostic, self.manager.clone())
    }
}

/// Builds a diagnostic at a span.
pub fn at(
    source: SourceId,
    manager: &SourceManager,
    span: SourceSpan,
    code: &str,
    message: impl Into<String>,
    notes: &[&str],
    help: Option<&str>,
) -> StageError {
    FileDiagnostic::from_span(source, manager, span).build(code, message, notes, help)
}
