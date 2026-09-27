//! What the frontend shows, as values.
//!
//! # Why this is separate from the window
//!
//! Everything a debugger displays is decided here, and none of it needs a
//! display to decide. That is not tidiness for its own sake — it is what makes
//! the frontend testable at all. A test can run a real program on a real machine
//! through the real debug API, build a [`View`], and check that the register
//! panel says what the machine's registers are. Without this layer, the only way
//! to test the frontend is to open a window, which is exactly the kind of test
//! that gets skipped.
//!
//! So the split is: this module turns a [`DebugController`] into panels of
//! lines; [`crate::window`] turns panels of lines into pixels. Neither knows much
//! about the other, and both are checked.
//!
//! # What this is allowed to read
//!
//! Only the debug API. There is no `&mut LazalithMachine` here, no `&RegisterFile`,
//! and no way to reach one, because the controller does not hand them out. Every
//! number shown comes from one owned snapshot, so a panel cannot show a `pc` from
//! one moment and a `sp` from another.
//!
//! # What a read that fails looks like
//!
//! A panel that cannot read something says so on that panel and records a
//! [`Diagnostic`]. It does not fail the whole view, because a debugger that
//! refuses to show the registers because the stack is unreadable is not a
//! debugger — it is a window that says "error" instead of showing the program.

use std::fmt::Write as _;
use std::string::{String, ToString};
use std::vec::Vec;

use lazalith_debug::{DebugController, DebugError, Disassembly, ExecutionState, RegisterSnapshot};
use lazalith_devices::Device;
use lazalith_os::{ProcessId, ThreadId};

/// The panels the roadmap asks the frontend to display, in the order it lists
/// them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Panel {
    /// The machine's own display, as the guest drew it.
    Screen,
    /// Every general register.
    Registers,
    /// The program counter, and where it is in the source.
    ProgramCounter,
    /// The status register, one line per flag.
    Flags,
    /// The instructions around the program counter.
    Disassembly,
    /// A hex dump of memory.
    Memory,
    /// The stack, with return addresses resolved to lines.
    Stack,
    /// What the program has written to the terminal.
    Console,
    /// Every process the machine is running.
    Processes,
    /// Structured diagnostics.
    Diagnostics,
}

impl Panel {
    /// Every panel, in display order.
    pub const ALL: [Self; 10] = [
        Self::Screen,
        Self::Registers,
        Self::ProgramCounter,
        Self::Flags,
        Self::Disassembly,
        Self::Memory,
        Self::Stack,
        Self::Console,
        Self::Processes,
        Self::Diagnostics,
    ];

    /// The panel's title, as it is drawn.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::Registers => "registers",
            Self::ProgramCounter => "pc",
            Self::Flags => "flags",
            Self::Disassembly => "disassembly",
            Self::Memory => "memory",
            Self::Stack => "stack",
            Self::Console => "console",
            Self::Processes => "processes",
            Self::Diagnostics => "diagnostics",
        }
    }
}

/// How a line is drawn, which is all the styling a panel needs.
///
/// There is no colour here. Deciding what a colour *means* is the window's job,
/// and a panel that could pick its own would make the two palettes drift.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Emphasis {
    /// Ordinary text.
    Plain,
    /// The line the program is stopped on.
    Current,
    /// A line that already ran.
    Executed,
    /// Something wrong: a fault, a failure to read, a diagnostic.
    Fault,
    /// A breakpoint.
    Breakpoint,
}

impl Emphasis {
    /// The colour this emphasis is drawn in.
    ///
    /// Amber is the instruction about to execute, green is code already past, red
    /// is something wrong, and white is a breakpoint — which is a *marker* on an
    /// instruction rather than a state of it, so it does not compete with the
    /// colour that says where the program is.
    pub const fn color(self) -> lazalith_sdl3::Color {
        match self {
            Self::Plain => lazalith_sdl3::Color::WHITE,
            Self::Current => lazalith_sdl3::Color::AMBER,
            Self::Executed => lazalith_sdl3::Color::GREEN,
            Self::Fault => lazalith_sdl3::Color::RED,
            Self::Breakpoint => lazalith_sdl3::Color::BLUE,
        }
    }
}

/// One line of a panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    /// A left-hand label, which is a column of its own.
    pub label: String,
    /// The text.
    pub text: String,
    /// How to draw it.
    pub emphasis: Emphasis,
}

impl Line {
    /// A line with `label` and `text`.
    pub fn new(label: impl Into<String>, text: impl Into<String>, emphasis: Emphasis) -> Self {
        Self {
            label: label.into(),
            text: text.into(),
            emphasis,
        }
    }

    /// An ordinary line.
    pub fn plain(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(label, text, Emphasis::Plain)
    }

    /// The line the program is stopped on.
    pub fn current(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(label, text, Emphasis::Current)
    }

    /// A line that already ran.
    pub fn executed(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(label, text, Emphasis::Executed)
    }

    /// A line drawn as something wrong.
    pub fn fault(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(label, text, Emphasis::Fault)
    }

    /// A line drawn as a breakpoint.
    pub fn breakpoint(label: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(label, text, Emphasis::Breakpoint)
    }
}

/// A rectangle, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    /// A rectangle at `(x, y)` of `w` by `h`.
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// A panel's lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Section {
    /// Which panel this is.
    pub panel: Panel,
    /// Its title, as drawn.
    pub title: String,
    /// Its lines.
    pub lines: Vec<Line>,
}

/// The machine's screen, as pixels the window can upload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Screen {
    /// Its width in pixels, or zero when there is no screen.
    pub width: u32,
    /// Its height in pixels, or zero when there is no screen.
    pub height: u32,
    /// The framebuffer, four bytes per pixel.
    ///
    /// The guest's own format is already four bytes per pixel, so these bytes are
    /// the ones the guest wrote, read through the debug API like any other memory.
    /// A copy rather than a view because the machine keeps running, and a view
    /// would be a borrow of bytes that change under the drawer.
    pub pixels: Vec<u8>,
    /// Why there is no screen, if there is not one.
    pub unavailable: Option<String>,
}

impl Screen {
    /// Whether there is anything to draw.
    pub const fn is_present(&self) -> bool {
        self.unavailable.is_none() && self.width != 0 && self.height != 0
    }
}

/// Everything the frontend would draw about one process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct View {
    /// The process the panels describe.
    pub process: ProcessId,
    /// The thread within it.
    pub thread: ThreadId,
    /// The machine's screen.
    pub screen: Screen,
    /// The panels, in display order.
    pub sections: Vec<Section>,
}

/// The frontend's diagnostics, newest last.
///
/// A list with a cap, because a program in a loop that faults on every iteration
/// would otherwise grow it without bound and take the window down with it. The cap
/// keeps the *end*, which is where the recent ones are.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diagnostics {
    entries: Vec<Diagnostic>,
}

/// The most diagnostics kept.
pub const MAX_DIAGNOSTICS: usize = 200;

impl Diagnostics {
    /// An empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a diagnostic, dropping the oldest if the list is full.
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if self.entries.len() == MAX_DIAGNOSTICS {
            self.entries.remove(0);
        }
        self.entries.push(diagnostic);
    }

    /// Everything recorded, oldest first.
    pub fn entries(&self) -> &[Diagnostic] {
        &self.entries
    }

    /// How many there are.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// A diagnostic, as the frontend holds it.
///
/// Structured from the start rather than a string. A frontend that rendered an
/// error message and then had someone *read it back* to decide what to highlight
/// would highlight the wrong thing the day a message changes; every field here
/// is a value and a panel formats it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// What sort of thing this is.
    pub kind: DiagnosticKind,
    /// A stable identifier, so a frontend can filter on it and a test can assert
    /// on it without matching prose.
    pub code: String,
    /// The message.
    pub message: String,
    /// Where in the source it came from, if the image carried debug information.
    pub source: Option<SourcePlace>,
    /// The guest's program counter when it happened.
    pub guest_pc: Option<u64>,
    /// The guest's instruction there, as the toolchain formats it.
    pub instruction: Option<String>,
    /// Which machine, and what it was doing.
    pub machine: Option<String>,
}

impl Diagnostic {
    /// A diagnostic with only a kind, a code and a message.
    ///
    /// The other fields are filled in as they become known, which is how a
    /// frontend reports a problem it found itself: it knows what it was doing and
    /// not, yet, where the guest was.
    pub fn new(kind: DiagnosticKind, code: &str, message: impl Into<String>) -> Self {
        Self {
            kind,
            code: code.to_string(),
            message: message.into(),
            source: None,
            guest_pc: None,
            instruction: None,
            machine: None,
        }
    }

    /// Records where the guest was.
    #[must_use]
    pub fn at(mut self, pc: u64) -> Self {
        self.guest_pc = Some(pc);
        self
    }

    /// Records where in the source the guest was.
    #[must_use]
    pub fn in_source(mut self, source: SourcePlace) -> Self {
        self.source = Some(source);
        self
    }

    /// Records the instruction the guest was about to execute.
    #[must_use]
    pub fn executing(mut self, instruction: impl Into<String>) -> Self {
        self.instruction = Some(instruction.into());
        self
    }

    /// Records what the machine was doing.
    #[must_use]
    pub fn during(mut self, machine: impl Into<String>) -> Self {
        self.machine = Some(machine.into());
        self
    }
}

/// What sort of thing a diagnostic is.
///
/// A guest fault and an emulator bug are different in the only way that matters:
/// the first is the program's mistake and the second is ours, so a frontend that
/// showed them the same way would send someone to look in the wrong place.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    /// The guest program did something wrong: a fault, a trap, a bad address.
    GuestFault,
    /// The emulator found something that should not be possible.
    ///
    /// An invariant was violated or a value was out of range. This is *our* bug,
    /// and the diagnostic says which Rust file and line noticed.
    EmulatorBug,
    /// The frontend could not do what it was asked to.
    Frontend,
    /// The program reported a diagnostic of its own, such as a compile error in a
    /// source file someone is looking at.
    GuestReport,
}

impl DiagnosticKind {
    /// The label this kind is drawn under.
    pub const fn label(self) -> &'static str {
        match self {
            Self::GuestFault => "guest fault",
            Self::EmulatorBug => "emulator bug",
            Self::Frontend => "frontend",
            Self::GuestReport => "guest report",
        }
    }
}

/// A place in a source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePlace {
    /// The file, as the compiler was given it.
    pub name: String,
    /// The line, one-based.
    pub line: u32,
    /// The column, one-based.
    pub column: u32,
}

impl SourcePlace {
    /// A place in `name` at `line` and `column`.
    pub fn new(name: impl Into<String>, line: u32, column: u32) -> Self {
        Self {
            name: name.into(),
            line,
            column,
        }
    }
}

/// How much of each panel to build.
///
/// Values rather than constants in the code, so a test can ask for less and a
/// user can ask for more.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ViewOptions {
    /// How many instructions of disassembly to show.
    pub disassembly_lines: usize,
    /// How many bytes of memory to show, as eight per line.
    pub memory_lines: usize,
    /// How many stack words to show.
    pub stack_words: usize,
    /// How many console lines to show.
    pub console_lines: usize,
    /// Where the memory panel looks.
    pub memory_address: u64,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            disassembly_lines: 16,
            memory_lines: 8,
            stack_words: 8,
            console_lines: 12,
            memory_address: 0,
        }
    }
}

/// The most memory lines a view will show.
///
/// A panel that showed everything it could read would be a panel that filled the
/// window and left no room for the program. The cap is here rather than in the
/// window because it is a fact about what the panel means, not about how much
/// space there is.
pub const MAX_MEMORY_LINES: usize = 64;

/// Builds the view of `process` in `controller`.
///
/// Anything that could not be read is recorded in `diagnostics` and shown on the
/// panel that wanted it, so this returns a view rather than a `Result` for the
/// ordinary cases. The one thing it will not paper over is a controller that will
/// not describe the process at all, which is a frontend bug rather than a guest
/// one and so is reported as an error.
pub fn build<D: Device>(
    controller: &DebugController<D>,
    process: ProcessId,
    thread: ThreadId,
    options: &ViewOptions,
    diagnostics: &mut Diagnostics,
) -> Result<View, DebugError> {
    // The session is checked for here rather than in each panel, so that a view
    // of a process that is not loaded is one error rather than ten panels each
    // saying "unreadable".
    if controller.session(process).is_none() {
        return Err(DebugError::Snapshot(format!(
            "there is no session for process {}",
            process.get()
        )));
    }
    let registers = controller.registers();
    let screen = screen(controller, process, &registers, diagnostics);
    let sections = vec![
        screen_section(&screen),
        registers_section(&registers),
        program_counter_section(controller, &registers),
        flags_section(&registers),
        disassembly_section(controller, process, &registers, options, diagnostics),
        memory_section(controller, options, diagnostics),
        stack_section(controller, options, diagnostics),
        console_section(controller, options),
        processes_section(controller),
        diagnostics_section(diagnostics),
    ];
    Ok(View {
        process,
        thread,
        screen,
        sections,
    })
}

/// Reads the machine's screen through the debug API.
fn screen<D: Device>(
    controller: &DebugController<D>,
    process: ProcessId,
    registers: &RegisterSnapshot,
    diagnostics: &mut Diagnostics,
) -> Screen {
    let absent = |why: String| Screen {
        width: 0,
        height: 0,
        pixels: Vec::new(),
        unavailable: Some(why),
    };
    let Some(session) = controller.session(process) else {
        return absent(String::from("there is no such process"));
    };
    let display = controller.display();
    // The frame is asked for *before* the process state is. A program that has
    // exited still has the last picture it drew, and that is what a person wants
    // to see: the screen they were looking at when the program stopped is not
    // replaced by the word "exited". Only when there is no frame at all does the
    // state become the explanation.
    //
    // A window that has been opened is not a frame: until the guest presents,
    // there is nothing on its screen, and reporting a frame that was never drawn
    // would be reporting something the user never saw.
    let Some(frame) = display.presented() else {
        return absent(if display.width() == 0 {
            String::from("the guest has not opened a display")
        } else if matches!(session.state(), ExecutionState::Exited { .. }) {
            String::from("the program exited without presenting a frame")
        } else {
            String::from("the guest has not presented a frame yet")
        });
    };
    let (Ok(width), Ok(height)) = (u32::try_from(frame.width), u32::try_from(frame.height)) else {
        return absent(String::from(
            "the display is larger than this frontend can show",
        ));
    };
    let Some(byte_count) = frame.bytes().and_then(|count| usize::try_from(count).ok()) else {
        return absent(String::from("the display geometry overflows"));
    };
    match controller.read_memory(frame.address, byte_count) {
        Ok(guest) => Screen {
            width,
            height,
            // The conversion happens here, once, so the window uploads bytes
            // without knowing anything about a guest pixel format. A guest format
            // that changes is a change in one function rather than in every
            // drawing path.
            pixels: to_window_pixels(&guest),
            unavailable: None,
        },
        Err(error) => {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticKind::Frontend,
                    "gui-screen-unreadable",
                    format!(
                        "the framebuffer at {:#x} could not be read: {error}",
                        frame.address
                    ),
                )
                .at(registers.pc())
                .during(String::from("reading the machine's screen")),
            );
            absent(format!("the framebuffer could not be read: {error}"))
        }
    }
}

/// Converts a guest framebuffer into the byte order the window uploads.
///
/// The guest writes a pixel as alpha, red, green, blue — `write_pixel` in the SDK
/// puts alpha in byte zero — and SDL's `XRGB8888` wants red, green, blue and does
/// not read the fourth byte. Uploading the guest's bytes unchanged would swap two
/// channels of every pixel, and it would look *plausible*, because a test image
/// that is mostly grey survives a channel swap almost perfectly.
///
/// The conversion is here rather than in the window for two reasons: it happens
/// once per frame rather than once per draw call, and it is testable without a
/// display, which is how the test suite knows the channel order is right rather
/// than assuming it.
///
/// The guest's alpha byte is dropped rather than carried across, because `XRGB8888`
/// has no alpha channel and SDL documents the fourth byte as unused. A window with
/// a transparent background would make every pixel the host drew behind the
/// machine's screen show through, which is not what a debugger is for.
///
/// A trailing partial pixel is dropped rather than padded. The display's own
/// geometry decides how many bytes a frame is, so a partial one means the two
/// disagree, and inventing three more bytes would draw a pixel the guest never
/// wrote.
fn to_window_pixels(guest: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(guest.len());
    for pixel in guest.chunks_exact(4) {
        out.push(pixel[1]);
        out.push(pixel[2]);
        out.push(pixel[3]);
        out.push(u8::MAX);
    }
    out
}

fn screen_section(screen: &Screen) -> Section {
    let lines = match &screen.unavailable {
        Some(why) => vec![Line::plain("", why)],
        None => vec![
            Line::plain("", format!("{}x{}", screen.width, screen.height)),
            Line::plain("", format!("{} bytes, {} presents", screen.pixels.len(), 0)),
        ],
    };
    section(Panel::Screen, lines)
}

fn registers_section(registers: &RegisterSnapshot) -> Section {
    let mut lines = Vec::new();
    for value in registers.general() {
        lines.push(Line::plain(
            format!("r{}", value.index),
            format!("{:#018x}", value.value),
        ));
    }
    lines.push(Line::plain("sp", format!("{:#018x}", registers.sp())));
    section(Panel::Registers, lines)
}

fn program_counter_section<D: Device>(
    controller: &DebugController<D>,
    registers: &RegisterSnapshot,
) -> Section {
    let mut lines = vec![Line::current("pc", format!("{:#018x}", registers.pc()))];
    let source = match controller.source_location() {
        Some(place) => {
            lines.push(Line::plain(
                "source",
                format!(
                    "{}:{}:{}",
                    place.name,
                    place.line_number(),
                    place.column_number()
                ),
            ));
            Some(SourcePlace::new(
                place.name,
                place.line_number(),
                place.column_number(),
            ))
        }
        None => {
            lines.push(Line::plain(
                "source",
                if controller.debug_info().is_some() {
                    String::from("not inside any mapped source range")
                } else {
                    String::from("the image carries no debug information")
                },
            ));
            None
        }
    };
    lines.push(Line::plain(
        "privilege",
        format!("{:?}", registers.privilege()),
    ));
    let _ = source;
    section(Panel::ProgramCounter, lines)
}

fn flags_section(registers: &RegisterSnapshot) -> Section {
    let status = registers.status();
    let flag = |name: &str, value: bool| {
        if value {
            Line::current(name, "set")
        } else {
            Line::plain(name, "clear")
        }
    };
    section(
        Panel::Flags,
        vec![
            flag("negative", status.negative()),
            flag("zero", status.zero()),
            flag("carry", status.carry()),
            flag("overflow", status.overflow()),
            flag("interrupts", status.interrupts_enabled()),
        ],
    )
}

fn disassembly_section<D: Device>(
    controller: &DebugController<D>,
    process: ProcessId,
    registers: &RegisterSnapshot,
    options: &ViewOptions,
    diagnostics: &mut Diagnostics,
) -> Section {
    let pc = registers.pc();
    let count = options.disassembly_lines.clamp(1, 512);
    let session = controller.session(process);
    let breakpoint = |address: u64| session.is_some_and(|session| session.is_breakpoint(address));
    match controller.disassemble(pc, count) {
        Ok(instructions) => {
            let lines = instructions
                .iter()
                .map(|instruction| {
                    disassembly_line(controller, instruction, registers.pc(), &breakpoint)
                })
                .collect();
            section(Panel::Disassembly, lines)
        }
        Err(error) => {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticKind::Frontend,
                    "gui-disassembly-failed",
                    format!("the code at {pc:#x} could not be disassembled: {error}"),
                )
                .at(pc)
                .during(String::from("disassembling")),
            );
            section(
                Panel::Disassembly,
                vec![Line::fault(
                    format!("{pc:#010x}"),
                    format!("disassembly failed: {error}"),
                )],
            )
        }
    }
}

fn disassembly_line<D: Device>(
    controller: &DebugController<D>,
    instruction: &Disassembly,
    pc: u64,
    breakpoint: &dyn Fn(u64) -> bool,
) -> Line {
    let label = format!("{:#010x}", instruction.address);
    let text = match controller.source_location_at(instruction.address) {
        Some(place) => format!(
            "{:<30} ; {}:{}",
            instruction.text,
            place.name,
            place.line_number()
        ),
        None => instruction.text.clone(),
    };
    if breakpoint(instruction.address) {
        Line::breakpoint(label, text)
    } else if instruction.address == pc {
        Line::current(label, text)
    } else {
        Line::executed(label, text)
    }
}

fn memory_section<D: Device>(
    controller: &DebugController<D>,
    options: &ViewOptions,
    diagnostics: &mut Diagnostics,
) -> Section {
    let address = options.memory_address;
    let lines_wanted = options.memory_lines.clamp(1, MAX_MEMORY_LINES);
    let mut lines = Vec::new();
    for row in 0..lines_wanted {
        let row_address = address + (row as u64) * 32;
        let label = format!("{row_address:#010x}");
        match controller.read_memory(row_address, 32) {
            Ok(bytes) => lines.push(Line::plain(label, hex_row(&bytes))),
            Err(error) => {
                diagnostics.push(
                    Diagnostic::new(
                        DiagnosticKind::Frontend,
                        "gui-memory-unreadable",
                        format!("memory at {row_address:#x} could not be read: {error}"),
                    )
                    .during(String::from("dumping memory")),
                );
                lines.push(Line::fault(label, String::from("unreadable")));
                // One failure is enough: the next 63 rows would fail the same
                // way, and a panel of identical complaints buries the others.
                break;
            }
        }
    }
    section(Panel::Memory, lines)
}

/// One line of a hex dump: eight words and the bytes as text.
fn hex_row(bytes: &[u8]) -> String {
    let mut words = String::new();
    for chunk in bytes.chunks(4) {
        let word = chunk.iter().fold(0u32, |accumulator, byte| {
            (accumulator << 8) | u32::from(*byte)
        });
        let _ = write!(words, " {word:08x}");
    }
    let text: String = bytes
        .iter()
        .map(|byte| {
            if (0x20..0x7f).contains(byte) {
                char::from(*byte)
            } else {
                '.'
            }
        })
        .collect();
    format!("{words}  {text}")
}

fn stack_section<D: Device>(
    controller: &DebugController<D>,
    options: &ViewOptions,
    diagnostics: &mut Diagnostics,
) -> Section {
    let words = options.stack_words.clamp(1, 256);
    let pc = controller.registers().pc();
    match controller.stack(words) {
        Ok(stack) => {
            let mut lines = vec![Line::plain("sp", format!("{:#018x}", stack.sp))];
            if !stack.has_call_chain {
                // Said on the panel, once, rather than left to be inferred from a
                // flat list of numbers: a column headed "stack" invites the reader
                // to read it as frames, and it is not frames.
                lines.push(Line::plain(
                    "",
                    String::from("no frame pointers, so these are words and not frames"),
                ));
            }
            for (index, word) in stack.words.iter().enumerate() {
                let address = stack.sp + (index as u64) * 8;
                let resolved = match controller.source_location_at(*word) {
                    Some(place) => format!("  ; {}:{}", place.name, place.line_number()),
                    None => String::new(),
                };
                lines.push(Line::plain(
                    format!("{address:#010x}"),
                    format!("{word:#018x}{resolved}"),
                ));
            }
            section(Panel::Stack, lines)
        }
        Err(error) => {
            diagnostics.push(
                Diagnostic::new(
                    DiagnosticKind::Frontend,
                    "gui-stack-unreadable",
                    format!("the stack could not be read: {error}"),
                )
                .at(pc)
                .during(String::from("reading the stack")),
            );
            section(
                Panel::Stack,
                vec![Line::fault("", format!("unreadable: {error}"))],
            )
        }
    }
}

fn console_section<D: Device>(controller: &DebugController<D>, options: &ViewOptions) -> Section {
    let bytes = controller.terminal_output();
    let text = String::from_utf8_lossy(&bytes);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(options.console_lines);
    let lines = all[start..]
        .iter()
        .map(|line| Line::plain("", (*line).to_string()))
        .collect();
    section(Panel::Console, lines)
}

fn processes_section<D: Device>(controller: &DebugController<D>) -> Section {
    let mut lines = Vec::new();
    for session in controller.sessions() {
        let state = session.state();
        let count = session.breakpoints().len();
        let breakpoints = if count == 0 {
            String::from("no breakpoints")
        } else if count == 1 {
            String::from("1 breakpoint")
        } else {
            format!("{count} breakpoints")
        };
        let text = format!(
            "{} pid={} {breakpoints}",
            describe_state(&state),
            session.process().get()
        );
        lines.push(Line::new(
            String::new(),
            text,
            match state {
                ExecutionState::Faulted { .. } | ExecutionState::Exited { .. } => Emphasis::Fault,
                _ => Emphasis::Plain,
            },
        ));
    }
    if lines.is_empty() {
        lines.push(Line::plain("", String::from("no processes are loaded")));
    }
    section(Panel::Processes, lines)
}

/// A process's state as one word, for a list a person scans.
fn describe_state(state: &ExecutionState) -> &'static str {
    match state {
        ExecutionState::Ready => "ready",
        ExecutionState::Running => "running",
        ExecutionState::Stopped { .. } => "stopped",
        ExecutionState::Exited { code } if *code == 0 => "exited",
        ExecutionState::Exited { .. } => "failed",
        ExecutionState::Faulted { .. } => "faulted",
    }
}

fn diagnostics_section(diagnostics: &Diagnostics) -> Section {
    let mut lines = Vec::new();
    if diagnostics.is_empty() {
        lines.push(Line::plain("", String::from("nothing has gone wrong")));
    }
    for diagnostic in diagnostics.entries() {
        let mut text = String::from(diagnostic.kind.label());
        let _ = write!(text, ": {}", diagnostic.message);
        if let Some(source) = &diagnostic.source {
            let _ = write!(text, "  {}:{}:{}", source.name, source.line, source.column);
        }
        if let Some(pc) = diagnostic.guest_pc {
            let _ = write!(text, "  pc={pc:#x}");
        }
        if let Some(instruction) = &diagnostic.instruction {
            let _ = write!(text, "  {instruction}");
        }
        lines.push(Line::new(
            format!("[{}]", diagnostic.code),
            text,
            match diagnostic.kind {
                DiagnosticKind::EmulatorBug | DiagnosticKind::GuestFault => Emphasis::Fault,
                _ => Emphasis::Plain,
            },
        ));
    }
    section(Panel::Diagnostics, lines)
}

/// Wraps lines into a section.
fn section(panel: Panel, lines: Vec<Line>) -> Section {
    Section {
        panel,
        title: panel.title().to_string(),
        lines,
    }
}
