# The hardening phase

`instruction.md` ends with the hardening phase and its eighteen clusters. This
document is the running record of what the audit *found*, because the findings are
the point: a hardening phase that reports "no issues" has either not been run or
has not been honest.

Every entry here follows the same shape — a reproducer, what it should have
answered, what it answered, the root cause, the fix, and the regression test that
fails without the fix. That shape is the phase's rule made mechanical.

## The baseline, and what "green" was worth

At the start of this phase the suite was **1176 tests, all passing**, with clean
formatting, clean strict Clippy, a clean workspace check, green `nix flake check`
and a clean `nix build`.

That is worth recording as what it was: a *consistent* platform. Every layer
agreed with itself, the test suite was green, and the architecture was what step
99's thirteen checks said it was. It says nothing about agreement with **C's
semantics**, because nothing in the suite asked.

---

## H5 — ISA, CPU and machine: arithmetic against an independent model

**New test:** `crates/lazalith-cpu/tests/hardening_arithmetic.rs`.

The existing CPU tests assert what the interpreter *is*. A test that computes an
expected value by calling the same `WordWidth` method the interpreter calls is a
test that agrees with the bug, so this file writes the model from the ISA document
instead — explicit masking, Rust's own `i32`/`i64` arithmetic, a hand-written
sign extension — and compares after every instruction, through a real interpreter
and a real memory.

**Campaign:** 13 operations × 2 widths × 400 random pairs, where half the values
are drawn from a table of the interesting ones (zero, one, all ones, the sign
bit, one below it) because those are where a width or sign mistake shows up and a
uniform sample almost never lands on them. Over 10,000 comparisons.

**Result: no defect.** Every operation agrees with the model at both widths, the
fault cases fault, and the two widths agree wherever the widths say they should.

Two *test* defects were found and fixed while writing it, and they are recorded
because a test that asserts the wrong thing is a defect too:

- The division-by-zero test treated `u64::MAX` as zero. It is zero at 32 bits
  only when truncated to zero, and it is a perfectly good divisor at both widths.
  Asserting that it faults would have asserted a bug.
- The model's own sign extension did `1i64 << 64`, which does not exist. A model
  that cannot express the case it is testing is a model that quietly skips it.

**What this cluster establishes:** arithmetic is the part of the platform most
likely to be quietly wrong, and it is clean. The reason is worth naming — every
width operation funnels through one `WordWidth` type, and that type's
`div_signed`, `shl`, `sar` and `truncate` were written with the overflow cases in
mind (`MIN / -1` is a `WidthError`, not a wrapped answer).

---

## H9 — runtime and C frontend: three confirmed wrong-answer defects

**New tests:** `crates/lazalith-c-compiler/tests/hardening_widening.rs` and
`crates/lazalith-c-compiler/tests/cross_frontend.rs`.

The C frontend is an independent implementation of C semantics that shares one IR,
one lowerer and one code generator with Lazen. That makes it the only oracle this
project has: two implementations of overlapping semantics, and a disagreement is
a bug in one of them. The first defect was found by asking it a question the
existing suite never asked.

### Defect 1 — widening a signed value sign-extended from the wrong bit

```c
int main(void) { int v = -3; long w = v; if (w < 0) { return 1; } return 0; }
```

**Answers `0`.** Should answer `1`.

The interesting part is *where* it is wrong, because the wrongness is selective
and that is what makes it dangerous:

| expression | answer | |
| --- | --- | --- |
| `int v = -3; if (v < 0)` | correct | no widening |
| `long w = v; if (w < 0)` | **wrong** | widening through the conversion path |
| `long w = (long)v; if (w < 0)` | **wrong** | same |
| `neg(v)` with a `long` parameter | **wrong** | same |
| `long w = 0 + v;` | correct | arithmetic, not a conversion |
| `long w = v / 2;` | correct | arithmetic |
| `long w = -3;` | correct | a constant is folded elsewhere |

So the same value is right in arithmetic and wrong in a conversion, which is
exactly the signature of a defect in the *conversion* path and nothing else.

**Root cause** — `crates/lazalith-c-compiler/src/ir.rs`, `Emitter::store`'s
conversion branch. It widens through a scratch slot, and it built the load that
reads the scratch back as

```rust
CType::Int { bits: target * 8, signed: shape.signed }
```

— the **target's** width with the **source's** signedness. The comment directly
above it says the right thing: *"how far to extend follows the source"*. The width
was the target's, so a widening conversion sign-extended from bit 63 of a
pre-zeroed scratch instead of from bit 31 of the value. The scratch is cleared
before the value is written to it, so bit 63 is always zero and every widened
negative came out positive.

**Fix:** read the scratch back at the **source's** width and signedness. The
load's *declared* type stays the target's, so the value that comes out is a
full-width one, which is what the store below it wants — and the load is where the
backend sign-extends, from the width it is told to read.

The first attempt at the fix was to store at the target's width instead, which is
the other thing that would work and which the IR verifier correctly refuses: a
store may not be wider than the value it stores. That the verifier caught it is
worth recording, because the alternative would have been a codegen change for no
reason.

### Defect 2 — `unsigned int` was a signed `int`

```c
int main(void) { unsigned int v = 4294967293u; if (v > 2147483647u) { return 1; } return 0; }
```

**Answers `0`.** Should answer `1`. And `show((long)v)` printed `-3`.

**Root cause** — two places, and the second only shows up once the first is
fixed.

- `BaseType::Int` had no `unsigned` field. `BaseType::Short { unsigned }` and
  `BaseType::Long { unsigned }` both had one, and `int` — the most-used integer
  type in the language — did not, so there was nowhere for the parser to put the
  flag and `unsigned int` parsed as `int`.
- The parser's specifier merge applied a written signedness to `char` only. A
  bare `int` following `unsigned` was taken as the whole answer, so the flag was
  dropped on the floor.

**Fix:** `BaseType::Int { unsigned: bool }`, the merge applies the flag to `Int`
as well as to `Char`, and `BaseType::Int { unsigned }` maps to
`CType::Int { bits: 32, signed: !unsigned }`.

### Defect 3 — a bare `signed` was a `char`

Found by the *same* fix, in the arm that previously read

```rust
(0, 0) => if signed { BaseType::Char { signed: true } } else { BaseType::Int }
```

C says a bare `signed` is `signed int`. It was a one-byte **char** — a different
*width*, not merely a different sign — so `sizeof (signed x)` said 1 and every
arithmetic operation on it happened at 8 bits.

**Fix:** `(0, 0) => BaseType::Int { unsigned }`. `signed` is no longer needed as
a parameter: in C the only place `signed` is load-bearing on its own is `char`,
and `char` is a base keyword that never reaches this function.

### Defect 4 — `long int` and `unsigned long int` lost their width

Found by the specifier-table test rather than by reasoning, and the same merge is
its root cause: with `long int`, the `long` set the width keyword and the `int`
then overrode the whole type with a 32-bit `int`. C says `long int` is a `long`.

**Fix:** the merge now lets a width or signedness keyword win over a following
bare `int`, which is the rule C actually states. The test table has 21 spellings
and each is checked twice — once for `sizeof`, which catches a width, and once by
comparing against a value above `INT_MAX`, which catches a sign.

### What none of this says about Lazen

The same differential runs the same ten algorithms through both front ends and
compares the printed value and the exit status. Both agree on all ten, and both
agree with values computed by hand. Lazen's own conversions were not touched by
any of this, and the C frontend's fixes do not change a single Lazen output.

The `match`-and-agree test is the one that would have caught all of this had it
existed during the first hundred steps, and its absence is the lesson: the C
frontend was tested against **itself** and against the shared backend, and never
against C.

---

## Running totals

| | before | after |
| --- | --- | --- |
| tests | 1176 | 1189 |
| confirmed defects | — | 4 (all wrong-answer, all in the C frontend) |
