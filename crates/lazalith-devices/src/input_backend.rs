//! B9: the input backend boundary, and the input architectures of §30.
//!
//! # The same asymmetry as B7, and for the same reason
//!
//! A display is *pulled* by the host, so `DisplayDevice` holds no backend. Input is the
//! same shape from the other side: a host produces events — asynchronously, whenever a
//! key is pressed — and a guest consumes them from a queue. Nothing happens during a
//! guest's register read.
//!
//! So the boundary is a **source**: [`InputBackend::poll`] asks the host for the next
//! event, and [`pump_input`] moves events from a backend into a device. The device
//! never calls the host. If it did, a guest's register read would call into SDL, and a
//! host that had stopped answering would stall the machine with no fault and no
//! timeout — the same argument that put the display backend in the host crate.
//!
//! # What this replaces
//!
//! `HostScript::replay(&mut InputDevice)` reached *into* the guest's device and pushed
//! events at it. That works and it is the shape §30 asks to separate, so the method
//! stays — but it is now [`ScriptedInputBackend`] implementing [`InputBackend`], and
//! `replay` is a two-line call through the boundary rather than a direct reach. A
//! caller with two input sources can now mix them, which it could not do at all.
//!
//! # What §30 names, and what exists
//!
//! §30 asks for a host input backend, and names PS/2, USB HID and modern virtual input
//! as things to investigate, plus a USB hierarchy of controller → bus → device →
//! backend. [`InputProfile`] names the three input architectures; only
//! [`InputProfile::Virtual`] is constructible. [`usb`] names the four levels of the USB
//! hierarchy and builds none of them — a USB bus is a bus, and §33's expansion bus is
//! the stage that would have one.

use alloc::vec::Vec;
use core::fmt;

use lazalith_types::DeviceId;

use crate::host_input::HostAction;
use crate::input::{Event, InputDevice, InputError};

/// A host's source of input events.
///
/// **`no_std` and `Debug`, for B7's reasons.** A backend here cannot open a window or
/// read a keyboard — those are host resources, and `lazalith-devices` is `no_std`
/// because it runs on the ISA target. `Sdl3InputBackend` lives in `lazalith-gui`.
///
/// The trait is a *source* rather than a sink deliberately. A sink would mean the host
/// calls `device.inject()` directly, which is what `HostScript::replay` used to do, and
/// it gives the host a handle on the guest's device. A source means the host is asked
/// and the device is only ever written by `pump_input`, which is this crate's function
/// and can therefore be checked.
pub trait InputBackend: fmt::Debug {
    /// The next event, `None` if the host has none right now, or a host failure.
    ///
    /// **Fallible, and that is the design rather than an accident.** An earlier draft had
    /// this return a bare `Option<Event>`, and the result was that the SDL backend had
    /// nowhere to put a failure and could only count it as a dropped event 2014 which
    /// made "SDL is broken" and "the keyboard is unplugged" indistinguishable to a
    /// caller, and those are the two diagnoses a user actually reports. A host that
    /// cannot answer says so.
    ///
    /// `None` still means "nothing yet", not "nothing ever": a host idle between key
    /// presses returns `Ok(None)` and is asked again.
    fn poll(&mut self) -> Result<Option<Event>, InputBackendError>;

    /// A device that has gone away, for a backend that has one.
    ///
    /// `None` for a backend with no notion of a device — a script, a replay, a
    /// synthetic source. A backend that *does* track devices reports which one it is
    /// currently reading, so a caller can tell "the keyboard is idle" from "the keyboard
    /// is not plugged in", which are different facts and both are `None` from `poll`.
    fn device(&self) -> Option<DeviceId> {
        None
    }
}

/// Why an input backend refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputBackendError {
    /// The device's queue is full, so an event was refused.
    ///
    /// Carries the device's own refusal rather than flattening it, because "the queue is
    /// full" and "the buffer was too small" are both things a caller acts on and
    /// neither is a host failure.
    Device(InputError),
    /// The host's source failed.
    Host {
        /// What was being asked of the host.
        operation: &'static str,
        /// What the host said.
        detail: alloc::string::String,
    },
}

impl fmt::Display for InputBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(source) => write!(f, "{source}"),
            Self::Host { operation, detail } => {
                write!(f, "the input backend could not {operation}: {detail}")
            }
        }
    }
}

impl core::error::Error for InputBackendError {}

impl From<InputError> for InputBackendError {
    fn from(source: InputError) -> Self {
        Self::Device(source)
    }
}

/// What happened when the host moved input into a device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputPump {
    /// The host had no event.
    Idle,
    /// Events were delivered.
    Delivered {
        /// How many.
        count: u64,
    },
    /// The device's queue filled up and the event was refused.
    ///
    /// **Reported, not retried and not dropped silently.** An event the host produced
    /// and the guest never saw is a keystroke the user typed and the program missed, and
    /// the only honest thing a pump can do is say so. The device's queue is the guest's
    /// to drain.
    QueueFull {
        /// The queue's limit.
        limit: usize,
    },
}

/// Moves events from a host backend into a device, up to `budget` of them.
///
/// **The host calls this, on the host's schedule** — the mirror of B7's `pump_display`.
/// It is a free function rather than a method on `InputDevice` for the same reason:
/// the device is the guest's, and a function the host calls is the host's.
///
/// `budget` bounds the work per pump so a host with a deep event backlog does not spend
/// an unbounded time inside one call. A budget of zero is refused rather than treated as
/// "do nothing", because a caller that computed a budget of zero has a bug and a
/// silent no-op would hide it.
pub fn pump_input(
    device: &mut InputDevice,
    backend: &mut dyn InputBackend,
    budget: u64,
) -> Result<InputPump, InputBackendError> {
    if budget == 0 {
        return Err(InputBackendError::Device(InputError::CapacityTooLarge {
            capacity: 0,
            limit: InputDevice::QUEUE_LIMIT as u64,
        }));
    }
    let mut count = 0u64;
    while count < budget {
        let event = match backend.poll() {
            Ok(Some(event)) => event,
            Ok(None) => break,
            Err(source) => return Err(source),
        };
        match device.inject(event) {
            Ok(()) => count += 1,
            Err(InputError::QueueFull { limit }) => return Ok(InputPump::QueueFull { limit }),
            Err(source) => return Err(source.into()),
        }
    }
    Ok(if count == 0 {
        InputPump::Idle
    } else {
        InputPump::Delivered { count }
    })
}

/// A backend that replays a fixed list of actions, for tests and for deterministic
/// replay.
///
/// **This is what `HostScript` became.** `replay(&self, device)` still exists and is
/// now a loop over `pump_input` with an unbounded budget, so a caller that wants the old
/// one-call behaviour keeps it and a caller that wants a bound or a second backend
/// alongside it does not have to give that up.
#[derive(Debug, Default)]
pub struct ScriptedInputBackend {
    actions: Vec<HostAction>,
    cursor: usize,
    /// Events from the action being served, not yet handed over.
    ///
    /// **This is a correctness requirement, not an optimisation.** One `HostAction` can
    /// produce several guest events 2014 `Printable` is a key *and* a character 2014 so a
    /// backend that returned an action.s first event and moved on would silently drop the
    /// rest. It was written that way first and `lazalith-stdlib` caught it: a guest that
    /// was promised six events was given one.
    pending: Vec<Event>,
    device: Option<DeviceId>,
}

impl ScriptedInputBackend {
    /// A backend with nothing to replay.
    pub fn new() -> Self {
        Self {
            actions: Vec::new(),
            cursor: 0,
            pending: Vec::new(),
            device: None,
        }
    }

    /// A backend that replays these actions, and claims to be this device.
    ///
    /// The `device` is what lets a caller tell "idle" from "not plugged in": a pump that
    /// returns `Idle` and a backend reporting `None` from `device()` means the host has
    /// a keyboard and it has nothing to say, which is not the same as a machine with no
    /// keyboard at all.
    pub fn from_actions(actions: Vec<HostAction>, device: DeviceId) -> Self {
        Self {
            actions,
            cursor: 0,
            pending: Vec::new(),
            device: Some(device),
        }
    }

    /// Adds an action to the end of the script.
    pub fn push(&mut self, action: HostAction) {
        self.actions.push(action);
    }

    /// How many actions are left.
    pub fn remaining(&self) -> usize {
        self.actions.len().saturating_sub(self.cursor)
    }

    /// Whether the script has been fully delivered.
    pub fn is_exhausted(&self) -> bool {
        self.remaining() == 0 && self.pending.is_empty()
    }
}

impl InputBackend for ScriptedInputBackend {
    fn poll(&mut self) -> Result<Option<Event>, InputBackendError> {
        loop {
            if let Some(event) = self.pending.first().copied() {
                self.pending.remove(0);
                return Ok(Some(event));
            }
            if self.cursor >= self.actions.len() {
                return Ok(None);
            }
            let action = self.actions[self.cursor];
            self.cursor += 1;
            // An action with no events is a no-op, not the end of the script: a
            // `HostAction` that produces nothing is a caller that said "nothing now",
            // and treating it as exhaustion would end a replay early for no reason.
            self.pending = action.events();
        }
    }

    fn device(&self) -> Option<DeviceId> {
        self.device
    }
}

/// An input backend with nothing behind it, for a headless machine.
///
/// It reports no device and never produces an event, which is a *refusal to claim* input
/// rather than a device that silently eats keystrokes. A machine with
/// `AbsentInputBackend` has no keyboard, and a program that polls one learns that
/// nothing is coming rather than waiting forever for a keypress that was never going to
/// arrive.
#[derive(Debug, Default)]
pub struct AbsentInputBackend {
    reason: Option<DeviceId>,
}

impl AbsentInputBackend {
    /// A machine with no input at all.
    pub const fn new() -> Self {
        Self { reason: None }
    }

    /// A machine whose input device is missing, attributable to the device that asked.
    pub const fn missing_for(reason: DeviceId) -> Self {
        Self {
            reason: Some(reason),
        }
    }

    /// The device whose input is missing.
    pub const fn reason(&self) -> Option<DeviceId> {
        self.reason
    }
}

impl InputBackend for AbsentInputBackend {
    fn poll(&mut self) -> Result<Option<Event>, InputBackendError> {
        Ok(None)
    }

    fn device(&self) -> Option<DeviceId> {
        None
    }
}

/// An input architecture, as §30 names them.
///
/// Only [`InputProfile::Virtual`] builds. PS/2 and USB HID belong to the compatibility
/// machine (B27) and to a USB bus (§33), and a PS/2 controller needs the PIO address
/// space that LZA does not have — B4's recorded, still-open question.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum InputProfile {
    /// A modern virtual input device: an event queue behind a register window. What
    /// exists.
    Virtual,
    /// A PS/2 keyboard and mouse on the legacy ports.
    Ps2,
    /// A USB HID device behind a USB controller.
    UsbHid,
}

impl InputProfile {
    /// Whether this build can construct an input device of this profile.
    pub const fn is_constructible(self) -> bool {
        matches!(self, Self::Virtual)
    }

    /// The name, for a diagnostic and for a profile that names one.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Virtual => "virtual",
            Self::Ps2 => "ps/2",
            Self::UsbHid => "usb-hid",
        }
    }
}

impl fmt::Display for InputProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// §30's USB hierarchy, named and not built.
///
/// ```text
/// USB controller  →  USB bus  →  USB device  →  host backend
/// ```
///
/// Four levels, and the reason they are four is worth writing down: the *host backend*
/// is B9's business and exists, and the other three are a bus. §33's expansion bus is
/// the stage that would have one, and a USB device with no bus behind it is a device
/// that can be described and not attached.
pub mod usb {
    use core::fmt;

    /// A level of §30's USB hierarchy.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
    pub enum UsbLevel {
        /// The host-side controller that owns a bus.
        Controller,
        /// The bus itself: addresses, bandwidth, and who is attached to what.
        Bus,
        /// An attached device.
        Device,
    }

    impl UsbLevel {
        /// Whether this build can construct anything at this level.
        ///
        /// All `false`. A USB device with no bus behind it is a device that can be
        /// described and not attached, and a bus with no expansion-bus architecture
        /// behind it is a list of names.
        pub const fn is_buildable(self) -> bool {
            false
        }

        /// The name.
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Controller => "controller",
                Self::Bus => "bus",
                Self::Device => "device",
            }
        }
    }

    impl fmt::Display for UsbLevel {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.as_str())
        }
    }

    /// The whole hierarchy, in the order §30 draws it.
    pub const LEVELS: [UsbLevel; 3] = [UsbLevel::Controller, UsbLevel::Bus, UsbLevel::Device];
}
