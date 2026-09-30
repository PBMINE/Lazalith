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
//!
//! # A decline is not a fault, and the types are how that is enforced
//!
//! B23's central distinction is between two ways a step can fail to retire an
//! instruction, and they are *different types* rather than two arms of one enum:
//!
//! | | type | what it means | what the machine does |
//! |---|---|---|---|
//! | the guest did something wrong | [`CpuFault`] | a guest fault | enter a trap |
//! | **this engine** cannot run here | [`EngineDecline`] | an engine limitation | hand the instruction to another engine |
//!
//! **Making the decline a different type, rather than a variant inside
//! [`CpuFaultCause`], is what makes the mistake impossible.** B22 signalled a JIT
//! decline with `CpuFaultCause::JitDeclined`, and the machine — which treats every
//! `Err` from an engine as a guest fault — trapped the program. That is the worst
//! possible response: a guest was trapped, visibly, for using an instruction the JIT
//! had not learned yet, and if the trap could not be entered the machine became
//! terminally [`MachineState::Faulted`](crate) over a compiler limitation.
//!
//! With `EngineFault` in the return type, a machine that wants to trap a decline has
//! to write an arm that says so. The compiler will not let the decline reach
//! `enter_trap` by accident, which is the only thing that actually prevents it.

use crate::{CpuFault, CpuMemory, Processor, StepResult};
use core::error::Error;
use core::fmt;
use lazalith_isa::Instruction;
use lazalith_types::InstructionAddress;

/// Why an execution engine cannot run at this point.
///
/// **Not a guest error, and not an emulator bug.** Nothing the guest did was wrong and
/// no invariant of ours was violated; this engine simply cannot execute here and a
/// different engine can. That is what makes the response a handoff rather than a trap.
///
/// The `reason` strings are diagnostics, not control flow. A caller switches on the
/// *variant*; the string is what a human reads, and it is a `&'static str` rather than
/// an `enum` because the set of reasons is open — every new instruction an engine
/// declines is a new sentence, and none of them is a decision the machine should make.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineDecline {
    /// The engine has no implementation or no translation for what is at this PC.
    ///
    /// The ordinary case, and entirely expected: a JIT that handles straight-line
    /// register operations declines every memory access, every branch and every
    /// privileged instruction, forever, and the interpreter runs them instead.
    UnsupportedInstruction {
        /// What specifically this engine could not do, in one clause.
        reason: &'static str,
    },
    /// The engine cannot run at all for this host or this guest configuration.
    ///
    /// **Distinct from a declined instruction because it will not become true by
    /// running something else.** A 32-bit guest, or a host this crate emits no code
    /// for, is a property of the machine rather than of the program, so the machine's
    /// handoff has to be to an engine that *can* run it — and if the only engine is
    /// this one, the machine keeps interpreting rather than trapping.
    UnsupportedConfiguration {
        /// What about the configuration the engine could not handle.
        reason: &'static str,
    },
    /// The engine could not obtain the host resources it needs.
    ///
    /// A JIT that cannot map an executable page has a problem the guest cannot
    /// cause, cannot fix, and must not be told about. This is a host failure and it
    /// belongs with the decline rather than with the guest's faults.
    HostUnavailable {
        /// What the host would not provide.
        reason: &'static str,
    },
}

impl fmt::Display for EngineDecline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedInstruction { reason } => {
                write!(f, "this engine does not execute this instruction: {reason}")
            }
            Self::UnsupportedConfiguration { reason } => {
                write!(
                    f,
                    "this engine does not run in this configuration: {reason}"
                )
            }
            Self::HostUnavailable { reason } => {
                write!(f, "this engine cannot obtain what it needs: {reason}")
            }
        }
    }
}

/// What went wrong at the execution-engine boundary.
///
/// **A sum, not a choice, and that is the whole design.** [`Guest`] is a [`CpuFault`]
/// and means the guest did something wrong; [`Declined`] is an [`EngineDecline`] and
/// means this engine cannot run here. A machine receives one or the other and cannot
/// receive one *as* the other, because they are different variants of a type the
/// machine has to match on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineFault<E> {
    /// The guest caused a fault. The machine enters a trap.
    Guest(CpuFault<E>),
    /// This engine cannot run here. The machine hands the instruction to another engine.
    Declined(EngineDecline),
}

impl<E> EngineFault<E> {
    /// The guest's fault, if this is one.
    pub const fn guest(&self) -> Option<&CpuFault<E>> {
        match self {
            Self::Guest(fault) => Some(fault),
            Self::Declined(_) => None,
        }
    }

    /// The guest's fault, if this is one — consuming, for a caller that wants the fields.
    ///
    /// **Written out because the wrappers are the point.** A test asserting that a guest
    /// instruction faulted wants the fault, not the possibility of a decline, and
    /// `into_guest().expect("…")` says that in the place where it is asserted rather than
    /// in a helper that quietly unwraps for everyone.
    pub fn into_guest(self) -> Result<CpuFault<E>, EngineDecline> {
        match self {
            Self::Guest(fault) => Ok(fault),
            Self::Declined(decline) => Err(decline),
        }
    }

    /// Why this engine declined, if it declined.
    pub const fn decline(&self) -> Option<EngineDecline> {
        match self {
            Self::Guest(_) => None,
            Self::Declined(decline) => Some(*decline),
        }
    }

    /// Whether this is a guest fault, which is the question the trap path asks.
    pub const fn is_guest_fault(&self) -> bool {
        matches!(self, Self::Guest(_))
    }
}

impl<E: Error + 'static> fmt::Display for EngineFault<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Guest(fault) => write!(f, "{fault}"),
            Self::Declined(decline) => write!(f, "{decline}"),
        }
    }
}

impl<E: Error + 'static> Error for EngineFault<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Guest(fault) => Some(fault),
            Self::Declined(_) => None,
        }
    }
}

/// A guest fault becomes an [`EngineFault`], so an engine's existing error sites work.
///
/// **This is the only reason the conversion exists**, and it is why the interpreters
/// were not rewritten fault by fault: every internal helper still returns
/// `Result<_, CpuFault<E>>`, and the `?` in front of it converts. An engine whose
/// `step` genuinely cannot produce a decline simply never constructs the other variant.
impl<E> From<CpuFault<E>> for EngineFault<E> {
    fn from(fault: CpuFault<E>) -> Self {
        Self::Guest(fault)
    }
}

impl From<EngineDecline> for EngineFault<()> {
    fn from(decline: EngineDecline) -> Self {
        Self::Declined(decline)
    }
}

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
    /// An optimised interpreter with the same semantics, checked against the reference
    /// step for step.
    ///
    /// **Present since B21, and never the authority.** §10 makes the Reference
    /// Interpreter the semantic oracle; this is the same ISA executed with some
    /// redundant work removed, and the only thing that makes it trustworthy is that
    /// `crates/lazalith-cpu/tests/speed.rs` compares it against the reference after
    /// every instruction.
    Optimized,
    /// A just-in-time compiler: LZA instructions translated to host machine code.
    ///
    /// **Present since B22 and the first engine that is not an interpreter.** §10's
    /// diagram puts it beside the reference as another way to execute LZA code, and §11
    /// is explicit that it is an execution engine and not a compiler, an assembler,
    /// another ISA, or a language frontend. It translates a *conservative* subset —
    /// straight-line runs of register-only instructions — and everything else runs
    /// through the reference; which subset is `lazalith_jit::TRANSLATABLE`'s business
    /// and not this enum's.
    Jit,
}

impl EngineKind {
    /// A short stable name, for diagnostics and for the switch API.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Optimized => "optimized",
            Self::Jit => "jit",
        }
    }

    /// Every engine this build has, in the order the machine reports them.
    pub const ALL: &'static [EngineKind] = &[Self::Reference, Self::Optimized, Self::Jit];
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
/// - `step` executes **exactly one** guest instruction — or, for an engine that
///   translates blocks, **one block of them** — or reports why it could not. It does
///   not loop, and it does not stop early without a reason.
/// - `step` leaves the [`Processor`] either advanced by exactly the instructions it
///   reports in [`StepResult::instructions`] or **unchanged**. A step that faults
///   half way through must not have committed half of an instruction's effects, and a
///   step that *declines* must have committed nothing at all. This is why the
///   reference engine builds a candidate state and commits it, and why every other
///   engine must do the same.
/// - `step` is the *only* place an engine may change architectural state. Reading
///   is free; writing happens through the processor or not at all.
/// - The engine may keep whatever private state it likes, and may drop it in
///   [`ExecutionEngine::discard_private_state`] — which the machine calls on a
///   switch. A guest must not be able to tell the difference.
///
/// # Declining
///
/// **An engine that cannot run the instruction at the current PC returns
/// [`EngineFault::Declined`] and changes nothing.** That is not a failure of the
/// machine: the machine is expected to hand the instruction to an engine that can
/// run it, and the guest sees an instruction that executed normally.
///
/// The obligation this places on an engine is sharp, because the handoff depends on
/// it: **a decline must leave the [`Processor`] byte-for-byte as it was found.** In
/// particular the program counter must still name the instruction that was declined,
/// or the interpreter runs the wrong one. An engine that fetches, partially decodes,
/// or allocates host resources for an instruction it then declines is fine — none of
/// that is architectural — but an engine that has already written a register, moved
/// the PC, or spent the clock cannot decline afterwards, and must return a guest
/// fault instead.
pub trait ExecutionEngine<M: CpuMemory>: fmt::Debug {
    /// Which engine this is.
    fn kind(&self) -> EngineKind;

    /// Executes one guest instruction, or one translated block of them.
    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, EngineFault<M::Error>>;

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
    ) -> Result<StepResult, EngineFault<M::Error>> {
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
    ) -> Result<StepResult, EngineFault<M::Error>>;

    /// A guest address this engine must not execute an instruction *at* in its next step.
    ///
    /// **This is what stops a translated block from running past a breakpoint.** An
    /// engine that retires several instructions in one `step` would otherwise step
    /// straight over a guest address the debugger has asked to stop at, and the guest
    /// would skip a breakpoint it set — which is the one way a JIT can be observably
    /// wrong about debugging, because everything the guest computes is still correct.
    ///
    /// The contract for an engine is to treat `boundary` as the end of its block: it
    /// must not *include* the instruction at `boundary`, and everything before it is
    /// its to run. An engine that cannot honour a boundary declines, and the machine
    /// runs the instruction on an engine that can. An engine that only ever retires
    /// one instruction ignores this, which is the correct behaviour and is why the
    /// method has a default.
    ///
    /// `None` means no boundary, which is the common case. A boundary at or below the
    /// current program counter is refused by the machine rather than silently
    /// producing an empty block that would make the machine decline forever.
    fn set_yield_boundary(&mut self, boundary: Option<InstructionAddress>) {
        let _ = boundary;
    }

    /// How many guest instructions this engine has retired by its own means, if it counts.
    ///
    /// **`None` for an engine with nothing to report, which is most of them.** An
    /// interpreter that retires one instruction per call has no interesting number here —
    /// it is just its step count.
    ///
    /// This exists because B23 makes "did host code actually run?" unanswerable from the
    /// outside. A JIT that quietly delegated to the interpreter would leave the machine's
    /// architectural state *identical* to a genuine JIT run, and would pass every
    /// differential test in the repository, because the interpreter is the oracle. The
    /// only evidence that host code executed is the engine's own count of it, and
    /// without this accessor a test cannot reach that count through the machine's trait
    /// object.
    ///
    /// It is observability, never control flow: nothing branches on it, and a `None` is
    /// not a failure.
    fn native_instruction_count(&self) -> Option<u64> {
        None
    }

    /// Drops everything this engine holds that is not architectural.
    ///
    /// Called on a switch and on a reset. An engine whose private state affects
    /// what the guest can observe must not have that state; an engine that does not
    /// can leave this empty.
    fn discard_private_state(&mut self) {}
}
