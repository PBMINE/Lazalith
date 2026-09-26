# Lazen GUI Library

The first-party widget set, `gui`. It is a Lazen module written in Lazen, it is
built on the Lazen SDK, and it does not talk to SDL3 or to any host library.

## Where it lives

The library is Lazen source in the `lazalith-gui` crate, and the runtime
composes it after the standard library. That is the same arrangement the
standard library has, and for the same reason: a widget set that is *generated*
code can disagree with the compiler about what the language means, and a bug in
`menu_at` is a bug anyone can point at in a file anyone can read.

```text
rt::      the syscall wrappers and memory primitives   (lazalith-runtime)
std::     core, io, text, math, collections, fs, ...  (lazalith-stdlib)
gui::     the widget set                               (lazalith-gui)
```

Nothing in `gui` names a window handle, a device, an event queue, or SDL3. The
layer above the SDK is a library and the layer below it is a host, and this is
the step where that sentence becomes checkable rather than aspirational.

## Widgets are geometry, not objects

Lazen v1 has no structs, no methods and no enums. A widget therefore cannot be a
value with fields, and is instead its own geometry passed to a function:

```lazen
gui::draw_button(canvas.as_mut_slice(), size, rect, "ok");
```

There is no `Button` value to build, store in a list, or hand to a layout. What
there is instead is a rectangle, a word or two of state, and a function that
draws it. This is a real limitation and it is worth stating what it costs: a
widget cannot carry behaviour, so a program that wants a button to act on release
rather than on press writes that itself. In exchange a widget is three words of
arguments rather than an allocation, and there is no lifetime to get wrong.

## The six-word argument budget, which shapes every signature

The ABI passes at most **six argument words**. Two of the types a widget needs are
two words each — a `&mut [u8]` canvas is an address and a length, and a `&str` is
the same — so a drawing function has spent four of its six words on *what to draw
on* and *what to write*, and has two left.

Every signature in the library is shaped by that:

| What a widget needs | How it is passed | Words |
| --- | --- | --- |
| the canvas | `&mut [u8]` | 2 |
| the canvas's size | `std::graphics::pack_surface(w, h)` | 1 |
| a rectangle | `std::graphics::pack_rect(x, y, w, h)` | 1 |
| a position and a colour | `std::graphics::pack_ink(x, y, colour)` | 1 |
| the text | `&str` | 2 |

So `draw_label(canvas, canvas_size, ink, text)` is exactly six words, and
`draw_button(canvas, canvas_size, rect, text)` is six. A widget set where half
the calls are refused at the ABI is not a widget set, and a function the compiler
accepted but no program can call is worse than one that does not exist.

Two consequences are visible in the API rather than hidden in it:

- **A button's colours are the library's.** Theming a button needs two more words
  than there are. A program that wants its own draws the face with `draw_panel`
  and the text with `draw_label` at `button_label_at`, and both exist for exactly
  that. A *held* button is a different face colour rather than a shifted label,
  which is the same argument: the shift needs a second geometry word and a
  different colour needs none.
- **`draw_canvas` blits over the whole destination canvas.** A source view and a
  destination rectangle are two words between them, and with the canvas and the
  source there is no seventh word. A program that wants a viewport smaller than
  its window draws the border and blits into the window.

## The components

| Component | What it is | Function |
| --- | --- | --- |
| Window | the program's own framebuffer, opened and presented | `open`, `present` |
| Layout | a cursor that moves down or across | `layout_begin`, `layout_step`, `layout_x`, `layout_y` |
| Panel | a filled, clipped rectangle | `draw_panel` |
| Label | text at a position, sized from the text | `draw_label` |
| Button | a face and a centred label, held or not | `draw_button`, `draw_button_held`, `button_clicked` |
| TextInput | a single-line field: append, backspace, read | `text_input_insert`, `text_input_backspace`, `text_input_text`, `draw_text_input`, `draw_caret` |
| Canvas | a window onto another canvas, clipped at both ends | `draw_canvas`, `pack_view` |
| Menu | a bar of equal columns and a selection | `draw_menu`, `menu_at`, `menu_clicked`, `menu_select` |

## Two rules everything else follows from

- **A widget says whether it drew anything.** Every `draw_*` returns a `bool` that
  is false when the widget fell entirely outside the canvas. A program that lays
  out more than fits can therefore *see* that it did, rather than having drawn
  nothing and not known.
- **A hit test answers with a number, and the count is "not found".** This is the
  convention `std::input::find` already uses: an index below the count is a hit,
  and the count is not. A menu that answered 0 for a miss would select the first
  item every time anything was clicked, which is the bug the convention exists to
  prevent.

## Hit tests and drawings must agree

A rectangle is half-open on its far edges: a widget at x = 0 with a width of 8
covers columns 0 to 7. So two widgets laid out edge to edge do not both claim the
column between them, and a button is pressable exactly where it is drawn. A hit
test that used closed edges would make a button pressable one pixel to its left,
which is the bug a person notices first and believes least.

## What this library deliberately does not have

- **No widget objects, no event loop, and no focus order.** Whether a text field
  has a caret is the program's decision, and `draw_caret` is separate for that
  reason: a library that decided focus would have to own a focus order.
- **No menu items as strings.** A menu is a bar and a selection; a program draws
  its own labels into the rectangles `menu_item` returns. v1 has no array of
  `str`, and a menu that owned a font would need one a host does not have.
- **No drawing of its own beyond the SDK's primitives.** Every widget is built
  from `fill_rect`, `put_pixel` and `draw_text`. There is no anti-aliasing, no
  rounded corner, and no theming, because each of those is a shape this backend
  cannot afford to compute per pixel — see the cost table in
  `lazen-graphics.md`.

## Verifying the contract

The seven tests in `crates/lazalith-gui/tests/gui.rs` write Lazen programs that
use the library and read the frame the device presented. They state that a widget
set can open a window and present a frame with no `std::graphics` call of its
own; that a layout puts a widget where its cursor said; that a hit test and the
drawing agree about every edge; that a menu answers "which item" with a number
and the count is "none"; that a field's three editing rules are three rules; and
that a canvas blit clips at the source's edge and refuses a source whose geometry
does not divide its length.
