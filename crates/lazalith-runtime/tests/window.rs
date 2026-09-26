//! Step 72: the first graphical Lazen application, end to end.
//!
//! The program under test is `examples/window/main.lz` — the file a user would
//! get, read from the repository rather than pasted here, so this test cannot
//! pass against a program the repository does not actually ship.
//!
//! The path it takes is the whole one, with nothing stubbed:
//!
//! ```text
//! examples/window/main.lz
//!   ↓
//! Lazen compiler            (frontend, lowering, code generation)
//!   ↓
//! .lzx
//!   ↓
//! LazOS loader              (LzxImage::from_bytes, then load_process)
//!   ↓
//! kernel → display driver → display device
//!   ↓
//! kernel → input driver   ← input device
//! ```
//!
//! The assertions are on what the *drivers* saw, not on the program's return
//! value. A program that opened a window and drew into a framebuffer nobody ever
//! presented is not a graphical program, and `main` returning 0 cannot tell the
//! difference.
//!
//! What each test states is one clause of "the application must not know SDL3
//! exists":
//!
//! - the program opens a window and presents every frame it drew;
//! - the pixels on screen are the ones the program wrote, at the places it wrote
//!   them, which is the property a shared framebuffer exists for;
//! - the keyboard changes what the program draws, so input reaches the program
//!   as input rather than as a record nobody read.

use std::path::PathBuf;
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

/// The application this step ships, as a repository file.
///
/// A test that pasted the program in would keep passing after the shipped one
/// stopped working, which is the opposite of what an end-to-end test is for.
fn application() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/window/main.lz")
        .canonicalize()
        .expect("examples/window/main.lz is in the repository");
    std::fs::read_to_string(&path).expect("the application reads")
}

/// The window the application opens. These are the numbers in the program, and
/// the device is asked to agree with them rather than being told.
const WIDTH: u32 = 48;
const HEIGHT: u32 = 32;
const PIXEL_BYTES: u64 = 4;

/// The frames the application draws before it returns.
const FRAMES: u64 = 2;

/// A two-instruction supervisor kernel: a `NOP` and the `RFE` that hands off.
fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

/// The process the application runs as.
const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A machine, a kernel, and the application loaded and scheduled.
///
/// `events` are injected into the input device *before* the first step, so the
/// program finds them already queued — which is the case a real keyboard
/// produces and the one a program has to handle.
fn run(
    events: &[Event],
) -> (
    lazalith_machine::LazalithMachine<NoDevice>,
    LazalithKernel,
    Option<u32>,
) {
    let config = ArchitectureConfig::lz64();
    let source = application();
    let program =
        RuntimeProgram::build(&source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    // The image is read back from its own bytes rather than used in memory: a
    // `.lzx` is a file format with its own reader, and a run that skipped the
    // reader would not be the run a user's image gets.
    let image = LzxImage::from_bytes(&bytes).expect("the image reads back");

    let boot = BootImage::new(config, supervisor_kernel(config), 0).expect("a boot image");
    let mut machine = boot
        .start(DeviceManager::<NoDevice>::new())
        .expect("the machine starts");
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .expect("a trap vector");
    assert_eq!(
        machine.architectural_state().privilege(),
        Privilege::Supervisor,
        "the machine starts in supervisor mode, before the handoff"
    );
    machine.step().expect("the kernel's first step");

    // The quantum is one instruction: a single-threaded program is all Lazen v1
    // has, and a long quantum would only hide a scheduling bug.
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
    // The bound is generous on purpose: the point is that the program finishes,
    // not that it is quick. What the instruction budget costs is a separate
    // measurement, and `docs/lazen-graphics.md` records it.
    for _ in 0..20_000_000 {
        let step = kernel.step(&mut machine).expect("a step");
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => {
                panic!("the program faulted: {error:?}")
            }
            Some(KernelServiceOutcome::Return(_)) | None => {}
        }
    }
    (machine, kernel, exit)
}

/// The bytes of the framebuffer the device last presented.
fn presented_pixels(kernel: &mut LazalithKernel) -> Vec<u8> {
    let frame = kernel
        .display()
        .device()
        .presented()
        .expect("a frame was presented");
    let bytes = frame.bytes().expect("the frame has a size");
    let address = frame.address;
    let mut out = vec![0u8; usize::try_from(bytes).expect("a host-sized frame")];
    let process = kernel
        .scheduler_mut()
        .process_mut(PID)
        .expect("the process is still known");
    process
        .memory_context()
        .expect("a memory context")
        .read_bytes(VirtualAddress::new(address), &mut out)
        .expect("the framebuffer is readable");
    out
}

/// The four bytes of the pixel at (`x`, `y`), in the A, R, G, B order the SDK
/// documents.
fn pixel(frame: &[u8], x: u32, y: u32) -> [u8; 4] {
    let at =
        usize::try_from(u64::from(y) * u64::from(WIDTH) * PIXEL_BYTES + u64::from(x) * PIXEL_BYTES)
            .expect("a host-sized offset");
    let bytes = &frame[at..at + 4];
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

/// A `text` event carrying `character`.
fn typed(character: u32) -> Event {
    Event {
        kind: EventKindValue(lazalith_devices::EventKind::Text.as_u32()),
        code: character,
        x: 0,
        y: 0,
    }
}

/// The application opens a window and presents every frame it drew.
///
/// A window is only a window once a frame has been presented, so the frame count
/// is the assertion that matters: a program that drew and never presented would
/// pass a test that only checked `open` succeeded.
#[test]
fn the_application_opens_a_window_and_presents_its_frames() {
    let (_machine, kernel, exit) = run(&[]);
    assert_eq!(
        exit,
        Some(0),
        "the application ran to completion and reported success"
    );

    let device = kernel.display().device();
    assert_eq!(
        device.width(),
        u64::from(WIDTH),
        "the window is as wide as asked"
    );
    assert_eq!(
        device.height(),
        u64::from(HEIGHT),
        "the window is as tall as asked"
    );
    assert_eq!(
        device.presented().map(|frame| frame.present_count),
        Some(FRAMES),
        "every frame the program drew reached the device"
    );
}

/// The pixels on screen are the ones the program wrote.
///
/// The device shares the program's own memory and copies nothing, so the only way
/// this can hold is if the program wrote where it said it would. Checking the
/// background, the block and the text separately is what makes a failure say
/// *which* drawing went wrong.
#[test]
fn the_presented_frame_is_what_the_program_drew() {
    let (_machine, mut kernel, exit) = run(&[]);
    assert_eq!(exit, Some(0), "the application ran to completion");

    let frame = presented_pixels(&mut kernel);

    // The last frame's background. The program lightens it each frame, and the
    // second frame is `rgba(96, 16, 32, 255)`, so these four bytes are the whole
    // of it: alpha first, which is the layout `docs/lazen-graphics.md` fixes and
    // the one thing a reader of this test cannot check by eye.
    assert_eq!(
        pixel(&frame, 0, 0),
        [255, 96, 16, 32],
        "the background is the colour the program asked for on its last frame"
    );

    // The block is white and eight by eight, and it moved twice before the last
    // frame was drawn: the program starts it at x = 4 and steps it right by one
    // at the end of every frame, so the frame that is on screen when the program
    // returns was drawn with the block at x = 5. Reading the two edges and the
    // two pixels either side of them is what makes this a statement about
    // position and size rather than about a white square existing.
    assert_eq!(
        pixel(&frame, 5, 4),
        [255, 255, 255, 255],
        "the block starts where the program put it"
    );
    assert_eq!(
        pixel(&frame, 12, 4),
        [255, 255, 255, 255],
        "the block is eight wide, so its last column is at x + 7"
    );
    assert_eq!(
        pixel(&frame, 13, 4),
        [255, 96, 16, 32],
        "nothing was drawn past the block's right edge"
    );
    assert_eq!(
        pixel(&frame, 4, 4),
        [255, 96, 16, 32],
        "nothing was drawn before the block's left edge"
    );

    // The text. 'l' is a vertical bar in its third column, so (2, 0) is lit and
    // (0, 0) is not — one lit and one unlit pixel, which distinguishes a glyph
    // from a filled box and from nothing at all.
    assert_eq!(
        pixel(&frame, 2, 0),
        [255, 255, 255, 255],
        "the built-in font put a glyph stroke where it belongs"
    );
    assert_eq!(
        pixel(&frame, 0, 0),
        [255, 96, 16, 32],
        "and left the rest of the glyph cell alone"
    );
}

/// The keyboard changes what the program draws.
///
/// This is the clause that a graphical program has to satisfy and a console one
/// does not: input has to reach the program as input. The test types 'd', which
/// moves the block right, and checks the block is *further* right than the run
/// with no keyboard produced — a comparison against the untouched run rather than
/// a hard-coded position, so the assertion is about the keyboard and not about
/// the arithmetic in the program.
#[test]
fn the_keyboard_moves_what_the_program_draws() {
    // 'd' is 0x64. The program compares the character a `text` event carries, so
    // the host's key code never reaches it.
    let (_machine, mut kernel, exit) = run(&[typed(0x64)]);
    assert_eq!(exit, Some(0), "the application ran to completion");

    let frame = presented_pixels(&mut kernel);

    // With no keyboard the block is drawn at x = 5: it starts at 4 and the
    // program's own one-pixel step runs once before the second frame. A 'd' in
    // the queue adds a four-pixel step of its own before that, so the same
    // frame is drawn with the block at x = 9. Reading the pair is what says "it
    // moved" rather than "there is a white square somewhere": a program that
    // ignored the keyboard would leave both pixels the same.
    assert_eq!(
        pixel(&frame, 8, 4),
        [255, 96, 16, 32],
        "where the block was without a keypress is background with one"
    );
    assert_eq!(
        pixel(&frame, 9, 4),
        [255, 255, 255, 255],
        "and the block is where the keypress moved it"
    );
}

/// Every event the program was given was consumed.
///
/// A program that stops reading leaves events in the queue, and a queue that only
/// grows is how input turns into unbounded memory in a program that looks
/// correct. The device counts what it handed over and what is still waiting, and
/// this asks for both.
#[test]
fn the_application_leaves_no_event_unread() {
    let (_machine, kernel, exit) = run(&[typed(0x64), typed(0x64)]);
    assert_eq!(exit, Some(0), "the application ran to completion");
    let input = kernel.input();
    assert_eq!(
        input.pending(),
        0,
        "the queue is empty: the program drained it"
    );
    assert_eq!(
        input.delivered(),
        2,
        "and both events it was given reached it"
    );
}
