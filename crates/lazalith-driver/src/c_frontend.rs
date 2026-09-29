//! The C frontend, and C's backend.
//!
//! # C is a first-class target, so it uses the same backend
//!
//! `binstruction.md` §14 makes C a first-class LZA target and §18 puts one LZA
//! backend in the stage chain. So the C path here is *not* a second compiler and not
//! a stub: it is the C frontend, then the **same** [`lazalith_codegen::generate`]
//! that Lazen uses, and the same `.lzo` the linker already reads.
//!
//! ```text
//! C source → C frontend → Lazalith IR → [the shared LZA backend] → .lzo → lazld → .lzx
//! ```
//!
//! **An earlier draft of this file refused C and named a missing "C-to-object
//! backend".** That was wrong, and checking was cheap: `lazalith_c_compiler::ir::lower`
//! emits a `lazalith_ir::Module` and a `Vec<lazalith_ir::FrameLayout>` — the *same*
//! two types the Lazen lowering emits — and `lazalith_codegen::generate` takes
//! exactly those. The backend was never missing. What was missing was about
//! twenty lines calling it.
//!
//! That is worth writing down, because the refusal looked principled. It was named,
//! it explained itself, and it pointed at a future stage. It was also a claim about
//! the codebase that nobody had checked, made in the same breath as six refusals
//! that *were* accurate. A refusal is not self-justifying: the same discipline that
//! makes a refusal trustworthy — believe the code, not the plan — is what makes it
//! possible to discover the refusal was unnecessary.
//!
//! # What is genuinely not here
//!
//! A C **preprocessor**: no `#include`, no `#define`, no macros. That is recorded in
//! `docs/c-compiler.md` and is real missing work, unrelated to the backend.

use core::fmt;

use lazalith_c_compiler::frontend;
use lazalith_c_compiler::ir::{self, LowerError};
use lazalith_codegen::{CodegenError, CodegenOptions, generate};
use lazalith_toolchain::ObjectFile;
use lazalith_types::{ArchitectureConfig, SourceManager};

use crate::DriverError;

/// Why the C frontend refused.
#[derive(Debug)]
pub enum CFrontendError {
    /// The frontend found a diagnostic.
    ///
    /// **The message is rendered.** A diagnostic that reached a tool has to be a
    /// sentence pointing at a line: the stage boundary is where a caller stops being
    /// able to reach the compiler's source map, so rendering happens before it.
    Diagnostics {
        /// How many the file had.
        count: usize,
        /// The first one, rendered.
        first: String,
    },
    /// The program checked, and the lowering refused.
    Lower {
        /// What the lowering refused.
        detail: String,
    },
    /// The program lowered, and the backend refused.
    Codegen {
        /// What the backend refused.
        detail: String,
    },
    /// An allocation failed while checking.
    Allocation,
}

impl fmt::Display for CFrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Diagnostics { count, first } => {
                if *count == 1 {
                    write!(f, "{first}")
                } else {
                    write!(f, "{first} (and {} more)", count - 1)
                }
            }
            Self::Lower { detail } => {
                write!(f, "the C program could not be lowered: {detail}")
            }
            Self::Codegen { detail } => {
                write!(f, "the C program could not be generated: {detail}")
            }
            Self::Allocation => f.write_str("out of memory while checking the file"),
        }
    }
}

impl core::error::Error for CFrontendError {}

impl From<CFrontendError> for DriverError {
    fn from(source: CFrontendError) -> Self {
        Self::C(source)
    }
}

/// The symbol a C program's `main` reaches the object file as.
///
/// **`fn.` + the IR name, and the `fn.` is the backend's, not this crate's.** Code
/// generation mangles every function as `fn.<ir-name>`, and a C `main` is `c.main`
/// in the IR, so it is `fn.c.main` in the object. It is named here because
/// `lazld --entry` needs it and a literal written twice is a literal that will
/// eventually disagree with itself.
pub const C_OBJECT_ENTRY: &str = "fn.c.main";

/// How to build a C program.
///
/// The mirror of `lazalith_runtime::BuildOptions`, and separate for the same
/// reason: a C program's *runtime* is C, and folding it into Lazen's options would
/// mean one struct with a field only one of the two languages reads.
#[derive(Clone, Debug)]
pub struct CBuildOptions {
    /// The machine to build for.
    pub architecture: ArchitectureConfig,
    /// The path the compiler was told, which goes into the debug block.
    pub source_path: String,
    /// The C runtime composed in front of the program.
    ///
    /// Empty for a freestanding program that defines everything it calls. The Lazen
    /// path has the same choice in `BuildOptions::prelude`, and it is a real choice
    /// rather than a convenience: B25's freestanding kernel will pass the empty
    /// string here and must not get a hosted runtime it did not ask for.
    pub runtime: String,
}

impl CBuildOptions {
    /// A hosted C program: the C runtime in front, on LZ64.
    pub fn hosted(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            runtime: String::from(lazalith_c_runtime::C_RUNTIME),
        }
    }

    /// A freestanding C program: nothing in front but the program.
    pub fn freestanding(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            runtime: String::new(),
        }
    }
}

impl Default for CBuildOptions {
    fn default() -> Self {
        Self::hosted(DEFAULT_PROGRAM_NAME)
    }
}

/// The name a program gets when the caller did not say.
const DEFAULT_PROGRAM_NAME: &str = "program.c";

/// Compiles a C program to an object, stopping before the link.
///
/// **The stages are the same ones Lazen uses.** C is lexed, parsed, resolved and
/// type-checked by `lazalith-c-compiler`, lowered to the shared `lazalith_ir::Module`,
/// and handed to `lazalith_codegen::generate` — the one backend, which takes no
/// argument about which language it was given. What makes the object a *C* object is
/// the frontend in front of it, not a branch anywhere downstream.
///
/// A `.c` file therefore produces a `.lzo` that `lazld` links and the machine runs,
/// with nothing downstream knowing which language it came from. That is the claim
/// §14 and §18 make, and `tests/stages.rs` checks it by running the result.
pub fn compile_c(text: &str, options: &CBuildOptions) -> Result<ObjectFile, DriverError> {
    // The runtime and the program are one translation unit, exactly as the Lazen
    // standard library and a Lazen program are. That is what lets a C program call
    // `putchar` and a Lazen program call `rt::sys::print` without either language
    // knowing about the other's library.
    let unit = if options.runtime.is_empty() {
        String::from(text)
    } else {
        let mut unit = options.runtime.clone();
        unit.push('\n');
        unit.push_str(text);
        unit
    };
    let mut sources = SourceManager::new();
    let analysis = frontend::analyse(&mut sources, options.source_path.as_str(), &unit);
    if !analysis.diagnostics.is_empty() {
        // Every diagnostic, each *rendered*. A stage boundary is the last place a
        // caller can still reach the source map, so a tool that reports
        // `error: unknown name` instead of a line and a caret has thrown away the
        // part of the diagnostic that makes it useful. `analyse` rather than
        // `compile` because a file with four type errors should be told about all
        // four, not one per run.
        return Err(DriverError::C(CFrontendError::Diagnostics {
            count: analysis.diagnostics.len(),
            first: analysis.diagnostics[0].render(),
        }));
    }
    let checked = match analysis.checked {
        Some(checked) => checked,
        None => return Err(DriverError::C(CFrontendError::Allocation)),
    };
    let lowered = ir::lower(&checked).map_err(|error: LowerError| {
        DriverError::C(CFrontendError::Lower {
            detail: error.to_string(),
        })
    })?;
    let program = generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions {
            architecture: options.architecture,
            source_path: options.source_path.clone(),
        },
        &unit,
    )
    .map_err(|error: CodegenError| {
        DriverError::C(CFrontendError::Codegen {
            detail: error.to_string(),
        })
    })?;
    Ok(program.object().clone())
}
