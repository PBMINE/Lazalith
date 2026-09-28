# Nix integration

Step 95 asks for three commands and a coverage list:

```text
nix develop        nix build        nix flake check

Rust · emulator · OS · assembler · linker · Lazen · C compiler · SDL3 frontend · tests
```

All three commands work. What this step changed is that the coverage list is now
*checkable*, and that `nix build` produces something anybody can run.

## What each check is, and which area it covers

| check | covers | what it does |
| --- | --- | --- |
| `workspace` | Rust, emulator, OS, assembler, linker, Lazen, C compiler, SDL3 frontend, tests | builds every crate and runs the whole test suite; the SDL3 crate's C probe compiles as part of that build |
| `formatting` | style | `cargo fmt --all --check` in a clean sandbox, not in the developer's checkout |
| `clippy` | style | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `program` | Lazen, and the artifact itself | runs the installed `lazen` on two example programs |
| `devShell` | the development environment | builds the dev shell |

Two of those are new.

### `program`: the system being used, not merely built

Everything else in the flake *builds* the system. This check runs it. It formats
both examples, type-checks both, reports the version, and then asserts that the
binary it tested is the one this build produced rather than one that happened to be
on `PATH` — because a check that silently tests some other build's `lazen` is a
check that has stopped checking.

`lazen check` on a real example is worth having as an integration test: it runs
the lexer, the parser, the resolver, and the type checker over a program that uses
the standard library, in a file the repository ships, and it is the only check that
uses the *installed* artifact rather than the source tree.

### `devShell`: `nix develop` is verified by building it

`nix develop` is on the list of things to verify, and a dev shell that has quietly
stopped building is the reproducibility failure nobody notices until somebody new
clones the repository. Building it as a check is the only way it gets noticed on
the day it breaks. It evaluates on both `x86_64-linux` and `aarch64-linux`.

## `nix build` now produces a program

This was the step's biggest actual defect. The install phase named **eleven library
crates and no executables**, so `nix build` produced a package containing rlibs that
nothing outside a cargo workspace could use, and no program at all. A package with
no program is a way of checking that a build succeeds, which `nix flake check` and
`cargo build` between them already did.

The install phase now discovers what to install rather than listing it:

- every `lib*.rlib` the build produced — 24 of them, not 11;
- every executable the build produced, `lazen` among them;
- and it then **asserts that `lazen` exists**, so a build that stopped producing the
  command fails here with a sentence saying so, instead of succeeding and shipping
  nothing.

Discovering rather than listing is the same reasoning as the documentation filter
below: a list of what to install is a list that is wrong the next time a crate is
added, and it is wrong *quietly*.

## The documentation list can no longer be one short

The source fileset listed thirty-odd `docs/*.md` paths by hand. That is a list
waiting to be one document short, and it fails *silently* — a document missing from
the source tarball is not a build error, it is a document missing from a release.
Every document that has been added since (the formatter's, the package format's,
step 91's, step 92's, this step's predecessors) was a chance to get it wrong.

It is now a filter over the directory:

```nix
docs = nixpkgs.lib.fileset.fromSource (nixpkgs.lib.cleanSourceWith {
  src = ./docs;
  filter = path: _type: nixpkgs.lib.hasSuffix ".md" path;
});
```

All 34 documents are found structurally. Adding a document now cannot be forgotten,
because there is nothing to add it to.

## Three bugs found while making the checks real

Worth recording, because all three failed the *build* rather than a test, and each
is a mistake the next person would otherwise make:

1. **`[ -x "$program" ]` is true for a directory.** A directory is searchable, so
   the first version of the install loop tried to install `release/build` and failed
   the whole build with coreutils' `install: omitting directory` — a confusing way to
   learn that a shell test needs `-f`. The loop now requires a regular file.
2. **`"$out/bin/lazen"` inside `passthru` is the literal string `$out/bin/lazen`.**
   Nix does not substitute `$out` outside a derivation's own attribute set, so the
   check that used it was handed a path that did not exist, and reported
   `lazen: command not found` for a binary that was sitting in the store.
3. **Escaping `${...}` as `''${...}` in a `runCommand` script passes it to the
   shell.** The shell then tries to expand a *Nix* attribute path as a *shell*
   variable, and fails with `bad substitution`. The right move for a store path is to
   let Nix interpolate it.

None of the three would have been caught by `nix build` alone, because the first
only appears with the new install phase, and the second and third only inside the
new check. Adding a check that cannot pass is the only way to find out whether it
can.

## What is verified, and how

```
nix develop                # rustc, cargo, rustfmt, clippy, rust-analyzer,
                           # pkg-config, cmake, ninja, gcc, gdb, sdl3
nix build                  # the package, including $out/bin/lazen
nix flake check            # workspace, formatting, clippy, program, devShell
```

- `nix flake check` reports **all checks passed** on `x86_64-linux`, and evaluates
  the dev shell for `aarch64-linux` as well. It prints a warning that
  `aarch64-linux` is an incompatible system on this machine and is therefore
  omitted; that warning is honest and is left in place rather than silenced by
  narrowing the system list to the one that happens to work here.
- The SDL3 frontend is covered in the *build* environment, not only the dev shell,
  because a package that builds in `nix develop` and not in `nix build` is broken
  for everyone who installs it. `PKG_CONFIG_PATH` is set explicitly rather than left
  to the pkg-config setup hook, which would usually do it and, when it does not,
  produces the confusing failure of pkg-config finding no `sdl3.pc` in a build that
  looks correctly configured.
- The test suite that `workspace` runs is the whole suite, and its expected values
  were written down before any of the optimization work existed.

## The honest limit

This step verifies that the *system* is reproducible. It does not verify that a
*program* is, which is step 96's job: the pipeline from a Lazen source file through
the compiler, the object, the linker, the image, the kernel, a process, a syscall, a
device, and the emulator's expected output. `docs/nix.md` builds the same package
`nix build` builds, in the same sandbox, so a bug in the packaging is a bug there —
but a program that misbehaves on one machine and not another is not something a
build sandbox can see, and pretending otherwise would be the wrong lesson to end a
reproducibility step on.
