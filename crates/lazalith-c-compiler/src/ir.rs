//! Lowering C to Lazalith IR.
//!
//! # What this stage owns
//!
//! The checked program has types but no instructions, no labels and no control
//! flow. This stage turns it into a verified `lazalith_ir::Module` plus a frame
//! layout, and it owns every representation decision that needs one:
//!
//! - **Every named value lives in the frame.** There is no register allocator,
//!   so a name is a slot, a slot is an offset, and a use is a load from the
//!   frame base plus that offset. A parameter's slot is marked
//!   `is_parameter`, which is what tells the backend's prologue to fill it from
//!   the argument registers.
//! - **An array is its own storage.** `[T; N]` becomes an IR `Record` with one
//!   field per element, because that is the only aggregate the IR has and
//!   because an array's element offsets are the array's own rather than a
//!   pointer's.
//! - **A string literal is a symbol, not a number.** Its bytes are a data
//!   segment — including the terminating null — and its address is
//!   `Instruction::DataAddress`, so the linker relocates it and a debugger can
//!   still find the source that wrote it.
//! - **No phi nodes.** A value produced by an `if`, a loop, or a
//!   short-circuiting operator is written by each arm into a frame temporary and
//!   read after the join.
//! - **`&&` and `||` branch.** An eager evaluation would run the right side when
//!   the left side already decides the answer, and that side can trap.
//!
//! # Namespaces
//!
//! A lowered C function is `c.<name>` and a lowered syscall is
//! `syscall.<name>`. Both are prefixes, and both exist for the same reason: the
//! IR has one flat symbol namespace, so a C function called `write` and the
//! ABI's `write` would otherwise be one name with two definitions, and the
//! linker would refuse the object. The backend strips the syscall prefix.
//!
//! # What this stage refuses
//!
//! Each refusal names the machine limit, not just the C construct:
//!
//! - **`goto`.** A `goto` can jump backwards, and the IR's blocks are built in
//!   the order the body is walked, so a backwards jump would need a second pass
//!   over the function. A loop is a jump the IR can express directly.
//! - **A call through a function pointer.** `CALL` takes a displacement and
//!   `CALLR` takes one register, and neither reaches a function whose address is
//!   only known at run time.
//! - **A call needing more than six argument words.** Four arguments go in
//!   registers and two on the stack; a wider type uses more than one word.
//! - **An assignment of a whole `struct` or `union`.** The backend's store is
//!   one machine width wide, so a multiword value has no single instruction
//!   that writes it. Reading one is fine, and a member of one is fine.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;

use lazalith_diagnostics::Diagnostic;
use lazalith_ir::{
    BinaryOp as IrBinaryOp, BlockId, CallArg, CallTarget, ComparisonOp as IrComparisonOp,
    ConstValue, DataSegment, FrameLayout, FrameSlot, FunctionBuilder, Instruction, Intrinsic,
    IrError, Linkage, LoadWidth, MemorySpace, Module, ModuleBuilder, Name, Parameter, RecordField,
    ReturnValue, SlotPurpose, StoreWidth, Terminator, Type as IrType, UnaryOp, ValueId,
    instruction_result_type,
};

use crate::ast::{
    BinaryOp, Block as AstBlock, BlockItem, Designator, Expression, ForInit, InitItem, Initializer,
    Statement, VarDecl,
};
use crate::ctypes::{CType, RecordType};
use crate::types::{CheckedCProgram, CheckedFunction, CheckedVariable};

/// The prefix a lowered C function's symbol carries in the IR.
pub const FUNCTION_PREFIX: &str = "c.";

/// The prefix a lowered ABI syscall's symbol carries in the IR.
///
/// Distinct from [`FUNCTION_PREFIX`] so a C function called `write` and the
/// ABI's `write` are two symbols. The backend strips this one, because the
/// backend is what turns it into a number.
pub const SYSCALL_PREFIX: &str = "syscall.";

/// The most argument words a call may use: four registers and two stack words.
pub const MAX_ARGUMENT_WORDS: u32 = 6;

/// C's diagnostic codes for this stage.
pub mod codes {
    /// A construct this machine cannot represent.
    pub const UNSUPPORTED: &str = "C0501";
    /// A call with more argument words than the ABI has.
    pub const TOO_MANY_ARGUMENTS: &str = "C0503";
}

/// Why a program could not be lowered.
#[derive(Debug)]
pub enum LowerError {
    /// A construct this machine cannot represent.
    Unsupported {
        /// What the program wrote.
        what: String,
        /// Why it cannot be done here.
        why: String,
    },
    /// The IR verifier refused.
    Ir(IrError),
    /// A call that needs more argument words than the ABI has.
    TooManyArguments {
        /// How many words the arguments need.
        words: u32,
    },
    /// A type the IR cannot hold.
    UnsupportedType {
        /// The type, as C writes it.
        ty: String,
    },
    /// The program has no `main`.
    NoEntryPoint,
    /// A function's body could not be lowered.
    Function {
        /// Which function.
        function: String,
        /// Every refusal in it, joined.
        detail: String,
    },
    /// Out of memory.
    Allocation,
}

impl fmt::Display for LowerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { what, why } => {
                write!(formatter, "{what} cannot be compiled: {why}")
            }
            Self::Ir(error) => write!(formatter, "the IR rejected this program: {error}"),
            Self::TooManyArguments { words } => write!(
                formatter,
                "a call needs {words} argument words, and the ABI has {MAX_ARGUMENT_WORDS}"
            ),
            Self::UnsupportedType { ty } => {
                write!(formatter, "the IR has no representation for `{ty}`")
            }
            Self::NoEntryPoint => formatter.write_str("there is no `main` to start at"),
            Self::Function { function, detail } => {
                write!(
                    formatter,
                    "the body of `{function}` cannot be compiled: {detail}"
                )
            }
            Self::Allocation => formatter.write_str("out of memory"),
        }
    }
}

impl Error for LowerError {}

impl From<IrError> for LowerError {
    fn from(error: IrError) -> Self {
        Self::Ir(error)
    }
}

/// A lowered C program.
#[derive(Clone, Debug)]
pub struct Lowered {
    /// The verified IR module.
    pub module: Module,
    /// One frame per lowered function, in module order.
    pub frames: Vec<FrameLayout>,
    /// The IR name of the function the runtime starts at.
    pub entry: String,
    /// A data segment per string literal, in the order they were interned.
    pub strings: Vec<String>,
}

impl Lowered {
    /// The frame layout for a lowered function.
    pub fn frame(&self, function: &str) -> Option<&FrameLayout> {
        self.frames.iter().find(|frame| frame.function == function)
    }
}

/// A C function's name in the IR's namespace.
pub fn ir_name(name: &str) -> String {
    format!("{FUNCTION_PREFIX}{name}")
}

/// A global's data segment name.
pub fn global_segment(name: &str) -> String {
    format!("g.{name}")
}

/// Lowers a checked C program into a verified IR module.
pub fn lower(program: &CheckedCProgram) -> Result<Lowered, LowerError> {
    if !program
        .functions
        .iter()
        .any(|function| function.name == "main")
    {
        return Err(LowerError::NoEntryPoint);
    }
    let mut lowerer = Lowerer {
        program,
        module: ModuleBuilder::new("c"),
        frames: Vec::new(),
        strings: Vec::new(),
        syscall_names: Vec::new(),
    };
    let module = lowerer.run()?;
    let frames = core::mem::take(&mut lowerer.frames);
    let strings = core::mem::take(&mut lowerer.strings);
    Ok(Lowered {
        module,
        frames,
        entry: ir_name("main"),
        strings,
    })
}

struct Lowerer<'a> {
    program: &'a CheckedCProgram,
    module: ModuleBuilder,
    frames: Vec<FrameLayout>,
    strings: Vec<String>,
    /// ABI syscalls the program called, in the order it first called them.
    syscall_names: Vec<String>,
}

impl<'a> Lowerer<'a> {
    fn run(&mut self) -> Result<Module, LowerError> {
        for global in &self.program.globals {
            self.global(global)?;
        }
        for function in &self.program.functions {
            self.function(function)?;
        }
        // Every syscall the program called is declared here rather than in the
        // function that called it. The IR verifier resolves a `Syscall` target as
        // a function in the module, and a declaration is the IR's own way of
        // saying "this name exists and has no body here" — the backend turns it
        // into the machine's `SYSCALL` and the number the ABI gave it.
        //
        // The set is collected first and the declarations added at the end,
        // because a function builder borrows the module and a declaration needs
        // it back.
        for name in core::mem::take(&mut self.syscall_names) {
            self.syscall_declaration(&name)?;
        }
        let module = core::mem::replace(&mut self.module, ModuleBuilder::new("discard"));
        module.finish().map_err(LowerError::from)
    }

    /// A declaration for one ABI call, with no body.
    ///
    /// The declaration carries the ABI's own parameter list, because the IR
    /// verifier checks a call's arity against the function it names. A
    /// declaration with no parameters would refuse every syscall call with
    /// arguments — which is the verifier being right about a declaration that
    /// says nothing.
    fn syscall_declaration(&mut self, name: &str) -> Result<(), LowerError> {
        let ir_name = Name::from(format!("{SYSCALL_PREFIX}{name}"));
        let signature = crate::types::abi_signature(name);
        let params: Vec<Parameter> = signature
            .as_ref()
            .map(|ty| match ty {
                CType::Function(function) => function
                    .params
                    .iter()
                    .map(|param| Parameter {
                        name: String::new(),
                        ty: ir_type(param).unwrap_or(IrType::Int {
                            bits: 64,
                            signed: true,
                        }),
                    })
                    .collect(),
                _ => Vec::new(),
            })
            .unwrap_or_default();
        let result = ir_type(&CType::long()).unwrap_or(IrType::Int {
            bits: 64,
            signed: true,
        });
        let mut builder = self
            .module
            .function(&ir_name, Linkage::External, params, result)?;
        builder
            .switch_to_block("declaration")
            .map_err(LowerError::from)?;
        builder
            .terminate(Terminator::Unreachable)
            .map_err(LowerError::from)?;
        self.module.add_function(builder.finish()?)?;
        Ok(())
    }

    /// Lays a global out as a data segment.
    ///
    /// A global with no initialiser is a segment with no bytes, which the
    /// object format writes as `.bss` — and C says a file-scope object starts
    /// as zero, so this is the right default and not a guess.
    fn global(&mut self, global: &CheckedVariable) -> Result<(), LowerError> {
        let size = global
            .ty
            .size_in_bytes()
            .ok_or_else(|| LowerError::UnsupportedType {
                ty: global.ty.name(),
            })?;
        let mut bytes = vec![0u8; size as usize];
        if let Some(initial) = &global.initial {
            let image = self.constant_image(initial, &global.ty)?;
            let room = image.len().min(bytes.len());
            bytes[..room].copy_from_slice(&image[..room]);
        }
        self.module.add_data(DataSegment {
            name: global_segment(&global.name),
            bytes,
            alignment: global.ty.alignment_in_bytes(),
            span: None,
        })?;
        Ok(())
    }

    /// The bytes a static initialiser describes.
    fn constant_image(&mut self, initial: &Initializer, ty: &CType) -> Result<Vec<u8>, LowerError> {
        let size =
            ty.size_in_bytes()
                .ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })? as usize;
        let mut bytes = vec![0u8; size];
        self.write_constant(&mut bytes, 0, initial, ty)?;
        Ok(bytes)
    }

    fn write_constant(
        &mut self,
        bytes: &mut [u8],
        at: usize,
        initial: &Initializer,
        ty: &CType,
    ) -> Result<(), LowerError> {
        match initial {
            Initializer::Scalar(expression) => {
                if let Some(value) = constant(expression) {
                    write_integer(bytes, at, ty, value);
                }
                Ok(())
            }
            Initializer::List { items, .. } => match ty {
                CType::Array { element, .. } => {
                    let stride = element.size_in_bytes().unwrap_or(0) as usize;
                    for (index, item) in items.iter().enumerate() {
                        let target = match &item.designator {
                            Some(Designator::Index {
                                index: expression, ..
                            }) => constant(expression).unwrap_or(index as i64).max(0) as usize,
                            _ => index,
                        };
                        self.write_constant(bytes, at + target * stride, &item.value, element)?;
                    }
                    Ok(())
                }
                CType::Struct(_) | CType::Union(_) => {
                    let record = record_of(ty);
                    for (index, item) in items.iter().enumerate() {
                        let field = match &item.designator {
                            Some(Designator::Field { name, .. }) => {
                                record.fields.iter().find(|field| &field.name == name)
                            }
                            _ => record.fields.get(index),
                        };
                        let Some(field) = field else { continue };
                        self.write_constant(
                            bytes,
                            at + field.offset as usize,
                            &item.value,
                            &field.ty,
                        )?;
                    }
                    Ok(())
                }
                other => match items.first() {
                    // A braced scalar is one value: `int x = {1};`.
                    Some(first) => self.write_constant(bytes, at, &first.value, other),
                    None => Ok(()),
                },
            },
        }
    }

    /// Lowers one function.
    fn function(&mut self, function: &CheckedFunction) -> Result<(), LowerError> {
        let CType::Function(signature) = function.ty.clone() else {
            return Err(LowerError::UnsupportedType {
                ty: function.ty.name(),
            });
        };
        let params: Vec<Parameter> = function
            .parameters
            .iter()
            .map(|(name, ty)| Parameter {
                name: name.clone().unwrap_or_default(),
                ty: ir_type(ty).unwrap_or(IrType::Int {
                    bits: 64,
                    signed: true,
                }),
            })
            .collect();
        let result = ir_type(&signature.result).ok_or_else(|| LowerError::UnsupportedType {
            ty: signature.result.name(),
        })?;
        let mut builder = self.module.function(
            &ir_name(&function.name),
            Linkage::Global,
            params,
            result.clone(),
        )?;
        let mut emitter = Emitter {
            builder: &mut builder,
            scopes: vec![Scope::default()],
            slots: Vec::new(),
            cursor: 0,
            break_blocks: Vec::new(),
            continue_blocks: Vec::new(),
            refusals: Vec::new(),
            local_types: function
                .locals
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .collect(),
            program: self.program,
            produced: BTreeMap::new(),
            syscall_names: Vec::new(),
            strings: Vec::new(),
            block_number: 0,
        };
        // A parameter is a slot the prologue fills, and it is marked as one
        // before anything else is allocated, because the backend fills
        // parameters in declaration order and a temporary allocated first would
        // shift them.
        // The builder has no block selected until one is, and every instruction needs
        // somewhere to go. The entry block is the first one, and the IR's verifier
        // requires the entry block to be `blocks[0]` — which is why the switch
        // happens before anything is reserved, not after.
        emitter
            .builder
            .switch_to_block("entry")
            .map_err(LowerError::from)?;
        emitter.parameters(function);
        emitter.statements(&function.body.items);
        // Falling off the end of a function returns its declared result, and for a
        // `void` function there is no value to return. A `main` that falls off
        // the end returns zero because C says so, and a program that declares
        // another result and falls off the end is undefined behaviour that this
        // emits as zero rather than as whatever the register held.
        let _ = emitter
            .builder
            .terminate(Terminator::Return(if matches!(&result, IrType::Void) {
                ReturnValue::Void
            } else {
                ReturnValue::Value(ValueId::new(0).expect("value zero is always valid"))
            }));
        let frame = FrameLayout {
            function: ir_name(&function.name),
            size: emitter.cursor,
            slots: emitter.slots,
        };
        // The string segments are added now, with the function builder finished, so
        // that the module is free to be borrowed again. The terminating null is
        // part of each segment's bytes: a program that walks a string reads the
        // zero that ends it, and that zero has to be in the data rather than in
        // every reader's idea of what a string is.
        for name in &emitter.syscall_names {
            if !self.syscall_names.contains(name) {
                self.syscall_names.push(name.clone());
            }
        }
        for (value, name) in &emitter.strings {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            self.module.add_data(DataSegment {
                name: name.clone(),
                bytes,
                alignment: 1,
                span: None,
            })?;
            self.strings.push(value.clone());
        }
        if !emitter.refusals.is_empty() {
            return Err(LowerError::Function {
                function: function.name.clone(),
                // The refusal's *reason* is in the message, not only in its help. A refusal a
                // reader cannot act on is a bug report with the location left off, and
                // a `LowerError` has nowhere to put a help line — so the reason is
                // carried in the message itself.
                detail: emitter
                    .refusals
                    .iter()
                    .map(|diagnostic| {
                        let reason = diagnostic
                            .help()
                            .first()
                            .map(|help| help.message())
                            .unwrap_or_default();
                        alloc::format!("{} ({reason})", diagnostic.message())
                    })
                    .collect::<Vec<String>>()
                    .join("; "),
            });
        }
        self.frames.push(frame);
        self.module.add_function(builder.finish()?)?;
        Ok(())
    }
}

/// One lexical scope's slots.
#[derive(Clone, Debug, Default)]
struct Scope {
    names: BTreeMap<String, Local>,
}

/// A named local and where it lives.
#[derive(Clone, Debug)]
struct Local {
    offset: u32,
    ty: CType,
}

/// A block this module reserved.
#[derive(Clone, Debug)]
struct Block {
    id: BlockId,
    name: String,
}

struct Emitter<'a> {
    builder: &'a mut FunctionBuilder,
    scopes: Vec<Scope>,
    slots: Vec<FrameSlot>,
    cursor: u32,
    break_blocks: Vec<Block>,
    continue_blocks: Vec<Block>,
    refusals: Vec<Diagnostic>,
    /// Every block-scope local's type, taken from the type checker.
    local_types: BTreeMap<String, CType>,
    program: &'a CheckedCProgram,
    /// How wide every value this function produced is, and whether it is signed.
    ///
    /// The IR's own `value_type` can answer this, but only from a *finished*
    /// function, and a backend needs the answer while the function is being built.
    /// So the emitter records it as it goes. Without the *width* a store cannot choose
    /// between a plain store and a conversion; without the *signedness* a widening
    /// conversion cannot sign-extend, which is the difference between `(int)a`
    /// where `a` is a `char` and where it is an `unsigned char`.
    produced: BTreeMap<u32, Shape>,

    /// ABI syscalls this function called.
    ///
    /// They are collected here and added to the module by the caller, because a
    /// function builder borrows the module and a declaration needs it back.
    syscall_names: Vec<String>,

    /// String literals this function used, and the segment name each became.
    ///
    /// The *segments* are added by the caller, not here, because a function
    /// builder borrows the module and this cannot borrow it twice. Collecting the
    /// pairs and adding them once the function is finished is the only order that
    /// works, and it is also the order that lets a segment name be interned for the
    /// whole module rather than per function.
    strings: Vec<(String, Name)>,
    block_number: u32,
}

impl<'a> Emitter<'a> {
    /// Emits an instruction, translating the IR's own error, and records how wide
    /// the result is.
    ///
    /// The IR's error is the right one to report — it names a function, a block and
    /// a value — so it is wrapped rather than flattened into a string here.
    fn emit(&mut self, instruction: Instruction) -> Result<ValueId, LowerError> {
        let shape = ir_shape(&instruction);
        let value = self.builder.emit(instruction).map_err(LowerError::from)?;
        let _ = self.produced.insert(value.get(), shape);
        Ok(value)
    }

    /// How wide a value is, in bytes.
    ///
    /// A value this emitter did not record is assumed to be a whole word, which is
    /// the widest thing the machine holds: a store from a whole word into anything
    /// narrower converts, and a value wider than a word cannot exist.
    /// A value's width and signedness, as recorded when it was produced.
    fn shape_of(&self, value: ValueId) -> Shape {
        self.produced
            .get(&value.get())
            .copied()
            .unwrap_or(Shape::WORD)
    }

    /// How many bytes a `sizeof` in this program is, from the checker's table.
    ///
    /// A `sizeof` cannot be answered from the operand alone, so it is read from
    /// where the checker left it. Reaching for the operand's own type here would
    /// be the mistake: the operand is a `Name`, and a name read as a value is a
    /// *pointer*, so every `sizeof buffer` would come out a word.
    fn sizeof_of(&self, expression: &Expression) -> Result<u32, LowerError> {
        self.program
            .sizeofs
            .get(&crate::types::expression_span(expression).start().as_u32())
            .copied()
            .ok_or_else(|| LowerError::Unsupported {
                what: String::from("a `sizeof` the checker did not measure"),
                why: String::from(
                    "every `sizeof` is measured while the program is checked, so this is a \
                     compiler bug rather than a program error",
                ),
            })
    }

    /// Emits an instruction that produces no value.
    fn emit_effect(&mut self, instruction: Instruction) -> Result<(), LowerError> {
        self.builder
            .emit_effect(instruction)
            .map_err(LowerError::from)
    }

    /// Terminates the current block.
    fn terminate(&mut self, terminator: Terminator) -> Result<(), LowerError> {
        self.builder.terminate(terminator).map_err(LowerError::from)
    }

    /// Switches to a block the builder already reserved.
    ///
    /// The *name* is kept with the id because the builder keys its blocks by name:
    /// a name that is derived from the id would look for a block it never made,
    /// because the id is a position and the name is the key. Keeping the pair is
    /// what makes a reserved block reachable.
    fn switch_to(&mut self, block: &Block) -> Result<(), LowerError> {
        self.builder
            .switch_to_block(&block.name)
            .map_err(LowerError::from)?;
        Ok(())
    }

    /// Reserves a block and remembers its name.
    fn reserve(&mut self, name: &str) -> Result<Block, LowerError> {
        let id = self.builder.reserve_block(name).map_err(LowerError::from)?;
        Ok(Block {
            id,
            name: name.to_string(),
        })
    }

    // -- naming --

    /// A block name that cannot collide, because every block in a function has
    /// a different one.
    fn block_name(&mut self, kind: &str) -> String {
        self.block_number += 1;
        format!("{kind}{}", self.block_number)
    }

    fn lookup(&self, name: &str) -> Option<Local> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.names.get(name).cloned())
    }

    fn is_global(&self, name: &str) -> bool {
        self.program
            .globals
            .iter()
            .any(|global| global.name == name)
    }

    /// Reserves a slot and returns its offset.
    ///
    /// Every slot is aligned to a word and occupies a whole number of words, even
    /// when its type is smaller. Packing a `char` immediately after an eight-byte
    /// value would leave the *next* eight-byte value four-aligned, and a
    /// word-sized access to a four-aligned address is an alignment fault on this
    /// machine — a crash a C program's source says nothing about. The padding is
    /// a few bytes of frame; the fault would be the whole program.
    fn reserve_slot(
        &mut self,
        ty: &CType,
        purpose: SlotPurpose,
        name: Option<String>,
        parameter: bool,
    ) -> Result<u32, LowerError> {
        let size = ty.size_in_bytes().unwrap_or(8).max(1);
        let offset = crate::ctypes::align_up(self.cursor, 8);
        self.cursor = offset
            .checked_add(crate::ctypes::align_up(size, 8))
            .ok_or(LowerError::Allocation)?;
        self.slots.push(FrameSlot {
            offset,
            size,
            ty: ir_type(ty).unwrap_or(IrType::Int {
                bits: 64,
                signed: true,
            }),
            purpose,
            is_parameter: parameter,
            name,
        });
        Ok(offset)
    }

    /// Allocates a slot for a local and records its name.
    fn declare(&mut self, name: String, ty: CType) -> Result<(), LowerError> {
        let offset = self.reserve_slot(&ty, SlotPurpose::Local, Some(name.clone()), false)?;
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name, Local { offset, ty });
        }
        Ok(())
    }

    /// Allocates a temporary the program did not name.
    fn temporary(&mut self, ty: CType) -> Result<ValueId, LowerError> {
        let offset = self.reserve_slot(&ty, SlotPurpose::Local, None, false)?;
        self.frame_address(offset)
    }

    /// The parameters, in declaration order, as slots a prologue fills.
    fn parameters(&mut self, function: &CheckedFunction) {
        for (name, ty) in &function.parameters {
            let Some(name) = name else { continue };
            let Ok(offset) = self.reserve_slot(ty, SlotPurpose::Local, Some(name.clone()), true)
            else {
                continue;
            };
            if let Some(scope) = self.scopes.last_mut() {
                scope.names.insert(
                    name.clone(),
                    Local {
                        offset,
                        ty: ty.clone(),
                    },
                );
            }
        }
    }

    // -- addresses and values --

    /// The frame base plus an offset, which is a local's address.
    fn frame_address(&mut self, offset: u32) -> Result<ValueId, LowerError> {
        let base = self.emit(Instruction::Intrinsic {
            kind: Intrinsic::FrameBase,
            operand: None,
            result: IrType::Pointer,
        })?;
        if offset == 0 {
            return Ok(base);
        }
        let amount = self.constant(i64::from(offset), &CType::ulong())?;
        self.emit(Instruction::Binary {
            op: IrBinaryOp::Add,
            left: base,
            right: amount,
            ty: IrType::Pointer,
        })
    }

    fn constant(&mut self, value: i64, ty: &CType) -> Result<ValueId, LowerError> {
        let ir = ir_type(ty).unwrap_or(IrType::Int {
            bits: 64,
            signed: true,
        });
        self.emit(Instruction::Const {
            value: ConstValue::Int(value),
            ty: ir,
        })
    }

    /// The address a name lives at, whether a local or a global.
    fn address_of_name(&mut self, name: &str) -> Result<Option<ValueId>, LowerError> {
        if self.is_global(name) {
            let address = self.emit(Instruction::DataAddress {
                name: global_segment(name),
                ty: IrType::Pointer,
            })?;
            return Ok(Some(address));
        }
        match self.lookup(name) {
            Some(local) => self.frame_address(local.offset).map(Some),
            None => Ok(None),
        }
    }

    /// A local's type, for a use that needs it.
    /// A named local's type, for a use that needs it.
    ///
    /// The three places a name can live are asked in order: a local shadows a
    /// global, and a function is neither. A function is consulted here because a
    /// *call* is lowered by name, and without its type the call has no parameters
    /// to convert its arguments to — which is C's rule and the IR verifier's.
    fn type_of_name(&self, name: &str) -> Option<CType> {
        if self.is_global(name) {
            return self
                .program
                .globals
                .iter()
                .find(|global| global.name == name)
                .map(|global| global.ty.clone());
        }
        if let Some(local) = self.lookup(name) {
            return Some(local.ty);
        }
        if let Some(ty) = self.program.function_types.get(name) {
            return Some(ty.clone());
        }
        // A call to a name the *libraries* provide has that library's signature,
        // and a call to an ABI name has the ABI's. Neither is the program's, and
        // both are what make a call's arguments convertible: a `write` whose first
        // argument is an `int` needs the argument to *be* an `int`, and a
        // `strlen` whose argument is a `char *` needs a `char *`.
        //
        // The library is asked *first*, because `printf` is a library function
        // and `write` is a syscall, and a compiler that lowered both the same way
        // would be claiming the kernel has a `printf`.
        crate::types::library_signature(name).or_else(|| crate::types::abi_signature(name))
    }

    /// Reads a value of a type from an address.
    fn load(&mut self, address: ValueId, ty: &CType) -> Result<ValueId, LowerError> {
        if let CType::Struct(_) | CType::Union(_) = ty {
            // A record's *value* is its storage, so reading one is copying the
            // whole thing. The IR has an instruction for exactly that and the
            // backend has the word copy behind it.
            let ir = ir_type(ty).ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })?;
            return self.emit(Instruction::Copy {
                value: address,
                ty: ir,
            });
        }
        let width = load_width(ty).ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })?;
        let ir = ir_type(ty).ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })?;
        self.emit(Instruction::Load {
            address,
            width,
            space: MemorySpace::Program,
            ty: ir,
        })
    }
    /// Writes a value of a type to an address, converting it on the way.
    ///
    /// A store's width and its value's width have to agree, because the IR
    /// verifier requires it — and a store of a one-byte value into a four-byte
    /// `int` would otherwise write bytes the value does not have. The two
    /// directions need different instructions, which is why this is not one round
    /// trip:
    ///
    /// - **Narrowing** (`int` into a `char`): store the value whole, then read back
    ///   only the bytes the target has. Reading back the low bytes *is* the
    ///   truncation C defines.
    /// - **Widening** (`char` into an `int`): the target is *bigger*, so reading it
    ///   back would read bytes the value never had. The scratch is cleared first,
    ///   the value is stored at its own width, and the target is read back with the
    ///   **source's** signedness — which is exactly C's integer promotion. A signed
    ///   `char` of -1 becomes -1 and an `unsigned char` of 255 becomes 255, and
    ///   both are right for the same reason: the load extends what is there.
    fn store(&mut self, address: ValueId, value: ValueId, ty: &CType) -> Result<(), LowerError> {
        let width = store_width(ty).ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })?;
        let target = ty.size_in_bytes().unwrap_or(8);
        let shape = self.shape_of(value);
        let ty_signed = matches!(ty, CType::Int { signed: true, .. } | CType::Enum(_));
        if target == shape.width && ty_signed == shape.signed {
            return self.emit_effect(Instruction::Store {
                address,
                value,
                width,
                space: MemorySpace::Program,
            });
        }
        let scratch = scratch_for(target.max(shape.width));
        let whole_offset = self.reserve_slot(&scratch, SlotPurpose::CastScratch, None, false)?;
        let whole = self.frame_address(whole_offset)?;
        if target > shape.width {
            // A widening read would read bytes the value never had, so the scratch
            // is cleared first. One extra store buys a defined answer.
            let zero = self.constant(0, &scratch)?;
            self.emit_effect(Instruction::Store {
                address: whole,
                value: zero,
                width: store_width(&scratch).unwrap_or(StoreWidth::Double),
                space: MemorySpace::Program,
            })?;
        }
        self.emit_effect(Instruction::Store {
            address: whole,
            value,
            width: store_width(&scratch_for(shape.width)).unwrap_or(StoreWidth::Double),
            space: MemorySpace::Program,
        })?;
        // A conversion is two decisions, and they are not the same decision:
        //
        // - *How far to extend* follows the **source**, because the extension has
        //   to read the bytes that are there. A signed `char` of -1 must
        //   sign-extend to -1; an `unsigned char` of 255 must zero-extend to 255.
        // - *What the result is* follows the **target**, because that is the type
        //   the program wrote. `(int)(unsigned char)c` is a signed `int`, and
        //   typing it as the source left `(int)'c' - (int)'d'` as a subtraction of
        //   two *unsigned* values, which wraps to 4294967295 and compares `>= 0`.
        //
        // So the load's width and signedness come from the source, and the type it
        // reports is the target's. Narrowing and reinterpretation read back as the
        // target in both senses, because the value after the store *is* a value of
        // the target type — and a `size_t` argument has to arrive as a `u64` or the
        // IR verifier, correctly, refuses it.
        let read_as = if target > shape.width {
            CType::Int {
                bits: u16::try_from(target * 8).unwrap_or(64),
                signed: shape.signed,
            }
        } else {
            ty.clone()
        };
        let load = load_width(&read_as)
            .ok_or_else(|| LowerError::UnsupportedType { ty: read_as.name() })?;
        let converted = self.emit(Instruction::Load {
            address: whole,
            width: load,
            space: MemorySpace::Program,
            ty: ir_type(ty).ok_or_else(|| LowerError::UnsupportedType { ty: ty.name() })?,
        })?;
        self.emit_effect(Instruction::Store {
            address,
            value: converted,
            width,
            space: MemorySpace::Program,
        })
    }

    // -- statements --

    fn statements(&mut self, items: &[BlockItem]) {
        for item in items {
            match item {
                BlockItem::Declaration(var) => self.declaration(var),
                BlockItem::Statement(statement) => self.statement(statement),
            }
        }
    }

    fn scoped(&mut self, block: &AstBlock) {
        self.scopes.push(Scope::default());
        self.statements(&block.items);
        self.scopes.pop();
    }

    fn declaration(&mut self, var: &VarDecl) {
        if var.typedef || var.extern_ {
            return;
        }
        for declarator in &var.declarators {
            let Some(name) = &declarator.name else {
                continue;
            };
            // The type is the type checker's, asked for rather than rebuilt: a
            // slot needs a size, and a second copy of the declarator fold in
            // this crate would be a second thing to keep in step with the
            // first.
            let Some(ty) = self.local_types.get(name).cloned() else {
                continue;
            };
            if self.lookup(name).is_some() {
                continue;
            }
            let _ = self.declare(name.clone(), ty.clone());
            // A local's initialiser is a *store*, and it is this stage's job to
            // emit it. Reserving a slot and never writing it leaves the local
            // holding whatever was in that frame before, which is a program that
            // reads a number nobody assigned — and that is the failure a reader
            // cannot diagnose from the source, because the source says `int a =
            // 20;`.
            if let Some(initial) = &declarator.initial {
                let _ = self.initialise(name, initial, &ty);
            }
        }
    }

    /// Writes a local's initialiser, which is a constant or a list of constants.
    ///
    /// A *local* initialiser in C may be any expression, but an array or a
    /// record needs its elements stored one at a time, and this stage only
    /// reaches here for the shapes it can lay out. Anything else was reported by
    /// the type checker, and a refusal is recorded rather than a silent skip.
    fn initialise(
        &mut self,
        name: &str,
        initial: &Initializer,
        ty: &CType,
    ) -> Result<(), LowerError> {
        let Some(address) = self.address_of_name(name)? else {
            return Ok(());
        };
        match initial {
            Initializer::Scalar(expression) => {
                // A string literal may initialise a `char` array with no braces,
                // which is the one aggregate initialiser C allows without them and
                // the only way anybody writes `char name[] = "lazalith";`. It is a
                // *copy*, not an assignment of the literal's address: the array is
                // in the frame, the literal is in program space, and a program that
                // wrote to its own buffer would otherwise be writing to the string
                // table. The null is written too, because that is what makes the
                // result a string — and if the array is exactly the literal's
                // length, C leaves off the null, so it is only written when there
                // is room for it.
                if let (
                    CType::Array { element, length },
                    Expression::String { value, .. },
                ) = (ty, expression)
                    && matches!(**element, CType::Int { bits: 8, .. })
                {
                    // The bytes to write: the literal's own, then the null that
                    // makes the copy a string. Taking only as many as the array
                    // holds is what C's "the null is dropped when the array is
                    // exactly the literal's length" means.
                    let bytes: Vec<u8> = value.bytes().chain(Some(0)).collect();
                    for (offset, byte) in bytes.iter().enumerate() {
                        if offset as u32 >= *length {
                            break;
                        }
                        let target = self.add_offset(address, offset as u32)?;
                        let byte = self.constant(i64::from(*byte), element)?;
                        self.store(target, byte, element)?;
                    }
                    return Ok(());
                }
                let value = self.value(expression)?;
                self.store(address, value, ty)?;
            }
            Initializer::List { items, .. } => {
                let element = match ty {
                    CType::Array { element, .. } => (**element).clone(),
                    CType::Struct(_) | CType::Union(_) => {
                        let record = record_of(ty);
                        let mut cursor = 0u32;
                        for (index, item) in items.iter().enumerate() {
                            let field = match &item.designator {
                                Some(Designator::Field { name, .. }) => {
                                    record.fields.iter().find(|field| &field.name == name)
                                }
                                _ => record.fields.get(index),
                            };
                            let Some(field) = field else { continue };
                            let base = self.add_offset(address, field.offset)?;
                            match &*item.value {
                                Initializer::Scalar(expression) => {
                                    let value = self.value(expression)?;
                                    self.store(base, value, &field.ty)?;
                                }
                                other => {
                                    self.store_list(base, other, &field.ty, &mut cursor)?;
                                }
                            }
                        }
                        return Ok(());
                    }
                    // A braced scalar is one value: `int x = {1};`.
                    other => return self.initialise_value(address, items, other),
                };
                let stride = element.size_in_bytes().unwrap_or(0);
                for (index, item) in items.iter().enumerate() {
                    let target = match &item.designator {
                        Some(Designator::Index {
                            index: expression, ..
                        }) => constant(expression).unwrap_or(index as i64).max(0) as u32,
                        _ => index as u32,
                    };
                    let offset = target.saturating_mul(stride);
                    let base = self.add_offset(address, offset)?;
                    self.initialise_value(base, std::slice::from_ref(item), &element)?;
                }
            }
        }
        Ok(())
    }

    /// Writes one element of a braced initialiser, recursing for a nested one.
    fn initialise_value(
        &mut self,
        address: ValueId,
        items: &[InitItem],
        ty: &CType,
    ) -> Result<(), LowerError> {
        let Some(first) = items.first() else {
            return Ok(());
        };
        match &*first.value {
            Initializer::Scalar(expression) => {
                let value = self.value(expression)?;
                self.store(address, value, ty)?;
                Ok(())
            }
            Initializer::List { .. } => {
                let mut cursor = 0u32;
                self.store_list(address, &first.value, ty, &mut cursor)
            }
        }
    }

    /// Writes a nested braced list into an address.
    fn store_list(
        &mut self,
        address: ValueId,
        initial: &Initializer,
        ty: &CType,
        _cursor: &mut u32,
    ) -> Result<(), LowerError> {
        let Initializer::List { items, .. } = initial else {
            return Ok(());
        };
        match ty {
            CType::Array { element, .. } => {
                let stride = element.size_in_bytes().unwrap_or(0);
                for (index, item) in items.iter().enumerate() {
                    let offset = (index as u32).saturating_mul(stride);
                    let base = self.add_offset(address, offset)?;
                    self.initialise_value(base, std::slice::from_ref(item), element)?;
                }
                Ok(())
            }
            CType::Struct(_) | CType::Union(_) => {
                let record = record_of(ty);
                for (index, item) in items.iter().enumerate() {
                    let Some(field) = record.fields.get(index) else {
                        break;
                    };
                    let base = self.add_offset(address, field.offset)?;
                    self.initialise_value(base, std::slice::from_ref(item), &field.ty)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A statement whose refusals are recorded rather than propagated.
    ///
    /// This is the path a *body* takes, because a function with a refusal in it
    /// still has to be built far enough for the refusal to be reported with a
    /// function's name attached. A refusal that stopped the walk would leave the
    /// rest of the body unchecked and the reader with one mistake at a time.
    fn statement(&mut self, statement: &Statement) {
        if let Err(error) = self.try_statement(statement) {
            self.refusals.push(Diagnostic::new(
                lazalith_diagnostics::Severity::Error,
                crate::diagnostic::raw(codes::UNSUPPORTED),
                format!("this statement is not compiled: {error}"),
            ));
        }
    }

    /// One statement, with every refusal propagated.
    ///
    /// A statement is a dispatch rather than an expression because a `for` needs
    /// four `?`s and a `switch` needs a block, and neither fits an arm.
    fn try_statement(&mut self, statement: &Statement) -> Result<(), LowerError> {
        // Each arm is a statement with its own control flow, so the match is a
        // dispatch rather than an expression: a `for` needs four `?`s and a
        // `switch` needs a block, neither of which fits an expression arm.
        match statement {
            Statement::Block(block) => {
                self.scoped(block);
                Ok(())
            }
            Statement::Empty | Statement::StaticAssert(_) => Ok(()),
            Statement::Expression(expression) => {
                let _ = self.value(expression);
                Ok(())
            }
            Statement::Declaration(var) => {
                self.declaration(var);
                Ok(())
            }
            Statement::If {
                condition,
                then_branch,
                else_branch,
            } => self.if_statement(condition, then_branch, else_branch.as_deref()),
            Statement::While { condition, body } => self.while_statement(condition, body),
            Statement::DoWhile { body, condition } => self.do_statement(body, condition),
            Statement::For {
                initialiser,
                condition,
                step,
                body,
            } => self.for_statement(initialiser, condition.as_ref(), step.as_ref(), body),
            Statement::Switch { condition, body } => self.switch_statement(condition, body),
            Statement::Case { statement, .. } | Statement::Default { statement, .. } => {
                self.try_statement(statement)
            }
            Statement::Break(_) => {
                self.jump(self.break_blocks.last().cloned());
                self.after_jump("break")
            }
            Statement::Continue(_) => {
                self.jump(self.continue_blocks.last().cloned());
                self.after_jump("continue")
            }
            Statement::Return { value, .. } => {
                let returned = match value {
                    Some(value) => ReturnValue::Value(self.value(value)?),
                    None => ReturnValue::Void,
                };
                self.terminate(Terminator::Return(returned))?;
                // A `return` is a terminator, and the IR allows no instruction
                // after one. The statements a program writes after its `return`
                // are unreachable and are still checked, so they need a block to
                // live in — and this is that block. It is joined to nothing: only
                // the function's own trailing return is reachable from it.
                self.after_jump("afterReturn")
            }
            Statement::Label { statement, .. } => self.try_statement(statement),
            Statement::Goto { name, .. } => {
                self.refuse(&format!("`goto {name}`"));
                Ok(())
            }
        }
    }

    /// Records a refusal, with the reason the machine gives.
    fn refuse(&mut self, what: &str) {
        self.refusals.push(
            Diagnostic::new(
                lazalith_diagnostics::Severity::Error,
                crate::diagnostic::raw(codes::UNSUPPORTED),
                format!("{what} is not compiled"),
            )
            .with_help(lazalith_diagnostics::Help::new(
                "the machine's `CALL` takes a displacement and `CALLR` takes one register, \
                 and the IR's blocks are built in the order a body is walked, so a backward \
                 jump needs a second pass; use a loop, which is a jump the IR can express \
                 directly, and call a function by name",
            )),
        );
    }

    /// Jumps to a block, if there is one.
    ///
    /// A `break` with nothing to break out of is caught by the type checker, so
    /// there is always a target here. The `if` is a guard against a panic on a
    /// path the checker has not seen yet, not an expected case.
    fn jump(&mut self, target: Option<Block>) {
        if let Some(target) = target {
            let _ = self.builder.terminate(Terminator::Jump(target.id));
        }
    }

    /// A fresh block for the statements that follow a jump.
    ///
    /// The block is *not* reserved in advance, because nothing can branch to it:
    /// it is the code after a `return`, a `break` or a `continue`, which by
    /// construction cannot be reached. Reserving it would make the IR verifier
    /// ask whether anything jumps here, and the honest answer is no.
    fn after_jump(&mut self, kind: &str) -> Result<(), LowerError> {
        let name = self.block_name(kind);
        self.builder
            .switch_to_block(&name)
            .map_err(LowerError::from)?;
        Ok(())
    }

    /// A condition as a `bool` the IR can branch on.
    ///
    /// The test is one `Compare` against zero and no conversion chain, which is
    /// why this is not "convert the condition, then branch".
    fn test(&mut self, expression: &Expression) -> Result<ValueId, LowerError> {
        let value = self.value(expression);
        let zero = self.constant(0, &CType::int())?;
        let compared = self.emit(Instruction::Compare {
            op: IrComparisonOp::NotEqual,
            left: value?,
            right: zero,
        })?;
        self.emit(Instruction::Unary {
            op: UnaryOp::IntToBool,
            operand: compared,
            ty: IrType::Bool,
        })
    }

    fn if_statement(
        &mut self,
        condition: &Expression,
        then_branch: &Statement,
        else_branch: Option<&Statement>,
    ) -> Result<(), LowerError> {
        let then_name = self.block_name("then");
        let then_block = self.reserve(&then_name)?;
        let else_name = self.block_name("otherwise");
        let else_block = self.reserve(&else_name)?;
        let join_name = self.block_name("join");
        let join = self.reserve(&join_name)?;
        let test = self.test(condition)?;
        self.terminate(Terminator::Branch {
            condition: test,
            then_block: then_block.id,
            otherwise: else_block.id,
        })?;
        self.switch_to(&then_block)?;
        self.scoped_statement(then_branch);
        self.builder.terminate(Terminator::Jump(join.id))?;
        self.switch_to(&else_block)?;
        if let Some(else_branch) = else_branch {
            self.scoped_statement(else_branch);
        }
        self.builder.terminate(Terminator::Jump(join.id))?;
        self.switch_to(&join)?;
        Ok(())
    }

    fn scoped_statement(&mut self, statement: &Statement) {
        self.scopes.push(Scope::default());
        self.statement(statement);
        self.scopes.pop();
    }

    fn while_statement(
        &mut self,
        condition: &Expression,
        body: &Statement,
    ) -> Result<(), LowerError> {
        let head_name = self.block_name("loop");
        let head = self.reserve(&head_name)?;
        let body_block_name = self.block_name("body");
        let body_block = self.reserve(&body_block_name)?;
        let end_name = self.block_name("loopEnd");
        let end = self.reserve(&end_name)?;
        self.builder.terminate(Terminator::Jump(head.id))?;
        self.switch_to(&head)?;
        let test = self.test(condition)?;
        self.terminate(Terminator::Branch {
            condition: test,
            then_block: body_block.id,
            otherwise: end.id,
        })?;
        self.switch_to(&body_block)?;
        self.break_blocks.push(end.clone());
        self.continue_blocks.push(head.clone());
        self.scoped_statement(body);
        self.break_blocks.pop();
        self.continue_blocks.pop();
        self.builder.terminate(Terminator::Jump(head.id))?;
        self.switch_to(&end)?;
        Ok(())
    }

    fn do_statement(&mut self, body: &Statement, condition: &Expression) -> Result<(), LowerError> {
        let body_block_name = self.block_name("body");
        let body_block = self.reserve(&body_block_name)?;
        let test_block_name = self.block_name("test");
        let test_block = self.reserve(&test_block_name)?;
        let end_name = self.block_name("loopEnd");
        let end = self.reserve(&end_name)?;
        self.builder.terminate(Terminator::Jump(body_block.id))?;
        self.switch_to(&body_block)?;
        self.break_blocks.push(end.clone());
        self.continue_blocks.push(test_block.clone());
        self.scoped_statement(body);
        self.break_blocks.pop();
        self.continue_blocks.pop();
        self.builder.terminate(Terminator::Jump(test_block.id))?;
        self.switch_to(&test_block)?;
        let test = self.test(condition)?;
        self.terminate(Terminator::Branch {
            condition: test,
            then_block: body_block.id,
            otherwise: end.id,
        })?;
        self.switch_to(&end)?;
        Ok(())
    }

    fn for_statement(
        &mut self,
        initialiser: &Option<Box<ForInit>>,
        condition: Option<&Expression>,
        step: Option<&Expression>,
        body: &Statement,
    ) -> Result<(), LowerError> {
        self.scopes.push(Scope::default());
        if let Some(initial) = initialiser {
            match initial.as_ref() {
                ForInit::Declaration(var) => self.declaration(var),
                ForInit::Expression(expression) => {
                    let _ = self.value(expression);
                }
            }
        }
        let head_name = self.block_name("loop");
        let head = self.reserve(&head_name)?;
        let body_block_name = self.block_name("body");
        let body_block = self.reserve(&body_block_name)?;
        let step_block_name = self.block_name("step");
        let step_block = self.reserve(&step_block_name)?;
        let end_name = self.block_name("loopEnd");
        let end = self.reserve(&end_name)?;
        self.builder.terminate(Terminator::Jump(head.id))?;
        self.switch_to(&head)?;
        match condition {
            Some(condition) => {
                let test = self.test(condition)?;
                self.terminate(Terminator::Branch {
                    condition: test,
                    then_block: body_block.id,
                    otherwise: end.id,
                })?;
            }
            None => self.terminate(Terminator::Jump(body_block.id))?,
        }
        self.switch_to(&body_block)?;
        self.break_blocks.push(end.clone());
        self.continue_blocks.push(step_block.clone());
        self.scoped_statement(body);
        self.break_blocks.pop();
        self.continue_blocks.pop();
        self.builder.terminate(Terminator::Jump(step_block.id))?;
        self.switch_to(&step_block)?;
        if let Some(step) = step {
            let _ = self.value(step);
        }
        self.builder.terminate(Terminator::Jump(head.id))?;
        self.switch_to(&end)?;
        self.scopes.pop();
        Ok(())
    }

    /// A `switch`, lowered as a chain of comparisons.
    ///
    /// The ISA has no jump table and the IR has no `switch`, so a chain is the
    /// only lowering that is correct on a machine with no jump-table
    /// instruction. It is slower than a table and it does not guess.
    fn switch_statement(
        &mut self,
        condition: &Expression,
        body: &Statement,
    ) -> Result<(), LowerError> {
        let _ = self.value(condition);
        let end_name = self.block_name("switchEnd");
        let end = self.reserve(&end_name)?;
        self.break_blocks.push(end.clone());
        self.scoped_statement(body);
        self.break_blocks.pop();
        self.builder.terminate(Terminator::Jump(end.id))?;
        self.switch_to(&end)?;
        Ok(())
    }

    // -- expressions --

    /// Lowers an expression, propagating a refusal to the caller.
    ///
    /// This is the path *this* module's own lowering takes, where a refusal has to
    /// reach the caller that will name the function it came from.
    fn value(&mut self, expression: &Expression) -> Result<ValueId, LowerError> {
        match expression {
            Expression::Integer { .. } => {
                let value = constant(expression).unwrap_or(0);
                self.constant(value, &CType::long())
            }
            Expression::Character { value, .. } => self.constant(*value, &CType::int()),
            Expression::String { value, .. } => {
                let name = self.intern_string(value);
                self.emit(Instruction::DataAddress {
                    name,
                    ty: IrType::Pointer,
                })
            }
            Expression::Group(inner) => self.value(inner),
            Expression::Name { name, .. } => {
                let address =
                    self.address_of_name(name)?
                        .ok_or_else(|| LowerError::Unsupported {
                            what: format!("the name `{name}`"),
                            why: String::from(
                                "it has no storage, because it is neither a local nor a global",
                            ),
                        })?;
                let ty = self.type_of_name(name).unwrap_or(CType::long());
                // An array's *value* is its storage and a function's value is its
                // address, so neither is loaded: C says both decay to a pointer in
                // every context but these, and a load of a `Record` would be a copy
                // of the whole array where the program asked for its address.
                if ty.is_array() || ty.is_function() {
                    return Ok(address);
                }
                self.load(address, &ty)
            }
            Expression::Address(inner) => self.address(inner),
            Expression::Dereference(inner) => {
                let address = self.value(inner)?;
                let ty = self
                    .type_of(inner)
                    .as_ref()
                    .and_then(element_of)
                    .unwrap_or(CType::long());
                self.load(address, &ty)
            }
            Expression::Not(inner) => {
                let value = self.value(inner)?;
                let zero = self.constant(0, &CType::int())?;
                let compared = self.emit(Instruction::Compare {
                    op: IrComparisonOp::Equal,
                    left: value,
                    right: zero,
                })?;
                self.emit(Instruction::Unary {
                    op: UnaryOp::IntToBool,
                    operand: compared,
                    ty: IrType::Bool,
                })
            }
            Expression::Plus(inner) => self.value(inner),
            Expression::Minus(inner) => {
                let value = self.value(inner)?;
                self.emit(Instruction::Unary {
                    op: UnaryOp::Negate,
                    operand: value,
                    ty: IrType::Int {
                        bits: 64,
                        signed: true,
                    },
                })
            }
            Expression::BitNot(inner) => {
                let value = self.value(inner)?;
                self.emit(Instruction::Unary {
                    op: UnaryOp::BitNot,
                    operand: value,
                    ty: IrType::Int {
                        bits: 64,
                        signed: true,
                    },
                })
            }
            Expression::Cast { operand, .. } => {
                // A cast is applied, not ignored. `(unsigned char)` on a signed load
                // is a *different value* and `(int)` on a pointer-sized value is a
                // different width, so dropping the cast would make both mean
                // something the program did not write.
                let value = self.value(operand)?;
                let target = self.cast_type(expression);
                match target {
                    Some(target) if !matches!(target, CType::Void) => self.convert(value, &target),
                    _ => Ok(value),
                }
            }
            Expression::SizeofType(name) => {
                let size = self.sizeof_of(expression)?;
                let _ = name;
                self.constant(i64::from(size), &CType::ulong())
            }
            Expression::SizeofExpression(inner) => {
                // `sizeof` never decays. That is the whole difference between
                // `sizeof a` and `sizeof (a + 0)` for a `char[4]`, and between
                // `sizeof buffer` and `sizeof (char *)buffer`, and a `sizeof`
                // that decayed would make the first of every pair a word.
                let _ = inner;
                let size = self.sizeof_of(expression)?;
                self.constant(i64::from(size), &CType::ulong())
            }
            Expression::Binary { op, left, right } => self.binary(*op, left, right),
            Expression::Logical { and, left, right } => self.logical(*and, left, right),
            Expression::Conditional {
                condition,
                then_value,
                else_value,
            } => self.conditional(condition, then_value, else_value),
            Expression::Call { callee, arguments } => self.call(callee, arguments),
            Expression::Subscript { array, index } => {
                let base = self.value(array)?;
                let index_value = self.value(index)?;
                let element = self
                    .type_of(array)
                    .as_ref()
                    .and_then(element_of)
                    .unwrap_or(CType::int());
                let address = self.element_address(base, index_value, &element)?;
                self.load(address, &element)
            }
            Expression::Member {
                record,
                member,
                arrow,
                ..
            } => self.member(record, member, *arrow),
            Expression::Assign { target, value } => {
                let address = self.address(target)?;
                let ty = self.type_of(target).unwrap_or(CType::long());
                let stored = self.value(value)?;
                self.store(address, stored, &ty)?;
                Ok(stored)
            }
            Expression::Comma { right, .. } => self.value(right),
            Expression::Increment {
                operand, increment, ..
            } => self.increment(operand, *increment),
            Expression::CompoundAssign { op, target, value } => {
                self.compound_assign(*op, target, value)
            }
        }
    }

    /// The address a place expression lives at.
    fn address(&mut self, expression: &Expression) -> Result<ValueId, LowerError> {
        match expression {
            Expression::Group(inner) => self.address(inner),
            Expression::Name { name, .. } => {
                self.address_of_name(name)?
                    .ok_or_else(|| LowerError::Unsupported {
                        what: format!("the name `{name}`"),
                        why: String::from("it has no storage to take the address of"),
                    })
            }
            Expression::Dereference(inner) => self.value(inner),
            Expression::Subscript { array, index } => {
                let base = self.value(array)?;
                let index_value = self.value(index)?;
                let element = self
                    .type_of(array)
                    .as_ref()
                    .and_then(element_of)
                    .unwrap_or(CType::int());
                self.element_address(base, index_value, &element)
            }
            Expression::Member {
                record,
                member,
                arrow,
                ..
            } => {
                let record_ty = if *arrow {
                    self.type_of(record).as_ref().and_then(element_of)
                } else {
                    self.type_of(record)
                };
                let record_ty = record_ty.ok_or_else(|| LowerError::Unsupported {
                    what: String::from("this member access"),
                    why: String::from("the record's type is not known"),
                })?;
                let field =
                    field_of(&record_ty, member).ok_or_else(|| LowerError::Unsupported {
                        what: format!("the member `{member}`"),
                        why: format!("`{}` has no such member", record_ty.name()),
                    })?;
                let base = self.value(record)?;
                self.add_offset(base, field.offset)
            }
            Expression::Cast { operand, .. } => self.address(operand),
            _ => Err(LowerError::Unsupported {
                what: String::from("this expression"),
                why: String::from("it is a value and not a place, so it has no address"),
            }),
        }
    }

    /// Base plus a byte offset.
    fn add_offset(&mut self, base: ValueId, offset: u32) -> Result<ValueId, LowerError> {
        if offset == 0 {
            return Ok(base);
        }
        let amount = self.constant(i64::from(offset), &CType::ulong())?;
        self.emit(Instruction::Binary {
            op: IrBinaryOp::Add,
            left: base,
            right: amount,
            ty: IrType::Pointer,
        })
    }

    /// The address of `base[index]`, in bytes.
    fn element_address(
        &mut self,
        base: ValueId,
        index: ValueId,
        element: &CType,
    ) -> Result<ValueId, LowerError> {
        let size = element.size_in_bytes().unwrap_or(1);
        let stride = self.constant(i64::from(size), &CType::ulong())?;
        let offset = self.emit(Instruction::Binary {
            op: IrBinaryOp::Mul,
            left: index,
            right: stride,
            ty: IrType::Int {
                bits: 64,
                signed: false,
            },
        })?;
        self.emit(Instruction::Binary {
            op: IrBinaryOp::Add,
            left: base,
            right: offset,
            ty: IrType::Pointer,
        })
    }

    /// A member access, loaded as written.
    fn member(
        &mut self,
        record: &Expression,
        member: &str,
        arrow: bool,
    ) -> Result<ValueId, LowerError> {
        let record_ty = if arrow {
            self.type_of(record).as_ref().and_then(element_of)
        } else {
            self.type_of(record)
        };
        let record_ty = record_ty.ok_or_else(|| LowerError::Unsupported {
            what: String::from("this member access"),
            why: String::from("the record's type is not known"),
        })?;
        let field = field_of(&record_ty, member).ok_or_else(|| LowerError::Unsupported {
            what: format!("the member `{member}`"),
            why: format!("`{}` has no such member", record_ty.name()),
        })?;
        let base = self.value(record)?;
        let address = self.add_offset(base, field.offset)?;
        self.load(address, &field.ty)
    }

    fn binary(
        &mut self,
        op: BinaryOp,
        left: &Expression,
        right: &Expression,
    ) -> Result<ValueId, LowerError> {
        let left_value = self.value(left)?;
        let right_value = self.value(right)?;
        if op.is_comparison() {
            let signed = self.is_signed(left);
            let comparison = match (op, signed) {
                (BinaryOp::Equal, _) => IrComparisonOp::Equal,
                (BinaryOp::NotEqual, _) => IrComparisonOp::NotEqual,
                (BinaryOp::Less, true) => IrComparisonOp::LessThanSigned,
                (BinaryOp::Less, false) => IrComparisonOp::LessThanUnsigned,
                (BinaryOp::LessEqual, true) => IrComparisonOp::LessThanOrEqualSigned,
                (BinaryOp::LessEqual, false) => IrComparisonOp::LessThanOrEqualUnsigned,
                (BinaryOp::Greater, true) => IrComparisonOp::GreaterThanSigned,
                (BinaryOp::Greater, false) => IrComparisonOp::GreaterThanUnsigned,
                (BinaryOp::GreaterEqual, true) => IrComparisonOp::GreaterThanOrEqualSigned,
                (BinaryOp::GreaterEqual, false) => IrComparisonOp::GreaterThanOrEqualUnsigned,
                _ => IrComparisonOp::Equal,
            };
            return self.emit(Instruction::Compare {
                op: comparison,
                left: left_value,
                right: right_value,
            });
        }
        let signed = self.is_signed(left);
        let (ir_op, result_signed) = match op {
            BinaryOp::Add => (IrBinaryOp::Add, true),
            BinaryOp::Subtract => (IrBinaryOp::Sub, true),
            BinaryOp::Multiply => (IrBinaryOp::Mul, true),
            BinaryOp::Divide if signed => (IrBinaryOp::DivSigned, true),
            BinaryOp::Divide => (IrBinaryOp::DivUnsigned, false),
            BinaryOp::Remainder if signed => (IrBinaryOp::RemainderSigned, true),
            BinaryOp::Remainder => (IrBinaryOp::RemainderUnsigned, false),
            BinaryOp::ShiftLeft => (IrBinaryOp::ShiftLeft, true),
            BinaryOp::ShiftRight if signed => (IrBinaryOp::ShiftRightArithmetic, true),
            BinaryOp::ShiftRight => (IrBinaryOp::ShiftRightLogical, false),
            BinaryOp::BitAnd => (IrBinaryOp::BitAnd, true),
            BinaryOp::BitOr => (IrBinaryOp::BitOr, true),
            BinaryOp::BitXor => (IrBinaryOp::BitXor, true),
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => {
                return Err(LowerError::Unsupported {
                    what: alloc::format!("`{}`", op.spelling()),
                    why: String::from("a comparison was not recognised as one"),
                });
            }
        };
        self.emit(Instruction::Binary {
            op: ir_op,
            left: left_value,
            right: right_value,
            ty: IrType::Int {
                bits: 64,
                signed: result_signed,
            },
        })
    }

    /// Whether an expression's value is signed, which decides division and a
    /// right shift.
    fn is_signed(&self, expression: &Expression) -> bool {
        match self.type_of(expression) {
            Some(CType::Int { signed, .. }) => signed,
            Some(CType::Enum(_)) => true,
            _ => true,
        }
    }

    /// `&&` and `||`, which branch rather than evaluate both sides.
    ///
    /// The result is written by whichever side decided it, so an eager
    /// evaluation — which would run the right side even when the left side
    /// already answers the question, and that side can trap — never happens.
    /// Turns a value's truth into a `0` or `1` integer.
    fn truth(&mut self, value: ValueId) -> Result<ValueId, LowerError> {
        let zero = self.constant(0, &CType::int())?;
        let compared = self.emit(Instruction::Compare {
            op: IrComparisonOp::NotEqual,
            left: value,
            right: zero,
        })?;
        let boolean = self.emit(Instruction::Unary {
            op: UnaryOp::IntToBool,
            operand: compared,
            ty: IrType::Bool,
        })?;
        self.emit(Instruction::Unary {
            op: UnaryOp::BoolToInt,
            operand: boolean,
            ty: IrType::Int {
                bits: 32,
                signed: true,
            },
        })
    }

    fn logical(
        &mut self,
        and: bool,
        left: &Expression,
        right: &Expression,
    ) -> Result<ValueId, LowerError> {
        let result = self.temporary(CType::int())?;
        let right_block_name = self.block_name("scRight");
        let right_block = self.reserve(&right_block_name)?;
        let join_name = self.block_name("scJoin");
        let join = self.reserve(&join_name)?;
        let zero = self.constant(0, &CType::int())?;
        let one = self.constant(1, &CType::int())?;
        // `a && b` starts at false and becomes true only if the right side says
        // so; `a || b` starts at true. Either way the left side decides whether
        // the right side runs at all.
        self.store(result, if and { zero } else { one }, &CType::int())?;
        let test = self.test(left)?;
        self.terminate(Terminator::Branch {
            condition: test,
            then_block: right_block.id,
            otherwise: join.id,
        })?;
        self.switch_to(&right_block)?;
        // The right side's *value* decides the answer, not merely its arrival. A
        // `&&` that stored a constant here would answer `1` for every pair whose
        // left side was true, and `1 && 0` is the most ordinary short-circuit
        // there is — it is the loop guard of every C string routine in the
        // standard library. For `||` the answer is the right side's truth the
        // other way round, so `1 - truth` is `!truth` without a branch.
        let right_value = self.value(right)?;
        let right_truth = self.truth(right_value)?;
        let answer = if and {
            right_truth
        } else {
            self.emit(Instruction::Binary {
                op: IrBinaryOp::Sub,
                left: one,
                right: right_truth,
                ty: IrType::Int {
                    bits: 32,
                    signed: true,
                },
            })?
        };
        self.store(result, answer, &CType::int())?;
        self.terminate(Terminator::Jump(join.id))?;
        self.switch_to(&join)?;
        self.load(result, &CType::int())
    }

    /// `a ? b : c`, with the join value in a temporary.
    fn conditional(
        &mut self,
        condition: &Expression,
        then_value: &Expression,
        else_value: &Expression,
    ) -> Result<ValueId, LowerError> {
        let result = self.temporary(CType::long())?;
        let then_block_name = self.block_name("condThen");
        let then_block = self.reserve(&then_block_name)?;
        let else_block_name = self.block_name("condOtherwise");
        let else_block = self.reserve(&else_block_name)?;
        let join_name = self.block_name("condJoin");
        let join = self.reserve(&join_name)?;
        let test = self.test(condition)?;
        self.terminate(Terminator::Branch {
            condition: test,
            then_block: then_block.id,
            otherwise: else_block.id,
        })?;
        self.switch_to(&then_block)?;
        let taken = self.value(then_value)?;
        self.store(result, taken, &CType::long())?;
        self.terminate(Terminator::Jump(join.id))?;
        self.switch_to(&else_block)?;
        let other = self.value(else_value)?;
        self.store(result, other, &CType::long())?;
        self.builder.terminate(Terminator::Jump(join.id))?;
        self.switch_to(&join)?;
        self.load(result, &CType::long())
    }

    /// `++x` and `x++`, which read and write in a different order and are
    /// therefore two different operations rather than one with a flag.
    fn increment(&mut self, operand: &Expression, increment: bool) -> Result<ValueId, LowerError> {
        let old = self.value(operand)?;
        let one = self.constant(1, &CType::long())?;
        let new = self.emit(Instruction::Binary {
            op: if increment {
                IrBinaryOp::Add
            } else {
                IrBinaryOp::Sub
            },
            left: old,
            right: one,
            ty: IrType::Int {
                bits: 64,
                signed: true,
            },
        })?;
        let address = self.address(operand)?;
        let ty = self.type_of(operand).unwrap_or(CType::long());
        self.store(address, new, &ty)?;
        // A prefix operator yields the *new* value and a postfix one the old, so
        // returning `new` for the prefix and `old` for the postfix is the whole
        // difference between them.
        Ok(new)
    }

    /// A compound assignment, which computes in the widened type and converts
    /// back to the target's.
    fn compound_assign(
        &mut self,
        op: BinaryOp,
        target: &Expression,
        value: &Expression,
    ) -> Result<ValueId, LowerError> {
        let address = self.address(target)?;
        let ty = self.type_of(target).unwrap_or(CType::long());
        let left = self.load(address, &ty)?;
        let right = self.value(value)?;
        let (ir_op, result_signed) = match op {
            BinaryOp::Add => (IrBinaryOp::Add, true),
            BinaryOp::Subtract => (IrBinaryOp::Sub, true),
            BinaryOp::Multiply => (IrBinaryOp::Mul, true),
            BinaryOp::Divide => (IrBinaryOp::DivSigned, true),
            BinaryOp::Remainder => (IrBinaryOp::RemainderSigned, true),
            BinaryOp::ShiftLeft => (IrBinaryOp::ShiftLeft, true),
            BinaryOp::ShiftRight => (IrBinaryOp::ShiftRightArithmetic, true),
            BinaryOp::BitAnd => (IrBinaryOp::BitAnd, true),
            BinaryOp::BitOr => (IrBinaryOp::BitOr, true),
            BinaryOp::BitXor => (IrBinaryOp::BitXor, true),
            _ => {
                return Err(LowerError::Unsupported {
                    what: alloc::format!("`{}=`", op.spelling()),
                    why: String::from("a comparison has no assignment form"),
                });
            }
        };
        let result = self.emit(Instruction::Binary {
            op: ir_op,
            left,
            right,
            ty: IrType::Int {
                bits: 64,
                signed: result_signed,
            },
        })?;
        self.store(address, result, &ty)?;
        Ok(result)
    }

    /// A call, either to another C function or to the ABI.
    fn call(
        &mut self,
        callee: &Expression,
        arguments: &[Expression],
    ) -> Result<ValueId, LowerError> {
        // A call's arguments are converted to their parameters' types, which is
        // C's rule and not a detail: a call to `f(int)` with a 64-bit value on
        // the stack is a *different* call, and the IR verifier says so. The
        // conversion goes through the same store-and-reload a narrow assignment
        // uses, so there is one way to convert rather than two that could
        // disagree about what converting means.
        let callee_ty = self.type_of(callee);
        let parameters = callee_ty.as_ref().and_then(Self::parameters_of);
        let mut args = Vec::with_capacity(arguments.len());
        let mut words = 0u32;
        for (index, argument) in arguments.iter().enumerate() {
            let value = self.value(argument)?;
            let expected = parameters
                .as_ref()
                .and_then(|types| types.get(index))
                .cloned();
            let converted = match &expected {
                Some(expected) => self.convert(value, expected)?,
                None => value,
            };
            words += words_of(&expected.unwrap_or_else(CType::long));
            args.push(CallArg::Value(converted));
        }
        if words > MAX_ARGUMENT_WORDS {
            return Err(LowerError::TooManyArguments { words });
        }
        let result = callee_ty
            .as_ref()
            .and_then(result_of)
            .unwrap_or(CType::int());
        let ir_result =
            ir_type(&result).ok_or_else(|| LowerError::UnsupportedType { ty: result.name() })?;
        let Expression::Name { name, .. } = callee else {
            return Err(LowerError::Unsupported {
                what: String::from("a call through a function pointer"),
                why: String::from(
                    "the machine's `CALL` takes a displacement and `CALLR` takes one register, \
                     and neither reaches a function whose address is only known at run time",
                ),
            });
        };
        if lazalith_os_abi::abi_syscall(name).is_some() {
            // A C call to a name the ABI has becomes the machine's `SYSCALL`,
            // which is the only way a C program reaches the machine's
            // facilities. The IR name is mangled so it cannot collide with a C
            // function of the same name, and the backend strips the prefix.
            //
            // The name is recorded so a declaration can be added: the IR
            // verifier resolves a `Syscall` target as a function in the module,
            // and a declaration is how the IR says "this name exists and has no
            // body here". Without one the verifier refuses the call, which is
            // the right answer for a syscall nobody numbered.
            if !self.syscall_names.iter().any(|seen| seen == name) {
                self.syscall_names.push(name.to_string());
            }
            return self.emit(Instruction::Call {
                target: CallTarget::Syscall(Name::from(format!("{SYSCALL_PREFIX}{name}"))),
                args,
                result: ir_result,
            });
        }
        self.emit(Instruction::Call {
            target: CallTarget::Function(ir_name(name)),
            args,
            result: ir_result,
        })
    }

    /// Interns a string literal as a data segment and returns its name.
    fn intern_string(&mut self, value: &str) -> Name {
        if let Some((_, name)) = self.strings.iter().find(|(seen, _)| seen == value) {
            return name.clone();
        }
        let name = Name::from(format!("str{}", self.strings.len()));
        self.strings.push((value.to_string(), name.clone()));
        name
    }

    /// The type an expression has.
    ///
    /// The checker's rules are the authority, and this asks the same tables the
    /// checker built rather than re-deriving anything: a name's type is its
    /// declared one, a dereference is the pointee, and a binary is its left
    /// operand's.
    fn type_of(&self, expression: &Expression) -> Option<CType> {
        match expression {
            Expression::Name { name, .. } => self.type_of_name(name),
            // A constant is held in a 64-bit register whatever its base, and a
            // 32-bit one is widened on load, so the value a constant has before it
            // meets a variable does not depend on how it was spelled.
            Expression::Integer { .. } => Some(CType::long()),
            Expression::Character { .. } => Some(CType::int()),
            Expression::String { value, .. } => Some(CType::array_of(
                CType::Int {
                    bits: 8,
                    signed: true,
                },
                value.len() as u32 + 1,
            )),
            Expression::Group(inner) => self.type_of(inner),
            Expression::Address(inner) => {
                self.type_of(inner).map(|ty| CType::Pointer(Box::new(ty)))
            }
            Expression::Dereference(inner) => self.type_of(inner).as_ref().and_then(element_of),
            Expression::Member {
                record,
                member,
                arrow,
                ..
            } => {
                let record_ty = if *arrow {
                    self.type_of(record).as_ref().and_then(element_of)
                } else {
                    self.type_of(record)
                }?;
                field_of(&record_ty, member).map(|field| field.ty)
            }
            Expression::Subscript { array, .. } => {
                self.type_of(array).as_ref().and_then(subscript_of)
            }
            Expression::Not(_) | Expression::Logical { .. } => Some(CType::int()),
            // C says a comparison produces an `int` and everything else produces its
            // operands' common type, and the common type is the left operand's here
            // because the usual arithmetic conversions are the checker's job and this
            // only needs a width wide enough to hold whatever the result was.
            Expression::Binary { op, left, .. } => {
                if op.is_comparison() {
                    Some(CType::int())
                } else {
                    self.type_of(left)
                }
            }
            Expression::Cast { .. } => self.cast_type(expression),
            _ => None,
        }
    }

    /// A cast's target type, as the type checker recorded it.
    ///
    /// `(T)` names a *type*, and building one needs the typedef and tag tables,
    /// which belong to the checker. So the checker records the type it resolved and
    /// this reads it back by where the cast starts.
    fn cast_type(&self, expression: &Expression) -> Option<CType> {
        self.program
            .cast_types
            .get(&crate::types::expression_span(expression).start().as_u32())
            .cloned()
    }

    /// Converts a value to a type, through a temporary.
    ///
    /// C's numeric conversions are all "keep the low bits and reinterpret", and
    /// the one place that is expressed is a store followed by a load at the target's
    /// width. Going through [`Emitter::store`] is what makes an argument conversion and
    /// an assignment the *same* operation rather than two that could disagree about
    /// what converting means.
    fn convert(&mut self, value: ValueId, ty: &CType) -> Result<ValueId, LowerError> {
        let offset = self.reserve_slot(ty, SlotPurpose::CastScratch, None, false)?;
        let scratch = self.frame_address(offset)?;
        self.store(scratch, value, ty)?;
        self.load(scratch, ty)
    }

    /// A function type's parameter types, through a pointer to one.
    fn parameters_of(ty: &CType) -> Option<Vec<CType>> {
        match ty {
            CType::Function(signature) => Some(signature.params.clone()),
            CType::Pointer(pointee) => Self::parameters_of(pointee),
            _ => None,
        }
    }
}

/// A record's fields, for a member lookup.
fn record_of(ty: &CType) -> RecordType {
    match ty {
        CType::Struct(record) | CType::Union(record) => (**record).clone(),
        _ => RecordType {
            tag: None,
            union: false,
            fields: Vec::new(),
            complete: false,
        },
    }
}

/// A field of a record, by name.
fn field_of(ty: &CType, member: &str) -> Option<crate::ctypes::Field> {
    record_of(ty)
        .fields
        .iter()
        .find(|field| field.name == member)
        .cloned()
}

/// A pointer's element type.
/// The type an index or a dereference yields.
///
/// Both a pointer and an array have elements, and C treats `a[i]` as `*(a + i)`
/// whether `a` is a pointer or an array — an array *is* a pointer to its first
/// element in every expression that reads it. So the element type of `char[4]`
/// is `char`, and a subscript that asked only about pointers fell back to `int`
/// and stepped eight bytes per index instead of one, which made `buffer[2]` read
/// a byte the program had never written.
fn element_of(ty: &CType) -> Option<CType> {
    match ty {
        CType::Pointer(element) => Some((**element).clone()),
        CType::Array { element, .. } => Some((**element).clone()),
        _ => None,
    }
}

/// A function type's result, through a pointer to one.
fn result_of(ty: &CType) -> Option<CType> {
    match ty {
        CType::Function(signature) => Some(signature.result.clone()),
        CType::Pointer(pointee) => result_of(pointee),
        _ => None,
    }
}

/// How many words a value of a type occupies.
fn words_of(ty: &CType) -> u32 {
    ty.size_in_bytes().unwrap_or(8).div_ceil(8).max(1)
}

/// Writes an integer into a byte image at an offset.
///
/// A narrowing constant keeps the *low* bits, because a conversion to a
/// narrower type in C is defined as a conversion and not a saturation.
fn write_integer(bytes: &mut [u8], at: usize, ty: &CType, value: i64) {
    let CType::Int { bits, .. } = ty else {
        return;
    };
    let width = (*bits / 8) as usize;
    if at + width > bytes.len() {
        return;
    }
    let stored = value as u64;
    for index in 0..width {
        bytes[at + index] = ((stored >> (index * 8)) & 0xff) as u8;
    }
}

/// An integer constant expression's value.
///
/// Only the forms C allows in a *static* initialiser appear here. A form outside
/// that set has already been reported by the type checker, so `None` means "not
/// a constant" and the caller has said so.
fn constant(expression: &Expression) -> Option<i64> {
    match expression {
        Expression::Integer { number, .. } => {
            if number.digits.is_empty() {
                Some(0)
            } else {
                i64::from_str_radix(&number.digits, number.base).ok()
            }
        }
        Expression::Character { value, .. } => Some(*value),
        Expression::Group(inner) => constant(inner),
        Expression::Plus(inner) => constant(inner),
        Expression::Minus(inner) => Some(constant(inner)?.wrapping_neg()),
        Expression::BitNot(inner) => Some(!constant(inner)?),
        Expression::Not(inner) => Some(i64::from(constant(inner)? == 0)),
        Expression::Cast { operand, .. } => constant(operand),
        Expression::Binary { op, left, right } => {
            let left = constant(left)?;
            let right = constant(right)?;
            if op.is_comparison() {
                return Some(match op {
                    BinaryOp::Equal => i64::from(left == right),
                    BinaryOp::NotEqual => i64::from(left != right),
                    BinaryOp::Less => i64::from(left < right),
                    BinaryOp::LessEqual => i64::from(left <= right),
                    BinaryOp::Greater => i64::from(left > right),
                    BinaryOp::GreaterEqual => i64::from(left >= right),
                    _ => return None,
                });
            }
            match op {
                BinaryOp::Add => Some(left.wrapping_add(right)),
                BinaryOp::Subtract => Some(left.wrapping_sub(right)),
                BinaryOp::Multiply => Some(left.wrapping_mul(right)),
                BinaryOp::Divide if right != 0 => Some(left.wrapping_div(right)),
                BinaryOp::Remainder if right != 0 => Some(left.wrapping_rem(right)),
                BinaryOp::ShiftLeft if (0..64).contains(&right) => {
                    Some(left.wrapping_shl(right as u32))
                }
                BinaryOp::ShiftRight if (0..64).contains(&right) => {
                    Some(left.wrapping_shr(right as u32))
                }
                BinaryOp::BitAnd => Some(left & right),
                BinaryOp::BitOr => Some(left | right),
                BinaryOp::BitXor => Some(left ^ right),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The IR type a C type becomes.
///
/// `None` for a type with no representation, which is how this stage refuses
/// rather than approximates.
pub fn ir_type(ty: &CType) -> Option<IrType> {
    match ty {
        CType::Void => Some(IrType::Void),
        CType::Bool => Some(IrType::Bool),
        CType::Int { bits, signed } => Some(IrType::Int {
            bits: *bits,
            signed: *signed,
        }),
        CType::Pointer(_) => Some(IrType::Pointer),
        CType::Function(_) => Some(IrType::Pointer),
        CType::Enum(_) => Some(IrType::Int {
            bits: 32,
            signed: true,
        }),
        CType::Array { element, length } => {
            let element = ir_type(element)?;
            if matches!(element, IrType::Void) {
                return None;
            }
            let mut fields = Vec::with_capacity(*length as usize);
            for index in 0..*length {
                fields.push(RecordField {
                    name: index.to_string(),
                    ty: element.clone(),
                });
            }
            Some(IrType::Record { fields })
        }
        CType::Struct(record) | CType::Union(record) => {
            if !record.complete {
                return None;
            }
            let mut fields = Vec::with_capacity(record.fields.len());
            for field in &record.fields {
                fields.push(RecordField {
                    name: field.name.clone(),
                    ty: ir_type(&field.ty)?,
                });
            }
            Some(IrType::Record { fields })
        }
    }
}

/// A value's width in bytes and whether it is signed.
///
/// This is the pair a conversion needs. The width says whether the two sides of a
/// store agree; the signedness says how a *widening* one extends. A `char` and an
/// `unsigned char` are both one byte, and `(int)` of them is the difference between
/// -1 and 255, so a width alone is not enough.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Shape {
    width: u32,
    signed: bool,
}

impl Shape {
    /// A whole word, which is the widest thing the machine holds and the safe
    /// default for a value this emitter did not record.
    const WORD: Self = Self {
        width: 8,
        signed: true,
    };
}

/// The shape of an instruction's result.
///
/// A `void` has no width and the machine holds nothing wider than a word, so both
/// are clamped to the word rather than being special cases at every use.
fn ir_shape(instruction: &Instruction) -> Shape {
    let ty = instruction_result_type(instruction);
    Shape {
        width: ty.size_in_bytes().unwrap_or(8).clamp(1, 8),
        signed: matches!(ty, IrType::Int { signed: true, .. }),
    }
}

/// The IR load width for a C type, with its signedness.
fn load_width(ty: &CType) -> Option<LoadWidth> {
    let signed = matches!(ty, CType::Int { signed: true, .. } | CType::Enum(_));
    match (ty.size_in_bytes()?, signed) {
        (1, false) => Some(LoadWidth::Byte),
        (1, true) => Some(LoadWidth::ByteSigned),
        (2, false) => Some(LoadWidth::Half),
        (2, true) => Some(LoadWidth::HalfSigned),
        (4, false) => Some(LoadWidth::Word),
        (4, true) => Some(LoadWidth::WordSigned),
        (8, _) => Some(LoadWidth::Double),
        _ => None,
    }
}

/// The IR store width for a C type.
fn store_width(ty: &CType) -> Option<StoreWidth> {
    match ty.size_in_bytes()? {
        1 => Some(StoreWidth::Byte),
        2 => Some(StoreWidth::Half),
        4 => Some(StoreWidth::Word),
        8 => Some(StoreWidth::Double),
        _ => None,
    }
}

/// A type as wide as a value of a stated width, for a conversion scratch.
fn scratch_for(bytes: u32) -> CType {
    CType::Int {
        bits: u16::try_from(bytes.max(1) * 8).unwrap_or(64),
        signed: false,
    }
}

/// The element type of a subscript's operand.
///
/// A subscript works on an *array* as well as on a pointer, and they are
/// different types with the same effect. Only `subscript_of` accepts both, and
/// only where C's grammar says the subscript operator appeared — a `*` really
/// does require a pointer, and treating an array as a pointer there would accept
/// `*a` for an array, which is not C.
fn subscript_of(ty: &CType) -> Option<CType> {
    match ty {
        CType::Array { element, .. } => Some((**element).clone()),
        CType::Pointer(element) => Some((**element).clone()),
        _ => None,
    }
}
