//! The premises `docs/lazen-packages.md` rests on.
//!
//! A design document that cites code is only as good as the code, and a design
//! document whose premises have quietly stopped being true is worse than one with no
//! claims at all: it is confidently wrong. So the claims that can be *checked* are
//! checked here.
//!
//! Each test is one row of the "what the repository already knows" table in the
//! design. When one of them starts failing, the design has gone stale and the
//! failure says which claim moved.

use lazalith_os::{
    DEFAULT_MAX_FILE_BYTES, LZX_FORMAT_VERSION, LZX_MAGIC, LZX_MAX_FILE_SIZE, LazalithKernel,
    LzxArchitecture, LzxImage, LzxSection,
};

/// A minimal image: one code section and an entry.
fn image() -> LzxImage {
    LzxImage::new(
        LzxArchitecture::Lz64,
        0,
        0,
        0x400,
        0x1_0000,
        vec![
            LzxSection::new(
                lazalith_os::LzxSectionKind::Code,
                lazalith_os::LZX_CODE_PERMISSIONS,
                0,
                8,
                4,
                &[0_u8; 8],
            )
            .expect("a code section"),
        ],
    )
    .expect("an image")
}

/// The design's rule 1: a package stores a complete `.lzx` and the `.lzx` is
/// authoritative. That only works if an image is self-describing, so this is the
/// assumption the whole container rests on.
#[test]
fn an_executable_is_self_describing_and_round_trips() {
    let bytes = image().to_bytes().expect("the image serialises");
    assert_eq!(
        &bytes[0..8],
        &LZX_MAGIC,
        "the image does not start with its magic"
    );
    assert_eq!(
        u16::from_le_bytes([bytes[8], bytes[9]]),
        LZX_FORMAT_VERSION,
        "the image does not start with its format version"
    );
    let read = LzxImage::from_bytes(&bytes).expect("the image reads back");
    assert_eq!(read, image());
    assert_eq!(
        read.to_bytes().expect("it re-encodes"),
        bytes,
        "an image did not survive its own encoding"
    );
}

/// The design's central gap: a bare executable has nowhere to put a name or a
/// version, so two applications built from identical code are the same file. This is
/// *why* a package header exists, and it stops being true the moment someone adds
/// identity to `.lzx` — which would be a better fix, and would make this test say so
/// rather than leave a stale design standing.
#[test]
fn a_bare_executable_carries_no_identity() {
    let first = image().to_bytes().expect("serialises");
    let second = image().to_bytes().expect("serialises");
    assert_eq!(
        first, second,
        "two images built the same way are now different files, so the .lzx has \
         started carrying something that varies \x2d\x2d see the design's central gap"
    );
    // And the header has no room for a name or a version: it is a fixed 64 bytes of
    // magic, version, architecture, counts, requirements and two debug-block words.
    // There is no field for identity, which is why the container needs one.
    assert_eq!(
        LZX_MAGIC.len(),
        8,
        "the magic is not eight bytes, so the design's header layout is out of date"
    );
}

/// The design's first driver, stated as far as the code allows.
///
/// `SpawnProcess` names a *path* and the only way into the scheduler today is to
/// hand the kernel an image. The missing middle — turning that path into bytes — is
/// the design's reason for a resolution rule, and it is recorded as a gap because it
/// cannot be asserted: a test for "this does not exist" is a comment. What can be
/// asserted is the half that does exist, and this is it. An image goes in, a
/// process comes out under the id it was asked for, and a corrupted image is refused
/// at the reader rather than half-started — which is what the design's rule 1 depends
/// on: the `.lzx` inside a package is parsed by the `.lzx` reader or it is not an
/// image.
#[test]
fn the_only_way_to_start_a_process_today_is_to_hand_the_kernel_an_image() {
    let mut kernel = LazalithKernel::new(
        1_000,
        lazalith_os::VirtualTerminal::new(b"").expect("a terminal"),
        lazalith_os::VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("a kernel");
    let process = lazalith_os::ProcessId::new(1).expect("a process id");
    let thread = lazalith_os::ThreadId::new(1).expect("a thread id");
    kernel
        .start_image(image(), process, thread)
        .expect("an image starts a process");
    assert_eq!(
        kernel.scheduler().process(process).map(|found| found.id()),
        Some(process),
        "the process the kernel started is not the one it was asked for"
    );
    let mut broken = image().to_bytes().expect("serialises");
    broken[0] = 0;
    assert!(
        LzxImage::from_bytes(&broken).is_err(),
        "an image with a broken magic was accepted"
    );
}

/// The design's second gap, asserted rather than reconciled: a package is bounded by
/// the *smaller* of the executable limit and the VFS's per-file limit, and today the
/// VFS is the smaller one. The design leaves this question open on purpose, so this
/// test records which way it currently points. If it ever stops being true, the
/// design's open question has been answered by something, and should be closed.
#[test]
// The comparison is between two constants, and that is the whole point: the design
// records a contradiction between two published limits, and the only way a test can
// notice that the contradiction has been resolved is to compare them at run time and
// be told it is now trivially true.
#[allow(clippy::assertions_on_constants)]
fn a_package_is_bounded_by_the_smaller_of_two_limits_that_disagree() {
    assert!(
        DEFAULT_MAX_FILE_BYTES < LZX_MAX_FILE_SIZE,
        "the VFS now holds a whole executable, so the design.s size question is \
         answered and the gap table in docs/lazen-packages.md can be closed"
    );
}

/// Step 89's other half: the resolution rule the first design driver asked for.
///
/// `SpawnProcess` names a path, and this is what a path's bytes become. The rule
/// decides by **content** and never by name, so the same bytes resolve the same way
/// whatever the file was called — which is the property that makes `install` a copy
/// rather than a naming convention.
#[test]
fn a_path_resolves_by_content_and_never_by_name() {
    use lazalith_os::{Container, LzaPackage, LzaResource, resolve};

    let built = image().to_bytes().expect("the image serialises");
    let manifest = b"[application]\nname = \"hello\"\nversion = \"1.2.3\"\nentry = \"m.lz\"\narchitecture = \"any\"\n";
    let package = LzaPackage::new(
        b"hello",
        lazalith_os::PackageVersion::new(1, 2, 3),
        LzxArchitecture::Lz64,
        manifest,
        &built,
        &[],
    )
    .expect("the package builds");
    let packed = package.to_bytes().expect("the package serialises");

    assert_eq!(Container::of(&built), Container::Image);
    assert_eq!(Container::of(&packed), Container::Package);
    // The same executable, reached through a package and through nothing.
    let direct = resolve(&built).expect("an image resolves");
    let through = resolve(&packed).expect("a package resolves");
    assert!(!direct.is_package());
    assert!(through.is_package());
    assert!(direct.into_image().is_ok());
    assert!(through.into_image().is_ok());
    // And the resource table a manifest declares survives the container, which is
    // the one thing the package adds that the executable does not have.
    let with_resource = LzaPackage::new(
        b"hello",
        lazalith_os::PackageVersion::new(1, 2, 3),
        LzxArchitecture::Lz64,
        b"[application]\nname = \"hello\"\nversion = \"1.2.3\"\nentry = \"m.lz\"\narchitecture = \"any\"\n\n[resources]\nlogo = \"a.rgb\"\n",
        &built,
        &[LzaResource::label(b"logo")],
    )
    .expect("the package builds");
    let read =
        LzaPackage::from_bytes(&with_resource.to_bytes().expect("serialises")).expect("reads back");
    assert_eq!(read.resources.len(), 1);
    assert_eq!(read.resources[0].name, b"logo");
}
