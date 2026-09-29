# Lazalith

A self-hosting-ish software platform: a 32/64-bit instruction set, an interpreter that
is the semantic authority for it, a memory model, a kernel, a system ABI, three language
front ends, an object format, a linker, a loader, virtual graphics and input devices, a
debugger with source-level information, snapshots and deterministic replay, and a
command-line toolchain that drives all of it.

The whole thing is one Rust workspace of 24 crates with a single integrated executable,
[`lazen`](#run). It is a **Phase I** platform: the 100-step roadmap is complete and
frozen, and an adversarial hardening phase has since found and fixed twelve real defects
in it.

```text
   Lazen ─┐                    ┌──────────── Lazalith machine ────────────┐
     C ───┼──→ IR ──→ .lzo ──→ link ──→ .lzx ──→ loader ──→ CPU + bus + memory
    Asm ──┘                                              │
                                                          ▼
                                            LazOS  ·  devices  ·  debugger
```

---

## Architecture

### The instruction set and its interpreter

`lazalith-isa` defines the instruction encoding, the metadata table that drives both
the encoder and the disassembler, and the two widths the platform ships: **LZ32** and
**LZ64**. Instructions are a fixed 8 bytes. The ISA is defined *once*, in one table, and
the encoder, decoder, disassembler and assembler all read it — so a new instruction
cannot be added to one of them and forgotten in another.

`lazalith-cpu` contains the **Reference Interpreter**: the executable statement of what
the ISA means. It is not an optimisation and not a second implementation for speed; it
is the definition, and every other path is checked against it. On top of it sit the
arithmetic operations, the trap controller, and the architectural/execution state a
program can observe.

`lazalith-machine` turns a Reference Interpreter into a running machine: reset vectors,
the trap vector, stepping, and the context switching that swaps a process's memory in and
out of the bus.

### Memory, the bus, and devices

`lazalith-memory` is the address space: regions with permissions, an address-space
*identity* used for context switching, and the fault kinds. Its central promise is
**validate before mutation** — a refused access leaves every byte exactly as it was,
which `hardening_validate_before_mutation.rs` checks in twelve different ways.

`lazalith-devices` holds the virtual devices: the display (windows, presented frames,
geometry), input (an event queue with delivery counts), and the bus-facing device
interface. `lazalith-boot` builds the initial machine image: the boot ROM, the reset
vector, and the kernel load.

### LazOS and the system ABI

`lazalith-os` is the kernel: processes, threads, a round-robin scheduler with
preemption and context switching, the system calls, a virtual filesystem, the
`lazos` userspace model, a package premise resolver, and a headless shell. It is a
**Rust kernel on the host**, not a guest kernel: it runs as host code beside the machine
and is exercised by running guest programs against it.

`lazalith-os-abi` is the contract between the kernel and a guest: the syscall numbers
and their names in one table, the record layouts with their sizes, open flags, seek
origins, and the validation every call goes through. It is a separate crate precisely so
that "the ABI is defined once" is a property of the build graph.

### The three front ends, and the pipeline they share

- **`lazalith-compiler`** — the Lazen front end: lexer, parser, resolver, type checker,
  and lowering to Lazalith IR. Lazen is the platform's own language, with a standard
  library written in Lazen (`lazalith-stdlib`) and a widget library (`lazalith-ui`).
- **`lazalith-c-compiler`** and **`lazalith-c-runtime`** — a C front end and a C runtime
  library, both substantial enough to compile the runtime itself.
- **`lazalith-toolchain`** — the assembler, the disassembler, and the linker, plus the
  object format.

They converge on one target pipeline:

```text
Lazen ─┐
C ─────┼──→ Lazalith IR ──→ code generation ──→ .lzo object ──→ linker ──→ .lzx image
Asm ───┘         (lazalith-ir, lazalith-codegen)         (lazalith-toolchain)
```

- `lazalith-ir` is the checked intermediate representation: SSA-style blocks, explicit
  terminators, a builder that refuses an instruction after a terminator or a block
  without one. It is also where a small, explicit optional-backend seam lives.
- `lazalith-codegen` turns IR into an object, resolving frames, calling conventions and
  relocation sites.
- **`.lzo`** is the object format — a fixed-layout header, then section, symbol,
  relocation and debug tables, a string blob and a payload. It carries source-level
  debug information (file paths, source text, and instruction-to-source mappings) so a
  debugger can name a line without opening a file.
- **`.lzx`** is the linked image: sections placed, relocations applied, an entry symbol.
  `lazalith-runtime` reads it back from its own bytes and boots it.

### LazOS, devices, debugger

`lazalith-runtime` is the composition root on the guest side: the startup sequence, the
Lazen library every program links, and the runner that takes an image to an exit code,
a syscall count, a terminal transcript, and the pixels the program presented.

`lazalith-debug` is the debug API — a controller and a session over a machine it does
**not** expose: breakpoints, watchpoints, stepping, disassembly, memory reads, stack
views, source locations, **machine snapshots** and **deterministic replay**. A frontend
asks questions; it cannot reach into CPU internals.

`lazalith-sdl3` is the SDL3 boundary: the only crate in the workspace containing
`unsafe`, wrapping a C library's `extern "C"` declarations in a safe API. It draws
rectangles and reads events; it holds no Lazalith logic.

`lazalith-gui` is the SDL3 frontend — a debugger window built entirely on the debug API.
`lazalith-fuzz` holds the fuzz targets for everything that reads bytes it did not write.

---

## Current Status

```text
Phase I ──────────── Steps 1–100 complete. Frozen. Tag: lazalith-phase1-100
Hardening ────────── In progress. 12 of 18 clusters audited; 12 real defects fixed.
Beyond Lazalith ──── Not started. No work in this repository.
```

**Phase I is complete.** The roadmap in [`instruction.md`](instruction.md) is 100 steps,
all implemented, all tested, all documented in [`docs/`](docs/).

**Hardening is an adversarial audit and it is not finished.** It began with a green
1,176-test suite and has since found and fixed **twelve confirmed defects** — ten in the
C front end, one in the runtime, one in the debugger — plus **26 defective tests**. It
is recorded in full, with reproducers, root causes and the tests that fail without each
fix, in [`docs/hardening.md`](docs/hardening.md).

**Not present in this repository.** Stated plainly because these are the things a
reader might assume:

- There is **no JIT** and no register allocator. The IR has an optional-backend seam
  ([`docs/llvm-backend.md`](docs/llvm-backend.md)) and nothing plugged into it.
- There is **no VM manager** of any kind — no VirtualBox-style, no hypervisor, no
  ability to create, configure, snapshot or destroy a machine as an object.
- LazOS is a **Rust kernel on the host**, not a guest operating system written in C and
  running on Lazalith. `lazalith-c-runtime` is a C *library* for guest programs.
- **No Linux port.** No Linux 0.01, and no attempt at one.
- **No hard disk, no real network, no audio, no threads created by a guest**, and no
  `malloc` in the ABI — a guest's allocator comes from the C runtime's arena.

---

## Build

Every command below was run and passed at the frozen commit. All of them need the
repository's Nix environment, which is what pins the toolchain:

```bash
nix develop path:. -c cargo fmt --all --check
nix develop path:. -c cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop path:. -c cargo check --workspace --all-targets --all-features
nix develop path:. -c cargo test  --workspace --all-features
nix flake check path:.
nix build path:.
```

If you have a suitable Rust toolchain directly, the five `nix develop ... -c` commands
are just `cargo` commands and will work without Nix. The two Nix commands need Nix.

`nix build` produces `result/`, a symlink to the built package, containing:

```text
result/bin/lazen          the integrated Lazalith toolchain
result/bin/lazalith-fuzz   the fuzz targets
```

The workspace **forbids `unsafe_code`** (`[workspace.lints.rust]` in
[`Cargo.toml`](Cargo.toml)) and `lazalith-sdl3` opts back in for the one crate that
needs it. Strict Clippy (`all = "warn"`, denied with `-D warnings`) is part of the
build, not a suggestion.

---

## Test

```bash
nix develop path:. -c cargo test --workspace --all-features
```

**1,279 tests pass** at the frozen commit, with zero failures. They are real
end-to-end tests rather than unit tests: the codegen tests run generated code on the real
machine and compare what it wrote and what it exited with; the runtime tests build whole
programs from the prelude and run them under the kernel; the graphics tests draw and read
the pixels back; the replay tests run a program twice from a recorded state and require
identical results; the fuzz targets are run by the suite as bounded campaigns.

`nix flake check` additionally runs the workspace build, the formatting check, the
test suite, and a C probe for the SDL3 boundary — in Nix's sandbox rather than in
whatever state a developer's checkout happens to be in.

---

## Run

Everything below is a real command against this repository.

```bash
# Build once and use the binary directly.
nix build path:.

# The first program.
result/bin/lazen run examples/hello/main.lz
# → Hello, Lazalith

# A window, a moving block, and the keyboard, headless under a test and
# on a real display through the SDL3 frontend.
result/bin/lazen run examples/window/main.lz
```

`lazen` is the toolchain. Its commands:

```text
lazen new <name>        create a Lazen project in ./<name>
lazen check [file]      parse, resolve and type-check, generating nothing
lazen build [file]      compile and link to a .lzx image
lazen run [file]        build, then execute the program under LazOS
lazen test [file]       run a project's tests and report pass or fail
lazen pack [out]        write a .lza from a manifest and a built image
lazen deps              resolve a manifest's dependencies against local directories
lazen fmt [--check] [file]   format a file in the canonical style
```

`lazen run` is the whole platform in one command: it builds the program (front end →
type check → IR → code generation → `.lzo` → link → `.lzx`), loads the image from its
own bytes, boots the machine, and runs it under LazOS with the display, input and
terminal devices attached.

A windowed debugger needs a display and is built from the same workspace:

```bash
nix develop path:. -c cargo run -p lazalith-gui
```

---

## Repository Structure

```text
Cargo.toml                 the workspace: 24 members, the lint policy, resolver 3
flake.nix                  the Nix package, the checks, and the pinned toolchain
flake.lock                 pinned Nix inputs
instruction.md             the 100-step Phase I roadmap
crates/
  lazalith-types           shared types: widths, addresses, registers, configuration
  lazalith-diagnostics     the single diagnostic type and its rendering
  lazalith-isa             the instruction set: one metadata table, encode, decode
  lazalith-cpu             the Reference Interpreter, arithmetic, traps, state
  lazalith-memory          the address space, regions, faults, the decode cache
  lazalith-devices         the virtual display, input, and the device interface
  lazalith-machine         a Reference Interpreter made into a running machine
  lazalith-boot            the boot image: ROM, reset vector, kernel load
  lazalith-os              LazOS: processes, scheduler, syscalls, filesystem
  lazalith-os-abi          the syscall table, record layouts, and their validation
  lazalith-ir              the checked intermediate representation
  lazalith-compiler        the Lazen front end and its lowering to IR
  lazalith-c-compiler      the C front end
  lazalith-c-runtime       the C runtime library, in C
  lazalith-codegen         IR to object code
  lazalith-toolchain       assembler, disassembler, linker, object format
  lazalith-stdlib          the Lazen standard library, as Lazen source
  lazalith-ui              the Lazen widget library, as Lazen source
  lazalith-runtime         the startup sequence, the Lazen library, the runner
  lazalith-debug           the debug API: sessions, snapshots, deterministic replay
  lazalith-sdl3            the SDL3 boundary — the only `unsafe` in the workspace
  lazalith-gui             the SDL3 debugger frontend, built on the debug API
  lazalith-cli             `lazen`, the command-line toolchain
  lazalith-properties      the property-test generator, with no dependencies
  lazalith-fuzz            fuzz targets for everything that reads untrusted bytes
docs/                      39 documents; docs/platform.md is the overview
examples/hello             the first Lazen program
examples/window            graphics and input
```

`docs/platform.md` is the shortest tour of what is *verified* and by what.
`docs/project-state.md` is the running development log, and `docs/hardening.md` is the
audit record.

---

## Design Principles

These are enforced by the build graph or by the test suite, not merely stated.

- **Safe Rust by default, with one audited exception.** `unsafe_code = "forbid"` at the
  workspace level. `lazalith-sdl3` is the only crate that opts in, and an architecture
  test asserts that no other crate contains `unsafe`. The `unsafe` in it is auditable by
  reading one file, and that file has no Lazalith logic in it.
- **The Reference Interpreter is the semantic authority.** Every other execution path —
  the machine, the reference bus, the differential interpreter — is checked against it,
  including a whole register file and a whole program's output.
- **One definition per thing.** The ISA, the ABI, the syscall table and the
  diagnostics are each defined once; an architecture test asserts it, so a second copy
  cannot appear.
- **Validate before mutation.** A refused memory access changes nothing, and nothing
  partial is written on the way to the refusal.
- **The debug API does not expose the machine.** A frontend asks questions through
  `lazalith-debug`; it cannot reach CPU internals. `lazalith-gui` depends on nothing
  that holds machine state, which makes "never access CPU internals" a property of the
  dependency graph.
- **Explicit ABI.** Guest-visible records have fixed sizes and are laid out in the
  guest's own frame; out-parameters are addresses, because Lazen v1 has no scalar
  references and the round trip is checked at the boundary rather than trusted.
- **Deterministic replay.** A machine state, an image and an input log replay to the
  same result; virtual time advances by the step, not by the clock.
- **Differential, property and fuzz testing.** The C front end is checked against
  Lazen and against hand-computed values; the CPU against a model written from the ISA
  document; the filesystem against a model of its own documented semantics over 60,000
  randomised operations; and everything that reads bytes it did not write has a fuzz
  target.
- **No hidden global mutable state.** State lives in the machine, the kernel or a
  process, and the architecture tests check the dependencies that would make it possible
  to hide any.

---

## Hardening

The Phase I roadmap asked for a finished platform. It got one, and then an **adversarial
audit** asked what a green test suite does not know.

It started with 1,176 passing tests and found **twelve confirmed defects**:

```text
C front end   10   wrong-answer: unsigned int parsed signed; int→long sign-extended
                  from bit 63; bare `signed` became char; `long int` lost its width;
                  every `ul` constant panicked the compiler; a `ul` constant above
                  LONG_MAX silently became zero; the range check derived a constant's
                  type from a zero value; a 2-D array's initialiser was checked against
                  the wrong dimension; `switch` discarded the value it was switching on
                  and always ran the first case; pointer arithmetic was not scaled by
                  the pointee
runtime        1   a restored debugger machine read a dead process's frame through a
                  memory that no longer owned it — every frame read back as zeroes
debugger       1   after restoring a snapshot, `run` reported `Exit { code: 0 }` after
                  one instruction, having run nothing
```

Not one was found by reading code. Ten were found by asking the C front end to do
something and checking the number.

The phase also found **26 defective tests** against those twelve — and that ratio is its
real result. On this platform the implementation proved more reliable than the tests
describing it. Every one of the twenty-six was a test that would have passed, or failed,
for the wrong reason: a stale hand-written constant, a case whose values did not
distinguish the behaviours it compared, a baseline taken before a legitimate write, a
limitation record that checked the front end instead of the pipeline, and one assertion
that demanded a checksum the object format was never going to have.

One of the twelve is worth singling out, because it is not a missing test at all. It was
a defect **recorded in this repository, with a reproduction and a theory**, for a whole
cluster — and the theory was wrong. The full record, including the rules that came out
of it, is in [`docs/hardening.md`](docs/hardening.md).

**Hardening is not finished.** Eleven of eighteen clusters are audited; the rest remain.

---

## Limitations

Accurate as of the freeze, and not hidden.

**The ISA**

- **No conditional branch.** The machine has no `if`-style jump; conditional control flow
  is expressed by a compare and a branch-on-equal, or by arithmetic. See
  [`docs/isa.md`](docs/isa.md).
- Fixed 8-byte instructions, which is a deliberate simplicity trade.

**Lazen**

- **No `&mut [T]` to `&T` coercion**, so a standard-library function taking `&[u8]` is
  unreachable from a program holding a mutable array without building a read-only view
  from an address. A loud failure, not a wrong answer.
- **No generic subscripts, no references to array elements, no `unsafe`, no threads, no
  heap allocation in the language itself** — a guest's allocator comes from the C
  runtime's arena.
- One compilation unit per program: a `use` names a module declared in the same unit.
  See [`docs/lazen-modules.md`](docs/lazen-modules.md).

**C**

- No whole-struct assignment, no member access through a *dereference* (`(*p).field`),
  no call through a function pointer, and an element of a multi-dimensional array cannot
  be read. Each is refused loudly, with a message naming what is missing.
- The front end is substantial but not complete C; it is the language's own dialect, not
  a conforming implementation.

**LazOS**

- **A Rust kernel on the host, not a guest OS in C.** It runs beside the machine.
- **No disk, no network, no audio, no PCI.** The devices are virtual and in-process.
- **No guest-created threads.** The scheduler is single-machine and processes are
  pre-emptible, but a guest cannot spawn one.
- No `malloc` in the system ABI; memory comes from the C runtime's fixed arena.
- A user's files are a virtual filesystem with a package premise resolver, not a host
  filesystem.

**Performance**

- **Bounds-checked indexing is expensive.** An array element access costs roughly 150
  instructions and clearing a pixel about 600, which is why the example window is
  48×32. Making bulk memory a primitive is a prerequisite for anything larger.
  ([`docs/lazen-graphics.md`](docs/lazen-graphics.md))
- The decode cache is a real optimisation (measured at 1.67–2×) and is checked against a
  reference bus, but it is the only one. There is no JIT, no register allocator, no
  superword or vector work.

**Testing gaps**

- **No real display is tested.** Graphics are verified headless — a program draws, the
  host reads the pixels back out of guest memory — but no test puts a window on a
  physical screen. The SDL3 boundary has a C probe in the Nix build, not a display.
- The fuzz targets are run as bounded campaigns inside the suite rather than under a
  fuzzing driver with a corpus.

**The object format**

- **No checksum.** A corrupted data field yields a *valid* object rather than a rejected
  one. Not a current threat model, since objects are built and consumed locally by the
  toolchain, and deliberately not fixed: adding a digest is a design change.

---

## Roadmap

```text
Phase I ──────────── Steps 1–100. Complete, tested, documented.
                      Frozen as `lazalith-phase1-100`.
                      Overview: docs/platform.md
                      Log:       docs/project-state.md

Hardening ────────── An adversarial audit, in progress.
                      12 of 18 clusters audited.
                      12 confirmed defects fixed, 26 defective tests fixed.
                      Record:   docs/hardening.md
                      Next:     the remaining seven clusters, by the same method —
                               differential, property and adversarial tests, each
                               asking a question the existing suite does not.

Beyond Lazalith ──── Not started. No work of any kind in this repository.
                      Nothing in this document is a description of future features,
                      and nothing in the tree should be read as a promise of one.
```

---

## License

[MIT](LICENSE) — see [`LICENSE`](LICENSE).
