//! Step 73: the Lazen GUI library, drawing.
//!
//! The library is Lazen source in `lazalith-ui`, so testing it is a matter of
//! writing Lazen programs that use it and looking at the pixels. Nothing here
//! reaches into the library: a program draws with `gui::draw_*` and this test
//! reads the frame the device presented, exactly as Step 72's does.
//!
//! What each test states is one thing the library claims:
//!
//! - a widget set is written on the SDK and reaches nothing below it, so a
//!   program that uses only `gui` gets a window and a keyboard;
//! - a layout moves a cursor, and a widget drawn from that cursor is where the
//!   cursor said it would be;
//! - a hit test and the drawing agree about where a widget's edges are, which is
//!   the property that makes a button pressable where it looks pressable;
//! - a menu answers "which item" with a number, and the item count is "none";
//! - editing a field is three rules, and each of them is stated by a test.

use std::vec::Vec;

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::Privilege;
use lazalith_devices::{DeviceManager, Event, EventKindValue, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{
    KernelServiceOutcome, LazalithKernel, LzxImage, ProcessId, ThreadId, VirtualFileSystem,
    VirtualTerminal,
};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::{ArchitectureConfig, InstructionAddress, VirtualAddress};

/// The window every program in this file opens.
///
/// A widget set is not a window, so the size here is the test's and not the
/// library's. It is small because `std::graphics::put_pixel` costs about 2300
/// instructions on this backend — a measurement Step 72 took and
/// `docs/lazen-graphics.md` records — so a window big enough to be a real one
/// would not fit the tool's runaway budget.
const WIDTH: u32 = 32;
const HEIGHT: u32 = 24;
const SIZE: u64 = 32 * 24 * 4;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid id");

fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

fn build(source: &str) -> LzxImage {
    let program =
        RuntimeProgram::build(source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    LzxImage::from_bytes(&bytes).expect("the image reads back")
}

/// Runs `source` to completion with `events` queued, and returns the kernel.
///
/// The program is run under the real scheduler with a one-instruction quantum
/// and the image goes through its serialised form, so this is the path a user's
/// program takes rather than a shortcut through the driver.
fn run(source: &str, events: &[Event]) -> (LazalithKernel, Option<u32>) {
    let config = ArchitectureConfig::lz64();
    let image = build(source);
    let boot = BootImage::new(config, supervisor_kernel(config), 0).expect("a boot image");
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    assert_eq!(
        machine.architectural_state().privilege(),
        Privilege::Supervisor
    );
    machine.step().expect("the kernel's first step");

    let mut kernel = LazalithKernel::new(
        1,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
    )
    .expect("the kernel starts");
    for event in events {
        kernel
            .input_mut()
            .device_mut()
            .inject(*event)
            .expect("the event is queued");
    }
    kernel
        .start_image(image, PID, TID)
        .expect("the program is scheduled");

    let mut exit = None;
    for _ in 0..20_000_000 {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => panic!("the program faulted: {error:?}"),
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                panic!(
                    "the program trapped ({cause:?}, payload {payload}) at {:#x}",
                    machine.architectural_state().pc().as_u64()
                )
            }

            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    (kernel, exit)
}

/// The bytes of the framebuffer the device last presented.
fn presented(kernel: &mut LazalithKernel) -> Vec<u8> {
    let frame = kernel
        .display()
        .device()
        .presented()
        .expect("a frame was presented");
    let bytes = frame.bytes().expect("the frame has a size");
    let address = frame.address;
    let mut out = vec![0u8; usize::try_from(bytes).expect("a host-sized frame")];
    kernel
        .scheduler_mut()
        .process_mut(PID)
        .expect("the process is still known")
        .memory_context()
        .expect("a memory context")
        .read_bytes(VirtualAddress::new(address), &mut out)
        .expect("the framebuffer is readable");
    out
}

fn pixel(frame: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at = usize::try_from(u64::from(y) * u64::from(WIDTH) * 4 + u64::from(x) * 4)
        .expect("a host-sized offset");
    [frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]
}

/// A mouse-down at (`x`, `y`).
fn clicked(x: i32, y: i32) -> Event {
    Event {
        kind: EventKindValue(lazalith_devices::EventKind::MouseDown.as_u32()),
        code: 1,
        x,
        y,
    }
}

/// A program's usual shape: open, draw through `gui`, present, exit.
///
/// `body` is a run of *statements*, not an expression — v1 has no block
/// expression, and a test that needed one would be testing the template rather
/// than the library. A `return` in the body leaves `main` without presenting,
/// which is how a check reports a wrong answer through the exit code.
fn program(body: &str) -> String {
    format!(
        r#"
fn main() -> i32 {{
    let width: u32 = {WIDTH}u32;
    let height: u32 = {HEIGHT}u32;
    let size: u64 = std::graphics::pack_surface(width, height);
    let mut canvas: [u8; {SIZE}] = [0u8; {SIZE}];
    if !gui::open(width, height, canvas.as_mut_slice()) {{ return 90; }}
{body}
    let mut count: [u8; 8] = [0u8; 8];
    if !gui::present(canvas.as_mut_slice(), count.as_mut_slice()) {{ return 91; }}
    return 0;
}}
"#
    )
}

/// A program that reports an answer in its exit code and draws nothing.
///
/// Some properties are pure functions of their arguments — a hit test, a menu's
/// answer to a point — and the honest way to test one is to ask it and read the
/// answer. These programs open no window and present no frame, so a program that
/// ends in `return` does not run into the "instruction follows a terminator"
/// that inlining a body after one would cause.
fn answering(body: &str) -> String {
    format!(
        r#"
fn main() -> i32 {{
{body}
}}
"#
    )
}

/// A widget set is enough to open a window, draw and present.
///
/// The program does nothing but call `gui::open`, `gui::draw_panel`,
/// `gui::present` — and it works. That is the step's claim stated as a fact: a
/// program written against the widget set needs no `std::graphics` call of its
/// own to put a picture on the screen, and certainly nothing host-shaped.
#[test]
fn the_widget_set_opens_a_window_and_presents_a_frame() {
    let source = program(
        r#"
        gui::draw_panel(
            canvas.as_mut_slice(),
            size,
            std::graphics::pack_rect(0u32, 0u32, 8u32, 8u32),
            std::graphics::white()
        );
        "#,
    );
    let (mut kernel, exit) = run(&source, &[]);
    assert_eq!(exit, Some(0), "the program ran to completion");
    assert_eq!(
        kernel.display().device().width(),
        u64::from(WIDTH),
        "the window opened through the widget set is as wide as asked"
    );
    assert_eq!(
        kernel
            .display()
            .device()
            .presented()
            .map(|f| f.present_count),
        Some(1),
        "the frame reached the device"
    );
    let frame = presented(&mut kernel);
    assert_eq!(
        pixel(&frame, 0, 0),
        [255, 255, 255, 255],
        "the panel drew at the origin"
    );
    assert_eq!(
        pixel(&frame, 9, 0),
        [0, 0, 0, 0],
        "and only inside its own rectangle"
    );
}

/// A layout moves a cursor down, and a widget lands where the cursor said.
///
/// Three panels are laid out with `layout_step` and the test reads all three.
/// A layout that advanced by a fixed amount, or not at all, would put at least
/// one of them somewhere else — so reading every one is what makes this a test
/// of the layout rather than of the first widget.
#[test]
fn a_layout_places_widgets_where_the_cursor_said() {
    let source = program(
        r#"
        let mut cursor: u64 = gui::layout_begin(1u32, 2u32, gui::layout_down());
        let tall: u32 = 4u32;
        let gap: u32 = 2u32;
        let mut drawn: u32 = 0u32;
        while drawn < 3u32 {
            let rect: u64 = std::graphics::pack_rect(
                gui::layout_x(cursor),
                gui::layout_y(cursor),
                3u32,
                tall
            );
            if !gui::draw_panel(
                canvas.as_mut_slice(),
                size,
                rect,
                std::graphics::white()
            ) { return 1; }
            cursor = gui::layout_step(cursor, tall, gap);
            drawn = drawn + 1u32;
        }
        if gui::layout_y(cursor) != 2u32 + 3u32 * (4u32 + 2u32) { return 2; }
        if gui::layout_x(cursor) != 1u32 { return 3; }
        "#,
    );
    let (mut kernel, exit) = run(&source, &[]);
    assert_eq!(exit, Some(0), "the layout advanced as the program expected");

    let frame = presented(&mut kernel);
    // First row at y = 2, second at y = 8, third at y = 14: each is the previous
    // one's height of 4 plus the gap of 2. And x is 1 for all three, because a
    // downward layout moves y and leaves x alone.
    for row in 0..3u32 {
        let top: u32 = 2u32 + row * 6u32;
        assert_eq!(
            pixel(&frame, 1, top),
            [255, 255, 255, 255],
            "a widget is at the top of its row"
        );
        assert_eq!(
            pixel(&frame, 3, top),
            [255, 255, 255, 255],
            "and three wide"
        );
        assert_eq!(
            pixel(&frame, 4, top),
            [0, 0, 0, 0],
            "and one pixel past its right edge is not"
        );
        assert_eq!(
            pixel(&frame, 1, top + 4u32),
            [0, 0, 0, 0],
            "and one row past its bottom edge is not either"
        );
        assert_eq!(
            pixel(&frame, 1, top + 5u32),
            [0, 0, 0, 0],
            "the gap is empty"
        );
    }
}

/// The hit test and the drawing agree about where a button's edges are.
///
/// The test clicks four points: two inside the button and two just outside it,
/// and the two outside are the interesting ones. A hit test that used closed
/// edges would claim the column to the right of the button as part of it, and a
/// button that is drawn one way and pressed another is the bug a person notices
/// first and believes least.
#[test]
fn a_button_is_clicked_exactly_where_it_is_drawn() {
    // One rectangle, written once, used by every run below. The test clicks six
    // points: two inside the button and four just outside it, and the four
    // outside are the interesting ones. A hit test that used closed edges would
    // claim the column to the right of the button as part of it, and a button
    // that is drawn one way and pressed another is the bug a person notices first
    // and believes least.
    let rect: &str = "std::graphics::pack_rect(4u32, 4u32, 8u32, 8u32)";
    for (x, y, want, why) in [
        (4i32, 4i32, 0i32, "the near corner is inside"),
        (11, 11, 0, "the far corner is inside"),
        (12, 4, 7, "one past the right edge is not"),
        (4, 12, 7, "one past the bottom edge is not"),
        (3, 4, 7, "one before the left edge is not"),
        (4, 3, 7, "one before the top edge is not"),
    ] {
        // `button_clicked` is a pure function of the events it is given, so each
        // point is its own run, and the program reports the answer in its exit
        // code: 0 for claimed, 7 for not. The events are *polled*, not handed
        // over as a zeroed array — the point of the test is that a press which
        // travelled through the device, the driver and the ABI is the press the
        // hit test sees.
        let probe = answering(&format!(
            r#"
            let mut events: [u8; 16] = [0u8; 16];
            let count: u32 = std::input::poll(events.as_mut_slice(), 1u32);
            if count != 1u32 {{ return 8; }}
            let rect: u64 = {rect};
            if gui::button_clicked(events.as_slice(), count, rect) {{ return 0; }}
            return 7;
            "#,
            rect = rect
        ));
        let (_, exit) = run(&probe, &[clicked(x, y)]);
        assert_eq!(exit, Some(want as u32), "{why} (at {x},{y})");
    }
}

/// A menu answers with a number, and the item count is "none".
///
/// The test clicks each of four columns and one point below the bar. The
/// interesting case is the last one: a menu that returned 0 for a miss would
/// select the first item every time anything was clicked, which is the bug this
/// convention exists to prevent.
#[test]
fn a_menu_reports_which_item_was_clicked() {
    let bar: &str = "std::graphics::pack_rect(0u32, 0u32, 12u32, 6u32)";
    for (x, y, want, why) in [
        (0i32, 0i32, 0i32, "the first column"),
        (3, 0, 0, "the first column, again, at its right half"),
        (4, 0, 1, "the second column"),
        (8, 0, 2, "the third column"),
        (9, 0, 2, "the third column, at its right half"),
        (20, 0, 3, "a click to the right of the bar"),
        (0, 9, 3, "a click below the bar"),
    ] {
        let probe = answering(&format!(
            r#"
            let mut events: [u8; 16] = [0u8; 16];
            let count: u32 = std::input::poll(events.as_mut_slice(), 1u32);
            if count != 1u32 {{ return 8; }}
            let bar: u64 = {bar};
            let chose: u32 = gui::menu_clicked(events.as_slice(), count, bar, 3u32);
            if chose != {want}u32 {{ return 7; }}
            return 0;
            "#,
            bar = bar
        ));
        let (_, exit) = run(&probe, &[clicked(x, y)]);
        assert_eq!(exit, Some(0), "{why} (at {x},{y})");
    }
}

/// A menu's selected item is drawn, and `menu_select` changes it.
///
/// The mark is a different colour, so the test reads the selected column and the
/// unselected one. A `menu_select` that lost the item count or the bar colour
/// would draw a menu with one item or the wrong colour, and reading the mark
/// catches the first while the surrounding pixels catch the second.
#[test]
fn a_menu_draws_its_selected_item() {
    let source = program(
        r#"
        let bar: u64 = std::graphics::pack_rect(0u32, 0u32, 12u32, 6u32);
        let menu: u64 = gui::pack_menu(3u32, 1u32);
        if gui::menu_items(menu) != 3u32 { return 1; }
        if gui::menu_selected(menu) != 1u32 { return 2; }
        if gui::menu_selected(gui::menu_select(menu, 2u32)) != 2u32 { return 3; }
        if gui::menu_items(gui::menu_select(menu, 2u32)) != 3u32 { return 4; }
        if !gui::draw_menu(
            canvas.as_mut_slice(), size, bar, menu, std::graphics::black()
        ) { return 5; }
        "#,
    );
    let (mut kernel, exit) = run(&source, &[]);
    assert_eq!(exit, Some(0), "the menu's state round-tripped");

    let frame = presented(&mut kernel);
    // A, R, G, B from offset zero, so the alpha byte comes first: an opaque
    // colour starts 255 and a transparent one starts 0.
    let mark = [255, 64, 96, 160];
    let bar_colour = [255, 0, 0, 0];
    assert_eq!(
        pixel(&frame, 0, 0),
        bar_colour,
        "the first item is not the selected one, so it is bare bar"
    );
    assert_eq!(
        pixel(&frame, 4, 0),
        mark,
        "the second item is selected, so it is marked"
    );
    assert_eq!(pixel(&frame, 8, 0), bar_colour, "and the third is not");
}

/// Editing a field is three rules, and each is stated here.
///
/// Append, refuse-when-full, and remove-the-last. The test drives all three
/// through the real functions and checks the resulting bytes, because a text
/// field that is one character short or one too long is a bug a person finds by
/// typing and a return value would not have shown.
#[test]
fn a_text_field_edits_by_three_rules() {
    let source = program(
        r#"
        let mut field: [u8; 4] = [0u8; 4];
        let mut length: [u8; 8] = [0u8; 8];

        // Append: 'a' then 'b', and the bytes are in that order.
        if !gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 97u32) {
            return 1;
        }
        if !gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 98u32) {
            return 2;
        }
        if field[0] != 97u8 { return 3; }
        if field[1] != 98u8 { return 4; }

        // Full: a third character does not fit in four bytes, so it is refused
        // rather than written past the end.
        if !gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 99u32) {
            return 5;
        }
        if !gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 100u32) {
            return 6;
        }
        if gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 101u32) {
            return 7;
        }

        // A code with no glyph is refused, because a field that swallowed a
        // keystroke the user can never see is worse than one that refuses it.
        if gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 7u32) {
            return 8;
        }

        // Remove the last character, all the way down, and then refuse to remove
        // another. Four characters went in, so four removals succeed and the
        // fifth has nothing left to remove — which is the case a field gets
        // wrong by being off by one in either direction.
        if !gui::text_input_backspace(field.as_mut_slice(), length.as_mut_slice()) {
            return 9;
        }
        if !gui::text_input_backspace(field.as_mut_slice(), length.as_mut_slice()) {
            return 10;
        }
        if !gui::text_input_backspace(field.as_mut_slice(), length.as_mut_slice()) {
            return 11;
        }
        if !gui::text_input_backspace(field.as_mut_slice(), length.as_mut_slice()) {
            return 12;
        }
        if gui::text_input_backspace(field.as_mut_slice(), length.as_mut_slice()) {
            return 13;
        }

        // An emptied field reads back as no text at all.
        let mut empty_ok: bool = true;
        let empty: &str = gui::text_input_text(field.as_slice(), length.as_mut_slice(), empty_ok);
        if std::text::len(empty) != 0u64 { return 14; }

        // And a field used again holds exactly the one character put in it.
        if !gui::text_input_insert(field.as_mut_slice(), length.as_mut_slice(), 97u32) {
            return 15;
        }
        let mut ok: bool = true;
        let text: &str = gui::text_input_text(field.as_slice(), length.as_mut_slice(), ok);
        if std::text::len(text) != 1u64 { return 16; }
        if std::text::byte_at(text, 0u64) != 97u8 { return 17; }

        if !gui::draw_text_input(
            canvas.as_mut_slice(), size,
            std::graphics::pack_rect(2u32, 2u32, 16u32, 10u32),
            text
        ) { return 18; }
        if !gui::draw_caret(
            canvas.as_mut_slice(), size, std::graphics::pack_point(10u32, 5u32)
        ) { return 19; }
        "#,
    );
    let (mut kernel, exit) = run(&source, &[]);
    assert_eq!(exit, Some(0), "the field followed all three editing rules");

    let frame = presented(&mut kernel);
    // The field's face is black, so the rectangle is distinguishable from the
    // untouched canvas around it.
    assert_eq!(
        pixel(&frame, 2, 2),
        [255, 0, 0, 0],
        "the field drew its face"
    );
    // The text. The field is at (2, 2) and 16 by 10, so `button_label_at`
    // centres a one-character label at (6, 3) and `draw_text_input` puts it one
    // pixel further right, at (7, 3). The glyph cell is eight wide and starts
    // there.
    //
    // 'a' has row 2 as 0b00111110, and `glyph_pixel` counts columns from the
    // *high* bit, so that row lights columns two through six: (9, 5) through
    // (13, 5). Its widest row is row 4, 0b01111111, which lights columns one
    // through seven and so reaches (14, 7) — the rightmost pixel the label can
    // draw. Reading that pixel and the one past it pins the glyph's extent, and
    // reading (9, 3) says the cell started on the row the geometry said rather
    // than one above it.
    assert_eq!(
        pixel(&frame, 9, 5),
        [255, 255, 255, 255],
        "the field drew its text where the geometry said"
    );
    assert_eq!(
        pixel(&frame, 14, 7),
        [255, 255, 255, 255],
        "and the glyph's rightmost column is where the font puts it"
    );
    assert_eq!(
        pixel(&frame, 15, 7),
        [255, 0, 0, 0],
        "with nothing one pixel beyond it"
    );
    assert_eq!(
        pixel(&frame, 9, 3),
        [255, 0, 0, 0],
        "and the cell starting on the row the geometry said, not above it"
    );
    // The caret, at (10, 5) and six tall, so it covers rows 5 to 10 and not 4 or
    // 11. Both of those are inside the field, whose face is opaque black, so
    // they are black because nothing was drawn over them — which is exactly what
    // "the caret is six tall" has to mean for a caret that sits on a field.
    assert_eq!(
        pixel(&frame, 10, 5),
        [255, 255, 255, 255],
        "and the caret starts where the program asked for it"
    );
    assert_eq!(
        pixel(&frame, 10, 10),
        [255, 255, 255, 255],
        "and runs for six rows"
    );
    assert_eq!(
        pixel(&frame, 10, 4),
        [255, 0, 0, 0],
        "with nothing one row above it"
    );
    assert_eq!(
        pixel(&frame, 10, 11),
        [255, 0, 0, 0],
        "and nothing one row below it"
    );
}

/// A canvas blits a source and clips at the source's edge.
///
/// The program makes a 4-by-4 source with one white pixel in it, then draws it
/// into the window at two origins: one at the origin, and one past the source's
/// bottom-right corner. The second case is the interesting one — a blit that read
/// past the source would draw a pixel that is not the source's, and the test
/// checks that nothing was drawn at all.
#[test]
fn a_canvas_blits_a_source_and_clips_at_its_edge() {
    let source = program(
        r#"
        // A 4-by-4 picture with one white pixel and one opaque black pixel in
        // it. The black one matters: a source of nothing but transparent pixels
        // would look identical whether the blit copied them or left the
        // destination alone.
        let mut picture: [u8; 64] = [0u8; 64];
        std::graphics::put_pixel(
            picture.as_mut_slice(), 4u32, 4u32,
            std::graphics::pack_point(1u32, 1u32), std::graphics::white()
        );
        std::graphics::put_pixel(
            picture.as_mut_slice(), 4u32, 4u32,
            std::graphics::pack_point(3u32, 3u32), std::graphics::black()
        );
        // A 4-by-4 view of the whole picture.
        if !gui::draw_canvas(
            canvas.as_mut_slice(), size, picture.as_slice(), gui::pack_view(0u32, 0u32, 4u32)
        ) { return 1; }
        // A view that starts past the source's bottom-right corner: everything is
        // clipped, so nothing is drawn and the answer is false.
        if gui::draw_canvas(
            canvas.as_mut_slice(), size, picture.as_slice(), gui::pack_view(9u32, 9u32, 4u32)
        ) { return 2; }
        // A zero-width source is refused rather than guessed at.
        if gui::draw_canvas(
            canvas.as_mut_slice(), size, picture.as_slice(), gui::pack_view(0u32, 0u32, 0u32)
        ) { return 3; }
        // A source whose length is not a whole number of pixels at that width
        // is refused too: a blit from a source whose geometry the caller got
        // wrong would read the wrong bytes and draw them.
        if gui::draw_canvas(
            canvas.as_mut_slice(), size, picture.as_slice(), gui::pack_view(0u32, 0u32, 7u32)
        ) { return 4; }
        "#,
    );
    let (mut kernel, exit) = run(&source, &[]);
    assert_eq!(
        exit,
        Some(0),
        "the blit drew and all three refusals happened"
    );

    let frame = presented(&mut kernel);
    assert_eq!(
        pixel(&frame, 1, 1),
        [255, 255, 255, 255],
        "the source's white pixel arrived at the same place"
    );
    assert_eq!(
        pixel(&frame, 3, 3),
        [255, 0, 0, 0],
        "and so did its opaque black one"
    );
    assert_eq!(
        pixel(&frame, 9, 9),
        [0, 0, 0, 0],
        "the clipped blit wrote nothing"
    );
}
