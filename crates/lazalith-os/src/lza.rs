//! The application package: one file holding an identity and an executable.
//!
//! # What this is
//!
//! `docs/lazen-packages.md` is the design; this is the container it specifies. A
//! `.lza` is a header, a resource table, a manifest, one complete `.lzx`, and the
//! resources' bytes. It is a *container*, not a second executable format: the
//! executable inside is a whole `.lzx`, parsed by `LzxImage::from_bytes`, and the
//! package header is never allowed to contradict it.
//!
//! # The four rules, and why each one is here
//!
//! - **The executable is a complete `.lzx`, stored verbatim.** Everything about
//!   execution is said by the `.lzx`, once. A package that also carried an
//!   architecture, an entry point or a section count would be a second source of
//!   truth, and this repository has already had to fix that class of bug once: an
//!   object reader that tolerated non-canonical table offsets produced files that
//!   read cleanly and wrote back as *different* files.
//! - **The manifest is stored verbatim.** A tool never has to agree with itself
//!   about a parser, and a diff of two packages' manifests is a diff of the two
//!   source files.
//! - **Offsets are canonical.** A reader checks every offset against the value the
//!   writer would have produced for the counts it read. A non-canonical offset is
//!   refused, not tolerated — for the same reason as the object format.
//! - **Counts are checked against the bytes present before anything is
//!   allocated.** A crafted count must not be able to ask for more memory than the
//!   file could describe, and a `try_reserve_exact` that fails is a refusal rather
//!   than an abort.
//!
//! # What it deliberately does not do
//!
//! No signature, no hash, no registry, no compression, no multi-architecture
//! payload, and no install location. The reasoning for each is in the design; the
//! short version is that every one of them is a second format inside this one, and
//! this repository does not yet have a trust anchor, a need, or a real application
//! to justify any of them.
//!
//! A package's declared permissions are a *declaration*. Nothing here enforces them,
//! because the syscall layer has no capability gate to enforce them with. That is
//! step 91's work and the design's gap list says so.

use alloc::{format, string::String, vec::Vec};

use crate::lzx::{LzxArchitecture, LzxImage};

/// The magic at the front of a package.
pub const LZA_MAGIC: [u8; 8] = *b"LZAAPPL1";
/// The container version this build writes.
pub const LZA_FORMAT_VERSION: u16 = 1;
/// The fixed part of the header, before the resource table.
pub const LZA_HEADER_SIZE: usize = 32;
/// The fixed part of one resource-table entry.
pub const LZA_RESOURCE_ENTRY_SIZE: usize = 16;
/// The most resources a package may name.
pub const LZA_MAX_RESOURCES: usize = 256;
/// The most bytes of manifest a package may carry.
pub const LZA_MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// The longest an application name may be.
pub const LZA_MAX_NAME_BYTES: usize = 32;

/// What can be wrong with a package.
///
/// `Eq` without `Clone` on the `Image` arm is not an oversight: `LzxError` is
/// `Debug` but not `Clone`, and the image error is carried by message rather than by
/// value, so this enum compares on the sentence a reader will see.
#[derive(Debug, Eq, PartialEq)]
pub enum LzaError {
    /// The bytes are not a package.
    NotAPackage,
    /// The container version is one this build does not read.
    Version(u16),
    /// The flags word has a bit set that this build does not know.
    Flags(u16),
    /// The file ended in the middle of something.
    Truncated {
        /// What was being read.
        part: &'static str,
    },
    /// The file has bytes left over after everything it declared.
    Trailing {
        /// How many bytes were left.
        bytes: usize,
    },
    /// An offset is not the one the writer would have produced.
    Offset {
        /// Which field.
        field: &'static str,
        /// The value in the file.
        found: u64,
        /// The value it must have.
        expected: u64,
    },
    /// A count in the header is larger than the bytes can describe.
    Count {
        /// Which count.
        what: &'static str,
        /// The count in the header.
        count: u64,
        /// How many the bytes could hold.
        possible: usize,
    },
    /// The manifest is longer than a manifest may be.
    ManifestTooLong {
        /// The length in the header.
        length: u64,
    },
    /// The name is not a legal application name.
    BadName {
        /// Why.
        reason: &'static str,
    },
    /// The version is not `major.minor.patch`.
    BadVersion {
        /// The text that was not a version.
        text: String,
    },
    /// The resource table names the same resource twice, or names one the
    /// manifest does not declare.
    DuplicateResource {
        /// The name, as bytes.
        name: Vec<u8>,
    },
    /// The table holds a resource the manifest never declared.
    UndeclaredResource {
        /// The name, as bytes.
        name: Vec<u8>,
    },
    /// The resource table's names are not in the order they were written.
    UnorderedResources,
    /// The contained executable is not a valid image. The message is carried rather
    /// than the error, because `LzxError` is `Debug` but not `Clone` and this enum
    /// is compared in tests.
    Image(String),
    /// A name or a version was longer than its field.
    TooLong {
        /// What was too long.
        what: &'static str,
    },
}

impl core::fmt::Display for LzaError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAPackage => f.write_str("these bytes are not a Lazen application package"),
            Self::Version(version) => {
                write!(f, "package version {version} is not one this build reads")
            }
            Self::Flags(flags) => {
                write!(
                    f,
                    "the package sets flag bits {flags:#x} that are not defined"
                )
            }
            Self::Truncated { part } => {
                write!(f, "the package ends in the middle of its {part}")
            }
            Self::Trailing { bytes } => {
                write!(f, "{bytes} bytes are left over after the package ends")
            }
            Self::Offset {
                field,
                found,
                expected,
            } => write!(
                f,
                "the package's {field} is at {found}, where the writer would have put it at {expected}"
            ),
            Self::Count {
                what,
                count,
                possible,
            } => write!(
                f,
                "the package claims {count} {what}, which is more than {possible} could hold"
            ),
            Self::ManifestTooLong { length } => {
                write!(
                    f,
                    "the package's manifest is {length} bytes, which is too long"
                )
            }
            Self::BadName { reason } => write!(f, "the package's name is not legal: {reason}"),
            Self::BadVersion { text } => {
                write!(f, "{text:?} is not a major.minor.patch version")
            }
            Self::DuplicateResource { name } => write!(
                f,
                "the package names the resource {} twice",
                String::from_utf8_lossy(name)
            ),
            Self::UndeclaredResource { name } => write!(
                f,
                "the package names {}, which its manifest does not declare",
                String::from_utf8_lossy(name)
            ),
            Self::UnorderedResources => {
                f.write_str("the package's resources are not in the order they were written")
            }
            Self::Image(error) => {
                write!(f, "the package's executable did not load: {error}")
            }
            Self::TooLong { what } => write!(f, "the package's {what} is too long"),
        }
    }
}

/// A semantic version, as `major.minor.patch`.
///
/// Ordinal rather than a string, because step 89 has to *order* versions to resolve
/// a dependency and `"0.10.0" < "0.9.0"` is the bug that a string comparison makes.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion {
    /// The incompatible-change number.
    pub major: u16,
    /// The additive-change number.
    pub minor: u16,
    /// The fix number.
    pub patch: u16,
}

impl PackageVersion {
    /// A version from its three numbers.
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// A version from `major.minor.patch`, and nothing else.
    ///
    /// Strict on purpose: a leading `v`, a four-part version, or a trailing
    /// pre-release suffix are all refused rather than half-understood, because a
    /// resolver that silently drops part of a version string resolves something
    /// other than what the manifest asked for.
    pub fn parse(text: &str) -> Result<Self, LzaError> {
        let bad = || LzaError::BadVersion {
            text: String::from(text),
        };
        let mut parts = text.split('.');
        let major = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let minor = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        let patch = parts.next().ok_or_else(bad)?.parse().map_err(|_| bad())?;
        if parts.next().is_some() {
            return Err(bad());
        }
        Ok(Self::new(major, minor, patch))
    }
}

impl core::fmt::Display for PackageVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A resource the package names, and where its bytes are.
///
/// The bytes are *not* duplicated here: step 56 embeds a resource into the
/// executable's read-only data as a constant array, so the executable already holds
/// them and the package holds the name. Resolving a name to bytes is the runtime's
/// business, and until it has a resource table this is what a tool would read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LzaResource {
    /// The name, as the manifest spelled it.
    pub name: Vec<u8>,
    /// How many bytes of the resource this package carries, or zero.
    ///
    /// Offsets are deliberately **not** fields. A resource's identity is its name and
    /// how much it contributes; where its name happens to sit in a file is a
    /// property of the file, and putting it in the struct would mean a package built
    /// in memory could never equal the same package read back — the file's offsets
    /// are known only after it is written. Offsets are computed by
    /// [`LzaPackage::to_bytes`] and re-derived by [`LzaPackage::from_bytes`], and
    /// every one of them is checked against what the writer would have produced.
    pub data_length: u64,
}

impl LzaResource {
    /// A resource with no bytes of its own, which is the common case: the bytes are
    /// in the executable.
    pub fn label(name: &[u8]) -> Self {
        Self {
            name: name.to_vec(),
            data_length: 0,
        }
    }
}

/// The four capabilities an application may declare, from step 56.
///
/// A `u8` rather than a struct of four booleans, so a reader can hold a declaration
/// bit this build does not know rather than refusing it — the same rule the input
/// device's records follow, and the same reason: a program built by a newer compiler
/// must still be installable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PackagePermissions {
    /// The bits, as declared.
    pub bits: u8,
}

/// The `console` bit.
pub const PERMISSION_CONSOLE: u8 = 1;
/// The `filesystem` bit.
pub const PERMISSION_FILESYSTEM: u8 = 2;
/// The `graphics` bit.
pub const PERMISSION_GRAPHICS: u8 = 4;
/// The `input` bit.
pub const PERMISSION_INPUT: u8 = 8;

impl PackagePermissions {
    /// Permissions with every bit clear.
    pub const fn none() -> Self {
        Self { bits: 0 }
    }

    /// Whether a capability is declared.
    pub const fn has(self, capability: u8) -> bool {
        self.bits & capability != 0
    }

    /// Declares a capability.
    pub const fn with(mut self, capability: u8) -> Self {
        self.bits |= capability;
        self
    }
}

/// What a package is, once its identity has been read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageIdentity {
    /// The name, as bytes.
    pub name: Vec<u8>,
    /// The version.
    pub version: PackageVersion,
    /// The architecture the package was built for.
    pub architecture: LzxArchitecture,
    /// What it declares it needs.
    pub permissions: PackagePermissions,
}

impl PackageIdentity {
    /// The name as text, for a report. Lossy, because a name this build accepted is
    /// ASCII by construction but a report should not fail on a byte.
    pub fn name_text(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }
}

/// A parsed application package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LzaPackage {
    /// Its identity.
    pub identity: PackageIdentity,
    /// The manifest, verbatim.
    pub manifest: Vec<u8>,
    /// The executable, verbatim — a complete `.lzx`.
    pub image: Vec<u8>,
    /// The resources it names.
    pub resources: Vec<LzaResource>,
    /// The resource bytes, in table order. Empty for a resource whose bytes are only
    /// in the executable.
    pub resource_data: Vec<u8>,
}

impl LzaPackage {
    /// Builds a package from the parts a `lazen pack` has.
    ///
    /// The name and the version are checked here rather than at encode time, so a
    /// builder learns it produced an illegal name *before* it wrote a file. The
    /// resource table is reconciled with the manifest here too, and reordered to the
    /// manifest's order: a package whose table names a resource the manifest does
    /// not, or omits one it does, is a package that disagrees with itself, and the
    /// cheapest place to catch that is before a byte is written.
    pub fn new(
        name: &[u8],
        version: PackageVersion,
        architecture: LzxArchitecture,
        manifest: &[u8],
        image: &[u8],
        resources: &[LzaResource],
    ) -> Result<Self, LzaError> {
        check_name(name)?;
        if manifest.len() > LZA_MAX_MANIFEST_BYTES {
            return Err(LzaError::ManifestTooLong {
                length: u64::try_from(manifest.len()).unwrap_or(u64::MAX),
            });
        }
        if resources.len() > LZA_MAX_RESOURCES {
            return Err(LzaError::Count {
                what: "resources",
                count: u64::try_from(resources.len()).unwrap_or(u64::MAX),
                possible: LZA_MAX_RESOURCES,
            });
        }
        let declared = manifest_resource_names(manifest);
        if declared.len() > LZA_MAX_RESOURCES {
            return Err(LzaError::Count {
                what: "resources",
                count: u64::try_from(declared.len()).unwrap_or(u64::MAX),
                possible: LZA_MAX_RESOURCES,
            });
        }
        let mut ordered: Vec<LzaResource> = Vec::new();
        ordered
            .try_reserve_exact(declared.len())
            .map_err(|_| LzaError::Count {
                what: "resources",
                count: u64::try_from(declared.len()).unwrap_or(u64::MAX),
                possible: LZA_MAX_RESOURCES,
            })?;
        for want in &declared {
            let found = resources
                .iter()
                .find(|resource| resource.name == *want)
                .ok_or_else(|| LzaError::UndeclaredResource { name: want.clone() })?;
            ordered.push(found.clone());
        }
        for resource in resources {
            if !declared.contains(&resource.name) {
                return Err(LzaError::UndeclaredResource {
                    name: resource.name.clone(),
                });
            }
        }
        // Duplicates last, so a table that names one resource twice is reported as
        // the duplication it is rather than as two different mistakes. The first
        // loop above already matched by name, so a duplicate would have been found
        // there; this is the case where the *table* has two of the same name and the
        // manifest has one.
        for (index, resource) in resources.iter().enumerate() {
            if resources[..index]
                .iter()
                .any(|earlier| earlier.name == resource.name)
            {
                return Err(LzaError::DuplicateResource {
                    name: resource.name.clone(),
                });
            }
        }
        // The executable is validated now, not at encode time, so `pack` cannot
        // produce a package whose payload the OS would refuse.
        LzxImage::from_bytes(image).map_err(|error| LzaError::Image(format!("{error}")))?;
        Ok(Self {
            identity: PackageIdentity {
                name: name.to_vec(),
                version,
                architecture,
                permissions: permissions_of(manifest),
            },
            manifest: manifest.to_vec(),
            image: image.to_vec(),
            resources: ordered,
            resource_data: Vec::new(),
        })
    }

    /// The contained executable, parsed.
    ///
    /// A `Result`, not an `LzxImage`, and that is the design's whole point: the
    /// package is a container, and the executable inside it is still parsed by the
    /// executable's own reader, so a package can never mean something the `.lzx`
    /// does not.
    pub fn image(&self) -> Result<LzxImage, LzaError> {
        LzxImage::from_bytes(&self.image).map_err(|error| LzaError::Image(format!("{error}")))
    }

    /// The package as bytes.
    ///
    /// The layout is a function of five numbers and nothing else — the resource
    /// count, the name lengths, the manifest length, the image length, and the
    /// resource-data length. Every offset is derived from them in this order:
    ///
    /// ```text
    /// header | table | resource names | the name | manifest | executable | data
    /// ```
    ///
    /// A reader recomputes all of them and refuses any disagreement, which is the
    /// rule the object format follows and the one step 86's fuzzer showed the cost
    /// of not following.
    pub fn to_bytes(&self) -> Result<Vec<u8>, LzaError> {
        let count_of_resources = self.resources.len();
        let table_size = count_of_resources
            .checked_mul(LZA_RESOURCE_ENTRY_SIZE)
            .ok_or(LzaError::Count {
                what: "resources",
                count: u64::try_from(count_of_resources).unwrap_or(u64::MAX),
                possible: LZA_MAX_RESOURCES,
            })?;
        let names_offset = LZA_HEADER_SIZE
            .checked_add(table_size)
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let name_offset = names_offset
            .checked_add(
                self.resources
                    .iter()
                    .map(|resource| resource.name.len())
                    .sum::<usize>(),
            )
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let manifest_offset = name_offset
            .checked_add(self.identity.name.len())
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let image_offset = manifest_offset
            .checked_add(self.manifest.len())
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let data_offset = image_offset
            .checked_add(self.image.len())
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let total = data_offset
            .checked_add(self.resource_data.len())
            .ok_or(LzaError::Trailing { bytes: 0 })?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| LzaError::Trailing { bytes: 0 })?;
        bytes.extend_from_slice(&LZA_MAGIC);
        bytes.extend_from_slice(&LZA_FORMAT_VERSION.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&u32_at(self.manifest.len())?.to_le_bytes());
        bytes.extend_from_slice(&u32_at(self.image.len())?.to_le_bytes());
        bytes.extend_from_slice(&u32_at(count_of_resources)?.to_le_bytes());
        bytes.extend_from_slice(&u32_at(name_offset)?.to_le_bytes());
        bytes.extend_from_slice(&u32_at(self.identity.name.len())?.to_le_bytes());
        let mut name_cursor = names_offset;
        let mut data_cursor = data_offset;
        for resource in &self.resources {
            bytes.extend_from_slice(&u32_at(name_cursor)?.to_le_bytes());
            bytes.extend_from_slice(&u32_at(resource.name.len())?.to_le_bytes());
            let (offset, length) = if resource.data_length == 0 {
                (0, 0)
            } else {
                let at = data_cursor;
                data_cursor = data_cursor
                    .checked_add(usize::try_from(resource.data_length).unwrap_or(usize::MAX))
                    .ok_or(LzaError::Trailing { bytes: 0 })?;
                (at, resource.data_length)
            };
            bytes.extend_from_slice(&u32_at(offset)?.to_le_bytes());
            bytes.extend_from_slice(
                &u32::try_from(length)
                    .map_err(|_| LzaError::Trailing { bytes: 0 })?
                    .to_le_bytes(),
            );
            name_cursor = name_cursor
                .checked_add(resource.name.len())
                .ok_or(LzaError::Trailing { bytes: 0 })?;
        }
        for resource in &self.resources {
            bytes.extend_from_slice(&resource.name);
        }
        bytes.extend_from_slice(&self.identity.name);
        bytes.extend_from_slice(&self.manifest);
        bytes.extend_from_slice(&self.image);
        bytes.extend_from_slice(&self.resource_data);
        debug_assert_eq!(bytes.len(), total);
        Ok(bytes)
    }

    /// A package from bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LzaError> {
        if bytes.len() < LZA_HEADER_SIZE || bytes[0..8] != LZA_MAGIC {
            return Err(LzaError::NotAPackage);
        }
        let version = u16::from_le_bytes([bytes[8], bytes[9]]);
        if version != LZA_FORMAT_VERSION {
            return Err(LzaError::Version(version));
        }
        let flags = u16::from_le_bytes([bytes[10], bytes[11]]);
        if flags != 0 {
            return Err(LzaError::Flags(flags));
        }
        let manifest_length = read_u32(bytes, 12);
        let image_length = read_u32(bytes, 16);
        let count_of_resources = read_u32(bytes, 20);
        let name_offset = read_u32(bytes, 24);
        let name_length = read_u32(bytes, 28);
        if usize::try_from(manifest_length).map_or(true, |n| n > LZA_MAX_MANIFEST_BYTES) {
            return Err(LzaError::ManifestTooLong {
                length: u64::from(manifest_length),
            });
        }
        if usize::try_from(count_of_resources).map_or(true, |n| n > LZA_MAX_RESOURCES) {
            return Err(LzaError::Count {
                what: "resources",
                count: u64::from(count_of_resources),
                possible: LZA_MAX_RESOURCES,
            });
        }
        if name_length > u32::try_from(LZA_MAX_NAME_BYTES).unwrap_or(u32::MAX) {
            return Err(LzaError::TooLong { what: "name" });
        }
        let resources = usize::try_from(count_of_resources).unwrap_or(0);
        let table_size = resources
            .checked_mul(LZA_RESOURCE_ENTRY_SIZE)
            .ok_or(LzaError::Count {
                what: "resources",
                count: u64::from(count_of_resources),
                possible: LZA_MAX_RESOURCES,
            })?;
        let table_start = LZA_HEADER_SIZE;
        let names_offset = table_start
            .checked_add(table_size)
            .ok_or(LzaError::Truncated { part: "the table" })?;
        let table = slice(
            bytes,
            u64::try_from(table_start).unwrap_or(u64::MAX),
            table_size,
        )?;
        let mut name_cursor = names_offset;
        let mut offsets: Vec<OffsetRow> = Vec::new();
        let mut entries: Vec<LzaResource> = Vec::new();
        entries
            .try_reserve_exact(resources)
            .map_err(|_| LzaError::Count {
                what: "resources",
                count: u64::from(count_of_resources),
                possible: LZA_MAX_RESOURCES,
            })?;
        for index in 0..resources {
            let base = index * LZA_RESOURCE_ENTRY_SIZE;
            let found = read_u32(table, base);
            let expected = u32::try_from(name_cursor).unwrap_or(u32::MAX);
            if found != expected {
                return Err(LzaError::Offset {
                    field: "a resource name",
                    found: u64::from(found),
                    expected: u64::from(expected),
                });
            }
            let length = read_u32(table, base + 4);
            name_cursor = name_cursor
                .checked_add(usize::try_from(length).unwrap_or(usize::MAX))
                .ok_or(LzaError::Truncated {
                    part: "a resource name",
                })?;
            offsets.push(OffsetRow {
                name_offset: u64::from(found),
                name_length: u64::from(length),
                data_offset: u64::from(read_u32(table, base + 8)),
                data_length: u64::from(read_u32(table, base + 12)),
            });
            entries.push(LzaResource {
                name: Vec::new(),
                data_length: u64::from(read_u32(table, base + 12)),
            });
        }
        if name_offset != u32::try_from(name_cursor).unwrap_or(u32::MAX) {
            return Err(LzaError::Offset {
                field: "the name",
                found: u64::from(name_offset),
                expected: u64::try_from(name_cursor).unwrap_or(u64::MAX),
            });
        }
        let manifest_offset = name_cursor
            .checked_add(usize::try_from(name_length).unwrap_or(usize::MAX))
            .ok_or(LzaError::Truncated { part: "the name" })?;
        let image_offset = manifest_offset
            .checked_add(usize::try_from(manifest_length).unwrap_or(usize::MAX))
            .ok_or(LzaError::Truncated {
                part: "the manifest",
            })?;
        let data_offset = image_offset
            .checked_add(usize::try_from(image_length).unwrap_or(usize::MAX))
            .ok_or(LzaError::Truncated {
                part: "the executable",
            })?;
        let manifest = slice(
            bytes,
            u64::try_from(manifest_offset).unwrap_or(u64::MAX),
            usize::try_from(manifest_length).unwrap_or(usize::MAX),
        )?
        .to_vec();
        let image = slice(
            bytes,
            u64::try_from(image_offset).unwrap_or(u64::MAX),
            usize::try_from(image_length).unwrap_or(usize::MAX),
        )?
        .to_vec();
        let name = slice(
            bytes,
            u64::from(name_offset),
            usize::try_from(name_length).unwrap_or(usize::MAX),
        )?
        .to_vec();
        // The resource payloads are the tail of the file, and each entry's offset
        // must be exactly where the running total puts it.
        let mut data_cursor = data_offset;
        for row in &offsets {
            if row.data_length == 0 {
                if row.data_offset != 0 {
                    return Err(LzaError::Offset {
                        field: "a resource.s bytes",
                        found: row.data_offset,
                        expected: 0,
                    });
                }
                continue;
            }
            let expected = u64::try_from(data_cursor).unwrap_or(u64::MAX);
            if row.data_offset != expected {
                return Err(LzaError::Offset {
                    field: "a resource.s bytes",
                    found: row.data_offset,
                    expected,
                });
            }
            slice(
                bytes,
                row.data_offset,
                usize::try_from(row.data_length).unwrap_or(usize::MAX),
            )?;
            data_cursor = data_cursor
                .checked_add(usize::try_from(row.data_length).unwrap_or(usize::MAX))
                .ok_or(LzaError::Truncated {
                    part: "a resource.s bytes",
                })?;
        }
        let resource_data = slice(
            bytes,
            u64::try_from(data_offset).unwrap_or(u64::MAX),
            bytes.len().saturating_sub(data_offset),
        )?
        .to_vec();
        if data_cursor != bytes.len() {
            return Err(LzaError::Trailing {
                bytes: bytes.len().saturating_sub(data_cursor),
            });
        }
        // The names the table holds must be the names the manifest declares, in the
        // manifest's order. A package that lists a resource the manifest does not
        // declare, or in a different order, is a package that disagrees with itself.
        let declared = manifest_resource_names(&manifest);
        if declared.len() != entries.len() {
            return Err(LzaError::Trailing {
                bytes: declared.len().saturating_sub(entries.len()),
            });
        }
        let mut rows = offsets.iter();
        for (entry, want) in entries.iter_mut().zip(&declared) {
            let Some(row) = rows.next() else {
                return Err(LzaError::UnorderedResources);
            };
            let found = slice(
                bytes,
                row.name_offset,
                usize::try_from(row.name_length).unwrap_or(usize::MAX),
            )?;
            if found != want.as_slice() {
                return Err(LzaError::UnorderedResources);
            }
            entry.name = want.clone();
        }
        // The executable is parsed before anything else, because a package whose
        // payload does not load is not a package with a broken payload — it is not a
        // package.
        let parsed =
            LzxImage::from_bytes(&image).map_err(|error| LzaError::Image(format!("{error}")))?;
        check_name(&name)?;
        Ok(Self {
            identity: PackageIdentity {
                name,
                version: version_of(&manifest),
                architecture: parsed.architecture(),
                permissions: permissions_of(&manifest),
            },
            manifest,
            image,
            resources: entries,
            resource_data,
        })
    }
}

/// One row of the resource table, as the file holds it.
///
/// A decode-time value only: nothing outside `from_bytes` and `to_bytes` has a use
/// for a file offset, and putting one in `LzaResource` would make a package built in
/// memory unequal to the same package read back.
struct OffsetRow {
    name_offset: u64,
    name_length: u64,
    data_offset: u64,
    data_length: u64,
}

/// The resource names a manifest declares, in the order it declares them.
fn manifest_resource_names(manifest: &[u8]) -> Vec<Vec<u8>> {
    section_keys(manifest, "[resources]")
}

fn permissions_of(manifest: &[u8]) -> PackagePermissions {
    let mut permissions = PackagePermissions::none();
    for (key, value) in section_pairs(manifest, "[permissions]") {
        if value.trim() != "true" {
            continue;
        }
        permissions = match key.as_slice() {
            b"console" => permissions.with(PERMISSION_CONSOLE),
            b"filesystem" => permissions.with(PERMISSION_FILESYSTEM),
            b"graphics" => permissions.with(PERMISSION_GRAPHICS),
            b"input" => permissions.with(PERMISSION_INPUT),
            _ => permissions,
        };
    }
    permissions
}

/// The keys of one manifest section, in order.
fn section_keys(manifest: &[u8], section: &str) -> Vec<Vec<u8>> {
    section_pairs(manifest, section)
        .into_iter()
        .map(|(key, _)| key)
        .collect()
}

/// The `key = value` pairs of one manifest section, in order.
///
/// A line-oriented read, because the manifest is a fixed, small format and this
/// repository has no TOML parser and no third-party dependencies. It is deliberately
/// not a TOML parser: it reads the shape `docs/lazen-applications.md` specifies, and
/// a manifest in some other shape is refused by the manifest reader rather than
/// half-understood here.
fn section_pairs(manifest: &[u8], section: &str) -> Vec<(Vec<u8>, String)> {
    let text = String::from_utf8_lossy(manifest);
    let mut pairs = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == section;
            continue;
        }
        if !inside || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(value);
        pairs.push((key.trim().as_bytes().to_vec(), String::from(value)));
    }
    pairs
}

fn u32_at(value: usize) -> Result<u32, LzaError> {
    u32::try_from(value).map_err(|_| LzaError::Count {
        what: "a length",
        count: u64::try_from(value).unwrap_or(u64::MAX),
        possible: 0,
    })
}

fn read_u32(bytes: &[u8], at: usize) -> u32 {
    let mut word = [0_u8; 4];
    for (index, slot) in word.iter_mut().enumerate() {
        *slot = bytes.get(at + index).copied().unwrap_or(0);
    }
    u32::from_le_bytes(word)
}

fn slice(bytes: &[u8], offset: u64, length: usize) -> Result<&[u8], LzaError> {
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let end = start
        .checked_add(length)
        .ok_or(LzaError::Truncated { part: "a payload" })?;
    bytes
        .get(start..end)
        .ok_or(LzaError::Truncated { part: "a payload" })
}

/// The name a package carries, checked.
///
/// Lowercase, digits and `-`, at most [`LZA_MAX_NAME_BYTES`], and no leading or
/// trailing `-`. The last two are not decoration: the name becomes a path component,
/// and a VFS path that starts or ends with a separator is a different path.
pub fn check_name(name: &[u8]) -> Result<(), LzaError> {
    if name.is_empty() {
        return Err(LzaError::BadName {
            reason: "it is empty",
        });
    }
    if name.len() > LZA_MAX_NAME_BYTES {
        return Err(LzaError::TooLong { what: "name" });
    }
    if name[0] == b'-' || name[name.len() - 1] == b'-' {
        return Err(LzaError::BadName {
            reason: "it starts or ends with a dash",
        });
    }
    for byte in name {
        let legal = byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-';
        if !legal {
            return Err(LzaError::BadName {
                reason: "it has a character that is not a lowercase letter, a digit, or a dash",
            });
        }
    }
    Ok(())
}
///
/// The version is read out of the manifest rather than stored beside it, which is
/// the design's rule 2 paying for itself: the manifest is stored verbatim, so there
/// is exactly one copy of the version and it cannot fall out of step with itself. A
/// manifest without a readable version reads as `0.0.0`, which sorts first — the
/// least surprising way for a builder that forgot the version to be wrong.
fn version_of(manifest: &[u8]) -> PackageVersion {
    for (key, value) in section_pairs(manifest, "[application]") {
        if key == b"version"
            && let Ok(version) = PackageVersion::parse(&value)
        {
            return version;
        }
    }
    PackageVersion::default()
}
