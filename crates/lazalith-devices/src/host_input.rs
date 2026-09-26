//! The host input adapter.
//!
//! # Where this sits
//!
//! ```text
//! SDL3 (Step 77) ─┐
//! a script ───────┴─→ HostInputAdapter ─→ Virtual Input Device ─→ LazOS driver
//! ```
//!
//! This is the **only** component that knows what a keyboard is. It holds the
//! host's own key numbering and Lazen's, and translates between them. Nothing
//! downstream of it — the device, the driver, the SDK, the application — ever
//! sees a host code, which is what makes the same Lazen program behave
//! identically under a graphical host and under a script.
//!
//! # What "the same program sees the same records" requires
//!
//! Two things, and only two. The *key code* a program tests must not depend on
//! the host, so Lazen's numbering is frozen here and is not a host's. And the
//! *order* must not depend on timing, so a [`HostScript`] replays a fixed
//! sequence rather than racing a real keyboard. Change this file's translation
//! and every program that used keys changes meaning; change anything else and
//! nothing does.
//!
//! # The host numbering here is SDL3's
//!
//! [`HostKey`] is numbered with SDL3's `SDL_Scancode` values, so the Step 77
//! frontend is a pass-through rather than a second translation. Those numbers
//! are the one thing in this file that cannot be checked from inside Lazalith —
//! there is no SDL in the build — so they were read out of SDL 3.4.16's
//! `SDL_scancode.h` and pinned by `the_host_numbering_is_sdl3s`, which states
//! them as numbers rather than as a comment. If SDL ever renumbers one, that test
//! is where it shows up, and the fix is this one enum: the mapping below it does
//! not change, which is why the numbering is confined to a single place.
//!
//! # Text is the host's to compose
//!
//! `key_down` and `key_up` carry a key; `text` carries a character. The adapter
//! does not derive one from the other, because case, dead keys and input methods
//! are host concerns and `docs/lazen-input.md` explicitly puts text editing in
//! the GUI library rather than here. A host that wants a printable key to
//! produce both uses [`HostAction::Printable`], which is a convenience and not a
//! rule: nothing stops a host sending a key with no text, or text with no key.

use alloc::vec::Vec;

use crate::input::{Event, EventKind, InputDevice, InputError};

/// A key, in the *host's* numbering.
///
/// This is SDL3's `SDL_Scancode`, which is what the Step 77 frontend receives
/// and therefore what it can pass straight through. The discriminants are
/// explicitly **not** Lazen key codes: translating is this file's job, and
/// keeping the two apart is what stops a host renumbering from renumbering the
/// guest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum HostKey {
    /// A key the host reported that this adapter does not name.
    Unknown = 0,
    /// `A`.
    A = 4,
    /// `B`.
    B = 5,
    /// `C`.
    C = 6,
    /// `D`.
    D = 7,
    /// `E`.
    E = 8,
    /// `F`.
    F = 9,
    /// `G`.
    G = 10,
    /// `H`.
    H = 11,
    /// `I`.
    I = 12,
    /// `J`.
    J = 13,
    /// `K`.
    K = 14,
    /// `L`.
    L = 15,
    /// `M`.
    M = 16,
    /// `N`.
    N = 17,
    /// `O`.
    O = 18,
    /// `P`.
    P = 19,
    /// `Q`.
    Q = 20,
    /// `R`.
    R = 21,
    /// `S`.
    S = 22,
    /// `T`.
    T = 23,
    /// `U`.
    U = 24,
    /// `V`.
    V = 25,
    /// `W`.
    W = 26,
    /// `X`.
    X = 27,
    /// `Y`.
    Y = 28,
    /// `Z`.
    Z = 29,
    /// `1`.
    Digit1 = 30,
    /// `2`.
    Digit2 = 31,
    /// `3`.
    Digit3 = 32,
    /// `4`.
    Digit4 = 33,
    /// `5`.
    Digit5 = 34,
    /// `6`.
    Digit6 = 35,
    /// `7`.
    Digit7 = 36,
    /// `8`.
    Digit8 = 37,
    /// `9`.
    Digit9 = 38,
    /// `0`.
    Digit0 = 39,
    /// Return, enter, or the keypad's enter.
    Return = 40,
    /// Escape.
    Escape = 41,
    /// Backspace.
    Backspace = 42,
    /// Tab.
    Tab = 43,
    /// Space.
    Space = 44,
    /// The minus key, `-`.
    Minus = 45,
    /// The equals key, `=`.
    Equals = 46,
    /// Backslash, `\`.
    Backslash = 49,
    /// Semicolon, `;`.
    Semicolon = 51,
    /// Comma, `,`.
    Comma = 54,
    /// Period, `.`.
    Period = 55,
    /// Forward slash, `/`.
    Slash = 56,
    /// The left control key.
    LeftControl = 224,
    /// The left shift key.
    LeftShift = 225,
    /// The left alt key.
    LeftAlt = 226,
    /// The left super key, which is the Windows or Command key.
    LeftSuper = 227,
    /// The right control key.
    RightControl = 228,
    /// The right shift key.
    RightShift = 229,
    /// The right alt key.
    RightAlt = 230,
    /// The right super key.
    RightSuper = 231,
}

impl HostKey {
    /// The host's number for this key, which is what a host library reports.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }

    /// The Lazen key code for this key, or [`KEY_UNKNOWN`] for one this adapter
    /// does not name.
    ///
    /// A host that reports a key outside this table still gets a usable event: it
    /// arrives as [`KEY_UNKNOWN`], which a program can test and ignore, instead
    /// of being dropped. A key a program cannot recognise but can *count* is more
    /// useful than a key that vanished.
    pub const fn lazen_code(self) -> u32 {
        match self {
            Self::Unknown => KEY_UNKNOWN,
            Self::A => KEY_LETTER_FIRST,
            Self::B => KEY_LETTER_FIRST + 1,
            Self::C => KEY_LETTER_FIRST + 2,
            Self::D => KEY_LETTER_FIRST + 3,
            Self::E => KEY_LETTER_FIRST + 4,
            Self::F => KEY_LETTER_FIRST + 5,
            Self::G => KEY_LETTER_FIRST + 6,
            Self::H => KEY_LETTER_FIRST + 7,
            Self::I => KEY_LETTER_FIRST + 8,
            Self::J => KEY_LETTER_FIRST + 9,
            Self::K => KEY_LETTER_FIRST + 10,
            Self::L => KEY_LETTER_FIRST + 11,
            Self::M => KEY_LETTER_FIRST + 12,
            Self::N => KEY_LETTER_FIRST + 13,
            Self::O => KEY_LETTER_FIRST + 14,
            Self::P => KEY_LETTER_FIRST + 15,
            Self::Q => KEY_LETTER_FIRST + 16,
            Self::R => KEY_LETTER_FIRST + 17,
            Self::S => KEY_LETTER_FIRST + 18,
            Self::T => KEY_LETTER_FIRST + 19,
            Self::U => KEY_LETTER_FIRST + 20,
            Self::V => KEY_LETTER_FIRST + 21,
            Self::W => KEY_LETTER_FIRST + 22,
            Self::X => KEY_LETTER_FIRST + 23,
            Self::Y => KEY_LETTER_FIRST + 24,
            Self::Z => KEY_LETTER_FIRST + 25,
            Self::Digit0 => KEY_DIGIT_FIRST,
            Self::Digit1 => KEY_DIGIT_FIRST + 1,
            Self::Digit2 => KEY_DIGIT_FIRST + 2,
            Self::Digit3 => KEY_DIGIT_FIRST + 3,
            Self::Digit4 => KEY_DIGIT_FIRST + 4,
            Self::Digit5 => KEY_DIGIT_FIRST + 5,
            Self::Digit6 => KEY_DIGIT_FIRST + 6,
            Self::Digit7 => KEY_DIGIT_FIRST + 7,
            Self::Digit8 => KEY_DIGIT_FIRST + 8,
            Self::Digit9 => KEY_DIGIT_FIRST + 9,
            Self::Comma => KEY_COMMA,
            Self::Period => KEY_PERIOD,
            Self::Slash => KEY_SLASH,
            Self::Semicolon => KEY_SEMICOLON,
            Self::Minus => KEY_MINUS,
            Self::Equals => KEY_EQUALS,
            Self::Backslash => KEY_BACKSLASH,
            Self::Return => KEY_ENTER,
            Self::Escape => KEY_ESCAPE,
            Self::Backspace => KEY_BACKSPACE,
            Self::Tab => KEY_TAB,
            Self::Space => KEY_SPACE,
            Self::LeftControl => KEY_LEFT_CONTROL,
            Self::RightControl => KEY_RIGHT_CONTROL,
            Self::LeftShift => KEY_LEFT_SHIFT,
            Self::RightShift => KEY_RIGHT_SHIFT,
            Self::LeftAlt => KEY_LEFT_ALT,
            Self::RightAlt => KEY_RIGHT_ALT,
            Self::LeftSuper => KEY_LEFT_SUPER,
            Self::RightSuper => KEY_RIGHT_SUPER,
        }
    }

    /// The character this key types unshifted, if it is a printable one.
    ///
    /// A host composes text; this is the adapter's *offer* of the obvious
    /// character, not a decision. A host with a keyboard layout, a shift state,
    /// or an input method uses [`HostAction::Text`] with what it actually
    /// composed.
    pub const fn unshifted_character(self) -> Option<char> {
        Some(match self {
            Self::A => 'a',
            Self::B => 'b',
            Self::C => 'c',
            Self::D => 'd',
            Self::E => 'e',
            Self::F => 'f',
            Self::G => 'g',
            Self::H => 'h',
            Self::I => 'i',
            Self::J => 'j',
            Self::K => 'k',
            Self::L => 'l',
            Self::M => 'm',
            Self::N => 'n',
            Self::O => 'o',
            Self::P => 'p',
            Self::Q => 'q',
            Self::R => 'r',
            Self::S => 's',
            Self::T => 't',
            Self::U => 'u',
            Self::V => 'v',
            Self::W => 'w',
            Self::X => 'x',
            Self::Y => 'y',
            Self::Z => 'z',
            Self::Digit0 => '0',
            Self::Digit1 => '1',
            Self::Digit2 => '2',
            Self::Digit3 => '3',
            Self::Digit4 => '4',
            Self::Digit5 => '5',
            Self::Digit6 => '6',
            Self::Digit7 => '7',
            Self::Digit8 => '8',
            Self::Digit9 => '9',
            Self::Comma => ',',
            Self::Period => '.',
            Self::Slash => '/',
            Self::Semicolon => ';',
            Self::Minus => '-',
            Self::Equals => '=',
            Self::Backslash => '\\',
            Self::Space => ' ',
            _ => return None,
        })
    }
}

/// The Lazen key code for a key this adapter does not name.
pub const KEY_UNKNOWN: u32 = 0;
/// Left control.
pub const KEY_LEFT_CONTROL: u32 = 1;
/// Right control.
pub const KEY_RIGHT_CONTROL: u32 = 2;
/// Left shift.
pub const KEY_LEFT_SHIFT: u32 = 3;
/// Right shift.
pub const KEY_RIGHT_SHIFT: u32 = 4;
/// Left alt.
pub const KEY_LEFT_ALT: u32 = 5;
/// Right alt.
pub const KEY_RIGHT_ALT: u32 = 6;
/// The left super key, which is Windows or Command.
pub const KEY_LEFT_SUPER: u32 = 7;
/// The right super key.
pub const KEY_RIGHT_SUPER: u32 = 8;
/// Backspace.
pub const KEY_BACKSPACE: u32 = 9;
/// Tab.
pub const KEY_TAB: u32 = 10;
/// Return, enter, or the keypad's enter.
pub const KEY_ENTER: u32 = 11;
/// Escape.
pub const KEY_ESCAPE: u32 = 12;
/// Space.
pub const KEY_SPACE: u32 = 13;
/// The minus key, `-`.
pub const KEY_MINUS: u32 = 14;
/// The equals key, `=`.
pub const KEY_EQUALS: u32 = 15;
/// Backslash, `\`.
pub const KEY_BACKSLASH: u32 = 16;

/// The first Lazen letter key code.
///
/// The letters occupy **one contiguous range in ASCII order**, so a program can
/// tell a letter from a digit with two comparisons and read which letter with a
/// subtraction. That is the whole reason for the numbering: classification
/// without a table.
///
/// Every `KEY_*_FIRST` and `KEY_*_LAST` pair in this file is **inclusive** of
/// both ends, so a range test is `first <= code && code <= last` and there is no
/// off-by-one to get wrong at either edge. `KEY_MAX` is the one exclusive
/// bound, and it exists only to say "past the end".
pub const KEY_LETTER_FIRST: u32 = 17;
/// The last letter key code, `z`.
pub const KEY_LETTER_LAST: u32 = KEY_LETTER_FIRST + 25;

/// The first Lazen digit key code, `0`.
pub const KEY_DIGIT_FIRST: u32 = KEY_LETTER_LAST + 1;
/// The last digit key code, `9`.
pub const KEY_DIGIT_LAST: u32 = KEY_DIGIT_FIRST + 9;

/// Comma, `,`.
pub const KEY_COMMA: u32 = KEY_DIGIT_LAST + 1;
/// Period, `.`.
pub const KEY_PERIOD: u32 = KEY_COMMA + 1;
/// Forward slash, `/`.
pub const KEY_SLASH: u32 = KEY_PERIOD + 1;
/// Semicolon, `;`.
pub const KEY_SEMICOLON: u32 = KEY_SLASH + 1;
/// One past the last key code this build assigns.
pub const KEY_MAX: u32 = KEY_SEMICOLON + 1;

/// Whether `code` names a letter key.
pub const fn is_letter(code: u32) -> bool {
    code >= KEY_LETTER_FIRST && code <= KEY_LETTER_LAST
}

/// Whether `code` names a digit key.
pub const fn is_digit(code: u32) -> bool {
    code >= KEY_DIGIT_FIRST && code <= KEY_DIGIT_LAST
}

/// The lower-case letter a key code names, if it names one.
///
/// This is the property the contiguous range buys: the letter is the code's
/// distance from the start of the range, with no table at all.
pub const fn letter_of(code: u32) -> Option<u8> {
    if is_letter(code) {
        Some(b'a' + (code - KEY_LETTER_FIRST) as u8)
    } else {
        None
    }
}

/// The digit a key code names, if it names one.
pub const fn digit_of(code: u32) -> Option<u8> {
    if is_digit(code) {
        Some(b'0' + (code - KEY_DIGIT_FIRST) as u8)
    } else {
        None
    }
}

/// Something a host did, in the host's own terms.
///
/// A script is written in terms of these and translated on the way in, so a
/// test says what a *user* did rather than what a guest-visible number is. That
/// matters for the property the design asks for: the same script under a
/// graphical host produces the same records, and a test that hard-coded guest
/// numbers would not be able to say so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostAction {
    /// A key went down.
    KeyDown(HostKey),
    /// A key came up.
    KeyUp(HostKey),
    /// A character was typed.
    Text(char),
    /// A printable key went down *and* typed its unshifted character.
    ///
    /// A convenience, and the reason the two are separate actions: a host with
    /// shift state, a layout, or an input method uses `KeyDown` and `Text`
    /// separately, and must be able to.
    Printable(HostKey),
    /// The pointer moved to an absolute position.
    MouseMove(i32, i32),
    /// A mouse button went down at an absolute position.
    MouseDown(u32, i32, i32),
    /// A mouse button came up at an absolute position.
    MouseUp(u32, i32, i32),
    /// The program was asked to quit.
    Quit,
}

impl HostAction {
    /// The guest-visible events this action produces, in order.
    ///
    /// An action can produce more than one event, and the order is part of the
    /// contract: a `Printable` key arrives as its key *then* its text, so a
    /// program that reads keys and a program that reads text see the same press.
    pub fn events(self) -> Vec<Event> {
        match self {
            Self::KeyDown(key) => alloc::vec![Event::new(EventKind::KeyDown, key.lazen_code())],
            Self::KeyUp(key) => alloc::vec![Event::new(EventKind::KeyUp, key.lazen_code())],
            Self::Text(character) => alloc::vec![Event::new(EventKind::Text, character as u32,)],
            Self::Printable(key) => {
                let mut events = alloc::vec![Event::new(EventKind::KeyDown, key.lazen_code())];
                if let Some(character) = key.unshifted_character() {
                    events.push(Event::new(EventKind::Text, character as u32));
                }
                events
            }
            Self::MouseMove(x, y) => alloc::vec![Event::pointer(EventKind::MouseMove, 0, x, y)],
            Self::MouseDown(button, x, y) => {
                alloc::vec![Event::pointer(EventKind::MouseDown, button, x, y)]
            }
            Self::MouseUp(button, x, y) => {
                alloc::vec![Event::pointer(EventKind::MouseUp, button, x, y)]
            }
            Self::Quit => alloc::vec![Event::new(EventKind::Quit, 0)],
        }
    }
}

/// A fixed sequence of host actions.
///
/// A script is the *test* host. Replaying one into a device produces exactly the
/// same stream every time, which is what makes a graphical program testable
/// headlessly — and it is the reason the driver has no timing and no blocking
/// read to depend on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HostScript {
    actions: Vec<HostAction>,
}

impl HostScript {
    /// An empty script.
    pub const fn new() -> Self {
        Self {
            actions: Vec::new(),
        }
    }

    /// A script that is the given actions, in order.
    pub fn from_actions(actions: Vec<HostAction>) -> Self {
        Self { actions }
    }

    /// Appends an action.
    pub fn push(&mut self, action: HostAction) {
        self.actions.push(action);
    }

    /// The actions, in order.
    pub fn actions(&self) -> &[HostAction] {
        &self.actions
    }

    /// Replays the script into `device`.
    ///
    /// Stops at the first refusal rather than skipping, for the same reason the
    /// device's own `inject_all` does: skipping the action that did not fit would
    /// deliver a *reordered* stream, and a program whose keys arrived out of order
    /// would have no way to tell. Returns how many actions went in.
    pub fn replay(&self, device: &mut InputDevice) -> Result<u64, InputError> {
        let mut done: u64 = 0;
        for action in &self.actions {
            for event in action.events() {
                device.inject(event)?;
            }
            done += 1;
        }
        Ok(done)
    }
}
