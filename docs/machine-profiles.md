# Machine profiles

`binstruction.md` §27 asks for versioned machine profiles, and §53 places them at
**B4** — immediately after the VM core boundary, before the device model split.

**Status: B4 is implemented.** The heterogeneous device set, the profile type, the
validation, `LazalithMachine::from_profile`, and `lza64-native-v1` all exist and
are tested. `lza64-virt-v1` and `lza64-at-v1` are designed and not built; §4 and
§7 say why and what each is waiting on.

---

## 1. What a profile is

**Requirement** (`binstruction.md` §27). A profile describes:

```text
architecture
CPU
RAM
firmware
boot behavior
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
compatibility behavior
```

**And it must be versioned to preserve guest compatibility.**

**Fact — how the list maps onto the type.** §27 lists the MMIO/PIO map *and*
display, input, storage, serial, network and audio as separate items. They are not
separate: a display is a device at an address. So `MachineProfile` has one
`devices` list, and `device_of` / `devices_of` are how a caller asks "where is the
display". A profile that also had a `display:` field could describe two different
machines, and the disagreement would not be a build error — it would be a machine
that boots and then draws nowhere.

| §27 says | `MachineProfile` has |
| --- | --- |
| architecture | `ProfileName::architecture()` |
| CPU | the architecture's feature set, carried by the name |
| RAM, firmware, boot behavior, MMIO/PIO map | `layout` (`LZA64_LAYOUT`) and `regions` |
| device inventory | `devices` |
| timer | `timer` |
| compatibility behavior | `compatibility` |
| display, input, storage, serial, network, audio | *derived* from `devices` by class |

**What a profile is not:**

- **not firmware.** It says *where* firmware lives and where the reset vector
  points. The bytes are an image, and the boot path supplies them. That split is
  what lets one machine run a different kernel without the machine changing.
- **not the operating system.** The LazOS kernel and user region layout stays in
  `lazalith-os`, layered over the machine, exactly as today.
  `lazalith-machine` must not depend on the OS, and a profile carrying the OS's
  region layout would be that dependency by another name.

---

## 2. Why the device set had to be fixed first

**Fact, before B4.** `DeviceManager<D>` was `Vec<Entry<D>>` with one concrete `D`:
no trait object, no device enum, no heterogeneous set. A machine could have a
console, *or* a timer, *or* a display, *or* an input device — never two of different
kinds at once. `BootImage::machine_setup` refused any non-empty device manager
outright.

So a profile describing a device *inventory* was a description no machine could be
built from, and B4's first half is what makes one possible.

**How it was fixed, and why it is additive.** `Box<dyn Device>` now implements
`Device`, forwarding all ten methods. `DeviceManager<Box<dyn Device>>` is a
`DeviceManager` of some `D: Device`, and every generic in `lazalith-memory` and
`lazalith-machine` was already written in terms of `D` — so
`LazalithMachine<Box<dyn Device>>` is a machine, from the same constructor, with
the same `map_device`, the same routing by `DeviceId` and the same per-device
snapshot contract.

**Not one existing call site changed.** `LazalithMachine<ConsoleDevice>` is still
monomorphic and still fast, and `NoDevice` still means something an empty erased
list does not: *a machine that cannot hold a device at all*, rather than *a machine
holding nothing*. Both distinctions are tested
(`a_phase_i_machine_cannot_hold_a_device_at_all`).

**The cost** is a dynamic call per register access, which is why a machine with one
kind of device should not do this. It is a choice a profile makes, not a default
that was imposed.

---

## 3. The versioned identity

**Fact.** `ProfileName { architecture, family, version }`, and `Display` produces
`lza64-native-v1`. The version is a *field*, not a string, so a profile cannot be
called `lza64-native-v1` and then describe an LZ32 machine — the kind of
disagreement otherwise found only by a guest.

Three things make "versioned to preserve guest compatibility" a real property
rather than a label:

| | |
| --- | --- |
| the version is in the name | a guest can say which machine it booted on |
| `version() == 0` is refused | a profile that could be unversioned could be changed incompatibly |
| `isa_version` is checked against `ISA_VERSION` | a profile for an ISA this build does not implement is refused, so an ISA version bump cannot leave existing profiles quietly describing a machine that no longer exists |

---

## 4. The three profiles

| Name | Status |
| --- | --- |
| `lza64-native-v1` | **built and tested.** Boot ROM, RAM, a console, a timer |
| `lza64-virt-v1` | not built. Needs a storage backend (B8) to be worth having |
| `lza64-at-v1` | nameable, and **refused** by `validate`. Needs the four compatibility devices in `docs/linux-0.01-port.md` §3.7 |

**`lza64-native-v1` is minimal on purpose.** §53 puts machine profiles before the
target-side OS, and a freestanding kernel (B25, B26) is built against a machine
with nothing on it but a CPU and memory. A native profile whose default shape
included a display would mean every kernel target had to opt out of a device it
never asked for, and opting out is how a guest quietly acquires a dependency on a
device that is not always there. `the_native_profile_carries_only_what_a_kernel_needs`
holds that.

**`lza64-at-v1` is refused rather than absent.** The name has to be writable down
and referable before the device set exists, and asking for a machine this build
cannot produce has to be a clear refusal rather than an `lza64-at-v1` that quietly
has no VGA in it.

---

## 5. The layout, and the duplication it removed

**Fact.** `MachineLayout` is the machine's physical geometry, and `LZA64_LAYOUT` is
its one definition. It lives in `lazalith-machine` because that is where a profile
is, and a profile that had to import its geometry from the boot crate would be a
layering inversion.

`lazalith-boot` and `lazalith-os` re-export from it. Two constants —
`KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP` — were **declared in both crates**
with the same values before B4. That is the duplication that mattered: a change to
one would have left one crate describing a kernel window of one size and the other
of another, and the symptom would be a kernel loaded somewhere it does not fit.

Three tests hold this, in `crates/lazalith-boot/tests/profile_layout.rs` — which is
the only crate that can see boot, os and machine at once:

- `the_phase_i_layout_is_unchanged` — the pre-B4 numbers, written down;
- `every_re_export_points_at_the_one_definition` — every re-export resolves to
  `LZA64_LAYOUT`;
- `the_layout_is_internally_consistent` — the numbers agree *with each other*: a
  header inside its ROM, a payload limit below the ROM size, a kernel window
  inside RAM, a stack pointer above the image and inside RAM.

And one architecture invariant, `the_machine_geometry_is_defined_once`, which
greps for a **literal** rather than for a name. That distinction matters: a rule
that greps for the identifier cannot tell a re-export from a redefinition, so it
would be a rule that cannot fail — which is worse than no rule, because it looks
like one. It was verified to fail when a second literal is reintroduced in
`lazalith-os`, and to pass again when it was removed.

---

## 6. What a profile refuses

Validated **before** a machine is built, so a bad profile is a refusal rather than
a half-built machine. Each variant names the part to change.

| Refused | Why |
| --- | --- |
| no version | a profile that could be unversioned could be changed incompatibly |
| a different ISA version | the compatibility claim, checked |
| a family that disagrees with its compatibility behaviour | a *native* profile claiming AT hardware is wrong about something more basic than its devices |
| AT compatibility hardware | it does not exist in this build |
| an empty region | there is no such thing |
| an unaddressable region or device window | the architecture cannot name it |
| two overlapping regions | a machine cannot map both |
| two devices with one id | one address cannot route to two devices |
| two overlapping device windows | the bus refuses, so the profile is what's wrong |
| an executable device window | `Bus::map_device` refuses these |
| a device class this build cannot construct | refused, **not skipped** — skipping builds a machine missing something the profile promised while the profile still validates |

**What is deliberately *not* refused: a region's permissions.** The memory model
builds what it is given; writable-and-executable RAM is constructible and the
Phase-I OS tests use it for code. A profile that refused it would be inventing a
policy the platform does not have, and would refuse to describe machines that
genuinely exist. (This was written, tested, found wrong by a test that needed
executable RAM for its code, and removed.)

**And after a machine exists**, `matches_profile` checks the machine is the one the
profile describes: architecture, reset vector, initial stack pointer, and the
device set in both directions. A profile that is accepted and a machine built from
it can still disagree — a device mapped where the profile did not say — and the
machine boots and then misbehaves. That is a round trip, not a construction.

---

## 7. What B4 is not, and what is next

**Not built by B4:** `lza64-virt-v1`, `lza64-at-v1`, storage, audio, networking,
hot-plug, a device *backend* layer, and any change to the `Device` trait itself.

**B4's two halves, and both are done:**
1. a heterogeneous device set — `impl Device for Box<dyn Device>`;
2. a profile that describes one, with a round-trip check.

**What the next stage was.** B5 is the device **frontend/backend** split
(`binstruction.md` §26), and the profile is what it attaches to: a profile says
which devices a machine has, and a backend says how each one reaches the host's
resources. `docs/device-model.md` §5 has the shape and the three rules it obeys.

---

## 7a. B5: a profile names the storage too

`DeviceClass::Block` was `is_constructible() == false` through B4, and a profile
naming one was refused. B5 built the backend layer, so a profile can now name a
block device — and a `DeviceProfile` gained one per-class fact alongside
`console_capacity`:

| Class | Field | Meaning |
| --- | --- | --- |
| `Block` | `block: Some(BlockStorage::Memory { bytes })` | a flat image of `bytes` |
| `Block` | `block: Some(BlockStorage::CopyOnWrite { base })` | a sparse overlay over a read-only `base` |

**The storage is a field on the device, not a second profile section.** A profile
that listed devices in one place and their storage in another could describe a
machine where a device had no storage, and the disagreement would not be a build
error — it would be a machine that boots and then reads a disk that is not there.

**Four refusals**, each because the alternative builds a machine that is not the one
described:

- a block device with `block: None` — a promise with nothing behind it;
- a disk of zero bytes;
- a disk that is not a whole number of 512-byte sectors, **refused rather than
  rounded**: rounding 513 bytes down to one sector would give a guest a capacity the
  profile never promised;
- `block: Some(..)` on a device that is not a `Block` — a timer with a disk is a
  profile that does not mean what it says.

**The backend is built and moved into the device**, so the machine holds the storage
once, behind the device. A machine that also held the backend beside the device
would be holding the disk twice, and the copy beside it would be the one that went
stale.

**An overlay presents its base's capacity.** A copy-on-write layer is not a way to
make a disk bigger, and a profile wanting a bigger disk names a bigger base. The
refusal that matters here is `BackendError::WritableBase`: an overlay over a
writable base is a stack of layers that happens not to write through today, and
nothing would notice until a future backend that did. `BlockStorage::CopyOnWrite`
builds its base read-only *by construction* rather than trusting a caller to.

---

## 8. Two open questions B4 deliberately did not answer

Both recorded rather than guessed, and neither is settled by B5:

- **PIO.** `binstruction.md` §25 lists "MMIO/PIO map" as a profile property, and
  LZA has no port I/O at all. `docs/linux-0.01-port.md` §3.2 makes this the single
  largest item in that port: eleven driver files use `in`/`out` today. Whether LZA
  gains a PIO space is an architectural decision, and it belongs with the device
  model rather than with a profile that would have to describe something that does
  not exist.

  B5 did **not** answer it, and deliberately: the block device's `BLOCK_REGISTER_DATA`
  is a *register* in an MMIO window, not a port in a PIO space. A future PIO space
  would be a separate address space on `Bus`, and reusing the word for a register
  would make the two indistinguishable in a profile.

- **A profile *file*.** A profile is a Rust value here. Whether there is an
  on-disk encoding — and therefore something a third party could ship — is B19's
  question, because a serialised profile is a management API's input and the
  management API does not exist yet.


---

## Related

- `docs/virtual-machine.md` — what a profile configures
- `docs/device-model.md` — the device contract a profile's `devices` list uses
- `docs/toolchain.md` — why the compiler does not need a profile, and the sysroot
- `docs/linux-0.01-port.md` — where `lza64-at-v1` comes from
- `docs/beyond-lazalith.md` — the roadmap status
