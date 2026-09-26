//! The Lazen v1 abstract syntax tree.
//!
//! A typed tree of enums, not a stringly-typed one: every construct in
//! `docs/lazen-syntax.md` is a variant, and every node that came from source
//! carries the `SourceSpan` it was written at, so a later stage can point a
//! diagnostic at the exact characters.
//!
//! The tree holds no types and makes no semantic decisions. Inference,
//! mutability, and control-flow requirements belong to the type checker.

use alloc::{boxed::Box, string::String, vec::Vec};
use lazalith_types::SourceSpan;

/// A name as written, with its span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Name {
    /// The identifier text.
    pub text: String,
    /// Where it was written.
    pub span: SourceSpan,
}

impl Name {
    /// Builds a name.
    pub fn new(text: String, span: SourceSpan) -> Self {
        Self { text, span }
    }
}

/// A path such as `geometry` or `geometry::area`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Path {
    /// The segments, left to right. Never empty.
    pub segments: Vec<Name>,
    /// The span of the whole path.
    pub span: SourceSpan,
}

impl Path {
    /// The first segment, which is the item or module the path starts at.
    pub fn head(&self) -> &Name {
        &self.segments[0]
    }

    /// The last segment.
    pub fn tail(&self) -> &Name {
        &self.segments[self.segments.len() - 1]
    }
}

/// A type as written in the source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TypeExpr {
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
    /// `str`
    Str,
    /// `&str`
    StrRef,
    /// `ptr<T>`
    Ptr(Box<TypeExpr>),
    /// `&[T]` or `&mut [T]`
    Slice {
        /// The element type.
        element: Box<TypeExpr>,
        /// Whether the view is mutable.
        mutable: bool,
    },
    /// `[T; N]`
    Array {
        /// The element type.
        element: Box<TypeExpr>,
        /// The element count.
        length: u64,
    },
}

/// A type together with where it was written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TypeAnnotation {
    /// The type.
    pub kind: TypeExpr,
    /// The span covering the whole type, `&[u8]` or `ptr<u8>` included.
    pub span: SourceSpan,
}

impl TypeAnnotation {
    /// Builds an annotation.
    pub fn new(kind: TypeExpr, span: SourceSpan) -> Self {
        Self { kind, span }
    }
}

/// A unary operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOp {
    /// `-x`, wrapping negation.
    Negate,
    /// `!x`, logical negation of a `bool`.
    Not,
    /// `&x`, the address of a place.
    Address,
    /// `&mut x`, the mutable address of a place.
    AddressMut,
    /// `*x`, reading through a pointer.
    Deref,
}

/// A binary arithmetic operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArithOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
}

/// A binary comparison operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompareOp {
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
}

/// A binary operator of either family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// An arithmetic operator, which wraps.
    Arith(ArithOp),
    /// A comparison, which yields `bool`.
    Compare(CompareOp),
    /// `&&`
    And,
    /// `||`
    Or,
}

impl BinaryOp {
    /// The operator's source text.
    pub fn text(self) -> &'static str {
        match self {
            BinaryOp::Arith(ArithOp::Add) => "+",
            BinaryOp::Arith(ArithOp::Sub) => "-",
            BinaryOp::Arith(ArithOp::Mul) => "*",
            BinaryOp::Arith(ArithOp::Div) => "/",
            BinaryOp::Arith(ArithOp::Rem) => "%",
            BinaryOp::Compare(CompareOp::Equal) => "==",
            BinaryOp::Compare(CompareOp::NotEqual) => "!=",
            BinaryOp::Compare(CompareOp::Less) => "<",
            BinaryOp::Compare(CompareOp::LessEqual) => "<=",
            BinaryOp::Compare(CompareOp::Greater) => ">",
            BinaryOp::Compare(CompareOp::GreaterEqual) => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        }
    }
}

/// An integer literal, its radix, its value, and any type suffix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntLiteral {
    /// The value.
    pub value: u128,
    /// 10 or 16.
    pub radix: u32,
    /// The suffix the source gave, if any.
    pub suffix: Option<crate::lexer::IntSuffix>,
}

/// One arm of a conditional, or the `else` arm.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfArm {
    /// The condition, or `None` for `else`.
    pub condition: Option<Expr>,
    /// The arm's block.
    pub body: Block,
    /// The span from `if`/`else` to the closing brace.
    pub span: SourceSpan,
}

/// An expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expr {
    /// An integer literal.
    Int {
        /// The literal.
        literal: IntLiteral,
        /// The span of the whole literal, suffix included.
        span: SourceSpan,
    },
    /// A string literal.
    Str {
        /// The decoded bytes.
        value: String,
        /// The span including the quotes.
        span: SourceSpan,
    },
    /// `true` or `false`.
    Bool {
        /// The value.
        value: bool,
        /// The span.
        span: SourceSpan,
    },
    /// A name, possibly a path such as `geometry::area`.
    Path {
        /// The path.
        path: Path,
        /// The span.
        span: SourceSpan,
    },
    /// A call: `f(a, b)`.
    Call {
        /// The callee expression, which must be a path.
        callee: Box<Expr>,
        /// The arguments, in order.
        arguments: Vec<Expr>,
        /// The span from the callee to the closing parenthesis.
        span: SourceSpan,
    },
    /// A method call: `x.len()`.
    MethodCall {
        /// The receiver.
        receiver: Box<Expr>,
        /// The method name.
        method: Name,
        /// The arguments, in order.
        arguments: Vec<Expr>,
        /// The span from the receiver to the closing parenthesis.
        span: SourceSpan,
    },
    /// An index: `values[0]`.
    Index {
        /// The indexed expression.
        base: Box<Expr>,
        /// The index.
        index: Box<Expr>,
        /// The span including both brackets.
        span: SourceSpan,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        operator: UnaryOp,
        /// The operand.
        operand: Box<Expr>,
        /// The span.
        span: SourceSpan,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        operator: BinaryOp,
        /// The left operand.
        left: Box<Expr>,
        /// The right operand.
        right: Box<Expr>,
        /// The span covering both operands.
        span: SourceSpan,
    },
    /// A cast: `value as u64`.
    Cast {
        /// The value being cast.
        operand: Box<Expr>,
        /// The target type.
        target: TypeAnnotation,
        /// The span from the operand to the end of the type.
        span: SourceSpan,
    },
    /// An array literal: `[1, 2, 3]`.
    Array {
        /// The elements, in order.
        elements: Vec<Expr>,
        /// The span including the brackets.
        span: SourceSpan,
    },
    /// A repeated array literal: `[0u8; 16]`.
    ArrayRepeat {
        /// The element value.
        value: Box<Expr>,
        /// The repeat count.
        count: Box<Expr>,
        /// The span including the brackets.
        span: SourceSpan,
    },
    /// A conditional used as a value.
    If {
        /// The arms, always ending in an `else` when used as a value.
        arms: Vec<IfArm>,
        /// The span from `if` to the final closing brace.
        span: SourceSpan,
    },
    /// A block used as a value, which must have a tail expression.
    Block {
        /// The block.
        block: Box<Block>,
        /// The span, which is the block's own span.
        span: SourceSpan,
    },
}

/// A block: a list of statements and an optional tail expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Block {
    /// The statements, in order.
    pub statements: Vec<Stmt>,
    /// The tail expression, if the block ends in one.
    pub tail: Option<Box<Expr>>,
    /// The span including both braces.
    pub span: SourceSpan,
}

impl Block {
    /// Whether this block ends in a value.
    pub fn is_value(&self) -> bool {
        self.tail.is_some()
    }
}

/// A statement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Stmt {
    /// `let name: T = value;`
    Let {
        /// The name being bound.
        name: Name,
        /// Whether the binding is mutable.
        mutable: bool,
        /// The written type, if any.
        annotation: Option<TypeAnnotation>,
        /// The initial value.
        value: Expr,
        /// The span of the whole statement.
        span: SourceSpan,
    },
    /// `place = value;`
    Assign {
        /// The place being written.
        target: Expr,
        /// The new value.
        value: Expr,
        /// The span of the whole statement.
        span: SourceSpan,
    },
    /// An expression evaluated for its effect.
    Expression {
        /// The expression.
        expression: Expr,
        /// The span of the whole statement.
        span: SourceSpan,
    },
    /// A conditional statement, used for its effect.
    If {
        /// The arms.
        arms: Vec<IfArm>,
        /// The span.
        span: SourceSpan,
    },
    /// `while condition { }`
    While {
        /// The condition.
        condition: Expr,
        /// The body.
        body: Block,
        /// The span.
        span: SourceSpan,
    },
    /// `for name in start..end { }`, or `for name in collection { }`
    For {
        /// The loop variable.
        name: Name,
        /// The iterated expression: a range when `end` is `Some`, and a
        /// collection otherwise. Lazen v1 has no loop over a collection, so the
        /// type checker rejects the second form by name.
        iterated: Expr,
        /// The exclusive end of the range, when this is a range loop.
        end: Option<Expr>,
        /// The body.
        body: Block,
        /// The span.
        span: SourceSpan,
    },
    /// `loop { }`
    Loop {
        /// The body.
        body: Block,
        /// The span.
        span: SourceSpan,
    },
    /// `break`
    Break {
        /// The span.
        span: SourceSpan,
    },
    /// `continue`
    Continue {
        /// The span.
        span: SourceSpan,
    },
    /// `return value?`
    Return {
        /// The returned value, or `None` for a bare `return`.
        value: Option<Expr>,
        /// The span.
        span: SourceSpan,
    },
    /// A nested block used for its effect.
    Block {
        /// The block.
        block: Box<Block>,
        /// The span, which is the block's own span.
        span: SourceSpan,
    },
}

impl Stmt {
    /// The statement's span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Expression { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Loop { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Return { span, .. }
            | Stmt::Block { span, .. } => span,
        }
    }
}

/// A function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    /// The parameter's name.
    pub name: Name,
    /// Its type.
    pub annotation: TypeAnnotation,
    /// The span of the whole parameter.
    pub span: SourceSpan,
}

/// A function definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
    /// The function's name.
    pub name: Name,
    /// Whether it is `pub`.
    pub is_public: bool,
    /// The parameters, in order.
    pub parameters: Vec<Parameter>,
    /// The result type, or `None` when written as `-> ()`. v1 always has
    /// exactly one result, so this is only ever absent for the unit result.
    pub result: Option<TypeAnnotation>,
    /// The body.
    pub body: Block,
    /// The span of the whole definition.
    pub span: SourceSpan,
}

/// The only foreign calling convention in v1.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Abi {
    /// `extern "syscall"`: the OS ABI's own argument order.
    Syscall,
}

/// An `extern` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternDecl {
    /// The calling convention.
    pub abi: Abi,
    /// The declared name.
    pub name: Name,
    /// The parameters, in ABI order.
    pub parameters: Vec<Parameter>,
    /// The result type.
    pub result: TypeAnnotation,
    /// The span of the whole declaration.
    pub span: SourceSpan,
}

/// A `mod` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleDecl {
    /// The module's name.
    pub name: Name,
    /// Whether it is `pub`.
    pub is_public: bool,
    /// The items inside it.
    pub items: Vec<Item>,
    /// The span from `mod` to the closing brace.
    pub span: SourceSpan,
}

/// A `use` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseDecl {
    /// The path being imported.
    pub path: Path,
    /// An `as` rename, if any.
    pub alias: Option<Name>,
    /// The span of the whole declaration.
    pub span: SourceSpan,
}

/// A `const` declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstDecl {
    /// The constant's name.
    pub name: Name,
    /// Whether it is `pub`.
    pub is_public: bool,
    /// Its type, if written.
    pub annotation: Option<TypeAnnotation>,
    /// Its value.
    pub value: Expr,
    /// The span of the whole declaration.
    pub span: SourceSpan,
}

/// A top-level item, or an item inside a module.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Item {
    /// A function.
    Function(Function),
    /// An `extern` declaration.
    Extern(ExternDecl),
    /// A `mod` declaration.
    Module(ModuleDecl),
    /// A `use` declaration.
    Use(UseDecl),
    /// A `const` declaration.
    Const(ConstDecl),
}

impl Item {
    /// The item's span.
    pub fn span(&self) -> &SourceSpan {
        match self {
            Item::Function(item) => &item.span,
            Item::Extern(item) => &item.span,
            Item::Module(item) => &item.span,
            Item::Use(item) => &item.span,
            Item::Const(item) => &item.span,
        }
    }

    /// The item's declared name.
    pub fn name(&self) -> &Name {
        match self {
            Item::Function(item) => &item.name,
            Item::Extern(item) => &item.name,
            Item::Module(item) => &item.name,
            Item::Use(item) => item.path.tail(),
            Item::Const(item) => &item.name,
        }
    }

    /// Whether the item is `pub`.
    pub fn is_public(&self) -> bool {
        match self {
            Item::Function(item) => item.is_public,
            Item::Module(item) => item.is_public,
            Item::Const(item) => item.is_public,
            Item::Extern(_) | Item::Use(_) => false,
        }
    }
}

/// A whole parsed file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Program {
    /// The top-level items, in order.
    pub items: Vec<Item>,
    /// The span of the whole file.
    pub span: SourceSpan,
}

impl Program {
    /// Every top-level function, extern, module, use, and const, in order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
}

impl Expr {
    /// The expression's span.
    ///
    /// Every variant carries one, so this is total: a node cannot exist without
    /// knowing where it was written.
    pub fn span_of(&self) -> &SourceSpan {
        match self {
            Expr::Int { span, .. }
            | Expr::Str { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Path { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Index { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Cast { span, .. }
            | Expr::Array { span, .. }
            | Expr::ArrayRepeat { span, .. }
            | Expr::If { span, .. }
            | Expr::Block { span, .. } => span,
        }
    }

    /// The expression's start offset.
    pub fn start(&self) -> u32 {
        self.span_of().start().as_u32()
    }

    /// The expression's end offset.
    pub fn end(&self) -> u32 {
        self.span_of().end().as_u32()
    }
}
