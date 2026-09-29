//! What step 91 added, area by area, and the honest size of each.
//!
//! The roadmap asks for eight areas. This file has a section per area, and each
//! section says what exists now and what does not — because a step that quietly did a
//! tenth of one of them and said nothing is worse than one that says so.

use lazalith_os::{
    FileSystemError, LzaPackage, PackagePermissions, Process, ProcessId, Resolved, ThreadId,
    VirtualFileSystem, resolve,
};
use lazalith_os_abi::{
    ALL_CAPABILITIES, Capabilities, OPEN_READ, OpenFlags, PERMISSION_CONSOLE,
    PERMISSION_FILESYSTEM, PERMISSION_GRAPHICS, PERMISSION_INPUT, Syscall,
};

fn filesystem() -> VirtualFileSystem {
    VirtualFileSystem::with_defaults().expect("a filesystem")
}

/// A process built from eight bytes of code.
///
/// The tests here are about *processes* — permissions, threads, scheduling — and none
/// of them runs the program, so the code is a single halt. Building a process from a
/// real image would make every one of them depend on the compiler for a fact that is
/// not about code.
fn process_with(process: u32, thread: u32) -> Process {
    let program =
        lazalith_os::ProgramImage::new(lazalith_types::ArchitectureConfig::lz64(), 0, &[0_u8; 8])
            .expect("a program image");
    Process::new(
        ProcessId::new(process).expect("a process id"),
        ThreadId::new(thread).expect("a thread id"),
        program,
    )
    .expect("a process")
}

// -- permissions ------------------------------------------------------------

/// The gate's whole rule, in one test: a process started from a package gets what the
/// package declared, and a process started from a bare executable gets everything.
#[test]
fn a_package_declares_and_a_bare_executable_is_trusted() {
    let declared = PackagePermissions::none().with(PERMISSION_CONSOLE);
    let restricted = Capabilities::for_start(declared);
    assert!(restricted.allows(Syscall::Write), "console was declared");
    assert!(
        !restricted.allows(Syscall::Open),
        "opening a file needs the filesystem capability, which was not declared"
    );
    assert!(
        restricted.allows(Syscall::Exit),
        "a process that cannot stop is broken, not restricted"
    );
    let trusted = Capabilities::for_trusted_image();
    for call in [
        Syscall::Write,
        Syscall::Open,
        Syscall::DisplayOpen,
        Syscall::InputPoll,
    ] {
        assert!(
            trusted.allows(call),
            "a bare executable was refused {call:?}"
        );
    }
}

/// Every syscall names exactly one requirement, and a syscall that needs nothing is
/// always allowed.
///
/// The exhaustiveness is the point. `required_by` matches every variant of `Syscall`
/// with no wildcard, so adding a syscall without deciding what it needs is a compile
/// error rather than a silent permission — and this test is what says so out loud.
#[test]
fn every_syscall_names_exactly_one_requirement() {
    let nothing = Capabilities::default();
    for call in Syscall::ALL {
        let required = Capabilities::required_by(*call);
        let allowed = nothing.allows(*call);
        match required {
            None => assert!(allowed, "{call:?} needs nothing and was refused"),
            Some(capability) => assert!(
                !allowed,
                "{call:?} needs {capability} and was allowed with nothing"
            ),
        }
    }
    assert_eq!(ALL_CAPABILITIES, 0b1111);
}

/// A capability this build does not know is *not* granted — the opposite of a package
/// record's unknown bits, which are held, and deliberately so.
#[test]
fn an_unknown_capability_is_not_granted() {
    let declared = PackagePermissions {
        bits: PERMISSION_CONSOLE | 0x80,
    };
    let capabilities = Capabilities::for_start(declared);
    assert_eq!(capabilities.bits, PERMISSION_CONSOLE);
    assert!(
        !capabilities.has(0x80),
        "a kernel granted a capability it cannot enforce"
    );
}

#[test]
fn a_process_remembers_whether_it_was_restricted() {
    let mut process = process_with(1, 1);
    assert!(!process.is_restricted(), "a bare image is the trusted path");
    assert_eq!(process.capabilities(), Capabilities::for_trusted_image());

    process.restrict_to(Capabilities::for_start(
        PackagePermissions::none().with(PERMISSION_FILESYSTEM),
    ));
    assert!(process.is_restricted(), "the restriction did not stick");
    assert!(!process.capabilities().has(PERMISSION_CONSOLE));
    assert!(process.capabilities().has(PERMISSION_FILESYSTEM));
}

/// A restriction only ever takes away: a process that somehow already had a capability
/// cannot keep it.
#[test]
fn a_restriction_is_an_intersection_and_never_a_widening() {
    let everything = Capabilities {
        bits: ALL_CAPABILITIES,
    };
    assert_eq!(
        everything.intersect(Capabilities::for_trusted_image()),
        everything
    );
    assert_eq!(
        everything.intersect(Capabilities::default()),
        Capabilities::default()
    );
}

// -- process management -----------------------------------------------------

/// A minimal image, for the resolution tests. Eight bytes is enough for `LzxImage`,
/// which checks its own shape and nothing about what the code does.
fn built_image() -> Vec<u8> {
    lazalith_os::LzxImage::new(
        lazalith_os::LzxArchitecture::Lz64,
        0,
        0,
        0x400,
        0x1_0000,
        vec![lazalith_os::LzxSection::code(&[0_u8; 8]).expect("a code section")],
    )
    .expect("an image")
    .to_bytes()
    .expect("it serialises")
}

fn package_with(permissions: PackagePermissions) -> Vec<u8> {
    let manifest = format!(
        "[application]\nname = \"tiny\"\nversion = \"1.0.0\"\nentry = \"m.lz\"\narchitecture = \"any\"\n\n[permissions]\nconsole = {}\nfilesystem = {}\ngraphics = {}\ninput = {}\n",
        permissions.has(PERMISSION_CONSOLE),
        permissions.has(PERMISSION_FILESYSTEM),
        permissions.has(PERMISSION_GRAPHICS),
        permissions.has(PERMISSION_INPUT),
    );
    LzaPackage::new(
        b"tiny",
        lazalith_os::PackageVersion::new(1, 0, 0),
        lazalith_os::LzxArchitecture::Lz64,
        manifest.as_bytes(),
        &built_image(),
        &[],
    )
    .expect("the package builds")
    .to_bytes()
    .expect("it serialises")
}

#[test]
fn a_path_naming_a_package_resolves_to_its_executable() {
    let package = package_with(PackagePermissions::none().with(PERMISSION_CONSOLE));
    let resolved = resolve(&package).expect("a package resolves");
    assert!(
        resolved.is_package(),
        "the package was read as a bare image"
    );
    assert!(resolved.into_image().is_ok());
}

#[test]
fn a_path_naming_an_image_resolves_to_the_image() {
    let resolved = resolve(&built_image()).expect("an image resolves");
    assert!(!resolved.is_package());
    assert!(resolved.into_image().is_ok());
}

#[test]
fn a_path_naming_something_else_resolves_to_nothing() {
    assert!(matches!(
        resolve(b"#!/bin/sh\necho hi\n"),
        Err(lazalith_os::ResolveError::NotExecutable { .. })
    ));
}

/// The two halves of steps 88, 89 and 91 meeting: a package's declaration is readable
/// at the point the process is created, which is the only place a restriction can be
/// applied.
#[test]
fn a_packages_declaration_survives_to_the_resolution() {
    let package = package_with(
        PackagePermissions::none()
            .with(PERMISSION_CONSOLE)
            .with(PERMISSION_FILESYSTEM),
    );
    let Resolved::Package(package) = resolve(&package).expect("a package resolves") else {
        panic!("not a package");
    };
    let declared = package.identity.permissions;
    assert!(declared.has(PERMISSION_CONSOLE));
    assert!(declared.has(PERMISSION_FILESYSTEM));
    assert!(!declared.has(PERMISSION_GRAPHICS));
    let capabilities = Capabilities::for_start(declared);
    assert!(capabilities.allows(Syscall::Write));
    assert!(!capabilities.allows(Syscall::DisplayOpen));
}

// -- a better filesystem ----------------------------------------------------

#[test]
fn a_rename_moves_the_node_and_not_its_bytes() {
    let mut filesystem = filesystem();
    filesystem.insert_file(b"/a", b"hello").expect("a file");
    let before = filesystem.metadata(b"/a").expect("its metadata");
    filesystem.rename(b"/a", b"/b").expect("the rename");
    assert!(
        filesystem.metadata(b"/a").is_err(),
        "the old name still resolves"
    );
    let after = filesystem.metadata(b"/b").expect("the new name");
    assert_eq!(
        before.node, after.node,
        "the rename copied instead of moving"
    );
    assert_eq!(after.size, before.size);
}

#[test]
fn a_rename_refuses_to_build_a_cycle_or_overwrite() {
    let mut filesystem = filesystem();
    filesystem.insert_directory(b"/dir").expect("a directory");
    filesystem.insert_file(b"/dir/inner", b"x").expect("a file");
    filesystem.insert_file(b"/taken", b"y").expect("a file");
    assert!(
        filesystem.rename(b"/dir", b"/dir/inner").is_err(),
        "a directory was moved inside itself"
    );
    assert!(
        filesystem.rename(b"/dir", b"/taken").is_err(),
        "a rename overwrote a name that existed"
    );
    assert!(
        filesystem.rename(b"/missing", b"/other").is_err(),
        "a rename of something that is not there succeeded"
    );
}

#[test]
fn a_remove_takes_a_file_or_an_empty_directory_and_nothing_else() {
    let mut filesystem = filesystem();
    filesystem.insert_file(b"/a", b"x").expect("a file");
    filesystem.remove(b"/a").expect("the removal");
    assert!(filesystem.metadata(b"/a").is_err());
    filesystem.insert_directory(b"/empty").expect("a directory");
    filesystem
        .remove(b"/empty")
        .expect("an empty directory goes");
    filesystem.insert_directory(b"/full").expect("a directory");
    filesystem
        .insert_file(b"/full/child", b"x")
        .expect("a child");
    assert!(
        matches!(
            filesystem.remove(b"/full"),
            Err(FileSystemError::IsDirectory)
        ),
        "a directory with children was removed"
    );
    assert!(
        filesystem.metadata(b"/full/child").is_ok(),
        "and the children went with it"
    );
}

#[test]
fn a_truncate_grows_with_zeros_and_cuts_short() {
    let mut filesystem = filesystem();
    filesystem.insert_file(b"/a", b"hello").expect("a file");
    let flags = OpenFlags::new(OPEN_READ).expect("read flags");
    let opened = filesystem.open(b"/a", flags).expect("an open");
    filesystem.truncate(b"/a", 3).expect("a cut");
    let found = filesystem
        .read_at(opened.node, 0, 8, opened.access)
        .expect("a read");
    assert_eq!(found, b"hel");
    filesystem.truncate(b"/a", 6).expect("a grow");
    let found = filesystem
        .read_at(opened.node, 0, 8, opened.access)
        .expect("a read");
    assert_eq!(found, b"hel\0\0\0", "a grown file is not zero filled");
}

#[test]
fn a_walk_is_complete_and_in_the_same_order_twice() {
    let mut filesystem = filesystem();
    filesystem.insert_file(b"/b", b"1").expect("a file");
    filesystem.insert_file(b"/a", b"2").expect("a file");
    filesystem.insert_directory(b"/dir").expect("a directory");
    filesystem
        .insert_file(b"/dir/deep", b"3")
        .expect("a nested file");
    let first = filesystem.walk(b"/").expect("a walk");
    let second = filesystem.walk(b"/").expect("a second walk");
    assert_eq!(first, second, "two walks of one tree differ");
    for wanted in [b"/a".as_slice(), b"/b", b"/dir", b"/dir/deep"] {
        assert!(
            first.iter().any(|found| found == wanted),
            "the walk missed {}: {first:?}",
            String::from_utf8_lossy(wanted)
        );
    }
}

#[test]
fn a_walk_from_a_subtree_stays_in_that_subtree() {
    let mut filesystem = filesystem();
    filesystem.insert_directory(b"/dir").expect("a directory");
    filesystem.insert_file(b"/dir/inner", b"x").expect("a file");
    filesystem.insert_file(b"/outside", b"y").expect("a file");
    let found = filesystem.walk(b"/dir").expect("a walk");
    assert!(
        found.iter().all(|path| path.starts_with(b"/dir")),
        "a subtree walk left the subtree: {found:?}"
    );
    assert!(
        found.contains(&b"/dir/inner".to_vec()),
        "the subtree walk missed its own file: {found:?}"
    );
}

#[test]
fn a_truncate_on_a_directory_is_refused() {
    let mut filesystem = filesystem();
    filesystem.insert_directory(b"/dir").expect("a directory");
    assert!(
        matches!(
            filesystem.truncate(b"/dir", 0),
            Err(FileSystemError::IsDirectory)
        ),
        "a directory was truncated"
    );
}

// -- threads ----------------------------------------------------------------

/// A process holds one register file per thread, and the memory context is chosen by
/// thread id. What step 91 checked is that the per-thread state is real: a context for
/// a thread that does not exist is refused rather than quietly built from the first
/// thread's registers.
#[test]
fn a_process_holds_one_state_per_thread() {
    let mut process = process_with(1, 1);
    assert_eq!(
        process.primary_thread().id(),
        ThreadId::new(1).expect("an id")
    );
    assert!(
        process
            .memory_context_for_thread(ThreadId::new(7).expect("an id"))
            .is_err(),
        "a context was made for a thread that does not exist"
    );
    assert!(
        process
            .thread_state(ThreadId::new(7).expect("an id"))
            .expect("the query is answered")
            .is_none(),
        "a state was invented for a thread that does not exist"
    );
}

/// One thread per process in this build, and the test says so — so a process that
/// grew a second thread with nothing creating one would fail here rather than passing
/// quietly.
#[test]
fn a_process_starts_with_exactly_one_thread() {
    let process = process_with(2, 3);
    assert_eq!(process.thread_count(), 1);
    assert_eq!(
        process.primary_thread().id(),
        ThreadId::new(3).expect("an id")
    );
    let state = process
        .thread_state(ThreadId::new(3).expect("an id"))
        .expect("the query is answered")
        .expect("the thread exists");
    assert_eq!(
        state.pc(),
        process.primary_thread().cpu().pc(),
        "the state is a real register file and not a copy of the default"
    );
}

// -- more drivers -----------------------------------------------------------

/// The timer reports the cycle count, refuses writes, and is in a snapshot because a
/// program can read it.
#[test]
fn a_timer_reports_the_cycle_count_and_refuses_a_write() {
    use lazalith_devices::{Device, DeviceError, DeviceOffset, TimerDevice};
    use lazalith_isa::DataSize;
    use lazalith_types::CycleCount;

    let mut timer = TimerDevice::new();
    assert_eq!(timer.cycles(), CycleCount::new(0));
    assert_eq!(
        timer
            .read(DeviceOffset::new(0), DataSize::Double)
            .expect("a read"),
        0
    );
    // `tick` is told what time it is, not how much time passed, so the second call
    // replaces the first. **This asserted 20 for the whole of B5–B19** — the counter
    // accumulated, so 17 + 3 — which was right only because the machine's clock moved
    // at most once before execution charged anything, and a single tick looks the same
    // either way. The machine's `DeviceManager::tick` passes the new absolute elapsed
    // time, so a timer that adds was computing a sum of absolute timestamps, and a
    // guest reading it would be told a time the machine was never at.
    timer.tick(CycleCount::new(17));
    timer.tick(CycleCount::new(3));
    assert_eq!(
        timer
            .read(DeviceOffset::new(0), DataSize::Double)
            .expect("a read"),
        3,
        "the counter reports the time the machine last said it was, and does not \
         accumulate the timestamps it was handed"
    );
    // A write is refused rather than ignored, and `DeviceError` has no `PartialEq`
    // because two errors can be the same *kind* and different *events*; the
    // discriminant is what matters here, so it is matched.
    assert!(matches!(
        timer.write(DeviceOffset::new(0), DataSize::Double, 0),
        Err(DeviceError::WriteUnsupported)
    ));
    assert!(
        timer.read(DeviceOffset::new(8), DataSize::Double).is_err(),
        "a read past the register was accepted"
    );
    assert!(
        timer.read(DeviceOffset::new(0), DataSize::Word).is_err(),
        "a narrow read of a 64-bit counter was accepted"
    );
}

#[test]
fn a_timer_round_trips_through_a_snapshot() {
    use lazalith_devices::{Device, DeviceOffset, TimerDevice};
    use lazalith_isa::DataSize;
    use lazalith_types::CycleCount;

    let mut timer = TimerDevice::new();
    timer.tick(CycleCount::new(99));
    let snapshot = timer.snapshot();
    let mut restored = TimerDevice::new();
    restored.restore(&snapshot).expect("the restore");
    assert_eq!(
        restored
            .read(DeviceOffset::new(0), DataSize::Double)
            .expect("a read"),
        99,
        "a restored timer is not the timer that was captured"
    );
    assert!(
        TimerDevice::new().restore(b"short").is_err(),
        "a malformed timer snapshot was accepted"
    );
}

#[test]
fn a_timer_reset_puts_the_counter_back() {
    use lazalith_devices::{Device, TimerDevice};
    use lazalith_types::CycleCount;

    let mut timer = TimerDevice::new();
    timer.tick(CycleCount::new(500));
    timer.reset();
    assert_eq!(timer.cycles(), CycleCount::new(0));
}

// -- what is deliberately absent --------------------------------------------

/// Networking and audio have nothing in this build: there is no network device and no
/// audio device, so there is nothing to drive.
///
/// The step listed them, so the honest thing is to say so rather than to invent a
/// socket that always fails and call it a network stack. What the test asserts is the
/// *absence*, so that a socket or a sound syscall appearing without a device behind it
/// would fail here.
#[test]
fn there_is_no_network_or_audio_device_and_no_abi_for_one() {
    for call in Syscall::ALL {
        let name = format!("{call:?}");
        assert!(
            !name.contains("Socket") && !name.contains("Audio") && !name.contains("Sound"),
            "{name} implies a device this build does not have"
        );
    }
}
