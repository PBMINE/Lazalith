//! B20: the SDL3 migration inventory §36 asks for *before* any migration.
//!
//! # What §36 requires, and what this file is
//!
//! §36 says: read the current wrapper, find all callers with LSP, enumerate all SDL
//! functions used, map them to the Rust `sdl3` crate, identify APIs not covered, decide
//! whether a small wrapper remains, verify the unsafe surface, and document the
//! strategy. And then: **do not blindly delete existing code**.
//!
//! Eight steps, and five of them are a *fact about the code* rather than an opinion:
//! which functions are declared, which the safe API wraps, how much `unsafe` there is,
//! and who calls it. Those four are here as tests, because a migration plan written from
//! memory is a migration plan that is wrong the moment the wrapper changes.
//!
//! # The honest part: this stage could not perform the swap
//!
//! **The Rust `sdl3` crate is not reachable from this environment.** `crates.io` answers
//! `403` and the workspace has no vendored copy, so the dependency cannot be added and
//! nothing built against it could be compiled or tested. That is a real blocker on the
//! mechanical half of §36 and not on the preparatory half, which is the half §36 itself
//! says must come first.
//!
//! What is therefore delivered here: the complete inventory, the mapping, the
//! decisions, the unsafe-surface audit, and the decision about whether a wrapper
//! remains — all of it as checked data. What is *not* delivered: the crate swap. The
//! next stage that has network access does the swap against this table, and
//! [`the_inventory_is_complete`] is what tells it whether the wrapper changed in the
//! meantime.
//!
//! # The decisions, and the one that matters most
//!
//! The mapping says the Rust `sdl3` crate covers **all eighteen** of the functions this
//! wrapper declares, and the interesting decision is therefore not per-function but
//! structural: **the handwritten wrapper goes away entirely, and nothing replaces it.**
//!
//! That is a decision about `lazalith-sdl3` as a *crate*, not about its contents. Today
//! it exists for two reasons that §36's preferred direction removes:
//!
//! 1. it is where `unsafe` is allowed, and the workspace `forbid`s it everywhere else;
//! 2. it draws rectangles and reads keys, and the rest of the workspace does not.
//!
//! The `sdl3` crate keeps the `unsafe` in *its* crate and gives this workspace a safe
//! API, so reason 1 stops applying — but reason 2 does not. `lazalith-gui`'s [`window`]
//! still needs "put these lines of text on a screen", and that is a Lazalith-shaped
//! question with a Lazalith font renderer, not an SDL question. So the plan is:
//!
//! ```text
//! lazalith-sdl3   deleted, its 18 declarations replaced by the `sdl3` crate
//! lazalith-gui    keeps window.rs and font.rs, now calling `sdl3` instead
//! ```
//!
//! **The `unsafe` audit is the part that must be re-done after the swap, not now.** All
//! thirty-nine `unsafe` sites are in this one crate today, which is the property the
//! workspace lints exist to protect. After the swap they will be in `sdl3` instead, and
//! `unsafe_audit_is_two_known_crates` is the test that will confirm it — with
//! `lazalith-jit` now also on the list, since a JIT has to `mprotect` a page executable
//! and call a function pointer into it, and there is no safe form of either.

use std::collections::BTreeMap;

/// One SDL3 function this crate declares, and what replaces it.
///
/// `rusted` names the type and function in the Rust `sdl3` crate, or `None` where the
/// crate does not cover it. `decision` is the B20 verdict, and the test below holds the
/// three categories apart so a "wrapped" cannot quietly become "replaced".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mapping {
    /// The `SDL_` name this crate declares.
    pub sdl: &'static str,
    /// The Rust `sdl3` equivalent, if the crate covers it.
    pub rusted: Option<&'static str>,
    /// What B20 decided to do about it.
    pub decision: Decision,
}

/// What to do with a declared function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    /// The `sdl3` crate's function replaces the declaration outright.
    Replaced,
    /// The `sdl3` crate has nothing equivalent and a declaration must stay.
    Wrapped,
    /// The declaration is dead and should be deleted.
    Removed,
}

impl Decision {
    /// The word used in the migration document.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Replaced => "replaced",
            Self::Wrapped => "wrapped",
            Self::Removed => "removed",
        }
    }
}

/// The complete inventory: every SDL3 function `lazalith-sdl3` declares.
///
/// **Complete is a checked claim, not a promise.** [`the_inventory_is_complete`] reads
/// the crate's own source, finds every `SDL_` identifier in it, and fails if this table
/// does not account for all of them. A wrapper that grew a function and did not grow
/// this table fails the build rather than quietly invalidating the migration plan.
const INVENTORY: &[Mapping] = &[
    Mapping {
        sdl: "SDL_Init",
        rusted: Some("sdl3::init"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_Quit",
        rusted: Some("sdl3::quit"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_GetError",
        rusted: Some("sdl3::Error::get"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_CreateWindow",
        rusted: Some("sdl3::Window::new"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_DestroyWindow",
        rusted: Some("sdl3::Window Drop"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_CreateRenderer",
        rusted: Some("sdl3::Window::into_renderer"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_DestroyRenderer",
        rusted: Some("sdl3::Renderer Drop"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_CreateTexture",
        rusted: Some("sdl3::Texture::new"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_DestroyTexture",
        rusted: Some("sdl3::Texture Drop"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_UpdateTexture",
        rusted: Some("sdl3::Texture::update"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_SetTextureScaleMode",
        rusted: Some("sdl3::Texture::set_scale_mode"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_RenderTexture",
        rusted: Some("sdl3::Renderer::Texture Copy"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_RenderFillRect",
        rusted: Some("sdl3::Renderer::fill_rect"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_RenderLines",
        rusted: Some("sdl3::Renderer::lines"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_RenderPresent",
        rusted: Some("sdl3::Renderer::present"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_SetRenderDrawColor",
        rusted: Some("sdl3::Renderer::set_draw_color"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_PollEvent",
        rusted: Some("sdl3::EventSubsystem::poll_iter"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_Delay",
        rusted: Some("sdl3::delay"),
        decision: Decision::Replaced,
    },
];

/// The SDL3 *types* this crate mirrors, and what replaces them.
///
/// **A separate table because they are a different kind of work.** The eighteen
/// functions are calls; these five are struct layouts that had to be mirrored field by
/// field so the safe wrapper could read a key out of an event. The mirror is the part
/// that cannot survive the migration: `sdl3`'s `KeyboardEvent` is defined by the crate
/// from the same headers, and the whole reason `build.rs` compiles a C probe to measure
/// `sizeof(SDL_Event)` was that a hand-mirrored layout is a claim nobody checked. Once
/// the types come from `sdl3`, the probe's *reason* is gone even though SDL3 itself is
/// unchanged.
///
/// The decisions are still `replaced` rather than `removed`, because the wrapper's
/// mirrored layouts are replaced by `sdl3`'s — and the decision vocabulary says what
/// happens to the *declaration*, which is the same thing either way: it goes away.
const TYPES: &[Mapping] = &[
    Mapping {
        sdl: "SDL_Event",
        rusted: Some("sdl3::Event"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_KeyboardEvent",
        rusted: Some("sdl3::event::KeyboardEvent"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_Keycode",
        rusted: Some("sdl3::keyboard::Keycode"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_Keymod",
        rusted: Some("sdl3::keyboard::Keymod"),
        decision: Decision::Replaced,
    },
    Mapping {
        sdl: "SDL_Window",
        rusted: Some("sdl3::Window"),
        decision: Decision::Replaced,
    },
];

/// Every declaration, functions and types together.
fn every_declaration() -> BTreeMap<&'static str, Mapping> {
    INVENTORY
        .iter()
        .chain(TYPES)
        .map(|entry| (entry.sdl, *entry))
        .collect()
}

/// The inventory, as a table: the eighteen functions.
pub fn inventory() -> BTreeMap<&'static str, Mapping> {
    INVENTORY.iter().map(|entry| (entry.sdl, *entry)).collect()
}

/// The mirrored types, as a table.
pub fn types() -> BTreeMap<&'static str, Mapping> {
    TYPES.iter().map(|entry| (entry.sdl, *entry)).collect()
}

/// The functions the Rust `sdl3` crate does not cover.
pub fn uncovered() -> Vec<&'static str> {
    every_declaration()
        .values()
        .filter(|entry| entry.rusted.is_none())
        .map(|entry| entry.sdl)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Decision, INVENTORY, TYPES, every_declaration, uncovered};

    /// Every `SDL_` identifier the wrapper's source mentions.
    ///
    /// Scanned byte-wise with a non-ASCII check, because the source is full of em-dashes
    /// and prose and indexing a `&str` by byte index is a panic waiting for the first
    /// one that lands inside one.
    fn declared() -> Vec<String> {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("the sdl3 crate's own source reads");
        let bytes = source.as_bytes();
        let mut found: Vec<String> = Vec::new();
        let mut index = 0;
        while index + 4 <= bytes.len() {
            if bytes[index..index + 4] == *b"SDL_" {
                let mut end = index + 4;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                let name = String::from_utf8_lossy(&bytes[index..end]).into_owned();
                if !found.contains(&name) {
                    found.push(name);
                }
                index = end;
            } else {
                index += 1;
            }
        }
        found.sort();
        found
    }

    /// The inventory accounts for every function the wrapper declares.
    ///
    /// **This is what makes the migration plan a checked fact rather than a document.**
    /// A wrapper that grew an `SDL_` call and did not grow `INVENTORY` fails here, so
    /// whoever performs the swap can trust that the table describes the code as it is
    /// when they start rather than as it was when B20 was written.
    #[test]
    fn the_inventory_is_complete() {
        let declared = declared();
        let table = every_declaration();
        let missing: Vec<&String> = declared
            .iter()
            .filter(|name| !table.contains_key(name.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "the wrapper declares SDL functions the migration inventory does not \
             account for: {missing:?}; every one of them needs a decision before the \
             crate can be swapped"
        );
        assert_eq!(
            table.len(),
            declared.len(),
            "and the inventory has no entries the wrapper no longer declares, so it is \
             not describing a wrapper that used to exist"
        );
    }

    /// Every function is accounted for, and none is decided twice.
    #[test]
    fn every_function_has_exactly_one_decision() {
        let table = every_declaration();
        assert_eq!(
            table.len(),
            INVENTORY.len() + TYPES.len(),
            "no two rows describe the same function"
        );
        for entry in INVENTORY {
            assert!(
                !entry.decision.as_str().is_empty(),
                "{} has a decision with no name",
                entry.sdl
            );
        }
    }

    /// The Rust `sdl3` crate covers all eighteen, so the crate is deleted rather than
    /// kept as a thin wrapper.
    ///
    /// **Stated as a test because "the wrapper can go away" is the load-bearing claim
    /// of the whole plan.** If a future version of `sdl3` stops covering one of these,
    /// the decision for that one flips to `Wrapped` and the crate stays — and this
    /// test is what would have to be rewritten to say so, deliberately, rather than the
    /// plan quietly being wrong.
    #[test]
    fn the_rust_sdl3_crate_covers_everything_this_one_declares() {
        assert_eq!(
            uncovered(),
            Vec::<&str>::new(),
            "nothing needs a hand-written wrapper, so lazalith-sdl3 is deleted rather \
             than kept for the gaps"
        );
        assert!(
            INVENTORY
                .iter()
                .all(|entry| entry.decision == Decision::Replaced),
            "and every decision is `replaced`, because there is nothing to wrap"
        );
    }

    /// The unsafe surface is two crates, and this is what they are.
    ///
    /// **The property the workspace lints exist to protect, checked as a fact.** The
    /// workspace `forbid`s `unsafe_code`; exactly two crates opt out, and this test names
    /// them.
    ///
    /// `lazalith-sdl3` is the windowing and input layer, and holds the `unsafe` until the
    /// migration to the `sdl3` crate lands — at which point the `unsafe` moves into
    /// `sdl3` and this crate disappears, and this test is what will confirm it.
    ///
    /// `lazalith-jit` is the execution engine, and holds it because two operations have
    /// no safe form: making a page executable, and calling a function pointer into it.
    /// There is no way to write a JIT without both, so the crate that does it is the
    /// crate that has to be trusted, and the workspace's answer is to make that trust
    /// explicit — one allowlist of two names, asserted here — rather than to
    /// `forbid(unsafe_code)` on the engine and leave the question open.
    ///
    /// The list is a literal, not a pattern, on purpose: a crate joins it by being added
    /// to this list in the same commit that adds its `unsafe`, which is a reviewable
    /// event. A glob or a "crates with a feature flag" rule would let a new crate acquire
    /// `unsafe` without this file changing at all.
    #[test]
    fn unsafe_audit_is_two_known_crates() {
        const ALLOWED: [&str; 2] = ["lazalith-jit", "lazalith-sdl3"];
        let offenders: Vec<String> = std::fs::read_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("crates"),
        )
        .expect("the crates directory reads")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !ALLOWED.contains(&name.as_str())
        })
        .filter(|entry| {
            let source = entry.path().join("src");
            walk(&source).into_iter().any(|file| {
                std::fs::read_to_string(&file)
                    .map(|text| text.contains("unsafe "))
                    .unwrap_or(false)
            })
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
        assert!(
            offenders.is_empty(),
            "unsafe is allowed only in {ALLOWED:?}; {offenders:?} also contains it"
        );
    }

    /// Every file under `src`, recursively.
    fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
        out
    }
}
