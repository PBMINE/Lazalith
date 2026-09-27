//! Step 63: code generation from the Lazalith IR to a Lazalith object.
//!
//! # What this stage is
//!
//! The lowering produces a verified `lazalith_ir::Module` and, per function, a
//! `FrameLayout`. This stage turns both into a `.lzo` object through the
//! existing object model: `ObjectBuilder`, `Section`, `Symbol`, `Relocation`,
//! `DebugSource` and `CodeMapping`. It encodes instructions with
//! `lazalith_isa::encode`, the same encoder the assembler uses, and it resolves
//! syscalls through `lazalith_os_abi::Syscall`, the same table the kernel uses.
//! Neither the encoding nor the ABI is restated here.
//!
//! # Where values live
//!
//! There is no register allocator, and none is needed for correctness: **every
//! IR value lives in a frame slot**, and each instruction loads its operands and
//! stores its result. The prologue reserves that area, so a value's address is
//! `frame base + offset` like any other local's.
//!
//! This is what keeps Step 62's invariants true by construction rather than by
//! care:
//!
//! - The frame sits above the stack pointer the prologue leaves. The prologue
//!   moves SP down by the frame size it computed and the epilogue moves it back
//!   before `RET`, which is the convention `docs/lz64.md` already documents
//!   ("callee restores its entry SP before RET"). The first bytes of the frame are
//!   the two words a *callee* reads for arguments five and six, and
//!   `Intrinsic::FrameBase` is SP plus that reserve, so a local is still at
//!   `frame base + offset` with the offsets the lowering computed.
//! - A view is two words, so its slot is two words. A two-word value is never
//!   stored, copied, or returned as one word: the IR has no instruction that
//!   could, and this stage rejects one that tried.
//! - A load's width and signedness come from the IR, and the ISA's `LDZ`/`LDS` and
//!   `ST` express exactly that: a load extends, a store truncates, and arithmetic
//!   is at the word. Because a value is re-loaded from its slot at its declared
//!   width before every use, a 32-bit value's arithmetic is 32-bit arithmetic
//!   without a masking instruction anywhere.
//! - A bounds check compares the index and the length it was given, as unsigned
//!   values, and traps.
//!
//! # The calling convention
//!
//! `docs/lz64.md` already fixes one: `r0`–`r3` hold the first four word-sized
//! arguments, arguments five and six are at `[SP+8]` and `[SP+16]` at callee
//! entry, `r0` is a one-word return, `r0`–`r7` and NZCV are caller-saved, and
//! `r8`–`r15` are callee-saved. This stage uses exactly that and nothing else.
//! A Lazen function cannot return two words, because the frontend's return rule
//! only allows a one-word type, so `r0` is always a whole return value.
//!
//! A `CALL` pushes the return PC at `oldSP-8`, so a callee is entered one word
//! *below* its caller and the convention's `[SP+8]` and `[SP+16]` are the
//! caller's own `[SP+0]` and `[SP+8]`. The caller therefore owns those two words,
//! which is what the reserve at the base of every frame is for: a frame whose
//! first local sat at offset zero would have a call with five or more argument
//! words overwrite it.
//!
//! Only three caller-saved scratch registers are needed, because only a few
//! values are ever live at once: `r7` holds a frame address, `r6` and `r5` hold
//! operand words, and `r4` receives a result. No callee-saved register is
//! touched, so the prologue and epilogue are three instructions each and no
//! register has to be spilled around a call.
//!
//! # Labels, not block numbers
//!
//! A branch carries a displacement and the linker fills it in from a relocation
//! against a symbol, so no branch here needs its target's address: each one names
//! the target's label. That is what lets a loop's `continue` and `break` be label
//! names — a loop whose body contains an `if` creates blocks the loop itself
//! could not have predicted, and a name does not care.
//!
//! # Syscalls
//!
//! `docs/os-abi.md` fixes the entry state: `r0` is the word-sized syscall
//! number, `r1`–`r6` are the arguments, `r7` is reserved and must be zero, and a
//! returning service writes its status to `r0` and its payload to `r1`. A Lazen
//! declaration returns a one-word `i64`, so the result this stage stores is `r0`.
//! The ABI zero-extends the status to the word, so reading `r0` as a 64-bit
//! integer is the status and not half of an uninitialised register.

#![no_std]

extern crate alloc;

mod emit;
mod layout;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use lazalith_ir::{FrameLayout, Function, Module, Name, Type as IrType};
use lazalith_isa::{Instruction as MachineInstruction, Opcode, Operand, encode};

use lazalith_toolchain::{
    CodeMapping, DebugSource, ObjectBuilder, ObjectError, ObjectFile, Relocation, RelocationKind,
    Section, Symbol, SymbolBinding,
};
use lazalith_types::{ArchitectureConfig, SourceId, WordWidth};

use emit::FunctionEmitter;
use layout::{FunctionLayout, OUTGOING_ARGUMENT_BYTES};

/// How to generate code for a lowered program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodegenOptions {
    /// The machine to generate for. Only LZ64 is supported, because the lowering
    /// is LZ64-only and this stage will not generate LZ64 arithmetic for an LZ32
    /// machine.
    pub architecture: ArchitectureConfig,
    /// The source path recorded in the object's debug information.
    pub source_path: String,
}

impl CodegenOptions {
    /// Options for a 64-bit machine generating from `source_path`.
    pub fn lz64(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
        }
    }
}

/// Why a lowered module could not be turned into an object.
#[derive(Debug)]
pub enum CodegenError {
    /// The machine's word width has no code generation.
    UnsupportedArchitecture {
        /// The requested word width.
        word: WordWidth,
    },
    /// A function has no reported frame layout, so its prologue cannot be sized.
    MissingFrame {
        /// The function's qualified name.
        function: String,
    },
    /// An instruction has no representation on this machine.
    UnsupportedInstruction {
        /// The function being generated.
        function: String,
        /// What the instruction was.
        detail: String,
    },
    /// A value's type has no size on this machine.
    UnsupportedValueType {
        /// The function being generated.
        function: String,
        /// What the type was.
        detail: String,
    },
    /// A displacement or immediate does not fit the machine's encoding.
    EncodingRange {
        /// The function being generated.
        function: String,
        /// What did not fit.
        detail: String,
    },
    /// A call named something that is not in the module.
    UnknownTarget {
        /// The function containing the call.
        function: String,
        /// What was called.
        name: String,
    },
    /// A call to an extern the ABI has not numbered.
    UnnumberedSyscall {
        /// The function containing the call.
        function: String,
        /// The extern's name.
        name: String,
    },
    /// A data address named a segment the module does not have.
    UnknownData {
        /// The function containing the address.
        function: String,
        /// The missing segment.
        name: String,
    },
    /// A frame does not fit the machine's word.
    FrameTooLarge {
        /// The function whose frame is too large.
        function: String,
        /// How large it is.
        size: u64,
    },
    /// The program has more than one source file, and one debug path cannot
    /// describe both.
    MultipleSources {
        /// How many distinct sources the spans name.
        count: usize,
    },
    /// A span reaches past the text the caller supplied.
    ///
    /// The object carries the text so it is self-describing, and a mapping is an
    /// offset into that text. An offset past the end of it is unresolvable, so it
    /// is refused while the program is being generated rather than surfacing as a
    /// debugger showing the wrong line.
    SourceOutOfRange {
        /// The end of the offending span.
        length: u32,
        /// How long the text actually is.
        available: u32,
    },
    /// The object model refused something.
    Object(ObjectError),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedArchitecture { word } => write!(
                f,
                "no code generation for a {}-bit machine: the lowering is 64-bit only",
                word.bits()
            ),
            Self::MissingFrame { function } => {
                write!(
                    f,
                    "{function} has no frame layout, so its prologue cannot be sized"
                )
            }
            Self::UnsupportedInstruction { function, detail } => {
                write!(f, "{function}: no machine code for {detail}")
            }
            Self::UnsupportedValueType { function, detail } => {
                write!(
                    f,
                    "{function}: no machine representation for a value of type {detail}"
                )
            }
            Self::EncodingRange { function, detail } => {
                write!(
                    f,
                    "{function}: {detail} does not fit the machine's encoding"
                )
            }
            Self::UnknownTarget { function, name } => {
                write!(
                    f,
                    "{function} calls {name}, which the module does not define"
                )
            }
            Self::UnnumberedSyscall { function, name } => write!(
                f,
                "{function} calls the syscall {name}, which the ABI has not numbered"
            ),
            Self::UnknownData { function, name } => write!(
                f,
                "{function} takes the address of the data segment {name}, which the module \
                 does not have"
            ),
            Self::FrameTooLarge { function, size } => write!(
                f,
                "{function} needs a {size}-byte frame, and a machine word's immediate cannot \
                 address it"
            ),
            Self::MultipleSources { count } => write!(
                f,
                "the program's spans name {count} sources, and one debug path cannot describe \
                 them all"
            ),
            Self::SourceOutOfRange { length, available } => write!(
                f,
                "a source span reaches byte {length}, and the source is {available} bytes"
            ),
            Self::Object(error) => write!(f, "the object model refused this program: {error}"),
        }
    }
}

impl Error for CodegenError {}

impl From<ObjectError> for CodegenError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

impl CodegenError {
    /// Names the function an error came from.
    ///
    /// The layout is computed before the emitter exists, so an error from there
    /// has no function of its own; this fills it in rather than leaving the
    /// reader with a message about an unnamed function.
    pub fn with_function(self, function: &str) -> Self {
        match self {
            Self::MissingFrame { .. } => Self::MissingFrame {
                function: String::from(function),
            },
            Self::FrameTooLarge { size, .. } => Self::FrameTooLarge {
                function: String::from(function),
                size,
            },
            Self::UnsupportedInstruction { detail, .. } => Self::UnsupportedInstruction {
                function: String::from(function),
                detail,
            },
            Self::UnsupportedValueType { detail, .. } => Self::UnsupportedValueType {
                function: String::from(function),
                detail,
            },
            Self::EncodingRange { detail, .. } => Self::EncodingRange {
                function: String::from(function),
                detail,
            },
            other => other,
        }
    }
}

/// What code generation produced: the object, and what a runtime needs to know
/// about it.
///
/// The frame sizes are part of the result rather than an internal detail: a
/// caller that loads this object has to know how much stack its entry function
/// needs, and a linker or a loader cannot work that out from an object whose
/// prologue subtracts a constant.
#[derive(Clone, Debug)]
pub struct Program {
    object: ObjectFile,
    frames: Vec<GeneratedFrame>,
}

/// One function's generated frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedFrame {
    /// The function's qualified name.
    pub function: String,
    /// The whole frame in bytes: the reserve for outgoing argument words, the
    /// lowering's own frame, and the area this stage adds for IR values.
    pub frame_size: u32,
    /// The lowering's frame on its own, before the reserve and the value area.
    pub reported_frame_size: u32,
    /// The bytes reserved at the base of the frame for arguments passed on the
    /// stack.
    ///
    /// These are reported apart from the value area because they are not this
    /// stage's storage for values: they are space a *callee* reads at the
    /// caller's own `[SP+0]` and `[SP+8]`, and a stack-size calculation that
    /// counted them as value slots would be right by accident.
    pub outgoing_arguments: u32,
    /// How many bytes the value area adds.
    pub value_area: u32,
    /// The frame's offset from the text section where the code begins.
    pub code_offset: u64,
}

impl Program {
    /// The object.
    pub fn object(&self) -> &ObjectFile {
        &self.object
    }

    /// The generated frame of one function.
    pub fn frame(&self, function: &str) -> Option<&GeneratedFrame> {
        self.frames.iter().find(|frame| frame.function == function)
    }

    /// The generated frames, in module order.
    pub fn frames(&self) -> &[GeneratedFrame] {
        &self.frames
    }
}

/// Generates Lazalith object code from a lowered program.
///
/// The module has already been through the IR verifier, so this stage does not
/// repeat those checks; it fails instead on anything the machine cannot
/// represent, and never approximates.
///
/// The three pieces are the backend's whole input: a verified module, the frame
/// each function's values live in, and the name of the function the program
/// starts at. None of them mentions a source language, so this is the same entry
/// point for a Lazen program and a C one. It used to take the Lazen front end's
/// own `Lowered` struct, which meant a C program could only be compiled by
/// importing Lazen's types — a dependency pointing from the backend to one
/// language, in a project with two.
pub fn generate(
    module: &Module,
    frames: &[FrameLayout],
    entry: &str,
    options: &CodegenOptions,
    source_text: &str,
) -> Result<Program, CodegenError> {
    if options.architecture.word_width() != WordWidth::W64 {
        return Err(CodegenError::UnsupportedArchitecture {
            word: options.architecture.word_width(),
        });
    }
    validate_debug_source(module, source_text)?;
    let frame_table = frame_table(module, frames)?;
    let mut generated = Generated::new(&options.architecture, &module.data);
    generated.data_segments()?;
    let mut reported = Vec::new();
    for (function, frame) in module.functions.iter().zip(frame_table.iter()) {
        reported.push((function.name.clone(), generated.function(function, *frame)?));
    }
    let object = generated.finish(
        module,
        frames,
        entry,
        &options.source_path.clone(),
        source_text,
    )?;
    let records = reported
        .into_iter()
        .map(|(function, emitted)| GeneratedFrame {
            frame_size: emitted.total,
            reported_frame_size: emitted.reported,
            outgoing_arguments: OUTGOING_ARGUMENT_BYTES,
            value_area: emitted
                .total
                .saturating_sub(emitted.reported)
                .saturating_sub(OUTGOING_ARGUMENT_BYTES),
            code_offset: emitted.code_offset,
            function,
        })
        .collect();
    Ok(Program {
        object,
        frames: records,
    })
}

/// Checks that a lowered program's spans can be described by one source.
///
/// Two things have to hold for a debug mapping to mean anything: every span names
/// the same file, because one path cannot describe two; and no span reaches past
/// the end of the text, because a mapping is an offset into that text and an offset
/// past its end resolves to no line at all. Both are checked here, while the
/// program is being generated, rather than surfacing later as a debugger showing
/// the wrong one.
///
/// The *text* comes from the caller, because the spans only carry offsets into it
/// and this stage has no source manager. That is deliberate: the text goes into
/// the object so the object is self-describing, and whoever built the program is
/// the only thing that still has it.
fn validate_debug_source(module: &Module, source_text: &str) -> Result<(), CodegenError> {
    let mut ids: Vec<SourceId> = Vec::new();
    let mut length = 0u32;
    let mut note = |span: &lazalith_types::SourceSpan| {
        if !ids.contains(&span.id()) {
            ids.push(span.id());
        }
        length = length.max(span.end().as_u32());
    };
    for function in &module.functions {
        if let Some(span) = &function.span {
            note(span);
        }
    }
    for segment in &module.data {
        if let Some(span) = &segment.span {
            note(span);
        }
    }
    if ids.len() > 1 {
        return Err(CodegenError::MultipleSources { count: ids.len() });
    }
    // A span that reaches past the text it came from is a bug in the front end,
    // and carrying the text anyway would put an offset in the object that no
    // reader could resolve — so it is refused here rather than discovered by a
    // debugger showing the wrong line.
    if usize::try_from(length).unwrap_or(usize::MAX) > source_text.len() {
        return Err(CodegenError::SourceOutOfRange {
            length,
            available: u32::try_from(source_text.len()).unwrap_or(u32::MAX),
        });
    }
    Ok(())
}

/// The frame layouts, in the module's function order.
fn frame_table<'a>(
    module: &'a Module,
    frames: &'a [FrameLayout],
) -> Result<Vec<Option<&'a FrameLayout>>, CodegenError> {
    let mut table = Vec::new();
    for function in &module.functions {
        // A declared extern has no body and therefore no frame: its one block is
        // a trap. A defined function without one is an error, because a prologue
        // sized from a missing frame would be a guess.
        let frame = match frames.iter().find(|frame| frame.function == function.name) {
            Some(frame) => Some(frame),
            None if function.linkage == lazalith_ir::Linkage::External => None,
            None => {
                return Err(CodegenError::MissingFrame {
                    function: function.name.clone(),
                });
            }
        };
        table.push(frame);
    }
    Ok(table)
}

/// A relocation the emitter recorded against a label it registered.
#[derive(Clone, Debug)]
struct PendingRelocation {
    label: Name,
    kind: RelocationKind,
    offset: u64,
    addend: i64,
}

/// What emitting one function reported.
#[derive(Clone, Copy, Debug)]
struct Emitted {
    total: u32,
    reported: u32,
    code_offset: u64,
}

/// A code offset and the source range it came from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingMapping {
    pub(crate) offset: u64,
    pub(crate) start: u32,
    pub(crate) length: u32,
}

struct Generated<'a> {
    architecture: &'a ArchitectureConfig,
    /// The module's data segments, for the symbols a `DataAddress` names.
    segments: &'a [lazalith_ir::DataSegment],
    text: Vec<u8>,
    data: Vec<u8>,
    /// The alignment the data section needs, from the segments in it.
    data_alignment: u64,
    /// Whether the module has any data segment at all.
    ///
    /// This is not the same as `data` being non-empty: a segment can have no
    /// bytes, and an image still needs a data section to place it in.
    has_data: bool,
    /// `(symbol name, text offset)` for a block or function label.
    text_labels: Vec<(Name, u64)>,
    /// `(function name, text offset)` for each function, whose symbol the caller
    /// creates with the linkage the IR gave it.
    function_entries: Vec<(Name, u64)>,
    /// `(symbol name, data offset, size)` for a data segment.
    data_labels: Vec<(Name, u64, u64)>,
    relocations: Vec<PendingRelocation>,
    mappings: Vec<PendingMapping>,
}

impl<'a> Generated<'a> {
    fn new(architecture: &'a ArchitectureConfig, segments: &'a [lazalith_ir::DataSegment]) -> Self {
        Self {
            architecture,
            segments,
            text: Vec::new(),
            data: Vec::new(),
            data_alignment: 1,
            has_data: false,
            text_labels: Vec::new(),
            function_entries: Vec::new(),
            data_labels: Vec::new(),
            relocations: Vec::new(),
            mappings: Vec::new(),
        }
    }

    /// Lays the module's data segments out and records their symbols.
    ///
    /// This runs once for the module rather than once per function, because a
    /// segment is one object of bytes however many functions name it, and
    /// `DataAddress` reaches it by symbol.
    ///
    /// A segment with no bytes still counts as a segment. An empty string
    /// literal interns to a zero-length segment, and its address is still an
    /// address the linker has to place: a program holding `""` and asking for its
    /// pointer is asking for a real address, and an image with no data section at
    /// all would have nowhere to put one.
    fn data_segments(&mut self) -> Result<(), CodegenError> {
        let mut alignment = 1u64;
        for segment in self.segments {
            let required = u64::from(segment.alignment.max(1));
            // The machine faults on a misaligned access, so the padding is
            // emitted rather than hoped for: a segment that asked for eight and
            // got one would be read wrongly or not at all.
            while u64::try_from(self.data.len()).unwrap_or(u64::MAX) % required != 0 {
                self.data.push(0);
            }
            let offset = self.data.len() as u64;
            self.data.extend_from_slice(&segment.bytes);
            self.data_labels
                .push((segment.name.clone(), offset, segment.bytes.len() as u64));
            alignment = alignment.max(required);
            self.has_data = true;
        }
        self.data_alignment = alignment;
        Ok(())
    }

    /// Generates one function's code and records its symbols and relocations.
    fn function(
        &mut self,
        function: &Function,
        frame: Option<&FrameLayout>,
    ) -> Result<Emitted, CodegenError> {
        let reported = frame.map_or(0, |frame| frame.size);
        let owned;
        let frame = match frame {
            Some(frame) => frame,
            None => {
                // A declared extern has no locals at all, so an empty frame is not a
                // guess: there is nothing to reserve.
                owned = FrameLayout {
                    function: function.name.clone(),
                    size: 0,
                    slots: Vec::new(),
                };
                &owned
            }
        };
        let layout = FunctionLayout::new(function, frame, self.architecture)
            .map_err(|error| error.with_function(&function.name))?;
        let first_mapping = self.mappings.len();
        let mut emitter = FunctionEmitter::new(
            function,
            self.segments,
            &layout,
            self.architecture,
            &mut self.text,
            &mut self.text_labels,
            &mut self.relocations,
            &mut self.mappings,
        );
        let offset = emitter.run()?;
        self.function_entries
            .push((function_symbol(&function.name), offset));
        // The function's own span is inserted *first*, covering the prologue the
        // per-instruction map does not reach. Inserting rather than pushing keeps
        // the table in address order, which is what the linker's fix-up and the
        // debugger's backwards walk both assume, and assuming it in one place and
        // providing it in another is how a table ends up half sorted.
        if let Some(span) = &function.span {
            let from = span.start().as_u32();
            let to = span.end().as_u32();
            self.mappings.insert(
                first_mapping,
                PendingMapping {
                    offset,
                    start: from,
                    length: to.saturating_sub(from),
                },
            );
        }
        Ok(Emitted {
            total: layout.total,
            reported,
            code_offset: offset,
        })
    }

    /// Assembles the gathered parts into an object.
    fn finish(
        mut self,
        module: &Module,
        _frames: &[FrameLayout],
        entry: &str,
        source: &str,
        source_text: &str,
    ) -> Result<ObjectFile, CodegenError> {
        let text_section = Section::text("text", *self.architecture, &self.text)?;
        let data_section = if !self.has_data {
            None
        } else {
            Some(Section::read_only_data(
                "rodata",
                self.data_alignment,
                &self.data,
            )?)
        };
        let mut builder = ObjectBuilder::new(*self.architecture);
        let text_index = builder.add_section(text_section)?;
        let data_index = match data_section {
            Some(section) => Some(builder.add_section(section)?),
            None => None,
        };

        // Block and function labels are local to this object; functions are also
        // global when the IR says so, so another object can call them.
        let mut order: Vec<(
            Name,
            SymbolBinding,
            lazalith_toolchain::SectionIndex,
            u64,
            u64,
        )> = Vec::new();
        for (name, offset) in &self.text_labels {
            order.push((name.clone(), SymbolBinding::Local, text_index, *offset, 0));
        }
        for (name, offset, size) in &self.data_labels {
            let index = data_index.ok_or_else(|| CodegenError::UnknownData {
                function: String::from("<module>"),
                name: name.clone(),
            })?;
            order.push((name.clone(), SymbolBinding::Local, index, *offset, *size));
        }
        for function in &module.functions {
            let name = function_symbol(&function.name);
            let offset = self
                .function_entries
                .iter()
                .find(|(label, _)| *label == name)
                .map(|(_, offset)| *offset)
                .ok_or_else(|| CodegenError::MissingFrame {
                    function: function.name.clone(),
                })?;
            let binding = match function.linkage {
                lazalith_ir::Linkage::Global | lazalith_ir::Linkage::External => {
                    SymbolBinding::Global
                }
                lazalith_ir::Linkage::Local => SymbolBinding::Local,
            };
            // The image's entry point is reached by whoever loads the image, not
            // by another module, so it has to be callable from outside this object
            // even when the Lazen declaration is private. `pub` governs Lazen-level
            // visibility between modules; a private `main` is still the only way in
            // for the loader, and a local symbol would leave the image with an
            // entry no startup code could call.
            let binding = if function.name == entry {
                SymbolBinding::Global
            } else {
                binding
            };
            order.push((name, binding, text_index, offset, 0));
        }
        let mut indices: Vec<(Name, lazalith_toolchain::SymbolIndex)> = Vec::new();
        for (name, binding, section, offset, size) in &order {
            let symbol = Symbol::section_defined(name.clone(), *binding, *section, *offset, *size);
            indices.push((name.clone(), builder.add_symbol(symbol)?));
        }
        for pending in &self.relocations {
            let index = indices
                .iter()
                .find(|(name, _)| *name == pending.label)
                .map(|(_, index)| *index)
                .ok_or_else(|| CodegenError::UnknownTarget {
                    function: String::from("<module>"),
                    name: pending.label.clone(),
                })?;
            builder.add_relocation(Relocation::new(
                index,
                text_index,
                pending.kind,
                pending.offset,
                pending.addend,
            ))?;
        }
        let source = builder.add_debug_source(DebugSource::new(source, source_text))?;
        for mapping in core::mem::take(&mut self.mappings) {
            builder.add_debug_mapping(CodeMapping::new(
                text_index,
                mapping.offset,
                source,
                mapping.start,
                mapping.length,
            ))?;
        }
        let entry_name = function_symbol(entry);
        let entry = indices
            .iter()
            .find(|(name, _)| *name == entry_name)
            .map(|(_, index)| *index)
            .ok_or_else(|| CodegenError::MissingFrame {
                function: entry.to_string(),
            })?;
        builder.set_entry(entry)?;
        Ok(builder.build()?)
    }
}

/// The prefix the compiler puts on an ABI syscall's name in the IR.
///
/// A Lazen function and an ABI syscall can share a name — a program that declares
/// `fn read` collides with the ABI's `read` — and the IR keeps every function in
/// one namespace. A dot cannot appear in a Lazen qualified name, which either has
/// no separator or joins its modules with `::`, so the prefix cannot collide with a
/// function however the language grows. The same reasoning is behind the `fn.`
/// prefix on a function's symbol.
pub(crate) const SYSCALL_PREFIX: &str = "syscall.";

/// A function's symbol name in the object.
pub(crate) fn function_symbol(name: &str) -> Name {
    let mut symbol = String::from("fn.");
    symbol.push_str(name);
    symbol
}

/// The syscall a name maps to, from the compiler's one name table.
///
/// The frontend already maps a source name to a `Syscall` and refuses a name the
/// ABI has no number for, so the same table answers here, and it answers with
/// the whole `Syscall` rather than only its number: the ABI's entry state also
/// says which argument registers a given service requires to be zero, and reading
/// that from a second table here would be a table that could disagree with the
/// first about which registers a call means.
pub(crate) fn abi_syscall(name: &str) -> Option<lazalith_os_abi::Syscall> {
    lazalith_os_abi::abi_syscall(name)
}

/// The size in bytes of a value of this IR type, or `None` when the machine has
/// no representation for it.
pub(crate) fn value_size(ty: &IrType) -> Option<u32> {
    ty.size_in_bytes()
}

/// The alignment a value of this IR type needs in the frame.
///
/// The machine faults on a misaligned access, so this is a correctness
/// requirement and not a preference.
pub(crate) fn value_alignment(ty: &IrType) -> u64 {
    u64::from(ty.alignment_in_bytes())
}

/// Encodes one machine instruction and appends it to `code`.
///
/// Every instruction this stage emits goes through the ISA's own encoder, so
/// there is exactly one definition of an instruction's encoding.
pub(crate) fn emit_instruction(
    architecture: ArchitectureConfig,
    code: &mut Vec<u8>,
    opcode: Opcode,
    operands: &[Operand],
) -> Result<u64, lazalith_isa::InstructionError> {
    let instruction = MachineInstruction::new(architecture, opcode, operands)?;
    let bytes = encode(architecture, &instruction)?;
    let offset = code.len() as u64;
    code.extend_from_slice(&bytes);
    Ok(offset)
}

/// The relocation kind for a branch, which the linker patches for `BR` and
/// `CALL` alike.
pub(crate) const BRANCH_RELOCATION: RelocationKind = RelocationKind::PcRelativeBranch;

/// A data address is a `LI` whose immediate the linker fills in.
pub(crate) const DATA_RELOCATION: RelocationKind = RelocationKind::LiImmediate;
