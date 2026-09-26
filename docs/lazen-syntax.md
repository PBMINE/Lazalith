# Lazen Syntax Prototype

This document is Step 52 of the roadmap: it fixes a candidate syntax by writing
real example programs *before* any parser exists. The examples are the
specification. Every program in this file is a test fixture in
`crates/lazalith-compiler/tests/documented_examples.rs`, so the syntax cannot
drift silently away from the documentation.

The parser was written in Step 61, from this document rather than from any
existing code.

## Notation

Whitespace is insignificant. Statements end with `;`. Blocks are `{ ... }`.
Types and keywords are lowercase. `//` starts a line comment; `/* */` is
deliberately **not** supported in v1, so that comment handling cannot diverge
between the lexer, the documentation, and the compiler.

A string literal contains raw bytes with exactly six escapes: `\n`, `\r`, `\t`,
`\0`, `\\`, and `\"`. There is no `\u`, no `\x`, and no octal or decimal escape,
because each of those needs a decoding rule the compiler would then have to
agree with the standard library about. A string literal may not span a line.

## 1. Hello world

```lazen
extern "syscall" fn write(fd: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    let message = "Hello, Lazalith\n";
    let bytes = message.as_bytes();
    write(1, bytes, message.len() as u64, bytes.as_ptr());
    0
}
```

`extern "syscall"` declares an OS ABI call. The declaration mirrors the ABI
argument order exactly; the compiler maps the name to the shared
`lazalith_os_abi::Syscall` value and refuses a declaration whose arity exceeds
the ABI's own argument count.

A call must pass exactly the declared arguments, no more and no fewer. There is
no default argument and no partial call: a call that omits an argument is a
`T0004` arity error, because filling in a value the program did not write would
be a guess.

## 2. Variables

```lazen
fn main() -> i32 {
    let answer = 42;
    let mut total: i64 = 0;
    let label = "sum";
    let flag = true;
    let ratio = 1.5;
    total = total + answer as i64;
    if flag && total > 0 {
        total = total + 1;
    }
    let unused = ratio;
    0
}
```

Types are inferred where unambiguous, and an explicit type is allowed where it
aids the reader. `mut` is required to assign after declaration; omitting it is a
semantic error, not a warning.

Lazen v1 has no floating-point type, so the `1.5` above is rejected with
`T0103 float literals are not part of Lazen v1`. Every other line in this
example is valid.

## 3. Functions

```lazen
fn square(value: i32) -> i32 {
    value * value
}

fn clamp(value: i32, low: i32, high: i32) -> i32 {
    if value < low {
        return low;
    }
    if value > high {
        return high;
    }
    value
}

pub fn main() -> i32 {
    let result = square(7);
    let bounded = clamp(result, 0, 40);
    bounded as i32
}
```

A function whose body ends in an expression evaluates that expression as its
result. A `return` exits immediately. `pub` is meaningful only inside a module
(Step 55).

## 4. Conditionals

```lazen
fn classify(value: i32) -> i32 {
    if value < 0 {
        -1
    } else if value == 0 {
        0
    } else {
        1
    }
}

fn main() -> i32 {
    let first = classify(-5);
    let second = classify(0);
    let third = classify(9);
    (first + second + third) as i32
}
```

`else if` chains without needing braces around the nested `if`. Comparison
operators are `==`, `!=`, `<`, `<=`, `>`, `>=`; logical operators are `&&` and
`||`.

## 5. Loops

```lazen
fn main() -> i32 {
    let mut total: i32 = 0;
    let mut index: i32 = 0;
    while index < 10 {
        total = total + index;
        index = index + 1;
    }
    for value in 0..10 {
        total = total + value;
    }
    let mut countdown: i32 = 3;
    loop {
        if countdown == 0 {
            break;
        }
        countdown = countdown - 1;
        if countdown == 1 {
            continue;
        }
    }
    total
}
```

`for x in a..b` is half-open: `b` is never taken as a value. `break` and
`continue` apply to the innermost enclosing loop.

## 6. Arrays

```lazen
fn main() -> i32 {
    let mut values = [1, 2, 3, 4];
    let mut index: usize = 0;
    let mut total: i32 = 0;
    while index < values.len() {
        total = total + values[index];
        index = index + 1;
    }
    values[0] = 10;
    let first = values[0];
    let slice = values.as_slice();
    total + first + slice.len() as i32
}
```

Indexing is bounds-checked; an out-of-range index raises a synchronous trap with
a documented code instead of undefined behaviour. `len` is the builtin for an
array, a slice, or a string, and `.as_slice()` borrows an array as `&[T]`.

## 7. Modules

```lazen
mod geometry {
    pub fn area(width: u32, height: u32) -> u32 {
        width * height
    }

    fn unused() -> i32 {
        0
    }
}

fn main() -> i32 {
    let size = 4;
    geometry::area(size, 5) as i32
}
```

`mod` declares a module, `use` imports a path into the current scope, and `pub`
controls visibility across module boundaries. Referring to a private item from
another module is a semantic error.

## 8. Pointers

```lazen
extern "syscall" fn write(handle: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn main() -> i32 {
    let message = "Hello, Lazalith\n";
    let bytes = message.as_bytes();
    let address = bytes.as_ptr() as u64;
    if address == 0 {
        return 1;
    }
    let mut scratch = [0u8; 16];
    scratch[0] = 65;
    write(1, bytes, message.len() as u64, scratch.as_mut_slice().as_ptr() as ptr<u8>);
    0
}
```

`ptr<T>` is an address for the OS ABI. There is no `unsafe` block: a
dereference is always through a typed slice or array, so the compiler can bounds
check it, and a raw pointer is only ever passed to the OS.

## 9. Errors

Lazen v1 has no `optional`, no enums, and no exceptions. A fallible operation
returns an integer status that the program inspects, and a helper returns `0` for
success. This keeps the v1 type set closed and small; richer error modelling is
deferred until the OS serves the calls that need it.

```lazen
extern "syscall" fn open(path: &[u8], path_length: u64, flags: u32, handle: ptr<u32>) -> i64;
extern "syscall" fn read(handle: u32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;
extern "syscall" fn close(handle: u32) -> i64;

fn read_first_byte(path: &str) -> i32 {
    let mut handle: u32 = 0;
    let status = open(path.as_bytes(), path.len() as u64, 1, &mut handle as ptr<u32>);
    if status != 0 {
        return -1;
    }
    let mut buffer = [0u8; 256];
    let mut result = [0u8; 16];
    let read_status = read(handle, buffer.as_slice(), 256, result.as_ptr());
    let _ = close(handle);
    if read_status != 0 {
        return -2;
    }
    buffer[0] as i32
}

fn main() -> i32 {
    read_first_byte("/hello.txt")
}
```

A program that cannot continue calls `exit`, which the standard library wraps.
There is no panic, and the compiler rejects a statement expression that is not a
call, so a mistake cannot be silently discarded.

## 10. File I/O through the standard library

```lazen
mod fs {
    // The SDK provides this; the raw ABI is shown for reference.
    pub fn read_file(path: &str, buffer: &mut [u8]) -> i64 {
        0
    }
}
```

`std::fs` wraps the same OS ABI calls and converts a status into a value a
program can test. Step 67 implements it; no application writes raw status
handling once the standard library exists.

## 11. Graphics

```lazen
extern "syscall" fn display_open(width: u32, height: u32, framebuffer: ptr<u64>) -> i64;
extern "syscall" fn display_present() -> i64;

mod ui {
    pub fn open(width: u32, height: u32) -> i64 {
        let mut framebuffer: u64 = 0;
        display_open(width, height, &mut framebuffer as ptr<u64>)
    }

    pub fn present() -> bool {
        display_present() == 0
    }
}
```

A graphical program obtains the framebuffer address from the OS and then writes
pixels through a checked slice. It never learns a physical address, a device
register layout, or anything about SDL3.

## 12. Input

```lazen
extern "syscall" fn input_poll(events: ptr<u32>, capacity: u32) -> i64;

fn drain(buffer: &mut [u32]) -> u32 {
    let count = input_poll(buffer.as_mut_slice().as_ptr() as ptr<u32>, 32);
    if count < 0 {
        return 0;
    }
    count as u32
}
```

Events arrive as fixed-layout records written by the OS into caller-provided
memory. The application never sees a host event structure.

## 13. Deliberate omissions in v1

The v1 language is deliberately small. These are **not** part of Lazen v1, and
the compiler reports each of them with a specific diagnostic rather than
accepting a construct it cannot lower:

- records (`struct`) and their literals;
- enums, `optional`, and `match`;
- closures, generics, traits, iterators, and operator overloading;
- `for x in collection` over arrays or slices; ranges only;
- string interpolation; formatting lives in the standard library;
- `unsafe` blocks;
- a preprocessor, macros, and compile-time evaluation;
- block comments, for the reason given at the top of this document.

Every one of these was considered. The reason for excluding all of them is the
same: each needs a subsystem the OS or the toolchain does not have yet, and a
language rule that cannot be checked exhaustively is worse than a missing one.
`docs/lazen-types.md` records the same decisions from the type system's side, and
`docs/lazen-memory-model.md` explains why a closed type set matters for a
platform whose heap is not yet served.

## 14. How these examples are validated

Every program in this document is a test fixture.
`crates/lazalith-compiler/tests/documented_examples.rs` compiles each of them
through the real frontend, and rejects each documented omission with the code
this document names. A change to the grammar requires changing this document in
the same commit, which keeps the specification and the implementation in
agreement.

Where this document and the compiler disagreed while the compiler was being
written, this document was corrected rather than the compiler, because a
specification that cannot compile is not a specification. The three corrections
were: section 1's call now passes all four arguments its declaration names,
section 7's `u32` parameter is given a `u32` binding, and the notation section
now specifies the six string escapes the examples rely on.
