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
host backend                  ← SDL3, a file, a socket, a host audio API.
                                 THIS DOES NOT EXIST.
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

**The shape, as a proposal:**

```text
Device (guest-visible)          the trait that exists today
    ↓
Backend                         how the host's resources are used
    ├── FileBackend             a path on the host
    ├── SparseBackend           a sparse image
    ├── CowBackend              a copy-on-write layer over another backend
    ├── MemoryBackend           a buffer, for tests and for snapshots
    ├── Sdl3DisplayBackend      the only host backend that exists today
    └── HostInputBackend        scripted input, for deterministic replay
```

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

---

## 6. What is not here

- **No storage, audio, network, USB, PCI or VGA.** Not "simplified": absent.
- **No backend abstraction of any kind.** §5 is a proposal, and it is B5.
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
