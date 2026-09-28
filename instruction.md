# LAZALITH — OPENCode IMPLEMENTATION INSTRUCTIONS

You are implementing **Lazalith**, a complete custom computer platform.

Lazalith consists of:

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
Lazen Language
    ↓
Applications
```

The project also includes:

```text
Rust Toolchain
Assembly
C compiler
Lazen compiler
Debugger
SDL3 GUI
Nix Flake
```

The entire project is implemented in **Rust**.

The GUI uses **SDL3**.

The development environment uses **Nix Flakes**.

---

# HOW YOU MUST WORK

This is an implementation roadmap.

**Do not implement everything at once.**

Follow the steps in order.

However, the steps are **dependency-aware**, not artificially "easy first".

Sometimes a difficult component must be partially implemented because an easier
component depends on it.

For example:

```text
Diagnostics
```

must exist early because the assembler/compiler/OS will need it.

Likewise:

```text
ISA
ABI
Object Format
```

must exist before the compiler can become useful.

---

# RULE FOR EVERY STEP

For every step:

```text
1. Inspect the current repository.
2. Read the relevant existing code.
3. Determine what this step depends on.
4. Implement only what this step requires.
5. Write tests immediately.
6. Run cargo fmt.
7. Run cargo clippy.
8. Run cargo test.
9. Run relevant Nix checks.
10. Fix failures.
11. Update documentation.
12. Verify error handling.
13. Only then move to the next step.
```

Do not skip tests because the implementation is "small".

Do not continue while the current step is fundamentally broken.

---

# RULE: NEVER REWRITE WORKING CODE WITHOUT REASON

If the repository already contains useful functionality:

- inspect it
- understand it
- reuse it when compatible
- refactor carefully

Do not delete working functionality merely because the current implementation is
different from your preferred design.

---

# RULE: DO NOT OVER-ENGINEER EARLY

Use strong abstractions where they protect architecture.

Do not create abstraction layers that have no purpose.

The goal is:

```text
easy to reason about
hard to misuse
easy to test
easy to extend
```

---

# RULE: CORE MUST BE INDEPENDENT FROM SDL3

The CPU, memory, bus, machine, ISA, OS logic, compiler, assembler, and linker
must not depend on SDL3.

SDL3 belongs to the host/frontend layer.

Correct:

```text
Lazalith Core
    ↑
SDL3 Frontend
```

Incorrect:

```text
CPU
 ↓
SDL3
```

The emulator must always be able to run headlessly.

---

# RULE: STRONG TYPES

Do not use raw integers everywhere.

Where appropriate, create types such as:

```text
Address
PhysicalAddress
VirtualAddress
InstructionAddress
RegisterIndex
DeviceId
DeviceOffset
SectionOffset
FileOffset
CycleCount
InstructionCount
ProcessId
ThreadId
FileHandle
PageNumber
```

Do not make everything a `u64`.

The purpose is to prevent accidental mixing of unrelated values.

---

# RULE: NO GLOBAL MACHINE STATE

Do not use global mutable state for:

```text
CPU
RAM
devices
machine
kernel
processes
compiler state
```

Ownership must remain explicit.

Do not solve architecture problems by turning the entire machine into:

```text
Arc<Mutex<Machine>>
```

unless there is a real, documented reason.

---

# RULE: NO DIRECT INTERNAL ACCESS

Other layers must not directly manipulate:

```text
CPU register arrays
RAM byte arrays
device internals
kernel internals
```

Use APIs.

---

# RULE: NO HIDDEN MUTATION

Functions such as:

```text
decode
peek
inspect
```

must not unexpectedly mutate state.

Explicitly distinguish:

```text
fetch
read
write
peek
```

when their behavior differs.

---

# RULE: VALIDATE BEFORE MUTATION

Prefer:

```text
validate
 ↓
calculate
 ↓
commit
```

rather than:

```text
mutate
 ↓
discover error
```

This is particularly important for:

```text
CPU instructions
stack operations
traps
memory operations
linker relocations
program loading
kernel operations
```

---

# RULE: STRUCTURED ERRORS

Do not use:

```rust
Err("something went wrong")
```

throughout the project.

Use structured error types and a shared diagnostics system.

Errors should retain their cause.

---

# RULE: PRECISE DIAGNOSTICS

Whenever a failure originates from source code, preserve:

```text
file
line
column
source span
error code
message
notes
help
```

Whenever possible preserve the path:

```text
source
 ↓
AST
 ↓
IR
 ↓
machine instruction
 ↓
executable
 ↓
guest PC
 ↓
runtime/emulator error
```

Internal emulator bugs should also report useful Rust source context:

```text
crate
file
line
column
subsystem
operation
```

---

# STEP 1 — INSPECT THE REPOSITORY

Do not modify implementation code yet.

Determine:

```text
What files exist?
Is Cargo already configured?
Is Nix already configured?
Is there an existing emulator?
Is there existing ISA code?
Is there existing GUI code?
```

Create:

```text
docs/project-state.md
```

Document the current situation.

### DONE WHEN

You understand the existing repository and have documented it.

---

# STEP 2 — CREATE THE RUST WORKSPACE

Create or clean up the Cargo workspace.

Start with only the foundations needed now.

At minimum eventually provide space for:

```text
lazalith-types
lazalith-diagnostics
```

Do not create every future crate immediately.

### DONE WHEN

This works:

```bash
cargo build
cargo test
cargo fmt
cargo clippy
```

---

# STEP 3 — CREATE THE NIX FLAKE

Create:

```text
flake.nix
flake.lock
```

The development shell must provide the tools needed by the project.

Include appropriate:

```text
Rust
Cargo
rustfmt
clippy
rust-analyzer support
SDL3 development dependencies
linker/build tools
testing tools
```

Provide:

```bash
nix develop
nix build
nix flake check
```

### DONE WHEN

A clean development environment can enter:

```bash
nix develop
```

and successfully build/test the project.

---

# STEP 4 — CREATE THE DIAGNOSTIC SYSTEM

Create:

```text
lazalith-diagnostics
```

This will eventually be shared by:

```text
assembler
compiler
linker
loader
kernel
debugger
CLI
GUI
emulator
```

Do not create separate error-rendering systems for every component.

---

# STEP 5 — CREATE SOURCE LOCATION TYPES

Implement:

```text
SourceId
SourceFile
SourceManager
SourceSpan
ByteOffset
```

A source span must be able to resolve to:

```text
filename
line
column
end line
end column
```

Use one authoritative source-map implementation.

### TEST

Create tests that verify exact line/column calculation.

### DONE WHEN

A span such as:

```text
SourceId + start + end
```

can reliably produce:

```text
file.lz:42:17
```

---

# STEP 6 — CREATE STRUCTURED DIAGNOSTICS

Implement concepts similar to:

```text
Diagnostic
Severity
DiagnosticCode
Label
Note
Help
Cause
```

A diagnostic should be data, not preformatted terminal text.

### DONE WHEN

You can generate something like:

```text
error[E1001]: example failure
 --> example.lz:4:8
  |
4 | hello
  | ^^^^^
  |
  = help: example
```

from structured data.

---

# STEP 7 — CREATE SHARED ARCHITECTURAL TYPES

Implement strong types needed across the platform.

Examples:

```text
Address
PhysicalAddress
VirtualAddress
InstructionAddress
RegisterIndex
DeviceId
DeviceOffset
CycleCount
InstructionCount
```

Add conversions only when they are semantically valid.

### DONE WHEN

Important architectural concepts no longer require arbitrary `u64`s everywhere.

---

# STEP 8 — DESIGN THE LAZALITH ISA

Do not write the whole CPU yet.

First write:

```text
docs/isa.md
docs/lz32.md
docs/lz64.md
```

Decide:

```text
register count
register names
PC
SP
flags/status
instruction encoding
endianness
alignment
memory model
trap model
privilege model
```

Design a genuinely custom ISA.

Do not simply reproduce:

```text
x86
ARM
MIPS
RISC-V
```

---

# STEP 9 — DEFINE LZ32 AND LZ64

Create:

```text
ArchitectureConfig
WordWidth
FeatureSet
```

Support:

```text
LZ32
LZ64
```

from one architectural model.

Do not create two completely separate CPUs.

Document:

```text
register width
pointer width
address width
stack behavior
data sizes
ABI differences
```

### DONE WHEN

The architecture can answer:

```text
What changes between LZ32 and LZ64?
```

without relying on scattered implementation conditionals.

---

# STEP 10 — CENTRALIZE WIDTH OPERATIONS

Implement shared helpers/types for:

```text
truncation
zero extension
sign extension
address masking
wrapping
overflow
shift width
```

Do not allow each instruction to implement these rules differently.

### DONE WHEN

Width semantics are defined in one architectural location.

---

# STEP 11 — DEFINE INSTRUCTIONS AS STRUCTURED DATA

Create:

```text
Opcode
InstructionDefinition
InstructionFormat
OperandKind
Instruction
Operand
```

Make instruction metadata reusable by:

```text
decoder
encoder
assembler
disassembler
compiler backend
documentation
tests
```

### DONE WHEN

The same instruction definition can be referenced by multiple tools.

---

# STEP 12 — IMPLEMENT THE REGISTER FILE

Create:

```text
RegisterFile
```

Do not expose the internal array.

Handle:

```text
read
write
validation
special registers
width behavior
```

### DONE WHEN

Invalid register access is rejected cleanly.

---

# STEP 13 — IMPLEMENT CPU STATE

Create a clean separation between:

```text
Architectural CPU State
Execution State
Debug State
```

Architectural state should contain only guest-visible CPU state.

Debugger metadata must not become CPU state.

---

# STEP 14 — IMPLEMENT STATUS / FLAGS

Create:

```text
StatusRegister
```

Centralize flag operations.

Arithmetic instructions must use shared flag logic where appropriate.

### DONE WHEN

ADD/SUB/etc. do not each invent separate flag semantics.

---

# STEP 15 — IMPLEMENT EXECUTION OUTCOMES

Create something similar to:

```text
ExecutionOutcome
```

with:

```text
Continue
Jump
Call
Return
Trap
Halt
```

The main CPU loop applies the outcome.

This prevents inconsistent PC handling.

---

# STEP 16 — IMPLEMENT THE REFERENCE CPU

Create:

```text
ReferenceInterpreter
```

It must prioritize:

```text
correctness
simplicity
determinism
```

not speed.

Separate:

```text
decode
validate
execute
```

### DONE WHEN

Tiny hand-written instruction sequences can execute correctly.

---

# STEP 17 — ADD BASIC CPU TESTS

Write tests for:

```text
ADD
SUB
MUL
DIV
REM
AND
OR
XOR
NOT
SHL
SHR
SAR
branches
CALL
RET
```

Test:

```text
register state
PC
flags
```

### DONE WHEN

Every implemented instruction has execution tests.

---

# STEP 18 — IMPLEMENT MEMORY

Create:

```text
AddressSpace
MemoryRegion
RegionPermissions
AccessType
AccessSize
```

Support initially:

```text
RAM
```

and prepare the abstraction for:

```text
ROM
MMIO
```

---

# STEP 19 — SEPARATE FETCH FROM DATA ACCESS

Provide conceptually:

```text
fetch_instruction
read_data
write_data
peek
```

Do not treat every operation as arbitrary memory access.

### DONE WHEN

Instruction fetch and data access have distinct semantics.

---

# STEP 20 — IMPLEMENT MEMORY FAULTS

Support structured faults for:

```text
unmapped address
alignment
permission
invalid width
overflow
```

Each fault should retain:

```text
address
access type
access size
PC when available
```

---

# STEP 21 — IMPLEMENT THE BUS

Create:

```text
Bus
```

with routing between:

```text
CPU
 ↓
Bus
 ├── RAM
 ├── ROM
 └── MMIO
```

The CPU must not know device addresses.

Prevent overlapping mappings.

### DONE WHEN

A CPU can execute code fetched through the bus.

---

# STEP 22 — IMPLEMENT GENERIC DEVICES

Create:

```text
Device
DeviceManager
```

The interface should support operations similar to:

```text
reset
read
write
tick
```

Do not include SDL3 in this abstraction.

---

# STEP 23 — IMPLEMENT THE CONSOLE DEVICE

Create a basic virtual console.

The guest should be able to output text through an architectural interface.

### DONE WHEN

A machine program can produce:

```text
Hello, Lazalith
```

without SDL3.

---

# STEP 24 — IMPLEMENT VIRTUAL TIME

Create:

```text
VirtualClock
```

Do not make guest execution directly depend on the host's wall clock.

Devices receive controlled virtual time/cycle updates.

---

# STEP 25 — IMPLEMENT THE MACHINE

Create:

```text
LazalithMachine
```

containing:

```text
CPU
Bus
Memory
Devices
ArchitectureConfig
VirtualClock
```

Do not expose internals directly.

---

# STEP 26 — IMPLEMENT MACHINE LIFECYCLE

Define:

```text
Created
Reset
Running
Paused
Halted
Faulted
```

Implement:

```text
reset
step
run
pause
```

Reject invalid state transitions.

### DONE WHEN

The machine cannot casually continue after entering a terminal/faulted state.

---

# STEP 27 — CREATE THE BOOT SPECIFICATION

Write:

```text
docs/boot.md
```

Define:

```text
reset state
reset vector
boot address
initial memory
kernel loading
entry point
boot image
```

Keep the initial boot mechanism simple.

---

# STEP 28 — IMPLEMENT A BASIC BOOTLOADER

Implement only what is required to:

```text
initialize machine
locate kernel
validate kernel
load kernel
jump to kernel
```

Do not put OS logic inside the bootloader.

---

# STEP 29 — DESIGN LAZOS

Now write:

```text
docs/os-design.md
```

Design a small native operating system for Lazalith.

Call it:

```text
LazOS
```

The initial OS should eventually provide:

```text
processes
memory management
system calls
filesystem
console
display
input
program loading
basic multitasking
```

Do not attempt to reproduce Linux.

---

# STEP 30 — DESIGN USER/KERNEL SEPARATION

Define:

```text
Kernel
User Space
Hardware
```

Architecture:

```text
User Program
    ↓
Syscall
    ↓
Kernel
    ↓
Virtual Hardware
```

Define what user software cannot do directly.

---

# STEP 31 — IMPLEMENT TRAPS AND INTERRUPTS

Create:

```text
TrapController
InterruptController
```

Implement the minimum needed for the OS.

Define:

```text
trap entry
trap return
interrupt delivery
interrupt masking
```

Keep architecture semantics centralized.

---

# STEP 32 — IMPLEMENT PRIVILEGE LEVELS

Introduce the minimum useful privilege model.

For example:

```text
Supervisor
User
```

The exact names are up to the ISA design.

Privileged operations must be architecturally defined.

---

# STEP 33 — DESIGN THE OS MEMORY MODEL

Write:

```text
docs/os-memory.md
```

Define:

```text
physical memory
kernel memory
user memory
stack
heap
address spaces
permissions
page size if virtual memory is used
```

If paging is too much for the first OS milestone, use the simplest protection
model that still gives a clean path toward paging later.

---

# STEP 34 — IMPLEMENT BASIC KERNEL MEMORY MANAGEMENT

Implement the minimum needed for:

```text
kernel allocation
user process memory
stack
heap
```

Do not implement a sophisticated allocator prematurely.

---

# STEP 35 — DESIGN THE SYSTEM CALL ABI

Create a dedicated ABI layer.

Define syscalls such as:

```text
exit
read
write
open
close
seek
time
sleep
memory allocation
```

Graphics/input can be added when their drivers exist.

Do not copy Linux syscall numbers.

---

# STEP 36 — CREATE THE OS ABI CRATE

Create:

```text
lazalith-os-abi
```

The same definitions must be shared by:

```text
kernel
Lazen runtime
C runtime
SDK
debugger
```

Do not duplicate syscall definitions.

---

# STEP 37 — IMPLEMENT THE SYSCALL DISPATCHER

Implement:

```text
User
 ↓
SYSCALL
 ↓
Kernel Dispatcher
 ↓
Kernel Service
```

Validate syscall arguments.

Invalid syscalls must produce structured errors/status results.

---

# STEP 38 — IMPLEMENT PROCESSES

Create:

```text
ProcessId
ThreadId
Process
Thread
```

A process should have explicit:

```text
address space
CPU state
stack
program image
handles
state
```

Use explicit states:

```text
Created
Ready
Running
Blocked
Exited
Faulted
```

---

# STEP 39 — IMPLEMENT A SIMPLE SCHEDULER

Use a very simple scheduler initially.

For example:

```text
round-robin
```

if appropriate.

The goal is simply:

```text
multiple user programs can execute
```

Do not optimize scheduling yet.

---

# STEP 40 — IMPLEMENT THE LAZOS PROGRAM LOADER

The kernel must be able to load:

```text
.lzx
```

into a user process.

Validate:

```text
architecture
ISA version
ABI
sections
permissions
entry point
memory requirements
```

Malformed programs must not crash the kernel.

---

# STEP 41 — IMPLEMENT THE FILESYSTEM

Start simple.

Provide:

```text
open
read
write
close
seek
stat
```

Use a clean virtual filesystem abstraction.

The first implementation can be a simple in-memory or block-backed filesystem.

---

# STEP 42 — CREATE USERSPACE INIT

Boot flow should now become:

```text
Bootloader
   ↓
LazOS kernel
   ↓
init
```

Create a minimal `init` program.

---

# STEP 43 — CREATE THE USERSPACE SHELL

Create:

```text
lazos$
```

with minimal commands:

```text
help
echo
ls
cat
run
clear
```

### DONE WHEN

You can boot:

```text
bootloader
 ↓
kernel
 ↓
init
 ↓
shell
```

inside the headless emulator.

---

# STEP 44 — MAKE THE FIRST END-TO-END ASSEMBLY PROGRAM

Now implement enough toolchain infrastructure to make:

```text
Assembly
 ↓
Object
 ↓
Executable
 ↓
LazOS
 ↓
Process
```

possible.

Do not start the C compiler yet.

---

# STEP 45 — DESIGN THE OBJECT FORMAT

Define native:

```text
.lzo
.lzx
```

formats.

Support:

```text
architecture
ISA version
ABI version
sections
symbols
relocations
entry point
debug metadata
```

Create typed structures:

```text
ObjectFile
Section
Symbol
Relocation
```

---

# STEP 46 — IMPLEMENT THE ASSEMBLER

Pipeline:

```text
Source
 ↓
Lexer
 ↓
Parser
 ↓
Semantic validation
 ↓
Encoder
 ↓
Relocations
 ↓
.lzo
```

Every token should have source location.

Every error should have:

```text
file
line
column
span
```

---

# STEP 47 — IMPLEMENT THE DISASSEMBLER

Use the same ISA definitions.

Verify instruction round-tripping where appropriate.

---

# STEP 48 — IMPLEMENT THE LINKER

Pipeline:

```text
Objects
 ↓
Validation
 ↓
Symbol resolution
 ↓
Section layout
 ↓
Relocations
 ↓
Debug mappings
 ↓
Executable
```

Reject incompatible:

```text
LZ32/LZ64
ISA versions
ABI versions
```

---

# STEP 49 — RUN A REAL ASSEMBLY PROGRAM UNDER LAZOS

The milestone is:

```text
hello.lzs
 ↓
assembler
 ↓
.lzo
 ↓
linker
 ↓
.lzx
 ↓
LazOS loader
 ↓
process
 ↓
console
```

### DONE WHEN

The shell can launch a real Lazalith assembly program.

---

# STEP 50 — DESIGN THE LAZEN LANGUAGE

Now begin Lazen.

Do NOT immediately code the compiler.

Write:

```text
docs/lazen-design.md
docs/lazen-rationale.md
```

Ask:

> What language would make building native Lazalith applications easy?

Do not simply copy:

```text
C
Rust
Pascal
Go
Swift
```

Study useful ideas from those languages, then design something appropriate for
Lazalith.

---

# STEP 51 — DEFINE LAZEN'S PURPOSE

Lazen should be the easiest way to create:

```text
LazOS applications
GUI applications
games
utilities
terminal tools
interactive programs
```

C remains useful for:

```text
systems programming
porting
interoperability
low-level runtime work
```

Assembly remains the lowest-level interface.

---

# STEP 52 — PROTOTYPE LAZEN SYNTAX

Before freezing syntax, write example programs for:

```text
hello world
variables
functions
conditionals
loops
arrays
records/structs
modules
errors
file I/O
graphics
input
```

Use these examples to determine whether the language is actually pleasant.

Do not implement the parser yet.

---

# STEP 53 — DESIGN LAZEN'S MEMORY MODEL

Determine whether Lazen uses:

```text
ownership
manual memory
reference counting
garbage collection
arenas
hybrid
```

Choose based on:

```text
LazOS
runtime size
predictability
performance
debugging
developer experience
```

Do not choose simply because Lazalith itself is written in Rust.

---

# STEP 54 — DESIGN LAZEN'S TYPE SYSTEM

Decide:

```text
integer types
boolean
string
array
struct
enum
function
pointer/reference
optional values
```

Optional features:

```text
type inference
generics
pattern matching
traits/interfaces
```

Only add features that make sense.

---

# STEP 55 — DESIGN LAZEN MODULES AND PACKAGES

Design:

```text
imports
visibility
modules
packages
dependencies
```

A project should eventually look approximately like:

```text
hello/
├── lazen.toml
└── src/
    └── main.lz
```

---

# STEP 56 — DESIGN LAZEN APPLICATIONS

Define what makes a Lazen application different from a raw executable.

Determine:

```text
name
version
entry point
architecture
resources
permissions
dependencies
```

Potentially use:

```text
lazen.toml
```

but choose the final format deliberately.

---

# STEP 57 — DESIGN THE LAZEN SDK

Lazen should not require users to know:

```text
MMIO
framebuffer addresses
device registers
SDL3
```

Design a high-level SDK:

```text
Console
Filesystem
Time
Graphics
Input
Resources
Process APIs
```

The SDK should call the OS, not the emulator internals.

---

# STEP 58 — DESIGN LAZEN GRAPHICS API

Design a simple native graphics API.

Possible concepts:

```text
Window
Canvas
Color
Image
Text
Event
```

Do not copy SDL APIs into Lazen.

SDL3 is a host implementation detail.

---

# STEP 59 — DESIGN LAZEN INPUT API

Design a clean event system:

```text
KeyDown
KeyUp
MouseMove
MouseButton
Controller
```

The application must not know about SDL event structures.

---

# STEP 60 — IMPLEMENT LAZALITH IR

Create the common low-level IR used by:

```text
Lazen
C
```

This should sit below language-specific ASTs.

Architecture:

```text
Lazen
   ↓
Lazen representation
   ↓
Lazalith IR
```

and:

```text
C
 ↓
C representation
 ↓
Lazalith IR
```

---

# STEP 61 — IMPLEMENT THE LAZEN COMPILER FRONTEND

Create:

```text
Lexer
Parser
AST
Name Resolution
Type Checking
Semantic Analysis
```

Use the shared diagnostic system.

Every AST node that originates from source should carry source provenance where
useful.

---

# STEP 62 — IMPLEMENT LAZEN IR / LOWERING

Transform:

```text
Lazen AST
 ↓
Typed Lazen representation
 ↓
Lazalith IR
```

Do not emit machine code directly from the parser.

---

# STEP 63 — IMPLEMENT LAZEN CODE GENERATION

Use:

```text
Lazalith ISA
Lazalith ABI
Lazalith IR
```

to produce Lazalith instructions.

Do not duplicate ABI rules.

---

# STEP 64 — IMPLEMENT THE LAZEN RUNTIME

Create the runtime needed by user programs.

It should provide:

```text
startup
stack setup
syscall wrappers
memory helpers
string helpers
application entry
```

The runtime communicates with LazOS through the OS ABI.

---

# STEP 65 — COMPILE THE FIRST LAZEN PROGRAM

Start with:

```text
Hello, Lazalith
```

Pipeline:

```text
main.lz
 ↓
Lazen compiler
 ↓
Lazalith object
 ↓
linker
 ↓
.lzx
 ↓
LazOS loader
 ↓
process
 ↓
console
```

### DONE WHEN

A Lazen program runs under LazOS.

---

# STEP 66 — IMPLEMENT LAZEN CLI

Provide commands gradually:

```text
lazen new
lazen check
lazen build
lazen run
lazen test
lazen fmt
```

Do not implement commands whose underlying functionality does not exist yet.

---

# STEP 67 — FIRST LAZEN STANDARD LIBRARY

Start with:

```text
core
io
text
math
collections
fs
time
process
```

Only add APIs that the OS actually supports.

---

# STEP 68 — IMPLEMENT DISPLAY HARDWARE

Now implement:

```text
Virtual Display Device
```

The guest owns the authoritative framebuffer.

The device must not depend on SDL3.

---

# STEP 69 — IMPLEMENT INPUT HARDWARE

Create:

```text
Virtual Input Device
```

with clean guest-visible events.

---

# STEP 70 — IMPLEMENT LAZOS DISPLAY DRIVER

Architecture:

```text
Lazen Application
 ↓
Lazen SDK
 ↓
LazOS
 ↓
Display Driver
 ↓
Virtual Display Device
```

Do not bypass the OS.

---

# STEP 71 — IMPLEMENT LAZOS INPUT DRIVER

Architecture:

```text
SDL3
 ↓
Host Input Adapter
 ↓
Virtual Input Device
 ↓
LazOS Driver
 ↓
Lazen SDK
 ↓
Application
```

---

# STEP 72 — FIRST GRAPHICAL LAZEN APPLICATION

Create a simple graphical program.

It should:

```text
create a window
draw something
receive keyboard input
update state
```

The application must not know SDL3 exists.

---

# STEP 73 — IMPLEMENT LAZEN GUI LIBRARY

Create a first-party UI library eventually.

Possible components:

```text
Window
Panel
Button
Label
TextInput
Canvas
Menu
Layout
```

This library uses the Lazen SDK.

It does not talk directly to SDL3.

---

# STEP 74 — IMPLEMENT THE DEBUG API

Create:

```text
DebugController
DebugSession
```

Support:

```text
run
pause
step
continue
breakpoint
watchpoint
register inspection
memory inspection
stack
disassembly
snapshot
restore
```

Do not allow frontends to manipulate CPU internals directly.

---

# STEP 75 — IMPLEMENT MACHINE SNAPSHOTS

Create:

```text
MachineSnapshot
CpuSnapshot
DeviceSnapshot
ProcessSnapshot
```

Support:

```text
snapshot
restore
```

Only guest-visible state belongs in machine snapshots.

---

# STEP 76 — SOURCE-LEVEL DEBUG INFORMATION

Connect:

```text
Lazen source
 ↓
AST
 ↓
IR
 ↓
machine code
 ↓
object
 ↓
executable
 ↓
debugger
```

A debugger should eventually map:

```text
PC
 ↓
Lazalith instruction
 ↓
Lazen source line
```

---

# STEP 77 — IMPLEMENT THE SDL3 FRONTEND

Create:

```text
lazalith-gui
```

using SDL3.

It should display:

```text
machine screen
registers
PC
flags
disassembly
memory
stack
console
processes
diagnostics
```

Use the debug API.

Never access CPU internals directly.

---

# STEP 78 — IMPLEMENT GUI CONTROLS

Provide:

```text
Run
Pause
Step
Reset
Continue
Breakpoint
```

The GUI sends commands to the machine controller.

---

# STEP 79 — IMPLEMENT GUI DIAGNOSTICS

The GUI must consume structured diagnostics directly.

Do not parse terminal error strings.

It should be capable of showing:

```text
error code
message
source
line
column
guest PC
instruction
stack trace
```

where available.

---

# STEP 80 — INTERNAL EMULATOR ERROR REPORTING

For genuine emulator bugs, report:

```text
subsystem
operation
guest PC
instruction
address
machine state
invariant violated
Rust file
Rust line
Rust column
```

Use Rust source-location facilities where possible.

Distinguish:

```text
guest fault
```

from:

```text
emulator bug
```

---

# STEP 81 — C COMPILER

Only now implement the C compiler.

Use:

```text
C
 ↓
Lexer
 ↓
Parser
 ↓
AST
 ↓
Semantic Analysis
 ↓
Lazalith IR
 ↓
Native backend
 ↓
.lzo
```

Use the existing:

```text
ISA
ABI
IR
Object Format
Diagnostics
Linker
```

Do not create competing infrastructure.

---

# STEP 82 — C RUNTIME

Implement a minimal C runtime using LazOS syscalls.

Support enough for:

```c
int main(void) {
    return 0;
}
```

then expand to:

```c
printf
malloc
free
file operations
```

as the OS supports them.

---

# STEP 83 — ASSEMBLY / C / LAZEN COMPATIBILITY

All three should eventually converge:

```text
Assembly ──┐
           │
C ─────────┼──→ Lazalith Object
           │
Lazen ─────┘
```

then:

```text
Object
 ↓
Linker
 ↓
Executable
 ↓
LazOS
```

Do not create separate executable ecosystems unnecessarily.

---

# STEP 84 — PROPERTY TESTING

Add property tests for:

```text
instruction encode/decode
register behavior
width conversions
memory
object format
parser
IR
```

Example:

```text
decode(encode(instruction)) == instruction
```

for valid encodings.

---

# STEP 85 — DIFFERENTIAL TESTING

Compare:

```text
ReferenceInterpreter
```

against any optimized emulator.

Compare:

```text
registers
PC
flags
memory
devices
virtual time
process state
```

---

# STEP 86 — FUZZING

Fuzz:

```text
instruction decoder
assembler
Lazen parser
C parser
object reader
executable loader
kernel loader
filesystem metadata
debugger commands
snapshot reader
```

Malformed data must not silently corrupt state.

---

# STEP 87 — DETERMINISTIC REPLAY

Implement:

```text
InputLog
ReplaySession
MachineSnapshot
```

A bug should eventually be reproducible from:

```text
binary
architecture config
initial state
input log
```

---

# STEP 88 — APPLICATION PACKAGING

After the executable/app model works, design an application package.

Potentially:

```text
.lza
```

containing:

```text
executable
manifest
resources
metadata
debug information
```

Do not finalize this format before understanding actual application
requirements.

---

# STEP 89 — LAZEN PACKAGE MANAGEMENT

Add local package/dependency support.

Keep the first implementation simple.

Do not build a giant online package registry immediately.

---

# STEP 90 — LAZEN FORMATTER

Implement:

```text
lazen fmt
```

with one canonical formatting style.

---

# STEP 91 — EXPAND LAZOS

Only after the basic system is stable, expand:

```text
better filesystem
process management
threads
networking
audio
more drivers
permissions
virtual memory
```

Do not add these before the basic platform works.

---

# STEP 92 — EXPAND LAZEN

Only after the core language is stable, consider:

```text
generics
pattern matching
advanced collections
concurrency
advanced modules
macros/metaprogramming
```

Do not add features merely because another language has them.

---

# STEP 93 — OPTIMIZATION

Only after the reference emulator, OS, compiler, and tests are stable should
optimization begin.

Possible:

```text
optimized interpreter
faster memory paths
instruction caching
JIT
```

Every optimized implementation must preserve reference behavior.

---

# STEP 94 — OPTIONAL LLVM BACKEND

LLVM is optional.

Possible architecture:

```text
Lazen ──┐
        ├──→ Lazalith IR → Native Backend
C ──────┘                 \
                           → Optional LLVM Backend
```

LLVM must NOT become mandatory.

Native Lazalith infrastructure remains the foundation.

---

# STEP 95 — NIX INTEGRATION

Make the full system reproducible.

Verify:

```text
nix develop
nix build
nix flake check
```

cover:

```text
Rust
emulator
OS
assembler
linker
Lazen
C compiler
SDL3 frontend
tests
```

---

# STEP 96 — FINAL INTEGRATION TEST

Create a test that effectively performs:

```text
Lazen source
 ↓
Lazen compiler
 ↓
Lazalith object
 ↓
Linker
 ↓
.lzx
 ↓
LazOS loader
 ↓
Process
 ↓
Syscalls
 ↓
Virtual Hardware
 ↓
Emulator
```

and verifies the expected result.

---

# STEP 97 — FINAL GRAPHICS TEST

Create:

```text
Lazen graphical application
```

that:

```text
boots under LazOS
creates a window
draws graphics
receives input
responds to input
```

through:

```text
Lazen SDK
 ↓
LazOS
 ↓
virtual devices
 ↓
SDL3
```

---

# STEP 98 — FINAL DEBUGGING TEST

Demonstrate:

```text
Lazen source
 ↓
compiler
 ↓
executable
 ↓
LazOS
 ↓
runtime fault
```

and ensure the debugger can recover:

```text
source file
line
column
Lazen function
Lazalith instruction
guest PC
registers
stack
```

where debug information exists.

---

# STEP 99 — FINAL ARCHITECTURE REVIEW

Before considering the project complete, verify that:

```text
CPU does not depend on SDL3.
Machine does not depend on compiler.
Compiler does not depend on emulator implementation.
GUI does not access CPU internals.
Lazen does not bypass LazOS.
C does not bypass the ABI.
Assembly can still access low-level functionality.
LZ32 and LZ64 share architecture infrastructure.
ISA definitions are not duplicated.
ABI definitions are not duplicated.
Syscall definitions are not duplicated.
Diagnostics are centralized.
Guest faults and emulator bugs are distinguishable.
```

---

# STEP 100 — FINAL PROJECT

The finished platform should provide:

```text
                   LAZALITH
                       │
      ┌────────────────┼────────────────┐
      │                │                │
     CPU             Tools            GUI
      │                │                │
      ▼                ▼                ▼
   Machine      Assembler/Linker      SDL3
      │
      ▼
  Lazalith HW
      │
      ▼
    LazOS
      │
      ▼
  System ABI
      │
      ▼
   ┌───────┐
   │ Lazen │
   └───┬───┘
       │
       ▼
Applications
```

Programming layers:

```text
Lazen
    ↓
Easy native applications

C
    ↓
Systems / interoperability

Assembly
    ↓
Low-level machine control
```

All three target:

```text
Lazalith Object
    ↓
Linker
    ↓
Lazalith Executable
    ↓
LazOS
    ↓
Lazalith Machine
```

---

# MOST IMPORTANT INSTRUCTION

Do not treat this document as a request to generate 100 steps of code
immediately.

Treat it as a **state machine for development**.

At any point:

```text
Current Step
    ↓
Implement
    ↓
Test
    ↓
Fix
    ↓
Document
    ↓
Next Step
```

Do not jump over foundational steps.

Do not implement future features simply because their names appear later in this
document.

Do not create fake implementations just to satisfy a milestone.

When a future subsystem needs a foundation, implement the smallest correct part
of that foundation early.

For example:

```text
Lazen needs OS syscalls
```

therefore establish the syscall ABI before the Lazen runtime.

Likewise:

```text
OS needs executable loading
```

therefore establish the object/executable format before building the full
program loader.

Likewise:

```text
compiler diagnostics need source locations
```

therefore establish `SourceSpan` before building the compiler.

---

# DEFINITION OF DONE

A step is complete only when the relevant implementation:

```text
works
+
is tested
+
has structured error handling
+
has diagnostics where applicable
+
is documented
+
passes Cargo checks
+
passes relevant Nix checks
```

Do not mark a step complete merely because the code compiles.

---

# FINAL DESIGN PHILOSOPHY

Build Lazalith in this order of conceptual dependency:

```text
FOUNDATION
    ↓
ISA
    ↓
CPU
    ↓
Memory
    ↓
Bus
    ↓
Virtual Hardware
    ↓
Machine
    ↓
Boot
    ↓
LazOS
    ↓
OS ABI
    ↓
Lazalith IR
    ↓
Lazen Design
    ↓
Lazen Compiler
    ↓
Lazen Runtime
    ↓
Applications
    ↓
Graphics/Input SDK
    ↓
SDL3 GUI
    ↓
C Toolchain
    ↓
Debugger
    ↓
Advanced Testing
    ↓
Optimization
```

The difficult pieces are introduced exactly when later components require them.

The central rule is:

> **Do not make Lazalith merely an emulator. Build the machine, then the
> operating system, then the native language and application ecosystem that
> naturally belongs on that machine.**

The final user experience should eventually be as simple as:

```bash
nix develop
lazen new myapp
cd myapp
lazen run
```

which results in:

```text
LazOS
  ↓
myapp.lzx
  ↓
Lazen application running inside Lazalith
```

while an advanced developer can still use:

```text
Assembly
C
Debugger
Raw machine interfaces
```

when necessary.

---
# HARDENING PHASE

The first 100 steps are complete and verified. This phase is not a feature phase.

Its question is:

```text
What is still wrong with Lazalith despite all existing tests passing?
```

The answer is not "nothing", and the defects that matter most are the ones a
green suite cannot see: wrong answers rather than traps, behaviour that is only
wrong end to end, representation lost between two stages, a stale assumption, an
ABI mistake, a width or sign handled in the wrong place, a test that was green
because it asserted nothing.

So this phase is an **adversarial audit**. For every subsystem: read it, state
its invariants, design inputs that should break them, run them against the
*unmodified* implementation, and only then patch. A finding that cannot be
reproduced is not a finding.

A defect is only fixed when it leaves behind:

```text
a minimal reproducer
the root cause
the fix
a regression test that fails without the fix
an entry in docs/project-state.md
```

## The clusters

Numbered as steps so each one is a checkpoint, following the convention above.

```text
H1   architecture audit
H2   compiler frontend audit
H3   IR verifier and value-flow audit
H4   code generation and frame-layout audit
H5   ISA, CPU and machine audit
H6   memory, bus and device audit
H7   kernel, ABI and OS audit
H8   filesystem, process and device-service audit
H9   runtime and C frontend audit
H10  graphics, input, GUI and SDL audit
H11  debugger, snapshot and replay audit
H12  object format, linker, package and build audit
H13  property, fuzz and differential expansion
H14  performance and resource audit
H15  state isolation and repeatability
H16  public-API hardening
H17  documentation and specification consistency
H18  final adversarial regression campaign
```

## What this phase must not become

No feature wishlist. A language feature, an ABI call, a GUI widget, an ISA
instruction, an OS service, a third-party dependency or a speculative abstraction
is added only when it is needed to fix a demonstrated defect, to expose one, to
meet an existing contract, or to remove a demonstrated defect's cause.

An **acceptable limitation** is not a defect and is not "fixed" into undefined
behaviour: a missing allocator, a missing thread, a headless-only test, and a
display frame address the platform cannot yet vouch for are all recorded, none
of them are invented around.

## The rule at the end

```text
"all tests pass"
```

is not

```text
"the platform is thoroughly hardened"
```

The purpose of these steps is to find what the current tests are **not** seeing.
