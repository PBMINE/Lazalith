//! The SDL3 boundary.
//!
//! # Why this crate exists
//!
//! SDL3 is a C library. Calling into one requires `unsafe`, and the workspace
//! forbids `unsafe_code` in every other crate — a rule worth keeping, because it
//! is what makes it impossible for a frontend to write a CPU register or to build
//! a pointer into machine memory by accident.
//!
//! So the rule is relaxed here and only here. This crate is the entire `unsafe`
//! surface of the project: a set of `extern "C"` declarations and the safe
//! functions that wrap them. It has no Lazalith logic in it, knows nothing about
//! machines, registers or guest memory, and cannot execute an instruction. The
//! frontend above it is ordinary safe Rust and is checked like the rest.
//!
//! That split is also what makes the frontend testable. Everything that decides
//! *what* to draw lives above this line and is headless; this file only knows how
//! to put pixels on a screen and read key presses.
//!
//! # What the safe API guarantees
//!
//! - Every pointer SDL returns is owned by exactly one Rust value, and that value
//!   destroys it in `Drop`. There is no path that leaks a `SDL_Window` and no
//!   double free, because there is no way to construct one of these values except
//!   by the call that returns it.
//! - `SDL_Init` and `SDL_Quit` are paired by [`Video`], so a frontend that drops
//!   its video shuts SDL down.
//! - An event buffer is larger than SDL's own event union, so SDL cannot write
//!   past it. That size is checked by a test, which compares it against the
//!   headers' declared structs rather than against a number written down here.
//! - Every fallible call returns a `Result` carrying SDL's own error text. A
//!   frontend that cannot draw says why instead of showing a black window.

#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

use core::ffi::{CStr, c_char, c_int};
use core::ptr::NonNull;

/// The sizes and offsets SDL3's own headers declare, measured by the C compiler.
///
/// The build script compiles a probe against the headers in use and generates
/// these. Every layout claim this file makes is a `const` assertion against them,
/// so a header that moved a field, grew an event, or changed a type's width is a
/// compile error naming the field — not a frontend reading a keycode out of the
/// middle of a timestamp.
mod layout {
    include!(concat!(env!("OUT_DIR"), "/layout.rs"));
}

// Imported under a name rather than glob-imported, because the measured names
// are the C field names themselves — `scancode`, `repeat` — and a bare import would
// shadow every local binding of the same name in this file.
use layout as measured;

/// An SDL error, with SDL's own words for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdlError {
    /// What SDL said, which is more specific than any message written here.
    pub message: String,
}

impl core::fmt::Display for SdlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SdlError {}

/// The result of a call into SDL.
pub type SdlResult<T> = Result<T, SdlError>;

/// A colour, as four bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Color {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha, where `0` is transparent and `255` is opaque.
    pub a: u8,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// Opaque white.
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    /// Opaque grey, for panel backgrounds and separators.
    pub const GREY: Self = Self {
        r: 32,
        g: 34,
        b: 40,
        a: 255,
    };
    /// Opaque dark green, for a line that executed.
    pub const GREEN: Self = Self {
        r: 96,
        g: 200,
        b: 120,
        a: 255,
    };
    /// Opaque amber, for the instruction about to execute.
    pub const AMBER: Self = Self {
        r: 230,
        g: 180,
        b: 70,
        a: 255,
    };
    /// Opaque red, for a fault or an error.
    pub const RED: Self = Self {
        r: 220,
        g: 90,
        b: 90,
        a: 255,
    };
    /// Opaque blue, for memory the program has written.
    pub const BLUE: Self = Self {
        r: 110,
        g: 150,
        b: 230,
        a: 255,
    };
}

/// An axis-aligned rectangle, in the coordinates it is given.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

/// What the user did that the frontend cares about.
///
/// SDL reports a great deal more than this; a debugger needs a key press and a
/// request to close. Reducing the rest here means the frontend above cannot
/// accidentally depend on a field of an SDL struct whose layout it would then be
/// guessing at.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Input {
    /// The window was closed, or the platform asked the application to stop.
    Quit,
    /// A key went down. SDL reports many more events than this; a debugger needs a
    /// key press and a request to close, and reducing the rest here means the
    /// frontend cannot depend on a field whose layout it would be guessing at.
    KeyDown {
        /// SDL's scancode for the key: which physical key it is, independent
        /// of the layout someone is typing in.
        scancode: u32,
        /// SDL's keycode for the key: which character it would produce.
        key: u32,
        /// Whether this is the keyboard's own auto-repeat rather than a new press.
        repeat: bool,
    },
}

/// SDL3's own declarations.
///
/// Nothing in here is called directly. The functions above go through these
/// names, which is what keeps the `unsafe` blocks in one file and lets every
/// call site elsewhere be a safe function call.
mod sys {
    use core::ffi::{c_char, c_float, c_int};

    /// An opaque SDL window.
    #[repr(C)]
    pub struct SdlWindow {
        _private: [u8; 0],
    }

    /// An opaque SDL renderer.
    #[repr(C)]
    pub struct SdlRenderer {
        _private: [u8; 0],
    }

    /// An opaque SDL texture.
    #[repr(C)]
    pub struct SdlTexture {
        _private: [u8; 0],
    }

    /// SDL's keyboard event, in the C layout.
    ///
    /// Every field's offset and the struct's own size are asserted against what
    /// the C compiler measured, so this is not a claim about SDL's layout but a
    /// check of it.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct SdlKeyboardEvent {
        /// The event type, which is the union's discriminant.
        pub event_type: u32,
        /// Reserved.
        pub reserved: u32,
        /// When it happened, in nanoseconds.
        pub timestamp: u64,
        /// The window with focus, or zero.
        pub window_id: u32,
        /// The keyboard instance, or zero.
        pub which: u32,
        /// The physical key.
        pub scancode: u32,
        /// The virtual key.
        pub key: u32,
        /// Held modifiers.
        pub modifiers: u16,
        /// The platform's own scancode.
        pub raw: u16,
        /// Whether the key is down.
        pub down: bool,
        /// Whether this is an auto-repeat.
        pub repeat: bool,
    }

    /// An event, as a union with padding.
    ///
    /// The padding arm is what makes this sound: SDL writes at most one event
    /// struct, and the union's size is its largest member, so a buffer of this
    /// size cannot be overrun no matter which event arrives. `EVENT_BUFFER_SIZE`
    /// is checked against the headers by a test.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union SdlEvent {
        /// The event type, valid for every event.
        pub event_type: u32,
        /// The keyboard event, valid when the type says so.
        pub key: SdlKeyboardEvent,
        /// Padding, so the buffer is bigger than any event SDL can write.
        pub padding: [u8; crate::EVENT_BUFFER_SIZE],
    }

    unsafe extern "C" {
        pub fn SDL_Init(flags: u32) -> bool;
        pub fn SDL_Quit();
        pub fn SDL_GetError() -> *const c_char;

        pub fn SDL_CreateWindow(
            title: *const c_char,
            width: c_int,
            height: c_int,
            flags: u64,
        ) -> *mut SdlWindow;
        pub fn SDL_DestroyWindow(window: *mut SdlWindow);

        pub fn SDL_CreateRenderer(window: *mut SdlWindow, name: *const c_char) -> *mut SdlRenderer;
        pub fn SDL_DestroyRenderer(renderer: *mut SdlRenderer);

        pub fn SDL_CreateTexture(
            renderer: *mut SdlRenderer,
            format: u32,
            access: c_int,
            width: c_int,
            height: c_int,
        ) -> *mut SdlTexture;
        pub fn SDL_DestroyTexture(texture: *mut SdlTexture);
        pub fn SDL_SetTextureScaleMode(texture: *mut SdlTexture, mode: c_int) -> bool;
        pub fn SDL_UpdateTexture(
            texture: *mut SdlTexture,
            rect: *const sys_rect,
            pixels: *const u8,
            pitch: c_int,
        ) -> bool;

        pub fn SDL_SetRenderDrawColor(
            renderer: *mut SdlRenderer,
            r: u8,
            g: u8,
            b: u8,
            a: u8,
        ) -> bool;
        pub fn SDL_RenderFillRect(renderer: *mut SdlRenderer, rect: *const sys_rect) -> bool;
        pub fn SDL_RenderLines(
            renderer: *mut SdlRenderer,
            points: *const point,
            count: c_int,
        ) -> bool;
        pub fn SDL_RenderTexture(
            renderer: *mut SdlRenderer,
            texture: *mut SdlTexture,
            source: *const sys_rect,
            destination: *const sys_rect,
        ) -> bool;
        pub fn SDL_RenderPresent(renderer: *mut SdlRenderer) -> bool;

        pub fn SDL_PollEvent(event: *mut SdlEvent) -> bool;
        pub fn SDL_Delay(milliseconds: u32);
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct sys_rect {
        /// Left edge.
        pub x: c_float,
        /// Top edge.
        pub y: c_float,
        /// Width.
        pub w: c_float,
        /// Height.
        pub h: c_float,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct point {
        /// Horizontal coordinate.
        pub x: c_float,
        /// Vertical coordinate.
        pub y: c_float,
    }
}

/// How many bytes an event buffer must be able to hold.
///
/// SDL's `SDL_Event` is a union whose size is its largest member. The C compiler
/// measured that size when this crate was built, and the assertion below fails the
/// build if the buffer is the smaller of the two. Over-estimating costs a few
/// hundred bytes; guessing low would be a buffer overflow, so the guess is not
/// made — it is measured.
pub const EVENT_BUFFER_SIZE: usize = 256;

// The layout claims, as compile-time checks.
//
// Each of these is a claim about a C struct that this crate depends on. Asserting
// them here means an SDL3 that moved a field, grew an event, or changed a type's
// width produces a build error naming the field, rather than a frontend that
// reads a keycode out of the middle of a timestamp.
const _: () = {
    // The buffer must hold the largest event, or SDL can write past it.
    assert!(
        EVENT_BUFFER_SIZE >= measured::event,
        "SDL3's SDL_Event is larger than this crate's event buffer, so an \
         event could be written past the end of it. Raise EVENT_BUFFER_SIZE."
    );
    // The C and Rust sizes of the struct this crate mirrors must agree, or every
    // field after the first disagreement is at the wrong offset.
    assert!(
        core::mem::size_of::<sys::SdlKeyboardEvent>() == measured::key_event,
        "the Rust SDL_KeyboardEvent is a different size from the C one, so its \
         fields are at different offsets than SDL writes them"
    );
    assert!(
        core::mem::offset_of!(sys::SdlKeyboardEvent, key) == measured::key_field,
        "the keycode is at a different offset than SDL writes it"
    );
    assert!(
        core::mem::offset_of!(sys::SdlKeyboardEvent, scancode) == measured::scancode,
        "the scancode is at a different offset than SDL writes it"
    );
    assert!(
        core::mem::offset_of!(sys::SdlKeyboardEvent, repeat) == measured::repeat,
        "the repeat flag is at a different offset than SDL writes it"
    );
    // The union's key arm is where the keyboard event lands.
    assert!(
        core::mem::offset_of!(sys::SdlEvent, key) == measured::key_offset,
        "the keyboard event is at a different offset within SDL_Event than it is here"
    );
    // The widths the field types assume.
    assert!(
        measured::bool_size == 1,
        "C's bool is not one byte, and the Rust `bool` this mirrors is one"
    );
    assert!(
        measured::keycode == 4,
        "SDL3's SDL_Keycode is not four bytes, and this mirrors it as a u32"
    );
    assert!(
        measured::keymod == 2,
        "SDL3's SDL_Keymod is not two bytes, and this mirrors it as a u16"
    );
};

const INIT_VIDEO: u32 = 0x0000_0020;
const WINDOW_RESIZABLE: u64 = 0x0000_0020;
const TEXTUREACCESS_STREAMING: c_int = 1;
const SCALEMODE_NEAREST: c_int = 1;
const PIXELFORMAT_XRGB8888: u32 = 0x1616_1804;
const EVENT_QUIT: u32 = 0x100;
const EVENT_KEY_DOWN: u32 = 0x300;

/// SDL's scancode for F4, as the headers declare it.
///
/// Bindings are by *scancode* rather than by keycode on purpose. A scancode is a
/// physical key and a keycode is a character, so a binding on a keycode would
/// move when someone changed their keyboard layout — and a debugger's function-key
/// bindings are about physical keys. These are measured from the headers by the
/// build script rather than written down, because a debugger that binds F9 to the
/// wrong key is a debugger that is wrong in a way nobody would trace back here.
pub const SCANCODE_F4: u32 = measured::scancode_f4 as u32;
/// SDL's scancode for F5.
pub const SCANCODE_F5: u32 = measured::scancode_f5 as u32;
/// SDL's scancode for F6.
pub const SCANCODE_F6: u32 = measured::scancode_f6 as u32;
/// SDL's scancode for F8.
pub const SCANCODE_F8: u32 = measured::scancode_f8 as u32;
/// SDL's scancode for F9.
pub const SCANCODE_F9: u32 = measured::scancode_f9 as u32;
/// SDL's scancode for F10.
pub const SCANCODE_F10: u32 = measured::scancode_f10 as u32;
/// SDL's scancode for F11.
pub const SCANCODE_F11: u32 = measured::scancode_f11 as u32;

/// SDL's last error, as a `String`.
///
/// # Safety
///
/// Must only be called when SDL has just reported a failure, because SDL's error
/// string is a single global slot that any call can overwrite.
unsafe fn last_error() -> SdlError {
    // SAFETY: SDL guarantees a non-null NUL-terminated string for as long as SDL
    // is initialised, and this is called only from a failure path where SDL has
    // just written to it.
    let message = unsafe {
        let raw = sys::SDL_GetError();
        if raw.is_null() {
            String::new()
        } else {
            CStr::from_ptr(raw).to_string_lossy().into_owned()
        }
    };
    SdlError { message }
}

/// SDL's video subsystem, initialised for as long as this value exists.
pub struct Video {
    /// Kept so the type cannot be constructed without calling `SDL_Init`.
    _private: (),
}

impl Video {
    /// Starts SDL's video subsystem.
    ///
    /// # Errors
    ///
    /// If SDL cannot reach a display, which is the usual failure on a machine
    /// with no graphics session. A frontend is the one program where that is worth
    /// reporting rather than working around.
    pub fn start() -> SdlResult<Self> {
        // SAFETY: `SDL_Init` is a plain C call with no pointer arguments, and the
        // result is a bool this checks. Calling it twice is defined by SDL, and
        // this returns a value that pairs the call with `SDL_Quit`.
        if unsafe { sys::SDL_Init(INIT_VIDEO) } {
            Ok(Self { _private: () })
        } else {
            // SAFETY: `SDL_Init` just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        // SAFETY: paired with the `SDL_Init` in `start`, and SDL counts these, so
        // dropping one `Video` for each successful start is balanced.
        unsafe { sys::SDL_Quit() };
    }
}

/// A window, destroyed with the value that owns it.
pub struct Window {
    /// The SDL window. `NonNull` because SDL returns null on failure and that is
    /// checked before this is constructed.
    raw: NonNull<sys::SdlWindow>,
    /// The size it was created at, kept so a caller can lay out against it
    /// without another FFI call. A resized window would make this stale, which is
    /// why a caller that cares about the real size must ask SDL.
    size: (u32, u32),
}

impl Window {
    /// Opens a resizable window.
    ///
    /// # Errors
    ///
    /// If SDL cannot create a window.
    pub fn new(video: &Video, title: &str, width: u32, height: u32) -> SdlResult<Self> {
        // `Video` is taken by reference on purpose: the value must outlive every
        // window, and taking it by value would let a caller drop it first.
        let _ = video;
        let text = CString::new(title);
        // SAFETY: `text` is a valid NUL-terminated string for the duration of the
        // call, SDL copies the title, and the width and height are plain integers.
        let raw = unsafe {
            sys::SDL_CreateWindow(
                text.as_ptr(),
                c_int::try_from(width).unwrap_or(c_int::MAX),
                c_int::try_from(height).unwrap_or(c_int::MAX),
                WINDOW_RESIZABLE,
            )
        };
        match NonNull::new(raw) {
            Some(raw) => Ok(Self {
                raw,
                size: (width, height),
            }),
            // SAFETY: the call just failed, so the error slot is ours to read.
            None => Err(unsafe { last_error() }),
        }
    }

    /// The size the window was created at.
    ///
    /// A hint, not the truth: SDL was told the window is resizable, so a person
    /// may have made it another size. A caller that draws to the window rather
    /// than into it does not care, and this says so rather than pretending.
    pub const fn size_hint(&self) -> (u32, u32) {
        self.size
    }

    /// The pointer this window owns.
    fn raw(&self) -> *mut sys::SdlWindow {
        self.raw.as_ptr()
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `SDL_CreateWindow`, is stored once, and is
        // destroyed exactly here. A `Window` cannot be cloned or moved out of.
        unsafe { sys::SDL_DestroyWindow(self.raw.as_ptr()) };
    }
}

/// A renderer for one window.
pub struct Renderer {
    /// The SDL renderer.
    raw: NonNull<sys::SdlRenderer>,
}

impl Renderer {
    /// Creates a renderer, letting SDL pick a driver.
    ///
    /// # Errors
    ///
    /// If SDL cannot create a renderer. The name is passed as null, which is how
    /// SDL3 spells "choose for me": a frontend that named a driver would fail on
    /// a machine that only has another one, and a debugger is the last program
    /// that should be fussy about that.
    pub fn new(window: &Window) -> SdlResult<Self> {
        // SAFETY: the window pointer is valid because the caller holds the window,
        // and a null name is SDL3's documented way to request automatic selection.
        let raw = unsafe { sys::SDL_CreateRenderer(window.raw(), core::ptr::null()) };
        match NonNull::new(raw) {
            Some(raw) => Ok(Self { raw }),
            // SAFETY: the call just failed, so the error slot is ours to read.
            None => Err(unsafe { last_error() }),
        }
    }

    /// Sets the colour subsequent fills and lines use.
    ///
    /// # Errors
    ///
    /// If SDL rejects the colour.
    pub fn set_color(&mut self, color: Color) -> SdlResult<()> {
        // SAFETY: the renderer is valid and the four bytes are plain values.
        let ok = unsafe {
            sys::SDL_SetRenderDrawColor(self.raw.as_ptr(), color.r, color.g, color.b, color.a)
        };
        if ok {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }

    /// Fills a rectangle with the current colour.
    ///
    /// # Errors
    ///
    /// If SDL rejects the rectangle.
    pub fn fill(&mut self, rect: Rect) -> SdlResult<()> {
        let native = sys::sys_rect {
            x: rect.x,
            y: rect.y,
            w: rect.w,
            h: rect.h,
        };
        // SAFETY: the renderer is valid and `native` is a live local of the exact
        // layout SDL expects, borrowed for the call.
        let ok = unsafe { sys::SDL_RenderFillRect(self.raw.as_ptr(), &native) };
        if ok {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }

    /// Draws a line through `points`.
    ///
    /// # Errors
    ///
    /// If SDL rejects the points. Fewer than two points draws nothing, which SDL
    /// reports as success, and this does too rather than inventing an error.
    pub fn polyline(&mut self, points: &[(f32, f32)]) -> SdlResult<()> {
        if points.len() < 2 {
            return Ok(());
        }
        let native: Vec<sys::point> = points.iter().map(|&(x, y)| sys::point { x, y }).collect();
        // SAFETY: the renderer is valid, `native` holds at least two points for
        // the duration of the call, and the count matches the slice's length.
        let ok = unsafe {
            sys::SDL_RenderLines(
                self.raw.as_ptr(),
                native.as_ptr(),
                c_int::try_from(native.len()).unwrap_or(c_int::MAX),
            )
        };
        if ok {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }

    /// Shows what has been drawn since the last present.
    ///
    /// # Errors
    ///
    /// If SDL cannot present.
    pub fn present(&mut self) -> SdlResult<()> {
        // SAFETY: the renderer is valid and takes no pointer arguments.
        if unsafe { sys::SDL_RenderPresent(self.raw.as_ptr()) } {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }

    /// The pointer this renderer owns.
    fn raw(&self) -> *mut sys::SdlRenderer {
        self.raw.as_ptr()
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `SDL_CreateRenderer` and is destroyed
        // exactly once, here. The window it belongs to outlives it because a
        // `Renderer` borrows its `Window` to be created and the frontend holds
        // the window for as long as it holds the renderer.
        unsafe { sys::SDL_DestroyRenderer(self.raw.as_ptr()) };
    }
}

/// A texture a caller uploads pixels into.
pub struct Texture {
    /// The SDL texture.
    raw: NonNull<sys::SdlTexture>,
    /// The width the texture was created with, kept so `upload` can check a
    /// caller's slice length without asking SDL.
    width: u32,
    /// The height the texture was created with.
    height: u32,
}

impl Texture {
    /// Creates a streaming texture `width` by `height`.
    ///
    /// The pixel format is `XRGB8888`: three colour bytes and one unused byte,
    /// which is what a machine's framebuffer is. The unused byte is written by
    /// SDL, not read back, so a caller does not have to care about it.
    ///
    /// # Errors
    ///
    /// If SDL cannot create the texture, which is what a zero-sized texture is.
    pub fn new(renderer: &Renderer, width: u32, height: u32) -> SdlResult<Self> {
        // SAFETY: the renderer is valid, and the dimensions are plain integers.
        // SDL3 takes them as `int`, so a framebuffer wider than `c_int::MAX` would
        // be clamped rather than rejected; the machine's own display is far
        // smaller than that, and the check below turns anything else into an error
        // rather than a silent clamp.
        let width = c_int::try_from(width).map_err(|_| SdlError {
            message: format!("a texture cannot be {width} pixels wide"),
        })?;
        let height = c_int::try_from(height).map_err(|_| SdlError {
            message: format!("a texture cannot be {height} pixels tall"),
        })?;
        if width <= 0 || height <= 0 {
            return Err(SdlError {
                message: format!("a texture cannot be {width} by {height} pixels"),
            });
        }
        let raw = unsafe {
            sys::SDL_CreateTexture(
                renderer.raw(),
                PIXELFORMAT_XRGB8888,
                TEXTUREACCESS_STREAMING,
                width,
                height,
            )
        };
        let Some(raw) = NonNull::new(raw) else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            return Err(unsafe { last_error() });
        };
        // Nearest-neighbour scaling, because a debugger showing a 320-by-200
        // machine screen at eight times size should show the machine's pixels and
        // not a blur invented by the host.
        // SAFETY: the texture was just created and is not null.
        let _ = unsafe { sys::SDL_SetTextureScaleMode(raw.as_ptr(), SCALEMODE_NEAREST) };
        Ok(Self {
            raw,
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
        })
    }

    /// The width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Replaces the texture's pixels.
    ///
    /// `pixels` must be exactly `width * height * 4` bytes, in `XRGB8888` order.
    ///
    /// # Errors
    ///
    /// If `pixels` is the wrong length for this texture. A short slice would have
    /// SDL read past its end, so it is refused here rather than clamped.
    pub fn upload(&mut self, pixels: &[u8]) -> SdlResult<()> {
        let expected = self
            .width
            .checked_mul(self.height)
            .and_then(|count| count.checked_mul(4))
            .and_then(|count| usize::try_from(count).ok())
            .ok_or_else(|| SdlError {
                message: String::from("a texture that large cannot be described"),
            })?;
        if pixels.len() != expected {
            return Err(SdlError {
                message: format!(
                    "a {}x{} texture needs {expected} bytes of pixels, not {}",
                    self.width,
                    self.height,
                    pixels.len()
                ),
            });
        }
        let pitch = c_int::try_from(self.width * 4).map_err(|_| SdlError {
            message: String::from("a texture wider than `c_int` cannot be uploaded"),
        })?;
        // SAFETY: the texture is valid, `pixels` is exactly the length this texture
        // needs and outlives the call, and a null rect means the whole texture.
        let ok = unsafe {
            sys::SDL_UpdateTexture(self.raw.as_ptr(), core::ptr::null(), pixels.as_ptr(), pitch)
        };
        if ok {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }

    /// Draws the texture into `destination`, scaled.
    ///
    /// # Errors
    ///
    /// If SDL rejects the destination.
    pub fn draw(&mut self, renderer: &mut Renderer, destination: Rect) -> SdlResult<()> {
        let native = sys::sys_rect {
            x: destination.x,
            y: destination.y,
            w: destination.w,
            h: destination.h,
        };
        // SAFETY: the renderer and texture are both valid for the duration of the
        // call, and `native` is a live local of the layout SDL expects.
        let ok = unsafe {
            sys::SDL_RenderTexture(
                renderer.raw(),
                self.raw.as_ptr(),
                core::ptr::null(),
                &native,
            )
        };
        if ok {
            Ok(())
        } else {
            // SAFETY: the call just failed, so the error slot is ours to read.
            Err(unsafe { last_error() })
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `SDL_CreateTexture` and is destroyed
        // exactly once, here.
        unsafe { sys::SDL_DestroyTexture(self.raw.as_ptr()) };
    }
}

/// Sleeps for at least `milliseconds`.
///
/// The frontend uses this instead of spinning when the program is not running, so
/// that a paused debugger does not occupy a core. A spin would be faster and would
/// also make a laptop fan turn on to show nothing changing.
pub fn delay(milliseconds: u32) {
    // SAFETY: a plain C call taking one integer, with no pointer arguments.
    unsafe { sys::SDL_Delay(milliseconds) };
}

/// The next input event, or `None` if there is none waiting.
///
/// # Errors
///
/// If SDL reports a failure. SDL3 has no way to fail this call today, so the
/// error arm exists for a future version that can, rather than being left as a
/// case nobody thought about.
pub fn poll_event() -> SdlResult<Option<Input>> {
    let mut slot = sys::SdlEvent {
        // Written by `SDL_PollEvent` immediately; the padding arm is the one
        // initialised so the buffer's size is right, and `type` is set by SDL.
        padding: [0; EVENT_BUFFER_SIZE],
    };
    // SAFETY: `event` is a live local of at least `sizeof(SDL_Event)` bytes, and
    // `SDL_PollEvent` writes exactly one event into it or nothing at all.
    let pending = unsafe { sys::SDL_PollEvent(&mut slot) };
    if !pending {
        return Ok(None);
    }
    // SAFETY: SDL wrote an event, so reading the union's `type` member is valid —
    // it is the member every event struct begins with. The other arms are only
    // read after the type has been matched against the arm that is live, which is
    // what makes reading a union field sound.
    let event_type = unsafe { slot.event_type };
    let input = match event_type {
        EVENT_QUIT => Some(Input::Quit),
        EVENT_KEY_DOWN => {
            // SAFETY: the type says this is a keyboard event, so the union's `key`
            // arm is the live one and reading it is reading the event SDL wrote.
            // Each field is read separately because they are separate fields.
            let scancode = unsafe { slot.key.scancode };
            // SAFETY: as above, for the keycode.
            let key = unsafe { slot.key.key };
            // SAFETY: as above, for the repeat flag.
            let repeat = unsafe { slot.key.repeat };
            Some(Input::KeyDown {
                scancode,
                key,
                repeat,
            })
        }
        _ => None,
    };
    Ok(input)
}

/// A NUL-terminated string, for handing a title to SDL.
struct CString(Vec<u8>);

impl CString {
    /// A copy of `text` with a NUL appended.
    ///
    /// An interior NUL becomes a truncation at that point rather than an error,
    /// because a window title with a NUL in it is a caller's mistake that should
    /// not stop a debugger from opening.
    fn new(text: &str) -> Self {
        let mut bytes = text.as_bytes().to_vec();
        let cut = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        bytes.truncate(cut);
        bytes.push(0);
        Self(bytes)
    }

    /// The NUL-terminated bytes, for an FFI call.
    fn as_ptr(&self) -> *const c_char {
        self.0.as_ptr().cast::<c_char>()
    }
}
