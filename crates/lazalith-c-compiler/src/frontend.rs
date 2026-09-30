//! The frontend pipeline.
//!
//! One entry point runs the stages in order and stops at the first failure, so a
//! program never reaches a later stage built on an earlier stage's mistake:
//!
//! ```text
//! source -> preprocessor -> lexer -> parser -> resolver -> type checker -> CheckedCProgram
//! ```
//!
//! The preprocessor is a stage of its own rather than a step inside the lexer, because
//! it is the one stage that can *fail* for a reason that is not about the program's
//! grammar: a missing header is not a syntax error, and reporting it as one would send
//! someone looking in the wrong file for a mistake that is not there.
//!
//! Every stage reports through `lazalith_diagnostics`, and every stage keeps the
//! `SourceManager` it was given, so a caller can render any failure with the shared
//! renderer. That matters more than usual here: a header is added to the *same* source
//! map as the program, so a diagnostic from inside one renders against the header's own
//! text rather than against the program that included it.
//!
//! # Two entry points, on purpose
//!
//! [`compile`] stops at the first failure, and [`analyse`] collects them all.
//! Both exist because both are right for a caller: a build script wants the
//! first, because it will fail anyway, and an editor wants all of them, because
//! a person fixing a program wants to see every mistake in it rather than
//! finding them one run at a time.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use lazalith_diagnostics::Diagnostic;
use lazalith_types::{ByteOffset, SourceError, SourceId, SourceManager};

use crate::ast::TranslationUnit;
use crate::diagnostic::StageError;
use crate::lexer::{self, Token};
use crate::parser;
use crate::preprocess::{self, Includes};
use crate::resolve::{self, Resolved};
use crate::types::{self, CheckedCProgram};

/// Runs the whole frontend on one file, stopping at the first failure.
///
/// The caller owns the source map and the file's name. On success the caller owns the
/// checked program, whose every node still points into that map.
///
/// **No headers, and no architecture.** This is the entry point for a program that has
/// neither, and it is not a limitation but a description: a program that includes a header
/// needs someone to say where headers are, and [`compile_for`] is where that is said.
pub fn compile(
    sources: &mut SourceManager,
    name: &str,
    text: &str,
) -> Result<(SourceId, CheckedCProgram), StageError> {
    let mut resolver = preprocess::NoIncludes;
    let mut includes = Includes::none(&mut resolver);
    compile_for(sources, name, text, &mut includes)
}

/// Runs the whole frontend on one file, with headers and a target.
///
/// **This is the entry point a real build uses,** and the difference from [`compile`] is
/// exactly the difference between a program and a translation unit: a translation unit can
/// include a header, and which one it means depends on a target and an include path that
/// only the caller knows.
pub fn compile_for(
    sources: &mut SourceManager,
    name: &str,
    text: &str,
    includes: &mut Includes<'_>,
) -> Result<(SourceId, CheckedCProgram), StageError> {
    let source = add_file(sources, name, text)?;
    let tokens = preprocess_file(source, name, sources, includes)?;
    let unit = parse_tokens(source, sources, tokens)?;
    let resolved = resolve::resolve(source, sources, unit);
    let mut checked = types::check(source, sources, &resolved);
    if !checked.diagnostics.is_empty() {
        let mut diagnostics = core::mem::take(&mut checked.diagnostics);
        return Err(diagnostics.remove(0));
    }
    Ok((source, checked))
}

/// Runs the whole frontend and reports every failure it finds.
///
/// Returns the resolved tree even when there were failures, because a caller that wants to
/// highlight every problem needs the tree the problems are in. It is only absent when the
/// file could not be read at all, which is the one failure that leaves nothing to point at.
pub fn analyse(sources: &mut SourceManager, name: &str, text: &str) -> Analysis {
    let mut resolver = preprocess::NoIncludes;
    let mut includes = Includes::none(&mut resolver);
    analyse_for(sources, name, text, &mut includes)
}

/// [`analyse`], with headers and a target.
///
/// **The same reason [`compile_for`] exists, and the same asymmetry:** a build script wants
/// the first diagnostic and an editor wants all of them, and both want the same headers.
pub fn analyse_for(
    sources: &mut SourceManager,
    name: &str,
    text: &str,
    includes: &mut Includes<'_>,
) -> Analysis {
    let source = match add_file(sources, name, text) {
        Ok(source) => source,
        Err(error) => {
            return Analysis {
                source: None,
                resolved: empty_resolved(),
                checked: None,
                diagnostics: alloc::vec![error],
            };
        }
    };
    let preprocessed = preprocess::preprocess(sources, source, name, includes);
    if !preprocessed.diagnostics.is_empty() {
        return Analysis {
            source: Some(source),
            resolved: empty_resolved(),
            checked: None,
            diagnostics: preprocessed
                .diagnostics
                .into_iter()
                .map(|diagnostic| StageError::from_parts(diagnostic, sources.clone()))
                .collect(),
        };
    }
    let parsed = parser::parse(source, sources, preprocessed.tokens);
    if !parsed.diagnostics.is_empty() {
        return Analysis {
            source: Some(source),
            resolved: empty_resolved(),
            checked: None,
            diagnostics: parsed.diagnostics,
        };
    }
    let resolved = resolve::resolve(source, sources, parsed.unit);
    if !resolved.diagnostics.is_empty() {
        let diagnostics = resolved.diagnostics.clone();
        return Analysis {
            source: Some(source),
            resolved,
            checked: None,
            diagnostics,
        };
    }
    let mut checked = types::check(source, sources, &resolved);
    let diagnostics = core::mem::take(&mut checked.diagnostics);
    Analysis {
        source: Some(source),
        resolved,
        checked: Some(checked),
        diagnostics,
    }
}

/// What one [`analyse`] found.
#[derive(Debug)]
pub struct Analysis {
    /// The file, unless it could not be read.
    pub source: Option<SourceId>,
    /// The resolved tree, which is empty only when the file could not be read.
    pub resolved: Resolved,
    /// The checked program, present only when nothing failed.
    pub checked: Option<CheckedCProgram>,
    /// Every diagnostic, in the order the stages found them.
    pub diagnostics: Vec<StageError>,
}

impl Analysis {
    /// Whether anything failed.
    pub fn failed(&self) -> bool {
        !self.diagnostics.is_empty()
    }
}

/// Lexes and parses one file, without resolving or checking it.
pub fn parse_file(
    sources: &mut SourceManager,
    name: &str,
    text: &str,
) -> Result<(SourceId, TranslationUnit), StageError> {
    let source = add_file(sources, name, text)?;
    let tokens = lex_file(source, sources)?;
    parse_tokens(source, sources, tokens).map(|unit| (source, unit))
}

/// Preprocesses one file into tokens.
///
/// `name` is the file's name as written, and it is passed on to the preprocessor because
/// the include-cycle check compares *names*: every read of a header adds a new file to the
/// map, so an id-based check could never see a cycle.
pub fn preprocess_file(
    source: SourceId,
    name: &str,
    sources: &mut SourceManager,
    includes: &mut Includes<'_>,
) -> Result<Vec<Token>, StageError> {
    let preprocessed = preprocess::preprocess(sources, source, name, includes);
    let tokens = preprocessed.tokens;
    match preprocessed.diagnostics.into_iter().next() {
        None => Ok(tokens),
        Some(error) => Err(StageError::from_parts(error, sources.clone())),
    }
}

/// Lexes one file into tokens.
pub fn lex_file(source: SourceId, sources: &SourceManager) -> Result<Vec<Token>, StageError> {
    let lexed = lexer::lex(source, sources);
    let tokens = lexed.tokens;
    match lexed.diagnostics.into_iter().next() {
        None => Ok(tokens),
        Some(error) => Err(StageError::from_parts(error, sources.clone())),
    }
}

/// Parses a token stream, stopping at the first failure.
pub fn parse_tokens(
    source: SourceId,
    sources: &SourceManager,
    tokens: Vec<Token>,
) -> Result<TranslationUnit, StageError> {
    let parsed = parser::parse(source, sources, tokens);
    match parsed.diagnostics.into_iter().next() {
        None => Ok(parsed.unit),
        Some(error) => Err(error),
    }
}

/// Adds a file to the map, reporting a failure that way.
fn add_file(sources: &mut SourceManager, name: &str, text: &str) -> Result<SourceId, StageError> {
    let source = sources
        .add_file(name, text)
        .map_err(|error| refused_source(name, error, sources))?;
    Ok(source)
}

/// A diagnostic for a file that could not be added.
///
/// The span is the start of the *first* file in the map, because a file that
/// was never added has no offset in anything. Pointing at another file's first
/// line is not right, and the message says which file it is about, so a reader
/// is never misled about the program's own text.
fn refused_source(name: &str, error: SourceError, sources: &SourceManager) -> StageError {
    let source = SourceId::new(0);
    let span = sources
        .source_span(source, ByteOffset::new(0), ByteOffset::new(0))
        .ok();
    let mut diagnostic = Diagnostic::new(
        lazalith_diagnostics::Severity::Error,
        crate::diagnostic::raw("F0001"),
        alloc::format!("`{name}` could not be added to the source map: {error}"),
    )
    .with_help(lazalith_diagnostics::Help::new(
        "every diagnostic a compile reports has to name a file the renderer has",
    ));
    if let Some(span) = span {
        diagnostic = diagnostic.with_label(lazalith_diagnostics::Label::primary(
            span,
            "this file was not added",
        ));
    }
    StageError::from_parts(diagnostic, sources.clone())
}

/// A resolved tree with nothing in it.
///
/// Every field is empty rather than absent, so a caller that reads one finds
/// nothing — which is the truth — and does not have to unwrap an `Option`
/// before finding out.
fn empty_resolved() -> Resolved {
    Resolved {
        unit: TranslationUnit {
            declarations: Vec::new(),
        },
        globals: BTreeMap::new(),
        tags: BTreeMap::new(),
        typedefs: BTreeMap::new(),
        functions: Vec::new(),
        diagnostics: Vec::new(),
    }
}
