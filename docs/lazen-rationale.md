# Lazen Rationale

Lazen exists to answer one question:

> What language would make building native Lazalith applications easy?

The answer is not a general-purpose language that hides Lazalith. It is a
small, explicit language that makes the platform's real constraints visible
while keeping everyday application code pleasant.

## Why a separate language

Lazalith already has several layers: a typed ISA, a protected memory model, a
trap-based syscall boundary, a virtual filesystem, and native toolchain
formats. A language that assumes a large host, unbounded integers, implicit
ownership, or a garbage collector would make those layers harder to reason
about.

Lazen instead treats the platform as the specification:

- `int` follows the selected word width;
- pointers are checked against a named memory region;
- privilege is explicit;
- system calls are typed wrappers over the existing ABI;
- objects map directly to `.lzo`;
- the compiler can therefore explain every machine instruction it emits.

## Ease means fewer surprises

A native application is easy to build when the compiler can answer these
questions early:

1. Which architecture and ABI is this program for?
2. Where does every pointer point, and who may access it?
3. Which instructions are User-safe?
4. Which symbols and relocations will the linker resolve?
5. Which source span explains every rejected operation?

Lazen's type and memory rules are chosen to make those answers local and
precise. A `slice` carries a length. A `ptr<T>` carries its target type. A
module export becomes a symbol with a source location. A syscall wrapper
carries the ABI result type. There is no implicit host allocation or implicit
privilege transition.

## Why not copy an existing language

Copying C would encourage undefined behavior, implicit conversions, and raw
memory patterns that are difficult to validate against LazOS. Copying Rust
would introduce ownership and lifetime machinery before the platform and
toolchain contracts are stable. Copying Pascal or Go would import conventions
for packages, runtime state, and error handling that do not match the ISA.

Lazen keeps familiar words where they are clear, but its grammar, types, and
runtime model are its own. It should feel like a systems language with strong
diagnostics, not a dialect of another project.

## Memory is a feature

Native applications need to place data in User memory, use a terminal, open a
file, and pass buffers to the kernel. Lazen makes those operations visible:

```text
let text = "hello";
let result = write(1, slice<byte>(text));
```

The compiler lowers the slice to a pointer/length pair and the call to the
existing validated ABI. It can reject a buffer that is out of bounds before a
syscall traps. A future region-aware type can strengthen this without changing
the object format.

## Determinism is a feature

Lazen does not expose wall-clock time, random host state, filesystem shortcuts,
or network calls implicitly in its core language. Time and entropy, when
reached, are explicit OS services obtained through a declared wrapper such as
`time()`, and remain deterministic in headless execution. This
makes the same source produce the same observable behavior in a unit test, a
boot session, and a future SDL frontend.

## Smallness is a feature

The first compiler can be built in stages:

1. lexer and diagnostics;
2. modules, declarations, and explicit types;
3. expressions and control flow;
4. lower to the existing ISA;
5. emit `.lzo` sections, symbols, and relocations.

A small language keeps each stage understandable and makes it possible to
compare compiler output against the reference CPU and canonical codec. It also
prevents a runtime library from becoming a second, undocumented operating
system.

## Safety and privilege

Lazen rejects Supervisor-only instructions in User code at compile time where
the target region is known. It does not provide a convenient escape hatch for
raw privileged assembly. System services are the supported boundary, and their
results are structured. This matches the LazOS principle that User software
must not obtain a backdoor through a hidden runtime entry point.

The compiler may later offer a narrowly scoped inline-assembly escape hatch,
but it should require an explicit target and preserve source spans and
privilege checks. It is not a default v1 feature.

## Toolchain ownership

Lazen does not replace the existing pipeline. It feeds the same components:

```text
Lazen -> .lzo -> linker -> .lzx -> LazOS loader -> process
```

The linker, disassembler, relocation model, and OS ABI remain shared. This is
important because a language-specific executable format would duplicate
architecture validation and make hand-written assembly a second-class citizen.

## Success criteria

Lazen v1 is successful when a new developer can:

- write a module with a typed entry procedure;
- place text and buffers in explicit regions;
- call a terminal or file service without raw syscall numbers;
- receive a precise diagnostic for a bad pointer, type, or privilege use;
- assemble and link the result with existing `.lzo`/`.lzx` tools; and
- run the resulting program headlessly with deterministic output.

These criteria measure usefulness without requiring the language to imitate a
larger ecosystem before the platform is ready.
