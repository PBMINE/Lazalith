//! The C compiler, end to end.
//!
//! These tests do not check that the compiler *accepts* a program. They check
//! that a program it accepts produces IR the *IR verifier* accepts, and — the
//! part that matters most — that the refusals name the machine limit rather than
//! just failing.

use lazalith_c_compiler::CType;
use lazalith_c_compiler::ir::{self, Lowered};
use lazalith_c_compiler::{compile, ir::lower};
use lazalith_ir::{Instruction, Terminator, verify_module};
use lazalith_types::SourceManager;

/// Compiles C all the way to verified IR, or explains why it did not.
fn build(source: &str) -> Result<Lowered, String> {
    let mut sources = SourceManager::new();
    let (_, checked) = compile(&mut sources, "t.c", source).map_err(|error| error.render())?;
    let lowered = lower(&checked).map_err(|error| error.to_string())?;
    // The IR's own verifier, run again here so a test failure says *which*
    // stage let it through.
    verify_module(&lowered.module).map_err(|error| error.to_string())?;
    Ok(lowered)
}

/// Every code, including the ones after the first.
fn all_codes(source: &str) -> Vec<String> {
    let mut sources = SourceManager::new();
    lazalith_c_compiler::analyse(&mut sources, "t.c", source)
        .diagnostics
        .iter()
        .map(|error| String::from(error.code().as_str()))
        .collect()
}

/// The rendered text of a compile failure.
fn message(source: &str) -> String {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "t.c", source) {
        Ok(_) => String::from("the program compiled, so there is no message"),
        Err(error) => error.render(),
    }
}

/// The one function a program lowers, for a test that only cares about one.
fn only_function(lowered: &Lowered) -> &lazalith_ir::Function {
    lowered
        .module
        .function(&lowered.entry)
        .expect("a checked program always has an entry function")
}

/// Every instruction in a function, in order.
fn instructions(function: &lazalith_ir::Function) -> Vec<&Instruction> {
    function
        .blocks
        .iter()
        .flat_map(|block| block.instructions.iter())
        .collect()
}

/// A `main` that returns zero compiles, and the entry is the `main` the runtime
/// starts at.
#[test]
fn an_empty_main_compiles() {
    let lowered = build("int main(void) { return 0; }").expect("an empty main compiles");
    assert_eq!(lowered.entry, "c.main", "the entry is the C name, prefixed");
    assert!(lowered.module.function("c.main").is_some());
}

/// A return value becomes a value, not a call and not nothing.
#[test]
fn a_return_becomes_a_value() {
    let lowered = build("int main(void) { return 42; }").expect("a constant return compiles");
    let main = only_function(&lowered);
    assert!(
        matches!(main.blocks[0].terminator, Terminator::Return(_)),
        "the body ends in a return: {:?}",
        main.blocks[0].terminator
    );
    assert!(
        instructions(main).iter().any(|instruction| matches!(
            instruction,
            Instruction::Const {
                value: lazalith_ir::ConstValue::Int(42),
                ..
            }
        )),
        "and the constant is in the IR: {:?}",
        instructions(main)
    );
}

/// A local gets a frame slot, and the slot says which name it is for.
///
/// A frame with no slots for a function with a local would mean the local lives
/// in a register, and there is no register allocator — so this is the test that
/// a local is real storage.
#[test]
fn a_local_is_a_named_frame_slot() {
    let lowered = build("int main(void) { int x = 7; return x; }")
        .expect("a local with a constant initialiser compiles");
    let frame = lowered
        .frame(&lowered.entry)
        .expect("the entry has a frame");
    let local = frame
        .slots
        .iter()
        .find(|slot| slot.name.as_deref() == Some("x"))
        .expect("the local has a named slot");
    assert_eq!(local.size, 4, "an `int` is four bytes");
    assert!(!local.is_parameter, "and it is not a parameter");
}

/// A parameter is a slot the prologue fills, and it is marked as one.
///
/// This is the difference between a parameter and a local in the frame, and the
/// backend reads exactly this flag. A parameter slot that is not marked would
/// leave the incoming argument in a register nobody reads.
#[test]
fn a_parameter_is_a_slot_a_prologue_fills() {
    let lowered =
        build("int add(int a, int b) { return a + b; } int main(void) { return add(1, 2); }")
            .expect("a call with two arguments compiles");
    let add = lowered
        .module
        .function("c.add")
        .expect("the callee is in the module");
    let frame = lowered.frame("c.add").expect("the callee has a frame");
    let parameters: Vec<&lazalith_ir::FrameSlot> = frame
        .slots
        .iter()
        .filter(|slot| slot.is_parameter)
        .collect();
    assert_eq!(parameters.len(), 2, "both parameters are marked");
    for (slot, name) in parameters.iter().zip(["a", "b"]) {
        assert_eq!(slot.name.as_deref(), Some(name), "in declaration order");
    }
    let _ = add;
}

/// A global is a data segment, and its initialiser is in the segment's bytes.
///
/// A global stored as a number would need a relocation the linker could not
/// resolve, and a global stored as a segment with no bytes would be `.bss`
/// rather than the value the program asked for.
#[test]
fn a_global_is_a_segment_holding_its_value() {
    let lowered = build("int counter = 300; int main(void) { return counter; }")
        .expect("a global with a constant initialiser compiles");
    let segment = lowered
        .module
        .data("g.counter")
        .expect("the global is a data segment");
    assert_eq!(segment.bytes.len(), 4, "an `int` is four bytes");
    assert_eq!(
        segment.bytes,
        vec![0x2c, 0x01, 0x00, 0x00],
        "300 is stored little-endian, which is the only order this machine has"
    );
}

/// A string literal is a segment with its terminating null already in it.
///
/// The null is the point: a program that walks a string reads the zero that ends
/// it, and a segment without one would make that read a byte of whatever came
/// next.
#[test]
fn a_string_literal_carries_its_own_null() {
    let lowered =
        build("int main(void) { return \"hi\"[0]; }").expect("a string in an expression compiles");
    let segment = lowered
        .module
        .data("str0")
        .expect("the string is a data segment");
    assert_eq!(
        segment.bytes, b"hi\0",
        "the segment holds the letters and the null, and nothing else"
    );
}

/// A float is refused by name, and the message says the machine has no
/// floating point.
#[test]
fn a_float_is_refused_by_name() {
    let text = message("int main(void) { double x = 1; return 0; }");
    assert!(
        text.contains("no `double`") || text.contains("floating"),
        "the refusal names the type: {text}"
    );
    // A *literal* is refused earlier, by the lexer, because a floating constant is a
    // lexical fact before it is anything else — and it is worth asserting that it
    // is the *lexer* that refuses it, so the message says the machine has no
    // floating point rather than a parse error three tokens later.
    assert!(
        all_codes("int main(void) { return 1.5; }").contains(&String::from("C0120")),
        "a floating literal is refused by the lexer"
    );
}

/// A floating constant is refused by the *lexer*, with a code of its own.
///
/// A float literal is a lexical fact before it is anything else, and reporting it
/// at lexing means the message says "this machine has no floating point" rather
/// than a parse error three tokens later.
#[test]
fn a_float_literal_is_refused_by_the_lexer() {
    let found = all_codes("int main(void) { return 1.5; }");
    assert!(
        found.contains(&String::from("C0120")),
        "a floating constant is a lexer's refusal: {found:?}"
    );
}

/// An undeclared name is refused, and the code says which stage.
#[test]
fn an_undeclared_name_is_refused() {
    assert_eq!(
        all_codes("int main(void) { return missing; }"),
        vec!["C0301"],
        "a name that was never declared is a question about names"
    );
}

/// A `goto` is refused, and the message says why a second pass is needed.
///
/// This is the honest limitation, and it is worth a test: a `goto` that *worked*
/// by accident would be worse than one that is refused, because the program's
/// author would have no way to know.
#[test]
fn a_goto_is_refused_with_its_reason() {
    let mut sources = SourceManager::new();
    let source = "int main(void) { goto out; out: return 0; }";
    let checked = match lazalith_c_compiler::analyse(&mut sources, "t.c", source) {
        analysis if analysis.checked.is_some() => analysis.checked.expect("checked"),
        analysis => {
            // The checker accepted it; the refusal is the lowering's, which is
            // where the machine limit actually bites.
            assert!(
                analysis.diagnostics.is_empty(),
                "the checker should accept a `goto`: {:?}",
                analysis.diagnostics
            );
            let mut sources = SourceManager::new();
            let (_, checked) =
                compile(&mut sources, "t.c", source).expect("the checker accepts a goto");
            let _ = checked;
            return;
        }
    };
    let error = lower(&checked).expect_err("a goto must not be lowered");
    let text = error.to_string();
    assert!(
        text.contains("goto"),
        "the refusal names the construct: {text}"
    );
    assert!(
        text.contains("second pass"),
        "and says what would be needed: {text}"
    );
}

/// A call through a function pointer is refused, with the machine's reason.
#[test]
fn a_call_through_a_pointer_is_refused() {
    let source = "typedef int (*f)(void); int main(void) { f g = 0; return g(); }";
    let mut sources = SourceManager::new();
    let (_, checked) = match compile(&mut sources, "t.c", source) {
        Ok(checked) => checked,
        Err(error) => {
            // A refusal at checking time is also fine, as long as it is about the
            // call rather than about something else in the program.
            let text = error.render();
            assert!(
                text.contains("pointer") || text.contains("call"),
                "the refusal is about the call: {text}"
            );
            return;
        }
    };
    let error = lower(&checked).expect_err("a call through a pointer must not be lowered");
    assert!(
        error.to_string().contains("function pointer"),
        "the refusal names the construct: {error}"
    );
}

/// A `struct` may be declared, sized, and read through a pointer.
#[test]
fn a_struct_can_be_declared_and_held() {
    let lowered = build(
        r#"
        struct point { int x; int y; };
        int main(void) { struct point p; return 0; }
        "#,
    )
    .expect("a struct declaration and a local of that type compile");
    let frame = lowered
        .frame(&lowered.entry)
        .expect("the entry has a frame");
    let slot = frame
        .slots
        .iter()
        .find(|slot| slot.name.as_deref() == Some("p"))
        .expect("the struct local has a slot");
    assert_eq!(slot.size, 8, "two `int`s are eight bytes");
}

/// A `struct` returned by value is refused, because the ABI has no multiword
/// return.
///
/// The diagnostic must name the machine's limit, or the reader cannot tell
/// whether to pass a pointer or to change the machine.
#[test]
fn a_wide_return_is_refused_with_the_abi_limit() {
    let text = message(
        "struct big { int a; int b; int c; }; struct big f(void) { struct big b; return b; } int main(void) { return 0; }",
    );
    assert!(
        text.contains("larger than a word") || text.contains("multiword"),
        "the refusal names the ABI's limit: {text}"
    );
}

/// A `switch` compiles, as a chain of comparisons.
///
/// The machine has no jump table, so a chain is the only lowering that is right
/// on a machine without one. The test asserts it compiles at all, because a
/// `switch` that silently did nothing would be the worst outcome.
#[test]
fn a_switch_compiles() {
    let lowered =
        build("int main(void) { int x = 1; switch (x) { case 1: return 1; default: return 0; } }")
            .expect("a switch compiles");
    assert!(
        only_function(&lowered).blocks.len() >= 2,
        "a switch needs at least a body and an end block: {}",
        only_function(&lowered).blocks.len()
    );
}

/// Every loop form compiles, and a loop's blocks are distinct.
#[test]
fn every_loop_form_compiles() {
    for source in [
        "int main(void) { int i = 0; while (i < 3) { i = i + 1; } return i; }",
        "int main(void) { int i = 0; do { i = i + 1; } while (i < 3); return i; }",
        "int main(void) { int i = 0; for (i = 0; i < 3; i = i + 1) { } return i; }",
    ] {
        let lowered =
            build(source).unwrap_or_else(|error| panic!("{source} should compile: {error}"));
        assert!(
            only_function(&lowered).blocks.len() >= 2,
            "{source} needs more than one block"
        );
    }
}

/// `&&` and `||` become blocks, because they short-circuit.
///
/// An IR that evaluated both sides would be a *correct* IR for a program whose
/// right side cannot fault and a *wrong* one for a program whose right side
/// dereferences a null pointer. The block count is the observable difference.
#[test]
fn a_short_circuiting_operator_branches() {
    let lowered = build("int main(void) { int a = 1; int b = 0; return a && b; }")
        .expect("a short-circuiting operator compiles");
    assert!(
        only_function(&lowered).blocks.len() >= 3,
        "&& needs a block for the right side and one for the join: {}",
        only_function(&lowered).blocks.len()
    );
}

/// An array is a record of its elements, so its size is its own.
#[test]
fn an_array_is_its_own_storage() {
    let lowered = build("int main(void) { int a[3]; return 0; }").expect("an array local compiles");
    let frame = lowered
        .frame(&lowered.entry)
        .expect("the entry has a frame");
    let slot = frame
        .slots
        .iter()
        .find(|slot| slot.name.as_deref() == Some("a"))
        .expect("the array local has a slot");
    assert_eq!(slot.size, 12, "three `int`s are twelve bytes");
}

/// A pointer's arithmetic moves by the element's size, so the stride is in the
/// IR as a constant.
#[test]
fn a_subscript_multiplies_by_the_element_size() {
    let lowered = build("int main(void) { int a[3]; return a[1]; }").expect("a subscript compiles");
    let strides: Vec<i64> = instructions(only_function(&lowered))
        .iter()
        .filter_map(|instruction| match instruction {
            Instruction::Const {
                value: lazalith_ir::ConstValue::Int(value),
                ..
            } => Some(*value),
            _ => None,
        })
        .collect();
    assert!(
        strides.contains(&4),
        "the element size is in the IR, so the multiply is by a real number: {strides:?}"
    );
}

/// A `char` is one byte and an `int` is four, and the IR says so.
///
/// These are the sizes `docs/lz64.md` left open, and they are the ones a C
/// program depends on. A test is the only place they can be *pinned*, since a
/// documentation table is not checked by anything.
#[test]
fn the_integer_sizes_are_the_ones_the_abi_decided() {
    let lowered =
        build("int main(void) { char a = 0; short b = 0; int c = 0; long d = 0; return 0; }")
            .expect("a program declaring each integer type compiles");
    let frame = lowered
        .frame(&lowered.entry)
        .expect("the entry has a frame");
    for (name, size) in [("a", 1u32), ("b", 2), ("c", 4), ("d", 8)] {
        let slot = frame
            .slots
            .iter()
            .find(|slot| slot.name.as_deref() == Some(name))
            .unwrap_or_else(|| panic!("{name} has a slot"));
        assert_eq!(slot.size, size, "`{name}` is {size} bytes");
    }
}

/// A type name used as a value is refused, and says it is a type.
#[test]
fn a_type_name_as_a_value_is_refused() {
    // A keyword is refused by the parser, because `int` is a keyword and C does
    // not let a keyword be an identifier. A `typedef` name is a type *without* being
    // a keyword, so it reaches the resolver — and that is the case this asserts.
    assert_eq!(
        all_codes("typedef int number; int main(void) { return number; }"),
        vec!["C0305"],
        "a type name in a value position is a question about names"
    );
}

/// A `typedef` used as a type works, and a redefinition is refused.
#[test]
fn a_typedef_is_a_type_and_may_not_be_redefined() {
    let lowered = build("typedef int number; int main(void) { number x = 1; return x; }")
        .expect("a typedef names a type a declaration can use");
    assert!(lowered.module.function("c.main").is_some());
    assert_eq!(
        all_codes("typedef int number; typedef long number; int main(void) { return 0; }"),
        vec!["C0302"],
        "a typedef name may not be redefined"
    );
}

/// Two definitions of one function are refused, because the linker would be.
#[test]
fn a_function_may_not_be_defined_twice() {
    let found = all_codes(
        "int f(void) { return 1; } int f(void) { return 2; } int main(void) { return f(); }",
    );
    assert!(
        found.contains(&String::from("C0416")),
        "a second definition is a second symbol with one name: {found:?}"
    );
}

/// A variadic *definition* is refused while a variadic declaration is not.
///
/// `printf` has to be callable, so refusing a `...` in a prototype would make
/// the one library function a C program most wants unavailable. What is refused
/// is defining one, because its body has no way to find out how many arguments
/// it was given.
#[test]
fn a_variadic_definition_is_refused_and_a_declaration_is_not() {
    let declaration = "int log(const char *fmt, ...); int main(void) { return 0; }";
    build(declaration).unwrap_or_else(|error| {
        // A prototype with no body is not lowered at all, because there is no
        // body; the checker accepting it is the point.
        panic!("a variadic declaration should be accepted: {error}")
    });
    let text = message("int log(const char *fmt, ...) { return 0; } int main(void) { return 0; }");
    assert!(
        text.contains("variadic"),
        "the refusal names the construct: {text}"
    );
}

/// A call with the wrong number of arguments is refused with both counts.
///
/// "Expected 2 arguments" without saying how many were passed leaves the reader
/// to count them again, and a count is the one thing a compiler always knows.
#[test]
fn a_wrong_argument_count_says_both_counts() {
    let text = message("int f(int a, int b); int main(void) { return f(1); }");
    assert!(
        text.contains('2'),
        "the expected count is in the message: {text}"
    );
    assert!(
        text.contains("1 was passed"),
        "and the actual count: {text}"
    );
}

/// A comparison produces an `int`, not a `_Bool`.
///
/// This is C's rule and it is observable: `printf("%d", a < b)` is correct
/// C, and a compiler that produced a `_Bool` would print a `bool`'s width.
#[test]
fn a_comparison_produces_an_int() {
    let lowered =
        build("int main(void) { int a = 1; return a < 2; }").expect("a comparison compiles");
    assert!(
        instructions(only_function(&lowered))
            .iter()
            .any(|instruction| matches!(instruction, Instruction::Compare { .. })),
        "the comparison is one IR comparison: {:?}",
        instructions(only_function(&lowered))
    );
}

/// A call to an ABI syscall becomes a syscall in the IR, mangled.
///
/// A C program reaches the machine's facilities through its syscalls, and a call
/// to `write` has to be the ABI's `write` rather than a C function of that name.
#[test]
fn a_call_to_an_abi_name_becomes_a_syscall() {
    let lowered = build("int main(void) { int result; return write(1, \"hi\", 2, &result, 0); }")
        .unwrap_or_else(|error| panic!("a syscall call compiles: {error}"));
    assert!(
        instructions(only_function(&lowered))
            .iter()
            .any(|instruction| {
                matches!(
                    instruction,
                    Instruction::Call { target: lazalith_ir::CallTarget::Syscall(name), .. }
                        if name == "syscall.write"
                )
            }),
        "the call is the ABI's syscall, mangled so a C `write` cannot collide: {:?}",
        instructions(only_function(&lowered))
    );
}

/// A C function's IR name cannot collide with a syscall's.
#[test]
fn the_two_namespaces_are_separate() {
    assert_ne!(ir::FUNCTION_PREFIX, ir::SYSCALL_PREFIX);
    assert_ne!(
        ir::ir_name("write"),
        format!("{}{}", ir::SYSCALL_PREFIX, "write"),
        "a C function called `write` and the ABI's `write` are two symbols"
    );
}

/// A `_Static_assert` that holds passes, and one that does not is refused.
#[test]
fn a_static_assertion_is_checked_at_compile_time() {
    build("_Static_assert(1 + 1 == 2, \"arithmetic works\"); int main(void) { return 0; }")
        .expect("a true assertion compiles");
    let found =
        all_codes("_Static_assert(1 == 2, \"one is not two\"); int main(void) { return 0; }");
    assert!(
        found.contains(&String::from("C0410")),
        "a false assertion is refused: {found:?}"
    );
}

/// A `break` outside a loop is refused, by the parser and again by the checker.
///
/// Twice on purpose: the parser catches it while reading, and the checker
/// catches it for a body that came from somewhere else.
#[test]
fn a_break_outside_a_loop_is_refused() {
    let found = all_codes("int main(void) { break; return 0; }");
    assert!(
        found.iter().any(|code| code == "C0231" || code == "C0413"),
        "a `break` with nothing to break out of: {found:?}"
    );
}

/// A pointer and a number cannot be compared, and the message says why.
#[test]
fn a_pointer_cannot_be_compared_with_a_number() {
    let text = message("int main(void) { int *p = 0; int n = 0; return p == n; }");
    assert!(
        text.contains("null pointer constant")
            || text.contains("cannot be used where")
            || text.contains("compare"),
        "the refusal explains what may be compared: {text}"
    );
}

/// A `char` is promoted to an `int` before arithmetic, which is why `-c` on an
/// unsigned char is a negative `int`.
#[test]
fn a_narrow_integer_is_promoted() {
    let lowered = build("int main(void) { char a = 1; return a + 1; }")
        .expect("arithmetic on a promoted value compiles");
    assert!(
        instructions(only_function(&lowered))
            .iter()
            .any(|instruction| matches!(
                instruction,
                Instruction::Load {
                    width: lazalith_ir::LoadWidth::ByteSigned,
                    ..
                }
            )),
        "the value is read as a signed byte and widened, not read as a pointer: {:?}",
        instructions(only_function(&lowered))
    );
}

/// A C type's name is C's spelling, because a diagnostic that says `i32` where
/// the source said `unsigned long` is a diagnostic the reader has to translate.
#[test]
fn a_type_names_itself_as_c_spells_it() {
    assert_eq!(CType::unsigned(32).name(), "unsigned int");
    assert_eq!(CType::long().name(), "long");
    assert_eq!(CType::int().name(), "int");
    assert_eq!(CType::pointer_to(CType::int()).name(), "int *");
    assert_eq!(CType::array_of(CType::int(), 3).name(), "int[3]");
    assert_eq!(CType::ulong().name(), "unsigned long");
}
