# LZA64, and the difference between LZA and LZ

`binstruction.md` §7 names the architecture **LZA — the Lazalith Architecture**,
in two widths, `LZA32` and `LZA64`, with `LZA64` as the primary 64-bit target
identity. It also says the existing `LZ32` / `LZ64` terminology "may remain in
current APIs and documentation where already established", and asks for the
distinction to be documented.

This is that document.

---

## The short version

| | |
| --- | --- |
| **LZA64** | a *name*. The architecture: an ISA, an ABI, an object model, a VM contract, a device contract. |
| **LZ64** | a *width*. One of the two word widths that architecture is parameterised by. |

`LZ64` answers "how wide is a register". `LZA64` answers "which machine am I
targeting". They are not competing names for the same thing, and a document that
uses them interchangeably is one a reader cannot act on.

The same holds for `LZA32` and `LZ32`.

---

## What is already true in this repository

**Fact.** There is one architecture, parameterised by width. This is enforced, not
asserted: `lz32_and_lz64_share_one_architecture` in
`crates/lazalith-cli/tests/architecture.rs` checks three things at once.

1. `ArchitectureConfig::lz32().word_width() == WordWidth::W32` and
   `ArchitectureConfig::lz64().word_width() == WordWidth::W64`.
2. The **same encoded `NOP` bytes decode correctly under both configurations** —
   so a binary produced for one is not a different encoding, it is the same
   encoding at a different width.
3. No crate in `lazalith-isa`, `lazalith-cpu`, `lazalith-memory` or
   `lazalith-machine` depends on a crate whose name ends in `32` or `64`. There is
   no `lazalith-cpu64`.

That third check is the one that matters for naming. A fork per width would make
"LZA64" a crate name and "LZ64" a crate name and the two indistinguishable in any
discussion about which one a change belongs in. Not having the fork is what makes
the distinction worth keeping.

**Fact.** Where the width is actually decided, in one place:

| | |
| --- | --- |
| `lazalith_types::WordWidth` | `W32` / `W64` — the only enumeration of width in the platform |
| `lazalith_types::ArchitectureConfig` | a word width plus a feature set; `lz32()` and `lz64()` are its two constructors |
| `lazalith_boot::BootArchitecture` | `Lz32 = 1`, `Lz64 = 2` — the tag in the boot image header |
| `lazalith_os::LzxArchitecture` | the tag in the `.lzx` executable header |

**Fact.** `docs/lz32.md` and `docs/lz64.md` are the Phase-I documents that answer
"what changes between the two widths". They are still the answer, and nothing in
this pass changed them. The answer is: the register width, the pointer width, the
address width, the stack behaviour, the natural data sizes, and the ABI argument
and return conventions — all derived from `WordWidth`, none of them a separate
implementation.

---

## What a target identity would add, and does not exist yet

**Proposal.** `lza64-unknown-lazos` as a Rust-style target triple. `binstruction.md`
§7 offers this and calls it a proposal. It is recorded as one here and nothing in
this repository reads it: no build script, no compiler flag, no package format and
no test consumes a target triple, because no stage has needed one.

**Requirement, for the stage that does need one.** The C compiler is the first
thing that will. A target identity is the name under which a sysroot is chosen, a
startup object is picked, and an ABI is selected — so it becomes real at B15
(sysroot) and B25 (target-side LazOS), not before. When it does, it has to be
derived from the same `ArchitectureConfig` everything else uses, or a target name
will be able to disagree with the machine it names.

---

## Naming rules, so this stays true

1. **`LZA64` names the target. `LZ64` names the width.** A sentence that means
   "the machine" says LZA64. A sentence that means "32 versus 64" says LZ32/LZ64.
2. **Nothing gets renamed for this.** The Phase-I names are load-bearing in
   on-disk formats: `BootArchitecture::Lz64` and `LzxArchitecture` are tag values
   that exist in images this platform has already produced. Renaming a Rust
   variant does not rename a byte, and pretending otherwise would make the code
   and the format disagree.
3. **A document states which it means**, on first use in a section, when both could
   be read either way. This document is an example.

---

## Related

- `docs/architecture.md` — the whole contract, and where the boundaries are enforced
- `docs/lz32.md`, `docs/lz64.md` — the Phase-I width semantics, unchanged
- `docs/beyond-lazalith.md` — the master Beyond document
