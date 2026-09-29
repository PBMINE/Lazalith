# Pull request

<!--
Keep this short. The section that matters is "What did you check", because the
repository's own definition of done is a command list, not a reviewer's opinion.
-->

## What this changes

<!-- One or two sentences. What is different after this merges? -->

## Why

<!-- What was wrong, missing, or too slow. A pull request that fixes nothing in
     particular is fine, but say which of the two it is. -->

## How this was checked

<!-- The commands you actually ran, and their results. The repository validates
     with these, and `docs/ci-cd.md` says which CI job runs each one. -->

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
nix flake check
nix build
```

<!--
Not every change needs every command, but saying which ones ran is better than
letting CI be the first place they are tried. If a command is genuinely
inapplicable, saying so is better than leaving it out silently.
-->

## Contracts this touches

<!--
Tick what applies. These are the contracts `docs/architecture.md` and
`binstruction.md` freeze; a pull request that ticks one of them should be read
against that document.

- [ ] Guest-visible CPU, memory, or trap semantics
- [ ] The ISA definition table, or the encoder/decoder/disassembler that read it
- [ ] The LZA32/LZA64 width behaviour
- [ ] The OS ABI, the syscall table, or the object/executable formats (.lzo/.lzx)
- [ ] The device contracts, or the bus
- [ ] The toolchain boundary (a frontend, the assembler, or the linker)
- [ ] The VM core, an execution engine, or engine switching
- [ ] The host frontend / SDL3 boundary
- [ ] The build, the flake, or CI
- [ ] Documentation only
-->

## Notes for a reviewer

<!--
Anything that deserves to be read twice: an invariant you are relying on, a
limitation you are not fixing here, or a test that could pass while the thing it
names is broken.
-->
