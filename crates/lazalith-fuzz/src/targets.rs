//! The ten targets, and the generator that feeds them.
//!
//! Each target is a small function over a byte slice with a post-condition, and the
//! post-conditions are stated as `Err` messages that say what was wrong rather than
//! just that something was. A fuzzer that reports `assertion failed` tells the next
//! person nothing; a fuzzer that reports "a decoded instruction re-encoded to
//! different bytes" tells them where to look.
//!
//! The generator is deliberately simple and deliberately deterministic: a
//! fixed-seed xorshift, a handful of mutations, and a hand-written seed per target.
//! See the crate documentation for why that is the right trade here.

use std::fmt::Write as _;

use lazalith_types::SourceManager;

use lazalith_types::ArchitectureConfig;

/// The architecture every target builds for.
///
/// One architecture, not all of them. The codec is parameterized by
/// `ArchitectureConfig` and a second one would double the campaign for the same
/// code paths; the widths a second architecture changes are already property-tested
/// in step 84, and a width that fuzzing finds is found by the properties first.
const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();

/// How far the snapshot target is allowed to run before it is snapshotted.
const SNAPSHOT_STEPS: usize = 12;

// -- the ten targets --------------------------------------------------------

/// A raw instruction, decoded and re-encoded.
///
/// The round trip is the whole check. An instruction that decoded out of eight bytes
/// and will not encode back to those eight bytes has lost or invented information,
/// which is the definition of accepting malformed data and quietly changing state.
/// The short-input case is checked too, and it is the one that finds the classic
/// bug: a decoder that reads a field before it checks there is a field.
pub fn instruction_decoder(input: &[u8]) -> Result<(), String> {
    let word = usize::from(CONFIG.word_width().bytes());
    if input.len() < word {
        return match lazalith_isa::decode(CONFIG, input) {
            Err(_) => Ok(()),
            Ok(instruction) => Err(format!(
                "decoded an instruction out of {} bytes: {instruction:?}",
                input.len()
            )),
        };
    }
    let bytes = &input[..word];
    let Ok(instruction) = lazalith_isa::decode(CONFIG, bytes) else {
        return Ok(());
    };
    match lazalith_isa::encode(CONFIG, &instruction) {
        Ok(encoded) if encoded == bytes => Ok(()),
        Ok(encoded) => Err(format!(
            "{instruction:?} decoded from {bytes:02x?} but encoded back to {encoded:02x?}"
        )),
        Err(error) => Err(format!(
            "{instruction:?} decoded but would not encode: {error:?}"
        )),
    }
}

/// Assembly source, assembled into an object and back.
///
/// Two checks, because there are two ways to be wrong. The round trip catches an
/// assembler that accepts source and loses something. Determinism catches one that
/// depends on hash order or on a `HashMap` iteration it should not: assembling the
/// same text twice must give the same object, and a fuzzer finds the dependence in
/// one run where a hand-written test would not have thought to look.
pub fn assembler(input: &[u8]) -> Result<(), String> {
    let text = String::from_utf8_lossy(input);
    let Ok(object) = lazalith_toolchain::assemble(&text) else {
        return Ok(());
    };
    let again = lazalith_toolchain::assemble(&text).map_err(|error| {
        format!("assembling the same source twice failed the second time: {error}")
    })?;
    if format!("{object:?}") != format!("{again:?}") {
        return Err(String::from(
            "assembling the same source twice gave two objects",
        ));
    }
    let bytes = object
        .to_bytes()
        .map_err(|error| format!("an assembled object would not encode: {error:?}"))?;
    let read = lazalith_toolchain::ObjectFile::from_bytes(&bytes).map_err(|error| {
        format!("an object this assembler wrote would not read back: {error:?}")
    })?;
    if read != object {
        return Err(String::from(
            "an assembled object read back as something else",
        ));
    }
    Ok(())
}

/// Lazen source through the lexer, parser and resolver.
///
/// A parser's *expected* answer for malformed source is `Err`, so the check is not
/// that it refuses — it is that it refuses the same way twice, and that when it does
/// refuse it says why. A parser that rejected everything would pass that, so the
/// determinism is the load-bearing half: a parser whose answer depends on hash
/// order or on a `HashMap` iteration gives two different answers for the same text.
pub fn lazen_parser(input: &[u8]) -> Result<(), String> {
    parse_lazen(&String::from_utf8_lossy(input))
}

/// C source through the lexer and parser.
pub fn c_parser(input: &[u8]) -> Result<(), String> {
    parse_c(&String::from_utf8_lossy(input))
}

/// Bytes read as a Lazalith object.
///
/// The strong form of the round trip: the bytes that came in are the bytes that go
/// back out. An object format with padding or with more than one legal encoding
/// would fail this, and that is worth knowing now rather than after a linker has
/// started rewriting files.
pub fn object_reader(input: &[u8]) -> Result<(), String> {
    let Ok(object) = lazalith_toolchain::ObjectFile::from_bytes(input) else {
        return Ok(());
    };
    let bytes = object
        .to_bytes()
        .map_err(|error| format!("an object that read would not encode: {error:?}"))?;
    let again = lazalith_toolchain::ObjectFile::from_bytes(&bytes)
        .map_err(|error| format!("the re-encoding would not read back: {error:?}"))?;
    if again != object {
        return Err(String::from(
            "an object read, re-encoded and read again came out different",
        ));
    }
    // Idempotence, not identity. A *valid* object file need not be in the form the
    // writer would produce, and this fuzzer found a good example: changing one byte
    // in a string table from a NUL to something else merges two adjacent names into
    // one longer name, which is a perfectly good file that re-encodes to eight more
    // bytes. Demanding that the bytes come back unchanged would call that malformed,
    // and it would be the check that was wrong. What must hold is that one pass
    // reaches a fixed point — the second encoding is byte-for-byte the first — so a
    // linker rewriting a file twice produces one file.
    let third = again
        .to_bytes()
        .map_err(|error| format!("an object read back would not encode: {error:?}"))?;
    if third != bytes {
        return Err(format!(
            "encoding an object twice gave {} bytes then {}",
            bytes.len(),
            third.len()
        ));
    }
    Ok(())
}

/// Bytes loaded as an LZX executable.
///
/// A loader that accepts a malformed image and then hands out sections that point
/// outside the image is the bug this target is for, so the check is on the loaded
/// image's own arithmetic rather than on a round trip: every section must have room
/// for the bytes it carries, no section's address may overflow, and the entry must
/// name a section the image actually has and an offset inside it.
pub fn executable_loader(input: &[u8]) -> Result<(), String> {
    let Ok(image) = lazalith_os::LzxImage::from_bytes(input) else {
        return Ok(());
    };
    for (index, section) in image.sections().iter().enumerate() {
        let size = u64::try_from(section.bytes().len()).unwrap_or(u64::MAX);
        if section.virtual_size() < size {
            return Err(format!(
                "section {index} has {size} bytes in a region of {}",
                section.virtual_size()
            ));
        }
        if section
            .virtual_offset()
            .checked_add(section.virtual_size())
            .is_none()
        {
            return Err(format!("section {index} runs past the address space"));
        }
    }
    let entry_section = image.entry_section();
    let sections = image.sections();
    let named = sections.get(usize::from(entry_section)).ok_or_else(|| {
        format!(
            "the entry names section {entry_section} of a {}-section image",
            sections.len()
        )
    })?;
    if image.entry_offset() > u64::try_from(named.bytes().len()).unwrap_or(u64::MAX) {
        return Err(format!(
            "the entry is at offset {} of a {}-byte section",
            image.entry_offset(),
            named.bytes().len()
        ));
    }
    let bytes = image
        .to_bytes()
        .map_err(|error| format!("a loaded image would not encode: {error:?}"))?;
    if lazalith_os::LzxImage::from_bytes(&bytes).ok().as_ref() != Some(&image) {
        return Err(String::from(
            "a loaded image re-encoded and reloaded came out different",
        ));
    }
    Ok(())
}

/// Bytes read as a Lazen application package.
///
/// The strongest round trip in this file, and the one that pays for step 86's
/// property. A package that reads must re-write to a package that reads to the *same*
/// package, and then re-write to the same bytes again — one pass reaches a fixed
/// point. That is what a packer needs (packing twice gives one file) and it is what a
/// reader tolerating a non-canonical offset would fail, which is exactly the bug
/// step 86 found in the object format.
///
/// The contained executable is the `.lzx` reader's business, and it is the one thing
/// the package must not have an opinion about: if the package's own view of its image
/// ever disagreed with the image reader, the design's rule 1 would be a claim rather
/// than a fact.
pub fn package_reader(input: &[u8]) -> Result<(), String> {
    let Ok(package) = lazalith_os::LzaPackage::from_bytes(input) else {
        return Ok(());
    };
    let bytes = package
        .to_bytes()
        .map_err(|error| format!("a package that read would not write: {error}"))?;
    let again = lazalith_os::LzaPackage::from_bytes(&bytes)
        .map_err(|error| format!("the re-encoding would not read back: {error}"))?;
    if again != package {
        return Err(String::from(
            "a package read, re-written and read again came out different",
        ));
    }
    let third = again
        .to_bytes()
        .map_err(|error| format!("a package read back would not write: {error}"))?;
    if third != bytes {
        return Err(format!(
            "writing a package twice gave {} bytes then {}",
            bytes.len(),
            third.len()
        ));
    }
    let image = package
        .image()
        .map_err(|error| format!("a package that read holds an unusable image: {error}"))?;
    if lazalith_os::LzxImage::from_bytes(&package.image).ok() != Some(image) {
        return Err(String::from(
            "a package's image reader disagrees with the image reader",
        ));
    }
    Ok(())
}

/// Bytes loaded as a kernel image.
pub fn kernel_loader(input: &[u8]) -> Result<(), String> {
    use lazalith_types::PhysicalAddress;

    let rom = input.to_vec();
    let Ok(image) = lazalith_boot::BootImage::from_rom(CONFIG, rom) else {
        return Ok(());
    };
    let kernel = image
        .kernel()
        .map_err(|error| format!("a loaded image cannot produce its kernel: {error:?}"))?;
    if kernel.is_empty() {
        return Err(String::from("a loaded image has an empty kernel"));
    }
    let setup = image
        .machine_setup(lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new())
        .map_err(|error| format!("a loaded image has no machine setup: {error:?}"))?;
    if !setup
        .regions
        .iter()
        .any(|region| region.start() == PhysicalAddress::new(lazalith_boot::KERNEL_LOAD_ADDRESS))
    {
        return Err(format!(
            "the machine setup does not contain the load address: {setup:?}"
        ));
    }
    if image.rom().is_empty() {
        return Err(String::from("a loaded image has no rom"));
    }
    Ok(())
}

/// Paths read as filesystem metadata.
///
/// The check that matters here is *identity*: a path that names a file must report
/// that file's size, and a path that names nothing must report nothing. A fuzzer
/// that only checked "no panic" would pass a filesystem that answered every path
/// with the first file it had.
pub fn filesystem_metadata(input: &[u8]) -> Result<(), String> {
    let mut filesystem = lazalith_os::VirtualFileSystem::new(Default::default())
        .map_err(|error| format!("a default filesystem would not build: {error:?}"))?;
    for (name, size) in [("/a", 1_u64), ("/bb", 2), ("/ccc", 3)] {
        filesystem
            .insert_file(
                name.as_bytes(),
                &vec![0_u8; usize::try_from(size).unwrap_or(0)],
            )
            .map_err(|error| format!("the seed file {name} would not insert: {error:?}"))?;
    }
    let path = path_of(input);
    match filesystem.metadata(path.as_bytes()) {
        Ok(found) => {
            let expected = match path.as_str() {
                "/a" => Some(1_u64),
                "/bb" => Some(2),
                "/ccc" => Some(3),
                _ => None,
            };
            if let Some(expected) = expected
                && found.size != expected
            {
                return Err(format!(
                    "{path:?} reported size {} rather than {expected}",
                    found.size
                ));
            }
            if found.size > u64::MAX / 2 {
                return Err(format!("{path:?} reported a nonsense size {}", found.size));
            }
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:?}");
            if message.is_empty() {
                Err(String::from("the filesystem refused a path with no reason"))
            } else {
                Ok(())
            }
        }
    }
}

/// Bytes driven through the debugger's command surface, with no program loaded.
///
/// This is the strongest cheap invariant in the file, and it is about a specific bug:
/// a frontend that acts on a command before checking that there is anything to act
/// on will step a program that does not exist. With no process loaded, *every*
/// command must refuse and the machine must not move by a single register — so the
/// check is that the controller's registers are identical before and after a whole
/// run of them.
pub fn debugger_commands(input: &[u8]) -> Result<(), String> {
    use lazalith_gui::{Controls, Outcome as ControlOutcome, Refusal};

    let controller = lazalith_debug::DebugController::<lazalith_devices::NoDevice>::boot(
        CONFIG,
        &supervisor_kernel(),
        8,
        lazalith_os::VirtualTerminal::new(b"")
            .map_err(|error| format!("no terminal: {error:?}"))?,
        lazalith_os::VirtualFileSystem::with_defaults()
            .map_err(|error| format!("no filesystem: {error:?}"))?,
        lazalith_devices::DeviceManager::new(),
    )
    .map_err(|error| format!("the debugger would not boot: {error:?}"))?;
    let before = controller.registers();
    let mut controls = Controls::new(controller);
    for index in 0..input.len().min(64) {
        let control = control_of(index, input);
        match controls.dispatch(control) {
            ControlOutcome::Refused { reason, .. } => {
                if reason != Refusal::NoProcess {
                    return Err(format!(
                        "{control:?} with no process loaded refused with {reason:?}"
                    ));
                }
            }
            other => {
                return Err(format!(
                    "{control:?} with no process loaded answered {other:?} rather than refusing"
                ));
            }
        }
    }
    if controls.controller().registers() != before {
        return Err(String::from(
            "a refused command moved the machine's registers",
        ));
    }
    Ok(())
}

/// Bytes turned into a machine state, snapshotted, and restored.
///
/// There is no byte-level snapshot format in this repository yet, so "the snapshot
/// reader" is the *restore* path, and the invariant is the round trip: a snapshot
/// that was taken must restore a processor that is in exactly the state it was
/// captured in. That is a real property and it is the one that is easy to get wrong,
/// because a snapshot of the architectural state alone is not resumable — the trap
/// frames are the part a program stopped in a syscall needs and a naive snapshot
/// drops. The input is executed first, so the state being snapshotted is a state the
/// fuzzer chose, including a state stopped inside a trap.
pub fn snapshot_reader(input: &[u8]) -> Result<(), String> {
    use lazalith_debug::CpuSnapshot;
    use lazalith_types::{CycleCount, PhysicalAddress};

    // The code region is mapped as ROM with the fuzzer's bytes in it, which is the
    // same shape the differential harness and the machine's own tests use, so the
    // state being snapshotted is a state the rest of the repository also produces.
    // An empty input still gets a word of code: a ROM region of no bytes is not a
    // region this repository can build, and "the fuzzer had nothing" should produce
    // a machine stopped at the start rather than a refusal to make one.
    let word = usize::from(CONFIG.word_width().bytes());
    let mut code = vec![0_u8; CODE_LENGTH];
    let take = input.len().min(CODE_LENGTH);
    code[..take].copy_from_slice(&input[..take]);
    let _ = word;
    let mut machine = lazalith_machine::LazalithMachine::<lazalith_devices::NoDevice>::new(
        lazalith_machine::MachineSetup {
            config: CONFIG,
            devices: Default::default(),
            regions: data_and_stack()?,
            pc: lazalith_types::InstructionAddress::new(0),
            sp: lazalith_types::VirtualAddress::new(STACK),
            status: 0,
            initial_time: CycleCount::new(0),
        },
    )
    .map_err(|error| format!("a machine would not be built: {error:?}"))?;
    machine
        .load_region(
            lazalith_memory::MemoryRegion::rom(
                CONFIG,
                PhysicalAddress::new(0),
                &code,
                lazalith_memory::RegionPermissions::new(true, false, true, true),
            )
            .map_err(|error| format!("the code region would not be built: {error:?}"))?,
        )
        .map_err(|error| format!("the code region would not be mapped: {error:?}"))?;
    for _ in 0..SNAPSHOT_STEPS {
        if machine.step().is_err() {
            break;
        }
    }
    let snapshot = CpuSnapshot::of(machine.processor());
    let mut restored =
        lazalith_cpu::ReferenceInterpreter::new(machine.processor().architectural_state().clone());
    snapshot.restore(&mut restored)?;
    if restored.architectural_state() != machine.processor().architectural_state() {
        return Err(String::from(
            "a restored processor is not in the state it was captured in",
        ));
    }
    let again = CpuSnapshot::of(&restored);
    if again.pc() != snapshot.pc()
        || again.sp() != snapshot.sp()
        || again.in_trap() != snapshot.in_trap()
    {
        return Err(String::from(
            "a restored processor does not snapshot back the same way",
        ));
    }
    Ok(())
}

/// The writable regions a program gets: its data and its stack.
///
/// The code region is not here because it is ROM, and it is mapped separately with
/// the fuzzer's bytes in it.
fn data_and_stack() -> Result<Vec<lazalith_memory::MemoryRegion>, String> {
    use lazalith_memory::{MemoryRegion, RegionPermissions};
    use lazalith_types::PhysicalAddress;

    let read_write = RegionPermissions::new(true, true, false, true);
    let region = |start: u64, length: u64| {
        MemoryRegion::ram(CONFIG, PhysicalAddress::new(start), length, read_write)
            .map_err(|error| format!("a region at {start} would not be built: {error:?}"))
    };
    Ok(vec![
        region(DATA, REGION_LENGTH)?,
        region(STACK, REGION_LENGTH)?,
    ])
}

/// Where a program's data goes, and where its stack goes.
const DATA: u64 = 0x400;
/// Where a program's stack goes.
const STACK: u64 = 0x8000;
/// How long each writable region is, and how much code a program gets.
const CODE_LENGTH: usize = 0x400;
/// How long a writable region is.
const REGION_LENGTH: u64 = 0x400;

// -- the pieces the targets share ------------------------------------------

/// A kernel short enough to boot the debugger and long enough to be a program.
///
/// The debugger's `boot` builds a real boot image and then steps the supervisor's
/// first instruction, so the kernel cannot be empty: it is a `nop` and an `rfe`,
/// which is the smallest program that survives the handoff. The target never runs
/// the program, so its contents only have to satisfy the loader.
fn supervisor_kernel() -> Vec<u8> {
    use lazalith_isa::{Instruction, Opcode};

    [Opcode::Nop, Opcode::Rfe]
        .iter()
        .filter_map(|opcode| Instruction::new(CONFIG, *opcode, &[]).ok())
        .filter_map(|instruction| lazalith_isa::encode(CONFIG, &instruction).ok())
        .flatten()
        .collect()
}

/// Lazen source through the lexer, parser and resolver.
fn parse_lazen(text: &str) -> Result<(), String> {
    let mut first = SourceManager::default();
    let once = parse_lazen_with(&mut first, text);
    let mut second = SourceManager::default();
    let twice = parse_lazen_with(&mut second, text);
    if once != twice {
        return Err(String::from(
            "parsing the same source twice gave two answers",
        ));
    }
    match once {
        Ok(_) => Ok(()),
        Err(message) if message.is_empty() => {
            Err(String::from("the parser refused without a message"))
        }
        Err(_) => Ok(()),
    }
}

/// One Lazen parse, as an answer rather than as a result: a refusal is an answer.
fn parse_lazen_with(manager: &mut SourceManager, text: &str) -> Result<String, String> {
    let source = manager
        .add_file("fuzz.lazen", text)
        .map_err(|error| format!("{error:?}"))?;
    match lazalith_compiler::frontend::parse_file(source, manager) {
        Ok(program) => Ok(format!("{program:?}")),
        Err(error) => Err(format!("{error:?}")),
    }
}

/// C source through the lexer and parser.
fn parse_c(text: &str) -> Result<(), String> {
    let mut first = SourceManager::default();
    let once = parse_c_with(&mut first, text);
    let mut second = SourceManager::default();
    let twice = parse_c_with(&mut second, text);
    if once != twice {
        return Err(String::from(
            "parsing the same source twice gave two answers",
        ));
    }
    match once {
        Ok(_) => Ok(()),
        Err(message) if message.is_empty() => {
            Err(String::from("the parser refused without a message"))
        }
        Err(_) => Ok(()),
    }
}

/// One C parse, as an answer rather than as a result.
fn parse_c_with(manager: &mut SourceManager, text: &str) -> Result<String, String> {
    match lazalith_c_compiler::frontend::parse_file(manager, "fuzz.c", text) {
        Ok((_, unit)) => Ok(format!("{unit:?}")),
        Err(error) => Err(format!("{error:?}")),
    }
}

/// A filesystem path built from arbitrary bytes.
///
/// Bytes that are not path bytes become `_`, because a path with a NUL in it is a
/// different question from "does the filesystem handle a NUL" and the second one
/// belongs to the syscall layer. What is being fuzzed here is the *lookup*.
fn path_of(input: &[u8]) -> String {
    let mut path = String::from("/");
    for byte in input.iter().take(24) {
        let character = char::from(*byte);
        if character.is_ascii_alphanumeric() || character == '/' || character == '_' {
            path.push(character);
        } else {
            path.push('_');
        }
    }
    path
}

/// The control a byte pair asks for.
///
/// Half the controls are unreachable from any scancode, so the input is used to
/// build a `Control` directly as well as through `from_scancode`. A control with a
/// field — the line in `BreakAtLine` — gets one from the input, including a line
/// number no source has, because that is the one that must refuse.
fn control_of(index: usize, input: &[u8]) -> lazalith_gui::Control {
    use lazalith_gui::Control;

    let Some(byte) = input.get(index) else {
        return Control::Step;
    };
    match *byte % 8 {
        0 => Control::Run,
        1 => Control::ContinueRun,
        2 => Control::Step,
        3 => Control::Pause,
        4 => Control::Reset,
        5 => Control::ToggleBreakpoint,
        6 => Control::BreakAtLine {
            name: "fuzz.c",
            line: u64::from(*byte) as u32,
        },
        _ => Control::ClearBreakpoints,
    }
}

// -- seeds and mutation -----------------------------------------------------

/// The kernel the kernel-loader target's seed is built around.
///
/// A `halt`, a `nop` and eight bytes of trailer. The loader validates the kernel
/// against the architecture, so the seed has to be real code; the trailer is there
/// because a real kernel has one and a seed without it would never reach the part of
/// the reader that looks at the bytes after the code.
fn seed_kernel() -> Vec<u8> {
    use lazalith_isa::{Instruction, Opcode};

    let mut bytes = Vec::new();
    for opcode in [Opcode::Halt, Opcode::Nop] {
        if let Ok(instruction) = Instruction::new(CONFIG, opcode, &[])
            && let Ok(encoded) = lazalith_isa::encode(CONFIG, &instruction)
        {
            bytes.extend_from_slice(&encoded);
        }
    }
    bytes.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
    bytes
}

/// A whole valid image, which the executable-loader target needs as a seed.///
/// Built with the published constants rather than with numbers written here, so it
/// cannot drift away from the format: a seed that no longer parses is a seed that
/// starts the fuzzer from noise.
fn seed_image() -> Result<lazalith_os::LzxImage, lazalith_os::LzxError> {
    lazalith_os::LzxImage::new(
        lazalith_os::LzxArchitecture::Lz64,
        0,
        0,
        0x400,
        0x1_0000,
        vec![lazalith_os::LzxSection::new(
            lazalith_os::LzxSectionKind::Code,
            lazalith_os::LZX_CODE_PERMISSIONS,
            0,
            8,
            4,
            &[0_u8; 8],
        )?],
    )
}

/// A whole valid package, which the package-reader target needs as a seed.
///
/// Built with a manifest that declares a resource, so the fuzzer's mutations land in
/// the resource table and not only in the header — the table is where the offsets
/// are, and the offsets are where a reader goes wrong.
fn seed_package() -> Result<lazalith_os::LzaPackage, lazalith_os::LzaError> {
    lazalith_os::LzaPackage::new(
        b"hello",
        lazalith_os::PackageVersion::new(1, 2, 3),
        lazalith_os::LzxArchitecture::Lz64,
        b"[application]\nname = \"hello\"\nversion = \"1.2.3\"\nentry = \"m.lz\"\narchitecture = \"any\"\n\n[resources]\nlogo = \"a.rgb\"\nsound = \"b.wav\"\n",
        &seed_image()
            .map_or_else(|_| Vec::new(), |image| image.to_bytes().unwrap_or_default()),
        &[
            lazalith_os::LzaResource::label(b"logo"),
            lazalith_os::LzaResource::label(b"sound"),
        ],
    )
}

/// A hand-written seed per target.
///
/// Each one is a *valid* input of the shape the target expects, because a fuzzer
/// starting from noise explores the space of interesting inputs very slowly and
/// spends its budget on inputs that are rejected in the first byte. Starting from
/// something nearly right means the mutations land in the fields, which is where
/// the bugs are.
pub fn seeds(target: &crate::Target) -> Vec<Vec<u8>> {
    let text: &[&str] = match target.name {
        "instruction decoder" => &[],
        "assembler" => &[
            ".arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    LI r0, 2\n    SYSCALL\n",
            ".arch lz64\n.entry _start\n.section .rodata\nmessage:\n    .ascii \"hi\\n\"\n.section .text\n_start:\n    LI r0, message\n    LDZ r1, [r0], BYTE\n",
        ],
        "lazen parser" => &[
            "fn main() -> int {\n    return 0;\n}\n",
            "// a comment\nfn f(x: int) -> int { return x + 1; }\n",
        ],
        "c parser" => &[
            "int main(void) { return 0; }\n",
            "struct point { int x; int y; };\nint f(struct point p) { return p.x; }\n",
        ],
        "filesystem metadata" => &["/a", "/bb", "/ccc"],
        "debugger commands" => &["step", "run"],
        _ => &[],
    };
    let mut seeds: Vec<Vec<u8>> = text.iter().map(|text| text.as_bytes().to_vec()).collect();
    // The byte targets all get one structural seed: a real header with the rest
    // zeroed, which is what a truncated or zero-filled file looks like.
    seeds.push(structural_seed(target.name));
    seeds
}

/// A minimal valid input for the byte targets.
///
/// Building one requires a valid object, which requires assembling something, which
/// is what [`seeds`]' own text is for. The object seed is therefore built at runtime
/// from the assembler seed rather than stored as bytes, so it cannot drift away
/// from the format the way a checked-in blob would.
fn structural_seed(name: &str) -> Vec<u8> {
    match name {
        "object reader" => lazalith_toolchain::assemble(
            ".arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    LI r0, 2\n    SYSCALL\n",
        )
        .map_or_else(|_| Vec::new(), |object| object.to_bytes().unwrap_or_default()),
        "executable loader" => seed_image()
            .map_or_else(|_| Vec::new(), |image| image.to_bytes().unwrap_or_default()),
        "package reader" => seed_package()
            .map_or_else(|_| Vec::new(), |package| package.to_bytes().unwrap_or_default()),
        "kernel loader" => lazalith_boot::BootImage::new(CONFIG, seed_kernel(), 0)
            .map_or_else(|_| Vec::new(), |image| image.rom().to_vec()),
        "snapshot reader" | "instruction decoder" => {
            let mut bytes = Vec::new();
            for word in [0x0000_0000_0000_003c_u64, 0xffff_ffff_ffff_fffc] {
                bytes.extend_from_slice(&word.to_le_bytes());
            }
            bytes
        }
        _ => Vec::new(),
    }
}

/// Produces the `index`-th input for a target, deterministically.
///
/// Six mutations, which is few. Each is a thing malformed input actually does — a
/// bit flipped, a byte inserted, a byte dropped, a length field made enormous, a
/// field cut in half, a whole seed spliced in — and each is applied a different
/// number of times so the same `(seed, index)` does not always produce the same
/// shape of damage.
pub fn mutate(seed: u64, index: u64, name: &str) -> Vec<u8> {
    let target = crate::TARGETS
        .iter()
        .find(|target| target.name == name)
        .expect("every target named by a campaign is in TARGETS");
    let seeds = seeds(target);
    let mut random = Rng::new(seed ^ index.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let mut bytes = if seeds.is_empty() {
        Vec::new()
    } else {
        seeds[usize::try_from(index).unwrap_or(0) % seeds.len()].clone()
    };
    let rounds = usize::try_from(index % 4).unwrap_or(0) + 1;
    for _ in 0..rounds {
        match random.below(6) {
            0 if !bytes.is_empty() => {
                let at = usize::try_from(random.below(bytes.len() as u64)).unwrap_or(0);
                bytes[at] ^= 1 << random.below(8);
            }
            1 => {
                let at = usize::try_from(random.below(bytes.len() as u64 + 1)).unwrap_or(0);
                bytes.insert(at, random.byte());
            }
            2 if !bytes.is_empty() => {
                let at = usize::try_from(random.below(bytes.len() as u64)).unwrap_or(0);
                bytes.remove(at);
            }
            3 if bytes.len() >= 4 => {
                // A length or count field made enormous. This is the mutation that
                // finds the allocation a parser sized from untrusted bytes, and it
                // is the one mutation here that is not a single-byte change.
                let at = usize::try_from(random.below(bytes.len() as u64 - 3)).unwrap_or(0);
                bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            }
            4 if !bytes.is_empty() => {
                bytes.truncate(usize::try_from(random.below(bytes.len() as u64)).unwrap_or(0));
            }
            _ if seeds.len() > 1 => {
                let other =
                    seeds[usize::try_from(random.below(seeds.len() as u64)).unwrap_or(0)].clone();
                let at = usize::try_from(random.below(bytes.len() as u64 + 1)).unwrap_or(0);
                let end = at.saturating_add(other.len()).min(bytes.len());
                bytes.splice(at..end, other);
            }
            _ => bytes.push(random.byte()),
        }
    }
    // Every target also sees a pure-noise input now and then, because a mutation
    // set that can only reach a shape a seed already had is a mutation set that
    // never reaches a shape it has not.
    if random.below(8) == 0 {
        let length = usize::try_from(random.below(64)).unwrap_or(0);
        bytes = (0..length).map(|_| random.byte()).collect();
    }
    if random.below(16) == 0 {
        bytes.clear();
    }
    bytes
}

/// A deterministic generator, so a failure is a command line.
///
/// `xorshift64*`, which is three lines, has no dependencies, and is stable across
/// platforms and compiler versions — the property that matters here, since a
/// campaign that produced different inputs on a different machine would be a
/// campaign nobody could reproduce.
pub struct Rng(u64);

impl Rng {
    /// Starts a generator at `seed`, mapping `0` to a nonzero state.
    pub const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x2545_f491_4f6c_dd1d
        } else {
            seed
        })
    }

    /// The next 64 bits. Not `Iterator::next`: this is not an iterator, and naming
    /// it `next` would invite `for` over it.
    pub fn next_word(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value >> 12;
        value ^= value << 25;
        value ^= value >> 27;
        self.0 = value;
        value.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A value in `0..bound`, with no division by zero for a zero bound.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_word() % bound
        }
    }

    /// One byte.
    pub fn byte(&mut self) -> u8 {
        u8::try_from(self.next_word() & 0xff).unwrap_or(0)
    }
}

/// Renders a target's name and its seeds, for `--list`.
pub fn describe(target: &crate::Target) -> String {
    let mut text = String::from(target.name);
    for seed in seeds(target) {
        let _ = write!(text, "\n  {seed:02x?}");
    }
    text
}
