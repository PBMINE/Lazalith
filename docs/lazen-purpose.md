# Lazen's Purpose

Lazen is the native application language of Lazalith. This document fixes what
Lazen is *for*, so that later design decisions can be judged against a single
statement of intent.

## What Lazen must make easy

Lazen is the easiest way to create programs that run on Lazalith:

```text
LazOS applications
GUI applications
games
utilities
terminal tools
interactive programs
```

"Easiest" means a person who knows Lazalith and ordinary programming can write a
complete program without reading the machine specification, without learning a
manual, and without touching hardware registers. Concretely, Lazen must make
these easy:

- obtaining input and producing output through the OS ABI;
- owning and passing data with predictable cost;
- handling failure without a runtime panic;
- splitting a program into modules and reusing them;
- producing a native Lazalith executable that the real loader accepts;
- writing graphical and interactive programs through a high-level SDK;
- debugging, because the debugger is part of the platform, not an afterthought.

## What Lazen must not become

Lazen is not a general-purpose systems language competing with C, and it is not
a research language. It is an application language for one platform.

The following are explicitly out of scope for Lazen v1:

- concurrency beyond what LazOS schedules (processes, not threads);
- a garbage collector or reference counting (see `docs/lazen-memory-model.md`);
- unchecked arithmetic overflow being silently permitted;
- reimplementing the OS ABI, the assembler, or the linker;
- any construct that requires the programmer to know MMIO, framebuffer
  addresses, device registers, or SDL3.

## Where C and assembly still belong

C remains useful, and Lazen must not pretend otherwise:

```text
systems programming
porting
interoperability
low-level runtime work
```

Assembly remains the lowest-level interface. It stays the escape hatch for
anything the compiler cannot yet express, and the assembler and linker remain
first-class components of the toolchain.

This creates a deliberate division of labour:

| Concern | Owner |
| --- | --- |
| Applications, tools, games, GUIs | Lazen |
| OS services and drivers | LazOS, written in Rust and assembly |
| Machine-level code, new instructions | Assembly |
| Porting existing C code | C |

Lazen's success criterion is that a program that *could* be written in C is
more pleasant in Lazen, while a program that *must* be written in C remains
possible.

## Relationship to the rest of Lazalith

```text
Lazen source
    ↓ Lazen compiler
Lazalith IR
    ↓ Lazalith code generator
Lazalith object (.lzo)
    ↓ linker
Lazalith executable (.lzx)
    ↓ LazOS loader
process
    ↓ scheduler, Reference Interpreter, machine
hardware
```

Every arrow is a real component of this repository. Lazen adds no new
executable format and no new system call transport: it lowers into the same IR
that a C frontend will use, and it links through the same `.lzo` and `.lzx`
formats that assembly programs already use.

## Consequence for the language design

The purpose statement constrains the later documents:

- the memory model must have predictable cost and a debuggable failure mode, so
  it is neither garbage collected nor silently unsafe (`lazen-memory-model.md`);
- the type system must be small enough to check exhaustively, and must make
  failure explicit rather than exceptional (`lazen-types.md`);
- the SDK must hide MMIO and device registers completely (`lazen-sdk.md`,
  `lazen-graphics.md`, `lazen-input.md`);
- the compiler must produce diagnostics a person can act on, because the
  platform ships its own debugger.
