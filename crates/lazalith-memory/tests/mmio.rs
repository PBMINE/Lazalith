use lazalith_devices::{
    Device, DeviceError as E, DeviceId as Id, DeviceManager, DeviceOffset as O,
};
use lazalith_memory::{
    AddressSpace, Bus, DataAccessKind as K, DataSize as S, MemoryFaultKind as F, MemoryRegion,
    Privilege as U, RegionPermissions as RP,
};
use lazalith_types::{
    ArchitectureConfig as C, CycleCount, InstructionAddress as I, PhysicalAddress as P,
    VirtualAddress as V,
};

#[derive(Debug)]
struct RegisterDevice {
    bytes: [u8; 8],
    reads: u64,
    ticks: u64,
    peekable: bool,
}

impl RegisterDevice {
    fn new(peekable: bool) -> Self {
        Self {
            bytes: [0; 8],
            reads: 0,
            ticks: 0,
            peekable,
        }
    }
}

impl Device for RegisterDevice {
    fn address_len(&self) -> u64 {
        8
    }
    fn reset(&mut self) {
        self.bytes = [0; 8];
        self.reads = 0;
        self.ticks = 0;
    }
    fn validate_read(&self, offset: O, size: S) -> Result<(), E> {
        if offset.as_u64() >= 8 {
            return Err(E::InvalidRange { offset, bytes: 1 });
        }
        if size != S::Byte {
            return Err(E::UnsupportedSize(size));
        }
        if self.reads == u64::MAX {
            return Err(E::Capacity);
        }
        Ok(())
    }
    fn validate_write(&self, offset: O, size: S, value: u64) -> Result<(), E> {
        self.validate_read(offset, size)?;
        if value == 255 {
            return Err(E::Capacity);
        }
        Ok(())
    }
    fn read(&mut self, offset: O, size: S) -> Result<u64, E> {
        self.validate_read(offset, size)?;
        self.reads += 1;
        Ok(u64::from(self.bytes[offset.as_u64() as usize]))
    }
    fn write(&mut self, offset: O, size: S, value: u64) -> Result<(), E> {
        self.validate_write(offset, size, value)?;
        self.bytes[offset.as_u64() as usize] = value as u8;
        Ok(())
    }
    fn peek(&self, offset: O, output: &mut [u8]) -> Result<(), E> {
        if !self.peekable {
            return Err(E::Unpeekable);
        }
        let start = usize::try_from(offset.as_u64()).map_err(|_| E::InvalidRange {
            offset,
            bytes: output.len() as u64,
        })?;
        let bytes = self
            .bytes
            .get(start..)
            .and_then(|bytes| bytes.get(..output.len()))
            .ok_or(E::InvalidRange {
                offset,
                bytes: output.len() as u64,
            })?;
        output.copy_from_slice(bytes);
        Ok(())
    }
    fn tick(&mut self, elapsed: CycleCount) {
        self.ticks = elapsed.as_u64();
    }
}

fn manager(peekable: bool) -> DeviceManager<RegisterDevice> {
    let mut devices = DeviceManager::new();
    devices
        .insert(Id::new(1), RegisterDevice::new(peekable))
        .unwrap();
    devices
        .insert(Id::new(2), RegisterDevice::new(peekable))
        .unwrap();
    devices
}

fn bus(config: C, peekable: bool) -> Bus<RegisterDevice> {
    let mut bus = Bus::with_devices(AddressSpace::new(config), manager(peekable));
    bus.map_device(Id::new(1), P::new(64), RP::new(true, true, false, true))
        .unwrap();
    bus
}

#[test]
fn manager_validates_before_effects_and_resets_owned_devices() {
    let mut devices = manager(true);
    assert!(matches!(
        devices.insert(Id::new(1), RegisterDevice::new(true)),
        Err(E::DuplicateDevice(_))
    ));
    assert_eq!(devices.len(), 2);
    for (id, offset, size, value) in [
        (3, 0, S::Byte, 1),
        (1, 8, S::Byte, 1),
        (1, u64::MAX, S::Byte, 1),
        (1, 0, S::Word, 1),
        (1, 0, S::Byte, 255),
    ] {
        assert!(
            devices
                .write(Id::new(id), O::new(offset), size, value)
                .is_err()
        );
        assert_eq!(devices.device(Id::new(1)).unwrap().bytes, [0; 8]);
    }
    devices
        .write(Id::new(1), O::new(3), S::Byte, 0x142)
        .unwrap();
    assert_eq!(devices.read(Id::new(1), O::new(3), S::Byte).unwrap(), 0x42);
    devices.reset();
    assert_eq!(devices.device(Id::new(1)).unwrap().reads, 0);
    assert_eq!(devices.device(Id::new(1)).unwrap().bytes, [0; 8]);
}

#[test]
fn bus_resets_devices_at_a_new_epoch_without_removing_mappings() {
    let mut bus = bus(C::lz64(), true);
    bus.tick_devices(CycleCount::new(9)).unwrap();
    let write = bus
        .data_access(V::new(66), 0, S::Byte, K::Write, U::User)
        .unwrap();
    lazalith_memory::CpuMemory::write_data(&mut bus, write, 0x141).unwrap();

    bus.reset_devices(CycleCount::new(4));

    assert_eq!(bus.devices().clock().elapsed(), CycleCount::new(4));
    for id in [1, 2] {
        let device = bus.devices().device(Id::new(id)).unwrap();
        assert_eq!(device.ticks, 4);
        assert_eq!(device.bytes, [0; 8]);
    }
    bus.reset_devices(CycleCount::new(0));
    assert_eq!(bus.devices().clock().elapsed(), CycleCount::new(0));
    for id in [1, 2] {
        assert_eq!(bus.devices().device(Id::new(id)).unwrap().ticks, 0);
    }
    let write = bus
        .data_access(V::new(66), 0, S::Byte, K::Write, U::User)
        .unwrap();
    lazalith_memory::CpuMemory::write_data(&mut bus, write, 0x142).unwrap();
    assert_eq!(bus.devices().device(Id::new(1)).unwrap().bytes[2], 0x42);
}

#[test]
fn manager_tick_overflow_is_atomic_across_devices() {
    let mut devices = manager(true);
    devices.tick(CycleCount::new(u64::MAX)).unwrap();
    assert!(matches!(devices.tick(CycleCount::new(1)), Err(E::Clock(_))));
    for id in [1, 2] {
        assert_eq!(devices.device(Id::new(id)).unwrap().ticks, u64::MAX);
    }
    assert_eq!(devices.clock().elapsed().as_u64(), u64::MAX);
    devices.tick(CycleCount::new(0)).unwrap();
    devices.reset();
    assert_eq!(devices.clock().elapsed().as_u64(), 0);
}

#[test]
fn mmio_routes_real_cpu_memory_accesses_in_both_modes() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, true);
        let write = bus
            .data_access(V::new(66), 0, S::Byte, K::Write, U::User)
            .unwrap();
        lazalith_memory::CpuMemory::write_data(&mut bus, write, 0x141).unwrap();
        let read = bus
            .data_access(V::new(66), 0, S::Byte, K::Read, U::User)
            .unwrap();
        assert_eq!(
            lazalith_memory::CpuMemory::read_data(&mut bus, read).unwrap(),
            65
        );
        assert_eq!(bus.devices().device(Id::new(1)).unwrap().reads, 1);
        let mut bytes = [0; 8];
        bus.peek(P::new(64), &mut bytes).unwrap();
        assert_eq!(bytes, [0, 0, 65, 0, 0, 0, 0, 0]);
        assert_eq!(bus.devices().device(Id::new(1)).unwrap().reads, 1);
    }
}

#[test]
fn mappings_are_complete_and_disjoint_in_every_insertion_order() {
    for config in [C::lz32(), C::lz64()] {
        for rom in [false, true] {
            for device_first in [false, true] {
                for start in [57, 64, 71] {
                    let mut bus = Bus::with_devices(AddressSpace::new(config), manager(true));
                    let region = if rom {
                        MemoryRegion::rom(
                            config,
                            P::new(start),
                            &[0; 8],
                            RP::new(true, false, true, true),
                        )
                        .unwrap()
                    } else {
                        MemoryRegion::ram(
                            config,
                            P::new(start),
                            8,
                            RP::new(true, true, false, true),
                        )
                        .unwrap()
                    };
                    if device_first {
                        bus.map_device(Id::new(1), P::new(64), RP::new(true, true, false, true))
                            .unwrap();
                        assert!(matches!(
                            bus.map(region).unwrap_err().kind,
                            F::Overlap { .. }
                        ));
                        assert!(bus.address_space().regions().is_empty());
                    } else {
                        bus.map(region).unwrap();
                        assert!(matches!(
                            bus.map_device(
                                Id::new(1),
                                P::new(64),
                                RP::new(true, true, false, true)
                            )
                            .unwrap_err()
                            .kind,
                            F::Overlap { .. }
                        ));
                    }
                }
            }
        }
        let mut bus = bus(config, true);
        assert!(
            bus.map_device(Id::new(2), P::new(71), RP::new(true, true, false, true))
                .is_err()
        );
        bus.map_device(Id::new(2), P::new(72), RP::new(true, true, false, true))
            .unwrap();
        assert!(matches!(
            bus.map_device(Id::new(1), P::new(80), RP::new(true, true, false, true))
                .unwrap_err()
                .kind,
            F::DeviceAlreadyMapped(_)
        ));
        let mut bytes = [9; 2];
        assert!(matches!(
            bus.peek(P::new(71), &mut bytes).unwrap_err().kind,
            F::CrossRegion { .. }
        ));
        assert_eq!(bytes, [9; 2]);
    }
}

#[test]
fn mmio_rejects_fetch_stack_loader_and_unpeekable_without_reading() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = bus(config, false);
        assert!(matches!(
            bus.fetch_instruction(config, I::new(64), U::Supervisor)
                .unwrap_err()
                .kind,
            F::AccessPolicy
        ));
        let size = if config == C::lz32() {
            S::Word
        } else {
            S::Double
        };
        for kind in [K::StackRead, K::StackWrite] {
            let access = bus
                .data_access(V::new(64), 0, size, kind, U::Supervisor)
                .unwrap();
            assert!(bus.peek_stack(access).is_err());
            assert!(bus.write_data(access, 1).is_err());
            assert!(bus.read_data(access).is_err());
        }
        assert!(bus.initialize(P::new(64), &[1]).is_err());
        let mut bytes = [9; 8];
        assert!(matches!(
            bus.peek(P::new(64), &mut bytes).unwrap_err().kind,
            F::Device(E::Unpeekable)
        ));
        assert_eq!(bytes, [9; 8]);
        assert_eq!(bus.devices().device(Id::new(1)).unwrap().reads, 0);
        assert_eq!(bus.devices().device(Id::new(1)).unwrap().bytes, [0; 8]);
    }
}

#[test]
fn permissions_configuration_and_device_validation_precede_mutation() {
    for config in [C::lz32(), C::lz64()] {
        for read in [false, true] {
            for write in [false, true] {
                for user in [false, true] {
                    let mut bus = Bus::with_devices(AddressSpace::new(config), manager(true));
                    bus.map_device(Id::new(1), P::new(64), RP::new(read, write, false, user))
                        .unwrap();
                    for privilege in [U::Supervisor, U::User] {
                        let access = bus
                            .data_access(V::new(64), 0, S::Byte, K::Write, privilege)
                            .unwrap();
                        assert_eq!(
                            bus.write_data(access, 42).is_ok(),
                            write && (user || privilege == U::Supervisor)
                        );
                        let access = bus
                            .data_access(V::new(64), 0, S::Byte, K::Read, privilege)
                            .unwrap();
                        assert_eq!(
                            bus.read_data(access).is_ok(),
                            read && (user || privilege == U::Supervisor)
                        );
                    }
                    let before = bus.devices().device(Id::new(1)).unwrap().bytes;
                    let access = bus
                        .data_access(V::new(64), 0, S::Byte, K::Write, U::Supervisor)
                        .unwrap();
                    assert!(bus.write_data(access, 255).is_err());
                    let other = if config == C::lz32() {
                        C::lz64()
                    } else {
                        C::lz32()
                    };
                    let wrong = lazalith_memory::DataAccess::new(
                        other,
                        V::new(64),
                        0,
                        S::Byte,
                        K::Write,
                        U::Supervisor,
                    )
                    .unwrap();
                    assert!(matches!(
                        bus.write_data(wrong, 3).unwrap_err().kind,
                        F::Configuration { .. }
                    ));
                    assert_eq!(bus.devices().device(Id::new(1)).unwrap().bytes, before);
                }
            }
        }
    }
}

#[test]
fn mapping_validation_retains_manager_and_handles_address_limits() {
    for config in [C::lz32(), C::lz64()] {
        let mut bus = Bus::with_devices(AddressSpace::new(config), manager(true));
        assert!(matches!(
            bus.map_device(Id::new(3), P::new(0), RP::new(true, true, false, true))
                .unwrap_err()
                .kind,
            F::Device(E::UnknownDevice(_))
        ));
        assert!(
            bus.map_device(Id::new(1), P::new(0), RP::new(true, true, true, true))
                .is_err()
        );
        let max = if config == C::lz32() {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        assert!(
            bus.map_device(
                Id::new(1),
                P::new(max - 6),
                RP::new(true, true, false, true)
            )
            .is_err()
        );
        bus.map_device(
            Id::new(1),
            P::new(max - 7),
            RP::new(true, true, false, true),
        )
        .unwrap();
        let access = bus
            .data_access(V::new(max), 0, S::Byte, K::Write, U::Supervisor)
            .unwrap();
        bus.write_data(access, 42).unwrap();
        let mut bytes = [0];
        bus.peek(P::new(max), &mut bytes).unwrap();
        assert_eq!(bytes, [42]);
        assert_eq!(bus.devices().len(), 2);
    }
}
