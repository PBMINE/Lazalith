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
