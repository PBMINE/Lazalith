//! The LazOS display driver.
//!
//! # Where this sits
//!
//! ```text
//! Lazen application
//!     ↓ std::graphics          (the SDK: pure Lazen)
//! LazOS display driver       ← this file
//!     ↓ display ABI            (display_open, display_present)
//! Virtual Display Device     (the device, which shares guest memory)
//!     ↓
//! guest-owned framebuffer in machine RAM
//! ```
//!
//! The driver is the *only* thing that talks to the display device, and it does so
//! on the application's behalf through the ABI. A Lazen program has no path to
//! the device: there is no syscall that returns a device address, and the frame
//! the program draws into is memory it allocated itself and named through
//! `display_open`. That is the whole of "do not bypass the OS" — there is nothing
//! to bypass it *with*.
//!
//! # What the driver does and does not copy
//!
//! Nothing. `display_open` takes the address of a framebuffer the *guest* already
//! owns and records it; `display_present` records that the frame at that address is
//! the visible one. The driver never reads a pixel and never writes one. A driver
//! that copied would make every present a transfer of `width * height * 4` bytes
//! and would let the device's view and the guest's memory disagree — which is the
//! failure `docs/lazen-graphics.md` exists to rule out.
//!
//! # Headless by construction
//!
//! This driver knows nothing about a host, a window, or a screen. It reports what
//! the device last presented, and a host frontend in Step 77 reads that and draws
//! it. A driver that knew about a host could not be tested without one.

use lazalith_devices::{DisplayDevice, DisplayError};
use lazalith_os_abi::{DisplayRecord, IoResult, SyscallError, SyscallStatus, TaggedOutcome};
use lazalith_types::{ArchitectureConfig, VirtualAddress};

use crate::syscall::{
    KernelService, ServiceOutcome, UserMemoryContext, ValidatedSyscall, ValidatedSyscallKind,
};

/// The display driver, sitting on a display device.
#[derive(Debug)]
pub struct DisplayService {
    device: DisplayDevice,
    architecture: ArchitectureConfig,
}

impl DisplayService {
    /// A driver for `architecture`, over a device with no window open.
    pub const fn new(architecture: ArchitectureConfig) -> Self {
        Self {
            device: DisplayDevice::new(),
            architecture,
        }
    }

    /// A driver over an existing device, for a caller that opened one already.
    pub const fn with_device(architecture: ArchitectureConfig, device: DisplayDevice) -> Self {
        Self {
            device,
            architecture,
        }
    }

    /// The device, so a host frontend can read what was presented.
    pub const fn device(&self) -> &DisplayDevice {
        &self.device
    }

    /// The device, mutably, for a caller that injects a window directly.
    ///
    /// This is how a *test* or a host adapter sets up a window without a guest
    /// asking for one. A Lazen program cannot reach it: the SDK's `open` goes
    /// through `display_open`, not through here.
    pub fn device_mut(&mut self) -> &mut DisplayDevice {
        &mut self.device
    }

    /// The last frame the guest presented, if any.
    pub fn last_frame(&self) -> Option<PresentedDisplay> {
        self.device.presented().map(|frame| PresentedDisplay {
            address: frame.address,
            width: frame.width,
            height: frame.height,
            present_count: frame.present_count,
        })
    }
}

/// What a driver reports as the visible frame.
///
/// This is an *address*, not pixels. The driver holds no copy, so anything that
/// wants the pixels resolves the address against the machine's memory — which is
/// the property Step 68's design is for, and the reason a host frontend in Step 77
/// reads guest memory rather than asking the driver for a bitmap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PresentedDisplay {
    /// The framebuffer's address in guest physical memory.
    pub address: u64,
    /// The width in pixels.
    pub width: u64,
    /// The height in pixels.
    pub height: u64,
    /// How many frames had been presented when this one was.
    pub present_count: u64,
}

impl DisplayService {
    /// `display_open`: open a window over the guest's framebuffer.
    fn open(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        width: u32,
        height: u32,
        framebuffer: VirtualAddress,
        record: VirtualAddress,
    ) -> ServiceOutcome {
        let address = framebuffer.as_u64();
        if let Err(error) = self
            .device
            .open(u64::from(width), u64::from(height), address)
        {
            return failure(syscall_error(error));
        }
        let reported = match DisplayRecord::new(self.architecture, width, height, address) {
            Ok(reported) => reported,
            Err(_) => return failure(SyscallError::InvalidArgument),
        };
        let bytes = reported.encode();
        if let Err(_error) = memory.write_bytes(record, &bytes) {
            // The window is left closed: validation checked the record was
            // writable, so this is a device fault rather than a bad call, and
            // leaving a window open whose record the guest never received would
            // be a window the guest cannot know about.
            self.device.close();
            return failure(SyscallError::InvalidPointer);
        }
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }

    /// `display_present`: record that the guest's frame is the visible one.
    fn present(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        framebuffer: VirtualAddress,
        result: VirtualAddress,
    ) -> ServiceOutcome {
        let address = framebuffer.as_u64();
        // Two different bugs, so two different answers. A present with no window
        // open is a program that skipped `display_open`, and a present of some
        // other address is a program that drew into the wrong frame. Both would
        // otherwise read as "presented a frame that is not on screen", which is
        // the one answer that hides both.
        if !self.device.is_open() {
            return self.report(memory, result, 0, SyscallStatus::InvalidHandle);
        }
        // A present of an address that is not the open window's is refused, rather
        // than presented anyway. A program that presents the wrong frame has a bug,
        // and showing *something* would hide it until the picture was wrong.
        if self.device.framebuffer() != address {
            return self.report(memory, result, 0, SyscallStatus::InvalidArgument);
        }
        match self.device.present() {
            // The record reports the number of frames presented *so far*, which is
            // how a program knows a frame reached the screen without asking the
            // device a second question.
            Ok(()) => {
                let count = self.device.present_count();
                self.report(memory, result, count, SyscallStatus::Ok)
            }
            Err(error) => {
                let count = self.device.present_count();
                self.report(
                    memory,
                    result,
                    count,
                    SyscallStatus::from(syscall_error(error)),
                )
            }
        }
    }

    /// Writes an `IoResult` saying how many frames have been presented.
    ///
    /// The record is always written, success or failure: a program that gets a
    /// refusal can then read how far it got, which is what makes a present loop
    /// report *where* it stopped rather than just that it stopped.
    fn report(
        &self,
        memory: &mut UserMemoryContext<'_>,
        result: VirtualAddress,
        transferred: u64,
        status: SyscallStatus,
    ) -> ServiceOutcome {
        let record = match IoResult::new(self.architecture, transferred, status) {
            Ok(record) => record,
            Err(_) => return failure(SyscallError::InvalidArgument),
        };
        if memory.write_bytes(result, &record.encode()).is_err() {
            return failure(SyscallError::InvalidPointer);
        }
        // The *return value* is a success either way: the call reached the driver
        // and the driver answered, in the record. Whether the present itself
        // worked is the record's status, which is the question a program asking
        // "how many frames reached the screen" actually has.
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }
}

/// The syscall error a display refusal becomes.
///
/// This is a function rather than a `From` impl because `SyscallError` belongs to
/// the ABI crate, and a foreign type cannot gain a trait impl here. The mapping is
/// stated once, and the error a caller sees is one of the ABI's own — so a program
/// tests a display failure against the same table it tests every other failure
/// against, rather than against a display-specific code.
fn syscall_error(error: DisplayError) -> SyscallError {
    match error {
        DisplayError::EmptyWindow | DisplayError::ReadOnlyRegister(_) => {
            SyscallError::InvalidArgument
        }
        // A geometry that changed without a new open is a guest bug, and the guest is
        // what this mapping is for: the syscall surface reports it as an invalid
        // argument rather than hiding it, because a guest whose window size is not what
        // it thinks is a guest whose drawing will be wrong.
        // A host that returned the wrong number of bytes for a frame is the host.s bug,
        // not the guest.s, but the guest sees a refusal either way 2014 and a guest that
        // cannot see a frame is a guest that should be told its drawing is not landing.
        DisplayError::ShortFrameRead { .. } | DisplayError::WindowGeometryMismatch { .. } => {
            SyscallError::InvalidArgument
        }
        DisplayError::WindowTooLarge { .. } | DisplayError::FramebufferOverflow { .. } => {
            SyscallError::ResourceExhausted
        }
        DisplayError::NullFramebuffer => SyscallError::InvalidPointer,
        DisplayError::NoWindow => SyscallError::InvalidHandle,
    }
}

impl KernelService for DisplayService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::DisplayOpen {
                width,
                height,
                framebuffer,
                record,
            } => self.open(memory, width, height, framebuffer, record),
            ValidatedSyscallKind::DisplayPresent {
                framebuffer,
                result,
            } => self.present(memory, framebuffer, result),
            // The driver handles only the display calls. A guest that reaches it
            // with anything else has a number the driver does not own, and saying
            // so is better than pretending.
            _ => failure(SyscallError::UnknownSyscall),
        }
    }
}

fn failure(error: SyscallError) -> ServiceOutcome {
    ServiceOutcome::Return(TaggedOutcome::failure(error, 0))
}

/// Reads the window a `display_open` reported, from a guest record.
///
/// A host frontend that wants to learn a window.s geometry reads the record the
/// guest was given, through the one function that decodes it. The driver does not
/// need this: it wrote the record and remembers the window itself.
pub fn read_display_record(
    record: &[u8],
    architecture: ArchitectureConfig,
) -> Result<DisplayRecord, lazalith_os_abi::AbiError> {
    DisplayRecord::decode(record, architecture)
}
