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
    /// Whether this translation unit is a **library** rather than a program.
    ///
    /// **A library is a C file with no `main`, and refusing one would stop §16 before
    /// it starts.** A library is a collection of definitions that something else is
    /// linked into; demanding an entry point of it is asking a library to be a program.
    /// With this set, a missing `main` is the fact being stated rather than an error.
    pub library: bool,
    /// The headers `#include` may find.
    ///
    /// **A list of texts, not a directory and not a trait object.** A build that knows
    /// where its headers are has already read them, and handing the front end a map means
    /// three things at once: the preprocessor's `IncludeResolver` stays a trait the caller
    /// implements, `CBuildOptions` stays `Clone + Debug` with no lifetime and no `Box`,
    /// and a build is *reproducible* — the headers a build used are the ones in this
    /// struct, so a sysroot changing under a running build cannot change its output.
    ///
    /// Empty means "no headers", which is a fact and not a gap: a program with no
    /// `#include` behaves identically with headers and without, and one *with* an
    /// `#include` gets a diagnostic naming the header it could not find.
    pub headers: Headers,
}

/// The headers a C build may include, as `(name, text)`.
///
/// **A newtype rather than a bare `Vec`,** because a `Vec<(String, String)>` in a build
/// options struct is a field whose units are unclear, and the compiler turns that into a
/// diagnostic about a tuple. The name is the one C spells: `lazos/syscall.h`, with the
/// directory, and without the `<>` or `""` a program wrote around it.
#[derive(Clone, Debug, Default)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    /// No headers.
    pub fn new() -> Self {
        Headers::default()
    }

    /// Adds a header, replacing any header of the same name.
    pub fn insert(&mut self, name: &str, text: &str) {
        self.entries
            .retain(|(existing, _)| existing != name);
        self.entries.push((name.to_string(), text.to_string()));
    }

    /// How many headers there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The names, sorted, which is what a diagnostic about a missing one should offer.
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .entries
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    /// A resolver over these headers.
    ///
    /// The map the preprocessor takes, built from this list. Returning a resolver rather
    /// than exposing the entries is what keeps the preprocessor's trait the only thing
    /// that has to know how headers are stored.
    pub fn resolver(&self) -> lazalith_c_compiler::MapIncludes {
        let mut map = lazalith_c_compiler::MapIncludes::new();
        for (name, text) in &self.entries {
            map.insert(name, text);
        }
        map
    }
}

impl CBuildOptions {
    /// A hosted C program: the C runtime in front, on LZ64.
    pub fn hosted(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            runtime: String::from(lazalith_c_runtime::C_RUNTIME),
            library: false,
            headers: Headers::new(),
        }
    }

    /// Marks this translation unit as a library: definitions with no `main`.
    ///
    /// The runtime stays, because a library usually calls the C library too ''"strlen"''.
    /// What changes is only the entry-point requirement.
    pub fn as_library(mut self) -> Self {
        self.library = true;
        self
    }

    /// A freestanding C program: nothing in front but the program.
    pub fn freestanding(source_path: impl Into<String>) -> Self {
        Self {
            architecture: ArchitectureConfig::lz64(),
            source_path: source_path.into(),
            runtime: String::new(),
            library: false,
            headers: Headers::new(),
        }
    }

    /// This build.s headers.
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = headers;
        self
    }
}

impl Default for CBuildOptions {
    fn default() -> Self {
        Self::hosted(DEFAULT_PROGRAM_NAME)
    }
}

/// The name a program gets when the caller did not say.
const DEFAULT_PROGRAM_NAME: &str = "program.c";

/// Compiles a C translation unit to an object, stopping before the link.
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
///
/// # A library and a program are both translation units
///
/// §16 requires C, Lazen and assembly to converge into one object pipeline, and the
/// first thing that stopped them converging was that there was no way to build a C
/// file that is not a program. A C *library* has no `main` — it is linked into
/// something that has one — so `compile_c` does not demand an entry point, and the
/// object records that it has none. See [`CBuildOptions::library`].
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
    // The preprocessor gets the build's headers and the target's width. **Both, and in
    // that order, because a header that picks a word size needs to know the target and a
    // target that is wrong would otherwise be discovered by a link error.**
    let mut resolver = options.headers.resolver();
    let architecture = if options.architecture.word_bits() == 32 {
        "lz32"
    } else {
        "lz64"
    };
    let mut includes = lazalith_c_compiler::Includes::for_arch(architecture, &mut resolver);
    let analysis = frontend::analyse_for(
        &mut sources,
        options.source_path.as_str(),
        &unit,
        &mut includes,
    );
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
    // A library and a program differ in one thing: whether they have to have a
    // `main`. Everything after this line is identical, and that is the point of §16 —
    // there is no second object pipeline, there is one pipeline and two questions
    // about the entry point.
    let lowered = if options.library {
        ir::lower_library(&checked)
    } else {
        ir::lower(&checked)
    }
    .map_err(|error: LowerError| {
        DriverError::C(CFrontendError::Lower {
            detail: error.to_string(),
        })
    })?;
    // `generate` marks the entry symbol callable, and a library has none: `None` is
    // passed straight through, so the object records that it is not entered.
    let entry = lowered.entry.clone();
    let program = generate(
        &lowered.module,
        &lowered.frames,
        entry.as_deref(),
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
    let mut object = program.object().clone();
    // §17: one debug-information pipeline, and it has to name the file a user wrote.
    //
    // A C build is `libc + program` compiled as one text, so every span in the program
    // is offset by however much libc precedes it, and the debug source is named for
    // the user's file while holding libc's text as well. A debugger reading that shows
    // a line number from `libc` while claiming to be in `hello.c` — confidently wrong,
    // which is worse than silent.
    //
    // The split is done in the toolchain, on the object, because that is where the
    // mappings are and where the format is known. The offset is derived from the same
    // composition the compiler was given, so it cannot disagree with it.
    // The offset is derived from the same composition the compiler was given, so it
    // cannot disagree with it, and the names are in *text* order: the C build is
    // `libc + program`, so the runtime is the head. Getting that pair the wrong way
    // round produces a debugger that is confidently right about the wrong file --
    // every line number in `hello.c` would resolve into libc.
    if !options.runtime.is_empty()
        && let Some(index) = object
            .debug_sources()
            .iter()
            .position(|source| source.path() == options.source_path)
    {
        let offset = u32::try_from(options.runtime.len() + 1).unwrap_or(u32::MAX);
        object
            .split_debug_source(index, offset, C_RUNTIME_NAME, &options.source_path)
            .map_err(DriverError::Object)?;
    }
    Ok(object)
}

/// The name the C library's half of a composed unit is recorded under.
///
/// **Not `libc.c`,** which is the file name a sysroot gives it. This is the *debug
/// source* for a build where the library is compiled in, and there is no file on disk
/// behind it — a debugger that printed a path the user could open would be promising
/// something that does not exist.
pub const C_RUNTIME_NAME: &str = "<lazalith-c-runtime>";
