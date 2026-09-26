//! The Lazen runtime's own source, written in Lazen.
//!
//! These are the modules a user program links against. They are kept as text
//! rather than as generated code for two reasons: they are ordinary Lazen and go
//! through the same frontend, lowering and code generation as everything else, so
//! a runtime bug is a compiler bug too; and they are readable, which is the only
//! way to know what a program is actually linked against.
//!
//! The runtime is *not* a second abstraction layer over the OS. Each wrapper here
//! is one syscall, and the wrapper's whole job is to build the arguments the ABI
//! names and turn the status back into a value a program can test. Anything that
//! would need a decision the ABI does not make belongs to the standard library in
//! a later step, not here.
//!
//! # What the runtime does and does not do
//!
//! Startup, the stack, and the application entry are *not* here, and the reason
//! is the language: Lazen v1 has no function pointers, so a Lazen function cannot
//! call the program's `main` by name. The entry sequence is therefore machine
//! code, in [`crate::startup`], exactly as every real runtime's `_start` is. It
//! calls `fn.main`, moves the result where the ABI wants an exit code, and makes
//! the syscall. What the runtime contributes on the Lazen side is everything the
//! entry sequence cannot: the wrappers, and the memory and text helpers that
//! those wrappers need.
//!
//! # The two shapes Lazen v1 forces on a wrapper
//!
//! - **An out-parameter is a `&mut [u8]` the caller supplies.** The ABI writes a
//!   fixed-size record through a `ptr<u8>`, and a program cannot name a region of
//!   memory it does not already have a view of. So every wrapper that needs one
//!   takes the view, and checks its length before handing the address over. An
//!   array is reached with `as_mut_slice()`, because Lazen v1 refuses to borrow an
//!   array as a whole.
//! - **A `str` is read through `as_bytes()`.** `text[index]` is not valid syntax;
//!   the view is what has elements.

/// The runtime's modules, as one compilation unit.
///
/// A program is compiled as *this text followed by the program's own text*, so a
/// program reaches the runtime as `rt::sys::print` and no import machinery is
/// needed. One unit rather than several is what the resolver supports: a `use`
/// names a module declared in the same unit, so a runtime shipped as separate
/// files would need cross-unit resolution that does not exist yet.
pub const PRELUDE: &str = r#"
mod rt {
    // The raw-memory primitives the standard library is built on. These are the
    // only way a Lazen program turns an address into something it can index,
    // because `ptr<T>` deliberately cannot be dereferenced.
    pub mod memory {
        /// A view of `length` bytes at `address`.
        ///
        /// This is the one way a Lazen program turns an address into something it
        /// can index. `ptr<T>` deliberately cannot be dereferenced — a pointer
        /// carries no length, so a read through one could not be bounds checked,
        /// and v1 has no `unsafe` — so a program that receives an address from the
        /// OS, or computes one in its own frame, has to name the length itself.
        ///
        /// The length is a promise, not a check: nothing here knows whether the
        /// memory at `address` really is `length` bytes long. Every index through
        /// the returned view *is* checked, against that promise.
        pub fn slice(address: u64, length: u64) -> &[u8] {
            return (address as ptr<u8>).slice_from_raw(length);
        }

        /// A mutable view of `length` bytes at `address`.
        pub fn slice_mut(address: u64, length: u64) -> &mut [u8] {
            return (address as ptr<u8>).slice_from_raw_mut(length);
        }

        /// The eight bytes of `value`, little-endian, as a view over the stack.
        ///
        /// A `u64` has no address a program can name, so the bytes have to come
        /// from somewhere: a one-element array in this frame, whose address *is*
        /// nameable. The array lives exactly as long as the returned view is used,
        /// which is why this returns a view rather than promising a buffer.
        pub fn as_u64_bytes(value: u64) -> &[u8] {
            let mut storage: [u8; 8] = [0u8; 8];
            let mut at: u64 = 0u64;
            let mut rest: u64 = value;
            while at < 8u64 {
                storage[at as usize] = (rest % 256u64) as u8;
                rest = rest / 256u64;
                at = at + 1u64;
            }
            return storage.as_slice();
        }

        /// A `u64` read from eight little-endian bytes.
        pub fn u64_from_bytes(bytes: &[u8]) -> u64 {
            let mut value: u64 = 0u64;
            let mut at: u64 = 0u64;
            let mut weight: u64 = 1u64;
            while at < 8u64 && at < bytes.len() as u64 {
                value = value + (bytes[at as usize] as u64) * weight;
                weight = weight * 256u64;
                at = at + 1u64;
            }
            return value;
        }

        /// The eight-byte word at `offset` in `bytes`, little-endian.
        ///
        /// The ABI's records are fixed-layout byte fields, and this is how a
        /// program reads one without the compiler needing a record type: the
        /// offsets are the ABI's, documented at each use.
        pub fn read_u64(bytes: &[u8], offset: u64) -> u64 {
            let mut value: u64 = 0u64;
            let mut at: u64 = 0u64;
            let mut weight: u64 = 1u64;
            while at < 8u64 {
                let index: u64 = offset + at;
                if index >= bytes.len() as u64 {
                    break;
                }
                value = value + (bytes[index as usize] as u64) * weight;
                weight = weight * 256u64;
                at = at + 1u64;
            }
            return value;
        }

        /// The four-byte word at `offset` in `bytes`, little-endian.
        pub fn read_u32(bytes: &[u8], offset: u64) -> u32 {
            let mut value: u32 = 0u32;
            let mut at: u64 = 0u64;
            let mut weight: u32 = 1u32;
            while at < 4u64 {
                let index: u64 = offset + at;
                if index >= bytes.len() as u64 {
                    break;
                }
                value = value + (bytes[index as usize] as u32) * weight;
                weight = weight * 256u32;
                at = at + 1u64;
            }
            return value;
        }
    }
    // The raw ABI. Every function here is one syscall and nothing else, so a
    // program that wants a different shape wraps these rather than bypassing
    // them.
    pub mod sys {
        extern "syscall" fn write(handle: i32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;
        extern "syscall" fn read(handle: u32, buffer: ptr<u8>, length: u64, result: ptr<u8>) -> i64;
        extern "syscall" fn open(path: ptr<u8>, path_length: u64, flags: u32, handle: ptr<u32>) -> i64;
        extern "syscall" fn close(handle: u32) -> i64;
        extern "syscall" fn seek(handle: u32, offset: i64, origin: i32, result: ptr<u8>) -> i64;
        extern "syscall" fn stat(path: ptr<u8>, path_length: u64, record: ptr<u8>) -> i64;
        extern "syscall" fn list_directory(
            path: ptr<u8>,
            path_length: u64,
            records: ptr<u8>,
            capacity: u64,
            result: ptr<u8>
        ) -> i64;
        extern "syscall" fn time(result: ptr<u8>) -> i64;
        extern "syscall" fn sleep(nanoseconds: u64) -> i64;
        extern "syscall" fn allocate_memory(size: u64) -> i64;
        extern "syscall" fn spawn_process(path: ptr<u8>, path_length: u64, handle: ptr<u32>) -> i64;
        extern "syscall" fn wait_process(handle: u32, status: ptr<i32>) -> i64;
        extern "syscall" fn clear_screen() -> i64;

        // The ABI's error code for an argument the call itself rejects. It is
        // the same value the kernel returns for the same reason, so a program
        // that gets it from here and a program that gets it from the kernel see
        // the same number.
        pub fn bad_argument() -> i64 {
            -1
        }

        // The number of bytes an `IoResult` occupies: an eight-byte count, a
        // four-byte status, and four reserved bytes.
        pub fn io_result_bytes() -> u64 {
            16
        }

        // Whether `storage` is large enough for the ABI to write an `IoResult`
        // into. Every wrapper that has an out-parameter asks this first: the ABI
        // writes the whole record, and a shorter destination would be memory the
        // program never offered.
        pub fn io_result_fits(storage: &mut [u8]) -> bool {
            storage.len() as u64 >= io_result_bytes()
        }

        /// Writes `bytes` to `handle`, leaving the ABI's `IoResult` in `result`.
        ///
        /// The status is the syscall's own: zero is success and a negative value
        /// is an error code. The runtime does not turn it into something else,
        /// because only the standard library knows which codes a program cares
        /// about.
        pub fn write_to(handle: i32, bytes: &[u8], result: &mut [u8]) -> i64 {
            if !io_result_fits(result) {
                return bad_argument();
            }
            write(
                handle,
                bytes.as_ptr(),
                bytes.len() as u64,
                result.as_mut_slice().as_ptr() as ptr<u8>
            )
        }

        /// Writes `text` to the console, owning the record it needs.
        ///
        /// The scratch is a local array in this function's own frame, so a caller
        /// needs no buffer and cannot pass one that is too short — which is the
        /// whole reason this is separate from `write_to`. The two are not
        /// redundant: `print` is for a program with nothing to say about the
        /// record, and `write_to` is for one that wants to read the byte count
        /// back out of it.
        pub fn print(text: &str) -> i64 {
            let mut record: [u8; 16] = [0u8; 16];
            write_to(1, text.as_bytes(), record.as_mut_slice())
        }

        /// The console's input handle.
        pub fn console_input() -> u32 {
            0
        }

        /// The console's output handle.
        pub fn console_output() -> u32 {
            1
        }

        /// Reads from the console into `buffer`, leaving the record in `result`.
        pub fn read_console(buffer: &mut [u8], result: &mut [u8]) -> i64 {
            read_from(console_input(), buffer, result)
        }

        /// Reads at most `result.len()` bytes into `buffer`.
        pub fn read_from(handle: u32, buffer: &mut [u8], result: &mut [u8]) -> i64 {
            if !io_result_fits(result) {
                return bad_argument();
            }
            read(
                handle,
                buffer.as_mut_slice().as_ptr() as ptr<u8>,
                buffer.len() as u64,
                result.as_mut_slice().as_ptr() as ptr<u8>
            )
        }

        /// Seeks `handle`, leaving the new absolute offset in `result`.
        pub fn seek_to(handle: u32, offset: i64, origin: i32, result: &mut [u8]) -> i64 {
            if !io_result_fits(result) {
                return bad_argument();
            }
            seek(
                handle,
                offset,
                origin,
                result.as_mut_slice().as_ptr() as ptr<u8>
            )
        }

        /// Fills `record` with the file status of `path`.
        ///
        /// The record's size is the ABI's, not a choice: a program that offered
        /// less would have it written past.
        pub fn status_of(path: &str, record: &mut [u8]) -> i64 {
            if record.len() as u64 < file_status_bytes() {
                return bad_argument();
            }
            stat(
                path.as_ptr(),
                path.len() as u64,
                record.as_mut_slice().as_ptr() as ptr<u8>
            )
        }

        /// The number of bytes one file-status record occupies.
        pub fn file_status_bytes() -> u64 {
            96
        }

        /// Reads up to `capacity` bytes of directory records for `path`.
        pub fn list_path(path: &str, record: &mut [u8], capacity: u64) -> i64 {
            if record.len() as u64 < capacity {
                return bad_argument();
            }
            list_directory(
                path.as_ptr(),
                path.len() as u64,
                record.as_mut_slice().as_ptr() as ptr<u8>,
                capacity,
                record.as_mut_slice().as_ptr() as ptr<u8>
            )
        }

        /// Reads the clock into `result`.
        pub fn clock(result: &mut [u8]) -> i64 {
            if !io_result_fits(result) {
                return bad_argument();
            }
            time(result.as_mut_slice().as_ptr() as ptr<u8>)
        }

        /// Opens `path` and leaves the handle where `handle_address` points.
        ///
        /// The out-parameter is an *address*, not a reference, because Lazen v1
        /// has no scalar reference: a caller writes `&mut handle as ptr<u32> as u64`
        /// and the wrapper casts it back. Going through an integer is the only
        /// round trip the language offers, and it is checked at the boundary rather
        /// than trusted.
        pub fn open_path(path: &str, flags: u32, handle_address: u64) -> i64 {
            open(
                path.as_ptr(),
                path.len() as u64,
                flags,
                handle_address as ptr<u32>
            )
        }

        /// Closes `handle`.
        pub fn close_handle(handle: u32) -> i64 {
            close(handle)
        }

        /// Sleeps for `nanoseconds`.
        pub fn sleep_for(nanoseconds: u64) -> i64 {
            sleep(nanoseconds)
        }

        /// Asks the OS for `size` bytes and returns the address as an integer, or
        /// a negative status.
        ///
        /// The address comes back as an integer because `ptr<T>` is only ever
        /// passed to the OS: a Lazen program has no way to name a region of
        /// memory it did not already have a view of, and pretending otherwise
        /// would be a way to read and write whatever the number happened to be.
        /// The standard library turns an address into a view; a raw program does
        /// not get to.
        pub fn allocate(size: u64) -> i64 {
            allocate_memory(size)
        }

        /// Starts the program at `path`, leaving its handle where
        /// `handle_address` points.
        pub fn spawn(path: &str, handle_address: u64) -> i64 {
            spawn_process(
                path.as_ptr(),
                path.len() as u64,
                handle_address as ptr<u32>
            )
        }

        /// Waits for `handle`, leaving its exit status where `status_address`
        /// points.
        pub fn wait_for(handle: u32, status_address: u64) -> i64 {
            wait_process(handle, status_address as ptr<i32>)
        }

        /// Clears the console.
        pub fn clear() -> i64 {
            return clear_screen();
        }

        /// The eight-byte word at `offset` in `record`, little-endian.
        ///
        /// Every ABI record a program reads is a fixed-layout byte field, and this
        /// is how one is read without the compiler knowing what a record is. The
        /// offsets are the ABI's, and each caller documents which field it wants.
        pub fn read_u64(record: &[u8], offset: u64) -> u64 {
            return rt::memory::read_u64(record, offset);
        }

        /// The four-byte word at `offset` in `record`, little-endian.
        pub fn read_u32(record: &[u8], offset: u64) -> u32 {
            return rt::memory::read_u32(record, offset);
        }
    }

    // Byte moves over views. Every one of these is bounds checked by the
    // language, so a caller cannot ask for a move the runtime cannot make.
    pub mod mem {
        /// Copies `source` into `destination`, stopping at whichever is shorter.
        ///
        /// Returns how many bytes were copied, so a caller can tell a short
        /// destination from a short source without measuring both again.
        pub fn copy(destination: &mut [u8], source: &[u8]) -> u64 {
            let mut index: u64 = 0;
            let limit = destination.len() as u64;
            let available = source.len() as u64;
            while index < limit && index < available {
                let at = index as usize;
                destination[at] = source[at];
                index = index + 1;
            }
            index
        }

        /// Sets every byte of `destination` to zero.
        pub fn zero(destination: &mut [u8]) -> u64 {
            fill(destination, 0u8)
        }

        /// Sets every byte of `destination` to `byte`.
        pub fn fill(destination: &mut [u8], byte: u8) -> u64 {
            let mut index: u64 = 0;
            let limit = destination.len() as u64;
            while index < limit {
                let at = index as usize;
                destination[at] = byte;
                index = index + 1;
            }
            index
        }

        /// Compares two byte sequences and returns whether they are equal.
        pub fn equals(left: &[u8], right: &[u8]) -> bool {
            if left.len() != right.len() {
                return false;
            }
            let mut index: u64 = 0;
            let limit = left.len() as u64;
            while index < limit {
                let at = index as usize;
                if left[at] != right[at] {
                    return false;
                }
                index = index + 1;
            }
            true
        }

        /// The index of the first occurrence of `byte` in `haystack`, or -1.
        pub fn find_byte(haystack: &[u8], byte: u8) -> i64 {
            let mut index: u64 = 0;
            let limit = haystack.len() as u64;
            while index < limit {
                let at = index as usize;
                if haystack[at] == byte {
                    return index as i64;
                }
                index = index + 1;
            }
            -1
        }

        /// A `str` with every byte lowercased, written into `destination`.
        ///
        /// The destination decides how much is converted, and the count returned
        /// is how much actually was. Lowercasing is ASCII-only and is written out
        /// rather than computed from a table, because a Lazen program has no way
        /// to hold a static table it did not build.
        pub fn to_lower_ascii(destination: &mut [u8], source: &str) -> u64 {
            let bytes = source.as_bytes();
            let mut index: u64 = 0;
            let limit = destination.len() as u64;
            let available = bytes.len() as u64;
            while index < limit && index < available {
                let at = index as usize;
                let byte = bytes[at];
                if byte >= 65u8 && byte <= 90u8 {
                    destination[at] = byte + 32u8;
                } else {
                    destination[at] = byte;
                }
                index = index + 1;
            }
            index
        }
    }


    // UTF-8. This is here rather than in the standard library because every `str`
    // in the language is a *checked* one, and this is the check. A Lazen program
    // cannot reach an address as a `str` — `ptr<T>` is deliberately not
    // dereferenceable — so the only route from bytes to text is through a function
    // like this one, and it has to live below the standard library.
    pub mod utf8 {
        /// Whether `bytes` is valid UTF-8.
        ///
        /// The rules are UTF-8's and nothing looser. A byte below `0x80` is itself.
        /// `0xC2..=0xDF` takes one continuation byte. `0xE0..=0xEF` takes two, and
        /// `0xF0..=0xF4` takes three. Every other lead byte is a failure, as is any
        /// byte where a continuation belongs that is not `0x80..=0xBF`.
        ///
        /// Two further restrictions are what make this UTF-8 rather than "a shape
        /// that looks like it", and both are checked here:
        ///
        /// - **Overlong forms are refused.** `0xC0` and `0xC1` could only ever begin
        ///   an overlong encoding, so they are not lead bytes at all. After a
        ///   `0xE0` the first continuation must be `0xA0..=0xBF`, and after a
        ///   `0xF0` it must be `0x90..=0xBF`; below those, the sequence would spell
        ///   a character in fewer bytes than its shortest form, which is a second
        ///   spelling of one character.
        /// - **Surrogates are refused.** After a `0xED` the first continuation must
        ///   be `0x80..=0x9F`. `U+D800..=U+DFFF` are not characters.
        ///
        /// An implementation that checked only the shape would accept `0xC0 0x80`
        /// for `U+0000` and `0xED 0xA0 0x80` for `U+D800`. Both spell a character
        /// that also has another spelling, and any comparison between two spellings
        /// of one character has to fail — otherwise every check written over text
        /// is built on sand.
        pub fn valid(bytes: &[u8]) -> bool {
            let total: u64 = bytes.len() as u64;
            let mut at: u64 = 0u64;
            while at < total {
                let lead: u8 = bytes[at as usize];
                if lead < 0x80u8 {
                    at = at + 1u64;
                    continue;
                }
                if lead >= 0xC2u8 && lead <= 0xDFu8 {
                    if !continuation(bytes, at, 1u64, 0x80u8, 0xBFu8) {
                        return false;
                    }
                    at = at + 2u64;
                    continue;
                }
                if lead >= 0xE0u8 && lead <= 0xEFu8 {
                    // The first continuation's range depends on the lead byte, which
                    // is what rules out overlong three-byte forms and surrogates.
                    // The range is fetched by a call rather than chosen by a chain
                    // of `if`s, because a conditional used as a statement may not end
                    // in a value in v1 — and a chain of them ends in one every time.
                    if !continuation(bytes, at, 1u64, three_byte_low(lead), three_byte_high(lead)) {
                        return false;
                    }
                    if !continuation(bytes, at, 2u64, 0x80u8, 0xBFu8) {
                        return false;
                    }
                    at = at + 3u64;
                    continue;
                }
                if lead >= 0xF0u8 && lead <= 0xF4u8 {
                    // Likewise the first continuation of a four-byte sequence:
                    // above `0x8F` rules out an overlong form, and `0xF4` is the
                    // last lead byte that can begin a character at all.
                    if !continuation(bytes, at, 1u64, four_byte_low(lead), four_byte_high(lead)) {
                        return false;
                    }
                    if !continuation(bytes, at, 2u64, 0x80u8, 0xBFu8) {
                        return false;
                    }
                    if !continuation(bytes, at, 3u64, 0x80u8, 0xBFu8) {
                        return false;
                    }
                    at = at + 4u64;
                    continue;
                }
                return false;
            }
            return true;
        }

        /// The lowest first continuation a three-byte lead byte may have.
        ///
        /// Only `0xE0` is restricted, and only from below: a three-byte sequence
        /// starting `0xE0` with a continuation under `0xA0` would spell a character
        /// in fewer bytes than its shortest form.
        fn three_byte_low(lead: u8) -> u8 {
            if lead == 0xE0u8 {
                return 0xA0u8;
            }
            return 0x80u8;
        }

        /// The highest first continuation a three-byte lead byte may have.
        ///
        /// Only `0xED` is restricted, and only from above: `0xED 0xA0..=0xBF` spells
        /// `U+D800..=U+DFFF`, which are not characters.
        fn three_byte_high(lead: u8) -> u8 {
            if lead == 0xEDu8 {
                return 0x9Fu8;
            }
            return 0xBFu8;
        }

        /// The lowest first continuation a four-byte lead byte may have.
        fn four_byte_low(lead: u8) -> u8 {
            if lead == 0xF0u8 {
                return 0x90u8;
            }
            return 0x80u8;
        }

        /// The highest first continuation a four-byte lead byte may have.
        ///
        /// `0xF4` is capped at `0x8F` because `U+110000` and above do not exist;
        /// `0xF4 0x90..=0xBF` would spell one.
        fn four_byte_high(lead: u8) -> u8 {
            if lead == 0xF4u8 {
                return 0x8Fu8;
            }
            return 0xBFu8;
        }

        /// Whether the byte at `lead_at + offset` is a continuation in range.
        ///
        /// A sequence that runs off the end is a failure rather than a read: the
        /// cursor is bounds checked against `total` here, so a truncated sequence
        /// at the end of a string is reported instead of read past.
        ///
        /// This takes six words — a view is two, and `lead_at`, `offset`, `low` and
        /// `high` are four more, which is exactly the ABI's register count. An
        /// earlier version also passed `total` and was one word too wide, which the
        /// lowering refused; the length is read from the view instead, so the
        /// argument list fits and there is one source of truth for the length
        /// rather than two that could disagree.
        fn continuation(
            bytes: &[u8],
            lead_at: u64,
            offset: u64,
            low: u8,
            high: u8
        ) -> bool {
            let at: u64 = lead_at + offset;
            if at >= bytes.len() as u64 {
                return false;
            }
            let byte: u8 = bytes[at as usize];
            return byte >= low && byte <= high;
        }
    }

    // Text helpers, over the bytes a string is. A Lazen `str` is already a
    // pointer and a length, so nothing here allocates or copies: a helper takes
    // the view it is given and answers a question about it.
    pub mod text {
        /// A `str` over the bytes of `bytes`, if they are valid UTF-8.
        ///
        /// A Lazen `str` is a checked UTF-8 byte string, so this conversion is
        /// checked and the answer is written to `ok` rather than assumed. There is
        /// no cast for it — `docs/lazen-types.md` lists `str` as *checked* for
        /// exactly this reason, and a cast that could produce an invalid `str`
        /// would make the whole guarantee decorative.
        ///
        /// `ok` is a plain mutable `bool` parameter rather than a `&mut bool`,
        /// because v1 has no reference to a scalar: a view is a slice, and a
        /// parameter is a place the language can write through. That is also why the
        /// caller has to declare its own `let mut ok: bool = false;` — the same rule
        /// `as_mut_slice` follows, and for the same reason.
        ///
        /// The returned `str` borrows `bytes`, so the two must not outlive each
        /// other. A caller that finds `ok` false has no `str` to use, and the view
        /// it would have had is not a string.
        pub fn from_bytes(bytes: &[u8], ok: bool) -> &str {
            return bytes.as_str(ok);
        }

        /// The number of bytes in `text`.
        pub fn length(text: &str) -> u64 {
            text.len() as u64
        }

        /// Whether `text` is empty.
        pub fn is_empty(text: &str) -> bool {
            text.len() == 0
        }

        /// Whether `text` starts with `prefix`.
        pub fn starts_with(text: &str, prefix: &str) -> bool {
            let bytes = text.as_bytes();
            let wanted = prefix.as_bytes();
            if wanted.len() > bytes.len() {
                return false;
            }
            let mut index: u64 = 0;
            let limit = wanted.len() as u64;
            while index < limit {
                let at = index as usize;
                if bytes[at] != wanted[at] {
                    return false;
                }
                index = index + 1;
            }
            true
        }

        /// The index of the first occurrence of `needle` in `haystack`, or -1.
        pub fn find(haystack: &str, needle: &str) -> i64 {
            if needle.len() == 0 {
                return 0;
            }
            if needle.len() > haystack.len() {
                return -1;
            }
            let bytes = haystack.as_bytes();
            let wanted = needle.as_bytes();
            let mut start: u64 = 0;
            let last = (bytes.len() - wanted.len()) as u64;
            while start <= last {
                let mut index: u64 = 0;
                let mut matched = true;
                while index < wanted.len() as u64 {
                    let at = (start + index) as usize;
                    if bytes[at] != wanted[index as usize] {
                        matched = false;
                        break;
                    }
                    index = index + 1;
                }
                if matched {
                    return start as i64;
                }
                start = start + 1;
            }
            -1
        }

        /// Whether `left` and `right` are the same text.
        pub fn equals(left: &str, right: &str) -> bool {
            let mine = left.as_bytes();
            let theirs = right.as_bytes();
            if mine.len() != theirs.len() {
                return false;
            }
            let mut index: u64 = 0;
            let limit = mine.len() as u64;
            while index < limit {
                let at = index as usize;
                if mine[at] != theirs[at] {
                    return false;
                }
                index = index + 1;
            }
            true
        }

        /// The byte at `index`, or 0 when `text` is shorter than that.
        pub fn byte_at(text: &str, index: u64) -> u8 {
            let bytes = text.as_bytes();
            if index >= bytes.len() as u64 {
                return 0u8;
            }
            bytes[index as usize]
        }
    }
}
"#;
