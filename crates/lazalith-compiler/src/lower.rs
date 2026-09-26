//! Step 62: lowering the checked tree to IR.
//!
//! # What this stage is responsible for
//!
//! The checked tree has slots with byte offsets, resolved names, loop levels and
//! interned strings, but no instructions, no labels and no control flow. This
//! stage turns all of that into IR, and it owns every representation decision
//! that needs one:
//!
//! - A local lives in the current function's frame, at the offset the frontend
//!   assigned, reached through `Intrinsic::FrameBase`. The frame base is the
//!   machine's stack pointer, so a backend materialises it with real
//!   instructions; it is never a function's address.
//! - Parameters arrive in registers and the prologue stores them into their
//!   slots, so the body reads every local, parameter or not, from the frame.
//! - A `str` and a slice are a pointer and a length: two words, kept together. A
//!   view is built with `Insert` and taken apart with `Extract` and
//!   `Intrinsic::SliceLength`, so a length cannot be dropped by accident, and a
//!   view in the frame is stored as its two words.
//! - A string literal's address is a symbol, not a number, so it is
//!   `Instruction::DataAddress` and the bytes are a data segment.
//! - There are no phi nodes, so a value that comes out of an `if` is written by
//!   each arm into a frame temporary and read after the join.
//! - `&&` and `||` short-circuit through branches rather than through
//!   `Instruction::LogicalAnd`, because an eager evaluation would run the right
//!   side when the left side already decides the answer, and that side can trap.
//! - `break` and `continue` jump to real blocks; neither becomes `unreachable`.
//! - An index is bounds-checked before its address is formed, so an
//!   out-of-range index traps instead of reading memory that is not its own.
//!
//! # What this stage refuses to do
//!
//! - It refuses a 32-bit target. `i64`, `u64` and `usize` are wider than a
//!   32-bit machine's registers, and lowering them honestly means register pairs
//!   and arithmetic the ISA does not have. That is real work, so this stage
//!   reports it instead of miscompiling.
//! - It refuses a call that needs more argument registers than the ABI has, a
//!   call to a syscall the ABI has not numbered, and a place through a view
//!   reference, which has no single address to compute.
//! - It never invents a frame size, a layout, or an address. Every offset comes
//!   from the checked tree or from a temporary this stage allocates and reports
//!   in the frame layout.
//!
//! # Block naming
//!
//! A branch needs the identifier of a block that may not exist yet. The IR
//! builder numbers blocks in creation order, so this stage counts the blocks it
//! has created: `predict` names the next one, and `create_run` checks that the
//! blocks it creates are the ones it predicted. A forward branch therefore
//! targets a real block, and the count is asserted against the finished
//! function.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::error::Error;
use core::fmt;
use core::mem;

use lazalith_ir::{
    BinaryOp, BlockId, CallArg, CallTarget, ComparisonOp, ConstValue, DataSegment, Function,
    FunctionBuilder, Instruction, Intrinsic, IrError, Linkage, LoadWidth, MemorySpace, Module,
    ModuleBuilder, Name, Parameter, RecordField, ReturnValue, StoreWidth, Terminator,
    Type as IrType, UnaryOp, ValueId,
};
use lazalith_types::{SourceSpan, WordWidth};

use crate::types::{
    CheckedArm, CheckedBlock, CheckedExpr, CheckedExtern, CheckedFunction, CheckedPlace,
    CheckedProgram, CheckedStmt, LocalSlot, Type, ty_name,
};

/// The most argument words a call may pass.
///
/// The kernel reads a syscall's number from `r0` and its arguments from `r1`
/// through `r6`, so a call has six argument registers and no more. This counts
/// *words* and not arguments because a view is two words and occupies two
/// registers. A 64-bit integer is one word and one register — the registers are
/// 64 bits wide, so nothing narrower than a view costs more than one.
pub const MAX_ARGUMENT_WORDS: usize = 6;

/// The trap code for falling off the end of a function that owes a value.
///
/// A checked program cannot reach this, because the frontend requires such a
/// function to end in a value. It is here so that a bug traps loudly instead of
/// returning whatever happened to be in a register.
pub const TRAP_FELL_OFF_THE_END: u32 = 1;

/// The trap code for an index outside its array or slice.
pub const TRAP_BOUNDS: u32 = 2;

/// Why a checked program could not be lowered.
#[derive(Debug)]
pub enum LowerError {
    /// The target's word width has no lowering yet.
    UnsupportedTarget {
        /// The target's word width.
        word: WordWidth,
    },
    /// The program has no `main` to start at.
    NoEntryPoint,
    /// A builtin method the lowering does not know.
    UnknownBuiltin {
        /// The method's source name.
        method: String,
        /// Where it was written.
        span: SourceSpan,
    },
    /// An operator the lowering does not know.
    UnknownOperator {
        /// The operator's source text.
        operator: String,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A call needs more argument registers than the ABI has.
    TooManyArgumentWords {
        /// What was called.
        callee: String,
        /// How many words the arguments need.
        needed: usize,
        /// How many the ABI has.
        allowed: usize,
        /// Where the call was written.
        span: SourceSpan,
    },
    /// A call to an extern the ABI has not given a syscall number.
    UnnumberedSyscall {
        /// The extern's name.
        name: String,
        /// Where the call was written.
        span: SourceSpan,
    },
    /// An expression or place whose shape this stage cannot lower.
    UnsupportedShape {
        /// What the shape was.
        detail: String,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A `break` or `continue` that names a loop level this function does not have.
    InvalidLoopLevel {
        /// How many levels it named.
        levels: u32,
        /// Where it was written.
        span: SourceSpan,
    },
    /// The IR itself refused something.
    Ir(IrError),
    /// An allocation failed.
    Allocation,
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget { word } => write!(
                f,
                "no lowering for a {}-bit target yet: 64-bit values need register pairs the \
                 machine does not have",
                word.bits()
            ),
            Self::NoEntryPoint => f.write_str("the program has no `main` to start at"),
            Self::UnknownBuiltin { method, .. } => {
                write!(f, "no lowering for the builtin method `{method}`")
            }
            Self::UnknownOperator { operator, .. } => {
                write!(f, "no lowering for the operator `{operator}`")
            }
            Self::TooManyArgumentWords {
                callee,
                needed,
                allowed,
                ..
            } => write!(
                f,
                "calling {callee} needs {needed} argument words, and the ABI has {allowed}"
            ),
            Self::UnnumberedSyscall { name, .. } => write!(
                f,
                "the ABI has not numbered the syscall {name}, so it cannot be called yet"
            ),
            Self::UnsupportedShape { detail, .. } => write!(f, "cannot lower {detail}"),
            Self::InvalidLoopLevel { levels, .. } => {
                write!(f, "no enclosing loop {levels} levels out")
            }
            Self::Ir(error) => write!(f, "the IR rejected this program: {error}"),
            Self::Allocation => f.write_str("out of memory"),
        }
    }
}

impl Error for LowerError {}

impl From<IrError> for LowerError {
    fn from(error: IrError) -> Self {
        Self::Ir(error)
    }
}

/// Why a frame slot exists, so a later stage can name it in debug information.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SlotPurpose {
    /// A local or parameter the frontend allocated.
    Local,
    /// A value that the arms of an `if` write and the join reads.
    JoinValue,
    /// A loop's end value, computed once before the loop.
    LoopBound,
    /// The result of a short-circuiting operator.
    ShortCircuit,
    /// Somewhere to put a value whose width is changing.
    CastScratch,
}

/// One slot in a lowered function's frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameSlot {
    /// The slot's byte offset from the frame base.
    pub offset: u32,
    /// The slot's size in bytes.
    pub size: u32,
    /// The type the slot holds.
    pub ty: Type,
    /// Why the slot exists.
    pub purpose: SlotPurpose,
    /// Whether this slot receives one of the function's parameters.
    ///
    /// A backend needs this to know which slots a prologue must fill from the
    /// argument registers, and a parameter is otherwise just a local.
    pub is_parameter: bool,
    /// The local's name, when it has one.
    pub name: Option<String>,
}

/// A lowered function's frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    /// The function's qualified name.
    pub function: String,
    /// The frame's total size in bytes, rounded up to a whole word.
    pub size: u32,
    /// The slots, in offset order.
    pub slots: Vec<FrameSlot>,
}

/// A lowered program.
#[derive(Clone, Debug)]
pub struct Lowered {
    /// The verified IR module.
    pub module: Module,
    /// One frame per lowered function, in module order.
    pub frames: Vec<FrameLayout>,
    /// The function the runtime starts at.
    pub entry: String,
    /// A data segment per interned string, in interning order.
    pub strings: Vec<String>,
}

impl Lowered {
    /// The frame layout for a function.
    pub fn frame(&self, function: &str) -> Option<&FrameLayout> {
        self.frames.iter().find(|frame| frame.function == function)
    }
}

/// Lowers a checked program into a verified IR module.
pub fn lower(program: &CheckedProgram) -> Result<Lowered, LowerError> {
    if program.word != WordWidth::W64 {
        return Err(LowerError::UnsupportedTarget { word: program.word });
    }
    let entry = program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .map(|function| function.qualified_name.clone())
        .ok_or(LowerError::NoEntryPoint)?;

    let mut module = ModuleBuilder::new("lazen");
    let mut strings = Vec::new();
    for (index, string) in program.strings.iter().enumerate() {
        let name = format!("str{index}");
        module.add_data(DataSegment {
            name: name.clone(),
            bytes: string.text.as_bytes().to_vec(),
            alignment: 1,
            span: None,
        })?;
        strings.push(name);
    }
    // An extern is declared, not defined: the module needs its signature so that
    // a call can be checked against it, and its body is unreachable because the
    // symbol is defined elsewhere.
    for declaration in &program.externs {
        module.add_function(extern_declaration(declaration)?)?;
    }
    let mut frames = Vec::new();
    for function in &program.functions {
        let (lowered, frame) = FunctionLowering::new(program, function)?.run()?;
        module.add_function(lowered)?;
        frames.push(frame);
    }
    Ok(Lowered {
        module: module.finish()?,
        frames,
        entry,
        strings,
    })
}

/// An extern's IR declaration: its signature, and no body of its own.
fn extern_declaration(declaration: &CheckedExtern) -> Result<Function, LowerError> {
    let mut params = Vec::new();
    for parameter in &declaration.parameters {
        params.push(Parameter {
            name: Name::from(parameter.name.as_str()),
            ty: ir_type(&parameter.ty)?,
        });
    }
    let mut module = ModuleBuilder::new("declaration");
    let mut builder = module.function(
        &declaration.name,
        Linkage::External,
        params,
        ir_type(&declaration.result)?,
    )?;
    builder.switch_to_block("declaration")?;
    builder.terminate(Terminator::Unreachable)?;
    Ok(builder.finish()?)
}

/// Whether a type is a view: a pointer and a length, two words.
fn is_view(ty: &Type) -> bool {
    matches!(ty, Type::Str | Type::Slice { .. })
}

/// Whether a type is an array, whose data lives in the frame itself.
fn is_array(ty: &Type) -> bool {
    matches!(ty, Type::Array { .. })
}

/// The Lazen type's IR type.
fn ir_type(ty: &Type) -> Result<IrType, LowerError> {
    Ok(match ty {
        Type::Unit => IrType::Void,
        Type::Bool => IrType::Bool,
        Type::I8 => int_type(8, true),
        Type::I16 => int_type(16, true),
        Type::I32 => int_type(32, true),
        Type::I64 => int_type(64, true),
        Type::U8 => int_type(8, false),
        Type::U16 => int_type(16, false),
        Type::U32 => int_type(32, false),
        Type::U64 => int_type(64, false),
        Type::Usize => int_type(64, false),
        // A str is a read-only view of bytes, exactly like `&[u8]`.
        Type::Str => IrType::Slice {
            element: Box::new(int_type(8, false)),
            mutable: false,
        },
        Type::Slice { element, mutable } => IrType::Slice {
            element: Box::new(ir_type(element)?),
            mutable: *mutable,
        },
        // An array is inline data, not a view: its value is its own storage,
        // with one field per element, so its size and its element offsets are
        // the array's own and not a pointer's.
        Type::Array { element, length } => {
            let element = ir_type(element)?;
            let mut fields = Vec::new();
            for index in 0..*length {
                fields.push(RecordField {
                    name: index.to_string(),
                    ty: element.clone(),
                });
            }
            IrType::Record { fields }
        }
        Type::Pointer { .. } | Type::Reference { .. } => IrType::Pointer,
    })
}

fn int_type(bits: u16, signed: bool) -> IrType {
    IrType::Int { bits, signed }
}

/// A scalar's size in bytes and its load and store widths.
///
/// A pointer and a reference are one word each, exactly as
/// `Type::size_in_bytes` says, so they load and store like a `usize`. Leaving
/// them out made a `ptr<T>` local impossible to lower at all.
fn scalar_width(ty: &Type) -> Option<(u32, LoadWidth, StoreWidth)> {
    Some(match ty {
        Type::Bool => (1, LoadWidth::Byte, StoreWidth::Byte),
        Type::I8 => (1, LoadWidth::ByteSigned, StoreWidth::Byte),
        Type::U8 => (1, LoadWidth::Byte, StoreWidth::Byte),
        Type::I16 => (2, LoadWidth::HalfSigned, StoreWidth::Half),
        Type::U16 => (2, LoadWidth::Half, StoreWidth::Half),
        Type::I32 => (4, LoadWidth::WordSigned, StoreWidth::Word),
        Type::U32 => (4, LoadWidth::Word, StoreWidth::Word),
        Type::I64 | Type::U64 | Type::Usize => (8, LoadWidth::Double, StoreWidth::Double),
        Type::Pointer { .. } | Type::Reference { .. } => (8, LoadWidth::Double, StoreWidth::Double),
        _ => return None,
    })
}

/// Whether a comparison on this type is signed.
fn is_signed(ty: &Type) -> bool {
    matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64)
}

/// How many words a value occupies in an argument register.
/// How many machine words an argument of this type occupies in a call.
///
/// This is the size of the value rounded up to words, and nothing else: the
/// argument registers are 64 bits wide, so an `i64`, a `u64` and a `usize` are
/// one word each and only a two-word view costs two. Counting a 64-bit integer
/// as two words refused calls the ABI can pass — `list_directory` takes two of
/// them — and disagreed with the code generator, which sized the same argument
/// from the value's own size.
pub fn argument_words(ty: &Type) -> usize {
    usize::try_from(lazalith_ir::argument_words(
        &ir_type(ty).unwrap_or(IrType::Void),
    ))
    .unwrap_or(usize::MAX)
}

/// Rounds a size up to a whole word.
fn round_up_word(size: u32) -> u32 {
    size.saturating_add(7) / 8 * 8
}

/// Where a loop's `break` and `continue` go.
#[derive(Clone, Copy, Debug)]
struct LoopTargets {
    continue_block: BlockId,
    break_block: BlockId,
}

/// A block this stage has reserved but not filled in yet.
///
/// The identifier is the IR builder's own, not a prediction. A loop or a
/// conditional has to name a block it has not built yet, and the number that
/// block *would* get is not knowable in advance: a body containing another `if`,
/// or a short-circuiting `&&`, creates blocks of its own and shifts every later
/// number. Reserving the block makes the identifier real, so a `break` inside a
/// nested `if` names the loop's exit rather than whatever block happens to sit
/// where the exit was predicted to be.
#[derive(Clone, Debug)]
struct Reserved {
    /// The block's label, which is also how it is found again to fill it in.
    name: String,
    /// The block's real identifier.
    id: BlockId,
}

/// One function's lowering.
struct FunctionLowering<'a> {
    program: &'a CheckedProgram,
    function: &'a CheckedFunction,
    builder: FunctionBuilder,
    result: Type,
    /// Whether the current block already has its terminator.
    ///
    /// A body that ends in `return`, `break` or `continue` leaves its block
    /// closed, and the code that follows must not add a jump to it. The IR
    /// builder would reject that, but it has no way to ask, so this stage keeps
    /// the answer.
    terminated: bool,
    /// The next block label's number, so that no two labels collide.
    next_label: u32,
    /// The next free byte offset above the frontend's frame.
    next_temporary: u32,
    /// The slots this stage allocated.
    temporaries: Vec<FrameSlot>,
    /// The frame slots the frontend allocated, by slot number.
    locals: BTreeMap<u32, &'a LocalSlot>,
    /// The enclosing loops, innermost last.
    loops: Vec<LoopTargets>,
    /// A reusable one-byte slot for a short-circuiting operator's result.
    short_circuit: Option<u32>,
    /// A reusable word slot for a cast that changes width.
    cast_scratch: Option<u32>,
}

impl<'a> FunctionLowering<'a> {
    fn new(program: &'a CheckedProgram, function: &'a CheckedFunction) -> Result<Self, LowerError> {
        let mut params = Vec::new();
        for parameter in &function.parameters {
            params.push(Parameter {
                name: Name::from(parameter.name.as_str()),
                ty: ir_type(&parameter.ty)?,
            });
        }
        let mut module = ModuleBuilder::new("lowering");
        let linkage = if function.is_public {
            Linkage::Global
        } else {
            Linkage::Local
        };
        // The span is recorded *before* the function is started, because a span
        // belongs to the function that follows it. Recording it afterwards left
        // every lowered function with no span at all, which is where a backend's
        // debug information comes from.
        module.set_function_span(function.span.clone());
        let mut builder = module.function(
            &function.qualified_name,
            linkage,
            params,
            ir_type(&function.result)?,
        )?;
        builder.switch_to_block("entry")?;
        let locals = function
            .locals
            .iter()
            .map(|local| (local.slot, local))
            .collect();
        Ok(Self {
            program,
            function,
            builder,
            result: function.result.clone(),
            terminated: false,
            next_label: 0,
            next_temporary: round_up_word(function.frame_size),
            temporaries: Vec::new(),
            locals,
            loops: Vec::new(),
            short_circuit: None,
            cast_scratch: None,
        })
    }

    /// Lowers the body, returning the function and its frame layout.
    fn run(mut self) -> Result<(Function, FrameLayout), LowerError> {
        let body = &self.function.body;
        for statement in &body.statements {
            self.statement(statement)?;
        }
        // The function body's tail is the one tail that is a return value.
        if let Some(tail) = &body.tail {
            let value = self.value(tail)?;
            if self.result != Type::Unit {
                self.close(Terminator::Return(ReturnValue::Value(value)))?;
            }
        }
        // The body's tail may have returned already, in which case the function
        // is finished and there is nothing left to close.
        if !self.terminated {
            if self.result == Type::Unit {
                self.close(Terminator::Return(ReturnValue::Void))?;
            } else {
                // The frontend requires a value here, so this is unreachable in
                // a checked program. It traps rather than returning a stale
                // value.
                self.builder.emit_effect(Instruction::Trap {
                    code: TRAP_FELL_OFF_THE_END,
                })?;
                self.close(Terminator::Unreachable)?;
            }
        }
        let builder = mem::replace(
            &mut self.builder,
            ModuleBuilder::new("discard")
                .function("discard", Linkage::Local, Vec::new(), IrType::Void)
                .map_err(LowerError::from)?,
        );
        let lowered = builder.finish()?;
        let mut slots: Vec<FrameSlot> = self
            .function
            .locals
            .iter()
            .map(|local| FrameSlot {
                offset: local.offset,
                size: local.ty.size_in_bytes(WordWidth::W64),
                ty: local.ty.clone(),
                purpose: SlotPurpose::Local,
                is_parameter: local.is_parameter,
                name: Some(local.name.clone()),
            })
            .collect();
        slots.append(&mut self.temporaries);
        slots.sort_by_key(|slot| slot.offset);
        Ok((
            lowered,
            FrameLayout {
                function: self.function.qualified_name.clone(),
                size: round_up_word(self.next_temporary),
                slots,
            },
        ))
    }

    // -- emitting --

    fn emit(&mut self, instruction: Instruction) -> Result<ValueId, LowerError> {
        Ok(self.builder.emit(instruction)?)
    }

    fn effect(&mut self, instruction: Instruction) -> Result<(), LowerError> {
        self.builder.emit_effect(instruction)?;
        Ok(())
    }

    fn constant(&mut self, value: i64, ty: &Type) -> Result<ValueId, LowerError> {
        self.emit(Instruction::Const {
            value: ConstValue::Int(value),
            ty: ir_type(ty)?,
        })
    }

    /// A fresh label, unique within the function.
    fn label(&mut self, stem: &str) -> String {
        let name = format!("{stem}.{}", self.next_label);
        self.next_label += 1;
        name
    }

    /// Reserves a block, so a branch can name it before it is filled in.
    ///
    /// The identifier comes back from the IR builder, so it is the identifier the
    /// block will really have. That is the whole point: a loop body may create
    /// blocks of its own, and a number worked out in advance would then name the
    /// wrong block.
    fn reserve(&mut self, stem: &str) -> Result<Reserved, LowerError> {
        let name = self.label(stem);
        let id = self.builder.reserve_block(&name)?;
        Ok(Reserved { name, id })
    }

    /// Reserves a run of blocks, in order.
    fn reserve_run(&mut self, stems: &[&str]) -> Result<Vec<Reserved>, LowerError> {
        let mut reserved = Vec::new();
        for stem in stems {
            reserved.push(self.reserve(stem)?);
        }
        Ok(reserved)
    }

    /// Enters a reserved block to fill it in.
    fn fill(&mut self, block: &Reserved) -> Result<BlockId, LowerError> {
        let id = self.builder.switch_to_block(&block.name)?;
        debug_assert_eq!(
            id, block.id,
            "a reserved block was given a different identifier"
        );
        self.terminated = false;
        Ok(id)
    }

    /// Terminates the current block, unless something already did.
    fn close(&mut self, terminator: Terminator) -> Result<(), LowerError> {
        if self.terminated {
            return Ok(());
        }
        self.builder.terminate(terminator)?;
        self.terminated = true;
        Ok(())
    }

    /// Jumps to a block, unless the current block is already closed.
    fn jump(&mut self, target: BlockId) -> Result<(), LowerError> {
        self.close(Terminator::Jump(target))
    }

    /// Allocates a temporary slot and returns its offset.
    fn temporary(&mut self, size: u32, ty: Type, purpose: SlotPurpose) -> Result<u32, LowerError> {
        let offset = self.next_temporary;
        self.next_temporary = self
            .next_temporary
            .checked_add(round_up_word(size.max(1)))
            .ok_or(LowerError::Allocation)?;
        self.temporaries.push(FrameSlot {
            offset,
            size,
            ty,
            purpose,
            is_parameter: false,
            name: None,
        });
        Ok(offset)
    }

    // -- the frame --

    /// The frame base.
    fn frame_base(&mut self) -> Result<ValueId, LowerError> {
        self.emit(Instruction::Intrinsic {
            kind: Intrinsic::FrameBase,
            operand: None,
            result: IrType::Pointer,
        })
    }

    /// The address of a frame offset.
    fn frame_address(&mut self, offset: u32) -> Result<ValueId, LowerError> {
        let base = self.frame_base()?;
        let delta = self.emit(Instruction::Const {
            value: ConstValue::Pointer(u64::from(offset)),
            ty: IrType::Pointer,
        })?;
        self.emit(Instruction::Binary {
            op: BinaryOp::Add,
            left: base,
            right: delta,
            ty: IrType::Pointer,
        })
    }

    /// `address + offset`.
    fn add_offset(&mut self, address: ValueId, offset: u32) -> Result<ValueId, LowerError> {
        let delta = self.emit(Instruction::Const {
            value: ConstValue::Pointer(u64::from(offset)),
            ty: IrType::Pointer,
        })?;
        self.emit(Instruction::Binary {
            op: BinaryOp::Add,
            left: address,
            right: delta,
            ty: IrType::Pointer,
        })
    }

    /// Reads a scalar from the frame.
    fn load_slot(&mut self, offset: u32, ty: &Type) -> Result<ValueId, LowerError> {
        let (_, width, _) = self.scalar_width(ty, offset)?;
        let address = self.frame_address(offset)?;
        self.emit(Instruction::Load {
            address,
            width,
            space: MemorySpace::Program,
            ty: ir_type(ty)?,
        })
    }

    /// Writes a scalar into the frame.
    fn store_slot(&mut self, offset: u32, value: ValueId, ty: &Type) -> Result<(), LowerError> {
        let (_, _, width) = self.scalar_width(ty, offset)?;
        let address = self.frame_address(offset)?;
        self.effect(Instruction::Store {
            address,
            value,
            width,
            space: MemorySpace::Program,
        })
    }

    fn scalar_width(
        &self,
        ty: &Type,
        offset: u32,
    ) -> Result<(u32, LoadWidth, StoreWidth), LowerError> {
        scalar_width(ty).ok_or_else(|| LowerError::UnsupportedShape {
            detail: format!(
                "a value of type `{}` in the frame at offset {offset} as a single word",
                ty_name(ty)
            ),
            span: self.function.span.clone(),
        })
    }

    // -- views --

    /// Splits a view into its address and its length.
    fn view_parts(&mut self, view: ValueId) -> Result<(ValueId, ValueId), LowerError> {
        let address = self.emit(Instruction::Extract {
            aggregate: view,
            offset: 0,
            ty: IrType::Pointer,
        })?;
        let length = self.emit(Instruction::Intrinsic {
            kind: Intrinsic::SliceLength,
            operand: Some(view),
            result: int_type(64, false),
        })?;
        Ok((address, length))
    }

    /// Builds a view from an address and a length.
    fn build_view(
        &mut self,
        address: ValueId,
        length: ValueId,
        ty: &Type,
    ) -> Result<ValueId, LowerError> {
        let view_ty = ir_type(ty)?;
        // A zero view of the right type is the starting value, so the two
        // inserts determine every bit of the result. Both halves are written: a
        // view whose length was never set would be a view of nothing.
        let zero = self.emit(Instruction::Const {
            value: ConstValue::Int(0),
            ty: view_ty.clone(),
        })?;
        let with_address = self.emit(Instruction::Insert {
            aggregate: zero,
            offset: 0,
            value: address,
            result: view_ty.clone(),
        })?;
        self.emit(Instruction::Insert {
            aggregate: with_address,
            offset: 8,
            value: length,
            result: view_ty,
        })
    }

    /// Reads a view from a frame slot: its address word and its length word.
    fn load_view_slot(&mut self, offset: u32, ty: &Type) -> Result<ValueId, LowerError> {
        let address = self.load_slot(offset, &Type::U64)?;
        let length = self.load_slot(offset + 8, &Type::Usize)?;
        self.build_view(address, length, ty)
    }

    /// Writes a view into a frame slot as its two words.
    fn store_view_slot(
        &mut self,
        offset: u32,
        view: ValueId,
        _ty: &Type,
    ) -> Result<(), LowerError> {
        let (address, length) = self.view_parts(view)?;
        self.store_slot(offset, address, &Type::U64)?;
        self.store_slot(offset + 8, length, &Type::Usize)
    }

    /// Reads a scalar or a view from an address.
    fn load_from(&mut self, address: ValueId, ty: &Type) -> Result<ValueId, LowerError> {
        if is_view(ty) {
            let view_address = self.emit(Instruction::Load {
                address,
                width: LoadWidth::Double,
                space: MemorySpace::Program,
                ty: IrType::Pointer,
            })?;
            let second = self.add_offset(address, 8)?;
            let view_length = self.emit(Instruction::Load {
                address: second,
                width: LoadWidth::Double,
                space: MemorySpace::Program,
                ty: int_type(64, false),
            })?;
            return self.build_view(view_address, view_length, ty);
        }
        let (_, width, _) = self.scalar_width(ty, 0)?;
        self.emit(Instruction::Load {
            address,
            width,
            space: MemorySpace::Program,
            ty: ir_type(ty)?,
        })
    }

    /// Writes a scalar or a view to an address.
    fn store_to(&mut self, address: ValueId, value: ValueId, ty: &Type) -> Result<(), LowerError> {
        if is_view(ty) {
            let (view_address, view_length) = self.view_parts(value)?;
            self.effect(Instruction::Store {
                address,
                value: view_address,
                width: StoreWidth::Double,
                space: MemorySpace::Program,
            })?;
            let second = self.add_offset(address, 8)?;
            return self.effect(Instruction::Store {
                address: second,
                value: view_length,
                width: StoreWidth::Double,
                space: MemorySpace::Program,
            });
        }
        let (_, _, width) = self.scalar_width(ty, 0)?;
        self.effect(Instruction::Store {
            address,
            value,
            width,
            space: MemorySpace::Program,
        })
    }

    // -- places --

    /// The address of a place.
    fn place_address(&mut self, place: &CheckedPlace) -> Result<ValueId, LowerError> {
        match place {
            CheckedPlace::Local { offset, .. } if is_view(place.ty()) => {
                // A view local holds a pointer and a length, not its referent, so
                // the address of what it names is the pointer it holds. Indexing
                // one reaches through it: `view[at]` writes where the view points,
                // not over the two words of the view itself.
                self.load_slot(*offset, &Type::U64)
            }
            CheckedPlace::Local { offset, .. } => self.frame_address(*offset),
            CheckedPlace::Index {
                base,
                index,
                element_offset,
                ty,
                span,
                ..
            } => {
                let index_value = self.value(index)?;
                // The check comes before the address is formed, so an
                // out-of-range index traps instead of forming an address that
                // would read memory that is not the element's.
                let length = self.place_length(base, span)?;
                self.effect(Instruction::BoundsCheck {
                    index: index_value,
                    length,
                    code: TRAP_BOUNDS,
                })?;
                let element_size = ty.size_in_bytes(WordWidth::W64).max(1);
                let base_address = self.place_address(base)?;
                let scaled = self.scale(index_value, element_size)?;
                let moved = self.emit(Instruction::Binary {
                    op: BinaryOp::Add,
                    left: base_address,
                    right: scaled,
                    ty: IrType::Pointer,
                })?;
                self.add_offset(moved, *element_offset)
            }
            CheckedPlace::Deref {
                reference,
                ty,
                span,
                ..
            } => {
                if is_view(ty) {
                    // A view's referent has no single address: the reference *is*
                    // the view, so the only address available is inside whatever
                    // holds the reference, which this stage does not track.
                    // Reaching through one is refused rather than guessed at.
                    return Err(LowerError::UnsupportedShape {
                        detail: String::from("a place through a view reference"),
                        span: span.clone(),
                    });
                }
                self.value(reference)
            }
        }
    }

    /// How many elements a place holds, as the value a bounds check compares with.
    fn place_length(
        &mut self,
        place: &CheckedPlace,
        span: &SourceSpan,
    ) -> Result<ValueId, LowerError> {
        let ty = place.ty().clone();
        match (&ty, place) {
            (Type::Array { length, .. }, _) => self.constant(*length as i64, &Type::Usize),
            (_, CheckedPlace::Local { offset, .. }) => self.load_slot(offset + 8, &Type::Usize),
            (_, CheckedPlace::Index { .. }) => Err(LowerError::UnsupportedShape {
                detail: String::from("a length for a place that is not an array or a view"),
                span: span.clone(),
            }),
            (_, CheckedPlace::Deref { reference, .. }) => {
                let view = self.value(reference)?;
                let (_, length) = self.view_parts(view)?;
                Ok(length)
            }
        }
    }

    /// Multiplies an index by an element's size, as a pointer offset.
    fn scale(&mut self, index: ValueId, element_size: u32) -> Result<ValueId, LowerError> {
        if element_size == 1 {
            return Ok(index);
        }
        let size = self.constant(i64::from(element_size), &Type::Usize)?;
        self.emit(Instruction::Binary {
            op: BinaryOp::Mul,
            left: size,
            right: index,
            ty: ir_type(&Type::Usize)?,
        })
    }

    /// Reads whatever a place holds.
    fn read_place(&mut self, place: &CheckedPlace) -> Result<ValueId, LowerError> {
        let ty = place.ty().clone();
        if is_array(&ty) {
            // An array's value would be a copy of every element. The frontend
            // has no way to name such a value, so this is only reachable from a
            // builtin method, and those take the address instead.
            return Err(LowerError::UnsupportedShape {
                detail: format!(
                    "an array of `{}` as a value: an array is its own storage",
                    ty_name(&ty)
                ),
                span: place.span().clone(),
            });
        }
        match place {
            CheckedPlace::Local { offset, .. } if is_view(&ty) => self.load_view_slot(*offset, &ty),
            CheckedPlace::Local { offset, .. } => self.load_slot(*offset, &ty),
            CheckedPlace::Index { .. } | CheckedPlace::Deref { .. } => {
                let address = self.place_address(place)?;
                self.load_from(address, &ty)
            }
        }
    }

    /// Writes whatever a place holds.
    fn write_place(&mut self, place: &CheckedPlace, value: ValueId) -> Result<(), LowerError> {
        let ty = place.ty().clone();
        if is_array(&ty) {
            return Err(LowerError::UnsupportedShape {
                detail: String::from("an array assigned as a single value"),
                span: place.span().clone(),
            });
        }
        match place {
            CheckedPlace::Local { offset, .. } if is_view(&ty) => {
                self.store_view_slot(*offset, value, &ty)
            }
            CheckedPlace::Local { offset, .. } => self.store_slot(*offset, value, &ty),
            CheckedPlace::Index { .. } | CheckedPlace::Deref { .. } => {
                let address = self.place_address(place)?;
                self.store_to(address, value, &ty)
            }
        }
    }

    /// Initialises a place, element by element when it is an array.
    ///
    /// An array is its own storage, so `let values = [1, 2, 3];` is three stores
    /// into the local's slot and not a copy of a value. The count in the place's
    /// type is the count, so a literal cannot half-fill a slot: the frontend
    /// fixes the type from the literal.
    fn initialise(&mut self, place: &CheckedPlace, expr: &CheckedExpr) -> Result<(), LowerError> {
        let ty = place.ty().clone();
        match (&ty, expr) {
            (Type::Array { element, length }, CheckedExpr::Array { elements, .. }) => {
                if elements.len() as u64 != *length {
                    return Err(LowerError::UnsupportedShape {
                        detail: format!(
                            "an array of {length} elements initialised from {}",
                            elements.len()
                        ),
                        span: expr.span().clone(),
                    });
                }
                let base = self.place_address(place)?;
                let size = element.size_in_bytes(WordWidth::W64).max(1);
                for (index, item) in elements.iter().enumerate() {
                    let value = self.value(item)?;
                    let offset = u32::try_from(index).unwrap_or(u32::MAX) * size;
                    let address = self.add_offset(base, offset)?;
                    self.store_to(address, value, element)?;
                }
                Ok(())
            }
            (Type::Array { element, length }, CheckedExpr::ArrayRepeat { value, count, .. }) => {
                if *count != *length {
                    return Err(LowerError::UnsupportedShape {
                        detail: format!("an array of {length} elements initialised from {count}"),
                        span: expr.span().clone(),
                    });
                }
                let base = self.place_address(place)?;
                let size = element.size_in_bytes(WordWidth::W64).max(1);
                let item = self.value(value)?;
                for index in 0..*length {
                    let offset = u32::try_from(index).unwrap_or(u32::MAX) * size;
                    let address = self.add_offset(base, offset)?;
                    self.store_to(address, item, element)?;
                }
                Ok(())
            }
            (_, _) => {
                let value = self.value(expr)?;
                self.write_place(place, value)
            }
        }
    }

    // -- statements --

    /// Lowers a nested block: its statements, then its tail for its effects.
    ///
    /// A nested block is not the function's body, so its tail is not the
    /// function's return value even when the function returns something. Only
    /// `run` knows that, and only it turns a tail into a return.
    fn block(&mut self, block: &CheckedBlock) -> Result<(), LowerError> {
        for statement in &block.statements {
            self.statement(statement)?;
        }
        match &block.tail {
            // The tail is still evaluated: it may contain a call.
            Some(tail) => {
                self.value(tail)?;
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn statement(&mut self, statement: &CheckedStmt) -> Result<(), LowerError> {
        match statement {
            CheckedStmt::Let { local, value, .. } => match local {
                Some(local) => {
                    let place = CheckedPlace::Local {
                        slot: local.slot,
                        offset: local.offset,
                        ty: local.ty.clone(),
                        mutable: local.mutable,
                        span: local.span.clone(),
                    };
                    self.initialise(&place, value)
                }
                // `let _ = ...;` evaluates and discards, so a call in it happens.
                None => self.value(value).map(|_| ()),
            },
            CheckedStmt::Assign { place, value, .. } => self.initialise(place, value),
            CheckedStmt::Expression { expression, .. } => self.value(expression).map(|_| ()),
            CheckedStmt::If { arms, .. } => self.conditional(arms, None).map(|_| ()),
            CheckedStmt::While {
                condition, body, ..
            } => self.while_loop(condition, body),
            CheckedStmt::For {
                slot,
                ty,
                start,
                end,
                body,
                ..
            } => self.for_loop(*slot, ty, start, end, body),
            CheckedStmt::Loop { body, .. } => self.loop_loop(body),
            CheckedStmt::Break { levels, span } => {
                let target = self.loop_target(*levels, span.clone(), true)?;
                self.close(Terminator::Jump(target))
            }
            CheckedStmt::Continue { levels, span } => {
                let target = self.loop_target(*levels, span.clone(), false)?;
                self.close(Terminator::Jump(target))
            }
            CheckedStmt::Return { value, .. } => match value {
                Some(value) if self.result != Type::Unit => {
                    let value = self.value(value)?;
                    self.close(Terminator::Return(ReturnValue::Value(value)))
                }
                Some(value) => {
                    self.value(value)?;
                    self.close(Terminator::Return(ReturnValue::Void))
                }
                None => self.close(Terminator::Return(ReturnValue::Void)),
            },
            CheckedStmt::Block { block, .. } => self.block(block),
        }
    }

    /// The block a `break` or `continue` jumps to.
    fn loop_target(
        &self,
        levels: u32,
        span: SourceSpan,
        breaking: bool,
    ) -> Result<BlockId, LowerError> {
        let count = self.loops.len();
        let too_far = levels == 0 || usize::try_from(levels).is_ok_and(|level| level > count);
        if too_far {
            return Err(LowerError::InvalidLoopLevel { levels, span });
        }
        let target = self.loops[count - levels as usize];
        Ok(if breaking {
            target.break_block
        } else {
            target.continue_block
        })
    }

    // -- conditionals --

    /// Lowers a chain of `if` arms.
    ///
    /// With no join slot this is a statement, and every arm jumps to the join.
    /// With a join slot each arm stores its value there and the join reads it,
    /// which is how a value leaves an `if` without a phi node.
    ///
    /// The blocks are laid out first and opened as the emission reaches them.
    /// An arm's condition is tested in a different block from its body, so the
    /// layout is, in order: the first arm's body, then for each later arm a test
    /// block followed by that arm's body, and finally the join. An arm's false
    /// branch always goes to the block *after* the one it is emitted in, so every
    /// branch target is the next slot or the one after it.
    fn conditional(
        &mut self,
        arms: &[CheckedArm],
        join: Option<(u32, &Type)>,
    ) -> Result<Option<ValueId>, LowerError> {
        // What each block of the layout is: an arm's body, or a test for an arm
        // that is not the first.
        enum Slot {
            Test(usize),
            Body(usize),
            Join,
        }
        let mut layout: Vec<Slot> = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            if index > 0 && arm.condition.is_some() {
                layout.push(Slot::Test(index));
            }
            layout.push(Slot::Body(index));
        }
        layout.push(Slot::Join);

        // Every block is reserved before any of them is filled, so each
        // identifier is the one the builder really gave it. An arm's condition
        // may create blocks of its own — a short-circuiting `&&` makes three — so
        // a number worked out in advance would name the wrong block.
        let mut blocks: Vec<Reserved> = Vec::new();
        for slot in &layout {
            let stem = match slot {
                Slot::Test(_) => "if.test",
                Slot::Body(_) => "if.arm",
                Slot::Join => "if.join",
            };
            blocks.push(self.reserve(stem)?);
        }
        let join_block = blocks[blocks.len() - 1].id;

        // The first arm's condition is tested in the block before the
        // conditional, and its false branch goes to the block after its body.
        if let Some(condition) = &arms[0].condition {
            let test = self.value(condition)?;
            self.close(Terminator::Branch {
                condition: test,
                then_block: blocks[0].id,
                otherwise: blocks[1].id,
            })?;
        }

        for (offset, slot) in layout.iter().enumerate() {
            match slot {
                Slot::Test(arm) => {
                    self.fill(&blocks[offset])?;
                    let condition = arms[*arm].condition.as_ref().expect("a test slot");
                    let test = self.value(condition)?;
                    // The body is the next block, and the false branch skips it.
                    let next = blocks.get(offset + 1).map_or(join_block, |block| block.id);
                    let after = blocks.get(offset + 2).map_or(join_block, |block| block.id);
                    self.close(Terminator::Branch {
                        condition: test,
                        then_block: next,
                        otherwise: after,
                    })?;
                }
                Slot::Body(arm) => {
                    self.fill(&blocks[offset])?;
                    let checked = &arms[*arm];
                    for statement in &checked.statements {
                        self.statement(statement)?;
                    }
                    if let Some(tail) = &checked.tail {
                        match join {
                            Some((offset, ty)) => {
                                let value = self.value(tail)?;
                                self.store_slot(offset, value, ty)?;
                            }
                            None => {
                                self.value(tail)?;
                            }
                        }
                    }
                    self.jump(join_block)?;
                }
                Slot::Join => {
                    self.fill(&blocks[offset])?;
                }
            }
        }

        match join {
            Some((offset, ty)) => Ok(Some(self.load_slot(offset, ty)?)),
            None => Ok(None),
        }
    }

    // -- loops --

    /// The four blocks a loop is made of.
    ///
    /// `test` decides whether the body runs, `body` is the body, `step` is where
    /// `continue` goes, and `exit` is where `break` goes. All four are reserved
    /// before the body is lowered, so a `break` or `continue` inside a body that
    /// creates blocks of its own still names the loop's own step and exit.
    fn loop_layout(
        &mut self,
        test: &str,
        body: &str,
        step: &str,
        exit: &str,
    ) -> Result<LoopBlocks, LowerError> {
        let blocks = self.reserve_run(&[test, body, step, exit])?;
        let mut blocks = blocks.into_iter();
        Ok(LoopBlocks {
            test: blocks.next().ok_or(LowerError::Allocation)?,
            body: blocks.next().ok_or(LowerError::Allocation)?,
            step: blocks.next().ok_or(LowerError::Allocation)?,
            exit: blocks.next().ok_or(LowerError::Allocation)?,
        })
    }

    /// `while condition { body }`.
    fn while_loop(
        &mut self,
        condition: &CheckedExpr,
        body: &CheckedBlock,
    ) -> Result<(), LowerError> {
        let blocks = self.loop_layout("while.test", "while.body", "while.step", "while.exit")?;
        self.jump(blocks.test.id)?;
        self.fill(&blocks.test)?;
        let test = self.value(condition)?;
        self.close(Terminator::Branch {
            condition: test,
            then_block: blocks.body.id,
            otherwise: blocks.exit.id,
        })?;

        self.fill(&blocks.body)?;
        self.loops.push(LoopTargets {
            continue_block: blocks.step.id,
            break_block: blocks.exit.id,
        });
        self.block(body)?;
        self.loops.pop();
        self.jump(blocks.step.id)?;

        self.fill(&blocks.step)?;
        self.jump(blocks.test.id)?;

        self.fill(&blocks.exit)?;
        Ok(())
    }

    /// `for name in start..end { body }`.
    ///
    /// The induction variable is the loop's own local, so the body reads and
    /// writes the slot the frontend allocated. The end value is computed once,
    /// before the loop, and kept in a temporary, so a body that changes the bound
    /// cannot change where the loop stops.
    fn for_loop(
        &mut self,
        slot: u32,
        ty: &Type,
        start: &CheckedExpr,
        end: &CheckedExpr,
        body: &CheckedBlock,
    ) -> Result<(), LowerError> {
        let local = match self.locals.get(&slot) {
            Some(local) => *local,
            None => {
                return Err(LowerError::UnsupportedShape {
                    detail: format!("a loop variable in slot {slot}"),
                    span: self.function.span.clone(),
                });
            }
        };
        let offset = local.offset;

        let first = self.value(start)?;
        self.store_slot(offset, first, ty)?;
        let end_value = self.value(end)?;
        let bound = self.temporary(
            ty.size_in_bytes(WordWidth::W64),
            ty.clone(),
            SlotPurpose::LoopBound,
        )?;
        self.store_slot(bound, end_value, ty)?;

        let blocks = self.loop_layout("for.test", "for.body", "for.step", "for.exit")?;
        self.jump(blocks.test.id)?;
        self.fill(&blocks.test)?;
        let counter = self.load_slot(offset, ty)?;
        let limit = self.load_slot(bound, ty)?;
        let op = if is_signed(ty) {
            ComparisonOp::GreaterThanOrEqualSigned
        } else {
            ComparisonOp::GreaterThanOrEqualUnsigned
        };
        let test = self.emit(Instruction::Compare {
            op,
            left: counter,
            right: limit,
        })?;
        // The comparison asks whether the counter has *reached* the bound, so the
        // body is what happens when it has **not**: the true branch leaves the
        // loop. Getting this backwards runs the body zero times for a non-empty
        // range and never stops for an empty one, and both look like a correct
        // loop in a test that only inspects the shape.
        self.close(Terminator::Branch {
            condition: test,
            then_block: blocks.exit.id,
            otherwise: blocks.body.id,
        })?;

        self.fill(&blocks.body)?;
        self.loops.push(LoopTargets {
            continue_block: blocks.step.id,
            break_block: blocks.exit.id,
        });
        self.block(body)?;
        self.loops.pop();
        self.jump(blocks.step.id)?;

        self.fill(&blocks.step)?;
        // The counter is re-read rather than carried, so a body that changed it
        // is respected, and adding one wraps like every other arithmetic.
        let counter = self.load_slot(offset, ty)?;
        let one = self.constant(1, ty)?;
        let next = self.emit(Instruction::Binary {
            op: BinaryOp::Add,
            left: counter,
            right: one,
            ty: ir_type(ty)?,
        })?;
        self.store_slot(offset, next, ty)?;
        self.jump(blocks.test.id)?;

        self.fill(&blocks.exit)?;
        Ok(())
    }

    /// `loop { body }`.
    fn loop_loop(&mut self, body: &CheckedBlock) -> Result<(), LowerError> {
        let blocks = self.loop_layout("loop.test", "loop.body", "loop.step", "loop.exit")?;
        self.jump(blocks.test.id)?;
        self.fill(&blocks.test)?;
        self.jump(blocks.body.id)?;

        self.fill(&blocks.body)?;
        self.loops.push(LoopTargets {
            continue_block: blocks.body.id,
            break_block: blocks.exit.id,
        });
        self.block(body)?;
        self.loops.pop();
        self.jump(blocks.body.id)?;

        // `loop` has no test, so its step block is the body's own back edge. It
        // is still reserved and still filled, so every loop has the same shape and
        // a `continue` has somewhere real to go.
        self.fill(&blocks.step)?;
        self.jump(blocks.body.id)?;

        self.fill(&blocks.exit)?;
        Ok(())
    }

    // -- expressions --

    /// Lowers an expression to a value.
    fn value(&mut self, expr: &CheckedExpr) -> Result<ValueId, LowerError> {
        match expr {
            CheckedExpr::Integer { value, ty, .. } => self.emit(Instruction::Const {
                // The bit pattern is kept: a value that does not fit in `i64` is
                // a `u64`, and truncating to the bit pattern is what the constant
                // means.
                value: ConstValue::Int(*value as i64),
                ty: ir_type(ty)?,
            }),
            CheckedExpr::Bool { value, .. } => self.emit(Instruction::Const {
                value: ConstValue::Bool(*value),
                ty: IrType::Bool,
            }),
            CheckedExpr::Str { index, ty, .. } => self.string(*index, ty),
            CheckedExpr::Unit { .. } => self.emit(Instruction::Const {
                value: ConstValue::Void,
                ty: IrType::Void,
            }),
            CheckedExpr::Read { place, .. } => self.read_place(place),
            CheckedExpr::AddressOf { place, ty, .. } => {
                if is_view(ty) {
                    // A view's address is the view itself, so borrowing a view
                    // copies its address and its length together. Returning only
                    // the address would be a view of the right bytes and the
                    // wrong length.
                    self.read_place(place)
                } else {
                    self.place_address(place)
                }
            }
            CheckedExpr::Array { .. } | CheckedExpr::ArrayRepeat { .. } => {
                Err(LowerError::UnsupportedShape {
                    detail: String::from("an array as a value: an array is its own storage"),
                    span: expr.span().clone(),
                })
            }
            CheckedExpr::Unary {
                operator,
                operand,
                ty,
                span,
            } => self.unary(operator, operand, ty, span),
            CheckedExpr::Binary {
                operator,
                left,
                right,
                ty,
                span,
            } => self.binary(operator, left, right, ty, span),
            CheckedExpr::Cast {
                operand, from, to, ..
            } => self.cast(operand, from, to),
            CheckedExpr::Call {
                callee,
                is_extern,
                arguments,
                ty,
                span,
            } => self.call(callee, *is_extern, arguments, ty, span.clone()),
            CheckedExpr::Builtin {
                method,
                receiver,
                arguments,
                ty,
                span,
            } => self.builtin(method, receiver, arguments, ty, span.clone()),
            CheckedExpr::If { arms, ty, .. } => {
                let offset = self.temporary(
                    ty.size_in_bytes(WordWidth::W64),
                    ty.clone(),
                    SlotPurpose::JoinValue,
                )?;
                let value = self.conditional(arms, Some((offset, ty)))?;
                value.ok_or_else(|| LowerError::UnsupportedShape {
                    detail: String::from("a value from an `if`"),
                    span: expr.span().clone(),
                })
            }
            CheckedExpr::Block {
                statements, tail, ..
            } => {
                for statement in statements {
                    self.statement(statement)?;
                }
                self.value(tail)
            }
        }
    }

    /// A string literal: the address of its bytes, and how many there are.
    fn string(&mut self, index: u32, ty: &Type) -> Result<ValueId, LowerError> {
        let text = self
            .program
            .strings
            .get(index as usize)
            .ok_or(LowerError::Allocation)?;
        let length = text.text.len();
        let address = self.emit(Instruction::DataAddress {
            name: format!("str{index}"),
            ty: IrType::Pointer,
        })?;
        let length = self.constant(i64::try_from(length).unwrap_or(i64::MAX), &Type::Usize)?;
        self.build_view(address, length, ty)
    }

    /// The frame address of a builtin argument that the frontend required to be a
    /// writable place.
    ///
    /// A builtin that writes through an argument cannot use `value`, which *reads*
    /// the place; it needs to know where the place lives. The frontend has already
    /// checked that the argument is a place and is writable, so this is a lookup
    /// rather than a judgement — an argument that is not a place here is a frontend
    /// bug, and it is reported as an unsupported shape rather than silently writing
    /// somewhere else.
    fn place_address_of_argument(&mut self, argument: &CheckedExpr) -> Result<ValueId, LowerError> {
        match argument {
            CheckedExpr::Read { place, .. } => self.place_address(place),
            other => Err(LowerError::UnsupportedShape {
                detail: String::from("a builtin argument that is written through is not a place"),
                span: other.span().clone(),
            }),
        }
    }

    /// A unary operator.
    fn unary(
        &mut self,
        operator: &str,
        operand: &CheckedExpr,
        ty: &Type,
        span: &SourceSpan,
    ) -> Result<ValueId, LowerError> {
        match operator {
            "-" => {
                let value = self.value(operand)?;
                self.emit(Instruction::Unary {
                    op: UnaryOp::Negate,
                    operand: value,
                    ty: ir_type(ty)?,
                })
            }
            "!" => {
                // Logical negation, not a conversion: the operand is already a
                // `bool`, and asking whether it is zero would hand back the
                // operand unchanged.
                let value = self.value(operand)?;
                self.emit(Instruction::Unary {
                    op: UnaryOp::Not,
                    operand: value,
                    ty: IrType::Bool,
                })
            }
            "*" => {
                // Reading through a reference is a place, not an operation on a
                // value, so it is lowered as a read of the place the reference
                // points at.
                let place = CheckedPlace::Deref {
                    reference: Box::new(operand.clone()),
                    ty: ty.clone(),
                    mutable: false,
                    span: span.clone(),
                };
                self.read_place(&place)
            }
            _ => Err(LowerError::UnknownOperator {
                operator: String::from(operator),
                span: span.clone(),
            }),
        }
    }

    /// A binary operator.
    fn binary(
        &mut self,
        operator: &str,
        left: &CheckedExpr,
        right: &CheckedExpr,
        ty: &Type,
        span: &SourceSpan,
    ) -> Result<ValueId, LowerError> {
        let left_ty = left.ty();
        if operator == "&&" || operator == "||" {
            return self.short_circuit(operator, left, right);
        }
        let left_value = self.value(left)?;
        let right_value = self.value(right)?;
        let signed = is_signed(&left_ty);
        if let Some(op) = comparison(operator, signed) {
            return self.emit(Instruction::Compare {
                op,
                left: left_value,
                right: right_value,
            });
        }
        if let Some(op) = arithmetic(operator, signed) {
            return self.emit(Instruction::Binary {
                op,
                left: left_value,
                right: right_value,
                ty: ir_type(ty)?,
            });
        }
        Err(LowerError::UnknownOperator {
            operator: String::from(operator),
            span: span.clone(),
        })
    }

    /// `a && b` or `a || b`, which do not always evaluate the right side.
    ///
    /// The right side is put in its own block, so it runs only when the left side
    /// does not decide the answer. The result comes out of a one-byte frame
    /// temporary, because the IR has no value that two blocks can both define.
    fn short_circuit(
        &mut self,
        operator: &str,
        left: &CheckedExpr,
        right: &CheckedExpr,
    ) -> Result<ValueId, LowerError> {
        let slot = match self.short_circuit {
            Some(slot) => slot,
            None => {
                let slot = self.temporary(1, Type::Bool, SlotPurpose::ShortCircuit)?;
                self.short_circuit = Some(slot);
                slot
            }
        };
        let left_value = self.value(left)?;
        // Three blocks: the right side, the left side's own answer, and the join
        // that reads the result out of the frame. They are reserved before the
        // first branch, so the branch names the blocks themselves.
        let right_block = self.reserve("sc.right")?;
        let other = self.reserve("sc.other")?;
        let join = self.reserve("sc.join")?;
        self.close(Terminator::Branch {
            condition: left_value,
            then_block: right_block.id,
            otherwise: other.id,
        })?;

        self.fill(&right_block)?;
        // Reaching the right side means the left did not decide the answer, so
        // the answer is the right side's value: its own for `&&`, and true for
        // `||`, because the right side of `||` only runs when the left was false.
        let value = if operator == "&&" {
            self.value(right)?
        } else {
            self.constant(1, &Type::Bool)?
        };
        self.store_slot(slot, value, &Type::Bool)?;
        self.jump(join.id)?;

        self.fill(&other)?;
        self.store_slot(slot, left_value, &Type::Bool)?;
        self.jump(join.id)?;

        self.fill(&join)?;
        self.load_slot(slot, &Type::Bool)
    }

    /// An integer cast.
    ///
    /// A cast is one load out of the frame: widening reads the source's width and
    /// lets the load extend it into the wider type, which is what the machine's
    /// `LDZ` and `LDS` do, and narrowing reads the target's width, which keeps
    /// the low bits and drops the rest. The extension follows the *source*
    /// type, so a `u8` of 200 becomes 200 and an `i8` of -1 becomes -1.
    ///
    /// The value is stored at its own width first, and the scratch slot is reused
    /// because each store is immediately followed by its own load.
    fn cast(
        &mut self,
        operand: &CheckedExpr,
        from: &Type,
        to: &Type,
    ) -> Result<ValueId, LowerError> {
        if ir_type(from)? == ir_type(to)? {
            return self.value(operand);
        }
        let (_, source_width, _) =
            scalar_width(from).ok_or_else(|| LowerError::UnsupportedShape {
                detail: format!("a cast from `{}`", ty_name(from)),
                span: operand.span().clone(),
            })?;
        let (target_size, target_width, _) =
            scalar_width(to).ok_or_else(|| LowerError::UnsupportedShape {
                detail: format!("a cast to `{}`", ty_name(to)),
                span: operand.span().clone(),
            })?;
        // A target wider than a word cannot be produced by a single load, so the
        // conversion is reported rather than approximated.
        if target_size > 8 {
            return Err(LowerError::UnsupportedShape {
                detail: format!("a cast to `{}`", ty_name(to)),
                span: operand.span().clone(),
            });
        }
        let slot = match self.cast_scratch {
            Some(slot) => slot,
            None => {
                let slot = self.temporary(8, Type::U64, SlotPurpose::CastScratch)?;
                self.cast_scratch = Some(slot);
                slot
            }
        };
        let value = self.value(operand)?;
        self.store_slot(slot, value, from)?;
        let address = self.frame_address(slot)?;
        // The load reads back what the store just wrote, so it can never be
        // wider than the source: the store put `source_width`'s bytes in the
        // scratch and the bytes above them were never written. A load narrower
        // than its type is the extension the cast asks for, so the narrower of
        // the two is what carries the value across without inventing bytes.
        let width = if target_width.bytes() <= source_width.bytes() {
            target_width
        } else {
            source_width
        };
        self.emit(Instruction::Load {
            address,
            width,
            space: MemorySpace::Program,
            ty: ir_type(to)?,
        })
    }

    /// A call to a Lazen function or to an extern.
    fn call(
        &mut self,
        callee: &str,
        is_extern: bool,
        arguments: &[CheckedExpr],
        ty: &Type,
        span: SourceSpan,
    ) -> Result<ValueId, LowerError> {
        let mut words = 0usize;
        let mut args = Vec::new();
        for argument in arguments {
            words = words.saturating_add(argument_words(&argument.ty()));
            args.push(CallArg::Value(self.value(argument)?));
        }
        if words > MAX_ARGUMENT_WORDS {
            return Err(LowerError::TooManyArgumentWords {
                callee: String::from(callee),
                needed: words,
                allowed: MAX_ARGUMENT_WORDS,
                span: span.clone(),
            });
        }
        let target = if is_extern {
            let declaration = self
                .program
                .externs
                .iter()
                .find(|candidate| candidate.name == callee)
                .ok_or_else(|| LowerError::UnsupportedShape {
                    detail: format!("a call to the undeclared extern `{callee}`"),
                    span: span.clone(),
                })?;
            if declaration.syscall.is_none() {
                // The graphics and input designs name calls the ABI has not
                // numbered. Inventing a number would make a program that calls
                // the wrong thing, so this is refused here.
                return Err(LowerError::UnnumberedSyscall {
                    name: String::from(callee),
                    span: span.clone(),
                });
            }
            CallTarget::Syscall(Name::from(callee))
        } else {
            CallTarget::Function(Name::from(callee))
        };
        self.emit(Instruction::Call {
            target,
            args,
            result: ir_type(ty)?,
        })
    }

    /// One of the builtin methods.
    fn builtin(
        &mut self,
        method: &str,
        receiver: &CheckedExpr,
        arguments: &[CheckedExpr],
        ty: &Type,
        span: SourceSpan,
    ) -> Result<ValueId, LowerError> {
        match method {
            "len" => match &receiver.ty().clone() {
                Type::Array { length, .. } => {
                    self.constant(i64::try_from(*length).unwrap_or(i64::MAX), &Type::Usize)
                }
                _ => {
                    let view = self.value(receiver)?;
                    let (_, length) = self.view_parts(view)?;
                    Ok(length)
                }
            },
            "as_ptr" => match array_place(receiver) {
                // An array is already at its own address.
                Some(place) => self.place_address(&place),
                None => {
                    let view = self.value(receiver)?;
                    self.emit(Instruction::Extract {
                        aggregate: view,
                        offset: 0,
                        ty: IrType::Pointer,
                    })
                }
            },
            "as_bytes" | "as_slice" | "as_mut_slice" => match array_place(receiver) {
                // An array becomes a view of itself: its address and its element
                // count. This is the one place an array's data is named rather
                // than copied, because a view is a reference to it.
                Some(place) => {
                    let address = self.place_address(&place)?;
                    let count = match place.ty() {
                        Type::Array { length, .. } => *length,
                        _ => 0,
                    };
                    let length =
                        self.constant(i64::try_from(count).unwrap_or(i64::MAX), &Type::Usize)?;
                    self.build_view(address, length, ty)
                }
                None => {
                    let view = self.value(receiver)?;
                    if ir_type(&receiver.ty())? == ir_type(ty)? {
                        // Same type: the view already is the answer, length and
                        // all.
                        Ok(view)
                    } else {
                        let (address, length) = self.view_parts(view)?;
                        self.build_view(address, length, ty)
                    }
                }
            },
            // A view over memory named by an address. The receiver *is* the
            // address, so the view is built from it and the stated length, and
            // that is the whole operation: no load happens here, and the bounds
            // check on every later index is what makes the stated length a
            // promise rather than a licence.
            "slice_from_raw" | "slice_from_raw_mut" => {
                let address = self.value(receiver)?;
                let length_argument =
                    arguments
                        .first()
                        .ok_or_else(|| LowerError::UnknownBuiltin {
                            method: String::from(method),
                            span: span.clone(),
                        })?;
                let length = self.value(length_argument)?;
                self.build_view(address, length, ty)
            }
            // A `str` over the bytes of a slice, if the bytes are valid UTF-8.
            //
            // The check itself is a call, not a cast and not a compiler loop:
            // `str` is a *checked* UTF-8 byte string, string literals are checked
            // by the compiler, and this is the only other way one can exist. The
            // validator lives in the runtime because a Lazen program cannot reach
            // an address as a `str` — `ptr<T>` is deliberately not
            // dereferenceable — so a program could not write this check itself.
            //
            // The status out-parameter is how the failure is reported. A Lazen
            // function has exactly one result and no `optional`, so returning
            // "the text or a failure" needs somewhere to put the failure, and the
            // ABI's own out-parameter convention is what this language has.
            "as_str" => {
                let bytes = self.value(receiver)?;
                let status_address = self.place_address_of_argument(&arguments[0])?;
                // The validator's answer. It is a `bool` and the result of this
                // builtin is a `str`, so the view is built separately and the
                // answer only decides what is written to the status slot.
                let valid = self.emit(Instruction::Call {
                    target: CallTarget::Function(Name::from("rt::utf8::valid")),
                    args: alloc::vec![CallArg::Value(bytes)],
                    result: IrType::Bool,
                })?;
                // A `bool` is one byte, so the status the caller reads back is
                // stored at the width the caller will load it at. The store is an
                // *effect* — it produces no value — so the `str` returned is the
                // view built below, not the store's.
                self.effect(Instruction::Store {
                    address: status_address,
                    value: valid,
                    width: StoreWidth::Byte,
                    space: MemorySpace::Program,
                })?;
                // The result is the same bytes the receiver was: a `str` over a
                // `&[u8]` is the same pointer and length with a different element
                // type, so the view is rebuilt from the receiver's two words rather
                // than copied. A caller that got `false` has no valid `str` to use,
                // which is exactly what the status says.
                let (address, length) = self.view_parts(bytes)?;
                self.build_view(address, length, ty)
            }
            _ => Err(LowerError::UnknownBuiltin {
                method: String::from(method),
                span,
            }),
        }
    }
}

/// A comparison's IR operator.
fn comparison(operator: &str, signed: bool) -> Option<ComparisonOp> {
    Some(match operator {
        "==" => ComparisonOp::Equal,
        "!=" => ComparisonOp::NotEqual,
        "<" if signed => ComparisonOp::LessThanSigned,
        "<" => ComparisonOp::LessThanUnsigned,
        "<=" if signed => ComparisonOp::LessThanOrEqualSigned,
        "<=" => ComparisonOp::LessThanOrEqualUnsigned,
        ">" if signed => ComparisonOp::GreaterThanSigned,
        ">" => ComparisonOp::GreaterThanUnsigned,
        ">=" if signed => ComparisonOp::GreaterThanOrEqualSigned,
        ">=" => ComparisonOp::GreaterThanOrEqualUnsigned,
        _ => return None,
    })
}

/// An arithmetic operator's IR operator.
fn arithmetic(operator: &str, signed: bool) -> Option<BinaryOp> {
    Some(match operator {
        "+" => BinaryOp::Add,
        "-" => BinaryOp::Sub,
        "*" => BinaryOp::Mul,
        "/" if signed => BinaryOp::DivSigned,
        "/" => BinaryOp::DivUnsigned,
        "%" if signed => BinaryOp::RemainderSigned,
        "%" => BinaryOp::RemainderUnsigned,
        "&" => BinaryOp::BitAnd,
        "|" => BinaryOp::BitOr,
        "^" => BinaryOp::BitXor,
        _ => return None,
    })
}

/// The place behind an expression, when it names one directly.
///
/// `values.as_slice()` needs an array's address, and an array has no value, so
/// the receiver has to be a place. A cast or a block around it is peeled away
/// first, because those still name the same place.
fn array_place(expr: &CheckedExpr) -> Option<CheckedPlace> {
    match expr {
        CheckedExpr::Read { place, .. } if is_array(place.ty()) => Some((**place).clone()),
        CheckedExpr::Cast { operand, .. } => array_place(operand),
        CheckedExpr::Block { tail, .. } => array_place(tail),
        _ => None,
    }
}

/// The four blocks a loop is made of.
///
/// `test` decides whether the body runs, `body` is the body, `step` is where
/// `continue` goes, and `exit` is where `break` goes. All four exist before the
/// body is lowered, because the body can jump to the step and exit blocks and
/// they do not exist yet when the body's first block is opened.
#[derive(Clone, Debug)]
struct LoopBlocks {
    test: Reserved,
    body: Reserved,
    step: Reserved,
    exit: Reserved,
}
