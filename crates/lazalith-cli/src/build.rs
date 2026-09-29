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
//!
//! The compilation itself is `lazalith_driver::build_lazen` — the same stage
//! functions `lazcc` and `lazld` are thin wrappers around. A second build path
//! here would be a second thing to keep correct.

use std::ffi::OsString;
use std::fs;

use lazalith_driver::build_lazen;

use crate::{CliError, Outcome, build_options, image_path, one_file, read_source};

/// `lazen build [file]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    build(one_file("build", arguments)?)
}

/// Builds one program and writes its image.
pub(crate) fn build(explicit: Option<&OsString>) -> Result<Outcome, CliError> {
    let (path, text) = read_source(explicit)?;
    // The stage library, not `RuntimeProgram::build`: the driver coordinates the
    // same `lazen_object` → `link` → `image_bytes` chain that `lazcc` and `lazld`
    // run as separate tools, so `lazen build` and those tools cannot drift apart.
    let build = build_lazen(text.as_str(), &build_options(&path))
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let target = image_path(&path);
    fs::write(&target, &build.bytes).map_err(|error| CliError::io("write", &target, error))?;
    println!(
        "built {} ({} bytes, entry {})",
        target.display(),
        build.bytes.len(),
        build.entry_symbol()
    );
    Ok(Outcome::Done)
}
