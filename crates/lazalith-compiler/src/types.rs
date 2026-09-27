//! Type checking and semantic analysis for Lazen v1.
//!
//! This stage owns every typing rule. It works on the closed v1 type set and
//! nothing else: `bool`, `i8`–`i64`, `u8`–`u64`, `usize`, `str`, `ptr<T>`,
//! `&[T]`, `&mut [T]`, and `[T; N]`. `optional`, enums, records, and `match` do
//! not exist here, so a program that needs them is rejected rather than
//! approximated.
//!
//! It emits a [`CheckedProgram`]: a typed tree in which every local has a frame
//! slot and a byte offset, every expression has a type, every `break` and
//! `continue` names its loop, and every string literal is interned. It contains
//! **no** instructions, no labels, no IR nodes, and no code generation. The
//! frame *offsets* are data the next stage needs; the frame *pointer* is the
//! machine's own stack pointer, and nothing here fabricates one.
//!
//! Inference is deliberately narrow. An integer literal takes its type from
//! context, or `i32` when it has none. Nothing else is inferred, and no
//! conversion is implicit: every conversion in a Lazen program is an explicit
//! `as` cast, which is why the arithmetic in a program that mixes widths is
//! also explicit.

use alloc::{
    boxed::Box,
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Help, Label, Note, Severity};
use lazalith_os_abi::Syscall;
use lazalith_types::{SourceId, SourceManager, SourceSpan, WordWidth};

use crate::ast::{ArithOp, BinaryOp, Block, CompareOp, Expr, IfArm, Stmt, TypeExpr, UnaryOp};
use crate::diagnostic::StageError;
use crate::lexer::IntSuffix;
use crate::resolve::{self, Resolved, ResolvedConstant, ResolvedExtern, ResolvedFunction, Symbol};

/// The type checker's diagnostic codes.
pub mod codes {
    /// A type did not match the type required here.
    pub const MISMATCH: &str = "T0001";
    /// A name is not defined.
    pub const UNRESOLVED_NAME: &str = "T0002";
    /// A name was called but is not a function.
    pub const NOT_CALLABLE: &str = "T0003";
    /// A call passed the wrong number of arguments.
    pub const ARITY: &str = "T0004";
    /// An argument's type did not match the parameter's.
    pub const ARGUMENT: &str = "T0005";
    /// An assignment to a binding that is not `mut`.
    pub const IMMUTABLE: &str = "T0006";
    /// An assignment to something that is not a place.
    pub const NOT_A_PLACE: &str = "T0007";
    /// `break` or `continue` outside a loop.
    pub const OUTSIDE_LOOP: &str = "T0008";
    /// A `return` whose value does not match the result type.
    pub const RETURN_TYPE: &str = "T0009";
    /// A dereference of a raw pointer.
    pub const RAW_DEREF: &str = "T0010";
    /// A cast that Lazen v1 does not permit.
    pub const INVALID_CAST: &str = "T0011";
    /// A condition that is not a `bool`.
    pub const CONDITION: &str = "T0012";
    /// An index that is not a `usize`.
    pub const INDEX_TYPE: &str = "T0013";
    /// A method that does not exist on the receiver's type.
    pub const UNKNOWN_METHOD: &str = "T0014";
    /// A statement whose value is discarded and that is not a call.
    pub const DISCARDED_VALUE: &str = "T0015";
    /// A `for` over something that is not a range.
    pub const NOT_A_RANGE: &str = "T0016";
    /// An integer literal that does not fit its type.
    pub const LITERAL_RANGE: &str = "T0017";
    /// An `extern` declaration that does not match the OS ABI.
    pub const ABI_MISMATCH: &str = "T0018";
    /// A `const` whose value is not a constant, or whose type is not concrete.
    pub const NOT_CONSTANT: &str = "T0019";
    /// A construct that is deliberately not in v1.
    pub const UNSUPPORTED: &str = "T0020";
    /// A function that cannot produce its declared result.
    pub const MISSING_RESULT: &str = "T0021";
    /// A `&` or `&mut` of something that cannot be borrowed that way.
    pub const BAD_BORROW: &str = "T0022";
    /// A duplicate or misplaced name that survived resolution.
    pub const SHADOWING: &str = "T0023";
    /// An array length that does not fit the address space.
    pub const ARRAY_TOO_LARGE: &str = "T0024";
    /// A name that is not usable where it was written.
    pub const NOT_A_VALUE: &str = "T0025";
    /// An array literal with nothing in it.
    pub const EXPECTED_ARRAY: &str = "T0026";
    /// A conditional used as a value that binds a name in one of its arms.
    ///
    /// Step 61 records a value conditional's arms without a frame of their own, so
    /// a name bound inside an arm would have no slot to live in. Rather than invent
    /// one, the construct is rejected; Step 62 allocates slots when it lowers the
    /// arms into the enclosing frame and can lift this restriction then.
    pub const ARM_BINDING: &str = "T0027";
}

/// A Lazen v1 type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Type {
    /// The result of a function that returns nothing.
    Unit,
    /// `bool`
    Bool,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `usize`
    Usize,
    /// `str`: a checked byte string, a pointer and a length.
    Str,
    /// `&[T]` or `&mut [T]`: a view, a pointer and a length.
    Slice {
        /// The element type.
        element: Box<Type>,
        /// Whether the view may be written through.
        mutable: bool,
    },
    /// `[T; N]`: a fixed-length array held by value.
    Array {
        /// The element type.
        element: Box<Type>,
        /// The element count.
        length: u64,
    },
    /// `ptr<T>`: an address for the OS ABI, with no pointee metadata.
    Pointer {
        /// The pointee type.
        pointee: Box<Type>,
    },
    /// `&T` or `&mut T`: a reference to a whole local, one word.
    Reference {
        /// The referent type.
        pointee: Box<Type>,
        /// Whether the referent may be written.
        mutable: bool,
    },
}

impl Type {
    /// Whether this is an integer type, of any width.
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::I8
                | Type::I16
                | Type::I32
                | Type::I64
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
                | Type::Usize
        )
    }

    /// Whether this is a signed integer type.
    pub fn is_signed(&self) -> bool {
        matches!(self, Type::I8 | Type::I16 | Type::I32 | Type::I64)
    }

    /// The integer type of a literal suffix.
    pub fn from_suffix(suffix: IntSuffix) -> Type {
        match suffix {
            IntSuffix::I8 => Type::I8,
            IntSuffix::I16 => Type::I16,
            IntSuffix::I32 => Type::I32,
            IntSuffix::I64 => Type::I64,
            IntSuffix::U8 => Type::U8,
            IntSuffix::U16 => Type::U16,
            IntSuffix::U32 => Type::U32,
            IntSuffix::U64 => Type::U64,
            IntSuffix::Usize => Type::Usize,
        }
    }

    /// Whether this type can hold every value of `other`, exactly.
    pub fn same_as(&self, other: &Type) -> bool {
        self == other
    }

    /// The element type of an array or slice.
    pub fn element(&self) -> Option<&Type> {
        match self {
            Type::Array { element, .. } | Type::Slice { element, .. } => Some(element),
            _ => None,
        }
    }

    /// The number of bytes this type occupies in a frame.
    pub fn size_in_bytes(&self, word: WordWidth) -> u32 {
        match self {
            Type::Unit => 0,
            Type::Bool | Type::I8 | Type::U8 => 1,
            Type::I16 | Type::U16 => 2,
            Type::I32 | Type::U32 => 4,
            Type::I64 | Type::U64 => u32::from(word.bytes()),
            Type::Usize => u32::from(word.bytes()),
            // A str and a slice are a pointer and a length.
            Type::Str | Type::Slice { .. } => u32::from(word.bytes()) * 2,
            Type::Pointer { .. } | Type::Reference { .. } => u32::from(word.bytes()),
            Type::Array { element, length } => {
                let count = u32::try_from(*length).unwrap_or(u32::MAX);
                element.size_in_bytes(word).saturating_mul(count)
            }
        }
    }

    /// The alignment this type requires in a frame, in bytes.
    pub fn alignment_in_bytes(&self, word: WordWidth) -> u32 {
        match self {
            Type::Unit => 1,
            Type::Bool | Type::I8 | Type::U8 => 1,
            Type::I16 | Type::U16 => 2,
            Type::I32 | Type::U32 => 4,
            Type::I64 | Type::U64 | Type::Usize => u32::from(word.bytes()),
            // Two words need only word alignment, not double-word alignment:
            // a view is a pointer and a length, and the ISA loads each word
            // separately.
            Type::Str | Type::Slice { .. } | Type::Pointer { .. } | Type::Reference { .. } => {
                u32::from(word.bytes())
            }
            Type::Array { element, .. } => element.alignment_in_bytes(word),
        }
    }

    /// The smallest and largest value of an integer type, for literal checks.
    fn integer_range(ty: &Type) -> Option<(i128, u128)> {
        Some(match ty {
            Type::I8 => (i128::from(i8::MIN), i128::from(i8::MAX) as u128),
            Type::I16 => (i128::from(i16::MIN), i128::from(i16::MAX) as u128),
            Type::I32 => (i128::from(i32::MIN), i128::from(i32::MAX) as u128),
            Type::I64 => (i128::from(i64::MIN), i128::from(i64::MAX) as u128),
            Type::U8 => (0, u128::from(u8::MAX)),
            Type::U16 => (0, u128::from(u16::MAX)),
            Type::U32 => (0, u128::from(u32::MAX)),
            Type::U64 => (0, u128::from(u64::MAX)),
            _ => return None,
        })
    }

    /// Whether a possibly negative literal fits in this integer type.
    ///
    /// A negated literal is the one place a value below zero appears in v1, so
    /// `-1u8` must be rejected rather than wrapping to 255.
    pub fn fits_signed_literal(ty: &Type, value: i128) -> bool {
        match ty {
            Type::Usize => u64::try_from(value as u128).is_ok(),
            // A negative value is compared against the signed minimum, because
            // casting it to u128 would turn it into a huge positive number.
            _ => Type::integer_range(ty).is_some_and(|(min, max)| {
                if value < 0 {
                    value >= min
                } else {
                    (value as u128) <= max
                }
            }),
        }
    }

    /// Whether an unsigned literal fits in this integer type.
    pub fn fits_literal(ty: &Type, value: u128) -> bool {
        match ty {
            Type::Usize => u64::try_from(value).is_ok(),
            _ => Type::integer_range(ty).is_some_and(|(_, max)| value <= max),
        }
    }
}

/// A checked local: a name, a type, and where it lives in the frame.
#[derive(Clone, Debug)]
pub struct LocalSlot {
    /// The local's name, or `_` for a discarded binding.
    pub name: String,
    /// Its type.
    pub ty: Type,
    /// The frame slot number, counting from zero.
    pub slot: u32,
    /// The byte offset from the frame's base.
    pub offset: u32,
    /// Whether the binding is `mut`.
    pub mutable: bool,
    /// Whether this slot is a parameter rather than a `let`.
    pub is_parameter: bool,
    /// Where it was written.
    pub span: SourceSpan,
}

/// A checked parameter.
#[derive(Clone, Debug)]
pub struct CheckedParameter {
    /// The parameter's name.
    pub name: String,
    /// Its type.
    pub ty: Type,
    /// Its frame slot.
    pub slot: u32,
    /// Its byte offset.
    pub offset: u32,
    /// Where it was written.
    pub span: SourceSpan,
}

/// A place: somewhere a value can be read from or written to.
#[derive(Clone, Debug)]
pub enum CheckedPlace {
    /// A local or parameter.
    Local {
        /// The frame slot.
        slot: u32,
        /// The byte offset.
        offset: u32,
        /// The place's type.
        ty: Type,
        /// Whether it may be written.
        mutable: bool,
        /// Where it was written.
        span: SourceSpan,
    },
    /// An element of an array or a slice.
    Index {
        /// The place being indexed.
        base: Box<CheckedPlace>,
        /// The index expression.
        index: Box<CheckedExpr>,
        /// The element's byte offset within the base.
        element_offset: u32,
        /// The element's type.
        ty: Type,
        /// Whether the element may be written.
        mutable: bool,
        /// Where it was written.
        span: SourceSpan,
    },
    /// The referent of a reference.
    Deref {
        /// The reference expression.
        reference: Box<CheckedExpr>,
        /// The referent's type.
        ty: Type,
        /// Whether the referent may be written.
        mutable: bool,
        /// Where it was written.
        span: SourceSpan,
    },
}

impl CheckedPlace {
    /// The place's type.
    pub fn ty(&self) -> &Type {
        match self {
            CheckedPlace::Local { ty, .. }
            | CheckedPlace::Index { ty, .. }
            | CheckedPlace::Deref { ty, .. } => ty,
        }
    }

    /// Whether the place may be written.
    pub fn is_mutable(&self) -> bool {
        match self {
            CheckedPlace::Local { mutable, .. }
            | CheckedPlace::Index { mutable, .. }
            | CheckedPlace::Deref { mutable, .. } => *mutable,
        }
    }

    /// The place's span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            CheckedPlace::Local { span, .. }
            | CheckedPlace::Index { span, .. }
            | CheckedPlace::Deref { span, .. } => span,
        }
    }

    /// The place's type, cloned.
    pub fn ty_cloned(&self) -> Type {
        self.ty().clone()
    }
}

/// A checked expression.
#[derive(Clone, Debug)]
pub enum CheckedExpr {
    /// An integer literal, already typed.
    Integer {
        /// The value.
        value: u128,
        /// Its type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A string literal, referring to the program's string table.
    Str {
        /// The index of the interned string.
        index: u32,
        /// Its type, always `str`.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// `true` or `false`.
    Bool {
        /// The value.
        value: bool,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A read of a place.
    Read {
        /// The place.
        place: Box<CheckedPlace>,
        /// The place's type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A call of a function or an OS syscall.
    Call {
        /// The callee's qualified name, or the syscall's name.
        callee: String,
        /// Whether the callee is an `extern` syscall.
        is_extern: bool,
        /// The arguments, in order.
        arguments: Vec<CheckedExpr>,
        /// The call's type, the callee's result.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A call of a v1 builtin method.
    Builtin {
        /// The method's name.
        method: String,
        /// The receiver.
        receiver: Box<CheckedExpr>,
        /// The arguments, in order. Every builtin so far took none; the ones that
        /// name a view's length take one.
        arguments: Vec<CheckedExpr>,
        /// The method's type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A unary operation.
    Unary {
        /// The operator's source text.
        operator: String,
        /// The operand.
        operand: Box<CheckedExpr>,
        /// The result type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A binary operation.
    Binary {
        /// The operator's source text.
        operator: String,
        /// The left operand.
        left: Box<CheckedExpr>,
        /// The right operand.
        right: Box<CheckedExpr>,
        /// The result type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A cast.
    Cast {
        /// The value being cast.
        operand: Box<CheckedExpr>,
        /// The source type.
        from: Type,
        /// The target type.
        to: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A conditional used as a value.
    If {
        /// The arms, with the final one being `else`.
        arms: Vec<CheckedArm>,
        /// The arms' common type.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A block used as a value.
    Block {
        /// The block's statements.
        statements: Vec<CheckedStmt>,
        /// The block's tail.
        tail: Box<CheckedExpr>,
        /// Where it was written.
        span: SourceSpan,
    },
    /// The address of a place, which is a reference in v1.
    AddressOf {
        /// The place.
        place: Box<CheckedPlace>,
        /// The reference's type.
        ty: Type,
        /// Whether the reference is mutable.
        mutable: bool,
        /// Where it was written.
        span: SourceSpan,
    },
    /// An array literal, whose elements are initialised in order.
    Array {
        /// The elements, already checked against the array's element type.
        elements: Vec<CheckedExpr>,
        /// The array type, which fixes the element count.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// An array filled by writing one value to every element.
    ArrayRepeat {
        /// The value written to each element.
        value: Box<CheckedExpr>,
        /// How many elements to write, which must be a literal.
        count: u64,
        /// The array type, which agrees with `count`.
        ty: Type,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A value of unit type, produced by a bare `return;`.
    Unit {
        /// Where it was written.
        span: SourceSpan,
    },
}

impl CheckedExpr {
    /// The expression's type.
    pub fn ty(&self) -> Type {
        match self {
            CheckedExpr::Integer { ty, .. }
            | CheckedExpr::Str { ty, .. }
            | CheckedExpr::Call { ty, .. }
            | CheckedExpr::Builtin { ty, .. }
            | CheckedExpr::Unary { ty, .. }
            | CheckedExpr::Binary { ty, .. }
            | CheckedExpr::If { ty, .. }
            | CheckedExpr::Array { ty, .. }
            | CheckedExpr::ArrayRepeat { ty, .. } => ty.clone(),
            CheckedExpr::Bool { .. } => Type::Bool,
            CheckedExpr::Read { ty, .. } => ty.clone(),
            CheckedExpr::Cast { to, .. } => to.clone(),
            CheckedExpr::Block { tail, .. } => tail.ty(),
            CheckedExpr::AddressOf { ty, .. } => ty.clone(),
            CheckedExpr::Unit { .. } => Type::Unit,
        }
    }

    /// The expression's span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            CheckedExpr::Integer { span, .. }
            | CheckedExpr::Str { span, .. }
            | CheckedExpr::Bool { span, .. }
            | CheckedExpr::Read { span, .. }
            | CheckedExpr::Call { span, .. }
            | CheckedExpr::Builtin { span, .. }
            | CheckedExpr::Unary { span, .. }
            | CheckedExpr::Binary { span, .. }
            | CheckedExpr::Cast { span, .. }
            | CheckedExpr::If { span, .. }
            | CheckedExpr::Block { span, .. }
            | CheckedExpr::AddressOf { span, .. }
            | CheckedExpr::Array { span, .. }
            | CheckedExpr::ArrayRepeat { span, .. }
            | CheckedExpr::Unit { span } => span,
        }
    }
}

/// One arm of a checked conditional.
#[derive(Clone, Debug)]
pub struct CheckedArm {
    /// The condition, or `None` for `else`.
    pub condition: Option<CheckedExpr>,
    /// The arm's statements.
    pub statements: Vec<CheckedStmt>,
    /// The arm's tail, when it has one.
    pub tail: Option<CheckedExpr>,
    /// The arm's type, which is the value it produces if it has one.
    pub ty: Option<Type>,
    /// Where the arm was written.
    pub span: SourceSpan,
}

/// A checked statement.
#[derive(Clone, Debug)]
pub enum CheckedStmt {
    /// A `let` binding.
    Let {
        /// The new local, or `None` for `let _ = ...`.
        local: Option<LocalSlot>,
        /// The bound value.
        value: Box<CheckedExpr>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// An assignment to a place.
    Assign {
        /// The place written.
        place: Box<CheckedPlace>,
        /// The new value.
        value: Box<CheckedExpr>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A call whose value is discarded.
    Expression {
        /// The expression, always a call or a builtin call.
        expression: Box<CheckedExpr>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A conditional statement.
    If {
        /// The arms.
        arms: Vec<CheckedArm>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A `while` loop.
    While {
        /// The condition.
        condition: Box<CheckedExpr>,
        /// The body.
        body: Box<CheckedBlock>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A `for` loop over a range.
    For {
        /// The loop variable's slot.
        slot: u32,
        /// The loop variable's type.
        ty: Type,
        /// The inclusive start.
        start: Box<CheckedExpr>,
        /// The exclusive end.
        end: Box<CheckedExpr>,
        /// The body.
        body: Box<CheckedBlock>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A `loop`.
    Loop {
        /// The body.
        body: Box<CheckedBlock>,
        /// Where the statement was written.
        span: SourceSpan,
    },
    /// A `break`, naming the loop it leaves.
    Break {
        /// How many loops out this breaks.
        levels: u32,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A `continue`, naming the loop it continues.
    Continue {
        /// How many loops out this continues.
        levels: u32,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A `return`.
    Return {
        /// The returned value, or `None` for a bare `return;`.
        value: Option<Box<CheckedExpr>>,
        /// Where it was written.
        span: SourceSpan,
    },
    /// A nested block.
    Block {
        /// The block.
        block: Box<CheckedBlock>,
        /// Where the statement was written.
        span: SourceSpan,
    },
}

impl CheckedStmt {
    /// The statement's span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            CheckedStmt::Let { span, .. }
            | CheckedStmt::Assign { span, .. }
            | CheckedStmt::Expression { span, .. }
            | CheckedStmt::If { span, .. }
            | CheckedStmt::While { span, .. }
            | CheckedStmt::For { span, .. }
            | CheckedStmt::Loop { span, .. }
            | CheckedStmt::Break { span, .. }
            | CheckedStmt::Continue { span, .. }
            | CheckedStmt::Return { span, .. }
            | CheckedStmt::Block { span, .. } => span,
        }
    }
}

/// A checked block.
#[derive(Clone, Debug)]
pub struct CheckedBlock {
    /// The statements, in order.
    pub statements: Vec<CheckedStmt>,
    /// The tail expression, if the block ends in one.
    pub tail: Option<CheckedExpr>,
    /// Where the block was written.
    pub span: SourceSpan,
}

/// A checked function.
#[derive(Clone, Debug)]
pub struct CheckedFunction {
    /// The declared name.
    pub name: String,
    /// The fully qualified name, with `::` between modules.
    pub qualified_name: String,
    /// Whether the function is `pub`.
    pub is_public: bool,
    /// The parameters, in order.
    pub parameters: Vec<CheckedParameter>,
    /// The result type.
    pub result: Type,
    /// The frame's size in bytes.
    pub frame_size: u32,
    /// Every local, including parameters, in declaration order.
    pub locals: Vec<LocalSlot>,
    /// The body.
    pub body: CheckedBlock,
    /// The module path the function belongs to.
    pub module: Vec<String>,
    /// Where the function was written.
    pub span: SourceSpan,
}

/// A checked `extern` declaration, matched against the OS ABI.
#[derive(Clone, Debug)]
pub struct CheckedExtern {
    /// The declared name.
    pub name: String,
    /// The syscall it maps to, or `None` for a call the design reserves for a
    /// later ABI step, which has no number yet.
    pub syscall: Option<Syscall>,
    /// The parameters, in ABI order.
    pub parameters: Vec<CheckedParameter>,
    /// The result type.
    pub result: Type,
    /// The module path the declaration belongs to.
    pub module: Vec<String>,
    /// Where it was written.
    pub span: SourceSpan,
}

/// A checked `const`.
#[derive(Clone, Debug)]
pub struct CheckedConstant {
    /// The declared name.
    pub name: String,
    /// Its fully qualified name.
    pub qualified_name: String,
    /// Its type.
    pub ty: Type,
    /// Its value.
    pub value: CheckedExpr,
    /// The module path the constant belongs to.
    pub module: Vec<String>,
    /// Where it was written.
    pub span: SourceSpan,
}

/// An interned string literal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckedString {
    /// The literal's bytes.
    pub text: String,
}

/// A fully checked program: the output of Step 61.
#[derive(Clone, Debug)]
pub struct CheckedProgram {
    /// The word width the frame layout was computed for.
    pub word: WordWidth,
    /// Every function, in source order.
    pub functions: Vec<CheckedFunction>,
    /// Every `extern` declaration, in source order.
    pub externs: Vec<CheckedExtern>,
    /// Every `const`, in source order.
    pub constants: Vec<CheckedConstant>,
    /// Every string literal, deduplicated.
    pub strings: Vec<CheckedString>,
}

impl CheckedProgram {
    /// The function with this qualified name.
    pub fn function(&self, qualified_name: &str) -> Option<&CheckedFunction> {
        self.functions
            .iter()
            .find(|function| function.qualified_name == qualified_name)
    }

    /// The `main` function, if the program has one.
    pub fn entry(&self) -> Option<&CheckedFunction> {
        self.function("main")
    }
}

/// The target the frame layout is computed for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Target {
    /// The machine's word width.
    pub word: WordWidth,
}

impl Default for Target {
    fn default() -> Self {
        Self {
            word: WordWidth::W64,
        }
    }
}

/// Type-checks a resolved program for the default target.
pub fn check(
    source: SourceId,
    sources: &SourceManager,
    resolved: &Resolved,
) -> Result<CheckedProgram, StageError> {
    check_for(source, sources, resolved, Target::default())
}

/// Type-checks a resolved program for a specific target.
///
/// The target only affects frame layout, which is data the next stage needs.
/// It does not affect which programs are valid.
pub fn check_for(
    source: SourceId,
    sources: &SourceManager,
    resolved: &Resolved,
    target: Target,
) -> Result<CheckedProgram, StageError> {
    let mut checker = Checker {
        source,
        sources,
        resolved,
        target,
        loop_depth: 0,
        current_module: Vec::new(),
        current_result: Type::Unit,
        current_locals: Vec::new(),
        strings: Vec::new(),
        string_index: BTreeMap::new(),
        functions: Vec::new(),
        externs: Vec::new(),
        constants: Vec::new(),
    };
    checker.run()
}

/// What an expression's type is before context is applied.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Inferred {
    /// An integer literal whose type is not decided yet.
    IntegerLiteral(u128),
    /// A decided type.
    Concrete(Type),
    /// A type that is never valid, carrying the diagnostic already built.
    Invalid,
}

impl Inferred {
    /// The decided type, if there is one.
    fn concrete(&self) -> Option<&Type> {
        match self {
            Inferred::Concrete(ty) => Some(ty),
            _ => None,
        }
    }

    /// The default type for an undecided literal, which is `i32`.
    fn defaulted(self) -> Inferred {
        match self {
            Inferred::IntegerLiteral(_) => Inferred::Concrete(Type::I32),
            other => other,
        }
    }
}

struct Checker<'a> {
    source: SourceId,
    sources: &'a SourceManager,
    resolved: &'a Resolved,
    target: Target,
    /// How many loops enclose the expression being checked.
    ///
    /// A conditional used as a value is checked from `check_expression`, which does
    /// not take a depth parameter, so the depth lives here and is set by
    /// `check_block` for every loop body it checks. A `break` or `continue` in an
    /// arm of a conditional inside a loop must still see that loop.
    loop_depth: u32,
    /// The module the function being checked belongs to.
    ///
    /// A name is resolved from here first, so a function can call a sibling of
    /// its own module without `pub`.
    current_module: Vec<String>,
    /// The result type of the function being checked.
    ///
    /// A `return` is checked against *this* and not against the expected type of
    /// the block it appears in. A block's expected type is what that block
    /// evaluates to, which is `()` for a loop body — and `return` does not
    /// evaluate to anything, it leaves. So `return false;` inside a `while` was
    /// checked against `()` and rejected, which is why no function in the
    /// documentation returned from inside a loop.
    current_result: Type,
    strings: Vec<CheckedString>,
    string_index: BTreeMap<String, u32>,
    functions: Vec<CheckedFunction>,
    externs: Vec<CheckedExtern>,
    constants: Vec<CheckedConstant>,
    /// The locals and parameters of the function being checked.
    ///
    /// Only the parameters are meaningful for borrow rules; a local's own
    /// `mut` is already in its `Binding`. This exists because the scope a body is
    /// checked in does not say which bindings are parameters, and a parameter is
    /// writable without `mut`.
    current_locals: Vec<LocalSlot>,
}

impl<'a> Checker<'a> {
    fn run(&mut self) -> Result<CheckedProgram, StageError> {
        // The file's own module is not in `modules`: that map holds only nested
        // modules, so the root has to be walked alongside them.
        let all: Vec<&crate::resolve::ResolvedModule> = self.resolved.all_modules().collect();
        // Externs first, so a call can be checked against a declaration that
        // appears later in the file.
        for module in &all {
            for name in &module.order {
                if let Some(Symbol::Extern(item)) = module.items.get(name) {
                    let checked = self.check_extern(item)?;
                    self.externs.push(checked);
                }
            }
        }
        for module in &all {
            for name in &module.order {
                if let Some(Symbol::Constant(item)) = module.items.get(name) {
                    let checked = self.check_constant(item)?;
                    self.constants.push(checked);
                }
            }
        }
        for module in &all {
            for name in &module.order {
                if let Some(Symbol::Function(item)) = module.items.get(name) {
                    let checked = self.check_function(item)?;
                    self.functions.push(checked);
                }
            }
        }
        Ok(CheckedProgram {
            word: self.target.word,
            functions: self.functions.clone(),
            externs: self.externs.clone(),
            constants: self.constants.clone(),
            strings: self.strings.clone(),
        })
    }

    // ------------------------------------------------------------- externs

    fn check_extern(&mut self, item: &ResolvedExtern) -> Result<CheckedExtern, StageError> {
        let syscall = abi_syscall(&item.name);
        if syscall.is_none() && !is_reserved_design_syscall(&item.name) {
            return Err(self.error(
                codes::ABI_MISMATCH,
                format!("`{}` is not an OS ABI syscall", item.name),
                &item.span,
                &[
                    "an `extern \"syscall\"` declaration must name a call in the shared `lazalith_os_abi::Syscall` table, or one of the calls the Lazen design reserves for a later ABI step",
                ],
                Some("see the syscall table in crates/lazalith-os-abi/src/syscall.rs"),
                &[],
            ));
        }
        // A reserved name has no number yet, so its arity cannot be checked
        // against the ABI. The declaration is still fully type-checked.
        let abi_arguments = match syscall {
            Some(syscall) => syscall.argument_count(),
            None => item.parameters.len(),
        };
        if item.parameters.len() > abi_arguments {
            return Err(self.error(
                codes::ABI_MISMATCH,
                format!(
                    "`{}` takes {} arguments, but this declaration has {}",
                    item.name,
                    abi_arguments,
                    item.parameters.len()
                ),
                &item.span,
                &["the declaration must mirror the ABI's argument order exactly"],
                Some("remove the extra parameters, or declare a different syscall"),
                &[],
            ));
        }
        let mut offset = 0u32;
        let mut parameters = Vec::new();
        for parameter in &item.parameters {
            let ty = self.type_of(&parameter.annotation)?;
            let alignment = ty.alignment_in_bytes(self.target.word);
            offset = align_up(offset, alignment);
            parameters.push(CheckedParameter {
                name: parameter.name.clone(),
                ty: ty.clone(),
                slot: parameters.len() as u32,
                offset,
                span: parameter.span.clone(),
            });
            offset = offset.saturating_add(ty.size_in_bytes(self.target.word));
        }
        let result = self.type_of(&item.result)?;
        Ok(CheckedExtern {
            name: item.name.clone(),
            syscall,
            parameters,
            result,
            module: item.module.clone(),
            span: item.span.clone(),
        })
    }

    // ------------------------------------------------------------ constants

    fn check_constant(&mut self, item: &ResolvedConstant) -> Result<CheckedConstant, StageError> {
        let expected = match &item.annotation {
            Some(annotation) => Some(self.type_of(annotation)?),
            None => None,
        };
        let inferred = self.check_constant_expression(&item.value, expected.clone())?;
        let ty = match (expected, inferred) {
            (Some(expected), Inferred::Concrete(actual)) => {
                if expected != actual {
                    return Err(self.error(
                        codes::MISMATCH,
                        format!(
                            "this `const` is declared `{}` but its value is `{}`",
                            ty_name(&expected),
                            ty_name(&actual)
                        ),
                        &span_of(&item.value),
                        &[],
                        Some("change the declared type, or the value"),
                        &[],
                    ));
                }
                expected
            }
            (Some(expected), Inferred::IntegerLiteral(value)) => {
                self.check_literal_fits(value, &expected, &span_of(&item.value))?;
                expected
            }
            (None, Inferred::Concrete(actual)) => actual,
            (None, Inferred::IntegerLiteral(_)) => Type::I32,
            (_, Inferred::Invalid) => {
                return Err(self.error(
                    codes::NOT_CONSTANT,
                    "this `const` value is not valid",
                    &span_of(&item.value),
                    &[],
                    Some("a `const` may be an integer, a string, or a bool literal"),
                    &[],
                ));
            }
        };
        let value = self.finalize(
            &item.value,
            Inferred::Concrete(ty.clone()),
            Some(ty.clone()),
            &[],
        )?;
        Ok(CheckedConstant {
            name: item.name.clone(),
            qualified_name: qualified_name(&item.module, &item.name),
            ty,
            value,
            module: item.module.clone(),
            span: item.span.clone(),
        })
    }

    /// A `const` initialiser: a literal, or a cast of a literal.
    fn check_constant_expression(
        &self,
        expression: &Expr,
        expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        match expression {
            Expr::Int { literal, .. } => Ok(match literal.suffix {
                Some(suffix) => Inferred::Concrete(Type::from_suffix(suffix)),
                None => Inferred::IntegerLiteral(literal.value),
            }),
            Expr::Str { span, .. } => {
                // A literal still has to be the type the context asked for: this is
                // what rejects `[1, "two"]`.
                if let Some(expected) = &expected
                    && expected != &Type::Str
                {
                    return Err(self.mismatch(
                        span,
                        &Type::Str,
                        expected,
                        "a string literal cannot be used where a non-string is required",
                    ));
                }
                Ok(Inferred::Concrete(Type::Str))
            }
            Expr::Bool { span, .. } => {
                if let Some(expected) = &expected
                    && expected != &Type::Bool
                {
                    return Err(self.mismatch(
                        span,
                        &Type::Bool,
                        expected,
                        "a `bool` literal cannot be used where a non-`bool` is required",
                    ));
                }
                Ok(Inferred::Concrete(Type::Bool))
            }
            Expr::Cast {
                operand, target, ..
            } => {
                let target_type = self.type_of(target)?;
                let inner = self.check_constant_expression(operand, Some(target_type.clone()))?;
                if let Inferred::IntegerLiteral(value) = inner {
                    self.check_literal_fits(value, &target_type, &span_of(operand))?;
                }
                let _ = expected;
                Ok(Inferred::Concrete(target_type))
            }
            Expr::Unary {
                operator: UnaryOp::Negate,
                operand,
                ..
            } => match self.check_constant_expression(operand, expected.clone())? {
                Inferred::IntegerLiteral(value) => {
                    // A negated `const` literal is range-checked as the negative value
                    // it is, so `const X: u8 = -1;` cannot slip through.
                    let ty = expected.filter(|ty| ty.is_integer()).unwrap_or(Type::I32);
                    let signed = -(value as i128);
                    if !Type::fits_signed_literal(&ty, signed) {
                        return Err(self.literal_range_error_negated(
                            signed,
                            &ty,
                            &span_of(operand),
                        ));
                    }
                    Ok(Inferred::Concrete(ty))
                }
                other => Ok(other),
            },
            _ => Ok(Inferred::Invalid),
        }
    }

    // ------------------------------------------------------------ functions

    fn check_function(&mut self, item: &ResolvedFunction) -> Result<CheckedFunction, StageError> {
        // Names in this body resolve from the module that declares it.
        self.current_module = item.module.clone();
        let result = match &item.result {
            Some(annotation) => self.type_of(annotation)?,
            None => Type::Unit,
        };
        // A `return` anywhere in this body is checked against this, including
        // from inside a loop or a conditional, where the enclosing block's own
        // expected type is something else entirely.
        self.current_result = result.clone();
        let mut scope: Vec<Binding> = Vec::new();
        let mut locals: Vec<LocalSlot> = Vec::new();
        // The parameters are the whole of this function's frame at the point a
        // body is checked, and knowing which bindings are parameters is what lets
        // a `&mut [T]` parameter and an `as_str` status be written through without
        // the caller having said `mut`. It is set once the parameters are laid out
        // and read from then on, so it is never observed half-built.
        self.current_locals = Vec::new();
        let mut offset = 0u32;
        let mut parameters = Vec::new();
        for parameter in &item.parameters {
            let ty = self.type_of(&parameter.annotation)?;
            offset = align_up(offset, ty.alignment_in_bytes(self.target.word));
            let slot = parameters.len() as u32;
            parameters.push(CheckedParameter {
                name: parameter.name.clone(),
                ty: ty.clone(),
                slot,
                offset,
                span: parameter.span.clone(),
            });
            locals.push(LocalSlot {
                name: parameter.name.clone(),
                ty,
                slot,
                offset,
                mutable: false,
                is_parameter: true,
                span: parameter.span.clone(),
            });
            scope.push(Binding {
                name: parameter.name.clone(),
                slot,
                offset,
                ty: locals[locals.len() - 1].ty.clone(),
                mutable: false,
                span: parameter.span.clone(),
            });
            offset =
                offset.saturating_add(locals[locals.len() - 1].ty.size_in_bytes(self.target.word));
        }
        let function_span = item.span.clone();
        // The parameters are laid out; the body can now be checked against them.
        self.current_locals = locals.clone();
        let body = self.check_block(
            &item.body,
            &mut scope,
            &mut locals,
            &mut offset,
            result.clone(),
            0,
        )?;
        // A function that produces a value must produce one on every path that
        // falls off the end. A function that produces *nothing* has nothing to
        // produce: control reaching the end of a unit function is an ordinary
        // return, and the lowering emits exactly that. Requiring a trailing
        // `return;` from a unit function whose last statement is a loop or a
        // conditional would reject correct code for the sake of a value that does
        // not exist.
        if result != Type::Unit && body.tail.is_none() && !self.always_produces(&item.body) {
            return Err(self.error(
                codes::MISSING_RESULT,
                format!(
                    "this function must end in a value of type `{}`",
                    ty_name(&result)
                ),
                &function_span,
                &[if result == Type::Unit {
                    "a function with no result type must end in `return;` or in a `loop` that never breaks"
                } else {
                    "a function whose body ends in an expression evaluates that expression as its result"
                }],
                Some("add the missing expression, or a `return`"),
                &[],
            ));
        }
        let frame_size = align_up(offset, u32::from(self.target.word.bytes()));
        Ok(CheckedFunction {
            name: item.name.clone(),
            qualified_name: qualified_name(&item.module, &item.name),
            is_public: item.is_public,
            parameters,
            result,
            frame_size,
            locals,
            body,
            module: item.module.clone(),
            span: function_span,
        })
    }

    /// Whether a block is guaranteed not to fall through.
    fn always_produces(&self, block: &Block) -> bool {
        let Some(last) = block.statements.last() else {
            return block.tail.is_some();
        };
        self.statement_diverges(last)
    }

    fn statement_diverges(&self, statement: &Stmt) -> bool {
        match statement {
            Stmt::Return { .. } => true,
            Stmt::Loop { body, .. } => !block_contains_break(&body.statements),
            Stmt::If { arms, .. } => match arms.last() {
                Some(arm) => match arm.condition {
                    None => arm.body.tail.is_some() || self.always_produces(&arm.body),
                    Some(_) => false,
                },
                None => false,
            },
            Stmt::Block { block, .. } => self.always_produces(block),
            _ => false,
        }
    }

    // ---------------------------------------------------------------- types

    /// Converts a written type into a checked one, rejecting anything outside
    /// the closed v1 set.
    fn type_of(&self, annotation: &crate::ast::TypeAnnotation) -> Result<Type, StageError> {
        self.type_expr(&annotation.kind, &annotation.span)
    }

    /// Converts one type expression into a checked type.
    ///
    /// The parser gives a whole annotation one span, so a nested type is
    /// reported against the annotation that contains it.
    fn type_expr(&self, kind: &TypeExpr, span: &SourceSpan) -> Result<Type, StageError> {
        Ok(match kind {
            TypeExpr::Bool => Type::Bool,
            TypeExpr::I8 => Type::I8,
            TypeExpr::I16 => Type::I16,
            TypeExpr::I32 => Type::I32,
            TypeExpr::I64 => Type::I64,
            TypeExpr::U8 => Type::U8,
            TypeExpr::U16 => Type::U16,
            TypeExpr::U32 => Type::U32,
            TypeExpr::U64 => Type::U64,
            TypeExpr::Usize => Type::Usize,
            // `str` and `&str` are one type in v1: a str is already a view, so
            // a reference to it would have nothing to add.
            TypeExpr::Str | TypeExpr::StrRef => Type::Str,
            TypeExpr::Ptr(pointee) => Type::Pointer {
                pointee: Box::new(self.type_expr(pointee, span)?),
            },
            TypeExpr::Slice { element, mutable } => Type::Slice {
                element: Box::new(self.type_expr(element, span)?),
                mutable: *mutable,
            },
            TypeExpr::Array { element, length } => {
                let element = self.type_expr(element, span)?;
                if *length == 0 {
                    return Err(self.error(
                        codes::ARRAY_TOO_LARGE,
                        "an array of length 0 has no type to infer",
                        span,
                        &["Lazen v1 has no way to name the element type of an empty array"],
                        Some("give the length a value of at least 1"),
                        &[],
                    ));
                }
                let element_bytes = element.size_in_bytes(self.target.word);
                let bytes = array_bytes(*length, element_bytes);
                if bytes > u128::from(u32::MAX) {
                    return Err(self.error(
                        codes::ARRAY_TOO_LARGE,
                        format!("this array of {length} elements is too large for a frame"),
                        span,
                        &[
                            format!("it would need {bytes} bytes, and a frame is at most 4 GiB")
                                .as_str(),
                        ],
                        Some("use a smaller array, or a slice"),
                        &[],
                    ));
                }
                Type::Array {
                    element: Box::new(element),
                    length: *length,
                }
            }
        })
    }

    // ------------------------------------------------------------- blocks

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn check_block(
        &mut self,
        block: &Block,
        scope: &mut Vec<Binding>,
        locals: &mut Vec<LocalSlot>,
        offset: &mut u32,
        expected: Type,
        loop_depth: u32,
    ) -> Result<CheckedBlock, StageError> {
        // A block may be checked more than once for the same source block, so the
        // enclosing depth is saved and restored rather than assigned.
        let saved_depth = self.loop_depth;
        self.loop_depth = loop_depth;
        // Bindings before this index belong to an enclosing scope, so a name from
        // one of them may be reused here.
        let scope_start = scope.len();
        let mut statements = Vec::new();
        for statement in &block.statements {
            let checked = self.check_statement(
                statement,
                scope,
                locals,
                offset,
                &expected,
                loop_depth,
                scope_start,
            )?;
            statements.push(checked);
        }
        let tail = match &block.tail {
            Some(tail) => {
                let inferred = self.check_expression(tail, scope, Some(expected.clone()))?;
                let checked = self.finalize(tail, inferred, Some(expected.clone()), scope)?;
                Some(checked)
            }
            None => None,
        };
        self.loop_depth = saved_depth;
        Ok(CheckedBlock {
            statements,
            tail,
            span: block.span.clone(),
        })
    }

    // ---------------------------------------------------------- statements

    #[allow(clippy::too_many_arguments)]
    fn check_statement(
        &mut self,
        statement: &Stmt,
        scope: &mut Vec<Binding>,
        locals: &mut Vec<LocalSlot>,
        offset: &mut u32,
        expected: &Type,
        loop_depth: u32,
        scope_start: usize,
    ) -> Result<CheckedStmt, StageError> {
        match statement {
            Stmt::Let {
                name,
                mutable,
                annotation,
                value,
                span,
            } => {
                let declared = match annotation {
                    Some(annotation) => Some(self.type_of(annotation)?),
                    None => None,
                };
                let inferred = self.check_expression(value, scope, declared.clone())?;
                let ty = self.resolve_inferred(
                    inferred,
                    declared.clone(),
                    &span_of(value),
                    "this binding",
                )?;
                let discarded = name.text == "_";
                // A nested block has its own scope, so a name may be reused there;
                // a function body is one scope, so it may not. The scope chain is
                // already flattened, so one search finds the nearest binding.
                let shadows = scope
                    .iter()
                    .rposition(|binding| binding.name == name.text)
                    .filter(|index| *index >= scope_start);
                if !discarded && shadows.is_some() {
                    return Err(self.error(
                        crate::resolve::codes::DUPLICATE_BINDING,
                        format!("`{}` is already bound in this scope", name.text),
                        &name.span,
                        &["a name may be bound once per scope"],
                        Some("rename one of the two bindings"),
                        &[],
                    ));
                }
                if discarded && *mutable {
                    return Err(self.error(
                        codes::BAD_BORROW,
                        "`_` cannot be `mut`",
                        &name.span,
                        &["`let _ = value;` evaluates the value and discards it"],
                        Some("write `let value = ...;` if you need the value"),
                        &[],
                    ));
                }
                let local = if discarded {
                    None
                } else {
                    let alignment = ty.alignment_in_bytes(self.target.word);
                    *offset = align_up(*offset, alignment);
                    let slot = locals.len() as u32;
                    let local = LocalSlot {
                        name: name.text.clone(),
                        ty: ty.clone(),
                        slot,
                        offset: *offset,
                        mutable: *mutable,
                        is_parameter: false,
                        span: name.span.clone(),
                    };
                    *offset = offset.saturating_add(ty.size_in_bytes(self.target.word));
                    scope.push(Binding {
                        name: name.text.clone(),
                        slot,
                        offset: local.offset,
                        ty: ty.clone(),
                        mutable: *mutable,
                        span: name.span.clone(),
                    });
                    locals.push(local.clone());
                    Some(local)
                };
                let value = self.finalize(value, Inferred::Concrete(ty), declared, scope)?;
                Ok(CheckedStmt::Let {
                    local,
                    value: Box::new(value),
                    span: span.clone(),
                })
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let place = self.check_place(target, scope)?;
                let place_type = place.ty_cloned();
                if !place.is_mutable() {
                    return Err(self.immutable_error(target, &place, scope));
                }
                let inferred = self.check_expression(value, scope, Some(place_type.clone()))?;
                let value = self.finalize(value, inferred, Some(place_type.clone()), scope)?;
                Ok(CheckedStmt::Assign {
                    place: Box::new(place),
                    value: Box::new(value),
                    span: span.clone(),
                })
            }
            Stmt::Expression { expression, span } => {
                let inferred = self.check_expression(expression, scope, None)?;
                match expression {
                    Expr::Call { .. } | Expr::MethodCall { .. } => {
                        let checked = self.finalize(expression, inferred, None, scope)?;
                        Ok(CheckedStmt::Expression {
                            expression: Box::new(checked),
                            span: span.clone(),
                        })
                    }
                    _ => {
                        let ty = inferred.clone().defaulted();
                        let Some(ty) = ty.concrete() else {
                            return Err(self.error(
                                codes::DISCARDED_VALUE,
                                "this expression has no type to discard",
                                &span_of(expression),
                                &[],
                                Some("a statement must be a call, a `let`, or an assignment"),
                                &[],
                            ));
                        };
                        // A bare `if` used as a statement is a conditional, not
                        // a discarded value, and produces no value at all.
                        if let Expr::If { arms, .. } = expression {
                            let mut checked_arms = Vec::new();
                            for arm in arms {
                                checked_arms.push(self.check_arm(
                                    arm,
                                    scope,
                                    locals,
                                    offset,
                                    &Type::Unit,
                                    loop_depth,
                                )?);
                            }
                            return Ok(CheckedStmt::If {
                                arms: checked_arms,
                                span: span.clone(),
                            });
                        }
                        Err(self.error(
                            codes::DISCARDED_VALUE,
                            format!(
                                "this expression has type `{}`, and its value would be discarded",
                                ty_name(ty)
                            ),
                            &span_of(expression),
                            &["Lazen v1 accepts only a call as a statement whose value is not used"],
                            Some("bind it with `let`, or call a function"),
                            &[],
                        ))
                    }
                }
            }
            Stmt::If { arms, span } => {
                // The conditions are checked first, so a condition that is not a
                // `bool` is reported as such rather than as a discarded value.
                for arm in arms {
                    if let Some(condition) = &arm.condition {
                        let inferred = self.check_condition(condition, scope)?;
                        self.finalize(condition, inferred, Some(Type::Bool), scope)?;
                    }
                }
                // A conditional written as a statement discards whatever its arms
                // produce, and the syntax document rejects exactly that: a value
                // that is computed and thrown away is a mistake worth reporting.
                if let Some(arm) = arms.iter().find(|arm| arm.body.tail.is_some()) {
                    let tail_span = arm
                        .body
                        .tail
                        .as_ref()
                        .map(|tail| tail.span_of().clone())
                        .unwrap_or_else(|| arm.span.clone());
                    return Err(self.error(
                        codes::DISCARDED_VALUE,
                        "this conditional statement ends in a value, which would be discarded",
                        &tail_span,
                        &["Lazen v1 accepts only a call as a statement whose value is not used"],
                        Some("use the conditional as a value, or remove the trailing expression"),
                        &[],
                    ));
                }
                let mut checked_arms = Vec::new();
                for arm in arms {
                    checked_arms
                        .push(self.check_arm(arm, scope, locals, offset, expected, loop_depth)?);
                }
                Ok(CheckedStmt::If {
                    arms: checked_arms,
                    span: span.clone(),
                })
            }
            Stmt::While {
                condition,
                body,
                span,
            } => {
                let inferred = self.check_condition(condition, scope)?;
                let condition = self.finalize(condition, inferred, Some(Type::Bool), scope)?;
                let mut inner_scope = scope.clone();
                let mut inner_locals = locals.clone();
                let mut inner_offset = *offset;
                let block = self.check_block(
                    body,
                    &mut inner_scope,
                    &mut inner_locals,
                    &mut inner_offset,
                    Type::Unit,
                    loop_depth + 1,
                )?;
                // A `let` inside a `while` body is a local of the *function*: the
                // block is a scope, not a frame. Writing the body's locals and
                // offset back is what puts them in the frame layout — without it
                // they are allocated in a list that is then thrown away, so the
                // slot is not reported and the frame is too small for it.
                *locals = inner_locals;
                *offset = inner_offset;
                Ok(CheckedStmt::While {
                    condition: Box::new(condition),
                    body: Box::new(block),
                    span: span.clone(),
                })
            }
            Stmt::For {
                name,
                iterated,
                end,
                body,
                span,
            } => {
                // `for x in a..b` iterates an integer range. The other form,
                // `for x in collection`, is a documented omission, so it is
                // rejected here by name rather than guessed at.
                let Some(end) = end else {
                    let inferred = self.check_expression(iterated, scope, None)?;
                    let ty = inferred.defaulted();
                    let ty_name = ty
                        .concrete()
                        .map(ty_name)
                        .unwrap_or_else(|| "an untyped value".to_string());
                    return Err(self.error(
                        codes::NOT_A_RANGE,
                        format!("Lazen v1 cannot iterate over `{ty_name}`"),
                        &span_of(iterated),
                        &["Lazen v1 ranges are integer ranges only: `for x in start..end`"],
                        Some("iterate over an integer range, or index the value yourself"),
                        &[],
                    ));
                };
                let start_inferred = self.check_expression(iterated, scope, None)?;
                let start_type = self.require_integer(
                    start_inferred,
                    &span_of(iterated),
                    "the start of a range",
                )?;
                let end_inferred = self.check_expression(end, scope, Some(start_type.clone()))?;
                let end_type = self.integer_operand(
                    end_inferred,
                    &start_type,
                    &span_of(end),
                    "the end of a range",
                )?;
                if start_type != end_type {
                    return Err(self.mismatch(
                        &span_of(end),
                        &end_type,
                        &start_type,
                        "a range's two bounds must have the same type",
                    ));
                }
                let start_checked = self.finalize(
                    iterated,
                    Inferred::Concrete(start_type.clone()),
                    None,
                    scope,
                )?;
                let end_checked =
                    self.finalize(end, Inferred::Concrete(start_type.clone()), None, scope)?;
                let mut inner_scope = scope.clone();
                let mut inner_locals = locals.clone();
                let mut inner_offset = *offset;
                let alignment = start_type.alignment_in_bytes(self.target.word);
                inner_offset = align_up(inner_offset, alignment);
                let slot = inner_locals.len() as u32;
                let local = LocalSlot {
                    name: name.text.clone(),
                    ty: start_type.clone(),
                    slot,
                    offset: inner_offset,
                    mutable: false,
                    is_parameter: false,
                    span: name.span.clone(),
                };
                // The induction variable is a local of the function and is live
                // for the whole loop, so its storage is reserved *before* the body
                // is checked. Leaving it out would let the body allocate over it,
                // and would leave `frame_size` too small to hold it.
                inner_offset =
                    inner_offset.saturating_add(start_type.size_in_bytes(self.target.word));
                inner_locals.push(local.clone());
                inner_scope.push(Binding {
                    name: name.text.clone(),
                    slot,
                    offset: local.offset,
                    ty: start_type.clone(),
                    mutable: false,
                    span: name.span.clone(),
                });
                let block = self.check_block(
                    body,
                    &mut inner_scope,
                    &mut inner_locals,
                    &mut inner_offset,
                    Type::Unit,
                    loop_depth + 1,
                )?;
                // The body may bind more locals; they belong to the function.
                *locals = inner_locals;
                *offset = inner_offset;
                Ok(CheckedStmt::For {
                    slot,
                    ty: start_type,
                    start: Box::new(start_checked),
                    end: Box::new(end_checked),
                    body: Box::new(block),
                    span: span.clone(),
                })
            }
            Stmt::Loop { body, span } => {
                let mut inner_scope = scope.clone();
                let mut inner_locals = locals.clone();
                let mut inner_offset = *offset;
                let block = self.check_block(
                    body,
                    &mut inner_scope,
                    &mut inner_locals,
                    &mut inner_offset,
                    Type::Unit,
                    loop_depth + 1,
                )?;
                *locals = inner_locals;
                *offset = inner_offset;
                Ok(CheckedStmt::Loop {
                    body: Box::new(block),
                    span: span.clone(),
                })
            }
            Stmt::Break { span } => {
                if loop_depth == 0 {
                    return Err(self.error(
                        codes::OUTSIDE_LOOP,
                        "`break` is only meaningful inside a loop",
                        span,
                        &["Lazen v1 has no labelled break"],
                        Some("move the `break` into a `while`, `for`, or `loop` body"),
                        &[],
                    ));
                }
                Ok(CheckedStmt::Break {
                    levels: 1,
                    span: span.clone(),
                })
            }
            Stmt::Continue { span } => {
                if loop_depth == 0 {
                    return Err(self.error(
                        codes::OUTSIDE_LOOP,
                        "`continue` is only meaningful inside a loop",
                        span,
                        &["Lazen v1 has no labelled continue"],
                        Some("move the `continue` into a `while`, `for`, or `loop` body"),
                        &[],
                    ));
                }
                Ok(CheckedStmt::Continue {
                    levels: 1,
                    span: span.clone(),
                })
            }
            Stmt::Return { value, span } => {
                // The value is checked against the *function's* result, not
                // against what the enclosing block evaluates to. A `return` inside
                // a `while` or a `for` sits in a block whose expected type is
                // `()`, and checking against that rejected every return from
                // inside a loop.
                let returns = self.current_result.clone();
                let checked = match value {
                    Some(value) => {
                        let inferred =
                            self.check_expression(value, scope, Some(returns.clone()))?;
                        Some(Box::new(self.finalize(
                            value,
                            inferred,
                            Some(returns.clone()),
                            scope,
                        )?))
                    }
                    None => {
                        if returns != Type::Unit {
                            return Err(self.error(
                                codes::RETURN_TYPE,
                                format!(
                                    "a bare `return;` produces no value, but this function returns `{}`",
                                    ty_name(&returns)
                                ),
                                span,
                                &["Lazen v1 has exactly one result type per function"],
                                Some("return a value, or remove the function's result type"),
                                &[],
                            ));
                        }
                        None
                    }
                };
                Ok(CheckedStmt::Return {
                    value: checked,
                    span: span.clone(),
                })
            }
            Stmt::Block { block, span } => {
                let mut inner_scope = scope.clone();
                let mut inner_locals = locals.clone();
                let mut inner_offset = *offset;
                let checked = self.check_block(
                    block,
                    &mut inner_scope,
                    &mut inner_locals,
                    &mut inner_offset,
                    Type::Unit,
                    loop_depth,
                )?;
                *locals = inner_locals;
                *offset = inner_offset;
                Ok(CheckedStmt::Block {
                    block: Box::new(checked),
                    span: span.clone(),
                })
            }
        }
    }

    fn check_arm(
        &mut self,
        arm: &IfArm,
        scope: &mut [Binding],
        locals: &mut Vec<LocalSlot>,
        offset: &mut u32,
        expected: &Type,
        loop_depth: u32,
    ) -> Result<CheckedArm, StageError> {
        // The condition is kept, not replaced: Step 62 has to emit the test.
        let checked_condition = match &arm.condition {
            Some(condition) => {
                let inferred = self.check_condition(condition, scope)?;
                Some(self.finalize(condition, inferred, Some(Type::Bool), scope)?)
            }
            None => None,
        };
        let mut inner_scope = scope.to_vec();
        let mut inner_locals = locals.clone();
        let mut inner_offset = *offset;
        let block = self.check_block(
            &arm.body,
            &mut inner_scope,
            &mut inner_locals,
            &mut inner_offset,
            expected.clone(),
            loop_depth,
        )?;
        *locals = inner_locals;
        *offset = inner_offset;
        let ty = block.tail.as_ref().map(CheckedExpr::ty);
        Ok(CheckedArm {
            condition: checked_condition,
            statements: block.statements,
            tail: block.tail,
            ty,
            span: arm.span.clone(),
        })
    }

    fn check_condition(
        &mut self,
        condition: &Expr,
        scope: &[Binding],
    ) -> Result<Inferred, StageError> {
        let inferred = self.check_expression(condition, scope, Some(Type::Bool))?;
        match &inferred {
            Inferred::Concrete(ty) if ty == &Type::Bool => Ok(inferred),
            Inferred::Concrete(ty) => Err(self.error(
                codes::CONDITION,
                format!("this condition has type `{}`, not `bool`", ty_name(ty)),
                &span_of(condition),
                &["Lazen v1 has no truthiness: a condition must be a `bool`"],
                Some("compare explicitly, as in `if flag && total > 0`"),
                &[],
            )),
            Inferred::IntegerLiteral(_) => Err(self.error(
                codes::CONDITION,
                "this condition is an integer literal, not a `bool`",
                &span_of(condition),
                &["Lazen v1 has no truthiness: `0` is not false"],
                Some("compare explicitly, as in `if count > 0`"),
                &[],
            )),
            Inferred::Invalid => Ok(Inferred::Invalid),
        }
    }

    /// The integer type of an operand.
    ///
    /// A literal has already been given `expected` as its context and range-checked
    /// against it by `check_expression`, so it takes that type rather than the `i32`
    /// default. Defaulting it here would reject `f() == 0` for a `f() -> i64`.
    fn integer_operand(
        &self,
        inferred: Inferred,
        expected: &Type,
        span: &SourceSpan,
        what: &str,
    ) -> Result<Type, StageError> {
        match inferred {
            // A literal has already been given `expected` as its context and
            // range-checked against it, so it takes that type rather than the `i32`
            // default. Defaulting it here would reject `f() == 0` for an `f() -> i64`.
            Inferred::IntegerLiteral(_) => Ok(expected.clone()),
            Inferred::Concrete(ty) if ty.is_integer() => Ok(ty),
            Inferred::Concrete(ty) => Err(self.error(
                codes::MISMATCH,
                format!(
                    "{what} has type `{}`, but this operator needs an integer",
                    ty_name(&ty)
                ),
                span,
                &["Lazen v1 has no floating-point type and no operator overloading"],
                Some("use an integer, or an explicit `as` cast"),
                &[],
            )),
            Inferred::Invalid => Ok(expected.clone()),
        }
    }

    /// The integer type of an operand of `+ - * / %` or a comparison, or a
    /// clear type error.
    ///
    /// Neither is a range, so a non-integer operand is reported as the type error it
    /// is rather than as a bad iteration.
    fn integer_operand_or_error(
        &self,
        inferred: Inferred,
        span: &SourceSpan,
        what: &str,
    ) -> Result<Type, StageError> {
        match inferred.defaulted() {
            Inferred::Concrete(ty) if ty.is_integer() => Ok(ty),
            Inferred::Concrete(ty) => Err(self.mismatch(
                span,
                &ty,
                &Type::I32,
                &format!("{what} of a comparison must be an integer"),
            )),
            _ => Ok(Type::I32),
        }
    }

    fn require_integer(
        &self,
        inferred: Inferred,
        span: &SourceSpan,
        what: &str,
    ) -> Result<Type, StageError> {
        match inferred.defaulted() {
            Inferred::Concrete(ty) if ty.is_integer() => Ok(ty),
            Inferred::Concrete(ty) => Err(self.error(
                codes::NOT_A_RANGE,
                format!(
                    "{what} has type `{}`, which is not an integer",
                    ty_name(&ty)
                ),
                span,
                &["Lazen v1 ranges are integer ranges only"],
                Some("iterate over an integer range, or index a slice yourself"),
                &[],
            )),
            Inferred::IntegerLiteral(_) => Ok(Type::I32),
            Inferred::Invalid => Ok(Type::I32),
        }
    }

    // ------------------------------------------------------------- places

    fn check_place(
        &mut self,
        expression: &Expr,
        scope: &[Binding],
    ) -> Result<CheckedPlace, StageError> {
        match expression {
            Expr::Path { path, .. } if path.segments.len() == 1 => {
                let name = &path.segments[0].text;
                if let Some(binding) = scope.iter().rev().find(|binding| *binding.name == *name) {
                    return Ok(CheckedPlace::Local {
                        slot: binding.slot,
                        offset: binding.offset,
                        ty: binding.ty.clone(),
                        mutable: binding.mutable,
                        span: path.span.clone(),
                    });
                }
                // Not a local: it may be a constant, which is not a place.
                if let Some(found) = resolve::lookup_parts(self.resolved, &[name.as_str()])
                    && matches!(found.symbol, Symbol::Constant(_))
                {
                    return Err(self.error(
                        codes::NOT_A_PLACE,
                        format!("`{name}` is a `const`, and a `const` is not a place"),
                        &path.span,
                        &["a `const` is substituted at compile time and cannot be assigned"],
                        Some("use a `let mut` binding if you need to write it"),
                        &[],
                    ));
                }
                Err(self.unresolved_name(name, &path.span))
            }
            Expr::Index { base, index, span } => {
                // `values.as_mut_slice()[0] = 1` is a write into `values`, so the
                // view is resolved to the array it points at rather than being
                // treated as a temporary that cannot be written through.
                let base = match base.as_ref() {
                    Expr::MethodCall {
                        receiver,
                        method,
                        arguments,
                        ..
                    } if arguments.is_empty()
                        && matches!(method.text.as_str(), "as_slice" | "as_mut_slice") =>
                    {
                        if method.text == "as_mut_slice" && !self.place_is_mutable(receiver, scope)
                        {
                            return Err(self.error(
                                codes::BAD_BORROW,
                                "`as_mut_slice` needs a mutable array",
                                &method.span,
                                &["this array was not declared `mut`"],
                                Some("write `let mut values = ...;`"),
                                &[],
                            ));
                        }
                        receiver.as_ref()
                    }
                    other => other,
                };
                let base_place = self.check_place(base, scope)?;
                let base_type = base_place.ty_cloned();
                let (element, mutable) = match &base_type {
                    Type::Array { element, .. } => {
                        (element.as_ref().clone(), base_place.is_mutable())
                    }
                    // A `&mut [T]` parameter may be written through even though the
                    // parameter binding itself is not `mut`: it is the pointed-to
                    // data that the reference makes mutable.
                    Type::Slice { element, mutable } => (
                        element.as_ref().clone(),
                        *mutable || base_place.is_mutable(),
                    ),
                    Type::Str => (Type::U8, false),
                    other => {
                        return Err(self.error(
                            codes::MISMATCH,
                            format!("cannot index a value of type `{}`", ty_name(other)),
                            &span_of(base),
                            &["only arrays, slices, and `str` can be indexed"],
                            Some(if *other == Type::Str {
                                "index a string to read a byte"
                            } else {
                                "index an array or a slice"
                            }),
                            &[],
                        ));
                    }
                };
                let index_inferred = self.check_expression(index, scope, Some(Type::Usize))?;
                if let Inferred::Concrete(ty) = &index_inferred
                    && ty != &Type::Usize
                {
                    return Err(self.error(
                        codes::INDEX_TYPE,
                        format!("this index has type `{}`, not `usize`", ty_name(ty)),
                        &span_of(index),
                        &["Lazen v1 indexes with `usize`, because the index is a bound check"],
                        Some("cast the index explicitly, as in `values[i as usize]`"),
                        &[],
                    ));
                }
                let index_checked =
                    self.finalize(index, index_inferred, Some(Type::Usize), scope)?;
                Ok(CheckedPlace::Index {
                    base: Box::new(base_place),
                    index: Box::new(index_checked),
                    // The base address already points at element zero and the
                    // index is scaled by the element's size when the address is
                    // formed, so an element sits at offset zero within the base.
                    // Adding the element size here instead would push every
                    // element one stride past where it belongs.
                    element_offset: 0,
                    ty: element,
                    mutable,
                    span: span.clone(),
                })
            }
            Expr::Unary {
                operator: UnaryOp::Deref,
                operand,
                span,
            } => {
                let inferred = self.check_expression(operand, scope, None)?;
                let referenced = match inferred.defaulted() {
                    Inferred::Concrete(ty) => ty,
                    _ => Type::Unit,
                };
                let reference_type = referenced.clone();
                match referenced {
                    Type::Reference { pointee, mutable } => Ok(CheckedPlace::Deref {
                        reference: Box::new(self.finalize(
                            operand,
                            Inferred::Concrete(reference_type),
                            None,
                            scope,
                        )?),
                        ty: pointee.as_ref().clone(),
                        mutable,
                        span: span.clone(),
                    }),
                    Type::Pointer { .. } => Err(self.error(
                        codes::RAW_DEREF,
                        "Lazen v1 does not dereference a raw pointer",
                        span,
                        &[
                            "`ptr<T>` carries no length, so a dereference could not be bounds checked",
                            "Lazen v1 has no `unsafe` block in which such a read could be justified",
                        ],
                        Some("take a `&[T]` view of the memory and read through that"),
                        &[],
                    )),
                    other => Err(self.error(
                        codes::MISMATCH,
                        format!("cannot dereference a value of type `{}`", ty_name(&other)),
                        &span_of(operand),
                        &["`*` applies to a reference"],
                        Some("write `*reference`, or use `.as_ptr()` for an address"),
                        &[],
                    )),
                }
            }
            other => Err(self.error(
                codes::NOT_A_PLACE,
                "this expression cannot be assigned to",
                &span_of(other),
                &["a place is a variable, an index, or a dereference"],
                Some("assign to a `let mut` binding, an element, or a referent"),
                &[],
            )),
        }
    }

    /// Whether an expression names a place that may be written, ignoring an
    /// invalid one: a bad place is reported by the caller that checks it properly.
    /// Whether `expression` names a parameter of the function being checked.
    ///
    /// A parameter's slot belongs to the callee's own frame, so writing to it is
    /// invisible to the caller and needs no `mut`. This is the rule that lets
    /// `as_str(status)` work when `status` is a parameter, and it is the same rule
    /// that already lets a `&mut [T]` parameter be written through.
    fn argument_is_parameter(&self, expression: &Expr) -> bool {
        let Expr::Path { path, .. } = expression else {
            return false;
        };
        if path.segments.len() != 1 {
            return false;
        }
        self.current_locals
            .iter()
            .any(|slot| slot.is_parameter && slot.name == path.segments[0].text)
    }

    fn place_is_mutable(&self, expression: &Expr, scope: &[Binding]) -> bool {
        // A name that is not a local binding is not a mutable place; the caller
        // reports the real problem.
        self.place_mutability(expression, scope).unwrap_or(false)
    }

    fn place_mutability(&self, expression: &Expr, scope: &[Binding]) -> Option<bool> {
        match expression {
            Expr::Path { path, .. } if path.segments.len() == 1 => scope
                .iter()
                .rev()
                .find(|binding| binding.name == path.segments[0].text)
                .map(|binding| binding.mutable),
            _ => None,
        }
    }

    fn immutable_error(
        &self,
        target: &Expr,
        place: &CheckedPlace,
        scope: &[Binding],
    ) -> StageError {
        let reason = match place {
            CheckedPlace::Local { .. } => "this binding was declared without `mut`",
            CheckedPlace::Index { base, .. } => match base.ty() {
                Type::Slice { mutable: false, .. } => "this slice is an immutable `&[T]` view",
                _ => "the value being indexed is not `mut`",
            },
            CheckedPlace::Deref { .. } => "this reference is not `&mut`",
        };
        // Point at the `let` that made this immutable, so the fix is obvious.
        let mut extra = Vec::new();
        if let CheckedPlace::Local { slot, .. } = place
            && let Some(binding) = scope.iter().find(|binding| binding.slot == *slot)
        {
            extra.push((
                binding.span.clone(),
                format!("`{}` is declared here", binding.name),
            ));
        }
        self.error(
            codes::IMMUTABLE,
            "cannot assign to this place",
            &span_of(target),
            &[reason],
            Some("declare the binding with `let mut`, or take a `&mut` view"),
            &extra,
        )
    }

    // -------------------------------------------------------- expressions

    fn check_expression(
        &mut self,
        expression: &Expr,
        scope: &[Binding],
        expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        match expression {
            Expr::Int { literal, span } => {
                if let Some(suffix) = literal.suffix {
                    let ty = Type::from_suffix(suffix);
                    if let Some(expected) = &expected
                        && expected.is_integer()
                        && expected != &ty
                    {
                        return Err(self.mismatch(
                            span,
                            &ty,
                            expected,
                            "a literal suffix fixes the type, so it cannot differ from the type required here",
                        ));
                    }
                    if !Type::fits_literal(&ty, literal.value) {
                        return Err(self.literal_range_error(literal.value, &ty, span));
                    }
                    return Ok(Inferred::Concrete(ty));
                }
                if let Some(expected) = &expected {
                    if !expected.is_integer() {
                        return Err(self.mismatch(
                            span,
                            &Type::I32,
                            expected,
                            "an integer literal cannot be used where a non-integer is required",
                        ));
                    }
                    if !Type::fits_literal(expected, literal.value) {
                        return Err(self.literal_range_error(literal.value, expected, span));
                    }
                }
                Ok(Inferred::IntegerLiteral(literal.value))
            }
            Expr::Str { span, .. } => {
                // A literal must still be the type the context asked for. This is
                // what rejects `[1, "two"]` and `if 1 { }`.
                if let Some(expected) = &expected
                    && expected != &Type::Str
                {
                    return Err(self.mismatch(
                        span,
                        &Type::Str,
                        expected,
                        "a string literal cannot be used where a non-string is required",
                    ));
                }
                Ok(Inferred::Concrete(Type::Str))
            }
            Expr::Bool { span, .. } => {
                if let Some(expected) = &expected
                    && expected != &Type::Bool
                {
                    return Err(self.mismatch(
                        span,
                        &Type::Bool,
                        expected,
                        "a `bool` literal cannot be used where a non-`bool` is required",
                    ));
                }
                Ok(Inferred::Concrete(Type::Bool))
            }
            Expr::Path { path, .. } => self.check_path(expression, path, scope, expected),
            Expr::Call {
                callee,
                arguments,
                span,
            } => self.check_call(expression, callee, arguments, span, scope, expected),
            Expr::MethodCall {
                receiver,
                method,
                arguments,
                span,
            } => self.check_method_call(expression, receiver, method, arguments, span, scope),
            Expr::Index { .. } => {
                let place = self.check_place(expression, scope)?;
                let ty = place.ty_cloned();
                Ok(Inferred::Concrete(ty))
            }
            Expr::Unary {
                operator,
                operand,
                span,
            } => self.check_unary(expression, *operator, operand, span, scope, expected),
            Expr::Binary {
                operator,
                left,
                right,
                span,
            } => self.check_binary(expression, *operator, left, right, span, scope, expected),
            Expr::Cast {
                operand,
                target,
                span,
            } => {
                let to = self.type_of(target)?;
                let inferred = self.check_expression(operand, scope, None)?;
                if let Inferred::IntegerLiteral(value) = inferred {
                    self.check_literal_fits(value, &to, &span_of(operand))?;
                }
                let from = inferred.defaulted();
                let from_type = from.concrete().cloned().unwrap_or(Type::I32);
                if !cast_is_allowed(&from_type, &to) {
                    return Err(self.invalid_cast(span, &from_type, &to));
                }
                Ok(Inferred::Concrete(to))
            }
            Expr::Array { elements, span } => {
                let first = elements.first().ok_or_else(|| {
                    self.error(
                        codes::EXPECTED_ARRAY,
                        "an array literal needs at least one element",
                        span,
                        &[],
                        Some("write `[0u8; 16]` for a repeated array"),
                        &[],
                    )
                })?;
                let inferred = self.check_expression(first, scope, expected.clone())?;
                let element = self.resolve_inferred(
                    inferred,
                    expected.as_ref().and_then(|ty| ty.element().cloned()),
                    &span_of(first),
                    "this array's element",
                )?;
                for element_expr in &elements[1..] {
                    let element_inferred =
                        self.check_expression(element_expr, scope, Some(element.clone()))?;
                    self.finalize(element_expr, element_inferred, Some(element.clone()), scope)?;
                }
                Ok(Inferred::Concrete(Type::Array {
                    element: Box::new(element),
                    length: elements.len() as u64,
                }))
            }
            Expr::ArrayRepeat { value, count, span } => {
                let element = match expected.as_ref().and_then(|ty| ty.element().cloned()) {
                    Some(element) => element,
                    None => {
                        let inferred = self.check_expression(value, scope, None)?;
                        self.resolve_inferred(
                            inferred,
                            None,
                            &span_of(value),
                            "this array's element",
                        )?
                    }
                };
                let value_inferred = self.check_expression(value, scope, Some(element.clone()))?;
                self.finalize(value, value_inferred, Some(element.clone()), scope)?;
                let count_inferred = self.check_expression(count, scope, Some(Type::Usize))?;
                if let Inferred::Concrete(ty) = &count_inferred
                    && ty != &Type::Usize
                {
                    return Err(self.error(
                        codes::INDEX_TYPE,
                        format!(
                            "an array repeat count has type `{}`, not `usize`",
                            ty_name(ty)
                        ),
                        &span_of(count),
                        &["the count says how many elements to write"],
                        Some("write a `usize` count"),
                        &[],
                    ));
                }
                let length = match count_inferred {
                    Inferred::IntegerLiteral(value) => u64::try_from(value).unwrap_or(u64::MAX),
                    _ => u64::MAX,
                };
                if length == 0 {
                    return Err(self.error(
                        codes::ARRAY_TOO_LARGE,
                        "an array of length 0 has no type to infer",
                        span,
                        &[],
                        Some("use at least 1 element"),
                        &[],
                    ));
                }
                let bytes = array_bytes(length, element.size_in_bytes(self.target.word));
                if bytes > u128::from(u32::MAX) {
                    return Err(self.error(
                        codes::ARRAY_TOO_LARGE,
                        "this array is too large for a frame",
                        span,
                        &[],
                        Some("use a smaller array, or a slice"),
                        &[],
                    ));
                }
                Ok(Inferred::Concrete(Type::Array {
                    element: Box::new(element),
                    length,
                }))
            }
            Expr::If { arms, span } => {
                let mut result = expected.clone();
                let mut checked_conditions = Vec::new();
                for arm in arms {
                    let condition = match &arm.condition {
                        Some(condition) => {
                            let inferred = self.check_condition(condition, scope)?;
                            Some(self.finalize(condition, inferred, Some(Type::Bool), scope)?)
                        }
                        None => None,
                    };
                    checked_conditions.push(condition);
                }
                let mut arm_types = Vec::new();
                for arm in arms {
                    // The type a conditional used as a value produces is the type
                    // of its arms' tails. When the context has not said what it
                    // is, the first arm's tail decides it, and the other arms are
                    // checked against that. Checking the first arm as a unit
                    // block instead would reject every value conditional, because
                    // its tail is a value and not nothing.
                    let mut block_expected = result.clone().unwrap_or(Type::Unit);
                    if result.is_none()
                        && let Some(tail) = &arm.body.tail
                    {
                        let inferred = self.check_expression(tail, scope, None)?;
                        block_expected = self.resolve_inferred(
                            inferred,
                            None,
                            &span_of(tail),
                            "this conditional's value",
                        )?;
                    }
                    // A value conditional has no frame of its own in Step 61, so
                    // an arm may not bind a name: there would be no slot for it.
                    if let Some(binding) = arm
                        .body
                        .statements
                        .iter()
                        .find(|statement| matches!(statement, Stmt::Let { .. }))
                    {
                        return Err(self.error(
                            codes::ARM_BINDING,
                            "a conditional used as a value may not bind a name",
                            binding.span(),
                            &["Step 61 gives a value conditional no frame of its own"],
                            Some("bind the name before the conditional"),
                            &[],
                        ));
                    }
                    let block = self.check_block(
                        &arm.body,
                        &mut scope.to_vec(),
                        &mut Vec::new(),
                        &mut 0,
                        block_expected,
                        self.loop_depth,
                    )?;
                    match block.tail {
                        Some(tail) => {
                            let ty = tail.ty();
                            result = Some(match result.clone() {
                                Some(previous) => {
                                    if previous != ty && previous != Type::Unit {
                                        return Err(self.mismatch(
                                            span,
                                            &ty,
                                            &previous,
                                            "every arm of a conditional used as a value must produce the same type",
                                        ));
                                    }
                                    ty.clone()
                                }
                                None => ty.clone(),
                            });
                            arm_types.push(Some(ty));
                        }
                        None => arm_types.push(None),
                    }
                }
                if arm_types.iter().any(|ty| ty.is_none()) && !arm_types.is_empty() {
                    // A conditional used as a value must produce a value in
                    // every arm.
                    if result.is_some() {
                        return Err(self.error(
                            codes::MISMATCH,
                            "this conditional is used as a value, so every arm must produce one",
                            span,
                            &["an arm without a tail expression produces nothing"],
                            Some("end each arm with an expression, or use the `if` as a statement"),
                            &[],
                        ));
                    }
                }
                Ok(match result {
                    Some(ty) => Inferred::Concrete(ty),
                    None => Inferred::Concrete(Type::Unit),
                })
            }
            Expr::Block { block, .. } => {
                let checked = self.check_block(
                    block,
                    &mut scope.to_vec(),
                    &mut Vec::new(),
                    &mut 0,
                    expected.unwrap_or(Type::Unit),
                    self.loop_depth,
                )?;
                match checked.tail {
                    Some(tail) => Ok(Inferred::Concrete(tail.ty())),
                    None => Ok(Inferred::Concrete(Type::Unit)),
                }
            }
        }
    }

    fn check_path(
        &mut self,
        expression: &Expr,
        path: &crate::ast::Path,
        scope: &[Binding],
        _expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        let _ = expression;
        if path.segments.len() == 1 {
            let name = &path.segments[0].text;
            if let Some(binding) = scope.iter().rev().find(|binding| *binding.name == *name) {
                return Ok(Inferred::Concrete(binding.ty.clone()));
            }
        }
        let Some(found) = self.lookup_path(path) else {
            return Err(self.unresolved_path(path));
        };
        if !found.visible {
            return Err(self.error(
                crate::resolve::codes::PRIVATE,
                format!("`{}` is private to its module", path_name(path)),
                &path.span,
                &["every item used from another module must be `pub`"],
                Some("write `pub` on the item, or use it inside its own module"),
                &[],
            ));
        }
        match found.symbol {
            Symbol::Function(function) => {
                Err(self.error(
                    codes::NOT_A_VALUE,
                    format!("`{}` is a function, and a function is not a value", function.name),
                    &path.span,
                    &["Lazen v1 has no function types as values, so there are no closures or callbacks"],
                    Some("call the function instead"),
                    &[],
                ))
            }
            Symbol::Constant(constant) => {
                Ok(Inferred::Concrete(self.constant_type(constant)?))
            }
            Symbol::Extern(_) => Err(self.error(
                codes::NOT_A_VALUE,
                format!("`{}` is an `extern` declaration, and it is not a value", path_name(path)),
                &path.span,
                &["an `extern \"syscall\"` declaration names a call, not a value"],
                Some("call the syscall instead"),
                &[],
            )),
            Symbol::Module(_) => Err(self.error(
                crate::resolve::codes::NOT_A_MODULE,
                format!("`{}` is a module, not a value", path_name(path)),
                &path.span,
                &["a module is a namespace, not a value"],
                Some("name an item inside the module, as in `module::item`"),
                &[],
            )),
        }
    }

    /// Resolves a path from the module the current function belongs to.
    fn lookup_path(&self, path: &crate::ast::Path) -> Option<resolve::Found<'a>> {
        let segments: Vec<&str> = path
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect();
        resolve::lookup_from(self.resolved, &self.current_module, &segments)
    }

    fn constant_type(&self, constant: &ResolvedConstant) -> Result<Type, StageError> {
        match &constant.annotation {
            Some(annotation) => self.type_of(annotation),
            None => match &constant.value {
                Expr::Str { .. } => Ok(Type::Str),
                Expr::Bool { .. } => Ok(Type::Bool),
                Expr::Int { literal, .. } => Ok(match literal.suffix {
                    Some(suffix) => Type::from_suffix(suffix),
                    None => Type::I32,
                }),
                _ => Ok(Type::I32),
            },
        }
    }

    fn check_call(
        &mut self,
        expression: &Expr,
        callee: &Expr,
        arguments: &[Expr],
        span: &SourceSpan,
        scope: &[Binding],
        _expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        let _ = expression;
        let Expr::Path { path, .. } = callee else {
            return Err(self.error(
                codes::NOT_CALLABLE,
                "only a named function or syscall can be called",
                &span_of(callee),
                &["Lazen v1 has no function values, so a call's callee is always a name"],
                Some("call a function by name, as in `square(7)`"),
                &[],
            ));
        };
        // A single-segment name may be a local, which is not callable.
        if path.segments.len() == 1 {
            let name = &path.segments[0].text;
            if scope.iter().any(|binding| binding.name == *name) {
                return Err(self.error(
                    codes::NOT_CALLABLE,
                    format!("`{name}` is a local, not a function"),
                    &span_of(callee),
                    &["Lazen v1 has no function types as values"],
                    Some("call a function by name"),
                    &[],
                ));
            }
        }
        let Some(found) = self.lookup_path(path) else {
            return Err(self.unresolved_path(path));
        };
        if !found.visible {
            return Err(self.error(
                crate::resolve::codes::PRIVATE,
                format!("`{}` is private to its module", path_name(path)),
                &path.span,
                &["every item used from another module must be `pub`"],
                Some("write `pub` on the item, or call it inside its own module"),
                &[],
            ));
        }
        let (parameters, result, is_extern) = match found.symbol {
            Symbol::Function(function) => {
                let mut parameters = Vec::new();
                for parameter in &function.parameters {
                    parameters.push((parameter.name.clone(), self.type_of(&parameter.annotation)?));
                }
                let result = match &function.result {
                    Some(annotation) => self.type_of(annotation)?,
                    None => Type::Unit,
                };
                (parameters, result, false)
            }
            Symbol::Extern(declaration) => {
                let mut parameters = Vec::new();
                for parameter in &declaration.parameters {
                    parameters.push((parameter.name.clone(), self.type_of(&parameter.annotation)?));
                }
                (parameters, self.type_of(&declaration.result)?, true)
            }
            Symbol::Constant(_) | Symbol::Module(_) => {
                return Err(self.error(
                    codes::NOT_CALLABLE,
                    format!("`{}` is not a function", path_name(path)),
                    &span_of(callee),
                    &["only `fn` and `extern \"syscall\"` items can be called"],
                    Some("call a function"),
                    &[],
                ));
            }
        };
        if parameters.len() != arguments.len() {
            return Err(self.error(
                codes::ARITY,
                format!(
                    "`{}` takes {} argument{}, but {} {} given",
                    path_name(path),
                    parameters.len(),
                    if parameters.len() == 1 { "" } else { "s" },
                    arguments.len(),
                    if arguments.len() == 1 { "was" } else { "were" }
                ),
                span,
                &["the call's arguments must match the declaration exactly"],
                Some(format!("write `{}`", path_name(path)).as_str()),
                &[],
            ));
        }
        for (index, argument) in arguments.iter().enumerate() {
            let (name, expected) = &parameters[index];
            let inferred = self.check_expression(argument, scope, Some(expected.clone()))?;
            if let Inferred::Concrete(actual) = &inferred
                && actual != expected
            {
                let mut labels = Vec::new();
                if let Some(span) = self.find_parameter(found.module.path.as_slice(), name) {
                    labels.push((span, format!("this parameter is `{}`", ty_name(expected))));
                }
                return Err(self.error(
                    codes::ARGUMENT,
                    format!(
                        "this argument has type `{}`, but `{}` takes `{}`",
                        ty_name(actual),
                        path_name(path),
                        ty_name(expected)
                    ),
                    &span_of(argument),
                    &[],
                    Some("cast the argument explicitly, or change the declaration"),
                    &labels,
                ));
            }
            self.finalize(argument, inferred, Some(expected.clone()), scope)?;
        }
        let _ = is_extern;
        Ok(Inferred::Concrete(result))
    }

    fn find_parameter(&self, module: &[String], name: &str) -> Option<SourceSpan> {
        for item in self.resolved.all_modules() {
            for symbol in item.items.values() {
                if let Symbol::Function(function) = symbol {
                    if function.module != module {
                        continue;
                    }
                    for parameter in &function.parameters {
                        if parameter.name == name {
                            return Some(parameter.span.clone());
                        }
                    }
                }
                if let Symbol::Extern(declaration) = symbol {
                    if declaration.module != module {
                        continue;
                    }
                    for parameter in &declaration.parameters {
                        if parameter.name == name {
                            return Some(parameter.span.clone());
                        }
                    }
                }
            }
        }
        None
    }

    fn check_method_call(
        &mut self,
        _expression: &Expr,
        receiver: &Expr,
        method: &crate::ast::Name,
        arguments: &[Expr],
        span: &SourceSpan,
        scope: &[Binding],
    ) -> Result<Inferred, StageError> {
        let receiver_inferred = self.check_expression(receiver, scope, None)?;
        let receiver_type = receiver_inferred.defaulted();
        let Some(receiver_type) = receiver_type.concrete().cloned() else {
            return Err(self.error(
                codes::UNKNOWN_METHOD,
                "this receiver has no type to call a method on",
                &span_of(receiver),
                &[],
                Some("bind the value with an explicit type first"),
                &[],
            ));
        };
        for argument in arguments {
            self.check_expression(argument, scope, None)?;
        }
        let name = method.text.as_str();
        let ty = match (name, &receiver_type) {
            ("len", Type::Str) => Type::Usize,
            ("len", Type::Array { .. }) | ("len", Type::Slice { .. }) => Type::Usize,
            ("as_bytes", Type::Str) => Type::Slice {
                element: Box::new(Type::U8),
                mutable: false,
            },
            ("as_slice", Type::Array { element, .. }) => Type::Slice {
                element: element.clone(),
                mutable: false,
            },
            // `as_mut_slice` on an already mutable view is the identity, which is what
            // the syntax document writes when it has a `&mut [T]` parameter.
            (
                "as_mut_slice",
                Type::Slice {
                    element,
                    mutable: true,
                },
            ) => Type::Slice {
                element: element.clone(),
                mutable: true,
            },
            ("as_mut_slice", Type::Array { element, .. }) => {
                let place = self.check_place(receiver, scope);
                let mutable = place
                    .as_ref()
                    .map(CheckedPlace::is_mutable)
                    .unwrap_or(false);
                if !mutable {
                    return Err(self.error(
                        codes::BAD_BORROW,
                        "`as_mut_slice` needs a mutable array",
                        &method.span,
                        &["this array was not declared `mut`"],
                        Some("write `let mut values = ...;`"),
                        &[],
                    ));
                }
                Type::Slice {
                    element: element.clone(),
                    mutable: true,
                }
            }
            ("as_ptr", Type::Array { element, .. } | Type::Slice { element, .. }) => {
                Type::Pointer {
                    pointee: element.clone(),
                }
            }
            // A str is a `&[u8]` view, so its data address is the same operation.
            ("as_ptr", Type::Str) => Type::Pointer {
                pointee: Box::new(Type::U8),
            },
            // A view over memory named by an address. This is how a program turns
            // an address the OS gave it — an allocated block, a buffer it laid out
            // in its own frame — into something it can index. It is the *only*
            // route from an address to a view, because `ptr<T>` deliberately
            // cannot be dereferenced: a pointer carries no length, so a read
            // through one could not be bounds checked, and v1 has no `unsafe`.
            // The length is the caller's to state, and stating it wrong is the one
            // mistake this can make, so the name says so.
            ("slice_from_raw", Type::Pointer { pointee }) => {
                if arguments.len() != 1 {
                    return Err(self.error(
                        codes::UNKNOWN_METHOD,
                        format!(
                            "`slice_from_raw` takes one length, and {} were given",
                            arguments.len()
                        ),
                        &method.span,
                        &["write `address.slice_from_raw(length)`"],
                        None,
                        &[],
                    ));
                }
                Type::Slice {
                    element: pointee.clone(),
                    mutable: false,
                }
            }
            // The mutable form. The caller is stating that the memory really is
            // writable, which is the same claim `&mut` makes anywhere else.
            ("slice_from_raw_mut", Type::Pointer { pointee }) => {
                if arguments.len() != 1 {
                    return Err(self.error(
                        codes::UNKNOWN_METHOD,
                        format!(
                            "`slice_from_raw_mut` takes one length, and {} were given",
                            arguments.len()
                        ),
                        &method.span,
                        &["write `address.slice_from_raw_mut(length)`"],
                        None,
                        &[],
                    ));
                }
                Type::Slice {
                    element: pointee.clone(),
                    mutable: true,
                }
            }
            // A `str` over the bytes of a slice, if the bytes are valid UTF-8.
            //
            // This is a *checked* conversion and the check is the point. A cast
            // cannot be it, because a `&[u8]` of arbitrary bytes is not a `str` and
            // the language has no way to say "I checked": `docs/lazen-types.md`
            // lists `str` as a *checked* UTF-8 byte string for exactly this reason.
            // Here the check lives, so every `str` in the language is one that was
            // verified — whether it came from a literal, which the compiler checks,
            // or from this, which checks at run time.
            ("as_str", Type::Slice { element, .. }) => {
                if !matches!(element.as_ref(), Type::U8) {
                    return Err(self.error(
                        codes::UNKNOWN_METHOD,
                        format!("`as_str` needs a `&[u8]`, not a `&[{}]`", ty_name(element)),
                        &method.span,
                        &["only a byte slice is a candidate for text"],
                        None,
                        &[],
                    ));
                }
                if arguments.len() != 1 {
                    return Err(self.error(
                        codes::UNKNOWN_METHOD,
                        format!(
                            "`as_str` takes one status out-parameter, and {} were given",
                            arguments.len()
                        ),
                        &method.span,
                        &["write `bytes.as_str(&mut ok)`"],
                        None,
                        &[],
                    ));
                }
                // The status is written through, so it has to be a writable
                // `bool` place. Accepting an expression that is not one would let
                // a program ask whether bytes are text and have nowhere to be told.
                let status_inferred =
                    self.check_expression(&arguments[0], scope, Some(Type::Bool))?;
                match status_inferred.concrete() {
                    Some(Type::Bool) => {}
                    Some(other) => {
                        return Err(self.error(
                            codes::MISMATCH,
                            format!("`as_str` writes a `bool` status, not `{}`", ty_name(other)),
                            &span_of(&arguments[0]),
                            &["the status says whether the bytes are valid UTF-8"],
                            Some("pass a `bool` you can write to"),
                            &[],
                        ));
                    }
                    None => {
                        return Err(self.error(
                            codes::MISMATCH,
                            "this `as_str` status has no type to write to",
                            &span_of(&arguments[0]),
                            &["the status is written by the conversion, so it must be a place"],
                            Some("bind a `bool` first, as in `let mut ok: bool = false;`"),
                            &[],
                        ));
                    }
                }
                // The status is written through, so it has to be a place this
                // function may write. A local needs `mut`; a *parameter* does not,
                // because a parameter's slot is this function's own frame and
                // writing there is local. That is the same rule `&mut [T]`
                // parameters already follow, and for the same reason: the frame is
                // the callee's, so a write cannot be seen by the caller.
                match self.check_place(&arguments[0], scope) {
                    Ok(CheckedPlace::Local { mutable, span, .. })
                        if !mutable && !self.argument_is_parameter(&arguments[0]) =>
                    {
                        return Err(self.error(
                            codes::BAD_BORROW,
                            "`as_str` needs a status it can write to",
                            &span,
                            &["this binding was not declared `mut`"],
                            Some("write `let mut ok: bool = false;`"),
                            &[],
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => return Err(error),
                }
                Type::Str
            }
            _ => {
                return Err(self.error(
                    codes::UNKNOWN_METHOD,
                    format!("`{}` has no method `{}`", ty_name(&receiver_type), name),
                    &method.span,
                    &["Lazen v1 has exactly these builtin methods: `len`, `as_bytes`, `as_slice`, `as_mut_slice`, `as_ptr`, `slice_from_raw`, `slice_from_raw_mut`, and `as_str`"],
                    Some(format!(
                        "valid methods for `{}`: `{}`",
                        ty_name(&receiver_type),
                        methods_for(&receiver_type)
                    )
                    .as_str()),
                    &[],
                ));
            }
        };
        // The three builtins that take an argument: the two view-from-address forms,
        // which take a length, and `as_str`, which takes the status to write. Every
        // other builtin answers a question about its receiver alone, which is why
        // the general rule below is "no arguments" rather than an arity table.
        if !arguments.is_empty()
            && !matches!(name, "slice_from_raw" | "slice_from_raw_mut" | "as_str")
        {
            return Err(self.error(
                codes::UNKNOWN_METHOD,
                format!("`{name}` takes no arguments"),
                span,
                &["Lazen v1 has no methods with parameters, and no traits"],
                Some("call the method with no arguments"),
                &[],
            ));
        }
        Ok(Inferred::Concrete(ty))
    }

    fn check_unary(
        &mut self,
        expression: &Expr,
        operator: UnaryOp,
        operand: &Expr,
        span: &SourceSpan,
        scope: &[Binding],
        expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        match operator {
            UnaryOp::Negate => {
                // A negated literal is decided here rather than through the operand,
                // because `-128i8` is the minimum value and `128i8` on its own is not
                // a valid `i8`.
                if let Expr::Int { literal, span } = operand {
                    let ty = match literal.suffix {
                        Some(suffix) => Type::from_suffix(suffix),
                        None => expected
                            .clone()
                            .filter(|ty| ty.is_integer())
                            .unwrap_or(Type::I32),
                    };
                    if !Type::fits_signed_literal(&ty, -(literal.value as i128)) {
                        return Err(self.literal_range_error_negated(
                            -(literal.value as i128),
                            &ty,
                            span,
                        ));
                    }
                    return Ok(Inferred::Concrete(ty));
                }
                let inferred = self.check_expression(operand, scope, expected.clone())?;
                // A negated literal is checked as the negative value it will be, so
                // `-1u8` is an error and `-128i8` is not.
                if let Inferred::IntegerLiteral(value) = &inferred {
                    let signed = -(*value as i128);
                    let ty = expected
                        .clone()
                        .filter(|ty| ty.is_integer())
                        .or_else(|| inferred.concrete().cloned())
                        .unwrap_or(Type::I32);
                    if !Type::fits_signed_literal(&ty, signed) {
                        return Err(self.literal_range_error_negated(
                            signed,
                            &ty,
                            &span_of(operand),
                        ));
                    }
                }
                match inferred.defaulted() {
                    Inferred::Concrete(ty) if ty.is_integer() => Ok(Inferred::Concrete(ty)),
                    Inferred::Concrete(ty) => Err(self.error(
                        codes::MISMATCH,
                        format!("cannot negate a value of type `{}`", ty_name(&ty)),
                        &span_of(operand),
                        &["`-` applies to integers in Lazen v1; there are no floats"],
                        Some("use a signed integer, or subtract instead"),
                        &[],
                    )),
                    _ => Ok(Inferred::IntegerLiteral(0)),
                }
            }
            UnaryOp::Not => {
                let inferred = self.check_expression(operand, scope, Some(Type::Bool))?;
                if let Inferred::Concrete(ty) = &inferred
                    && ty != &Type::Bool
                {
                    return Err(self.error(
                        codes::MISMATCH,
                        format!("`!` needs a `bool`, not `{}`", ty_name(ty)),
                        &span_of(operand),
                        &["`!` is logical negation in Lazen v1"],
                        Some("compare explicitly instead of negating a truthiness"),
                        &[],
                    ));
                }
                Ok(Inferred::Concrete(Type::Bool))
            }
            UnaryOp::Deref => {
                // A dereference is always a place, so it is checked as one.
                let place = self.check_place(expression, scope)?;
                let ty = place.ty_cloned();
                Ok(Inferred::Concrete(ty))
            }
            UnaryOp::Address | UnaryOp::AddressMut => {
                let mutable = operator == UnaryOp::AddressMut;
                // A str is already a view, so `&s` and `&str` are the same type.
                if !mutable
                    && let Expr::Path { path, .. } = operand
                    && path.segments.len() == 1
                    && let Some(binding) = scope
                        .iter()
                        .rev()
                        .find(|binding| binding.name == path.segments[0].text)
                    && binding.ty == Type::Str
                {
                    return Ok(Inferred::Concrete(Type::Str));
                }
                let place = self.check_place(operand, scope)?;
                let ty = place.ty_cloned();
                if matches!(ty, Type::Array { .. }) {
                    return Err(self.error(
                        codes::BAD_BORROW,
                        "an array cannot be borrowed as a whole in Lazen v1",
                        span,
                        &["a reference to an array would have no length, so a bound check could not be done"],
                        Some("write `values.as_slice()`, or `values.as_mut_slice()` for a mutable view"),
                        &[],
                    ));
                }
                if mutable && !place.is_mutable() {
                    return Err(self.error(
                        codes::BAD_BORROW,
                        "cannot take a mutable reference to this value",
                        span,
                        &["this binding was declared without `mut`"],
                        Some("write `let mut`, or borrow immutably with `&`"),
                        &[],
                    ));
                }
                Ok(Inferred::Concrete(Type::Reference {
                    pointee: Box::new(ty),
                    mutable,
                }))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_binary(
        &mut self,
        _expression: &Expr,
        operator: BinaryOp,
        left: &Expr,
        right: &Expr,
        span: &SourceSpan,
        scope: &[Binding],
        _expected: Option<Type>,
    ) -> Result<Inferred, StageError> {
        match operator {
            BinaryOp::And | BinaryOp::Or => {
                // Each operand is checked on its own first, so a non-`bool` operand is
                // reported as the truthiness mistake it is. A literal takes its default
                // here, which is what makes `1 && true` a truthiness error.
                let left_inferred = self.check_expression(left, scope, None)?;
                let left_type = left_inferred.defaulted();
                if let Some(ty) = left_type.concrete()
                    && ty != &Type::Bool
                {
                    return Err(self.condition_error(left, ty, operator.text()));
                }
                let right_inferred = self.check_expression(right, scope, None)?;
                let right_type = right_inferred.defaulted();
                if let Some(ty) = right_type.concrete()
                    && ty != &Type::Bool
                {
                    return Err(self.condition_error(right, ty, operator.text()));
                }
                Ok(Inferred::Concrete(Type::Bool))
            }
            BinaryOp::Compare(comparison) => {
                let left_inferred = self.check_expression(left, scope, None)?;
                let left_type = self.integer_operand_or_error(
                    left_inferred,
                    &span_of(left),
                    "the left operand",
                )?;
                let right_inferred =
                    self.check_expression(right, scope, Some(left_type.clone()))?;
                let right_type = self.integer_operand(
                    right_inferred,
                    &left_type,
                    &span_of(right),
                    "the right operand",
                )?;
                if left_type != right_type {
                    return Err(self.mismatch(
                        &span_of(right),
                        &right_type,
                        &left_type,
                        &format!(
                            "the two operands of `{}` must have the same type",
                            comparison_text(comparison)
                        ),
                    ));
                }
                Ok(Inferred::Concrete(Type::Bool))
            }
            BinaryOp::Arith(arith) => {
                let left_inferred = self.check_expression(left, scope, None)?;
                let left_type = self.integer_operand_or_error(
                    left_inferred,
                    &span_of(left),
                    "the left operand",
                )?;
                let right_inferred =
                    self.check_expression(right, scope, Some(left_type.clone()))?;
                let right_type = self.integer_operand(
                    right_inferred,
                    &left_type,
                    &span_of(right),
                    "the right operand",
                )?;
                if left_type != right_type {
                    return Err(self.mismatch(
                        &span_of(right),
                        &right_type,
                        &left_type,
                        &format!(
                            "the two operands of `{}` must have the same type",
                            arith_text(arith)
                        ),
                    ));
                }
                let _ = span;
                Ok(Inferred::Concrete(left_type))
            }
        }
    }

    // ------------------------------------------------------------- helpers

    /// Decides a literal's type, or reports why it cannot have one.
    fn resolve_inferred(
        &self,
        inferred: Inferred,
        declared: Option<Type>,
        span: &SourceSpan,
        what: &str,
    ) -> Result<Type, StageError> {
        match (declared, inferred) {
            (Some(declared), Inferred::Concrete(actual)) => {
                if declared != actual {
                    Err(self.mismatch(
                        span,
                        &actual,
                        &declared,
                        &format!("{what} does not have its declared type"),
                    ))
                } else {
                    Ok(declared)
                }
            }
            (Some(declared), Inferred::IntegerLiteral(value)) => {
                self.check_literal_fits(value, &declared, span)?;
                Ok(declared)
            }
            (None, Inferred::Concrete(actual)) => Ok(actual),
            (None, Inferred::IntegerLiteral(_)) => Ok(Type::I32),
            (_, Inferred::Invalid) => Ok(Type::Unit),
        }
    }

    fn check_literal_fits(
        &self,
        value: u128,
        ty: &Type,
        span: &SourceSpan,
    ) -> Result<(), StageError> {
        if !ty.is_integer() {
            return Err(self.mismatch(
                span,
                &Type::I32,
                ty,
                "an integer literal cannot be used where a non-integer is required",
            ));
        }
        if Type::fits_literal(ty, value) {
            Ok(())
        } else {
            Err(self.literal_range_error(value, ty, span))
        }
    }

    fn literal_range_error_negated(&self, value: i128, ty: &Type, span: &SourceSpan) -> StageError {
        self.error(
            codes::LITERAL_RANGE,
            format!("this negated literal does not fit in `{}`", ty_name(ty)),
            span,
            &[format!("`{value}` is outside the range of `{}`", ty_name(ty)).as_str()],
            Some("a literal that does not fit is an error, never a silent truncation"),
            &[],
        )
    }

    fn literal_range_error(&self, value: u128, ty: &Type, span: &SourceSpan) -> StageError {
        self.error(
            codes::LITERAL_RANGE,
            format!("this literal does not fit in `{}`", ty_name(ty)),
            span,
            &[format!("`{value}` is outside the range of `{}`", ty_name(ty)).as_str()],
            Some("a literal that does not fit is an error, never a silent truncation"),
            &[],
        )
    }

    fn mismatch(
        &self,
        span: &SourceSpan,
        actual: &Type,
        expected: &Type,
        reason: &str,
    ) -> StageError {
        self.error(
            codes::MISMATCH,
            format!(
                "expected `{}`, found `{}`",
                ty_name(expected),
                ty_name(actual)
            ),
            span,
            &[reason],
            Some("Lazen v1 has no implicit conversions; write an explicit `as` cast"),
            &[],
        )
    }

    fn condition_error(&self, expression: &Expr, ty: &Type, operator: &str) -> StageError {
        self.error(
            codes::CONDITION,
            format!(
                "`{operator}` needs `bool` operands, found `{}`",
                ty_name(ty)
            ),
            &span_of(expression),
            &["Lazen v1 has no truthiness"],
            Some("compare explicitly, as in `total > 0`"),
            &[],
        )
    }

    fn invalid_cast(&self, span: &SourceSpan, from: &Type, to: &Type) -> StageError {
        self.error(
            codes::INVALID_CAST,
            format!(
                "Lazen v1 does not allow casting `{}` to `{}`",
                ty_name(from),
                ty_name(to)
            ),
            span,
            &["Lazen v1 allows casts between integer types, between a reference and `ptr<T>`, and between `ptr<T>` and an integer"],
            Some("reinterpret the value explicitly, one step at a time"),
            &[],
        )
    }

    fn unresolved_name(&self, name: &str, span: &SourceSpan) -> StageError {
        self.error(
            crate::resolve::codes::UNRESOLVED,
            format!("`{name}` is not defined"),
            span,
            &["Lazen v1 has no implicit globals: every name is a parameter, a `let` binding, a `const`, or a path to an item"],
            Some("declare it with `let`, or spell its full path"),
            &[],
        )
    }

    fn unresolved_path(&self, path: &crate::ast::Path) -> StageError {
        let name = path_name(path);
        self.error(
            crate::resolve::codes::UNRESOLVED,
            format!("`{name}` does not name anything"),
            &path.span,
            &["Lazen v1 modules are declared with `mod`, and their items are reached with `::`"],
            Some("check the spelling, or declare the module or item"),
            &[],
        )
    }

    /// Turns a checked expression into its final node, discarding the inferred
    /// wrapper. This is where a literal's decided type is recorded.
    fn finalize(
        &mut self,
        expression: &Expr,
        inferred: Inferred,
        expected: Option<Type>,
        scope: &[Binding],
    ) -> Result<CheckedExpr, StageError> {
        // The context is checked here as well as in `check_expression`, because
        // this is the one place every expression passes through: a call whose
        // result is discarded as the wrong type is caught even though nothing
        // looked at it in particular.
        if let (Some(expected), Inferred::Concrete(actual)) = (&expected, &inferred)
            && expected != actual
            && !matches!(expression, Expr::Block { .. })
        {
            return Err(self.mismatch(
                &span_of(expression),
                actual,
                expected,
                "this expression does not have the type required here",
            ));
        }
        let expected = expected.or_else(|| inferred.concrete().cloned());
        Ok(match (expression, inferred) {
            (Expr::Int { literal, span }, _) => {
                let ty = expected.unwrap_or(Type::I32);
                CheckedExpr::Integer {
                    value: literal.value,
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::Str { value, span }, _) => {
                let index = self.intern_string(value);
                CheckedExpr::Str {
                    index,
                    ty: Type::Str,
                    span: span.clone(),
                }
            }
            (Expr::Bool { value, span }, _) => CheckedExpr::Bool {
                value: *value,
                span: span.clone(),
            },
            (Expr::Path { path, .. }, Inferred::Concrete(_)) => {
                let name = path_name(path);
                // A local is read from its frame slot; a `const` is substituted.
                if path.segments.len() == 1
                    && let Some(binding) = scope
                        .iter()
                        .rev()
                        .find(|binding| binding.name == path.segments[0].text)
                {
                    return Ok(CheckedExpr::Read {
                        place: Box::new(CheckedPlace::Local {
                            slot: binding.slot,
                            offset: binding.offset,
                            ty: binding.ty.clone(),
                            mutable: binding.mutable,
                            span: path.span.clone(),
                        }),
                        ty: binding.ty.clone(),
                        span: path.span.clone(),
                    });
                }
                self.constant_value(&name, &path.span)?
            }
            (
                Expr::Call {
                    callee,
                    arguments,
                    span,
                },
                Inferred::Concrete(ty),
            ) => {
                let Expr::Path { path, .. } = &**callee else {
                    return Err(self.error(
                        codes::NOT_CALLABLE,
                        "only a named function or syscall can be called",
                        span,
                        &[],
                        Some("call a function by name"),
                        &[],
                    ));
                };
                let found = self.lookup_path(path);
                let is_extern = found
                    .as_ref()
                    .is_some_and(|found| matches!(found.symbol, Symbol::Extern(_)));
                // The call target has to be the function's *qualified* name.
                // Keeping only the last path segment made `rt::sys::print` a call
                // to a function called `print`, which no module declares — so
                // every call across a module boundary lowered to a target the IR
                // verifier rejected, and no single-module test could see it. A
                // syscall keeps its bare name, because that is the name the ABI
                // table holds.
                let callee = match found {
                    Some(found) => match &found.symbol {
                        Symbol::Function(function) => {
                            qualified_name(&found.module.path, &function.name)
                        }
                        _ => name_of_path(path),
                    },
                    None => name_of_path(path),
                };
                let mut checked = Vec::new();
                for argument in arguments {
                    let parameter = self
                        .parameter_type_for(path, checked.len())
                        .unwrap_or(Type::Unit);
                    let expected_parameter = parameter.clone();
                    let argument_inferred =
                        self.check_expression(argument, scope, Some(expected_parameter.clone()))?;
                    checked.push(self.finalize(
                        argument,
                        argument_inferred,
                        Some(expected_parameter),
                        scope,
                    )?);
                }
                CheckedExpr::Call {
                    callee,
                    is_extern,
                    arguments: checked,
                    ty,
                    span: span.clone(),
                }
            }
            (
                Expr::MethodCall {
                    receiver,
                    method,
                    arguments,
                    span,
                },
                Inferred::Concrete(ty),
            ) => {
                let receiver_inferred = self.check_expression(receiver, scope, None)?;
                let checked_receiver = self.finalize(receiver, receiver_inferred, None, scope)?;
                let mut checked = Vec::new();
                for argument in arguments {
                    let argument_inferred = self.check_expression(argument, scope, None)?;
                    checked.push(self.finalize(argument, argument_inferred, None, scope)?);
                }
                let _ = checked;
                CheckedExpr::Builtin {
                    method: method.text.clone(),
                    receiver: Box::new(checked_receiver),
                    arguments: checked,
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::Index { .. }, Inferred::Concrete(_)) => {
                let place = self.check_place(expression, scope)?;
                let ty = place.ty_cloned();
                CheckedExpr::Read {
                    place: Box::new(place),
                    ty,
                    span: span_of(expression),
                }
            }
            (
                Expr::Unary {
                    operator,
                    operand,
                    span,
                },
                Inferred::Concrete(ty),
            ) => {
                if matches!(operator, UnaryOp::Address | UnaryOp::AddressMut) {
                    // A str borrow is a str, so it takes no address: it is a
                    // read of the borrowed str's place, which keeps the value
                    // rather than discarding it.
                    if ty == Type::Str {
                        let place = self.check_place(operand, scope)?;
                        let span = span.clone();
                        return Ok(CheckedExpr::Read {
                            place: Box::new(place),
                            ty,
                            span,
                        });
                    }
                    let place = self.check_place(operand, scope)?;
                    return Ok(CheckedExpr::AddressOf {
                        place: Box::new(place),
                        ty,
                        mutable: *operator == UnaryOp::AddressMut,
                        span: span.clone(),
                    });
                }
                // A negated literal is built here rather than re-checked through
                // its operand, because the operand on its own may not be a valid
                // value: `128i8` is not an `i8`, while `-128i8` is.
                if let (UnaryOp::Negate, Expr::Int { literal, span }) =
                    (*operator, operand.as_ref())
                {
                    return Ok(CheckedExpr::Unary {
                        operator: unary_text(*operator).to_string(),
                        operand: Box::new(CheckedExpr::Integer {
                            value: literal.value,
                            ty: ty.clone(),
                            span: span.clone(),
                        }),
                        ty,
                        span: span.clone(),
                    });
                }
                // A dereference's operand is a reference, which the place check
                // already verified; constraining it to the result type would be
                // wrong, because `*r` has type T while `r` has type `&T`.
                let operand_expected = match operator {
                    UnaryOp::Deref => None,
                    _ => Some(ty.clone()),
                };
                let operand_inferred =
                    self.check_expression(operand, scope, operand_expected.clone())?;
                let checked = self.finalize(operand, operand_inferred, operand_expected, scope)?;
                CheckedExpr::Unary {
                    operator: unary_text(*operator).to_string(),
                    operand: Box::new(checked),
                    ty,
                    span: span.clone(),
                }
            }
            (
                Expr::Binary {
                    operator,
                    left,
                    right,
                    span,
                },
                Inferred::Concrete(ty),
            ) => {
                // The left operand is checked with no expectation, because the first pass
                // already decided its type: a comparison, a logical operator, and an
                // arithmetic operator all constrain it differently.
                let left_inferred = self.check_expression(left, scope, None)?;
                let left_checked = self.finalize(left, left_inferred, None, scope)?;
                // A logical operator needs `bool` on the right; every other
                // operator needs whatever type its left operand has.
                let right_expected = match operator {
                    BinaryOp::And | BinaryOp::Or => Some(Type::Bool),
                    _ => Some(left_checked.ty()),
                };
                let right_inferred = self.check_expression(right, scope, right_expected.clone())?;
                let right_checked = self.finalize(right, right_inferred, right_expected, scope)?;
                CheckedExpr::Binary {
                    operator: operator.text().to_string(),
                    left: Box::new(left_checked),
                    right: Box::new(right_checked),
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::Cast { operand, span, .. }, Inferred::Concrete(to)) => {
                // The source type is the operand's own type, re-derived here so the
                // cast records what it actually converts rather than a guess.
                let operand_inferred = self.check_expression(operand, scope, None)?;
                let from = match operand_inferred.clone().defaulted() {
                    Inferred::Concrete(ty) => ty,
                    // An undecided literal is an `i32`, as everywhere else.
                    _ => Type::I32,
                };
                let checked =
                    self.finalize(operand, operand_inferred, Some(from.clone()), scope)?;
                CheckedExpr::Cast {
                    operand: Box::new(checked),
                    from,
                    to,
                    span: span.clone(),
                }
            }
            (Expr::Array { elements, span }, Inferred::Concrete(ty)) => {
                let element = ty.element().cloned().unwrap_or(Type::I32);
                let mut checked = Vec::new();
                for element_expr in elements {
                    let element_inferred =
                        self.check_expression(element_expr, scope, Some(element.clone()))?;
                    checked.push(self.finalize(
                        element_expr,
                        element_inferred,
                        Some(element.clone()),
                        scope,
                    )?);
                }
                // The elements are kept, not folded into a block of discarded
                // statements: the array's contents are what a later stage writes
                // to the frame, so dropping them here would lose the program.
                CheckedExpr::Array {
                    elements: checked,
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::ArrayRepeat { span, .. }, Inferred::Concrete(ty)) => {
                // The value and the count are kept, for the same reason as an
                // array literal's elements: they are the stores themselves.
                // `check_expression` only accepts a literal count, so the count
                // is a number here and not an expression.
                let (value, count) = match expression {
                    Expr::ArrayRepeat { value, count, .. } => {
                        let element = ty.element().cloned().unwrap_or(Type::I32);
                        let value_inferred =
                            self.check_expression(value, scope, Some(element.clone()))?;
                        let checked_value =
                            self.finalize(value, value_inferred, Some(element), scope)?;
                        let count_inferred =
                            self.check_expression(count, scope, Some(Type::Usize))?;
                        let count = match count_inferred {
                            Inferred::IntegerLiteral(value) => {
                                u64::try_from(value).unwrap_or(u64::MAX)
                            }
                            _ => 0,
                        };
                        (checked_value, count)
                    }
                    _ => return Ok(CheckedExpr::Unit { span: span.clone() }),
                };
                CheckedExpr::ArrayRepeat {
                    value: Box::new(value),
                    count,
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::If { arms, span }, Inferred::Concrete(ty)) => {
                // Each arm's condition is checked once, here, and kept: Step 62
                // needs the real test expression for every arm.
                let mut checked_arms = Vec::new();
                for arm in arms.iter() {
                    let checked_condition = match &arm.condition {
                        Some(condition) => {
                            let inferred = self.check_condition(condition, scope)?;
                            Some(self.finalize(condition, inferred, Some(Type::Bool), scope)?)
                        }
                        None => None,
                    };
                    let block = self.check_block(
                        &arm.body,
                        &mut scope.to_vec(),
                        &mut Vec::new(),
                        &mut 0,
                        ty.clone(),
                        self.loop_depth,
                    )?;
                    let arm_ty = block.tail.as_ref().map(CheckedExpr::ty);
                    checked_arms.push(CheckedArm {
                        condition: checked_condition,
                        statements: block.statements,
                        tail: block.tail,
                        ty: arm_ty,
                        span: arm.span.clone(),
                    });
                }
                CheckedExpr::If {
                    arms: checked_arms,
                    ty,
                    span: span.clone(),
                }
            }
            (Expr::Block { block, span }, Inferred::Concrete(_)) => {
                let checked = self.check_block(
                    block,
                    &mut scope.to_vec(),
                    &mut Vec::new(),
                    &mut 0,
                    expected.unwrap_or(Type::Unit),
                    self.loop_depth,
                )?;
                let tail = checked.tail.clone();
                match tail {
                    Some(tail) => CheckedExpr::Block {
                        statements: checked.statements,
                        tail: Box::new(tail),
                        span: span.clone(),
                    },
                    None => CheckedExpr::Unit { span: span.clone() },
                }
            }
            (other, Inferred::IntegerLiteral(_)) => {
                return Err(self.error(
                    codes::MISMATCH,
                    "this expression has no type",
                    &span_of(other),
                    &[],
                    Some("annotate the binding or parameter that needs it"),
                    &[],
                ));
            }
            (other, Inferred::Invalid) => {
                return Err(self.error(
                    codes::MISMATCH,
                    "this expression is not valid",
                    &span_of(other),
                    &[],
                    Some("fix the errors reported above first"),
                    &[],
                ));
            }
        })
    }

    fn intern_string(&mut self, value: &str) -> u32 {
        if let Some(index) = self.string_index.get(value) {
            return *index;
        }
        let index = u32::try_from(self.strings.len()).unwrap_or(u32::MAX);
        self.strings.push(CheckedString {
            text: String::from(value),
        });
        self.string_index.insert(String::from(value), index);
        index
    }

    fn constant_value(&mut self, name: &str, span: &SourceSpan) -> Result<CheckedExpr, StageError> {
        for module in self.resolved.all_modules() {
            for symbol in module.items.values() {
                if let Symbol::Constant(constant) = symbol
                    && constant.name == name
                {
                    let ty = self.constant_type(constant)?;
                    return self.constant_expression(&constant.value, ty, span);
                }
            }
        }
        Err(self.unresolved_name(name, span))
    }

    fn constant_expression(
        &mut self,
        expression: &Expr,
        ty: Type,
        span: &SourceSpan,
    ) -> Result<CheckedExpr, StageError> {
        Ok(match expression {
            Expr::Int { literal, .. } => CheckedExpr::Integer {
                value: literal.value,
                ty,
                span: expression_span(expression, span),
            },
            Expr::Str { value, .. } => {
                let index = self.intern_string(value);
                CheckedExpr::Str {
                    index,
                    ty: Type::Str,
                    span: expression_span(expression, span),
                }
            }
            Expr::Bool { value, .. } => CheckedExpr::Bool {
                value: *value,
                span: expression_span(expression, span),
            },
            other => {
                return Err(self.error(
                    codes::NOT_CONSTANT,
                    "this `const` value is not a literal",
                    &span_of(other),
                    &["Lazen v1 `const` values are integer, string, or bool literals"],
                    Some("write a literal, or compute the value in a function"),
                    &[],
                ));
            }
        })
    }

    /// The type of a callee's parameter, resolved through the symbol table.
    ///
    /// The path is resolved rather than matched by name, so a `use` alias
    /// reaches the same declaration the call does.
    fn parameter_type_for(&self, path: &crate::ast::Path, index: usize) -> Option<Type> {
        let found = self.lookup_path(path)?;
        let parameters = match found.symbol {
            Symbol::Function(function) => &function.parameters,
            Symbol::Extern(declaration) => &declaration.parameters,
            Symbol::Constant(_) | Symbol::Module(_) => return None,
        };
        let parameter = parameters.get(index)?;
        self.type_of(&parameter.annotation).ok()
    }

    /// Builds a diagnostic in the shared system.
    fn error(
        &self,
        raw_code: &str,
        message: impl Into<String>,
        span: &SourceSpan,
        notes: &[&str],
        help: Option<&str>,
        extra: &[(SourceSpan, String)],
    ) -> StageError {
        let code = DiagnosticCode::new(raw_code)
            .unwrap_or_else(|_| DiagnosticCode::new("T9999").expect("the fallback code is valid"));
        let mut diagnostic = Diagnostic::new(Severity::Error, code, message)
            .with_label(Label::primary(span.clone(), "here"));
        for note in notes {
            diagnostic = diagnostic.with_note(Note::new(*note));
        }
        for (label_span, text) in extra {
            diagnostic = diagnostic.with_label(Label::secondary(label_span.clone(), text.clone()));
        }
        if let Some(help) = help {
            diagnostic = diagnostic.with_help(Help::new(help));
        }
        let _ = self.source;
        StageError::from_parts(diagnostic, self.sources.clone())
    }
}

/// A name bound in a function body.
#[derive(Clone, Debug)]
struct Binding {
    name: String,
    slot: u32,
    offset: u32,
    ty: Type,
    mutable: bool,
    /// Where the binding was written, for a secondary diagnostic label.
    span: SourceSpan,
}

/// The byte size of an array, saturating rather than wrapping.
fn array_bytes(length: u64, element_bytes: u32) -> u128 {
    u128::from(length).saturating_mul(u128::from(element_bytes))
}

/// Rounds `offset` up to a multiple of `alignment`.
fn align_up(offset: u32, alignment: u32) -> u32 {
    if alignment <= 1 {
        return offset;
    }
    let remainder = offset % alignment;
    if remainder == 0 {
        offset
    } else {
        offset.saturating_add(alignment - remainder)
    }
}

/// The name of a type, for a diagnostic.
pub fn ty_name(ty: &Type) -> String {
    match ty {
        Type::Unit => "()".to_string(),
        Type::Bool => "bool".to_string(),
        Type::I8 => "i8".to_string(),
        Type::I16 => "i16".to_string(),
        Type::I32 => "i32".to_string(),
        Type::I64 => "i64".to_string(),
        Type::U8 => "u8".to_string(),
        Type::U16 => "u16".to_string(),
        Type::U32 => "u32".to_string(),
        Type::U64 => "u64".to_string(),
        Type::Usize => "usize".to_string(),
        Type::Str => "str".to_string(),
        Type::Slice {
            element,
            mutable: false,
        } => format!("&[{}]", ty_name(element)),
        Type::Slice {
            element,
            mutable: true,
        } => format!("&mut [{}]", ty_name(element)),
        Type::Array { element, length } => format!("[{}; {length}]", ty_name(element)),
        Type::Pointer { pointee } => format!("ptr<{}>", ty_name(pointee)),
        Type::Reference {
            pointee,
            mutable: false,
        } => format!("&{}", ty_name(pointee)),
        Type::Reference {
            pointee,
            mutable: true,
        } => format!("&mut {}", ty_name(pointee)),
    }
}

/// The OS ABI syscalls a Lazen program may declare, by source name.
///
/// The table itself is the ABI's, and lives in `lazalith-os-abi` so that both
/// front ends read one copy. A test checks it against `Syscall::ALL`, so adding
/// a syscall to the ABI without naming it there fails the suite instead of
/// silently making a declaration unresolvable.
pub use lazalith_os_abi::{ABI_SYSCALLS, abi_syscall};

/// Syscall names the Lazen design reserves for a later ABI step.
///
/// `docs/lazen-graphics.md` and `docs/lazen-input.md` specified these calls
/// before the ABI numbered them, and every one of them is numbered now. The list
/// is kept because it is the frontend's rule for what to do with a declaration
/// that has no number: it is accepted and recorded as unmapped, so the compiler
/// never invents a syscall number. A name in this list is one the design has
/// promised and the ABI has not yet delivered, and an empty list is the honest
/// state once every promised call exists.
pub const RESERVED_DESIGN_SYSCALLS: &[&str] = &[];

/// Whether a name is one the design reserves for a later ABI step.
pub fn is_reserved_design_syscall(name: &str) -> bool {
    RESERVED_DESIGN_SYSCALLS.contains(&name)
}

/// Whether v1 permits a cast from one type to another.
pub fn cast_is_allowed(from: &Type, to: &Type) -> bool {
    if from == to {
        return true;
    }
    if from.is_integer() && to.is_integer() {
        return true;
    }
    if matches!(from, Type::Reference { .. }) && matches!(to, Type::Pointer { .. }) {
        return true;
    }
    if matches!(from, Type::Pointer { .. }) && to.is_integer() {
        return true;
    }
    if from.is_integer() && matches!(to, Type::Pointer { .. }) {
        return true;
    }
    false
}

/// The methods available on a type.
pub fn methods_for(ty: &Type) -> &'static str {
    match ty {
        Type::Str => "`len`, `as_bytes`, `as_ptr`",
        Type::Array { .. } => "`len`, `as_slice`, `as_mut_slice`, `as_ptr`",
        Type::Slice { .. } => "`len`, `as_ptr`",
        Type::Pointer { .. } => "`slice_from_raw`, `slice_from_raw_mut`",
        _ => "none",
    }
}

fn unary_text(operator: UnaryOp) -> &'static str {
    match operator {
        UnaryOp::Negate => "-",
        UnaryOp::Not => "!",
        UnaryOp::Address => "&",
        UnaryOp::AddressMut => "&mut",
        UnaryOp::Deref => "*",
    }
}

fn arith_text(operator: ArithOp) -> &'static str {
    match operator {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "*",
        ArithOp::Div => "/",
        ArithOp::Rem => "%",
    }
}

fn comparison_text(operator: CompareOp) -> &'static str {
    match operator {
        CompareOp::Equal => "==",
        CompareOp::NotEqual => "!=",
        CompareOp::Less => "<",
        CompareOp::LessEqual => "<=",
        CompareOp::Greater => ">",
        CompareOp::GreaterEqual => ">=",
    }
}

impl BinaryOp {
    /// Whether this is a comparison, which yields `bool`.
    pub fn is_comparison(&self) -> bool {
        matches!(self, BinaryOp::Compare(_))
    }
}

/// The source text of a path, for a diagnostic.
fn path_name(path: &crate::ast::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join("::")
}

/// The last segment of a path, which is the item's own name.
fn name_of_path(path: &crate::ast::Path) -> String {
    path.segments
        .last()
        .map(|segment| segment.text.clone())
        .unwrap_or_default()
}

/// A fully qualified name.
fn qualified_name(module: &[String], name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{}::{name}", module.join("::"))
    }
}

/// The span of an expression.
pub fn span_of(expression: &Expr) -> SourceSpan {
    expression.span_of().clone()
}

fn expression_span(expression: &Expr, _fallback: &SourceSpan) -> SourceSpan {
    expression.span_of().clone()
}

/// Whether a block's statements contain a `break` at this nesting level.
fn block_contains_break(statements: &[Stmt]) -> bool {
    statements.iter().any(|statement| match statement {
        Stmt::Break { .. } => true,
        Stmt::While { body, .. } | Stmt::Loop { body, .. } => {
            block_contains_break(&body.statements)
        }
        Stmt::For { body, .. } => block_contains_break(&body.statements),
        Stmt::If { arms, .. } => arms
            .iter()
            .any(|arm| block_contains_break(&arm.body.statements)),
        Stmt::Block { block, .. } => block_contains_break(&block.statements),
        _ => false,
    })
}
