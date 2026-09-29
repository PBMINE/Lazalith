//! A snapshot layer: a copy-on-write layer a running machine can take and drop.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use lazalith_devices::{
    Backend, BackendError, BackendIdentity, BackendKind, BlockBackend, SECTOR_BYTES,
};

use crate::error::StorageError;

/// A copy-on-write layer over another backend, which can be discarded.
///
/// # Why this is not B5's `CopyOnWriteBlockBackend`
///
/// That one is how a backend is *built*: a machine profile says "copy on write" and the
/// overlay exists from the start. This is what a *running* machine takes: writes go
/// here, the base is untouched, and dropping the layer puts the machine back on the
/// base with every write it made since gone.
///
/// The two differ in one way that matters: a `CopyOnWriteBlockBackend` discards its
/// contents only by being dropped, and nothing can drop it from under a device. A
/// `SnapshotLayer` is a value a host holds, so "go back to before" is a `drop` rather
/// than a rebuild.
///
/// # The `no_std` question
///
/// This needs no filesystem and no `std`, and it is in `lazalith-storage` anyway. The
/// reason is that a snapshot layer is a *storage* concept, and the alternative — moving
/// it into `lazalith-devices` — would put "snapshot" into a `no_std` crate at exactly
/// the point where B5 and B6 had just separated two meanings of the word: a device's own
/// state, and a layer over storage. They are different things with different rules, and
/// the type names say so.
#[derive(Debug)]
pub struct SnapshotLayer {
    identity: BackendIdentity,
    /// The storage this layer sits on.
    base: Box<dyn BlockBackend>,
    /// Sector number → the bytes written into the layer.
    ///
    /// A `BTreeMap` because a layer is *usually* small — that is the point of taking
    /// one — and a bitmap sized for a 64 GiB base would be larger than the layer for any
    /// snapshot shorter than a day.
    written: BTreeMap<u64, Vec<u8>>,
    /// Set once the layer has been discarded, so a use-after-discard is a refusal
    /// rather than a silent read of an empty layer.
    discarded: bool,
}

impl SnapshotLayer {
    /// A layer over `base`.
    ///
    /// The base is required to be **read-only**, and the refusal is
    /// `BackendError::WritableBase` for the same reason B5's overlay has it: a layer
    /// over a writable base is not a snapshot, it is a layer that happens not to write
    /// through today, and nothing would notice until a backend that did.
    pub fn new(base: Box<dyn BlockBackend>) -> Result<Self, StorageError> {
        if base.writable() {
            return Err(StorageError::BadBase {
                detail: "a snapshot layer needs a read-only base, or discarding it would \
                         not discard anything",
            });
        }
        if base.capacity() < SECTOR_BYTES {
            return Err(StorageError::BadBase {
                detail: "the base cannot hold a whole sector, so a layer over it could \
                         never answer a read",
            });
        }
        Ok(Self {
            identity: BackendIdentity::fresh(),
            base,
            written: BTreeMap::new(),
            discarded: false,
        })
    }

    /// The same layer, with a fresh identity.
    ///
    /// **And this is the important one.** A layer that is reused after being discarded
    /// has the same `BackendIdentity` as the machine snapshot that recorded it, so
    /// restoring that snapshot would be accepted and would put a *different* set of
    /// writes in place — which is the one failure B5's identity rule exists to prevent.
    /// Taking a new layer mints a new identity for exactly that reason.
    pub fn renew(mut self) -> Self {
        self.identity = BackendIdentity::fresh();
        self
    }

    /// How many sectors this layer holds.
    ///
    /// The number that makes a snapshot visible: a machine that has run for an hour
    /// holds an hour's worth of writes, not a copy of its disk.
    pub fn len(&self) -> u64 {
        self.written.len() as u64
    }

    /// Whether the layer holds nothing.
    pub fn is_empty(&self) -> bool {
        self.written.is_empty()
    }

    /// Whether a sector is in the layer rather than the base.
    pub fn is_written(&self, sector: u64) -> bool {
        self.written.contains_key(&sector)
    }

    /// The sectors this layer holds, in order.
    ///
    /// `BTreeMap` iterates in key order, so this is sorted without a sort.
    pub fn sector_numbers(&self) -> impl Iterator<Item = u64> + '_ {
        self.written.keys().copied()
    }

    /// Discards every write, and marks the layer unusable.
    ///
    /// **Not a truncation, and not a clear.** The bytes are dropped and the layer refuses
    /// to answer afterwards, so a caller that still holds it gets a refusal rather than
    /// an empty disk that looks like a freshly-created one. Dropping the layer and
    /// replacing it with `base` is the way back; mutating this one is a trap.
    pub fn discard(&mut self) {
        self.written.clear();
        self.discarded = true;
    }

    /// Whether this layer has been discarded.
    pub const fn is_discarded(&self) -> bool {
        self.discarded
    }

    /// The base, for a host that wants to reach what the guest has not written.
    ///
    /// **The host's way in, and the device has no such accessor.** B5's rule is that a
    /// device must not expose its backend, and a layer's base is a backend. This exists
    /// because the host that took the snapshot may legitimately want the base — to hash
    /// it, to compare it, to hand it back.
    pub fn base(&self) -> &dyn BlockBackend {
        self.base.as_ref()
    }

    fn check(&self, sector: u64) -> Result<(), BackendError> {
        if self.discarded {
            return Err(BackendError::Corrupt);
        }
        if sector
            .saturating_mul(SECTOR_BYTES)
            .saturating_add(SECTOR_BYTES)
            > self.capacity()
        {
            return Err(BackendError::OutOfRange {
                sector,
                bytes: SECTOR_BYTES,
                capacity: self.capacity(),
            });
        }
        Ok(())
    }
}

impl Backend for SnapshotLayer {
    fn kind(&self) -> BackendKind {
        BackendKind::SnapshotLayer
    }

    fn identity(&self) -> BackendIdentity {
        self.identity
    }

    fn reset(&mut self) {
        // The layer is the *device's* dirty state, not a cache of something else. A
        // machine reset is not a machine that forgot what it wrote — and QEMU behaves
        // the same way: taking a snapshot makes the old image the backing file, and a
        // reset does not undo that.
    }
}

impl BlockBackend for SnapshotLayer {
    fn capacity(&self) -> u64 {
        self.base.capacity()
    }

    fn writable(&self) -> bool {
        true
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8]) -> Result<(), BackendError> {
        if out.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: out.len(),
            });
        }
        self.check(sector)?;
        if let Some(bytes) = self.written.get(&sector) {
            out.copy_from_slice(bytes);
            return Ok(());
        }
        // The fall-through. Not a copy of the base into the layer — that is what makes
        // a layer the size of the disk — just this sector, on the way to the guest.
        self.base.read_sector(sector, out)
    }

    fn write_sector(&mut self, sector: u64, data: &[u8]) -> Result<(), BackendError> {
        if data.len() as u64 != SECTOR_BYTES {
            return Err(BackendError::WrongSectorSize {
                expected: SECTOR_BYTES as usize,
                found: data.len(),
            });
        }
        self.check(sector)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(SECTOR_BYTES as usize)
            .map_err(|_| BackendError::Corrupt)?;
        bytes.extend_from_slice(data);
        self.written.insert(sector, bytes);
        Ok(())
    }
}

/// Formats a layer's holdings for a diagnostic: which sectors it holds.
///
/// A free function rather than only a `Display`, because a person wants the *summary* —
/// "3 sectors: 0, 1, 4096" — and `Debug` on the layer already prints the map.
pub fn describe(layer: &SnapshotLayer) -> String {
    if layer.is_discarded() {
        return String::from("discarded");
    }
    if layer.is_empty() {
        return String::from("empty");
    }
    // `BTreeMap` iterates in key order, so no sorting is needed here — the sort a first
    // draft of this function did was re-sorting an already-sorted iterator.
    let names: Vec<String> = layer
        .sector_numbers()
        .map(|sector| sector.to_string())
        .collect();
    format!("{} sectors: {}", layer.len(), names.join(", "))
}

impl fmt::Display for SnapshotLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&describe(self))
    }
}
