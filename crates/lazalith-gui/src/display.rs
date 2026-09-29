//! B7: the SDL3 display backend.
//!
//! §28: "The host may use Rust sdl3 as the rendering backend. The guest never depends
//! on SDL3." This file is the second half of that sentence made structural — the
//! guest-visible `DisplayDevice` knows nothing about this module, and this module knows
//! nothing about a guest except the bytes a frame carries.
//!
//! # Why this is in `lazalith-sdl3` and not `lazalith-devices`
//!
//! `lazalith-devices` is `no_std` because it runs on the ISA target, and a window is a
//! host resource. So `DisplayBackend` is declared in `lazalith-devices` and implemented
//! here, in the one crate that is allowed `unsafe` and is allowed to talk to a C
//! library. This is the same split B5 chose for storage: a trait the device crate
//! declares, a backend the host crate provides.
//!
//! # The conversion
//!
//! A guest's pixel format is not SDL's, and the conversion is here rather than in the
//! device so that changing the guest format is a change in one function. The format is
//! 4 bytes per pixel, little-endian, in `0xAARRGGBB` order — see
//! `lazalith_devices::display` — and SDL is given `0xAARRGGBB` directly, so for the
//! native format the conversion is a byte-for-byte copy and only a *different* guest
//! format would need work here.
//!
//! That is worth being explicit about rather than writing a `to_rgba` that looks
//! general: this backend declares the format it handles, and [`Sdl3DisplayBackend`]
//! documents it. A second guest format means a second backend, not a branch here.

use lazalith_devices::{
    DisplayBackend, DisplayBackendError, DisplayDevice, DisplayError, DisplayFrame, PIXEL_BYTES,
};

use lazalith_sdl3::{Color, Rect, Renderer, SdlError, SdlResult, Texture, Video, Window};

/// A display backend that draws into an SDL window.
///
/// Owns the window, the renderer and the texture, and recreates them when the guest
/// changes geometry. The recreation is the interesting part: a guest that resizes its
/// framebuffer is doing something real, and a backend that refused would make a
/// legitimate program fail. So the window follows the guest, and
/// `HeadlessDisplayBackend` — which *does* refuse — is the one that exists to catch a
/// geometry that changed without being asked for.
pub struct Sdl3DisplayBackend {
    window: Window,
    renderer: Renderer,
    texture: Option<Texture>,
    width: u32,
    height: u32,
    frames: u64,
}

/// Hand-written because the SDL wrappers are not `Debug`.
///
/// They hold a raw pointer, and printing a pointer is both useless and a small
/// information leak into any log a caller writes. So this prints what is actually
/// interesting about a display backend 2014 its geometry, its frame count, and whether a
/// texture is live 2014 and nothing about the pointers.
impl core::fmt::Debug for Sdl3DisplayBackend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Sdl3DisplayBackend")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("frames", &self.frames)
            .field("texture", &self.texture.is_some())
            .finish()
    }
}

impl Sdl3DisplayBackend {
    /// Opens a window for a display of this geometry.
    ///
    /// # Errors
    ///
    /// Returns SDL's own error text if the window, the renderer or the initial texture
    /// cannot be created. A headless machine has no video device, and this is where that
    /// surfaces — as an error a caller can report rather than as a black window.
    pub fn new(video: &Video, title: &str, width: u32, height: u32) -> SdlResult<Self> {
        DisplayDevice::validate_geometry(u64::from(width), u64::from(height)).map_err(|error| {
            SdlError {
                message: error.to_string(),
            }
        })?;
        let window = Window::new(video, title, width, height)?;
        let renderer = Renderer::new(&window)?;
        let texture = Texture::new(&renderer, width, height)?;
        Ok(Self {
            window,
            renderer,
            texture: Some(texture),
            width,
            height,
            frames: 0,
        })
    }

    /// How many frames have been drawn.
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// The window.s current size.
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The SDL window this backend draws into.
    ///
    /// **And the field is not dead weight**, which is worth saying because a reader
    /// will otherwise delete it and break the backend in a way that compiles: the
    /// `Window` value owns the `SDL_Window` and destroys it in `Drop`, and the renderer
    /// and texture are built *from* it. Dropping this field would destroy the window
    /// out from under both of them, and SDL would have a dangling renderer. Keeping it
    /// is what makes the order of destruction correct.
    ///
    /// This is a host accessor for a host resource, which is the opposite direction
    /// from B5.s boundary: that rule is that a *device* must not expose its *backend*.
    /// A backend exposing its own window reaches no guest-visible state.
    pub const fn window(&self) -> &Window {
        &self.window
    }

    /// Rebuilds the texture for a new geometry.
    ///
    /// SDL textures are fixed size, so a resize means a new one. The old one is dropped
    /// with the old `Texture` value, which is the whole of this crate's pointer
    /// ownership story: there is no other way to make a texture, and the value that
    /// makes one destroys it.
    fn resize(&mut self, width: u64, height: u64) -> Result<(), DisplayBackendError> {
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return Err(DisplayBackendError::Host {
                operation: "address a window of that size",
                detail: String::from(
                    "the guest asked for a window larger than this host can address",
                ),
            });
        };
        if width == self.width && height == self.height {
            return Ok(());
        }
        self.texture = Some(
            Texture::new(&self.renderer, width, height).map_err(|error| {
                DisplayBackendError::Host {
                    operation: "create a texture",
                    detail: error.to_string(),
                }
            })?,
        );
        self.width = width;
        self.height = height;
        Ok(())
    }
}

/// Turns an SDL failure into a backend failure that names the step.
///
/// A local function rather than a `From<SdlError> for DisplayBackendError` impl,
/// because that impl would be an orphan: `DisplayBackendError` belongs to
/// `lazalith-devices` and `SdlError` to `lazalith-sdl3`, so neither crate may write
/// it. The alternative — a `String` in the device crate's error and no typed host
/// error at all — would put host text into a structural type, which is what the split
/// was for.
fn host(operation: &'static str) -> impl Fn(SdlError) -> DisplayBackendError {
    move |error| DisplayBackendError::Host {
        operation,
        detail: error.to_string(),
    }
}

impl DisplayBackend for Sdl3DisplayBackend {
    fn open(&mut self, width: u64, height: u64) -> Result<(), DisplayBackendError> {
        // A guest may resize its window, and SDL textures are fixed size, so this is a
        // rebuild rather than a refusal. The geometry is still validated by the device
        // crate, so a nonsense size is refused before it reaches SDL.
        DisplayDevice::validate_geometry(width, height)?;
        self.resize(width, height)
    }

    fn present(&mut self, frame: &DisplayFrame<'_>) -> Result<(), DisplayBackendError> {
        if frame.pixels.len() as u64 != frame.width * frame.height * PIXEL_BYTES {
            return Err(DisplayError::FramebufferOverflow {
                width: frame.width,
                height: frame.height,
            }
            .into());
        }
        self.resize(frame.width, frame.height)?;
        let Some(texture) = self.texture.as_mut() else {
            return Err(DisplayError::NoWindow.into());
        };
        texture.upload(frame.pixels).map_err(host("upload"))?;
        let (width, height) = (self.width, self.height);
        texture
            .draw(
                &mut self.renderer,
                Rect::new(0.0, 0.0, width as f32, height as f32),
            )
            .map_err(host("draw"))?;
        self.renderer.present().map_err(host("present"))?;
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    fn close(&mut self) {
        self.texture = None;
    }
}

/// The framebuffer bytes of a guest frame, in the order SDL wants them.
///
/// A real function rather than an `upload` call, so the byte order is written down in
/// one place and can be asserted by a test without a window. The native guest format is
/// already `0xAARRGGBB` little-endian, which is SDL's `SDL_PIXELFORMAT_RGBA32`, so this
/// is a copy — and it is a copy *on purpose*, named, so that the day the guest format
/// changes this is the one function that has to.
pub fn framebuffer_bytes(pixels: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; pixels.len()];
    out.copy_from_slice(pixels);
    out
}

/// The clear colour a display backend starts with.
///
/// Black. Not "whatever was in the texture", because a texture SDL has just created has
/// undefined contents and a guest that opens a window and has not drawn yet would show
/// whatever the allocator left there.
pub const CLEAR: Color = Color {
    r: 0,
    g: 0,
    b: 0,
    a: 255,
};
