#![no_std]

extern crate alloc;
use alloc::{format, string::String, string::ToString, sync::Arc, vec::Vec};
use core::{error::Error, fmt};
use lazalith_types::{InvalidSpan, SourceManager, SourceSpan};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Severity {
    Error,
    Warning,
    Note,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        })
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidDiagnosticCode> {
        let value = value.into();
        let bytes = value.as_bytes();
        if bytes.len() < 2
            || !bytes[0].is_ascii_uppercase()
            || !bytes[1..].iter().all(u8::is_ascii_digit)
        {
            return Err(InvalidDiagnosticCode { value });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidDiagnosticCode {
    value: String,
}

impl InvalidDiagnosticCode {
    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for InvalidDiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid diagnostic code {:?}: expected an ASCII uppercase letter followed by digits",
            self.value
        )
    }
}

impl Error for InvalidDiagnosticCode {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Label {
    style: LabelStyle,
    span: SourceSpan,
    message: String,
}

impl Label {
    pub fn primary(span: SourceSpan, message: impl Into<String>) -> Self {
        Self {
            style: LabelStyle::Primary,
            span,
            message: message.into(),
        }
    }

    pub fn secondary(span: SourceSpan, message: impl Into<String>) -> Self {
        Self {
            style: LabelStyle::Secondary,
            span,
            message: message.into(),
        }
    }

    pub fn style(&self) -> LabelStyle {
        self.style
    }

    pub fn span(&self) -> SourceSpan {
        self.span.clone()
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Note(String);

impl Note {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Help(String);

impl Help {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    severity: Severity,
    code: DiagnosticCode,
    message: String,
    /// The underlying error, shared rather than owned.
    ///
    /// An `Arc` and not a `Box`, because a diagnostic is a *value* that a
    /// frontend keeps, copies into a panel, and hands to a callback — and a type
    /// that cannot be copied is a type every consumer has to work around. The
    /// error itself is immutable once recorded, so sharing it costs nothing.
    cause: Option<Arc<dyn Error + Send + Sync>>,
    labels: Vec<Label>,
    notes: Vec<Note>,
    help: Vec<Help>,
}

impl Diagnostic {
    pub fn new(severity: Severity, code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            cause: None,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
        }
    }

    pub fn with_cause(mut self, cause: impl Error + Send + Sync + 'static) -> Self {
        self.cause = Some(Arc::new(cause));
        self
    }

    pub fn with_label(mut self, label: Label) -> Self {
        self.labels.push(label);
        self
    }

    pub fn with_note(mut self, note: Note) -> Self {
        self.notes.push(note);
        self
    }

    pub fn with_help(mut self, help: Help) -> Self {
        self.help.push(help);
        self
    }

    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    pub fn notes(&self) -> &[Note] {
        &self.notes
    }

    pub fn help(&self) -> &[Help] {
        &self.help
    }

    pub fn cause(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        self.cause.as_deref()
    }

    pub fn severity(&self) -> Severity {
        self.severity
    }

    pub fn code(&self) -> &DiagnosticCode {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity, self.code, self.message)
    }
}

impl Error for Diagnostic {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as &dyn Error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderError {
    WrongSource {
        label_index: usize,
        span: SourceSpan,
    },
    MissingSource {
        label_index: usize,
        span: SourceSpan,
    },
    InvalidSpan {
        label_index: usize,
        span: SourceSpan,
        source: InvalidSpan,
    },
    UnresolvedSpan {
        label_index: usize,
        span: SourceSpan,
    },
    MissingLine {
        label_index: usize,
        span: SourceSpan,
        line: u32,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongSource { label_index, span } => {
                write!(
                    f,
                    "label {label_index}: source {} has different provenance",
                    span.id()
                )
            }
            Self::MissingSource { label_index, span } => {
                write!(f, "label {label_index}: source {} is missing", span.id())
            }
            Self::InvalidSpan {
                label_index,
                source,
                ..
            } => {
                write!(f, "label {label_index}: {source}")
            }
            Self::UnresolvedSpan { label_index, .. } => {
                write!(f, "label {label_index}: source span cannot be resolved")
            }
            Self::MissingLine {
                label_index,
                span,
                line,
            } => {
                write!(
                    f,
                    "label {label_index}: source {} has no line {line}",
                    span.id()
                )
            }
        }
    }
}

impl Error for RenderError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidSpan { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub fn render_plain(
    diagnostic: &Diagnostic,
    sources: &SourceManager,
) -> Result<String, RenderError> {
    let resolved = diagnostic
        .labels()
        .iter()
        .enumerate()
        .map(|(label_index, label)| {
            let span = label.span();
            if sources.file(span.id()).is_none() {
                return Err(RenderError::MissingSource { label_index, span });
            }
            sources
                .resolve(&span)
                .ok_or(RenderError::WrongSource { label_index, span })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut output = format!("{diagnostic}\n");
    for (label_index, (label, resolved)) in diagnostic.labels().iter().zip(resolved).enumerate() {
        let start = resolved.start();
        let end = resolved.end();
        let last_line = if end.line > start.line && end.column == 1 {
            end.line - 1
        } else {
            end.line
        };
        let width = format!("{last_line}").len();
        let gutter = " ".repeat(width);
        output.push_str(&format!(
            "{gutter}--> {}:{start}\n{gutter} |\n",
            resolved.file_name()
        ));
        for line in start.line..=last_line {
            let text =
                sources
                    .line_text(label.span().id(), line)
                    .ok_or(RenderError::MissingLine {
                        label_index,
                        span: label.span(),
                        line,
                    })?;
            let first = if line == start.line {
                start.column as usize - 1
            } else {
                0
            };
            let last = if line == end.line {
                end.column as usize - 1
            } else {
                text.chars().count()
            };
            let marker = match label.style() {
                LabelStyle::Primary => "^",
                LabelStyle::Secondary => "-",
            };
            output.push_str(&format!(
                "{line:>width$} | {text}\n{gutter} | {}{}",
                " ".repeat(first),
                marker.repeat(last.saturating_sub(first).max(1))
            ));
            if line == last_line && !label.message().is_empty() {
                output.push(' ');
                output.push_str(label.message());
            }
            output.push('\n');
        }
        output.push_str(&format!("{gutter} |\n"));
    }
    for note in diagnostic.notes() {
        output.push_str(&format!("  = note: {}\n", note.message()));
    }
    for help in diagnostic.help() {
        output.push_str(&format!("  = help: {}\n", help.message()));
    }
    if let Some(cause) = diagnostic.cause() {
        output.push_str(&format!("  = cause: {cause}\n"));
    }
    Ok(output)
}

/// Two diagnostics are equal when everything a consumer can see is equal.
///
/// The cause is compared by its rendered text rather than by identity. A cause is
/// a `dyn Error` and so has no equality of its own, and two diagnostics that
/// report the same underlying error are the same diagnostic to anything reading
/// them — a test that asserted otherwise would be asserting that two separate
/// `WidthError` values differed, which says nothing about a diagnostic.
impl PartialEq for Diagnostic {
    fn eq(&self, other: &Self) -> bool {
        self.severity == other.severity
            && self.code == other.code
            && self.message == other.message
            && self.labels == other.labels
            && self.notes == other.notes
            && self.help == other.help
            && self.cause.as_ref().map(ToString::to_string)
                == other.cause.as_ref().map(ToString::to_string)
    }
}

impl Eq for Diagnostic {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, string::ToString};

    fn diagnostic() -> Diagnostic {
        Diagnostic::new(
            Severity::Error,
            DiagnosticCode::new("E1001").unwrap(),
            "example failure",
        )
    }

    fn span(
        sources: &SourceManager,
        id: lazalith_types::SourceId,
        start: u32,
        end: u32,
    ) -> SourceSpan {
        sources
            .source_span(
                id,
                lazalith_types::ByteOffset::new(start),
                lazalith_types::ByteOffset::new(end),
            )
            .unwrap()
    }

    #[test]
    fn attachments_remain_structured_and_ordered() {
        let mut sources = SourceManager::new();
        let id = sources.add_file("example.lz", "hello").unwrap();
        let range = span(&sources, id, 0, 5);
        let diagnostic = diagnostic()
            .with_label(Label::secondary(range.clone(), String::from("context")))
            .with_label(Label::primary(range.clone(), "failure"))
            .with_note(Note::new(String::from("first note")))
            .with_note(Note::new("second note"))
            .with_help(Help::new(String::from("first help")))
            .with_help(Help::new("second help"))
            .with_cause(DiagnosticCode::new("bad").unwrap_err());
        assert_eq!(diagnostic.labels()[0].style(), LabelStyle::Secondary);
        assert_eq!(diagnostic.labels()[1].style(), LabelStyle::Primary);
        assert_eq!(diagnostic.labels()[0].span(), range);
        assert_eq!(diagnostic.labels()[0].message(), "context");
        assert_eq!(
            diagnostic.notes(),
            &[Note::new("first note"), Note::new("second note")]
        );
        assert_eq!(
            diagnostic.help(),
            &[Help::new("first help"), Help::new("second help")]
        );
        assert_eq!(
            diagnostic
                .cause()
                .unwrap()
                .downcast_ref::<InvalidDiagnosticCode>()
                .unwrap()
                .value(),
            "bad"
        );
        assert_eq!(diagnostic.to_string(), "error[E1001]: example failure");
    }

    #[test]
    fn renders_the_roadmap_example() {
        let mut sources = SourceManager::new();
        let id = sources.add_file("example.lz", "\n\n\nhello").unwrap();
        let diagnostic = diagnostic()
            .with_label(Label::primary(span(&sources, id, 3, 8), ""))
            .with_help(Help::new("example"));
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                " --> example.lz:4:1\n",
                "  |\n",
                "4 | hello\n",
                "  | ^^^^^\n",
                "  |\n",
                "  = help: example\n",
            )
        );
    }

    #[test]
    fn renders_without_labels_and_with_typed_cause() {
        let sources = SourceManager::new();
        for (severity, name) in [
            (Severity::Error, "error"),
            (Severity::Warning, "warning"),
            (Severity::Note, "note"),
        ] {
            let diagnostic =
                Diagnostic::new(severity, DiagnosticCode::new("N1").unwrap(), "message");
            assert_eq!(
                render_plain(&diagnostic, &sources).unwrap(),
                format!("{name}[N1]: message\n")
            );
        }
        let diagnostic = diagnostic()
            .with_note(Note::new("first"))
            .with_note(Note::new("second"))
            .with_help(Help::new("try this"))
            .with_help(Help::new("or this"))
            .with_cause(DiagnosticCode::new("bad").unwrap_err());
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                "  = note: first\n",
                "  = note: second\n",
                "  = help: try this\n",
                "  = help: or this\n",
                "  = cause: invalid diagnostic code \"bad\": expected an ASCII uppercase letter followed by digits\n",
            )
        );
        assert!(diagnostic.cause().unwrap().is::<InvalidDiagnosticCode>());
    }

    #[test]
    fn renders_multiple_overlapping_labels_and_files_in_insertion_order() {
        let mut sources = SourceManager::new();
        let a = sources.add_file("a.lz", "hello").unwrap();
        let b = sources.add_file("b.lz", "world").unwrap();
        let diagnostic = diagnostic()
            .with_label(Label::secondary(span(&sources, a, 1, 4), "context"))
            .with_label(Label::primary(span(&sources, b, 0, 5), "failure"))
            .with_label(Label::primary(span(&sources, a, 2, 3), "overlap"));
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                " --> a.lz:1:2\n  |\n1 | hello\n  |  --- context\n  |\n",
                " --> b.lz:1:1\n  |\n1 | world\n  | ^^^^^ failure\n  |\n",
                " --> a.lz:1:3\n  |\n1 | hello\n  |   ^ overlap\n  |\n",
            )
        );
    }

    #[test]
    fn renders_unicode_scalar_columns_not_bytes_or_display_cells() {
        let mut sources = SourceManager::new();
        let id = sources.add_file("unicode.lz", "é界e\u{301}🙂z").unwrap();
        let diagnostic =
            diagnostic().with_label(Label::primary(span(&sources, id, 5, 12), "three scalars"));
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                " --> unicode.lz:1:3\n  |\n1 | é界e\u{301}🙂z\n  |   ^^^ three scalars\n  |\n",
            )
        );
    }

    #[test]
    fn renders_multiline_spans_and_blank_lines_with_aligned_gutters() {
        let mut sources = SourceManager::new();
        let id = sources
            .add_file("multi.lz", "\n\n\n\n\n\n\n\nab\n\ncdé")
            .unwrap();
        let diagnostic =
            diagnostic().with_label(Label::secondary(span(&sources, id, 9, 16), "range"));
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                "  --> multi.lz:9:2\n   |\n",
                " 9 | ab\n   |  -\n",
                "10 | \n   | -\n",
                "11 | cdé\n   | --- range\n   |\n",
            )
        );
    }

    #[test]
    fn excludes_an_end_line_at_column_one_from_half_open_ranges() {
        let mut sources = SourceManager::new();
        let id = sources.add_file("lines.lz", "ab\ncd\n").unwrap();
        for (start, end, expected) in [
            (1, 3, " --> lines.lz:1:2\n  |\n1 | ab\n  |  ^\n  |\n"),
            (2, 3, " --> lines.lz:1:3\n  |\n1 | ab\n  |   ^\n  |\n"),
            (
                0,
                6,
                " --> lines.lz:1:1\n  |\n1 | ab\n  | ^^\n2 | cd\n  | ^^\n  |\n",
            ),
        ] {
            let diagnostic =
                diagnostic().with_label(Label::primary(span(&sources, id, start, end), ""));
            assert_eq!(
                render_plain(&diagnostic, &sources).unwrap(),
                format!("error[E1001]: example failure\n{expected}")
            );
        }
    }

    #[test]
    fn renders_points_at_empty_files_eof_and_inside_lines() {
        for (text, offset, expected) in [
            ("", 0, " --> point.lz:1:1\n  |\n1 | \n  | ^ here\n  |\n"),
            ("é", 2, " --> point.lz:1:2\n  |\n1 | é\n  |  ^ here\n  |\n"),
            ("x\n", 2, " --> point.lz:2:1\n  |\n2 | \n  | ^ here\n  |\n"),
            (
                "ab",
                1,
                " --> point.lz:1:2\n  |\n1 | ab\n  |  ^ here\n  |\n",
            ),
        ] {
            let mut sources = SourceManager::new();
            let id = sources.add_file("point.lz", text).unwrap();
            let diagnostic =
                diagnostic().with_label(Label::primary(span(&sources, id, offset, offset), "here"));
            assert_eq!(
                render_plain(&diagnostic, &sources).unwrap(),
                format!("error[E1001]: example failure\n{expected}")
            );
        }
    }

    #[test]
    fn preserves_cr_and_tabs_as_single_scalars() {
        let mut sources = SourceManager::new();
        let id = sources.add_file("crlf.lz", "\tx\r\ny").unwrap();
        let diagnostic = diagnostic().with_label(Label::primary(span(&sources, id, 2, 5), ""));
        assert_eq!(
            render_plain(&diagnostic, &sources).unwrap(),
            concat!(
                "error[E1001]: example failure\n",
                " --> crlf.lz:1:3\n  |\n1 | \tx\r\n  |   ^\n2 | y\n  | ^\n  |\n",
            )
        );
    }

    #[test]
    fn rejects_missing_source_even_after_valid_labels() {
        let mut original = SourceManager::new();
        original.add_file("first.lz", "a").unwrap();
        let id = original.add_file("missing.lz", "b").unwrap();
        let missing = span(&original, id, 0, 1);
        let mut sources = SourceManager::new();
        let first = sources.add_file("first.lz", "a").unwrap();
        let diagnostic = diagnostic()
            .with_label(Label::primary(span(&sources, first, 0, 1), "valid"))
            .with_label(Label::secondary(missing.clone(), "missing"));
        let error = render_plain(&diagnostic, &sources).unwrap_err();
        assert_eq!(
            error,
            RenderError::MissingSource {
                label_index: 1,
                span: missing
            }
        );
        assert_eq!(error.to_string(), "label 1: source #1 is missing");
        assert!(error.source().is_none());
    }

    #[test]
    fn rejects_spans_from_another_source_manager() {
        let mut original = SourceManager::new();
        let original_id = original.add_file("original.lz", "abcd").unwrap();
        let invalid = span(&original, original_id, 1, 3);
        let mut sources = SourceManager::new();
        let id = sources.add_file("replacement.lz", "abcd").unwrap();
        let diagnostic = diagnostic()
            .with_label(Label::primary(span(&sources, id, 0, 0), "valid"))
            .with_label(Label::secondary(invalid.clone(), "invalid"));
        let error = render_plain(&diagnostic, &sources).unwrap_err();
        assert_eq!(
            error,
            RenderError::WrongSource {
                label_index: 1,
                span: invalid,
            }
        );
        assert_eq!(
            error.to_string(),
            "label 1: source #0 has different provenance"
        );
        assert!(error.source().is_none());
    }

    #[test]
    fn accepts_codes_without_losing_their_identity() {
        for value in ["E0", "W0001", "N42", "E1001"] {
            let code = DiagnosticCode::new(value).unwrap();
            assert_eq!(code.as_str(), value);
            assert_eq!(code.to_string(), value);
            assert_eq!(code.clone(), code);
        }
    }

    #[test]
    fn rejects_invalid_codes_with_original_input() {
        for value in [
            "", "E", "1001", "e1001", "E 1", " E1", "E1\n", "E１", "É1", "E1A",
        ] {
            let error = DiagnosticCode::new(value).unwrap_err();
            assert_eq!(error.value(), value);
            assert!(error.source().is_none());
            assert_eq!(
                error.to_string(),
                format!(
                    "invalid diagnostic code {value:?}: expected an ASCII uppercase letter followed by digits"
                )
            );
        }
    }

    #[test]
    fn retains_structured_fields_without_a_cause() {
        let diagnostic = diagnostic();
        assert_eq!(diagnostic.severity(), Severity::Error);
        assert_eq!(diagnostic.code().as_str(), "E1001");
        assert_eq!(diagnostic.message(), "example failure");
        assert!(diagnostic.source().is_none());
    }

    #[test]
    fn formats_each_severity_without_terminal_styling() {
        for (severity, name) in [
            (Severity::Error, "error"),
            (Severity::Warning, "warning"),
            (Severity::Note, "note"),
        ] {
            let diagnostic = Diagnostic::new(
                severity,
                DiagnosticCode::new("E1001").unwrap(),
                "example failure",
            );
            assert_eq!(
                diagnostic.to_string(),
                format!("{name}[E1001]: example failure")
            );
        }
    }

    #[test]
    fn preserves_typed_causes_and_nested_error_chains() {
        let root = DiagnosticCode::new("invalid").unwrap_err();
        let inner = diagnostic().with_cause(root);
        let outer = diagnostic().with_cause(inner);
        let source = outer.source().unwrap();
        assert!(source.downcast_ref::<Diagnostic>().is_some());
        let root = source.source().unwrap();
        assert_eq!(
            root.downcast_ref::<InvalidDiagnosticCode>()
                .unwrap()
                .value(),
            "invalid"
        );
        assert!(root.source().is_none());
        assert_eq!(outer.to_string(), "error[E1001]: example failure");
    }

    #[test]
    fn diagnostics_can_be_owned_by_host_frontends() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Diagnostic>();
        let message = String::from("owned message");
        let diagnostic = Diagnostic::new(
            Severity::Warning,
            DiagnosticCode::new("W1").unwrap(),
            message,
        );
        assert_eq!(diagnostic.message(), "owned message");
    }
}
