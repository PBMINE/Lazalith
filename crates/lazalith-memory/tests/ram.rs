use lazalith_memory::{
    AccessSize, AccessType, AddressSpace, DataSize, MemoryAddress, MemoryFaultKind, MemoryRegion,
    RegionPermissions,
};
use lazalith_types::{ArchitectureConfig, PhysicalAddress as P, VirtualAddress as V};
use std::error::Error;

const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);

#[test]
fn mappings_are_disjoint_inclusive_and_mode_checked() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let mut space = AddressSpace::new(config);
        space
            .map(MemoryRegion::ram(config, P::new(16), 16, RW).unwrap())
            .unwrap();
        for (base, len) in [(16, 16), (15, 2), (31, 2), (0, 64), (20, 1)] {
            let error = space
                .map(MemoryRegion::ram(config, P::new(base), len, RW).unwrap())
                .unwrap_err();
            assert!(matches!(error.kind, MemoryFaultKind::Overlap { .. }));
            assert_eq!(space.regions().len(), 1);
        }
        space
            .map(MemoryRegion::ram(config, P::new(32), 1, RW).unwrap())
            .unwrap();
        space
            .map(MemoryRegion::ram(config, P::new(15), 1, RW).unwrap())
            .unwrap();
        space
            .map(MemoryRegion::ram(config, P::new(config.word_width().mask()), 1, RW).unwrap())
            .unwrap();
        assert_eq!(space.regions()[0].start(), P::new(16));
        assert_eq!(space.regions()[0].end(), P::new(31));
        assert_eq!(space.regions()[0].permissions(), RW);
        assert_eq!(space.config(), config);
    }
    let region = MemoryRegion::ram(ArchitectureConfig::lz64(), P::new(1 << 32), 1, RW).unwrap();
    assert!(
        AddressSpace::new(ArchitectureConfig::lz32())
            .map(region)
            .is_err()
    );
}

#[test]
fn invalid_ranges_and_allocations_retain_causes() {
    for (config, base, len) in [
        (ArchitectureConfig::lz32(), 1 << 32, 1),
        (ArchitectureConfig::lz32(), u32::MAX as u64, 2),
        (ArchitectureConfig::lz64(), u64::MAX, 2),
        (ArchitectureConfig::lz64(), 0, 0),
    ] {
        let error = MemoryRegion::ram(config, P::new(base), len, RW).unwrap_err();
        assert_eq!(error.address, MemoryAddress::Physical(P::new(base)));
        assert_eq!(error.access, AccessType::Map);
        assert_eq!(error.size, AccessSize::Bytes(len));
        assert!(matches!(error.kind, MemoryFaultKind::Width(_)));
        assert!(error.source().unwrap().source().is_some());
    }
    let error = MemoryRegion::ram(ArchitectureConfig::lz64(), P::new(0), u64::MAX, RW).unwrap_err();
    assert!(matches!(
        error.kind,
        MemoryFaultKind::Allocation(_) | MemoryFaultKind::HostSize(_)
    ));
    assert!(error.source().unwrap().source().is_some());
}

#[test]
fn translation_is_explicit_identity_without_masking() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let space = AddressSpace::new(config);
        for address in [0, 1, config.word_width().mask()] {
            assert_eq!(
                space.translate_identity(V::new(address)).unwrap(),
                P::new(address)
            );
        }
    }
    assert!(
        AddressSpace::new(ArchitectureConfig::lz32())
            .translate_identity(V::new(1 << 32))
            .is_err()
    );
    assert_eq!(AccessSize::Instruction.bytes(), 8);
    for &size in DataSize::ALL {
        assert_eq!(AccessSize::Data(size).bytes(), u64::from(size.bytes()));
    }
}

#[test]
fn loader_cannot_split_even_adjacent_regions() {
    let config = ArchitectureConfig::lz64();
    let mut space = AddressSpace::new(config);
    for base in [0, 4] {
        space
            .map(MemoryRegion::ram(config, P::new(base), 4, RW).unwrap())
            .unwrap();
    }
    let error = space.initialize(P::new(3), &[1, 2]).unwrap_err();
    assert!(matches!(error.kind, MemoryFaultKind::CrossRegion { .. }));
    assert!(matches!(
        space.initialize(P::new(8), &[1]).unwrap_err().kind,
        MemoryFaultKind::Unmapped
    ));
}
