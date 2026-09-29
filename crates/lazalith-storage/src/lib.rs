//! Host storage backends: raw images, sparse images, and snapshot layers.
//!
//! `binstruction.md` §31 names four things to investigate — raw images, sparse images,
//! copy-on-write, snapshot layers — and says "do not implement all at once" about the
//! guest controllers. Copy-on-write arrived in B5 (`CopyOnWriteBlockBackend`, in
//! `lazalith-devices`, because it needs no host APIs). This crate is the other three,
//! and every one of them needs `std::fs`, which is why they could not be there.
//!
//! # The chain
//!
//! §31 draws:
//!
//! ```text
//! guest block device
//!  ↓
//! controller\/device model
//!  ↓
//! host storage backend
//! ```
//!
//! The first box is B5's `BlockDevice` and the last box is this crate. **The middle box
//! is deliberately empty**, and that is a decision rather than an omission — see
//! [`controllers`].
//!
//! # What lives where, and why
//!
//! | Backend | Kind | Needs `std`? | Why here |
//! | --- | --- | --- | --- |
//! | `RawImageBackend` | `RawImage` | yes | a file *is* the disk |
//! | `SparseImageBackend` | `SparseImage` | yes | a file plus an allocation index |
//! | `SnapshotLayer` | `SnapshotLayer` | no | over any backend |
//! | `CopyOnWriteBlockBackend` | `CopyOnWrite` | **no** | B5, in the device crate |
//!
//! `SnapshotLayer` is in this crate but does not need `std`, which looks inconsistent
//! and is not: it is here because a snapshot layer is a *storage* concept, and the
//! alternative — putting it in `lazalith-devices` — would mean a `no_std` crate
//! knowing what a snapshot is, at a point in the architecture where "snapshot" means
//! two different things (a device's own state, and a layer over storage) that B5 and
//! B6 had just separated on purpose.
//!
//! # Nothing here is guest-visible
//!
//! B5's invariant stands and this crate is where it is most load-bearing: a `File` is
//! a host resource, and no path from a `BlockDevice` reaches one. Every type here
//! implements `BlockBackend`, which has no `DeviceOffset`, no `DataAccess` and no
//! `PhysicalAddress` in its signatures — so a guest cannot name a file, and a device
//! cannot hand one out. `crates/lazalith-cli/tests/architecture.rs` checks it.

use std::fs::OpenOptions;
use std::path::Path;

use lazalith_devices::BackendIdentity;

extern crate alloc;

mod error;
mod raw;
mod snapshot;
mod sparse;

pub use controllers::BlockController;
pub use error::StorageError;
pub use raw::RawImageBackend;
pub use snapshot::{SnapshotLayer, describe};
pub use sparse::SparseImageBackend;

/// The guest controllers §31 names, and which of them exist.
///
/// §31 lists IDE/ATA, VirtIO block and NVMe and says "Do not implement all at once."
/// None of them is built, and this type is the record of that — a list a caller can
/// ask, rather than a paragraph in a document that drifts out of date with the code.
///
/// **The three are not interchangeable and the differences are the point.** A Linux
/// guest wants `virtio-blk` for speed and simplicity, `IDE/ATA` because it is what old
/// code expects to find at 0x1F0, and NVMe because that is what a modern guest probes
/// for. Building all three "eventually" is three device models, three register maps and
/// three sets of guest-visible semantics, and the *guest-visible* parts are what make
/// them a stage each rather than a flag.
pub mod controllers {
    /// A guest block controller §31 names.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub enum BlockController {
        /// IDE\/ATA, at the traditional legacy addresses. For the compatibility
        /// machine and the Linux port, which probes for it by convention.
        IdeAta,
        /// VirtIO block: a paravirtualised device the guest driver talks to over a
        /// shared ring. The simplest of the three for a modern guest.
        VirtIoBlock,
        /// NVMe, for a guest that expects a modern PCIe-attached namespace.
        NvMe,
    }

    impl BlockController {
        /// Whether this build can attach a controller of this kind.
        ///
        /// All three are `false`, and that is a statement rather than a placeholder: a
        /// guest that finds no controller is a guest that falls back to polling, and a
        /// guest that finds a half-built one is a guest that faults. The distinction is
        /// the same one B4 drew for `lza64-at-v1`.
        pub const fn is_buildable(self) -> bool {
            false
        }

        /// The name, for a diagnostic and for a profile that names one.
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::IdeAta => "ide/ata",
                Self::VirtIoBlock => "virtio-blk",
                Self::NvMe => "nvme",
            }
        }
    }

    impl core::fmt::Display for BlockController {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str(self.as_str())
        }
    }
}
// Opens a file for reading and writing, creating it if it does not exist.
///
/// **Every backend here goes through this**, so the access mode and the create
/// behaviour are in one function rather than three. A backend that opened its file
/// read-only would fail at the first write with an error from the OS that says
/// something about permissions rather than about the backend's contract.
pub(crate) fn open_image(path: &Path) -> Result<std::fs::File, StorageError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|source| StorageError::Open {
            path: path.to_path_buf(),
            source,
        })
}

/// A fresh backend identity, minted the way B5 mints one.
///
/// Shared by every backend here so a diagnostic can tell them apart and so no two of
/// them can accidentally compare equal — a snapshot restored onto the wrong image is
/// precisely what an identity is for, and an identity scheme that handed two backends
/// the same token would defeat it.
pub(crate) fn identity() -> BackendIdentity {
    BackendIdentity::fresh()
}
