# LazOS Memory Model v1

## Scope

LazOS v1 uses the flat, identity-mapped memory model already defined by the
Lazalith ISA and implemented by `AddressSpace` and `Bus`. It adds explicit kernel
and User region policy, stacks, heaps, and per-process address-space ownership.
It does not implement paging, MMU state, virtual-page translation,
shared physical page objects, guard pages, or a Rust global allocator.

This model is deliberately smaller than Linux's memory subsystem while keeping
the ownership and permission boundaries needed for User/kernel isolation.

## Address model

All executable, data, stack, heap, and device addresses use the architecture's
single flat address space. Translation is explicit identity at the Bus boundary:

```text
VirtualAddress == PhysicalAddress
```

A `VirtualAddress` is never implicitly cast to `PhysicalAddress`. OS policy
constructs each mapping with the correct strong address type and validates the
inclusive complete range against the selected `ArchitectureConfig`.

The machine initially owns one active `AddressSpace`. Each `Process` owns an
independent `AddressSpace` built from the same static layout. Scheduler context
switching will atomically replace the machine's active address space and CPU
context at a kernel-owned boundary. In this non-paged model, each process space
owns separate RAM backing for the same virtual layout; v1 provides no shared
writable pages between processes. A later MMU can replace whole-space switching
with per-page translation without changing process ownership.

## Physical memory

Physical RAM is a bounded contiguous range separate from boot ROM and MMIO.
The mandatory v1 RAM window is:

```text
0x0010_0000 ..= 0x0040_ffff   (0x310000 bytes, 3.0625 MiB)
```

The trusted boot setup maps all regions below before executing the kernel.
`MemoryRegion::ram` creates zeroed RAM backing on a cold machine. MMIO is never
part of this pool and cannot be allocated as heap or stack memory.

The 3 MiB window is deliberately small and deterministic. It is enough for the
initial kernel image and one complete physically backed User layout. Step 39's
headless scheduler can exercise multiple independently backed process spaces
with an explicit aggregate admission budget, but production use must extend the
physical RAM map and reclamation policy before relying on more simultaneous
processes; the allocator will not silently map arbitrary host memory.

## Static memory layout

All ranges are mode-valid, aligned, and disjoint:

| Purpose | Start | Length | End inclusive | Permissions | User |
| --- | --- | --- | --- | --- | --- |
| Boot ROM | `0x0000_0000` | `0x0008_0000` | `0x0007_ffff` | R+X | denied |
| Kernel image | `0x0010_0000` | `0x0008_0000` | `0x0017_ffff` | R+W+X | denied |
| Kernel stack | `0x0018_0000` | `0x0001_0000` | `0x0018_ffff` | R+W | denied |
| Kernel heap | `0x0019_0000` | `0x0007_0000` | `0x001f_ffff` | R+W | denied |
| User code | `0x0020_0000` | `0x0010_0000` | `0x002f_ffff` | R+X | allowed |
| User data/heap | `0x0030_0000` | `0x0010_0000` | `0x003f_ffff` | R+W | allowed |
| User stack | `0x0040_0000` | `0x0001_0000` | `0x0040_ffff` | R+W | allowed |

The kernel image remains RWX in v1 because the Step 28 bootloader writes the
flat kernel RAM mapping before entry and there is no permission-change
instruction. User code is never writable. The kernel heap and both stacks are
never executable. Supervisor still obeys every R/W/X permission; it does not
gain access to User-denied regions by mode alone.

The mandatory map contains no device region. A later driver step must choose a
disjoint MMIO range explicitly. Device discovery cannot silently consume RAM.

## Kernel memory

Kernel memory consists of:

- the validated image loaded by the bootloader;
- one downward-growing kernel stack beginning at SP `0x0018_f000`;
- one non-executable kernel heap in `0x0019_0000..0x001f_ffff`; and
- future allocator metadata owned by LazOS, never mapped for User access.

The kernel stack is distinct from every User stack. Trap entry does not switch SP
automatically, so kernel trap code must arrange a safe Supervisor stack before
calling procedures. The initial SP leaves a 4 KiB top margin inside the mapped
stack region.

Kernel allocation and User allocation metadata are not architectural memory and
must not be placed in User-accessible ranges. The Step 34 `KernelMemory` and
`UserMemory` layouts hard-code this map and their pools are bounded by the
regions defined here, so disjointness is structural rather than re-validated per
pool.

## User memory

A User process receives three logical regions:

1. executable/read-only code at `0x0020_0000`;
2. writable data and heap at `0x0030_0000`; and
3. a writable, non-executable stack at `0x0040_0000`.

The initial User SP is `0x0040_f000`. It is inside the stack mapping and aligned
to both four-byte LZ32 and eight-byte LZ64 stack words. Stacks grow toward lower
addresses under the existing CALL/RET contract. v1 has no guard page; the memory
manager validates the reserved range, while hardware still checks every mapped
access.

User code, data, heap, and stack have `user=true`. The kernel image, kernel
stack, kernel heap, boot ROM, and future kernel-only MMIO have `user=false`.
Debugger `peek` is not a User access mechanism and does not relax these rules.

## Heaps

The kernel and each User data/heap region use a simple contiguous bump allocator
in v1:

- allocation validates nonzero size, requested alignment, complete range, pool
  capacity, and arithmetic overflow before mutation;
- successful allocation advances one cursor;
- exhaustion fails without changing the cursor;
- a fresh process space starts with a zero cursor and zeroed RAM;
- v1 does not implement individual free, reallocation, fragmentation control,
  compaction, or guard pages.

A later allocator may replace this policy behind the same kernel-owned API.
Guest software receives allocation results only through a future validated
system call; it cannot modify pool metadata.

## Address spaces and context switching

Each process owns one `AddressSpace` plus its explicit User layout. Creating
a process constructs and validates a complete candidate address space before
publishing the process. Loading code uses the same atomic rule: no live process
is partially remapped.

At a scheduler boundary, Step 39:

1. validates that the currently running thread is stopped at a safe kernel/User
   boundary with no active trap frame;
2. preserves its complete User CPU context;
3. transfers only User regions and the active address-space identity through a
   typed, pre-reserved, overlap-checked operation while retaining the machine's
   Supervisor regions;
4. installs the selected process's CPU context and a fresh execution token; and
5. returns the previous live User space to its owning process before selecting
   the next ready process.

The process's logical identity remains stable while the underlying active
`AddressSpace` identity travels with the swapped User regions. Services receive
only the restricted `UserSpace` capability and validated memory operations; raw
live address-space mutation and direct controller access are not scheduler
APIs. There is no concurrent mutation of an active address space. Inspection
uses read-only APIs; scheduler switching is an explicit ownership transfer, not a
hidden `Arc<Mutex<AddressSpace>>` convention. The scheduler has an aggregate
User-space admission budget; production capacity still accounts for the fixed
kernel map and future physical backing reclamation.

## Permissions

Every region has independent read, write, execute, and User bits. The memory
layer checks:

- mode-width range validity;
- complete mapped-range containment;
- access kind (fetch, data, stack, loader, debugger peek);
- R/W/X permission; and
- `user=false` rejection for User execution.

Supervisor mode bypasses none of these permission checks. Host boot/load APIs are
separate trusted operations and are never callable by User software.

## Stacks

Both stacks use the architectural downward direction and mode-sized words.
CALL/RET validate and access only RAM stack words. Stack pointers and initial SP
values are checked with `stack_alignment()`. A stack range is a memory ownership
boundary, not a new CPU address type or implicit stack-limit register.

The v1 model has no dynamic stack growth, stack coloring, alternate stack for
User traps, or guard-page fault. Kernel software switches or establishes a safe
Supervisor stack explicitly before calling procedures.

## Paging and future extension

v1 has no virtual pages and therefore defines no active page size. Instructions
are four-byte aligned, and Four KiB is only the planned minimum future page
size; Step 33 does not expose `PageNumber`, page tables,
MMU state, TLB behavior, demand paging, copy-on-write, or shared mappings.

A future MMU can be inserted at the Bus translation boundary. Process and kernel
ownership, strong address domains, complete-range validation, permission checks,
and validate-before-mutate rules remain unchanged.

## Required implementation scope for Step 34

Step 34 implements only:

- a checked aligned bump pool;
- a kernel/user memory layout matching this document;
- initial kernel and User stack values;
- kernel and per-process heap metadata ownership; and
- focused integration tests with real Bus/CPU permission and CALL/RET behavior.

It does not create processes, switch contexts, parse executables, expose system
calls, implement paging, or attach a global allocator.
