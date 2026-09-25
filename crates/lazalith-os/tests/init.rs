use lazalith_cpu::Privilege;
use lazalith_isa::{Opcode, decode};
use lazalith_os::{
    INIT_CODE_LENGTH, INIT_EXIT_CODE, LzxArchitecture, LzxImage, ProcessId, ThreadId,
    USER_INITIAL_SP, build_init_image,
};
use lazalith_types::{ArchitectureConfig as C, RegisterIndex};

#[test]
fn init_image_is_a_real_lzx_exit_program_in_both_modes() {
    let expected = [2, 0, 0, 0, 1, 0, 0, 0, 0x50, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(INIT_EXIT_CODE, 0);
    for (architecture, config) in [
        (LzxArchitecture::Lz32, C::lz32()),
        (LzxArchitecture::Lz64, C::lz64()),
    ] {
        let image = build_init_image(architecture).unwrap();
        assert_eq!(image.architecture(), architecture);
        assert_eq!(image.sections().len(), 1);
        assert_eq!(image.sections()[0].bytes(), expected);
        assert_eq!(image.sections()[0].virtual_size(), INIT_CODE_LENGTH as u64);
        assert_eq!(image.required_data(), 0);
        assert_eq!(image.required_stack(), lazalith_os::USER_STACK_LENGTH);
        assert_eq!(image.entry_section(), 0);
        assert_eq!(image.entry_offset(), 0);

        let first = decode(config, &expected[..8]).unwrap();
        let second = decode(config, &expected[8..]).unwrap();
        assert_eq!(first.opcode(), Opcode::Li);
        assert_eq!(
            first.operands()[0],
            lazalith_isa::Operand::Register(RegisterIndex::try_from(0).unwrap())
        );
        assert_eq!(first.operands()[1], lazalith_isa::Operand::Immediate(1));
        assert_eq!(second.opcode(), Opcode::Syscall);

        let encoded = image.to_bytes().unwrap();
        assert_eq!(encoded.len(), 128);
        assert_eq!(LzxImage::from_bytes(&encoded).unwrap(), image);
        let mut process = LzxImage::from_bytes(&encoded)
            .unwrap()
            .load_process(ProcessId::new(1).unwrap(), ThreadId::new(1).unwrap())
            .unwrap();
        assert_eq!(process.program().bytes(), expected);
        assert_eq!(process.primary_thread().cpu().privilege(), Privilege::User);
        assert_eq!(
            process.primary_thread().cpu().sp().as_u64(),
            USER_INITIAL_SP
        );
        assert!(
            process
                .memory()
                .address_space()
                .regions()
                .iter()
                .any(|region| {
                    region.start().as_u64() == lazalith_os::USER_CODE_START
                        && region.length() == lazalith_os::USER_CODE_LENGTH
                })
        );
        process.mark_ready().unwrap();
    }
}
