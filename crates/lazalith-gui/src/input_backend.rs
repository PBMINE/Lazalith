//! B9: the SDL3 input backend.
//!
//! §30: "Separate `guest keyboard/mouse/controller` from `host input backend`." This
//! file is the second half, and it lives in `lazalith-gui` for B7's reason — it is the
//! crate that may talk to SDL3, and `lazalith-sdl3` has no Lazalith dependencies at all
//! because it is the project's entire auditable `unsafe` surface.
//!
//! # The mapping, and what it is honest about
//!
//! SDL reports a *scancode* (which physical key) and a *keycode* (which character it
//! would produce). A guest event carries a `u32` code, and which of the two goes in it
//! is a real decision:
//!
//! - the **keycode** is what a program wants — "the user typed `a`" — and is layout
//!   dependent, so the same physical key produces different codes under different
//!   keyboard layouts;
//! - the **scancode** is what a program that cares about *position* wants, and is
//!   layout independent.
//!
//! The keycode is used, and auto-repeat is **dropped**. Repeat is a property of how long
//! a key is held, not of what the user asked for, and a guest that received repeats
//! would see a different event stream from the same keystroke depending on how the host
//! was scheduled. That makes a replayed session and a live one differ, which is exactly
//! the kind of difference B18's replay work exists to eliminate. The `repeat` field is
//! read and discarded here rather than in the GUI, so the decision is in one place.

use core::fmt;

use lazalith_devices::{Event, EventKind, InputBackend, InputBackendError, InputProfile};
use lazalith_sdl3::{self, Input as SdlInput};

/// A guest event kind for a key press.
const KEY_DOWN: EventKind = EventKind::KeyDown;

/// A host input backend that reads SDL's event queue.
///
/// Holds no device: it is the *source*, and the device it feeds is the guest's. That is
/// the whole asymmetry B7 established, seen from the other side.
pub struct Sdl3InputBackend {
    device: lazalith_types::DeviceId,
    profile: InputProfile,
    dropped: u64,
}

impl Sdl3InputBackend {
    /// A backend that produces events for this guest device.
    pub const fn new(device: lazalith_types::DeviceId) -> Self {
        Self {
            device,
            profile: InputProfile::Virtual,
            dropped: 0,
        }
    }

    /// How many host events were not turned into guest events.
    ///
    /// Non-zero is normal — a window close is not a keystroke, and an auto-repeat is
    /// deliberately dropped — and it is counted so that a caller debugging "the guest
    /// did not see my key" can see that events *were* arriving and being discarded
    /// rather than never arriving at all.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

impl fmt::Debug for Sdl3InputBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sdl3InputBackend")
            .field("device", &self.device)
            .field("profile", &self.profile)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl InputBackend for Sdl3InputBackend {
    fn poll(&mut self) -> Result<Option<Event>, InputBackendError> {
        match lazalith_sdl3::poll_event() {
            Ok(None) => Ok(None),
            Ok(Some(SdlInput::Quit)) => {
                // A window close is not a keystroke. Counted rather than reported: the
                // guest is not going to see it, and a pump that failed every time the
                // window manager asked politely would be useless.
                self.dropped = self.dropped.saturating_add(1);
                Ok(None)
            }
            Ok(Some(SdlInput::KeyDown { key, repeat, .. })) => {
                if repeat {
                    self.dropped = self.dropped.saturating_add(1);
                    return Ok(None);
                }
                Ok(Some(Event::new(KEY_DOWN, key)))
            }
            Err(error) => {
                self.dropped = self.dropped.saturating_add(1);
                Err(InputBackendError::Host {
                    operation: "read an input event",
                    detail: error.to_string(),
                })
            }
        }
    }
    fn device(&self) -> Option<lazalith_types::DeviceId> {
        Some(self.device)
    }
}
