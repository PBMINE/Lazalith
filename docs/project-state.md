# Lazalith — Project State

Last updated: 2026-09-17 (Steps 1–25 of the roadmap in `instruction.md`)

## Step 25 — Privately Owned Machine

Added `lazalith-machine` (no_std + alloc; only local CPU/devices/ISA/memory/
types dependencies), registered in the workspace, lockfile, and Nix library
installation (seven libraries). `LazalithMachine<D: Device>` exclusively owns a
private CPU, Bus, ArchitectureConfig, and VirtualClock; nothing is exposed
directly. Construction is explicit via `MachineSetup { config, devices,
regions, pc, sp, status, initial_time }` and validation happens once at the
memory/CPU boundary; the machine never invents state. The clock is prevalidated
(`VirtualClock::advanced`) before device ticks, preserving both on failure, and
no wall-clock enters the system.

Step 25 is deliberately not Step 26: the machine exposes `step` and bounded
`run(limit)` only — no reset/pause/resume/lifecycle or state machine. `step`
runs one CPU instruction through the bus and returns `MachineEvent::Stepped`
or `MachineEvent::Halted`; `run` reports per-call `executed` (including the
halting step) and cumulative `halted_at` (instructions executed since
construction at first halt; `None` when the limit bound the loop). Faults,
clock overflow, and post-halt steps return typed `MachineError`s
(`Fault(Box<CpuFault<MemoryFault>>)` keeps the error small) and leave halted,
state, clock, and device state untouched; no fake trap delivery or controller.
Inspection APIs are read-only: config, clock, is_halted, architectural_state,
devices, memory, and a pure peek_memory. Loading (`load_region`, `load_bytes`,
`map_device`) is allowed before execution and fails without mutation.

Four tests cover: end-to-end `Hello, Lazalith` in both modes through a real
ROM+RAM+MMIO machine with exact halt timing and no internal exposure; fault
and clock-overflow atomicity with unchanged CPU state, output, and elapsed
cycles; deterministic repeated construction and bounded then completing runs;
and setup/loader failure contracts, including an unmapped-PC machine whose
first step faults without effect and a machine that halts inside a bounded run
(returning an error, not a fake success). Workspace total: 156 tests (4 machine,
3 devices, 27 memory, 35 CPU, 56 types, 14 ISA, 17 diagnostics), zero doctests.
A clippy `result_large_err` error was fixed by boxing the fault variant, and
three test-side issues (a stray `second_machine` binding, two shadowed helper
names, and one run-semantics miscount) were corrected before the full rerun.
All gates passed: fmt/fmt check, strict all-target Clippy, workspace
check/test, all three host Nix checks, and package build
(`/nix/store/j566vaghra4j72hhh3zl6jdfgpsnqdsb-lazalith-foundations-0.1.0`).
No comments, external dependencies, staging, or commits. Scope stops at Step 25:
machine lifecycle, controller/OS/traps, and SDL frontends are explicitly out of
scope and remain for later steps.

### Step 25 post-review corrections

An independent review found three real machine-layer defects; all are fixed and
covered by three new tests (7 machine tests total, 159 workspace tests):

- Construction previously ticked pre-elapsed devices by the full
  `initial_time`, double-advancing them relative to the machine clock.
  Construction now derives the delta `initial_time - devices.clock().elapsed()`
  (structured `MachineError::InitialClock { devices, requested }` when devices
  are ahead) and ticks only that delta, so both clocks and devices agree.
- `run()` previously ignored trap outcomes and re-executed the unchanged trap
  PC forever. It now stops at the trap, returns `MachineRun.trap =
  Some((TrapRequest, resume_pc))` with exact pre-state preserved (traps do not
  advance PC), and never re-executes it.
- Cumulative execution counting moved into `step()` (checked, overflow is
  `MachineError::InstructionCountOverflow`), so stepped-then-run sequences
  report correct totals (`halted_at` includes pre-run steps).


## Step 24 — Deterministic Virtual Clock

`VirtualClock` now lives in `lazalith-types` beside the shared `CycleCount` it
wraps: `new`/`at` construction, pure `advanced`, fallible `advance`, and
`prepare_advance` returning a `PreparedAdvance` whose dropped plans change
nothing and whose single `commit` publishes the prevalidated next clock.
Overflow is `ClockOverflow { current, delta }` (an `Error`), never a wrap; a
failed advance leaves the clock bit-identical. Zero deltas are exact no-ops.
There is no host wall-clock dependency anywhere in the workspace.

`DeviceManager` now owns a private `VirtualClock` instead of a raw counter, and
`Device::tick` receives `CycleCount`. `DeviceError::TickOverflow` was replaced
by `DeviceError::Clock(ClockOverflow)` with a typed source chain. Manager tick
stays atomic: overflow touches neither any device nor the clock, zero deltas
call no device, and `clock()` exposes the elapsed count for hosts. Console
stores the shared `CycleCount`, and `Bus::tick_devices` forwards it unchanged.

Three new `lazalith-types` clock unit tests (boundaries/atomic overflow, pure
prepared plans, deterministic repeated sequences) plus the updated devices and
MMIO tests all pass. Workspace total: 152 tests (3 devices, 27 memory, 35 CPU,
56 types, 14 ISA, 17 diagnostics), zero doctests. Full gates passed: fmt/fmt
check, strict all-target Clippy, workspace check/test, all three host Nix
checks, and package build
(`/nix/store/z916r7rxqps69g0pwmf8fy3lbyk60g1d-lazalith-foundations-0.1.0`).
No new dependencies; aarch64-linux remains untested. No comments, staging, or
commits. Step 25 (machine ownership, not lifecycle) is next.


## Step 23 — Bounded Byte Console

`ConsoleDevice::new(capacity)` reserves the entire bounded output buffer with
try_reserve_exact before returning. There are no allocations during guest
writes, reset, peek or tick. Offset zero is the sole byte-wide write-only
register: each successful store appends the low byte, including arbitrary binary
data; wrong offsets/sizes and full buffers fail without effects. Guest reads
are unsupported and MMIO peek is Unpeekable. Host output()/capacity()/elapsed()
provide immutable inspection, never the mutable Vec or internal fields. reset
clears output while retaining the reserved storage.

Three console tests cover all 256 bytes, truncation, reset/reuse, zero/full
capacity, deterministic oversized allocation failure, bad accesses and pure
inspection. Two console_cpu tests execute actual encoded ROM instructions via
ReferenceInterpreter + Bus in LZ32 and LZ64, producing `Hello, Lazalith` without
SDL; the full-buffer fault preserves CPU state, RAM and earlier output. The
fixture includes distinct ROM code, RAM stack storage and write-only MMIO.
A test initially used tuple syntax for the existing struct-shaped Memory fault;
that compile error was corrected, then the complete focused suite passed.

Before Step 24: all 149 workspace tests passed (3 devices, 27 memory, 35 CPU,
14 ISA, 53 types, 17 diagnostics), zero doctests. cargo fmt/fmt-check, strict
workspace/all-target Clippy, workspace/all-target check, all three host Nix
checks and package build passed. Package output:
`/nix/store/ivsgrayr5b5mhhr8mpnincn4rh6vswkb-lazalith-foundations-0.1.0`.
No dependency or flake changes were necessary for these modules/tests;
aarch64-linux remains untested. No comments, external dependencies, staging or
commits. The allocation test deterministically exercises capacity overflow,
not injected host allocator exhaustion.


## Step 22 — Owned Generic Devices and MMIO

Added `lazalith-devices` (no_std + alloc; only local ISA/types dependencies),
`Device`, `DeviceManager<D>`, `DeviceError`, and the uninhabited `NoDevice`
default for memory-only buses. DeviceId/DeviceOffset are re-exports of the
existing shared types, not parallel identifier domains. Workspace, lockfile,
and Nix installation now include six libraries.

`Bus<D>` exclusively owns AddressSpace and DeviceManager. `with_devices` and
`map_device(id, physical_start, permissions)` map the complete captured device
extent, once per ID. No alias mappings, partial extents, executable MMIO, or
RAM/ROM/MMIO overlaps are accepted, in either insertion order. Mapping errors
leave bus mappings untouched. Data routing checks configuration, full ranges,
operation kinds and permissions before invoking a device. Fetch/stack/loader
never invoke MMIO reads or writes. Host peek routes purely, never falls back to
read, and rejects unpeekable devices with a typed error. Device failures retain
memory address/access/size/privilege and participate in Error::source chains.

Device implementer contract: address_len remains stable; validate_read/write
are pure; read/write must validate all conditions and reserve any required
capacity before effects, returning error with no observable mutation. Successful
validation cannot authorize a later partial failure. peek is pure and leaves its
output unchanged on error; unpeekable registers return Unpeekable without read.
reset is infallible, allocation-free and restores initial state without changing
extent. tick receives absolute elapsed virtual cycles, is infallible and
allocation-free for every u64 input, and cannot invalidate an already accepted
MMIO transaction. The manager checks elapsed addition before ticking any device;
overflow changes neither elapsed nor any device. Zero delta invokes no devices.
Insertion reserves before effects and synchronizes the incoming device to elapsed.
These are trusted Rust implementation contracts, not a sandbox for arbitrary
trait implementations; no rollback or panic interception is claimed. Step 24
will replace raw tick counts with the shared CycleCount and clock preparation.

Seven new integration tests exercise manager validation/reset/overflow, actual
CpuMemory MMIO routing in both modes, pure/unpeekable inspection, all permission
combinations, complete/disjoint mappings across RAM/ROM/MMIO, both insertion
orders, duplicate/unknown IDs, width limits, and fetch/stack/loader rejection.
Workspace total: 144 tests (25 memory, 35 CPU, 14 ISA, 53 types, 17 diagnostics),
zero doctests. Focused baseline and MMIO suites passed. A peek closure borrow
compile error was corrected before the focused run; a final review replaced
initial duplicated IDs with shared types, followed by a complete successful rerun.
Before Step 23: cargo fmt/fmt-check, strict workspace/all-target Clippy,
workspace/all-target check, workspace test, all three host Nix checks and package
build passed. Nix output: `/nix/store/aykfniyqx3ylk39xzkgx62nic79g87sv-lazalith-foundations-0.1.0`.
aarch64-linux remains untested. No comments, external dependencies, staging,
commits, SDL, CPU dependencies in devices, or shared mutable ownership added.


## Initial Inspection

Before this report was created, the only project file present was
`instruction.md`, which contains the full 100-step implementation roadmap for
the Lazalith platform. Git metadata is present in `.git/`.

### Project files present before Step 1

```text
instruction.md
```

### Answers to the Step 1 checklist

| Question                          | Answer                                        |
| --------------------------------- | --------------------------------------------- |
| What files exist?                 | Only `instruction.md`                         |
| Is Cargo already configured?      | No — no `Cargo.toml`, no workspace            |
| Is Nix already configured?        | No — no `flake.nix`, no `flake.lock`          |
| Is there an existing emulator?    | No                                            |
| Is there existing ISA code?       | No                                            |
| Is there existing GUI code?       | No                                            |
| Git repository?                   | Yes, branch `main` exists but has no commits  |

## Step 1 Verification and Handoff

Step 1 is complete: the initial repository contents were inspected and this
report was read back. No implementation code was created or modified.
`git status --short` shows only the untracked `instruction.md` and `docs/`.
No commits were created.

The host provides Nix 2.34.8, but neither `cargo` nor `rustc` is on `PATH`.
Cargo formatting, linting, build, and test checks are not applicable to this
documentation-only step: there is no Rust workspace yet. Nix project checks
are also not applicable because no flake exists. No executable error-handling
paths were introduced.

The next step is Step 2: create the minimal Cargo workspace. Obtain a Rust
toolchain through Nix before validating it. Do not mark Step 2 complete until
`cargo build`, `cargo fmt`, `cargo clippy`, and `cargo test` succeed. Step 3
then establishes the reproducible project flake.

## Step 2 — Workspace

The Cargo workspace now contains only `lazalith-types` and
`lazalith-diagnostics`. Both are library scaffolds with no public API yet;
no placeholder functions or artificial passing tests were added. The diagnostics
crate has a local dependency on shared types. Both currently use `no_std`;
source storage and rendering may require `alloc` or `std` in subsequent steps.
Unsafe code is forbidden through inherited workspace lints.

A temporary Nix shell provides Cargo, Rust, rustfmt, Clippy, and GCC. Verified:

- `cargo check --workspace`
- `cargo fmt` and `cargo fmt --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo build --workspace`
- `cargo test --workspace` (zero tests: no behavior implemented yet)

These validate workspace wiring, not platform functionality. Behavioral tests
must accompany the first implemented APIs. Cargo.lock is retained for Nix
builds; build outputs are ignored. No runtime error paths exist yet. Nix flake
validation belongs to Step 3, which is next.

## Step 3 — Nix Development Environment

The flake pins nixpkgs in `flake.lock`. It provides Rust, Cargo, rustfmt,
Clippy, rust-analyzer with Rust sources, GCC, GDB, pkg-config, CMake, Ninja,
and SDL3 development files. SDL3 is only a host development dependency;
neither Rust crate links it.

The default package builds and tests both libraries and installs their `.rlib`
artifacts under `lib/`. There is deliberately no executable at this stage.
Source filtering excludes build outputs and unrelated project files.
Flake checks cover workspace build/tests, Rust formatting, and strict Clippy.

Verified on x86_64-linux:

- `nix build path:. --no-link --print-out-paths`, with both library artifacts
  confirmed in the output.
- `nix flake check path:.` passed all host checks.
- `nix develop path:. -c cargo build --workspace` succeeded.
- Inside the development shell: formatting, Clippy, type checking, and tests
  succeeded; tests still number zero because there are no implemented APIs.
- `pkg-config --modversion sdl3` reported 3.4.16.

The first flake check exposed an incorrect SDL package attribute, which was
fixed. Inspecting the first package output exposed missing library installation,
which was fixed and reverified. aarch64-linux outputs are declared but were not
built on this host.

Because all project files remain untracked, bare `nix flake check` rejects the
untracked flake. Use `nix develop path:.`, `nix build path:.`, and
`nix flake check path:.` until the project files are tracked by Git; the usual
commands then use the same outputs. Nothing has been staged or committed.

This session stops after the workspace/environment foundation. Step 4 is next:
implement the shared diagnostic system, with source location and structured
rendering introduced in Steps 5–6. No ISA, CPU, emulator, OS, compiler, or GUI
functionality has been implemented.

## Step 4 — Shared Diagnostic Foundation

`lazalith-diagnostics` now provides the first real API:

- `Severity` (`Error`/`Warning`/`Note`) with plain-name display.
- `DiagnosticCode`, validated as one ASCII uppercase letter followed by at
  least one digit (e.g. `E1001`). Construction returns
  `InvalidDiagnosticCode`, which retains the rejected input, implements
  `Display` and `core::error::Error`, and never panics.
- `Diagnostic`, data rather than preformatted text: severity, code, owned
  message, and optional cause box. It is `Send + Sync` so host frontends can
  own it, implements `Display` as `error[E1001]: message`, and implements
  `Error::source` so typed cause chains survive. `SourceSpan`-style labels,
  notes, and help attachments are intentionally deferred to Steps 5–6, when
  source locations exist.
- Display output contains no terminal styling; rendering stays a later,
  separate concern. There is no `DynSourceMap`-style API yet.

Six unit tests cover accepted/rejected code forms (including non-ASCII,
empty, and trailing-letter inputs), field retention, per-severity formatting,
nested diagnostic cause chains through `Error::source`, and frontend
ownership of message strings. Verified: formatting, strict Clippy
(`-D warnings`), `cargo check`, `cargo test` (6 passed), and all Nix flake
checks plus a package build. The flake's clippy check compiled the new code
with the same strict flags. Still no ISA, CPU, emulator, OS, compiler, or GUI
functionality; Step 5 (source location types) is next.

## Step 5 — Source Location Types

`lazalith-types` now provides the platform's single authoritative source map:

- `ByteOffset`: u32-backed byte offset; saturating-free checked arithmetic
  with panics on overflow (documented), plus `as_u32`/`as_usize`.
- `SourceId`: opaque file identifier; `SourceFile`: immutable name + text +
  precomputed line-start map, built in one pass at registration.
- `SourceManager`: owns files, assigns ids in registration order, validates
  and constructs spans, and resolves positions. All other components must
  resolve line/column through it — no second source map exists.
- `SourceSpan`: byte range constructible only via `source_span` validation
  (reversed, out-of-bounds/unknown-id, and non-char-boundary ends are
  rejected with a structured `InvalidSpan`); resolution of a validated span
  cannot fail.
- `LineColumn` (1-based line, 1-based char column) and `ResolvedSpan`, which
  renders as `file.lz:2:1` (point) or `file.lz:1:1-3:6` (range).

Line semantics: lines split on `\n` only; `\r` is an ordinary character so
CRLF files do not gain extra lines and CR never inflates line counts; a
trailing newline starts a final empty line. Columns count chars, so `é`
occupies one column; offsets inside a multi-byte character resolve to
`None` rather than a wrong position. 15 unit tests cover exact line/column
computation (multi-byte, LF, CRLF, CR), point vs range rendering, empty
files, reversed/out-of-bounds/unknown-id/split-UTF-8 rejection, name
validation, and value-type behavior.

Two test failures during development were real bugs, each fixed and
reverified: validation initially sliced text before checking char boundaries
(panicking instead of erroring), and an over-broad interior-byte check
wrongly rejected spans containing whole multi-byte characters. Verified:
formatting, strict Clippy, `cargo check`, `cargo test` (21 tests: 15 types +
6 diagnostics), all Nix flake checks, and a package build. Step 6 (structured
diagnostics rendering on top of these spans) is next.

## Step 6 — Structured Diagnostics

`lazalith-diagnostics` now treats a diagnostic purely as structured data and
gains the first shared renderer:

- `Label` (primary/secondary via `LabelStyle`) carrying a validated
  `SourceSpan` and message; `Note` and `Help` are plain text attachments.
  All attach through `Diagnostic::with_label/with_note/with_help` and are
  readable through `labels()`, `notes()`, `help()`; the existing typed
  cause box remains, now also readable via `Diagnostic::cause()`. Nothing
  preformats terminal text; data stays data until rendering.
- `render_plain(&Diagnostic, &SourceManager) -> Result<String, RenderError>`
  produces rustc-style plain text. It resolves every label span exclusively
  through `SourceManager` (header position, line text via the new
  `SourceFile::line_text`/`SourceManager::line_text` API — no duplicate line
  map). It revalidates all label spans against the supplied manager before
  formatting any output; invalid spans return errors rather than panicking.
  Rendering failures use structured `RenderError` variants (`MissingSource`,
  `InvalidSpan` carrying the `InvalidSpan` cause, `UnresolvedSpan`,
  `MissingLine`), each retaining its label index.
- Rendering conventions: markers `^` (primary) / `-` (secondary); blank
  messages omit the trailing space; labels render in insertion order even
  across files; multiline spans mark every covered line with per-line gutter
  alignment; a span ending at column 1 on a later line is treated as ending
  at the previous line's end (half-open convention, no empty marker rows);
  notes, helps, and the cause render as `= note:`/`= help:`/`= cause:`
  blocks after the snippets. Columns remain the 1-based char/scalar-count
  convention from Step 5. Tabs and CR are preserved verbatim and each counts
  as one scalar; terminal display-cell alignment is not attempted. Empty/EOF
  points and newline-only selections receive at least one marker. The direct
  cause is rendered once; nested typed causes remain accessible through
  `Error::source` rather than being recursively formatted.

Eleven new diagnostics tests (17 total) cover attachment reads and ordering,
exact output for the roadmap-style snippet at `example.lz:4:1`, all
severities, notes/help/direct causes, overlapping and cross-file labels,
Unicode scalar columns, multiline spans and blank lines, aligned multi-digit
gutters, half-open end handling, empty-file/EOF/interior points, CR and tabs.
Error tests assert missing-source and invalid-span variants, label indices,
error text, and retained typed causes, including invalid later labels and
spans from another manager with shorter text or incompatible UTF-8 boundaries.
`UnresolvedSpan` and `MissingLine` are defensive fallbacks not reached by
these tests. Two new types tests (17 total) cover line text for empty and
unterminated files, Unicode, CR, blank lines, unknown sources, and invalid
line numbers.

Source ids remain manager-local: revalidation catches invalid ranges, not
provenance mismatches when another manager has the same id and a valid range.
Callers must retain the corresponding source manager. Source and attachment
text is not escaped; this is a minimal plain renderer, not a terminal-control
sanitizer. Both crates remain `no_std` with `alloc`; no code comments were
added.

Verified in the Nix dev shell: `cargo fmt`, `cargo clippy --workspace
--all-targets -- -D warnings`, `cargo check --workspace`, `cargo test
--workspace` (34 passed: 17 diagnostics + 17 types), then `nix flake check
path:.` (all host checks passed) and `nix build path:. --no-link`.
Validation ran on x86_64-linux; aarch64-linux was omitted as incompatible
with this host. No dependencies were added; nothing staged or committed.
Work stops after Step 6; Step 7 and architecture design were not started.

## Step 7 — Shared Architectural Types

Inspected the existing workspace, source types, tests, flake, and verification
instructions before implementation. Architectural types live separately in
`crates/lazalith-types/src/architecture.rs` and are re-exported from the crate
root in `src/lib.rs`. Existing source-location types and behavior are unchanged.

Eight distinct, private-field value types were added, with `Clone`, `Copy`,
`Debug`, equality, ordering, and hashing:

| Type | Backing | Construction and reads | Checked arithmetic |
| --- | --- | --- | --- |
| `PhysicalAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `VirtualAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `InstructionAddress` | `u64` | `new(u64)`, `as_u64()` | `checked_add(u64)`, `checked_sub(u64)` |
| `DeviceId` | `u32` | `new(u32)`, `as_u32()` | None |
| `DeviceOffset` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `CycleCount` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `InstructionCount` | `u64` | `new(u64)`, `as_u64()` | `checked_add(Self)`, `checked_sub(Self)` |
| `RegisterIndex` | `u8` | `TryFrom<u8>`, `as_u8()`, `as_usize()` | None |

Address arithmetic takes unsigned byte offsets, not other addresses or device
relative offsets. All checked operations return `Option<Self>`: overflow or
underflow returns `None` without mutation, wrapping, saturation, or panic.
There are no arithmetic operator implementations or automatic conversions
between architectural domains. No generic `Address` was needed. Constructors
and read getters are `const`; address constructors preserve the entire `u64`
range without imposing configuration, width, alignment, or mapping validation.
Those policies remain later work.

`RegisterIndex::COUNT` is 16. The index is an encoding field with exactly
`0..16` accepted (0 through 15), not a register file or register-role model.
The later ISA design will have ordinary r0–r15 with PC/SP separate; no special
register semantics are implemented here. There is no unchecked constructor,
public field, or infallible conversion into an index. Invalid construction
returns `InvalidRegisterIndex`, whose `input()` retains the original `u8`;
its `Display` reports the input and exclusive range and it implements
`core::error::Error` with no underlying cause.

Nine new tests cover all three address domains and all three offset/count
types at zero, above the 32-bit boundary, and `u64::MAX`; zero deltas, ordinary
arithmetic, exact maximum/zero results, and overflow/underflow; full-range
`DeviceId` reads; every one of the 16 valid register encodings and all 240
invalid `u8` inputs, including retained values, exact error text, and error
source behavior. Total: 43 passing unit tests (26 types + 17 diagnostics),
with zero doctests and no placeholder tests.

Verified on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed (workspace build/tests,
  formatting, and strict Clippy); aarch64-linux was omitted as incompatible.
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

No dependencies or code comments were added. Nothing was staged or committed.
Work stops at Step 7; Step 8 ISA design and later subsystems remain unimplemented.

## Step 8 — ISA v1 Design Draft

Step 8 is complete as documentation only. Read the roadmap's Steps 8–10 and
adjacent dependencies, the existing project-state documentation, shared
`architecture.rs` types and crate exports, and the Nix verification setup.
Created only the three roadmap-requested design documents:

- `docs/isa.md`: authoritative shared v1 encoding, opcode allocation, width and
  flag semantics, checked memory/control flow, precise traps, privilege,
  centralized interrupt draft, provisional procedure ABI, and Steps 9–10 handoff.
- `docs/lz32.md`: 32-bit register/pointer/address configuration, four-byte stack
  words, supported data sizes, width boundaries, and ABI differences.
- `docs/lz64.md`: 64-bit configuration, eight-byte stack words/data access,
  high-half address arithmetic, signed immediate behavior, and ABI differences.

Firm decisions: 16 ordinary writable r0–r15, separate PC/SP/status, little endian,
fixed eight-byte instructions aligned to four bytes in both modes; natural data
alignment 1/2/4 and additionally 8 only in LZ64. CALL pushes checked nextPC and
RET pops a mode-sized return address on a downward, word-aligned RAM stack.
Relative control flow uses nextPC + signed_i32(displacement)*4 without wrapping.
Arithmetic wraps at word width; addresses, access ends, SP updates, and PC+8
are checked. SUB C is borrow, signed overflow is separate, shifts normalize
modulo W and set N/Z with C/V cleared, and signed division/remainder trap for
MIN/-1 as well as all division/remainder variants trapping on zero divisor.

Supervisor/User and one active controller-owned trap frame are specified without
CPU implementation. Exact immutable pre-entry snapshots are separate from
editable resume PC/SP/status. Entry does not push to or switch the User stack;
RFE restores resume control state but leaves handler-arranged general registers.
Faults have no instruction partial effects. Interrupts are latched, selected
centrally at instruction boundaries, and deferred while a frame is active;
synchronous double traps are terminal rather than recursively overwriting state.
The procedure ABI is provisional; OS syscall numbers, service/register ABI,
boot mappings, and device interrupt assignments remain deliberately deferred.

### Firm handoff to Step 9

Implement immutable `ArchitectureConfig`, `WordWidth::W32/W64`, and validated
`FeatureSet`, with tests; do not implement opcodes or a CPU. Configuration queries
must expose word bits/bytes, pointer/address bits, register count, instruction
bytes/alignment, stack alignment, supported data sizes, and features. Use the
exact values and API semantics in `docs/isa.md` under “Architecture configuration”.
`FeatureSet` accepts exactly u32 bits 1 (BaseInteger); zero and any unknown bits
are structured errors retaining the input. Named mode constructors are infallible;
raw-feature construction is fallible. Preserve all existing Step 7 constructors,
checked-u64 arithmetic, register-index semantics, and address-domain separation.

### Firm handoff to Step 10

Implement only centralized pure width helpers and tests according to
`docs/isa.md` under “Shared width semantics”: truncation, source-width zero/sign
extension, explicit bit masking distinct from address validation, wrapping
arithmetic and consistent result flags, normalized shifts, division errors, and
checked signed address offsets/access ends. Never hide guest-address wrapping or
host overflow in a helper. Preserve typed domains; width helpers return values
or structured errors, not mutations of future CPU/status/trap objects. Instruction
metadata starts at Step 11; Steps 9–10 are not implemented by this session.

### Review and verification

Manually cross-reviewed all three documents for field coverage and reserved-zero
rules, unique opcode/selectors, byte order, mode widths, borrow predicates,
shift/division behavior, PC-relative base/scaling, atomic CALL/RET, exact trap
snapshots versus resume fields, fault ordering, feature validation, and ABI stack
slot offsets. Renamed the shared immediate-only format to IMM so TRAP's payload
is not mislabeled as a relative displacement; clarified CALL validation order and
raw-feature versus already-validated configuration construction.

An ephemeral Python arithmetic check decoded and re-encoded all four documented
byte vectors and checked selected both-mode extension, wrapping, shift amount,
PC/access-end, CALL/RET slot, and displacement-range examples. It passed using
`nix shell --inputs-from path:. nixpkgs#python3`; the first direct invocation
failed because python3 is not on the host PATH. No script, dependency, Rust test,
or executable ISA implementation was added. Remaining semantic acceptance cases
are explicitly future test obligations, not claimed implemented ISA coverage.

Required gates passed on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- 43 unit tests passed (17 diagnostics + 26 types); zero doctests.
- `nix flake check path:.` passed: workspace, formatting, and strict Clippy
  derivations evaluated and existing cached results were accepted (zero new
  checks executed). aarch64-linux was omitted as incompatible with this host.
- `nix build path:. --no-link --print-out-paths` succeeded with the existing
  foundations package. The flake source filter excludes docs, so these gates
  validate unchanged Rust foundations, not ISA document semantics.

No code, code comments, dependencies, or Nix configuration were changed. All
project files were already untracked; nothing was staged or committed. Work
stops after Step 8 documentation; Step 9 is next.

## Step 9 — Architecture Configuration

Implemented only configuration and its tests in
`crates/lazalith-types/src/config.rs`, re-exported from `src/lib.rs`.
Existing Step 7 architecture and source/diagnostic APIs are unchanged. No
comments, external dependencies, width arithmetic, opcodes, or CPU were added.

- `WordWidth::W32/W64` exposes `bits()` and `bytes()` as u8.
- `FeatureSet::base_v1()`, `try_from_bits(u32)`, and `bits()` accept exactly
  raw bits 1. Private storage prevents invalid feature sets. Structured
  `InvalidFeatureSet::UnsupportedBits { input, unsupported_bits }` takes
  priority over `MissingBaseInteger { input }`; `input()` retains the raw
  rejected u32. The error implements `Display` and `core::error::Error`.
- Immutable `ArchitectureConfig` offers `new(WordWidth, FeatureSet)`, `lz32()`,
  `lz64()`, and fallible `try_from_bits(WordWidth, u32)`. Its read-only queries
  are `word_width()`, `word_bits()`, `word_bytes()`, `pointer_bits()`,
  `address_bits()`, `register_count()`, `instruction_bytes()`,
  `instruction_alignment()`, `stack_alignment()`, `supported_data_sizes()`,
  `supports_data_size(u8)`, and `features()`, with exactly the Step 9 handoff
  types and mode values. Width-dependent properties cannot be independently set.

Nine new tests cover both complete mode query tables, width queries, every u8
size in both modes, all valid construction paths, missing BaseInteger, each of
31 unsupported bits with and without BaseInteger, combined unsupported masks
including u32::MAX, error priority, retained raw inputs/masks, and error display
and source behavior. All error cases also exercise configuration construction
in both modes. Total: 52 passing unit tests (35 types + 17 diagnostics), zero
doctests.

Successful verification on x86_64-linux, before this Step 9 report was updated:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed; aarch64-linux was
  omitted as incompatible with this host.
- `nix build path:. --no-link` — package build succeeded.

Nothing was staged or committed. Step 9 is complete; Step 10 pure width helpers
remain separate and unimplemented. Earlier step reports above are historical.

## Step 10 — Centralized Width Operations

Implemented only the shared width contract and its tests in a new
`crates/lazalith-types/src/width.rs`, re-exported from `src/lib.rs`. Existing
Step 7 architecture, Step 9 configuration, source, and diagnostic APIs are
unchanged. No comments, external dependencies, CPU, status, opcodes, or address
types were added; all operations are pure `WordWidth` methods over `u64`.

- Pure bit utilities: `mask()`, `truncate(value)`, and the explicit
  `mask_address_bits(value)` alias, all `const`. The mask is `u32::MAX as u64`
  or `u64::MAX`; no `1u64 << 64` is evaluated.
- Fallible extensions `zero_extend(value, source_bits: u8)` and
  `sign_extend(value, source_bits: u8)`. Only source widths 8/16/32/64 not
  exceeding the word width are accepted; other inputs return structured
  `WidthError::InvalidSourceWidth`, which retains the unmasked value, source
  bits, and width. Sign extension first masks to the declared source width and
  ignores higher container bits; a full-width source is an identity after
  masking.
- Infallible value-only wrapping arithmetic `wrapping_add/wrapping_sub/
  wrapping_mul` returning `u64`, with inputs masked first.
- Flagged `ArithmeticResult { value, negative, zero, carry, overflow }` from
  `add`, `sub`, `mul`, and logic helpers `bitand`, `bitor`, `bitxor`, `not`.
  Inputs are masked first. ADD carry is the unsigned sum exceeding the mask;
  SUB carry is borrow (`left < right`); MUL and all logic clear carry and
  overflow. Signed overflow follows the equal-sign/different-sign rules at the
  word sign bit. No flag inputs or status mutation exist.
- Normalized shifts: `shift_amount(value)` returns the truncated unsigned
  amount modulo W as `u8`; `shl`, `shr`, and `sar` normalize before shifting,
  so amounts W, W+1, 2W, and `u64::MAX` behave like 0 and W-1 without host
  overflow or host-signed shift behavior. SAR replicates the word sign bit.
  All return `ArithmeticResult` with N/Z recomputed and C/V cleared, including
  shift by zero.
- Fallible division `div_unsigned`, `rem_unsigned`, `div_signed`, `rem_signed`.
  Zero divisor returns `WidthError::DivisionByZero` and signed `MIN_W / -1`
  returns `WidthError::SignedDivisionOverflow` for both quotient and remainder;
  errors retain the original unmasked operands and width. Signed results
  truncate toward zero with `a = q*b + r` and `abs(r) < abs(b)`; no flags
  change on traps because no result is produced.
- Checked addresses: `validate_address(value)` accepts unchanged values at or
  below the mask, otherwise `WidthError::AddressOutOfRange`.
  `checked_address_offset(base, delta: i64)` validates the base first
  (`WidthError::InvalidOffsetBase`) and then accepts mathematical base+delta
  within `0..=M` using an `i128` intermediate
  (`WidthError::AddressOffsetOutOfRange`), covering every u64 base and the full
  scaled i32 displacement. `checked_access_end(base, size: u64)` validates the
  base (`WidthError::InvalidAccessBase`) and positive size
  (`WidthError::ZeroAccessSize`), then returns the inclusive last address
  base+size-1 via `u128` without wrapping
  (`WidthError::AccessEndOutOfRange`). All errors retain their operands and
  width; base rejection takes priority over delta/size handling. These helpers
  never hide wrapping, do not touch address-domain wrappers, and impose no
  alignment, mapping, permission, or memory-size policy.

Eighteen new tests cover both widths with boundary tables and independent
`u128`/`i128` reference grids: mask/truncate/address-mask identities, all 256
source-bit values against both extensions with retained inputs, extension
boundaries that ignore container bits above the source, add/sub carry and
signed-overflow tables, full arithmetic grids against an independent wide
reference, logic N/Z with C/V cleared, normalized shifts for 0, W-1, W, W+1,
2W, `2^32`, and `u64::MAX` against multiplication/floor-division references,
signed division sign/rounding tables including zero quotients, division error
retention for div and rem on every grid operand, unsigned MIN/-1 results,
division reference grids, address validation never masking, offset tables with
high LZ64 addresses and `i64::MIN`/`i64::MAX`/scaled-i32 deltas, access-end
tables at every boundary including `u64::MAX` sizes and `2^63` accesses, base
error priority, and per-variant Display/context checks. Total: 70 passing unit
tests (53 types + 17 diagnostics), zero doctests.

`docs/isa.md` gained a “Step 10 Rust API mapping” section documenting the
method names, signatures, error variants, and precedence rules above.

Successful verification on x86_64-linux, before this Step 10 report was added:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — all three host checks passed (workspace build/tests,
  formatting, and strict Clippy); aarch64-linux was omitted as incompatible
  with this host.
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

No dependencies were added. Nothing was staged or committed. Work stops at
Step 10; Step 11 instruction metadata remains unimplemented.

## Step 11 — Structured Instructions and Minimal Canonical Codecs

Step 11 is complete. Read the current roadmap, the entire shared ISA encoding,
opcode, selector, and metadata contract, both mode documents, shared types and
width/configuration implementations, workspace, and flake before implementation.
The existing workspace built successfully as the baseline. No earlier working
implementation was rewritten. Work stops here; Step 12 has not been implemented.

### APIs and files

Created `crates/lazalith-isa/Cargo.toml`, `src/lib.rs`, `src/metadata.rs`,
`src/operand.rs`, `src/codec.rs`, and `tests/isa.rs`. The library is `no_std` and
allocation-free, with only a local `lazalith-types` runtime dependency and local
`lazalith-diagnostics` test dependency. No external packages, SDL dependency, or
code comments were added. Existing types and diagnostics are unchanged.

- `Opcode`, `InstructionDefinition`, `InstructionFormat`, and `OperandKind`
  cover all 40 opcodes and 13 formats. One opcode declaration generates enum,
  lookup, enumeration, and metadata; one format layout supplies operand order,
  fields, and the derived reserved mask to encoder, decoder, and validation.
  Definitions also expose privilege requirement, BaseInteger, NZCV effect, and
  immediate meaning. Public read-only references make metadata reusable by
  future assembler/disassembler/compiler/documentation consumers.
- `Operand` uses strong `RegisterIndex`, `DataSize`, `Condition`, and
  `ControlRegister` values. Memory is a composite base plus signed-i32 byte
  displacement. Wider-i64 convenience constructors reject immediate overflow
  with retained input and conversion cause; no silent truncation is performed.
- `Instruction::new(config, opcode, operands)` validates before private storage;
  getters expose opcode, shared definition, and immutable operands.
  `validate(config)` and `encode(config, &instruction)` recheck mode widths.
  `decode(config, &[u8])` accepts exactly eight bytes and returns an Instruction.
  Encoder returns a new canonical little-endian `[u8; 8]`; neither codec mutates
  machine or caller state. AX order is control then register, and r0 is ordinary.
- Structured `DecodeError`, `InstructionError`, `ValidationError`,
  `OperandError`, and `UnknownOpcode` retain rejected inputs and typed causes.
  Decode checks length, opcode, every reserved bit, selectors, then mode width.
  Size selector 3 in LZ32 is InvalidWidth; invalid selectors/reserved encodings
  are distinct illegal-encoding cases. Full rejected instruction bytes survive.
  A defensive register-conversion error retains InvalidRegisterIndex but is
  unreachable under the current four-bit layout. Shared diagnostics integration
  preserves the full decode/validation cause chain without another renderer.
- Privilege and control access permissions are metadata, not codec rejection:
  CSRW read-only controls remains canonically valid for later InvalidControlState.
  No fetch, PC arithmetic, register file, status, CPU execution, assembler parser,
  disassembler, or trap delivery was introduced.

Updated workspace `Cargo.toml` and generated `Cargo.lock` for the new local crate.
Updated `flake.nix` to install `liblazalith_isa.rlib` alongside both foundations
and include `docs/isa.md` in the source filter because the opcode contract test
reads the full allocation table with `include_str!`. Added the Step 11 API,
error precedence, scope, and test coverage section to `docs/isa.md`; the design's
canonical byte assignments and semantics were not changed.

### Tests and verification

14 new ISA integration tests passed; workspace total is 84 tests (14 ISA,
53 types, 17 diagnostics), zero doctests. Tests check all opcode rows against the
actual design file, all format masks and non-overlap, exact published bytes,
and independent expected bytes for both-mode roundtrips over every register
tuple, legal selector, and nine signed immediate boundary/pattern values.
Coverage is exhaustive for opcode/format/register/selector combinations, not
all 2^32 immediate patterns or all 2^64 encodings. Malformed cases include every
unallocated opcode, each reserved bit per opcode and combined masks, selector
precedence, all raw u8 selector conversions and encoded nibbles, MEM widths,
length/count/kind errors, wide immediate bounds, mode revalidation, immutability,
selector name/value mapping, Display, and typed diagnostic cause chains.

The first strict Clippy run found a collapsible conditional, which was fixed;
subsequent verification passed. There were no failing ISA tests. Final successful
gates on x86_64-linux, completed before this report was updated:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo check --workspace && cargo test --workspace'`
- `nix flake check path:.` — formatting, strict Clippy, and workspace build/tests
  passed. aarch64-linux was omitted as incompatible and is not claimed tested.
- `nix build path:. --no-link --print-out-paths` — package build succeeded;
  output `lib/` was inspected and contains all three `.rlib` artifacts.
- Inspected the new crate for comment markers: none. Cargo.lock contains only
  the three local workspace packages. Nothing was staged or committed.

### Next handoff — Step 12 only

Implement the register file next, with private storage and validated read/write
APIs using the existing `RegisterIndex`, `ArchitectureConfig`, and centralized
`WordWidth` behavior. All sixteen general registers are writable ordinary
registers; PC/SP/status are separate, never aliases. Preserve existing shared
metadata/codecs; later CPU validation must keep fetch, canonical decode, runtime
privilege, checked nextPC, and instruction-specific checks in design order.
Continue to use `path:.` Nix commands while files remain untracked. Earlier step
reports are historical; no Step 12 or later functionality is part of Step 11.

## Step 12 — Register File

Step 12 is complete. Created `crates/lazalith-cpu` (`no_std`, zero dependencies
beyond local `lazalith-types`, no SDL, no code comments) with the first CPU-side
type, `RegisterFile`, in `src/registers.rs` and re-exported from the crate root.

- Private `[u64; 16]` storage; the array is never exposed. Construction takes
  `ArchitectureConfig`, records its `WordWidth`, and zeroes all registers.
- `read(RegisterIndex) -> u64` and `write(RegisterIndex, value)`; every write is
  truncated through the shared `WordWidth::truncate` contract (32 bits in LZ32,
  64 in LZ64), so values above the architectural word never survive. There is no
  `set`/`get` that bypasses truncation.
- All sixteen registers, including `r0`, are writable ordinary registers; reads
  of a never-written register return zero. No SP/PC/status aliasing exists at
  this layer at all; those live in later architectural state, not here.
- Raw access goes through the existing validated `RegisterIndex` type:
  `read_raw(u8) -> Result<u64, InvalidRegisterIndex>` and
  `write_raw(u8, value) -> Result<(), InvalidRegisterIndex>` reuse the Step 7
  error, which retains the rejected index. Invalid raw accesses mutate nothing.
- `word_width()` exposes the configured mode. No SP/PC/status fields, no flags,
  no execution or debug state; those are Steps 13–15.

Workspace `Cargo.toml` gained the `lazalith-cpu` member; `Cargo.lock` was
regenerated; `flake.nix` installs `liblazalith_cpu.rlib` with the other three
artifacts.

Three integration tests cover: both modes with all 16 registers starting zero
and independently writable (including `r0` and `r15`); truncation tables at
8/16/32/64-bit boundaries for every register in both modes; and every invalid
raw index 16..=255 for read and write, each retaining its exact input while the
whole file compares equal to its pre-call state. Workspace total is 87 tests
(3 CPU + 14 ISA + 17 diagnostics + 53 types), zero doctests.

Verified before this section was written, on x86_64-linux:

- `nix develop path:. -c bash -c 'cargo fmt --all && cargo fmt --all --check
  && cargo clippy --workspace --all-targets -- -D warnings && cargo check
  --workspace && cargo test --workspace'`
- `nix flake check path:.` — all host checks passed (aarch64-linux omitted,
  incompatible with this host).
- `nix build path:. --no-link --print-out-paths` — package build succeeded.

Nothing was staged or committed. Step 13 (CPU state separation) is next.

## Step 13 — Separated CPU State

Implemented `ArchitecturalState` with private configuration, register file,
`InstructionAddress` PC, `VirtualAddress` SP, and status. Construction takes
explicit PC/SP/status rather than inventing boot/reset values. General register
writes delegate to the Step 12 file; only an immutable register-file reference
is exposed, preventing replacement by a file with another mode. Privilege is
derived from status, never duplicated. `ExecutionState::Running/Halted` and
`DebugState { single_step }` are independent owner-held values, not fields of
guest state; machine lifecycle, counters, breakpoints, and traps remain later.

`validate_pc`/`validate_sp`, `set_pc`/`set_sp`, and atomic `restore_control`
validate width before alignment, PC before SP before status. Control restoration
preserves general registers. Structured `ControlStateError` retains the offending
register/input and width or typed `WidthError`/`InvalidStatus` cause. No masking,
mapping checks, fetch checks, or guest instruction privilege enforcement is
hidden in these host state APIs. Four-aligned PC is valid even when a subsequent
fetch or PC+8 would overflow; SP alignment is four/eight by mode.

The permitted minimal Step 14 dependency is `StatusRegister`: private valid
low-six-bit storage, checked raw construction retaining all rejected bits,
`bits()` and `privilege()`. No arithmetic/IE/branch methods yet.

Four new tests cover both-mode width/alignment boundaries including high-half
LZ64 and end-of-address-space values, separation/no aliases, every reserved
status bit (including above bit 31 in LZ32), constructor rejection, and failed
single/combined updates leaving the full state unchanged with retained errors.
All gates passed on x86_64-linux before this report: fmt and fmt check, strict
workspace/all-target Clippy, workspace check/test (91 tests: 7 CPU + 84 existing,
zero doctests), `nix flake check path:.`, and `nix build path:. --no-link
--print-out-paths`. Three host check derivations passed; aarch64 was omitted.
No staging/commits, external dependencies, SDL, comments, interpreter, or fake
trap controller. Step 14 is next.

## Step 14 — Centralized Status and Conditions

Completed `StatusRegister` with exact N/Z/C/V/IE/U bits 0–5; all other raw bits
are rejected, never masked. Added control-only construction, individual flag
queries, IE/privilege setters preserving other bits, `update_arithmetic` consuming
the shared `ArithmeticResult`, and `matches(lazalith_isa::Condition)` for all 15
shared conditions. No selector duplication. Architectural state delegates its
arithmetic and IE updates; raw control restoration remains atomic. These are host
APIs: the future interpreter must enforce EI/DI/RFE permissions first.

Added local `lazalith-isa` dependency and regenerated Cargo.lock; still no external
packages, allocation, SDL, or code comments. Four new tests exhaust all 64 status
values in both modes, all 64 × 16 flag replacements, all shared branch predicates,
control preservation, and real width-helper arithmetic/logic/shift/division
boundaries including borrow, overflow, and failed division leaving state intact.
The first Clippy gate rejected ambiguous operator precedence in flag packing;
parentheses fixed it. The complete rerun passed before this report: fmt/check,
strict workspace/all-target Clippy, workspace check/test (95 tests: 11 CPU + 84
existing, zero doctests), all three host Nix check outputs and package build.
aarch64-linux remains untested. Nothing staged/committed. Step 15 is next.

## Step 15 — Typed Execution Outcomes

Implemented `ExecutionOutcome::{Continue, Jump, Call, Return, Trap, Halt}` in
`src/outcome.rs`, with typed absolute/relative targets, return instruction
addresses, and syscall/software requests. Shared checked nextPC applies to all
variants; branch displacement scales by four without wrapping. CALL/RET derive
mode-sized stack updates; no SP payload supplied by callers can bypass arithmetic.
HALT checks Supervisor permission, advances once, and blocks future outcomes.
Trap returns a typed request/resume PC without changing exact pre-entry state.

`prepare_outcome` validates without mutation and returns an exclusive-borrowed,
single-use `PreparedOutcome`. Dropping it is harmless. Its `commit` invokes a
caller-owned fallible stack transaction before infallible PC/SP/execution commit;
errors leave CPU state unchanged. It does not fake memory success, mapping,
permissions, trap entry, or a controller. The documented caller contract requires
success-or-no-effect memory operations and correct RET pre-read validation order.
`OutcomeError` retains the original PC/outcome and typed cause chain. Pure
`checked_next_pc`/`checked_return_sp` expose the prevalidation needed by Step 16.

Eight tests cover all outcomes in both modes, actual little-endian test-stack
words and unchanged popped bytes, relative extrema/high-half addresses, last
representable PC/stack boundaries, invalid values and fault order, dropped plans,
failed callbacks, exact trap state/payloads, and terminal/privileged HALT.
Total: 103 tests (19 CPU + 14 ISA + 53 types + 17 diagnostics), zero doctests.
Before this report all Step 15 gates passed: fmt and fmt check, strict
workspace/all-target Clippy, workspace check/test, all three host Nix check
outputs, and `nix build path:. --no-link --print-out-paths`. aarch64-linux remains
omitted and untested. Step 14 also received a separate four-test status-suite
rerun before Step 15 began; it passed.

`docs/isa.md` contains the full Step 16 API and transaction handoff. In particular,
stage instruction effects in a candidate architectural state, validate outcome
before external side effects, and publish only after infallible final commit.
Faults are not successful Trap outcomes; RFE/controller ownership and privilege
checks remain future work. No interpreter, memory subsystem, reset policy,
trap controller, SDL, external dependency, or code comment was introduced.
Nothing was staged or committed. Work stops after Step 15.

### Per-step verification counts

| Step | New CPU tests | CPU total | Workspace total | Host Nix check outputs |
| --- | --- | --- | --- | --- |
| 12 | 3 | 3 | 87 | 3 passed + package build |
| 13 | 4 | 7 | 91 | 3 passed + package build |
| 14 | 4 | 11 | 95 | 3 passed + package build |
| 15 | 8 | 19 | 103 | 3 passed + package build |
| 16 | 5 | 24 | 108 | 3 passed + package build |
| 17 | 11 | 35 | 119 | 3 passed + package build |

Each row completed fmt/clippy/check/test and Nix checks/build, then its docs,
before implementation of the next step. Nix sometimes reports four build tasks
because Cargo vendor metadata is rebuilt; the flake defines three check outputs
(workspace, formatting, Clippy), not four independent checks. The initial
Step 12 conversational count was mistaken; the corrected 87 is authoritative.

## Step 16 — Reference CPU and Transactional Memory Boundary

`lazalith-cpu` now provides the Step 16 `ReferenceInterpreter` and the minimal
CPU-side memory boundary Step 18–21 agents must implement against:

- `ReferenceInterpreter::new(ArchitecturalState)` starts `Running`; it exposes
  `architectural_state()`, `execution_state()`, and three entry points:
  `step(&mut M)`, `step_bytes(&[u8], &mut M)`, and `execute(&Instruction, &mut M)`
  for `M: CpuMemory`. Every entry first validates execution/halt state, PC
  width/alignment, and the complete eight-byte fetch range. `step` then performs
  a fetch through the memory implementation; `step_bytes` feeds an explicit
  eight-byte sequence (test/bring-up path, no fetch transaction).
- Execution order per instruction: canonical decode and mode revalidation,
  `InstructionDefinition::supervisor_only` permission check, shared checked
  nextPC, then instruction-specific validation and arithmetic. No partial state
  is visible: effects are staged in a private candidate `ArchitecturalState`,
  outcome preparation (including CALL target/newSP and RET return-SP checks)
  completes before any memory transaction, and the candidate is published only
  after the stack transaction succeeds. A discarded candidate leaves PC, SP,
  registers, flags, and memory untouched.
- All ordinary integer/move/compare/shift/logic/division instructions, LI/GETPC/
  GETSP/SETSP/GETSTATUS, MEM loads/stores, BR (taken via
  `StatusRegister::matches`, untaken as `Continue` with no target work), JMP/
  CALL/CALLR/RET, SYSCALL/TRAP as typed `TrapRequest` events with unchanged
  pre-state, privileged HALT, and privilege-checked EI/DI. Flags come from the
  shared width helpers only; non-arithmetic instructions preserve NZCV. Loads
  extend, stores truncate, MEM effective addresses use the checked
  base+displacement/access-end contract. LZ32 Double instructions fail ISA
  validation/decode before execution; direct DataAccess construction rejects
  Double with `DataAccessError::InvalidWidth`.
- Structured faults: `CpuFault { pc, opcode, cause }` with typed causes
  (`Halted`, `PrivilegeViolation`, `Decode`, `Instruction`, `Control`, `Width`,
  `NextPc`, `Outcome`, `DataAccess`, `Fetch`, `Memory`, `OperandLayout`) and
  full `Error::source` chains. Faults are errors, never Trap outcomes. RFE,
  CSRR, and CSRW are rejected as
  `CpuFaultCause::UnsupportedUntilTrapController` (typed error, not emulation);
  their privilege violations still precede that rejection.
- No new dependencies; `lazalith-cpu` remains `no_std`. Five tests
  (`tests/reference.rs` + shared `tests/support/` RAM) run both modes: a fetched
  CALL/RET/Halt sequence with exact PC/SP/stack-word/terminal-Halt checks,
  byte-encoded fetches with flags/loads/stores, fault atomicity including a
  failing transaction, nextPC-before-effects ordering for loads/RET/CALL/HALT/
  RFE, and trap-event plus controller-instruction rejection.

### Memory contract for Steps 18–21 (CPU side, `src/memory.rs`)

- `DataAccess::new(config, base, displacement: i32, size, kind, privilege)` is
  the single validation point: supported-size check (LZ32 rejects Double),
  checked base+displacement, checked inclusive end, natural alignment. It
  retains config/address/size/kind/privilege as read-only queries. Kinds are
  `Read`, `Write`, `StackRead`, `StackWrite`.
- `trait CpuMemory { type Error: Error + 'static; }` with exactly four methods:
  `fetch_instruction(&self, config, pc, privilege) -> Result<[u8; 8], Error>`
  (eight-byte execute access, four-aligned PC, no side effects);
  `read_data(&mut self, access) -> Result<u64, Error>` (little-endian,
  zero-padded container); `write_data(&mut self, access, value: u64)`
  (truncates to access size); `peek_stack(&self, access)` (side-effect-free
  RET target read). Implementations must guarantee success-or-no-effect per
  access and enforce mapping/permissions/device policy themselves; the CPU
  never masks or splits addresses. AddressSpace/Bus/MMIO layers (Steps 18–21)
  implement this trait behind the bus; the CPU crate keeps no concrete RAM.
  The test-only RAM lives in `crates/lazalith-cpu/tests/support/mod.rs` and is
  not part of the shipped crate.

Verified for Step 16 before Step 17: fmt/fmt-check, strict workspace/all-target
Clippy, workspace check, workspace test (108 total: 24 CPU + 14 ISA + 53 types +
17 diagnostics), `nix build path:.`, and `nix flake check path:.` (3 host check
outputs). aarch64-linux remains untested. Nothing staged or committed.
Step 17 test expansion followed this gate separately.

## Step 17 — Comprehensive CPU Execution Tests

Added eleven integration test functions in `tests/comprehensive.rs`:

- Independent i128/u128 arithmetic oracles exercise all binary ALU opcodes with
  both-mode boundary grids and destination aliases r0/r1/r2/r15. Expected complete
  architectural state includes exact PC, unchanged SP, NZCV, IE/U and other
  registers. Additional cases cover ADDI/SUBI, CMP and NOT, signed overflow flags,
  borrow, normalized shifts and exact division faults.
- All 16 register destinations for LI and special/move operations; both-mode
  signed immediates, unchanged status, SETSP acceptance/rejection, and writable r0.
- All 15 branch conditions over every NZCV pattern, relative displacements,
  taken target failures and untaken hypothetical overflow without speculation.
- Every supported LDZ/LDS/ST size, sign/zero extension, little-endian truncation,
  destination/base aliases, adjacent bytes unchanged, range/alignment/mapping/
  privilege/transaction failures and no late fault after a successful device read.
- CALL/CALLR/RET stack words, unchanged popped RAM, User/Supervisor calls,
  target-before-stack and return-SP-before-peek priorities, unmapped slots,
  failing transactions, RAM-only stack policy, and subsequent target fetch.
- Privileged HALT/EI/DI, all shared CSR selectors rejected explicitly until Step
  31, halted execution through every entry point, byte decode/selector errors,
  LZ32 revalidation of Double instructions, execute-versus-read permission,
  four-aligned fetch and incomplete fetch failures, typed source chains.

The initial test draft contained compile errors and incorrect expectation tables;
it was replaced with the independent oracle/focused cases before gating. Its
subsequent failure identified a real diagnostic gap: direct `execute` now retains
its supplied opcode on early faults, including Halted, without changing priority.
All eleven tests passed alone before the full gate. Step 16 tests still pass.

Verified: fmt/fmt-check, strict workspace/all-target Clippy, workspace/all-target
check, workspace tests (119 total: 35 CPU + 14 ISA + 53 types + 17 diagnostics;
zero doctests), Nix package build and all three x86_64-linux flake checks.
aarch64-linux was omitted by Nix and remains untested. `docs/isa.md` contains the
complete production memory trait/ownership/precision contract for Steps 18–21,
including pure RET peek with exclusive memory ownership, no MMIO rollback fiction,
and the distinction between production fetch and explicit injection APIs.

The existing flake already includes new CPU sources/tests and ISA documentation;
no flake change was needed. No dependencies, code comments, staging or commits
were added. No Step 5 changes or production memory/bus/trap controller work was
performed. Step 18 is next.

## Step 18 — Checked RAM Foundation

Added `lazalith-memory` (no_std + alloc, local CPU/ISA/types dependencies only),
registered it in the workspace/lock and Nix library installation. `AddressSpace`
owns disjoint `MemoryRegion::ram` mappings with private zero-filled storage.
Inclusive ends support a single byte at the architectural maximum. Constructors
and mapping validate guest ranges, host sizes, allocation and overlap before
mutation. `initialize(PhysicalAddress, &[u8])` is a privileged host loader API:
it bypasses guest permissions but requires a nonempty complete single-region
range. No public RAM slice or mutable mapping access is exposed.

`RegionPermissions` records independent R/W/X/User bits. `AccessType` wraps the
existing CPU `DataAccessKind`; `AccessSize::Data` wraps ISA `DataSize`, with
separate Instruction and host byte-span forms. Explicit `translate_identity`
validates a virtual address before constructing its physical equivalent; no
implicit conversion or masking. Faults already retain address domain, operation,
size, optional PC/privilege and typed width/allocation causes. Access operations
and their remaining fault variants follow in Steps 19–20; no bus/devices yet.

Baseline: 119 passing tests. Step 18 adds five tests (one private-storage atomicity
unit test, four integration tests), totaling 124. One test compile failure from
iterating DataSize references was fixed. All gates then passed on x86_64-linux:
Cargo fmt/check, strict workspace/all-target Clippy, workspace/all-target check,
workspace tests, `nix flake check path:.`, and `nix build path:. --no-link`.
Nix defines three host checks; vendor metadata is a fourth build task.
aarch64-linux remains untested. Documentation completed before Step 19.

## Step 19 — Distinct Memory Operations

`AddressSpace` now separates immutable eight-byte/four-aligned execute fetch,
little-endian `read_data`/`write_data`, pure `peek_stack`, and physical debugger
`peek`. Data operations consume the CPU's already-validated `DataAccess`; no
second effective-address/size validator was invented. Configuration mismatch and
wrong operation kinds fail explicitly. All mapping, R/W/X/User, and word-sized
stack checks precede mutation. Supervisor does not bypass R/W/X. Fetch needs X,
not R, and observes prior writes. Stack kinds require the architectural word.

Debugger peek is a host inspection API: bypasses guest permissions/alignment,
requires a nonempty complete physical range in one region, never changes memory,
and leaves the destination buffer unchanged on failure. Loader has the same
range/atomicity constraints. Both remain separate from guest and stack reads.

Six new tests exhaust supported data sizes in both modes, all 16 permission
combinations and privileges, four-aligned fetch, write visibility, kind/stack
size rejection, debugger purity, mode mismatch, and adjacent-region rejection.
Memory total 11; workspace total 130; zero doctests. An intermediate compile
check caught missing Display arms while extending faults; all were implemented
before testing. Targeted tests and then full fmt/clippy/check/test and all three
host Nix checks/package build passed. Documented before advancing to Step 20.

## Step 20 — Structured Memory Faults

Every memory failure is now a `MemoryFault` carrying typed domain address,
operation, size, optional PC and privilege, and a `MemoryFaultKind` cause chain.
`data_access(base, displacement, size, kind, privilege)` validates once through
the existing CPU `DataAccess` contract and returns `InvalidDataAccess` retaining
base, displacement, and the untouched CPU error. Width, alignment, mapping
priority (Permission before CrossRegion before Unmapped), end-of-space fetch
width, and word-sized stack rules all preserve typed causes. Exact boundaries:
a one-byte access at the architectural maximum succeeds; base+1, halfword there,
or a fetch with end beyond it are rejected without masking or wrapping.

`MemoryFault` implements `Error` with a `MemoryFaultKind` source that chains
into CPU `DataAccessError`/`ControlStateError` and shared `WidthError` causes;
an integration test verifies the full chain under `lazalith-diagnostics`
(added as this crate's first dev-dependency). Failed writes leave RAM unchanged
across all fault classes; peek/initialize destinations stay intact on failure.
No new numeric trap codes were allocated.

Five new tests cover the priority/retention table, diagnostics chaining, fetch
fault context in both modes, exact maximum-address boundaries, and mapping
before permission with byte preservation. Memory total 16; workspace total 135;
zero doctests. Full fmt/clippy/check/test gates and all three host Nix checks
plus package build passed; the Cargo.lock dev-dependency update is included.
aarch64-linux remains untested. Documented before advancing to Step 21.

## Step 21 — Bus with RAM and ROM Routing, CPU Integration

Revised twice after review. The first pass had only a `CpuMemory` impl for
`AddressSpace` and a panicking `map_mmio_placeholder`; the placeholder was
removed and no `unimplemented!`, `todo!`, or other panic hook remains in the
crate (swept by grep). The second pass used `Rc<RefCell<AddressSpace>>` with a
`Clone` Bus and `&self` mutation through a `with_address_space_mut` closure;
that design permitted nested-borrow panics and hidden mutable access, so it
was replaced before Step 22.

`Bus` (`src/bus.rs`) now directly owns a private `AddressSpace`. It is not
`Clone`; there is no interior mutability, no closure-based accessor, and no
way to obtain a mutable reference or internals from `&self`. Mutating routing
takes `&mut self` (`map`, `initialize`, and the explicit `read_data`/
`write_data` transactional pair); inspection is `&self` (`peek`, `fetch_`
instruction`, `peek_stack`, `data_access` construction, and an immutable
`address_space()` accessor exposing only the space's own public read APIs).
`Bus` implements the CPU crate's `CpuMemory` trait (`fetch_instruction`,
`read_data`, `write_data`, `peek_stack`, error type `MemoryFault`) exactly as
the trait's ownership model requires: reads/fetch/peek take `&self`,
read/write take `&mut self`, so RET's pure peek through exclusive `&mut Bus`
ownership is compile-time guaranteed and hidden mutation is impossible by
type. `AddressSpace` still implements `CpuMemory` directly, preserving the
previous API. The CPU still knows no addresses or device layout. No MMIO
exists and no device API was invented; Step 22 attaches devices to this
routing without redesign.

ROM is a real region kind: `MemoryRegion::rom(config, start, contents,
permissions)` is constructed from explicit contents, rejects writable
permissions at construction (`WritableRom` fault), and serves read/fetch only.
`RegionKind { Ram, Rom }` is public with `MemoryRegion::kind()`. Loading
(`initialize`) and guest writes into ROM return the new `ReadOnly` fault
variant, typed with region start/end/kind, before any byte changes. Stack
accesses (`StackRead`/`StackWrite`) now fault with `StackRegion` unless the
containing region is RAM, after the existing word-size check: stack is
RAM-only in both directions. ROM writes are primarily rejected by the normal
R/W/X permission layer; `ReadOnly` is defense-in-depth for loaders.

`tests/bus_cpu.rs` exercises the real `Bus` without clones or closure
reentrancy: the six-instruction program (two LI, ADD, ST, LDZ +4, HALT)
fetches/loads/stores through an exclusively owned `Bus` in both modes to
Halted with r3/r4 = 47, verified stack slot, and untouched bytes beyond; a
second test checks writable-ROM construction rejection, ROM read/fetch,
permission-layer write rejection with unchanged bytes, loader rejection on
ROM, ROM kind, `StackRegion` fault for a stack peek inside ROM, and a
successful word-sized stack write in RAM. Development used standalone probes
to separate test bugs (alignment, wrong expected variant) from real behavior;
no production code changed because of them.

Memory total 18; workspace total 137 tests, zero doctests, zero failures.
Full fmt/strict Clippy/check/test passed; all three host Nix checks and
package build passed. aarch64-linux remains untested. No staging or commits.
Work stops here: Step 22 (generic devices) is next and was not started; MMIO
routing is absent, not faked.

## Initial Consequences for the Roadmap

Since the initial repository was empty:

- **Step 2** (Cargo workspace) must create everything from scratch.
- There is no working code to preserve; the "never rewrite working code"
  rule has nothing to apply to yet.
- The dependency-first ordering still applies: `lazalith-types` and
  `lazalith-diagnostics` come before CPU, ISA, or GUI code, because
  assembler/compiler/OS work will need diagnostics and shared strong types.

## Planned Platform (from `instruction.md`)

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

Implementation language: **Rust**. GUI: **SDL3** (host/frontend only, never in
core). Dev environment: **Nix Flakes**.

## Key Rules to Observe Going Forward

1. Follow the steps in order; steps are dependency-aware, not difficulty-ordered.
2. Every step: implement → test → `cargo fmt` → `cargo clippy` → `cargo test` →
   Nix checks → fix → document → next step.
3. Core (CPU, memory, bus, machine, ISA, OS logic, compilers) must stay
   independent of SDL3 and always run headlessly.
4. Use strong types (`Address`, `RegisterIndex`, `ProcessId`, ...) instead of raw
   integers.
5. No global mutable state; ownership stays explicit.
6. Validate before mutation; no hidden mutation in `peek`/`decode`/`inspect`.
7. Structured error types and one shared diagnostics system — no string errors.
8. Do not jump ahead: implement the smallest correct part of a foundation when a
   later step needs it, but do not build future subsystems prematurely.
