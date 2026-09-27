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
| 8 | 2 | format version | `2` |
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
| 44 | 4 | section table size | section count multiplied by `48` |
| 48 | 8 | payload offset | eight-byte-aligned end of the section table |
| 56 | 4 | debug block offset | after the last section, or `0` when there is none |
| 60 | 4 | debug block size | `0` when there is none |

The loader accepts a maximum file size of 4 MiB. The payload begins at the
declared payload offset; section file offsets must lie within the payload and
their ranges must not overlap.

### What version 2 changed

Version 1 spent eight bytes at offset 44 on a *section table offset* that the
header already implied: the table starts where the header ends, and the reader
was required to check that the stored value was exactly `64`. Version 2 uses
those eight bytes for the debug block's offset and length, which is a fact the
reader could not previously learn from the file.

The two fields are 32-bit because the header is a fixed 64 bytes and the file is
capped at 4 MiB, so an offset of eight bytes would be wider than anything the
format can address. A version-1 image read by a version-2 reader is rejected
with `UnsupportedFormat` rather than misread: the two formats disagree about
what those bytes mean, and guessing which one a file is would be exactly the
kind of silent corruption this format is built to avoid.

## Debug block

An image built with debug information carries one `LZXDBG01` block after its
last section. An image built without one has both header words zero, and the
reader treats that as "no block" rather than as an empty table at some offset.

The block holds the source text each mapping is an offset into, and one entry
per run of instructions: an address, a source index, and a byte range in that
source. The source text travels with the mappings because a mapping is an
offset, and an offset into text that is not there resolves to whatever line
happens to be at the same number. A debugger handed only the offsets would have
to open files on the host, which is precisely what fails when a crash is
reproduced somewhere the source is not.

A block that does not start with the magic, names a version this build does not
read, promises more files or mappings than the bytes hold, has a mapping that
reaches past the end of its source or names a source it does not carry, or has
bytes after everything it declared, is rejected. The failure is reported as
`LzxError::DebugBlock` and says which of those it was; the image is not loaded
with half a mapping table.

An image with no block is a complete image, not a degraded one. A debugger
loaded with it answers "no source location" and sets no source breakpoints,
and the program runs exactly as it would without a debugger.

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

The v2 format has no implicit entry decoding, relocation, dynamic linking,
compression, or symbol table. Those features require an explicit future format
revision. The one piece of metadata it does carry is the debug block described
above, which the Step 76 linker builds: it merges every object's source text and
mappings, and rewrites each mapping's address from the object's own section
offset to the address that code ended up at in this image. A mapping left
object-relative would still round-trip perfectly and still resolve to whichever
function happened to be laid out at that address, which is the failure this
table exists to prevent.
