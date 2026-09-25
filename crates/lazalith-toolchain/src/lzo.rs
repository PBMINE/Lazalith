use crate::{
    CodeMapping, DebugSource, DebugSourceIndex, OBJECT_DEBUG_MAPPING_ENTRY_SIZE,
    OBJECT_DEBUG_SOURCE_ENTRY_SIZE, OBJECT_FORMAT_VERSION, OBJECT_HEADER_SIZE, OBJECT_MAGIC,
    OBJECT_MAX_DEBUG_MAPPINGS, OBJECT_MAX_DEBUG_SOURCES, OBJECT_MAX_FILE_SIZE,
    OBJECT_MAX_MATERIALIZED_NAME_BYTES, OBJECT_MAX_RELOCATIONS, OBJECT_MAX_SECTIONS,
    OBJECT_MAX_SYMBOLS, OBJECT_NO_ENTRY, OBJECT_RELOCATION_ENTRY_SIZE, OBJECT_SECTION_ENTRY_SIZE,
    OBJECT_SYMBOL_ENTRY_SIZE, ObjectError, ObjectFile, ObjectTarget, Relocation, RelocationKind,
    Section, SectionIndex, SectionKind, Symbol, SymbolBinding, SymbolIndex, SymbolKind,
};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use lazalith_types::ArchitectureConfig;

pub(crate) fn encode(object: &ObjectFile) -> Result<Vec<u8>, ObjectError> {
    object.validate()?;
    let section_count = count_u16(object.sections().len(), "section")?;
    let symbol_count = count_u32(object.symbols().len(), "symbol")?;
    let relocation_count = count_u32(object.relocations().len(), "relocation")?;
    let debug_source_count = count_u32(object.debug_sources().len(), "debug source")?;
    let debug_mapping_count = count_u32(object.debug_mappings().len(), "debug mapping")?;
    let mut strings = StringTable::new()?;
    for section in object.sections() {
        strings.intern(section.name())?;
    }
    for symbol in object.symbols() {
        strings.intern(symbol.name())?;
    }
    for source in object.debug_sources() {
        strings.intern(source.path())?;
    }

    let section_offset = OBJECT_HEADER_SIZE as u64;
    let symbol_offset = add_offset(
        section_offset,
        u64::from(section_count),
        OBJECT_SECTION_ENTRY_SIZE,
        "section",
    )?;
    let relocation_offset = add_offset(
        symbol_offset,
        u64::from(symbol_count),
        OBJECT_SYMBOL_ENTRY_SIZE,
        "symbol",
    )?;
    let debug_source_offset = add_offset(
        relocation_offset,
        u64::from(relocation_count),
        OBJECT_RELOCATION_ENTRY_SIZE,
        "relocation",
    )?;
    let debug_mapping_offset = add_offset(
        debug_source_offset,
        u64::from(debug_source_count),
        OBJECT_DEBUG_SOURCE_ENTRY_SIZE,
        "debug source",
    )?;
    let string_offset = add_offset(
        debug_mapping_offset,
        u64::from(debug_mapping_count),
        OBJECT_DEBUG_MAPPING_ENTRY_SIZE,
        "debug mapping",
    )?;
    let string_size =
        u64::try_from(strings.bytes.len()).map_err(|_| ObjectError::InvalidCount {
            table: "string",
            value: strings.bytes.len() as u64,
        })?;
    let string_end = checked_add(string_offset, string_size, "string")?;
    let payload_offset = align_up(string_end, 8)?;

    let mut payload_offsets = Vec::new();
    payload_offsets
        .try_reserve_exact(object.sections().len())
        .map_err(ObjectError::Allocation)?;
    let mut cursor = payload_offset;
    for section in object.sections() {
        let file_size = section.file_size();
        if file_size == 0 {
            payload_offsets.push(0);
        } else {
            cursor = align_up(cursor, 8)?;
            payload_offsets.push(cursor);
            cursor = checked_add(cursor, file_size, "section payload")?;
        }
    }
    let total = usize::try_from(cursor).map_err(|_| ObjectError::InvalidCount {
        table: "file",
        value: cursor,
    })?;
    if total > OBJECT_MAX_FILE_SIZE {
        return Err(ObjectError::InvalidCount {
            table: "file",
            value: cursor,
        });
    }
    let mut section_symbol_counts = Vec::new();
    let mut section_relocation_counts = Vec::new();
    section_symbol_counts
        .try_reserve_exact(object.sections().len())
        .map_err(ObjectError::Allocation)?;
    section_relocation_counts
        .try_reserve_exact(object.sections().len())
        .map_err(ObjectError::Allocation)?;
    for _ in 0..object.sections().len() {
        section_symbol_counts.push(0u32);
        section_relocation_counts.push(0u32);
    }
    for symbol in object.symbols() {
        if let Some(section) = symbol.section() {
            let count = section_symbol_counts
                .get_mut(section.get() as usize)
                .ok_or(ObjectError::InvalidSymbol {
                    index: 0,
                    reason: "section index is out of range",
                })?;
            *count = count.checked_add(1).ok_or(ObjectError::InvalidCount {
                table: "symbol",
                value: u64::MAX,
            })?;
        }
    }
    for relocation in object.relocations() {
        let count = section_relocation_counts
            .get_mut(relocation.section().get() as usize)
            .ok_or(ObjectError::InvalidRelocation {
                index: 0,
                reason: "section index is out of range",
            })?;
        *count = count.checked_add(1).ok_or(ObjectError::InvalidCount {
            table: "relocation",
            value: u64::MAX,
        })?;
    }
    let mut output = Vec::new();
    output
        .try_reserve_exact(total)
        .map_err(ObjectError::Allocation)?;
    put_bytes(&mut output, &OBJECT_MAGIC);
    put_u16(&mut output, OBJECT_FORMAT_VERSION);
    put_u16(&mut output, OBJECT_HEADER_SIZE as u16);
    put_u8(
        &mut output,
        match object.config().word_width() {
            lazalith_types::WordWidth::W32 => 1,
            lazalith_types::WordWidth::W64 => 2,
        },
    );
    put_u8(&mut output, 0);
    put_u16(&mut output, object.isa_version());
    put_u16(&mut output, object.abi_version());
    put_u16(&mut output, section_count);
    put_u16(&mut output, 0);
    put_u16(&mut output, 0);
    put_u32(&mut output, symbol_count);
    put_u32(&mut output, relocation_count);
    put_u32(&mut output, debug_source_count);
    put_u32(&mut output, debug_mapping_count);
    put_u32(
        &mut output,
        object.entry().map_or(OBJECT_NO_ENTRY, |entry| entry.get()),
    );
    put_u32(
        &mut output,
        u32::try_from(strings.bytes.len()).map_err(|_| ObjectError::InvalidCount {
            table: "string",
            value: strings.bytes.len() as u64,
        })?,
    );
    put_u64(&mut output, section_offset);
    put_u64(&mut output, symbol_offset);
    put_u64(&mut output, relocation_offset);
    put_u64(&mut output, debug_source_offset);
    put_u64(&mut output, debug_mapping_offset);
    put_u64(&mut output, string_offset);
    put_u64(&mut output, payload_offset);
    put_u64(&mut output, 0);
    put_u64(&mut output, 0);
    put_u64(&mut output, 0);

    for (index, section) in object.sections().iter().enumerate() {
        let symbol_count_for_section = section_symbol_counts[index];
        let relocation_count_for_section = section_relocation_counts[index];
        put_u32(
            &mut output,
            strings
                .offset(section.name())
                .ok_or(ObjectError::InvalidString { offset: 0 })?,
        );
        put_u8(&mut output, section.kind() as u8);
        put_u8(&mut output, 0);
        put_u16(&mut output, 0);
        put_u64(&mut output, section.alignment());
        put_u64(&mut output, section.size());
        put_u64(&mut output, payload_offsets[index]);
        put_u64(&mut output, section.file_size());
        put_u32(&mut output, symbol_count_for_section);
        put_u32(&mut output, relocation_count_for_section);
        put_u64(&mut output, 0);
        put_u64(&mut output, 0);
    }
    for symbol in object.symbols() {
        put_u32(
            &mut output,
            strings
                .offset(symbol.name())
                .ok_or(ObjectError::InvalidString { offset: 0 })?,
        );
        put_u8(&mut output, symbol.kind() as u8);
        put_u8(&mut output, symbol.binding() as u8);
        put_u16(&mut output, 0);
        put_u16(
            &mut output,
            symbol.section().map_or(u16::MAX, |section| section.get()),
        );
        put_u16(&mut output, 0);
        put_u32(&mut output, 0);
        put_u64(&mut output, symbol.value());
        put_u64(&mut output, symbol.size());
    }
    for relocation in object.relocations() {
        put_u32(&mut output, relocation.symbol().get());
        put_u16(&mut output, relocation.section().get());
        put_u8(&mut output, relocation.kind() as u8);
        put_u8(&mut output, 0);
        put_u64(&mut output, relocation.offset());
        put_i64(&mut output, relocation.addend());
        put_u32(&mut output, 0);
        put_u32(&mut output, 0);
    }
    for source in object.debug_sources() {
        put_u32(
            &mut output,
            strings
                .offset(source.path())
                .ok_or(ObjectError::InvalidString { offset: 0 })?,
        );
        put_u32(&mut output, source.length());
        put_u64(&mut output, 0);
    }
    for mapping in object.debug_mappings() {
        put_u16(&mut output, mapping.section().get());
        put_u16(&mut output, 0);
        put_u64(&mut output, mapping.offset());
        put_u32(&mut output, mapping.source().get());
        put_u32(&mut output, mapping.source_offset());
        put_u32(&mut output, mapping.source_length());
    }
    output.extend_from_slice(&strings.bytes);
    output.resize(payload_offset as usize, 0);
    for (index, section) in object.sections().iter().enumerate() {
        if section.file_size() != 0 {
            output.resize(payload_offsets[index] as usize, 0);
            output.extend_from_slice(section.bytes());
        }
    }
    debug_assert_eq!(output.len(), total);
    Ok(output)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<ObjectFile, ObjectError> {
    if bytes.len() > OBJECT_MAX_FILE_SIZE {
        return Err(ObjectError::InvalidCount {
            table: "file",
            value: bytes.len() as u64,
        });
    }
    let mut reader = Reader::new(bytes);
    if reader.take(8)? != OBJECT_MAGIC {
        return Err(ObjectError::InvalidMagic);
    }
    let version = reader.u16()?;
    if version != OBJECT_FORMAT_VERSION {
        return Err(ObjectError::UnsupportedFormat(version));
    }
    let header_size = reader.u16()?;
    if header_size != OBJECT_HEADER_SIZE as u16 {
        return Err(ObjectError::InvalidHeaderSize(header_size));
    }
    let architecture = reader.u8()?;
    let flags = reader.u8()?;
    if flags != 0 {
        return Err(ObjectError::InvalidFlags(flags));
    }
    let isa_version = reader.u16()?;
    let abi_version = reader.u16()?;
    let section_count = reader.u16()?;
    let reserved = reader.u16()?;
    if reserved != 0 {
        return Err(ObjectError::ReservedField { offset: 20 });
    }
    let reserved = reader.u16()?;
    if reserved != 0 {
        return Err(ObjectError::ReservedField { offset: 22 });
    }
    let symbol_count = reader.u32()?;
    let relocation_count = reader.u32()?;
    let debug_source_count = reader.u32()?;
    let debug_mapping_count = reader.u32()?;
    let entry_raw = reader.u32()?;
    let string_size = reader.u32()?;
    let section_offset = reader.u64()?;
    let symbol_offset = reader.u64()?;
    let relocation_offset = reader.u64()?;
    let debug_source_offset = reader.u64()?;
    let debug_mapping_offset = reader.u64()?;
    let string_offset = reader.u64()?;
    let payload_offset = reader.u64()?;
    for offset in [104u64, 112, 120] {
        if reader.u64()? != 0 {
            return Err(ObjectError::ReservedField { offset });
        }
    }
    let target = ObjectTarget::from_wire(architecture, isa_version, abi_version)?;
    let section_count_usize = usize::from(section_count);
    let symbol_count_usize =
        usize::try_from(symbol_count).map_err(|_| ObjectError::InvalidCount {
            table: "symbol",
            value: symbol_count as u64,
        })?;
    let relocation_count_usize =
        usize::try_from(relocation_count).map_err(|_| ObjectError::InvalidCount {
            table: "relocation",
            value: relocation_count as u64,
        })?;
    let debug_source_count_usize =
        usize::try_from(debug_source_count).map_err(|_| ObjectError::InvalidCount {
            table: "debug source",
            value: debug_source_count as u64,
        })?;
    let debug_mapping_count_usize =
        usize::try_from(debug_mapping_count).map_err(|_| ObjectError::InvalidCount {
            table: "debug mapping",
            value: debug_mapping_count as u64,
        })?;
    if section_count_usize > OBJECT_MAX_SECTIONS
        || symbol_count_usize > OBJECT_MAX_SYMBOLS
        || relocation_count_usize > OBJECT_MAX_RELOCATIONS
        || debug_source_count_usize > OBJECT_MAX_DEBUG_SOURCES
        || debug_mapping_count_usize > OBJECT_MAX_DEBUG_MAPPINGS
    {
        return Err(ObjectError::InvalidCount {
            table: "object",
            value: section_count as u64,
        });
    }
    let expected_section = OBJECT_HEADER_SIZE as u64;
    let expected_symbol = add_offset(
        expected_section,
        section_count_usize as u64,
        OBJECT_SECTION_ENTRY_SIZE,
        "section",
    )?;
    let expected_relocation = add_offset(
        expected_symbol,
        symbol_count_usize as u64,
        OBJECT_SYMBOL_ENTRY_SIZE,
        "symbol",
    )?;
    let expected_debug_source = add_offset(
        expected_relocation,
        relocation_count_usize as u64,
        OBJECT_RELOCATION_ENTRY_SIZE,
        "relocation",
    )?;
    let expected_debug_mapping = add_offset(
        expected_debug_source,
        debug_source_count_usize as u64,
        OBJECT_DEBUG_SOURCE_ENTRY_SIZE,
        "debug source",
    )?;
    let expected_string = add_offset(
        expected_debug_mapping,
        debug_mapping_count_usize as u64,
        OBJECT_DEBUG_MAPPING_ENTRY_SIZE,
        "debug mapping",
    )?;
    let expected_payload = align_up(
        checked_add(expected_string, string_size as u64, "string")?,
        8,
    )?;
    check_table_offset("section", section_offset, expected_section)?;
    check_table_offset("symbol", symbol_offset, expected_symbol)?;
    check_table_offset("relocation", relocation_offset, expected_relocation)?;
    check_table_offset("debug source", debug_source_offset, expected_debug_source)?;
    check_table_offset(
        "debug mapping",
        debug_mapping_offset,
        expected_debug_mapping,
    )?;
    check_table_offset("string", string_offset, expected_string)?;
    check_table_offset("payload", payload_offset, expected_payload)?;
    if payload_offset > bytes.len() as u64 {
        let needed = usize::try_from(payload_offset)
            .ok()
            .and_then(|offset| offset.checked_sub(bytes.len()))
            .unwrap_or(usize::MAX);
        return Err(ObjectError::Truncated {
            offset: bytes.len(),
            needed,
            available: 0,
        });
    }
    let string_start = usize::try_from(string_offset).map_err(|_| ObjectError::InvalidString {
        offset: string_size,
    })?;
    let string_end = usize::try_from(checked_add(string_offset, string_size as u64, "string")?)
        .map_err(|_| ObjectError::InvalidString {
            offset: string_size,
        })?;
    let string_bytes = bytes
        .get(string_start..string_end)
        .ok_or(ObjectError::Truncated {
            offset: string_start,
            needed: string_end.saturating_sub(string_start),
            available: bytes.len().saturating_sub(string_start),
        })?;
    if string_bytes.first() != Some(&0) {
        return Err(ObjectError::InvalidString {
            offset: string_size,
        });
    }
    let config = target.architecture();
    let max_name_bytes = OBJECT_MAX_MATERIALIZED_NAME_BYTES;
    let mut name_bytes = 0usize;
    let mut string_ranges: Vec<(usize, usize)> = Vec::new();
    let mut sections = Vec::new();
    sections
        .try_reserve_exact(section_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut section_symbol_counts = Vec::new();
    let mut section_relocation_counts = Vec::new();
    section_symbol_counts
        .try_reserve_exact(section_count_usize)
        .map_err(ObjectError::Allocation)?;
    section_relocation_counts
        .try_reserve_exact(section_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut actual_symbol_counts = Vec::new();
    let mut actual_relocation_counts = Vec::new();
    actual_symbol_counts
        .try_reserve_exact(section_count_usize)
        .map_err(ObjectError::Allocation)?;
    actual_relocation_counts
        .try_reserve_exact(section_count_usize)
        .map_err(ObjectError::Allocation)?;
    for _ in 0..section_count_usize {
        actual_symbol_counts.push(0usize);
        actual_relocation_counts.push(0usize);
    }
    let mut section_reader = Reader::at(bytes, section_offset as usize)?;
    let mut payload_cursor = payload_offset;
    for index in 0..section_count_usize {
        let name_offset = section_reader.u32()?;
        let kind_raw = section_reader.u8()?;
        if section_reader.u8()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: section_offset + (index as u64) * 64 + 5,
            });
        }
        if section_reader.u16()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: section_offset + (index as u64) * 64 + 6,
            });
        }
        let alignment = section_reader.u64()?;
        let size = section_reader.u64()?;
        let file_offset = section_reader.u64()?;
        let file_size = section_reader.u64()?;
        let symbol_count = section_reader.u32()?;
        let relocation_count = section_reader.u32()?;
        if section_reader.u64()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: section_offset + (index as u64) * 64 + 48,
            });
        }
        if section_reader.u64()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: section_offset + (index as u64) * 64 + 56,
            });
        }
        let name = read_string_bounded(
            string_bytes,
            name_offset,
            &mut name_bytes,
            max_name_bytes,
            &mut string_ranges,
        )?;
        let kind = section_kind(kind_raw, index)?;
        let expected_file_offset = if file_size == 0 {
            0
        } else {
            let aligned = align_up(payload_cursor, 8)?;
            let padding = bytes.get(payload_cursor as usize..aligned as usize).ok_or(
                ObjectError::Truncated {
                    offset: payload_cursor as usize,
                    needed: (aligned - payload_cursor) as usize,
                    available: bytes.len().saturating_sub(payload_cursor as usize),
                },
            )?;
            if padding.iter().any(|byte| *byte != 0) {
                return Err(ObjectError::InvalidSection {
                    index,
                    reason: "payload alignment padding is nonzero",
                });
            }
            aligned
        };
        if file_offset != expected_file_offset {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "payload offset is not canonical",
            });
        }
        let end = checked_add(file_offset, file_size, "section payload")?;
        if end > bytes.len() as u64 {
            return Err(ObjectError::Truncated {
                offset: file_offset as usize,
                needed: file_size as usize,
                available: bytes.len().saturating_sub(file_offset as usize),
            });
        }
        if kind == SectionKind::Bss && file_size != 0 {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "BSS must not have file bytes",
            });
        }
        let payload = &bytes[file_offset as usize..end as usize];
        let section = match kind {
            SectionKind::Text => Section::text(name, config, payload)
                .map_err(|source| map_section_error(source, index))?,
            SectionKind::ReadOnlyData => Section::read_only_data(name, alignment, payload)
                .map_err(|source| map_section_error(source, index))?,
            SectionKind::Data => Section::data(name, alignment, payload)
                .map_err(|source| map_section_error(source, index))?,
            SectionKind::Bss => Section::bss(name, alignment, size)
                .map_err(|source| map_section_error(source, index))?,
        };
        if section.size() != size {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "wire size does not match section",
            });
        }
        payload_cursor = if file_size == 0 { payload_cursor } else { end };
        sections.push(section);
        section_symbol_counts.push(symbol_count as usize);
        section_relocation_counts.push(relocation_count as usize);
    }
    let mut symbols = Vec::new();
    symbols
        .try_reserve_exact(symbol_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut symbol_reader = Reader::at(bytes, symbol_offset as usize)?;
    for index in 0..symbol_count_usize {
        let name_offset = symbol_reader.u32()?;
        let kind = symbol_kind(symbol_reader.u8()?, index)?;
        let binding = symbol_binding(symbol_reader.u8()?, index)?;
        if symbol_reader.u16()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: symbol_offset + (index as u64) * 32 + 6,
            });
        }
        let section = symbol_reader.u16()?;
        if symbol_reader.u16()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: symbol_offset + (index as u64) * 32 + 10,
            });
        }
        if symbol_reader.u32()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: symbol_offset + (index as u64) * 32 + 12,
            });
        }
        let value = symbol_reader.u64()?;
        let size = symbol_reader.u64()?;
        let name = read_string_bounded(
            string_bytes,
            name_offset,
            &mut name_bytes,
            max_name_bytes,
            &mut string_ranges,
        )?;
        if section != u16::MAX {
            let section_index = usize::from(section);
            let count =
                actual_symbol_counts
                    .get_mut(section_index)
                    .ok_or(ObjectError::InvalidSymbol {
                        index,
                        reason: "section index is out of range",
                    })?;
            *count = count.checked_add(1).ok_or(ObjectError::InvalidCount {
                table: "symbol",
                value: u64::MAX,
            })?;
        }
        symbols.push(Symbol::from_wire(
            name,
            kind,
            binding,
            (section != u16::MAX).then_some(SectionIndex::new(section)),
            value,
            size,
        ));
    }
    let mut relocations = Vec::new();
    relocations
        .try_reserve_exact(relocation_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut relocation_reader = Reader::at(bytes, relocation_offset as usize)?;
    for index in 0..relocation_count_usize {
        let symbol = SymbolIndex::new(relocation_reader.u32()?);
        let section = SectionIndex::new(relocation_reader.u16()?);
        let kind = relocation_kind(relocation_reader.u8()?, index)?;
        if relocation_reader.u8()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: relocation_offset + (index as u64) * 32 + 7,
            });
        }
        let target_offset = relocation_reader.u64()?;
        let addend = relocation_reader.i64()?;
        if relocation_reader.u32()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: relocation_offset + (index as u64) * 32 + 24,
            });
        }
        if relocation_reader.u32()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: relocation_offset + (index as u64) * 32 + 28,
            });
        }
        let section_index = usize::from(section.get());
        let count = actual_relocation_counts.get_mut(section_index).ok_or(
            ObjectError::InvalidRelocation {
                index,
                reason: "section index is out of range",
            },
        )?;
        *count = count.checked_add(1).ok_or(ObjectError::InvalidCount {
            table: "relocation",
            value: u64::MAX,
        })?;
        relocations.push(Relocation::new(
            symbol,
            section,
            kind,
            target_offset,
            addend,
        ));
    }
    let mut debug_sources = Vec::new();
    debug_sources
        .try_reserve_exact(debug_source_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut debug_source_reader = Reader::at(bytes, debug_source_offset as usize)?;
    for index in 0..debug_source_count_usize {
        let name_offset = debug_source_reader.u32()?;
        let length = debug_source_reader.u32()?;
        if debug_source_reader.u64()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: debug_source_offset + (index as u64) * 16 + 8,
            });
        }
        debug_sources.push(DebugSource::new(
            read_string_bounded(
                string_bytes,
                name_offset,
                &mut name_bytes,
                max_name_bytes,
                &mut string_ranges,
            )?,
            length,
        ));
    }
    let mut debug_mappings = Vec::new();
    debug_mappings
        .try_reserve_exact(debug_mapping_count_usize)
        .map_err(ObjectError::Allocation)?;
    let mut debug_mapping_reader = Reader::at(bytes, debug_mapping_offset as usize)?;
    for index in 0..debug_mapping_count_usize {
        let section = SectionIndex::new(debug_mapping_reader.u16()?);
        if debug_mapping_reader.u16()? != 0 {
            return Err(ObjectError::ReservedField {
                offset: debug_mapping_offset + (index as u64) * 24 + 2,
            });
        }
        let offset = debug_mapping_reader.u64()?;
        let source = DebugSourceIndex::new(debug_mapping_reader.u32()?);
        let source_offset = debug_mapping_reader.u32()?;
        let source_length = debug_mapping_reader.u32()?;
        debug_mappings.push(CodeMapping::new(
            section,
            offset,
            source,
            source_offset,
            source_length,
        ));
    }
    if payload_cursor != bytes.len() as u64 {
        return Err(ObjectError::InvalidSection {
            index: sections.len(),
            reason: "payload does not end at EOF",
        });
    }
    verify_string_table_coverage(string_bytes, string_ranges)?;
    for (index, (raw, actual)) in section_symbol_counts
        .iter()
        .zip(actual_symbol_counts.iter())
        .enumerate()
    {
        if raw != actual {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "section symbol count is not exact",
            });
        }
    }
    for (index, (raw, actual)) in section_relocation_counts
        .iter()
        .zip(actual_relocation_counts.iter())
        .enumerate()
    {
        if raw != actual {
            return Err(ObjectError::InvalidSection {
                index,
                reason: "section relocation count is not exact",
            });
        }
    }
    let object = ObjectFile::from_wire(
        target,
        (entry_raw != OBJECT_NO_ENTRY).then_some(SymbolIndex::new(entry_raw)),
        sections,
        symbols,
        relocations,
        debug_sources,
        debug_mappings,
    );
    object.validate()?;
    Ok(object)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn at(bytes: &'a [u8], position: usize) -> Result<Self, ObjectError> {
        if position > bytes.len() {
            return Err(ObjectError::Truncated {
                offset: position,
                needed: 1,
                available: 0,
            });
        }
        Ok(Self { bytes, position })
    }
    fn take(&mut self, length: usize) -> Result<&'a [u8], ObjectError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(ObjectError::Truncated {
                offset: self.position,
                needed: length,
                available: 0,
            })?;
        let result = self
            .bytes
            .get(self.position..end)
            .ok_or(ObjectError::Truncated {
                offset: self.position,
                needed: length,
                available: self.bytes.len().saturating_sub(self.position),
            })?;
        self.position = end;
        Ok(result)
    }
    fn u8(&mut self) -> Result<u8, ObjectError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, ObjectError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
    fn u32(&mut self) -> Result<u32, ObjectError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
    fn u64(&mut self) -> Result<u64, ObjectError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
    fn i64(&mut self) -> Result<i64, ObjectError> {
        Ok(self.u64()? as i64)
    }
}

struct StringTable {
    bytes: Vec<u8>,
    values: BTreeMap<String, u32>,
}
impl StringTable {
    fn new() -> Result<Self, ObjectError> {
        let mut bytes = Vec::new();
        bytes.try_reserve(1).map_err(ObjectError::Allocation)?;
        bytes.push(0);
        Ok(Self {
            bytes,
            values: BTreeMap::new(),
        })
    }
    fn intern(&mut self, value: &str) -> Result<u32, ObjectError> {
        if value.is_empty() || value.as_bytes().contains(&0) {
            return Err(ObjectError::InvalidString { offset: 0 });
        }
        if let Some(offset) = self.offset(value) {
            return Ok(offset);
        }
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ObjectError::InvalidCount {
            table: "string",
            value: self.bytes.len() as u64,
        })?;
        self.bytes
            .try_reserve(value.len() + 1)
            .map_err(ObjectError::Allocation)?;
        self.bytes.extend_from_slice(value.as_bytes());
        self.bytes.push(0);
        self.values.insert(String::from(value), offset);
        Ok(offset)
    }
    fn offset(&self, value: &str) -> Option<u32> {
        self.values.get(value).copied()
    }
}

fn read_string_bounded(
    table: &[u8],
    offset: u32,
    used: &mut usize,
    maximum: usize,
    referenced: &mut Vec<(usize, usize)>,
) -> Result<String, ObjectError> {
    if offset == 0 {
        return Err(ObjectError::InvalidString { offset });
    }
    let start = usize::try_from(offset).map_err(|_| ObjectError::InvalidString { offset })?;
    if start >= table.len() || table[start] == 0 {
        return Err(ObjectError::InvalidString { offset });
    }
    let length = table[start..]
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(ObjectError::InvalidString { offset })?;
    let next = used
        .checked_add(length)
        .ok_or(ObjectError::InvalidString { offset })?;
    if next > maximum {
        return Err(ObjectError::InvalidString { offset });
    }
    let value = core::str::from_utf8(&table[start..start + length])
        .map_err(|_| ObjectError::InvalidString { offset })?;
    if value.is_empty() {
        return Err(ObjectError::InvalidString { offset });
    }
    *used = next;
    referenced.push((start, start + length + 1));
    Ok(String::from(value))
}

fn verify_string_table_coverage(
    table: &[u8],
    mut referenced: Vec<(usize, usize)>,
) -> Result<(), ObjectError> {
    referenced.sort_unstable();
    let mut cursor = 1usize;
    for (start, end) in referenced {
        if end <= cursor {
            continue;
        }
        if start != cursor {
            return Err(ObjectError::InvalidString {
                offset: u32::try_from(start).unwrap_or(u32::MAX),
            });
        }
        cursor = end;
    }
    if cursor != table.len() {
        return Err(ObjectError::InvalidString {
            offset: u32::try_from(table.len()).unwrap_or(u32::MAX),
        });
    }
    Ok(())
}
fn put_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(bytes);
}
fn put_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}
fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn put_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn put_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}
fn count_u16(value: usize, table: &'static str) -> Result<u16, ObjectError> {
    u16::try_from(value).map_err(|_| ObjectError::InvalidCount {
        table,
        value: value as u64,
    })
}
fn count_u32(value: usize, table: &'static str) -> Result<u32, ObjectError> {
    u32::try_from(value).map_err(|_| ObjectError::InvalidCount {
        table,
        value: value as u64,
    })
}
fn checked_add(left: u64, right: u64, table: &'static str) -> Result<u64, ObjectError> {
    left.checked_add(right).ok_or(ObjectError::InvalidCount {
        table,
        value: u64::MAX,
    })
}
fn align_up(value: u64, alignment: u64) -> Result<u64, ObjectError> {
    let mask = alignment - 1;
    let sum = value.checked_add(mask).ok_or(ObjectError::InvalidCount {
        table: "alignment",
        value,
    })?;
    Ok(sum & !mask)
}
fn add_offset(
    base: u64,
    count: u64,
    width: usize,
    table: &'static str,
) -> Result<u64, ObjectError> {
    let bytes = count
        .checked_mul(u64::try_from(width).map_err(|_| ObjectError::InvalidCount {
            table,
            value: u64::MAX,
        })?)
        .ok_or(ObjectError::InvalidCount {
            table,
            value: u64::MAX,
        })?;
    checked_add(base, bytes, table)
}
fn check_table_offset(table: &'static str, value: u64, expected: u64) -> Result<(), ObjectError> {
    if value == expected {
        Ok(())
    } else {
        Err(ObjectError::TableOffset {
            table,
            value,
            expected,
        })
    }
}
fn section_kind(value: u8, index: usize) -> Result<SectionKind, ObjectError> {
    match value {
        1 => Ok(SectionKind::Text),
        2 => Ok(SectionKind::ReadOnlyData),
        3 => Ok(SectionKind::Data),
        4 => Ok(SectionKind::Bss),
        _ => Err(ObjectError::InvalidSection {
            index,
            reason: "unknown section kind",
        }),
    }
}
fn symbol_kind(value: u8, index: usize) -> Result<SymbolKind, ObjectError> {
    match value {
        0 => Ok(SymbolKind::Undefined),
        1 => Ok(SymbolKind::Absolute),
        2 => Ok(SymbolKind::Section),
        _ => Err(ObjectError::InvalidSymbol {
            index,
            reason: "unknown symbol kind",
        }),
    }
}
fn symbol_binding(value: u8, index: usize) -> Result<SymbolBinding, ObjectError> {
    match value {
        0 => Ok(SymbolBinding::Local),
        1 => Ok(SymbolBinding::Global),
        _ => Err(ObjectError::InvalidSymbol {
            index,
            reason: "unknown symbol binding",
        }),
    }
}
fn relocation_kind(value: u8, index: usize) -> Result<RelocationKind, ObjectError> {
    match value {
        1 => Ok(RelocationKind::AbsoluteWord32),
        2 => Ok(RelocationKind::AbsoluteWord64),
        3 => Ok(RelocationKind::PcRelativeWord32),
        4 => Ok(RelocationKind::PcRelativeBranch),
        5 => Ok(RelocationKind::LiImmediate),
        6 => Ok(RelocationKind::MemoryDisplacement32),
        _ => Err(ObjectError::InvalidRelocation {
            index,
            reason: "unknown relocation kind",
        }),
    }
}
fn map_section_error(error: ObjectError, index: usize) -> ObjectError {
    match error {
        ObjectError::InvalidSection { reason, .. } => ObjectError::InvalidSection { index, reason },
        ObjectError::Decode { offset, source, .. } => ObjectError::Decode {
            section: index,
            offset,
            source,
        },
        ObjectError::Instruction { offset, source, .. } => ObjectError::Instruction {
            section: index,
            offset,
            source,
        },
        ObjectError::NonCanonical { offset, .. } => ObjectError::NonCanonical {
            section: index,
            offset,
        },
        other => other,
    }
}

const _: () = assert!(OBJECT_SECTION_ENTRY_SIZE == 64);
const _: () = assert!(OBJECT_SYMBOL_ENTRY_SIZE == 32);
const _: () = assert!(OBJECT_RELOCATION_ENTRY_SIZE == 32);
const _: () = assert!(OBJECT_DEBUG_SOURCE_ENTRY_SIZE == 16);
const _: () = assert!(OBJECT_DEBUG_MAPPING_ENTRY_SIZE == 24);
const _: () = assert!(ArchitectureConfig::lz32().instruction_bytes() == 8);
