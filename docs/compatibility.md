# Compatibility hardware

`binstruction.md` §25 puts USB, PCI/PCIe, VGA, IDE/ATA, NVMe, VirtIO-style
devices, SoundBlaster, AC'97, HDA, TPM and legacy controllers in a third tier it
calls *Expansion / Compatibility*, and §28 says VGA in particular is a
compatibility architecture rather than part of LZA.

**Status: none of it exists.** This document is about what that tier is for, what
it must not be, and the one compatibility machine that is actually planned.

---

## 1. What the tier is for

**Research.** QEMU keeps many target architectures and many machine types, and its
documentation is explicit that "options, properties, and command lines that work
for one architecture or machine type will not necessarily work for another".
<https://www.qemu.org/docs/master/system/introduction.html>

That is the shape of the tier. The native LZA machine is one machine; the
compatibility tier is a set of machines that present hardware an *existing*
software already knows how to drive, so that software can be ported by changing
its architecture assumptions rather than by being rewritten.

**Lazalith's own rule** (`binstruction.md` §51) is stricter than QEMU's and is the
one to follow:

> Do not implement peripherals merely because they exist on a checklist.
> Prioritize by real guest/software requirements.

So the tier is populated by what a guest *proves* it needs, in that guest's
source, and not by a list. §46 makes the same point: "do not assume a device is
required merely because it existed on the historical PC."

**The one member of this tier with a proven requirement is `lza64-at-v1`**, and
its inventory is being derived from the Linux 0.01 source rather than from a
checklist. See `docs/linux-0.01-port.md`.

---

## 2. What compatibility hardware must not be

These are the failure modes the tier is most likely to produce, and they are worth
writing down before any of it exists.

**It must not become the ISA.** `binstruction.md` §60 requires: "VGA is
compatibility hardware, not the LZA ISA." A VGA register write is an MMIO write to
a device window; it is not an instruction, it is not privileged by the ISA, and a
program that uses it is a program that can only run on a machine that has it. The
distinction has to be visible in the *profile* — `lza64-at-v1` carries
`compatibility: at-v1` — rather than in a flag on a device.

**It must not become the default.** The native profile must be buildable and
runnable with no compatibility device present, or every guest will quietly depend
on one and stop being portable. This is why `lza64-native-v1` is the profile to
build first, and why it is the one B25/B26 need.

**It must not change guest-visible semantics.** A device's registers are MMIO
addresses. A compatibility device is a device. If adding it changes what `ADD`
means, or what a syscall returns, or what a `.lzx` file means, then it is not a
device.

**A compatibility device must be *documented as a compatibility device*.** A
future reader who finds a `VgaDevice` and cannot tell whether it is part of LZA or
a historical courtesy has been left worse off than if it did not exist. Every
compatibility type says so in its own module documentation, names the software it
exists for, and names the profile that carries it.

---

## 3. VGA and display, specifically

**Fact.** The native display abstraction exists and works: `DisplayDevice` with
guest-owned framebuffers, presented frames and window geometry, reached by a guest
through `Lazen SDK → LazOS display driver → device`, with no SDL3 anywhere in that
path. `docs/lazen-graphics.md` has the detail and its measurements.

`binstruction.md` §28 asks for a future compatibility presentation device
alongside it, and says the host may render with Rust `sdl3` while the guest never
depends on SDL3. That separation is already the native design's shape; a
compatibility presentation device would be a second front end over the same
backend, which is exactly why the device model wants a backend layer
(`docs/device-model.md` §5).

**Nothing VGA exists.** No VGA text mode, no VGA graphics mode, no CRTC, no
palette, no `0xB8000` segment, no int 10h. Linux 0.01's console driver is the
reason this would ever be wanted, and its source has not yet been read for the
purpose — see `docs/linux-0.01-port.md` §5.

**Research required before implementation, per `binstruction.md` §28:** VGA/EGA
behaviour must come from historical primary sources — the IBM VGA and EGA
technical references and the Linux 0.01 console driver's own register accesses —
and not from a description of VGA in a modern manual. This is recorded as work to
do, not work done.

---

## 4. Storage controllers, and why the tier is the wrong place for the first one

**Fact.** There is no storage at all. The filesystem is in-memory
(`VirtualFileSystem`), the process loader reads images the host hands it, and there
is no block device in any boot path.

`binstruction.md` §31 puts IDE/ATA, VirtIO block and NVMe in the compatibility
tier, but that is a statement about *which* controllers, not about whether the
platform needs storage at all. Storage is B8, and it is a native guest block
device over a host backend — a file, a sparse image, a copy-on-write layer — before
any controller exists.

**A controller is a translation layer over a block device, not a block device.**
`lza64-at-v1` may need one because Linux 0.01's driver does, and the driver in the
source will say so. Until then there is nothing for a controller to translate.

---

## 5. Expansion bus, and why a native one comes first

`binstruction.md` §33 says a native Lazalith expansion bus may come before PCI,
and that PCI/PCIe can later become a compatibility profile. That ordering is
right, and the reason is that device *discovery* is a native concern and PCI is a
historical one.

**Absent:** any discovery mechanism, configuration space, interrupt routing for
devices, or bus topology. B12, and it needs B4's profile to have somewhere to
record what was discovered — which is why it is listed after B8/B9 in the roadmap
rather than before them.

---

## 6. Inventory

Every item in this document, with its status. Nothing here is a partial
implementation; every row is either absent or planned.

| Item | Status | Blocked on |
| --- | --- | --- |
| `lza64-at-v1` machine profile | designed | B4 |
| VGA-compatible presentation device | designed, not built | `lza64-at-v1`, VGA research from primary sources |
| IDE/ATA | absent | a block device (B8) and a proven guest need |
| NVMe | absent | same |
| VirtIO-style devices | absent | same |
| PCI / PCIe | absent | a native expansion bus (B12) |
| USB | absent | a native bus, and no guest needs one |
| SoundBlaster, AC'97, HDA | absent | an audio device (B10), which does not exist |
| TPM | absent | nothing |
| legacy controllers | absent | nothing |

**There is no audio device, so there is no audio compatibility work to do.** B10
is not started and the sound cards in `binstruction.md` §29's list are
future compatibility-machine work that depends on it.

---

## Related

- `docs/device-model.md` — the tier this belongs in, and the backend layer it needs
- `docs/machine-profiles.md` — where `lza64-at-v1` is described
- `docs/linux-0.01-port.md` — the only proven requirement in this tier
- `docs/virtual-machine.md` — the VM contract a compatibility device must not break
