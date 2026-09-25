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
        assert!(
            LzxImage::from_bytes(&encoded[..end]).is_err(),
            "a {end}-byte prefix must not be accepted"
        );
    }
    for extra in 1..8usize {
        let mut extended = encoded.clone();
        extended.extend(core::iter::repeat_n(0u8, extra));
        assert!(
            LzxImage::from_bytes(&extended).is_err(),
            "trailing bytes must be rejected"
        );
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

fn set_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn lzx_rejects_every_unreachable_header_and_table_field() {
    let image = image_with_sections(
        LzxArchitecture::Lz64,
        &[0; 16],
        Some((&[1, 2, 3, 4], 0)),
        Some((16, 8)),
        32,
    );
    let canonical = image.to_bytes().unwrap();
    let record = LZX_HEADER_SIZE;

    let mut header_size = canonical.clone();
    set_u16(&mut header_size, 10, 63);
    assert!(matches!(
        LzxImage::from_bytes(&header_size),
        Err(LzxError::InvalidHeaderSize { .. })
    ));

    let mut header_reserved = canonical.clone();
    header_reserved[26] = 1;
    assert!(matches!(
        LzxImage::from_bytes(&header_reserved),
        Err(LzxError::InvalidReserved { .. })
    ));

    let mut section_count = canonical.clone();
    set_u16(&mut section_count, 18, 4);
    assert!(matches!(
        LzxImage::from_bytes(&section_count),
        Err(LzxError::InvalidSectionCount { .. })
    ));

    let mut table_offset = canonical.clone();
    set_u64(&mut table_offset, 44, 8);
    assert!(matches!(
        LzxImage::from_bytes(&table_offset),
        Err(LzxError::InvalidSectionTableOffset { .. })
    ));

    let mut table_size = canonical.clone();
    set_u32(&mut table_size, 52, 47);
    assert!(matches!(
        LzxImage::from_bytes(&table_size),
        Err(LzxError::InvalidSectionTableSize { .. })
    ));

    let mut required_data = canonical.clone();
    set_u64(&mut required_data, 28, 8);
    assert!(matches!(
        LzxImage::from_bytes(&required_data),
        Err(LzxError::InvalidMemoryRequirement { .. })
    ));

    let mut required_stack = canonical.clone();
    set_u64(&mut required_stack, 36, USER_STACK_LENGTH + 8);
    assert!(matches!(
        LzxImage::from_bytes(&required_stack),
        Err(LzxError::InvalidStackRequirement { .. })
    ));

    let mut entry_section = canonical.clone();
    set_u16(&mut entry_section, 20, 1);
    assert!(matches!(
        LzxImage::from_bytes(&entry_section),
        Err(LzxError::InvalidEntrySection { .. })
    ));

    let mut entry_offset = canonical.clone();
    set_u32(&mut entry_offset, 22, 2);
    assert!(LzxImage::from_bytes(&entry_offset).is_err());

    let mut section_reserved = canonical.clone();
    section_reserved[record + 2] = 1;
    assert!(matches!(
        LzxImage::from_bytes(&section_reserved),
        Err(LzxError::InvalidReserved { .. })
    ));

    let mut section_kind = canonical.clone();
    section_kind[record] = 9;
    assert!(matches!(
        LzxImage::from_bytes(&section_kind),
        Err(LzxError::InvalidSectionKind { .. })
    ));

    let mut section_alignment = canonical.clone();
    set_u64(&mut section_alignment, record + 36, 3);
    assert!(matches!(
        LzxImage::from_bytes(&section_alignment),
        Err(LzxError::InvalidSectionAlignment { .. })
    ));

    let mut code_offset = canonical.clone();
    set_u64(&mut code_offset, record + 4, 8);
    assert!(matches!(
        LzxImage::from_bytes(&code_offset),
        Err(LzxError::InvalidCodeSection { .. })
    ));

    let mut code_size = canonical.clone();
    set_u64(
        &mut code_size,
        record + 12,
        lazalith_os::USER_CODE_LENGTH + 8,
    );
    assert!(LzxImage::from_bytes(&code_size).is_err());

    let mut bss_file_bytes = canonical.clone();
    set_u64(&mut bss_file_bytes, record + 2 * 48 + 28, 4);
    assert!(matches!(
        LzxImage::from_bytes(&bss_file_bytes),
        Err(LzxError::SectionRange { .. })
    ));
    let mut bss_virtual = canonical.clone();
    set_u64(&mut bss_virtual, record + 2 * 48 + 12, 0);
    assert!(matches!(
        LzxImage::from_bytes(&bss_virtual),
        Err(LzxError::InvalidSectionSize { .. })
    ));
    let mut data_short = canonical.clone();
    set_u64(&mut data_short, record + 48 + 12, 2);
    assert!(matches!(
        LzxImage::from_bytes(&data_short),
        Err(LzxError::InvalidSectionSize { .. })
    ));

    let mut too_large = vec![0u8; 4 * 1024 * 1024 + 1];
    too_large[..canonical.len()].copy_from_slice(&canonical);
    assert!(matches!(
        LzxImage::from_bytes(&too_large),
        Err(LzxError::FileTooLarge { .. })
    ));

    assert_eq!(LzxImage::from_bytes(&canonical).unwrap(), image);
}
