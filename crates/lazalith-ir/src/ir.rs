//! The IR data model.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::fmt;

/// Identifies a value produced inside one function.
///
/// Value identifiers are dense and function-local: a verifier can walk a
/// function's instructions in order and know exactly which identifiers exist.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ValueId(u32);

impl ValueId {
    /// Creates a value identifier, rejecting identifiers above `u32::MAX`.
    pub const fn new(raw: u32) -> Option<Self> {
        match raw {
            raw if raw < u32::MAX => Some(Self(raw)),
            _ => None,
        }
    }

    /// The raw identifier, for diagnostics and round-tripping.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for ValueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A basic block label.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BlockId(u32);

impl BlockId {
    /// Creates a block label, rejecting labels above `u32::MAX`.
    pub const fn new(raw: u32) -> Option<Self> {
        match raw {
            raw if raw < u32::MAX => Some(Self(raw)),
            _ => None,
        }
    }

    /// The raw label, for diagnostics and round-tripping.
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b{}", self.0)
    }
}

/// A name in the IR. Names exist for symbols and diagnostics; the verifier
/// never requires a name to be present, because IR may be built
/// programmatically.
pub type Name = String;

/// A function's linkage, mirroring the object format's symbol bindings so a
/// code generator can forward it without inventing a new notion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Linkage {
    /// Visible only inside the defining module.
    Local,
    /// Visible to the linker, may collide with another global of the same name.
    Global,
    /// Visible to the linker and guaranteed unique.
    External,
}

/// The Lazalith IR type lattice.
///
/// The set is closed on purpose: a frontend that needs a type outside this list
/// must lower it, because code generation must be able to compute a size and an
/// alignment for every value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    /// No value.
    Void,
    /// A single bit, stored in one byte.
    Bool,
    /// An integer of the given bit width, signed or unsigned.
    Int {
        /// Bit width: 8, 16, 32, or 64.
        bits: u16,
        /// Whether the value is interpreted as signed.
        signed: bool,
    },
    /// An address. The IR never records what it points at, because Lazalith v1
    /// has no MMU and a pointer is an address.
    Pointer,
    /// A two-word view: an address and a length in elements.
    Slice {
        /// Element type, used for size and alignment.
        element: Box<Type>,
        /// Whether the view permits writes through it.
        mutable: bool,
    },
    /// A fixed-size aggregate with named, ordered fields.
    Record {
        /// Field names in declaration order.
        fields: Vec<RecordField>,
    },
    /// A tag plus a payload area sized for the widest variant.
    Enum {
        /// Variant names in tag order.
        variants: Vec<Name>,
    },
    /// A function type. Function values do not exist in v1; the type exists so
    /// an `extern` declaration can be typed and checked.
    Function {
        /// Parameter types.
        params: Vec<Type>,
        /// Result type.
        result: Box<Type>,
    },
}

/// One field of a record type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordField {
    /// Field name.
    pub name: Name,
    /// Field type.
    pub ty: Type,
}

impl Type {
    /// The size of a value of this type in bytes, or `None` for `Void`.
    pub fn size_in_bytes(&self) -> Option<u32> {
        match self {
            Self::Void => None,
            Self::Bool => Some(1),
            Self::Int { bits, .. } => Some(u32::from(*bits / 8)),
            Self::Pointer => Some(8),
            Self::Slice { .. } => Some(16),
            Self::Record { fields } => {
                let mut total = 0u32;
                for field in fields {
                    total = total.checked_add(field.ty.size_in_bytes()?)?;
                }
                Some(total)
            }
            Self::Enum { variants } => {
                let tag = 8u32;
                let payload = u32::try_from(variants.len())
                    .unwrap_or(u32::MAX)
                    .saturating_mul(8);
                tag.checked_add(payload)
            }
            Self::Function { .. } => Some(8),
        }
    }

    /// The alignment of a value of this type in bytes.
    pub fn alignment_in_bytes(&self) -> u32 {
        match self {
            Self::Void | Self::Bool => 1,
            Self::Int { bits, .. } => u32::from(bits / 8).max(1),
            Self::Pointer | Self::Function { .. } => 8,
            Self::Slice { element, .. } => element.alignment_in_bytes().max(8),
            Self::Record { fields } => fields
                .iter()
                .map(|field| field.ty.alignment_in_bytes())
                .max()
                .unwrap_or(1),
            Self::Enum { .. } => 8,
        }
    }

    /// Whether values of this type are copied by value without an address.
    pub fn is_aggregate(&self) -> bool {
        matches!(
            self,
            Self::Slice { .. } | Self::Record { .. } | Self::Enum { .. }
        )
    }
}

/// A literal value used by a `Const` instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConstValue {
    /// A `bool`.
    Bool(bool),
    /// An integer, already widened to the full 64-bit register domain. The
    /// value's declared type in the instruction carries the interpretation.
    Int(i64),
    /// An address.
    Pointer(u64),
    /// The unit value.
    Void,
}

/// Integer binary operations. Division and remainder are kept separate because
/// the ISA has separate signed and unsigned forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// Addition.
    Add,
    /// Subtraction.
    Sub,
    /// Multiplication.
    Mul,
    /// Unsigned division.
    DivUnsigned,
    /// Signed division.
    DivSigned,
    /// Unsigned remainder.
    RemainderUnsigned,
    /// Signed remainder.
    RemainderSigned,
    /// Bitwise and.
    BitAnd,
    /// Bitwise or.
    BitOr,
    /// Bitwise exclusive or.
    BitXor,
    /// Shift left.
    ShiftLeft,
    /// Logical shift right.
    ShiftRightLogical,
    /// Arithmetic shift right.
    ShiftRightArithmetic,
}

/// Integer unary operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    /// Arithmetic negation.
    Negate,
    /// Bitwise complement.
    BitNot,
    /// Convert a `bool` to an integer: 0 or 1.
    BoolToInt,
    /// Convert an integer to a `bool`: zero is false.
    IntToBool,
}

/// Comparison operations. A comparison always produces `Bool`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonOp {
    /// Equal.
    Equal,
    /// Not equal.
    NotEqual,
    /// Unsigned less than.
    LessThanUnsigned,
    /// Unsigned less than or equal.
    LessThanOrEqualUnsigned,
    /// Unsigned greater than.
    GreaterThanUnsigned,
    /// Unsigned greater than or equal.
    GreaterThanOrEqualUnsigned,
    /// Signed less than.
    LessThanSigned,
    /// Signed less than or equal.
    LessThanOrEqualSigned,
    /// Signed greater than.
    GreaterThanSigned,
    /// Signed greater than or equal.
    GreaterThanOrEqualSigned,
}

/// Load width, in bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadWidth {
    /// One byte, zero extended.
    Byte,
    /// One byte, sign extended.
    ByteSigned,
    /// Two bytes, zero extended.
    Half,
    /// Two bytes, sign extended.
    HalfSigned,
    /// Four bytes, zero extended.
    Word,
    /// Four bytes, sign extended.
    WordSigned,
    /// Eight bytes.
    Double,
}

impl LoadWidth {
    /// The number of bytes this load reads.
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte | Self::ByteSigned => 1,
            Self::Half | Self::HalfSigned => 2,
            Self::Word | Self::WordSigned => 4,
            Self::Double => 8,
        }
    }

    /// Whether the loaded value is sign extended.
    pub const fn is_signed(self) -> bool {
        matches!(self, Self::ByteSigned | Self::HalfSigned | Self::WordSigned)
    }
}

/// Store width, in bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreWidth {
    /// One byte.
    Byte,
    /// Two bytes.
    Half,
    /// Four bytes.
    Word,
    /// Eight bytes.
    Double,
}

impl StoreWidth {
    /// The number of bytes this store writes.
    pub const fn bytes(self) -> u32 {
        match self {
            Self::Byte => 1,
            Self::Half => 2,
            Self::Word => 4,
            Self::Double => 8,
        }
    }
}

/// Which address space a memory operation refers to.
///
/// Lazalith has no MMU, so this is not an isolation boundary. It exists so a
/// frontend can state intent, and so the debugger and future drivers can tell a
/// program's own memory from a device region.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemorySpace {
    /// The program's own address space.
    Program,
    /// A device or platform region the program was granted.
    Platform,
}

/// An argument to a call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallArg {
    /// Pass a computed value.
    Value(ValueId),
    /// Pass an immediate, for `extern` declarations whose ABI takes literals.
    Immediate(ConstValue),
}

/// How a call reaches its target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallTarget {
    /// Call the function with this IR name in the same module.
    Function(Name),
    /// Call a function in another module, by name.
    Imported(Name),
    /// Call a declared OS ABI function. The name is the ABI-level name; the
    /// compiler's ABI table maps it to the shared syscall definition, so no
    /// syscall number is written here.
    Syscall(Name),
}

/// A fixed operation the backend must implement, as opposed to a call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intrinsic {
    /// The address of a function, for taking a function's identity.
    FunctionAddress,
    /// The number of elements in a slice.
    SliceLength,
    /// A bounds check that traps instead of branching. A backend that cannot
    /// emit a trap must reject the IR rather than drop the check.
    BoundsCheck,
}

/// One instruction. Every instruction except `Store` produces a value; the
/// producing value is the instruction's implicit result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Instruction {
    /// A literal.
    Const {
        /// The literal.
        value: ConstValue,
        /// The declared type of the result.
        ty: Type,
    },
    /// Integer binary operation.
    Binary {
        /// The operation.
        op: BinaryOp,
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
        /// Result type.
        ty: Type,
    },
    /// Integer unary operation.
    Unary {
        /// The operation.
        op: UnaryOp,
        /// The operand.
        operand: ValueId,
        /// Result type.
        ty: Type,
    },
    /// Comparison, producing `Bool`.
    Compare {
        /// The operation.
        op: ComparisonOp,
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
    /// Logical and, short-circuiting.
    LogicalAnd {
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
    /// Logical or, short-circuiting.
    LogicalOr {
        /// Left operand.
        left: ValueId,
        /// Right operand.
        right: ValueId,
    },
    /// A load from memory.
    Load {
        /// Address.
        address: ValueId,
        /// Width and extension.
        width: LoadWidth,
        /// Address space.
        space: MemorySpace,
        /// Result type.
        ty: Type,
    },
    /// A store to memory. Produces no value.
    Store {
        /// Address.
        address: ValueId,
        /// Value to write.
        value: ValueId,
        /// Width.
        width: StoreWidth,
        /// Address space.
        space: MemorySpace,
    },
    /// A call.
    Call {
        /// Where to call.
        target: CallTarget,
        /// Arguments in declaration order.
        args: Vec<CallArg>,
        /// Result type, `Void` for no result.
        result: Type,
    },
    /// A backend operation with no arguments beyond the operands shown.
    Intrinsic {
        /// Which operation.
        kind: Intrinsic,
        /// First operand, if any.
        operand: Option<ValueId>,
        /// Result type.
        result: Type,
    },
    /// Copy a value. Lowering uses it to make aggregates explicit, which keeps
    /// code generation free of implicit move rules.
    Copy {
        /// The value copied.
        value: ValueId,
    },
    /// Extract a field or variant payload from an aggregate at a constant byte
    /// offset. Frontends lower field access and enum payload access here so the
    /// backend never needs a type layout of its own.
    Extract {
        /// The aggregate.
        aggregate: ValueId,
        /// Byte offset into the aggregate.
        offset: u32,
        /// Type of the extracted part.
        ty: Type,
    },
    /// Build an aggregate by writing a field or variant payload at a constant
    /// byte offset. The aggregate operand supplies the initial value, normally a
    /// zero constant, so the result is fully determined.
    Insert {
        /// The aggregate to modify.
        aggregate: ValueId,
        /// Byte offset within the aggregate.
        offset: u32,
        /// The value written.
        value: ValueId,
        /// Type of the resulting aggregate.
        result: Type,
    },
    /// A software trap with a code. This is how Lazen reports an out-of-bounds
    /// access: the program is faulted by the kernel, not panicked.
    Trap {
        /// Stable trap code.
        code: u32,
    },
}

/// A return payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReturnValue {
    /// Return nothing.
    Void,
    /// Return one value.
    Value(ValueId),
}

/// How a block ends. Exactly one terminator per block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Terminator {
    /// Unconditional branch.
    Jump(BlockId),
    /// Conditional branch: when `condition` is true go to `then`, otherwise
    /// `otherwise`.
    Branch {
        /// The condition.
        condition: ValueId,
        /// Taken when the condition is true.
        then_block: BlockId,
        /// Taken when the condition is false.
        otherwise: BlockId,
    },
    /// Return from the function.
    Return(ReturnValue),
    /// Unreachable, for a path no control flow can reach. Required as an
    /// explicit terminator so a verifier can prove a function is total.
    Unreachable,
}

/// A basic block: a label, straight-line instructions, and a terminator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// The block label.
    pub id: BlockId,
    /// The block's name, for diagnostics and readable IR text.
    pub name: Name,
    /// Instructions in execution order.
    pub instructions: Vec<Instruction>,
    /// How the block ends.
    pub terminator: Terminator,
}

/// A parameter of a function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    /// Parameter name.
    pub name: Name,
    /// Parameter type.
    pub ty: Type,
}

/// An IR function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// Function name.
    pub name: Name,
    /// Linkage.
    pub linkage: Linkage,
    /// Parameter types in order.
    pub params: Vec<Parameter>,
    /// Result type.
    pub result: Type,
    /// Blocks; the first is the entry block.
    pub blocks: Vec<Block>,
    /// Where the function came from in the source, when known.
    pub span: Option<lazalith_types::SourceSpan>,
}

impl Function {
    /// The entry block, or `None` for a function with no blocks.
    pub fn entry(&self) -> Option<&Block> {
        self.blocks.first()
    }

    /// Looks up a block by label.
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.iter().find(|block| block.id == id)
    }
}

/// A read-only or zero-initialized data segment, emitted as `.rodata` and
/// `.bss` respectively.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataSegment {
    /// Segment name, which becomes the section symbol's name.
    pub name: Name,
    /// Byte contents. Empty means a zero-initialized `.bss` segment.
    pub bytes: Vec<u8>,
    /// Required alignment in bytes.
    pub alignment: u32,
    /// Where the segment came from in the source, when known.
    pub span: Option<lazalith_types::SourceSpan>,
}

/// An IR module: one compilation unit's worth of functions and data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Module {
    /// Module name, used as a symbol prefix.
    pub name: Name,
    /// Functions in declaration order.
    pub functions: Vec<Function>,
    /// Data segments in emission order.
    pub data: Vec<DataSegment>,
}

impl Module {
    /// Looks up a function by name.
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|function| function.name == name)
    }
}

/// Alias kept for readability at call sites that build modules.
pub type IrModule = Module;

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Void => f.write_str("void"),
            Self::Bool => f.write_str("bool"),
            Self::Int { bits, signed } => write!(f, "{}{bits}", if *signed { "i" } else { "u" }),
            Self::Pointer => f.write_str("ptr"),
            Self::Slice { element, mutable } => {
                write!(f, "&{}", if *mutable { "mut " } else { "" })?;
                write!(f, "[{element}]")
            }
            Self::Record { fields } => {
                f.write_str("struct {")?;
                for (index, field) in fields.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{}: {field:?}", field.name)?;
                }
                f.write_str("}")
            }
            Self::Enum { variants } => {
                f.write_str("enum {")?;
                for (index, variant) in variants.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(variant)?;
                }
                f.write_str("}")
            }
            Self::Function { params, result } => {
                f.write_str("fn(")?;
                for (index, parameter) in params.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{parameter}")?;
                }
                write!(f, ") -> {result}")
            }
        }
    }
}

impl fmt::Display for RecordField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.ty)
    }
}
