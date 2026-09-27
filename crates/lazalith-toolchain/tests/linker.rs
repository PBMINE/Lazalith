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
    // The canonical image is compared section by section rather than byte for
    // byte, because it no longer can be compared byte for byte: this object was
    // assembled from a named source, so its image carries a debug block saying
    // so, and the canonical image carries none. What has to match is the code
    // the linker produced and the memory it asks for, which is what this test is
    // about.
    let linked = program.image();
    let canonical = build_init_image(LzxArchitecture::Lz32).unwrap();
    assert_eq!(
        linked.sections().len(),
        canonical.sections().len(),
        "the linker did not add or drop a section"
    );
    for (linked, canonical) in linked.sections().iter().zip(canonical.sections()) {
        assert_eq!(
            linked.bytes(),
            canonical.bytes(),
            "a section's bytes differ"
        );
        assert_eq!(linked.kind(), canonical.kind(), "a section's kind differs");
        assert_eq!(
            linked.virtual_offset(),
            canonical.virtual_offset(),
            "a section's address differs"
        );
    }
    assert_eq!(linked.required_data(), canonical.required_data());
    assert_eq!(linked.required_stack(), canonical.required_stack());
    // And the image that can be read back still is, debug block and all.
    let round_tripped = LzxImage::from_bytes(&linked.to_bytes().unwrap()).unwrap();
    assert_eq!(&round_tripped, linked);
    assert_eq!(
        round_tripped.debug().map(|debug| debug.files()[0].name()),
        Some("exit.lzs"),
        "the image names the object it was linked from"
    );
    assert!(
        canonical.debug().is_none(),
        "the canonical image is built without debug information"
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
fn an_entry_in_a_later_object_gets_its_offset_in_the_whole_code_section() {
    // The image has one code section built by concatenating every object's text,
    // so an entry in the second object sits at a non-zero offset inside it. The
    // offset is measured from the bottom of the code region, not from the entry
    // object's own slice of it: measured from the object, the offset would be
    // zero and the image would start at whatever code happened to be first.
    let library = assemble_named(
        "library.lzs",
        ".arch lz64\n.entry _start\n_start:\n NOP\n NOP\n",
    )
    .unwrap();
    let program_object = assemble_named(
        "program.lzs",
        ".arch lz64\n.entry main\n.global main\nmain:\n LI r0, 1\n SYSCALL\n",
    )
    .unwrap();
    let options = LinkOptions {
        entry_symbol: Some(String::from("main")),
    };
    let program = link_objects(&[library, program_object], &options).unwrap();
    assert_eq!(program.entry_symbol(), "main");
    let entry_offset = program.entry_offset();
    assert_eq!(
        entry_offset, 16,
        "`main` follows the library's two instructions"
    );
    // The offset has to name `main` in the linked image, not merely be non-zero,
    // so the instruction the loader would fetch first is checked.
    let code = program.image().sections()[0].bytes();
    let at_entry = code
        .get(entry_offset as usize..entry_offset as usize + 8)
        .expect("the entry offset is inside the code section");
    assert_eq!(
        decode(ArchitectureConfig::lz64(), at_entry).unwrap(),
        Instruction::new(
            ArchitectureConfig::lz64(),
            Opcode::Li,
            &[
                Operand::Register(RegisterIndex::try_from(0).unwrap()),
                Operand::Immediate(1),
            ],
        )
        .unwrap()
    );
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

#[test]

fn linker_writes_the_documented_value_for_every_relocation_kind() {
    for config in [ArchitectureConfig::lz64(), ArchitectureConfig::lz32()] {
        let dword = if config == ArchitectureConfig::lz64() {
            ".dword target\n"
        } else {
            ""
        };
        let source = format!(
            ".arch {arch}\n.entry _start\n.section .rodata\nro:\n.word target\n.pcrelword target\n{dword}.section .text\n_start:\nBR AL, target\nCALL target\nLI r0, target\nLDZ r1, [r2 + target], BYTE\nSYSCALL\ntarget:\nNOP\n",
            arch = if config == ArchitectureConfig::lz32() {
                "lz32"
            } else {
                "lz64"
            }
        );
        let name = if config == ArchitectureConfig::lz32() {
            "all-relocations-lz32.lzs"
        } else {
            "all-relocations-lz64.lzs"
        };
        let object = assemble_named(name, &source).unwrap();
        let program = link_objects(&[object], &LinkOptions::default()).unwrap();
        let code = program.image().sections()[0].bytes();
        let data = program.image().sections()[1].bytes();
        let code_base = 0x0020_0000u64;
        let data_base = 0x0030_0000u64;
        let instruction = 8u64;
        let word_field = 4u64;
        let text_size = code.len() as u64;
        let target = code_base + text_size - instruction;

        let branch = decode(config, &code[..8]).unwrap();
        assert_eq!(branch.opcode(), Opcode::Br);
        assert_eq!(
            branch.operands()[1],
            Operand::Immediate(((target - (code_base + instruction)) / 4) as i32)
        );
        let call = decode(config, &code[8..16]).unwrap();
        assert_eq!(call.opcode(), Opcode::Call);
        assert_eq!(
            call.operands()[0],
            Operand::Immediate(((target - (code_base + instruction * 2)) / 4) as i32)
        );
        let load = decode(config, &code[16..24]).unwrap();
        assert_eq!(load.opcode(), Opcode::Li);
        assert_eq!(load.operands()[1], Operand::Immediate(target as i32));
        let memory = decode(config, &code[24..32]).unwrap();
        assert_eq!(memory.opcode(), Opcode::Ldz);
        assert_eq!(
            memory.operands()[1],
            Operand::Memory {
                base: RegisterIndex::try_from(2).unwrap(),
                displacement: target as i32,
            }
        );

        assert_eq!(
            u64::from(u32::from_le_bytes(data[0..4].try_into().unwrap())),
            target,
            "AbsoluteWord32 stores the symbol runtime address"
        );
        assert_eq!(
            i64::from(i32::from_le_bytes(data[4..8].try_into().unwrap())),
            i64::try_from(target).unwrap() - i64::try_from(data_base + word_field).unwrap(),
            "PcRelativeWord32 is relative to the relocated field"
        );
        if config == ArchitectureConfig::lz64() {
            assert_eq!(
                u64::from_le_bytes(data[8..16].try_into().unwrap()),
                target,
                "AbsoluteWord64 stores the symbol runtime address"
            );
        }
    }
}

#[test]
fn linker_rejects_relocations_it_cannot_represent() {
    let narrow = assemble_named(
        "narrow.lzs",
        ".arch lz32\n.entry _start\n_start:\nLI r0, 0x1_0000_0000\n",
    );
    assert!(narrow.is_err());
    let misaligned = assemble_named(
        "misaligned.lzs",
        ".arch lz64\n.entry _start\n.section .text\n_start:\nBR AL, target\n.section .rodata\n.byte 0\ntarget:\nNOP\n",
    );
    assert!(misaligned.is_err());
    let overflow = assemble_named(
        "overflow.lzs",
        ".arch lz64\n.entry _start\n_start:\nBR AL, target\n.section .rodata\n.zero 0x40000000\ntarget:\nNOP\n",
    );
    assert!(overflow.is_err());
}
