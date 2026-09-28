# Expanding Lazen

Step 92 of the roadmap lists six things to consider:

```text
generics
pattern matching
advanced collections
concurrency
advanced modules
macros/metaprogramming
```

and then says, in the same breath, **"Do not add features merely because another
language has them."** That instruction is the step. The interesting work is not
deciding which of the six to build; it is deciding which of the six this
repository has a use for, and being able to say *why* in terms of something
checkable rather than in terms of taste.

So this document does both halves: it records the one that was built, and for
each of the five that were not it names the **gate** — the thing in this
repository that would have to exist first, and which is worth building on its own
merits whether or not the feature ever follows.

| area | verdict | the gate |
| --- | --- | --- |
| **pattern matching** | **built** — `match` on a number or a `bool` | none; the arm grammar *is* a comparison chain |
| generics | not yet | a user-defined container to be generic over, and monomorphisation in the backend |
| advanced collections | not yet | an allocator the SDK can reach — Lazen has no `alloc` and no length-carrying growth |
| concurrency | not yet | `spawn_thread` — step 91 built the per-thread state and did not build creation |
| advanced modules | not yet | a module tree that has a facade to re-export through |
| macros / metaprogramming | not yet | compile-time evaluation in the IR |

## `match`: the one that was built

The compiler *already* refused `match`, and its refusal said why: *"Lazen v1 has
no enums, so there is nothing to match."* That reasoning was half right, and the
half that was wrong is what step 92 found.

**What is right:** Lazen has no enums, no records, and no destructuring. A pattern
cannot bind a name or pull a field out of a value, because there is nothing to
pull a field out of. So a pattern can only ever be *a value to compare against*.

**What is wrong:** everything in this platform returns a tagged integer. A syscall
returns an `i64` status. `std::string::get` returns a byte and an index. A
`for` loop over a view returns a count. The *entire* platform is shaped like an
untagged union, and every program that consumes one writes the same chain:

```lazen
if status == 0 {
    // ok
} else if status == 1 {
    // retry
} else {
    // failed
}
```

which is a `match` with the comparisons written out and — the part that matters
— **the scrutinee written out too**. So every one of those chains calls the
scoring function once per arm. Not because the language is broken, but because the
language asks the programmer to remember something the programmer did not know
they had to remember.

So `match` is sugar for that chain, and the sugar is where the guarantee lives:

```lazen
fn name_of(status: i32) -> &[u8] {
    match status {
        0 => { return "ok"; },
        1 => { return "retry"; },
        else => { return "failed"; },
    }
}
```

### Four decisions, each of which could have gone the other way

**The scrutinee is bound, and the binding cannot be captured.** The desugaring
is `let $match_4 = status;` followed by `if $match_4 == 0 { } else if …`. That name
begins with a `$`, and the lexer cannot produce a `$` in an identifier — an
identifier is a letter or `_` followed by letters, digits, and `_` (`tests/lexer.rs`
pins this, and the test is written so that it will fail if a `$` ever becomes
identifier text). So the binding cannot shadow a name in the arm bodies, no arm
body can accidentally refer to it, and no program can declare it. Hygiene by
construction rather than by a renaming pass.

**A `bool` pattern is the condition, not a comparison.** `==` is an integer
operator in Lazen, so `flag == true` is not an expression that exists. Rather
than teach `==` about `bool` to make one sugar desugaring work, `match flag { true
=> … }` becomes `if flag { … }` and `false` becomes `if !flag { … }`. A `bool`
pattern *means* "when the value is true", and the language already had that
expression, so the sugar uses it.

**The `else` arm is required.** With no enums, the compiler cannot know which
values an `i32` can hold, so exhaustiveness checking is impossible and pretending
otherwise would be worse than not having it. The alternative — a `match` that is
allowed to fall off the end — makes an unhandled value a silent no-op, and a
silent no-op in a syscall-status chain is a bug that does not reproduce. So the
`else` is mandatory, with its own diagnostic (`P0109`) saying exactly this. A
`match` with no arms to choose from is a refusal too, pointing at the `if` that
says the same thing in fewer characters.

**A `match` is a block expression, and that is what makes arms able to `return`.**
A value conditional in this language has no frame of its own, so an arm of a
*value* `match` may not bind a name (`ARM_BINDING`). In *statement* position a
`match` becomes a binding plus an `if` statement, so its arms may `return`, may
bind, and need no value at all — which is the shape a status chain wants. A
`match` whose arms all `return` is a statement, and a `match` whose arms produce
values is a value; the parser decides from the context, exactly as it already
does for `if`.

### Why there is no `Match` node

`match` has no variant in the AST. It is desugared in the parser, so the type
checker sees the same `if` chain a programmer would have written, the IR is the
same IR, the lowerer has nothing to know, and the verifier checks the same thing
it checks everywhere else.

The alternative — a `Match` variant threaded through the tree, the type checker,
the lowerer, the IR and the formatter — buys nothing except four more places for
the feature to disagree with itself. A feature that can be expressed in the
language already has is, in this language, already implemented.

One consequence is worth naming because it is the kind of thing that surprises
people later: **the formatter would have broken `match` if it printed from the
tree.** Step 90's formatter works on the *token* stream precisely so that
comments and blank lines survive, so it prints `match` back out unchanged. A
tree-printing formatter would have rewritten a person's `match` as a `$match_4`
binding and an `if` chain — a different program in their file, and one that would
not even compile. `tests/format.rs` asserts that no `$match` ever reaches a file.

## The five that were not, and what unblocks each

### Generics — gated on there being something to be generic over

Lazen's type set is closed: `bool` and ten integer widths. `docs/lazen-types.md`
explains why that is a feature on a platform whose heap is not yet served, and
`docs/lazen-memory-model.md` explains the consequence — every value's size is
known from its type, so a frame is a layout, not a guess.

Generics need a *uniform* type: one name standing for a set of types, with the
set's members known at the point of use. The two hard parts are both about that
set:

- **What would be generic?** Not the built-in types — they are ten concrete
  widths, each with a deliberately different ABI story. A user-defined container,
  or a function that takes "any array", and there is neither today. Adding
  `fn sum<T>(…)` before there is a `Vec` or a `Stack` would be a feature with no
  program that could use it, which is exactly the "merely because another
  language has them" case.
- **Where would it monomorphise?** A generic function must be lowered to one
  concrete copy per type, which means the resolver has to instantiate, the frame
  layout has to be computed per instantiation, and the linker has to mangle them
  apart. The linker already mangles Lazen functions apart, but the *frame* of a
  generic function is not a frame of anything until the type is known, and
  `lower.rs` computes a frame per function. This is real work with a real design
  question in it, and doing it first would be doing it blind.

**The gate is a container.** When Lazen has a growable collection, "the same code
for any element type" becomes a real request with a real answer, and the
monomorphisation question has a real first case. Build the collection first.

### Advanced collections — gated on an allocator Lazen can reach

Lazen has fixed-size arrays `[T; N]` and views `&[T]`, and the memory model
document is explicit that there is no heap: a program's memory is its frame, its
static data, and the memory the OS hands it in the process layout.

So `Vec<T>`, `String` (a growing one — the existing `String` is a view over
bytes), `HashMap`, and every dynamic collection need three things, in order:

1. **An allocator the SDK can call.** The OS has `AllocateMemory`; the Lazen ABI
   has no allocation call, and step 91's capability table has no memory capability
   to gate one behind. Until a program can ask for memory, a growing collection
   has nowhere to grow to.
2. **A reallocation rule.** Growing means a new block, a copy, and a free of the
   old one — which is a *capability* question as much as a memory question, and
   step 91's gate is the right place for it.
3. **A borrow story for a moving buffer.** A `&[T]` into a `Vec<T>` is
   invalidated by a push. The current model has no way to express "this borrow
   ends here", because there is no `Result` to return the error through and no
   lifetime to shorten.

**The gate is a length-carrying allocation call in the Lazen ABI, with a
capability behind it.** That is step 91's `memory` capability waiting for a
syscall to gate, and it is a smaller and more useful piece of work than a
collection type.

### Concurrency — gated on `spawn_thread`, which step 91 did not build

Step 91's table is explicit: the per-thread state is real (each thread id has its
own `ArchitecturalState`, and the scheduler moves a machine's state into a *named*
thread), and **nothing creates a second thread**. Every process has exactly one,
and `assert_eq!(process.thread_count(), 1)` is in the tests so that a process
which grew a second thread with nothing creating one would fail rather than pass
quietly.

Threads in the *language* are therefore blocked on a thing the *OS* is missing. And
the OS half is not a syscall; it is three things at once:

- a place for the new thread's stack inside the process's own memory — which is
  the virtual-memory work, since the stack must be mapped rather than merely
  reserved;
- a decision about what a thread starts *running* — the ISA has a register state
  and the loader has a `main`, but a second entry point is a loader concept the
  image format does not have;
- the scheduler round-robining over threads as well as over processes, which is
  where a fairness property would first need to be stated.

**The gate is `spawn_thread` in the OS, and a second entry point in the image
format.** Both are worth doing for the OS's own sake: step 91 left the platform
unable to use a second core for anything.

### Advanced modules — gated on there being a facade

Lazen has `mod`, `use`, paths, `pub`, and `const`. The obvious "advanced" features
are `pub use` re-exports, glob imports, and module aliases.

The SDK is the test of whether they are needed, and the SDK says no: every one of
its symbols is called at its own path — `std::graphics::white()`,
`std::input::poll()` — and `std::graphics` has no inner `std` of its own, so
there is nothing to re-export *through*. A re-export only earns its place when a
module is a facade over another module, and the one module tree in this
repository has no facade in it.

Glob imports are refused for a sharper reason: a glob is a name the reader cannot
see. `use std::graphics::*;` makes `white` in a function body a name whose origin
is three files away, and the diagnostic for a typo'd name — which is a good
diagnostic, and one step 92 did not want to make worse everywhere — becomes
"unknown name" where the real answer is "it was never imported".

**The gate is a module that is genuinely a facade.** When one exists — a
convenience namespace over a subsystem, most likely — re-exports and aliases are
the right answer and will be obvious rather than speculative.

### Macros and metaprogramming — gated on compile-time evaluation

This is the most tempting of the six and the one with the most misleading
"already half built" argument, so it is worth being precise about what exists:

- Step 90's formatter gives the toolchain a **token stream with comments
  attached** — the substrate a hygienic expander needs. True.
- Nothing evaluates anything at compile time. The IR has no constant folder, no
  partial evaluator, and no compile-time interpreter.

A macro that cannot be evaluated at compile time is a function call with extra
steps, and a function is already that. So a macro system here would either be (a)
compile-time evaluation, which is a real subsystem with a real cost — a
`String`-returning program run by the compiler, in the compiler, with all the
decisions about what a program may do at compile time that implies — or (b) a
call, which `fn` already is and which adds only a name to grep for.

The tempting middle is a "text-substitution macro", the preprocessor every
language has had and most have regretted: it re-introduces the token-pasting bugs
that make error messages point at generated code nobody wrote, and it contradicts
`docs/lazen-syntax.md`'s explicit omission of "a preprocessor, macros, and
compile-time evaluation".

**The gate is compile-time evaluation in the IR** — a constant folder and a way
for a program to run during lowering. That is worth having on its own, because it
is what makes `const` more than a literal with a name, and it is checkable
without designing a macro system at all.

## What would change these verdicts

A verdict this document can be argued with is worth more than one that cannot, so
here is what each answer to "no" would look like:

- **generics** — a Lazen container type exists. Then `fn len<T>(…)` has a first
  case, and monomorphisation can be designed against a real instantiation.
- **collections** — the ABI has an allocation call. Then `Vec<u8>` is a page of
  code, and its borrow story can be written down before it is written in Rust.
- **concurrency** — the OS can spawn a thread. Then the language question is
  "what does a thread return", and there is an honest answer in `i64` plus an
  out-parameter.
- **modules** — a facade module exists. Then `pub use` is a two-line feature
  instead of a name-resolution design.
- **macros** — the IR folds constants. Then a macro is a function the compiler
  runs, and the remaining question is hygiene, which is tractable once evaluation
  is real.

None of the five is blocked on difficulty. Each is blocked on a prerequisite
that is worth building anyway — an allocator, a thread, a container, a facade, a
constant folder. That is the shape a good roadmap item has: the feature waits for
its reason.

## Verification

- `match` is covered by tests at every layer the change touches: the lexer (a
  keyword, and a `$` that cannot be an identifier), the parser (the binding, the
  comparison, and the three refusals), the type checker (that it adds no rule of
  its own, and that it is refused wherever an `if` chain would be), the lowerer
  (one evaluation of the scrutinee, arm order, and a `bool` pattern becoming a
  condition), and the formatter (that it comes back out as a `match`).
- `docs/lazen-syntax.md` section 13 no longer lists `match` as an omission, and
  the test that pins section 13 now checks the inexhaustive-`match` diagnostic
  instead of the old "no such feature" one.
- 1119 workspace tests pass, and fmt, Clippy, check, `nix flake check` and
  `nix build` are green.
