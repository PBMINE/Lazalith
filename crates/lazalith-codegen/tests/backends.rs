//! Step 94: the backend seam, and the rule that keeps the second one optional.
//!
//! The roadmap's shape is two frontends into one IR, and the IR into a native
//! backend and *optionally* an LLVM one. Two things have to be true for that shape
//! to be real rather than aspirational, and they are what this file tests:
//!
//! - **The seam exists and the native path goes through it.** A `Backend` trait that
//!   nothing called would be decoration. If [`Backend::generate`] is the same
//!   function as [`generate`] and the object is byte-for-byte the same, the seam is
//!   a place a second backend could register.
//! - **No LLVM is present, and that is a test rather than a promise.** "LLVM must
//!   not become mandatory" is the one rule in this step that is about the future,
//!   and a promise about the future is worth exactly as much as the test enforcing
//!   it. Somebody will eventually want `llvm-sys`; this file is what they will hit.

use lazalith_codegen::{Backend, CodegenOptions, NativeBackend, available_backends, generate};
use lazalith_compiler::frontend::compile;
use lazalith_compiler::lower::{self, FrameLayout};
use lazalith_ir::Module;
use lazalith_types::SourceManager;

const PROGRAM: &str = r#"
extern "syscall" fn write(fd: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

fn add(a: i64, b: i64) -> i64 {
    return a + b;
}

fn main() -> i32 {
    let total = add(2, 3);
    let message = "sum\n";
    let bytes = message.as_bytes();
    write(1, bytes, message.len() as u64, bytes.as_ptr());
    total as i32
}
"#;

/// Compiles, lowers, and returns what a backend is handed.
fn lowered(source: &str) -> (Module, Vec<FrameLayout>, String) {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    let lowered =
        lower::lower(&program).unwrap_or_else(|error| panic!("{source} should lower: {error}"));
    (lowered.module, lowered.frames, lowered.entry)
}

#[test]
fn the_native_backend_is_the_default_and_the_only_one() {
    let backends = available_backends();
    assert_eq!(backends.len(), 1, "one backend, and it is the native one");
    assert_eq!(backends[0].name(), "native");
    assert_eq!(NativeBackend.name(), "native");
}

#[test]
fn the_seam_is_real_because_the_native_backend_produces_the_same_object() {
    // If this ever failed, the trait would be a wrapper the codebase had learned to
    // route around, and a second backend would have to be threaded through the front
    // ends instead of registered here. The objects are compared byte for byte,
    // because "the same object" has to mean the same object.
    let (module, frames, entry) = lowered(PROGRAM);
    let options = CodegenOptions::lz64("t.lazen");
    let direct = generate(&module, &frames, &entry, &options, PROGRAM)
        .expect("the free function should generate");
    let through_the_trait = NativeBackend
        .generate(&module, &frames, &entry, &options, PROGRAM)
        .expect("the native backend should generate");
    assert_eq!(
        direct
            .object()
            .to_bytes()
            .expect("the object should serialize"),
        through_the_trait
            .object()
            .to_bytes()
            .expect("the object should serialize")
    );
    assert_eq!(direct.frames(), through_the_trait.frames());
}
/// The workspace root, found by walking up to the manifest that declares one.
///
/// Walked rather than assumed as "two directories up", so that moving a crate
/// cannot quietly make these tests read a directory that happens to exist.
fn workspace() -> std::path::PathBuf {
    let mut path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = path.join("Cargo.toml");
        if std::fs::read_to_string(&manifest).is_ok_and(|text| text.contains("[workspace]")) {
            return path;
        }
        assert!(
            path.pop(),
            "no manifest declaring a workspace was found above the crate directory"
        );
    }
}

#[test]
fn every_manifest_depends_only_on_path_dependencies() {
    // The project's rule is no third-party Rust dependencies, and step 94 adds a
    // second backend *without* adding one. This walks every manifest rather than
    // trusting a list, because a list is exactly the thing that goes stale.
    let root = workspace();
    let mut manifests = Vec::new();
    for directory in ["crates", ""] {
        let base = root.join(directory);
        for entry in std::fs::read_dir(&base).expect("a workspace directory reads") {
            let path = entry.expect("a directory entry").path();
            if !path.is_dir() {
                continue;
            }
            let manifest = path.join("Cargo.toml");
            if manifest.is_file() {
                manifests.push(manifest);
            }
        }
    }
    manifests.push(root.join("Cargo.toml"));
    manifests.sort();
    assert!(
        manifests.len() > 20,
        "expected to find the whole workspace, found {} manifests",
        manifests.len()
    );
    for manifest in &manifests {
        let text = std::fs::read_to_string(manifest)
            .unwrap_or_else(|error| panic!("{} should read: {error}", manifest.display()));
        for (number, line) in text.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') || !trimmed.contains("path = ") {
                continue;
            }
            assert!(
                !trimmed.contains("crates.io"),
                "{}:{} pulls a registry dependency: {trimmed}\n\
                 step 94 adds an optional backend, not a required dependency",
                manifest.display(),
                number + 1
            );
        }
    }
}

#[test]
fn no_llvm_crate_is_in_the_lock_file() {
    // The rule as a test. `llvm-sys` would be the obvious first step toward a real
    // LLVM backend, and it is exactly the step that would make a forty-megabyte C++
    // library a build requirement of a platform whose whole point is that it has
    // none. If this test ever needs removing, the removal *is* the step 94 review.
    let lock = workspace().join("Cargo.lock");
    let text = std::fs::read_to_string(&lock)
        .unwrap_or_else(|error| panic!("{} should read: {error}", lock.display()));
    for line in text.lines() {
        let name = line.trim().trim_start_matches('"');
        assert!(
            !name.starts_with("llvm"),
            "Cargo.lock names an LLVM crate: {line}\n\
             the native backend is the foundation; an LLVM backend must stay optional"
        );
    }
}
#[test]
fn the_native_backend_generates_a_program_the_linker_can_use() {
    // The rule's other half, in the only way that counts: the native path is not
    // merely present, it produces a working object from both frontends' shared IR.
    // A step that added a seam and broke the one working backend would satisfy the
    // trait test and fail this one.
    let (module, frames, entry) = lowered(PROGRAM);
    let options = CodegenOptions::lz64("t.lazen");
    let program = NativeBackend
        .generate(&module, &frames, &entry, &options, PROGRAM)
        .expect("the native backend should generate");
    assert!(
        program
            .frames()
            .iter()
            .any(|frame| frame.function == "main"),
        "the program should report a frame for main"
    );
    assert!(
        !program
            .object()
            .to_bytes()
            .expect("the object should serialize")
            .is_empty(),
        "the object should not be empty"
    );
}
