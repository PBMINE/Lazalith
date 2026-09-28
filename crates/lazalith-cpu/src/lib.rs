#![no_std]

extern crate alloc;

mod fault;
mod interpreter;
mod memory;
mod outcome;
mod trap;

pub use fault::{CpuFault, CpuFaultCause, FaultOrigin};
pub use interpreter::ReferenceInterpreter;
pub use memory::{CpuMemory, DataAccess, DataAccessError, DataAccessKind, FetchedInstruction};
mod registers;

pub use outcome::{
    ControlTarget, ExecutionOutcome, OutcomeApplication, OutcomeError, OutcomeErrorKind,
    PreparedOutcome, StackEffect, TrapRequest, checked_next_pc, checked_return_sp, prepare_outcome,
};

mod state;
mod status;

pub use registers::RegisterFile;
pub use state::{
    ArchitecturalState, ControlStateError, DebugState, ExecutionState, SpecialRegister,
    validate_pc, validate_sp,
};
pub use status::{InvalidStatus, Privilege, StatusRegister};
pub use trap::{
    DoubleTrap, ExecutionContextId, PrepareEntryError, SyscallAdmission, SyscallCompletion,
    TrapAttempt, TrapCause, TrapController, TrapFrame, TrapSnapshot,
};
