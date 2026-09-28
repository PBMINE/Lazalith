//! Turning the bytes at a path into something a process can run.
//!
//! # The gap this closes
//!
//! `SpawnProcess` names a *path*. Until this module existed, nothing turned that
//! path into bytes: `LazalithKernel::start_image` took an `LzxImage` by value, and
//! the ABI layer validated a path it then dropped. `docs/lazen-packages.md` records
//! that as the design's first driver, and this is the small, local answer the
//! roadmap asks for in step 89 — *local* package support, no registry.
//!
//! # The rule
//!
//! ```text
//! a path names a package   when the bytes at it are a .lza
//! a path names an image    when the bytes at it are a .lzx
//! a path naming neither    is NotFound, whatever it contains
//! ```
//!
//! Deciding by **content** rather than by **name** is the whole design, for three
//! reasons that are all about what already exists:
//!
//! - `FileMetadata` has no "this is an executable" flag today, so a reader deciding
//!   by name would be inventing a convention in the loader that nothing else in the
//!   system agrees with.
//! - A user can rename a file. Deciding by name makes `install` a naming convention
//!   and `run` a privilege question, and neither is a question the kernel should be
//!   answering.
//! - A package and a bare executable are then interchangeable, so a system with no
//!   package manager still runs programs.
//!
//! # What it does not do
//!
//! It does not enforce permissions. A package's `[permissions]` is a *declaration*
//! and this module reads it and hands it to the caller; nothing here gates a syscall
//! on it, because the syscall layer has no capability gate to gate with. That is
//! step 91's work and the design's gap table says so.

use alloc::{boxed::Box, vec::Vec};

use crate::{
    LZX_MAGIC,
    lza::{LzaError, LzaPackage},
    lzx::{LzxError, LzxImage},
};

/// What a path turned out to name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Resolved {
    /// A `.lza` package, and the executable inside it.
    Package(Box<LzaPackage>),
    /// A bare `.lzx` image.
    Image(Box<LzxImage>),
}

impl Resolved {
    /// The image, whichever kind it was.
    ///
    /// The one operation a caller wants, because a package and an executable are
    /// interchangeable to everything downstream of this point — which is the property
    /// that makes a package a *container* rather than a second executable format.
    pub fn into_image(self) -> Result<LzxImage, ResolveError> {
        match self {
            Self::Package(package) => package.image().map_err(ResolveError::Package),
            Self::Image(image) => Ok(*image),
        }
    }

    /// Whether this was a package or a bare image.
    pub const fn is_package(&self) -> bool {
        matches!(self, Self::Package(_))
    }
}

/// Why a path could not be resolved.
///
/// Not `Clone` or `Eq` for the same reason `LzaError` is not: the two inner errors
/// are `Debug` and not `Clone`, and a resolution failure is a sentence a reader sees
/// rather than a value a program branches on.
#[derive(Debug)]
pub enum ResolveError {
    /// The bytes are neither a package nor an executable.
    NotExecutable {
        /// How many bytes there were, for a report.
        bytes: usize,
    },
    /// The bytes looked like a package and were not one.
    Package(LzaError),
    /// The bytes looked like an image and were not one.
    Image(LzxError),
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotExecutable { bytes } => {
                write!(f, "{bytes} bytes are neither a package nor an executable")
            }
            Self::Package(error) => write!(f, "these bytes claim to be a package: {error}"),
            Self::Image(error) => write!(f, "these bytes claim to be an executable: {error}"),
        }
    }
}

/// What a file's first eight bytes say it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Container {
    /// Nothing this system loads.
    Unknown,
    /// An application package.
    Package,
    /// A bare executable.
    Image,
}

impl Container {
    /// What the bytes claim to be, from their magic alone.
    ///
    /// This reads eight bytes and nothing else, and it is deliberately not a parser:
    /// the point is to decide *which* reader to hand the bytes to, and a reader that
    /// had already parsed the file would be redundant with the one it is choosing
    /// between. A file whose magic is right and whose contents are not is that
    /// reader's problem to report, and it does.
    pub fn of(bytes: &[u8]) -> Self {
        if bytes.len() < 8 {
            Self::Unknown
        } else if bytes[0..8] == crate::lza::LZA_MAGIC {
            Self::Package
        } else if bytes[0..8] == LZX_MAGIC {
            Self::Image
        } else {
            Self::Unknown
        }
    }
}

/// Turns the bytes at a path into an image.
///
/// The one function the spawn path needs, and the whole of the design's resolution
/// rule: a name is never consulted, and a file that is neither is `NotExecutable`
/// rather than a guess.
pub fn resolve(bytes: &[u8]) -> Result<Resolved, ResolveError> {
    match Container::of(bytes) {
        Container::Package => LzaPackage::from_bytes(bytes)
            .map(|package| Resolved::Package(Box::new(package)))
            .map_err(ResolveError::Package),
        Container::Image => LzxImage::from_bytes(bytes)
            .map(|image| Resolved::Image(Box::new(image)))
            .map_err(ResolveError::Image),
        Container::Unknown => Err(ResolveError::NotExecutable { bytes: bytes.len() }),
    }
}

/// The bytes a caller would install, given a path's contents and a name.
///
/// `install` is not a step-89 deliverable — where a package lands is the *system's*
/// decision, and the design says a file that says where it goes has to be rewritten
/// on every move. What step 89 owes is the *content*: given the bytes at a path, hand
/// back either an image that is a valid image or a package that is a valid package,
/// without consulting the path. This is that, as a function a caller can use.
pub fn installable(bytes: &[u8]) -> Result<Vec<u8>, ResolveError> {
    match resolve(bytes)? {
        Resolved::Package(package) => package.to_bytes().map_err(ResolveError::Package),
        Resolved::Image(image) => image.to_bytes().map_err(ResolveError::Image),
    }
}
