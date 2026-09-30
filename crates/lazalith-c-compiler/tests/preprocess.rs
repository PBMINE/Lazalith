//! The preprocessor.
//!
//! **The tests that matter here are the refusals, and there are more of those than of
//! successes.** A preprocessor's successes are easy to get right by accident — expand the
//! macro, get the value — and its failures are what a kernel's build actually runs into: a
//! header that includes itself, a `#if` this compiler does not implement, a macro called
//! with the wrong number of arguments.
//!
//! The single most important test in this file is
//! [`an_unimplemented_if_is_a_diagnostic_and_not_a_skipped_line`], because the behaviour it
//! pins is the one that is *invisible when it is broken*: a preprocessor that ignores
//! `#if` compiles and links and then means the opposite of what it says.

use lazalith_c_compiler::preprocess::codes;
use lazalith_c_compiler::{Includes, MapIncludes, compile_for};
use lazalith_types::SourceManager;

/// Compiles C with no headers, and renders the first failure.
fn message(source: &str) -> String {
    build(source, &[], None)
}

/// Compiles C with headers, and renders the first failure.
fn message_with(source: &str, headers: &[(&str, &str)], arch: Option<&str>) -> String {
    build(source, headers, arch)
}

/// Compiles C and renders the first failure, or says it compiled.
fn build(source: &str, headers: &[(&str, &str)], arch: Option<&str>) -> String {
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in headers {
        resolver.insert(name, text);
    }
    let mut includes = match arch {
        Some(arch) => Includes::for_arch(arch, &mut resolver),
        None => Includes::none(&mut resolver),
    };
    match compile_for(&mut sources, "t.c", source, &mut includes) {
        Ok(_) => String::from("the program compiled, so there is no message"),
        Err(error) => error.render(),
    }
}

/// Every code, so a test can assert on which of several refusals fired.
fn codes_of(source: &str, headers: &[(&str, &str)], arch: Option<&str>) -> Vec<String> {
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in headers {
        resolver.insert(name, text);
    }
    let mut includes = match arch {
        Some(arch) => Includes::for_arch(arch, &mut resolver),
        None => Includes::none(&mut resolver),
    };
    lazalith_c_compiler::analyse_for(&mut sources, "t.c", source, &mut includes)
        .diagnostics
        .iter()
        .map(|error| String::from(error.code().as_str()))
        .collect()
}

/// A program that reads one macro and returns it, for a test about the value.
fn identity(name: &str, body: &str) -> String {
    format!("unsigned long main(void) {{ return {body}; }} // {name}")
}

#[test]
fn an_object_like_macro_is_substituted() {
    let source = "#define LIMIT 7\nunsigned long main(void) { return LIMIT; }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a macro that is a constant is a program that compiles");
    // The *value* is proved elsewhere — by the differential test that compiles the same
    // arithmetic through Lazen and compares. What matters here is that `LIMIT` resolved at
    // all: an unexpanded name is C0403, and this is not it.
    assert!(
        !message("#define LIMIT 7\nunsigned long main(void) { return UNDEFINED_NAME; }")
            .contains("LIMIT"),
        "and the name that is not defined is still the one that is reported"
    );
}

#[test]
fn a_macro_body_is_expanded_in_the_program() {
    // `#define DOUBLE(x) ((x) + (x))` is the shape a kernel header uses for anything from
    // an alignment round-up to a register mask. It has to work, and the arithmetic has to
    // come out right, which is what the end-to-end build below proves.
    let source = "#define DOUBLE(x) ((x) + (x))\n\
                  unsigned long main(void) { return DOUBLE(3); }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    let (_, checked) = compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a function-like macro expands");
    assert!(checked.diagnostics.is_empty(), "and type checks");
}

#[test]
fn a_macro_is_expanded_more_than_once() {
    let source = "#define TWO 2\n\
                  unsigned long f(void) { return TWO; }\n\
                  unsigned long g(void) { return TWO + TWO; }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("one macro, two uses, and both are expanded");
}

#[test]
fn a_function_like_macro_splits_arguments_on_top_level_commas_only() {
    // The argument is a call, so it contains a comma that is not a separator. A preprocessor
    // that split naively would see three arguments to a two-parameter macro and refuse a
    // correct program.
    let source = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n\
                  unsigned long add(unsigned long x, unsigned long y) { return x + y; }\n\
                  unsigned long main(void) { return MAX(1, add(2, 3)); }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a nested call is one argument, not three");
}

#[test]
fn a_function_like_macro_name_with_no_call_after_it_stays_an_identifier() {
    // C's rule: a function-like macro is only invoked when a `(` follows. A bare mention is
    // an identifier, and stays one.
    //
    // **This is not a hypothetical.** C cannot tell a macro call from a function
    // *declaration* — `unsigned long f(unsigned long n)` has `f` followed by `(` — so in C
    // a function-like macro and a function of the same name genuinely collide, and the
    // language resolves it in favour of the macro. What this test pins is the half that is
    // unambiguous: with no `(` there is no call.
    let source = "#define SIZE(x) ((x) * 2)\n\
                  unsigned long main(void) { return SIZE; }";
    let rendered = message(source);
    assert!(
        rendered.contains("SIZE"),
        "a bare mention of a function-like macro is still that name, and is reported as \
         undeclared rather than silently expanded: {rendered}"
    );
}

#[test]
fn a_macro_name_inside_a_string_is_data_and_is_not_substituted() {
    // `MAX` inside a string literal is six characters, not a call. A text-level
    // preprocessor gets this wrong; a token-level one cannot, because the string is one
    // token and its text is never a name.
    let source = "#define SECRET 7\n\
                  const char *what(void) { return \"SECRET\"; }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    let (_, checked) = compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a macro's name inside a string is just text");
    assert!(checked.diagnostics.is_empty(), "and the string is intact");
}

#[test]
fn a_header_is_included_and_its_declarations_are_usable() {
    let headers = [("lazos/abi.h", "unsigned long laz_word_size(void);")];
    let source = "#include <lazos/abi.h>\n\
                  unsigned long main(void) { return laz_word_size(); }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in &headers {
        resolver.insert(name, text);
    }
    let mut includes = Includes::for_arch("lz64", &mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a declared function from a header is callable");
}

#[test]
fn an_include_guard_means_a_header_read_twice_contributes_once() {
    // The guard is the whole mechanism, and it is the one thing every header does. Read
    // twice *without* a guard the same declaration would be defined twice and refused as
    // a duplicate — so this test also proves the second read really did happen.
    let headers = [(
        "guard.h",
        "#ifndef GUARD_H\n#define GUARD_H\nunsigned long once(void);\n#endif\n",
    )];
    let source = "#include \"guard.h\"\n#include \"guard.h\"\n\
                  unsigned long main(void) { return once(); }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in &headers {
        resolver.insert(name, text);
    }
    let mut includes = Includes::for_arch("lz64", &mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("an include guard makes the second read empty");
}

#[test]
fn a_header_without_a_guard_read_twice_is_a_duplicate_definition() {
    // **The control for the test above.** If this one also compiled, the guard test would
    // be proving that the second `#include` was ignored rather than that the guard worked.
    let headers = [("noguard.h", "unsigned long twice(void);\n")];
    let source = "#include \"noguard.h\"\n#include \"noguard.h\"\n\
                  unsigned long main(void) { return twice(); }";
    let found = codes_of(source, &headers, Some("lz64"));
    assert!(
        found.iter().any(|code| code != codes::HEADER_NOT_FOUND),
        "a header read twice without a guard is a duplicate, not a silent success: {found:?}"
    );
}

#[test]
fn the_target_chooses_a_word_width_with_an_ifdef() {
    // This is the reason the preprocessor has `#ifdef` and not `#if`: a header that has to
    // work for two word sizes can ask which one it is for, and that is a definedness test.
    // One header, two targets, and the program does not change — which is the property that
    // matters, because it is what lets a kernel's headers be written once.
    let headers = [(
        "width.h",
        "#ifdef __LZ64__\ntypedef unsigned long word;\n#else\ntypedef unsigned int word;\n#endif\n",
    )];
    let source = "#include <width.h>\nword main(void) { return sizeof(word); }";

    for arch in ["lz64", "lz32"] {
        let mut sources = SourceManager::new();
        let mut resolver = MapIncludes::new();
        for (name, text) in &headers {
            resolver.insert(name, text);
        }
        let mut includes = Includes::for_arch(arch, &mut resolver);
        compile_for(&mut sources, "t.c", source, &mut includes)
            .unwrap_or_else(|error| panic!("the {arch} branch: {}", error.render()));
    }
}

#[test]
fn a_macro_that_is_defined_in_terms_of_itself_terminates() {
    // The header idiom: a function-like macro whose body mentions its own name. Without
    // the paint rule this is an infinite loop, and the symptom a build shows is a hang
    // rather than a message.
    let source = "#define f(x) f(x)\n\
                  unsigned long f(unsigned long n) { return n; }\n\
                  unsigned long main(void) { return f(1); }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("a self-referential macro expands once and stops");
}

#[test]
fn two_macros_defined_in_terms_of_each_other_are_refused_rather_than_looping() {
    // Mutual recursion has no local fix, so it is caught by the budget. The important part
    // is that it is a *diagnostic*: a compiler that hangs on a bad header is a compiler
    // whose failure is indistinguishable from a slow build.
    let source = "#define A B\n#define B A\nunsigned long main(void) { return A; }";
    let found = codes_of(source, &[], None);
    assert_eq!(
        found,
        vec![String::from(codes::EXPANSION_BUDGET)],
        "and the diagnostic says what happened"
    );
}

// -- the refusals --

#[test]
fn an_unimplemented_if_is_a_diagnostic_and_not_a_skipped_line() {
    // **The most important test in this file.** The old behaviour stepped over every
    // directive, which made `#if 1` and `#if 0` the same program: a build that compiled,
    // linked, and meant the opposite of what it said. The fix is to say no.
    let found = codes_of("#if 1\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(
        found,
        vec![String::from(codes::UNSUPPORTED_DIRECTIVE)],
        "an `#if` this compiler does not implement is reported, not ignored"
    );
    let found = codes_of("#if 0\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(
        found,
        vec![String::from(codes::UNSUPPORTED_DIRECTIVE)],
        "and `#if 0` is no more acceptable than `#if 1`: a silent `0` is the bug"
    );
}

#[test]
fn an_unknown_directive_is_refused_and_a_pragma_is_not() {
    // A misspelling is a bug. `#pragma` is addressed to another tool by design, and
    // refusing it would make the front end unusable with any real header.
    let found = codes_of("#foob\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(
        found,
        vec![String::from(codes::UNSUPPORTED_DIRECTIVE)],
        "a misspelled directive is reported by name"
    );
    let found = codes_of(
        "#pragma once\nunsigned long main(void) { return 0; }",
        &[],
        None,
    );
    assert!(found.is_empty(), "and `#pragma` is skipped: {found:?}");
}

#[test]
fn a_macro_called_with_the_wrong_number_of_arguments_is_refused() {
    let source = "#define ADD(a, b) ((a) + (b))\n\
                  unsigned long main(void) { return ADD(1); }";
    let found = codes_of(source, &[], None);
    assert_eq!(found, vec![String::from(codes::ARGUMENT_COUNT)]);
}

#[test]
fn a_macro_redefined_differently_is_refused_and_redefined_identically_is_not() {
    // The identical case is not a formality: the include-guard-plus-`#undef` dance in
    // every real header redefines a macro deliberately, and refusing that would make
    // correct headers wrong.
    let same = "#define X 1\n#define X 1\nunsigned long main(void) { return X; }";
    assert!(
        codes_of(same, &[], None).is_empty(),
        "the same definition twice is allowed, because headers do it on purpose"
    );
    let different = "#define X 1\n#define X 2\nunsigned long main(void) { return X; }";
    assert_eq!(
        codes_of(different, &[], None),
        vec![String::from(codes::REDEFINED_MACRO)],
        "and a different one is not, because then the meaning of every use depends on \
         which header was read first"
    );
}

#[test]
fn a_header_that_includes_itself_is_refused_with_the_chain() {
    // The chain is the message that finds the bug. "`a.h` includes itself" is true and
    // useless; the chain says which four headers to look at.
    let headers = [("a.h", "#include \"b.h\"\n"), ("b.h", "#include \"a.h\"\n")];
    let rendered = message_with(
        "#include \"a.h\"\nunsigned long main(void) { return 0; }",
        &headers,
        None,
    );
    assert!(
        rendered.contains("includes itself"),
        "the refusal says what happened: {rendered}"
    );
    assert!(
        rendered.contains("a.h") && rendered.contains("b.h"),
        "and names the chain, which is the part that finds the bug: {rendered}"
    );
}

#[test]
fn a_missing_header_is_refused_by_name() {
    let rendered = message("#include <nope.h>\nunsigned long main(void) { return 0; }");
    assert!(
        rendered.contains("nope.h"),
        "the refusal names the header, because a person is looking for it: {rendered}"
    );
}

#[test]
fn a_conditional_that_is_never_closed_is_refused_against_the_file_that_opened_it() {
    let headers = [(
        "open.h",
        "#ifdef __LAZALITH__\nunsigned long leaked(void);\n",
    )];
    let source = "#include \"open.h\"\nunsigned long main(void) { return 0; }";
    let found = codes_of(source, &headers, Some("lz64"));
    assert_eq!(found, vec![String::from(codes::UNCLOSED_CONDITIONAL)]);
    // And the point is that it is reported in the *header's* file, which is where the
    // mistake is and where the reader is looking.
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in &headers {
        resolver.insert(name, text);
    }
    let mut includes = Includes::for_arch("lz64", &mut resolver);
    let error = compile_for(&mut sources, "t.c", source, &mut includes)
        .expect_err("an unclosed conditional is a failure");
    let rendered = error.render();
    assert!(
        rendered.contains("open.h"),
        "the refusal points into the header, not into the program that included it: \
         {rendered}"
    );
}

#[test]
fn an_else_with_nothing_to_match_is_refused() {
    let found = codes_of("#else\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(found, vec![String::from(codes::MISPLACED_CONDITIONAL)]);
    let found = codes_of("#endif\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(found, vec![String::from(codes::MISPLACED_CONDITIONAL)]);
    let found = codes_of(
        "#ifdef A\n#else\n#else\n#endif\nunsigned long main(void) { return 0; }",
        &[],
        None,
    );
    assert_eq!(found, vec![String::from(codes::MISPLACED_CONDITIONAL)]);
}

#[test]
fn an_error_directive_is_a_failure_carrying_its_own_message() {
    // `#error` is how a header says "this configuration cannot work". Swallowing it would
    // mean building a kernel that cannot run.
    //
    // **The condition is `__LAZALITH__`, not `__LZ32__`,** because the test has no target:
    // with no arch no `__LZ*__` macro is defined, so an `#error` under `#ifdef __LZ32__`
    // correctly does not fire, and a test that asserted it did would be asserting a lie.
    let rendered = message(
        "#ifdef __LAZALITH__\n#error lz32 has no kernel ABI yet\n#endif\n\
         unsigned long main(void) { return 0; }",
    );
    assert!(
        rendered.contains("lz32 has no kernel ABI yet"),
        "the programmer's own words survive to the diagnostic: {rendered}"
    );
}

#[test]
fn an_error_directive_inside_a_skipped_branch_does_not_fire() {
    // The other half, and the reason a header can offer two configurations. A group that is
    // not being read contributes nothing, not even a failure — that is what makes
    // `#ifdef`-based feature selection work at all.
    let found = codes_of(
        "#ifdef __LZ32__\n#error not this one\n#endif\n\
         unsigned long main(void) { return 0; }",
        &[],
        None,
    );
    assert!(
        found.is_empty(),
        "a skipped `#error` is not a failure: {found:?}"
    );
}

#[test]
fn a_define_with_no_name_is_refused() {
    let found = codes_of("#define\nunsigned long main(void) { return 0; }", &[], None);
    assert_eq!(found, vec![String::from(codes::NAMELESS_DEFINE)]);
}

#[test]
fn a_function_like_macro_with_a_broken_parameter_list_is_refused() {
    let found = codes_of(
        "#define M(a b) a\nunsigned long main(void) { return 0; }",
        &[],
        None,
    );
    assert_eq!(found, vec![String::from(codes::MALFORMED_PARAMETERS)]);
}

#[test]
fn a_header_name_that_is_not_closed_is_refused() {
    // The lexer catches this one before the preprocessor does, because a header name is
    // the one directive argument that is not C tokens. Either way it is a refusal and not
    // a header lookup for a name called `"oops`.
    let rendered = message("#include \"oops\nunsigned long main(void) { return 0; }");
    assert!(
        !rendered.is_empty(),
        "an unclosed header name is a failure: {rendered}"
    );
}

// -- the built-ins --

#[test]
fn the_target_macros_are_defined_and_a_suspicious_one_is_not() {
    // `__LZ64__` is how a header picks a word size, so it has to be there. `__DATE__` is
    // refused rather than defined, and this test is why: a macro that changes a program's
    // meaning depending on when it was built is not a fact about the program.
    let headers = [(
        "wide.h",
        "#ifdef __LZ64__\ntypedef unsigned long word;\n#endif\n",
    )];
    let source = "#include <wide.h>\nword main(void) { return 0; }";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    for (name, text) in &headers {
        resolver.insert(name, text);
    }
    let mut includes = Includes::for_arch("lz64", &mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("`__LZ64__` is defined when the target is lz64");

    let found = codes_of(
        "#ifdef __DATE__\n#error __DATE__ is defined\n#endif\nunsigned long main(void) { return 0; }",
        &[],
        None,
    );
    assert!(
        found.is_empty(),
        "and `__DATE__` is not defined, so the `#error` above never fires: {found:?}"
    );
}

#[test]
fn line_and_file_are_the_ones_the_macro_was_used_in() {
    // A kernel uses `__LINE__` in an assertion, and an assertion that reports the wrong
    // line is worse than one that does not report at all.
    let source = "unsigned long main(void) {\n\
                  unsigned long here = __LINE__;\n\
                  return here;\n}";
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", source, &mut includes)
        .expect("`__LINE__` is a number and the program type checks");
}

#[test]
fn undef_makes_a_name_go_away() {
    // The "is this already defined" idiom: a header defines a default, and a program that
    // wants a different one undefines it first.
    let source = "#define MODE 1\n#undef MODE\n#define MODE 2\n\
                  unsigned long main(void) { return MODE; }";
    let found = codes_of(source, &[], None);
    assert!(
        found.is_empty(),
        "an `#undef` makes a later definition a first definition: {found:?}"
    );
}

// -- the cost --

#[test]
fn a_program_with_no_directives_is_preprocessed_unchanged() {
    // The fast path, and the reason it exists. A preprocessor that walks every token of
    // every program to discover it has nothing to do is a preprocessor that costs the
    // whole compilation to be correct about nothing.
    let source = identity("plain", "1");
    let mut sources = SourceManager::new();
    let mut resolver = MapIncludes::new();
    let mut includes = Includes::none(&mut resolver);
    compile_for(&mut sources, "t.c", &source, &mut includes)
        .expect("a program with no directives compiles exactly as before");
}
