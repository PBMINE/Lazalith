use lazalith_cpu::DataAccessError;
use lazalith_memory::{
    AccessSize, AccessType, AddressSpace, DataAccessKind as K, DataSize as S, MemoryAddress,
    MemoryFaultKind as F, MemoryRegion, Privilege as U, RegionPermissions,
};
use lazalith_types::{
    ArchitectureConfig as C, InstructionAddress as I, PhysicalAddress as P, VirtualAddress as V,
    WidthError,
};
use std::error::Error;

#[test]
fn invalid_data_retains_inputs_effective_address_and_priority() {
    let space = AddressSpace::new(C::lz32());
    let cases = [
        (u64::MAX, 1, S::Double),
        (1 << 32, -1, S::Word),
        (0, -1, S::Word),
        (u32::MAX as u64, 0, S::Word),
        (0, 1, S::Word),
    ];
    for (index, (base, displacement, size)) in cases.into_iter().enumerate() {
        let error = space
            .data_access(V::new(base), displacement, size, K::Write, U::User)
            .unwrap_err()
            .with_pc(I::new(0x100));
        assert_eq!(error.access, AccessType::Data(K::Write));
        assert_eq!(error.size, AccessSize::Data(size));
        assert_eq!(error.pc, Some(I::new(0x100)));
        assert_eq!(error.privilege, Some(U::User));
        let F::InvalidDataAccess {
            base: retained,
            displacement: delta,
            source,
        } = &error.kind
        else {
            panic!("{error}")
        };
        assert_eq!(*retained, V::new(base));
        assert_eq!(*delta, displacement);
        match (index, source) {
            (0, DataAccessError::InvalidWidth { .. })
            | (1, DataAccessError::Width(WidthError::InvalidOffsetBase { .. }))
            | (2, DataAccessError::Width(WidthError::AddressOffsetOutOfRange { .. }))
            | (3, DataAccessError::Width(WidthError::AccessEndOutOfRange { .. }))
            | (4, DataAccessError::Alignment { .. }) => {}
            _ => panic!("{error}"),
        }
        assert_eq!(
            error.address,
            MemoryAddress::Virtual(V::new(if index == 4 { 1 } else { base }))
        );
        assert!(
            error
                .source()
                .unwrap()
                .source()
                .unwrap()
                .is::<DataAccessError>()
        );
        assert!(error.to_string().contains("PC"));
    }
}

#[test]
fn fault_chains_integrate_with_shared_diagnostics() {
    let space = AddressSpace::new(C::lz64());
    let fault = space
        .data_access(V::new(u64::MAX), 0, S::Double, K::Read, U::Supervisor)
        .unwrap_err();
    let diagnostic = lazalith_diagnostics::Diagnostic::new(
        lazalith_diagnostics::Severity::Error,
        lazalith_diagnostics::DiagnosticCode::new("E2001").unwrap(),
        "memory failure",
    )
    .with_cause(fault);
    let memory = diagnostic.source().unwrap();
    assert!(memory.is::<lazalith_memory::MemoryFault>());
    let kind = memory.source().unwrap();
    assert!(kind.is::<F>());
    let data = kind.source().unwrap();
    assert!(data.is::<DataAccessError>());
    assert!(data.source().unwrap().is::<WidthError>());
}

#[test]
fn fetch_faults_retain_pc_privilege_and_full_instruction_size() {
    for config in [C::lz32(), C::lz64()] {
        let space = AddressSpace::new(config);
        for address in [2, config.word_width().mask() - 3, 0] {
            let error = space
                .fetch_instruction(config, I::new(address), U::User)
                .unwrap_err();
            assert_eq!(error.address, MemoryAddress::Instruction(I::new(address)));
            assert_eq!(error.pc, Some(I::new(address)));
            assert_eq!(error.privilege, Some(U::User));
            assert_eq!(error.access, AccessType::Fetch);
            assert_eq!(error.size, AccessSize::Instruction);
            if address == 2 {
                assert!(matches!(error.kind, F::Control(_)));
            } else if address == 0 {
                assert!(matches!(error.kind, F::Unmapped));
            } else {
                assert!(matches!(
                    error.kind,
                    F::Width(WidthError::AccessEndOutOfRange { .. })
                ));
            }
        }
    }
}

#[test]
fn exact_high_address_boundaries_never_wrap() {
    for config in [C::lz32(), C::lz64()] {
        let max = config.word_width().mask();
        let mut space = AddressSpace::new(config);
        space
            .map(
                MemoryRegion::ram(
                    config,
                    P::new(max - 15),
                    16,
                    RegionPermissions::new(true, true, true, true),
                )
                .unwrap(),
            )
            .unwrap();
        let a = space
            .data_access(V::new(max), 0, S::Byte, K::Write, U::Supervisor)
            .unwrap();
        space.write_data(a, 0xab).unwrap();
        let a = space
            .data_access(V::new(max), 0, S::Byte, K::Read, U::Supervisor)
            .unwrap();
        assert_eq!(space.read_data(a).unwrap(), 0xab);
        assert_eq!(
            space
                .fetch_instruction(config, I::new(max - 7), U::Supervisor)
                .unwrap(),
            [0, 0, 0, 0, 0, 0, 0, 0xab]
        );
        assert!(
            space
                .data_access(V::new(max), 1, S::Byte, K::Read, U::Supervisor)
                .is_err()
        );
        assert!(
            space
                .data_access(V::new(max), 0, S::Half, K::Read, U::Supervisor)
                .is_err()
        );
        assert!(
            space
                .fetch_instruction(config, I::new(max - 3), U::Supervisor)
                .is_err()
        );
    }
}

#[test]
fn mapping_precedes_permissions_and_failed_writes_preserve_bytes() {
    let config = C::lz64();
    let mut space = AddressSpace::new(config);
    space
        .map(
            MemoryRegion::ram(
                config,
                P::new(0),
                6,
                RegionPermissions::new(false, false, false, false),
            )
            .unwrap(),
        )
        .unwrap();
    for (address, expected) in [(0, 0), (4, 1), (8, 2)] {
        let a = space
            .data_access(V::new(address), 0, S::Word, K::Write, U::User)
            .unwrap();
        let error = space.write_data(a, u64::MAX).unwrap_err();
        assert_eq!(error.address, MemoryAddress::Virtual(V::new(address)));
        assert_eq!(error.size, AccessSize::Data(S::Word));
        assert_eq!(error.privilege, Some(U::User));
        assert_eq!(error.pc, None);
        assert!(matches!(
            (expected, error.kind),
            (0, F::Permission { .. }) | (1, F::CrossRegion { .. }) | (2, F::Unmapped)
        ));
    }
    let mut bytes = [0xaa; 6];
    space.peek(P::new(0), &mut bytes).unwrap();
    assert_eq!(bytes, [0; 6]);
}
