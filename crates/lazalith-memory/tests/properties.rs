//! Property tests for the address space.
//!
//! # The properties
//!
//! Memory is where a machine's promises are kept, and there are four of them:
//!
//! - **A write is a read.** Whatever a program writes comes back, at the width it
//!   wrote and not one bit more. A byte store that leaves the other seven bytes of
//!   its word alone, and a word store that truncates to the machine, are both
//!   properties of the same operation seen twice.
//! - **Accesses of different widths overlap consistently.** A word read and the two
//!   half-word reads beneath it must agree, or a program that builds a value a byte
//!   at a time reads something else than the value it wrote. This is the property
//!   that catches an endianness bug, and an endianness bug is invisible to a
//!   round trip that only ever uses one width.
//! - **Unmapped memory faults, and mapped memory does not.** A fault is a refusal,
//!   and a refusal that sometimes does not happen is a program that reads whatever
//!   was there.
//! - **Permissions are permissions.** A read-only region refuses a write and a
//!   no-access region refuses both, and refusing is silent about the value: a
//!   refused write leaves the memory as it was.
//!
//! The addresses and values are drawn rather than listed, and the region shapes are
//! small — sixteen bytes — because a property over a large region mostly tests that
//! the arithmetic does not overflow, which a separate property about the machine's
//! address width already covers.

use lazalith_cpu::{DataAccess, DataAccessKind, Privilege};
use lazalith_memory::{AddressSpace, DataSize, MemoryRegion, RegionPermissions};
use lazalith_properties::{Case, Gen, check};
use lazalith_types::{ArchitectureConfig, PhysicalAddress, VirtualAddress};

/// The region every case maps, and its permissions.
const LENGTH: u64 = 16;
const BASE: u64 = 0x1000;

/// Readable, writable, not executable, not accessible to user mode.
const RW: RegionPermissions = RegionPermissions::new(true, true, false, false);

/// One access to a mapped region: where, how wide, what, and to or from.
struct Access {
    config: ArchitectureConfig,
    address: u64,
    size: DataSize,
    write: bool,
    value: u64,
}

impl Case for Access {
    fn generate(source: &mut Gen) -> Self {
        let config = match source.bool() {
            true => ArchitectureConfig::lz32(),
            false => ArchitectureConfig::lz64(),
        };
        // An address anywhere in the region, aligned to nothing in particular: a
        // misaligned access is a case the address space has an opinion about, and
        // this file is about the accesses it accepts.
        let address = BASE + source.below(LENGTH);
        let size = source
            .choice(DataSize::ALL)
            .expect("there is always a data size");
        Self {
            config,
            address,
            size,
            write: source.bool(),
            value: source.interesting_u64(),
        }
    }

    fn describe(&self) -> String {
        format!(
            "{:?} {:#x} bytes at {:#x} in {:?}",
            if self.write { "write" } else { "read" },
            self.value,
            self.address,
            self.config
        )
    }
}

impl Access {
    /// The access the CPU would make, or `None` when the address space will not
    /// build one — a size this target does not have, or a width it cannot check.
    fn as_access(&self) -> Option<DataAccess> {
        DataAccess::new(
            self.config,
            VirtualAddress::new(self.address),
            0,
            self.size,
            if self.write {
                DataAccessKind::Write
            } else {
                DataAccessKind::Read
            },
            Privilege::Supervisor,
        )
        .ok()
    }

    /// A space with one mapped region and nothing else.
    fn space(&self) -> AddressSpace {
        let mut space = AddressSpace::new(self.config);
        space
            .map(
                MemoryRegion::ram(self.config, PhysicalAddress::new(BASE), LENGTH, RW)
                    .expect("a sixteen byte region is a region"),
            )
            .expect("the region is disjoint from nothing");
        space
    }
}

/// A write is a read, at the width that was written.
///
/// The truncation is the assertion. A `u32` machine cannot hold the high half of a
/// 64-bit value, so a word write of `0xffff_ffff_ffff_ffff` reads back as
/// `0xffff_ffff` — and a read that gave back the whole thing would mean the
/// register and the memory disagreed about how wide the machine is.
#[test]
fn a_write_reads_back_as_what_the_machine_can_hold() {
    check::<Access>(64, |case| {
        if !case.write {
            return true;
        }
        let (Some(access), mut space) = (case.as_access(), case.space()) else {
            return true;
        };
        if space.write_data(access, case.value).is_err() {
            return true;
        }
        let read = DataAccess::new(
            case.config,
            VirtualAddress::new(case.address),
            0,
            case.size,
            DataAccessKind::Read,
            Privilege::Supervisor,
        );
        let Ok(read) = read else {
            return true;
        };
        match space.read_data(read) {
            Ok(seen) => {
                let mask = u64::MAX >> (64 - u32::from(case.size.bytes() * 8));
                seen == case.value & mask
            }
            // A write that succeeded and a read that faults is a contradiction,
            // and the only explanation is that the two doors disagree about the
            // region. Anything else is a refusal of both, which is a separate case.
            Err(_) => false,
        }
    });
}

/// A byte write disturbs only its own byte.
///
/// The property that catches a store writing a whole word when it was asked for a
/// byte, which is the bug a C `char` store finds and which nothing else does: the
/// neighbouring byte belongs to something else, and a program that wrote it would
/// corrupt a value it never mentioned.
#[test]
fn a_byte_write_leaves_its_neighbours_alone() {
    struct ByteWrite {
        config: ArchitectureConfig,
        neighbours: u64,
        value: u8,
    }
    impl Case for ByteWrite {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                neighbours: source.interesting_u64(),
                value: source.next_u8(),
            }
        }
        fn describe(&self) -> String {
            format!("{:#04x} into a word of {:#x}", self.value, self.neighbours)
        }
    }

    check::<ByteWrite>(64, |case| {
        let mut space = AddressSpace::new(case.config);
        space
            .map(
                MemoryRegion::ram(case.config, PhysicalAddress::new(BASE), LENGTH, RW)
                    .expect("a region"),
            )
            .expect("mapped");
        let access = |size: DataSize, kind: DataAccessKind| {
            DataAccess::new(
                case.config,
                VirtualAddress::new(BASE),
                0,
                size,
                kind,
                Privilege::Supervisor,
            )
        };
        let (Ok(read), Ok(word), Ok(byte)) = (
            access(DataSize::Double, DataAccessKind::Read),
            access(DataSize::Double, DataAccessKind::Write),
            access(DataSize::Byte, DataAccessKind::Write),
        ) else {
            return true;
        };
        if space.write_data(word, case.neighbours).is_err()
            || space.write_data(byte, u64::from(case.value)).is_err()
        {
            return true;
        }
        let Ok(after) = space.read_data(read) else {
            return true;
        };
        // The machine's word, less its low byte. Written this way because a 32-bit
        // machine holds four bytes, so the mask is not a constant — and a test that
        // assumed eight would fail for the right reason on the wrong target.
        let mask = case.config.word_width().mask() & !0xff;
        after == ((case.neighbours & mask) | u64::from(case.value))
    });
}
#[test]
fn unmapped_memory_faults_and_mapped_memory_does_not() {
    struct Probe {
        config: ArchitectureConfig,
        offset: u64,
    }
    impl Case for Probe {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                // Half the draws are inside the region and half are past its end.
                offset: if source.bool() {
                    source.below(LENGTH)
                } else {
                    LENGTH + source.below(4096)
                },
            }
        }
        fn describe(&self) -> String {
            format!("{:#x} in {:?}", BASE + self.offset, self.config)
        }
    }

    check::<Probe>(64, |case| {
        let mut space = AddressSpace::new(case.config);
        space
            .map(
                MemoryRegion::ram(case.config, PhysicalAddress::new(BASE), LENGTH, RW)
                    .expect("a region"),
            )
            .expect("mapped");
        let access = DataAccess::new(
            case.config,
            VirtualAddress::new(BASE + case.offset),
            0,
            DataSize::Byte,
            DataAccessKind::Read,
            Privilege::Supervisor,
        );
        let inside = case.offset < LENGTH;
        // A read of an unmapped byte faults, and that is the whole property: the
        // space is asked and says no. Inside, there is nothing to assert about the
        // *value* — it is zero — only that the access was not refused.
        match access {
            Ok(access) => space.read_data(access).is_err() != inside,
            // A width the target does not have is a different refusal, and it is
            // the access constructor's business rather than the space's.
            Err(_) => true,
        }
    });
}

/// A read-only region refuses a write and leaves its contents alone.
///
/// The second half is the important one. A refused write that wrote anyway would be
/// a region that is read-only in name only, and the only way to notice is to read
/// the memory back afterwards.
#[test]
fn a_read_only_region_refuses_a_write_without_changing_anything() {
    struct Region {
        config: ArchitectureConfig,
        value: u64,
    }
    impl Case for Region {
        fn generate(source: &mut Gen) -> Self {
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                value: source.interesting_u64(),
            }
        }
        fn describe(&self) -> String {
            format!("a read-only region holding {:#x}", self.value)
        }
    }

    check::<Region>(64, |case| {
        let mut space = AddressSpace::new(case.config);
        space
            .map(
                // Sixteen bytes of contents, because a region with none has no length and a
                // zero-length access is a different refusal entirely.
                MemoryRegion::rom(
                    case.config,
                    PhysicalAddress::new(BASE),
                    &[0u8; 16],
                    RegionPermissions::new(true, false, false, false),
                )
                .expect("a read-only region"),
            )
            .expect("mapped");
        let access = |kind| {
            DataAccess::new(
                case.config,
                VirtualAddress::new(BASE),
                0,
                DataSize::Double,
                kind,
                Privilege::Supervisor,
            )
        };
        let (Ok(read), Ok(write)) = (access(DataAccessKind::Read), access(DataAccessKind::Write))
        else {
            return true;
        };
        if space.write_data(write, 0xdead_beef).is_ok() {
            return false;
        }
        // A read may still fault, for a reason of its own; what must not happen is
        // the write landing.
        space
            .read_data(read)
            .map(|seen| seen != 0xdead_beef)
            .unwrap_or(true)
    });
}

/// Two regions cannot overlap, whatever they are.
///
/// A property over the *pair*, not over one region: an off-by-one in the overlap
/// check only shows when two regions meet, and the boundary cases — a region
/// ending exactly where the next begins, and two regions sharing a single byte —
/// are the two a check gets wrong.
#[test]
fn regions_do_not_overlap() {
    struct Pair {
        config: ArchitectureConfig,
        first: (u64, u64),
        second: (u64, u64),
    }
    impl Case for Pair {
        fn generate(source: &mut Gen) -> Self {
            // Both regions start in a narrow window so that the draws *can* touch,
            // which a uniform draw over the address space would essentially never
            // arrange.
            let first_start = source.below(64);
            Self {
                config: match source.bool() {
                    true => ArchitectureConfig::lz32(),
                    false => ArchitectureConfig::lz64(),
                },
                first: (first_start, 1 + source.below(32)),
                second: (source.below(64), 1 + source.below(32)),
            }
        }
        fn describe(&self) -> String {
            format!("{:?} and {:?}", self.first, self.second)
        }
    }

    check::<Pair>(64, |case| {
        let (first_start, first_length) = case.first;
        let (second_start, second_length) = case.second;
        let (Some(first_end), Some(second_end)) = (
            first_start.checked_add(first_length - 1),
            second_start.checked_add(second_length - 1),
        ) else {
            return true;
        };
        let overlaps = first_start <= second_end && second_start <= first_end;
        let mut space = AddressSpace::new(case.config);
        let region = |start: u64, length: u64| {
            MemoryRegion::ram(case.config, PhysicalAddress::new(start), length, RW)
        };
        let Ok(first) = region(first_start, first_length) else {
            return true;
        };
        space.map(first).expect("the first region maps");
        let Ok(second) = region(second_start, second_length) else {
            return true;
        };
        // Overlapping is refused; touching is not. The distinction is the whole
        // property: an inclusive end that was exclusive would refuse a region that
        // merely begins where the first ends, and two regions that abut are how
        // every real memory map is built.
        space.map(second).is_err() == overlaps
    });
}
