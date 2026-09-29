#![no_std]

extern crate alloc;

mod engine;
mod fast;
mod fault;
mod interpreter;
mod memory;
mod outcome;
mod processor;
mod trap;

pub use engine::{EngineError, EngineKind, ExecutionEngine};
pub use fast::FastInterpreter;
pub use fault::{CpuFault, CpuFaultCause, FaultOrigin};
pub use interpreter::ReferenceInterpreter;
pub use memory::{CpuMemory, DataAccess, DataAccessError, DataAccessKind, FetchedInstruction};
mod registers;

pub use outcome::{
    ControlTarget, ExecutionOutcome, OutcomeApplication, OutcomeError, OutcomeErrorKind,
    PreparedOutcome, StackEffect, StepResult, TrapRequest, checked_next_pc, checked_return_sp,
    prepare_outcome,
};

pub use processor::Processor;

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

pub(crate) use trap::prepare_entry_control;
