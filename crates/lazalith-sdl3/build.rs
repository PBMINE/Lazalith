//! Finds SDL3, checks the event layout, and prints the link flags.
//!
//! SDL3 is a C library with no Rust bindings in this workspace, so the build has
//! to ask the C toolchain where it is. `pkg-config` is the mechanism the rest of
//! the Nix build already uses, and asking it rather than hard-coding a path is
//! what lets the same source build against whatever SDL3 the flake provides.
//!
//! # Why this script compiles a C program
//!
//! `src/lib.rs` declares `SDL_Event` as a union whose largest member is a padding
//! array, on the claim that a buffer of `EVENT_BUFFER_SIZE` bytes cannot be
//! overrun by an event SDL writes. It also mirrors `SDL_KeyboardEvent` field by
//! field to read the key out of it. Both of those are claims about a C layout, and
//! a claim about a C layout that nobody checks is a comment.
//!
//! So this script asks the C compiler. It compiles a probe that fails to build if
//! `SDL_Event` is larger than the buffer, prints the sizes and offsets the Rust
//! side needs, and `src/lib.rs` asserts its own layout against those numbers. An
//! SDL3 that changed either would fail the build here with a message saying so,
//! rather than producing a frontend that reads a key out of the wrong bytes.

use std::{env, path::PathBuf, process::Command};

/// The probe compiled against SDL3's headers.
///
/// `EVIDENCE` is printed on one line, space separated, and read back by
/// `src/lib.rs`. The static assertion is the important half: it makes the build
/// fail here if SDL3 ever grows an event past the buffer this crate provides.
const PROBE: &str = r#"
#include <stdio.h>
#include <stddef.h>
#include <SDL3/SDL.h>

int main(void) {
    /* The buffer must be at least as large as any event SDL can write. A C
     * compile error here is the whole point: it names the change that broke the
     * assumption, in the file that made it. */
    char buffer[LAZALITH_EVENT_BUFFER_SIZE];
    (void) buffer;
    typedef char lazalith_event_fits[
        sizeof(SDL_Event) <= LAZALITH_EVENT_BUFFER_SIZE ? 1 : -1
    ];
    printf("event=%lu key_event=%lu key_offset=%lu key_field=%lu "
           "scancode=%lu repeat=%lu bool_size=%lu keycode=%lu keymod=%lu\n",
           (unsigned long) sizeof(SDL_Event),
           (unsigned long) sizeof(SDL_KeyboardEvent),
           (unsigned long) offsetof(SDL_Event, key),
           (unsigned long) offsetof(SDL_KeyboardEvent, key),
           (unsigned long) offsetof(SDL_KeyboardEvent, scancode),
           (unsigned long) offsetof(SDL_KeyboardEvent, repeat),
           (unsigned long) sizeof(bool),
           (unsigned long) sizeof(SDL_Keycode),
           (unsigned long) sizeof(SDL_Keymod));
    return 0;
}
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    for variable in ["PKG_CONFIG_PATH", "SDL3_NO_PKG_CONFIG", "CC"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    let library = pkg_config::Config::new()
        .probe("sdl3")
        .unwrap_or_else(|error| {
            panic!(
                "SDL3 was not found by pkg-config, so the frontend cannot be built.\n\
             {error}\n\
             The Nix dev shell provides it; outside Nix, install SDL3 development \
             files or point PKG_CONFIG_PATH at them."
            )
        });

    // The buffer size is duplicated here rather than imported, because the build
    // script runs before the library is compiled and cannot read its constants.
    // `src/lib.rs` asserts the two agree, so a change to one without the other is
    // a build failure rather than a silent mismatch.
    const EVENT_BUFFER_SIZE: usize = 256;
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let probe = out_dir.join("layout_probe.c");
    let binary = out_dir.join("layout_probe");
    std::fs::write(&probe, PROBE).expect("the probe is written");

    let compiler = env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let status = Command::new(&compiler)
        .arg(&probe)
        .arg("-o")
        .arg(&binary)
        .arg(format!("-DLAZALITH_EVENT_BUFFER_SIZE={EVENT_BUFFER_SIZE}"))
        .args(["-I", &library.include_paths[0].display().to_string()])
        .status()
        .unwrap_or_else(|error| panic!("the C compiler {compiler} could not be run: {error}"));
    assert!(
        status.success(),
        "the SDL3 layout probe did not compile with {compiler}.\n\
         A failure here is one of two things: the C compiler is missing, or \
         SDL3's event union is larger than {EVENT_BUFFER_SIZE} bytes and this \
         crate's event buffer would be overrun. The compiler's message is above."
    );

    let output = Command::new(&binary)
        .output()
        .unwrap_or_else(|error| panic!("the layout probe could not be run: {error}"));
    assert!(
        output.status.success(),
        "the SDL3 layout probe ran but failed"
    );
    let evidence = String::from_utf8_lossy(&output.stdout);
    let mut generated = String::from(
        "// Generated by build.rs from SDL3's own headers. Do not edit.\n\
         //\n\
         // Every number here was measured by compiling a C program against the\n\
         // headers in use, so `src/lib.rs` can assert its layout against the real\n\
         // thing rather than against a comment about it.\n",
    );
    for pair in evidence.split_whitespace() {
        let Some((name, value)) = pair.split_once('=') else {
            panic!("the SDL3 layout probe printed {pair:?}, which is not name=value");
        };
        // A non-numeric value would produce a Rust file that does not compile,
        // which is the right outcome: a layout this crate cannot check is a layout
        // it must not depend on.
        //
        // The name is upper-cased because it becomes a Rust constant, and the
        // generated file carries an `allow` for the doc comment a public constant
        // in an included file would otherwise need written twice.
        generated.push_str(&format!(
            "#[allow(non_upper_case_globals, missing_docs)]\n\
             pub const {name}: usize = {value};\n"
        ));
    }
    std::fs::write(out_dir.join("layout.rs"), &generated).expect("the measured layout is written");
    println!("cargo:rerun-if-changed={}", probe.display());
}
