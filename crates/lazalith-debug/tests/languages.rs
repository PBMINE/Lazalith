//! B17: one debug-information pipeline for C, Lazen and LZA assembly (§17).
//!
//! # What §17 asks for
//!
//! ```text
//! source -> compiler/assembler -> debug metadata -> .lzo -> .lzx -> VM -> debugger
//! ```
//!
//! and, where debug information exists, a mapping from guest PC to
//!
//! ```text
//! source file, line, column, function, LZA instruction
//! ```
//!
//! # What was actually there when this stage started
//!
//! The *format* was already right. `DebugSource` and `CodeMapping` exist in the object
//! format, the assembler emits both, the linker gathers them into a `DebugBlock`, the
//! image carries it, and `DebugController` can resolve a PC to a line and set a
//! breakpoint on a line. That pipeline was built for one language and worked for it.
//!
//! Three things were missing, and none of them would have been found by a test that
//! only used Lazen:
//!
//! 1. **C code had no debug information at all.** The Lazen lowering marks each
//!    statement on the IR builder; the C lowering did not. With no marks the IR's
//!    `source_map` is empty, code generation emits no mappings, and a C image's debug
//!    block contains *nothing but the startup sequence*. Not degraded C debugging --
//!    absent C debugging, with nothing to tell a user the front end never took part.
//! 2. **Line numbers resolved into the runtime, while claiming to be the user's file.**
//!    A Lazen build is `program + stdlib` and a C build is `libc + program`, compiled as
//!    one text. The debug source is named for the *user's* file and holds the runtime's
//!    text too, so a breakpoint on line 2 of `hello.lz` resolved to line 2 of a
//!    3081-line standard library. Confidently wrong, which is worse than silent.
//! 3. **There was no "function".** §17 lists it. The image carried no symbol table, so
//!    no stage above the VM could answer it without reaching back into the linker --
//!    which would only work on the machine that linked the program.
//!
//! Each test below is the one that would have failed for its defect.

use lazalith_types::ArchitectureConfig;

/// A Lazen object, built the way `lazen build` builds one.
fn lazen_object(source: &str, path: &str) -> lazalith_toolchain::ObjectFile {
    lazalith_driver::lazen_object(
        source,
        &lazalith_runtime::BuildOptions {
            architecture: ArchitectureConfig::lz64(),
            source_path: String::from(path),
            prelude: lazalith_runtime::library_text(),
        },
    )
    .expect("the Lazen program compiles")
}

/// A C program object, built the way `lazcc` builds one.
fn c_object(source: &str, path: &str) -> lazalith_toolchain::ObjectFile {
    lazalith_driver::compile_c(source, &lazalith_driver::CBuildOptions::hosted(path))
        .expect("the C program compiles")
}

/// A C library object: definitions with no `main`.
fn c_library(source: &str, path: &str) -> lazalith_toolchain::ObjectFile {
    lazalith_driver::compile_c(
        source,
        &lazalith_driver::CBuildOptions::hosted(path).as_library(),
    )
    .expect("the C library compiles")
}

/// An assembly object, built the way `lazas` builds one.
fn assembly_object(source: &str, path: &str) -> lazalith_toolchain::ObjectFile {
    lazalith_toolchain::assemble_named(path, source).expect("the assembly assembles")
}

/// Links into an image and returns its debug block.
fn debug_block(
    objects: Vec<lazalith_toolchain::ObjectFile>,
    entry: &str,
) -> lazalith_os::debug::DebugBlock {
    let image = lazalith_driver::link(&objects, ArchitectureConfig::lz64(), entry)
        .expect("the objects link");
    image
        .debug()
        .cloned()
        .expect("the image carries debug information")
}

/// Every source location in a block that lands in a file the user wrote.
fn user_locations(block: &lazalith_os::debug::DebugBlock, suffix: &str) -> Vec<(u32, u32)> {
    block
        .entries()
        .iter()
        .filter_map(|entry| block.resolve(entry.address))
        .filter(|location| location.name.ends_with(suffix))
        .map(|location| (location.line_number(), location.column_number()))
        .collect()
}

/// The text the block carries for the source called `name`.
fn text_of<'a>(block: &'a lazalith_os::debug::DebugBlock, name: &str) -> &'a str {
    block
        .files()
        .iter()
        .find(|file| file.name() == name)
        .map(|file| file.text())
        .unwrap_or_else(|| panic!("the block has a source called {name}"))
}

#[test]
fn lazen_source_maps_to_the_users_own_lines() {
    let source = "fn main() -> i32 {\n    return 3;\n}\n";
    let block = debug_block(
        vec![lazen_object(source, "hello.lz")],
        lazalith_driver::LAZEN_ENTRY,
    );

    let paths: Vec<&str> = block.files().iter().map(|file| file.name()).collect();
    assert!(
        paths.contains(&"hello.lz"),
        "the user's file is a source in its own right: {paths:?}"
    );
    assert!(
        paths.iter().any(|path| path.contains("lazalith-runtime")),
        "and the standard library is a separate one rather than folded into it: {paths:?}"
    );

    // The one trailing blank line is deliberate. `compose` puts a blank line between
    // the program and the prelude and it belongs to neither half; giving it to the
    // program's half leaves the **runtime's** line numbers exact, and an extra blank
    // line at the end of a user's file is a far smaller lie than a standard library
    // that is off by one everywhere.
    assert_eq!(
        text_of(&block, "hello.lz").trim_end(),
        source.trim_end(),
        "and the file the debugger reads back is the text the user wrote. Before B17 \
         this was the 3081-line composed unit, so every line number was the runtime's"
    );

    let locations = user_locations(&block, ".lz");
    assert!(
        locations.contains(&(1, 1)),
        "a mapping in the program resolves to line 1: {locations:?}"
    );
    assert!(
        locations.iter().any(|(line, _)| *line == 2),
        "and one resolves to line 2, which is the `return`: {locations:?}"
    );
}

#[test]
fn c_source_maps_to_the_users_own_lines() {
    let source = "int main(void) {\n    return 3;\n}\n";
    let block = debug_block(
        vec![c_object(source, "hello.c")],
        &lazalith_driver::c_entry(),
    );

    let locations = user_locations(&block, ".c");
    assert!(
        locations.iter().any(|(line, _)| *line == 2),
        "a C mapping resolves to the line the user wrote, not to libc: {locations:?}"
    );
    assert_eq!(
        text_of(&block, "hello.c").trim_end(),
        source.trim_end(),
        "and the C file holds exactly the text the user wrote. The C build composes \
         `libc + program`, so before the split this was 422 lines of runtime with the \
         user's two lines at the end -- and every line number was wrong while claiming \
         to be right"
    );
}

#[test]
fn assembly_source_maps_to_its_own_lines() {
    let source = ".arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    LI r0, 1\n    SYSCALL\n";
    let block = debug_block(vec![assembly_object(source, "boot.la")], "_start");
    let locations = user_locations(&block, ".la");
    assert!(
        locations.iter().any(|(line, _)| *line == 6),
        "the `LI` on line 6 resolves to line 6: {locations:?}"
    );
    assert!(
        locations.iter().any(|(line, _)| *line == 7),
        "and the `SYSCALL` on line 7: {locations:?}"
    );
}

#[test]
fn a_guest_address_names_its_function() {
    let block = debug_block(
        vec![lazen_object(
            "fn main() -> i32 {\n    return 3;\n}\n",
            "fns.lz",
        )],
        lazalith_driver::LAZEN_ENTRY,
    );
    assert!(
        !block.functions().is_empty(),
        "the linked image carries functions, not just source mappings"
    );
    let names: Vec<&str> = block.functions().iter().map(|f| f.name.as_str()).collect();
    assert!(
        names.contains(&"fn.main"),
        "and the user's own `main` is one of them; the first are {:?}",
        &names[..3.min(names.len())]
    );
    assert!(
        names.iter().any(|name| name.contains("rt::sys::print")),
        "and so is a standard-library function, because the table is built from the \
         linker's symbols and knows both"
    );

    // The table round-trips: it is written into the image and read back out.
    let decoded =
        lazalith_os::debug::DebugBlock::decode(&block.encode()).expect("the block decodes");
    assert_eq!(
        decoded.functions().len(),
        block.functions().len(),
        "the function table survives the encoding"
    );
    for function in block.functions() {
        assert!(
            decoded
                .function_at(function.start)
                .is_some_and(|found| found.name == function.name),
            "and `{}` is still findable at its own address after a round trip",
            function.name
        );
    }
}

#[test]
fn a_function_range_excludes_the_gap_after_it() {
    let mut block = lazalith_os::debug::DebugBlock::new();
    block.add_function(lazalith_os::debug::DebugFunction {
        name: String::from("first"),
        start: 0x1000,
        end: 0x1010,
    });
    block.add_function(lazalith_os::debug::DebugFunction {
        name: String::from("second"),
        start: 0x1020,
        end: 0x1030,
    });
    assert_eq!(
        block.function_at(0x1000).map(|f| f.name.as_str()),
        Some("first")
    );
    assert_eq!(
        block.function_at(0x100f).map(|f| f.name.as_str()),
        Some("first")
    );
    assert_eq!(
        block.function_at(0x1010).map(|f| f.name.as_str()),
        None,
        "the byte after the first function names nothing rather than the first function"
    );
    assert_eq!(
        block.function_at(0x1018).map(|f| f.name.as_str()),
        None,
        "nor does the padding between them"
    );
    assert_eq!(
        block.function_at(0x1020).map(|f| f.name.as_str()),
        Some("second")
    );
}

#[test]
fn all_three_languages_share_one_debug_block() {
    let block = debug_block(
        vec![
            c_library("int from_c(void) { return 4; }\n", "c.c"),
            assembly_object(
                ".arch lz64\n.entry _start\n.global _start\n.section .text\n_start:\n    LI r0, 0\n    RET\n",
                "a.la",
            ),
            lazen_object("fn main() -> i32 {\n    return 2;\n}\n", "m.lz"),
        ],
        lazalith_driver::LAZEN_ENTRY,
    );
    let names: Vec<&str> = block.files().iter().map(|file| file.name()).collect();
    for suffix in [".c", ".la", ".lz"] {
        assert!(
            names.iter().any(|name| name.ends_with(suffix)),
            "the block carries a {suffix} source: {names:?}"
        );
    }
    assert!(
        !user_locations(&block, ".c").is_empty(),
        "and the C half has mappings of its own, not only the ones it brought along"
    );
    assert!(
        !user_locations(&block, ".la").is_empty(),
        "and the assembly half"
    );
    assert!(
        !user_locations(&block, ".lz").is_empty(),
        "and the Lazen half"
    );
    assert!(
        block
            .functions()
            .iter()
            .any(|function| function.name == "fn.c.from_c"),
        "and one function table covers all three languages, including a C function: {:?}",
        block
            .functions()
            .iter()
            .filter(|f| f.name.contains("from_c") || f.name == "fn.main")
            .map(|f| &f.name)
            .collect::<Vec<_>>()
    );
}
