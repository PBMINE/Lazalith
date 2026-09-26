//! `lazen build` — compile and link to a `.lzx` image.
//!
//! A build produces two things: the image, and the fact that the image is the
//! program's whole executable. There is no intermediate directory and no
//! intermediate artifact left behind, because the object format is designed to be
//! linked and the linker is the only thing that reads it — a `.lzo` left on disk
//! would be a second way to run a program that the toolchain does not otherwise
//! offer.
//!
//! The image is written next to its source, named after it, so building two
//! programs in one directory does not have one silently overwrite the other.

use std::ffi::OsString;
use std::fs;

use lazalith_runtime::RuntimeProgram;

use crate::{CliError, Outcome, build_options, image_path, one_file, read_source};

/// `lazen build [file]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    build(one_file("build", arguments)?)
}

/// Builds one program and writes its image.
pub(crate) fn build(explicit: Option<&OsString>) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    let program = RuntimeProgram::build(text.as_str(), &build_options(&path))
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let bytes = program
        .to_image_bytes()
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let target = image_path(&path);
    fs::write(&target, &bytes).map_err(|error| CliError::io("write", &target, error))?;
    println!(
        "built {} ({} bytes, entry {})",
        target.display(),
        bytes.len(),
        program.entry_symbol()
    );
    Ok(Outcome::Done)
}
