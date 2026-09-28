# The whole chain

Step 96 asks for a test that performs this and verifies the expected result:

```text
Lazen source
 ↓
Lazen compiler
 ↓
Lazalith object
 ↓
Linker
 ↓
.lzx
 ↓
LazOS loader
 ↓
Process
 ↓
Syscalls
 ↓
Virtual hardware
 ↓
Emulator
```

`crates/lazalith-runtime/tests/pipeline.rs` does exactly that, in seven tests that
between them walk every arrow. What is worth writing down is what the step *found*,
because the two things it found are the reason it was worth writing.

## The boot-and-run path had no test under it

The sequence that boots a machine, hands off from the supervisor kernel, drives the
kernel loop, and reports a program's output and status lived in the `lazen` command's
`run.rs`. That was defended in a comment: *"the runtime crate builds and links images;
it deliberately does not own a machine, because a library that started one would be a
library with a global."*

The first half of that is right. The second half is a non-argument: the function
**returns** the machine, so it owns no global state, and "a library with a global" is
a claim about a design that was never the one in question. And the consequence of
where it lived was concrete — **the one code path in the project that boots a
compiled program had no test under it**, because the only way to reach it was to build
the `lazen` binary and run it as a subprocess. Every other layer was covered by
hundreds of tests; the layer where all of them meet was covered by a shell
invocation.

So it moved to `lazalith_runtime::run`, and both `lazen run` and this test call it.
The step budget moved with it, so the command and the test agree on what "unfinished"
means rather than each having their own number. This is the change that makes the rest
of the step possible, and it was a mistake that had been sitting in a comment.

## The syscall count was being inferred from an event that no longer existed

`Finished` reports how many syscalls the program made, which is how a test tells
"the output arrived through the kernel" from "the output appeared". The first
implementation counted `MachineEvent::Trapped` in the step the kernel returned, and
a program that printed two lines reported **one** trap.

The reason is the kernel's own design, and it is correct: `LazalithKernel::step`
*replaces* the step that trapped with the step that returned from the syscall, so
the trap event has been consumed by the time the step is returned. The runner was
counting an event the kernel had already overwritten.

So the count is now taken from what the kernel reports: a dispatched syscall comes
back as `Return`, and the last one as `Exit`. That is the kernel's own account rather
than an inference from something it discarded, and it is a better measure for it —
a trap count would also have counted a guest's `TRAP`, which is *not* a syscall and
which the kernel deliberately reports under a different name.

## What the seven tests assert, and where

| test | arrow it pins down |
| --- | --- |
| `a_lazen_program_runs_from_source_text_to_console_output` | the whole chain, end to end, with the exact bytes on the console |
| `a_programs_output_reached_the_console_through_syscalls` | **syscalls** — the output cannot arrive without traps, and a run that reported zero would mean the kernel was not involved |
| `a_programs_exit_status_is_the_status_its_main_returned` | **process** — four different statuses, so a constant cannot pass |
| `a_program_computes_its_own_output_rather_than_repeating_a_literal` | **compiler + emulator** — a loop that sums one through ten, printed as 55, exited as 55 |
| `the_image_is_read_back_through_the_file_format_rather_than_used_as_built` | **linker → .lzx** — the image is serialised, read back with the file reader, re-serialised, and run twice with the same answer |
| `a_device_is_reachable_from_a_running_program` | **virtual hardware** — a device attached to a booted machine, not to a machine in isolation |
| `a_program_that_runs_away_is_reported_rather_than_waited_on` | **emulator** — the step budget, reported as *unfinished* rather than as a crash |

Two of these are worth expanding, because they are the ones a weaker test would skip.

### The image is read back, twice

The runner takes **bytes**, not a decoded image, and decodes them with the same
`LzxImage::from_bytes` a person's `.lzx` file gets. A run that used the in-memory
image it was built as would skip the serialiser, and a serialiser that was wrong
would pass — right up to the day somebody built a program and ran it. The test goes
further and re-serialises the decoded image and runs *that* as well, requiring the
same output and status, so the reader cannot be the only thing being trusted.

### The program computes its own output

`a_program_computes_its_own_output_rather_than_repeating_a_literal` runs a loop that
adds one through ten, prints `55`, and returns 55. It is the strongest end-to-end
claim the chain can make about arithmetic: Lazen's `while`, the type checker's
arithmetic rules, the lowerer's comparison and branch, the code generator's encoding
of both, the emulator's execution of them, and the syscall that puts the result on a
console. Every stage is load-bearing for the number on screen, and no stage can be
replaced by a stub that returns the right answer.

## What this step does not cover

- **C.** The diagram in step 94 has two frontends feeding one IR, and this chain
  exercises the Lazen half. The C front end has its own end-to-end tests
  (`lazalith-c-compiler`'s `convergence.rs` and `end_to_end.rs`) and converges on
  the same object format; a single test that compiled one C program and one Lazen
  program and ran both under the same kernel would be a good future addition, and
  this step does not pretend to be it.
- **`.lza` packages.** Step 89's resolver and step 91's capability gate are in the
  path only for packages, and this step runs a bare `.lzx`, which is the *trusted*
  half of the design. A run of a package-built image would exercise the other half,
  and `docs/os-expansion.md` says which.
- **Performance.** `Finished` reports an instruction count, and nothing here asserts
  anything about it. Step 93 measured throughput deliberately in an example rather
  than in a test, and this is the same call: a number in a test is a number that
  fails on a busy machine.

## Verification

- 7 tests, all of which boot a real machine and run a real compiled program.
- `lazen run` and `lazen test` still work through the shared runner — the CLI's own
  36 tests pass unchanged, which is the check that the move did not change the
  command's behaviour.
- 1144 workspace tests pass, and fmt, Clippy, check, `nix flake check` and
  `nix build` are green.
