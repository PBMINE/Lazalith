# Lazen Application Packages (`.lza`)

This document is Step 88 of the roadmap. Step 56 defined the *manifest* — the
source-level package a person authors. This one defines the **package** — the
single file a person shares and installs — and, deliberately, the parts of it that
are still open.

The roadmap says not to finalize the format before understanding actual application
requirements. The requirements were read out of the repository rather than invented:
what follows cites the code that imposes each one, and the last section says plainly
what could not be determined and why.

## The three units, so nothing is confused for anything else

```text
application   lazen.toml + src/     what a person authors      (step 56)
executable    .lzx                  what LazOS loads           (existing)
package       .lza                  what a person shares       (this document)
```

A `.lza` is **not** a new executable format and **not** a replacement for the
manifest. It is the manifest's identity, the executable's bytes, and nothing else,
in one file that can be handed to someone who has no toolchain.

The design rule that follows from this: a `.lza` contains exactly one `.lzx`, and
that `.lzx` is authoritative for everything about *execution*. A package must never
be able to say something about execution the executable does not also say. Anything
in the package header that duplicates an `.lzx` field is a second source of truth,
and this repository has been bitten by exactly that before — see
`docs/lzo.md`'s rule that an object's table offsets are checked against the ones the
writer would produce, and the idempotence property step 86's fuzzer found.

## What the repository already knows, and what it does not

This is the investigation the roadmap asks for. Each row is a fact in the code, not
a guess.

| Question | Answer today | Where |
| --- | --- | --- |
| How is a program identified at run time? | **A path.** `SpawnProcess` validates `path` and `path_length` and nothing else. | `crates/lazalith-os/src/syscall.rs:853` |
| What does the kernel do with a path? | **Nothing yet.** `LazalithKernel::start_image` takes an `LzxImage` by value. There is no path→image resolution anywhere. | `crates/lazalith-os/src/kernel.rs:199` |
| What is in an `.lzx`? | architecture, ISA/ABI version, entry, required data and stack, ≤3 sections, and an optional source-level debug block. | `crates/lazalith-os/src/lzx.rs:158` |
| What are the executable's limits? | magic `LZXLOAD1`, format 2, 64-byte header, 48-byte section entries, ≤3 sections, ≤4 MiB. | `crates/lazalith-os/src/lzx.rs:10` |
| Where does an application's identity live today? | Only in `lazen.toml`, which is a *source* file. A built `.lzx` has no name and no version. | `docs/lazen-applications.md` |
| Where do resources go? | Embedded into the executable's read-only data as constant byte arrays. The *names* are not preserved. | `docs/lazen-applications.md` |
| Where do permissions go? | Declared in the manifest and "checked against the source", but there is no field in the `.lzx` for them. | `docs/lazen-applications.md` |
| What is the largest thing the VFS can hold? | 1 MiB per file, 1024 nodes, 1024 directory entries. | `crates/lazalith-os/src/filesystem.rs:11` |
| What does the shell do? | `build_init_shell_image` produces a native image; there is no program-launching path yet. | `crates/lazalith-os/src/native_shell.rs` |
| What CLI commands exist? | `new`, `check`, `build`, `run`, `test`. No `pack`, no `install`. | `crates/lazalith-cli/src/main.rs:191` |

Two of those are the design drivers.

**The path is the identity a process has at run time, and nothing can turn a path
into an executable today.** That gap is why a package needs a *resolution rule* and
not just a container, and it is the single most important requirement in this
document.

**A `.lzx` has no name and no version.** So a bare `.lzx` cannot answer "which
application is this, and may I start it?" — which is what an installer, a package
manager, and a capability check all need. That is what the package header is for.

## The container

A `.lza` is a header, a table, and a payload, in the same spirit as `.lzx` and
`.lzo`: fixed-size records, counts in the header, everything derived, and a reader
that checks each offset against the one the writer would produce.

```text
offset  size  field
     0     8  magic "LZAAPPL1"
     8     2  container version (1)
    10     2  flags (reserved, must be 0)
    12     4  manifest length
    16     4  executable length
    20     4  resource count
    24     4  entry name offset   (into the manifest)
    28     4  entry name length
    32    ...  resource table: name offset, name length, data offset, data length
         ...  manifest bytes       (the lazen.toml, verbatim)
         ...  executable bytes     (a complete .lzx)
         ...  resource data
```

Four rules, each of which exists because of something in this repository:

1. **The executable is a complete `.lzx`**, stored verbatim, with its own magic and
   its own header. A package reader does not parse it to find the entry; it hands
   the bytes to `LzxImage::from_bytes`. If the package header and the `.lzx` header
   ever disagree, the `.lzx` wins, and a reader that finds a disagreement reports it
   rather than choosing.
2. **The manifest is stored verbatim**, not re-serialized. It is the exact file the
   person wrote, so a diff of two packages' manifests is a diff of the source, and a
   tool never has to agree with itself about a TOML parser.
3. **Offsets are canonical.** A reader checks every offset against the value the
   writer would have produced for the counts it read, exactly as `check_table_offset`
   does in `crates/lazalith-toolchain/src/lzo.rs`. A non-canonical offset is
   refused, not tolerated — step 86's fuzzer showed what tolerating one costs: a
   file that reads cleanly and writes back as a *different* file.
4. **Ids are lowercase, and lengths are checked before anything is allocated.** Every
   count is bounded against the bytes present before a `Vec` is reserved, so a
   crafted count cannot ask for more memory than the file could describe.

## Identity: the four fields the manifest owns

```toml
[application]
name = "hello"
version = "0.1.0"
architecture = "any"
```

| Field | Rule | Why the rule |
| --- | --- | --- |
| `name` | 1–32 bytes, `[a-z0-9-]`, no leading or trailing `-` | It becomes a path component, and the VFS splits paths on `/`. A name that can contain `/` is a name that can escape its own directory. |
| `version` | `major.minor.patch`, each ≤ 65535 | Step 89 needs to compare and order versions, and a total order needs a total domain. |
| `architecture` | `any`, `lz32`, `lz64` | Already fixed in step 56. `any` in a *package* means "the contained `.lzx` decides", which is stronger than at build time. |
| `permissions` | the four booleans from step 56 | Already declared; see the gap below. |

`name` and `version` together are the package's **identity**, and identity is what
makes a package *replaceable*: installing `hello 0.2.0` over `hello 0.1.0` is one
file replaced, not a directory tree merged.

## Resources, and why they are in the package and not only in the executable

Step 56 embeds resources into the executable's read-only data as constant arrays.
That is right for *code* and wrong for *identity*: the executable holds the bytes
and loses the names, so a program cannot ask "what resources am I?" and a package
manager cannot show a user what an application carries.

So the package keeps both: the names in its table, and the bytes already in the
executable. The design does **not** duplicate the bytes. A resource in the table is
a *label* for a constant the compiler emitted; resolving a label to bytes is the
runtime's business, and until the runtime has a table, the package's table is what a
tool would read. This is a real loose end and it is recorded as such below.

## The resolution rule, which is the point

`SpawnProcess` names a path. Something must turn that path into bytes. The rule:

```text
a path names a package when the VFS file at that path is a .lza
a path names an executable when the VFS file at that path is a .lzx
a path that names neither is NotFound, whatever it contains
```

Deciding by **content**, not by **name**, is deliberate:

- A VFS that has no separate executable flag on a node (and today it does not — see
  `FileMetadata`) cannot decide by name without one, so content is what it has.
- A user can rename a file; the kernel should not care. Deciding by name makes
  `install` a naming convention and `run` a privilege question.
- It means a package and a bare executable are interchangeable for the loader, so a
  system with no package manager still runs programs.

The consequence is honest and worth stating: **a `.lza` is not sandboxed and not
verified.** A package's permissions are a *declaration*, and a kernel that starts a
package has not enforced them, because the syscall layer has no capability gate today
(see the gap list). Step 91's expanded LazOS and step 96's integration work are where
that gets enforced; until then a package is a transport.

## What the format does *not* do, and why

- **No signatures, no hashes, no certificates.** Step 56 says no signing; nothing in
  the repository has a trust anchor to verify against, and a hash nobody checks is
  worse than none because it looks like a check.
- **No dependency table in the container.** Step 56's `[dependencies]` is a *source*
  concern and step 89 owns resolving it. A package carrying a resolved dependency
  list would duplicate what the linker already decided.
- **No multi-architecture payload.** `architecture = "any"` produces one `.lzx` for
  one word size. A universal package needs a different container and a different
  resolution rule, and inventing one now would fix a choice the repository has not
  needed to make.
- **No compression.** Resources are raw bytes, per step 56. A compression scheme is
  a second format with its own version and its own failure modes inside the first.
- **No install location in the package.** Where a package lands is the *system's*
  decision; a file that says where it goes is a file that has to be rewritten on
  every move.

## The gaps this design deliberately does not close

Each of these is a real hole, found in the code, and each names where it is closed.

| Gap | Evidence | Closed by |
| --- | --- | --- |
| Nothing resolves a path to an executable. | `SpawnProcess` validates a path; `start_image` takes an image. | Step 89, with the resolution rule above |
| Permissions are declared and never enforced. | step 56 says "checked against the source"; no syscall gate exists. | Step 91 |
| Resources have names in the package and no runtime lookup. | the executable holds only anonymous constants. | Step 91 or 96, whichever brings the resource table |
| No `pack` or `install` command. | `crates/lazalith-cli/src/main.rs:191` | Step 89 |
| A package is 1 MiB-limited by the VFS, and the `.lzx` limit is 4 MiB. | `filesystem.rs:11`, `lzx.rs:17` | Unresolved — see below |
| No version *compatibility* rule, only a version *identity*. | step 56 fixes the version format; nothing says what a 0.2 may assume about a 0.1's files. | Unresolved — see below |

The last two are genuinely open, and they are open because the repository contains
no answer, not because the answer is hard:

- **Size.** The `.lzx` may be 4 MiB and the VFS file may be 1 MiB, so a large
  application cannot be installed as a single VFS file today. Either the VFS limit
  rises, or a package becomes a directory, or the VFS grows a streaming install. All
  three are reasonable; picking one now would fix a decision whose cost depends on
  what real applications turn out to weigh, and that is exactly what this step was
  told not to guess.
- **Compatibility.** `docs/lazen-applications.md` fixes `major.minor.patch` and says
  the version is not the ISA, ABI, or object version — all of which the `.lzx`
  carries. Whether `minor` may change resource names or only bug-fix behaviour is a
  promise to *users of an application*, and no such promise has been made to anyone
  yet because no application has been published.

## Compatibility requirements on what already exists

The container is **versioned separately from everything it contains**, and each
version is checked where it belongs:

| Checked | By | Failure |
| --- | --- | --- |
| container version | the package reader | the build is older than the file |
| `.lzx` format, ISA, ABI versions | `LzxImage::from_bytes` | the load is older than the file |
| manifest fields | the package reader | a malformed manifest is not a package |

A tool that can read version *n* of the container must refuse version *n+1* rather
than guess. That is the `.lzo` reader's rule, and it is the same rule: an unknown
version is an error, never a best effort.

## Proposed interfaces, for the step that implements it

Not implemented here. These are the shapes step 89 should build against, so that the
container and the package manager do not get designed independently.

```rust
// where: crates/lazalith-os/src/lza.rs (or a new crate, if it grows)
pub struct LzaPackage { /* header fields, manifest bytes, image, resource table */ }
impl LzaPackage {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LzaError>;
    pub fn to_bytes(&self) -> Result<Vec<u8>, LzaError>;
    pub const fn name(&self) -> &[u8];
    pub const fn version(&self) -> PackageVersion;
    pub const fn manifest(&self) -> &[u8];
    pub fn image(&self) -> Result<LzxImage, LzaError>;   // parses the contained .lzx
    pub fn resources(&self) -> &[LzaResource];
    pub fn resolve(&self) -> PackageIdentity;            // name + version + arch + permissions
}

// crates/lazalith-toolchain or the CLI, for `lazen pack`
pub fn pack(manifest: &Manifest, image: &LzxImage, resources: &[(String, u64)]) -> Result<Vec<u8>, LzaError>;
```

`image()` returning a `Result` rather than an `LzxImage` is the point of the whole
document: the package is a container, and the executable inside it is still parsed by
the executable's own reader, so a package can never mean something the `.lzx` does
not.

## Validation this design owes its reader

A design step that adds no code still owes something checkable. What is checkable
*now*, without fixing the format:

- Every requirement in the table above cites code, and the citations were read
  against the tree this document was written in. A requirement that cannot be cited
  is an assumption, and the ones that are assumptions are in the gaps list.
- The three rules about canonical offsets, checked-before-allocate, and
  content-based resolution are each *implementable as a property*, and step 89 should
  write them as such: an object that round-trips is idempotent (step 86's
  idempotence property, reused verbatim), a count is refused when it exceeds the
  bytes present, and a resolution never depends on a file's name.
- The size limit is a real, currently-contradictory pair of constants
  (`LZX_MAX_FILE_SIZE` 4 MiB against `DEFAULT_MAX_FILE_BYTES` 1 MiB). It is left
  visible in the gaps table rather than quietly reconciled, because reconciling it
  is a decision about real applications and this step has none.
