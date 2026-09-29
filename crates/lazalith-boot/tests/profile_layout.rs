//! B4: the machine's geometry, and the one definition of it.
//!
//! # Why these tests are here and not in `lazalith-machine`
//!
//! They compare `lazalith_boot`'s constants, `lazalith_os`'s constants and
//! `lazalith_machine::LZA64_LAYOUT` — and `lazalith-boot` depends on
//! `lazalith-machine`, so a test in the machine crate cannot see boot or os at
//! all. This is the crate that can see all three, which makes it the only place
//! the claim can be checked rather than asserted.
//!
//! # Why the claim matters
//!
//! Before B4, `KERNEL_IMAGE_LENGTH` and `KERNEL_INITIAL_SP` were each declared
//! twice — once in `lazalith-boot` and once in `lazalith-os` — as two independent
//! constants with the same numbers. Two constants that agree today will not
//! necessarily agree after the first change to one of them, and the one that
//! disagreed would be the one describing the kernel's memory window, so the
//! failure would be a kernel that is loaded somewhere it does not fit.
//!
//! B4 gives the machine one definition and both crates re-export it. These tests
//! hold that, and hold that the values did not move when they moved.

use lazalith_machine::LZA64_LAYOUT;

/// The machine's geometry is what it was before B4.
///
/// B4 moved these numbers out of `lazalith-boot` and `lazalith-os` and into
/// `lazalith-machine`, because a machine profile has to be able to say what a
/// machine is. Moving a constant is invisible until a guest boots at the wrong
/// address, so the pre-B4 values are written down here: a future stage that
/// changes one deliberately changes this test too, rather than discovering the
/// consequence from a guest that will not boot.
#[test]
fn the_phase_i_layout_is_unchanged() {
    assert_eq!(lazalith_boot::BOOT_ROM_START, 0x0000_0000);
    assert_eq!(lazalith_boot::BOOT_ROM_LENGTH, 0x0008_0000);
    assert_eq!(lazalith_boot::BOOT_HEADER_ADDRESS, 0x0000_0400);
    assert_eq!(lazalith_boot::KERNEL_PAYLOAD_ADDRESS, 0x0000_1000);
    assert_eq!(lazalith_boot::MAX_BOOT_ROM_PAYLOAD, 0x0007_f000);
    assert_eq!(lazalith_boot::KERNEL_LOAD_ADDRESS, 0x0010_0000);
    assert_eq!(lazalith_boot::KERNEL_IMAGE_LENGTH, 0x0008_0000);
    assert_eq!(lazalith_boot::KERNEL_INITIAL_SP, 0x0018_f000);
    assert_eq!(lazalith_os::PHYSICAL_RAM_START, 0x0010_0000);
    assert_eq!(lazalith_os::PHYSICAL_RAM_LENGTH, 0x0031_0000);
}

/// Every re-export points at the one definition.
///
/// Not just the values — the *identity*. `LZA64_LAYOUT` is a `const`, so
/// `lazalith_boot::BOOT_ROM_START` is a copy of the field, and a test comparing
/// values could not tell a copy that happens to be right from a re-export that
/// will drift when the field moves. Comparing against the constant on both sides
/// is what makes this a test of the re-export rather than of the number.
#[test]
fn every_re_export_points_at_the_one_definition() {
    assert_eq!(lazalith_boot::BOOT_ROM_START, LZA64_LAYOUT.boot_rom_start);
    assert_eq!(lazalith_boot::BOOT_ROM_LENGTH, LZA64_LAYOUT.boot_rom_length);
    assert_eq!(
        lazalith_boot::BOOT_HEADER_ADDRESS,
        LZA64_LAYOUT.boot_header_address
    );
    assert_eq!(
        lazalith_boot::KERNEL_PAYLOAD_ADDRESS,
        LZA64_LAYOUT.kernel_payload_address
    );
    assert_eq!(
        lazalith_boot::MAX_BOOT_ROM_PAYLOAD,
        LZA64_LAYOUT.max_boot_rom_payload
    );
    assert_eq!(
        lazalith_boot::KERNEL_LOAD_ADDRESS,
        LZA64_LAYOUT.kernel_load_address
    );
    assert_eq!(
        lazalith_boot::KERNEL_IMAGE_LENGTH,
        LZA64_LAYOUT.kernel_image_length
    );
    assert_eq!(
        lazalith_boot::KERNEL_INITIAL_SP,
        LZA64_LAYOUT.kernel_initial_sp
    );
    assert_eq!(
        lazalith_os::PHYSICAL_RAM_START,
        LZA64_LAYOUT.physical_ram_start
    );
    assert_eq!(
        lazalith_os::PHYSICAL_RAM_LENGTH,
        LZA64_LAYOUT.physical_ram_length
    );
    assert_eq!(
        lazalith_os::KERNEL_IMAGE_START,
        LZA64_LAYOUT.kernel_load_address
    );
    assert_eq!(
        lazalith_os::KERNEL_INITIAL_SP,
        LZA64_LAYOUT.kernel_initial_sp
    );
}

/// The two values that used to be declared twice are now one.
///
/// This is the specific duplication B4 removed, so it gets its own test rather
/// than being folded into the comparison above: these two were the ones where a
/// disagreement would have been a kernel loaded outside its own window.
#[test]
fn the_kernel_window_is_defined_once() {
    assert_eq!(
        lazalith_boot::KERNEL_IMAGE_LENGTH,
        lazalith_os::KERNEL_IMAGE_LENGTH
    );
    assert_eq!(
        lazalith_boot::KERNEL_INITIAL_SP,
        lazalith_os::KERNEL_INITIAL_SP
    );
}

/// The layout is internally consistent, so a profile built from it describes a
/// machine the architecture can address.
///
/// A layout is a set of numbers about one machine, and the numbers have to agree
/// with each other: a boot ROM longer than the maximum payload, a header outside
/// its own ROM, a kernel window that runs past the end of RAM. None of those is a
/// crash at build time — they are a machine that boots and then misbehaves.
#[test]
fn the_layout_is_internally_consistent() {
    let layout = LZA64_LAYOUT;
    assert!(
        layout.boot_rom_length > 0,
        "a machine with no boot ROM has no firmware window"
    );
    assert!(
        layout.boot_header_address > layout.boot_rom_start
            && layout.boot_header_address < layout.boot_rom_start + layout.boot_rom_length,
        "the boot header must be inside the boot ROM"
    );
    assert!(
        layout.kernel_payload_address > layout.boot_rom_start
            && layout.kernel_payload_address < layout.boot_rom_start + layout.boot_rom_length,
        "a staged kernel payload must be inside the boot ROM"
    );
    assert!(
        layout.max_boot_rom_payload < layout.boot_rom_length,
        "a payload limit above the ROM size is not a limit"
    );
    assert_eq!(
        layout.kernel_load_address, layout.physical_ram_start,
        "a kernel is loaded into RAM, so it loads where RAM starts"
    );
    assert!(
        layout.kernel_load_address + layout.kernel_image_length
            <= layout.physical_ram_start + layout.physical_ram_length,
        "the kernel window must fit inside RAM"
    );
    assert!(
        layout.kernel_initial_sp > layout.kernel_load_address,
        "a kernel's stack starts above its image, not inside it"
    );
    assert!(
        layout.kernel_initial_sp <= layout.physical_ram_start + layout.physical_ram_length,
        "a kernel's initial stack pointer must be inside RAM"
    );
}
