//! B7: the display backend boundary, and the display profiles of §28.
//!
//! # Why a display needs a backend at all, when it is pulled and not pushed
//!
//! A block device has a backend because a guest's register write *goes somewhere*: the
//! data has to reach the host's storage during the write, so the device needs a way to
//! reach it. A display does not. A guest writes pixels into ordinary memory and then
//! rings a present register; the frame is a **description** — a width, a height and an
//! address — and the host decides when to go and look. Nothing happens during the
//! register write.
//!
//! That difference is load-bearing, and it is why `DisplayDevice` holds **no** backend
//! and this trait is fed by the host instead:
//!
//! - if the device called a backend during `present`, a guest's register write would
//!   call into the host synchronously. A slow or blocked host — a window the user
//!   dragged, a compositor that stopped answering — would stall the machine from a
//!   single guest instruction, with no fault and no timeout;
//! - pulling means the host renders on its own schedule, and a machine whose display
//!   nobody is watching costs nothing.
//!
//! So the boundary is the mirror image of B5's: the backend is the host's, the device
//! is the guest's, and the two meet in a resolved frame.
//!
//! # What §28 asked for, and what exists
//!
//! §28 names three architectures — a native Lazalith display, a VGA-compatible one,
//! and a modern framebuffer — and says the host may use SDL3 while the guest never
//! depends on it. [`DisplayProfile`] names all three so a machine can say which it is,
//! and **only the native one is constructible**: the other two belong to the
//! compatibility machine (`lza64-at-v1`, B27) and describing them as buildable would be
//! a lie a caller finds out about at boot time. The same treatment B4 gave
//! `lza64-at-v1`.
//!
//! For Linux 0.01, §28 says VGA/EGA support "must be researched from historical
//! primary sources before implementation". That research has not been done, so
//! [`DisplayProfile::VgaCompatible`] carries no register map and no behaviour. What
//! carries is the refusal.

use alloc::{string::String, vec::Vec};
use core::fmt;

use lazalith_types::PhysicalAddress;

use crate::display::{DisplayDevice, DisplayError, PresentedFrame};

/// A resolved frame: geometry plus the pixels, already read out of guest memory.
///
/// **Bytes, not a window.** A backend is handed a guest pixel format it did not choose,
/// and converting it is the backend's business. Handing a backend a window type instead
/// would put the guest's format in the window's signature, and a guest that changed
/// format would become a change to every host.
#[derive(Clone, Copy, Debug)]
pub struct DisplayFrame<'a> {
    /// Pixels across.
    pub width: u64,
    /// Pixels down.
    pub height: u64,
    /// How many frames the guest has presented, this being one of them.
    pub present_count: u64,
    /// `width * height * PIXEL_BYTES` bytes of guest pixel data.
    pub pixels: &'a [u8],
}

impl<'a> DisplayFrame<'a> {
    /// Builds a frame, checking that the pixels are exactly the right number.
    ///
    /// The length check is here rather than in each backend because a backend that
    /// received a short slice would either read past it or render garbage, and both
    /// would be reported as the backend's fault.
    pub fn new(
        width: u64,
        height: u64,
        present_count: u64,
        pixels: &'a [u8],
    ) -> Result<Self, DisplayError> {
        let expected = frame_len(width, height);
        if pixels.len() as u64 != expected {
            return Err(DisplayError::ShortFrameRead {
                expected,
                found: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            present_count,
            pixels,
        })
    }
}

/// How many bytes a frame of this geometry is.
pub const fn frame_len(width: u64, height: u64) -> u64 {
    width
        .saturating_mul(height)
        .saturating_mul(crate::display::PIXEL_BYTES)
}

/// Why a display backend refused or failed.
///
/// **Not [`DisplayError`], and the reason is a derive.** `DisplayError` is `Copy` and
/// every variant of it is a fact about the *device* — a geometry that is too large, a
/// register that is read-only — that a caller might reasonably hold on to and pass
/// around. A backend failure is a fact about the *host*: SDL's own error text, a window
/// that could not be opened, a texture upload that failed. That text is a `String`, so
/// putting it in `DisplayError` would make that error non-`Copy` and give every
/// structural device refusal a heap allocation it does not need.
///
/// So the two are separate types, and a backend that wants to report a device refusal
/// converts with `From<DisplayError>`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayBackendError {
    /// A structural refusal from the display device, unchanged.
    Device(DisplayError),
    /// The host backend failed, with its own words.
    Host {
        /// Which step failed, in this project's words.
        operation: &'static str,
        /// What the host said.
        detail: String,
    },
}

impl fmt::Display for DisplayBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(source) => write!(f, "{source}"),
            Self::Host { operation, detail } => {
                write!(f, "the display backend could not {operation}: {detail}")
            }
        }
    }
}

impl core::error::Error for DisplayBackendError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            Self::Host { .. } => None,
        }
    }
}

impl From<DisplayError> for DisplayBackendError {
    fn from(source: DisplayError) -> Self {
        Self::Device(source)
    }
}

/// What a host does with the frames a guest presents.
///
/// **`no_std` and `Debug`, and neither is decoration.** `lazalith-devices` is
/// `no_std` because it runs on the ISA target, so a backend here cannot open a window —
/// `Sdl3DisplayBackend` lives in `lazalith-sdl3` and implements this trait. That is the
/// same split as B5's: a file backend is a host-side implementation of a trait the
/// device crate declares, not a new method on the device.
pub trait DisplayBackend: fmt::Debug {
    /// A window of this geometry is about to be used.
    fn open(&mut self, width: u64, height: u64) -> Result<(), DisplayBackendError>;

    /// This frame should be shown.
    fn present(&mut self, frame: &DisplayFrame<'_>) -> Result<(), DisplayBackendError>;

    /// The window is gone.
    fn close(&mut self);
}

/// A display architecture, as §28 names them.
///
/// Only [`DisplayProfile::Native`] builds. The other two exist so a machine can be
/// *described* as having one, which is not the same as being able to build it — the
/// distinction B4 drew for `lza64-at-v1`, and the reason a profile naming
/// `VgaCompatible` is refused with a fact a caller can act on rather than built into
/// something that cannot work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum DisplayProfile {
    /// The platform's own display: a framebuffer, a geometry, and a present counter.
    Native,
    /// A VGA-compatible display, for the AT machine and the Linux port.
    ///
    /// **No register map.** §28 requires VGA/EGA to be researched from historical
    /// primary sources first, and that has not happened. Carrying a guessed
    /// CRTC/palette/controller model here would be worse than carrying nothing: it
    /// would look like an implementation.
    VgaCompatible,
    /// A modern framebuffer: no legacy compatibility modes, more colours.
    ModernFramebuffer,
}

impl DisplayProfile {
    /// Whether this build can construct a display of this profile.
    pub const fn is_constructible(self) -> bool {
        matches!(self, Self::Native)
    }

    /// The name, for a diagnostic.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::VgaCompatible => "vga",
            Self::ModernFramebuffer => "modern-framebuffer",
        }
    }
}

impl fmt::Display for DisplayProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What happened when the host looked at a display.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayPump {
    /// The guest has not presented a frame since the last pump.
    Nothing,
    /// A frame reached the backend.
    Presented {
        /// How many frames the guest has now presented in total.
        present_count: u64,
    },
    /// A window is open but the framebuffer could not be read.
    Unreadable {
        /// The address that could not be read.
        address: PhysicalAddress,
    },
}

/// Reads the frame a guest has presented and hands it to a backend.
///
/// **The host calls this, on the host's schedule.** That is the whole of the pull model
/// and it is why this is a free function rather than a method on `DisplayDevice`: the
/// device is the guest's, and a function the host calls is the host's.
///
/// `read` fetches the bytes at a physical address. It returns `None` for an address it
/// cannot read — a framebuffer outside the machine's memory, or a region the guest has
/// no access to — which is reported as [`DisplayPump::Unreadable`] rather than as a
/// backend failure, because the backend was never at fault.
pub fn pump_display<R>(
    device: &mut DisplayDevice,
    backend: &mut dyn DisplayBackend,
    mut read: R,
) -> Result<DisplayPump, DisplayBackendError>
where
    R: FnMut(PhysicalAddress, u64) -> Option<Vec<u8>>,
{
    let Some(presented) = device.presented() else {
        return Ok(DisplayPump::Nothing);
    };
    if !device.is_open() {
        return Ok(DisplayPump::Nothing);
    }
    let byte_count = frame_len(presented.width, presented.height);
    let address = PhysicalAddress::new(presented.address);
    let Some(pixels) = read(address, byte_count) else {
        return Ok(DisplayPump::Unreadable { address });
    };
    let frame = DisplayFrame::new(
        presented.width,
        presented.height,
        presented.present_count,
        &pixels,
    )?;
    backend.open(presented.width, presented.height)?;
    backend.present(&frame)?;
    Ok(DisplayPump::Presented {
        present_count: presented.present_count,
    })
}

/// Opens a window, if the device has one to show.
///
/// Separate from [`pump_display`] because a host may want a window the moment the
/// guest opens it rather than at the first present, and because a window opened here
/// and a frame presented there must not disagree about geometry.
pub fn open_window(
    device: &mut DisplayDevice,
    backend: &mut dyn DisplayBackend,
) -> Result<Option<(u64, u64)>, DisplayBackendError> {
    if !device.is_open() {
        return Ok(None);
    }
    let (width, height) = (device.width(), device.height());
    backend.open(width, height)?;
    Ok(Some((width, height)))
}

/// A display backend that shows nothing and remembers everything.
///
/// **The test and headless backend, and a real one.** A machine with no window — a CI
/// job, a test, a headless server — still has a display device, and a guest that
/// presents frames must not be told its display is broken. This one accepts every
/// frame and records the last geometry and a checksum of the pixels, so a test can
/// assert *what was drawn* rather than only that something was.
///
/// The checksum is FNV-1a, chosen because it is four lines and has no parameters to
/// get wrong. It is a fingerprint, not a cryptographic hash, and is documented as one.
#[derive(Debug, Default)]
pub struct HeadlessDisplayBackend {
    width: u64,
    height: u64,
    open: bool,
    last_count: u64,
    last_checksum: u64,
    frames: u64,
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

impl HeadlessDisplayBackend {
    /// A backend that has shown nothing.
    pub const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            open: false,
            last_count: 0,
            last_checksum: 0,
            frames: 0,
        }
    }

    /// The geometry of the window, or `None` if none is open.
    pub const fn window(&self) -> Option<(u64, u64)> {
        if self.open {
            Some((self.width, self.height))
        } else {
            None
        }
    }

    /// How many frames this backend has been handed.
    pub const fn frames(&self) -> u64 {
        self.frames
    }

    /// The present count of the last frame.
    pub const fn last_present_count(&self) -> u64 {
        self.last_count
    }

    /// An FNV-1a fingerprint of the last frame's pixels.
    ///
    /// A fingerprint, not a hash for security: it detects "the screen changed" and
    /// "the screen is what it was", and says nothing about an adversary.
    pub const fn last_checksum(&self) -> u64 {
        self.last_checksum
    }
}

impl DisplayBackend for HeadlessDisplayBackend {
    fn open(&mut self, width: u64, height: u64) -> Result<(), DisplayBackendError> {
        DisplayDevice::validate_geometry(width, height)?;
        self.width = width;
        self.height = height;
        self.open = true;
        Ok(())
    }

    fn present(&mut self, frame: &DisplayFrame<'_>) -> Result<(), DisplayBackendError> {
        if !self.open {
            return Err(DisplayError::NoWindow.into());
        }
        if frame.width != self.width || frame.height != self.height {
            // A backend whose window was opened at one geometry and handed a frame of
            // another is being lied to, and accepting it would mean the guest had
            // changed its geometry without a new `open`.
            DisplayDevice::validate_geometry(frame.width, frame.height)?;
            return Err(DisplayBackendError::Device(
                DisplayError::WindowGeometryMismatch {
                    window: (self.width, self.height),
                    frame: (frame.width, frame.height),
                },
            ));
        }
        let mut checksum = FNV_OFFSET;
        for byte in frame.pixels {
            checksum ^= u64::from(*byte);
            checksum = checksum.wrapping_mul(FNV_PRIME);
        }
        self.last_checksum = checksum;
        self.last_count = frame.present_count;
        self.frames = self.frames.saturating_add(1);
        Ok(())
    }

    fn close(&mut self) {
        self.open = false;
        self.width = 0;
        self.height = 0;
    }
}

/// The frame a display device last presented, without resolving its pixels.
///
/// For a caller that only wants the geometry — a debugger showing "the guest has a
/// 640x480 window" — and must not allocate to say so.
pub fn presented_geometry(device: &DisplayDevice) -> Option<(u64, u64)> {
    let presented: Option<PresentedFrame> = device.presented();
    presented.map(|frame| (frame.width, frame.height))
}
