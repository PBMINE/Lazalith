# Lazen Graphics API

This document is Step 58 of the roadmap. It designs a native graphics API. It
does not copy SDL, and SDL is a host implementation detail.

## Model

```text
Lazen application
    ↓  std::graphics (pure Lazen)
LazOS display driver
    ↓  display ABI
Virtual Display Device
    ↓
guest-owned framebuffer in machine RAM
```

The guest owns the authoritative framebuffer. The device does not keep a private
copy of pixels that must be uploaded; the guest writes into a linear pixel
buffer that the driver presents. A present is a synchronization point, not a
transfer of the whole buffer, because the device already shares the memory.

## Types

```lazen
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

pub struct Canvas {
    pub pixels: &mut [u8],   // ARGB8888, row-major, no padding
    pub width: u32,
    pub height: u32,
}

pub struct Window {
    // opaque handle
}
```

`Canvas` is a *view* into the framebuffer plus its dimensions. A `Window` is an
opaque handle: the application never learns a framebuffer address, and cannot
construct one.

## Pixel format

One format, fixed, versioned with the ABI:

```text
ARGB8888, 4 bytes per pixel, row-major, rows top to bottom
byte offset 0 = A, 1 = R, 2 = G, 3 = B
```

Note that the byte order above is the reverse of `0xAARRGGBB` read
little-endian. That is deliberate and it is the ABI's decision: a *pixel* is four
bytes in this order, while a *colour* is a number a program compares and masks, and
the number is `0xAARRGGBB`. `std::graphics::write_pixel` and
`std::graphics::read_pixel` are the only two places that cross between the two, so
a program never has to hold both in its head at once.

A canvas of `width` by `height` pixels is exactly `width * height * 4` bytes.
The canvas slice handed to the application is exactly that length, so indexing
is a bounds-checked arithmetic operation with no stride and no padding to get
wrong. The compiler checks the multiplication for overflow when computing a
pixel index.

## Operations

```lazen
pub fn open(width: u32, height: u32) -> optional<Window>;
pub fn close(window: Window);
pub fn canvas(window: Window) -> optional<Canvas>;
pub fn present(window: Window) -> bool;
pub fn clear(canvas: &mut Canvas, color: Color);
pub fn put_pixel(canvas: &mut Canvas, x: u32, y: u32, color: Color);
pub fn fill_rect(canvas: &mut Canvas, x: u32, y: u32, w: u32, h: u32, color: Color);
pub fn draw_text(canvas: &mut Canvas, x: u32, y: u32, text: &str, color: Color);
```

Every coordinate-taking operation is bounds-checked. Drawing outside the canvas
is not an error: it is clipped, and clipping is total, so a partially visible
rectangle draws its visible part. This matches what a real window system does
and avoids the failure mode where a program crashes because an animation moved
one pixel off screen.

`draw_text` uses a built-in 8x8 font stored in the read-only data section. The
font is a resource of the SDK, not a host asset, so text rendering is identical
headless and graphical.

### How this became `std::graphics` in Lazen v1

Step 70 implements the operations above as `std::graphics`, and the language it
had to fit into is smaller than the signatures above assume. Two adaptations,
both forced by the language rather than chosen:

**No `Window` or `Canvas` type.** Lazen v1 has no `struct` and no `impl`, so a
canvas is a `&mut [u8]` the caller owns and the geometry is passed beside it. That
is the *same* memory the design describes — `Canvas` was a view plus two numbers,
and the view is now the argument — but it means the address is the caller's to
name. A Step 73 opaque handle can restore the hiding without changing the
drawing operations.

**Packed geometry.** The ABI has six argument words and a view costs two, so
`draw_text` as written above would need eight. `pack_point`, `pack_rect`,
`pack_surface` and `pack_ink` exist for that reason and are documented at each
field. They are not a stylistic choice: a call that cannot be made is a call a
program cannot use.

## Event and redraw model

A graphical program is an ordinary loop:

```lazen
let mut window = ui::open(320, 200);
loop {
    let mut canvas = match ui::canvas(window) { ... };
    ui::clear(&mut canvas, ui::BLACK);
    ui::fill_rect(&mut canvas, 8, 8, 64, 64, ui::BLUE);
    ui::draw_text(&mut canvas, 8, 80, "Lazen", ui::WHITE);
    ui::present(window);
    ...
}
```

There is no retained scene graph, no immediate-mode widget system, and no
damage tracking in the graphics layer. Those belong to the GUI library (Step
73), which is written on top of these primitives.

## What this API deliberately does not have

- No windows overlapping or stacking: one display, one window, one canvas.
- No surfaces, textures, render targets, or shaders.
- No resizing: the canvas size is fixed when the window is opened, because the
  framebuffer is a fixed region of machine RAM in v1.
- No double buffering, and therefore no frame pacing. `present` is a
  synchronization point for a headless or recorded run.
- No colour space, gamma, or blending. One format, opaque pixels.
- No event loop inside the graphics module. Input is a separate API (Step 59),
  and a program decides how to combine the two.

## Why not copy SDL

An SDL-shaped API in Lazen would push host concepts into the guest: surfaces,
renderers, and hints are artifacts of one host implementation. The Lazalith
model is a guest-owned linear framebuffer with a present operation, so the
native API is a canvas with pixel and rectangle operations. A host frontend can
present that framebuffer in a window, in a browser, or not at all, and the
application cannot tell the difference.

## Verifying the contract

Step 68 implements the device, Step 70 the driver, Step 72 an application. The
verification is that a program using only these functions runs to completion
with no host library linked, that the framebuffer contents the program wrote are
exactly the contents the device presents, and that a drawing operation clipped
at the canvas edge writes only in-bounds bytes.

## What a frame costs today

Step 72 built the first application and measured it, and the numbers are worth
writing down because they decide how big a window can be. They are **instruction
counts**, not timings: `lazen run` refuses a program that retires more than
5,000,000 instructions, so the budget is a count and a wall-clock measurement
would not say anything.

| What | Cost | Why |
| --- | --- | --- |
| `[0u8; N]` — initialising an `N`-byte array | ~92 per byte | a repeated array is a counted loop, and the loop body forms each element's address through the frame |
| `clear` — one pixel | ~600 | four byte stores, each one a bounds-checked index |
| `put_pixel` — one pixel | ~2300 | the same four stores, plus unpacking the point and decoding the colour from the packed `u32` |
| `display_open`, `present`, `input_poll` | tens | one syscall each |

Those are costs of the *code generator*, not of the API. The backend has no
register allocator and keeps every value in the frame, so each source-level
operation is a load, an operation, and a store through memory rather than a
register. `put_pixel` is nearly four times `clear` because it re-derives the four
channel bytes from the colour for every pixel, where `clear` reads them once.

The arithmetic that follows from the table: a 48-by-32 window is 1536 pixels,
which is about 0.9 million instructions to clear and about 0.6 million to
initialise, so `examples/window/main.lz` costs 3.9 million for two frames. A
320-by-200 window is 64000 pixels, which is about 38 million to clear and 23
million to initialise — roughly **sixty times** the whole runaway budget for one
frame, before anything is drawn.

### Why the budget was not raised

A larger budget would turn a refusal into a slow program rather than a fast one:
5,000,000 instructions already takes about ten seconds to interpret, so sixty
times that is about ten minutes for a single frame. Raising the limit hides the
cost instead of removing it, and the cost is real — a guest writing 256,000 bytes
through a bounds-checked index is doing 92 instructions per byte, and no budget
makes that a good design.

The two things that would actually remove it are both real work rather than
configuration:

- **A register allocator in `lazalith-codegen`.** Every operation would become a
  register-to-register instruction instead of a frame round trip, which is where
  most of the 92 and most of the 600 come from.
- **A bulk memory operation.** Clearing a framebuffer in words rather than bytes
  is an eightfold reduction on its own, and the ABI has no operation for it: the
  language has no word-wide store to widen a byte store into, and
  `RESERVED_DESIGN_SYSCALLS` is empty, so there is nowhere to put one without
  spending a name the design reserved.

Until one of those exists, the window size in the example is set by measurement
rather than by taste, and the number is written in the program so the next person
to change it finds out why.
