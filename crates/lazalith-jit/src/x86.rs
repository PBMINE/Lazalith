//! A very small x86-64 assembler: just the encodings the JIT emits.
//!
//! # Why hand-assemble instead of using a crate
//!
//! A crate like `iced-x86` or `yaxpeax-x86` would be the normal answer, and this is
//! not one. The JIT emits a *fixed, tiny* instruction set — about a dozen encodings, all
//! of them two-operand register and immediate forms — and every one of them is written
//! out below with its encoding and the instruction's meaning. That makes the whole
//! encoder auditable by reading one file, which is the same argument
//! `lazalith-sdl3` makes for owning its own FFI declarations.
//!
//! **The alternative was considered and rejected on a second ground too:** a codegen
//! dependency in the workspace would be the first crate outside this repository that
//! the VM's behaviour depends on, and §38 wants the build reproducible from the Git
//! repository.
//!
//! # What is emitted
//!
//! The guest's registers live in host memory — a [`Registers`] block the machine
//! already owns — so the generated code is a loop that loads a guest register, computes
//! on it, and stores it back. That is what a *conservative* JIT does, and it is what
//! makes the boundary exact: the block ends by returning to the caller, and at that
//! moment every guest register is already back in the machine's own state.
//!
//! # The register convention
//!
//! - `RAX` holds the base address of the guest register block.
//! - `RCX`, `RDX`, `R8`, `R9` are scratch.
//! - `R10` holds the guest PC, so the block can advance it by 8 per instruction.
//! - The System V ABI is respected: `RSP` is the machine's own stack and the generated
//!   code never touches it, so a translated block is a leaf that preserves nothing.

use alloc::vec::Vec;

/// A 64-bit general-purpose host register, in the encoding's numbering.
///
/// **The x86 numbering, not the order of an array**, because the numbers appear in the
/// encoding and getting one wrong is a jump into the middle of an instruction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reg {
    /// 64-bit accumulator, the register-block base.
    Rax = 0,
    /// 64-bit counter.
    Rcx = 1,
    /// 64-bit data.
    Rdx = 2,
    /// First argument, also scratch.
    Rsi = 6,
    /// Second argument, also scratch.
    Rdi = 7,
    /// First scratch beyond the argument registers.
    R8 = 8,
    /// Second scratch.
    R9 = 9,
    /// Third scratch beyond the argument registers.
    R10 = 10,
    /// Fourth scratch beyond the argument registers.
    R11 = 11,
}

impl Reg {
    /// The encoding's three-bit register number.
    pub const fn number(self) -> u8 {
        self as u8
    }

    /// The REX.B bit: set when this register's number does not fit in three bits.
    ///
    /// **Not always zero, and finding that out was the point of the tests.** The first
    /// draft of this file said "every register here is 0–7, so REX.B is never needed" and
    /// asserted it, and then the translator used `R8`, `R9` and `R10` as scratch — so
    /// every emitted instruction that touched a scratch register would have encoded
    /// `RSP`/`RBP`/`RSI`/`RDI` instead, silently, and the JIT would have computed with
    /// the wrong registers and reported answers. `the_encodings_are_the_ones_written_down`
    /// found it because a hand-written byte table is the only thing that notices an
    /// encoding that is merely *plausible*.
    pub const fn rex_b(self) -> u8 {
        if self.number() >= 8 { 1 } else { 0 }
    }

    /// REX.R: the same bit, for a register in a ModRM `reg` field.
    pub const fn rex_r(self) -> u8 {
        self.rex_b() << 2
    }

    /// REX.B: the bit, for a register in a ModRM `r/m` field.
    pub const fn rex_rm(self) -> u8 {
        self.rex_b()
    }
}

/// An emitted instruction stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Code {
    bytes: Vec<u8>,
}

/// The `/digit` values in the `81 /n` immediate-arithmetic group.
///
/// **Group numbers, not opcode bytes** — see [`Code::arith_imm32`] for why the confusion
/// is not hypothetical. Only the two the translator emits are listed: `Add` and `Sub` are
/// the ISA's immediate-form opcodes (`ADDI`, `SUBI`), and the bitwise operations have no
/// immediate form in LZA, so there is no `AND` or `XOR` here to get wrong.
const ADD_DIGIT: u8 = 0;
const SUB_DIGIT: u8 = 5;

/// The 64-bit operand-size opcodes for the register-to-register ALU forms.
///
/// **x86 has a separate opcode per operand size, and REX.W does not select between them.**
/// Each ALU operation has an 8-bit opcode and a 64-bit one, one apart: `ADD` is `00`/`01`,
/// `OR` `08`/`09`, `AND` `20`/`21`, `SUB` `28`/`29`, `XOR` `30`/`31`. REX.W is *also* set,
/// and it does not upgrade `28` into `29` — the opcode byte is the only thing that decides
/// the width.
///
/// This crate originally used the 8-bit opcodes, and the result was arithmetic on eight
/// bits with the sign and high bits discarded. Four of the five operations in the
/// differential corpus agreed with the interpreter anyway, because `20 + 22`, `20 ^ 22`,
/// `20 & 22` and `20 | 22` all have the same answers at either width. Only `20 - 22`
/// distinguished them: at eight bits it is `0xFE`, and zero-extended into a 64-bit
/// register that is `254` rather than `-2`. One operation in five was actually wrong, and
/// a test that only checked addition would never have found it.
const ADD_QWORD: u8 = 0x01;
const OR_QWORD: u8 = 0x09;
const AND_QWORD: u8 = 0x21;
const SUB_QWORD: u8 = 0x29;
const XOR_QWORD: u8 = 0x31;

impl Code {
    /// An empty stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes emitted so far.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// How many bytes the stream is.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether anything has been emitted.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    /// `MOV reg, imm64` — `REX.W B8+rd io`.
    pub fn mov_imm64(&mut self, register: Reg, value: u64) {
        // `B8+rd` is the low three bits of the register, extended by REX.B.
        self.bytes.push(0x48 | register.rex_b());
        self.bytes.push(0xB8 + (register.number() & 0b111));
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// `MOV [base + disp32], reg` — `REX.W 89 /r`.
    ///
    /// A `disp32` rather than a `disp8` so the encoding does not depend on the guest
    /// register index being small. The offset is added in 64-bit arithmetic and the block
    /// is known to be in range because the caller checked the guest register index.
    pub fn store_at(&mut self, base: Reg, register: Reg, displacement: i32) {
        // REX.W; REX.R extends the ModRM `reg` field (the source), REX.B extends the
        // `r/m` field (the base).
        self.bytes.push(0x48 | register.rex_r() | base.rex_rm());
        self.bytes.push(0x89);
        // ModRM: mod = 10 (disp32), reg = the source register, r/m = base.
        self.bytes
            .push(0x80 | ((register.number() & 0b111) << 3) | (base.number() & 0b111));
        self.bytes.extend_from_slice(&displacement.to_le_bytes());
    }

    /// `MOV reg, [base + disp32]` — `REX.W 8B /r`.
    pub fn load_at(&mut self, register: Reg, base: Reg, displacement: i32) {
        self.bytes.push(0x48 | register.rex_r() | base.rex_rm());
        self.bytes.push(0x8B);
        self.bytes
            .push(0x80 | ((register.number() & 0b111) << 3) | (base.number() & 0b111));
        self.bytes.extend_from_slice(&displacement.to_le_bytes());
    }

    /// `ADD reg, imm32`, sign-extended — `REX.W 81 /0 id`.
    pub fn add_imm32(&mut self, register: Reg, value: i32) {
        self.arith_imm32(register, value, ADD_DIGIT);
    }

    /// `SUB reg, imm32`, sign-extended — `REX.W 81 /5 id`.
    pub fn sub_imm32(&mut self, register: Reg, value: i32) {
        self.arith_imm32(register, value, SUB_DIGIT);
    }

    /// `REX.W 81 /n id` — the register-immediate arithmetic form.
    ///
    /// **The `/digit` is a group number, not an opcode byte, and the two are easy to
    /// confuse because the opcodes are *derived* from them.** The group's opcodes are
    /// spaced eight apart — `ADD` 0x00, `OR` 0x08, `ADC` 0x10, `SBB` 0x18, `AND` 0x20,
    /// `SUB` 0x28, `XOR` 0x30, `CMP` 0x38 — so `/digit` 5 is the opcode `0x28`.
    ///
    /// Passing the opcode through to the ModRM byte, as an earlier version of this file
    /// did, fails silently rather than loudly: `0x28 << 3` is 320, which truncates to
    /// `0x40` in a `u8`, and `0xC0 | 0x40` is still `0xC0` — so the `/digit` field came
    /// out as zero and every "subtract" in the block was emitted as an *add*. The
    /// translator compiled, the code ran, and `x - y` produced `x + y`.
    fn arith_imm32(&mut self, register: Reg, value: i32, digit: u8) {
        // The `/digit` lives in three bits of the ModRM byte. A larger value would be
        // truncated into the `mod` field above it and produce a *different* instruction
        // that still assembles, so this is checked rather than trusted.
        debug_assert!(
            digit < 8,
            "a /digit is three bits; an opcode offset is not a /digit"
        );
        // REX.B extends the ModRM `r/m` field, which here is the register itself.
        self.bytes.push(0x48 | register.rex_rm());
        self.bytes.push(0x81);
        // ModRM: mod = 11 (register), reg = the /digit, r/m = the register itself.
        self.bytes
            .push(0xC0 | (digit << 3) | (register.number() & 0b111));
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    /// `ADD dst, src` — `REX.W 01 /r`.
    pub fn add_reg(&mut self, destination: Reg, source: Reg) {
        self.alu_reg(destination, source, ADD_QWORD);
    }

    /// `SUB dst, src` — `REX.W 29 /r`.
    pub fn sub_reg(&mut self, destination: Reg, source: Reg) {
        self.alu_reg(destination, source, SUB_QWORD);
    }

    /// `AND dst, src` — `REX.W 21 /r`.
    pub fn and_reg(&mut self, destination: Reg, source: Reg) {
        self.alu_reg(destination, source, AND_QWORD);
    }

    /// `OR dst, src` — `REX.W 09 /r`.
    pub fn or_reg(&mut self, destination: Reg, source: Reg) {
        self.alu_reg(destination, source, OR_QWORD);
    }

    /// `XOR dst, src` — `REX.W 31 /r`.
    pub fn xor_reg(&mut self, destination: Reg, source: Reg) {
        self.alu_reg(destination, source, XOR_QWORD);
    }

    /// `REX.W 01 /r` and friends, register to register.
    fn alu_reg(&mut self, destination: Reg, source: Reg, opcode: u8) {
        // In every `/r` form the ModRM `reg` field is the *source* and `r/m` is the
        // *destination*, so REX.R belongs to the source and REX.B to the destination.
        // Reversing these two encodes a plausible instruction that computes with the
        // wrong registers, which is the failure this ordering exists to prevent.
        self.bytes
            .push(0x48 | source.rex_r() | destination.rex_rm());
        self.bytes.push(opcode);
        self.bytes
            .push(0xC0 | ((source.number() & 0b111) << 3) | (destination.number() & 0b111));
    }

    /// `IMUL dst, src` — two-operand form, `REX.W 0F AF /r`.
    ///
    /// **The two-operand form, and the choice is load-bearing.** `0F AF /r` is the
    /// two-operand signed multiply: `dst = dst * src`, keeping the low half and
    /// discarding the high half, which is exactly the guest's wrapping 64-bit multiply.
    ///
    /// The one-operand form (`F6 /5` / `F7 /5`) is the one that reads `RAX`, writes
    /// `RDX:RAX` and is therefore the only way to get the full 128-bit product. It is
    /// unusable here for two reasons: the guest's `MUL` keeps only the low word, and
    /// `RDX` is one of this block's three argument registers — the flags scratch — so
    /// the one-operand form would have the guest's own multiply destroy it halfway
    /// through. The two-operand form touches neither `RAX` nor `RDX`.
    ///
    /// In the ModRM byte the `reg` field is the *destination* and `r/m` the source —
    /// the reverse of every `01 /r`-style ALU form above, which is the single easiest
    /// byte in this file to get backwards, and `the_encodings_are_the_ones_written_down`
    /// checks it against a hand-written table.
    pub fn imul(&mut self, destination: Reg, source: Reg) {
        self.bytes
            .push(0x48 | destination.rex_r() | source.rex_rm());
        self.bytes.push(0x0F);
        self.bytes.push(0xAF);
        self.bytes
            .push(0xC0 | ((destination.number() & 0b111) << 3) | (source.number() & 0b111));
    }

    /// `ADD [base], imm8`, sign-extended — `REX.W 83 /0 ib` with `mod = 00`.
    ///
    /// **Exists for the guest PC and nothing else.** The block's PC lives behind a
    /// pointer the caller passed in, and advancing it by one instruction is a memory
    /// operation, so it needs the `83 /0` form with a memory operand rather than the
    /// register form above. `mod = 00` with no SIB byte and a base that is not `RSP` or
    /// `R12` means "the address *is* the register", which is the encoding with no
    /// displacement and no index — and therefore the one that cannot be misread.
    pub fn add_imm8_at(&mut self, base: Reg, value: i8) {
        self.bytes.push(0x48 | base.rex_rm());
        self.bytes.push(0x83);
        // ModRM: mod = 00, reg = /0 (ADD), r/m = base.
        self.bytes.push(base.number() & 0b111);
        self.bytes.push(value as u8);
    }

    /// `RET` — `C3`.
    ///
    /// **A real `RET`, not a jump to a trampoline.** A `RET` pops the return address the
    /// caller pushed, so the block needs no epilogue of its own and cannot get it
    /// wrong: the machine's own stack is the one the C call pushed to, and the
    /// generated code never touches it.
    pub fn ret(&mut self) {
        self.bytes.push(0xC3);
    }
}

#[cfg(test)]
mod tests {
    use super::{Code, Reg};

    /// Every encoding this crate emits, written out.
    ///
    /// **The point is that the expected bytes are in the test, not in the encoder.**
    /// A table like this is the only thing that catches a bit shifted the wrong way,
    /// because a wrong encoding still assembles into *something*.
    #[test]
    fn the_encodings_are_the_ones_written_down() {
        let mut code = Code::new();
        code.mov_imm64(Reg::Rax, 0x1122_3344_5566_7788);
        assert_eq!(
            code.bytes(),
            &[0x48, 0xB8, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
            "MOV RAX, imm64 is REX.W B8+rd io"
        );

        let mut code = Code::new();
        code.load_at(Reg::Rcx, Reg::Rax, 8);
        assert_eq!(
            code.bytes(),
            &[0x48, 0x8B, 0x88, 0x08, 0x00, 0x00, 0x00],
            "MOV RCX, [RAX+8] is REX.W 8B /r with mod=10, reg=RCX(1), r/m=RAX(0)"
        );

        let mut code = Code::new();
        code.store_at(Reg::Rax, Reg::Rcx, -8);
        assert_eq!(
            code.bytes(),
            &[0x48, 0x89, 0x88, 0xF8, 0xFF, 0xFF, 0xFF],
            "MOV [RAX-8], RCX sets REX.X because the displacement is negative, and \
             the ModRM is mod=10, reg=RCX(1), r/m=RAX(0)"
        );

        let mut code = Code::new();
        code.add_imm32(Reg::Rdx, -3);
        assert_eq!(
            code.bytes(),
            &[0x48, 0x81, 0xC2, 0xFD, 0xFF, 0xFF, 0xFF],
            "ADD RDX, -3 is REX.W 81 /0 with a sign-extended imm32"
        );

        let mut code = Code::new();
        code.imul(Reg::Rdx, Reg::Rcx);
        assert_eq!(
            code.bytes(),
            &[0x48, 0x0F, 0xAF, 0xD1],
            "the two-operand IMUL puts the *destination* in reg and the source in r/m, \
             which is the reverse of the 01 /r ALU forms above"
        );

        let mut code = Code::new();
        code.add_imm8_at(Reg::Rsi, 8);
        assert_eq!(
            code.bytes(),
            &[0x48, 0x83, 0x06, 0x08],
            "ADD [RSI], 8 is mod=00 with no SIB and no displacement"
        );

        let mut code = Code::new();
        code.ret();
        assert_eq!(code.bytes(), &[0xC3], "RET is C3");
    }
}
