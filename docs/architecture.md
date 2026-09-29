# Lazalith architecture

This document is the **contract**: what the Lazalith architecture is, what the
boundaries between its parts are, and which of those boundaries the repository
enforces mechanically rather than merely promising.

`binstruction.md` is the specification of what the platform is intended to become.
This is the record of what it is. Where the two disagree, the repository is right
and this document says so.

Every claim below is one of five things, and the section it is in says which:

| | |
| --- | --- |
| **Fact** | verified in this repository, by a command or a test that names it |
| **Research** | an external documented fact, cited |
| **Requirement** | something `binstruction.md` obliges the project to |
| **Proposal** | a design that has not been built |
| **Deferred** | deliberately not started, with the reason |

---

## 1. The name: LZA

**Research / adopted as a requirement.** The architecture's formal name is
**LZA — the Lazalith Architecture**, in two widths:

```text
LZA32    the 32-bit variant
LZA64    the 64-bit variant, and the primary target identity
```

`binstruction.md` §7 fixes this name and asks for exactly one thing to be kept
straight, which is that there are two names in play and they mean different
things:

| | |
| --- | --- |
| **LZA64** | the formal architecture and target identity — a *name* |
| **LZ64** | the existing ISA and width terminology in this codebase |

`docs/lza64.md` is where the distinction is written out in full. The short version:
`LZ64` is a width, `LZA64` is an architecture. Saying "the LZ64 target" is
under-specified in a project that will eventually have more than one target, which
is why the architecture gets a name and the width keeps the term it has had since
Phase I.

**Fact.** No broad rename happened. `WordWidth::W64`, `ArchitectureConfig::lz64()`,
`BootArchitecture::Lz64` and `LZX`'s architecture tag are all still called what
they were called in Phase I, and `docs/lz64.md` says which is which. Renaming
25 crates and every format tag to say "LZA64" would have been a change with no
behaviour behind it, and `binstruction.md` §7 explicitly says not to.

**Proposal.** `lza64-unknown-lazos` as a Rust-style target triple. It is a
proposal. No triple exists, no tooling reads one, and nothing in this repository
consumes one.

---

## 2. What the platform is, in the order it depends on

**Fact.** The following layering is the real dependency order of the workspace,
read from the manifests. Each arrow is a crate that exists and is depended upon by
the one above it.

```text
lazalith-types            strong types; no dependencies of its own
    ↓
lazalith-diagnostics      diagnostics, spans, emulator-bug reports
    ↓
lazalith-isa              the instruction table, encode, decode
    ↓
lazalith-cpu              Processor (canonical state) + ReferenceInterpreter (engine)
    ↓
lazalith-devices          the guest-visible device contract
    ↓
lazalith-memory           address spaces, regions, faults, the bus, the decode cache
    ↓
lazalith-machine          the VM: processor + engine + bus + clock + lifecycle
    ↓
lazalith-os-abi           syscalls, records, capabilities
    ↓
lazalith-os               LazOS: processes, scheduler, syscalls, filesystem, drivers
    ↓
lazalith-boot             the boot ROM, the reset vector, the kernel load
    ↓
lazalith-ir               the low-level IR both compilers lower to
    ↓
lazalith-toolchain        assembler, object (.lzo), linker, executable (.lzx)
    ↓
lazalith-compiler         the Lazen front end
lazalith-c-compiler       the C front end
    ↓
lazalith-codegen          IR → LZA code, the shared back end
    ↓
lazalith-runtime          startup, the Lazen library, the run harness
lazalith-stdlib           the standard library, as Lazen source
lazalith-ui               the guest widget library, as Lazen source
    ↓
lazalith-debug            the debug API: controller, session, snapshot, replay
    ↓
lazalith-cli              `lazen`, the one binary
lazalith-fuzz             the deterministic fuzz harness

-- host only, and not in the guest's dependency graph at all --
lazalith-sdl3             the SDL3 FFI boundary
lazalith-gui              the host debugger window
```

**Fact.** `lazalith-sdl3` is the only crate in the workspace that contains
`unsafe`, and the workspace lint table sets `unsafe_code = "forbid"` everywhere
else. It is enabled for `lazalith-sdl3` by an explicit, reviewed override. That
makes "a CPU register cannot be written from a frontend" a property of the lint
configuration rather than a promise in a document.

**Fact.** `lazalith-stdlib` and `lazalith-ui` are Lazen *source text* held in Rust
crates. They have no library dependencies at all. A standard library that is only
type-checked is a standard library nobody has run, so their dev-dependencies are
the whole pipeline.

---

## 3. The boundaries, and which of them are enforced

A boundary nobody checks is a comment. These are the ones this repository checks,
and the check is named so you can run it yourself.

| Boundary | Enforced by | How |
| --- | --- | --- |
| The CPU, ISA, memory and machine do not depend on SDL3 | `the_cpu_does_not_depend_on_sdl3` | manifests |
| The machine does not depend on the compiler | `the_machine_does_not_depend_on_the_compiler` | manifests |
| The compiler does not depend on the emulator | `the_compiler_does_not_depend_on_the_emulator_implementation` | manifests |
| The GUI does not reach the CPU, and only the GUI and the SDL3 crate may name SDL3 | `the_gui_does_not_reach_into_the_cpu` | manifests |
| Lazen reaches the system through LazOS, not around it | `lazen_goes_through_lazos_rather_than_around_it` | builds a program |
| C reaches the system through the ABI, not around it | `c_goes_through_the_abi_rather_than_around_it` | builds a program |
| Assembly can still reach supervisor-only instructions | `assembly_can_still_reach_low_level_functionality` | assembles and validates |
| LZ32 and LZ64 share one architecture, not two forks | `lz32_and_lz64_share_one_architecture` | manifests and one encoding decoded twice |
| The ISA is defined once | `the_isa_is_defined_once` | one `src/` contains the opcode table |
| The ABI is defined once | `the_abi_is_defined_once` | one crate holds the version |
| The syscall table is defined once, with no two numbers equal | `the_syscall_table_is_defined_once` | source |
| Diagnostics are centralised | `diagnostics_are_centralised` | manifests |
| A guest fault and an emulator bug are different values | `guest_faults_and_emulator_bugs_are_distinguishable` | runs both |
| Only the debugger reaches the address-space mutators | `only_the_debugger_reaches_the_address_space_mutators` | source |
| **No execution engine owns the architectural state** | `no_execution_engine_owns_the_architectural_state` | source |
| **A machine holds its processor and its engine apart** | `a_machine_holds_the_processor_and_the_engine_apart` | source |

All sixteen are in `crates/lazalith-cli/tests/architecture.rs` and run under
`cargo test -p lazalith-cli --test architecture`. CI runs that suite as its own
job — see `docs/ci-cd.md`.

---

## 4. The VM core, and why it is shaped this way

### 4.1 The problem the extraction solved

**Fact, before B3.** `LazalithMachine` held a `ReferenceInterpreter` *by value*,
and the interpreter *owned* `ArchitecturalState`, `ExecutionState` and
`TrapController`. With one engine that is the simplest thing that could possibly
be written. With two, it is the wrong shape for a specific reason: a second engine
would have had to either reach inside the first — which is precisely the door
`lazalith-debug` is built not to have — or hold its own copy, and **a second copy
of a program's registers is a second architectural truth**.

`binstruction.md` §11 is explicit that a mode switch "MUST NOT reset, clone,
reinterpret, or silently alter guest state", and §12 that "JIT-private caches or
host register state must be synchronized or discarded before control returns to
the interpreter". A design in which the engine holds the state cannot satisfy that
by construction, because the engine and the state are the same object.

### 4.2 What the extraction did

**Fact.** `lazalith-cpu` now has two things where it had one:

```text
Processor          the canonical guest-visible state. Owned by the MACHINE.
                   Registers, pc, sp, status, trap frames, execution context.

ExecutionEngine<M> a trait. Implemented by ReferenceInterpreter today.
                   step(&mut Processor, &mut M) — borrows the state for one
                   instruction and gives it back.
```

`LazalithMachine` holds both, as two separate fields:

```rust
processor: Processor,                        // never replaced by a switch
engine: Box<dyn ExecutionEngine<Bus<D>>>,   // replaced by a switch
```

`LazalithMachine::switch_execution_engine(EngineKind)` is the one checked
operation that changes the second and not the first. What it guarantees, and what
`crates/lazalith-machine/tests/engine.rs` checks:

- the program counter, stack pointer, every register, the status register, the
  execution state, the machine's own lifecycle state, its virtual clock and its
  device state are all unchanged;
- a **live trap frame survives**, which is the case that matters most — a fault in
  compiled code has to be able to come back with the frame intact, or the program
  could never return from it;
- execution continues correctly, and the same program produces the same answer
  with and without a switch;
- the switch is **refused** during an active user execution context (the
  scheduler is between two steps and would not observe it) and on a faulted machine
  (a switch is not a reset);
- an **outside implementation of the trait can take over a running machine's
  processor and hand it back**, which is the property a JIT will need.

### 4.3 What this is not

**Fact.** There is **one** execution engine in this repository.
`EngineKind::ALL` has one entry. The switch replaces an engine with one that has
the same semantics and discards its private state, which today is nothing.

**Fact.** There is no JIT, no register allocator, no basic-block compiler and no
host code generation of any kind. `binstruction.md` §11 and the queued sessions
that follow make the JIT a later stage (B22–B24 in its roadmap).

**Requirement.** What B3 establishes is the *operation* and the property it has,
so that B22 is an engine behind a boundary that already exists rather than a
change to the VM that the engine has to be threaded through. The tests in
`engine.rs` are written against the requirement in `binstruction.md` and are
engine-agnostic: they do not care what is inside `step`.

### 4.4 The one asymmetry worth naming

`ExecutionEngine::step` takes `&mut Processor`, and the machine holds the engine
behind a trait object with a *concrete* memory type (`Bus<D>`). That means an
engine is monomorphic in its memory, which is a real constraint: a JIT will
generate code that talks to a `Bus`, not to an arbitrary `CpuMemory`. This is
recorded here rather than discovered later. It was the alternative of a
trait-object memory interface, which would have cost a dynamic dispatch on every
load and store in the reference interpreter — the one engine that must not be
slow, because it is the oracle the others are compared against.

---

## 5. Where the operating system boundary actually is today

**Fact.** LazOS is a Rust crate. `LazalithKernel` owns a `RoundRobinScheduler` and
a set of services, and `LazalithKernel::step` takes `&mut LazalithMachine<D>` and
advances it. The guest-visible "kernel" in the boot ROM is, in every production
path, a two-instruction stub:

```rust
NOP
RFE
```

`crates/lazalith-runtime/src/run.rs` builds exactly that and hands it to
`BootImage::new`. The scheduler and the syscall dispatcher are host Rust that
runs *outside* the machine, servicing traps the machine hands it.

**Fact.** The kernel uses a narrow slice of the machine's API. Of everything on
`LazalithMachine`, `lazalith-os` calls `step_managed`, `activate_user_context`,
`release_user_context`, `recover_user_context`, `invalidate_user_context`,
`return_from_syscall`, `abort_trap`, `with_user_context`, `with_trap_controller_mut`,
`trap_controller`, `architectural_state`, `memory`, `active_execution_context` and
`reset`. It never touches the bus, the devices, the clock or the interrupts.

**Requirement.** `binstruction.md` §20 requires that this become guest software
and §24 gives the staged migration. That is stages B25 and B26. The Phase-I path
stays runnable throughout, as §20 requires, and this repository's version of it is
the migration oracle.

**Consequence, stated here because it constrains everything upstream:** the
scheduler is *not* part of the VM contract. A guest-side LazOS will have its own
scheduler, written in C, running on the machine. The host Rust scheduler survives
only as a reference model, a test oracle and a bootstrap support. Anything designed
now to serve the host scheduler is designed against something that is going away.

---

## 6. The device model, as it is

**Fact.** The `Device` trait in `lazalith-devices` is the whole guest-facing
device contract: `address_len`, `reset`, `validate_read`, `validate_write`, `read`,
`write`, `peek`, `tick`, `snapshot`, `restore`. Two properties of it are load-bearing:

- **Validate before mutate.** Every write path validates and only then writes, so a
  refused access leaves every byte as it was. This is checked in
  `crates/lazalith-memory/tests/hardening_validate_before_mutation.rs` in twelve
  ways.
- **A device's snapshot is its own encoding.** A common encoding across devices
  would have to be a lowest common denominator that lost whatever made each device
  different, so each device encodes itself and `restore` refuses bytes that are not
  its own. A display's state cannot be restored into an input device.

**Fact, and a real first-order constraint.** `DeviceManager<D>` is *monomorphic*:
every entry in a machine's device manager is the same concrete type `D`. There is
no trait object, no device enum, no heterogeneous device set. A machine can hold a
console or a timer or a display or an input device, but not two of different kinds
at once.

**Research.** QEMU separates a device *front end* (how the guest sees it) from a
device *back end* (how the host's resources are used), and layers the two, with
back ends "sometimes stacked to implement features like snapshots"
(QEMU, *Device Emulation*). That is the shape `binstruction.md` §26 asks for, and
it is a genuine extension of what is here: the monomorphic manager is B5's
subject, not something this document pretends is solved.

**Fact.** The display and input devices are *not* machine devices in the boot
path. `DisplayService` and `InputService` own a `DisplayDevice` and an
`InputDevice` inside the kernel, and a guest reaches them only through
`display_open` / `display_present` / `input_poll` syscalls. There is no MMIO path
to a framebuffer, by design: a Lazen program has no way to obtain a device
address.

---

## 7. The toolchain, as it is

**Fact.** One object format and one executable format, and all three front ends
converge on them:

```text
C ──────┐
Lazen ──┼──→ Lazalith IR → LZA code → .lzo ──→ link ──→ .lzx ──→ LazOS
ASM ────┘
```

`crates/lazalith-c-compiler/tests/` contains the cross-frontend differential that
builds the same program through C and through Lazen and compares what each
prints. `crates/lazalith-cli/tests/architecture.rs` checks that `.lzo`, `.lzx` and
the syscall table each have exactly one definition.

**Fact.** The compiler, the assembler and the linker do not depend on the VM. This
is checked (`the_compiler_does_not_depend_on_the_emulator_implementation`), and it
is what makes a compiler usable without a machine.

**Fact.** There is one binary, `lazen`, with eight subcommands: `new`, `check`,
`build`, `run`, `test`, `pack`, `deps`, `fmt`. `binstruction.md` §18 proposes
splitting this into `lazcc` / `lazas` / `lazld` / `lazdbg` / `lazpkg` / `lazimg`
/ `lazen`. That split is B14 and has not happened.

**Fact.** There is no sysroot. `binstruction.md` §19 describes
`sysroot/{include,lib,crt,runtime}`; none of those directories exist. The Lazen
standard library and the C runtime prelude are composed by
`lazalith-runtime` in memory, not read from a sysroot tree. That is B15.

---

## 8. The host boundary, and the SDL3 question

**Fact.** SDL3 is host-only. `lazalith-sdl3` is a single 906-line file: 18 `extern
"C"` declarations and the safe functions around them, 32 `unsafe` blocks, 1
`unsafe fn`. `lazalith-gui` is the only crate that depends on it.

**Fact, and a correction to a claim in the code.** `lazalith-sdl3`'s own module
documentation says the event buffer size "is checked by a test, which compares it
against the headers' declared structs". There is no such test, and there is no
`tests/` directory in that crate. What exists is a C static assert in the build
script plus eight compile-time `const` assertions in Rust, both against a probe
that is compiled and *run* against the real SDL3 headers. Both are compile-time
checks; neither is a test. This is recorded rather than fixed here, because
correcting the comment is a one-line change to a file this pass did not otherwise
touch.

**Fact.** The probe measures `sizeof(SDL_Event)`, `sizeof(SDL_KeyboardEvent)`,
three `offsetof` values, `sizeof(bool)`, `sizeof(SDL_Keycode)`,
`sizeof(SDL_Keymod)` and eight scancodes. It does **not** measure the event-type
discriminants, `SDL_Rect`, `SDL_FPoint`, the pixel format, the scale mode or the
access mode — those are hardcoded constants in Rust. A header change that moved
`SDL_EVENT_KEY_DOWN` would not be caught by the probe.

**Research.** The Rust `sdl3` crate exists and is the direction `binstruction.md`
§36 names. Its own crate page says: *"Now that the SDL3 API is mostly stabilized,
we are working on a new version of the Rust bindings for SDL3... Expect some bugs
and missing features."* Latest published version at the time of writing is
0.18.4, over `sdl3-sys 0.6.0+SDL-3.4.0`.

**Proposal, not adopted.** Adopting `sdl3` would be the first third-party
dependency in a workspace that currently has exactly one (`pkg-config`, a build
dependency), and it would import a crate its authors describe as a work in
progress, in exchange for removing 32 `unsafe` blocks from a file that already has
a C probe proving its layout assumptions. B20 is where that decision belongs, and
it should be decided by measurement rather than by this paragraph.

---

## 9. What the interpreter is authoritative for

**Fact.** `ReferenceInterpreter` is the semantic authority and is checked against
nothing less, because there is nothing else yet. `crates/lazalith-machine/tests/differential.rs`
says so in its own module comment: "There is no optimised emulator: step 93
introduces one, and until then this repository has exactly one instruction
executor."

**Fact.** What the existing differential does compare is the reference interpreter
against a full machine — bare `Vec<u8>` memory against a real `AddressSpace`, a
`Bus`, region permissions, a `VirtualClock` and a mapped console — over a curated
corpus and a 48-seed random corpus, on both widths, comparing after *every* step:
registers, pc, sp, flags, halt/trap, fault classification, virtual time, and
memory windows at two addresses.

**Requirement.** `binstruction.md` §12: "Do not modify the Reference Interpreter
to make JIT output match. A semantic mismatch is a correctness defect." When a
second engine exists, this file is where it will be compared, and the reference
will not be touched.

---

## External design references

- QEMU, system emulation introduction — machine, CPU, accelerator, device, backend
  and interface as separate concerns, and TCG as a JIT.
  <https://www.qemu.org/docs/master/system/introduction.html>
- QEMU, device emulation — device front end, device bus, device back end, device
  pass-through, and back ends stacked for snapshots.
  <https://www.qemu.org/docs/master/system/device-emulation.html>
- GCC, overall options — the compilation stages a driver coordinates.
  <https://gcc.gnu.org/onlinedocs/gcc/Overall-Options.html>
- GCC, invoking GCC — driver behaviour.
  <https://gcc.gnu.org/onlinedocs/gcc/Invoking-GCC.html>
- Rust `sdl3` crate — <https://docs.rs/crate/sdl3/latest>
- GitHub Actions — <https://docs.github.com/en/actions>
- Linux 0.01, the authoritative source tree for the eventual port —
  <https://github.com/zavg/linux-0.01.git>, pinned to
  `5839d67d5825265fc665c9dc0ec2e767ff47a6dd` (see `docs/linux-0.01-port.md`).

## Related documents

| | |
| --- | --- |
| `docs/beyond-lazalith.md` | the master Beyond document, and where the roadmap stands |
| `docs/lza64.md` | LZA32/LZA64 against the LZ32/LZ64 terminology |
| `docs/virtual-machine.md` | the VM contract, the engine model, and the JIT's requirements |
| `docs/device-model.md` | the device contract, and the frontend/backend split |
| `docs/machine-profiles.md` | versioned machine profiles |
| `docs/toolchain.md` | the toolchain split, the sysroot, and the packaging model |
| `docs/compatibility.md` | the compatibility tier, and what it is for |
| `docs/linux-0.01-port.md` | the pinned source and the port's shape |
| `docs/ci-cd.md` | the GitHub automation, and what it does not check |
| `docs/project-state.md` | the running record of what has been built and measured |
