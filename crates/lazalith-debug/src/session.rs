//! One process's debugging state, and the snapshot of it.
//!
//! A [`DebugSession`] is what a frontend configures before it runs anything: the
//! breakpoints, the watchpoints, and where the program is stopped. It holds no
//! machine and no kernel, so it can be inspected, compared and printed while the
//! program runs without a borrow of anything that is executing.
//!
//! # Why a session and not a controller-wide breakpoint list
//!
//! Breakpoints belong to a *process*, not to a machine. Two processes on one
//! machine have two address spaces, and an address that is a breakpoint in one is
//! just an address in the other. Putting the list on the controller would make
//! "is this a breakpoint" a question about the whole machine, and a frontend
//! showing one process's source would be showing another's stops. A session is
//! per process, so the question is asked of the right one.
//!
//! # What a snapshot here is, and is not
//!
//! [`DebugSnapshot`] captures a session's *debugging* state: its breakpoints, its
//! watchpoints, where it was stopped, and how many steps it had retired. It does
//! **not** capture the machine. Machine state — the CPU, the devices, the
//! processes — is Step 75's subject, with its own types, and a snapshot of the
//! debugging state is useful on its own: a frontend can save a session's setup,
//! run the program to completion, and put the setup back without having to type
//! the addresses again.

use alloc::vec::Vec;

use lazalith_os::{ProcessId, ThreadId};

use crate::DebugError;

/// Where a process is, from a debugger's point of view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    /// Loaded and not yet run, or run and finished with this code.
    Ready,
    /// Running: the controller will step it on the next `run` or `step`.
    Running,
    /// Stopped at an address, waiting for the frontend to say what next.
    Stopped {
        /// Where it is stopped.
        address: u64,
    },
    /// The program called `exit` with this code.
    Exited {
        /// The code it passed.
        code: u32,
    },
    /// The program faulted, and the controller cannot continue it.
    Faulted {
        /// The fault, as the machine reported it.
        detail: &'static str,
    },
}

impl ExecutionState {
    /// Whether the program can be stepped or run again.
    pub const fn is_live(&self) -> bool {
        matches!(self, Self::Ready | Self::Running | Self::Stopped { .. })
    }
}

/// How wide a watchpoint is.
///
/// Four sizes rather than an arbitrary length, because a watchpoint that is not a
/// power of two straddles two words and would have to compare both — and because
/// a frontend offering a width it cannot honour is worse than one that refuses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchpointSize {
    /// One byte.
    Byte,
    /// Two bytes.
    Half,
    /// Four bytes.
    Word,
    /// Eight bytes.
    Double,
}

impl WatchpointSize {
    /// The size in bytes.
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Byte => 1,
            Self::Half => 2,
            Self::Word => 4,
            Self::Double => 8,
        }
    }

    /// The size for a byte count, or `None` if this API does not offer it.
    pub const fn of_bytes(bytes: u64) -> Option<Self> {
        match bytes {
            1 => Some(Self::Byte),
            2 => Some(Self::Half),
            4 => Some(Self::Word),
            8 => Some(Self::Double),
            _ => None,
        }
    }
}

/// A watch on `length` bytes at `address`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Watchpoint {
    /// The first byte watched.
    pub address: u64,
    /// How many bytes are watched.
    pub size: WatchpointSize,
}

impl Watchpoint {
    /// The last byte watched, inclusive.
    pub const fn end(&self) -> u64 {
        self.address + self.size.bytes() - 1
    }

    /// Whether this watch covers `address`.
    pub const fn covers(&self, address: u64) -> bool {
        address >= self.address && address <= self.end()
    }
}

/// One process's debugging state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugSession {
    process: ProcessId,
    thread: ThreadId,
    state: ExecutionState,
    breakpoints: Vec<u64>,
    watchpoints: Vec<Watchpoint>,
    steps: u64,
    word_size: u64,
}

/// What went wrong configuring a session.
fn not_aligned(address: u64, word_size: u64) -> DebugError {
    DebugError::UnalignedBreakpoint { address, word_size }
}

impl DebugSession {
    /// A session for a process that has been loaded but not run.
    pub const fn new(process: ProcessId, thread: ThreadId, word_size: u64) -> Self {
        Self {
            process,
            thread,
            state: ExecutionState::Ready,
            breakpoints: Vec::new(),
            watchpoints: Vec::new(),
            steps: 0,
            word_size,
        }
    }

    /// The process this session is debugging.
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// The thread this session is debugging.
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// Where the process is.
    pub const fn state(&self) -> ExecutionState {
        self.state
    }

    /// Moves the session to a state. The controller owns this; a frontend reads
    /// it and cannot move a stopped program by saying so.
    pub(crate) const fn set_state(&mut self, state: ExecutionState) {
        self.state = state;
    }

    /// How many instructions this session has retired.
    pub const fn steps(&self) -> u64 {
        self.steps
    }

    /// Counts one retired instruction.
    pub(crate) fn count_step(&mut self) {
        self.steps = self.steps.saturating_add(1);
    }

    /// Sets a breakpoint, returning whether it was new.
    ///
    /// A breakpoint is refused unless it is on an instruction boundary. A
    /// breakpoint between two instructions would never be reached, and a
    /// frontend that set one and then reported "no breakpoint was hit" would be
    /// reporting a mistake of its own as a fact about the program.
    pub fn set_breakpoint(&mut self, address: u64) -> Result<bool, DebugError> {
        if address % self.word_size != 0 {
            return Err(not_aligned(address, self.word_size));
        }
        if self.breakpoints.contains(&address) {
            return Ok(false);
        }
        self.breakpoints.push(address);
        self.breakpoints.sort_unstable();
        Ok(true)
    }

    /// Removes a breakpoint, returning whether there was one.
    pub fn clear_breakpoint(&mut self, address: u64) -> bool {
        let before = self.breakpoints.len();
        self.breakpoints.retain(|kept| *kept != address);
        self.breakpoints.len() != before
    }

    /// Removes every breakpoint, returning how many there were.
    pub fn clear_breakpoints(&mut self) -> usize {
        let count = self.breakpoints.len();
        self.breakpoints.clear();
        count
    }

    /// The breakpoints, in address order.
    pub fn breakpoints(&self) -> &[u64] {
        &self.breakpoints
    }

    /// Whether `address` is a breakpoint.
    pub fn is_breakpoint(&self, address: u64) -> bool {
        self.breakpoints.contains(&address)
    }

    /// Sets a watchpoint, returning whether it was new.
    pub fn set_watchpoint(
        &mut self,
        address: u64,
        size: WatchpointSize,
    ) -> Result<bool, DebugError> {
        if self.watchpoints.iter().any(|kept| kept.address == address) {
            return Ok(false);
        }
        self.watchpoints.push(Watchpoint { address, size });
        self.watchpoints.sort_unstable_by_key(|kept| kept.address);
        Ok(true)
    }

    /// Removes a watchpoint, returning whether there was one.
    pub fn clear_watchpoint(&mut self, address: u64) -> bool {
        let before = self.watchpoints.len();
        self.watchpoints.retain(|kept| kept.address != address);
        self.watchpoints.len() != before
    }

    /// Removes every watchpoint, returning how many there were.
    pub fn clear_watchpoints(&mut self) -> usize {
        let count = self.watchpoints.len();
        self.watchpoints.clear();
        count
    }

    /// The watchpoints, in address order.
    pub fn watchpoints(&self) -> &[Watchpoint] {
        &self.watchpoints
    }

    /// Captures the debugging state.
    pub fn snapshot(&self) -> DebugSnapshot {
        DebugSnapshot {
            process: self.process,
            thread: self.thread,
            breakpoints: self.breakpoints.clone(),
            watchpoints: self.watchpoints.clone(),
            state: self.state,
            steps: self.steps,
        }
    }

    /// Puts the debugging state back.
    ///
    /// The process and thread must match: restoring a session's breakpoints into
    /// a *different* process is not a mistake the caller can have meant, and
    /// silently doing it would put stops in the wrong address space.
    pub fn restore(&mut self, snapshot: &DebugSnapshot) -> Result<(), DebugError> {
        if snapshot.process != self.process || snapshot.thread != self.thread {
            return Err(DebugError::Snapshot(String::from(
                "the snapshot is of a different process",
            )));
        }
        self.breakpoints = snapshot.breakpoints.clone();
        self.watchpoints = snapshot.watchpoints.clone();
        self.state = snapshot.state;
        self.steps = snapshot.steps;
        Ok(())
    }
}

/// A process's debugging state, held on its own.
///
/// This is the snapshot a frontend saves and restores. It is deliberately *not*
/// machine state: capturing the CPU, the devices and the processes is Step 75's
/// subject with its own types, and folding a partial version of it in here would
/// mean two definitions of "the state of a running program" that disagree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugSnapshot {
    process: ProcessId,
    thread: ThreadId,
    breakpoints: Vec<u64>,
    watchpoints: Vec<Watchpoint>,
    state: ExecutionState,
    steps: u64,
}

impl DebugSnapshot {
    /// The process this snapshot is of.
    pub const fn process(&self) -> ProcessId {
        self.process
    }

    /// The thread this snapshot is of.
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// The breakpoints at the time of the snapshot.
    pub fn breakpoints(&self) -> &[u64] {
        &self.breakpoints
    }

    /// The watchpoints at the time of the snapshot.
    pub fn watchpoints(&self) -> &[Watchpoint] {
        &self.watchpoints
    }

    /// Where the process was at the time of the snapshot.
    pub const fn state(&self) -> ExecutionState {
        self.state
    }

    /// How many instructions the process had retired at the time.
    pub const fn steps(&self) -> u64 {
        self.steps
    }
}
