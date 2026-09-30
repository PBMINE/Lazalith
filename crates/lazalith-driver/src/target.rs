//! A build's target, as a directory it reads from.
//!
//! # Why a build is asked for a sysroot rather than told one
//!
//! §19 asks for the six things a build needs to be separate: the compiler, the C
//! library, the runtime, the startup objects, the OS headers and the target
//! libraries. Four of those were Rust `const`s inside the crates that used them, so a
//! build could not be pointed at a *different* C library or a *different* runtime —
//! there was only ever one, and it was compiled in.
//!
//! [`BuildTarget`] is the seam. It is either
//!
//! - [`Self::BuiltIn`], the `const`s the toolchain has always used, or
//! - [`Self::Sysroot`], a directory on disk, chosen by the caller.
//!
//! **Both are real and both are tested to produce the same object.** That is the
//! point: a sysroot is not a different language or a different library, it is the
//! *same* target described somewhere a build can name. A sysroot whose contents
//! differed from the built-in ones would be a second target, and B15's tests check
//! that it is not.
//!
//! A freestanding build naming a sysroot gets the refusal `lazalith-sysroot` already
//! gives — "this is a freestanding sysroot and has no C library" — rather than
//! silently falling back to the built-in one. A kernel that linked the hosted library
//! because asking for the wrong thing was inconvenient would be a worse outcome than
//! a build that stops.

use std::path::Path;

use lazalith_sysroot::{Sysroot, SysrootError, SysrootFlavour};
use lazalith_types::ArchitectureConfig;

use crate::DriverError;

/// Where a build gets its target from.
///
/// `BuiltIn` is the default because it is what a build gets when it names no
/// sysroot, which is what every build did before B15.
#[derive(Clone, Debug, Default)]
pub enum BuildTarget {
    /// The runtime and library compiled into the toolchain.
    ///
    /// What every build used before B15, and still what a build gets when it names
    /// no sysroot. It is not deprecated: it is the target that needs no directory.
    #[default]
    BuiltIn,
    /// A sysroot on disk.
    Sysroot(Sysroot),
}

impl BuildTarget {
    /// Opens whatever sysroot is at `root`, of either flavour.
    ///
    /// A build tool does not usually know which flavour a sysroot is; it opens the
    /// directory and asks for what it needs. So this does **not** require hosted,
    /// because requiring it would turn "here is a freestanding sysroot" into "here is
    /// not a sysroot", and the message a user needs is the one about the library.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, DriverError> {
        Sysroot::open(root)
            .map(Self::Sysroot)
            .map_err(|error| DriverError::Sysroot { error })
    }

    /// Opens a sysroot and requires it to be the kind this build needs.
    pub fn open_hosted(root: impl AsRef<Path>) -> Result<Self, DriverError> {
        Sysroot::open_as(root, SysrootFlavour::Hosted)
            .map(Self::Sysroot)
            .map_err(|error| DriverError::Sysroot { error })
    }

    /// Writes a new hosted sysroot at `root`, and checks it works.
    ///
    /// **The check is here and not in `lazalith-sysroot` because the capability is
    /// here.** The sysroot crate writes files; compiling C is the driver's job, and a
    /// sysroot crate that depended on the driver to verify its own output would be a
    /// cycle. So `create` writes and this verifies: a sysroot that reports success
    /// while holding a library that does not compile is a sysroot whose first *user*
    /// finds out instead of its writer.
    pub fn create(
        root: impl AsRef<Path>,
        architecture: ArchitectureConfig,
    ) -> Result<Self, DriverError> {
        let target = Sysroot::create(root, SysrootFlavour::Hosted, architecture)
            .map(Self::Sysroot)
            .map_err(|error| DriverError::Sysroot { error })?;
        if let Self::Sysroot(sysroot) = &target {
            verify_c_library(sysroot, architecture)?;
        }
        Ok(target)
    }

    /// Writes a new freestanding sysroot at `root`.
    ///
    /// No C library to check: that is the whole of what freestanding means here.
    pub fn create_freestanding(
        root: impl AsRef<Path>,
        architecture: ArchitectureConfig,
    ) -> Result<Self, DriverError> {
        Sysroot::create(root, SysrootFlavour::Freestanding, architecture)
            .map(Self::Sysroot)
            .map_err(|error| DriverError::Sysroot { error })
    }

    /// The C library's text, from wherever this target keeps it.
    ///
    /// A sysroot's refusal is passed through rather than replaced, because "this is a
    /// freestanding sysroot and has no C library" is a better answer than anything
    /// this crate could invent about it.
    pub fn c_library(&self) -> Result<String, DriverError> {
        match self {
            Self::BuiltIn => Ok(String::from(lazalith_c_runtime::C_RUNTIME)),
            Self::Sysroot(sysroot) => sysroot
                .c_library()
                .map_err(|error| DriverError::Sysroot { error }),
        }
    }

    /// The Lazen runtime's text, from wherever this target keeps it.
    pub fn lazen_runtime(&self) -> Result<String, DriverError> {
        match self {
            Self::BuiltIn => Ok(lazalith_runtime::library_text()),
            Self::Sysroot(sysroot) => sysroot
                .lazen_runtime()
                .map_err(|error| DriverError::Sysroot { error }),
        }
    }

    /// `BuildOptions` that read the Lazen runtime from this target.
    pub fn build_options(
        &self,
        architecture: ArchitectureConfig,
        source_path: String,
    ) -> Result<lazalith_runtime::BuildOptions, DriverError> {
        Ok(lazalith_runtime::BuildOptions {
            architecture,
            source_path,
            prelude: self.lazen_runtime()?,
        })
    }

    /// Every header this target has, read from its sysroot's `include/` directory.
    ///
    /// **Read once, at the start of a build, and carried in [`Headers`] from then on.**
    /// A resolver that opened a file on every `#include` would be a build whose output
    /// could change under it: a header edited halfway through a long build would be read
    /// twice and seen two ways. Reading the set up front costs a directory walk and makes
    /// the build's inputs its inputs.
    ///
    /// A built-in target has no directory, so it has no headers. That is a fact rather
    /// than a gap: the built-in C library carries its own declarations, exactly as a
    /// hosted libc does, and a program including `<stdint.h>` from a built-in target is
    /// told the header is missing rather than being given a header from somewhere else.
    pub fn headers(&self) -> Result<crate::Headers, DriverError> {
        let Self::Sysroot(sysroot) = self else {
            return Ok(crate::Headers::new());
        };
        let root = sysroot.root().join("include");
        let mut headers = crate::Headers::new();
        collect_headers(&root, &root, &mut headers)?;
        Ok(headers)
    }

    /// `CBuildOptions` that read the C library from this target.
    pub fn c_build_options(
        &self,
        architecture: ArchitectureConfig,
        source_path: String,
    ) -> Result<crate::CBuildOptions, DriverError> {
        Ok(crate::CBuildOptions {
            architecture,
            source_path,
            runtime: self.c_library()?,
            library: false,
            headers: self.headers()?,
        })
    }
}

/// Reads every file under `root` into `headers`, naming each by its path from `root`.
///
/// **Recurses, and names by relative path with `/` separators, because that is what a
/// `#include` writes.** A header at `include/lazos/abi.h` is included as
/// `#include <lazos/abi.h>`, and a sysroot whose nested headers were named by file name
/// alone would put `abi.h` at the root of the include path — which compiles, and then
/// picks up a *different* `abi.h` from somewhere else in the include path, which is the
/// kind of bug that is found a week later.
///
/// Non-header files are skipped rather than refused. A sysroot's `include/` directory is
/// a place a person might keep a note, and a build that refused to start because of a
/// `README` would be a build whose failure has nothing to do with the program.
fn collect_headers(
    root: &Path,
    directory: &Path,
    headers: &mut crate::Headers,
) -> Result<(), DriverError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(DriverError::Sysroot {
                error: SysrootError::Io {
                    path: directory.to_path_buf(),
                    message: error.to_string(),
                },
            })
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| DriverError::Sysroot {
            error: SysrootError::Io {
                path: directory.to_path_buf(),
                message: error.to_string(),
            },
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_headers(root, &path, headers)?;
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "h") {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|error| DriverError::Sysroot {
            error: SysrootError::Io {
                path: path.clone(),
                message: error.to_string(),
            },
        })?;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        headers.insert(&relative, &text);
    }
    Ok(())
}

impl From<SysrootError> for DriverError {
    fn from(error: SysrootError) -> Self {
        Self::Sysroot { error }
    }
}
/// Checks that the C library a sysroot is about to be trusted with is valid C.
///
/// **A frontend check, not a build.** The first version compiled the library into an
/// object, and `lazen sysroot` refused to write anything at all, reporting `there is
/// no `main` to start at`. That is the *right* refusal for a program and exactly the
/// wrong check for a library: a C library has no `main`, because it is a collection of
/// definitions a program links against, and demanding an entry point of one is asking
/// a library to be a program.
///
/// So this asks the question a library actually has to pass — does it lex, parse,
/// resolve and type-check? That is `frontend::analyse`, and no diagnostics is the
/// answer. A sysroot holding a library with a type error is found here rather than by
/// its first user.
fn verify_c_library(
    sysroot: &Sysroot,
    _architecture: ArchitectureConfig,
) -> Result<(), DriverError> {
    let library = sysroot
        .c_library()
        .map_err(|error| DriverError::Sysroot { error })?;
    let mut sources = lazalith_types::SourceManager::new();
    let analysis =
        lazalith_c_compiler::frontend::analyse(&mut sources, "lazos-sysroot-libc.c", &library);
    if let Some(first) = analysis.diagnostics.first() {
        return Err(DriverError::Sysroot {
            error: lazalith_sysroot::SysrootError::CLibrary {
                message: format!(
                    "{} diagnostic(s), the first:\n{}",
                    analysis.diagnostics.len(),
                    first.render()
                ),
            },
        });
    }
    Ok(())
}
