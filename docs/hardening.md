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
  that computes an expected value with a shift that overflows is agreeing with
  the implementation about the wrong thing, and this one would have failed for
  the same reason the implementation does.

---

## H9 — runtime and C frontend: four confirmed wrong-answer defects

**New tests:** `crates/lazalith-c-compiler/tests/hardening_widening.rs` and
`cross_frontend.rs`.

Nothing in the suite had ever asked the C frontend to agree with C. Every C test
asked what a program *did*, which is downstream of the decisions below and would
have agreed with a frontend that got them wrong in a new way. So the campaign was
differential: the same programs, run through the C frontend and through Lazen, with
the answers compared against each other and against values computed by hand.

Four wrong-answer defects, all with the same shape — a type spelled two or more
words was reduced to the wrong width or the wrong sign — and all found by that
campaign rather than by reading code.

### Defect 1 — widening an `int` to a `long` sign-extended from bit 63

`Emitter::store` in `ir.rs` widened by reading the source at the *target* width and
the *source's* signedness. An `int` is 32 bits, so reading it at 64 bits read
twelve bytes of whatever followed it — which, in a scratch buffer, was zero. So
moving a negative `int` into a `long` produced a large positive number, and every
value in the program was wrong by `2^32`.

**Fix:** read at the *source* width and signedness, and extend from there.

### Defect 2 — `unsigned int` was parsed and checked as signed `int`

`BaseType` had no unsigned flag for `int`, so `unsigned int` and `int` were the
same type. The result was a program that computed with signed rules and compared
with signed rules, and was wrong for exactly the values `unsigned` exists to hold.

**Fix:** `BaseType::Int { unsigned: bool }`, with the flag carried through parsing
and the type checker.

### Defect 3 — bare `signed` became a one-byte `char`

C says a bare `signed` is `signed int`. The specifier merge had:

```rust
(0, 0) => if signed { BaseType::Char { signed: true } } else { BaseType::Int }
```

so `signed` was a *different width*, not merely a different sign: `sizeof (signed x)`
said 1 and every arithmetic operation on it happened at 8 bits.

**Fix:** `(0, 0) => BaseType::Int { unsigned }`.

### Defect 4 — `long int` and `unsigned long int` lost their width

With `long int`, the `long` set the width keyword and the trailing `int` then
overrode the whole type with a 32-bit `int`. C says `long int` is a `long`.

**Fix:** the merge lets a width or signedness keyword win over a following bare
`int`, which is the rule C actually states.

### What none of this says about Lazen

The same differential runs the same algorithms through both front ends and compares
the printed value and the exit status. Both agree, and both agree with values
computed by hand. Lazen's own conversions were not touched, and the C frontend's
fixes changed no Lazen output.

The `match`-and-agree test is the one that would have caught all of this had it
existed during the first hundred steps, and its absence is the lesson: the C
frontend was tested against **itself** and against the shared backend, and never
against C.

---

## H4 — code generation: frames and the calling convention

**New test:** `crates/lazalith-codegen/tests/hardening_frames.rs`.

A frame defect is invisible in a program that calls one function once with two
arguments. It appears when the stack pointer moves a long way, when a call's
outgoing arguments have to sit *beside* the caller's own locals, when a temporary
has to survive a call, and when a register is live across one. Each of the six
cases here is one of those, and each returns its answer as an **exit status**,
because a value that is *almost* right is the failure that matters and a console
comparison would hide it among surrounding output.

**Result: no defect.** Deep recursion with four live temporaries per frame, nested
calls with five arguments, recursion with the ABI's full six-word argument budget,
values live across calls, repeated outgoing-argument use in a loop, and frame reuse
across sequential calls all return the right status.

Three things are worth recording, and all three are ways of writing a test that
would have failed for the wrong reason.

**The ABI has six argument words, and the test asked for ten.** The first version
passed ten parameters and the lowerer refused it with a clear message: *calling deep
needs 10 argument words, and the ABI has 6*. That is the front end doing its job — a
diagnostic rather than a truncated frame — so the test went to six parameters and
the expected value with it. Worth noting that the first draft also asserted `900`,
which was the answer for *ten* parameters and would have been "correct" for a
program that no longer existed.

**Two of the three failures were mine, not the platform's.** The
call-inside-an-expression case expected `140`; the answer is `132` (`nest(10) +
nest(20)`, where `nest(x) = add3(x,1,2,3,4) + add3(5,x,6,7,8)`, so `56 + 76`). The
recursion case expected `900` where the answer is `300`. Both constants are now
**computed in Rust from the same rule the program implements**.

That rule was learned the hard way and is now the file's convention, and it
reappears in every cluster below: **when a test's expected value is a closed-form
constant, derive it in the test.**

---

## H2 — the Lazen frontend: precedence, scoping and shadowing

**New test:** `crates/lazalith-compiler/tests/hardening_semantics.rs`.

Precedence and scoping are the two places a compiler produces a *plausible wrong
answer* rather than a diagnostic, because the program still type-checks and still
runs. A `<<` that binds tighter than `+` does not fail; it computes a different
number.

**Result: no defect.** Twelve cases: `*` over `+`, unary minus over both, `-` over
`<`, `&&` over `||`, left-associativity of `-` and `/` and `+`, comparison against
arithmetic, a cast's reach, a subscript with a side-effecting index, a block-scoped
`let` that must not leak, a block-scoped `let` that must shadow *within* its block,
a per-iteration binding in a loop body, and a parameter shadowing a same-named
value at file scope.

Two of these were not testing what they claimed, which is the finding.

**The `&&`/`||` case did not distinguish the two precedences.** It used
`a = false, b = true, c = false` and asserted that `a || b && c` is true. But
`b && c` is `true && false` — false — so `a || (b && c)` is false, and the
parenthesized reading `(a || b) && c` is *also* false. The test would have passed a
compiler with either precedence, which is to say it tested nothing. The values that
do distinguish them are `a = true, b = false, c = false`: tight gives true, loose
gives false. The test now asserts both readings, so either precedence fails it.

**The cast case used a value that does not truncate.** It cast `300i64 as i32`,
commented "300 truncates to 44", and asserted `45`. But `300` fits in an `i32`
perfectly well, so the cast was a no-op, the comment was wrong, and the test could
not tell a cast from no cast at all. `2^32 + 1` truncates to `1`, which no other
reading of the expression produces; the test now uses that and cross-checks the
unparenthesized, parenthesized, and cast-only forms against each other.

This is the same lesson as H4 from the other direction: **a test whose expected
value does not distinguish the behaviours it is comparing is a test that asserts
nothing** — whether the constant is wrong or merely undiscriminating.

---

## H6 — memory: a refused access changes nothing

**New test:** `crates/lazalith-memory/tests/hardening_validate_before_mutation.rs`.

`docs/os-memory.md` promises that a memory validates the whole access before it
changes anything. A memory that did not would look like a working program right up
until something read what a half-completed store left behind.

**Result: no defect.** Twelve cases, each failing in a different way memory can
fail — past the end of a region, spanning a gap, into unmapped space, into read-only
memory, as a user into supervisor-only memory, past the last address, below address
zero via a negative displacement, unaligned, at a width the machine does not have
— and after each one comparing *every byte* of the regions involved rather than the
byte near the failure.

The boundary cases matter as much as the failures, and they are in the file for a
reason: a memory that refused the last *valid* word in a region would fail the same
tests, so each region is also written to its very end successfully. Both sides of
the boundary are pinned.

Two geometry errors were mine. The first slicing check compared the bytes at the end
of a region against a baseline taken *before* a legitimate successful store to those
same bytes, so it was measuring the successful store. The second tried to make an
aligned four-byte access cross a region boundary — impossible when the region length
is a multiple of four, because the last word inside it ends exactly at the boundary.
A region of **14** bytes puts a boundary in the middle of a word, which is the only
way to reach the case with an aligned access. Getting that geometry right is what
made the `CrossRegion` fault reachable at all.

A note on coverage: the decode cache's interaction with stores is already covered by
`tests/instruction_cache.rs`, which tests the success path, the failure path, a store
landing in the middle of an instruction, a store spanning two instructions, and a
caching bus against a reference bus. Two cases written here were removed as
duplicates when that file was read.

---

## H12 — object format: the writer and reader agree, and the reader survives lies

**New test:** `crates/lazalith-toolchain/tests/hardening_object_format.rs`.

The object format is hand-written binary: a fixed-layout header, then section,
symbol, relocation and debug tables, a string blob, a debug-text blob, and a
payload. Every count and offset in that header is a `u32` or `u64` the reader must
trust enough to slice with, so this file attacks it from both sides.

**Round-tripping.** Eight hand-built shapes and 400 generated objects, each required
to decode back to the *same* object and re-encode to the same bytes. The shapes put
a value in every field a lazier writer would leave at zero: a `bss` section whose
size and file size differ, a section name that is a suffix of another, a negative
relocation addend (`i64` on the wire, which must not come back as its two's-complement
bits read unsigned), a symbol with no section, a debug source with empty text, a
mapping at offset zero into that empty text, every section kind, every relocation
kind.

**Result: no defect.** Every shape and every generated object round-trips exactly.

**Untrusted input.** Every prefix of every shape is tried — a truncated object must
be refused at every length — as is every byte position with five corruptions, and a
trailing garbage byte.

**One test defect here, and it is the most interesting thing in the file.** The
corruption test originally asserted that corruption is *always* refused. It failed:
corrupting a symbol-binding byte from `Local` to `Global` produced a **valid** object
that means something different, and the reader was right to accept it. The original
test was demanding a checksum the format does not have.

What is actually acceptable is: either an `Err`, or an object that passes its own
`validate()`. That is the strongest statement available without an integrity field,
and it is one the reader can be held to. A second case now targets the specific
danger directly, setting every four-byte word in the header to `u32::MAX` in turn to
confirm a corrupted count is refused against the file's real size before anything is
reserved or sliced.

---

## H1 — architecture: the widened door has exactly one caller

The hardening phase widened two kernel accessors for the debugger, and a widened
accessor is a hole in a rule rather than a detail of a fix — so the rule now has a
fourteenth check.

`Process::memory_mut` and `UserMemory::address_space_mut` were `pub(crate)` and became
public, because a whole-machine snapshot has to move a running process.s memory out of
the machine and back (defect 9). What makes the door safe is that **only the debugger
walks through it**: swapping a process.s address space behind the scheduler.s back
would put regions in the machine the scheduler does not know about, and the consequence
is a process reading memory that is not its own.

So the check is not "the accessor is public" — that is the fix — but "the accessor has
exactly one caller outside the kernel". Verified to bite: adding the call site to the
runtime.s source makes it fail with `left: {"lazalith-debug", "lazalith-runtime"}`.

The needle is `.memory_mut()` together with `address_space_mut(` rather than the
two-step chain, because `rustfmt` splits that chain across lines and a rule that
formatting can defeat is not a rule. It also has to exclude the machine and memory
crates, which reach *their own* address-space accessors, which are about a bus and are
not this door.

---

## H3, H11 — IR and the debugger: one confirmed defect

**New test:** `crates/lazalith-debug/tests/hardening_snapshot_run.rs`.

`tests/snapshot.rs` checks that a snapshot carries what it says it carries: the program
counter, the registers, the stack pointer, the process. Every one of those is a *field*
check, and every field check has a blind spot — the thing that was never mentioned is
the thing that was never captured.

So the question asked here is the one the field checks cannot ask: **run the program to
completion, restore the snapshot, run it again, and require the same answer.**

### Defect 9 — a restored machine reported the program finished, having run nothing

```text
first  stopped Exit { code: 0 }  steps 34753
second stopped Exit { code: 0 }  steps     1
```

The second run reported a successful exit after **one instruction**. A user who
restored a snapshot and pressed continue would have been told their program had
finished, having watched it do nothing at all. And the test passed the whole time,
because every existing test checked fields and this failure is not a field.

There were three layers to it, and each was found only by fixing the one above it.

**One: a process captured while it is running has no memory.** `ProcessSnapshot`
clones the process, and `ProcessSnapshot` documents itself as capturing "a process.s
state, memory, threads and handles" — "all four, because a process is all four". That
is false for a running process, and the reason is structural: activating a process
*swaps* its regions into the machine and the machine.s own user regions into the
process. So at every point a debugger can stop at, the process the scheduler holds has
the other half of the swap, and cloning it captures a process whose address space is
not its own. A snapshot that omitted the memory of whichever process happened to be
running would restore a machine whose program reads somebody else.s bytes.

**Fix:** `snapshot_machine` can now drive the machine. It releases the active context
— which puts the memory back where the process can be asked for it — takes the
snapshot, and activates the context again. The machine ends in exactly the state it
started in. This is why its signature changed from `&self` to `&mut self`, and why it
now returns a `Result`: a controller that could not be put back is a controller whose
next `run` would be meaningless, and the caller has to hear about it.

**Two: the restored process claimed to be running, and to own a context.**
`Process::restore` restores a `Running` process, but the scheduler.s own `current`
binding belongs to the run that just finished, and nothing in a restore re-establishes
it. `next_index` only ever selects a `Ready` process, so the scheduler found nothing
runnable, the kernel.s step did nothing, and `run` reported `Exit { code: 0 }` after a
single step — which is the first symptom above. And a process that still claims the
context it was activated on cannot be *re*-activated, so the fix is both: demote to
`Ready` and forget the context.

**Three: so does the machine.** The snapshot.s processor came back claiming the same
context, and `activate_user_context` refuses a machine that already has one. So the
three claims have to be undone together: the process.s state, the process.s context,
and the machine.s.

### And a question the test asked that turned out to be the wrong one

The first draft compared the *step counts* of the two runs, and they differed by 41.
That looked like a lossy restore, and it was not: restoring the snapshot twice and
running twice shows the second and third runs retiring the *same* number. The
difference is a one-off in the very first run, which begins before the process has
been through a debugger-driven activation. The assertion was moved to the test that
can actually answer it, and `a_restored_machine_runs_to_the_same_answer_as_the_original`
compares the program.s *output* instead — the thing a person would look at.

Two test defects here as well. The program used `0u32 | 1u32 | 2u32` for open flags,
which is C bitwise-or and not Lazen, where there is no single `|`. And the first
output comparison read the terminal.s whole cumulative buffer for both runs, which is
a comparison that can only ever fail once the program prints anything and says nothing
about the program; the second run.s own output is the tail, and that is what is
compared now.

### The language gap this hit twice

Lazen has no `&mut [u8]` to `&[u8]` coercion, so every standard-library function that
takes `&[u8]` is unreachable from a program holding a mutable array, and both this file
and H10.s had to build a read-only view from an address to reach one. It is a loud
failure — a diagnostic, not a wrong answer — so it is a gap rather than a defect, and
it is recorded as one.

---

## H7, H8 — kernel and filesystem: clean

**New test:** `crates/lazalith-os/tests/hardening_filesystem.rs`.

**Result: no defect.** Ten cases, the centre of which is a campaign rather than a list.

`tests/filesystem.rs` asks the filesystem a handful of questions. This file asks it
*sequences*: 300 randomised runs of 200 operations, each operation applied to both the
real filesystem and a model written from the documented semantics, comparing the
outcome of every call **and the whole contents of the file after every step** — so a
divergence names the operation that caused it rather than merely existing at the end.
That is 60,000 operations.

Positions are drawn from a table that deliberately includes the two ends, one either
side of each, and a random interior offset, because a uniform draw almost never lands
on a boundary and every interesting filesystem bug is on one.

Three documented rules are the specific targets, and each is a place where a reasonable
implementation could differ:

- A write past the end **extends**; a write *at* an offset beyond the end is an
  **error**, because there are no sparse files here and a write that would leave a
  hole is refused rather than silently zero-filled.
- A read past the end is a **short read**, not a failure; a read *from* beyond the end
  is an error.
- A seek is bounded by the file, with the end itself reachable — seeking to exactly the
  end is what "position at the end" means, and a filesystem that refused it would be
  wrong in the other direction.

Around the campaign: the open flags are held to what they claim (neither read nor
write is refused; create and truncate both require write; truncate on open clears the
file and an open without the flag does not); `truncate` extends with zeroes and cuts;
and a refused operation never changes the file.

The model holds **one** file, deliberately — every question is about one file.s bytes.
Two cases cover what a one-file model cannot: two files do not share bytes, and
removing one file does not disturb another or resurrect its contents on re-creation.

Two test defects here, and the first is worth recording because it is the same mistake
as H4.s and H2.s:

- The read-only/write-only case built its own `FileAccess` rather than using the
  handle.s. `read_at` and `write_at` take the access as an *argument*, so the test was
  asserting something about its own argument, and the write it expected to be refused
  succeeded. Passing the handle.s own access makes it a claim about the filesystem.
- The second is the absence of a defect, and is worth saying plainly rather than
  leaving implied: a 60,000-operation campaign that finds nothing is evidence, not
  proof, and the value of this file is that it can be re-run and will catch a regression
  the day one is written.

---

## H10 — graphics: the recorded defect, resolved

**New test:** `crates/lazalith-runtime/tests/hardening_graphics_address.rs`.

`docs/graphics-test.md` recorded an open defect: a program drew correctly, and the
address the display device recorded was a *different* address from the one the
program.s own framebuffer occupied. The document could not say which of the two was
wrong, and listed two candidates — the compiler handing the SDK a view onto a frame
slot, or the SDK passing a different slice to `display_open` than the program uses.

**Both candidates were wrong, and the recorded evidence was right.**

### Defect 8 — the runner read a dead process.s frame through a machine that no longer owned it

A process.s memory lives in the machine only while the process is resident.
`activate_user_context` swaps the process.s regions in and the machine.s own user
regions out; `release_user_context` swaps them back. `run_loaded` read the presented
frame through the machine *after* the scheduler had released the process.

Instrumenting the release shows it exactly:

```text
BEFORE release 0x40ea08=[255, 0, 0, 0, 255, 0, 0, 0]   <- the pixels, in the machine
AFTER  release 0x40ea08=[0, 0, 0, 0, 0, 0, 0, 0]        <- zeros, in a different stack
process address space now holds the three user regions   <- including the drawn one
```

The reader was right about the address and right about the machine, and wrong about
which of the two owned the picture. The device.s record, the SDK and the ABI were all
correct at every layer.

This also explains the two facts that made the original evidence look so strange. The
address "varied between runs" because the stack layout is not fixed. The recorded
address "read back as zeroes" because by the time anything read it, it was no longer
the frame.s address.

**Fix:** read the pixels from the *process.s* address space, which is where a dead
process.s memory still lives, rather than from the machine, which by then holds
somebody else.s. The host now returns the exact bytes the program drew, which is the
assertion step 97 said it could not make honestly.

The first five tests in the file are the ones that separate the recorded hypotheses,
all of which pass: a view of an array and a view of that view are at the same
address; a `&mut [u8]` keeps its address across a call; the SDK hands the ABI the
caller.s address; a second open reports the second buffer.s address; and a buffer
written through the ABI is the buffer the program reads.

Two test defects were found here, and both would have sent the investigation in the
wrong direction:

- The probe used `0o777` for an octal literal. C has no `0o` prefix; Lazen neither.
  The frontend was right to reject it.
- The first printer printed the buffer.s leading zeroes, because `write_u64` fills
  from the *end* of the buffer backwards and returns only a count. For a few minutes
  that looked like the platform returning null addresses. The file now has its own
  unsigned printer, and the assertion about channel order went the same way: the
  first draft assumed the alpha byte was last, and the format is ARGB.

---

## H2 again — the C frontend: integer constants

**New tests:** `crates/lazalith-c-compiler/tests/hardening_c_types.rs` and
`hardening_c_constants.rs`.

The C specifier matrix that H9's entry pointed at as missing is now permanent, and
writing it found two more defects — both in *constant handling*, which no test had
ever asked about.

### Defect 5 — every `unsigned long` constant panicked the compiler

`integer_value` in `types.rs` computed a constant's limit as `1u64 << bits`, and
checked `bits < 64` *afterwards*. For a 64-bit type the shift does not exist, so in
a debug build the check aborted:

```
attempt to shift left with overflow
```

This is not a rare corner. `1ul` triggers it exactly as `18446744073709551615ul`
does, because both are 64-bit unsigned, and **every** `u`/`ul` constant in every
program went through it. The compiler panicked on the most ordinary C there is.

The fix checks the width before the shift. The order *is* the defect: the guard that
made the shift safe was written after the shift, so it was never the reason the shift
was safe.

### Defect 6 — an `unsigned long` constant above `LONG_MAX` silently became zero

`constant()` in `ir.rs` — the path from a constant expression to IR — read a literal's
digits with `i64::from_str_radix`. `18446744073709551615ul` does not fit in an `i64`,
so the parse *failed*; and a failed parse in that position meant `None`, which the
caller turned into **zero**.

So `18446744073709551615ul` compiled cleanly, linked, ran, and was `0`. No
diagnostic, no trap, and nothing visibly wrong except the number. The fix reads the
digits as a `u64` — which is what a constant's digits *are* — and reinterprets the
64 bits only afterwards.

### Defect 7 — the range check derived the type from a zero value

With defect 5 fixed, constants C *should* have accepted started compiling and
immediately produced nonsense: `5000000000` and `0x80000000` were both refused with
*"`5000000000` does not fit in a `int`"*. The range check called
`constant_type(number, 0)` — deriving a constant's type from a **zero** value — so
every unsuffixed constant looked like a small `int` and anything above `INT_MAX` was
rejected. C says `5000000000` is a `long` and `0x80000000` is an `unsigned int`; both
are legal and both were errors.

The check now derives the type from the magnitude, using the ladder C states: decimal
constants climb `int` → `long` → `long long` and never become unsigned, while hex and
octal climb `int` → `unsigned int` → `unsigned long`. That asymmetry is pinned by
`a_wide_decimal_constant_becomes_a_long_and_a_wide_hex_one_an_unsigned`, which asserts
`sizeof(0x80000000) == 4` and `sizeof(2147483648) == 8` — same magnitude, different
type, different size.

This one is worth recording for a reason beyond itself: **it was hidden by defect 5**.
The panic fired first, so the wrong range check never got to produce a wrong answer.
Fixing a crash can unmask a bug underneath it, and the honest response to a crash fix
is to re-run everything and see what was waiting behind it, rather than to assume the
crash was the last of it.

### The specifier matrix, now permanent

`hardening_c_types.rs` covers 23 spellings, each measured three ways:

- `sizeof` — catches the **width**, and nothing else.
- A value above `INT_MAX`, printed back as unsigned — catches the **sign**, and
  nothing else.
- An all-ones value compared `< 0` — catches the sign a *third* way, and is the one
  that fails when a truncation lands the same way by accident.

Plus widening in both directions for every width, because the original defect was in
widening and shared code can regress a width it is not tested at.

Three more test defects here, all of the same family as H2's and H4's:

- The one-byte rows' expected values were hand-computed from `0xFF` as though the
  cast to `unsigned long` were zero-filling. C says converting a negative value to an
  unsigned type adds `2^N`, so a *signed* `char` holding `0xFF` widens to `0xFFFF…FF`,
  not to `255`. The table now says all-ones for the signed one-byte rows and `255`
  for the unsigned one.
- The test used the C runtime's `print_decimal`, which takes a **signed** `long`, so
  every value with its high bit set printed as `-1` and could not be parsed as a
  number. The file now brings an unsigned printer and checks it against known values
  before any measurement depends on it.
- A probe used `0o777` as an octal literal. C has no `0o` prefix — octal is a leading
  `0`. The frontend was right to reject it and the probe was wrong.

---

## Running totals

| | before | after |
| --- | --- | --- |
| tests | 1176 | 1263 |
| confirmed defects | — | 9 (7 in the C frontend, 1 in the runtime, 1 in the debugger) |
| test defects found and fixed | — | 22 |
| clusters audited | — | 11 (H1, H2 twice, H3/H11, H4, H5, H6, H7/H8, H9, H10, H12) |
| new tests | — | 87 |

Seven of the eight confirmed defects are in the C frontend, and all seven were found by a
*differential* or *property* test rather than by reading code. Nothing in the suite
had ever asked the C frontend to agree with C, and nothing had asked a constant to
be worth anything.

The eighth was the exception that proves the rule. It was not hidden by a missing
test at all — it was a defect **recorded in this repository, with a reproduction and a
theory**, for a whole cluster, and the theory was wrong. The reader was right about
the address and wrong about which memory owned it. What finally found it was asking a
question step 97.s investigation had not: not "which of these two addresses is right"
but "read the bytes through each of them and see which one holds the picture".

The ratio is the phase's main result so far, and it is worth stating plainly rather
than leaving to be inferred: on this platform the **implementation has been more
reliable than the tests that describe it**. Fifteen test defects against seven
implementation defects, and the pattern in the test defects is consistent — a
hand-computed constant that was stale, a case whose values did not distinguish the
behaviours it claimed to compare, a baseline taken before a legitimate write, and one
assertion that demanded a feature (a checksum) the format was never going to have.

Every one of those is a test that would have passed, or failed, for the wrong reason,
which is worse than a test that does not exist: a test that is confidently wrong
teaches its reader something false. That is the thing to watch for in the remaining
clusters, and it is the reason the rule from H4 — *derive the expected value in the
test* — is now stated as a convention rather than as a habit.

## Open items

- **No checksum in the object format.** Corrupting a data field yields a valid
  object rather than a rejected one. Not a current threat model (objects are local to
  the toolchain) and not fixed, deliberately: adding a digest to a binary format is a
  design change, not a hardening patch, and it is better made once than retrofitted.
- **The C constant work touched `types.rs` and `ir.rs`, so the full C campaign should
  be re-run against a clean tree** rather than trusted because its own tests pass.
  Done: 1240 tests, clean fmt, clean strict Clippy, green `nix flake check` and
  `nix build` at the time of writing.
- **`lazalith-c-runtime` and `lazalith-runtime` are dev-dependencies** of
  `lazalith-c-compiler`, having briefly been regular dependencies when the
  cross-frontend test was written. Corrected, and the architecture checks re-run.
