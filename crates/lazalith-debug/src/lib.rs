//! Step 74: the Lazalith debug API.
//!
//! # What this is
//!
//! A frontend — a terminal debugger today, something else tomorrow — drives a
//! running program through [`DebugController`] and [`DebugSession`]. The
//! controller owns the machine and the kernel; a session owns one process's
//! debugging state.
//!
//! # The rule this crate exists to enforce
//!
//! **A frontend cannot manipulate CPU internals directly.** That is not a
//! promise in a document; it is the shape of the API.
//!
//! There is no `&mut LazalithMachine` anywhere in the public surface, and no
//! method that hands one out. [`DebugController::registers`] returns an *owned*
//! [`Registers`] value, not a reference into the CPU's state, so a frontend can
//! read a register and cannot write one through it. Memory inspection returns an
//! owned `Vec<u8>`. Disassembly returns owned text.
//!
//! This is a real constraint on what a debugger can be, and it is deliberate. A
//! debugger that could hand out `&mut` to the machine could set the program
//! counter, forge a trap frame, or activate a supervisor context, and every
//! invariant the machine holds — that a user process runs at user privilege,
//! that its memory is bounds checked, that a syscall was validated before it ran
//! — would become a thing a frontend could opt out of. The kernel validates the
//! active binding on *every* step precisely so that nothing upstream can skip
//! it, and a debug API that leaked the machine would undo that from the other
//! direction.
//!
//! # Execution control
//!
//! - [`DebugController::run`] — continue until something stops it: a breakpoint,
//!   a watchpoint, a pause request, the program exiting, or a step limit.
//! - [`DebugController::step`] — one instruction.
//! - [`DebugController::pause`] — ask to stop at the next instruction.
//! - [`DebugController::continue_`] — the same as `run`, named for what a user
//!   types.
//!
//! # Why watchpoints are software
//!
//! The machine has no watchpoint register: `lazalith-cpu` has a `DebugState` with
//! a single `single_step` flag that nothing reads, which is all the debug surface
//! the ISA grew. So a watchpoint here is a *comparison*: the bytes under the
//! watched address are read before each step and compared after, and a change is
//! a hit. That is correct at instruction granularity, which is the finest
//! granularity a program can be stopped at anyway, and it costs one read and one
//! comparison per watchpoint per step. It is stated here rather than hidden
//! because a software watchpoint is slower than a hardware one, and a frontend
//! that watches a hot address should know that before it does.

extern crate alloc;

mod controller;
pub mod diagnostic;
mod registers;
mod session;
mod snapshot;

pub use controller::{
    DebugController, Disassembly, RunOutcome, StackView, StepOutcome, StopReason,
};
pub use registers::{REGISTER_COUNT, RegisterSnapshot, RegisterValue};
pub use session::{DebugSession, DebugSnapshot, ExecutionState, Watchpoint, WatchpointSize};
pub use snapshot::{CpuSnapshot, DeviceSnapshot, MachineSnapshot, ProcessSnapshot};

use core::fmt;

/// Anything that can go wrong driving a program under the debug API.
#[derive(Debug)]
pub enum DebugError {
    /// The machine could not be booted.
    Boot(Box<lazalith_boot::BootError>),
    /// A machine operation was refused.
    Machine(Box<lazalith_machine::MachineError>),
    /// The kernel refused the request.
    Kernel(Box<lazalith_os::KernelError>),
    /// The image could not be loaded.
    Image(Box<lazalith_os::LzxError>),
    /// An address was outside the range the architecture can name.
    Address(lazalith_types::WidthError),
    /// A read or a write fell outside mapped memory, or the privilege was wrong.
    Memory(Box<lazalith_memory::MemoryFault>),
    /// The bytes at an address are not an instruction.
    Disassembly(Box<lazalith_toolchain::DisassemblyError>),
    /// A breakpoint address is not instruction-aligned.
    UnalignedBreakpoint {
        /// The address that was asked for.
        address: u64,
        /// The instruction size the address has to be a multiple of.
        word_size: u64,
    },
    /// An address asked for a whole instruction was not instruction-aligned.
    UnalignedInstruction {
        /// The address that was asked for.
        address: u64,
        /// The instruction size the address has to be a multiple of.
        word_size: u64,
    },
    /// A watchpoint was given a size that is not one, two, four or eight bytes.
    UnsupportedWatchpointSize {
        /// The size that was asked for, in bytes.
        size: u64,
    },
    /// A snapshot was restored into a controller that is not stopped, or that
    /// has no such session.
    Snapshot(String),
}

impl fmt::Display for DebugError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boot(source) => write!(f, "the machine would not boot: {source}"),
            Self::Machine(source) => write!(f, "the machine refused: {source}"),
            Self::Kernel(source) => write!(f, "the kernel refused: {source}"),
            Self::Image(source) => write!(f, "the image would not load: {source}"),
            Self::Address(source) => write!(f, "the address is not one this target has: {source}"),
            Self::Memory(source) => write!(f, "the memory access was refused: {source}"),
            Self::Disassembly(source) => write!(f, "the bytes are not an instruction: {source}"),
            Self::UnalignedInstruction { address, word_size } => write!(
                f,
                "an instruction at {address:#x} is not a multiple of {word_size}"
            ),
            Self::UnalignedBreakpoint { address, word_size } => write!(
                f,
                "a breakpoint at {address:#x} is not a multiple of {word_size}"
            ),
            Self::UnsupportedWatchpointSize { size } => {
                write!(f, "a watchpoint of {size} bytes is not one this API offers")
            }
            Self::Snapshot(detail) => {
                write!(f, "the snapshot does not fit this controller: {detail}")
            }
        }
    }
}

impl core::error::Error for DebugError {}

impl From<lazalith_boot::BootError> for DebugError {
    fn from(source: lazalith_boot::BootError) -> Self {
        Self::Boot(Box::new(source))
    }
}

impl From<lazalith_machine::MachineError> for DebugError {
    fn from(source: lazalith_machine::MachineError) -> Self {
        Self::Machine(Box::new(source))
    }
}

impl From<lazalith_os::KernelError> for DebugError {
    fn from(source: lazalith_os::KernelError) -> Self {
        Self::Kernel(Box::new(source))
    }
}

impl From<lazalith_os::LzxError> for DebugError {
    fn from(source: lazalith_os::LzxError) -> Self {
        Self::Image(Box::new(source))
    }
}

impl From<lazalith_memory::MemoryFault> for DebugError {
    fn from(source: lazalith_memory::MemoryFault) -> Self {
        Self::Memory(Box::new(source))
    }
}

impl From<lazalith_toolchain::DisassemblyError> for DebugError {
    fn from(source: lazalith_toolchain::DisassemblyError) -> Self {
        Self::Disassembly(Box::new(source))
    }
}

impl From<lazalith_types::WidthError> for DebugError {
    fn from(source: lazalith_types::WidthError) -> Self {
        Self::Address(source)
    }
}
