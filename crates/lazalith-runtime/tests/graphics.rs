//! Step 97: a graphical Lazen program, booted, drawing, and answering the keyboard.
//!
//! ```text
//! Lazen SDK          std::graphics and std::input, written in Lazen
//!  ↓
//! LazOS              display_open, display_present, input_poll
//!  ↓
//! virtual devices    the display device and the input device
//!  ↓
//! SDL3               the frame a host frontend would blit — see the note below
//! ```
//!
//! # What this test reaches, and what it does not
//!
//! It runs real Lazen programs through a real kernel on a real machine and checks
//! the things that can be checked from both sides:
//!
//! - **the device's own record** — the window opened at the geometry the program
//!   asked for, and the number of frames it presented;
//! - **the program's own framebuffer** — a program that draws a known colour reads
//!   its own canvas back through its own pointer and prints what it finds, so the
//!   claim "the drawing calls wrote the pixels" is made by the code that did the
//!   writing rather than by a host peeking at memory;
//! - **the program's response to input** — a program that prints a number derived
//!   from the keys it read, so the answer is in its own output and the arithmetic
//!   can be checked by hand.
//!
//! It does **not** assert the presented frame's pixels from the host side, and
//! `docs/graphics-test.md` explains why with the evidence: the address the display
//! device records is not the address the program's own framebuffer lives at, and
//! it is not the same from run to run. `Finished::presented` therefore reports
//! `pixels: None` when the recorded address cannot be read, rather than handing back
//! a page of zeroes that looks exactly like a program which drew nothing.
//!
//! SDL3 is not linked, and that is not laziness: it needs a display server, and
//! every machine this project's tests run on is headless. The SDL3 half is covered
//! by `lazalith-sdl3`'s own tests. What this file pins is the *contract between
//! them* — a presented frame is four bytes per pixel in the guest's own memory at an
//! address the device was given — and it asserts the side this project owns.
//!
//! # Why a purpose-built program as well as the example
//!
//! `the_repository_s_window_example_runs_under_the_kernel` runs
//! `examples/window/main.lz`, the program a person would run. The rest use small
//! programs written for this file, because "responds to input" and "drew what it
//! says it drew" need provable answers, and a program's own output is a provable
//! answer while "the frame changed" is not — the example's block moves on its own.

use lazalith_devices::{DeviceManager, EventKind, NoDevice};
use lazalith_runtime::{BuildOptions, RuntimeProgram, SeedEvent, run_image_with};
use lazalith_types::ArchitectureConfig;

/// Builds a program and returns the `.lzx` bytes, the same path step 96 uses.
fn build(source: &str) -> Vec<u8> {
    let options = BuildOptions {
        architecture: ArchitectureConfig::lz64(),
        source_path: String::from("graphics.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    RuntimeProgram::build(source, &options)
        .unwrap_or_else(|error| panic!("the program should build: {error}"))
        .to_image_bytes()
        .unwrap_or_else(|error| panic!("the program should link: {error}"))
}

/// The repository's own window example, read from the file it ships as.
fn window_example() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/window/main.lz");
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{} should read: {error}", path))
}

/// A program that opens a 16-by-8 window, clears it, draws a white block in the
/// top-left corner, presents it, and then reads its *own* canvas back and prints
/// the first pixel.
///
/// The read-back is the point. Everything the program says about its pixels comes
/// from the same code that drew them, through the same pointer, so a host that
/// cannot vouch for the frame's address does not have to vouch for the drawing.
const DRAWS_AND_REPORTS: &str = r#"
fn main() -> i32 {
    let width: u32 = 16u32;
    let height: u32 = 8u32;
    let mut framebuffer: [u8; 2048] = [0u8; 2048];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(width, height, framebuffer.as_mut_slice(), record) {
        rt::sys::print("open refused\n");
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
        std::graphics::pack_rect(0u32, 0u32, 8u32, 4u32),
        std::graphics::white()
    );
    let mut count: [u8; 8] = [0u8; 8];
    if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) {
        rt::sys::print("present refused\n");
        return 3;
    }
    // The first pixel is inside the block, so it is white: 255, 255, 255, 255.
    // The program reads its own canvas and prints the four bytes, so the claim
    // comes from the code that drew rather than from a host.
    rt::sys::write(1, framebuffer.as_mut_slice().as_ptr(), 4u64, count.as_mut_slice().as_ptr());
    rt::sys::print(" drawn\n");
    return 0;
}
"#;

/// A program whose printed answer is its position, moved by the keys it reads.
const MOVING_BLOCK: &str = r#"
fn main() -> i32 {
    let width: u32 = 32u32;
    let height: u32 = 8u32;
    let mut framebuffer: [u8; 1024] = [0u8; 1024];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(width, height, framebuffer.as_mut_slice(), record) {
        return 1;
    }

    let mut left: u32 = 0u32;
    let mut events: [u8; 16] = [0u8; 16];

    // Three passes. Each draws, presents, and then drains the keyboard, so a
    // program that ignored its input and one that read it differ in the number the
    // third pass prints.
    let mut pass: u32 = 0u32;
    while pass < 3u32 {
        std::graphics::clear(
            framebuffer.as_mut_slice(),
            std::graphics::rgba(0u8, 0u8, 0u8, 255u8)
        );
        std::graphics::fill_rect(
            framebuffer.as_mut_slice(),
            width,
            height,
            std::graphics::pack_rect(left, 0u32, 4u32, 8u32),
            std::graphics::white()
        );
        let mut count: [u8; 8] = [0u8; 8];
        if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) {
            return 2;
        }

        let mut pending: u32 = std::input::poll(events.as_mut_slice(), 1u32);
        while pending > 0u32 {
            let character: u32 = std::input::text_of(events.as_slice(), 0u64);
            // Only `d` does anything, so a program that moved for every event
            // would fail this rather than pass for the wrong reason.
            if character == 100u32 {
                left = left + 4u32;
            }
            pending = std::input::poll(events.as_mut_slice(), 1u32);
        }
        pass = pass + 1u32;
    }

    // Print the final position, as two digits: `left` is a multiple of four and
    // never more than 12, so one digit plus the tens digit is the whole number.
    let mut out: [u8; 8] = [0u8; 8];
    let written: u64 = std::text::write_u64(left as u64, out.as_mut_slice());
    let mut text: [u8; 8] = [0u8; 8];
    let mut at: u64 = 0u64;
    while at < written {
        text[at as usize] = out[(8u64 - written + at) as usize];
        at = at + 1u64;
    }
    rt::sys::write(1, text.as_mut_slice().as_ptr(), written, text.as_mut_slice().as_ptr());
    rt::sys::print("\n");
    return 0;
}
"#;

#[test]
fn the_repository_s_window_example_runs_under_the_kernel() {
    // The program a person would actually run, through every layer below it.
    let image = build(&window_example());
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the window example should run: {error}"));
    assert_eq!(finished.exit_code, 0, "the example returns zero");
    let frame = finished
        .presented
        .as_ref()
        .expect("the example presents at least one frame");
    assert_eq!(
        (frame.width, frame.height),
        (48, 32),
        "the window's geometry, as the example asked for it"
    );
    assert!(
        frame.present_count >= 2,
        "the example runs two frames, saw {}",
        frame.present_count
    );
    assert_eq!(
        frame.address % 4,
        0,
        "a framebuffer is word aligned, so the address the device was given is"
    );
}

#[test]
fn a_window_opens_at_the_geometry_the_program_asked_for() {
    // Three programs, three geometries, all of them the *program's* numbers. A
    // device that answered from a default, or a kernel that ignored the arguments,
    // would give the same answer for all three.
    for (width, height) in [(16u32, 8u32), (32, 16), (8, 4)] {
        let image = build(&format!(
            r#"
fn main() -> i32 {{
    let width: u32 = {width}u32;
    let height: u32 = {height}u32;
    let mut framebuffer: [u8; 4096] = [0u8; 4096];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    if !std::graphics::open(width, height, framebuffer.as_mut_slice(), record) {{
        return 1;
    }}
    let mut count: [u8; 8] = [0u8; 8];
    if !std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice()) {{
        return 2;
    }}
    return 0;
}}
"#
        ));
        let finished = run_image_with(
            &image,
            ArchitectureConfig::lz64(),
            DeviceManager::<NoDevice>::new(),
        )
        .unwrap_or_else(|error| panic!("a {width}x{height} window should open: {error}"));
        let frame = finished
            .presented
            .unwrap_or_else(|| panic!("a {width}x{height} window presented no frame"));
        assert_eq!(
            (frame.width, frame.height),
            (width as u64, height as u64),
            "the device must report the geometry the program asked for"
        );
        assert_eq!(frame.present_count, 1, "one present, one frame");
    }
}

#[test]
fn a_program_draws_the_pixels_it_says_it_drew() {
    // The drawing path, asserted by the program that used it: it clears, fills, and
    // then reads its own canvas back through its own pointer. The first pixel is
    // inside the block and must be white in all four bytes.
    let image = build(DRAWS_AND_REPORTS);
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(finished.exit_code, 0, "the program returns zero");
    let output = finished.output;
    assert_eq!(
        output.len(),
        11,
        "four pixel bytes and a marker, got {output:?}"
    );
    assert_eq!(
        &output[..4],
        &[255u8, 255, 255, 255],
        "the pixel inside the block must be the white that was drawn into it"
    );
    assert_eq!(&output[4..], b" drawn\n", "the marker the program prints");
}

#[test]
fn a_program_that_ignored_the_keyboard_never_moves_its_block() {
    // The control. The same program, the same run, no input seeded: the position
    // stays at zero and the program says so. Without this, "responded to input"
    // could be satisfied by a program that always moves.
    let image = build(MOVING_BLOCK);
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "0\n",
        "with no keyboard, the block never moves"
    );
    assert_eq!(
        finished.input_delivered, 0,
        "no events were given, so none were delivered"
    );
}

#[test]
fn a_keystroke_moves_the_block_and_the_program_reports_where_it_ended() {
    // The step's actual claim: a graphical application that *responds* to input.
    // One `d` per pass, three passes, four pixels each, and the program prints the
    // result — so the expected answer is checkable by hand.
    let image = build(MOVING_BLOCK);
    let seed: Vec<SeedEvent> = (0..3)
        .map(|_| SeedEvent {
            kind: EventKind::Text.as_u32(),
            code: b'd' as u32,
            x: 0,
            y: 0,
        })
        .collect();
    let finished = lazalith_runtime::run_image_seeded(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
        &seed,
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "12\n",
        "three keystrokes of four pixels each, from zero"
    );
    assert_eq!(
        finished.input_delivered, 3,
        "the program must have been given all three events"
    );
    assert_eq!(
        finished
            .presented
            .expect("a frame was presented")
            .present_count,
        3,
        "three passes, three presents"
    );
}

#[test]
fn a_keystroke_the_program_does_not_handle_changes_nothing() {
    // The other half of the previous claim. An event naming a key the program
    // ignores — 'x' — must not move the block, so the test above is about the
    // program's logic and not about a device that moves whatever it is given.
    let image = build(MOVING_BLOCK);
    let seed: Vec<SeedEvent> = (0..3)
        .map(|_| SeedEvent {
            kind: EventKind::Text.as_u32(),
            code: b'x' as u32,
            x: 0,
            y: 0,
        })
        .collect();
    let finished = lazalith_runtime::run_image_seeded(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
        &seed,
    )
    .unwrap_or_else(|error| panic!("the program should run: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "0\n",
        "an unhandled key must not move the block"
    );
    assert_eq!(
        finished.input_delivered, 3,
        "the events were delivered; the program just ignored them"
    );
}

#[test]
fn a_program_that_never_opens_a_window_presents_nothing() {
    // The negative case, which is what keeps the positive ones honest: a program
    // that does not use the display has no frame, and reporting one would mean the
    // runner invented it.
    let image = build(
        r#"
fn main() -> i32 {
    rt::sys::print("no window here\n");
    return 0;
}
"#,
    );
    let finished = run_image_with(
        &image,
        ArchitectureConfig::lz64(),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the program should run");
    assert_eq!(
        String::from_utf8_lossy(&finished.output),
        "no window here\n"
    );
    assert!(
        finished.presented.is_none(),
        "a program that never presented a frame has none to report"
    );
    assert_eq!(finished.input_delivered, 0, "and no events were given");
}
