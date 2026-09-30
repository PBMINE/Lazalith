//! Making translated code executable, and calling it.
//!
//! This file and the `execute` function in the crate root are **the whole of this
//! crate's `unsafe`**, and they are kept together for that reason: the bytes written
//! here are the bytes called there, and the invariant between them — that the page is
//! writable exactly while it is being filled and executable exactly when it is called —
//! can be read in one place.
//!
//! # The sequence, and why it is that sequence
//!
//! 1. `mmap` with `PROT_READ | PROT_WRITE`. **Not** `PROT_EXEC`.
//! 2. Copy the emitted bytes in.
//! 3. `mprotect` to `PROT_READ | PROT_EXEC`.
//! 4. Call the page's start address.
//!
//! **Step 1 deliberately leaves the page non-executable while it is writable.** Writable
//! and executable at the same time is the W^X hole: anything that can write through a
//! pointer in this process can then run what it wrote. Lazalith's guest memory is
//! reachable from a fault report and from a debugger, so a guest that could reach a
//! writable-executable page would have a path to host code execution. The window between
//! the two `mprotect` calls is a few microseconds and is the price of not having that
//! hole at all.
//!
//! # What is not done
//!
//! **The page is never unmapped while the engine lives**, and a `Jit` that is dropped
//! leaks it. That is a deliberate trade for this stage: an `mprotect` back to
//! `PROT_NONE` in `Drop` would be four lines, and the reason it is not here is that the
//! interesting case is the *long-lived* one — a VM that switches engines thousands of
//! times must not leak a page per switch, and a leak-free design has to reuse a page
//! across blocks rather than make one per block. This crate makes exactly one page for
//! its whole life, so the leak is bounded by the number of engines, not by the number of
//! blocks. It is recorded in `docs/project-state.md` as a known limitation.

use core::ptr;

use crate::Decline;

/// The size of one executable page.
///
/// **A page, not a block.** A JIT that made a page per block would `mmap` on every
/// translation, which is a syscall per block and would dominate everything it saved; and
/// a page per *engine* means a VM that switches engines a thousand times still has one.
const PAGE: usize = 4096;

/// A page of host memory holding translated code, made executable on demand.
#[derive(Debug)]
pub struct ExecutablePage {
    base: *mut u8,
    filled: usize,
    writable: bool,
}

// SAFETY: the page is raw memory this crate allocated and exclusively owns. It is only
// touched through `&mut self` (so the pointers are not aliased) and never handed out
// except as an `entry` that the JIT immediately calls.
unsafe impl Send for ExecutablePage {}

impl ExecutablePage {
    /// Allocates a writable, non-executable page.
    pub fn new() -> Result<Self, Decline> {
        // SAFETY: `mmap` with these arguments is a plain allocation — no file, no
        // `MAP_FIXED`, no replacement of an existing mapping — and every failure mode
        // returns `MAP_FAILED`.
        let base = unsafe { libc_mmap(PAGE, PROT_READ | PROT_WRITE) };
        if base == MAP_FAILED {
            return Err(Decline::NoExecutablePage);
        }
        Ok(Self {
            base,
            filled: 0,
            writable: true,
        })
    }

    /// Copies `bytes` into the page, growing it if the page is not big enough.
    ///
    /// **A page that has been made executable is made writable again first**, and made
    /// non-executable immediately afterwards. That is the only place the W+X state
    /// exists and it is bracketed by two `mprotect` calls on the same line of reasoning
    /// as above.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), Decline> {
        if bytes.len() > PAGE {
            return Err(Decline::NoExecutablePage);
        }
        if !self.writable {
            self.set_protection(PROT_READ | PROT_WRITE)?;
            self.writable = true;
        }
        // SAFETY: `bytes` is a caller-owned slice and the copy is bounded by its length;
        // the destination is this page and `bytes.len() <= PAGE` was just checked.
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), self.base, bytes.len());
        }
        self.filled = self.filled.max(bytes.len());
        self.set_protection(PROT_READ | PROT_EXEC)?;
        self.writable = false;
        Ok(())
    }

    /// The page's start address, as a function pointer.
    ///
    /// **The address is *converted* to a function pointer; it is not read from the
    /// page.** Those are different operations and the first version of this method
    /// conflated them: `ptr::read(base as *const Entry)` reads eight bytes *at* the page
    /// and reinterprets them as a pointer, which here are the first eight bytes of the
    /// emitted code — `48 b9 14 00 00 00 00 00`, a `MOV RCX, 20` — so the JIT called
    /// `0x0000_0000_0014_b948` and died. The address and the thing at the address came
    /// out transposed, and the resulting jump target looked like a plausible heap pointer
    /// rather than an obviously wrong number, which is why it read as a mystery segfault
    /// rather than a cast error.
    pub fn entry(&self) -> Entry {
        debug_assert!(
            !self.writable,
            "the page is writable, so calling it would be calling whatever is in it"
        );
        // SAFETY: the address is the start of a page this crate mapped, filled with code
        // it emitted, and `write` ends by making that page `PROT_READ | PROT_EXEC`, so
        // the memory there is executable and the transmute is to the signature the
        // translator generates for. The result is used before `Drop` can unmap.
        unsafe { core::mem::transmute(self.base) }
    }

    fn set_protection(&self, protection: i32) -> Result<(), Decline> {
        // SAFETY: `mprotect` on a page this crate allocated, with a length equal to
        // the page, and a protection no more permissive than the kernel allows. The
        // address and length are unchanged since `mmap`.
        let result = unsafe { libc_mprotect(self.base, PAGE, protection) };
        if result != 0 {
            return Err(Decline::NoExecutablePage);
        }
        Ok(())
    }
}

impl Drop for ExecutablePage {
    fn drop(&mut self) {
        // SAFETY: `munmap` on the page this crate allocated, exactly once, with the
        // length it was allocated with. The page may be executable here, which is fine:
        // unmapping removes the mapping, and the W^X argument is about *when host code
        // can be written through a pointer*, not about the final teardown.
        unsafe {
            libc_munmap(self.base, PAGE);
        }
    }
}

/// The signature translated code is generated for.
///
/// **Three pointers in, nothing back, and the pointers are the ones the C ABI already
/// puts in registers.** On the System V x86-64 ABI the first three integer arguments
/// arrive in `RDI`, `RSI` and `RDX`, so this signature needs no prologue at all: the
/// register block, the guest PC and the flags scratch are already exactly where the
/// generated code wants them, and the block's own scratch is `R10` and `R11`.
///
/// That matters more than it looks. The first draft of this crate generated code that
/// began by loading the guest PC into `RDI` on the assumption that `RDI` was a scratch
/// register — which silently destroyed the register block pointer the caller had just
/// passed, so the block read its inputs from whatever address the PC happened to be and
/// wrote its results back over the machine's own code. It produced *no* fault: the page was
/// writable, the address was mapped, and the answer was simply wrong. The fix is not a
/// workaround but an agreement — the signature, the emitter's register assignment and the
/// caller's argument order are one decision recorded in three places, which is why they
/// are written next to each other.
///
/// It is `extern "C"` because the emitter's last instruction is a `RET`, and a `RET` pops
/// whatever the C caller pushed.
pub type Entry = extern "C" fn(*mut u64, *mut u64, *mut u64);

const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const PROT_EXEC: i32 = 4;

const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;

/// What `mmap` returns when it fails: `(void *) -1`.
///
/// **Not null, and the difference is a segfault.** An earlier version of this file
/// tested `base.is_null()`, which is the check almost every pointer gets by habit and
/// which `mmap` never satisfies. A failed mapping therefore came back as
/// `0xFFFF_FFFF_FFFF_FFFF`, passed the check, and was then handed to
/// `ptr::copy_nonoverlapping` as a destination — writing the translated code to the
/// highest address in the address space and killing the process. The kernel returned
/// `-ENOMEM`; the program died somewhere else entirely, with no hint that the JIT was
/// involved.
const MAP_FAILED: *mut u8 = usize::MAX as *mut u8;

// `mmap`, declared rather than depended on.
//
// Declared here, in the one crate allowed to contain `unsafe`, rather than pulled in as a
// dependency. A `libc` dependency for three syscalls would put an external crate
// between this one and the operating system for no benefit, and the declarations are a
// dozen lines whose exactness a reader can check against the x86-64 Linux ABI.
//
// **`mmap` takes six arguments and all six are declared here.** The first version of this
// block declared five, omitting the leading `addr`, and because the C ABI passes
// arguments in registers and a Rust declaration the compiler trusts, nothing complained:
// the call pushed `length` into the `addr` slot, `protection` into `length`, and a
// stack-slot garbage value into `offset`. `mmap` returned `EINVAL` — offset not a
// multiple of the page size — and the symptom was a JIT that declined every block
// because its page could not be allocated, with no error anywhere pointing at the
// declaration. A wrong arity in a hand-written FFI signature is silent by construction.
unsafe extern "C" {
    #[link_name = "mmap"]
    fn c_mmap(
        address: *mut u8,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut u8;
    #[link_name = "mprotect"]
    fn c_mprotect(address: *mut u8, length: usize, protection: i32) -> i32;
    #[link_name = "munmap"]
    fn c_munmap(address: *mut u8, length: usize) -> i32;
}

/// `mmap` of anonymous private memory, at whatever address the kernel chooses.
///
/// # Safety
///
/// The returned pointer must be unmapped with [`libc_munmap`] and the same length.
unsafe fn libc_mmap(length: usize, protection: i32) -> *mut u8 {
    // SAFETY: a null `address` with no `MAP_FIXED` asks the kernel to choose, which is
    // what an anonymous mapping wants; an anonymous private mapping has no file
    // descriptor and no offset meaning, and `fd` of -1 with `MAP_ANONYMOUS` is the
    // documented requirement.
    unsafe {
        c_mmap(
            ptr::null_mut(),
            length,
            protection,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    }
}

/// `mprotect` on a mapping this crate owns.
///
/// # Safety
///
/// `address` and `length` must describe a mapping this crate made and has not unmapped.
unsafe fn libc_mprotect(address: *mut u8, length: usize, protection: i32) -> i32 {
    // SAFETY: forwarded with the caller's precondition.
    unsafe { c_mprotect(address, length, protection) }
}

/// `munmap` of a mapping this crate owns.
///
/// # Safety
///
/// `address` and `length` must describe a mapping this crate made and has not unmapped.
unsafe fn libc_munmap(address: *mut u8, length: usize) -> i32 {
    // SAFETY: forwarded with the caller's precondition.
    unsafe { c_munmap(address, length) }
}
