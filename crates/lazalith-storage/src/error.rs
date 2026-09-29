//! What a host storage backend refuses, when the refusal was not the device's.

use core::fmt;
use std::path::PathBuf;

/// A failure reaching or shaping the host's storage.
///
/// **Separate from [`lazalith_devices::BackendError`] on purpose.** A `BackendError` is
/// something a *guest* can be told about, and every variant of it is reachable from a
/// register access. Everything here needs a filesystem, a path, or an OS error string —
/// a guest that got one of these would be learning about the host's filesystem, which
/// is exactly the leak B5's boundary exists to prevent.
///
/// So a `StorageError` is converted into a [`lazalith_devices::BackendError`] at the
/// point a guest can see it, and the host detail is kept for the host.
#[derive(Debug)]
pub enum StorageError {
    /// The image file could not be opened.
    Open {
        /// Which file.
        path: PathBuf,
        /// What the OS said.
        source: std::io::Error,
    },
    /// The file's size could not be read.
    Stat {
        /// Which file.
        path: PathBuf,
        /// What the OS said.
        source: std::io::Error,
    },
    /// The file could not be resized to the image's length.
    Resize {
        /// Which file.
        path: PathBuf,
        /// What the OS said.
        source: std::io::Error,
    },
    /// A read or write failed.
    Io {
        /// Which file.
        path: PathBuf,
        /// What the OS said.
        source: std::io::Error,
    },
    /// The image's length is not a whole number of sectors.
    ///
    /// **Refused rather than rounded.** A 513-byte image is not one sector with a byte
    /// left over; rounding it down would silently discard a byte a caller wrote, and
    /// rounding up would invent one. The image is what it is, and a caller who meant a
    /// sector-aligned image has to say so.
    NotWholeSectors {
        /// The file's length in bytes.
        bytes: u64,
    },
    /// A length that is not a whole number of sectors was asked for.
    LengthNotSectors {
        /// The length asked for, in bytes.
        bytes: u64,
    },
    /// A length of zero was asked for.
    EmptyImage,
    /// The image is larger than this host can address.
    TooLarge {
        /// The length asked for, in bytes.
        bytes: u64,
        /// The largest this host accepts.
        limit: u64,
    },
    /// The allocation index and the data file disagree.
    ///
    /// A sparse image is two files, and a host that is interrupted between writing a
    /// sector and marking it allocated leaves them inconsistent. That is detected on
    /// open rather than discovered later as a sector that reads back as zeroes.
    IndexMismatch {
        /// What was inconsistent, in words.
        detail: &'static str,
    },
    /// A snapshot layer was asked for a base it cannot sit on.
    BadBase {
        /// What was wrong with the base.
        detail: &'static str,
    },
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { path, source } => {
                write!(
                    f,
                    "the image at {} could not be opened: {source}",
                    path.display()
                )
            }
            Self::Stat { path, source } => {
                write!(
                    f,
                    "the image at {} could not be measured: {source}",
                    path.display()
                )
            }
            Self::Resize { path, source } => write!(
                f,
                "the image at {} could not be resized: {source}",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(f, "the image at {} failed: {source}", path.display())
            }
            Self::NotWholeSectors { bytes } => {
                write!(
                    f,
                    "a {bytes} byte image is not a whole number of 512-byte sectors"
                )
            }
            Self::LengthNotSectors { bytes } => {
                write!(f, "{bytes} bytes is not a whole number of 512-byte sectors")
            }
            Self::EmptyImage => f.write_str("an image of zero bytes holds nothing"),
            Self::TooLarge { bytes, limit } => {
                write!(
                    f,
                    "{bytes} bytes is larger than this host's {limit} byte limit"
                )
            }
            Self::IndexMismatch { detail } => {
                write!(f, "a sparse image's index and data disagree: {detail}")
            }
            Self::BadBase { detail } => {
                write!(f, "a snapshot layer needs a usable base: {detail}")
            }
        }
    }
}

impl core::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Open { source, .. }
            | Self::Stat { source, .. }
            | Self::Resize { source, .. }
            | Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<StorageError> for lazalith_devices::BackendError {
    /// Flattens a host failure into the two facts a guest can be told about.
    ///
    /// Everything becomes `OutOfRange` or `ReadOnly` because those are the only
    /// storage facts in `BackendError` that do not name a path, a file or an errno —
    /// and a guest error message that contained `/home/someone/disk.img` would hand the
    /// guest the host's directory layout. The host detail stays in the
    /// `StorageError`, for the host.
    fn from(source: StorageError) -> Self {
        match source {
            StorageError::NotWholeSectors { .. }
            | StorageError::LengthNotSectors { .. }
            | StorageError::EmptyImage
            | StorageError::TooLarge { .. }
            | StorageError::IndexMismatch { .. } => Self::Corrupt,
            StorageError::Open { .. }
            | StorageError::Stat { .. }
            | StorageError::Resize { .. }
            | StorageError::Io { .. }
            | StorageError::BadBase { .. } => Self::Unavailable,
        }
    }
}
