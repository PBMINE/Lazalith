# The graphics test, and what it found

Step 97 asks for a test that demonstrates a Lazen graphical application which:

```text
boots under LazOS  →  creates a window  →  draws graphics
                   →  receives input    →  responds to input
```

through the Lazen SDK, LazOS, the virtual devices, and SDL3.
`crates/lazalith-runtime/tests/graphics.rs` does the first four, and this document
records the one thing it could not do honestly, with the evidence — because that
thing is more useful than a passing test would have been.

## The defect: the device's framebuffer address is not the program's

The first version of this test asserted the *pixels*: after a program drew a white
block on a black window, the test read the framebuffer back out of the machine and
checked the colour at a coordinate. Every frame came back as zeroes.

The investigation, in the order it happened:

1. **The device reported a real address.** `DisplayDevice::presented()` returns
   `0x40dd08`-ish for a 16×8 window, and the window example reported `0x40c5f8` for
   its 48×32 one. Not zero, not unmapped, and a plausible guest address.
2. **The address was not the same from run to run.** Six runs of the same program
   produced six different pairs of addresses. Nothing deterministic was involved.
3. **The program and the device disagreed about where the canvas was.** A program
   was made to print its own `framebuffer.as_mut_slice().as_ptr()` and to read its
   first pixel back:

   ```text
   program says its framebuffer is at 0x4250888  device recorded 0x40dd08
   program reads its own first pixel: 255
   ```

   So the *drawing worked* — the program wrote white into the pixel and read white
   back through its own pointer — and the address the device was given was a
   different address from the one the program's local array occupies.

4. **The recorded address read back as zeroes.** The stack region starts at
   `USER_STACK_START = 0x0040_0000` and the address space translates identity, so
   reading it physically is reading the right page. That page holds zeroes.

**What is established:** the display device is handed an address that is not where
the program's framebuffer lives, and the address varies between runs of an
identical program. The drawing itself is correct, and the device's *record*
(geometry, present count) is correct.

**What is not established:** which of the two is wrong. The candidates are the
compiler handing the SDK a view onto a frame slot rather than onto the array, or
the SDK passing a different `as_mut_slice()` result to `display_open` than the one
the program itself uses. Both are consistent with the evidence, and separating them
needs a lower-level test than this step has budget for.

## What the API does about it

`Finished::presented` reports `pixels: Option<Vec<u8>>`, and the `None` case is
deliberate:

```rust
/// The pixels read back from `address` — or `None` when the machine could not read
/// that address at all.
```

A host-side read that fails is reported as "there was a frame and I could not read
it" rather than as a page of zeroes. Those are different facts, and in this build
they are genuinely different: a frame of zeroes is exactly what a program which
drew nothing would produce, so returning zeroes for a read that did not happen would
make the failure invisible to the very test written to catch it.

## What the tests do assert

Seven tests, and each one is a claim the *program* can be held to:

| test | claim |
| --- | --- |
| `the_repository_s_window_example_runs_under_the_kernel` | the program a person would run boots, opens a 48×32 window, presents at least two frames, and exits zero |
| `a_window_opens_at_the_geometry_the_program_asked_for` | three programs, three geometries; the device reports each program's own numbers |
| `a_program_draws_the_pixels_it_says_it_drew` | the program clears, fills, then reads its own canvas and finds white |
| `a_program_that_ignored_the_keyboard_never_moves_its_block` | the control: no keys, no movement |
| `a_keystroke_moves_the_block_and_the_program_reports_where_it_ended` | three `d` keys, three passes, and the program prints `12` |
| `a_keystroke_the_program_does_not_handle_changes_nothing` | three `x` keys, and the program prints `0` |
| `a_program_that_never_opens_a_window_presents_nothing` | the negative case, so the positive ones are not vacuous |

The two input tests are the step's real claim, and they are designed so that neither
can pass for the wrong reason:

- **"responds to input"** is checked by the program's *own output* — a number it
  computed from the keys it read, with the expected value checkable by hand. Not
  "the frame changed", which the example's block would satisfy by moving on its own.
- **"and only to input it handles"** is the matching negative: the same program, the
  same three events, naming a key it ignores, prints the position it did not move
  to. Without that second test, a device that fed the program any event at all would
  satisfy the first.
- **"responds to input"** also has a control for the *absence* of input, because a
  program that always moved would otherwise pass.

## The SDL3 boundary, stated precisely

SDL3 is not linked by this test, and the reason is not that it was inconvenient.
SDL3 needs a display server; every machine this project's tests run on is headless.
A test that needed one would fail everywhere the suite actually runs.

So this file pins the *contract* between the two halves:

- a presented frame is **four bytes per pixel**, in the guest's **own** memory, at
  an address the device was given — not a copy, not a host allocation;
- the geometry in the record is the geometry the program asked for;
- the present count is the number of times the frame was shown.

A frontend bug and a program bug show up in different places, and the tests assert the
side this project owns. `lazalith-sdl3`'s own tests cover the other side. What is
*not* covered is a window actually appearing on a display, and this document would
rather say that than imply otherwise.

## The one-line summary

The drawing path, the display syscall path, the input path, and the response to
input are all demonstrated end to end from real Lazen source. The address the
display device records does not match the address the program's framebuffer lives
at, and that is recorded here as an open defect with a reproduction rather than
worked around in a test.
