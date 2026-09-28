//! A run that can be repeated, and a bug report that can be shipped.
//!
//! The step's claim is that four things reproduce a run, and every test here is one
//! of the four failing if it is true.

use lazalith_debug::{
    InputLog, InputLogError, MachineState, ReplayError, ReplaySession, StateError, StateRegion,
    Stop, Trace,
};
use lazalith_devices::input::{Event, EventKind, EventKindValue};
use lazalith_memory::RegionPermissions;
use lazalith_os::LzxImage;
use lazalith_types::{ArchitectureConfig, CycleCount};

/// A program that adds its two arguments and halts.
const SOURCE: &str = ".arch lz64\n\
.entry _start\n\
.global _start\n\
.section .text\n\
_start:\n\
    LI r0, 20\n\
    LI r1, 22\n\
    ADD r0, r0, r1\n\
    HALT\n";

/// A program that reads the input device.s injected count and halts with it in
/// r0.
///
/// `REGISTER_INJECTED` is at offset 24 of the device, and the device window starts
/// at 0x1000, so the read is from 0x1018. This is the program that makes the input
/// log *load-bearing*: the answer it puts in r0 is the log, and nothing else.
///
/// Deliberately not a polling loop. A loop would read the count a second later than
/// a straight-line read, which is a difference the test about arrival cycles would
/// then have to account for; three instructions keep the cycle at which an event
/// becomes visible the only moving part.
const COUNTER: &str = ".arch lz64\n\
.entry _start\n\
.global _start\n\
.section .text\n\
_start:\n\
    LI r1, 0x1018\n\
    LDZ r0, [r1], DOUBLE\n\
    HALT\n";

const CONFIG: ArchitectureConfig = ArchitectureConfig::lz64();
/// Where the code goes.
const CODE: u64 = 0;
/// How much of it.
const CODE_LENGTH: u64 = 0x400;
/// Where the data goes.
const DATA: u64 = 0x400;
/// Where the stack goes.
const STACK: u64 = 0x8000;
/// How long each writable region is.
const DATA_LENGTH: u64 = 0x400;

/// The image for a program, as the artifact's `binary`.
fn image(source: &str) -> Vec<u8> {
    lazalith_toolchain::assemble_and_link(source)
        .expect("the program links")
        .to_bytes()
        .expect("the image serialises")
}

/// A machine with a program's regions and nothing in them.
fn state() -> MachineState {
    let read_write = RegionPermissions {
        read: true,
        write: true,
        execute: false,
        user: true,
    };
    let read_execute = RegionPermissions {
        read: true,
        write: false,
        execute: true,
        user: true,
    };
    MachineState {
        config: CONFIG,
        registers: [0; 16],
        pc: lazalith_types::InstructionAddress::new(CODE),
        sp: lazalith_types::VirtualAddress::new(STACK),
        status: 0,
        halted: false,
        trap_vector: lazalith_types::InstructionAddress::new(0x2000),
        regions: vec![
            StateRegion {
                start: CODE,
                permissions: read_execute,
                bytes: vec![0; usize::try_from(CODE_LENGTH).unwrap_or(0)],
            },
            StateRegion {
                start: DATA,
                permissions: read_write,
                bytes: vec![0; usize::try_from(DATA_LENGTH).unwrap_or(0)],
            },
            StateRegion {
                start: STACK,
                permissions: read_write,
                bytes: vec![0; usize::try_from(DATA_LENGTH).unwrap_or(0)],
            },
        ],
    }
}

fn session(source: &str, log: &InputLog) -> ReplaySession {
    ReplaySession::new(&image(source), &state(), log).expect("the session starts")
}

fn key(code: u32) -> Event {
    Event {
        kind: EventKindValue::from(EventKind::KeyDown),
        code,
        x: 0,
        y: 0,
    }
}

// -- the four things ------------------------------------------------------

#[test]
fn the_same_four_things_produce_the_same_run() {
    let log = InputLog::new();
    let first = session(SOURCE, &log).run(100).expect("the run finishes");
    let second = session(SOURCE, &log).run(100).expect("the run finishes");
    assert_eq!(first, second, "two runs of the same four things differed");
    assert_eq!(first.stop, Stop::Halted);
    assert_eq!(first.registers[0], 42, "the program did not add");
}

#[test]
fn a_state_that_goes_through_bytes_replays_to_itself() {
    let original = state();
    let bytes = original.encode();
    let restored = MachineState::decode(&bytes).expect("the state reads back");
    assert_eq!(
        restored, original,
        "a state did not survive its own encoding"
    );

    let log = InputLog::new();
    let direct = session(SOURCE, &log).run(100).expect("the run finishes");
    let through_bytes = ReplaySession::new(&image(SOURCE), &restored, &log)
        .expect("the session starts")
        .run(100)
        .expect("the run finishes");
    assert_eq!(
        direct, through_bytes,
        "a run from a state that went through bytes differed"
    );
}

#[test]
fn a_log_that_goes_through_bytes_replays_to_itself() {
    let mut log = InputLog::new();
    for step in 0..8 {
        log.push(
            CycleCount::new(step * 3),
            key(0x30 + u32::try_from(step).unwrap_or(0)),
        )
        .expect("in order");
    }
    let bytes = log.encode();
    let restored = InputLog::decode(&bytes).expect("the log reads back");
    assert_eq!(restored, log, "a log did not survive its own encoding");

    let first = session(COUNTER, &log).run(200).expect("the run finishes");
    let second = session(COUNTER, &restored)
        .run(200)
        .expect("the run finishes");
    assert_eq!(first, second, "a run from a re-read log differed");
}

#[test]
fn the_input_log_is_what_the_program_sees() {
    // The same program, three logs, three different answers. Without this the log
    // could be decorative and every other test here would still pass.
    let empty = InputLog::new();
    let mut one = InputLog::new();
    one.push(CycleCount::new(0), key(0x41)).expect("in order");
    let mut five = InputLog::new();
    for _ in 0..5 {
        five.push(CycleCount::new(0), key(0x41)).expect("in order");
    }
    let count = |log: &InputLog| -> u64 {
        session(COUNTER, log)
            .run(500)
            .expect("the run finishes")
            .registers[0]
    };
    assert_eq!(count(&empty), 0, "a program with no log counted something");
    assert_eq!(count(&one), 1, "one event was not counted once");
    assert_eq!(count(&five), 5, "five events were not counted five times");
}

#[test]
fn an_event_arrives_on_the_cycle_it_was_recorded_on() {
    // The same five events with every arrival cycle moved past the end of the
    // program, so the program must see none of them. If arrival cycles were ignored
    // and only the order mattered — which is the obvious wrong implementation — this
    // would report five.
    let mut late = InputLog::new();
    for step in 0..5_u64 {
        late.push(CycleCount::new(100 + step), key(0x41))
            .expect("in order");
    }
    let seen = session(COUNTER, &late)
        .run(500)
        .expect("the run finishes")
        .registers[0];
    assert_eq!(
        seen, 0,
        "an event arrived before the cycle it was recorded on"
    );
}

#[test]
fn a_program_outlives_its_own_log_and_still_sees_what_arrived() {
    // The complement of the test above: a program that runs long enough *does* see
    // the events, so the first test is not passing because the log was ignored.
    let mut late = InputLog::new();
    for step in 0..5_u64 {
        late.push(CycleCount::new(100 + step), key(0x41))
            .expect("in order");
    }
    let run = session(COUNTER, &late).run(2000).expect("the run finishes");
    assert!(
        run.stop == Stop::Halted && run.steps <= 2000,
        "the run did not finish the way it should"
    );
    assert_eq!(run.steps, 3, "the program did not run three instructions");
    assert_eq!(
        run.time.as_u64(),
        3,
        "three instructions did not advance three cycles"
    );
}

// -- what makes the promise hard to keep ----------------------------------

#[test]
fn virtual_time_advances_by_the_step_and_not_by_the_clock() {
    let log = InputLog::new();
    let run = session(SOURCE, &log).run(100).expect("the run finishes");
    assert_eq!(
        run.time.as_u64(),
        run.steps,
        "virtual time moved by something other than one cycle per instruction"
    );
}

#[test]
fn a_log_whose_order_is_ambiguous_is_refused() {
    let mut log = InputLog::new();
    log.push(CycleCount::new(10), key(1)).expect("in order");
    assert_eq!(
        log.push(CycleCount::new(5), key(2)),
        Err(InputLogError::OutOfOrder { at: 5, after: 10 }),
        "a log accepted an event that arrived before the last one"
    );
    assert_eq!(log.len(), 1, "the refused event was kept anyway");
}

#[test]
fn a_different_binary_replays_differently() {
    let log = InputLog::new();
    let add = session(SOURCE, &log).run(100).expect("the run finishes");
    let other = session(COUNTER, &log).run(100).expect("the run finishes");
    assert_eq!(
        add.difference(&other).as_deref(),
        Some("the number of instructions"),
        "two different programs produced the same trace"
    );
}

#[test]
fn a_different_initial_state_replays_differently() {
    let log = InputLog::new();
    let mut changed = state();
    changed.registers[5] = 0xdead_beef;
    let plain = ReplaySession::new(&image(SOURCE), &state(), &log)
        .expect("the session starts")
        .run(100)
        .expect("the run finishes");
    let mutated = ReplaySession::new(&image(SOURCE), &changed, &log)
        .expect("the session starts")
        .run(100)
        .expect("the run finishes");
    assert_eq!(
        plain.difference(&mutated).as_deref(),
        Some("register r5"),
        "a changed register did not change the run"
    );
}

#[test]
fn the_step_limit_is_reported_rather_than_hidden() {
    let log = InputLog::new();
    let run = session(COUNTER, &log).run(2).expect("the run finishes");
    assert_eq!(run.stop, Stop::Limit, "a truncated run did not say so");
    assert_eq!(run.steps, 2);
}

// -- malformed artifacts --------------------------------------------------

#[test]
fn an_artifact_that_is_not_one_says_so() {
    assert_eq!(
        MachineState::decode(b"not a state"),
        Err(StateError::NotAState)
    );
    assert_eq!(MachineState::decode(&[]), Err(StateError::NotAState));
    assert_eq!(InputLog::decode(b"not a log"), Err(InputLogError::NotALog));
    let mut bytes = state().encode();
    bytes.push(0);
    assert!(matches!(
        MachineState::decode(&bytes),
        Err(StateError::Trailing { .. })
    ));
    let mut bytes = state().encode();
    bytes.truncate(bytes.len() - 4);
    assert!(matches!(
        MachineState::decode(&bytes),
        Err(StateError::Truncated { .. } | StateError::NotAState)
    ));
}

#[test]
fn a_log_that_claims_more_events_than_it_has_is_refused() {
    let log = InputLog::new();
    let mut bytes = log.encode();
    bytes[6] = 4;
    assert!(matches!(
        InputLog::decode(&bytes),
        Err(InputLogError::TooManyRecords { records: 4, .. })
    ));
}

#[test]
fn a_binary_that_is_not_an_image_says_so() {
    let error = ReplaySession::new(b"not an image", &state(), &InputLog::new())
        .expect_err("a binary that is not an image was accepted");
    assert!(matches!(error, ReplayError::Image(_)), "{error}");
}

#[test]
fn an_image_that_does_not_fit_the_state_says_so() {
    // A state with a code region of one word cannot hold a program. The failure has
    // to name the region rather than surfacing as a fault in the first step, or the
    // report would point at the program instead of at the artifact.
    let mut narrow = state();
    narrow.regions[0].bytes = vec![0; 8];
    let error = ReplaySession::new(&image(SOURCE), &narrow, &InputLog::new())
        .expect_err("a program that does not fit was accepted");
    assert!(matches!(error, ReplayError::Region { .. }), "{error}");
}

#[test]
fn an_image_is_readable_on_its_own_terms() {
    // The artifact's `binary` is a real image and stays one, so a report can carry
    // the image and the person reading it can look at it.
    let bytes = image(SOURCE);
    let loaded = LzxImage::from_bytes(&bytes).expect("the image reads back");
    assert_eq!(
        loaded.to_bytes().expect("it re-encodes"),
        bytes,
        "the image the artifact carries is not the image it was built from"
    );
}

// -- the trace is evidence, not a copy ------------------------------------

#[test]
fn a_trace_names_the_field_that_differs() {
    let log = InputLog::new();
    let first = session(SOURCE, &log).run(100).expect("the run finishes");
    assert_eq!(
        first.difference(&first),
        None,
        "a trace differed from itself"
    );
    let mut changed = first.clone();
    changed.time = CycleCount::new(first.time.as_u64() + 1);
    assert_eq!(
        changed.difference(&first).as_deref(),
        Some("the virtual time")
    );
    let mut changed = first.clone();
    changed.in_trap = !first.in_trap;
    assert_eq!(
        changed.difference(&first).as_deref(),
        Some("whether a trap frame was open")
    );
}

#[test]
fn two_runs_of_a_program_that_halts_agree_on_everything() {
    // The whole trace, spelled out, so a future field added to `Trace` cannot be
    // left out of the comparison by accident.
    let log = InputLog::new();
    let run: Trace = session(SOURCE, &log).run(100).expect("the run finishes");
    let again = session(SOURCE, &log).run(100).expect("the run finishes");
    assert_eq!(run.steps, again.steps);
    assert_eq!(run.stop, again.stop);
    assert_eq!(run.registers, again.registers);
    assert_eq!(run.pc, again.pc);
    assert_eq!(run.sp, again.sp);
    assert_eq!(run.status, again.status);
    assert_eq!(run.time, again.time);
    assert_eq!(run.delivered, again.delivered);
    assert_eq!(run.in_trap, again.in_trap);
}
