//! Turning a run of LZA instructions into host code.
//!
//! # The rule
//!
//! **A translated block is a straight-line run of guest instructions that read and write
//! only registers.** No memory operand, no control transfer, no privileged operation, no
//! stack. Everything else ends the block, and the machine runs the rest through the
//! Reference Interpreter from the guest PC the block left behind.
//!
//! # Why the rule is this narrow
//!
//! **Conservatism is the correctness argument.** A JIT that translated a memory access
//! would have to reproduce the bus's permission checks, the address translation and the
//! fault-carrying struct in host code, and a divergence there is not a wrong answer but
//! a memory-safety bug. A JIT that translates only register operations cannot get memory
//! wrong, because it never touches guest memory — it reads and writes the machine's own
//! register file through a pointer the machine handed it.
//!
//! The cost is honest and is recorded in `docs/project-state.md`: most real guest code is
//! memory traffic, so this accelerates the register-bound parts of a program and leaves
//! the rest to the interpreter.
//!
//! # What the generated code looks like
//!
//! ```text
//!     ; on entry: RDI = guest register block
//!     ;           RSI = guest PC
//!     ;           RDX = three-word scratch for the flags
//!     MOV  R10, [RDI + r_a*8]     ; load the left source
//!     MOV  R11, [RDI + r_b*8]     ; load the right source
//!     ADD  R10, R11
//!     MOV  [RDI + r_d*8], R10     ; store the destination
//!     ADD  QWORD PTR [RSI], 8     ; the PC moved on by one instruction
//!     ...                         ; one such group per guest instruction
//!     RET                         ; the exact boundary
//! ```
//!
//! **There is no prologue, and that is deliberate.** `RDI` and `RSI` are where the System V
//! C ABI puts the first and second arguments, so the register block pointer and the PC
//! pointer arrive exactly where the emitted code wants them. The first version of this
//! file began by loading the PC into `RDI` on the assumption that `RDI` was free, which
//! overwrote the register block pointer with the PC and made the block read its inputs
//! from the wrong address — no crash, just wrong answers, which is the failure mode this
//! comment exists to prevent. The third argument lands in `RDX`, so `RDX` is the flags
//! scratch and the block's own scratch registers are `R10` and `R11` — deliberately *not*
//! the argument registers.
//!
//! **The `RET` is the boundary.** At that point the machine's canonical state holds
//! everything, so a switch, a snapshot or a fault at this point needs no reconciliation.

use alloc::format;
use alloc::string::String;

use lazalith_cpu::{CpuMemory, Processor};
use lazalith_isa::{Instruction, Operand, decode};
use lazalith_types::InstructionAddress;

use crate::Decline;
use crate::x86::{Code, Reg};

/// The most guest instructions in one translated block.
///
/// **Bounded by the clock, not by taste.** `StepResult` carries a block's cycle total in
/// a `u8`, and the most expensive instruction the ISA charges is 8 cycles — see
/// [`MAX_BLOCK_CYCLES`], which turns that into a hard limit. A bound picked for
/// roundness (64, as the first version did) admits a block of multiplies whose true cost
/// is 512, which does not fit, and both ways of coping are wrong: a saturating add reports
/// a *smaller* number than the block cost, so the virtual clock runs backwards, and a
/// wrapping one reports zero. A guest can read that clock, so either is a guest-visible
/// wrong answer rather than a rounding detail.
///
/// 31 is the largest block whose worst case still fits, at 248 cycles. It also keeps
/// every block comfortably inside the emitter's one-byte `JMP rel8` displacement, so no
/// block can ever be too long to skip — the emitter's constraint and the clock's happen
/// to agree, and the number is the one that satisfies both.
pub const MAX_BLOCK: usize = 31;

/// Whether the translator handles this instruction.
///
/// **The single place the answer lives.** Everything that needs to know — the
/// translator, and any caller deciding whether to expect native execution — goes through
/// here.
pub fn is_translatable(instruction: &Instruction) -> bool {
    let opcode = instruction.opcode();
    if crate::is_control_transfer(opcode) {
        return false;
    }
    if instruction.definition().supervisor_only {
        return false;
    }
    if !instruction
        .operands()
        .iter()
        .all(crate::is_register_or_immediate)
    {
        return false;
    }
    // Only the operations the emitter has an x86-64 encoding for. `Not`, the shifts and
    // the divides are not in the first translation pass — the divides because x86's
    // divide traps on zero and a divide-by-zero in a JIT-compiled block would be a *host*
    // `#DE` rather than a guest fault with a guest PC, and the shifts because the shift
    // count's masking rule has to match the ISA's exactly. Saying so here is what keeps
    // the emitter and the translator in step.
    matches!(
        opcode,
        lazalith_isa::Opcode::Li
            | lazalith_isa::Opcode::Mov
            | lazalith_isa::Opcode::Add
            | lazalith_isa::Opcode::Sub
            | lazalith_isa::Opcode::Addi
            | lazalith_isa::Opcode::Subi
            | lazalith_isa::Opcode::Mul
            | lazalith_isa::Opcode::And
            | lazalith_isa::Opcode::Or
            | lazalith_isa::Opcode::Xor
    )
}

/// The trait this module exposes so a caller can ask about a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Translatable {
    /// The instruction can appear in a block.
    Yes,
    /// It cannot, and this is why.
    No(&'static str),
}

/// Whether `instruction` is translatable, with a reason when it is not.
///
/// **Re-exported, because a caller debugging a slow program needs to know which
/// instruction is stopping translation and a `bool` cannot say.**
pub fn translatable(instruction: &Instruction) -> Translatable {
    let opcode = instruction.opcode();
    if crate::is_control_transfer(opcode) {
        return Translatable::No("it transfers control");
    }
    if instruction.definition().supervisor_only {
        return Translatable::No("it is privileged");
    }
    if let Some(operand) = instruction
        .operands()
        .iter()
        .find(|operand| !crate::is_register_or_immediate(operand))
    {
        return match operand {
            Operand::Memory { .. } => Translatable::No("it accesses memory"),
            _ => Translatable::No("its operands are not registers or immediates"),
        };
    }
    match is_translatable(instruction) {
        true => Translatable::Yes,
        false => Translatable::No("there is no host encoding for it yet"),
    }
}

/// A translated run of guest instructions.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Translated {
    /// The host code.
    pub code: Code,
    /// The guest address the block starts at.
    pub start: u64,
    /// The guest address the block ends at — the next instruction to run.
    ///
    /// **Carried out of the translator rather than written by the generated code.**
    /// The block does advance the PC through `RSI` as it goes, so the two agree; but the
    /// value the machine commits is this one, computed from the ISA's own instruction
    /// width as the block was built. A host-code PC update and a Rust PC computation are
    /// two ways of getting one number, and the number is architectural state, so the one
    /// the translator derived is the one that is trusted.
    pub end: u64,
    /// How many guest instructions it retires.
    pub instructions: u16,
    /// What those instructions cost, in total.
    pub cycles: u8,
    /// The last instruction in the block that set the guest's status flags, if any.
    ///
    /// **Only the last one matters, and that is why it is `Option` on the block and not a
    /// per-instruction list.** The status register is architectural state, so after a
    /// block only the most recent arithmetic result is observable. The block spills this
    /// instruction's operands and result into the flags scratch for exactly this reason.
    pub flags: Option<Flags>,
    /// The opcode of the last instruction, for diagnostics.
    ///
    /// **An `Option`, because a block that translated nothing has no last
    /// instruction** — and `Opcode` has no `Default`, which is the compiler saying
    /// exactly that. An empty block is declined before it is built, so this is only
    /// `Some` in a block that exists, but the type says so rather than a sentinel.
    pub last_opcode: Option<lazalith_isa::Opcode>,
}

/// Where a flag-setting instruction's operands came from.
///
/// **Distinguished rather than pre-loaded, because a register's value at translation time
/// is not its value when the block runs.** The block spills the values themselves, so all
/// this has to say is *which* operands to compute the flags from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlagsOperand {
    /// A guest register, spilled by the block.
    Register(u8),
    /// A 32-bit sign-extended immediate, which needs no spilling.
    Immediate(i32),
}

/// The instruction whose result is in the guest's status flags after this block runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Flags {
    /// The operation, which decides how the flags are derived.
    pub opcode: lazalith_isa::Opcode,
    /// The left operand.
    pub left: FlagsOperand,
    /// The right operand.
    pub right: FlagsOperand,
}

/// How many words the flags scratch holds: left, right, result.
pub const FLAGS_SCRATCH: usize = 3;

/// The most cycles a translated block can cost, given [`MAX_BLOCK`] and the ISA's costs.
///
/// **Computed in `usize` so the assertion below means something.** Written as a `u8` the
/// product would wrap at 256 and the check would compare a `u8` against `u8::MAX` —
/// always true, and a compile error for the wrong reason. The widest instruction the ISA
/// charges is 8 cycles, and the bound is 64 instructions.
pub const MAX_BLOCK_CYCLES: usize = MAX_BLOCK * 8;

const _: () = assert!(
    MAX_BLOCK_CYCLES <= u8::MAX as usize,
    "a translated block's cycle total must fit in the u8 StepResult carries"
);

/// The byte offset of a guest register in the register file.
///
/// **The machine's own `RegisterFile` layout, asserted in a test.** If the register
/// file ever changed shape — reordered, or given a flag word in front — every
/// translated block would read the wrong register, and the differential tests would
/// catch it as a wrong answer rather than as a wrong encoding. Asserting it here means
/// the failure is at this file instead.
pub const fn register_offset(index: u8) -> i32 {
    (index as i32) * 8
}

/// Translates the run of instructions starting at the processor's PC.
///
/// **Declines rather than guessing.** A decline is not a guest fault; the machine uses
/// it to run the instruction through the Reference Interpreter instead, which is why
/// this returns a `Result` with a `Decline` and not a `CpuFault`.
pub fn translation_of<M: CpuMemory>(
    processor: &Processor,
    memory: &mut M,
    boundary: Option<InstructionAddress>,
) -> Result<Translated, Decline> {
    let config = processor.config();

    // A 32-bit machine truncates every register write to 32 bits, and the generated code
    // does 64-bit arithmetic. Declining is the only way those two can agree without the
    // translator emitting a mask after every instruction, and a wrong register is a far
    // worse outcome than a slow one.
    if config.word_bits() != 64 {
        return Err(Decline::UnsupportedWordWidth);
    }

    // A boundary at or below the current program counter would exclude the instruction
    // the block was asked to start at, leaving nothing to run and declining forever. The
    // machine refuses to set such a boundary; this is the translator not trusting that.
    if boundary.is_some_and(|at| at <= processor.architectural().pc()) {
        return Err(Decline::NotTranslatable);
    }

    let mut pc = processor.architectural().pc();
    let start = pc.as_u64();
    let mut block = Translated {
        start,
        ..Translated::default()
    };
    let mut code = Code::new();

    // **Two passes, and the second one is why.** The block has to spill the operands and
    // result of its *last* flag-setting instruction, and whether an instruction is the
    // last one is not known until the block has been scanned to its end. Emitting as it
    // scanned would mean either spilling for every arithmetic instruction — correct, since
    // only the last write survives, but dishonest about the cost — or guessing. So the
    // instructions are decoded first, the last flag-setting one is identified, and then
    // they are all emitted with the spill placed exactly once.
    //
    // **No prologue.** `RDI` is the register block and `RSI` the PC, both already in place
    // from the C ABI; see the module documentation for why emitting a `MOV` here instead
    // was a bug that produced wrong answers rather than a fault.
    let mut instructions: Vec<Instruction> = Vec::new();
    for _ in 0..MAX_BLOCK {
        // **The boundary ends the block *before* the instruction it names.** Checked at
        // the top of the loop, before the instruction is fetched — checking after it was
        // appended let one instruction too many into the block, and the block then ran
        // through the breakpoint the debugger had set. That is precisely the bug a
        // boundary exists to prevent, committed in the boundary's own code, and it is why
        // the position of this line and not just its presence is load-bearing.
        if boundary == Some(pc) {
            break;
        }
        let Ok(bytes) = memory.fetch_instruction(config, pc, processor.privilege()) else {
            break;
        };
        let Ok(instruction) = decode(config, &bytes) else {
            break;
        };
        if !is_translatable(&instruction) {
            break;
        }
        instructions.push(instruction);
        pc = lazalith_types::InstructionAddress::new(pc.as_u64() + crate::INSTRUCTION_WIDTH);
    }

    if instructions.is_empty() {
        return Err(Decline::NotTranslatable);
    }

    // The index of the last instruction that set the flags, if any.
    let spilling = instructions.iter().rposition(|instruction| {
        matches!(
            instruction.opcode(),
            lazalith_isa::Opcode::Add
                | lazalith_isa::Opcode::Sub
                | lazalith_isa::Opcode::Addi
                | lazalith_isa::Opcode::Subi
                | lazalith_isa::Opcode::Mul
                | lazalith_isa::Opcode::And
                | lazalith_isa::Opcode::Or
                | lazalith_isa::Opcode::Xor
        )
    });

    for (index, instruction) in instructions.iter().enumerate() {
        emit(&mut code, instruction, Some(index) == spilling)?;
        block.instructions = block.instructions.saturating_add(1);
        block.last_opcode = Some(instruction.opcode());
        // `saturating_add` is a floor, not a ceiling, and `MAX_BLOCK_CYCLES` proves the
        // ceiling is never reached — so a saturation here would be a bug, not a guard.
        block.cycles = block.cycles.saturating_add(instruction.opcode().cycles());
        if Some(index) == spilling {
            block.flags = flags_of(instruction);
        }
        code.add_imm8_at(PC, crate::INSTRUCTION_WIDTH as i8);
    }

    code.ret();
    block.end = pc.as_u64();
    block.code = code;
    Ok(block)
}

/// Whether `instruction` sets the status flags, and from which operands.
///
/// **`None` for `Li` and `Mov`, and that is a fact about the ISA rather than an omission.**
/// Those two write a register directly and never go through the interpreter's arithmetic
/// path, so they leave N, Z, C and V exactly as they were — a block of `LI`s leaves the
/// status register untouched, and one that ends in a `MOV` after an `ADD` keeps the
/// `ADD`'s flags.
fn flags_of(instruction: &Instruction) -> Option<Flags> {
    let operands = instruction.operands();
    let operand = |index: usize| -> Option<FlagsOperand> {
        match operands.get(index) {
            Some(Operand::Register(register)) => Some(FlagsOperand::Register(register.as_u8())),
            Some(Operand::Immediate(value)) => Some(FlagsOperand::Immediate(*value)),
            _ => None,
        }
    };
    let opcode = instruction.opcode();
    // `Add` and `Sub` read two registers; `Addi` and `Subi` read one and an immediate;
    // the rest read two registers. Any other shape is not something this translator emits,
    // so declining to describe it is the honest answer.
    let (left, right) = match opcode {
        lazalith_isa::Opcode::Addi | lazalith_isa::Opcode::Subi => (operand(1)?, operand(2)?),
        lazalith_isa::Opcode::Add
        | lazalith_isa::Opcode::Sub
        | lazalith_isa::Opcode::Mul
        | lazalith_isa::Opcode::And
        | lazalith_isa::Opcode::Or
        | lazalith_isa::Opcode::Xor => (operand(1)?, operand(2)?),
        _ => return None,
    };
    Some(Flags {
        opcode,
        left,
        right,
    })
}

/// The host register holding the guest register block.
///
/// **`RDI`, and not a scratch register.** It is where the C ABI puts the block pointer,
/// so using it needs no prologue, and — unlike `RAX` — nothing in the emitted code is
/// required to preserve it. That is what makes the two-operand multiply possible.
const BASE: Reg = Reg::Rdi;

/// The host register holding the guest PC. Written through, never used as scratch.
const PC: Reg = Reg::Rsi;

/// The host register holding the flags scratch: the third C argument.
const FLAGS: Reg = Reg::Rdx;

/// The scratch register an instruction's destination is computed in.
///
/// **`R10`, because the three obvious choices are all taken.** `RCX` is where the C ABI
/// puts the third argument, `RDX` the flags scratch, and `RSI`/`RDI` the PC and the
/// register block. `R10` and `R11` are the remaining registers with no meaning the ABI
/// imposes, which is exactly what a scratch register has to be.
const ACCUMULATOR: Reg = Reg::R10;

/// The scratch register a second operand is loaded into.
const OPERAND: Reg = Reg::R11;

/// Emits one guest instruction.
///
/// **The one place a guest operation becomes host instructions**, so a divergence
/// between LZA semantics and what the host computes is a divergence here and nowhere
/// else. Every emitter arm is checked against the Reference Interpreter by
/// `crates/lazalith-jit/tests/jit.rs`, which is why they can be written without a second
/// reference implementation to compare against in this file.
///
/// # `d = a OP b`, and the destination is not an input
///
/// **Every three-operand LZA instruction names its left source explicitly**, in operand
/// one, and the destination in operand zero is *written and never read*. That is not the
/// shape of most ISAs, and getting it wrong is silent: the first version of this function
/// loaded the accumulator from the destination and treated the two sources as the
/// destination and operand one, so `XOR r3, r1, r2` computed `r3 ^ r2` and stored it in
/// `r3`. Because `r3` was zero beforehand, that read as `0 ^ 22 = 22` rather than
/// `20 ^ 22 = 2` — a wrong answer that looked like a plausible register value, and was
/// only caught because the differential test compares against the interpreter rather than
/// against a hand-written expectation.
///
/// # The flags
///
/// **A flag-setting instruction also spills its operands and its result** when
/// `spill` is set, so the caller can derive N, Z, C and V from exactly the values the
/// guest defined them from. Spilling *before* the operation for the operands and *after*
/// it for the result is what makes the destination-overlapping-a-source case work: for
/// `ADD r0, r0, r1` the left operand has to be the value `r0` held beforehand, which
/// reading `r0` afterwards would not give.
fn emit(code: &mut Code, instruction: &Instruction, spill: bool) -> Result<(), Decline> {
    let operands = instruction.operands();
    let destination = match operands.first() {
        Some(Operand::Register(index)) => index.as_u8(),
        _ => return Err(Decline::NotTranslatable),
    };
    let store = |code: &mut Code| code.store_at(BASE, ACCUMULATOR, register_offset(destination));
    // Spill the two operands, run the operation, then spill the result.
    let capture = |code: &mut Code| {
        if spill {
            code.store_at(FLAGS, ACCUMULATOR, 0);
            code.store_at(FLAGS, OPERAND, 8);
        }
    };
    let capture_result = |code: &mut Code| {
        if spill {
            code.store_at(FLAGS, ACCUMULATOR, 16);
        }
    };

    match instruction.opcode() {
        lazalith_isa::Opcode::Li => {
            let value = match operands.get(1) {
                Some(Operand::Immediate(value)) => *value,
                _ => return Err(Decline::NotTranslatable),
            };
            // `MOV R10, imm64` with the immediate already sign-extended, because the
            // guest's `LI` is a *sign-extended word* immediate (the ISA's `SignedWord`
            // meaning). A 32-bit move that zero-extends, or a 64-bit move of the raw
            // zero-extended value, would be wrong for a negative immediate.
            code.mov_imm64(ACCUMULATOR, value as i64 as u64);
            store(code);
        }
        lazalith_isa::Opcode::Mov => {
            // `d = a`: a plain move, which is the cheapest arm here and worth having
            // because register shuffling is common at the start of a straight-line run.
            let source = match operands.get(1) {
                Some(Operand::Register(index)) => index.as_u8(),
                _ => return Err(Decline::NotTranslatable),
            };
            code.load_at(ACCUMULATOR, BASE, register_offset(source));
            store(code);
        }
        lazalith_isa::Opcode::Add
        | lazalith_isa::Opcode::Sub
        | lazalith_isa::Opcode::And
        | lazalith_isa::Opcode::Or
        | lazalith_isa::Opcode::Xor => {
            // The left source, *not* the destination.
            let (left, right) = match (operands.get(1), operands.get(2)) {
                (Some(Operand::Register(left)), Some(Operand::Register(right))) => {
                    (left.as_u8(), right.as_u8())
                }
                _ => return Err(Decline::NotTranslatable),
            };
            code.load_at(ACCUMULATOR, BASE, register_offset(left));
            code.load_at(OPERAND, BASE, register_offset(right));
            capture(code);
            match instruction.opcode() {
                lazalith_isa::Opcode::Add => code.add_reg(ACCUMULATOR, OPERAND),
                lazalith_isa::Opcode::Sub => code.sub_reg(ACCUMULATOR, OPERAND),
                lazalith_isa::Opcode::And => code.and_reg(ACCUMULATOR, OPERAND),
                lazalith_isa::Opcode::Or => code.or_reg(ACCUMULATOR, OPERAND),
                lazalith_isa::Opcode::Xor => code.xor_reg(ACCUMULATOR, OPERAND),
                _ => return Err(Decline::NotTranslatable),
            }
            capture_result(code);
            store(code);
        }
        lazalith_isa::Opcode::Addi | lazalith_isa::Opcode::Subi => {
            // `d = a +i`, where the immediate is a 32-bit sign-extended word — which is
            // exactly what the `81 /n id` form sign-extends to a full 64-bit operand, so
            // no separate widening instruction is needed.
            let left = match operands.get(1) {
                Some(Operand::Register(index)) => index.as_u8(),
                _ => return Err(Decline::NotTranslatable),
            };
            let immediate = match operands.get(2) {
                Some(Operand::Immediate(value)) => *value,
                _ => return Err(Decline::NotTranslatable),
            };
            code.load_at(ACCUMULATOR, BASE, register_offset(left));
            // The immediate is also spilled, so the caller derives the flags from the
            // same sign-extended value the guest would have used.
            code.mov_imm64(OPERAND, immediate as i64 as u64);
            capture(code);
            if instruction.opcode() == lazalith_isa::Opcode::Addi {
                code.add_imm32(ACCUMULATOR, immediate);
            } else {
                code.sub_imm32(ACCUMULATOR, immediate);
            }
            capture_result(code);
            store(code);
        }
        lazalith_isa::Opcode::Mul => {
            let (left, right) = match (operands.get(1), operands.get(2)) {
                (Some(Operand::Register(left)), Some(Operand::Register(right))) => {
                    (left.as_u8(), right.as_u8())
                }
                _ => return Err(Decline::NotTranslatable),
            };
            // **The two-operand `IMUL`, and this arm is why.** The x86 multiply that
            // produces a full 128-bit product reads and writes `RAX` and `RDX`; using it
            // would mean the guest's own multiply clobbered whichever of those held the
            // register block pointer, so every register access after the first `MUL` in a
            // block would read from the wrong address. `0F AF /r` — the two-operand form —
            // multiplies two arbitrary registers and keeps the low half, which is exactly
            // the ISA's wrapping 64-bit multiply, and touches neither `RAX` nor `RDX`.
            code.load_at(ACCUMULATOR, BASE, register_offset(left));
            code.load_at(OPERAND, BASE, register_offset(right));
            capture(code);
            code.imul(ACCUMULATOR, OPERAND);
            capture_result(code);
            store(code);
        }
        _ => return Err(Decline::NotTranslatable),
    }
    Ok(())
}

/// A human-readable summary of why an instruction was not translated.
///
/// **A sentence, because a decline is a normal outcome and a caller debugging a slow
/// program needs to know which instruction is stopping translation.**
pub fn reason(instruction: &Instruction) -> String {
    match translatable(instruction) {
        Translatable::Yes => String::new(),
        Translatable::No(why) => format!(
            "{} ({}) is not translated: {why}",
            instruction.definition().mnemonic,
            instruction.opcode().as_u8()
        ),
    }
}
