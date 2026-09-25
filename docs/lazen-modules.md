# Lazen Modules and Packages

This document is Step 55 of the roadmap. It defines imports, visibility,
modules, packages, and dependencies.

## Modules

A module is a namespace. There are two ways to declare one.

**File module.** The file's own top-level items form the crate's root module.
Its name is the crate name.

**Inline module.**

```lazen
mod geometry {
    pub struct Size {
        pub width: u32,
        pub height: u32,
    }

    pub fn area(size: Size) -> u32 {
        size.width * size.height
    }
}
```

Modules nest. A path is written with `::` and the root is `crate`:

```lazen
crate::geometry::Size
geometry::Size            // after `use crate::geometry;`
```

## Visibility

Every item is private by default. `pub` makes an item visible to code outside
its declaring module. `pub` on a field makes that field visible.

```lazen
mod app {
    pub struct Config {
        pub name: str,     // visible everywhere
        secret: str,        // visible only inside app
    }
}
```

Visibility is checked, not advisory. Referring to a private item from another
module is a compile error naming the private item and its module. There is no
`pub(crate)` or `pub(super)` in v1: two visibilities are enough for a language
this small, and fewer rules are easier to check exhaustively.

## Imports

```lazen
use crate::geometry;
use crate::geometry::Size;
use crate::fs as filesystem;
```

`use` brings a name into the current module's scope for the rest of the file.
`as` renames it. Importing a name that is already bound in the same scope is an
error, so an import can never silently shadow or be silently shadowed.

A `use` path must resolve to a module, an item, or a record/enum field path. It
may not refer to a local variable, and a path with more components than the
module tree actually contains is an error rather than an empty result.

## Crates and packages

A **crate** is one compilation unit: one `main.lz` or one library `lib.lz` plus
the files it pulls in. A **package** is a directory containing a manifest and one
or more crates.

```text
hello/
├── lazen.toml
└── src/
    ├── main.lz
    └── helpers.lz
```

The manifest is `lazen.toml`. Its format is defined in
`docs/lazen-applications.md`; the module system is independent of it.

A file declares its own module name by its path, relative to the crate root:

```text
src/main.lz          -> crate root
src/helpers.lz       -> crate::helpers
src/ui/button.lz     -> crate::ui::button
```

There is no implicit "everything is visible" rule between files. A file must
`use` what it needs, exactly like a module in the same file.

## Dependency rules

- A crate depends only on declared dependencies. There is no implicit
  path-based discovery, so a build cannot pick up a file that was not intended.
- A package may depend on another package by name and version requirement.
- The standard library is a dependency named `std` and is linked into every
  program, whether or not it is used.
- Cycles between modules or crates are rejected during name resolution, with the
  cycle reported as a path.

## Module-level items

Only these items may appear at module scope:

```text
fn          function
extern      OS ABI declaration
struct      record
enum        tagged union
mod         nested module
use         import
const       compile-time constant
```

Statements, local variables, and expressions never appear at module scope. A
`const` is a typed integer or boolean constant:

```lazen
const MAX_EVENTS: u32 = 32;
const CLEAR: u32 = 0x0000_0000;
```

Constants are inlined at their use sites. There is no constant folding
requirement beyond what the IR already does, and a constant may not be
addressable: taking the address of a `const` is an error, so a constant never
needs a runtime allocation.

## What is deliberately not in v1

- No re-exports (`pub use`), no glob imports (`use module::*`).
- No conditional compilation, build scripts, or code generation.
- No per-module attributes, doc comments, or feature flags.
- No module-level initialization order rules, because v1 has no module-level
  runtime state at all.

The reason is the same as everywhere else in Lazen v1: every rule that exists
must be checkable exhaustively by a compiler that a single person can reason
about. Module-level state, glob imports, and conditional compilation all
introduce ordering or resolution questions that v1 has no need to answer.
