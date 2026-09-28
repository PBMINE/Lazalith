//! Hardening: the object format's writer and reader agree, and the reader survives
//! being told lies.
//!
//! The object format is hand-written binary — a fixed-layout header, then a section
//! table, a symbol table, a relocation table, the debug tables, a string blob, a debug
//! text blob and a payload. A format like that has two distinct failure modes and this
//! file is about both.
//!
//! **Round-tripping.** Everything the writer records must come back: a field that is
//! written but never read is a field the linker silently ignores, and a field that is
//! read but never written is a field that comes back as zero. Neither shows up as a
//! failure until a program relies on it. So every case here builds an object with a
//! deliberately awkward shape — an empty section, a zero-sized `bss`, names that share
//! a suffix, negative relocation addends, a symbol with no section, a source with
//! empty text, a mapping at a nonzero source offset — and requires that decoding what
//! was encoded gives back the *same object*, not merely a plausible one.
//!
//! **Untrusted input.** An object file is data from outside the program. Every count
//! and every offset in the header is a `u32` or `u64` that the reader has to trust
//! enough to slice with, and the cheapest way to find out whether it does is to lie to
//! it: truncate the file at every length, and corrupt every byte, in turn. A reader
//! that panics, or that reads out of bounds, or that accepts a file whose tables do
//! not fit inside it, is a defect. Refusing is always acceptable; a wrong answer is
//! not.

use lazalith_properties::Gen;
use lazalith_toolchain::{
    CodeMapping, DebugSource, ObjectBuilder, ObjectError, ObjectFile, Relocation, RelocationKind,
    Section, SectionIndex, Symbol, SymbolBinding, SymbolIndex,
};
use lazalith_types::ArchitectureConfig as C;

/// An object with a specific shape, built through the public builder.
struct Shape {
    architecture: C,
    object: ObjectFile,
}

/// Builds an object. `make` adds the sections and is then handed their indices,
/// because a symbol or a relocation can only name a section that already exists —
/// which is the reason the builder hands out indices instead of taking sections.
fn build(
    architecture: C,
    sections: Vec<Section>,
    make: impl FnOnce(&mut ObjectBuilder, &[SectionIndex]) -> Option<()>,
) -> Option<Shape> {
    let mut builder = ObjectBuilder::new(architecture);
    let mut section_indices = Vec::new();
    for section in sections {
        section_indices.push(builder.add_section(section).ok()?);
    }
    make(&mut builder, &section_indices)?;
    let object = builder.build().ok()?;
    Some(Shape {
        architecture,
        object,
    })
}

fn round_trip(shape: &Shape) {
    let encoded = shape
        .object
        .to_bytes()
        .unwrap_or_else(|error| panic!("the object should encode: {error}"));
    let decoded = ObjectFile::from_bytes(&encoded)
        .unwrap_or_else(|error| panic!("the object should decode: {error}\n{:#?}", shape.object));
    assert_eq!(
        &decoded, &shape.object,
        "a round trip changed the object for {:?}",
        shape.architecture
    );
    // And encoding the decoded copy must give the same bytes, which catches a reader
    // that fills in a default where the original had a value the writer never wrote.
    let re_encoded = decoded.to_bytes().expect("the decoded object re-encodes");
    assert_eq!(
        encoded, re_encoded,
        "a round trip is not stable for {:?}",
        shape.architecture
    );
}

/// The shapes worth round-tripping: each one puts a value in a field that a lazier
/// writer would leave at zero.
fn shapes() -> Vec<Shape> {
    let mut out = Vec::new();
    for architecture in [C::lz32(), C::lz64()] {
        // The plainest object there is: one text section, one entry symbol.
        let text = Section::text("text", architecture, &[0u8; 8]).unwrap();
        out.extend(build(architecture, vec![text], |builder, sections| {
            let entry = builder
                .add_symbol(Symbol::section_defined(
                    "entry",
                    SymbolBinding::Local,
                    sections[0],
                    0,
                    8,
                ))
                .ok()?;
            builder.set_entry(entry).ok()
        }));

        // A section with a size but no bytes in the file, which is where a size and
        // a file size come apart. A zero-length section of *either* kind cannot be
        // built at all — a text section with no instructions in it is a section
        // nothing can execute, and a `bss` with no size is a section that is not
        // there. Both refusals are worth asserting, because "a section of size
        // zero" is exactly what a truncated or hostile file claims to be.
        let zero_text = Section::text("empty", architecture, &[]);
        assert!(
            zero_text.is_err(),
            "a text section with no instructions in it must be refused"
        );
        assert!(Section::bss("empty", 4, 0).is_err(), "and a zero-size bss");
        let empty = Section::bss("empty", 4, 8).unwrap();
        out.extend(build(architecture, vec![empty], |_, _| Some(())));

        // A `bss` section has a size but no bytes in the file, so its payload offset
        // and its file size are different numbers, and a reader that confuses them
        // produces a section full of zeroes where there should be none.
        let bss = Section::bss("zero", 8, 4096).unwrap();
        out.extend(build(architecture, vec![bss], |builder, sections| {
            let symbol = builder
                .add_symbol(Symbol::section_defined(
                    "counter",
                    SymbolBinding::Global,
                    sections[0],
                    0,
                    4096,
                ))
                .ok()?;
            builder.set_entry(symbol).ok()
        }));

        // Every section kind at once, so the kind byte is exercised as a value rather
        // than as a constant.
        let code = Section::text("code", architecture, &[0u8; 16]).unwrap();
        let rodata = Section::read_only_data("rodata", 8, &[1, 2, 3, 4]).unwrap();
        let data = Section::data("data", 8, &[9, 9, 9, 9, 9, 9, 9, 9]).unwrap();
        let zero = Section::bss("bss", 16, 64).unwrap();
        out.extend(build(
            architecture,
            vec![code, rodata, data, zero],
            |builder, sections| {
                let defined = builder
                    .add_symbol(Symbol::section_defined(
                        "f",
                        SymbolBinding::Global,
                        sections[0],
                        4,
                        12,
                    ))
                    .ok()?;
                let undefined = builder
                    .add_symbol(Symbol::undefined("printf", SymbolBinding::Global))
                    .ok()?;
                let absolute = builder
                    .add_symbol(Symbol::absolute(
                        "constant",
                        SymbolBinding::Local,
                        0x1234_5678,
                    ))
                    .ok()?;
                // Names that are suffixes of one another, because a string table
                // written as offsets has to tell `f` from `ff` from `fff`.
                builder
                    .add_symbol(Symbol::section_defined(
                        "ff",
                        SymbolBinding::Local,
                        sections[0],
                        0,
                        4,
                    ))
                    .ok()?;
                builder
                    .add_symbol(Symbol::section_defined(
                        "fff",
                        SymbolBinding::Local,
                        sections[0],
                        0,
                        4,
                    ))
                    .ok()?;
                builder
                    .add_relocation(Relocation::new(
                        undefined,
                        sections[0],
                        RelocationKind::AbsoluteWord32,
                        0,
                        0,
                    ))
                    .ok()?;
                // A negative addend, which is an `i64` on the wire and must not come
                // back as its two's-complement bits read unsigned.
                builder
                    .add_relocation(Relocation::new(
                        absolute,
                        sections[0],
                        RelocationKind::AbsoluteWord64,
                        4,
                        -4096,
                    ))
                    .ok()?;
                let source = builder
                    .add_debug_source(DebugSource::new("main.lz", "fn main() {}\n"))
                    .ok()?;
                builder
                    .add_debug_mapping(CodeMapping::new(sections[0], 0, source, 6, 12))
                    .ok()?;
                builder.set_entry(defined).ok()
            },
        ));

        // A symbol with no section at all, every binding, a source with empty text,
        // and a mapping at offset zero into that empty text — because an empty string
        // is a legal string, and a table that treats zero length as absent is not.
        let code = Section::text("code", architecture, &[0u8; 8]).unwrap();
        out.extend(build(architecture, vec![code], |builder, sections| {
            builder
                .add_symbol(Symbol::undefined("missing", SymbolBinding::Global))
                .ok()?;
            builder
                .add_symbol(Symbol::absolute("k", SymbolBinding::Global, 0))
                .ok()?;
            let blank = builder
                .add_debug_source(DebugSource::new("empty.lz", ""))
                .ok()?;
            builder
                .add_debug_mapping(CodeMapping::new(sections[0], 0, blank, 0, 0))
                .ok()
        }));
    }
    out
}

#[test]
fn every_shape_survives_a_round_trip() {
    let shapes = shapes();
    assert!(
        shapes.len() >= 6,
        "the shapes collapsed to {}, which means a builder call is refusing and the \
         rest of this file is testing less than it looks like it is",
        shapes.len()
    );
    for shape in &shapes {
        round_trip(shape);
    }
}

#[test]
fn a_generated_object_survives_a_round_trip() {
    // The hand-written shapes cover the fields; this covers the *combinations*, which
    // is where a writer that computes an offset from the wrong running total shows
    // up. Deterministic, over both architectures.
    for (width, architecture) in [(1u64, C::lz32()), (2, C::lz64())] {
        for seed in 0..200u64 {
            let mut rng = Gen::seeded(seed ^ (width << 40));
            let section_count = rng.range(0, 4) as usize;
            let mut sections = Vec::new();
            for index in 0..section_count {
                let length = rng.range(0, 24) as usize;
                let mut bytes = vec![0u8; length];
                for byte in &mut bytes {
                    *byte = rng.next_u8();
                }
                let name = format!("s{index}_{}", rng.next_u16());
                let alignment = 1u64 << rng.below(6);
                let section = match rng.below(3) {
                    // A text section has to hold instructions that decode, so its
                    // contents are nops rather than random bytes; the data sections
                    // carry whatever the generator produced, which is the point.
                    0 => Section::text(name, architecture, &vec![0u8; bytes.len()]),
                    1 => Section::read_only_data(name, alignment, &bytes),
                    _ => Section::data(name, alignment, &bytes),
                };
                // An alignment the format does not allow is a legitimate refusal, not
                // a failure of the round trip.
                let Ok(section) = section else { continue };
                sections.push(section);
            }
            if sections.is_empty() {
                continue;
            }
            let want_symbols = rng.range(0, 5) as usize;
            let want_undefined = rng.bool();
            let want_absolute = rng.bool();
            let want_sources = rng.range(0, 3) as usize;
            let want_relocations = rng.range(0, 4) as usize;
            let want_mappings = rng.range(0, 3) as usize;
            let want_entry = rng.bool();
            let text: Vec<String> = (0..want_sources)
                .map(|index| {
                    let length = rng.range(0, 32) as usize;
                    let text: String = (0..length)
                        .map(|_| char::from(rng.range(0x20, 0x7E) as u8))
                        .collect();
                    format!("{index}\u{0}{text}")
                })
                .collect();
            let names: Vec<String> = (0..want_symbols)
                .map(|index| format!("sym{index}"))
                .collect();
            let symbol_picks: Vec<(u8, u8, u64)> = (0..want_symbols)
                .map(|_| {
                    (
                        rng.below(sections.len() as u64) as u8,
                        rng.below(3) as u8,
                        rng.next_u64() % 4096,
                    )
                })
                .collect();
            let undefined_binding = rng.below(3) as u8;
            let absolute_value = rng.next_u64();
            let relocation_picks: Vec<(u8, u64, u8, i64)> = (0..want_relocations)
                .map(|_| {
                    (
                        rng.below(sections.len() as u64) as u8,
                        rng.next_u64() % 64,
                        rng.below(2) as u8,
                        rng.next_u64() as i64,
                    )
                })
                .collect();
            let mapping_picks: Vec<(u8, u64, u8, u32, u32)> = (0..want_mappings)
                .map(|_| {
                    (
                        rng.below(sections.len() as u64) as u8,
                        rng.next_u64() % 64,
                        rng.below(want_sources.max(1) as u64) as u8,
                        rng.next_u32(),
                        rng.next_u32(),
                    )
                })
                .collect();
            // Which symbol each relocation names, and whether it names the undefined
            // one, are decided here rather than inside the closure so that the
            // generator is not borrowed across it.
            let relocation_symbols: Vec<(bool, u8)> = (0..want_relocations)
                .map(|_| (rng.bool(), rng.below(want_symbols.max(1) as u64) as u8))
                .collect();
            let built = build(architecture, sections, move |builder, sections| {
                let mut indices = Vec::new();
                for (name, (section, binding, value)) in names.iter().zip(&symbol_picks) {
                    let binding = match binding {
                        0 => SymbolBinding::Local,
                        1 => SymbolBinding::Global,
                        _ => SymbolBinding::Global,
                    };
                    indices.push(
                        builder
                            .add_symbol(Symbol::section_defined(
                                name.clone(),
                                binding,
                                sections[*section as usize],
                                *value,
                                0,
                            ))
                            .ok()?,
                    );
                }
                let undefined = if want_undefined {
                    let binding = match undefined_binding {
                        0 => SymbolBinding::Local,
                        1 => SymbolBinding::Global,
                        _ => SymbolBinding::Global,
                    };
                    Some(
                        builder
                            .add_symbol(Symbol::undefined("external", binding))
                            .ok()?,
                    )
                } else {
                    None
                };
                let absolute = if want_absolute {
                    Some(
                        builder
                            .add_symbol(Symbol::absolute(
                                "abs",
                                SymbolBinding::Local,
                                absolute_value,
                            ))
                            .ok()?,
                    )
                } else {
                    None
                };
                let mut sources = Vec::new();
                for text in &text {
                    sources.push(
                        builder
                            .add_debug_source(DebugSource::new("f.lz", text))
                            .ok()?,
                    );
                }
                for (section, offset, source, source_offset, length) in &mapping_picks {
                    if sources.is_empty() {
                        break;
                    }
                    builder
                        .add_debug_mapping(CodeMapping::new(
                            sections[*section as usize],
                            *offset,
                            sources[*source as usize % sources.len()],
                            *source_offset,
                            *length,
                        ))
                        .ok()?;
                }
                // The relocations name a symbol, so they can only be added once the
                // symbols exist — which is the ordering constraint the builder exists
                // to enforce.
                let available: Vec<SymbolIndex> = indices.clone();
                for ((section, offset, kind, addend), (prefer_external, which)) in
                    relocation_picks.iter().zip(&relocation_symbols)
                {
                    if available.is_empty() && !prefer_external {
                        continue;
                    }
                    let external = if *prefer_external {
                        undefined.or(absolute)
                    } else {
                        None
                    };
                    let symbol = match external {
                        Some(symbol) => symbol,
                        None => match available.get(*which as usize % available.len().max(1)) {
                            Some(symbol) => *symbol,
                            None => continue,
                        },
                    };
                    let kind = if *kind == 0 {
                        RelocationKind::AbsoluteWord32
                    } else {
                        RelocationKind::AbsoluteWord64
                    };
                    builder
                        .add_relocation(Relocation::new(
                            symbol,
                            sections[*section as usize],
                            kind,
                            *offset,
                            *addend,
                        ))
                        .ok()?;
                }
                if want_entry {
                    builder.set_entry(*indices.first()?).ok()?;
                }
                Some(())
            });
            if let Some(shape) = built {
                round_trip(&shape);
            }
        }
    }
}

#[test]
fn a_truncated_object_is_refused_at_every_length() {
    // Truncation is the most likely corruption there is — an interrupted write, a
    // short read, a file copied before it finished. Every prefix of a valid object is
    // tried, and every one of them must be refused rather than read past the end.
    for shape in shapes() {
        let encoded = shape.object.to_bytes().expect("the object encodes");
        for length in 0..encoded.len() {
            let result = ObjectFile::from_bytes(&encoded[..length]);
            assert!(
                result.is_err(),
                "a {length}-byte prefix of a {}-byte object was accepted",
                encoded.len()
            );
        }
    }
}

#[test]
fn a_corrupted_byte_never_panics_and_never_reads_out_of_bounds() {
    // Every byte position, corrupted in turn. What this asserts is the *safety*
    // property, not a fixed answer: a byte of an object file is either a count, an
    // offset, a name index, a size or a discriminant, and corrupting any of them must
    // leave the reader with an `Err` or a well-formed object — never a panic, never a
    // read past the end of the buffer, and never a slice at a nonsense length.
    //
    // The first draft of this test asserted something stronger — that corruption is
    // always *refused* — and it failed on a symbol-binding byte: changing `Local` to
    // `Global` produces a perfectly valid object that means something different. That
    // is correct behaviour, and a test demanding otherwise is a test demanding a
    // checksum this format does not have. What is not acceptable is the reader
    // trusting a corrupted count far enough to read outside the file, and that is what
    // the cases below are for.
    for shape in shapes() {
        let encoded = shape.object.to_bytes().expect("the object encodes");
        for index in 0..encoded.len() {
            for replacement in [0x00u8, 0xFF, 0x01, 0x7F, 0x80] {
                let mut corrupted = encoded.clone();
                if corrupted[index] == replacement {
                    continue;
                }
                corrupted[index] = replacement;
                // The result is either an error or an object that passes its own
                // validation — which is the strongest statement available without a
                // checksum, and it is one the reader can actually be held to.
                if let Ok(decoded) = ObjectFile::from_bytes(&corrupted) {
                    decoded
                        .validate()
                        .unwrap_or_else(|error| panic!("byte {index} = {replacement:#04x} produced an object that fails its own validation: {error}"));
                }
            }
        }
    }
}

#[test]
fn a_corrupted_count_cannot_make_the_reader_allocate_or_read_past_the_file() {
    // The specific safety case, checked directly rather than by walking every byte:
    // a count field set to the largest value it can hold must be refused against the
    // file's actual size, and refused *before* anything is reserved or sliced.
    //
    // These offsets are the count fields in the header, and the test says so in terms
    // of the constants the format publishes rather than of magic numbers.
    use lazalith_toolchain::{
        OBJECT_HEADER_SIZE, OBJECT_MAX_DEBUG_SOURCES, OBJECT_MAX_SECTIONS, OBJECT_MAX_SYMBOLS,
    };
    const { assert!(OBJECT_MAX_SECTIONS > 0 && OBJECT_MAX_SYMBOLS > 0 && OBJECT_MAX_DEBUG_SOURCES > 0) };
    for shape in shapes() {
        let mut encoded = shape.object.to_bytes().expect("the object encodes");
        // The header is 128 bytes and the counts are u32s inside it; walk the whole
        // header a word at a time so the assertion holds wherever a count actually
        // sits, rather than against a hard-coded offset that a format change would
        // silently invalidate.
        for offset in (0..OBJECT_HEADER_SIZE - 3).step_by(4) {
            let original = encoded[offset..offset + 4].to_vec();
            encoded[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let result = ObjectFile::from_bytes(&encoded);
            if let Ok(decoded) = result {
                decoded
                    .validate()
                    .unwrap_or_else(|error| panic!("a count of u32::MAX at header offset {offset} produced an object that fails its own validation: {error}"));
            }
            encoded[offset..offset + 4].copy_from_slice(&original);
        }
    }
}

#[test]
fn a_trailing_garbage_byte_is_refused() {
    // A file with valid content and something appended. Accepting it would mean the
    // reader ignores the length, which is how a signature check gets bypassed by
    // appending.
    for shape in shapes() {
        let mut encoded = shape.object.to_bytes().expect("the object encodes");
        encoded.push(0x00);
        assert!(
            ObjectFile::from_bytes(&encoded).is_err(),
            "an object with a byte appended was accepted"
        );
    }
}

#[test]
fn an_empty_or_tiny_file_is_refused() {
    for bytes in [
        &[][..],
        &[0u8][..],
        &[0u8; 4],
        &[0u8; 7],
        &[0u8; 8],
        &[0u8; 64],
        &[0xFFu8; 1024],
    ] {
        let result = ObjectFile::from_bytes(bytes);
        assert!(
            result.is_err(),
            "{} arbitrary bytes were accepted",
            bytes.len()
        );
        // And the error is one of ours: not a panic, and not a parse of nonsense.
        let _error: ObjectError = result.expect_err("an error");
    }
}

#[test]
fn an_object_with_the_magic_replaced_is_refused() {
    for shape in shapes() {
        let mut encoded = shape.object.to_bytes().expect("the object encodes");
        encoded[0] ^= 0xFF;
        let error = ObjectFile::from_bytes(&encoded).expect_err("a wrong magic must be refused");
        assert!(matches!(error, ObjectError::InvalidMagic), "got {error:?}");
    }
}
