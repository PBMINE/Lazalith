# The Lazalith Frontend

`lazalith-gui` is the window a person debugs a Lazalith program in. It is the
`lazalith-gui` of the roadmap, and it is not the same thing as
[`lazalith-ui`](lazen-ui.md), which is the widget library a *guest* program links
against. One is for the person, the other is for the program.

## The rule this frontend exists to enforce

**It never touches CPU internals.**

That is not a promise in a document; it is the shape of the dependency graph.
`lazalith-gui` depends on `lazalith-debug`, and `DebugController` does not hand
out a `&mut LazalithMachine`, a `&RegisterFile`, or any path to one. There is
nothing for the frontend to reach *through*, so the rule cannot be violated by a
later change that forgets to be careful.

Every value it shows comes from an owned snapshot. That matters for more than
tidiness: a frontend that read the CPU's register storage directly could show a
program counter from one moment and a stack pointer from another, and the
difference between those two would look like a compiler bug.

## How it is put together

```text
DebugController
      ↓  (the debug API, and nothing else)
    view.rs      — decides *what* to show: panels of lines
      ↓
  window.rs      — decides *where*: SDL3 draws them
    font.rs      — draws the text
```

The split is what makes the frontend testable. Every fact a panel displays is
decided in `view.rs`, which needs no display, so the test suite runs real programs
on real machines and checks the panels against the machine's own state. Without
that split the only way to test the frontend is to open a window, which is
exactly the kind of test that does not get run.

`window.rs` knows about rectangles and colours. It cannot resolve an address,
disassemble anything, or read a register — those decisions are already made by
the time it is called.

## The ten panels

The roadmap lists what the frontend should display, and each of those is a
[`Panel`]:

| Panel | What it shows | Where the numbers come from |
| --- | --- | --- |
| `screen` | the machine's display, as the guest drew it | `display().presented()`, pixels read with `read_memory` |
| `registers` | sixteen general registers and `sp` | one `RegisterSnapshot` |
| `pc` | the program counter, its privilege, and its line | `source_location()` |
| `flags` | negative, zero, carry, overflow, interrupts | `RegisterSnapshot::status` |
| `disassembly` | instructions around the counter, each with its source line | `disassemble`, `source_location_at` |
| `memory` | a hex dump with the bytes as text | `read_memory` |
| `stack` | the words at the stack pointer, with return addresses resolved | `stack`, `source_location_at` |
| `console` | what the program printed | `terminal_output` |
| `processes` | every process and its state | `sessions()` |
| `diagnostics` | structured diagnostics, never parsed strings | `Diagnostics` |

### The screen is the guest's own pixels

The display device hands back an *address*, not a bitmap — a device that copied
the framebuffer would be a second copy of it, which is the thing the display
design refuses to be. So the frontend resolves that address against the process's
memory with `read_memory`, exactly as it reads any other address.

The conversion to the window's byte order happens in `view.rs`, once per frame.
The guest writes a pixel as alpha, red, green, blue; SDL's `XRGB8888` wants red,
green, blue. Uploading the guest's bytes unchanged would swap two channels of
every pixel, and it would look *plausible*, because a mostly-grey test image
survives a channel swap almost perfectly. The test suite checks the channel order
against a program that puts a known colour at a known pixel.

A window that has been *opened* is not a frame. Until the guest presents, the
panel says so, because reporting a frame that was never drawn would be reporting
something the user never saw. And the last presented frame is shown even after the
program exits, because the picture someone was looking at is more use to them than
the word "exited".

### The stack is not a call chain

The calling convention reserves the return address below the frame and records no
frame pointer, so there is nothing to walk. The panel says so on its own face
rather than leaving a column of numbers under the heading "stack" for a reader to
mistake for frames. Step 76's source information makes those words *readable* —
each resolves to the line it passed through — which is not the same as being
frames.

## Text

A debugger's output is mostly words, so the frontend carries a 5×7 bitmap font
for printable ASCII rather than assuming the host has a font API. A frontend that
assumed one would work on a machine with the library and show nothing on a machine
without it.

The font is a table of five columns per character, the low seven bits of each
byte being the seven rows. Drawing is `text_pixel(text, x, y)`, a function rather
than a draw call, so text can be composed into a texture, a panel, or a test's
expected frame. A character with no glyph draws as a filled box: a word with a
hole in it is a message nobody can trust, and a box says which character is
missing.

## Diagnostics are structured

Every diagnostic is a value with a `kind`, a `code`, a `message`, and — where they
are known — a source place, a guest program counter, the instruction there, and
what the machine was doing. No panel ever reads a rendered error string back to
decide what to highlight, because that is a frontend that will highlight the wrong
thing the day a message changes.

`DiagnosticKind` distinguishes a **guest fault** from an **emulator bug**, and the
panels label them differently. That is not pedantry: the first is the program's
mistake and the second is ours, so a frontend that showed them identically would
send someone to look in the wrong place.

## Where the `unsafe` is

In `lazalith-sdl3`, and only there. Every other crate in the workspace has
`unsafe_code = "forbid"`, and the SDL3 boundary is a separate crate precisely so
that stays true.

The boundary crate is the entire `unsafe` surface of the project: a set of
`extern "C"` declarations and the safe functions that wrap them. It has no
Lazalith logic, knows nothing about machines, and cannot execute an instruction.
The `unsafe` there is auditable by reading one file.

### The boundary checks itself against the real headers

A C struct layout mirrored in Rust is a claim, and a claim nobody checks is a
comment. So the build script compiles a probe against SDL3's real headers,
measures `sizeof(SDL_Event)`, `sizeof(SDL_KeyboardEvent)`, and the offsets of the
fields the shim reads, and writes those numbers into a generated file. The library
then asserts its own layout against them as `const` assertions.

This is not ceremony. Writing the probe found that SDL3's `SDL_Keycode` is four
bytes, not the eight the first version of the shim assumed — a mismatch that would
have read the wrong field of every key event and reported plausible nonsense
keycodes.

The event buffer is a union whose largest member is padding, so `SDL_PollEvent`
cannot write past it. The buffer's size is asserted to be at least
`sizeof(SDL_Event)`, and the C probe fails the build if it is not.

### What the safe API guarantees

- Every pointer SDL returns is owned by exactly one Rust value, and that value
  destroys it in `Drop`. There is no path that leaks a window and no double free,
  because there is no way to construct one of these values except by the call that
  returns it.
- `SDL_Init` and `SDL_Quit` are paired by the `Video` value.
- A texture upload is refused unless the slice is exactly `width * height * 4`
  bytes, because a short slice would have SDL read past its end.
- Every fallible call returns a `Result` carrying SDL's own words, so a frontend
  that cannot draw says why instead of showing a black window.

## What the frontend does not do yet

- **It has no controls.** Step 77 is the display; Step 78 adds Run, Pause, Step,
  Reset, Continue and Breakpoint.
- **It does not render guest diagnostics structurally from the machine.** The
  diagnostic type is structured from the start, and the frontend records what it
  finds itself; Step 79 connects the guest's own diagnostics and Step 80
  distinguishes guest faults from emulator bugs in the machine's own reporting.
- **It is not tested with a real window.** The view model is tested against real
  machines, and the window layer is ordinary drawing code over it. Opening a
  window in CI needs a display, and a test that needs a display is a test that
  does not run.
