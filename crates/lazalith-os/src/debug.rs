//! Source-level debug information, and the format it travels in.
//!
//! # What this is
//!
//! A table of *where the code came from*, and the source it came from. One entry
//! per run of instructions, each naming an address in the loaded image and a byte
//! range in one of the embedded sources. That is enough to turn a program counter
//! into a file, a line and a column, and to turn a line back into the addresses a
//! breakpoint belongs at.
//!
//! # Why one definition, here
//!
//! The linker writes this block and the loader reads it, so the format has to be
//! written down once. It lives in the OS crate because that is the crate both of
//! them can see: `lazalith-toolchain` links images and already depends on the OS,
//! and duplicating the encoding in both would be two formats that drift.
//!
//! # Why the source text is embedded
//!
//! A mapping is a byte offset, and a byte offset is meaningless without the text
//! it is an offset into. So the text travels with the mappings. The alternative —
//! a path, and a debugger that opens files — breaks the moment the program is run
//! somewhere the source is not, which is most of the time a bug is being chased.
//!
//! # What is *not* here
//!
//! No host state, no device state, and no file handles. Everything in a debug
//! block is something a guest could have observed, which is the same rule a
//! machine snapshot follows and for the same reason.

use alloc::string::String;
use alloc::vec::Vec;

use lazalith_types::{ByteOffset, LineColumn, SourceFile, SourceId, SourceManager};

/// The block's magic, so a reader can tell a debug block from anything else.
pub const DEBUG_MAGIC: [u8; 8] = *b"LZXDBG01";

/// The block's format version.
pub const DEBUG_VERSION: u16 = 1;

/// The bytes one mapping takes: an address, a source, and a source range.
const ENTRY_SIZE: usize = 20;

/// The bytes a block's header takes, before any file or mapping.
const HEADER_SIZE: usize = 20;

/// The least a file costs: the two length words its name and text have.
const MINIMUM_FILE_SIZE: usize = 8;

/// Why a debug block would not read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebugError {
    /// The block does not start with the magic.
    Magic,
    /// The block's version is not one this build reads.
    Version {
        /// The version found.
        found: u16,
    },
    /// The block ended in the middle of something.
    Truncated {
        /// The offset it ran out at.
        offset: usize,
        /// How many more bytes it needed.
        needed: usize,
    },
    /// A count in the block does not fit a host's index space.
    Count {
        /// What was being counted.
        what: &'static str,
        /// The value found.
        value: u64,
    },
    /// A stored length and the bytes it describes disagree.
    ///
    /// The length is derived when the block is written, so this means the file was
    /// edited or truncated. It is refused rather than believed, because a mapping
    /// that resolves against a length the text does not have is a mapping that
    /// resolves to the wrong line.
    LengthMismatch {
        /// What the block said.
        stored: u32,
        /// How many bytes were there.
        found: u32,
    },
    /// A mapping reaches outside the source it names.
    Range {
        /// The end of the range.
        end: u32,
        /// The source's length.
        length: u32,
    },
    /// A source's name or text could not be built.
    Source,
    /// The block has bytes after everything it said it had.
    ///
    /// Refused rather than ignored: a block with a tail is a block whose writer
    /// and reader disagree about its contents, and reading the part both agree
    /// about would hide that instead of reporting it.
    TrailingBytes {
        /// The offset the tail starts at.
        offset: usize,
        /// How many bytes of tail there are.
        length: usize,
    },
}

impl core::fmt::Display for DebugError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Magic => write!(f, "the debug block does not start with its magic"),
            Self::Version { found } => {
                write!(
                    f,
                    "the debug block is version {found}, and this reads version 1"
                )
            }
            Self::Truncated { offset, needed } => {
                write!(
                    f,
                    "the debug block ended at {offset}, needing {needed} more"
                )
            }
            Self::Count { what, value } => {
                write!(f, "the debug block's {what} count of {value} does not fit")
            }
            Self::LengthMismatch { stored, found } => {
                write!(f, "the block says {stored} bytes and there are {found}")
            }
            Self::Range { end, length } => {
                write!(f, "a mapping ends at {end}, past a {length}-byte source")
            }
            Self::Source => write!(f, "a source in the block could not be built"),
            Self::TrailingBytes { offset, length } => {
                write!(f, "the debug block has {length} bytes of tail at {offset}")
            }
        }
    }
}

impl core::error::Error for DebugError {}

/// One source a mapping can point into.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugFile {
    name: String,
    text: String,
}

impl DebugFile {
    /// A source file, named and with its text.
    pub fn new(name: String, text: String) -> Self {
        Self { name, text }
    }
    /// The file's name as the compiler was given it.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The source text mappings are offsets into.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// The text's length in bytes, which is also its length in the block.
    pub fn length(&self) -> u32 {
        u32::try_from(self.text.len()).unwrap_or(u32::MAX)
    }
}

/// One run of instructions and the source it came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DebugEntry {
    /// The address in the loaded image the run starts at.
    pub address: u64,
    /// Which embedded source the offsets are into.
    pub source: u32,
    /// The first byte of the run's source range.
    pub offset: u32,
    /// How many bytes the run's source range covers.
    pub length: u32,
}

impl DebugEntry {
    /// The first byte *after* the run's source range.
    pub fn end(&self) -> u32 {
        self.offset.saturating_add(self.length)
    }
}

/// A where-did-the-code-come-from table, with the sources it refers to.
///
/// Entries are kept sorted by address, which is what both directions of the
/// mapping need: a program counter walks *backwards* to the entry at or before it,
/// and a source line walks *forwards* over the entries that name it.
#[derive(Clone, Debug, Default)]
pub struct DebugBlock {
    files: Vec<DebugFile>,
    entries: Vec<DebugEntry>,
    sources: SourceManager,
}

/// Two blocks are equal when they carry the same sources and the same mappings.
///
/// The `SourceManager` is deliberately left out of the comparison: it is built
/// from the files, so it cannot disagree with them, and it holds handles that
/// compare by identity rather than by content. Comparing it would say two blocks
/// with identical contents were different, which is the opposite of useful.
impl PartialEq for DebugBlock {
    fn eq(&self, other: &Self) -> bool {
        self.files == other.files && self.entries == other.entries
    }
}

impl Eq for DebugBlock {}

impl DebugBlock {
    /// An empty block: a program built without debug information.
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            entries: Vec::new(),
            sources: SourceManager::new(),
        }
    }

    /// A block for `files` and `entries`, sorted and validated.
    ///
    /// The sort is done here rather than trusted, because a caller that got the
    /// order wrong would produce a block whose lookups silently return the wrong
    /// line — and every producer of this data is somewhere else in the tree.
    pub fn with_entries(
        files: Vec<DebugFile>,
        mut entries: Vec<DebugEntry>,
    ) -> Result<Self, DebugError> {
        entries.sort_unstable_by_key(|entry| (entry.address, entry.source, entry.offset));
        let mut sources = SourceManager::new();
        for file in &files {
            let length = file.length();
            if length as usize != file.text().len() {
                return Err(DebugError::LengthMismatch {
                    stored: length,
                    found: u32::try_from(file.text().len()).unwrap_or(u32::MAX),
                });
            }
            sources
                .add_file(file.name(), file.text())
                .map_err(|_| DebugError::Source)?;
        }
        for entry in &entries {
            let file = files.get(entry.source as usize).ok_or(DebugError::Range {
                end: entry.source,
                length: u32::try_from(files.len()).unwrap_or(u32::MAX),
            })?;
            if entry.end() > file.length() {
                return Err(DebugError::Range {
                    end: entry.end(),
                    length: file.length(),
                });
            }
        }
        Ok(Self {
            files,
            entries,
            sources,
        })
    }

    /// Whether the block says anything at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The embedded sources.
    pub fn files(&self) -> &[DebugFile] {
        &self.files
    }

    /// The mappings, in address order.
    pub fn entries(&self) -> &[DebugEntry] {
        &self.entries
    }

    /// The source manager the offsets resolve against.
    pub const fn sources(&self) -> &SourceManager {
        &self.sources
    }

    /// The entry whose run of code contains `address`.
    ///
    /// This is the walk backwards: the last entry at or before the address. An
    /// address before the first entry has no answer, and saying so is the point —
    /// a debugger that invented one would be showing a line the program did not
    /// come from.
    pub fn entry_at(&self, address: u64) -> Option<&DebugEntry> {
        let index = self
            .entries
            .partition_point(|entry| entry.address <= address);
        index.checked_sub(1).map(|at| &self.entries[at])
    }

    /// Where `address` was written, with the line it is on.
    pub fn resolve(&self, address: u64) -> Option<SourceLocation<'_>> {
        let entry = self.entry_at(address)?;
        let file = self.files.get(entry.source as usize)?;
        let line = self
            .sources
            .line_column(SourceId::new(entry.source), ByteOffset::new(entry.offset))?;
        Some(SourceLocation {
            name: file.name(),
            line,
            end: ByteOffset::new(entry.end()),
        })
    }

    /// How many lines the file called `name` has, or `None` if the block does not
    /// carry it.
    ///
    /// This is what a frontend needs *before* setting a source breakpoint: a
    /// request for line 400 of a nine-line file is a bug in the request, and the
    /// cheapest honest answer to it is "there are nine".
    pub fn line_count(&self, name: &str) -> Option<u32> {
        let index = self.files.iter().position(|file| file.name() == name)?;
        let id = SourceId::new(u32::try_from(index).ok()?);
        self.sources.file(id).map(|file| file.line_count())
    }

    /// The addresses in the image that came from `name`'s line `line`.
    ///
    /// This is the other direction, and it is what a source-level breakpoint is
    /// made of. A line can hold more than one statement, so the answer is every
    /// entry that *starts* on that line: a breakpoint on a line has to be able to
    /// stop at each piece of code the line was written as, and a frontend that
    /// wants the conventional "first statement on the line" takes the lowest of
    /// these.
    ///
    /// The line is found by resolving each entry's start offset through the file's
    /// own line map, which is the same map that made the offsets. Comparing
    /// offsets against a line *start* instead would have been the same answer
    /// written a second way, and a second way to be wrong.
    ///
    /// An empty answer is a real answer: a line can be a comment, a declaration
    /// with no code, or a branch the backend never emitted.
    pub fn addresses_at_line(&self, name: &str, line: u32) -> Vec<u64> {
        if line == 0 {
            return Vec::new();
        }
        let mut found = Vec::new();
        for (index, file) in self.files.iter().enumerate() {
            if file.name() != name {
                continue;
            }
            let id = SourceId::new(u32::try_from(index).unwrap_or(0));
            if self.sources.line_text(id, line).is_none() {
                // The file does not have that line at all, so nothing can have come
                // from it. A stale breakpoint on a line that was since deleted
                // lands here rather than on whatever now occupies that number.
                return Vec::new();
            }
            for entry in &self.entries {
                if entry.source as usize != index {
                    continue;
                }
                let starts_here = self
                    .sources
                    .line_column(id, ByteOffset::new(entry.offset))
                    .is_some_and(|position| position.line == line);
                if starts_here {
                    found.push(entry.address);
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// How many bytes [`Self::encode`] will produce.
    ///
    /// The caller needs this to reserve before encoding rather than after: an
    /// image that encoded a large table and only then tried to reserve would have
    /// already paid for it.
    pub fn encoded_length(&self) -> usize {
        let file_bytes: usize = self
            .files
            .iter()
            .map(|file| 8 + file.name().len() + file.text().len())
            .sum();
        HEADER_SIZE + file_bytes + self.entries.len() * ENTRY_SIZE
    }

    /// Encodes the block.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&DEBUG_MAGIC);
        put_u16(&mut out, DEBUG_VERSION);
        put_u16(&mut out, 0);
        put_u32(
            &mut out,
            u32::try_from(self.files.len()).unwrap_or(u32::MAX),
        );
        put_u32(
            &mut out,
            u32::try_from(self.entries.len()).unwrap_or(u32::MAX),
        );
        for file in &self.files {
            put_bytes(&mut out, file.name().as_bytes());
            put_bytes(&mut out, file.text().as_bytes());
        }
        for entry in &self.entries {
            put_u64(&mut out, entry.address);
            put_u32(&mut out, entry.source);
            put_u32(&mut out, entry.offset);
            put_u32(&mut out, entry.length);
        }
        out
    }

    /// Decodes a block.
    pub fn decode(bytes: &[u8]) -> Result<Self, DebugError> {
        if bytes.len() < HEADER_SIZE {
            return Err(DebugError::Truncated {
                offset: bytes.len(),
                needed: HEADER_SIZE - bytes.len(),
            });
        }
        if bytes[..8] != DEBUG_MAGIC {
            return Err(DebugError::Magic);
        }
        let version = read_u16(bytes, 8);
        if version != DEBUG_VERSION {
            return Err(DebugError::Version { found: version });
        }
        let file_count = read_u32(bytes, 12) as usize;
        let entry_count = read_u32(bytes, 16) as usize;
        let mut at = HEADER_SIZE;
        let mut files = Vec::new();
        files
            .try_reserve_exact(file_count)
            .map_err(|_| DebugError::Count {
                what: "source",
                value: file_count as u64,
            })?;
        // The counts are checked against the bytes that are actually here before
        // anything is looped over. A file costs at least two length words, and an
        // entry is twenty bytes, so a count larger than the remaining file allows
        // is a block that was truncated or corrupted — and it is caught here
        // rather than by a reader that trusted the count and read past the end,
        // which would both take a long time and produce a table of zeros.
        let remaining = || bytes.len().saturating_sub(at);
        if file_count.saturating_mul(MINIMUM_FILE_SIZE) > remaining() {
            return Err(DebugError::Truncated {
                offset: at,
                needed: file_count.saturating_mul(MINIMUM_FILE_SIZE),
            });
        }
        if entry_count.saturating_mul(ENTRY_SIZE) > remaining() {
            return Err(DebugError::Truncated {
                offset: at,
                needed: entry_count.saturating_mul(ENTRY_SIZE),
            });
        }
        for _ in 0..file_count {
            let name = read_bytes(bytes, &mut at)?;
            let text = read_bytes(bytes, &mut at)?;
            files.push(DebugFile::new(name, text));
        }
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(entry_count)
            .map_err(|_| DebugError::Count {
                what: "mapping",
                value: entry_count as u64,
            })?;
        for _ in 0..entry_count {
            entries.push(DebugEntry {
                address: read_u64(bytes, at),
                source: read_u32(bytes, at + 8),
                offset: read_u32(bytes, at + 12),
                length: read_u32(bytes, at + 16),
            });
            at += ENTRY_SIZE;
        }
        if at != bytes.len() {
            return Err(DebugError::TrailingBytes {
                offset: at,
                length: bytes.len() - at,
            });
        }
        Self::with_entries(files, entries)
    }
}

/// Where a program counter was written, resolved against the embedded source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceLocation<'a> {
    /// The file's name, as the compiler was given it.
    pub name: &'a str,
    /// The line and column the run starts at.
    pub line: LineColumn,
    /// The first byte after the run's source range.
    pub end: ByteOffset,
}

impl<'a> SourceLocation<'a> {
    /// The line, one-based as a person counts.
    pub const fn line_number(&self) -> u32 {
        self.line.line
    }
    /// The column, one-based as a person counts.
    pub const fn column_number(&self) -> u32 {
        self.line.column
    }
}

/// A source file as the block carries it, for a caller that wants to read one.
pub type BlockSourceFile = SourceFile;

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) {
    put_u32(out, u32::try_from(value.len()).unwrap_or(u32::MAX));
    out.extend_from_slice(value);
}

fn read_u16(bytes: &[u8], at: usize) -> u16 {
    let at = at.min(bytes.len().saturating_sub(2));
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    let at = at.min(bytes.len().saturating_sub(4));
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_u64(bytes: &[u8], at: usize) -> u64 {
    let at = at.min(bytes.len().saturating_sub(8));
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(buf)
}

fn read_bytes(bytes: &[u8], at: &mut usize) -> Result<String, DebugError> {
    let length = read_u32(bytes, *at) as usize;
    *at += 4;
    let end = at.checked_add(length).ok_or(DebugError::Truncated {
        offset: *at,
        needed: length,
    })?;
    let slice = bytes.get(*at..end).ok_or(DebugError::Truncated {
        offset: *at,
        needed: length,
    })?;
    *at = end;
    Ok(String::from_utf8_lossy(slice).into_owned())
}
