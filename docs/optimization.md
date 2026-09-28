# Optimization

Step 93 lists four possibilities:

```text
optimized interpreter
faster memory paths
instruction caching
JIT
```

and one rule: **every optimized implementation must preserve reference behavior.**

The rule is the step. The four items are not four features; they are four answers
to "where does the time go", and answering that question honestly is most of the
work. So this document says what was measured, what was built, what was measured
again afterwards, and what was deliberately left alone.

## What was measured first

A loop body is fetched, decoded, and executed over and over. The obvious
suspicion is that decoding is a large share of that. It is worth checking rather
than assuming, and the check is cheap: `crates/lazalith-memory/examples/decode_speed.rs`
runs the same straight-line program on a bus that caches decoded instructions and
on one that does not, and reports instructions per second for both.

Two measurement decisions are worth naming, because a number is only as honest as
the way it was taken:

- **No threshold is asserted.** A timing assertion fails on a busy machine and
  teaches a team to ignore the test that matters. The example prints; the tests
  assert behaviour.
- **No benchmarking framework.** The project takes no third-party Rust
  dependencies, and a dependency is a worse thing to add than a number printed by
  a program anybody can read.

The measured result, on a release build of straight-line arithmetic:

| bus | lz32 | lz64 |
| --- | --- | --- |
| reference | 4.7 M inst/s | 5.0 M inst/s |
| cached | 7.8 M inst/s | 9.9 M inst/s |
| **relative** | **1.67x** | **2.00x** |

So decoding was worth roughly a third of the time, and that is a real enough share
to be worth a cache and not so large that a cache is a substitute for a better
interpreter.

## The instruction cache

`InstructionCache` is a direct-mapped table of 512 slots. An instruction is
decoded once per address instead of once per fetch. `Instruction` is `Copy` and
small — an opcode and three operands — so a hit hands back a value rather than a
borrow, which is what lets the fetch path stay free of aliasing problems.

### The rule the design is built around

**A cache may skip work, never a check.** So the checks are not in the cache at
all. `Bus::checked_fetch` does every check an instruction fetch owes — the
configuration agrees, the program counter is valid, the address is not a device
window — and only after they all pass is the table consulted. There is one copy of
those checks and both the cached and uncached paths call it, so the two cannot
drift apart.

That is the whole safety argument, and two tests hold it up. `a_warm_cache_does_not_
rescue_a_fetch_that_should_have_faulted` plants an entry at an *unaligned* program
counter on purpose — a test that only ever warms valid addresses proves nothing —
and requires the fetch to fault. `a_warm_cache_does_not_hide_a_configuration_
mismatch` asks a 64-bit bus about a 32-bit fetch and requires the configuration
fault. If somebody later moves the lookup above the checks, both fail.

### Invalidation, and the bug the first version had

The first version of `invalidate_range` scanned all 512 slots on every write. It
was correct, and it would have made the cache a *tax* on exactly the programs it
should help: 512 comparisons to invalidate one instruction, on every store, in a
loop that stores. A cache that makes a write-heavy program slower is not a cache.

So invalidation walks the addresses the store could have touched — `length / 8 + 2`
steps, two for a byte store — and their slots. That is exact rather than
approximate, and two tests check it by its *result* rather than its cost:
`a_store_forgets_exactly_the_instructions_it_overlaps` stores one byte inside the
second of four cached instructions and requires exactly one entry to go and three
to stay, and `a_store_spanning_two_instructions_forgets_both` stores eight bytes
starting four bytes into one instruction and requires two entries to go. A walk
that visited the wrong addresses would either leave a stale instruction — the bug
this step exists to prevent — or clear a neighbour it had no business clearing.

The overlap rule is stated about *ranges* rather than addresses, so a store that
begins in the middle of an instruction invalidates it. An equality test would get
that wrong, and it is the case a self-modifying program actually hits.

### Self-modifying code is a test, not a footnote

`writing_an_instruction_makes_the_cached_copy_be_ignored` runs a program, lets the
cache fill, overwrites a decoded instruction with a *different* instruction, and
requires the program to do the new thing. A cache that forgot to invalidate would
execute the old instruction, and the program would do something its author never
wrote — the single worst failure mode this platform has, because it is silent and
it is reproducible only sometimes.

### Turning the optimization off

`Bus::reference` is a bus with no cache: it decodes every instruction every time,
which is what the interpreter did before this step. It exists so the step's rule
can be *tested* rather than asserted. `a_caching_bus_and_a_reference_bus_run_a_
program_identically` runs one program on both and compares the program counter and
all eight registers after every single instruction. A timing comparison cannot
catch an optimization that changed behaviour; this can, because it compares
everything a machine can be observed doing.

It also means a future optimization has a pattern to follow: add a reference path,
and a test that the two agree.

## Faster memory paths: not done, and what blocked them

A memory path is where the next win probably is, and the reason it is not in this
step is that the fix is not a local change. Two observations from the code:

- **`step` validated the fetch twice.** `step` called `validate_fetch` and then
  handed the bytes to `step_bytes`, which called it again. That is fixed here, and
  it is a real win, but it is small: two control-register tests.
- **The architectural state is cloned per instruction.** Every step clones the
  register file and control state, executes on the clone, and commits it. This is
  almost certainly the largest single cost left — larger than decoding — and it is
  also the one place where a change is *observable*: a program can fault, and what
  the faulting instruction had already written must not survive. The commit
  discipline is what makes a fault transactional, and it is load-bearing for the
  trap behaviour in `docs/isa.md`.

So the next step in this direction is a register write barrier, not a clone
removal: write through to the real state, and keep a journal of what an instruction
changed so a fault can roll it back. That is a design with a real correctness
argument in it, and it belongs in its own step where the argument can be read.

## JIT: no

**No JIT, and not because it is too big.** Because on this platform it would be
optimizing the wrong thing, three times over:

1. **The target is not the bottleneck yet.** The interpreter spends its time in
   validation, cloning, and memory bookkeeping — all of which a JIT generated from
   this IR would still have to do, because the IR does not carry enough
   information to skip them. A JIT over an unoptimized IR produces unoptimized
   machine code.
2. **The ISA is the platform.** A JIT needs a register allocator, a calling
   convention, and a set of legal encodings, and step 94's work is already showing
   what that costs. Writing a second implementation of the semantics that the
   interpreter already has, and trusting it to agree, is exactly the kind of thing
   step 87's differential harness and step 93's reference bus exist to make
   expensive.
3. **There is no compilation story for the guest.** A JIT that compiles a guest
   program would make guest execution time depend on how much host CPU it got,
   which makes an emulator non-deterministic — and steps 86 and 87 built the
   determinism that lets a failure be reproduced from a log. A compiler that
   silently reintroduces non-determinism would undo both.

The honest summary: the interpreter is not yet slow for an interesting reason, and
the interesting reasons are all "the surrounding design costs more than the
decoding does". `docs/optimization.md`'s next entry is the write barrier.

## What this step did not touch

- **The `AddressSpace` also implements `CpuMemory` and has no cache.** Only the
  bus caches, because the bus is what a machine runs on. A direct
  `AddressSpace` user — the differential fuzzer's flat memory is its own type — is
  unaffected.
- **`step_bytes` is unchanged and does not consult or fill the cache.** It is the
  path that takes bytes from somewhere other than memory, which is what the
  differential fuzzer feeds it, and a cache that answered from a stale table when
  handed deliberate bytes would defeat the point of the fuzzer.
- **No codegen, no link-time, no compiler changes.** The compiler's output is the
  same object files as before, which is the testable form of "the cache does not
  change what runs".

## Verification

- 13 tests in `crates/lazalith-memory/tests/instruction_cache.rs`, covering: the
  cache is used; a warm entry does not rescue a faulting or mismatched fetch; a
  store over an instruction forgets it; a store into the middle of one forgets it;
  a store elsewhere does not; a store spanning two forgets both; a conflicting
  address misses rather than returning the other entry; a cached entry equals what
  its own bytes decode to; a long run leaves the table the same size; and a
  caching bus and a reference bus agree on every register after every instruction.
- The whole suite — 1132 tests — passes unchanged, and every expected value in it
  was written down before this step existed, which is the broad version of the same
  claim.
