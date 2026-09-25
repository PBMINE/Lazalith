use lazalith_toolchain::{ObjectFile, RelocationKind, SectionKind, assemble_named};

const SOURCE: &str = r#".arch lz64
.entry _start
.global _start
.section .rodata
message:
    .ascii "Hello, Lazalith!\n"
.section .text
_start:
    LI r0, 2
    LI r1, 1
    LI r2, message
    LI r3, 17
    LI r4, io_result
    LI r5, 0
    SYSCALL
    LI r0, 1
    LI r1, 0
    SYSCALL
.section .bss
io_result:
    .zero 16
"#;

#[test]
fn assembler_emits_sections_symbols_relocations_and_debug() {
    let object = assemble_named("hello.lzs", SOURCE).unwrap();
    assert_eq!(object.sections().len(), 3);
    assert!(
        object
            .sections()
            .iter()
            .any(|section| section.kind() == SectionKind::ReadOnlyData)
    );
    assert!(
        object
            .sections()
            .iter()
            .any(|section| section.kind() == SectionKind::Bss)
    );
    assert_eq!(object.symbols().len(), 3);
    assert_eq!(object.relocations().len(), 2);
    assert!(
        object
            .relocations()
            .iter()
            .all(|relocation| relocation.kind() == RelocationKind::LiImmediate)
    );
    assert_eq!(object.debug_mappings().len(), 10);
    let bytes = object.to_bytes().unwrap();
    assert_eq!(ObjectFile::from_bytes(&bytes).unwrap(), object);
}

#[test]
fn assembler_reports_unknown_symbols_and_bad_directives() {
    let error = assemble_named(
        "bad.lzs",
        ".arch lz64\n.entry _start\n_start:\n LI r0, missing\n",
    )
    .unwrap_err();
    let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
        panic!("expected assembly diagnostic");
    };
    assert_eq!(error.diagnostic().unwrap().code().as_str(), "E300");
    assert!(assemble_named("bad.lzs", ".arch lz64\n.entry _start\n.unknown\n").is_err());
}

#[test]
fn assembler_emits_all_relocation_kinds() {
    let source = ".arch lz64\n.entry _start\n.extern ext\n.section .rodata\nro:\n.word ext\n.pcrelword ext\n.dword ext\n.section .text\n_start:\nBR AL, target\nCALL target\nLI r0, ext\nLDZ r1, [r2 + ext], BYTE\nSYSCALL\ntarget:\nNOP\n";
    let object = assemble_named("relocs.lzs", source).unwrap();
    let kinds: Vec<_> = object
        .relocations()
        .iter()
        .map(|relocation| relocation.kind())
        .collect();
    assert!(kinds.contains(&RelocationKind::AbsoluteWord32));
    assert!(kinds.contains(&RelocationKind::AbsoluteWord64));
    assert!(kinds.contains(&RelocationKind::PcRelativeWord32));
    assert!(kinds.contains(&RelocationKind::PcRelativeBranch));
    assert!(kinds.contains(&RelocationKind::LiImmediate));
    assert!(kinds.contains(&RelocationKind::MemoryDisplacement32));
    assert_eq!(
        ObjectFile::from_bytes(&object.to_bytes().unwrap()).unwrap(),
        object
    );
}

#[test]
fn assembler_rejects_malformed_and_order_sensitive_inputs() {
    assert!(
        assemble_named(
            "memory.lzs",
            ".arch lz64\n.entry _start\n_start:\n LDZ r0,\n"
        )
        .is_err()
    );
    let error = assemble_named(
        "negative.lzs",
        ".arch lz64\n.entry _start\n_start:\n LI r0, -missing\n",
    )
    .unwrap_err();
    let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
        panic!("expected assembly diagnostic");
    };
    assert_eq!(error.diagnostic().unwrap().code().as_str(), "E306");
    let error = assemble_named(
        "dword.lzs",
        ".section .data\n.dword 1\n.arch lz32\n.entry _start\n.section .text\n_start:\n NOP\n",
    )
    .unwrap_err();
    let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
        panic!("expected assembly diagnostic");
    };
    assert_eq!(error.diagnostic().unwrap().code().as_str(), "E266");
    assert!(assemble_named("align.lzs", ".arch lz64\n.section .data\n.byte 1\n.align 0x8000000000000000\n.entry _start\n_start:\n NOP\n").is_err());
}

#[test]
fn assembler_reports_out_of_range_expression_arithmetic() {
    for (source, code) in [
        (
            ".arch lz64\n.entry _start\n_start:\n LI r0, target - 9223372036854775808\n.target target\n",
            "E296",
        ),
        (
            ".arch lz64\n.entry _start\n_start:\n LI r0, -170141183460469231731687303715884105728\n",
            "E215",
        ),
    ] {
        let error = assemble_named("range.lzs", source).unwrap_err();
        let lazalith_toolchain::ToolchainError::Assembly(error) = error else {
            panic!("expected assembly diagnostic");
        };
        assert_eq!(error.diagnostic().unwrap().code().as_str(), code);
    }
}
