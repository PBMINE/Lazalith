use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_toolchain::{
    CodeMapping, DebugSource, OBJECT_MAGIC, ObjectBuilder, ObjectFile, Relocation, RelocationKind,
    Section, Symbol, SymbolBinding,
};
use lazalith_types::ArchitectureConfig;

fn object(config: ArchitectureConfig) -> ObjectFile {
    let nop = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();
    let syscall = encode(
        config,
        &Instruction::new(config, Opcode::Syscall, &[]).unwrap(),
    )
    .unwrap();
    let mut code = Vec::new();
    code.extend_from_slice(&nop);
    code.extend_from_slice(&syscall);
    let mut builder = ObjectBuilder::new(config);
    let text = builder
        .add_section(Section::text("text", config, &code).unwrap())
        .unwrap();
    let _rodata = builder
        .add_section(Section::read_only_data("rodata", 4, &[1, 2, 3, 4]).unwrap())
        .unwrap();
    let data = builder
        .add_section(Section::data("data", 4, &[0; 4]).unwrap())
        .unwrap();
    builder
        .add_section(Section::bss("bss", 4, 16).unwrap())
        .unwrap();
    let entry = builder
        .add_symbol(Symbol::section_defined(
            "_start",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    builder.set_entry(entry).unwrap();
    let external = builder
        .add_symbol(Symbol::undefined("external", SymbolBinding::Global))
        .unwrap();
    builder
        .add_symbol(Symbol::absolute("answer", SymbolBinding::Global, 42))
        .unwrap();
    builder
        .add_symbol(Symbol::section_defined(
            "data_value",
            SymbolBinding::Global,
            data,
            0,
            4,
        ))
        .unwrap();
    builder
        .add_relocation(Relocation::new(
            external,
            data,
            RelocationKind::AbsoluteWord32,
            0,
            0,
        ))
        .unwrap();
    let source = builder
        .add_debug_source(DebugSource::new("object.lzs", "line one\nline two\n"))
        .unwrap();
    builder
        .add_debug_mapping(CodeMapping::new(text, 0, source, 0, 1))
        .unwrap();
    builder.build().unwrap()
}

fn padding_object() -> ObjectFile {
    let config = ArchitectureConfig::lz64();
    let instruction = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();
    let mut builder = ObjectBuilder::new(config);
    let text = builder
        .add_section(Section::text("text", config, &instruction).unwrap())
        .unwrap();
    builder
        .add_section(Section::data("before-zero", 1, &[3]).unwrap())
        .unwrap();
    builder
        .add_section(Section::bss("zero", 8, 32).unwrap())
        .unwrap();
    builder
        .add_section(Section::data("after-zero", 1, &[7]).unwrap())
        .unwrap();
    let entry = builder
        .add_symbol(Symbol::section_defined(
            "start",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    builder.set_entry(entry).unwrap();
    builder.build().unwrap()
}

#[test]
fn lzo_round_trip_preserves_sections_symbols_relocations_and_debug() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let object = object(config);
        let bytes = object.to_bytes().unwrap();
        assert_eq!(&bytes[..8], &OBJECT_MAGIC);
        let symbol_record = 384;
        assert_eq!(
            u32::from_le_bytes(bytes[symbol_record..symbol_record + 4].try_into().unwrap()),
            22
        );
        assert_eq!(bytes[symbol_record + 4], 2);
        assert_eq!(bytes[symbol_record + 5], 0);
        assert_eq!(
            u16::from_le_bytes(
                bytes[symbol_record + 8..symbol_record + 10]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        assert_eq!(
            u64::from_le_bytes(
                bytes[symbol_record + 16..symbol_record + 24]
                    .try_into()
                    .unwrap()
            ),
            0
        );
        let decoded = ObjectFile::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, object);
        assert_eq!(decoded.to_bytes().unwrap(), bytes);
        assert_eq!(decoded.sections().len(), 4);
        assert_eq!(decoded.symbols().len(), 4);
        assert_eq!(decoded.relocations().len(), 1);
        assert_eq!(decoded.debug_mappings().len(), 1);
    }
}

#[test]
fn lzo_header_offsets_and_padding_are_deterministic() {
    let bytes = object(ArchitectureConfig::lz64()).to_bytes().unwrap();
    assert_eq!(u64::from_le_bytes(bytes[48..56].try_into().unwrap()), 128);
    assert_eq!(u64::from_le_bytes(bytes[56..64].try_into().unwrap()), 384);
    assert_eq!(u64::from_le_bytes(bytes[64..72].try_into().unwrap()), 512);
    assert_eq!(u64::from_le_bytes(bytes[72..80].try_into().unwrap()), 544);
    assert_eq!(u64::from_le_bytes(bytes[80..88].try_into().unwrap()), 560);
    assert_eq!(u64::from_le_bytes(bytes[88..96].try_into().unwrap()), 584);
    let payload = u64::from_le_bytes(bytes[96..104].try_into().unwrap());
    assert_eq!(payload % 8, 0);

    let object = padding_object();
    let bytes = object.to_bytes().unwrap();
    assert_eq!(ObjectFile::from_bytes(&bytes).unwrap(), object);
}

#[test]
fn lzo_parser_rejects_malformed_and_truncated_inputs() {
    let object = object(ArchitectureConfig::lz64());
    let bytes = object.to_bytes().unwrap();
    for end in 0..bytes.len() {
        assert!(ObjectFile::from_bytes(&bytes[..end]).is_err());
    }
    let mut bad_magic = bytes.clone();
    bad_magic[0] ^= 1;
    assert!(ObjectFile::from_bytes(&bad_magic).is_err());
    let mut bad_reserved = bytes.clone();
    bad_reserved[20] = 1;
    assert!(ObjectFile::from_bytes(&bad_reserved).is_err());
    let mut bss_bytes = bytes;
    let bss_record = 128 + 3 * 64;
    let bss_offset = (bss_bytes.len().div_ceil(8) * 8) as u64;
    bss_bytes.resize(bss_offset as usize + 1, 0);
    bss_bytes[bss_record + 24..bss_record + 32].copy_from_slice(&bss_offset.to_le_bytes());
    bss_bytes[bss_record + 32..bss_record + 40].copy_from_slice(&1u64.to_le_bytes());
    assert!(ObjectFile::from_bytes(&bss_bytes).is_err());
}

#[test]
fn repeated_local_names_round_trip_without_string_budget_rejection() {
    let config = ArchitectureConfig::lz64();
    let instruction = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();
    let mut builder = ObjectBuilder::new(config);
    let text = builder
        .add_section(Section::text("text", config, &instruction).unwrap())
        .unwrap();
    let mut entry = None;
    for _ in 0..20 {
        let symbol = builder
            .add_symbol(Symbol::section_defined(
                "a".repeat(1000),
                SymbolBinding::Local,
                text,
                0,
                0,
            ))
            .unwrap();
        entry = Some(symbol);
    }
    builder.set_entry(entry.unwrap()).unwrap();
    let object = builder.build().unwrap();
    let bytes = object.to_bytes().unwrap();
    assert_eq!(ObjectFile::from_bytes(&bytes).unwrap(), object);
}

#[test]
fn step44_bridge_rejects_relocation_bearing_objects() {
    let object = object(ArchitectureConfig::lz64());
    assert!(lazalith_toolchain::link_object(&object).is_err());
}

fn header_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn header_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn set_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn set_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn lzo_rejects_non_canonical_tables_reserved_words_and_counts() {
    let canonical = object(ArchitectureConfig::lz64()).to_bytes().unwrap();
    let string_offset = header_u64(&canonical, 88) as usize;
    let string_size = header_u32(&canonical, 44) as usize;

    let mut trailing = canonical.clone();
    set_u32(&mut trailing, 44, (string_size + 1) as u32);
    trailing[string_offset + string_size] = b'x';
    assert!(
        ObjectFile::from_bytes(&trailing).is_err(),
        "an unreferenced trailing string byte is not canonical"
    );

    for field in [104u64, 112, 120] {
        let mut reserved = canonical.clone();
        set_u64(&mut reserved, field as usize, 1);
        assert!(ObjectFile::from_bytes(&reserved).is_err(), "field {field}");
    }

    let mut wrong_table = canonical.clone();
    set_u64(&mut wrong_table, 48, header_u64(&canonical, 48) + 8);
    assert!(ObjectFile::from_bytes(&wrong_table).is_err());

    let mut wrong_string_table = canonical.clone();
    set_u64(&mut wrong_string_table, 88, header_u64(&canonical, 88) + 8);
    assert!(ObjectFile::from_bytes(&wrong_string_table).is_err());

    let mut wrong_payload = canonical.clone();
    set_u64(&mut wrong_payload, 96, header_u64(&canonical, 96) + 8);
    assert!(ObjectFile::from_bytes(&wrong_payload).is_err());

    let short_payload = canonical.clone();
    let payload = header_u64(&canonical, 96) as usize;
    let truncated = &short_payload[..payload + 8];
    assert!(ObjectFile::from_bytes(truncated).is_err());
    let mut extra_payload = canonical.clone();
    extra_payload.push(0);
    assert!(ObjectFile::from_bytes(&extra_payload).is_err());
}

#[test]
fn lzo_rejects_duplicate_names_and_out_of_order_relocations() {
    let config = ArchitectureConfig::lz64();
    let nop = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();

    let mut duplicate_section = ObjectBuilder::new(config);
    let text = duplicate_section
        .add_section(Section::text("text", config, &nop).unwrap())
        .unwrap();
    duplicate_section
        .add_section(Section::read_only_data("text", 4, &[0]).unwrap())
        .unwrap();
    let entry = duplicate_section
        .add_symbol(Symbol::section_defined(
            "_start",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    duplicate_section.set_entry(entry).unwrap();
    assert!(matches!(
        duplicate_section.build(),
        Err(lazalith_toolchain::ObjectError::DuplicateSection { .. })
    ));

    let mut duplicate_symbol = ObjectBuilder::new(config);
    let text = duplicate_symbol
        .add_section(Section::text("text", config, &nop).unwrap())
        .unwrap();
    let entry = duplicate_symbol
        .add_symbol(Symbol::section_defined(
            "same",
            SymbolBinding::Global,
            text,
            0,
            0,
        ))
        .unwrap();
    duplicate_symbol.set_entry(entry).unwrap();
    duplicate_symbol
        .add_symbol(Symbol::absolute("same", SymbolBinding::Global, 1))
        .unwrap();
    assert!(matches!(
        duplicate_symbol.build(),
        Err(lazalith_toolchain::ObjectError::InvalidSymbol { .. })
    ));

    let mut unordered = ObjectBuilder::new(config);
    let mut code = nop.to_vec();
    code.extend_from_slice(&nop);
    let text = unordered
        .add_section(Section::text("text", config, &code).unwrap())
        .unwrap();
    let local = unordered
        .add_symbol(Symbol::section_defined(
            "local",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    unordered.set_entry(local).unwrap();
    unordered
        .add_relocation(Relocation::new(
            local,
            text,
            RelocationKind::LiImmediate,
            8,
            0,
        ))
        .unwrap();
    unordered
        .add_relocation(Relocation::new(
            local,
            text,
            RelocationKind::LiImmediate,
            0,
            0,
        ))
        .unwrap();
    assert!(
        unordered.build().is_err(),
        "relocations must be strictly ordered by target offset"
    );
}

/// The source text travels inside the object, not beside it.
///
/// A mapping is an offset into text, so an object that carried only the mapping
/// would need the text to mean anything. This states that the bytes on disk hold
/// the text: the object's own encoding contains the source it was built from.
#[test]
fn an_object_embeds_the_source_its_mappings_are_offsets_into() {
    let object = object(ArchitectureConfig::lz64());
    let bytes = object.to_bytes().unwrap();
    let text = b"line one\nline two\n";
    assert!(
        bytes.windows(text.len()).any(|window| window == text),
        "the object file holds the source text its mappings are offsets into"
    );
    let read_back = ObjectFile::from_bytes(&bytes).unwrap();
    assert_eq!(read_back.debug_sources().len(), 1);
    assert_eq!(read_back.debug_sources()[0].path(), "object.lzs");
    assert_eq!(read_back.debug_sources()[0].text(), "line one\nline two\n");
}

/// Two objects with different sources keep their sources apart.
#[test]
fn two_objects_keep_their_own_sources() {
    let config = ArchitectureConfig::lz64();
    let nop = encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap();
    let mut first = ObjectBuilder::new(config);
    let text = first
        .add_section(Section::text("text", config, &nop).unwrap())
        .unwrap();
    let entry = first
        .add_symbol(Symbol::section_defined(
            "_start",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    first.set_entry(entry).unwrap();
    let one = first
        .add_debug_source(DebugSource::new("one.lzs", "one\n"))
        .unwrap();
    first
        .add_debug_mapping(CodeMapping::new(text, 0, one, 0, 3))
        .unwrap();
    let first = first.build().unwrap();

    let mut second = ObjectBuilder::new(config);
    let text = second
        .add_section(Section::text("text", config, &nop).unwrap())
        .unwrap();
    let entry = second
        .add_symbol(Symbol::section_defined(
            "_start",
            SymbolBinding::Local,
            text,
            0,
            0,
        ))
        .unwrap();
    second.set_entry(entry).unwrap();
    let two = second
        .add_debug_source(DebugSource::new("two.lzs", "two two\n"))
        .unwrap();
    second
        .add_debug_mapping(CodeMapping::new(text, 0, two, 0, 7))
        .unwrap();
    let second = second.build().unwrap();

    let linked = lazalith_toolchain::link_objects(
        &[first.clone(), second.clone()],
        &lazalith_toolchain::LinkOptions::default(),
    )
    .expect("the objects link");
    let debug = linked.debug();
    let names: Vec<&str> = debug.files().iter().map(|file| file.name()).collect();
    assert!(
        names.contains(&"one.lzs") && names.contains(&"two.lzs"),
        "both sources survived the link: {names:?}"
    );
    // And the merged table still resolves both objects' code, which is the point
    // of the linker's fix-up: a mapping left at its object-relative offset would
    // resolve to whichever function happened to be laid out there, and a mapping
    // that named the wrong file would send a frontend to the wrong source.
    let mut one_addresses = Vec::new();
    let mut two_addresses = Vec::new();
    for entry in debug.entries() {
        let located = debug.resolve(entry.address).expect("every entry resolves");
        match located.name {
            "one.lzs" => {
                assert_eq!(located.line_number(), 1, "and to the line it mapped");
                one_addresses.push(entry.address);
            }
            "two.lzs" => {
                assert_eq!(located.line_number(), 1);
                two_addresses.push(entry.address);
            }
            other => panic!("a mapping resolved to a file neither object had: {other}"),
        }
    }
    assert_eq!(
        one_addresses.len(),
        1,
        "the first object's mapping survived"
    );
    assert_eq!(
        two_addresses.len(),
        1,
        "and the second object's did too, rather than one replacing the other"
    );
    assert!(
        one_addresses[0] != two_addresses[0],
        "the two objects' code is at different addresses, and each mapping \
         stayed with its own"
    );
}
