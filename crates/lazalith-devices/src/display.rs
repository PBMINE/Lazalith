//! The virtual display device.
//!
//! # The guest owns the framebuffer
//!
//! This is the whole design, and everything else follows from it. The device does
//! **not** keep a private copy of the pixels: a `present` is a synchronisation
//! point, not an upload, because the device and the guest are looking at the same
//! memory. A device that stored its own copy would make every frame a transfer of
//! `width * height * 4` bytes and would make the guest's writes and the device's
//! view able to disagree — which is the failure mode this design exists to remove.
//!
//! So the device owns two things and no pixels:
//!
//! - the **geometry**: the width and height the window was opened at, and
//! - the **present counter**: how many times the guest has asked for a frame.
//!
//! The pixels are a region of guest RAM the driver hands to the device as an
//! address. `DisplayDevice::present_at` records that the frame at *that address*
//! is now the visible one, and a reader takes the address and the geometry and
//! reads the pixels out of the machine's memory.
//!
//! # Why there is no SDL3 here
//!
//! Because there must not be. A device that knew about a host window would be a
//! device that could not run headless, could not be recorded deterministically,
//! and could not be tested without a display server. The host frontend in Step 77
//! reads the presented framebuffer and puts it in a window; the device never finds
//! out.
//!
//! # The register surface
//!
//! Devices are addressed by MMIO, and this one is eight double-words: the four
//! the guest writes to configure and present, and four it reads to learn what the
//! device knows. Every field has a fixed offset and a fixed width, and every access
//! is bounds checked, so a program that pokes a register it should not still gets
//! a typed refusal rather than a silent no-op.

use alloc::vec::Vec;
use core::fmt;

use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceOffset};

use crate::{Device, DeviceError};

/// The number of bytes one pixel occupies, in the one format the ABI defines.
///
/// ARGB8888: byte 0 is alpha, 1 red, 2 green, 3 blue. The format is fixed and
/// versioned with the ABI, so a driver and a program cannot disagree about it —
/// which is the alternative to a format field, and the reason there is no format
/// field.
pub const PIXEL_BYTES: u64 = 4;

/// The first register: the width in pixels.
pub const REGISTER_WIDTH: DeviceOffset = DeviceOffset::new(0);
/// The second register: the height in pixels.
pub const REGISTER_HEIGHT: DeviceOffset = DeviceOffset::new(8);
/// The third register: the framebuffer's address in guest physical memory.
///
/// This is where the guest-owned pixels live. The device records the address and
/// never copies from it; a reader of a presented frame resolves the address
/// against the machine's memory.
pub const REGISTER_FRAMEBUFFER: DeviceOffset = DeviceOffset::new(16);
/// The fourth register: writing it presents the frame at the current address.
///
/// The value is ignored. A present is a *synchronisation point*, and a
/// command-with-no-argument is the honest shape for that: a value here would
/// suggest the device interprets it, and it does not.
pub const REGISTER_PRESENT: DeviceOffset = DeviceOffset::new(24);
/// The fifth register: how many frames have been presented.
pub const REGISTER_PRESENT_COUNT: DeviceOffset = DeviceOffset::new(32);
/// The sixth register: the last address that was presented, or 0 for none.
pub const REGISTER_LAST_PRESENT: DeviceOffset = DeviceOffset::new(40);
/// The seventh register: the ABI version the device implements.
///
/// A program reads this before it trusts any other register. Without it, a driver
/// built for a different framebuffer layout would read plausible numbers from
/// registers that mean something else.
pub const REGISTER_ABI_VERSION: DeviceOffset = DeviceOffset::new(48);
/// The eighth register: the device's own status, as a bit set.
///
/// Bit 0 is set once a frame has been presented, so a program can tell "never
/// presented" from "presented a frame that happened to be blank".
pub const REGISTER_STATUS: DeviceOffset = DeviceOffset::new(56);

/// The ABI version this device implements.
pub const DISPLAY_ABI_VERSION: u64 = 1;

/// `REGISTER_STATUS` bit 0: at least one frame has been presented.
pub const STATUS_PRESENTED: u64 = 1;

/// How many bytes of register space the device occupies.
pub const REGISTER_BYTES: u64 = 64;

/// The largest width or height a window may be opened at.
///
/// The bound is on the *product*, not on each dimension, because the framebuffer
/// is `width * height * 4` bytes and that product is what has to fit in the
/// address space a `u32` index can name. Refusing at the device means a program
/// gets a refusal it can act on rather than a framebuffer that wraps.
pub const MAX_DIMENSION: u64 = 4096;

/// The virtual display device.
#[derive(Debug)]
pub struct DisplayDevice {
    width: u64,
    height: u64,
    framebuffer: u64,
    present_count: u64,
    last_present: u64,
    elapsed: CycleCount,
}

/// What a caller learns from a presented frame.
///
/// The device hands back an *address*, not pixels. Copying the pixels out would
/// make the device a second copy of the framebuffer, which is the thing this
/// design refuses to be. A caller that wants the pixels resolves the address
/// against the machine's memory and reads them there — the same bytes the guest
/// wrote, which is the property the whole design is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentedFrame {
    /// The framebuffer's address in guest physical memory.
    pub address: u64,
    /// The width in pixels.
    pub width: u64,
    /// The height in pixels.
    pub height: u64,
    /// How many times a frame had been presented when this one was.
    pub present_count: u64,
}

impl PresentedFrame {
    /// How many bytes this frame occupies.
    ///
    /// This is `width * height * 4`, and it is the length of the region the
    /// address names. It is a method rather than a field because the product can
    /// overflow a `u64` for a caller that supplies its own dimensions, and a
    /// device that has already validated them cannot.
    pub fn bytes(&self) -> Option<u64> {
        self.width
            .checked_mul(self.height)?
            .checked_mul(PIXEL_BYTES)
    }
}

impl DisplayDevice {
    /// A display with no window open.
    pub const fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            framebuffer: 0,
            present_count: 0,
            last_present: 0,
            elapsed: CycleCount::new(0),
        }
    }

    /// The window's width in pixels, or 0 when no window is open.
    pub const fn width(&self) -> u64 {
        self.width
    }

    /// The window's height in pixels, or 0 when no window is open.
    pub const fn height(&self) -> u64 {
        self.height
    }

    /// The framebuffer's address, or 0 when no window is open.
    pub const fn framebuffer(&self) -> u64 {
        self.framebuffer
    }

    /// How many frames have been presented.
    pub const fn present_count(&self) -> u64 {
        self.present_count
    }

    /// The last presented frame, or `None` when nothing has been presented.
    ///
    /// A window that was opened is not a frame: until `present` is called there
    /// is nothing on the screen, and saying otherwise would let a program report
    /// a frame it never drew.
    pub fn presented(&self) -> Option<PresentedFrame> {
        if self.present_count == 0 {
            return None;
        }
        Some(PresentedFrame {
            address: self.last_present,
            width: self.width,
            height: self.height,
            present_count: self.present_count,
        })
    }

    /// Opens a window of `width` by `height` over the framebuffer at `address`.
    ///
    /// This is the Rust-side door the driver uses, and it validates the geometry
    /// the register interface also validates — one check, called from both, so
    /// the two cannot come to disagree about what a legal window is.
    pub fn open(&mut self, width: u64, height: u64, address: u64) -> Result<(), DisplayError> {
        Self::validate_geometry(width, height)?;
        if address == 0 {
            // A framebuffer at address 0 is not a framebuffer: the guest's own
            // memory starts there, so a window there would be drawn over
            // whatever the machine keeps at the bottom of the address space.
            return Err(DisplayError::NullFramebuffer);
        }
        self.width = width;
        self.height = height;
        self.framebuffer = address;
        Ok(())
    }

    /// Whether a window is open.
    pub const fn is_open(&self) -> bool {
        self.width != 0 && self.height != 0
    }

    /// Presents the current framebuffer.
    ///
    /// A present with no window open is refused rather than counted. The counter
    /// is what a program uses to know a frame reached the screen, and a count
    /// that included presents of nothing would be a lie about that.
    pub fn present(&mut self) -> Result<(), DisplayError> {
        if !self.is_open() {
            return Err(DisplayError::NoWindow);
        }
        self.last_present = self.framebuffer;
        self.present_count = self.present_count.saturating_add(1);
        Ok(())
    }

    /// Closes the window.
    ///
    /// The presented count is kept: a program that closed its window can still say
    /// how many frames it showed, and forgetting that would make the counter mean
    /// "frames since the last open" rather than "frames shown".
    pub fn close(&mut self) {
        self.width = 0;
        self.height = 0;
        self.framebuffer = 0;
    }

    /// Whether a geometry is one this device can open a window at.
    pub fn validate_geometry(width: u64, height: u64) -> Result<(), DisplayError> {
        if width == 0 || height == 0 {
            return Err(DisplayError::EmptyWindow);
        }
        if width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(DisplayError::WindowTooLarge {
                width,
                height,
                limit: MAX_DIMENSION,
            });
        }
        // The product is computed here even though the caller may never need it,
        // because `width * height * 4` overflowing a `u64` is the case where a
        // window would be "opened" with a framebuffer that cannot be addressed.
        width
            .checked_mul(height)
            .and_then(|p| p.checked_mul(PIXEL_BYTES))
            .ok_or(DisplayError::FramebufferOverflow { width, height })?;
        Ok(())
    }

    /// A register's value, for a read.
    fn register(&self, offset: DeviceOffset) -> u64 {
        match offset.as_u64() {
            0 => self.width,
            8 => self.height,
            16 => self.framebuffer,
            24 => 0,
            32 => self.present_count,
            40 => self.last_present,
            48 => DISPLAY_ABI_VERSION,
            56 => {
                if self.present_count == 0 {
                    0
                } else {
                    STATUS_PRESENTED
                }
            }
            _ => 0,
        }
    }

    /// A write to a register, if it is one a guest may write.
    ///
    /// The geometry registers are read-only, and the reason is not caution: a
    /// framebuffer is sized for a *pair* of dimensions, so accepting one half
    /// would leave the device describing a region that does not exist. `open` is
    /// the only way to set geometry, and it validates both halves together.
    fn write_register(&mut self, offset: DeviceOffset, value: u64) -> Result<(), DisplayError> {
        match offset.as_u64() {
            0 | 8 => Err(DisplayError::ReadOnlyRegister(offset)),
            16 => {
                // The address may be changed, because a program that moves its
                // framebuffer needs to say so. The geometry check belongs to
                // `open`, and changing only the address cannot make a framebuffer
                // the wrong size.
                if value == 0 {
                    return Err(DisplayError::NullFramebuffer);
                }
                self.framebuffer = value;
                Ok(())
            }
            24 => self.present(),
            32 | 40 | 48 | 56 => Err(DisplayError::ReadOnlyRegister(offset)),
            // A write past the last register never reaches here: `write` validates
            // the range first, and this arm exists so a future register added to
            // the match is not silently accepted.
            _ => Err(DisplayError::ReadOnlyRegister(offset)),
        }
    }
}

impl Default for DisplayDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for DisplayDevice {
    fn address_len(&self) -> u64 {
        REGISTER_BYTES
    }

    fn reset(&mut self) {
        // A reset closes the window and forgets the frames. It deliberately does
        // *not* zero the count on its own: `reset` is called at machine start,
        // where there has been no window, and a device that remembered a count
        // across a reset would report frames for a window that never existed.
        *self = Self::new();
    }

    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        Self::validate_register_access(offset, size)
    }

    fn validate_write(
        &self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        Self::validate_register_access(offset, size)?;
        // The write is performed on a copy and thrown away, so a rejected write
        // leaves the device exactly as it was. This is the validate-before-mutate
        // rule applied to a device whose registers are not a flat byte range: a
        // `present` with no window open would otherwise have to be undone.
        let mut trial = DisplayDevice {
            width: self.width,
            height: self.height,
            framebuffer: self.framebuffer,
            present_count: self.present_count,
            last_present: self.last_present,
            elapsed: self.elapsed,
        };
        // A refused write is reported as the device refusing it. The *reason* is
        // in the `DisplayError` for a caller using `write_register` directly, and
        // the `Device` trait has one error type, so the specific complaint becomes
        // the trait's own "write unsupported" rather than being invented here.
        if trial.write_register(offset, value).is_err() {
            return Err(DeviceError::WriteUnsupported);
        }
        Ok(())
    }
    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError> {
        self.validate_read(offset, size)?;
        Ok(self.register(offset))
    }

    fn write(
        &mut self,
        offset: DeviceOffset,
        size: DataSize,
        value: u64,
    ) -> Result<(), DeviceError> {
        self.validate_write(offset, size, value)?;
        if self.write_register(offset, value).is_err() {
            return Err(DeviceError::WriteUnsupported);
        }
        Ok(())
    }

    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError> {
        self.validate_read(offset, DataSize::Double)?;
        // The output must be exactly the register's width. Copying what fits would
        // let a caller read the low half of a value and believe it had read the
        // whole thing — and a debugger that silently shows half a framebuffer
        // address is worse than one that refuses.
        if output.len() != DataSize::Double.bytes() as usize {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: output.len() as u64,
            });
        }
        output.copy_from_slice(&self.register(offset).to_le_bytes());
        Ok(())
    }

    fn tick(&mut self, elapsed: CycleCount) {
        self.elapsed = elapsed;
    }
}

impl DisplayDevice {
    /// Every register access is a whole double-word at a fixed offset.
    ///
    /// This returns a `DeviceError` because it is the `Device` trait's own error,
    /// and a register range that is wrong is wrong for the same reason on every
    /// device — not a display-specific complaint.
    fn validate_register_access(offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError> {
        if size != DataSize::Double {
            return Err(DeviceError::UnsupportedSize(size));
        }
        if !offset.as_u64().is_multiple_of(8) || offset.as_u64() >= REGISTER_BYTES {
            return Err(DeviceError::InvalidRange {
                offset,
                bytes: u64::from(size.bytes()),
            });
        }
        Ok(())
    }
}

/// Why a display operation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayError {
    /// A window of zero width or height was asked for.
    EmptyWindow,
    /// A window larger than [`MAX_DIMENSION`] in either direction was asked for.
    WindowTooLarge {
        /// The width that was asked for.
        width: u64,
        /// The height that was asked for.
        height: u64,
        /// The largest dimension allowed.
        limit: u64,
    },
    /// The framebuffer for this geometry does not fit an addressable size.
    FramebufferOverflow {
        /// The width that was asked for.
        width: u64,
        /// The height that was asked for.
        height: u64,
    },
    /// A framebuffer address of zero was given.
    NullFramebuffer,
    /// A present or geometry query with no window open.
    NoWindow,
    /// A register that only the device may write was written.
    ReadOnlyRegister(DeviceOffset),
}

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWindow => write!(f, "a window needs a width and a height above zero"),
            Self::WindowTooLarge {
                width,
                height,
                limit,
            } => write!(
                f,
                "a {width} by {height} window is larger than the {limit} pixel limit"
            ),
            Self::FramebufferOverflow { width, height } => {
                write!(
                    f,
                    "a {width} by {height} framebuffer is too large to address"
                )
            }
            Self::NullFramebuffer => {
                write!(f, "a framebuffer cannot start at address zero")
            }
            Self::NoWindow => write!(f, "no window is open"),
            Self::ReadOnlyRegister(offset) => {
                write!(f, "register {} is written by the device", offset.as_u64())
            }
        }
    }
}

impl core::error::Error for DisplayError {}

/// A display device with a window already open, for a caller that wants one
/// expression rather than three calls.
impl DisplayDevice {
    /// Opens a window and presents its first frame in one step.
    ///
    /// A first present is what a program means by "open a window": a window that
    /// exists but has never been presented is a window whose contents nobody has
    /// seen, and a caller counting frames would otherwise have to present before
    /// it could claim to have shown anything.
    pub fn open_and_present(
        &mut self,
        width: u64,
        height: u64,
        address: u64,
    ) -> Result<PresentedFrame, DisplayError> {
        self.open(width, height, address)?;
        self.present()?;
        self.presented().ok_or(DisplayError::NoWindow)
    }
}

/// The bytes a presented frame occupies, read out of a memory region.
///
/// This is a *function* rather than a method on the device because reading the
/// pixels needs the machine's memory, and the device does not have it — the
/// device's whole claim is that it does not need it. The address comes from the
/// device and the bytes come from RAM, and this is where the two meet.
///
/// `memory` is the region *as the framebuffer address indexes it*: a caller
/// resolving a frame inside a larger address space passes the whole space, and a
/// caller with only the framebuffer passes just that, in which case the frame's
/// address is its offset into what was passed. The device never resolves the
/// address itself, so this choice belongs to whoever holds the memory.
pub fn frame_bytes<'a>(frame: &PresentedFrame, memory: &'a [u8]) -> Option<&'a [u8]> {
    let length = frame.bytes()?;
    let start = usize::try_from(frame.address).ok()?;
    let end = start.checked_add(usize::try_from(length).ok()?)?;
    memory.get(start..end)
}

/// The pixel at (`x`, `y`) of a presented frame, as four bytes in ABI order.
///
/// Returns `None` for a coordinate outside the frame, because a pixel outside the
/// framebuffer is not a pixel with a default value — it is no pixel, and a
/// caller that drew one has a bug the default would hide.
pub fn pixel_at(frame: &PresentedFrame, memory: &[u8], x: u64, y: u64) -> Option<[u8; 4]> {
    if x >= frame.width || y >= frame.height {
        return None;
    }
    let row = y.checked_mul(frame.width)?.checked_add(x)?;
    let at = row.checked_mul(PIXEL_BYTES)?;
    let start = usize::try_from(frame.address)
        .ok()?
        .checked_add(usize::try_from(at).ok()?)?;
    let bytes = memory.get(start..start + 4)?;
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// A framebuffer of `width` by `height` zeroed pixels, for a caller that needs a
/// region to point a window at.
///
/// This is host-side setup, not device state: it is the memory a guest will own,
/// and allocating it here is how a test or a driver prepares it.
pub fn zeroed_framebuffer(width: u64, height: u64) -> Option<Vec<u8>> {
    let length = width.checked_mul(height)?.checked_mul(PIXEL_BYTES)?;
    let length = usize::try_from(length).ok()?;
    Some(alloc::vec![0u8; length])
}
