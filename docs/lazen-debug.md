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

## Source-level debugging

A program built from Lazen carries where its code came from: every statement is
marked as it is lowered, the mark becomes a code offset as instructions are
emitted, the object carries those offsets with the source text they point into,
and the linker rewrites them to the addresses that code actually got in the
image. So a program counter can be answered with a file and a line, and the
answer comes from the build rather than from a table anyone wrote by hand.

Three questions, and their honest answers:

- `source_location()` — where is the program counter, in the source it was
  written in? This walks *backwards* to the mapping at or before the address, so
  it answers for a PC in the middle of a statement as well as on its first
  instruction.
- `source_location_at(address)` — the same, for an address that is not the
  program counter. This is what makes a stack view readable: the return addresses
  on a stack are addresses the program passed through, and each resolves to its
  own line.
- `set_source_breakpoint(process, name, line)` — walk *forwards* over the mappings
  that start on that line and set a breakpoint on each.

`set_source_breakpoint` returns the addresses it resolved to, and **an empty
answer is not an error**. A line can be a comment, a declaration with no code, a
blank line, or a branch the backend never emitted. "No code for that line" is the
answer; a breakpoint on whatever instruction happened to be next would be a
breakpoint the user did not ask for, on a line they did not write. A frontend
that wants the conventional "first statement on the line" takes the lowest of the
addresses returned.

The line is found by resolving each mapping's start offset through the file's own
line map — the same map that made the offsets. Comparing offsets against a line
start instead would be the same answer written a second way, and a second way to
be wrong.

`debug_info()` returns the table itself, or `None`. A program assembled by hand,
or built by a toolchain that had no source, has no block: `source_location()`
answers `None` and no source breakpoint can be set, while every address-level
feature keeps working and the program runs exactly as it would without a
debugger. Losing debug information must not change what a program *does*.

The stack is still not a call chain. Source information does not fix that: the
calling convention reserves the return address below the frame and records no
frame pointer, so there is nothing to walk. A frontend that wants frames needs
the ISA to grow a frame pointer first.

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

## Machine snapshots

Step 75 added four types to this crate, and a `Device::snapshot` / `Device::restore`
pair to the device trait.

| Type | What it holds |
| --- | --- |
| `CpuSnapshot` | the architectural registers, the execution state, and the trap frame stack |
| `DeviceSnapshot` | one device's guest-visible state, as that device encoded it |
| `ProcessSnapshot` | a process's state, memory, threads and handles |
| `MachineSnapshot` | the three above, for a whole machine |

`DebugController::snapshot_machine` and `restore_machine` are the entry points.

### Only guest-visible state, and the list of what is not

The roadmap's sentence is the whole design constraint, so the exclusions are the
interesting part:

| Excluded | Why |
| --- | --- |
| a device's elapsed clock | a device ticks against it, but nothing in the register file reports it and no guest instruction can observe it |
| a console's emitted output | a console hands bytes to a *host*; its registers are write-only from the guest's side, so its output is not the guest's to see again |
| the machine's instruction count and virtual clock | host bookkeeping about how the machine got here |
| a framebuffer's pixels | those belong to the guest, and they are in the process's memory — a snapshot that copied them would hold a second copy of every one |

`a_snapshot_carries_no_host_state` and
`a_display_snapshot_leaves_the_clock_out` hold these.

### Why these are clones and not encodings

Every type but `DeviceSnapshot` holds a clone of the state it names. An encoding
has to be kept in step with the thing it encodes, and a snapshot that silently
omits a field is a snapshot that restores a machine which is *almost* the one that
was saved — which is the failure this step exists to prevent. A clone cannot
forget a field, so a field added to a process without a decision here is a field a
snapshot carries.

`DeviceSnapshot` is the exception, and deliberately: a device's state is a
device's business, and a common encoding in the `Device` trait would have to be a
lowest common denominator that lost whatever made each device different. A
display's snapshot is the window; an input device's is the whole queue, because a
program *owns* that queue — it drains it, and a snapshot that left the drained
events out would hand the same keystroke to the program twice, which is the one
thing about input the guest can observe.

`restore` checks the bytes are that device's own, so a display's state cannot be
put into an input device and a snapshot cannot be padded into a shape the device
would have refused.

### A restore is checked before anything is written

`restore_machine` compares the process count and the device list *before* it
touches anything, so a snapshot taken from a machine with a different shape is
refused rather than half-applied. A snapshot that named the wrong process and put
its memory into another would be worse than no restore at all.

A restore also brings the **sessions** back into line with the processes they
watch. That was a real bug: a program that had exited when the snapshot was taken
came back as a *running* process with a session that still said it had exited, so
`run` refused to continue a machine that was perfectly able to. The restore worked
and the debugger did not believe it.

## A debugger cannot step into a syscall

`Kernel::step` traps, dispatches **and** returns from the syscall before it comes
back, so the machine is never at rest inside one. A debugger can therefore not
stop inside a syscall on this machine, and no amount of snapshot support changes
that: it is the shape of `Kernel::step`, not of the debug API.

This is a real gap and it is recorded here rather than papered over. The
`CpuSnapshot` still carries the trap frame stack, because a machine *can* rest in
a trap and a snapshot that dropped the frames would be a snapshot that silently
omitted the return path of whatever was stopped in one — and
`a_snapshot_carries_the_whole_processor` holds the snapshot and the machine to
agreeing about a frame at every step of a run.

Making a syscall a stopping point means splitting `Kernel::step` so a trap is
observable between two steps. That is a change to the kernel's shape, not to the
debugger, and it belongs with the work that makes a debugger able to show a
program's system calls rather than only its arithmetic.

## Structured diagnostics

A program that does something wrong is reported as structure, not as text. A
`RuntimeDiagnostic` **is** a `lazalith_diagnostics::Diagnostic` — the same stable
code, severity, message and source label the compiler produces — plus what only
a running program has:

```text
code              R0001, a stable identifier in the runtime namespace
message           the trap's cause and the payload the program supplied
source            the file, line and column, from the image's own debug block
guest PC          the address of the trapping instruction
instruction       the disassembler's text, for a person to read
stack trace       the calls that led here, each verified against the code
```

Codes are `R0xxx`, extending the compiler's `L`/`P`/`N`/`T` namespaces. A
frontend can filter on the letter and tell a program that did something wrong
from a frontend that could not read something, without parsing either message.

The guest program counter is the address of the **trapping instruction**, not the
machine's program counter. A machine that has taken a trap is in the kernel's
trap frame, so its `pc` is the trap vector; a debugger that pointed there would
send a user looking at the kernel instead of at their own program. The address is
derived from the trap's resume point and then *verified* — the instruction there
has to decode as a `TRAP`, or no address is reported at all.

### A call chain is verified, not guessed

`call_chain` scans the stack for words that could be return addresses and checks
each one against the code. A word becomes a frame only if it is
instruction-aligned, the instruction immediately before it is a `CALL` or
`CALLR`, that call's target is itself real code, and the word resolves through
the debug table. A data word that merely looks like a code address would also
have to be preceded by a real call to a real function, so it does not become a
frame.

The scan reads a word at a time and keeps what it could read, because a trap
frame sits on the stack and its top can be past the end of the mapped region.
A trace that stopped because the memory did says how many words it looked at, so
a short trace reads as a short trace rather than as a truncated one nobody was
told about.

`instruction_at` is the structured counterpart to `disassemble`. Anything that
needs to *match* on an instruction — which is how the trace check works — uses it
rather than reading the disassembler's text.

### A fault stops a run

A run checks whether the process has faulted before each step. A fault that does
not stop a run is a fault nobody ever sees: the machine sits in a trap frame with
nothing able to leave it, the next run finds no runnable process, and a program
whose bounds check fired is reported as a program that finished.
