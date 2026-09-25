use lazalith_isa::{Condition, ControlRegister, DataSize, Instruction, Opcode, Operand, encode};
use lazalith_toolchain::{assemble_named, disassemble, disassemble_object};
use lazalith_types::{ArchitectureConfig, RegisterIndex};

#[test]
fn disassembler_formats_and_round_trips_instructions() {
    let config = ArchitectureConfig::lz64();
    let instructions = [
        Instruction::new(
            config,
            Opcode::Li,
            &[
                Operand::Register(RegisterIndex::try_from(0).unwrap()),
                Operand::Immediate(7),
            ],
        )
        .unwrap(),
        Instruction::new(
            config,
            Opcode::Br,
            &[Operand::Condition(Condition::Al), Operand::Immediate(0)],
        )
        .unwrap(),
        Instruction::new(
            config,
            Opcode::Ldz,
            &[
                Operand::Register(RegisterIndex::try_from(1).unwrap()),
                Operand::Memory {
                    base: RegisterIndex::try_from(2).unwrap(),
                    displacement: -4,
                },
                Operand::DataSize(DataSize::Byte),
            ],
        )
        .unwrap(),
        Instruction::new(config, Opcode::Syscall, &[]).unwrap(),
    ];
    let mut bytes = Vec::new();
    for instruction in &instructions {
        bytes.extend_from_slice(&encode(config, instruction).unwrap());
    }
    let result = disassemble(config, &bytes).unwrap();
    assert_eq!(result.len(), 4);
    assert_eq!(result[0].text(), "LI r0, 7");
    assert_eq!(result[1].text(), "BR AL, 0");
    assert_eq!(result[2].text(), "LDZ r1, [r2-4], BYTE");
    assert_eq!(result[3].text(), "SYSCALL");
    let round_trip = assemble_named(
        "round-trip.lzs",
        &format!(".arch lz64\n.entry _start\n_start:\n{}\n", result[0].text()),
    )
    .unwrap();
    assert_eq!(round_trip.code(), &bytes[..8]);
    for (index, instruction) in instructions.iter().enumerate() {
        assert_eq!(result[index].instruction(), *instruction);
    }
}

#[test]
fn disassembler_rejects_incomplete_and_noncanonical_bytes() {
    let config = ArchitectureConfig::lz64();
    let instruction = Instruction::new(config, Opcode::Nop, &[]).unwrap();
    let mut bytes = encode(config, &instruction).unwrap().to_vec();
    assert!(disassemble(config, &bytes[..7]).is_err());
    bytes[7] |= 1;
    assert!(disassemble(config, &bytes).is_err());
}

#[test]
fn disassembler_reads_text_from_an_object() {
    let object = assemble_named(
        "hello.lzs",
        ".arch lz64\n.entry _start\n_start:\n NOP\n SYSCALL\n",
    )
    .unwrap();
    let sections = disassemble_object(&object).unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].instructions().len(), 2);
    assert_eq!(sections[0].instructions()[0].text(), "NOP");
}

#[test]
fn every_printed_instruction_reassembles_to_identical_bytes() {
    for config in [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()] {
        let arch = if config == ArchitectureConfig::lz32() {
            "lz32"
        } else {
            "lz64"
        };
        let mut source = format!(".arch {arch}\n.entry _start\n_start:\n");
        let mut expected = Vec::new();
        for opcode in Opcode::ALL {
            let operands = sample_operands(*opcode, config);
            let Ok(instruction) = Instruction::new(config, *opcode, &operands) else {
                continue;
            };
            let Ok(bytes) = encode(config, &instruction) else {
                continue;
            };
            let printed = disassemble(config, &bytes).unwrap()[0].text().to_owned();
            source.push_str(&format!("{printed}\n"));
            expected.extend_from_slice(&bytes);
        }
        let object = assemble_named("every-form.lzs", &source).unwrap();
        assert_eq!(
            object.code(),
            &expected,
            "{arch}: every printed form must re-assemble"
        );
        let sections = disassemble_object(&object).unwrap();
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].section(), 0);
        assert_eq!(sections[0].instructions().len(), expected.len() / 8);
    }
}

fn sample_operands(opcode: Opcode, _config: ArchitectureConfig) -> Vec<Operand> {
    let register = RegisterIndex::try_from(1).unwrap();
    let mut operands = Vec::new();
    for definition in opcode.definition().operands() {
        let operand = match definition.kind {
            lazalith_isa::OperandKind::Register => Operand::Register(register),
            lazalith_isa::OperandKind::Immediate => Operand::Immediate(-4),
            lazalith_isa::OperandKind::Memory => Operand::Memory {
                base: register,
                displacement: -8,
            },
            lazalith_isa::OperandKind::DataSize => Operand::DataSize(DataSize::Word),
            lazalith_isa::OperandKind::Condition => Operand::Condition(Condition::Ne),
            lazalith_isa::OperandKind::Control => Operand::Control(ControlRegister::Estatus),
        };
        operands.push(operand);
    }
    operands
}
