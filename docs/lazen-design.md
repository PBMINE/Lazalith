# Lazen Design

Lazen is a small native application language for Lazalith. It is designed for
programs that run inside LazOS and use the Lazalith ISA directly, while keeping
the compiler and runtime model understandable to the platform.

This document defines the language direction. It does not specify a complete
compiler implementation yet.

## Goals

Lazen should make the common native-application path concise:

```text
source -> typed semantic model -> Lazalith assembly -> .lzo -> linker -> .lzx
```

The language should:

- describe memory, integers, text, and small collections directly;
- make User/Supervisor privilege boundaries explicit;
- use the Lazalith calling convention and OS ABI without hidden runtime calls;
- produce precise, source-located diagnostics;
- keep programs deterministic and free of implicit host facilities;
- remain small enough for a bootstrap compiler and a native toolchain.

## Language shape

A first Lazen program is a module with an entry procedure:

```text
module hello;

export proc main() -> int {
    return 0;
}
```

The syntax is intentionally distinct from C, Rust, Pascal, and Go. It uses
`module`, `proc`, `let`, `if`, `while`, `return`, and explicit type names. It
does not adopt their class, ownership, or package conventions.

## Target and entry mapping

A compilation unit selects exactly one target word width with a directive or
compiler flag:

```text
target lz32;
target lz64;
```

The directive maps to the assembler's `.arch` requirement; a missing or
conflicting target is a diagnostic. The exported `main` procedure is the
lowered entry symbol. The compiler emits `.entry main` in the module `.lzo` and
requires the linker root to select that symbol. V1 does not mangle export
names, so the mapping is inspectable in the object.

## Types

The initial type set is small and explicit:

- `int` — architecture word, signed two's-complement;
- `uint` — architecture word, unsigned;
- `bool` — one-bit logical value;
- `byte`, `half`, `word`, `dword` — explicit memory widths;
- `ptr<T>` — checked User-memory pointer to `T`;
- `string` — immutable length-delimited byte text;
- `array<T, N>` — fixed-size inline value;
- `slice<T>` — bounded view over an existing array or User allocation;
- `struct` — named, fixed-layout record;
- `enum` — closed integer-tag union;
- `proc` — typed function pointer in the target ISA;
- `unit` — no value.

There is no implicit widening between word types, no unchecked numeric
conversion, and no host-dependent `int` width. LZ32 and LZ64 are separate
compilation targets.

## Ownership and memory

Lazen does not hide memory management. A value is either inline, borrowed from a
validated slice, or allocated through an explicit OS ABI call. The compiler
records the target region, alignment, length, mutability, and lifetime of every
pointer. It rejects a pointer that can cross a privilege or object boundary.

`string` and `slice` carry bounds. A slice cannot be formed from an arbitrary
machine integer without a checked allocation or a bounds proof. `ptr<T>`
operations are explicit and lower to the shared memory ABI; the compiler does
not invent a garbage collector in v1.

## Procedures and calls

Procedures have explicit parameter and result types:

```text
proc write_all(text: slice<byte>, out: int) -> int
```

The first milestone lowers calls to the Lazalith stack convention. A procedure
may be marked `export` for a linker-visible symbol. Recursion, nested
procedures, and variadic calls remain out of the first language slice.

The compiler reserves registers according to the ISA contract and spills only
when required. It does not promise a stable register allocation across
compiler versions.

## Control flow

The initial statements are:

- expression statements;
- `let` bindings;
- `if`/`else` conditionals;
- `while` loops;
- `break` and `continue` within the nearest loop;
- `return`.

There are no exceptions, coroutines, generators, or implicit short-circuit
side effects in the core language. Every condition has type `bool`; every
branch and loop has a statically checked result type.

## Modules and visibility

A module has a name, typed declarations, and zero or more exports. Imports are
explicit and name a target module. Global state is not introduced implicitly;
a module-level `const` is immutable and a `global` requires an explicit region
and initialization rule.

The compiler emits one `.lzo` object per module. Import resolution and final
symbol layout belong to the linker, not the parser.

## Errors

A Lazen diagnostic has a stable code, message, file, line, column, span, and
optional related labels. It carries a typed cause chain across lexing, parsing,
name resolution, type checking, lowering, and encoding. The compiler never
turns malformed source into a partially published object.

## Privilege and system services

Lazen distinguishes ordinary User code from Supervisor-only operations. The
compiler marks regions and rejects privileged instructions in User code. OS
services are called through typed wrappers around the shared syscall ABI;
direct device access, trap-frame construction, and raw interrupt control are not
language features.

A source-level service call therefore remains auditable:

```text
write(fd, slice) -> result
exit(code) -> never
time() -> ticks
```

The wrapper names are conveniences, not a second ABI.

## Compilation model

The compiler pipeline is:

```text
bytes -> tokens -> AST -> name/type resolution -> lowered IR -> ISA -> .lzo
```

Every token and lowered instruction retains a source span. The IR is deliberately
close to the ISA so that branch, call, load, store, and immediate relocations
can be emitted without guessing. The assembler and linker remain reusable for
hand-written Lazalith assembly.

Four things in the IR exist because the language needs them and the ISA alone
cannot say them, and each is spelled so a backend cannot substitute a guess:

- **`FrameBase`** is the current function's frame base, which is the machine's
  stack pointer: the prologue moves SP down by the frame size and the base is
  whatever SP then holds. It is not a function's address, which is a different
  value that changes per call site.
- **`DataAddress`** names a data segment, because a string's address is decided
  by the linker and no front end can know it. A backend resolves the segment; it
  may not invent an address, and the verifier rejects a name the module does not
  have.
- **`BoundsCheck`** takes both the index and the length, compared as unsigned
  values of the same width. A check with only an index compares nothing, and a
  backend asked to invent the bound would be guessing where reading memory that
  belongs to something else would be the alternative.
- **`SliceLength`** reads a view's length from the view itself, so a length
  cannot be recomputed from an address that may no longer describe the same
  bytes.

A cast between integer widths is a load rather than a conversion instruction: a
load may be narrower than its type, which is exactly what the machine's extending
loads do, so `u8 as i64` is an eight-bit load typed as a 64-bit integer. A load
may never be *wider* than its type, and the verifier rejects one that is.

## Non-goals for v1

- copying C, Rust, Pascal, or Go wholesale;
- garbage collection or a background runtime;
- dynamic loading, threads, or network libraries;
- arbitrary inline assembly in the first compiler;
- exceptions, macros, templates, or a package ecosystem;
- changing the Lazalith ISA, OS ABI, or `.lzo`/`.lzx` contracts implicitly.

## Open design questions

- Should the first release include a bounded `match` construct for enums, or
  should users use `if` chains until its lowering cost is understood?
- Which fixed-width integer syntax best exposes LZ32/LZ64 differences without
  making ordinary programs noisy?
- Should `slice` carry a compile-time region identifier once multi-region
  programs are introduced?
- What diagnostic wording best distinguishes a rejected privilege boundary
  from an ordinary type error?
