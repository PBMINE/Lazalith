#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{error::Error, fmt};
use lazalith_cpu::{ArchitecturalState, OutcomeApplication, ReferenceInterpreter};
use lazalith_devices::{Device, DeviceId, DeviceManager};
use lazalith_memory::{AddressSpace, Bus, MemoryFault, MemoryRegion, RegionPermissions};
use lazalith_types::{
    ArchitectureConfig, CycleCount, InstructionAddress, PhysicalAddress, VirtualAddress,
    VirtualClock,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MachineEvent {
    Stepped { application: OutcomeApplication },
    Halted,
}

#[derive(Debug)]
pub enum MachineError {
    Fault(Box<lazalith_cpu::CpuFault<MemoryFault>>),
    Clock(lazalith_types::ClockOverflow),
    Device(lazalith_devices::DeviceError),
    Memory(lazalith_memory::MemoryFault),
    Cpu(lazalith_cpu::ControlStateError),
    Halted,
    InstructionCountOverflow,
    InitialClock {
        devices: CycleCount,
        requested: CycleCount,
    },
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "machine rejected operation: {self:?}")
    }
}

impl Error for MachineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Fault(source) => Some(source),
            Self::Clock(source) => Some(source),
            Self::Device(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::Cpu(source) => Some(source),
            Self::Halted | Self::InstructionCountOverflow | Self::InitialClock { .. } => None,
        }
    }
}

#[derive(Debug)]
pub struct MachineSetup<D: Device> {
    pub config: ArchitectureConfig,
    pub devices: DeviceManager<D>,
    pub regions: Vec<MemoryRegion>,
    pub pc: InstructionAddress,
    pub sp: VirtualAddress,
    pub status: u64,
    pub initial_time: CycleCount,
}

#[derive(Debug)]
pub struct LazalithMachine<D: Device> {
    cpu: ReferenceInterpreter,
    bus: Bus<D>,
    config: ArchitectureConfig,
    clock: VirtualClock,
    halted: bool,
    executed: u64,
}

impl<D: Device> LazalithMachine<D> {
    pub fn new(setup: MachineSetup<D>) -> Result<Self, MachineError> {
        let MachineSetup {
            config,
            devices,
            regions,
            pc,
            sp,
            status,
            initial_time,
        } = setup;
        let mut space = AddressSpace::new(config);
        for region in regions {
            space.map(region).map_err(MachineError::Memory)?;
        }
        let device_time = devices.clock().elapsed();
        let delta = initial_time
            .checked_sub(device_time)
            .ok_or(MachineError::InitialClock {
                devices: device_time,
                requested: initial_time,
            })?;
        let cpu = ReferenceInterpreter::new(
            ArchitecturalState::new(config, pc, sp, status).map_err(MachineError::Cpu)?,
        );
        let mut bus = Bus::with_devices(space, devices);
        bus.tick_devices(delta).map_err(MachineError::Device)?;
        let clock = VirtualClock::at(initial_time);
        Ok(Self {
            cpu,
            bus,
            config,
            clock,
            halted: false,
            executed: 0,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }
    pub const fn clock(&self) -> &VirtualClock {
        &self.clock
    }
    pub const fn is_halted(&self) -> bool {
        self.halted
    }
    pub fn architectural_state(&self) -> &ArchitecturalState {
        self.cpu.architectural_state()
    }
    pub fn devices(&self) -> &DeviceManager<D> {
        self.bus.devices()
    }
    pub const fn memory(&self) -> &AddressSpace {
        self.bus.address_space()
    }

    pub fn peek_memory(
        &self,
        address: PhysicalAddress,
        output: &mut [u8],
    ) -> Result<(), MemoryFault> {
        self.bus.peek(address, output)
    }

    pub fn load_region(&mut self, region: MemoryRegion) -> Result<(), MachineError> {
        self.bus.map(region).map_err(MachineError::Memory)
    }

    pub fn load_bytes(
        &mut self,
        address: PhysicalAddress,
        bytes: &[u8],
    ) -> Result<(), MachineError> {
        self.bus
            .initialize(address, bytes)
            .map_err(MachineError::Memory)
    }

    pub fn map_device(
        &mut self,
        id: DeviceId,
        start: PhysicalAddress,
        permissions: RegionPermissions,
    ) -> Result<(), MachineError> {
        self.bus
            .map_device(id, start, permissions)
            .map_err(MachineError::Memory)
    }

    pub fn step(&mut self) -> Result<MachineEvent, MachineError> {
        if self.halted || self.cpu.execution_state() == lazalith_cpu::ExecutionState::Halted {
            return Err(MachineError::Halted);
        }
        let executed = self
            .executed
            .checked_add(1)
            .ok_or(MachineError::InstructionCountOverflow)?;
        let application = self
            .cpu
            .step(&mut self.bus)
            .map_err(|fault| MachineError::Fault(Box::new(fault)))?;
        self.executed = executed;
        if application == OutcomeApplication::Halted {
            self.halted = true;
            return Ok(MachineEvent::Halted);
        }
        Ok(MachineEvent::Stepped { application })
    }

    pub fn run(&mut self, limit: u64) -> Result<MachineRun, MachineError> {
        let mut executed = 0u64;
        let mut trap = None;
        let halted_at = loop {
            if executed == limit {
                break None;
            }
            match self.step()? {
                MachineEvent::Halted => {
                    executed += 1;
                    break Some(self.executed);
                }
                MachineEvent::Stepped { application } => {
                    executed += 1;
                    if let OutcomeApplication::Trap { request, resume_pc } = application {
                        trap = Some((request, resume_pc));
                        break None;
                    }
                }
            }
        };
        Ok(MachineRun {
            executed,
            halted_at,
            trap,
        })
    }

    pub fn advance_clock(&mut self, delta: CycleCount) -> Result<(), MachineError> {
        let next = self.clock.advanced(delta).map_err(MachineError::Clock)?;
        self.bus.tick_devices(delta).map_err(MachineError::Device)?;
        self.clock = next;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineRun {
    pub executed: u64,
    pub halted_at: Option<u64>,
    pub trap: Option<(lazalith_cpu::TrapRequest, InstructionAddress)>,
}
