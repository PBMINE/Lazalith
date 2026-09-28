//! Hardening: a refused memory access leaves memory exactly as it was.
//!
//! The contract is the one `docs/os-memory.md` states: a memory validates the whole
//! access before it changes anything. It is the contract a whole class of bugs hides
//! behind, because a half-completed store that writes the bytes it *can* write and
//! then faults looks like a working program until something reads what it left behind.
//!
//! So each case here fails in a different way a memory can fail — past the end of a
//! region, spanning a gap, into unmapped space, into read-only memory, as a user into
//! supervisor-only memory, past the last address in the machine, at an unaligned
//! address, at a width the machine does not have — and then checks *every* byte of the
//! regions involved rather than the one near the failure, because a partial write a few
//! bytes away is precisely what this is looking for.
//!
//! The boundary cases matter as much as the failures. A memory that refused the last
//! valid word in a region would fail exactly these tests, so each region is also
//! written to its very end successfully, which pins the boundary from both sides.

use lazalith_cpu::{DataAccess, DataAccessError, DataAccessKind, Privilege};
use lazalith_memory::{
    AddressSpace, Bus, DataSize, MemoryFault, MemoryFaultKind, MemoryRegion,
    RegionPermissions as RP,
};
use lazalith_types::{ArchitectureConfig as C, PhysicalAddress as P, VirtualAddress as V};

fn bus(config: C, regions: Vec<MemoryRegion>) -> Bus {
    let mut bus = Bus::new(AddressSpace::new(config));
    for region in regions {
        bus.map(region).expect("the region maps");
    }
    bus
}

fn ram(config: C, start: u64, length: u64, permissions: RP) -> MemoryRegion {
    MemoryRegion::ram(config, P::new(start), length, permissions).expect("a ram region")
}

fn open() -> RP {
    RP::new(true, true, true, true)
}

fn access(config: C, address: u64, size: DataSize, kind: DataAccessKind) -> DataAccess {
    DataAccess::new(
        config,
        V::new(address),
        0,
        size,
        kind,
        Privilege::Supervisor,
    )
    .expect("a well-formed access")
}

fn store(
    config: C,
    bus: &mut Bus,
    address: u64,
    size: DataSize,
    value: u64,
) -> Result<(), MemoryFault> {
    bus.write_data(access(config, address, size, DataAccessKind::Write), value)
}

/// The bytes of a region, read back the way a debugger would.
fn bytes_of(bus: &Bus, start: u64, length: usize) -> Vec<u8> {
    let mut out = vec![0u8; length];
    bus.peek(P::new(start), &mut out)
        .expect("the region is mapped and readable");
    out
}

#[test]
fn a_store_past_the_end_of_a_region_changes_nothing() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, vec![ram(config, 0, 64, open())]);
        bus.initialize(P::new(0), &[0xAAu8; 64]).unwrap();
        // The last word that *does* fit, which must succeed. If this failed, the
        // region would be one word smaller than it says it is.
        let last = 64 - u64::from(DataSize::Word.bytes());
        store(config, &mut bus, last, DataSize::Word, 0x1122_3344)
            .unwrap_or_else(|error| panic!("the final word of a region must be writable: {error}"));
        // The baseline is taken *after* that store, so what the refused store must
        // leave alone is the memory as it is now, not as it started.
        let before = bytes_of(&bus, 0, 64);
        // One word later, which must not.
        let error = store(config, &mut bus, last + 8, DataSize::Word, 0xFFFF_FFFF)
            .expect_err("a word starting past the end must be refused");
        assert!(
            matches!(
                error.kind,
                MemoryFaultKind::CrossRegion { .. } | MemoryFaultKind::Unmapped
            ),
            "the fault should be about the address, got {:?}",
            error.kind
        );
        assert_eq!(
            before,
            bytes_of(&bus, 0, 64),
            "the refused store changed memory"
        );
        // The last word still holds what the successful store put there, so the
        // refused one did not reach backwards into it either.
        assert_eq!(
            &before[60..64],
            &[0x44, 0x33, 0x22, 0x11],
            "the final word was not the one the store wrote"
        );
    }
}

#[test]
fn a_store_spanning_two_regions_changes_neither() {
    // The interesting one: part of the word is inside a region and part of it is in
    // the gap after. A store that wrote what it could before noticing would leave a
    // word that never existed in either region.
    //
    // The region is 14 bytes rather than a round number because a *four-byte* access
    // that is properly aligned cannot cross a region whose length is a multiple of
    // four: the last word inside it ends exactly at the boundary. A length of 14
    // puts a boundary in the middle of a word, which is the only way to reach the
    // case with an aligned access.
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(
            config,
            vec![ram(config, 0, 14, open()), ram(config, 64, 16, open())],
        );
        bus.initialize(P::new(0), &[0x77u8; 14]).unwrap();
        bus.initialize(P::new(64), &[0x88u8; 16]).unwrap();
        let before_first = bytes_of(&bus, 0, 14);
        let before_second = bytes_of(&bus, 64, 16);
        let error = store(config, &mut bus, 12, DataSize::Word, 0xFFFF_FFFF)
            .expect_err("a word that ends in a gap must be refused whole");
        assert!(
            matches!(error.kind, MemoryFaultKind::CrossRegion { .. }),
            "crossing a region is its own fault, got {:?}",
            error.kind
        );
        assert_eq!(
            before_first,
            bytes_of(&bus, 0, 14),
            "the two bytes that were inside the region were written anyway"
        );
        assert_eq!(before_second, bytes_of(&bus, 64, 16));
    }
}

#[test]
fn a_store_into_unmapped_space_changes_nothing() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, vec![ram(config, 0, 64, open())]);
        bus.initialize(P::new(0), &[0x5Au8; 64]).unwrap();
        let before = bytes_of(&bus, 0, 64);
        let error = store(config, &mut bus, 0x8000, DataSize::Word, 0xFFFF_FFFF)
            .expect_err("a store into unmapped space must be refused");
        assert!(
            matches!(error.kind, MemoryFaultKind::Unmapped),
            "got {:?}",
            error.kind
        );
        assert_eq!(
            before,
            bytes_of(&bus, 0, 64),
            "a refused store changed memory"
        );
    }
}

#[test]
fn a_store_into_read_only_memory_changes_nothing() {
    for config in [C::lz32(), C::lz64()] {
        let region = MemoryRegion::rom(
            config,
            P::new(0),
            &[0x33u8; 32],
            RP::new(true, false, true, true),
        )
        .expect("a read-only region");
        let mut bus = bus(config, vec![region]);
        let before = bytes_of(&bus, 0, 32);
        let error = store(config, &mut bus, 8, DataSize::Word, 0xDEAD_BEEF)
            .expect_err("a read-only region must refuse a store");
        assert!(
            matches!(error.kind, MemoryFaultKind::Permission { .. }),
            "the bus should refuse on permissions, got {:?}",
            error.kind
        );
        assert_eq!(
            before,
            bytes_of(&bus, 0, 32),
            "a refused store wrote anyway"
        );
        // And a read still works, so the refusal is about writing and not about the
        // region having gone away.
        let read = bus
            .read_data(access(config, 8, DataSize::Word, DataAccessKind::Read))
            .expect("a read-only region must still be readable");
        assert_eq!(read, 0x3333_3333, "and it reads back what it was given");
    }
}

#[test]
fn a_user_store_into_supervisor_only_memory_changes_nothing() {
    // The permission that is easiest to get wrong, because the region is perfectly
    // readable and writable — just not by this privilege.
    let config = C::lz64();
    let mut bus = bus(
        config,
        vec![ram(config, 0, 32, RP::new(true, true, true, false))],
    );
    bus.initialize(P::new(0), &[0xC3u8; 32]).unwrap();
    let before = bytes_of(&bus, 0, 32);
    let user = DataAccess::new(
        config,
        V::new(8),
        0,
        DataSize::Word,
        DataAccessKind::Write,
        Privilege::User,
    )
    .expect("a well-formed access");
    let error = bus
        .write_data(user, 0xFFFF_FFFF)
        .expect_err("a user must not write supervisor memory");
    assert!(
        matches!(error.kind, MemoryFaultKind::Permission { .. }),
        "got {:?}",
        error.kind
    );
    assert_eq!(before, bytes_of(&bus, 0, 32));
    // The same store from the supervisor is allowed, so the region is not simply
    // read-only and the test above is not passing for the wrong reason.
    store(config, &mut bus, 8, DataSize::Word, 0x4444_4444).expect("the supervisor may write");
    assert_eq!(bytes_of(&bus, 0, 32)[8], 0x44);
}

#[test]
fn a_store_past_the_last_address_is_refused_rather_than_wrapped() {
    // A store whose *end* leaves the address space is the case that would be
    // catastrophic if it wrapped: the bytes would land at low addresses, over
    // whatever is there. This pins that the address arithmetic is checked rather
    // than truncated.
    for config in [C::lz32(), C::lz64()] {
        let last = config.word_width().mask();
        // Even a single byte at the last address is fine, because the last address
        // is the last *byte*.
        let access = DataAccess::new(
            config,
            V::new(last),
            0,
            DataSize::Byte,
            DataAccessKind::Write,
            Privilege::Supervisor,
        );
        assert!(
            access.is_ok(),
            "the last address holds one byte, as it should"
        );
        for size in [DataSize::Word, DataSize::Double] {
            if !config.supports_data_size(size.bytes()) {
                continue;
            }
            let error = DataAccess::new(
                config,
                V::new(last),
                0,
                size,
                DataAccessKind::Write,
                Privilege::Supervisor,
            )
            .expect_err("a wider access at the last address must be refused");
            assert!(
                matches!(error, DataAccessError::Width(_)),
                "a wider access past the end is a width error, got {error:?}"
            );
        }
    }
}

#[test]
fn a_displacement_that_leaves_the_address_space_is_refused() {
    // The same overflow, reached through the displacement field the instruction
    // decoder actually uses, rather than through a hand-built address.
    let config = C::lz64();
    let last = config.word_width().mask();
    let error = DataAccess::new(
        config,
        V::new(last),
        8,
        DataSize::Byte,
        DataAccessKind::Write,
        Privilege::Supervisor,
    )
    .expect_err("base plus displacement must be checked");
    assert!(matches!(error, DataAccessError::Width(_)), "got {error:?}");
    // A negative displacement from a low address is the other direction, and it is
    // refused by the same check rather than wrapping to a huge address.
    let error = DataAccess::new(
        config,
        V::new(4),
        -8,
        DataSize::Byte,
        DataAccessKind::Read,
        Privilege::Supervisor,
    )
    .expect_err("a negative displacement below zero must be refused");
    assert!(matches!(error, DataAccessError::Width(_)), "got {error:?}");
}

#[test]
fn an_unaligned_access_is_refused_before_it_can_reach_memory() {
    for config in [C::lz32(), C::lz64()] {
        for (address, size) in [
            (1u64, DataSize::Word),
            (2, DataSize::Word),
            (4, DataSize::Double),
            (3, DataSize::Half),
        ] {
            if !config.supports_data_size(size.bytes()) {
                continue;
            }
            let error = DataAccess::new(
                config,
                V::new(address),
                0,
                size,
                DataAccessKind::Write,
                Privilege::Supervisor,
            )
            .expect_err("an unaligned access must be refused where it is built");
            assert!(
                matches!(error, DataAccessError::Alignment { .. }),
                "unaligned {size:?} at {address}: {error:?}"
            );
        }
    }
}

#[test]
fn a_width_the_machine_does_not_have_is_refused_where_it_is_built() {
    // A 32-bit machine has no eight-byte access. The refusal has to happen where the
    // access is constructed, so no caller can ever hand the store something the
    // machine has no encoding for.
    assert!(
        !C::lz32().supports_data_size(8),
        "an lz32 machine has no double access"
    );
    let error = DataAccess::new(
        C::lz32(),
        V::new(0),
        0,
        DataSize::Double,
        DataAccessKind::Write,
        Privilege::Supervisor,
    )
    .expect_err("an lz32 machine must refuse an eight-byte access");
    assert!(
        matches!(error, DataAccessError::InvalidWidth { .. }),
        "got {error:?}"
    );
    for config in [C::lz32(), C::lz64()] {
        for size in [DataSize::Byte, DataSize::Half, DataSize::Word] {
            assert!(
                DataAccess::new(
                    config,
                    V::new(0),
                    0,
                    size,
                    DataAccessKind::Read,
                    Privilege::Supervisor,
                )
                .is_ok(),
                "{config:?} must support {size:?}"
            );
        }
    }
}

#[test]
fn an_overlapping_region_is_refused_and_leaves_the_first_intact() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, vec![ram(config, 0, 32, open())]);
        bus.initialize(P::new(0), &[0x42u8; 32]).unwrap();
        let error = bus
            .map(ram(config, 16, 32, open()))
            .expect_err("an overlapping region must be refused");
        assert!(
            matches!(error.kind, MemoryFaultKind::Overlap { .. }),
            "got {:?}",
            error.kind
        );
        assert!(
            bytes_of(&bus, 0, 32).iter().all(|byte| *byte == 0x42),
            "the refused mapping disturbed the region that was already there"
        );
        // The region is still exactly the size it says it is: a refused map must not
        // have shortened it to make room.
        assert_eq!(bus.address_space().regions()[0].length(), 32);
    }
}

#[test]
fn a_region_mapped_exactly_where_the_previous_one_ends_is_accepted() {
    // The other side of the overlap boundary. A memory that treated abutting regions
    // as overlapping would be unusable, and one that allowed overlap would be wrong.
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, vec![ram(config, 0, 32, open())]);
        bus.map(ram(config, 32, 32, open()))
            .expect("an abutting region must map");
        bus.initialize(P::new(0), &[0x11u8; 32]).unwrap();
        bus.initialize(P::new(32), &[0x22u8; 32]).unwrap();
        assert!(bytes_of(&bus, 0, 32).iter().all(|byte| *byte == 0x11));
        assert!(bytes_of(&bus, 32, 32).iter().all(|byte| *byte == 0x22));
    }
}

#[test]
fn a_rom_cannot_be_built_writable() {
    // The only way to get read-only memory is `MemoryRegion::rom`, and it refuses a
    // writable permission set at construction — before a bus, a machine, or a program
    // can see it. If this check lived in the bus instead, a caller could build a
    // writable ROM, hand it to something that checked nothing, and write it.
    for config in [C::lz32(), C::lz64()] {
        let error = MemoryRegion::rom(config, P::new(0), &[0u8; 8], open())
            .expect_err("a writable rom must be refused");
        assert!(
            matches!(error.kind, MemoryFaultKind::WritableRom { .. }),
            "got {:?}",
            error.kind
        );
    }
}
