# LazOS expansion

This document is Step 91 of the roadmap. The step lists eight areas and says only to
expand the system once the basic platform works. It works, so the eight areas are
expanded here — and the table below says exactly how far each one got, because a step
that quietly did a tenth of one of them and said nothing is worse than one that says
so.

| area | what exists now | what does not |
| --- | --- | --- |
| **permissions** | a capability gate on every syscall; a package's declaration is enforced, a bare `.lzx` is trusted | per-file or per-user permissions; the gate is not a sandbox against a malicious kernel |
| **better filesystem** | `rename`, `remove`, `truncate`, and a recursive `walk` | symlinks, hard links, timestamps, permissions on nodes, a real on-disk format |
| **process management** | a path resolves to a program — package or image — and a package's capabilities reach the process it creates | blocking `wait`; a process group; `exec` in place of the process's own image |
| **threads** | one register file per thread, a per-thread memory context, and a scheduler that saves and restores a named thread's state | *creating* a second thread. There is no `spawn_thread`, so every process still has exactly one. |
| **networking** | nothing: no network device, so no socket ABI | everything. Inventing a socket that always fails is not a network stack. |
| **audio** | nothing: no audio device, so no sound ABI | everything, for the same reason. |
| **more drivers** | a timer device: a 64-bit cycle counter readable through memory-mapped I/O, refusing writes, carried in snapshots | a timer *controller* — compare, interrupt, programmable period; a block device; a serial device |
| **virtual memory** | `AllocateMemory` hands out whole regions from a per-process layout | mapping at an address the process chose, unmapping, page faults, sharing |

## The capability gate, which is the interesting one

`docs/lazen-applications.md` says a manifest's `[permissions]` declares "the OS
capabilities the application intends to use", and `docs/lazen-packages.md` recorded
that nothing enforced them: the package carried a declaration and the syscall layer
had no gate. This is that gate, and it is the reason the step is worth more than the
other seven areas put together.

**A process started from a package may only make the syscalls its package declared. A
process started from a bare `.lzx` may make all of them.** That asymmetry is the
design, and it is deliberate:

- A package is the *untrusted* unit. It arrives from somewhere, its author wrote down
  what it needs, and the system can check the two agree.
- A bare `.lzx` is the *trusted* unit. It is what `lazen build` produced, what the
  boot ROM loads, and what a person runs on their own machine. A gate on it would
  break the kernel, the init shell, and every test, in exchange for protecting a
  program that already has the whole machine.

So the gate is a property of *how a process was started*, not of what it contains, and
`LazalithKernel::start_resolved` is the one place that decides — which means there is
no way to start a package and forget to restrict it, or to restrict a bare image by
accident.

Three details that took thought:

- **The check is on the syscall, not the device.** `write` is a console call when the
  handle is a terminal and a file call when it is not, and a program that declared
  `console = true, filesystem = false` and wrote to a file is exactly what the
  declaration should catch.
- **The refusal is the same whether or not the arguments were valid**, and it happens
  *before* the arguments are read. A process without the filesystem capability learns
  nothing about the ABI by calling `open` with a bad pointer, because the refusal does
  not depend on the pointer.
- **A capability this build cannot enforce is not granted.** The unknown bits of a
  package's *own* record fields are held rather than refused — a reader that refused a
  newer package could not install it at all — but a kernel that handed out a
  capability it did not implement would be promising something it does not have. The
  two rules are opposites and both are deliberate.

The gate lives in `lazalith-os-abi::capability`, not in the kernel, because the
permission bits are a contract between a program and the system that runs it. The
package format re-exports the ABI's definitions rather than declaring its own, so there
is one set of four bits rather than two that happen to agree.

## What the filesystem added, and why each way

- **`rename` is a re-link, not a copy.** The node keeps its identity, so a handle open
  on the old name still reads the same bytes. A copy would leave two nodes with the
  same contents and the next write through one handle invisible through the other. A
  directory that would move inside itself is refused by asking whether the *moved node*
  contains the *destination's parent* — the other direction of that question refuses
  every rename, because the destination parent is usually an ancestor of the node being
  moved.
- **`remove` refuses a directory with children.** Removing a directory means "I am done
  with this"; emptying a directory is a different act that happens to share a name.
- **`truncate` grows with zeros.** A program that seeks past the end and reads must get
  zeros and not another file's bytes.
- **`walk` is sorted and includes its start.** Two walks of one tree produce the same
  list, which is what makes it usable in a test. It reports the root as `/` rather than
  as the empty path, because the empty path is not a path anything can open.

## The timer, and why it is a device

`Time` and `Sleep` are enough for a program that asks the kernel how long it has been
running. They are not enough for a program that wants to *measure* something: a
syscall costs the program a trap, and a measurement that interrupts what it measures
is a measurement of the trap.

So the cycle count is also a device, read by loading from an address like anything
else. The register is 64 bits wide on both targets, because a counter that wrapped at
2³² on a 32-bit target would be a clock that lied after about seven minutes. Writes
are refused rather than ignored: a program that believed it had reset the clock and
had not would then measure a span it thought it controlled. And the counter is in the
device snapshot, because a program that read the clock and was restored onto a machine
with a different one must not read a time that never happened.

## Threads: how far, and what is missing

A process already carried a thread per thread id, each with its own `ArchitecturalState`,
and the scheduler already moved a machine's state into and out of a *named* thread. So
the per-thread state is real and was already load-bearing.

What is missing is the other half: **nothing creates a second thread.** There is no
`spawn_thread` syscall, so every process still has exactly one, and
`assert_eq!(process.thread_count(), 1)` is in the tests precisely so that a process
which grew a second thread with nothing creating one would fail rather than pass
quietly. `Process::thread_state` was added so a caller can ask for a thread's register
file and be told `None` for one that does not exist, rather than being handed the first
thread's registers.

Thread *creation* is the missing piece and it is not small: it needs a syscall, a place
for the new thread's stack inside the process's own memory, and the scheduler
round-robining over threads as well as over processes. It is the honest remainder of
this step.

## Networking and audio: nothing, on purpose

There is no network device and no audio device, so there is nothing to drive, so there
is no ABI. A socket that always returned `NotSupported` would be a syscall a program
could call and be refused by — which is a real thing, and not a network stack. The
test in `tests/expansion.rs` asserts the *absence*, so a socket or a sound syscall
appearing without a device behind it would fail there.

## Verification

- 1093 workspace tests pass.
- Every area above has at least one test named after it, in
  `crates/lazalith-os/tests/expansion.rs`, and the two areas that got nothing have a
  test saying so.
- `crates/lazalith-fuzz`'s eleven targets are clean, including the object and package
  readers this step's changes touched.
