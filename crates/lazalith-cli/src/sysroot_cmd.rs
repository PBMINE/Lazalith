//! `lazen sysroot` — write a target sysroot.
//!
//! # Why a command for it, and not just a crate
//!
//! §19 asks for a target sysroot: a directory a build reads its headers, libraries,
//! runtime and startup objects from. A `Sysroot` type that only a library could
//! construct would satisfy the type and not the requirement, because the thing §19
//! wants is something a *user* can point a build at — a path, in a shell, that either
//! is a sysroot or is not.
//!
//! So this writes one, and `lazcc --sysroot` reads one, and the test that matters is
//! that the second uses what the first wrote.
//!
//! # Both flavours, because the difference is the point
//!
//! §19's last sentence asks for "a clear distinction between hosted programs and
//! freestanding kernel builds". A hosted sysroot gets a C library, a runtime and
//! headers; a freestanding one gets headers and startup objects and *refuses* the
//! library. `--freestanding` writes the second, so that B25's kernel work has
//! somewhere to point and finds out immediately if it accidentally asks for a hosted
//! library.

use std::ffi::OsString;
use std::path::PathBuf;

use lazalith_driver::BuildTarget;
use lazalith_types::ArchitectureConfig;

use crate::CliError;
use crate::Outcome;

/// `lazen sysroot <dir> [--freestanding]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    // Parsed here rather than by `one_file`, which takes exactly one argument and no
    // flags. The rule this follows: a flag is a flag, a path is a path, and a second
    // path is a mistake worth reporting rather than ignoring.
    let mut directory: Option<PathBuf> = None;
    let mut freestanding = false;
    for argument in arguments {
        let text = argument.to_string_lossy().into_owned();
        match text.as_str() {
            "--freestanding" => freestanding = true,
            "--target" => {
                return Err(CliError::usage_with_help(
                    "lazen sysroot builds for the machine the toolchain targets, which is \
                     LZ64. A 32-bit sysroot is B28's work, and `lazcc --target lz32` is \
                     refused by the one backend rather than quietly producing a 64-bit \
                     sysroot under a 32-bit name",
                ));
            }
            other if other.starts_with('-') => {
                return Err(CliError::usage_with_help(format!(
                    "lazen sysroot does not take {other}"
                )));
            }
            path => {
                if directory.is_some() {
                    return Err(CliError::usage_with_help(
                        "lazen sysroot takes one directory, and two were given",
                    ));
                }
                directory = Some(PathBuf::from(path));
            }
        }
    }
    let root = directory
        .ok_or_else(|| CliError::usage_with_help("lazen sysroot needs a directory to write"))?;

    if freestanding {
        BuildTarget::create_freestanding(&root, ArchitectureConfig::lz64())
    } else {
        BuildTarget::create(&root, ArchitectureConfig::lz64())
    }
    .map_err(|error| CliError::Refused(error.to_string()))?;

    let flavour = if freestanding {
        "freestanding"
    } else {
        "hosted"
    };
    println!("wrote a {flavour} sysroot at {}", root.display());
    // The contents printed, because a sysroot a user cannot see inside is a sysroot
    // they cannot check, and "what is in this directory" is a question a build tool
    // should answer before a build does.
    println!("  include/lazos/   abi.h, syscall.h");
    if !freestanding {
        println!("  lib/{}", lazalith_sysroot::C_LIBRARY_FILE);
        println!("  runtime/{}", lazalith_sysroot::RUNTIME_FILE);
    }
    for language in lazalith_sysroot::EntryLanguage::ALL {
        println!(
            "  crt/{}",
            lazalith_sysroot::startup_name(ArchitectureConfig::lz64(), language)
        );
    }
    if freestanding {
        // Said plainly, because a freestanding sysroot with an empty `lib/` looks
        // like a mistake and is not one. The distinction is the requirement, and a
        // user who does not know it will assume the tool is broken.
        println!("  (freestanding: no C library and no hosted runtime, by design)");
    }
    Ok(Outcome::Done)
}
