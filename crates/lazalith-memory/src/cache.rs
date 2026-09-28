//! A decode cache for the instruction fetch path.
//!
//! Decoding eight bytes into an [`Instruction`] is not the most expensive thing an
//! interpreted instruction does, but it is pure repetition: a loop body is fetched,
//! decoded, and executed a million times, and the decode is the same answer a
//! million times. This remembers the answer.
//!
//! # What may be skipped, and what may not
//!
//! A cache may skip turning bytes into an instruction. It may not skip a *check*.
//! So a lookup here is reached only after the caller has already asked the bus to
//! do every fetch check — address validity, configuration agreement, and the
//! region permission — and this cache is written to be unreachable except through
//! that path. [`crate::Bus::fetch_instruction`] is the only caller of
//! [`InstructionCache::lookup`], and it performs those checks first.
//!
//! That division is the whole safety argument. A cache that short-circuited the
//! checks would be faster and would be wrong in a way that only appears after a
//! program does something the fast path did not anticipate.
//!
//! # Invalidation
//!
//! The cache is dropped for any written range that overlaps a cached instruction.
//! That rule is deliberately stated in terms of *overlap* rather than equality, and
//! it is stated about the *physical* range, so it stays correct if a future step
//! gives the address space a real translation: a cached entry is only ever
//! reachable through the translation that produced it, and a store is only ever
//! compared against the physical bytes it would change.
//!
//! Invalidation is a scan of a small fixed table rather than a per-page generation
//! counter, because the table is small enough that a scan is cheaper than the
//! bookkeeping, and a scan cannot be forgotten. A missed invalidation is a
//! correctness bug that shows up as a program executing a stale instruction, which
//! is about the worst failure this platform has; a small table makes that failure
//! require writing an instruction and then reaching it, which the tests do on
//! purpose.

use lazalith_isa::Instruction;
use lazalith_types::PhysicalAddress;

/// How many decoded instructions are remembered.
///
/// A power of two so the index is a mask rather than a division, and small on
/// purpose: this is a cache, and a cache that has to be tuned is a cache that has
/// to be *measured*. 512 entries of a 24-byte entry is 12 KiB, which is nothing
/// next to the memory a program is running out of.
pub const ENTRIES: usize = 512;

/// One remembered instruction, and where it came from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Entry {
    start: PhysicalAddress,
    instruction: Instruction,
}

/// A direct-mapped cache of decoded instructions.
///
/// Direct-mapped, so a conflicting address simply replaces the entry rather than
/// evicting a neighbour or probing: a miss costs one decode, which is the cost
/// this cache exists to avoid paying on the *common* case and can afford to pay
/// occasionally. A program whose hot instructions collide pays the reference
/// price and nothing more, so the worst case is the interpreter with a table
/// attached.
#[derive(Debug)]
pub struct InstructionCache {
    slots: [Option<Entry>; ENTRIES],
}

impl Default for InstructionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl InstructionCache {
    /// An empty cache.
    ///
    /// The table is an array rather than a `Vec` so that a cache cannot fail to
    /// be created: an allocation failure in the middle of starting a machine is a
    /// fault nobody wants to debug, and a cache is the last thing that should be
    /// able to cause one.
    pub const fn new() -> Self {
        Self {
            slots: [None; ENTRIES],
        }
    }

    /// How many entries are currently remembered.
    ///
    /// A test and a benchmark use this; nothing in the fetch path does, because
    /// nothing in the fetch path may depend on it.
    pub fn filled(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    /// The index an address maps to.
    ///
    /// The address is folded to a word and then to a byte index, so an address
    /// that is 256 bytes past another lands in a different slot: a mask of the
    /// low bits alone would put every instruction in a 16-instruction window on
    /// the same slot, which for a program with 4-byte instructions is a quarter of
    /// its straight-line code colliding.
    const fn index(address: PhysicalAddress) -> usize {
        let folded = (address.as_u64() / 4) as u32;
        (folded as usize) & (ENTRIES - 1)
    }

    /// The instruction decoded from `start`, if it is the one remembered there.
    pub fn lookup(&self, start: PhysicalAddress) -> Option<Instruction> {
        let entry = self.slots[Self::index(start)]?;
        if entry.start != start {
            return None;
        }
        Some(entry.instruction)
    }

    /// Remembers the instruction decoded from `start`.
    pub fn insert(&mut self, start: PhysicalAddress, instruction: Instruction) {
        let slot = &mut self.slots[Self::index(start)];
        *slot = Some(Entry { start, instruction });
    }

    /// Forgets every instruction overlapping `[start, end)`.
    ///
    /// The half-open range is the same one every other address calculation in this
    /// crate uses, so a store of `n` bytes at `start` invalidates exactly the
    /// instructions those `n` bytes could be part of — including a store that
    /// begins in the middle of a cached instruction, which is the case a
    /// per-instruction equality check would get wrong.
    ///
    /// It walks the *addresses the store could have touched* and their slots, not
    /// the table. The first version scanned all 512 entries on every write, which
    /// is 512 comparisons to invalidate one instruction: fine for a program that
    /// computes and terrible for a program that stores, where it would have made
    /// the cache a tax of about five hundred comparisons per store. The address
    /// walk is `length / 8 + 2` steps, so a byte store costs two.
    pub fn invalidate_range(&mut self, start: PhysicalAddress, length: u64) {
        let end = start.as_u64().saturating_add(length);
        // An instruction overlaps when its start is strictly inside
        // `(start - 8, end)`, so the candidates are the aligned addresses in that
        // window. Rounding *down* from `start - 7` may visit one address too many,
        // which costs nothing and keeps the bound a plain loop.
        let mut candidate =
            start.as_u64().saturating_sub(INSTRUCTION_BYTES - 1) & !(INSTRUCTION_BYTES - 1);
        while candidate < end {
            let slot = &mut self.slots[Self::index(PhysicalAddress::new(candidate))];
            let overlaps = match *slot {
                None => false,
                Some(entry) => {
                    let entry_start = entry.start.as_u64();
                    let entry_end = entry_start.saturating_add(INSTRUCTION_BYTES);
                    entry_start < end && start.as_u64() < entry_end
                }
            };
            if overlaps {
                *slot = None;
            }
            candidate += INSTRUCTION_BYTES;
        }
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.slots.fill(None);
    }
}
/// How many bytes an instruction occupies in memory.
///
/// Eight for every configuration, because the fetch path always reads eight bytes
/// and a shorter instruction still has its unused bytes compared against the same
/// encoding. Using the fetched width rather than the instruction's real length is
/// what makes the overlap test conservative: it can forget too much, which costs a
/// decode, and never forgets too little, which would be a stale instruction.
pub const INSTRUCTION_BYTES: u64 = 8;
