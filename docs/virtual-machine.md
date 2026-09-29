# The Lazalith virtual machine

This document states the VM contract: what the VM core is, what an execution
engine is, what a switch between them guarantees, and what a future JIT has to
satisfy before it can be called one.

`binstruction.md` §9 names a candidate for the abstraction — **LVMI, the
Lazalith Virtual Machine Interface** — and says explicitly: *do not finalize the
name until repository research confirms that it is suitable.*

**Assessment, after reading the repository: the name is not adopted, and here is
why.** LVMI would be a name for a *trait* or a *specification document*; what
exists in the repository is neither. There is one concrete type,
`LazalithMachine<D>`, which owns the bus, the devices, the clock, the interrupts,
the processor and the engine. Naming that type after an interface it does not
implement would be worse than not naming it: it would invite a caller to write
`impl LVMI for MyMachine` against a type that is not a trait and cannot become one
without splitting the machine in half.

So this document says **the VM contract** rather than **LVMI**, and records the
name as an open question for the stage that actually has an interface to name —
which is B19, the VM manager, where the thing to name is a management API and not
a machine.

---

## 1. What the VM core is

**Fact.** `lazalith-machine` depends on `lazalith-cpu`, `lazalith-memory`,
`lazalith-devices`, `lazalith-isa`, `lazalith-types` and `lazalith-diagnostics`. It
depends on no SDL3, no compiler, no toolchain, no host API, and it is `#![no_std]`
with `alloc`.

**Fact.** The `Device` trait is the only host-shaped thing anywhere below it, and
it is a trait over guest-visible registers: `read`, `write`, `peek`, `tick`,
`snapshot`, `restore`. A device has no window, no file handle, no socket and no
host clock. That is what lets the emulator run headlessly, which the entire test
suite depends on.

**Requirement** (`binstruction.md` §9): the VM core must not depend on SDL3, a GUI
toolkit, a host audio API, a host filesystem API, a host networking API or a
windowing framework. **Satisfied**, and checked by
`the_cpu_does_not_depend_on_sdl3` in the architecture suite.

### What the VM core owns

```text
LazalithMachine<D>
    processor   Processor            the canonical guest-visible CPU state
    engine      Box<dyn ExecutionEngine<Bus<D>>>   how that state is advanced
    bus         Bus<D>              address spaces, regions, device window, cache
    clock       VirtualClock        virtual time
    interrupts  InterruptController pending external interrupts
    state       MachineState        Created / Reset / Running / Paused / Halted / Faulted
    executed    u64                 the instruction count, host bookkeeping
```

`executed` and the machine's own state are **not** guest-visible. A snapshot does
not carry them (`docs/hardening.md` records that decision and its reasoning), and a
guest cannot read them. The clock is the same: `the_clock_moves_only_when_the_driver_moves_it`
in the differential suite holds that a step never advances virtual time, because
time is the driver's, not the CPU's.

---

## 2. The execution engine

**Fact.** An engine turns a `Processor` and memory into a *different* `Processor`
and the same memory, one guest instruction at a time.

```rust
pub trait ExecutionEngine<M: CpuMemory> {
    fn kind(&self) -> EngineKind;
    fn step(&mut self, processor: &mut Processor, memory: &mut M)
        -> Result<OutcomeApplication, CpuFault<M::Error>>;
    fn step_bytes(&mut self, processor: &mut Processor, bytes: &[u8], memory: &mut M) -> ...;
    fn execute(&mut self, processor: &mut Processor, instruction: &Instruction, memory: &mut M) -> ...;
    fn discard_private_state(&mut self) {}
}
```

**The rule that makes switching safe** is one line:

> An engine may hold private state. An engine may never hold architectural state.

**Fact, and why it is checked.** `Processor` is owned by the machine, not by an
engine. `no_execution_engine_owns_the_architectural_state` in
`crates/lazalith-cli/tests/architecture.rs` reads the sources and asserts that the
only types holding an `ArchitecturalState` are `Processor` itself and the
debugger's `CpuSnapshot`. The failure mode this prevents is invisible: an engine
that stores a `Processor`, adopted by a machine, compiles, passes every test in a
suite that only ever runs one engine, and is the second architectural truth the
whole design exists to prevent.

**Fact.** `ReferenceInterpreter` is a zero-sized type today. It holds no state
because it cannot. Its `execute` builds a candidate `ArchitecturalState`, mutates
the candidate, and only then calls `processor.commit(candidate)` — validate,
calculate, commit — and the same applies to the execution state through
`processor.set_execution`.

**Reference Interpreter, defined.** It is the executable statement of what the LZA
ISA means. It is not an optimisation and not a second implementation written for
speed. It is the definition, and every other engine is checked against it.

---

## 3. Switching

`binstruction.md` §11 makes this mandatory and lists the cases that must work
eventually. Here is where each one stands.

```text
Interpreter ──▶ JIT     hot code identified
JIT ──▶ Interpreter     breakpoint, fault, debug event, unsupported block
```

Both directions are the same operation, because both are the same fact: the
machine has one canonical architectural state, and the engine is how that state
is being advanced.

### What a switch guarantees

| Preserved | Checked by |
| --- | --- |
| program counter, stack pointer, every register, status/flags | `a_switch_preserves_everything_the_guest_can_see` |
| execution state (running / halted) | same |
| the machine's own lifecycle state | same |
| virtual clock | same |
| device state | same |
| a **live trap frame**, and the resume point of the last trap | `a_switch_does_not_disturb_a_live_trap_frame` |
| correctness of the rest of the program | `a_program_runs_correctly_across_a_switch`, `switching_between_every_instruction_changes_nothing` |
| that a reset still resets | `a_reset_after_a_switch_restores_the_initial_state` |

**Dropped:** the outgoing engine's private state, through
`discard_private_state`. A translation cache describes compiled code, not a
program, and a guest must not be able to tell it was dropped.

**Refused, with the reason named:**

| Refused when | Why |
| --- | --- |
| a user execution context is active | the scheduler is between two steps of a guest process and would not observe the change |
| the machine is `Faulted` | a terminal machine is over, and reviving it through an engine switch is a reset wearing a disguise |

**Allowed, and this is the case that matters most:** while a trap frame is active.
A fault in compiled code has to be able to come back with the frame intact, or the
program could never return from it. A design that refused a switch in a trap would
make the JIT-to-interpreter direction unusable exactly when it is needed.

### Where each `binstruction.md` §11 case stands

| Required case | Status |
| --- | --- |
| interpreter → JIT handoff | **operation exists and is tested; no JIT exists** |
| JIT → interpreter handoff | same |
| breakpoint during JIT execution | the *switch* is tested with a live trap frame; breakpoints are `lazalith-debug`'s and unchanged |
| guest fault/trap during JIT execution | same, tested |
| debugger single-step from JIT into interpreter | a single step across a switch is `a_program_runs_correctly_across_a_switch` at limit 1 |
| snapshot while JIT mode is active | snapshots do not record the engine, so this is vacuously satisfied; the debugger is unchanged |
| restore followed by interpreter execution | `crates/lazalith-debug/tests/snapshot.rs`, unchanged by the extraction |
| restore followed by JIT execution | depends on a JIT |
| deterministic replay across engine switches | `lazalith-debug`'s replay records guest-visible state only; it is engine-agnostic by construction |
| device/interrupt events that cause an engine switch | not implemented; the switch is a caller-driven operation, not an event |

**Honest summary:** every case that can be exercised with one engine is exercised
and green. Every case that needs a second engine is unexercised, and this document
does not pretend otherwise.

---

## 4. What a JIT has to satisfy

**Requirement** (`binstruction.md` §11, §12, §13). These are the properties, and
none of them is currently implemented:

1. **Yield at precise guest instruction boundaries.** A JIT block must be able to
   stop *before* an instruction whose effect has not been committed, so the
   interpreter resumes at an exact architectural state. The reference engine's
   validate/calculate/commit shape is the model: a partially-executed instruction
   is not a state, it is a bug.
2. **Hand the whole `Processor` back on exit.** Host registers, a translation
   cache and a block cache are the JIT's own. `pc`, the registers, the status
   register, the trap frame stack and the execution context are the machine's and
   must be written back before control returns.
3. **Reconstruct architectural state from its own copies on entry.** A JIT keeps
   guest registers in host registers for speed. It must be able to spill them into
   the `Processor` and reload them, and the reload must go through the same
   validation, or a JIT-compiled program and an interpreted one would disagree
   about what a bad program counter is.
4. **Enter traps the same way.** `Processor::enter_fault`, `enter_syscall`,
   `enter_software` and `enter_external` are *processor* methods, not engine
   methods, precisely so a JIT cannot push its own frame. A JIT that built a trap
   frame itself would leave a program the interpreter could not describe.
5. **Be differentially tested against the reference.** `binstruction.md` §12 is
   explicit: *do not modify the Reference Interpreter to make JIT output match; a
   semantic mismatch is a correctness defect.* The harness already exists —
   `crates/lazalith-machine/tests/differential.rs` — and it compares after every
   step, on both widths, over a curated corpus and a random one. A JIT becomes
   another column in that table.
6. **Not solve performance by raising budgets.** `binstruction.md` §13: the
   hardening phase already showed that frame-heavy generated code is extremely
   instruction-expensive, and that the fix is a register allocator or a bulk
   memory operation, not a bigger limit. The measurements are in
   `docs/lazen-graphics.md` and are not to be worked around.

**Not implemented here.** There is no JIT, no register allocator, no basic-block
compiler, no host code generation, and no measured speedup to report.

---

## 5. What the VM deliberately does not do

Stated so a later stage does not re-litigate it:

- **No multiple processors, no SMP, no CPU topology.** The VM owns one
  `Processor`. `binstruction.md` §25 lists CPU topology under the architectural
  core, but nothing in the ISA, the ABI or LazOS needs it, and adding it before
  an engine boundary is settled would multiply the ways a switch could go wrong.
- **No MMU or paging.** `AddressSpace` is a set of regions with permissions and an
  identity used for context switching. There is no translation, no page table and
  no `CR3`. The decode cache's invalidation rule is already written in terms of
  the *physical* range "so it stays correct if a future step gives the address
  space a real translation", which is a statement that translation was anticipated
  and deliberately not built.
- **No device hot-plug.** A machine's device set is fixed at construction. A
  machine profile (see `docs/machine-profiles.md`) would change that, and B4 is
  where it belongs.
- **No live migration.** Snapshots and restore exist and are tested; moving a
  running machine between hosts is B18 and depends on a device backend model that
  does not exist yet.

---

## Related

- `docs/architecture.md` — the whole platform contract
- `docs/device-model.md` — the device contract and the frontend/backend split
- `docs/machine-profiles.md` — versioned machine configurations
- `docs/beyond-lazalith.md` — the master Beyond document
- `crates/lazalith-machine/tests/engine.rs` — the tests described above
- `crates/lazalith-machine/tests/differential.rs` — the harness a JIT will be compared in
