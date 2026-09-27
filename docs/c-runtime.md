# The C runtime

Step 82 gives a C program the machine's facilities, using only what step 81
established: the C compiler, the OS ABI's syscalls by name, and the same object
and linker the Lazen standard library is built from.

The runtime is one C translation unit, `lazalith_c_runtime::C_RUNTIME`, that a
program compiles *in front of* itself — exactly the arrangement the Lazen
standard library uses, and for the same reason: a program may define a name the
runtime also defines, and the program's is the one that wins. The C compiler
compiles the result as one unit, so a definition in the runtime is a definition
the program's own calls reach.

## What is here

| Header | Functions |
|---|---|
| `<string.h>` | `strlen`, `strcmp`, `strncmp`, `strcpy`, `strncpy`, `strcat`, `strchr`, `strrchr`, `strstr`, `memcmp`, `memcpy`, `memmove`, `memset` |
| `<stdlib.h>` | `malloc`, `calloc`, `free`, `abort`, `atoi`, `abs`, `labs` |
| `<stdio.h>` | `putchar`, `puts`, `fputs`, `fwrite`, `fread`, `fflush`, `fclose`, `fseek`, `ftell` |
| Lazalith's own | `print`, `print_line`, `print_decimal` |

A file handle is an `int` throughout, not a `FILE *`. The ABI's handles are
`u32`, and dressing a small integer up as a pointer is how a program ends up
printing one and comparing the other. See the `fopen` section for why this
matters more than usual here.

## `malloc` is a bump allocator, and says so

`malloc` asks the kernel for a 64 KiB chunk the first time it is called and hands
out blocks from it, each with an eight-byte length in front of it. When the chunk
fills, it asks for another one and abandons what was left of the old.

This is the ABI's doing rather than a choice. `free` and `realloc` are not
syscalls, so a program has no way to describe a region it no longer wants.
Calling this a general-purpose heap would promise a guarantee the interface cannot
make. What it costs is real:

- Memory a program "freed" is not recovered. `free` is a no-op, deliberately,
  and the no-op is the honest one: a `free` that silently did nothing would let a
  program believe it had reclaimed memory and then read a block it had
  overwritten.
- A program that allocates in a loop gets chunks until the process's data region
  is gone, and then gets a null pointer. A null pointer is the *visible* failure:
  a program that ignores it writes to address zero and traps rather than
  quietly corrupting a neighbour.
- The first allocation in a process costs a whole chunk whether it needs one byte
  or all of it. That is the price of not spending a kernel allocation per C
  allocation, and the kernel's pool is a bump allocator too.

`calloc` is `malloc` followed by `memset`, which is the standard definition and
is also the only one that cannot be wrong about zeroing.

## There is no `printf`

Printing is a call per piece:

```c
print("n=");
print_decimal(-1234);
print_line("");
```

`print_decimal` uses a frame buffer rather than `malloc`, so a program's first
diagnostic does not depend on the allocator running. Thirty-two bytes covers
every 64-bit value: twenty digits, a sign, and room to be wrong.

A variadic function's body has to find the arguments past the ones it names, and
that is the one thing this C cannot write — a variadic *definition* is refused,
because the IR's blocks are built in the order a body is walked and a variadic
frame has nowhere to put the extra arguments. Declaring `printf` so that a call
would *check* against a plausible signature was tried and is worse than not
having it: the program compiled, and the failure arrived from the linker as an
undefined symbol, naming neither the reason nor the file to look at. An unknown
name is a compile error that says the name.

## There is no `fopen`, and the reason is an ABI gap

`fread`, `fwrite`, `fputs`, `fseek`, `ftell`, `fclose` and `fflush` all work on a
handle the program already has. Handles 0, 1 and 2 are the console, so a C
program can read and write them today.

Opening a *named* file does not work from C, and the cause is worth stating
precisely rather than as "not implemented":

- `write` and `read` report into an `IoResult` the caller supplies.
- `seek` reports the new offset through a pointer the caller supplies.
- `stat` fills a record the caller supplies.
- `open` reports the new file's handle in the **outcome payload** — the second
  register of the return.

A calling convention hands a caller the first register. There is no calling
convention in this machine that surfaces the second, so a handle returned there
reaches hand-written assembly and nothing else. The native-shell fixture, which
*is* hand-written assembly, reads it correctly; `fs::open` in Lazen and a C
`fopen` would both read a slot the kernel never wrote, find a zero handle, and
report that a file which was open could not be.

The fix is one argument — the handle as an out-parameter, like its siblings. It
touches a documented ABI, a fixture that reads the payload, and the index
conventions the validation errors use, and it is a piece of work of its own
rather than a line to change at the end of a step. Until it is done, an unknown
`fopen` is a compile error that names `fopen`, which is the only thing a C
programmer can be told here that is true.

## `exit` is the ABI's, and the runtime does not shadow it

The compiler asks its library table *before* the OS ABI, so a runtime `exit` would
win — and a wrapper around the only exit there is cannot exit. `exit` is
therefore not a library name, `abort` calls the real one, and a test checks that
a C call to `exit` lowers to a syscall.

An exit status is a bit pattern. A C `int` is 32 bits in a 64-bit register, so
`return -42` from `main` arrives sign-extended, and the kernel reads argument
zero as a word and keeps 32 bits of it. That is also what a host does with
`exit(-1)`.

## A status is zero or it is not

Every syscall here returns a `u32` status, and zero is the only success. The C
runtime originally compared `status < 0`, which is a check that can never fire:
`InvalidHandle` is 9, and a signed comparison calls it a success. A `fwrite` to a
handle the process does not own reported a count of bytes nobody had written.
All of them are `!= 0` now, and there is a test that a refused call is visible.

## The Lazen half

`lazalith_c_runtime::VARIADIC` is Lazen source, not C, and it is *not* the C
runtime's `printf`. It is the same three jobs — `put`, `decimal`, and
`print_number`/`print_line_number` — for a Lazen caller, in the language whose
callers are Lazen.

They are a separate object with separate names, and a test checks that the two
halves share no definition. There is deliberately no combined form: an earlier
`unit()` concatenated the two into one string, which is two languages in one
translation unit and would have compiled exactly as far as the first `pub`.

## Tests

`crates/lazalith-c-runtime/tests/runtime.rs` runs real programs on the real
machine and checks their output and their exit status. A library whose functions
*compile* but return nothing is worse than no library, so nothing here is checked
by inspection. Sixteen tests, covering the string functions, the allocator, the
console, the number helpers, the printing, and the two absences above.
