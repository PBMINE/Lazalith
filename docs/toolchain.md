# The Lazalith toolchain

`binstruction.md` §14, §15, §18, §19 and §22 ask for a GCC/binutils-shaped
toolchain: C and native assembly as first-class LZA target languages, a driver
that coordinates stages rather than reimplementing them, distinct user-facing
tools, and a target sysroot separating hosted from freestanding builds.

**Status: the separation is real for the three stages that are built; C is not.**
This document states what is real, names what is proposed, and says what has to be
true before the target-side LazOS work (B25, B26) can start.

---

## 1. What is real

**Fact.** Three front ends, one object format, one executable format, one linker,
one machine.

```text
C ──────┐
Lazen ──┼──→ Lazalith IR → LZA code ──→ .lzo ──→ lazld ──→ .lzx ──→ LazOS
ASM ────┘
```

| | |
| --- | --- |
| `.lzo` | object. `crates/lazalith-toolchain/src/object.rs` |
| `.lzx` | executable. `crates/lazalith-os/src/lzx.rs` |
| assembler | `crates/lazalith-toolchain/src/assembler.rs` |
| linker | `crates/lazalith-toolchain/src/linker.rs` |
| disassembler | `crates/lazalith-toolchain/src/disassembler.rs` |
| packages (`.lza`) | `crates/lazalith-toolchain/src/manifest.rs` |
| IR | `crates/lazalith-ir` |
| LZA back end | `crates/lazalith-codegen` |

**The convergence is tested, not asserted.**
`crates/lazalith-c-compiler/tests/` contains a cross-frontend differential that
builds the same program through C and through Lazen and compares what each prints.
`the_syscall_table_is_defined_once` and `the_isa_is_defined_once` check that the
object format, the executable format and the ISA each have exactly one definition.

**The toolchain does not depend on the VM.** Checked by
`the_compiler_does_not_depend_on_the_emulator_implementation`. This is what makes a
compiler usable without a machine, and `binstruction.md` §37 requires it.

**There is one binary.** `lazen`, with eight subcommands: `new`, `check`, `build`,
`run`, `test`, `pack`, `deps`, `fmt`. The flake's install phase discovers installed
binaries rather than listing them, and asserts that `lazen` is among them.

---

## 2. The tool split

`binstruction.md` §18 proposes `lazcc`, `lazas`, `lazen`, `lazld`, `lazdbg`,
`lazpkg`, `lazimg`, `lazalith`, and says to inspect the actual existing binaries
before freezing the names. That inspection has been done, and three of the names
are now programs:

| Proposed | Is | Program | Stage |
| --- | --- | --- | --- |
| `lazcc` | ✅ built | `crates/lazalith-driver/src/bin/lazcc.rs` | frontend + backend → `.lzo` |
| `lazas` | ✅ built | `crates/lazalith-driver/src/bin/lazas.rs` | assembler → `.lzo` |
| `lazen` | ✅ built | `crates/lazalith-cli` | the driver, and the VM |
| `lazld` | ✅ built | `crates/lazalith-driver/src/bin/lazld.rs` | link → `.lzx` |
| `lazdbg` | ❌ nothing | — | `lazalith-debug` is an API with no program |
| `lazpkg` | part of `lazen` | — | `lazen pack`, `lazen deps` |
| `lazimg` | ❌ nothing | — | — |
| `lazalith` | part of `lazen` | — | `lazen run` |

### The four programs, and what each one refuses to do

```console
$ lazcc hello.lz -o hello.lzo      # compile: stops at the object
$ lazas tiny.la -o tiny.lzo        # assemble: needs no compiler at all
$ lazld hello.lzo -o hello.lzx     # link: adds the entry sequence
$ lazen build hello.lz             # the driver, doing all three
```

- **`lazcc` does not link.** A compiler that also links cannot hand its output to a
  *different* linker, which is the whole of §18's separation.
- **`lazas` needs nothing.** No compiler, no prelude, no entry sequence, no linker. A
  caller with a `.la` file has a `.lzo` and nothing else to know.
- **`lazld` adds the entry sequence**, because the link is the one place that decides
  where an image starts. `--entry` names it; the default is the Lazen entry `fn.main`.
- **`lazen build` calls the same three functions**, via `lazalith_driver::build_lazen`.
  It is not a second implementation — that is what makes the two agree byte-for-byte
  (below), and what would stop them drifting apart.

### The driver coordinates; it does not reimplement

§18: "The compiler driver coordinates these stages instead of implementing a parallel
linker or object system." Every stage function in `crates/lazalith-driver` calls the
crate that already owns the work: `lazalith-compiler` for the frontend,
`lazalith-codegen` for the backend, `lazalith-toolchain` for the object and the link.
There is no second assembler and no second linker anywhere in the crate.

**The separation is tested by equality, not by inspection.** `lazcc hello.lz` followed
by `lazld hello.lzo` writes the **same bytes** as `lazen build hello.lz` — checked in
`crates/lazalith-driver/tests/stages.rs` and again in `nix flake check` and in CI, by
`cmp`. Two tools agreeing *by construction* is the separation working; two tools
agreeing *by testing* would be a duplicate implementation waiting to drift.

**One name is still not backed by anything: `lazdbg`.** `lazalith-debug` is a complete
debug *API* — a controller, a session, snapshots and deterministic replay, with no
`&mut LazalithMachine` anywhere in its public surface — and there is no command-line
program that drives it. `lazalith-gui` has a window and `lazalith-fuzz` fuzzes its
control layer, but no binary. So the debug API exists and its frontends are two
demonstrations rather than a product.

**Why these three and not eight.** §18 says to move *toward* distinct tools, and the
names are free because the inspection found one binary with eight subcommands. The
split is drawn at the three places where a file format already separates the work —
`.lzo` and `.lzx` are on disk, and a stage boundary at a file is a boundary a user can
see and a script can use. Splitting `lazpkg` and `lazimg` out would be splitting
command-line organization, which §18 does not ask for.

---

## 3. Native LZA assembly

**Fact.** `.lzs` assembly is a first-class front end. It assembles to `.lzo`, it
links, it runs, and it can reach supervisor-only instructions — `HALT`, `RFE`, `EI`,
`DI`, `CSRR`, `CSRW` are all `supervisor_only` in the ISA table and all assemble
normally. `assembly_can_still_reach_low_level_functionality` builds a `.lzs` that
uses one and checks the machine refuses it at User privilege and accepts it at
Supervisor. That is the correct arrangement: the assembler does not police
privilege, the machine does.

**The dialect** is LZA, not a copy of x86 or ARM syntax, and it is assembled
against the one authoritative instruction table, which the encoder, the decoder,
the disassembler and the assembler all read.

**Requirement** for the target-side kernel (§23): C objects and LZA assembly
objects must link into one image, one ABI, one object format, one linker. Nothing
in the current design prevents that — both produce `.lzo` and the linker does not
care which — and the test that would demonstrate it does not exist yet.

---

## 4. C as a first-class LZA target

**Fact.** `lazalith-c-compiler` is a real C front end: lexer, parser, semantic
analysis, lowering to the shared `lazalith-ir`, and then the shared `lazalith-codegen`
back end. `lazalith-c-runtime` is a minimal C runtime that resolves through LazOS
syscalls. **A C program compiles, links and runs** — see §4 and
`crates/lazalith-driver/tests/stages.rs`.

**Fact.** The hardening phase found and fixed twelve real defects in this front end,
several of them wrong-answer rather than trap defects. It is the most recently
exercised part of the toolchain, which is a reason for confidence and not a reason
to stop treating it as the newest code.

**The pipeline, as it is** (`binstruction.md` §14, satisfied and running):

```text
C source → C frontend → semantic analysis → Lazalith IR → LZA backend → .lzo → lazld → .lzx
```

**No C-specific executable format exists**, and `binstruction.md` §14 forbids one.

**No C-specific executable format exists**, and `binstruction.md` §14 forbids one.

**B14: `lazcc foo.c` produces a `.lzo`, and it is a real one.** The C path is the C
frontend, then the *same* `lazalith_codegen::generate` that Lazen uses — the backend
takes no argument about which language it was given. What makes the object a C object
is the frontend in front of it, not a branch anywhere downstream.

**This stage's first draft refused C, and was wrong.** It reported a missing
"C-to-object backend" — a claim about the codebase that nobody had checked, written
in the same register as six refusals that *were* accurate. Checking took a minute:
`lazalith_c_compiler::ir::lower` already emitted a `lazalith_ir::Module` and a
`Vec<lazalith_ir::FrameLayout>`, the same two types the Lazen lowering emitted, and
`generate` already took exactly those. The backend was never missing; about twenty
lines of calling it were.

A refusal is not self-justifying. The discipline that makes one trustworthy — believe
the code, not the plan — is the same discipline that eventually finds it unnecessary.

Two things had to be fixed for C to actually work end to end, and both were found by
running the output rather than by reading it:

1. **A C object and a Lazen object could not be linked together.** Both declare the
   same OS ABI, and an ABI declaration was lowered as an `External` global — a
   *guaranteed-unique* symbol — so the linker refused the pair as a duplicate
   `fn.syscall.write`. Neither was wrong about the ABI; both were claiming to own a
   symbol neither one owns. A syscall declaration is now `Local`, and
   `lazalith_ir::Function::is_declaration` answers the backend's real question ("does
   this have a body?") structurally rather than by reading linkage, because a C ABI
   declaration is `Local` and a Lazen one is `External` and both are declarations.
2. **The image started at the wrong symbol.** The driver's `link` passed the
   *program's* entry as the *image's* entry symbol, so the machine began at a
   function with no OS-established stack and no exit, and every program trapped. The
   startup sequence is now `lazalith_runtime::STARTUP_LABEL` and the program's entry
   is what it *calls*.

**Tested by running, not by comparing.** `a_c_program_compiles_links_and_runs` builds
a C program, links it and executes the image. The byte-for-byte test could not have
found defect 2 — every image the bug produced was internally consistent — which is
why a stage that produces executable artifacts is checked by executing them.

**Not implemented:** a C *preprocessor*. There is no `#include`, no `#define`, no
macro expansion. Anything C-shaped that needs one today is a limitation and is
recorded in `docs/c-compiler.md`; the Linux 0.01 port (§21, §48) will need this
before it can build `kernel/sched.c`, which uses macros heavily.


---

## 5. Hosted and freestanding

`binstruction.md` §22 requires the distinction:

```text
hosted target C        an ordinary program, with a runtime and syscalls
freestanding LZA C     a kernel, with no hosted runtime at all
```

**Fact.** There is no sysroot and no freestanding mode. `binstruction.md` §19 asks
for

```text
sysroot/
  include/
  lib/
  crt/
  runtime/
```

and none of those directories exist. The Lazen standard library and the C runtime
prelude are composed *in memory* by `lazalith-runtime`, which appends the prelude
and the library text to whatever it is compiling and then assembles the result.
There is no on-disk tree, and a program that needs a header finds it because the
compiler was handed it, not because it looked.

**This is B15, and it is on the critical path to B25/B26.** A freestanding kernel
build needs:

| | why |
| --- | --- |
| `sysroot/include` | kernel headers, device headers, ABI definitions |
| `sysroot/crt` | the startup object, which is *not* the hosted one |
| `sysroot/lib` | the target libraries, which are `.lzo`, not host `.rlib`s |
| a freestanding flag | it must be possible to compile a translation unit that links nothing and calls nothing |
| a link layout | the kernel's own memory layout, which `KernelMemory::regions` already describes |

**Two of those five are already implied by existing code and neither has been
extracted:** `lazalith-os-abi` holds the syscall numbers, the records and the
capabilities, and `KernelMemory::regions` holds the kernel's memory layout. Both
are currently reachable only from inside the workspace. Making them a *sysroot* is
mostly a matter of putting them on disk in a layout a driver can point at, and of
making the compiler read that layout instead of composing things in memory.

---

## 6. Object and executable compatibility

**Fact.** The formats are versioned, and the versions are single definitions:

| | |
| --- | --- |
| `.lzo` | `ObjectFile` — architecture, ISA version, ABI version, sections, symbols, relocations, entry point, debug metadata |
| `.lzx` | `LzxImage` — `LZX_FORMAT_VERSION`, `LZX_ISA_VERSION`, `LZX_ABI_VERSION` |
| `.lza` | `LZA_FORMAT_VERSION` — executable plus manifest plus resources |
| boot image | `BOOT_FORMAT_VERSION`, `BootArchitecture` (`Lz32 = 1`, `Lz64 = 2`) |

`the_abi_is_defined_once` checks that exactly one crate in the workspace holds the
ABI version, and that the compiler depends on it rather than restating it.

**The linker's rejections are the compatibility contract:** it refuses
incompatible LZ32/LZ64, ISA versions and ABI versions rather than producing an
image that might work. `crates/lazalith-toolchain/tests/hardening_object_format.rs`
checks that every shape round-trips, that truncation is refused at every length,
and that a corrupted byte never panics or over-reads.

---

## 7. What is not here

- **No sysroot, no freestanding mode, no startup objects on disk.** `CBuildOptions::freestanding`
  exists and takes an empty runtime, but there is no sysroot directory layout for it
  to mean anything against yet (§19, B15).
- **No C preprocessor.**
- **No `lazdbg`.** The debug *API* is complete; no program drives it. `lazpkg` and
  `lazimg` likewise stay as `lazen` subcommands — §18 asks to move toward distinct
  tools, and the split was drawn at the two file formats rather than at
  command-line organization.
- **No `--target` that means anything yet.** `lazcc --target lz32` parses and is
  rejected downstream by the one backend, which is 64-bit only; an unknown machine is
  refused rather than defaulted.
- **No command-line debugger**, though the debug API it would use is complete.
- **No target triple.** `lza64-unknown-lazos` is a proposal; see `docs/lza64.md`.
- **No LLVM backend.** The optional LLVM path of Phase-I step 94 exists as a
  documented option; the native back end is the one everything uses, which is what
  `binstruction.md` §18 requires.

---

## Related

- `docs/architecture.md` — the toolchain's place in the platform
- `docs/lza64.md` — the architecture name and the future target triple
- `docs/compatibility.md` — the compatibility tier
- `docs/linux-0.01-port.md` — what the port needs from this toolchain
- `docs/c-compiler.md`, `docs/frontend.md`, `docs/lzo.md`, `docs/lzx.md` — the
  current implementation in detail
