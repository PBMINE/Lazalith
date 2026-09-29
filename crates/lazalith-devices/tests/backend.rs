//! B5: the backend boundary, tested from both sides.
//!
//! Two things are being checked here, and they are different in kind.
//!
//! The first is behaviour: does a memory backend store a sector, does an overlay
//! fall through to its base, does an absent backend say so. That is ordinary
//! testing.
//!
//! The second is the *boundary itself* — that a backend is not a device, that the
//! backend trait has no guest vocabulary in its signatures, and that the device has
//! no door back to the storage. That is checked by reading this repository's own
//! source, because the boundary is a fact about the code's shape and no runtime test
//! can observe the absence of a method.
//!
//! The source tests are the only ones here that would pass on a rewrite that
//! quietly put a `fn backend()` on the device. They are the point.

use std::sync::OnceLock;

use lazalith_devices::{
    AbsentBlockBackend, BLOCK_REGISTER_BYTES, BLOCK_REGISTER_CAPACITY, BLOCK_REGISTER_COMMAND,
    BLOCK_REGISTER_DATA, BLOCK_REGISTER_REMAINING, BLOCK_REGISTER_SECTOR, BLOCK_REGISTER_STATUS,
    BLOCK_STATUS_BUSY, BLOCK_STATUS_FAILED, BLOCK_STATUS_READABLE, BLOCK_STATUS_WRITABLE, Backend,
    BackendError, BackendKind, BlockBackend, BlockDevice, BlockError, COMMAND_READ, COMMAND_WRITE,
    CopyOnWriteBlockBackend, Device, DeviceError, DeviceId, DeviceOffset, MemoryBlockBackend,
    SECTOR_BYTES,
};
use lazalith_isa::DataSize;

const QUAD: DataSize = DataSize::Double;
const SECTORS: u64 = 8;
const DISK: u64 = SECTORS * SECTOR_BYTES;

/// A sector whose every byte is identifiable, so a test can say which sector it got.
fn sector_filled(tag: u8) -> Vec<u8> {
    vec![tag; SECTOR_BYTES as usize]
}

fn memory_disk(sectors: u64) -> Box<dyn BlockBackend> {
    Box::new(MemoryBlockBackend::new(sectors * SECTOR_BYTES).expect("a disk of the asked size"))
}

/// Asserts that an operation was refused, with this particular block reason.
///
/// [`DeviceError`] is not `PartialEq` — it carries a `TryReserveError` and a
/// `MemoryFault` — so a block refusal cannot be compared as a value. It is unwrapped
/// to the [`BlockError`] it carries and compared there, which is also what makes these
/// tests assert the *reason* rather than merely that something went wrong.
#[track_caller]
fn block_refusal<T: std::fmt::Debug>(
    result: Result<T, DeviceError>,
    expected: BlockError,
    why: &str,
) {
    match result {
        Err(DeviceError::Block(found)) => assert_eq!(found, expected, "{why}"),
        other => panic!("expected a refusal carrying {expected:?}, got {other:?}: {why}"),
    }
}

/// `block_refusal`, with the explanation optional.
macro_rules! assert_block {
    ($result:expr, $expected:expr) => {
        block_refusal($result, $expected, "")
    };
    ($result:expr, $expected:expr, $why:expr) => {
        block_refusal($result, $expected, $why)
    };
}

// -- the boundary: the shapes, read off the source ----------------------------

/// This crate's own source, read once.
///
/// Located relative to the test file rather than the working directory, so the test
/// does not depend on being run from the workspace root.
fn source(relative: &str) -> &'static str {
    static CACHE: OnceLock<std::collections::BTreeMap<String, String>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        ["backend.rs", "storage.rs"]
            .into_iter()
            .map(|name| {
                let path = root.join("src").join(name);
                let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    panic!("the boundary test needs to read {}: {e}", path.display())
                });
                (name.to_string(), text)
            })
            .collect()
    });
    &cache[relative]
}

#[test]
fn a_backend_is_not_a_device() {
    let source = source("backend.rs");
    assert!(
        !source.contains("impl Device for"),
        "a backend must not be a device: a device is a guest window, a backend is \
         the host resource behind one, and one type cannot be both without the \
         guest reaching the storage"
    );
}

#[test]
fn the_backend_trait_has_no_guest_vocabulary() {
    let source = source("backend.rs");
    let start = source
        .find("pub trait Backend")
        .expect("the Backend trait is the thing being checked");
    let signatures: &str = &source[start..];
    for forbidden in [
        "DataAccess",
        "PhysicalAddress",
        "DeviceOffset",
        "Privilege",
        "CycleCount",
    ] {
        assert!(
            !signatures.contains(forbidden),
            "the backend trait mentions {forbidden}: a backend call names storage, so a \
             signature that takes guest vocabulary is a way for the two address spaces \
             to be confused for one"
        );
    }
}

#[test]
fn the_device_has_no_way_back_to_its_storage() {
    let source = source("storage.rs");
    // A diagnostic *kind* is allowed: `BlockDevice::backend_kind` returns one, and it
    // reaches no resource. What is forbidden is a door — anything that hands back a
    // handle to the storage itself.
    for forbidden in [
        "fn backend(",
        "fn storage(",
        "-> &dyn BlockBackend",
        "-> Box<dyn BlockBackend",
        "-> &dyn Backend",
    ] {
        assert!(
            !source.contains(forbidden),
            "the block device exposes {forbidden}: a getter for the backend is a door, and \
             the boundary is arranged so that a host resource cannot become a guest \
             interface one refactor after nobody is looking"
        );
    }
}

// -- a memory backend ---------------------------------------------------------

#[test]
fn a_memory_backend_stores_and_returns_a_sector() {
    let mut backend = MemoryBlockBackend::new(DISK).expect("a disk of the asked size");
    assert_eq!(backend.capacity(), DISK);
    assert!(backend.writable());

    let wanted = sector_filled(0xA5);
    backend
        .write_sector(3, &wanted)
        .expect("a writable disk takes a write");

    let mut read_back = vec![0u8; SECTOR_BYTES as usize];
    backend
        .read_sector(3, &mut read_back)
        .expect("a sector that was written can be read");
    assert_eq!(read_back, wanted);
}

#[test]
fn a_memory_backend_refuses_a_sector_it_does_not_hold() {
    let mut backend = MemoryBlockBackend::new(DISK).expect("a disk of the asked size");
    let mut out = vec![0u8; SECTOR_BYTES as usize];
    assert_eq!(
        backend.read_sector(SECTORS, &mut out),
        Err(BackendError::OutOfRange {
            sector: SECTORS,
            bytes: SECTOR_BYTES,
            capacity: DISK,
        }),
        "one sector past the end is out of range, and the error names the capacity so a \
         caller can tell a bad sector from a bad disk"
    );
}

#[test]
fn a_memory_backend_refuses_a_buffer_that_is_not_a_sector() {
    let mut backend = MemoryBlockBackend::new(DISK).expect("a disk of the asked size");
    let mut out = vec![0u8; 16];
    assert_eq!(
        backend.read_sector(0, &mut out),
        Err(BackendError::WrongSectorSize {
            expected: SECTOR_BYTES as usize,
            found: 16,
        }),
        "a sector is 512 bytes and a caller passing 16 has a bug the backend reports \
         rather than reads 16 bytes of a sector"
    );
}

#[test]
fn a_read_only_backend_refuses_writes() {
    let mut backend = MemoryBlockBackend::new(DISK)
        .expect("a disk of the asked size")
        .read_only();
    assert!(!backend.writable());
    assert_eq!(
        backend.write_sector(0, &sector_filled(1)),
        Err(BackendError::ReadOnly)
    );
}

#[test]
fn a_zero_sized_backend_is_refused() {
    assert!(
        MemoryBlockBackend::new(0).is_err(),
        "a backend holding nothing is a backend every access to which faults, and a \
         caller who asked for that has made a mistake worth naming"
    );
}

// -- copy on write ------------------------------------------------------------

#[test]
fn an_overlay_falls_through_to_a_base_it_has_not_written() {
    let base = MemoryBlockBackend::from_bytes(&sector_filled(0x11))
        .expect("a one-sector image")
        .read_only();
    let mut overlay =
        CopyOnWriteBlockBackend::new(Box::new(base)).expect("a read-only base is a base");

    let mut out = vec![0u8; SECTOR_BYTES as usize];
    overlay
        .read_sector(0, &mut out)
        .expect("an unwritten sector is the base's to answer");
    assert_eq!(
        out,
        sector_filled(0x11),
        "the read reached the base, not zeroes"
    );
    assert_eq!(
        overlay.allocated_sectors(),
        0,
        "a read that fell through allocated nothing: copying the base into the overlay \
         is what makes an overlay the size of the disk"
    );
}

#[test]
fn an_overlay_holds_only_what_was_written() {
    let base = MemoryBlockBackend::new(DISK)
        .expect("a disk of the asked size")
        .read_only();
    let base_identity = base.identity();
    let mut overlay = CopyOnWriteBlockBackend::new(Box::new(base)).expect("a read-only base");

    overlay
        .write_sector(5, &sector_filled(0x22))
        .expect("an overlay is writable");

    assert_eq!(overlay.allocated_sectors(), 1, "one sector was written");
    assert!(overlay.is_allocated(5));
    assert!(!overlay.is_allocated(4), "sector 4 was not written");
    assert_eq!(
        overlay.base().identity(),
        base_identity,
        "the base is still the base, and the overlay did not replace it"
    );
}

#[test]
fn an_overlay_does_not_modify_its_base() {
    let base = MemoryBlockBackend::from_bytes(&sector_filled(0x33))
        .expect("a one-sector image")
        .read_only();
    let mut overlay = CopyOnWriteBlockBackend::new(Box::new(base)).expect("a read-only base");

    // The base is read-only, so "the overlay did not write through" is enforced by the
    // base itself: a write-through would have been *refused* by the same buffer it
    // would otherwise have modified, and would have failed the whole write. The
    // observable proof is therefore that the write succeeded.
    let written = overlay.write_sector(0, &sector_filled(0x44)).is_ok();
    assert!(
        written,
        "the write did not reach the base, or the base would have refused it"
    );

    let mut out = vec![0u8; SECTOR_BYTES as usize];
    overlay
        .read_sector(0, &mut out)
        .expect("the written sector reads back");
    assert_eq!(out, sector_filled(0x44), "the guest sees what it wrote");
    assert_eq!(
        overlay.allocated_sectors(),
        1,
        "and it is in the overlay, not elsewhere"
    );
}

#[test]
fn an_overlay_over_a_writable_base_is_refused() {
    let base = MemoryBlockBackend::new(DISK).expect("a disk of the asked size");
    assert_eq!(
        CopyOnWriteBlockBackend::new(Box::new(base)).unwrap_err(),
        BackendError::WritableBase,
        "an overlay over a writable base is a stack of layers that happens not to write \
         through today, and nothing would notice until a future backend that does"
    );
}

// -- an absent backend --------------------------------------------------------

#[test]
fn an_absent_backend_refuses_everything_and_says_which_device_asked() {
    let id = DeviceId::new(7);
    let mut backend = AbsentBlockBackend::new(id);
    assert_eq!(backend.missing_for(), id);
    assert_eq!(backend.kind(), BackendKind::Absent);

    let mut out = vec![0u8; SECTOR_BYTES as usize];
    assert!(backend.read_sector(0, &mut out).is_err());
    assert!(backend.write_sector(0, &sector_filled(0)).is_err());
    // Not "returns zeroes": a machine with a dead disk that reads zeroes has a
    // guest that will write those zeroes somewhere.
}

// -- identity -----------------------------------------------------------------

#[test]
fn two_backends_have_two_identities() {
    let one = MemoryBlockBackend::new(DISK).expect("a disk");
    let other = MemoryBlockBackend::new(DISK).expect("a disk of the same size");
    assert_ne!(
        one.identity(),
        other.identity(),
        "two disks of the same size are still two disks, and a snapshot that cannot tell \
         them apart would restore onto the wrong one"
    );
}

// -- the device ---------------------------------------------------------------

fn device_over(disk: Box<dyn BlockBackend>) -> BlockDevice {
    BlockDevice::new(disk)
}

fn read_register(device: &mut BlockDevice, offset: DeviceOffset) -> u64 {
    device.read(offset, QUAD).expect("a register read")
}

fn write_register(device: &mut BlockDevice, offset: DeviceOffset, value: u64) {
    device.write(offset, QUAD, value).expect("a register write")
}

#[test]
fn a_block_device_reports_its_storage_through_registers() {
    let mut device = device_over(memory_disk(SECTORS));
    assert_eq!(device.address_len(), BLOCK_REGISTER_BYTES);
    assert_eq!(read_register(&mut device, BLOCK_REGISTER_CAPACITY), SECTORS);
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_STATUS),
        BLOCK_STATUS_READABLE | BLOCK_STATUS_WRITABLE,
        "a writable disk reports itself readable and writable, and nothing else"
    );
}

#[test]
fn a_read_only_disk_is_not_reported_as_writable() {
    let base = MemoryBlockBackend::new(DISK).expect("a disk").read_only();
    let mut device = device_over(Box::new(base));
    let status = read_register(&mut device, BLOCK_REGISTER_STATUS);
    assert!(status & BLOCK_STATUS_READABLE != 0, "it is still readable");
    assert!(
        status & BLOCK_STATUS_WRITABLE == 0,
        "a read-only disk must not report the writable bit, or a guest would attempt a \
         write that could only fail"
    );
}

#[test]
fn reading_a_sector_moves_512_bytes_through_the_data_port() {
    let image = sector_filled(0x5A);
    let mut disk = MemoryBlockBackend::from_bytes(&image).expect("a one-sector image");
    disk.write_sector(0, &image).expect("the write");
    let mut device = device_over(Box::new(disk));

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_STATUS) & BLOCK_STATUS_BUSY,
        BLOCK_STATUS_BUSY,
        "a command with 512 bytes still to move is busy, so a program can tell"
    );

    let mut collected = Vec::new();
    while read_register(&mut device, BLOCK_REGISTER_REMAINING) > 0 {
        collected.extend_from_slice(&read_register(&mut device, BLOCK_REGISTER_DATA).to_le_bytes());
    }
    assert_eq!(collected, image, "the sector came back byte for byte");
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_STATUS) & BLOCK_STATUS_BUSY,
        0,
        "and the device is idle once the last byte is taken"
    );
}

#[test]
fn writing_a_sector_hands_it_to_the_storage_when_the_last_byte_arrives() {
    let mut device = device_over(memory_disk(SECTORS));
    let written = sector_filled(0x77);

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 2);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_WRITE);
    let mut offset = 0usize;
    while offset < written.len() {
        let mut word = [0u8; 8];
        word.copy_from_slice(&written[offset..offset + 8]);
        write_register(&mut device, BLOCK_REGISTER_DATA, u64::from_le_bytes(word));
        offset += 8;
    }
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_STATUS) & BLOCK_STATUS_FAILED,
        0,
        "the write succeeded"
    );

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 2);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    let mut read_back = Vec::new();
    while read_register(&mut device, BLOCK_REGISTER_REMAINING) > 0 {
        read_back.extend_from_slice(&read_register(&mut device, BLOCK_REGISTER_DATA).to_le_bytes());
    }
    assert_eq!(
        read_back, written,
        "the sector the guest wrote is the sector it reads"
    );
}

#[test]
fn a_data_port_access_past_the_end_of_the_sector_is_refused_rather_than_padded() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);

    let mut moved = 0;
    while read_register(&mut device, BLOCK_REGISTER_REMAINING) > 0 {
        read_register(&mut device, BLOCK_REGISTER_DATA);
        moved += 1;
    }
    assert_eq!(moved, (SECTOR_BYTES / 8) as usize);
    assert_block!(
        device.read(BLOCK_REGISTER_DATA, QUAD),
        BlockError::NoTransfer,
        "the sector is fully moved, so the port has nothing to give — a padded read \
         would hand the guest bytes that were never stored"
    );
}

#[test]
fn reading_the_data_port_during_a_write_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_WRITE);
    write_register(&mut device, BLOCK_REGISTER_DATA, 0x11);

    assert_block!(
        device.read(BLOCK_REGISTER_DATA, QUAD),
        BlockError::NoTransfer,
        "reading the port mid-write would consume it and turn the write into a read, \
         losing the bytes the guest had already given with no fault raised anywhere"
    );

    // And the write is still a write: the remaining bytes still land in the sector.
    let mut offset = 8usize;
    while offset < SECTOR_BYTES as usize {
        write_register(&mut device, BLOCK_REGISTER_DATA, 0x22);
        offset += 8;
    }
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_STATUS) & BLOCK_STATUS_FAILED,
        0,
        "the write still completed"
    );

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    let first = read_register(&mut device, BLOCK_REGISTER_DATA);
    let second = read_register(&mut device, BLOCK_REGISTER_DATA);
    assert_eq!(
        first, 0x11,
        "the word written before the refused read survived it"
    );
    assert_eq!(second, 0x22, "and so did the word after");
}

#[test]
fn a_data_port_with_no_command_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    assert_block!(
        device.read(BLOCK_REGISTER_DATA, QUAD),
        BlockError::NoTransfer
    );
    assert_block!(
        device.write(BLOCK_REGISTER_DATA, QUAD, 7),
        BlockError::NoTransfer
    );
}

#[test]
fn writing_during_a_read_is_refused_rather_than_silently_losing_data() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    assert!(
        matches!(
            device.write(BLOCK_REGISTER_DATA, QUAD, 1),
            Err(DeviceError::WriteUnsupported)
        ),
        "a guest that thought it was writing during a read would lose those bytes"
    );
}

#[test]
fn a_command_issued_mid_transfer_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    assert_block!(
        device.write(BLOCK_REGISTER_COMMAND, QUAD, COMMAND_WRITE),
        BlockError::Busy,
        "the first transfer still owns the data port"
    );
}

#[test]
fn an_unknown_command_is_refused_and_named() {
    let mut device = device_over(memory_disk(SECTORS));
    assert_block!(
        device.write(BLOCK_REGISTER_COMMAND, QUAD, 0xDEAD),
        BlockError::UnknownCommand(0xDEAD)
    );
}

#[test]
fn a_sector_past_the_end_of_the_disk_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, SECTORS);
    assert_block!(
        device.write(BLOCK_REGISTER_COMMAND, QUAD, COMMAND_READ),
        BlockError::OutOfRange {
            sector: SECTORS,
            sectors: SECTORS,
        }
    );
}

#[test]
fn a_write_to_a_read_only_disk_is_refused() {
    let base = MemoryBlockBackend::new(DISK).expect("a disk").read_only();
    let mut device = device_over(Box::new(base));
    assert_block!(
        device.write(BLOCK_REGISTER_COMMAND, QUAD, COMMAND_WRITE),
        BlockError::NotWritable
    );
}

#[test]
fn a_register_access_of_the_wrong_shape_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    assert!(
        matches!(
            device.read(BLOCK_REGISTER_CAPACITY, DataSize::Byte),
            Err(DeviceError::UnsupportedSize(DataSize::Byte))
        ),
        "the register window is 8-byte registers and a byte access is a program bug, \
         not a request to read part of one"
    );
    assert!(
        matches!(
            device.read(DeviceOffset::new(1), QUAD),
            Err(DeviceError::InvalidRange { offset, .. }) if offset == DeviceOffset::new(1)
        ),
        "an unaligned register offset is refused rather than rounded to the nearest one"
    );
}

// -- peek ---------------------------------------------------------------------

#[test]
fn peeking_the_data_port_reports_the_next_word_without_moving_the_transfer() {
    let image = sector_filled(0x9C);
    let mut disk = MemoryBlockBackend::from_bytes(&image).expect("a one-sector image");
    disk.write_sector(0, &image).expect("the write");
    let mut device = device_over(Box::new(disk));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);

    let before = read_register(&mut device, BLOCK_REGISTER_REMAINING);
    let mut peeked = [0u8; 8];
    device
        .peek(BLOCK_REGISTER_DATA, &mut peeked)
        .expect("peeking the data port is allowed mid-transfer");
    assert_eq!(
        u64::from_le_bytes(peeked),
        u64::from_le_bytes([0x9C; 8]),
        "the first word of a sector filled with 0x9C"
    );
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_REMAINING),
        before,
        "a peek advanced the transfer: this device has a counted transfer, and a hidden \
         advance would move bytes no guest asked to move"
    );
}

// -- snapshots ----------------------------------------------------------------

#[test]
fn a_snapshot_carries_the_device_and_not_the_storage() {
    let device = device_over(memory_disk(1024));
    let snapshot = device.snapshot();
    assert!(
        snapshot.len() < SECTOR_BYTES as usize,
        "a {} byte snapshot of a {DISK} byte disk must not be holding the disk",
        snapshot.len()
    );
}

#[test]
fn a_snapshot_round_trips_the_device_state() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 6);
    let saved = device.snapshot();

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 1);
    device
        .restore(&saved)
        .expect("the same device takes its own snapshot back");
    assert_eq!(
        read_register(&mut device, BLOCK_REGISTER_SECTOR),
        6,
        "the snapshot restored the selected sector"
    );
}

#[test]
fn a_snapshot_is_refused_by_a_different_disk() {
    let first = device_over(memory_disk(SECTORS));
    let saved = first.snapshot();

    // A second disk of the same size, which is the case that matters: a restore that
    // compared capacities would accept this, and the machine would come up reading
    // someone else's disk.
    let mut other = device_over(memory_disk(SECTORS));
    match other.restore(&saved) {
        Err(DeviceError::Block(BlockError::DifferentBackend { device, snapshot })) => {
            assert_ne!(
                device, snapshot,
                "the refusal names two different disks, which is the whole reason it exists"
            );
        }
        other => panic!("expected a different-backend refusal, got {other:?}"),
    }
}

#[test]
fn a_snapshot_taken_mid_transfer_is_refused_on_restore() {
    let mut device = device_over(memory_disk(SECTORS));
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 0);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    read_register(&mut device, BLOCK_REGISTER_DATA);

    let saved = device.snapshot();
    // The device itself is mid-transfer, so its own restore refuses: restoring into a
    // half-moved sector leaves a data port mid-sector with nobody to finish it.
    assert_block!(device.restore(&saved), BlockError::TransferInFlight);

    // And on a fresh device, the snapshot says it was mid-transfer and is still
    // refused — the record is faithful, and the restore is the strict half.
    let mut fresh = device_over(memory_disk(SECTORS));
    assert_block!(fresh.restore(&saved), BlockError::TransferInFlight);
}

#[test]
fn a_snapshot_of_another_shape_is_refused() {
    let mut device = device_over(memory_disk(SECTORS));
    assert!(device.restore(&[0u8; 3]).is_err());
    assert!(device.restore(&[0u8; 512]).is_err());
}

// -- reset --------------------------------------------------------------------

#[test]
fn a_reset_clears_the_device_but_not_the_disk() {
    let mut disk = MemoryBlockBackend::new(DISK).expect("a disk");
    let written = sector_filled(0x66);
    disk.write_sector(4, &written).expect("the write");
    let mut device = device_over(Box::new(disk));

    write_register(&mut device, BLOCK_REGISTER_SECTOR, 7);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    device.reset();

    assert_eq!(read_register(&mut device, BLOCK_REGISTER_SECTOR), 0);
    assert_eq!(read_register(&mut device, BLOCK_REGISTER_REMAINING), 0);
    write_register(&mut device, BLOCK_REGISTER_SECTOR, 4);
    write_register(&mut device, BLOCK_REGISTER_COMMAND, COMMAND_READ);
    let first = read_register(&mut device, BLOCK_REGISTER_DATA);
    assert_eq!(
        first,
        u64::from_le_bytes([0x66; 8]),
        "a reset is not a machine that forgot its disk"
    );
}
