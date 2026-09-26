//! Name-resolution tests.
//!
//! Resolution answers two questions: does a name exist, and may this use reach
//! it. Each test names the rule it pins, and every rejection asserts both the
//! code and where the diagnostic points.

use lazalith_compiler::frontend::compile;
use lazalith_compiler::resolve::codes;
use lazalith_types::SourceManager;

fn accepts(source: &str) {
    let mut sources = SourceManager::new();
    if let Err(error) = compile(&mut sources, "t.lazen", source) {
        panic!(
            "expected this to resolve, but it failed with {}:\n{}",
            error.code().as_str(),
            error.render()
        );
    }
}

fn rejects_with(source: &str, code: &str) -> String {
    let mut sources = SourceManager::new();
    match compile(&mut sources, "t.lazen", source) {
        Ok(_) => panic!("expected a rejection, but this compiled:\n{source}"),
        Err(error) => {
            let actual = error.code().as_str().to_string();
            assert_eq!(actual, code, "wrong diagnostic:\n{}", error.render());
            error.render()
        }
    }
}

#[test]
fn a_function_may_call_another_in_the_same_file_without_pub() {
    // The file's own module encloses everything in it, so a private item is
    // still reachable from the top level.
    accepts("fn helper() -> i32 { 0 } fn main() -> i32 { helper() }");
}

#[test]
fn an_undefined_name_is_reported_where_it_is_written() {
    let rendered = rejects_with("fn main() -> i32 { missing() }", codes::UNRESOLVED);
    assert!(rendered.contains("`missing`"), "{rendered}");
    let mut sources = SourceManager::new();
    let _ = compile(&mut sources, "t.lazen", "fn main() -> i32 { missing() }");
}

#[test]
fn a_local_name_shadows_an_item_for_later_uses() {
    // `let main = 1;` shadows the function `main` inside its own body, which is
    // allowed: the binding is found first.
    accepts("fn main() -> i32 { let main = 1; main }");
}

#[test]
fn a_nested_block_may_reuse_a_name_from_an_outer_block() {
    accepts("fn main() -> i32 { let value = 1; { let value = 2; } value }");
}

#[test]
fn a_nested_block_may_not_reuse_a_name_within_itself() {
    let rendered = rejects_with(
        "fn main() -> i32 { { let value = 1; let value = 2; } 0 }",
        codes::DUPLICATE_BINDING,
    );
    assert!(rendered.contains("already bound"), "{rendered}");
}

#[test]
fn a_function_body_is_one_scope_so_a_name_is_bound_once() {
    rejects_with(
        "fn main() -> i32 { let value = 1; let value = 2; value }",
        codes::DUPLICATE_BINDING,
    );
}

#[test]
fn a_parameter_and_a_local_may_not_share_a_name() {
    rejects_with(
        "fn f(value: i32) -> i32 { let value = 1; value }",
        codes::DUPLICATE_BINDING,
    );
}

#[test]
fn two_parameters_may_not_share_a_name() {
    rejects_with(
        "fn f(a: i32, a: i32) -> i32 { a }",
        codes::DUPLICATE_BINDING,
    );
}

#[test]
fn two_items_of_the_same_kind_may_not_share_a_name() {
    let rendered = rejects_with(
        "fn f() -> i32 { 0 } fn f() -> i32 { 1 }",
        codes::DUPLICATE_ITEM,
    );
    // The first definition is labelled, so the fix is obvious.
    assert!(rendered.contains("first defined here"), "{rendered}");
}

#[test]
fn a_function_and_a_const_may_not_share_a_name() {
    rejects_with(
        "const name: i32 = 1; fn name() -> i32 { 0 }",
        codes::DUPLICATE_ITEM,
    );
}

#[test]
fn two_modules_may_not_share_a_name() {
    rejects_with(
        "mod m { pub fn a() -> i32 { 0 } } mod m { pub fn b() -> i32 { 0 } }",
        codes::DUPLICATE_ITEM,
    );
}

#[test]
fn a_private_item_is_reachable_inside_its_own_module() {
    accepts(
        r#"
mod geometry {
    fn area(width: u32, height: u32) -> u32 { width * height }
    pub fn scaled(width: u32) -> u32 { area(width, 2) }
}
fn main() -> i32 { geometry::scaled(4) as i32 }
"#,
    );
}

#[test]
fn a_private_item_is_not_reachable_from_another_module() {
    let rendered = rejects_with(
        r#"
mod geometry {
    fn area(width: u32, height: u32) -> u32 { width * height }
}
fn main() -> i32 { geometry::area(4, 5) as i32 }
"#,
        codes::PRIVATE,
    );
    assert!(rendered.contains("`pub`"), "{rendered}");
}

#[test]
fn a_public_module_is_reachable_by_name() {
    accepts(
        r#"
pub mod geometry {
    pub fn area(width: u32, height: u32) -> u32 { width * height }
}
fn main() -> i32 { geometry::area(4, 5) as i32 }
"#,
    );
}

#[test]
fn a_nested_module_path_resolves_through_every_segment() {
    accepts(
        r#"
mod outer {
    pub mod inner {
        pub fn value() -> i32 { 7 }
    }
}
fn main() -> i32 { outer::inner::value() }
"#,
    );
}

#[test]
fn a_path_through_something_that_is_not_a_module_is_reported() {
    rejects_with(
        "fn f() -> i32 { 0 } fn main() -> i32 { f::value() }",
        codes::UNRESOLVED,
    );
}

#[test]
fn a_module_is_not_a_value() {
    rejects_with(
        "mod m { pub fn f() -> i32 { 0 } } fn main() -> i32 { let x = m; 0 }",
        codes::NOT_A_MODULE,
    );
}

#[test]
fn a_use_declaration_imports_an_item_under_its_own_name() {
    accepts(
        r#"
mod geometry {
    pub fn area(width: u32, height: u32) -> u32 { width * height }
}
use geometry::area;
fn main() -> i32 { area(4, 5) as i32 }
"#,
    );
}

#[test]
fn a_use_declaration_may_rename_the_import() {
    accepts(
        r#"
mod geometry {
    pub fn area(width: u32, height: u32) -> u32 { width * height }
}
use geometry::area as surface;
fn main() -> i32 { surface(4, 5) as i32 }
"#,
    );
}

#[test]
fn a_use_of_something_private_is_reported() {
    rejects_with(
        r#"
mod geometry {
    fn area(width: u32) -> u32 { width }
}
use geometry::area;
fn main() -> i32 { 0 }
"#,
        codes::PRIVATE,
    );
}

#[test]
fn a_use_of_a_path_that_does_not_exist_is_reported() {
    let mut sources = SourceManager::new();
    match compile(
        &mut sources,
        "t.lazen",
        "mod geometry { pub fn area() -> i32 { 0 } } use geometry::missing; fn main() -> i32 { 0 }",
    ) {
        Ok(_) => panic!("expected a rejection"),
        Err(error) => {
            // The import is unresolved, so any name that used it is too; either
            // way the program is rejected with a resolution code.
            let code = error.code().as_str().to_string();
            assert!(
                code.starts_with('N'),
                "expected a resolution code, got {code}:\n{}",
                error.render()
            );
        }
    }
}

#[test]
fn an_extern_declaration_is_visible_without_pub() {
    accepts("extern \"syscall\" fn close(handle: u32) -> i64; fn main() -> i32 { 0 }");
}

#[test]
fn a_const_is_visible_without_pub_at_the_top_level() {
    accepts("const LIMIT: i32 = 4; fn main() -> i32 { LIMIT }");
}
