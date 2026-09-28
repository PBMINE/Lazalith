//! Hardening: every C integer spelling, measured twice.
//!
//! Four wrong-answer defects in this frontend all had the same root cause: a type
//! spelled two or more words — `unsigned int`, `long int`, bare `signed` — was
//! reduced to the wrong *width* or the wrong *sign*. A single test case per defect
//! would have caught each of them, and did; but a matrix catches the fifth, which is
//! the one nobody has written yet.
//!
//! The matrix is here because it is what *found* them. A fix without its finder is a
//! fix that can be quietly undone by the next person who touches the specifier merge,
//! and the C frontend has no other test that asks a question about a *type* at all —
//! every other test asks what a program *does*, which is downstream of this decision
//! and would agree with a frontend that had it wrong in a new way.
//!
//! Each spelling is measured **twice**, because the two measurements fail
//! differently:
//!
//! - `sizeof` catches the **width**, and nothing else. An `unsigned int` that
//!   became a `char` still has the right sign, so only a value above `INT_MAX` can
//!   catch it.
//! - A value above `INT_MAX` catches the **sign**, and nothing else. A correct
//!   `unsigned int` has the right size and the wrong sign if it is measured by size
//!   alone.
//!
//! So a spelling is only fully checked when both pass.

use lazalith_types::ArchitectureConfig;

/// Builds, links and runs a C program, and reports what it printed and returned.
fn run_c(body: &str) -> (String, u32) {
    use lazalith_codegen::{CodegenOptions, generate};
    use lazalith_toolchain::{LinkOptions, link_objects};

    let config = ArchitectureConfig::lz64();
    let mut unit = String::from(lazalith_c_runtime::C_RUNTIME);
    unit.push(char::from(10u8));
    unit.push_str(body);
    let mut sources = lazalith_types::SourceManager::new();
    let (_, checked) = lazalith_c_compiler::compile(&mut sources, "t.c", &unit)
        .unwrap_or_else(|error| panic!("the C program should compile:\n{}", error.render()));
    let lowered = lazalith_c_compiler::lower(&checked)
        .unwrap_or_else(|error| panic!("the C program should lower: {error}"));
    let program = generate(
        &lowered.module,
        &lowered.frames,
        &lowered.entry,
        &CodegenOptions::lz64("t.c"),
        &unit,
    )
    .unwrap_or_else(|error| panic!("the C program should generate: {error}"));
    let startup = lazalith_runtime::startup_object_for(config, "fn.c.main")
        .expect("the C entry sequence assembles");
    let linked = link_objects(
        &[program.object().clone(), startup],
        &LinkOptions {
            entry_symbol: Some(String::from("entry")),
        },
    )
    .unwrap_or_else(|error| panic!("the C program should link: {error}"));
    let bytes = linked.image().to_bytes().expect("the image serializes");
    let finished = lazalith_runtime::run_image_with(
        &bytes,
        config,
        lazalith_devices::DeviceManager::<lazalith_devices::NoDevice>::new(),
    )
    .unwrap_or_else(|error| panic!("the C program should run: {error}"));
    (
        String::from_utf8_lossy(&finished.output).into_owned(),
        finished.exit_code,
    )
}

/// A spelling, its size, and whether it is signed — the two properties C defines.
struct Spelling {
    /// Exactly as it would be written in a declaration.
    declaration: &'static str,
    /// C's `sizeof`, in bytes.
    size: u32,
    /// Whether C says the type is signed.

    /// Whether C says the type is signed.
    signed: bool,
    /// What the type makes of `4294967295`, printed back as an unsigned number.
    ///
    /// Written out per row rather than computed from `size` and `signed` inside this
    /// file, because a table computed inside the test would be a second
    /// implementation of C's conversion rules and could be wrong in the same way the
    /// frontend is. A literal cannot be wrong that way.
    holds_max: u64,
}

/// The base integer types and their multi-word spellings.
///
/// C leaves `char`'s sign implementation-defined; this platform makes it signed, as
/// `docs/c-language.md` records. It is listed so the matrix covers the widths as well
/// as the signs, and so a change to that decision would be a visible test failure
/// rather than a silent one.
const SPELLINGS: &[Spelling] = &[
    // One byte. `4294967295` truncates to `0xFF`, and a *signed* one-byte type holds
    // that as -1 — so widening it to `unsigned long` gives all ones, while an
    // unsigned one gives 255. The two measurements finally differ at one byte too.
    Spelling {
        declaration: "char",
        size: 1,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "signed char",
        size: 1,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "unsigned char",
        size: 1,
        signed: false,
        holds_max: 255,
    },
    // Two bytes. `0xFFFF` is -1 signed (so all ones once extended) and 65535
    // unsigned — the two measurements finally differ.
    Spelling {
        declaration: "short",
        size: 2,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "short int",
        size: 2,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "signed short",
        size: 2,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "signed short int",
        size: 2,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "unsigned short",
        size: 2,
        signed: false,
        holds_max: 65_535,
    },
    Spelling {
        declaration: "unsigned short int",
        size: 2,
        signed: false,
        holds_max: 65_535,
    },
    // Four bytes signed. `0xFFFFFFFF` is -1, so extending it gives all ones. A
    // four-byte *signed* type that reads back as 4294967295 is not extending.
    // `signed` with no `int` is the spelling that became a one-byte `char` before
    // the fix.
    Spelling {
        declaration: "signed",
        size: 4,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "signed int",
        size: 4,
        signed: true,
        holds_max: u64::MAX,
    },
    Spelling {
        declaration: "int",
        size: 4,
        signed: true,
        holds_max: u64::MAX,
    },
    // Four bytes unsigned. The value is kept, and this is the spelling that was
    // parsed as signed `int` before the fix.
    Spelling {
        declaration: "unsigned",
        size: 4,
        signed: false,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "unsigned int",
        size: 4,
        signed: false,
        holds_max: 4_294_967_295,
    },
    // Eight bytes. The value fits, so it stays 4294967295 whatever the sign — and
    // `long int` and `unsigned long int` are the spellings that lost their width,
    // so if the width comes back as 4 the `sizeof` case catches it and this one
    // catches the sign.
    Spelling {
        declaration: "long",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "long int",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "signed long",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "signed long int",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "unsigned long",
        size: 8,
        signed: false,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "unsigned long long",
        size: 8,
        signed: false,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "long long",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "long long int",
        size: 8,
        signed: true,
        holds_max: 4_294_967_295,
    },
    Spelling {
        declaration: "unsigned long long int",
        size: 8,
        signed: false,
        holds_max: 4_294_967_295,
    },
];

/// Prints `value` as an unsigned decimal, and returns 0.
///
/// The runtime's `print_decimal` takes a **signed** `long`, so casting an unsigned
/// value into it prints `-1` for `u64::MAX` — which is correct for the printer and
/// useless here, because every measurement in this file is about a value whose high
/// bit matters. So this file brings its own unsigned printer, and
/// `the_unsigned_printer_agrees_with_c` below checks it against values whose answer
/// is known before anything relies on it.
fn print_as_unsigned_source() -> String {
    String::from(
        "static void show(unsigned long value) { \
           char digits[24]; \
           unsigned long at = 0; \
           unsigned long started = 0; \
           while (value > 0) { \
             unsigned long rest = value / 10; \
             digits[at] = (char)(48 + (value - rest * 10)); \
             at = at + 1; \
             value = rest; \
             started = 1; \
           } \
           if (started == 0) { putchar(48); } \
           while (at > 0) { at = at - 1; putchar((int) digits[at]); } \
           putchar(10); \
         }\n",
    )
}

#[test]
fn the_unsigned_printer_agrees_with_c() {
    // Before the measurements below are trusted, the printer is checked against
    // values whose decimal form is not in question. A wrong printer would make every
    // signedness expectation in this file wrong in the same direction, which is the
    // failure mode a shared helper has.
    for (value, expected) in [
        (0u64, "0"),
        (1, "1"),
        (9, "9"),
        (10, "10"),
        (99, "99"),
        (255, "255"),
        (65_535, "65535"),
        (4_294_967_295, "4294967295"),
        (9_223_372_036_854_775_807, "9223372036854775807"),
        (u64::MAX, "18446744073709551615"),
    ] {
        let (output, status) = run_c(&format!(
            "{printer}int main() {{ show({value}ul); return 0; }}",
            printer = print_as_unsigned_source()
        ));
        assert_eq!(status, 0);
        assert_eq!(
            output.trim(),
            expected,
            "the unsigned printer printed {output:?} for {value}"
        );
    }
}

#[test]
fn the_matrix_is_the_size_it_claims_to_be() {
    // A table that quietly lost rows is a matrix that quietly stopped covering, so
    // the row count is asserted rather than trusted.
    assert_eq!(SPELLINGS.len(), 23, "the spelling matrix changed size");
    // And every declaration is distinct, because a duplicate row would make the
    // count look right while covering one spelling twice.
    for (index, spelling) in SPELLINGS.iter().enumerate() {
        for other in &SPELLINGS[index + 1..] {
            assert_ne!(
                spelling.declaration, other.declaration,
                "`{}` appears twice in the matrix",
                spelling.declaration
            );
        }
    }
}

#[test]
fn every_spelling_has_c_size() {
    for spelling in SPELLINGS {
        let (output, status) = run_c(&format!(
            "int main() {{ return sizeof({declaration}); }}",
            declaration = spelling.declaration
        ));
        assert_eq!(
            output, "",
            "`{}` should print nothing",
            spelling.declaration
        );
        assert_eq!(
            status,
            spelling.size,
            "sizeof({declaration}) is {size} in C, and the frontend said {status}",
            declaration = spelling.declaration,
            size = spelling.size
        );
    }
}

#[test]
fn every_spelling_has_c_signedness() {
    for spelling in SPELLINGS {
        let (output, status) = run_c(&format!(
            "{printer}int main() {{ \
             {declaration} value = ({declaration}) 4294967295; \
             show((unsigned long) value); \
             return 0; }}",
            printer = print_as_unsigned_source(),
            declaration = spelling.declaration
        ));
        assert_eq!(status, 0, "`{}` should return 0", spelling.declaration);
        let printed: u64 = output.trim().parse().unwrap_or_else(|_| {
            panic!(
                "`{}` printed {output:?}, which is not a number",
                spelling.declaration
            )
        });
        assert_eq!(
            printed,
            spelling.holds_max,
            "`{declaration}` holding 4294967295 printed {printed}, and C says {expected}",
            declaration = spelling.declaration,
            expected = spelling.holds_max
        );
    }
}

#[test]
fn every_spelling_compares_signed_the_way_c_says_it_does() {
    // A third measurement, and the one that uses `signed` directly rather than
    // reading it back out of a magnitude: an all-ones value is *negative* in a
    // signed type and the largest positive value in an unsigned one, so `< 0`
    // separates them.
    //
    // It fails differently from the other two. A type with the right width and the
    // wrong sign passes `sizeof`; a type with the right sign and the wrong width
    // passes the magnitude test above whenever the truncation happens to land the
    // same way. Neither of those fails here.
    for spelling in SPELLINGS {
        let (output, status) = run_c(&format!(
            "int main() {{ \
             {declaration} value = ({declaration}) -1; \
             if (value < 0) {{ print(\"NEG\"); }} else {{ print(\"POS\"); }} \
             return 0; }}",
            declaration = spelling.declaration
        ));
        assert_eq!(status, 0, "`{}` should return 0", spelling.declaration);
        assert_eq!(
            output.trim(),
            if spelling.signed { "NEG" } else { "POS" },
            "`{}` holding -1 compared `< 0` the wrong way for a {} type",
            spelling.declaration,
            if spelling.signed {
                "signed"
            } else {
                "unsigned"
            }
        );
    }
}
#[test]
fn a_negative_value_widens_to_every_wider_spelling() {
    // The other direction: a signed value stored in a wider type must *extend*, not
    // zero-fill. This is the defect the first hardening test found, and it is worth
    // having for every signed width rather than only for `int`, because the widening
    // code is shared and a fix to it could regress a width it is not tested at.
    for declaration in ["short", "int", "long"] {
        let (output, status) = run_c(&format!(
            "int main() {{ \
             {declaration} small = ({declaration}) -1; \
             unsigned long wide = (unsigned long) small; \
             return (int) (wide == {expected} ? 0 : 1); }}",
            declaration = declaration,
            expected = ALL_ONES
        ));
        assert_eq!(
            status, 0,
            "`{declaration}` -1 should sign-extend to all ones when widened to \
             unsigned long, and it did not"
        );
        assert_eq!(output, "");
    }
}

#[test]
fn an_unsigned_value_widens_to_every_wider_spelling() {
    // And the other half of widening: an *unsigned* value widened must zero-fill. A
    // frontend that sign-extended everything would pass the test above and fail this
    // one, which is the pair that makes "widen correctly" mean something.
    for declaration in [
        "unsigned char",
        "unsigned short",
        "unsigned int",
        "unsigned long",
    ] {
        let (output, status) = run_c(&format!(
            "int main() {{ \
             {declaration} small = ({declaration}) -1; \
             unsigned long wide = (unsigned long) small; \
             return (int) (wide == {expected} ? 0 : 1); }}",
            declaration = declaration,
            expected = mask_literal(declaration)
        ));
        assert_eq!(
            status,
            0,
            "`{declaration}` -1 should zero-extend to {expected} when widened, and it did not",
            expected = mask_for(declaration)
        );
        assert_eq!(output, "");
    }
}

/// The value an all-ones unsigned type widens to.
///
/// An eight-byte type is its own answer, so the shift is guarded rather than
/// performed: `1u64 << 64` does not exist, and a test that overflows a shift while
/// computing an expected value has taken the same kind of shortcut that made the
/// arithmetic model wrong in the first place.
fn mask_for(declaration: &str) -> u64 {
    let size = SPELLINGS
        .iter()
        .find(|spelling| spelling.declaration == declaration)
        .map(|spelling| spelling.size)
        .unwrap_or_else(|| panic!("`{declaration}` is not in the matrix"));
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

/// The C spelling of `u64::MAX`.
///
/// A literal this large has to carry an `l` suffix, and it is written out once so
/// that the two tests using it cannot drift apart — they are asking the same
/// question about different types.
const ALL_ONES: &str = "18446744073709551615ul";

/// The C spelling of the value an all-ones unsigned type of this width widens to.
fn mask_literal(declaration: &str) -> String {
    format!("{}ul", mask_for(declaration))
}
