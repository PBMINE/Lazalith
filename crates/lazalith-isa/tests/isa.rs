use lazalith_isa::*;
use lazalith_types::{ArchitectureConfig, FeatureSet, RegisterIndex, WordWidth};
use std::error::Error;

const MODES: [ArchitectureConfig; 2] = [ArchitectureConfig::lz32(), ArchitectureConfig::lz64()];
const IMMEDIATES: [i32; 9] = [
    i32::MIN,
    i32::MIN + 1,
    -4,
    -2,
    -1,
    0,
    1,
    0x12345678,
    i32::MAX,
];

fn reg(value: u8) -> Operand {
    Operand::Register(RegisterIndex::try_from(value).unwrap())
}

fn format_name(format: InstructionFormat) -> &'static str {
    match format {
        InstructionFormat::Z => "Z",
        InstructionFormat::D => "D",
        InstructionFormat::A => "A",
        InstructionFormat::Da => "DA",
        InstructionFormat::Dab => "DAB",
        InstructionFormat::Ab => "AB",
        InstructionFormat::Di => "DI",
        InstructionFormat::Dai => "DAI",
        InstructionFormat::Mem => "MEM",
        InstructionFormat::Br => "BR",
        InstructionFormat::Imm => "IMM",
        InstructionFormat::Dx => "DX",
        InstructionFormat::Ax => "AX",
    }
}

#[test]
fn metadata_matches_entire_design_opcode_table() {
    let design = include_str!("../../../docs/isa.md");
    let mut seen = Vec::new();
    for line in design.lines() {
        let cells: Vec<_> = line.split('|').map(str::trim).collect();
        if cells.len() != 7 || cells[1].len() != 2 {
            continue;
        }
        let Ok(byte) = u8::from_str_radix(cells[1], 16) else {
            continue;
        };
        let opcode = Opcode::try_from(byte).unwrap();
        let definition = opcode.definition();
        assert_eq!(definition.opcode, opcode);
        assert_eq!(definition.mnemonic, cells[2]);
        assert_eq!(format_name(definition.format), cells[3]);
        assert_eq!(
            definition.supervisor_only,
            cells[4].contains("Supervisor only")
        );
        let nzcv = match cells[5] {
            "Preserve" | "Preserve until entry" => NzcvEffect::Preserve,
            "Add rules" => NzcvEffect::Add,
            "Sub rules" => NzcvEffect::Subtract,
            "NZ; C=V=0" => NzcvEffect::ResultNzClearCv,
            "Restore saved control state" => NzcvEffect::Restore,
            other => panic!("unexpected NZCV {other}"),
        };
        assert_eq!(definition.nzcv, nzcv);
        assert_eq!(definition.required_features(), FeatureSet::base_v1());
        let meaning = match byte {
            0x02 | 0x11 | 0x13 => ImmediateMeaning::SignedWord,
            0x30..=0x32 => ImmediateMeaning::ByteDisplacement,
            0x40 | 0x42 => ImmediateMeaning::RelativeDisplacement,
            0x51 => ImmediateMeaning::TrapPayload,
            _ => ImmediateMeaning::None,
        };
        assert_eq!(definition.immediate, meaning);
        assert!(!seen.contains(&opcode));
        seen.push(opcode);
    }
    assert_eq!(seen.len(), 40);
    assert_eq!(seen, Opcode::ALL);
    for format in InstructionFormat::ALL {
        assert!(
            seen.iter()
                .any(|opcode| opcode.definition().format == *format)
        );
    }
}

fn reference_mask(format: InstructionFormat) -> u64 {
    match format {
        InstructionFormat::Z => 0xff,
        InstructionFormat::D => 0xfff,
        InstructionFormat::A => 0xf0ff,
        InstructionFormat::Da => 0xffff,
        InstructionFormat::Dab => 0xfffff,
        InstructionFormat::Ab => 0xff0ff,
        InstructionFormat::Di => 0xffff_ffff_0000_0fff,
        InstructionFormat::Dai => 0xffff_ffff_0000_ffff,
        InstructionFormat::Mem => 0xffff_ffff_00f0_ffff,
        InstructionFormat::Br => 0xffff_ffff_00f0_00ff,
        InstructionFormat::Imm => 0xffff_ffff_0000_00ff,
        InstructionFormat::Dx => 0xf0_0fff,
        InstructionFormat::Ax => 0xf0_f0ff,
    }
}

#[test]
fn layouts_cover_exact_fields_without_overlap() {
    assert_eq!(InstructionFormat::ALL.len(), 13);
    for format in InstructionFormat::ALL {
        let mut mask = 0xff;
        for operand in format.operands() {
            for field in operand.fields {
                assert_eq!(mask & field.mask(), 0);
                mask |= field.mask();
            }
        }
        assert_eq!(mask, reference_mask(*format));
        assert_eq!(format.used_mask(), mask);
        assert!(format.operands().len() <= 3);
    }
}

fn reference_bytes(opcode: Opcode, operands: &[Operand]) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0] = opcode.as_u8();
    let mut register_values = Vec::new();
    for operand in operands {
        match *operand {
            Operand::Register(index) => register_values.push(index.as_u8()),
            Operand::Immediate(value) => bytes[4..].copy_from_slice(&value.to_le_bytes()),
            Operand::Memory { base, displacement } => {
                register_values.push(base.as_u8());
                bytes[4..].copy_from_slice(&displacement.to_le_bytes());
            }
            Operand::DataSize(size) => bytes[2] = size.as_u8() * 16,
            Operand::Condition(condition) => bytes[2] = condition.as_u8() * 16,
            Operand::Control(control) => bytes[2] = control.as_u8() * 16,
        }
    }
    match opcode.definition().format {
        InstructionFormat::D | InstructionFormat::Di | InstructionFormat::Dx => {
            bytes[1] = register_values[0]
        }
        InstructionFormat::A | InstructionFormat::Ax => bytes[1] = register_values[0] * 16,
        InstructionFormat::Da | InstructionFormat::Dai | InstructionFormat::Mem => {
            bytes[1] = register_values[0] + register_values[1] * 16
        }
        InstructionFormat::Dab => {
            bytes[1] = register_values[0] + register_values[1] * 16;
            bytes[2] = register_values[2];
        }
        InstructionFormat::Ab => {
            bytes[1] = register_values[0] * 16;
            bytes[2] = register_values[1];
        }
        InstructionFormat::Z | InstructionFormat::Br | InstructionFormat::Imm => {}
    }
    bytes
}

fn choices(kind: OperandKind, config: ArchitectureConfig) -> Vec<Operand> {
    match kind {
        OperandKind::Register => (0..16).map(reg).collect(),
        OperandKind::Immediate => IMMEDIATES.into_iter().map(Operand::Immediate).collect(),
        OperandKind::Memory => (0..16)
            .flat_map(|base| {
                IMMEDIATES.map(|displacement| Operand::Memory {
                    base: RegisterIndex::try_from(base).unwrap(),
                    displacement,
                })
            })
            .collect(),
        OperandKind::DataSize => DataSize::ALL
            .iter()
            .filter(|size| config.supports_data_size(size.bytes()))
            .copied()
            .map(Operand::DataSize)
            .collect(),
        OperandKind::Condition => Condition::ALL
            .iter()
            .copied()
            .map(Operand::Condition)
            .collect(),
        OperandKind::Control => ControlRegister::ALL
            .iter()
            .copied()
            .map(Operand::Control)
            .collect(),
    }
}

fn roundtrip_combinations(config: ArchitectureConfig, opcode: Opcode, operands: &mut Vec<Operand>) {
    let definitions = opcode.definition().operands();
    if operands.len() < definitions.len() {
        for operand in choices(definitions[operands.len()].kind, config) {
            operands.push(operand);
            roundtrip_combinations(config, opcode, operands);
            operands.pop();
        }
        return;
    }
    let instruction = Instruction::new(config, opcode, operands).unwrap();
    assert_eq!(instruction.opcode(), opcode);
    assert_eq!(instruction.definition(), opcode.definition());
    assert_eq!(instruction.operands(), operands);
    let expected = reference_bytes(opcode, operands);
    assert_eq!(encode(config, &instruction), Ok(expected));
    assert_eq!(decode(config, &expected), Ok(instruction));
    assert_eq!(
        encode(config, &decode(config, &expected).unwrap()),
        Ok(expected)
    );
}

#[test]
fn every_opcode_format_register_tuple_selector_and_immediate_boundary_roundtrips() {
    for config in MODES {
        for opcode in Opcode::ALL {
            roundtrip_combinations(config, *opcode, &mut Vec::new());
        }
    }
}

#[test]
fn exact_published_vectors_and_control_operand_order() {
    let vectors = [
        (
            Opcode::Add,
            vec![reg(1), reg(2), reg(3)],
            [0x10, 0x21, 3, 0, 0, 0, 0, 0],
        ),
        (
            Opcode::Li,
            vec![reg(15), Operand::Immediate(-1)],
            [2, 15, 0, 0, 255, 255, 255, 255],
        ),
        (
            Opcode::Ldz,
            vec![
                reg(1),
                Operand::Memory {
                    base: RegisterIndex::try_from(2).unwrap(),
                    displacement: -4,
                },
                Operand::DataSize(DataSize::Word),
            ],
            [0x30, 0x21, 0x20, 0, 252, 255, 255, 255],
        ),
        (
            Opcode::Br,
            vec![Operand::Condition(Condition::Al), Operand::Immediate(-2)],
            [0x40, 0, 0, 0, 254, 255, 255, 255],
        ),
        (
            Opcode::Csrw,
            vec![Operand::Control(ControlRegister::Tpayload), reg(15)],
            [0x57, 0xf0, 0x50, 0, 0, 0, 0, 0],
        ),
        (
            Opcode::Csrr,
            vec![reg(15), Operand::Control(ControlRegister::Epc)],
            [0x56, 15, 0x10, 0, 0, 0, 0, 0],
        ),
    ];
    for config in MODES {
        for (opcode, operands, bytes) in &vectors {
            let instruction = Instruction::new(config, *opcode, operands).unwrap();
            assert_eq!(encode(config, &instruction), Ok(*bytes));
            assert_eq!(decode(config, bytes), Ok(instruction));
        }
    }
}

#[test]
fn every_unallocated_opcode_is_rejected_with_original_bytes_and_cause() {
    for config in MODES {
        for input in 0..=u8::MAX {
            if Opcode::ALL.iter().any(|opcode| opcode.as_u8() == input) {
                continue;
            }
            let source = UnknownOpcode { input };
            assert_eq!(Opcode::try_from(input), Err(source));
            for mut bytes in [[0; 8], [255; 8]] {
                bytes[0] = input;
                let error = decode(config, &bytes).unwrap_err();
                assert_eq!(error, DecodeError::UnknownOpcode { bytes, source });
                assert_eq!(
                    error.source().unwrap().downcast_ref::<UnknownOpcode>(),
                    Some(&source)
                );
            }
        }
    }
}

#[test]
fn every_reserved_bit_of_every_opcode_is_rejected_before_selectors() {
    for config in MODES {
        for opcode in Opcode::ALL {
            let mask = reference_mask(opcode.definition().format);
            for bit in 8..64 {
                if mask & (1u64 << bit) != 0 {
                    continue;
                }
                let nonzero = 1u64 << bit;
                let bytes = (u64::from(opcode.as_u8()) | nonzero).to_le_bytes();
                assert_eq!(
                    decode(config, &bytes),
                    Err(DecodeError::ReservedBits {
                        bytes,
                        opcode: *opcode,
                        nonzero
                    })
                );
            }
            let nonzero = !mask;
            let bits = u64::from(opcode.as_u8()) | nonzero | (mask & 0xf00000);
            let bytes = bits.to_le_bytes();
            assert_eq!(
                decode(config, &bytes),
                Err(DecodeError::ReservedBits {
                    bytes,
                    opcode: *opcode,
                    nonzero
                })
            );
        }
    }
}

#[test]
fn all_raw_selectors_and_mode_widths_are_checked() {
    for input in 0..=u8::MAX {
        assert_eq!(DataSize::try_from(input).is_ok(), input < 4);
        assert_eq!(Condition::try_from(input).is_ok(), input < 15);
        assert_eq!(ControlRegister::try_from(input).is_ok(), input < 6);
    }
    for config in MODES {
        for (opcode, kind, index, limit) in [
            (Opcode::Ldz, OperandKind::DataSize, 2, 4),
            (Opcode::Lds, OperandKind::DataSize, 2, 4),
            (Opcode::St, OperandKind::DataSize, 2, 4),
            (Opcode::Br, OperandKind::Condition, 0, 15),
            (Opcode::Csrr, OperandKind::Control, 1, 6),
            (Opcode::Csrw, OperandKind::Control, 0, 6),
        ] {
            for input in 0..16 {
                let bytes = (u64::from(opcode.as_u8()) | (u64::from(input) << 20)).to_le_bytes();
                let result = decode(config, &bytes);
                if input >= limit {
                    assert_eq!(
                        result,
                        Err(DecodeError::Operand {
                            bytes,
                            index,
                            source: OperandError::InvalidSelector { kind, input }
                        })
                    );
                } else if kind == OperandKind::DataSize
                    && input == 3
                    && config.word_width() == WordWidth::W32
                {
                    let source = InstructionError {
                        opcode,
                        source: ValidationError::InvalidWidth {
                            index,
                            size: DataSize::Double,
                            width: WordWidth::W32,
                        },
                    };
                    assert_eq!(result, Err(DecodeError::Validation { bytes, source }));
                } else {
                    assert_eq!(encode(config, &result.unwrap()), Ok(bytes));
                }
            }
        }
    }
    for (index, size) in DataSize::ALL.iter().enumerate() {
        assert_eq!(size.bytes(), [1, 2, 4, 8][index]);
    }
    for control in ControlRegister::ALL {
        assert_eq!(control.is_writable(), control.as_u8() < 4);
        assert_eq!(control.requires_active_frame(), control.as_u8() != 0);
    }
}

#[test]
fn exact_length_is_required_and_inputs_are_unchanged() {
    for config in MODES {
        for length in (0..=32).filter(|length| *length != 8) {
            let bytes = vec![0; length];
            assert_eq!(
                decode(config, &bytes),
                Err(DecodeError::Length { actual: length })
            );
            assert_eq!(bytes, vec![0; length]);
        }
    }
}

#[test]
fn constructors_reject_wrong_counts_and_each_wrong_operand_kind() {
    for config in MODES {
        for opcode in Opcode::ALL {
            let definitions = opcode.definition().operands();
            let operands: Vec<_> = definitions
                .iter()
                .map(|definition| choices(definition.kind, config)[0])
                .collect();
            for count in 0..=5 {
                if count == operands.len() {
                    continue;
                }
                assert_eq!(
                    Instruction::new(config, *opcode, &vec![Operand::Immediate(0); count]),
                    Err(InstructionError {
                        opcode: *opcode,
                        source: ValidationError::OperandCount {
                            expected: operands.len(),
                            actual: count
                        },
                    })
                );
            }
            for (index, definition) in definitions.iter().enumerate() {
                for kind in [
                    OperandKind::Register,
                    OperandKind::Immediate,
                    OperandKind::Memory,
                    OperandKind::DataSize,
                    OperandKind::Condition,
                    OperandKind::Control,
                ] {
                    if kind == definition.kind {
                        continue;
                    }
                    let mut invalid = operands.clone();
                    let actual = choices(kind, config)[0];
                    invalid[index] = actual;
                    assert_eq!(
                        Instruction::new(config, *opcode, &invalid),
                        Err(InstructionError {
                            opcode: *opcode,
                            source: ValidationError::OperandKind {
                                index,
                                expected: definition.kind,
                                actual
                            },
                        })
                    );
                }
            }
        }
    }
}

#[test]
fn encoding_revalidates_mode_without_mutating_instruction() {
    for opcode in [Opcode::Ldz, Opcode::Lds, Opcode::St] {
        let operands = [
            reg(15),
            Operand::Memory {
                base: RegisterIndex::try_from(15).unwrap(),
                displacement: i32::MIN,
            },
            Operand::DataSize(DataSize::Double),
        ];
        let instruction = Instruction::new(MODES[1], opcode, &operands).unwrap();
        let original = instruction;
        let error = InstructionError {
            opcode,
            source: ValidationError::InvalidWidth {
                index: 2,
                size: DataSize::Double,
                width: WordWidth::W32,
            },
        };
        assert_eq!(Instruction::new(MODES[0], opcode, &operands), Err(error));
        assert_eq!(encode(MODES[0], &instruction), Err(error));
        assert_eq!(instruction.validate(MODES[0]), Err(error));
        assert_eq!(instruction, original);
        assert_eq!(
            decode(MODES[1], &encode(MODES[1], &instruction).unwrap()),
            Ok(original)
        );
    }
}

#[test]
fn wide_immediates_never_silently_truncate() {
    let base = RegisterIndex::try_from(15).unwrap();
    for input in [
        i64::MIN,
        i64::from(i32::MIN) - 1,
        i64::from(i32::MAX) + 1,
        i64::MAX,
    ] {
        for result in [
            Operand::try_immediate(input),
            Operand::try_memory(base, input),
        ] {
            let error = result.unwrap_err();
            assert!(
                matches!(error, OperandError::ImmediateOutOfRange { input: retained, .. } if retained == input)
            );
            assert!(error.source().unwrap().is::<std::num::TryFromIntError>());
        }
    }
    for value in IMMEDIATES {
        assert_eq!(
            Operand::try_immediate(i64::from(value)),
            Ok(Operand::Immediate(value))
        );
        assert_eq!(
            Operand::try_memory(base, i64::from(value)),
            Ok(Operand::Memory {
                base,
                displacement: value
            })
        );
    }
}

#[test]
fn selector_names_have_exact_design_values() {
    for (input, condition) in [
        Condition::Al,
        Condition::Eq,
        Condition::Ne,
        Condition::Ult,
        Condition::Uge,
        Condition::Ule,
        Condition::Ugt,
        Condition::Slt,
        Condition::Sge,
        Condition::Sle,
        Condition::Sgt,
        Condition::Vs,
        Condition::Vc,
        Condition::Mi,
        Condition::Pl,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(condition.as_u8(), input as u8);
    }
    for (input, control) in [
        ControlRegister::Tvec,
        ControlRegister::Epc,
        ControlRegister::Esp,
        ControlRegister::Estatus,
        ControlRegister::Tcause,
        ControlRegister::Tpayload,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(control.as_u8(), input as u8);
    }
    for (size, bytes) in [
        (DataSize::Byte, 1),
        (DataSize::Half, 2),
        (DataSize::Word, 4),
        (DataSize::Double, 8),
    ] {
        assert_eq!(size.bytes(), bytes);
    }
}

#[test]
fn error_variants_display_context_and_expose_sources() {
    let invalid_register = RegisterIndex::try_from(16).unwrap_err();
    let bytes = [0; 8];
    let errors = [
        (
            DecodeError::Length { actual: 7 },
            "expected exactly 8 instruction bytes, got 7",
            false,
        ),
        (
            DecodeError::UnknownOpcode {
                bytes,
                source: UnknownOpcode { input: 255 },
            },
            "instruction decode: unallocated opcode 0xff",
            true,
        ),
        (
            DecodeError::ReservedBits {
                bytes,
                opcode: Opcode::Nop,
                nonzero: 256,
            },
            "NOP: nonzero reserved bits 0x0000000000000100",
            false,
        ),
        (
            DecodeError::Operand {
                bytes,
                index: 0,
                source: OperandError::InvalidSelector {
                    kind: OperandKind::Condition,
                    input: 15,
                },
            },
            "instruction decode operand 0: invalid Condition selector 15",
            true,
        ),
        (
            DecodeError::Register {
                bytes,
                index: 1,
                source: invalid_register,
            },
            "instruction decode operand 1: register index 16 is outside 0..16",
            true,
        ),
        (
            DecodeError::Validation {
                bytes,
                source: InstructionError {
                    opcode: Opcode::Nop,
                    source: ValidationError::OperandCount {
                        expected: 0,
                        actual: 1,
                    },
                },
            },
            "instruction decode: NOP: expected 0 operands, got 1",
            true,
        ),
    ];
    for (error, message, has_source) in errors {
        assert_eq!(error.to_string(), message);
        assert_eq!(error.source().is_some(), has_source);
    }
    let error = ValidationError::OperandKind {
        index: 1,
        expected: OperandKind::Register,
        actual: Operand::Immediate(-1),
    };
    assert_eq!(
        error.to_string(),
        "operand 1: expected Register, got Immediate(-1)"
    );
    assert!(error.source().is_none());
    let error = Operand::try_immediate(i64::MAX).unwrap_err();
    assert_eq!(
        error.to_string(),
        "immediate 9223372036854775807 is outside signed 32-bit range"
    );
}

#[test]
fn errors_integrate_with_shared_diagnostics_and_preserve_typed_chains() {
    use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Severity};
    let bytes = [0x30, 0, 0x30, 0, 0, 0, 0, 0];
    let error = decode(MODES[0], &bytes).unwrap_err();
    assert_eq!(
        error.to_string(),
        "instruction decode: LDZ: operand 2: 8-byte data size is unsupported at 32 bits"
    );
    let diagnostic = Diagnostic::new(
        Severity::Error,
        DiagnosticCode::new("E1101").unwrap(),
        "invalid instruction",
    )
    .with_cause(error);
    let decode_cause = diagnostic.source().unwrap();
    assert_eq!(decode_cause.downcast_ref::<DecodeError>(), Some(&error));
    let instruction_cause = decode_cause.source().unwrap();
    assert!(instruction_cause.is::<InstructionError>());
    assert_eq!(
        instruction_cause
            .source()
            .unwrap()
            .downcast_ref::<ValidationError>(),
        Some(&ValidationError::InvalidWidth {
            index: 2,
            size: DataSize::Double,
            width: WordWidth::W32
        })
    );
    assert!(instruction_cause.source().unwrap().source().is_none());
}
