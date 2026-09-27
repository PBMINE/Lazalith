use crate::{
    ObjectError, ObjectFile, Relocation, RelocationKind, SectionKind, SymbolBinding, SymbolIndex,
    SymbolKind,
};
use alloc::{borrow::ToOwned, collections::BTreeMap, string::String, vec::Vec};
use core::{error::Error, fmt};
use lazalith_isa::{Instruction, Opcode, Operand, decode, encode};
use lazalith_os::debug::{DebugBlock, DebugEntry, DebugFile};
use lazalith_os::{
    LZX_DATA_PERMISSIONS, LzxArchitecture, LzxError, LzxImage, LzxSection, LzxSectionKind,
    USER_CODE_START, USER_DATA_LENGTH, USER_DATA_START, USER_STACK_LENGTH,
};
use lazalith_types::ArchitectureConfig;

#[derive(Clone, Debug, Default)]
pub struct LinkOptions {
    pub entry_symbol: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LinkedProgram {
    image: LzxImage,
    entry_offset: u64,
    entry_symbol: String,
    debug: DebugBlock,
}

impl LinkedProgram {
    pub const fn image(&self) -> &LzxImage {
        &self.image
    }

    /// The program's source-level debug information, if it was built with any.
    ///
    /// This is where the mappings stop being per-object offsets and start being
    /// addresses in the loaded image, because that is the only thing a debugger
    /// has. A program assembled without debug information gets an empty block
    /// rather than a failure: assembly is a first-class input, and refusing to
    /// link a program because nobody wrote down where it came from would make the
    /// assembler a second-class one.
    pub const fn debug(&self) -> &DebugBlock {
        &self.debug
    }

    pub fn into_image(self) -> LzxImage {
        self.image
    }

    pub const fn entry_offset(&self) -> u64 {
        self.entry_offset
    }

    pub fn entry_symbol(&self) -> &str {
        &self.entry_symbol
    }
}

#[derive(Debug)]
pub enum LinkError {
    NoObjects,
    IncompatibleArchitecture,
    IncompatibleIsa {
        value: u16,
    },
    IncompatibleAbi {
        value: u16,
    },
    Object(ObjectError),
    MissingEntry,
    DuplicateGlobal {
        name: String,
    },
    UndefinedSymbol {
        name: String,
    },
    SymbolIndex {
        object: usize,
        index: u32,
    },
    SectionLayout {
        object: usize,
        section: u16,
    },
    DataOverflow,
    RelocationSymbol {
        object: usize,
        index: usize,
    },
    RelocationArithmetic {
        object: usize,
        index: usize,
    },
    RelocationTarget {
        object: usize,
        index: usize,
    },
    Image(LzxError),
    Allocation(alloc::collections::TryReserveError),
    /// A mapping names a source index the object did not have.
    UnknownDebugSource {
        /// The index that was named.
        value: u32,
    },
    /// A mapping names a section index the object did not have.
    UnknownDebugSection {
        /// The index that was named.
        value: u16,
    },
    /// More distinct sources than a `u32` index can name.
    TooManyDebugSources,
    /// The gathered debug table did not hold together.
    ///
    /// This should be unreachable: the table is built from validated objects and
    /// validated again on the way in. It is a separate case rather than a wrapped
    /// `DebugError` because a linker that reported "the image refused" for a
    /// problem in the *linker's own* table would send whoever is chasing the bug
    /// looking in the wrong place.
    BadDebugBlock,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoObjects => f.write_str("linker requires at least one object"),
            Self::IncompatibleArchitecture => f.write_str("objects use incompatible architectures"),
            Self::IncompatibleIsa { value } => {
                write!(f, "object ISA version {value} is incompatible")
            }
            Self::IncompatibleAbi { value } => {
                write!(f, "object ABI version {value} is incompatible")
            }
            Self::Object(source) => write!(f, "object validation failed: {source}"),
            Self::MissingEntry => f.write_str("linker could not find an entry symbol"),
            Self::DuplicateGlobal { name } => {
                write!(f, "global symbol {name} is defined more than once")
            }
            Self::UndefinedSymbol { name } => write!(f, "global symbol {name} is undefined"),
            Self::SymbolIndex { object, index } => {
                write!(f, "object {object} symbol index {index} is invalid")
            }
            Self::SectionLayout { object, section } => {
                write!(f, "object {object} section {section} cannot be laid out")
            }
            Self::DataOverflow => f.write_str("linked data does not fit the runtime data region"),
            Self::RelocationSymbol { object, index } => write!(
                f,
                "object {object} relocation {index} has no symbol address"
            ),
            Self::RelocationArithmetic { object, index } => write!(
                f,
                "object {object} relocation {index} overflows its arithmetic domain"
            ),
            Self::RelocationTarget { object, index } => write!(
                f,
                "object {object} relocation {index} has an invalid target"
            ),
            Self::Image(source) => write!(f, "linked executable is invalid: {source}"),
            Self::Allocation(source) => write!(f, "linker allocation failed: {source}"),
            Self::UnknownDebugSource { value } => write!(
                f,
                "a code mapping names source {value}, which the object does not have"
            ),
            Self::UnknownDebugSection { value } => write!(
                f,
                "a code mapping names section {value}, which the object does not have"
            ),
            Self::TooManyDebugSources => {
                write!(
                    f,
                    "the program has more sources than a debug table can name"
                )
            }
            Self::BadDebugBlock => write!(f, "the gathered debug table did not hold together"),
        }
    }
}

impl Error for LinkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Object(source) => Some(source),
            Self::Image(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SectionPlacement {
    base: u64,
    alignment: u64,
    size: u64,
}

#[derive(Clone, Debug)]
struct ObjectLayout {
    sections: Vec<SectionPlacement>,
}

#[derive(Clone, Copy, Debug)]
struct SymbolAddress {
    address: Option<u64>,
}

struct RelocationContext<'a> {
    objects: &'a [ObjectFile],
    addresses: &'a [Vec<SymbolAddress>],
    globals: &'a BTreeMap<String, Option<u64>>,
    placements: &'a [ObjectLayout],
}

struct LinkedLayout {
    code: Vec<u8>,
    data: Vec<u8>,
    bss_size: u64,
    data_alignment: u64,
    bss_alignment: u64,
    /// The name of the symbol the image starts at.
    entry_name: String,
    /// Where that symbol ended up.
    entry_address: u64,
    /// Where each object's sections landed.
    ///
    /// Kept here rather than recomputed because the placements are decided once,
    /// inside the layout, and a second pass that derived them again would be a
    /// second answer to the same question. The debug table needs them: a mapping's
    /// offset is relative to its own object, and only here does anyone know where
    /// that object went.
    placements: Vec<ObjectLayout>,
}

pub fn link_objects(
    objects: &[ObjectFile],
    options: &LinkOptions,
) -> Result<LinkedProgram, LinkError> {
    let layout = link_layout(objects, options)?;
    let mut image_sections = Vec::new();
    let code_section = LzxSection::code(&layout.code).map_err(LinkError::Image)?;
    image_sections
        .try_reserve(3)
        .map_err(LinkError::Allocation)?;
    image_sections.push(code_section);
    if !layout.data.is_empty() || layout.bss_size != 0 {
        image_sections
            .push(LzxSection::data(&layout.data, layout.data_alignment).map_err(LinkError::Image)?);
    }
    if layout.bss_size != 0 {
        let data_length = u64::try_from(layout.data.len()).map_err(|_| LinkError::DataOverflow)?;
        let mask = layout.bss_alignment - 1;
        let offset = data_length
            .checked_add(mask)
            .ok_or(LinkError::DataOverflow)?
            & !mask;
        image_sections.push(
            LzxSection::new(
                LzxSectionKind::Bss,
                LZX_DATA_PERMISSIONS,
                offset,
                layout.bss_size,
                layout.bss_alignment,
                &[],
            )
            .map_err(LinkError::Image)?,
        );
    }
    // The entry offset is relative to the *code section*, not to the entry
    // object's own copy of it. The image has one code section built by
    // concatenating every object's text, and it starts at the bottom of the code
    // region; an object placed later sits at a non-zero offset inside it. Taking
    // the entry object's section base as the origin made the offset relative to
    // that object, so an image whose entry was not in the first object started
    // somewhere else entirely — at whatever code happened to be first.
    let entry_offset = layout
        .entry_address
        .checked_sub(USER_CODE_START)
        .ok_or(LinkError::MissingEntry)?;
    let debug = collect_debug(objects, &layout)?;
    let data_mask = layout.bss_alignment - 1;
    let data_end = u64::try_from(layout.data.len())
        .ok()
        .and_then(|size| size.checked_add(data_mask))
        .map(|size| size & !data_mask)
        .ok_or(LinkError::DataOverflow)?;
    let required_data = data_end
        .checked_add(layout.bss_size)
        .ok_or(LinkError::DataOverflow)?;
    let image = LzxImage::with_debug(
        // Every object was checked to agree on the architecture, the ISA and the
        // ABI before anything was laid out, so the first one speaks for all of
        // them and the image needs no per-object target of its own.
        LzxArchitecture::from_config(objects[0].config()),
        0,
        entry_offset,
        required_data,
        USER_STACK_LENGTH,
        image_sections,
        // The block is empty when no object had debug information, and an empty
        // block is written as no block at all rather than as a table with no
        // entries — a program assembled by hand should not carry the eight bytes
        // of header that say so.
        // The clone is because `debug` is also handed to the `LinkedProgram`
        // below, and the two must not share: a caller that mutated one would
        // then see the other change under it.
        (!debug.is_empty()).then_some(debug.clone()),
    )
    .map_err(LinkError::Image)?;
    Ok(LinkedProgram {
        image,
        entry_offset,
        entry_symbol: layout.entry_name,
        debug,
    })
}

fn link_layout(objects: &[ObjectFile], options: &LinkOptions) -> Result<LinkedLayout, LinkError> {
    if objects.is_empty() {
        return Err(LinkError::NoObjects);
    }
    let architecture = objects[0].config();
    for object in objects {
        object.validate().map_err(LinkError::Object)?;
        if object.config() != architecture {
            return Err(LinkError::IncompatibleArchitecture);
        }
        if object.isa_version() != objects[0].isa_version() {
            return Err(LinkError::IncompatibleIsa {
                value: object.isa_version(),
            });
        }
        if object.abi_version() != objects[0].abi_version() {
            return Err(LinkError::IncompatibleAbi {
                value: object.abi_version(),
            });
        }
    }
    let mut code = Vec::new();
    let mut data = Vec::new();
    let mut data_alignment = 1u64;
    let mut bss_alignment = 1u64;
    let mut placements = Vec::new();
    for (object_index, object) in objects.iter().enumerate() {
        let mut placement = ObjectLayout {
            sections: Vec::new(),
        };
        for (section_index, section) in object.sections().iter().enumerate() {
            let alignment = section.alignment().max(1);
            let placement_for_section = match section.kind() {
                SectionKind::Text => {
                    let current = u64::try_from(code.len()).map_err(|_| LinkError::DataOverflow)?;
                    let mask = alignment - 1;
                    let aligned = current.checked_add(mask).ok_or(LinkError::DataOverflow)? & !mask;
                    let padding =
                        usize::try_from(aligned - current).map_err(|_| LinkError::DataOverflow)?;
                    code.try_reserve(padding + section.bytes().len())
                        .map_err(LinkError::Allocation)?;
                    code.resize(code.len() + padding, 0);
                    let base = USER_CODE_START
                        .checked_add(aligned)
                        .ok_or(LinkError::DataOverflow)?;
                    code.extend_from_slice(section.bytes());
                    SectionPlacement {
                        base,
                        alignment,
                        size: section.size(),
                    }
                }
                SectionKind::ReadOnlyData | SectionKind::Data => {
                    data_alignment = data_alignment.max(alignment);
                    let current = u64::try_from(data.len()).map_err(|_| LinkError::DataOverflow)?;
                    let mask = alignment - 1;
                    let aligned = current.checked_add(mask).ok_or(LinkError::DataOverflow)? & !mask;
                    let padding =
                        usize::try_from(aligned - current).map_err(|_| LinkError::DataOverflow)?;
                    data.try_reserve(padding + section.bytes().len())
                        .map_err(LinkError::Allocation)?;
                    data.resize(data.len() + padding, 0);
                    let base = USER_DATA_START
                        .checked_add(aligned)
                        .ok_or(LinkError::DataOverflow)?;
                    data.extend_from_slice(section.bytes());
                    SectionPlacement {
                        base,
                        alignment,
                        size: section.size(),
                    }
                }
                SectionKind::Bss => {
                    bss_alignment = bss_alignment.max(alignment);
                    SectionPlacement {
                        base: 0,
                        alignment,
                        size: section.size(),
                    }
                }
            };
            let _ = section_index;
            placement.sections.push(placement_for_section);
        }
        placements.push(placement);
        let _ = object_index;
    }
    let data_end = USER_DATA_START
        .checked_add(u64::try_from(data.len()).map_err(|_| LinkError::DataOverflow)?)
        .ok_or(LinkError::DataOverflow)?;
    let bss_mask = bss_alignment - 1;
    let bss_base = data_end
        .checked_add(bss_mask)
        .ok_or(LinkError::DataOverflow)?
        & !bss_mask;
    let mut bss_cursor = bss_base;
    for (object_index, placement) in placements.iter_mut().enumerate() {
        for (section_index, section) in placement.sections.iter_mut().enumerate() {
            if objects[object_index].sections()[section_index].kind() == SectionKind::Bss {
                let mask = section.alignment - 1;
                bss_cursor = bss_cursor
                    .checked_add(mask)
                    .ok_or(LinkError::DataOverflow)?
                    & !mask;
                section.base = bss_cursor;
                bss_cursor = bss_cursor
                    .checked_add(section.size)
                    .ok_or(LinkError::DataOverflow)?;
            }
        }
    }
    let bss_size = bss_cursor
        .checked_sub(bss_base)
        .ok_or(LinkError::DataOverflow)?;
    if data.len() > USER_DATA_LENGTH as usize {
        return Err(LinkError::DataOverflow);
    }
    let mut addresses = Vec::new();
    let mut globals: BTreeMap<String, Option<u64>> = BTreeMap::new();
    for (object_index, object) in objects.iter().enumerate() {
        let placement = placements[object_index].clone();
        let mut object_addresses = Vec::new();
        object_addresses
            .try_reserve_exact(object.symbols().len())
            .map_err(LinkError::Allocation)?;
        for symbol in object.symbols() {
            let address = match symbol.kind() {
                SymbolKind::Undefined => None,
                SymbolKind::Absolute => Some(symbol.value()),
                SymbolKind::Section => {
                    let section_index = symbol.section().map_or(0, |index| index.get());
                    let section = placement.sections.get(section_index as usize).ok_or(
                        LinkError::SectionLayout {
                            object: object_index,
                            section: section_index,
                        },
                    )?;
                    Some(
                        section
                            .base
                            .checked_add(symbol.value())
                            .ok_or(LinkError::DataOverflow)?,
                    )
                }
            };
            let global = symbol.binding() == SymbolBinding::Global;
            if global {
                match address {
                    Some(value) => {
                        if globals
                            .insert(symbol.name().to_owned(), Some(value))
                            .is_some_and(|value| value.is_some())
                        {
                            return Err(LinkError::DuplicateGlobal {
                                name: symbol.name().to_owned(),
                            });
                        }
                    }
                    None => {
                        globals.entry(symbol.name().to_owned()).or_insert(None);
                    }
                }
            }
            object_addresses.push(SymbolAddress { address });
        }
        addresses.push(object_addresses);
    }
    for (name, address) in &globals {
        if address.is_none() {
            return Err(LinkError::UndefinedSymbol { name: name.clone() });
        }
    }
    let (_entry_object, _entry_index, entry_name, entry_address) =
        choose_entry(objects, options, &addresses)?;
    let entry_address = entry_address.ok_or(LinkError::MissingEntry)?;
    for (object_index, object) in objects.iter().enumerate() {
        for (relocation_index, relocation) in object.relocations().iter().enumerate() {
            apply_relocation(
                &RelocationContext {
                    objects,
                    addresses: &addresses,
                    globals: &globals,
                    placements: &placements,
                },
                object_index,
                relocation_index,
                relocation,
                &mut code,
                &mut data,
            )?;
        }
    }
    Ok(LinkedLayout {
        code,
        data,
        bss_size,
        data_alignment,
        bss_alignment,
        entry_name,
        entry_address,
        placements,
    })
}

fn choose_entry(
    objects: &[ObjectFile],
    options: &LinkOptions,
    addresses: &[Vec<SymbolAddress>],
) -> Result<(usize, SymbolIndex, String, Option<u64>), LinkError> {
    if let Some(name) = &options.entry_symbol {
        for (object_index, object) in objects.iter().enumerate() {
            for (index, symbol) in object.symbols().iter().enumerate() {
                if symbol.name() == name
                    && symbol.kind() != SymbolKind::Undefined
                    && valid_entry(object, index)
                {
                    return Ok((
                        object_index,
                        SymbolIndex::new(u32::try_from(index).map_err(|_| {
                            LinkError::SymbolIndex {
                                object: object_index,
                                index: u32::MAX,
                            }
                        })?),
                        name.clone(),
                        addresses[object_index][index].address,
                    ));
                }
            }
        }
        return Err(LinkError::MissingEntry);
    }
    for (object_index, object) in objects.iter().enumerate() {
        if let Some(entry) = object.entry() {
            let index = entry.get() as usize;
            let symbol = object.symbols().get(index).ok_or(LinkError::MissingEntry)?;
            if !valid_entry(object, index) {
                return Err(LinkError::MissingEntry);
            }
            return Ok((
                object_index,
                entry,
                symbol.name().to_owned(),
                addresses[object_index][index].address,
            ));
        }
    }
    Err(LinkError::MissingEntry)
}

fn valid_entry(object: &ObjectFile, index: usize) -> bool {
    let Some(symbol) = object.symbols().get(index) else {
        return false;
    };
    let Some(section) = symbol.section() else {
        return false;
    };
    let Some(section) = object.sections().get(section.get() as usize) else {
        return false;
    };
    section.kind() == SectionKind::Text
        && symbol.value().is_multiple_of(4)
        && symbol
            .value()
            .checked_add(8)
            .is_some_and(|end| end <= section.size())
}

fn apply_relocation(
    context: &RelocationContext<'_>,
    object_index: usize,
    relocation_index: usize,
    relocation: &Relocation,
    code: &mut [u8],
    data: &mut [u8],
) -> Result<(), LinkError> {
    let object = &context.objects[object_index];
    let symbol = object
        .symbols()
        .get(relocation.symbol().get() as usize)
        .ok_or(LinkError::RelocationSymbol {
            object: object_index,
            index: relocation_index,
        })?;
    let symbol_address = if symbol.binding() == SymbolBinding::Global {
        context
            .globals
            .get(symbol.name())
            .and_then(|address| *address)
    } else {
        context.addresses[object_index]
            .get(relocation.symbol().get() as usize)
            .and_then(|symbol| symbol.address)
    }
    .ok_or(LinkError::RelocationSymbol {
        object: object_index,
        index: relocation_index,
    })?;
    let section_index = relocation.section().get() as usize;
    let section = object
        .sections()
        .get(section_index)
        .ok_or(LinkError::RelocationTarget {
            object: object_index,
            index: relocation_index,
        })?;
    let placement = context.placements[object_index]
        .sections
        .get(section_index)
        .ok_or(LinkError::RelocationTarget {
            object: object_index,
            index: relocation_index,
        })?;
    let target =
        placement
            .base
            .checked_add(relocation.offset())
            .ok_or(LinkError::RelocationArithmetic {
                object: object_index,
                index: relocation_index,
            })?;
    match section.kind() {
        SectionKind::Text => patch_code(
            object.config(),
            code,
            target,
            relocation,
            symbol_address,
            object_index,
            relocation_index,
        ),
        SectionKind::ReadOnlyData | SectionKind::Data => patch_data(
            data,
            target,
            relocation,
            symbol_address,
            object_index,
            relocation_index,
        ),
        SectionKind::Bss => Err(LinkError::RelocationTarget {
            object: object_index,
            index: relocation_index,
        }),
    }
}

fn patch_data(
    data: &mut [u8],
    target: u64,
    relocation: &Relocation,
    symbol: u64,
    object: usize,
    index: usize,
) -> Result<(), LinkError> {
    let value = i128::from(symbol) + i128::from(relocation.addend());
    let bytes = match relocation.kind() {
        RelocationKind::AbsoluteWord32 => {
            let value = u32::try_from(value)
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            value.to_le_bytes().to_vec()
        }
        RelocationKind::AbsoluteWord64 => {
            let value = u64::try_from(value)
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            value.to_le_bytes().to_vec()
        }
        RelocationKind::PcRelativeWord32 => {
            let value = i32::try_from(value - i128::from(target))
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            value.to_le_bytes().to_vec()
        }
        _ => return Err(LinkError::RelocationTarget { object, index }),
    };
    let start = usize::try_from(
        target
            .checked_sub(USER_DATA_START)
            .ok_or(LinkError::RelocationTarget { object, index })?,
    )
    .map_err(|_| LinkError::RelocationTarget { object, index })?;
    let end = start
        .checked_add(bytes.len())
        .ok_or(LinkError::RelocationTarget { object, index })?;
    data.get_mut(start..end)
        .ok_or(LinkError::RelocationTarget { object, index })?
        .copy_from_slice(&bytes);
    Ok(())
}

fn patch_code(
    config: ArchitectureConfig,
    code: &mut [u8],
    target: u64,
    relocation: &Relocation,
    symbol: u64,
    object: usize,
    index: usize,
) -> Result<(), LinkError> {
    let start = usize::try_from(
        target
            .checked_sub(USER_CODE_START)
            .ok_or(LinkError::RelocationTarget { object, index })?,
    )
    .map_err(|_| LinkError::RelocationTarget { object, index })?;
    let end = start
        .checked_add(8)
        .ok_or(LinkError::RelocationTarget { object, index })?;
    let bytes = code
        .get(start..end)
        .ok_or(LinkError::RelocationTarget { object, index })?;
    let decoded =
        decode(config, bytes).map_err(|_| LinkError::RelocationTarget { object, index })?;
    let value = i128::from(symbol) + i128::from(relocation.addend());
    let replacement = match relocation.kind() {
        RelocationKind::LiImmediate => {
            let value = i32::try_from(value)
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            let register = match decoded.operands().first() {
                Some(Operand::Register(register)) => *register,
                _ => {
                    return Err(LinkError::RelocationTarget { object, index });
                }
            };
            Instruction::new(
                config,
                Opcode::Li,
                &[Operand::Register(register), Operand::Immediate(value)],
            )
        }
        RelocationKind::PcRelativeBranch => {
            let delta = value - i128::from(target + 8);
            if delta % 4 != 0 {
                return Err(LinkError::RelocationArithmetic { object, index });
            }
            let displacement = i32::try_from(delta / 4)
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            match decoded.opcode() {
                Opcode::Br => {
                    let condition = match decoded.operands().first() {
                        Some(Operand::Condition(condition)) => *condition,
                        _ => {
                            return Err(LinkError::RelocationTarget { object, index });
                        }
                    };
                    Instruction::new(
                        config,
                        Opcode::Br,
                        &[
                            Operand::Condition(condition),
                            Operand::Immediate(displacement),
                        ],
                    )
                }
                Opcode::Call => {
                    Instruction::new(config, Opcode::Call, &[Operand::Immediate(displacement)])
                }
                _ => {
                    return Err(LinkError::RelocationTarget { object, index });
                }
            }
        }
        RelocationKind::MemoryDisplacement32 => {
            let value = i32::try_from(value)
                .map_err(|_| LinkError::RelocationArithmetic { object, index })?;
            let (register, base) = match decoded.operands() {
                [Operand::Register(register), Operand::Memory { base, .. }, _] => {
                    (*register, *base)
                }
                _ => {
                    return Err(LinkError::RelocationTarget { object, index });
                }
            };
            Instruction::new(
                config,
                decoded.opcode(),
                &[
                    Operand::Register(register),
                    Operand::Memory {
                        base,
                        displacement: value,
                    },
                    decoded.operands()[2],
                ],
            )
        }
        _ => {
            return Err(LinkError::RelocationTarget { object, index });
        }
    }
    .map_err(|_| LinkError::RelocationTarget { object, index })?;
    let encoded =
        encode(config, &replacement).map_err(|_| LinkError::RelocationTarget { object, index })?;
    code[start..end].copy_from_slice(&encoded);
    Ok(())
}

/// Gathers every object's debug information into one block, with real addresses.
///
/// A mapping's offset is relative to its own object's text section, and the
/// linker concatenates those sections into one code region — so an offset means
/// nothing until it is added to the base that section was placed at. Doing that
/// here rather than leaving it to the loader is the point: the loader should not
/// have to know how the code was laid out in order to read a table about it.
///
/// Sources are merged by *text*, not by name. Two objects that compiled the same
/// file produce the same bytes, and giving them one entry makes the table smaller
/// and the resolution unambiguous; two objects with the same name but different
/// text are two sources, because a debugger that silently picked one of them
/// would show a line from the wrong file.
fn collect_debug(objects: &[ObjectFile], layout: &LinkedLayout) -> Result<DebugBlock, LinkError> {
    let placements = &layout.placements;
    let mut files: Vec<DebugFile> = Vec::new();
    let mut entries: Vec<DebugEntry> = Vec::new();
    for (object_index, object) in objects.iter().enumerate() {
        if object.debug_sources().is_empty() {
            continue;
        }
        let placement = placements
            .get(object_index)
            .ok_or(LinkError::DataOverflow)?;
        let mut local: Vec<u32> = Vec::new();
        local
            .try_reserve(object.debug_sources().len())
            .map_err(LinkError::Allocation)?;
        for source in object.debug_sources() {
            let found = files.iter().position(|file| file.text() == source.text());
            let index = match found {
                Some(index) => u32::try_from(index).map_err(|_| LinkError::TooManyDebugSources)?,
                None => {
                    files.push(DebugFile::new(
                        String::from(source.path()),
                        String::from(source.text()),
                    ));
                    let raw = files.len() - 1;
                    u32::try_from(raw).map_err(|_| LinkError::TooManyDebugSources)?
                }
            };
            local.push(index);
        }
        for mapping in object.debug_mappings() {
            let source = mapping.source().get();
            let source = local
                .get(source as usize)
                .copied()
                .ok_or(LinkError::UnknownDebugSource { value: source })?;
            let section = mapping.section().get();
            let base = placement
                .sections
                .get(section as usize)
                .map(|placed| placed.base)
                .ok_or(LinkError::UnknownDebugSection { value: section })?;
            // A mapping into a section the linker placed at zero — a `.bss` — is
            // about data, not code, and no program counter is ever inside one. It
            // is dropped rather than pointed at address zero, which is where the
            // image starts and where a wrong answer would be least noticeable.
            if base == 0 && layout.bss_size > 0 {
                continue;
            }
            let address = base
                .checked_add(mapping.offset())
                .ok_or(LinkError::DataOverflow)?;
            entries.push(DebugEntry {
                address,
                source,
                offset: mapping.source_offset(),
                length: mapping.source_length(),
            });
        }
    }
    DebugBlock::with_entries(files, entries).map_err(|_| LinkError::BadDebugBlock)
}
