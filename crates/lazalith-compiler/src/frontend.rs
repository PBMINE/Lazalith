//! The frontend pipeline.
//!
//! One entry point runs the stages in order and stops at the first failure, so
//! a program never reaches a later stage built on an earlier stage's mistake:
//!
//! ```text
//! source -> lexer -> parser -> resolver -> type checker -> CheckedProgram
//! ```
//!
//! Every stage reports through `lazalith_diagnostics`, and every stage keeps the
//! `SourceManager` it was given, so a caller can render any failure with the
//! shared renderer.

use alloc::vec::Vec;
use lazalith_types::{SourceId, SourceManager};

use crate::ast::Program;
use crate::diagnostic::StageError;
use crate::lexer::{self, Token};
use crate::parser;
use crate::resolve::{self, Resolved};
use crate::types::{self, CheckedProgram};

/// Runs the whole frontend on one file.
///
/// The caller owns the source map and the file name. On success the caller owns
/// the checked program, whose every node still points into that source map.
pub fn compile(
    sources: &mut SourceManager,
    name: &str,
    text: &str,
) -> Result<(SourceId, CheckedProgram), StageError> {
    let source = sources
        .add_file(name, text)
        .map_err(|error| unregistered_source(name, error))?;
    let program = parse_file(source, sources)?;
    let resolved = resolve::resolve(source, sources, program)?;
    let checked = types::check(source, sources, &resolved)?;
    Ok((source, checked))
}

/// Lexes and parses one file, without resolving or checking it.
pub fn parse_file(source: SourceId, sources: &SourceManager) -> Result<Program, StageError> {
    let tokens = lex_file(source, sources)?;
    parser::parse(source, sources, tokens)
}

/// Lexes one file into tokens.
pub fn lex_file(source: SourceId, sources: &SourceManager) -> Result<Vec<Token>, StageError> {
    let lexed = lexer::lex(source, sources);
    if lexed.diagnostics.is_empty() {
        return Ok(lexed.tokens);
    }
    Err(lexed
        .diagnostics
        .into_iter()
        .next()
        .expect("a failed lexing has at least one diagnostic"))
}

/// Resolves and type-checks an already parsed program.
pub fn check_program(
    source: SourceId,
    sources: &SourceManager,
    program: Program,
) -> Result<CheckedProgram, StageError> {
    let resolved: Resolved = resolve::resolve(source, sources, program)?;
    types::check(source, sources, &resolved)
}

/// A source file could not be registered at all, so there is no span to point
/// at. The diagnostic therefore carries no label.
fn unregistered_source(name: &str, error: lazalith_types::SourceError) -> StageError {
    use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Severity};
    let code = DiagnosticCode::new("F0001").expect("the source code literal is valid");
    StageError::from_parts(
        Diagnostic::new(
            Severity::Error,
            code,
            alloc::format!("cannot register the source file `{name}`: {error}"),
        ),
        SourceManager::new(),
    )
}
