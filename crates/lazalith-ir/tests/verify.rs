//! Verifier tests.
//!
//! Each test states the invariant it proves and would fail if the corresponding
//! check were removed, which is the property that matters for a gate between a
//! frontend and code generation.

use lazalith_ir::{
    BinaryOp, Block, BlockId, CallArg, CallTarget, ConstValue, DataSegment, Function,
    FunctionBuilder, Instruction, IrErrorKind, Linkage, MemorySpace, Module, ModuleBuilder,
    Parameter, RecordField, ReturnValue, StoreWidth, Terminator, Type, ValueId, verify_module,
};
use std::{string::String, vec, vec::Vec};

fn int_type() -> Type {
    Type::Int {
        bits: 32,
        signed: true,
    }
}

fn parameter(name: &str) -> Parameter {
    Parameter {
        name: String::from(name),
        ty: int_type(),
    }
}

fn function_builder(module: &mut ModuleBuilder, result: Type) -> FunctionBuilder {
    module
        .function("main", Linkage::External, vec![parameter("argc")], result)
        .expect("function builder")
}

/// `main` that returns its parameter: the smallest well-formed function.
fn trivial_returning_function() -> Function {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, int_type());
    function.switch_to_block("entry").expect("block");
    let value = function.param_value(0).expect("parameter value");
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(7),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(Terminator::Return(ReturnValue::Value(value)))
        .expect("terminator");
    function.finish().expect("function")
}

#[test]
fn a_well_formed_module_verifies() {
    let function = trivial_returning_function();
    let mut module = ModuleBuilder::new("test");
    module.add_function(function).expect("add function");
    module
        .add_data(DataSegment {
            name: String::from("text"),
            bytes: vec![1, 2, 3],
            alignment: 4,
            span: None,
        })
        .expect("add data");
    let module = module.finish().expect("module verifies");
    assert_eq!(module.functions.len(), 1);
    assert_eq!(module.function("main").map(|f| f.blocks.len()), Some(1));
}

#[test]
fn value_identifiers_are_dense_and_function_local() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("block");
    let peeked = function.peek_next_value().expect("value");
    assert_eq!(peeked.get(), 1, "the parameter occupies value 0");
    let emitted = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    assert_eq!(emitted.get(), 1, "emitting consumes the next identifier");
    let second = function
        .emit(Instruction::Const {
            value: ConstValue::Int(2),
            ty: int_type(),
        })
        .expect("const");
    assert_eq!(second.get(), 2);
    assert_eq!(function.peek_next_value().expect("value").get(), 3);
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    function.finish().expect("function");
}

#[test]
fn an_undefined_operand_is_rejected() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("block");
    let missing = ValueId::new(99).expect("identifier");
    function
        .emit(Instruction::Unary {
            op: lazalith_ir::UnaryOp::Negate,
            operand: missing,
            ty: int_type(),
        })
        .expect("emit");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("undefined value");
    assert_eq!(
        error.kind,
        IrErrorKind::UndefinedValue { value: 99 },
        "removing the operand check must fail this test"
    );
    assert_eq!(error.function.as_deref(), Some("main"));
}

#[test]
fn an_undominated_use_is_rejected() {
    // main:
    //   entry: jmp then
    //   then:  r = const; jmp join
    //   join:  use r        <- r does not dominate this block
    //          ret
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, vec![], Type::Void)
        .expect("builder");
    let entry = function.switch_to_block("entry").expect("entry");
    // The entry block branches to both successors, so the join block is
    // reachable without passing through the block that defines the value.
    let condition = function
        .emit(Instruction::Const {
            value: ConstValue::Bool(true),
            ty: Type::Bool,
        })
        .expect("condition");
    function
        .terminate(Terminator::Branch {
            condition,
            then_block: BlockId::new(1).expect("then label"),
            otherwise: BlockId::new(2).expect("join label"),
        })
        .expect("terminator");

    let then_block = function.switch_to_block("then").expect("then");
    assert_eq!(then_block.get(), 1);
    let defined = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(Terminator::Unreachable)
        .expect("then end");

    function.switch_to_block("join").expect("join");
    function
        .emit(Instruction::Unary {
            op: lazalith_ir::UnaryOp::Negate,
            operand: defined,
            ty: int_type(),
        })
        .expect("use");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    assert_eq!(entry.get(), 0);
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("undominated use");
    assert!(
        matches!(error.kind, IrErrorKind::UndominatedUse { value, .. } if value == defined.get()),
        "removing dominance analysis must fail this test, got {:?}",
        error.kind
    );
}

#[test]
fn a_dominated_use_is_accepted() {
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, vec![], Type::Void)
        .expect("builder");
    function.switch_to_block("entry").expect("entry");
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(Terminator::Jump(BlockId::new(1).expect("join")))
        .expect("jump");
    function.switch_to_block("join").expect("join");
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(2),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let module = module.finish().expect("dominated use verifies");
    assert_eq!(module.function("main").expect("function").blocks.len(), 2);
}

#[test]
fn a_block_must_be_terminated_before_switching() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    let error = function
        .switch_to_block("other")
        .expect_err("must refuse an unterminated block");
    assert!(matches!(
        error.kind,
        IrErrorKind::InvalidBuilderState { .. }
    ));
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    function.switch_to_block("other").expect("now allowed");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    function.finish().expect("function");
}

#[test]
fn an_instruction_may_not_follow_a_terminator() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let error = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect_err("must refuse");
    assert!(matches!(
        error.kind,
        IrErrorKind::InvalidBuilderState { .. }
    ));
    function.finish().expect("function");
}

#[test]
fn a_function_must_terminate_every_block() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    function.switch_to_block("second").expect("second");
    let error = function.finish().expect_err("unterminated block");
    assert_eq!(error.kind, IrErrorKind::MissingTerminator { block: 1 });
}

#[test]
fn a_return_must_match_the_declared_result() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, int_type());
    function.switch_to_block("entry").expect("entry");
    function
        .terminate(Terminator::Return(ReturnValue::Void))
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("void return for int result");
    assert!(matches!(error.kind, IrErrorKind::InvalidType { .. }));

    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("other", Linkage::Local, vec![], Type::Void)
        .expect("builder");
    function.switch_to_block("entry").expect("entry");
    let value = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("const");
    function
        .terminate(Terminator::Return(ReturnValue::Value(value)))
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("value return for void result");
    assert!(matches!(error.kind, IrErrorKind::InvalidType { .. }));
}

#[test]
fn a_call_to_a_missing_function_is_rejected() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    function
        .emit(Instruction::Call {
            target: CallTarget::Function(String::from("absent")),
            args: Vec::new(),
            result: Type::Void,
        })
        .expect("call");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("unknown callee");
    assert_eq!(
        error.kind,
        IrErrorKind::UnknownCallee {
            name: String::from("absent")
        }
    );
}

#[test]
fn an_unresolved_import_is_rejected() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    function
        .emit(Instruction::Call {
            target: CallTarget::Imported(String::from("other::helper")),
            args: Vec::new(),
            result: Type::Void,
        })
        .expect("call");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("unresolved import");
    assert_eq!(
        error.kind,
        IrErrorKind::UnresolvedImport {
            name: String::from("other::helper")
        }
    );
}

#[test]
fn call_arity_and_argument_types_are_checked() {
    let mut module = ModuleBuilder::new("test");
    let mut callee = module
        .function(
            "add",
            Linkage::Local,
            vec![parameter("a"), parameter("b")],
            Type::Void,
        )
        .expect("builder");
    callee.switch_to_block("entry").expect("entry");
    callee
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let callee = callee.finish().expect("function");
    module.add_function(callee).expect("add");

    let mut caller = function_builder(&mut module, Type::Void);
    caller.switch_to_block("entry").expect("entry");
    let argument = caller.param_value(0).expect("value");
    caller
        .emit(Instruction::Call {
            target: CallTarget::Function(String::from("add")),
            args: vec![CallArg::Value(argument)],
            result: Type::Void,
        })
        .expect("call");
    caller
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let caller = caller.finish().expect("function");
    module.add_function(caller).expect("add");
    let error = module.finish().expect_err("arity");
    assert_eq!(
        error.kind,
        IrErrorKind::ArityMismatch {
            expected: 2,
            actual: 1
        }
    );

    let mut module = ModuleBuilder::new("test");
    let mut callee = module
        .function(
            "takes_bool",
            Linkage::Local,
            vec![Parameter {
                name: String::from("flag"),
                ty: Type::Bool,
            }],
            Type::Void,
        )
        .expect("builder");
    callee.switch_to_block("entry").expect("entry");
    callee
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let callee = callee.finish().expect("function");
    module.add_function(callee).expect("add");
    let mut caller = function_builder(&mut module, Type::Void);
    caller.switch_to_block("entry").expect("entry");
    let argument = caller.param_value(0).expect("value");
    caller
        .emit(Instruction::Call {
            target: CallTarget::Function(String::from("takes_bool")),
            args: vec![CallArg::Value(argument)],
            result: Type::Void,
        })
        .expect("call");
    caller
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let caller = caller.finish().expect("function");
    module.add_function(caller).expect("add");
    let error = module.finish().expect_err("argument type");
    assert!(matches!(
        error.kind,
        IrErrorKind::ArgumentTypeMismatch { index: 0, .. }
    ));
}

#[test]
fn a_store_width_must_match_the_stored_value() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    let address = function
        .emit(Instruction::Const {
            value: ConstValue::Pointer(0x1000),
            ty: Type::Pointer,
        })
        .expect("address");
    let value = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: int_type(),
        })
        .expect("value");
    function
        .emit_effect(Instruction::Store {
            address,
            value,
            width: StoreWidth::Double,
            space: lazalith_ir::MemorySpace::Program,
        })
        .expect("store");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("width mismatch");
    assert_eq!(error.kind, IrErrorKind::WidthMismatch { bytes: 8, size: 4 });
}

#[test]
fn duplicate_functions_blocks_and_data_are_rejected() {
    let mut module = ModuleBuilder::new("test");
    module
        .add_function(trivial_returning_function())
        .expect("first");
    let error = module
        .add_function(trivial_returning_function())
        .expect_err("duplicate function");
    assert!(matches!(error.kind, IrErrorKind::RedefinedFunction { .. }));

    let mut module = ModuleBuilder::new("test");
    module
        .add_data(DataSegment {
            name: String::from("dup"),
            bytes: Vec::new(),
            alignment: 1,
            span: None,
        })
        .expect("first");
    let error = module
        .add_data(DataSegment {
            name: String::from("dup"),
            bytes: Vec::new(),
            alignment: 1,
            span: None,
        })
        .expect_err("duplicate data");
    assert!(matches!(error.kind, IrErrorKind::RedefinedFunction { .. }));

    let function = Function {
        name: String::from("dup"),
        linkage: Linkage::Local,
        params: Vec::new(),
        result: Type::Void,
        blocks: vec![
            Block {
                id: BlockId::new(0).expect("block"),
                name: String::from("a"),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            },
            Block {
                id: BlockId::new(0).expect("block"),
                name: String::from("b"),
                instructions: Vec::new(),
                terminator: Terminator::Unreachable,
            },
        ],
        span: None,
    };
    let module = Module {
        name: String::from("test"),
        functions: vec![function],
        data: Vec::new(),
    };
    let error = verify_module(&module).expect_err("duplicate block");
    assert_eq!(error.kind, IrErrorKind::RedefinedBlock { block: 0 });
}

#[test]
fn type_sizes_and_alignments_are_computed_and_bounded() {
    assert_eq!(Type::Void.size_in_bytes(), None);
    assert_eq!(Type::Bool.size_in_bytes(), Some(1));
    assert_eq!(
        Type::Int {
            bits: 64,
            signed: false
        }
        .size_in_bytes(),
        Some(8)
    );
    assert_eq!(Type::Pointer.size_in_bytes(), Some(8));
    assert_eq!(
        Type::Slice {
            element: Box::new(Type::Bool),
            mutable: false
        }
        .size_in_bytes(),
        Some(16)
    );
    assert_eq!(
        Type::Record {
            fields: vec![
                RecordField {
                    name: String::from("a"),
                    ty: Type::Bool,
                },
                RecordField {
                    name: String::from("b"),
                    ty: int_type(),
                },
            ],
        }
        .size_in_bytes(),
        Some(5)
    );
    assert_eq!(
        Type::Enum {
            variants: vec![String::from("A"), String::from("B")]
        }
        .size_in_bytes(),
        Some(24)
    );
    assert_eq!(Type::Bool.alignment_in_bytes(), 1);
    assert_eq!(Type::Pointer.alignment_in_bytes(), 8);
    assert_eq!(int_type().alignment_in_bytes(), 4);
    assert!(
        Type::Record {
            fields: vec![RecordField {
                name: String::from("a"),
                ty: Type::Pointer,
            }],
        }
        .is_aggregate()
    );
    assert!(!Type::Pointer.is_aggregate());
}

#[test]
fn a_data_segment_alignment_must_be_a_power_of_two() {
    let mut module = ModuleBuilder::new("test");
    module
        .add_data(DataSegment {
            name: String::from("bad"),
            bytes: Vec::new(),
            alignment: 3,
            span: None,
        })
        .expect("add");
    let error = module.finish().expect_err("alignment");
    assert!(matches!(error.kind, IrErrorKind::InvalidType { .. }));
}

#[test]
fn a_branch_must_have_two_distinct_targets() {
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::Local, vec![], Type::Void)
        .expect("builder");
    function.switch_to_block("entry").expect("entry");
    let other = BlockId::new(1).expect("block");
    let condition = function
        .emit(Instruction::Const {
            value: ConstValue::Bool(true),
            ty: Type::Bool,
        })
        .expect("const");
    function
        .terminate(Terminator::Branch {
            condition,
            then_block: other,
            otherwise: other,
        })
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let error = module.finish().expect_err("degenerate branch");
    assert!(matches!(
        error.kind,
        IrErrorKind::InvalidBuilderState { .. }
    ));
}

#[test]
fn traps_and_stores_produce_no_value_while_other_instructions_do() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("entry");
    let address = function
        .emit(Instruction::Const {
            value: ConstValue::Pointer(0x1000),
            ty: Type::Pointer,
        })
        .expect("address");
    let byte = function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: Type::Bool,
        })
        .expect("value");
    function
        .emit_effect(Instruction::Store {
            address,
            value: byte,
            width: StoreWidth::Byte,
            space: lazalith_ir::MemorySpace::Program,
        })
        .expect("store");
    let next = function
        .emit(Instruction::Const {
            value: ConstValue::Int(2),
            ty: int_type(),
        })
        .expect("const");
    function
        .emit_effect(Instruction::Trap { code: 1 })
        .expect("trap");
    let after_trap = function
        .emit(Instruction::Binary {
            op: BinaryOp::Add,
            left: next,
            right: next,
            ty: int_type(),
        })
        .expect("add");
    function
        .terminate(Terminator::Unreachable)
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add");
    let module = module.finish().expect("verifies");
    let blocks = &module.function("main").expect("function").blocks;
    // address, byte, const, store, add, trap: the store and trap produce nothing
    assert_eq!(blocks[0].instructions.len(), 6);
    assert_eq!(
        after_trap.get(),
        4,
        "store and trap must not consume value identifiers"
    );
}

#[test]
fn a_copy_of_an_aggregate_keeps_the_aggregate_type() {
    // A copy used to be typed `Void`, so a copied slice's uses read as nothing
    // at all and its length could have been dropped. The verifier must see the
    // copy's own type, which is what makes a use of the result checkable.
    verify_module(&copying_slice_function()).expect("a copy of a slice verifies");
}

#[test]
fn a_copy_of_a_slice_can_be_stored() {
    // The store-width check reads the value's type, so a copy must report the
    // slice's size rather than nothing at all.
    let mut module = ModuleBuilder::new("store_copy");
    let mut function = module
        .function(
            "main",
            Linkage::External,
            vec![Parameter {
                name: String::from("s"),
                ty: slice_type(),
            }],
            Type::Void,
        )
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    let value = function.param_value(0).expect("parameter value");
    let address = function
        .emit(Instruction::Const {
            value: ConstValue::Pointer(0x1000),
            ty: Type::Pointer,
        })
        .expect("a constant");
    let copied = function
        .emit(Instruction::Copy {
            value,
            ty: slice_type(),
        })
        .expect("a copy produces a value");
    function
        .emit_effect(Instruction::Store {
            address,
            value: copied,
            // A view is stored as its pointer word, which is the one width the
            // verifier allows for a slice; the length word is stored separately.
            width: StoreWidth::Double,
            space: MemorySpace::Program,
        })
        .expect("a store");
    function
        .terminate(Terminator::Return(ReturnValue::Void))
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add function");
    let module = module.finish().expect("module");
    verify_module(&module).expect("a copied slice stores cleanly");
}

/// A slice type used by the copy tests.
fn slice_type() -> Type {
    Type::Slice {
        element: Box::new(Type::Int {
            bits: 8,
            signed: false,
        }),
        mutable: false,
    }
}

/// A module with one function that takes a slice and returns it copied.
fn copying_slice_function() -> Module {
    let mut module = ModuleBuilder::new("copy");
    let mut function = module
        .function(
            "main",
            Linkage::External,
            vec![Parameter {
                name: String::from("s"),
                ty: slice_type(),
            }],
            slice_type(),
        )
        .expect("function builder");
    function.switch_to_block("entry").expect("block");
    let value = function.param_value(0).expect("parameter value");
    let copied = function
        .emit(Instruction::Copy {
            value,
            ty: slice_type(),
        })
        .expect("a copy produces a value");
    function
        .terminate(Terminator::Return(ReturnValue::Value(copied)))
        .expect("terminator");
    let function = function.finish().expect("function");
    module.add_function(function).expect("add function");
    module.finish().expect("module")
}

/// A string's address is a symbol, and a symbol the module does not have is a
/// compile-time failure. Without this check a program that reads a string whose
/// bytes were never emitted would read whatever is at some address instead.
#[test]
fn a_data_address_must_name_a_segment_the_module_has() {
    let function = {
        let mut module = ModuleBuilder::new("test");
        let mut function = function_builder(&mut module, Type::Void);
        function.switch_to_block("entry").expect("block");
        function
            .emit(Instruction::DataAddress {
                name: String::from("text"),
                ty: Type::Pointer,
            })
            .expect("data address");
        function
            .terminate(Terminator::Return(ReturnValue::Void))
            .expect("terminator");
        function.finish().expect("function")
    };

    let mut with_segment = ModuleBuilder::new("test");
    with_segment.add_function(function.clone()).expect("add");
    with_segment
        .add_data(DataSegment {
            name: String::from("text"),
            bytes: vec![b'h', b'i'],
            alignment: 1,
            span: None,
        })
        .expect("segment");
    with_segment
        .finish()
        .expect("a segment that exists must verify");

    let mut without = ModuleBuilder::new("test");
    without.add_function(function).expect("add");
    let error = without
        .finish()
        .expect_err("a missing segment must be rejected");
    assert_eq!(
        error.kind,
        IrErrorKind::UnknownData {
            name: String::from("text")
        }
    );
}

#[test]
fn a_data_address_is_a_pointer() {
    let mut module = ModuleBuilder::new("test");
    let mut function = function_builder(&mut module, Type::Void);
    function.switch_to_block("entry").expect("block");
    function
        .emit(Instruction::DataAddress {
            name: String::from("text"),
            ty: int_type(),
        })
        .expect("data address");
    function
        .terminate(Terminator::Return(ReturnValue::Void))
        .expect("terminator");
    module
        .add_function(function.finish().expect("function"))
        .expect("add");
    module
        .add_data(DataSegment {
            name: String::from("text"),
            bytes: vec![b'h'],
            alignment: 1,
            span: None,
        })
        .expect("segment");
    let error = module
        .finish()
        .expect_err("a data address that is not a pointer must be rejected");
    assert!(
        matches!(error.kind, IrErrorKind::InvalidType { .. }),
        "{error}"
    );
}

/// A reserved block keeps the identifier it was given, and the blocks a function
/// ends up with are in the order they were filled in.
///
/// Both halves matter. A front end that has to name a block it has not built yet
/// gets a real identifier from `reserve_block` rather than predicting one, and
/// the finished function still lists its blocks in emission order — which is what
/// numbers a function's values, because identifiers are dense and follow the
/// blocks.
#[test]
fn a_reserved_block_keeps_its_identifier_and_its_place_in_the_emission_order() {
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("entry block");
    // Reserve the loop's blocks before writing the body, the way a front end
    // lowering `while` does, and write the body's own block in between.
    let test = function.reserve_block("test").expect("reserve test");
    let body = function.reserve_block("body").expect("reserve body");
    let exit = function.reserve_block("exit").expect("reserve exit");
    assert_ne!(test, body, "each reservation is its own block");
    assert_ne!(body, exit, "each reservation is its own block");

    function
        .emit(Instruction::Const {
            value: ConstValue::Int(0),
            ty: int_type(),
        })
        .expect("a constant");
    function
        .terminate(Terminator::Jump(test))
        .expect("jump to the test");
    function.switch_to_block("test").expect("fill the test");
    let condition = function
        .emit(Instruction::Const {
            value: ConstValue::Bool(true),
            ty: Type::Bool,
        })
        .expect("a condition");
    function
        .terminate(Terminator::Branch {
            condition,
            then_block: body,
            otherwise: exit,
        })
        .expect("branch");
    function.switch_to_block("body").expect("fill the body");
    function
        .terminate(Terminator::Jump(exit))
        .expect("leave the body");
    function.switch_to_block("exit").expect("fill the exit");
    let result = function
        .emit(Instruction::Const {
            value: ConstValue::Int(7),
            ty: int_type(),
        })
        .expect("a result");
    function
        .terminate(Terminator::Return(ReturnValue::Value(result)))
        .expect("return");

    let function = function.finish().expect("the function is complete");
    let names: Vec<&str> = function
        .blocks
        .iter()
        .map(|block| block.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["entry", "test", "body", "exit"],
        "the blocks are in the order they were filled in, which is the order \
         their values are numbered in"
    );
    // Values are numbered in the order the blocks were *filled in*, not the order
    // they were reserved: the entry block's `i32` is first even though `exit` was
    // reserved before the entry block had a value in it.
    let types: Vec<Option<Type>> = (0..3)
        .map(|raw| {
            let value = ValueId::new(raw).expect("a value");
            lazalith_ir::value_type(&function, value)
        })
        .collect();
    assert_eq!(
        types,
        vec![Some(int_type()), Some(Type::Bool), Some(int_type()),],
        "one value per value-producing instruction, in block order"
    );
}

/// A block that is reserved and never filled in is reported rather than dropped.
///
/// A branch to a block that does not exist is not a link error the program can
/// survive, so leaving it out of the finished function would turn a front end's
/// mistake into a jump to nothing.
#[test]
fn a_reserved_block_that_is_never_filled_in_is_reported() {
    let mut module = ModuleBuilder::new("test");
    let mut function = module
        .function("main", Linkage::External, Vec::new(), int_type())
        .expect("function builder");
    function.switch_to_block("entry").expect("entry block");
    function.reserve_block("never").expect("reserve a block");
    let error = function.finish().expect_err("the function is incomplete");
    assert!(
        error.to_string().contains("never"),
        "the message names the block that was left out: {error}"
    );
}
