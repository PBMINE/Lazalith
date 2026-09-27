//! The startup sequence: the one part of a running program that is not Lazen.
//!
//! # Why this is machine code
//!
//! Lazen v1 has no function pointers — a function's name is not a value, and
//! `documented_examples.rs` has a test that rejects treating it as one. So a
//! Lazen function cannot call the program's `main` by name, and the thing that
//! does call it cannot be written in Lazen. This is the same reason every real
//! system has a `_start` in assembly: the entry sequence is below the language.
//!
//! # What it does
//!
//! ```text
//! entry:
//!     CALL fn.main        ; the program's entry, called through the ABI
//!     MOV r1, r0          ; its result becomes the exit code
//!     LI r0, 1            ; Exit
//!     LI r7, 0            ; the ABI reserves r7 and requires it to be zero
//!     SYSCALL
//! ```
//!
//! That is the whole of it, and each line is there for a stated reason:
//!
//! - `CALL` rather than a jump, so `main`'s own `RET` is real. A jump would leave
//!   it returning to whatever followed the entry point.
//! - `r1` rather than `r0` for the code, because the syscall number is in `r0`.
//!   Reading the result from `r0` after setting the number would exit with the
//!   number.
//! - `LI r7, 0` because the ABI reserves `r7` and the kernel refuses a call whose
//!   reserved register is not zero. `r7` is also the generated code's address
//!   scratch, so it is not zero on arrival.
//!
//! The stack needs no setup here. The OS establishes `USER_INITIAL_SP` when it
//! loads the image, and every generated function's prologue reserves its own frame
//! — including the sixteen bytes of outgoing argument space the calling
//! convention needs — so the entry sequence's only stack obligation is to leave
//! SP alone.

use alloc::string::String;
use core::{error::Error, fmt};

use lazalith_toolchain::{ObjectFile, ToolchainError, assemble_named};
use lazalith_types::{ArchitectureConfig, WordWidth};

/// The assembler source of the entry sequence.
pub fn startup_source(architecture: ArchitectureConfig) -> String {
    startup_source_for(architecture, ENTRY_SYMBOL)
}

/// The symbol a Lazen program.s `main` reaches the object file as.
///
/// Code generation prefixes every function with `fn.`, so a Lazen `main` is
/// `fn.main` in the object. It is named here because the entry sequence needs it
/// and a literal in two places is a literal that will eventually disagree with
/// itself.
pub const ENTRY_SYMBOL: &str = "fn.main";

/// The entry sequence for a program whose entry has a stated symbol.
///
/// The sequence itself is language-neutral: call the program's entry, then exit with
/// its result. Only the *name* differs, because a C program's `main` is `c.main`
/// in the IR and `fn.c.main` in the object, and a sequence that hardcoded one
// language's name would need a second sequence for the other. One sequence with a
/// name is the difference between a parameter and a fork.
pub fn startup_source_for(architecture: ArchitectureConfig, entry: &str) -> String {
    let mut source = String::from(".arch ");
    source.push_str(isa_name(architecture));
    source.push_str("\n.entry entry\n");
    // The program's entry point. Code generation makes this symbol global even
    // when the source declared it private, because the loader is not a module.
    source.push_str(&alloc::format!(".extern {entry}\n"));
    source.push_str("entry:\n");
    source.push_str(&alloc::format!("         CALL {entry}\n"));
    source.push_str("         MOV r1, r0\n");
    source.push_str("         LI r0, 1\n");
    source.push_str("         LI r7, 0\n");
    source.push_str("         SYSCALL\n");
    source
}

/// The assembler's name for this machine.
fn isa_name(architecture: ArchitectureConfig) -> &'static str {
    match architecture.word_width() {
        WordWidth::W32 => "lz32",
        WordWidth::W64 => "lz64",
    }
}

/// Assembles the entry sequence into an object.
pub fn startup_object(architecture: ArchitectureConfig) -> Result<ObjectFile, StartupError> {
    let source = startup_source(architecture);
    assemble_named("lazen.startup", &source).map_err(StartupError::Assembly)
}

/// The entry sequence for a program whose entry has a stated symbol.
pub fn startup_object_for(
    architecture: ArchitectureConfig,
    entry: &str,
) -> Result<ObjectFile, StartupError> {
    let source = startup_source_for(architecture, entry);
    assemble_named("c.startup", &source).map_err(StartupError::Assembly)
}

/// Why the entry sequence could not be built.
#[derive(Debug)]
pub enum StartupError {
    /// The assembler refused the entry sequence.
    Assembly(ToolchainError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assembly(error) => write!(f, "the startup sequence did not assemble: {error}"),
        }
    }
}

impl Error for StartupError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Assembly(error) => Some(error),
        }
    }
}
