# The Lazalith toolchain

`binstruction.md` §14, §15, §18, §19 and §22 ask for a GCC/binutils-shaped
toolchain: C and native assembly as first-class LZA target languages, a driver
that coordinates stages rather than reimplementing them, distinct user-facing
tools, and a target sysroot separating hosted from freestanding builds.

**Status: the shape exists; the separation does not.** This document states what is
real, names what is proposed, and says what has to be true before the target-side
LazOS work (B25, B26) can start.

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

## 2. The proposed tool split

`binstruction.md` §18 proposes `lazcc`, `lazas`, `lazen`, `lazld`, `lazdbg`,
`lazpkg`, `lazimg`, `lazalith`, and says to inspect the actual existing binaries
before freezing the names. That inspection has been done:

| Proposed | Would be | Today |
| --- | --- | --- |
| `lazcc` | the C compiler driver | part of `lazen build` |
| `lazas` | the assembler | `lazen build` on a `.lzs` |
| `lazen` | the Lazen compiler | `lazen build` |
| `lazld` | the linker | internal to `lazen build` |
| `lazdbg` | the debugger | **does not exist as a program at all** |
| `lazpkg` | package management | `lazen pack` and `lazen deps` |
| `lazimg` | image building | does not exist |
| `lazalith` | the VM | `lazen run` |

**One name in that table is not yet backed by anything: `lazdbg`.**
`lazalith-debug` is a complete debug *API* — a controller, a session, snapshots and
deterministic replay, with no `&mut LazalithMachine` anywhere in its public
surface — and there is no command-line program that drives it. `lazalith-gui` has
a window (`crates/lazalith-gui/src/window.rs`) and `crates/lazalith-fuzz` fuzzes its
control layer, but no binary. So the debug API exists and its frontends are two
demonstrations rather than a product.

**Research — what GCC's separation actually buys.** GCC's documented stages are
preprocess → compile → assemble → link, and the *driver* coordinates them rather
than each stage being a separate program the user has to sequence. The
corresponding separation for Lazalith is therefore not "eight binaries" but:

1. **distinct, separately invokable stages** — assembler, compiler, linker, which
   is what `.lzo` and `.lzx` already are;
2. **a driver that coordinates them** — which is what `lazen build` is today;
3. **names a user can type** — which is a usability question, and §18's list is a
   good starting point.

**Recommendation, and the reason it is a recommendation rather than a decision:**
splitting the binary is B14 and should be decided when there is a second thing that
needs to call the linker without the driver. Today `lazen build` is the only
caller, so splitting it would add process boundaries with no consumer.

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
syscalls.

**Fact.** The hardening phase found and fixed twelve real defects in this front end,
several of them wrong-answer rather than trap defects. It is the most recently
exercised part of the toolchain, which is a reason for confidence and not a reason
to stop treating it as the newest code.

**The pipeline, as it is** (`binstruction.md` §14, already satisfied in shape):

```text
C source → C frontend → semantic analysis → Lazalith IR → LZA backend → .lzo → lazld → .lzx
```

**No C-specific executable format exists**, and `binstruction.md` §14 forbids one.

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

- **No sysroot, no freestanding mode, no startup objects on disk.**
- **No C preprocessor.**
- **No separate tool binaries.** One, `lazen`.
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
