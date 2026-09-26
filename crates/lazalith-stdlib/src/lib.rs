//! Step 67: the Lazen standard library, as Lazen source.
//!
//! # Why this is text in a crate and not a compiled library
//!
//! The same reason the runtime's own wrappers are text: a standard library that
//! is *generated* code is a standard library that can disagree with the compiler
//! about what the language means. This goes through the same frontend, lowering and
//! code generation as a user program, so a bug in `text::split` is a bug anyone
//! can point at in a file anyone can read.
//!
//! # The rule this step is built on
//!
//! The roadmap says to add only APIs the OS actually supports, and the fourteen
//! numbered syscalls are the whole of what "actually" means. So every function
//! here is either
//!
//! - a syscall wrapper the runtime already provides, given a shape a program can
//!   use, or
//! - a computation over bytes and numbers, which needs no syscall at all.
//!
//! There is no networking, because there is no socket syscall. There is no
//! thread, because there is no thread syscall. There is no heap-allocating
//! collection, because `allocate` returns an address a program cannot yet name as
//! a slice — so `collections` here means fixed-capacity containers over caller
//! memory, which is what Lazen v1 can honestly offer.
//!
//! # What the modules are
//!
//! ```text
//! core          integers, the widest-value helpers, and Result-shaped returns
//! io            reading, writing, and turning a status into something testable
//! text          str and [u8] questions: compare, search, split, number format
//! math          integer arithmetic, no floating point
//! collections   fixed-capacity buffers, vectors and byte builders
//! fs            paths, open, seek, stat, list
//! time          the clock and sleeping
//! process       exit, spawn, wait
//! ```
//!
//! # Conventions, and why
//!
//! - **A fallible call writes its value out and returns whether it worked.** Lazen
//!   v1 has no tuples, no `Result` and no enums, so `(value, ok)` is not a type a
//!   function can return. A function that can fail takes a `&mut [u8]` the value
//!   goes into and returns an `i64` status: zero for success, negative for the
//!   ABI's error. This is the ABI's own convention, not a library invention, so
//!   there is exactly one shape to learn and it is the one the hardware already
//!   uses.
//! - **A buffer is a caller-provided slice.** Lazen v1 has no heap, so nothing
//!   here allocates. A function that needs room takes `&mut [u8]` and refuses
//!   rather than writing past it.
//! - **Errors are negative `i64` statuses**, because that is what the ABI
//!   returns. `core::ok` turns one into a `bool` so a program never has to know
//!   the numbers.
//! - **No floating point**, because the ISA has none. A software float here would
//!   be a library the machine cannot check.

/// The standard library's modules, prepended to every program's text.
pub const STDLIB: &str = r#"
mod std {
    // Integers and the shapes a fallible call returns.
    //
    // Lazen v1 has no `Result`, no enums and no pattern matching, so this module
    // cannot be a sum type and does not pretend to be one. It offers what the
    // language can honestly express: arithmetic that cannot overflow silently,
    // and the `(value, ok)` convention every fallible call follows.
    pub mod core {
        /// The largest value of a `u32`.
        pub fn u32_max() -> u32 {
            0xffff_ffffu32
        }

        /// The largest value of a `u64`.
        pub fn u64_max() -> u64 {
            0xffff_ffff_ffff_ffffu64
        }

        /// The largest value of an `i32`.
        pub fn i32_max() -> i32 {
            0x7fff_ffffi32
        }

        /// The smallest value of an `i32`.
        pub fn i32_min() -> i32 {
            -0x7fff_ffffi32 - 1i32
        }

        /// `value` clamped to `low ..= high`.
        ///
        /// Written as a comparison chain because v1 has no `min` for integers that
        /// takes both sides by value, and a clamp that reads as one expression is
        /// worth three lines.
        pub fn clamp_u32(value: u32, low: u32, high: u32) -> u32 {
            if value < low {
                return low;
            }
            if value > high {
                return high;
            }
            return value;
        }

        /// `base` raised to `exponent`, written to `out`, reporting success.
        ///
        /// The multiplication is checked before it happens rather than after, so an
        /// overflow is a refusal this function chose and not a value the machine
        /// wrapped. The result goes into `out` because v1 has no tuple to return
        /// it in, and a caller that ignored the status would be reading a value
        /// this function never wrote.
        pub fn checked_pow(base: u64, exponent: u32, out: &mut [u8]) -> i64 {
            if out.len() as u64 < 8u64 {
                return -1i64;
            }
            let mut result: u64 = 1u64;
            let mut remaining: u32 = exponent;
            while remaining > 0u32 {
                if result > u64_max() / base {
                    return -2i64;
                }
                result = result * base;
                remaining = remaining - 1u32;
            }
            write_u64_to(out, result);
            return 0i64;
        }

        /// Writes `value` to the eight bytes at `out`.
        ///
        /// The encoding is little-endian, matching the ISA's, so these bytes are
        /// what a `u64` in a frame looks like and a program can read one back with
        /// `u64_from`.
        pub fn write_u64_to(out: &mut [u8], value: u64) {
            let mut at: u64 = 0u64;
            let mut rest: u64 = value;
            while at < 8u64 {
                out[at as usize] = (rest % 256u64) as u8;
                rest = rest / 256u64;
                at = at + 1u64;
            }
        }

        /// The `u64` in the eight bytes at `from`, little-endian.
        pub fn u64_from(from: &[u8]) -> u64 {
            let mut value: u64 = 0u64;
            let mut at: u64 = 0u64;
            let mut weight: u64 = 1u64;
            while at < 8u64 {
                if at >= from.len() as u64 {
                    break;
                }
                value = value + (from[at as usize] as u64) * weight;
                weight = weight * 256u64;
                at = at + 1u64;
            }
            return value;
        }

        /// Whether a syscall status means success.
        ///
        /// Zero is success and anything negative is an error code, which is the
        /// ABI's own convention. A program should use this rather than comparing
        /// against a number, so the two are in one place.
        pub fn succeeded(status: i64) -> bool {
            status == 0i64
        }

        /// Whether a syscall status is an error.
        pub fn failed(status: i64) -> bool {
            status < 0i64
        }

        /// The absolute value of an `i64`, written to `out`, reporting success.
        ///
        /// `i64::MIN` has no positive counterpart, and returning it unchanged would
        /// be a lie a caller could not detect, so it is refused instead. A caller
        /// that ignored the status would read a value this function never wrote.
        pub fn abs_i64(value: i64, out: &mut [u8]) -> i64 {
            if out.len() as u64 < 8u64 {
                return -1i64;
            }
            if value == i64_min() {
                return -2i64;
            }
            if value < 0i64 {
                write_u64_to(out, (0i64 - value) as u64);
            } else {
                write_u64_to(out, value as u64);
            }
            return 0i64;
        }

        /// The smallest value of an `i64`.
        pub fn i64_min() -> i64 {
            -0x7fff_ffff_ffff_ffffi64 - 1i64
        }
    }

    // Reading and writing, and the difference between a status and a result.
    //
    // The runtime's `rt::sys` wrappers return the ABI's status and leave the
    // ABI's records in caller memory. This module is the layer above: it gives a
    // program a `(value, ok)` answer and a `Console` it can print through,
    // because `rt::rt::sys::print(1, ...)` with a hand-built record is not something a
    // program should have to write.
    pub mod io {
        /// Reads and writes, in the shape a program uses.
        pub mod console {

            /// The console's input handle.
            pub fn input() -> u32 {
                rt::sys::console_input()
            }

            /// The console's output handle, as the ABI's `write` wants it.
            ///
            /// `write` takes an `i32` and the other calls take a `u32`, so this is
            /// the one conversion in the library and it is here rather than spread
            /// across the call sites, where a mistyped cast would be easy to miss.
            pub fn output() -> i32 {
                return 1i32;
            }

            /// Writes `text` to the console.
            pub fn write_line(text: &str) -> bool {
                print(text)
            }

            /// Writes `text` to the console with no newline.
            pub fn write(text: &str) -> bool {
                let mut record: [u8; 16] = [0u8; 16];
                let status: i64 = rt::sys::write_to(
                    output(),
                    text.as_bytes(),
                    record.as_mut_slice()
                );
                return std::core::succeeded(status);
            }

            /// Writes `text` and a newline.
            pub fn print(text: &str) -> bool {
                if !write(text) {
                    return false;
                }
                return write("\n");
            }

            /// Clears the console.
            pub fn clear() -> bool {
                return std::core::succeeded(rt::sys::clear());
            }
        }

        /// Reading from an open handle.
        pub mod input {

            /// Reads up to `buffer.len()` bytes into `buffer`.
            ///
            /// The count actually read is left in `count`, and the status says
            /// whether the read worked. A short read is normal and a caller has to
            /// be able to tell it from an error, which is why the two are separate:
            /// a return value alone could not say which happened.
            pub fn read(handle: u32, buffer: &mut [u8], count: &mut [u8]) -> i64 {
                if count.len() as u64 < 8u64 {
                    return -1i64;
                }
                let mut record: [u8; 16] = [0u8; 16];
                let status: i64 = rt::sys::read_from(handle, buffer, record.as_mut_slice());
                if std::core::failed(status) {
                    return status;
                }
                std::core::write_u64_to(count, rt::sys::read_u64(record.as_slice(), 0));
                return 0i64;
            }
        }
    }

    // Text: questions about bytes, and turning numbers into them.
    //
    // Everything here is a view over bytes. A `str` is a pointer and a length, so
    // "the text" is always the pair, and a function that only needs to look at it
    // copies nothing.
    pub mod text {

        /// The number of bytes in `text`.
        pub fn len(text: &str) -> u64 {
            return text.as_bytes().len() as u64;
        }

        /// Whether `text` has no bytes.
        pub fn is_empty(text: &str) -> bool {
            return len(text) == 0u64;
        }

        /// Whether `text` and `other` have the same bytes.
        pub fn eq(text: &str, other: &str) -> bool {
            return rt::mem::equals(text.as_bytes(), other.as_bytes());
        }

        /// The index of `needle` in `text`, or `u64::MAX` when absent.
        pub fn find(text: &str, needle: &str) -> u64 {
            let at: i64 = rt::text::find(text, needle);
            if at < 0i64 {
                return std::core::u64_max();
            }
            return at as u64;
        }

        /// The byte at `index`, or 0 when `text` is shorter.
        pub fn byte_at(text: &str, index: u64) -> u8 {
            let bytes = text.as_bytes();
            if index >= bytes.len() as u64 {
                return 0u8;
            }
            return bytes[index as usize];
        }

        /// Whether `text` begins with `prefix`.
        ///
        /// The comparison is over bytes, not over two `str`s: a sub-view of a
        /// string cannot be made without a `str` from a raw address, and there is
        /// no unchecked way to do that in v1. Bytes are the honest level for this
        /// question anyway — the answer is about a byte-for-byte match.
        pub fn starts_with(text: &str, prefix: &str) -> bool {
            let wanted: u64 = len(prefix);
            if len(text) < wanted {
                return false;
            }
            return rt::mem::equals(
                bytes_from(text.as_ptr() as u64, wanted),
                prefix.as_bytes()
            );
        }

        /// Whether `text` ends with `suffix`.
        pub fn ends_with(text: &str, suffix: &str) -> bool {
            let wanted: u64 = len(suffix);
            let total: u64 = len(text);
            if total < wanted {
                return false;
            }
            return rt::mem::equals(
                bytes_from(text.as_ptr() as u64 + total - wanted, wanted),
                suffix.as_bytes()
            );
        }

        /// A view of `length` bytes at `address`.
        ///
        /// The caller states the length, so the length is a promise rather than a
        /// check — but every index through the returned view is bounds checked
        /// against it, which is the guarantee the language offers everywhere.
        pub fn bytes_from(address: u64, length: u64) -> &[u8] {
            return rt::memory::slice(address, length);
        }

        /// Writes the decimal digits of `value` into `out`, returning the count.
        ///
        /// The digits are produced least-significant first, so `out` is filled back
        /// to front. A value of 0 writes one digit rather than none, because "0" is
        /// how a person writes zero and an empty field is how a bug looks.
        pub fn write_u64(value: u64, out: &mut [u8]) -> u64 {
            let room: u64 = out.len() as u64;
            if room == 0u64 {
                return 0u64;
            }
            let mut at: u64 = room;
            let mut rest: u64 = value;
            loop {
                at = at - 1u64;
                out[at as usize] = (rest % 10u64) as u8 + 48u8;
                rest = rest / 10u64;
                if rest == 0u64 {
                    break;
                }
            }
            return room - at;
        }

        /// The start index of what `write_u64` filled, for reversing the fill.
        pub fn u64_start(room: u64, written: u64) -> u64 {
            return room - written;
        }

        /// Writes `value` as decimal, leaving the count in `count`.
        ///
        /// A buffer too small to hold even one digit is refused rather than
        /// partially filled, so a caller cannot mistake a truncated number for a
        /// whole one.
        pub fn u64_to_bytes(value: u64, out: &mut [u8], count: &mut [u8]) -> i64 {
            if count.len() as u64 < 8u64 {
                return -1i64;
            }
            if out.len() as u64 == 0u64 {
                return -2i64;
            }
            let written: u64 = write_u64(value, out);
            std::core::write_u64_to(count, written);
            return 0i64;
        }
    }

    // Integer arithmetic. No floating point, because the ISA has none and
    // inventing a software float here would be a library the machine cannot check.
    pub mod math {
        /// The greatest common divisor of two unsigned values.
        pub fn gcd_u64(left: u64, right: u64) -> u64 {
            let mut a: u64 = left;
            let mut b: u64 = right;
            while b != 0u64 {
                let next: u64 = a % b;
                a = b;
                b = next;
            }
            return a;
        }

        /// The least common multiple, or 0 when either value is 0.
        pub fn lcm_u64(left: u64, right: u64) -> u64 {
            if left == 0u64 || right == 0u64 {
                return 0u64;
            }
            let product: u64 = left * right;
            return product / gcd_u64(left, right);
        }

        /// `value` raised to `exponent`, written to `out`, reporting success.
        ///
        /// 0 is the result for a value that does not fit, which is why `out` and a
        /// status are both needed: 0 is also a legitimate power, and a caller that
        /// only saw the value could not tell the two apart.
        pub fn pow_u64(value: u64, exponent: u32, out: &mut [u8]) -> i64 {
            return std::core::checked_pow(value, exponent, out);
        }

        /// The integer square root: the largest `n` with `n * n <= value`.
        ///
        /// Newton's method, and the loop stops when the estimate stops *changing*
        /// rather than when it stops decreasing. That distinction is the whole
        /// algorithm: the sequence decreases past the answer and then lands on it,
        /// so a loop that ends at the first non-decrease returns one step too many.
        /// For 144 the estimates run 144, 72, 36, 18, 12, 12 — and it is the second
        /// 12 that is the answer.
        pub fn sqrt_u64(value: u64) -> u64 {
            if value < 2u64 {
                return value;
            }
            let mut guess: u64 = value;
            let mut next: u64 = (guess + 1u64) / 2u64;
            while next < guess {
                guess = next;
                next = (guess + value / guess) / 2u64;
            }
            return guess;
        }

        /// The remainder of a division that rounds towards negative infinity.
        ///
        /// The sign test is written as separate `if`s because v1's comparison
        /// operators are defined on integers only — there is no `!=` for `bool` —
        /// and because a conditional used as a *statement* may not end in a value
        /// at all. Both constraints push the same way here, and the result reads
        /// the same as the one-line form would.
        pub fn floor_mod_i64(value: i64, modulus: i64) -> i64 {
            let mut rest: i64 = value % modulus;
            if rest == 0i64 {
                return 0i64;
            }
            if rest < 0i64 {
                if modulus > 0i64 {
                    rest = rest + modulus;
                }
            }
            if rest > 0i64 {
                if modulus < 0i64 {
                    rest = rest + modulus;
                }
            }
            return rest;
        }
    }

    // Fixed-capacity containers over caller memory.
    //
    // Lazen v1 has no heap, no `struct` and no `impl`, so a "container" here is
    // the pair the language can actually name: a `&mut [u8]` the program owns and a
    // `u64` saying how much of it is live. Every function takes that pair. This is
    // not a stand-in for a heap container — it is the shape this language has, and
    // a `Vec` that quietly allocated would be a lie about what a program is linked
    // against.
    //
    // There is no `new` returning a view, because v1 has no tuple to return one
    // in. A caller makes the view itself — `address.slice_from_raw_mut(capacity)`
    // — and passes it with its length, which is also what makes the storage
    // explicit at every use.
    pub mod collections {

        /// How many more bytes fit in a buffer of `length` live in `capacity`.
        pub fn remaining(capacity: u64, length: u64) -> u64 {
            if length >= capacity {
                return 0u64;
            }
            return capacity - length;
        }

        /// Appends one byte at `length`, returning the new length or 0 when full.
        ///
        /// Returning 0 for "full" is safe here only because a caller with a
        /// zero-length buffer cannot have added anything anyway, so 0 always means
        /// "nothing was added" rather than "added nothing to an empty buffer".
        pub fn push(storage: &mut [u8], length: u64, byte: u8) -> u64 {
            if remaining(storage.len() as u64, length) == 0u64 {
                return 0u64;
            }
            storage[length as usize] = byte;
            return length + 1u64;
        }

        /// Appends `bytes`, returning the new length.
        pub fn extend(storage: &mut [u8], length: u64, bytes: &[u8]) -> u64 {
            let mut at: u64 = 0u64;
            while at < bytes.len() as u64 {
                if push(storage, length + at, bytes[at as usize]) == 0u64 {
                    break;
                }
                at = at + 1u64;
            }
            return length + at;
        }

        /// The live bytes of a buffer, as a slice.
        pub fn live(storage: &[u8], length: u64) -> &[u8] {
            return rt::memory::slice(storage.as_ptr() as u64, length);
        }

        /// A stack of `u64` values, over storage the caller provides.
        ///
        /// The storage is a slice the caller made — `address.slice_from_raw_mut(n)`
        /// — so the stack's capacity is visible in the call rather than hidden in a
        /// constructor, and the count is a separate value the caller keeps.
        pub mod stack {

            /// How many values fit in `capacity` bytes.
            ///
            /// The capacity is passed as a count rather than taken from a slice,
            /// because v1 has no coercion from `&mut [u8]` to `&[u8]`: a mutable
            /// view is not a readable one as far as the type system is concerned,
            /// and pretending otherwise would let a program read through a view it
            /// only lent out.
            pub fn capacity_in(capacity: u64) -> u64 {
                return capacity / 8u64;
            }

            /// Pushes a value, returning the new count or 0 when full.
            pub fn push(storage: &mut [u8], count: u64, value: u64) -> u64 {
                if count >= capacity_in(storage.len() as u64) {
                    return 0u64;
                }
                let slot: &mut [u8] = rt::memory::slice_mut(storage.as_ptr() as u64 + count * 8u64, 8u64);
                std::core::write_u64_to(slot, value);
                return count + 1u64;
            }

            /// Pops a value into `out`, returning the new count.
            ///
            /// The value goes to `out` rather than being returned, because v1 has
            /// no tuple to return it with the count in. A count of 0 in means the
            /// stack was empty, and `out` is left alone: there is no value to
            /// report, and writing one would let a caller read a value this
            /// function never popped.
            pub fn pop(storage: &[u8], count: u64, out: &mut [u8]) -> u64 {
                if out.len() as u64 < 8u64 {
                    return 0u64;
                }
                if count == 0u64 {
                    return 0u64;
                }
                let slot: &[u8] = rt::memory::slice(storage.as_ptr() as u64 + (count - 1u64) * 8u64, 8u64);
                std::core::write_u64_to(out, std::core::u64_from(slot));
                return count - 1u64;
            }
        }
    }

    // The filesystem: what the `open`, `seek`, `stat` and `list_directory`
    // syscalls can honestly do.
    //
    // A `File` is a handle and nothing else. There is no `File` value that owns
    // anything, because a program cannot allocate one and the ABI hands back a
    // `u32`.
    pub mod fs {

        /// The flags `open` understands, as the ABI numbers them.
        pub mod flags {
            /// Open for reading.
            pub fn read() -> u32 {
                return 1u32;
            }
            /// Open for writing.
            pub fn write() -> u32 {
                return 2u32;
            }
            /// Create the file if it does not exist.
            pub fn create() -> u32 {
                return 4u32;
            }
            /// Truncate an existing file to nothing.
            pub fn truncate() -> u32 {
                return 8u32;
            }
        }

        /// Where a seek counts from.
        pub mod origin {
            /// From the start of the file.
            pub fn start() -> i32 {
                return 0i32;
            }
            /// From the current position.
            pub fn current() -> i32 {
                return 1i32;
            }
            /// From the end of the file.
            pub fn end() -> i32 {
                return 2i32;
            }
        }

        /// Opens `path`, leaving the handle in `handle_out`.
        ///
        /// The handle goes to a caller-provided slot because v1 has no tuple to
        /// return it in, and a handle of 0 is a legitimate value: returning 0 for
        /// "failed" would be indistinguishable from opening the first file.
        pub fn open(path: &str, flags: u32, handle_out: &mut [u8]) -> i64 {
            if handle_out.len() as u64 < 4u64 {
                return -1i64;
            }
            let mut slot: [u32; 1] = [0u32; 1];
            let status: i64 = rt::sys::open_path(
                path,
                flags,
                slot.as_mut_slice().as_ptr() as u64
            );
            if std::core::failed(status) {
                return status;
            }
            std::fs::write_u32_to(handle_out, slot[0]);
            return 0i64;
        }

        /// Writes `value` to the four bytes at `out`, little-endian.
        ///
        /// This is the `u32` counterpart to `core::write_u64_to`, and it is what a
        /// handle or an exit status round-trips through.
        pub fn write_u32_to(out: &mut [u8], value: u32) {
            let mut at: u64 = 0u64;
            let mut rest: u32 = value;
            while at < 4u64 {
                out[at as usize] = (rest % 256u32) as u8;
                rest = rest / 256u32;
                at = at + 1u64;
            }
        }

        /// The `u32` in the four bytes at `out`, little-endian.
        pub fn handle_from(handle_out: &[u8]) -> u32 {
            let mut value: u32 = 0u32;
            let mut at: u64 = 0u64;
            let mut weight: u32 = 1u32;
            while at < 4u64 && at < handle_out.len() as u64 {
                value = value + (handle_out[at as usize] as u32) * weight;
                weight = weight * 256u32;
                at = at + 1u64;
            }
            return value;
        }

        /// The `u64` in the eight bytes at `out`, little-endian.
        pub fn u64_from_out(out: &[u8]) -> u64 {
            return std::core::u64_from(out);
        }

        /// Closes a handle.
        pub fn close(handle: u32) -> bool {
            return std::core::succeeded(rt::sys::close_handle(handle));
        }

        /// Seeks, leaving the new absolute offset in `offset_out`.
        pub fn seek(handle: u32, offset: i64, origin: i32, offset_out: &mut [u8]) -> i64 {
            if offset_out.len() as u64 < 8u64 {
                return -1i64;
            }
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::seek_to(handle, offset, origin, record.as_mut_slice());
            if std::core::failed(status) {
                return status;
            }
            std::core::write_u64_to(offset_out, rt::sys::read_u64(record.as_slice(), 0));
            return 0i64;
        }

        /// Reads up to `buffer.len()` bytes from `handle`, leaving the count in
        /// `count`.
        pub fn read(handle: u32, buffer: &mut [u8], count: &mut [u8]) -> i64 {
            return std::io::input::read(handle, buffer, count);
        }

        /// Writes `bytes` to `handle`, leaving how many moved in `count`.
        ///
        /// The handle is an `i32` because the ABI's `write` is: a descriptor of
        /// 0 is a legitimate one, and an unsigned type could not express "none".
        /// This is the one place the two handle widths meet, and it is the ABI's
        /// choice rather than the library's.
        pub fn write(handle: i32, bytes: &[u8], count: &mut [u8]) -> i64 {
            if count.len() as u64 < 8u64 {
                return -1i64;
            }
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::write_to(handle, bytes, record.as_mut_slice());
            if std::core::failed(status) {
                return status;
            }
            std::core::write_u64_to(count, rt::sys::read_u64(record.as_slice(), 0));
            return 0i64;
        }

        /// The size of `path` in bytes, left in `size_out`.
        pub fn size(path: &str, size_out: &mut [u8]) -> i64 {
            if size_out.len() as u64 < 8u64 {
                return -1i64;
            }
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::status_of(path, record.as_mut_slice());
            if std::core::failed(status) {
                return status;
            }
            // The size is the third field of the ABI's sixteen-byte record: four
            // bytes of kind, four of permissions, then the eight-byte size.
            std::core::write_u64_to(size_out, rt::sys::read_u64(record.as_slice(), 8));
            return 0i64;
        }

        /// Whether `path` names a directory.
        pub fn is_directory(path: &str) -> bool {
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::status_of(path, record.as_mut_slice());
            if std::core::failed(status) {
                return false;
            }
            return rt::sys::read_u32(record.as_slice(), 0) == 2u32;
        }

        /// Whether `path` names a file.
        pub fn is_file(path: &str) -> bool {
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::status_of(path, record.as_mut_slice());
            if std::core::failed(status) {
                return false;
            }
            return rt::sys::read_u32(record.as_slice(), 0) == 1u32;
        }

        /// How many bytes one directory record occupies.
        pub fn directory_record_bytes() -> u64 {
            return 256u64;
        }

        /// How many records fit in `record`.
        pub fn directory_capacity(record: &mut [u8]) -> u64 {
            return record.len() as u64 / directory_record_bytes();
        }

        /// Lists `path` into `record`, returning how many records were written.
        pub fn list(path: &str, record: &mut [u8]) -> u64 {
            let capacity: u64 = directory_capacity(record);
            if capacity == 0u64 {
                return 0u64;
            }
            let status: i64 = rt::sys::list_path(
                path,
                record,
                capacity * directory_record_bytes()
            );
            if std::core::failed(status) {
                return 0u64;
            }
            return record.len() as u64 / directory_record_bytes();
        }

        /// The name in directory record `index`, as bytes.
        ///
        /// A record's name is a fixed-capacity, NUL-padded field, so the name ends
        /// at the first zero rather than at the record's end.
        ///
        /// The result is `&[u8]` and not `&str` because a name in a directory is
        /// bytes the filesystem supplied, and a filesystem is not obliged to supply
        /// text. A caller that wants a `str` asks for one with `as_str`, which
        /// checks — a directory entry with a byte sequence no encoding allows is a
        /// real thing to find, and reading it as text anyway is how a program ends
        /// up comparing two different byte strings as the same name.
        pub fn entry_name(record: &[u8], index: u64) -> &[u8] {
            let base: u64 = index * directory_record_bytes();
            let mut length: u64 = 0u64;
            while length < 64u64 && base + length < record.len() as u64 {
                if record[(base + length) as usize] == 0u8 {
                    break;
                }
                length = length + 1u64;
            }
            return rt::memory::slice(record.as_ptr() as u64 + base, length);
        }
    }

    // The clock and sleeping.
    pub mod time {

        /// The current time in nanoseconds, left in `now`.
        ///
        /// The ABI's `time` writes a single word, so the value is a count the OS
        /// defines rather than a calendar. A program that needs a date has to count
        /// for itself, which is honest about what the machine knows: it has a clock,
        /// not a calendar.
        pub fn now_nanoseconds(now: &mut [u8]) -> i64 {
            if now.len() as u64 < 8u64 {
                return -1i64;
            }
            let mut record: [u8; 16] = [0u8; 16];
            let status: i64 = rt::sys::clock(record.as_mut_slice());
            if std::core::failed(status) {
                return status;
            }
            std::core::write_u64_to(now, rt::sys::read_u64(record.as_slice(), 0));
            return 0i64;
        }

        /// Sleeps for `nanoseconds`.
        pub fn sleep(nanoseconds: u64) -> bool {
            return std::core::succeeded(rt::sys::sleep_for(nanoseconds));
        }

        /// Sleeps for `milliseconds`, converted to nanoseconds.
        pub fn sleep_millis(milliseconds: u64) -> bool {
            return sleep(milliseconds * 1_000_000u64);
        }
    }

    // Starting and waiting for programs.
    //
    // A handle is a `u32` the OS gave out, and waiting is a call that blocks the
    // calling thread until the other process leaves. There is no thread here and
    // no non-blocking wait, because the syscalls do not have one.
    pub mod process {

        /// Starts the program at `path`, leaving its handle in `handle_out`.
        pub fn spawn(path: &str, handle_out: &mut [u8]) -> i64 {
            if handle_out.len() as u64 < 4u64 {
                return -1i64;
            }
            let mut slot: [u32; 1] = [0u32; 1];
            let status: i64 = rt::sys::spawn(
                path,
                slot.as_mut_slice().as_ptr() as u64
            );
            if std::core::failed(status) {
                return status;
            }
            std::fs::write_u32_to(handle_out, slot[0]);
            return 0i64;
        }

        /// Waits for `handle`, leaving its exit status in `status_out`.
        ///
        /// The status is an `i32` in four bytes, which is the ABI's own width for
        /// it. A status of 0 is a program that asked to leave, and a caller that
        /// only wanted to know whether the wait worked should test the returned
        /// status rather than the one in `status_out`.
        pub fn wait(handle: u32, status_out: &mut [u8]) -> i64 {
            if status_out.len() as u64 < 4u64 {
                return -1i64;
            }
            let mut slot: [i32; 1] = [0i32; 1];
            let status: i64 = rt::sys::wait_for(
                handle,
                slot.as_mut_slice().as_ptr() as u64
            );
            if std::core::failed(status) {
                return status;
            }
            std::fs::write_u32_to(status_out, slot[0] as u32);
            return 0i64;
        }

        /// The `i32` exit status in the four bytes at `out`.
        pub fn status_from(status_out: &[u8]) -> i32 {
            return std::fs::handle_from(status_out) as i32;
        }

    }

    // The graphics SDK.
    //
    // This is the whole of `std::graphics`: pure Lazen, sitting on the two display
    // syscalls, with no host library anywhere in it. A program that uses only what
    // is here runs identically headless and in a window and cannot tell the
    // difference — which is the property `docs/lazen-graphics.md` asks for, and the
    // reason this is a module rather than a binding to something else.
    //
    // The drawing calls are total: a coordinate outside the canvas is *clipped*,
    // not an error. A program whose animation moves one pixel off screen draws its
    // visible part rather than faulting, which is what a real window system does
    // and what stops a cosmetic mistake from being a crash.
    pub mod graphics {
        // The two display calls. They are the only way this module reaches the
        // device, and a program has no other way to name a framebuffer.
        extern "syscall" fn display_open(
            width: u32,
            height: u32,
            framebuffer: ptr<u8>,
            record: ptr<u8>
        ) -> i64;
        extern "syscall" fn display_present(framebuffer: ptr<u8>, result: ptr<u8>) -> i64;

        /// How many bytes one pixel occupies: ARGB8888, four bytes.
        pub fn pixel_bytes() -> u64 {
            return 4u64;
        }

        /// How many bytes one `display_open` record occupies.
        pub fn record_bytes() -> u64 {
            return 24u64;
        }

        /// A colour, packed with A in the high byte and B in the low.
        ///
        /// This is the colour *as a number*, which is how a program names one:
        /// `0xAARRGGBB`, so a colour can be compared and masked as an integer. It
        /// is deliberately **not** the memory layout — `docs/lazen-graphics.md`
        /// fixes a pixel's bytes as A, R, G, B from offset 0, which is the
        /// opposite order from this number read little-endian. `write_pixel` and
        /// `read_pixel` are the only places that cross between the two, so the
        /// difference is written down once instead of being something every
        /// caller has to know.
        pub fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> u32 {
            return (alpha as u32) * 16777216u32
                + (red as u32) * 65536u32
                + (green as u32) * 256u32
                + (blue as u32);
        }

        /// The red channel of `color`.
        pub fn red_of(color: u32) -> u8 {
            return ((color / 65536u32) % 256u32) as u8;
        }

        /// The green channel of `color`.
        pub fn green_of(color: u32) -> u8 {
            return ((color / 256u32) % 256u32) as u8;
        }

        /// The blue channel of `color`.
        pub fn blue_of(color: u32) -> u8 {
            return (color % 256u32) as u8;
        }

        /// The alpha channel of `color`.
        pub fn alpha_of(color: u32) -> u8 {
            return ((color / 16777216u32) % 256u32) as u8;
        }

        /// Opaque black.
        pub fn black() -> u32 {
            return rgba(0u8, 0u8, 0u8, 255u8);
        }

        /// Opaque white.
        pub fn white() -> u32 {
            return rgba(255u8, 255u8, 255u8, 255u8);
        }

        /// Writes `color` into the four bytes at `out`.
        ///
        /// This is the one place the pixel layout is written out.
        /// `docs/lazen-graphics.md` fixes a pixel's bytes as A, R, G, B from
        /// offset 0, so the alpha byte is first in memory — the reverse of
        /// `0xAARRGGBB` read little-endian, which is why the mapping is here and
        /// not in `rgba`. Everything else in this module moves whole pixels
        /// around, so a mistake here would be a mistake everywhere at once.
        pub fn write_pixel(out: &mut [u8], color: u32) {
            out[0] = alpha_of(color);
            out[1] = red_of(color);
            out[2] = green_of(color);
            out[3] = blue_of(color);
        }

        /// The colour in the four bytes at `from`, read the way `write_pixel`
        /// wrote it.
        ///
        /// A short input reads the bytes it was given and leaves the rest
        /// transparent, rather than reading past the end or refusing: a caller
        /// that handed over fewer than four bytes gets a partially transparent
        /// colour, which is what those bytes actually say.
        pub fn read_pixel(from: &[u8]) -> u32 {
            let mut color: u32 = 0u32;
            let mut at: u64 = 0u64;
            while at < 4u64 && at < from.len() as u64 {
                let byte: u32 = (from[at as usize] as u32) * weight_of(at);
                color = color + byte;
                at = at + 1u64;
            }
            return color;
        }

        /// The weight a pixel byte carries in a colour number.
        ///
        /// Byte 0 is alpha, so it is the *high* byte of the number; byte 3 is
        /// blue, the low one. This is the inverse of the layout above, written
        /// once so `read_pixel` states its own rule.
        pub fn weight_of(at: u64) -> u32 {
            if at == 0u64 {
                return 16777216u32;
            }
            if at == 1u64 {
                return 65536u32;
            }
            if at == 2u64 {
                return 256u32;
            }
            return 1u32;
        }

        /// Opens a window of `width` by `height` over `framebuffer`.
        ///
        /// The window is over memory the *caller* owns. The device shares it, so
        /// nothing is copied and nothing is allocated here — a program that wants
        /// to draw somewhere draws in a framebuffer it already has.
        ///
        /// The two length checks are here rather than only in the kernel because a
        /// caller can detect a too-small buffer itself, and a refusal the program
        /// can act on beats a fault it cannot.
        pub fn open(
            width: u32,
            height: u32,
            framebuffer: &mut [u8],
            record: &mut [u8]
        ) -> bool {
            if framebuffer.len() as u64 < (width as u64) * (height as u64) * pixel_bytes() {
                return false;
            }
            if record.len() as u64 < record_bytes() {
                return false;
            }
            let status: i64 = display_open(
                width,
                height,
                framebuffer.as_mut_slice().as_ptr(),
                record.as_mut_slice().as_ptr()
            );
            return std::core::succeeded(status);
        }

        /// Presents the frame in `framebuffer`, leaving the frame count in `count`.
        ///
        /// The call *reaching* the driver is not the same as the frame being
        /// shown, and the driver answers both questions: the syscall's own status
        /// says whether the call was valid, and the result record's status says
        /// what became of the frame. Presenting an address that is not the open
        /// window is a valid call about a frame that was refused, so this reads
        /// the record's status and not only the call's.
        pub fn present(framebuffer: &mut [u8], count: &mut [u8]) -> bool {
            if count.len() as u64 < 8u64 {
                return false;
            }
            // Word-backed for the same reason `poll`'s record is: the ABI's
            // records are word-aligned and an array of bytes is not, so a byte
            // array would work or fail depending on where the frame put it.
            let mut result: [u64; 2] = [0u64, 0u64];
            let status: i64 = display_present(
                framebuffer.as_mut_slice().as_ptr(),
                result.as_ptr() as u64 as ptr<u8>
            );
            if !std::core::succeeded(status) {
                return false;
            }
            // `IoResult` is a count then a status, and on this target that is the
            // low word's two halves.
            std::core::write_u64_to(count, result[0]);
            // The frame count is left in `count` either way, so a caller that
            // wants to know how far it got can read it after a refusal.
            return result[1] % 4294967296u64 == 0u64;
        }

        /// The window's width, as `display_open` reported it.
        pub fn record_width(record: &[u8]) -> u32 {
            return rt::sys::read_u32(record, 0);
        }

        /// The window's height, as `display_open` reported it.
        pub fn record_height(record: &[u8]) -> u32 {
            return rt::sys::read_u32(record, 4);
        }

        /// The framebuffer address, as `display_open` reported it.
        ///
        /// This is the address the driver recorded, and it names the *same* memory
        /// the caller passed to `open`. A program that reads a different address
        /// here has been given a different framebuffer, which is worth knowing.
        pub fn record_framebuffer(record: &[u8]) -> u64 {
            return rt::sys::read_u64(record, 8);
        }

        /// Fills the whole canvas with `color`.
        pub fn clear(canvas: &mut [u8], color: u32) {
            let mut pixel: [u8; 4] = [0u8; 4];
            write_pixel(pixel.as_mut_slice(), color);
            let mut at: u64 = 0u64;
            while at + 4u64 <= canvas.len() as u64 {
                canvas[at as usize] = pixel[0];
                canvas[(at + 1u64) as usize] = pixel[1];
                canvas[(at + 2u64) as usize] = pixel[2];
                canvas[(at + 3u64) as usize] = pixel[3];
                at = at + 4u64;
            }
        }

        /// Draws one pixel at (`x`, `y`), clipped to the canvas.
        ///
        /// Returns whether a pixel was drawn. A coordinate outside the canvas
        /// draws nothing, because a program whose animation went off screen has a
        /// cosmetic bug and not a fatal one.
        ///
        /// The coordinate is one *packed* word for the same reason `fill_rect`'s
        /// rectangle is: with the canvas, the geometry and the colour this call
        /// would need seven argument words, and the ABI has six.
        pub fn put_pixel(
            canvas: &mut [u8],
            width: u32,
            height: u32,
            at: u64,
            color: u32
        ) -> bool {
            let x: u32 = ((at / 65536u64) % 65536u64) as u32;
            let y: u32 = (at % 65536u64) as u32;
            if x >= width || y >= height {
                return false;
            }
            let offset: u64 = (y as u64) * (width as u64) * pixel_bytes()
                + (x as u64) * pixel_bytes();
            if offset + 4u64 > canvas.len() as u64 {
                return false;
            }
            let mut pixel: [u8; 4] = [0u8; 4];
            write_pixel(pixel.as_mut_slice(), color);
            canvas[offset as usize] = pixel[0];
            canvas[(offset + 1u64) as usize] = pixel[1];
            canvas[(offset + 2u64) as usize] = pixel[2];
            canvas[(offset + 3u64) as usize] = pixel[3];
            return true;
        }

        /// Packs a point into one word: `x` in the high half, `y` in the low.
        pub fn pack_point(x: u32, y: u32) -> u64 {
            return (x as u64) * 65536u64 + (y as u64);
        }

        /// The `x` of a packed point.
        pub fn point_x(at: u64) -> u32 {
            return ((at / 65536u64) % 65536u64) as u32;
        }

        /// The `y` of a packed point.
        pub fn point_y(at: u64) -> u32 {
            return (at % 65536u64) as u32;
        }

        /// Fills a rectangle, clipped to the canvas on every side.
        ///
        /// The clip is computed on the *requested* rectangle and then drawn row by
        /// row, so a rectangle hanging off two edges draws the part that is visible
        /// and writes nothing outside. A rectangle entirely off the canvas draws
        /// nothing at all, which is the same answer as a zero-sized one.
        ///
        /// The rectangle is one *packed* word rather than four arguments, because
        /// with the canvas, the geometry and the colour this call would need nine
        /// argument words and the ABI has six. That is not a stylistic choice: a
        /// call that cannot be made is a call a program cannot use. The packing is
        /// written down at each field below, so it is a documented shape and not a
        /// surprise.
        pub fn fill_rect(
            canvas: &mut [u8],
            width: u32,
            height: u32,
            packed: u64,
            color: u32
        ) {
            let left: u64 = rect_x(packed) as u64;
            let top: u64 = rect_y(packed) as u64;
            let mut right: u64 = left + rect_w(packed) as u64;
            let mut bottom: u64 = top + rect_h(packed) as u64;
            if right > width as u64 {
                right = width as u64;
            }
            if bottom > height as u64 {
                bottom = height as u64;
            }
            let mut row: u64 = top;
            while row < bottom {
                let mut column: u64 = left;
                while column < right {
                    put_pixel(
                        canvas,
                        width,
                        height,
                        pack_point(column as u32, row as u32),
                        color
                    );
                    column = column + 1u64;
                }
                row = row + 1u64;
            }
        }

        /// Packs a rectangle's origin and size into one word.
        ///
        /// Four 16-bit fields, so a rectangle up to 65535 on a side fits. The
        /// fields are the *same* width, which is the property worth stating: a
        /// packer that gave one field fewer bits than its reader expected would
        /// lose the high half of that coordinate and draw the rectangle somewhere
        /// the caller did not ask for.
        pub fn pack_rect(x: u32, y: u32, w: u32, h: u32) -> u64 {
            return (x as u64) * 281474976710656u64
                + (y as u64) * 4294967296u64
                + (w as u64) * 65536u64
                + (h as u64);
        }

        /// The `x` of a packed rectangle.
        pub fn rect_x(packed: u64) -> u32 {
            return ((packed / 281474976710656u64) % 65536u64) as u32;
        }

        /// The `y` of a packed rectangle.
        pub fn rect_y(packed: u64) -> u32 {
            return ((packed / 4294967296u64) % 65536u64) as u32;
        }

        /// The `w` of a packed rectangle.
        pub fn rect_w(packed: u64) -> u32 {
            return ((packed / 65536u64) % 65536u64) as u32;
        }

        /// The `h` of a packed rectangle.
        pub fn rect_h(packed: u64) -> u32 {
            return (packed % 65536u64) as u32;
        }

        /// The colour of the pixel at (`x`, `y`), or 0 outside the canvas.
        ///
        /// Zero is returned for a coordinate outside the canvas because there is no
        /// pixel there; a caller that drew off the edge and read back zero is
        /// learning that the draw was clipped, which is the truth.
        pub fn get_pixel(canvas: &[u8], width: u32, height: u32, x: u32, y: u32) -> u32 {
            if x >= width || y >= height {
                return 0u32;
            }
            let at: u64 = (y as u64) * (width as u64) * pixel_bytes()
                + (x as u64) * pixel_bytes();
            if at + 4u64 > canvas.len() as u64 {
                return 0u32;
            }
            return read_pixel(rt::memory::slice(canvas.as_ptr() as u64 + at, 4u64));
        }
        /// The 8x8 font, two hexadecimal digits per row byte.
        ///
        /// Ninety-five glyphs for printable ASCII, 32 to 126. Each glyph is eight
        /// rows and each row is one byte of pixels, written as two hexadecimal
        /// digits so the table can live in a string literal: Lazen v1 strings have
        /// no `\x` escape, and a font is exactly the kind of data a string
        /// literal should not have to encode by hand.
        ///
        /// The table is one line because Lazen v1 has no line continuation in a
        /// string. It is data, not code, and reading it is `draw_text`'s job.
        const FONT: &str = "000000000000000030303030300030006C6C0000000000006C6CFF6CFF6C6C003C66603C06663C0066660C1830666600183030703C361E0030300000000000001830606060301800180C0606060C180000CC78FE78CC0000003030FC303000000000000000303060000000FC000000000000000000303000060C183060C000003C666C7E6C663C0030703030303078003C66060C3060FC003C66061C06663C001E366666FF060600FC60607C06663C003860607C66663C00FC060C18303030003C66663C66663C003C66663E06061C00003030003030000000303000303060000C18306030180C000000FC00FC00000030180C060C1830003C66060C300030003C666E6E6E603C003C66667E666666007C66667C66667C003C66606060663C007C66666666667C00FC60607C6060FC00FC60607C606060003C66606E66663E006666667E666666003C18181818183C001E0C0C0C0C6C3800666C78E0786C6600606060606060FC00667E6E6E66666600666E7C6E6E6666003C66666666663C007C66667C606060003C6666666E6C36007C66667C786C66003E66603C06667C00FC303030303030006666666666663C0066666666663C18006666666E6E7E660066663C183C66660066663C1818181800FC060C183060FC003C30303030303C006030180C060300003C0C0C0C0C0C3C00183C66000000000000000000000000FC603000000000000000003E067F667C0060607C6666667C0000003C6660663C000C0C3E6666663E0000003E667E603E001C3630783030300000003E66663E0C3C60607C6666666600300070303030780018003818181838606060666C786C660070303030303078000000D8FEDADADA0000007C666666660000003C6666663C0000007C66667C606000003E66663E0C0C00006E766060600000007C603C067C003030783030361C0000006666666E3A0000006666663C18000000666E6E7E3C000000663C183C660000006666663E0C3C00007E0C18307E000E18183018180E0030303030303030007018180C181870000000366600000000";

        /// The first ASCII code the font has a glyph for.
        pub fn font_first() -> u32 {
            return 32u32;
        }

        /// One past the last ASCII code the font has a glyph for.
        pub fn font_last() -> u32 {
            return 127u32;
        }

        /// The font as bytes, for a caller that wants to read a glyph itself.
        pub fn font_bytes() -> &[u8] {
            return FONT.as_bytes();
        }

        /// How many hexadecimal characters one glyph row occupies: two.
        pub fn glyph_row_chars() -> u64 {
            return 2u64;
        }

        /// The value of one hexadecimal digit, or 255 if it is not one.
        ///
        /// A digit outside `0`..`9` and `A`..`F` has no value, and returning 255
        /// rather than zero means a corrupted table lights up every pixel of the
        /// glyph instead of quietly drawing an empty one.
        pub fn hex_value(digit: u8) -> u32 {
            if digit >= 48u8 && digit <= 57u8 {
                return (digit - 48u8) as u32;
            }
            if digit >= 65u8 && digit <= 70u8 {
                return (digit - 55u8) as u32;
            }
            return 255u32;
        }

        /// The pixel row of one glyph: eight bits, bit 7 leftmost.
        ///
        /// A code outside the font draws nothing, which is returned as a row of no
        /// lit pixels. A program that draws a control character learns that
        /// nothing appeared, rather than drawing the glyph of whatever character
        /// happens to sit at that offset in the table.
        pub fn glyph_row(character: u8, row: u32) -> u8 {
            if character < font_first() as u8 || character >= font_last() as u8 {
                return 0u8;
            }
            if row >= 8u32 {
                return 0u8;
            }
            let font: &[u8] = FONT.as_bytes();
            let at: u64 = (character as u64 - font_first() as u64) * 16u64
                + (row as u64) * glyph_row_chars();
            if at + 1u64 >= font.len() as u64 {
                return 0u8;
            }
            return ((hex_value(font[at as usize]) * 16u32 + hex_value(font[(at + 1u64) as usize])) % 256u32)
                as u8;
        }

        /// Whether one pixel of a glyph row is lit.
        ///
        /// `column` counts from the left, so column 0 is the glyph's leftmost
        /// pixel and is bit 7 of the row byte.
        ///
        /// The bit's weight is found by halving from 128 rather than from a shift
        /// or a table: Lazen v1 has neither a shift operator nor an array-valued
        /// `const`, and at most seven halvings is not a cost worth a data
        /// structure. It also cannot go wrong in a way a hand-written constant
        /// table can — the weight of column 0 is 128 by where the loop starts.
        pub fn glyph_pixel(row: u8, column: u32) -> bool {
            if column >= 8u32 {
                return false;
            }
            let mut weight: u32 = 128u32;
            let mut step: u32 = 0u32;
            while step < column {
                weight = weight / 2u32;
                step = step + 1u32;
            }
            let bit: u32 = (row as u32) / weight;
            return bit % 2u32 == 1u32;
        }

        /// Packs a canvas's size into one word: width in the high half, height in
        /// the low.
        pub fn pack_surface(width: u32, height: u32) -> u64 {
            return (width as u64) * 4294967296u64 + (height as u64);
        }

        /// The width of a packed surface.
        pub fn surface_width(surface: u64) -> u32 {
            return ((surface / 4294967296u64) % 4294967296u64) as u32;
        }

        /// The height of a packed surface.
        pub fn surface_height(surface: u64) -> u32 {
            return (surface % 4294967296u64) as u32;
        }

        /// Packs where text starts and what colour it is in, into one word.
        ///
        /// `x` and `y` are 16 bits each and the colour is 32, which is the whole
        /// word. The reason this is packed at all is the argument count: the
        /// canvas is two words and the text is two more, so the geometry and the
        /// ink have to share what is left of the ABI's six.
        pub fn pack_ink(x: u32, y: u32, color: u32) -> u64 {
            return (x as u64) * 281474976710656u64
                + (y as u64) * 4294967296u64
                + (color as u64);
        }

        /// The `x` of a packed ink.
        pub fn ink_x(ink: u64) -> u32 {
            return ((ink / 281474976710656u64) % 65536u64) as u32;
        }

        /// The `y` of a packed ink.
        pub fn ink_y(ink: u64) -> u32 {
            return ((ink / 4294967296u64) % 65536u64) as u32;
        }

        /// The colour of a packed ink.
        pub fn ink_color(ink: u64) -> u32 {
            return (ink % 4294967296u64) as u32;
        }

        /// Draws `text` with the built-in font, starting at the packed ink's
        /// position, in the packed ink's colour.
        ///
        /// Each character is 8 pixels wide and moves the pen 8 pixels right, so
        /// characters are single-spaced and the last one hangs a column over the
        /// string's width. Text is clipped like everything else: a string that
        /// runs off the right or bottom edge draws the part that is on the canvas,
        /// and one that starts off the left or top edge draws from the first pixel
        /// that is visible.
        ///
        /// The arguments are packed for the reason `pack_ink` says: this is
        /// `canvas`, `surface`, `ink` and `text`, which is two words, one, one and
        /// two.
        pub fn draw_text(canvas: &mut [u8], surface: u64, ink: u64, text: &str) {
            let width: u32 = surface_width(surface);
            let height: u32 = surface_height(surface);
            let mut start: u64 = ink_x(ink) as u64;
            let top: u64 = ink_y(ink) as u64;
            let color: u32 = ink_color(ink);
            let mut at: u64 = 0u64;
            let characters: &[u8] = text.as_bytes();
            while at < characters.len() as u64 {
                let mut row: u64 = 0u64;
                while row < 8u64 {
                    let bits: u8 = glyph_row(characters[at as usize], row as u32);
                    let mut column: u64 = 0u64;
                    while column < 8u64 {
                        if glyph_pixel(bits, column as u32) {
                            put_pixel(
                                canvas,
                                width,
                                height,
                                pack_point(
                                    (start + column) as u32,
                                    (top + row) as u32
                                ),
                                color
                            );
                        }
                        column = column + 1u64;
                    }
                    row = row + 1u64;
                }
                at = at + 1u64;
                start = start + 8u64;
            }
        }
    }    /// Queued input, polled rather than delivered.
    ///
    /// There is no `next_event` and nothing that blocks. A program asks what has
    /// happened and gets an answer, which is what keeps a graphical program's
    /// behaviour a function of its input rather than of when it was scheduled.
    ///
    /// The array is the caller's own memory and the driver fills it, so a program
    /// that stops polling simply stops receiving events.
    pub mod input {
        // The one call that reaches the device. A program has no other way to
        // name it: there is no call that returns an event, and none that injects
        // one.
        extern "syscall" fn input_poll(
            events: ptr<u8>,
            capacity: u32,
            result: ptr<u8>
        ) -> i64;

        /// The event kinds, as the ABI numbers them.
        ///
        /// These are the numbers a `kind` field holds. A kind this build does not
        /// name is still delivered, as its own number, because a program running
        /// against a newer device must be able to *hold* an event it does not
        /// understand and skip it.
        pub fn key_down() -> u32 {
            return 1u32;
        }
        /// A key came up.
        pub fn key_up() -> u32 {
            return 2u32;
        }
        /// The pointer moved.
        pub fn mouse_move() -> u32 {
            return 3u32;
        }
        /// A mouse button went down.
        pub fn mouse_down() -> u32 {
            return 4u32;
        }
        /// A mouse button came up.
        pub fn mouse_up() -> u32 {
            return 5u32;
        }
        /// A character was typed.
        pub fn text() -> u32 {
            return 6u32;
        }
        /// The program was asked to quit.
        pub fn quit() -> u32 {
            return 7u32;
        }

        /// A key this build does not name.
        pub fn key_unknown() -> u32 {
            return 0u32;
        }
        /// Left control.
        pub fn key_left_control() -> u32 {
            return 1u32;
        }
        /// Right control.
        pub fn key_right_control() -> u32 {
            return 2u32;
        }
        /// Left shift.
        pub fn key_left_shift() -> u32 {
            return 3u32;
        }
        /// Right shift.
        pub fn key_right_shift() -> u32 {
            return 4u32;
        }
        /// Left alt.
        pub fn key_left_alt() -> u32 {
            return 5u32;
        }
        /// Right alt.
        pub fn key_right_alt() -> u32 {
            return 6u32;
        }
        /// The left super key, which is Windows or Command.
        pub fn key_left_super() -> u32 {
            return 7u32;
        }
        /// The right super key.
        pub fn key_right_super() -> u32 {
            return 8u32;
        }
        /// Backspace.
        pub fn key_backspace() -> u32 {
            return 9u32;
        }
        /// Tab.
        pub fn key_tab() -> u32 {
            return 10u32;
        }
        /// Return, enter, or the keypad's enter.
        pub fn key_enter() -> u32 {
            return 11u32;
        }
        /// Escape.
        pub fn key_escape() -> u32 {
            return 12u32;
        }
        /// Space.
        pub fn key_space() -> u32 {
            return 13u32;
        }
        /// The minus key, `-`.
        pub fn key_minus() -> u32 {
            return 14u32;
        }
        /// The equals key, `=`.
        pub fn key_equals() -> u32 {
            return 15u32;
        }
        /// Backslash, `\`.
        pub fn key_backslash() -> u32 {
            return 16u32;
        }

        /// The first letter key code, `a`.
        pub fn key_letter_first() -> u32 {
            return 17u32;
        }
        /// The last letter key code, `z`.
        pub fn key_letter_last() -> u32 {
            return 42u32;
        }
        /// The first digit key code, `0`.
        pub fn key_digit_first() -> u32 {
            return 43u32;
        }
        /// The last digit key code, `9`.
        pub fn key_digit_last() -> u32 {
            return 52u32;
        }
        /// Comma, `,`.
        pub fn key_comma() -> u32 {
            return 53u32;
        }
        /// Period, `.`.
        pub fn key_period() -> u32 {
            return 54u32;
        }
        /// Forward slash, `/`.
        pub fn key_slash() -> u32 {
            return 55u32;
        }
        /// Semicolon, `;`.
        pub fn key_semicolon() -> u32 {
            return 56u32;
        }
        /// One past the last key code this build assigns.
        pub fn key_max() -> u32 {
            return 57u32;
        }

        /// How many bytes one event record occupies: a kind, a code, and a pair
        /// of coordinates.
        pub fn record_bytes() -> u64 {
            return 16u64;
        }

        /// How many bytes the result record occupies: a count and a status.
        pub fn result_bytes() -> u64 {
            return 16u64;
        }

        /// Drains up to `capacity` events into `events` and returns how many.
        ///
        /// A return of zero means **nothing pending**, not an error: a program
        /// that polls once per frame is supposed to get zero most of the time.
        ///
        /// If the queue holds more events than the array can take, the rest stays
        /// queued and the next call continues from there. Nothing is ever dropped
        /// silently, which is the whole reason the device is a queue rather than a
        /// sample of the current key state.
        ///
        /// Both the call's status and the count come back checked, because they
        /// are separate claims: a call can be refused *and* report a count, and a
        /// program that read the count without the status would take a refusal for
        /// a partial success.
        ///
        /// The array holds whole records, so `capacity` is bounded by its length
        /// here rather than by the kernel: a program cannot ask for more events
        /// than it has room for, and gets zero instead of a fault.
        pub fn poll(events: &mut [u8], capacity: u32) -> u32 {
            if (events.len() as u64) < (capacity as u64) * record_bytes() {
                return 0u32;
            }
            // The record is backed by *words*, not by bytes. The ABI's records
            // are word-aligned, and an array of bytes has an alignment of one: a
            // frame that happened to place one at an aligned address would work
            // and one that did not would be refused for a misalignment the
            // program never chose and cannot see. Backing it with a pair of
            // words makes the alignment the compiler's decision.
            //
            // The fields are read as words for the same reason. `IoResult` is a
            // count then a status, which on this target is the low word's two
            // halves — so `words[0]` is the count and the low half of `words[1]`
            // is the status, with no byte view needed.
            let mut result: [u64; 2] = [0u64, 0u64];
            let status: i64 = input_poll(
                events.as_mut_slice().as_ptr(),
                capacity,
                result.as_ptr() as u64 as ptr<u8>
            );
            if !std::core::succeeded(status) {
                return 0u32;
            }
            if result[1] % 4294967296u64 != 0u64 {
                return 0u32;
            }
            let count: u64 = result[0];
            if count > capacity as u64 {
                return 0u32;
            }
            return count as u32;
        }

        /// How many events `poll` can take from an array of `len` bytes.
        ///
        /// A whole number of records, so a caller sizing a buffer does not have to
        /// divide by sixteen itself and get it subtly wrong.
        pub fn capacity_for(len: u64) -> u32 {
            return (len / record_bytes()) as u32;
        }

        /// The `kind` of the event in the record at `at`.
        pub fn kind_of(events: &[u8], at: u64) -> u32 {
            return rt::sys::read_u32(events, at * record_bytes());
        }

        /// The `code` of the event in the record at `at`: a key code, a mouse
        /// button, or a Unicode value.
        pub fn code_of(events: &[u8], at: u64) -> u32 {
            return rt::sys::read_u32(events, at * record_bytes() + 4u64);
        }

        /// The `x` of the event in the record at `at`, or zero for an event that
        /// has no position.
        pub fn x_of(events: &[u8], at: u64) -> i32 {
            return rt::sys::read_u32(events, at * record_bytes() + 8u64) as i32;
        }

        /// The `y` of the event in the record at `at`, or zero for an event that
        /// has no position.
        pub fn y_of(events: &[u8], at: u64) -> i32 {
            return rt::sys::read_u32(events, at * record_bytes() + 12u64) as i32;
        }

        /// Whether the event in the record at `at` is of `kind`.
        pub fn is(events: &[u8], at: u64, kind: u32) -> bool {
            return kind_of(events, at) == kind;
        }

        /// Whether `code` names a letter key.
        pub fn is_letter(code: u32) -> bool {
            return code >= key_letter_first() && code <= key_letter_last();
        }

        /// Whether `code` names a digit key.
        pub fn is_digit(code: u32) -> bool {
            return code >= key_digit_first() && code <= key_digit_last();
        }

        /// The lower-case letter a key code names, or zero if it names none.
        ///
        /// The letters are one contiguous range in ASCII order, so the letter is
        /// the code's distance from the start of the range: classification
        /// without a table.
        pub fn letter_of(code: u32) -> u32 {
            if is_letter(code) {
                return 97u32 + (code - key_letter_first());
            }
            return 0u32;
        }

        /// The digit a key code names, or zero if it names none.
        pub fn digit_of(code: u32) -> u32 {
            if is_digit(code) {
                return 48u32 + (code - key_digit_first());
            }
            return 0u32;
        }

        /// The character a `text` event carries, as a Unicode value, or zero.
        ///
        /// The code *is* the scalar value, so this is a spelling of the same
        /// number: a program that reads a text event reads this and nothing else.
        pub fn text_of(events: &[u8], at: u64) -> u32 {
            if is(events, at, text()) {
                return code_of(events, at);
            }
            return 0u32;
        }

        /// The first event in the array that is of `kind`, or `count` if there is
        /// none.
        ///
        /// Returning the count rather than a sentinel means the caller can tell
        /// "not found" from "found at the last index" without a second value.
        pub fn find(events: &[u8], count: u32, kind: u32) -> u32 {
            let mut index: u32 = 0u32;
            while index < count {
                if is(events, index as u64, kind) {
                    return index;
                }
                index = index + 1u32;
            }
            return count;
        }
    }

}
"#;
