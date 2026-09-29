# BEYOND LAZALITH — MASTER NEW-SESSION ARCHITECTURE, RESEARCH & TOOLCHAIN PASS

You are starting a **NEW SESSION** on the Lazalith repository.

This task establishes the architecture for **Beyond Lazalith**, the post-Step-100 phase.

This is a **research, repository-analysis, architecture, and documentation pass**.

**DO NOT begin full Phase-II implementation yet.**

The objective is to define the architecture first, based on:

- the actual current repository
- actual Rust implementations
- existing Phase-I contracts
- OpenCode Read inspection
- rust-analyzer/LSP analysis
- authoritative external WebSearch research
- the future target-side OS/toolchain requirements
- the historical Linux 0.01 source tree

---

# 0. AUTHORITATIVE PROJECT FILES

This is a NEW SESSION.

Before doing architecture work, inspect the attached/current project files.

At minimum:

```text
instruction.md
docs/project-state.md
Cargo.toml
flake.nix
flake.lock
```

Also inspect these if present:

```text
docs/architecture.md
docs/isa.md
docs/lz32.md
docs/lz64.md
docs/platform.md
docs/os-design.md
docs/os-memory.md
docs/lazen-design.md
docs/lazen-types.md
docs/c-runtime.md
docs/graphics-test.md
```

The repository itself is the ultimate source of truth:

```text
/home/pbmine/Documents/Lazalith
```

Do not assume the attached files completely represent the repository.

Use the actual repository for code inspection.

---

# 1. INITIAL NEW-SESSION WORKFLOW

Before making architectural decisions:

1. Read `instruction.md`.
2. Read `docs/project-state.md`.
3. Inspect root `Cargo.toml`.
4. Inspect `flake.nix` and `flake.lock`.
5. Inspect all workspace crates.
6. Identify actual binaries/composition roots.
7. Identify current machine/VM architecture.
8. Identify current OS boundary.
9. Identify current compiler/toolchain boundary.
10. Identify current GUI/SDL3 boundary.
11. Identify current object/linker boundary.
12. Identify current debugger/snapshot/replay boundary.
13. Inspect the GitHub repository/CI/CD surface and existing automation.

Do NOT begin Phase-II implementation during this pass.
### GitHub / CI-CD repository surface

Inspect these if present:

```text
.github/
.github/workflows/
.github/dependabot.yml
.github/CODEOWNERS
.github/ISSUE_TEMPLATE/
.github/PULL_REQUEST_TEMPLATE.md
```

GitHub is part of the project engineering architecture, not merely the remote Git host.

Determine how CI/CD currently validates the same contracts that are expected locally, and identify what is missing.


---

# 2. MANDATORY OPENCODE READ

Use OpenCode's **Read** feature extensively.

Do not design from:

- filenames alone
- summaries
- memory
- grep output alone
- generated API documentation alone

Read the actual Rust implementation.

At minimum inspect the relevant implementations of:

```text
LazalithMachine
ReferenceInterpreter
CPU
Bus
AddressSpace / memory
Device / DeviceManager
VirtualClock
MachineSetup / machine configuration
snapshot / restore
DebugController
DebugSession
loader
linker
IR
codegen
Lazen compiler
C compiler
C runtime
Lazen runtime
CLI/composition root
GUI
SDL3 boundary
graphics
input
kernel / OS integration
```

Read enough surrounding code to understand:

- ownership
- invariants
- error propagation
- state transitions
- public visibility
- device lifecycle
- VM lifecycle
- debugger lifecycle
- compiler stages
- serialization contracts
- actual dependency relationships

---

# 3. MANDATORY RUST-ANALYZER / LSP

Use rust-analyzer/LSP together with OpenCode Read.

Use LSP for:

- go-to-definition
- find references
- trait implementations
- call hierarchy
- type information
- diagnostics
- API consumers
- cross-crate relationships
- duplicate semantic implementations
- stale references
- refactoring impact

For any major proposed API boundary:

```text
LSP references
→ Read implementation
→ Read callers
→ Read tests
→ identify invariants
→ design interface
```

Do not rely solely on text search.

Use all of:

```text
LSP
+
OpenCode Read
+
repository search
```

---

# 4. MANDATORY OPENCODE WEBSEARCH

Use OpenCode's **WebSearch** feature for external technical research.

Do not rely on memory when authoritative documentation is available.

Research at minimum:

```text
QEMU system emulation architecture
QEMU machine types
QEMU CPU/execution engine model
QEMU TCG/JIT
QEMU device model
QEMU buses
QEMU device frontend/backend model

VirtualBox VM management
VirtualBox virtual hardware
VirtualBox storage
VirtualBox networking
VirtualBox USB
VirtualBox audio
VirtualBox display

GCC compilation stages
GCC compiler driver
GNU assembler / binutils
GNU ld object/linking model
GCC sysroot organization

Rust SDL3
sdl3 crate
sdl3-sys
SDL3 API coverage

VGA / EGA
audio devices
storage controllers
USB
PCI / PCIe
IDE / ATA
NVMe
serial
keyboard/input
firmware/boot

Linux 0.01 source
Linux 0.01 architecture assumptions
Linux 0.01 hardware requirements


### GitHub / CI-CD research

Research current official GitHub documentation for:

```text
GitHub Actions
workflow syntax
job dependencies
matrix builds
artifacts
environments
releases
tag-triggered workflows
Dependabot
CODEOWNERS
branch protection / required status checks
GitHub security scanning where appropriate
GitHub-hosted runners
```

Use this to design a maintainable Lazalith CI/CD system. Prefer GitHub's official documentation for GitHub-specific behavior.

Prefer:

- official upstream documentation
- historical primary sources
- official specifications
- upstream repositories

For any externally-informed architectural decision:

1. identify the source
2. read the relevant material
3. distinguish fact from inference
4. explain the Lazalith-specific decision
5. cite the source in the master document

Do not copy another project's architecture blindly.

---

# 5. PHASE-I BASELINE

Phase I intentionally produced a tightly integrated platform.

The historical stack is:

```text
Lazalith ISA
    ↓
Lazalith CPU
    ↓
Memory / Bus
    ↓
Virtual Hardware
    ↓
Bootloader
    ↓
LazOS
    ↓
System ABI
    ↓
Lazen Runtime
    ↓
Lazen
    ↓
Applications
```

The project also includes:

```text
Rust toolchain
Assembly
C compiler
Lazen compiler
Debugger
SDL3 GUI
Nix environment
```

Do not rewrite this history.

Beyond Lazalith is the next architecture phase.

---

# 6. MAIN GOAL OF BEYOND LAZALITH

The objective is:

> **Make Lazalith look and behave like a real computer architecture ecosystem, compiler/toolchain ecosystem, and VM platform rather than one giant integrated implementation.**

Conceptually, the ecosystem should become closer to:

```text
GCC / binutils
        +
QEMU / VirtualBox
        +
a real target operating system
```

while remaining unmistakably Lazalith.

The result must NOT become:

```text
generic VM
+
random compiler
+
random JIT
```

The stable identity is:

```text
LZA architecture
+
Lazalith instruction semantics
+
Lazalith ABI
+
Lazalith object/executable model
+
Lazalith VM semantics
+
Lazalith device contracts
```

---

# 7. FORMAL ARCHITECTURE NAME

Use:

**LZA — Lazalith Architecture**

Variants:

```text
LZA32
LZA64
```

Use `LZA64` as the primary 64-bit architecture/target identity.

Existing terminology:

```text
LZ32
LZ64
```

may remain in current APIs and documentation where already established.

Document the distinction:

```text
LZA64
= formal architecture / target identity

LZ64
= existing ISA / width terminology
```

Do NOT perform a broad rename during this architecture-only pass.

A possible future target identifier is:

```text
lza64-unknown-lazos
```

but that is a proposal, not an implemented target triple.

---

# 8. THE IDENTITY RULE

Beyond Lazalith is still Lazalith because the guest-visible foundation remains:

```text
LZA architecture
    ↓
instruction semantics
    ↓
Lazalith ABI
    ↓
Lazalith object/executable formats
    ↓
Lazalith VM contract
    ↓
Lazalith device contracts
```

Host implementations may change.

Guest-visible semantics must not casually change.

Host-specific technologies such as:

```text
SDL3
Wayland
host audio
host filesystem
host networking
host sockets
host USB
GUI frameworks
```

must remain implementation details behind Lazalith abstractions.

---

# 9. LAZALITH VM CORE / LVMI

Design a stable VM abstraction.

A candidate name:

**LVMI — Lazalith Virtual Machine Interface**

Do not finalize the name until repository research confirms that it is suitable.

The VM Core should define, where applicable:

```text
CPU execution
architectural CPU state
memory/address spaces
instruction fetch
bus accesses
MMIO
interrupt injection
interrupt acknowledgement
virtual clock
virtual timers
DMA
reset
boot
device lifecycle
machine lifecycle
machine state
snapshot/restore
debug hooks
```

The VM Core MUST NOT depend on:

```text
SDL3
GUI toolkit
host audio API
host filesystem API
host networking API
windowing framework
```

---

# 10. MULTIPLE EXECUTION ENGINES

The Reference Interpreter remains the semantic authority.

The architecture should support:

```text
Reference Interpreter
        ↓
semantic oracle

JIT
        ↓
accelerated execution

future execution engines
        ↓
same LZA semantics
```

The VM must not care whether execution is performed by the interpreter or JIT.

Every execution engine must preserve guest-visible LZA behavior.

Differential testing against the Reference Interpreter is mandatory.

---

# 11. JIT — FIRST-CLASS BEYOND LAZALITH GOAL

The JIT is:

- NOT the compiler
- NOT the assembler
- NOT another ISA
- NOT another object format
- NOT a language frontend

It is an **execution engine for LZA code**.

Target model:

```text
.lzx / loaded LZA executable
        ↓
      VM
        ├── Reference Interpreter
        │        ↓
        │   semantic authority
        │
        └── JIT
                 ↓
           host-native execution
```

The JIT may translate:

```text
LZA machine instructions
→ host machine code
```

but guest semantics remain LZA.

Do not create:

```text
C → special JIT
Lazen → special JIT
ASM → special JIT
```

Instead:

```text
C ───────┐
Lazen ───┼──→ common object/executable → VM
ASM ─────┘                                │
                                          ├── Interpreter
                                          └── JIT
```

## Interpreter ↔ JIT interoperability is mandatory

The JIT MUST be interoperable with the Reference Interpreter at runtime.
They are two execution modes of the same VM, not two separate virtual machines.

The architecture must support transitions such as:

```text
Interpreter
    ↓
  switch
    ↓
   JIT
    ↓
  switch
    ↓
Interpreter
```

and:

```text
JIT execution
    ↓
breakpoint / fault / debug event / unsupported block
    ↓
recover architectural state
    ↓
Interpreter continues from the same guest state
```

and:

```text
Interpreter execution
    ↓
hot code identified
    ↓
JIT compiles guest code
    ↓
JIT continues from the same architectural state
```

Both modes MUST share the same canonical VM architectural state:

```text
PC
SP
registers
status / flags
memory
MMU/address-space state
interrupt state
virtual clock
architecturally visible device state
pending faults/events
```

A mode switch MUST NOT reset, clone, reinterpret, or silently alter guest state.
Switching engines is an execution-engine change, not a machine reset.

The JIT must be able to yield at precise guest instruction boundaries so the interpreter can resume from an exact architectural state.
The interpreter must be able to hand execution to JIT from an exact architectural state without requiring a process or VM restart.

Required interoperability cases should eventually include:

- interpreter → JIT handoff
- JIT → interpreter handoff
- breakpoint during JIT execution
- guest fault/trap during JIT execution
- debugger single-step from JIT into interpreter
- snapshot while JIT mode is active
- restore followed by interpreter execution
- restore followed by JIT execution
- deterministic replay across engine switches
- device/interrupt events that cause an engine switch

Any JIT-private cached state must be transient execution state, never a second source of guest architectural truth.
If host-native execution cannot safely resume at a precise guest boundary, that limitation must be explicit in the VM/JIT contract rather than hidden.

The Reference Interpreter remains the semantic oracle.
# 12. JIT CORRECTNESS AND INTEROPERABILITY CONTRACT

Compare:

```text
Reference Interpreter
        vs
JIT
```

where meaningful.

Compare:

```text
registers
PC
SP
status / flags
memory
faults
device-visible state
virtual time
process-visible behavior
```

Also verify engine-switch boundaries:

```text
Interpreter state
      ↓
    handoff
      ↓
JIT starts from exactly that state

JIT state
      ↓
    handoff
      ↓
Interpreter starts from exactly that state
```

The handoff must preserve the canonical architectural state.
JIT-private caches or host register state must be synchronized or discarded before control returns to the interpreter.

Do not modify the Reference Interpreter to make JIT output match.

A semantic mismatch is a correctness defect.
A failed interpreter/JIT handoff is also a correctness defect.
# 13. JIT PERFORMANCE TARGET

The hardening phase has already demonstrated that frame-heavy generated code is extremely instruction-expensive.

Beyond Lazalith should investigate:

```text
register allocation
host register caching
basic-block compilation
compiled loops
constant folding
dead-code elimination where legal
call optimization
memory access optimization
```

Do not hide performance problems by increasing instruction limits.

Benchmark before and after optimization.

---

# 14. C IS A FIRST-CLASS LZA TARGET LANGUAGE

The toolchain must support C directly as an LZA target.

Eventually support:

```text
hosted C
freestanding kernel C
```

Pipeline:

```text
C source
  ↓
C frontend
  ↓
semantic analysis
  ↓
Lazalith IR
  ↓
LZA backend
  ↓
.lzo
  ↓
lazld
  ↓
.lzx
```

Do NOT create a C-specific executable format.

---

# 15. LAZALITH NATIVE ASSEMBLY IS A FIRST-CLASS LANGUAGE

Lazalith must have its own native assembly dialect.

This is assembly for **LZA**, not x86, ARM, or RISC-V syntax copied unchanged.

Pipeline:

```text
Lazalith ASM
   ↓
lexer
   ↓
parser
   ↓
semantic / operand validation
   ↓
authoritative LZA instruction definitions
   ↓
encoder
   ↓
.lzo
   ↓
lazld
   ↓
.lzx
```

The dialect should expose, where applicable:

```text
registers
instructions
labels
branches
calls
returns
stack operations
memory operands
immediates
symbols
sections
alignment
data
constants
relocations
extern/global
ABI directives
debug source mappings
```

Use one authoritative instruction-definition source for:

```text
CPU
decoder
encoder
assembler
disassembler
compiler backend
debugger
documentation
tests
```

---

# 16. C + ASM + LAZEN INTEROPERABILITY

All three frontend languages must converge into the SAME object pipeline:

```text
C ──────┐
        │
Lazen ──┼──→ .lzo → lazld → .lzx
        │
ASM ────┘
```

The same linker must handle:

```text
symbols
sections
relocations
ABI
entry points
debug metadata
```

across all three.

There must NOT be separate executable ecosystems for C, Lazen, and assembly.

## 16.1 LAZEN (`.lz`) MUST HAVE ITS OWN NATIVE SYSTEM

The `.lz` / Lazen environment must be a real target-side language/runtime ecosystem,
not a thin syntax layer over the host Rust implementation.

A Lazen program MUST NOT rely on host Rust runtime internals such as:

```text
rt::sys::*
Rust stdout/stderr internals
host process APIs
host filesystem APIs
host allocation APIs
host threading primitives
host GUI APIs
host libc
```

For example, Lazen `print`, file I/O, time, process, input, graphics, allocation,
and other system functionality must resolve through Lazalith/LazOS target interfaces
and the Lazen runtime/standard library, not through the Rust compiler's own runtime.

The host Rust implementation may IMPLEMENT the Lazen compiler, tooling, emulator,
and temporary bootstrap components, but host Rust APIs must not become the guest
program's hidden execution environment.

The intended relationship is:

```text
.lz source
   ↓
Lazen frontend
   ↓
Lazalith IR
   ↓
LZA code generation
   ↓
Lazen target runtime / standard library
   ↓
.lzo
   ↓
lazld
   ↓
.lzx
   ↓
Lazalith VM / LazOS
```

### Lazen language character

Lazen should be **stack-oriented underneath, but not look like raw Forth**.
Its execution model may use a data stack, stack effects, explicit words, and
composition in the spirit of Forth, while its source syntax should be substantially
more structured and readable.

The surface language should intentionally take useful ideas from:

```text
Forth  → stack-oriented execution, words, composition, explicit stack effects
Pascal → clear procedures/functions, readable declarations, structured blocks
Python → readable control flow, concise syntax, low ceremony
Rust   → strong types, explicit mutability, expressions, pattern matching, robust
          compile-time checking
```

Do NOT simply clone Forth, Pascal, Python, or Rust.

The goal is a distinct Lazen language with:

```text
Forth-like execution model
+
Pascal-like structured programming
+
Python-like readability
+
Rust-like type discipline
```

### Stack-oriented semantics

The compiler/runtime should preserve a coherent stack model. Where useful, a
function or word may expose an explicit stack effect such as:

```text
(n: i64 -- result: i64)
```

Primitive words such as `dup`, `swap`, and `drop` may exist, but ordinary Lazen
code should NOT require unreadable postfix programs for normal application code.

Named locals, typed parameters, infix expressions, structured conditionals, loops,
functions/procedures, and pattern matching should be first-class source constructs.

A representative style is:

```text
word square (n: i64 -- result: i64):
    n * n
end

fn main() -> i32:
    let value: i64 = 21
    print("answer = " + (value * 2).to_string())
    0
end
```

The exact grammar is still subject to repository research and language-design work.
The important architectural rule is that the syntax remains readable while the
compiler targets the stack-oriented Lazen/LZA execution model.

### Lazen-native system API

Define a Lazen-facing system layer for functionality such as:

```text
console / text output
filesystem
time
processes
input
graphics
resources
memory
errors
``

Those APIs must terminate at the LazOS ABI / target device contracts rather than
calling host Rust facilities.

The system layer must be testable headlessly and must have a documented distinction
between:

```text
Lazen language/runtime
LazOS system API
LZA device interfaces
host VM backends
```

A future host GUI, SDL3 backend, or Rust implementation must not leak into the `.lz`
programming model.

---

# 17. DEBUGGING ALL THREE LANGUAGES

The debugger should eventually support:

```text
C source
Lazen source
Lazalith assembly source
```

through one debug-information pipeline:

```text
source
  ↓
compiler / assembler
  ↓
debug metadata
  ↓
.lzo
  ↓
.lzx
  ↓
VM / JIT
  ↓
debugger
```

Where debug information exists, the debugger should map:

```text
source file
line
column
function
LZA instruction
guest PC
```

---

# 18. GCC / BINUTILS-LIKE TOOLCHAIN SEPARATION

Move toward distinct user-facing tools.

Possible names:

```text
lazcc
lazas
lazen
lazld
lazdbg
lazpkg
lazimg
lazalith
```

Inspect actual existing binaries before freezing these names.

Conceptual stages:

```text
source
 ↓
frontend
 ↓
Lazalith IR
 ↓
optimizer
 ↓
LZA backend
 ↓
.lzo
 ↓
lazld
 ↓
.lzx
```

Assembly independently produces `.lzo`.

Linking independently consumes `.lzo` and libraries and produces `.lzx`.

The compiler driver coordinates these stages instead of implementing a parallel linker or object system.

GCC's documentation is a reference for the separation between preprocessing, compilation, assembly, object output, and linking. [GCC overall options and compilation stages](https://gcc.gnu.org/onlinedocs/gcc/Overall-Options.html?utm_source=chatgpt.com)

---

# 19. TARGET SYSROOT

Design a target sysroot:

```text
sysroot/
  include/
  lib/
  crt/
  runtime/
```

Separate:

```text
compiler
C library
runtime
startup objects
OS headers
target libraries
```

The eventual toolchain should have a clear distinction between hosted programs and freestanding kernel builds.

---

# 20. REQUIRED OS TRANSITION — HOST RUST → TARGET C

This is a **core Beyond Lazalith objective**.

The current Phase-I OS implementation is integrated with the host Rust platform.

The eventual architecture MUST transition to:

```text
HOST
  ↓
Lazalith VM
  ↓
LZA CPU + Memory + Devices
  ↓
Firmware / Bootloader
  ↓
LazOS kernel binary
  ↓
userspace
```

The OS must become actual guest software.

It must NOT secretly execute through host Rust kernel logic.

The existing Phase-I Rust-integrated LazOS path MUST remain runnable during the
transition. Do not remove or break the Phase-I boot/run path until the target-side
LazOS implementation has an equivalent verified guest path and the migration is
explicitly documented. The old path may temporarily serve as a reference and
migration oracle.

---

# 21. LAZOS MUST EVENTUALLY BE WRITTEN IN C

The target-side LazOS kernel should eventually be implemented primarily in C.

Target build path:

```text
LazOS C source
      ↓
Lazalith C compiler
      ↓
.lzo
      ↓
lazld
      ↓
kernel.lzx
      ↓
firmware / bootloader
      ↓
Lazalith VM
      ↓
LazOS runs as guest code
```

Lazalith native assembly remains available for:

```text
boot
interrupt/trap entry
context switching
very-low-level CPU routines
hardware-specific code
architecture-specific paths
```

The target kernel must NOT depend on:

```text
host libc
host filesystem
host networking
SDL3
host threads
host allocation
host Rust kernel objects
```

unless a dependency is strictly build-time and not guest runtime behavior.

---

# 22. FREESTANDING C SUPPORT

Distinguish:

```text
hosted target C
```

from:

```text
freestanding LZA kernel C
```

The compiler/sysroot must eventually support a freestanding environment with:

```text
kernel headers
startup objects
linker layout
compiler runtime support
LZA ABI support
device headers
low-level runtime
```

without requiring hosted libc functionality.

---

# 23. C + ASM KERNEL INTEROPERABILITY

A future target-side kernel may contain:

```text
kernel.c
scheduler.c
memory.c
filesystem.c
drivers.c
...
boot.asm
trap.asm
context.asm
...
```

All produce:

```text
.lzo
```

Then:

```text
C objects
+
LZA assembly objects
+
libraries
      ↓
    lazld
      ↓
 kernel.lzx
```

One ABI.

One object format.

One linker.

One executable model.

---

# 24. KERNEL MIGRATION STRATEGY

Do NOT immediately delete the current Rust-integrated kernel.

Use a staged transition:

```text
A
Rust-integrated kernel remains the reference implementation

B
Define target-side kernel contracts

C
Implement C kernel subsystems

D
Run C LazOS on the LZA VM in parallel

E
Differentially compare guest-visible behavior

F
Move normal VM execution to target-side LazOS

G
Demote/remove the Rust kernel from the normal guest path
```

The old Rust implementation may remain temporarily as:

```text
reference model
migration oracle
test oracle
bootstrap support
```

but it must not remain the hidden implementation of final LazOS.

---

# 25. HARDWARE TIERS

Separate hardware into:

## Architectural Core

```text
CPU
MMU/address translation
memory
bus
interrupt controller
timers
virtual clock
DMA
reset/power model
firmware/boot interface
CPU topology/SMP
```

## Standard Computer Hardware

```text
display
keyboard
mouse
serial
RTC
block storage
network
audio
```

## Expansion / Compatibility

```text
USB
PCI/PCIe
VGA
IDE/ATA
NVMe
VirtIO-style devices
SoundBlaster
AC'97
HDA
TPM
legacy controllers
```

Do not implement everything immediately.

Document dependency ordering.

---

# 26. DEVICE FRONTEND / BACKEND MODEL

Design:

```text
Guest
 ↓
guest-visible device
 ↓
Lazalith device model
 ↓
host backend
```

Examples:

```text
Guest display
 ↓
Lazalith display device
 ↓
Rust SDL3 backend

Guest audio
 ↓
Lazalith audio device
 ↓
host audio backend

Guest block storage
 ↓
Lazalith block device
 ↓
raw/sparse/COW backend

Guest NIC
 ↓
Lazalith NIC
 ↓
NAT/host/tap backend
```

This separation is inspired by QEMU's machine/device/backend architecture, but the actual APIs must be Lazalith-specific.

QEMU documents system emulation, machine models and device frontend/backend separation as distinct concepts. [QEMU system emulation documentation](https://www.qemu.org/docs/master/system/introduction.html?utm_source=chatgpt.com) [QEMU device emulation documentation](https://www.qemu.org/docs/master/system/device-emulation.html?utm_source=chatgpt.com)

---

# 27. MACHINE PROFILES

Design versioned profiles:

```text
lza64-virt-v1
lza64-native-v1
lza64-at-v1
```

A profile describes:

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

Profiles must be versioned to preserve guest compatibility.

---

# 28. DISPLAY / VGA

Retain the native Lazalith display abstraction.

Add a future compatibility architecture:

```text
native Lazalith display
VGA-compatible display
future modern framebuffer/display profile
```

The guest sees a guest-visible display device.

The host may use:

```text
Rust sdl3
```

as the rendering backend.

The guest never depends on SDL3.

For Linux 0.01, VGA/EGA support is particularly relevant and must be researched from historical primary sources before implementation.

---

# 29. AUDIO

Audio is a standard VM device direction.

Investigate:

```text
PCM playback
PCM capture
sample rate
sample format
channel count
buffers/rings
DMA
interrupts
```

Host backends must be independent of the guest ABI.

Compatibility devices such as:

```text
SoundBlaster
AC'97
HDA
```

are future compatibility-machine work.

---

# 30. INPUT / USB

Separate:

```text
guest keyboard/mouse/controller
```

from:

```text
host input backend
```

Investigate:

```text
PS/2
USB HID
modern virtual input
```

USB architecture:

```text
USB controller
 ↓
USB bus
 ↓
USB device
 ↓
host backend
```

---

# 31. STORAGE

Design:

```text
guest block device
 ↓
controller/device model
 ↓
host storage backend
```

Investigate:

```text
raw images
sparse images
copy-on-write
snapshot layers
```

and guest controllers:

```text
IDE/ATA
VirtIO block
NVMe
```

Do not implement all at once.

---

# 32. NETWORKING

Design:

```text
Guest NIC
 ↓
Lazalith NIC
 ↓
host backend
```

Possible host backends:

```text
NAT
user-mode
bridged
host-only
tap/socket
```

These remain host-side implementation details.

---

# 33. EXPANSION BUS

Design a device-discovery mechanism.

Investigate:

```text
device identifiers
configuration
MMIO
interrupt routing
DMA
bus topology
```

A native Lazalith expansion bus may come before PCI.

PCI/PCIe can later become a compatibility profile.

---

# 34. FIRMWARE / BOOT

Separate:

```text
VM Manager
 ↓
Machine Profile
 ↓
Firmware
 ↓
Bootloader
 ↓
LazOS
```

Potential future firmware layers:

```text
minimal Lazalith firmware
BIOS-like compatibility firmware
UEFI-like future firmware
```

Do not implement them during this architecture pass.

---

# 35. VM MANAGER / VIRTUALBOX-LIKE EXPERIENCE

Beyond Lazalith should eventually have a dedicated VM management layer.

It should manage:

```text
VM creation
VM configuration
machine profile
CPU configuration
RAM configuration
storage
network
audio
USB
display
boot
start
pause
resume
reset
shutdown
snapshot
restore
clone
debugger attachment
console
```

The GUI and CLI consume this management API.

Neither directly manipulates CPU internals.

VirtualBox's separation between VM management and virtual hardware is an external reference for this design. Use current Oracle documentation to research exact concepts rather than copying its implementation.

---

# 36. GUI / RUST SDL3

The Phase-I GUI is a host frontend.

Beyond Lazalith should make it:

```text
VM Manager API
      ↑
   ┌──┴───┐
   CLI   GUI
```

The GUI should use the **Rust `sdl3` crate** as the preferred SDL3 integration.

Desired path:

```text
Lazalith GUI
    ↓
Rust `sdl3`
    ↓
SDL3
```

NOT a large handwritten C FFI layer.

Before migration:

1. Read the current SDL3 wrapper.
2. Use LSP to find all callers.
3. Enumerate all SDL functions currently used.
4. Map them to Rust `sdl3`.
5. Identify APIs not covered.
6. Decide whether a very small wrapper remains necessary.
7. Verify the unsafe surface.
8. Document the migration strategy.

Do not blindly delete existing code.

The Rust `sdl3` crate is the preferred direction because it provides a higher-level Rust interface while `sdl3-sys` supplies the lower-level bindings. Verify feature coverage against the actual project needs. [Rust sdl3 documentation](https://docs.rs/sdl3/latest/?utm_source=chatgpt.com)

---

# 37. TOOLCHAIN / VM SEPARATION

The mature ecosystem should eventually look like:

```text
Developer side

lazcc
lazas
lazen
lazld
lazdbg
lazpkg
lazimg

        ↓

Target artifacts

.lzo
.lzx

        ↓

Execution side

lazalith
VM manager
Reference Interpreter
JIT
machine profiles
device models
host backends
```

The compiler must work without running a VM.

The assembler must work without running a VM.

The linker must work without running a VM.

The VM executes the resulting artifacts.

---

# 38. GITHUB CI/CD AND REPOSITORY AUTOMATION

GitHub CI/CD is a **first-class part of the Lazalith engineering architecture**.

It must not become a collection of unrelated convenience workflows. The automation must continuously validate the same architectural contracts that make Lazalith reproducible, modular, and testable.

## 38.1 Source of truth

The Git repository remains authoritative. GitHub automation must build/test the repository itself and must not depend on an author's private machine state.

Nix should be used wherever practical to provide the reproducible toolchain/environment already established by the project.

## 38.2 Pull-request CI

The PR validation path should eventually verify, as applicable:

```text
cargo fmt --check
cargo check --workspace
cargo clippy --workspace --all-targets --all-features
cargo test --workspace
nix flake check
nix build
documentation / consistency checks
integration tests
architecture/invariant checks
toolchain/object/linker tests
VM / emulator tests
cross-language pipeline tests
```

Do not reduce pull-request validation to compilation only.

Do not add expensive jobs merely for appearance; measure and separate fast PR checks from deeper validation when necessary.

## 38.3 Layered CI

Prefer logically separated workflows/jobs such as:

```text
fast validation
    ↓
workspace tests
    ↓
architecture / integration validation
    ↓
Nix reproducibility/build validation
    ↓
artifact/release validation
```

Use dependency relationships between jobs rather than duplicating the same command in many workflows.

## 38.4 Architecture-aware CI

CI must eventually catch defects that ordinary Rust compilation cannot detect. Test coverage should include, where implemented:

```text
ISA semantic correctness
Reference Interpreter correctness
execution-engine equivalence
memory invariants
device contracts
ABI compatibility
.lzo compatibility
.lzx compatibility
assembler/decoder round-trips
linker relocations
C → LZA → .lzo → .lzx
ASM → LZA → .lzo → .lzx
Lazen → IR → LZA → .lzo → .lzx
snapshot/replay determinism
machine-profile compatibility
LazOS guest boot
debug metadata
```

Where a later JIT exists, differential tests against the Reference Interpreter must be available to CI.

## 38.5 Test tiers

Organize tests into appropriate tiers rather than making every PR run every expensive experiment.

Possible tiers:

```text
Tier 1 — formatting / static validation / fast unit tests
Tier 2 — full workspace + integration tests
Tier 3 — Nix reproducibility / full builds
Tier 4 — fuzz/property/differential/regression campaigns
Tier 5 — release validation
```

The exact split must follow measured project runtime and actual repository structure.

## 38.6 Artifacts

CI should preserve useful build outputs from successful validation jobs where appropriate, including future:

```text
host binaries
toolchain binaries
.lzo objects
.lzx executables
VM images
boot artifacts
checksums
test reports
benchmark reports where useful
```

Do not commit generated build output to the source tree merely because CI produces it.

## 38.7 Release automation

Design a tag-driven release pipeline capable of eventually producing GitHub Releases containing reproducible Lazalith artifacts.

A future release should be derived from an exact Git commit/tag and should include:

```text
version
source revision
build metadata
platform artifacts
checksums
release notes
```

Do not create a release claiming that an artifact was produced by CI unless CI actually produced and verified it.

Do not force-push release tags.

## 38.8 Dependency and security automation

Investigate and document appropriate use of:

```text
Dependabot
Cargo dependency checks
GitHub security scanning
secret detection
workflow permission hardening
minimal GITHUB_TOKEN permissions
```

Only enable mechanisms that fit the repository and verify their behavior.

Do not grant broad workflow permissions unnecessarily.

## 38.9 Repository governance

Research and document appropriate use of:

```text
CODEOWNERS
PR templates
issue templates
required status checks
protected main branch
release/tag policy
```

Do not assume repository administration settings can be changed merely by editing files; clearly distinguish repository-file automation from GitHub-side settings.

## 38.10 CI must remain reproducible

Avoid workflows that silently depend on:

```text
local caches
undeclared host libraries
developer home directories
private credentials
unstated environment variables
mutable external branches
```

External services may be used only where required and their provenance/configuration must be documented.

## 38.11 CI/CD is cross-cutting, not a separate product

CI/CD must validate every mature layer of Beyond Lazalith as that layer becomes implemented.

The intended relationship is:

```text
LZA / ISA
  ↓
VM Core / execution engines
  ↓
machine profiles / devices
  ↓
toolchain / object / linker
  ↓
target-side OS
  ↓
VM manager / host frontends
  ↓
GitHub CI/CD
```

GitHub automation observes and validates these layers; it does not become part of guest execution semantics.

## 38.12 CI documentation

Create or update a dedicated repository document, preferably:

```text
docs/ci-cd.md
```

It should document:

```text
workflow inventory
trigger rules
job responsibilities
commands executed
Nix/Cargo relationship
cache strategy
artifact strategy
release strategy
security/permissions
required GitHub settings
local equivalents
known limitations
```

The document must distinguish what already exists from what is planned.

# 39. DEBUGGER

Keep the debugger above the VM abstraction:

```text
lazdbg
  ↓
VM debug API
  ↓
VM Core
```

It must not directly depend on CPU implementation arrays or device internals.

Support future debug mapping for:

```text
C
Lazen
Lazalith assembly
```

through common debug metadata.

---

# 40. SNAPSHOT / REPLAY / MIGRATION

Promote snapshots to VM-level infrastructure.

Investigate state coverage:

```text
CPU
memory
devices
virtual clock
interrupt state
machine configuration
storage state
network state
debug state
```

Future capabilities:

```text
snapshot
restore
clone
replay
migration
```

Document which host-backed devices are deterministic/snapshot-safe.

---

# 41. LINUX 0.01 — AUTHORITATIVE SOURCE

The eventual Linux 0.01 port MUST use this repository as its source reference:

**https://github.com/zavg/linux-0.01.git**

This is the authoritative Linux 0.01 source tree for the porting project.

Do not substitute another Linux 0.01 tree without documenting why.

---

# 42. LINUX 0.01 SOURCE PINNING

When porting begins:

```bash
git clone https://github.com/zavg/linux-0.01.git
```

Then:

1. inspect the repository history
2. choose the exact commit used
3. record that commit in Lazalith documentation
4. pin the port to that revision
5. do not track a moving branch for reproducible builds

Do not rewrite the upstream repository.

Maintain clear provenance between upstream Linux 0.01 and Lazalith modifications.

---

# 43. LINUX 0.01 — READ THE REAL SOURCE

Before implementing the compatibility machine or port:

Use OpenCode Read and WebSearch against the actual repository.

Inspect at minimum:

```text
boot/
boot/boot.s
boot/head.s

init/
init/main.c

kernel/
kernel/sched.c
kernel/system_call.s
kernel/traps.c
kernel/asm.s

mm/
mm/memory.c

fs/
fs/*.c

include/
include/linux/*
include/asm/*

tools/
Makefile
```

Search the whole repository for additional architecture assumptions.

Do not infer dependencies from secondary summaries.

---

# 44. LINUX 0.01 ARCHITECTURE PORT

Do NOT turn LZA into an x86 emulator.

The goal is:

```text
Original Linux 0.01
        ↓
identify x86/AT-specific assumptions
        ↓
LZA architecture adaptation
        ↓
LZA64
```

This is a real architecture port.

---

# 45. LINUX 0.01 CPU / ARCHITECTURE INVENTORY

Identify all dependencies on:

```text
x86 registers
segment registers
GDT
IDT
CR0
CR2
CR3
protected mode
A20
x86 interrupt/trap semantics
x86 calling conventions
inline assembly
i386-specific instructions
```

Do not simply pass them through the LZA compiler.

Determine whether each becomes:

```text
LZA equivalent
LZA-specific kernel code
software implementation
removed because unnecessary
```

The exact decision must be documented.

---

# 46. LINUX 0.01 DEVICE INVENTORY

Identify actual hardware assumptions from the upstream source.

Build an inventory of:

```text
display
keyboard
storage
serial
timer
interrupts
memory
boot
other hardware
```

Then map:

```text
Linux expectation
      ↓
LZA compatibility interface
      ↓
lza64-at-v1
```

Do not assume a device is required merely because it existed on the historical PC.

Prove the dependency from source.

---

# 47. LINUX 0.01 BOOT PORT

Treat the historical boot path as architecture-specific.

Identify assumptions involving:

```text
real mode
A20
protected mode
GDT
IDT
x86 paging
control registers
x86 interrupt setup
```

Then map those requirements onto:

```text
LZA firmware
LZA bootloader
LZA kernel entry
LZA memory architecture
LZA interrupts
LZA MMU
```

Do not reproduce x86 boot semantics unnecessarily.

---

# 48. LINUX 0.01 C + LZA ASM

The eventual kernel must be buildable using the Lazalith toolchain:

```text
Linux 0.01-derived C
      ↓
Lazalith C compiler

LZA architecture assembly
      ↓
Lazalith assembler
```

Then:

```text
C objects
+
LZA ASM objects
+
runtime/startup
      ↓
.lzo
      ↓
lazld
      ↓
Linux-0.01-derived LZA kernel image
```

Do not make GCC the required target compiler.

Host GCC may be used for comparison/reference experiments where useful.

The final LZA kernel should be buildable using Lazalith's own C + ASM toolchain.

---

# 49. LINUX 0.01 PRESERVATION / PROVENANCE

When porting:

- preserve recognizable upstream structure
- isolate architecture-specific LZA changes
- mark significant porting changes
- preserve attribution
- retain provenance
- do not pretend modified files are upstream originals

Prefer:

```text
upstream source
+
explicit LZA architecture layer
```

over an untraceable rewrite.

---

# 50. LINUX 0.01 PORT DOCUMENT

Before implementation begins, create:

`docs/linux-0.01-port.md`

It must contain:

```text
upstream URL
exact pinned commit
original architecture assumptions
CPU assumptions
memory assumptions
boot assumptions
interrupt/trap assumptions
timer assumptions
storage assumptions
display assumptions
keyboard assumptions
serial assumptions
filesystem assumptions
compiler assumptions
assembly assumptions
ABI assumptions
LZA replacements
lza64-at-v1 requirements
remaining unknowns
```

For every significant item, state whether it comes from:

```text
upstream source
Lazalith architecture
external research
porting inference
future design
```

---

# 51. HARDWARE COMPATIBILITY PRIORITY

A mature computer-oriented LZA machine should eventually investigate at least:

```text
RAM
interrupt controller
timer
RTC
display
keyboard
mouse
serial
block storage
network
audio
```

Then expansion/compatibility:

```text
USB
PCI/PCIe
VGA
IDE/ATA
NVMe
VirtIO
```

Prioritize by real guest/software requirements.

Do not implement peripherals merely because they exist on a checklist.

---

# 52. TARGET-SIDE LAZOS BOOTSTRAP

The eventual bootstrap target is:

```text
Lazalith C compiler
        ↓
LazOS C kernel
        ↓
LZA objects
        ↓
lazld
        ↓
kernel.lzx
        ↓
LZA firmware/bootloader
        ↓
Lazalith VM
        ↓
LazOS
        ↓
userspace
```

Lazalith assembly remains the low-level escape hatch.

---

# 53. BEYOND LAZALITH ROADMAP — EASY → HARDEST

Use this as a planning framework, subject to actual repository research.
The ordering is intentionally arranged from lower-complexity foundations toward progressively harder and more invasive platform work.
Dependency order takes precedence over superficial feature size.

```text
B1   LZA architecture naming / contract freeze
B2   GitHub CI/CD and repository automation foundation
B3   VM Core / LVMI boundary extraction
B4   machine profiles
B5   device frontend/backend model separation
B6   common VM lifecycle / reset / boot contracts
B7   native display architecture
B8   storage architecture
B9   input architecture
B10  audio architecture
B11  networking architecture
B12  expansion bus / device discovery
B13  firmware / boot profiles
B14  GCC-like toolchain separation
B15  sysroot / runtime / packaging separation
B16  native Lazen (`.lz`) language/runtime/system boundary
B17  debugger integration through the VM API
B18  advanced snapshot / replay state model
B19  VM manager / management API
B20  VM GUI / Rust SDL3 migration boundary
B21  optimized interpreter / execution-engine abstractions
B22  JIT execution engine
B23  interpreter ↔ JIT interoperability and state handoff
B24  differential JIT verification / deterministic engine-switch testing
B25  target-side LazOS transition and freestanding C environment
B26  LazOS built and booted by the Lazalith C + LZA ASM toolchain
B27  LZA AT compatibility machine
B28  Linux 0.01 architecture port
B29  Linux 0.01 boot milestone
```

The exact implementation sequence may be adjusted after repository research, but the final plan must preserve the dependency relationship:

```text
architecture
→ automation
→ VM abstraction
→ machine/device foundations
→ host-facing platform services
→ toolchain/runtime infrastructure
→ native Lazen system/runtime
→ debugger/snapshot infrastructure
→ optimized execution
→ JIT
→ interpreter/JIT interoperability
→ target-side OS
→ compatibility machine
→ Linux 0.01 architecture port
```

The JIT and Interpreter are never treated as isolated products. They are interchangeable execution modes of the same VM and must be able to hand execution back and forth while preserving one canonical guest architectural state.

---

# 54. MASTER DOCUMENTS

Create or update:

```text
docs/architecture.md
docs/lza64.md
docs/beyond-lazalith.md
docs/virtual-machine.md
docs/device-model.md
docs/machine-profiles.md
docs/toolchain.md
docs/ci-cd.md
docs/compatibility.md
docs/linux-0.01-port.md
docs/ci-cd.md
```

Also add a clearly separated section to:

`instruction.md`

with the heading:

```text
# BEYOND LAZALITH
```

Do not erase the historical Phase-I roadmap.

---

# 55. EXTERNAL DESIGN REFERENCES

The master documents must contain an:

**External design references**

section.

At minimum research and cite:

## QEMU

System emulation:

https://www.qemu.org/docs/master/system/introduction.html

Device emulation:

https://www.qemu.org/docs/master/system/device-emulation.html

Use these for research into:

- machine architecture
- CPU/execution engine separation
- machine types
- accelerators
- devices
- buses
- guest-facing device model
- host backend separation

## GCC

Compilation stages:

https://gcc.gnu.org/onlinedocs/gcc/Overall-Options.html

Driver behavior:

https://gcc.gnu.org/onlinedocs/gcc/Invoking-GCC.html

Use these for research into:

- compiler-driver behavior
- preprocessing
- compilation
- assembly
- object output
- linking
- toolchain boundaries

## VirtualBox

Use current Oracle documentation for:

- VM management
- virtual hardware
- storage
- networking
- USB
- sound
- display

## Rust SDL3

Use:

https://docs.rs/sdl3/latest/

as the primary reference for the Rust SDL3 integration.

Verify actual feature coverage against Lazalith's existing SDL usage.

## GitHub

Use current official GitHub documentation for GitHub Actions, workflow syntax, artifacts, releases, environments, Dependabot, CODEOWNERS, and repository security/permissions:

https://docs.github.com/en/actions

Treat GitHub documentation as the authority for GitHub platform behavior; verify any current feature/setting before documenting it as available.

## Linux 0.01

Use:

**https://github.com/zavg/linux-0.01.git**

as the authoritative source tree for the eventual port.

Do not substitute another source tree without explicit documentation.

---

# 56. FACT / PROPOSAL SEPARATION

Every important master-document statement must clearly distinguish:

```text
Existing Lazalith fact
External documented fact
Lazalith requirement
Proposed design
Future implementation
```

The same distinction applies to CI/CD: do not claim a workflow, required status check, release automation, dependency bot, security scan, or repository protection rule is active unless the repository or GitHub-side configuration actually proves that it is.

Never describe a proposal as implemented.

Never describe QEMU/VirtualBox behavior as a Lazalith requirement without explaining why Lazalith adopts it.

Never claim:

- JIT exists
- target-side C LazOS exists
- VGA compatibility exists
- Linux 0.01 runs
- VM manager exists

unless the repository actually contains the working implementation.

---

# 57. REQUIRED SDL3 MIGRATION INVESTIGATION

As part of the architecture pass, investigate replacing the handwritten SDL3 FFI boundary with Rust `sdl3`.

Use:

```text
OpenCode Read
rust-analyzer/LSP
OpenCode WebSearch
docs.rs
repository search
```

Inspect:

```text
current lazalith-sdl3 implementation
all callers
all unsafe blocks
all SDL functions used
window handling
rendering
textures
input
timing
events
error handling
ABI assumptions
```

Map each operation to Rust `sdl3`.

Document:

```text
directly replaceable
needs compatibility wrapper
not currently supported
should remain custom
unsafe removable
unsafe still necessary
```

Do not blindly delete `lazalith-sdl3`.

Do not implement the migration during this architecture-only session unless the current repository roadmap specifically requires it.

---

# 58. FINAL ARCHITECTURE DECISION RECORD

Produce a concise ADR covering:

```text
LZA32 / LZA64
VM Core / LVMI
Reference Interpreter
JIT
machine profiles
device model
storage
display
VGA
audio
input
USB
networking
expansion bus
firmware
boot
VM manager
GUI
Rust SDL3
C compiler
native LZA assembly
common .lzo / .lzx
sysroot
debugger
snapshots
replay
migration
target-side LazOS
C + ASM kernel build
Linux 0.01 compatibility
GitHub CI/CD and repository automation
```

Every decision must identify whether its basis is:

```text
existing repository
external research
Lazalith requirement
proposed design
future work
```

---

# 59. DO NOT IMPLEMENT PHASE II IN THIS TASK

This is documentation/research only for the Beyond Lazalith guest/platform architecture.

**GitHub CI/CD and repository automation are explicitly in scope** because they are part of the project's engineering foundation. They may be inspected, documented, and established where required without implementing the Beyond guest/hardware features below.

Do NOT implement:

- JIT
- register allocator
- VGA
- audio
- USB
- PCI/PCIe
- networking
- VM manager
- new GUI architecture
- target-side C LazOS
- Linux 0.01 port
- broad SDL3 migration

The output is the architecture that the later implementation sessions will follow.

---

# 60. FINAL CONSISTENCY CHECK

Before committing, verify:

```text
LZA naming is consistent
LZ32/LZ64 terminology is explained
VM Core boundary is explicit
Reference Interpreter remains authoritative
JIT is a separate execution engine
JIT and Interpreter share one canonical VM architectural state
Interpreter → JIT handoff requires no VM restart
JIT → Interpreter handoff requires no VM restart
JIT can yield at precise guest instruction boundaries
breakpoints/faults/debugging can return JIT execution to the Interpreter
snapshots/restores remain valid across execution-engine modes
replay remains deterministic across execution-engine switches
JIT-private state cannot become a second architectural source of truth
C compiler is independent of VM execution
Lazalith ASM is a first-class frontend
C + ASM + Lazen share .lzo/.lzx
toolchain and VM are separate
`.lz` is a real Lazen target language, not a host-Rust runtime wrapper
Lazen has its own target runtime/system API and standard-library boundary
Lazen system APIs do not depend on Rust `rt::sys::*` or host runtime facilities
Lazen is stack-oriented internally while using a structured Pascal/Python/Rust-inspired surface syntax
Lazen stack effects and low-level words remain available without forcing raw Forth syntax on ordinary code
machine profiles are versioned
device frontend/backend boundary is explicit
host backends are not guest interfaces
SDL3 remains host-only
Rust sdl3 is the preferred SDL3 binding
audio/display/network/storage are guest device abstractions
VGA is compatibility hardware, not the LZA ISA
USB/PCI are expansion/compatibility infrastructure
target-side LazOS is real future guest software
LazOS is eventually compiled by Lazalith's own C compiler
C and LZA ASM can cooperate in one kernel image
Linux 0.01 is a real future architecture port
Linux 0.01 source is pinned to zavg/linux-0.01.git
GitHub CI/CD is treated as cross-cutting engineering infrastructure
CI validates the same core architectural contracts used locally
release automation is tag/revision based
future features are not claimed as implemented
external research is cited
repository facts and proposals are separated
```

---

# 61. GIT

Commit architecture/documentation changes and, where established by this pass, the repository CI/CD configuration that implements the documented engineering foundation.

Do not commit Phase-II guest/hardware implementations merely to exercise CI.

Where GitHub workflow files are changed, validate their commands/configuration as far as the available environment permits and clearly distinguish local validation from GitHub-side execution.


Suggested commit:

```text
Define Beyond Lazalith architecture, VM platform, and CI/CD foundation
```

Do not rewrite Phase-I history.

Leave the tree clean.

Do not begin Phase-II implementation in this session.

---

# FINAL RESPONSE

Report:

- formal architecture name
- why LZA is the architecture identity
- VM Core/LVMI
- execution-engine model, including Interpreter ↔ JIT interoperability
- JIT direction
- C compiler direction
- native `.lz` / Lazen language direction
- Lazen-native runtime/system API and its separation from host Rust
- native Lazalith assembly direction
- shared `.lzo` / `.lzx` ecosystem
- machine profiles
- device model
- storage/display/VGA/audio/input/USB/network roadmap
- VirtualBox-like VM manager
- Rust `sdl3` direction
- target-side C LazOS transition
- C + assembly kernel model
- Linux 0.01 source repository and pinning strategy
- Linux 0.01 port strategy
- master documents created/updated
- external sources researched
- GitHub CI/CD design and automation status
- workflows/jobs introduced or intentionally deferred
- release/artifact/dependency automation decisions
- final commit

Then STOP.

Do not implement Beyond Lazalith in this session.