use lazalith_os::{
    LZX_CODE_PERMISSIONS, LZX_DATA_PERMISSIONS, LZX_HEADER_SIZE, LZX_MAGIC, LzxArchitecture,
    LzxError, LzxImage, LzxSection, LzxSectionKind, ProcessId, ThreadId, USER_DATA_START,
    USER_INITIAL_SP, USER_STACK_LENGTH,
};
use lazalith_types::PhysicalAddress;

fn image_with_sections(
    architecture: LzxArchitecture,
    code: &[u8],
    data: Option<(&[u8], u64)>,
    bss: Option<(u64, u64)>,
    required_data: u64,
) -> LzxImage {
    let mut sections = vec![LzxSection::code(code).unwrap()];
    if let Some((bytes, offset)) = data {
        sections.push(
            LzxSection::new(
                LzxSectionKind::Data,
                LZX_DATA_PERMISSIONS,
                offset,
                bytes.len() as u64,
                8,
                bytes,
            )
            .unwrap(),
        );
    }
    if let Some((size, offset)) = bss {
        sections.push(
            LzxSection::new(
                LzxSectionKind::Bss,
                LZX_DATA_PERMISSIONS,
                offset,
                size,
                8,
                &[],
            )
            .unwrap(),
        );
    }
    LzxImage::new(
        architecture,
        0,
        0,
        required_data,
        USER_STACK_LENGTH,
        sections,
    )
    .unwrap()
}

fn minimal_image() -> LzxImage {
    image_with_sections(LzxArchitecture::Lz64, &[0; 8], None, None, 0)
}

#[test]
fn lzx_v1_roundtrips_and_loads_owned_process_memory() {
    for architecture in [LzxArchitecture::Lz32, LzxArchitecture::Lz64] {
        let image = image_with_sections(
            architecture,
            &[1, 2, 3, 4, 5, 6, 7, 8],
            Some((b"data", 0x100)),
            Some((0x40, 0x200)),
            0x300,
        );
        let encoded = image.to_bytes().unwrap();
        let parsed = LzxImage::from_bytes(&encoded).unwrap();
        assert_eq!(parsed, image);
        assert_eq!(parsed.architecture(), architecture);
        assert_eq!(parsed.sections().len(), 3);
        assert_eq!(parsed.sections()[0].kind(), LzxSectionKind::Code);
        assert_eq!(parsed.sections()[1].bytes(), b"data");
        assert_eq!(parsed.sections()[2].virtual_size(), 0x40);

        let mut process = parsed
            .load_process(ProcessId::new(7).unwrap(), ThreadId::new(9).unwrap())
            .unwrap();
        assert_eq!(process.program().bytes(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        let mut data = [0u8; 4];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(USER_DATA_START + 0x100), &mut data)
            .unwrap();
        assert_eq!(&data, b"data");
        let mut bss = [0xffu8; 0x40];
        process
            .memory()
            .address_space()
            .peek(PhysicalAddress::new(USER_DATA_START + 0x200), &mut bss)
            .unwrap();
        assert_eq!(bss, [0; 0x40]);
        let allocation = process.memory_context().unwrap().allocate(8, 8).unwrap();
        assert!(allocation.address().as_u64() >= USER_DATA_START + 0x300);
        assert_eq!(
            process.primary_thread().cpu().sp().as_u64(),
            USER_INITIAL_SP
        );
    }
}

#[test]
fn lzx_allows_an_empty_data_section() {
    let image = image_with_sections(
        LzxArchitecture::Lz64,
        &[1, 2, 3, 4, 5, 6, 7, 8],
        Some((&[], 0x100)),
        None,
        0x100,
    );
    let parsed = LzxImage::from_bytes(&image.to_bytes().unwrap()).unwrap();
    let process = parsed
        .load_process(ProcessId::new(3).unwrap(), ThreadId::new(4).unwrap())
        .unwrap();
    assert_eq!(process.program().bytes(), &[1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn lzx_parser_rejects_malformed_headers_without_panicking() {
    let encoded = minimal_image().to_bytes().unwrap();
    for end in 0..encoded.len() {
        let _ = LzxImage::from_bytes(&encoded[..end]);
    }
    let mut bad_magic = encoded.clone();
    bad_magic[0] ^= 1;
    assert!(matches!(
        LzxImage::from_bytes(&bad_magic),
        Err(LzxError::InvalidMagic)
    ));
    let mut bad_version = encoded.clone();
    bad_version[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_version),
        Err(LzxError::UnsupportedFormat { version: 2 })
    ));
    let mut bad_architecture = encoded.clone();
    bad_architecture[12] = 3;
    assert!(matches!(
        LzxImage::from_bytes(&bad_architecture),
        Err(LzxError::UnsupportedArchitecture { value: 3 })
    ));
    let mut bad_flags = encoded.clone();
    bad_flags[13] = 1;
    assert!(matches!(
        LzxImage::from_bytes(&bad_flags),
        Err(LzxError::InvalidHeaderFlags { value: 1 })
    ));
    let mut bad_isa = encoded.clone();
    bad_isa[14..16].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_isa),
        Err(LzxError::UnsupportedIsaVersion { value: 2 })
    ));
    let mut bad_abi = encoded.clone();
    bad_abi[16..18].copy_from_slice(&2u16.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_abi),
        Err(LzxError::UnsupportedAbiVersion { value: 2 })
    ));
    let mut bad_count = encoded.clone();
    bad_count[18..20].copy_from_slice(&0u16.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_count),
        Err(LzxError::InvalidSectionCount { value: 0 })
    ));
    let mut bad_entry_section = encoded.clone();
    bad_entry_section[20..22].copy_from_slice(&1u16.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_entry_section),
        Err(LzxError::InvalidEntrySection { value: 1 })
    ));
    let mut bad_table = encoded.clone();
    bad_table[44..52].copy_from_slice(&65u64.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_table),
        Err(LzxError::InvalidSectionTableOffset { .. })
    ));
    let mut bad_table_size = encoded.clone();
    bad_table_size[52..56].copy_from_slice(&0u32.to_le_bytes());
    assert!(matches!(
        LzxImage::from_bytes(&bad_table_size),
        Err(LzxError::InvalidSectionTableSize { .. })
    ));
    let mut trailing = encoded.clone();
    trailing.push(0);
    assert!(matches!(
        LzxImage::from_bytes(&trailing),
        Err(LzxError::TrailingBytes { .. })
    ));
}

#[test]
fn lzx_semantic_validation_rejects_bad_sections_and_requirements() {
    assert!(matches!(
        LzxImage::new(
            LzxArchitecture::Lz64,
            0,
            1,
            0,
            USER_STACK_LENGTH,
            vec![LzxSection::code(&[0; 8]).unwrap()],
        ),
        Err(LzxError::InvalidEntryOffset { .. })
    ));
    assert!(matches!(
        LzxImage::new(
            LzxArchitecture::Lz64,
            0,
            0,
            0,
            USER_STACK_LENGTH - 1,
            vec![LzxSection::code(&[0; 8]).unwrap()],
        ),
        Err(LzxError::InvalidStackRequirement { .. })
    ));
    let overlapping = vec![
        LzxSection::code(&[0; 8]).unwrap(),
        LzxSection::new(
            LzxSectionKind::Data,
            LZX_DATA_PERMISSIONS,
            0x100,
            0x20,
            8,
            &[1; 0x20],
        )
        .unwrap(),
        LzxSection::new(
            LzxSectionKind::Bss,
            LZX_DATA_PERMISSIONS,
            0x110,
            0x20,
            8,
            &[],
        )
        .unwrap(),
    ];
    assert!(matches!(
        LzxImage::new(
            LzxArchitecture::Lz64,
            0,
            0,
            0x200,
            USER_STACK_LENGTH,
            overlapping,
        ),
        Err(LzxError::SectionVirtualOverlap { .. })
    ));
    let wrong_permissions = vec![
        LzxSection::new(LzxSectionKind::Code, LZX_DATA_PERMISSIONS, 0, 8, 4, &[0; 8]).unwrap(),
    ];
    assert!(matches!(
        LzxImage::new(
            LzxArchitecture::Lz64,
            0,
            0,
            0,
            USER_STACK_LENGTH,
            wrong_permissions,
        ),
        Err(LzxError::InvalidSectionPermissions { .. })
    ));
}

#[test]
fn lzx_file_header_and_section_layout_are_stable() {
    let image = minimal_image();
    let encoded = image.to_bytes().unwrap();
    assert_eq!(&encoded[..LZX_MAGIC.len()], &LZX_MAGIC);
    assert_eq!(u16::from_le_bytes([encoded[8], encoded[9]]), 1);
    assert_eq!(u16::from_le_bytes([encoded[10], encoded[11]]), 64);
    assert_eq!(encoded[12], 2);
    assert_eq!(u16::from_le_bytes([encoded[18], encoded[19]]), 1);
    assert_eq!(
        u32::from_le_bytes([encoded[52], encoded[53], encoded[54], encoded[55]]),
        48
    );
    assert_eq!(u64::from_le_bytes(encoded[56..64].try_into().unwrap()), 112);
    assert_eq!(encoded[LZX_HEADER_SIZE], LzxSectionKind::Code as u8);
    assert_eq!(encoded[LZX_HEADER_SIZE + 1], LZX_CODE_PERMISSIONS);
}
