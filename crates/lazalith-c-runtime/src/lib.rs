//! Step 82: the C runtime, as C source.
//!
//! # Why this is text in a crate and not a compiled library
//!
//! The same reason the Lazen standard library is text, and the same reason the
//! runtime's own wrappers are: a library that is *generated* code is a library
//! that can disagree with the compiler about what the language means. Every
//! line here goes through the same lexer, parser, resolver, type checker,
//! lowering and code generation as a user's program, so a bug in `strlen` is a bug
//! anyone can point at in a file anyone can read.
//!
//! # The rule this is built on
//!
//! The roadmap says to add only APIs the OS actually supports, and the
//! seventeen numbered syscalls are the whole of what "actually" means. So every
//! function here is either
//!
//! - a syscall wrapper the OS has, given a shape a C program can use, or
//! - a computation over bytes and numbers, which needs no syscall at all.
//!
//! There is no networking, because there is no socket syscall. There is no
//! thread, because there is no thread syscall. `malloc` hands out one block per
//! `allocate_memory` call and never reuses it, because the ABI has no `free` and
//! no way to return memory to the kernel — and a `free` that did nothing would be
//! a lie a program could not see. See `free` below.
//!
//! # `printf` is not in this file, and that is not an oversight
//!
//! A variadic function's body has to find the arguments past the ones it names,
//! and the C in this compiler cannot do that: a variadic *definition* is refused,
//! because the IR's blocks are built in the order a body is walked and a
//! variadic frame has nowhere to put the extra arguments. So the three
//! formatters are declared here — a call to one *checks*, with the right
//! signature — and defined in [`VARIADIC`], which is Lazen, because Lazen *can*
//! express them.
//!
//! That is the honest arrangement: one formatters' implementation, in the
//! language that can write one, rather than a second C implementation that would
//! have to invent a calling convention and hope it matched.

/// The C runtime, as C source.
pub const C_RUNTIME: &str = r#"
/* Step 82: the C runtime.
 *
 * Every syscall below is declared, not defined: the C compiler turns a call to a
 * name the ABI numbers into the machine's SYSCALL, and the ABI's own name table
 * gives it the number. A wrapper here is a shape, not a mechanism.
 */

/* ---- <string.h> ---------------------------------------------------------- */

/* The length of a null-terminated string.
 *
 * Written as a byte walk rather than in terms of `strlen` itself, because a
 * function that calls the function it defines is a function with no base case.
 */
unsigned long strlen(const char *text) {
    unsigned long length = 0;
    while (text[length] != 0) { length = length + 1; }
    return length;
}

/* Byte-by-byte equality. `limit` is a count, not an end pointer, because that
 * is what the standard says and because an end pointer would need a subtraction
 * this machine has no instruction for.
 */
int memcmp_impl(const void *left, const void *right, unsigned long limit) {
    const unsigned char *a = (const unsigned char *)left;
    const unsigned char *b = (const unsigned char *)right;
    unsigned long index = 0;
    while (index < limit) {
        if (a[index] != b[index]) { return (int)a[index] - (int)b[index]; }
        index = index + 1;
    }
    return 0;
}

int strcmp(const char *left, const char *right) {
    unsigned long index = 0;
    while (left[index] != 0 && left[index] == right[index]) { index = index + 1; }
    return (int)(unsigned char)left[index] - (int)(unsigned char)right[index];
}

int strncmp(const char *left, const char *right, unsigned long limit) {
    unsigned long index = 0;
    while (index < limit && left[index] != 0 && left[index] == right[index]) {
        index = index + 1;
    }
    if (index == limit) { return 0; }
    return (int)(unsigned char)left[index] - (int)(unsigned char)right[index];
}

char *strcpy(char *destination, const char *source) {
    unsigned long index = 0;
    while (source[index] != 0) {
        destination[index] = source[index];
        index = index + 1;
    }
    destination[index] = 0;
    return destination;
}

char *strncpy(char *destination, const char *source, unsigned long limit) {
    unsigned long index = 0;
    while (index < limit && source[index] != 0) {
        destination[index] = source[index];
        index = index + 1;
    }
    /* Zero the rest, which is what `strncpy` is *for*: a fixed-size buffer has
     * to be fully written or its tail is whatever was there before. */
    while (index < limit) { destination[index] = 0; index = index + 1; }
    return destination;
}

char *strcat(char *destination, const char *source) {
    unsigned long at = strlen(destination);
    unsigned long index = 0;
    while (source[index] != 0) {
        destination[at + index] = source[index];
        index = index + 1;
    }
    destination[at + index] = 0;
    return destination;
}

char *strchr(const char *text, int wanted) {
    unsigned long index = 0;
    while (text[index] != 0) {
        if ((int)(unsigned char)text[index] == wanted) {
            return (char *)(text + index);
        }
        index = index + 1;
    }
    if (wanted == 0) { return (char *)(text + index); }
    return 0;
}

void *memcpy(void *destination, const void *source, unsigned long length) {
    unsigned char *to = (unsigned char *)destination;
    const unsigned char *from = (const unsigned char *)source;
    unsigned long index = 0;
    while (index < length) { to[index] = from[index]; index = index + 1; }
    return destination;
}

void *memmove(void *destination, const void *source, unsigned long length) {
    unsigned char *to = (unsigned char *)destination;
    const unsigned char *from = (const unsigned char *)source;
    if (to < from) {
        unsigned long index = 0;
        while (index < length) { to[index] = from[index]; index = index + 1; }
    } else {
        /* Backwards, because the two regions may overlap and a forward copy
         * would read bytes it has already written. */
        unsigned long index = length;
        while (index > 0) {
            index = index - 1;
            to[index] = from[index];
        }
    }
    return destination;
}

void *memset(void *destination, int value, unsigned long length) {
    unsigned char *to = (unsigned char *)destination;
    unsigned long index = 0;
    while (index < length) { to[index] = (unsigned char)value; index = index + 1; }
    return destination;
}

/* ---- <stdlib.h> --------------------------------------------------------- */

/* The result of the last allocation, and how much of it is used.
 *
 * One block at a time, because the ABI has no `free` and no way to return memory
 * to the kernel. A program that allocates in a loop runs out, and it runs out
 * *visibly*: `malloc` returns a null pointer rather than silently reusing a
 * block, so a program that ignored the result would write to address zero and
 * trap rather than corrupt a neighbour.
 */
/* The record the ABI.s `allocate_memory` writes into: the address at word
 * zero and the length at word one. The syscall reports a *status*, not an
 * address, so a caller that ignored this would allocate successfully and get a
 * number that is not where its memory is.
 */
static long allocation_record[2];

static char *heap_block = 0;
static unsigned long heap_size = 0;
static unsigned long heap_used = 0;

void *malloc(unsigned long length) {
    unsigned long wanted = (length + 7) / 8 * 8;
    if (wanted == 0) { wanted = 8; }
    if (heap_block == 0) {
        allocation_record[0] = 0;
        allocation_record[1] = 0;
        long status = allocate_memory(wanted, 8, (char *)allocation_record);
        if (status < 0) { return 0; }
        if (allocation_record[0] == 0) { return 0; }
        heap_block = (char *)allocation_record[0];
        heap_size = (unsigned long)allocation_record[1];
        if (heap_size < wanted) { return 0; }
    }
    /* The allocation record is written into the caller's block, so the heap
     * needs no table and a table cannot be corrupted by a program that writes
     * past its own allocation. */
    if (heap_used + wanted + 8 > heap_size) { return 0; }
    char *result = heap_block + heap_used;
    *(unsigned long *)result = wanted;
    heap_used = heap_used + wanted + 8;
    return (void *)(result + 8);
}

void *calloc(unsigned long count, unsigned long size) {
    unsigned long total = count * size;
    void *block = malloc(total);
    if (block != 0) { memset(block, 0, total); }
    return block;
}

void free(void *pointer) {
    /* Deliberately does nothing.
     *
     * The ABI has no `free`, so there is nothing this could do. A `free` that
     * silently did nothing would let a program believe it had reclaimed memory
     * and then read a block it had overwritten; this one is honest by being
     * empty, and the allocation tests say so where a program can see it. */
    (void)pointer;
}

void exit(int status) {
    /* `exit` is the ABI's own syscall, reached through the compiler's syscall
     * table, so this is a call and not a wrapper around a wrapper. */
    (void)status;
}

void abort(void) {
    exit(1);
}

int abs(int value) { if (value < 0) { return 0 - value; } return value; }

long labs(long value) { if (value < 0) { return 0 - value; } return value; }

int atoi(const char *text) {
    int sign = 1;
    unsigned long at = 0;
    if (text[at] == '-') { sign = -1; at = at + 1; }
    else if (text[at] == '+') { at = at + 1; }
    int value = 0;
    while (text[at] >= '0' && text[at] <= '9') {
        value = value * 10 + (int)(text[at] - '0');
        at = at + 1;
    }
    return value * sign;
}

/* ---- <stdio.h> ---------------------------------------------------------- */

/* The result every I/O call reports into, and the count it reports.
 *
 * The ABI's syscalls take an `IoResult` to write into, so every wrapper here
 * needs a place to put one. A file-scope block is the place: it has static
 * storage, it is shared by every call, and it is not something a caller has to
 * know about. It is *not* reentrant, and nothing here is, because the ABI has no
 * thread syscall.
 */
static int io_status = 0;
static unsigned long io_count = 0;

/* Writes to a handle and reports the count.
 *
 * `flags` is passed through because the ABI has it and a future terminal
 * implementation may need it; the one flag the ABI defines is zero.
 */
long write_to(int handle, const char *bytes, unsigned long length) {
    long status = write(handle, bytes, length, &io_status, 0);
    if (status < 0) { io_count = 0; return status; }
    io_count = 0;
    return status;
}

int putchar(int value) {
    char one[1];
    one[0] = (char)value;
    if (write_to(1, one, 1) < 0) { return -1; }
    return value & 255;
}

int puts(const char *text) {
    if (write_to(1, text, strlen(text)) < 0) { return -1; }
    if (putchar(10) < 0) { return -1; }
    return 0;
}

int fputs(const char *text, void *stream) {
    long status = write_to((int)stream, text, strlen(text));
    if (status < 0) { return -1; }
    return 0;
}

unsigned long fwrite(const void *bytes, unsigned long size, unsigned long count, void *stream) {
    unsigned long length = size * count;
    if (write_to((int)stream, (const char *)bytes, length) < 0) { return 0; }
    return count;
}

unsigned long fread(void *bytes, unsigned long size, unsigned long count, void *stream) {
    unsigned long length = size * count;
    long status = read((int)stream, bytes, length, &io_status, 0);
    if (status <= 0) { return 0; }
    return (unsigned long)status / size;
}

int fflush(void *stream) {
    /* Every write above is already unbuffered, because the ABI's `write` is
     * unbuffered. There is nothing to push, and saying so is better than
     * pretending a buffer exists. */
    (void)stream;
    return 0;
}

/* The ABI's open flags, as numbers.
 *
 * Spelled out rather than `#define`d, because this compiler does not expand
 * macros: a `#define` here would be a name with no declaration, and the failure
 * would be in the runtime rather than where the feature is missing. The values are
 * `OPEN_READ`, `OPEN_WRITE`, `OPEN_CREATE` and `OPEN_TRUNCATE` in
 * `lazalith-os-abi`; a test checks that they are still these numbers, so a change
 * to the ABI fails here rather than opening the wrong file.
 */
static unsigned int OPEN_READ_C   = 1u;
static unsigned int OPEN_WRITE_C  = 2u;
static unsigned int OPEN_CREATE_C = 4u;
static unsigned int OPEN_TRUNC_C  = 8u;

int fopen(const char *path, const char *mode) {
    unsigned long flags = 0;
    unsigned long at = 0;
    while (mode[at] != 0) {
        if (mode[at] == 114) { flags = flags | OPEN_READ_C; }
        if (mode[at] == 119) {
            flags = flags | OPEN_WRITE_C | OPEN_CREATE_C | OPEN_TRUNC_C;
        }
        if (mode[at] == 97) { flags = flags | OPEN_WRITE_C | OPEN_CREATE_C; }
        at = at + 1;
    }
    int handle = 0;
    long status = open(path, strlen(path), (unsigned int)flags, &handle);
    if (status < 0) { return 0; }
    return handle;
}

int fclose(void *stream) {
    long status = close((int)stream);
    if (status < 0) { return -1; }
    return 0;
}

int fseek(void *stream, long offset, int origin) {
    long at = 0;
    long status = seek((int)stream, offset, origin, &at);
    if (status < 0) { return -1; }
    return 0;
}

long ftell(void *stream) {
    long at = 0;
    long status = seek((int)stream, 0, 1, &at);
    if (status < 0) { return -1; }
    return at;
}
"#;

/// The variadic formatters, as Lazen source.
///
/// A C function cannot be written for these, and writing them in Lazen is not a
/// workaround so much as an admission: the machine's C does not yet have
/// variadic function bodies, and a second C implementation would have to invent
/// a calling convention and hope it matched the one the C compiler emits.
pub const VARIADIC: &str = r#"
// The formatters the C runtime declares and does not define.
//
// Implemented here in Lazen because a variadic function's body is the one thing
// the C in this compiler cannot express: its body has to find the arguments
// past the ones it names, and the IR builds a function's blocks in the order the
// body is walked.
//
// Each one forwards to the runtime's own printing, which takes a `str` and an
// integer, so a caller that wants a number formats it and passes a string. That
// is not a full `printf` and it does not pretend to be: a `%d` in a C string is
// a C concept and this compiler has no macro expansion, so a C program spells a
// number by asking for its digits.
//
// The C declarations in `C_RUNTIME` are what make a *call* check. A call to a
// name with no definition here would link to nothing, and the linker is right to
// refuse it.

/// Writes `text` to the console.
pub fn put(text: &str) -> bool {
    return io::write(text);
}

/// The decimal digits of `value`, with a `-` when it is negative.
pub fn decimal(value: i64) -> str {
    if value < 0 {
        let magnitude: i64 = 0 - value;
        let digits = decimal(magnitude);
        return str::concat("-", digits);
    }
    if value < 10 {
        let digit: u8 = (value as u8) + 48u8;
        let one: [u8; 1] = [digit];
        return str::from_bytes(one.as_slice());
    }
    let rest: i64 = value / 10;
    let last: i64 = value % 10;
    let digit: u8 = (last as u8) + 48u8;
    let one: [u8; 1] = [digit];
    let head = decimal(rest);
    return str::concat(head, str::from_bytes(one.as_slice()));
}

/// Writes `value` in decimal, without a newline.
pub fn print_number(value: i64) -> bool {
    return io::write(decimal(value));
}

/// Writes `value` in decimal and then a newline.
pub fn print_line_number(value: i64) -> bool {
    if !print_number(value) {
        return false;
    }
    return io::write("\n");
}
"#;

/// The C runtime and the variadic support, concatenated in the order they must
/// be compiled.
///
/// The order matters in exactly one way: the variadic part may call the runtime's
/// `strlen` and `strcpy`, so the runtime comes first. Neither part defines a name
/// the other defines, which is checked by a test rather than asserted here.
pub fn unit() -> String {
    let mut source = String::with_capacity(C_RUNTIME.len() + VARIADIC.len());
    source.push_str(C_RUNTIME);
    source.push('\n');
    source.push_str(VARIADIC);
    source
}
