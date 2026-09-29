//! The Lazen frontend and backend as one callable stage.
//!
//! Kept in its own file so `lib.rs` reads as the stage list rather than as a
//! compile pipeline, and so the "this calls the crates that own the work and does not
//! reimplement them" claim is checkable by reading a short file.

use lazalith_runtime::{BuildOptions, RuntimeError};
use lazalith_toolchain::ObjectFile;

use crate::{DriverError, Stage};

/// Why the Lazen stages refused.
#[derive(Debug)]
pub enum LazenFrontendError {
    /// The frontend, lowerer or backend refused.
    ///
    /// **The message is already rendered.** A diagnostic that reached a tool has to be
    /// a sentence pointing at a line, not a data structure: the stage boundary is where
    /// a caller stops being able to reach the compiler's source map, so rendering has to
    /// happen before it.
    Stage {
        /// Which stage refused.
        stage: Stage,
        /// The rendered diagnostic.
        message: String,
    },
}

impl core::fmt::Display for LazenFrontendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Stage { stage, message } => write!(f, "{stage}: {message}"),
        }
    }
}

impl core::error::Error for LazenFrontendError {}

impl From<RuntimeError> for LazenFrontendError {
    fn from(source: RuntimeError) -> Self {
        // `RuntimeError` knows which of its own stages failed; `Stage` is the
        // crate-wide vocabulary and the mapping is the whole of what this
        // conversion does. Guessing "the backend" because an object failed to build
        // would be worse than saying `Backend` only where that is true.
        let stage = match &source {
            RuntimeError::Compile(_) => Stage::Frontend,
            RuntimeError::Lower(_) | RuntimeError::Codegen(_) => Stage::Backend,
            RuntimeError::Link(_) => Stage::Link,
            RuntimeError::Startup(_) => Stage::Backend,
            RuntimeError::Image(_) | RuntimeError::Allocation => Stage::Image,
        };
        Self::Stage {
            stage,
            message: source.to_string(),
        }
    }
}

impl From<LazenFrontendError> for DriverError {
    fn from(source: LazenFrontendError) -> Self {
        match source {
            LazenFrontendError::Stage { stage, message } => Self::Lazen { stage, message },
        }
    }
}

/// Compiles a Lazen program to an object, stopping before the link.
///
/// Deliberately the whole of `RuntimeProgram::build` up to the objects: §18's chain
/// runs `source → frontend → IR → optimizer → backend → .lzo`, and this stops at the
/// `.lzo`. Everything after that is [`crate::link`]'s business, and a caller that
/// wanted an object had no way to get one before this existed.
pub fn compile_lazen(source: &str, options: &BuildOptions) -> Result<ObjectFile, DriverError> {
    crate::lazen_object(source, options)
}
