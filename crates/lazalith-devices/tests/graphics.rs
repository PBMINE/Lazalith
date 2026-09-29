//! B7: the display backend boundary.
//!
//! # What is being held here
//!
//! **The device holds no backend.** This is the whole design, and it is the opposite
//! of B5's block device, so it is worth stating why rather than asserting.
//!
//! A block device has a backend because a guest's register write *goes somewhere*: the
//! data must reach the host's storage during the write. A display does not — a guest
//! writes pixels into ordinary memory and rings a present register, and the frame is a
//! *description*. If the device called a backend during `present`, one guest
//! instruction would call into the host synchronously, and a host that had stopped
//! answering would stall the machine with no fault and no timeout. So the host pulls,
//! on its own schedule, and a machine whose display nobody watches costs nothing.
//!
//! The tests therefore check three things: the device's own state is unchanged by a
//! pump (it is the guest's device, not the host's scratch space), a frame reaches a
//! backend with the geometry and pixels the guest produced, and a host failure is
//! reported rather than swallowed.
//!
//! **The `no_std` boundary.** `DisplayBackend` is declared in `lazalith-devices`
//! because that is where the device is, and implemented in `lazalith-gui` because that
//! is the crate that may talk to SDL3. It is deliberately **not** in `lazalith-sdl3`,
//! which has no Lazalith dependencies at all and whose entire content is the auditable
//! `unsafe` surface. A display backend there would have made the FFI boundary know
//! what a guest frame is.

use lazalith_devices::{
    DisplayBackend, DisplayBackendError, DisplayDevice, DisplayError, DisplayFrame, DisplayProfile,
    DisplayPump, HeadlessDisplayBackend, PIXEL_BYTES, frame_len, open_window, presented_geometry,
    pump_display,
};
use lazalith_types::PhysicalAddress;

const WIDTH: u64 = 8;
const HEIGHT: u64 = 4;
const FRAMEBUFFER: u64 = 0x0010_4000;

/// A device with a window open and one frame presented, which is the state a pump
/// acts on.
fn device_with_frame(pixels: Vec<u8>) -> DisplayDevice {
    let mut device = DisplayDevice::new();
    device
        .open(WIDTH, HEIGHT, FRAMEBUFFER)
        .expect("the geometry is legal");
    device.present().expect("a window is open");
    let _ = pixels;
    device
}

/// A framebuffer of solid colour, so a checksum change means the pixels changed.
fn solid(value: u8) -> Vec<u8> {
    vec![value; (WIDTH * HEIGHT * PIXEL_BYTES) as usize]
}

/// A `read` that always answers, from one buffer.
fn reader(pixels: Vec<u8>) -> impl FnMut(PhysicalAddress, u64) -> Option<Vec<u8>> {
    move |_address, length| {
        if length as usize == pixels.len() {
            Some(pixels.clone())
        } else {
            None
        }
    }
}

// -- the profile taxonomy of §28 ----------------------------------------------

#[test]
fn only_the_native_display_is_constructible() {
    assert!(DisplayProfile::Native.is_constructible());
    for profile in [
        DisplayProfile::VgaCompatible,
        DisplayProfile::ModernFramebuffer,
    ] {
        assert!(
            !profile.is_constructible(),
            "{profile} is not constructible, and a machine that claimed one would be a \
             machine that cannot draw"
        );
        assert!(
            !profile.as_str().is_empty(),
            "a profile has a name for diagnostics"
        );
    }
}

#[test]
fn the_vga_profile_names_no_registers_because_there_are_none() {
    // §28 requires VGA/EGA to be researched from historical primary sources before it
    // is implemented, and that has not happened. This test is the record of that: if
    // someone later implements VGA, this is the line to delete, and deleting it should
    // mean the CRTC/palette model arrived *with* the research rather than before it.
    let profile = DisplayProfile::VgaCompatible;
    assert_eq!(profile.as_str(), "vga");
    assert!(!profile.is_constructible());
}

// -- the device is not touched by a pump ---------------------------------------

#[test]
fn a_device_with_no_presented_frame_pumps_nothing() {
    let mut device = DisplayDevice::new();
    let mut backend = HeadlessDisplayBackend::new();
    let outcome = pump_display(&mut device, &mut backend, reader(solid(1)))
        .expect("a pump with nothing to do is not a failure");
    assert_eq!(outcome, DisplayPump::Nothing);
    assert_eq!(backend.frames(), 0, "and the backend was handed nothing");
}

#[test]
fn a_pump_does_not_change_the_device_s_own_state() {
    let mut device = device_with_frame(solid(0x7F));
    let before_present = device.presented().map(|frame| frame.present_count);
    let mut backend = HeadlessDisplayBackend::new();

    pump_display(&mut device, &mut backend, reader(solid(0x7F))).expect("the frame resolves");

    assert_eq!(
        device.presented().map(|frame| frame.present_count),
        before_present,
        "a pump is a host reading the device, not the device doing something: if the \
         present count moved, the guest would see its own counter change because a \
         window was on screen"
    );
    assert_eq!(
        presented_geometry(&device),
        Some((WIDTH, HEIGHT)),
        "and the geometry is still the guest's"
    );
}

#[test]
fn a_pump_hands_the_backend_the_geometry_and_the_pixels() {
    let pixels = solid(0x3C);
    let mut device = device_with_frame(pixels.clone());
    let mut backend = HeadlessDisplayBackend::new();

    let outcome = pump_display(&mut device, &mut backend, reader(pixels.clone()))
        .expect("the frame resolves");
    assert_eq!(outcome, DisplayPump::Presented { present_count: 1 });
    assert_eq!(backend.window(), Some((WIDTH, HEIGHT)));
    assert_eq!(backend.frames(), 1);

    // The checksum is the point: it is computed from the bytes, so this asserts the
    // backend saw *these* pixels and not merely a frame of the right size.
    let expected = checksum(&pixels);
    assert_eq!(
        backend.last_checksum(),
        expected,
        "the backend was handed the framebuffer the guest wrote, not zeroes"
    );
    assert_eq!(backend.last_present_count(), 1);
}

#[test]
fn two_different_frames_give_two_different_checksums() {
    let mut device = device_with_frame(Vec::new());
    let mut backend = HeadlessDisplayBackend::new();
    pump_display(&mut device, &mut backend, reader(solid(0x11))).expect("the first frame resolves");
    let first = backend.last_checksum();
    pump_display(&mut device, &mut backend, reader(solid(0x22)))
        .expect("the second frame resolves");
    assert_ne!(
        first,
        backend.last_checksum(),
        "a display backend that cannot tell two frames apart is not showing anything"
    );
}

/// FNV-1a, the same function `HeadlessDisplayBackend` uses. Written out here rather
/// than reused so a test that asserts the checksum is asserting the *algorithm* and
/// not merely that the implementation agrees with itself.
fn checksum(pixels: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

// -- failures are reported -----------------------------------------------------

#[test]
fn an_unreadable_framebuffer_is_reported_and_not_shown() {
    let mut device = device_with_frame(Vec::new());
    let mut backend = HeadlessDisplayBackend::new();

    // A reader that cannot read anything at all.
    let outcome = pump_display(&mut device, &mut backend, |_address, _length| None)
        .expect("an unreadable framebuffer is an outcome, not an error");
    assert_eq!(
        outcome,
        DisplayPump::Unreadable {
            address: PhysicalAddress::new(FRAMEBUFFER)
        },
        "the backend was never at fault, so this is not a backend error"
    );
    assert_eq!(backend.frames(), 0, "and nothing was drawn");
}

#[test]
fn a_short_read_is_refused_before_the_backend_sees_it() {
    let mut device = device_with_frame(Vec::new());
    let mut backend = HeadlessDisplayBackend::new();
    // A reader that returns half the framebuffer: a backend handed this would either
    // read past it or draw garbage, and either would be reported as the backend's fault.
    let error = pump_display(&mut device, &mut backend, |_address, length| {
        Some(vec![0; (length / 2) as usize])
    })
    .expect_err("a short read is caught while the frame is being resolved");
    assert!(
        matches!(
            error,
            DisplayBackendError::Device(DisplayError::ShortFrameRead { .. })
        ),
        "the backend is never handed a slice it would have to read past: {error:?}"
    );
    assert_eq!(backend.frames(), 0, "and nothing was drawn");
}

#[test]
fn a_frame_larger_than_its_geometry_is_refused() {
    let pixels = solid(1);
    let error = DisplayFrame::new(WIDTH, HEIGHT, 1, &pixels[..16]).unwrap_err();
    assert_eq!(
        error,
        DisplayError::ShortFrameRead {
            expected: frame_len(WIDTH, HEIGHT),
            found: 16,
        },
        "a short frame is named as a short frame, not as a geometry that does not fit: \
         the two are fixed by different people"
    );
}

#[test]
fn a_backend_given_a_frame_the_was_not_opened_for_refuses_it() {
    let mut backend = HeadlessDisplayBackend::new();
    let pixels = solid(1);
    let frame = DisplayFrame::new(WIDTH, HEIGHT, 1, &pixels).expect("the frame is the right size");
    DisplayBackend::open(&mut backend, WIDTH, HEIGHT).expect("a window opens");

    let wider = vec![0; (WIDTH * 2 * HEIGHT * PIXEL_BYTES) as usize];
    let wrong_size = DisplayFrame::new(WIDTH * 2, HEIGHT, 1, &wider)
        .expect("the frame is the right size for its own geometry");
    let error = backend.present(&wrong_size).unwrap_err();
    assert!(
        matches!(
            error,
            DisplayBackendError::Device(DisplayError::WindowGeometryMismatch { .. })
        ),
        "a backend that resized itself to match would hide a guest that changed its \
         geometry without asking, and the guest's window would become a function of \
         what the host felt like doing. Got {error:?}"
    );
    // And a present with no window at all is refused too.
    let mut closed = HeadlessDisplayBackend::new();
    let error = closed.present(&frame).unwrap_err();
    assert!(matches!(
        error,
        DisplayBackendError::Device(DisplayError::NoWindow)
    ));
}

#[test]
fn closing_a_window_hides_it_but_keeps_what_was_drawn() {
    let mut backend = HeadlessDisplayBackend::new();
    let pixels = solid(9);
    let frame = DisplayFrame::new(WIDTH, HEIGHT, 3, &pixels).expect("the frame is the right size");
    backend.open(WIDTH, HEIGHT).expect("a window opens");
    backend.present(&frame).expect("a frame");
    let checksum = backend.last_checksum();

    backend.close();
    assert_eq!(backend.window(), None, "the window is gone");
    assert_eq!(
        backend.last_checksum(),
        checksum,
        "and what was last drawn is still remembered: a program that closed its window \
         can still say what it drew, and forgetting would make the record mean 'since \
         the last open' rather than 'what was on screen'"
    );
    assert_eq!(backend.last_present_count(), 3);
}

#[test]
fn an_open_window_is_reported_by_the_host_when_there_is_one() {
    let mut device = device_with_frame(Vec::new());
    let mut backend = HeadlessDisplayBackend::new();
    assert_eq!(
        open_window(&mut device, &mut backend).expect("opening a window"),
        Some((WIDTH, HEIGHT))
    );

    device.close();
    let mut other = HeadlessDisplayBackend::new();
    assert_eq!(
        open_window(&mut device, &mut other).expect("no window to open"),
        None,
        "a closed device has no window to open, and saying so is better than \
         re-opening one at zero size"
    );
}

#[test]
fn a_frame_is_exactly_as_many_bytes_as_its_geometry_says() {
    assert_eq!(frame_len(8, 4), 8 * 4 * PIXEL_BYTES);
    assert_eq!(
        solid(1).len() as u64,
        frame_len(WIDTH, HEIGHT),
        "the test's own framebuffer agrees with the function the pump uses to ask for \
         one, so a change to the format breaks this test rather than silently making \
         the pump ask for the wrong number of bytes"
    );
}
