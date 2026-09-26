use lazalith_os_abi::{
    ABI_VERSION, AbiError, AbiFileKind, DIRECTORY_NAME_CAPACITY, DIRECTORY_RECORD_SIZE,
    DirectoryRecord, EXIT_STATUS_RECORD_SIZE, ExitStatusRecord, FILE_STAT_SIZE, FileHandle,
    FilePermissions, FileStat, IO_RESULT_SIZE, IoResult, MAX_ARGUMENT_BYTES, MAX_ARGUMENT_COUNT,
    MAX_ARGUMENT_TOTAL_BYTES, MAX_PATH_BYTES, MEMORY_ALLOCATION_SIZE, MemoryAllocation, OPEN_ALL,
    OPEN_CREATE, OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, OpenFlags, ProcessExitReason, ProcessHandle,
    SYSCALL_ARGUMENT_COUNT, SYSCALL_FIRST_ARGUMENT_REGISTER, SYSCALL_NUMBER_REGISTER,
    SYSCALL_RESERVED_REGISTER, SeekOrigin, Syscall, SyscallArguments, SyscallError, SyscallStatus,
    TaggedOutcome, WordValueOutOfRange, valid_open_flags, validate_range,
    validate_reserved_register,
};
use lazalith_types::{ArchitectureConfig as C, WidthError, WordWidth};

#[test]
fn syscall_numbers_are_stable_complete_and_unique() {
    const EXPECTED: [(Syscall, u16); 17] = [
        (Syscall::Exit, 0x0001),
        (Syscall::Write, 0x0002),
        (Syscall::Read, 0x0003),
        (Syscall::Open, 0x0004),
        (Syscall::Close, 0x0005),
        (Syscall::Seek, 0x0006),
        (Syscall::Stat, 0x0007),
        (Syscall::ListDirectory, 0x0008),
        (Syscall::Time, 0x0009),
        (Syscall::Sleep, 0x000a),
        (Syscall::AllocateMemory, 0x000b),
        (Syscall::SpawnProcess, 0x000c),
        (Syscall::WaitProcess, 0x000d),
        (Syscall::ClearScreen, 0x000e),
        (Syscall::DisplayOpen, 0x000f),
        (Syscall::DisplayPresent, 0x0010),
        (Syscall::InputPoll, 0x0011),
    ];

    assert_eq!(ABI_VERSION, 1);
    assert_eq!(MAX_PATH_BYTES, 4096);
    assert_eq!(MAX_ARGUMENT_COUNT, 1024);
    assert_eq!(MAX_ARGUMENT_BYTES, 65_536);
    assert_eq!(MAX_ARGUMENT_TOTAL_BYTES, 1_048_576);
    assert_eq!(SYSCALL_NUMBER_REGISTER, 0);
    assert_eq!(SYSCALL_FIRST_ARGUMENT_REGISTER, 1);
    assert_eq!(SYSCALL_ARGUMENT_COUNT, 6);
    assert_eq!(SYSCALL_RESERVED_REGISTER, 7);
    assert_eq!(Syscall::ALL, EXPECTED.map(|(call, _)| call));
    for (call, raw) in EXPECTED {
        assert_eq!(call.as_u16(), raw);
        assert_eq!(Syscall::try_from(raw), Ok(call));
        assert_eq!(Syscall::from_word(C::lz32(), u64::from(raw)), Ok(call));
        assert_eq!(Syscall::from_word(C::lz64(), u64::from(raw)), Ok(call));
        assert!(!Syscall::is_reserved(u64::from(raw)));
    }
    for raw in [0_u16, 0x0012, 0x0100, 0xffff] {
        assert_eq!(
            Syscall::try_from(raw),
            Err(AbiError::UnknownSyscall(u64::from(raw)))
        );
    }
    for raw in [0_u64, 0x0012, 0x0100, 0xffff, 0x0001_0001, u64::MAX] {
        assert_eq!(
            Syscall::from_word(C::lz64(), raw),
            Err(AbiError::UnknownSyscall(raw))
        );
    }
    assert_eq!(
        Syscall::from_word(C::lz32(), 0x0001_0001),
        Err(AbiError::UnknownSyscall(0x0001_0001))
    );
    for raw in [0x0100, 0x0101, 0xffff] {
        assert!(Syscall::is_reserved(raw));
    }
    assert!(!Syscall::is_reserved(0x0001_0000));
    assert!(!Syscall::is_reserved(u64::MAX));
}

#[test]
fn syscall_metadata_defines_the_shared_request_contract() {
    const EXPECTED: [(Syscall, usize, bool, u64, u64); 14] = [
        (Syscall::Exit, 1, false, 0, 0x003e),
        (Syscall::Write, 5, true, 0x0010, 0x0020),
        (Syscall::Read, 5, true, 0x0010, 0x0020),
        (Syscall::Open, 4, true, 0x0008, 0x0030),
        (Syscall::Close, 1, true, 0, 0x003e),
        (Syscall::Seek, 4, true, 0, 0x0030),
        (Syscall::Stat, 4, true, 0x0008, 0x0030),
        (Syscall::ListDirectory, 5, true, 0, 0x0020),
        (Syscall::Time, 1, true, 0, 0x003e),
        (Syscall::Sleep, 1, true, 0, 0x003e),
        (Syscall::AllocateMemory, 4, true, 0x0008, 0x0030),
        (Syscall::SpawnProcess, 5, true, 0, 0x0020),
        (Syscall::WaitProcess, 3, true, 0x0004, 0x0038),
        (Syscall::ClearScreen, 0, true, 0x003f, 0),
    ];

    for (call, count, returns, required_zero, ignored) in EXPECTED {
        assert_eq!(call.argument_count(), count);
        assert_eq!(call.returns(), returns);
        assert_eq!(call.required_zero_argument_mask(), required_zero);
        assert_eq!(call.ignored_argument_mask(), ignored);
        assert_eq!(required_zero & ignored, 0);
    }

    assert!(
        SyscallArguments::new([0, 1, 0, 0, 0, 0])
            .validate_required_zero(Syscall::Exit)
            .is_ok()
    );
    assert_eq!(
        SyscallArguments::new([0, 0, 0, 0, 1, 0]).validate_required_zero(Syscall::Write),
        Err(AbiError::InvalidArgument { index: 5 })
    );
    assert!(
        SyscallArguments::new([0, 0, 0, 0x1000, 0, 0])
            .validate_required_zero(Syscall::Seek)
            .is_ok()
    );
    assert_eq!(
        SyscallArguments::new([1, 0, 0, 0, 0, 0]).validate_required_zero(Syscall::ClearScreen),
        Err(AbiError::InvalidArgument { index: 1 })
    );
    assert_eq!(validate_reserved_register(0), Ok(()));
    assert_eq!(
        validate_reserved_register(1),
        Err(AbiError::InvalidArgument { index: 7 })
    );
}

#[test]
fn syscall_statuses_errors_and_tagged_outcomes_are_stable() {
    const VALUES: [(SyscallStatus, SyscallError, u32); 19] = [
        (
            SyscallStatus::UnknownSyscall,
            SyscallError::UnknownSyscall,
            1,
        ),
        (
            SyscallStatus::InvalidArgument,
            SyscallError::InvalidArgument,
            2,
        ),
        (
            SyscallStatus::InvalidPointer,
            SyscallError::InvalidPointer,
            3,
        ),
        (SyscallStatus::RangeOverflow, SyscallError::RangeOverflow, 4),
        (SyscallStatus::Misaligned, SyscallError::Misaligned, 5),
        (SyscallStatus::NotFound, SyscallError::NotFound, 6),
        (SyscallStatus::AlreadyExists, SyscallError::AlreadyExists, 7),
        (
            SyscallStatus::PermissionDenied,
            SyscallError::PermissionDenied,
            8,
        ),
        (SyscallStatus::InvalidHandle, SyscallError::InvalidHandle, 9),
        (SyscallStatus::NotDirectory, SyscallError::NotDirectory, 10),
        (SyscallStatus::IsDirectory, SyscallError::IsDirectory, 11),
        (SyscallStatus::NotSupported, SyscallError::NotSupported, 12),
        (
            SyscallStatus::ResourceExhausted,
            SyscallError::ResourceExhausted,
            13,
        ),
        (SyscallStatus::InvalidState, SyscallError::InvalidState, 14),
        (SyscallStatus::IoFailure, SyscallError::IoFailure, 15),
        (
            SyscallStatus::DeviceFailure,
            SyscallError::DeviceFailure,
            16,
        ),
        (
            SyscallStatus::ProcessFailure,
            SyscallError::ProcessFailure,
            17,
        ),
        (SyscallStatus::Faulted, SyscallError::Faulted, 18),
        (SyscallStatus::Internal, SyscallError::Internal, 19),
    ];

    assert_eq!(SyscallStatus::Ok.as_u32(), 0);
    assert_eq!(SyscallStatus::try_from(0), Ok(SyscallStatus::Ok));
    for (status, error, raw) in VALUES {
        assert_eq!(status.as_u32(), raw);
        assert_eq!(SyscallStatus::try_from(raw), Ok(status));
        assert_eq!(error.as_u32(), raw);
        assert_eq!(SyscallStatus::from(error), status);
    }
    assert_eq!(
        SyscallStatus::try_from(20),
        Err(AbiError::InvalidStatus(20))
    );

    let success = TaggedOutcome::success(u32::MAX);
    assert_eq!(success.status(), SyscallStatus::Ok);
    assert_eq!(success.payload(), u32::MAX);
    assert_eq!(success.registers(), [0, u64::from(u32::MAX)]);
    assert_eq!(TaggedOutcome::decode(success.registers()), Ok(success));

    let failure = TaggedOutcome::failure(SyscallError::InvalidArgument, 7);
    assert_eq!(failure.status(), SyscallStatus::InvalidArgument);
    assert_eq!(failure.payload(), 7);
    assert_eq!(failure.registers(), [2, 7]);
    assert_eq!(TaggedOutcome::decode(failure.registers()), Ok(failure));
    assert_eq!(
        TaggedOutcome::decode([20, 0]),
        Err(AbiError::InvalidStatus(20))
    );
    assert_eq!(
        TaggedOutcome::decode([1 << 32, 0]),
        Err(AbiError::InvalidStatus(1 << 32))
    );
    assert_eq!(
        TaggedOutcome::decode([0, 1 << 32]),
        Err(AbiError::InvalidArgument { index: 1 })
    );
}

#[test]
fn syscall_arguments_enforce_scalar_and_pointer_widths() {
    let arguments = SyscallArguments::new([
        u64::from(u32::MAX),
        u64::from(u32::MAX) + 1,
        0x1_0000_0000,
        0,
        0,
        0,
    ]);

    assert_eq!(arguments.get(0), Some(u64::from(u32::MAX)));
    assert_eq!(arguments.get(6), None);
    assert_eq!(arguments.word(C::lz32(), 0), Ok(u64::from(u32::MAX)));
    assert_eq!(arguments.word(C::lz64(), 1), Ok(u64::from(u32::MAX) + 1));
    assert_eq!(
        arguments.word(C::lz32(), 1),
        Err(AbiError::InvalidArgumentWidth {
            index: 1,
            source: WordValueOutOfRange::new(u64::from(u32::MAX) + 1, WordWidth::W32),
        })
    );
    assert_eq!(arguments.u32(0), Ok(u32::MAX));
    assert_eq!(
        arguments.u32(1),
        Err(AbiError::InvalidArgument { index: 1 })
    );
    assert_eq!(
        arguments.word(C::lz64(), 6),
        Err(AbiError::InvalidArgument { index: 6 })
    );
    assert_eq!(
        arguments.u32(6),
        Err(AbiError::InvalidArgument { index: 6 })
    );
    assert_eq!(
        arguments.pointer(C::lz32(), 0),
        Ok(lazalith_types::VirtualAddress::new(u64::from(u32::MAX)))
    );
    assert_eq!(
        arguments.pointer(C::lz32(), 1),
        Err(AbiError::InvalidPointerWidth {
            index: 1,
            source: WordValueOutOfRange::new(u64::from(u32::MAX) + 1, WordWidth::W32),
        })
    );
    assert_eq!(
        arguments.pointer(C::lz64(), 1),
        Ok(lazalith_types::VirtualAddress::new(u64::from(u32::MAX) + 1))
    );
    assert_eq!(
        arguments.pointer(C::lz32(), 6),
        Err(AbiError::InvalidArgument { index: 6 })
    );
    assert_eq!(arguments.usize(C::lz32(), 0), Ok(u32::MAX as usize));
    assert_eq!(
        arguments.usize(C::lz32(), 1),
        Err(AbiError::InvalidArgumentWidth {
            index: 1,
            source: WordValueOutOfRange::new(u64::from(u32::MAX) + 1, WordWidth::W32),
        })
    );
    let expected = usize::try_from(u64::from(u32::MAX) + 1)
        .map_err(|_| AbiError::InvalidArgument { index: 1 });
    assert_eq!(arguments.usize(C::lz64(), 1), expected);

    let signed = SyscallArguments::new([u64::from(u32::MAX), 1_u64 << 31, 1_u64 << 63, 0, 0, 0]);
    assert_eq!(signed.signed_word(C::lz32(), 0), Ok(-1));
    assert_eq!(signed.signed_word(C::lz64(), 0), Ok(i64::from(u32::MAX)));
    assert_eq!(signed.signed_word(C::lz32(), 1), Ok(i64::from(i32::MIN)));
    assert_eq!(signed.signed_word(C::lz64(), 1), Ok(1 << 31));
    assert_eq!(
        signed.signed_word(C::lz32(), 2),
        Err(AbiError::InvalidArgumentWidth {
            index: 2,
            source: WordValueOutOfRange::new(1_u64 << 63, WordWidth::W32),
        })
    );
    assert_eq!(signed.signed_word(C::lz64(), 2), Ok(i64::MIN));
    assert_eq!(
        arguments.signed_word(C::lz32(), 1),
        Err(AbiError::InvalidArgumentWidth {
            index: 1,
            source: WordValueOutOfRange::new(u64::from(u32::MAX) + 1, WordWidth::W32),
        })
    );
    assert_eq!(arguments.signed_word(C::lz64(), 1), Ok(1 << 32));
}

#[test]
fn range_validation_is_width_safe_and_atomic() {
    for config in [C::lz32(), C::lz64()] {
        assert_eq!(validate_range(config, 0x1000, 0, 8), Ok(()));
        assert_eq!(validate_range(config, 0x1000, 8, 8), Ok(()));
        assert_eq!(
            validate_range(config, 0x1001, 8, 8),
            Err(AbiError::Misaligned {
                address: 0x1001,
                alignment: 8
            })
        );
        assert_eq!(
            validate_range(config, 0x1000, 8, 0),
            Err(AbiError::InvalidArgument { index: 0 })
        );
        assert_eq!(
            validate_range(config, 0x1000, 8, 3),
            Err(AbiError::InvalidArgument { index: 0 })
        );
    }

    assert_eq!(validate_range(C::lz32(), u64::from(u32::MAX), 0, 1), Ok(()));
    assert_eq!(
        validate_range(C::lz32(), u64::from(u32::MAX) - 7, 8, 1),
        Ok(())
    );
    assert_eq!(validate_range(C::lz32(), u64::from(u32::MAX), 1, 1), Ok(()));
    assert_eq!(
        validate_range(C::lz32(), u64::from(u32::MAX), 2, 1),
        Err(AbiError::Width(WidthError::AccessEndOutOfRange {
            base: u64::from(u32::MAX),
            size: 2,
            width: WordWidth::W32,
        }))
    );
    assert_eq!(
        validate_range(C::lz32(), 0, 1_u64 << 32, 1),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            1_u64 << 32,
            WordWidth::W32
        )))
    );
    assert_eq!(validate_range(C::lz64(), u64::MAX - 7, 8, 1), Ok(()));
    assert_eq!(validate_range(C::lz64(), u64::MAX, 0, 1), Ok(()));
    assert_eq!(validate_range(C::lz64(), u64::MAX, 1, 1), Ok(()));
    assert_eq!(
        validate_range(C::lz64(), u64::MAX, 2, 1),
        Err(AbiError::Width(WidthError::AccessEndOutOfRange {
            base: u64::MAX,
            size: 2,
            width: WordWidth::W64,
        }))
    );
}

#[test]
fn handles_flags_permissions_and_seek_origins_are_checked() {
    assert_eq!(FileHandle::new(0), Err(AbiError::InvalidHandle(0)));
    assert_eq!(FileHandle::new(1), Err(AbiError::InvalidHandle(1)));
    assert_eq!(FileHandle::new(7).map(FileHandle::get), Ok(7));
    assert_eq!(ProcessHandle::new(0), Err(AbiError::InvalidHandle(0)));
    assert_eq!(ProcessHandle::new(9).map(ProcessHandle::get), Ok(9));

    assert_eq!(OPEN_READ, 1);
    assert_eq!(OPEN_WRITE, 2);
    assert_eq!(OPEN_CREATE, 4);
    assert_eq!(OPEN_TRUNCATE, 8);
    assert_eq!(OPEN_ALL, 0x0f);
    for flags in 0..=OPEN_ALL {
        assert!(valid_open_flags(flags));
        assert_eq!(OpenFlags::new(flags).map(OpenFlags::bits), Ok(flags));
    }
    assert!(!valid_open_flags(0x10));
    assert!(!valid_open_flags(u32::MAX));
    assert_eq!(
        OpenFlags::new(0x10),
        Err(AbiError::InvalidArgument { index: 2 })
    );

    for permissions in 0..=7 {
        assert_eq!(
            FilePermissions::new(permissions).map(FilePermissions::bits),
            Ok(permissions)
        );
    }
    assert_eq!(
        FilePermissions::new(8),
        Err(AbiError::InvalidArgument { index: 0 })
    );

    for (raw, expected) in [
        (0, SeekOrigin::Start),
        (1, SeekOrigin::Current),
        (2, SeekOrigin::End),
    ] {
        assert_eq!(SeekOrigin::try_from(raw), Ok(expected));
    }
    for raw in [3, u32::MAX] {
        assert_eq!(
            SeekOrigin::try_from(raw),
            Err(AbiError::InvalidArgument { index: 3 })
        );
    }
}

#[test]
fn abi_validation_errors_map_to_stable_syscall_statuses() {
    let retained = AbiError::Width(WidthError::AccessEndOutOfRange {
        base: 1,
        size: 2,
        width: WordWidth::W32,
    });
    assert!(core::error::Error::source(&retained).is_some());
    let nested = AbiError::InvalidPointerWidth {
        index: 1,
        source: WordValueOutOfRange::new(1_u64 << 32, WordWidth::W32),
    };
    assert!(core::error::Error::source(&nested).is_some());
    let cases = [
        (AbiError::UnknownSyscall(9), SyscallError::UnknownSyscall),
        (AbiError::InvalidStatus(20), SyscallError::Internal),
        (AbiError::InvalidHandle(0), SyscallError::InvalidHandle),
        (
            AbiError::ReservedNonzero {
                field: "reserved",
                value: 1,
            },
            SyscallError::Internal,
        ),
        (
            AbiError::InvalidNameLength {
                length: 253,
                maximum: 252,
            },
            SyscallError::InvalidArgument,
        ),
        (
            AbiError::InvalidExitReason(4),
            SyscallError::InvalidArgument,
        ),
        (
            AbiError::InvalidPointer { index: 1 },
            SyscallError::InvalidPointer,
        ),
        (
            AbiError::InvalidPointerWidth {
                index: 1,
                source: WordValueOutOfRange::new(1_u64 << 32, WordWidth::W32),
            },
            SyscallError::InvalidPointer,
        ),
        (
            AbiError::InvalidArgument { index: 2 },
            SyscallError::InvalidArgument,
        ),
        (
            AbiError::InvalidArgumentWidth {
                index: 2,
                source: WordValueOutOfRange::new(1_u64 << 32, WordWidth::W32),
            },
            SyscallError::InvalidArgument,
        ),
        (
            AbiError::Misaligned {
                address: 1,
                alignment: 2,
            },
            SyscallError::Misaligned,
        ),
        (
            AbiError::RangeOverflow {
                address: 1,
                length: 2,
            },
            SyscallError::RangeOverflow,
        ),
        (
            AbiError::WordValueOutOfRange(WordValueOutOfRange::new(1_u64 << 32, WordWidth::W32)),
            SyscallError::RangeOverflow,
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(SyscallError::from(error), expected);
    }
}

#[test]
fn fixed_records_match_the_v1_little_endian_layouts() {
    assert_eq!(IO_RESULT_SIZE, 16);
    let io = IoResult::new(C::lz64(), 0x0102_0304_0506_0708, SyscallStatus::NotFound).unwrap();
    let encoded = io.encode();
    assert_eq!(encoded, [8, 7, 6, 5, 4, 3, 2, 1, 6, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(IoResult::decode(&encoded, C::lz64()), Ok(io));
    assert_eq!(
        IoResult::decode(&encoded, C::lz32()),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            0x0102_0304_0506_0708,
            WordWidth::W32
        )))
    );
    assert_eq!(
        IoResult::new(C::lz32(), u64::from(u32::MAX) + 1, SyscallStatus::Ok),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            u64::from(u32::MAX) + 1,
            WordWidth::W32
        )))
    );
    let mut reserved = encoded;
    reserved[12] = 1;
    assert_eq!(
        IoResult::decode(&reserved, C::lz64()),
        Err(AbiError::ReservedNonzero {
            field: "IoResult.reserved",
            value: 1
        })
    );
    assert_eq!(
        IoResult::decode(&encoded[..15], C::lz64()),
        Err(AbiError::InvalidArgument { index: 0 })
    );

    assert_eq!(FILE_STAT_SIZE, 16);
    let canonical = [2, 0, 0, 0, 5, 0, 0, 0, 42, 0, 0, 0, 0, 0, 0, 0];
    for config in [C::lz32(), C::lz64()] {
        let stat = FileStat::new(
            config,
            AbiFileKind::Directory,
            FilePermissions::new(5).unwrap(),
            42,
        )
        .unwrap();
        assert_eq!(stat.encode(), canonical);
        assert_eq!(FileStat::decode(&canonical, config), Ok(stat));
    }
    let mut invalid = canonical;
    invalid[0] = 3;
    assert_eq!(
        FileStat::decode(&invalid, C::lz64()),
        Err(AbiError::InvalidArgument { index: 0 })
    );
    invalid = canonical;
    invalid[4] = 8;
    assert_eq!(
        FileStat::decode(&invalid, C::lz64()),
        Err(AbiError::InvalidArgument { index: 0 })
    );
    assert_eq!(
        FileStat::new(
            C::lz32(),
            AbiFileKind::File,
            FilePermissions::new(0).unwrap(),
            u64::from(u32::MAX) + 1,
        ),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            u64::from(u32::MAX) + 1,
            WordWidth::W32
        )))
    );
}

#[test]
fn directory_records_encode_kind_in_the_documented_u16_field() {
    assert_eq!(DIRECTORY_RECORD_SIZE, 256);
    assert_eq!(DIRECTORY_NAME_CAPACITY, 252);
    let record = DirectoryRecord::new(b"init", AbiFileKind::File).unwrap();
    let encoded = record.encode();
    assert_eq!(&encoded[..8], &[4, 0, 1, 0, b'i', b'n', b'i', b't']);
    assert_eq!(&encoded[8..], &[0; 248]);
    assert_eq!(record.name(), b"init");
    assert_eq!(record.kind(), AbiFileKind::File);
    assert_eq!(DirectoryRecord::decode(&encoded), Ok(record));

    let overlong = [0; DIRECTORY_NAME_CAPACITY + 1];
    assert_eq!(
        DirectoryRecord::new(&overlong, AbiFileKind::Directory),
        Err(AbiError::InvalidNameLength {
            length: DIRECTORY_NAME_CAPACITY + 1,
            maximum: DIRECTORY_NAME_CAPACITY
        })
    );
    let mut invalid = encoded;
    invalid[..2].copy_from_slice(
        &u16::try_from(DIRECTORY_NAME_CAPACITY + 1)
            .unwrap()
            .to_le_bytes(),
    );
    assert_eq!(
        DirectoryRecord::decode(&invalid),
        Err(AbiError::InvalidNameLength {
            length: DIRECTORY_NAME_CAPACITY + 1,
            maximum: DIRECTORY_NAME_CAPACITY
        })
    );
    invalid = encoded;
    invalid[2] = 3;
    assert_eq!(
        DirectoryRecord::decode(&invalid),
        Err(AbiError::InvalidArgument { index: 0 })
    );
    assert_eq!(
        DirectoryRecord::decode(&encoded[..255]),
        Err(AbiError::InvalidArgument { index: 0 })
    );
}

#[test]
fn allocation_and_exit_records_round_trip_without_host_layout_dependence() {
    assert_eq!(MEMORY_ALLOCATION_SIZE, 16);
    let allocation =
        MemoryAllocation::new(C::lz64(), 0x0012_3456, u64::from(u32::MAX) + 9).unwrap();
    let encoded = allocation.encode();
    assert_eq!(&encoded[..11], &[0x56, 0x34, 0x12, 0, 0, 0, 0, 0, 8, 0, 0]);
    assert_eq!(allocation.address().as_u64(), 0x0012_3456);
    assert_eq!(
        MemoryAllocation::decode(&encoded, C::lz64()),
        Ok(allocation)
    );
    assert_eq!(
        MemoryAllocation::decode(&encoded[..15], C::lz64()),
        Err(AbiError::InvalidArgument { index: 0 })
    );
    assert_eq!(
        MemoryAllocation::decode(&encoded, C::lz32()),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            u64::from(u32::MAX) + 9,
            WordWidth::W32
        )))
    );
    assert_eq!(
        MemoryAllocation::new(C::lz32(), 0x1000, u64::from(u32::MAX) + 1),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            u64::from(u32::MAX) + 1,
            WordWidth::W32
        )))
    );
    assert_eq!(
        MemoryAllocation::new(C::lz32(), 0, 1_u64 << 32),
        Err(AbiError::WordValueOutOfRange(WordValueOutOfRange::new(
            1_u64 << 32,
            WordWidth::W32
        )))
    );
    assert!(MemoryAllocation::new(C::lz64(), u64::MAX - 7, 8).is_ok());
    assert_eq!(
        MemoryAllocation::new(C::lz64(), u64::MAX, 2),
        Err(AbiError::Width(WidthError::AccessEndOutOfRange {
            base: u64::MAX,
            size: 2,
            width: WordWidth::W64,
        }))
    );

    assert_eq!(EXIT_STATUS_RECORD_SIZE, 8);
    for (raw, reason) in [
        (0, ProcessExitReason::Normal),
        (1, ProcessExitReason::Killed),
        (2, ProcessExitReason::Faulted),
        (3, ProcessExitReason::Terminated),
    ] {
        assert_eq!(reason.as_u32(), raw);
        assert_eq!(ProcessExitReason::try_from(raw), Ok(reason));
    }
    assert_eq!(
        ProcessExitReason::try_from(4),
        Err(AbiError::InvalidExitReason(4))
    );
    let status = ExitStatusRecord::new(7, ProcessExitReason::Faulted);
    let encoded = status.encode();
    assert_eq!(encoded, [7, 0, 0, 0, 2, 0, 0, 0]);
    assert_eq!(status.reason(), ProcessExitReason::Faulted);
    assert_eq!(ExitStatusRecord::decode(&encoded), Ok(status));
    let mut invalid = encoded;
    invalid[4] = 4;
    assert_eq!(
        ExitStatusRecord::decode(&invalid),
        Err(AbiError::InvalidExitReason(4))
    );
    assert_eq!(
        ExitStatusRecord::decode(&encoded[..7]),
        Err(AbiError::InvalidArgument { index: 0 })
    );
}
