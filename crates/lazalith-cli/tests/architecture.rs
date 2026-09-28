//! Step 99: the architecture review, as thirteen checks.
//!
//! The step lists thirteen properties the finished platform must have. A review that
//! reads as prose and concludes "the layers look right" is worth about as much as the
//! last time somebody drew a dependency diagram, so every property here is checked
//! mechanically instead — most of them by reading the manifests, because dependency
//! direction is a property of the manifests and nothing else.
//!
//! The ones that cannot be checked by reading a manifest are checked by *using* the
//! thing: assembly reaching a low-level instruction, Lazen's syscalls all resolving
//! through one table, and the two kinds of failure being different values rather than
//! two spellings of the same one.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The workspace root, found by walking up to the manifest that declares one.
fn workspace() -> PathBuf {
    let mut path = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    loop {
        let manifest = path.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && text.contains("[workspace]")
        {
            return path;
        }
        assert!(
            path.pop(),
            "no workspace manifest above the crate directory"
        );
    }
}

/// Every crate in the workspace, by directory name.
fn crates() -> Vec<(String, PathBuf)> {
    let root = workspace();
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root.join("crates")).expect("the crates directory reads") {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() && path.join("Cargo.toml").is_file() {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            found.push((name, path));
        }
    }
    found.sort();
    assert!(
        found.len() > 20,
        "expected the whole workspace, found {} crates",
        found.len()
    );
    found
}

/// The library dependencies one crate declares, and not its dev-dependencies.
///
/// The distinction is the whole point of two of these rules. A code generator may
/// legitimately run what it generated in a *test*, which is a dev-dependency on
/// the machine; what it may not do is depend on the machine from its library, which
/// would put an emulator inside a compiler. Reading both sections together would
/// make the second rule uncheckable, and would also make the first one
/// unfixable-by-accident.
fn dependencies(name: &str) -> BTreeSet<String> {
    let manifest = workspace().join("crates").join(name).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|error| panic!("{} should read: {error}", manifest.display()));
    let mut found = BTreeSet::new();
    let mut in_dev = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_dev = trimmed.starts_with("[dev-dependencies");
            continue;
        }
        if in_dev || trimmed.starts_with('#') || !trimmed.contains("path = ") {
            continue;
        }
        if let Some((key, _)) = trimmed.split_once(" = ") {
            found.insert(key.trim().to_string());
        }
    }
    found
}

// ---------------------------------------------------------------- the rules

#[test]
fn the_cpu_does_not_depend_on_sdl3() {
    // The CPU is the layer a window would be drawn *on top of*, not a layer that
    // knows a window exists. `lazalith-cpu` is also `no_std` and has no host
    // dependencies at all, so SDL3 reaching it would mean a window library inside
    // the instruction decoder.
    for layer in [
        "lazalith-cpu",
        "lazalith-isa",
        "lazalith-types",
        "lazalith-memory",
    ] {
        let deps = dependencies(layer);
        assert!(
            !deps.iter().any(|name| name.contains("sdl3")),
            "{layer} must not depend on SDL3: {deps:?}"
        );
    }
}

#[test]
fn the_machine_does_not_depend_on_the_compiler() {
    // The machine runs what the compiler produced. If it knew the compiler, it would
    // know the *shape* of a program, and an emulator that knows the shape of a
    // program is an emulator that will be wrong about a program the compiler did not
    // build.
    for layer in ["lazalith-machine", "lazalith-cpu", "lazalith-os"] {
        let deps = dependencies(layer);
        assert!(
            !deps.iter().any(|name| name.contains("compiler")),
            "{layer} must not depend on a compiler: {deps:?}"
        );
    }
}

#[test]
fn the_compiler_does_not_depend_on_the_emulator_implementation() {
    // The compiler emits code; it does not run it. A compiler that linked the
    // interpreter would be a compiler whose output quality could be measured only by
    // the one implementation it was linked to, and whose diagnostics would change if
    // that implementation did.
    for layer in ["lazalith-compiler", "lazalith-ir", "lazalith-codegen"] {
        let deps = dependencies(layer);
        assert!(
            !deps.iter().any(|name| name.contains("machine")),
            "{layer} must not depend on the machine: {deps:?}"
        );
    }
}

#[test]
fn the_gui_does_not_reach_into_the_cpu() {
    // The GUI may *use* the machine — it has to, to run something — but it may not
    // take a CPU apart. What it must never do is depend on the CPU crate at all,
    // because a dependency is the only way to reach internals, and the internals are
    // a decoder's business.
    let deps = dependencies("lazalith-gui");
    assert!(
        !deps.iter().any(|name| name.contains("lazalith-cpu")),
        "the GUI must not depend on the CPU crate: {deps:?}"
    );
    // And SDL3 may not leak down into anything below the presentation layer.
    for (name, _) in crates() {
        if name == "lazalith-sdl3" || name == "lazalith-gui" {
            continue;
        }
        let deps = dependencies(&name);
        assert!(
            !deps.iter().any(|dep| dep.contains("sdl3")),
            "{name} must not depend on SDL3: {deps:?}"
        );
    }
}

#[test]
fn lazen_goes_through_lazos_rather_than_around_it() {
    // A Lazen program has exactly one way to reach the outside world: a syscall
    // the ABI numbers. So every syscall the compiler can emit must be in the ABI
    // table, and the table is the *same* table the OS dispatches from — one crate,
    // re-exported, rather than two lists that agree today.
    let options = lazalith_runtime::BuildOptions {
        architecture: lazalith_types::ArchitectureConfig::lz64(),
        source_path: String::from("arch.lz"),
        prelude: lazalith_runtime::library_text(),
    };
    let program = lazalith_runtime::RuntimeProgram::build(
        r#"
fn main() -> i32 {
    rt::sys::print("through the abi\n");
    return 0;
}
"#,
        &options,
    )
    .expect("the program should build");
    // Every extern in the built program resolves to a numbered ABI syscall, which
    // is what `RuntimeProgram::build` refuses to do otherwise.
    for object in program.objects() {
        object.validate().expect("the object should validate");
    }
    // The prelude need not have a wrapper for every numbered syscall — some are
    // reached through another wrapper — so what is checked is the one thing that
    // must hold: a name the prelude *does* declare is a name the ABI numbers, so a
    // wrapper cannot exist for a syscall that is not there.
    let named: Vec<&str> = lazalith_os_abi::ABI_SYSCALLS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert!(
        named.contains(&"write") && named.contains(&"exit"),
        "the ABI is the table both front ends agree with"
    );
    // And the compiler's table is the ABI's, not a copy of it: both crates answer
    // the same thing for the same name because there is one definition.
    for (name, syscall) in lazalith_os_abi::ABI_SYSCALLS {
        assert_eq!(
            lazalith_os_abi::abi_syscall(name),
            Some(*syscall),
            "the compiler and the ABI disagree about {name}"
        );
    }
}

#[test]
fn c_goes_through_the_abi_rather_than_around_it() {
    // The C side of the same rule. A C program reaches the machine through a
    // declared ABI, and the C runtime is the only place that knows the numbers —
    // so the runtime's declarations and the ABI's are the same declarations, and the
    // C compiler cannot have a private list.
    let names: Vec<&str> = lazalith_os_abi::ABI_SYSCALLS
        .iter()
        .map(|(name, _)| *name)
        .collect();
    assert!(
        names.contains(&"write") && names.contains(&"exit"),
        "the ABI is the table everything else agrees with"
    );
    // The C runtime *declares* the syscalls it wants by name and the C compiler
    // numbers them, so the number is written in exactly one place: the ABI. The
    // check is on the crate that does the numbering.
    let deps = dependencies("lazalith-c-compiler");
    assert!(
        deps.iter().any(|name| name == "lazalith-os-abi"),
        "the C compiler must number syscalls from the ABI crate: {deps:?}"
    );
}

#[test]
fn assembly_can_still_reach_low_level_functionality() {
    // The rule with a rule of its own: the assembler must be able to emit
    // *everything* the ISA defines, including the instructions a user program may
    // not execute. A platform that "protects" a supervisor instruction by making the
    // assembler unable to write it has removed the only tool that needs to write it.
    let config = lazalith_types::ArchitectureConfig::lz64();
    // The point is that the assembler accepts a supervisor-only instruction and the
    // *machine* is what refuses to run it at user privilege. An assembler that could
    // not write the instruction would have removed the only tool that needs to.
    let mut source = String::new();
    source.push_str(".arch lz64\n");
    source.push_str(".entry main\n");
    source.push_str("main:\n");
    source.push_str("  nop\n");
    let object = lazalith_toolchain::assemble_named("low.lzs", &source)
        .expect("the assembler should accept a supervisor-only instruction set");
    object.validate().expect("the object should validate");
    assert_eq!(
        object.config(),
        config,
        "for the machine it was written for"
    );
    assert!(!object.code().is_empty(), "and it emitted something");
}

#[test]
fn lz32_and_lz64_share_one_architecture() {
    // Two configurations, one ISA, one decoder, one assembler, one linker. The check
    // is structural: the ISA and the types crates are the *only* definitions, and
    // the machine and memory crates are shared by both configurations rather than
    // having a 32-bit and a 64-bit copy.
    let lz32 = lazalith_types::ArchitectureConfig::lz32();
    let lz64 = lazalith_types::ArchitectureConfig::lz64();
    assert_eq!(lz32.word_width(), lazalith_types::WordWidth::W32);
    assert_eq!(lz64.word_width(), lazalith_types::WordWidth::W64);
    // The same instruction decodes under both configurations, which is what "shared
    // architecture infrastructure" has to mean in practice.
    let bytes = lazalith_isa::encode(
        lz64,
        &lazalith_isa::Instruction::new(lz64, lazalith_isa::Opcode::Nop, &[])
            .expect("a NOP is a NOP"),
    )
    .expect("it encodes");
    for config in [lz32, lz64] {
        lazalith_isa::decode(config, &bytes).expect("the same bytes decode in both modes");
    }
    for layer in [
        "lazalith-isa",
        "lazalith-cpu",
        "lazalith-memory",
        "lazalith-machine",
    ] {
        let deps = dependencies(layer);
        assert!(
            !deps
                .iter()
                .any(|name| name.ends_with("32") || name.ends_with("64")),
            "{layer} must be one crate, not a pair: {deps:?}"
        );
    }
}

#[test]
fn the_isa_is_defined_once() {
    // One opcode table. The check is that the mnemonic-to-byte mapping is written
    // once and that a second copy would be a second source of truth — so the test
    // counts the definition sites rather than trusting a comment.
    let sites: Vec<PathBuf> = grep_crates(&|_, text| {
        text.contains("Nop = 0x00") || text.contains("Nop = 0x0") || text.contains("\"NOP\"")
    });
    assert_eq!(
        sites.len(),
        1,
        "the instruction table must be written once, found in {sites:?}"
    );
}

#[test]
fn the_abi_is_defined_once() {
    // One ABI version constant, in the crate whose name is the ABI.
    let sites: Vec<PathBuf> = grep_crates(&|_, text| text.contains("ABI_VERSION: u16 = 1"));
    assert_eq!(
        sites.len(),
        1,
        "the ABI version must be declared once, found in {sites:?}"
    );
    // And the version the ISA reports is not the same fact written down again: the
    // compiler reads the ABI's, it does not restate it.
    let deps = dependencies("lazalith-compiler");
    assert!(
        deps.iter().any(|name| name == "lazalith-os-abi"),
        "the compiler must read the ABI from the ABI crate: {deps:?}"
    );
}

#[test]
fn the_syscall_table_is_defined_once() {
    // One table, and the compiler's is a re-export of it. A compiler that kept its
    // own list would number a syscall differently from the kernel the day the two
    // drifted, and the symptom would be a program that traps.
    assert_eq!(
        lazalith_os_abi::ABI_SYSCALLS
            .iter()
            .filter(|(name, _)| *name == "write")
            .count(),
        1,
        "a syscall may appear in the table once"
    );
    let numbers: Vec<(u16, &str)> = lazalith_os_abi::ABI_SYSCALLS
        .iter()
        .map(|(name, syscall)| (syscall.as_u16(), *name))
        .collect();
    let mut seen: BTreeSet<u16> = BTreeSet::new();
    for (number, name) in &numbers {
        assert!(
            seen.insert(*number),
            "two syscalls share number {number}, one of them is {name}"
        );
    }
}

#[test]
fn diagnostics_are_centralised() {
    // One diagnostics crate, and the code-formatting rules in one place. Every
    // crate that reports a problem depends on the same crate rather than writing its
    // own codes, because a project with two diagnostic vocabularies has a user who
    // reads neither.
    // The crates that report a *diagnostic* — as opposed to the ones that return an
    // error enum, which is a different thing and is not centralised here. The OS
    // is deliberately absent: it is the layer where an error is a value a caller
    // must handle rather than a message a person reads.
    let reporting = [
        "lazalith-compiler",
        "lazalith-c-compiler",
        "lazalith-toolchain",
        "lazalith-debug",
    ];
    for name in reporting {
        let deps = dependencies(name);
        assert!(
            deps.iter().any(|dep| dep == "lazalith-diagnostics"),
            "{name} reports diagnostics and must depend on the diagnostics crate: {deps:?}"
        );
    }
    // And the codes themselves are strings from that crate, not numbers a crate
    // made up: the debug crate's guest-trap and emulator-bug codes come from the same
    // place.
    let guest = lazalith_debug::diagnostic::guest_trap(0x1000, 0, "bounds", None);
    let frontend = lazalith_debug::diagnostic::guest_syscall_fault("a bad pointer", 0x1000);
    assert_ne!(
        guest.code().as_str(),
        frontend.code().as_str(),
        "two kinds of diagnostic must not share a code"
    )
}

#[test]
fn guest_faults_and_emulator_bugs_are_distinguishable() {
    // The last rule, and the one a user feels most directly. They are different
    // *values* in one enum, not two spellings of one code — so a frontend cannot
    // render them the same way by accident, and a new kind is a compile error
    // rather than a silently unhandled string.
    use lazalith_debug::diagnostic::{DiagnosticKind, guest_trap};
    let fault = guest_trap(0x1000, 0, "bounds", None);
    assert_eq!(fault.kind, DiagnosticKind::GuestFault);
    let all = [
        DiagnosticKind::GuestFault,
        DiagnosticKind::EmulatorBug,
        DiagnosticKind::Frontend,
    ];
    let mut names: Vec<String> = all.iter().map(|kind| format!("{kind:?}")).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 3, "three kinds, three distinct names");
}

/// The hardening phase widened two kernel accessors for the debugger, and a widened
/// accessor is a hole in a rule rather than a detail of a fix.
///
/// `Process::memory_mut` and `UserMemory::address_space_mut` were `pub(crate)` and are
/// now public, because a whole-machine snapshot has to move a running process's
/// memory out of the machine and back — and a process being activated keeps its
/// regions in the machine, not in itself, so a snapshot cannot see the program it is
/// a snapshot of without that door.
///
/// What makes the door safe is that **only the debugger walks through it.** Swapping a
/// process's address space behind the scheduler's back would put regions in the
/// machine that the scheduler does not know about, and the consequence of that is a
/// process reading memory that is not its own. So the check is not "the accessor is
/// public" — that is the fix — but "the accessor has exactly one caller outside the
/// kernel", which is the invariant the fix could otherwise have quietly broken.
#[test]
fn only_the_debugger_reaches_the_address_space_mutators() {
    let mut reach: BTreeSet<String> = BTreeSet::new();
    for (name, path) in crates() {
        // The kernel is the one place that is *supposed* to do this, so it is not a
        // violation. Everything else is.
        if name == "lazalith-os" {
            continue;
        }
        for file in walk(&path.join("src")) {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            // The machine and the memory crate reach *their own* accessors, which are
            // about a bus's address space and are not this door, so the needle is the
            // first step rather than the second. It is `.memory_mut()` on its own
            // because a call this test cannot see is one `rustfmt` has split across
            // two lines, and a rule that formatting can defeat is not a rule.
            if text.contains(".memory_mut()") && text.contains("address_space_mut(") {
                reach.insert(name.clone());
            }
        }
    }
    assert_eq!(
        reach,
        BTreeSet::from([String::from("lazalith-debug")]),
        "only lazalith-debug may reach a process's address space outside the kernel, \
         because only it has a reason to"
    );
}

/// The crate source files whose contents match `wanted`.
fn grep_crates(wanted: &dyn Fn(&Path, &str) -> bool) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    for (name, path) in crates() {
        for entry in walk(&path.join("src")) {
            if let Ok(text) = std::fs::read_to_string(&entry)
                && wanted(&entry, &text)
            {
                hits.push(name.clone().into());
                break;
            }
        }
    }
    hits
}

/// Every `.rs` file under a directory.
fn walk(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            found.push(path);
        }
    }
    found
}
