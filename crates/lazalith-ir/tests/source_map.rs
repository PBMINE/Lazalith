//! Source maps, at the layer that builds them.
//!
//! A source map is the bridge between a statement and the instructions it became,
//! and every later layer — code generation, the object file, the linker, the
//! debugger — trusts what this one records. So the tests here are about the two
//! things that layer has to get right: which statement a given instruction came
//! from, and that the table stays the size of the source rather than the size of
//! the program.
//!
//! Each test states the invariant it proves and would fail if the corresponding
//! check were removed.

use lazalith_ir::{ConstValue, FunctionBuilder, Instruction, Linkage, ModuleBuilder, Type};
use lazalith_types::{ByteOffset, SourceManager, SourceSpan};
use std::vec::Vec;

fn int_type() -> Type {
    Type::Int {
        bits: 32,
        signed: true,
    }
}

/// A file long enough that offsets on separate lines do not coincide.
const TEXT: &str =
    "fn main() -> i32 {\n    let a: i32 = 1i32;\n    let b: i32 = 2i32;\n    return a + b;\n}\n";

/// A span over line `line`, counting lines from one.
fn span_on(sources: &SourceManager, line: u32) -> SourceSpan {
    let start = (line as usize - 1) * 5;
    let end = start + 4;
    sources
        .source_span(
            lazalith_types::SourceId::new(0),
            ByteOffset::new(start as u32),
            ByteOffset::new(end as u32),
        )
        .expect("the span is inside the file")
}

/// A function with two marked statements of two instructions each.
///
/// The shape is deliberate: a statement is *not* one instruction, so a map that
/// only recorded the first instruction of a statement would resolve the rest of
/// it to whatever statement came before.
fn marked_function() -> (SourceManager, lazalith_ir::Function) {
    let mut sources = SourceManager::new();
    sources.add_file("main.lz", TEXT).expect("the file");
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    let mut values = Vec::new();
    for line in [2u32, 3u32] {
        function.mark(span_on(&sources, line));
        for _ in 0..2 {
            values.push(
                function
                    .emit(Instruction::Const {
                        value: ConstValue::Int(1),
                        ty: int_type(),
                    })
                    .expect("const"),
            );
        }
    }
    function
        .terminate(lazalith_ir::Terminator::Return(
            lazalith_ir::ReturnValue::Value(values[0]),
        ))
        .expect("terminator");
    let function = function.finish().expect("function");
    (sources, function)
}

/// An instruction resolves to the statement it came from, not to a neighbour.
#[test]
fn every_instruction_of_a_statement_names_that_statement() {
    let (sources, function) = marked_function();
    let first = span_on(&sources, 2);
    let second = span_on(&sources, 3);
    for instruction in 0..2 {
        assert_eq!(
            function.source_of(0, instruction),
            Some(&first),
            "instruction {instruction} came from line 2"
        );
    }
    for instruction in 2..4 {
        assert_eq!(
            function.source_of(0, instruction),
            Some(&second),
            "instruction {instruction} came from line 3"
        );
    }
}

/// A statement marked twice is one entry, because two would resolve the same way.
#[test]
fn a_statement_marked_twice_is_one_entry() {
    let mut sources = SourceManager::new();
    sources.add_file("main.lz", TEXT).expect("the file");
    let mut module = ModuleBuilder::new("test");
    let mut function: FunctionBuilder = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    for _ in 0..2 {
        function.mark(span_on(&sources, 2));
        function
            .emit(Instruction::Const {
                value: ConstValue::Int(1),
                ty: int_type(),
            })
            .expect("const");
    }
    function
        .terminate(lazalith_ir::Terminator::Return(
            lazalith_ir::ReturnValue::Value(lazalith_ir::ValueId::new(0).expect("value id")),
        ))
        .expect("terminator");
    let function = function.finish().expect("function");
    assert_eq!(
        function.source_map.len(),
        1,
        "a run of instructions from one span is one entry"
    );
}

/// The map is the size of the source, not of the program.
#[test]
fn the_map_has_one_entry_per_statement_not_per_instruction() {
    let (_, function) = marked_function();
    assert_eq!(
        function.blocks[0].instructions.len(),
        4,
        "the function has four instructions"
    );
    assert_eq!(
        function.source_map.len(),
        2,
        "and two statements, so two entries"
    );
}

/// An instruction in a later block resolves to that block's statement.
#[test]
fn a_second_block_has_its_own_mappings() {
    let mut sources = SourceManager::new();
    sources.add_file("main.lz", TEXT).expect("the file");
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    function.mark(span_on(&sources, 2));
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(lazalith_ir::Terminator::Jump(
            lazalith_ir::BlockId::new(1).expect("block id"),
        ))
        .expect("terminator");
    function.switch_to_block("then").expect("block");
    function.mark(span_on(&sources, 3));
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(2),
            ty: int_type(),
        })
        .expect("const");
    let value = lazalith_ir::ValueId::new(0).expect("value id");
    function
        .terminate(lazalith_ir::Terminator::Return(
            lazalith_ir::ReturnValue::Value(value),
        ))
        .expect("terminator");
    let function = function.finish().expect("function");
    assert_eq!(function.source_of(0, 0), Some(&span_on(&sources, 2)));
    assert_eq!(
        function.source_of(1, 0),
        Some(&span_on(&sources, 3)),
        "the second block's instruction names the second block's statement"
    );
}

/// An instruction before any mark has no source, and says so.
#[test]
fn an_unmarked_instruction_resolves_to_nothing() {
    let mut sources = SourceManager::new();
    sources.add_file("main.lz", TEXT).expect("the file");
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    function.mark(span_on(&sources, 2));
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(2),
            ty: int_type(),
        })
        .expect("const");
    let value = lazalith_ir::ValueId::new(0).expect("value id");
    function
        .terminate(lazalith_ir::Terminator::Return(
            lazalith_ir::ReturnValue::Value(value),
        ))
        .expect("terminator");
    let function = function.finish().expect("function");
    assert_eq!(
        function.source_of(0, 0),
        None,
        "the first instruction was pushed before anything marked it, and \
         guessing a line for it would be inventing a source"
    );
    assert_eq!(function.source_of(0, 1), Some(&span_on(&sources, 2)));
}
