#![no_std]

extern crate alloc;

mod assembler;
mod disassembler;
mod linker;
mod lzo;
mod manifest;
mod object;

pub use assembler::{AssemblyError, assemble, assemble_for, assemble_named};
pub use disassembler::{
    DisassembledInstruction, DisassemblyError, ObjectDisassembly, disassemble, disassemble_object,
    disassemble_one,
};
pub use linker::{LinkError, LinkOptions, LinkedProgram, link_objects};
pub use manifest::{
    Architecture, MANIFEST_NAME, Manifest, ManifestError, ResolveError, Resolved,
    ResolvedDependency, Resolver, VersionRequirement,
};
pub use object::{
    CodeMapping, DebugSource, DebugSourceIndex, OBJECT_DEBUG_MAPPING_ENTRY_SIZE,
    OBJECT_DEBUG_SOURCE_ENTRY_SIZE, OBJECT_FORMAT_VERSION, OBJECT_HEADER_SIZE, OBJECT_ISA_VERSION,
    OBJECT_MAGIC, OBJECT_MAX_DEBUG_MAPPINGS, OBJECT_MAX_DEBUG_SOURCES, OBJECT_MAX_FILE_SIZE,
    OBJECT_MAX_MATERIALIZED_NAME_BYTES, OBJECT_MAX_RELOCATIONS, OBJECT_MAX_SECTIONS,
    OBJECT_MAX_SYMBOLS, OBJECT_NO_ENTRY, OBJECT_RELOCATION_ENTRY_SIZE, OBJECT_SECTION_ENTRY_SIZE,
    OBJECT_SYMBOL_ENTRY_SIZE, ObjectBuilder, ObjectError, ObjectFile, ObjectTarget, Relocation,
    RelocationKind, Section, SectionIndex, SectionKind, Symbol, SymbolBinding, SymbolIndex,
    SymbolKind, ToolchainError, assemble_and_link, link_object,
};
