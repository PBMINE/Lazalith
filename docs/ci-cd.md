# Lazalith CI/CD

This document is the record of the GitHub automation in this repository: what
exists, what each job runs, what it does not check, and which parts of GitHub's
behaviour live outside the repository and therefore cannot be verified from here.

`binstruction.md` §38 makes CI/CD a first-class part of the engineering
architecture rather than a set of convenience scripts, and §38.12 asks for exactly
this document.

Throughout, the distinction §56 requires is kept: **what exists** is separated
from **what is planned**, and a workflow that has never run on GitHub is not
described as if it had.

---

## 1. Inventory

Everything lives in `.github/`.

| File | Kind | Status |
| --- | --- | --- |
| `.github/workflows/ci.yml` | workflow | written and locally validated; **not yet executed on GitHub** |
| `.github/workflows/campaign.yml` | workflow | written and locally validated; **not yet executed on GitHub** |
| `.github/workflows/release.yml` | workflow | written and locally validated; **no tag has ever been cut, so no release has been produced** |
| `.github/dependabot.yml` | configuration | written; runs only once Dependabot is enabled for the repository |
| `.github/CODEOWNERS` | governance | written |
| `.github/PULL_REQUEST_TEMPLATE.md` | governance | written |
| `.github/ISSUE_TEMPLATE/bug_report.yml` | governance | written |
| `.github/ISSUE_TEMPLATE/limitation.yml` | governance | written |
| `.github/ISSUE_TEMPLATE/beyond_lazalith.yml` | governance | written |
| `.github/ISSUE_TEMPLATE/config.yml` | governance | written; disables blank issues |

**There was no `.github/` directory at all before this pass.** Nothing in the
repository validated itself on a hosted runner, and the Phase-I freeze record in
`docs/project-state.md` was produced by hand.

---

## 2. Trigger rules

| Workflow | Trigger | Why |
| --- | --- | --- |
| `ci.yml` | `push` to `main`, `pull_request` | the merge path and the review path are the same checks |
| `campaign.yml` | `schedule` nightly at 04:17 UTC, plus `workflow_dispatch` | too expensive for a pull request; available on demand |
| `release.yml` | `push` of a tag matching `v*` | a release is an event, not a branch state |

`ci.yml` sets `concurrency` with `cancel-in-progress`, keyed on workflow and ref, so
a second push to a branch cancels the first. The cancelled run describes a commit
nobody is going to merge.

The nightly minute is `17`, not `00`. Every scheduled workflow on the platform
queues at the top of the hour.

---

## 3. Jobs and what they run

```
ci.yml
  fmt ─────────────┐
  clippy ──────────┼──▶ test ──┐
  architecture ────┘           ├──▶ nix ──▶ smoke
                                │
  (fmt, clippy, architecture run in parallel; test waits for all three)
```

### `fmt` — Tier 1

```bash
nix develop --command cargo fmt --all --check
```

### `clippy` — Tier 1

```bash
nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
```

`--all-targets` is what lints the tests and the examples, not only the libraries.

### `test` — Tier 2

```bash
nix develop --command cargo test --workspace --all-features
```

The whole suite. **Locally measured at 1295 tests, 2m53s wall, 10m19s user** on the
machine this was written on, in a debug build.

### `architecture` — Tier 2

```bash
nix develop --command cargo test -p lazalith-cli --test architecture -- --nocapture
nix develop --command cargo test -p lazalith-machine --test differential -- --nocapture
nix develop --command cargo test -p lazalith-boot -p lazalith-toolchain --all-features
```

Run as its own job so a failure reads as an architecture failure rather than as
"a test failed somewhere in 1295 of them". These are the suites ordinary
compilation cannot substitute for: the dependency graph, the ISA/ABI/syscall-table
uniqueness checks, the reference-interpreter-versus-machine differential, and the
object-format and linker suites.

### `nix` — Tier 3

```bash
nix flake check --print-build-logs
nix build --print-build-logs
```

### `smoke` — Tier 3

Runs the binary the package installs, on the programs the repository ships:

```bash
lazen --version
lazen check examples/hello/main.lz
lazen check examples/window/main.lz
lazen fmt  --check examples/hello/main.lz
lazen fmt  --check examples/window/main.lz
lazen run examples/hello/main.lz      # output must contain "Hello, Lazalith"
lazen run examples/window/main.lz
lazalith-fuzz --list
```

and asserts that `command -v lazen` is `$PWD/result/bin/lazen` — so the program
tested is the one this build produced, not one that happened to be on the path.

### `campaign.yml` — Tier 4

```bash
cargo run --release -p lazalith-fuzz -- --iterations 20000 --seed 1
cargo test --release --workspace --all-features -- properties
cargo test --release -p lazalith-machine --test differential -- --nocapture
```

The fuzzer is a seeded generator, not a coverage-guided one, so the seed is pinned
in the workflow and a failure is reproducible by a person with the printed seed
and no access to the original run. `--list` runs first, so the log always names
the targets the campaign covered.

### `release.yml` — Tier 5

Tag-driven. It refuses to proceed unless the tag looks like `vMAJOR.MINOR.PATCH` and
the tagged commit is an ancestor of `origin/main`. It builds with `nix build
.#default`, writes a `BUILDINFO` recording the version, the revision and the
pinned nixpkgs revision, produces `SHA256SUMS`, **verifies the artifacts before
publishing them** (`./dist/lazen --version`, `./dist/lazalith-fuzz --list`,
`sha256sum --check`), and then publishes with `gh release create`.

`gh` is preinstalled on GitHub-hosted runners, so no third-party release action is
used.

---

## 4. Why Nix, in every job

`lazalith-sdl3` compiles a C probe against real SDL3 headers at build time and
asserts the measured layout against the Rust side at compile time. A CI job that
installed Rust by hand and no SDL3 would either fail at the pkg-config lookup or,
worse, be green because it never built the frontend.

Using the project's own flake means CI runs the same environment `nix develop`
gives a developer, which is the only way "works locally" and "works in CI" can mean
the same thing. `binstruction.md` §38.1 asks for exactly this.

---

## 5. Cache strategy

| What | How |
| --- | --- |
| nixpkgs and the dev shell | the upstream binary cache, configured in the project's own Nix configuration. No cache action is used. |
| `target/` | `actions/cache`, keyed on `hashFiles('Cargo.lock', 'flake.lock')` with a `cargo-target-` restore prefix |

**An honest note on the `target/` cache.** The workspace has exactly one
third-party Rust dependency (`pkg-config`, a build dependency of `lazalith-sdl3`).
There are no crates.io dependencies to download. So the cache exists purely to avoid
recompiling 25 local crates, and it saves a few minutes, not tens of them. It is not
a load-bearing optimisation and nothing fails without it.

---

## 6. Artifact strategy

`ci.yml`'s `smoke` job uploads `result/bin/lazen` and `result/bin/lazalith-fuzz` as
the `lazalith-binaries` artifact, with `if-no-files-found: error` so a build that
produced no binaries fails rather than uploading nothing. Retention is 14 days.

**Not uploaded, deliberately:** `.lzo` objects, `.lzx` executables, VM images and
boot artifacts. `binstruction.md` §38.6 anticipates them, and they are not
produced by a check today. Producing them "for the artifacts" would mean building
something nobody has asked to build, and §38.6's own rule applies: do not commit
generated build output merely because CI produces it.

---

## 7. Release strategy

**Status: designed, validated locally, never executed.** No tag matching `v*` has
been pushed, so **no GitHub Release exists** and nothing in this repository should
be read as claiming one.

The design, for when a tag is cut:

- the artifact is derived from an exact commit, checked out by the workflow itself,
  so the artifact and the source revision cannot disagree;
- `BUILDINFO` records the version, the full revision, the pinned nixpkgs revision,
  the runner OS and the build profile;
- `SHA256SUMS` sits next to the binaries, and the release notes tell a downloader
  to run `sha256sum --check SHA256SUMS`;
- artifacts are verified **before** they are published, because the only moment a
  broken release can be stopped is before it is named;
- tags are never moved by the workflow and are never force-pushed. A wrong release
  is fixed by deleting the tag and cutting a new one from a corrected commit.

---

## 8. Security and permissions

| Workflow | `GITHUB_TOKEN` permissions |
| --- | --- |
| `ci.yml` | `contents: read` at the top level; every job inherits |
| `campaign.yml` | `contents: read` |
| `release.yml` | `contents: read` at the top level, `contents: write` on the release job only |

Per GitHub's documented rule, **any permission not named is `none`** — so the
release workflow's jobs other than `release`, and every job in the other two
workflows, cannot write to the repository, create issues, publish packages or push
anything.

Every action is pinned to a **full commit SHA** with the version in a trailing
comment:

| Action | Pinned to | Version |
| --- | --- | --- |
| `actions/checkout` | `3d3c42e5aac5ba805825da76410c181273ba90b1` | v7.0.1 |
| `cachix/install-nix-action` | `13d8dd58da0234aa297dedd986986ccb8e7f3e24` | v31.11.1 |
| `actions/cache` | `55cc8345863c7cc4c66a329aec7e433d2d1c52a9` | v6.1.0 |
| `actions/upload-artifact` | `043fb46d1a93c77aae656e7c1c64a875d1fc6a0a` | v7.0.1 |

These SHAs were resolved from each project's release tag at the time of writing.
Dependabot is configured to update them and opens a reviewable diff when it does.

**No secret scanning or CodeQL is enabled.** `binstruction.md` §38.8 says to
investigate appropriate use and to enable only what fits and can be verified.
This repository has one build-time dependency and no runtime third-party code, so
the useful surfaces today are the workflows themselves — which are pinned to SHAs
and carry minimal permissions — and a code scanner would have nothing to scan
that the pin does not already cover. If that changes, this is where the decision
should be revisited.

---

## 9. Repository settings that live OUTSIDE the repository

`binstruction.md` §38.9 is explicit that a file is not a setting. These cannot be
verified from a checkout and are **not claimed to be active**:

| Setting | Where | Status |
| --- | --- | --- |
| protected `main` branch | repository settings | **unknown** |
| required status checks | branch protection rules | **unknown** |
| required reviews | branch protection rules | **unknown** |
| Dependabot alerts / security updates | repository settings | **unknown** |
| Actions permissions (default token) | repository/org settings | **unknown** |

`.github/dependabot.yml` and `.github/CODEOWNERS` are *configuration* that only take
effect once the corresponding features are switched on. Writing the file is not the
same as enabling the feature, and this document does not conflate them.

The right status checks to require, once protection is configured, are exactly the
`ci.yml` job names: `formatting`, `lints`, `workspace tests`, `architecture
invariants`, `nix flake check`, `installed program`.

---

## 10. Dependabot

| Ecosystem | Interval | What it covers |
| --- | --- | --- |
| `cargo` | weekly, Monday 06:00 UTC | `Cargo.lock` — currently one package, `pkg-config` |
| `github-actions` | weekly, Monday 06:00 UTC | the pinned action SHAs |

**`nix` is absent, and that is a real gap.** Dependabot has no Nix ecosystem, so
`flake.lock` — which pins the nixpkgs revision that the reproducibility guarantee
rests on — is updated by hand. Today:

```bash
nix flake update              # bump the lock file
nix flake check               # prove the new revision still builds and passes
git commit -am 'nix: update flake.lock'
```

This is a recorded limitation, not a solved problem. An unautomated update path for
the one file CI's reproducibility depends on is a real gap and is the first thing
to fix if the repository grows a second maintainer.

---

## 11. Local equivalents

Every command CI runs can be run by hand, and the two are the same commands:

```bash
nix develop
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test -p lazalith-cli --test architecture
cargo test -p lazalith-machine --test differential
nix flake check
nix build
./result/bin/lazen run examples/hello/main.lz
cargo run -p lazalith-fuzz -- --iterations 200000 --seed 7
```

`nix build` leaves a `result` symlink to the store path, so
`./result/bin/lazen` is the same binary the package installs and the one the
`smoke` job tests. Nothing in this list is a CI-only command.

---

## 12. How these files were validated, and what was measured

Every claim in this document about "it passes" was checked on this machine before
the files were written, and a check that could not be run is not claimed.

| Validated | How | Result |
| --- | --- | --- |
| every workflow file is well-formed YAML | `yq` over each of the eight files | all parse |
| every workflow is valid GitHub Actions | `actionlint` (nixpkgs 1.7.12), which also runs `shellcheck` over every `run:` block | clean, exit 0 |
| `cargo fmt --all --check` | run | clean |
| `cargo check --workspace --all-targets` | run | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | run | clean, no warnings |
| `cargo test --workspace --all-features` | run | **1295 passed, 0 failed**, 2m53s wall / 10m19s user, debug build |
| `nix flake check --print-build-logs` | run against a **changed** tree | **all checks passed**, 2m42s |
| `nix build` | run | `result/bin/{lazen, lazalith-fuzz}` |
| the built binary, on the shipped examples | run | `Hello, Lazalith`; `examples/window` exits 0; both `check` and `fmt --check` pass |
| `lazalith-fuzz --list` from the built package | run | 33 lines of output, listing the targets |

### Two things the validation turned up that are worth knowing

**`actionlint` found a real defect.** `concurrency` was initially written *inside*
the `on:` block. GitHub parses that as an unknown webhook event and ignores the
whole key, so a superseded run would never have been cancelled and the setting
would have looked present in review. It is now a top-level key. This is the
argument for linting workflow files rather than trusting that they look right.

**`nix build` and `nix flake check` build the git *tree*, not the working
directory.** A new source file that has not been `git add`ed is invisible to
them, and the failure appears *inside the sandbox* as a confusing
`error[E0583]: file not found for module 'engine'` — pointing at a file that is
plainly right there. This was reproduced here: two new modules were added under
`crates/lazalith-cpu/src/`, and `nix build` failed on them while `cargo check` in
the same working directory passed.

It does not affect GitHub Actions — `actions/checkout` produces a full tree — but
it is a genuine local footgun and worth knowing about before concluding that Nix
is broken.

### The `nix flake check` measurement, and what it is not

`nix flake check` on a **warm** store completes in about 3.4 seconds and reports
`running 0 flake checks`. It proves nothing: every derivation is already realised,
so there is nothing to check.

Against a changed tree the same command ran every check and took **2m42s**, and
most of that was fetching from the binary cache rather than compiling — the `user`
time was 2.2 seconds against 2m42s of wall clock. So:

- 2m42s is a **real upper-ish bound for this machine with a warm binary cache**,
  which is roughly what a GitHub runner will have;
- a genuinely cold store — no `cache.nixos.org` hit for the nixpkgs closure — is
  **not measured**, and would be substantially longer;
- the tiering in §3 is ordered on the cargo measurements above, which *are* real,
  and puts `nix` last because it is the most expensive. If a cold run turns out
  to be prohibitive, the fix is to run it fully on `push` to `main` and on pull
  requests touching `flake.nix`, `flake.lock` or any `Cargo.toml` — a change to be
  made from a measurement, not from the guess made here.

### `nix flake check` and `aarch64-linux`

`nix flake check` on an x86 host says:

```text
warning: The check omitted these incompatible systems: aarch64-linux
Use '--all-systems' to check all.
```

`flake.nix` declares `x86_64-linux` and `aarch64-linux`, and only the former is
validated anywhere. The Phase-I freeze record in `docs/project-state.md` already
says "aarch64-linux remains untested"; nothing here changes that, and no workflow
changes it either.

---

## 13. Known limitations

1. **Nothing here has run on GitHub.** Every workflow is locally validated; none
   has executed. The first push to `main` with a pull request is the first real
   test of the runner assumptions, and in particular of whether
   `cachix/install-nix-action` is permitted in this repository's Actions policy.
2. **A genuinely cold `nix flake check` is not measured** — only a 2m42s run
   against a changed tree with a warm binary cache. See §12.
3. **`flake.lock` is not automated.** Dependabot has no Nix ecosystem.
4. **Only `x86_64-linux` is validated.** `flake.nix` declares `aarch64-linux` too,
   and `nix flake check` on an x86 host says so:
   `warning: The check omitted these incompatible systems: aarch64-linux`. The
   `Phase-I` freeze record in `docs/project-state.md` already says "aarch64-linux
   remains untested"; that is still true, and no workflow changes it.
5. **No required status checks, no protected branch, no security scanning.** All
   repository settings; see §9.
6. **The fuzz campaign is far smaller in CI than the documented manual one.** The
   workflow default is 20 000 inputs; `docs/fuzzing.md` suggests 200 000 by hand.
   That is a deliberate time budget, and it is recorded here rather than left to be
   discovered.
7. **`docs/fuzzing.md` is out of date**: it says "ten targets" and lists ten, while
   `lazalith_fuzz::TARGETS` has eleven. Not fixed in this pass; recorded here so
   the next session does not treat either number as authoritative.
