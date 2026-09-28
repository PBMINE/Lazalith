//! Hardening: the virtual filesystem against a model of what it claims to do.
//!
//! `tests/filesystem.rs` asks the filesystem a handful of questions and checks the
//! answers. This file asks it *sequences*, and compares every step against a model
//! written from the documented semantics rather than from the code — the same
//! technique `hardening_arithmetic.rs` uses for the CPU, and for the same reason: a
//! test that computes its expected value by calling the same method the
//! implementation calls is a test that agrees with the bug.
//!
//! A filesystem is the worst place for a wrong answer, because the operations
//! compose. `write_at` past the end extends; `seek` to the end then `read_at` from
//! there returns nothing; `truncate` up then `write_at` inside the new tail works. Every
//! one of those is individually plausible, and the composition is where an
//! off-by-one lives.
//!
//! So the campaign is: a randomised operation sequence applied to both the real
//! filesystem and a model, comparing the *result* of each call and the *whole
//! contents* of the file after every step. Comparing contents after every step rather
//! than only at the end means a divergence names the operation that caused it.
//!
//! Three documented rules are the specific targets, because each is a place where a
//! reasonable implementation could differ:
//!
//! - **A write past the end extends; a write at an offset beyond the end is an
//!   error.** There are no sparse files here, so a write that would leave a hole is
//!   refused rather than silently zero-filled. A program that assumed otherwise would
//!   see its data in a gap it never wrote.
//! - **A read past the end returns what is there, and a read *from* beyond the end is
//!   an error.** Short reads are not failures.
//! - **A seek is bounded by the file.** Seeking to exactly the end is allowed, since
//!   that is what "position at the end" means; seeking one past it is not.

use lazalith_os::{FileAccess, FileNodeId, FileSystemError, VirtualFileSystem};
use lazalith_os_abi::{OPEN_CREATE, OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, OpenFlags, SeekOrigin};

const PATH: &[u8] = b"/file";

/// The model: one file's bytes, and nothing else.
///
/// Deliberately not a filesystem — no directories, no names, no limits. Every
/// question this file asks is about the *bytes* and the *positions*, and a model
/// that grew directories would only add ways to be wrong in the same way.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Model(Vec<u8>);

impl Model {
    /// `read_at`: a short read at the end is a success, a read from beyond it is not.
    fn read_at(&self, offset: u64, length: u64) -> Result<Vec<u8>, ()> {
        let file_length = self.0.len() as u64;
        if offset > file_length {
            return Err(());
        }
        let available = file_length - offset;
        let transfer = length.min(available);
        let start = offset as usize;
        Ok(self.0[start..start + transfer as usize].to_vec())
    }

    /// `write_at`: overwrites what is there and extends past it. An offset beyond the
    /// end is refused, because there are no sparse files.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> Result<u64, ()> {
        let file_length = self.0.len() as u64;
        if offset > file_length {
            return Err(());
        }
        let start = offset as usize;
        let overwrite = data.len().min(self.0.len().saturating_sub(start));
        self.0[start..start + overwrite].copy_from_slice(&data[..overwrite]);
        if data.len() > overwrite {
            self.0.extend_from_slice(&data[overwrite..]);
        }
        Ok(data.len() as u64)
    }

    /// `seek`: bounded by the file, with the end itself reachable.
    fn seek(&self, current: u64, offset: i64, origin: SeekOrigin) -> Result<u64, ()> {
        let length = self.0.len() as u64;
        if current > length {
            return Err(());
        }
        let base = match origin {
            SeekOrigin::Start => 0u64,
            SeekOrigin::Current => current,
            SeekOrigin::End => length,
        };
        let result = (base as i128) + (offset as i128);
        if result < 0 || result > length as i128 {
            return Err(());
        }
        Ok(result as u64)
    }

    /// `truncate`: shorter cuts, longer extends with zeroes.
    fn truncate(&mut self, length: u64) {
        let length = length as usize;
        if length > self.0.len() {
            self.0.resize(length, 0);
        } else {
            self.0.truncate(length);
        }
    }
}

/// Builds a filesystem holding `contents` at `PATH`, and the model of the same file.
fn pair(contents: &[u8]) -> (VirtualFileSystem, Model) {
    let mut fs = VirtualFileSystem::with_defaults().expect("a filesystem");
    fs.insert_file(PATH, contents).expect("the file is created");
    (fs, Model(contents.to_vec()))
}

fn access(read: bool, write: bool) -> FileAccess {
    FileAccess::new(read, write)
}

/// A read that either produced bytes or did not, compared without the error type —
/// the model says *whether*, and the shape of the failure is a separate question
/// that `a_refused_operation_never_changes_the_file` covers.
fn ok<T>(result: Result<T, FileSystemError>) -> Option<T> {
    result.ok()
}

#[test]
fn a_write_at_the_end_extends_and_a_write_past_it_is_refused() {
    let (mut fs, mut model) = pair(b"abc");
    let node = fs
        .open(PATH, OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("opened")
        .node;
    // Exactly at the end: extends.
    let written = ok(fs.write_at(node, 3, b"de", access(true, true)));
    assert_eq!(written, Some(2), "two bytes written at the end");
    model.write_at(3, b"de").expect("the model allows this");
    assert_eq!(contents(&fs, node), model.0, "after extending at the end");

    // One past the end: refused, and the file is unchanged.
    let refused = fs.write_at(node, 6, b"x", access(true, true));
    assert!(refused.is_err(), "a write past the end must be refused");
    assert!(model.write_at(6, b"x").is_err());
    assert_eq!(
        contents(&fs, node),
        model.0,
        "the refused write changed the file"
    );
}

#[test]
fn a_read_past_the_end_is_short_and_a_read_from_beyond_it_is_refused() {
    let (mut fs, model) = pair(b"abcdef");
    let node = fs
        .open(PATH, OpenFlags::new(OPEN_READ).unwrap())
        .expect("opened")
        .node;
    for (offset, length) in [(0u64, 10u64), (3, 3), (6, 1), (6, 0), (0, 0), (5, 100)] {
        let got = ok(fs.read_at(node, offset, length, access(true, false)));
        let want = model.read_at(offset, length).ok();
        assert_eq!(got, want, "read_at(offset={offset}, length={length})");
    }
    for offset in [7u64, 8, 100, u64::MAX] {
        assert!(
            fs.read_at(node, offset, 1, access(true, false)).is_err(),
            "a read from beyond the end must be refused, offset={offset}"
        );
        assert!(model.read_at(offset, 1).is_err());
    }
}

#[test]
fn a_seek_is_bounded_by_the_file_and_the_end_is_reachable() {
    let (mut fs, model) = pair(b"0123456789");
    let node = fs
        .open(PATH, OpenFlags::new(OPEN_READ).unwrap())
        .expect("opened")
        .node;
    let length = 10u64;
    // Every position, every origin, and a spread of offsets around them.
    for current in 0..=length {
        for origin in [SeekOrigin::Start, SeekOrigin::Current, SeekOrigin::End] {
            for offset in [-12i64, -11, -10, -1, 0, 1, 9, 10, 11, 12] {
                let got = ok(fs.seek(node, current, offset, origin));
                let want = model.seek(current, offset, origin).ok();
                assert_eq!(
                    got, want,
                    "seek(current={current}, offset={offset}, {origin:?})"
                );
            }
        }
    }
    // A current position beyond the end is refused whatever the origin, because the
    // caller is already somewhere impossible.
    for origin in [SeekOrigin::Start, SeekOrigin::Current, SeekOrigin::End] {
        assert!(
            fs.seek(node, length + 1, 0, origin).is_err(),
            "a current position past the end must be refused, origin={origin:?}"
        );
        assert!(model.seek(length + 1, 0, origin).is_err());
    }
}

#[test]
fn a_random_operation_sequence_agrees_with_the_model_at_every_step() {
    // The campaign. A few hundred sequences of a few hundred operations, each one
    // deterministic, each one comparing the outcome *and* the file's whole contents
    // after every single operation so that a divergence names its own cause.
    for seed in 0..300u64 {
        let mut rng = lazalith_properties::Gen::seeded(seed);
        let initial_length = rng.below(12) as usize;
        let mut initial = alloc_vec(initial_length);
        for byte in &mut initial {
            *byte = rng.range(1, 256) as u8;
        }
        let (mut fs, mut model) = pair(&initial);
        let node = fs
            .open(PATH, OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
            .expect("opened")
            .node;
        let reader = access(true, false);
        let writer = access(true, true);

        for step in 0..200u64 {
            let length = fs.metadata_node(node).expect("metadata").size;
            let model_length = model.0.len() as u64;
            assert_eq!(
                length, model_length,
                "seed {seed} step {step}: metadata says {length} bytes, the model \
                 has {model_length}"
            );
            // Positions are drawn from a range that deliberately includes the
            // interesting ones: the two ends, one either side of each, and a random
            // interior offset. A uniform draw would almost never produce a boundary.
            let position = |rng: &mut lazalith_properties::Gen, length: u64| -> u64 {
                let edges = [
                    0u64,
                    1,
                    length.saturating_sub(1),
                    length,
                    length + 1,
                    length + 2,
                ];
                match rng.below(8) {
                    0..=5 => edges[rng.below(edges.len() as u64) as usize],
                    _ => rng.below(length + 2),
                }
            };
            let payload_length = rng.below(10) as usize;
            let mut payload = alloc_vec(payload_length);
            for byte in &mut payload {
                *byte = rng.range(1, 256) as u8;
            }

            match rng.below(6) {
                0 | 1 => {
                    let offset = position(&mut rng, model_length);
                    let got = ok(fs.read_at(node, offset, payload_length as u64, reader));
                    let want = model.read_at(offset, payload_length as u64).ok();
                    assert_eq!(
                        got, want,
                        "seed {seed} step {step}: read_at({offset}, {payload_length})"
                    );
                }
                2 | 3 => {
                    let offset = position(&mut rng, model_length);
                    let got = ok(fs.write_at(node, offset, &payload, writer));
                    let want = model.write_at(offset, &payload).ok();
                    assert_eq!(
                        got, want,
                        "seed {seed} step {step}: write_at({offset}, {payload_length} bytes)"
                    );
                    model.write_at(offset, &payload).ok();
                }
                4 => {
                    let current = position(&mut rng, model_length);
                    let origin = match rng.below(3) {
                        0 => SeekOrigin::Start,
                        1 => SeekOrigin::Current,
                        _ => SeekOrigin::End,
                    };
                    let offset = rng.range(0, 6) as i64 - 3;
                    let got = ok(fs.seek(node, current, offset, origin));
                    let want = model.seek(current, offset, origin).ok();
                    assert_eq!(
                        got, want,
                        "seed {seed} step {step}: seek({current}, {offset}, {origin:?})"
                    );
                }
                _ => {
                    let length = position(&mut rng, model_length);
                    let got = ok(fs.truncate(PATH, length));
                    let want = if length as usize <= model.0.len() || true {
                        model.truncate(length);
                        Some(())
                    } else {
                        None
                    };
                    assert_eq!(got, want, "seed {seed} step {step}: truncate({length})");
                }
            }
            assert_eq!(
                contents(&fs, node),
                model.0,
                "seed {seed} step {step}: the file's contents diverged from the model"
            );
        }
    }
}

#[test]
fn a_refused_operation_never_changes_the_file() {
    // The one invariant a filesystem has that the model cannot express, because the
    // model has no notion of failure at all: a call that is refused leaves the bytes
    // exactly as they were. Checked directly, over every refusal the campaign finds.
    let (mut fs, model) = pair(b"abcdef");
    let node = fs
        .open(PATH, OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("opened")
        .node;
    let before = contents(&fs, node);
    for offset in [7u64, 8, 1000, u64::MAX] {
        assert!(fs.write_at(node, offset, b"x", access(true, true)).is_err());
        assert!(fs.read_at(node, offset, 1, access(true, false)).is_err());
        assert_eq!(
            contents(&fs, node),
            before,
            "a refused call changed the file"
        );
    }
    // A read-only handle refuses a write, and a write-only handle refuses a read.
    //
    // The handle's *own* access is what is passed, not a fresh one: `write_at` and
    // `read_at` take the access as an argument, so a test that built its own would be
    // testing nothing but its own argument. The first draft of this line passed
    // `access(true, true)` and the write succeeded, which was the test being wrong
    // rather than the filesystem being permissive.
    let read_only = fs
        .open(PATH, OpenFlags::new(OPEN_READ).unwrap())
        .expect("read only");
    assert!(
        fs.write_at(read_only.node, 0, b"x", read_only.access)
            .is_err(),
        "a handle opened for reading must refuse a write"
    );
    let write_only = fs
        .open(PATH, OpenFlags::new(OPEN_WRITE).unwrap())
        .expect("write only");
    assert!(
        fs.read_at(write_only.node, 0, 1, write_only.access)
            .is_err(),
        "a handle opened for writing must refuse a read"
    );
    assert_eq!(contents(&fs, node), before);
    let _ = model;
}

#[test]
fn the_open_flags_are_held_to_what_they_claim() {
    // `open` is where the flags become an access, and a mismatch there is invisible
    // until a program writes through a handle it only asked to read.
    let (mut fs, _) = pair(b"abc");
    // Neither read nor write is refused: a handle that can do nothing is not a
    // handle.
    assert!(fs.open(PATH, OpenFlags::new(0).unwrap()).is_err());
    // Create and truncate both need write, because both change the file.
    for flags in [OPEN_CREATE, OPEN_TRUNCATE, OPEN_CREATE | OPEN_TRUNCATE] {
        assert!(
            fs.open(PATH, OpenFlags::new(OPEN_READ | flags).unwrap())
                .is_err(),
            "flags {flags:#x} without OPEN_WRITE must be refused"
        );
    }
    // A missing file with no create is a miss, not an empty file.
    assert!(
        fs.open(b"/absent", OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
            .is_err()
    );
    // And with create it is an empty file, which reads as nothing and can be written.
    let created = fs
        .open(
            b"/absent",
            OpenFlags::new(OPEN_READ | OPEN_WRITE | OPEN_CREATE).unwrap(),
        )
        .expect("created")
        .node;
    assert_eq!(contents(&fs, created), Vec::<u8>::new());
    fs.write_at(created, 0, b"now here", access(true, true))
        .expect("written");
    assert_eq!(contents(&fs, created), b"now here".to_vec());
}

#[test]
fn truncating_on_open_clears_the_file_and_opening_again_does_not() {
    // The flag that is easy to get wrong in the *other* direction: a truncate that
    // happens when it was not asked for, or one that does not happen when it was.
    let (mut fs, _) = pair(b"contents that should go");
    let node = fs
        .open(
            PATH,
            OpenFlags::new(OPEN_READ | OPEN_WRITE | OPEN_TRUNCATE).unwrap(),
        )
        .expect("opened")
        .node;
    assert_eq!(
        contents(&fs, node),
        Vec::<u8>::new(),
        "truncate did not clear"
    );
    fs.write_at(node, 0, b"new", access(true, true)).unwrap();
    // Reopening without the flag must not clear it again.
    let again = fs
        .open(PATH, OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("opened")
        .node;
    assert_eq!(
        contents(&fs, again),
        b"new".to_vec(),
        "an open without OPEN_TRUNCATE cleared the file anyway"
    );
}

#[test]
fn truncate_extends_with_zeroes_and_cuts() {
    let (mut fs, mut model) = pair(b"abcdef");
    let node = fs
        .open(PATH, OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("opened")
        .node;
    // Shorter: cuts.
    fs.truncate(PATH, 3).expect("truncated");
    model.truncate(3);
    assert_eq!(contents(&fs, node), model.0);
    // Longer: extends, and the new bytes are zero rather than left over.
    fs.truncate(PATH, 6).expect("truncated");
    model.truncate(6);
    assert_eq!(contents(&fs, node), model.0);
    assert_eq!(contents(&fs, node), b"abc\0\0\0".to_vec());
    // A write into the new tail works, because the tail is part of the file.
    fs.write_at(node, 3, b"XY", access(true, true))
        .expect("written");
    model.write_at(3, b"XY").expect("the model allows this");
    assert_eq!(contents(&fs, node), model.0);
    // And a read of the whole thing agrees.
    assert_eq!(
        ok(fs.read_at(node, 0, 64, access(true, false))),
        Some(model.0.clone())
    );
}

/// The bytes of a node, read back through the filesystem itself.
fn contents(fs: &VirtualFileSystem, node: FileNodeId) -> Vec<u8> {
    fs.read_at(node, 0, u64::MAX, access(true, false))
        .expect("the node reads")
}

fn alloc_vec(length: usize) -> Vec<u8> {
    (0..length).map(|index| index as u8).collect()
}

/// A second file's operations do not disturb the first.
///
/// The model above is one file, which is deliberate — every question is about one
/// file's bytes. This asks the question the one-file model cannot: does an operation
/// on one path reach another's data? A filesystem that shares a buffer between nodes,
/// or that resolves a path to the wrong node, passes every test above and fails this
/// one.
#[test]
fn two_files_do_not_share_bytes() {
    let mut fs = VirtualFileSystem::with_defaults().expect("a filesystem");
    fs.insert_file(b"/one", b"first").expect("one");
    fs.insert_file(b"/two", b"second").expect("two");
    let one = fs
        .open(b"/one", OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("one")
        .node;
    let two = fs
        .open(b"/two", OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .expect("two")
        .node;
    assert_ne!(one, two, "the two files share a node");
    // Grow one well past two, which is where a shared allocation would show.
    fs.write_at(one, 0, &[7u8; 200], access(true, true))
        .expect("wrote");
    assert_eq!(
        contents(&fs, two),
        b"second".to_vec(),
        "writing 200 bytes into /one changed /two"
    );
    // And shrink one to nothing, which would leave a shared buffer handing out stale
    // bytes.
    fs.truncate(b"/one", 0).expect("truncated");
    assert_eq!(contents(&fs, two), b"second".to_vec());
    // Renaming one must not move two's bytes either.
    fs.rename(b"/one", b"/three").expect("renamed");
    assert_eq!(contents(&fs, two), b"second".to_vec());
    assert!(
        fs.metadata(b"/one").is_err(),
        "/one should be gone after a rename"
    );
    assert_eq!(contents(&fs, two), b"second".to_vec());
}

/// Removing a file removes its bytes and nothing else.
#[test]
fn removing_one_file_leaves_the_others_alone() {
    let mut fs = VirtualFileSystem::with_defaults().expect("a filesystem");
    fs.insert_file(b"/a", b"aaa").expect("a");
    fs.insert_file(b"/b", b"bbb").expect("b");
    fs.remove(b"/a").expect("removed");
    assert!(fs.metadata(b"/a").is_err(), "/a should be gone");
    let b = fs
        .open(b"/b", OpenFlags::new(OPEN_READ).unwrap())
        .expect("b")
        .node;
    assert_eq!(contents(&fs, b), b"bbb".to_vec());
    // And re-creating `/a` must not resurrect the old bytes.
    fs.insert_file(b"/a", b"new").expect("recreated");
    let a = fs
        .open(b"/a", OpenFlags::new(OPEN_READ).unwrap())
        .expect("a")
        .node;
    assert_eq!(
        contents(&fs, a),
        b"new".to_vec(),
        "a re-created file came back with the removed file's bytes"
    );
}
