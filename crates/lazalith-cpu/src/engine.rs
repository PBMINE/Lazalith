//! The execution-engine boundary.
//!
//! # What an engine is
//!
//! An engine turns a [`Processor`] and memory into a *different* [`Processor`] and
//! the same memory, one guest instruction at a time. That is the whole interface.
//! Everything else a VM needs — the bus, the devices, the clock, the machine
//! lifecycle — stays where it was, and an engine never sees any of it.
//!
//! ```text
//! Interpreter ─┐
//!              ├──▶ same Processor, same memory, different instructions run
//! JIT ─────────┘
//! ```
//!
//! # The rule that makes switching safe
//!
//! The [`Processor`] is owned by the machine, not by an engine. An engine borrows
//! it for one step. So switching engines cannot clone the architectural state, and
//! therefore cannot fork it: there is nothing to keep in sync because there is only
//! ever one copy. A JIT's translation cache is its own and is discarded on a
//! switch; the program counter it was about to resume at is the machine's, and it
//! is the same program counter either engine reads.
//!
//! This is the difference between an execution engine and a second virtual
//! machine, and it is the whole reason this trait takes a `&mut Processor` rather
//! than owning one.
//!
//! # The Reference Interpreter is the oracle
//!
//! [`ReferenceInterpreter`] is the statement of what the ISA means. It stays that
//! statement whatever else is added: an engine that disagrees with it is wrong, and
//! the disagreement is a defect in the new engine, never a reason to change the
//! reference. Differential testing against it is how that is enforced, not
//! agreement by construction.

use crate::{CpuFault, CpuMemory, OutcomeApplication, Processor};
use core::error::Error;
use core::fmt;
use lazalith_isa::Instruction;

/// Which execution engine a machine is running on.
///
/// This is a closed set on purpose. A machine switches between engines *by name*,
/// through [`EngineKind`], and the machine decides what a name constructs. That is
/// what lets the switch be a single checked operation in the VM core rather than a
/// caller reaching in and swapping an implementation, and it is what makes
/// "which engine is this" a fact a debugger can print rather than a type the
/// compiler has already erased.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum EngineKind {
    /// The reference interpreter. The semantic authority, and always present.
    Reference,
}

impl EngineKind {
    /// A short stable name, for diagnostics and for the switch API.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
        }
    }

    /// Every engine this build has, in the order the machine reports them.
    pub const ALL: &'static [EngineKind] = &[Self::Reference];
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What went wrong in the execution-engine boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    /// The machine was asked to run on an engine this build does not have.
    UnknownEngine(EngineKind),
    /// The machine cannot change engines right now.
    ///
    /// The reason is named rather than implied, because the two refusals mean
    /// different things to whoever hit them: a terminal machine is over, and an
    /// active execution context means the scheduler is between two steps and would
    /// not notice the change.
    SwitchRefused { reason: &'static str },
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEngine(kind) => write!(f, "there is no {kind} execution engine"),
            Self::SwitchRefused { reason } => {
                write!(f, "the execution engine cannot be changed: {reason}")
            }
        }
    }
}

impl Error for EngineError {}

/// Something that executes LZA instructions.
///
/// # The contract
///
/// - `step` executes **exactly one** guest instruction, or reports why it could
///   not. It does not loop, and it does not stop early without a reason.
/// - `step` leaves the [`Processor`] either advanced by one instruction or
///   **unchanged**. A step that faults half way through must not have committed
///   half of an instruction's effects. This is why the reference engine builds a
///   candidate state and commits it, and why every other engine must do the same.
/// - `step` is the *only* place an engine may change architectural state. Reading
///   is free; writing happens through the processor or not at all.
/// - The engine may keep whatever private state it likes, and may drop it in
///   [`ExecutionEngine::discard_private_state`] — which the machine calls on a
///   switch. A guest must not be able to tell the difference.
///
/// `Debug` is a supertrait because a machine is `Debug` and holds one: a bug report
/// about a machine that cannot be printed is a bug report that has to be assembled
/// by hand from the pieces around the interesting part.
pub trait ExecutionEngine<M: CpuMemory>: fmt::Debug {
    /// Which engine this is.
    fn kind(&self) -> EngineKind;

    /// Executes one guest instruction.
    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>>;

    /// Executes one instruction whose bytes the caller already has.
    ///
    /// Used by tests and by a caller that fetched the bytes itself — an engine
    /// that decodes as it fetches will not need it, and one that does will
    /// override it.
    fn step_bytes(
        &mut self,
        processor: &mut Processor,
        bytes: &[u8],
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>> {
        let instruction = lazalith_isa::decode(processor.config(), bytes).map_err(|source| {
            CpuFault::at(
                processor.architectural().pc(),
                bytes.first().copied(),
                crate::CpuFaultCause::Decode(source),
            )
        })?;
        self.execute(processor, &instruction, memory)
    }

    /// Executes an instruction that has already been decoded.
    fn execute(
        &mut self,
        processor: &mut Processor,
        instruction: &Instruction,
        memory: &mut M,
    ) -> Result<OutcomeApplication, CpuFault<M::Error>>;

    /// Drops everything this engine holds that is not architectural.
    ///
    /// Called on a switch and on a reset. An engine whose private state affects
    /// what the guest can observe must not have that state; an engine that does not
    /// can leave this empty.
    fn discard_private_state(&mut self) {}
}
