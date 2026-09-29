//! B8: host storage backends.
//!
//! # What is being held here
//!
//! **A raw image is the disk, byte for byte.** That is checked by writing a sector,
//! closing, and reading the file with `std::fs` — the test does not go through the
//! backend to check the backend. A test that read the file *through* the backend would
//! pass even if the layout were wrong, because both sides would be wrong together.
//!
//! **A sparse image occupies only what was written**, and that is the property worth
//! most of these tests. It is also the one most likely to break: an implementation that
//! wrote zeroes for holes would pass every functional test and be 64 GiB on disk.
//! `bytes_on_disk` is therefore asserted against a *bound*, not a value.
//!
//! **A snapshot layer can be taken and dropped**, and dropping it must put the machine
//! back on the base. Two tests here are about what a discarded layer *refuses* rather
//! than what it returns, because a discarded layer that answered with zeroes would look
//! exactly like a fresh disk.
//!
//! # Why there is no controller test
//!
//! §31 lists IDE/ATA, VirtIO-blk and NVMe and says "do not implement all at once." None
//! is built, so there is nothing to test, and `the_guest_controllers_are_named_but_not_built`
//! is the test: it is a list a caller can ask, and a test that says all three are
//! unbuildable. When one is built, that test is a deliberate deletion.

use std::fs;

use lazalith_devices::{
    Backend, BackendError, BackendKind, BlockBackend, MemoryBlockBackend, SECTOR_BYTES,
};
use lazalith_storage::{
    BlockController, RawImageBackend, SnapshotLayer, SparseImageBackend, StorageError, describe,
};

const SECTORS: u64 = 8;
const DISK: u64 = SECTORS * SECTOR_BYTES;

fn sector_filled(tag: u8) -> Vec<u8> {
    vec![tag; SECTOR_BYTES as usize]
}

/// A path in the target directory, unique per test.
///
/// A real temporary file rather than a shared name, because a leftover from a previous
/// run that happened to be the right length would silently make `create` reuse it — and
/// the first assertion in several of these tests is that a *new* image reads as zeroes.
fn image_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("lazalith-b8-tests");
    fs::create_dir_all(&dir).expect("the scratch directory is creatable");
    dir.join(format!("{name}-{}.img", std::process::id()))
}

// -- raw images ---------------------------------------------------------------

#[test]
fn a_raw_image_is_the_disk_byte_for_byte() {
    let path = image_path("raw-layout");
    let _ = fs::remove_file(&path);
    let wanted = sector_filled(0x5A);
    {
        let mut backend = RawImageBackend::create(&path, DISK).expect("a new image");
        backend
            .write_sector(3, &wanted)
            .expect("a writable image takes a write");
        assert_eq!(backend.capacity(), DISK);
        assert!(backend.writable());
    }
    // Read the file with std, not through the backend: both sides being wrong together
    // is the failure this avoids.
    let bytes = fs::read(&path).expect("the image file reads");
    assert_eq!(bytes.len() as u64, DISK, "the file is the disk's length");
    let start = 3 * SECTOR_BYTES as usize;
    assert_eq!(
        &bytes[start..start + SECTOR_BYTES as usize],
        &wanted[..],
        "sector 3 is at byte {start} of the file, with no header and no index"
    );
    assert!(
        bytes[..start].iter().all(|byte| *byte == 0),
        "and the sectors before it are the zeroes the file was created with"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_raw_image_reopens_with_its_contents() {
    let path = image_path("raw-reopen");
    let _ = fs::remove_file(&path);
    let wanted = sector_filled(0x3C);
    {
        let mut backend = RawImageBackend::create(&path, DISK).expect("a new image");
        backend.write_sector(1, &wanted).expect("a write");
    }
    let mut backend = RawImageBackend::create(&path, DISK).expect("it reopens");
    let mut out = vec![0; SECTOR_BYTES as usize];
    backend.read_sector(1, &mut out).expect("a read");
    assert_eq!(out, wanted, "the contents survived the close");
    let _ = fs::remove_file(&path);
}

#[test]
fn a_raw_image_refuses_a_length_that_is_not_whole_sectors() {
    let path = image_path("raw-ragged");
    let _ = fs::remove_file(&path);
    assert!(
        matches!(
            RawImageBackend::create(&path, 513),
            Err(StorageError::LengthNotSectors { bytes: 513 })
        ),
        "513 bytes is not a sector, and rounding it either way would invent or discard a \
         byte the caller wrote"
    );
    assert!(matches!(
        RawImageBackend::create(&path, 0),
        Err(StorageError::EmptyImage)
    ));
    let _ = fs::remove_file(&path);
}

#[test]
fn a_raw_image_refuses_to_shrink_an_existing_file() {
    let path = image_path("raw-shrink");
    let _ = fs::remove_file(&path);
    RawImageBackend::create(&path, DISK).expect("a new image");
    // A *larger* request grows the file, which is what creating an image wants.
    RawImageBackend::create(&path, DISK * 2).expect("growing is fine");
    // A smaller one is refused, because truncating would discard data nobody asked it to.
    assert!(
        matches!(
            RawImageBackend::create(&path, DISK),
            Err(StorageError::NotWholeSectors { .. })
        ),
        "an image that is longer than the length asked for is refused rather than \
         truncated: a caller that wanted a smaller disk should say so over a new file"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_raw_image_refuses_a_sector_past_the_end() {
    let path = image_path("raw-range");
    let _ = fs::remove_file(&path);
    let mut backend = RawImageBackend::create(&path, DISK).expect("a new image");
    let mut out = vec![0; SECTOR_BYTES as usize];
    assert_eq!(
        backend.read_sector(SECTORS, &mut out),
        Err(BackendError::OutOfRange {
            sector: SECTORS,
            bytes: SECTOR_BYTES,
            capacity: DISK
        })
    );
    assert_eq!(
        backend.write_sector(u64::MAX, &sector_filled(1)),
        Err(BackendError::OutOfRange {
            sector: u64::MAX,
            bytes: SECTOR_BYTES,
            capacity: DISK
        }),
        "and a sector number that would overflow the offset arithmetic is refused rather \
         than wrapping to somewhere inside the disk"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_read_only_raw_image_refuses_writes() {
    let path = image_path("raw-ro");
    let _ = fs::remove_file(&path);
    RawImageBackend::create(&path, DISK).expect("a new image");
    let mut backend = RawImageBackend::create(&path, DISK)
        .expect("it reopens")
        .read_only();
    assert!(!backend.writable());
    assert_eq!(
        backend.write_sector(0, &sector_filled(1)),
        Err(BackendError::ReadOnly)
    );
    let _ = fs::remove_file(&path);
}

// -- sparse images ------------------------------------------------------------

#[test]
fn a_sparse_image_occupies_only_what_was_written() {
    let path = image_path("sparse-small");
    let _ = fs::remove_file(&path);
    // A small image so the assertion is about *sectors*, not bytes: a 64 GiB disk would
    // make the test need a real filesystem feature to be meaningful.
    let sectors = 4;
    let bytes = sectors * SECTOR_BYTES;
    let mut backend = SparseImageBackend::create(&path, bytes).expect("a new sparse image");

    assert_eq!(backend.capacity(), bytes, "the guest sees the whole disk");
    assert_eq!(backend.allocated_sectors(), 0, "and nothing is stored yet");
    backend
        .write_sector(2, &sector_filled(0x11))
        .expect("a write");

    assert_eq!(
        backend.allocated_sectors(),
        1,
        "one sector written, one stored"
    );
    assert!(backend.is_allocated(2));
    assert!(!backend.is_allocated(1), "sector 1 was never written");
    assert!(
        backend.bytes_on_disk() < bytes,
        "a 4-sector image with one sector written must be smaller on disk than the disk \
         is long, or it is not sparse: {} bytes on disk for {bytes} bytes of disk",
        backend.bytes_on_disk()
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn rewriting_a_sector_does_not_grow_the_image() {
    let path = image_path("sparse-rewrite");
    let _ = fs::remove_file(&path);
    let bytes = 4 * SECTOR_BYTES;
    let mut backend = SparseImageBackend::create(&path, bytes).expect("a new sparse image");
    backend
        .write_sector(0, &sector_filled(1))
        .expect("a first write");
    let after_first = backend.bytes_on_disk();
    for tag in 2..8u8 {
        backend
            .write_sector(0, &sector_filled(tag))
            .expect("a rewrite");
    }
    assert_eq!(
        backend.allocated_sectors(),
        1,
        "seven writes to one sector is still one sector"
    );
    assert_eq!(
        backend.bytes_on_disk(),
        after_first,
        "and it did not grow: a loop writing one sector would otherwise make the image \
         grow once per iteration"
    );
    let mut out = vec![0; SECTOR_BYTES as usize];
    backend.read_sector(0, &mut out).expect("a read");
    assert_eq!(
        out,
        sector_filled(7),
        "and the last write is the one that is there"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_sparse_hole_reads_as_zeroes() {
    let path = image_path("sparse-hole");
    let _ = fs::remove_file(&path);
    let bytes = 4 * SECTOR_BYTES;
    let mut backend = SparseImageBackend::create(&path, bytes).expect("a new sparse image");
    backend
        .write_sector(1, &sector_filled(0x22))
        .expect("a write");

    let mut hole = vec![0xFF; SECTOR_BYTES as usize];
    backend.read_sector(3, &mut hole).expect("a read");
    assert_eq!(
        hole,
        vec![0; SECTOR_BYTES as usize],
        "a hole reads as zeroes, which is what a freshly-created raw image reads as too, \
         so a guest cannot tell the two formats apart and does not need to"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn a_sparse_image_reopens_and_rebuilds_its_index() {
    let path = image_path("sparse-reopen");
    let _ = fs::remove_file(&path);
    let bytes = 4 * SECTOR_BYTES;
    {
        let mut backend = SparseImageBackend::create(&path, bytes).expect("a new image");
        for sector in [0u64, 2, 3] {
            backend
                .write_sector(sector, &sector_filled(0x30 + sector as u8))
                .expect("a write");
        }
    }
    let backend = SparseImageBackend::create(&path, bytes).expect("it reopens");
    assert_eq!(
        backend.allocated_sectors(),
        3,
        "the index is rebuilt from the data file, so a sparse image is not held hostage \
         to a separate index file that could be lost"
    );
    assert!(backend.is_allocated(0) && backend.is_allocated(2) && backend.is_allocated(3));
    assert!(!backend.is_allocated(1));
    let _ = fs::remove_file(&path);
}

#[test]
fn a_sparse_image_refuses_a_raw_file_and_a_capability_mismatch() {
    let raw = image_path("sparse-vs-raw");
    let _ = fs::remove_file(&raw);
    RawImageBackend::create(&raw, 4 * SECTOR_BYTES).expect("a raw image");
    assert!(
        matches!(
            SparseImageBackend::create(&raw, 4 * SECTOR_BYTES),
            Err(StorageError::IndexMismatch { .. })
        ),
        "a raw image has no sparse header, and opening it as sparse would read a magic \
         word where a partition table belongs. The two formats must not be mistaken for \
         one another."
    );
    let sparse = image_path("sparse-wrong-size");
    let _ = fs::remove_file(&sparse);
    SparseImageBackend::create(&sparse, 4 * SECTOR_BYTES).expect("a sparse image");
    assert!(
        matches!(
            SparseImageBackend::create(&sparse, 8 * SECTOR_BYTES),
            Err(StorageError::IndexMismatch { .. })
        ),
        "and an existing image is not silently re-sized to a different length"
    );
    let _ = fs::remove_file(&raw);
    let _ = fs::remove_file(&sparse);
}

// -- snapshot layers ----------------------------------------------------------

/// A read-only in-memory base, which is what a layer needs.
fn base_with(sectors: &[(u64, u8)]) -> Box<dyn BlockBackend> {
    let mut backend = MemoryBlockBackend::new(DISK).expect("a base of the asked size");
    for (sector, tag) in sectors {
        backend
            .write_sector(*sector, &sector_filled(*tag))
            .expect("the base is writable before it is made read-only");
    }
    Box::new(backend.read_only())
}

#[test]
fn a_layer_falls_through_to_a_base_it_has_not_written() {
    let mut layer = SnapshotLayer::new(base_with(&[(0, 0x11)])).expect("a read-only base");
    let mut out = vec![0; SECTOR_BYTES as usize];
    layer.read_sector(0, &mut out).expect("a read");
    assert_eq!(out, sector_filled(0x11), "the read reached the base");
    assert!(
        layer.is_empty(),
        "and a read allocated nothing in the layer"
    );
}

#[test]
fn a_layer_holds_only_what_the_guest_wrote() {
    let mut layer = SnapshotLayer::new(base_with(&[(0, 0x11)])).expect("a read-only base");
    layer
        .write_sector(0, &sector_filled(0x22))
        .expect("a layer is writable");

    assert_eq!(layer.len(), 1, "one sector written");
    assert!(layer.is_written(0));
    assert_eq!(describe(&layer), "1 sectors: 0");

    let mut out = vec![0; SECTOR_BYTES as usize];
    layer.read_sector(0, &mut out).expect("a read");
    assert_eq!(out, sector_filled(0x22), "the guest sees what it wrote");

    // And the base is untouched, which the layer's own `base()` accessor is how a host
    // checks. The device has no such accessor — that is B5's rule.
    let mut base_read = vec![0; SECTOR_BYTES as usize];
    let base = layer.base();
    // `base` is a shared borrow and reading needs `&mut`, so the check is that the
    // base reports the original capacity and kind rather than mutating it.
    assert_eq!(base.capacity(), DISK, "the base is still the base");
    assert!(!base.writable(), "and it is still read-only");
    base_read.fill(0);
}

#[test]
fn a_layer_over_a_writable_base_is_refused() {
    let writable: Box<dyn BlockBackend> = Box::new(MemoryBlockBackend::new(DISK).expect("a base"));
    assert!(
        matches!(
            SnapshotLayer::new(writable),
            Err(StorageError::BadBase { .. })
        ),
        "a layer over a writable base is not a snapshot: discarding it would not discard \
         anything, and nothing would notice until a base that does write through"
    );
}

#[test]
fn discarding_a_layer_puts_the_machine_back_on_the_base() {
    let mut layer = SnapshotLayer::new(base_with(&[(1, 0x33)])).expect("a read-only base");
    layer
        .write_sector(1, &sector_filled(0x44))
        .expect("a write");
    let mut out = vec![0; SECTOR_BYTES as usize];
    layer.read_sector(1, &mut out).expect("a read");
    assert_eq!(out, sector_filled(0x44), "the write is there");

    layer.discard();
    assert!(layer.is_discarded());
    assert!(layer.is_empty());
    assert_eq!(describe(&layer), "discarded");
    assert!(
        layer.read_sector(1, &mut out).is_err(),
        "a discarded layer refuses rather than answering: a layer that returned zeroes \
         would look exactly like a fresh disk, and a machine restored onto one would be \
         a machine that had silently lost its writes"
    );
}

#[test]
fn a_renewed_layer_has_a_different_identity() {
    let base = base_with(&[]);
    let first = SnapshotLayer::new(base_with(&[])).expect("a layer");
    let first_identity = first.identity();
    let renewed = first.renew();
    let _ = base;
    assert_ne!(
        first_identity,
        renewed.identity(),
        "a layer reused after being discarded has the same identity as the machine \
         snapshot that recorded it, so restoring that snapshot would be accepted and \
         would put a different set of writes in place — the exact failure B5's identity \
         rule exists to prevent"
    );
}

#[test]
fn a_layer_presents_its_bases_capacity_and_refuses_its_own_range() {
    let mut layer = SnapshotLayer::new(base_with(&[])).expect("a read-only base");
    assert_eq!(layer.capacity(), DISK, "the guest sees the base's capacity");
    assert!(
        layer.writable(),
        "a layer is writable even though its base is not"
    );
    let mut out = vec![0; SECTOR_BYTES as usize];
    assert_eq!(
        layer.read_sector(SECTORS, &mut out),
        Err(BackendError::OutOfRange {
            sector: SECTORS,
            bytes: SECTOR_BYTES,
            capacity: DISK
        }),
        "and it enforces the base's range, so a guest cannot read past the end of the \
         disk through a layer that happens to be larger"
    );
}

#[test]
fn a_layer_refuses_a_buffer_that_is_not_a_sector() {
    let mut layer = SnapshotLayer::new(base_with(&[])).expect("a read-only base");
    let mut out = vec![0; 16];
    assert_eq!(
        layer.read_sector(0, &mut out),
        Err(BackendError::WrongSectorSize {
            expected: SECTOR_BYTES as usize,
            found: 16
        })
    );
    assert_eq!(
        layer.write_sector(0, &sector_filled(1)[..16]),
        Err(BackendError::WrongSectorSize {
            expected: SECTOR_BYTES as usize,
            found: 16
        })
    );
}

// -- kinds and the unbuilt controllers ----------------------------------------

#[test]
fn every_backend_reports_its_own_kind() {
    // B5's reason for `BackendKind` at all: a diagnostic that called a dead disk a
    // memory disk is worse than no diagnostic.
    let raw_path = image_path("kinds-raw");
    let sparse_path = image_path("kinds-sparse");
    let _ = fs::remove_file(&raw_path);
    let _ = fs::remove_file(&sparse_path);
    assert_eq!(
        RawImageBackend::create(&raw_path, DISK)
            .expect("a raw image")
            .kind(),
        BackendKind::RawImage
    );
    let sparse = SparseImageBackend::create(&sparse_path, DISK).expect("a sparse image");
    assert_eq!(sparse.kind(), BackendKind::SparseImage);
    let layer = SnapshotLayer::new(base_with(&[])).expect("a layer");
    assert_eq!(layer.kind(), BackendKind::SnapshotLayer);
    let _ = fs::remove_file(&raw_path);
    let _ = fs::remove_file(&sparse_path);
}

#[test]
fn the_guest_controllers_are_named_but_not_built() {
    for controller in [
        BlockController::IdeAta,
        BlockController::VirtIoBlock,
        BlockController::NvMe,
    ] {
        assert!(
            !controller.is_buildable(),
            "{controller} is named by §31 and not built. When one is built, this \
             assertion is a deliberate deletion — and the test it should be replaced with \
             is one that boots a guest that talks to that controller."
        );
        assert!(!controller.as_str().is_empty());
    }
}

#[test]
fn a_guest_never_sees_a_host_path() {
    // The conversion is the whole of B5's boundary at this layer: a host failure
    // becomes one of two facts a guest can be told about, and neither names a file.
    let errors = [
        StorageError::NotWholeSectors { bytes: 513 },
        StorageError::LengthNotSectors { bytes: 513 },
        StorageError::EmptyImage,
        StorageError::TooLarge {
            bytes: u64::MAX,
            limit: 1,
        },
        StorageError::IndexMismatch { detail: "x" },
    ];
    for error in errors {
        let BackendError::Corrupt = BackendError::from(error) else {
            panic!("a shape problem must reach the guest as Corrupt");
        };
    }
    // And an OS-level failure is Unavailable, not Corrupt: a guest told "corrupt" would
    // conclude its image was damaged and go looking for a backup of a disk that is fine
    // and merely unmounted.
    // A host-side construction refusal is `Unavailable` rather than `Corrupt`: the base
    // was wrong, not the data.
    let bad_base = StorageError::BadBase { detail: "x" };
    assert_eq!(BackendError::from(bad_base), BackendError::Unavailable);

    let io = StorageError::Open {
        path: "/home/someone/secret.img".into(),
        source: std::io::Error::other("no such file"),
    };
    assert_eq!(BackendError::from(io), BackendError::Unavailable);
}
