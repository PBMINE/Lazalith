# Lazalith — Project State

Last updated: 2026-09-26 (Steps 1–63 verified; Steps 64–75 not started)

## Where the roadmap stands

```text
Steps 1–50    complete, audited, repaired, verified, committed (31dbf86)
Steps 51–59   complete: the Lazen design cluster (documentation only)
Step  60      complete: lazalith-ir, the shared low-level IR (66d29cf)
Step  61      complete: lazalith-compiler, the Lazen frontend (7a4a5b3)
Step  62      complete: lowering from the checked tree to lazalith-ir
Step  63      complete: code generation to a Lazalith object
Steps 64–75   NOT started. No runtime, driver, GUI, or debug code exists.
```

This milestone added code generation. Steps 1–62 are unchanged except for the
defects Step 63 found by running the generated code, each of which is listed
under its own heading below. The 588 workspace tests all pass, including 22 in
`crates/lazalith-codegen/tests/codegen.rs` that run generated code on the real
machine and compare what it wrote and what it exited with.

### What deliberately did not land

An attempt was made to carry Steps 61–63 in the Step 60 milestone. The frontend
reached a working lexer, a recursive-descent parser, and a name resolver, but
the type checker and lowering pass were drafted against a larger language than
the milestone could finish and verify, and the drafted lowering was itself
unsound (it treated a function-address intrinsic as a frame pointer, lowered
`continue` to `unreachable`, and dropped slice lengths). Rather than commit code
that compiles-but-is-wrong, or leave a non-compiling crate in the tree, the
compiler crate was removed and that milestone was closed at Step 60.

The consequence for the design documents is that they now describe the language
that will actually be built, not a larger one: `docs/lazen-syntax.md` and
`docs/lazen-types.md` record that Lazen v1 has no `optional`, no enums, no
records, and no `match`, with the reason for each exclusion. That decision was
made *because* of the failed attempt, and it is the right one: every remaining
step depends on a type system and a lowering pass that can be audited, and a
closed v1 type set is what makes that possible.

Step 61 was then rebuilt from the documents rather than from the deleted code,
and the three mistakes that made the attempt unsound are now structural: the
frontend emits frame *offsets* and never a frame pointer, it records a slice or a
`str` as both a pointer and a length, and it has no lowering at all, so none of
those decisions can be wrong yet.

## Steps 51–60 Dependency Map and Entry Points

The roadmap for the rest of this milestone, with the dependency each step has:

```text
51 purpose ─┐
52 syntax ──┤
53 memory ──┤ design cluster: documentation only, no code      [complete]
54 types ───┤
55 modules ─┤
56 apps ────┤
57 sdk ─────┤
58 graphics ┤
59 input ───┘
            ↓
60 Lazalith IR (crates/lazalith-ir)                            [complete]
            ↓
61 Lazen frontend: lexer, parser, AST, name resolution, type checking
            ↓
62 Lazen lowering: checked program → Lazalith IR
            ↓
63 Lazen code generation: IR → Lazalith instructions → .lzo
            ↓
64 Lazen runtime: startup, stack, syscall wrappers (linked into every program)
            ↓
65 first Lazen program runs under LazOS            ← milestone checkpoint
            ↓
66 CLI: new, check, build, run (test/fmt deferred: no test runner, no formatter)
            ↓
67 standard library: core, io, text, math, collections, fs, process
            ↓
68 Virtual Display Device        69 Virtual Input Device
            ↓                                  ↓
70 LazOS display driver      71 LazOS input driver + host input adapter
            ↓                                  ↓
72 first graphical Lazen application (window, draw, keyboard, state)
            ↓
73 Lazen GUI library
            ↓
74 Debug API: DebugController, DebugSession
            ↓
75 machine snapshots: CPU, device, process
```

Decisions taken before implementation, so the design documents and the code
target the same thing:

- **Lazen v1 has no ownership, GC, or reference counting** (`lazen-memory-model.md`).
  LazOS has no served heap, so a collector would be a collector over a bump
  allocator. Values live in static data, on the stack, or in OS memory.
- **The v1 type set is closed**: `bool`, `i8`–`i64`, `u8`–`u64`, `usize`, `str`,
  `ptr<T>`, `&[T]`, `&mut [T]`, `[T; N]`. No `optional`, enums, records, or
  `match` (`lazen-types.md`).
- **Bounds failures trap, they do not panic.** An out-of-range index executes the
  ISA software trap with a documented code, which the scheduler already turns
  into a faulted process.
- **The code generator will use a stack discipline.** The ISA is a flat register
  machine with sixteen registers, so an expression stack in real stack memory is
  the correct v1 lowering. Locals are addressed relative to the stack pointer,
  which the ISA can read with `GETSP`; a register allocator is future work.
- **The OS ABI grows new calls within v1.** Steps 70 and 71 need display and
  input calls. `ABI_VERSION` stays 1, the new IDs are appended above the existing
  ones, and the addition is recorded in `docs/os-abi.md`.
- **The display is not MMIO.** The display device owns a framebuffer *RAM
  region*, and `present` is a synchronization point, so a User context and a
  device mapping never need to coexist. This keeps the Step 25–50 rule that a
  user context cannot be bound while a device mapping exists intact.

## Step 60 — Lazalith IR

`crates/lazalith-ir` is the common low-level representation that sits below
language-specific syntax trees. It contains no language concept: no generics, no
closures, no traits, no ownership metadata.

```text
IrModule      name, functions, data segments
Function      name, linkage, params, result, blocks, span
Block         label, instructions, one terminator
Instruction   Const Binary Unary Compare LogicalAnd LogicalOr Load Store
              Call Intrinsic Copy Extract Insert Trap
Terminator    Jump Branch Return Unreachable
Type          Void Bool Int Pointer Slice Record Enum Function
```

Decisions recorded in the crate:

- **Values are function-local and densely numbered.** Parameters occupy the first
  identifiers, then every instruction that produces a value takes the next one.
  `Store` and `Trap` produce nothing and consume no identifier.
- **The verifier is a gate, not a suggestion.** It checks uniqueness, that every
  operand is defined, that every use is dominated by its definition using an
  iterative dominator computation over the reverse postorder, that a call's arity
  and argument types match the callee, that a store's width matches the value's
  size, that a return matches the declared result, and that a data segment's
  alignment is a power of two.
- **The builder prevents malformed modules structurally.** A block must be
  terminated before a new one is created, an instruction cannot follow a
  terminator, switching back to a finished block is allowed so a front end can
  close a loop, and finishing a function checks that every block is terminated.
- **`CallTarget::Syscall` records a name, not a number.** The IR never contains a
  syscall number, so the ABI mapping stays in one place: the compiler's table
  over the shared `lazalith-os-abi` definitions.
- **Unreachable blocks are not dominance-checked.** Nothing can reach them, so a
  verifier cannot prove anything about them, and a code generator may drop them.

18 tests cover the verifier, including tests written so that removing a specific
check fails the suite: undefined operands, undominated uses, unknown callees,
unresolved imports, arity and argument-type mismatches, store width mismatches,
return/result mismatches, degenerate branches, duplicate definitions, and the
builder's own structural rules. The earlier Steps 25–50 suite (334 tests) still
passes unchanged, so the new crate introduced no regression.

## Steps 51–59 — Lazen Design Cluster

Nine documents, no code, in the order the roadmap requires:

| Step | Artifact | Decision recorded |
| --- | --- | --- |
| 51 | `docs/lazen-purpose.md` | Lazen is the application language; C and assembly keep their documented roles |
| 52 | `docs/lazen-syntax.md` | 12 worked example programs, the explicit v1 omissions, and the type of each builtin |
| 53 | `docs/lazen-memory-model.md` | manual memory, no ownership, three memory places, trap-based bounds checks |
| 54 | `docs/lazen-types.md` | the closed v1 type set, ten checker rules, and the rejected features with reasons |
| 55 | `docs/lazen-modules.md` | modules, visibility, imports, crates, packages, dependency rules |
| 56 | `docs/lazen-applications.md` | `lazen.toml` manifest, entry point, resources, permissions, versioning |
| 57 | `docs/lazen-sdk.md` | the SDK is Lazen over the OS ABI only, with a per-module availability table |
| 58 | `docs/lazen-graphics.md` | guest-owned framebuffer, ARGB8888, canvas primitives, clipping |
| 59 | `docs/lazen-input.md` | 16-byte guest event record, stable key codes, polling, determinism |

The syntax document is load-bearing: its example programs are the test fixtures
for the frontend, so the documented grammar cannot drift from the implemented
grammar. The graphics and input documents specify contracts for the ABI calls
that Steps 68–71 will implement; the Step 75 audit must check that the documents
and the implementation agree.

## Step 61 — Lazen Compiler Frontend

`crates/lazalith-compiler` is the Lazen frontend: lexer, parser, AST, resolver,
type checker, and semantic analysis. It is `no_std` with `unsafe_code = forbid`,
depends only on `lazalith-types`, `lazalith-diagnostics`, `lazalith-ir`,
`lazalith-isa`, and `lazalith-os-abi`, and it is headless: no SDL, no runtime, no
code generation, no machine execution, and no filesystem access.

```text
source -> lexer -> parser -> AST -> resolver -> type checker -> CheckedProgram
```

Every failure is a shared `lazalith_diagnostics::Diagnostic` with a stable code, a
real `SourceSpan`, and the shared `SourceManager`. There is no second diagnostic
architecture, and no path from source text to a panic.

| File | Contents |
| --- | --- |
| `src/lib.rs` | crate documentation, the pipeline map, the public surface |
| `src/diagnostic.rs` | `StageError`, `CompileError`, the code helper, the renderer bridge |
| `src/lexer.rs` | tokens, spans, integer suffixes, and every lexical diagnostic |
| `src/ast.rs` | the typed syntax tree; every node that came from source carries a span |
| `src/parser.rs` | recursive descent over the documented grammar |
| `src/resolve.rs` | modules, visibility, imports, scopes, duplicates |
| `src/types.rs` | the closed v1 type set, every typing rule, the checked program |
| `src/frontend.rs` | `compile`, the one entry point, which stops at the first failure |

### Diagnostic codes

| Range | Stage | Example |
| --- | --- | --- |
| `L0xxx` | lexer | `L0103` a float literal, `L0101` a block comment, `L0113` an unknown escape |
| `P0xxx` | parser | `P0001` a missing token, `P0100` a `struct`, `P0108` the `?` operator |
| `N0xxx` | resolution | `N0001` an unknown name, `N0002` a duplicate item, `N0004` a private item |
| `T0xxx` | types | `T0001` a mismatch, `T0006` an immutable assignment, `T0011` a forbidden cast |

### Decisions recorded in the crate

- **The v1 type set is closed, and the checker is the only place that knows it.**
  `bool`, `i8`–`i64`, `u8`–`u64`, `usize`, `str`, `&str`, `ptr<T>`, `&[T]`,
  `&mut [T]`, and `[T; N]`. `optional`, enums, records, and `match` have no
  representation here at all, so a program that needs one is rejected by name.
- **`str` and `&str` are one type.** A `str` is already a pointer and a length,
  so a reference to it would add nothing.
- **Only a conditional with an `else` can be a value.** Without one there is no
  value when the condition is false, so the parser records it as a statement, and
  a statement that ends in a value is a `T0015` discarded-value error.
- **A raw pointer is never dereferenced.** `ptr<T>` carries no length, so a
  dereference could not be bounds checked, and v1 has no `unsafe` in which to
  justify one. `*p` on a `ptr<T>` is `T0010` with that reason.
- **A value conditional may not bind a name in an arm** (`T0027`). Step 61 records
  a value conditional's arms without a frame of their own, so a name bound there
  would have no slot; rather than invent one, the construct is rejected. Step 62
  allocates slots when it lowers the arms and can lift this.
- **Inference is narrow on purpose.** An integer literal takes its type from
  context, or `i32` when it has none; a literal suffix fixes it; a literal that
  does not fit is `T0017`, never a truncation. Nothing else is inferred and no
  conversion is implicit, which is why a program that mixes widths says `as`.
- **A negated literal is range-checked as the negative value it is.** `-128i8` is
  the minimum value and is valid; `-1u8` is an error. The literal's digits alone
  are not what the value is.
- **The builtin methods are a closed set**: `len`, `as_bytes`, `as_slice`,
  `as_mut_slice`, and `as_ptr`. There are no traits, so an unknown method is
  rejected with the list of valid ones for the receiver's type.
- **A frame slot has a number and a byte offset; the frame has no pointer here.**
  Offsets are data Step 62 needs, computed from the target's word size. The frame
  *base* is the machine's own stack pointer; the frontend never fabricates one,
  which is the mistake that made the deleted attempt unsound.
- **An extern declaration is checked against the shared ABI, and a reserved name
  carries no number.** `write` and `close` map to `lazalith_os_abi::Syscall`
  values; `display_open`, `display_present`, and `input_poll` are the calls the
  graphics and input documents specify, and they are accepted with
  `syscall: None` because the ABI does not number them yet. A test asserts the
  name table covers `Syscall::ALL` exactly, so a new ABI syscall cannot slip in
  unresolvable.
- **Every documented omission is rejected by name**, with the reason and a
  pointer to section 13 of `docs/lazen-syntax.md`: records, enums, `match`,
  `optional`, `some`/`none`, traits, `impl`, `type`, `unsafe`, `?`, `self`, block
  comments, float literals, `for` over a collection, and the `*T` pointer type.

### Bugs found and fixed while building this step

Each of these was found by a test in this crate, not by inspection:

- The lexer skipped trivia only before the first token, so every token after
  whitespace was reported as an unknown character.
- `&mut` was matched without an identifier boundary, so `&mutate` lexed as `&mut`
  followed by `ate`.
- A string literal advanced one byte at a time, which split a multi-byte UTF-8
  character and panicked.
- `0..10` was lexed as a float literal; a `.` after digits is a float only when a
  digit follows it.
- `parse_arguments` consumed the closing parenthesis and its caller expected it
  again, so no call with an argument list parsed.
- A cast bound looser than a unary operator, so `&mut handle as ptr<u32>` parsed
  as `&mut (handle as ptr<u32>)` and was rejected as an unassignable place.
- `parse_if_arms` consumed `else` and then advanced again, skipping the `{`, so
  every `if`/`else` failed to parse.
- A statement's tail-or-statement decision was made from the token *before* the
  expression, so `f(1);` in a block was taken as a tail and the next `;` was a
  parse error.
- Compound assignment was consumed as an addition followed by an `=`.
- The root module was not in the module map, so no top-level function, extern, or
  const was ever checked.
- Name lookup walked every path segment but the last as a module, so
  `geometry::area` never resolved.
- Names were looked up only in the root module, so a function could not call a
  private sibling of its own module, and a `use` alias broke parameter lookup.
- A conditional used as a value re-checked its arms with an empty scope and a
  fresh slot allocator, so a name in an arm resolved to nothing and a binding
  would have been given a slot that collided with the function's.
- A `&mut [T]` parameter could not be written through, because the parameter
  binding itself is immutable even though the data it points at is not.
- `values.as_mut_slice()[0] = 1` was rejected as an unassignable place; the view
  now resolves to the array it points at.
- A literal in a context expecting a different type was accepted, so `[1, "two"]`
  compiled.
- A call's result was never compared with the type its context required, so
  `fn f() -> i64` satisfied a `-> i32` function.
- An integer literal with a context was defaulted to `i32` inside a comparison, so
  `f() == 0` failed for an `f() -> i64`.
- A loop's depth was lost when a loop body's last statement was a conditional, so
  `continue` inside `loop { if c { continue; } }` was rejected.
- `&mut` used as a name prefix was not reported, and `match`, `some`, and `none`
  parsed as ordinary names.

### Tests

| File | Tests | Covers |
| --- | --- | --- |
| `tests/lexer.rs` | 25 | every token family, spans, boundaries, every lexical diagnostic |
| `tests/parser.rs` | 35 | every type form, precedence and associativity, tail versus statement, delimiter errors, EOF, nesting limits |
| `tests/resolver.rs` | 23 | duplicates, scopes, shadowing, visibility, `use`, paths |
| `tests/typecheck.rs` | 56 | one positive and one negative case per rule, plus source locations |
| `tests/documented_examples.rs` | 36 | every program in `docs/lazen-syntax.md`, and every documented omission |
| `tests/smoke.rs` | 6 | the pipeline, frame layout per target, and 60 malformed programs that must not panic |

181 tests in the crate; 533 in the workspace, all passing. The earlier 352
tests pass unchanged.

### Specification corrections

Building the frontend found three places where the documents contradicted
themselves. The documents were corrected, because the repository is the source of
truth and a specification that cannot compile is not a specification:

- Section 1 called `write` with two arguments against its own four-argument
  declaration. It now passes all four, and the document states that a call must
  pass exactly the declared arguments.
- Section 7 passed an `i32` literal to a `u32` parameter. It now annotates the
  binding, because v1 has no implicit conversion.
- The notation section did not say whether strings have escapes, while the
  examples used `\n`. The six v1 escapes are now specified, and the float
  literal in section 2 is marked as the rejection it is, with its code.

### Remaining limitations

- A value conditional cannot bind a name in an arm (`T0027`), as recorded above.
- `display_open`, `display_present`, and `input_poll` are accepted but carry no
  syscall number until the ABI step that adds them.
- There is no `lazen.toml` reading, no package resolution, and no module file
  loading: Step 61 compiles one file, which is what the roadmap asks for. The
  manifest and package rules of `docs/lazen-applications.md` and
  `docs/lazen-modules.md` are Step 66's work.
- A `while true { }` is not a way to spell an infinite loop; `loop { }` is, and a
  function whose body is a `loop` with no `break` satisfies its result type.
- `aarch64-linux` remains untested.

## Step 62 — Lowering to IR

`crates/lazalith-compiler/src/lower.rs` turns a `CheckedProgram` into a verified
`lazalith_ir::Module` plus one `FrameLayout` per function. Nothing below is
invented: every frame offset comes from the frontend's `LocalSlot`, and the
temporaries this stage needs are reported in the layout rather than hidden.

### Representation decisions

- **The frame base is the machine's stack pointer.** A local's address is
  `Intrinsic::FrameBase + offset`. The ISA has `GETSP` and `SETSP`, so a backend
  materialises this with real instructions: the prologue moves SP down by the
  reported frame size, and the base is whatever SP then holds. It is never
  `FunctionAddress`, which is a function's identity and differs per call site.
- **Parameters arrive in registers and the prologue stores them into their
  slots.** The body then reads every local the same way, parameter or not, so
  there is one rule instead of two.
- **A view is two words and is never half-copied.** A `str` and a slice are built
  with `Insert` at offsets 0 and 8, taken apart with `Extract` and
  `Intrinsic::SliceLength`, and stored in the frame as their two words. The
  deleted prototype dropped lengths here, which is the whole reason the
  `Instruction::Copy` fix below exists.
- **A string's address is a symbol.** `Instruction::DataAddress` names a data
  segment the linker places; a frontend cannot know an address, and a made-up one
  reads the wrong bytes silently.
- **There are no phi nodes, so a value leaves an `if` through the frame.** Each
  arm stores its value into a `JoinValue` temporary and the join block reads it.
- **`&&` and `||` short-circuit through branches.** `Instruction::LogicalAnd` is
  eager, and the right side of `&&` can trap, so the right side gets its own
  block and the result comes out of a one-byte `ShortCircuit` temporary.
- **`break` and `continue` are jumps to real blocks.** Every loop has four: a
  test, a body, a step, and an exit. The step block exists so `continue` does not
  have to jump to the test and skip the body's last effect.
- **An index is checked before its address is formed.**
  `Instruction::BoundsCheck` compares the index against the place's own length,
  as unsigned values of the same width, and traps with `TRAP_BOUNDS`.
- **A cast is one load.** Widening reads the source's width and lets the load
  extend it, which is what `LDZ`/`LDS` do; narrowing reads the target's width.
  The extension follows the *source* type, so a `u8` of 200 stays 200.
- **Block identifiers come from the builder, not from arithmetic.** A loop or a
  conditional has to name a block it has not built yet. Step 62 worked that out
  by counting, and the count was wrong as soon as a body created a block of its
  own; the fix is in Step 63's section below. The lowering now reserves each
  block and uses the identifier the IR builder hands back.

### Bugs this step found and fixed

- **`Instruction::Copy` was typed `Void`.** A copied aggregate therefore read as
  nothing at all, which is precisely how a slice length could be lost. It now
  carries its type.
- **A one-operand bounds check could not check anything.** `Intrinsic::BoundsCheck`
  took only an index, leaving a backend to invent the bound. It is now
  `Instruction::BoundsCheck { index, length, code }`, and the verifier checks that
  both are unsigned integers of the same width.
- **An array literal and an array repeat lost their contents.** Step 61 recorded
  them as a `CheckedExpr::Unit`, discarding the elements, the value and the count,
  so lowering had nothing to store. Both are now `CheckedExpr::Array` and
  `CheckedExpr::ArrayRepeat` with their contents intact.
- **`&text` for a `str` produced a unit value.** A `str` borrow is a `str`, so
  Step 61 returned `Unit` and threw the value away. It is now a read of the
  borrowed place.
- **A value-producing `if` never compiled.** With no type known for the
  conditional, the first arm was checked as a unit block, so every value
  conditional was rejected with "expected `()`, found `i32`". The first arm's
  tail now decides the type and the other arms are checked against it.
- **A nested block's tail was treated as the function's return value.** A loop
  body ending in `break` with a unit tail emitted an instruction after a
  terminator. Only a function body's tail is a return.
- **`ModuleBuilder::set_function_span` did nothing.** Every function in a module
  reported no span. It now records the span for the next function started, and
  takes it, so a span cannot leak from one function to the next.
- **The verifier did not check a load's width.** A load may be narrower than its
  type, which is how a conversion is spelled, but never wider, which would read
  bytes the value does not have.

### Decisions

- **A 32-bit target is refused.** `i64`, `u64` and `usize` are wider than a
  32-bit machine's registers; lowering them honestly means register pairs and
  arithmetic the ISA does not have. Step 62 reports that instead of miscompiling,
  and LZ64 is the default target.
- **A call is limited to six argument words.** The kernel reads a syscall number
  from `r0` and arguments from `r1`–`r6`, and a view or a 64-bit integer is two
  words, so three views already fill the registers. A wider call is refused by
  name with its word count.
- **A syscall the ABI has not numbered cannot be called.** `display_open`,
  `display_present` and `input_poll` are accepted as declarations because the
  designs name them, but a call to one is refused rather than given an invented
  number. This is what the graphics and input steps will have to add.
- **An array is its own storage, never a value.** `[T; N]` is a `Record` of `N`
  fields in the IR, initialised element by element. `as_slice` and `as_ptr` take
  its address, which is the one place array data is named rather than copied.
- **A place through a view reference is refused.** `*r` where `r: &mut [T]` has no
  single address to compute, because the reference *is* the view. The error names
  the shape rather than guessing at an address.

### Limitations

- There is no constant folding, no copy propagation, and no dead-store removal:
  every `let` is a store and every read is a load, even for a literal. Step 63 may
  do this in registers, and a later step may do it here.
- Frame addresses are recomputed per access (`FrameBase`, a constant, an add)
  rather than kept in a register, and the frame base is re-materialised for each
  one. A register-allocating backend will hoist these; correctness does not
  depend on it.
- The cast scratch slot and the short-circuit temporary are one slot per function,
  reused because each use is a store immediately followed by its own load or by
  the join. That reuse is sound only because nothing else can write them in
  between, which is why they are not shared across nesting.
- A loop's induction variable is the frontend's slot for the loop, re-read at each
  step, so a body that assigns to it is respected. Rejecting that assignment is
  the frontend's business, not this stage's.
- `aarch64-linux` remains untested.

## Step 63 — Code Generation

`crates/lazalith-codegen` turns a verified `lazalith_ir::Module` plus its frame
layouts into a `.lzo` object through the existing object model: `ObjectBuilder`,
`Section`, `Symbol`, `Relocation`, `DebugSource` and `CodeMapping`. Instructions
are encoded with `lazalith_isa::encode` — the assembler encoder — and syscalls
are resolved through `lazalith_os_abi::Syscall`, the same table the kernel uses.
Neither the encoding nor the ABI is restated in the new crate.

### Representation decisions

- **No register allocator.** Every IR value lives in a frame slot and each
  instruction loads its operands and stores its result. This is not a
  simplification for its own sake: it is what makes a 32-bit value's arithmetic
  32-bit arithmetic with no masking instruction, because a value is re-loaded at
  its declared width before every use and the store truncates to it.
- **Three caller-saved scratch registers and nothing else.** `r7` holds a frame
  address, `r6` and `r5` hold operand words, `r4` takes a result. No callee-saved
  register is touched, so nothing has to be spilled around a call and the prologue
  and epilogue are three instructions each.
- **A branch names a label, never a block number.** A branch carries a
  displacement and the linker fills it in from a relocation against a symbol, so
  no branch needs its target's address. A loop's `continue` and `break` are
  therefore label names, which is what lets a loop whose body contains an `if`
  work at all.
- **A comparison is a branch over a constant.** The ISA has no instruction that
  writes a condition into a register — `CMP` sets NZCV and `BR` reads it — so a
  `bool` result is materialised by storing `1` on the taken side and `0` on the
  other, with the value stored rather than left in the flags.
- **A data address is a `LI` with a relocation.** The segment's address is the
  linker's to decide, so the immediate is a hole for it to fill.
- **The first bytes of every frame are the outgoing argument words.** A `CALL`
  pushes the return PC at `oldSP-8`, so the convention's `[SP+8]` and `[SP+16]`
  at callee entry are the caller's own `[SP+0]` and `[SP+8]`. The caller owns
  them, so every frame reserves sixteen bytes below its first local.
- **A declared extern is one `TRAP 0`.** It has no body and its symbol is defined
  elsewhere, so anything that reached it would fail loudly rather than run
  whatever the linker placed next.

### Bugs this step found by running the code

Every one of these passed Step 62's tests, because each was invisible in the shape
of the IR and only appeared when the generated code ran on the machine.

- **Block identifiers were predicted, and the prediction was wrong.** The lowering
  counted the blocks a loop or a conditional would need and used those numbers as
  branch targets. A body that creates blocks of its own — a nested `if`, or a
  short-circuiting `&&` — shifts every later number, so a `continue` inside a
  nested `if` jumped to a block that was not the loop's step. The worst case was
  silent: `for index in 0..limit { if index == 2 { continue; } }` lowered to a
  block that jumped to itself, an infinite loop the verifier accepted. The IR
  builder now has `reserve_block`, which creates a block, hands back its real
  identifier, and leaves it out of the finished function's block list until the
  front end fills it in — so identifiers are real and the blocks are still in
  emission order, which is what numbers a function's values.
- **A `for` loop never ran its body.** The loop's test asks whether the counter
  has reached the bound, and the lowering branched *into* the body when it had.
  A non-empty range ran zero times and an empty one never stopped. Both directions
  produce a well-formed `Branch`, so no test that inspected the shape could tell.
- **A `for` loop's induction variable had no frame space.** It was allocated and
  then never counted, leaving `frame_size` a whole word short — so the loop's
  first temporary, which holds the end value, landed on top of the loop variable.
  The counter and the bound were the same four bytes.
- **A `let` inside a `while` body was not a local of the function.** The body was
  checked into a throwaway list of locals that was then discarded, so no slot was
  reported for it and the frame was too small to hold it. `while` now writes the
  body's locals and offset back, as `for`, `loop` and a nested block already did.
- **A `ptr<T>` local had no width.** The lowering's width table covered the
  integer types only, so every program that named a pointer local failed to
  lower — which is how the documented `write` example, whose buffer is a
  `ptr<u8>`, could not be lowered at all.
- **The documented `write` example corrupted its own message.** `result` is
  where the call leaves its 16-byte `IoResult`, and the hello-world example passed
  the message's own address as that pointer, so the kernel wrote the byte count
  over the message. The example now uses a 16-byte array of its own, and the
  reason is stated where the example is.
- **A function's `frame_size` did not include the outgoing argument words,** so a
  call with five or more argument words would have overwritten the caller's first
  local. Step 63 fixes this in the backend rather than in the frontend: the
  reserve belongs to the calling convention, and the frontend does not know it.

### Tests

22 tests in `crates/lazalith-codegen/tests/codegen.rs`. Most run the generated
code: a hand-written assembly harness calls a generated function, the object is
linked with the real linker, and the image is loaded and stepped by the real
machine and kernel, so a return value is observed as a process exit code and a
write is observed as console output. The harness is hand-written assembly because
the runtime that would call a program's `main` is Step 64's work; the tests say so
rather than depending on it.

The properties under test are the ones Step 62 established and a backend could
plausibly break: a local is reached through the stack pointer; a view keeps its
address *and* its length; a comparison becomes a real comparison and a real
`0`/`1` value; `&&` does not evaluate its right side when the left decides;
`break` and `continue` reach real code; an out-of-range index traps; a string's
address is a relocation; a 32-bit value wraps at 32 bits; a call passes and
returns through the documented convention; and every emitted instruction decodes
back to the opcode that was meant.

### Limitations

- The code is large and slow. Every frame access is `GETSP`, an add and a load,
  every `let` is a store and every read is a load, and a loop iteration is over a
  hundred instructions. Correctness does not depend on any of that, and a later
  step can hoist it.
- `Intrinsic::FunctionAddress` is refused: the instruction carries no function to
  name, so there is nothing to resolve, and answering with the current function's
  address would answer a different question. Nothing in Lazen v1 produces it yet.
- A syscall argument the ABI has not numbered, a store of a whole view, a load
  from the platform space, a 32-bit target, and a return value of more than one
  word are each refused with a typed error rather than approximated.
- More than one source file in one module is refused: the object model carries one
  debug source, and describing two would mean picking one.
- The frontend checks a syscall's name and arity but not its argument *kinds*, so
  a declaration with the right arity and the wrong types is accepted and fails in
  the ABI instead. That belongs to the frontend and is not fixed here.
- `aarch64-linux` remains untested.

## Next step

Step 64 is the runtime: the program image loader and the entry path that calls a
Lazen program's `main`. Everything it needs now exists — a `.lzx` image, a linker,
a frame size per function reported by `Program::frame`, and generated code whose
entry symbol is `fn.<name>`. The one thing it must not do is re-decide anything
the codegen already decided, including the sixteen bytes every frame reserves for
outgoing arguments.

## Steps 1–50 Retrospective Audit and Repair

A read-only audit of every step from 25 through 50 was performed against
`instruction.md`, the implementation, the tests, the Nix configuration, and the
documentation, followed by a repair pass. The audit found no P1 defect and no
invariant violation: the implementation is Rust-only with `unsafe_code =
forbid`, has no global mutable state, keeps LZ32 and LZ64 first-class, uses
strong domain types, and keeps the Reference Interpreter, real Bus ownership,
and machine-owned hardware state intact. Every confirmed finding below was a
missing check, an unenforced contract, stale documentation, or an untested
invariant. The historical entries below are preserved as written; this section
records the corrections.

Repaired defects and their regression tests:

- **Scheduler activation failure never poisoned.** `activate_next` returned a
  machine error while every other machine-error path poisoned the scheduler, so
  the documented `reset_after_poison` recovery was unreachable and the
  scheduler silently accepted processes it could never run. Both activation
  failure paths now poison. Regression test:
  `scheduler_poisons_on_activation_failure_and_recovers_after_reset`.
- **Poisoning destroyed the terminal machine state.**
  `recover_user_context`/`invalidate_user_context` overwrote `Faulted` with
  `Halted` and discarded `clear_execution_context`'s result, so `is_halted()`
  reported a faulted machine as merely halted. Both now check the result and
  preserve `Faulted`. Regression tests:
  `scheduler_preserves_the_faulted_machine_state_while_poisoned`,
  `scheduler_release_returns_the_machine_to_halted`.
- **VFS read access was unenforced in the abstraction.** `read_at` now takes the
  handle access and rejects a read-less handle. Regression tests:
  `file_access_is_enforced_by_the_filesystem_for_reads_and_writes` for the
  filesystem itself and `a_write_only_handle_cannot_read_through_the_dispatcher`
  for the syscall path; `stale_foreign_and_closed_handles_are_rejected_end_to_end`
  covers stale and foreign handle indices.
- **Single-stepping never delivered interrupts.** Delivery required the
  `Running` state, which `step` never enters, so a debugger driving `step`
  silently never serviced a pending enabled interrupt. Eligibility is now the
  shared executable-state set (`Reset`, `Running`, `Paused`). Regression tests:
  `interrupts_deliver_at_every_executable_boundary_lowest_first_and_defer_in_frame`,
  `interrupt_delivery_requires_an_enabled_interrupt_at_an_executable_boundary`.
- **The syscall completion path could panic.** `unreachable!()` guarded a
  status-range check; an out-of-range status now produces
  `DispatchOutcome::Fault(SyscallError::Internal)`. Regression test:
  `every_syscall_error_status_fits_the_one_shot_completion_range`, which pins
  every `SyscallError` inside the one-shot completion range.
- **The host service API could wedge the scheduler.**
  `UserMemoryContext::exit` and `transition(Ready)` on the running process
  produced a `ProcessStateMismatch` poison or a double trap. `exit` now requires
  a dispatched syscall context and `Running -> Ready` is rejected through the
  context; `Blocked` remains available to services and hosts. Regression tests:
  `scheduler_rejects_host_termination_of_the_running_process`,
  `process_context_lends_owned_memory_and_handles_only_to_its_process`.
- **Stale bindings were detected but unproven.** The binding checks in
  `validate_active_binding` and the dispatcher survived guard removal in
  mutation testing. `ContextMismatch` also reported two equal identifiers; it
  now reports the side that actually diverged. Regression test:
  `scheduler_detects_a_stale_execution_context_binding`.
- **Blocked-process resume was unverified.** The saved CPU context on suspend
  could be dropped with every test still green. Regression test:
  `scheduler_resumes_a_blocked_process_with_its_own_cpu_context`, plus
  `scheduler_unblock_enforces_its_preconditions_and_keeps_the_cursor` and
  `scheduler_reaping_rejects_non_terminal_and_keeps_rotation_ordered`.
- **`.lzo` was never serialized on the product path.** The linker consumed the
  in-memory model, so the Step 49 chain had no `.lzo` stage. The pipeline is now
  proven end to end through the real serialized object in both modes:
  `the_serialized_lzo_format_is_the_real_linker_input_in_both_modes`.
- **`.lzo` canonicality and validation were largely untested.** The string
  table now rejects unreferenced trailing bytes, and rejection tests cover
  reserved words, table offsets, payload EOF, duplicate section and symbol
  names, and relocation ordering. Tests:
  `lzo_rejects_non_canonical_tables_reserved_words_and_counts`,
  `lzo_rejects_duplicate_names_and_out_of_order_relocations`.
- **Relocation values were unverified.** Every relocation kind is now decoded
  and compared against the documented formula in both widths:
  `linker_writes_the_documented_value_for_every_relocation_kind` and
  `linker_rejects_relocations_it_cannot_represent`.
- **`.lzx` rejection coverage was ineffective.** The truncation loop discarded
  its results; it now asserts every prefix and trailing extension is rejected,
  and a new test pins thirteen previously untested `LzxError` variants:
  `lzx_rejects_every_unreachable_header_and_table_field`.
- **The disassembler round trip covered one form of forty** and located
  sections by pointer identity. Lookup is now index-based, and every mnemonic
  printed form is re-assembled and compared byte for byte in both widths:
  `every_printed_instruction_reassembles_to_identical_bytes`.
- **Assembler defects:** an undefined `.global` name was silently dropped and
  now becomes an undefined global symbol that fails at link time; `map_error`
  discarded its cause and now preserves it in the diagnostic. The first is
  covered by `a_global_name_that_is_never_defined_becomes_an_undefined_symbol`.
  The second is defensive: its only reachable inputs are allocation failures
  from `try_reserve`, which no public source can provoke, so it is recorded as a
  repaired code path without a dedicated test. The section-alignment padding
  path now grows the buffer through `try_reserve` instead of `Vec::resize`.
- **Pool accounting overstated capacity.** `BumpPool::Exhausted.remaining` now
  reports the bytes usable from the aligned cursor, so alignment padding is not
  counted as allocatable. Test:
  `bump_pool_reports_alignment_aware_remaining_capacity`.
- **Boot constants were dead and duplicated.** `RESET_VECTOR`,
  `BOOT_ADDRESS`, and `BOOT_ROM_PHYSICAL_START` now bind the machine setup, and
  the duplicate kernel-stack constants were removed from the boot crate. Test:
  `the_documented_reset_vector_is_the_single_boot_source_of_truth`.
- **Machine diagnostics fell back to a debug dump.** Every `MachineError`
  variant now has an intentional message.
- **`FileMetadata.permissions` implied a security guarantee it did not
  provide.** It is now `capabilities`, documented as the node capability class;
  per-handle `FileAccess` remains the enforced grant.
- **The two `lazos$` shells were an undocumented pair of specifications.**
  The host `HeadlessShell` grammar is now declared authoritative and the
  in-image shell is declared a conformance subset; the divergence is pinned by
  `the_two_shells_declare_one_conformance_contract`.
- **The pending-run latch was opaque.** `ShellError::PendingRun` now carries the
  queued path so a host can report which program must be cleared.

Documentation corrections: `docs/os-design.md` (interrupt boundary on `step`,
primary-thread-only dispatch, `NotSupported` services, dormant-space
`memory_context` safety, unreachable User MMIO, shell conformance),
`docs/os-memory.md` (four-byte instruction alignment, Step 34 ownership),
`docs/os-abi.md` (`MAX_PATH_BYTES` terminator rule, `IoResult.status`
semantics, `FileStat` capability semantics), `docs/boot.md` (HALT terminality
for execution, reset-vector source of truth, validation ordering),
`docs/lzx.md` and `docs/lzo.md` (debug-mapping drop, string-table coverage), and
`docs/lazen-rationale.md` (explicit time service).

Known limitations that remain by design: `aarch64-linux` is untested; in-image
`SpawnProcess`/`WaitProcess` and the in-image `run` command are unimplemented;
the guest shell's `ls`/`cat` are fixed-path; `time`, `sleep`, and
`AllocateMemory` are validated by the dispatcher but return `NotSupported` from
the shipped service; `.lzx` v1 carries no debug metadata; `PcRelativeWord32` has
no ISA form that consumes it from data.

## Step 50 — Lazen Language Design

Added `docs/lazen-design.md` and `docs/lazen-rationale.md`. The design defines a
small native systems language with explicit LZ32/LZ64 targets, `main` entry
mapping, checked regions and pointers, typed OS wrappers, deterministic
semantics, and a compiler pipeline into the shared `.lzo` toolchain. The
rationale answers what would make building native Lazalith applications easy
while explicitly rejecting wholesale C/Rust/Pascal/Go conventions. No Lazen
compiler is claimed or started; grammar details remain future work.

## Step 49 — Real Assembly Program Under LazOS

Completed the bounded headless milestone. The headless shell's `run` command now
validates a VFS path, reads the actual `.lzx` bytes, parses them, and schedules
the resulting process through `LazalithKernel::start_image`. The integration
fixture assembles and links a real LZ64 program, round-trips the executable,
boots through ROM and Supervisor RFE, executes `Write(1)` into the virtual
terminal, and observes `Exit(0)`. The in-image native shell still reports
in-image `run` as deferred because `SpawnProcess`/`WaitProcess` services are
future work; the host shell path is the documented bounded claim.

## Step 48 — Relocatable Linker

Implemented multi-object compatibility checks, global/local symbol resolution,
aligned parallel section layout, runtime code/data address bases, all six
relocation evaluations, canonical instruction patching, BSS placement, and
`.lzx` v1 emission including BSS-only images. Linker tests cover cross-object
symbols, `BR`/`CALL`, memory and data relocations, alignment, BSS-only output,
incompatible targets, and executable round trips. The existing single-text
bridge remains the bounded Step 44 compatibility path.

## Step 47 — Shared-ISA Disassembler

Implemented canonical decode/encode verification and text formatting for the
shared ISA, object text-section disassembly, comma-separated re-assemblable
output, and structured rejection of incomplete or noncanonical bytes.

## Step 46 — Source-Spanned Assembler

Replaced the Step 44 line shim with a bounded source lexer, parser, semantic
checks, metadata-driven operand handling, section accumulators, symbol
resolution, data directives, debug mappings, and all six relocation emitters.
Supported directives include `.arch`, `.entry`, `.section`, `.global`,
`.extern`, `.equ`, `.align`, `.zero`, `.ascii`, `.asciz`, `.byte`, `.half`,
`.word`, `.dword`, and `.pcrelword`; every ISA mnemonic and operand form is
parsed through `lazalith-isa`. Source errors retain typed diagnostics, spans,
and managers, with regression coverage for malformed memory operands, negative
symbols, ordering, alignment, dword width, BSS, and unknown symbols.

Full workspace Rust formatting, strict Clippy, check, and all-target tests pass
with 334 tests and zero doctests after the retrospective repair pass. Current
Rust source/test line count is 39,404 at that milestone; the workspace now
holds 352 tests and 41,839 lines after Steps 51-60, whose build output was
`/nix/store/b5wqs8lpgjh0sv24vvc23jlvzlr81l38-lazalith-foundations-0.1.0`.
After Step 61 the workspace holds 533 tests and 53,060 lines, with build output
`/nix/store/69dg0yxw0a7gxab0a282azgpnaafg2rg-lazalith-foundations-0.1.0`.
After Step 62 the workspace holds 558 tests and 56,309 lines, with build output
`/nix/store/k8a4zdffwyl9fnra59qw5kyhry01mlf2-lazalith-foundations-0.1.0`.
After Step 63 the workspace holds 588 tests and 60,077 lines, with build output
`/nix/store/y1s0r23gic6wjk7lpb2v7v27b5247ycx-lazalith-foundations-0.1.0`.
The Nix build at the end of the Steps 26-50 repair pass was
`/nix/store/mkvdplx6wsyb28878jlpbakjk7nvdsfa-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. The Steps 26–50 audit repaired linker BSS
alignment accounting, made syscall-admission identity structurally
non-cloneable, hardened assembler expression-range arithmetic with source-located
diagnostics, and corrected stale LazOS `run`/toolchain/ABI wording in
`docs/os-design.md`. Independent review found no remaining P0/P1 defects for the
bounded scopes; no staging or commit was created.

## Step 45 — Native `.lzo` Object Format

Added the typed relocatable `.lzo` v1 model and canonical little-endian codec to
`lazalith-toolchain`. `ObjectTarget`, `Section`, `Symbol`, `Relocation`,
`DebugSource`, `CodeMapping`, `ObjectBuilder`, and `ObjectFile` cover
architecture/ISA/ABI metadata, four section kinds, symbol bindings and entries,
six relocation kinds, and source mappings. The format has checked canonical table
boundaries, zero reserved fields, bounded payloads/name materialization,
canonical instruction validation, strict BSS and payload rules, and no runtime
kernel dependency. `docs/lzo.md` records the wire contract; `.lzx` v1 remains
fixed and independent.

Round-trip, header-offset, padding, repeated-name, exhaustive-truncation,
malformed-header, BSS-byte, and relocation-rejection tests pass alongside the
Step 44 LZ32/LZ64 boot path. Full workspace Rust formatting, strict Clippy,
check, and all-target tests pass with 291 tests and zero doctests. Current Rust
source/test line count is 34,856. Path-based Nix flake checks and package build
pass with output
`/nix/store/in4h1rspwf223arwqg04vizkfissicai-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. Independent review found no remaining P0/P1/P2
defects; no staging or commit was created. Step 46 is next.

## Step 44 — First End-to-End Assembly Program

Added the `lazalith-toolchain` crate with a deliberately bounded Step 44
assembly surface: `.arch`, `.entry`, standalone labels, `LI`, and `SYSCALL`.
Successful source is encoded through the shared canonical ISA codec into a
versioned in-memory `ObjectFile`, then linked as one code-only section into the
existing validated `.lzx` v1 container. Source failures retain a typed
`Diagnostic`, `SourceSpan`, and `SourceManager`; object validation rejects empty,
misaligned, unsupported, undecodable, and noncanonical code before publication.

The boot integration test covers both LZ32 and LZ64 through
`assembly -> object -> executable bytes -> reparsed LZX -> Process -> LazOS
scheduler -> Exit(0)`, and matches the existing init image byte-for-byte. This
is intentionally not the complete Step 45 `.lzo` format, full Step 46 assembler,
multi-object linker, shell `run`, or packaged guest kernel. Workspace Rust
formatting, strict Clippy, check, and all-target tests pass with 286 tests and
zero doctests. Current Rust source/test line count is 32,578. Path-based Nix
flake checks and package build pass with output
`/nix/store/wpk7vj2b82fzrvfvqiqpjhwzqs7885ia-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. Independent review found no remaining P0/P1/P2
defects for the bounded Step 44 scope; no staging or commit was created. Step
45 is next.

## Step 43 — Native Composite Shell Fixture

Implemented both the bounded `HeadlessShell` model and a real composite
init/shell `.lzx` fixture. `build_init_shell_image` emits typed User code/data/
BSS, descriptor-aware `Read(0)`/`Write(1)`, bounded line framing, `help`, `echo`,
`ls`, `cat`, `clear`, and an explicitly deferred `run`. `TerminalService` owns
bounded scripted input/output, rejects overlong lines before fragmentation, and
tracks clear state; `IoHandle` reserves 0/1 while file handles start at 2, and
`LazalithKernel` owns the scheduler/service/image-start loop with post-return
scheduler metadata. The boot test runs the ROM handoff, Supervisor RFE
trampoline, native User shell, returning syscalls, EOF exit, fatal-error status,
and process release in both LZ32 and LZ64.

This is a native composite fixture, not a packaged guest-kernel image or
SpawnProcess/WaitProcess implementation. `run`, arbitrary shell paths, a real
device-backed terminal, and a separate init-to-child launch remain future
scope; the full Step 43 acceptance criterion is therefore not claimed. Full
workspace Rust formatting, strict Clippy, check, and all-target tests pass with
282 tests and zero doctests. Current Rust source/test line count is 31,765.
Path-based Nix flake checks and package build pass with output
`/nix/store/dzhhirwpfy3x01mgkqkpjrmxvn1vmndj-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. Independent review found no remaining P0/P1/P2
defects for the documented bounded scope; no staging or commit was created. Step
44 is next.

## Step 42 — Userspace Init

Added the typed `build_init_image` constructor and exported its fixed metadata.
It emits a native `.lzx` v1 image for LZ32/LZ64 containing `LI r0, 1` followed
by `SYSCALL`, with a 16-byte code section, fixed User stack requirement, and
entry offset zero. Added canonical ISA decode/round-trip/process ownership tests
and a bootloader-to-scheduler integration test that executes the real User
image, observes the syscall trap, dispatches a typed Exit service, and verifies
non-returning process release. This is a minimal init milestone, not a packaged
guest kernel; production kernel orchestration remains later work. Workspace Rust
formatting, strict Clippy, check, and all-target tests pass with 270 tests and
zero doctests. Current Rust source/test line count is 28,626. Path-based Nix
flake checks and package build pass with output
`/nix/store/k511ivnrhm5radb4rpm7cg5kwgkhp49c-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. Independent Step 42 review found no remaining
P0/P1/P2 defects. No staging or commit was created. Step 43 (userspace shell)
is next.

## Step 41 — Virtual Filesystem

Implemented `VirtualFileSystem`, an owned in-memory backend with explicit node,
file-size, directory-entry, and path limits. It supports absolute byte paths,
regular files/directories, deterministic sorted directory records, checked
create/truncate/open modes, short EOF reads, checked writes, signed seek origins,
metadata, and fallible allocation. `ProcessHandles` now owns monotonic file
handles with node/access/offset state, rejects stale or foreign handles, and
uses reservation-before-commit so failed opens do not consume handle IDs.
`FileSystemService` adapts the existing validated ABI to checked User-memory
buffers and structured statuses without unsafe code or global mutable state.
The backend, process-handle, and syscall adapter tests pass. Workspace Rust
formatting, strict Clippy, check, and all-target tests pass with 268 tests and
zero doctests. Current Rust source/test line count is 28,395. Path-based Nix
flake checks and package build pass with output
`/nix/store/bx7mff7k9fwsbqbg8b5fs65d405rwhk2-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. Independent Step 41 review found no remaining
P0/P1/P2 defects. No staging or commit was created. Step 42 (userspace init)
is next.

## Step 40 — Native `.lzx` Program Loader

Designed and documented the native `.lzx` v1 container in `docs/lzx.md` and
implemented its bounded little-endian parser, typed section model, builder, and
atomic process loader in `lazalith-os`. The format validates magic, format/ISA/
ABI versions, LZ32/LZ64 architecture, header flags, section count/order,
permissions, file and virtual ranges, entry alignment and instruction bounds,
memory requirements, and section overlap before constructing an unpublished
`Process`. Code, initialized data, and zero-filled BSS load into process-owned
User memory; malformed input cannot publish a partial process. v1 intentionally
has no compression, relocation, symbol, or debug extensions.

Five `.lzx` integration tests cover valid LZ32/LZ64 round trips, data/BSS
loading, empty data sections, stable header/section layout, exhaustive
truncation handling, malformed headers, and semantic section/requirement
rejection. Workspace Rust gates pass with 259 tests, zero doctests. Current
Rust source/test line count is 26,724. Full workspace formatting, strict
Clippy, check, all-target tests, Nix flake checks, and package build pass. The
verified package output is
`/nix/store/b9v3463gx1ya17q7q7c88rd0diff1lvh-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created.

## Step 39 — Deterministic Round-Robin Scheduler

Implemented `RoundRobinScheduler` in `lazalith-os`. It owns the process table,
cursor, nonzero quantum, unique `ExecutionContextId` tokens, stable logical
process identities, and an optional aggregate User-space admission budget; the
normal constructor budgets two logical process layouts, while explicit
unbounded/test construction remains available. Duplicate or pre-bound process
IDs, incompatible architectures, invalid lifecycle states, poisoned schedulers,
and exhausted budgets reject before insertion. Terminal processes can be reaped
to reclaim their resident-memory accounting.

The scheduler activates only validated User contexts. Machine-side activation
checks architecture, User privilege, switchable lifecycle state, no active
frame, and no already-active token. Context switches use a typed, fallible
User-region transfer that pre-reserves all storage and rejects cross-space
overlap, retaining machine-owned Supervisor regions. At a safe boundary the
scheduler saves the complete User CPU state, returns the live User space to its
process, validates lifecycle/token/identity state, and selects the next ready
process. `with_active_memory_context` and `dispatch_syscall` derive service
bindings from the scheduler's active process, token, thread, and restricted
User-space capability; raw live address-space and admission access are not
exposed.

A User `SYSCALL` or software trap consumes one quantum at trap entry. Framed
handler instructions, external-interrupt delivery, and `RFE` itself do not
consume User quantum; a pending yield is applied after a frame-free return.
Synchronous faults abort their frame and transition only the affected process to
`Faulted`. Blocked processes are suspended before another process runs and can
be explicitly unblocked. Non-returning `Exit` and dispatcher faults are cleaned
up without a returning completion, including services that mutate exit state
before returning. A terminal machine error poisons the scheduler and requires
coordinated machine reset; recovery refuses to clear poisoning if User-space
ownership cannot be restored. Round-robin has no priorities or SMP.

Twenty-two scheduler integration tests cover validation/budgets/reaping,
multiple-user execution, quantum and trap accounting, handler-frame boundaries,
restricted active service memory, syscall capture/completion, blocked
reconciliation, non-returning cleanup, terminal-error poisoning/reset,
privilege-return rejection, run continuation/counting, and out-of-image
architectural control state. Workspace Rust gates pass with 254 tests, zero doctests. Current Rust
source/test line count is 24,983. Full workspace
formatting, strict Clippy, check, all-target tests, Nix flake checks, and package
build passed on x86_64-linux. Package output:
`/nix/store/3fddkl6ywvmnqnbsn1pddkrc3c2srf0v-lazalith-foundations-0.1.0`. aarch64-linux remains
untested. No staging or commit was created. Step 40 (`.lzx` program loader) is
next.

## Step 38 — Processes and Threads

Implemented explicit `ProcessId`, `ThreadId`, `Process`, and `Thread` aggregates
in `lazalith-os`. A process owns an independent `UserMemory` address space and
allocator, a copied and bounded `ProgramImage`, its `StackRegion`, typed
`ProcessHandles`, lifecycle state/exit code, and one or more thread CPU states.
`ProgramImage` validates non-empty input, architecture, entry alignment and
bounds, instruction-sized entry, address overflow, and code-region capacity
before loading. Threads start in User mode with interrupts disabled; additional
threads use `Thread::for_process`, which validates the image entry, stack
membership, mapped User RAM, and architecture before construction, and
`Process::attach_thread` records ownership while rejecting duplicate or foreign
threads.

Process states are `Created`, `Ready`, `Running`, `Blocked`, `Exited`, and
`Faulted`. Transition validation is atomic and terminal states cannot be
resurrected. `Process::memory_context` and `memory_context_for_thread` are the
only constructors for the service context. The context binds process ID, thread
ID, active scheduler execution token, and address-space identity and lends the
process-owned address space, allocator, handles, active thread CPU state,
image/stack metadata, and lifecycle/exit fields. A process must be `Running` and
explicitly activated with the token held by the machine trap controller before
syscall admission. Syscall requests cannot be rebound after admission, and the
dispatcher rejects any mismatched process, thread, execution token, lifecycle
state, or address-space identity. Successful `Exit` dispatch atomically records
`Exited` and its code.

Eleven Step 38 integration tests cover IDs, image ownership/validation, stack
and entry rejection, handle tables, lifecycle transitions, CPU/privilege setup,
address-space isolation, complete process service context, explicit execution
activation, thread attachment, and ownership transfer. Ten Step 37 syscall tests
remain meaningful and now use the mandatory process context and execution token.
Workspace Rust gates pass with 232 tests, zero doctests. Current Rust
source/test line count is 21,885. Full workspace
formatting, strict Clippy, check, all-target tests, Nix flake checks, and
package build passed on x86_64-linux. Generic CPU `RFE` remains available for
ordinary software/interrupt traps, while syscall-frame `RFE` requires the exact
completion authorization. Package output:
`/nix/store/nggysgwk3x7h93z2sr6pipq9543anlbn-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created. The verified
Step 39 handoff is recorded above; Step 40 is next.

## Step 37 — Syscall Dispatcher

Implemented the trap-admitted `SyscallDispatcher` in `lazalith-os` and
integrated it with the existing CPU/machine trap path. `SyscallRequest` is
created from an active `TrapCause::Syscall` frame, captures the pre-entry
register snapshot, validates Supervisor/TVEC/resume-control state, and consumes
a controller-generation admission exactly once. Trap controllers are no longer
cloneable, and requests carry a stable controller identity so a request or
completion cannot cross machine generations.

The dispatcher consumes the one-shot request, binds it to one exclusive
`UserMemoryContext`, and validates the full v1 contract before invoking an
injected `KernelService`. It covers all fourteen calls, required-zero versus
ignored arguments, full-word LZ32/LZ64 values, handles/flags/origins, scalar and
signed words, output alignment/size, complete non-crossing User ranges,
permissions, path/argv termination, byte-capacity records, and the frozen input
budgets (`MAX_PATH_BYTES`, `MAX_ARGUMENT_COUNT`, `MAX_ARGUMENT_BYTES`, and
`MAX_ARGUMENT_TOTAL_BYTES`). Zero-length transfers are explicit no-dereference
operations. `AddressSpaceIdentity` is a stable per-space identity rather than a
host address assertion.

Returning calls receive an opaque `ValidatedSyscallKind` view and produce a
one-shot typed completion containing only valid ABI `u32` status/payload values.
`LazalithMachine::return_from_syscall` requires that completion, an active
admitted frame, a live matching controller, Supervisor state, and an actual
`RFE` instruction before writing `r0`/`r1` and returning. Generic CPU `RFE`
remains unchanged for software/interrupt traps. `Exit` and dispatcher faults
have no returning completion. No default service or concrete filesystem/process
service was faked; those remain later work.

Eight new OS syscall integration tests cover real User `SYSCALL` trap entry,
trap admission/replay rejection, controller and address-space binding,
required-zero/identity checks, all fourteen typed calls, pointer/path/capacity
and aggregate-string rejection, service return-kind enforcement, and checked
RFE completion. Workspace Rust gates pass with 219 tests, zero doctests. Full workspace
formatting, strict Clippy, check, all-target tests, Nix flake checks, and package
build passed on x86_64-linux. Package output:
`/nix/store/8j839labqpghpvm3j17hw821xk91986f-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created. The verified
Step 38 and Step 39 handoffs are recorded above; Step 40 is next.

## Step 36 — Shared OS ABI Crate

Created and Nix-installed the no_std `lazalith-os-abi` workspace crate and made
`lazalith-os` consume it through the canonical `lazalith_os::abi` re-export. The
kernel does not copy IDs, statuses, flags, records, or conversion rules. The
crate centralizes the fourteen frozen v1 syscall IDs, full-word LZ32/LZ64 ID
validation, used/required-zero/ignored argument metadata, six typed argument
slots, reserved `r7` validation, returning/non-returning metadata, and the
mode-independent `r0` status plus `r1` payload result.

All fixed little-endian v1 structures are materialized and checked:
`IoResult`, `FileStat`, 256-byte `DirectoryRecord`, `MemoryAllocation`, and
`ExitStatusRecord`. Handles, open flags, permissions, seek origins, and frozen
process-exit reasons are typed. Constructors and decoders reject wrong sizes,
reserved data, unknown values, out-of-width fields, and invalid address/length
ranges before exposing a value. Range checks use the inclusive final byte, accept
valid ranges through each architecture's maximum address, and independently
bound LZ32 lengths; they do not manufacture an unrepresentable exclusive end.

`AbiError` retains typed width/value causes internally and maps centrally to
the nineteen stable `SyscallError` values only at the ABI boundary. LZ32/LZ64
pointer, unsigned-word, signed-word, host-size, and record conversions have
coverage. The former packed-u64-in-`r0` proposal was removed after review proved
it impossible for LZ32. Required-zero fields are distinct from ignored trailing
arguments, so `Seek` retains its output pointer and `Exit` may ignore later
arguments as documented. A final independent review found no remaining Step 36
code or specification defect.

Ten ABI integration tests plus one kernel shared-definition test bring the
workspace to 211 tests, zero doctests. They cover every ID/status/error, complete
request metadata, return transport, required-zero and ignored arguments, r7,
both pointer/word modes, signed boundaries, terminal ranges, exact wire bytes,
reserved fields, record decoding, host-layout independence, and error causes.
Rust source/test line count is 20,062. Strict workspace all-target Clippy,
formatting, check, all-target tests, Nix flake
checks, and package build passed on x86_64-linux. Package output:
`/nix/store/d0l82y2yxc673ms17gw8sk8x3qjn2zlx-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No dispatcher or service behavior was faked,
and no staging or commit was created. At the end of Step 36, the dispatcher was
next; the verified Step 37 handoff is recorded above.

## Step 35 — System Call ABI Design

Created and Nix-packaged `docs/os-abi.md`, freezing ABI version 1 before shared
code creation. It defines custom stable syscall IDs, not Linux numbers:
Exit=0x0001, Write=0x0002, Read=0x0003, Open=0x0004, Close=0x0005,
Seek=0x0006, Stat=0x0007, ListDirectory=0x0008, Time=0x0009, Sleep=0x000a,
AllocateMemory=0x000b, SpawnProcess=0x000c, WaitProcess=0x000d, and
ClearScreen=0x000e. `0x0100..0xffff` is reserved; unknown/reserved IDs reject.

The register convention uses word-sized `r0` for the number and `r1..r6` for at
most six arguments, with `r7` zero on entry and `r7`/`r8..r15` preserved.
Returning services write a mode-independent `u32` status to `r0` and `u32`
payload to `r1`, zero-extended in both LZ32 and LZ64; Step 36 review rejected
the earlier impossible packed-u64-in-`r0` result. SP/NZCV are preserved. Wide
values use checked fixed-size output structures. The design fixes little-endian
layouts/sizes for `IoResult`, `FileStat`, 256-byte `DirectoryRecord`,
`MemoryAllocation`, and `ExitStatusRecord`, plus custom open flags,
handle/offset types, and a structured non-Linux error set.

Pointer validation is defined before side effects: address-space identity,
mode-width complete range, read/write permission, alignment, exact structure
size/reserved fields, scalar/path constraints, and variable-output capacity.
Debugger peek cannot authorize User memory. Short I/O alone may commit a
validated prefix; other failures are atomic. `Sleep` uses virtual cycles,
process launch consumes the future shared `.lzx` model, directory records are
deterministic, and ClearScreen is a virtual-terminal operation. Display/graphics,
signals, networking, environment mutation, and wall-clock syscalls are
explicitly deferred.

No Rust API or runtime dispatcher was added because Step 35 is design-only; all
200 tests remain meaningful. Manual review checked every roadmap-named service,
future Step 41–43 requirements, register preservation, LZ32/LZ64 layout,
reserved numbering, structured errors, and Linux-divergence constraints. Full
Rust gates, host Nix flake checks, and package build passed on x86_64-linux.
Package output:
`/nix/store/azanmslryvz2q41dslwq63lpmrkp89q5-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created. At the end of
Step 35, materializing the single shared `lazalith-os-abi` crate was next; the
verified Step 36 handoff is recorded above.

## Step 34 — Basic Kernel and User Memory

Added `lazalith-os` (`no_std + alloc`; local memory/types dependencies only),
registered/installed it in Cargo/Nix, and implemented the minimum allocation and
layout substrate from `docs/os-memory.md`. `BumpPool` validates nonzero
power-of-two alignment, mode-width complete ranges, checked aligned extent, and
capacity before moving its cursor. Failure preserves the exact pool state.
Exact exhaustion returns `cursor() == None` rather than manufacturing an invalid
one-past address; no individual free or global allocator exists.

`StackRegion` validates complete mode range and alignment and requires the
initial SP to be mapped (`start <= SP < end`), fixing the prior one-past/out-of-
range ambiguity. `KernelMemory` owns the checked kernel heap and stack plus the
canonical kernel image/stack/heap `MemoryRegion` builders. `UserMemoryLayout`
owns the User heap/stack allocator, while each `UserMemory` owns exactly one
configuration-bound `AddressSpace`; `into_parts` transfers layout and space
together for future process ownership. Cross-config/repeated-space misuse and
mutable raw region/byte access are not exposed.

The mandatory physical RAM length is corrected to `0x310000` bytes
(`0x0010_0000..0x0040_ffff`, 3.0625 MiB), including the User stack. Boot setup
now extends the Step 28 ROM with the same centralized kernel and User region
builders, so every successful boot machine has the complete backed v1 map
before execution. This integration replaces the historical three-RAM-region
Step 28 setup; no allocator can return an unmapped kernel heap address.

Six OS integration tests cover low and exact LZ32 upper-bound pool exhaustion,
invalid alignment/zero/overflow atomicity, stack SP boundaries, complete physical
range and all six permissions, fresh zeroed backing, failed overlapping code-load
atomicity, real Bus/CPU User code/data/heap/CALL/RET, User stack NX in both modes,
and real Bus/CPU Supervisor kernel code/heap/CALL/RET. Workspace total: 200
tests, zero doctests.

Independent review found and corrected the physical endpoint, incomplete boot
backing, address-space/config ownership, one-past stack SP, exhausted cursor,
and direct-AddressSpace test gaps. Full required Rust gates, strict workspace
all-target Clippy, host Nix flake checks, and package build passed on
x86_64-linux. Package output:
`/nix/store/y12zlba5k2fyqx3h88l2zbcssa2ad210-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No SDL, external dependency, staging, or commit
was added. Rust source/test line count is 15,954. Step 35 (system-call ABI
design) is next.

## Step 33 — Flat OS Memory Model

Created and Nix-packaged `docs/os-memory.md`. It defines a v1 flat,
identity-mapped, region-permission memory model using the existing strong
address domains and explicit Bus translation boundary. The mandatory physical
RAM window is `0x0010_0000..0x0040_ffff` (`0x310000` bytes,
3.0625 MiB), partitioned into the Step 28
kernel image, a distinct Supervisor kernel stack/heap, and User code,
data/heap, and stack regions. Boot ROM remains outside RAM. Every range,
length, permission, and initial SP is explicit for both LZ32/LZ64.

The design assigns Supervisor R/W/X to the bootloader-writable kernel image,
Supervisor R/W to kernel stack/heap, User R/X to code, and User R/W to
data/heap/stack. Supervisor still receives no R/W/X bypass. User stack SP
`0x0040_f000` and kernel SP `0x0018_f000` are mode-valid and aligned; stacks grow
downward and have no guard pages. MMIO is excluded from allocation and requires
a later disjoint driver mapping.

Each process will own an independent `AddressSpace` with the same static layout;
whole active-space/CPU context replacement will occur only at scheduler-owned
boundaries. v1 has no shared writable pages, `Arc<Mutex<...>>`, or concurrent
address-space mutation. The current machine still owns one static space:
Steps 34, 38, and 39 will add pool metadata, process ownership, and explicit
context activation in that order.

Kernel and User heaps are checked contiguous bump allocators with fresh zeroed
RAM, aligned allocation, no individual free, and no cursor mutation on failure.
There is no active page size or paging. Four KiB is only the planned future
minimum page size; no `PageNumber`, page table, MMU, demand paging, shared
mapping, guard page, or global allocator is claimed. The document defines the
smallest complete Step 34 scope without implementing it early.

No Rust API or test was added because Step 33 is design-only; all 194 existing
tests remain meaningful. Manual cross-checking covered every Step 33 field,
boot-map compatibility, strong address separation, current `MemoryRegion`
capabilities, CALL/RET stack rules, and the later process boundary. Full Rust
gates, host Nix flake checks, and package build passed on x86_64-linux. Package
output:
`/nix/store/81dnc6sdvknljqz026sf56lzc3z75npl-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created. Step 34
(checked bump pools and kernel/User layouts) is next.

## Step 32 — Supervisor/User Privilege Contract

Completed and verified the minimum useful privilege model using the existing
centralized architecture rather than adding a duplicate OS-only enum. The ISA
has exactly `Supervisor` and `User`, represented by status bit `U`; `EI`, `DI`,
`HALT`, `RFE`, `CSRR`, and `CSRW` are Supervisor-only. User execution is rejected
before any instruction, memory, status, or controller effect. `SYSCALL` and
software `TRAP` remain legal in both modes and do not change privilege until
trap entry. Arithmetic changes only NZCV, never U or IE.

Trap entry from User forces Supervisor and clears IE while preserving NZCV, SP,
and all general registers. RFE restores only the frame's validated editable
PC/SP/status, including User and IE, and deliberately does not restore the
immutable general-register snapshot. Region-level `user` permission and
Supervisor R/W/X enforcement are unchanged and continue to use the existing Bus
and memory tests; no third ring, kernel backdoor, or host-only guest privilege
path was introduced.

Three dedicated CPU privilege tests cover all privileged operations and atomic
User rejection, both-mode SYSCALL/TRAP behavior, and exact User/IE entry-return
semantics in LZ32/LZ64. Existing comprehensive, status, memory-permission, and
trap suites continue to cover instruction metadata, reserved status bits,
Supervisor non-bypass, and controller ordering. Workspace total: 194 tests,
zero doctests.

No new production abstraction was required for this step. Full required Rust
gates, strict workspace/all-target Clippy, host Nix flake checks, and package
build passed on x86_64-linux. Package output:
`/nix/store/46y5whswx7pv8azs30ra1bxpysqbmnd9-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No SDL, external dependency, staging, or commit
was added. Rust source/test line count is 15,067. Step 33 (flat OS memory model)
is next.

## Step 31 — Trap and Interrupt Controllers

Implemented the CPU-owned `TrapController` and machine-owned
`InterruptController` required by Step 31. `ReferenceInterpreter` now has one
TVEC, one active frame, immutable exact pre-entry snapshots, typed causes, and
editable EPC/ESP/ESTATUS. `CSRR`/`CSRW`/`RFE` execute real controller behavior
instead of the old placeholder fault. Entry validates the complete Supervisor
execute fetch before mutation, forces Supervisor/IE-off while preserving
NZCV, SP, and all registers, and writes no guest frame. Typed entry methods keep
software payloads sign-extended by mode, external IDs in payload, and fault
payloads zero.

Failed entry and double-trap records retain the first triggering attempt and
second context; controller terminal state blocks direct CPU execution, RFE,
control access, and repeated delivery. `MachineError::TrapEntry` retains the
boxed failure, optional original CPU fault, and boxed `TrapAttempt`. Successful
fault delivery keeps the original fault available through the active frame;
handler instructions do not erase it, and RFE/reset releases it. Machine step
events now distinguish `Stepped`, `Trapped { TrapEvent }`, and `Halted`; bounded
runs stop at a delivered trap and preserve counts. Fault classification now
distinguishes illegal/privilege/width/address/alignment/division/control/
unmapped/permission/device causes instead of flattening them.

Interrupt requests use strong `InterruptId` values, coalesce duplicates, remain
sorted for lowest-ID delivery, respect IE, defer while a frame is active, and
acknowledge only after successful entry. Delivery occurs only at an eligible
running boundary; failed target fetches leave the request pending. Reset clears
frames, pending requests, and retained faults. HALT cannot be woken by an
interrupt. There is no syscall service implementation or device interrupt
assignment yet; `SYSCALL` still produces a delivered trap boundary for Step 35–37.

Seven CPU trap tests cover entry snapshots, typed payload rules, CSRR/CSRW/RFE,
invalid entry atomicity, repeated terminal double traps, external deferral, HALT
behavior, and frame-control ordering. Nineteen machine tests cover trap
handoffs, successful/failed/fault delivery, precise cause vectors, interrupt
masking/order/ack/reset, retained diagnostics, EI/RFE boundaries, and both
architecture modes; one interrupt-controller unit test covers coalescing and
post-ack re-request. Workspace total: 191 tests, zero doctests. Independent
review corrections included precise nested cause mapping, boxed error size,
first-attempt retention, terminal controller enforcement, and direct external
entry semantics.

Full required Rust gates, strict workspace/all-target Clippy, host Nix flake
checks, and package build passed on x86_64-linux. Package output:
`/nix/store/42vgz6zw6zdwhiz26cqq5ipm1h2m263k-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No SDL, external dependency, staging, or commit
was added. Rust source/test line count is 14,955. Step 32 (complete and verify
the Supervisor/User privilege contract) is next.

## Step 30 — User/Kernel/Hardware Boundary

Extended `docs/os-design.md` with the normative separation required by Step 30.
It defines User as unprivileged application execution, Supervisor as the
bootloader/kernel execution mode, and hardware as the privately owned machine,
Bus, address space, devices, and virtual clock. The required path is fixed as
`User -> SYSCALL/trap -> TrapController -> kernel dispatcher/service -> virtual
hardware -> structured result`; host or SDL paths cannot substitute for it.

A direct-access matrix and prohibition list make the boundary explicit. User
cannot execute `HALT`, `RFE`, `EI`, `DI`, `CSRR`, or `CSRW`; access trap/control
state; touch kernel-only or User-denied memory/device mappings; change privilege
through a side channel; forge/push a trap frame; retain kernel-owned handles; or
invoke Bus, mapping, device, allocator, scheduler, raw peek/load, host reset, or
machine internals. `SYSCALL`/`TRAP` remain legal in both modes but change
privilege only on trap entry. Trap entry/RFE preserve the already-defined
register/status/frame rules.

Supervisor is explicitly not an architecture superuser: it still obeys every
R/W/X, width, address, mapping, fetch, stack, and device policy. Kernel services
must validate syscall identity, pointers, lengths, handles, versions,
permissions, ownership, and limits before mutation. Exact syscall IDs and result
encoding remain deferred to Step 35; the document creates no ABI constants.
Headless and SDL frontends use the same kernel path and cannot grant access.

No Rust API or test was added because this is a design step; the existing 174
tests remain meaningful. Manual review against ISA privilege/trap/memory clauses
and current APIs found no duplicate privilege model, hidden User bypass, Linux
assumption, or unimplemented-service claim. Full Rust gates, host Nix flake
checks, and package build passed on x86_64-linux. Package output:
`/nix/store/1l1h9l6zkddcwn6xn943n2f6648lggjj-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No staging or commit was created. Step 31
(traps and interrupts) is next.

## Step 29 — LazOS Architecture Design

Created `docs/os-design.md` and added it to the Nix source set. The document
defines LazOS as a small native OS rather than a Linux imitation and fixes its
architectural principles: explicit machine/kernel/process ownership, reference
interpreter authority, validate-before-mutation, structured failure, enforced
privilege, deterministic virtual time, simple complete mechanisms, and headless
operation independent of SDL3.

The design records the current machine/boot/device foundation separately from
future kernel subsystems. It assigns trap control to Step 31, privilege to Steps
30–32, flat protected memory to Steps 33–34, the shared ABI/dispatcher to Steps
35–37, process/thread state to Step 38, deterministic round-robin scheduling to
Step 39, validated `.lzx` loading to Step 40, a virtual in-memory filesystem to
Step 41, and real `init`/shell delivery to Steps 42–43. It requires the kernel
to own process address spaces, CPU contexts, stacks, images, and handles while
reusing existing `MachineState`, `ExecutionState`, Bus, device, clock, and memory
contracts rather than duplicating them.

The document also fixes the intended service boundaries: one boot handoff into
Supervisor, one machine-owned trap controller, a later flat User/Kernel memory
map, a shared custom OS ABI, validated process services, bounded deterministic
round-robin scheduling, one native executable semantic model shared by loader
and linker, and a virtual filesystem with per-process handles. Console is the
only implemented device; display/input drivers, persistence, paging, and SDL
presentation remain future work. Graphics/input ABI calls are not advertised
before drivers exist, and syscall numbers remain deferred to Step 35.

No Rust API or test was added because Step 29 is documentation-only; all 174
existing tests remain meaningful. A manual cross-check against `docs/isa.md`,
`docs/boot.md`, current machine/device APIs, and the exact Step 29 roadmap scope
found no Linux-model substitution or premature implementation claim. Full
format, strict all-target Clippy, all-target Cargo check/test, host Nix flake
checks, and package build passed on x86_64-linux. Package output:
`/nix/store/mc5cw970gfvhvqy1s3fcg41a6jjqp8ny-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No external dependency, Rust code, staging, or
commit was added. Step 30 (User/Kernel/Hardware separation) is next.

## Step 28 — Typed ROM Bootloader

Added `lazalith-boot` (`no_std + alloc`; local devices/ISA/machine/memory/types
dependencies only), registered it in the workspace/lockfile/Nix installation,
and included `docs/boot.md` in the Nix source set. `BootImage` owns one exact
materialized 512 KiB ROM, exposes read-only config/header/ROM/kernel views, and
constructs an empty-device `MachineSetup`; a non-empty device manager is rejected
rather than silently left unmapped. `BootImage::start` builds a fresh machine,
executes only the fixed ROM bootloader, and returns stopped precisely at the
validated kernel entry. No arbitrary caller-supplied-machine reboot API is
claimed.

The real bootloader is emitted as canonical `lazalith-isa::Instruction` values
and encoded with the shared ISA codec. It checks header high halves, fixed load
address, nonzero ROM-payload-bounded length, entry alignment, and entry range
without wrapping, copies the payload byte-by-byte to kernel RAM, forms the
absolute entry, clears scratch registers, and executes `JMP`. Host parsing first
validates exact ROM length, magic/header size/version/architecture/flags/load/
reserved fields, LZ32 high-half rejection, payload capacity and bounds, entry
and canonical first instruction, checksum, canonical bootloader prefix, and all
reserved gaps/tail bytes. CRC-32/ISO-HDLC is independently checked against its
standard vector. `BootError` retains structured field values and typed cause
chains, including decode, width, memory, allocation, conversion, and machine
failures.

The mandatory map is boot ROM Supervisor R+X, kernel-image RAM Supervisor RWX
(the bootloader must write it in the same flat mapping), and kernel stack
Supervisor RW. After transfer, `start` performs a pure executable fetch through
the new `LazalithMachine::inspect_instruction` API and verifies PC, SP,
Supervisor/IE status, every handoff register, and zeroed scratch registers before
returning the `Reset`-lifecycle machine at the kernel entry. The first kernel
instruction remains unexecuted. There is no filesystem, relocation, `.lzo`/`.lzx`,
syscall, process, trap, interrupt, User transition, or OS policy in the
bootloader.

Ten boot tests cover both modes, zero/nonzero entries, exact copied bytes and
permissions, full handoff, next kernel execution, exact ROM round trip, all
header/checksum/reserved/code rejection and combined-error ordering, maximum
payload plus overflow, LZ32 high-half values, empty-device enforcement, device
clock mismatch, runtime guard HALT before any partial copy (including LZ32
subtraction underflow), and entry-fetch permission failure after an exact copy.
Workspace total: 174 tests (10 boot, 11 machine, 28 memory, 35 CPU, 56 types,
14 ISA, 3 devices, 17 diagnostics), zero doctests.

Development found and corrected real defects before completion: ST operand
order, four-byte branch scaling, the fixed-load shift, absolute entry addition,
the static instruction-count limit for variable payloads, the R+X-versus-copy
permission contradiction, RAM-sized versus ROM-sized payload limits, validation
order/reserved bytes/typed range causes, and the initially over-broad arbitrary
machine reboot API. The final independent re-review found no remaining
confirmed Step 28 implementation defect.

All required gates passed on x86_64-linux: format check, strict workspace
all-target Clippy, all-target workspace check/test, all host `nix flake check
path:.` outputs, and `nix build path:.`. Package output:
`/nix/store/60amgz104ahga6lbc9mx6z6qzv5gvddc-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No external dependency, SDL, unsafe, code
comment, staging, or commit was added. Rust source/test line count is 13,083.
Step 29 (LazOS design) is next.

## Step 27 — Flat Boot Specification

Created `docs/boot.md` as the normative v1 boot contract for both LZ32 and LZ64.
It defines exact reset CPU/device/epoch state; one reset vector and boot address
at `0x0000_0000`; a flat identity-mapped boot ROM, kernel-image RAM, and separate
kernel stack; fixed kernel load/entry validation; a deterministic boot image; and
the Supervisor handoff registers. Step 28 now implements the specified loader and
fixed ROM bootloader; no LazOS kernel exists yet.

The mandatory map is boot ROM `0x0000_0000..0x0007_ffff` (Supervisor R+X),
kernel image RAM `0x0010_0000..0x0017_ffff` (Supervisor RWX so the bootloader can
populate it), and kernel stack RAM `0x0018_0000..0x0018_ffff` (Supervisor R+W),
with initial SP `0x0018_f000`. Cold RAM begins zero; warm machine reset preserves
RAM by the Step 26 contract, and kernel software may not depend on unrelated
bytes being cleared. The common SP is valid and aligned for four-byte LZ32 and
eight-byte LZ64 stack words.

The exact materialized boot ROM stores the fixed bootloader at address zero, a
48-byte `LZBOOT01` little-endian header at `0x0000_0400`, and up to
`0x0007_f000` payload bytes at `0x0000_1000`. The header fixes format version 1,
architecture, zero flags, load address `0x0010_0000`, nonzero image length,
relative entry offset, CRC-32/ISO-HDLC payload checksum, and zero reserved data.
Unassigned code/header gaps and the tail must be zero. Step 28 intentionally uses
an empty device manager and installs no MMIO mapping.

The successful handoff fixes PC to the validated entry, SP to `0x0018_f000`,
Supervisor mode with IE disabled, `r0=0`, `r1=load address`, `r2=image length`,
`r3=r15=entry`, and `r4`–`r14=0`; NZCV is explicitly boot-owned rather than a
stable handoff value. Trap state is initially empty/unset. The specification
rejects filesystem boot, `.lzo`/`.lzx`, relocations, compression, probing,
paging, User entry, driver policy, and OS behavior as premature v1 scope.

This documentation-only step added no placeholder Rust API or meaningless test;
its pre-Step-28 workspace total was 164 tests. Its full Rust/Nix gates passed
before Step 28, and `docs/boot.md` is now part of the Nix source set. No staging
or commit was created. The Step 28 handoff above supersedes the earlier
implementation-status wording.

## Step 26 — Validated Machine Lifecycle

`LazalithMachine` now owns an explicit six-state lifecycle: `Created`, `Reset`,
`Running`, `Paused`, `Halted`, and `Faulted`. Construction succeeds in `Created`
and does not implicitly begin execution. The transition contract is:

| Current state | `reset` | `step` | `run` | `pause` |
| --- | --- | --- | --- | --- |
| `Created` | `Reset` | reject | reject | reject |
| `Reset` | `Reset` | execute, remain `Reset` | enter `Running` | reject |
| `Running` | `Reset` | execute, remain `Running` | continue | `Paused` |
| `Paused` | `Reset` | execute, remain `Paused` | enter `Running` | reject |
| `Halted` | `Reset` | reject | reject | reject |
| `Faulted` | `Reset` | reject | reject | reject |

`run(0)` is a real lifecycle transition but executes no instruction. A successful
`HALT` overrides the source state with `Halted`; any CPU fault overrides it with
`Faulted` while returning the original boxed, structured fault. Rejected execution
returns `MachineError::InvalidTransition { operation, state }`; the old generic
`MachineError::Halted` variant is gone. The error has precise display text and no
synthetic cause. Invalid transitions, including terminal-state `run(0)`, perform
no CPU, clock, count, device, or lifecycle mutation.

A CPU fault still leaves the faulting instruction architecturally atomic, but the
machine lifecycle intentionally changes to `Faulted`. A bounded `run` remains
non-atomic across instructions: successful earlier instructions and their memory
or device effects remain when a later instruction faults. Traps are outcomes,
not machine faults; `run` stops at a trap without re-executing its unchanged PC
within that call. Trap delivery across calls remains Step 31 work, so a later
explicit step may observe the same trap and no fake `TrapController` was added.

`reset` is valid from every state and infallible under the existing trusted CPU
and `Device::reset` contracts. The machine privately retains its validated setup
CPU snapshot and initial virtual epoch. Reset reconstructs a fresh
`ReferenceInterpreter` (restoring setup PC/SP/status, zeroed ordinary registers,
and CPU Running execution state), resets every device, synchronizes the device
manager and all devices to `MachineSetup::initial_time`, restores the machine
clock to that same epoch, and enters `Reset`. `DeviceManager::reset_at` and the
narrow `Bus::reset_devices` bridge implement this without exposing device
internals. Existing RAM/ROM regions, RAM contents, and device mappings are
preserved: Step 27 owns boot/reset images and initial-memory policy, so Step 26
does not invent a cold-memory snapshot.

The successful-instruction counter remains a lifetime diagnostic count since
construction across resets; `MachineRun::executed` is per call and
`halted_at` remains cumulative. A first-count overflow is preflighted before
`run` changes `Reset` or `Paused` to `Running`. Host setup, clock, and read-only
inspection APIs retain their Step 25 behavior and do not themselves resume a
terminal CPU; lifecycle enforcement applies to `step`, `run`, and `pause`.

The machine suite now has 11 tests covering the complete transition table,
repeated reset, zero-work runs, paused single-step, both LZ32/LZ64 paths, exact
HALT and capacity-fault terminal behavior, full invalid-operation matrices,
recovery from both terminal states, trap distinction, cumulative counts, full
CPU/device/clock reset, preserved memory/mappings, and deterministic execution.
One new MMIO test verifies reset-at-epoch across multiple preticked devices and
preserved routing. Workspace total: 164 tests (11 machine, 28 memory, 35 CPU,
56 types, 14 ISA, 3 devices, 17 diagnostics), zero doctests. An independent
review found and corrected first-step count-overflow ordering and two weakened
fault assertions before the final run.

At takeover, `lazalith-types` and `lazalith-diagnostics` already had uncommitted
source-provenance work. Its baseline tests did not compile because `SourceSpan`
was no longer `Copy`, and one diagnostic still expected cross-manager range
revalidation rather than the new `WrongSource` result. Only the necessary test
ownership clones and provenance expectation were repaired; the pre-existing
production provenance changes were preserved rather than reverted or claimed as
Step 26 work.

All required gates passed on x86_64-linux: `cargo fmt --all --check`, strict
workspace/all-target Clippy with `-D warnings`, `cargo check --workspace`,
`cargo test --workspace`, all host `nix flake check path:.` checks, and
`nix build path:. --no-link --print-out-paths`. Package output:
`/nix/store/n06vi4x0lrn6agwnqz1k5bgmi92chxpl-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No dependency, SDL, Nix, or code-comment changes;
no staging or commit. Rust source/test line count is 11,296. Step 27 (the boot
specification) is next.

## Step 25 — Privately Owned Machine

Added `lazalith-machine` (no_std + alloc; only local CPU/devices/ISA/memory/
types dependencies), registered in the workspace, lockfile, and Nix library
installation (seven libraries). `LazalithMachine<D: Device>` exclusively owns a
private CPU, Bus, ArchitectureConfig, and VirtualClock; nothing is exposed
directly. Construction is explicit via `MachineSetup { config, devices,
regions, pc, sp, status, initial_time }` and validation happens once at the
memory/CPU boundary; the machine never invents state. The clock is prevalidated
(`VirtualClock::advanced`) before device ticks, preserving both on failure, and
no wall-clock enters the system.

At the Step 25 boundary, the machine deliberately stopped short of Step 26: it
exposed `step` and bounded `run(limit)` only — no reset/pause/resume/lifecycle or
state machine. At that boundary, `step` ran one CPU instruction through the bus
and returned `MachineEvent::Stepped`
or `MachineEvent::Halted`; `run` reports per-call `executed` (including the
halting step) and cumulative `halted_at` (instructions executed since
construction at first halt; `None` when the limit bound the loop). Faults,
clock overflow, and post-halt steps return typed `MachineError`s
(`Fault(Box<CpuFault<MemoryFault>>)` keeps the error small) and left the then-current
non-halted execution flag, clock, and device state untouched; no fake trap
delivery or controller existed.
Inspection APIs are read-only: config, clock, is_halted, architectural_state,
devices, memory, and a pure peek_memory. Loading (`load_region`, `load_bytes`,
`map_device`) is allowed before execution and fails without mutation.

Four tests cover: end-to-end `Hello, Lazalith` in both modes through a real
ROM+RAM+MMIO machine with exact halt timing and no internal exposure; fault
and clock-overflow atomicity with unchanged CPU state, output, and elapsed
cycles; deterministic repeated construction and bounded then completing runs;
and setup/loader failure contracts, including an unmapped-PC machine whose
first step faults without effect and a machine that halts inside a bounded run
(returning an error, not a fake success). Workspace total: 156 tests (4 machine,
3 devices, 27 memory, 35 CPU, 56 types, 14 ISA, 17 diagnostics), zero doctests.
A clippy `result_large_err` error was fixed by boxing the fault variant, and
three test-side issues (a stray `second_machine` binding, two shadowed helper
names, and one run-semantics miscount) were corrected before the full rerun.
All gates passed: fmt/fmt check, strict all-target Clippy, workspace
check/test, all three host Nix checks, and package build
(`/nix/store/j566vaghra4j72hhh3zl6jdfgpsnqdsb-lazalith-foundations-0.1.0`).
No comments, external dependencies, staging, or commits. Scope stops at Step 25:
machine lifecycle, controller/OS/traps, and SDL frontends are explicitly out of
scope and remain for later steps.

### Step 25 post-review corrections

An independent review found three real machine-layer defects; all are fixed and
covered by three new tests (7 machine tests total, 159 workspace tests):

- Construction previously ticked pre-elapsed devices by the full
  `initial_time`, double-advancing them relative to the machine clock.
  Construction now derives the delta `initial_time - devices.clock().elapsed()`
  (structured `MachineError::InitialClock { devices, requested }` when devices
  are ahead) and ticks only that delta, so both clocks and devices agree.
- `run()` previously ignored trap outcomes and re-executed the unchanged trap
  PC forever. It now stops at the trap, returns `MachineRun.trap =
  Some((TrapRequest, resume_pc))` with exact pre-state preserved (traps do not
  advance PC), and never re-executes it.
- Cumulative execution counting moved into `step()` (checked, overflow is
  `MachineError::InstructionCountOverflow`), so stepped-then-run sequences
  report correct totals (`halted_at` includes pre-run steps).


## Step 24 — Deterministic Virtual Clock

`VirtualClock` now lives in `lazalith-types` beside the shared `CycleCount` it
wraps: `new`/`at` construction, pure `advanced`, fallible `advance`, and
`prepare_advance` returning a `PreparedAdvance` whose dropped plans change
nothing and whose single `commit` publishes the prevalidated next clock.
Overflow is `ClockOverflow { current, delta }` (an `Error`), never a wrap; a
failed advance leaves the clock bit-identical. Zero deltas are exact no-ops.
There is no host wall-clock dependency anywhere in the workspace.

`DeviceManager` now owns a private `VirtualClock` instead of a raw counter, and
`Device::tick` receives `CycleCount`. `DeviceError::TickOverflow` was replaced
by `DeviceError::Clock(ClockOverflow)` with a typed source chain. Manager tick
stays atomic: overflow touches neither any device nor the clock, zero deltas
call no device, and `clock()` exposes the elapsed count for hosts. Console
stores the shared `CycleCount`, and `Bus::tick_devices` forwards it unchanged.

Three new `lazalith-types` clock unit tests (boundaries/atomic overflow, pure
prepared plans, deterministic repeated sequences) plus the updated devices and
MMIO tests all pass. Workspace total: 152 tests (3 devices, 27 memory, 35 CPU,
56 types, 14 ISA, 17 diagnostics), zero doctests. Full gates passed: fmt/fmt
check, strict all-target Clippy, workspace check/test, all three host Nix
checks, and package build
(`/nix/store/z916r7rxqps69g0pwmf8fy3lbyk60g1d-lazalith-foundations-0.1.0`).
No new dependencies; aarch64-linux remains untested. No comments, staging, or
commits. Step 25 (machine ownership, not lifecycle) is next.


## Step 23 — Bounded Byte Console

`ConsoleDevice::new(capacity)` reserves the entire bounded output buffer with
try_reserve_exact before returning. There are no allocations during guest
writes, reset, peek or tick. Offset zero is the sole byte-wide write-only
register: each successful store appends the low byte, including arbitrary binary
data; wrong offsets/sizes and full buffers fail without effects. Guest reads
are unsupported and MMIO peek is Unpeekable. Host output()/capacity()/elapsed()
provide immutable inspection, never the mutable Vec or internal fields. reset
clears output while retaining the reserved storage.

Three console tests cover all 256 bytes, truncation, reset/reuse, zero/full
capacity, deterministic oversized allocation failure, bad accesses and pure
inspection. Two console_cpu tests execute actual encoded ROM instructions via
ReferenceInterpreter + Bus in LZ32 and LZ64, producing `Hello, Lazalith` without
SDL; the full-buffer fault preserves CPU state, RAM and earlier output. The
fixture includes distinct ROM code, RAM stack storage and write-only MMIO.
A test initially used tuple syntax for the existing struct-shaped Memory fault;
that compile error was corrected, then the complete focused suite passed.

Before Step 24: all 149 workspace tests passed (3 devices, 27 memory, 35 CPU,
14 ISA, 53 types, 17 diagnostics), zero doctests. cargo fmt/fmt-check, strict
workspace/all-target Clippy, workspace/all-target check, all three host Nix
checks and package build passed. Package output:
`/nix/store/ivsgrayr5b5mhhr8mpnincn4rh6vswkb-lazalith-foundations-0.1.0`.
No dependency or flake changes were necessary for these modules/tests;
aarch64-linux remains untested. No comments, external dependencies, staging or
commits. The allocation test deterministically exercises capacity overflow,
not injected host allocator exhaustion.


## Step 22 — Owned Generic Devices and MMIO

Added `lazalith-devices` (no_std + alloc; only local ISA/types dependencies),
`Device`, `DeviceManager<D>`, `DeviceError`, and the uninhabited `NoDevice`
default for memory-only buses. DeviceId/DeviceOffset are re-exports of the
existing shared types, not parallel identifier domains. Workspace, lockfile,
and Nix installation now include six libraries.

`Bus<D>` exclusively owns AddressSpace and DeviceManager. `with_devices` and
`map_device(id, physical_start, permissions)` map the complete captured device
extent, once per ID. No alias mappings, partial extents, executable MMIO, or
RAM/ROM/MMIO overlaps are accepted, in either insertion order. Mapping errors
leave bus mappings untouched. Data routing checks configuration, full ranges,
operation kinds and permissions before invoking a device. Fetch/stack/loader
never invoke MMIO reads or writes. Host peek routes purely, never falls back to
read, and rejects unpeekable devices with a typed error. Device failures retain
memory address/access/size/privilege and participate in Error::source chains.

Device implementer contract: address_len remains stable; validate_read/write
are pure; read/write must validate all conditions and reserve any required
capacity before effects, returning error with no observable mutation. Successful
validation cannot authorize a later partial failure. peek is pure and leaves its
output unchanged on error; unpeekable registers return Unpeekable without read.
reset is infallible, allocation-free and restores initial state without changing
extent. tick receives absolute elapsed virtual cycles, is infallible and
allocation-free for every u64 input, and cannot invalidate an already accepted
MMIO transaction. The manager checks elapsed addition before ticking any device;
overflow changes neither elapsed nor any device. Zero delta invokes no devices.
Insertion reserves before effects and synchronizes the incoming device to elapsed.
These are trusted Rust implementation contracts, not a sandbox for arbitrary
trait implementations; no rollback or panic interception is claimed. Step 24
will replace raw tick counts with the shared CycleCount and clock preparation.

Seven new integration tests exercise manager validation/reset/overflow, actual
CpuMemory MMIO routing in both modes, pure/unpeekable inspection, all permission
combinations, complete/disjoint mappings across RAM/ROM/MMIO, both insertion
orders, duplicate/unknown IDs, width limits, and fetch/stack/loader rejection.
Workspace total: 144 tests (25 memory, 35 CPU, 14 ISA, 53 types, 17 diagnostics),
zero doctests. Focused baseline and MMIO suites passed. A peek closure borrow
compile error was corrected before the focused run; a final review replaced
initial duplicated IDs with shared types, followed by a complete successful rerun.
Before Step 23: cargo fmt/fmt-check, strict workspace/all-target Clippy,
workspace/all-target check, workspace test, all three host Nix checks and package
build passed. Nix output: `/nix/store/aykfniyqx3ylk39xzkgx62nic79g87sv-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No comments, external dependencies, staging,
commits, SDL, CPU dependencies in devices, or shared mutable ownership added.


## Initial Inspection

Before this report was created, the only project file present was
`instruction.md`, which contains the full 100-step implementation roadmap for
the Lazalith platform. Git metadata is present in `.git/`.

### Project files present before Step 1

```text
instruction.md
```

### Answers to the Step 1 checklist

| Question                          | Answer                                        |
| --------------------------------- | --------------------------------------------- |
| What files exist?                 | Only `instruction.md`                         |
| Is Cargo already configured?      | No — no `Cargo.toml`, no workspace            |
| Is Nix already configured?        | No — no `flake.nix`, no `flake.lock`          |
| Is there an existing emulator?    | No                                            |
| Is there existing ISA code?       | No                                            |
| Is there existing GUI code?       | No                                            |
| Git repository?                   | Yes, branch `main` exists but has no commits  |

## Step 1 Verification and Handoff

Step 1 is complete: the initial repository contents were inspected and this
report was read back. No implementation code was created or modified.
`git status --short` shows only the untracked `instruction.md` and `docs/`.
No commits were created.

The host provides Nix 2.34.8, but neither `cargo` nor `rustc` is on `PATH`.
Cargo formatting, linting, build, and test checks are not applicable to this
documentation-only step: there is no Rust workspace yet. Nix project checks
are also not applicable because no flake exists. No executable error-handling
paths were introduced.

The next step is Step 2: create the minimal Cargo workspace. Obtain a Rust
toolchain through Nix before validating it. Do not mark Step 2 complete until
`cargo build`, `cargo fmt`, `cargo clippy`, and `cargo test` succeed. Step 3
then establishes the reproducible project flake.

## Step 2 — Workspace

The Cargo workspace now contains only `lazalith-types` and
`lazalith-diagnostics`. Both are library scaffolds with no public API yet;
no placeholder functions or artificial passing tests were added. The diagnostics
crate has a local dependency on shared types. Both currently use `no_std`;
source storage and rendering may require `alloc` or `std` in subsequent steps.
Unsafe code is forbidden through inherited workspace lints.

A temporary Nix shell provides Cargo, Rust, rustfmt, Clippy, and GCC. Verified:

- `cargo check --workspace`
- `cargo fmt` and `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo build --workspace`
- `cargo test --workspace` (zero tests: no behavior implemented yet)

These validate workspace wiring, not platform functionality. Behavioral tests
must accompany the first implemented APIs. Cargo.lock is retained for Nix
builds; build outputs are ignored. No runtime error paths exist yet. Nix flake
validation belongs to Step 3, which is next.

## Step 3 — Nix Development Environment

The flake pins nixpkgs in `flake.lock`. It provides Rust, Cargo, rustfmt,
Clippy, rust-analyzer with Rust sources, GCC, GDB, pkg-config, CMake, Ninja,
and SDL3 development files. SDL3 is only a host development dependency;
neither Rust crate links it.

The default package builds and tests both libraries and installs their `.rlib`
artifacts under `lib/`. There is deliberately no executable at this stage.
Source filtering excludes build outputs and unrelated project files.
Flake checks cover workspace build/tests, Rust formatting, and strict Clippy.

Verified on x86_64-linux:

- `nix build path:. --no-link --print-out-paths`, with both library artifacts
  confirmed in the output.
- `nix flake check path:.` passed all host checks.
- `nix develop path:. -c cargo build --workspace` succeeded.
- Inside the development shell: formatting, Clippy, type checking, and tests
  succeeded; tests still number zero because there are no implemented APIs.
- `pkg-config --modversion sdl3` reported 3.4.16.

The first flake check exposed an incorrect SDL package attribute, which was
fixed. Inspecting the first package output exposed missing library installation,
which was fixed and reverified. aarch64-linux outputs are declared but were not
built on this host.

Because all project files remain untracked, bare `nix flake check` rejects the
untracked flake. Use `nix develop path:.`, `nix build path:.`, and
`nix flake check path:.` until the project files are tracked by Git; the usual
commands then use the same outputs. Nothing has been staged or committed.

This session stops after the workspace/environment foundation. Step 4 is next:
implement the shared diagnostic system, with source location and structured
rendering introduced in Steps 5–6. No ISA, CPU, emulator, OS, compiler, or GUI
functionality has been implemented.

## Step 4 — Shared Diagnostic Foundation

`lazalith-diagnostics` now provides the first real API:

- `Severity` (`Error`/`Warning`/`Note`) with plain-name display.
- `DiagnosticCode`, validated as one ASCII uppercase letter followed by at
  least one digit (e.g. `E1001`). Construction returns
  `InvalidDiagnosticCode`, which retains the rejected input, implements
  `Display` and `core::error::Error`, and never panics.
- `Diagnostic`, data rather than preformatted text: severity, code, owned
  message, and optional cause box. It is `Send + Sync` so host frontends can
  own it, implements `Display` as `error[E1001]: message`, and implements
  `Error::source` so typed cause chains survive. `SourceSpan`-style labels,
  notes, and help attachments are intentionally deferred to Steps 5–6, when
  source locations exist.
- Display output contains no terminal styling; rendering stays a later,
  separate concern. There is no `DynSourceMap`-style API yet.

Six unit tests cover accepted/rejected code forms (including non-ASCII,
empty, and trailing-letter inputs), field retention, per-severity formatting,
nested diagnostic cause chains through `Error::source`, and frontend
ownership of message strings. Verified: formatting, strict Clippy
(`-D warnings`), `cargo check`, `cargo test` (6 passed), and all Nix flake
checks plus a package build. The flake's clippy check compiled the new code
with the same strict flags. Still no ISA, CPU, emulator, OS, compiler, or GUI
functionality; Step 5 (source location types) is next.

## Step 5 — Source Location Types

`lazalith-types` now provides the platform's single authoritative source map:

- `ByteOffset`: u32-backed byte offset; saturating-free checked arithmetic
  with panics on overflow (documented), plus `as_u32`/`as_usize`.
- `SourceId`: opaque file identifier; `SourceFile`: immutable name + text +
  precomputed line-start map, built in one pass at registration.
- `SourceManager`: owns files, assigns ids in registration order, validates
  and constructs spans, and resolves positions. All other components must
  resolve line/column through it — no second source map exists.
- `SourceSpan`: byte range constructible only via `source_span` validation
  (reversed, out-of-bounds/unknown-id, and non-char-boundary ends are
  rejected with a structured `InvalidSpan`); resolution of a validated span
  cannot fail.
- `LineColumn` (1-based line, 1-based char column) and `ResolvedSpan`, which
  renders as `file.lz:2:1` (point) or `file.lz:1:1-3:6` (range).

Line semantics: lines split on `\n` only; `\r` is an ordinary character so
CRLF files do not gain extra lines and CR never inflates line counts; a
trailing newline starts a final empty line. Columns count chars, so `é`
occupies one column; offsets inside a multi-byte character resolve to
`None` rather than a wrong position. 15 unit tests cover exact line/column
computation (multi-byte, LF, CRLF, CR), point vs range rendering, empty
files, reversed/out-of-bounds/unknown-id/split-UTF-8 rejection, name
validation, and value-type behavior.

Two test failures during development were real bugs, each fixed and
reverified: validation initially sliced text before checking char boundaries
(panicking instead of erroring), and an over-broad interior-byte check
wrongly rejected spans containing whole multi-byte characters. Verified:
formatting, strict Clippy, `cargo check`, `cargo test` (21 tests: 15 types +
6 diagnostics), all Nix flake checks, and a package build. Step 6 (structured
diagnostics rendering on top of these spans) is next.

## Step 6 — Structured Diagnostics

`lazalith-diagnostics` now treats a diagnostic purely as structured data and
gains the first shared renderer:

- `Label` (primary/secondary via `LabelStyle`) carrying a validated
  `SourceSpan` and message; `Note` and `Help` are plain text attachments.
  All attach through `Diagnostic::with_label/with_note/with_help` and are
  readable through `labels()`, `notes()`, `help()`; the existing typed
  cause box remains, now also readable via `Diagnostic::cause()`. Nothing
  preformats terminal text; data stays data until rendering.
- `render_plain(&Diagnostic, &SourceManager) -> Result<String, RenderError>`
  produces rustc-style plain text. It resolves every label span exclusively
  through `SourceManager` (header position, line text via the new
  `SourceFile::line_text`/`SourceManager::line_text` API — no duplicate line
  map). It revalidates all label spans against the supplied manager before
  formatting any output; invalid spans return errors rather than panicking.
  Rendering failures use structured `RenderError` variants (`MissingSource`,
  `InvalidSpan` carrying the `InvalidSpan` cause, `UnresolvedSpan`,
  `MissingLine`), each retaining its label index.
- Rendering conventions: markers `^` (primary) / `-` (secondary); blank
  messages omit the trailing space; labels render in insertion order even
  across files; multiline spans mark every covered line with per-line gutter
  alignment; a span ending at column 1 on a later line is treated as ending
  at the previous line's end (half-open convention, no empty marker rows);
  notes, helps, and the cause render as `= note:`/`= help:`/`= cause:`
  blocks after the snippets. Columns remain the 1-based char/scalar-count
  convention from Step 5. Tabs and CR are preserved verbatim and each counts
  as one scalar; terminal display-cell alignment is not attempted. Empty/EOF
  points and newline-only selections receive at least one marker. The direct
  cause is rendered once; nested typed causes remain accessible through
  `Error::source` rather than being recursively formatted.

Eleven new diagnostics tests (17 total) cover attachment reads and ordering,
exact output for the roadmap-style snippet at `example.lz:4:1`, all
severities, notes/help/direct causes, overlapping and cross-file labels,
Unicode scalar columns, multiline spans and blank lines, aligned multi-digit
gutters, half-open end handling, empty-file/EOF/interior points, CR and tabs.
Error tests assert missing-source and invalid-span variants, label indices,
error text, and retained typed causes, including invalid later labels and
spans from another manager with shorter text or incompatible UTF-8 boundaries.
`UnresolvedSpan` and `MissingLine` are defensive fallbacks not reached by
these tests. Two new types tests (17 total) cover line text for empty and
unterminated files, Unicode, CR, blank lines, unknown sources, and invalid
line numbers.

Source ids remain manager-local: revalidation catches invalid ranges, not
provenance mismatches when another manager has the same id and a valid range.
Callers must retain the corresponding source manager. Source and attachment
text is not escaped; this is a minimal plain renderer, not a terminal-control
sanitizer. Both crates remain `no_std` with `alloc`; no code comments were
added.

Verified in the Nix dev shell: `cargo fmt`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo check --workspace`, `cargo test
--workspace` (34 passed: 17 diagnostics + 17 types), then `nix flake check
path:.` (all host checks passed) and `nix build path:. --no-link`.
Validation ran on x86_64-linux; aarch64-linux was omitted as incompatible
with this host. No dependencies were added; nothing staged or committed.
Work stops after Step 6; Step 7 and architecture design were not started.

## Step 7 — Shared Architectural Types

Inspected the existing workspace, source types, tests, flake, and verification
instructions before implementation. Architectural types live separately in
`crates/lazalith-types/src/architecture.rs` and are re-exported from the crate
root in `src/lib.rs`. Existing source-location types and behavior are unchanged.

Eight distinct, private-field value types were added, with `Clone`, `Copy`,
`Debug`, equality, ordering, and hashing:

| Type | Backing | Construction and reads | Checked arithmetic |
| --- | --- | --- | --- |
| `PhysicalAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `VirtualAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `InstructionAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `DeviceId` | `u32` | `new(u32)`, `as_u32()` | None |
| `DeviceOffset` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `CycleCount` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `InstructionCount` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `RegisterIndex` | `u8` | `TryFrom<u8>`, `as_u8()`, `as_usize()` | None |

Address arithmetic takes unsigned byte offsets, not other addresses or device
relative offsets. All checked operations return `Option<Self>`: overflow or
underflow returns `None` without mutation, wrapping, saturation, or panic.
There are no arithmetic operator implementations or automatic conversions
between architectural domains. No generic `Address` was needed. Constructors
and read getters are `const`; address constructors preserve the entire `u64`
range without imposing configuration, width, alignment, or mapping validation.
Those policies remain later work.

`RegisterIndex::COUNT` is 16. The index is an encoding field with exactly
`0..16` accepted (0 through 15), not a register file or register-role model.
The later ISA design will have ordinary r0–r15 with PC/SP separate; no special
register semantics are implemented here. There is no unchecked constructor,
public field, or infallible conversion into an index. Invalid construction
returns `InvalidRegisterIndex`, whose `input()` retains the original `u8`;
its `Display` reports the input and exclusive range and it implements
`core::error::Error` with no underlying cause.

Nine new tests cover all three address domains and all three offset/count
types at zero, above the 32-bit boundary, and `u64::MAX`; zero deltas, ordinary
arithmetic, exact maximum/zero results, and overflow/underflow; full-range
`DeviceId` reads; every one of the 16 valid register encodings and all 240
invalid `u8` inputs, including retained values, exact error text, and error
source behavior. Total: 43 passing unit tests (26 types + 17 diagnostics),
with zero doctests and no placeholder tests.

Verified on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed (workspace build/tests,
  formatting, and strict Clippy); aarch64-linux was omitted as incompatible.
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

No dependencies or code comments were added. Nothing was staged or committed.
Work stops at Step 7; Step 8 ISA design and later subsystems remain unimplemented.

## Step 8 — ISA v1 Design Draft

Step 8 is complete as documentation only. Read the roadmap's Steps 8–10 and
adjacent dependencies, the existing project-state documentation, shared
`architecture.rs` types and crate exports, and the Nix verification setup.
Created only the three roadmap-requested design documents:

- `docs/isa.md`: authoritative shared v1 encoding, opcode allocation, width and
  flag semantics, checked memory/control flow, precise traps, privilege,
  centralized interrupt draft, provisional procedure ABI, and Steps 9–10 handoff.
- `docs/lz32.md`: 32-bit register/pointer/address configuration, four-byte stack
  words, supported data sizes, width boundaries, and ABI differences.
- `docs/lz64.md`: 64-bit configuration, eight-byte stack words/data access,
  high-half address arithmetic, signed immediate behavior, and ABI differences.

Firm decisions: 16 ordinary writable r0–r15, separate PC/SP/status, little endian,
fixed eight-byte instructions aligned to four bytes in both modes; natural data
alignment 1/2/4 and additionally 8 only in LZ64. CALL pushes checked nextPC and
RET pops a mode-sized return address on a downward, word-aligned RAM stack.
Relative control flow uses nextPC + signed_i32(displacement)*4 without wrapping.
Arithmetic wraps at word width; addresses, access ends, SP updates, and PC+8
are checked. SUB C is borrow, signed overflow is separate, shifts normalize
modulo W and set N/Z with C/V cleared, and signed division/remainder trap for
MIN/-1 as well as all division/remainder variants trapping on zero divisor.

Supervisor/User and one active controller-owned trap frame are specified without
CPU implementation. Exact immutable pre-entry snapshots are separate from
editable resume PC/SP/status. Entry does not push to or switch the User stack;
RFE restores resume control state but leaves handler-arranged general registers.
Faults have no instruction partial effects. Interrupts are latched, selected
centrally at instruction boundaries, and deferred while a frame is active;
synchronous double traps are terminal rather than recursively overwriting state.
The procedure ABI is provisional; OS syscall numbers, service/register ABI,
boot mappings, and device interrupt assignments remain deliberately deferred.

### Firm handoff to Step 9

Implement immutable `ArchitectureConfig`, `WordWidth::W32/W64`, and validated
`FeatureSet`, with tests; do not implement opcodes or a CPU. Configuration queries
must expose word bits/bytes, pointer/address bits, register count, instruction
bytes/alignment, stack alignment, supported data sizes, and features. Use the
exact values and API semantics in `docs/isa.md` under “Architecture configuration”.
`FeatureSet` accepts exactly u32 bits 1 (BaseInteger); zero and any unknown bits
are structured errors retaining the input. Named mode constructors are infallible;
raw-feature construction is fallible. Preserve all existing Step 7 constructors,
checked-u64 arithmetic, register-index semantics, and address-domain separation.

### Firm handoff to Step 10

Implement only centralized pure width helpers and tests according to
`docs/isa.md` under “Shared width semantics”: truncation, source-width zero/sign
extension, explicit bit masking distinct from address validation, wrapping
arithmetic and consistent result flags, normalized shifts, division errors, and
checked signed address offsets/access ends. Never hide guest-address wrapping or
host overflow in a helper. Preserve typed domains; width helpers return values
or structured errors, not mutations of future CPU/status/trap objects. Instruction
metadata starts at Step 11; Steps 9–10 are not implemented by this session.

### Review and verification

Manually cross-reviewed all three documents for field coverage and reserved-zero
rules, unique opcode/selectors, byte order, mode widths, borrow predicates,
shift/division behavior, PC-relative base/scaling, atomic CALL/RET, exact trap
snapshots versus resume fields, fault ordering, feature validation, and ABI stack
slot offsets. Renamed the shared immediate-only format to IMM so TRAP's payload
is not mislabeled as a relative displacement; clarified CALL validation order and
raw-feature versus already-validated configuration construction.

An ephemeral Python arithmetic check decoded and re-encoded all four documented
byte vectors and checked selected both-mode extension, wrapping, shift amount,
PC/access-end, CALL/RET slot, and displacement-range examples. It passed using
`nix shell --inputs-from path:. nixpkgs#python3`; the first direct invocation
failed because python3 is not on the host PATH. No script, dependency, Rust test,
or executable ISA implementation was added. Remaining semantic acceptance cases
are explicitly future test obligations, not claimed implemented ISA coverage.

Required gates passed on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- 43 unit tests passed (17 diagnostics + 26 types); zero doctests.
- `nix flake check path:.` passed: workspace, formatting, and strict Clippy
  derivations evaluated and existing cached results were accepted (zero new
  checks executed). aarch64-linux was omitted as incompatible with this host.
- `nix build path:. --no-link --print-out-paths` succeeded with the existing
  foundations package. The flake source filter excludes docs, so these gates
  validate unchanged Rust foundations, not ISA document semantics.

No code, code comments, dependencies, or Nix configuration were changed. All
project files were already untracked; nothing was staged or committed. Work
stops after Step 8 documentation; Step 9 is next.

## Step 9 — Architecture Configuration

Implemented only configuration and its tests in
`crates/lazalith-types/src/config.rs`, re-exported from `src/lib.rs`.
Existing Step 7 architecture and source/diagnostic APIs are unchanged. No
comments, external dependencies, width arithmetic, opcodes, or CPU were added.

- `WordWidth::W32/W64` exposes `bits()` and `bytes()` as u8.
- `FeatureSet::base_v1()`, `try_from_bits(u32)`, and `bits()` accept exactly
  raw bits 1. Private storage prevents invalid feature sets. Structured
  `InvalidFeatureSet::UnsupportedBits { input, unsupported_bits }` takes
  priority over `MissingBaseInteger { input }`; `input()` retains the raw
  rejected u32. The error implements `Display` and `core::error::Error`.
- Immutable `ArchitectureConfig` offers `new(WordWidth, FeatureSet)`, `lz32()`,
  `lz64()`, and fallible `try_from_bits(WordWidth, u32)`. Its read-only queries
  are `word_width()`, `word_bits()`, `word_bytes()`, `pointer_bits()`,
  `address_bits()`, `register_count()`, `instruction_bytes()`,
  `instruction_alignment()`, `stack_alignment()`, `supported_data_sizes()`,
  `supports_data_size(u8)`, and `features()`, with exactly the Step 9 handoff
  types and mode values. Width-dependent properties cannot be independently set.

Nine new tests cover both complete mode query tables, width queries, every u8
size in both modes, all valid construction paths, missing BaseInteger, each of
31 unsupported bits with and without BaseInteger, combined unsupported masks
including u32::MAX, error priority, retained raw inputs/masks, and error display
and source behavior. All error cases also exercise configuration construction
in both modes. Total: 52 passing unit tests (35 types + 17 diagnostics), zero
doctests.

Successful verification on x86_64-linux, before this Step 9 report was updated:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed; aarch64-linux was
  omitted as incompatible with this host.
- `nix build path:. --no-link` — package build succeeded.

Nothing was staged or committed. Step 9 is complete; Step 10 pure width helpers
remain separate and unimplemented. Earlier step reports above are historical.

## Step 10 — Centralized Width Operations

Implemented only the shared width contract and its tests in a new
`crates/lazalith-types/src/width.rs`, re-exported from `src/lib.rs`. Existing
Step 7 architecture, Step 9 configuration, source, and diagnostic APIs are
unchanged. No comments, external dependencies, CPU, status, opcodes, or address
types were added; all operations are pure `WordWidth` methods over `u64`.

- Pure bit utilities: `mask()`, `truncate(value)`, and the explicit
  `mask_address_bits(value)` alias, all `const`. The mask is `u32::MAX as u64`
  or `u64::MAX`; no `1u64 << 64` is evaluated.
- Fallible extensions `zero_extend(value, source_bits: u8)` and
  `sign_extend(value, source_bits: u8)`. Only source widths 8/16/32/64 not
  exceeding the word width are accepted; other inputs return structured
  `WidthError::InvalidSourceWidth`, which retains the unmasked value, source
  bits, and width. Sign extension first masks to the declared source width and
  ignores higher container bits; a full-width source is an identity after
  masking.
- Infallible value-only wrapping arithmetic `wrapping_add/wrapping_sub/
  wrapping_mul` returning `u64`, with inputs masked first.
- Flagged `ArithmeticResult { value, negative, zero, carry, overflow }` from
  `add`, `sub`, `mul`, and logic helpers `bitand`, `bitor`, `bitxor`, `not`.
  Inputs are masked first. ADD carry is the unsigned sum exceeding the mask;
  SUB carry is borrow (`left < right`); MUL and all logic clear carry and
  overflow. Signed overflow follows the equal-sign/different-sign rules at the
  word sign bit. No flag inputs or status mutation exist.
- Normalized shifts: `shift_amount(value)` returns the truncated unsigned
  amount modulo W as `u8`; `shl`, `shr`, and `sar` normalize before shifting,
  so amounts W, W+1, 2W, and `u64::MAX` behave like 0 and W-1 without host
  overflow or host-signed shift behavior. SAR replicates the word sign bit.
  All return `ArithmeticResult` with N/Z recomputed and C/V cleared, including
  shift by zero.
- Fallible division `div_unsigned`, `rem_unsigned`, `div_signed`, `rem_signed`.
  Zero divisor returns `WidthError::DivisionByZero` and signed `MIN_W / -1`
  returns `WidthError::SignedDivisionOverflow` for both quotient and remainder;
  errors retain the original unmasked operands and width. Signed results
  truncate toward zero with `a = q*b + r` and `abs(r) < abs(b)`; no flags
  change on traps because no result is produced.
- Checked addresses: `validate_address(value)` accepts unchanged values at or
  below the mask, otherwise `WidthError::AddressOutOfRange`.
  `checked_address_offset(base, delta: i64)` validates the base first
  (`WidthError::InvalidOffsetBase`) and then accepts mathematical base+delta
  within `0..=M` using an `i128` intermediate
  (`WidthError::AddressOffsetOutOfRange`), covering every u64 base and the full
  scaled i32 displacement. `checked_access_end(base, size: u64)` validates the
  base (`WidthError::InvalidAccessBase`) and positive size
  (`WidthError::ZeroAccessSize`), then returns the inclusive last address
  base+size-1 via `u128` without wrapping
  (`WidthError::AccessEndOutOfRange`). All errors retain their operands and
  width; base rejection takes priority over delta/size handling. These helpers
  never hide wrapping, do not touch address-domain wrappers, and impose no
  alignment, mapping, permission, or memory-size policy.

Eighteen new tests cover both widths with boundary tables and independent
`u128`/`i128` reference grids: mask/truncate/address-mask identities, all 256
source-bit values against both extensions with retained inputs, extension
boundaries that ignore container bits above the source, add/sub carry and
signed-overflow tables, full arithmetic grids against an independent wide
reference, logic N/Z with C/V cleared, normalized shifts for 0, W-1, W, W+1,
2W, `2^32`, and `u64::MAX` against multiplication/floor-division references,
signed division sign/rounding tables including zero quotients, division error
retention for div and rem on every grid operand, unsigned MIN/-1 results,
division reference grids, address validation never masking, offset tables with
high LZ64 addresses and `i64::MIN`/`i64::MAX`/scaled-i32 deltas, access-end
tables at every boundary including `u64::MAX` sizes and `2^63` accesses, base
error priority, and per-variant Display/context checks. Total: 70 passing unit
tests (53 types + 17 diagnostics), zero doctests.

`docs/isa.md` gained a “Step 10 Rust API mapping” section documenting the
method names, signatures, error variants, and precedence rules above.

Successful verification on x86_64-linux, before this Step 10 report was added:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed (workspace build/tests,
  formatting, and strict Clippy); aarch64-linux was omitted as incompatible
  with this host.
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

No dependencies were added. Nothing was staged or committed. Work stops at
Step 10; Step 11 instruction metadata remains unimplemented.

## Step 11 — Structured Instructions and Minimal Canonical Codecs

Step 11 is complete. Read the current roadmap, the entire shared ISA encoding,
opcode, selector, and metadata contract, both mode documents, shared types and
width/configuration implementations, workspace, and flake before implementation.
The existing workspace built successfully as the baseline. No earlier working
implementation was rewritten. Work stops here; Step 12 has not been implemented.

### APIs and files

Created `crates/lazalith-isa/Cargo.toml`, `src/lib.rs`, `src/metadata.rs`,
`src/operand.rs`, `src/codec.rs`, and `tests/isa.rs`. The library is `no_std` and
allocation-free, with only a local `lazalith-types` runtime dependency and local
`lazalith-diagnostics` test dependency. No external packages, SDL dependency, or
code comments were added. Existing types and diagnostics are unchanged.

- `Opcode`, `InstructionDefinition`, `InstructionFormat`, and `OperandKind`
  cover all 40 opcodes and 13 formats. One opcode declaration generates enum,
  lookup, enumeration, and metadata; one format layout supplies operand order,
  fields, and the derived reserved mask to encoder, decoder, and validation.
  Definitions also expose privilege requirement, BaseInteger, NZCV effect, and
  immediate meaning. Public read-only references make metadata reusable by
  future assembler/disassembler/compiler/documentation consumers.
- `Operand` uses strong `RegisterIndex`, `DataSize`, `Condition`, and
  `ControlRegister` values. Memory is a composite base plus signed-i32 byte
  displacement. Wider-i64 convenience constructors reject immediate overflow
  with retained input and conversion cause; no silent truncation is performed.
- `Instruction::new(config, opcode, operands)` validates before private storage;
  getters expose opcode, shared definition, and immutable operands.
  `validate(config)` and `encode(config, &instruction)` recheck mode widths.
  `decode(config, &[u8])` accepts exactly eight bytes and returns an Instruction.
  Encoder returns a new canonical little-endian `[u8; 8]`; neither codec mutates
  machine or caller state. AX order is control then register, and r0 is ordinary.
- Structured `DecodeError`, `InstructionError`, `ValidationError`,
  `OperandError`, and `UnknownOpcode` retain rejected inputs and typed causes.
  Decode checks length, opcode, every reserved bit, selectors, then mode width.
  Size selector 3 in LZ32 is InvalidWidth; invalid selectors/reserved encodings
  are distinct illegal-encoding cases. Full rejected instruction bytes survive.
  A defensive register-conversion error retains InvalidRegisterIndex but is
  unreachable under the current four-bit layout. Shared diagnostics integration
  preserves the full decode/validation cause chain without another renderer.
- Privilege and control access permissions are metadata, not codec rejection:
  CSRW read-only controls remains canonically valid for later InvalidControlState.
  No fetch, PC arithmetic, register file, status, CPU execution, assembler parser,
  disassembler, or trap delivery was introduced.

Updated workspace `Cargo.toml` and generated `Cargo.lock` for the new local crate.
Updated `flake.nix` to install `liblazalith_isa.rlib` alongside both foundations
and include `docs/isa.md` in the source filter because the opcode contract test
reads the full allocation table with `include_str!`. Added the Step 11 API,
error precedence, scope, and test coverage section to `docs/isa.md`; the design's
canonical byte assignments and semantics were not changed.

### Tests and verification

14 new ISA integration tests passed; workspace total is 84 tests (14 ISA,
53 types, 17 diagnostics), zero doctests. Tests check all opcode rows against the
actual design file, all format masks and non-overlap, exact published bytes,
and independent expected bytes for both-mode roundtrips over every register
tuple, legal selector, and nine signed immediate boundary/pattern values.
Coverage is exhaustive for opcode/format/register/selector combinations, not
all 2^32 immediate patterns or all 2^64 encodings. Malformed cases include every
unallocated opcode, each reserved bit per opcode and combined masks, selector
precedence, all raw u8 selector conversions and encoded nibbles, MEM widths,
length/count/kind errors, wide immediate bounds, mode revalidation, immutability,
selector name/value mapping, Display, and typed diagnostic cause chains.

The first strict Clippy run found a collapsible conditional, which was fixed;
subsequent verification passed. There were no failing ISA tests. Final successful
gates on x86_64-linux, completed before this report was updated:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — formatting, strict Clippy, and workspace build/tests
  passed. aarch64-linux was omitted as incompatible and is not claimed tested.
- `nix build path:. --no-link --print-out-paths` — package build succeeded;
  output `lib/` was inspected and contains all three `.rlib` artifacts.
- Inspected the new crate for comment markers: none. Cargo.lock contains only
  the three local workspace packages. Nothing was staged or committed.

### Next handoff — Step 12 only

Implement the register file next, with private storage and validated read/write
APIs using the existing `RegisterIndex`, `ArchitectureConfig`, and centralized
`WordWidth` behavior. All sixteen general registers are writable ordinary
registers; PC/SP/status are separate, never aliases. Preserve existing shared
metadata/codecs; later CPU validation must keep fetch, canonical decode, runtime
privilege, checked nextPC, and instruction-specific checks in design order.
Continue to use `path:.` Nix commands while files remain untracked. Earlier step
reports are historical; no Step 12 or later functionality is part of Step 11.

## Step 12 — Register File

Step 12 is complete. Created `crates/lazalith-cpu` (`no_std`, zero dependencies
beyond local `lazalith-types`, no SDL, no code comments) with the first CPU-side
type, `RegisterFile`, in `src/registers.rs` and re-exported from the crate root.

- Private `[u64; 16]` storage; the array is never exposed. Construction takes
  `ArchitectureConfig`, records its `WordWidth`, and zeroes all registers.
- `read(RegisterIndex) -> u64` and `write(RegisterIndex, value)`; every write is
  truncated through the shared `WordWidth::truncate` contract (32 bits in LZ32,
  64 in LZ64), so values above the architectural word never survive. There is no
  `set`/`get` that bypasses truncation.
- All sixteen registers, including `r0`, are writable ordinary registers; reads
  of a never-written register return zero. No SP/PC/status aliasing exists at
  this layer at all; those live in later architectural state, not here.
- Raw access goes through the existing validated `RegisterIndex` type:
  `read_raw(u8) -> Result<u64, InvalidRegisterIndex>` and
  `write_raw(u8, value) -> Result<(), InvalidRegisterIndex>` reuse the Step 7
  error, which retains the rejected index. Invalid raw accesses mutate nothing.
- `word_width()` exposes the configured mode. No SP/PC/status fields, no flags,
  no execution or debug state; those are Steps 13–15.

Workspace `Cargo.toml` gained the `lazalith-cpu` member; `Cargo.lock` was
regenerated; `flake.nix` installs `liblazalith_cpu.rlib` with the other three
artifacts.

Three integration tests cover: both modes with all 16 registers starting zero
and independently writable (including `r0` and `r15`); truncation tables at
8/16/32/64-bit boundaries for every register in both modes; and every invalid
raw index 16..=255 for read and write, each retaining its exact input while the
whole file compares equal to its pre-call state. Workspace total is 87 tests
(3 CPU + 14 ISA + 17 diagnostics + 53 types), zero doctests.

Verified before this section was written, on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check
  && cargo clippy --workspace --all-targets -- -D warnings && cargo check
  --workspace && cargo test --workspace'`
- `nix flake check path:.` — all host checks passed (aarch64-linux omitted,
  incompatible with this host).
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

Nothing was staged or committed. Step 13 (CPU state separation) is next.

## Step 13 — Separated CPU State

Implemented `ArchitecturalState` with private configuration, register file,
`InstructionAddress` PC, `VirtualAddress` SP, and status. Construction takes
explicit PC/SP/status rather than inventing boot/reset values. General register
writes delegate to the Step 12 file; only an immutable register-file reference
is exposed, preventing replacement by a file with another mode. Privilege is
derived from status, never duplicated. `ExecutionState::Running/Halted` and
`DebugState { single_step }` are independent owner-held values, not fields of
guest state; machine lifecycle, counters, breakpoints, and traps remain later.

`validate_pc`/`validate_sp`, `set_pc`/`set_sp`, and atomic `restore_control`
validate width before alignment, PC before SP before status. Control restoration
preserves general registers. Structured `ControlStateError` retains the offending
register/input and width or typed `WidthError`/`InvalidStatus` cause. No masking,
mapping checks, fetch checks, or guest instruction privilege enforcement is
hidden in these host state APIs. Four-aligned PC is valid even when a subsequent
fetch or PC+8 would overflow; SP alignment is four/eight by mode.

The permitted minimal Step 14 dependency is `StatusRegister`: private valid
low-six-bit storage, checked raw construction retaining all rejected bits,
`bits()` and `privilege()`. No arithmetic/IE/branch methods yet.

Four new tests cover both-mode width/alignment boundaries including high-half
LZ64 and end-of-address-space values, separation/no aliases, every reserved
status bit (including above bit 31 in LZ32), constructor rejection, and failed
single/combined updates leaving the full state unchanged with retained errors.
All gates passed on x86_64-linux before this report: fmt and fmt check, strict
workspace/all-target Clippy, workspace check/test (91 tests: 7 CPU + 84 existing,
zero doctests), `nix flake check path:.`, and `nix build path:. --no-link
--print-out-paths`. Three host check derivations passed; aarch64 was omitted.
No staging/commits, external dependencies, SDL, comments, interpreter, or fake
trap controller. Step 14 is next.

## Step 14 — Centralized Status and Conditions

Completed `StatusRegister` with exact N/Z/C/V/IE/U bits 0–5; all other raw bits
are rejected, never masked. Added control-only construction, individual flag
queries, IE/privilege setters preserving other bits, `update_arithmetic` consuming
the shared `ArithmeticResult`, and `matches(lazalith_isa::Condition)` for all 15
shared conditions. No selector duplication. Architectural state delegates its
arithmetic and IE updates; raw control restoration remains atomic. These are host
APIs: the future interpreter must enforce EI/DI/RFE permissions first.

Added local `lazalith-isa` dependency and regenerated Cargo.lock; still no external
packages, allocation, SDL, or code comments. Four new tests exhaust all 64 status
values in both modes, all 64 × 16 flag replacements, all shared branch predicates,
control preservation, and real width-helper arithmetic/logic/shift/division
boundaries including borrow, overflow, and failed division leaving state intact.
The first Clippy gate rejected ambiguous operator precedence in flag packing;
parentheses fixed it. The complete rerun passed before this report: fmt/check,
strict workspace/all-target Clippy, workspace check/test (95 tests: 11 CPU + 84
existing, zero doctests), all three host Nix check outputs and package build.
aarch64-linux remains untested. Nothing staged/committed. Step 15 is next.

## Step 15 — Typed Execution Outcomes

Implemented `ExecutionOutcome::{Continue, Jump, Call, Return, Trap, Halt}` in
`src/outcome.rs`, with typed absolute/relative targets, return instruction
addresses, and syscall/software requests. Shared checked nextPC applies to all
variants; branch displacement scales by four without wrapping. CALL/RET derive
mode-sized stack updates; no SP payload supplied by callers can bypass arithmetic.
HALT checks Supervisor permission, advances once, and blocks future outcomes.
Trap returns a typed request/resume PC without changing exact pre-entry state.

`prepare_outcome` validates without mutation and returns an exclusive-borrowed,
single-use `PreparedOutcome`. Dropping it is harmless. Its `commit` invokes a
caller-owned fallible stack transaction before infallible PC/SP/execution commit;
errors leave CPU state unchanged. It does not fake memory success, mapping,
permissions, trap entry, or a controller. The documented caller contract requires
success-or-no-effect memory operations and correct RET pre-read validation order.
`OutcomeError` retains the original PC/outcome and typed cause chain. Pure
`checked_next_pc`/`checked_return_sp` expose the prevalidation needed by Step 16.

Eight tests cover all outcomes in both modes, actual little-endian test-stack
words and unchanged popped bytes, relative extrema/high-half addresses, last
representable PC/stack boundaries, invalid values and fault order, dropped plans,
failed callbacks, exact trap state/payloads, and terminal/privileged HALT.
Total: 103 tests (19 CPU + 14 ISA + 53 types + 17 diagnostics), zero doctests.
Before this report all Step 15 gates passed: fmt and fmt check, strict
workspace/all-target Clippy, workspace check/test, all three host Nix check
outputs, and `nix build path:. --no-link --print-out-paths`. aarch64-linux remains
omitted and untested. Step 14 also received a separate four-test status-suite
rerun before Step 15 began; it passed.

`docs/isa.md` contains the full Step 16 API and transaction handoff. In particular,
stage instruction effects in a candidate architectural state, validate outcome
before external side effects, and publish only after infallible final commit.
Faults are not successful Trap outcomes; RFE/controller ownership and privilege
checks remain future work. No interpreter, memory subsystem, reset policy,
trap controller, SDL, external dependency, or code comment was introduced.
Nothing was staged or committed. Work stops after Step 15.

### Per-step verification counts

| Step | New CPU tests | CPU total | Workspace total | Host Nix check outputs |
| --- | --- | --- | --- | --- |
| 12 | 3 | 3 | 87 | 3 passed + package build |
| 13 | 4 | 7 | 91 | 3 passed + package build |
| 14 | 4 | 11 | 95 | 3 passed + package build |
| 15 | 8 | 19 | 103 | 3 passed + package build |
| 16 | 5 | 24 | 108 | 3 passed + package build |
| 17 | 11 | 35 | 119 | 3 passed + package build |

Each row completed fmt/clippy/check/test and Nix checks/build, then its docs,
before implementation of the next step. Nix sometimes reports four build tasks
because Cargo vendor metadata is rebuilt; the flake defines three check outputs
(workspace, formatting, Clippy), not four independent checks. The initial
Step 12 conversational count was mistaken; the corrected 87 is authoritative.

## Step 16 — Reference CPU and Transactional Memory Boundary

`lazalith-cpu` now provides the Step 16 `ReferenceInterpreter` and the minimal
CPU-side memory boundary Step 18–21 agents must implement against:

- `ReferenceInterpreter::new(ArchitecturalState)` starts `Running`; it exposes
  `architectural_state()`, `execution_state()`, and three entry points:
  `step(&mut M)`, `step_bytes(&[u8], &mut M)`, and `execute(&Instruction, &mut M)`
  for `M: CpuMemory`. Every entry first validates execution/halt state, PC
  width/alignment, and the complete eight-byte fetch range. `step` then performs
  a fetch through the memory implementation; `step_bytes` feeds an explicit
  eight-byte sequence (test/bring-up path, no fetch transaction).
- Execution order per instruction: canonical decode and mode revalidation,
  `InstructionDefinition::supervisor_only` permission check, shared checked
  nextPC, then instruction-specific validation and arithmetic. No partial state
  is visible: effects are staged in a private candidate `ArchitecturalState`,
  outcome preparation (including CALL target/newSP and RET return-SP checks)
  completes before any memory transaction, and the candidate is published only
  after the stack transaction succeeds. A discarded candidate leaves PC, SP,
  registers, flags, and memory untouched.
- All ordinary integer/move/compare/shift/logic/division instructions, LI/GETPC/
  GETSP/SETSP/GETSTATUS, MEM loads/stores, BR (taken via
  `StatusRegister::matches`, untaken as `Continue` with no target work), JMP/
  CALL/CALLR/RET, SYSCALL/TRAP as typed `TrapRequest` events with unchanged
  pre-state, privileged HALT, and privilege-checked EI/DI. Flags come from the
  shared width helpers only; non-arithmetic instructions preserve NZCV. Loads
  extend, stores truncate, MEM effective addresses use the checked
  base+displacement/access-end contract. LZ32 Double instructions fail ISA
  validation/decode before execution; direct DataAccess construction rejects
  Double with `DataAccessError::InvalidWidth`.
- Structured faults: `CpuFault { pc, opcode, cause }` with typed causes
  (`Halted`, `PrivilegeViolation`, `Decode`, `Instruction`, `Control`, `Width`,
  `NextPc`, `Outcome`, `DataAccess`, `Fetch`, `Memory`, `OperandLayout`) and
  full `Error::source` chains. Faults are errors, never Trap outcomes. RFE,
  CSRR, and CSRW are rejected as
  `CpuFaultCause::UnsupportedUntilTrapController` (typed error, not emulation);
  their privilege violations still precede that rejection.
- No new dependencies; `lazalith-cpu` remains `no_std`. Five tests
  (`tests/reference.rs` + shared `tests/support/` RAM) run both modes: a fetched
  CALL/RET/Halt sequence with exact PC/SP/stack-word/terminal-Halt checks,
  byte-encoded fetches with flags/loads/stores, fault atomicity including a
  failing transaction, nextPC-before-effects ordering for loads/RET/CALL/HALT/
  RFE, and trap-event plus controller-instruction rejection.

### Memory contract for Steps 18–21 (CPU side, `src/memory.rs`)

- `DataAccess::new(config, base, displacement: i32, size, kind, privilege)` is
  the single validation point: supported-size check (LZ32 rejects Double),
  checked base+displacement, checked inclusive end, natural alignment. It
  retains config/address/size/kind/privilege as read-only queries. Kinds are
  `Read`, `Write`, `StackRead`, `StackWrite`.
- `trait CpuMemory { type Error: Error + 'static; }` with exactly four methods:
  `fetch_instruction(&self, config, pc, privilege) -> Result<[u8; 8], Error>`
  (eight-byte execute access, four-aligned PC, no side effects);
  `read_data(&mut self, access) -> Result<u64, Error>` (little-endian,
  zero-padded container); `write_data(&mut self, access, value: u64)`
  (truncates to access size); `peek_stack(&self, access)` (side-effect-free
  RET target read). Implementations must guarantee success-or-no-effect per
  access and enforce mapping/permissions/device policy themselves; the CPU
  never masks or splits addresses. AddressSpace/Bus/MMIO layers (Steps 18–21)
  implement this trait behind the bus; the CPU crate keeps no concrete RAM.
  The test-only RAM lives in `crates/lazalith-cpu/tests/support/mod.rs` and is
  not part of the shipped crate.

Verified for Step 16 before Step 17: fmt/fmt-check, strict workspace/all-target
Clippy, workspace check, workspace test (108 total: 24 CPU + 14 ISA + 53 types +
17 diagnostics), `nix build path:.`, and `nix flake check path:.` (3 host check
outputs). aarch64-linux remains untested. Nothing staged or committed.
Step 17 test expansion followed this gate separately.

## Step 17 — Comprehensive CPU Execution Tests

Added eleven integration test functions in `tests/comprehensive.rs`:

- Independent i128/u128 arithmetic oracles exercise all binary ALU opcodes with
  both-mode boundary grids and destination aliases r0/r1/r2/r15. Expected complete
  architectural state includes exact PC, unchanged SP, NZCV, IE/U and other
  registers. Additional cases cover ADDI/SUBI, CMP and NOT, signed overflow flags,
  borrow, normalized shifts and exact division faults.
- All 16 register destinations for LI and special/move operations; both-mode
  signed immediates, unchanged status, SETSP acceptance/rejection, and writable r0.
- All 15 branch conditions over every NZCV pattern, relative displacements,
  taken target failures and untaken hypothetical overflow without speculation.
- Every supported LDZ/LDS/ST size, sign/zero extension, little-endian truncation,
  destination/base aliases, adjacent bytes unchanged, range/alignment/mapping/
  privilege/transaction failures and no late fault after a successful device read.
- CALL/CALLR/RET stack words, unchanged popped RAM, User/Supervisor calls,
  target-before-stack and return-SP-before-peek priorities, unmapped slots,
  failing transactions, RAM-only stack policy, and subsequent target fetch.
- Privileged HALT/EI/DI, all shared CSR selectors rejected explicitly until Step
  31, halted execution through every entry point, byte decode/selector errors,
  LZ32 revalidation of Double instructions, execute-versus-read permission,
  four-aligned fetch and incomplete fetch failures, typed source chains.

The initial test draft contained compile errors and incorrect expectation tables;
it was replaced with the independent oracle/focused cases before gating. Its
subsequent failure identified a real diagnostic gap: direct `execute` now retains
its supplied opcode on early faults, including Halted, without changing priority.
All eleven tests passed alone before the full gate. Step 16 tests still pass.

Verified: fmt/fmt-check, strict workspace/all-target Clippy, workspace/all-target
check, workspace tests (119 total: 35 CPU + 14 ISA + 53 types + 17 diagnostics;
zero doctests), Nix package build and all three x86_64-linux flake checks.
aarch64-linux was omitted by Nix and remains untested. `docs/isa.md` contains the
complete production memory trait/ownership/precision contract for Steps 18–21,
including pure RET peek with exclusive memory ownership, no MMIO rollback fiction,
and the distinction between production fetch and explicit injection APIs.

The existing flake already includes new CPU sources/tests and ISA documentation;
no flake change was needed. No dependencies, code comments, staging or commits
were added. No Step 5 changes or production memory/bus/trap controller work was
performed. Step 18 is next.

## Step 18 — Checked RAM Foundation

Added `lazalith-memory` (no_std + alloc, local CPU/ISA/types dependencies only),
registered it in the workspace/lock and Nix library installation. `AddressSpace`
owns disjoint `MemoryRegion::ram` mappings with private zero-filled storage.
Inclusive ends support a single byte at the architectural maximum. Constructors
and mapping validate guest ranges, host sizes, allocation and overlap before
mutation. `initialize(PhysicalAddress, &[u8])` is a privileged host loader API:
it bypasses guest permissions but requires a nonempty complete single-region
range. No public RAM slice or mutable mapping access is exposed.

`RegionPermissions` records independent R/W/X/User bits. `AccessType` wraps the
existing CPU `DataAccessKind`; `AccessSize::Data` wraps ISA `DataSize`, with
separate Instruction and host byte-span forms. Explicit `translate_identity`
validates a virtual address before constructing its physical equivalent; no
implicit conversion or masking. Faults already retain address domain, operation,
size, optional PC/privilege and typed width/allocation causes. Access operations
and their remaining fault variants follow in Steps 19–20; no bus/devices yet.

Baseline: 119 passing tests. Step 18 adds five tests (one private-storage atomicity
unit test, four integration tests), totaling 124. One test compile failure from
iterating DataSize references was fixed. All gates then passed on x86_64-linux:
Cargo fmt/check, strict workspace/all-target Clippy, workspace/all-target check,
workspace tests, `nix flake check path:.`, and `nix build path:. --no-link`.
Nix defines three host checks; vendor metadata is a fourth build task.
aarch64-linux remains untested. Documentation completed before Step 19.

## Step 19 — Distinct Memory Operations

`AddressSpace` now separates immutable eight-byte/four-aligned execute fetch,
little-endian `read_data`/`write_data`, pure `peek_stack`, and physical debugger
`peek`. Data operations consume the CPU's already-validated `DataAccess`; no
second effective-address/size validator was invented. Configuration mismatch and
wrong operation kinds fail explicitly. All mapping, R/W/X/User, and word-sized
stack checks precede mutation. Supervisor does not bypass R/W/X. Fetch needs X,
not R, and observes prior writes. Stack kinds require the architectural word.

Debugger peek is a host inspection API: bypasses guest permissions/alignment,
requires a nonempty complete physical range in one region, never changes memory,
and leaves the destination buffer unchanged on failure. Loader has the same
range/atomicity constraints. Both remain separate from guest and stack reads.

Six new tests exhaust supported data sizes in both modes, all 16 permission
combinations and privileges, four-aligned fetch, write visibility, kind/stack
size rejection, debugger purity, mode mismatch, and adjacent-region rejection.
Memory total 11; workspace total 130; zero doctests. An intermediate compile
check caught missing Display arms while extending faults; all were implemented
before testing. Targeted tests and then full fmt/clippy/check/test and all three
host Nix checks/package build passed. Documented before advancing to Step 20.

## Step 20 — Structured Memory Faults

Every memory failure is now a `MemoryFault` carrying typed domain address,
operation, size, optional PC and privilege, and a `MemoryFaultKind` cause chain.
`data_access(base, displacement, size, kind, privilege)` validates once through
the existing CPU `DataAccess` contract and returns `InvalidDataAccess` retaining
base, displacement, and the untouched CPU error. Width, alignment, mapping
priority (Permission before CrossRegion before Unmapped), end-of-space fetch
width, and word-sized stack rules all preserve typed causes. Exact boundaries:
a one-byte access at the architectural maximum succeeds; base+1, halfword there,
or a fetch with end beyond it are rejected without masking or wrapping.

`MemoryFault` implements `Error` with a `MemoryFaultKind` source that chains
into CPU `DataAccessError`/`ControlStateError` and shared `WidthError` causes;
an integration test verifies the full chain under `lazalith-diagnostics`
(added as this crate's first dev-dependency). Failed writes leave RAM unchanged
across all fault classes; peek/initialize destinations stay intact on failure.
No new numeric trap codes were allocated.

Five new tests cover the priority/retention table, diagnostics chaining, fetch
fault context in both modes, exact maximum-address boundaries, and mapping
before permission with byte preservation. Memory total 16; workspace total 135;
zero doctests. Full fmt/clippy/check/test gates and all three host Nix checks
plus package build passed; the Cargo.lock dev-dependency update is included.
aarch64-linux remains untested. Documented before advancing to Step 21.

## Step 21 — Bus with RAM and ROM Routing, CPU Integration

Revised twice after review. The first pass had only a `CpuMemory` impl for
`AddressSpace` and a panicking `map_mmio_placeholder`; the placeholder was
removed and no `unimplemented!`, `todo!`, or other panic hook remains in the
crate (swept by grep). The second pass used `Rc<RefCell<AddressSpace>>` with a
`Clone` Bus and `&self` mutation through a `with_address_space_mut` closure;
that design permitted nested-borrow panics and hidden mutable access, so it
was replaced before Step 22.

`Bus` (`src/bus.rs`) now directly owns a private `AddressSpace`. It is not
`Clone`; there is no interior mutability, no closure-based accessor, and no
way to obtain a mutable reference or internals from `&self`. Mutating routing
takes `&mut self` (`map`, `initialize`, and the explicit `read_data`/
`write_data` transactional pair); inspection is `&self` (`peek`, `fetch_`
instruction`, `peek_stack`, `data_access` construction, and an immutable
`address_space()` accessor exposing only the space's own public read APIs).
`Bus` implements the CPU crate's `CpuMemory` trait (`fetch_instruction`,
`read_data`, `write_data`, `peek_stack`, error type `MemoryFault`) exactly as
the trait's ownership model requires: reads/fetch/peek take `&self`,
read/write take `&mut self`, so RET's pure peek through exclusive `&mut Bus`
ownership is compile-time guaranteed and hidden mutation is impossible by
type. `AddressSpace` still implements `CpuMemory` directly, preserving the
previous API. The CPU still knows no addresses or device layout. No MMIO
exists and no device API was invented; Step 22 attaches devices to this
routing without redesign.

ROM is a real region kind: `MemoryRegion::rom(config, start, contents,
permissions)` is constructed from explicit contents, rejects writable
permissions at construction (`WritableRom` fault), and serves read/fetch only.
`RegionKind { Ram, Rom }` is public with `MemoryRegion::kind()`. Loading
(`initialize`) and guest writes into ROM return the new `ReadOnly` fault
variant, typed with region start/end/kind, before any byte changes. Stack
accesses (`StackRead`/`StackWrite`) now fault with `StackRegion` unless the
containing region is RAM, after the existing word-size check: stack is
RAM-only in both directions. ROM writes are primarily rejected by the normal
R/W/X permission layer; `ReadOnly` is defense-in-depth for loaders.

`tests/bus_cpu.rs` exercises the real `Bus` without clones or closure
reentrancy: the six-instruction program (two LI, ADD, ST, LDZ +4, HALT)
fetches/loads/stores through an exclusively owned `Bus` in both modes to
Halted with r3/r4 = 47, verified stack slot, and untouched bytes beyond; a
second test checks writable-ROM construction rejection, ROM read/fetch,
permission-layer write rejection with unchanged bytes, loader rejection on
ROM, ROM kind, `StackRegion` fault for a stack peek inside ROM, and a
successful word-sized stack write in RAM. Development used standalone probes
to separate test bugs (alignment, wrong expected variant) from real behavior;
no production code changed because of them.

Memory total 18; workspace total 137 tests, zero doctests, zero failures.
Full fmt/strict Clippy/check/test passed; all three host Nix checks and
package build passed. aarch64-linux remains untested. No staging or commits.
Work stops here: Step 22 (generic devices) is next and was not started; MMIO
routing is absent, not faked.

## Initial Consequences for the Roadmap

Since the initial repository was empty:

- **Step 2** (Cargo workspace) must create everything from scratch.
- There is no working code to preserve; the "never rewrite working code"
  rule has nothing to apply to yet.
- The dependency-first ordering still applies: `lazalith-types` and
  `lazalith-diagnostics` come before CPU, ISA, or GUI code, because
  assembler/compiler/OS work will need diagnostics and shared strong types.

## Planned Platform (from `instruction.md`)

```text
Lazalith ISA
    ↓
Lazalith CPU
    ↓
Memory / Bus
    ↓
Virtual Hardware
    ↓
Bootloader
    ↓
LazOS
    ↓
System ABI
    ↓
Lazen Runtime
    ↓
Lazen Language
    ↓
Applications
```

Implementation language: **Rust**. GUI: **SDL3** (host/frontend only, never in
core). Dev environment: **Nix Flakes**.

## Key Rules to Observe Going Forward

1. Follow the steps in order; steps are dependency-aware, not difficulty-ordered.
2. Every step: implement → test → `cargo fmt` → `cargo clippy` → `cargo test` →
   Nix checks → fix → document → next step.
3. Core (CPU, memory, bus, machine, ISA, OS logic, compilers) must stay
   independent of SDL3 and always run headlessly.
4. Use strong types (`Address`, `RegisterIndex`, `ProcessId`, ...) instead of raw
   integers.
5. No global mutable state; ownership stays explicit.
6. Validate before mutation; no hidden mutation in `peek`/`decode`/`inspect`.
7. Structured error types and one shared diagnostics system — no string errors.
8. Do not jump ahead: implement the smallest correct part of a foundation when a
   later step needs it, but do not build future subsystems prematurely.
