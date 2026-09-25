# Lazen Memory Model

This document is Step 53 of the roadmap. It decides how Lazen manages memory.
The decision is driven by LazOS, not by the fact that Lazalith is written in
Rust.

## The decision

Lazen v1 uses **explicit manual memory with no ownership tracking**:

```text
ownership            no
reference counting   no
garbage collection   no
arena                no, as a language feature
hybrid               no
```

Concretely:

- every value has a fixed, statically known size and alignment;
- data lives in one of three places: the program's static data, the stack, or
  memory the program obtains from the OS;
- there is no `free`, no destructor, and no drop order to reason about;
- a value is copied when it is assigned, passed, or returned;
- references and slices are *unchecked views* that the programmer must keep
  valid; the compiler checks their shape but not their lifetime.

## Why not garbage collection

A collector needs a root set, a write barrier, and a heap the collector can
walk. LazOS currently has no heap: `UserMemory` owns a bump pool, and the
`AllocateMemory` syscall is validated by the dispatcher but not yet served. A
collector would therefore be a collector over a bump allocator, which is
strictly worse than a bump allocator: it would add runtime size, make
execution non-deterministic, and require a memory model the OS does not have.

Lazen's target programs are terminal tools, games, and small utilities. Their
allocation behavior is known at authoring time well enough that a bump
allocator plus explicit reuse is sufficient for the first milestone.

## Why not ownership

Ownership (move semantics, borrows, lifetimes) is the right answer for a
general-purpose language, and it may become Lazen's answer later. It is not the
right answer for v1 because:

- it requires a borrow checker, which is a large semantic subsystem whose
  diagnostics are the part users notice most, and which cannot be made
  genuinely good in a first milestone;
- it interacts with the FFI boundary: every `extern "syscall"` declaration
  would need lifetime annotations for data that the OS owns;
- it would give the *appearance* of safety over a raw syscall ABI that is
  checked only at the ABI layer, which is worse than being honest about it.

## Why not manual memory with `malloc`

Because there is nothing to implement. A `malloc` needs a free list, coalescing,
or a size class, and the OS does not yet serve `AllocateMemory`. When the OS
grows a real heap, Lazen grows an allocator in the standard library on top of it.
The language does not change.

## The three memory places

| Place | Lifetime | Obtained by | Freed by |
| --- | --- | --- | --- |
| Static data | whole program | `const` and string literals | never |
| Stack | enclosing call | `let`, call frames | return |
| OS memory | program | `extern` allocation call, once served | never in v1 |

Static data is read-only and is placed in the object's read-only data section by
the code generator. The stack grows down from the loader-provided stack pointer.
OS memory is address-only: Lazen has no type that owns it, and the program
decides how long it lives.

## The four memory-related types

```lazen
i32                 // 4 bytes, no interior mutability
[u8; 256]           // 1024 bytes, inline in the frame
&[u8]               // 16 bytes: pointer + length
&mut [u8]           // 16 bytes: pointer + length
*T                  // 8 bytes: an address
ptr<T>              // 8 bytes: an address for the OS ABI
optional<T>         // tag + payload area
```

`&[T]` and `&mut [T]` are the only compound view types in v1. There is no
`&T`/`&mut T` single-element reference: taking `&array[0]` is spelled
`array.as_slice()` and then indexing, which keeps one bounds-checked path.

A `str` is a distinct primitive that is *not* the same as `&[u8]`: a `str` is a
UTF-8 string literal or a value derived from one, and it lowers to a checked
`(pointer, length)` pair. `s.as_bytes()` produces `&[u8]`.

## Bounds checking is a trap, not a panic

Indexing and slicing bounds are checked. A failed check does not panic, does not
abort, and does not print. It executes the ISA's software trap instruction with
a documented code, which the scheduler turns into a `Faulted` process with a
retained cause:

```text
index out of bounds   -> TRAP 1
slice range invalid   -> TRAP 2
```

This is a deliberate choice. It means an out-of-bounds access in Lazen is
observable, deterministic, and identical headless or under SDL3, and it reuses
the trap machinery that already exists rather than inventing a runtime.

## What is deliberately unchecked

- Dereferencing `*T`.
- The validity lifetime of `&[T]` and `&mut [T]`.
- Whether an address obtained from the OS is still valid later.

These are the documented unsafe surface of Lazen v1. They are small, they are
written down, and they are the price of not owning a borrow checker yet.

## Memory layout of a compiled program

The code generator emits the same three-section object the assembler emits:

```text
.text    instructions
.rodata  string literals and static initializers
.bss     zero-initialized statics
```

`required_data` in the `.lzx` header is the sum of the read-only and
zero-initialized extents, so the loader's heap advance stays correct. The
existing linker already computes this, including alignment gaps between BSS
sections, so Lazen inherits that correctness rather than reimplementing it.

## Consequences for the rest of the roadmap

- Step 67's standard library may provide collections, but only ones that live on
  the stack or in static data. A growing collection needs a heap, so `Vec`-like
  growth is out of scope until the OS serves `AllocateMemory`.
- Step 60's IR needs explicit load/store, no alias analysis, and no ownership
  metadata in values.
- Step 74's debugger gets a real story: stack frames are visible, the trap
  codes are visible, and there is nothing to inspect about a collector.
