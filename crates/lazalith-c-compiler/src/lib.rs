#[macro_use]
extern crate alloc;

pub mod ast;
pub mod ctypes;
pub mod diagnostic;
pub mod frontend;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod resolve;
pub mod types;

pub use ctypes::{CType, EnumType, Enumerator, Field, FunctionType, RecordType};
pub use diagnostic::{CompileError, StageError, render};
pub use frontend::{analyse, compile};
pub use ir::lower;
pub use types::CheckedCProgram;
