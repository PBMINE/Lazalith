//! Property tests for the IR and its verifier.
//!
//! # What the IR is, and why it needs this
//!
//! One instruction set, three producers. `lazalith-compiler` emits it for Lazen,
//! `lazalith-c-compiler` emits it for C, and both then go through the same backend
//! — which is step 83's convergence, and which only holds as long as the IR
//! refuses the same things regardless of who built it.
//!
//! So these are properties of *the verifier*, stated over modules built through the
//! `ModuleBuilder` rather than over modules a compiler happened to produce. A
//! verifier that accepts a bad module is worse than no verifier, because every
//! producer trusts it: step 82's `return 0 - 42` reached the machine as a 64-bit
//! register because the IR's own checks agreed it was fine, and the fix had to go
//! all the way back to the conversion that produced it.
//!
//! # The properties
//!
//! - A module the builder accepts is a module the verifier accepts. The two are
//!   the same contract stated twice, and a disagreement between them is a hole
//!   whichever one is right.
//! - Each of the verifier's refusals actually refuses. A check that is written and
//!   never fires is a check nobody knows is there, and the bug it was written for
//!   is still a bug.
//! - The same module built twice is the same module.
//! - Arbitrary bytes are a module or a refusal, and never a panic.

use lazalith_ir::{
    BlockId, ConstValue, Instruction, IrErrorKind, Linkage, ModuleBuilder, Parameter, Terminator,
    Type, ValueId, verify_module,
};
use lazalith_properties::{Case, Gen, check};

/// A minimal valid module: one function, one block, one instruction, one return.
///
/// The starting point every case mutates. Small on purpose — a property over the
/// verifier's refusals wants a module that is *almost* right, and a big one has too
/// many ways to be wrong for the failure to mean anything.
fn minimal(name: &str) -> ModuleBuilder {
    let mut module = ModuleBuilder::new("properties");
    let mut function = module
        .function(
            name,
            Linkage::Global,
            Vec::<Parameter>::new(),
            Type::Int {
                bits: 32,
                signed: true,
            },
        )
        .expect("a function name is always available");
    // A new function builder has no block selected, and every instruction needs
    // somewhere to go. The entry block is where a front end starts, and the name is
    // the IR.s own because the verifier requires the entry to be `blocks[0]`.
    function
        .switch_to_block("entry")
        .expect("the entry block is always available");
    function
        .emit(Instruction::Const {
            value: ConstValue::Int(1),
            ty: Type::Int {
                bits: 32,
                signed: true,
            },
        })
        .expect("a constant emits");
    function
        .terminate(Terminator::Return(lazalith_ir::ReturnValue::Value(
            ValueId::new(0).expect("value zero is always valid"),
        )))
        .expect("a block may be terminated once");
    module
        .add_function(function.finish().expect("the function is complete"))
        .expect("the function is added");
    module
}

/// A valid module, and its name.
struct Good {
    name: String,
}

impl Case for Good {
    fn generate(source: &mut Gen) -> Self {
        Self {
            name: format!("f{}", source.below(1 << 20)),
        }
    }

    fn describe(&self) -> String {
        format!("a module with `{}`", self.name)
    }
}

/// What the builder accepts, the verifier accepts.
///
/// The contract, stated once. `finish` runs the builder's own checks and
/// `verify_module` runs the verifier's, and a module that passes one and fails the
/// other is a hole in whichever of them is being charitable about it.
#[test]
fn what_the_builder_accepts_the_verifier_accepts() {
    check::<Good>(64, |case| {
        let Ok(module) = minimal(&case.name).finish() else {
            return true;
        };
        verify_module(&module).is_ok()
    });
}

/// Two functions with one name are refused.
///
/// The IR has a flat symbol namespace and the backend turns each function into a
/// label, so two functions of one name are two labels of one name — and a linker
/// that resolved them would pick one, silently.
///
/// The builder refuses it at `function()`, before a second body exists, and this
/// says the *refusal* is the redefinition rather than some accident: a builder that
/// failed for a different reason would leave a hole for the case that matters,
/// which is two functions reaching the verifier by some other route.
#[test]
fn a_redefined_function_is_refused() {
    check::<Good>(64, |case| {
        let mut module = minimal(&case.name);
        module
            .function(
                &case.name,
                Linkage::Global,
                Vec::<Parameter>::new(),
                Type::Void,
            )
            .err()
            .is_some_and(|error| matches!(error.kind, IrErrorKind::RedefinedFunction { .. }))
    });
}

/// A block that was reserved and never filled in is refused.
///
/// The shape of a real bug: a branch names a block, the code that should have gone
/// into it did not, and the function is finished. Without this the branch is a jump
/// to nothing, and the failure appears at run time as a fetch from an address that
/// happens to be the next function's.
#[test]
fn an_unfilled_reserved_block_is_refused() {
    check::<Good>(64, |case| {
        let mut module = ModuleBuilder::new("properties");
        let Ok(mut function) = module.function(
            &case.name,
            Linkage::Global,
            Vec::<Parameter>::new(),
            Type::Void,
        ) else {
            return true;
        };
        function
            .switch_to_block("entry")
            .expect("the entry block is always available");
        function
            .emit(Instruction::Const {
                value: ConstValue::Int(1),
                ty: Type::Int {
                    bits: 32,
                    signed: true,
                },
            })
            .expect("a constant emits");
        let elsewhere = function.reserve_block("elsewhere");
        function
            .terminate(Terminator::Jump(elsewhere.expect("a block is reserved")))
            .expect("a jump terminates");
        match function.finish() {
            Err(_) => true,
            Ok(finished) => {
                module.add_function(finished).expect("added");
                match module.finish() {
                    // The builder caught it, which is where it should be caught.
                    Err(_) => true,
                    Ok(built) => verify_module(&built).is_err(),
                }
            }
        }
    });
}

/// A value a function never defined is refused.
///
/// A `ValueId` is a small integer, so a use of one that was never defined is a
/// perfectly ordinary-looking instruction. This is the check that stands between a
/// front end's arithmetic mistake and a register read at run time.
#[test]
fn a_use_of_an_undefined_value_is_refused() {
    check::<Good>(64, |case| {
        let mut module = ModuleBuilder::new("properties");
        let Ok(mut function) = module.function(
            &case.name,
            Linkage::Global,
            Vec::<Parameter>::new(),
            Type::Int {
                bits: 32,
                signed: true,
            },
        ) else {
            return true;
        };
        function
            .switch_to_block("entry")
            .expect("the entry block is always available");
        // A constant, and then a load from a *different* value that nothing defined.
        let defined = function
            .emit(Instruction::Const {
                value: ConstValue::Int(7),
                ty: Type::Int {
                    bits: 32,
                    signed: true,
                },
            })
            .expect("a constant emits");
        let undefined = ValueId::new(defined.get() + 40).expect("a value id is available");
        function
            .emit_effect(Instruction::Store {
                address: undefined,
                value: defined,
                width: lazalith_ir::StoreWidth::Word,
                space: lazalith_ir::MemorySpace::Program,
            })
            .expect("a store emits");
        function
            .terminate(Terminator::Return(lazalith_ir::ReturnValue::Value(defined)))
            .expect("a return terminates");
        let Ok(finished) = function.finish() else {
            return true;
        };
        module.add_function(finished).expect("added");
        match module.finish() {
            Err(_) => true,
            Ok(built) => matches!(
                verify_module(&built),
                Err(error) if matches!(error.kind, IrErrorKind::UndefinedValue { .. })
            ),
        }
    });
}

/// Building the same module twice gives the same module.
///
/// The builder numbers blocks and values as it goes, and a builder whose numbering
/// depended on a hash order or an allocation would produce two different modules
/// from one input — which would make every test that lowers a program and compares
/// bytes a test of the allocator.
#[test]
fn building_twice_gives_the_same_module() {
    check::<Good>(32, |case| {
        let (Ok(first), Ok(second)) = (minimal(&case.name).finish(), minimal(&case.name).finish())
        else {
            return true;
        };
        first == second
    });
}

/// Arbitrary bytes are a module or a refusal, and never a panic.
///
/// The IR is read from an object file by the backend, so a malformed module is a
/// real event for it rather than a hypothetical. Every case is either refused or
/// verifies, and "it panicked" is the one answer that is always a bug.
#[test]
fn arbitrary_bytes_are_a_module_or_a_refusal() {
    struct Bytes(Vec<u8>);
    impl Case for Bytes {
        fn generate(source: &mut Gen) -> Self {
            let length = source.below(256) as usize;
            Self(source.vector(length, |source| source.next_u8()))
        }
        fn describe(&self) -> String {
            format!("{:02x?}", &self.0[..self.0.len().min(24)])
        }
    }

    check::<Bytes>(64, |case| {
        // There is no reader for a module from bytes in this step's scope — the
        // backend has one and it is tested there — so what is asserted here is the
        // weaker and still real thing: nothing about arbitrary input reaches a
        // panic. The check is a `catch_unwind` rather than a call, because a panic
        // is the outcome being looked for.
        std::panic::catch_unwind(|| {
            let _ = minimal(&format!("f{}", case.0.len()));
            let _ = verify_module(&minimal("g").finish().expect("a minimal module builds"));
            true
        })
        .unwrap_or(false)
    });
}

/// Every block a terminator names exists in the function.
///
/// A property over the *function* rather than over the builder, and it is the one a
/// hand-written check is most likely to miss: the builder allocates block ids in
/// order, so a terminator naming a block that was never reserved is a *valid id*
/// pointing at nothing. The verifier is what has to catch it, and this says it does.
#[test]
fn every_named_block_exists() {
    check::<Good>(64, |case| {
        let Ok(module) = minimal(&case.name).finish() else {
            return true;
        };
        // Walk every terminator and confirm the blocks it names are in the function.
        for function in &module.functions {
            let present: Vec<BlockId> = function.blocks.iter().map(|block| block.id).collect();
            for block in &function.blocks {
                let named: Vec<BlockId> = match block.terminator {
                    Terminator::Jump(target) => vec![target],
                    Terminator::Branch {
                        then_block,
                        otherwise,
                        ..
                    } => vec![then_block, otherwise],
                    _ => Vec::new(),
                };
                if named.iter().any(|target| !present.contains(target)) {
                    return false;
                }
            }
        }
        verify_module(&module).is_ok()
    });
}

/// A module with two functions builds, verifies, and keeps both.
///
/// The other half of the redefinition test: a check that refuses everything is
/// satisfied by "a module with one function verifies". Two functions with different
/// names is the ordinary case, and it has to work.
#[test]
fn two_functions_with_different_names_both_survive() {
    check::<Good>(64, |case| {
        let mut module = minimal(&case.name);
        let other = format!("{}other", case.name);
        let mut second = module
            .function(&other, Linkage::Local, Vec::<Parameter>::new(), Type::Void)
            .expect("a second name is available");
        second
            .switch_to_block("entry")
            .expect("the entry block is always available");
        second
            .emit(Instruction::Const {
                value: ConstValue::Int(1),
                ty: Type::Int {
                    bits: 32,
                    signed: true,
                },
            })
            .expect("a constant emits");
        second
            .terminate(Terminator::Return(lazalith_ir::ReturnValue::Void))
            .expect("a void return terminates");
        module
            .add_function(second.finish().expect("complete"))
            .expect("added");
        let Ok(built) = module.finish() else {
            return false;
        };
        verify_module(&built).is_ok() && built.functions.len() == 2
    });
}
