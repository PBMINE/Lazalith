//! Structured IR errors.

use alloc::string::String;
use core::fmt;
use lazalith_types::SourceSpan;

/// The kind of IR failure, without the location or context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrErrorKind {
    /// A block label was used that the function does not contain.
    UnknownBlock {
        /// The missing label.
        block: u32,
    },
    /// A value was used that the function does not define.
    UndefinedValue {
        /// The missing value.
        value: u32,
    },
    /// A value was defined more than once.
    RedefinedValue {
        /// The duplicated value.
        value: u32,
    },
    /// A function had no blocks.
    EmptyFunction,
    /// A function had no terminator in some block.
    MissingTerminator {
        /// The block without one.
        block: u32,
    },
    /// A block was defined more than once.
    RedefinedBlock {
        /// The duplicated label.
        block: u32,
    },
    /// A function was defined more than once.
    RedefinedFunction {
        /// The duplicated name.
        name: String,
    },
    /// The entry block branched away instead of being the first block.
    EntryBlockTerminator,
    /// A call referenced a function that does not exist.
    UnknownCallee {
        /// The missing name.
        name: String,
    },
    /// A call referenced an import, which this module cannot resolve.
    UnresolvedImport {
        /// The unresolved name.
        name: String,
    },
    /// A use was not dominated by its definition.
    UndominatedUse {
        /// The used value.
        value: u32,
        /// The use site, as a block label.
        block: u32,
    },
    /// A type's size or alignment is not representable.
    InvalidType {
        /// A human-readable description of the problem.
        detail: String,
    },
    /// A field or variant offset was outside its aggregate.
    OffsetOutOfRange {
        /// The requested offset.
        offset: u32,
        /// The aggregate's size.
        size: u32,
    },
    /// A field name was used that the record does not have.
    UnknownField {
        /// The missing name.
        name: String,
    },
    /// A store width and value type disagreed.
    WidthMismatch {
        /// The width the store declared.
        bytes: u32,
        /// The size of the stored value.
        size: u32,
    },
    /// A call's argument count disagreed with its target's declaration.
    ArityMismatch {
        /// The expected count.
        expected: usize,
        /// The supplied count.
        actual: usize,
    },
    /// A call's argument type disagreed with its target's declaration.
    ArgumentTypeMismatch {
        /// The zero-based argument index.
        index: usize,
        /// A human-readable description of the disagreement.
        detail: String,
    },
    /// The builder was used in a way its invariants forbid.
    InvalidBuilderState {
        /// What went wrong.
        detail: String,
    },
    /// A fallible allocation failed.
    Allocation,
}

/// An IR failure: what went wrong, where, and in which function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IrError {
    /// The failure kind.
    pub kind: IrErrorKind,
    /// The function it occurred in, when known.
    pub function: Option<String>,
    /// The block it occurred in, when known.
    pub block: Option<u32>,
    /// The source location, when known.
    pub span: Option<SourceSpan>,
}

impl IrError {
    /// Creates an error with no function or location.
    pub const fn new(kind: IrErrorKind) -> Self {
        Self {
            kind,
            function: None,
            block: None,
            span: None,
        }
    }

    /// Attaches a function name.
    pub fn in_function(mut self, name: &str) -> Self {
        self.function = Some(String::from(name));
        self
    }

    /// Attaches a block label.
    pub fn with_block(mut self, block: crate::BlockId) -> Self {
        self.block = Some(block.get());
        self
    }

    /// Attaches a source location.
    pub fn at(mut self, span: SourceSpan) -> Self {
        self.span = Some(span);
        self
    }
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.function {
            Some(function) => {
                let kind = &self.kind;
                write!(f, "{function}: {kind}")
            }
            None => {
                let kind = &self.kind;
                write!(f, "{kind}")
            }
        }
    }
}

impl fmt::Display for IrErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBlock { block } => write!(f, "unknown block b{block}"),
            Self::UndefinedValue { value } => write!(f, "undefined value v{value}"),
            Self::RedefinedValue { value } => write!(f, "value v{value} is defined twice"),
            Self::EmptyFunction => f.write_str("function has no blocks"),
            Self::MissingTerminator { block } => {
                write!(f, "block b{block} has no terminator")
            }
            Self::RedefinedBlock { block } => write!(f, "block b{block} is defined twice"),
            Self::RedefinedFunction { name } => write!(f, "function {name} is defined twice"),
            Self::EntryBlockTerminator => {
                f.write_str("the entry block must not branch to another block")
            }
            Self::UnknownCallee { name } => write!(f, "call target {name} does not exist"),
            Self::UnresolvedImport { name } => write!(f, "import {name} is not resolved"),
            Self::UndominatedUse { value, block } => {
                write!(
                    f,
                    "value v{value} is used in b{block} before it dominates the use"
                )
            }
            Self::InvalidType { detail } => write!(f, "invalid type: {detail}"),
            Self::OffsetOutOfRange { offset, size } => {
                write!(f, "offset {offset} is outside a {size}-byte aggregate")
            }
            Self::UnknownField { name } => write!(f, "record has no field {name}"),
            Self::WidthMismatch { bytes, size } => {
                write!(f, "store of {bytes} bytes cannot write a {size}-byte value")
            }
            Self::ArityMismatch { expected, actual } => {
                write!(f, "call takes {expected} arguments but {actual} were given")
            }
            Self::ArgumentTypeMismatch { index, detail } => {
                write!(f, "argument {index} type mismatch: {detail}")
            }
            Self::InvalidBuilderState { detail } => {
                write!(f, "invalid builder state: {detail}")
            }
            Self::Allocation => f.write_str("IR allocation failed"),
        }
    }
}

impl core::error::Error for IrError {}
