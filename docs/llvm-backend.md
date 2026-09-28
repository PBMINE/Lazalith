# The optional second backend

Step 94 says LLVM is optional, gives a shape, and states two rules:

```text
Lazen ──┐
        ├──→ Lazalith IR → Native Backend
C ──────┘                 \
                           → Optional LLVM Backend
```

**LLVM must NOT become mandatory. Native Lazalith infrastructure remains the
foundation.**

This step adds no LLVM. What it adds is the thing that makes "optional" mean
something structural rather than aspirational, and a test for each rule.

## The shape, and the one thing it was missing

Two frontends already feed one IR — that is the diagram's left half, and it exists.
The right half is a *fan-out*: one IR into more than one backend. What was missing
was any place for the second backend to attach, because `lazalith-codegen` exported
a free function `generate` and nothing else. A second backend under that shape
would have to be threaded through the compiler, the C compiler, the toolchain, and
the command line — which means it would be *mandatory* in the only sense that
matters: every caller would have to know it exists.

So this step adds `pub trait Backend`, `NativeBackend`, and `available_backends()`.
`generate` is now a free function that calls `NativeBackend`, so **every existing
caller is unchanged** and the seam is a place a second backend can register rather
than a layer the codebase has learned to route around.

`the_seam_is_real_because_the_native_backend_produces_the_same_object` is what makes
that more than a comment: it compares the two objects byte for byte and the frame
records exactly. If someone later routes around the trait, that test fails.

## The rules, as tests

### "LLVM must not become mandatory"

This is the rule about the future, and a promise about the future is worth exactly
as much as the test enforcing it. Two tests:

- **`no_llvm_crate_is_in_the_lock_file`** walks `Cargo.lock` and fails on any crate
  whose name starts with `llvm`. `llvm-sys` would be the obvious first step toward a
  real LLVM backend, and it is exactly the step that would make a forty-megabyte C++
  library a build requirement of a platform whose entire premise is that it has no
  third-party Rust dependencies.
- **`every_manifest_depends_only_on_path_dependencies`** walks all 26 manifests and
  fails on any registry dependency. It walks them rather than checking a list,
  because a list is the thing that goes stale, and a test that checks a list tests
  the list.

If either test ever needs removing, the removal *is* the step 94 review. That is
written into the test so that whoever reaches for it reads the sentence first.

### "Native Lazalith infrastructure remains the foundation"

Three things hold that up, and all three are checked:

- `available_backends()` has one entry and it is `native`
  (`the_native_backend_is_the_default_and_the_only_one`).
- The free function every existing caller uses *is* the native path
  (`the_seam_is_real_because_...`).
- The native backend still produces a working program with a frame for `main` and a
  non-empty object (`the_native_backend_generates_a_program_the_linker_can_use`).

The last one is the one that matters most. A step that added a seam and broke the
one working backend would satisfy the first two tests and fail that one.

## What "optional" has to mean in a build system

The roadmap says *optional*, and the temptation is to read that as "there, but
nobody uses it". In a build system that reading is wrong, because the cost of an
unusable dependency is paid by everyone who builds the project. So the test above is
written the other way around: there is **no** LLVM code in the tree at all.

A real second backend, if it is ever built, would be:

- a crate that depends on LLVM;
- behind a Cargo feature that is **off by default**, so `nix flake check` and
  `nix build` never see it;
- a new entry in `available_backends()` compiled only under that feature;
- and it would have to *earn* its place in the two rules above, which means the
  LLVM-specific tests would be the only tests that could fail on a machine without
  LLVM — and the native tests would have to keep passing without it.

That is the shape. Writing it down is most of what this step can do honestly.

## What an LLVM backend would actually have to do

Not "call into LLVM" — the work is deciding what it is compiling *from*, and there
is a trap in it worth naming.

The IR is a **register-machine IR**: it has virtual registers, a frame layout the
lowerer computed, and no notion of a host calling convention. An LLVM backend
cannot just hand it to LLVM. It would have to, in order:

1. **Allocate host registers.** The IR's registers are the guest's; a backend must
   assign them to host registers or to stack slots, which is a register allocator.
2. **Match the calling convention.** The frame layout in `docs/isa.md` is a *guest*
   convention. The ABI also has a documented calling convention for a guest calling
   a guest. LLVM wants a *host* one. Something has to bridge them, and that bridge
   is where a JIT is born — which is why step 93 refused the JIT for the same reason
   this section is here.
3. **Decide what an instruction means to the host.** The ISA's arithmetic has
   specified trapping, alignment, and width behaviour. LLVM's does not. Every
   instruction the backend lowers has to re-implement Lazalith's semantics in terms
   of host operations plus checks for everything LLVM would do differently — signed
   overflow, unaligned access, and the trap that must fire instead of a wrap.
4. **Emit an object and relocations.** The native backend assembles Lazalith
   machine code and produces relocations the project linker resolves. An LLVM
   backend would produce a host object, which the project linker would then have to
   be able to read — and `lazalith-toolchain`'s object reader is written for
   Lazalith's own format.

Point 4 is the one that makes this a project rather than a feature. An LLVM backend
that only worked for the *native* target would be a different toolchain, not a
backend, and it would not run on the guest at all.

## Why it was not built anyway

Three reasons, in order of weight:

1. **It cannot be verified against anything.** A second backend is a second
   implementation of the semantics. The native one is checked by the whole suite and,
   since step 85, differentially against a bare reference interpreter. A second
   implementation that nobody can check is not a faster platform, it is a second
   thing to be wrong.
2. **It buys nothing the platform needs.** Step 93 measured where the time goes:
   validation, state cloning, and memory bookkeeping — not code quality. LLVM
   optimizes code generation. The interpreter is not yet slow for a reason LLVM
   could fix, and by the time it were, step 93's write barrier would be the more
   valuable work.
3. **It would be the most expensive dependency in the project**, added for a
   capability nothing currently asks for, in a platform whose stated values include
   a zero-dependency toolchain. That trade is only worth making when something
   needs it.

## What this step is, then

Not an LLVM backend. A **seam, two tests, and an honest account of what the work
would be** — plus the finding that the work is larger than it looks, because the
project's object format, calling convention, and trap semantics would all have to
be expressed twice.

That last part is the real output of the step. A reader who assumed "add LLVM" meant
"call LLVM from the codegen" now knows it means "decide what Lazalith's arithmetic
means on a host whose arithmetic differs, then express it twice, and prove the two
expressions agree."
