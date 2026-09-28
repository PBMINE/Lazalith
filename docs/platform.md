# The finished platform

Step 100 asks for a project that provides this:

```text
                   LAZALITH
                       │
      ┌────────────────┼────────────────┐
      │                │                │
     CPU             Tools            GUI
      │                │                │
      ▼                ▼                ▼
   Machine      Assembler/Linker      SDL3
      │
      ▼
  Lazalith HW
      │
      ▼
    LazOS
      │
      ▼
  System ABI
      │
      ▼
   ┌───────┐
   │ Lazen │
   └───┬───┘
       │
       ▼
Applications
```

and three programming layers, and one target pipeline:

```text
Lazen ─┐
C ─────┼──→ Lazalith Object ──→ Linker ──→ Lazalith Executable ──→ LazOS ──→ Machine
Asm ───┘
```

This document says what is *verified*, by what, and what is not. The step's own
instruction at the end of the roadmap is the governing idea: treat the roadmap as a state
machine, and a step is finished when it is implemented, tested, fixed, and documented — not
when the diagram can be drawn.

## The one claim the diagram makes, tested

`crates/lazalith-c-compiler/tests/platform.rs` is the only place where all three languages
go through one path and are compared. The individual languages were already tested —
`lazalith-runtime`'s `pipeline.rs` for Lazen, `lazalith-c-compiler`'s `end_to_end.rs` for
C, the toolchain's assembler tests for assembly — but each in its own test file, in its own
crate, against its own copy of the boot sequence.

The claim is narrow and mechanical: **the same program, in three languages, produces the
same arithmetic, the same exit status, and the same bytes through one object format, one
linker, one loader, and one machine.** The program is a sum of one through ten, computed
rather than stated, so the number on the screen and the status the process exits with are
both the program's own arithmetic.

| language | what it goes through | test |
| --- | --- | --- |
| Lazen | frontend → IR → object → link → `.lzx` → kernel → machine | `a_lazen_program_sums_and_reports_through_the_shared_path` |
| C | the C frontend → the *same* IR → object → link → `.lzx` → kernel → machine | `a_c_program_sums_and_reports_through_the_same_path` |
| assembly | assembler → object → link → `.lzx` → kernel → machine | `an_assembly_program_sums_through_the_same_path` |
| all three, compared | — | `all_three_languages_agree_on_the_same_program` |
| all three, one format | the same ISA version, ABI version and section layout | `all_three_produce_a_lazalith_executable_rather_than_their_own` |

The three paths are written out separately in that file, deliberately, with a comment
saying why: the claim is that the paths *coincide*, and a test that shared the code
between them would be asserting that they were the same code.

## What the assembly case found

The assembly program is **unrolled**, and the reason is the ISA rather than the test: the
instruction set has `BR`, `JMP` and `CALL`, and **no conditional branch**. A hand-written
loop therefore cannot be written without a branch the architecture does not have.

That is worth stating plainly rather than working around quietly, because it is the
sharpest real difference between the three layers on this platform. The compilers have a
conditional branch to give a program, so `while` in Lazen and in C costs a programmer
nothing. At the assembly layer the same loop is not expressible, and the cost lands on
whoever is writing. It is a gap in the ISA, not a gap in the assembler — the assembler
faithfully emits everything the ISA defines — and the next ISA revision is where it would
be closed.

The entry sequence also taught the file the ABI's return convention by being wrong first:
the startup does `CALL <entry>; MOV r1, r0`, so a program's result is returned in **r0**.
The first version of the assembly program put the sum in `r3` and exited 0, which is a
better demonstration of the convention than a comment would have been.

## The programming layers, and where each is documented

| layer | for | document |
| --- | --- | --- |
| **Lazen** | native applications — the platform's own language | `docs/lazen-syntax.md`, `docs/lazen-types.md`, `docs/lazen-memory-model.md` |
| **C** | systems and interoperability | `docs/c-compiler.md`, `docs/c-runtime.md`, `docs/convergence.md` |
| **assembly** | low-level machine control | `docs/isa.md`, `docs/lzx.md` |

Lazen is the language the platform is for, and it is the one with a standard library, a
formatter, a package format, a capability system, and a debugger that maps its instructions
back to its lines. C is for code that already exists in another language or that has to
interoperate, and `docs/convergence.md` records what it shares with Lazen and where it
stops. Assembly is for the parts of the machine the other two will not let you reach, and
step 99 checks that it still can.

## The platform layers, and what enforces each boundary

Step 99 turned the thirteen architectural rules into thirteen checks, so this table is not
a diagram with a hopeful caption — each row names the test that would fail.

| layer | crate | boundary it may not cross | enforced by |
| --- | --- | --- | --- |
| CPU | `lazalith-cpu`, `lazalith-isa` | knows nothing about a window, a compiler, or a machine | `the_cpu_does_not_depend_on_sdl3`, `the_machine_does_not_depend_on_the_compiler` |
| Machine | `lazalith-machine`, `lazalith-memory` | knows nothing about a compiler's *output shape* | `the_compiler_does_not_depend_on_the_emulator_implementation` |
| Tools | `lazalith-toolchain`, `lazalith-codegen` | does not run what it built, from its library | the same, reading `[dependencies]` and not `[dev-dependencies]` |
| GUI | `lazalith-gui`, `lazalith-sdl3` | does not take the CPU apart; SDL3 does not leak below it | `the_gui_does_not_reach_into_the_cpu` |
| OS | `lazalith-os`, `lazalith-os-abi` | one syscall table, one ABI version, one ISA | `the_syscall_table_is_defined_once`, `lazen_goes_through_lazos_rather_than_around_it` |
| Lazen | `lazalith-compiler`, `lazalith-runtime` | cannot name a syscall the ABI has not numbered | the same, and `c_goes_through_the_abi_rather_than_around_it` |
| diagnostics | `lazalith-diagnostics` | one vocabulary, and two kinds of failure | `diagnostics_are_centralised`, `guest_faults_and_emulator_bugs_are_distinguishable` |

## What the platform does today

Everything below is exercised by a test, and the test count is the only claim made about
scale: **1176 tests**, all passing, with `cargo fmt --all --check`, strict Clippy over every
target, `nix flake check` and `nix build` green.

- **Build and run Lazen**, C, and assembly programs to one executable format, and run them
  on one machine (`platform.rs`, `pipeline.rs`, `end_to_end.rs`).
- **A working operating system**: a loader, processes, a scheduler, a filesystem, a
  terminal, a display driver, an input driver, and a timer (steps 70, 91 and the OS
  tests).
- **A capability system**: a program from a package may only make the syscalls its package
  declared, and a bare executable may make all of them — with the asymmetry deliberate and
  documented (`docs/os-expansion.md`).
- **A debugger** that recovers a source file, line, column, instruction, guest program
  counter, registers and stack from a runtime fault, and that keeps a guest fault and an
  emulator bug apart (`docs/graphics-test.md`'s neighbour, step 98).
- **Graphics**: a Lazen program opens a window, draws, and answers the keyboard, through
  the SDK and the OS, with the pixels in the guest's own memory (`docs/graphics-test.md`).
- **Deterministic emulation**: a decode cache that is indistinguishable from no cache, a
  differential harness against a bare reference interpreter, a replay session, and a fuzz
  target set (`docs/optimization.md`, `docs/fuzzing.md`).
- **A reproducible build**: one flake, five checks, and a dev shell that is itself built as
  a check (`docs/nix.md`).

## What the platform does not do, and where that is written down

A finished project's honest boundary is part of finishing it. These are not oversights;
each has a document that says what it would take.

| not done | why | where |
| --- | --- | --- |
| generics, a heap, a growing collection | the Lazen ABI has no allocation call, so a container has nowhere to grow to | `docs/lazen-expansion.md` |
| threads | step 91 built per-thread state and deliberately not creation | `docs/os-expansion.md` |
| networking, audio | no device, so no ABI; a socket that always failed is not a stack | `docs/os-expansion.md` |
| an LLVM backend | would be a second implementation of the semantics, and the project's premise is that it has no dependencies | `docs/llvm-backend.md` |
| a JIT | step 93 measured where the time goes, and it is not code quality | `docs/optimization.md` |
| the display's frame address | step 97 found it is not the program's framebuffer address, and recorded the evidence rather than working around it | `docs/graphics-test.md` |
| a window on a real display | SDL3 is not linked in tests, because every machine they run on is headless | `docs/graphics-test.md` |

## Where to start reading

If you want to *run* something: `examples/hello/main.lz` and `examples/window/main.lz`,
built and run with the `lazen` command, or the two `nix flake check` checks that run the
installed binary on them.

If you want to *know* something: `docs/isa.md` for the instruction set, `docs/lzx.md` for
the executable format, `docs/os-abi.md` for the system call boundary, and
`docs/lazen-syntax.md` for the language. `docs/project-state.md` is the running record, one
entry per step, in the order the steps happened.
