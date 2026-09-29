# Machine profiles

`binstruction.md` §27 asks for versioned machine profiles, and §53 places them at
**B4** — immediately after the VM core boundary, before the device model split.

**Status: designed, not built.** This document is the design and the reasons for it.
It is written before the code on purpose: a profile format is a compatibility
promise, and a format that has been used once is much harder to change than one
that has never been used.

---

## 1. What a profile is

**Requirement** (`binstruction.md` §27). A profile describes:

```text
architecture
CPU
RAM
firmware
boot behaviour
interrupt model
timer
device inventory
MMIO/PIO map
display
input
storage
serial
network
audio
compatibility behaviour
```

**And it must be versioned to preserve guest compatibility.** That last clause is
the whole design constraint. A profile is not a configuration file; it is the
*statement of what a machine is*, and a guest that booted on `lza64-virt-v1` must
still boot on it after the implementation changes. So a profile is addressed by
name **and version**, the version is part of the identity, and an incompatible
change is a new name-version rather than an edit.

---

## 2. Why it is the next stage, and not an earlier one

**The dependency argument**, which is the reason and not a preference:

- **B5, the device frontend/backend split, needs something to attach backends
  to.** A backend answers "how does this guest block device reach storage". The
  profile is what says "this machine has a block device, and its backend is a
  copy-on-write layer over a raw image". Doing B5 first means inventing that
  description and then moving it.
- **B8, B10, B11 (storage, audio, networking) each need it.** Each of those is a
  guest device plus a host backend, and "which backends, and of what kind" is a
  property of the machine, not of the device class.
- **B13, firmware and boot profiles, are defined by it.** §34's chain is
  `VM Manager → Machine Profile → Firmware → Bootloader → LazOS`. The profile is
  second in that chain, not a peer of the firmware.
- **B19, the VM manager, exists to create and run profiles.** A management API
  with nothing to manage is a wrapper around `LazalithMachine::new`, which is
  what exists now.
- **B27, `lza64-at-v1`, is a profile plus a compatibility device set.** It is
  three stages before the Linux port that needs it.

**What exists today that a profile has to absorb.** The machine's configuration is
currently scattered over four crates:

| What | Where it lives |
| --- | --- |
| physical layout constants (ROM, header, payload, load address, initial SP) | `crates/lazalith-boot/src/lib.rs` |
| kernel regions (image, stack, heap) | `crates/lazalith-os/src/memory.rs`, `KernelMemory::regions` |
| user regions (code, data, stack) | `crates/lazalith-os/src/memory.rs`, `UserMemory::regions` |
| `pc` / `sp` / `status` / `initial_time` | `crates/lazalith-boot/src/image.rs`, `BootImage::machine_setup` |
| the trap vector | set **after** construction, by each caller |
| the device set | supplied by each caller, and **monomorphic** |

`BootImage::machine_setup` is the nearest thing to a single description, and it is
one function in one crate, and it refuses any non-empty device manager outright
(`BootError::UnexpectedDevices`). That refusal is the clearest statement of what a
profile would fix.

---

## 3. A first-order constraint a profile has to solve

**Fact.** `DeviceManager<D>` is monomorphic. Every entry in a machine's device
manager is the same concrete type `D`. There is no trait object, no device enum,
no heterogeneous device set.

A machine today can hold a console, *or* a timer, *or* a display, *or* an input
device — but not two of different kinds at once. A profile that says "this machine
has a console and a display and a timer and a disk" is therefore not a matter of
writing a description; it is impossible to honour until the device manager can hold
more than one kind.

**So B4 is two things, in this order:**

1. **heterogeneous devices** — a device set that is a collection of distinct
   device types, and
2. **the profile** that describes one.

Doing the profile first would produce a description the machine cannot be built
from, which is the same failure as documenting an interface before deciding which
layer it belongs to. The heterogeneous device set is small: a `Vec<Box<dyn Device>>`
with each entry carrying its `DeviceId` and its address length, which is the shape
`DeviceManager` already has internally (`Vec<Entry<D>>` with `id` and `length`).
This is B5's first half and it is a prerequisite, not a separate project.

---

## 4. The three profiles `binstruction.md` names

| Name | What it is for |
| --- | --- |
| `lza64-virt-v1` | the general-purpose profile. What the platform's own software targets. |
| `lza64-native-v1` | the minimal profile: CPU, memory, console, timer. No display, no storage. The one a bootloader or a freestanding kernel is built against. |
| `lza64-at-v1` | the compatibility profile for the Linux 0.01 port. AT-derived hardware, an LZA-compatibility presentation device, keyboard, and whatever else the port's source actually proves it needs. |

**`lza64-native-v1` is the one to build first**, and the reason is dependency
order: B25 and B26 — target-side LazOS in C, and LazOS built by the Lazalith
toolchain — need a profile that means "a machine with nothing on it but a CPU and
memory", because a kernel does not want a display it will never draw on. A
`virt`-shaped profile as the only one would mean every freestanding target has to
opt out of devices it never asked for.

**`lza64-at-v1` is the one with the most research left**, and it should not be
written from a list. `binstruction.md` §46 says "do not assume a device is required
merely because it existed on the historical PC" and "prove the dependency from
source". See `docs/linux-0.01-port.md`.

---

## 5. Shape of the description

**Proposal.** A profile is data, not code, and it is versioned in the same
namespace as the machine it describes.

```text
lza64-native-v1
    architecture   LZA64
    cpu            default
    ram            { base, length }
    firmware       { kind, entry }
    boot           { vector, kernel load, kernel initial sp }
    interrupts     { controller, vector }
    timer          { cycles-per-tick, irq }
    devices        [ { id, kind, address, length, permissions, backend } ]
    compatibility  { profile: none | at-v1 }
```

Three decisions in that shape are worth stating before the code, because they are
the ones that are expensive to change:

1. **`devices` is a list, not a map keyed by name.** A machine may have two
   displays; a map would have to decide which one is "the" display, and that
   decision belongs to the guest, not the host.
2. **Every device carries its `backend` explicitly, including `none`.** A missing
   backend and a deliberately-absent one must not be the same value, or a machine
   profile that forgot to say so would silently produce a device that does nothing.
3. **`compatibility` is a field, not a separate profile type.** `lza64-at-v1` is a
   profile that happens to be a compatibility profile, and encoding "is it AT" as
   a type rather than as data would make a profile that is *partly* AT impossible
   to express — which is exactly the profile the Linux port may turn out to need.

**The versioning rule:** the version is part of the name (`-v1`), a change that
any guest could observe is a new version, and a change that no guest can observe is
an edit. The archive, object and executable formats in this repository already
work that way — `BOOT_FORMAT_VERSION`, `LZX_FORMAT_VERSION`, `LZA_FORMAT_VERSION`,
`LZX_ISA_VERSION`, `LZX_ABI_VERSION` — and a machine profile should not invent a
second convention.

---

## 6. What a profile must not be able to do

Stated now, because a machine description that can do these things is not a
description of a machine.

- **It cannot change guest-visible semantics.** A profile chooses which devices
  exist and where they are mapped. It does not get to decide what `ADD` means, what
  a syscall returns, or what `.lzx` contains. Those are the architecture, and they
  are fixed by the ISA and ABI versions, which the profile *records* and does not
  *define*.
- **It cannot overlap its own mappings.** The bus already refuses overlapping
  regions and a device that is already mapped; a profile that asked for one would
  be refused at load, and that refusal is a test, not a runtime surprise.
- **It cannot produce a machine that fails its own description.** A profile is
  checked against the machine built from it, and a disagreement is a build failure.

---

## 7. What B4 is, concretely

The smallest correct stage, in order:

1. **Heterogeneous device set** — `DeviceManager` over a collection of distinct
   device types, preserving the existing per-device `validate`/`snapshot`/
   `restore` contract and the existing "a snapshot of the wrong shape is refused"
   behaviour. `BootImage::machine_setup` stops refusing a non-empty manager.
2. **`MachineProfile`** — the data above, with a version, a name, and validation
   that refuses what §6 forbids.
3. **`LazalithMachine::from_profile`** — and a test that a machine built from a
   profile and then inspected matches the profile that described it. That test is
   the deliverable; a profile type with no round-trip check is a comment.
4. **`lza64-native-v1`** as the first real profile, because it is the one B25 and
   B26 need.
5. **CI**: the round-trip test runs in the `architecture` job, and a new
   compatibility test for the profile encoding goes in the `campaign` job if the
   encoding is something malformed input could reach.

**What is not in B4:** `lza64-virt-v1`, `lza64-at-v1`, storage, audio, networking,
hot-plug, or any change to the device trait. Those are B5, B8, B10, B11 and B12,
and B4 should make them possible without doing any of them.

---

## Related

- `docs/virtual-machine.md` — what a profile configures
- `docs/device-model.md` — the device contract a profile's `devices` list uses
- `docs/toolchain.md` — why the compiler does not need a profile, and the sysroot
- `docs/linux-0.01-port.md` — where `lza64-at-v1` comes from
- `docs/beyond-lazalith.md` — the roadmap status
