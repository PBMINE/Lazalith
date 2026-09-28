//! The manifest, and resolving its dependencies against a directory.
//!
//! # Why a hand-written reader and not a TOML parser
//!
//! This repository has no third-party Rust dependencies, and a TOML parser is not
//! something to write as a side effect of a package step. So this is not a TOML
//! parser: it reads exactly the shape `docs/lazen-applications.md` specifies — four
//! tables, three kinds of value — and **refuses everything else**. A manifest in
//! some other shape is an error with a line number, not a best-effort reading.
//!
//! That is a real limitation and it is the right one here. TOML has arrays of
//! tables, inline tables, multi-line strings, and dates, and a manifest that used
//! any of them would be accepted by a real TOML parser and by nothing else in this
//! repository. A format this reader accepts and another tool also accepts is worth
//! more than a format this reader half-implements.
//!
//! # What "local" means here
//!
//! `[dependencies]` names a package and a version requirement, and the requirement
//! is resolved against **directories on disk**: each immediate subdirectory of a
//! search path is a candidate, its own `lazen.toml` is read, and the highest version
//! that satisfies the requirement wins. There is no registry, no index, no network,
//! and no lock file — step 89 says do not build a registry, and a lock file is a
//! registry's memory of what it resolved, so it waits.
//!
//! Two things are refused rather than guessed, because both would otherwise produce
//! a build that is not the one the manifest asked for:
//!
//! - **A missing dependency.** Named and not found is an error listing where it
//!   looked.
//! - **A cycle.** `docs/lazen-modules.md` says cycles are rejected during name
//!   resolution and reported as a path, so `resolve` reports one as a path too:
//!   `a -> b -> a`.

use alloc::{collections::BTreeMap, string::String, vec, vec::Vec};

use lazalith_os::{PackageVersion, check_name};

/// The name of a manifest.
pub const MANIFEST_NAME: &str = "lazen.toml";
/// The longest a manifest may be.
pub const MANIFEST_MAX_BYTES: usize = 64 * 1024;

/// What can be wrong with a manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ManifestError {
    /// The manifest is longer than a manifest may be.
    TooLong {
        /// Its length.
        bytes: usize,
    },
    /// A line is not in the format this reader accepts.
    Syntax {
        /// Which line, one-based.
        line: u32,
        /// What is wrong with it.
        reason: String,
    },
    /// A section this reader does not know about.
    UnknownSection {
        /// Which line.
        line: u32,
        /// The section's name.
        section: String,
    },
    /// A required field is missing.
    Missing {
        /// The section.
        section: &'static str,
        /// The field.
        field: &'static str,
    },
    /// A field's value is not what the section expects.
    Value {
        /// The field.
        field: &'static str,
        /// What was wrong.
        reason: String,
    },
    /// The application name is not legal.
    Name {
        /// Why.
        reason: &'static str,
    },
    /// A dependency's requirement is not a version requirement this build reads.
    Requirement {
        /// The dependency.
        name: String,
        /// The text that was not a requirement.
        text: String,
    },
}

impl core::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooLong { bytes } => {
                write!(
                    f,
                    "the manifest is {bytes} bytes, which is longer than a manifest may be"
                )
            }
            Self::Syntax { line, reason } => write!(f, "line {line}: {reason}"),
            Self::UnknownSection { line, section } => {
                write!(
                    f,
                    "line {line}: `{section}` is not a section this build reads"
                )
            }
            Self::Missing { section, field } => {
                write!(f, "[{section}] has no `{field}`")
            }
            Self::Value { field, reason } => write!(f, "`{field}` {reason}"),
            Self::Name { reason } => write!(f, "the application name is not legal: {reason}"),
            Self::Requirement { name, text } => {
                write!(
                    f,
                    "the requirement on `{name}` is {text:?}, which is not one this build reads"
                )
            }
        }
    }
}

/// Which architecture an application was built for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Architecture {
    /// Whichever word size the compiler is invoked with.
    #[default]
    Any,
    /// A 32-bit target.
    Lz32,
    /// A 64-bit target.
    Lz64,
}

impl Architecture {
    /// The text this build reads.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Lz32 => "lz32",
            Self::Lz64 => "lz64",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "any" => Some(Self::Any),
            "lz32" => Some(Self::Lz32),
            "lz64" => Some(Self::Lz64),
            _ => None,
        }
    }
}

/// A version requirement, as a manifest writes it.
///
/// `major.minor` and `major.minor.patch` only, with no operators, no wildcards, and
/// no pre-release suffixes. That is deliberate and it is the smallest thing that can
/// resolve: a requirement is a *floor* within one minor series, which is what makes
/// "take the highest that satisfies" well-defined without a registry to consult. A
/// future format can widen it without changing this one, because this one is already
/// a subset of what the format will accept.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionRequirement {
    /// The lowest major version that satisfies it.
    pub major: u16,
    /// The lowest minor version that satisfies it.
    pub minor: u16,
    /// The lowest patch version that satisfies it.
    pub patch: u16,
    /// Whether a higher minor in the same major is allowed.
    pub any_minor: bool,
    /// Whether a higher patch in the same minor is allowed.
    pub any_patch: bool,
}

impl VersionRequirement {
    /// The requirement a bare `0.1` means: `>=0.1.0, <0.2.0`.
    pub const fn caret(major: u16, minor: u16) -> Self {
        Self {
            major,
            minor,
            patch: 0,
            any_minor: false,
            any_patch: true,
        }
    }

    /// Whether a version satisfies this requirement.
    ///
    /// A requirement is a floor **inside one minor series**, which is what makes
    /// "take the highest that satisfies" well-defined with no registry to consult.
    /// A different major is therefore never accepted — `0.1` does not admit `1.0.0`,
    /// and a resolver that let it through would silently resolve a dependency across
    /// an incompatible change, which is the one thing a version number exists to
    /// prevent.
    pub const fn accepts(self, version: PackageVersion) -> bool {
        let (major, minor, patch) = (version.major, version.minor, version.patch);
        if major != self.major {
            return false;
        }
        if minor != self.minor {
            return self.any_minor && minor > self.minor;
        }
        if patch != self.patch {
            return self.any_patch && patch > self.patch;
        }
        true
    }
}

/// A parsed manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    /// The application's name.
    pub name: String,
    /// Its version.
    pub version: PackageVersion,
    /// Its entry file, relative to the manifest.
    pub entry: String,
    /// Which architecture it targets.
    pub architecture: Architecture,
    /// The resources it names, in order.
    pub resources: Vec<String>,
    /// The capabilities it declares, by name, in order.
    pub permissions: Vec<String>,
    /// Its dependencies, by name and requirement.
    pub dependencies: BTreeMap<String, VersionRequirement>,
}

impl Manifest {
    /// A manifest with the four required fields and nothing else.
    pub fn new(name: &str, version: PackageVersion, entry: &str) -> Self {
        Self {
            name: String::from(name),
            version,
            entry: String::from(entry),
            architecture: Architecture::Any,
            resources: Vec::new(),
            permissions: Vec::new(),
            dependencies: BTreeMap::new(),
        }
    }

    /// Reads a manifest.
    pub fn parse(text: &[u8]) -> Result<Self, ManifestError> {
        if text.len() > MANIFEST_MAX_BYTES {
            return Err(ManifestError::TooLong { bytes: text.len() });
        }
        let text = core::str::from_utf8(text).map_err(|_| ManifestError::Syntax {
            line: 1,
            reason: String::from("it is not UTF-8"),
        })?;
        let mut application: BTreeMap<&str, String> = BTreeMap::new();
        let mut resources: Vec<String> = Vec::new();
        let mut permissions: Vec<String> = Vec::new();
        let mut dependencies: BTreeMap<String, VersionRequirement> = BTreeMap::new();
        let mut section = "";
        for (index, raw) in text.lines().enumerate() {
            let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let Some(name) = rest.strip_suffix(']') else {
                    return Err(ManifestError::Syntax {
                        line: line_number,
                        reason: String::from("a section header does not close"),
                    });
                };
                section = match name {
                    "application" => "application",
                    "resources" => "resources",
                    "permissions" => "permissions",
                    "dependencies" => "dependencies",
                    other => {
                        return Err(ManifestError::UnknownSection {
                            line: line_number,
                            section: String::from(other),
                        });
                    }
                };
                continue;
            }
            let Some((key, raw_value)) = line.split_once('=') else {
                return Err(ManifestError::Syntax {
                    line: line_number,
                    reason: String::from("a field has no `=`"),
                });
            };
            let key = key.trim();
            let value = unquote(raw_value.trim());
            match section {
                "application" => {
                    application.insert(key, value);
                }
                "resources" => resources.push(String::from(key)),
                "permissions" => permissions.push(String::from(key)),
                "dependencies" => {
                    let requirement =
                        requirement_of(&value).ok_or_else(|| ManifestError::Requirement {
                            name: String::from(key),
                            text: value.clone(),
                        })?;
                    if dependencies
                        .insert(String::from(key), requirement)
                        .is_some()
                    {
                        return Err(ManifestError::Value {
                            field: "dependencies",
                            reason: String::from("names a dependency twice"),
                        });
                    }
                }
                _ => {
                    return Err(ManifestError::Syntax {
                        line: line_number,
                        reason: String::from("a field appears before any section"),
                    });
                }
            }
        }
        let field = |name: &'static str| -> Result<String, ManifestError> {
            application
                .get(name)
                .cloned()
                .ok_or(ManifestError::Missing {
                    section: "application",
                    field: name,
                })
        };
        let name = field("name")?;
        check_name(name.as_bytes()).map_err(|error| ManifestError::Name {
            reason: match error {
                lazalith_os::LzaError::BadName { reason } => reason,
                lazalith_os::LzaError::TooLong { .. } => "it is longer than a name may be",
                _ => "it is not legal",
            },
        })?;
        let version_text = field("version")?;
        let version = PackageVersion::parse(&version_text).map_err(|_| ManifestError::Value {
            field: "version",
            reason: String::from("is not a major.minor.patch version"),
        })?;
        let entry = field("entry")?;
        let architecture_text = field("architecture")?;
        let architecture = Architecture::parse(&architecture_text).ok_or(ManifestError::Value {
            field: "architecture",
            reason: String::from("is not `any`, `lz32`, or `lz64`"),
        })?;
        for permission in &permissions {
            if !matches!(
                permission.as_str(),
                "console" | "filesystem" | "graphics" | "input"
            ) {
                return Err(ManifestError::Value {
                    field: "permissions",
                    reason: String::from("names a capability this build does not know"),
                });
            }
        }
        Ok(Self {
            name,
            version,
            entry,
            architecture,
            resources,
            permissions,
            dependencies,
        })
    }
}

/// A dependency that resolved, and where it resolved to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDependency {
    /// Its name.
    pub name: String,
    /// The version that was chosen.
    pub version: PackageVersion,
    /// Where its manifest was found.
    pub manifest: String,
    /// Its dependencies, resolved the same way.
    pub dependencies: Vec<ResolvedDependency>,
}

/// Everything a manifest needed, found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolved {
    /// The root manifest, which is not a dependency of anything.
    pub root: Manifest,
    /// Its dependencies, each with its own, in a name-sorted order.
    pub dependencies: Vec<ResolvedDependency>,
}

/// What can be wrong with resolving.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// A dependency was named and nothing provided it.
    Missing {
        /// Its name.
        name: String,
        /// Everywhere that was searched.
        searched: Vec<String>,
    },
    /// The dependencies depend on each other in a circle.
    Cycle {
        /// The path, starting and ending at the same name.
        path: Vec<String>,
    },
    /// A candidate's manifest could not be read.
    Candidate {
        /// The dependency it was a candidate for.
        name: String,
        /// Where it was.
        manifest: String,
        /// What was wrong with it.
        error: ManifestError,
    },
    /// A manifest could not be read.
    Manifest {
        /// Where it was.
        path: String,
        /// What was wrong with it.
        error: ManifestError,
    },
}

impl core::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Missing { name, searched } => {
                write!(f, "no local package provides `{name}`; looked in ")?;
                for (index, place) in searched.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    f.write_str(place)?;
                }
                Ok(())
            }
            Self::Cycle { path } => {
                f.write_str("the dependencies form a cycle: ")?;
                for (index, name) in path.iter().enumerate() {
                    if index > 0 {
                        f.write_str(" -> ")?;
                    }
                    f.write_str(name)?;
                }
                Ok(())
            }
            Self::Candidate {
                name,
                manifest,
                error,
            } => write!(f, "`{name}` at {manifest} is not a candidate: {error}"),
            Self::Manifest { path, error } => write!(f, "{path}: {error}"),
        }
    }
}

/// One directory a resolver looks in, and what it found there.
#[derive(Clone, Debug)]
struct Candidate {
    manifest: Manifest,
    path: String,
}

/// Resolves dependencies against directories on disk.
///
/// The caller supplies what it can read; this crate has no filesystem, so the
/// search path arrives as a list of `name -> manifest text` pairs and the host does
/// the directory walking. That keeps every filesystem question in one place — the
/// CLI, which already has one — and makes the resolver testable without a disk.
#[derive(Clone, Debug, Default)]
pub struct Resolver {
    candidates: Vec<Candidate>,
    searched: Vec<String>,
}

impl Resolver {
    /// A resolver that has seen nothing.
    pub const fn new() -> Self {
        Self {
            candidates: Vec::new(),
            searched: Vec::new(),
        }
    }

    /// Adds every package in one search directory.
    ///
    /// `packages` is `name -> (manifest text, where it was found)`, because a
    /// resolver that decides for itself what a "package" is would be guessing: a
    /// directory is a package if it has a `lazen.toml`, and that is the host's
    /// filesystem to say, not this function's.
    pub fn add_directory(&mut self, where_: &str, packages: &[(String, String)]) {
        self.searched.push(String::from(where_));
        for (name, text) in packages {
            // A candidate whose manifest does not parse is skipped rather than
            // fatal: a search directory may hold things that are not packages, and a
            // resolver that refused to look because of one broken neighbour could
            // never resolve anything. The name is the directory's, so the manifest's
            // own name is not used to filter — but a mismatch is worth refusing,
            // because a package under the wrong name resolves the wrong thing.
            if let Ok(manifest) = Manifest::parse(text.as_bytes())
                && manifest.name == *name
            {
                self.candidates.push(Candidate {
                    manifest,
                    path: String::from(where_),
                });
            }
        }
    }

    /// Where this resolver looked.
    pub fn searched(&self) -> &[String] {
        &self.searched
    }

    /// Resolves a manifest's dependencies, transitively.
    pub fn resolve(&self, root: &Manifest) -> Result<Resolved, ResolveError> {
        let mut done: Vec<ResolvedDependency> = Vec::new();
        let mut path: Vec<String> = vec![root.name.clone()];
        self.walk(root, &mut done, &mut path)?;
        Ok(Resolved {
            root: root.clone(),
            dependencies: done,
        })
    }

    /// One level of the walk, with the cycle path carried down.
    fn walk(
        &self,
        manifest: &Manifest,
        done: &mut Vec<ResolvedDependency>,
        path: &mut Vec<String>,
    ) -> Result<(), ResolveError> {
        for (name, requirement) in &manifest.dependencies {
            if path.iter().any(|seen| seen == name) {
                let mut cycle = path.clone();
                cycle.push(name.clone());
                return Err(ResolveError::Cycle { path: cycle });
            }
            if done.iter().any(|entry| entry.name == *name) {
                continue;
            }
            let chosen = self
                .candidates
                .iter()
                .filter(|candidate| candidate.manifest.name == *name)
                .filter(|candidate| requirement.accepts(candidate.manifest.version))
                .max_by_key(|candidate| candidate.manifest.version)
                .ok_or_else(|| ResolveError::Missing {
                    name: name.clone(),
                    searched: self.searched.clone(),
                })?;
            path.push(name.clone());
            let mut dependencies = Vec::new();
            self.walk(&chosen.manifest, &mut dependencies, path)?;
            path.pop();
            done.push(ResolvedDependency {
                name: name.clone(),
                version: chosen.manifest.version,
                manifest: chosen.path.clone(),
                dependencies,
            });
        }
        Ok(())
    }

    /// The versions of every package this resolver can see, by name.
    ///
    /// For `lazen deps`, and for a person who wants to know what is on disk without
    /// reading a manifest. Sorted, so the output is stable.
    pub fn available(&self) -> Vec<(String, PackageVersion)> {
        let mut versions: BTreeMap<String, PackageVersion> = BTreeMap::new();
        for candidate in &self.candidates {
            versions
                .entry(candidate.manifest.name.clone())
                .and_modify(|current| {
                    if candidate.manifest.version > *current {
                        *current = candidate.manifest.version;
                    }
                })
                .or_insert(candidate.manifest.version);
        }
        versions.into_iter().collect()
    }
}

/// Reads a `major.minor` or `major.minor.patch` requirement.
fn requirement_of(text: &str) -> Option<VersionRequirement> {
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    match parts.next() {
        None => Some(VersionRequirement::caret(major, minor)),
        Some(patch) => {
            let patch = patch.parse().ok()?;
            if parts.next().is_some() {
                return None;
            }
            Some(VersionRequirement {
                major,
                minor,
                patch,
                any_minor: false,
                any_patch: false,
            })
        }
    }
}

/// Drops a `#` comment, if the value is not a string containing one.
///
/// A `#` inside a quoted string is part of the value; a `#` after one is a comment.
/// A full TOML parser would handle both plus escapes, and this handles the subset the
/// manifest format needs, which is what it claims to be.
fn strip_comment(line: &str) -> &str {
    let mut inside = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => inside = !inside,
            '#' if !inside => return &line[..index],
            _ => {}
        }
    }
    line
}

fn unquote(value: &str) -> String {
    let trimmed = value.trim();
    let without_quotes = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    String::from(without_quotes)
}
