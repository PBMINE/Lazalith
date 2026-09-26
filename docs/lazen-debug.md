# Lazalith Debug API

`lazalith-debug` is how a frontend drives a running program. It has two types:
`DebugController`, which owns a machine and a kernel, and `DebugSession`, which
holds one process's debugging state.

## The rule this API exists to enforce

**A frontend cannot manipulate CPU internals directly.**

That is not a promise in a document; it is the shape of the API.

- There is no `&mut LazalithMachine` anywhere in the public surface, and no method
  that hands one out.
- `DebugController::registers` returns an **owned** `RegisterSnapshot`, not a
  reference into the CPU's `RegisterFile`.
- `read_memory` returns an owned `Vec<u8>`.
- `disassemble` returns owned text.
- There is no "write a register" and no "write memory", because there is no way
  to do either that leaves the machine's own checks in place.

A `&RegisterFile` would have been the natural signature and would have been the
wrong one. `RegisterFile` is the CPU's own storage, so a shared reference to it is
a view of live state that changes under the caller: a frontend reading `r0` twice
would get two answers with no step in between, and a frontend that decided it
needed to *write* would find the obvious next step is to ask for a mutable
reference. An owned copy is a consistent snapshot, and the missing write path is
the point.

This matters because the kernel validates the active binding on *every* step, so
that nothing upstream can skip it. A debug API that leaked the machine would undo
that from the other direction.

## Execution control

| Call | What it does |
| --- | --- |
| `run` | continue until a breakpoint, a watchpoint, a pause, the exit, or the step limit |
| `continue_` | the same, named for what a user types |
| `step` | one instruction |
| `pause` | ask to stop at the next instruction boundary |

A **pause** is cooperative. It is checked at an instruction boundary, so a program
already inside a syscall is allowed to finish the call it is in. Refusing to
interrupt a validated syscall would leave a program blocked in a driver
undebuggable; interrupting one part-way would leave a kernel structure
half-updated.

A pending pause is taken by the next `run` and *consumed* by it, not discarded at
the start. The moment a frontend asks for a pause is the moment it is stopped,
changes a breakpoint, and continues — so clearing the request on entry would
discard exactly the pause that mattered.

The **step limit** is a bound on how long a *debugger* waits, not a change to the
machine. A program that never stops is a program the user asked to run, and a
debugger that hung instead of returning would be the bug.

## Breakpoints

A breakpoint is refused unless it is on an instruction boundary. One between two
instructions would never be reached, and a frontend that set it and was then told
"no breakpoint was hit" would be reporting a mistake of its own as a fact about
the program.

A run that stops on a breakpoint reports the address the program counter is *at*,
which means the instruction there has **not** run. A breakpoint that stopped after
would report the address already executed, and a frontend showing source would
highlight the wrong line for every stop.

Breakpoints belong to a **session**, not to the controller. Two processes on one
machine have two address spaces, and an address that is a breakpoint in one is
just an address in the other.

## Watchpoints

The machine has no watchpoint register: `lazalith-cpu` has a `DebugState` with a
single `single_step` flag that nothing reads, which is all the debug surface the
ISA grew. So a watchpoint here is a **comparison** — the bytes under the watched
address are read before each step and compared after, and a change is a hit.

That is correct at instruction granularity, which is the finest granularity a
program can be stopped at anyway. It costs one read and one comparison per
watchpoint per step, and it is stated here rather than hidden because a frontend
that watches a hot address should know that before it does.

Sizes are one, two, four or eight bytes. A watchpoint that is not a power of two
straddles two words and would have to compare both, and a frontend offering a
width it cannot honour is worse than one that refuses.

## Inspection

- `registers()` — an owned snapshot: sixteen general registers, the program
  counter, the stack pointer, the status register and the privilege, all read at
  the same instant.
- `read_memory(address, length)` — an owned `Vec<u8>`. A read must be a whole
  number of words: a partial word is a caller mistake, and returning the words
  that *were* whole would look like a short read of memory rather than a refusal
  to read it.
- `stack(words)` — the stack pointer and the words above it, and
  `has_call_chain: false`. **There is no call chain.** The calling convention
  reserves the return address below the frame but records no frame pointer, so a
  walk would be a walk of whatever numbers happened to be on the stack. A
  debugger that printed addresses and called them a call stack would be showing a
  heap of numbers, so the field says so.
- `disassemble(address, count)` — one instruction at a time, stopping at the
  first byte that is not a canonical instruction. A debugger walking a range that
  runs into data should show the address it stopped at, because "where does the
  code end" is a question a frontend has to be able to ask.

## Snapshots

`DebugSnapshot` captures a **session's debugging state**: its breakpoints, its
watchpoints, where it was stopped, and how many instructions it had retired.

It does **not** capture the machine. Machine state — the CPU, the devices, the
processes — is Step 75's subject with its own types, and a snapshot of the
debugging state is useful on its own: a frontend can save a session's setup, run
the program to completion, and put the setup back without typing the addresses
again.

A restore is refused into a *live* session, because changing breakpoints under a
`run` in progress would make the run stop somewhere nobody asked about, and refused
into a *different* process, because silently doing that would put stops in the
wrong address space.

## Verifying the contract

Thirteen tests in `crates/lazalith-debug/tests/debug.rs` drive real machines over
real `.lzx` images. They state that a frontend reads registers without being able
to write them, that a breakpoint stops the program *before* the instruction at its
address, that an off-boundary breakpoint is refused and not recorded, that a
watchpoint fires where the bytes changed and does *not* fire when they did not,
that `step` retires exactly one instruction, that a pause is taken at a boundary
and consumed once, that a run with nothing to stop it stops at its limit, that
memory inspection reads the program's own bytes and refuses a partial word, that
the stack says it is not a call chain, that disassembly names where it stopped,
and that a session snapshot round-trips and refuses the wrong process.
