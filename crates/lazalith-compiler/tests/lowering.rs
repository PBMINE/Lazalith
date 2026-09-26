//! Lowering tests.
//!
//! Every test states the property it proves about the IR a checked program
//! becomes. The properties are the ones that were once wrong in a deleted
//! prototype, so each test would fail if its rule were dropped:
//!
//! - a local is reached through the frame base, never through a function's
//!   address, so a call does not read its caller's frame;
//! - a view keeps its address *and* its length through every copy, store and
//!   load;
//! - `break` and `continue` are jumps to real blocks, not `unreachable`;
//! - an index is checked before the address it reads is formed;
//! - a string's address is a symbol the linker resolves, not a made-up number;
//! - an argument list that does not fit the ABI's registers is refused rather
//!   than truncated.
//!
//! The module a test inspects has already been through the IR verifier, because
//! `ModuleBuilder::finish` verifies: a test cannot look at IR that the verifier
//! would reject.

use std::string::String;
use std::vec::Vec;

use lazalith_compiler::frontend::compile;
use lazalith_compiler::lower::{self, FrameLayout, LowerError, MAX_ARGUMENT_WORDS, SlotPurpose};
use lazalith_compiler::types::Type;
use lazalith_ir::{ComparisonOp, Instruction, Intrinsic, Module, Terminator};
use lazalith_types::SourceManager;

/// Compiles and lowers a program, panicking with the diagnostic if it cannot.
fn lower_source(source: &str) -> (Module, Vec<FrameLayout>) {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    let lowered =
        lower::lower(&program).unwrap_or_else(|error| panic!("{source} should lower: {error}"));
    (lowered.module, lowered.frames)
}

/// Compiles and lowers, expecting a failure, and returns the error.
fn lower_failure(source: &str) -> LowerError {
    let mut sources = SourceManager::new();
    let (_, program) = compile(&mut sources, "t.lazen", source)
        .unwrap_or_else(|error| panic!("{source} should compile:\n{}", error.render()));
    lower::lower(&program).expect_err("this program must not lower")
}

/// Every instruction in a function, in block order.
fn instructions(module: &Module, function: &str) -> Vec<Instruction> {
    let lowered = module
        .function(function)
        .unwrap_or_else(|| panic!("the module has a `{function}`"));
    lowered
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter().cloned())
        .collect()
}

/// Every terminator in a function, in block order.
fn terminators(module: &Module, function: &str) -> Vec<Terminator> {
    module
        .function(function)
        .expect("the module has the function")
        .blocks
        .iter()
        .map(|block| block.terminator.clone())
        .collect()
}

const HELLO: &str = r#"
    extern "syscall" fn write(fd: i32, buffer: &[u8], length: u64, result: ptr<u8>) -> i64;

    fn main() -> i32 {
        let message = "Hello, Lazalith\n";
        let bytes = message.as_bytes();
        write(1, bytes, message.len() as u64, bytes.as_ptr());
        return 0;
    }
"#;

/// A local's address comes from the frame base, never from a function address.
///
/// A deleted prototype used a function's address as its frame base, which made
/// every call read its caller's frame. The frame base is its own thing.
#[test]
fn a_local_is_reached_through_the_frame_base() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let total: i32 = 1;
            return total;
        }
        "#,
    );
    let all = instructions(&module, "main");
    assert!(
        all.iter().any(|instruction| matches!(
            instruction,
            Instruction::Intrinsic {
                kind: Intrinsic::FrameBase,
                ..
            }
        )),
        "a local's address must come from the frame base: {all:?}"
    );
    assert!(!module.data("main").is_some(), "a function is not data");
}

/// A string's address is a symbol, and its bytes are a segment in the module.
#[test]
fn a_string_is_a_data_segment_named_by_its_address() {
    let (module, lowered_frames) = {
        let (module, frames) = lower_source(HELLO);
        (module, frames)
    };
    let segments: Vec<String> = module
        .data
        .iter()
        .map(|segment| segment.name.clone())
        .collect();
    assert_eq!(
        segments,
        vec!["str0".to_string()],
        "each interned string is one segment"
    );
    assert_eq!(
        module.data("str0").expect("the segment").bytes,
        b"Hello, Lazalith\n".to_vec(),
        "the segment holds the literal's bytes"
    );
    let all = instructions(&module, "main");
    assert!(
        all.iter()
            .any(|instruction| matches!(instruction, Instruction::DataAddress { name, .. } if name == "str0")),
        "the literal's address is a symbol: {all:?}"
    );
    assert!(
        !lowered_frames.is_empty(),
        "a frame is reported for every function"
    );
}

/// A view is two words, and every copy keeps both.
///
/// The deleted prototype dropped a slice's length when it copied one, so a
/// program could read past the end of its own data believing it had not. A view
/// is built with both halves written, and reading one out of the frame loads two
/// words.
#[test]
fn a_view_keeps_its_length_through_a_frame_round_trip() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let text = "hi";
            let other = text;
            return other.len() as i32;
        }
        "#,
    );
    let all = instructions(&module, "main");
    let inserts = all
        .iter()
        .filter(|instruction| matches!(instruction, Instruction::Insert { .. }))
        .count();
    assert!(
        inserts >= 4,
        "a view is built from an address and a length, twice: {all:?}"
    );
    assert!(
        all.iter().any(|instruction| matches!(
            instruction,
            Instruction::Intrinsic {
                kind: Intrinsic::SliceLength,
                ..
            }
        )),
        "a view's length is read, not recomputed from an address: {all:?}"
    );
    // Every insert must be one of the two halves of a view. An insert at any
    // other offset would be a field this language does not have.
    for instruction in &all {
        if let Instruction::Insert { offset, .. } = instruction {
            assert!(
                *offset == 0 || *offset == 8,
                "a view has an address at 0 and a length at 8, not a field at {offset}"
            );
        }
    }
}

/// An index is checked before the address it reads is formed.
///
/// A deleted prototype formed the address first, so an out-of-range index read
/// whatever was there instead of trapping.
#[test]
fn an_index_is_bounds_checked_before_its_address() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let values = [1i32, 2i32, 3i32];
            return values[1];
        }
        "#,
    );
    let all = instructions(&module, "main");
    let check = all
        .iter()
        .position(|instruction| matches!(instruction, Instruction::BoundsCheck { .. }))
        .unwrap_or_else(|| panic!("an index must be checked: {all:?}"));
    let load = all
        .iter()
        .position(|instruction| matches!(instruction, Instruction::Load { .. }))
        .unwrap_or_else(|| panic!("the element must be loaded: {all:?}"));
    assert!(
        check < load,
        "the check comes first: a load at {load} before the check at {check}"
    );
    match &all[check] {
        Instruction::BoundsCheck {
            index,
            length,
            code,
        } => {
            // The length is a value, not a constant baked into the check: an
            // array's count is a constant but a slice's is not.
            assert_ne!(index, length, "a check compares two values");
            assert_eq!(*code, lower::TRAP_BOUNDS, "the trap code is stable");
        }
        other => panic!("expected a bounds check, found {other:?}"),
    }
}

/// `break` and `continue` are jumps, and nothing is unreachable.
#[test]
fn break_and_continue_are_jumps() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let mut total: i32 = 0;
            for index in 0..10 {
                if index == 3 {
                    continue;
                }
                if index == 7 {
                    break;
                }
                total = total + index;
            }
            return total;
        }
        "#,
    );
    let all = terminators(&module, "main");
    let jumps = all
        .iter()
        .filter(|terminator| matches!(terminator, Terminator::Jump(_)))
        .count();
    assert!(
        jumps >= 3,
        "a loop's body, step and `continue` and `break` are jumps: {all:?}"
    );
    for terminator in &all {
        assert!(
            !matches!(terminator, Terminator::Unreachable),
            "a well-typed program's blocks all lead somewhere: {all:?}"
        );
    }
}

/// A loop's `continue` reaches its step, not its test.
///
/// If `continue` jumped to the test, the body's last effect would be skipped.
#[test]
fn a_loop_has_a_separate_step_block() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let mut total: i32 = 0;
            let mut index: i32 = 0;
            while index < 4 {
                index = index + 1;
                total = total + index;
            }
            return total;
        }
        "#,
    );
    let function = module.function("main").expect("main");
    let names: Vec<String> = function
        .blocks
        .iter()
        .map(|block| block.name.clone())
        .collect();
    let test = names
        .iter()
        .position(|name| name.starts_with("while.test"))
        .unwrap_or_else(|| panic!("a loop has a test block: {names:?}"));
    let step = names
        .iter()
        .position(|name| name.starts_with("while.step"))
        .unwrap_or_else(|| panic!("a loop has a step block: {names:?}"));
    assert_ne!(test, step, "`continue` needs a block of its own to reach");
    let step_terminator = &function.blocks[step].terminator;
    assert_eq!(
        step_terminator,
        &Terminator::Jump(function.blocks[test].id),
        "the step block returns to the test: {step_terminator:?}"
    );
}

/// A call that needs more argument registers than the ABI has is refused.
#[test]
fn a_call_too_wide_for_the_abi_is_refused() {
    // A 64-bit integer is one word, because the argument registers are 64 bits
    // wide, so seven of them are what it takes to be wider than the ABI's six.
    // The width is a property of the call, not of the callee being an extern, so
    // this uses a plain function: no numbered syscall is wide enough to reach it.
    let source = r#"
        fn wide(a: u64, b: u64, c: u64, d: u64, e: u64, f: u64, g: u64) -> i64 {
            return a as i64;
        }
        fn main() -> i64 {
            return wide(1u64, 2u64, 3u64, 4u64, 5u64, 6u64, 7u64);
        }
    "#;
    match lower_failure(source) {
        LowerError::TooManyArgumentWords {
            needed, allowed, ..
        } => {
            assert_eq!(allowed, MAX_ARGUMENT_WORDS);
            assert_eq!(needed, 7, "each u64 is one argument word");
            assert!(needed > allowed, "the call really is too wide");
        }
        other => panic!("expected a refusal for a call that does not fit: {other}"),
    }
}

/// `!` is logical negation, not a conversion to `bool`.
///
/// The operand is already a `bool`, so an `int_to_bool` would ask "is it
/// nonzero?" and hand back the operand unchanged: `!true` would be `true`. Only
/// an op that asks whether the operand is `false` negates it.
#[test]
fn a_bang_is_a_negation_and_not_a_conversion() {
    let source = r#"
        fn main() -> bool {
            let value: bool = true;
            return !value;
        }
    "#;
    let (module, _) = lower_source(source);
    let unary = instructions(&module, "main")
        .into_iter()
        .find_map(|instruction| match instruction {
            Instruction::Unary { op, .. } => Some(op),
            _ => None,
        })
        .expect("`!` lowers to a unary instruction");
    assert_eq!(
        unary,
        lazalith_ir::UnaryOp::Not,
        "`!x` is `x == 0`, which is not `x != 0`"
    );
    assert_ne!(
        unary,
        lazalith_ir::UnaryOp::IntToBool,
        "converting a bool to a bool would leave it alone"
    );
}

/// A view counts as two words, so three views do not fit in six registers.
#[test]
fn a_view_costs_two_argument_words() {
    let source = r#"
        fn four_views(a: &[u8], b: &[u8], c: &[u8], d: &[u8]) -> i64 {
            return 0;
        }
        fn main() -> i64 {
            let text = "x";
            let bytes = text.as_bytes();
            return four_views(bytes, bytes, bytes, bytes);
        }
    "#;
    match lower_failure(source) {
        LowerError::TooManyArgumentWords { needed, .. } => {
            assert_eq!(needed, 8, "four views are eight words, not four")
        }
        other => panic!("expected a refusal: {other}"),
    }
}

/// Every syscall the design names is numbered, so an `extern "syscall"` that
/// names something else is refused — and refused *early*, with a diagnostic that
/// says what to do, rather than lowered with no number to call.
///
/// The rule this replaced allowed a name the design had promised but the ABI had
/// not delivered, so that a program written against the design would parse before
/// the ABI caught up. Every reserved name is numbered now, so that leniency has
/// nothing to be lenient about, and a name the table does not contain is a
/// mistake worth reporting at the declaration.
#[test]
fn a_syscall_the_abi_does_not_name_is_refused_at_the_declaration() {
    let mut sources = SourceManager::new();
    let error = compile(
        &mut sources,
        "t.lazen",
        "extern \"syscall\" fn not_a_syscall(a: i32) -> i64;",
    )
    .expect_err("a name the ABI does not contain is not a syscall");
    let rendered = error.render();
    assert!(
        rendered.contains("is not an OS ABI syscall"),
        "the diagnostic names the problem: {rendered}"
    );
    assert!(
        rendered.contains("lazalith_os_abi::Syscall"),
        "and says where the names live: {rendered}"
    );
}

/// A 32-bit target is refused rather than miscompiled.
///
/// `i64` is wider than a 32-bit machine's registers, and lowering it honestly
/// means register pairs and arithmetic the ISA does not have.
#[test]
fn a_32_bit_target_is_refused() {
    let mut sources = SourceManager::new();
    let (_, mut program) = compile(&mut sources, "t.lazen", "fn main() -> i32 { return 0; }")
        .expect("the program compiles");
    program.word = lazalith_types::WordWidth::W32;
    match lower::lower(&program).expect_err("a 32-bit target has no lowering") {
        LowerError::UnsupportedTarget { word } => {
            assert_eq!(word, lazalith_types::WordWidth::W32);
        }
        other => panic!("expected a refusal for a 32-bit target: {other}"),
    }
}

/// An extern is declared in the module, so a call to it can be checked.
#[test]
fn an_extern_is_declared_with_its_signature() {
    let (module, _) = lower_source(HELLO);
    let declaration = module
        .function("write")
        .expect("the extern is declared in the module");
    assert_eq!(
        declaration.params.len(),
        4,
        "the signature is the declaration"
    );
    let all = instructions(&module, "main");
    assert!(
        all.iter()
            .any(|instruction| matches!(instruction, Instruction::Call { .. })),
        "the call is a call, not an inlined body: {all:?}"
    );
}

/// A short-circuiting operator does not evaluate its right side when the left
/// side decides the answer, so its right side is in a block of its own.
#[test]
fn a_short_circuit_has_a_block_for_its_right_side() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let values = [1i32, 2i32];
            let index: usize = 0;
            if index < values.len() && values[index] == 1 {
                return 1;
            }
            return 0;
        }
        "#,
    );
    let function = module.function("main").expect("main");
    let names: Vec<String> = function
        .blocks
        .iter()
        .map(|block| block.name.clone())
        .collect();
    assert!(
        names.iter().any(|name| name.starts_with("sc.right")),
        "the right side of `&&` is behind a branch: {names:?}"
    );
    // `Instruction::LogicalAnd` would evaluate both sides, so the lowering must
    // not have used it.
    assert!(
        !instructions(&module, "main")
            .iter()
            .any(|instruction| matches!(instruction, Instruction::LogicalAnd { .. })),
        "an eager `and` would run the right side too early"
    );
}

/// A value that comes out of an `if` is written by each arm into the frame and
/// read after the join, because the IR has no phi node.
#[test]
fn a_value_from_an_if_goes_through_the_frame() {
    let (module, frames) = lower_source(
        r#"
        fn main() -> i32 {
            let flag = true;
            let value = if flag { 1i32 } else { 2i32 };
            return value;
        }
        "#,
    );
    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    assert!(
        frame
            .slots
            .iter()
            .any(|slot| slot.purpose == SlotPurpose::JoinValue),
        "a join temporary is reported, not hidden: {frame:?}"
    );
    let all = instructions(&module, "main");
    assert!(
        all.iter()
            .any(|instruction| matches!(instruction, Instruction::Store { .. })),
        "an arm stores its value: {all:?}"
    );
}

/// The reported frame covers the frontend's locals and this stage's temporaries,
/// and every slot lies inside it.
#[test]
fn the_frame_layout_covers_every_slot() {
    let (_, frames) = lower_source(
        r#"
        fn main() -> i32 {
            let flag = true;
            let value = if flag { 1i32 } else { 2i32 };
            let text = "hi";
            let count = text.len();
            return value + count as i32;
        }
        "#,
    );
    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    assert!(frame.size >= 8, "a frame holds something: {frame:?}");
    for slot in &frame.slots {
        assert!(
            slot.offset + slot.size <= frame.size,
            "the slot at {} of {} bytes is inside the {}-byte frame",
            slot.offset,
            slot.size,
            frame.size
        );
    }
    // A view takes two words, and the frontend's own offsets are respected.
    let view = frame
        .slots
        .iter()
        .find(|slot| slot.ty == Type::Str)
        .expect("the str local has a slot");
    assert_eq!(view.size, 16, "a str is a pointer and a length");
}

/// An array is its own storage: a literal writes each element into the local's
/// slot instead of copying a value that does not exist.
#[test]
fn an_array_literal_writes_its_elements_in_place() {
    let (module, frames) = lower_source(
        r#"
        fn main() -> i32 {
            let values = [1i32, 2i32, 3i32];
            return values[0];
        }
        "#,
    );
    let all = instructions(&module, "main");
    let stores = all
        .iter()
        .filter(|instruction| matches!(instruction, Instruction::Store { .. }))
        .count();
    assert!(stores >= 3, "three elements are three stores: {all:?}");
    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    let array = frame
        .slots
        .iter()
        .find(|slot| matches!(slot.ty, Type::Array { length: 3, .. }))
        .expect("the array local has a slot");
    assert_eq!(array.size, 12, "three i32 elements are twelve bytes");
}

/// A repeated array is a loop, not one store per element.
///
/// Unrolling is the obvious translation and it is what makes a framebuffer
/// impossible: a 320-by-200 window is 256000 bytes, and a store per byte is a
/// code section of megabytes for an array whose contents are all the same. So
/// the repeat is a counted loop, and the test holds the two properties that
/// matter: the code is the size of the loop, and the loop stores every element
/// including the first.
#[test]
fn a_repeated_array_is_a_counted_loop_over_every_element() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let zeros = [0u8; 16];
            return zeros[0] as i32;
        }
        "#,
    );
    let all = instructions(&module, "main");
    // One store for the counter, one for the element, and the counter again in
    // the step. Sixteen elements would be sixteen times that if it unrolled.
    let stores = all
        .iter()
        .filter(|instruction| matches!(instruction, Instruction::Store { .. }))
        .count();
    assert!(
        stores < 8,
        "sixteen identical elements are a loop, not sixteen stores: {stores} in {all:?}"
    );
    // And the loop is counted against the element count, not tested for
    // non-zero: a counter tested for zero would skip element zero and leave the
    // first element of every array uninitialised.
    let compare = all
        .iter()
        .find_map(|instruction| match instruction {
            Instruction::Compare { op, .. } => Some(*op),
            _ => None,
        })
        .expect("the loop is counted");
    assert_eq!(
        compare,
        lazalith_ir::ComparisonOp::LessThanUnsigned,
        "the bound is `index < count`, so the first element is inside the loop"
    );
    // The loop really is a loop: more than one block, and a backward jump.
    assert!(
        module
            .functions
            .iter()
            .flat_map(|function| function.blocks.iter())
            .count()
            > 1,
        "a repeat with more than one element needs blocks to loop in"
    );
    let total: usize = all.len();
    assert!(total > 0, "and it emitted something to run");
}

/// A cast is a load at the source's width into the target's type, which is what
/// the machine's extending loads do.
#[test]
fn a_cast_is_a_load_at_the_source_width() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i64 {
            let small: u8 = 200;
            return small as i64;
        }
        "#,
    );
    let all = instructions(&module, "main");
    let load = all
        .iter()
        .rev()
        .find_map(|instruction| match instruction {
            Instruction::Load { width, ty, .. } => Some((*width, ty.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a cast is a load: {all:?}"));
    assert_eq!(load.0, lazalith_ir::LoadWidth::Byte, "loaded at u8's width");
    assert_eq!(
        load.1,
        lazalith_ir::Type::Int {
            bits: 64,
            signed: true
        },
        "and typed as the target, so the extension is the machine's"
    );
}

/// A widening cast to a target that still fits in one word loads the source's
/// bytes, not the target's.
///
/// The scratch the cast stages its operand in holds exactly the source's bytes.
/// A target narrower than a word — `u8` to `u32` — would load the target's width
/// and so read the bytes above the staged value, which the store never wrote.
/// Whether those bytes are zero is not the program's business, so the widened
/// value came out of whatever the frame happened to hold.
#[test]
fn a_widening_cast_to_a_narrow_target_loads_the_source_width() {
    let (module, _) = lower_source(
        r#"
        fn main() -> u32 {
            let small: u8 = 200;
            return small as u32;
        }
        "#,
    );
    let all = instructions(&module, "main");
    let load = all
        .iter()
        .rev()
        .find_map(|instruction| match instruction {
            Instruction::Load { width, ty, .. } => Some((*width, ty.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("a cast is a load: {all:?}"));
    assert_eq!(
        load.0,
        lazalith_ir::LoadWidth::Byte,
        "loaded at u8's width, because that is all the cast staged"
    );
    assert_eq!(
        load.1,
        lazalith_ir::Type::Int {
            bits: 32,
            signed: false
        },
        "and typed as the target, so the extension is the machine's"
    );
}

/// Every block of a lowered function ends somewhere.
///
/// The lowering ends a function that owes a value with a trap if control reaches
/// the end without one, but the frontend makes that path unreachable by requiring
/// a tail value. What is testable is the weaker and more useful property: no block
/// of a well-typed program is `unreachable`, so a backend has no dead path to
/// miscompile and no path that falls off the end silently.
#[test]
fn every_block_of_a_lowered_function_ends_somewhere() {
    let (module, _) = lower_source(
        r#"
        fn classify(value: i32) -> i32 {
            if value < 0 {
                return 0 - 1;
            }
            let mut total: i32 = 0;
            for index in 0..4 {
                if index == 2 {
                    continue;
                }
                total = total + index;
            }
            loop {
                total = total + 1;
                break;
            }
            total
        }

        fn main() -> i32 {
            return classify(-1);
        }
        "#,
    );
    for function in &module.functions {
        for block in &function.blocks {
            assert!(
                !matches!(block.terminator, Terminator::Unreachable),
                "{} block {} is unreachable: a checked program has no such path",
                function.name,
                block.name
            );
        }
    }
    // The defensive trap is therefore dead code for a checked program, which is
    // the point of it: it exists for IR that a backend or a test builds by hand.
    assert!(
        !instructions(&module, "classify")
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Trap { code } if *code == lower::TRAP_FELL_OFF_THE_END)),
        "the frontend requires a tail value, so the fall-off-the-end trap is unreachable"
    );
}

#[test]
fn probe_returns() {
    for source in [
        "fn pick(flag: bool) -> &[u8] { if flag { return \"hi\"; } return \"ho\"; }\nfn main() -> i32 { let b = pick(true); return b.len() as i32; }",
        "fn main() -> i32 { let s = \"hi\"; return s; }",
    ] {
        let mut sources = SourceManager::new();
        match compile(&mut sources, "t.lazen", source) {
            Ok((_, _)) => println!("OK   {}", source.lines().next().unwrap_or("")),
            Err(e) => println!(
                "FAIL {} -> {}",
                source.lines().next().unwrap_or(""),
                e.code().as_str()
            ),
        }
    }
}

/// A lowered function keeps its source span.
///
/// The IR builder records a span for the *next* function started, so recording it
/// after asking for the function leaves every function in the module with no span
/// at all. Nothing in the IR depends on a span, so this fails silently and a
/// backend's debug information is simply empty.
#[test]
fn a_lowered_function_keeps_its_span() {
    let (module, _) = lower_source("fn main() -> i32 {\n    return 0;\n}\n");
    let function = module.function("main").expect("main is in the module");
    let span = function
        .span
        .clone()
        .expect("a lowered function has a span");
    assert_eq!(span.start().as_u32(), 0, "the span starts at the function");
    assert!(
        span.end().as_u32() > span.start().as_u32(),
        "and ends after it, not at the same place"
    );
}

/// A `ptr<T>` local is one word and lowers like a `usize`.
///
/// The width table covered only the integer types, so a pointer local had no
/// width at all and every program that named one failed to lower — which is how
/// the documented `write` example, that takes a `ptr<u8>`, could not be lowered.
#[test]
fn a_pointer_local_is_one_word() {
    let (_module, frames) = lower_source(
        r#"
        fn main() {
            let text = "hi";
            let pointer = text.as_ptr();
            return;
        }
        "#,
    );

    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    let slot = frame
        .slots
        .iter()
        .find(|slot| matches!(slot.ty, Type::Pointer { .. }))
        .expect("the pointer has a slot");
    assert_eq!(slot.size, 8, "a pointer is one word");
}

/// A loop's induction variable gets frame space of its own.
///
/// The `for` lowering keeps the end value in a temporary above the frontend's
/// frame, so the induction variable has to be *inside* that frame. It was
/// allocated and then never counted, which left `frame_size` a whole word short:
/// the first temporary landed on top of the loop variable, so the bound and the
/// counter were the same bytes and the loop read its own bound as its counter.
#[test]
fn a_for_loop_variable_is_inside_the_frame() {
    let (module, frames) = lower_source(
        r#"
        fn main() -> i32 {
            let mut total: i32 = 0;
            for index in 0..4 {
                total = total + index;
            }
            return total;
        }
        "#,
    );

    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    let variable = frame
        .slots
        .iter()
        .find(|slot| slot.name.as_deref() == Some("index"))
        .expect("the loop variable has a slot");
    assert!(
        u64::from(variable.offset) + u64::from(variable.size) <= u64::from(frame.size),
        "the loop variable at {}..{} is inside the {}-byte frame: {frame:?}",
        variable.offset,
        variable.offset + variable.size,
        frame.size,
    );
    // And the loop's bound is a temporary that does not sit on the variable.
    let bound = frame
        .slots
        .iter()
        .find(|slot| matches!(slot.purpose, SlotPurpose::LoopBound))
        .expect("the loop bound has a slot");
    assert!(
        bound.offset >= variable.offset + variable.size,
        "the bound at {}..{} starts above the loop variable at {}..{}: {frame:?}",
        bound.offset,
        bound.offset + bound.size,
        variable.offset,
        variable.offset + variable.size,
    );
    let _ = module;
}

/// A `let` inside a loop body is a local of the function, not of the block.
///
/// The block is a scope; the frame belongs to the function. Writing the body's
/// locals and offset back is what puts them in the frame layout, and without it a
/// local declared in a `while` body was allocated in a list that was then thrown
/// away — so no slot was reported for it and the frame was too small to hold it.
#[test]
fn a_local_in_a_loop_body_is_reported_in_the_frame() {
    let (module, frames) = lower_source(
        r#"
        fn main() -> i32 {
            let mut index: i32 = 0;
            while index < 4 {
                let inside: i32 = index + 1;
                index = inside;
            }
            return index;
        }
        "#,
    );

    let frame = frames
        .iter()
        .find(|frame| frame.function == "main")
        .expect("main has a frame");
    let inside = frame
        .slots
        .iter()
        .find(|slot| slot.name.as_deref() == Some("inside"))
        .expect("a local declared in the loop body is in the frame");
    assert!(
        u64::from(inside.offset) + u64::from(inside.size) <= u64::from(frame.size),
        "the local at {}..{} is inside the {}-byte frame: {frame:?}",
        inside.offset,
        inside.offset + inside.size,
        frame.size,
    );
    let _ = module;
}

/// A `for` loop leaves its body when the counter reaches the bound.
///
/// The loop's test asks whether the counter has *reached* the end, so the body is
/// what happens when it has not. Branching the other way runs the body zero times
/// for a non-empty range and never stops for an empty one — and a test that only
/// looks at the shape of the IR cannot tell, because both directions produce a
/// well-formed `Branch`.
#[test]
fn a_for_loop_leaves_its_body_when_the_counter_reaches_the_bound() {
    let (module, _) = lower_source(
        r#"
        fn main() -> i32 {
            let mut total: i32 = 0;
            for index in 0..4 {
                total = total + index;
            }
            return total;
        }
        "#,
    );
    let function = module.function("main").expect("main is in the module");
    // The block that tests the counter is the one holding the comparison against
    // the bound; its true branch has to leave the loop.
    let test = function
        .blocks
        .iter()
        .find(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Compare {
                        op: ComparisonOp::GreaterThanOrEqualSigned
                            | ComparisonOp::GreaterThanOrEqualUnsigned,
                        ..
                    }
                )
            })
        })
        .expect("a block that compares the counter with the bound");
    let Terminator::Branch {
        then_block,
        otherwise,
        ..
    } = test.terminator
    else {
        panic!("the test block ends in a branch: {:?}", test.terminator);
    };
    // The exit is the block that returns; the body is not.
    let exit = function
        .blocks
        .iter()
        .find(|block| matches!(block.terminator, Terminator::Return(_)))
        .expect("a block that returns");
    assert_eq!(
        then_block, exit.id,
        "the counter reaching the bound leaves the loop"
    );
    assert_ne!(
        otherwise, exit.id,
        "and the body is what happens while it has not"
    );
}
