use lazalith_isa::{Instruction, Opcode, Operand, decode};
use lazalith_os::{LzxArchitecture, LzxImage, build_init_image};
use lazalith_toolchain::{LinkOptions, assemble_named, link_objects};
use lazalith_types::{ArchitectureConfig, RegisterIndex};

#[test]
fn linker_resolves_global_relocations_across_objects() {
    let root = assemble_named(
        "root.lzs",
        ".arch lz64\n.entry _start\n.extern ext\n_start:\n LI r0, ext\n LI r1, 1\n SYSCALL\n",
    )
    .unwrap();
    let library = assemble_named(
        "library.lzs",
        ".arch lz64\n.entry ext\n.global ext\next:\n NOP\n",
    )
    .unwrap();
    let program = link_objects(&[root, library], &LinkOptions::default()).unwrap();
    assert_eq!(program.entry_symbol(), "_start");
    assert_eq!(program.entry_offset(), 0);
    let bytes = program.image().sections()[0].bytes();
    let instruction = decode(ArchitectureConfig::lz64(), &bytes[..8]).unwrap();
    assert_eq!(
        instruction,
        Instruction::new(
            ArchitectureConfig::lz64(),
            Opcode::Li,
            &[
                Operand::Register(RegisterIndex::try_from(0).unwrap()),
                Operand::Immediate(0x0020_0000 + 24),
            ],
        )
        .unwrap()
    );
}

#[test]
fn linker_preserves_single_object_bridge_bytes() {
    let object = assemble_named(
        "exit.lzs",
        ".arch lz32\n.entry _start\n_start:\n LI r0, 1\n SYSCALL\n",
    )
    .unwrap();
    let program = link_objects(&[object], &LinkOptions::default()).unwrap();
    assert_eq!(program.entry_offset(), 0);
    assert_eq!(
        program.image().to_bytes().unwrap(),
        build_init_image(LzxArchitecture::Lz32)
            .unwrap()
            .to_bytes()
            .unwrap()
    );
}

#[test]
fn linker_rejects_incompatible_or_undefined_objects() {
    let lz32 = assemble_named("a.lzs", ".arch lz32\n.entry _start\n_start:\n NOP\n").unwrap();
    let lz64 = assemble_named("b.lzs", ".arch lz64\n.entry _start\n_start:\n NOP\n").unwrap();
    assert!(link_objects(&[lz32.clone(), lz64], &LinkOptions::default()).is_err());
    let undefined = assemble_named(
        "undefined.lzs",
        ".arch lz64\n.entry _start\n.extern missing\n_start:\n LI r0, missing\n SYSCALL\n",
    )
    .unwrap();
    assert!(link_objects(&[undefined], &LinkOptions::default()).is_err());
}

#[test]
fn linker_patches_branches_calls_memory_and_data_relocations() {
    let source = ".arch lz64\n.entry _start\n.section .rodata\nro:\n.word target\n.pcrelword target\n.dword target\n.section .text\n_start:\nBR AL, target\nCALL target\nLI r0, target\nLDZ r1, [r2 + target], BYTE\nSYSCALL\ntarget:\nNOP\n";
    let object = assemble_named("all-relocations.lzs", source).unwrap();
    let program = link_objects(&[object], &LinkOptions::default()).unwrap();
    let bytes = program.image().to_bytes().unwrap();
    assert_eq!(
        LzxImage::from_bytes(&bytes).unwrap().to_bytes().unwrap(),
        bytes
    );
    let code = program.image().sections()[0].bytes();
    assert_eq!(
        decode(ArchitectureConfig::lz64(), &code[..8])
            .unwrap()
            .opcode(),
        Opcode::Br
    );
    assert_eq!(
        decode(ArchitectureConfig::lz64(), &code[8..16])
            .unwrap()
            .opcode(),
        Opcode::Call
    );
}

#[test]
fn linker_emits_data_section_for_bss_only_objects() {
    let object = assemble_named(
        "bss-only.lzs",
        ".arch lz64\n.entry _start\n.section .bss\nresult:\n.zero 16\n.section .text\n_start:\nLI r0, 1\nSYSCALL\n",
    )
    .unwrap();
    let program = link_objects(&[object], &LinkOptions::default()).unwrap();
    assert_eq!(program.image().sections().len(), 3);
    assert_eq!(program.image().sections()[1].virtual_size(), 0);
}

#[test]
fn linker_accounts_for_alignment_gaps_between_bss_sections() {
    let first = assemble_named(
        "first.lzs",
        ".arch lz64\n.entry _start\n.section .bss\n.align 16\na_slot:\n.zero 8\n.section .text\n_start:\n LI r0, 1\n SYSCALL\n",
    )
    .unwrap();
    let second = assemble_named(
        "second.lzs",
        ".arch lz64\n.entry patch\n.section .bss\n.align 16\nb_slot:\n.zero 8\n.section .text\npatch:\n LI r0, b_slot\n NOP\n",
    )
    .unwrap();
    let program = link_objects(&[first, second], &LinkOptions::default()).unwrap();
    let code = program.image().sections()[0].bytes();
    let patched = decode(ArchitectureConfig::lz64(), &code[16..24]).unwrap();
    assert!(matches!(
        patched.operands()[1],
        Operand::Immediate(0x0030_0010)
    ));
    assert_eq!(program.image().sections()[2].virtual_size(), 24);
    assert_eq!(program.image().sections()[2].virtual_offset(), 0);
    assert_eq!(program.image().required_data(), 24);
}

#[test]
fn linker_honors_data_and_bss_alignment() {
    let object = assemble_named(
        "aligned.lzs",
        ".arch lz64\n.entry _start\n.section .data\n.byte 1\n.align 16\nvalue:\n.word value\n.section .bss\n.align 16\nslot:\n.zero 8\n.section .text\n_start:\nLI r0, value\nSYSCALL\n",
    )
    .unwrap();
    let program = link_objects(&[object], &LinkOptions::default()).unwrap();
    let code = program.image().sections()[0].bytes();
    let instruction = decode(ArchitectureConfig::lz64(), &code[..8]).unwrap();
    assert!(matches!(
        instruction.operands()[1],
        Operand::Immediate(0x0030_0010)
    ));
    assert_eq!(program.image().sections()[2].virtual_offset() % 16, 0);
}
