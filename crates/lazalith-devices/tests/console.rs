use lazalith_devices::{ConsoleDevice, Device, DeviceError as E, DeviceOffset as O};
use lazalith_isa::DataSize as S;

#[test]
fn byte_output_is_bounded_and_reset_reuses_capacity() {
    let mut console = ConsoleDevice::new(256).unwrap();
    assert_eq!(console.address_len(), 1);
    assert_eq!(console.capacity(), 256);
    for byte in 0..=255 {
        console.write(O::new(0), S::Byte, byte).unwrap();
    }
    assert_eq!(console.output(), (0..=255).collect::<Vec<u8>>());
    assert!(matches!(
        console.write(O::new(0), S::Byte, 1),
        Err(E::Capacity)
    ));
    assert_eq!(console.output().len(), 256);
    console.reset();
    assert!(console.output().is_empty());
    assert_eq!(console.capacity(), 256);
    console.write(O::new(0), S::Byte, 0x141).unwrap();
    assert_eq!(console.output(), b"A");
}

#[test]
fn invalid_register_operations_and_peek_leave_output_unchanged() {
    let mut console = ConsoleDevice::new(4).unwrap();
    console.write(O::new(0), S::Byte, 65).unwrap();
    for size in [S::Half, S::Word, S::Double] {
        assert!(console.write(O::new(0), size, 1).is_err());
    }
    for offset in [1, u64::MAX] {
        assert!(console.write(O::new(offset), S::Byte, 1).is_err());
    }
    assert!(matches!(
        console.read(O::new(0), S::Byte),
        Err(E::ReadUnsupported)
    ));
    let mut byte = [9];
    assert!(matches!(
        console.peek(O::new(0), &mut byte),
        Err(E::Unpeekable)
    ));
    assert_eq!(byte, [9]);
    assert_eq!(console.output(), b"A");
    console.tick(lazalith_types::CycleCount::new(100));
    assert_eq!(console.elapsed(), lazalith_types::CycleCount::new(100));
    assert_eq!(console.output(), b"A");
}

#[test]
fn capacity_and_allocation_failure_happen_before_output_effects() {
    assert!(matches!(
        ConsoleDevice::new(usize::MAX),
        Err(E::Allocation(_))
    ));
    let mut console = ConsoleDevice::new(0).unwrap();
    assert!(matches!(
        console.validate_write(O::new(0), S::Byte, 1),
        Err(E::Capacity)
    ));
    assert!(matches!(
        console.write(O::new(0), S::Byte, 1),
        Err(E::Capacity)
    ));
    assert!(console.output().is_empty());
}
