# Lazalith ISA v1 — Design Draft

## Status and scope

This is the Step 8 design contract, not an implemented ISA or a frozen binary
compatibility promise. [LZ32](lz32.md) and [LZ64](lz64.md) instantiate this one
custom architecture. Decisions here are firm inputs to Steps 9–10; later design
changes must update all three documents together. No CPU, instruction metadata,
encoder, decoder, register file, memory, trap controller, or OS ABI is implemented
by this step. Instruction metadata starts at Step 11.

The design uses ordinary registers, a separate control state, one fixed encoding,
and explicit checked control/address operations. It does not inherit another
ISA's encoding or ABI. Floating point, vectors, atomics, paging, caches,
multiprocessor ordering, and optional ISA extensions are outside v1.

## Architectural state and mode

Let `W` be the word width in bits, `B = W / 8`, and `M = 2^W - 1`.

| Property | LZ32 | LZ64 |
| --- | --- | --- |
| General registers | 16 ordinary `r0`–`r15` | Same |
| Register / pointer / address bits | 32 / 32 / 32 | 64 / 64 / 64 |
| PC and SP bits | 32 | 64 |
| Instruction length / alignment, bytes | 8 / 4 | 8 / 4 |
| Data sizes and their natural alignments, bytes | 1, 2, 4 | 1, 2, 4, 8 |
| Stack alignment, bytes | 4 | 8 |
| Required feature set | `BaseInteger` | `BaseInteger` |
| Byte order | Little endian | Little endian |

`r0` is writable, not a zero register. No general register aliases PC, SP, or
status. All 4-bit register fields name ordinary registers, including `r15`.
PC names the first byte of the current instruction. SP points to the current
stack top; an empty stack's initial SP is a representable, word-aligned address
chosen by platform setup. There is no representable one-past-maximum address.
Mode is fixed by machine configuration, not switchable by an instruction.

Status is a word-sized bitset: bit 0 `N` (negative), bit 1 `Z` (zero), bit 2 `C`
(add carry / subtract borrow), bit 3 `V` (signed overflow), bit 4 `IE` (external
interrupt enable), bit 5 `U` (`1` User, `0` Supervisor). Bits `W-1:6` are reserved
zero. Arithmetic changes only `N/Z/C/V`. Ordinary status reads are allowed;
ordinary status writes are not. Supervisor may change IE using `EI`/`DI` and
return to a validated saved status using `RFE`. No arithmetic changes privilege.

## Encoding

Every instruction is exactly eight bytes, including in LZ32. Read those bytes as
one little-endian 64-bit encoding, independently of guest data-word width. Bit 0
is the least significant bit of the first byte. PC must be divisible by four,
not necessarily eight; the eight-byte fetch is not an eight-byte data access.
There are no prefixes, delay slots, compressed forms, or implicit extensions.

| Bits, inclusive | Field | Meaning |
| --- | --- | --- |
| 7:0 | OP | Unsigned 8-bit opcode |
| 11:8 | D | 4-bit destination register, or store source |
| 15:12 | A | 4-bit first source / base register |
| 19:16 | BREG | 4-bit second source register |
| 23:20 | X | 4-bit size, condition, or control selector |
| 31:24 | R | Always reserved zero |
| 63:32 | I | Signed 32-bit two's-complement immediate when used |

The opcode selects the format; there is no separate format field. In the table
below every field not listed under “Used fields” MUST be zero, including `I`.
A used register field containing zero means `r0`, not an omitted operand.
Selectors are unsigned enumerations, not signed immediates.

| Format | Used fields in addition to OP | Assembly operand order |
| --- | --- | --- |
| Z | None | None |
| D | D | `rd` |
| A | A | `ra` |
| DA | D, A | `rd, ra` |
| DAB | D, A, BREG | `rd, ra, rb` |
| AB | A, BREG | `ra, rb` |
| DI | D, I | `rd, imm32` |
| DAI | D, A, I | `rd, ra, imm32` |
| MEM | D, A, X, I | `rd_or_source, [ra + disp32], size` |
| BR | X, I | `condition, disp32` |
| IMM | I | `imm32` (CALL displacement or TRAP payload) |
| DX | D, X | `rd, control` |
| AX | A, X | `control, ra` |

Unallocated opcodes, nonzero reserved/unused fields, and invalid selectors are
`IllegalInstruction`. A defined eight-byte data size in LZ32 instead faults as
`InvalidWidth`. There is exactly one canonical encoding per instruction and
operand tuple. Assemblers must reject out-of-range immediates, never silently
truncate them. Every encoded immediate is signed; logical immediate forms are
intentionally absent. Mnemonics below describe semantics, not a completed
assembler grammar.

## Opcode allocation

All numbers in the OP column are hexadecimal. All listed operations belong to
`BaseInteger`; privilege is a runtime permission, not a feature bit. Operands
are read from pre-instruction state, so source/destination aliasing is allowed.
Unless specified otherwise, successful instructions set PC to checked `nextPC`,
preserve SP and status, and only write their named destination.

| OP | Mnemonic | Format | Result / action | NZCV |
| --- | --- | --- | --- | --- |
| 00 | NOP | Z | No data effect | Preserve |
| 01 | MOV | DA | `rd = ra` | Preserve |
| 02 | LI | DI | `rd = sign_extend(I, 32, W)` | Preserve |
| 03 | GETPC | D | `rd = current PC`, not nextPC | Preserve |
| 04 | GETSP | D | `rd = SP` | Preserve |
| 05 | SETSP | A | `SP = ra`, require word alignment | Preserve |
| 06 | GETSTATUS | D | `rd = status` | Preserve |
| 10 | ADD | DAB | `rd = ra + rb` modulo `2^W` | Add rules |
| 11 | ADDI | DAI | ADD with sign-extended I | Add rules |
| 12 | SUB | DAB | `rd = ra - rb` modulo `2^W` | Sub rules |
| 13 | SUBI | DAI | SUB with sign-extended I | Sub rules |
| 14 | MUL | DAB | Low W bits of `ra * rb` | NZ; C=V=0 |
| 15 | DIVU | DAB | Unsigned quotient | NZ; C=V=0 |
| 16 | DIVS | DAB | Signed quotient, toward zero | NZ; C=V=0 |
| 17 | REMU | DAB | Unsigned remainder | NZ; C=V=0 |
| 18 | REMS | DAB | Signed remainder, dividend's sign or zero | NZ; C=V=0 |
| 19 | CMP | AB | SUB flags, no register result | Sub rules |
| 20 | AND | DAB | Bitwise AND | NZ; C=V=0 |
| 21 | OR | DAB | Bitwise OR | NZ; C=V=0 |
| 22 | XOR | DAB | Bitwise XOR | NZ; C=V=0 |
| 23 | NOT | DA | Word-width complement | NZ; C=V=0 |
| 24 | SHL | DAB | Logical left shift by `rb mod W` | NZ; C=V=0 |
| 25 | SHR | DAB | Logical right shift by `rb mod W` | NZ; C=V=0 |
| 26 | SAR | DAB | Arithmetic right shift by `rb mod W` | NZ; C=V=0 |
| 30 | LDZ | MEM | Load size bytes, zero-extend to W | Preserve |
| 31 | LDS | MEM | Load size bytes, sign-extend from `8*size` to W | Preserve |
| 32 | ST | MEM | Store low `8*size` bits of register D | Preserve |
| 40 | BR | BR | Conditional PC-relative branch | Preserve |
| 41 | JMP | A | Absolute branch to ra | Preserve |
| 42 | CALL | IMM | Push nextPC, PC-relative call | Preserve |
| 43 | CALLR | A | Push nextPC, absolute call to ra | Preserve |
| 44 | RET | Z | Pop return PC | Preserve |
| 50 | SYSCALL | Z | Synchronous syscall trap | Preserve until entry |
| 51 | TRAP | IMM | Synchronous software trap, signed I payload | Preserve until entry |
| 52 | HALT | Z | Supervisor only; PC=nextPC, enter Halted | Preserve |
| 53 | RFE | Z | Supervisor only; return from active trap | Restore saved control state |
| 54 | EI | Z | Supervisor only; IE=1 | Preserve |
| 55 | DI | Z | Supervisor only; IE=0 | Preserve |
| 56 | CSRR | DX | Supervisor only; rd=selected control | Preserve |
| 57 | CSRW | AX | Supervisor only; selected control=ra | Preserve |

`HALT` is terminal until an explicit machine reset; it is not wait-for-interrupt.
Interrupts do not wake it. There is no separate PUSH/POP: software uses GETSP,
SETSP, and ordinary loads/stores. These multi-instruction sequences are not
collectively atomic. Full-width LZ64 constants may be loaded from memory or
constructed using LI, shifts, and logical operations; no 64-bit immediate form
is implied by the eight-byte instruction size.

MEM size X: `0` = 1 byte, `1` = 2 bytes, `2` = 4 bytes, `3` = 8 bytes (LZ64
only); `4..15` invalid. Data alignment equals size, including signed loads.
No instruction operates on a partial general register: loads extend, stores
truncate, and arithmetic operates at W.

BR condition X, evaluated using pre-instruction flags:

| X | Name | Predicate |
| --- | --- | --- |
| 0 | AL | Always |
| 1 | EQ | Z |
| 2 | NE | not Z |
| 3 | ULT | C (borrow after CMP/SUB) |
| 4 | UGE | not C |
| 5 | ULE | C or Z |
| 6 | UGT | not C and not Z |
| 7 | SLT | N != V |
| 8 | SGE | N == V |
| 9 | SLE | Z or (N != V) |
| 10 | SGT | not Z and (N == V) |
| 11 | VS | V |
| 12 | VC | not V |
| 13 | MI | N |
| 14 | PL | not N |
| 15 | — | Invalid |

Comparison predicates describe flags typically produced by CMP; BR does not
remember which instruction last wrote flags.

## PC, control flow, and stack

For every successfully fetched, canonically decoded, permitted instruction,
first calculate `nextPC = currentPC + 8` in a checked address operation. If it
exceeds M, raise `AddressOverflow` before any instruction effect, even for JMP,
RET, RFE, HALT, or a taken branch. Never mask a PC increment. A fetch may fit at
the end of memory while its nextPC does not; that instruction cannot execute.

Taken BR and CALL use the mathematical signed sum:

```text
byte_displacement = signed_i32(I) * 4
target = nextPC + byte_displacement
```

Multiplication and addition occur without host or guest-width wrapping; validate
`0 <= target <= M` and four-byte alignment. Range is -8,589,934,592 through
+8,589,934,588 bytes from nextPC, identical in both modes. An untaken BR only
uses nextPC: its hypothetical target is not calculated or faulted. `I=0` targets
nextPC; `I=-2` targets the current instruction; `I=-1` targets currentPC+4.
Four-byte alignment intentionally allows a target in the second half of another
encoding; it decodes the eight bytes starting there, not an instruction boundary
map. Program layout should normally avoid overlapping instructions.

JMP/CALLR use the entire unsigned word in ra as an absolute byte address. All
selected branch/call/return targets are width- and alignment-checked before
commit. Mapping, execute permissions, and the full target fetch are checked on
the subsequent fetch, not speculatively by the branch. Thus a call to aligned
unmapped memory commits its stack push; the next fetch traps at the target.

The stack grows toward lower addresses and SP is always B-byte aligned:

- CALL/CALLR calculate `newSP = SP - B` with checked address arithmetic. Validate
  target, newSP, and the entire writable RAM stack word before mutation. Commit
  the little-endian W-bit nextPC to `[newSP]`, SP=newSP, and PC=target together.
- RET validates a readable RAM word at SP and checked `newSP = SP + B`, reads the
  proposed target without side effects, and validates its width/alignment.
  Commit PC=target and SP=newSP together; the popped memory bytes are unchanged.
- SETSP validates word alignment; it does not touch memory or require SP to be
  mapped. Later stack accesses validate their own ranges and permissions.
- Stack CALL/RET accesses to ROM or MMIO fault with a permission/access-policy
  fault. No device side effects may occur while validating a return target.

A fault leaves PC, SP, general registers, flags, and memory exactly as before the
instruction; only the subsequent trap-entry transition may change CPU control
state. CALL has no link register and RET has no implicit argument cleanup.

## Memory and precision

v1 is a single-core, byte-addressed, flat address model. Pointer, virtual,
physical, and instruction addresses have W significant bits, but their strong
types remain distinct. There is no paging or implicit type conversion: any
identity mapping belongs to an explicit address-space/bus boundary. Mapping
size is independent of the architectural maximum. Accesses require mapped
regions with read, write, execute, and User/Supervisor permission checks;
Supervisor does not bypass region read/write/execute permissions.

For MEM, `EA = unsigned_word(ra) + signed_i32(I)` is a mathematical sum in bytes,
not scaled. Reject negative or greater-than-M results as AddressOverflow. Also
check the inclusive final address `EA + size - 1`; this permits a one-byte data
access at M but never a multi-byte wrap. Then check natural alignment, mapping,
and permission for the complete access. Arithmetic ADD wrapping is deliberately
different from effective-address calculation. Memory APIs may not silently
apply an address mask.

Fetch is an eight-byte, four-aligned execute access, not LDZ with size 8. All
fetch bytes must be available with execute permission for the current privilege;
read permission is separate. Fetch and debugger peek have no device side
effects. v1 forbids executable MMIO. Stores are visible to subsequent fetches;
there is no guest cache synchronization instruction. CPU accesses are observed
in instruction order, and an interrupt is never delivered mid-instruction.

Future MMIO reads/writes must validate the entire transaction before any device
side effect; a successful read cannot be followed by a late instruction fault.
A device operation unable to guarantee success-or-no-effect must be rejected,
not partially performed. Cross-region operations are rejected in v1, even when
both regions separately permit access. This is not a bus implementation mandate
for Step 8, but a precision requirement for later memory/bus work.

Deterministic fault priority is: current-PC width/alignment and full fetch
(range, mapping, permissions), canonical decode/selectors, privilege, checked
nextPC, then instruction-specific validation. Within data accesses check width,
base/result/end range, alignment, mapping, permissions, then transaction success.
A mode-unsupported MEM size faults in the decode/selector stage. For CALL check
target range/alignment before stack subtraction/access; for RET check newSP
range before stack access, then loaded-target range/alignment. No writes occur
until every required validation and fallible read has succeeded.

## Shared width semantics — Step 10 contract

These are pure, centralized operations, not per-opcode implementations. Values
may be held in `u64` containers; that does not make LZ32 a 64-bit architecture.
For all word arithmetic inputs first retain their low W bits. Interpret signed
values as two's complement at W, never at the host's native width.

| Operation | Required result |
| --- | --- |
| `mask(W)` | M, including `u64::MAX` for W=64; never evaluate `1u64 << 64` |
| `truncate(value, W)` | `value & M`; upper host-container bits zero |
| `zero_extend(value, source_bits, W)` | Keep low source_bits, fill higher bits through W with zero |
| `sign_extend(value, source_bits, W)` | Keep low source_bits, replicate bit source_bits-1 through W-1; host bits above W zero |
| `wrapping_add/sub/mul` | Mathematical result modulo `2^W` |
| `shift_amount(value, W)` | Low-W unsigned value modulo W, range 0..W-1 |
| `mask_address_bits(value, W)` | Explicit bit utility equal to truncate; NOT validation or translation |
| `validate_address(value, W)` | Accept unchanged iff `value <= M`; otherwise structured width/range error |
| `checked_address_offset(base, delta, W)` | Validate base first, then accept mathematical base+signed delta iff in 0..M |
| `checked_access_end(base, size, W)` | Validate base and positive size, then check base+size-1 without wrapping |

Extension source_bits must be one of 8, 16, 32, 64 and no greater than W; invalid
source widths return a structured error rather than panic or truncate silently.
Extension of a full-width source is identity after masking. Extensions ignore
container bits above the declared source width: `sign_extend(0x180, 8, 32)` is
`0xffffff80`, not an interpretation of bit 8. Immediate extension always declares
source_bits=32. Memory size support remains a separate ArchitectureConfig query.

An arithmetic result concept contains `value`, `negative`, `zero`, `carry`, and
`overflow`; helpers do not mutate a StatusRegister. `negative` is result bit
W-1 and `zero` means the truncated result is zero. ADD/ADDI use C=true iff the
unsigned sum exceeds M; SUB/SUBI/CMP use C=true iff unsigned left < unsigned
right (borrow, NOT no-borrow). ADD signed overflow is equal-sign inputs with a
different-sign result; SUB signed overflow is different-sign inputs with a
result sign different from the left input. No carry-in or borrow-in is used.
MUL returns only the low word and deliberately clears C and V, even if the full
product does not fit. Signed and unsigned low-word multiplication are identical.

AND/OR/XOR/NOT, all shifts, and successful division/remainder set N/Z from the
result and clear C/V. Shifts never publish shifted-out carry; even a shift by
zero clears C/V and recomputes N/Z. SHL discards high bits, SHR fills with zeros,
and SAR fills with the original word sign bit. Normalize amounts before shifting
to avoid host shifts by 32/64 and host-dependent signed-shift behavior.

DIVU/REMU use unsigned inputs. DIVS/REMS interpret inputs at W; quotient truncates
toward zero, and `a = q*b + r` with `abs(r) < abs(b)`. All four trap on zero
divisor. Both signed operations trap on `MIN_W / -1`, including REMS: do not
return zero for that pair. Traps return no arithmetic result and change no flags
or destination. Signed overflow on ordinary ADD/SUB is a flag, not a trap.

Checked address operations must support all u64 bases and the full scaled i32
branch displacement without overflow in intermediate calculations. An `i128`
mathematical intermediate is sufficient; casting a high LZ64 address to i64 is
not valid. A positive/negative checked-u64 decomposition is also acceptable.
Preserve structured error kind and rejected operands/width; guest input must not
panic. Keep existing strong address-domain wrappers intact.

### Step 10 Rust API mapping

`lazalith-types` implements these operations as methods on `WordWidth` in
`src/width.rs`; `ArithmeticResult` and `WidthError` are re-exported at the crate
root. All methods take `self` by value. Word operands and results use `u64`.

| Contract | Methods and return types |
| --- | --- |
| Bit utilities | `mask()`, `truncate(value)`, `mask_address_bits(value)` → `u64` |
| Extensions | `zero_extend(value, source_bits: u8)`, `sign_extend(value, source_bits: u8)` → `Result<u64, WidthError>` |
| Value-only wrapping | `wrapping_add(left, right)`, `wrapping_sub(left, right)`, `wrapping_mul(left, right)` → `u64` |
| Flagged arithmetic | `add(left, right)`, `sub(left, right)`, `mul(left, right)` → `ArithmeticResult` |
| Flagged logic | `bitand(left, right)`, `bitor(left, right)`, `bitxor(left, right)`, `not(value)` → `ArithmeticResult` |
| Shifts | `shift_amount(value)` → `u8`; `shl(value, amount)`, `shr(value, amount)`, `sar(value, amount)` → `ArithmeticResult` |
| Division/remainder | `div_unsigned(left, right)`, `rem_unsigned(left, right)`, `div_signed(left, right)`, `rem_signed(left, right)` → `Result<ArithmeticResult, WidthError>` |
| Checked addresses | `validate_address(value)`, `checked_address_offset(base, delta: i64)`, `checked_access_end(base, size: u64)` → `Result<u64, WidthError>` |

`ArithmeticResult` exposes `value`, `negative`, `zero`, `carry`, and `overflow`.
There is no flag input or mutation. Extension errors retain the original value,
source bits, and width. Division errors distinguish zero from signed overflow
and retain both original, unmasked operands and width, including when masking
made the divisor zero. Division and remainder share those error variants.

Address errors distinguish standalone range rejection, invalid offset/access
base, offset/end range rejection, and zero size. Every variant retains all
original inputs and width; base rejection takes priority, even if a negative
delta would repair it or size is zero. Offset calculation uses `i128`; access
end uses `u128` and returns the inclusive last address, not an exclusive end.
These checks impose only width/range constraints, not alignment, mapping,
permissions, or supported memory sizes. Existing address-domain wrappers are
unchanged; callers explicitly extract and reconstruct their own domain.
`WidthError` implements `Display` and `core::error::Error`; it is not guest trap
delivery. Step 11 instruction metadata and CPU/status state remain separate.

## Step 11 Rust instruction API

`lazalith-isa` now implements metadata and canonical encoding/decoding only.
The design sections remain the architectural contract, not CPU execution claims.
The library is `no_std`, allocation-free, and depends only on local
`lazalith-types`; local diagnostics is a test-only dependency. No SDL or external
packages are used. No register file, execution, assembler grammar, disassembler,
trap delivery, address validation, or status mutation is implemented here.

### Shared definitions

- `Opcode::ALL`, `Opcode::try_from(u8)`, `as_u8()`, and `definition()` expose all
  40 allocated opcodes. Rust names use `Nop`, `Getpc`, `Csrr`, etc.; mnemonics
  retain the uppercase spellings in the allocation table.
- `InstructionDefinition` exposes opcode, mnemonic, format, `supervisor_only`,
  `NzcvEffect`, and `ImmediateMeaning`. `required_features()` returns
  `FeatureSet::base_v1()`; `operands()` returns the format's static operand layout.
  A single opcode macro defines the enum, raw lookup, enumeration, and metadata.
- `InstructionFormat::ALL` contains the 13 formats; Rust multi-letter names use
  `Da`, `Dab`, `Di`, `Dai`, `Mem`, `Br`, `Imm`, `Dx`, and `Ax`.
  Each format supplies ordered `OperandDefinition { kind, fields }` data through
  `operands()`. `EncodingField` supplies bit shifts/masks; `used_mask()` includes
  OP and all operand fields. Its complement is the reserved-zero mask, including R.
  Encoder, decoder, and constructor validation consume these same layouts rather
  than independent opcode/format encoding switches.
- `OperandKind` distinguishes Register, Immediate, Memory, DataSize, Condition,
  and Control. MEM is three operands: register D, a composite base/displacement
  memory operand, then size. AX is control then register A, not register first.
- `ImmediateMeaning` distinguishes signed word constants, byte displacements,
  relative displacements, trap payloads, and absence. Relative values remain
  signed i32 units of four bytes; codecs neither scale nor resolve addresses.
  `NzcvEffect::Preserve` for SYSCALL/TRAP describes the instruction before entry;
  `Restore` describes RFE. These are metadata, not flag implementations.

### Structured operands and instructions

`Operand` has variants `Register(RegisterIndex)`, `Immediate(i32)`,
`Memory { base: RegisterIndex, displacement: i32 }`, `DataSize(DataSize)`,
`Condition(Condition)`, and `Control(ControlRegister)`. Registers reuse the
validated Step 7 type; every encoded nibble including zero names an ordinary
register. `kind()` identifies the operand variant. For wider parser inputs,
`try_immediate(i64)` and `try_memory(RegisterIndex, i64)` reject values outside
signed i32 instead of truncating. Already-i32 inputs need no further range check.

`DataSize`, `Condition`, and `ControlRegister` are strong selector enums with
`ALL`, `TryFrom<u8>`, and `as_u8()`. DataSize Byte/Half/Word/Double maps to
selectors 0/1/2/3 and `bytes()` 1/2/4/8. Condition Al through Pl follows the exact
condition table. Control Tvec/Epc/Esp/Estatus/Tcause/Tpayload follows the control
table, with `is_writable()` and `requires_active_frame()` queries. These queries
are metadata only: CSRW to a read-only selector is canonically valid and must
later raise runtime InvalidControlState, not IllegalInstruction.

```rust
Instruction::new(config: ArchitectureConfig, opcode: Opcode, operands: &[Operand])
    -> Result<Instruction, InstructionError>
encode(config: ArchitectureConfig, instruction: &Instruction)
    -> Result<[u8; 8], InstructionError>
decode(config: ArchitectureConfig, input: &[u8])
    -> Result<Instruction, DecodeError>
```

Instruction stores private fixed-capacity operands and exposes only `opcode()`,
`definition()`, `operands()`, and `validate(config)`. Construction checks count,
then operand kind and mode-supported size in operand order before copying.
Encoding revalidates mode: an LZ64 eight-byte MEM instruction cannot be encoded
under LZ32. Instructions contain no mode or machine state. Both codecs are pure;
encode returns a new array, and decode accepts exactly eight bytes (trailing
bytes are rejected, not silently consumed). Fetching, stream slicing, PC width,
PC alignment, privilege, and checked nextPC belong to later layers.

### Error contract

All errors implement `Display` and `core::error::Error`, with typed causes
accessible via `source()`; no separate diagnostic renderer is introduced.
`Diagnostic::with_cause` can retain the complete decode → instruction validation
→ validation-reason chain. The integration test's E1101 is a test code, not an
allocated guest trap number or frozen diagnostics-code contract.

- `UnknownOpcode` retains the rejected byte.
- `OperandError::InvalidSelector` retains kind and raw selector;
  `ImmediateOutOfRange` retains the original i64 and `TryFromIntError` cause.
- `InstructionError` retains opcode and a `ValidationError`: wrong count
  (expected/actual), wrong kind (index, expected kind, actual operand), or
  InvalidWidth (index, DataSize, WordWidth).
- `DecodeError` distinguishes length, unknown opcode, reserved bits, operand
  selector, register conversion, and instruction validation. Every eight-byte
  failure retains the full original array; reserved-bit errors additionally
  retain opcode and the complete nonzero reserved mask. Length errors retain
  actual length. Register conversion retains `InvalidRegisterIndex`; that
  defensive path cannot be reached with the current four-bit register fields.

Decode precedence is exact length → allocated opcode → all reserved-zero bits
→ selectors in operand order → mode size validation. Unknown opcodes, reserved
bits, and invalid selectors correspond to the future IllegalInstruction fault;
Validation/InvalidWidth corresponds to InvalidWidth. Length is a caller/fetch
boundary error rather than a decoded guest instruction. Privilege and control
state checks are intentionally absent, preserving the design's runtime priority.

### Executable coverage

The 14 ISA integration tests check all 40 opcode rows directly against this
file, all 13 format masks and field non-overlap, exact published bytes, and
independently constructed expected bytes for both-mode roundtrips. Roundtrips
cover every register tuple (including aliases and r0/r15), every legal selector,
and nine signed immediate values including MIN, MIN+1, -4, -2, -1, 0, 1,
0x12345678, and MAX. This is exhaustive opcode/format/register/selector coverage,
not enumeration of all 2^32 immediate patterns or all 2^64 byte strings.

Malformed tests cover every unallocated opcode, each reserved bit of each opcode,
combined reserved masks and selector-error precedence, all raw u8 selector
conversions, all encoded selector nibbles, both-mode MEM widths, wrong lengths,
wrong operand counts/kinds, i64 immediate rejection boundaries, mode revalidation,
input immutability, exact selector names/values, error formatting, and retained
cause chains through shared diagnostics. Runtime architectural examples below
remain future CPU/memory tests. The flake includes this file in its source filter
because the opcode contract test uses `include_str!`.

## Step 12 register file mapping

`lazalith-cpu` implements the register file as `RegisterFile` in
`src/registers.rs`. It stores a private `[u64; 16]` plus the configured
`WordWidth` from its `RegisterFile::new(config)` constructor. Reads and
writes take `RegisterIndex`; writes truncate with the shared `WordWidth::truncate`
contract. `read_raw(u8)`/`write_raw(u8, u64)` reject indices outside 0..16 with
the retained-input `InvalidRegisterIndex` error and mutate nothing. All sixteen
registers including `r0` are ordinary writable registers; PC, SP, and status are
separate architectural state and are never aliases of any register file slot.

## Step 13 state API mapping

`ArchitecturalState::new(config, pc: InstructionAddress, sp: VirtualAddress,
status: u64)` is fallible and starts general registers at zero. Private state is
read via `config`, `registers`, `pc`, `sp`, `status`, and `privilege` getters.
`write_register`/`write_register_raw` delegate to the width-fixed register file.
`set_pc`, `set_sp`, and `restore_control(pc, sp, status)` validate before mutation;
restoration checks PC, SP, then status and never changes general registers.
Free `validate_pc(config, pc)`/`validate_sp(config, sp)` expose the same width-first,
alignment-second checks without mutation. Errors retain rejected input and causes.
These are host state-management APIs, not permission-checked guest opcodes or
CSRW implementations; memory mapping, fetch, and trap-frame ownership remain
outside them. `ExecutionState::Running/Halted` and `DebugState { single_step }`
are separate values. A minimal `StatusRegister::try_from_bits(width, input)`
scaffold validates exact reserved-zero bits and exposes `bits()`/`privilege()`;
Step 14 completes flag operations without changing this representation.

## Step 14 status API mapping

`StatusRegister` now provides `new(Privilege, interrupts_enabled: bool)` with
NZCV clear, `negative/zero/carry/overflow/interrupts_enabled` queries,
`set_interrupts_enabled`, `set_privilege`, `update_arithmetic(ArithmeticResult)`,
and `matches(Condition)`. Arithmetic copies the four supplied flags, not the
result value, and preserves IE/U. Consume results from the configured shared
`WordWidth` helpers; a failed helper produces no status update. Conditions use
the shared ISA enum and the exact borrow predicates above. Status remains a
validated mode-independent six-bit value returned zero-extended as u64; raw
construction takes width for retained error context and rejects upper container
bits even in LZ32. `ArchitecturalState::update_arithmetic` and
`set_interrupts_enabled` delegate without exposing mutable status storage.
Control APIs are host operations, not guest permission checks.

## Step 15 outcomes and Step 16 integration contract

`lazalith-cpu` exports the following control-flow data (no interpreter):

```rust
pub enum ControlTarget {
    Absolute(InstructionAddress),
    Relative(i32),
}
pub enum TrapRequest { Syscall, Software(i32) }
pub enum ExecutionOutcome {
    Continue,
    Jump(ControlTarget),
    Call(ControlTarget),
    Return(InstructionAddress),
    Trap(TrapRequest),
    Halt,
}
```

Relative payloads are signed instruction immediates in four-byte units, not
pre-scaled bytes. Return carries a target already read without side effects from
the RAM stack. Trap requests cover successful SYSCALL/TRAP only; faults remain
structured errors, not invented controller events. Software payload retains the
original signed i32; eventual TPAYLOAD reads must sign-extend at guest width.

`checked_next_pc(config, pc)` validates PC and checked PC+8.
`checked_return_sp(config, sp)` validates SP and checked SP+B. Both return typed
addresses or `OutcomeErrorKind` with typed control/width causes. The interpreter
must call next-PC validation after fetch, canonical decode and privilege checks,
but before any instruction-specific work, including a RET stack read or RFE.
For RET, check return SP before performing the side-effect-free RAM word read;
then pass the loaded target to `Return`. Preparation repeats arithmetic checks
as a defensive boundary, not as a replacement for correct read/error ordering.

`prepare_outcome(&mut ArchitecturalState, &mut ExecutionState, outcome)` returns
`Result<PreparedOutcome<'_>, OutcomeError>`. It performs no mutation. Error context
retains original PC and the entire outcome, plus `Halted`, `PrivilegeViolation`,
`Width(WidthError)`, or `Control(ControlStateError)` with Error source chains.
Already-Halted execution is rejected first. HALT permission precedes nextPC.
Every outcome requires checked nextPC, including jumps, returns, traps and HALT.
CALL validates target before stack subtraction. RET checks stack addition before
loaded-target alignment. Absolute targets never mask; relative sums use shared
wide checked offsets. No target mapping/fetch is speculated. Untaken BR must
produce `Continue`, so no hypothetical target is calculated.

The prepared value holds exclusive borrows of state/execution: it cannot become
stale, be applied to another machine, or be committed twice. Dropping it changes
nothing. Its read-only `next_pc()`, `destination()`, and `stack_effect()` expose
calculated control data. `StackEffect` is either absent or one of:

```rust
Push { address: VirtualAddress, return_pc: InstructionAddress, size: DataSize }
Pop  { address: VirtualAddress, return_pc: InstructionAddress, size: DataSize }
```

Push describes SP-B and nextPC; Pop describes old SP and the loaded target.
Size is Word in LZ32, Double in LZ64. The CPU layer does not pretend a push/read
occurred. `PreparedOutcome::commit<E>(transaction: impl FnOnce(StackEffect)
-> Result<(), E>) -> Result<OutcomeApplication, E>` calls the transaction exactly
once for stack outcomes and never for others. On callback failure it preserves
CPU/execution state and returns the original typed E. After callback success,
control commit is infallible; no late PC/SP error can follow a memory write.

The caller-owned transaction MUST validate complete RAM access, permissions and
mode-sized little-endian encoding before any write; it must guarantee success
or no memory effect. For Pop, confirm the earlier validated read/target belongs
to this slot and transaction; never write popped bytes or read MMIO. Retain
exclusive memory ownership across read/prepare/commit. An arbitrary callback
cannot be made atomic by this crate: a no-op successful callback does not
implement CALL/RET, and an Err after modifying memory violates the contract.
Memory/bus enforcement belongs to later steps, not a fake Step 15 controller.

`OutcomeApplication::Continue` means PC was set to nextPC/selected target, with
SP changed only for Call/Return. `Halted` means PC=nextPC and execution=Halted.
Neither modifies general registers or status. `Trap { request, resume_pc }`
leaves the entire architectural state and execution unchanged, including PC;
resume_pc is nextPC, while the future controller must capture exact current PC
and the full pre-state. The owner must stop normal execution and hand off this
request, not fetch the same trap instruction again. No trap entry, frame, vector,
interrupt delivery, reset, or RFE execution exists in this crate yet.

### Step 16 implementation obligations

- Use shared decode/metadata, then runtime permission checks, checked nextPC,
  then operand/address validation and pure width arithmetic in that order.
- Read operands from the original state. Stage register/flag/control changes in
  a local `ArchitecturalState` clone (or a future equivalent pending-effects
  value); do not mutate live state and subsequently discover an outcome fault.
  Complete outcome preparation before any external side effect; after successful
  transaction/control commit publish the candidate infallibly. A failed candidate
  is discarded. Host setters are not guest permission checks.
- Use `registers().read/read_raw`, `write_register/write_register_raw`,
  `update_arithmetic`, and `status().matches(shared_condition)`. Width helpers
  supply results; CMP changes status only. Preserve flags for non-arithmetic
  instructions. Use the state config, never host integer width.
- General-register changes and external memory transactions must share one
  validate/calculate/commit instruction boundary. No fallible work may remain
  after a successful device/stack transaction. This crate cannot roll back
  earlier caller mutations or device effects.
- Host `restore_control` supplies atomic PC/SP/status validation for later RFE,
  but does not validate privilege/frame ownership or clear a frame. CSRR/CSRW
  selectors stay shared ISA metadata; controller-owned TVEC/resume fields are
  not invented as live CPU aliases. Trap/interrupt controllers remain Step 31.
- Keep `ExecutionState` and `DebugState` separate from guest snapshots. The owner
  provides explicit reset/lifecycle later; outcome application never wakes Halted.

Eight outcome integration tests exercise all variants, relative extrema and
high-half LZ64 against an i128 oracle, both-mode maximum PC/stack boundaries,
precise error priority and retained inputs, dropping plans, failed transactions,
actual mode-sized little-endian test-stack pushes and unchanged pops, trap
pre-state/resume values, and terminal HALT. These test state/control semantics,
not a memory bus, interpreter, or real trap delivery.

## Steps 16–17 reference interpreter and memory handoff

`lazalith-cpu::ReferenceInterpreter` owns private architectural and execution
state. `new(ArchitecturalState)` consumes validated host setup and starts Running.
`architectural_state() -> &ArchitecturalState` and `execution_state() ->
ExecutionState` are inspection only; there is no mutable state or array getter.
Prepare initial registers through ArchitecturalState before moving it into the
interpreter. Reset, trap delivery, and machine lifecycle remain later work.

All entry points return `Result<OutcomeApplication, CpuFault<M::Error>>`:

- `step<M: CpuMemory>(&mut self, memory: &mut M)` fetches at the current PC.
- `step_bytes<M: CpuMemory>(&mut self, bytes: &[u8], memory: &mut M)` accepts
  exactly eight bytes representing the current instruction. The caller supplies
  the current instruction, not an entire program or a PC-indexed byte array.
- `execute<M: CpuMemory>(&mut self, instruction: &Instruction, memory: &mut M)`
  revalidates a structured instruction under this CPU's configuration.

The last two are explicit injection paths: they check current-PC alignment/range
and the entire fetch address range but bypass fetch mapping/execute permissions.
Production machines should use `step`. A successful step returns Continue,
Halted, or a Trap event, not an instruction-count or cycle estimate. Stop ordinary
execution on a Trap event and hand its request/resume PC plus unchanged pre-state
to a future controller; repeatedly stepping the event is not trap delivery.

All 37 non-controller opcodes have execution coverage in both modes. RFE/CSRR/
CSRW return `UnsupportedUntilTrapController(Opcode)` after canonical validation,
privilege, and checked nextPC; they never access fake CSRs or restore a fake frame.
EI/DI and HALT are Supervisor-only. Errors preserve architectural/execution state.
`CpuFault<E>` retains PC, optional raw opcode (including unknown decode bytes), and
`CpuFaultCause<E>`. Source chains retain ISA, width, control, outcome, data-access,
and concrete memory errors. Fetch/early byte-path failures have no fetched opcode;
direct execute retains its supplied opcode even on an early failure. These are
host structured errors, not a new numeric guest trap-cause allocation.

### Production memory trait for Steps 18–21

The CPU crate exports this allocation-free dependency boundary; no production
RAM, mappings, address space, or bus has been introduced:

```rust
pub trait CpuMemory {
    type Error: core::error::Error + 'static;
    fn fetch_instruction(
        &self,
        config: ArchitectureConfig,
        pc: InstructionAddress,
        privilege: Privilege,
    ) -> Result<[u8; 8], Self::Error>;
    fn read_data(&mut self, access: DataAccess) -> Result<u64, Self::Error>;
    fn write_data(&mut self, access: DataAccess, value: u64) -> Result<(), Self::Error>;
    fn peek_stack(&self, access: DataAccess) -> Result<u64, Self::Error>;
}
```

`DataAccess::new(config, base: VirtualAddress, displacement: i32, size: DataSize,
kind: DataAccessKind, privilege: Privilege) -> Result<DataAccess, DataAccessError>`
checks mode-supported width, base/result/end range, then natural alignment.
Private fields are exposed by value through `config/address/size/kind/privilege`.
Read/Write describe ordinary data; StackRead/StackWrite additionally require RAM.
`DataAccessError` retains InvalidWidth config/size, WidthError input/cause, or
Alignment address/size. Mapping and transaction errors wrap the entire validated
DataAccess in `CpuFaultCause::Memory { access, source }`; fetch errors retain PC
and the concrete source. Step 20 memory errors should additionally retain their
access kind, size, address, mapping/device cause and privilege as appropriate.
No diagnostic strings replace typed causes.

Implementors MUST satisfy all of the following:

1. Preserve address domains. CPU instruction addresses and virtual data addresses
   are not physical addresses; identity translation belongs explicitly inside
   AddressSpace/Bus. Never truncate addresses, wrap access ends, split a transfer,
   or guess CPU/device addresses. Reuse DataSize/config; do not reinterpret fetch
   as an eight-byte Double load (LZ32 fetches eight bytes too).
2. Before any mutation or device callback, validate the complete transfer within
   one mapped region, then permissions for the current privilege and operation,
   then access policy and transaction capability. Reject cross-region transfers,
   even adjacent compatible mappings; Supervisor never bypasses R/W/X. A failed
   read/write must leave RAM, devices, and observable device state unchanged.
3. Fetch is an eight-byte execute access aligned to four, with no side effects,
   no requirement for read permission, and no executable MMIO. Return exactly the
   eight bytes in address order. Check full mapping/range and execute/User policy.
   Stores must be visible to subsequent fetches without cache synchronization.
4. `read_data` accepts Read only. Return the little-endian unsigned value in the
   low size*8 bits of a u64, with higher bits zero. `write_data` accepts Write or
   StackWrite only; write the low size bytes of the supplied u64 in little-endian
   order. Validate before every device operation; do not promise rollback of an
   already observed MMIO effect. Reject operations whose backend cannot guarantee
   success-or-no-effect, rather than allowing a late device failure.
5. StackWrite requires a complete writable RAM word, not ROM or MMIO. CALL target
   and nextPC/newSP are validated before the stack transaction. Successful write
   is followed only by infallible CPU publication. Target mapping/execute access
   is intentionally checked at the next fetch; aligned unmapped calls still push.
6. `peek_stack` accepts StackRead only. Validate a complete readable RAM word,
   including privilege, before returning a pure little-endian peek. Reject ROM,
   MMIO, or any backend whose peek is side-effecting. No read counters, consumed
   device values, popped-byte clearing, or other observable changes are permitted,
   on either success or failure. The CPU checks newSP before this peek, then
   validates the returned PC with `prepare_outcome`; failure changes nothing.
7. Exclusive memory ownership spans each CPU step, especially RET peek through
   outcome commit. No background actor, interior mutation, remapping, DMA, or
   competing writer may change the slot/mapping between peek and publication.
   RET's prepared Pop callback performs no additional memory operation: the pure
   validated RAM peek plus exclusive ownership is its transaction proof. If a
   future concurrent backend cannot provide that guarantee, extend the boundary
   with a held transaction/token before attaching it; do not silently reread or
   accept a stale return target. No memory clone is required or performed.
8. Debugger peek is a separate future API, not `read_data` or `peek_stack` with
   weakened policy. The latter is deliberately RAM-stack-only. Bus/device layers
   must preserve all precision guarantees instead of leaking device internals.

Execution stages a small architectural-state clone, never a memory clone. Outcome
preparation precedes successful side-effecting data reads/writes; data-load result
extension uses shared width shifts/masks and is infallible after size validation.
No fallible state setter follows a successful device operation. RET's pure peek
necessarily precedes target preparation and is safe under the ownership contract.
Only the test fixture in `tests/support/mod.rs` implements RAM today; it is not a
production mapping implementation or a substitute for Steps 18–21 acceptance tests.

Step 16 adds five interpreter tests; Step 17 adds eleven independently gated tests,
including wide integer oracles with boundary grids/destination aliases, all branch
conditions over all 16 NZCV patterns, all supported MEM sizes and extensions,
privilege/controller rejections, decoded fetch faults, pure stack operations and
failed-transaction atomicity. Together with the earlier suites: 35 CPU tests,
119 workspace tests, zero doctests. These counts are test functions, not individual
oracle vectors. All implemented opcodes execute in both modes; Double MEM is
LZ64-only and its LZ32 rejection is explicitly exercised.

## Privilege, traps, and centralized interrupts

User may execute every non-Supervisor-only opcode, subject to memory permissions.
SYSCALL and TRAP are also legal in Supervisor; they are not privilege changes
by themselves until trap entry. User execution of HALT/RFE/EI/DI/CSRR/CSRW raises
PrivilegeViolation without applying the requested operation.

v1 uses one machine-owned trap controller and one external interrupt controller,
not per-instruction dispatch rules. A single active trap frame is enough:
external interrupts are deferred while it is active, even if IE is set again.
A synchronous fault/trap while a frame is active is a terminal double trap;
retain both contexts and do not overwrite the first frame. No nested interrupt
priorities, guest vector tables, or automatic memory-stack frames are required.

Before any trap-entry mutation, capture an exact immutable snapshot of all 16
general registers, current PC, SP, status/privilege, plus the typed cause and
available access/operand details. For synchronous faults this is the faulting
instruction's unmodified state. For SYSCALL/TRAP it is likewise the instruction's
pre-state, NOT a state with PC already advanced. External interrupts capture the
state at the next instruction boundary after the previous instruction committed.
Keep this snapshot distinct from editable resume control fields:

- Resume PC defaults to faulting PC for faults, checked nextPC for SYSCALL/TRAP,
  and the boundary PC for interrupts.
- Resume SP and status initially equal the exact captured SP and status.
- Entry validates the configured trap target's width, alignment, and complete
  executable fetch for Supervisor before changing control state. It then
  installs the frame, sets PC=trap target, U=0, IE=0, preserving NZCV, SP, and all
  general registers. No guest-memory write or implicit stack switch occurs.
- Missing/invalid trap target, failed entry validation, or a double trap leaves
  pre-entry guest state intact and reports terminal Faulted with the original
  snapshot/cause and the entry failure. No recursive delivery.
- RFE requires an active frame, validates saved PC width/alignment, saved SP
  width/word alignment, and saved status reserved bits before committing. It
  restores PC/SP/status from the editable resume fields and clears the frame.
  Target mapping/execute permission is tested on the subsequent fetch. It does
  NOT restore general registers from the immutable snapshot: the handler saves
  and restores registers in software as needed and may return result registers.
  A failed RFE changes nothing before the resulting fault/double-trap handling.

The immutable snapshot is controller-owned architectural event data, not a
specified guest-memory frame layout. General-register access remains through CPU
APIs, never public arrays. The handler must arrange its own safe Supervisor
stack before calling functions; entry cannot trust a User stack. Boot-time trap
target and initial context setup belong to later machine/boot steps.

Control selectors for CSRR/CSRW (X) are fixed as follows:

| X | Name | Access | Meaning and write validation |
| --- | --- | --- | --- |
| 0 | TVEC | Read/write | Single trap target, W-bit four-aligned; mapping validated on entry |
| 1 | EPC | Read/write, active frame | Editable resume PC, W-bit four-aligned |
| 2 | ESP | Read/write, active frame | Editable resume SP, W-bit word-aligned |
| 3 | ESTATUS | Read/write, active frame | Editable resume status, bits W-1:6 must be zero |
| 4 | TCAUSE | Read-only, active frame | Unsigned cause number below, zero-extended |
| 5 | TPAYLOAD | Read-only, active frame | TRAP signed I extended to W, external interrupt ID, otherwise zero |
| 6..15 | — | Invalid | IllegalInstruction |

Writing a read-only control or accessing a frame control without a frame raises
InvalidControlState. Invalid control values raise the relevant range/alignment/
status fault. CSRW never changes live PC/SP/status. TVEC is initially unset;
CSRR TVEC before setup raises InvalidControlState. Numeric cause allocation:
1 IllegalInstruction, 2 PrivilegeViolation, 3 InvalidWidth, 4 AddressOverflow,
5 Alignment, 6 Unmapped, 7 Permission, 8 DivideByZero, 9 DivisionOverflow,
10 InvalidControlState, 11 InvalidStatus, 12 DeviceAccess, 16 Syscall,
17 SoftwareTrap, 18 ExternalInterrupt. Others are reserved. Structured fault
records also retain PC, access type/size/address when available, and underlying
cause; TCAUSE/TPAYLOAD are not a complete diagnostics or OS syscall ABI.

External interrupt IDs are u16 unsigned values; no device assignments are fixed
here. The controller latches requests until accepted, coalesces repeated pending
requests of the same ID, and selects the lowest pending ID deterministically.
Delivery occurs before the next fetch when IE=1, no frame is active, and the
machine is Running. A request arriving during an instruction waits until that
instruction commits or faults. A synchronous fault wins over such a request.
Accept/acknowledge a request only when trap entry commits; masked/deferred
requests remain pending. EI/RFE can permit delivery at the following boundary.
Device acknowledgement protocols and controller register maps are later work.

## Provisional procedure ABI

This is a minimal software calling convention, not a frozen C data model or an
OS syscall convention. It applies equally to User and Supervisor procedures:

- r0–r3 carry the first four word-sized integer/pointer arguments; r0 returns one
  word. r0–r7 and NZCV are caller-saved. r8–r15 are callee-saved. There is no
  mandatory frame pointer, red zone, link register, or home/shadow area.
- Before CALL, the caller places additional word-sized arguments at ascending
  addresses starting at its current SP. At callee entry, `[SP]` is the pushed
  return PC, `[SP+B]` is argument 5, and `[SP+2B]` is argument 6. The caller
  allocates/removes the argument area; RET removes only the return PC.
- Callees may allocate downward but must restore SP to its entry value before
  RET and restore callee-saved registers. All SP values and stack slots are
  B-byte aligned. Separate software instructions for frame allocation retain
  their normal interruptibility.
- Narrow unsigned arguments/results are zero-extended to W; narrow signed ones
  are sign-extended from their declared source width. Pointers occupy one word.
  Aggregates, variadics, multiword values/returns, floating point, unwinding,
  struct layout, and C `int`/`long` sizes await a dedicated ABI design.
- Ordinary calls preserve privilege and IE. SYSCALL merely enters the trap
  mechanism: syscall numbers, argument registers, services, errors, and return
  conventions are deliberately not allocated here (roadmap Step 35).

The shared instruction encoding does not make binaries ABI-compatible: word
loads/stores, pointer layout, stack slots, arithmetic, and address ranges differ.
There is no mixed-mode call or automatic pointer conversion in v1.

## Architecture configuration — Step 9 handoff

Implement only configuration and its tests in Step 9, preserving Step 7 APIs.
`WordWidth` has exactly two variants, `W32` and `W64`, with pure `bits()` and
`bytes()` queries. `ArchitectureConfig` is an immutable validated value, with
named `lz32()` and `lz64()` constructors. A fallible
`try_from_bits(width: WordWidth, feature_bits: u32)` constructor validates raw
features; construction from an already validated WordWidth/FeatureSet pair can
be infallible, since both modes require exactly the same set. No independently
adjustable pointer width, address width, alignment, or register count is permitted.
Required read-only query semantics:

| Query | LZ32 | LZ64 |
| --- | --- | --- |
| `word_width()` | W32 | W64 |
| `word_bits()` / `word_bytes()` | 32 / 4 | 64 / 8 |
| `pointer_bits()` / `address_bits()` | 32 / 32 | 64 / 64 |
| `register_count()` | `RegisterIndex::COUNT` = 16 | Same |
| `instruction_bytes()` / `instruction_alignment()` | 8 / 4 | 8 / 4 |
| `stack_alignment()` | 4 | 8 |
| `supported_data_sizes()` | [1, 2, 4] bytes | [1, 2, 4, 8] bytes |
| `supports_data_size(bytes)` | True exactly for 1,2,4 | True exactly for 1,2,4,8 |
| `features()` | BaseInteger only | BaseInteger only |

`FeatureSet` is a validated u32 bitset: bit 0 is `BaseInteger`; bits 31:1 are
unsupported. v1 accepts exactly raw bits `0x00000001`, in both modes. Zero is
invalid (missing BaseInteger), and unknown bits are invalid, never ignored or
implicitly enabled. For a value with both problems, report unsupported bits
first, then missing BaseInteger. Retain rejected raw bits in a structured error.
Do not add empty valid sets or guessed floating-point/vector/privilege features.
`FeatureSet::base_v1()` is infallible; `FeatureSet::try_from_bits(u32)` is
fallible and `bits()` returns the accepted u32. ArchitectureConfig cannot contain
an invalid FeatureSet. Width/size/count queries return u8 values, except
`supported_data_sizes()` returns a read-only static slice of u8 byte sizes,
`supports_data_size(u8)` returns bool, and the named type-valued queries return
WordWidth and FeatureSet. Use structured errors with retained inputs, not strings.

Current `PhysicalAddress`, `VirtualAddress`, and `InstructionAddress` remain
u64-backed domain wrappers with unrestricted constructors and u64-checked byte
arithmetic. Configuration-aware checks are an additional layer, not a rewrite:
`InstructionAddress::new(3)` still succeeds but fails instruction alignment
validation; LZ32 rejects `0x1_0000_0000` at its architectural boundary. Do not
silently change existing constructors, add conversions between address domains,
or reinterpret `DeviceOffset`, `CycleCount`, or `InstructionCount` at word width.
Step 9 does not implement status, opcodes, decoding, register state, or a CPU.
Step 10 adds the pure width contract above and structured arithmetic errors;
status mutation and guest trap delivery stay in later steps.

## Design review and future acceptance cases

These are manually reviewed specification vectors, NOT executable ISA tests.
Existing Cargo/Nix tests only validate the implemented foundations. Turn these
cases into tests as their owning steps introduce implementation:

| Case | Expected |
| --- | --- |
| ADD r1,r2,r3 | little-endian bytes `10 21 03 00 00 00 00 00` |
| LI r15,-1 | little-endian bytes `02 0f 00 00 ff ff ff ff` |
| LDZ r1,[r2-4],4 | little-endian bytes `30 21 20 00 fc ff ff ff` |
| BR AL,-2 at PC=0x100 | Bytes `40 00 00 00 fe ff ff ff`; nextPC=0x108, target=0x100 |
| Reserved R byte nonzero | IllegalInstruction, no instruction effects |
| LZ32 ADD M,1 | result=0, N=0 Z=1 C=1 V=0 |
| LZ64 SUB 0,1 | result=M, N=1 Z=0 C=1 V=0 |
| ADD signed maximum,1 in either mode | result=MIN bit pattern, N=1 Z=0 C=0 V=1 |
| SUB MIN,1 in either mode | result=signed maximum, N=0 Z=0 C=0 V=1 |
| LZ64 LDS 0x80000000 from 4 bytes | 0xffffffff80000000; LDZ gives 0x0000000080000000 |
| SHL 1 by W; incoming C=V=1 | result=1, N=0 Z=0 C=0 V=0 |
| DIVS -7,3 / REMS -7,3 | quotient=-2 / remainder=-1 at W |
| DIVS or REMS MIN,-1; any division/remainder by zero | Typed trap, destination and flags unchanged before entry |
| Base 0 plus MEM displacement -1 | AddressOverflow, not address M |
| LZ32 PC=0xfffffff8 with readable executable bytes | nextPC overflows, no instruction effects |
| LZ64 PC=0xfffffffffffffff8 with executable bytes | Same checked-nextPC fault |
| CALL at PC=0x100, I=2, SP=0x1000 | target=0x110; SP=0xffc (LZ32) or 0xff8 (LZ64); push 0x108 |
| RET after that CALL with unchanged stack | PC=0x108, SP=0x1000; NZCV unchanged |
| CALL stack underflow / RET unaligned loaded PC | No push/pop/SP/PC/flag partial effects |
| Untaken BR with out-of-range hypothetical target | PC=nextPC; no target fault |
| SYSCALL at PC=0x100 | Snapshot PC=0x100; editable EPC=0x108; no pushed return PC |
| Invalid RFE / trap target / active-frame synchronous fault | No partial restore/entry; retain original and failure contexts |
| Feature bits 0, 2, 3, or 0xffffffff | Invalid; only 1 is accepted |

Review cross-checks: every format accounts for all 64 bits and requires unused
bits zero; every operand register is four bits; all opcodes/selectors are unique;
all data accesses and PC/SP updates have an explicit width/alignment policy;
carry means borrow for subtraction throughout predicates and helpers; shift and
division flags agree with the width contract; CALL/RET word size differs while
instruction length and displacement scaling do not; trap snapshots are exact
pre-entry state and not editable resume state. No implementation tests or
compatibility claims are inferred from this paper review.
