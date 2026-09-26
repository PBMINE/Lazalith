//! Step 71: the host input adapter, and the Lazen key codes it produces.
//!
//! The adapter is the only component that knows what a keyboard is, so these
//! tests are about the *translation* and about the two properties the design
//! asks for: that the same script always produces the same stream, and that a
//! program cannot tell a script from a graphical host because it never sees a
//! host code at all.

use std::collections::BTreeSet;

use lazalith_devices::{
    EventKind, HostAction, HostKey, HostScript, InputDevice, InputError, KEY_BACKSLASH,
    KEY_BACKSPACE, KEY_COMMA, KEY_DIGIT_FIRST, KEY_ENTER, KEY_EQUALS, KEY_ESCAPE, KEY_LEFT_ALT,
    KEY_LEFT_CONTROL, KEY_LEFT_SHIFT, KEY_LEFT_SUPER, KEY_LETTER_FIRST, KEY_LETTER_LAST, KEY_MAX,
    KEY_MINUS, KEY_RIGHT_ALT, KEY_RIGHT_CONTROL, KEY_RIGHT_SHIFT, KEY_RIGHT_SUPER, KEY_SEMICOLON,
    KEY_SPACE, KEY_TAB, KEY_UNKNOWN, digit_of, is_digit, is_letter, letter_of,
};

/// The host numbering is SDL3's, stated as numbers.
///
/// These were read out of SDL 3.4.16's `SDL_scancode.h`. The point of pinning
/// them is that they are the one thing in Lazalith that cannot be checked against
/// anything inside the build: if SDL renumbers a scancode, this is the test that
/// says so, and the fix is the one enum rather than the mapping.
#[test]
fn the_host_numbering_is_sdl3s() {
    let expected = [
        (HostKey::A, 4),
        (HostKey::B, 5),
        (HostKey::Z, 29),
        (HostKey::Digit1, 30),
        (HostKey::Digit9, 38),
        (HostKey::Digit0, 39),
        (HostKey::Return, 40),
        (HostKey::Escape, 41),
        (HostKey::Backspace, 42),
        (HostKey::Tab, 43),
        (HostKey::Space, 44),
        (HostKey::Minus, 45),
        (HostKey::Equals, 46),
        (HostKey::Backslash, 49),
        (HostKey::Semicolon, 51),
        (HostKey::Comma, 54),
        (HostKey::Period, 55),
        (HostKey::Slash, 56),
        (HostKey::LeftControl, 224),
        (HostKey::LeftShift, 225),
        (HostKey::LeftAlt, 226),
        (HostKey::LeftSuper, 227),
        (HostKey::RightControl, 228),
        (HostKey::RightShift, 229),
        (HostKey::RightAlt, 230),
        (HostKey::RightSuper, 231),
    ];
    for (key, scancode) in expected {
        assert_eq!(key.as_u32(), scancode, "{key:?} is SDL scancode {scancode}");
    }
}

/// Lazen's own key codes are frozen, and they are not a host's.
///
/// A program compiled against this table has to keep working when the host
/// adapter changes, which it cannot do if the guest-visible codes are the host's
/// numbers. The host numbers above and the Lazen numbers here are deliberately
/// different, and that difference is the contract.
#[test]
fn the_lazen_key_codes_are_frozen_and_not_a_hosts() {
    let expected = [
        (KEY_UNKNOWN, 0),
        (KEY_LEFT_CONTROL, 1),
        (KEY_RIGHT_CONTROL, 2),
        (KEY_LEFT_SHIFT, 3),
        (KEY_RIGHT_SHIFT, 4),
        (KEY_LEFT_ALT, 5),
        (KEY_RIGHT_ALT, 6),
        (KEY_LEFT_SUPER, 7),
        (KEY_RIGHT_SUPER, 8),
        (KEY_BACKSPACE, 9),
        (KEY_TAB, 10),
        (KEY_ENTER, 11),
        (KEY_ESCAPE, 12),
        (KEY_SPACE, 13),
        (KEY_MINUS, 14),
        (KEY_EQUALS, 15),
        (KEY_BACKSLASH, 16),
    ];
    for (code, frozen) in expected {
        assert_eq!(code, frozen, "Lazen key code {frozen} is frozen");
    }
    assert_eq!(
        KEY_LETTER_FIRST, 17,
        "the letters start after the named keys"
    );
    assert_eq!(
        KEY_DIGIT_FIRST,
        KEY_LETTER_LAST + 1,
        "the digits follow the letters"
    );
    assert_eq!(KEY_MAX, KEY_SEMICOLON + 1);
    // A host code and a Lazen code for the same key are different numbers, which
    // is what makes the translation a real step rather than a rename.
    assert_ne!(HostKey::A.as_u32(), HostKey::A.lazen_code());
    assert_ne!(HostKey::LeftShift.as_u32(), KEY_LEFT_SHIFT);
}

/// The letters and digits are one contiguous range each, in ASCII order.
///
/// The design asks for this so a program can classify a key with comparisons
/// instead of a table. The test is the property itself: every letter is exactly
/// where its distance from the start of the range says it is, with no table
/// anywhere.
#[test]
fn letters_and_digits_are_one_contiguous_range_each() {
    assert!(is_letter(KEY_LETTER_FIRST), "'a' is a letter");
    assert!(is_letter(KEY_LETTER_LAST), "'z' is a letter");
    assert!(!is_letter(KEY_LETTER_FIRST - 1), "the key before is not");
    assert!(!is_letter(KEY_LETTER_FIRST + 26), "the digit block is not");

    assert!(is_digit(KEY_DIGIT_FIRST), "'0' is a digit");
    assert!(is_digit(KEY_DIGIT_FIRST + 9), "'9' is a digit");
    assert!(!is_digit(KEY_DIGIT_FIRST - 1), "the letter block is not");
    assert!(
        !is_digit(KEY_DIGIT_FIRST + 10),
        "and the punctuation is not"
    );

    for (index, letter) in "abcdefghijklmnopqrstuvwxyz".bytes().enumerate() {
        let code = KEY_LETTER_FIRST + index as u32;
        assert_eq!(letter_of(code), Some(letter), "'{letter}' is code {}", code);
    }
    for (index, digit) in "0123456789".bytes().enumerate() {
        let code = KEY_DIGIT_FIRST + index as u32;
        assert_eq!(
            digit_of(code),
            Some(digit),
            "'{}' is code {code}",
            digit as char
        );
    }
    assert_eq!(letter_of(KEY_SPACE), None, "space is not a letter");
    assert_eq!(digit_of(KEY_COMMA), None, "comma is not a digit");
    assert_eq!(letter_of(KEY_MAX), None, "and nothing past the end is");
}

/// Each letter key maps to the code that classifies as that letter.
///
/// This is the property a program actually relies on: pressing `q` and testing
/// `letter_of(code) == Some(b'q')` has to work, which means the adapter's table
/// and the range arithmetic have to agree.
#[test]
fn every_letter_key_maps_to_the_code_for_that_letter() {
    let letters = [
        (HostKey::A, b'a'),
        (HostKey::B, b'b'),
        (HostKey::C, b'c'),
        (HostKey::D, b'd'),
        (HostKey::E, b'e'),
        (HostKey::F, b'f'),
        (HostKey::G, b'g'),
        (HostKey::H, b'h'),
        (HostKey::I, b'i'),
        (HostKey::J, b'j'),
        (HostKey::K, b'k'),
        (HostKey::L, b'l'),
        (HostKey::M, b'm'),
        (HostKey::N, b'n'),
        (HostKey::O, b'o'),
        (HostKey::P, b'p'),
        (HostKey::Q, b'q'),
        (HostKey::R, b'r'),
        (HostKey::S, b's'),
        (HostKey::T, b't'),
        (HostKey::U, b'u'),
        (HostKey::V, b'v'),
        (HostKey::W, b'w'),
        (HostKey::X, b'x'),
        (HostKey::Y, b'y'),
        (HostKey::Z, b'z'),
    ];
    for (key, letter) in letters {
        let code = key.lazen_code();
        assert_eq!(
            letter_of(code),
            Some(letter),
            "{key:?} types {letter} and says so"
        );
        assert_eq!(
            key.unshifted_character(),
            Some(letter as char),
            "and offers the same character as text"
        );
    }
    for (index, digit) in "0123456789".bytes().enumerate() {
        let key = match index {
            0 => HostKey::Digit0,
            other => match other {
                1 => HostKey::Digit1,
                2 => HostKey::Digit2,
                3 => HostKey::Digit3,
                4 => HostKey::Digit4,
                5 => HostKey::Digit5,
                6 => HostKey::Digit6,
                7 => HostKey::Digit7,
                8 => HostKey::Digit8,
                _ => HostKey::Digit9,
            },
        };
        assert_eq!(
            digit_of(key.lazen_code()),
            Some(digit),
            "{key:?} is the digit {}",
            digit as char
        );
    }
}

/// Every key the adapter names maps to a distinct, real code.
///
/// A named key translating to `Unknown` would be a silent hole: the program would
/// see a key it cannot test, with no way to know which key it lost. Two keys
/// sharing a code would be worse — a program could not tell them apart at all.
#[test]
fn every_named_key_maps_to_a_distinct_real_code() {
    let named = [
        HostKey::A,
        HostKey::B,
        HostKey::C,
        HostKey::D,
        HostKey::E,
        HostKey::F,
        HostKey::G,
        HostKey::H,
        HostKey::I,
        HostKey::J,
        HostKey::K,
        HostKey::L,
        HostKey::M,
        HostKey::N,
        HostKey::O,
        HostKey::P,
        HostKey::Q,
        HostKey::R,
        HostKey::S,
        HostKey::T,
        HostKey::U,
        HostKey::V,
        HostKey::W,
        HostKey::X,
        HostKey::Y,
        HostKey::Z,
        HostKey::Digit0,
        HostKey::Digit1,
        HostKey::Digit2,
        HostKey::Digit3,
        HostKey::Digit4,
        HostKey::Digit5,
        HostKey::Digit6,
        HostKey::Digit7,
        HostKey::Digit8,
        HostKey::Digit9,
        HostKey::Comma,
        HostKey::Period,
        HostKey::Slash,
        HostKey::Semicolon,
        HostKey::Minus,
        HostKey::Equals,
        HostKey::Backslash,
        HostKey::Return,
        HostKey::Escape,
        HostKey::Backspace,
        HostKey::Tab,
        HostKey::Space,
        HostKey::LeftControl,
        HostKey::RightControl,
        HostKey::LeftShift,
        HostKey::RightShift,
        HostKey::LeftAlt,
        HostKey::RightAlt,
        HostKey::LeftSuper,
        HostKey::RightSuper,
    ];
    let mut seen = BTreeSet::new();
    for key in named {
        let code = key.lazen_code();
        assert_ne!(
            code, KEY_UNKNOWN,
            "{key:?} is named, so it must not translate to Unknown"
        );
        assert!(code < KEY_MAX, "{key:?} is within the assigned range");
        assert!(seen.insert(code), "{key:?} collides with code {code}");
    }
    assert_eq!(seen.len(), named.len(), "no two keys share a code");
    assert_eq!(HostKey::Unknown.lazen_code(), KEY_UNKNOWN);
}

/// A printable key produces its key first and then its text.
///
/// The order is part of the contract: a program that reads keys and a program
/// that reads text must see the same press, in the same place in the stream.
#[test]
fn a_printable_key_is_its_key_then_its_text() {
    let events = HostAction::Printable(HostKey::A).events();
    assert_eq!(events.len(), 2, "a printable key is two events");
    assert!(events[0].is(EventKind::KeyDown));
    assert_eq!(events[0].code, KEY_LETTER_FIRST, "'a' is the first letter");
    assert!(events[1].is(EventKind::Text));
    assert_eq!(events[1].code, u32::from(b'a'));

    // A key with no character produces only its key, and does not invent a text
    // event for a character that does not exist.
    let shift = HostAction::Printable(HostKey::LeftShift).events();
    assert_eq!(shift.len(), 1, "shift is a key with no text");
    assert!(shift[0].is(EventKind::KeyDown));
    assert_eq!(shift[0].code, KEY_LEFT_SHIFT);
}

/// A pointer event carries an absolute position and a key event does not.
///
/// Every field of a record is defined for every kind and an unused one is zero,
/// so a reader never has to ask which fields are meaningful.
#[test]
fn a_pointer_event_carries_a_position_and_a_key_event_does_not() {
    let movement = HostAction::MouseMove(11, -3).events();
    assert_eq!(movement.len(), 1);
    assert_eq!(movement[0].x, 11);
    assert_eq!(movement[0].y, -3);
    assert_eq!(movement[0].code, 0, "a move has no button");

    let down = HostAction::MouseDown(1, 4, 5).events();
    assert_eq!(down[0].code, 1, "the button index is the code");
    assert_eq!(down[0].x, 4);
    assert_eq!(down[0].y, 5);

    let key = HostAction::KeyDown(HostKey::Z).events();
    assert_eq!(key[0].x, 0, "a key has no position");
    assert_eq!(key[0].y, 0);
    assert_eq!(key[0].code, KEY_LETTER_LAST, "'z' is the last letter");
}

/// A text event carries the character, not a key code.
///
/// The two are separate events precisely because a program that wants characters
/// and a program that wants keys are different programs, and an input method may
/// produce text with no key at all.
#[test]
fn a_text_event_carries_the_character() {
    for character in ['a', 'Z', '7', 'é', '中'] {
        let events = HostAction::Text(character).events();
        assert_eq!(events.len(), 1);
        assert!(events[0].is(EventKind::Text));
        assert_eq!(
            events[0].code, character as u32,
            "the code is the scalar value"
        );
    }
}

/// The same script produces the same stream, every time.
///
/// This is the property that makes a graphical program testable headlessly, and
/// it is a property of the *adapter* rather than of the device: the script is
/// written in host terms and the translation is deterministic.
#[test]
fn the_same_script_produces_the_same_stream() {
    let mut script = HostScript::new();
    script.push(HostAction::Printable(HostKey::H));
    script.push(HostAction::KeyUp(HostKey::H));
    script.push(HostAction::MouseMove(3, 4));
    script.push(HostAction::MouseDown(0, 3, 4));
    script.push(HostAction::MouseUp(0, 3, 4));
    script.push(HostAction::Quit);

    let mut first = InputDevice::new();
    script.replay(&mut first).expect("the script fits");
    for _ in 0..4 {
        let mut again = InputDevice::new();
        script.replay(&mut again).expect("the same script fits");
        assert_eq!(
            first.pending(),
            again.pending(),
            "the same host script is the same guest stream"
        );
    }
}

/// A script that does not fit stops at the first refusal, without reordering.
///
/// Skipping the action that did not fit would deliver a stream whose later events
/// arrived before earlier ones, and a program whose keys were out of order would
/// have no way to tell.
#[test]
fn a_script_that_does_not_fit_stops_at_the_first_refusal() {
    let mut script = HostScript::new();
    for _ in 0..InputDevice::QUEUE_LIMIT {
        script.push(HostAction::KeyDown(HostKey::A));
    }
    let mut device = InputDevice::new();
    // One more than the queue holds, so the last action cannot fit.
    script.push(HostAction::KeyDown(HostKey::B));
    let error = script.replay(&mut device).expect_err("the queue is full");
    assert!(
        matches!(error, InputError::QueueFull { .. }),
        "the refusal is the device's, not the adapter's: {error:?}"
    );
    assert_eq!(device.queued(), InputDevice::QUEUE_LIMIT as u64);
    assert!(
        device
            .pending()
            .iter()
            .all(|event| event.code == KEY_LETTER_FIRST),
        "everything that got in is the first action's event, in order"
    );
}

/// A key the adapter does not name still arrives, as `Unknown`.
///
/// Dropping it would lose a key the user pressed. A code a program cannot
/// interpret but can count is more useful than a key that vanished.
#[test]
fn an_unnamed_key_arrives_as_unknown_rather_than_being_dropped() {
    let events = HostAction::KeyDown(HostKey::Unknown).events();
    assert_eq!(events.len(), 1, "the event is still delivered");
    assert!(events[0].is(EventKind::KeyDown));
    assert_eq!(events[0].code, KEY_UNKNOWN);
}

/// Every event kind the ABI names has a host action that produces it.
///
/// A kind the design promises with no way to produce it would be a promise the
/// device cannot keep, and a program waiting for that event would wait forever.
#[test]
fn every_event_kind_can_be_produced_by_a_host_action() {
    let produced: BTreeSet<u32> = [
        HostAction::KeyDown(HostKey::A),
        HostAction::KeyUp(HostKey::A),
        HostAction::MouseMove(0, 0),
        HostAction::MouseDown(0, 0, 0),
        HostAction::MouseUp(0, 0, 0),
        HostAction::Text('a'),
        HostAction::Quit,
    ]
    .into_iter()
    .flat_map(|action| action.events())
    .map(|event| event.kind_value())
    .collect();
    for kind in EventKind::ALL {
        assert!(
            produced.contains(&kind.as_u32()),
            "{kind:?} is in the ABI and some action produces it"
        );
    }
    assert_eq!(produced.len(), EventKind::ALL.len());
}

/// No host code reaches the guest stream.
///
/// This is the property the whole file exists for: if a host number could reach
/// the guest, a host renumbering would renumber the guest with it.
#[test]
fn no_host_code_reaches_the_guest() {
    let mut script = HostScript::new();
    script.push(HostAction::KeyDown(HostKey::A));
    script.push(HostAction::KeyDown(HostKey::LeftShift));
    script.push(HostAction::KeyDown(HostKey::Digit4));
    let mut device = InputDevice::new();
    script.replay(&mut device).expect("the script fits");
    let codes: Vec<u32> = device.pending().iter().map(|event| event.code).collect();
    assert_eq!(
        codes,
        vec![KEY_LETTER_FIRST, KEY_LEFT_SHIFT, KEY_DIGIT_FIRST + 4],
        "each key arrives as its Lazen code"
    );
    for code in codes {
        assert!(
            code < KEY_MAX,
            "{code} is a Lazen code, not a host scancode"
        );
    }
}
