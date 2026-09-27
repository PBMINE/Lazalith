//! The frontend's panels, checked against a real machine.
//!
//! Every test here runs a real program on a real machine through the real debug
//! API, builds a [`View`], and checks what the frontend *would* draw. Nothing is
//! stubbed and no window is opened, which is the reason the view is separate from
//! the window: a frontend whose only test needs a display is a frontend whose
//! tests do not get run.
//!
//! What each test states is one thing the frontend claims:
//!
//! - every panel the roadmap lists is present, in the order it lists them;
//! - the register panel says what the machine's registers are;
//! - the program counter panel says where the program is *in the source*, and
//!   says so honestly when the image has no debug information;
//! - the screen panel shows the guest's own pixels, read through the API, and
//!   explains itself when there is no screen;
//! - a read that fails becomes a line on the panel that wanted it and a
//!   structured diagnostic, not a failed window;
//! - the stack panel says plainly that it is not a call chain;
//! - the diagnostics panel shows structure, never a parsed string.

use lazalith_debug::{DebugController, ExecutionState, StopReason};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_gui::font;
use lazalith_gui::view::{self, DiagnosticKind, Diagnostics, Panel, View, ViewOptions};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program with a loop and a `print`, so there is something to see on every
/// panel at once: registers that change, a stack with words on it, console
/// output, and an address in the middle of real code.
const SOURCE: &str = r#"fn main() -> i32 {
    let mut index: i64 = 0i64;
    let mut total: i64 = 0i64;
    while index < 6i64 {
        total = total + index;
        index = index + 1i64;
    }
    rt::sys::print("frontend\n");
    return 0;
}
"#;

/// A program that opens a display and draws into it.
///
/// It clears to one colour, puts a different one at the origin, and presents —
/// which is the minimum for the screen panel to have anything real to show, and
/// enough that the panel's bytes can be checked against the guest's own calls.
/// A program that opens a display, draws into it, presents, and then keeps
/// running.
///
/// It keeps running on purpose. A program that presented and exited would have
/// its memory torn down, and a debugger showing "the last frame" would then be
/// showing zeroes — which is why the screen is read while the process is alive,
/// and which is also what a person looking at a running program actually does.
const DRAWING: &str = r#"fn main() -> i32 {
    let mut framebuffer: [u8; 1024] = [0u8; 1024];
    let mut record_words: [u64; 3] = [0u64, 0u64, 0u64];
    let mut record: &mut [u8] = rt::memory::slice_mut(
        record_words.as_mut_slice().as_ptr() as u64,
        std::graphics::record_bytes()
    );
    // The `open` record is written straight into the guest's memory, so it has to
    // be word-aligned and a `[u8; 24]` is not; the `present` result is not, because
    // the SDK reads that one through its own word-backed buffer first. Both facts
    // are the documented alignment rule, and getting either wrong looks like a
    // display that silently never presents.
    let mut count: [u8; 8] = [0u8; 8];
    if std::graphics::open(16u32, 16u32, framebuffer.as_mut_slice(), record) {
        std::graphics::clear(framebuffer.as_mut_slice(), std::graphics::rgba(20u8, 30u8, 40u8, 255u8));
        std::graphics::put_pixel(framebuffer.as_mut_slice(), 16u32, 16u32, std::graphics::pack_point(0u32, 0u32), std::graphics::rgba(255u8, 0u8, 0u8, 255u8));
        std::graphics::present(framebuffer.as_mut_slice(), count.as_mut_slice());
    }
    let mut spin: i64 = 0i64;
    while spin < 1000000i64 {
        spin = spin + 1i64;
    }
    return 0;
}
"#;

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

fn controller(source: &str) -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
    let mut controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    controller
        .load_image(build(source), PID, TID)
        .expect("the program is scheduled");
    controller
}

/// The view of `source`'s program, with nothing run.
fn view_of(source: &str) -> (DebugController<NoDevice>, View, Diagnostics) {
    let controller = controller(source);
    let mut diagnostics = Diagnostics::new();
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    (controller, view, diagnostics)
}

fn text_of(view: &View, panel: Panel) -> String {
    view.section(panel)
        .lines
        .iter()
        .map(|line| format!("{} {}", line.label, line.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every panel the roadmap lists is present, in the order it lists them.
#[test]
fn the_view_has_every_panel_the_roadmap_lists() {
    let (_controller, view, _diagnostics) = view_of(SOURCE);
    let panels: Vec<Panel> = view.sections.iter().map(|section| section.panel).collect();
    assert_eq!(
        panels,
        Panel::ALL.to_vec(),
        "the ten panels, in the roadmap's order: screen, registers, pc, flags, \
         disassembly, memory, stack, console, processes, diagnostics"
    );
}

/// The register panel says what the machine's registers are.
#[test]
fn the_register_panel_says_what_the_registers_are() {
    let (controller, view, _diagnostics) = view_of(SOURCE);
    let registers = controller.registers();
    let text = text_of(&view, Panel::Registers);
    for value in registers.general() {
        let expected = format!("r{} {:#018x}", value.index, value.value);
        assert!(
            text.contains(&expected),
            "the register panel says {expected}, and it is what the machine's \
             snapshot says"
        );
    }
    assert!(
        text.contains(&format!("sp {:#018x}", registers.sp())),
        "and it shows the stack pointer"
    );
}

/// The program counter panel names the line the program is stopped on.
#[test]
fn the_program_counter_panel_names_the_line_it_is_on() {
    let (mut controller, _view, mut diagnostics) = view_of(SOURCE);
    // Run to a breakpoint on the loop body, so the program is stopped somewhere
    // known rather than at the entry.
    controller
        .set_source_breakpoint(PID, "main.lz", 5)
        .expect("the line has code");
    let run = controller.run(PID).expect("the program runs");
    assert!(
        matches!(run.reason, StopReason::Breakpoint { .. }),
        "stopped on a breakpoint: {:?}",
        run.reason
    );
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let text = text_of(&view, Panel::ProgramCounter);
    assert!(
        text.contains(&format!("pc {:#018x}", controller.registers().pc())),
        "the panel shows the program counter: {text}"
    );
    assert!(
        text.contains("source main.lz:5:"),
        "and names the line the program is stopped on, from the image's own \
         debug information: {text}"
    );
}

/// An image with no debug information is said so, not guessed at.
#[test]
fn a_program_without_debug_information_says_so() {
    let (_controller, _view, _diagnostics) = view_of(SOURCE);
    // Strip the block the only way a caller could, and reload.
    let with_debug = build(SOURCE);
    let stripped = LzxImage::new(
        with_debug.architecture(),
        0,
        // The entry offset is the linked image's own, not zero: the runtime links
        // the program after the library, so offset zero is a library function and
        // an image built with it runs something other than the program under test.
        with_debug.entry_offset(),
        with_debug.required_data(),
        with_debug.required_stack(),
        with_debug.sections().to_vec(),
    )
    .expect("an image without debug information is valid");
    let mut fresh = DebugController::boot(
        ArchitectureConfig::lz64(),
        &supervisor_kernel(ArchitectureConfig::lz64()),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    fresh
        .load_image(stripped, PID, TID)
        .expect("the program is scheduled");
    let mut diagnostics = Diagnostics::new();
    let view = view::build(&fresh, PID, TID, &ViewOptions::default(), &mut diagnostics)
        .expect("the view builds");
    let text = text_of(&view, Panel::ProgramCounter);
    assert!(
        text.contains("the image carries no debug information"),
        "the panel says why there is no source rather than showing a blank: {text}"
    );
    // And the disassembly panel still works: losing source information must not
    // lose the instruction text.
    let disassembly = text_of(&view, Panel::Disassembly);
    assert!(
        disassembly.lines().count() > 1,
        "the disassembly is there regardless, with no source comment to lose: \
         {disassembly}"
    );
    assert!(
        disassembly.lines().all(|line| !line.contains("main.lz:")),
        "and it says nothing about a source the image does not carry: {disassembly}"
    );
}

/// The disassembly panel shows the instruction at the program counter.
#[test]
fn the_disassembly_panel_shows_the_current_instruction() {
    let (mut controller, _view, _diagnostics) = view_of(SOURCE);
    // The handoff has to happen first: before it, the program counter is the
    // supervisor's own `RFE`, which is not code the image has any source for.
    controller.step(PID).expect("the handoff runs");
    let mut diagnostics = Diagnostics::new();
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let pc = controller.registers().pc();
    let section = view.section(Panel::Disassembly);
    let first = section.lines.first().expect("a line of disassembly");
    assert!(
        first.label == format!("{pc:#010x}"),
        "the first line is the instruction at the program counter, which is \
         {pc:#010x} and not {}",
        first.label
    );
    assert!(
        matches!(first.emphasis, view::Emphasis::Current),
        "and it is drawn as the one the program is stopped on"
    );
    assert!(
        first.text.contains("main.lz"),
        "and it names the source line it came from: {}",
        first.text
    );
}

/// The memory panel shows the bytes that are there.
#[test]
fn the_memory_panel_shows_real_bytes() {
    let (_controller, view, _diagnostics) = view_of(SOURCE);
    let text = text_of(&view, Panel::Memory);
    // The code section starts at zero, so the first line is the first instructions
    // the program will run, and a hex dump of them is not all zeros.
    let first = text.lines().next().expect("a memory line");
    assert!(
        first.starts_with("0x00000000 "),
        "the panel starts at the address it was asked for: {first}"
    );
    assert!(
        !first.ends_with("    "),
        "and it shows the bytes as text as well as hex: {first}"
    );
}

/// The stack panel says it is not a call chain.
#[test]
fn the_stack_panel_says_it_is_not_a_call_chain() {
    let (mut controller, _view, mut diagnostics) = view_of(SOURCE);
    controller.step(PID).expect("the handoff runs");
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let text = text_of(&view, Panel::Stack);
    assert!(
        text.contains("not frames"),
        "the panel says its words are not frames, because the calling \
         convention has no frame pointer to walk: {text}"
    );
    assert!(
        text.contains(&format!("sp {:#018x}", controller.registers().sp())),
        "and it shows the stack pointer: {text}"
    );
}

/// The console panel shows what the program printed.
#[test]
fn the_console_panel_shows_what_the_program_printed() {
    let (mut controller, _view, _diagnostics) = view_of(SOURCE);
    controller.run(PID).expect("the program runs to its end");
    let mut diagnostics = Diagnostics::new();
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds even after the program exited");
    let text = text_of(&view, Panel::Console);
    assert!(
        text.contains("frontend"),
        "the console panel shows the program's own output: {text}"
    );
}

/// The screen panel shows the guest's pixels, read through the API.
#[test]
fn the_screen_panel_shows_the_guests_pixels() {
    let (mut controller, view, mut diagnostics) = view_of(DRAWING);
    // A window that has been opened is not a frame, so the panel says so until
    // the guest presents one.
    let before = text_of(&view, Panel::Screen);
    assert!(
        before.contains("has not presented a frame yet") || before.contains("not opened"),
        "before a frame exists the panel says so: {before}"
    );
    // Stop after the present rather than running to the end. A program that ran to
    // its exit would have its memory torn down and the frame would read as zeroes,
    // and a program that ran to a step limit would need a limit large enough to be
    // sure the present had happened — which is a number a test should not have to
    // guess. A breakpoint on the line after the present says exactly "stop once
    // the picture is on the device", which is what a person does too.
    let loop_line = line_of(DRAWING, "while spin").expect("the program has a spin loop");
    let addresses = controller
        .set_source_breakpoint(PID, "main.lz", loop_line)
        .expect("the loop line has code");
    assert!(
        !addresses.is_empty(),
        "the loop line resolved to an address, so the breakpoint is real"
    );
    let run = controller.run(PID).expect("the program runs");
    assert!(
        matches!(run.reason, StopReason::Breakpoint { .. }),
        "the program is stopped at the breakpoint, still alive: {:?}",
        run.reason
    );
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    assert!(
        view.screen.is_present(),
        "after the guest presented, the screen is there: {:?}",
        view.screen.unavailable
    );
    assert_eq!(view.screen.width, 16);
    assert_eq!(view.screen.height, 16);
    assert_eq!(
        view.screen.pixels.len(),
        16 * 16 * 4,
        "and the framebuffer is exactly the bytes the guest wrote"
    );
    // The guest cleared to 20,30,40 and put one red pixel at the origin, so the
    // first pixel is red and a later one is the clear colour. This is the guest's
    // arithmetic, not a value the frontend invented.
    assert_eq!(
        &view.screen.pixels[0..4],
        &[255u8, 0, 0, 255],
        "the guest put red at the origin, and the panel would draw red there: the
         conversion from the guest's byte order to the window's is what makes
         that true"
    );
    assert_eq!(
        &view.screen.pixels[4 * 4..4 * 4 + 4],
        &[20u8, 30, 40, 255],
        "and the one after it is the colour the guest cleared to, in the guest's
         own channel order"
    );
}

/// A program with no screen is explained, not shown as an empty panel.
#[test]
fn a_program_with_no_screen_is_explained() {
    let (mut controller, _view, mut diagnostics) = view_of(SOURCE);
    controller.run(PID).expect("the program runs to its end");
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    assert!(
        !view.screen.is_present(),
        "a program that never opened a display has no screen"
    );
    let text = text_of(&view, Panel::Screen);
    assert!(
        text.contains("exited") || text.contains("not opened"),
        "and the panel says why rather than showing nothing: {text}"
    );
}

/// The processes panel lists the loaded process and its state.
#[test]
fn the_processes_panel_lists_the_process() {
    let (mut controller, _view, mut diagnostics) = view_of(SOURCE);
    controller.run(PID).expect("the program runs to its end");
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let text = text_of(&view, Panel::Processes);
    assert!(
        text.contains("pid=1"),
        "the panel names the process: {text}"
    );
    assert!(
        text.contains("exited"),
        "and its state, which the controller reports: {text}"
    );
    assert!(
        controller
            .session(PID)
            .is_some_and(|session| session.state() == ExecutionState::Exited { code: 0 }),
        "and the state the panel showed is the state the session has"
    );
}

/// A read that fails becomes a line and a diagnostic, not a failed window.
#[test]
fn an_unreadable_panel_becomes_a_line_and_a_diagnostic() {
    let (_controller, _view, diagnostics) = view_of(SOURCE);
    // A memory panel pointed at an address outside the machine's memory cannot
    // be read. The view still builds, because a debugger that refuses to show
    // the registers because one address was wrong is not a debugger.
    let mut diagnostics = diagnostics;
    let options = ViewOptions {
        memory_address: 0x7fff_ffff_ffff_0000,
        ..ViewOptions::default()
    };
    let controller = controller(SOURCE);
    let view = view::build(&controller, PID, TID, &options, &mut diagnostics)
        .expect("the view builds even though one panel could not be read");
    let text = text_of(&view, Panel::Memory);
    assert!(
        text.contains("unreadable"),
        "the memory panel says it could not read that address: {text}"
    );
    let diagnostic = diagnostics
        .entries()
        .iter()
        .find(|entry| entry.code == "gui-memory-unreadable")
        .expect("and a diagnostic says which panel and why");
    assert_eq!(diagnostic.kind, DiagnosticKind::Frontend);
    // The other panels are unaffected.
    assert!(
        text_of(&view, Panel::Registers).contains("r0 "),
        "and the register panel is still there: a failure in one panel is not \
         a failure of the view"
    );
    let _ = view;
}

/// The diagnostics panel shows structure, not a parsed string.
#[test]
fn the_diagnostics_panel_shows_structured_diagnostics() {
    let (_controller, view, _diagnostics) = view_of(SOURCE);
    let text = text_of(&view, Panel::Diagnostics);
    assert!(
        text.contains("nothing has gone wrong"),
        "with nothing wrong, the panel says so: {text}"
    );

    // A diagnostic built from its parts renders each part, and the panel's output
    // contains the *values* rather than a message someone would have to parse.
    let mut diagnostics = Diagnostics::new();
    diagnostics.push(
        view::Diagnostic::new(
            DiagnosticKind::GuestFault,
            "guest-trap",
            "the program trapped",
        )
        .at(0x1000)
        .in_source(view::SourcePlace::new("main.lz", 4, 9))
        .executing("TRAP r0"),
    );
    let controller = controller(SOURCE);
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let text = text_of(&view, Panel::Diagnostics);
    assert!(text.contains("[guest-trap]"), "the code is shown: {text}");
    assert!(
        text.contains("guest fault: the program trapped"),
        "and the kind, so a guest fault is not mistaken for an emulator bug: {text}"
    );
    assert!(text.contains("main.lz:4:9"), "and the source place: {text}");
    assert!(text.contains("pc=0x1000"), "and the guest's pc: {text}");
    assert!(text.contains("TRAP r0"), "and the instruction: {text}");
}

/// A guest fault and an emulator bug are shown differently.
#[test]
fn a_guest_fault_and_an_emulator_bug_are_distinguished() {
    let controller = controller(SOURCE);
    let mut diagnostics = Diagnostics::new();
    diagnostics.push(view::Diagnostic::new(
        DiagnosticKind::GuestFault,
        "guest-trap",
        "the program trapped",
    ));
    diagnostics.push(view::Diagnostic::new(
        DiagnosticKind::EmulatorBug,
        "cpu-invariant",
        "the program counter left the code section",
    ));
    let view = view::build(
        &controller,
        PID,
        TID,
        &ViewOptions::default(),
        &mut diagnostics,
    )
    .expect("the view builds");
    let section = view.section(Panel::Diagnostics);
    let fault = section
        .lines
        .iter()
        .find(|line| line.label == "[guest-trap]")
        .expect("the guest fault is listed");
    let bug = section
        .lines
        .iter()
        .find(|line| line.label == "[cpu-invariant]")
        .expect("the emulator bug is listed");
    assert_eq!(fault.emphasis, view::Emphasis::Fault);
    assert_eq!(
        bug.emphasis,
        view::Emphasis::Fault,
        "both are drawn as faults, because both are things to look at — but \
         their text says which is which"
    );
    assert!(fault.text.contains("guest fault"));
    assert!(bug.text.contains("emulator bug"));
}

/// The font draws the characters a debugger needs.
#[test]
fn the_font_draws_hex_digits_and_letters() {
    for character in "0123456789abcdefABCDEF:.#;/-_()[]{}<>=".chars() {
        assert!(
            font::glyph(character).is_some_and(|columns| columns.iter().any(|column| *column != 0)),
            "{character:?} has a glyph with something in it"
        );
    }
    assert!(
        font::glyph('é').is_none(),
        "a character with no glyph says so rather than drawing nothing"
    );
    // A space is blank, and a missing character draws as a visible box.
    assert_eq!(font::glyph(' '), Some(&[0, 0, 0, 0, 0]));
    assert!(
        font::text_pixel("\u{fffd}", 0, 0),
        "a character with no glyph draws something visible"
    );
    assert_eq!(font::text_width("abc"), 3 * 6 - 1);
    assert_eq!(font::text_width(""), 0);
}

/// The font's widths agree with what it draws.
#[test]
fn the_font_measures_what_it_draws() {
    let text = "r0 = 00000000";
    let width = font::text_width(text);
    // Nothing is drawn at or past the width the font reports, which is what lets
    // a caller place one string after another without measuring the glyphs.
    for x in width..width + 8 {
        for y in 0..font::GLYPH_HEIGHT {
            assert!(
                !font::text_pixel(text, x, y),
                "nothing is drawn at x={x}, which is at or past the width {width}"
            );
        }
    }
    // And nothing is drawn below the glyph's height.
    for y in font::GLYPH_HEIGHT..font::GLYPH_HEIGHT + 4 {
        assert!(
            !(0..width).any(|x| font::text_pixel(text, x, y)),
            "nothing is drawn at y={y}, which is below the glyph"
        );
    }
    // And there is something drawn inside it, so the checks above are not passing
    // because the function always says no.
    assert!(
        (0..width).any(|x| (0..font::GLYPH_HEIGHT).any(|y| font::text_pixel(text, x, y))),
        "the string has ink in it, so the bounds above are real bounds"
    );
}

/// The one-based line number of the first line of `source` containing `needle`.
///
/// A test that wrote a line number down would break the day someone reformatted
/// the program above it, and would break for a reason that has nothing to do with
/// the frontend.
fn line_of(source: &str, needle: &str) -> Option<u32> {
    source
        .lines()
        .position(|line| line.contains(needle))
        .and_then(|index| u32::try_from(index + 1).ok())
}
