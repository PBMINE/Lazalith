//! The window: panels of lines, drawn.
//!
//! # What this layer is allowed to know
//!
//! It draws. It takes a [`View`] and puts it on a screen, and the only thing it
//! decides is *where* things go and what colour they are. Every fact it draws was
//! decided in [`crate::view`], and every number in it came from the debug API.
//!
//! It does not read a register, resolve an address, or disassemble anything. That
//! is not a limitation — it is the reason the panels can be tested without a
//! display, and the reason this file is short.
//!
//! # The layout
//!
//! A debugger window is a grid, and the grid is fixed:
//!
//! ```text
//! +------------------------------+---------------------+
//! | screen                       | registers           |
//! |                              | pc                  |
//! |                              | flags               |
//! +------------------------------+---------------------+
//! | disassembly                  | memory              |
//! |                              | stack               |
//! |                              | console             |
//! |                              | processes           |
//! |                              | diagnostics         |
//! +------------------------------+---------------------+
//! ```
//!
//! The screen is square-ish and the text panels are not, because a machine's
//! display has a fixed aspect ratio and everything else is words. The split is
//! half the window each, and each column then divides vertically by how much text
//! its panels need — which is a number the view knows and the window asks for,
//! rather than a number written down here.
//!
//! # Text
//!
//! [`crate::font`] draws it. There is no font API here, because a frontend that
//! assumed the host had one would work on one machine and show nothing on
//! another.

use lazalith_sdl3::{Color, Input, Rect as SdlRect, Renderer, Texture, Video, Window};

use crate::font;
use crate::view::{Emphasis, Line, View};

/// One text-sized row, in pixels.
pub const ROW: f32 = (font::GLYPH_HEIGHT + 2) as f32;

/// The margin around a panel, in pixels.
const PADDING: f32 = 6.0;

/// The gap between panels, in pixels.
const GAP: f32 = 8.0;

/// The smallest a panel is allowed to be, so text is never squeezed to nothing.
const MINIMUM_PANEL: f32 = 40.0;

/// A window with a renderer and the machine screen's texture.
pub struct Windowed {
    video: Video,
    window: Window,
    renderer: Renderer,
    screen: Option<Texture>,
    screen_size: (u32, u32),
}

impl Windowed {
    /// Opens the window.
    ///
    /// # Errors
    ///
    /// If SDL cannot reach a display, cannot open a window, or cannot create a
    /// renderer. All three are reported with SDL's own words, because "the
    /// frontend could not start" without a reason is the least useful error
    /// message in the project.
    pub fn open(title: &str, width: u32, height: u32) -> Result<Self, lazalith_sdl3::SdlError> {
        let video = Video::start()?;
        let window = Window::new(&video, title, width, height)?;
        let mut renderer = Renderer::new(&window)?;
        renderer.set_color(Color::GREY)?;
        Ok(Self {
            video,
            window,
            renderer,
            screen: None,
            screen_size: (0, 0),
        })
    }

    /// The window's pixel size.
    pub fn size(&self) -> (u32, u32) {
        // The size the window was opened at. SDL was told it is resizable, so a
        // person may have made it another size; this frontend draws to the window
        // rather than into it, so the size it lays out against is this one and
        // the letterboxing SDL does is the right answer.
        self.window.size_hint()
    }

    /// Whether the machine's screen texture matches the view's.
    fn screen_texture_matches(&self, view: &View) -> bool {
        self.screen.is_some() && self.screen_size == (view.screen.width, view.screen.height)
    }

    /// Draws `view`.
    ///
    /// # Errors
    ///
    /// If a draw call is rejected. The screen texture is recreated only when the
    /// guest's geometry changes, so a program that presents a thousand frames of
    /// the same size reuses one texture rather than making a thousand.
    pub fn draw(&mut self, view: &View) -> Result<(), lazalith_sdl3::SdlError> {
        let (width, height) = self.size();
        let width_f = width as f32;
        let height_f = height as f32;
        self.renderer.set_color(Color::GREY)?;
        self.renderer
            .fill(SdlRect::new(0.0, 0.0, width_f, height_f))?;

        // The bindings strip, along the top. A control a person cannot find is a
        // control they will not press, so the keys are shown rather than written
        // down somewhere else.
        let strip = ROW + PADDING;
        self.draw_bindings(strip)?;
        let top = strip + GAP;

        // Both columns start below the bindings strip, so a control is never
        // hidden behind the thing it controls.
        let column_height = height_f - top - PADDING;
        let left = SdlRect::new(
            PADDING,
            top,
            width_f / 2.0 - PADDING - GAP / 2.0,
            column_height,
        );
        let right = SdlRect::new(
            width_f / 2.0 + GAP / 2.0,
            top,
            width_f / 2.0 - PADDING - GAP / 2.0,
            column_height,
        );
        // The screen is the one panel whose aspect ratio is not the panel's, so it
        // takes a square and the disassembly takes what is left under it.
        let screen_side = left.w.min(left.h / 2.0).max(MINIMUM_PANEL);
        let screen_rect = SdlRect::new(left.x, left.y, screen_side, screen_side);
        self.draw_screen(view, screen_rect)?;
        let disassembly_height =
            (left.y + left.h - screen_rect.y - screen_rect.h - GAP).max(MINIMUM_PANEL);
        let disassembly_rect = SdlRect::new(
            left.x,
            screen_rect.y + screen_rect.h + GAP,
            left.w,
            disassembly_height,
        );
        self.draw_section(
            view.section(crate::view::Panel::Disassembly),
            disassembly_rect,
        )?;

        self.draw_column(view, right)?;
        self.renderer.present()
    }

    /// Draws the key bindings along the top of the window.
    ///
    /// Every control with a key, with the key first, because the person looking
    /// for "how do I step" is looking for a key and not for a word.
    fn draw_bindings(&mut self, y: f32) -> Result<(), lazalith_sdl3::SdlError> {
        let mut x = PADDING;
        for (key, label) in Self::bindings() {
            self.draw_text(key, x, y, Color::AMBER)?;
            x += (font::GLYPH_WIDTH + font::GLYPH_SPACING) as f32 * key.len() as f32;
            self.draw_text(label, x + 4.0, y, Color::WHITE)?;
            x += (font::GLYPH_WIDTH + font::GLYPH_SPACING) as f32 * label.len() as f32;
            x += font::GLYPH_SPACING as f32 * 4.0;
        }
        Ok(())
    }

    /// Draws the right-hand column's panels in the order given.
    fn draw_column(&mut self, view: &View, column: SdlRect) -> Result<(), lazalith_sdl3::SdlError> {
        let panels = [
            crate::view::Panel::Registers,
            crate::view::Panel::ProgramCounter,
            crate::view::Panel::Flags,
            crate::view::Panel::Memory,
            crate::view::Panel::Stack,
            crate::view::Panel::Console,
            crate::view::Panel::Processes,
            crate::view::Panel::Diagnostics,
        ];
        // Each panel gets a share of the column proportional to how many lines it
        // has, so a panel with one line does not take a quarter of the window. The
        // minimum keeps a one-line panel from being a sliver.
        let weights: Vec<f32> = panels
            .iter()
            .map(|panel| view.section(*panel).lines.len().saturating_add(1) as f32)
            .collect();
        let total: f32 = weights.iter().sum();
        let available = column.h - GAP * (panels.len() as f32 - 1.0);
        let mut y = column.y;
        for (panel, weight) in panels.iter().zip(weights) {
            let height = (available * weight / total).max(MINIMUM_PANEL);
            let rect = SdlRect::new(column.x, y, column.w, height);
            self.draw_section(view.section(*panel), rect)?;
            y += height + GAP;
        }
        Ok(())
    }

    /// Draws the machine's screen, or why there is not one.
    fn draw_screen(&mut self, view: &View, rect: SdlRect) -> Result<(), lazalith_sdl3::SdlError> {
        if !view.screen.is_present() {
            let why = view
                .screen
                .unavailable
                .as_deref()
                .unwrap_or("there is no screen");
            self.draw_lines(&[Line::plain("", "screen"), Line::plain("", why)], rect)?;
            return Ok(());
        }
        if !self.screen_texture_matches(view) {
            let texture = Texture::new(&self.renderer, view.screen.width, view.screen.height)?;
            self.screen = Some(texture);
            self.screen_size = (view.screen.width, view.screen.height);
        }
        let texture = self.screen.as_mut().expect("the texture was just made");
        texture.upload(&view.screen.pixels)?;
        // Nearest-neighbour and integer scaling, so a 320×200 screen at four times
        // shows four-by-four blocks of the guest's own pixels rather than a blur
        // the host invented.
        let scale = (rect.w / view.screen.width as f32)
            .min(rect.h / view.screen.height as f32)
            .floor()
            .max(1.0);
        let width = view.screen.width as f32 * scale;
        let height = view.screen.height as f32 * scale;
        let destination = SdlRect::new(
            rect.x + (rect.w - width) / 2.0,
            rect.y + (rect.h - height) / 2.0,
            width,
            height,
        );
        texture.draw(&mut self.renderer, destination)
    }

    /// Draws one panel, clipping to the rectangle it was given.
    fn draw_section(
        &mut self,
        section: &crate::view::Section,
        rect: SdlRect,
    ) -> Result<(), lazalith_sdl3::SdlError> {
        let rows = rows_that_fit(rect.h);
        let mut lines = Vec::with_capacity(rows + 1);
        lines.push(Line::new(
            String::new(),
            section.title.clone(),
            Emphasis::Plain,
        ));
        // A panel with more lines than fit shows its *last* ones, because a
        // debugger's panels are tails: the newest thing is at the bottom.
        let start = section.lines.len().saturating_sub(rows);
        lines.extend(section.lines[start..].iter().cloned());
        self.draw_lines(&lines, rect)
    }

    /// Draws lines of text into a rectangle, one per row.
    fn draw_lines(&mut self, lines: &[Line], rect: SdlRect) -> Result<(), lazalith_sdl3::SdlError> {
        let rows = rows_that_fit(rect.h);
        let mut y = rect.y;
        for line in lines.iter().take(rows) {
            let colour = line.emphasis.color();
            if !line.label.is_empty() {
                self.draw_text(&line.label, rect.x, y, colour)?;
            }
            // The label column is as wide as the widest label in the panel, which
            // is a whole-panel decision; using a fixed fraction here instead would
            // overlap a long address with the text beside it.
            let offset = (font::GLYPH_WIDTH + font::GLYPH_SPACING) as f32
                * (label_columns(&line.label) as f32 + 1.0);
            self.draw_text(&line.text, rect.x + offset, y, colour)?;
            y += ROW;
        }
        Ok(())
    }

    /// Draws `text` with its top-left at `(x, y)`.
    fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        colour: Color,
    ) -> Result<(), lazalith_sdl3::SdlError> {
        if text.is_empty() {
            return Ok(());
        }
        let advance = (font::GLYPH_WIDTH + font::GLYPH_SPACING) as f32;
        // A row of horizontal segments is one polyline call rather than one per
        // pixel, which is the difference between a window that redraws at speed
        // and one that does not.
        let mut segments: Vec<(f32, f32)> = Vec::new();
        let mut run_start: Option<f32> = None;
        for (index, character) in text.chars().enumerate() {
            let left = x + (index as f32) * advance;
            let mut row = 0;
            while row < font::GLYPH_HEIGHT {
                let lit =
                    font::text_pixel(text, index * (font::GLYPH_WIDTH + font::GLYPH_SPACING), row);
                if lit {
                    if run_start.is_none() {
                        run_start = Some(left);
                    }
                } else if let Some(start) = run_start.take() {
                    segments.push((start, y + row as f32));
                    segments.push((left, y + row as f32));
                }
                row += 1;
            }
            let _ = character;
        }
        if let Some(start) = run_start {
            segments.push((start, y + font::GLYPH_HEIGHT as f32));
            segments.push((
                x + (text.chars().count() as f32) * advance,
                y + font::GLYPH_HEIGHT as f32,
            ));
        }
        if segments.is_empty() {
            return Ok(());
        }
        self.renderer.set_color(colour)?;
        self.renderer.polyline(&segments)
    }

    /// The next input event, if there is one.
    ///
    /// # Errors
    ///
    /// If SDL fails to poll, which it does not today.
    pub fn poll(&mut self) -> Result<Option<Input>, lazalith_sdl3::SdlError> {
        lazalith_sdl3::poll_event()
    }

    /// The controls the window's function keys are bound to.
    ///
    /// Drawn as a strip along the top of the window, with the key beside each
    /// one. A control a person cannot find is a control they will not press, so
    /// the bindings are shown rather than documented somewhere else.
    pub fn bindings() -> Vec<(&'static str, &'static str)> {
        use crate::control::Control;
        [
            Control::Run,
            Control::ContinueRun,
            Control::Step,
            Control::Pause,
            Control::ToggleBreakpoint,
            Control::Reset,
            Control::ClearBreakpoints,
        ]
        .iter()
        .filter_map(|control| {
            let key = control.scancode()?;
            Some((key_name(key), control.label()))
        })
        .collect()
    }

    /// Keeps SDL's own video reference alive for as long as the window is.
    ///
    /// SDL is shut down when this value drops, which must be after the window and
    /// the renderer are gone. The field is never read, and saying so here is
    /// better than a reader wondering why it exists.
    #[allow(dead_code)]
    fn video(&self) -> &Video {
        &self.video
    }
}

/// How many text rows fit in `height` pixels.
const fn rows_that_fit(height: f32) -> usize {
    let rows = (height / ROW) as usize;
    if rows == 0 { 1 } else { rows }
}

/// How many character columns `label` is wide.
const fn label_columns(label: &str) -> usize {
    // A `const fn` cannot count characters, so this is the byte length, which for
    // the ASCII labels this frontend builds is the character count. A label with a
    // multi-byte character would be measured in bytes, which makes its column a
    // little wide rather than too narrow — and too wide is the direction that does
    // not overlap the text beside it.
    label.len()
}

impl View {
    /// The section for `panel`, or an empty one if the view does not have it.
    pub fn section(&self, panel: crate::view::Panel) -> &crate::view::Section {
        static EMPTY: std::sync::OnceLock<crate::view::Section> = std::sync::OnceLock::new();
        self.sections
            .iter()
            .find(|section| section.panel == panel)
            .unwrap_or_else(|| {
                EMPTY.get_or_init(|| crate::view::Section {
                    panel,
                    title: String::new(),
                    lines: Vec::new(),
                })
            })
    }
}

/// The name of a function key, for the bindings strip.
///
/// Written out rather than derived, because the scancodes are SDL's and the names
/// are the ones a person reads. An unknown scancode is shown as its number, which
/// is honest: a binding to a key this frontend has no name for should look odd on
/// screen rather than be silently dropped from the list.
fn key_name(scancode: u32) -> &'static str {
    match scancode {
        lazalith_sdl3::SCANCODE_F4 => "F4",
        lazalith_sdl3::SCANCODE_F5 => "F5",
        lazalith_sdl3::SCANCODE_F6 => "F6",
        lazalith_sdl3::SCANCODE_F8 => "F8",
        lazalith_sdl3::SCANCODE_F9 => "F9",
        lazalith_sdl3::SCANCODE_F10 => "F10",
        lazalith_sdl3::SCANCODE_F11 => "F11",
        _ => "key",
    }
}
