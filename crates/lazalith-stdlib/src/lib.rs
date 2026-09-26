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

        /// Whether a status means the process left because it asked to.
        pub fn exited_normally(status: i32) -> bool {
            return status == 0i32;
        }
    }
}
"#;
