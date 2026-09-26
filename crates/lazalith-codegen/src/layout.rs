//! Per-function frame layout for code generation.
//!
//! The lowering reports a frame for every function: the frontend's locals and
//! parameters, plus the temporaries the lowering needed. Code generation adds one
//! more area above that, because this stage has no register allocator and keeps
//! **every IR value in the frame**.
//!
//! The prologue moves the stack pointer down by the total, so the new stack
//! pointer *is* the frame base, exactly as Step 62 assumed: the frontend's
//! offsets are still at `frame base + offset`, and the value area sits above
//! them. Nothing is addressed negatively, so no displacement sign is relied on.

use alloc::string::String;
use alloc::vec::Vec;

use lazalith_compiler::lower::FrameLayout;
use lazalith_ir::{Function, ValueId, instruction_result_type, produces_value};
use lazalith_types::ArchitectureConfig;

use crate::{CodegenError, value_alignment, value_size};

/// The bytes every frame reserves at its base for arguments passed on the stack.
///
/// `docs/lz64.md` puts argument words five and six at `[SP+8]` and `[SP+16]` at
/// callee entry, and a `CALL` pushes the return PC at `oldSP-8`. Those two words
/// are therefore the *caller's* own `[SP+0]` and `[SP+8]`, so a caller must own
/// them, and a frame that put its first local at offset zero would have a call
/// with five or more argument words overwrite it.
pub const OUTGOING_ARGUMENT_BYTES: u32 = 16;

/// The bytes every frame reserves *below* its stack pointer, for the return
/// address of a call it makes.
///
/// A `CALL` pushes the return PC at `oldSP-8` — one word *below* the caller's
/// stack pointer — so a function that calls anything needs those eight bytes to
/// be its own. Without the reserve, a function's first call overwrote the return
/// address its own caller had left for it, and the second `RET` in a call chain
/// returned to whatever the stack held instead.
pub const RETURN_ADDRESS_BYTES: u32 = 8;

/// A function's value slots and total frame size.
#[derive(Debug)]
pub struct FunctionLayout {
    /// The frame's total size: the locals, the lowering's temporaries, and the
    /// value area.
    pub total: u32,
    /// The frame offset of each parameter, in declaration order.
    parameters: Vec<u32>,
    /// `(offset, size)` per value, indexed by the value's identifier. Values are
    /// defined in identifier order, so the index *is* the identifier.
    slots: Vec<(u32, u32)>,
}

impl FunctionLayout {
    /// Works out where every value lives.
    pub fn new(
        function: &Function,
        frame: &FrameLayout,
        architecture: &ArchitectureConfig,
    ) -> Result<Self, CodegenError> {
        let _ = architecture;
        // Slot offsets are measured from the *frame's storage base*, which is the
        // stack pointer plus the outgoing argument reserve, so the lowering's own
        // offsets mean the same thing here as they do there. The reserve is
        // counted once, in `total` below, and the emitter adds it when it turns an
        // offset into an address.
        let value_base = align_up(frame.size, 8);
        // Parameters are the frontend's own slots: a body reads one through a
        // local reference, so the prologue has to store the incoming word there
        // rather than in a value slot of its own.
        let mut parameters: Vec<u32> = frame
            .slots
            .iter()
            .filter(|slot| slot.is_parameter)
            .map(|slot| slot.offset)
            .collect();
        parameters.truncate(function.params.len());
        let mut slots: Vec<(u32, u32)> = Vec::new();
        for _ in &function.params {
            slots.push((0, 0));
        }
        let mut cursor = value_base;
        for block in &function.blocks {
            for instruction in &block.instructions {
                if !produces_value(instruction) {
                    continue;
                }
                let ty = instruction_result_type(instruction);
                let size = value_size(&ty).unwrap_or(0);
                // The machine faults on a misaligned access, so a slot is aligned
                // to its own type's alignment rather than to a fixed one.
                let alignment = u32::try_from(value_alignment(&ty)).unwrap_or(1).max(1);
                let start = align_up(cursor, alignment);
                slots.push((start, size));
                // The next slot starts at a whole word, so a one-byte `bool`
                // cannot leave the following eight-byte value unaligned.
                cursor = align_up(start + size, 8);
            }
        }
        // The whole frame, measured from the stack pointer: the word below it for
        // the return address of a call this function makes, then the reserve for
        // outgoing argument words, then the lowering's own slots and the value
        // area. The return address comes first because it is the only part of the
        // frame *below* the stack pointer, and a `CALL` writes there.
        let total = RETURN_ADDRESS_BYTES
            .saturating_add(OUTGOING_ARGUMENT_BYTES)
            .saturating_add(align_up(cursor, 8));
        if u64::from(total) > i32::MAX as u64 {
            return Err(CodegenError::FrameTooLarge {
                function: String::from("<function>"),
                size: u64::from(total),
            });
        }
        Ok(Self {
            total,
            parameters,
            slots,
        })
    }

    /// The frame offset of the parameter at `index`.
    pub fn parameter(&self, index: usize) -> Option<u32> {
        self.parameters.get(index).copied()
    }

    /// The byte offset of a value from the frame base.
    pub fn offset(&self, value: ValueId) -> Result<u32, CodegenError> {
        self.slots
            .get(value.get() as usize)
            .map(|slot| slot.0)
            .ok_or_else(|| CodegenError::UnsupportedValueType {
                function: String::from("<function>"),
                detail: alloc::format!("a use of the undefined value {}", value.get()),
            })
    }

    /// The byte offset of the word at `word` within a value's slot.
    ///
    /// A view is two words and this is how its second word is named. A request
    /// past a value's end is refused rather than clamped, because reading a
    /// neighbouring value's bytes as this value's is the metadata loss this
    /// stage exists to prevent.
    pub fn word(&self, value: ValueId, word: u32) -> Result<u32, CodegenError> {
        let (offset, size) = self
            .slots
            .get(value.get() as usize)
            .copied()
            .ok_or_else(|| CodegenError::UnsupportedValueType {
                function: String::from("<function>"),
                detail: alloc::format!("a use of the undefined value {}", value.get()),
            })?;
        let byte = word
            .checked_mul(8)
            .ok_or_else(|| CodegenError::UnsupportedValueType {
                function: String::from("<function>"),
                detail: alloc::format!("word {word} of a value"),
            })?;
        if byte + 8 > size {
            return Err(CodegenError::UnsupportedValueType {
                function: String::from("<function>"),
                detail: alloc::format!(
                    "word {word} of a {size}-byte value, which has no such word"
                ),
            });
        }
        Ok(offset + byte)
    }
}

/// Rounds a byte count up to a multiple of `alignment`.
pub fn align_up(value: u32, alignment: u32) -> u32 {
    if alignment <= 1 {
        return value;
    }
    let mask = alignment - 1;
    value.saturating_add(mask) & !mask
}
