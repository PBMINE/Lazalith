//! The LazOS memory service.
//!
//! # What this owns
//!
//! One syscall: `allocate_memory`. It is the only facility that hands a program
//! memory it did not already have, and it is separate from the terminal, the
//! filesystem, the display driver and the input driver because it is a
//! different kind of thing. Those four answer questions about a world that
//! already exists — a console, a directory, a window, a keypress. This one
//! *makes* something, and what it makes is the program's own.
//!
//! # Why it is a bump pool
//!
//! Because the ABI says so, and because the alternative is not honest at this
//! size. `allocate_memory` takes a length and an alignment and reports an
//! address; it has no `free` argument and no `realloc` argument, so a program
//! cannot describe a region it no longer wants. A heap that cannot be given
//! memory back is a bump pool, and calling it anything else would promise a
//! guarantee the interface cannot make. It is the process's own pool, reset when
//! the process is, so a long-lived kernel does not accumulate every block every
//! process ever asked for.
//!
//! What that costs is real and is stated here rather than discovered: memory a
//! program `free`d is not recovered, and a program that allocates in a loop
//! eventually gets `ResourceExhausted`. The C runtime's `free` is a no-op for
//! exactly this reason, and a C program that allocates per iteration should
//! reuse its buffer instead — which is what C programmers are told to do anyway.
//!
//! # What the program is told
//!
//! A v1 call cannot return a length *and* an address, so the record is an
//! argument: a sixteen-byte `MemoryAllocation` at an address the program chose.
//! The service writes whole records and never a prefix of one, so a program
//! either has its allocation or has an error — there is no state in which it
//! has half a record and does not know which half.

use lazalith_os_abi::{MemoryAllocation, SyscallError, TaggedOutcome};
use lazalith_types::VirtualAddress;

use crate::syscall::{
    KernelService, ServiceOutcome, UserMemoryContext, ValidatedSyscall, ValidatedSyscallKind,
};

/// The LazOS memory service.
#[derive(Debug, Default)]
pub struct MemoryService;

impl MemoryService {
    /// A service with nothing to configure.
    pub const fn new() -> Self {
        Self
    }

    /// Serves one validated `allocate_memory`.
    ///
    /// The length and alignment have already been checked against the ABI by the
    /// time this is called — a zero length, a non-power-of-two alignment, or a
    /// destination too small for a record never reaches a service at all. So the
    /// only failure left is a pool with nothing left in it, and that is
    /// `ResourceExhausted` rather than `Internal`: the call was well formed and
    /// the machine simply has no more room, which is the one answer a program
    /// should be able to act on.
    fn invoke_allocate(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        length: u64,
        alignment: u64,
        result: VirtualAddress,
    ) -> ServiceOutcome {
        let block = match memory.allocate(length, alignment) {
            Ok(block) => block,
            Err(_) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::ResourceExhausted,
                    0,
                ));
            }
        };
        let Ok(allocation) =
            MemoryAllocation::new(memory.config(), block.address().as_u64(), block.length())
        else {
            return ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::Internal, 0));
        };
        match memory.write_bytes(result, &allocation.encode()) {
            Ok(()) => ServiceOutcome::Return(TaggedOutcome::success(0)),
            Err(_) => {
                ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::InvalidPointer, 0))
            }
        }
    }
}

impl KernelService for MemoryService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::AllocateMemory {
                length,
                alignment,
                result,
            } => self.invoke_allocate(memory, length, alignment, result),
            _ => ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::NotSupported, 0)),
        }
    }
}
