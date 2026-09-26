//! The owned copy of a machine's registers a frontend is given.
//!
//! This type is the reason a debug frontend cannot manipulate CPU internals.
//! [`DebugController::registers`] returns one of these *by value*: the caller
//! gets sixteen numbers, the program counter, the stack pointer and the status
//! register, and there is no path from what it holds back into the machine.
//!
//! A `&RegisterFile` would have been the natural signature and would have been
//! the wrong one. `RegisterFile` is the CPU's own storage, so a shared reference
//! to it is a read-only view of live state that changes under the caller — a
//! frontend reading `r0` twice would get two different answers with no step in
//! between, and a frontend that decided it needed to *write* would find the
//! obvious next step is to ask for a mutable reference. An owned copy is a
//! consistent snapshot, and the absence of a write path is the point.

use lazalith_cpu::ArchitecturalState;
use lazalith_cpu::{Privilege, StatusRegister};
use lazalith_types::RegisterIndex;

/// How many general registers this target has.
pub const REGISTER_COUNT: usize = RegisterIndex::COUNT as usize;

/// One register's number and value, as a frontend is given it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegisterValue {
    /// The register's index, zero-based.
    pub index: u8,
    /// Its value, truncated to this target's word width by the machine.
    pub value: u64,
}

/// A consistent snapshot of every register a program can see.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterSnapshot {
    general: [u64; REGISTER_COUNT],
    pc: u64,
    sp: u64,
    status: StatusRegister,
    privilege: Privilege,
}

impl RegisterSnapshot {
    /// Takes a snapshot of `state`.
    ///
    /// The state is read once, so every field in the result is the value it had
    /// at the same instant. That is the whole property: a frontend that reads
    /// `pc` and `sp` is looking at one moment, not two.
    pub fn of(state: &ArchitecturalState) -> Self {
        let mut general = [0u64; REGISTER_COUNT];
        for (index, slot) in general.iter_mut().enumerate() {
            let register = RegisterIndex::try_from(index as u8).expect("the index is in range");
            *slot = state.registers().read(register);
        }
        Self {
            general,
            pc: state.pc().as_u64(),
            sp: state.sp().as_u64(),
            status: state.status(),
            privilege: state.privilege(),
        }
    }

    /// The value of general register `index`, or `None` if there is no such
    /// register.
    pub fn get(&self, index: u8) -> Option<u64> {
        let register = RegisterIndex::try_from(index).ok()?;
        Some(self.general[register.as_usize()])
    }

    /// Every general register, in index order.
    pub fn general(&self) -> impl Iterator<Item = RegisterValue> + '_ {
        self.general
            .iter()
            .enumerate()
            .map(|(index, value)| RegisterValue {
                index: index as u8,
                value: *value,
            })
    }

    /// The program counter.
    pub const fn pc(&self) -> u64 {
        self.pc
    }

    /// The stack pointer.
    pub const fn sp(&self) -> u64 {
        self.sp
    }

    /// The status register.
    ///
    /// `StatusRegister` is a small set of predicates rather than a bit field, so
    /// a frontend asks `status().zero()` rather than masking a word. It is
    /// `Copy`, so handing it out hands out a value and not a view.
    pub const fn status(&self) -> StatusRegister {
        self.status
    }

    /// The privilege the program is running at.
    pub const fn privilege(&self) -> Privilege {
        self.privilege
    }
}
