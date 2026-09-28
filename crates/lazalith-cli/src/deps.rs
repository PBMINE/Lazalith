//! `lazen deps` — what a manifest needs, and what is on disk to satisfy it.
//!
//! The step-89 half of "local package/dependency support" that is *about* packages
//! rather than about the container: this is the command a person runs to find out
//! whether a build will resolve, before running one. It reads a manifest, walks a
//! search path, and prints either the resolution or the reason there is not one.
//!
//! Resolution itself is in the toolchain and takes no filesystem, so this file is
//! the only part that knows what a directory looks like. That split is deliberate:
//! a resolver that opened files itself could not be tested without a disk, and the
//! interesting questions — which version wins, what a cycle reports, whether a
//! missing dependency says where it looked — are all answerable without one.

use std::{collections::BTreeMap, ffi::OsString, fs, path::Path};

use lazalith_toolchain::{MANIFEST_NAME, Manifest, ResolveError, ResolvedDependency, Resolver};

use crate::{CliError, Outcome};

/// `lazen deps [--manifest FILE] [--path DIR]`
pub(crate) fn run(arguments: &[OsString]) -> Result<Outcome, CliError> {
    let mut manifest_path: Option<std::path::PathBuf> = None;
    let mut search: Vec<std::path::PathBuf> = Vec::new();
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
            "--path" => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError::usage_with_help("--path wants a directory"))?;
                search.push(std::path::PathBuf::from(value));
            }
            other if other.starts_with('-') => {
                return Err(CliError::usage_with_help(format!(
                    "{other} is not an option"
                )));
            }
            other => {
                return Err(CliError::usage_with_help(format!(
                    "{other} is not an option; deps takes only --manifest and --path"
                )));
            }
        }
        index += 1;
    }
    let manifest_path = manifest_path.unwrap_or_else(|| Path::new(MANIFEST_NAME).to_path_buf());
    let manifest_text =
        fs::read(&manifest_path).map_err(|error| CliError::io("read", &manifest_path, error))?;
    let manifest =
        Manifest::parse(&manifest_text).map_err(|error| CliError::Refused(error.to_string()))?;
    // The default search path is the manifest's own directory, then `packages`
    // inside it: a local dependency is a directory, and a project that depends on
    // something keeps it somewhere obvious.
    let mut directories = search;
    if directories.is_empty() {
        let base = manifest_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        directories.push(base.clone());
        directories.push(base.join("packages"));
    }
    let mut resolver = Resolver::new();
    for directory in &directories {
        resolver.add_directory(&directory.display().to_string(), &read_directory(directory));
    }
    match resolver.resolve(&manifest) {
        Ok(resolved) => {
            println!(
                "{} {} resolves with {} dependenc{}",
                manifest.name,
                manifest.version,
                resolved.dependencies.len(),
                if resolved.dependencies.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            );
            print_dependencies(&resolved.dependencies, 1);
            let unused = resolver
                .available()
                .into_iter()
                .filter(|(name, _)| {
                    !resolved
                        .dependencies
                        .iter()
                        .any(|entry| entry.name == *name)
                })
                .collect::<Vec<_>>();
            if !unused.is_empty() {
                println!("\navailable but not used:");
                for (name, version) in unused {
                    println!("  {name} {version}");
                }
            }
            Ok(Outcome::Done)
        }
        Err(error) => {
            report(&error);
            Ok(Outcome::Refused)
        }
    }
}

fn print_dependencies(entries: &[ResolvedDependency], depth: usize) {
    for entry in entries {
        println!(
            "{}{} {} ({})",
            "  ".repeat(depth),
            entry.name,
            entry.version,
            entry.manifest
        );
        print_dependencies(&entry.dependencies, depth + 1);
    }
}

fn report(error: &ResolveError) {
    eprintln!("the dependencies do not resolve: {error}");
}

/// Every package in one directory: its subdirectory name and its manifest's text.
///
/// A directory with no subdirectory, or a subdirectory with no manifest, contributes
/// nothing — it is not an error, because a search directory may hold other things.
fn read_directory(directory: &Path) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(text) = fs::read(path.join(MANIFEST_NAME)) else {
            continue;
        };
        let Ok(text) = String::from_utf8(text) else {
            continue;
        };
        found.insert(String::from(name), text);
    }
    found.into_iter().collect()
}
