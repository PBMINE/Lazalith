//! The Lazen compiler frontend.
//!
//! This crate implements Step 61: lexer, parser, AST, name resolution, type
//! checking, and semantic analysis for the Lazen v1 language defined in
//! `docs/lazen-syntax.md` and `docs/lazen-types.md`. It ends at a validated
//! typed representation. It emits no machine code, runs nothing, and depends on
//! no host facility: code generation is Step 63 and lives elsewhere.
//!
//! ```text
//! source -> lexer -> parser -> AST -> resolver -> type checker -> CheckedProgram
//! ```
//!
//! Every failure is a shared `lazalith_diagnostics::Diagnostic` with a stable
//! code, a real source span, and the shared `lazalith_types::SourceManager`.
//! The compiler has no second diagnostic architecture and no panicking path
//! reachable from source text.
//!
//! Lazen v1 is a closed language: `bool`, `i8`–`i64`, `u8`–`u64`, `usize`,
//! `str`, `&str`, `ptr<T>`, `&[T]`, `&mut [T]`, and `[T; N]`. Constructs outside
//! that set are rejected with a specific diagnostic rather than approximated.

#![no_std]

extern crate alloc;

pub mod ast;
pub mod diagnostic;
pub mod format;
pub mod frontend;
pub mod lexer;
pub mod lower;
pub mod parser;
pub mod resolve;
pub mod types;

pub use diagnostic::{CompileError, StageError, render};
pub use format::{FormatError, format, is_formatted};
pub use frontend::{check_program, compile, parse_file};
