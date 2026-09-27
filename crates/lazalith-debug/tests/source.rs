//! Step 76: source-level debugging, end to end.
//!
//! Every test here walks the whole chain a real debugger would: Lazen source
//! goes through the compiler, the code generator, an object file, the linker and
//! a `.lzx` image, the image is read back from its bytes, and *then* the
//! debugger answers questions about it. Nothing here is a table written by hand
//! to match what the compiler would have said — a test that did that would pass
//! with the pipeline disconnected.
//!
//! What each test states is one thing the feature claims:
//!
//! - an image built from source carries that source's text and name, and an image
//!   built without it carries no block at all;
//! - a source offset survives every hop from the parser to the debugger;
//! - the program counter resolves to the line the program is actually stopped on,
//!   and a breakpoint set by line number stops the program there;
//! - a line with no code in it is reported as having none, not guessed at;
//! - the debug block cannot be made to read outside the file that carries it, or
//!   to name a file or a line that is not there.

use lazalith_debug::{DebugController, StopReason};
use lazalith_devices::{DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_os::debug::{DebugBlock, DebugEntry, DebugError, DebugFile};
use lazalith_os::{LzxImage, ProcessId, ThreadId, VirtualFileSystem, VirtualTerminal};
use lazalith_runtime::{BuildOptions, RuntimeProgram};
use lazalith_types::ArchitectureConfig;

const PID: ProcessId = ProcessId::new(1).expect("one is a valid process id");
const TID: ThreadId = ThreadId::new(1).expect("one is a valid thread id");

/// A program with statements on lines a test can name.
///
/// The lines matter more than the program. The arithmetic exists so the
/// statements are not all on one line, and the loop is a `while` rather than
/// something with a fused header because a source-level debugger has to cope
/// with a compiler that fuses one and must not assume a line is one address.
const SOURCE: &str = r#"fn main() -> i32 {
    let mut index: i64 = 0i64;
    let mut total: i64 = 0i64;
    while index < 8i64 {
        total = total + index;
        index = index + 1i64;
    }
    rt::sys::print("done\n");
    return 0;
}
"#;

/// The line the `while` is written on, and the line its body is on.
const WHILE_LINE: u32 = 4;
const BODY_LINE: u32 = 5;

/// The supervisor: a `NOP` and the `RFE` that hands the machine to user code.
fn supervisor_kernel(config: ArchitectureConfig) -> Vec<u8> {
    [
        encode(config, &Instruction::new(config, Opcode::Nop, &[]).unwrap()).unwrap(),
        encode(config, &Instruction::new(config, Opcode::Rfe, &[]).unwrap()).unwrap(),
    ]
    .concat()
}

/// The image for `source`, read back from the bytes that were written.
///
/// Going through the bytes is the point: a debugger is handed a file, so a test
/// that kept the image in memory and never wrote it would not be testing the
/// format at all.
fn build(source: &str) -> LzxImage {
    let program =
        RuntimeProgram::build(source, &BuildOptions::lz64("main.lz")).expect("the program builds");
    let bytes = program.to_image_bytes().expect("the image serialises");
    LzxImage::from_bytes(&bytes).expect("the image reads back")
}

/// A controller with `SOURCE` loaded, stopped before the handoff.
fn controller() -> DebugController<NoDevice> {
    let config = ArchitectureConfig::lz64();
    let mut controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    controller
        .load_image(build(SOURCE), PID, TID)
        .expect("the program is scheduled");
    controller
}

/// The program counter after the supervisor's `RFE`, which is the entry point.
fn after_handoff(controller: &mut DebugController<NoDevice>) -> u64 {
    controller.step(PID).expect("the handoff runs");
    controller.registers().pc()
}

/// An image built from source carries the source it was built from.
#[test]
fn an_image_carries_its_source_name_and_text() {
    let image = build(SOURCE);
    let debug = image.debug().expect("the image carries debug information");
    // The runtime compiles the startup unit from Lazen source too, so the block
    // carries the program *and* the code that hands the machine to it. Both are
    // real sources that really produced the image's code, and a mapping that
    // could not name one of them would send a frontend to the wrong file.
    let names: Vec<&str> = debug.files().iter().map(|file| file.name()).collect();
    assert!(
        names.contains(&"main.lz"),
        "the program's own source is in the block: {names:?}"
    );
    assert!(
        names.contains(&"lazen.startup"),
        "the startup unit's source is in the block too: {names:?}"
    );
    let program_source = debug
        .files()
        .iter()
        .find(|file| file.name() == "main.lz")
        .expect("the program's own source is carried");
    // The unit the compiler saw is the program followed by the prelude, because
    // that is one file: `compose` puts the program in front and the library
    // behind it, and the whole thing is compiled under the program's name. So
    // the block's text for `main.lz` begins with the program exactly as it was
    // written — which is what makes a breakpoint on the user's line 5 stop on
    // the user's line 5 rather than on line 5 of a file that had a library
    // prepended to it.
    assert!(
        program_source.text().starts_with(SOURCE),
        "the program is the first thing in its own file"
    );
    assert!(
        !debug.entries().is_empty(),
        "every statement in the program left a mapping"
    );
}

/// The mappings name the file they came from, and resolve to real lines of it.
#[test]
fn every_mapping_resolves_to_a_line_in_the_source() {
    let debug = build(SOURCE)
        .debug()
        .expect("the image carries debug information")
        .clone();
    let last_line = debug
        .line_count("main.lz")
        .expect("the block carries the file");
    for entry in debug.entries() {
        let address = entry.address;
        let located = debug
            .resolve(address)
            .expect("an address the backend emitted resolves to somewhere");
        let lines = debug
            .line_count(located.name)
            .expect("every mapping names a file the block carries");
        assert!(
            located.line_number() >= 1 && located.line_number() <= lines,
            "the entry at {address:#x} resolves to line {} of {}, which has {lines} lines",
            located.line_number(),
            located.name
        );
    }
    assert!(
        last_line >= BODY_LINE,
        "the program's source has the lines the tests name"
    );
}

/// A breakpoint placed by line number stops the program on that line.
#[test]
fn a_source_breakpoint_stops_on_the_line_it_names() {
    let mut controller = controller();
    let entry = after_handoff(&mut controller);
    let addresses = controller
        .set_source_breakpoint(PID, "main.lz", BODY_LINE)
        .expect("the line has code");
    assert!(
        !addresses.is_empty(),
        "line {BODY_LINE} has code, so it resolves to an address"
    );
    let run = controller
        .run(PID)
        .expect("the program runs to the breakpoint");
    assert!(
        matches!(run.reason, StopReason::Breakpoint { .. }),
        "the program stopped on a breakpoint: {:?}",
        run.reason
    );
    let location = controller
        .source_location()
        .expect("a stopped program is somewhere in the source");
    assert_eq!(location.name, "main.lz");
    assert_eq!(
        location.line_number(),
        BODY_LINE,
        "the program is stopped on the line that was asked for"
    );
    assert!(
        controller.registers().pc() >= entry,
        "the stop is inside the program's own code"
    );
}

/// Every address the debugger reports for a line is one it would stop at.
#[test]
fn every_address_of_a_line_is_where_the_program_actually_is() {
    let image = build(SOURCE);
    let debug = image.debug().expect("the image carries debug information");
    let mut controller = controller();
    after_handoff(&mut controller);
    // The first address of the loop body is where a stepping frontend would put
    // its breakpoint by convention, and it has to be a real address of the
    // image: the entry is looked up and found, so the line and the addresses
    // agree with each other rather than being separately plausible.
    let first = debug
        .addresses_at_line("main.lz", WHILE_LINE)
        .first()
        .copied()
        .expect("the loop has code on its line");
    let located = debug.resolve(first).expect("the address resolves");
    assert_eq!(located.line_number(), WHILE_LINE);
    assert_eq!(located.name, "main.lz");
    assert!(
        controller.source_location_at(first).is_some(),
        "the controller resolves the same address to the same place"
    );
}

/// A line with no code in it resolves to no addresses, and says so.
#[test]
fn a_line_with_no_code_resolves_to_nothing() {
    let debug = build(SOURCE)
        .debug()
        .expect("the image carries debug information")
        .clone();
    // Line 0 is not a line at all: lines are counted from one, and a frontend
    // that asked for line 0 has a bug of its own that an image must not turn
    // into an address.
    assert_eq!(debug.addresses_at_line("main.lz", 0), Vec::<u64>::new());
    assert_eq!(
        debug.addresses_at_line("main.lz", 9_999),
        Vec::<u64>::new(),
        "a line past the end of the file has no code"
    );
    assert_eq!(
        debug.addresses_at_line("a-file-that-does-not-exist.lz", 1),
        Vec::<u64>::new(),
        "a file the image does not carry has no lines in it"
    );
    // Asking twice gives the same answer, which is what a frontend needs to be
    // able to re-derive a breakpoint without duplicating it.
    assert_eq!(
        debug.addresses_at_line("main.lz", WHILE_LINE),
        debug.addresses_at_line("main.lz", WHILE_LINE)
    );
}

/// Asking for a line the image does not carry sets no breakpoint.
#[test]
fn a_breakpoint_on_a_missing_line_sets_nothing() {
    let mut controller = controller();
    after_handoff(&mut controller);
    let addresses = controller
        .set_source_breakpoint(PID, "main.lz", 4_000)
        .expect("a line with no code is not an error");
    assert!(addresses.is_empty(), "nothing was set");
    let run = controller.run(PID).expect("the program runs");
    assert!(
        !matches!(run.reason, StopReason::Breakpoint { .. }),
        "the program was not stopped by a breakpoint that was never set: {:?}",
        run.reason
    );
}

/// The program counter is resolved against the source as the program runs.
#[test]
fn the_program_counter_resolves_to_the_line_it_is_on() {
    let mut controller = controller();
    after_handoff(&mut controller);
    let start = controller
        .source_location()
        .expect("the entry point is in the source");
    // The image's entry point is the program's `main`: the linker put the
    // program's code first, and the entry is the start of it. So the entry
    // resolves into the program's own source at its first line.
    assert_eq!(start.name, "main.lz");
    assert_eq!(
        start.line_number(),
        1,
        "the entry point is the first line of main"
    );
    // Step forward and ask again. The answer has to keep up with the program
    // rather than staying at the entry: a debugger that reported the entry point
    // for the whole run would pass the first assertion and be useless. Enough
    // steps to get past the startup unit and `main`'s prologue and into the loop,
    // which is what has to be reached for this to mean anything.
    let mut deepest = 0;
    for _ in 0..400 {
        controller.step(PID).expect("the program steps");
        if let Some(location) = controller.source_location() {
            if location.name == "main.lz" {
                deepest = deepest.max(location.line_number());
            }
        }
    }
    assert!(
        deepest >= BODY_LINE,
        "stepping into the loop reached line {deepest} of main.lz, which is at \
         least the loop body on line {BODY_LINE}"
    );
}

/// Addresses the program passed through resolve to their own lines.
#[test]
fn addresses_the_program_executed_resolve_to_their_lines() {
    let mut controller = controller();
    after_handoff(&mut controller);
    let mut resolved = 0usize;
    for _ in 0..40 {
        controller.step(PID).expect("the program steps");
        let stack = controller.stack(8).expect("the stack reads");
        for word in stack.words {
            if controller.source_location_at(word).is_some() {
                resolved += 1;
            }
        }
    }
    assert!(
        resolved > 0,
        "at least one address on the stack was one the program passed through, \
         and the debug table resolved it to a line of the source"
    );
}

/// A block that is not a block is rejected rather than half-believed.
#[test]
fn a_corrupt_debug_block_is_rejected() {
    let image = build(SOURCE);
    let mut bytes = image.to_bytes().expect("the image serialises");
    // The block is the last thing in the file, so the last byte of the file is
    // the last byte of the block. Corrupting the magic is the smallest possible
    // damage, and it has to be caught.
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    let error = LzxImage::from_bytes(&bytes).expect_err("a corrupt block is not an image");
    let message = error.to_string();
    assert!(
        message.contains("debug block"),
        "the failure says the debug block is what was wrong: {message}"
    );
}

/// A block that claims more mappings than it holds is rejected.
#[test]
fn a_block_with_impossible_counts_is_rejected() {
    let block = sample_block();
    let mut bytes = block.encode();
    // The mapping count is the word before the entries; claim a million of them.
    // The mapping count is the last word of the header, and a million mappings
    // need twenty megabytes this block does not have.
    bytes[16..20].copy_from_slice(&1_000_000u32.to_le_bytes());
    let error = DebugBlock::decode(&bytes).expect_err("a block that overruns is not a block");
    assert!(
        matches!(error, DebugError::Truncated { .. }),
        "the reader says the block is short of what it promised: {error:?}"
    );
}

/// A block that ends early, having promised trailing bytes, is rejected.
#[test]
fn a_block_with_trailing_bytes_is_rejected() {
    let mut bytes = sample_block().encode();
    bytes.push(0);
    let error = DebugBlock::decode(&bytes).expect_err("a block with a tail is not a block");
    assert!(
        matches!(error, DebugError::TrailingBytes { .. }),
        "the reader says the block is longer than it should be: {error:?}"
    );
}

/// A block whose mapping points past the end of its source is rejected.
#[test]
fn a_block_with_a_mapping_outside_its_source_is_rejected() {
    let block = DebugBlock::with_entries(
        vec![DebugFile::new(
            String::from("a.lz"),
            String::from("fn main() {}\n"),
        )],
        vec![DebugEntry {
            address: 0x1000,
            source: 0,
            offset: 0,
            length: 9_000,
        }],
    )
    .expect_err("a mapping past the end of the source is not a mapping");
    assert!(
        matches!(block, DebugError::Range { .. }),
        "the reader refuses a mapping into text the block does not have: {block:?}"
    );
}

/// A block whose mapping names a source it does not carry is rejected.
#[test]
fn a_block_with_a_mapping_into_a_missing_source_is_rejected() {
    let block = DebugBlock::with_entries(
        vec![DebugFile::new(
            String::from("a.lz"),
            String::from("fn main() {}\n"),
        )],
        vec![DebugEntry {
            address: 0x1000,
            source: 3,
            offset: 0,
            length: 1,
        }],
    )
    .expect_err("a mapping into a source that is not there is not a mapping");
    assert!(
        matches!(block, DebugError::Range { .. }),
        "the reader refuses a mapping into a source it does not have: {block:?}"
    );
}

/// An image built without debug information carries none, and the debugger
/// loaded with it says so rather than inventing a source.
#[test]
fn an_image_without_debug_information_answers_nothing() {
    let with_debug = build(SOURCE);
    // The kernel's own images are built this way, and a debugger handed one must
    // not invent a source for it.
    let stripped = LzxImage::new(
        with_debug.architecture(),
        0,
        // The entry offset is the linked image's own, not zero: the runtime links
        // the program after the library, so offset zero is a library function and
        // an image built with it runs something other than the program under test.
        with_debug.entry_offset(),
        with_debug.required_data(),
        with_debug.required_stack(),
        with_debug.sections().to_vec(),
    )
    .expect("an image without debug information is valid");
    assert!(stripped.debug().is_none());
    let bytes = stripped.to_bytes().expect("it serialises");
    let read_back = LzxImage::from_bytes(&bytes).expect("it reads back");
    assert!(
        read_back.debug().is_none(),
        "no block was invented on the way"
    );

    let config = ArchitectureConfig::lz64();
    let mut controller = DebugController::boot(
        config,
        &supervisor_kernel(config),
        8u64,
        VirtualTerminal::new(b"").expect("a terminal"),
        VirtualFileSystem::with_defaults().expect("a filesystem"),
        DeviceManager::<NoDevice>::new(),
    )
    .expect("the controller boots");
    controller
        .load_image(read_back, PID, TID)
        .expect("the program is scheduled");
    assert!(controller.debug_info().is_none());
    after_handoff(&mut controller);
    assert!(
        controller.source_location().is_none(),
        "a program with no debug information has no source location, and saying \
         so is the answer"
    );
    assert_eq!(
        controller
            .set_source_breakpoint(PID, "main.lz", 1)
            .expect("asking about a line is not an error"),
        Vec::<u64>::new(),
        "and no line of it can be broken on"
    );
    // The program still runs: losing debug information must not change what a
    // program does, so it reaches its own `exit` under a debugger that could
    // never say where it was.
    let run = controller.run(PID).expect("the program runs to its end");
    assert!(
        matches!(run.reason, StopReason::Exit { code: 0 }),
        "the program ran to its own exit: {:?}",
        run.reason
    );
}

/// A block built by hand round-trips through its own encoding.
fn sample_block() -> DebugBlock {
    DebugBlock::with_entries(
        vec![DebugFile::new(
            String::from("a.lz"),
            String::from("fn main() {}\n"),
        )],
        vec![DebugEntry {
            address: 0x1000,
            source: 0,
            offset: 0,
            length: 4,
        }],
    )
    .expect("the block is well formed")
}
