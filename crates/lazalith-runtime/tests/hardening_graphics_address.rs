//! Hardening: does `display_open` get the address the program's framebuffer is at?
//!
//! `docs/graphics-test.md` records an open defect: a program that draws a known
//! colour reads its own first pixel back correctly, the drawing works, and the
//! address the display device records is a *different* address from the one the
//! program's own framebuffer occupies. The document could not say which of the two
//! is wrong, because separating them needs a lower-level test than it had budget
//! for.
//!
//! This is that lower-level test, and it is narrower than the graphics suite on
//! purpose. It asks one question with one mechanism: the program takes the address
//! of its framebuffer **itself**, hands the same buffer to the SDK, and the SDK
//! takes the address of that buffer **itself**. If the two disagree, a view of an
//! array moved between the caller and the callee, and the SDK is at fault. If they
//! agree, the address the device recorded is not the one anybody asked for, and the
//! device is at fault.
//!
//! Every answer is printed by the guest, because the host cannot see the guest's
//! stack, and the printer is defined *inside* each program so that the numbers on
//! the console come from the guest's own arithmetic.

use lazalith_types::ArchitectureConfig as C;

/// Builds and runs a Lazen program, returning what it printed and its exit status.
fn run(source: &str) -> (String, u32) {
    let options = lazalith_runtime::BuildOptions {
        architecture: C::lz64(),
        source_path: String::from("probe.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build:\n{error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"));
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        C::lz64(),
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// The printer every probe shares. It is guest code so that the numbers come from
/// the guest's own arithmetic rather than from a host that read them back.
///
/// `write_u64` fills its buffer from the *end* backwards and returns only a count,
/// so the start is `u64_start(room, written)` — a detail the graphics suite's own
/// printer gets right and a first draft of this one did not, which printed the
/// buffer's leading zeroes.
const PRINTER: &str = r#"
fn pu(v: u64) {
    let mut text: [u8; 32] = [0u8; 32];
    let mut out: [u8; 32] = [0u8; 32];
    let written: u64 = std::text::write_u64(v, out.as_mut_slice());
    // `write_u64` fills from the end of the buffer backwards, so the digits start at
    // `8 - written` and have to be moved to the front before the address is handed
    // to `write`. The graphics suite's own printer does exactly this, and a first
    // draft of this one did not — which printed the buffer's leading zeroes and
    // looked for a moment like the platform was returning null addresses.
    let mut at: u64 = 0u64;
    while at < written {
        text[at as usize] = out[(32u64 - written + at) as usize];
        at = at + 1u64;
    }
    let mut sink: [u8; 32] = [0u8; 32];
    rt::sys::write(1, text.as_mut_slice().as_ptr(), written, sink.as_mut_slice().as_ptr());
}
"#;

/// Runs `body` inside a `main` that has a spare `out` buffer and a `record` buffer.
fn probe(body: &str) -> String {
    let source = format!(
        r#"
{PRINTER}
/// The lookalike of the SDK's `open`, written the way the SDK writes it: the same
/// expression, `framebuffer.as_mut_slice().as_ptr()`, on a `&mut [u8]` parameter.
fn handed_to_device(framebuffer: &mut [u8]) -> u64 {{
    return framebuffer.as_mut_slice().as_ptr() as u64;
}}

fn main() -> i32 {{
    let mut out: [u8; 64] = [0u8; 64];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    // Lazen v1 has no `&mut [u8]` to `&[u8]` coercion, so a read-only view of the
    // same bytes is built from the address. The type checker refusing the coercion
    // is a loud failure rather than a wrong answer, and it is recorded in the phase
    // record as a language gap rather than a defect.
    let record_read: &[u8] = rt::memory::slice(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
{body}
    return 0;
}}
"#
    );
    let (output, status) = run(&source);
    assert_eq!(status, 0, "the probe should return 0");
    output
}

/// Every number the probe printed, in order.
fn numbers(output: &str) -> Vec<u64> {
    output
        .split_whitespace()
        .map(|token| {
            token
                .parse()
                .unwrap_or_else(|_| panic!("expected only numbers and spaces, got {output:?}"))
        })
        .collect()
}

#[test]
fn an_array_and_a_view_of_it_are_at_the_same_address() {
    // The simplest question, and the one everything else rests on: taking a view of
    // an array and then taking that view's address must give the array's address.
    // `as_mut_slice` builds a view and `as_ptr` does not, so this is where a
    // disagreement would first show up.
    let output = probe(
        r#"
    let mut framebuffer: [u8; 2048] = [0u8; 2048];
    pu(framebuffer.as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    pu(framebuffer.as_mut_slice().as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    pu(handed_to_device(framebuffer.as_mut_slice()) as u64);
    rt::sys::print("\n");
"#,
    );
    let values = numbers(&output);
    assert_eq!(
        values.len(),
        3,
        "the probe should print three numbers: {output:?}"
    );
    assert_eq!(
        values[0], values[1],
        "a view of an array is at {} and a view of that view is at {}",
        values[0], values[1]
    );
    assert_eq!(
        values[0], values[2],
        "the caller's framebuffer is at {} and the callee sees it at {}",
        values[0], values[2]
    );
}

#[test]
fn a_reference_keeps_its_address_across_a_call() {
    // The question the open defect turns on. A `&mut [u8]` is passed by value to a
    // function; the callee takes `as_mut_slice().as_ptr()` of it and hands that to
    // the device. The caller takes the same expression on the same buffer. If these
    // disagree, a reference does not survive being passed — and *that* would make
    // every graphics program on this platform draw into memory the device never
    // looks at, which is exactly what the recorded evidence looks like.
    let output = probe(
        r#"
    let mut framebuffer: [u8; 2048] = [0u8; 2048];
    let view: &mut [u8] = framebuffer.as_mut_slice();
    pu(view.as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    pu(handed_to_device(view) as u64);
    rt::sys::print(" ");
    pu(framebuffer.as_mut_slice().as_ptr() as u64);
    rt::sys::print("\n");
"#,
    );
    let values = numbers(&output);
    assert_eq!(
        values.len(),
        3,
        "the probe should print three numbers: {output:?}"
    );
    assert_eq!(
        values[0], values[1],
        "a &mut [u8] is at {} in the caller and {} in the callee, so the address \
         the device was given is not the caller's framebuffer",
        values[0], values[1]
    );
    assert_eq!(values[0], values[2]);
}

#[test]
fn the_sdk_hands_the_abi_the_callers_address() {
    // The same question asked of the SDK itself rather than of a lookalike. The
    // record is the ABI's report of the address it was given, so a mismatch is the
    // SDK or the syscall, not the device.
    let output = probe(
        r#"
    let mut framebuffer: [u8; 2048] = [0u8; 2048];
    pu(framebuffer.as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    if !std::graphics::open(16u32, 8u32, framebuffer.as_mut_slice(), record) {
        rt::sys::print("refused\n");
        return 1;
    }
    pu(std::graphics::record_framebuffer(record_read) as u64);
    rt::sys::print("\n");
"#,
    );
    let values = numbers(&output);
    assert_eq!(
        values.len(),
        2,
        "the probe should print two numbers: {output:?}"
    );
    assert_eq!(
        values[0], values[1],
        "the program asked for the display over {} and the ABI recorded {} — so the \
         two addresses differ and the recorded one is not the program's framebuffer",
        values[0], values[1]
    );
}

#[test]
fn a_second_open_reports_the_second_address() {
    // If the recorded address were a *fixed* frame slot rather than the caller's
    // buffer, two different buffers would both be reported as that slot. This is the
    // cheapest test that separates "the SDK miscomputes" from "the kernel invents an
    // address", and it is the one the open defect's evidence could not distinguish.
    let output = probe(
        r#"
    let mut first: [u8; 2048] = [0u8; 2048];
    let mut second: [u8; 2048] = [0u8; 2048];
    if !std::graphics::open(16u32, 8u32, first.as_mut_slice(), record) { return 1; }
    let mut record_words2: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record2: &mut [u8] = rt::memory::slice_mut(
        record_words2.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    let record2_read: &[u8] = rt::memory::slice(
        record_words2.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(16u32, 8u32, second.as_mut_slice(), record2) { return 2; }
    pu(first.as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    pu(std::graphics::record_framebuffer(record_read) as u64);
    rt::sys::print(" ");
    pu(second.as_mut_slice().as_ptr() as u64);
    rt::sys::print(" ");
    pu(std::graphics::record_framebuffer(record2_read) as u64);
    rt::sys::print("\n");
"#,
    );
    let values = numbers(&output);
    assert_eq!(
        values.len(),
        4,
        "the probe should print four numbers: {output:?}"
    );
    let (first_buffer, first_recorded, second_buffer, second_recorded) =
        (values[0], values[1], values[2], values[3]);
    assert_ne!(
        first_buffer, second_buffer,
        "the two buffers should be at different addresses for this test to mean \
         anything; they are both at {first_buffer}"
    );
    assert_eq!(
        first_buffer, first_recorded,
        "the first window was opened over {first_buffer} and the ABI recorded \
         {first_recorded}"
    );
    assert_eq!(
        second_buffer, second_recorded,
        "the second window was opened over {second_buffer} and the ABI recorded \
         {second_recorded}"
    );
}

#[test]
fn a_buffer_written_through_the_abi_is_the_buffer_the_program_reads() {
    // The end-to-end version, and the one that would catch a kernel that *copies*:
    // the program writes a pattern into its own framebuffer, asks the ABI for that
    // address, reads the byte back **through the address the ABI reported**, and
    // checks it is the pattern. A kernel that recorded a different address would
    // send this program to read zeroes.
    let output = probe(
        r#"
    let mut framebuffer: [u8; 2048] = [0u8; 2048];
    framebuffer[0u64 as usize] = 77u8;
    framebuffer[1u64 as usize] = 88u8;
    if !std::graphics::open(16u32, 8u32, framebuffer.as_mut_slice(), record) { return 1; }
    let reported: u64 = std::graphics::record_framebuffer(record_read);
    let through_record: &mut [u8] = rt::memory::slice_mut(reported, 2048u64);
    pu(through_record[0u64 as usize] as u64);
    rt::sys::print(" ");
    pu(through_record[1u64 as usize] as u64);
    rt::sys::print(" ");
    pu(framebuffer[0u64 as usize] as u64);
    rt::sys::print(" ");
    pu(framebuffer[1u64 as usize] as u64);
    rt::sys::print("\n");
"#,
    );
    let values = numbers(&output);
    assert_eq!(
        values.len(),
        4,
        "the probe should print four numbers: {output:?}"
    );
    let (record_byte0, record_byte1, own_byte0, own_byte1) =
        (values[0], values[1], values[2], values[3]);
    assert_eq!(
        (record_byte0, record_byte1),
        (own_byte0, own_byte1),
        "the program wrote {own_byte0} {own_byte1} and read {record_byte0} \
         {record_byte1} back through the address the ABI reported, so the ABI's \
         address is not the program's buffer"
    );
    assert_eq!(
        (own_byte0, own_byte1),
        (77, 88),
        "and the program should have written 77 88"
    );
}

/// The defect `docs/graphics-test.md` records, and the one this whole file exists to
/// chase: the host read the frame back as zeroes for every program.
///
/// A process's memory is in the machine only while it is resident. The runner used
/// to read the presented frame through the machine *after* the scheduler had released
/// the process, at which point the machine held a different set of user regions at
/// the same addresses — zeroed ones belonging to no process. The device's record, the
/// SDK and the ABI were all right; the reader was looking at memory that no longer
/// held the picture.
///
/// This asserts the pixels, which is the assertion step 97 said it could not make
/// honestly. It can now.
#[test]
fn the_host_reads_back_the_pixels_the_program_drew() {
    // A 4×2 window, opaque black everywhere, and one 2×2 white block in the middle.
    // The block is not at a pixel boundary — it starts at x=1 — so a reader that got
    // the row stride or the origin wrong would still produce *some* bytes, just not
    // these.
    let source = r#"
fn main() -> i32 {
    let width: u32 = 4u32;
    let height: u32 = 2u32;
    let mut framebuffer: [u8; 64] = [0u8; 64];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(width, height, framebuffer.as_mut_slice(), record) {
        return 1;
    }
    std::graphics::clear(
        framebuffer.as_mut_slice(),
        std::graphics::rgba(0u8, 0u8, 0u8, 255u8)
    );
    std::graphics::fill_rect(
        framebuffer.as_mut_slice(),
        width,
        height,
        std::graphics::pack_rect(1u32, 0u32, 2u32, 2u32),
        std::graphics::white()
    );
    let mut count: [u8; 8] = [0u8; 8];
    if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) {
        return 3;
    }
    return 0;
}
"#;
    let options = lazalith_runtime::BuildOptions {
        architecture: C::lz64(),
        source_path: String::from("pixels.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build:\n{error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"));
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        C::lz64(),
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(finished.exit_code, 0, "the program should exit 0");
    let frame = finished
        .presented
        .as_ref()
        .expect("the program presented a frame");
    assert_eq!((frame.width, frame.height), (4, 2));
    let pixels = frame
        .pixels
        .as_ref()
        .expect("the host should have been able to read the frame back");
    assert_eq!(pixels.len(), 32, "four pixels of four bytes, twice");

    // The block is at x = 1 and x = 2, in both rows, because the window is 4 wide and
    // the block is 2 wide starting one pixel in.
    //
    // The bytes are **ARGB** with the alpha byte first, which is the order
    // `rgba(r, g, b, a)` establishes and the order `write_pixel` writes. The first
    // draft of this assertion assumed the alpha was last, and failed on the black
    // pixels — which is the test being wrong about the format rather than the
    // platform being wrong about the picture, and is worth one line here so the
    // next reader does not make the same assumption.
    let white = [255u8, 255, 255, 255];
    let black = [255u8, 0, 0, 0];
    for row in 0..2usize {
        for column in 0..4usize {
            let at = (row * 4 + column) * 4;
            let expected: &[u8] = if (1..3).contains(&column) {
                &white
            } else {
                &black
            };
            assert_eq!(
                &pixels[at..at + 4],
                expected,
                "pixel ({column}, {row}) is not what the program drew"
            );
        }
    }
}

/// The same read-back, for a frame the program drew and then *stopped drawing*: the
/// failure this defect produced was a page of zeroes, and a page of zeroes is also
/// what a program that drew nothing would produce. This pins the two apart.
#[test]
fn an_undrawn_frame_reads_back_as_the_colour_the_program_never_wrote() {
    let source = r#"
fn main() -> i32 {
    let mut framebuffer: [u8; 64] = [99u8; 64];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(4u32, 2u32, framebuffer.as_mut_slice(), record) { return 1; }
    let mut count: [u8; 8] = [0u8; 8];
    if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) { return 3; }
    return 0;
}
"#;
    let options = lazalith_runtime::BuildOptions {
        architecture: C::lz64(),
        source_path: String::from("undrawn.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let bytes = lazalith_runtime::RuntimeProgram::build(source, &options)
        .unwrap()
        .to_image_bytes()
        .unwrap();
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        C::lz64(),
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap();
    let frame = finished.presented.as_ref().expect("a frame");
    let pixels = frame
        .pixels
        .as_ref()
        .expect("the host should have been able to read the frame back");
    assert!(
        pixels.iter().all(|byte| *byte == 99),
        "the program filled its canvas with 99 and never drew, so every byte should \
         be 99; got {pixels:?}"
    );
}
