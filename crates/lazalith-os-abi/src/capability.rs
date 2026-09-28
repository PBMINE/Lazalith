//! What a process is allowed to ask the system to do.
//!
//! # The gap this closes
//!
//! `docs/lazen-applications.md` says a manifest's `[permissions]` declares "the OS
//! capabilities the application intends to use", and `docs/lazen-packages.md` records
//! that nothing enforced them: the package carried a declaration and the syscall
//! layer had no capability gate to enforce it with. This is that gate.
//!
//! # The rule
//!
//! A process started from a **package** may only make the syscalls its package
//! declared. A process started from a **bare `.lzx`** may make all of them, because a
//! bare executable has declared nothing to enforce.
//!
//! That asymmetry is the whole design and it is deliberate:
//!
//! - A package is the *untrusted* unit. It arrives from somewhere, its author wrote
//!   down what it needs, and the system can check the two agree.
//! - A bare `.lzx` is the *trusted* unit. It is what `lazen build` produced, it is
//!   what the boot ROM loads, and it is what a person runs on their own machine. A
//!   gate on it would break the kernel itself, the init shell, and every test, in
//!   exchange for protecting a program that already has the whole machine.
//!
//! So the gate is a property of *how a process was started*, not of what it contains,
//! and the ABI says so in one place: [`Capabilities::for_start`].
//!
//! # Why a capability rather than a device check
//!
//! Checking the device a syscall would touch is the wrong level. `Write` is a console
//! capability whether the handle is a terminal or a file, and a program with
//! `console = true, filesystem = false` that writes to a file is exactly the thing the
//! declaration is supposed to catch. So the check is on the *syscall*, and each
//! syscall names the one capability it needs. A syscall that needs none — `Exit`,
//! `Time`, `Sleep` — is always allowed, because a program that cannot ask the time or
//! stop is not a program with fewer privileges, it is a broken one.

use alloc::string::String;

use crate::{Syscall, SyscallStatus};

/// The capability for writing to a terminal.
///
/// The bits live *here* rather than in the package format because they are a contract
/// between a program and the system that runs it, and the contract is the ABI.s job.
/// `lazalith_os::lza` re-exports these, so a package and a syscall are talking about
/// the same four bits rather than about two sets that happen to agree.
pub const PERMISSION_CONSOLE: u8 = 1;
/// The capability for the filesystem: opening, reading, listing, and a process.s own
/// heap. The process capability is folded in here because a process that cannot open
/// a file has no use for a heap it cannot address, and a capability a program has to
/// declare twice is one it will forget.
pub const PERMISSION_FILESYSTEM: u8 = 2;
/// The capability for the display.
pub const PERMISSION_GRAPHICS: u8 = 4;
/// The capability for the keyboard and pointer.
pub const PERMISSION_INPUT: u8 = 8;

/// The capabilities a package declared, as this build reads them.
///
/// A `u8` rather than four booleans so a reader can *hold* a declaration bit it does
/// not know rather than refusing it — the same rule the input device's event records
/// follow, and for the same reason: a program built by a newer toolchain must still
/// be installable on an older kernel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PackagePermissions {
    /// The bits, as the package declared them.
    pub bits: u8,
}

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

    /// This set, as a process's capabilities.
    pub const fn as_capabilities(self) -> Capabilities {
        Capabilities { bits: self.bits }
    }
}

/// What a process may do.
///
/// A bit set rather than four booleans, and deliberately: a bit a future build knows
/// about and this one does not must be *held* rather than refused, because a process
/// started by a newer toolchain has to keep working on an older kernel. This is the
/// same rule the input device's event records follow, and for the same reason.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Capabilities {
    /// The bits.
    pub bits: u8,
}

impl Capabilities {
    /// Only the capabilities both sets hold.
    ///
    /// Used when a restriction is applied to a process that has one, so the gate is an
    /// intersection with what was granted and never a widening.
    pub const fn intersect(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }
}

/// Every capability this build knows.
pub const ALL_CAPABILITIES: u8 =
    PERMISSION_CONSOLE | PERMISSION_FILESYSTEM | PERMISSION_GRAPHICS | PERMISSION_INPUT;

impl Capabilities {
    /// A process started from a package gets exactly what the package declared.
    ///
    /// The unknown bits are dropped rather than held, which is the one place this
    /// module truncates: a capability this build cannot *enforce* is a capability it
    /// must not hand out, so an older kernel refuses rather than waves through. That
    /// is the opposite of the rule for a package's *own* record fields, where an
    /// unknown bit is held — and deliberately so, because a reader that refused a
    /// newer package could not install it at all, while a kernel that grants an
    /// unknown capability would be promising something it does not implement.
    pub const fn for_start(declared: PackagePermissions) -> Self {
        Self {
            bits: declared.bits & ALL_CAPABILITIES,
        }
    }

    /// A process started from a bare executable gets everything, because a bare
    /// executable has declared nothing to enforce.
    pub const fn for_trusted_image() -> Self {
        Self {
            bits: ALL_CAPABILITIES,
        }
    }

    /// Whether a capability is held.
    pub const fn has(self, capability: u8) -> bool {
        self.bits & capability != 0
    }

    /// What a syscall needs, or `None` if it needs nothing.
    ///
    /// The whole gate in one function, so that adding a syscall and forgetting to
    /// decide what it needs is a visible omission rather than a silent permission.
    pub const fn required_by(syscall: Syscall) -> Option<u8> {
        match syscall {
            // Nothing. A program that cannot read the clock or stop is not a program
            // with fewer privileges.
            Syscall::Exit | Syscall::Time | Syscall::Sleep => None,
            // The filesystem capability, not the console one: `write` is a console
            // call when the handle is a terminal and a file call when it is not, and
            // the *filesystem* capability is the one that is about the guest's own
            // data. A program with `console = true` and no `filesystem` can therefore
            // still be given a terminal handle and a file handle cannot be opened —
            // and the terminal path is checked by the handle, not by the syscall.
            Syscall::Open
            | Syscall::Read
            | Syscall::Stat
            | Syscall::ListDirectory
            | Syscall::Seek => Some(PERMISSION_FILESYSTEM),
            Syscall::Write | Syscall::Close => Some(PERMISSION_CONSOLE),
            Syscall::AllocateMemory => Some(PERMISSION_FILESYSTEM),
            Syscall::SpawnProcess | Syscall::WaitProcess => Some(PERMISSION_FILESYSTEM),
            Syscall::ClearScreen | Syscall::DisplayOpen | Syscall::DisplayPresent => {
                Some(PERMISSION_GRAPHICS)
            }
            Syscall::InputPoll => Some(PERMISSION_INPUT),
        }
    }

    /// Whether a syscall is allowed.
    pub const fn allows(self, syscall: Syscall) -> bool {
        match Self::required_by(syscall) {
            None => true,
            Some(capability) => self.has(capability),
        }
    }
}

/// Why a syscall was refused for want of a capability.
///
/// Carries the syscall and the capability rather than a sentence, so a caller can
/// render it however it likes and a test can assert on the pair.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefusedCapability {
    /// The syscall that was refused.
    pub syscall: Syscall,
    /// The capability it needs.
    pub capability: u8,
}

impl RefusedCapability {
    /// The status a guest sees.
    pub const fn status(self) -> SyscallStatus {
        SyscallStatus::PermissionDenied
    }

    /// The capability's name, for a report.
    pub fn capability_name(self) -> String {
        String::from(match self.capability {
            PERMISSION_CONSOLE => "console",
            PERMISSION_FILESYSTEM => "filesystem",
            PERMISSION_GRAPHICS => "graphics",
            PERMISSION_INPUT => "input",
            _ => "unknown",
        })
    }
}

impl core::fmt::Display for RefusedCapability {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:?} needs the {} capability, which this process was not given",
            self.syscall,
            self.capability_name()
        )
    }
}

/// The check itself, as a function a dispatcher calls once per syscall.
pub fn check(capabilities: Capabilities, syscall: Syscall) -> Result<(), RefusedCapability> {
    let Some(capability) = Capabilities::required_by(syscall) else {
        return Ok(());
    };
    if capabilities.has(capability) {
        return Ok(());
    }
    Err(RefusedCapability {
        syscall,
        capability,
    })
}
