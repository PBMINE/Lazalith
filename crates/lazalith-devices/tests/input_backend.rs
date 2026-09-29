//! B9: the input backend boundary.
//!
//! # What is being held here
//!
//! **The device is written only by `pump_input`.** This is B7's asymmetry seen from the
//! other side: a display is pulled by the host, input is pushed *by* the host, and in
//! both cases the guest's device is never the thing that calls out. If the device asked
//! the host for events during a register read, a guest's read would call into SDL and a
//! host that had stopped answering would stall the machine with no fault and no timeout.
//!
//! So the boundary is a **source** — `InputBackend::poll` — and the device is only ever
//! written by a free function the host calls. A test that a pump does not read the
//! device's own counters as a side effect is the one that would catch a boundary quietly
//! eroding back into a sink.
//!
//! **`poll` is fallible, and that is the design.** An earlier draft returned a bare
//! `Option<Event>`, which left the SDL backend nowhere to put a failure: it could only
//! count it as a dropped event, making "SDL is broken" and "the keyboard is unplugged"
//! indistinguishable — and those are the two diagnoses a user actually reports. The
//! tests here check that a host failure is reported rather than swallowed.
//!
//! **A full queue is reported, not retried and not dropped.** An event the host
//! produced and the guest never saw is a keystroke the user typed and the program
//! missed.

use lazalith_devices::host_input::{HostAction, HostKey, HostScript};
use lazalith_devices::{
    AbsentInputBackend, Event, EventKind, InputBackend, InputDevice, InputError, InputProfile,
    InputPump, ScriptedInputBackend, pump_input, usb,
};
use lazalith_types::DeviceId;

const DEVICE: DeviceId = DeviceId::new(3);

fn key_event(code: u32) -> Event {
    Event::new(EventKind::KeyDown, code)
}

fn device() -> InputDevice {
    InputDevice::new()
}

/// A backend that returns a fixed list and then goes quiet.
struct Fixed {
    events: Vec<Event>,
    cursor: usize,
    fail_after: Option<usize>,
    polls: u64,
}

impl Fixed {
    fn new(events: Vec<Event>) -> Self {
        Self {
            events,
            cursor: 0,
            fail_after: None,
            polls: 0,
        }
    }

    /// Fails once the given number of events have been produced.
    fn failing_after(events: Vec<Event>, count: usize) -> Self {
        Self {
            events,
            cursor: 0,
            fail_after: Some(count),
            polls: 0,
        }
    }
}

impl core::fmt::Debug for Fixed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Fixed")
            .field("cursor", &self.cursor)
            .field("polls", &self.polls)
            .finish()
    }
}

impl InputBackend for Fixed {
    fn poll(&mut self) -> Result<Option<Event>, lazalith_devices::InputBackendError> {
        self.polls = self.polls.saturating_add(1);
        if self.fail_after == Some(self.cursor) {
            return Err(lazalith_devices::InputBackendError::Host {
                operation: "read an input event",
                detail: String::from("the host input device went away"),
            });
        }
        let event = self.events.get(self.cursor).copied();
        self.cursor = self.cursor.saturating_add(1);
        Ok(event)
    }

    fn device(&self) -> Option<DeviceId> {
        Some(DEVICE)
    }
}

// -- the pump -----------------------------------------------------------------

#[test]
fn a_pump_moves_events_into_the_device() {
    let mut device = device();
    let mut backend = Fixed::new(vec![key_event(1), key_event(2), key_event(3)]);
    let outcome = pump_input(&mut device, &mut backend, 8).expect("a pump succeeds");
    assert_eq!(outcome, InputPump::Delivered { count: 3 });
    assert_eq!(device.queued(), 3, "the device is holding all three");
    assert_eq!(
        backend.polls, 4,
        "three events and one that came back empty: a pump must ask once more to learn \
         there is nothing left, or it cannot know it delivered everything"
    );
}

#[test]
fn an_idle_host_pumps_nothing() {
    let mut device = device();
    let mut backend = Fixed::new(Vec::new());
    assert_eq!(
        pump_input(&mut device, &mut backend, 8).expect("a pump succeeds"),
        InputPump::Idle
    );
    assert_eq!(device.queued(), 0);
}

#[test]
fn a_budget_bounds_the_work_in_one_pump() {
    let mut device = device();
    let mut backend = Fixed::new((0..10u32).map(key_event).collect());
    let outcome = pump_input(&mut device, &mut backend, 4).expect("a pump succeeds");
    assert_eq!(outcome, InputPump::Delivered { count: 4 });
    assert_eq!(
        device.queued(),
        4,
        "so a host with a deep backlog does not spend an unbounded time inside one call"
    );
}

#[test]
fn a_zero_budget_is_refused_rather_than_silently_doing_nothing() {
    let mut device = device();
    let mut backend = Fixed::new(vec![key_event(1)]);
    let error = pump_input(&mut device, &mut backend, 0).expect_err("zero is refused");
    assert!(
        matches!(
            error,
            lazalith_devices::InputBackendError::Device(InputError::CapacityTooLarge { .. })
        ),
        "a caller that computed a budget of zero has a bug, and a silent no-op would \
         hide it until a program mysteriously received no input. Got {error:?}"
    );
}

#[test]
fn a_full_queue_is_reported_rather_than_dropped() {
    let mut device = device();
    // Fill it past the limit by pumping a long list.
    let events: Vec<Event> = (0..(InputDevice::QUEUE_LIMIT as u32 + 8))
        .map(key_event)
        .collect();
    let mut backend = Fixed::new(events);
    let outcome = pump_input(&mut device, &mut backend, u64::MAX)
        .expect("a full queue is an outcome, not an error");
    assert_eq!(
        outcome,
        InputPump::QueueFull {
            limit: InputDevice::QUEUE_LIMIT
        },
        "an event the host produced and the guest never saw is a keystroke the user typed \
         and the program missed; the only honest thing a pump can do is say so"
    );
    assert_eq!(device.queued(), InputDevice::QUEUE_LIMIT as u64);
}

#[test]
fn a_host_failure_is_reported_and_not_turned_into_idleness() {
    let mut device = device();
    let mut backend = Fixed::failing_after(vec![key_event(1), key_event(2)], 1);
    let error = pump_input(&mut device, &mut backend, 8).expect_err("the host fails");
    match error {
        lazalith_devices::InputBackendError::Host { operation, detail } => {
            assert_eq!(operation, "read an input event");
            assert!(
                detail.contains("went away"),
                "the host's own words survive, so a caller can report what happened: {detail}"
            );
        }
        other => panic!("a host failure must not be flattened into a device error: {other:?}"),
    }
    assert_eq!(
        device.queued(),
        1,
        "and the event the host did produce before failing was still delivered"
    );
}

// -- the boundary -------------------------------------------------------------

#[test]
fn a_pump_does_not_read_the_devices_counters_as_a_side_effect() {
    let mut device = device();
    let mut backend = Fixed::new(vec![key_event(1)]);
    pump_input(&mut device, &mut backend, 4).expect("a pump succeeds");
    let delivered = device.queued();
    let injected = device.injected();

    // Pump again with an empty host: nothing should change, because a pump that
    // delivered nothing did not deliver nothing *by touching the device*.
    let mut empty = Fixed::new(Vec::new());
    assert_eq!(
        pump_input(&mut device, &mut empty, 4).expect("a pump succeeds"),
        InputPump::Idle
    );
    assert_eq!(device.queued(), delivered, "an idle pump changed the queue");
    assert_eq!(
        device.injected(),
        injected,
        "and the injected counter, which a guest can read, moved for no reason"
    );
}

#[test]
fn the_device_never_reaches_the_backend() {
    // The source shape is what makes this hold: `InputDevice` has no method that takes
    // a backend, and a backend has no way to hold a device. What is tested here is the
    // consequence — a caller can have two sources feeding one device, which it could
    // not do while `replay` reached directly into the device.
    let mut device = device();
    let mut script =
        ScriptedInputBackend::from_actions(vec![HostAction::KeyDown(HostKey::A)], DEVICE);
    let mut absent = AbsentInputBackend::new();
    assert_eq!(
        pump_input(&mut device, &mut script, 4).expect("the script pumps"),
        InputPump::Delivered { count: 1 }
    );
    assert_eq!(
        pump_input(&mut device, &mut absent, 4).expect("the absent backend pumps"),
        InputPump::Idle,
        "and a second source is consulted without either one knowing about the other"
    );
    assert_eq!(device.queued(), 1);
}

// -- the architectures of §30 --------------------------------------------------

#[test]
fn only_the_virtual_input_profile_is_constructible() {
    assert!(InputProfile::Virtual.is_constructible());
    for profile in [InputProfile::Ps2, InputProfile::UsbHid] {
        assert!(
            !profile.is_constructible(),
            "{profile} is not constructible. PS/2 needs the PIO address space LZA does \
             not have — B4’s recorded, still-open question — and USB HID needs a bus, \
             which is §33."
        );
        assert!(!profile.as_str().is_empty());
    }
}

#[test]
fn the_usb_hierarchy_is_named_and_unbuilt() {
    assert_eq!(
        usb::LEVELS,
        [
            usb::UsbLevel::Controller,
            usb::UsbLevel::Bus,
            usb::UsbLevel::Device
        ],
        "§30 draws four levels and the host backend is B9’s, which exists; the other \
         three are a bus and are §33’s"
    );
    for level in usb::LEVELS {
        assert!(
            !level.is_buildable(),
            "a USB device with no bus behind it is a device that can be described and not \
             attached, and a bus with no expansion-bus architecture behind it is a list \
             of names: {level}"
        );
    }
}

// -- the scripted backend, and `replay` through it ----------------------------

#[test]
fn the_old_replay_still_works_and_goes_through_the_boundary() {
    let script = HostScript::from_actions(vec![
        HostAction::KeyDown(HostKey::A),
        HostAction::KeyDown(HostKey::B),
    ]);
    let mut device = device();
    let count = script.replay(&mut device).expect("the script fits");
    assert_eq!(count, 2, "two actions, two events");
    assert_eq!(device.queued(), 2);
}

#[test]
fn a_script_reports_which_device_it_is_and_when_it_is_done() {
    let mut backend =
        ScriptedInputBackend::from_actions(vec![HostAction::KeyDown(HostKey::A)], DEVICE);
    assert_eq!(
        backend.device(),
        Some(DEVICE),
        "so idle is not confused with unplugged"
    );
    assert_eq!(backend.remaining(), 1);
    assert!(!backend.is_exhausted());
    assert!(backend.poll().expect("a script does not fail").is_some());
    assert!(
        backend.is_exhausted(),
        "one action, one event, then nothing"
    );
    assert!(
        backend.poll().expect("a script does not fail").is_none(),
        "and a finished script stays finished rather than repeating"
    );
}

#[test]
fn an_empty_script_is_exhausted_rather_than_broken() {
    let backend = ScriptedInputBackend::new();
    assert!(backend.is_exhausted());
    assert_eq!(backend.device(), None, "and it claims no device");
}

#[test]
fn an_absent_backend_never_pretends_to_have_input() {
    let mut backend = AbsentInputBackend::missing_for(DEVICE);
    assert_eq!(
        backend.reason(),
        Some(DEVICE),
        "it says which device is missing"
    );
    assert_eq!(
        backend.device(),
        None,
        "and claims none, so a caller can tell 'no keyboard' from 'a keyboard with \
         nothing to say' — both are None from a poll, and only one of them is a fault"
    );
    assert!(
        backend
            .poll()
            .expect("an absent backend cannot fail")
            .is_none()
    );
}

#[test]
fn two_sources_can_feed_one_device() {
    // The concrete capability the boundary buys, and the reason `replay` was rerouted:
    // a deterministic script *and* a real keyboard, with neither reaching the device.
    let mut device = device();
    let mut script = ScriptedInputBackend::from_actions(
        vec![
            HostAction::KeyDown(HostKey::A),
            HostAction::KeyDown(HostKey::B),
        ],
        DEVICE,
    );
    let mut script2 =
        ScriptedInputBackend::from_actions(vec![HostAction::KeyDown(HostKey::C)], DeviceId::new(4));
    assert_eq!(
        pump_input(&mut device, &mut script, 4).expect("pumps"),
        InputPump::Delivered { count: 2 }
    );
    assert_eq!(
        pump_input(&mut device, &mut script2, 4).expect("pumps"),
        InputPump::Delivered { count: 1 }
    );
    assert_eq!(
        device.queued(),
        3,
        "three events from two independent sources"
    );
}

/// An action that produces several events delivers all of them, in order.
///
/// **This is a bug that was written, shipped, and caught by `lazalith-stdlib`.** The
/// first `ScriptedInputBackend` returned an action's *first* event and moved its cursor
/// on, so `HostAction::Printable` — which is a key *and* a character — lost its second
/// event. Nothing in the devices crate noticed, because every test used single-event
/// actions. A stdlib test that boots a real guest, replays a script through it, and has
/// the program count the events it received failed with "the program read the whole
/// script" and an empty output.
///
/// So the test is here now, at the level that can see it: a multi-event action, all of
/// its events, in order.
#[test]
fn a_multi_event_action_delivers_every_event_in_order() {
    let mut backend =
        ScriptedInputBackend::from_actions(vec![HostAction::Printable(HostKey::A)], DEVICE);
    let mut delivered = Vec::new();
    while let Some(event) = backend.poll().expect("a script does not fail") {
        delivered.push(event);
    }
    let expected = HostAction::Printable(HostKey::A).events();
    assert_eq!(
        delivered, expected,
        "every event of the action, in the order the action defines them -- a backend that \
         serves one event per action loses the rest of a Printable"
    );
    assert!(
        delivered.len() > 1,
        "and this test is only meaningful if the action really does produce several \
         events: it produced {}",
        delivered.len()
    );
    assert!(backend.is_exhausted());
}

/// The same, through the pump, because that is the path a real caller takes.
#[test]
fn a_multi_event_action_survives_the_pump() {
    let mut device = device();
    let script = HostScript::from_actions(vec![HostAction::Printable(HostKey::A)]);
    let expected = script.events().count() as u64;
    let delivered = script.replay(&mut device).expect("the script fits");
    assert_eq!(
        delivered, expected,
        "replay reports what it delivered, and a caller comparing that with what it \
         scripted is the check that catches a dropped event"
    );
    assert_eq!(device.queued(), expected);
}
