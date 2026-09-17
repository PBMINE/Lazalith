//! Shared source location primitives for the Lazalith platform.
//!
//! One authoritative implementation lives here: [`SourceManager`] owns source
//! files, computes a line-start map once per file, and resolves validated
//! [`SourceSpan`]s to exact line/column positions. Lines are split on `\n`;
//! every other byte (including `\r`) is an ordinary character. Line numbers
//! and columns are 1-based; columns count `char`s, so multi-byte characters
//! occupy one column.

#![no_std]

extern crate alloc;

mod architecture;
mod clock;
pub use clock::{ClockOverflow, PreparedAdvance, VirtualClock};
mod config;
mod width;

pub use width::{ArithmeticResult, WidthError};

pub use config::{ArchitectureConfig, FeatureSet, InvalidFeatureSet, WordWidth};

pub use architecture::{
    CycleCount, DeviceId, DeviceOffset, InstructionAddress, InstructionCount, InvalidRegisterIndex,
    PhysicalAddress, RegisterIndex, VirtualAddress,
};

use alloc::{boxed::Box, string::String, vec, vec::Vec};
use core::{
    error::Error,
    fmt,
    ops::{Add, Sub},
};

/// A byte offset into a source file.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteOffset(u32);

impl ByteOffset {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }

    pub const fn as_usize(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for ByteOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Add for ByteOffset {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0.checked_add(rhs.0).expect("byte offset overflow"))
    }
}

impl Sub for ByteOffset {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0.checked_sub(rhs.0).expect("byte offset underflow"))
    }
}

/// Identifies a file registered with a [`SourceManager`].
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceId(u32);

impl SourceId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A source file with its precomputed line-start map.
#[derive(Clone, Debug)]
pub struct SourceFile {
    name: String,
    text: Box<str>,
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Builds the file and its authoritative line map in one pass.
    pub fn new(
        name: impl Into<String>,
        text: impl Into<Box<str>>,
    ) -> Result<Self, EmptySourceName> {
        let name = name.into();
        if name.is_empty() {
            return Err(EmptySourceName);
        }
        let text = text.into();
        let line_starts = line_starts_for(&text);
        Ok(Self {
            name,
            text,
            line_starts,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Number of lines; a trailing newline starts a final empty line.
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    pub fn line_text(&self, line: u32) -> Option<&str> {
        if line == 0 || line > self.line_count() {
            return None;
        }
        let start = self.line_starts[(line - 1) as usize] as usize;
        let end = if (line as usize) < self.line_starts.len() {
            self.line_starts[line as usize] as usize - 1
        } else {
            self.text.len()
        };
        Some(&self.text[start..end])
    }

    fn line_index_containing(&self, offset: u32) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(index) => index as u32,
            Err(index) => (index - 1) as u32,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptySourceName;

impl fmt::Display for EmptySourceName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("source file name must not be empty")
    }
}

impl Error for EmptySourceName {}

/// The single authoritative source map for the platform. Other components
/// must resolve locations through this type rather than re-deriving
/// line/column themselves.
#[derive(Clone, Debug, Default)]
pub struct SourceManager {
    files: Vec<SourceFile>,
}

impl SourceManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file; ids are assigned in registration order.
    pub fn add_file(
        &mut self,
        name: impl Into<String>,
        text: impl Into<Box<str>>,
    ) -> Result<SourceId, EmptySourceName> {
        let file = SourceFile::new(name, text)?;
        let id = SourceId::new(self.files.len() as u32);
        self.files.push(file);
        Ok(id)
    }

    pub fn file(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id.0 as usize)
    }

    pub fn line_text(&self, id: SourceId, line: u32) -> Option<&str> {
        self.file(id)?.line_text(line)
    }

    /// Validates and constructs a span against the current file contents.
    pub fn source_span(
        &self,
        id: SourceId,
        start: ByteOffset,
        end: ByteOffset,
    ) -> Result<SourceSpan, InvalidSpan> {
        SourceSpan::new(self, id, start, end)
    }

    /// Resolves `offset` to a 1-based line and 1-based char column.
    pub fn line_column(&self, id: SourceId, offset: ByteOffset) -> Option<LineColumn> {
        let file = self.files.get(id.0 as usize)?;
        resolve_line_column(file, offset)
    }

    /// Resolves a validated span to exact start/end positions.
    pub fn resolve(&self, span: &SourceSpan) -> Option<ResolvedSpan<'_>> {
        let start = self.line_column(span.id, span.start)?;
        let end = if span.end == span.start {
            start
        } else {
            self.line_column(span.id, span.end)?
        };
        Some(ResolvedSpan {
            file: self.files.get(span.id.0 as usize)?,
            start,
            end,
        })
    }
}

/// A 1-based line/column position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LineColumn {
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for LineColumn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// A span resolved against a specific file, ready for diagnostics.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedSpan<'a> {
    file: &'a SourceFile,
    start: LineColumn,
    end: LineColumn,
}

impl ResolvedSpan<'_> {
    pub fn file(&self) -> &SourceFile {
        self.file
    }

    pub fn file_name(&self) -> &str {
        self.file.name()
    }

    pub fn start(&self) -> LineColumn {
        self.start
    }

    pub fn end(&self) -> LineColumn {
        self.end
    }
}

impl fmt::Display for ResolvedSpan<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}",
            self.file.name(),
            self.start.line,
            self.start.column
        )?;
        if self.end != self.start {
            write!(f, "-{}:{}", self.end.line, self.end.column)?;
        }
        Ok(())
    }
}

fn line_starts_for(text: &str) -> Vec<u32> {
    let mut starts = vec![0u32];
    for (index, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            starts.push(index as u32 + 1);
        }
    }
    starts
}

fn resolve_line_column(file: &SourceFile, offset: ByteOffset) -> Option<LineColumn> {
    let offset = offset.as_usize();
    let text = file.text();
    if offset > text.len() || !text.is_char_boundary(offset) {
        return None;
    }
    let line_index = file.line_index_containing(offset as u32);
    let line_start = file.line_starts[line_index as usize] as usize;
    let column = text[line_start..offset].chars().count() as u32 + 1;
    Some(LineColumn {
        line: line_index + 1,
        column,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSpan {
    kind: InvalidSpanKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InvalidSpanKind {
    Reversed,
    OutOfBounds,
    InteriorBoundary,
}

impl fmt::Display for InvalidSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            InvalidSpanKind::Reversed => "source span end precedes start",
            InvalidSpanKind::OutOfBounds => "source span extends past end of file",
            InvalidSpanKind::InteriorBoundary => "source span interior splits a UTF-8 character",
        })
    }
}

impl Error for InvalidSpan {}

/// A byte range within one source file. Constructed only through
/// [`SourceManager::source_span`], which validates it; resolution cannot
/// fail for a span kept with its own manager.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceSpan {
    id: SourceId,
    start: ByteOffset,
    end: ByteOffset,
}

impl SourceSpan {
    fn new(
        manager: &SourceManager,
        id: SourceId,
        start: ByteOffset,
        end: ByteOffset,
    ) -> Result<Self, InvalidSpan> {
        if end < start {
            return Err(InvalidSpan {
                kind: InvalidSpanKind::Reversed,
            });
        }
        let Some(file) = manager.file(id) else {
            return Err(InvalidSpan {
                kind: InvalidSpanKind::OutOfBounds,
            });
        };
        let text = file.text();
        if end.as_u32() > text.len() as u32 {
            return Err(InvalidSpan {
                kind: InvalidSpanKind::OutOfBounds,
            });
        }
        if !text.is_char_boundary(start.as_usize()) || !text.is_char_boundary(end.as_usize()) {
            return Err(InvalidSpan {
                kind: InvalidSpanKind::InteriorBoundary,
            });
        }
        Ok(Self { id, start, end })
    }

    pub fn id(&self) -> SourceId {
        self.id
    }

    pub fn start(&self) -> ByteOffset {
        self.start
    }

    pub fn end(&self) -> ByteOffset {
        self.end
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, string::ToString, vec, vec::Vec};

    const SOURCE: &str = "let x = 1\nlet y = \"héllo\"\nlet z = 2\n";

    fn manager() -> (SourceManager, SourceId) {
        let mut manager = SourceManager::new();
        let id = manager.add_file("file.lz", SOURCE).unwrap();
        (manager, id)
    }

    fn find(manager: &SourceManager, id: SourceId, needle: &str) -> ByteOffset {
        let text = manager.file(id).unwrap().text();
        ByteOffset::new(text.find(needle).expect("needle present") as u32)
    }

    #[test]
    fn retrieves_line_text_from_the_authoritative_map() {
        let mut manager = SourceManager::new();
        let id = manager.add_file("lines.lz", "é\r\n\n\t界\r\n").unwrap();
        let file = manager.file(id).unwrap();
        for (line, expected) in [
            (0, None),
            (1, Some("é\r")),
            (2, Some("")),
            (3, Some("\t界\r")),
            (4, Some("")),
            (5, None),
            (u32::MAX, None),
        ] {
            assert_eq!(file.line_text(line), expected);
            assert_eq!(manager.line_text(id, line), expected);
        }
        assert_eq!(manager.line_text(SourceId::new(99), 1), None);
    }

    #[test]
    fn retrieves_empty_and_unterminated_final_lines() {
        let mut manager = SourceManager::new();
        let empty = manager.add_file("empty.lz", "").unwrap();
        assert_eq!(manager.line_text(empty, 1), Some(""));
        assert_eq!(manager.line_text(empty, 2), None);
        let id = manager.add_file("last.lz", "a\nβ").unwrap();
        assert_eq!(manager.line_text(id, 1), Some("a"));
        assert_eq!(manager.line_text(id, 2), Some("β"));
        assert_eq!(manager.line_text(id, 3), None);
    }

    #[test]
    fn computes_exact_one_based_lines_and_char_columns() {
        let (manager, id) = manager();
        let cases = [
            ("let x = 1", 1, 1),
            ("t x = 1", 1, 3),
            ("= 1", 1, 7),
            ("1\n", 1, 9),
            ("let y", 2, 1),
            ("y = ", 2, 5),
            ("let z", 3, 1),
            ("2\n", 3, 9),
        ];
        for (needle, line, column) in cases {
            let position = manager.line_column(id, find(&manager, id, needle)).unwrap();
            assert_eq!(
                (position.line, position.column),
                (line, column),
                "{needle:?}"
            );
        }
    }

    #[test]
    fn resolves_multibyte_columns_by_characters_not_bytes() {
        let (manager, id) = manager();
        let offset = find(&manager, id, "é");
        let position = manager.line_column(id, offset).unwrap();
        assert_eq!((position.line, position.column), (2, 11));
        let after = ByteOffset::new(offset.as_u32() + 2);
        let position = manager.line_column(id, after).unwrap();
        assert_eq!((position.line, position.column), (2, 12));
    }

    #[test]
    fn counts_lf_only_lines_and_reports_the_final_position() {
        let mut manager = SourceManager::new();
        let id = manager.add_file("lf.lz", "a\nb\nc\n").unwrap();
        let file = manager.file(id).unwrap();
        assert_eq!(file.line_count(), 4);
        let position = manager.line_column(id, ByteOffset::new(6)).unwrap();
        assert_eq!((position.line, position.column), (4, 1));
    }

    #[test]
    fn treats_carriage_returns_as_ordinary_characters() {
        let mut manager = SourceManager::new();
        let id = manager.add_file("crlf.lz", "ab\r\ncd\r\n").unwrap();
        assert_eq!(manager.file(id).unwrap().line_count(), 3);
        for (offset, line, column) in [(2u32, 1u32, 3u32), (4, 2, 1), (5, 2, 2)] {
            let position = manager.line_column(id, ByteOffset::new(offset)).unwrap();
            assert_eq!(
                (position.line, position.column),
                (line, column),
                "offset {offset}"
            );
        }
        let id = manager.add_file("cr.lz", "ab\rcd").unwrap();
        let position = manager.line_column(id, ByteOffset::new(3)).unwrap();
        assert_eq!((position.line, position.column), (1, 4));
        assert_eq!(manager.file(id).unwrap().line_count(), 1);
    }

    #[test]
    fn spans_render_as_file_line_column_pairs() {
        let (manager, id) = manager();
        let start = find(&manager, id, "let y");
        let span = manager
            .source_span(id, start, ByteOffset::new(start.as_u32() + 5))
            .unwrap();
        let resolved = manager.resolve(&span).unwrap();
        assert_eq!(resolved.file_name(), "file.lz");
        assert_eq!(resolved.start().line, 2);
        assert_eq!(resolved.start().column, 1);
        assert_eq!(resolved.end().line, 2);
        assert_eq!(resolved.end().column, 6);
        assert_eq!(resolved.to_string(), "file.lz:2:1-2:6");

        let point = manager.source_span(id, start, start).unwrap();
        let resolved = manager.resolve(&point).unwrap();
        assert!(point.is_empty());
        assert_eq!(resolved.to_string(), "file.lz:2:1");
    }

    #[test]
    fn empty_and_single_char_spans_stay_distinguishable() {
        let (manager, id) = manager();
        let start = find(&manager, id, "let y");
        let empty = manager.source_span(id, start, start).unwrap();
        let resolved = manager.resolve(&empty).unwrap();
        assert!(empty.is_empty());
        assert_eq!(resolved.start(), resolved.end());

        let single = manager
            .source_span(id, start, ByteOffset::new(start.as_u32() + 1))
            .unwrap();
        let resolved = manager.resolve(&single).unwrap();
        assert!(!single.is_empty());
        assert_eq!(resolved.end().column, resolved.start().column + 1);
    }

    #[test]
    fn multi_line_spans_report_distinct_end_lines() {
        let (manager, id) = manager();
        let start = find(&manager, id, "let x");
        let end = find(&manager, id, "let z") + ByteOffset::new(5);
        let span = manager.source_span(id, start, end).unwrap();
        let resolved = manager.resolve(&span).unwrap();
        assert_eq!((resolved.start().line, resolved.start().column), (1, 1));
        assert_eq!((resolved.end().line, resolved.end().column), (3, 6));
        assert_eq!(resolved.to_string(), "file.lz:1:1-3:6");
    }

    #[test]
    fn rejects_reversed_spans() {
        let (manager, id) = manager();
        let error = manager
            .source_span(id, ByteOffset::new(5), ByteOffset::new(3))
            .unwrap_err();
        assert_eq!(error.to_string(), "source span end precedes start");
    }

    #[test]
    fn rejects_out_of_bounds_and_unknown_source_ids() {
        let (manager, id) = manager();
        let len = manager.file(id).unwrap().text().len() as u32;
        for (id, end) in [(id, len + 1), (id, u32::MAX), (SourceId::new(99), 1)] {
            let error = manager
                .source_span(id, ByteOffset::new(0), ByteOffset::new(end))
                .unwrap_err();
            assert_eq!(error.to_string(), "source span extends past end of file");
        }
        assert!(manager.file(SourceId::new(99)).is_none());
        assert!(
            manager
                .line_column(SourceId::new(99), ByteOffset::new(0))
                .is_none()
        );
    }

    #[test]
    fn rejects_offsets_that_split_utf8_characters() {
        let (manager, id) = manager();
        let offset = find(&manager, id, "é") + ByteOffset::new(1);
        assert!(manager.line_column(id, offset).is_none());
        let error = manager
            .source_span(id, ByteOffset::new(offset.as_u32() - 1), offset)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "source span interior splits a UTF-8 character"
        );
    }

    #[test]
    fn rejects_empty_source_names_without_registering_a_file() {
        let mut manager = SourceManager::new();
        let error = manager.add_file("", "text").unwrap_err();
        assert_eq!(error.to_string(), "source file name must not be empty");
        assert!(manager.file(SourceId::new(0)).is_none());
    }

    #[test]
    fn empty_files_support_zero_length_spans() {
        let mut manager = SourceManager::new();
        let id = manager.add_file("empty.lz", "").unwrap();
        assert_eq!(manager.file(id).unwrap().line_count(), 1);
        let position = manager.line_column(id, ByteOffset::new(0)).unwrap();
        assert_eq!((position.line, position.column), (1, 1));
        assert!(
            manager
                .source_span(id, ByteOffset::new(0), ByteOffset::new(0))
                .is_ok()
        );
    }

    #[test]
    fn offsets_compose_arithmetically() {
        let a = ByteOffset::new(4);
        let b = ByteOffset::new(10);
        assert_eq!(a + b, ByteOffset::new(14));
        assert_eq!(b - a, ByteOffset::new(6));
        assert_eq!(a.as_u32(), 4);
        assert_eq!(a.as_usize(), 4);
        assert_eq!(b.to_string(), "10");
    }

    #[test]
    fn source_ids_render_prefixed_and_ordered() {
        let low = SourceId::new(0);
        let high = SourceId::new(7);
        assert_eq!(low.to_string(), "#0");
        assert_eq!(high.to_string(), "#7");
        assert!(low < high);
    }

    #[test]
    fn spans_are_copyable_value_types() {
        let (manager, id) = manager();
        let span = manager
            .source_span(id, ByteOffset::new(0), ByteOffset::new(3))
            .unwrap();
        let copy = span;
        assert_eq!(span, copy);
        assert_eq!(copy.id(), id);
        let collected: Vec<SourceSpan> = vec![span, copy];
        assert_eq!(collected.len(), 2);
        assert_eq!(format!("{copy:?}"), format!("{span:?}"));
    }
}
