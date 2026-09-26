# LazOS System Call ABI v1

## Scope and status

This document defines the v1 LazOS system-call contract materialized by the
shared `lazalith-os-abi` crate. It covers the roadmap's exit, read, write,
open, close, seek, time, sleep, and memory-allocation services plus the minimum
directory, process-launch, wait, exit-status, and virtual-terminal operations
needed to make the later init/shell real.

The shared ABI definitions are materialized once for every consumer. Step 36
materializes the data definitions, Step 37 implements validation and dispatch,
Step 38 supplies the mandatory process/thread service context, Step 41
provides the in-memory `FileSystemService`, and Step 43 adds descriptor-aware
`TerminalService` support for stdin/stdout and clear-screen operations. Graphics
calls are deferred until a display driver exists. Input uses the existing
headless virtual-terminal model rather than SDL events.

## ABI versioning

```text
LazOS ABI version = 1
```

The ABI version is independent of the Lazalith ISA version and architecture.
A program carries both the OS ABI version and selected LZ32/LZ64 architecture.
There is no mixed-width binary and no compatibility promise for a different
major ABI version.

System-call numbers are Lazalith-specific stable v1 values. They are not Linux
numbers and do not copy Linux flags, structure layouts, or error conventions.

## Entry and return convention

A User process invokes a service with one `SYSCALL` instruction. Register state
at trap entry is exact:

| Register | v1 entry meaning | v1 returning-service meaning |
| --- | --- | --- |
| `r0` | Word-sized syscall number | `u32` status, zero-extended to the word |
| `r1` | Argument 1 | `u32` payload, zero-extended to the word |
| `r2` | Argument 2 | Preserved |
| `r3` | Argument 3 | Preserved |
| `r4` | Argument 4 | Preserved |
| `r5` | Argument 5 | Preserved |
| `r6` | Argument 6 | Preserved |
| `r7` | Reserved; must be zero | Preserved |
| `r8`–`r15` | Callee-saved | Preserved |
| SP | Preserved; SYSCALL creates no guest frame | Preserved |
| NZCV, U, IE | Preserved except required trap-entry changes | Restored by RFE |

At trap entry the controller changes PC to the kernel trap target, forces
Supervisor, clears IE, preserves SP/registers/NZCV, and sets editable resume PC
to the instruction after SYSCALL. The dispatcher validates the request. A normal
service writes its status/payload pair to `r0`/`r1`; RFE then restores the saved
control state and returns to the saved resume PC. `Exit` never returns and does
not use the returning-service column.

`Exit` and terminal double trap do not return normally. Invalid control return
and failed entry are machine faults, not syscall statuses. The dispatcher returns
a typed, one-shot completion capability for a returning call; the kernel handler
must use that capability at an actual `RFE` instruction. The machine authorizes
that exact completion for the live syscall frame before executing `RFE`; a
generic `RFE` over an unadmitted syscall frame is rejected. These host APIs are
kernel-internal capabilities, not User-call entry points; the CPU keeps `RFE`
available for ordinary software/interrupt trap return.

## Tagged outcome

A returning syscall writes one mode-independent logical pair:

```text
r0 = u32 SyscallStatus, zero-extended to the architectural word
r1 = u32 detail payload, zero-extended to the architectural word
```

This is intentionally not a 64-bit value packed into `r0`: LZ32 has no
multiword-return convention and its `r0` is only 32 bits. Both LZ32 and LZ64
therefore use the same two-register result. `r2` through `r7` remain available
to the caller after return.

`SyscallStatus::Ok` is zero. A nonzero status is a structured `SyscallError`;
the payload is a small detail code defined by that error, not a string and not
a hidden calling convention. Values wider than 32 bits are written to validated
caller memory through an explicit output pointer.

A failed service must not partially mutate process, file, allocator, scheduler,
or device state unless its documented contract explicitly returns a short
transfer after committing the transferred bytes.

## Syscall numbers

| ID | Name | Returning | Purpose |
| ---: | --- | --- | --- |
| `0x0001` | `Exit` | no | Terminate the calling process with a status |
| `0x0002` | `Write` | yes | Write bytes to a file or terminal descriptor |
| `0x0003` | `Read` | yes | Read bytes from a file or input descriptor |
| `0x0004` | `Open` | yes | Open a filesystem path |
| `0x0005` | `Close` | yes | Close a process-owned handle |
| `0x0006` | `Seek` | yes | Move a file handle offset |
| `0x0007` | `Stat` | yes | Write file metadata |
| `0x0008` | `ListDirectory` | yes | Write deterministic directory records |
| `0x0009` | `Time` | yes | Return deterministic virtual cycles |
| `0x000a` | `Sleep` | yes | Block until a virtual-cycle deadline |
| `0x000b` | `AllocateMemory` | yes | Allocate aligned zeroed User memory |
| `0x000c` | `SpawnProcess` | yes | Load and create a child process |
| `0x000d` | `WaitProcess` | yes | Wait for a child and return its exit status |
| `0x000e` | `ClearScreen` | yes | Clear the virtual terminal |
| `0x000f` | `DisplayOpen` | yes | Open a window over the caller's framebuffer |
| `0x0010` | `DisplayPresent` | yes | Record the caller's frame as the visible one |
| `0x0100`–`0xffff` | Reserved | rejected | Future ABI extension range |

Unknown and reserved numbers return `SyscallError::UnknownSyscall`. LZ64
validates the complete word-sized `r0`; a value above `0xffff` is never narrowed
and aliased to a v1 number. LZ32 can observe only the already-truncated 32-bit
register defined by the ISA, so it cannot recover a pre-truncation host value.
No dispatcher entry point aliases two visible architectural numbers.

## Shared ABI values

All structures are little-endian and use fixed v1 layouts. Rust `repr(C)` is not
part of the wire contract; the shared crate publishes explicit constants and
checked conversion helpers.

### Handles and flags

```text
FileHandle    = u32, 0/1 are reserved terminal descriptors; file values start at 2
ProcessHandle = u32, zero is never valid
FileOffset    = architecture word, interpreted as signed two's complement for Seek
CycleCount    = u64 memory value; delay register value is an architecture word
ProcessId     = u32
ExitStatus    = u32
```

The shared crate owns the checked word conversions used for these values. The
existing platform `CycleCount` remains the general clock type; syscall records
and conversions are imported from `lazalith-os-abi` rather than redefined.

Open flags occupy a `u32`:

| Flag | Meaning |
| ---: | --- |
| `0x0000_0001` | read |
| `0x0000_0002` | write |
| `0x0000_0004` | create |
| `0x0000_0008` | truncate |

Unknown flags are rejected. There is no mode copied from Linux.

The frozen v1 input limits are `MAX_PATH_BYTES = 4096` for a path byte string
(no embedded NUL; the supplied length is the exact path extent and the path is
not NUL-terminated on the wire), `MAX_ARGUMENT_COUNT = 1024` argv entries, and
`MAX_ARGUMENT_BYTES = 65536` bytes per NUL-terminated argv string including its
terminator. `MAX_ARGUMENT_TOTAL_BYTES = 1048576` bounds the aggregate string
bytes, including terminators, in one `SpawnProcess` request. A zero-length path
is invalid; a zero-length data transfer does not dereference its pointer but the
pointer value must still be a valid architectural word. A NUL byte inside the
declared path range is rejected, so the maximum usable path is a full
`MAX_PATH_BYTES` bytes.

### `IoResult`

```text
size 16
0x00  u64 transferred
0x08  u32 status
0x0c  u32 reserved = 0
```

`Write`/`Read` return this structure through a validated output pointer and the
normal `r0`/`r1` status/payload pair. `transferred` may be short for files,
input, or output capacity; it never exceeds the requested length. The record's
`status` field is reserved and always `Ok` on a returned record: transfer
failures are reported through `r0`/`r1` instead, so a consumer must not read
this field as a per-transfer result.

### `FileStat`

```text
size 16
0x00  u32 kind          1 = file, 2 = directory
0x04  u32 permissions   read/write/execute bits defined by the filesystem
0x08  u64 size
```

`Stat` rejects a path to a missing object and does not follow a symbolic-link
model that v1 does not implement. `permissions` reports the node's capability
class, that is, what a fully granted handle may do with it. It is not a
per-handle grant: v1 has no accounts or node-level permissions, and read/write
access is enforced per open handle, so a consumer must not treat this field as
an authorization decision.

### `DirectoryRecord`

```text
size 256
0x000  u16 name_length
0x002  u16 kind
0x004  u8  name[252]
```

The record is not NUL-terminated. Names longer than 252 bytes cannot exist in
v1. `ListDirectory` writes records in deterministic bytewise name order until
output capacity or directory end and reports the count in `IoResult`.

### `MemoryAllocation`

```text
size 16
0x00  u64 address
0x08  u64 length
```

`AllocateMemory` writes the result to caller memory. Addresses are
`VirtualAddress` values in the calling process. v1 allocation has no individual
free syscall; all process allocations are reclaimed when the process exits.

### `ExitStatusRecord`

```text
size 8
0x00  u32 exit_code
0x04  u32 reason
```

`WaitProcess` writes this record. `reason` is one of the frozen v1 values:
`Normal=0`, `Killed=1`, `Faulted=2`, or `Terminated=3`. A child that is merely
blocked keeps the caller blocked; `Terminated` is used only when process policy
ends it without one of the preceding reasons.

### `DisplayRecord`

```text
size 24
0x00  u32 width
0x04  u32 height
0x08  u64 framebuffer
```

`DisplayOpen` writes this record. The `framebuffer` field is the address the
driver recorded, and it names the *same* memory the caller passed: the driver
copies no pixels, so a caller that reads a different address here has been given
a different framebuffer, which is worth knowing.

## Service contracts

Each service listing below defines its used argument slots. A later `r1`–`r6`
slot not listed for that service is ignored and preserved on return. A field
explicitly named `flags`, `mode`, or `reserved` is not ignored: it must be zero.
`ClearScreen` is the one all-zero exception. The shared crate freezes the used
count plus required-zero and ignored masks so Step 37 does not rebuild this
metadata as a second service table.

### `Exit(exit_code)`

```text
r1 = u32 exit_code
```

Terminates the calling process. Other arguments are ignored. Exit never returns
to User execution.

### `Write(handle, buffer, length, out_io_result)`

```text
r1 = FileHandle
r2 = pointer to source bytes
r3 = u64 byte length
r4 = pointer to IoResult
r5 = flags = 0
```

`length=0` is valid and returns zero transferred. The complete source range
must be readable by the process before any bytes are written. Descriptor 1 is
the process console output when the process runtime installs it.

### `Read(handle, buffer, length, out_io_result)`

```text
r1 = FileHandle
r2 = pointer to destination bytes
r3 = u64 byte length
r4 = pointer to IoResult
r5 = flags = 0
```

The complete destination range is validated writable before input is consumed.
Descriptor 0 is process console input when installed. A short read is normal.

### `Open(path, path_length, flags, mode)`

```text
r1 = pointer to path bytes
r2 = u64 path_length
r3 = u32 open flags
r4 = u32 mode = 0
```

Returns a `FileHandle` in the low payload. Paths are byte strings interpreted
by the v1 virtual filesystem. Empty/overlong paths, embedded NUL, unknown flags,
and permission failures are rejected.

### `Close(handle)`

```text
r1 = FileHandle
```

Closes one process-owned handle. Stale, foreign, or already closed handles are
rejected. Close has no silent global side effect.

### `Seek(handle, offset, origin, out_offset)`

```text
r1 = FileHandle
r2 = u64 signed offset encoded as two's complement
r3 = u32 origin: 0 = start, 1 = current, 2 = end
r4 = pointer to u64 resulting offset
```

The resulting range is checked before the handle offset changes.

### `Stat(path, path_length, out_stat)`

```text
r1 = path pointer
r2 = path length
r3 = pointer to 16-byte FileStat
r4 = reserved = 0
```

### `ListDirectory(path, path_length, records, capacity, out_io_result)`

```text
r1 = path pointer
r2 = path length
r3 = DirectoryRecord pointer
r4 = u64 output byte capacity
r5 = pointer to IoResult
```

Capacity must be a whole-number multiple of 256.

### `Time(out_cycles)`

```text
r1 = pointer to u64 CycleCount
```

Returns deterministic virtual time. Wall-clock time is not exposed through
this v1 ABI.

### `Sleep(delay_cycles)`

```text
r1 = u64 delay_cycles
```

Blocks the calling process for the specified virtual duration. The scheduler
uses virtual cycles, never a host wall clock.

### `AllocateMemory(length, alignment, out_allocation)`

```text
r1 = u64 length
r2 = u64 nonzero power-of-two alignment
r3 = pointer to MemoryAllocation
r4 = reserved = 0
```

Memory is zeroed, writable, non-executable, process-owned, and User-permitted.
The complete metadata/range check occurs before cursor movement.

### `SpawnProcess(path, path_length, argv, argc, out_handle)`

```text
r1 = executable path pointer
r2 = path length
r3 = argv pointer to argc NUL-terminated strings
r4 = u32 argc
r5 = pointer to u32 ProcessHandle
```

The child receives an empty environment in v1. The executable is loaded through
the shared `.lzx` model, not a raw machine-code blob. Spawn is atomic: an
invalid image creates no child and consumes no permanent handle.

### `WaitProcess(handle, out_status)`

```text
r1 = ProcessHandle
r2 = pointer to ExitStatusRecord
r3 = flags = 0
```

Waits for a child, writes status, and consumes the wait relationship exactly
once. It is invalid for a non-child handle.

### `ClearScreen()`

All arguments are zero. It emits a defined virtual-terminal clear operation
through the terminal service, not an SDL call and not raw framebuffer access.

### `DisplayOpen(width, height, framebuffer, out_display_record)`

The window is over the caller's own memory. `framebuffer` must be writable and
must hold `width * height * 4` bytes; the product is checked for overflow before
it is multiplied, so a geometry whose byte count does not fit a word is
`ResourceExhausted` rather than a wrapped length that would pass a bounds check.
`out_display_record` must be writable and at least `DISPLAY_RECORD_SIZE` bytes,
and it is checked **before** the device is touched: a window whose record the
caller cannot receive is a window the caller cannot know about, so it is not
opened.

### `DisplayPresent(framebuffer, out_io_result)`

Records the frame at `framebuffer` as the visible one. The result record's
`transferred` field is the number of frames presented so far, and its `status` is
what became of this present: `Ok`, or `InvalidHandle` when no window is open and
`InvalidArgument` when `framebuffer` is not the open window's address. Those are
two different program bugs, so they are two different answers.

The syscall's own return value says the *call* was valid, which a refused present
still is: a valid call about a frame that was refused is not a failed call. A
caller reads both, and `std::graphics::present` does.

## Errors

The v1 `SyscallError` set is structured and stable:

```text
UnknownSyscall
InvalidArgument
InvalidPointer
RangeOverflow
Misaligned
NotFound
AlreadyExists
PermissionDenied
InvalidHandle
NotDirectory
IsDirectory
NotSupported
ResourceExhausted
InvalidState
IoFailure
DeviceFailure
ProcessFailure
Faulted
Internal
```

Payload details are documented per call where useful; the dispatcher never
returns a raw negative Linux errno. Errors retain a cause chain internally and
map to the ABI code at the boundary.

For the display calls the detail is the **offending argument index**: `2` for the
framebuffer, `3` for the `display_open` record, and `1` for the
`display_present` result. A caller that gets `InvalidPointer` can therefore say
which of its four arguments was bad, which is the difference between a diagnostic
and a shrug.

## Pointer and buffer validation

Every pointer is a `VirtualAddress` in the calling process. The dispatcher
validates, before side effects:

1. process/thread identity, active execution token, and address-space identity
   through the exclusive `UserMemoryContext` bound to the request; the context
   also supplies the process-owned allocator, handles, CPU state, image/stack
   metadata, and lifecycle fields;
2. address-width and the complete inclusive byte range without wrapping;
3. required read/write permission and natural alignment;
4. exact known structure size and reserved fields;
5. path/string, descriptor, flag, and scalar constraints; and
6. output capacity before beginning a variable-length transfer.

A zero-length data range validates its word-sized pointer value but does not
require a mapped region because no byte is dereferenced. Nonzero ranges must
remain wholly inside one User-readable or User-writable RAM/ROM region.

Debugger `peek` is never used to authorize User memory. A pointer that becomes
invalid during a transfer is rejected by the owning process model before the
transfer; the v1 process has one stable address space during a syscall.

Short I/O may commit only the reported prefix after full destination/source
validation. Other failures are atomic. Services return after the owning state
change is committed and the typed result is constructed.

## LZ32 and LZ64

The entry and `r0`/`r1` result layout is identical. Differences are those already
defined by Lazalith:

- pointers, `FileOffset`, and architecture-word lengths use one word;
- LZ32 pointers, lengths, transfer counts, and file sizes are validated within
  32 bits before service logic sees them;
- `Seek` decodes `r2` as a two's-complement signed 32-bit value in LZ32 and as a
  signed 64-bit value in LZ64;
- all status/payload values are written as zero-extended 32-bit values, never as
  an impossible packed 64-bit LZ32 result; and
- fixed structure byte layouts do not depend on native Rust pointer size.

## Consumer contract

The single `lazalith-os-abi` crate is the only source for IDs, flags, statuses,
error values, sizes, constants, and checked layout conversion used by the
kernel, Lazen runtime, C runtime, SDK, debugger, init, and shell. Consumers
must not copy these definitions or link kernel implementation types. The Step
37 dispatcher admits an active trap exactly once, binds the request to one
`UserMemoryContext`, validates all fields before invoking an injected service,
and emits either a returning completion or a non-returning exit/fault outcome.
Concrete filesystem, process, and terminal services are intentionally supplied
by later steps; the dispatcher has no default service that pretends they exist.
Service implementations must preserve atomicity when they commit process,
file, allocator, or device state.

## Deferred services

Display/graphics, raw input device control, signals, permissions changes,
environment mutation, networking, and wall-clock time are not v1 syscalls.
Future additions use the reserved range and a new documented layout/version;
they do not reinterpret v1 structures.
