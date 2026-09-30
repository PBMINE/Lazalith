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
    CpuFault, CpuFaultCause, CpuMemory, EngineKind, ExecutionEngine, OutcomeApplication, Processor,
    StepResult,
};
use lazalith_isa::{Instruction, Opcode, Operand};
use lazalith_types::InstructionAddress;

use crate::memory::Entry;

/// Why the JIT declined to run something.
///
/// **Separate from a `CpuFault`, because a JIT saying no is not a guest error.** A fault
/// is something the guest did; this is something this engine cannot do, and the
/// difference is what lets the machine fall back to the interpreter instead of trapping.
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
            executed_blocks: 0,
            executed_instructions: 0,
        }
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
        if let Some(found) = self
            .blocks
            .iter()
            .find(|entry| entry.start == start)
            .cloned()
        {
            return Ok(found);
        }
        let block = translation_of(processor, memory)?;
        let compiled = Compiled {
            start: block.start,
            end: block.end,
            instructions: block.instructions,
            cycles: block.cycles,
            flags: block.flags,
            offset: 0,
        };
        // The code goes into the page, and the metadata into the cache. **The offset is
        // zero for every block, because the page holds one block at a time** — see
        // `the_page_holds_one_block_at_a_time`, which is what keeps that from being a
        // silent bug: a second translation replaces the first, and the cache entry for
        // the first becomes stale, so the cache is cleared with the page.
        if self.page.is_none() {
            self.page = Some(ExecutablePage::new()?);
        }
        let page = self.page.as_mut().ok_or(Decline::NoExecutablePage)?;
        page.write(block.code.bytes())?;
        self.blocks.clear();
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
        let entry = page.entry();
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
    fn step(
        &mut self,
        processor: &mut Processor,
        memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        // Translate, then run, and decline to a fault-free error the machine can act on
        // by falling back. The machine calls the reference interpreter for anything the
        // JIT will not do, so a decline is *not* a guest fault and must not become one.
        if let Ok(block) = self.compile(processor, memory)
            && block.instructions > 0
        {
            match self.run_block(processor) {
                Ok(result) => return Ok(result),
                Err(_) => {
                    // A page that could not be entered is a host problem, not a guest
                    // one. Fall through to the interpreter rather than trapping, and
                    // drop the cache so the next attempt starts clean.
                    self.blocks.clear();
                    self.page = None;
                }
            }
        }
        Err(CpuFault::at(
            processor.architectural().pc(),
            None,
            CpuFaultCause::JitDeclined,
        ))
    }
    /// This JIT does not implement `execute`, and that is a decision rather than an
    /// omission.
    ///
    /// **The trait's `execute` is "run this one instruction the caller already
    /// decoded", which is a debugging and testing entry point.** The JIT translates runs
    /// of instructions starting from a program counter and has no way to start halfway
    /// through one, so honouring `execute` would mean either translating a one-instruction
    /// block — which is the B21 null result with extra steps — or quietly delegating to
    /// the interpreter, which is a JIT that claims to be a JIT and is not.
    ///
    /// Declining is therefore the honest answer, and `CpuFaultCause::JitDeclined` is a
    /// *machine-visible* signal rather than a guest fault: the caller asked for something
    /// this engine does not do, and the machine's response is to use the other engine.
    fn execute(
        &mut self,
        processor: &mut Processor,
        _instruction: &Instruction,
        _memory: &mut M,
    ) -> Result<StepResult, CpuFault<M::Error>> {
        Err(CpuFault::at(
            processor.architectural().pc(),
            None,
            CpuFaultCause::JitDeclined,
        ))
    }
    fn discard_private_state(&mut self) {
        self.blocks.clear();
        self.executed_blocks = 0;
        self.executed_instructions = 0;
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
