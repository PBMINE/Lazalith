//! The C type system, and what this machine can honestly represent.
//!
//! # Sizes, decided here because they had not been decided anywhere
//!
//! `docs/lz64.md` left C's `int` and `long` sizes explicitly open, pending "a
//! dedicated ABI design". This is that decision, and it is the only one the
//! project was missing before a C program could mean anything:
//!
//! | C type | bytes | IR type |
//! |---|---|---|
//! | `_Bool` | 1 | `Type::Bool` |
//! | `char`, `signed char`, `unsigned char` | 1 | `Int { 8, signed }` |
//! | `short` | 2 | `Int { 16, signed }` |
//! | `int` | 4 | `Int { 32, signed }` |
//! | `long`, `long long` | 8 | `Int { 64, signed }` |
//! | pointer | 8 | `Type::Pointer` |
//!
//! `long` and `long long` are both eight bytes, which is LP64 and matches the
//! machine: sixteen general registers, all 64 bits, no register pairs. This is
//! the standard C model (C11 5.1.1.2) in which `int` is 32 bits and `long` is
//! 64, and a program compiled here is not portable to an ILP32 host, which is
//! said out loud rather than implied.
//!
//! `size_t` is `unsigned long`, so it is `Int { 64, unsigned }`.
//!
//! # What is refused, and why
//!
//! A compiler that quietly miscompiles the parts of C this machine cannot
//! represent is worse than one that refuses them, because the program's author
//! has no way to find out which half they got. So the following are rejected
//! with a specific diagnostic naming the reason:
//!
//! - `float` and `double`. The ISA has no floating-point instruction, and the
//!   honest options — a software implementation, or a narrower integer type
//!   wearing a float's name — are both a different project.
//! - A `struct` or `union` larger than one word passed or returned by value. The
//!   machine's ABI has no multiword return and no aggregate argument passing,
//!   and this stage does not invent one. Such a type is still fine to declare,
//!   to hold in a local, to take a pointer to, and to read a member of.
//! - A `switch` on a floating value, for the same reason.
//!
//! Each refusal names the C construct and the machine limit, so the reader knows
//! what to change rather than just what failed.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

/// A C type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CType {
    /// `void`, and only `void`.
    Void,
    /// `_Bool`.
    Bool,
    /// A signed integer of a stated width in bits.
    Int {
        /// The width in bits. Always a multiple of eight, and never zero.
        bits: u16,
        /// Whether the value is negative below zero.
        signed: bool,
    },
    /// `T *`.
    ///
    /// `T` may itself be a pointer, so a `char **` is a pointer to a pointer to
    /// a `char`, and a null `T` is `Void`.
    Pointer(Box<CType>),
    /// `T[N]`, an array of `N` elements.
    ///
    /// C has no way to write an array type that is not a pointer's target, so
    /// arrays cannot be returned or passed by value; the usual arithmetic
    /// conversions turn them into pointers first.
    Array {
        /// The element type.
        element: Box<CType>,
        /// How many elements, which is never zero.
        length: u32,
    },
    /// `struct S`, `union S`, or an unnamed one spelled inline.
    Struct(Box<RecordType>),
    /// `union S`, kept apart from `struct` because the two share a tag space
    /// but not a layout.
    Union(Box<RecordType>),
    /// `enum S`, or C's anonymous integer enum.
    ///
    /// The underlying type is `Int { 32, signed }` unless a value needs more,
    /// which is what C requires and what keeps a common case narrow.
    Enum(Box<EnumType>),
    /// `T (...)`, a function type.
    Function(Box<FunctionType>),
}

/// A `struct` or `union`'s definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordType {
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// Whether this is a `union` rather than a `struct`.
    pub union: bool,
    /// The fields in declaration order.
    ///
    /// A union's members all start at offset zero; the list is kept whole
    /// because a union with three members still has to be *read* correctly even
    /// though it holds one of them.
    pub fields: Vec<Field>,
    /// Whether the layout has been worked out.
    ///
    /// A type can be named before it is complete — `struct S *next;` inside
    /// `struct S` is legal C — so a record with no layout is normal rather than
    /// broken, and asking for one anyway is an error.
    pub complete: bool,
}

/// One `struct` or `union` member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Field {
    /// The member's name.
    pub name: String,
    /// The member's type.
    pub ty: CType,
    /// The member's byte offset from the start of the record.
    pub offset: u32,
    /// The member's size in bytes.
    pub size: u32,
}

/// An `enum`'s definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnumType {
    /// The tag, when one was written.
    pub tag: Option<String>,
    /// The enumerators, in declaration order.
    pub members: Vec<Enumerator>,
    /// Whether the layout has been worked out.
    pub complete: bool,
}

/// One `enum` constant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Enumerator {
    /// The name as written.
    pub name: String,
    /// Its value.
    pub value: i64,
}

/// A function type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionType {
    /// The return type.
    pub result: CType,
    /// The declared parameters, already adjusted.
    ///
    /// An array parameter is a *pointer* parameter and a function parameter is a
    /// *function pointer* parameter, because that is what C says happens to a
    /// parameter's declared type before the body ever sees it. Storing the
    /// adjusted type means the body cannot be handed an array to subscript.
    pub params: Vec<CType>,
    /// Whether the declaration was a definition, which is what gives a
    /// prototype its parameter names.
    pub variadic: bool,
    /// The parameter names, when the declaration gave them.
    pub names: Vec<Option<String>>,
}

impl CType {
    /// The type a `_Bool` holds.
    pub const fn bool_type() -> Self {
        Self::Bool
    }

    /// A signed integer type of a stated width.
    pub const fn signed(bits: u16) -> Self {
        Self::Int { bits, signed: true }
    }

    /// An unsigned integer type of a stated width.
    pub const fn unsigned(bits: u16) -> Self {
        Self::Int {
            bits,
            signed: false,
        }
    }

    /// `int`, which is four bytes and signed.
    pub const fn int() -> Self {
        Self::Int {
            bits: 32,
            signed: true,
        }
    }

    /// `unsigned int`.
    pub const fn uint() -> Self {
        Self::Int {
            bits: 32,
            signed: false,
        }
    }

    /// `long`, which is eight bytes and signed.
    pub const fn long() -> Self {
        Self::Int {
            bits: 64,
            signed: true,
        }
    }

    /// `unsigned long`, which is also `size_t`.
    pub const fn ulong() -> Self {
        Self::Int {
            bits: 64,
            signed: false,
        }
    }

    /// A pointer to `inner`.
    pub fn pointer_to(inner: CType) -> Self {
        Self::Pointer(Box::new(inner))
    }

    /// An array of `length` elements.
    pub fn array_of(element: CType, length: u32) -> Self {
        Self::Array {
            element: Box::new(element),
            length,
        }
    }

    /// `void`, which is a type like any other.
    pub const fn void() -> Self {
        Self::Void
    }

    /// The type's size in bytes, or `None` when it has none.
    ///
    /// `void` and a function have no size because they cannot be stored, and an
    /// incomplete record does not have one until its definition is closed.
    pub fn size_in_bytes(&self) -> Option<u32> {
        match self {
            Self::Void | Self::Function(_) => None,
            Self::Bool => Some(1),
            Self::Int { bits, .. } => Some(u32::from(*bits / 8)),
            Self::Pointer(_) => Some(8),
            Self::Array { element, length } => element.size_in_bytes()?.checked_mul(*length),
            Self::Struct(record) | Self::Union(record) => {
                if !record.complete {
                    None
                } else {
                    Some(record_size(record))
                }
            }
            Self::Enum(enumeration) => {
                if !enumeration.complete {
                    None
                } else {
                    Some(4)
                }
            }
        }
    }

    /// The type's alignment in bytes.
    ///
    /// Always a power of two, and never larger than the type: a one-byte array
    /// of doubles is eight-byte aligned, because its elements are.
    pub fn alignment_in_bytes(&self) -> u32 {
        match self {
            Self::Void => 1,
            Self::Bool => 1,
            Self::Int { bits, .. } => u32::from(*bits / 8),
            Self::Pointer(_) => 8,
            Self::Array { element, .. } => element.alignment_in_bytes(),
            Self::Struct(record) | Self::Union(record) => {
                if !record.complete {
                    1
                } else {
                    record
                        .fields
                        .iter()
                        .map(|field| field.ty.alignment_in_bytes())
                        .max()
                        .unwrap_or(1)
                }
            }
            Self::Enum(_) => 4,
            Self::Function(_) => 8,
        }
    }

    /// Whether the type is an integer, counting `_Bool` and `char`.
    pub const fn is_integer(&self) -> bool {
        matches!(self, Self::Bool | Self::Int { .. } | Self::Enum(_))
    }

    /// Whether the type is an arithmetic type, which is an integer type — there
    /// is no floating-point type this machine can represent.
    pub const fn is_arithmetic(&self) -> bool {
        self.is_integer()
    }

    /// Whether the type is a pointer, including a function pointer.
    pub const fn is_pointer(&self) -> bool {
        matches!(self, Self::Pointer(_))
    }

    /// Whether the type is a pointer to an object rather than to a function.
    ///
    /// This is the difference C's arithmetic needs: a `void (*)(void)` can be
    /// called but not added to, and treating the two alike would let `p + 1`
    /// mean "the next function".
    pub const fn is_object_pointer(&self) -> bool {
        match self {
            Self::Pointer(inner) => !matches!(**inner, Self::Function(_)),
            _ => false,
        }
    }

    /// Whether the type is an array, which decays to a pointer in almost every
    /// context.
    pub const fn is_array(&self) -> bool {
        matches!(self, Self::Array { .. })
    }

    /// Whether the type is a function, which decays to a function pointer.
    pub const fn is_function(&self) -> bool {
        matches!(self, Self::Function(_))
    }

    /// Whether the type is `void`.
    pub const fn is_void(&self) -> bool {
        matches!(self, Self::Void)
    }

    /// Whether the type is complete: its size is known.
    pub fn is_complete(&self) -> bool {
        self.size_in_bytes().is_some()
    }

    /// The integer type an enum's constants have.
    pub fn enum_underlying(&self) -> Self {
        match self {
            Self::Enum(_) => Self::Int {
                bits: 32,
                signed: true,
            },
            _ => self.clone(),
        }
    }

    /// The type with every array and function turned into a pointer.
    ///
    /// This is C's *decay*, and it happens in three places: where a function
    /// argument is declared, where an expression that is not an lvalue is used,
    /// and where a subscript or call is applied. Doing it in one function means
    /// the three places cannot disagree.
    pub fn decayed(&self) -> CType {
        match self {
            Self::Array { element, .. } => CType::Pointer(element.clone()),
            Self::Function(_) => CType::Pointer(Box::new(self.clone())),
            other => other.clone(),
        }
    }

    /// The type as C writes it, for a diagnostic.
    pub fn name(&self) -> String {
        match self {
            Self::Void => String::from("void"),
            Self::Bool => String::from("_Bool"),
            Self::Int { bits, signed } => signed_name(*bits, *signed),
            Self::Pointer(inner) => alloc::format!("{} *", inner.name()),
            Self::Array { element, length } => alloc::format!("{}[{length}]", element.name()),
            Self::Struct(record) => match &record.tag {
                Some(tag) => alloc::format!("struct {tag}"),
                None => String::from("struct"),
            },
            Self::Union(record) => match &record.tag {
                Some(tag) => alloc::format!("union {tag}"),
                None => String::from("union"),
            },
            Self::Enum(enumeration) => match &enumeration.tag {
                Some(tag) => alloc::format!("enum {tag}"),
                None => String::from("enum"),
            },
            Self::Function(signature) => {
                let mut name = alloc::format!("{} (", signature.result.name());
                for (index, parameter) in signature.params.iter().enumerate() {
                    if index > 0 {
                        name.push_str(", ");
                    }
                    name.push_str(&parameter.name());
                }
                if signature.variadic {
                    if !signature.params.is_empty() {
                        name.push_str(", ");
                    }
                    name.push_str("...");
                }
                name.push(')');
                name
            }
        }
    }
}

/// An integer type's name in C's spelling, which depends on both width and
/// signedness.
///
/// `char`, `short`, `int` and `long` are the signed names; the unsigned ones
/// are spelled `unsigned X` except for the two whose C spelling is its own
/// keyword, `unsigned char` and `unsigned short`. Getting this right is not
/// cosmetic: a diagnostic that says "expected i32" where the source said
/// `unsigned long` is a diagnostic the reader has to translate.
fn signed_name(bits: u16, signed: bool) -> String {
    let base = match (bits, signed) {
        (8, false) => "unsigned char",
        (8, true) => "signed char",
        (16, false) => "unsigned short",
        (16, true) => "short",
        (32, false) => "unsigned int",
        (32, true) => "int",
        (64, false) => "unsigned long",
        (64, true) => "long",
        _ => "an integer",
    };
    base.to_string()
}

/// A record's total size, padded so the next thing is aligned.
fn record_size(record: &RecordType) -> u32 {
    if record.union {
        return align_up(
            record
                .fields
                .iter()
                .map(|field| field.size)
                .max()
                .unwrap_or(0),
            record_alignment(record),
        );
    }
    align_up(record.last_offset(), record_alignment(record))
}

impl RecordType {
    /// The record's own alignment, which is the strictest member's.
    pub fn record_alignment(&self) -> u32 {
        record_alignment(self)
    }

    /// Where a `struct`'s padding ends, before the record's own padding.
    fn last_offset(&self) -> u32 {
        self.fields
            .iter()
            .map(|field| field.offset.saturating_add(field.size))
            .max()
            .unwrap_or(0)
    }
}

/// A record's alignment.
fn record_alignment(record: &RecordType) -> u32 {
    record
        .fields
        .iter()
        .map(|field| field.ty.alignment_in_bytes())
        .max()
        .unwrap_or(1)
}

/// Rounds `value` up to a whole multiple of `alignment`.
///
/// `alignment` is always a power of two here, so this is a mask, and a value
/// that is already aligned is returned unchanged rather than "padded" by zero.
pub fn align_up(value: u32, alignment: u32) -> u32 {
    let alignment = alignment.max(1);
    value.wrapping_add(alignment - 1) & !(alignment - 1)
}

impl fmt::Display for CType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.name())
    }
}
