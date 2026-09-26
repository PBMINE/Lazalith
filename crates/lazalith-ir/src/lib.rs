//! Lazalith IR: the common low-level intermediate representation.
//!
//! This crate sits *below* language-specific syntax trees. A Lazen frontend and
//! a future C frontend both lower into these types, so nothing here may mention
//! a language concept: there are no generics, no closures, no traits, and no
//! ownership metadata. A value is an integer, a pointer, an aggregate of
//! values, or a function reference.
//!
//! The representation is deliberately simple and *verifiable*:
//!
//! - a function is a list of basic blocks;
//! - every block ends in exactly one terminator;
//! - every value is produced by exactly one instruction;
//! - every use is dominated by its definition;
//! - the verifier rejects anything it cannot prove, with a structured error and
//!   a source location, rather than emitting code for it.
//!
//! There is no optimizer, no SSA renaming, and no register allocation. Code
//! generation owns instruction selection and register use.

#![no_std]

extern crate alloc;

mod builder;
mod error;
mod ir;
mod verify;

pub use builder::{BlockBuilder, FunctionBuilder, ModuleBuilder};
pub use error::{IrError, IrErrorKind};
pub use ir::{
    BinaryOp, Block, BlockId, CallArg, CallTarget, ComparisonOp, ConstValue, DataSegment, Function,
    IrModule, Linkage, MemorySpace, Module, Name, Parameter, RecordField, ReturnValue, StoreWidth,
    Terminator, Type, UnaryOp, ValueId,
};
pub use ir::{Instruction, Intrinsic, LoadWidth};
pub use verify::{instruction_result_type, produces_value, value_type, verify_module};
