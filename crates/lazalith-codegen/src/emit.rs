//! Per-function machine code emission.
//!
//! Every value lives in the frame, so an instruction is a load, an operation, and
//! a store. The register use is fixed and small, because the convention
//! `docs/lz64.md` already documents leaves `r0`–`r7` caller-saved:
//!
//! | Register | Use |
//! | --- | --- |
//! | `r0`–`r3` | the first four argument words, and a one-word return |
//! | `r4` | the result of the operation being emitted |
//! | `r5`, `r6` | operand words |
//! | `r7` | a frame or computed address, and the ABI's reserved syscall register |
//!
//! No callee-saved register is touched, so the prologue and epilogue are three
//! instructions each and nothing has to be spilled around a call.
//!
//! # Labels, not block numbers
//!
//! A branch carries a displacement, and the linker fills it in from a
//! *relocation against a symbol*. So this stage never needs a target block's
//! machine address to write a branch: it names the target's label and lets the
//! linker resolve it. That is why a loop's `continue` and `break` are label
//! names and not block numbers — a loop whose body contains an `if` creates
//! blocks the loop itself could not have predicted, and a name does not care.
//!
//! # Why a comparison takes three blocks
//!
//! The ISA has no "set condition" instruction: `CMP` writes NZCV and `BR` reads
//! it. A comparison whose result is a *value* therefore cannot be one
//! instruction, and the only correct lowering is a branch over a constant:
//!
//! ```text
//! CMP left, right
//! BR <predicate>, L                return Ok(lazalith_ir::argument_words(&ty));true, L_false
//! L_true:  LI r4, 1 ; ST [slot]
//! BR L_join
//! L_false: LI r4, 0 ; ST [slot]
//! L_join:
//! ```
//!
//! `docs/isa.md` is explicit that `BR` "does not remember which instruction last
//! wrote flags", so the value has to be stored rather than left in the flags.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazalith_ir::{
    BinaryOp, BlockId, CallArg, CallTarget, ComparisonOp, ConstValue, DataSegment, Function,
    Instruction, Intrinsic, Linkage, MemorySpace, Name, ReturnValue, Terminator, Type as IrType,
    UnaryOp, ValueId,
};
use lazalith_isa::{Condition, DataSize, Opcode, Operand};
use lazalith_toolchain::RelocationKind;
use lazalith_types::{ArchitectureConfig, RegisterIndex};

use crate::layout::{FunctionLayout, OUTGOING_ARGUMENT_BYTES};
use crate::{
    BRANCH_RELOCATION, CodegenError, DATA_RELOCATION, PendingRelocation, function_symbol,
    value_size,
};

/// The address scratch, and the register the OS ABI reserves at a syscall.
const ADDRESS: u8 = 7;
/// The first operand scratch.
const OPERAND_A: u8 = 6;
/// The second operand scratch.
const OPERAND_B: u8 = 5;
/// Where a result is computed.
const RESULT: u8 = 4;
/// The most argument words a call may pass, from the ABI's `r1`–`r6`.
const MAX_ARGUMENT_WORDS: usize = 6;
/// The register a one-word return value is in.
const RETURN_REGISTER: u8 = 0;

fn register(index: u8) -> RegisterIndex {
    RegisterIndex::try_from(index).expect("register index in range")
}

/// The data size for a byte count the machine can access.
fn data_size(bytes: u32) -> Option<DataSize> {
    Some(match bytes {
        1 => DataSize::Byte,
        2 => DataSize::Half,
        4 => DataSize::Word,
        8 => DataSize::Double,
        _ => return None,
    })
}

/// The machine opcode for an arithmetic operator.
fn arithmetic(op: BinaryOp) -> Opcode {
    match op {
        BinaryOp::Add => Opcode::Add,
        BinaryOp::Sub => Opcode::Sub,
        BinaryOp::Mul => Opcode::Mul,
        BinaryOp::DivUnsigned => Opcode::Divu,
        BinaryOp::DivSigned => Opcode::Divs,
        BinaryOp::RemainderUnsigned => Opcode::Remu,
        BinaryOp::RemainderSigned => Opcode::Rems,
        BinaryOp::BitAnd => Opcode::And,
        BinaryOp::BitOr => Opcode::Or,
        BinaryOp::BitXor => Opcode::Xor,
        BinaryOp::ShiftLeft => Opcode::Shl,
        // A logical right shift is the machine's `SHR` and an arithmetic one is
        // its `SAR`; the IR keeps them apart, so they must not be merged.
        BinaryOp::ShiftRightLogical => Opcode::Shr,
        BinaryOp::ShiftRightArithmetic => Opcode::Sar,
    }
}

/// The branch predicate for a comparison.
///
/// The ISA defines these from `CMP`'s flags: `ULT` is the carry, meaning a
/// borrow, so an unsigned less-than is exactly "a borrow happened".
fn comparison(op: ComparisonOp) -> Condition {
    match op {
        ComparisonOp::Equal => Condition::Eq,
        ComparisonOp::NotEqual => Condition::Ne,
        ComparisonOp::LessThanUnsigned => Condition::Ult,
        ComparisonOp::LessThanOrEqualUnsigned => Condition::Ule,
        ComparisonOp::GreaterThanUnsigned => Condition::Ugt,
        ComparisonOp::GreaterThanOrEqualUnsigned => Condition::Uge,
        ComparisonOp::LessThanSigned => Condition::Slt,
        ComparisonOp::LessThanOrEqualSigned => Condition::Sle,
        ComparisonOp::GreaterThanSigned => Condition::Sgt,
        ComparisonOp::GreaterThanOrEqualSigned => Condition::Sge,
    }
}

/// Emits one function.
pub struct FunctionEmitter<'a> {
    function: &'a Function,
    segments: &'a [DataSegment],
    layout: &'a FunctionLayout,
    architecture: ArchitectureConfig,
    code: &'a mut Vec<u8>,
    text_labels: &'a mut Vec<(Name, u64)>,
    relocations: &'a mut Vec<PendingRelocation>,
    /// A counter for machine-only label names.
    next_label: u32,
    /// The next value identifier this function's instructions will define.
    ///
    /// An IR instruction does not name its own result: a function's values are
    /// dense and numbered in definition order, parameters first and then every
    /// value-producing instruction in block and instruction order. That is the
    /// same order `FunctionLayout` laid the slots out in and the same order
    /// `lazalith_ir::value_type` reads them back in, so all three agree by
    /// construction rather than by a shared list.
    next_value: u32,
    name: String,
    /// Whether the current block already has its terminator.
    ///
    /// A body that ends in `return`, `break` or `continue` leaves its block
    /// closed, and the code that follows must not add a jump to it. The IR
    /// builder would reject that, but it has no way to ask, so this stage keeps
    /// the answer.
    terminated: bool,
}

impl<'a> FunctionEmitter<'a> {
    /// Prepares to emit `function`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        function: &'a Function,
        segments: &'a [DataSegment],
        layout: &'a FunctionLayout,
        architecture: &ArchitectureConfig,
        code: &'a mut Vec<u8>,
        text_labels: &'a mut Vec<(Name, u64)>,
        relocations: &'a mut Vec<PendingRelocation>,
    ) -> Self {
        Self {
            name: function.name.clone(),
            function,
            segments,
            layout,
            architecture: *architecture,
            code,
            text_labels,
            relocations,
            next_label: 0,
            next_value: u32::try_from(function.params.len()).unwrap_or(u32::MAX),
            terminated: false,
        }
    }

    /// Emits the whole function and returns the offset its code starts at.
    ///
    /// The function's own symbol is created by the caller, which knows its
    /// linkage; only the offset is this stage's to report.
    pub fn run(&mut self) -> Result<u64, CodegenError> {
        let entry = self.code.len() as u64;
        // A declared extern has no body of its own: the symbol is defined
        // elsewhere. Its one block traps, so anything that reached it would fail
        // loudly instead of running whatever the linker placed next.
        if self.function.linkage == Linkage::External {
            self.trap(0)?;
            self.terminated = true;
            self.mark(&self.block_label(self.function.blocks[0].id));
            return Ok(entry);
        }
        self.prologue()?;
        for block in &self.function.blocks {
            self.terminated = false;
            self.mark(&self.block_label(block.id));
            for instruction in &block.instructions {
                self.instruction(instruction)?;
            }
            self.terminator(&block.terminator)?;
        }
        Ok(entry)
    }

    // -- labels --

    /// The label of a block in this function.
    fn block_label(&self, block: BlockId) -> String {
        format!("{}.b{}", self.name, block.get())
    }

    /// A machine-only label, for the branches a comparison needs.
    fn local_label(&mut self, stem: &str) -> String {
        let label = format!("{}.{}.{}", self.name, stem, self.next_label);
        self.next_label += 1;
        label
    }

    /// Records a label's position, so a relocation can name it.
    fn mark(&mut self, label: &str) {
        let offset = self.code.len() as u64;
        self.text_labels.push((String::from(label), offset));
    }

    // -- emitting --

    /// Appends one instruction, encoded by the ISA's own encoder.
    fn emit(&mut self, opcode: Opcode, operands: &[Operand]) -> Result<u64, CodegenError> {
        crate::emit_instruction(self.architecture, self.code, opcode, operands).map_err(|error| {
            CodegenError::UnsupportedInstruction {
                function: self.name.clone(),
                detail: format!("{opcode:?} ({error})"),
            }
        })
    }

    /// Records a relocation for the instruction at `offset`.
    fn relocate(
        &mut self,
        kind: RelocationKind,
        label: &str,
        offset: u64,
    ) -> Result<(), CodegenError> {
        self.relocations.push(PendingRelocation {
            label: String::from(label),
            kind,
            offset,
            addend: 0,
        });
        Ok(())
    }

    /// Branches to a label on the flags already set.
    fn branch_to(&mut self, condition: Condition, label: &str) -> Result<(), CodegenError> {
        let offset = self.emit(
            Opcode::Br,
            &[Operand::Condition(condition), Operand::Immediate(0)],
        )?;
        self.relocate(BRANCH_RELOCATION, label, offset)
    }

    /// An unconditional jump that does not close the current block.
    ///
    /// A block's own terminator ends it, and a jump over a trap or past a
    /// materialised value does not: execution carries on afterwards. Sharing one
    /// helper for the two would mean a jump that continues marking a block as
    /// finished, and then the real terminator after it would be dropped.
    fn branch_always(&mut self, label: &str) -> Result<(), CodegenError> {
        let offset = self.emit(
            Opcode::Br,
            &[Operand::Condition(Condition::Al), Operand::Immediate(0)],
        )?;
        self.relocate(BRANCH_RELOCATION, label, offset)
    }

    /// An unconditional jump.
    ///
    /// The ISA spells this as an always-taken `BR`: `JMP` takes a register
    /// rather than a displacement, and a displacement is what a relocation
    /// carries.
    fn jump_to(&mut self, label: &str) -> Result<(), CodegenError> {
        if self.terminated {
            return Ok(());
        }
        self.branch_always(label)?;
        self.terminated = true;
        Ok(())
    }

    /// A two-way branch on a bool value.
    fn branch_on(
        &mut self,
        condition: ValueId,
        then_label: &str,
        otherwise_label: &str,
    ) -> Result<(), CodegenError> {
        if self.terminated {
            return Ok(());
        }
        // A bool is already `0` or `1`, so one comparison against zero gives
        // both the taken and the untaken path.
        self.load_value(condition, OPERAND_A)?;
        self.li(OPERAND_B, 0)?;
        self.emit(
            Opcode::Cmp,
            &[
                Operand::Register(register(OPERAND_A)),
                Operand::Register(register(OPERAND_B)),
            ],
        )?;
        let then_offset = self.emit(
            Opcode::Br,
            &[Operand::Condition(Condition::Ne), Operand::Immediate(0)],
        )?;
        let otherwise_offset = self.emit(
            Opcode::Br,
            &[Operand::Condition(Condition::Al), Operand::Immediate(0)],
        )?;
        self.relocate(BRANCH_RELOCATION, then_label, then_offset)?;
        self.relocate(BRANCH_RELOCATION, otherwise_label, otherwise_offset)?;
        self.terminated = true;
        Ok(())
    }

    /// `LI rd, immediate`.
    fn li(&mut self, into: u8, value: i32) -> Result<(), CodegenError> {
        self.emit(
            Opcode::Li,
            &[Operand::Register(register(into)), Operand::Immediate(value)],
        )?;
        Ok(())
    }

    /// `GETSP rA; ADDI rA, rA, offset`, leaving a frame address in the scratch.
    ///
    /// The stack pointer is the *bottom* of the frame, and the first words above
    /// it are the outgoing argument words the convention reads at a caller's
    /// `[SP+0]` and `[SP+8]`. The frame's own storage starts above those, so the
    /// displacement adds the reserve before it adds the offset. Reading
    /// `base - offset` would land inside the frame for a small offset — mapped,
    /// and silently wrong — and outside the stack altogether for a large one.
    fn frame_address(&mut self, offset: u32) -> Result<(), CodegenError> {
        self.emit(Opcode::Getsp, &[Operand::Register(register(ADDRESS))])?;
        let step = i32::try_from(i64::from(offset) + i64::from(OUTGOING_ARGUMENT_BYTES)).map_err(
            |_| CodegenError::EncodingRange {
                function: self.name.clone(),
                detail: format!("the frame offset {offset}"),
            },
        )?;
        self.emit(
            Opcode::Addi,
            &[
                Operand::Register(register(ADDRESS)),
                Operand::Register(register(ADDRESS)),
                Operand::Immediate(step),
            ],
        )?;
        Ok(())
    }

    /// Loads the word at a frame offset into a register.
    fn load_frame(&mut self, offset: u32, into: u8) -> Result<(), CodegenError> {
        self.frame_address(offset)?;
        self.memory_at(ADDRESS, into, DataSize::Double)
    }

    /// Stores a register into a frame offset.
    fn put_frame(&mut self, offset: u32, from: u8) -> Result<(), CodegenError> {
        self.frame_address(offset)?;
        self.store_at(ADDRESS, from, DataSize::Double)
    }

    /// `LDZ rD, [base + 0]` at a stated width, zero-extending.
    ///
    /// The base is a parameter because a computed address cannot live in the
    /// address scratch: loading it there would overwrite the very frame address
    /// the load reads through, and the load would read whatever that scratch
    /// happened to hold.
    fn memory_at(&mut self, base: u8, into: u8, width: DataSize) -> Result<(), CodegenError> {
        self.emit(
            Opcode::Ldz,
            &[
                Operand::Register(register(into)),
                Operand::Memory {
                    base: register(base),
                    displacement: 0,
                },
                Operand::DataSize(width),
            ],
        )?;
        Ok(())
    }

    /// `LDS rD, [base + 0]` at a stated width, sign-extending.
    ///
    /// The ISA keeps the two apart, and so does the IR: a `Load` says whether it
    /// extends or it names `Byte` and `ByteSigned` as different things. Reading
    /// an `i8` of `-1` through `LDZ` would produce 255, which is a different
    /// number in a different type rather than the same number in another one.
    fn memory_signed_at(
        &mut self,
        base: u8,
        into: u8,
        width: DataSize,
    ) -> Result<(), CodegenError> {
        self.emit(
            Opcode::Lds,
            &[
                Operand::Register(register(into)),
                Operand::Memory {
                    base: register(base),
                    displacement: 0,
                },
                Operand::DataSize(width),
            ],
        )?;
        Ok(())
    }

    /// `ST [base + 0], rS` at a stated width.
    fn store_at(&mut self, base: u8, from: u8, width: DataSize) -> Result<(), CodegenError> {
        self.emit(
            Opcode::St,
            &[
                Operand::Register(register(from)),
                Operand::Memory {
                    base: register(base),
                    displacement: 0,
                },
                Operand::DataSize(width),
            ],
        )?;
        Ok(())
    }

    /// Loads a value's slot into a register, at the value's own width.
    ///
    /// The width and the extension both come from the IR's type, not from the
    /// word. Loading a `u32` as a word and computing on the 64-bit register would
    /// give 64-bit arithmetic for a 32-bit type, and the result would be a
    /// different number once the value overflowed: there is no masking
    /// instruction to fix it afterwards, so the load has to be narrow.
    fn load_value(&mut self, value: ValueId, into: u8) -> Result<(), CodegenError> {
        let ty = self.value_type(value)?;
        let (width, signed) = self.scalar_shape(&ty, "load")?;
        let offset = self.layout.offset(value)?;
        self.frame_address(offset)?;
        if signed {
            self.memory_signed_at(ADDRESS, into, width)
        } else {
            self.memory_at(ADDRESS, into, width)
        }
    }

    /// Stores a register into a value's slot, truncating to the value's width.
    ///
    /// The register holds a whole word and the slot may be narrower, and the ISA
    /// says a store truncates. That is the width rule in one place: the value is
    /// narrowed on the way in and widened again on the way out, so a 32-bit
    /// value's arithmetic is 32-bit arithmetic and no mask is ever needed.
    fn store_value(&mut self, value: ValueId, from: u8) -> Result<(), CodegenError> {
        let ty = self.value_type(value)?;
        let (width, _) = self.scalar_shape(&ty, "store")?;
        let offset = self.layout.offset(value)?;
        self.put_frame_width(offset, from, width)
    }

    /// The IR type of one of this function's values.
    fn value_type(&self, value: ValueId) -> Result<IrType, CodegenError> {
        lazalith_ir::value_type(self.function, value).ok_or_else(|| {
            CodegenError::UnsupportedValueType {
                function: self.name.clone(),
                detail: alloc::format!("a use of the undefined value {}", value.get()),
            }
        })
    }

    /// The access width and whether it extends, for a type this machine can hold
    /// in one register.
    ///
    /// A type wider than a word has no single-register shape: a view is two
    /// words and an aggregate is however many it is, and both are named word by
    /// word through [`FunctionLayout::word`]. Asking this for one of those is a
    /// backend bug rather than a program the program could have avoided, so it is
    /// reported rather than approximated.
    fn scalar_shape(&self, ty: &IrType, action: &str) -> Result<(DataSize, bool), CodegenError> {
        let signed = match ty {
            IrType::Int { signed, .. } => *signed,
            _ => false,
        };
        let bytes = value_size(ty).unwrap_or(0);
        let width = data_size(bytes).ok_or_else(|| CodegenError::UnsupportedValueType {
            function: self.name.clone(),
            detail: alloc::format!("a {action} of a {bytes}-byte {ty:?} value in one register"),
        })?;
        Ok((width, signed))
    }

    /// `GETSP rA; ADDI rA, rA, offset`, leaving a *stack* address in the scratch.
    ///
    /// This is the stack pointer itself, with no frame base added. The two are
    /// not interchangeable: the first `OUTGOING_ARGUMENT_BYTES` below the stack
    /// pointer are the words a callee reads its fifth and sixth arguments from,
    /// and they belong to no local. A frame address would land `OUTGOING` bytes
    /// higher, which is the caller's *first local* — so a caller with a five-word
    /// call wrote its last argument over its own variable.
    fn stack_address(&mut self, offset: u32) -> Result<(), CodegenError> {
        self.emit(Opcode::Getsp, &[Operand::Register(register(ADDRESS))])?;
        let step = i32::try_from(i64::from(offset)).map_err(|_| CodegenError::EncodingRange {
            function: self.name.clone(),
            detail: format!("the stack offset {offset}"),
        })?;
        self.emit(
            Opcode::Addi,
            &[
                Operand::Register(register(ADDRESS)),
                Operand::Register(register(ADDRESS)),
                Operand::Immediate(step),
            ],
        )?;
        Ok(())
    }

    /// Stores a register into a frame offset at a stated width.
    fn put_frame_width(
        &mut self,
        offset: u32,
        from: u8,
        width: DataSize,
    ) -> Result<(), CodegenError> {
        self.frame_address(offset)?;
        self.store_at(ADDRESS, from, width)
    }

    /// Copies a value's words into `destination`.
    ///
    /// A value wider than a word is a sequence of words, and a copy is all of
    /// them: a copied view that kept its address and dropped its length would be
    /// a view of the right bytes and the wrong size. Reading the length out of
    /// the source's own slot is what makes the copy complete.
    fn copy_words(&mut self, source: ValueId, destination: u32) -> Result<(), CodegenError> {
        let ty = self.value_type(source)?;
        let size = value_size(&ty).unwrap_or(0);
        let words = size.div_ceil(8);
        for word in 0..words {
            let from = self.layout.word(source, word)?;
            self.load_frame(from, OPERAND_A)?;
            self.put_frame(destination + word * 8, OPERAND_A)?;
        }
        Ok(())
    }

    /// The next value identifier, and the slot the layout gave it.
    fn define(&mut self) -> Result<ValueId, CodegenError> {
        let raw = self.next_value;
        self.next_value = self.next_value.saturating_add(1);
        let value = ValueId::new(raw).ok_or_else(|| CodegenError::UnsupportedValueType {
            function: self.name.clone(),
            detail: String::from("more values than an identifier can name"),
        })?;
        Ok(value)
    }

    /// A constant in the result register.
    fn constant(&mut self, value: ConstValue, ty: &IrType) -> Result<(), CodegenError> {
        match value {
            ConstValue::Bool(flag) => self.li(RESULT, i32::from(flag)),
            ConstValue::Int(number) => {
                let size = value_size(ty).unwrap_or(8);
                self.wide_constant(number as u64, size, RESULT)
            }
            ConstValue::Pointer(address) => {
                self.wide_constant(address, value_size(ty).unwrap_or(8), RESULT)
            }
            // A void value has no representation and nothing may read one.
            ConstValue::Void => Ok(()),
        }
    }

    /// Materialises a 64-bit bit pattern in `into`, at the value's own width.
    ///
    /// A word's immediate is a signed 32-bit value, so a wide constant is built
    /// from its halves, and `LI` *sign-extends* the half it is given. That
    /// extension is the whole difficulty: `LI` of a low half whose bit 31 is set
    /// fills the whole register with ones, so the only patterns a single `LI`
    /// gets right are the ones where that is what the number actually is.
    ///
    /// The high half goes in first and is shifted up, and the low half is
    /// truncated back down to 32 bits by a shift left and a *logical* shift
    /// right, which drops the extension the `LI` added. Building the halves the
    /// other way round — shifting the low half up and or-ing the high half in
    /// unshifted — produces `(low << 32) | high`, so `4294967296` came out as
    /// `1`.
    ///
    /// A single `LI` is still preferred where it is correct, because shifting
    /// unconditionally would turn `Pointer(8)` into `0x8_0000_0008`.
    fn wide_constant(&mut self, bits: u64, size: u32, into: u8) -> Result<(), CodegenError> {
        let low = bits as u32;
        if size <= 4 {
            // The store that follows truncates to the value's own width, so a
            // sign-extended register is narrowed back to the value it came from.
            return self.li(into, low as i32);
        }
        let high = (bits >> 32) as u32;
        // `LI` is exactly the number when the high half is a sign extension of
        // the low half's bit 31: all zeros above a low half below 2^31, or all
        // ones above a low half at or above it.
        if (high == 0 && low < 0x8000_0000) || (high == u32::MAX && low >= 0x8000_0000) {
            return self.li(into, low as i32);
        }
        // `high << 32`. The shift discards the low 32 bits, so the sign
        // extension `LI` added above them does not reach the result.
        self.li(into, high as i32)?;
        self.li(OPERAND_A, 32)?;
        self.emit(
            Opcode::Shl,
            &[
                Operand::Register(register(into)),
                Operand::Register(register(into)),
                Operand::Register(register(OPERAND_A)),
            ],
        )?;
        // `low` as 32 unsigned bits, by shifting its extension out of the way.
        self.li(OPERAND_B, low as i32)?;
        self.emit(
            Opcode::Shl,
            &[
                Operand::Register(register(OPERAND_B)),
                Operand::Register(register(OPERAND_B)),
                Operand::Register(register(OPERAND_A)),
            ],
        )?;
        self.emit(
            Opcode::Shr,
            &[
                Operand::Register(register(OPERAND_B)),
                Operand::Register(register(OPERAND_B)),
                Operand::Register(register(OPERAND_A)),
            ],
        )?;
        self.emit(
            Opcode::Or,
            &[
                Operand::Register(register(into)),
                Operand::Register(register(into)),
                Operand::Register(register(OPERAND_B)),
            ],
        )?;
        Ok(())
    }

    /// A constant into an argument register, at its declared width.
    ///
    /// An immediate call argument has to be a word, because a register is a word
    /// and the argument's own slot is not the callee's to read. A value that does
    /// not fit in an immediate is built from its halves like any other wide
    /// constant, so a caller never has to know whether its number was small.
    fn constant_into(&mut self, value: ConstValue, into: u8) -> Result<(), CodegenError> {
        match value {
            ConstValue::Bool(flag) => self.li(into, i32::from(flag)),
            ConstValue::Int(number) => self.wide_constant(number as u64, 8, into),
            ConstValue::Pointer(address) => self.wide_constant(address, 8, into),
            ConstValue::Void => self.li(into, 0),
        }
    }

    // -- prologue and epilogue --

    /// Reserves the frame and stores the parameters into their slots.
    ///
    /// The stack pointer after this *is* the frame base, so every offset the
    /// frontend and this stage computed is measured from it. The epilogue gives
    /// the word back before `RET`, which is what `docs/lz64.md` requires of a
    /// callee: "callee restores its entry SP before RET".
    fn prologue(&mut self) -> Result<(), CodegenError> {
        let total = self.layout.total;
        if u64::from(total) > i32::MAX as u64 {
            return Err(CodegenError::FrameTooLarge {
                function: self.name.clone(),
                size: u64::from(total),
            });
        }
        self.emit(Opcode::Getsp, &[Operand::Register(register(ADDRESS))])?;
        self.emit(
            Opcode::Addi,
            &[
                Operand::Register(register(ADDRESS)),
                Operand::Register(register(ADDRESS)),
                Operand::Immediate(-(total as i32)),
            ],
        )?;
        self.emit(Opcode::Setsp, &[Operand::Register(register(ADDRESS))])?;
        self.store_parameters()
    }

    /// Moves the arguments into the frame slots the body reads.
    ///
    /// `docs/lz64.md` fixes the entry state: the first four word-sized arguments
    /// are in `r0`–`r3`, and arguments five and six are at `[SP+8]` and
    /// `[SP+16]` relative to the stack pointer the callee was entered with. That
    /// pointer is now `total` bytes higher, so a stack argument is read from
    /// `total + 8` and `total + 16`. A parameter is a frontend local, so a body
    /// that reads the parameter finds it in its own slot.
    fn store_parameters(&mut self) -> Result<(), CodegenError> {
        let stack_base = self.layout.total;
        let mut word = 0usize;
        for (index, parameter) in self.function.params.iter().enumerate() {
            let size = value_size(&parameter.ty).unwrap_or(8);
            if size > 16 {
                return Err(CodegenError::UnsupportedValueType {
                    function: self.name.clone(),
                    detail: format!("a {size}-byte parameter"),
                });
            }
            let slot = self
                .layout
                .parameter(index)
                .ok_or_else(|| CodegenError::MissingFrame {
                    function: self.name.clone(),
                })?;
            for part in 0..=usize::from(size > 8) {
                let target = slot + if part == 0 { 0 } else { 8 };
                // Each half of a parameter is stored at *its own* width. A `u32`
                // is four bytes, and storing a whole word for it would write over
                // the next parameter's slot — which is not a corrupted value but a
                // *missing* one, and it fails only when a call has two narrow
                // parameters. That is why `f(a: u32, b: u32)` was the smallest
                // failing case and `f(a: u32)` was not.
                let width = if part == 0 {
                    data_size(size.min(8)).ok_or_else(|| CodegenError::UnsupportedValueType {
                        function: self.name.clone(),
                        detail: format!("a {size}-byte parameter"),
                    })?
                } else {
                    DataSize::Double
                };
                if word < Self::ARGUMENT_REGISTERS {
                    self.put_frame_width(target, word as u8, width)?;
                } else {
                    // A stack argument is measured from the stack pointer, not
                    // from the frame base. The callee was entered one word below
                    // its caller, so the convention's `[SP+8]` — argument five —
                    // is the caller's own `[SP+0]`, which is `total + 8` from
                    // here: this function's stack pointer plus the whole frame
                    // it reserved, plus the eight bytes that separate them.
                    let at = stack_base + (word as u32 - Self::ARGUMENT_REGISTERS as u32) * 8 + 8;
                    self.stack_address(at)?;
                    self.memory_at(ADDRESS, OPERAND_A, width)?;
                    self.put_frame_width(target, OPERAND_A, width)?;
                }
                word += 1;
            }
        }
        Ok(())
    }

    /// Gives the frame back and returns.
    fn epilogue(&mut self) -> Result<(), CodegenError> {
        let total = self.layout.total;
        self.emit(Opcode::Getsp, &[Operand::Register(register(ADDRESS))])?;
        self.emit(
            Opcode::Addi,
            &[
                Operand::Register(register(ADDRESS)),
                Operand::Register(register(ADDRESS)),
                Operand::Immediate(total as i32),
            ],
        )?;
        self.emit(Opcode::Setsp, &[Operand::Register(register(ADDRESS))])?;
        self.emit(Opcode::Ret, &[])?;
        Ok(())
    }

    /// `TRAP payload`.
    fn trap(&mut self, code: u32) -> Result<(), CodegenError> {
        let payload = i32::try_from(code).unwrap_or(i32::MAX);
        self.emit(Opcode::Trap, &[Operand::Immediate(payload)])?;
        Ok(())
    }

    // -- instructions --

    /// Emits one IR instruction.
    ///
    /// Every variant is handled here. An instruction with no machine form is a
    /// typed error naming the variant, never a silently skipped line: the module
    /// has already been verified, so a variant reaching this function is either a
    /// genuine gap in this stage or a check the IR should have made, and both are
    /// worth stopping for rather than emitting code that does not compute the
    /// instruction's result.
    fn instruction(&mut self, instruction: &Instruction) -> Result<(), CodegenError> {
        match instruction {
            Instruction::Const { value, ty } => {
                let result = self.define()?;
                let size = value_size(ty).unwrap_or(0);
                if size > 8 {
                    // A constant wider than a word is a *zeroed* aggregate, and
                    // that is the whole of what the IR can spell: `Insert` builds
                    // an aggregate by writing into a zero one, so the bytes a
                    // field does not mention have to start at zero. Zeroing one
                    // word would leave the rest of the slot holding whatever the
                    // previous call put there, and a view whose length word was
                    // left over is a view of the right bytes and the wrong size.
                    if !matches!(value, ConstValue::Int(0)) {
                        return Err(CodegenError::UnsupportedInstruction {
                            function: self.name.clone(),
                            detail: format!("a {size}-byte constant of {value:?}"),
                        });
                    }
                    return self.zero_slot(result, size);
                }
                if matches!(ty, IrType::Void) {
                    // A unit value is numbered like any other, so the numbering
                    // stays dense, but it has no representation and nothing reads
                    // it.
                    return Ok(());
                }
                self.constant(*value, ty)?;
                self.store_value(result, RESULT)
            }
            Instruction::Binary {
                op, left, right, ..
            } => {
                let result = self.define()?;
                self.load_value(*left, OPERAND_A)?;
                self.load_value(*right, OPERAND_B)?;
                self.emit(
                    arithmetic(*op),
                    &[
                        Operand::Register(register(RESULT)),
                        Operand::Register(register(OPERAND_A)),
                        Operand::Register(register(OPERAND_B)),
                    ],
                )?;
                // The store truncates to the result's own width, so a 32-bit
                // addition wraps at 32 bits without a mask: the word held the
                // right low half and the width dropped the rest.
                self.store_value(result, RESULT)
            }
            Instruction::Unary { op, operand, .. } => {
                let result = self.define()?;
                match op {
                    UnaryOp::Negate => {
                        self.load_value(*operand, OPERAND_A)?;
                        self.li(OPERAND_B, 0)?;
                        self.emit(
                            Opcode::Sub,
                            &[
                                Operand::Register(register(RESULT)),
                                Operand::Register(register(OPERAND_B)),
                                Operand::Register(register(OPERAND_A)),
                            ],
                        )?;
                    }
                    UnaryOp::BitNot => {
                        self.load_value(*operand, OPERAND_A)?;
                        self.emit(
                            Opcode::Not,
                            &[
                                Operand::Register(register(RESULT)),
                                Operand::Register(register(OPERAND_A)),
                            ],
                        )?;
                    }
                    UnaryOp::BoolToInt => {
                        // A `bool` is `0` or `1`, and loading it zero-extends it to
                        // exactly that, so widening it is a matter of storing the
                        // register at the wider type's own width.
                        self.load_value(*operand, OPERAND_A)?;
                    }
                    UnaryOp::IntToBool => {
                        self.load_value(*operand, OPERAND_A)?;
                        self.li(OPERAND_B, 0)?;
                        self.emit(
                            Opcode::Cmp,
                            &[
                                Operand::Register(register(OPERAND_A)),
                                Operand::Register(register(OPERAND_B)),
                            ],
                        )?;
                        return self.branch_to_value(Condition::Ne, result);
                    }
                    UnaryOp::Not => {
                        // A `bool` is `0` or `1`, so `!b` is `b == 0`. Comparing
                        // against zero is what makes this a negation: a
                        // complement of the bit pattern would turn `0` into every
                        // bit set, which is still a true value.
                        self.load_value(*operand, OPERAND_A)?;
                        self.li(OPERAND_B, 0)?;
                        self.emit(
                            Opcode::Cmp,
                            &[
                                Operand::Register(register(OPERAND_A)),
                                Operand::Register(register(OPERAND_B)),
                            ],
                        )?;
                        return self.branch_to_value(Condition::Eq, result);
                    }
                }
                self.store_value(result, RESULT)
            }
            Instruction::Compare { op, left, right } => {
                let result = self.define()?;
                // The comparison itself is unsigned or signed by what the IR
                // asked for, not by what the machine would prefer, and the
                // operands are loaded at their own widths so the flags describe
                // the two numbers rather than two words containing them.
                self.load_value(*left, OPERAND_A)?;
                self.load_value(*right, OPERAND_B)?;
                self.emit(
                    Opcode::Cmp,
                    &[
                        Operand::Register(register(OPERAND_A)),
                        Operand::Register(register(OPERAND_B)),
                    ],
                )?;
                self.branch_to_value(comparison(*op), result)
            }
            Instruction::LogicalAnd { left, right } | Instruction::LogicalOr { left, right } => {
                let result = self.define()?;
                let opcode = if matches!(instruction, Instruction::LogicalAnd { .. }) {
                    Opcode::And
                } else {
                    Opcode::Or
                };
                // Both operands are already `0` or `1` and both are already
                // evaluated. The *short-circuiting* is in the control flow: the
                // lowering put the right side in a block that only the left
                // side's value can reach, so this sees both as ordinary values.
                self.load_value(*left, OPERAND_A)?;
                self.load_value(*right, OPERAND_B)?;
                self.emit(
                    opcode,
                    &[
                        Operand::Register(register(RESULT)),
                        Operand::Register(register(OPERAND_A)),
                        Operand::Register(register(OPERAND_B)),
                    ],
                )?;
                self.store_value(result, RESULT)
            }
            Instruction::Load {
                address,
                width,
                space,
                ..
            } => {
                let result = self.define()?;
                self.check_space(*space, "a load")?;
                self.load_value(*address, OPERAND_A)?;
                let size = data_size(width.bytes()).ok_or_else(|| {
                    CodegenError::UnsupportedInstruction {
                        function: self.name.clone(),
                        detail: format!("a {}-byte load", width.bytes()),
                    }
                })?;
                if width.is_signed() {
                    self.memory_signed_at(OPERAND_A, RESULT, size)?;
                } else {
                    self.memory_at(OPERAND_A, RESULT, size)?;
                }
                self.store_value(result, RESULT)
            }
            Instruction::Store {
                address,
                value,
                width,
                space,
            } => {
                self.check_space(*space, "a store")?;
                let size = data_size(width.bytes()).ok_or_else(|| {
                    CodegenError::UnsupportedInstruction {
                        function: self.name.clone(),
                        // A store of a whole view would write its length as if it
                        // were bytes, so the width has to be a machine width.
                        detail: format!("a {}-byte store", width.bytes()),
                    }
                })?;
                self.load_value(*address, OPERAND_A)?;
                self.load_value(*value, OPERAND_B)?;
                self.store_at(OPERAND_A, OPERAND_B, size)
            }
            Instruction::Call {
                target,
                args,
                result,
                ..
            } => {
                let value = self.define()?;
                self.call(target, args, result)?;
                if !matches!(result, IrType::Void) {
                    let size = value_size(result).unwrap_or(0);
                    if size > 8 {
                        // A two-word result is the callee's `r0` and `r1`, and it is
                        // written to the result's own slot. Storing it — rather than
                        // leaving it in registers — is what lets an expression that
                        // returns a view sit in the middle of a larger expression:
                        // every later read of that value goes to its slot, and a
                        // register would be gone by then.
                        let offset = self.layout.offset(value)?;
                        self.put_frame(offset, RETURN_REGISTER)?;
                        self.put_frame(offset + 8, RETURN_REGISTER + 1)?;
                    } else {
                        self.store_value(value, RETURN_REGISTER)?;
                    }
                }
                Ok(())
            }
            Instruction::Intrinsic { kind, operand, .. } => {
                let value = self.define()?;
                match kind {
                    Intrinsic::FrameBase => {
                        if operand.is_some() {
                            return Err(CodegenError::UnsupportedInstruction {
                                function: self.name.clone(),
                                detail: String::from("a frame base with an operand"),
                            });
                        }
                        // The stack pointer is the bottom of the frame; the
                        // frame's own storage starts above the outgoing argument
                        // words, so this is `SP + reserve`.
                        self.emit(Opcode::Getsp, &[Operand::Register(register(RESULT))])?;
                        self.emit(
                            Opcode::Addi,
                            &[
                                Operand::Register(register(RESULT)),
                                Operand::Register(register(RESULT)),
                                Operand::Immediate(OUTGOING_ARGUMENT_BYTES as i32),
                            ],
                        )?;
                        self.store_value(value, RESULT)
                    }
                    Intrinsic::SliceLength => {
                        let view = operand.ok_or_else(|| CodegenError::UnsupportedInstruction {
                            function: self.name.clone(),
                            detail: String::from("a slice length with no view"),
                        })?;
                        if !lazalith_ir::value_type(self.function, view)
                            .is_some_and(|ty| matches!(ty, IrType::Slice { .. }))
                        {
                            return Err(CodegenError::UnsupportedInstruction {
                                function: self.name.clone(),
                                detail: format!("a length of the non-view value {}", view.get()),
                            });
                        }
                        // The length is the view's second word, read from the
                        // view's own slot. Reading it from anywhere else would be a
                        // length that belonged to something else.
                        let offset = self.layout.word(view, 1)?;
                        self.load_frame(offset, RESULT)?;
                        self.store_value(value, RESULT)
                    }
                    Intrinsic::FunctionAddress => {
                        // The instruction carries no function name, so there is
                        // nothing to resolve and no address to compute. Answering
                        // with the current function's address would answer a
                        // different question.
                        Err(CodegenError::UnsupportedInstruction {
                            function: self.name.clone(),
                            detail: String::from(
                                "a function address, which carries no function to name",
                            ),
                        })
                    }
                }
            }
            Instruction::Copy { value, .. } => {
                let result = self.define()?;
                self.copy_value(*value, result)
            }
            Instruction::Extract {
                aggregate,
                offset,
                ty,
                ..
            } => {
                let result = self.define()?;
                let (width, signed) = self.scalar_shape(ty, "field read")?;
                let base = self.layout.offset(*aggregate)?;
                let at = base.checked_add(*offset).ok_or_else(|| {
                    CodegenError::UnsupportedValueType {
                        function: self.name.clone(),
                        detail: format!("a field at byte {offset}"),
                    }
                })?;
                self.frame_address(at)?;
                if signed {
                    self.memory_signed_at(ADDRESS, RESULT, width)?;
                } else {
                    self.memory_at(ADDRESS, RESULT, width)?;
                }
                self.store_value(result, RESULT)
            }
            Instruction::Insert {
                aggregate,
                offset,
                value,
                result: ty,
            } => {
                let result = self.define()?;
                let size = value_size(ty).unwrap_or(0);
                if size <= 8 {
                    // The whole result is the field, so the field is the whole
                    // result and the rest of the aggregate operand has nowhere to
                    // go.
                    self.load_value(*value, RESULT)?;
                    return self.store_value(result, RESULT);
                }
                // An aggregate is built by writing into a copy of the initial
                // value, so the fields the insert does not mention survive.
                self.copy_words(*aggregate, self.layout.offset(result)?)?;
                let part = self.value_type(*value)?;
                let (width, _) = self.scalar_shape(&part, "field write")?;
                let at = self
                    .layout
                    .offset(result)?
                    .checked_add(*offset)
                    .ok_or_else(|| CodegenError::UnsupportedValueType {
                        function: self.name.clone(),
                        detail: format!("a field at byte {offset}"),
                    })?;
                self.load_value(*value, OPERAND_A)?;
                self.put_frame_width(at, OPERAND_A, width)
            }
            Instruction::Trap { code } => self.trap(*code),
            Instruction::BoundsCheck {
                index,
                length,
                code,
            } => {
                // The check compares what it was given and traps; it is not a
                // branch the program can write around, and the index is compared
                // as an unsigned value so a negative index cannot pass as a small
                // one. The flags come straight from `CMP`, so the comparison is
                // "index >= length" and nothing is invented.
                self.load_value(*index, OPERAND_A)?;
                self.load_value(*length, OPERAND_B)?;
                self.emit(
                    Opcode::Cmp,
                    &[
                        Operand::Register(register(OPERAND_A)),
                        Operand::Register(register(OPERAND_B)),
                    ],
                )?;
                let failing = self.local_label("bounds.fail");
                let join = self.local_label("bounds.join");
                self.branch_to(Condition::Uge, &failing)?;
                self.branch_always(&join)?;
                self.mark(&failing);
                self.trap(*code)?;
                self.mark(&join);
                Ok(())
            }
            Instruction::DataAddress { name, .. } => {
                let result = self.define()?;
                self.data_address(name, result)
            }
        }
    }

    /// Zeroes a value's whole slot.
    ///
    /// The slot is rounded up to a whole word by the layout, so the padding after
    /// a size that is not a multiple of eight belongs to this value and clearing
    /// it costs nothing and reads as nothing.
    fn zero_slot(&mut self, value: ValueId, size: u32) -> Result<(), CodegenError> {
        let base = self.layout.offset(value)?;
        for word in 0..size.div_ceil(8) {
            self.li(OPERAND_A, 0)?;
            self.put_frame(base + word * 8, OPERAND_A)?;
        }
        Ok(())
    }

    /// Copies a value into another slot, at the value's own shape.
    fn copy_value(&mut self, source: ValueId, result: ValueId) -> Result<(), CodegenError> {
        let ty = self.value_type(source)?;
        if value_size(&ty).unwrap_or(0) > 8 {
            // Wider than a word, so every word of it: a copy that moved the
            // address and left the length would be a view of the right bytes and
            // the wrong size.
            return self.copy_words(source, self.layout.offset(result)?);
        }
        self.load_value(source, RESULT)?;
        self.store_value(result, RESULT)
    }

    /// The address of a data segment, in `result`.
    fn data_address(&mut self, name: &str, result: ValueId) -> Result<(), CodegenError> {
        if !self.segments.iter().any(|segment| segment.name == name) {
            return Err(CodegenError::UnknownData {
                function: self.name.clone(),
                name: String::from(name),
            });
        }
        // `LI` with a zero immediate and a relocation: the segment's address is
        // the linker's to decide, so the immediate is a hole for it to fill and
        // not an address this stage made up.
        let at = self.emit(
            Opcode::Li,
            &[Operand::Register(register(RESULT)), Operand::Immediate(0)],
        )?;
        self.relocate(DATA_RELOCATION, name, at)?;
        self.store_value(result, RESULT)
    }

    /// Materialises a `bool` from the flags `CMP` has just set.
    ///
    /// The ISA has no instruction that writes a condition into a register, so the
    /// value has to come from a store of a constant on each side of a branch.
    fn branch_to_value(
        &mut self,
        condition: Condition,
        result: ValueId,
    ) -> Result<(), CodegenError> {
        let taken = self.local_label("bool.true");
        let join = self.local_label("bool.join");
        self.branch_to(condition, &taken)?;
        self.li(RESULT, 0)?;
        self.store_value(result, RESULT)?;
        self.branch_always(&join)?;
        self.mark(&taken);
        self.li(RESULT, 1)?;
        self.store_value(result, RESULT)?;
        self.mark(&join);
        Ok(())
    }

    /// A memory space the machine has no address for.
    fn check_space(&self, space: MemorySpace, action: &str) -> Result<(), CodegenError> {
        if space == MemorySpace::Program {
            return Ok(());
        }
        Err(CodegenError::UnsupportedInstruction {
            function: self.name.clone(),
            // The machine has one address space, and treating a platform address
            // as a program one would read whatever happens to be mapped there.
            detail: format!(
                "{action} in the {} space",
                format!("{space:?}").to_lowercase()
            ),
        })
    }

    // -- calls --

    /// Emits a call's argument passing and the call itself.
    ///
    /// The two kinds of call have different conventions, and the ABI already
    /// states both, so neither is invented here. A Lazen function takes its
    /// first four argument words in `r0`–`r3` and its fifth and sixth at
    /// `[SP+8]` and `[SP+16]`, and returns one word in `r0`. A syscall takes its
    /// number in `r0`, its arguments in `r1`–`r6`, its status in `r0` and its
    /// payload in `r1`, and the registers its ABI entry state reserves must be
    /// zero.
    fn call(
        &mut self,
        target: &CallTarget,
        args: &[CallArg],
        result: &IrType,
    ) -> Result<(), CodegenError> {
        match target {
            CallTarget::Syscall(name) => self.syscall(name, args, result),
            CallTarget::Function(name) => self.call_function(&function_symbol(name), args, result),
            CallTarget::Imported(name) => self.call_function(name, args, result),
        }
    }

    /// How many words one argument takes on the stack.
    fn argument_words(&self, argument: &CallArg) -> Result<u32, CodegenError> {
        let size: u32 = match argument {
            // The IR's own rule, so the compiler's decision that a call fits and
            // this stage's placement of the arguments cannot disagree: a value's
            // size rounded up to words, which is one for a 64-bit integer and two
            // for a view.
            CallArg::Value(value) => {
                let ty = self.value_type(*value)?;
                return Ok(lazalith_ir::argument_words(&ty));
            }
            // An immediate is a word, because the callee reads words and an
            // immediate has no slot of its own to be read from.
            CallArg::Immediate(_) => 8,
        };
        Ok(size.div_ceil(8))
    }

    /// Moves the arguments into the places the callee will read them.
    ///
    /// A view is two words and is passed as two words, in the same order the
    /// callee's prologue stores them: this is the ABI, and the frontend's
    /// two-word return rule is the same rule on the way back.
    fn place_arguments(
        &mut self,
        args: &[CallArg],
        first: u8,
        stack_base: Option<u32>,
    ) -> Result<(), CodegenError> {
        let mut word = 0usize;
        for argument in args {
            let count = self.argument_words(argument)?;
            for part in 0..count {
                let Some(target) = self.argument_place(word, first, stack_base) else {
                    return Err(CodegenError::UnsupportedValueType {
                        function: self.name.clone(),
                        detail: format!(
                            "{word} argument words, and the convention has {}",
                            match stack_base {
                                // Four registers and the two stack words a frame
                                // reserves below them.
                                Some(_) =>
                                    Self::ARGUMENT_REGISTERS + OUTGOING_ARGUMENT_BYTES as usize / 8,
                                None => MAX_ARGUMENT_WORDS,
                            }
                        ),
                    });
                };
                match argument {
                    CallArg::Value(value) => {
                        // A view's words come from the view's own slot, so a
                        // passed view is the same two words it holds.
                        let offset = if part == 0 {
                            self.layout.offset(*value)?
                        } else {
                            self.layout.word(*value, part)?
                        };
                        self.load_frame(offset, OPERAND_A)?;
                        self.put_argument(target, OPERAND_A)?;
                    }
                    CallArg::Immediate(value) => {
                        self.constant_into(*value, OPERAND_A)?;
                        self.put_argument(target, OPERAND_A)?;
                    }
                }
                word += 1;
            }
        }
        Ok(())
    }

    /// The argument words a *Lazen* call passes in registers.
    ///
    /// A Lazen call passes its first four words in `r0`-`r3` and the rest on the
    /// stack, so its callee reads a fifth and sixth word from the stack the
    /// caller reserved. A syscall is different: it reads six words from `r1`-`r6`
    /// and has no stack at all. Both allow six words, so the count alone does not
    /// say where a word goes — whether the call has a stack does.
    const ARGUMENT_REGISTERS: usize = 4;

    /// Where argument word `word` goes.
    ///
    /// A Lazen function's first four words are registers and the rest are at the
    /// stack, so a function is given `first = 0` and the frame offsets the ABI
    /// fixes. A syscall's words are all registers, starting after the number, so
    /// it is given `first = 1` and no stack at all.
    fn argument_place(
        &self,
        word: usize,
        first: u8,
        stack_base: Option<u32>,
    ) -> Option<ArgumentPlace> {
        // A call that has a stack to spill into is a Lazen call, and it has four
        // argument registers. A syscall has no stack and uses all six.
        let registers = if stack_base.is_some() {
            Self::ARGUMENT_REGISTERS
        } else {
            MAX_ARGUMENT_WORDS
        };
        if word < registers {
            return Some(ArgumentPlace::Register(first + u8::try_from(word).ok()?));
        }
        let base = stack_base?;
        // A `CALL` pushes the return PC at `oldSP-8`, so the callee is entered
        // with its stack pointer one word *below* the caller's. The convention's
        // `[SP+8]` and `[SP+16]` are therefore the caller's own `[SP+0]` and
        // `[SP+8]`: argument word five goes at the caller's offset zero and word
        // six at offset eight, which is why a frame reserves them at its base.
        let offset = u32::try_from(word - registers).ok()?.checked_mul(8)?;
        Some(ArgumentPlace::Frame(base + offset))
    }

    /// Writes one argument word to its place.
    fn put_argument(&mut self, place: ArgumentPlace, from: u8) -> Result<(), CodegenError> {
        match place {
            ArgumentPlace::Register(index) => {
                self.emit(
                    Opcode::Mov,
                    &[
                        Operand::Register(register(index)),
                        Operand::Register(register(from)),
                    ],
                )?;
                Ok(())
            }
            ArgumentPlace::Frame(offset) => {
                self.stack_address(offset)?;
                self.store_at(ADDRESS, from, DataSize::Double)
            }
        }
    }

    /// A call to another function in the image.
    fn call_function(
        &mut self,
        symbol: &str,
        args: &[CallArg],
        result: &IrType,
    ) -> Result<(), CodegenError> {
        let size = value_size(result).unwrap_or(0);
        if size > 16 {
            // One word comes back in `r0` and a two-word value in `r0` and `r1`.
            // No Lazen type is wider than a view, so this is the whole of what
            // cannot be returned rather than a limit that will be reached.
            return Err(CodegenError::UnsupportedValueType {
                function: self.name.clone(),
                detail: format!("a {size}-byte return value"),
            });
        }
        // No callee-saved register is live, so nothing has to be spilled across
        // the call, and `r7` is caller-saved so the callee may clobber it: every
        // address is rebuilt from the frame afterwards.
        self.place_arguments(args, 0, Some(0))?;
        let at = self.emit(Opcode::Call, &[Operand::Immediate(0)])?;
        self.relocate(BRANCH_RELOCATION, symbol, at)
    }

    /// A call through the OS ABI.
    fn syscall(
        &mut self,
        name: &str,
        args: &[CallArg],
        _result: &IrType,
    ) -> Result<(), CodegenError> {
        let abi = crate::abi_syscall(name).ok_or_else(|| CodegenError::UnnumberedSyscall {
            function: self.name.clone(),
            name: String::from(name),
        })?;
        // The number goes in first: it is `r0`, and the arguments start at `r1`.
        let number = i32::from(abi.as_u16());
        self.li(RETURN_REGISTER, number)?;
        self.place_arguments(args, 1, None)?;
        // The ABI's entry state says `r7` is reserved and must be zero, and it
        // says which argument registers a given service requires to be zero. Both
        // are stated here rather than assumed, because this stage uses `r7` as its
        // address scratch and a service is entitled to check.
        self.li(ADDRESS, 0)?;
        let required = abi.required_zero_argument_mask();
        for index in 0..MAX_ARGUMENT_WORDS {
            if required & (1_u64 << index) != 0 {
                let at = 1 + u8::try_from(index).unwrap_or(0);
                self.li(at, 0)?;
            }
        }
        self.emit(Opcode::Syscall, &[]).map(|_| ())
    }
    // -- terminators --

    /// Emits how a block ends.
    ///
    /// A jump and a branch name the target's *label*, never an offset or a block
    /// number: the displacement is the linker's to fill in, so a target that has
    /// not been emitted yet, or that a loop reaches from a block the loop could
    /// not have predicted, is a name rather than a guess.
    fn terminator(&mut self, terminator: &Terminator) -> Result<(), CodegenError> {
        match terminator {
            Terminator::Jump(target) => {
                let label = self.block_label(*target);
                self.jump_to(&label)
            }
            Terminator::Branch {
                condition,
                then_block,
                otherwise,
            } => {
                let then_label = self.block_label(*then_block);
                let otherwise_label = self.block_label(*otherwise);
                self.branch_on(*condition, &then_label, &otherwise_label)
            }
            Terminator::Return(ReturnValue::Void) => {
                self.epilogue()?;
                self.terminated = true;
                Ok(())
            }
            Terminator::Return(ReturnValue::Value(value)) => {
                let ty = self.value_type(*value)?;
                let size = value_size(&ty).unwrap_or(0);
                if size > 16 {
                    // One word comes back in `r0` and a two-word value in `r0` and
                    // `r1`; anything wider would need a third register the
                    // convention does not have, and no Lazen type is that wide.
                    return Err(CodegenError::UnsupportedValueType {
                        function: self.name.clone(),
                        detail: format!("a {size}-byte return value"),
                    });
                }
                // A view is two words and is returned the same way it is passed:
                // the address in `r0`, the length in `r1`. Returning it any other way
                // would mean a caller had to know where the callee put it, which is
                // the one thing a calling convention exists to prevent.
                if size > 8 {
                    // A view returns the same two words it is passed as: the
                    // address in `r0` and the length in `r1`. Emitting a move per
                    // word, rather than immediates, is what makes this work for a
                    // value the compiler cannot know at compile time — which is the
                    // only kind that reaches here.
                    let address_offset = self.layout.offset(*value)?;
                    let length_offset = self.layout.word(*value, 1)?;
                    self.load_frame(address_offset, ADDRESS)?;
                    self.emit(
                        Opcode::Mov,
                        &[
                            Operand::Register(register(RETURN_REGISTER)),
                            Operand::Register(register(ADDRESS)),
                        ],
                    )?;
                    self.load_frame(length_offset, ADDRESS)?;
                    self.emit(
                        Opcode::Mov,
                        &[
                            Operand::Register(register(RETURN_REGISTER + 1)),
                            Operand::Register(register(ADDRESS)),
                        ],
                    )?;
                } else {
                    self.load_value(*value, RETURN_REGISTER)?;
                }
                self.epilogue()?;
                self.terminated = true;
                Ok(())
            }
            Terminator::Unreachable => {
                // No control flow can arrive here, so this never runs. It is still
                // emitted rather than left as whatever the next function's code
                // happens to be: an instruction has to be *something*, and a trap
                // is the one answer that is wrong loudly if the reasoning above
                // is ever wrong.
                self.trap(0)?;
                self.terminated = true;
                Ok(())
            }
        }
    }
}

/// Where one argument word goes.
#[derive(Clone, Copy, Debug)]
enum ArgumentPlace {
    /// A register, numbered from the machine's `r0`.
    Register(u8),
    /// A frame offset above the caller's own reserved area.
    Frame(u32),
}
