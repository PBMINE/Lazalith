# Beyond Lazalith

The master document for the phase after the Phase-I freeze.

`binstruction.md` is the specification: what the platform is intended to become and
the B1–B29 roadmap it is intended to become it by. This document is the record of
**where that actually stands**, what was established in the first Beyond pass, and
what a queued session should do next.

The two roles are kept separate on purpose. `binstruction.md` describes what the
project is supposed to be. `docs/` describes what it actually became, why, and how
it works. A proposal is never described here as implemented.

---

## 1. The goal, in one paragraph

`binstruction.md` §6: make Lazalith look and behave like a real computer
architecture ecosystem, a real compiler/toolchain ecosystem, and a real VM
platform, rather than one giant integrated implementation. Conceptually closer to
GCC + binutils, QEMU, VirtualBox, and a real target operating system — while
remaining unmistakably Lazalith.

The stable identity is fixed by §8 and is not negotiable:

```text
LZA architecture
  ↓
Lazalith instruction semantics
  ↓
Lazalith ABI
  ↓
Lazalith object/executable formats
  ↓
Lazalith VM contract
  ↓
Lazalith device contracts
```

Host implementations may change. Guest-visible semantics do not.

---

## 2. Where the roadmap stands

`binstruction.md` §53 defines B1–B29. Status as of this pass:

| | Stage | Status |
| --- | --- | --- |
| B1 | LZA architecture naming / contract freeze | **done** — `docs/architecture.md`, `docs/lza64.md`, `docs/virtual-machine.md` |
| B2 | GitHub CI/CD and repository automation foundation | **done** — `.github/`, `docs/ci-cd.md` |
| B3 | VM core / execution-engine boundary extraction | **done** — `Processor` + `ExecutionEngine`, tested |
| B4 | machine profiles | designed, not built — `docs/machine-profiles.md` |
| B5 | device frontend/backend model separation | designed, not built — `docs/device-model.md` |
| B6 | common VM lifecycle / reset / boot contracts | **partly present**; see §4 |
| B7 | native display architecture | **present**; §4 |
| B8 | storage architecture | not started |
| B9 | input architecture | **present**; §4 |
| B10 | audio architecture | not started |
| B11 | networking architecture | not started |
| B12 | expansion bus / device discovery | not started |
| B13 | firmware / boot profiles | **partly present**; §4 |
| B14 | GCC-like toolchain separation | designed, not built — `docs/toolchain.md` |
| B15 | sysroot / runtime / packaging separation | designed, not built — `docs/toolchain.md` |
| B16 | native Lazen language/runtime/system boundary | **partly present**; §4 |
| B17 | debugger integration through the VM API | **present**; §4 |
| B18 | advanced snapshot / replay state model | **partly present**; §4 |
| B19 | VM manager / management API | not started |
| B20 | VM GUI / Rust SDL3 migration boundary | researched, not decided — `docs/architecture.md` §8 |
| B21 | optimized interpreter / execution-engine abstractions | **B3 lays the groundwork**; not started |
| B22 | JIT execution engine | not started |
| B23 | interpreter ↔ JIT interoperability and state handoff | **the operation exists and is tested; no JIT** |
| B24 | differential JIT verification | harness exists; nothing to run against |
| B25 | target-side LazOS transition and freestanding C | not started |
| B26 | LazOS built and booted by the Lazalith C + ASM toolchain | not started |
| B27 | LZA AT compatibility machine | not started |
| B28 | Linux 0.01 architecture port | not started — source pinned, `docs/linux-0.01-port.md` |
| B29 | Linux 0.01 boot milestone | not started |

"Present" in this table means *the Phase-I implementation already satisfies the
stage's intent*, not that the stage was executed. §4 says which and how.

---

## 3. What this pass actually changed

Three stages, and what is real about each.

### B1 — the contract is frozen

**New documents**, each grounded in the repository rather than in the
specification's descriptions of it:

| | |
| --- | --- |
| `docs/architecture.md` | the platform contract, the real dependency order, and the sixteen boundaries with the test that enforces each |
| `docs/lza64.md` | LZA32/LZA64 against the LZ32/LZ64 terminology, and why no rename happened |
| `docs/virtual-machine.md` | the VM contract, the engine model, the switch guarantees, and what a JIT has to satisfy |
| `docs/device-model.md`, `docs/machine-profiles.md`, `docs/toolchain.md`, `docs/compatibility.md`, `docs/linux-0.01-port.md` | the design for the stages that have not run yet |
| `docs/ci-cd.md` | the automation, what it does not check, and the measurements that were and were not taken |

**A decision was reached rather than deferred.** `binstruction.md` §9 offers
**LVMI** as a candidate name and says not to finalize it until repository research
confirms it is suitable. Research did not confirm it, and the reason is recorded in
`docs/virtual-machine.md`: there is no interface in this repository to name. There
is one concrete type, `LazalithMachine<D>`, which is not a trait and could not
become one without splitting the machine. Naming it after an interface it does not
implement would invite callers to write code that cannot compile. The name stays
open for B19, where there is a real management API to name.

### B2 — GitHub CI/CD exists

There was no `.github/` directory at all. Now there is a tiered workflow set, issue
and pull-request templates, `CODEOWNERS` and a Dependabot configuration. Every
command in every job was run locally before the file was written; every workflow
was linted with `actionlint`, which found a real defect — `concurrency` written
inside the `on:` block, where GitHub would have ignored it.

`docs/ci-cd.md` is explicit about what is *not* established: no workflow has run
on GitHub, no release tag has ever been cut, no branch protection or required
status check is claimed, and the cold `nix flake check` cost is not yet measured.

### B3 — the execution-engine boundary

This is the stage with code behind it, and it is the one that matters most, because
`binstruction.md` §11 makes interpreter↔JIT interoperability mandatory and the
repository could not express it.

**Before:** `LazalithMachine` held a `ReferenceInterpreter` by value, and the
interpreter *owned* the architectural state. A second engine would have had to
reach inside the first or hold a copy, and a copy is a second architectural truth —
which §11 forbids.

**After:** `Processor` holds the canonical state and the *machine* owns it.
`ExecutionEngine<M>` is a trait whose `step` borrows a `&mut Processor` for one
instruction. `LazalithMachine` holds the two as separate fields and
`switch_execution_engine` is the one checked operation that replaces the second.

**What is true today:** the switch preserves everything the guest can see, survives
a live trap frame, is refused in the two cases where it should be, and works
across every instruction of a program. Fourteen tests in
`crates/lazalith-machine/tests/engine.rs`, plus two new architecture invariants.

**What is not true today:** there is one engine. `EngineKind::ALL` has one entry.
The switch currently replaces an engine with one that has identical semantics and
discards its private state, which is nothing. There is no JIT, and
`docs/virtual-machine.md` §4 lists the six things a JIT must satisfy before it is
one.

---

## 4. What Phase I already satisfies

Several roadmap stages ask for something that exists and works, arrived at from a
different direction. Saying so is not claiming credit for the stage; it is saving
the next session from rebuilding it.

| Stage | What already exists | Evidence |
| --- | --- | --- |
| B6 lifecycle / reset / boot | `MachineState` with six states, invalid transitions refused; `reset`; `BootImage` with a real boot ROM, a reset vector, a kernel load and a register-for-register handoff check | `crates/lazalith-machine`, `crates/lazalith-boot/tests/boot.rs` |
| B7 native display | `DisplayDevice` with guest-owned framebuffers, presented frames, and a `Lazen SDK → LazOS → device` path with no SDL3 anywhere in it | `docs/lazen-graphics.md`, `crates/lazalith-runtime/tests/window.rs` |
| B9 input | `InputDevice` with a guest-drained event queue, delivery counts, and a driver above it | `docs/lazen-input.md`, `crates/lazalith-devices/tests/input.rs` |
| B13 firmware / boot | a real boot ROM the CPU fetches and executes; the bootloader is LZA instructions the assembler emits, not host Rust | `crates/lazalith-boot/src/bootloader.rs` |
| B16 Lazen boundary | the Lazen standard library is Lazen source; its system calls go through the OS ABI and the table has one definition | `the_syscall_table_is_defined_once`, `docs/lazen-design.md` |
| B17 debugger through the VM API | `DebugController` / `DebugSession` with **no `&mut LazalithMachine` anywhere in the public surface**; registers come back as owned snapshots | `the_gui_does_not_reach_into_the_cpu`, `docs/lazen-debug.md` |
| B18 snapshots / replay | `MachineSnapshot` with CPU, device and process state; `ReplaySession` over guest-visible state only | `crates/lazalith-debug/tests/snapshot.rs`, `replay.rs` |

**One architectural consequence, stated because it constrains the OS work.** The
guest-visible kernel in the boot ROM is a two-instruction stub — `NOP; RFE` — and
the scheduler and syscall dispatcher are host Rust running outside the machine.
The kernel uses a narrow slice of the machine's API and never touches the bus, the
devices, the clock or the interrupts.

**That means the host scheduler is not part of the VM contract.** B25 will replace
it with a guest-side one written in C. Anything designed now to serve the host
scheduler is designed against something that is going away.

---

## 5. Dependency order, and why the next stage is what it is

`binstruction.md` §53 fixes the chain:

```text
architecture → automation → VM abstraction → machine/device foundations
  → host-facing platform services → toolchain/runtime infrastructure
  → native Lazen system/runtime → debugger/snapshot infrastructure
  → optimized execution → JIT → interpreter/JIT interoperability
  → target-side OS → compatibility machine → Linux 0.01 port
```

B1, B2 and B3 are the first three links and are now done. The next link is
**B4, machine profiles**, and the reason is dependency order rather than appeal:

- B5 (device frontend/backend) needs a thing to attach backends to. A machine
  profile is the description of *what is attached* — architecture, CPU, RAM,
  firmware, interrupt model, timers, device inventory, MMIO map. Building B5 first
  means inventing a shape for that description twice.
- B8/B10/B11 (storage, audio, networking) each need somewhere to say "this
  machine has a disk and this is its backend". That is the profile.
- B13 (firmware/boot profiles) is *defined* by the profile, not alongside it.
- B19 (the VM manager) exists to create, configure and run profiles. A management
  API with nothing to manage is a wrapper over `LazalithMachine::new`.
- B27 (`lza64-at-v1`) is a profile plus a compatibility device set, and it is
  three stages away from the Linux port that consumes it.

So B4 is next, and it is small: a versioned, serialisable description of a machine,
and a check that a machine built from a profile matches its description.

**B2 changed the cost of everything that follows.** A new subsystem now has to add
its tests to a job rather than to a developer's memory, so the next session should
add whatever the new stage needs to `.github/workflows/` in the same commit that
adds the code — not afterwards.

---

## 6. Rules that survive into every later session

From `binstruction.md` §8, §11, §12, §56, §60, restated because they are the ones
that are easiest to break by accident:

1. **Never describe a proposal as implemented.** No JIT, no target-side C LazOS,
   no VGA, no Linux 0.01, no VM manager exists in this repository. If a document
   needs to say one of those things is coming, it says it is coming.
2. **Never describe QEMU's or VirtualBox's behaviour as a Lazalith requirement.**
   External research is a reference; adopting it needs a reason.
3. **The Reference Interpreter is not to be weakened to make a JIT match.** A
   semantic mismatch is a defect in the new engine.
4. **An engine may never hold architectural state.** It is checked mechanically by
   `no_execution_engine_owns_the_architectural_state`, and it is the single
   invariant that makes interpreter↔JIT switching safe rather than merely
   possible.
5. **A mode switch is not a machine reset.** PC, SP, registers, status, memory,
   interrupt state, device-visible state, virtual clock, debug state and the
   machine's own lifecycle all survive it.
6. **Performance problems are not solved by raising instruction budgets or
   timeouts.** The measurements behind that rule are in `docs/lazen-graphics.md`.
7. **Host technologies stay behind Lazalith abstractions.** SDL3, and later host
   audio, host networking, host USB, windowing frameworks.
8. **CI validates the same contracts the developer validates locally**, and is
   extended in the same commit as the subsystem it covers.

---

## External design references

Full citations are in `docs/architecture.md`. The ones that shaped a decision here:

- QEMU system emulation — machine / CPU / accelerator / device / backend / boot as
  separate concerns, and TCG as the JIT.
  <https://www.qemu.org/docs/master/system/introduction.html>
- QEMU device emulation — front end, bus, back end, pass-through; back ends stacked
  for snapshots. <https://www.qemu.org/docs/master/system/device-emulation.html>
- GCC compilation stages and driver behaviour.
  <https://gcc.gnu.org/onlinedocs/gcc/Overall-Options.html>
- Rust `sdl3` — the crate's own page says the bindings are still in progress and to
  expect missing features. <https://docs.rs/crate/sdl3/latest>
- GitHub Actions permissions — any permission not named is `none`.
  <https://docs.github.com/en/actions/using-workflows/workflow-syntax-for-github-actions>
- Linux 0.01, pinned to `5839d67d5825265fc665c9dc0ec2e767ff47a6dd`.
  <https://github.com/zavg/linux-0.01>

---

## Related

| | |
| --- | --- |
| `docs/architecture.md` | the platform contract |
| `docs/virtual-machine.md` | the VM contract and the JIT's requirements |
| `docs/ci-cd.md` | the automation |
| `docs/project-state.md` | the running record of what has been built and measured |
| `docs/machine-profiles.md` | the next stage |
| `binstruction.md` | the specification this document reports on |
