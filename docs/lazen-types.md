# Lazen Type System

This document is Step 54 of the roadmap. It fixes the types Lazen v1 has, the
types it deliberately does not have, and the rules the checker enforces.

## Scalar types

| Type | Size | Notes |
| --- | --- | --- |
| `bool` | 1 | `true` / `false`, no implicit conversion to integers |
| `i8` `i16` `i32` `i64` | 1 2 4 8 | two's complement, wrapping arithmetic |
| `u8` `u16` `u32` `u64` | 1 2 4 8 | wrapping arithmetic |
| `usize` | word | target word size, 4 in LZ32 and 8 in LZ64 |

Integer literals are context-inferred. `42` is any integer type that fits;
`0x1F` is the same. A literal that does not fit its context is an error, never a
silent truncation.

Arithmetic wraps, and it wraps *silently by design*: the language has no
checked-arithmetic mode because the ISA has no trap-on-overflow and inventing
one would make Lazen slower than the platform it targets. Programs that need
range discipline use explicit casts, and the SDK's checked helpers are ordinary
functions.

## Compound types

| Type | Representation |
| --- | --- |
| `str` | checked UTF-8 byte string: pointer + length |
| `[T; N]` | N elements of T, inline in the frame |
| `&[T]` | pointer + length, immutable view |
| `&mut [T]` | pointer + length, mutable view |
| `ptr<T>` | address for the OS ABI, no pointee metadata |
| `*T` | address, read or written only through a typed view |

These are the whole v1 compound set. `optional`, `struct`, and `enum` are
deliberately absent; the next section says why.

Slices are two words because a slice is a view, not an owner. This is stated
here because it is observable: a `&[T]` occupies 2 words in both LZ32 and LZ64.

## Function types

Functions are declared with `fn` and have exactly one result type. A function
whose body ends in an expression evaluates it as the result; `return` exits
early. `extern "syscall"` declarations are the only foreign functions in v1,
they declare no body, and their argument order is the OS ABI's argument order.

There are no function types as values. A function name is a compile-time
entity, not a value, which removes closure, callback, and higher-order
machinery from v1 entirely.

## What v1 does not have

Lazen v1 has **no** `optional<T>`, no enums, and no records, and therefore no
`match`. This is a deliberate decision, not an omission in the writing:

- `optional` and `match` need a tagged union, and a tagged union needs a
  decision about where the tag lives in a value. That decision is easy to get
  subtly wrong in the first milestone, and getting it wrong is invisible until a
  program misbehaves on real data.
- Domain errors are, in v1, integer statuses that the program tests. A program
  that needs richer modelling composes it from `if` and integer constants, which
  is exactly what the ABI already gives it.
- The type checker, the lowering pass, and the code generator all become
  materially smaller and auditable, which is what makes the rest of the
  milestone verifiable at all.

The parser recognises the *syntax* of these features so that the diagnostic can
be specific ("records are not part of Lazen v1") rather than a generic syntax
error, and the type checker rejects them. Adding them later is a language change
recorded in a new version of this document, with the layout rules written down
before the first program uses them.

## Casts

`value as T` performs a value conversion. Legal conversions:

- integer to integer, with truncation or sign extension by the destination width;
- integer to `bool` (zero is false) and `bool` to integer;
- `ptr<T>` to an integer and an integer to `ptr<T>`.

Illegal conversions are compile errors, and a cast never changes a
representation silently in a way the target cannot express: `i64 as u8`
truncates exactly as the ISA does.

## Type checking rules

The checker enforces these rules, and each has a test:

1. **No implicit numeric conversion.** `i32 + i64` is an error; write the cast.
2. **Arithmetic operand agreement.** Both operands of an arithmetic operator
   must have the same type, and both must be integers.
3. **Comparison compatibility.** Comparison operands must have the same type, or
   one must be a pointer and the other an integer.
4. **Conditions must be boolean.** `if 1 { }` is an error.
5. **`mut` is required to assign.** A binding without `mut` is immutable, and
   assigning to it is an error naming the declaration.
6. **No use before declaration**, and no shadowing a binding in the same scope.
7. **Call arity and argument types match the declaration**, for both Lazen
   functions and OS ABI declarations.
8. **Indexing yields the element type**, and the index must be an integer.
9. **Effects.** A statement expression must be a call; anything else is an error,
   so a silent no-op cannot hide a mistake.
10. **Unsupported constructs are rejected by name**, not mis-parsed.

## Features considered and rejected for v1

| Feature | Verdict | Reason |
| --- | --- | --- |
| Type inference | adopted, for locals and literals only | removes annotation noise without hiding types from a debugger |
| `optional<T>` | rejected | needs a tagged-union layout decision; integer statuses serve v1 |
| Enums and `match` | rejected | same tagged-union decision, with no v1 program that needs it yet |
| Records | rejected | need a layout and copy rule that nothing in v1 exercises yet |
| Generics | rejected | needs monomorphisation and a much larger checker |
| Traits and interfaces | rejected | v1 has no use; the SDK is plain functions |
| Closures and callbacks | rejected | function values need a calling convention |
| Operator overloading | rejected | operators stay machine operations |
| Iterators | rejected | imply library machinery over collections that need a heap |
| Null | rejected | an address is a typed `ptr<T>`, never an implicit null |
| Implicit conversions | rejected | silent narrowing is the most common source of OS bugs |

## Consequences for the compiler

- The IR needs no generics, no closures, and no ownership metadata: values are
  integers, pointers, arrays, and two-word views.
- The type checker is a straightforward walk whose interesting output is
  diagnostics, not inference.
- Layout is fully determined by the type, so slot offsets are a function of the
  type table rather than a solver.
- Because there is no tagged union, a `match` lowering does not exist yet, and
  adding enums later is a change confined to the type table, the checker, and one
  lowering case.

## What Step 61 made concrete

The type checker in `crates/lazalith-compiler` implements this document exactly,
and four rules that the document left open are now decided. Each is enforced by
a test in `crates/lazalith-compiler/tests/typecheck.rs`.

| Question | Answer | Why |
| --- | --- | --- |
| Is `&str` a separate type from `str`? | no | a `str` is already a pointer and a length, so a reference adds nothing; the two spellings name one type |
| Can a `ptr<T>` be dereferenced? | no | it carries no length, so a read could not be bounds checked, and v1 has no `unsafe`; take a `&[T]` view instead |
| Which casts exist? | integer to integer, reference to `ptr<T>`, `ptr<T>` to integer, integer to `ptr<T>`, and any type to itself | each is a machine operation with an exact meaning; a slice, array, or `str` cannot become an integer |
| How is a `&mut [T]` parameter written through? | by indexing it | the parameter binding is immutable, but the data the reference points at is not, so mutability comes from the view's type rather than from the binding |

Two more rules came out of writing the checker:

- **An integer literal is range-checked as the value it will be.** `-128i8` is the
  minimum value and is valid; `-1u8` is an error. The digits alone are not the
  value.
- **A conditional used as a value may not bind a name in an arm.** Step 61 records
  a value conditional's arms without a frame of their own, so a name bound there
  would have no slot to live in. Rather than invent one, the construct is
  rejected. Step 62 allocates slots when it lowers the arms into the enclosing
  frame and can lift this restriction.

The type set is closed in the implementation as well as in this document: the
checker has no representation for `optional`, an enum, a record, or `match`, so
each of them is rejected by name with the reason from the table above rather than
approximated.
