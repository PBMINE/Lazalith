# Lazalith `.lzx` v1

`.lzx` v1 is the bounded native executable container consumed by the LazOS
loader. It is deliberately small and has no compression, relocation, symbol, or
debug extensions. Later object/executable work may add a separate versioned
container; it must not reinterpret v1 bytes.

All integers are little-endian. Offsets are absolute file offsets unless a
field is explicitly named `virtual_offset`. A file is invalid if any reserved
field is nonzero, any range is outside the file, or any declared section is
not consumed by the v1 layout.

## Header

The fixed header is 64 bytes:

| Offset | Size | Field | Value or rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | ASCII `LZXLOAD1` |
| 8 | 2 | format version | `1` |
| 10 | 2 | header size | `64` |
| 12 | 1 | architecture | `1` LZ32, `2` LZ64 |
| 13 | 1 | header flags | `0` |
| 14 | 2 | ISA version | `1` |
| 16 | 2 | OS ABI version | `1` |
| 18 | 2 | section count | `1..=3` |
| 20 | 2 | entry section | `0` |
| 22 | 4 | entry offset | instruction-aligned offset in code |
| 26 | 2 | reserved | `0` |
| 28 | 8 | required data bytes | at least the end of data/BSS, at most User data size |
| 36 | 8 | required stack bytes | exactly the fixed User stack size |
| 44 | 8 | section table offset | `64` |
| 52 | 4 | section table size | section count multiplied by `48` |
| 56 | 8 | payload offset | eight-byte-aligned end of the section table |

The loader accepts a maximum file size of 4 MiB. The payload begins at the
declared payload offset; section file offsets must lie within the payload and
their ranges must not overlap.

## Section table

Each section entry is 48 bytes:

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 1 | kind: `1` code, `2` data, `3` BSS |
| 1 | 1 | permissions |
| 2 | 2 | reserved, zero |
| 4 | 8 | virtual offset |
| 12 | 8 | virtual size |
| 20 | 8 | file offset |
| 28 | 8 | file size |
| 36 | 8 | alignment |
| 44 | 4 | reserved, zero |

Sections are ordered code, optional data, optional BSS. Code is mandatory and
must be first. The code section has virtual offset zero, alignment four, and
permissions `0x0d` (read, execute, User). Its file size equals its virtual size,
is nonempty, and fits the fixed User code region. Data and BSS use permissions
`0x0b` (read, write, User), fit the fixed User data region, and have aligned
virtual offsets. Data file size equals virtual size and may be empty; BSS has
no file bytes and must have a nonzero virtual size. Data and BSS virtual ranges
cannot overlap.

The entry offset is relative to the code section, is four-byte aligned, and
must leave a complete eight-byte instruction inside the section. The loader
rejects unsupported architecture, ISA, ABI, permissions, flags, versions,
ranges, requirements, and section layouts before constructing a process.

## Loader behavior

The parser copies only validated section payloads. The loader then constructs
an unpublished `Process`, loads code through `ProgramImage`, applies data or
zeroed BSS to the process-owned User memory, and advances the process heap past
the declared data requirement. Any failure drops the unpublished process;
malformed input cannot partially publish a process.

The v1 format has no implicit entry decoding, relocation, dynamic linking,
compression, symbol table, or debug metadata. Those features require an
explicit future format revision. Because of that, the Step 48 linker resolves
relocations and then drops object debug mappings: the linked `.lzx` carries no
debug section, and `LinkedProgram` exposes no debug API. Debug mappings remain
available on the `.lzo` object, which is the artifact a debugger should read.
