# Lazalith Boot Specification v1

## Scope and status

This document defines the initial Lazalith boot contract for both LZ32 and LZ64.
Step 28 implements the fixed loader and ROM bootloader. The OS library now
provides the native `.lzx` loader, virtual filesystem, descriptor-aware
headless terminal, process model, and composite native init/shell fixture
exercised through the boot integration test; there is not yet a packaged guest
kernel image or automatic production kernel entry routine. The machine, bus,
memory permissions, status register, virtual clock, and reference interpreter
remain the boot foundations.

The first boot mechanism is deliberately small:

```text
trusted boot ROM
    -> fixed-format kernel payload
    -> fixed kernel RAM region
    -> Supervisor kernel entry
```

It does not use a filesystem, relocations, paging, compression, or pluggable
boot drivers. The bootloader transfers only the fixed kernel payload; the
separate native `.lzx` loader is used by LazOS after the handoff.

## Architectural reset state

A cold machine is constructed with the boot image, the mandatory memory map in
this document, an explicitly empty Step 28 device manager, and virtual time zero.
No MMIO mapping is installed by this initial loader; later device/driver work may
add them only through a typed boot extension. Construction leaves
`LazalithMachine` in `Created`. Calling `reset` produces the architectural reset
state below and moves the machine lifecycle to `Reset`; the host then executes it
explicitly.

| State | Reset value |
| --- | --- |
| Machine lifecycle | `Reset` |
| CPU execution state | Running internally, but no fetch occurs until an explicit `step` or `run` |
| PC | `0x0000_0000` |
| SP | `0x0018_f000` |
| General registers | `r0`–`r15` all zero before boot code executes |
| Privilege | Supervisor (`U = 0`) |
| Interrupt enable | Disabled (`IE = 0`) |
| NZCV | Zero before boot code executes; boot instructions may change NZCV |
| Trap target | Unset; no trap frame is active |
| Pending external interrupts | None |
| Virtual clock | `CycleCount(0)` |
| Devices | Reset and synchronized to virtual time zero |

A warm `LazalithMachine::reset` retains the architecture's existing reset
semantics: it restores the initial CPU/device/epoch snapshot but does not clear
RAM or remove mappings. Booting again overwrites exactly the validated kernel
payload. Kernel software must not depend on unrelated RAM bytes being zero after
a warm reset. Device output is reset by the device contract; MMIO mappings are
preserved by the machine.

`HALT` is terminal for instruction execution: `step`, `run`, and `pause` are
rejected afterwards and only `reset` or binding a User execution context can
leave that state, because a context switch is a host-driven scheduling event
rather than continued execution of the halted program. Interrupts are disabled
at architectural reset and no device interrupt assignment exists in this boot
mechanism.

## Reset vector and boot address

The v1 reset vector and boot address are the same value:

```text
RESET_VECTOR = BOOT_ADDRESS = 0x0000_0000
```

`BOOT_ADDRESS` is an `InstructionAddress` alias of `RESET_VECTOR`, and
`BOOT_ROM_PHYSICAL_START` is the `PhysicalAddress` form of the same value; the
machine setup binds its reset program counter and ROM base directly to these
constants. `0` is representable and four-byte aligned in both LZ32 and LZ64. The first fetch must obtain eight canonical
instruction bytes from the boot ROM with Supervisor read and execute permission.
Anything else is a structured boot failure before normal boot execution.

The reset vector is a fixed address, not a runtime pointer stored in RAM. This
keeps the first fetch deterministic and does not require writable memory or a
device discovery phase.

## Mandatory initial memory map

The v1 boot map is flat and identity-mapped. Every range is disjoint, mode-width
valid, and checked as an inclusive complete range before mapping.

| Region | Start | Length | End inclusive | Access | User access |
| --- | --- | --- | --- | --- | --- |
| Boot ROM | `0x0000_0000` | `0x0008_0000` | `0x0007_ffff` | read + execute | denied |
| Kernel image RAM | `0x0010_0000` | `0x0008_0000` | `0x0017_ffff` | read + write + execute | denied |
| Kernel stack RAM | `0x0018_0000` | `0x0001_0000` | `0x0018_ffff` | read + write | denied |
| Kernel heap RAM | `0x0019_0000` | `0x0007_0000` | `0x001f_ffff` | read + write | denied |
| User code RAM | `0x0020_0000` | `0x0010_0000` | `0x002f_ffff` | read + execute | allowed |
| User data/heap RAM | `0x0030_0000` | `0x0010_0000` | `0x003f_ffff` | read + write | allowed |
| User stack RAM | `0x0040_0000` | `0x0001_0000` | `0x0040_ffff` | read + write | allowed |

No MMIO region is mandatory. Later console and driver mappings must not overlap
these ranges and must be specified by their owning steps.

The initial SP, `0x0018_f000`, is inside the kernel-stack RAM region and aligned
to both four-byte LZ32 stack words and eight-byte LZ64 stack words. Four KiB at
the top of that region is left unused as a simple guard margin. The stack still
grows toward lower addresses under the ISA CALL/RET contract; this specification
does not add hardware stack bounds checking.

Supervisor does not bypass read/write/execute permissions. The v1 kernel-image
region is writable because the bootloader must populate that same flat RAM
mapping before jumping; it remains User-denied. Step 34 owns the listed kernel
heap and User regions; later MMIO mappings remain driver policy.

## Boot image layout

The boot image is the exact `0x0008_0000` bytes of the materialized boot ROM. Fixed addresses inside the ROM are:

| Address | Contents |
| --- | --- |
| `0x0000_0000` | Bootloader entry code |
| `0x0000_0400` | Fixed boot header |
| `0x0000_1000` | Kernel payload bytes |
| `0x0008_0000` | One byte past boot ROM |

The bootloader code and bytes between these landmarks are deterministic ROM
contents. Unlisted bytes are reserved and must be zero in v1 images. The ROM
length and maximum payload follow from the map:

```text
MAX_KERNEL_IMAGE_LENGTH = 0x0008_0000
MAX_BOOT_ROM_PAYLOAD    = 0x0008_0000 - 0x0000_1000
                        = 0x0007_f000
```

The host boot-image builder uses the existing canonical ISA encoder for the small
fixed bootloader. It does not introduce an assembler, object format, or linker
before their roadmap steps.

## Fixed kernel header

All header integers are unsigned little-endian. Offsets are byte offsets from
`0x0000_0400`.

| Offset | Size | Field | v1 requirement |
| --- | --- | --- | --- |
| `0x00` | 8 | Magic | ASCII `LZBOOT01` |
| `0x08` | 4 | Format version | `1` |
| `0x0c` | 1 | Architecture | `1 = LZ32`, `2 = LZ64` |
| `0x0d` | 1 | Flags | `0`; every other value is unsupported |
| `0x0e` | 2 | Header size | `48` |
| `0x10` | 8 | Kernel load address | exactly `0x0010_0000` |
| `0x18` | 8 | Kernel image length | `1..=0x0007_f000` (boot-ROM payload limit) |
| `0x20` | 8 | Kernel entry offset | relative to the kernel load address |
| `0x28` | 4 | Payload checksum | CRC-32/ISO-HDLC of payload bytes |
| `0x2c` | 4 | Reserved | `0` |

CRC-32/ISO-HDLC uses the reflected polynomial `0xedb88320`, initial value
`0xffffffff`, and final XOR `0xffffffff`. The checksum covers exactly
`kernel_image_length` bytes beginning at ROM address `0x0000_1000`; it does not
cover the bootloader or header.

The entry must satisfy all of these conditions:

```text
entry_offset % 4 == 0
entry_offset < kernel_image_length
entry_offset + 8 <= kernel_image_length
entry_address = 0x0010_0000 + entry_offset
entry_address + 8 - 1 lies inside the kernel image RAM mapping
```

All header arithmetic is checked without guest-width or host-width wrapping.
LZ32 additionally rejects any nonzero address or length above `0xffff_ffff`
before truncating it, even though the fixed v1 values already fit both modes.
The first eight bytes at the entry must decode as one canonical instruction for
the selected architecture. Later instructions are validated by normal execution
faults; the initial bootloader does not attempt whole-kernel semantic analysis.

## Boot image validation and loading

Boot-image parsing and all validation occur before constructing or starting the
machine. The trusted host validates, in this order:

1. Materialized ROM length is exactly `0x0008_0000`.
2. Magic and fixed header size match.
3. Format version, architecture, flags, load address, and reserved field match.
4. Image length is nonzero, mode-valid, and no larger than the boot-ROM payload
   capacity and mandatory RAM region.
5. Payload plus fixed ROM offset stays inside boot ROM without wrapping.
6. Entry offset, checked entry address, and complete first instruction range
   are valid and executable.
7. The first instruction decodes canonically for the selected configuration.
8. The payload checksum matches.
9. The bootloader prefix is the canonical generated code, and all bytes not
   assigned to code, header, or payload are zero.
10. The mandatory boot ROM, kernel image, and kernel stack mappings are mutually
    disjoint and satisfy their permissions; this is enforced when the host
    constructs the machine, because region overlap is rejected by the address
    space itself.

After all static checks, the host creates the machine with the boot ROM,
mandatory RAM mappings, and explicit reset state. The parser itself writes no
kernel RAM. On a cold machine the new RAM backing begins zero. The bootloader
copies the complete validated ROM payload into the kernel image RAM region and
jumps to the kernel. Before `start` exposes the machine, it verifies the complete
documented CPU handoff registers and then performs a pure Supervisor execute
fetch at the entry, so a corrupted handoff is reported before any guest
instruction is retired. Loading and execution are never interleaved with
unchecked input.

The Step 28 API constructs a fresh machine for each boot. The machine layer's
warm-reset/RAM-preservation semantics remain available to a future typed reboot
owner, but this step does not expose an unchecked arbitrary-machine reboot API.

The fixed bootloader repeats the header-width, fixed-load-address, payload
capacity, and entry-range guards at runtime before copying. Those checks guard
against an internally corrupted boot ROM; they are not a parser for untrusted
files. A failed trusted-host validation
returns a structured boot error and does not run guest code. An impossible
runtime guard failure in an already validated immutable ROM branches to
Supervisor `HALT` rather than jumping through an unvalidated address. HALT is
not used as a substitute for structured host-side validation.

## Bootloader responsibility

The bootloader performs only these actions:

1. Start from the architectural reset state.
2. Read the fixed boot header and locate the payload at `0x0000_1000`.
3. Revalidate header widths, fixed load address, payload capacity, and entry.
4. Copy the complete payload into the kernel image RAM region.
5. Transfer control to the validated kernel entry.

It does not initialize interrupts, install a trap handler, enable User mode,
create a heap, parse a filesystem, relocate code, discover devices, create
processes, dispatch system calls, or implement LazOS policy. Those are later
kernel responsibilities.

The host performs checksum validation before execution. The ROM is immutable,
so the runtime bootloader does not recompute CRC-32; doing so would add code
without changing the trusted-input contract.

## Kernel entry contract

On successful transfer, the bootloader establishes this deterministic handoff:

| Item | Kernel entry value |
| --- | --- |
| PC | Validated absolute kernel entry |
| SP | `0x0018_f000` |
| Privilege | Supervisor |
| IE | Disabled |
| NZCV | Boot-loader result; not part of the stable handoff |
| `r0` | `0` (boot transfer completed) |
| `r1` | Kernel load address `0x0010_0000` |
| `r2` | Kernel image length |
| `r3` | Kernel entry address |
| `r4`–`r14` | Zero |
| `r15` | Kernel entry address |

The kernel entry is Supervisor-only and must begin in the executable kernel
image. It is responsible for validating any additional boot information it needs
before trusting it. The later trap controller is initially unset; the kernel
must not execute trap-return or control-register operations until Step 31 gives
them real controller semantics.

## LZ32 and LZ64

Both modes use the same physical boot map and handoff values because all v1
addresses fit below 4 GiB. They differ only in existing architectural behavior:

- registers and general pointers are 32-bit in LZ32 and 64-bit in LZ64;
- CALL/RET stack words are four bytes and eight bytes respectively;
- the shared SP is valid and aligned for both;
- address-width validation and canonical instruction decoding use the selected
  `ArchitectureConfig` in both modes;
- LZ32 rejects high-half addresses instead of truncating them.

There is no mixed-mode boot image and no automatic pointer conversion.

## Rejected alternatives for v1

The following are deliberately deferred rather than partially emulated:

- filesystem discovery and boot files;
- `.lzo`/`.lzx` containers, relocations, and debug metadata;
- decompression, encryption, signatures, or multi-stage boot;
- device probing and driver selection;
- paging, page tables, and per-process address spaces;
- user-mode entry from the bootloader;
- trap-controller installation or interrupt policy;
- console, input, timer, or power-management policy.

These exclusions prevent the bootloader from acquiring OS responsibilities. A
later step may replace the fixed kernel-image mechanism only through an explicit
specification and migration decision.
