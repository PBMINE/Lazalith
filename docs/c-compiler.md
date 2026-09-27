# The C compiler

`lazalith-c-compiler` is a C front end for the Lazalith machine. It compiles C
to the same Lazalith IR the Lazen compiler produces and hands it to the same
native backend, so a C program and a Lazen program are two front ends over one
machine rather than two machines.

```text
C source -> lexer -> parser -> AST -> resolver -> type checker -> CheckedCProgram
         -> IR (lazalith_ir::Module + frames) -> native backend -> .lzo
         -> linker -> .lzx -> LazOS
```

## The sizes

`docs/lz64.md` left C's `int` and `long` open, pending "a dedicated ABI design".
This is that decision.

| C type | Bytes | IR type | Notes |
|---|---|---|---|
| `void` | — | `Type::Void` | cannot be stored |
| `_Bool` | 1 | `Type::Bool` | promoted to `int` in arithmetic |
| `char`, `signed char`, `unsigned char` | 1 | `Int { 8, _ }` | `char` is **signed** on this target |
| `short` | 2 | `Int { 16, _ }` | |
| `int` | 4 | `Int { 32, _ }` | |
| `long`, `long long` | 8 | `Int { 64, _ }` | both are eight bytes |
| pointer | 8 | `Type::Pointer` | |
| `size_t` | 8 | `Int { 64, unsigned }` | it is `unsigned long` |

`long` and `long long` are both eight bytes, which is LP64 and matches the
machine: sixteen general registers, all 64 bits, no register pairs. This is the
standard C model (C11 5.1.1.2) in which `int` is 32 bits and `long` is 64.

**A program compiled here is not portable to an ILP32 host.** That is said here
rather than left for a reader to discover, because the two disagree about
`sizeof(int)` and about what a `long` fits in.

`char` is signed. C leaves plain `char`'s signedness to the implementation, and
this is a 64-bit machine with a sign-extending `LDS`, so `char` matches `signed
char`.

## What is supported

- Every expression: the unary and binary operators, `?:`, `,`, assignment and
  every compound assignment, `++` and `--` in both orders, casts, `sizeof` of
  both a type and an expression, and `&`, `*` and `[]`.
- Every statement: `if`/`else`, `while`, `do`/`while`, `for`, `switch` with
  `case` and `default`, `break`, `continue`, `return`, compound statements, and
  labels.
- Functions, with prototypes, recursion, and a call to a name the OS ABI numbers
  becoming the machine's `SYSCALL`.
- `struct`, `union` and `enum`, including `typedef` and anonymous members.
- Arrays, as their own storage. `[T; N]` becomes an IR record with one field per
  element, because that is the only aggregate the IR has and because an array's
  element offsets are the array's own rather than a pointer's.
- `const`, `volatile`, `restrict`, `static`, `extern` and `inline`, accepted with
  the qualifiers recorded and the effects ignored: the machine has no
  volatile-access ordering to promise, every local is already in a frame, and
  code generation has no inlining.
- `const char *s = "text";` and every other string use. A string literal is a
  data segment whose bytes include the terminating null, and its address is a
  symbol the linker relocates rather than a number the compiler made up.
- `_Static_assert`, checked at compile time.
- The C conversion rules: integer promotion, the usual arithmetic conversions,
  array and function decay, and assignment conversion. Every conversion is one
  store of the whole word followed by one load at the target's width, because
  that *is* C's conversion: the low bytes are what survives, and a
  sign-extending load is what makes a signed target sign-extend.

## What is refused, and why

Every refusal names the C construct **and** the machine limit behind it. A
refusal that only says "unsupported" tells the reader nothing they can act on.

| Refused | Why | What to write instead |
|---|---|---|
| `float`, `double` | the ISA has no floating-point instruction, and a software implementation is a different project | an integer |
| a `struct` or `union` wider than a word passed or returned **by value** | the ABI has one return register and no aggregate argument passing | pass a pointer |
| a variadic function **definition** | its body has no way to learn how many arguments it was given | a variadic *declaration* is fine, so `printf` is callable |
| `goto` | the IR's blocks are built in the order a body is walked, and a backwards jump needs a second pass | a loop |
| a call through a function pointer | `CALL` takes a displacement and `CALLR` takes one register; neither reaches a function whose address is only known at run time | a call by name, or a `switch` |
| a call needing more than six argument words | four arguments are in registers and two on the stack, and a wider type uses more than one word | fewer or narrower arguments |
| `#define` and macro expansion | not implemented; a directive is stepped over and a `#define`d name fails as undeclared | write the name out |
| a floating *literal*, even where the type would be an integer | a floating constant is a lexical fact, and refusing it in the lexer is where the message can say the machine has no floating point | an integer literal |

A `struct` is still fully usable *except* by value: it can be declared, sized
with `sizeof`, held in a local or a global, pointed to, and have its members read
and written through `.` and `->`.

## Namespaces

A lowered C function is `c.<name>` and a lowered syscall is `syscall.<name>`.
The IR has one flat symbol namespace, so without the prefixes a C function called
`write` and the ABI's `write` would be two definitions of one name and the linker
would refuse the object. Both prefixes exist for that reason alone.

A C function's object symbol is `fn.c.main`, and the entry sequence is assembled
for that name: `lazalith_runtime::startup_object_for` takes the entry symbol as a
parameter rather than hardcoding Lazen's, because a sequence that hardcoded one
language's name would need a second sequence for the other.

## Diagnostics

C's codes are the `C` family, split into ranges so a code says which stage
refused:

| range | stage |
|---|---|
| `C01xx` | lexer |
| `C02xx` | parser |
| `C03xx` | name resolution |
| `C04xx` | types and semantics |
| `C05xx` | lowering to IR |
| `C9xxx` | the per-stage fallback for a malformed code |

This is the same scheme the Lazen front end uses (`L` lexer, `P` parser, `N`
resolution, `T` types) and it is deliberately *not* the same numbering: the two
languages are different languages, and sharing a number for "expected an
expression" would make a code mean two things.

`lazalith_c_compiler::analyse` reports every failure rather than the first, and
`lazalith_c_compiler::compile` stops at the first. Both exist because both are
right for a caller: a build script wants the first and an editor wants all of
them.

## Reachable from C

A C program reaches the machine's facilities through the OS ABI's syscalls, by
name. A name the ABI has needs no declaration.

| Name | Signature |
|---|---|
| `exit` | `long exit(int status)` |
| `write` | `long write(int handle, char *buffer, u64 length, i32 *out_io_result, u32 flags)` |
| `read` | `long read(int handle, char *buffer, u64 length, i32 *out_io_result, u32 flags)` |
| `open` | `long open(char *path, u64 path_length, u32 flags, u32 mode)` |
| `close` | `long close(int handle)` |
| `seek` | `long seek(int handle, long offset, int origin, long *out_offset)` |
| `stat` | `long stat(char *path, u64 path_length, char *out_stat)` |
| `list_directory` | `long list_directory(char *path, u64 path_length, char *records, u64 capacity, i32 *out_io_result)` |
| `time` | `long time(long *out_cycles)` |
| `sleep` | `long sleep(u64 delay_cycles)` |
| `allocate_memory` | `long allocate_memory(u64 length, u64 alignment, char *out_allocation)` |
| `spawn_process` | `long spawn_process(char *path, u64 path_length, char **argv, u64 argc, int *out_handle)` |
| `wait_process` | `long wait_process(int handle, int *out_status)` |
| `clear_screen` | `long clear_screen(void)` |
| `input_poll` | `long input_poll(char *events, u64 capacity, i32 *out_io_result)` |
| `display_open` | `long display_open(u32 width, u32 height, char *framebuffer, char *out_record)` |
| `display_present` | `long display_present(char *framebuffer, u64 length, u64 stride, u32 flags)` |

These are `docs/os-abi.md`'s, argument for argument. They are not inferred from
the names: `write` takes an `IoResult` to report into, and a C program that
passed three arguments would leave the fourth register holding whatever it held
before — which reads as a write that wrote nothing.

A name the ABI has numbered but whose signature it has not stated is accepted
with any arguments and a `long` result. That is the one place this compiler
declines to check rather than refuses, and it is deliberate: refusing would make a
facility unavailable, and inventing a signature would be worse than declining.

## Building one

```rust
use lazalith_c_compiler::{compile, ir::lower};
use lazalith_codegen::{CodegenOptions, generate};
use lazalith_types::{ArchitectureConfig, SourceManager};

let mut sources = SourceManager::new();
let (_, checked) = compile(&mut sources, "hello.c", "int main(void) { return 42; }")?;
let lowered = lower(&checked)?;
let config = ArchitectureConfig::lz64();
let program = generate(
    &lowered.module,
    &lowered.frames,
    &lowered.entry,
    &CodegenOptions::lz64("hello.c"),
    "int main(void) { return 42; }",
)?;
let startup = lazalith_runtime::startup_object_for(config, "fn.c.main")?;
let linked = lazalith_toolchain::link_objects(
    &[program.object().clone(), startup],
    &lazalith_toolchain::LinkOptions { entry_symbol: Some(String::from("entry")) },
)?;
```

`crates/lazalith-c-compiler/tests/end_to_end.rs` is the worked example: it does
exactly that, boots a machine, and runs the program to its exit status.
