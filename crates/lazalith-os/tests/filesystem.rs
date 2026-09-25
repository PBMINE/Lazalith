use lazalith_os::abi::{OPEN_CREATE, OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, OpenFlags, SeekOrigin};
use lazalith_os::{
    FileAccess, FileNodeId, FileNodeKind, FileSystemError, FileSystemLimits, ProcessHandles,
    VirtualFileSystem,
};

#[test]
fn virtual_filesystem_resolves_files_and_supports_offsets() {
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_file(b"/greeting.txt", b"abc").unwrap();
    let opened = filesystem
        .open(
            b"/greeting.txt",
            OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap(),
        )
        .unwrap();
    assert_eq!(opened.access, FileAccess::new(true, true));
    assert_eq!(
        filesystem
            .read_at(opened.node, 0, 3, opened.access)
            .unwrap(),
        b"abc"
    );
    assert_eq!(
        filesystem
            .read_at(opened.node, 0, 0, opened.access)
            .unwrap(),
        b""
    );
    assert_eq!(
        filesystem
            .read_at(opened.node, 1, 99, opened.access)
            .unwrap(),
        b"bc"
    );
    assert_eq!(
        filesystem
            .write_at(opened.node, 0, b"", opened.access)
            .unwrap(),
        0
    );
    assert_eq!(
        filesystem
            .write_at(opened.node, 3, b"def", opened.access)
            .unwrap(),
        3
    );
    assert_eq!(
        filesystem
            .read_at(opened.node, 0, 99, opened.access)
            .unwrap(),
        b"abcdef"
    );
    assert_eq!(
        filesystem
            .seek(opened.node, 0, 0, SeekOrigin::Start)
            .unwrap(),
        0
    );
    assert_eq!(
        filesystem
            .seek(opened.node, 2, 2, SeekOrigin::Current)
            .unwrap(),
        4
    );
    assert_eq!(
        filesystem
            .seek(opened.node, 0, -2, SeekOrigin::End)
            .unwrap(),
        4
    );
    assert!(matches!(
        filesystem.seek(opened.node, 0, -7, SeekOrigin::End),
        Err(FileSystemError::InvalidSeekOffset { .. })
    ));
}

#[test]
fn creation_truncation_listing_and_limits_are_deterministic() {
    let mut filesystem = VirtualFileSystem::new(FileSystemLimits::new(8, 8, 2)).unwrap();
    filesystem.insert_directory(b"/src").unwrap();
    filesystem.insert_file(b"/src/z.txt", b"z").unwrap();
    filesystem.insert_file(b"/src/a.txt", b"a").unwrap();
    let entries = filesystem.list(b"/src").unwrap();
    assert_eq!(entries[0].name(), b"a.txt");
    assert_eq!(entries[0].kind(), FileNodeKind::File);
    assert_eq!(entries[1].name(), b"z.txt");

    let created = filesystem
        .open(
            b"/new.txt",
            OpenFlags::new(OPEN_WRITE | OPEN_CREATE).unwrap(),
        )
        .unwrap();
    assert_eq!(filesystem.metadata_node(created.node).unwrap().size, 0);
    assert!(
        filesystem
            .open(b"/new.txt", OpenFlags::new(OPEN_READ).unwrap())
            .is_ok()
    );
    filesystem
        .open(
            b"/new.txt",
            OpenFlags::new(OPEN_WRITE | OPEN_TRUNCATE).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        filesystem.write_at(created.node, 0, b"123456789", FileAccess::new(false, true)),
        Err(FileSystemError::FileTooLarge { .. })
    ));
    assert!(matches!(
        filesystem.read_at(created.node, 0, 99, created.access),
        Err(FileSystemError::PermissionDenied)
    ));
    let reader = filesystem
        .open(b"/new.txt", OpenFlags::new(OPEN_READ).unwrap())
        .unwrap();
    assert_eq!(
        filesystem
            .read_at(reader.node, 0, 99, reader.access)
            .unwrap(),
        b""
    );
    let mut bounded = VirtualFileSystem::new(FileSystemLimits::new(8, 4, 2)).unwrap();
    bounded.insert_file(b"/full", b"12345678").unwrap();
    let overwrite = bounded
        .open(b"/full", OpenFlags::new(OPEN_READ | OPEN_WRITE).unwrap())
        .unwrap();
    assert_eq!(
        bounded
            .write_at(overwrite.node, 0, b"X", overwrite.access)
            .unwrap(),
        1
    );
    assert!(matches!(
        bounded.write_at(overwrite.node, 7, b"12", overwrite.access),
        Err(FileSystemError::FileTooLarge { maximum: 8, .. })
    ));
    assert!(matches!(
        filesystem.insert_file(b"/third.txt", b"x"),
        Err(FileSystemError::DirectoryLimit { .. })
    ));
    let mut limited = VirtualFileSystem::new(FileSystemLimits::new(8, 2, 2)).unwrap();
    limited.insert_file(b"/first", b"x").unwrap();
    assert!(matches!(
        limited.insert_file(b"/second", b"x"),
        Err(FileSystemError::NodeLimit { .. })
    ));
}

#[test]
fn file_access_is_enforced_by_the_filesystem_for_reads_and_writes() {
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_file(b"/secret", b"classified").unwrap();
    let write_only = filesystem
        .open(b"/secret", OpenFlags::new(OPEN_WRITE).unwrap())
        .unwrap();
    assert_eq!(write_only.access, FileAccess::new(false, true));
    assert!(matches!(
        filesystem.read_at(write_only.node, 0, 99, write_only.access),
        Err(FileSystemError::PermissionDenied)
    ));
    assert_eq!(filesystem.metadata(b"/secret").unwrap().size, 10);

    let read_only = filesystem
        .open(b"/secret", OpenFlags::new(OPEN_READ).unwrap())
        .unwrap();
    assert_eq!(read_only.access, FileAccess::new(true, false));
    assert_eq!(
        filesystem
            .read_at(read_only.node, 0, 99, read_only.access)
            .unwrap(),
        b"classified"
    );
    assert!(matches!(
        filesystem.write_at(read_only.node, 0, b"x", read_only.access),
        Err(FileSystemError::PermissionDenied)
    ));
    assert_eq!(
        filesystem
            .read_at(read_only.node, 0, 99, read_only.access)
            .unwrap(),
        b"classified"
    );
}

#[test]
fn invalid_paths_and_modes_do_not_mutate_files() {
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_file(b"/file", b"value").unwrap();
    for path in [
        &b""[..],
        b"relative",
        b"/",
        b"//file",
        b"/./file",
        b"/../file",
        b"/file/",
        b"/file\0name",
    ] {
        let result = filesystem.open(path, OpenFlags::new(OPEN_READ).unwrap());
        assert!(result.is_err(), "path {:?} unexpectedly opened", path);
    }
    assert_eq!(filesystem.metadata(b"/file").unwrap().size, 5);
    let mut boundary = vec![b'/'];
    boundary.extend(core::iter::repeat_n(b'a', 252));
    filesystem.insert_file(&boundary, b"ok").unwrap();
    boundary[1] = b'b';
    boundary.push(b'b');
    assert!(matches!(
        filesystem.insert_file(&boundary, b"no"),
        Err(FileSystemError::InvalidName { maximum: 252, .. })
    ));
    assert!(matches!(
        filesystem.open(
            b"/missing",
            OpenFlags::new(OPEN_READ | OPEN_CREATE).unwrap()
        ),
        Err(FileSystemError::InvalidAccess)
    ));
    assert!(matches!(
        filesystem.open(b"/missing", OpenFlags::new(OPEN_CREATE).unwrap()),
        Err(FileSystemError::InvalidAccess)
    ));
    assert!(matches!(
        filesystem.open(b"/missing", OpenFlags::new(OPEN_READ).unwrap()),
        Err(FileSystemError::NotFound)
    ));
    assert_eq!(filesystem.metadata(b"/file").unwrap().size, 5);
}

#[test]
fn process_file_handles_are_owned_and_stale_handles_fail() {
    let node = FileNodeId::new(1).unwrap();
    let mut first = ProcessHandles::new();
    let handle = first.open_file(node, FileAccess::new(true, true)).unwrap();
    assert_eq!(first.file(handle).unwrap().node(), node);
    assert_eq!(first.file(handle).unwrap().offset(), 0);
    assert!(first.close_file(handle).is_ok());
    assert!(first.close_file(handle).is_err());
    assert!(first.file(handle).is_err());
    let next = first.open_file(node, FileAccess::new(true, false)).unwrap();
    assert_ne!(next, handle);

    let mut second = ProcessHandles::new();
    let other = second
        .open_file(node, FileAccess::new(false, true))
        .unwrap();
    assert_eq!(other, handle);
    assert_eq!(second.file(other).unwrap().node(), node);
    assert!(first.file(other).is_err());
}
