//! The virtual input device.
//!
//! The claims being tested are the ones the design rests on: events are **queued**
//! rather than sampled, a poll that cannot take everything **keeps the rest**, an
//! event record defines every field for every kind, and the device holds nothing
//! that belongs to a host.

extern crate alloc;

use lazalith_devices::Device;
use lazalith_devices::{
    EVENT_BYTES, Event, EventKind, EventKindValue, INPUT_ABI_VERSION, InputDevice, InputError,
    MAX_POLL_CAPACITY, STATUS_INJECTED,
};
use lazalith_isa::DataSize;
use lazalith_types::DeviceOffset;

/// A buffer big enough for any poll in these tests.
fn room(count: usize) -> Vec<Event> {
    alloc::vec![Event::default(); count]
}

/// An event arrives, and a poll returns it.
#[test]
fn an_injected_event_is_polled_back() {
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::KeyDown, 16))
        .expect("injects");
    let mut out = room(4);
    assert_eq!(device.poll(&mut out, 4).expect("polls"), 1);
    assert!(
        out[0].is(EventKind::KeyDown),
        "and it is the event injected"
    );
    assert_eq!(out[0].code, 16, "with the code it carried");
}

/// **The central property.** A poll that cannot take every pending event keeps the
/// rest for the next one. Nothing is dropped to make a buffer fit, because a
/// dropped key press is a program that intermittently misbehaves with no way to
/// tell why.
#[test]
fn a_poll_that_cannot_take_everything_keeps_the_rest() {
    let mut device = InputDevice::new();
    for code in 1..=5u32 {
        device
            .inject(Event::new(EventKind::KeyDown, code))
            .expect("injects");
    }
    let mut out = room(2);
    assert_eq!(device.poll(&mut out, 2).expect("polls"), 2, "it took two");
    assert_eq!(out[0].code, 1, "the first two, in order");
    assert_eq!(out[1].code, 2);
    assert_eq!(device.queued(), 3, "and three are still waiting");

    let mut out = room(8);
    assert_eq!(
        device.poll(&mut out, 8).expect("polls again"),
        3,
        "the next poll takes the remainder"
    );
    assert_eq!(out[0].code, 3, "and it is the *next* events, not a repeat");
    assert_eq!(out[2].code, 5);
    assert_eq!(
        device.queued(),
        0,
        "so nothing was lost and nothing repeated"
    );
}

/// Events are delivered in the order they were received.
#[test]
fn events_are_delivered_in_the_order_they_arrived() {
    let mut device = InputDevice::new();
    let script = [
        Event::new(EventKind::KeyDown, 16),
        Event::new(EventKind::KeyDown, 17),
        Event::new(EventKind::KeyUp, 16),
        Event::pointer(EventKind::MouseMove, 0, 10, 20),
        Event::new(EventKind::Text, b'a' as u32),
    ];
    for event in script {
        device.inject(event).expect("injects");
    }
    let mut out = room(8);
    assert_eq!(device.poll(&mut out, 8).expect("polls"), 5);
    for (index, event) in script.iter().enumerate() {
        assert_eq!(out[index], *event, "event {index} arrived unchanged");
    }
}

/// A poll that finds nothing returns zero, and zero means "nothing pending"
/// rather than an error.
#[test]
fn polling_an_empty_queue_is_zero_and_not_an_error() {
    let mut device = InputDevice::new();
    let mut out = room(4);
    assert_eq!(device.poll(&mut out, 4).expect("polls"), 0);
    assert_eq!(device.queued(), 0, "and there was nothing to deliver");
}

/// A poll cannot ask for more events than the caller's buffer holds.
#[test]
fn a_poll_cannot_exceed_the_callers_buffer() {
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::Quit, 0))
        .expect("injects");
    let mut out = room(2);
    assert_eq!(
        device.poll(&mut out, 8),
        Err(InputError::CapacityExceedsBuffer {
            capacity: 8,
            buffer: 2,
        }),
        "a capacity past the buffer would have the device write past it"
    );
    assert_eq!(device.queued(), 1, "and the event is still queued");
}

/// A poll cannot ask for an unbounded number of events.
#[test]
fn a_poll_cannot_exceed_the_devices_limit() {
    let mut device = InputDevice::new();
    let mut out = room(MAX_POLL_CAPACITY as usize + 1);
    assert_eq!(
        device.poll(&mut out, MAX_POLL_CAPACITY + 1),
        Err(InputError::CapacityTooLarge {
            capacity: MAX_POLL_CAPACITY as usize + 1,
            limit: MAX_POLL_CAPACITY,
        })
    );
}

/// A record defines every field for every kind, and an unused one is zero.
#[test]
fn a_record_defines_every_field_for_every_kind() {
    let key = Event::new(EventKind::KeyDown, 16);
    assert_eq!(key.x, 0, "a key event has no pointer position");
    assert_eq!(key.y, 0);
    let move_event = Event::pointer(EventKind::MouseMove, 0, -3, 7);
    assert_eq!(move_event.code, 0, "a move has no button or key code");
    let encoded = key.encode();
    assert_eq!(encoded.len(), EVENT_BYTES as usize, "a record is 16 bytes");
    assert_eq!(Event::decode(&encoded), Some(key), "and round trips");
    assert_eq!(
        Event::decode(&encoded[..15]),
        None,
        "a short record is not one"
    );
}

/// A record's fields are at the offsets the ABI fixes, little-endian.
#[test]
fn a_records_fields_are_at_the_abi_offsets() {
    let event = Event::pointer(EventKind::MouseDown, 2, 0x0102_0304, -1);
    let bytes = event.encode();
    assert_eq!(&bytes[0..4], &4u32.to_le_bytes(), "kind at 0x00");
    assert_eq!(&bytes[4..8], &2u32.to_le_bytes(), "code at 0x04");
    assert_eq!(&bytes[8..12], &0x0102_0304u32.to_le_bytes(), "x at 0x08");
    assert_eq!(&bytes[12..16], &(-1i32).to_le_bytes(), "y at 0x0c");
}

/// Every kind has a stable number, and the numbers round trip.
#[test]
fn every_kind_has_a_stable_number() {
    let expected = [
        (EventKind::KeyDown, 1),
        (EventKind::KeyUp, 2),
        (EventKind::MouseMove, 3),
        (EventKind::MouseDown, 4),
        (EventKind::MouseUp, 5),
        (EventKind::Text, 6),
        (EventKind::Quit, 7),
    ];
    for (kind, number) in expected {
        assert_eq!(kind.as_u32(), number, "{kind:?} is number {number}");
        assert_eq!(
            EventKind::from_u32(number),
            Some(kind),
            "and it comes back from its number"
        );
    }
    assert_eq!(EventKind::from_u32(0), None, "zero is not a kind");
    assert_eq!(EventKind::from_u32(8), None, "and neither is past the last");
    assert_eq!(EventKind::ALL.len(), expected.len(), "all seven are listed");
}

/// An unknown kind is *held*, not refused, so a program can skip an event it does
/// not understand rather than failing on a newer device.
#[test]
fn an_unknown_kind_is_held_rather_than_refused() {
    let mut bytes = Event::new(EventKind::Quit, 0).encode();
    bytes[0..4].copy_from_slice(&99u32.to_le_bytes());
    let event = Event::decode(&bytes).expect("a record with an unknown kind decodes");
    assert_eq!(event.kind_value(), 99, "the number is kept as it was");
    assert_eq!(
        event.kind(),
        None,
        "and this build does not claim to know it"
    );
    assert_eq!(
        event.kind,
        EventKindValue(99),
        "while the field is still the number, not a guess"
    );
    assert!(!event.is(EventKind::Quit), "and it is not any kind we know");
}

/// A `Text` event and a `KeyDown` for the same press are not duplicates.
#[test]
fn text_and_keydown_are_different_events_for_one_press() {
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::KeyDown, 16))
        .expect("injects the key");
    device
        .inject(Event::new(EventKind::Text, b'q' as u32))
        .expect("injects the character");
    let mut out = room(4);
    assert_eq!(device.poll(&mut out, 4).expect("polls"), 2);
    assert!(out[0].is(EventKind::KeyDown), "the key is one event");
    assert!(out[1].is(EventKind::Text), "the character is another");
    assert_eq!(out[1].code, b'q' as u32, "carrying the character itself");
}

/// The queue is bounded, and a full queue refuses rather than dropping.
#[test]
fn a_full_queue_refuses_rather_than_dropping() {
    let mut device = InputDevice::new();
    for _ in 0..InputDevice::QUEUE_LIMIT {
        device
            .inject(Event::new(EventKind::KeyDown, 1))
            .expect("injects");
    }
    assert_eq!(
        device.inject(Event::new(EventKind::KeyDown, 2)),
        Err(InputError::QueueFull {
            limit: InputDevice::QUEUE_LIMIT
        }),
        "a full queue refuses, because dropping the oldest would lose input"
    );
    // Draining makes room again, and the refused event can then be injected.
    let mut out = room(InputDevice::QUEUE_LIMIT);
    device
        .poll(&mut out, MAX_POLL_CAPACITY)
        .expect("drains some");
    device
        .inject(Event::new(EventKind::KeyDown, 2))
        .expect("and now it fits");
}

/// `inject_all` stops at the first refusal rather than skipping.
#[test]
fn injecting_several_stops_at_the_first_refusal() {
    let mut device = InputDevice::new();
    for _ in 0..InputDevice::QUEUE_LIMIT {
        device
            .inject(Event::new(EventKind::KeyDown, 1))
            .expect("fills");
    }
    let extra = [
        Event::new(EventKind::KeyDown, 7),
        Event::new(EventKind::KeyDown, 8),
    ];
    assert!(device.inject_all(&extra).is_err());
    let mut out = room(4);
    device.poll(&mut out, 4).expect("drains some");
    assert!(
        device.pending().iter().all(|event| event.code != 7),
        "neither event got in, so a host that skipped one would reorder the stream"
    );
}

/// The counters distinguish queued, delivered and injected.
#[test]
fn the_counters_distinguish_queued_delivered_and_injected() {
    let mut device = InputDevice::new();
    assert_eq!(device.injected(), 0, "nothing injected yet");
    assert_eq!(device.delivered(), 0, "and nothing delivered");
    for code in 1..=3u32 {
        device
            .inject(Event::new(EventKind::KeyDown, code))
            .expect("injects");
    }
    assert_eq!(device.injected(), 3, "three injected");
    assert_eq!(device.delivered(), 0, "but none delivered yet");
    let mut out = room(2);
    device.poll(&mut out, 2).expect("polls");
    assert_eq!(device.delivered(), 2, "two delivered");
    assert_eq!(device.queued(), 1, "one still queued");
    assert_eq!(device.injected(), 3, "and the injected total is unchanged");
}

/// The last poll's count and capacity are readable, so a driver can check itself.
#[test]
fn the_last_polls_count_and_capacity_are_remembered() {
    let mut device = InputDevice::new();
    for code in 1..=4u32 {
        device
            .inject(Event::new(EventKind::KeyDown, code))
            .expect("injects");
    }
    let mut out = room(4);
    device.poll(&mut out, 3).expect("polls for three of four");
    assert_eq!(device.last_count(), 3, "three were written");
    assert_eq!(device.last_capacity(), 3, "and three were asked for");
}

/// Every register reads back what the device knows.
#[test]
fn the_registers_report_what_the_device_knows() {
    use lazalith_devices::input::REGISTER_ABI_VERSION as VERSION;
    use lazalith_devices::input::REGISTER_DELIVERED;
    use lazalith_devices::input::REGISTER_INJECTED;
    use lazalith_devices::input::REGISTER_LAST_CAPACITY;
    use lazalith_devices::input::REGISTER_LAST_COUNT;
    use lazalith_devices::input::REGISTER_PENDING;
    use lazalith_devices::input::REGISTER_STATUS;
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::Quit, 0))
        .expect("injects");
    let mut out = room(4);
    device.poll(&mut out, 4).expect("polls");

    assert_eq!(device.read(REGISTER_PENDING, DataSize::Double).unwrap(), 0);
    assert_eq!(
        device.read(REGISTER_DELIVERED, DataSize::Double).unwrap(),
        1
    );
    assert_eq!(device.read(REGISTER_INJECTED, DataSize::Double).unwrap(), 1);
    assert_eq!(
        device.read(VERSION, DataSize::Double).unwrap(),
        INPUT_ABI_VERSION
    );
    assert_eq!(
        device.read(REGISTER_STATUS, DataSize::Double).unwrap(),
        STATUS_INJECTED
    );
    assert_eq!(
        device.read(REGISTER_LAST_COUNT, DataSize::Double).unwrap(),
        1
    );
    assert_eq!(
        device
            .read(REGISTER_LAST_CAPACITY, DataSize::Double)
            .unwrap(),
        4
    );
}

/// The status distinguishes "the host has said nothing" from "nothing yet".
#[test]
fn the_status_distinguishes_no_input_from_no_events_yet() {
    use lazalith_devices::input::REGISTER_STATUS;
    let mut device = InputDevice::new();
    assert_eq!(device.read(REGISTER_STATUS, DataSize::Double).unwrap(), 0);
    device
        .inject(Event::new(EventKind::Text, 65))
        .expect("injects");
    assert_ne!(
        device.read(REGISTER_STATUS, DataSize::Double).unwrap(),
        0,
        "an injected event is distinguishable"
    );
}

/// Only the poll register is writable, and writing it drains nothing.
#[test]
fn only_the_poll_register_is_writable() {
    use lazalith_devices::input::REGISTER_ABI_VERSION as VERSION;
    use lazalith_devices::input::REGISTER_DELIVERED;
    use lazalith_devices::input::REGISTER_INJECTED;
    use lazalith_devices::input::REGISTER_LAST_COUNT;
    use lazalith_devices::input::REGISTER_PENDING;
    use lazalith_devices::input::REGISTER_POLL;
    use lazalith_devices::input::REGISTER_STATUS;
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::KeyDown, 1))
        .expect("injects");
    device
        .write(REGISTER_POLL, DataSize::Double, 0)
        .expect("the poll register is writable");
    assert_eq!(
        device.queued(),
        1,
        "but a write to it does not drain: the device has nowhere to put bytes"
    );
    for offset in [
        REGISTER_PENDING,
        REGISTER_DELIVERED,
        REGISTER_INJECTED,
        VERSION,
        REGISTER_STATUS,
        REGISTER_LAST_COUNT,
    ] {
        assert!(
            device.write(offset, DataSize::Double, 0).is_err(),
            "{} is written by the device",
            offset.as_u64()
        );
    }
}

/// A register access must be a whole double-word at a register offset.
#[test]
fn a_register_access_must_be_a_whole_word_at_a_register_offset() {
    let mut device = InputDevice::new();
    use lazalith_devices::input::REGISTER_PENDING;
    assert!(device.read(DeviceOffset::new(1), DataSize::Double).is_err());
    assert!(device.read(REGISTER_PENDING, DataSize::Byte).is_err());
    assert!(
        device
            .read(
                DeviceOffset::new(lazalith_devices::input::REGISTER_BYTES),
                DataSize::Double
            )
            .is_err()
    );
    assert!(device.read(REGISTER_PENDING, DataSize::Double).is_ok());
}

/// A reset discards the queue, and a key that was down before it is not down
/// after it.
#[test]
fn a_reset_discards_the_queue() {
    let mut device = InputDevice::new();
    device
        .inject(Event::new(EventKind::KeyDown, 16))
        .expect("injects");
    device.reset();
    assert_eq!(device.queued(), 0, "the queue is empty");
    assert_eq!(device.injected(), 0, "and the injected count is forgotten");
    let mut out = room(4);
    assert_eq!(device.poll(&mut out, 4).expect("polls"), 0);
}

/// The same script injected twice produces the same stream, which is what makes a
/// graphical program testable.
#[test]
fn the_same_script_produces_the_same_stream() {
    let script = [
        Event::new(EventKind::KeyDown, 16),
        Event::new(EventKind::Text, b'h' as u32),
        Event::new(EventKind::Text, b'i' as u32),
        Event::new(EventKind::KeyUp, 16),
        Event::new(EventKind::Quit, 0),
    ];
    let run = || {
        let mut device = InputDevice::new();
        device.inject_all(&script).expect("injects");
        let mut out = room(8);
        let count = device.poll(&mut out, 8).expect("polls");
        out.truncate(count as usize);
        out
    };
    assert_eq!(run(), run(), "two runs of one script agree exactly");
}

/// An input device's snapshot carries the queue, because the program owns it.
///
/// This is the property that makes an input snapshot worth taking. A snapshot that
/// kept the counters but not the queued events would restore a device that handed
/// the same keystroke to the program a second time — and the program is the thing
/// that drained it, so the repetition would be visible to the guest and not to
/// anybody else.
#[test]
fn an_input_snapshot_carries_the_queue() {
    use lazalith_devices::Device;
    let mut device = InputDevice::new();
    let key = Event::new(EventKind::KeyDown, 30);
    device.inject(key).expect("the event is queued");
    device.inject(key).expect("and another");
    assert_eq!(device.queued(), 2, "two events are waiting");

    let saved = device.snapshot();

    // Drain both, the way a program would: poll into an array of records.
    let mut events = [Event::decode(&[0u8; EVENT_BYTES as usize]).unwrap(); 4];
    let capacity = 4u64;
    let drained = device.poll(&mut events, capacity).expect("a poll");
    assert_eq!(drained, 2, "the device handed over both");
    assert_eq!(device.queued(), 0, "the queue is empty");

    device.restore(&saved).expect("a restore");
    assert_eq!(
        device.queued(),
        2,
        "both events are back, so restoring will not hand the program a third"
    );
    assert_eq!(
        device.delivered(),
        0,
        "and the delivered count came back with them, so the program is not told \
         it has already seen them"
    );
}

/// An input device refuses a snapshot that is not its shape.
#[test]
fn an_input_device_refuses_a_snapshot_that_is_not_its_shape() {
    use lazalith_devices::{Device, DeviceError};
    let mut device = InputDevice::new();
    // Only the *length* can be checked, and that is worth checking: every
    // sixteen-byte pattern decodes to some event — sixteen zero bytes are a
    // perfectly good "a key this build does not name, code zero" — so a length
    // is the only shape a restore can refuse, and a refused restore must leave
    // the device as it was rather than partly restored.
    for wrong in [3usize, 47, 49, 63] {
        assert!(
            matches!(
                device.restore(&vec![0u8; wrong]),
                Err(DeviceError::SnapshotShape { .. }),
            ),
            "{wrong} bytes is not a whole number of events plus the trailer"
        );
    }
    assert_eq!(device.queued(), 0, "and no refused restore queued anything");
}
