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

Key codes are Lazen's own stable numbering, assigned in
`docs/lazen-input.md`'s companion table in the SDK and frozen once assigned:

```text
0   unknown        8   backspace      16  'q' .. 'p'
1   left control   9   tab            17  'a' .. 'l'
2   right control  10  enter          18  'z' .. 'm'
3   left shift     11  escape         19  '0' .. '9'
4   right shift    12  space          20  comma
5   left alt       13  minus          21  period
6   right alt      14  equals         22  slash
7   left super     15  backslash      23  semicolon
```

Letters and digits occupy contiguous ranges in ASCII order so a program can
classify a code with a range test. Printable keys also arrive as `Text` events,
which is what a program should use for character input; `KeyDown`/`KeyUp` are
for control keys and for games.

## Operations

```lazen
pub fn poll(events: &mut [Event], capacity: u32) -> u32;
pub fn text_length() -> u32;
```

`poll` writes at most `capacity` events into the caller's array and returns the
number written. If the queue holds more events than the capacity, the remainder
stays queued for the next call; events are never silently dropped. A return of
zero means "nothing pending", not "an error".

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
