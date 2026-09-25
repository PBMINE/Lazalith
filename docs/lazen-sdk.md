# Lazen SDK

This document is Step 57 of the roadmap. It defines the high-level surface a
Lazen program uses so that it never needs to know MMIO, framebuffer addresses,
device registers, or SDL3.

## The rule

Every SDK function is either:

- a Lazen function in the `std` package that calls an `extern "syscall"`
  declaration, or
- a pure Lazen function with no OS interaction at all.

There is no third kind. The SDK contains no inline assembly, no pointer
arithmetic against hardware addresses, and no host-specific types. Anything the
SDK needs from the platform arrives through the OS ABI.

## Module layout

```text
std
├── core      types, optionals, panic-free helpers
├── io        console and file byte streams
├── text      string handling
├── math      integer helpers
├── collections  stack and static data structures
├── fs        filesystem convenience
├── process   process information
├── graphics  window, canvas, color, text, image   (Step 58)
└── input     events                                  (Step 59)
```

Only modules whose underlying OS capability is served are provided. The rule
from Step 67 applies from the first commit: an SDK function that cannot work is
not written down, and an OS call that returns `NotSupported` is not wrapped as
though it worked.

### Availability in the first milestone

| Module | Status | Reason |
| --- | --- | --- |
| `core` | available | pure Lazen |
| `math` | available | pure Lazen |
| `text` | available | pure Lazen over byte slices |
| `collections` | available, stack/static only | no heap yet |
| `io` | available for console and files | `Read`/`Write`/`Open`/`Close` are served |
| `fs` | available for the served operations | `Stat` and `ListDirectory` are served |
| `process` | limited to the running process | `SpawnProcess`/`WaitProcess` are not served |
| `graphics` | available after Step 70 | display driver ships in Step 70 |
| `input` | available after Step 71 | input driver ships in Step 71 |
| time | not provided | `Time` and `Sleep` return `NotSupported` |

## The ABI boundary

`std` declares the OS ABI once, in one module, and every other SDK function is
written in terms of those declarations:

```lazen
// std::sys — the only place raw status codes appear
extern "syscall" fn write(handle: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;
```

Consequences:

- A program's raw ABI surface is greppable. Every `extern "syscall"` in a
  program is either in `std::sys` or explicitly written by the programmer.
- Status codes are converted to types exactly once. `io::write` returns
  `optional<usize>`; no caller outside `std` ever sees a status integer.
- The compiler maps each declaration to `lazalith_os_abi::Syscall` and checks the
  arity against the shared ABI table, so a renamed or reordered ABI call cannot
  be silently mistyped.

## Error policy

SDK functions never panic and never abort. Each one returns either a plain
value, an `optional<T>`, or a `Result`-shaped enum, and the reason lives in the
type. `std::io::write` returning `none` means the transfer did not happen; the
caller decides whether that is fatal.

The SDK does not provide a `panic!` equivalent. A program that cannot continue
calls `std::process::exit(code)`.

## Resources

Resource files declared in `lazen.toml` become byte arrays the program can
address. The SDK exposes them as read-only slices through
`std::resources::load(name)`; there is no file system lookup at runtime for an
embedded resource, and a missing name is a `none` rather than a load-time
failure, because resource names are checked by the compiler against the
manifest.

## What the SDK must never do

- Take an MMIO or device-register address.
- Require the application to link against a host library.
- Depend on SDL3, directly or transitively.
- Provide a function whose behavior differs between the headless core and a
  graphical host. If a capability is unavailable, the function is absent or
  returns a typed failure, and the difference is documented here.

## Verifying the rule

Step 70 and Step 71 both add a driver, and both add a test that proves the guest
never bypasses the OS: the driver is the only component holding the display or
input device, and the application is driven only through `std` wrappers. The
graphical application in Step 72 is compiled and executed with no host library
present at all, which is the strongest available proof that the SDK leaks
nothing.
