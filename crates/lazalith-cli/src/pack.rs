//! `lazen pack` — turn a manifest and a built image into one `.lza` file.
//!
//! This is the packaging half of step 89. Where the package *lands* is not here:
//! `docs/lazen-packages.md` says a file that records where it goes has to be
//! rewritten on every move, so `pack` writes next to the manifest and `install` —
//! which is a system's decision, not a build's — is not a step-89 command.
//!
//! The build is not here either. `pack` takes a manifest and an image that already
//! exist, so it works on a machine that has no compiler: the point of a package is
//! that the person receiving it does not need one.

use std::{ffi::OsString, fs, path::Path};

use lazalith_os::{LzaPackage, LzaResource};
use lazalith_toolchain::{Architecture, MANIFEST_NAME, Manifest};

use crate::{CliError, Outcome};

/// `lazen pack [--manifest FILE] [--image FILE] [OUT]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let mut manifest_path: Option<std::path::PathBuf> = None;
    let mut image_path: Option<std::path::PathBuf> = None;
    let mut output: Option<std::path::PathBuf> = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy().into_owned();
        match argument.as_str() {
            "--manifest" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::usage_with_help("--manifest wants a path"))?;
                manifest_path = Some(std::path::PathBuf::from(value));
            }
            "--image" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::usage_with_help("--image wants a path"))?;
                image_path = Some(std::path::PathBuf::from(value));
            }
            other if other.starts_with('-') => {
                return Err(CliError::usage_with_help(format!(
                    "{other} is not an option"
                )));
            }
            other => {
                if output.is_some() {
                    return Err(CliError::usage_with_help(String::from(
                        "pack takes one output path",
                    )));
                }
                output = Some(std::path::PathBuf::from(other));
            }
        }
        index += 1;
    }
    let manifest_path = manifest_path.unwrap_or_else(|| Path::new(MANIFEST_NAME).to_path_buf());
    let manifest_text =
        fs::read(&manifest_path).map_err(|error| CliError::io("read", &manifest_path, error))?;
    let manifest =
        Manifest::parse(&manifest_text).map_err(|error| CliError::Refused(error.to_string()))?;
    // The image defaults to the one `lazen build` wrote for the manifest's entry,
    // which is what build names it after: `src/main.lz` builds to `src/main.lzx`.
    // Guessing any other name would make `pack` fail for a reason that has nothing
    // to do with packaging, and the fix would be to pass `--image` for every project.
    let default_image = {
        let entry = manifest.entry.replace(".lz", ".lzx");
        manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(if entry.is_empty() {
                String::from("main.lzx")
            } else {
                entry
            })
    };
    let image_path = image_path.unwrap_or(default_image);
    let image = fs::read(&image_path).map_err(|error| CliError::io("read", &image_path, error))?;
    let resources: Vec<LzaResource> = manifest
        .resources
        .iter()
        .map(|name| LzaResource::label(name.as_bytes()))
        .collect();
    let package = LzaPackage::new(
        manifest.name.as_bytes(),
        manifest.version,
        architecture_of(manifest.architecture, &image)?,
        &manifest_text,
        &image,
        &resources,
    )
    .map_err(|error| CliError::Refused(error.to_string()))?;
    let bytes = package
        .to_bytes()
        .map_err(|error| CliError::Refused(error.to_string()))?;
    let target = output.unwrap_or_else(|| {
        manifest_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!("{}.lza", manifest.name))
    });
    fs::write(&target, &bytes).map_err(|error| CliError::io("write", &target, error))?;
    println!(
        "packed {} {} ({} bytes, {} resources)",
        manifest.name,
        manifest.version,
        bytes.len(),
        manifest.resources.len()
    );
    Ok(Outcome::Done)
}

/// The architecture the package records: the one the manifest pins, or the one the
/// image was actually built for.
///
/// The image wins when the manifest says `any`, because the executable is the
/// authority on execution. A manifest that pinned a word width the image does not
/// have is an error rather than a coercion, because coercing it would produce a
/// package that lies about the only thing it exists to describe.
fn architecture_of(
    requested: Architecture,
    image: &[u8],
) -> Result<lazalith_os::LzxArchitecture, CliError> {
    let built = lazalith_os::LzxImage::from_bytes(image)
        .map_err(|error| CliError::Refused(format!("the image did not load: {error}")))?
        .architecture();
    let matches = match requested {
        Architecture::Any => true,
        Architecture::Lz32 => built == lazalith_os::LzxArchitecture::Lz32,
        Architecture::Lz64 => built == lazalith_os::LzxArchitecture::Lz64,
    };
    if matches {
        Ok(built)
    } else {
        Err(CliError::Refused(format!(
            "the manifest asks for {} but the image is {}",
            requested.as_str(),
            built_name(built)
        )))
    }
}

fn built_name(architecture: lazalith_os::LzxArchitecture) -> &'static str {
    match architecture {
        lazalith_os::LzxArchitecture::Lz32 => "lz32",
        lazalith_os::LzxArchitecture::Lz64 => "lz64",
    }
}
