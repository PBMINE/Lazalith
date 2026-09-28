# Fuzzing

`lazalith-fuzz` is the answer to one question: **what happens when something reads
bytes it did not write?** Ten things in this repository do, and they are the ten in
step 86:

| target | what it reads | what "correct" means |
|---|---|---|
| instruction decoder | raw bytes as an instruction | it re-encodes to the same bytes |
| assembler | text as assembly | twice gives the same object, and the object reads back |
| Lazen parser | text as Lazen | twice gives the same answer |
| C parser | text as C | twice gives the same answer |
| object reader | bytes as a `.lzo` object | the object survives a re-encode, and one pass reaches a fixed point |
| executable loader | bytes as an `.lzx` image | every section lies inside the image, and the entry names one of them |
| kernel loader | bytes as a boot ROM | the image has a kernel, and its machine setup holds the load address |
| filesystem metadata | a path | the path reports *its own* file, and a refusal says why |
| debugger commands | controls with no program loaded | every one refuses, and no refusal moves a register |
| snapshot reader | bytes as a machine's state | a restored processor is in the state it was captured in |

Run it:

```text
cargo run -p lazalith-fuzz -- --iterations 200000 --seed 7
cargo run -p lazalith-fuzz -- --target "object reader" --seed 11
cargo run -p lazalith-fuzz -- --list
```

## Why there is no `cargo-fuzz`

No libFuzzer, no `arbitrary`, no coverage instrumentation, for the same reason step
84 wrote its own property generator: this repository has no third-party Rust
dependencies, and a fuzzing harness is a hundred lines of generator and a driver.

The trade is real and worth stating plainly. A coverage-guided fuzzer finds deeper
bugs in less time on a single target. A deterministic campaign finds the same class
of bug every time, runs on every commit, and never flakes. For a repository whose
central claim is reproducibility, the second is worth more than the first — and the
`--iterations`/`--seed` pair means a failure is a command line rather than an
afternoon of guessing.

## The invariant, and why it is about outcomes

"Malformed data must not silently corrupt state" is a statement about *answers*, not
about crashes. Each target has three acceptable answers and one unacceptable one:

- **Refuse**, with a reason. A target that refused everything would pass every test
  here, so each one also has a second half that requires an acceptance.
- **Accept, and be self-consistent.** A decoded instruction re-encodes to its
  bytes; an object that reads re-encodes to an object that reads the same; a
  snapshot restores a machine in the state it was captured in. This is the half that
  catches "it accepted the input and quietly changed the state", which is the
  failure the step is about and the only one a no-panic test would miss.
- **Neither**: a panic, a hang, an overflow, or an acceptance that is not
  self-consistent. A panic is *caught* rather than avoided, so one broken target does
  not hide the other nine.

## What it found

The object reader, on its first campaign, through a check that turned out to be
wrong.

The target originally demanded that a file re-encode to itself, byte for byte. The
fuzzer produced a file that read cleanly and re-encoded to *eight bytes more*. It
turned out to be a perfectly valid object: one byte in a string table had been
changed from a NUL to something else, which merged two adjacent names — `text` and
`_start` — into the single longer name `text\x01_start`. The file is legal, the
reader was right to accept it, and the writer is right to emit eight more bytes for
a seven-byte-longer name.

The check was the bug, not the reader. Demanding byte-identity would have rejected
valid objects, and the fix was to state the property that actually matters: **one
pass through read-then-write reaches a fixed point.** A linker that rewrites a file
twice now produces one file, which is the property a linker needs, and a valid object
in a non-canonical spelling is still accepted.

Two other things the harness got wrong the same way, and both are worth recording:

- A parser target treated a refusal as a *failure*. For a parser, refusing malformed
  source is the correct answer; treating it as a bug made every mutation look like a
  finding. The load-bearing half of a parser target is determinism — the same text
  must give the same answer twice, which is what catches a resolver that depends on
  hash order.
- An empty string table and a zero-byte code region are not "malformed input" but
  "there is no program", and both have to produce a machine that stops at the start
  rather than a refusal to build one.

## What it does not do

- **No coverage guidance.** See above.
- **One architecture.** Every target builds for `lz64`. A second architecture would
  double the campaign for the same code paths, and the widths a second one changes
  are already property-tested in step 84.
- **The snapshot target fuzzes the restore path, not a file format.** There is no
  byte-level snapshot format in this repository yet, so "the snapshot reader" is
  `CpuSnapshot::restore`, and the invariant is the round trip. A persisted snapshot
  is step 96's business and will need its own target.
- **The debugger target drives commands, not text.** The command surface is
  `lazalith_gui::Control`, and a key is a scancode rather than a line of text, so the
  input is a sequence of controls. A text REPL would be a different parser with the
  same determinism requirement.
