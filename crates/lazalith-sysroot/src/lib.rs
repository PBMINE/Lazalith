//! B15: the target sysroot.
//!
//! # What a sysroot is for
//!
//! §19 asks for a directory a build reads its target from:
//!
//! ```text
//! sysroot/
//!   include/     OS headers, for C
//!   lib/         target libraries, as objects
//!   crt/         startup objects
//!   runtime/     the language runtime
//! ```
//!
//! and for the six things that are currently tangled together to be separate: the
//! compiler, the C library, the runtime, the startup objects, the OS headers and the
//! target libraries.
//!
//! **Before this crate, four of those six did not exist separately at all.** The C
//! library was a Rust `const` inside `lazalith-c-runtime`. The Lazen runtime was a
//! Rust `const` inside `lazalith-runtime`. The startup object was not a file but
//! assembly the linker generated on the spot, from a `format!` in
//! `lazalith-runtime/src/startup.rs`. And the OS headers did not exist in any form —
//! there was no C header for the ABI, so no C program could `#include` anything.
//!
//! That last one is worth pausing on, because it is the difference between "C is a
//! first-class target" and "C is a target with a compiler attached". B14 made a C
//! program compile, link and run. But it could only do so by calling library
//! functions whose declarations it had to have written out by hand, against a signature
//! nobody had written down anywhere except a `match` in the C front end. The header
//! and that `match` were the same knowledge in two places, and a header generated
//! from the ABI's own table cannot disagree with it.
//!
//! # Hosted and freestanding, which is the point
//!
//! §19's last sentence: "a clear distinction between hosted programs and freestanding
//! kernel builds". A [`SysrootFlavour`] is that distinction, and it is enforced by
//! what the sysroot *contains* rather than by a flag a build passes:
//!
//! - a **hosted** sysroot has a C library, a Lazen runtime and OS headers;
//! - a **freestanding** sysroot has a startup object and headers, and **no** library
//!   and **no** hosted runtime.
//!
//! So `CBuildOptions::freestanding` in B14 was a field with nothing behind it. Here it
//! has somewhere to point: a kernel build opens a freestanding sysroot, asks it for a
//! startup object, and is *told* there is no hosted C library — rather than silently
//! linking one because the toolchain had a `const` in it.
//!
//! # Nothing is invented here
//!
//! Every file this writes is either already in the tree or generated from the ABI's
//! own tables. The headers come from `lazalith_os_abi::ABI_SYSCALLS` and
//! `lazalith_c_compiler::types::abi_signature`; the startup object comes from
//! `lazalith_runtime::startup_object_for`; the C library is the existing
//! `lazalith_c_runtime::C_RUNTIME`. If the ABI grows a syscall, the header grows a
//! declaration, because there is no second place for it to be written.

#![deny(missing_docs)]

use core::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use lazalith_c_compiler::ctypes::CType;
use lazalith_c_compiler::types::abi_signature;
use lazalith_os_abi::ABI_SYSCALLS;
use lazalith_toolchain::ObjectFile;
pub mod freestanding;

use lazalith_types::ArchitectureConfig;

/// The four directories §19 names, in the order it lists them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Directory {
    /// OS headers, for C.
    Include,
    /// Target libraries, as objects.
    Lib,
    /// Startup objects.
    Crt,
    /// The language runtime.
    Runtime,
}

impl Directory {
    /// Every directory, in §19's order.
    pub const ALL: [Self; 4] = [Self::Include, Self::Lib, Self::Crt, Self::Runtime];

    /// The directory's name on disk.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Lib => "lib",
            Self::Crt => "crt",
            Self::Runtime => "runtime",
        }
    }

    /// Whether a **freestanding** sysroot has this directory.
    ///
    /// All four exist, because §19's shape is the shape either way. What differs is
    /// what is *in* them, and that is enforced by the reader rather than here: a
    /// freestanding sysroot's `lib/` and `runtime/` are present and empty, which is
    /// more honest than not creating them, because a build that reaches for a library
    /// then gets "there is no C library here" instead of "no such directory".
    pub const fn in_freestanding(self) -> bool {
        true
    }
}

impl fmt::Display for Directory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which kind of program a sysroot is for.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SysrootFlavour {
    /// An ordinary program: a C library, a runtime, headers, a startup object.
    Hosted,
    /// A kernel: headers and a startup object, and nothing else.
    Freestanding,
}

impl SysrootFlavour {
    /// Whether this flavour has a hosted C library.
    pub const fn has_c_library(self) -> bool {
        matches!(self, Self::Hosted)
    }

    /// Whether this flavour has the hosted language runtime.
    pub const fn has_runtime(self) -> bool {
        matches!(self, Self::Hosted)
    }
}

impl fmt::Display for SysrootFlavour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Hosted => "hosted",
            Self::Freestanding => "freestanding",
        })
    }
}

/// Why a sysroot was refused.
#[derive(Debug)]
pub enum SysrootError {
    /// A directory §19 names is not there.
    MissingDirectory {
        /// Which one.
        directory: Directory,
        /// Where it was looked for.
        root: PathBuf,
    },
    /// The startup object a build needs is not in `crt/`.
    MissingStartup {
        /// Where it was looked for.
        path: PathBuf,
    },
    /// Something a build needs from this flavour is not in it.
    ///
    /// **The freestanding case, named.** A kernel build that reaches for a hosted
    /// library gets this, and the message says which flavour was opened, so the
    /// failure is "wrong sysroot" rather than "missing file".
    NotInFlavour {
        /// What was asked for.
        what: &'static str,
        /// The flavour that does not have it.
        flavour: SysrootFlavour,
    },
    /// The sysroot on disk is not the kind of sysroot the caller needs.
    WrongFlavour {
        /// What the directory turned out to be.
        found: SysrootFlavour,
        /// What the caller needed.
        wanted: SysrootFlavour,
        /// Where it looked.
        root: PathBuf,
    },
    /// A file that must exist for a sysroot to be usable does not.
    MissingFile {
        /// The path.
        path: PathBuf,
    },
    /// The filesystem said no.
    Io {
        /// The path.
        path: PathBuf,
        /// What it said.
        message: String,
    },
    /// A startup object would not assemble.
    Startup {
        /// What it said.
        message: String,
    },
    /// The C library would not compile.
    CLibrary {
        /// What it said.
        message: String,
    },
}

impl fmt::Display for SysrootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingDirectory { directory, root } => write!(
                f,
                "{} is not a sysroot: it has no {directory}/ directory",
                root.display()
            ),
            Self::MissingStartup { path } => write!(
                f,
                "there is no startup object at {}. A sysroot without one cannot build \
                 an image, because the image has to start somewhere",
                path.display()
            ),
            Self::NotInFlavour { what, flavour } => write!(
                f,
                "this is a {flavour} sysroot and has no {what}. A {flavour} build gets \
                 no hosted library: that is what {flavour} means"
            ),
            Self::WrongFlavour {
                found,
                wanted,
                root,
            } => write!(
                f,
                "{} is a {found} sysroot and this build needs a {wanted} one",
                root.display()
            ),
            Self::MissingFile { path } => write!(f, "{} does not exist", path.display()),
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Startup { message } => {
                write!(f, "the startup object did not assemble: {message}")
            }
            Self::CLibrary { message } => {
                write!(f, "the C library did not compile: {message}")
            }
        }
    }
}

impl std::error::Error for SysrootError {}

/// A target sysroot on disk.
#[derive(Clone, Debug)]
pub struct Sysroot {
    root: PathBuf,
    flavour: SysrootFlavour,
}

/// Which language's entry a startup object starts.
///
/// **A sysroot needs one per language, and the evidence is that it does.** The first
/// version wrote a single `crt1-lz64.lzo` using the Lazen entry, and a C program
/// linked against it failed with `UndefinedSymbol: fn.main` — the object was real,
/// assembled, on disk, and started the wrong function. §19 asks for *startup objects*
/// in the plural, and a C program's `main` is `fn.c.main` where a Lazen program's is
/// `fn.main`; one file cannot start both.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EntryLanguage {
    /// A Lazen program's `main`, which is `fn.main` in an object.
    Lazen,
    /// A C program's `main`, which is `fn.c.main` in an object.
    C,
}

impl EntryLanguage {
    /// Every entry language a sysroot has startup objects for.
    pub const ALL: [Self; 2] = [Self::Lazen, Self::C];

    /// The label used in the object file's name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lazen => "lazen",
            Self::C => "c",
        }
    }

    /// The symbol the startup sequence calls.
    ///
    /// **Asked of the stages that own the two names.** `fn.main` is the Lazen
    /// lowering's entry, and `fn.c.main` is the C lowering's IR name put through
    /// codegen's own mangling. Neither is written out here, because a constant in
    /// this crate that spelled either one would be a third spelling of a name two
    /// other crates already own — and B14's `C_OBJECT_ENTRY` was exactly such a
    /// third spelling before it moved to the stage that owns it.
    pub fn symbol(self) -> String {
        match self {
            Self::Lazen => String::from(lazalith_runtime::ENTRY_SYMBOL),
            Self::C => lazalith_codegen::object_symbol(&lazalith_c_compiler::ir::ir_name("main")),
        }
    }
}

/// The name of the startup object for a machine and an entry language.
pub fn startup_name(architecture: ArchitectureConfig, language: EntryLanguage) -> String {
    format!("crt1-{}-{}.lzo", machine(architecture), language.as_str())
}

/// The machine's assembly name, which is the ISA's.
fn machine(architecture: ArchitectureConfig) -> &'static str {
    match architecture.word_width() {
        lazalith_types::WordWidth::W32 => "lz32",
        lazalith_types::WordWidth::W64 => "lz64",
    }
}

impl Sysroot {
    /// Writes a sysroot at `root`, refusing to overwrite a directory that has one.
    ///
    /// The six things §19 asks to be separate are separate here: `include/` holds
    /// headers generated from the ABI, `lib/` holds the C library as an object,
    /// `crt/` holds the startup object, and `runtime/` holds the language runtime.
    pub fn create(
        root: impl AsRef<Path>,
        flavour: SysrootFlavour,
        architecture: ArchitectureConfig,
    ) -> Result<Self, SysrootError> {
        let root = root.as_ref().to_path_buf();
        for directory in Directory::ALL {
            let path = root.join(directory.as_str());
            fs::create_dir_all(&path).map_err(|error| SysrootError::Io {
                path,
                message: error.to_string(),
            })?;
        }
        let sysroot = Self { root, flavour };
        sysroot.write_headers(architecture)?;
        sysroot.write_startup(architecture)?;
        if flavour.has_c_library() {
            sysroot.write_c_library()?;
            sysroot.write_runtime()?;
        }
        sysroot.write_kernel_library()?;
        Ok(sysroot)
    }

    /// Opens an existing sysroot and works out what kind it is.
    ///
    /// **The flavour is read from the directory, not told to it.** A caller that said
    /// "hosted" about a sysroot with no C library in it would get a refusal later and
    /// a confusing one — `No such file or directory` for a file the caller believed
    /// existed. A sysroot knows what it is because of what is in it, and asking it to
    /// guess otherwise is asking it to agree with a claim.
    ///
    /// The test is the C library, because that is the thing the two flavours
    /// actually differ about. A sysroot with `lib/libc.c` is hosted; one without is
    /// freestanding.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SysrootError> {
        let root = root.as_ref().to_path_buf();
        for directory in Directory::ALL {
            let path = root.join(directory.as_str());
            if !path.is_dir() {
                return Err(SysrootError::MissingDirectory { directory, root });
            }
        }
        let flavour = if root
            .join(Directory::Lib.as_str())
            .join(C_LIBRARY_FILE)
            .is_file()
        {
            SysrootFlavour::Hosted
        } else {
            SysrootFlavour::Freestanding
        };
        let sysroot = Self { root, flavour };
        // A startup object is what makes this a sysroot rather than a layout.
        if sysroot.crt().read_dir().is_err() {
            return Err(SysrootError::MissingStartup {
                path: sysroot.crt().to_path_buf(),
            });
        }
        Ok(sysroot)
    }

    /// Opens a sysroot and checks it is the kind the caller needs.
    ///
    /// The check exists because "the build needs a C library" and "the sysroot has
    /// one" are different statements, and a build that assumed they agreed would find
    /// out at link time. Saying so here names both sides.
    pub fn open_as(root: impl AsRef<Path>, wanted: SysrootFlavour) -> Result<Self, SysrootError> {
        let sysroot = Self::open(root)?;
        if sysroot.flavour != wanted {
            return Err(SysrootError::WrongFlavour {
                found: sysroot.flavour,
                wanted,
                root: sysroot.root,
            });
        }
        Ok(sysroot)
    }

    /// This sysroot's root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Which kind of program this sysroot is for.
    pub const fn flavour(&self) -> SysrootFlavour {
        self.flavour
    }

    /// The OS headers, for C.
    pub fn include(&self) -> PathBuf {
        self.root.join(Directory::Include.as_str())
    }

    /// The target libraries.
    pub fn lib(&self) -> PathBuf {
        self.root.join(Directory::Lib.as_str())
    }

    /// The startup objects.
    pub fn crt(&self) -> PathBuf {
        self.root.join(Directory::Crt.as_str())
    }

    /// The language runtime.
    pub fn runtime(&self) -> PathBuf {
        self.root.join(Directory::Runtime.as_str())
    }

    /// The startup object for `architecture`.
    /// The startup object for a machine and an entry language.
    pub fn startup_object(
        &self,
        architecture: ArchitectureConfig,
        language: EntryLanguage,
    ) -> Result<ObjectFile, SysrootError> {
        let path = self.crt().join(startup_name(architecture, language));
        let bytes = fs::read(&path).map_err(|error| SysrootError::Io {
            path: path.clone(),
            message: error.to_string(),
        })?;
        lazalith_toolchain::ObjectFile::from_bytes(&bytes).map_err(|error| SysrootError::Startup {
            message: error.to_string(),
        })
    }

    /// The Lazen runtime's text, composed in front of a program.
    ///
    /// Refused for a freestanding sysroot, because that is the distinction: a kernel
    /// that got the hosted standard library linked into it would have `rt::sys::print`
    /// and a hosted allocator, and neither belongs in a kernel.
    pub fn lazen_runtime(&self) -> Result<String, SysrootError> {
        if !self.flavour.has_runtime() {
            return Err(SysrootError::NotInFlavour {
                what: "hosted runtime",
                flavour: self.flavour,
            });
        }
        let path = self.runtime().join(RUNTIME_FILE);
        fs::read_to_string(&path).map_err(|error| SysrootError::Io {
            path,
            message: error.to_string(),
        })
    }

    /// The C library's text, composed in front of a C program.
    pub fn c_library(&self) -> Result<String, SysrootError> {
        if !self.flavour.has_c_library() {
            return Err(SysrootError::NotInFlavour {
                what: "C library",
                flavour: self.flavour,
            });
        }
        let path = self.lib().join(C_LIBRARY_FILE);
        fs::read_to_string(&path).map_err(|error| SysrootError::Io {
            path,
            message: error.to_string(),
        })
    }

    /// Every header this sysroot has, as `(name, text)`.
    ///
    /// **The freestanding ones are in both flavours, and that is not a mistake.** C says
    /// `stddef.h`, `stdint.h` and `stdbool.h` are *freestanding headers*: an implementation
    /// that cannot compile without them has not implemented freestanding, whatever else it
    /// has. A hosted program includes them too, and including them from a hosted sysroot
    /// costs three small files and saves a build that has two definitions of `size_t` in
    /// it — one from a header and one from the C library.
    pub fn headers(&self, architecture: ArchitectureConfig) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = [
            ("lazos/abi.h", abi_header()),
            ("lazos/syscall.h", syscall_header()),
        ]
        .into_iter()
        .map(|(name, text)| (String::from(name), text))
        .collect();
        for (name, text) in freestanding::freestanding_headers(u32::from(architecture.word_bits()))
        {
            headers.push((String::from(name), text));
        }
        headers
    }

    /// The freestanding library's text, composed in front of a kernel.
    ///
    /// **Refused in a hosted sysroot, the same way `c_library` is.** A hosted build gets
    /// the C library, and a hosted build that also got `libk.c` would have two
    /// `memcpy` definitions and a link error that says nothing about which one is wrong.
    pub fn kernel_library(&self) -> Result<String, SysrootError> {
        if self.flavour.has_c_library() {
            return Err(SysrootError::NotInFlavour {
                what: "kernel library",
                flavour: self.flavour,
            });
        }
        Ok(String::from(freestanding::KERNEL_LIBRARY))
    }

    fn write_kernel_library(&self) -> Result<(), SysrootError> {
        // The freestanding library, written into `lib/` and named so that it is *not*
        // `libc.c`. The name is load-bearing in one place: `open` infers the flavour from
        // whether `lib/libc.c` exists, so a freestanding library that claimed the hosted
        // name would make every freestanding sysroot look hosted the next time it is
        // opened.
        let path = self.lib().join(KERNEL_LIBRARY_FILE);
        fs::write(&path, freestanding::KERNEL_LIBRARY).map_err(|error| SysrootError::Io {
            path,
            message: error.to_string(),
        })
    }

    fn write_headers(&self, architecture: ArchitectureConfig) -> Result<(), SysrootError> {
        let include = self.include();
        let laz_os = include.join("lazos");
        fs::create_dir_all(&laz_os).map_err(|error| SysrootError::Io {
            path: laz_os.clone(),
            message: error.to_string(),
        })?;
        for (name, text) in self.headers(architecture) {
            // `lazos/abi.h` lives in a subdirectory and the rest do not, so the name is
            // taken apart rather than joined onto a fixed path. A header at the root of
            // `include/` and one a directory down are both ordinary C header layouts, and a
            // sysroot that could only write one of them would be a layout rather than a
            // sysroot.
            let path = match name.split_once('/') {
                Some((directory, file)) => include.join(directory).join(file),
                None => include.join(&name),
            };
            fs::write(&path, text).map_err(|error| SysrootError::Io {
                path,
                message: error.to_string(),
            })?;
        }
        Ok(())
    }

    fn write_startup(&self, architecture: ArchitectureConfig) -> Result<(), SysrootError> {
        // The startup objects, assembled here once and kept on disk. Before B15 the
        // sequence was assembled inside every link, which meant it existed in two
        // places at once — the generator, and whatever object a link happened to be
        // holding — and only ever for one language's entry.
        for language in EntryLanguage::ALL {
            let object = lazalith_runtime::startup_object_for(architecture, &language.symbol())
                .map_err(|error| SysrootError::Startup {
                    message: error.to_string(),
                })?;
            let bytes = object.to_bytes().map_err(|error| SysrootError::Startup {
                message: error.to_string(),
            })?;
            let path = self.crt().join(startup_name(architecture, language));
            fs::write(&path, bytes).map_err(|error| SysrootError::Io {
                path,
                message: error.to_string(),
            })?;
        }
        Ok(())
    }

    fn write_c_library(&self) -> Result<(), SysrootError> {
        // The C library is stored as **source**, not as an object. §19 asks for a
        // separation, and source is the separation that can be extended: an object
        // would be a build artefact of one compiler version, and a sysroot that
        // silently stopped working when the compiler changed would be worse than one
        // that never claimed to be prebuilt.
        let path = self.lib().join(C_LIBRARY_FILE);
        fs::write(&path, lazalith_c_runtime::C_RUNTIME).map_err(|error| SysrootError::Io {
            path,
            message: error.to_string(),
        })
    }

    fn write_runtime(&self) -> Result<(), SysrootError> {
        let path = self.runtime().join(RUNTIME_FILE);
        fs::write(&path, lazalith_runtime::library_text()).map_err(|error| SysrootError::Io {
            path,
            message: error.to_string(),
        })
    }
}

/// The Lazen runtime's file name inside `runtime/`.
pub const RUNTIME_FILE: &str = "lazen-runtime.lz";

/// The C library.s file name inside `lib/`.
///
/// **The name `open` infers the flavour from**, which is why the freestanding library
/// below is called something else: a freestanding library that claimed this name would make
/// every freestanding sysroot look hosted the moment it was opened again.
pub const C_LIBRARY_FILE: &str = "libc.c";

/// The freestanding library.s file name inside `lib/`.
pub const KERNEL_LIBRARY_FILE: &str = "libk.c";

/// The header that says what the ABI is, and that it is generated.
pub fn abi_header() -> String {
    let mut header = String::new();
    header.push_str(
        "/* Lazalith OS ABI.\n\
         *\n\
         * GENERATED from `lazalith_os_abi::ABI_SYSCALLS` by `lazalith-sysroot`.\n\
         * Do not edit: a hand-written copy of this file is a second place for the\n\
         * ABI to live, and the second place is the one that will be wrong.\n\
         */\n\n",
    );
    header.push_str("#ifndef LAZOS_ABI_H\n#define LAZOS_ABI_H\n\n");
    header.push_str("#define LAZOS_ABI 1\n\n");
    header.push_str("/* A file handle from `open`. */\ntypedef unsigned int laz_file_handle;\n");
    header.push_str("/* A process handle from `spawn_process`. */\ntypedef unsigned int laz_process_handle;\n\n");
    header.push_str("/* The sizes the ABI writes records in. */\n");
    header.push_str(&format!(
        "#define LAZOS_IO_RESULT_SIZE {}\n",
        lazalith_os_abi::IO_RESULT_SIZE
    ));
    header.push_str(&format!(
        "#define LAZOS_FILE_STAT_SIZE {}\n",
        lazalith_os_abi::FILE_STAT_SIZE
    ));
    header.push_str(&format!(
        "#define LAZOS_DIRECTORY_RECORD_SIZE {}\n",
        lazalith_os_abi::DIRECTORY_RECORD_SIZE
    ));
    header.push_str(&format!(
        "#define LAZOS_EXIT_STATUS_RECORD_SIZE {}\n",
        lazalith_os_abi::EXIT_STATUS_RECORD_SIZE
    ));
    header.push_str("\n/* The `open` flags. */\n");
    for (name, value) in [
        ("OPEN_READ", lazalith_os_abi::OPEN_READ),
        ("OPEN_WRITE", lazalith_os_abi::OPEN_WRITE),
        ("OPEN_CREATE", lazalith_os_abi::OPEN_CREATE),
        ("OPEN_TRUNCATE", lazalith_os_abi::OPEN_TRUNCATE),
        ("OPEN_ALL", lazalith_os_abi::OPEN_ALL),
    ] {
        header.push_str(&format!("#define LAZOS_{name} 0x{value:08x}u\n"));
    }
    header.push_str("\n#endif\n");
    header
}

/// The header that declares the syscalls, from the ABI's own table.
pub fn syscall_header() -> String {
    let mut header = String::new();
    header.push_str(
        "/* The Lazalith syscalls.\n\
         *\n\
         * GENERATED from `lazalith_os_abi::ABI_SYSCALLS` and the C front end's\n\
         * `abi_signature`, by `lazalith-sysroot`. Do not edit.\n\
         *\n\
         * A C program calls these through the ABI, so a declaration here that the ABI\n\
         * disagrees with would be a call that reads the wrong register. That is the\n\
         * reason this file is generated: the signature comes from the same table the\n\
         * compiler checks calls against.\n\
         */\n\n",
    );
    header.push_str("#ifndef LAZOS_SYSCALL_H\n#define LAZOS_SYSCALL_H\n\n");
    header.push_str("#include \"lazos/abi.h\"\n\n");
    for (name, syscall) in ABI_SYSCALLS {
        let signature = abi_signature(name);
        let text = match &signature {
            Some(signature) => declare(name, signature),
            None => {
                header.push_str(&format!(
                    "/* `{name}` takes {words} words and this front end has no C type for it. */\n",
                    words = syscall.argument_count()
                ));
                continue;
            }
        };
        header.push_str(&text);
        header.push('\n');
    }
    header.push_str("#endif\n");
    header
}

/// One C declaration for one syscall, in the ABI's own shape.
///
/// **Written as a declarator rather than as a type name**, because `CType::name()`
/// gives a *type* — `int *`, `unsigned long *` — and a declaration needs the name
/// *inside* that. `long *` with the name appended is `long *write`, which is a
/// multiplication, not a declaration. The pointer case is the only one that needs
/// care, and it is the case every syscall here has.
fn declare(name: &str, signature: &CType) -> String {
    let CType::Function(function) = signature else {
        return String::new();
    };
    let parameters = if function.params.is_empty() {
        String::from("void")
    } else {
        // Each parameter's name is handed to the declarator rather than prepended to
        // it: for a pointer, the name goes *inside* the type (`int *p0`), so a name
        // spliced on the front gives `p0int *` — which is not C at all.
        function
            .params
            .iter()
            .enumerate()
            .map(|(index, parameter)| declarator(&format!("p{index}"), parameter))
            .collect::<Vec<String>>()
            .join(", ")
    };
    format!("{} {name}({parameters})", declarator("", &function.result))
}

/// `declarator` written around C's type name: the name goes where C puts it.
fn declarator(name: &str, ty: &CType) -> String {
    match ty {
        CType::Pointer(inner) => {
            // `*` binds to the name, so `int *p` rather than `int* p`, and a
            // `* *` chain keeps its spaces for the same reason.
            declarator(&format!("*{name}"), inner)
        }
        other => {
            let spelling = other.name();
            if name.is_empty() {
                spelling
            } else {
                format!("{spelling} {name}")
            }
        }
    }
}
