# Lazen Input API

This document is Step 59 of the roadmap. It designs a guest-visible event system.
The application must not know that a host event structure exists.

## Model

```text
host input source            (SDL3 in a graphical host, a script in tests)
    ↓  Host Input Adapter
Virtual Input Device         (guest-visible event queue)
    ↓  input ABI
LazOS input driver
    ↓  std::input
application
```

The device owns a queue of guest-visible events. The driver drains it into
application-provided memory on request. The application polls; there is no
interrupt-driven event delivery in v1, and no blocking `next_event`.

## Event record

One record layout, fixed, versioned with the ABI:

```text
size 16
0x00  u32 kind
0x04  u32 code
0x08  i32 x
0x0c  i32 y
```

| `kind` | Meaning | `code` | `x`, `y` |
| --- | --- | --- | --- |
| 1 | `KeyDown` | stable key code | unused, 0 |
| 2 | `KeyUp` | stable key code | unused, 0 |
| 3 | `MouseMove` | 0 | absolute pointer position |
| 4 | `MouseDown` | mouse button index | absolute pointer position |
| 5 | `MouseUp` | mouse button index | absolute pointer position |
| 6 | `Text` | Unicode scalar value | unused, 0 |
| 7 | `Quit` | 0 | unused, 0 |

Every field is defined for every `kind`; an unused field is zero. A reader never
has to check which fields are meaningful.

## Key codes

Key codes are Lazen's own stable numbering, frozen once assigned. They are
**not** a host's: the host adapter is the only component that translates, and
keeping the two apart is what stops a host renumbering from renumbering the
guest.

```text
0   unknown        8   right super    16  backslash
1   left control   9   backspace      17  'a' .. 'z'   (17..42)
2   right control  10  tab            43  '0' .. '9'   (43..52)
3   left shift     11  enter          53  comma
4   right shift    12  escape         54  period
5   left alt       13  space          55  slash
6   right alt      14  minus          56  semicolon
7   left super     15  equals
```

Letters and digits occupy **one contiguous range each, in ASCII order**, so a
program can classify a code with two comparisons and read which letter with a
subtraction — classification without a table:

```lazen
if code >= 17 && code <= 42 { letter = 97 + (code - 17); }
```

Both `KEY_*_FIRST` and `KEY_*_LAST` are inclusive of both ends, so a range test
is `first <= code && code <= last` with no off-by-one at either edge.

This table corrects the four sub-ranges the first draft of this document gave
(`'q'..'p'`, `'a'..'l'`, `'z'..'m'`, `'0'..'9'`). Those cannot be right as
written: `'q'..'p'` and `'z'..'m'` are not contiguous in ASCII order, and the
ranges overlap. The property the document asks for — that a program classifies a
code with a range test — needs one range per character class, and there are two
classes. The right-hand super key, named by the design and missing from its own
table, is code 8.

Printable keys also arrive as `Text` events, which is what a program should use
for character input; `KeyDown`/`KeyUp` are for control keys and for games.

## Operations

```lazen
pub fn poll(events: &mut [u8], capacity: u32) -> u32;
```

`poll` writes at most `capacity` records into the caller's array and returns the
number written. If the queue holds more events than the capacity, the remainder
stays queued for the next call; events are never silently dropped. A return of
zero means "nothing pending", not "an error".

### How this became `std::input` in Lazen v1

Step 71 implements this as `std::input`, and the language is smaller than the
signatures assume. Three adaptations, each forced by the language:

**A record is sixteen bytes, not a struct.** Lazen v1 has no `struct`, so an
event is a `&[u8]` of fixed-size records and `std::input` reads the four fields
by offset: `kind_of`, `code_of`, `x_of`, `y_of`. A reader still never has to ask
which fields are meaningful, because an unused field is zero.

**The count comes back through a record, not the return value.** A v1 call
returns one `i64` and that word is the status, so `input_poll` takes an
`IoResult` out-parameter and the SDK reads both. The SDK's `poll` returns the
count, so a program sees the shape this document describes.

**Key codes are functions, not constants.** A `const` inside a nested module is
not reachable by path in Lazen v1, so `KEY_LEFT_SHIFT` and the rest are
`std::input::key_left_shift()`. The numbering is unchanged; only the spelling
differs. This is the same reason `std::graphics` spells `pixel_bytes()`.

There is no `text_length`: nothing in v1 composes text, so there is no composed
length to report. `text_of` reads the character out of a `Text` event instead.

## Event ordering and determinism

Events are delivered in the order the device received them, and the device
preserves host order. Two runs with the same injected event script produce the
same event stream, which is what makes a graphical program testable headlessly.

A `Text` event for a key press that is also delivered as `KeyDown` is not a
duplicate: `Text` carries the character, `KeyDown` carries the key. A program
that wants characters reads `Text`; a program that wants keys reads `KeyDown`.

## What this API deliberately does not have

- No blocking wait, no timeouts, and no `await`. A program that wants to wait
  polls, which keeps the whole input path deterministic.
- No text editing, key bindings, or IME support. Those are GUI concerns.
- No raw scancodes, no key symbols, and no host modifier conventions.
- No event allocation inside the driver: the driver writes into application
  memory, so a program that stops polling simply stops receiving events.

## Verifying the contract

Step 69 implements the device with an injectable event source. Step 71 adds the
LazOS driver and the host input adapter, where the adapter is the only component
that knows about the host and where the SDL3-backed adapter arrives with the
Step 77 frontend. The verification is that a Lazen program compiled and run
headlessly, driven by a scripted event source, reacts to keyboard input with no
host library present, and that the same program under a host adapter sees the
identical event records.
