# LazOS Design

## Purpose

LazOS is the native operating system for Lazalith. It provides a small, explicit
environment for Lazen applications and future system tools while preserving the
Reference Interpreter as the authoritative CPU behavior.

LazOS is not a Linux clone and does not reproduce Linux process, filesystem,
driver, or syscall models. It uses Lazalith's own flat memory model, trap
semantics, device interfaces, and native toolchain formats. The first milestone
prioritizes correctness, deterministic headless execution, and a complete
vertical path from bootloader to user program.

## Design principles

1. **Explicit ownership:** the machine owns hardware state; the kernel owns OS
   policy; processes own their CPU and memory state. No global machine or kernel
   mutable state is required.
2. **Reference execution first:** correctness and determinism come from the
   reference CPU. Interrupt delivery, scheduling, and program loading must obey
   its architectural behavior rather than approximate it.
3. **Validate before mutation:** image, trap, syscall, memory, and process
   operations preflight ranges, permissions, versions, and allocation effects.
4. **Structured failure:** malformed input and runtime faults return typed errors
   with retained context and causes; arbitrary bytes never cause kernel panic.
5. **No accidental privilege:** User software reaches hardware only through
   validated kernel services and architecture-enforced permissions.
6. **Deterministic virtual time:** scheduling and delay semantics use controlled
   virtual cycles, never the host wall clock.
7. **Small complete mechanisms:** round-robin scheduling, flat protected regions,
   and an in-memory filesystem are sufficient before optimization or paging.
8. **Headless first:** all kernel, process, filesystem, and application tests
   run without SDL3. SDL3 remains a later host/frontend.

## Current foundation

The following components exist before LazOS:

- validated LZ32/LZ64 configuration and shared architectural types;
- structured diagnostics and source provenance;
- shared ISA metadata, canonical instruction encoding/decoding, and reference
  interpretation;
- register file, architectural CPU state, status/conditions, and typed outcomes;
- flat RAM/ROM mappings, permissions, structured memory faults, and a Bus;
- generic devices, a bounded console, and deterministic virtual time;
- a privately owned `LazalithMachine` with explicit lifecycle states;
- a fixed typed bootloader that validates and loads a kernel image;
- the shared v1 OS ABI and trap-admitted syscall dispatcher; and
- explicit `Process`/`Thread` ownership with validated program images, stacks,
  handles, and lifecycle states;
- deterministic round-robin process scheduling with explicit context tokens;
- the bounded native `.lzx` v1 loader; and
- an owned in-memory virtual filesystem with per-process handles and a typed
  syscall service.

LazOS does not duplicate those contracts. The bounded userspace shell and
minimal native `init` image are implemented; the source assembler, object
linker, and host-side headless `run` path are implemented for bounded images,
while real input devices, in-image process launch, and a packaged guest kernel
remain future roadmap work.

## System layers

```text
Host / headless frontend
    |
    | owns machine construction and deterministic input policy
    v
LazalithMachine + Bus + devices + virtual clock
    |
    | boot ROM transfer, machine-owned controller boundary
    v
LazOS kernel (Supervisor)
    |
    | typed service calls and validated virtual hardware
    v
User processes
```

The host may construct a machine, load a boot image, and supply a filesystem or
input script for tests. Those host capabilities are not guest-visible kernel
APIs. The bootloader transfers control in Supervisor mode; LazOS establishes
all later User execution policy.

## User, Kernel, and Hardware separation

LazOS has exactly two privilege modes defined by the ISA:

- **User** (`U=1`) runs application code. It receives only the memory and
  services explicitly granted by kernel policy.
- **Supervisor** (`U=0`) runs the bootloader and kernel. It may use architectural
  control operations, but it still obeys every mapped region's R/W/X permissions.

The hardware substrate is the `LazalithMachine`, Bus, address space, devices,
and virtual clock. Hardware is not a third privilege mode and is not owned by
User software. The kernel is the only component allowed to connect User requests
to hardware services.

The required request path is:

```text
User Program
    -> SYSCALL (or TRAP/fault/interrupt)
    -> machine-owned TrapController
    -> kernel dispatcher and validated service
    -> Bus / virtual hardware
    -> structured result returned through the ABI
```

A User program cannot turn a syscall into a direct function call into kernel
memory. The shared code pages, if any, contain no hidden entry port; control
transfers through the architectural trap mechanism and kernel dispatcher only.

### Direct-access rules

| Operation or resource | User | Supervisor / kernel |
| --- | --- | --- |
| Ordinary non-privileged instructions | Allowed subject to memory checks | Allowed subject to memory checks |
| `SYSCALL` and `TRAP` | Allowed; privilege changes only on entry | Allowed; may enter for OS handling |
| `HALT`, `RFE`, `EI`, `DI`, `CSRR`, `CSRW` | `PrivilegeViolation` before effects | Allowed subject to controller state rules |
| User-permitted RAM/ROM | R/W/X only as mapped and `user=true` | Same permission checks; Supervisor has no R/W/X bypass |
| Kernel-only regions | Rejected | Allowed only where their R/W/X bits permit |
| Trap TVEC, EPC, ESP, ESTATUS, TCAUSE, TPAYLOAD | Not directly accessible | Accessed only through validated controller operations |
| Device registers/MMIO | Never mapped: a User context cannot be bound while a device mapping exists | Routed through validated Bus/device operations |
| `Bus`, `AddressSpace`, `DeviceManager`, host loaders, mappings | Not guest-visible | Private implementation details, never exposed wholesale |
| Debugger `peek` and raw host inspection | Not a guest operation | Trusted frontend/debugger only; never substituted for User access |
| Physical/instruction/virtual address policy | User pointers only through ABI validation | Typed kernel-owned domains, no implicit conversions |

`SYSCALL` and `TRAP` are not privileged instructions: both modes may execute
them, and neither changes privilege until trap entry commits. Arithmetic,
loads, stores, branches, and calls never change `U` or `IE`. Trap entry forces
Supervisor and clears `IE` while preserving NZCV, SP, and all general registers.
`RFE` restores only validated editable resume PC/SP/status; the immutable trap
snapshot is not automatically restored into registers.

User software therefore cannot:

1. execute `HALT`, `RFE`, interrupt-control, or control-register instructions;
2. read or write kernel-only memory or User-denied device mappings;
3. change its own privilege or interrupt state through a side channel;
4. push or forge a kernel trap frame, replace TVEC, or select a private handler;
5. invoke Bus mapping, device insertion/reset, host loading, raw peek, machine
   reset, scheduler, allocator metadata, or process-table internals;
6. retain a file/process handle outside its owning kernel table; or
7. bypass kernel validation of pointers, lengths, offsets, versions, permissions,
   ownership, or resource limits.

The kernel cannot evade hardware policy: Supervisor does not bypass R/W/X, MMIO
fetch/stack restrictions, width checks, address checks, or device validation.
A kernel service is privileged code plus explicit policy, not an architecture
superuser.

### Syscall boundary

A syscall is admitted only after trap entry supplies a typed request. The
Step 37 dispatcher validates the syscall identity, argument count, pointer
range, integer width, address-space binding, and resource limits before invoking
an injected service. A rejected request returns a structured status/error result
and does not partially mutate process or kernel state. Services translate
validated requests into Bus/device/filesystem/process operations and return only
the result contract defined by `lazalith-os-abi`.

The exact syscall IDs, register mapping, and result encoding are Step 35. This
separation document deliberately does not invent them early. It establishes the
rule that all future User-to-kernel communication follows the same trap,
validation, service, and result path.

### Host and frontend boundary

The host may construct a machine, provide a boot image, attach trusted virtual
devices, inspect state, and collect output. Those capabilities are outside the
guest ISA. A headless frontend or SDL frontend must not become a shortcut for a
User syscall: the same kernel dispatcher and virtual hardware must execute in
both cases. SDL3 rendering may observe kernel output but cannot grant User
memory/device access or alter trap semantics.

Steps 31 and 32 implement this design using the existing `Privilege`, status,
metadata, and memory permission checks. Steps 33–43 extend the protected regions
and services; they do not create a second User/Kernel flag or a kernel backdoor.

## Boot handoff

The Step 28 bootloader:

1. validates the fixed boot image;
2. maps boot ROM, kernel-image RAM, and the kernel stack;
3. copies the kernel payload;
4. verifies the kernel entry instruction and handoff state; and
5. stops before executing the first kernel instruction.

The kernel entry contract is defined in `docs/boot.md`. The first LazOS
responsibilities are to validate any additional handoff data it needs, establish
its own invariants, and continue explicit machine execution. Boot success does
not imply that traps, User mode, interrupts, devices, or the filesystem are ready.

## Kernel responsibilities

The kernel is one Supervisor component with these eventual subsystems:

| Subsystem | Responsibility | Roadmap implementation |
| --- | --- | --- |
| Trap control | Trap entry/return, active frame, synchronous causes | Step 31 (implemented) |
| Privilege | Supervisor/User operation and memory policy | Step 32 (implemented) |
| Memory | Kernel/user layouts, stack, heap, process address spaces | Steps 33–34 and 38 (implemented) |
| ABI/services | Shared syscall numbers, arguments, results, errors, trap admission | Steps 35–37 (implemented) |
| Processes | IDs, ownership, state, handles, program image, service context | Step 38 (implemented) |
| Scheduling | Runnable/blocked state and round-robin progress | Step 39 (implemented) |
| Program loading | Validate and map `.lzx` executables | Step 40 (implemented) |
| Filesystem | Virtual paths and open/read/write/close/seek/stat | Step 41 (implemented) |
| Userspace | `init` and the `lazos$` shell | Steps 42–43 native composite fixture implemented |

Each subsystem remains private behind kernel-owned APIs. A frontend or compiler
consumes data contracts such as the OS ABI and object format, not kernel memory
or machine internals.

## Process and execution model

LazOS owns explicit `Process` and `Thread` aggregates. A process owns its
independent address space and allocator, program image, stack, handles, process
state, and one or more threads. A thread owns an isolated `ArchitecturalState`
initialized in User mode with interrupts disabled. `ProgramImage` owns a copied,
bounded byte image and validates its architecture, entry alignment, entry
bounds, and instruction-sized entry before loading.

Process states are explicit: `Created`, `Ready`, `Running`, `Blocked`, `Exited`,
and `Faulted`. Transitions are checked before mutation; terminal states cannot
be resurrected, and exit records the code. The first thread is created with the
image entry and the process stack's validated initial SP. Later threads must
use `Thread::for_process`, which checks the image entry, stack ownership, mapped
User RAM, architecture, and mode before creating CPU state. New threads are
attached through `Process::attach_thread`, which records the owner and rejects
duplicate or foreign thread IDs.

`Process::memory_context` and `memory_context_for_thread` are the constructors
used for host-side inspection of a process that is not currently running. While
a process is running, the machine holds its User regions, so only
`memory_context_for_thread_in_space`, built from the live machine space, is a
valid service context; the other two would observe the process's dormant space.
The resulting context binds the request to one
process/thread identity, active scheduler execution token, and address-space
identity while lending the process's allocator, mutable User address space,
typed handles, active thread CPU state, image/stack metadata, and
lifecycle/exit fields. A process must be `Running` and explicitly activated
with the same execution token held by the machine trap controller before a
syscall can be admitted. The dispatcher rejects a request whose process,
thread, execution token, lifecycle state, or address-space identity does not
match that context; requests cannot be rebound after admission.

The first scheduler is deterministic round-robin. A scheduler decision selects
which process is active; the thread it binds is always that process's primary
thread, and additional threads are owned, addressable state that no scheduler
path dispatches yet. It does not copy global machine state or hide context
switches inside the CPU. Blocking, exit, and fault transitions are
validated by the process model. Exact state-transition tables and IDs are
defined by the Step 38 implementation, not inferred from CPU execution state.

`RoundRobinScheduler` owns the process table, cursor, quantum, unique execution
contexts, stable logical process identities, and an optional aggregate
User-space admission budget. At a trap-free boundary it validates the selected
process/thread and machine state, uses a typed fallible transfer to swap only
User address-space regions while retaining Supervisor regions, installs the
owned CPU context, and binds a fresh token. At every yield it saves the complete
User CPU context, returns the live User space to the process, validates and
clears the token, and selects the next ready process. A User `SYSCALL` or
software trap consumes one quantum at trap entry; handler instructions,
external-interrupt delivery, and `RFE` do not consume User quantum, and a
pending yield is applied after a frame-free return. Synchronous faults abort
only their own frame and transition that process to `Faulted`; syscall, software
traps, and external interrupts remain framed for the kernel handler. Non-
returning exit/fault outcomes are finalized without a returning completion.
A terminal machine error poisons the scheduler until a coordinated machine
reset, and recovery never reuses a binding whose User-space ownership cannot be
restored. The scheduler has no priorities, SMP, or hidden implicit context
switch.

## Memory direction

LazOS v1 uses the flat, identity-mapped architecture already implemented in
`AddressSpace` and `Bus`. It separates regions by Supervisor/User and R/W/X
permissions. The boot image owns boot ROM, kernel image, and kernel stack;
`Process` owns an independent validated User address space and layout. The
Step 39 scheduler activates one process space and CPU context at a time while
retaining Supervisor regions in the machine-owned space.

The kernel will provide the minimum aligned kernel allocation, user memory,
stack, and heap services required by programs. It will not call itself a
virtual-memory system until real translation exists. The exact flat model and
allocation policy belong to Steps 33–34.

## Traps and interrupts

A SYSCALL, software TRAP, synchronous fault, or accepted external interrupt is
handled by one machine-owned trap controller. Entry snapshots exact CPU state,
delivers validated Supervisor control state without pushing a frame onto an
untrusted User stack, and uses a single active frame. RFE returns through
validated resume fields. External interrupt IDs remain coalesced and pending
until accepted according to `docs/isa.md`.

The minimum trap behavior is now implemented in the CPU and machine layers. One
controller owns TVEC, a single active frame, immutable pre-entry snapshots, and
editable EPC/ESP/ESTATUS. Entry validates the target execute fetch, preserves
registers/SP/NZCV, forces Supervisor, clears IE, and writes no guest frame.
CSRR/CSRW and RFE use those controls. A missing/invalid target, failed entry, or
double trap is terminal and retains the triggering attempt/context. External
requests are coalesced, sorted, masked by IE, delivered only at an eligible
instruction boundary of an executable machine state (`Reset`, `Running`, or
`Paused`, so single-stepping still services them), and acknowledged only after
entry succeeds. The syscall
dispatcher and trap-aware scheduler boundary are implemented; interrupt device
assignments and the concrete kernel handler remain later work.

## System calls

The OS ABI is materialized once in `lazalith-os-abi` and is currently shared
by the kernel, its syscall dispatcher, and the headless shell fixtures. The same
contract is intended to serve the future Lazen runtime, C runtime, SDK, and
debugger. It defines custom syscall IDs, register argument/result transport,
structured status/error results, pointers, handles, lengths, offsets, and mode
differences.

The initial service set is intentionally aligned with later filesystem and
process work: exit, read, write, open, close, seek, stat, list, time, sleep,
memory allocation, spawn, wait, and clear screen. The shipped
`FileSystemService` implements exit, read, write, open, close, seek, stat, list,
and clear screen for real. `time`, `sleep`, `AllocateMemory`,
`SpawnProcess`, and `WaitProcess` are fully validated by the dispatcher and
reach the service, which returns `NotSupported` in this milestone; the user
allocator exists in `UserMemory` but is not yet wired to a syscall. Graphics and
input calls are deferred until drivers exist. No Linux numbers or Linux calling
convention are copied.

## Filesystem and program loading

The first filesystem is a clean virtual filesystem backed by deterministic
in-memory storage. It supports regular files, directories as needed by `ls`,
per-process handles, open modes, seeking, metadata, and explicit close. Path
and error semantics are implemented with the filesystem, not guessed by the
shell. Paths are absolute byte strings with no NUL, empty, `.`/`..`, repeated
separator, or overlong components; components are bounded by the ABI's
252-byte directory-record capacity. The backend has explicit file, node, and
directory-entry limits, and every allocation is fallible. A process owns
monotonic file handles and offsets; handles are never recycled after close.
`FileSystemService` translates the already-validated ABI operations to checked
User-memory transfers and returns structured syscall statuses. Directory
records are emitted in bytewise name order, and directory sizes report the
number of immediate entries. No host filesystem, persistence, symlink, or
dedicated mkdir/remove/rename syscall is part of this first implementation;
`OPEN_CREATE` may add a regular file to an existing directory.

The first executable loader parses the bounded native `.lzx` v1 contract in
`docs/lzx.md` and validates architecture, ISA version, OS ABI version, sections,
permissions, entry, and memory requirements before process mutation. The v1
container is little-endian, non-compressed, and has explicit code/data/BSS
sections; its parser is shared with the later toolchain model rather than
accepting a raw code blob. Filesystem persistence and drivers are not part of
the initial milestone.

`build_init_image` constructs the minimal Step 42 init executable as a typed
`.lzx` image for either architecture. It contains only `LI r0, 1` and `SYSCALL`,
then exits with status zero through the real trap dispatcher; it is not a host
binary or an opaque checked-in blob. The boot integration test proves the fixed
ROM handoff, User activation, instruction execution, syscall trap, and
non-returning exit path. A packaged guest kernel and automatic production boot
orchestration remain future work; the native scheduler/dispatcher APIs stand in
for that kernel-side coordination in this milestone.

`HeadlessShell` provides a bounded host-side command engine over the owned
virtual filesystem. It emits the exact `lazos$ ` prompt, supports `help`,
`echo`, `ls`, `cat`, and `clear`, keeps a bounded deterministic transcript with
clear-generation tracking, and runs `run <path>` by reading the actual `.lzx`
file from the virtual filesystem, parsing it, and scheduling the process through
`LazalithKernel::start_image`. In-image native `run` remains explicitly
deferred because `SpawnProcess`/`WaitProcess` services are future work. Its
grammar is intentionally line-oriented: surrounding spaces/tabs are trimmed,
`echo` accepts the remaining text, and path commands accept one whitespace-free
byte path. This host grammar is the authoritative prompt specification.

`build_init_shell_image` adds a real composite init/shell `.lzx` fixture. Its
User code uses typed instruction emission, terminal descriptors 0/1, checked
`IoResult` records, and filesystem syscalls for a bounded `ls`/`cat` path.
`LazalithKernel` owns the scheduler, terminal/filesystem service, image start,
and trap-return loop used by the headless boot test. The test drives the actual
ROM handoff, Supervisor RFE trampoline, User activation, returning syscalls,
EOF exit, and process release in both architectures. `run` is recognized and
explicitly deferred because SpawnProcess/WaitProcess ownership is not
implemented yet; a separate packaged kernel image and real device-backed
terminal remain future work.

The in-image shell is a declared conformance subset of the host specification,
not a second specification: it implements `help`, `echo`, `clear`, and a
fixed-path `ls`/`cat` over the fixture filesystem, reports `unknown command` for
input outside that subset (including a blank line and a path argument), and
defers `run`. The shared conformance test in
`crates/lazalith-os/tests/native_shell.rs` pins the prompt, the `unknown
command` behavior, the `ls` divergence, and clear-generation tracking for both
implementations, so the difference stays declared and tested.

## Devices, console, display, and input

Devices remain private to the machine Bus and device manager. The kernel reaches
them through validated service boundaries, not raw host references. The existing
console is a deterministic bounded byte sink. `init` and the shell can use the
kernel console service without SDL.

A display driver, input driver, framebuffer policy, and interactive terminal
protocol require later roadmap hardware steps. The headless frontend will use
scripted byte input and captured byte output. SDL3 may later render the same
virtual devices but cannot define kernel semantics or become a CPU dependency.

## Time and scheduling

`CycleCount` and `VirtualClock` provide deterministic virtual time. Device
ticks and later sleep/blocking behavior use this clock. The kernel does not
sample host wall time. Step 39 uses an explicit instruction quantum and
round-robin cursor; it has no priorities or SMP in the initial milestone.

## Errors and diagnostics

Every externally visible failure retains structured context:

- boot images retain field, offset, architecture, and cause;
- CPU/memory/trap failures retain PC, privilege, access, width, and typed cause;
- syscall failures retain syscall identity, argument validation, and status;
- image and filesystem failures retain offsets, handles, limits, and causes;
- process/scheduler transitions retain IDs and rejected states.

Internal invariant violations may stop a test or host, but malformed guest data
must never use `panic!`, `todo!`, `unimplemented!`, wrapping allocation, or a
string error as normal control flow.

## Delivery sequence

LazOS is delivered dependency-first:

1. Step 30 fixes the User/Kernel/Hardware boundary and forbidden operations.
2. Steps 31 and 32 now implement traps/interrupts and the privilege policy.
3. Steps 33–34 define and implement flat protected memory management.
4. Steps 35–39 define and implement the shared ABI, syscall dispatcher,
   process/thread ownership, and deterministic scheduling.
5. Step 40 validates `.lzx` images into a process.
7. Step 41 implements the virtual filesystem.
8. Steps 42–43 boot `init` and the real `lazos$` shell headlessly.
9. Steps 44–49 complete the real assembly/object/executable path and shell-launched
   program.

A later step does not backfill these documents with claims of code that does not
yet exist.

## Initial success criteria

LazOS is operational for the Step 43 milestone only when a headless machine
boots the fixed bootloader, reaches a real kernel, creates and schedules User
processes, loads a validated program, starts `init`, displays `lazos$`, executes
the required shell commands through kernel services, and remains deterministic
without SDL. Until then, earlier design text describes intended architecture,
not implemented behavior.
