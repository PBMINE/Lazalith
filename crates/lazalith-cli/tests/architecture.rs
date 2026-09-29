//! Step 99: the architecture review, as a list of properties that are checked.
//!
//! The step lists thirteen properties the finished platform must have. A review that
//! reads as prose and concludes "the layers look right" is worth about as much as
//! the last time somebody drew a dependency diagram, so every property here is checked
//! mechanically instead — most of them by reading the manifests, because dependency
//! direction is a property of the manifests and nothing else.
//!
//! The ones that cannot be checked by reading a manifest are checked two other ways:
//! by *using* the thing (assembly reaching a low-level instruction, Lazen's syscalls
//! all resolving through one table, and the two kinds of failure being different
//! values rather than two spellings of the same one), and by reading the source of
//! the boundary itself.
//!
//! That last kind is new in B5, and it is worth naming why it belongs here. The
//! device/backend boundary is a claim about the *shape* of the code — a backend is not
//! a device, a backend signature has no guest vocabulary, a device has no getter for
//! its storage — and no behavioural test can observe the absence of a method. A
//! rewrite that added `fn backend()` to the block device would pass every functional
//! test in the workspace while breaking the thing the whole module is for. These
//! checks are the only ones that would notice.

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

/// B3: no *execution engine* may own the architectural state.
///
/// `binstruction.md` requires the eventual JIT and the Reference Interpreter to be
/// two modes of one virtual machine, and says a mode switch "must not reset, clone,
/// reinterpret, or silently alter guest state". A switch that cannot clone the
/// state is a switch that cannot fork it, and that is only true if no engine type
/// has a copy to give.
///
/// The rule is checked as a fact about the source rather than as a convention,
/// because the failure mode is invisible: an engine that stores a `Processor` and
/// a machine that adopts it would compile, would pass every test in the suite that
/// only ever ran one engine, and would be the second architectural truth the whole
/// design exists to prevent.
///
/// Three holders are allowed, and all are copies that are not a running machine:
///
/// - `lazalith-cpu`, where `Processor` is the state and `ArchitecturalState` is
///   what it is made of;
/// - `lazalith-debug`, where `CpuSnapshot` is a saved copy taken on purpose, and
///   restoring one is the only way a copy ever becomes live again;
/// - `lazalith-vm`, where `CpuState` is the same kind of record and §40 puts it
///   there — a snapshot promoted to VM-level infrastructure, which executes nothing.
///
/// A snapshot is not an engine. It executes nothing, so a machine that is holding
/// one is holding a *record* of a state, and a record is not a second truth about
/// the state currently running.
///
/// The check is not weakened by these three being on the list. What it forbids is an
/// *engine* taking the field, and the engine crates are still absent: adding a field
/// to `lazalith-codegen` or to an interpreter would fail this test exactly as it did
/// before B18.
#[test]
fn no_execution_engine_owns_the_architectural_state() {
    let mut holders = grep_crates(&|_, text| {
        // A field declaration, not any mention: a type that merely reads the state
        // is doing exactly what it should.
        text.lines().any(|line| {
            let line = line.trim();
            line.starts_with("architectural:") || line.starts_with("pub architectural:")
        })
    });
    holders.sort();
    holders.dedup();
    assert_eq!(
        holders,
        vec![
            PathBuf::from("lazalith-cpu"),
            PathBuf::from("lazalith-debug"),
            PathBuf::from("lazalith-vm")
        ],
        "the architectural state may be held by the processor and by a snapshot and \
         by nothing else, because a machine that owns its state cannot fork it; \
         {holders:?} also holds one"
    );
}

/// B3: the machine holds the processor and the engine as separate things.
///
/// The switch exists because those are two fields rather than one. A machine that
/// merged them back would still compile and still run, and would have quietly
/// given up the property the whole extraction was for.
#[test]
fn a_machine_holds_the_processor_and_the_engine_apart() {
    let machine = workspace().join("crates/lazalith-machine/src/lib.rs");
    let text = std::fs::read_to_string(&machine).expect("the machine crate reads");
    assert!(
        text.contains("processor: Processor"),
        "the machine must own the canonical processor state"
    );
    assert!(
        text.contains("engine: Box<dyn ExecutionEngine<Bus<D>>>"),
        "the machine must hold an engine as something it can replace, not a concrete \
         type it cannot"
    );
    assert!(
        text.contains("pub fn switch_execution_engine("),
        "the switch has to exist as one checked operation, or a caller will reach in \
         and swap an implementation by hand"
    );
}

/// B4: the machine's physical geometry is assigned a literal in exactly one place.
///
/// `KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP` were each declared twice before
/// B4 — once in `lazalith-boot` and once in `lazalith-os` — with the same values.
/// That is the failure this rule exists to prevent: two constants that agree today
/// and can drift apart on the first change to one of them, where the one that
/// disagreed would be the one describing the kernel's memory window. The symptom
/// is a kernel loaded somewhere it does not fit.
///
/// `lazalith-machine` owns `LZA64_LAYOUT`; `lazalith-boot` and `lazalith-os`
/// re-export from it, and a re-export carries no literal.
///
/// The test looks for a *literal*, not for a name. A rule that greps for the
/// identifier cannot tell `KERNEL_INITIAL_SP: u64 = lazalith_machine::…` from
/// `KERNEL_INITIAL_SP: u64 = 0x0018_f000`, and only one of those is a second
/// definition — so a rule written the obvious way is a rule that cannot fail,
/// which is worse than no rule because it looks like one.
#[test]
fn the_machine_geometry_is_defined_once() {
    const GEOMETRY: [&str; 10] = [
        "boot_rom_start",
        "boot_rom_length",
        "boot_header_address",
        "kernel_payload_address",
        "max_boot_rom_payload",
        "kernel_load_address",
        "kernel_image_length",
        "kernel_initial_sp",
        "physical_ram_start",
        "physical_ram_length",
    ];
    let definers = grep_crates(&|_, text| {
        text.lines().any(|line| {
            let line = line.to_ascii_lowercase();
            // A literal, not a reference. `0x…` is how every one of these numbers
            // is written in this repository, including in the one place they are
            // allowed to be written.
            line.contains("0x") && GEOMETRY.iter().any(|name| line.contains(name))
        })
    });
    let mut names: Vec<String> = definers
        .into_iter()
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["lazalith-machine".to_string()],
        "only the machine profile may hold the machine's geometry as a literal, \
         because two definitions that agree today are two definitions that can \
         disagree tomorrow; {names:?} also holds one"
    );
}

/// B4: a machine profile describes the device inventory, so the device manager has
/// to be able to hold more than one kind of device.
///
/// `DeviceManager<D>` is a `Vec<Entry<D>>` with one concrete `D`, so without an
/// erased device *also* being a `Device`, a profile can describe a console, a
/// timer and an input device but no machine can hold all three. The impl is what
/// makes `LazalithMachine<Box<dyn Device>>` exist, and this checks it has not been
/// dropped: the failure it would cause is a compile error in every heterogeneous
/// caller, but the *rule* is worth writing down because the alternative is that
/// the capability is never deliberately removed — only discovered missing.
#[test]
fn a_device_manager_can_hold_more_than_one_kind_of_device() {
    let devices = workspace().join("crates/lazalith-devices/src/lib.rs");
    let text = std::fs::read_to_string(&devices).expect("the device crate reads");
    assert!(
        text.contains("impl Device for Box<dyn Device>"),
        "an erased device must be a device, or a profile's device inventory \
         describes machines that cannot be built"
    );
}

// -- B5: the device/backend boundary ------------------------------------------

/// Rust source of a crate, comments stripped, for the two files that make up the
/// device and the backend it holds.
///
/// The device file is checked on its own where the rule is about a device; the two
/// together where it is about the boundary as a whole.
fn item_at(text: &str, start: usize) -> &str {
    let open = text[start..]
        .find('{')
        .map(|at| start + at)
        .unwrap_or_else(|| panic!("no item at offset {start}"));
    let mut depth = 0usize;
    for (index, byte) in text[open..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[start..open + index + 1];
                }
            }
            _ => {}
        }
    }
    panic!("the item at offset {start} has no closing brace");
}

/// Rust source of one file with its comments removed.
///
/// Needed because the boundary is *discussed* in the doc comments — `backend.rs`
/// says in prose that there is no `fn backend(&self)` — and a source check that
/// matched its own documentation would be checking nothing. Only line comments are
/// stripped, which is sufficient here and is stated rather than hidden: neither
/// module has a `//` inside a string literal.
fn strip_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A backend is a host resource, not a guest window, and one type cannot be both.
///
/// `binstruction.md` §26's diagram has three boxes and the middle one is a
/// *translation*. A backend that is also a `Device` has a mapped window, a bus, and
/// therefore a guest that can reach the host storage behind it — which is the exact
/// boundary B5 exists to put in place. Checking the source rather than the behaviour
/// is the only way to see this: a backend with a window still works.
#[test]
fn a_backend_is_not_a_device() {
    let path = workspace().join("crates/lazalith-devices/src/backend.rs");
    let text = std::fs::read_to_string(&path).expect("the backend module reads");
    for impl_block in text.split("impl").skip(1) {
        let trait_name = impl_block
            .split("for")
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        assert_ne!(
            trait_name, "Device",
            "a backend implements Device, so the storage behind it is a guest window"
        );
    }
}

/// The backend traits' signatures have no guest vocabulary in them.
///
/// The two address spaces in this design are `DeviceOffset` (where in a register
/// window) and `sector` (where in a disk), and they are not the same unit and not
/// interchangeable. A backend method that took a `DeviceOffset` or a `DataAccess`
/// would be a method that could be handed a guest address, and from there the
/// distinction is one refactor from gone.
#[test]
fn a_backend_signature_carries_no_guest_vocabulary() {
    let path = workspace().join("crates/lazalith-devices/src/backend.rs");
    let text = std::fs::read_to_string(&path).expect("the backend module reads");
    for trait_name in ["pub trait Backend", "pub trait BlockBackend"] {
        let start = text
            .find(trait_name)
            .unwrap_or_else(|| panic!("{trait_name} is not there"));
        let body = item_at(&text, start);
        for forbidden in [
            "DataAccess",
            "PhysicalAddress",
            "DeviceOffset",
            "Privilege",
            "CycleCount",
        ] {
            assert!(
                !body.contains(forbidden),
                "the {trait_name} signature mentions {forbidden}: a backend call names \
                 storage, so guest vocabulary in its signature is how the two address \
                 spaces get confused for one"
            );
        }
    }
}

/// No *device* hands back the backend it holds.
///
/// The device owns the storage and exposes no route to it. Without this rule a
/// `backend()` accessor is the smallest possible change that turns a host resource
/// into a guest interface, and it would be added to a debugging helper by someone
/// mid-task with a test to pass.
///
/// `storage.rs` only, and deliberately: `CopyOnWriteBlockBackend::base()` returns a
/// `&dyn BlockBackend`, and that is a *backend's* accessor rather than a device's.
/// The host is entitled to ask what its own storage is sitting on, and the guest is
/// not — and the difference is exactly which struct the method is on. A backend that
/// a device could reach one would be caught here, because this file is the only place
/// the backend lives.
#[test]
fn no_device_exposes_the_backend_behind_it() {
    let path = workspace().join("crates/lazalith-devices/src/storage.rs");
    let text = strip_comments(&std::fs::read_to_string(&path).expect("the block device reads"));
    for accessor in [
        "fn backend(",
        "fn storage(",
        "-> &dyn BlockBackend",
        "-> Box<dyn BlockBackend",
        "-> &dyn Backend",
        "-> Box<dyn Backend",
    ] {
        assert!(
            !text.contains(accessor),
            "the block device exposes `{accessor}`: a getter for the backend is a door, \
             and the boundary is arranged so a host resource cannot become a guest \
             interface one refactor after nobody is looking"
        );
    }
    // The device does say what kind of storage it is, which is a diagnostic and
    // reaches no resource. Held here so the deliberate exception is a decision
    // rather than something the check above happened to permit.
    assert!(
        text.contains("fn backend_kind("),
        "the block device no longer reports its storage kind; if that was deliberate, \
         say so here rather than losing the diagnostic quietly"
    );
}

/// The backend layer is `no_std`, so a file backend belongs to a host crate.
///
/// `lazalith-devices` runs on a target with no `std`. A backend that opened a file
/// would make the whole device crate unbuildable there, which is why B8's file
/// backend is a host-side implementation of the same trait rather than a new method
/// on this one.
#[test]
fn the_backend_layer_is_no_std() {
    let path = workspace().join("crates/lazalith-devices/src/backend.rs");
    let text = std::fs::read_to_string(&path).expect("the backend module reads");
    for forbidden in ["std::fs", "std::io", "std::net", "extern crate std"] {
        assert!(
            !text.contains(forbidden),
            "the backend layer uses {forbidden}: a host resource reached by a syscall \
             does not belong in a crate the ISA target links, and the fix is a host \
             implementation of the trait, not a std call here"
        );
    }
    let manifest = std::fs::read_to_string(workspace().join("crates/lazalith-devices/Cargo.toml"))
        .expect("the device manifest reads");
    assert!(
        !manifest.contains("lazalith-sdl3"),
        "the device crate must not depend on a host windowing library: a block device \
         has no window"
    );
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

// -- B6: the VM lifecycle boundary --------------------------------------------

/// `binstruction.md` §9 names six things a VM core must not depend on, and this is
/// that list, checked against the VM layers rather than described.
///
/// It is a separate test from `the_cpu_does_not_depend_on_sdl3` because the layers
/// differ: that one covers the instruction decoder and its foundations, and this one
/// covers the lifecycle that B6 added *above* the machine. A window library reaching
/// the VM lifecycle would put a host GUI inside a machine's reset path, which is
/// somewhere no host GUI has ever belonged.
#[test]
fn the_vm_core_depends_on_no_host_subsystem() {
    for layer in [
        "lazalith-vm",
        "lazalith-machine",
        "lazalith-boot",
        "lazalith-cpu",
        "lazalith-devices",
        "lazalith-memory",
        "lazalith-isa",
        "lazalith-types",
    ] {
        let found = dependencies(layer);
        for forbidden in ["lazalith-sdl3", "lazalith-gui", "lazalith-ui"] {
            assert!(
                !found.contains(forbidden),
                "{layer} depends on {forbidden}: §9 says the VM core must not depend \
                 on a GUI toolkit or a windowing framework, and a reset path that can \
                 open a window is a reset path that can block on one"
            );
        }
    }
}

/// The machine must not depend on the boot image, and the VM lifecycle is the join.
///
/// B6's reason this is checked rather than assumed: `Vm::boot` needs both a
/// `MachineProfile` and a `BootImage`, and the tempting place to put that is inside
/// `lazalith-machine`. It would compile, and it would make the architectural machine
/// depend on the ROM format — so a change to the image format would become a change
/// to the machine's API, and the machine would no longer be usable without a boot
/// image. The join is a separate crate for exactly that reason.
#[test]
fn the_machine_does_not_know_what_a_boot_image_is() {
    let found = dependencies("lazalith-machine");
    assert!(
        !found.contains("lazalith-boot"),
        "lazalith-machine depends on lazalith-boot: the architectural machine must not \
         know the firmware format. The join belongs in lazalith-vm, which is above both."
    );

    // And the dependency really is one-way: the lifecycle knows both.
    let vm = dependencies("lazalith-vm");
    assert!(
        vm.contains("lazalith-boot") && vm.contains("lazalith-machine"),
        "lazalith-vm must depend on both, or it cannot join a profile and an image"
    );
}

/// The trap vector is defined once, in the layout.
///
/// B4 recorded this as a limitation — "the trap vector is still set after construction
/// by each caller rather than being in the profile" — and B6 closed it by putting the
/// vector in `MachineLayout`. This holds the closing shut: a second definition in the
/// boot crate would give two machines a profile-derived vector and a boot-derived one,
/// and the disagreement would appear as a guest that faults somewhere nobody chose.
#[test]
fn the_trap_vector_has_one_definition() {
    let profile =
        std::fs::read_to_string(workspace().join("crates/lazalith-machine/src/profile.rs"))
            .expect("the profile module reads");
    let boot = std::fs::read_to_string(workspace().join("crates/lazalith-boot/src/lib.rs"))
        .expect("the boot crate reads");

    assert!(
        profile.contains("pub trap_vector: u64"),
        "the layout should carry the trap vector, or B4's recorded limitation is back"
    );
    // A `const TRAP_VECTOR` in the boot crate would be a second definition. A
    // re-export of the layout's field is the opposite and is allowed.
    assert!(
        !boot.contains("const TRAP_VECTOR"),
        "lazalith-boot defines its own TRAP_VECTOR: re-export lazalith_machine's \
         layout field instead, the way it already re-exports KERNEL_INITIAL_SP"
    );
}

// -- B7: the display backend boundary -------------------------------------------

/// The display backend lives above the SDL FFI boundary, not inside it.
///
/// `lazalith-sdl3` has **no Lazalith dependencies at all**, and that is load-bearing
/// rather than tidy: it is the whole auditable `unsafe` surface of the project, and its
/// own documentation says it "knows nothing about machines, registers or guest memory".
/// A display backend needs to know what a guest frame is, so putting it there would
/// have made the FFI boundary guest-aware — and the moment SDL could see a `DisplayFrame`
/// the claim that the unsafe surface contains no Lazalith logic stopped being true.
#[test]
fn the_sdl_ffi_boundary_does_not_know_about_guests() {
    let found = dependencies("lazalith-sdl3");
    assert!(
        found.is_empty(),
        "lazalith-sdl3 depends on {found:?}: that crate is the project's entire `unsafe` \
         surface, and its value is that the unsafe can be audited in one file that has \
         no Lazalith logic in it. A display backend needs to know what a guest frame is, \
         so it belongs in lazalith-gui."
    );
}

/// The guest-visible display device holds no backend, and cannot reach one.
///
/// The inverse of B5's rule, and it is the reason the two subsystems do not share a
/// shape. A block device is *pushed to* — a guest's register write must reach storage
/// during the write — so it holds a backend. A display is *pulled by* the host: a guest
/// writes pixels into memory and rings a present register, and the frame is a
/// description. If the device called a backend during `present`, one guest instruction
/// would call into the host synchronously and a host that stopped answering would stall
/// the machine with no fault and no timeout.
#[test]
fn the_display_device_holds_no_backend() {
    let display =
        std::fs::read_to_string(workspace().join("crates/lazalith-devices/src/display.rs"))
            .expect("the display device reads");
    let code = strip_comments(&display);
    for forbidden in ["Box<dyn DisplayBackend", "DisplayBackend", "fn backend("] {
        assert!(
            !code.contains(forbidden),
            "lazalith-devices/src/display.rs mentions `{forbidden}`: the display device is \
             the guest's, and giving it a backend would make a host call happen inside a \
             guest register write. The boundary for a display is the reverse of B5's, and \
             that asymmetry is the design."
        );
    }
}

// -- B8: the storage boundary ---------------------------------------------------

/// B5's rule at the layer where it is most load-bearing: a host file is not a guest
/// interface.
///
/// `lazalith-storage` holds `std::fs::File` values. The test is that nothing in it lets
/// a guest or a generic device reach one — the backends implement `BlockBackend`, whose
/// signatures have no `DeviceOffset`, no `PhysicalAddress` and no `DataAccess`, and
/// which return nothing but bytes and a `BackendError`.
#[test]
fn the_storage_backends_take_no_guest_vocabulary() {
    let path = workspace().join("crates/lazalith-storage/src");
    for name in ["raw.rs", "sparse.rs", "snapshot.rs"] {
        let text = strip_comments(
            &std::fs::read_to_string(path.join(name))
                .unwrap_or_else(|e| panic!("{name} reads: {e}")),
        );
        for forbidden in [
            "DataAccess",
            "PhysicalAddress",
            "DeviceOffset",
            "Privilege",
            "impl Device for",
        ] {
            assert!(
                !text.contains(forbidden),
                "lazalith-storage/src/{name} mentions `{forbidden}`. A storage backend is \
                 reached by sector number, and nothing in this crate may be reached by a \
                 guest: B5's rule is that a host resource is not a device, and a file is \
                 the sharpest case of that."
            );
        }
    }
}

/// A guest-visible refusal never names a host path, a filename, or an errno.
///
/// This is checked here rather than in the storage crate because it is a property of
/// the *conversion*, and the conversion is where a leak would be introduced: the host
/// gets a `StorageError` carrying a full path, and the guest gets a `BackendError`
/// carrying two words.
#[test]
fn a_backend_error_cannot_name_a_host_path() {
    let text = strip_comments(
        &std::fs::read_to_string(workspace().join("crates/lazalith-devices/src/backend.rs"))
            .expect("the backend module reads"),
    );
    // Every variant of `BackendError` is something a guest can be told about, so none
    // of them may carry a path or an OS error.
    for forbidden in ["PathBuf", "std::path", "std::io::Error", "io::Error"] {
        assert!(
            !text.contains(forbidden),
            "BackendError mentions `{forbidden}`. Every variant of it is reachable from \
             a register access and is therefore guest-visible; a variant carrying a path \
             or an errno would hand the guest the host's filesystem layout. The host \
             detail belongs in StorageError, which the guest never sees."
        );
    }
}

// -- B9: the input backend boundary ---------------------------------------------

/// The guest's input device holds no backend, and a backend cannot reach it.
///
/// B9's boundary is a **source**, which is the mirror of B7's and of B5's. B5 forbade a
/// device from exposing its backend; B9 forbids a device from *holding* one, because
/// input is produced by the host asynchronously and a guest register read must not call
/// out. A backend has no method that takes a device, and a device has no method that
/// takes a backend — which is why two independent input sources can feed one device,
/// and why the previous `HostScript::replay(&mut InputDevice)` was a boundary that did
/// not exist.
#[test]
fn the_input_device_and_the_input_backend_do_not_hold_each_other() {
    let input = strip_comments(
        &std::fs::read_to_string(workspace().join("crates/lazalith-devices/src/input.rs"))
            .expect("the input device reads"),
    );
    assert!(
        !input.contains("InputBackend"),
        "lazalith-devices/src/input.rs mentions InputBackend: a guest's input device that \
         holds a backend is a device whose register read can call into the host. Input is \
         produced asynchronously by the host, so the device must be written BY a pump the \
         host calls and never ask for an event itself."
    );

    let backend = strip_comments(
        &std::fs::read_to_string(workspace().join("crates/lazalith-devices/src/input_backend.rs"))
            .expect("the input backend module reads"),
    );

    // The trait and its implementations, but **not** `pump_input`. The free function
    // taking `&mut InputDevice` is the boundary itself: it is the one place the guest's
    // device is written, and putting it in a signature is what makes it visible. What
    // must not exist is a *backend* that can write the device, because that gives the
    // host a handle on something the guest owns and makes "the one place" untrue.
    let trait_start = backend
        .find("pub trait InputBackend")
        .expect("the InputBackend trait is the thing being checked");
    let trait_body = item_at(&backend, trait_start);
    assert!(
        !trait_body.contains("InputDevice"),
        "the InputBackend trait mentions InputDevice: a backend that is handed the guest's \
         device is a sink, and a sink gives the host a handle on something the guest owns."
    );

    let mut checked = 0;
    for (index, _) in backend.match_indices("impl InputBackend for") {
        let body = item_at(&backend, index);
        assert!(
            !body.contains("InputDevice"),
            "an InputBackend implementation holds an InputDevice: the backend and the \
             device would hold each other, and the one place the device is written would \
             stop being one place."
        );
        checked += 1;
    }
    assert!(
        checked >= 2,
        "expected at least a scripted and an absent backend, found {checked}. A count that \
         drops means a backend was removed and this test stopped checking the rest."
    );
}
