# Lazen Applications

This document is Step 56 of the roadmap. It defines what makes a Lazen
application different from a raw executable, and fixes the manifest format.

## Executable versus application

A raw executable is a `.lzx` file: sections, an entry point, and an ISA/ABI
version. It says nothing about who produced it or what it needs.

A **Lazen application** is a package: a manifest plus sources, which the
compiler turns into a raw executable. The application is the unit a person
authors, shares, and installs; the executable is the unit LazOS loads.

```text
application (lazen.toml + src/)   -- what a person ships
    ↓ lazen build
executable (.lzx)                 -- what LazOS loads
```

## Manifest

```toml
[application]
name = "hello"
version = "0.1.0"
entry = "src/main.lz"
architecture = "any"
```

### Fields

| Field | Required | Meaning |
| --- | --- | --- |
| `name` | yes | package name; lowercase, digits and `-` |
| `version` | yes | semantic version, `major.minor.patch` |
| `entry` | yes | path to the crate's root file, relative to the manifest |
| `architecture` | yes | `any`, `lz32`, or `lz64` |

### Optional sections

```toml
[resources]
logo = "assets/logo.rgb"

[permissions]
console = true
filesystem = true
graphics = false
input = false

[dependencies]
std = "0.1"
```

- `resources` names data files to embed into the executable's read-only data
  section. The compiler records each as a constant byte array.
- `permissions` declares the OS capabilities the application intends to use.
  The declarations are *checked against the source*: an application that
  declares `graphics = false` and calls the display ABI is a compile error. A
  declaration that is never used is allowed, because a future LazOS may refuse
  to start a program that asks for a capability it does not need.
- `dependencies` names other packages, by name and version requirement. v1
  resolves `std` and application packages; there is no registry.

## Architecture selection

`architecture = "any"` produces a program that builds for the target word size
the compiler is invoked with, which is how one source tree yields both LZ32 and
LZ64 executables. `architecture = "lz32"` or `"lz64"` pins the target and makes
a mismatched build an error rather than a silent difference.

## Entry point

The entry file must define:

```lazen
pub fn main() -> i32 { ... }
```

`main` returns the process exit code. The runtime calls it and passes the result
to the `Exit` syscall, so the application's exit status is the value `main`
returns. There is no implicit global initialization, so there is nothing else
that could run first.

## Versioning rules

The manifest version is the *application* version. It is not the ISA version, the
ABI version, or the object format version: those are properties of the toolchain
and are recorded in the executable itself. The compiler refuses to build if the
ISA or ABI version it emits does not match what the loader it targets accepts,
rather than deferring the failure to load time.

## The build product

`lazen build` writes exactly one file, the `.lzx` executable. It does not write
intermediate object files next to the sources; objects are held in memory during
the build and discarded. There is no separate runtime library file: the runtime
is linked into the executable, because a Lazalith process image is a single
loadable unit with no dynamic loader.

## What is deliberately not in v1

- No dynamic linking, plugins, or shared objects.
- No install targets, package signing, or a registry.
- No build profiles, optimization levels, or per-package compiler flags. A Lazen
  program is compiled the same way every time.
- No asset pipelines. Resources are raw bytes; the compiler does not decode
  images or fonts.
- No application-level "main arguments" beyond what the OS passes. LazOS does
  not pass arguments to a process in v1, so `main` takes none.

The last item is stated explicitly because it will change: when `SpawnProcess`
is served, `main`'s signature will gain arguments, and this document will be
updated in the same commit that implements it.
