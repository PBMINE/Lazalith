# The Linux 0.01 architecture port

`binstruction.md` §50 requires this document to exist **before** implementation
begins, and lists what it must contain. This is that document.

**Status: source pinned and inspected. No port work done. No LZA compatibility
machine. No x86 emulation.**

---

## 1. Upstream source and the exact pinned revision

**Requirement** (`binstruction.md` §41, §42). The authoritative source is
`binstruction.md`-specified and is not substituted:

```text
https://github.com/zavg/linux-0.01.git
```

The repository has two commits. The `master` head is:

```text
5839d67d5825265fc665c9dc0ec2e767ff47a6dd   "Create README.md"   2013-10-03T17:57:40Z
5839d67d5825265fc665c9dc0ec2e767ff47a6dd   is refs/heads/master
```

Its parent, and the commit that actually added the kernel, is:

```text
6bdd50cda07d0b548af99f4d605d9812fa1e99fa   "first commit"       2013-10-03T17:44:51Z
```

**The pin is `5839d67d5825265fc665c9dc0ec2e767ff47a6dd`** — the `master` head, which
is a superset of its parent and is what a `git clone` of this repository gives.

**The pin is a commit, never a branch.** The repository's own history is not
rewritten, and a moving branch is not tracked, because a reproducible build from a
moving ref is not a reproducible build.

**Provenance for every future LZA change**, per §49: upstream files stay
recognisably upstream, LZA changes live in an explicitly separate layer, and a
modified file is never presented as an original. Concretely, that means a
`lza/` directory beside the upstream tree rather than edits in place, and a
manifest that names every file the port touched and why.

**Note on other trees.** Several forks of this source exist that are *modified*
to build with a modern GCC. `binstruction.md` §41 says not to substitute another
tree without documenting why, so: none is used, and if one ever is, the reason gets
written here first.

---

## 2. What the source actually contains

**Fact**, from the pinned tree, 84 files:

```text
boot/       boot.s, head.s
kernel/     sched.c, system_call.s, traps.c, asm.s, fork.c, exit.c, hd.c,
            console.c, serial.c, printk.c, panic.c, tty_io.c, vsprintf.c,
            keyboard.s, mktime.c, sys.c
mm/         memory.c, page.s
fs/         21 files: exec.c, namei.c, super.c, inode.c, read_write.c, open.c,
            tty_ioctl.c, block_dev.c, char_dev.c, file_dev.c, pipe.c, ...
lib/        12 files: write.c, open.c, close.c, execve.c, wait.c, _exit.c, ...
init/       main.c
include/    linux/, asm/, sys/, plus libc-ish headers
tools/      build.c
Makefile
```

`tools/build.c` is worth noting on its own: a build tool **written in C** that is
compiled by the host compiler to build the kernel. It is a bootstrap
chicken-and-egg problem that already has a precedent in the source, and a Lazalith
port inherits it — whatever builds the first `lazcc` cannot be `lazcc`.

---

## 3. Where the x86 assumptions are, from the source

**Fact.** These are quoted or paraphrased from the pinned revision, with the file
each came from. `binstruction.md` §45 says: "Do not simply pass them through the LZA
compiler." Each row says what happens to it.

### 3.1 Boot (`boot/head.s`)

| Upstream | What it does | Decision |
| --- | --- | --- |
| starts at absolute address `0x00000000`, which is also where the page directory will be | the first 4 KiB of physical memory is both code and a page table | **removed.** LZA boots from `BOOT_ROM_START` with a separate load address; `KERNEL_LOAD_ADDRESS` is `0x0010_0000` and the ROM header is at `0x0000_0400` |
| `mov %ax,%ds/%es/%fs/%gs` after loading a GDT | segment registers | **removed as a concept.** LZA has no segment registers; `Privilege::User` / `Privilege::Supervisor` in the status register is the equivalent |
| `setup_idt` writes 256 entries of `{offset, 0x8E00}` with selector `0x0008` | an IDT of 32-bit interrupt gates, DPL 0 | **replaced.** LZA has `TrapController`, a `TVEC`, and `TrapCause`; the number of causes and their priority are an architectural decision, not 256 gates |
| `setup_gdt` | a GDT | **removed as a concept**, for the same reason as segments |
| the A20 check: write to `0x000000`, compare `0x100000`, loop | prove the address line is on | **removed.** A20 is a property of the 8086's external hardware and has no LZA equivalent |
| `movl %eax,%cr0; andl $0x80000011; testl $0x10` else set the emulate bit | detect a 387, set the EM bit | **removed as a concept.** LZA has no FPU and no control-register bit for one |
| `jmp after_page_tables` | protected mode with paging on from the start | **removed as a concept.** LZA has no paging; see §3.5 |

### 3.2 Port I/O (`include/asm/io.h`)

```c
#define outb(value,port) __asm__ ("outb %%al,%%dx"::"a" (value),"d" (port))
#define inb(port) ({ unsigned char _v;
    __asm__ volatile ("inb %%dx,%%al":"=a" (_v):"d" (port)); _v; })
```

plus `_p` variants that insert a `jmp` pair for the I/O wait cycle.

**Decision: this is the single largest piece of work in the port, and it is a
missing ISA feature rather than a translation problem.** LZA has **no port I/O at
all** — no `IN`, no `OUT`, no I/O address space, and therefore no PIO in the
machine's memory model. The M40T and M41T work is to decide whether LZA gains a
PIO space, and if so where it sits in the machine profile.

`binstruction.md` §25 lists "MMIO/PIO map" as a property of a machine profile, so
a PIO space is contemplated but not designed. It has to be designed before the port
can proceed, and the alternative — rewriting every device access as an MMIO window —
is a change to 11 device driver files and is worse.

The `_p` wait variants are the interesting part: they are a *timing* hack for a bus
that needed a settle cycle. An LZA device that reports a settle time in a register
would be a better design; whether that is in scope is an open question recorded in
§6.

### 3.3 Segment overrides (`include/asm/segment.h`)

```c
extern inline unsigned char get_fs_byte(const char * addr)
{ unsigned register char _v;
  __asm__ ("movb %%fs:%1,%0":"=r" (_v):"m" (*addr)); return _v; }
```

and `put_fs_byte` / `get_fs_long` / `put_fs_long` to match, using the `fs` segment
to reach *user* memory from kernel code.

**Decision: LZA has a privilege model, not segments.** `Privilege::User` is in the
status register and a user access is checked against the *active* address space's
region permissions, which the machine already enforces — `activate_user_context`
swaps the process's `AddressSpace` into the bus, and `UserSpace` is what a syscall
gets. The direct analogue of `get_fs_byte` is a checked user-memory read, which
`UserMemoryContext::read_bytes` / `write_bytes` already are.

**This is the one place where the port is not adding work but *removing* it**:
LZA's context-switching address-space model makes `fs`-relative access
unnecessary, so 8 `extern inline` functions in the source become ordinary checked
calls. The `memcpy` in `include/asm/memory.h` has the same dependency:
`cld; rep; movsb` with the comment that it "assumes ds=es=normal data segment".

### 3.4 Privilege transitions (`include/asm/system.h`)

```c
#define move_to_user_mode() \
__asm__ ("movl %%esp,%%eax\n\t" \
    "pushl $0x17\n\t" "pushl %%eax\n\t" "pushfl\n\t" "pushl $0x0f\n\t" \
    "pushl $1f\n\t" "iret\n\t" ...
#define sti() __asm__ ("sti"::)
#define cli() __asm__ ("cli"::)
#define nop() __asm__ ("nop"::)
#define iret() __asm__ ("iret"::)
```

**Decision.** `move_to_user_mode` becomes LZA's `EI` (enable interrupts) plus a
return from a trap, which is what the `RFE` opcode is. `sti`/`cli` are `EI`/`DI`
and already exist. `nop` exists. `iret` has no analogue because LZA's trap
mechanism is not an instruction-stack unwind — see §3.6.

### 3.5 Paging (`include/asm/memory.h`, `mm/memory.c`, `mm/page.s`)

`memcpy` is `cld; rep; movsb`. `mm/page.s` is a hand-written page-fault and
copy-page routine. `head.s` builds page tables at address 0.

**Decision: this is the largest *conceptual* gap, and it is not one.** LZA has no
paging, no page tables, no `CR2`/`CR3`, and no demand-fault model.
`binstruction.md` §33 of the Phase-I roadmap anticipated this: "If paging is too
much for the first OS milestone, use the simplest protection model that still gives
a clean path toward paging later", and `docs/os-memory.md` is what that produced —
a region-based address space with permissions and an identity for context
switching.

`mm/memory.c` allocates and frees *pages* (`get_free_page`, `free_page`) and
`mm/page.s` handles the fault. A faithful port therefore has to choose:

- **implement paging in LZA**, which is a new architectural feature, an MMU in the
  VM core, and a `lza64-native-v1` profile that can turn it on — or
- **provide page granularity in software**, by making the allocator's page a region
  and the "fault" an explicit check.

The second is what a Phase-I-style design points at, and the first is what the
source assumes. This is a decision for B25/B27, not something to settle in a
document. It is recorded in §6 as the largest open question in the port.

### 3.6 Traps and interrupts (`kernel/traps.c`, `kernel/system_call.s`)

`traps.c` builds an IDT with handlers for divide error, debug, NMI, breakpoint,
overflow, bounds check, general protection, page fault, and the coprocessor
errors, pushes `error code` / `segment` / `EIP` / `CS` / `EFLAGS` and calls
`spurious_irq`/`default` handlers. `system_call.s` does `int $0x80` and pushes all
of `gs`, `fs`, `es`, `ds`, `edi`, `esi`, `ebp`, `eax`, `ebx`, `edx`, `ecx`, `ebp`,
`eip`, `cs`, `eflags`.

**Decision.** The *mechanism* maps onto LZA's `TrapController`: a `TVEC`, a
`TrapCause`, a frame that saves the registers and the return control, and `RFE` to
return. The *table* of 16 x86 vectors does not: LZA's `TrapCause` is the
architectural set, and the port maps the causes Linux cares about onto the subset
that exists and refuses the ones it does not.

The syscall entry is the interesting one. Linux 0.01 uses `int $0x80` with the
syscall number in `eax` and arguments in `ebx`/`ecx`/`edx`. LZA's ABI puts the
number in `r0` and six argument words in `r1`–`r6`, with `r7` reserved-zero —
which `lazalith-os-abi` already defines and `return_from_syscall` already validates.
The port therefore **rewrites `system_call.s`**, and the `SyscallRequest` /
`UserMemoryContext` validation the Phase-I kernel already does is exactly the
validation the x86 version skipped.

### 3.7 Devices

Proven from the source, not assumed. `binstruction.md` §46: "Do not assume a device
is required merely because it existed on the historical PC. Prove the dependency
from source."

| Device | Proven dependency | Files |
| --- | --- | --- |
| **Console (VGA text)** | `kernel/console.c` defines `SCREEN_START 0xb8000`, `SCREEN_END 0xc0000`, `LINES 25`, `COLUMNS 80`, and writes character/attribute pairs directly into that memory | `kernel/console.c` |
| **Keyboard (PS/2)** | `kernel/keyboard.s` does `inb $0x60` for the scan code, tests for the `0xe0`/`0xe1` extended prefixes, reads `0x61` for the acknowledge, and dispatches through a 128-entry `key_table`; it is wired to `keyboard_interrupt` and produces escape sequences for the arrows | `kernel/keyboard.s`, `fs/tty_ioctl.c` |
| **Hard disk (AT/IDE)** | `include/linux/hdreg.h` gives the port map — `HD_DATA 0x1f0`, `HD_ERROR 0x1f1`, and the rest — plus a hardcoded geometry: `HARD_DISK_TYPE 17`, `_CYL 977`, `_HEAD 5`, `_SECT 17`, and a comment that says "We don't use BIOS for anything else, why should we get HD-type from it?" | `kernel/hd.c`, `include/linux/hdreg.h`, `fs/block_dev.c` |
| **Serial** | `kernel/serial.c` and `kernel/rs_io.s` drive the 8250/16550 UART | those files |

**What is NOT proven to be needed, and therefore is not in the inventory:**
a mouse, a real-time clock, a network card, sound, a second hard disk. The
`RELNOTES-0.01` text in other mirrors of this release says "Only a subset of
AT-hardware is supported (hard-disk, screen, keyboard and serial lines)", which
matches the four rows above exactly. Anything beyond those four would be a device
added from a checklist, which `binstruction.md` §51 forbids.

**Implication for `lza64-at-v1`:** the profile needs a *presentation* device
comparable to VGA text memory, a keyboard device with a scan-code interface
including the `0xe0` prefix, a block device, and a serial device. All four are
compatibility hardware and all four are absent. `docs/compatibility.md` and
`docs/machine-profiles.md` cover what building them means.

---

## 4. The port, and what it is not

**Requirement** (`binstruction.md` §44). The goal is:

```text
Original Linux 0.01
  ↓  identify x86/AT-specific assumptions
LZA architecture adaptation
  ↓
LZA64
```

**This is not turning LZA into an x86 emulator.** Concretely, from the evidence
above, the port does *not* produce:

- segment registers, or a GDT, or an IDT of x86 gates;
- paging, page tables, or a `CR3`;
- `in`/`out`, or an I/O address space — unless M40T adds one deliberately;
- A20, the 387 detect, or the EM bit;
- any x86 instruction in the ISA table.

And the port does *not* pass x86 inline assembly through the LZA C compiler. §45
lists the assumptions to identify and says "do not simply pass them through"; the
inline assembly in `io.h`, `segment.h`, `system.h` and `memory.h` is exactly the
material that gets rewritten rather than translated.

---

## 5. What the port needs from the toolchain

**Requirement** (`binstruction.md` §48). The kernel must be buildable with:

```text
Linux 0.01-derived C   →  Lazalith C compiler
LZA architecture asm   →  Lazalith assembler
                              ↓
                         .lzo objects
                              ↓
                         lazld
                              ↓
                         kernel.lzx
```

**Host GCC is not the target compiler.** It may be used for comparison experiments.

**Four toolchain capabilities the port needs, and the state of each:**

| Need | State |
| --- | --- |
| C with `static` initialisers, arrays, structs, function pointers | real — `lazalith-c-compiler` |
| C **macros** | **absent.** No preprocessor. `kernel/sched.c`, `kernel/printk.c` and most of `include/linux/*.h` use macros heavily. This is the single largest toolchain gap for the port |
| `#include` | **absent.** Headers are not on a sysroot; they are composed in memory |
| freestanding compilation (no hosted runtime, no libc) | **absent.** B15 |
| assembly objects linking with C objects into one image | real in shape — both produce `.lzo` and the linker does not care — **not demonstrated by a test** |

**The `extern inline` problem.** `include/asm/segment.h` and `include/asm/memory.h`
use GCC's `extern inline` and statement-extended `asm`. Both are compiler
extensions, and the C compiler does not support inline assembly at all. So the
port rewrites those headers rather than compiling them, which is a *change to
upstream files* and therefore has to live in the LZA layer with its provenance
recorded, per §49.

---

## 6. Open questions, with what would settle each

Ordered by how much of the port each one blocks. These are the honest unknowns;
none of them has a decided answer in this repository.

| Question | What would settle it |
| --- | --- |
| **Does LZA get a port I/O space?** | An architectural decision in M40T, recorded in `docs/isa.md` and a profile's MMIO/PIO map. Without it, 11 driver files change shape |
| **Paging, or software page granularity?** | An M40T decision. Software granularity is what the Phase-I region model points at; x86 paging is what the source assumes. This is the largest single gap |
| **What is the syscall ABI for a ported kernel?** | Largely settled: `lazalith-os-abi` already defines numbers, records, capabilities and a `r7`-must-be-zero rule. The port needs a *mapping* from Linux's syscall numbers to LZA's, and that mapping is data |
| **Does the LZA C compiler need a preprocessor, and how much?** | Measured against the source: what subset of the macros in `include/linux/*.h` and `kernel/*.c` the port actually uses. §45's rule applies — prove it from the source |
| **Does an LZA device report a settle time, or is the `_p` wait a fixed cycle?** | A device-model decision in M50T. The `_p` variants exist because the 8250 needed a delay, and a fixed cycle in LZA would be the same hack one layer down |
| **How faithful must the console be?** | `console.c` is 25×80 text at `0xb8000` with attributes. A `lza64-at-v1` presentation device matching that is a compatibility device; a lazalith-native text console is not, and only the former makes the port a port |
| **What is the process/thread model in LZA terms?** | `kernel/fork.c` and `mm/memory.c` assume x86 `CR3` manipulation for the address space. LZA's `activate_user_context` swaps a whole `AddressSpace`, which is a different and simpler operation — the port simplifies rather than complicates here, and that is worth noting because it is the direction the port should move in |

---

## 7. What has NOT been done

Stated plainly, because `binstruction.md` §56 requires it and because this document
is the one a reader is most likely to over-read:

- Nothing has been ported. No Linux 0.01 code has been compiled by the Lazalith
  toolchain.
- The upstream tree has not been cloned into this repository. It was **read** at
  the pinned revision over HTTPS, and the findings in §3 are from that read.
- No `lza64-at-v1` profile exists.
- No VGA, PS/2 keyboard, AT block or serial device exists in any form.
- No LZA port I/O space, no paging, and no privilege-transition rework has been
  attempted.
- The device inventory in §3.7 is derived from four files. A full read of all 84
  files, as `binstruction.md` §43 requires before implementation, has not been
  done — §3 is the start of that read, not the end of it.

---

## Related

- `docs/compatibility.md` — the tier these devices belong in, and what it must not be
- `docs/machine-profiles.md` — where `lza64-at-v1` is described
- `docs/toolchain.md` — the four toolchain gaps this port depends on
- `docs/virtual-machine.md` — the VM contract a compatibility device must not break
- `docs/architecture.md` — the platform contract
