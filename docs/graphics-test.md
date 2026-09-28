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

## RESOLVED — the defect was in the runner, not in the drawing path

> **This defect is fixed.** The investigation below is kept because it is the record
> of how it was found and because its conclusion was wrong in an instructive way.
> See "What the defect actually was" at the end of this section.

## The defect: the device.s framebuffer address is not the program.s

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

**What is not established (and turned out to be the wrong question):** which of the two
is wrong. The candidates considered were the compiler handing the SDK a view onto a
frame slot rather than onto the array, or the SDK passing a different
`as_mut_slice()` result to `display_open` than the one the program itself uses. Both
are consistent with the evidence. Neither is what was happening, and separating them
did need a lower-level test — just not the one either of these hypotheses implied.

### What the defect actually was

None of the above. The address was correct at every layer, and the test was reading
the right address in the wrong memory.

A process.s memory lives in the machine only while the process is resident.
`activate_user_context` swaps the process.s regions *in* and the machine.s own user
regions *out*; `release_user_context` swaps them back. `run_loaded` read the presented
frame through the machine *after* the scheduler had released the process — at which
point the machine held a different, freshly zeroed set of user regions at the same
addresses, belonging to no process.

So the reader was correct about the address and correct about the machine, and wrong
about which of the two owned the picture. Instrumenting the release shows it exactly:

```text
BEFORE release 0x40ea08=[255, 0, 0, 0, 255, 0, 0, 0]   <- the pixels, in the machine
AFTER  release 0x40ea08=[0, 0, 0, 0, 0, 0, 0, 0]        <- zeros, in a different stack
process address space now holds the three user regions   <- including the drawn one
```

This also explains the two facts that made the evidence look so strange. The
address "varied between runs" because the stack layout is not fixed. The recorded
address "read back as zeroes" because it was no longer the frame.s address by the
time anything read it.

**The fix** is one line of ownership: the runner reads the pixels from the *process.s*
address space, which is where a dead process.s memory still lives, rather than from
the machine, which by then holds somebody else.s. `docs/hardening.md` records this as
defect 8, and `crates/lazalith-runtime/tests/hardening_graphics_address.rs` asserts
the pixels directly — the assertion step 97 said it could not make honestly, and now
can.

## What the API does about it

`Finished::presented` reports `pixels: Option<Vec<u8>>`, and the `None` case is
deliberate:

```rust
/// The pixels read back from `address` — or `None` when the machine could not read
/// that address at all.
```

A host-side read that fails is reported as "there was a frame and I could not read
it" rather than as a page of zeroes. Those are different facts, and they were
genuinely different *while the bug above was live*: a frame of zeroes is exactly what
a program which drew nothing would produce, so returning zeroes for a read that did
not happen would have made the failure invisible to the very test written to catch
it — which is what happened, and is why the bug survived a passing suite. The `None`
case is kept because the two answers are still different, and now that the read is
done in the right memory they are distinguishable.

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
input are all demonstrated end to end from real Lazen source, and the host now reads
back the exact pixels the program drew. The defect this document recorded is fixed and
resolved above: it was the runner reading a dead process.s frame through a machine
that no longer owned it.
