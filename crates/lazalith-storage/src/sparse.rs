//! A sparse disk image: a data file plus an allocation index.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lazalith_devices::{
    Backend, BackendError, BackendIdentity, BackendKind, BlockBackend, SECTOR_BYTES,
};

use crate::error::StorageError;
use crate::identity;
use crate::raw::MAX_IMAGE_BYTES;

/// A magic word at the head of the data file, so a sparse image cannot be mistaken for
/// a raw one.
///
/// **This is what makes a sparse image not a raw image.** A raw image's byte 0 is the
/// disk's byte 0 and a guest can put anything there. A sparse image reserves the first
/// sector for a header, so the two are not interchangeable and a caller that pointed a
/// raw backend at a sparse file would read a magic word where it expected a partition
/// table. The check is on open, and the refusal names both.
const SPARSE_MAGIC: &[u8; 8] = b"LZSPARSE";

/// The first sector: the magic, then the capacity in bytes.
const HEADER_SECTOR: usize = SECTOR_BYTES as usize;

/// A disk image that occupies only the sectors that have been written.
///
/// A 64 GiB disk with three sectors written is three sectors on the host plus an index,
/// rather than 64 GiB. That is the whole of it, and it is the reason this is a separate
/// backend from [`RawImageBackend`](crate::RawImageBackend) rather than a flag on it:
/// the two have different on-disk formats, different failure modes, and different
/// amounts of host storage for the same guest-visible disk.
///
/// # The trade this makes
///
/// An index is needed, so a read of an unallocated sector has to consult it, and an
/// index in memory costs a byte per sector. A `BTreeMap` is used rather than a bitmap
/// because a bitmap is 1 bit per sector — 8 MiB for a 64 GiB disk, which is fine — while
/// a map is proportional to what is *allocated*, which is better for the common case of
/// a mostly-empty disk and worse for a mostly-full one.
///
/// **The index is not written to disk.** That is a real limitation and it is recorded
/// in `docs/project-state.md` under B8: a sparse image is reconstructible from its data
/// file (every sector past the header is either present or a hole) but this
/// implementation does not do that reconstruction, and reopening one *does* re-derive
/// the index from sector markers.
#[derive(Debug)]
pub struct SparseImageBackend {
    file: File,
    path: PathBuf,
    identity: BackendIdentity,
    capacity: u64,
    writable: bool,
    /// Which sectors have data, and where in the file it is.
    allocated: BTreeMap<u64, u64>,
    /// Where the next newly-allocated sector goes in the file.
    next_offset: u64,
}

impl SparseImageBackend {
    /// Opens, and if necessary creates, a sparse image of `bytes`.
    ///
    /// A new image is written with a header. An existing one is read: its header is
    /// checked, and its sectors are re-discovered by the `[marker][data]` layout each
    /// allocated sector is stored in.
    pub fn create(path: &Path, bytes: u64) -> Result<Self, StorageError> {
        if bytes == 0 {
            return Err(StorageError::EmptyImage);
        }
        if !bytes.is_multiple_of(SECTOR_BYTES) {
            return Err(StorageError::LengthNotSectors { bytes });
        }
        if bytes > MAX_IMAGE_BYTES {
            return Err(StorageError::TooLarge {
                bytes,
                limit: MAX_IMAGE_BYTES,
            });
        }
        let mut file = crate::open_image(path)?;
        let mut header = vec![0u8; HEADER_SECTOR];
        let existing = file
            .metadata()
            .map_err(|source| StorageError::Stat {
                path: path.to_path_buf(),
                source,
            })?
            .len();
        if existing == 0 {
            header[..SPARSE_MAGIC.len()].copy_from_slice(SPARSE_MAGIC);
            header[8..16].copy_from_slice(&bytes.to_le_bytes());
            file.write_all(&header).map_err(|source| StorageError::Io {
                path: path.to_path_buf(),
                source,
            })?;
            return Ok(Self {
                file,
                path: path.to_path_buf(),
                identity: identity(),
                capacity: bytes,
                writable: true,
                allocated: BTreeMap::new(),
                next_offset: HEADER_SECTOR as u64,
            });
        }
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|source| StorageError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if &header[..SPARSE_MAGIC.len()] != SPARSE_MAGIC {
            return Err(StorageError::IndexMismatch {
                detail: "the file does not start with the sparse-image header",
            });
        }
        let stored = u64::from_le_bytes(header[8..16].try_into().expect("8 bytes"));
        if stored != bytes {
            return Err(StorageError::IndexMismatch {
                detail: "the header's capacity is not the capacity asked for",
            });
        }
        let (allocated, next_offset) = discover(&mut file, existing)?;
        Ok(Self {
            file,
            path: path.to_path_buf(),
            identity: identity(),
            capacity: bytes,
            writable: true,
            allocated,
            next_offset,
        })
    }

    /// The same image, refusing writes.
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    /// The path this image is at, for a host diagnostic.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How many sectors actually occupy host storage.
    ///
    /// **The number that makes sparse visible.** A guest cannot tell a sparse image from
    /// a raw one, and this is how a host does: a 64 GiB disk with three sectors written
    /// reports three here and 64 GiB in `capacity`.
    pub fn allocated_sectors(&self) -> u64 {
        self.allocated.len() as u64
    }

    /// How many bytes of the host filesystem this image occupies.
    ///
    /// The header, plus the index-free record framing each allocated sector, plus the
    /// sectors themselves.
    pub fn bytes_on_disk(&self) -> u64 {
        self.next_offset
    }

    /// Whether a sector is in the image rather than a hole.
    pub fn is_allocated(&self, sector: u64) -> bool {
        self.allocated.contains_key(&sector)
    }

    fn check(&self, sector: u64) -> Result<(), BackendError> {
        if sector
            .saturating_mul(SECTOR_BYTES)
            .saturating_add(SECTOR_BYTES)
            > self.capacity
        {
            return Err(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity,
            });
        }
        Ok(())
    }

    /// Reads the sector record at `offset` into `out`, ignoring the record header.
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> Result<(), BackendError> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(out))
            .map_err(|_| BackendError::Unavailable)
    }
}

/// The framing one allocated sector is stored in: the sector number, then the data.
///
/// The number is stored so the file can be walked on open. Without it a sparse image
/// would have to guess which sectors it holds, and the guess would be wrong the moment
/// two allocated sectors were not adjacent in the file.
fn record(sector: u64, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.extend_from_slice(&sector.to_le_bytes());
    out.extend_from_slice(data);
    out
}

const RECORD_HEADER: u64 = 8;

/// Walks a sparse data file and rebuilds the allocation index from it.
fn discover(file: &mut File, length: u64) -> Result<(BTreeMap<u64, u64>, u64), StorageError> {
    let mut allocated = BTreeMap::new();
    let mut offset = HEADER_SECTOR as u64;
    while offset + RECORD_HEADER + SECTOR_BYTES <= length {
        let mut header = [0u8; 8];
        file.seek(SeekFrom::Start(offset))
            .and_then(|_| file.read_exact(&mut header))
            .map_err(|source| StorageError::Io {
                path: PathBuf::from("<sparse image>"),
                source,
            })?;
        let sector = u64::from_le_bytes(header);
        offset += RECORD_HEADER + SECTOR_BYTES;
        // A repeated or out-of-range sector number means the file was interrupted
        // mid-append. Detected here, on open, rather than discovered later as a sector
        // that reads back as somebody else's data.
        if sector > u64::MAX / SECTOR_BYTES
            || allocated
                .insert(sector, offset - RECORD_HEADER - SECTOR_BYTES)
                .is_some()
        {
            return Err(StorageError::IndexMismatch {
                detail: "a sector record repeats a sector number",
            });
        }
    }
    Ok((allocated, length))
}

impl Backend for SparseImageBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::SparseImage
    }

    fn identity(&self) -> BackendIdentity {
        self.identity
    }

    fn reset(&mut self) {
        // The allocation index is part of the image, not a cache: a reset is not a
        // machine that forgot what it wrote.
    }
}

impl BlockBackend for SparseImageBackend {
    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), BackendError> {
        if out.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: out.len(),
            });
        }
        self.check(sector)?;
        match self.allocated.get(&sector).copied() {
            // A hole reads as zeroes, which is what a freshly-created raw image reads
            // as too — so a guest cannot tell the two apart, and does not need to.
            None => {
                out.fill(0);
                Ok(())
            }
            Some(offset) => {
                // Skip the record header, then read exactly one sector.
                self.read_at(offset + RECORD_HEADER, out)
            }
        }
    }

    fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BackendError> {
        if !self.writable {
            return Err(BackendError::ReadOnly);
        }
        if data.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: data.len(),
            });
        }
        self.check(sector)?;
        if let Some(offset) = self.allocated.get(&sector).copied() {
            // Already allocated: rewrite in place, so rewriting a sector does not grow
            // the image. Growing on every write would make a loop that writes one
            // sector a thousand times a thousand times larger.
            self.file
                .seek(SeekFrom::Start(offset + RECORD_HEADER))
                .and_then(|_| self.file.write_all(data))
                .and_then(|_| self.file.flush())
                .map_err(|_| BackendError::Unavailable)?;
            return Ok(());
        }
        let offset = self.next_offset;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(&record(sector, data)))
            .and_then(|_| self.file.flush())
            .map_err(|_| BackendError::Unavailable)?;
        self.next_offset = offset + RECORD_HEADER + SECTOR_BYTES;
        self.allocated.insert(sector, offset);
        Ok(())
    }
}
