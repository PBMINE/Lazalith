use lazalith_memory::{
    AddressSpace, DataAccess, DataAccessKind as K, DataSize, MemoryFaultKind, MemoryRegion,
    Privilege, RegionPermissions,
};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress as I, PhysicalAddress as P, VirtualAddress as V,
};

fn space(config: C, permissions: RegionPermissions) -> AddressSpace {
    let mut space = AddressSpace::new(config);
    space
        .map(MemoryRegion::ram(config, P::new(0), 32, permissions).unwrap())
        .unwrap();
    space
}

fn access(config: C, address: u64, size: DataSize, kind: K, privilege: Privilege) -> DataAccess {
    DataAccess::new(config, V::new(address), 0, size, kind, privilege).unwrap()
}

#[test]
fn every_data_size_is_little_endian_zero_extended_and_truncated() {
    for config in [C::lz32(), C::lz64()] {
        for &size in DataSize::ALL {
            if !config.supports_data_size(size.bytes()) {
                continue;
            }
            let mut space = space(config, RegionPermissions::new(true, true, false, true));
            for privilege in [Privilege::User, Privilege::Supervisor] {
                let value = 0xfedc_ba98_7654_3210u64;
                space
                    .write_data(access(config, 8, size, K::Write, privilege), value)
                    .unwrap();
                let mut actual = [0; 32];
                space.peek(P::new(0), &mut actual).unwrap();
                let mut expected = [0; 32];
                expected[8..8 + usize::from(size.bytes())]
                    .copy_from_slice(&value.to_le_bytes()[..usize::from(size.bytes())]);
                assert_eq!(actual, expected);
                let mask = u64::MAX >> (64 - size.bytes() * 8);
                assert_eq!(
                    space
                        .read_data(access(config, 8, size, K::Read, privilege))
                        .unwrap(),
                    value & mask
                );
            }
        }
    }
}

#[test]
fn fetch_is_pure_eight_bytes_four_aligned_execute_not_read() {
    for config in [C::lz32(), C::lz64()] {
        let mut space = space(config, RegionPermissions::new(false, true, true, true));
        space
            .initialize(P::new(4), &[1, 2, 3, 4, 5, 6, 7, 8])
            .unwrap();
        for _ in 0..2 {
            assert_eq!(
                space
                    .fetch_instruction(config, I::new(4), Privilege::User)
                    .unwrap(),
                [1, 2, 3, 4, 5, 6, 7, 8]
            );
        }
        assert!(
            space
                .read_data(access(
                    config,
                    4,
                    DataSize::Word,
                    K::Read,
                    Privilege::Supervisor
                ))
                .is_err()
        );
        assert!(
            space
                .fetch_instruction(config, I::new(2), Privilege::Supervisor)
                .is_err()
        );
        assert!(
            space
                .fetch_instruction(config, I::new(28), Privilege::Supervisor)
                .is_err()
        );
        space
            .write_data(
                access(config, 4, DataSize::Word, K::Write, Privilege::Supervisor),
                0x44332211,
            )
            .unwrap();
        assert_eq!(
            space
                .fetch_instruction(config, I::new(4), Privilege::Supervisor)
                .unwrap(),
            [0x11, 0x22, 0x33, 0x44, 5, 6, 7, 8]
        );
    }
}

#[test]
fn all_permission_combinations_separate_user_and_supervisor() {
    let config = C::lz64();
    for bits in 0..16 {
        let permissions =
            RegionPermissions::new(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0);
        for privilege in [Privilege::Supervisor, Privilege::User] {
            let mut space = space(config, permissions);
            let user_ok = privilege == Privilege::Supervisor || permissions.user;
            assert_eq!(
                space
                    .fetch_instruction(config, I::new(0), privilege)
                    .is_ok(),
                permissions.execute && user_ok
            );
            assert_eq!(
                space
                    .read_data(access(config, 0, DataSize::Word, K::Read, privilege))
                    .is_ok(),
                permissions.read && user_ok
            );
            assert_eq!(
                space
                    .write_data(access(config, 0, DataSize::Word, K::Write, privilege), 17)
                    .is_ok(),
                permissions.write && user_ok
            );
            let mut bytes = [0; 4];
            space.peek(P::new(0), &mut bytes).unwrap();
            assert_eq!(
                bytes,
                if permissions.write && user_ok {
                    [17, 0, 0, 0]
                } else {
                    [0; 4]
                }
            );
        }
    }
}

#[test]
fn debugger_peek_bypasses_permissions_and_alignment_but_not_bounds() {
    let config = C::lz64();
    let mut space = space(config, RegionPermissions::new(false, false, false, false));
    space.initialize(P::new(1), &[7, 8, 9]).unwrap();
    for _ in 0..2 {
        let mut bytes = [0; 3];
        space.peek(P::new(1), &mut bytes).unwrap();
        assert_eq!(bytes, [7, 8, 9]);
    }
    let mut bytes = [0xaa; 3];
    for base in [31, 32, u64::MAX] {
        assert!(space.peek(P::new(base), &mut bytes).is_err());
        assert_eq!(bytes, [0xaa; 3]);
    }
    assert!(space.peek(P::new(0), &mut []).is_err());
}

#[test]
fn operation_kinds_and_word_sized_pure_stack_are_enforced() {
    for config in [C::lz32(), C::lz64()] {
        let mut space = space(config, RegionPermissions::new(true, true, true, true));
        let size = if config.word_bytes() == 4 {
            DataSize::Word
        } else {
            DataSize::Double
        };
        for kind in [K::Read, K::Write, K::StackRead, K::StackWrite] {
            let a = access(config, 8, size, kind, Privilege::Supervisor);
            assert_eq!(space.read_data(a).is_ok(), kind == K::Read);
            assert_eq!(
                space.write_data(a, 12).is_ok(),
                matches!(kind, K::Write | K::StackWrite)
            );
            assert_eq!(space.peek_stack(a).is_ok(), kind == K::StackRead);
        }
        let mut before = [0; 32];
        space.peek(P::new(0), &mut before).unwrap();
        for _ in 0..2 {
            assert_eq!(
                space
                    .peek_stack(access(config, 8, size, K::StackRead, Privilege::Supervisor))
                    .unwrap(),
                12
            );
        }
        assert!(matches!(
            space
                .write_data(
                    access(
                        config,
                        8,
                        DataSize::Byte,
                        K::StackWrite,
                        Privilege::Supervisor
                    ),
                    99
                )
                .unwrap_err()
                .kind,
            MemoryFaultKind::StackSize
        ));
        let mut after = [0; 32];
        space.peek(P::new(0), &mut after).unwrap();
        assert_eq!(before, after);
    }
}

#[test]
fn complete_access_cannot_cross_adjacent_regions_or_change_mode() {
    let config = C::lz64();
    let mut space = AddressSpace::new(config);
    for (base, len) in [(0, 6), (6, 10)] {
        space
            .map(
                MemoryRegion::ram(
                    config,
                    P::new(base),
                    len,
                    RegionPermissions::new(true, true, true, true),
                )
                .unwrap(),
            )
            .unwrap();
    }
    assert!(matches!(
        space
            .write_data(
                access(config, 4, DataSize::Word, K::Write, Privilege::Supervisor),
                u64::MAX
            )
            .unwrap_err()
            .kind,
        MemoryFaultKind::CrossRegion { .. }
    ));
    assert!(
        space
            .fetch_instruction(config, I::new(0), Privilege::Supervisor)
            .is_err()
    );
    for (base, len) in [(0, 6), (6, 10)] {
        let mut bytes = vec![1; len];
        space.peek(P::new(base), &mut bytes).unwrap();
        assert!(bytes.iter().all(|b| *b == 0));
    }
    assert!(matches!(
        space
            .read_data(access(
                C::lz32(),
                0,
                DataSize::Word,
                K::Read,
                Privilege::Supervisor
            ))
            .unwrap_err()
            .kind,
        MemoryFaultKind::Configuration { .. }
    ));
}
