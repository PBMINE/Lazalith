# Lazalith — Project State

Last updated: 2026-09-29 (Phase I frozen as lazalith-phase1-100; hardening: 12 clusters audited, 12 confirmed defects, 1279 tests)

## Where the roadmap stands

```text
Steps 1–50    complete, audited, repaired, verified, committed (31dbf86)
Steps 51–59   complete: the Lazen design cluster (documentation only)
Step  60      complete: lazalith-ir, the shared low-level IR (66d29cf)
Step  61      complete: lazalith-compiler, the Lazen frontend (7a4a5b3)
Step  62      complete: lowering from the checked tree to lazalith-ir
Step  63      complete: code generation to a Lazalith object (bb0439b)
Step  64      complete: lazalith-runtime, the runtime a program links against (7ada7f9)
Step  65      complete: the first Lazen program, end to end under LazOS (0910c76)
Step  66      complete: the lazen command-line toolchain (a699c5d)
Step  67      complete: the first Lazen standard library (106c48f)
Step  68      complete: the virtual display device (9188e6a)
Step  69      complete: the virtual input device
Step  70      complete: the LazOS display driver and the Lazen SDK (a8da827)
Step  71      complete: the LazOS input driver and the host adapter (38be9ca)
Step  72      complete: the first graphical Lazen application (831c1a7)
Step  73      complete: the first-party GUI library (ffef9c8)
Step  74      complete: the Lazalith debug API (575841c)
Step  75      complete: machine snapshots (1caf29e)
Step  76      complete: source-level debug information (7c0dd70)
Step  77      complete: the SDL3 frontend (063dd76)
Step  78      complete: the GUI controls (c42d57f)
Step  79      complete: structured diagnostics (e04118a)
Step  80      complete: internal emulator error reporting (cb84333)
Step  81      complete: the C compiler
Step  82      complete: the C runtime
Step  83      complete: assembly, C and Lazen convergence
Step  84      complete: property testing
Step  85      complete: differential testing
Step  86      complete: fuzzing
Step  87      complete: deterministic replay
Step  88      complete: application packaging design
Step  89      complete: Lazen package management
Step  90      complete: lazen fmt
Step  91      complete: expand LazOS
Step  92      complete: expand Lazen
Step  93      complete: optimization
Step  94      complete: optional LLVM backend
Step  95      complete: Nix integration
Step  96      complete: final integration test
Step  97      complete: final graphics test
Step  98      complete: final debugging test
Step  99      complete: final architecture review
Step 100      complete: the final project
```

The 1189 workspace tests all pass, including the 4 in
`crates/lazalith-runtime/tests/window.rs` that build
`examples/window/main.lz` from the repository and run it through the display and
input drivers, and the 16 in `crates/lazalith-gui/tests/panels.rs` that run real
programs on real machines and check every panel the frontend draws, the 15 in
`crates/lazalith-gui/tests/controls.rs` that press every control against a real
machine, and the 13 in `crates/lazalith-gui/tests/diagnostics.rs` that run
programs that do something wrong and check the structured diagnostics that come
back, and the 11 in `crates/lazalith-debug/tests/bug_reports.rs` that state
which faults are the guest's and which are ours.

## Step 79 — structured diagnostics

The GUI consumes structured diagnostics directly. Nothing reads a rendered error
string back to decide what to show: every field a panel draws is a value.

A `RuntimeDiagnostic` **is** a `lazalith_diagnostics::Diagnostic` — the same
stable code, severity, message and source label the compiler produces — plus the
things only a running program has: the guest's program counter, the instruction
there, and a call chain. That is a reuse of the existing diagnostic type rather
than a second one built for the UI.

Codes are `R0xxx`, extending the compiler's `L`/`P`/`N`/`T` namespaces, so a
frontend can tell a program that did something wrong from a frontend that could
not read something without reading either message.

### The call chain is verified, not guessed

The calling convention reserves a word below the frame for the return address and
saves the caller's frame base, so a chain *is* walkable. This walks it by
scanning the stack for words that could be return addresses and **checking each
against the code**: the word must be instruction-aligned, the instruction before
it must be a `CALL` or `CALLR`, that call's target must be real code, and the
word must resolve through the debug table. A data word that merely looks like a
code address does not become a frame, because it would also have to be preceded
by a real call to a real function. A word that fails the check is skipped, and
the diagnostic says how many words were examined so a short trace reads as a short
trace.

`DebugController::instruction_at` is new and is the structured counterpart to
`disassemble`. A frontend that wanted to know whether an instruction is a call
would otherwise have to read the disassembler's text and match words in it —
exactly the parse-a-string approach this step exists to avoid.

### Four bugs the tests found

- **A trapping program was reported as having exited successfully.** The kernel
  discarded every non-syscall trap, so the machine sat in a trap frame with
  nothing able to leave it; the next run found no runnable process and reported
  `Exit { code: 0 }`. A program whose bounds check fired was a program that
  finished. The kernel now reports `KernelServiceOutcome::GuestTrap { cause,
  payload }` with the real cause, the controller marks the process faulted, and a
  `run` stops when it sees that — a fault that does not stop a run is a fault
  nobody ever sees.
- **A program's own `fn read` broke the standard library.** A path with module
  segments fell through to the root module's items, so `std::io::input::read`
  *inside the library* resolved to the program's function and the library failed
  to compile with an argument-count error pointing at its own source. Behind that
  was a second bug: the IR keeps every function in one namespace, so the
  program's `read` and the ABI's `read` were the same name. Syscall
  declarations and syscall call targets are now both `syscall.`-mangled, and the
  root-module fallback applies only to a *bare* name.
- **Three tests were running the wrong program.** They rebuilt an image without
  debug information using entry offset zero, and the runtime links the program
  *after* the library — so offset zero is a library function. They passed only
  because the controller was swallowing the resulting trap. Every stripped-image
  construction now keeps the linked image's own entry offset.
- **The whole stack read failed when one word was unmapped.** A trap frame sits
  on the stack, so its top can be past the end of the mapped region, and an
  all-or-nothing read lost the frames *below* — the ones a trace is for. A fault's
  trace is now read a word at a time and keeps what could be read.

`Diagnostic` also became `Clone` and comparable: its cause moved from `Box` to
`Arc`, because a structured diagnostic that cannot be copied is one every
consumer has to work around, and a GUI keeps several.

## Step 80 — internal emulator error reporting

A **guest fault** is the *program's* mistake. An **emulator bug** is *ours*. They
look the same to a machine and are not the same to a user: the first sends them
to their Lazen, the second sends them to this codebase.

`CpuFaultCause::origin` decides it, and the split is drawn where it is
defensible rather than where it is convenient:

- The guest's: `Decode` (its bytes), `DataAccess`/`Fetch`/`Memory` (it named the
  address), `NextPc`/`Width` (its arithmetic left the architecture's range), and
  the four refusals that are a program doing what it may not.
- Ours: `Instruction` and `OperandLayout` (the *decoder* is ours, and it cannot
  produce an instruction the ISA forbids from correctly compiled code),
  `Control`/`Outcome`/`TrapEntry` (our own state machines refusing to move), and
  `TerminalTrap` (this machine failed to enter a trap and is now stuck).

A guest cause reports **no invariant**, because there is none: a program is
*allowed* to read an address it does not own, and inventing a rule it broke would
report the user's bug as ours.

An `EmulatorBug` carries everything the roadmap lists — subsystem, operation,
guest program counter, instruction, address, machine state, the invariant
violated, and the Rust file, line and column. The location is captured by
`Location::caller()` behind `#[track_caller]`, never typed in: a hand-written
`"src/lib.rs:412"` goes stale the moment the file is edited, and a stale line in
a bug report sends someone to the right file and the wrong line.

A report carries **no source label**, and that is deliberate. A label points into
the *guest's* source; an emulator bug is in this codebase. Pointing a frontend's
"jump to source" at a line of Lazen would be a lie about where the bug is, and
the Rust location is already in the message.

`LazalithMachine::last_emulator_bug` records only faults whose origin is the
emulator's. A program reading past the end of its array must not be reported as
a bug in Lazalith, and a guest fault is deliberately not kept there at all.

`RuntimeDiagnostic` now carries a `DiagnosticKind` that the *machine* decides and
the frontend copies. It started as a match on code strings in the GUI, which is
the same mistake Step 79 removed everywhere else: a frontend whose correctness
depends on the spelling of every code. The kind is a value now, and a new code
cannot be shown as the wrong sort of thing.

## Step 81 — the C compiler

`lazalith-c-compiler` compiles C to the *same* Lazalith IR the Lazen compiler
produces and hands it to the *same* native backend. Nothing here is new
infrastructure: the ISA, the ABI, the IR, the object format, the diagnostics and
the linker are all the existing ones, and a C program and a Lazen program are two
front ends over one machine. Nine of its tests run a C program on the real
interpreter, through the real linker and the real kernel, and check its exit
status.

### The sizes, decided here because nothing had decided them

`docs/lz64.md` left C's `int` and `long` open, pending "a dedicated ABI design".
This is that decision: `char` is one byte, `short` two, `int` four, `long` and
`long long` eight, and a pointer eight. That is LP64 and it matches the machine —
sixteen 64-bit registers and no register pairs. A program compiled here is *not*
portable to an ILP32 host, and `docs/c-compiler.md` says so rather than implying
it.

### What is refused, and why

- **`float` and `double`.** The ISA has no floating-point instruction, so a
  software implementation and an integer wearing a float's name are both
  different projects. A floating *literal* is refused by the lexer and a floating
  *type* by the parser, each by name.
- **A `struct` or `union` larger than a word passed or returned by value.** The
  ABI has one return register and no aggregate argument passing. The type is
  still fine to declare, to hold, to point to, and to read a member of.
- **A variadic function *definition*.** A variadic *declaration* is accepted,
  because `printf` has to be callable, and a definition is refused because its
  body has no way to learn how many arguments it was given.
- **`goto`.** The IR's blocks are built in the order a body is walked, and a
  backwards jump needs a second pass. A loop is a jump the IR can express
  directly.
- **A call through a function pointer.** `CALL` takes a displacement and `CALLR`
  takes one register, and neither reaches a function whose address is only known
  at run time.
- **A call needing more than six argument words.** Four arguments go in registers
  and two on the stack; a wider type uses more than one word.

Every one of those diagnostics names the C construct *and* the machine limit,
because "unsupported" on its own tells the reader nothing about what to change.

### Three bugs the tests found, and what they were

- **A `==` between two integers was refused.** The pointer rules for `==` and
  `!=` were applied to *every* equality, so `c == 0` and `i == 3` — the two
  comparisons a C program writes most — were both errors. The pointer rules now
  apply only when a pointer is on one side, which is the only time a comparison
  is about addresses.
- **A local's initialiser was never written.** The slot was reserved and the
  declaration was otherwise complete, so `int a = 20;` compiled to a slot holding
  whatever the frame held before. A test that checked an *addition* passed and a
  test that checked a *local* returned zero, which is why the end-to-end file
  exists and asserts the values themselves.
- **An eight-byte slot could land four-aligned.** Slots were packed at each
  type's own alignment, so a `char` between two pointers left the next pointer
  four-aligned, and a word-sized access to a four-aligned address is an alignment
  fault. Every slot is now a whole number of words. The padding is a few bytes of
  frame; the fault would have been the whole program.

### Two things that had to move, and why

- **The native backend no longer depends on the Lazen front end.** It was handed
  `lazalith_compiler::lower::Lowered`, so producing code for the backend meant
  importing Lazen's types, and a C program could not be compiled without them.
  `FrameSlot`, `FrameLayout` and `SlotPurpose` moved to `lazalith_ir` — they are
  the agreement between the stage that decides where a value lives and the stage
  that emits its address — and `generate` now takes `(module, frames, entry)`.
- **`ABI_SYSCALLS` moved to `lazalith_os_abi`.** The name-to-number table is the
  ABI's knowledge, and a table only one front end can see is a table the other
  gets wrong. C lowers a call to a library function by name to a syscall, and it
  needs the same table Lazen does.

### Two namespaces, and why they are not one

A lowered C function is `c.<name>` and a lowered syscall is `syscall.<name>`. The
IR has one flat symbol namespace, so without the prefixes a C function called
`write` and the ABI's `write` would be two definitions of one name and the linker
would refuse the object. Both prefixes exist for that reason alone, and a test
asserts they are different.

### A conversion, and where it lives

The IR requires a store's width and its value's width to agree, so a four-byte
value cannot be written to a `char` in one instruction. Every conversion here is
therefore the same two instructions: store the whole word, then read back only the
bytes the target has. Reading back the low bytes *is* the truncation C defines,
and a sign-extending load for a signed target is the sign extension C defines. An
argument conversion goes through the same path as an assignment, so there is one
way to convert rather than two that could disagree.

### What the docs say

`docs/c-compiler.md` states the sizes, the supported subset, every refusal and the
reason for it. A reader who hits a refusal should be able to find the sentence
that explains it.


## Step 77 — the SDL3 frontend

`lazalith-gui` is the window a person debugs in, and it shows the ten things the
roadmap lists by asking the debug API and nothing else. There is no
`&mut LazalithMachine` in the crate and no path to one, because
`DebugController` does not hand them out.

Three crates, because the `unsafe` had to go somewhere and the tests had to be
able to run without a display:

- **`lazalith-sdl3`** is the entire `unsafe` surface of the project: `extern "C"`
  declarations and the safe functions wrapping them. Every other crate keeps
  `unsafe_code = "forbid"`. The build script compiles a probe against SDL3's real
  headers and the library asserts its own struct layouts against the numbers the
  C compiler measured, so "this matches SDL3" is checked rather than claimed.
- **`lazalith-gui`** splits what the frontend shows from how. `view.rs` turns a
  controller into panels of lines and needs no display; `window.rs` draws them
  with SDL3 and knows nothing about registers or addresses; `font.rs` is a 5×7
  bitmap font, because a debugger's output is mostly words and a frontend that
  assumed the host had a font would show nothing on a machine without one.
- **`lazalith-ui`** is the guest-side Lazen widget library, renamed from
  `lazalith-gui` so the roadmap's name could be the host frontend. One is for the
  person, the other is for the program.

### Two defects the tests found

- **The screen would have shown every picture with two channels swapped.** The
  guest writes a pixel as alpha, red, green, blue; SDL wants red, green, blue.
  Uploading the guest's bytes unchanged swaps red and blue, and it looks
  *plausible* because a mostly-grey test image survives a channel swap almost
  perfectly. The conversion is now in the view model, where a test can check it
  against a program that puts a known colour at a known pixel.
- **SDL3's `SDL_Keycode` is four bytes**, not the eight the first version of the
  FFI shim assumed. Only the C probe found that; it would have read the wrong
  field of every key event and reported plausible nonsense keycodes.

### What deliberately did not land

- The window layer is not tested with a real window. The view model is tested
  against real machines, which is where every decision is made; opening a window
  in CI needs a display, and a test that needs a display does not get run.

## Step 78 — the GUI controls

Run, Continue, Step, Pause, Breakpoint and Reset live in `control.rs`, not in the
window. A key press is an event, a debugger's action is a decision, and the layer
between them is where the interesting mistakes are — so it is headless and every
control is tested against a real machine with no window and no synthesised key
events. The window only turns a scancode into a `Control`, and draws the
bindings along the top so a control a person cannot find is not a control they
will press.

Every outcome is a value, and a refusal is a `Refusal` variant with a stable
diagnostic code rather than a message a caller has to read back.

### Two gaps the tests found in what came before

- **`continue` did not continue.** `DebugController::continue_` was an alias for
  `run`, so it stopped at the breakpoints the user was explicitly asking it to
  ignore. It now runs with them suspended and restores them when the run ends.
  A Continue implemented by clearing and re-setting them would be wrong in a way
  nobody would notice until a program set its own breakpoint mid-run, and one
  that *deleted* them would leave the user with a debugger that had forgotten
  where they were.
- **Reset went to the wrong place.** The reset point was taken before the
  supervisor handoff, so a reset returned the program counter to the kernel's
  `RFE` and needed another step before the user was back where they were.
  Loading a program in the debugger now includes the handoff, and the reset point
  is the program's entry point — which is where a person means by "the start".

The first version of the pause control claimed to stop a program it had not
stopped. The controller steps synchronously, so there is no moment between two
instructions to interrupt, and the outcome is now named `PauseRequested` for
what it is: the next run or step takes it at its first instruction boundary.


`crates/lazalith-runtime/tests/window.rs` that build
`examples/window/main.lz` from the repository and run it through the display and
input drivers, and the 7 in `crates/lazalith-ui/tests/ui.rs` that draw with the
widget set and read the frame back, and the 13 in
`crates/lazalith-debug/tests/debug.rs` that drive a real machine through the
controller, and the 7 in `crates/lazalith-debug/tests/snapshot.rs` that save
and restore whole machines, and the 14 in
`crates/lazalith-debug/tests/source.rs` that build a program from Lazen source
and read a source location and a line breakpoint out of the resulting image.

## Step 76 — source-level debug information

The chain the roadmap asks for is now real end to end: Lazen source, AST, IR,
machine code, object, executable, debugger. Nothing in it is a table written by
hand to agree with what the compiler would have said.

- **The IR records statements.** `FunctionBuilder::mark` is called once per
  checked statement, and the mark covers every instruction that statement emits.
  The map is therefore the size of the source rather than the size of the
  program, and a PC inside a long statement names the statement.
- **Code generation turns marks into addresses.** Each IR instruction's code
  offset is recorded against the statement it came from, and the function's
  prologue is covered by an entry that resolves to the function's first
  statement.
- **The object carries the text.** A mapping is a byte offset, and a byte offset
  is meaningless without the text it is an offset into, so `.lzo` version 2 has
  a source-text region and each source record's previously reserved word points
  into it. The two reserved words were already checked to be zero, so no record
  changed size and no table moved.
- **The linker merges and fixes up.** `LinkedProgram` carries a `DebugBlock`:
  sources are merged, and each mapping's address is rewritten from the object's
  section offset to the address that code got in the image.
- **The image carries it.** `.lzx` version 2 spends the eight bytes that held a
  redundant section-table offset on the block's offset and length. The block
  follows the last section, and an image with none has both words zero.
- **The debugger answers.** `DebugController` reads `source_location()`,
  `source_location_at(address)` and `set_source_breakpoint(process, name, line)`.
  A line with no code in it is reported as having none, which is a real answer.

### What this does not do

- The stack is still not a call chain. The calling convention reserves the
  return address below the frame and records no frame pointer, so there is
  nothing to walk. `source_location_at` makes the words on a stack *readable*,
  which is not the same as being frames.
- A line can hold more than one statement, so a breakpoint on a line resolves to
  every address that line's statements start at. A frontend wanting the
  conventional first-statement breakpoint takes the lowest of them.
- Debug information is only as good as the spans the frontend produced. A span
  covering a whole function maps a whole function.

## Step 82 — the C runtime

A C program reaches the machine's facilities through the same objects the Lazen
standard library is built from: the compiler, the OS ABI's syscalls by name, the
object writer and the linker. The runtime is one C translation unit,
`lazalith_c_runtime::C_RUNTIME`, that a program compiles in front of itself.

Sixteen tests in `crates/lazalith-c-runtime/tests/runtime.rs` run real programs
on the real machine and check their output and their exit status. A library
whose functions *compile* but return nothing is worse than no library, so
nothing in the runtime is checked by inspection. `docs/c-runtime.md` is the
reference.

### The kernel had no allocator

`allocate_memory` was numbered, validated, and then reached no service, so it
answered `NotSupported` — a `malloc` that compiled, linked, called the ABI, and
got back 12. There is now a `MemoryService` in `lazalith-os`, routed to by the
same composite that routes the terminal, the display and the input: a service
that answers questions about a world and a service that *makes* memory are
different kinds of thing, and lumping them together would hide which one a call
reached.

It is a bump pool, and that is the ABI's doing. `free` and `realloc` are not
syscalls, so a program has no way to describe a region it no longer wants.
Exhaustion is `ResourceExhausted` rather than `Internal`: the call was well
formed and the machine simply has no more room, which is the one answer a
program can act on.

### A process could not exit with a negative status

`Syscall::Exit` read argument zero as a `u32`, which is what the ABI says, and a
`u32` read out of a 64-bit register is `0xffffffffffffffd6` when the program put
a negative number there. So `return -42` from `main` was *rejected by the
kernel* — a machine on which reporting a negative number was impossible, for a
program that did nothing else. The status is a bit pattern and its sign is not
information: it is the low 32 bits whatever the register's sign bit says, which
is what a host does with `exit(-1)` too. Reading the argument as a word and
keeping 32 bits keeps the width check as well.

### Nine compiler bugs, and the theme

Writing a standard library is a far better test of a compiler than writing
`return 42`. Every one of these was invisible to step 81's suite, and each is a
commit of its own with the reasoning:

- **`|` did not parse.** The precedence walk applied every operator *except* the
  lowest, because `&&` and `||` have their own functions and level zero returned
  before applying anything. `a + b` and `a < b` worked, which is why a suite of
  small programs missed it.
- **`int *f(void)` was built as a pointer to a function.** `int *f(void)` and
  `int (*f)(void)` produce the same list of derivations in the same order and
  mean opposite things; the difference is whether the parentheses were written.
  This is the most common declarator in C and it was wrong.
- **A cast was dropped.** `(unsigned char)` on a signed load is a *different
  value* and `(int)` on a pointer-sized value is a different width, so an
  ignored cast made both mean something the program did not write. A cast is the
  one expression whose type the emitter cannot rebuild — `(T)` names a type, and
  building one needs the typedef and tag tables — so the checker records each
  cast's target and the emitter reads it back by where the cast starts.
- **`char` was signed whatever the program said.** The type checker wrote
  `signed: *signed || true`, and the parser dropped the signedness keyword before
  `char` reached it, so `unsigned char` was the same type as `char`. The
  standard library does not care except through `strcmp`, which casts both
  operands to `unsigned char` *precisely* so a byte above 127 sorts above `'a'`.
- **A widening conversion read bytes the value never had.** An `int` widened from
  a `char` stored one byte into an eight-byte scratch and read four back, three
  of which were whatever the frame last held. It clears the scratch now. And the
  extension kind and the result type are separate decisions: how far to extend
  follows the source, what the result *is* follows the target. Getting that
  backwards typed `(int)(unsigned char)c` as unsigned, so `(int)'c' - (int)'d'`
  was a subtraction of two unsigned values, wrapped, and compared `>= 0`.
- **`a && b` answered for `a` alone.** The lowering branched around the right
  side and stored a constant, so the right side was evaluated for its side
  effects and its value discarded: `1 && 0` was `1`. That is the loop guard of
  every C string routine in the library.
- **`||` branched the wrong way.** It stored the answer for "the left side was
  enough" and then sent a *true* left side to the right side — `&&`'s rule. So
  `a || b` evaluated `b` exactly when `a` had already settled it, and used `a`'s
  truth negated. `malloc`'s first line of real work is
  `if (heap_block == 0 || heap_used + wanted + 8 > heap_size)`, so with a null
  heap pointer the guard was skipped and the allocator wrote its block header to
  address zero.
- **`sizeof` was a machine word.** Not "a word on this target" — a hardcoded `8`
  standing in for the size of whatever was being measured, so `sizeof buffer` for
  a `char[16]` said sixteen bytes were eight. The size is measured in the
  *checker*, because a `sizeof` operand names a type and the emitter has no
  typedef table to build one with. That also exposed the next one.
- **A subscript stepped by four bytes into a `char` array.** `element_of` knew
  about pointers and not arrays, asked for the element of a pointer, found none,
  and defaulted to `int`. C treats `a[i]` as `*(a + i)` whether `a` is a pointer
  or an array, so an array has an element type now too.
- **`return expr;` did not convert.** C converts a returned expression to the
  function's declared result type, and it did not, so a `long` expression
  returned from an `int` function arrived as a full 64-bit register.
- **A local array's initialiser was never written.** `char buffer[16] = "abc";`
  is a *copy* — the literal is in program space and the buffer is in the frame,
  and a program that wrote to its own buffer would have been writing to the
  string table. The null is written too, and left off when the array is exactly
  the literal's length, which is what C says.

### An unknown name was an `int`

The checker's `name` asked every source of a name in turn and, finding none,
returned `CType::int()` — the same thing as assuming the program was right about
a declaration nobody wrote. So `printf("hi")` type-checked as *calling an `int`*
and the program was told ``int` cannot be called`, which names the wrong thing in
the wrong place. Every stage past the resolver now says the name is not declared,
and returns an `int` only so the rest of the program is still checked and still
gets its other diagnostics.

### A status is zero or it is not

The C runtime compared `status < 0` after every syscall, which is a check that
can never fire: an ABI status is a `u32`, `InvalidHandle` is 9, and a signed
comparison calls it a success. A `fwrite` to a handle the process does not own
reported a count of bytes nobody had written. There is a test that a refused call
is visible.

### Two absences, and neither is an oversight

- **No `printf`.** A variadic body is the one thing this C cannot write, because
  a variadic definition is refused. Printing is a call per piece: `print`,
  `print_line`, `print_decimal`. Declaring `printf` so a call would *check* was
  tried and is worse — the program compiled and the failure came back from the
  linker as an undefined symbol, naming neither the reason nor the file.
- **No `fopen`,** and this one is an ABI gap rather than a compiler limit.
  `write` and `read` report into an `IoResult` the caller supplies, `seek`
  reports through a pointer, `stat` fills a record — and `open` reports the new
  file's handle in the *outcome payload*, which is the second register of the
  return. No calling convention in this machine hands a caller the second
  register, so the handle reaches hand-written assembly and nothing else. The
  native-shell fixture reads it correctly; `fs::open` in Lazen and a C `fopen`
  would both read a slot the kernel never wrote.

  The fix is one argument — the handle as an out-parameter, like its siblings.
  It touches a documented ABI, a fixture that reads the payload, and the index
  conventions the validation errors use, and it is worth doing as its own piece
  of work rather than at the end of another step. A C program can use the handles
  it already has: 0, 1 and 2 are the console, and `fread`, `fwrite`, `fputs`,
  `fseek`, `ftell`, `fclose` and `fflush` all work on those.

## Step 83 — assembly, C and Lazen convergence

The three front ends were already converging, and this step is the check that
keeps them converging rather than the work that made them. Step 61 built the seam
on purpose: the native backend takes a `lazalith_ir::Module` and a frame layout
rather than a Lazen `Lowered`, and the name-to-syscall table lives in
`lazalith_os_abi` where both front ends can see it. Step 81 recorded those as two
things that had to move, and they were enough.

`docs/convergence.md` is the reference, and it ends with the list of what a
second ecosystem would look like — a `.lzc` object format, a linker that treats
C modules differently, a `main` wrapper that skips the entry sequence, a boot
path of its own — so that a change doing one of them is recognisable as that and
not as an improvement.

Four tests in `crates/lazalith-c-compiler/tests/convergence.rs`:

- **All three produce Lazalith objects**, read from the bytes, so a second object
  format fails at the earliest point it could be caught.
- **All three run the same program to the same answer.** One program written
  three ways, through one linker, into one image, on the real machine, compared
  against each other. A test that only *ran* them would pass with one of them
  wrong.
- **Three objects link into one image.** Convergence is not sameness: three
  objects with three different entry names become one image with one code
  section, which is why a C object and a Lazen object can end up in the same
  program.
- **One entry sequence serves three entry names.** The sequence is generated by
  one function and only the name it calls varies, so the three code sections are
  byte-identical and the difference is a relocation rather than a byte of code.

### The program is deliberately dull

It returns 42, writes nothing, and links no library. A C program that printed
something would need the C runtime and a Lazen one the standard library, and a
failure would then be a failure of one of those rather than of the route. The
shortest possible program is the one that isolates the claim.

### The one place the three may differ

The *name* of `main`, and only because the IR has a single flat symbol namespace.
`fn.` is the backend's prefix for everything it emits, so a C function called
`write` cannot be the ABI's `write`; `c.` is the module name the C compiler chose,
so a C function and a Lazen function of the same name cannot collide. Those two
prefixes are the whole of the per-language difference in the object, and they are
namespace hygiene rather than a dialect.

## Step 84 — property testing

The step asks for property tests over seven areas: instruction encode and decode,
register behaviour, width conversions, memory, the object format, the parser, and
the IR. All seven have them, and the example it gives —
`decode(encode(instruction)) == instruction` — is the ISA's first one.

### The generator, and why it is not a framework

`crates/lazalith-properties` is about two hundred lines and has no dependencies.
Property testing is a habit rather than a dependency, and this repository has no
third-party Rust dependencies at all: every crate is `no_std`, `unsafe`-free, and
built by a Nix expression that vendors nothing. A framework would have been the
fast route and the wrong one.

What a property test needs from a framework is four things, and this has all four:
generation, repetition, reproducibility, and a seed corpus. The corpus is a *fixed*
list — a suite that draws a different 500 cases every run finds a different bug
every time and is never the same test twice, while a fixed list means a failure
found today is still found tomorrow and a fix shows up as a test that stopped
failing rather than as a quiet change of inputs. The seeds are the Fibonacci
numbers, which has no meaning beyond being a sequence nobody would choose twice.

It does not shrink, and says so. What it does instead is report the whole failing
case and the seed that made it, with a one-line re-run. A bad shrink is worse than
none, because it reports a case that does not fail.

### Two bugs, and what they say about the rest of the suite

- **The C lexer panicked on a non-ASCII character.** `at` is a byte offset and
  every span is built from one, and the lexer's "make progress" step advanced it by
  one *byte*. On a three-byte character that leaves `at` in the middle of it, and
  the next token's span is not a character boundary, so `slice` panicked. A C file
  with an `é` in a comment is not exotic; it is a file somebody's editor produced.
- **Sign extension from a 64-bit source was not the identity.**
  `WordWidth::sign_extend` used the standard `(x ^ sign) - sign`, and that identity
  is wrong at the full width: its `sign` is `1 << 63`, and
  `(0xffff_ffff_0000_0000 ^ 1 << 63) - (1 << 63)` is all ones. The existing table
  test missed it because it tries six values per width and none of the six for 64
  bits is the one that breaks. Which is the argument for this step in one line.

Neither was in an area that had no tests. Both were in areas with *exhaustive*
tests, which is the point: a hand-written list is the list somebody imagined, and
step 82's compiler bugs came from exactly the same place — `a + b` and `a < b` were
the only comparisons anyone wrote, and `|` did not parse.

### What each area gets

| Area | File | Properties |
|---|---|---|
| Instruction codec | `lazalith-isa/tests/properties.rs` | the round trip both ways, a length that does not depend on its operands, a refusal that says why, arbitrary bytes decoded or refused |
| Width conversions | `lazalith-types/tests/properties.rs` | a zero extension is the low bits, a sign extension fills from the source's sign bit, the two differ exactly when the source is narrow and negative, truncation is idempotent, addition commutes, subtraction undoes addition, multiplication distributes, every result fits, an impossible width is refused |
| Registers | `lazalith-cpu/tests/properties.rs` | a register holds what was written at the machine's width, the last write wins, writing one disturbs no other, every register starts at zero, the two accessors agree, a refused write reaches nothing |
| Memory | `lazalith-memory/tests/properties.rs` | a write reads back as what the machine can hold, a byte write leaves its neighbours alone, unmapped faults *and* mapped does not, a read-only region refuses without changing anything, regions that overlap are refused while regions that abut are not |
| Object format | `lazalith-toolchain/tests/properties.rs` | the round trip, encoding is a function of the object, the bytes begin with the magic, arbitrary bytes are an object or a refusal, every proper prefix of a valid object is refused |
| Parsers | `lazalith-compiler` and `lazalith-c-compiler` | random text is a program or a refusal and never a panic, a refusal says something, compiling twice gives the same answer, every diagnostic is reported |
| IR | `lazalith-ir/tests/properties.rs` | what the builder accepts the verifier accepts, each refusal actually refuses with the right kind, building twice gives the same module, every block a terminator names exists |

### Three properties that were wrong before they were right

Left in the files as they are, because being wrong about them is how two of the
bugs above were found.

- "The two extensions differ iff the value is negative" is false for a 64-bit
  source, which sign-extends to itself. Stating the rule rather than the summary is
  what turned up the real bug.
- "Every prefix of a program is refused" is false of the prefix that stops one byte
  short of the final newline, which is a perfectly good program.
- An empty file is a translation unit rather than a mistake: the front end answers
  "is this C", the lowering answers "is this a program". Asserting otherwise would
  have asserted the wrong layering — and it did, until the test failed and the
  layering turned out to be right.

## Step 85 — differential testing

`crates/lazalith-machine/tests/differential.rs` runs the same program on two paths
and compares what each of them did, after every step rather than at the end.

### What is compared, and what is not

The step asks for a `ReferenceInterpreter` checked against *an optimised emulator*.
There is no optimised emulator — step 93 introduces one, and until then this
repository has exactly one instruction executor, which `LazalithMachine` calls.

Building a second executor here to compare against would be step 93 written badly,
and a differential between two implementations written in the same sitting catches
less than the harness it needs anyway. So this builds the *harness* and points it
at the two genuinely independent paths that exist:

| | what it is | what a disagreement would mean |
|---|---|---|
| **bare** | `ReferenceInterpreter` against a flat byte array, stepped directly | — |
| **machine** | `LazalithMachine` against a real `AddressSpace`, a `Bus`, a region table, a device and a clock | the bus, the address space, the region permissions or the clock disagree with the processor |

The processor code is shared, so this cannot find a bug *in* the interpreter. Step
93's differential will, once there are two. What it can find is a disagreement about
anything the machine adds on top: an address translation, a data size, a permission,
a fault classification, a clock.

The step's list is addressed one item at a time, and where an item cannot be
compared the reason is in the test:

- **registers, PC, flags, memory** — compared on every step, plus a per-address
  memory window so a store that lands in the wrong place is caught.
- **devices** — the bare path has no devices, so a program that writes to one is
  compared for its *permissions* on both paths and its *output* on the machine,
  against the byte it stored. A console on one path and nothing on the other is not
  a difference to report; it is the difference between a processor and a machine.
- **virtual time** — the clock belongs to the machine and not the processor. A step
  does not move it and the scheduler advances it by a quantum per process, so the
  property is that it moves *only* when the driver moves it, which is what makes a
  replay reproducible. A step-count property would have been wrong.
- **process state** — not comparable here: a process is a kernel object, and a bare
  processor has none. The kernel's process state is compared by step 96's
  integration test and by `lazalith-os`'s own suites.

### Two things the harness had to get right about the two paths

- **The same memory permissions on both.** The flat array started with none, so a
  program storing into the code region *succeeded* on the bare path and was refused
  by the machine — a difference that is the region table working correctly. The flat
  path now answers the same three questions the region table answers, over the same
  regions, including the device window.
- **The same notion of "where the program is".** The machine *enters a trap* and its
  architectural `pc` becomes the trap vector; the bare processor *returns* a trap
  request with the resume point in the outcome. Comparing the two `pc`s compares a
  vector with a program counter. Both observers now report the **guest's** pc — the
  trap event's `resume_pc` and the outcome's `resume_pc` — which is the thing a
  program can observe.

Their vocabularies for *why* also differ, and the comparison is on coarse categories
rather than on a field-by-field translation: the bare processor has one `Width`
cause for a divide by zero, an overflow and a misaligned displacement, and the
machine splits it into four. Both are `width`.

### The corpus

Curated programs for what a random draw will not reach — a call and a return, a trap,
a fault on unmapped memory, a store into a read-only region, a write to the device,
and an arithmetic chain that sets and clears every flag — and *random* instruction
sequences over step 84's fixed seed corpus for everything else. A random program
that faults is compared too: a fault is an answer.

The operand lists in the curated corpus are written out rather than generated. A
corpus built by asking the format what it wants is a corpus of the *builder's*
opinion rather than of a program somebody meant — and the first version of this file
had `Addi` with two operands, which is not an instruction this ISA has.

## Step 86 — fuzzing

`lazalith-fuzz` asks one question of the ten things in this repository that read
bytes they did not write: *what happens when the bytes are wrong?* `docs/fuzzing.md`
has the target list and the reasoning; the short version is that the step's
requirement — "malformed data must not silently corrupt state" — is a statement about
*answers*, so every target states the answers it will accept and a target that
refused everything would fail a second test rather than pass quietly.

No `cargo-fuzz` and no coverage instrumentation, for the same reason step 84 wrote its
own property generator: this repository has no third-party Rust dependencies. A
coverage-guided fuzzer finds deeper bugs in less time on one target; a deterministic
campaign finds the same class every time, runs on every commit, and never flakes, and
`--iterations N --seed S` makes a failure a command line. For a repository whose
central claim is reproducibility, that is the better trade, and it is a trade rather
than a free win.

### What it found

The object reader, on the first campaign, through a check that was itself wrong.

The target demanded that a file re-encode to itself byte for byte. The fuzzer
produced a file that read cleanly and re-encoded to eight bytes *more*. It was a
perfectly valid object: one byte in a string table had changed from a NUL to
something else, which merged the adjacent names `text` and `_start` into the single
name `text\x01_start`. The file is legal, the reader was right to accept it, and the
writer is right to spend eight more bytes on a seven-byte-longer name.

So the check was the bug, and the property worth stating is **idempotence**: one
pass through read-then-write reaches a fixed point, so a linker that rewrites a file
twice produces one file. A valid object in a non-canonical spelling is still accepted,
which byte-identity would have rejected.

Two smaller things the harness got wrong the same way, both now fixed and both worth
recording: a parser target counted a refusal as a failure when refusing malformed
source is the correct answer, and an empty string table was treated as malformed
input rather than as "there is no program", which has to produce a machine that
stops at the start rather than a refusal to build one.

## Step 87 — deterministic replay

A run is reproducible from four things:

```text
binary          what the program is
architecture    which machine it runs on
initial state   where that machine starts
input log       what the outside world did to it
```

`lazalith_debug::ReplaySession` takes exactly those four and produces a `Trace`.
The same four twice give the same trace field for field; a state or a log written
down and read back replays to itself; and `Trace::difference` names the first field
two traces disagree about, because a `PartialEq` on a twelve-field struct reports
*that* two runs differ, which is the one thing someone reproducing a bug does not
need to be told twice.

Three decisions in it are the interesting part.

**Devices are not in the initial state.** The obvious design snapshots every device.
This one does not, because a device is a *model of the outside world* and the outside
world is not in the snapshot — the input log is. A device is a function of the input
it was given and the virtual time it has been ticked, and the artifact carries both,
so snapshotting a device would store a cache of something derivable and create a
second source of truth to disagree with the first. What replaces it is stricter: a
trace records how many events reached the program, so a replay that fed a device
differently produces a different trace and is caught rather than assumed right.

**Virtual time is one cycle per instruction.** A scheduler that advanced the clock by
a host tick or a wall clock would make the trace depend on the machine it ran on,
which is the one thing a reproduction must not do. There is a test that asserts
`trace.time == trace.steps` for exactly this reason.

**A log whose order is ambiguous is refused rather than sorted.** Two events on the
same cycle could go either way, and a log that ordered them arbitrarily would replay
into a *different* run than the one recorded — the worst failure available, because
the reproduction would be confidently wrong. `InputLog::push` refuses an event that
arrives before the last one, and the log is a list rather than a map.

### Where it sits, and what that costs

Below the kernel: a machine, a program image, and a log. No supervisor, no
filesystem, no process table. That is deliberate — the kernel's own state would have
to be in the artifact for the promise to hold, and until it is, a replay that
included it would be a promise about something this implementation does not check.
The initial state carries everything a *program* can observe: the processor, the
trap vector, and every region's bytes and permissions.

A by-product of writing it: `DeviceManager` gained `device_mut`. A manager whose
devices could only be reached through a full-state restore had a gap in it — a host
feeding an input device had to reconstruct a queue in order to append to it.

## Step 88 — application packaging

`docs/lazen-packages.md` defines the third unit, the one a person shares:

```text
application   lazen.toml + src/     what a person authors   (step 56)
executable    .lzx                  what LazOS loads        (existing)
package       .lza                  what a person shares     (this step)
```

The roadmap says not to finalize the format before understanding the requirements, so
the requirements were read out of the tree rather than invented, and the design
cites the code for each one. Three findings drove it:

- **`SpawnProcess` names a path, and nothing turns a path into an executable.**
  `syscall.rs:853` validates a path and a length; `LazalithKernel::start_image`
  takes an `LzxImage` by value. The middle is missing, and it is the reason the
  design specifies a *resolution rule* and not just a container.
- **A bare `.lzx` has nowhere to put a name or a version**, so it cannot answer
  "which application is this, and may I start it" — which is all an installer, a
  package manager, and a capability check need. That is what the package header is.
- **A package must never say anything about execution that the `.lzx` does not
  also say**, because that is a second source of truth. So the container stores one
  complete `.lzx` verbatim, stores the manifest verbatim rather than re-serializing
  it, and resolves by *content* rather than by file name — which is also what a VFS
  with no executable flag on a node can actually do.

Two constants currently contradict each other — `LZX_MAX_FILE_SIZE` is 4 MiB and
`DEFAULT_MAX_FILE_BYTES` is 1 MiB — so a large application cannot be installed as a
single VFS file today. The design leaves that open rather than guessing, and records
it as a gap with the three reasonable answers and none of them chosen, because which
one is right depends on what real applications weigh and no application has been
published yet. The same applies to version *compatibility*: `major.minor.patch` is
fixed by step 56, but nothing has been promised to a user of an application, so there
is nothing to be compatible with.

### What the step owes its reader

`crates/lazalith-os/tests/package_premises.rs` asserts the four design claims that can
be checked, so the document cannot go stale silently: an image is self-describing and
round-trips (rule 1's assumption), two identically built images are the same file
(the central gap, which stops being true the moment someone adds identity to `.lzx`),
the only way into the scheduler is an image (the first driver), and the size
contradiction still points where the design says it does. A test for "this does not
exist" is a comment, so the missing path→image step is recorded as a gap rather than
asserted.

## Step 89 — Lazen package management

Local package and dependency support, kept small, with no registry. Four things, each
one a promise rather than a feature.

**The container** (`crates/lazalith-os/src/lza.rs`) is step 88's design in code: a
header, a resource table, the resource names, the application's name, the manifest
verbatim, one complete `.lzx`, and the resource bytes. Every offset is *derived* from
the counts and the lengths, and a reader that finds a disagreement refuses rather
than tolerates — the property step 86's fuzzer earned the hard way, in the object
format, and it is the first test in `packages.rs` that touches it.

**The resolution rule** (`crates/lazalith-os/src/resolve.rs`) closes step 88's first
driver. `SpawnProcess` names a path; `resolve` turns the bytes at that path into
either a package or an image, **deciding by content and never by name**. Three reasons
for that, each about something that already exists: `FileMetadata` has no
"executable" flag, a user can rename a file, and deciding by name would make `install`
a naming convention and `run` a privilege question. The property that matters is that
a package and a bare executable are interchangeable to everything downstream —
`Resolved::into_image` is the whole of what a caller needs.

**The manifest reader** (`crates/lazalith-toolchain/src/manifest.rs`) is deliberately
not a TOML parser. This repository has no third-party dependencies and a TOML parser
is not a side effect of a package step, so it reads exactly the four tables
`docs/lazen-applications.md` specifies and **refuses everything else** — a table
array is an error with a line number, not a best-effort reading. That is a real
limitation and the right one: a format this reader accepts and another tool also
accepts is worth more than one it half-implements.

**The resolver** takes a list of `name -> manifest text` pairs and no filesystem, so
the directory walking stays in the CLI and the interesting questions — which version
wins, what a cycle reports, whether a missing dependency says where it looked — are
answerable without a disk. A requirement is a floor *inside one minor series*: `0.1`
means `>=0.1.0, <0.2.0` and never admits `1.0.0`, because a resolver that let it
through would silently resolve across an incompatible change, which is the one thing
a version number exists to prevent. A missing dependency is an error listing every
directory searched, and a cycle is reported as the path `hello -> a -> b -> a`, which
is what `docs/lazen-modules.md` asks for.

Two commands: `lazen pack` writes `NAME.lza` next to the manifest, and `lazen deps`
prints the resolution or the reason there is not one. `pack` refuses a manifest that
pins a word width its image does not have rather than producing a package that lies
about the only thing it describes. There is no `install`: where a package lands is
the system's decision, and step 88's design says a file recording where it goes has
to be rewritten on every move.

### Bugs found while building it

Three, all in code written for this step and all found by the tests for it:

- **`VersionRequirement::accepts` admitted a different major.** `0.1` accepted
  `1.0.0`, which is precisely the failure a version number exists to prevent. Now a
  requirement never leaves its major.
- **`LzaResource` carried file offsets.** A package built in memory therefore could
  never equal the same package read back, because the built one had no offsets and
  the read one did. Offsets are now decode-time values in a private row type, and a
  resource's identity is its name and how much it contributes.
- **A resource the manifest did not declare was reported as a duplicate.** Two
  different mistakes, one error. The table is now reconciled against the manifest
  name by name, and the error says which one it was.

`lazalith-fuzz` grew an eleventh target for the package reader, since step 86's
harness is exactly the right tool for a new container. 60,000 inputs on it alone and
30,000 across all eleven are clean. 1057 tests pass, and fmt, Clippy, check,
`nix flake check` and `nix build` are green.

## Step 90 — the Lazen formatter

`lazen fmt` and one canonical style, in `docs/lazen-formatting.md`. Two things about
it are the actual content of the step, and one of them is a bug the step found.

**It changes whitespace and nothing else, and that is asserted rather than claimed.**
The formatted text lexes to the same token stream as the input, with the same
comments, in the same order. A formatter that changed a token would be a compiler, and
this repository has one. Working from the token stream is what makes the guarantee
cheap: literals are tokens, so there is no "did I mean to rewrite this string"
question to answer once per language construct, forever.

**It is idempotent, and that is what makes it usable.** `fmt(fmt(x)) == fmt(x)` —
a formatter that is not idempotent fights the next `fmt`, and a repository that says
"run `lazen fmt`" with no idempotence test is asking people to check `git diff` every
time for no reason. Every rule is either forced by the tokens or a *preservation* of
something the author wrote, and a preservation is stable by construction.

### The four things the style preserves, and why

Each is a case where a token stream cannot say what the author meant, and a formatter
that guessed would be making a claim about the program:

- **`-` is subtraction or negation.** Both are the same token. The formatter reads
  the source's own spacing — no space before and a space after is binary, a space
  before and none after is unary — and preserves that, falling back to "binary after
  something that can end an expression" when the author was not clear. The lexer has
  keyword tokens of its own, so "after a keyword" is a fact and not a guess. The
  alternative was a formatter that only worked on a file that already compiles, at
  exactly the moment a person most wants to format one.
- **A blank line is grouping.** Deleting every blank line in a file deletes its
  structure, which is the difference between a formatter and a refactoring. One is
  kept, runs collapse to one, and one before a comment is kept too.
- **A call broken across lines stays broken.** There is no width in a token stream, so
  keeping the author's choice is the only thing possible — and the alternative is a
  formatter that joins a 40-line call into one unreadable 400-character line.
- **`struct S {}` and `if a { b(); }` and `) {` stay as written.** An empty body means
  "nothing here", and a brace on its own line is a claim about the body the author did
  not make.

### The bug the step found, in the lexer

The formatter could not keep a comment, because **the lexer discarded them** — it
skipped over `//` and threw the text away. The two obvious fixes are both wrong: scan
the source for comments in the formatter (a second lexer, and a second thing to get
wrong), or print from the AST (a parser, and the same problem as the sign). So
`Lexed` grew a `comments` field. Nothing in the compiler reads it and nothing needs
to; the cost is one vector, and the benefit is exactly one answer to "where are the
comments".

The comment's text keeps its leading whitespace, because a space after the `//` is
content — `//     lazen run main.lz` is an example in a comment, and trimming it
deletes the example.

### The check that says the most

`examples/window/main.lz` — the largest program in the repository, using nested calls,
array types with lengths, casts, comparison chains, one-line blocks, multi-line calls
and comments inside blocks — is a **fixed point of the formatter**. `lazen fmt --check`
passes on it byte for byte, and so does it on `examples/hello/main.lz`. The
repository's own code is the style's worked example, and a test asserts it, so the two
cannot drift apart without something failing.

`fmt` and `fmt --check` are separate because the first rewrites a person's file and a
script must not do that by accident. A file that does not lex is refused with the
lexer's reason and is not touched; a file that does not compile but does lex is
formatted happily.

1071 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 91 — expand LazOS

`docs/os-expansion.md` has the whole step, and it opens with a table that says how far
each of the roadmap's eight areas got — because a step that quietly did a tenth of one
of them and said nothing is worse than one that says so. Summary: the capability gate,
four filesystem operations, spawn-by-path with the package's capabilities attached, a
timer device, and the threads that already existed made *queryable*. Networking and
audio got nothing, and a test asserts the absence so a syscall cannot appear without a
device behind it.

### The capability gate is the step

`docs/lazen-applications.md` says a manifest's `[permissions]` declares the
capabilities an application intends to use, and `docs/lazen-packages.md` recorded that
nothing enforced them. This is the enforcement, and it is worth more than the other
seven areas together: it is the one that turns a package from a transport into a
promise.

The rule is an asymmetry, and the asymmetry is the design. **A process started from a
package may only make the syscalls its package declared; a process started from a bare
`.lzx` may make all of them.** A package is the untrusted unit — it arrives from
somewhere and its author wrote down what it needs. A bare executable is the trusted
unit: it is what `lazen build` produced, what the boot ROM loads, and what a person
runs on their own machine, and a gate on it would break the kernel, the init shell and
every test in exchange for protecting a program that already has the whole machine.

So the gate is a property of *how a process was started*, decided in one place
(`LazalithKernel::start_resolved`), which means there is no way to start a package and
forget to restrict it.

Three details that took thought:

- **The check is on the syscall, not the device.** `write` is a console call when the
  handle is a terminal and a file call when it is not, and a program that declared
  `console = true, filesystem = false` and then wrote to a file is exactly what the
  declaration should catch.
- **The refusal is the same whether or not the arguments were valid**, and it happens
  before the arguments are read — so a process without the capability learns nothing
  about the ABI by calling `open` with a bad pointer.
- **A capability this build cannot enforce is not granted**, which is the *opposite* of
  a package record's unknown bits being held rather than refused. Both are deliberate:
  a reader that refused a newer package could not install it at all, and a kernel that
  granted a capability it did not implement would be promising something it does not
  have.

The gate lives in `lazalith-os-abi::capability` rather than in the kernel, because the
permission bits are a contract between a program and the system that runs it. The
package format re-exports the ABI's four constants rather than declaring its own — one
set of bits, not two that happen to agree.

### The rest

- **`rename` is a re-link, not a copy**, so a handle open on the old name still reads
  the same bytes. The cycle check asks whether the *moved node* contains the
  *destination's parent*; the other direction of that question refuses every rename,
  because the destination's parent is usually an ancestor of the node being moved. That
  was a real bug the test found on the first run.
- **`remove` refuses a directory with children**, because removing a directory means "I
  am done with this" and emptying one is a different act that shares a name.
- **`truncate` grows with zeros**, because a program that seeks past the end must read
  zeros and not another file's bytes.
- **`walk` reports the root as `/`** rather than as the empty path, which is not a path
  anything can open — also a real bug the test found.
- **The timer exists because `Time` is a trap.** A program that wants to *measure*
  something cannot afford a measurement that interrupts it, so the cycle count is a
  device read by loading from an address. The register is 64 bits on both targets: a
  counter that wrapped at 2³² on a 32-bit target would be a clock that lied after
  about seven minutes. Writes are refused rather than ignored, because a program that
  believed it had reset the clock and had not would measure a span it thought it
  controlled. And it is in the device snapshot, because a restored program must not read
  a time that never happened.
- **Threads were already per-thread state.** A process carried a thread per thread id
  with its own register file, and the scheduler already moved a machine's state into a
  *named* thread — so the state is real and was already load-bearing. What is missing is
  creation: there is no `spawn_thread`, every process still has exactly one, and
  `assert_eq!(process.thread_count(), 1)` is in the tests so a process that grew a
  second thread with nothing creating one would fail rather than pass quietly.
  `Process::thread_state` was added so a caller can ask for a thread's registers and be
  told `None` rather than handed the first thread's.

### The layering bug this step exposed

The permission bits were being declared in the package format, which sits *above* the
ABI. The capability gate needs them at the ABI layer, because they are a contract
between a program and the system that runs it — and `lazalith-os-abi` cannot depend on
`lazalith-os` without inverting the whole stack. So the four constants moved down to
`lazalith-os-abi::capability` and the package format re-exports them, which is the same
"one definition, not two that agree" rule the ISA, ABI, and syscall lists already
follow. Found by trying to write the gate and finding the dependency pointed the wrong
way.

1093 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 92 — expand Lazen

`docs/lazen-expansion.md` is the step. The roadmap lists six things to consider
and then says, in the same breath, "Do not add features merely because another
language has them" — so the step is not picking six features, it is picking which
ones this repository has a use for and saying why in terms of something
checkable. One was built; five were not, and for each of the five the document
names the *gate* — the thing that would have to exist first, and which is worth
building on its own merits whether or not the feature ever follows.

| area | verdict | the gate |
| --- | --- | --- |
| pattern matching | **built** | none; the arm grammar *is* a comparison chain |
| generics | not yet | a container to be generic over, and monomorphisation in the backend |
| advanced collections | not yet | an allocator the SDK can reach |
| concurrency | not yet | `spawn_thread` — step 91 built the per-thread state, not creation |
| advanced modules | not yet | a module that is genuinely a facade |
| macros | not yet | compile-time evaluation in the IR |

### The `match`, and the half-right argument it replaced

The compiler already refused `match`, and its refusal said why: *"Lazen v1 has no
enums, so there is nothing to match."* That was half right, and the half that was
wrong is the whole step. **Right:** with no enums, records, or destructuring, a
pattern cannot bind a name or pull a field out of a value, so a pattern can only
be a value to compare against. **Wrong:** everything on this platform returns a
tagged integer — a syscall returns an `i64` status, a string lookup returns a
byte and an index, a `for` over a view returns a count — so the entire platform
is shaped like an untagged union and every program consuming one writes the same
`if status == 0 { } else if status == 1 { } else { }` chain.

Which is a `match` with the comparisons written out **and the scrutinee written
out too**. So every one of those chains calls the scoring function once per arm,
and the language was asking the programmer to remember something they did not know
they had to remember. That is the bug, and it is why the sugar earns its place.

Four decisions, each of which could have gone the other way:

- **The scrutinee is bound to `$match_<offset>`, and the binding cannot be
  captured.** A `$` cannot appear in a Lazen identifier — the lexer cannot
  produce one — so the binding cannot shadow a name in an arm body, no arm body
  can refer to it, and no program can declare it. Hygiene by construction rather
  than by a renaming pass, and `tests/lexer.rs` is written so it fails if a `$`
  ever becomes identifier text.
- **A `bool` pattern is the condition, not a comparison.** `==` is an integer
  operator here, so `flag == true` is not an expression that exists. Rather than
  teach `==` about `bool` to make one desugaring work, `match flag { true => … }`
  becomes `if flag { }` — because that is what the pattern *means* and the
  language already had the expression.
- **The `else` arm is required.** With no enums, exhaustiveness is not checkable
  and pretending otherwise is worse than not having it; the alternative is a
  `match` that falls off the end, which makes an unhandled status a silent no-op
  — a bug that does not reproduce. Its own code, `P0109`, says exactly this.
- **A `match` in statement position is a binding plus an `if`**, so its arms may
  `return` and need no value. A value conditional in this language has no frame,
  so a value `match` may not bind a name; the parser decides from context, exactly
  as it already does for `if`.

**There is no `Match` node in the AST.** It desugars in the parser, so the type
checker sees the `if` chain, the IR is the IR, the lowerer knows nothing, and the
verifier checks what it always checks. The alternative threads a variant through
five places and buys the right to disagree with itself in each. One consequence
is worth naming: *the formatter would have broken `match` if it printed from the
tree* — step 90 formats tokens precisely so comments survive, so a tree-printing
formatter would have rewritten a person's `match` as a `$match_4` binding, a
different program in their file that would not even compile. `tests/format.rs`
asserts no `$match` ever reaches a file.

### The five refusals

Each is refused for a reason from *this* repository, not from taste:

- **Generics** need a uniform type. The built-in types are ten concrete widths
  with deliberately different ABI stories; there is no user container to be
  generic over; and a generic function's frame is not a frame of anything until
  the type is known, so monomorphisation is a real design question. Gate: a
  container.
- **Collections** need somewhere to grow to. The OS has `AllocateMemory`; the
  Lazen ABI has no allocation call, and step 91's gate has no memory capability
  to put one behind. Then reallocation is a capability question, and a view into
  a growing buffer is invalidated by a push, which the current model has no way
  to express. Gate: a length-carrying allocation call.
- **Concurrency** is blocked on the OS, not the language: step 91 left per-thread
  state real and creation missing, and creation needs a stack mapped inside the
  process (the virtual-memory work), a second entry point (a loader concept the
  image format lacks), and a scheduler that round-robins threads as well as
  processes. Gate: `spawn_thread`, worth having because the platform otherwise
  cannot use a second core for anything.
- **Modules**: the obvious features are `pub use` and globs, and the SDK says
  neither is needed — every symbol is called at its own path and there is no
  facade to re-export *through*. A re-export earns its place only when a module
  is a facade over another, and there is not one. Globs are refused more sharply:
  a glob makes `white` a name whose origin is three files away, and it turns
  "you misspelled it" into "it was never imported". Gate: a facade.
- **Macros** have the most misleading "already half built" argument here. Step 90
  gives the toolchain a token stream with comments, which is the substrate a
  hygienic expander needs — and nothing evaluates anything at compile time. A
  macro that cannot be evaluated at compile time is a function call, and a
  function already is one; so this is either compile-time evaluation, or a call
  with an extra name to grep for, or a preprocessor, which this language's own
  omissions list rules out. Gate: a constant folder in the IR, which is worth
  having because it is what makes `const` more than a named literal.

`docs/lazen-syntax.md` section 13 no longer lists `match` as an omission and
documents the four decisions, and the test pinning section 13 now checks the
inexhaustive-`match` diagnostic instead of the old "no such feature" one. The
old `P0102` stays defined and unused: it is a published code, and reusing it for
something else would be worse than leaving it dead.

1119 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 93 — optimization

`docs/optimization.md` is the step. The roadmap lists four possibilities — optimized
interpreter, faster memory paths, instruction caching, JIT — and one rule: *every
optimized implementation must preserve reference behavior*. The rule is the step.
The four items are not four features, they are four answers to "where does the time
go", and answering that honestly is most of the work.

**Measured first.** `crates/lazalith-memory/examples/decode_speed.rs` runs the same
straight-line program on a bus that caches decoded instructions and on one that does
not, and reports instructions per second. Two decisions worth naming: no threshold is
asserted (a timing assertion fails on a busy machine and teaches a team to ignore
the test that matters), and no benchmarking framework is used (the project takes no
third-party Rust dependencies, and a dependency is worse than a number printed by a
program anyone can read). The result: **1.67x on lz32, 2.00x on lz64.** So decoding
was about a third of the time — enough to be worth a cache, not so much that a cache
substitutes for a better interpreter.

**The cache, and the rule it is built around.** A cache may skip work, never a
check. So the checks are not in the cache: `Bus::checked_fetch` does every check an
instruction fetch owes, and the table is consulted only after they all pass. One
copy of those checks, called by both paths, so they cannot drift apart. Two tests
hold it up — one plants an entry at an *unaligned* program counter on purpose, since
a test that only warms valid addresses proves nothing, and requires the fetch to
fault; the other asks a 64-bit bus about a 32-bit fetch and requires the
configuration fault. Move the lookup above the checks and both fail.

**A bug the first version had.** `invalidate_range` originally scanned all 512 slots
on every write. Correct, and a tax on exactly the programs a cache should help: 512
comparisons to invalidate one instruction, per store, in a loop that stores. It now
walks the addresses the store could have touched — `length / 8 + 2` steps, two for a
byte store — which is exact rather than approximate. The tests check it by result,
not by cost: a byte store inside the second of four cached instructions forgets
exactly one and leaves three, and an eight-byte store starting four bytes into an
instruction forgets two. A walk that visited the wrong addresses would either leave
a stale instruction or clear a neighbour it had no business clearing.

The overlap rule is about *ranges*, not addresses, so a store beginning in the middle
of an instruction invalidates it — the case an equality test gets wrong, and the one
a self-modifying program actually hits. `writing_an_instruction_makes_the_cached_copy_
be_ignored` runs a program, lets the cache fill, overwrites a decoded instruction with
a *different* one, and requires the new thing to happen.

**Turning the optimization off.** `Bus::reference` is a bus with no cache — what the
interpreter did before this step. It exists so the step's rule can be tested rather
than asserted: one program on both buses, comparing the program counter and all
eight registers after every instruction. A timing comparison cannot catch an
optimization that changed behaviour; this can, because it compares everything a
machine can be observed doing. It is also the pattern a future optimization should
follow.

**Faster memory paths: not done, and what blocked them.** `step` validated the fetch
twice — `step` called `validate_fetch`, then handed the bytes to `step_bytes`, which
called it again. Fixed, and a real win, but small. The architectural state is cloned
per instruction, and that is almost certainly the largest cost left; it is also where
a change is *observable*, because a program can fault and what the faulting
instruction already wrote must not survive. The commit discipline is what makes a
fault transactional and it is load-bearing for the trap behaviour in `docs/isa.md`. So
the next entry in that direction is a register write barrier with a rollback journal,
not a clone removal — a design with a real correctness argument, in its own step
where the argument can be read.

**JIT: no, and not because it is too big.** Because it would be optimizing the wrong
thing, three times. The target is not the bottleneck yet: the interpreter spends its
time in validation, cloning, and memory bookkeeping, and a JIT over an IR that does
not carry the information to skip them produces unoptimized machine code. The ISA is
the platform: a JIT needs a register allocator, a calling convention, and legal
encodings, and step 94 is already showing what that costs — a second implementation
of the semantics, trusted to agree. And there is no compilation story for the guest:
a JIT would make guest execution time depend on how much host CPU it got, which makes
an emulator non-deterministic, undoing the determinism steps 86 and 87 built so a
failure can be reproduced from a log.

Also untouched, deliberately: `AddressSpace` has no cache (only the bus caches, since
the bus is what a machine runs on); `step_bytes` neither consults nor fills the cache,
because it takes bytes from somewhere other than memory and a cache answering from a
stale table when handed deliberate bytes would defeat the differential fuzzer; and no
compiler change at all, so the compiler's output is the same object files as before —
the testable form of "the cache does not change what runs".

1132 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 94 — the optional LLVM backend

LLVM is optional, so this step adds none. What it adds is the thing that makes
"optional" mean something structural rather than aspirational, and a test for each of
the roadmap's two rules.

**The shape was missing its join.** Two frontends already feed one IR — that is the
diagram's left half and it exists. The right half is a fan-out, one IR into more than
one backend, and what was missing was any place for a second backend to attach:
`lazalith-codegen` exported a free function `generate` and nothing else. A second
backend under that shape would have had to be threaded through the compiler, the C
compiler, the toolchain, and the command line — which makes it *mandatory* in the only
sense that matters, because every caller would have to know it exists. So this step
adds `pub trait Backend`, `NativeBackend`, and `available_backends()`, with `generate`
now a free function that calls `NativeBackend`, so every existing caller is unchanged
and the seam is a place a backend can register rather than a layer the codebase has
learned to route around. `the_seam_is_real_because_the_native_backend_produces_the_
same_object` compares the two objects byte for byte and the frame records exactly, so
if someone routes around the trait later, a test fails.

**"LLVM must not become mandatory" is now a test.** It is the rule about the future,
and a promise about the future is worth exactly as much as the test enforcing it. One
test walks `Cargo.lock` and fails on any crate whose name starts with `llvm` —
`llvm-sys` being the obvious first step toward a real backend, and exactly the step
that would make a forty-megabyte C++ library a build requirement of a platform whose
entire premise is that it has none. Another walks all 26 manifests and fails on any
registry dependency, walking them rather than checking a list because a list is the
thing that goes stale. If either test ever needs removing, the removal *is* the step 94
review — written into the test so whoever reaches for it reads that sentence first.

**"Native infrastructure remains the foundation"** is held by three checked things:
`available_backends()` has one entry and it is `native`; the free function every
caller uses *is* the native path; and the native backend still produces a working
program with a frame for `main` and a non-empty object. The last matters most — a step
that added a seam and broke the one working backend would satisfy the first two and
fail it.

**What "optional" has to mean in a build system.** The temptation is to read optional
as "there, but nobody uses it", and in a build system that reading is wrong, because
the cost of an unusable dependency is paid by everyone who builds the project. So there
is no LLVM code in the tree at all. A real one, if ever built, would be a crate behind
a feature that is *off by default* — so `nix flake check` and `nix build` never see
it — with its own entry in `available_backends()` compiled only under that feature.

**The finding, which is the real output of the step.** An LLVM backend cannot just
hand the IR to LLVM. The IR is a register-machine IR with virtual registers and a
frame the lowerer computed, so a backend would have to, in order: allocate host
registers; match a *host* calling convention when the one on record is a *guest* one,
which is where a JIT is born and why step 93 refused one for the same reason; decide
what each instruction means on a host whose arithmetic differs, re-implementing
Lazalith's trapping, alignment, and width behaviour plus checks for everything LLVM
would do differently; and emit an object the project linker can read — and
`lazalith-toolchain`'s reader is written for Lazalith's own format. That last point
is what makes this a project rather than a feature: an LLVM backend that only worked
for the native target would be a different toolchain, not a backend, and would not run
on the guest at all.

**Why not built anyway.** It cannot be verified against anything — a second backend is
a second implementation of the semantics, and the native one is checked by the suite
and, since step 85, differentially against a bare reference interpreter; a second
implementation nobody can check is not a faster platform, it is a second thing to be
wrong. It buys nothing the platform needs: step 93 measured the time going to
validation, state cloning, and memory bookkeeping, not code quality, and LLVM
optimizes code generation. And it would be the project's most expensive dependency,
added for a capability nothing asks for, in a platform whose stated values include a
zero-dependency toolchain.

A reader who assumed "add LLVM" meant "call LLVM from the codegen" now knows it means
"decide what Lazalith's arithmetic means on a host whose arithmetic differs, express it
twice, and prove the two expressions agree."

1137 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 95 — Nix integration

All three commands work. What changed is that the coverage list is now *checkable* and
that `nix build` produces something anybody can run.

| check | covers | what it does |
| --- | --- | --- |
| `workspace` | Rust, emulator, OS, assembler, linker, Lazen, C compiler, SDL3, tests | builds every crate, runs the whole suite, and compiles the SDL3 crate's C probe |
| `formatting` | style | `cargo fmt --all --check` in a clean sandbox, not the developer's checkout |
| `clippy` | style | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| `program` | Lazen, and the artifact | runs the *installed* `lazen` on two example programs |
| `devShell` | the development environment | builds the dev shell |

**`nix build` produced a package with no program in it.** That was the step's biggest
actual defect: the install phase named eleven library crates and no executables, so
the package contained rlibs that nothing outside a cargo workspace could use. A
package with no program is a way of checking that a build succeeds, which
`nix flake check` and `cargo build` between them already did. The install phase now
discovers what to install — all 24 rlibs, every executable, `lazen` among them — and
then *asserts* `lazen` exists, so a build that stopped producing the command fails
with a sentence saying so instead of succeeding and shipping nothing. Discovering
rather than listing is the same reasoning as the documentation filter below.

**The documentation list can no longer be one short.** The source fileset listed
thirty-odd `docs/*.md` paths by hand, and a hand-written list is a list that will be
one document short the day somebody adds one — and it fails *silently*, because a
document missing from the source tarball is not a build error, it is a document
missing from a release. Every document added since was a chance to get it wrong. It
is now a `cleanSourceWith` filter over the directory, and all 34 are found
structurally: adding a document cannot be forgotten because there is nothing to add
it to.

**`program`: the system being used, not merely built.** Everything else in the flake
builds the system; this check runs it. It formats both examples, type-checks both,
reports the version, and asserts the binary it tested is the one this build produced
rather than one that happened to be on `PATH` — because a check that silently tests
some other build's `lazen` has stopped checking. `lazen check` on a real example is
the only check that uses the installed artifact rather than the source tree, and it
runs the lexer, parser, resolver, and type checker over a program that uses the
standard library.

**`devShell`: `nix develop` is verified by building it.** A dev shell that has quietly
stopped building is the reproducibility failure nobody notices until somebody new
clones the repository, and building it as a check is the only way it gets noticed on
the day it breaks. It evaluates on both `x86_64-linux` and `aarch64-linux`.

**Three bugs found while making the checks real**, each of which failed the *build*
rather than a test, and each of which the next person would otherwise make: `[ -x ]`
is true for a directory, so the install loop tried to install `release/build` and
failed with coreutils' `install: omitting directory` — a confusing way to learn that a
shell test needs `-f`; `"$out/bin/lazen"` inside `passthru` is the *literal string*
`$out/bin/lazen`, because Nix does not substitute `$out` outside a derivation's own
attribute set, so the check using it was handed a path that did not exist; and
escaping `${...}` as `''${...}` in a `runCommand` script passes it to the shell, which
then tries to expand a Nix attribute path as a shell variable. None would have been
caught by `nix build` alone, because the first only appears with the new install phase
and the other two only inside the new check. Adding a check that cannot pass is the
only way to find out whether it can.

`nix flake check` reports all checks passed on `x86_64-linux` and evaluates the dev
shell for `aarch64-linux` as well. It prints a warning that `aarch64-linux` is
incompatible on this machine and therefore omitted, and that warning is left in place
rather than silenced by narrowing the system list to the one that happens to work
here. The SDL3 frontend is covered in the *build* environment and not only the dev
shell, because a package that builds in `nix develop` and not in `nix build` is broken
for everyone who installs it.

The honest limit: this step verifies the *system* is reproducible, not that a
*program* is — which is step 96's job. A program that misbehaves on one machine and
not another is not something a build sandbox can see, and pretending otherwise would
be the wrong lesson to end a reproducibility step on.

1137 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 96 — the final integration test

`crates/lazalith-runtime/tests/pipeline.rs` walks the whole chain — Lazen source,
compiler, object, linker, `.lzx`, loader, process, syscalls, virtual hardware,
emulator — in seven tests that assert something at each arrow rather than only at the
end. A test that checked only the final output would pass for a program that got
there by accident: with the wrong exit code, or without ever reading its image back
from the file format, or with a syscall count of zero, which would mean the console
output came from somewhere other than the kernel.

**The boot-and-run path had no test under it.** The sequence that boots a machine,
hands off from the supervisor kernel, drives the kernel loop, and reports output and
status lived in the `lazen` command — defended in a comment saying the runtime crate
"deliberately does not own a machine, because a library that started one would be a
library with a global". The first half of that is right. The second is a
non-argument: the function *returns* the machine, so it owns no global, and "a
library with a global" describes a design that was never the one in question. The
consequence was concrete — **the one code path in the project that boots a compiled
program had no test under it**, because the only way to reach it was to build the
binary and run it as a subprocess. Every other layer had hundreds of tests; the layer
where all of them meet was covered by a shell invocation. It moved to
`lazalith_runtime::run`, with the step budget, and both `lazen run` and the new test
call it.

**The syscall count was being inferred from an event that no longer existed.** The
first implementation counted `MachineEvent::Trapped` in the step the kernel returns,
and a program printing two lines reported one trap. The reason is the kernel's own
design and it is correct: `LazalithKernel::step` *replaces* the step that trapped
with the step that returned from the syscall, so the trap event has been consumed by
the time the step is handed back. The runner was counting something the kernel had
already overwritten. The count is now what the kernel reports — a dispatched syscall
comes back as `Return`, the last one as `Exit` — which is a better measure anyway, since
a trap count would also have counted a guest's `TRAP`, which is not a syscall and which
the kernel deliberately reports under a different name.

The two tests worth expanding are the ones a weaker suite would skip. The runner takes
*bytes*, not a decoded image, and decodes them with the same reader a person's `.lzx`
gets — a run that used the in-memory image it was built as would skip the serialiser,
and a wrong serialiser would pass right up to the day somebody built a program and ran
it. The test goes further, re-serialises the decoded image and runs that too, requiring
the same answer, so the reader cannot be the only thing being trusted. And
`a_program_computes_its_own_output_rather_than_repeating_a_literal` runs a loop that
sums one through ten, prints `55`, and returns 55: Lazen's `while`, the type checker's
arithmetic rules, the lowerer's comparison and branch, the code generator's encoding of
both, the emulator's execution of them, and the syscall that puts the result on a
console, with every stage load-bearing for the number on screen.

Not covered, and not pretended to be: the C half of step 94's two-frontends diagram
(the C front end has its own end-to-end tests and converges on the same object
format, but no single test runs a C program and a Lazen program under one kernel);
`.lza` packages, since this runs a bare `.lzx` which is the *trusted* half of step 91's
design; and performance, because `Finished` reports an instruction count and nothing
asserts anything about it — step 93 measured throughput in an example for exactly this
reason, and a number in a test is a number that fails on a busy machine.

`lazen run` and `lazen test` still work through the shared runner, which the CLI's own
36 unchanged tests confirm.

1144 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 97 — the final graphics test

`crates/lazalith-runtime/tests/graphics.rs` runs real Lazen graphical programs through
a real kernel on a real machine, and `docs/graphics-test.md` records what it found —
which is more useful than a passing test would have been.

**The defect.** The first version asserted the *pixels*: a program drew a white block
on a black window and the test read the framebuffer back out of the machine. Every
frame came back as zeroes. The investigation: the device reported a plausible guest
address (`0x40dd08` for a 16×8 window); six runs of one program produced six different
address pairs, so nothing deterministic was involved; and a program made to print its
own `as_ptr()` and read its own first pixel back reported

```text
program says its framebuffer is at 0x4250888   device recorded 0x40dd08
program reads its own first pixel: 255
```

So the drawing is correct — the program wrote white and read white back through its
own pointer — and the address the device was *given* is a different address from the
one the program's array occupies. The recorded address reads back as zeroes even
though the stack region starts at `USER_STACK_START = 0x0040_0000` and translation is
identity, so it is the right page. Established: the device is handed an address that is
not where the framebuffer lives, and it varies between runs of an identical program.
Not established, and not guessed: which of the two is wrong — the compiler handing the
SDK a view onto a frame slot, or the SDK passing a different `as_mut_slice()` result
to `display_open` than the program uses. Both fit the evidence, and separating them
needs a lower-level test than this step had budget for.

**What the API does about it.** `Finished::presented` reports `pixels: Option<Vec<u8>>`,
and the `None` is deliberate: a host-side read that fails is reported as "there was a
frame and I could not read it" rather than as a page of zeroes. Those are different
facts, and in this build they are genuinely different, because a frame of zeroes is
exactly what a program which drew nothing produces — so returning zeroes for a read
that never happened would make the failure invisible to the test written to catch it.

**What the seven tests do assert**, each a claim the program can be held to: the
repository's own `examples/window/main.lz` boots, opens a 48×32 window, presents at
least two frames and exits zero; three programs with three geometries each get their
own numbers back from the device; a program clears, fills, then reads its own canvas
and finds white; a program with no keys never moves its block; three `d` keys over
three passes make the program print `12`; three `x` keys — which it ignores — make it
print `0`; and a program that never opens a window presents nothing, so the positive
cases are not vacuous. The two input tests are the step's real claim and are built so
neither can pass for the wrong reason: "responds to input" is checked by the program's
*own output*, a number it computed from the keys it read, not by "the frame changed",
which the example's block would satisfy by moving on its own; and the matching negative
shows a device that fed the program any event at all would not be enough.

**The SDL3 boundary, stated precisely.** SDL3 is not linked, and not because it was
inconvenient: it needs a display server, and every machine this project's tests run on
is headless, so a test needing one would fail everywhere the suite actually runs. This
file pins the *contract* instead — a presented frame is four bytes per pixel in the
guest's own memory at an address the device was given, not a copy and not a host
allocation; the record's geometry is the geometry the program asked for; the present
count is how many times the frame was shown. A frontend bug and a program bug show up
in different places, and the tests assert the side this project owns. What is not
covered is a window appearing on a display, and the document says so rather than
implying otherwise.

The one-line summary: the drawing path, the display syscall path, the input path, and
the response to input are all demonstrated end to end from real Lazen source, and the
address the display device records does not match the address the program's framebuffer
lives at — recorded as an open defect with a reproduction rather than worked around in a
test.

1151 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 98 — the final debugging test

`crates/lazalith-debug/tests/fault_recovery.rs` runs a Lazen program that indexes out of
bounds, all the way from source text to a diagnosis a person could act on, in seven tests
with one per item the step lists. A test per item is the point: a debugger that recovers
six of them and a debugger that recovers none look identical to a user who only ever saw
the one they needed.

The program is `let value: u32 = values[index as usize];` with `index` equal to 7 in a
four-element array, so the fault is the **bounds check the compiler emits** rather than a
`TRAP` the programmer wrote. That choice is what makes the source mapping testable: an
explicit `TRAP` would fault just as well but would land on a line nobody wrote, and a
debugger that pointed at a line the user could not see would look like a debugger that
worked.

**The guest's program counter is not the machine's**, and that is the most important thing
this step pins. While a guest trap is being handled the machine's own program counter is in
the kernel, and the guest's is in the trap frame; `RuntimeDiagnostic::guest_pc` is where the
guest's address lives. A frontend that read the register file would point a user at the
kernel's next instruction and call it their own program. Two tests depend on the
distinction and would fail if it were dropped, and one of them asserts the two are *not*
equal, with both addresses in the failure message.

**The fault is the program's, not the emulator's.** `docs/isa.md` separates "the program
did something wrong" from "the emulator is broken", and the test checks the
`DiagnosticKind` rather than matching a code string — a frontend that worked the kind out
by matching a code would be a frontend whose correctness depends on the spelling of every
code, and a new code would silently be shown as the wrong sort of thing. An
out-of-bounds index must be a `GuestFault` and must not be an `EmulatorBug`, or a person
would go hunting for a CPU bug when their own index was out of range.

The rest, item by item: the source file is named as the compiler was given it, the line and
column are ones a person can use, and the recovered line is checked to be *the line that
indexed* — a debugger that named a different line would be confidently pointing at the
wrong statement, which is worse than naming none. The instruction at the guest program
counter is rendered, and it is a `TRAP`, which is the bounds check itself. The registers
are all readable, including ones the program never wrote. And the stack carries its honest
limit: the calling convention reserves the return address below the frame but records no
frame pointer, so there is no call chain to walk, and `StackView::has_call_chain` is false —
a debugger that printed addresses and called them a call stack would be showing a heap of
numbers. The diagnostic's own scan depth is bounded and non-empty, so a user looking at one
frame can tell whether there was one or whether the scan stopped.

The control runs the same program with an in-range index: the session reaches
`Exited { code: 0 }`, the controller has no diagnostics, and the console shows the program's
output. A debugger that reported a fault for a healthy program would make every other test
here worthless.

1158 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 99 — the final architecture review

The step lists thirteen properties the finished platform must have.
`crates/lazalith-cli/tests/architecture.rs` checks all thirteen mechanically, because a
review that reads as prose and concludes "the layers look right" is worth about as much as
the last time somebody drew a dependency diagram. Most of the rules are properties of the
manifests, so they are checked by reading the manifests; the rest are checked by *using* the
thing.

| rule | how it is checked |
| --- | --- |
| CPU does not depend on SDL3 | `lazalith-cpu`, `-isa`, `-types`, `-memory` declare no `sdl3` |
| Machine does not depend on compiler | `lazalith-machine`, `-cpu`, `-os` declare no compiler |
| Compiler does not depend on the emulator | the compiler, the IR and codegen declare no `lazalith-machine` **as a library** |
| GUI does not access CPU internals | `lazalith-gui` declares no `lazalith-cpu`, and SDL3 reaches exactly two crates: itself and the GUI |
| Lazen does not bypass LazOS | a program builds, links and validates, and the compiler's syscall table *is* the ABI's |
| C does not bypass the ABI | the C compiler numbers syscalls from the ABI crate |
| Assembly can reach low-level functionality | the assembler accepts a supervisor-only program, and the machine is what refuses to run it |
| LZ32 and LZ64 share architecture infrastructure | the same bytes decode under both configurations, and no layer has a `32`/`64` twin |
| ISA not duplicated | exactly one crate writes the instruction table |
| ABI not duplicated | exactly one crate writes `ABI_VERSION`, and the compiler reads the ABI from the ABI crate |
| Syscalls not duplicated | one table, every number distinct, one definition |
| Diagnostics centralised | each crate that emits a diagnostic depends on the one diagnostics crate |
| Guest faults and emulator bugs distinguishable | different *values* in one enum, with different codes |

**The review found two real things, which is the argument for doing it mechanically.**

First, `lazalith-gui` depended on `lazalith-cpu` — and never used it. A manifest is the
*only* way to reach a crate's internals, so an unused dependency in the presentation layer
is one import away from violating "the GUI does not access CPU internals", and nothing but
a test would ever have noticed. Removed.

Second, `lazalith-codegen` declared `lazalith-machine` as a **library** dependency while only
its *tests* used it — the tests run generated code in a machine, which is exactly what they
should do. As a library dependency it put an emulator inside a code generator, which is the
"compiler does not depend on the emulator implementation" rule waiting to be broken. Moved
to `[dev-dependencies]`, and the checker now reads the two sections separately, because a
check that cannot tell them apart cannot enforce either rule.

**Three of my own premises were wrong, and the tests said so.** The GUI *should* depend on
SDL3 — it is the presentation layer SDL3 exists for; the rule is that SDL3 must not leak
*below* it. `lazalith-os` does not depend on the diagnostics crate, and should not: it is
the layer where an error is a value a caller must handle rather than a message a person
reads, so it is absent from the list of crates that report diagnostics. And the C runtime
does not need the ABI crate, because it *declares* its syscalls by name and the C compiler
numbers them — so the check belongs on the crate that does the numbering, which is the
whole point of the rule rather than a detail of it. A test written from a wrong premise
failing is the cheapest possible way to find out.

`dependencies()` reads only `[dependencies]` and not `[dev-dependencies]`, with the
reasoning written down in the function: a code generator may legitimately run what it
generated in a test, and what it may not do is depend on the machine from its library.

1171 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## Step 100 — the final project

`docs/platform.md` is the step, and it is the first one that adds no feature: a finished
project's honest boundary is part of finishing it. The claim the diagram makes is that three
languages reach one machine, so that is what the step *tests*.

**The one claim, in one test file.** `crates/lazalith-c-compiler/tests/platform.rs` is the
only place where all three languages go through one path and are compared. The individual
languages were already tested — `pipeline.rs` for Lazen, `end_to_end.rs` for C, the
toolchain's assembler tests for assembly — but each in its own test file, in its own crate,
against its own copy of the boot sequence. The claim is narrow and mechanical: the same
program, in three languages, produces the same arithmetic, the same exit status and the same
bytes through one object format, one linker, one loader and one machine. The program sums
one through ten, *computed* rather than stated, so the number on screen and the status the
process exits with are both the program's own arithmetic. The three paths are written out
separately, with a comment saying why: the claim is that the paths coincide, and a test that
shared the code between them would be asserting that they were the same code.

**The assembly case found something.** The program is unrolled, and the reason is the ISA
rather than the test: the instruction set has `BR`, `JMP` and `CALL`, and **no conditional
branch**, so a hand-written loop cannot be written without a branch the architecture does
not have. That is worth stating plainly rather than working around quietly, because it is
the sharpest real difference between the three layers: the compilers have a conditional
branch to give a program, so `while` costs a Lazen or C programmer nothing, and at the
assembly layer the same loop is not expressible and the cost lands on whoever is writing. It
is a gap in the ISA, not in the assembler — the assembler faithfully emits everything the
ISA defines — and the next ISA revision is where it would be closed.

**The entry sequence taught the file the return convention by being wrong first.** The
startup does `CALL <entry>; MOV r1, r0`, so a program's result comes back in r0. The first
version of the assembly program put the sum in r3 and exited 0, which demonstrates the
convention better than a comment would have.

**The layer table is not a hopeful caption.** Every row names the step-99 check that would
fail if the boundary were crossed, so the diagram is backed by thirteen tests rather than
by a drawing.

**What the platform does today**, all exercised by a test: build and run Lazen, C and
assembly to one executable format on one machine; a working OS with a loader, processes, a
scheduler, a filesystem, a terminal, a display driver, an input driver and a timer; a
capability system where a package may only make the syscalls it declared; a debugger that
recovers file, line, column, instruction, guest PC, registers and stack from a runtime fault
and keeps a guest fault apart from an emulator bug; graphics through the SDK and the OS
with the pixels in the guest's own memory; deterministic emulation with a decode cache, a
differential harness, replay and a fuzz target set; and a reproducible build whose dev shell
is itself a check.

**What it does not do**, each with the document that says what it would take: generics, a
heap and a growing collection (the ABI has no allocation call); threads (step 91 built the
per-thread state and deliberately not creation); networking and audio (no device, so no
ABI); an LLVM backend (a second implementation of the semantics, and the project's premise
is that it has no dependencies); a JIT (step 93 measured where the time goes, and it is not
code quality); the display's frame address (step 97 recorded the evidence rather than
working around it); and a window on a real display (SDL3 is not linked in tests, because
every machine they run on is headless).

**That is the roadmap's own instruction applied.** "Do not treat this document as a request
to generate 100 steps of code immediately. Treat it as a state machine for development."
Each of the hundred steps is implemented, tested, fixed and documented in
`docs/project-state.md`, in the order the steps happened — and the last entry says what is
missing, which is the state a state machine is in.

1176 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.


---

# HARDENING PHASE

The first 100 steps are complete. This phase is an adversarial audit, and its record
is `docs/hardening.md`. Eleven clusters are done and **nine confirmed defects** have been
found: seven in the C frontend, all the kind a green suite cannot see — a program using
`unsigned int` ran and produced wrong answers, every `unsigned long` constant panicked
the compiler, and one silently became zero — one in the runtime, which is the graphics
defect `docs/graphics-test.md` had recorded with a reproduction and a *wrong theory* for
a whole cluster, and one in the debugger, where restoring a snapshot and pressing
continue reported the program finished after a single instruction.

Twenty-two *test* defects were found alongside them, which is the phase.s more
interesting result: on this platform the implementation has been more reliable than
the tests describing it.

## H5 — ISA, CPU and machine: clean

`crates/lazalith-cpu/tests/hardening_arithmetic.rs` compares every arithmetic
operation against a model written from the ISA document rather than from the
implementation — explicit masking, Rust's own integer arithmetic, a hand-written
sign extension — after every instruction, through a real interpreter and a real
memory. A test that computes its expected value by calling the same `WordWidth`
method the interpreter calls is a test that agrees with the bug.

Over 10,000 comparisons across 13 operations and both widths, with half the values
drawn from a table of interesting ones because those are where a width or sign
mistake shows up: **no defect**. The fault cases fault, and the two widths agree
wherever the widths say they should.

Two *test* defects were found and fixed on the way, and they are recorded because a
test that asserts the wrong thing is a defect too: the division-by-zero case
treated `u64::MAX` as zero — it is zero at 32 bits only when truncated, and a good
divisor at both — and the model's own sign extension did `1i64 << 64`, which does
not exist, so a model that cannot express a case quietly skips it.

## H9 — runtime and C frontend: four confirmed defects

`crates/lazalith-c-compiler/tests/cross_frontend.rs` runs nine algorithms through
both front ends and compares the printed value and the exit status. That is the only
oracle this project has — two implementations of overlapping semantics sharing one
IR, one lowerer and one code generator — and a disagreement is a bug in one of them.
It found four.

**1. A widening conversion sign-extended from the wrong bit.** `int v = -3; long
w = v; if (w < 0)` answers `0`. The wrongness is *selective*, which is what makes
it dangerous: the same value is correct in `0 + v` and in `v / 2`, because arithmetic
is emitted at the full width and never came through the conversion path. The
conversion's scratch slot is pre-cleared and then the load that reads it back was
built with the **target's** width and the **source's** signedness, so the extension
came from bit 63 of a zero rather than bit 31 of the value. The comment directly
above that line already said "how far to extend follows the source"; the width was
the target's. Fixed by reading the scratch at the source's width and keeping the
load's *declared* type the target's. The first attempt — storing at the target's
width — is the other thing that would have worked, and the IR verifier correctly
refused it, which is worth recording: the alternative would have been a codegen
change for no reason.

**2. `unsigned int` was a signed `int`.** `unsigned int v = 4294967293u; if (v >
2147483647u)` answers `0`, and printing it prints `-3`. `BaseType::Int` had no
`unsigned` field — `Short` and `Long` both had one, and the most-used integer type
in the language did not — so there was nowhere for the parser to put the flag. The
specifier merge then applied a written signedness to `char` only, so a bare `int`
after `unsigned` overrode the whole type.

**3. A bare `signed` was a `char`.** Found by the fix for #2, in the arm that read
`if signed { Char } else { Int }`. C says a bare `signed` is `signed int`; it was a
one-byte **char**, so `sizeof` said 1 and every operation on it happened at 8 bits.

**4. `long int` and `unsigned long int` lost their width.** Found by a specifier
table rather than by reasoning, with the same merge as its cause: `long` set the
width keyword and the following `int` overrode the whole type. C says `long int` is
a `long`.

The merge now lets a width or signedness keyword win over a following bare `int`,
and a 23-spelling table, now permanent as `hardening_c_types.rs`, checks each type
three times — once for `sizeof` (a width), once against a value above `INT_MAX` (a
sign), and once by comparing an all-ones value `< 0`. All four have a regression
test that fails without the fix, verified by running each against the unpatched
implementation before patching.

**What none of this says about Lazen.** The differential agrees with itself and
s with hand-computed values on every algorithm; Lazen's conversions were not
touched, and the C fixes change no Lazen output. The lesson is about the *absence*
of the differential during the first hundred steps: the C frontend was tested
against itself and against the shared backend, and never against C.

1189 tests pass, and fmt, Clippy, check, `nix flake check` and `nix build` are green.

## H4, H2, H6, H7, H8, H12 — clean; H2, H3/H10/H11 again — five more

`docs/hardening.md` has the full record. In summary:

- **H4 (code generation)**, `crates/lazalith-codegen/tests/hardening_frames.rs`: six
  frame and calling-convention programs — deep recursion with several live
  temporaries per frame, nested calls with five arguments, recursion with the ABI's
  full six-word argument budget, values live across calls, repeated outgoing-argument
  use in a loop, and frame reuse. All return the right exit status.
- **H2 (Lazen frontend)**, `crates/lazalith-compiler/tests/hardening_semantics.rs`:
  twelve cases on precedence and scoping. All correct.
- **H6 (memory)**,
  `crates/lazalith-memory/tests/hardening_validate_before_mutation.rs`: twelve
  refused-access cases, each checking every byte of the regions involved afterwards.
  Validation is complete before mutation in all of them, and the boundary is pinned
  from both sides — the last valid word of a region is writable, and the next one is
  not.
- **H12 (object format)**,
  `crates/lazalith-toolchain/tests/hardening_object_format.rs`: 408 objects
  round-tripped exactly, and every prefix and every single-byte corruption of eight
  of them either refused or validated. The reader never reads past the file.
- **H2 again (C constants)**: three more confirmed defects, in code nothing had ever
  asked about. Every `unsigned long` constant **panicked** the compiler (`1ul` as
  much as `18446744073709551615ul`); a `ul` constant above `LONG_MAX` **silently
  became zero**; and the range check derived a constant's type from a *zero* value,
  so `5000000000` and `0x80000000` — both legal C — were rejected with a diagnostic
  naming a type they never had. The specifier matrix H9 promised is now permanent at
  23 spellings measured three ways each.

Two of those three were stacked: the panic fired first and hid the range check's
wrong answer, so fixing the crash unmasked the bug underneath it.

**The ratio is the finding.** Seven implementation defects, fifteen *test* defects,
and every test defect was a test that would have passed or failed for the wrong
reason — a stale hand-computed constant, a case whose values did not distinguish the
behaviours it compared, a baseline taken before a legitimate write, and one assertion
demanding a checksum the object format was never going to have. Deriving an expected
value in the test rather than writing it down is now a stated convention.

## H1 — architecture: the widened door has exactly one caller

The H11 fix widened two kernel accessors for the debugger, and a widened accessor is a
hole in a rule rather than a detail of a fix. `Process::memory_mut` and
`UserMemory::address_space_mut` were `pub(crate)` and became public, because a snapshot
has to move a running process.s memory out of the machine. The rule now has a
fourteenth check: **only the debugger reaches it**, because swapping an address space
behind the scheduler.s back would put regions in the machine it does not know about.
Verified to bite.

## H3, H11 — the debugger: a restored machine reported the program finished, having run nothing

`crates/lazalith-debug/tests/snapshot.rs` checked that a snapshot carries what it says
it carries, field by field. That is exactly the shape of test that cannot see a
missing piece, so the new file asks the question a field check cannot: run to
completion, restore, run again, same answer.

It did not have one. The second run reported `Exit { code: 0 }` after **one
instruction**. A user who restored a snapshot and pressed continue would have been
told their program had finished, having watched it do nothing.

Three layers, each found only by fixing the one above. A `ProcessSnapshot` documents
itself as capturing "state, memory, threads and handles — all four, because a process
is all four", and for a *running* process that is false: activating a process swaps
its regions into the machine, so the process the scheduler holds has the other half of
the swap and cloning it captures a process whose address space is not its own.
`snapshot_machine` now releases the active context, captures, and activates it again,
which is why it takes `&mut self` and returns a `Result`. Then the restored process
claimed `Running` with a context nothing was standing behind, so the scheduler found
nothing runnable; and the machine claimed the same context, so it could not be
re-activated. The three claims have to be undone together.

The first draft also compared the two runs.s *step counts* and they differed by 41,
which looked like a lossy restore. Restoring twice and running twice shows runs two and
three are identical: the difference is a one-off in the first run, not a loss in the
restore. The assertion moved to the test that can answer it.

## H7, H8 — kernel and filesystem: clean

`crates/lazalith-os/tests/hardening_filesystem.rs` runs 300 randomised sequences of
200 operations — 60,000 of them — against a model of the documented semantics,
comparing every call.s outcome and the file.s whole contents after every step, with
positions drawn from a table of the boundaries a uniform sample would miss. Every
operation agreed. The documented rules pinned: a write at the end extends, a write
*past* the end is refused (no sparse files), a read past the end is short rather than
a failure, a seek is bounded by the file with the end itself reachable.

One test defect: the read-only case built its own `FileAccess` rather than using the
handle.s, so it was asserting something about its own argument. That is the third
time in this phase that a hand-built value has stood in for a value the system
produced, and the rule — derive the expectation, do not construct it — now covers
handles as well as constants.

## H10 — graphics: the recorded defect, resolved

`docs/graphics-test.md` had recorded an open defect with a reproduction and two
hypotheses: a program drew correctly, and the address the display device recorded was
not the address the program.s framebuffer occupied. Both hypotheses were wrong. The
address was correct at every layer, and the runner was reading the right address in
the wrong memory.

A process.s memory lives in the machine only while the process is resident —
`activate_user_context` swaps the process.s regions in and the machine.s own out, and
`release_user_context` swaps them back. `run_loaded` read the presented frame through
the machine *after* the release, by which point the machine held a freshly zeroed set
of user regions belonging to no process. The fix reads the pixels from the *process.s*
address space, where a dead process.s memory still lives, and the host now returns the
exact bytes the program drew.

That is the assertion step 97 said it could not make honestly. It can now, and
`crates/lazalith-runtime/tests/hardening_graphics_address.rs` makes it.

The lesson is worth stating on its own: the defect was not hidden by a missing test.
It was *documented*, with evidence and a theory, and the theory was what kept it
alive. Asking "which of these two addresses is right" could not have found it; asking
"read the bytes through each and see which holds the picture" did, immediately.

1279 tests pass, and fmt, strict Clippy, check, `nix flake check` and `nix build` are
green. The third sweep of the C front end found three more defects and is recorded in
`docs/hardening.md`: a multi-dimensional array.s initialiser checked against the wrong
dimension, `switch` discarding the value it was switching on, and pointer arithmetic not
scaled by the pointee. `lazalith-c-runtime` and `lazalith-runtime` are dev-dependencies of
`lazalith-c-compiler`, as they should be; they were briefly regular dependencies when
the cross-frontend test was written.


## Earlier milestones

This milestone added code generation. Steps 1–62 are unchanged except for the
defects Step 63 found by running the generated code, each of which is listed
under its own heading below. The 691 workspace tests all pass, including 24 in
`crates/lazalith-codegen/tests/codegen.rs` that run generated code on the real
machine and compare what it wrote and what it exited with, 10 in
`crates/lazalith-runtime/tests/runtime.rs` that do the same for a whole program
built from the prelude, 16 in `crates/lazalith-stdlib/tests/stdlib.rs` that do
the same for the standard library, and 22 in
`crates/lazalith-devices/tests/display.rs` for the display device and 21 in
`crates/lazalith-devices/tests/input.rs` for the input device.

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

## Step 64 — Runtime

`crates/lazalith-runtime` is the thing a Lazen program links against, and it is
two things that are deliberately kept apart.

The **entry sequence** is machine code. It calls the program's `main` through the
documented convention and turns the result into an exit status. It cannot be
Lazen: Lazen v1 has no function pointers and a function's name is not a value, so
nothing written in Lazen can call `main` by name.

The **library** is Lazen, in `source::PRELUDE`: the syscall wrappers, byte moves
and text helpers. It goes through the same frontend, lowering and code generation
as a user program, so it cannot disagree with the compiler about what the
language means.

A program is one compilation unit — the prelude's text in front of the user's —
and the entry sequence is an object beside the generated one. Nothing in the path
is special-cased for the runtime.

### Defects running whole programs found

Every one of these produced *wrong answers rather than traps*, which is why the
runtime tests compare what a program wrote and what it exited with instead of
inspecting the object.

- **A stack argument arrived as zero.** The caller and the callee disagreed on how
  many argument words travel in registers. `MAX_ARGUMENT_WORDS` is six for a
  *syscall* (`r1`–`r6`), but a Lazen call passes four words in `r0`–`r3` and
  spills the rest below the stack pointer. The caller used the six, so the fifth
  and sixth words went to registers the callee never read. Both sides now name the
  count they mean — `ARGUMENT_REGISTERS` for a call with a stack, `r1`–`r6` for a
  syscall — because the total alone does not say where a word goes.
- **A frame did not reserve the word its own `CALL` overwrites.** Sixteen bytes
  were reserved for outgoing argument words, but the return address a call pushes
  sits one word *below* the stack pointer, in the frame's own reserve. Without
  `RETURN_ADDRESS_BYTES` a function's first call overwrote the return address its
  caller had left for it.
- **Indexing a view wrote over the view.** `place_address` returned the frame
  address of a view local, where it had to return the pointer the view *holds*, so
  `view[at] = x` wrote over the two words of the view and left the data untouched.
  `place_length` already read *through* the view, which is why the bounds check
  was right and the store was not.
- **Every index expression was off by one element.** `CheckedPlace::Index` carried
  `element_offset: element_size`, but the base address already points at element
  zero and the index is scaled by the element size when the address is formed, so
  the offset is zero. This was invisible until a view base met a concrete array
  read: on a concrete array the same expression was written *and* read, and both
  were wrong by the same amount.
- **`!` was a conversion, not a negation.** It lowered to `int_to_bool`, which
  asks "is this nonzero?". The operand is already a `bool`, so `!true` came back
  `true` and `!false` came back `false` — `!` did nothing at all. The IR gained
  `UnaryOp::Not`, which asks whether the operand is `false`; `BitNot` would not do,
  because the complement of `0` is every bit set, which is still true.
- **An entry in a later object started at the wrong instruction.** The entry
  offset is measured from the bottom of the code region; it was measured from the
  entry object's own slice of it, so an image whose entry was not in the first
  object started wherever the first code happened to be. This is the prelude case:
  the prelude is the first object and the program's `main` is not.

### Deliberate boundaries

- The runtime returns the ABI's status and does not turn it into a value a program
  tests; that is the standard library's job, in Step 67.
- It does not allocate. Lazen v1 has no heap, so a wrapper that needs an
  `IoResult` takes a caller's buffer.
- It does not set up the stack. The OS establishes `USER_INITIAL_SP` when it loads
  the image, and every generated prologue reserves its own frame, so the entry
  sequence's only stack obligation is to leave SP alone.
- Lowering still requires a `main`, so the refusal for a program without one comes
  from lowering rather than from the link. A library-only module would need the
  entry to become optional; nothing in the roadmap needs one before Step 67.

## Step 65 — The First Lazen Program

`examples/hello/main.lz` is a real Lazen source file, and
`crates/lazalith-runtime/tests/hello.rs` builds *that file* and runs it through
the whole pipeline in the order a user's program goes through it:

```text
main.lz → frontend → lazalith-ir → codegen → .lzo → linker → .lzx
       → LzxImage::from_bytes → load_process → process → console
```

The test asserts what the console received and what the process exited with, not
that an object has the right shape. The machine goes through a real boot handoff
(`BootImage::start`, then the supervisor kernel's `RFE`) and the image goes through
its serialized bytes, so the two parts of the path with a format of their own are
under test rather than skipped.

Six tests, each for a different way this pipeline can be wrong:

- `a_lazen_program_runs_under_lazos` — the greeting arrives, `main`'s `0` becomes
  the process's exit code, and the process reaches `ProcessState::Exited`.
- `a_computed_value_reaches_the_console` — `6 * 7` is divided down to `042` in a
  frame and written through the same `write` wrapper. A bug making every frame
  slot read as zero would pass the first test and fail this one.
- `the_checked_in_example_builds_and_runs` — the artifact in `examples/` is what
  is compiled, so the example cannot rot while a test still passes on a copy.
- `the_program_runs_as_a_user_thread` — the process loads with `Privilege::User`
  and its entry lies inside the user code region. A program running with
  supervisor rights would leave every later security property untested.
- `the_image_survives_a_serialisation_round_trip` — a `.lzx` re-serializes
  byte for byte, and a truncated or magic-corrupted image is refused rather than
  loaded with whatever survived.
- `a_broken_program_never_becomes_an_image` — a compile failure arrives as a
  diagnostic naming the missing name and the file, not a panic and not an empty
  image.

### One thing the entry assertion had to get right

The entry is the startup sequence, which the linker places *after* the program's
own text, so it is not at `USER_CODE_START` — that is the base of the user code
region, and an image whose only object is the program does start there. The test
asserts the entry is inside the mapped region rather than at its first byte,
because a program that starts at the region's base would be starting in the
middle of its own text whenever anything else is linked first.

### Limits

- The example uses `rt::sys::print` directly. Step 67's standard library is where
  a program should be reading and writing, and Step 66 is what lets a user build
  and run this file without naming a path.
- Only `.lz64` is exercised here. `.lz32` is refused by lowering as a target with
  no register-pair lowering, which is a documented Step 62 decision, not a gap
  this step introduced.

## Step 66 — The `lazen` CLI

`crates/lazalith-cli` builds the `lazen` binary. Five of the six commands the
roadmap names are implemented:

```text
lazen new <name>       scaffold a project
lazen check [file]     parse, resolve, type-check; generate nothing
lazen build [file]     compile and link to a .lzx beside the source
lazen run [file]       build, then execute under LazOS
lazen test [file]      run a project's tests
```

**There is no `lazen fmt`.** The formatter is Step 90, and a `fmt` that exited 0
having rewritten nothing would tell a user their file had been formatted. The
usage text and `help` both say it is absent, and a test asserts the file is left
untouched.

Exit codes are the tool's interface, so they are stated rather than incidental:
`0` success, `1` the program refused (a diagnostic, or a non-zero status from
`run`), `2` the command line could not be acted on. A program's own status is
passed through as the tool's, so `lazen run` on a program returning 7 exits 7.

### The two design decisions that were not obvious

**`check` composes the runtime library before checking.** Checking the user's
file alone calls every library name undefined, so `lazen check` rejected the
scaffold its own `lazen new` had just written. `check` now performs the same
composition `build` does and stops after the type checker — which is what makes
it a faster `build` rather than a different one.

**The program's text comes first in the composed unit.** A diagnostic reports a
line number into the unit's text, so a library placed first pushed every one of
the user's lines up by the library's length: a one-line program reported an error
at line 410, in a file the user had written four hundred lines of. Putting the
program first keeps the user's lines where they wrote them. This is only sound
because Lazen resolves names independently of order — verified before relying on
it — and `compose` documents that dependency so it cannot be broken silently.

### `lazen test`

A test is a top-level function named `test_*`, taking no arguments and returning
`0`. There is no framework, no attribute and no discovery file. The names come
from the *checked* program, so a `test_*` inside a comment is not a test and one
that does not compile is a compile error rather than a silently missing test.

Each test runs as its own program, built from the project's items minus `main`
with a generated `main` that returns the test's result. Running separately is what
makes one test's corrupted frame irrelevant to the next. A test may call helpers
written beside it, and its console output is shown whether it passed or failed —
a test that prints is reporting something, and discarding it because the return
value happened to be 0 would throw that away.

The function text is extracted by brace depth rather than through the parser, and
that limit is documented at the extractor: it is only sound because the result is
fed straight back to the compiler, so a wrong extraction produces a real
diagnostic instead of a guess. The alternative needs the compiler to expose item
source ranges, which is more coupling than this command justifies.

### Tests

23 tests in `crates/lazalith-cli/tests/cli.rs` run the real binary in a temporary
directory and judge it by stdout, stderr, exit code and the files left on disk.
Testing the command functions directly would miss the parts that are actually the
contract: which stream a message uses, what the exit code is, and whether a file
appeared.

Notable ones: `new_then_run_prints_and_succeeds` is the roadmap's own final
experience; `check_sees_the_runtime_library` guards the composition bug above;
`check_reports_a_bad_program_with_a_usable_diagnostic` asserts the line number is
inside the user's file and not shifted by the library; `two_files_are_refused`
guards against silently checking one file of two; `fmt_is_refused_with_a_reason`
guards against a `fmt` that does nothing and claims success.

## Step 67 — The First Lazen Standard Library

`crates/lazalith-stdlib` holds the library as **Lazen source**, for the same
reason the runtime's wrappers are text: a standard library that was generated code
could disagree with the compiler about what the language means. It goes through the
same frontend, lowering and code generation as a user program.

The eight modules the roadmap names, and nothing more:

```text
core          integers, checked arithmetic, the status convention
io            console and reading from a handle
text          str and byte questions, and decimal formatting
math          integer arithmetic, no floating point
collections   fixed-capacity buffers and stacks over caller memory
fs            open, close, read, write, seek, stat, list
time          the clock and sleeping
process       spawn and wait
```

The rule the roadmap states — *only add APIs the OS actually supports* — is
enforced by the fourteen numbered syscalls. There is no networking because there
is no socket syscall, no threads because there is no thread syscall, and
`collections` means fixed-capacity containers over caller memory because
`allocate` returns an address a v1 program cannot name as a slice. A `Vec` that
quietly allocated would be a lie about what a program is linked against.

### The conventions, and why they are what they are

**A fallible call writes its value out and returns a status.** Lazen v1 has no
tuples, no `Result` and no enums, so `(value, ok)` is not a type a function can
return. Each such function takes a `&mut [u8]` the value goes into and returns
`i64`: zero for success, negative for the ABI's error. This is the ABI's own
convention rather than a library invention, so there is one shape to learn and it
is the one the hardware already uses. It is also *necessary* rather than merely
convenient: a handle of 0 is legitimate, so returning 0 for "failed" would be
indistinguishable from opening the first file.

**The prelude and the standard library are separate, and both are linked by
default.** `BuildOptions::lz64` composes both; `BuildOptions::freestanding`
composes only the prelude. That is what keeps the two honest — a bug in `std`
cannot make a runtime test pass, and a bug in the runtime cannot make a `std` test
pass. `a_freestanding_program_needs_no_standard_library` proves the smaller build
still runs.

### Five language and compiler changes this step forced

Building a real library exposed five gaps, each of which produced a *wrong answer*
rather than a refusal:

- **A narrow parameter was stored a whole word wide.** `f(a: u32, b: u32)` put
  `b` four bytes after `a`, and storing eight bytes for `a` wrote over `b`'s slot.
  The smallest failing case was two narrow parameters, not one, and the symptom
  was a *missing* value rather than a corrupt one.
- **A view could not be returned.** A view is two words and the convention only
  had `r0`. The return path now uses `r0` and `r1`, and the caller *stores* both
  into the result's slot — reading them from registers would break any expression
  that returns a view in the middle of a larger one.
- **A nested module could not use a root-absolute path.** `rt::memory::copy`
  inside `rt::sys` was looked for as `rt::sys::rt::memory`, so deeply nested
  libraries were impossible to write without a `use` at every level.
- **A view of memory at an address could not be expressed.** `ptr<T>` is
  deliberately not dereferenceable, so a program that received an address from the
  OS had no way to index it. Two builtins were added: `slice_from_raw` and
  `slice_from_raw_mut`, each taking the length the caller states.
- **Bytes could not be offered as text.** `str` is a *checked* UTF-8 byte string,
  and there was no route from arbitrary bytes to one, so a program reading a file
  could not tell text from noise. The `as_str` builtin is that route, and it
  *checks* — the validator lives in the prelude as `rt::utf8::valid`, because a
  Lazen program could not write the check itself.

The UTF-8 validator is the most careful code in the step. It refuses overlong
encodings and surrogates, because an implementation that only checked the *shape*
would accept `0xC0 0x80` for `U+0000` and `0xED 0xA0 0x80` for `U+D800` — both of
which spell a character that also has another spelling, and any comparison between
two spellings of one character has to fail. Two tests guard this from both sides:
`text_refuses_overlong_and_surrogate_encodings` and
`text_accepts_the_characters_next_to_the_refused_ones`, so the ranges cannot be
fixed by being made too tight.

### Tests

16 end-to-end tests in `crates/lazalith-stdlib/tests/stdlib.rs`. Each builds a
program that uses the library through its **public** names — `std::text::len`,
not `rt::mem::equals` — runs it under LazOS, and checks the exit code. A library
whose own tests reach past its API is not being tested at the interface it offers.

Two regression tests were added to `crates/lazalith-codegen/tests/codegen.rs` for
the codegen defects above, and both were **verified to fail when the fix is
reverted** — the narrow-parameter one and the two-word-result one.

### Limits

- `fs::open`, `read`, `write`, `seek`, `size`, `process::spawn` and `wait` are
  implemented and type-check, and are exercised by the OS's own syscall tests
  rather than by a Lazen program here. A stdlib test for them needs a filesystem
  with known contents, which the virtual filesystem provides but no stdlib test
  populates yet.
- `text` has no `split` or `trim`. Both are expressible in the closed type set,
  and both are omitted rather than approximated: a `split` that returned a
  borrowed sub-view would need sub-view syntax v1 does not have, and returning
  indices instead is a different function with a different name.
- `math` has no floating point, and will not until the ISA has it.

## Step 68 — The Virtual Display Device

`DisplayDevice` in `crates/lazalith-devices/src/display.rs`, with 22 tests in
`crates/lazalith-devices/tests/display.rs`.

### The design, and the one property it exists for

**The guest owns the authoritative framebuffer.** The device keeps *no* pixels: it
holds the geometry, the framebuffer's address, a present counter, and the address
of the last frame presented. A present is a synchronisation point, not an upload,
because the device and the guest are looking at the same memory.

That is the whole design, and it is what makes a headless run and a windowed run
identical — a device that stored its own copy would make every frame a transfer of
`width * height * 4` bytes, and would let the guest's memory and the device's view
disagree, which is the failure this removes.

`the_guest_owns_the_pixels_and_the_device_holds_no_copy` is the test that
separates the two designs, and it does so by **mutating guest memory after
presenting**. A test that compared what the device presented against what the
guest had written *at present time* would pass on a device that kept a copy. The
mutation is what makes the claim testable.

### The register surface

Eight double-words, fixed offsets, every access bounds checked:

```text
0  width            read-only    32  present count    read-only
8  height           read-only    40  last presented   read-only
16 framebuffer      writable     48  ABI version      read-only
24 present          writable     56  status           read-only
```

The geometry registers are **read-only** for a stated reason: a framebuffer is
sized for a *pair* of dimensions, so accepting one half would leave the device
describing a region that does not exist. `DisplayDevice::open` is the only way to
set geometry, and it validates both halves and the `width * height * 4` product
together — which is also what catches a framebuffer too large to address before a
window is opened rather than after.

`REGISTER_ABI_VERSION` exists so a program can check the ABI *before* trusting any
other register; without it, a driver built for a different layout would read
plausible numbers from registers that mean something else. `REGISTER_STATUS`
distinguishes "never presented" from "presented a blank frame", which a
present-counter-only design cannot.

### Two rules the implementation follows

**Validate before mutate.** `validate_write` performs the write on a *copy* and
throws it away, so a refused write leaves the device byte-for-byte as it was. The
interesting case is a present with the window closed: without this, validation
would count a frame that was then refused. `a_refused_write_changes_nothing` and
`a_present_with_no_window_is_refused` both cover it.

**A refused write is a refusal, not a silent no-op.** A present with no window
open, a write to a read-only register, and a framebuffer address of zero are each
refused with a reason. Each would otherwise be a program that believes it drew
something.

`peek` requires an output buffer of *exactly* the register width. Copying what fits
would let a caller read the low half of a value and believe it had read the whole
thing, and a debugger showing half a framebuffer address is worse than one that
refuses.

### Limits

- The pixel helpers (`frame_bytes`, `pixel_at`, `zeroed_framebuffer`) are free
  functions, not device methods, because reading pixels needs memory the device
  does not have. The device's whole claim is that it does not need it.
- There is no damage tracking, no double buffering and no frame pacing. The
  design document rules all three out for v1, and a present counter is the whole
  of what synchronisation a headless run needs.
- Nothing here knows SDL3 exists, and nothing may: a device that knew about a host
  window could not run headless, could not be recorded deterministically, and
  could not be tested without a display server. The host frontend in Step 77 reads
  the presented framebuffer; the device never finds out.

## Step 69 — The Virtual Input Device

`InputDevice` in `crates/lazalith-devices/src/input.rs`, with 21 tests in
`crates/lazalith-devices/tests/input.rs`.

### A queue, not a sample

The device owns a queue of guest-visible events and the guest **drains** it. That
is the whole design decision, and it exists to prevent a specific, common loss: a
device that reported only the *current* key state would lose every press and
release between two polls, so a program polling once per frame at thirty frames a
second would lose any key tapped faster than that. Losing input is not a detail —
it is the difference between a program that works and one that intermittently does
not, with nothing to say which.

So `poll` takes up to `capacity` and **the remainder stays queued**.
`a_poll_that_cannot_take_everything_keeps_the_rest` is the test, and it was
**verified to fail when the drain is changed to discard the remainder**.

### Events are records, not host structures

Sixteen bytes, every field defined for every kind, an unused field zero — so a
reader never has to ask which fields are meaningful, which is what lets a program
match on the kind and read the rest without branching.

**An unknown kind is held, not refused.** `Event::kind` is a `u32` wrapper rather
than the enum, because a program running against a newer device must be able to
*hold* an event it does not understand and skip it. Refusing the record would make
that impossible, and would turn a forward-compatible device into one that crashes
old programs. `an_unknown_kind_is_held_rather_than_refused` covers it.

### Three rules the implementation follows

**A full queue refuses, it does not drop.** The queue is host-fed and
guest-drained, so a host producing faster than a program polls would otherwise
grow it without limit. Refusing at the bound is honest — a program that is not
polling has said it is not ready — where dropping the oldest would lose input
silently.

**`inject_all` stops at the first refusal rather than skipping.** A host adapter
that skipped the event that did not fit would deliver a *reordered* stream, and a
program that got its keys in the wrong order would have no way to tell.
`injecting_several_stops_at_the_first_refusal` checks that neither event got in.

**A guest cannot inject its own events.** `inject` is deliberately not reachable
from a register: an input device a program can lie to is not an input device.

### No blocking, no timeouts

There is no "wait for an event". A blocking read would make a program's behaviour
depend on when it was scheduled, and two runs with the same injected events would
not necessarily agree. A poll that finds nothing returns zero, which means
"nothing pending" and not "something went wrong".

`the_same_script_produces_the_same_stream` is the determinism property the design
document asks for, stated as a test.

### The device holds nothing that belongs to a host

There is no host key code, no scancode, and no modifier convention here. The
device knows that something pressed a key with a *stable Lazen code*; the host
adapter in Step 71 is the only component that knows what a keyboard is, and the
same program under it must see the identical records.

### Limits

- `REGISTER_POLL` is writable and drains **nothing**: a device has nowhere to put
  the bytes. The events themselves travel through the ABI in Step 71, and the
  register exists so a driver can acknowledge a poll. This is the one part of the
  surface that is not yet the finished shape, and it is finished in Step 71 rather
  than half-built here.
- The queue limit is 4096 events. A program that polls less often than the host
  produces will be refused injection, which is a loud failure rather than a silent
  one — but it is a failure, and a program that legitimately polls rarely would
  need a larger limit.
- `Text` carries a Unicode scalar value and nothing is composed: there is no IME
  and no dead-key handling, both of which the design document assigns to the GUI
  library in Step 73.

## Step 70 — The LazOS Display Driver and the Lazen SDK

`DisplayService` in `crates/lazalith-os/src/display.rs`, with 13 tests in
`crates/lazalith-os/tests/display.rs`; `std::graphics` in
`crates/lazalith-stdlib/src/lib.rs`, with 8 end-to-end tests in
`crates/lazalith-stdlib/tests/stdlib.rs`. The ABI additions are
`Syscall::DisplayOpen = 0x000f`, `Syscall::DisplayPresent = 0x0010`, and the
24-byte `DisplayRecord`.

### The driver copies nothing, and that is the design

`display_open` takes the address of a framebuffer the **guest already owns** and
records it; `display_present` records that the frame at that address is the
visible one. The driver never reads a pixel and never writes one.

This is not a performance choice. A driver that copied would make every present a
transfer of `width * height * 4` bytes and would let the device's view and the
guest's memory disagree — which is precisely the failure
`docs/lazen-graphics.md` exists to rule out. `the_driver_never_touches_a_pixel`
writes a pixel into guest memory, opens a window over it, presents, and requires
the pixel to be byte-for-byte unchanged;
`the_driver_reports_an_address_and_not_pixels` requires the reported frame to be
four scalars, so there is nowhere for a copy to be hiding.

The consequence for a host is stated rather than hidden: a frontend resolves the
address against guest memory itself, and the driver hands it an address.

### A Lazen program has nothing to bypass the driver with

There is no syscall that returns a device address, and no way to name the display
device from Lazen at all. The only path is `std::graphics` → the ABI → the driver
→ the device. `a_program_that_never_opens_a_window_has_no_frame` states this from
the outside: a program that never calls the SDK leaves the driver with no window
and no frame, and the test reads the driver's own state rather than the program's
claim.

### Two different refusals, because they are two different bugs

A present with **no window open** is `InvalidHandle`; a present of **some other
address** is `InvalidArgument`. Collapsing them would read as "presented a frame
that is not on screen", which is the one answer that hides both. The frame count
is written on refusal too, so a program that gets one can read how far it got.

The **return value** of a present is a success whenever the call reached the
driver, and the outcome is in the result record's status. `display_present` is a
valid call *about* a frame that can be refused, so the SDK's `present` reads both:
`presenting_reports_frames_and_the_driver_sees_them` requires a refused present to
return `false` while the count stays where it was.

### A refusal names the argument that was wrong

The ABI's convention is a status plus a detail word, and for display calls the
detail is the offending argument index — 2 for the framebuffer, 3 for the record,
1 for the result. A caller can therefore say *which* of its four arguments was
bad, which is the difference between a diagnostic and a shrug. Each refusal test
asserts both the status and the index.

### Validate before mutate

`display_open` checks the record is writable and that the framebuffer holds the
whole window **before** the device is touched, and closes the window again if the
record cannot be written. A window whose record the guest never received is a
window the guest cannot know about, so it is not left open.
`an_unwritable_record_is_refused_and_leaves_no_window` covers that, and the
geometry's pixel count is checked for overflow before it is multiplied
(`a_geometry_whose_pixel_count_overflows_is_refused`).

### The SDK is pure Lazen, and packs what the ABI cannot carry

`std::graphics` is Lazen source compiled like any other program. Lazen v1 has no
opaque `Window` or `Canvas` type, so a canvas is a `&mut [u8]` the caller owns
and the geometry is passed alongside it — the same memory, named directly.

The ABI has six argument words and a view is two, so a call with a canvas, some
geometry, and a colour is already at the limit. Two shapes are therefore packed,
and each is documented at the field that says what it is:

- `pack_point(x, y)` and `pack_rect(x, y, w, h)` — two 16-bit fields each.
- `pack_surface(width, height)` for `draw_text`, and `pack_ink(x, y, color)` —
  16, 16 and 32 bits, because `draw_text` needs a canvas (two words), a surface,
  an ink and the text (two more) and that is eight words.

A packer whose fields were narrower than its readers was a real bug here and is
now covered: `a_packed_rectangle_round_trips_every_field` uses coordinates above
255 in every field, which a 16/16/8/8 packing silently truncated.

### Clipping is total

Drawing off an edge is not an error. `put_pixel` returns whether it drew,
`fill_rect` clips the requested rectangle and draws row by row, and `draw_text`
clips both ends. `drawing_is_clipped_on_every_side` hangs a rectangle off two
edges, requires its visible part and nothing else, and requires a rectangle
entirely off the canvas to draw nothing at all.

### The font is an SDK resource, and it is checked

`draw_text` uses a built-in 8×8 font for printable ASCII, held as a string
constant in the read-only data section — two hexadecimal digits per row byte,
because Lazen v1 strings have no `\x` escape and a font is exactly the kind of
data a string literal should not have to encode by hand. It is one line because
Lazen v1 has no line continuation in a string.

`text_draws_glyphs_from_the_builtin_font` checks the font's bits, that a
character advances the pen a whole eight-pixel cell, and that text off either
edge is clipped. The font is therefore identical headless and graphical, which is
the property the design asks for and the reason no host asset is involved.

### Two compiler bugs this step found

Both were found by the SDK and both are regressions now, because a display driver
is the first thing that multiplies by `2^32` on purpose.

**A widening cast to a narrow target loaded too wide.** A cast stages its operand
in a scratch word and loads it back; the load took the *target's* width, so
`u8 as u32` read three bytes of frame the store never wrote. Whether the answer
was right depended on what else the function had put in the frame.
`a_widening_cast_to_a_narrow_target_loads_the_source_width` and
`a_widened_value_keeps_the_bytes_it_was_widened_from` cover the IR shape and the
program. The existing `a_cast_is_a_load_at_the_source_width` had used `u8 as i64`,
an eight-byte target, which took the working path.

**A wide constant was built from its halves in the wrong order.** `LI` is a signed
32-bit immediate, so a 64-bit constant is two halves — and the code shifted the
*low* half up and or-ed the high half in unshifted, producing `(low << 32) | high`.
`4294967296` arrived as `1`. The two early returns were unsound in the same way:
`LI` alone is the number only when the high half *is* the low half's sign
extension, which is two cases out of four.
`a_wide_constant_is_the_number_it_was_written_as` covers all six shapes and was
**verified to fail with the old code**, which produced `1` for four of them.

## Step 71 — The LazOS Input Driver and the Host Input Adapter

`InputService` in `crates/lazalith-os/src/input.rs` with 13 tests in
`crates/lazalith-os/tests/input.rs`; `HostInputAdapter`, `HostKey` and
`HostScript` in `crates/lazalith-devices/src/host_input.rs` with 13 tests in
`crates/lazalith-devices/tests/host_input.rs`; `std::input` in
`crates/lazalith-stdlib/src/lib.rs` with 8 end-to-end tests in
`crates/lazalith-stdlib/tests/stdlib.rs`. The ABI additions are
`Syscall::InputPoll = 0x0011` and the 16-byte `InputEventRecord`. That was the
last reserved name, so `RESERVED_DESIGN_SYSCALLS` is now empty and an
`extern "syscall"` the ABI does not name is refused at its declaration.

### The adapter is the only component that knows what a keyboard is

`HostKey` is numbered with SDL3's `SDL_Scancode` values, and those numbers are
the one thing in Lazalith that cannot be checked against anything inside the
build. They were read out of SDL 3.4.16's `SDL_scancode.h` and pinned by
`the_host_numbering_is_sdl3s`, which states them as numbers rather than as a
comment — three of them were wrong when first written and the test is where that
showed up. Step 77 is then a pass-through rather than a second translation, and
if SDL ever renumbers one, the fix is that one enum.

`no_host_code_reaches_the_guest` is the property the file exists for: if a host
number could reach the guest, a host renumbering would renumber the guest.

### Lazen's key codes are frozen, and are not a host's

A program compiled against this table has to keep working when the host adapter
changes. So the guest-visible codes are Lazen's own, and they are deliberately
*different numbers* from the host's — which is what makes the translation a real
step rather than a rename.

The letters occupy **one contiguous range in ASCII order** and the digits
another, so a program classifies a key with two comparisons and reads which
letter with a subtraction. That is the entire reason for the numbering:
classification without a table. `key_codes_classify_without_a_table` walks both
ranges in a Lazen program and checks every value against the arithmetic, with no
table anywhere in the program.

`docs/lazen-input.md` listed the letters as four ranges, which cannot be right:
`'q'..'p'` and `'z'..'m'` are not contiguous in ASCII order, and the ranges
overlap. The doc has been corrected to the two contiguous ranges, and the
right-hand super key — named by the design, absent from its table — is now
assigned.

### The remainder stays queued, and this is tested through the ABI

A poll writes whole records into the caller's array and returns how many. If the
queue holds more than the array takes, the rest **stays queued at the device**.
`a_poll_that_cannot_take_everything_keeps_the_rest` holds that through the real
syscall, and it was **verified to fail when the driver is changed to take at most
one event** — three tests fail, because a drain that discarded the remainder
loses input silently and a program polling once per frame loses any key tapped
faster than that.

A return of zero means *nothing pending*, not an error, and
`a_poll_with_nothing_pending_is_zero` polls five times to say so.

### The count comes back in a record, like every other value

A v1 call returns one `i64` and that word is the status, so the count goes in an
`IoResult` — the same reason `write` and `time` report through a record. The
driver writes that record on *every* path including a refusal, so a program whose
array turned out to be unusable can read how far it got. The SDK reads both the
call's status and the record's, because they are separate claims: a refused call
that also reported a count would be read as a partial success otherwise.

### The records are word-backed, and that was a real bug

The ABI's records are word-aligned and the kernel checks it. An SDK scratch of
`[u8; 16]` has an alignment of *one*, so whether the call worked depended on
where the frame layout happened to put it: Step 70's `present` worked by luck
and Step 71's `poll` did not, failing with `Misaligned` in one frame and not
another. Both now back their scratch with `[u64; 2]` and read the record as
words — `IoResult` is a count then a status, which on this target is the low
word's two halves, so no byte view is needed. The test
`a_program_reacts_to_scripted_keyboard_input` was verified to fail with the byte
array put back.

### A repeated array is a loop, not a store per element

Found by the same test: a framebuffer-sized array is initialised in the *code*
section, and one store per byte is a code section of megabytes for an array
whose contents are all the same. A 64000-byte array did not link. `ArrayRepeat`
is now a counted loop, so the code is the size of the loop rather than the size
of the data, and a 64000-byte array builds.

The loop's bound is `index < count` and it is tested *before* the body, because
branching on the counter alone would skip element zero and leave the first
element of every array uninitialised. `a_repeated_array_is_a_counted_loop_over_every_element`
holds both the code size and the bound, and
`a_repeated_array_fills_every_element_at_any_size` runs it as a program.

### Limits

- The host numbering is pinned against SDL 3.4.16 and Step 77 must confirm it
  against the SDL3 it links. That is the one number here with no in-build check.
- A repeat is bounded by the *step* budget, not by the image format, because the
  generated code keeps every value in the frame and reloads it. A 256KB window
  is a real window and the backend is unoptimised; Step 72 will need either a
  larger budget or a cheaper `clear`.
- `REGISTER_POLL` is still writable and drains nothing. The events now travel
  through the ABI, so the register is the last part of the Step 69 surface that
  is a placeholder rather than a finished shape.
- Text is the host's to compose. `HostAction::Printable` is a convenience, not a
  rule, because case, dead keys and input methods are host concerns and the
  design puts text editing in the GUI library.

### Next step

Step 72 is the **first graphical Lazen application**: a program that creates a
window, draws, receives keyboard input, and updates its state — and knows
neither SDL3 nor the kernel. `a_window_draws_and_quits_on_a_scripted_key` is
already that program in miniature, so Step 72 is its real shape rather than a new
idea.

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

## Step 72 — The First Graphical Lazen Application

`examples/window/main.lz` is the application, and it is a repository file rather
than a test fixture: the four tests in `crates/lazalith-runtime/tests/window.rs`
read it from disk and build it through the whole pipeline, so the test cannot
pass against a program the repository does not actually ship.

It opens a 48-by-32 window over a framebuffer it owns, clears it, draws a block
and the word "lazen" in the built-in font, presents the frame, reads the
keyboard, and updates its state from what the keyboard said — a `text` event's
character moves the block and changes its colour. It mentions no window handle,
no event queue, and nothing about SDL3. The whole path is

```text
examples/window/main.lz → compiler → .lzx → LazOS loader → kernel
    → display driver → display device
    → input driver   ← input device
```

and the assertions are on what the *drivers* saw rather than on `main`'s return
value, because a program that drew into a framebuffer nobody presented returns
zero just the same.

### The frame is the program's own memory

`display_open` copies nothing, so the pixels on screen are the bytes the program
wrote. `the_presented_frame_is_what_the_program_drew` reads the address the
device reported back out of the process's memory and checks the background, both
edges of the block, the pixels either side of it, and one lit and one unlit pixel
of the first glyph. A test that only checked "a white pixel exists somewhere"
would pass a program that drew its block in the wrong place, at the wrong size, or
not at all.

Reading the frame needed `RoundRobinScheduler::process_mut`. A memory context is
a capability rather than a value — it is what the syscall dispatcher is handed —
so a `&Process` cannot be given one, and a host frontend resolving what a device
reported would hit the same wall.

### The keyboard is input, not a record nobody read

`the_keyboard_moves_what_the_program_draws` queues a `text` event carrying `d`
and compares the block's position against the same program with no keyboard. The
pair of assertions says "it moved" rather than "there is a white square": a
program that ignored input leaves both pixels identical, so the test fails.

`the_application_leaves_no_event_unread` then asks the device what is still
queued and what was handed over. A program that stops reading leaves events in
the queue, and a queue that only grows is how input becomes unbounded memory in a
program that looks correct.

### The record the window comes back in is 24 bytes, and is word-backed

`open` takes the record from the caller, and the ABI wants it word-aligned. A
`[u8; 24]` has an alignment of *one*, so whether the call worked would depend on
where the frame layout happened to put the array — the same defect Step 71 fixed
inside `poll` and Step 70 inside `present`, and the same reason the application
backs it with `[u64; 3]`. The size is worth stating separately: the display
record is **24** bytes, not 16, and the first version of the application used 16
and was refused at the SDK's own length check before a syscall was ever made.

### The frame base is a register, and that was the expensive part of this step

Before Step 72 a 320-by-200 framebuffer could not be initialised at all within
the tool's runaway budget, so the step began by finding out why. Forming the
address of any frame slot was `GETSP r7; ADDI r7, r7, offset` — two
instructions to reach a slot whose address does not change — and a trivial `while`
loop that stores one byte cost 31 machine instructions, of which 15 were spent
recomputing addresses.

The frame base now lives in `r8` for the length of a function's body, which makes
a slot's address one instruction instead of two. `r8` was chosen because
`docs/isa.md` makes `r8`–`r15` callee-saved, so the discipline is the one the
calling convention already requires and no new convention is invented: each
function saves the caller's `r8` and restores it.

**The save slot's position is load-bearing, and two wrong positions both looked
plausible.** The obvious spot — the frame's own `[SP+0]` — is where a call writes
argument words five and six, so a function that made a call with five or more
argument words overwrote its own saved `r8` and came back with whatever the
callee had left there. A slot sized from the value area but addressed from the
*stack pointer* rather than from the frame base lands 16 bytes low, inside the
value area, and overwrites a local. Neither shows up in a program that calls a
function with two arguments.
`a_frame_stays_addressable_across_a_call_with_six_arguments` calls with six
argument words at two depths and reads and writes locals *after* the call, and it
was verified to fail with each wrong position put back.

### Two hot loops in the SDK were doing avoidable work

`clear` re-read the four channel bytes out of a temporary array on every pixel —
four bounds-checked loads per pixel, in the one function every graphical program
runs over its whole framebuffer every frame. They are read once now, and a
measured 4096-byte clear went from 720 instructions per pixel to 600.

`put_pixel` built a temporary `[u8; 4]` per pixel, which is a *repeated-array
initialisation* — a counted loop — on every single pixel, and then copied it into
the canvas. It now writes through a four-byte view at the offset, so the layout
is still written in exactly one place (`write_pixel`) and the temporary is gone.

### Limits

- **A 256 KiB window does not fit, and the budget was not raised to make it.**
  The measured costs are in `docs/lazen-graphics.md`: about 92 instructions per
  byte to initialise a framebuffer, 600 per pixel to clear one, and 2300 per
  `put_pixel`. A 320-by-200 window is roughly sixty times the whole runaway
  budget for a single frame. Raising the limit would hide that rather than remove
  it, since 5,000,000 instructions already takes about ten seconds to interpret.
  What removes it is a register allocator in the backend or a bulk memory
  operation in the ABI, and neither exists yet.
- The interpreter runs at roughly 145,000 instructions per second in a debug
  build, so the four tests in this step take about 24 seconds between them. That
  is the interpreter's speed, not the application's work: a release build is
  about three times quicker and the instruction count — which is what the budget
  is about — is unchanged.
- The application runs a fixed number of frames and returns. An interactive
  program would loop until it saw the quit request; the bounded loop is what
  makes this one testable end to end, and it is the only difference.
- `std::graphics::open` is the one SDK function that still takes a record from
  the caller, so it is the one place a program can get the word-alignment
  requirement wrong. Every other record is allocated inside the SDK, where the
  compiler decides the alignment.

### Next step

Step 73 is the **first-party GUI library** — `Window`, `Panel`, `Button`, `Label`,
`TextInput`, `Canvas`, `Menu` and `Layout` — built on the Lazen SDK and knowing
nothing about SDL3. `examples/window/main.lz` is the shape of a program that would
use it, and the open block of work is a layout and a widget set that a program
draws with `std::graphics` rather than one that reaches past it.

## Step 73 — The First-Party GUI Library

`lazalith-gui` is a new crate whose whole content is the `gui` module, written in
Lazen and composed after the standard library by `library_text`. Seven tests in
`crates/lazalith-gui/tests/gui.rs` write Lazen programs that use it and read the
frame the device presented. The design is written down in `docs/lazen-gui.md`.

The step's claim — that the layer above the SDK is a library and the layer below
it is a host, and neither leaks into the other — is checkable rather than
aspirational: nothing in the 900 lines of the module names a window handle, a
device, an event queue, or SDL3.

### The ABI's six argument words shaped the whole API

This is the constraint that decided every signature, and it was found the hard
way. The first version of the library had

```
draw_label(canvas, width, height, x, y, text, colour)
```

which is **nine** argument words, and every program in the workspace stopped
building with `calling gui::draw_label needs 9 argument words, and the ABI has
6`. A `&mut [u8]` canvas is an address and a length; a `&str` is the same. So a
drawing function has spent four of its six words on *what to draw on* and *what
to write*, and has two left — which is exactly enough for a packed canvas size
and a packed rectangle, and not for a colour as well.

So the library packs its geometry, using packers the standard library already had:
`pack_surface` for a canvas's size, `pack_rect` for a rectangle, `pack_ink` for a
position and a colour. `draw_label(canvas, canvas_size, ink, text)` is six words,
`draw_button(canvas, canvas_size, rect, text)` is six, and a function the compiler
accepts is a function a program can call.

Two things this cost are visible in the API rather than hidden in it. A button's
colours are the library's, because theming one needs two words more than there
are; a program that wants its own draws the face with `draw_panel` and the text at
`button_label_at`, and both exist for exactly that. And `draw_canvas` blits over
the whole destination canvas, because a source view and a destination rectangle
are two words between them and there is no seventh.

### Widgets are geometry, not objects

Lazen v1 has no structs, so a widget cannot be a value with fields. There is no
`Button` to build or store: there is a rectangle, a state word, and
`draw_button`. The cost is real and is written down — a widget cannot carry
behaviour, so a program that wants a button to act on release writes that itself
— and the exchange is that a widget is three words of arguments rather than an
allocation, with no lifetime to get wrong.

### Three defects the tests found, all of them real

**A menu packed a 32-bit colour beside two 16-bit fields.** `pack_menu` wanted
items, a selection, and the bar's colour, which is 64 bits plus 32, so the
colour and the selection *overlapped*: a menu with the second item selected read
back its selection as zero and drew as if the colour were a very dark selection.
The bar's colour is a drawing parameter now, and `pack_menu(items, selected)` is
two fields that fit.

**`draw_text`'s surface is the canvas, not the text.** `draw_label` passed
`pack_surface(text_len * 8, 8)` — the string's own extent. But `draw_text` uses
the surface as the bounds every glyph is clipped against, so passing the text's
extent clips the text to a box at the *canvas origin*, and a label vanishes the
moment it is placed away from (0, 0). This one is worth recording because it
looks like a font bug and is an argument bug, and because `draw_text`'s own
documentation says the surface is a canvas's size while its parameter name
suggests otherwise.

**The pixel layout is A, R, G, B from offset zero.** Four of the seven tests
failed on their first run with `[0, 0, 0, 255]` where they expected opaque
black. `rgba` packs `0xAARRGGBB`, which *read little-endian* is B, G, R, A — the
reverse of the order the bytes are in memory. The tests now say so where the
colours are read, because this is the mistake every reader of this file would
otherwise make once.

### The hit test and the drawing have to agree

A rectangle is half-open on its far edges, so a widget at x = 0 with a width of 8
covers columns 0 to 7. `a_button_is_clicked_exactly_where_it_is_drawn` clicks six
points — two inside, four one pixel outside on each side — and each is its own
program run, because `button_clicked` is a pure function of the events it is
given. The events are *polled* rather than handed over as a zeroed array, so the
press that is tested is one that travelled through the device, the driver and the
ABI.

### Limits

- **The library is in every program.** v1 resolves names only within one unit, so
  there is no import machinery and `library_text` appends the GUI module to
  everything a program is compiled against, whether it uses `gui` or not. The
  standard library has the same property and the same cost. It is worth paying
  once and worth revisiting when v1 grows a way to import a module.
- **A button cannot be themed through its own call**, and a menu's items are
  rectangles rather than strings, for the reason in the section above: v1 has no
  array of `str`, and the ABI has no seventh argument word. Both are v1 limits
  rather than design choices, and both are written into `docs/lazen-gui.md`.
- **A text field has no cursor.** Insertion appends and the caret is always at
  the end. A cursor in the middle needs somewhere to put it between calls, and
  the only place v1 offers is another out-parameter, which would make every call
  site declare one and read it back.
- **A widget is expensive to draw.** `put_pixel` costs about 2300 instructions on
  this backend, so the tests use a 32-by-24 window and one-character labels. A
  widget set for a real window needs the register allocator or the bulk memory
  operation that `docs/lazen-graphics.md` says is missing; the library's shape
  does not have to change when either arrives.

### Next step

Step 74 is the **debug API**: `DebugController` and `DebugSession`, with run,
pause, step, continue, breakpoints, watchpoints, register and memory inspection,
stack, disassembly, snapshot and restore — and no way for a frontend to touch CPU
internals directly. The kernel already has the pieces a debugger needs
(`RoundRobinScheduler` validates the active binding on every step, and
`process_mut` from Step 72 makes guest memory readable), so the shape of the work
is an API over what exists rather than new machinery.

## Step 74 — The Lazalith Debug API

`lazalith-debug` is a new crate with two types: `DebugController`, which owns a
machine and a kernel, and `DebugSession`, which holds one process's debugging
state. Thirteen tests in `crates/lazalith-debug/tests/debug.rs` drive real
machines over real `.lzx` images. The design is written down in
`docs/lazen-debug.md`.

### "Do not allow frontends to manipulate CPU internals directly" is the shape of the API

The roadmap says it, and the crate is how it is enforced rather than how it is
promised. There is no `&mut LazalithMachine` in the public surface and no method
that hands one out. `registers()` returns an **owned** `RegisterSnapshot`, not a
`&RegisterFile`; `read_memory` returns an owned `Vec<u8>`; `disassemble` returns
owned text. There is no write-a-register and no write-memory, because there is no
way to do either that leaves the machine's own checks in place.

A `&RegisterFile` was the natural signature and would have been the wrong one.
`RegisterFile` is the CPU's own storage, so a shared reference to it is a view of
live state that changes under the caller: a frontend reading `r0` twice would get
two answers with no step in between, and a frontend that decided it needed to
*write* would find the obvious next step is to ask for a mutable reference. The
owned copy is a consistent snapshot, and the missing write path is the point.

This is the counterpart to the kernel validating the active binding on every step.
That check exists so nothing upstream can skip it, and a debug API that leaked the
machine would undo it from the other direction.

### Two bugs the tests found, both in the controller

**`run` cleared a pending pause before honouring it.** The pause check was in the
loop but the flag was cleared at the top of `run`, so a pause was always
discarded before the loop could see it. The moment a frontend asks for a pause is
the moment it is stopped, changes a breakpoint, and continues — so clearing on
entry discarded exactly the pause that mattered. The request is now taken *in* the
loop and consumed there, and `step` consumes it too, because a step is stopping.

**The trap vector was placed past the end of the kernel.** `boot` computed it as
`KERNEL_LOAD_ADDRESS + kernel_bytes.len()`, which is *after* the supervisor's own
`RFE`, so the first syscall return found no `RFE` and every program that made a
syscall died with `InvalidSyscallReturn`. The trap vector is a parameter now:
where a trap lands is a property of the machine's setup, and a controller that
guessed at it would be guessing where a kernel keeps its epilogue. The terminal and
the filesystem are parameters for the same reason.

### Watchpoints are a comparison, and the ISA is why

The machine has no watchpoint register — `lazalith-cpu` has a `DebugState` with a
single `single_step` flag that nothing reads, which is the whole of the debug
surface the ISA grew. So a watchpoint here reads the bytes under the address
before each step and compares them after, which is correct at instruction
granularity and costs one read and one comparison per watchpoint per step. That is
stated in the API rather than hidden, because a frontend that watches a hot address
should know before it does.

Finding a watchpoint's address took two attempts and the second one is the
interesting part. The first watched the stack pointer's own word, which the
program never writes after its prologue, so it never fired. The second uses a
program that *calls in a loop*, because a `CALL` pushes the return address at
`SP - 8` every time: a word the test can name without reading the frame layout.
Getting even that right needed a `stack_after_prologue` helper that steps until the
stack pointer *moves*, because a test cannot know how many instructions a prologue
is — it depends on the frame's size and on how many values the function has. A
test that guessed would break the day a register allocator arrives.

### The stack is reported, and it says it is not a call chain

`stack(words)` returns the stack pointer and the words above it, and
`has_call_chain` is `false`. The calling convention reserves the return address
below the frame but records no frame pointer, so there is no chain to walk: a walk
would be a walk of whatever numbers happened to be on the stack. A debugger that
printed addresses and called them a call stack would be showing a heap of numbers,
so the field says so. The roadmap's "stack" is delivered as what can honestly be
delivered, with the gap named.

### Limits

- **A snapshot is the session's debugging state, not the machine's.** Capturing
  the CPU, the devices and the processes is Step 75's subject with its own types.
  Folding a partial version of it in here would mean two definitions of "the state
  of a running program" that disagree, so `DebugSnapshot` holds breakpoints,
  watchpoints, the stop address and the step count, and `docs/lazen-debug.md` says
  so in the same place a reader will look.
- **There is no call chain**, as above, and no source-level information: there is
  no mapping from an address to a line, which is Step 76.
- **A watchpoint is O(watchpoints) per step**, and a run over a hot address with
  several watches is measurably slower than one without. That is the price of a
  machine with no watchpoint register.
- **`DebugError` boxes its sources.** Clippy's `result_large_err` was right:
  `KernelError` alone is 128 bytes, and an error type that size is returned on
  every path a frontend can take. Boxed, so the common case is a pointer.

## Step 75 — Machine Snapshots

`CpuSnapshot`, `DeviceSnapshot`, `ProcessSnapshot` and `MachineSnapshot` in
`crates/lazalith-debug/src/snapshot.rs`, with `DebugController::snapshot_machine`
and `restore_machine`; seven tests in `crates/lazalith-debug/tests/snapshot.rs`
plus five in the device crate. The design is in `docs/lazen-debug.md`, in the
same file as Step 74's because a snapshot is what a debug session saves.

### The trap frame is in the CPU snapshot, and that is not a detail

A program stopped in a syscall has its return address and saved registers in the
*trap frame*, not in the architectural state. A snapshot of the registers alone
would restore a machine the program could never return from. So `CpuSnapshot`
carries the trap controller's frame stack alongside the state, and restores
through `ReferenceInterpreter::restore`, which puts the architectural state in
first and *through the same validation a normal step uses* — a restore is not a
way to smuggle an inconsistent processor past the checks the machine makes every
step.

### A debugger cannot step into a syscall, and the reason is the kernel's shape

This is the significant finding of the step, and it is a limitation rather than a
bug. `Kernel::step` traps, dispatches **and** returns from the syscall before it
comes back, so the machine is never at rest inside one. A search for a live trap
frame across a whole run finds nothing: the only supervisor-privilege moment
observable from outside is the pre-handoff state.

So no snapshot support changes it — it is the shape of `Kernel::step`. Making a
syscall a stopping point means splitting that step so a trap is observable
between two of them, which is a change to the kernel rather than to the debugger.
It is written down in `docs/lazen-debug.md` under its own heading so the next
person to look for "why can't I step into a syscall" finds the answer rather than
the search.

The `CpuSnapshot` carries the frame stack anyway, and
`a_snapshot_carries_the_whole_processor` holds the snapshot and the machine to
agreeing about a frame at every one of 200 steps. A machine *can* rest in a trap,
and a snapshot that dropped the frames would silently omit the return path of
whatever was stopped in one.

### Only guest-visible state, and the list of what is not

The roadmap's sentence is the design constraint, so the exclusions are the
interesting part. A device's `elapsed` clock, a console's emitted output, the
machine's instruction count and virtual clock, and every framebuffer's pixels are
all **out**. The last is the one worth explaining: the pixels belong to the guest
and they live in the process's memory, so a snapshot that copied them would hold
a second copy of every framebuffer, which is the single thing the display
device's design refuses to be. `a_snapshot_carries_no_host_state` and
`a_display_snapshot_leaves_the_clock_out` hold this.

### The devices are the one thing that is encoded rather than cloned

`CpuSnapshot` and `ProcessSnapshot` hold clones, so a field added to a process
without a decision here is a field a snapshot carries. `DeviceSnapshot` holds the
device's *own* encoding, because a device's state is a device's business and a
common encoding in the `Device` trait would have to be a lowest common denominator
that lost whatever made each device different. `restore` checks the bytes are that
device's own, so a display's state cannot be put into an input device.

An input device's snapshot carries **the whole queue**, and that is the property
that makes one worth taking: a program *owns* the queue, it drains it, and a
snapshot that kept the counters but not the events would hand the same keystroke
to the program a second time. Only the guest can see that, because the guest is
what drained it.

### Two real bugs the tests found

**A restore left the sessions behind.** The processes came back correctly, but a
session whose process had exited still said `Exited`, so `run` refused to continue
a machine that was perfectly able to. The restore worked and the debugger did not
believe it. A restore now brings each session into line with the process it
watches.

**`InputDevice::restore` read its trailer from the wrong place.** The trailer is
*appended*, so it begins after the events — at 32 for a two-event snapshot — and
the code used the trailer's *length* (48) as its start offset, running off the end
of a snapshot of exactly the right length. Found by
`an_input_snapshot_carries_the_queue`, which restores a snapshot of a real drained
queue and then reads it back.

A third was a test's own fault and is worth recording because it is a limit of
what can be checked: the input device's shape test asserted that sixteen zero
bytes are not a valid event. They are — a key this build does not name, code zero —
so *only the length* can be refused, and the test now says so.

### Limits

- **A snapshot is a clone, so it is as large as the memory it holds.** A process
  with 256 KiB of framebuffer makes a 256 KiB snapshot. That is the price of a
  snapshot that cannot forget a field, and a machine with many processes would want
  copy-on-write sharing — which is real work and is not here.
- **A snapshot cannot be restored into a machine with a different shape.** The
  process count and the device list are checked first, so a refusal is a refusal
  rather than a half-applied restore. A snapshot taken before a process was
  spawned is not usable on the machine that process was spawned on.
- **No source-level information**, so a snapshot records addresses and not lines.
  That is Step 76.

---

# PHASE I FREEZE

Phase I — the hundred-step roadmap — is complete and is frozen. The frozen state is the
commit this file was last changed on, carrying the tag:

```text
lazalith-phase1-100
```

What is frozen:

```text
Phase I        Steps 1–100. Complete, tested, documented. Not a work in progress.
Hardening      In progress and NOT frozen as complete. Eleven of the roadmap's
               eighteen clusters are audited; the remaining seven are not.
               12 confirmed defects fixed, 26 defective tests fixed, 1279 tests.
Beyond Lazalith  Not started. No work of any kind is in this repository.
```

The hardening phase is deliberately *not* part of the freeze claim. The tag records the
Step-100 baseline and the hardening work that stood at the moment of freezing; it does
not assert that the audit is finished, and `docs/hardening.md` says so in its own open
items.

Verified at the freeze:

```text
cargo fmt --all --check                                     clean
cargo clippy --workspace --all-targets --all-features       clean (-D warnings)
cargo check --workspace --all-targets --all-features        clean
cargo test  --workspace --all-features                      1279 passed, 0 failed
nix flake check path:.                                       all checks passed
nix build path:.                                             result/bin/{lazen, lazalith-fuzz}
lazen run examples/hello/main.lz                            Hello, Lazalith
lazen run examples/window/main.lz                           exit 0
```

---

# BEYOND LAZALITH — SESSION 1: B1, B2, B3

The first Beyond-Lazalith session. `binstruction.md` §53's roadmap begins at B1;
this session completed the first three stages and left the fourth for the next
queued run.

**Read `docs/beyond-lazalith.md` first.** This file is the chronological record;
that one is the state.

---

## What this session inspected

Not a summary. The repository, at `7b424ce` (the Phase-I freeze), before anything
was changed.

| | |
| --- | --- |
| Workspace | 25 crate directories, 238 Rust files, 122 695 lines, one binary (`lazen`) |
| `git status` | clean except two untracked files: `binstruction.md` (the specification, never committed) and `myapp/main.lz` (a stray `lazen new` scaffold) |
| `.github/` | **did not exist.** No CI, no templates, no CODEOWNERS, no dependabot |
| `docs/` | 45 files, `project-state.md` at 353 KB |
| Tests | 1279 passing at baseline, 0 failing, 2m41s wall / 9m41s user in a debug build |
| Toolchain | no `cargo` or `rustc` on `PATH` outside Nix; everything runs through `nix develop` |

Read in full: `crates/lazalith-machine/src/lib.rs`,
`crates/lazalith-cpu/src/{interpreter,state,memory,lib}.rs`,
`crates/lazalith-devices/src/lib.rs`, `crates/lazalith-boot/src/lib.rs`,
`crates/lazalith-os/src/lib.rs`, `crates/lazalith-debug/src/lib.rs`,
`crates/lazalith-sdl3/{src/lib.rs,build.rs}`, `crates/lazalith-memory/src/{bus,cache}.rs`,
`crates/lazalith-cli/src/main.rs`, `crates/lazalith-fuzz/src/main.rs`, `flake.nix`,
`Cargo.toml`, and every crate manifest.

Surveyed in depth by subagents: the SDL3 boundary (18 SDL symbols, 32 `unsafe`
blocks, 1 `unsafe fn`, the C probe, every call site), and the LazOS↔VM coupling
(what `LazalithKernel` owns, the complete list of machine APIs the OS uses, the
boot path, every `LazalithMachine` construction site, and the full
test/fuzz/property inventory).

## What this session discovered

Findings that changed a decision, rather than confirming one.

**The execution engine could not have been made pluggable without moving the
state.** `LazalithMachine` held a `ReferenceInterpreter` *by value*, and the
interpreter *owned* `ArchitecturalState`, `ExecutionState` and `TrapController`.
A second engine would have had to reach inside the first — the exact door
`lazalith-debug` is built not to have — or hold a copy. A copy is a second
architectural truth, which `binstruction.md` §11 forbids. This is B3's whole
reason for existing, and it was not visible from the file layout.

**`DeviceManager<D>` is monomorphic.** `Vec<Entry<D>>`, one concrete device type
per machine, no trait object and no device enum. A machine can have a console
*or* a timer *or* a display *or* an input device, never two of different kinds.
`BootImage::machine_setup` refuses any non-empty device manager outright. This is
the blocker on `binstruction.md` §26's frontend/backend model and on any profile
with a device inventory, which is why it is B4's first half rather than part of
B4.

**The guest-visible kernel is two instructions.** `crates/lazalith-runtime/src/run.rs`
builds `NOP; RFE` and hands it to `BootImage::new`; the scheduler and syscall
dispatcher are host Rust running *outside* the machine. The OS uses a narrow slice
of the machine's API and never touches the bus, devices, clock or interrupts.
So the host scheduler is not part of the VM contract, and B25 replaces it.

**`docs/fuzzing.md` says "ten targets" and lists ten; `TARGETS` has eleven.** The
`package reader` target is missing from the document. Recorded, not fixed.

**`lazalith-sdl3`'s module documentation claims a test that does not exist.** It
says the event buffer size "is checked by a test, which compares it against the
headers' declared structs". There is no `tests/` directory in that crate. What
exists is a C static assert in `build.rs` plus eight compile-time `const`
assertions — both compile-time, neither a test. Recorded, not fixed.

**The SDL3 probe does not cover the values most likely to break.** It measures
`sizeof(SDL_Event)`, `sizeof(SDL_KeyboardEvent)`, three `offsetof`s, `sizeof(bool)`,
`sizeof(SDL_Keycode)`, `sizeof(SDL_Keymod)` and eight scancodes. It does *not*
measure `SDL_Rect`, `SDL_FPoint`, the event-type discriminants, the pixel format,
the scale mode or the access mode — those are hardcoded Rust constants, so a
header change that moved `SDL_EVENT_KEY_DOWN` would not be caught.

**The flake's source fileset omitted every document at the repository root.**
`instruction.md`, `README.md` and `LICENSE` were not in the tarball a release
builds from — which is precisely the failure the fileset comment warns about
("a document missing from the source tarball is not a build error, it is a
document that is missing from a release"). Fixed in this session.

**`nix build` builds the git tree, not the working directory.** A new source file
that is not `git add`ed is invisible to it, and the failure appears *inside the
sandbox* as `error[E0583]: file not found for module 'engine'` while `cargo check`
in the same directory passes. Reproduced here, twice. Harmless in CI, a real local
footgun.

**`nix flake check` against a warm store is a no-op.** It reported
`running 0 flake checks` in 3.4 seconds. A check that validates nothing looks
exactly like a check that passed.

## External research, and what it changed

| Source | What was taken | What it changed |
| --- | --- | --- |
| QEMU, *system emulation* | machine / CPU / accelerator / device / backend / boot as separate concerns; TCG as a JIT | the shape of `docs/machine-profiles.md` and of B4 |
| QEMU, *device emulation* | front end / bus / back end / pass-through, and "back ends can sometimes be stacked to implement features like snapshots"; "features will not be reported to the guest if the back end is unable to support it" | the proposed backend layer in `docs/device-model.md`, and the rule that a backend that cannot do something says so rather than emulating badly |
| GCC, *Overall Options* / *Invoking GCC* | preprocessing → compile → assemble → link, coordinated by a *driver* | the argument in `docs/toolchain.md` that splitting `lazen` into eight binaries is a usability question, not a stage prerequisite |
| GitHub Actions, *workflow syntax* | "any permission not named is set to `none`" | the permissions table in `docs/ci-cd.md`: `contents: read` everywhere, `contents: write` on the release job only |
| Rust `sdl3`, crate page | version 0.18.4 over `sdl3-sys 0.6.0+SDL-3.4.0`; the crate's own text says the bindings are still in progress and to "expect some bugs and missing features" | B20 is **not** recommended on the strength of a direction alone; the trade is 32 `unsafe` blocks in an already-probed file against a first third-party dependency that self-describes as incomplete |
| `zavg/linux-0.01` | `master` head is `5839d67d5825265fc665c9dc0ec2e767ff47a6dd`; the source was **read** at that revision, not cloned | the whole of `docs/linux-0.01-port.md` §3, which quotes real code rather than describing a kernel from memory |

## B1 — the architecture contract is frozen

Ten new documents, each grounded in the repository rather than in the
specification's description of it:

```text
docs/architecture.md        the platform contract, the real dependency order, and
                            the sixteen boundaries with the test that enforces each
docs/lza64.md               LZA32/LZA64 against LZ32/LZ64, and why no rename happened
docs/virtual-machine.md     the VM contract, the engine model, the switch guarantees,
                            and the six things a JIT must satisfy
docs/device-model.md        the device contract, the monomorphic blocker, and the
                            proposed frontend/backend split
docs/machine-profiles.md    versioned profiles, and why B4 starts with heterogeneous
                            devices rather than with a profile type
docs/toolchain.md           the toolchain split, the missing sysroot, the four gaps
                            the Linux port depends on
docs/compatibility.md       the compatibility tier, what it must not become
docs/linux-0.01-port.md     the pinned revision, and the port grounded in real source
docs/beyond-lazalith.md     the master Beyond document and the B1–B29 status table
docs/ci-cd.md               the automation, and what it does not check
```

Plus a `# BEYOND LAZALITH` section appended to `instruction.md`, which points at
the specification and at the record without restating either. The Phase-I roadmap
above it is untouched.

**A decision, not a deferral: the name LVMI was not adopted.** `binstruction.md`
§9 offers it and says not to finalize it until research confirms it is suitable.
Research did not confirm it. There is no interface in this repository to name —
there is one concrete type, `LazalithMachine<D>`, which is not a trait and could
not become one without splitting the machine — so naming it after an interface it
does not implement would invite callers to write code that cannot compile. The
name stays open for B19, where a real management API exists to name. The reasoning
is in `docs/virtual-machine.md`.

## B2 — GitHub CI/CD exists

There was no `.github/` directory. Now:

```text
.github/workflows/ci.yml        push to main and pull_request. fmt, clippy, test,
                                architecture, nix flake check, smoke. Layered with
                                `needs`, cheapest first.
.github/workflows/campaign.yml  nightly + workflow_dispatch. The fuzz campaign and a
                                release-profile property/differential sweep.
.github/workflows/release.yml   tag `v*` only. Builds, verifies artifacts BEFORE
                                publishing, emits SHA256SUMS and BUILDINFO.
.github/dependabot.yml          cargo (weekly) and github-actions (weekly).
.github/CODEOWNERS              one owner, with the architectural core called out.
.github/PULL_REQUEST_TEMPLATE.md
.github/ISSUE_TEMPLATE/{bug_report,limitation,beyond_lazalith}.yml, config.yml
```

Every action is pinned to a full commit SHA resolved from the upstream release tag
at the time of writing, with the version in a trailing comment. Every job runs in
the project's own Nix dev shell, because `lazalith-sdl3` compiles a C probe
against real SDL3 headers and a CI job without SDL3 would be green only by not
building the frontend.

**Status, stated plainly and not to be read otherwise: nothing here has run on
GitHub.** No tag has been cut, so **no GitHub Release exists**. No branch
protection, required status check, or security scan is claimed — those are
repository settings, not files, and `docs/ci-cd.md` §9 lists them as unknown.

## B3 — the execution-engine boundary

The one stage with code behind it, and the one `binstruction.md` §11 depends on.

```rust
// lazalith-cpu, new
Processor                    the canonical guest-visible state
ExecutionEngine<M: CpuMemory>  step(&mut Processor, &mut M) -> ...
EngineKind                   the closed set of engines, with Reference the only member
EngineError                  unknown engine, or a refused switch with a reason

// lazalith-machine
processor: Processor,                        // never replaced by a switch
engine: Box<dyn ExecutionEngine<Bus<D>>>,   // replaced by a switch
LazalithMachine::switch_execution_engine(EngineKind)
```

`ReferenceInterpreter` went from a state-owning struct to a zero-sized engine. Its
`execute` still builds a candidate `ArchitecturalState`, mutates the candidate and
only then commits — validate, calculate, commit — so the semantics are byte for
byte what they were; only the owner changed.

Fourteen tests in `crates/lazalith-machine/tests/engine.rs` and two new
architecture invariants. What is now *checked* rather than asserted:

- a switch preserves pc, sp, every register, status, execution state, the
  machine's own state, the virtual clock and device state;
- a switch **survives a live trap frame**, with the frame and the resume point
  unchanged — the case that matters most, because a fault in compiled code has to
  be able to return to the interpreter with the frame intact;
- the same program produces the same answer with a switch between every
  instruction;
- a switch is refused during an active user execution context, and on a faulted
  machine, each with a named reason;
- a reset after a switch still resets;
- **an `ExecutionEngine` implemented outside the workspace can take over a running
  machine's processor and hand it back** — the property a JIT will need, proven
  with a trivial counting wrapper so the test is about the seam and not about a
  compiler that does not exist;
- `no_execution_engine_owns_the_architectural_state` reads the sources and asserts
  that only `Processor` and the debugger's `CpuSnapshot` hold an
  `ArchitecturalState` — the invariant that makes a switch unable to fork the
  state, and the one whose failure would be invisible to every other test;
- `a_machine_holds_the_processor_and_the_engine_apart` asserts the two are two
  fields and that the switch exists as one checked operation.

**What is not true:** `EngineKind::ALL` has one entry. The switch replaces an
engine with one of identical semantics and discards its private state, which is
nothing. **There is no JIT.** `docs/virtual-machine.md` §4 lists the six things
one would have to satisfy, and none of them is satisfied.

## A change with a release consequence

`flake.nix`'s source fileset gained `instruction.md`, `binstruction.md`,
`README.md` and `LICENSE`. They are named rather than filtered because the root is
a mixed directory, and the comment explains why a named list here is safe where
the replaced list was not. Verified: the store path the package builds from now
contains all four plus `docs/`, `crates/`, `examples/`, `Cargo.toml` and
`Cargo.lock`.

## Scratch material removed

`myapp/main.lz` was a leftover `lazen new myapp` scaffold — the default template,
unreferenced by any code, test, flake or document. The repository already ships
`examples/hello/main.lz` for the same purpose. Deleted rather than committed, so
that the tree is clean and nothing implies a second example project exists.

## Tests and measurements taken

```text
cargo fmt --all --check                                     clean
cargo check --workspace --all-targets                       clean
cargo clippy --workspace --all-targets --all-features -D    clean, no warnings
cargo test  --workspace --all-features                      1295 passed, 0 failed
                                                             (2m53s wall, 10m19s user, debug)
                                                             baseline was 1279; +14 engine
                                                             tests, +2 architecture tests
nix flake check --print-build-logs                          all checks passed (2m42s)
nix build --print-build-logs                                result/bin/{lazen, lazalith-fuzz}
result/bin/lazen --version                                  lazen 0.1.0
result/bin/lazen check   examples/{hello,window}/main.lz    both ok
result/bin/lazen fmt --check examples/{hello,window}/main.lz both formatted
result/bin/lazen run     examples/hello/main.lz             Hello, Lazalith
result/bin/lazen run     examples/window/main.lz            exit 0
result/bin/lazalith-fuzz --list                             11 targets
actionlint .github/workflows/*.yml                          clean (also shellchecks every run:)
yq over the eight YAML files                                all parse
```

**The one measurement that was attempted and did not work:** a genuinely cold
`nix flake check`. A warm store makes it a no-op, and clearing the local store is
not something this session should do to a developer's machine. What is recorded —
2m42s against a changed tree with a warm binary cache, 3.4s and zero checks
against a warm one — is in `docs/ci-cd.md` §12, and the cold number is left
unmeasured rather than guessed.

## Known limitations, at the end of this session

1. **No workflow has run on GitHub.** Every one is locally validated and none has
   executed. The first run is the first real test of the runner assumptions,
   including whether `cachix/install-nix-action` is permitted here.
2. **No release has ever been produced.** The pipeline is written; no tag was cut.
3. **No branch protection, no required status checks, no security scanning.** All
   repository settings; unknown from here.
4. **`flake.lock` is not automated.** Dependabot has no Nix ecosystem, and the
   pinned nixpkgs revision is what the reproducibility guarantee rests on.
5. **Cold `nix flake check` is unmeasured.** See above.
6. **`aarch64-linux` is declared and never checked**, here or in CI. Unchanged
   from the freeze.
7. **One execution engine exists.** The switch is the operation; the JIT is not.
8. **`DeviceManager` is still monomorphic.** B4's first half.
9. **No sysroot, no freestanding C, no C preprocessor, no separate tool binaries,
   no command-line debugger** — the four toolchain gaps `docs/toolchain.md` §5
   lists, all of which B25/B26 depend on.
10. **No storage, audio, network, USB, PCI, VGA, RTC, serial, DMA, MMU, SMP or
    power states.** The hardware tiers in `docs/device-model.md` §4 say which of
    these are absent and which block.
11. **Two documentation defects found and recorded but not fixed**: the fuzz
    target count, and the `lazalith-sdl3` claim of a test that does not exist.
    Both are one-line corrections and are left for a session that is already
    editing those files.
12. **The Linux 0.01 source was read, not cloned**, and 8 files of the 84 were
    read in detail. `binstruction.md` §43's "read the whole repository" is the
    start of that work, not its end.

## Next step

**B4, machine profiles** — and its first half is heterogeneous devices, not a
profile type, because `DeviceManager<D>` cannot currently express a machine with
a console *and* a display. The dependency argument for B4 over B5 is in
`docs/beyond-lazalith.md` §5 and `docs/machine-profiles.md` §2.

B4 should build `lza64-native-v1` first, because B25 and B26 need a profile that
means "a machine with nothing on it but a CPU and memory" and a freestanding
kernel does not want a display it will never draw on. Its round-trip test — a
machine built from a profile, then inspected, must match the profile — runs in
the `architecture` job, and a fuzz target for the profile encoding goes in the
`campaign` job if the encoding is something malformed input can reach.

Two things the next session should carry forward:

- **Extend CI in the same commit as the subsystem it covers.** B2 made the cost of
  a new invariant a line in a workflow file rather than a habit.
- **Record a measurement rather than a guess** wherever a decision depends on how
  long something takes. Three numbers in this session would otherwise have been
  invented: the test suite's runtime, `nix flake check`'s, and the fact that the
  latter is a no-op against a warm store.

---

# BEYOND LAZALITH — SESSION 2: B4, MACHINE PROFILES

B1–B3 were committed at `40587af`. This session completed **B4**, the first
incomplete B-stage, and nothing else. B1's, B2's and B3's work was not redone.

Read `docs/beyond-lazalith.md` first. That is the state; this is the record.

---

## What was inspected before anything was changed

- `binstruction.md` §25, §26, §27, §53 — the hardware tiers, the device
  frontend/backend model, the machine-profile list, the roadmap.
- `crates/lazalith-devices/src/lib.rs` — the `Device` trait and `DeviceManager<D>`.
- `crates/lazalith-memory/src/bus.rs`, in full — the routing, the mapping
  overlap rules, and the `CpuMemory` implementation.
- `crates/lazalith-boot/src/lib.rs` and `crates/lazalith-os/src/memory.rs` — the two
  sets of layout constants.
- Every `console`/`timer`/`display`/`input` device constructor, its
  `address_len`, and its `peek`.
- `crates/lazalith-machine/src/profile.rs`, written this session and reviewed
  against the above.

## What was already there, confirmed

The B1 finding was right and still is: `DeviceManager<D>` was `Vec<Entry<D>>` with
one concrete `D`. No trait object, no device enum, no heterogeneous set. A machine
could hold a console, *or* a timer, *or* a display, *or* an input device, and
`BootImage::machine_setup` refused any non-empty device manager outright.

## What changed

### `impl Device for Box<dyn Device>` — the heterogeneous device set

Ten forwarding methods in `lazalith-devices`. `DeviceManager<Box<dyn Device>>` is
a `DeviceManager` of some `D: Device`, and every generic in `lazalith-memory` and
`lazalith-machine` was already written in terms of `D`.

**Not one existing call site changed.** `LazalithMachine<ConsoleDevice>` is still
monomorphic and still fast; `NoDevice` is still an uninhabited enum, so
`DeviceManager<NoDevice>` is still *a machine that cannot hold a device at all*
rather than an empty erased list. Both distinctions are tested, because the
stronger one is the reason the Phase-I path is untouched rather than merely
working.

`CONSOLE_REGISTER_BYTES` was extracted from a bare `1` in `ConsoleDevice::address_len`
— fine for a device, useless to anything that has to know the window size before
mapping one, which is exactly what a profile's overlap check has to do. Display and
input register sizes are now re-exported under unambiguous names alongside the
existing ones.

### `MachineProfile`, and the geometry it owns

`crates/lazalith-machine/src/profile.rs`, new. `ProfileName { architecture, family,
version }` displaying as `lza64-native-v1`; `MachineLayout` and `LZA64_LAYOUT`; a
device inventory with `DeviceClass`; a region list; a timer period; a compatibility
class; `validate()`; `machine_setup()`; `LazalithMachine::from_profile`; and
`matches_profile`.

`EngineKind`, `Processor` and the execution-engine boundary from B3 are untouched:
`Processor` is still owned by the machine, `EngineKind::ALL` still has one entry,
and `switch_execution_engine` still changes the engine without touching the
architectural state.

### The duplicated geometry, unified

`KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP` were each declared **twice** before
B4 — once in `lazalith-boot`, once in `lazalith-os` — with the same values. That
is the duplication that mattered: a change to one would have left one crate
describing a kernel window of one size and the other of another, and the symptom
would be a kernel loaded somewhere it does not fit.

`lazalith-boot` and `lazalith-os` now re-export from `lazalith_machine::LZA64_LAYOUT`.
**Every value and every public name is unchanged**; only the definition moved.

## Invariants now checked rather than asserted

| | |
| --- | --- |
| a profile with no version is refused | a profile that could be unversioned could be changed incompatibly |
| a profile for another ISA version is refused | the §27 compatibility claim, checked rather than labelled |
| a family that disagrees with its compatibility behaviour is refused | a *native* profile claiming AT hardware is wrong about more than its devices |
| `lza64-at-v1` can be named and is refused | it exists in §27; it does not exist in this build |
| a machine built from a profile is the machine the profile describes | a round trip, not a construction |
| two devices with one id are refused *before* a machine exists | otherwise a half-built machine and an error about mapping |
| two overlapping device windows are refused | the bus would refuse them, but the profile is what's wrong |
| an executable device window is refused | `Bus::map_device` refuses these |
| an unconstructible device class is refused, **not skipped** | skipping builds a machine missing what the profile promised |
| the machine's geometry is a literal in exactly one crate | see below |
| four devices of four kinds coexist, each window answering for itself | the thing B4's first half exists for |
| one guest reads three device windows in one program | what a guest actually experiences |
| a profile can describe two devices of one class | the inventory is a list, because a machine may have two displays |
| `lza64-native-v1` carries nothing a freestanding kernel must opt out of | B25/B26 need it minimal |

**The geometry rule was written wrong first, and the fix is the interesting part.**
The obvious version greps for the identifier `kernel_initial_sp`, which matches
both a re-export and a redefinition — so it cannot tell `KERNEL_INITIAL_SP: u64 =
lazalith_machine::…` from `KERNEL_INITIAL_SP: u64 = 0x0018_f000`, and would have
been a rule that cannot fail. It is written instead to look for a **literal**, and
it was verified: reintroducing `0x0018_f000` into `lazalith-os` makes it fail with
`["lazalith-machine", "lazalith-os"]`, and removing it makes it pass again.

## Two things this session got wrong

**A W^X rule that was not the platform's policy.** The first `validate()` refused
writable-and-executable RAM. The memory model builds what it is given, several
Phase-I OS tests use executable RAM for code, and B4's own guest test failed on the
rule. A profile that refuses something the platform genuinely has is inventing
policy, so the rule is gone, the test with it, and the reason is written into the
`validate` documentation so it is not added back.

**A test that reached for a seam the platform does not have.** The first attempt at
the device-inventory test wanted to inject an event, which needs a downcast from
`Box<dyn Device>`, which needs a production trait change made only for a test. It
was replaced with a real guest program that reads three device windows — which is a
stronger test anyway, because it checks the path a guest uses rather than a host
peek. No test-only seam was added to the device layer.

## One Phase-I defect found, recorded, not fixed

**`TimerDevice::peek` returns `Ok` and writes nothing.**

```rust
fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
    let _ = output;
    self.validate_read(offset, DataSize::Double)
}
```

It validates the register and discards the buffer, so `Bus::peek` hands the caller
back the bytes the caller already had. A debugger, or any host code reading a timer
window, gets zeros and cannot tell them from a real reading. `DisplayDevice::peek`
and `InputDevice::peek` both fill the buffer; `ConsoleDevice::peek` returns
`Unpeekable`, which is honest. The timer is the odd one out.

This is a wrong-answer defect in a device, which is the hardening phase's subject
and not B4's, and fixing it here would have meant changing a device the B4 tests do
not otherwise need. **It is recorded rather than fixed**, and the consequence is
that B4's device test reads the timer through a guest `LDZ` — which goes through
`Device::read` and does return the value — rather than through a peek, which does
not. `docs/machine-profiles.md` and the test's own comment say so.

## Tests and measurements

```text
cargo fmt --all --check                                      clean
cargo clippy --workspace --all-targets --all-features -D    clean, no warnings
cargo test  --workspace --all-features                       1318 passed, 0 failed
                                                              3m16s wall, 13m24s user (debug)
                                                              baseline 1295; +17 profile,
                                                              +4 layout, +2 architecture
actionlint .github/workflows/*.yml                           clean
```

New suites, and which job runs them:

| Suite | Tests | CI job |
| --- | --- | --- |
| `crates/lazalith-machine/tests/profile.rs` | 17 | `architecture`, named individually |
| `crates/lazalith-boot/tests/profile_layout.rs` | 4 | `architecture`, named individually |
| `crates/lazalith-cli/tests/architecture.rs` | +2 | `architecture` |

The layout tests live in `lazalith-boot` because it is the only crate that can see
`lazalith_boot`, `lazalith_os` and `lazalith_machine` at once — `lazalith-boot`
depends on `lazalith-machine`, so a test in the machine crate cannot see the other
two and the claim could not be checked there at all.

## CI implications

`.github/workflows/ci.yml`'s `architecture` job now names the profile round trip,
the geometry tests and the engine-boundary suite as separate steps, so a failure
reads as a profile failure rather than as "a test failed somewhere in 1318". No new
job and no new workflow: the suites are existing test targets, so `cargo test
--workspace` already covered them, and the point was only to make a failure legible.

`actionlint` is clean. **No workflow has run on GitHub**; that remains unknown, as
B2 recorded it.

## Limitations at the end of this stage

1. **`lza64-virt-v1` and `lza64-at-v1` do not exist.** Both are nameable and both
   are refused, with a reason.
2. **No backend layer.** `docs/device-model.md` §5 is a proposal. That is B5.
3. **No storage, audio, network, serial, VGA, PCI, USB, DMA, MMU, SMP or power
   states.** Unchanged from B1's inventory.
4. **No PIO address space.** §25 lists a PIO map as a profile property and LZA has
   none. `docs/linux-0.01-port.md` §3.2 makes it the largest single item in that
   port, because eleven driver files use `in`/`out`.
5. **No on-disk profile encoding.** A profile is a Rust value. A serialised one is
   a management API's input, and that API is B19.
6. **`TimerDevice::peek` is still wrong**, as above. Recorded, not fixed.
7. **`BootImage::machine_setup` still refuses a non-empty device manager.** A
   profile-built machine bypasses it entirely, so the two paths exist side by side:
   the boot path builds a Phase-I machine, the profile path builds a described one.
   Uniting them is B6's business, and doing it here would have touched the boot
   path for no gain.
8. **The trap vector is still set after construction** by each caller rather than
   being in the profile. A profile records the *interrupt model* in §27's list but
   this build has no way to express "and the vector is here" without a guest-supplied
   value, so the field is not in the type. Named rather than left to be noticed.
9. **aarch64-linux is still unchecked**, and no cold `nix flake check` timing was
   taken this session.

## Next stage

**B5, the device frontend/backend model separation** (`binstruction.md` §26). Its
first question is the one B4 deliberately left open: a backend is a host resource,
and nothing in the platform yet distinguishes a host resource from a device
register. The shape and the three rules are in `docs/device-model.md` §5; the
dependency argument for B5 immediately after B4 is in `docs/beyond-lazalith.md` §5.

B5 should start by answering the PIO question, because B4's profiles have no way to
express a PIO map and the Linux port needs one. The rest of B5 — a `Backend` trait,
a per-device backend slot, and the snapshot rule for a backend that cannot restore
its own state — follows from it.

Two things the next session should carry forward, both learned here:

- **Check that an invariant test can fail before trusting it.** The geometry rule
  was written in the obvious way, looked correct, and could not fail. Introducing a
  second definition and watching the test catch it is the only way to know.
- **Do not add a rule the platform does not have.** The W^X refusal was a
  reasonable-sounding policy that the memory model never adopted, and it cost a
  test to discover.

---

# B5 — the device frontend/backend model separation

`binstruction.md` §26. Preceded by `3b7cadb` (B4). The stage that answers B4's
left-open question: *a backend is a host resource, and nothing in the platform
distinguished a host resource from a device register.*

## What was already there, confirmed

- `Device` with ten methods, and `DeviceManager<D>` holding one concrete `D`.
- B4's `impl Device for Box<dyn Device>`, which is what lets a profile's device
  inventory be *held* and not merely described.
- `DeviceError` with eleven variants and no notion of a host resource.
- `DeviceClass::Block` in `lazalith-machine`, nameable and unconstructible.

## What changed

### `Backend` and `BlockBackend`, in `crates/lazalith-devices/src/backend.rs`

`Backend` is `kind`, `identity`, `reset`. `BlockBackend` adds `capacity`,
`writable`, `read_sector`, `write_sector`. Three implementations:

- `MemoryBlockBackend` — a flat buffer, writable or `read_only()`.
- `CopyOnWriteBlockBackend` — a sparse `BTreeMap` overlay over a read-only base.
  Reads fall through; writes allocate; the base is never touched.
- `AbsentBlockBackend` — refuses everything and names the device that asked.

`BackendIdentity` is a `u64` from an `AtomicU64` counter, not a pointer: a pointer
would make the identity mean "where this was allocated", which is host layout
leaking into something a snapshot compares.

### `BlockDevice`, in `crates/lazalith-devices/src/storage.rs`

The §26 chain end to end. A 64-byte register window, a data port, and whole 512-byte
sectors crossing the boundary. The translation is the whole of it: a register names
an offset in the *device*, a backend call names a *sector*, and nothing here lets
one become the other.

### `BlockStorage`, in `crates/lazalith-machine/src/profile.rs`

`DeviceProfile` gained `block: Option<BlockStorage>` next to `console_capacity`, and
`DeviceClass::Block` became constructible. `BlockStorage::CopyOnWrite` builds its
base read-only by construction, so the `WritableBase` refusal cannot be reached by
accident from a profile.

## Invariants now checked rather than asserted

Four of them, in `crates/lazalith-cli/tests/architecture.rs`, and they are a
different kind of check from the other twenty-one:

| Invariant | Test |
| --- | --- |
| A backend is not a device | `a_backend_is_not_a_device` |
| A backend signature carries no guest vocabulary | `a_backend_signature_carries_no_guest_vocabulary` |
| No device exposes the backend behind it | `no_device_exposes_the_backend_behind_it` |
| The backend layer is `no_std` | `the_backend_layer_is_no_std` |

**These are the four that could not be written any other way.** A `BlockDevice` with
a public `backend()` method behaves identically to one without it. Every functional
test in the workspace would pass. The boundary §26 asks for is a property of the
*shape* of the code, and absence is not observable by running the program — so it is
checked by reading the source, which is why `strip_comments` exists in that file: the
boundary is also *discussed* in the doc comments, and a check that matched its own
documentation would check nothing.

## Five real bugs, none of them caught by compiling

Recorded because each was invisible by inspection and every one of them shipped into
a build that compiled, passed `clippy`, and looked finished.

1. **A read transfer never completed.** `take` set `moved` but never returned the
   device to `Idle`, so `BLOCK_STATUS_BUSY` stayed set for the life of the machine
   and every subsequent command was refused as `Busy`. A device that reads a sector
   and then refuses every further command is not a device that works.
2. **`Transfer::remaining` reported 512 when idle.** It was derived as
   `SECTOR_BYTES - moved`, and `Idle` contributes a `moved` of zero — so a device
   waiting for a command reported a full sector outstanding. A program that polls
   `BLOCK_REGISTER_REMAINING` before issuing a command waits for a transfer that
   never started. The idle case is now a separate arm rather than the arithmetic one.
3. **The snapshot encoding did not match its decoder.** `BLOCK_SNAPSHOT_BYTES` said
   24; the encoder wrote 29. Every restore failed with `SnapshotShape`. Found
   immediately, because the constant and the encoder were written in different
   places — which is itself the lesson.
4. **`AbsentBlockBackend::kind` returned `BackendKind::Memory`.** A diagnostic
   reported a dead disk as a memory disk of zero sectors. It now has its own
   `BackendKind::Absent`.
5. **The data port was readable during a write.** `validate_read` allowed
   `BLOCK_REGISTER_DATA` whenever a transfer was in flight, and `in_flight()` is
   true for a *write* too — so a read mid-write consumed the port, turned the write
   into a read, and discarded the bytes the guest had already given **with no fault
   raised anywhere**. The gate is now `Transfer::Reading { .. }` specifically.

   This one was found by reading rather than by a failing test, which is the reason
   the test was then written and **verified to fail** with the fix reverted: it
   reports exactly one failure, `reading_the_data_port_during_a_write_is_refused`, and
   passes with the fix. A test that has never been seen to fail is not evidence.

A sixth was a *design* bug rather than a logic one: the block device's registers
were exported as `REGISTER_CAPACITY`, `REGISTER_SECTOR`, `REGISTER_STATUS` and so
on, and **`REGISTER_STATUS` already existed** in two other device modules. `lib.rs`
had been papering over it with an alias, and my first test run failed on
`ReadUnsupported` because the test imported the *display's* `REGISTER_STATUS`. They
are now `BLOCK_REGISTER_*` and `BLOCK_STATUS_*`.

## One thing this session got wrong

**I added a `backend_kind()` accessor and then wrote a test forbidding
`fn backend_kind(`.** Both are defensible — a kind reaches no resource — and having
them disagree in the same commit meant neither had been decided. The test was
right that this must be a decision rather than an accident, and wrong to make the
decision by accident. The accessor survives, the test asserts it deliberately, and
the architecture test now *requires* it be there so a future removal has to be
explicit.

## Limitations at the end of this stage

1. **Block storage is the only backend.** `Sdl3DisplayBackend` and
   `HostInputBackend` from the original §5 sketch remain proposals. The boundary is
   general; the first thing standing on it is a disk.
2. **No file or sparse-image backend.** `MemoryBlockBackend` is a buffer and a COW
   overlay is a `BTreeMap`. B8, in a host crate, because `lazalith-devices` is
   `no_std`.
3. **The overlay presents its base's capacity.** A copy-on-write layer is not a way
   to make a disk bigger. `BlockStorage::CopyOnWrite { base }` has no size of its
   own, and adding one would mean a growth policy that §31 does not specify.
4. **The data port moves 8 bytes per access** and refuses anything crossing the end
   of the sector. There is no sub-word or burst access, because `DataSize::Double`
   is the only shape the `Device` trait has for a window.
5. **No `lza64-virt-v1` profile.** A block device is buildable; a *profile* that
   has one is B8's, because it needs a file backend to be worth having.
6. **`TimerDevice::peek` is still wrong**, unchanged from B4. Recorded, not fixed:
   it is a separate defect and folding it in here would have made the B5 diff
   unreadable.
7. **No PIO address space.** B4's question survives, deliberately. The block
   device's data port is a *register* in an MMIO window, not a port in a PIO space,
   and calling it "port" makes the two indistinguishable in a profile.
8. **`BootImage::machine_setup` still refuses a non-empty device manager.** The two
   machine-construction paths remain side by side, as in B4.
9. **aarch64-linux still unchecked**, and no cold `nix flake check` timing was taken.

## Next stage

**B6, the common VM lifecycle / reset / boot contracts** — which is where the two
machine paths recorded in limitation 8 get united, and where the trap vector noted
in B4's limitation 8 belongs.

**B8, storage architecture**, is the other candidate and the more attractive: a
file backend over the boundary B5 just built is a contained piece of work, and it
is what turns `lza64-virt-v1` from a nameable profile into a real one.

## Two things the next session should carry forward

- **A constant and the code that produces it belong in the same place.** Limitation 3
  in the bugs above was a mismatch between `BLOCK_SNAPSHOT_BYTES` and the encoder
  that had to agree with it, written 300 lines apart. Nothing checks that but a
  test that exercises both, and that test existed only because the boundary work
  was tested at all.
- **Check the names a new device exports before exporting them.** `REGISTER_STATUS`
  already existed twice, and the collision was invisible until a test imported the
  wrong one. `rustc` cannot catch two constants with the same meaning in different
  modules; a person can, in about ten seconds, by looking at what else is called
  that.

---

# B6 — the common VM lifecycle, reset and boot contract

`binstruction.md` §6, §9, §34. Preceded by `4c2cfd2` (B5). New crate:
`crates/lazalith-vm`.

## What was already there, confirmed

- `LazalithMachine::reset`, which reset the machine and knew nothing about booting.
- `BootImage::start`, which built a machine *and* ran a bootloader in one call, and
  refused a non-empty device manager with `BootError::UnexpectedDevices`.
- `ProfiledMachine::from_profile`, which built a machine *with* devices from a profile
  and knew nothing about booting.
- `DeviceManager::snapshot`, and `MachineState`, which recorded whether a machine would
  execute.

## What changed

### One construction path: `Vm<D>`

`lazalith-vm` owns a machine, the stage it is in, the profile it came from, and where
the last boot stopped. It is the only place a lifecycle transition is decided.

`Vm` is **additive**. `LazalithMachine` is unchanged in what it can do, Phase-I's
`BootImage::start` still builds and boots, and `Vm::from_machine` adopts an existing
machine — which is how a Phase-I `NoDevice` machine gets a lifecycle. That is tested,
because "B6 did not break Phase-I" is a claim that deserves a test rather than an
inspection.

### The boot contract, and the check that did not exist

Before B6 there were two ways to make a machine and nothing compared them. The rule now
is **each is the authority for what it knows, and the overlap is checked**:

| Fact | Authority |
| --- | --- |
| memory — a kernel image, a kernel stack, a user region, with the OS's permissions | the boot image |
| devices, the timer, the geometry, the interrupt model | the profile |

Four fields are described by both and must agree: `physical_ram_start`,
`reset_vector`, `kernel_load_address`, `kernel_initial_sp`. A profile that moved the
kernel load address and an image built for this build's layout are **refused**, with
the field named. Before B6 they were accepted, and the failure appeared as a fault at
the first instruction of a kernel loaded somewhere else.

**Two of those four names are one fact.** `reset_vector` *is* `boot_rom_start`, and
`BootAgreement` reports it under the name `BootAgreement`'s callers think in. The test
asserts the refusal names `reset_vector` for a moved `boot_rom_start`, because giving
a caller two fields to go and fix for one mistake is worse than one.

### B4's recorded limitation, closed

`MachineLayout` gained `trap_vector`, so the interrupt model is part of the machine's
description rather than something each caller sets afterwards.
`ProfiledMachine::from_profile` applies it, and `matches_profile` checks it — the
round trip now verifies six things instead of five. A machine whose trap vector depends
on who built it is a machine whose interrupts land somewhere a caller that may not
have known there was a choice.

## Two real bugs, and the second one is the interesting one

### The read-only ROM, and a wrong `boot`

B6's first `Vm::boot` wrote the firmware into the machine's boot ROM with
`machine.load_bytes`. The memory model refused it:

```text
MemoryFault { access: Initialize, size: Bytes(524288),
              kind: ReadOnly { region_start: 0, kind: Rom } }
```

**The memory model was right and the implementation was wrong.** A profile's ROM window
is mapped read-only because a guest must not be able to write its own firmware — and
that same fact means firmware cannot be installed after construction. So `boot` was
rewritten to do what §34 says: build the machine from the *image's* memory and this
profile's devices, rather than patching firmware into a described machine. That
required `MachineProfile::build_devices`, which is a real API with a real reason to
exist.

The refused attempt is recorded in the method's documentation, because the next person
to read `boot` will otherwise reasonably ask why it rebuilds the machine rather than
loading a ROM into it.

### Snapshots could not undo anything

The first `Vm::restore` advanced the clock to the snapshot's time and **refused a
snapshot taken earlier than the machine had reached**. That is backwards, and it made
every snapshot useless: you could snapshot, run, and then not restore — and undoing
the run is the only reason to take one.

The fix was in the machine, not in the rule. `LazalithMachine::advance_clock` only
moves forward, while **devices had been able to rewind all along**, because each
device's `restore` writes its own elapsed count straight back. The device manager was
rewindable and the machine was not, and nothing had noticed because nothing had tried.

So B6 added:

- `DeviceManager::set_clock` — sets the manager's time without ticking, documented as
  the restore path and as a trap if used alone (a device's own count comes from its
  snapshot bytes; ticking here as well would apply the interval twice).
- `Bus::set_device_clock` — so the two clocks move together.
- `LazalithMachine::restore_time` — sets the processor's and the devices' clocks in one
  call, because a machine whose two clocks disagree is a machine where a device has
  been told a time the processor does not believe, and nothing else would report it.

`Vm::restore` now rewinds, and the two refusals that remain are about what a restore
cannot conjure: a snapshot from another stage (a machine cannot become booted by being
restored, because booting ran firmware) and a different device count (device snapshots
are restored by position).

### A rule that was wrong on arrival

`Vm` originally refused to map a device or load a region "while the guest is running".
Execution here is **synchronous**: `run` executes and returns, leaving
`MachineState::Running` as bookkeeping. That rule would have refused `reset()`
immediately after `run(1000)` — the most ordinary thing a caller does. It is now keyed
on an **active execution context**, which is the only genuinely in-flight thing
present, and the reasoning is in the module documentation so it is not re-invented.

## Invariants now checked rather than asserted

Three new rows in `crates/lazalith-cli/tests/architecture.rs`, on top of B5's four:

| Invariant | Test |
| --- | --- |
| The VM core depends on no host subsystem (§9's list) | `the_vm_core_depends_on_no_host_subsystem` |
| The machine does not know what a boot image is | `the_machine_does_not_know_what_a_boot_image_is` |
| The trap vector has one definition | `the_trap_vector_has_one_definition` |

The second is the one that matters. `Vm::boot` needs both a profile and an image, and
the tempting place to put that is inside `lazalith-machine`. It would compile, and it
would make the architectural machine depend on the ROM format — so a change to the
image format would become a change to the machine's API, and the machine would no
longer be usable without a boot image. The join is a separate crate for that reason.

## Tests

`crates/lazalith-vm/tests/lifecycle.rs`, 16 tests. The ones that matter most:

- `the_boot_agreement_can_be_made_to_fail_on_every_field` — forces each of the four
  disagreements in turn, so the check cannot pass by covering only some fields. A check
  that has only ever seen agreeing inputs has not been shown to be able to fail, and
  B4 recorded that lesson twice.
- `a_boot_onto_a_profile_that_disagrees_leaves_the_machine_alone` — checks the PC is
  unchanged after a refusal, which is the half of the contract that says the check
  happens before the machine is built.
- `a_machine_adopted_from_the_boot_path_can_hold_a_lifecycle` — Phase-I's
  `BootImage::start` still works, and a machine with no profile is *refused* by
  `matches_profile` rather than reported as agreeing.

**No test-only constructors were added.** The "restore a snapshot taken earlier" case
is produced by snapshotting, advancing the clock, and restoring; the disagreements are
produced by editing a `MachineLayout`, which is a public type with public fields. A
test that could only be written with a bespoke `#[doc(hidden)]` constructor is usually
testing a shape no caller can reach.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1377 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

Phase-I's boot, OS, stdlib and runtime suites are unchanged and green, which is the
real evidence that splitting `start` into `machine_setup` + `boot_into` did not disturb
them.

## Limitations at the end of this stage

1. **No firmware.** §34: "Do not implement them during this architecture pass." The
   firmware is a ROM a caller loads; what B6 builds is the *contract* for loading one.
2. **One ROM window.** `MachineLayout` has no nested firmware volumes, so §34's
   minimal-Lazalith / BIOS-like / UEFI-like layers cannot be described yet. They need a
   `FirmwareProfile`, which is B13.
3. **`Vm::from_machine`'s `stage` is the caller's claim.** There is nothing to compute
   it from — a `LazalithMachine` does not record whether a bootloader ran, and B6 did
   not add that field to it, because the fact belongs to the lifecycle. A caller that
   claims `Booted` when nothing booted gets a VM that believes it.
4. **`boot` discards the previous machine.** Deliberate and documented: a boot is a
   cold start, and preserving a previous machine's RAM and devices would be a way to
   get a machine that is half one profile's disk and half another's. But it means a
   booted VM cannot re-boot while keeping anything.
5. **`Vm` is above `lazalith-boot`, so `lazalith-boot` cannot use it.** A caller that
   wants a lifecycle around Phase-I's `BootImage::start` has to adopt the machine. That
   is `Vm::from_machine`, and it works — but it is a second path, and the run prompt's
   "unite the two paths" is only *mostly* satisfied: one path now *builds and boots*,
   and the other still exists as the Phase-I entry point it has always been.
6. **The clock is settable and therefore abusable.** `restore_time` will rewind a
   machine's clock without restoring device state, leaving devices ahead of the
   processor. The doc comment says to restore devices first and says why; nothing
   enforces it. A `restore` on `Vm` that a caller cannot get wrong would be better,
   and is noted rather than left for someone to trip over.
7. **PIO still does not exist**, unchanged from B4. B6 did not answer it, and a boot
   contract is the wrong place to.
8. **`aarch64-linux` still unchecked**, and no cold `nix flake check` timing was taken.

## Next stage

**B7, native display architecture** (§28). B4's `lza64_native_v1` has no display, and
`docs/device-model.md` §5.2 records that `Sdl3DisplayBackend` is a proposal. B7 is the
stage that puts a real display behind the B5 backend boundary.

**B8, storage architecture** (§31) is the other candidate and is a smaller piece of
work now: a *file* backend over `BlockBackend` makes `lza64-virt-v1` real, and B5's
invariant that a backend is never a guest interface is exactly the discipline a file
backend needs.

---

# B7 — native display architecture

`binstruction.md` §28, and §25's "Standard Computer Hardware → display". Preceded by
`1ec4a70` (B6).

## What was already there, confirmed

- `DisplayDevice` with a real guest-visible register interface: width, height,
  framebuffer address, present, present count, last present, ABI version, status.
- `PresentedFrame`, `frame_bytes`, `pixel_at` — a frame is a *description* the host
  resolves.
- `lazalith-sdl3`, the project's entire `unsafe` surface, with **no Lazalith
  dependencies at all**.
- `lazalith-gui/src/view.rs`, which pulled frames and converted them with
  `to_window_pixels` — correctly, and hard-wired to a window.

## What changed

### A `DisplayBackend` trait, in `lazalith-devices`

`open` / `present` / `close`, `no_std`, taking a resolved `DisplayFrame` of geometry
plus bytes. `HeadlessDisplayBackend` records geometry, a frame count and an FNV-1a
checksum of the pixels, so a test can assert *what was drawn* rather than only that
something was.

### `Sdl3DisplayBackend`, in `lazalith-gui` — **not** in `lazalith-sdl3`

The first attempt put it in `lazalith-sdl3` because that is where the SDL window lives.
It was moved on reading that crate's own documentation:

> "It has no Lazalith logic in it, knows nothing about machines, registers or guest
> memory, and cannot execute an instruction."

A display backend needs to know what a guest frame is. Putting it there would have made
the FFI boundary guest-aware, and the moment SDL could see a `DisplayFrame` the claim
that the unsafe surface contains no Lazalith logic would have stopped being true. So
the trait is declared in `lazalith-devices` and implemented in `lazalith-gui`, which
already depended on both. `the_sdl_ffi_boundary_does_not_know_about_guests` holds the
crate's zero-dependency property shut.

### `DisplayProfile`, and the two architectures that do not exist

`Native`, `VgaCompatible`, `ModernFramebuffer`. Only `Native` is constructible.
`DeviceProfile` gained `display: Option<DisplayProfile>`, and a profile naming one of
the other two is refused with `ProfileError::UnconstructibleDisplay` **naming which
architecture** — a caller who wrote `VgaCompatible` needs to know the VGA display is
missing, not that displays are generally unavailable, because those lead to different
work.

§28 requires VGA/EGA to be researched from historical primary sources first, and that
has not been done. So `VgaCompatible` carries no register map and no behaviour, and a
test says so by name so that implementing it later is a deliberate deletion rather than
an accident.

## The design decision worth arguing about

**A display device holds no backend, and that is the opposite of B5.**

A block device *is pushed to*: a guest's register write has to reach the host's storage
during the write, so the device holds a backend. A display is *pulled by* the host: a
guest writes pixels into ordinary memory and rings a present register, and the frame is
a description of where to look.

So:

- if the device called a backend during `present`, **one guest instruction would call
  into the host synchronously**. A host that had stopped answering — a window being
  dragged, a compositor that hung — would stall the machine with no fault and no
  timeout, and a guest could trigger it with a single store;
- pulling means the host renders on its own schedule, and a machine whose display nobody
  is watching costs nothing at all.

`pump_display` is therefore a free function the *host* calls, not a method on the
device. `a_pump_does_not_change_the_device_s_own_state` checks that a pump does not
move the present counter — if it did, the guest would see its own counter change because
a window happened to be on screen.

## Two error types, because of a derive

`DisplayError` is `Copy` and every variant of it is a structural fact about the device.
A backend failure is a fact about the *host* — SDL's own error text — and that is a
`String`.

The first attempt put `Backend { operation, detail: String }` into `DisplayError` and
the compiler refused: `String` is not `Copy`. That was the right refusal. Putting a host
message in a structural error would have given every geometry refusal a heap
allocation it does not need, so there are now two types — `DisplayError` (Copy,
structural) and `DisplayBackendError` (not Copy, carries host text), with
`From<DisplayError>` for a backend reporting a device refusal.

## A refusal that turned out to be misnamed

A host `read` that returns fewer bytes than the frame needs was producing
`FramebufferOverflow` — which is about a geometry that does not fit an addressable size.
The two are fixed by different people: one by the device's geometry check, the other by
whoever wrote the host's `read`. So `DisplayError::ShortFrameRead { expected, found }`
exists, and `a_short_read_is_refused_before_the_backend_sees_it` asserts the backend is
never handed a slice it would have to read past.

## Invariants now checked rather than asserted

| Invariant | Test |
| --- | --- |
| The SDL FFI boundary knows nothing about guests | `the_sdl_ffi_boundary_does_not_know_about_guests` |
| The display device holds no backend | `the_display_device_holds_no_backend` |

Both are claims about the *absence* of a thing, and both would pass on a rewrite that
added a backend to the device or a `DisplayFrame` to the FFI crate.

## Tests

`crates/lazalith-devices/tests/graphics.rs`, 13 tests: the profile taxonomy, that a
pump does not mutate the device, that a backend receives the geometry *and* the
pixels (by checksum, so a frame of the right size with the wrong contents fails), that
two different frames differ, that an unreadable framebuffer is an outcome rather than a
backend failure, and that a closed window keeps what was last drawn.

Three tests in `crates/lazalith-machine/tests/profile.rs` for the display profile.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1398 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **No VGA, and no modern framebuffer.** Named and refused. The VGA register map needs
   the primary-source research §28 requires, and that is B27's work with the Linux
   port.
2. **`Sdl3DisplayBackend` is untested at runtime.** It is in a crate whose whole point
   is to need a display device, so it cannot be tested in CI. The trait it implements
   and the geometry handling it does are covered through `HeadlessDisplayBackend`; the
   SDL calls themselves are not. That is a real gap and it is the crate's known cost.
3. **One guest pixel format.** 4 bytes per pixel, `0xAARRGGBB` little-endian, which is
   SDL's `RGBA32`, so `framebuffer_bytes` is a copy. A second guest format means a
   second backend, not a branch in this one — and the copy is named so that the day the
   format changes, the function to change is findable.
4. **The GUI's `view.rs` still has its own `to_window_pixels`.** B7 added the backend
   boundary and did not rewire the debugger's screen panel through it, so there are
   two paths from a `PresentedFrame` to pixels. They agree today. Unifying them is
   mechanical and is noted rather than done, because doing it would have meant changing
   a panel that 1398 tests currently agree on.
5. **`DisplayProfile` is per display device, not per machine.** A machine with two
   displays could name two different architectures. That may be right; it has not been
   argued either way.
6. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B8, storage architecture** (§31). A *file* backend over B5's `BlockBackend` is a
contained piece of work now that the boundary exists and its invariant is checked, and
it is what makes `lza64-virt-v1` — which `docs/machine-profiles.md` §4 records as
"not built, needs a storage backend" — into a real profile rather than a name.

---

# B8 — storage architecture

`binstruction.md` §31, and §25's "Standard Computer Hardware → block storage".
Preceded by `356ba46` (B7). New crate: `crates/lazalith-storage`.

## What was already there, confirmed

- B5's `BlockBackend` trait, and `MemoryBlockBackend` / `CopyOnWriteBlockBackend` /
  `AbsentBlockBackend`.
- B5's `BlockDevice`, with the 64-byte register window and the data port.
- `BlockStorage` on a machine profile, so a profile can name a disk.

**None of it touched a host file.** Every backend B5 built was in memory, which was
deliberate — `lazalith-devices` is `no_std` — but it also meant "storage" meant "a
buffer", and §31 asks for raw images, sparse images and snapshot layers.

## What changed

### `lazalith-storage`: three backends that need a filesystem

| Backend | Kind | On disk |
| --- | --- | --- |
| `RawImageBackend` | `RawImage` | the file *is* the disk, byte for byte |
| `SparseImageBackend` | `SparseImage` | a header, then a `[sector][data]` record per written sector |
| `SnapshotLayer` | `SnapshotLayer` | nothing — it is in memory over another backend |

`BackendKind` gained `RawImage`, `SparseImage` and `SnapshotLayer`. Three new kinds
rather than one `File`, because they are not interchangeable: a raw image of a 64 GiB
disk with three sectors written is 64 GiB on the host and a sparse one is 1.5 KiB, and a
diagnostic that called them both "file" would be wrong about where the space went.

### A sparse image reserves its first sector

A sparse image's byte 0 is a magic word, not the disk's byte 0. That is what stops a
raw image and a sparse one being confused: `a_raw_image_is_the_disk_byte_for_byte`
checks that a raw image's sector 3 is at byte 1536 with nothing before it, and
`a_sparse_image_refuses_a_raw_file_and_a_capability_mismatch` checks that pointing a
sparse backend at a raw file is refused rather than reading a magic word where a
partition table belongs.

The index is **not a separate file**. It is rebuilt on open by walking the
`[sector][data]` records, so a sparse image is not held hostage to a second file that
could be lost or desynchronised. A repeated sector number on that walk is
`IndexMismatch`, detected at open rather than discovered later as a sector that reads
back as somebody else's data.

### `SnapshotLayer`, and why it is not B5's copy-on-write

B5's `CopyOnWriteBlockBackend` is how a backend is *built*: a profile says "copy on
write" and the overlay exists from the start. A `SnapshotLayer` is what a *running*
machine takes — writes go here, the base is untouched, and dropping the layer puts the
machine back on the base with every write since gone.

Two properties that are tested rather than asserted:

- **a discarded layer refuses.** It does not answer with zeroes. A discarded layer that
  returned zeroes would look exactly like a fresh disk, and a machine restored onto one
  would be a machine that had silently lost its writes.
- **`renew()` mints a new identity.** A layer reused after being discarded has the same
  `BackendIdentity` as the machine snapshot that recorded it, so restoring that snapshot
  would be *accepted* and would put a different set of writes in place. That is the
  exact failure B5's identity rule exists to prevent, and it is the reason `renew`
  exists.

The layer requires a **read-only base** for the same reason B5's overlay does: a layer
over a writable base is not a snapshot, because discarding it would not discard
anything.

### The guest controllers §31 names, none of them built

`controllers::BlockController` is `IdeAta`, `VirtIoBlock`, `NvMe`, and
`is_buildable()` is `false` for all three. §31 says "do not implement all at once", and
the three are not variants of one thing: a guest driver, a register map, a set of
guest-visible semantics, and a different set for each.

**The middle box of §31's chain is deliberately empty.** It reads:

```text
guest block device
 ↓
controller/device model
 ↓
host storage backend
```

The first box is B5's `BlockDevice` and the last is this crate. The middle box — a
*controller* — is absent, and that is a decision rather than an omission: `BlockDevice`
already is the thing a guest driver talks to, and adding a controller on top of it would
mean a guest-visible device whose only job is to be another guest-visible device. The
controller belongs with a compatibility machine (B27) or a virtio-style device, where it
is the attachment point that actually needs a bus.

## Two new `BackendError` variants, and why

`Unavailable` and `Corrupt`. Both are needed because a host failure has to reach a guest
as *something*, and the existing variants were all about arithmetic:

- `Unavailable` — the storage could not be reached. **Not `Corrupt`**, because a guest
  told its image was corrupt would conclude the data was damaged and go looking for a
  backup of a disk that is fine and merely unmounted.
- `Corrupt` — the storage holds data that does not make sense: a length that is not a
  whole number of sectors, an index that disagrees with its data.

Neither names a path, a filename or an errno. `StorageError` in `lazalith-storage` holds
all of that and is never guest-visible, and
`a_backend_error_cannot_name_a_host_path` holds the rule down.

## Invariants now checked rather than asserted

| Invariant | Test |
| --- | --- |
| The storage backends take no guest vocabulary | `the_storage_backends_take_no_guest_vocabulary` |
| A `BackendError` cannot name a host path | `a_backend_error_cannot_name_a_host_path` |

The second is the one worth having. Every variant of `BackendError` is reachable from a
register access, so a variant carrying a `PathBuf` would hand the guest the host's
directory layout — which is why the conversion in `From<StorageError> for BackendError`
collapses to two facts and the host detail stays behind.

## Tests

`crates/lazalith-storage/tests/storage.rs`, 21 tests. The ones that matter:

- `a_raw_image_is_the_disk_byte_for_byte` reads the file with `std::fs` rather than
  through the backend. A test that read it through the backend would pass even if the
  layout were wrong, because both sides would be wrong together.
- `a_sparse_image_occupies_only_what_was_written` asserts `bytes_on_disk() < capacity`.
  This is the property most likely to break: an implementation that wrote zeroes for
  holes would pass every functional test and be 64 GiB on disk.
- `rewriting_a_sector_does_not_grow_the_image` — seven writes to one sector is still one
  sector, because a loop that rewrites one sector would otherwise make the image grow
  once per iteration.
- `discarding_a_layer_puts_the_machine_back_on_the_base` and
  `a_renewed_layer_has_a_different_identity`.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1421 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **No guest controller.** IDE/ATA, VirtIO-blk and NVMe are all named and unbuilt, so
   there is no guest-visible way to reach a disk. §31's middle box is empty by decision
   (above), and the controllers are empty by §31's own instruction.
2. **`lza64-virt-v1` is still not buildable**, which `docs/machine-profiles.md` §4 has
   said since B4. B8 made a file backend exist; it did not make a profile that can name
   one, because `BlockStorage` has no file variant. That is the obvious next piece and
   is B8's remaining half.
3. **A sparse image's index is in memory, proportional to allocated sectors.** A bitmap
   would be 1 bit per sector — 8 MiB for a 64 GiB disk — and would be better for a
   mostly-full image. `BTreeMap` is better for the common mostly-empty case and worse
   for the other. The trade is documented on the type and neither is right.
4. **The index is rebuilt by walking, not stored.** That is robust against a lost index
   file and O(allocated) on open. A stored index would be O(1) on open and would need its
   own consistency protocol.
5. **No `flush`-on-drop, no fsync, no write barriers.** Every write is `write_all` plus
   `flush`, which is a userspace flush, not an `fsync`. A host that loses power can lose
   a sector. Correct durability is not in §31 and is not here.
6. **A raw image refuses to shrink rather than truncating.** A caller that wants a
   smaller disk must use a new file. This is deliberate and tested, but it means
   "resize this image" has no API.
7. **`SnapshotLayer` is in a `std` crate and needs no `std`.** Documented, and it is a
   placement judgement rather than a necessity.
8. **PIO still does not exist.** Unchanged since B4. A guest controller — IDE/ATA above
   all — needs a port space, so B12 and B27 both depend on that question being answered.
9. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B9, input architecture** (§30). `InputDevice` exists from Phase-I and B4's profile can
name one, but — like the display before B7 — it has no backend, and §30 asks for input
and USB as a pair.

**B8's remaining half** is smaller and would finish §31: a `BlockStorage::File` variant
so `lza64-virt-v1` becomes a profile a caller can actually build.

---

# B9 — input architecture

`binstruction.md` §30, and §25's "Standard Computer Hardware → keyboard, mouse".
Preceded by `1dfc92b` (B8).

## What was already there, confirmed

- `InputDevice`: a real guest-visible register interface with an event queue, poll,
  pending, delivered, injected and status registers.
- `Event`, `EventKind`, and an `InputError` that distinguishes a full queue from a
  capacity mistake.
- `host_input`: `HostKey`, `HostAction`, and `HostScript` with a `replay` that pushed
  events **directly into the device**.

## What changed

### `InputBackend`, a source rather than a sink

`InputBackend::poll` asks the host for the next event; `pump_input` moves events from a
backend into a device up to a budget. `ScriptedInputBackend` and `AbsentInputBackend` in
`lazalith-devices`; `Sdl3InputBackend` in `lazalith-gui`.

**Source, not sink, and the reason is B7's.** Input is produced by the host
asynchronously, and a guest's register read must not call into SDL — a host that had
stopped answering would stall the machine with no fault and no timeout. So the device
never asks for an event; a free function the host calls writes it.

`HostScript::replay` still exists and still returns `Result<u64, InputError>`, but it is
now a `ScriptedInputBackend` being pumped. **The capability that buys is two sources on
one device** — a deterministic script *and* a real keyboard — which was impossible while
`replay` reached into the device itself.

### `InputProfile` and §30's USB hierarchy

`Virtual` (buildable), `Ps2`, `UsbHid` (named, unbuildable). `usb::LEVELS` is
`[Controller, Bus, Device]` with `is_buildable() == false` for all three.

PS/2 needs the PIO address space LZA does not have — **B4's recorded, still-open
question** — and USB HID needs a bus, which is §33. §30's fourth level, the host
backend, is this crate and exists.

### A design error, caught and fixed: `poll` has to be fallible

The first `InputBackend::poll` returned a bare `Option<Event>`. That left the SDL backend
nowhere to put a failure: it could only count it as a dropped event, which made "SDL is
broken" and "the keyboard is unplugged" **indistinguishable to a caller** — and those
are the two diagnoses a user actually reports. It also created an `InputBackendError::
Host` variant that could never be constructed, which is the clearest sign a design is
wrong.

So `poll` is `Result<Option<Event>, InputBackendError>` and an SDL failure is reported
with SDL's own words.

## The bug the stdlib suite caught

`ScriptedInputBackend` returned an action's **first** event and advanced its cursor past
the action. `HostAction::Printable` is a key *and* a character — two guest events — so
every `Printable` in a script silently lost its second event.

Nothing in `lazalith-devices` noticed, because every device test used single-event
actions. What caught it was
`lazalith-stdlib`'s `a_program_reacts_to_scripted_keyboard_input`: a real guest program,
booted, handed a six-event script, and asked to count what it received. It failed with
`"the program read the whole script: \"\""` and exit code 1.

**This is the strongest argument in this project's history for keeping the end-to-end
suites.** The boundary was new, the unit tests were new, and they all passed on a
backend that dropped events — because the bug was only visible in a guest that asked for
more than one. Two tests now exist at the devices level
(`a_multi_event_action_delivers_every_event_in_order` and
`a_multi_event_action_survives_the_pump`) so it is caught in the crate that owns it, and
the stdlib test remains as the one that boots a real program.

## Invariants now checked rather than asserted

| Invariant | Test |
| --- | --- |
| The input device and the input backend do not hold each other | `the_input_device_and_the_input_backend_do_not_hold_each_other` |

The first draft of that test forbade `&mut InputDevice` anywhere in the module, and
**failed on `pump_input`** — which is the boundary, written as a free function precisely
so the one place the device is written is visible in a signature. The test now checks the
`InputBackend` trait body and every `impl InputBackend for` block, and separately
asserts that at least two implementations exist so the test cannot quietly stop checking
the rest.

## Tests

`crates/lazalith-devices/tests/input_backend.rs`, 17 tests: the pump (delivery, idleness,
budget, a zero budget refused, a full queue reported, a host failure propagated rather
than flattened), the boundary (a pump does not move the device's counters, two sources on
one device), the profiles, and the scripted backend including the two multi-event tests.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1439 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **No PS/2, no USB HID.** Named and unbuildable. PS/2 needs PIO, which needs B4's
   question answered; USB HID needs a bus, which is §33.
2. **No USB bus, controller or device.** `usb::LEVELS` is a list of names. A USB device
   with no bus behind it is a device that can be described and not attached.
3. **The SDL backend maps only key presses.** SDL also reports mouse motion, buttons and
   text, and `SdlInput` currently reduces most of that away. `EventKind` and `HostAction`
 * both model pointers, so the guest side is ready; the SDL side is not. §30 asks for
   "keyboard/mouse/controller" and only the keyboard is mapped.
4. **Auto-repeat is dropped**, deliberately, so a replayed session and a live one produce
   the same event stream. The counter `dropped()` makes that visible.
5. **The keycode is sent, not the scancode.** Keycodes are layout dependent, so "the
   user pressed the key labelled Q" arrives as whatever that layout produces. A guest
   that cares about *position* needs the scancode, and there is no way to ask for it.
6. **`InputBackendError::Host` is unreachable from a scripted backend**, so
   `HostScript::replay`'s handler for it returns `QueueFull { limit: 0 }`. That is the
   least-wrong mapping and it is documented as such rather than `unreachable!()`-ed,
   because a panic in a library is worse than an approximation nobody can reach.
7. **PIO still does not exist**, which now blocks three things: B9's PS/2, B12's
   expansion bus, and B27's AT machine. It is the single highest-leverage unbuilt thing
   in the roadmap and it has been open since B4.
8. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B10, audio architecture** (§29). The third of §26's device list, and the one where the
pull/push question is *different again*: audio has a rate, so a backend has a clock and a
device has a buffer. That is worth getting right rather than copying B9's shape.

**B12, expansion bus / device discovery**, is the other candidate and is now the
bottleneck for several stages above: USB (§30), PIO, and B27's compatibility machine
all need a bus.

---

# B10 — audio architecture

`binstruction.md` §29, and §25's "Standard Computer Hardware → audio". Preceded by
`6d95fa6` (B9).

## What was already there, confirmed

- No audio device of any kind. §29 was entirely unimplemented.
- `Device` with ten methods, none of which could raise an interrupt or reach memory.

## Two gaps §29 exposed, and what was done about each

§29's list is: PCM playback, PCM capture, sample rate, sample format, channel count,
buffers/rings, **DMA**, **interrupts**.

### Interrupts — built

A device that consumes a *rate* learns something in `tick`, and a guest that must be
told has no other way. Polling a status register makes the guest busy-wait at its own
rate, and the device knows when its ring is half empty and the guest does not.

`Device` gained one method:

```rust
fn take_interrupt(&mut self) -> Option<InterruptId> { None }
```

**Taken, not read**, which is what makes it an edge: a `&self` peek would re-deliver
every cycle, and a guest clearing its own interrupt would race with the machine reading
it. `DeviceManager::take_interrupts`, `Bus::take_device_interrupts` and
`LazalithMachine::deliver_device_interrupts` carry it, and the machine asks *after*
every clock advance rather than from inside `tick` — so a device never needs to know an
interrupt controller exists and never delivers anything itself. That is what keeps
guest-controller vocabulary out of `Device`.

The default is `None`, so **no existing device changed**. That is the test of whether an
addition to a trait is additive or invasive.

### DMA — not built, with the reason

A device has **no way to reach guest memory**: `Device` gives a device its registers
and nothing else. Handing every device a memory handle would put an address space in
front of every device in the platform, including the ones that must not have one — the
display, the input device and the block device are all better off without it.

So audio samples move through the data port, one access at a time, **exactly as B5's
block device does**. That is not a workaround; it is the same decision reached
independently, and it is recorded here so the next person does not read the absence as
an oversight.

§29's verb for DMA is *investigate*, not implement, so this is within scope. It is
listed in limitations below with what it would actually cost.

## The device

`AudioDevice`: playback and capture rings, a format register triple, control, threshold,
a data port, status and level. `SampleFormat` is `S8`/`S16Le`/`S24Le`/`F32`;
`AudioFormat` binds the three so a rate cannot be set without a format — a rate in Hz
means nothing without knowing how many bytes a sample is.

**`AudioFrame` is §29's "host backends must be independent of the guest ABI" as a
type**: a format and a `&[f32]`. No registers, no offsets, no Lazalith structs. A
backend written against it could be driven by a different machine's audio device, and the
alternative — a backend taking a guest-visible struct — would make every host backend a
second guest driver.

`NullAudioBackend` is not a null object for convenience. A machine with no sound card
must still consume what a guest produces, or the guest gets `RingFull` and reports a
broken device.

## Three real bugs, and a pattern worth naming

1. **`AUDIO_STATUS_READY` read the wrong ring.** It tested the *playback* ring, so a
   guest in capture mode was told its buffer was empty while the host had four samples
   waiting for it. The bit answers "can the guest read a sample now", and a guest reads
   from capture.
2. **The snapshot encoder and decoder disagreed.** `snapshot` wrote `low_water_raised`
   as `u64::to_le_bytes()` — **eight** bytes — while `restore` read one. Every field
   after it was shifted by seven, and a restore read the low-water flag out of the
   middle of the playback level. The device worked perfectly; the snapshot was silently
   corrupt. Only a test asserting `snapshot().len() == AUDIO_SNAPSHOT_BYTES` noticed.
3. **A full-scale sample did not survive the register port.** Scaling by `i16::MAX`
   maps −32 768 to −1.00003, so a guest reading back its own full-scale negative got a
   number outside the range it wrote. Scaling by 32 768 makes −1.0 exact.

**The second of those is the fourth time this project has had a constant and the encoder
that must agree with it written in different places** — B5's `BLOCK_SNAPSHOT_BYTES`, and
now this. The fix that generalises is not a comment: it is a test that compares a
produced length against the declared constant, which now exists in both places.

## Tests

`crates/lazalith-devices/tests/audio.rs`, 24 tests. The ones that matter most:

- `a_low_water_interrupt_is_raised_once_and_latches` — checks the *latch*, not just
  that an interrupt arrived. A device that raises every cycle passes any test that only
  checks the first.
- `a_rate_mismatch_is_reported_rather_than_resampled` — and `Mismatch` is its own
  outcome rather than `Idle`, because "the host is at 44.1 and the guest asked for 48"
  and "the guest produced nothing" are the two things a person debugging silence needs
  told apart.
- `a_backend_needs_no_guest_types_at_all` builds an `AudioFrame` with no device, no
  machine and no register in scope, which is only possible if the type carries no guest
  vocabulary.
- `a_snapshot_carries_the_format_and_the_levels_but_not_the_audio`.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1463 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **No DMA.** A device cannot reach guest memory; see the reasoning above. Building it
   means a DMA engine on the machine that services device *requests* (address, length,
   direction) rather than handing every device a memory handle. That is a bus change
   and belongs with §33.
2. **No host audio backend.** `NullAudioBackend` and `RecordingAudioBackend` stand in.
   SDL's audio API exists but nothing wraps it — the same gap B7 recorded for
   `Sdl3DisplayBackend`.
3. **Playback and capture share one ring.** A real duplex codec has two. §29's job here
   was the ring and the rate, not duplex, and it is named as a simplification.
4. **A sample crosses the port as 16-bit signed, always.** A guest using `F32` or `S24Le`
   has its samples quantised on the way in and out, so a 32-bit float format is a lie
   about precision. Clamping is explicit, so nothing wraps, but a guest that wanted
   float precision is not getting it.
5. **`AUDIO_REGISTER_LEVEL` reports the playback level only** — it reads as 0 for a
   capture-only guest. It is the register a playback driver polls, and the capture side
   has `AUDIO_STATUS_READY`; making one register mean two things would be worse, so the
   second is missing instead.
6. **No SoundBlaster, AC'97 or HDA.** §29 puts them in compatibility-machine work, which
   is B27.
7. **No PIO**, unchanged since B4. Blocks B9's PS/2, B12's bus and B27's AT machine, and
   is still the highest-leverage unbuilt thing in the roadmap.
8. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B11, networking architecture** (§32). The fourth of §26's device directions, and the
first one with a *two-way* stream: a host socket in, a guest NIC out, with a host that
may be slow, may refuse, and may be a different machine than the guest believes.

---

# B11 — networking architecture

`binstruction.md` §32, and §25's "Standard Computer Hardware → network". Preceded by
`98a41c4` (B10).

## What was already there, confirmed

- No network device of any kind. §32 was entirely unimplemented.
- B10's `Device::take_interrupt`, which a network needs and which therefore cost nothing.

## The design, and the one property it turns on

§32 draws `Guest NIC → Lazalith NIC → host backend` and names five host backends —
NAT, user-mode, bridged, host-only, tap/socket — as "host-side implementation details".

A network is the **fourth** shape and the first two-way one. B7's display is pulled, B9's
input is pushed, B10's audio has a rate. A network sends without being asked and receives
without expecting, and the host may be slow, may refuse, or may be a different machine
from the one the guest believes it is on.

So a guest's transmit gets one of three answers:

- **Accepted** — a frame left the guest and reached the host;
- **Refused** — the host would not take it, and the guest is told *why*;
- **Dropped** — it is gone, and somebody is told.

**There is no "sent".** `NetworkDevice` has **no method that completes a transmit
without a `&mut dyn NetworkBackend`**, so a device reporting "sent" on its own could not
be written. A device that reported a transmit complete when the host had not accepted the
frame would tell a guest a packet was delivered that never left the machine — and a
network stack would retransmit nothing, because nothing looks wrong.

`NullNetworkBackend::send` returns `Ok(false)`, not `Ok(true)`, and that is the point: a
machine with no network has nowhere to put a frame, and a guest that believes otherwise
will not retransmit it.

## A frame is opaque, and that is a decision

§32 does not name a link-layer format. `LazFrame` is a length and bytes, deliberately.

An `Ethernet`-shaped frame would have decided the link layer *inside the guest-visible
device*, where every later decision inherits it — and §32's own words are that the
link-level concerns are host-side implementation details. A guest that wants Ethernet
framing builds one; the device carries octets.

`MAX_FRAME_BYTES` is 1514 and `MTU` is 1500, and the device refuses a larger frame
because a host receiving one from a bridge will already have dropped it: a device that
accepted it would be accepting a frame the network will never deliver.

## A frame is a port, not a buffer — for the fourth time

`Device` gives a device its registers and no access to guest memory, so a frame moves
through a data port one access at a time. That is B5's block device and B10's audio
device reaching the same conclusion independently, and the reason is always the same: a
device that could name a guest address would be a device that could be pointed at
anything.

## Two real bugs, both in the control register

Both were found by the test that drives the device **through MMIO** rather than through
its Rust methods, which is the test that has to exist for a device a guest will use.

1. **Begin-then-clear.** The control write called `begin_transmit` and then cleared
   `TX_ACTIVE` in the same arm, so the transmit was unset before the guest could write a
   byte into it. A guest could start a frame and not be able to put anything in it.
2. **Commit-abandons.** The first fix cleared `TX_ACTIVE` whenever `TX_COMMIT` was
   written, so committing a frame abandoned it. The arm now has **three written-out
   cases** — begin, commit, abandon — because two of them were got wrong first and the
   bit arithmetic was not the problem.

`NET_CONTROL_TX_COMMIT` exists because of the first version: `commit_transmit` is a Rust
call, so a guest driving the device through registers could assemble a frame and had no
way to say "that is all of it". The guest sets the bit; the **host** pumps. Nothing
transmits from inside a register write.

## Loss is counted, never silent

A network drops packets; that is what a network does. A receive queue of sixteen frames
is about 120 µs of a 1 Gb link — longer than a guest scheduling quantum — and when it
fills, the frames the host delivered that did not fit are counted in `dropped()`.

The unacceptable thing is not the loss; it is a host that fills a queue and discards
without saying so. `a_full_receive_queue_drops_and_counts` asserts the **exact** count
rather than `> 0`, because a counter that over-reports is as useless as one that
under-reports.

## Invariants

No new architecture test: B11's properties are all *behavioural* — a type that cannot be
written wrong is better than a source check, and the boundary here is a method signature
rather than an absence. `the_display_device_holds_no_backend` and its input sibling
remain the shape checks; the network's equivalent is that `commit_transmit` requires a
backend, which the compiler enforces.

## Tests

`crates/lazalith-devices/tests/net.rs`, 18 tests: the three transmit outcomes attacked
separately, an empty transmit refused, a full staging buffer refused rather than
truncated, a bounded pump, the two-way path, loss counting, the interrupt edge, register
access through MMIO, a peek that does not consume, and the snapshot's exclusion of
packets.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1481 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **No host network backend.** `NullNetworkBackend` and `RecordingNetworkBackend` stand
   in. §32's five host backends are all unimplemented, and none of them is trivial: a
   user-mode backend needs a host socket, a bridged one needs a host interface.
2. **No MAC address.** `address()` returns `Vec<u8>` from the host and the two MAC
   registers read zero, because a `u64` MAC would have decided the link is 48-bit
   Ethernet — the decision §32 declines to make. A host that has an address has no way to
   publish it to a guest yet.
3. **Frames go through a port, one access at a time.** No DMA, for B10's reason: a device
   has no way to reach guest memory. A 1500-byte frame is 188 register accesses.
4. **No interrupts on transmit.** Only a waiting receive frame raises one. A guest that
   wants to know its frame left polls `NET_STATUS_TX_REFUSED`.
5. **No link-state change notification.** `commit_transmit` reads the link state; a
   backend whose link goes down mid-pump is not watched between pumps.
6. **No PIO**, unchanged since B4. Blocks B9's PS/2, B12's bus and B27's AT machine.
7. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B12, expansion bus / device discovery** (§33). The stage several things above are now
waiting on: PIO, USB (§30), and B27's compatibility machine all need a bus, and §33 says a
native Lazalith expansion bus may come before PCI.

---

# B12 — expansion bus / device discovery

`binstruction.md` §33, and §25's "Architectural Core → bus". Preceded by `54fcb32` (B11).
New module: `crates/lazalith-memory/src/expansion.rs`.

## What was already there, confirmed

- `Bus`, which maps device windows into an address space and routes by `DeviceId`.
- B4's `MachineProfile`, which *describes* a machine's devices.
- B10's `Device::take_interrupt`, which a bus needs to route.

## §33's six items, answered four ways and recorded two

| §33 item | Here |
| --- | --- |
| device identifiers | `DeviceDescriptor`: id, class, name, window, interrupt, dma |
| configuration | `ExpansionBus::with_capacity`, checked at attach time |
| MMIO | already the only register mechanism; the bus is where a window is *assigned* |
| interrupt routing | `attach` refuses two devices on one line |
| bus topology | `BusTopology`, a bus → device tree in attachment order |
| DMA | **not built** — recorded |

## The PIO question, answered

B4 recorded this as open and **four stages have now deferred it**: B9 (PS/2), B11, B12,
and it blocks B27. LZA has no `in`/`out` instruction; eleven Linux driver files use them.

§33 says "a native Lazalith expansion bus may come before PCI", which argues for giving
the platform a bus of its own. But a bus with only MMIO cannot host a PS/2 controller, an
IDE channel or a VGA sequencer, because every one of those is *defined by its port
numbers*.

**The answer: port decode is a compatibility-machine concern, emulated host-side, and LZA
gets no port address space.** A PS/2 controller here is a device whose *host backend*
answers for ports 0x60 and 0x64, and a guest reaches it through an ordinary MMIO register
window the backend translates.

That is a real architectural position, not another deferral, and it has a consequence
worth naming: **a guest that speaks `in`/`out` cannot be ported by rewriting its driver
to use MMIO**, because the port numbers *are* the interface. The Linux port therefore
needs either an ISA with `in`/`out` or a compatibility layer that traps them. Both are
B27's question, and until it is answered this is the documented default rather than the
only possible answer.

## Discovery is a description, not a scan

A bus that *scans* for devices must know what a device looks like before it has one.
That is PCI's configuration space and it is a large, load-bearing decision.

`DeviceDescriptor` is the alternative: a device **says** what it is, and the bus places
it. No scan, no vendor list, no enumeration order — because the machine is *described*,
which B4 established, and a profile listing four devices has four devices.

**The cost, stated plainly:** a guest cannot find a device the host did not describe. For
a virtual machine that is a feature. For a machine meant to run code that probes for
hardware, it is a limitation — and it is exactly what B27's compatibility machine will
have to give up.

## Every refusal happens before anything is written

`attach` checks capacity, duplicate id, empty window, address overflow, window overlap
and interrupt conflict **before** it pushes. A bus that attached a device and *then*
found the overlap would leave a machine where one address has two possible destinations,
and nothing would ever report it. Every test attaches a good device, then a bad one, and
checks the bus length is unchanged.

**Interrupt lines are exclusive.** A controller that delivers one line to two devices
delivers it to neither, so a conflict is a refusal rather than a resolution order.

**Attachment order is meaningful and is recorded.** Windows are checked when the *second*
device is attached, so the order records which one "had" the address — which is what a
caller diagnosing an overlap wants to know.

## `dma: bool` that nothing acts on

`DeviceDescriptor::dma` is recorded and **not acted on**. §33 lists DMA and it is not
built, for B10's reason: a device has no way to reach guest memory, and building it means
a DMA engine on the machine that services device *requests* rather than handing every
device a memory handle.

The flag exists so a profile can say "this device will do DMA" *before* the engine can,
and a machine that has one finds out at run time rather than at boot. **A flag nothing
reads would be worse than no flag** — it would be a claim the platform cannot keep — so
`BusTopology::dma_capable()` exists to let a caller see what was claimed.

## Tests

`crates/lazalith-memory/tests/expansion.rs`, 17 tests. The ones that matter:

- `two_devices_whose_windows_overlap_are_refused` and
  `two_devices_on_one_interrupt_line_are_refused` — both attach a good device first and
  then check the bus is unchanged, which is the "all checks before any write" property.
- `a_window_that_would_overflow_the_address_space_has_no_end` — an overflow in a window
  end must be a refusal, not an end address that wrapped to something small and legal.
- `a_bus_with_no_capacity_is_a_legitimate_machine` — refusing every attach is a way to
  say "this machine has no expansion hardware", not an error.
- `an_erased_bus_converts_to_a_manager_and_keeps_its_descriptions` — the descriptors
  survive the conversion, because a machine that has the devices and not what they are
  cannot be shown to a person or snapshotted.

**The monomorphic test uses `TimerDevice`, not `NoDevice`.** `NoDevice` is an
*uninhabited* enum, so a bus of them cannot be built and the test could not exist. Phase-I
uses `NoDevice` as a type, never as a value, and the test says so.

## Validation

- `cargo fmt --all --check` ✅
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` ✅
- `cargo test --workspace --all-features`: **1498 passed, 0 failed**
- `nix flake check`, `nix build`, `actionlint` ✅

## Limitations at the end of this stage

1. **The bus is not yet in the machine.** `ExpansionBus` is a tested, standalone
   structure; `LazalithMachine` still uses its own `Bus` + `DeviceManager` directly. The
   join — a profile building an `ExpansionBus` and a machine adopting it — is mechanical
   and is not done. `into_manager` exists precisely so that join is one call.
2. **No DMA**, and the flag is inert. See above.
3. **No port address space**, by decision. See the PIO section.
4. **Class numbers are a registry, not a table.** `ClassRegistry` hands out `u8`s so
   adding a device is not a change to a table elsewhere, but nothing consults the numbers
   yet — a guest cannot enumerate by class because there is no guest-visible
   enumeration.
5. **No hot-plug.** A bus is built and attached to; removing a device would change what
   addresses mean under a running guest, and §33 does not ask for it.
6. **aarch64-linux unchecked**, no cold `nix flake check` timing.

## Next stage

**B13, firmware / boot profiles** (§34). Small, and it uses what B6 built: a profile that
records *where* firmware lives and what the boot chain is, without implementing any
firmware — §34 says "Do not implement them during this architecture pass."
