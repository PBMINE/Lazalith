//! B22: a host-native execution engine for LZA code.
//!
//! # What this is
//!
//! **A JIT that translates LZA machine instructions into host machine code and executes
//! that code.** §11 is explicit that the JIT is an execution engine and not a compiler,
//! an assembler, another ISA, or a language frontend — so this translates *loaded LZA
//! code*, at run time, and guest semantics stay LZA. There is no `C → special JIT` path
//! here and no way to add one: the JIT sees a `CpuMemory` and a `Processor` and nothing
//! about which front end produced the bytes.
//!
//! # The architecture it preserves
//!
//! ```text
//!                    ┌── Reference Interpreter  (semantic authority)
//! Canonical VM ──────┤
//!                    └── JIT                   (this crate)
//! ```
//!
//! **One canonical guest-visible state, in the `Processor`.** The JIT reads it, and
//! writes it back through the same accessors the interpreter uses. It holds no
//! architectural state of its own — its only private state is *translated code*, which
//! is a cache of the answer to "what host code computes these guest instructions", and
//! which is discarded on a switch by `discard_private_state`.
//!
//! # How a block runs, and why the boundary is exact
//!
//! A translated block is a straight-line run of guest instructions with **no memory
//! access and no control transfer** — see [`Translatable`] for the precise set. The
//! generated code loads each guest register from the machine's own register file,
//! computes on it, stores it back, advances the guest PC, and then executes a `RET`.
//!
//! **The `RET` is what makes the boundary exact.** It returns to the C caller, and at
//! that instant every guest register is already written back, the PC is already advanced,
//! and the machine's canonical state is complete and consistent. There is no "sync point",
//! no shadow state to flush, and no way for the JIT to be half-way through an update: a
//! block either ran to its `RET` or it did not run at all.
//!
//! That is also what makes §11's handoff requirements satisfiable without a second
//! mechanism. A breakpoint, a fault, a debug event, or an instruction the translator
//! refused all have the same shape — the block ends at the last instruction it could run,
//! the guest PC is at the next one, and the interpreter continues from there.
//!
//! # Why the translated subset is small
//!
//! **Conservatism here is a correctness argument, not a limitation to apologise for.** A
//! JIT that translated memory accesses would have to reproduce the bus's permission
//! checks, the MMU's address translation, and the fault-carrying struct *in host code*,
//! and any divergence would be a memory-safety bug rather than a wrong answer. A JIT that
//! translated only register operations cannot get memory wrong, because it does not touch
//! memory except the machine's own register file — and every instruction it does
//! translate is checked against the Reference Interpreter after every block, by
//! `crates/lazalith-machine/tests/differential.rs`.
//!
//! The cost of that choice is honest: most real guest code is memory traffic, so a JIT
//! that handles only register operations accelerates only part of a program. Extending it
//! to memory is the obvious next step and is *not* done here.
//!
//! # `unsafe`
//!
//! Two operations no safe Rust offers: making a page executable, and calling a function
//! pointer. Both are in [`memory`] and `execute`, and both are as small as they can be —
//! one `mmap`, one `mprotect`, one transmute, and the transmute is of an address into a
//! page this crate itself just filled.
//!
//! This is the **second and last** crate in the workspace allowed to contain `unsafe`,
//! after `lazalith-sdl3`. There is no way to write a JIT without those two operations, so
//! the crate that does it is the crate that has to be trusted; the workspace's answer is
//! to make that trust explicit — the `forbid` is overridden in this crate's own manifest
//! rather than removed, and `unsafe_audit_is_two_known_crates` in
//! `crates/lazalith-sdl3/tests/migration.rs` asserts the allowlist is exactly these two
//! names. A third crate would have to be added to that literal in review, which is the
//! point.

#![deny(missing_docs)]

extern crate alloc;

mod memory;
mod translate;
mod x86;

pub use memory::ExecutablePage;
pub use translate::translation_of;
pub use translate::{
    FLAGS_SCRATCH, Flags, FlagsOperand, Translatable, Translated, is_translatable, reason,
    translatable,
};

use alloc::vec::Vec;
use core::fmt;

use lazalith_cpu::{
    CpuMemory, EngineDecline, EngineFault, EngineKind, ExecutionEngine, OutcomeApplication,
    Processor, StepResult,
};
use lazalith_isa::{Instruction, Opcode, Operand};
use lazalith_types::InstructionAddress;

use crate::memory::Entry;

/// Why the JIT declined to run something.
///
/// **A JIT-internal reason code, and it becomes an [`EngineDecline`] at the boundary.**
/// This type is deliberately more specific than the boundary type: it says *which*
/// translator rule refused, which is what a person debugging a slow program needs, while
/// the machine only needs to know whether to hand the instruction to another engine. The
/// conversion is [`Decline::at`], and it is where the specificity is thrown away.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decline {
    /// The instruction at this address is not one the translator handles.
    NotTranslatable,
    /// There is no translated code for this address and none could be built.
    NoCode,
    /// The host is not one this crate emits code for.
    UnsupportedHost,
    /// The guest's word width is not one the generated code computes in.
    ///
    /// **A 32-bit machine is declined rather than half-supported.** The generated code
    /// does 64-bit arithmetic; a 32-bit machine truncates every register write, so the
    /// two disagree above the low word. Emitting a mask after every instruction would fix
    /// it and is not done, because a wrong register is worse than an interpreted one.
    UnsupportedWordWidth,
    /// The translated code could not be made executable.
    NoExecutablePage,
}

impl Decline {
    /// The boundary-level form of this reason.
    ///
    /// **The classification is what matters and the sentence is a diagnostic.** A 32-bit
    /// guest and a missing executable page are both `UnsupportedConfiguration` and
    /// `HostUnavailable` respectively — not `UnsupportedInstruction` — because neither
    /// becomes true by running a different instruction, and a machine that has only this
    /// engine must keep interpreting rather than trapping the guest over a property of
    /// the machine it is running on.
    pub const fn at(self) -> EngineDecline {
        match self {
            Self::NotTranslatable | Self::NoCode => EngineDecline::UnsupportedInstruction {
                reason: "the translator has no code for this instruction",
            },
            Self::UnsupportedHost => EngineDecline::UnsupportedConfiguration {
                reason: "this crate emits code for x86-64 only",
            },
            Self::UnsupportedWordWidth => EngineDecline::UnsupportedConfiguration {
                reason: "the generated code computes in 64 bits and this guest is 32-bit",
            },
            Self::NoExecutablePage => EngineDecline::HostUnavailable {
                reason: "no page could be mapped executable",
            },
        }
    }
}
impl fmt::Display for Decline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTranslatable => f.write_str("the instruction is not one the JIT translates"),
            Self::NoCode => f.write_str("there is no translated code at this address"),
            Self::UnsupportedHost => f.write_str("this host is not one the JIT emits code for"),
            Self::UnsupportedWordWidth => {
                f.write_str("the guest's word width is not one the generated code computes in")
            }
            Self::NoExecutablePage => {
                f.write_str("the translated code could not be made executable")
            }
        }
    }
}
/// A host-native execution engine for LZA code.
///
/// Holds translated blocks and the page they live in. **Nothing here is architectural**:
/// the guest's registers, PC and status are in the `Processor` the machine owns, and
/// this struct's fields are a cache keyed by guest address.
#[derive(Debug)]
pub struct Jit {
    blocks: Vec<Compiled>,
    page: Option<ExecutablePage>,
    /// A guest address the next translated block must not run an instruction at.
    ///
    /// **Set by the machine from the debugger's breakpoints, and honoured by the
    /// translator.** It lives here rather than being passed to `step` because it is a
    /// property of the *engine* for as long as it holds: a block translated under one
    /// boundary is a different block from one translated under another, so it has to
    /// invalidate the cache when it changes, which is not a decision a per-call argument
    /// could make.
    yield_boundary: Option<InstructionAddress>,
    /// Where the next block.s bytes go in the page.
    ///
    /// **A bump pointer, because the page is an arena rather than a slot.** B22 made it a
    /// slot {2014} one block, overwritten by the next {2014} which is a code cache that can never
    /// hit, and B24 measured the consequence: a JIT six times slower than the interpreter
    /// on the very code it translates. When the arena fills, the page is dropped and a new
    /// epoch begins, because every block in it is derivable from the guest.s bytes and a
    /// fresh `mmap` is cheaper than an eviction policy.
    next_offset: usize,
    /// How many times the translator has run since the last `discard_private_state`.
    translations: u64,
    /// How many blocks were executed natively, and how many instructions they retired.
    ///
    /// **Reported rather than assumed.** A JIT that quietly fell back to the
    /// interpreter for everything would pass every differential test, because the
    /// interpreter is correct. These counters are how a test asks the question that
    /// matters — did any guest instruction actually run as host code — and
    /// `the_jit_actually_executed_host_code` is that test.
    executed_blocks: u64,
    executed_instructions: u64,
}
impl Default for Jit {
    fn default() -> Self {
        Self::new()
    }
}
impl Jit {
    /// The engine.
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            page: None,
            yield_boundary: None,
            next_offset: 0,
            translations: 0,
            executed_blocks: 0,
            executed_instructions: 0,
        }
    }

    /// Forgets the translated code and the executable page, keeping the counters.
    ///
    /// **Split out of [`discard_private_state`] because a page that could not be entered
    /// is a different problem from an engine switch.** A page that failed to run is
    /// possibly a corrupted mapping, and re-entering it would be worse than translating
    /// again; the counters, on the other hand, are how the tests show host code really
    /// executed, and resetting them because a debugger moved a breakpoint would make the
    /// handoff untestable.
    fn discard_code(&mut self) {
        self.blocks.clear();
        self.page = None;
        self.next_offset = 0;
    }

    /// How many translated blocks have run natively.
    pub const fn executed_blocks(&self) -> u64 {
        self.executed_blocks
    }
    /// How many guest instructions have retired in host code.
    pub const fn executed_instructions(&self) -> u64 {
        self.executed_instructions
    }
    /// How many blocks are translated and cached.
    pub fn cached_blocks(&self) -> usize {
        self.blocks.len()
    }

    /// How many times the translator has run.
    ///
    /// **A count of *translations*, and it is the only performance number in this crate that
    /// is a regression guard.** Wall-clock figures depend on the host, on the page fault
    /// that did or did not happen, and on whatever else the machine is doing, and a test
    /// that asserts one gets deleted the first time it is inconvenient. This is a property
    /// of the engine.s logic: a block that is already translated must not be translated
    /// again, and when it was not, this number must have gone up.
    ///
    /// B24 measured the consequence of getting it wrong. The page held one block at a time,
    /// so every step translated a fresh block over the previous one, and the JIT came out
    /// *six times slower than the interpreter* on straight-line register arithmetic {2014} the
    /// code it translates perfectly. The number was 1 per step where it should have been 1
    /// per block, and it is public now so that it can be asserted on rather than inferred
    /// from a clock.
    pub const fn translations(&self) -> u64 {
        self.translations
    }

    /// The cached blocks, for a test that wants to look at them.
    ///
    /// **Read-only, and named for the test rather than for a use.** Nothing in the crate
    /// consults a block after compiling it except `run_block`, which looks it up by guest
    /// address; exposing the list is so that a test can check offsets and starts without
    /// reaching into private fields.
    pub fn blocks(&self) -> &[Compiled] {
        &self.blocks
    }

    /// Runs one already-compiled block, for a test that has compiled one itself.
    ///
    /// **The single step of `step` with no translation in front of it.** `step` translates
    /// and then runs; a test that has already translated wants the run, and going through
    /// `step` would translate again {2014} which is the thing such a test exists to avoid. It
    /// cannot be wrong about the cache because it does not consult it.
    pub fn run_block_for_test(&mut self, processor: &mut Processor) -> Result<StepResult, Decline> {
        self.run_block(processor)
    }
    /// Translates the run of instructions starting at the processor's PC, if it can.
    ///
    /// **Translated once and cached by guest address.** A JIT that translated on every
    /// step would be slower than the interpreter by the cost of an assembler, which is
    /// the failure mode B21's null result warns about in a different place.
    ///
    /// Returns the block's *metadata*, not a borrow of the cache. A `&Translated` would
    /// borrow `self` for as long as the caller held it, which is the opposite of what a
    /// code cache wants: the caller needs to run the block, and running it needs `&mut
    /// self` to count the run.
    pub fn compile<M: CpuMemory>(
        &mut self,
        processor: &Processor,
        memory: &mut M,
    ) -> Result<Compiled, Decline> {
        let start = processor.architectural().pc().as_u64();
        if !cfg!(target_arch = "x86_64") {
            return Err(Decline::UnsupportedHost);
        }
        // **The cache key is the address *and* the boundary.** A block translated with no
        // boundary runs up to 31 instructions, and one translated under a boundary stops
        // before it — so they are different code for the same address, and returning the
        // wrong one would run a block straight through a breakpoint the debugger has
        // since set. Comparing the boundary here, rather than trusting
        // `set_yield_boundary` to have cleared the cache, means the invariant holds even
        // if that method is ever changed to do less.
        if let Some(found) = self
            .blocks
            .iter()
            .find(|entry| entry.start == start && entry.boundary == self.yield_boundary)
            .copied()
        {
            return Ok(found);
        }
        let block = translation_of(processor, memory, self.yield_boundary)?;
        self.translations += 1;
        // The code goes into the page, and the metadata into the cache. **The offset is
        // where in the page this block's bytes went**, and it is not always zero: the page
        // is an arena that several blocks share.
        //
        // **A page that held one block at a time was a cache that never hit**, and B24's
        // measurement found it: the JIT was six times *slower* than the interpreter on
        // straight-line register arithmetic — the code it translates perfectly — because
        // every step translated a fresh block over the previous one, so no block was ever
        // run twice and the assembler ran on every step. That is not a performance
        // limitation of a JIT, it is a cache wired to never be read.
        if self.page.is_none() {
            self.page = Some(ExecutablePage::new()?);
            self.next_offset = 0;
        }
        let page = self.page.as_mut().ok_or(Decline::NoExecutablePage)?;
        let code = block.code.bytes();
        if self.next_offset + code.len() > page.capacity() {
            // The arena is full. **Throw it away and start a new epoch** rather than
            // evicting one block: this page is a cache, every entry in it is derivable from
            // the guest's bytes, and replacing the whole thing is one `mmap` instead of a
            // per-block bookkeeping scheme that would still be a cache with a bad policy.
            page.reset();
            self.blocks.clear();
            self.next_offset = 0;
        }
        let offset = self.next_offset;
        page.write_at(offset, code)?;
        self.next_offset = offset + code.len();
        let compiled = Compiled {
            start: block.start,
            end: block.end,
            instructions: block.instructions,
            cycles: block.cycles,
            flags: block.flags,
            boundary: self.yield_boundary,
            offset,
        };
        self.blocks.push(compiled);
        Ok(compiled)
    }
    /// Runs one translated block, or says why it cannot.
    fn run_block(&mut self, processor: &mut Processor) -> Result<StepResult, Decline> {
        let start = processor.architectural().pc().as_u64();
        let Some(block) = self
            .blocks
            .iter()
            .find(|entry| entry.start == start)
            .cloned()
        else {
            return Err(Decline::NoCode);
        };
        let page = self.page.as_ref().ok_or(Decline::NoExecutablePage)?;
        // The entry is the block.s own offset, not the page start: several blocks share
        // the page, and calling the first one for all of them would run the wrong code.
        let entry = page.entry_at(block.offset);
        // **The block is handed a copy of the register file and a pointer to the PC**,
        // and nothing else. It cannot reach the machine's memory, its devices or its
        // clock, so the worst a mistranslated block can do is compute a wrong register
        // value — which the differential tests turn into a wrong answer rather than a
        // wrong machine.
        //
        // SAFETY: the entry came from a page this crate filled with code it emitted,
        // and the two out-parameters are `execute`'s own stack.
        execute(entry, processor, &block)?;
        self.executed_blocks += 1;
        self.executed_instructions += u64::from(block.instructions);
        Ok(StepResult::block(
            OutcomeApplication::Continue,
            block.cycles,
            block.instructions,
        ))
    }
}
/// A block that has been translated and whose code is in the page.
///
/// **Metadata only — the code is in the [`ExecutablePage`]**, and this is what the cache
/// stores. The distinction matters because the page is *one block at a time* in this
/// stage: a second translation overwrites the first's bytes, so the cache is cleared
/// with it. That is a limitation, not a design, and it is what
/// `the_page_holds_one_block_at_a_time` asserts so it cannot be forgotten.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Compiled {
    /// The guest address the block starts at.
    pub start: u64,
    /// The guest address the block ends at.
    pub end: u64,
    /// How many guest instructions it retires.
    pub instructions: u16,
    /// What they cost in total.
    pub cycles: u8,
    /// Where its code sits in the page. Always zero in this stage.
    pub offset: usize,
    /// The instruction whose result is in the status register afterwards, if any.
    pub flags: Option<Flags>,
    /// The yield boundary this block was translated under.
    ///
    /// **Part of the cache key, and the reason is that a boundary changes the code.**
    /// Two blocks starting at the same guest address, one bounded and one not, retire
    /// different numbers of instructions and are therefore different programs as far as
    /// the debugger is concerned. Keeping it in the metadata is what lets `compile`
    /// refuse to hand back the wrong one.
    pub boundary: Option<InstructionAddress>,
}
/// Runs one translated block against the processor's architectural state.
///
/// # The copy in, copy out is the point
///
/// **The JIT is handed a stack array it owns, not a pointer into the machine's
/// registers.** A JIT that took `&mut RegisterFile` would be one accessor away from
/// holding architectural state, and B3's whole rule is that only the `Processor` may.
/// Copying sixteen words in and out costs a few nanoseconds against a block that has
/// already saved a dispatch per instruction, and it makes "the JIT owns no
/// architectural state" a structural property rather than a promise: there is no
/// pointer to fork, because the only memory the generated code can reach is this
/// function's stack frame.
///
/// # The flags are computed here, and that is a decision
///
/// **The guest's N, Z, C and V are derived from the spilled operands using
/// `lazalith_types::WordWidth` — the same functions the Reference Interpreter uses.** The
/// alternative is to build the guest's status out of x86's own flags with `SETcc` and the
/// `ADC`/`SBB` idioms, which is what a JIT with no fallback would do; it is not done here
/// for two reasons.
///
/// The first is correctness by construction. x86's carry and overflow are *almost* the
/// guest's, and "almost" is the problem: they agree for `ADD` and `SUB` and are
/// explicitly undefined for `IMUL`, so a `SETcc`-based status would have to clear them by
/// hand for multiply and would have to be argued rather than checked. Deriving the flags
/// from the same `width.add`/`width.sub`/`width.mul` calls means there is no second
/// definition of the guest's condition codes in the system at all, so the two engines
/// cannot disagree about them — they are running the same code.
///
/// The second is that the cost is bounded and off the critical path in a way that does not
/// matter here: three spilled words per block, and a handful of host operations after the
/// block has already retired its instructions. A JIT that eventually translates memory
/// will want the flags in host registers for the same reason it will want the block to
/// branch, and that is the step where this moves.
///
/// **This is a real limitation, recorded as one:** the flags are recomputed after the
/// block rather than computed by it, so a guest that reads the status register between two
/// translated instructions would see stale values. That cannot happen *in this stage*,
/// because every instruction this translator emits either sets the flags or leaves them
/// alone, and a block runs to its `RET` before the guest can observe anything.
///
/// # Why there is no error path
///
/// **Nothing the block does can fail, and that is the reason the function is allowed to
/// be total.** A block has no memory operand, no division, no control transfer and no
/// privileged operation, so it has no instruction that can trap. An earlier version
/// declared the call as returning `i32` and treated a non-zero return as "the block could
/// not finish" — but the generated code was a `RET` and nothing more, so what it read was
/// whatever the last arithmetic instruction happened to leave in `EAX`. On a block ending
/// in `ADD` that was non-zero, so the block reported failure on success. The check was not
/// merely inert; it was a coin flip on the last instruction, and it is gone because there
/// is nothing left to check.
fn execute(entry: Entry, processor: &mut Processor, block: &Compiled) -> Result<(), Decline> {
    let mut registers = [0u64; REGISTER_COUNT];
    let mut pc = processor.architectural().pc().as_u64();
    let mut scratch = [0u64; FLAGS_SCRATCH];
    for (index, slot) in registers.iter_mut().enumerate() {
        // A register the machine does not have reads as zero and is never written back,
        // because `write_register_raw` refuses an index past the count.
        *slot = processor
            .architectural()
            .registers()
            .read_raw(index as u8)
            .unwrap_or(0);
    }
    // SAFETY: forwarded from the caller, which built the page and the code. All three
    // out-parameters are this function's own stack, live across the call.
    unsafe { call(entry, registers.as_mut_ptr(), &mut pc, scratch.as_mut_ptr()) };
    write_back(processor, &registers);
    if let Some(flags) = block.flags {
        apply_flags(processor, &flags, &scratch)?;
    }
    // The PC the translator computed, not the one the block left behind: both agree, and
    // the translator's is the one derived from the ISA's own instruction width.
    processor
        .architectural_mut()
        .set_pc(InstructionAddress::new(block.end))
        .map_err(|_| Decline::NotTranslatable)?;
    Ok(())
}
/// Derives the guest's status flags from what a block spilled.
///
/// **The same arithmetic the interpreter performs**, so that a block's flags and an
/// interpreted instruction's flags are produced by one piece of code. The spilled words
/// are the instruction's two operands and its result; the result is used for N and Z and
/// the operands for C and V, which is why all three are needed and why the operands are
/// spilled *before* the operation.
fn apply_flags(
    processor: &mut Processor,
    flags: &Flags,
    scratch: &[u64; FLAGS_SCRATCH],
) -> Result<(), Decline> {
    let width = processor.config().word_width();
    let left = scratch[0];
    let right = scratch[1];
    let value = scratch[2];
    // `width.add` and friends return `Result` because a 32-bit machine rejects a value
    // that does not fit, and this translator declines those machines, so the error here
    // would be a bug rather than a guest condition. It is still propagated, not ignored.
    let result = match flags.opcode {
        Opcode::Add => width.add(left, right),
        Opcode::Sub => width.sub(left, right),
        Opcode::Addi => width.add(left, right),
        Opcode::Subi => width.sub(left, right),
        Opcode::Mul => width.mul(left, right),
        Opcode::And => width.bitand(left, right),
        Opcode::Or => width.bitor(left, right),
        Opcode::Xor => width.bitxor(left, right),
        _ => return Err(Decline::NotTranslatable),
    };
    // The result the block computed and the one the width functions derive must be the
    // same number, or the flags describe an arithmetic result the guest never produced.
    debug_assert_eq!(
        result.value, value,
        "the block's own result and the flags' result must agree"
    );
    processor.architectural_mut().update_arithmetic(result);
    Ok(())
}
/// Copies the block's results back into the processor.
fn write_back(processor: &mut Processor, registers: &[u64; REGISTER_COUNT]) {
    for (index, value) in registers.iter().enumerate() {
        // An invalid index is a bug in the translator rather than a guest error, and the
        // checked accessor refuses it, so a write that cannot happen cannot corrupt
        // anything.
        let _ = processor
            .architectural_mut()
            .write_register_raw(index as u8, *value);
    }
}
/// Calls translated code.
///
/// # Safety
///
/// `entry` must be executable code emitted for this host with this exact signature, and
/// all three pointers must be valid for the writes the block makes.
unsafe fn call(entry: Entry, registers: *mut u64, pc: *mut u64, scratch: *mut u64) {
    // SAFETY: the transmute is of a pointer this crate obtained from a page it filled
    // with code it generated, into the signature it generated that code for. The three
    // out-parameters are this function's own stack, live for the call.
    let function: unsafe extern "C" fn(*mut u64, *mut u64, *mut u64) =
        unsafe { core::mem::transmute(entry) };
    // SAFETY: forwarded from this function's own precondition.
    unsafe { function(registers, pc, scratch) };
}
/// How many general registers the machine has, as a compile-time count.
///
/// **A `const` assertion rather than a number written down**, so a change to the
/// register count is a compile error here instead of a block that copies too few
/// registers and silently leaves the rest stale.
pub const REGISTER_COUNT: usize = lazalith_types::RegisterIndex::COUNT as usize;
const _: () = assert!(
    REGISTER_COUNT == 16,
    "the JIT copies exactly the registers there are"
);
impl<M: CpuMemory> ExecutionEngine<M> for Jit {
    fn kind(&self) -> EngineKind {
        EngineKind::Jit
    }

    /// Runs one translated block, or declines so another engine can.
    ///
    /// **This is B23's JIT → interpreter handoff, and the two things that make it
    /// correct are both about what has *not* happened when this returns `Err`.**
    ///
    /// A decline leaves the [`Processor`] exactly as it was found. Translation writes
    /// only to this crate's own cache and page; nothing architectural is touched until
    /// `run_block` has called the generated code, and a block that could not be built
    /// never got that far. So the program counter still names the instruction that was
    /// declined, which is what lets the interpreter run *that* instruction rather than
    /// re-running or skipping one.
    ///
    /// Second, a decline is [`EngineFault::Declined`], not a fault. The machine's
    /// response is to hand the instruction to the reference interpreter, and the guest
    /// sees an ordinary retired instruction. In B22 this returned
    /// `CpuFaultCause::JitDeclined`, which the machine — treating every engine `Err` as
    /// a guest fault — turned into a trap, so a program was trapped for using an
    /// instruction the JIT had not learned. The type is what prevents that now.
    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, EngineFault<M::Error>> {
        let start = processor.architectural().pc().as_u64();
        match self.compile(processor, memory) {
            Ok(block) if block.instructions > 0 => match self.run_block(processor) {
                Ok(result) => Ok(result),
                Err(decline) => {
                    // Entering the generated code failed, which is a host problem and not
                    // a guest one. Forget the cache so the next attempt starts from a
                    // fresh page rather than re-entering code that did not run, and let
                    // the interpreter have this instruction.
                    self.discard_code();
                    Err(EngineFault::Declined(decline.at()))
                }
            },
            // Two declines, and the distinction is worth keeping: one means "this
            // instruction", the other means "this machine". `NoCode` is the second
            // because a cache hit cannot produce an empty block.
            Ok(_) => Err(EngineFault::Declined(Decline::NoCode.at())),
            Err(decline) => Err(EngineFault::Declined(decline.at())),
        }
        .inspect_err(|_| {
            debug_assert_eq!(
                processor.architectural().pc().as_u64(),
                start,
                "a decline must leave the program counter on the instruction that was \
                 declined, or the interpreter will run the wrong one"
            );
        })
    }

    /// This JIT does not implement `execute`, and that is a decision rather than an
    /// omission.
    ///
    /// **The trait's `execute` is "run this one instruction the caller already
    /// decoded", which is a debugging and testing entry point.** The JIT translates runs
    /// of instructions starting from a program counter and has no way to start halfway
    /// through one, so honouring `execute` would mean either translating a
    /// one-instruction block — which is the B21 null result with extra steps — or
    /// quietly delegating to the interpreter, which is a JIT that claims to be a JIT and
    /// is not.
    ///
    /// Declining is therefore the honest answer, and [`EngineFault::Declined`] is the
    /// *machine-visible* signal for it: the caller asked for something this engine does
    /// not do, and the machine's response is to use the other engine.
    fn execute(
        &mut self,
        _processor: &mut Processor,
        _instruction: &Instruction,
        _memory: &mut M,
    ) -> Result<StepResult, EngineFault<M::Error>> {
        Err(EngineFault::Declined(Decline::NotTranslatable.at()))
    }

    /// A guest address this engine must not execute an instruction at in its next step.
    ///
    /// **This is what stops a block running through a breakpoint.** A translated block
    /// retires up to 31 instructions in one `step`, so without a boundary a debugger's
    /// breakpoint at the third instruction would be stepped over silently — the guest's
    /// arithmetic would be right and the debugger would be lying about where it stopped.
    ///
    /// The boundary is stored and applied by the *translator*, not here, because it has
    /// to shorten the block during translation; by the time there is code to run, the
    /// decision has been made. The cache is invalidated when the boundary moves, since a
    /// block built for one boundary is wrong for another — a block compiled without a
    /// boundary would run straight past a breakpoint the debugger has since set.
    fn set_yield_boundary(&mut self, boundary: Option<InstructionAddress>) {
        if boundary != self.yield_boundary {
            // Only the *code* goes. The execution counters are how B23's tests show that
            // host code actually ran, and forgetting those on a debugger's whim would
            // make the handoff untestable.
            self.blocks.clear();
            self.yield_boundary = boundary;
        }
    }

    /// **Reported through the trait object, which is the only way a machine-level test can
    /// see it.** The machine holds a `Box<dyn ExecutionEngine>` and has no idea a JIT
    /// is behind it, so without this the count would be unreachable from a test that
    /// only has a machine — and "the JIT ran everything" and "the JIT ran nothing and
    /// the interpreter did all the work" are the same architectural state.
    fn native_instruction_count(&self) -> Option<u64> {
        Some(self.executed_instructions)
    }

    fn discard_private_state(&mut self) {
        self.blocks.clear();
        self.yield_boundary = None;
        self.executed_blocks = 0;
        self.executed_instructions = 0;
        self.translations = 0;
    }
}
/// The operand kinds the translator accepts.
///
/// **Only registers and immediates.** A memory operand is declined, and that is the whole
/// conservative argument: see the module documentation.
pub(crate) fn is_register_or_immediate(operand: &Operand) -> bool {
    matches!(operand, Operand::Register(_) | Operand::Immediate(_))
}
/// Whether this instruction is a control transfer, which ends a block.
pub(crate) fn is_control_transfer(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Br
            | Opcode::Jmp
            | Opcode::Call
            | Opcode::Callr
            | Opcode::Ret
            | Opcode::Rfe
            | Opcode::Syscall
            | Opcode::Trap
            | Opcode::Halt
    )
}
/// The instruction width, which the generated code adds to the guest PC after each one.
///
/// **A named constant, and the translator adds it to the PC itself** rather than the
/// host code. The host could have done `ADD RDI, 8`, and putting the width in Rust
/// instead means the PC the guest ends at is the PC the translator computed from the
/// ISA's own definition, with no second copy of the width in host code to disagree.
pub(crate) const INSTRUCTION_WIDTH: u64 = 8;
