//! A raw disk image: a file that *is* the disk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use lazalith_devices::{
    Backend, BackendError, BackendIdentity, BackendKind, BlockBackend, SECTOR_BYTES,
};

use crate::error::StorageError;
use crate::identity;

/// The largest image this host will make, in bytes.
///
/// `u32::MAX` sectors' worth, so a sector number fits in a `u32` for a caller that
/// wants one. Not `u64::MAX`: an image that large cannot be created on any filesystem
/// this runs on, and accepting the request and then failing at `set_len` would report
/// a filesystem error rather than saying the size was out of range.
pub const MAX_IMAGE_BYTES: u64 = u32::MAX as u64 * SECTOR_BYTES;

/// A disk image backed by a file, one byte per byte.
///
/// **Raw means raw.** No header, no allocation index, no magic. The file's byte *i* is
/// the disk's byte *i*. That is what makes it useful — it can be `dd`-ed, replaced by
/// hand, mounted by the host, or have a partition table written into its first sector —
/// and it is also what makes it expensive: a 64 GiB image with three sectors written
/// occupies 64 GiB on the host. [`SparseImageBackend`](crate::SparseImageBackend) is the
/// answer to that, and the two are different enough to be different kinds.
#[derive(Debug)]
pub struct RawImageBackend {
    file: File,
    path: PathBuf,
    identity: BackendIdentity,
    capacity: u64,
    writable: bool,
}

impl RawImageBackend {
    /// Opens, and if necessary creates, a raw image of exactly `bytes`.
    ///
    /// An existing file of a *different* length is **not** resized silently. Growing is
    /// fine and is what a caller creating an image wants; shrinking would discard data
    /// nobody asked it to, so a longer existing file is refused with
    /// [`StorageError::NotWholeSectors`] if it is not sector-aligned, and with a
    /// refusal naming the length otherwise.
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
        let file = crate::open_image(path)?;
        let existing = file
            .metadata()
            .map_err(|source| StorageError::Stat {
                path: path.to_path_buf(),
                source,
            })?
            .len();
        if existing > bytes {
            return Err(StorageError::NotWholeSectors { bytes: existing });
        }
        if existing < bytes {
            // Growing zero-fills, so a new image reads as an empty disk rather than as
            // whatever was in the file before.
            file.set_len(bytes).map_err(|source| StorageError::Resize {
                path: path.to_path_buf(),
                source,
            })?;
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
            identity: identity(),
            capacity: bytes,
            writable: true,
        })
    }

    /// The same image, refusing writes.
    ///
    /// How a base is made for a [`SnapshotLayer`](crate::SnapshotLayer) or B5's
    /// `CopyOnWriteBlockBackend`, and it is a *view* of the same file rather than a
    /// copy — so a write that somehow reached it would be refused by the same bytes it
    /// would otherwise have modified.
    pub fn read_only(mut self) -> Self {
        self.writable = false;
        self
    }

    /// The path this image is at, for a host diagnostic.
    ///
    /// **Host-only, and deliberately on the backend rather than the device.** B5's rule
    /// is that a *device* must not expose its backend; this is the host's own handle on
    /// its own file, and a guest has no path to a backend to ask.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The offset of a sector in the file.
    fn offset(&self, sector: u64) -> Result<u64, BackendError> {
        sector
            .checked_mul(SECTOR_BYTES)
            .filter(|offset| offset.saturating_add(SECTOR_BYTES) <= self.capacity)
            .ok_or(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity,
            })
    }
}

impl Backend for RawImageBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::RawImage
    }

    fn identity(&self) -> BackendIdentity {
        self.identity
    }

    fn reset(&mut self) {
        // Nothing. The file *is* the storage; a reset does not empty a disk, which
        // would be a machine that boots into a machine with no disk.
    }
}

impl BlockBackend for RawImageBackend {
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
        let offset = self.offset(sector)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(out))
            .map_err(|_| BackendError::Unavailable)?;
        Ok(())
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
        let offset = self.offset(sector)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(data))
            .and_then(|_| self.file.flush())
            .map_err(|_| BackendError::Unavailable)?;
        Ok(())
    }
}
