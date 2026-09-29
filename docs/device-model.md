# The device model

`binstruction.md` §25 and §26 ask for a tiered hardware model and for devices to
be separated into a guest-facing **front end** and a host-facing **back end**.

**Status: the front end is built, and so is the heterogeneous device set; the back
end does not exist.** This document states what is real, names what remains, and
describes the split `binstruction.md` asks for.

---

## 1. What is real

**Fact.** The guest-facing device contract is the `Device` trait in
`lazalith-devices`:

```rust
pub trait Device: fmt::Debug {
    fn address_len(&self) -> u64;
    fn reset(&mut self);
    fn validate_read(&self, offset: DeviceOffset, size: DataSize) -> Result<(), DeviceError>;
    fn validate_write(&self, offset: DeviceOffset, size: DataSize, value: u64) -> Result<(), DeviceError>;
    fn read(&mut self, offset: DeviceOffset, size: DataSize) -> Result<u64, DeviceError>;
    fn write(&mut self, offset: DeviceOffset, size: DataSize, value: u64) -> Result<(), DeviceError>;
    fn peek(&self, offset: DeviceOffset, output: &mut [u8]) -> Result<(), DeviceError>;
    fn tick(&mut self, elapsed: CycleCount);
    fn snapshot(&self) -> Vec<u8>;
    fn restore(&mut self, bytes: &[u8]) -> Result<(), DeviceError>;
}
```

Four devices are implemented: `ConsoleDevice`, `TimerDevice`, `DisplayDevice`,
`InputDevice`. Three properties of the contract are load-bearing and all three are
tested:

**Validate before mutate.** Every write path validates and only then writes, so a
refused access leaves every byte exactly as it was.
`crates/lazalith-memory/tests/hardening_validate_before_mutation.rs` checks this in
twelve different ways.

**A device's snapshot is its own encoding.** A common encoding across devices would
have to be a lowest common denominator that lost whatever made each one different,
so each device encodes itself and `restore` refuses bytes that are not its own. A
display's state cannot be restored into an input device, and a snapshot of the
wrong length is refused rather than padded. An input device's snapshot carries the
**whole event queue**, not just the counters, because a program owns that queue and
a snapshot that kept the counters but not the events would hand the same keystroke
to the program twice.

**A device has no host in it.** No window, no file handle, no socket, no host
clock. That is what lets the emulator run headlessly, which the whole test suite
depends on — every graphics, input and window test in this repository runs with no
display server.

**A device's guest-visible state is the *whole* of its state.** A display's window
geometry and frame count are in, because a program reads both through MMIO and is
entitled to see them again after a restore. The host clock a device ticks against
is out, and so is any buffer the host is using to stage a window.

---

## 2. The constraint that stopped the back end — and how B4 removed it

**Fact, before B4.** `DeviceManager<D>` was monomorphic:

```rust
pub struct DeviceManager<D: Device> {
    entries: Vec<Entry<D>>,   // every entry is the SAME concrete D
    clock: VirtualClock,
}
```

One machine held exactly one concrete device type. A machine could have a console,
*or* a timer, *or* a display, *or* an input device. It could not have two of
different kinds at once.

**This is why `binstruction.md` §26's model could not be built on the old shape.**
The four-layer model is:

```text
Guest
  ↓
guest-visible device          ← the front end. This exists.
  ↓
Lazalith device model         ← this is the trait. This exists.
  ↓
host backend                  ← §5, as of B5. `Backend` and `BlockBackend` exist.
```

You cannot attach a *different* backend to different devices of the same machine
when the manager has one homogeneous slot for all of them. A machine with a console
backed by stdio and a display backed by SDL3 needs two different concrete device
types in one device list, and the list could not hold that.

`BootImage::machine_setup` made the consequence explicit by refusing any non-empty
device manager at all (`BootError::UnexpectedDevices`).

**B4 removed the blocker, additively.** `Box<dyn Device>` now implements `Device`,
forwarding all ten methods, so `DeviceManager<Box<dyn Device>>` is a `DeviceManager`
of some `D: Device` and every generic above it was already written in terms of `D`.
`LazalithMachine<Box<dyn Device>>` is a machine built from the same constructor,
with the same `map_device`, the same routing by `DeviceId` and the same per-device
snapshot contract.

**Not one existing call site changed.** `LazalithMachine<ConsoleDevice>` is still
monomorphic; `NoDevice` still means *a machine that cannot hold a device at all*,
which is stronger than an empty erased list. Both distinctions are tested.

**The cost** is a dynamic call per register access, and it is opt-in: a machine with
one kind of device should keep its concrete `D`. `docs/machine-profiles.md` §2 has
the full reasoning.


---

## 3. What the display and input devices actually are

**Fact, and it is worth knowing before designing anything for them.**
`DisplayDevice` and `InputDevice` are **not machine devices in the boot path**.
`DisplayService` and `InputService` own a `DisplayDevice` and an `InputDevice`
*inside the kernel*, and a guest reaches them only through
`display_open` / `display_present` / `input_poll` syscalls.

There is no MMIO path to a framebuffer, by design: a Lazen program has no way to
obtain a device address, which is stated in the display service's own header
comment. A program owns its framebuffer in its own memory; `display_open` copies
nothing, so the pixels on screen are the bytes the program wrote.

**Consequence for the back end.** The separation of concerns the kernel already
enforces — guest-visible service above, device model below, no address handed to
the guest — is the same separation `binstruction.md` §26 asks for, one level
different. A `lza64-at-v1` compatibility profile would add a *presentation* device
that a Linux 0.01 kernel drives the way a PC driver drives VGA, and that device
would live beside the existing ones rather than replacing them.

---

## 4. The hardware tiers, and what is in each

`binstruction.md` §25 proposes three tiers. Against what exists:

### Architectural core

| | Status |
| --- | --- |
| CPU | real — `lazalith-cpu` |
| memory, bus | real — `lazalith-memory` |
| interrupt controller | real — `InterruptController` in `lazalith-machine`, plus `TrapController` for traps |
| timers | real as a device — `TimerDevice`; **not** wired into `advance_clock` in any boot path |
| virtual clock | real — `VirtualClock`, and the differential suite holds that a step never advances it |
| DMA | **absent** |
| reset / power model | reset real; **power states absent** |
| firmware / boot interface | real — the boot ROM |
| MMU / address translation | **absent.** `AddressSpace` is regions with permissions and an identity for context switching. There is no translation, no page table, no `CR3` |
| CPU topology / SMP | **absent** |

### Standard computer hardware

| | Status |
| --- | --- |
| display | real, as a syscall service |
| keyboard / mouse / input | real, as a syscall service, with a host input adapter for scripted input |
| serial | **absent** |
| RTC | **absent.** `VirtualClock` is not a guest-readable clock |
| block storage | **absent.** The filesystem is in-memory |
| network | **absent** |
| audio | **absent** |

### Expansion and compatibility

Everything in this tier is **absent**: USB, PCI/PCIe, VGA, IDE/ATA, NVMe,
VirtIO-style devices, SoundBlaster, AC'97, HDA, TPM.

**That is a deliberate ordering, not an oversight.** `binstruction.md` §25 says "do
not implement everything immediately" and §51 says "do not implement peripherals
merely because they exist on a checklist". A device is added when something in the
platform needs it: `lza64-at-v1` exists because the Linux 0.01 port needs it, and
`docs/linux-0.01-port.md` records which devices that is and which it is not yet
proven to be.

---

## 5. The front end / back end split, as it should be built

**Research.** QEMU's device emulation documentation separates four concepts:
a **device front end** is "how a device is presented to the guest"; a **device bus**
is what a device is attached to; a **device back end** is "how the data from the
emulated device will be processed by QEMU"; and **pass-through** is giving a device
access to real underlying hardware. Back ends "can sometimes be stacked to
implement features like snapshots", and "while the choice of back end is generally
transparent to the guest, there are cases where features will not be reported to the
guest if the back end is unable to support it".
<https://www.qemu.org/docs/master/system/device-emulation.html>

`binstruction.md` §26 asks for that separation and says the APIs must be
Lazalith-specific. What Lazalith should take from it, and what it should not:

**Take:** the three-layer shape, and the statement that a back end may be unable
to support something the guest asked for. That second one is the important half,
and it has a direct consequence for this platform's philosophy — the device model
has been strict about *refusing* rather than approximating, so a back end that
cannot do something has to say so to the guest, not emulate it badly.

**Do not take:** QEMU's bus hierarchy as a model for the native profile. A native
LZA expansion bus (§12's "a native Lazalith expansion bus may come before PCI")
should be simpler, and PCI should arrive as a *compatibility profile* rather than
as the platform's only way to attach a device.

**The shape, as built in B5:**

```text
Device (guest-visible)          the trait that exists today
    ↓
BlockDevice                     the first device with a backend. §5.1
    ↓
BlockBackend                    sectors in, sectors out. No guest vocabulary.
    ├── MemoryBlockBackend      a flat buffer, writable or read-only
    ├── CopyOnWriteBlockBackend a sparse overlay over a read-only base
    └── AbsentBlockBackend      no storage, and says which device asked
```

`Backend` is the whole of it — `kind`, `identity`, `reset` — and `BlockBackend`
adds `capacity`, `writable`, `read_sector`, `write_sector`. Four backends ship:
`MemoryBlockBackend`, `CopyOnWriteBlockBackend`, `AbsentBlockBackend`, and the
`CopyOnWriteBlockBackend` chain over a read-only memory base that
`BlockStorage::CopyOnWrite` builds. `Sdl3DisplayBackend` and `HostInputBackend`
from the earlier sketch are **still proposals**: the display and input devices have
no backend, and §5 is a block-storage boundary first, not a general one.

**Three rules**, in the order they matter:

1. **A backend is never a guest interface.** No guest-visible type, register, or
   error may name one. If it can, a guest can be built against a host choice, and
   the guest stops being portable between machines.
2. **A snapshot names the device's state, not the backend's.** A backend may be
   unable to restore its own state — a file that moved, a socket that closed — and
   the machine snapshot has to say so rather than restore a device that is quietly
   attached to nothing.
3. **A backend that cannot support what the guest asked reports it as a device
   fault, not as silence.** `DeviceError` already has the shape for this; the
   question of which faults are `Unmapped` and which are `Device` is settled per
   device.

### 5.1 The block device

The chain is real, and `BlockDevice` is its middle box. The transfer crosses it
as **whole 512-byte sectors**, and a register access names a register in the
device's own 64-byte window — never a sector, and never an offset into the disk.
That is the whole of the translation, and `BlockDevice` has no accessor that
returns the backend, so the only way a sector reaches the host is a command a
guest issued.

| Offset | Register | Access |
| --- | --- | --- |
| 0 | `BLOCK_REGISTER_CAPACITY` | read — sectors the storage holds |
| 8 | `BLOCK_REGISTER_SECTOR` | read/write — the sector the next command applies to |
| 16 | `BLOCK_REGISTER_COMMAND` | read/write — `COMMAND_READ`, `COMMAND_WRITE` |
| 24 | `BLOCK_REGISTER_REMAINING` | read — bytes of the transfer not yet moved |
| 32 | `BLOCK_REGISTER_DATA` | read/write — the data port |
| 40 | `BLOCK_REGISTER_STATUS` | read — `READABLE`, `WRITABLE`, `FAILED`, `BUSY` |

**Why a data port rather than guest memory.** `Device` gives a device
`read`/`write` on registers and no access to guest memory, so a buffer-backed
command would need a new method on the `Device` trait — a change to every device
in the workspace, for one device's benefit. A port moves the same bytes using the
contract that already exists, and it is the shape real port-I/O disks have for
the same reason.

**Three refusals worth naming**, because each is a case where the obvious
alternative is a lie:

- A data-port access past the end of the sector is **refused, not padded**. A short
  read hands the guest bytes that were never stored, and a program cannot tell them
  from data.
- A write during a read is **refused, not ignored**. A guest that thought it was
  writing would be silently losing those bytes.
- A copy-on-write overlay over a **writable** base is **refused**. An overlay that
  does not write through today is not copy-on-write; it is a stack of layers that
  happens not to, and nothing would notice until a future backend that did.

**Snapshots carry the device, not the disk.** 29 bytes: the selected sector, the
failure flag, the storage's identity, and the direction and progress of any
transfer. The storage itself is a host resource, and a machine snapshot that
copied a 64 MiB disk would be holding a second copy of it — the same reason a
display's snapshot excludes the framebuffer.

Two things make that omission safe rather than silent:

- the **identity** travels in the snapshot, so restoring onto a machine whose disk
  is a different disk is a **refusal**, not a best effort;
- the **transfer state** travels too. `Device::snapshot` returns `Vec<u8>` and so
  cannot refuse, and a snapshot that refused would have to be lying about having
  one. Instead the snapshot is a faithful record and `restore` is the strict half:
  a snapshot taken mid-transfer records that it was, and restoring it is refused,
  because a machine restored into a half-moved sector has a data port mid-sector
  with nobody to finish it.

### 5.2 What a backend is not, and where that is checked

`Backend` and `BlockBackend` take no `DataAccess`, `PhysicalAddress`,
`DeviceOffset`, `Privilege` or `CycleCount`. There is no `impl Device` for any
backend. `BlockDevice` has no `backend()` accessor — only `backend_kind()`,
which names a `BackendKind` and reaches no resource.

Those four are claims about the *shape* of the code, and no behavioural test can
observe the absence of a method: a rewrite that added `fn backend()` to the block
device would pass every functional test in the workspace while breaking the thing
the module is for. So they are checked by reading the source, in
`crates/lazalith-cli/tests/architecture.rs` — `a_backend_is_not_a_device`,
`a_backend_signature_carries_no_guest_vocabulary`,
`no_device_exposes_the_backend_behind_it`, and `the_backend_layer_is_no_std`.

---

## 6. What is not here

- **No storage, audio, network, USB, PCI or VGA.** Not "simplified": absent —
  except block storage, which B5 built as the §5 worked example.
- **No file, sparse-image or socket backend.** §5's boundary is real; the backends
  in it are `MemoryBlockBackend`, `CopyOnWriteBlockBackend` and
  `AbsentBlockBackend`. A file backend is B8, and it belongs in a host crate
  because `lazalith-devices` is `no_std`.
- **No display or input backend.** `BlockDevice` is the first device with one.
  `Sdl3DisplayBackend` and `HostInputBackend` remain proposals.
- **No `lza64-virt-v1` or `lza64-at-v1`.** Both are nameable; both are refused.
  See `docs/machine-profiles.md` §4.
- **No DMA, no MMU, no SMP, no power states.** See the tier tables.
- **No device discovery or bus topology.** B12, and it needs B4's profile to have
  somewhere to record what was discovered.

---

## Related

- `docs/virtual-machine.md` — what a device set is part of
- `docs/machine-profiles.md` — where a device inventory is described
- `docs/compatibility.md` — the tier the AT devices belong to
- `docs/linux-0.01-port.md` — the device inventory the port actually proves it needs
- `docs/lazen-graphics.md`, `docs/lazen-input.md` — the display and input paths as built
