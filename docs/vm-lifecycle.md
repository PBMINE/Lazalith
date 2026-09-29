# The VM lifecycle

`binstruction.md` §6, §9, §34. Implemented in `crates/lazalith-vm` at B6.

## What the lifecycle is for

`lazalith-machine` owns a machine: a processor, a bus, a clock, a device set, and
whether it will execute (`MachineState`). It does not own the questions of *whether
firmware has run* or *what a reset undoes* — those are lifecycle questions, and before
B6 they were answered in three different crates.

`lazalith-vm` owns them, in one type:

```text
Vm<D>
 ├── LazalithMachine<D>   §9: the architectural machine
 ├── BootStage            has firmware run?
 ├── MachineProfile       what the machine is, to re-check against
 ├── VmSnapshot           §40: the guest's state
 └── BootHandoff          where the last boot stopped
```

## The two stages, and why they are not `MachineState`

`MachineState` (`Created`, `Reset`, `Running`, `Paused`, `Halted`, `Faulted`) answers
*will this machine execute*. `BootStage` (`Cold`, `Booted`) answers *has firmware run*.

A machine reset and never booted, and a machine whose bootloader finished, are **both**
`MachineState::Reset`. They are not the same machine, and the difference is observable
— one has a kernel loaded and one has an empty ROM. So it is recorded somewhere.

They are orthogonal, both owned by `Vm`, and `MachineState` was not modified to make
room. A second enum that also answered "where is this machine" would be how a VM ends
up with two answers that disagree, which is the thing B6 exists to prevent.

## The boot contract

§34's chain:

```text
VM Manager        ← B19
 ↓
Machine Profile   ← what the machine is
 ↓
Firmware          ← a ROM, supplied as bytes
 ↓
Bootloader
 ↓
LazOS             ← where execution ends up
```

**Each layer is the authority for what it knows, and the overlap is checked.**

| Fact | Authority | Why |
| --- | --- | --- |
| memory | boot image | a kernel image, a kernel stack and a user region are the OS's business, and their permissions are not a profile's flat RAM |
| devices, timer, geometry, interrupt model | profile | a boot image does not know what devices a machine has, and refuses a device manager |

Four fields are described by both and must agree: `physical_ram_start`,
`reset_vector`, `kernel_load_address`, `kernel_initial_sp`. A disagreement is a
**refusal naming the field**, checked before any machine is built.

This check did not exist before B6. A profile that moved the kernel load address and
an image built for this build's layout were both accepted, and the failure appeared as
a fault at the first instruction of a kernel loaded somewhere else.

### `boot` rebuilds the machine

Not a convenience — the contract. A profile's ROM window is mapped **read-only**
because a guest must not be able to write its own firmware, and that same fact means
firmware cannot be installed into it afterwards. So a machine that both boots and has
devices is assembled from the image's memory and the profile's devices, rather than by
patching a ROM into a described machine.

Nothing is carried across the rebuild. A boot is a cold start.

## Reset

`Vm::reset` always returns to `BootStage::Cold`. A machine that had booted is not a
booted machine after a reset: booting ran firmware and loaded a kernel, and neither is
undone by returning the processor to its reset vector. The RAM still holds the old
kernel and the ROM still holds the bootloader, exactly as a real machine's would. What
is gone is the *claim* that the handoff happened.

A caller that resets a booted machine and then runs gets a machine executing its
bootloader from its entry — which is a real and useful thing to do, and one a
bootloader test needs. `stage` says `Cold` throughout.

## Snapshot and restore

`Vm::snapshot` captures the whole `Processor` (architectural registers *and* execution
and trap state), the virtual clock, and each device's own `Device::snapshot`. It does
**not** capture the host's storage — that is B5's rule, unchanged, and a block device
refuses a snapshot taken against different storage.

`Vm::restore` rewinds. Getting that right was the hard part: the first version advanced
the clock and refused a snapshot taken *earlier* than the machine had reached, which
made every snapshot useless. The fix was in the machine — `advance_clock` only moves
forward, while devices had always been able to rewind, because each one's `restore`
writes its own elapsed count back. B6 added `LazalithMachine::restore_time`, which sets
the processor's and the devices' clocks together.

Two refusals remain, both about what a restore cannot conjure:

- **a snapshot from another stage** — a machine cannot become booted by being
  restored, because booting ran firmware;
- **a different device count** — device snapshots are restored by position, so
  applying a two-device snapshot to a one-device machine would put the second device's
  state into the first.

## What this is not

- **Not firmware.** §34 forbids implementing it in this pass.
- **Not the VM manager.** B19. A manager creates, configures, persists and runs
  machines for a human.
- **Not PIO.** B4's question, still open. A future PIO space would be a separate
  address space on `Bus`, and reusing the word "port" for a register would make the
  two indistinguishable in a profile.
- **Not a replacement for `BootImage::start`.** Phase-I's path still builds and boots;
  `Vm::from_machine` adopts its result.

## The naming

§9 proposes **LVMI** (Lazalith Virtual Machine Interface) and says not to finalize it
without research. B6 does not claim it. The crate is `lazalith-vm` because that is
what it is, and the interface question is a separate decision that should not be
settled by a crate name chosen while writing code.

## Related

- `docs/device-model.md` §5 — the device/backend boundary this lifecycle builds on
- `docs/machine-profiles.md` — what a profile describes
- `docs/virtual-machine.md` — the machine this lifecycle owns
