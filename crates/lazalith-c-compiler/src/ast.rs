//! The C syntax tree.
//!
//! This tree is the parser's output and the resolver's input. It is a *syntax*
//! tree: it records what the source said, including the things semantic analysis
//! will throw away. It records no types, resolves no names, and makes no
//! judgement beyond what the grammar requires — a `struct S *` written before
//! `struct S` is defined is a perfectly good tree, and only the type checker
//! knows whether the program meant it.
//!
//! # Spans
//!
//! Every node that can produce a diagnostic carries a [`SourceSpan`]. The span
//! is the *node's* span, not its first token's, because "expected `;`" is about
//! the whole declaration and "no member named `x`" is about the member
//! expression. A diagnostic that can only point at one byte is not much of a
//! diagnostic.
//!
//! # Initialisers
//!
//! C's initialiser syntax is the part of C with the most rules and the least
//! redundancy: a brace may be elided, scalars may be braced, and designators
//! may reorder a list. It is parsed into one shape — a *designated list* — so
//! the type checker has a single thing to interpret and no chance to disagree
//! with the parser about what a `{1, 2}` meant.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use lazalith_types::SourceSpan;

use crate::lexer::Number;

/// A whole translation unit.
#[derive(Clone, Debug)]
pub struct TranslationUnit {
    /// The file's top-level declarations, in order.
    pub declarations: Vec<Declaration>,
}

/// A top-level declaration.
#[derive(Clone, Debug)]
pub enum Declaration {
    /// A function definition.
    Function(Box<FunctionDefinition>),
    /// A declaration with no body: a prototype, or a variable declared at file
    /// scope.
    Declaration(Box<VarDecl>),
    /// A `struct` or `union` definition with no declarator, which is legal and
    /// is the only way to define a tag in a header.
    Record(Box<RecordDefinition>),
    /// An `enum` definition with no declarator.
    Enum(Box<EnumDefinition>),
    /// A `_Static_assert`, checked by the type checker.
    StaticAssert(StaticAssert),
}

/// A function definition.
#[derive(Clone, Debug)]
pub struct FunctionDefinition {
    /// The declared base type, before the declarator is applied.
    pub base: TypeSpecifier,
    /// The declarator, whose outermost derivation is the function type.
    pub declarator: Declarator,
    /// The function's name.
    pub name: String,
    /// The whole definition's span.
    pub span: SourceSpan,
    /// The body.
    pub body: Box<Block>,
}

/// A declaration with no body.
#[derive(Clone, Debug)]
pub struct VarDecl {
    /// The storage class and qualifiers.
    pub storage: Storage,
    /// Whether the declaration said `extern`.
    pub extern_: bool,
    /// Whether it said `static`.
    pub static_: bool,
    /// Whether it said `typedef`.
    pub typedef: bool,
    /// The declared base type.
    pub base: TypeSpecifier,
    /// The declarators, in order.
    pub declarators: Vec<Declarator>,
}

/// A `struct` or `union` definition.
#[derive(Clone, Debug)]
pub struct RecordDefinition {
    /// Whether it is a `union`.
    pub union: bool,
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// The members, or nothing for an opaque forward declaration.
    pub members: Option<Vec<Member>>,
    /// Where the tag is.
    pub span: SourceSpan,
}

/// An `enum` definition.
#[derive(Clone, Debug)]
pub struct EnumDefinition {
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// The enumerators, or nothing for an opaque forward declaration.
    pub members: Option<Vec<EnumeratorDefinition>>,
    /// Where the tag is.
    pub span: SourceSpan,
}

/// One `enum` constant.
#[derive(Clone, Debug)]
pub struct EnumeratorDefinition {
    /// The name.
    pub name: String,
    /// Its value, when one was written.
    pub value: Option<Expression>,
    /// Where the name is.
    pub span: SourceSpan,
}

/// A `_Static_assert`.
#[derive(Clone, Debug)]
pub struct StaticAssert {
    /// The condition, which must be an integer constant expression.
    pub condition: Expression,
    /// The message, when one was written.
    pub message: Option<String>,
    /// Where the whole assertion is.
    pub span: SourceSpan,
}

/// The storage class and qualifiers on a declaration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Storage {
    /// `const`.
    pub constant: bool,
    /// `volatile`.
    pub volatile: bool,
    /// `restrict`, which this machine has no aliasing rules for and which is
    /// therefore accepted and ignored.
    pub restrict_: bool,
    /// `inline`, accepted and ignored: code generation has no inlining, and a
    /// function that is `inline` is still a real function.
    pub inline: bool,
}

/// A type as written, before any declarator is applied.
///
/// The parser cannot know what `int *x[3]` means without a type system, so it
/// records the base type and the declarator and lets the resolver put them
/// together. This is C's own rule — the base type is the *element* type and the
/// declarator binds from the name outwards — so nothing is lost.
#[derive(Clone, Debug)]
pub struct TypeSpecifier {
    /// The base type.
    pub base: BaseType,
    /// The qualifiers written after the base.
    pub qualifiers: Storage,
    /// Where it starts.
    pub span: SourceSpan,
}

/// A base type, before declarators.
#[derive(Clone, Debug)]
pub enum BaseType {
    /// `void`.
    Void,
    /// `_Bool`.
    Bool,
    /// `char`, `signed char` or `unsigned char`.
    Char {
        /// Whether `signed` or `unsigned` was written.
        signed: bool,
    },
    /// `short`, `short int`, `unsigned short`.
    Short {
        /// Whether `unsigned` was written.
        unsigned: bool,
    },
    /// `int` on its own, `signed int`, `unsigned int`, or bare `unsigned`.
    ///
    /// The `unsigned` flag is here because this is the one integer base type that
    /// used to have nowhere to put it: `short` and `long` both carry it, and `int`
    /// did not, so `unsigned int` parsed as `int` and every program that used it got
    /// signed comparisons, signed division and a sign-extended widening — silently,
    /// because the type checker believed what the parser told it.
    Int {
        /// Whether `unsigned` was written. C's `int` is signed, and a bare
        /// `unsigned` means `unsigned int`.
        unsigned: bool,
    },
    /// `long` or `long long`, with or without `unsigned`.
    Long {
        /// Whether a second `long` was written.
        doubled: bool,
        /// Whether `unsigned` was written.
        unsigned: bool,
    },
    /// A `struct` or `union`.
    Record(Box<RecordReference>),
    /// An `enum`.
    Enum(Box<EnumReference>),
    /// A name that must already be a `typedef` or a tag.
    Named(String),
    /// A parenthesised type, from `(int (*)[3])` and its relatives.
    Parenthesised(Box<TypeSpecifier>),
}

/// A reference to a `struct` or `union` tag, or an inline definition of one.
#[derive(Clone, Debug)]
pub struct RecordReference {
    /// Whether it is a `union`.
    pub union: bool,
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// An inline definition, when the braces were written here.
    pub definition: Option<Box<RecordDefinition>>,
    /// Where it starts.
    pub span: SourceSpan,
}

/// A reference to an `enum` tag, or an inline definition of one.
#[derive(Clone, Debug)]
pub struct EnumReference {
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// An inline definition, when the braces were written here.
    pub definition: Option<Box<EnumDefinition>>,
    /// Where it starts.
    pub span: SourceSpan,
}

/// One declarator: a name and the derivations applied to it.
///
/// Reading outward from the name is C's own rule, so this is a list in that
/// order rather than a tree built by the parser. `int *a[3]` is
/// `[Pointer] -> [Array(3)]` applied to `a`, and getting that backwards is the
/// classic C bug, so the list is stored in the order it was written and the
/// resolver folds it in the order C specifies.
#[derive(Clone, Debug)]
pub struct Declarator {
    /// The name, or nothing for an abstract declarator.
    pub name: Option<String>,
    /// The derivations, innermost (nearest the name) first.
    pub derivation: Vec<Derivation>,
    /// The initialiser, when one was written.
    ///
    /// It lives on the declarator rather than beside it because an initialiser
    /// belongs to *one* declarator: `int a = 1, b = 2;` has two, and pairing
    /// them by position in a list would be a guess.
    pub initial: Option<Initializer>,
    /// Where the name is, for a diagnostic about the name.
    pub span: SourceSpan,
}

/// A type name, as `sizeof (T)` and a cast both need one.
///
/// A name with no declarator, so it is the base type plus a derivation list and
/// not a [`Declarator`]: there is no name to record, and a `None` where a name
/// belongs is the same value as no declarator at all.
#[derive(Clone, Debug)]
pub struct TypeName {
    /// The base type.
    pub base: TypeSpecifier,
    /// The derivations, innermost first.
    pub derivation: Vec<Derivation>,
    /// Where the whole thing is.
    pub span: SourceSpan,
}

/// An initialiser, as written.
#[derive(Clone, Debug)]
pub enum Initializer {
    /// A single value, for a scalar.
    Scalar(Expression),
    /// A braced list, for an aggregate or for a braced scalar.
    List {
        /// The items, with their designators.
        items: Vec<InitItem>,
        /// Where the braces are.
        span: SourceSpan,
    },
}

/// One item in a braced initialiser.
#[derive(Clone, Debug)]
pub struct InitItem {
    /// The designator, when one was written: `.member` or `[index]`.
    pub designator: Option<Designator>,
    /// The value.
    pub value: Box<Initializer>,
}

/// A designator in a braced initialiser.
#[derive(Clone, Debug)]
pub enum Designator {
    /// `.member`, which says which member the next value is for.
    Field {
        /// The member's name.
        name: String,
        /// Where the name is.
        span: SourceSpan,
    },
    /// `[index]`, which says which element the next value is for.
    Index {
        /// The index.
        index: Expression,
        /// Where it is.
        span: SourceSpan,
    },
}

/// One derivation applied to a declarator.
#[derive(Clone, Debug)]
pub enum Derivation {
    /// `*`, with its qualifiers.
    Pointer(Storage),
    /// `[n]`, with its constant size.
    Array(Option<Expression>),
    /// `(parameters)`.
    ///
    /// The second field is whether the parentheses were *written* around what came
    /// before. It is the whole difference between `int *f(void)` and
    /// `int (*f)(void)`, and it cannot be recovered from the list of derivations
    /// afterwards: both produce the same list, in the same order, and mean opposite
    /// things.
    Function(Vec<ParameterDeclaration>, bool, bool),
}

/// One parameter in a function declarator.
#[derive(Clone, Debug)]
pub struct ParameterDeclaration {
    /// The storage class and qualifiers.
    pub storage: Storage,
    /// The parameter's type, before its declarator.
    pub base: TypeSpecifier,
    /// The parameter's declarator, or nothing for an abstract one.
    pub declarator: Option<Declarator>,
    /// Where it starts.
    pub span: SourceSpan,
}

/// One `struct` or `union` member.
#[derive(Clone, Debug)]
pub struct Member {
    /// The declared base type.
    pub base: TypeSpecifier,
    /// The declarators, which is empty for an anonymous member.
    pub declarators: Vec<Declarator>,
    /// Where it starts.
    pub span: SourceSpan,
}

/// A statement.
#[derive(Clone, Debug)]
pub enum Statement {
    /// A compound statement.
    Block(Box<Block>),
    /// An expression statement.
    Expression(Expression),
    /// An `if`.
    If {
        /// The condition.
        condition: Expression,
        /// The then branch.
        then_branch: Box<Statement>,
        /// The `else` branch.
        else_branch: Option<Box<Statement>>,
    },
    /// A `while`.
    While {
        /// The condition.
        condition: Expression,
        /// The body.
        body: Box<Statement>,
    },
    /// A `do ... while`.
    DoWhile {
        /// The body, which runs at least once.
        body: Box<Statement>,
        /// The condition.
        condition: Expression,
    },
    /// A `for`.
    For {
        /// The initialiser, if any.
        initialiser: Option<Box<ForInit>>,
        /// The condition, if any.
        condition: Option<Expression>,
        /// The step, if any.
        step: Option<Expression>,
        /// The body.
        body: Box<Statement>,
    },
    /// A `switch`, with its cases and its default.
    Switch {
        /// The value switched on.
        condition: Expression,
        /// The body.
        body: Box<Statement>,
    },
    /// A `case` label.
    Case {
        /// The constant it matches.
        value: Expression,
        /// The statement it labels.
        statement: Box<Statement>,
    },
    /// A `default` label.
    Default {
        /// The statement it labels.
        statement: Box<Statement>,
    },
    /// A `break`.
    Break(SourceSpan),
    /// A `continue`.
    Continue(SourceSpan),
    /// A `return`, with or without a value.
    Return {
        /// The value returned, or nothing for `return;`.
        value: Option<Expression>,
        /// Where it is.
        span: SourceSpan,
    },
    /// A `goto`.
    Goto {
        /// The label it jumps to.
        name: String,
        /// Where it is.
        span: SourceSpan,
    },
    /// A label.
    Label {
        /// The name.
        name: String,
        /// The statement it labels.
        statement: Box<Statement>,
        /// Where the name is.
        span: SourceSpan,
    },
    /// A declaration used as a statement.
    Declaration(Box<VarDecl>),
    /// A `_Static_assert`.
    StaticAssert(Box<StaticAssert>),
    /// Nothing, from an empty `;`.
    Empty,
}

/// A compound statement.
#[derive(Clone, Debug)]
pub struct Block {
    /// The declarations and statements, in order.
    pub items: Vec<BlockItem>,
    /// Where the braces are.
    pub span: SourceSpan,
}

/// One item in a block.
#[derive(Clone, Debug)]
pub enum BlockItem {
    /// A declaration.
    Declaration(Box<VarDecl>),
    /// A statement.
    Statement(Statement),
}

/// A `for`'s initialiser.
#[derive(Clone, Debug)]
pub enum ForInit {
    /// A declaration, which is scoped to the loop.
    Declaration(Box<VarDecl>),
    /// An expression.
    Expression(Expression),
}

/// An expression, as written.
///
/// The variants are the C grammar's, and the operands are kept unflattened: `a +
/// b + c` is `Add(a, Add(b, c))` because that is the tree the parser builds, and
/// the type checker decides associativity's consequences. A parser that
/// reassociated would make `a + b + c` and `a + (b + c)` the same tree, and they
/// are not: one can overflow differently and one can trap differently.
#[derive(Clone, Debug)]
pub enum Expression {
    /// A name.
    Name {
        /// The name.
        name: String,
        /// Where it is.
        span: SourceSpan,
    },
    /// An integer constant.
    Integer {
        /// Its digits, base and suffix.
        number: Number,
        /// Where it is.
        span: SourceSpan,
    },
    /// A character constant.
    Character {
        /// Its value.
        value: i64,
        /// Where it is.
        span: SourceSpan,
    },
    /// A string literal, with its value already unescaped.
    String {
        /// The bytes, without the terminating null.
        value: String,
        /// Where it is.
        span: SourceSpan,
    },
    /// `sizeof`, with a type or an expression.
    SizeofType(Box<TypeName>),
    /// `sizeof` an expression, which is not evaluated.
    SizeofExpression(Box<Expression>),
    /// A parenthesised expression, which yields a grouped value.
    Group(Box<Expression>),
    /// A unary `&`.
    Address(Box<Expression>),
    /// A unary `*`.
    Dereference(Box<Expression>),
    /// A unary `+`.
    Plus(Box<Expression>),
    /// A unary `-`.
    Minus(Box<Expression>),
    /// A unary `~`.
    BitNot(Box<Expression>),
    /// A unary `!`.
    Not(Box<Expression>),
    /// A `++` or `--`, before or after.
    Increment {
        /// The operand, which must be an lvalue.
        operand: Box<Expression>,
        /// Whether it is `++` rather than `--`.
        increment: bool,
        /// Whether it is a prefix.
        prefix: bool,
    },
    /// A binary operator.
    Binary {
        /// Which one.
        op: BinaryOp,
        /// The left operand.
        left: Box<Expression>,
        /// The right operand.
        right: Box<Expression>,
    },
    /// A logical `&&` or `||`, kept apart from the arithmetic binaries because
    /// they short-circuit and produce an `int` rather than a `bool`.
    Logical {
        /// Whether it is `&&` rather than `||`.
        and: bool,
        /// The left operand.
        left: Box<Expression>,
        /// The right operand.
        right: Box<Expression>,
    },
    /// A conditional, `a ? b : c`.
    Conditional {
        /// The condition.
        condition: Box<Expression>,
        /// The value when it is true.
        then_value: Box<Expression>,
        /// The value when it is false.
        else_value: Box<Expression>,
    },
    /// A call, with at least one argument.
    Call {
        /// The callee, which is a function designator or a pointer to one.
        callee: Box<Expression>,
        /// The arguments, in order.
        arguments: Vec<Expression>,
    },
    /// A plain assignment, `a = b`.
    ///
    /// Kept apart from [`Expression::CompoundAssign`] because plain assignment
    /// converts to the *target's* type and discards the source's, while a
    /// compound assignment computes in the promoted type and then converts
    /// back. `char c; c = 300;` and `c += 300;` are not the same operation.
    Assign {
        /// The target, which must be a modifiable lvalue.
        target: Box<Expression>,
        /// The value assigned to it.
        value: Box<Expression>,
    },
    /// A subscript, `a[i]`.
    Subscript {
        /// The array, which decays to a pointer.
        array: Box<Expression>,
        /// The index, which is an integer.
        index: Box<Expression>,
    },
    /// A member access, `a.b`.
    Member {
        /// The record, which must be a `struct` or a `union`.
        record: Box<Expression>,
        /// The member's name.
        member: String,
        /// Whether the access was through `->`, which changes what the record has
        /// to be.
        arrow: bool,
        /// Where the member name is.
        span: SourceSpan,
    },
    /// A compound assignment, `a += b`.
    ///
    /// Kept whole rather than desugared into `a = a + b` because it is *not* the
    /// same: the left operand is evaluated once, and for a `char` or a `short` the
    /// result of `+=` is converted back to the operand's type where an assignment
    /// through a temporary would not be.
    CompoundAssign {
        /// Which operator, without the `=`.
        op: BinaryOp,
        /// The left operand, which must be an lvalue.
        target: Box<Expression>,
        /// The right operand.
        value: Box<Expression>,
    },
    /// A comma expression, whose value is the last one.
    Comma {
        /// The left operand, evaluated and discarded.
        left: Box<Expression>,
        /// The right operand, whose value this has.
        right: Box<Expression>,
    },
    /// A cast, `(T) e`.
    Cast {
        /// The type written.
        ty: Box<TypeName>,
        /// The operand.
        operand: Box<Expression>,
    },
}

/// A C binary operator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Subtract,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Remainder,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
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

impl BinaryOp {
    /// The operator's spelling, for a diagnostic.
    pub const fn spelling(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Remainder => "%",
            Self::ShiftLeft => "<<",
            Self::ShiftRight => ">>",
            Self::BitAnd => "&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
        }
    }

    /// Whether the operator is a comparison, which produces an `int` in C and
    /// not a `bool`.
    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Equal
                | Self::NotEqual
                | Self::Less
                | Self::LessEqual
                | Self::Greater
                | Self::GreaterEqual
        )
    }

    /// Whether the operator is a shift, whose right operand has its own rules.
    pub const fn is_shift(self) -> bool {
        matches!(self, Self::ShiftLeft | Self::ShiftRight)
    }

    /// Whether the operator works on integers only, which on this machine means
    /// every operator there is.
    pub const fn is_integer_only(self) -> bool {
        true
    }
}
