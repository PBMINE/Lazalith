//! The virtual display device.
//!
//! Every test here checks a property the design claims, and the claims are the
//! point: the guest owns the pixels, the device owns only geometry and a counter,
//! and nothing about the device knows a host window exists.
//!
//! The framebuffer tests read pixels out of a *host-side* buffer that stands in
//! for guest RAM and then mutate it, because that is the property being tested: the
//! device's presented frame must be the bytes the guest wrote, with no copy
//! anywhere. A device that kept its own copy would pass a test that only compared
//! what it presented against what the guest wrote *at present time*; mutating the
//! buffer afterwards is what separates the two designs.

extern crate alloc;

use lazalith_devices::{
    DISPLAY_ABI_VERSION, DisplayDevice, DisplayError, MAX_DIMENSION, REGISTER_ABI_VERSION,
    REGISTER_BYTES, REGISTER_FRAMEBUFFER, REGISTER_HEIGHT, REGISTER_LAST_PRESENT, REGISTER_PRESENT,
    REGISTER_PRESENT_COUNT, REGISTER_STATUS, REGISTER_WIDTH, STATUS_PRESENTED, frame_bytes,
    pixel_at, zeroed_framebuffer,
};
use lazalith_isa::DataSize;
use lazalith_types::{CycleCount, DeviceOffset};

/// The device is a whole number of double-words, so a guest can address every
/// register it claims to have.
#[test]
fn the_register_block_is_a_whole_number_of_words() {
    assert_eq!(
        REGISTER_BYTES % 8,
        0,
        "every register is a double-word, so the block is a whole number of them"
    );
    assert_eq!(REGISTER_BYTES, 64, "eight registers, as documented");
}

/// A fresh device has no window and has presented nothing.
#[test]
fn a_fresh_device_has_no_window_and_no_frame() {
    let device = DisplayDevice::new();
    assert!(!device.is_open(), "no window is open");
    assert_eq!(device.width(), 0);
    assert_eq!(device.height(), 0);
    assert_eq!(device.presented(), None, "and no frame has been shown");
    assert_eq!(device.present_count(), 0);
}

/// A window opens at the geometry it was asked for, over the address it named.
#[test]
fn a_window_opens_at_the_geometry_it_was_given() {
    let mut device = DisplayDevice::new();
    device
        .open(320, 200, 0x4000_0000)
        .expect("a legal window opens");
    assert!(device.is_open());
    assert_eq!(device.width(), 320);
    assert_eq!(device.height(), 200);
    assert_eq!(device.framebuffer(), 0x4000_0000);
}

/// A present records the frame and counts it, and the count is what a program
/// uses to know something reached the screen.
#[test]
fn a_present_is_counted_and_remembers_the_address() {
    let mut device = DisplayDevice::new();
    device.open(8, 4, 0x1000).expect("opens");
    assert_eq!(
        device.presented(),
        None,
        "an open window is not a frame: nothing has been shown yet"
    );
    device.present().expect("presents");
    let frame = device.presented().expect("a frame is now shown");
    assert_eq!(frame.address, 0x1000);
    assert_eq!(frame.width, 8);
    assert_eq!(frame.height, 4);
    assert_eq!(frame.present_count, 1);
    device.present().expect("presents again");
    assert_eq!(device.present_count(), 2, "each present is counted");
    assert_eq!(
        device.presented().map(|frame| frame.present_count),
        Some(2),
        "and the frame reports the count at which it was shown"
    );
}

/// Presenting with no window open is refused, so the counter cannot claim frames
/// that were never displayed.
#[test]
fn a_present_with_no_window_is_refused() {
    let mut device = DisplayDevice::new();
    assert_eq!(device.present(), Err(DisplayError::NoWindow));
    assert_eq!(
        device.present_count(),
        0,
        "a refused present is not counted: the counter is what a program trusts"
    );
}

/// **The central property.** The presented frame names an address, and the bytes
/// there are the bytes the guest wrote — including after the guest changes them,
/// because the device never copied them.
#[test]
fn the_guest_owns_the_pixels_and_the_device_holds_no_copy() {
    let mut device = DisplayDevice::new();
    // `memory` stands in for guest RAM and the framebuffer sits at `base` inside
    // it, so the address the device was given is a real offset into it.
    let base = 0x10u64;
    let mut memory = alloc::vec![0u8; 0x10];
    memory.extend(zeroed_framebuffer(2, 2).expect("a 2x2 framebuffer"));
    // The device is told where the framebuffer is, not given its contents.
    device
        .open(2, 2, base)
        .expect("opens over the caller's memory");
    device.present().expect("presents");

    // The guest writes ARGB8888 into its own memory: byte 0 is alpha.
    let first = base as usize;
    memory[first..first + 4].copy_from_slice(&[0xff, 0x11, 0x22, 0x33]);
    let frame = device.presented().expect("a frame");
    assert_eq!(
        pixel_at(&frame, &memory, 0, 0),
        Some([0xff, 0x11, 0x22, 0x33]),
        "the presented frame reads the bytes the guest wrote"
    );
    assert_eq!(
        frame_bytes(&frame, &memory).map(<[u8]>::len),
        Some(2 * 2 * 4),
        "and the frame is exactly width * height * 4 bytes"
    );

    // Now the guest changes its memory *after* presenting. If the device had kept
    // a copy, the presented frame would still show the old pixels.
    memory[first..first + 4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
    assert_eq!(
        pixel_at(&frame, &memory, 0, 0),
        Some([0x01, 0x02, 0x03, 0x04]),
        "the frame shows what the guest's memory says *now*, because the device \
         never copied it"
    );
}

/// A pixel is addressed row-major with no padding, so (1, 1) of a 2x2 canvas is
/// the last four bytes and not a computed stride.
#[test]
fn a_pixel_is_row_major_with_no_padding() {
    let frame = lazalith_devices::PresentedFrame {
        address: 0,
        width: 2,
        height: 2,
        present_count: 1,
    };
    let mut memory = zeroed_framebuffer(2, 2).expect("a framebuffer");
    for index in 0..4u64 {
        let start = (index * 4) as usize;
        memory[start..start + 4].copy_from_slice(&[index as u8, 0, 0, 0]);
    }
    assert_eq!(pixel_at(&frame, &memory, 0, 0), Some([0, 0, 0, 0]));
    assert_eq!(pixel_at(&frame, &memory, 1, 0), Some([1, 0, 0, 0]));
    assert_eq!(pixel_at(&frame, &memory, 0, 1), Some([2, 0, 0, 0]));
    assert_eq!(
        pixel_at(&frame, &memory, 1, 1),
        Some([3, 0, 0, 0]),
        "row-major: (1, 1) is index 3, with no stride to get wrong"
    );
}

/// A coordinate outside the frame is no pixel, not a pixel with a default value.
#[test]
fn a_pixel_outside_the_frame_is_not_a_pixel() {
    let frame = lazalith_devices::PresentedFrame {
        address: 0,
        width: 2,
        height: 2,
        present_count: 1,
    };
    let memory = zeroed_framebuffer(2, 2).expect("a framebuffer");
    assert_eq!(pixel_at(&frame, &memory, 2, 0), None, "past the width");
    assert_eq!(pixel_at(&frame, &memory, 0, 2), None, "past the height");
    assert_eq!(pixel_at(&frame, &memory, u64::MAX, 0), None, "far past it");
}

/// A zero-sized or oversized window is refused, with a reason.
#[test]
fn an_illegal_geometry_is_refused_with_a_reason() {
    let mut device = DisplayDevice::new();
    assert_eq!(
        device.open(0, 10, 0x1000),
        Err(DisplayError::EmptyWindow),
        "a window with no width is not a window"
    );
    assert_eq!(device.open(10, 0, 0x1000), Err(DisplayError::EmptyWindow));
    assert_eq!(
        device.open(MAX_DIMENSION + 1, 10, 0x1000),
        Err(DisplayError::WindowTooLarge {
            width: MAX_DIMENSION + 1,
            height: 10,
            limit: MAX_DIMENSION,
        })
    );
    assert!(
        !device.is_open(),
        "and a refused open leaves no window behind"
    );
}

/// A framebuffer at address zero is refused: that address is the bottom of the
/// address space, not somewhere a window can live.
#[test]
fn a_framebuffer_at_address_zero_is_refused() {
    let mut device = DisplayDevice::new();
    assert_eq!(device.open(8, 8, 0), Err(DisplayError::NullFramebuffer));
    assert!(!device.is_open());
}

/// Closing forgets the window but keeps the count of frames that were shown.
#[test]
fn closing_keeps_the_frame_count() {
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    device.present().expect("presents");
    device.present().expect("presents");
    device.close();
    assert!(!device.is_open(), "the window is gone");
    assert_eq!(
        device.present_count(),
        2,
        "but the frames that were shown were shown, and forgetting that would \
         make the counter mean 'since the last open' rather than 'shown'"
    );
    assert_eq!(device.present(), Err(DisplayError::NoWindow));
}

/// `open_and_present` is what a program means by opening a window.
#[test]
fn opening_and_presenting_gives_a_frame_at_once() {
    let mut device = DisplayDevice::new();
    let frame = device
        .open_and_present(16, 16, 0x8000)
        .expect("a window and its first frame");
    assert_eq!(frame.address, 0x8000);
    assert_eq!(frame.width, 16);
    assert_eq!(frame.present_count, 1, "the first frame is presented");
}

/// Every register reads back what the device knows.
#[test]
fn the_registers_report_what_the_device_knows() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(12, 7, 0x3000).expect("opens");
    device.present().expect("presents");

    assert_eq!(
        device.read(REGISTER_WIDTH, DataSize::Double).unwrap(),
        12,
        "the width register"
    );
    assert_eq!(device.read(REGISTER_HEIGHT, DataSize::Double).unwrap(), 7);
    assert_eq!(
        device.read(REGISTER_FRAMEBUFFER, DataSize::Double).unwrap(),
        0x3000
    );
    assert_eq!(
        device
            .read(REGISTER_PRESENT_COUNT, DataSize::Double)
            .unwrap(),
        1
    );
    assert_eq!(
        device
            .read(REGISTER_LAST_PRESENT, DataSize::Double)
            .unwrap(),
        0x3000
    );
    assert_eq!(
        device.read(REGISTER_ABI_VERSION, DataSize::Double).unwrap(),
        DISPLAY_ABI_VERSION,
        "a program can check the ABI before trusting any other register"
    );
    assert_eq!(
        device.read(REGISTER_STATUS, DataSize::Double).unwrap(),
        STATUS_PRESENTED,
        "and the status says a frame has been presented"
    );
}

/// The status distinguishes "never presented" from "presented a blank frame".
#[test]
fn the_status_distinguishes_never_presented_from_blank() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    assert_eq!(
        device.read(REGISTER_STATUS, DataSize::Double).unwrap(),
        0,
        "an open window with no present is not a shown frame"
    );
    device.present().expect("presents");
    assert_ne!(
        device.read(REGISTER_STATUS, DataSize::Double).unwrap(),
        0,
        "a presented frame is distinguishable even if every pixel is zero"
    );
}

/// A geometry register is read-only, because half a geometry is not a geometry.
#[test]
fn the_geometry_registers_are_read_only() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(8, 8, 0x1000).expect("opens");
    assert!(
        device.write(REGISTER_WIDTH, DataSize::Double, 99).is_err(),
        "the width cannot be set on its own"
    );
    assert!(
        device.write(REGISTER_HEIGHT, DataSize::Double, 99).is_err(),
        "and neither can the height"
    );
    assert_eq!(
        device.read(REGISTER_WIDTH, DataSize::Double).unwrap(),
        8,
        "so the window is unchanged"
    );
    assert_eq!(device.read(REGISTER_HEIGHT, DataSize::Double).unwrap(), 8);
}

/// Writing the present register presents, and writing a count register does not.
#[test]
fn the_present_register_presents_and_the_count_registers_do_not() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    device
        .write(REGISTER_PRESENT, DataSize::Double, 0)
        .expect("a present takes no argument, so zero is as good as any value");
    assert_eq!(device.present_count(), 1, "the write presented a frame");
    for offset in [
        REGISTER_PRESENT_COUNT,
        REGISTER_LAST_PRESENT,
        REGISTER_ABI_VERSION,
        REGISTER_STATUS,
    ] {
        assert!(
            device.write(offset, DataSize::Double, 0).is_err(),
            "{} is written by the device",
            offset.as_u64()
        );
    }
    assert_eq!(
        device.present_count(),
        1,
        "and a refused write did not present a frame"
    );
}

/// A refused write leaves the device exactly as it was.
#[test]
fn a_refused_write_changes_nothing() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    device.present().expect("presents");
    // A present with the window closed is the interesting refusal: validating it
    // must not have counted it.
    device.close();
    assert!(device.write(REGISTER_PRESENT, DataSize::Double, 0).is_err());
    assert_eq!(
        device.present_count(),
        1,
        "the refused present was not counted during validation either"
    );
    assert_eq!(device.presented().map(|frame| frame.present_count), Some(1));
}

/// The framebuffer address may be changed, because a program may move its memory.
#[test]
fn the_framebuffer_address_can_be_changed() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    device
        .write(REGISTER_FRAMEBUFFER, DataSize::Double, 0x2000)
        .expect("a new address");
    device.present().expect("presents");
    assert_eq!(
        device.presented().map(|frame| frame.address),
        Some(0x2000),
        "and the presented frame is the new one"
    );
    assert!(
        device
            .write(REGISTER_FRAMEBUFFER, DataSize::Double, 0)
            .is_err(),
        "but not to address zero"
    );
    assert_eq!(
        device.read(REGISTER_FRAMEBUFFER, DataSize::Double).unwrap(),
        0x2000,
        "and the refused write left the old address in place"
    );
}

/// Every register access is a whole double-word at a register offset.
#[test]
fn a_register_access_must_be_a_whole_word_at_a_register_offset() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    assert!(
        device.read(DeviceOffset::new(1), DataSize::Double).is_err(),
        "an unaligned offset is not a register"
    );
    assert!(
        device.read(REGISTER_WIDTH, DataSize::Byte).is_err(),
        "and a register is not a byte"
    );
    assert!(
        device
            .read(DeviceOffset::new(REGISTER_BYTES), DataSize::Double)
            .is_err(),
        "an offset past the block is not a register either"
    );
    assert!(
        device.read(REGISTER_WIDTH, DataSize::Double).is_ok(),
        "while the documented access is fine"
    );
}

/// A reset closes the window and forgets the frames.
#[test]
fn a_reset_forgets_everything() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.open(4, 4, 0x1000).expect("opens");
    device.present().expect("presents");
    device.reset();
    assert!(!device.is_open(), "the window is closed");
    assert_eq!(device.presented(), None, "and no frame is shown");
    assert_eq!(device.present_count(), 0, "and nothing was ever shown");
    assert_eq!(device.framebuffer(), 0);
}

/// `peek` reads a register as bytes, so a debugger can look without a `read`.
#[test]
fn a_register_can_be_peeked_as_bytes() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    // The width is a two-byte value, so the *upper* half of the register is what
    // makes the little-endian assertion worth making.
    device.open(0x0102, 0x0304, 0x1000).expect("opens");
    let mut bytes = [0u8; 8];
    device
        .peek(REGISTER_WIDTH, &mut bytes)
        .expect("a register peeks as eight bytes");
    assert_eq!(
        u64::from_le_bytes(bytes),
        0x0102,
        "little-endian, like every other word in the machine"
    );
    assert!(device.peek(REGISTER_WIDTH, &mut [0u8; 1]).is_err());
}

/// `tick` records the clock, so a device that cares about time can.
#[test]
fn a_tick_records_the_clock() {
    use lazalith_devices::Device;
    let mut device = DisplayDevice::new();
    device.tick(CycleCount::new(1234));
    // The display does not use time, and the test says so by checking that
    // ticking changed nothing observable rather than by asserting a field the
    // device does not promise to expose.
    assert!(!device.is_open(), "a tick does not open a window");
    assert_eq!(device.presented(), None, "nor present a frame");
}
