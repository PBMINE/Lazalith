# Lazalith Object Format `.lzo` v1

`.lzo` is the native relocatable object format for Lazalith. It is produced by
the toolchain and consumed by the linker. It is not an executable format and is
never parsed by the kernel; `.lzx` remains the runtime executable format.

## Scope

Version 1 carries:

- LZ32 or LZ64 architecture selection;
- ISA and OS ABI versions;
- text, read-only data, data, and BSS sections;
- local, global, undefined, absolute, and section-defined symbols;
- typed relocations for later linking; and
- bounded source paths and instruction-to-source mappings.

It has no compression, signatures, dynamic linking, weak symbols, archives,
implicit addresses, relocatable debug text, or executable entry expansion.
Object files are little-endian and use no host-sized wire fields.

## Constants and limits

```text
magic             LZOBJ01\0
format version    1
header size       128 bytes
maximum file size 16 MiB
maximum materialized names 16 MiB
maximum sections  4096
```

The format and ISA versions are fixed at 1. The runtime `.lzx` v1 ISA version
is also fixed at 1; the shared ISA constant is checked against it at compile
time.

## File layout

```text
header
section table
symbol table
relocation table
debug-source table
debug-mapping table
NUL-terminated string table
zero padding to an 8-byte boundary
section payloads
EOF
```

All offsets are absolute byte offsets from the beginning of the file. Table
offsets are canonical and are derived from the preceding count and record
size. File-backed payloads are emitted in section order, each beginning at an
8-byte boundary. BSS and empty file-backed sections have no payload. Alignment
padding is zero. There are no undeclared gaps or trailing bytes.

## Header

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | magic |
| 8 | 2 | format version |
| 10 | 2 | header size |
| 12 | 1 | architecture: 1 LZ32, 2 LZ64 |
| 13 | 1 | flags, reserved zero |
| 14 | 2 | ISA version |
| 16 | 2 | OS ABI version |
| 18 | 2 | section count |
| 20 | 4 | reserved zero |
| 24 | 4 | symbol count |
| 28 | 4 | relocation count |
| 32 | 4 | debug-source count |
| 36 | 4 | debug-mapping count |
| 40 | 4 | entry symbol index or `0xffffffff` |
| 44 | 4 | string-table byte size |
| 48 | 8 | section-table offset |
| 56 | 8 | symbol-table offset |
| 64 | 8 | relocation-table offset |
| 72 | 8 | debug-source-table offset |
| 80 | 8 | debug-mapping-table offset |
| 88 | 8 | string-table offset |
| 96 | 8 | payload offset |
| 104 | 24 | reserved zero |

The reserved header words are checked before table contents are interpreted.
The parser validates counts, checked table arithmetic, exact canonical table
offsets, string bounds, and payload EOF before publishing a model.

## Section table

Each section record is 64 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | name string offset |
| 4 | 1 | kind |
| 5 | 3 | reserved zero |
| 8 | 8 | alignment |
| 16 | 8 | logical size |
| 24 | 8 | absolute file offset |
| 32 | 8 | file-backed size |
| 40 | 4 | section-defined symbol count |
| 44 | 4 | relocation count |
| 48 | 16 | reserved zero |

Kinds are 1 text, 2 read-only data, 3 data, and 4 BSS. Text is nonempty,
instruction-aligned, file-backed, and canonically encoded through the shared ISA
codec. Read-only data and data are file-backed with matching logical and file
sizes. BSS has a nonzero logical size and no file bytes. Alignments are nonzero
powers of two; text alignment is at least the architecture instruction
alignment. Names are nonempty, unique UTF-8 strings without NUL.

## Symbols

Each symbol record is 32 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | name string offset |
| 4 | 1 | kind |
| 5 | 1 | binding |
| 6 | 2 | reserved zero |
| 8 | 2 | section index or `0xffff` |
| 10 | 2 | reserved zero |
| 12 | 4 | reserved zero |
| 16 | 8 | value |
| 24 | 8 | size |

Symbol kinds are 0 undefined, 1 absolute, and 2 section-defined. Bindings are
0 local and 1 global. Undefined symbols are global-only and have no section,
value, or size. Absolute symbols have no section and fit the selected address
width. Section symbols have a valid section and a checked value/size range.
Global names are unique. The optional entry symbol is section-defined in text,
instruction-aligned, and points to a complete instruction.

## Relocations

Each relocation record is 32 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | symbol index |
| 4 | 2 | target section index |
| 6 | 1 | kind |
| 7 | 1 | reserved zero |
| 8 | 8 | section-relative target offset |
| 16 | 8 | signed addend |
| 24 | 8 | reserved zero |

Kinds are:

1. absolute 32-bit word in initialized data;
2. absolute 64-bit word in initialized LZ64 data;
3. PC-relative 32-bit word in initialized data;
4. PC-relative branch in a `BR` or `CALL` instruction;
5. immediate in an `LI` instruction; and
6. memory displacement in `LDZ`, `LDS`, or `ST`.

Relocations may refer to undefined global symbols, but not undefined local
symbols. They cannot target BSS, overlap an incomplete instruction, or use an
opcode incompatible with their kind. Records are grouped by target section and
strictly ordered by target offset.

For evaluation, let `S` be the linked runtime address of the symbol, `A` the
record addend, and `P` the runtime address of the relocated field or
instruction. The linker writes:

- `AbsoluteWord32`: `S + A` as a 4-byte value;
- `AbsoluteWord64`: `S + A` as an 8-byte value;
- `PcRelativeWord32`: `S + A - P` as signed 32-bit data;
- `PcRelativeBranch`: `I = (S + A - (P + 8)) / 4`, requiring four-byte
  divisibility and a signed 32-bit instruction immediate;
- `LiImmediate`: `S + A` as a signed 32-bit immediate; and
- `MemoryDisplacement32`: `S + A` as a signed 32-bit memory displacement.

The linked runtime bases are `0x00200000` for merged text and `0x00300000` for
merged data. The linker patches the existing canonical instruction through the
shared ISA codec; it never masks or truncates an out-of-range value.

## Debug metadata

A debug source record is 16 bytes: a 4-byte string offset, a 4-byte source byte
length, and 8 reserved zero bytes. A debug mapping is 24 bytes: a 2-byte text
section index, 2 reserved bytes, an 8-byte instruction offset, and three 4-byte
fields for source index, source offset, and source length.

Mappings are strictly ordered, refer to complete text instructions, and remain
within the referenced source length. Paths are logical source names, not host
paths. Source text and line maps are not duplicated into the object; consumers
recover them through the shared `SourceManager`.

## Compatibility and ownership

The `.lzo` model is owned by `lazalith-toolchain`; the kernel never depends on
the toolchain. The compatibility bridge accepts only a single text object
without relocations and emits the existing `.lzx` v1 container. The general
linker resolves symbols, lays out aligned sections, evaluates the relocation
formulas above, strips object-only metadata, and constructs the runtime
executable.
