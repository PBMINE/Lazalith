use crate::filesystem::{FileAccess, FileNodeId};
use crate::syscall::ProcessContextParts;
use crate::{
    MemoryError, StackRegion, USER_CODE_LENGTH, USER_CODE_START, UserMemory, UserMemoryContext,
    UserMemoryError,
};
use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt, num::NonZeroU32};
use lazalith_cpu::{
    ArchitecturalState, ControlStateError, ExecutionContextId, Privilege, StatusRegister,
    validate_pc, validate_sp,
};
use lazalith_memory::{RegionKind, RegionPermissions, UserSpace};
use lazalith_os_abi::{FileHandle, ProcessHandle};
use lazalith_types::{ArchitectureConfig, InstructionAddress, PhysicalAddress, VirtualAddress};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessId(NonZeroU32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ThreadId(NonZeroU32);

impl ProcessId {
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl ThreadId {
    pub const fn new(value: u32) -> Option<Self> {
        match NonZeroU32::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessState {
    Created,
    Ready,
    Running,
    Blocked,
    Exited,
    Faulted,
}

impl ProcessState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Faulted)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ProcessExecutionError {
    NotRunning {
        process_id: ProcessId,
        state: ProcessState,
    },
    AlreadyBound {
        process_id: ProcessId,
        context: ExecutionContextId,
    },
    NotBound {
        process_id: ProcessId,
    },
    ContextMismatch {
        expected: ExecutionContextId,
        actual: ExecutionContextId,
    },
}

impl fmt::Display for ProcessExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning { process_id, state } => {
                write!(f, "process {} is {state:?}, not Running", process_id.get())
            }
            Self::AlreadyBound {
                process_id,
                context,
            } => write!(
                f,
                "process {} is already bound to execution context {}",
                process_id.get(),
                context.get()
            ),
            Self::NotBound { process_id } => {
                write!(f, "process {} has no execution context", process_id.get())
            }
            Self::ContextMismatch { expected, actual } => write!(
                f,
                "execution context {} does not match {}",
                actual.get(),
                expected.get()
            ),
        }
    }
}

impl Error for ProcessExecutionError {}

#[derive(Debug)]
pub enum ProgramError {
    Empty,
    EntryMisaligned {
        offset: u64,
        alignment: u8,
    },
    EntryOutsideImage {
        offset: u64,
        length: u64,
    },
    ImageTooLarge {
        length: u64,
        maximum: u64,
    },
    ArchitectureMismatch {
        image: ArchitectureConfig,
        memory: ArchitectureConfig,
    },
    AddressOverflow,
    Allocation(TryReserveError),
    Load(MemoryError),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("program image is empty"),
            Self::EntryMisaligned { offset, alignment } => {
                write!(f, "entry offset {offset:#x} is not aligned to {alignment}")
            }
            Self::EntryOutsideImage { offset, length } => {
                write!(
                    f,
                    "entry offset {offset:#x} is outside image length {length:#x}"
                )
            }
            Self::ImageTooLarge { length, maximum } => {
                write!(f, "program image length {length:#x} exceeds {maximum:#x}")
            }
            Self::ArchitectureMismatch { image, memory } => {
                write!(f, "program image uses {image:?}, memory uses {memory:?}")
            }
            Self::AddressOverflow => f.write_str("program image address overflows"),
            Self::Allocation(source) => write!(f, "program image allocation failed: {source}"),
            Self::Load(source) => write!(f, "program image load failed: {source}"),
        }
    }
}

impl Error for ProgramError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Load(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ThreadError {
    StackOutside {
        thread_id: ThreadId,
        stack_pointer: VirtualAddress,
    },
    ArchitectureMismatch {
        thread_id: ThreadId,
        stack: ArchitectureConfig,
        thread: ArchitectureConfig,
    },
    EntryOutsideImage {
        thread_id: ThreadId,
        entry: InstructionAddress,
    },
    DuplicateThread {
        process_id: ProcessId,
        thread_id: ThreadId,
    },
    ForeignProcess {
        expected: ProcessId,
        actual: ProcessId,
    },
    UnknownThread(ThreadId),
    ThreadAllocation(TryReserveError),
    ProcessNotRunning {
        process_id: ProcessId,
        state: ProcessState,
    },
    InvalidPrivilege {
        thread_id: ThreadId,
        privilege: Privilege,
    },
    Cpu {
        thread_id: ThreadId,
        source: ControlStateError,
    },
}

impl fmt::Display for ThreadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StackOutside {
                thread_id,
                stack_pointer,
            } => write!(
                f,
                "thread {} stack pointer {:?} is outside the process stack",
                thread_id.get(),
                stack_pointer
            ),
            Self::ArchitectureMismatch {
                thread_id,
                stack,
                thread,
            } => write!(
                f,
                "thread {} uses {thread:?}, stack uses {stack:?}",
                thread_id.get()
            ),
            Self::EntryOutsideImage { thread_id, entry } => write!(
                f,
                "thread {} entry {entry:?} is outside the process image",
                thread_id.get()
            ),
            Self::DuplicateThread {
                process_id,
                thread_id,
            } => write!(
                f,
                "process {} already owns thread {}",
                process_id.get(),
                thread_id.get()
            ),
            Self::ForeignProcess { expected, actual } => write!(
                f,
                "thread belongs to process {}, expected {}",
                actual.get(),
                expected.get()
            ),
            Self::UnknownThread(thread_id) => {
                write!(f, "process has no thread {}", thread_id.get())
            }
            Self::ThreadAllocation(source) => {
                write!(f, "thread allocation failed: {source}")
            }
            Self::ProcessNotRunning { process_id, state } => {
                write!(f, "process {} is {state:?}, not Running", process_id.get())
            }
            Self::InvalidPrivilege {
                thread_id,
                privilege,
            } => write!(
                f,
                "thread {} CPU context has {privilege:?} privilege",
                thread_id.get()
            ),
            Self::Cpu { thread_id, source } => {
                write!(
                    f,
                    "thread {} CPU state is invalid: {source}",
                    thread_id.get()
                )
            }
        }
    }
}

impl Error for ThreadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Cpu { source, .. } => Some(source),
            Self::ThreadAllocation(source) => Some(source),
            Self::StackOutside { .. }
            | Self::ArchitectureMismatch { .. }
            | Self::EntryOutsideImage { .. }
            | Self::DuplicateThread { .. }
            | Self::ForeignProcess { .. }
            | Self::UnknownThread(_)
            | Self::ProcessNotRunning { .. }
            | Self::InvalidPrivilege { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum ProcessError {
    Memory(MemoryError),
    Program(ProgramError),
    Thread(ThreadError),
    ThreadAllocation(TryReserveError),
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(source) => write!(f, "process memory failed: {source}"),
            Self::Program(source) => write!(f, "process program failed: {source}"),
            Self::Thread(source) => write!(f, "process thread failed: {source}"),
            Self::ThreadAllocation(source) => {
                write!(f, "process thread allocation failed: {source}")
            }
        }
    }
}

impl Error for ProcessError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Memory(source) => Some(source),
            Self::Program(source) => Some(source),
            Self::Thread(source) => Some(source),
            Self::ThreadAllocation(source) => Some(source),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessStateError {
    InvalidTransition {
        process_id: ProcessId,
        from: ProcessState,
        to: ProcessState,
    },
    Terminal {
        process_id: ProcessId,
        state: ProcessState,
    },
    NotInSyscall {
        process_id: ProcessId,
    },
}

impl fmt::Display for ProcessStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition {
                process_id,
                from,
                to,
            } => write!(
                f,
                "process {} cannot transition from {from:?} to {to:?}",
                process_id.get()
            ),
            Self::Terminal { process_id, state } => write!(
                f,
                "process {} is terminal in state {state:?}",
                process_id.get()
            ),
            Self::NotInSyscall { process_id } => write!(
                f,
                "process {} can only be terminated from a dispatched syscall",
                process_id.get()
            ),
        }
    }
}

impl Error for ProcessStateError {}

#[derive(Debug)]
pub enum HandleError {
    Duplicate { entry: ProcessHandleEntry },
    NotFound { entry: ProcessHandleEntry },
    Exhausted,
    Allocation(TryReserveError),
}

impl fmt::Display for HandleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { entry } => write!(f, "process handle {entry:?} is already owned"),
            Self::NotFound { entry } => write!(f, "process handle {entry:?} is not owned"),
            Self::Exhausted => f.write_str("process file handles are exhausted"),
            Self::Allocation(source) => write!(f, "process handle allocation failed: {source}"),
        }
    }
}

impl Error for HandleError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessHandleEntry {
    File(FileHandle),
    Child(ProcessHandle),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenFile {
    handle: FileHandle,
    node: FileNodeId,
    offset: u64,
    access: FileAccess,
}

impl OpenFile {
    pub const fn handle(self) -> FileHandle {
        self.handle
    }

    pub const fn node(self) -> FileNodeId {
        self.node
    }

    pub const fn offset(self) -> u64 {
        self.offset
    }

    pub(crate) fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    pub const fn access(self) -> FileAccess {
        self.access
    }
}

pub(crate) struct PreparedFile {
    pub(crate) handle: FileHandle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessHandles {
    entries: Vec<ProcessHandleEntry>,
    files: Vec<OpenFile>,
    next_file_handle: Option<u32>,
}

impl ProcessHandles {
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
            files: Vec::new(),
            next_file_handle: Some(2),
        }
    }

    pub fn insert(&mut self, entry: ProcessHandleEntry) -> Result<(), HandleError> {
        if self.entries.contains(&entry) {
            return Err(HandleError::Duplicate { entry });
        }
        self.entries
            .try_reserve(1)
            .map_err(HandleError::Allocation)?;
        if let ProcessHandleEntry::File(handle) = entry {
            self.observe_file_handle(handle);
        }
        self.entries.push(entry);
        Ok(())
    }

    pub fn remove(&mut self, entry: ProcessHandleEntry) -> Result<(), HandleError> {
        let index = self
            .entries
            .iter()
            .position(|candidate| *candidate == entry)
            .ok_or(HandleError::NotFound { entry })?;
        if let ProcessHandleEntry::File(handle) = entry {
            self.files.retain(|file| file.handle != handle);
        }
        self.entries.remove(index);
        Ok(())
    }

    pub fn contains(&self, entry: ProcessHandleEntry) -> bool {
        self.entries.contains(&entry)
    }

    pub fn open_file(
        &mut self,
        node: FileNodeId,
        access: FileAccess,
    ) -> Result<FileHandle, HandleError> {
        let prepared = self.reserve_file()?;
        let handle = prepared.handle;
        self.commit_prepared_file(prepared, node, access)?;
        Ok(handle)
    }

    pub(crate) fn reserve_file(&mut self) -> Result<PreparedFile, HandleError> {
        let raw = self.next_file_handle.ok_or(HandleError::Exhausted)?;
        let handle = FileHandle::new(raw).map_err(|_| HandleError::Exhausted)?;
        self.entries
            .try_reserve(1)
            .map_err(HandleError::Allocation)?;
        self.files.try_reserve(1).map_err(HandleError::Allocation)?;
        Ok(PreparedFile { handle })
    }

    pub(crate) fn commit_prepared_file(
        &mut self,
        prepared: PreparedFile,
        node: FileNodeId,
        access: FileAccess,
    ) -> Result<(), HandleError> {
        let entry = ProcessHandleEntry::File(prepared.handle);
        if self.entries.contains(&entry)
            || self.files.iter().any(|file| file.handle == prepared.handle)
        {
            return Err(HandleError::Duplicate { entry });
        }
        self.next_file_handle = prepared.handle.get().checked_add(1);
        self.entries.push(entry);
        self.files.push(OpenFile {
            handle: prepared.handle,
            node,
            offset: 0,
            access,
        });
        Ok(())
    }

    pub fn file(&self, handle: FileHandle) -> Result<&OpenFile, HandleError> {
        self.files
            .iter()
            .find(|file| file.handle == handle)
            .ok_or(HandleError::NotFound {
                entry: ProcessHandleEntry::File(handle),
            })
    }

    pub fn file_mut(&mut self, handle: FileHandle) -> Result<&mut OpenFile, HandleError> {
        self.files
            .iter_mut()
            .find(|file| file.handle == handle)
            .ok_or(HandleError::NotFound {
                entry: ProcessHandleEntry::File(handle),
            })
    }

    pub fn close_file(&mut self, handle: FileHandle) -> Result<(), HandleError> {
        let file_index = self
            .files
            .iter()
            .position(|file| file.handle == handle)
            .ok_or(HandleError::NotFound {
                entry: ProcessHandleEntry::File(handle),
            })?;
        let entry_index = self
            .entries
            .iter()
            .position(|entry| *entry == ProcessHandleEntry::File(handle))
            .ok_or(HandleError::NotFound {
                entry: ProcessHandleEntry::File(handle),
            })?;
        self.files.remove(file_index);
        self.entries.remove(entry_index);
        Ok(())
    }

    pub fn files(&self) -> &[OpenFile] {
        &self.files
    }

    fn observe_file_handle(&mut self, handle: FileHandle) {
        let Some(next) = self.next_file_handle else {
            return;
        };
        if handle.get() >= next {
            self.next_file_handle = handle.get().checked_add(1);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[ProcessHandleEntry] {
        &self.entries
    }
}

impl Default for ProcessHandles {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone)]
pub struct ProgramImage {
    config: ArchitectureConfig,
    entry: InstructionAddress,
    bytes: Vec<u8>,
}

impl ProgramImage {
    pub fn new(
        config: ArchitectureConfig,
        entry_offset: u64,
        bytes: &[u8],
    ) -> Result<Self, ProgramError> {
        let length = u64::try_from(bytes.len()).map_err(|_| ProgramError::ImageTooLarge {
            length: u64::MAX,
            maximum: USER_CODE_LENGTH,
        })?;
        if bytes.is_empty() {
            return Err(ProgramError::Empty);
        }
        if length > USER_CODE_LENGTH {
            return Err(ProgramError::ImageTooLarge {
                length,
                maximum: USER_CODE_LENGTH,
            });
        }
        if !entry_offset.is_multiple_of(u64::from(config.instruction_alignment())) {
            return Err(ProgramError::EntryMisaligned {
                offset: entry_offset,
                alignment: config.instruction_alignment(),
            });
        }
        let instruction_end = entry_offset
            .checked_add(8)
            .ok_or(ProgramError::AddressOverflow)?;
        if entry_offset >= length || instruction_end > length {
            return Err(ProgramError::EntryOutsideImage {
                offset: entry_offset,
                length,
            });
        }
        let mut stored = Vec::new();
        stored
            .try_reserve_exact(bytes.len())
            .map_err(ProgramError::Allocation)?;
        stored.extend_from_slice(bytes);
        let entry_address = USER_CODE_START
            .checked_add(entry_offset)
            .ok_or(ProgramError::AddressOverflow)?;
        Ok(Self {
            config,
            entry: InstructionAddress::new(entry_address),
            bytes: stored,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn entry(&self) -> InstructionAddress {
        self.entry
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn contains_entry(&self, entry: InstructionAddress) -> bool {
        let start = USER_CODE_START;
        let Some(offset) = entry.as_u64().checked_sub(start) else {
            return false;
        };
        let Some(end) = offset.checked_add(8) else {
            return false;
        };
        offset.is_multiple_of(u64::from(self.config.instruction_alignment()))
            && end <= self.bytes.len() as u64
    }

    pub fn load_into(&self, memory: &mut UserMemory) -> Result<(), ProgramError> {
        if memory.config() != self.config {
            return Err(ProgramError::ArchitectureMismatch {
                image: self.config,
                memory: memory.config(),
            });
        }
        memory.load_code(0, &self.bytes).map_err(ProgramError::Load)
    }
}

impl fmt::Debug for ProgramImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgramImage")
            .field("config", &self.config)
            .field("entry", &self.entry)
            .field("length", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct Thread {
    process_id: ProcessId,
    id: ThreadId,
    cpu: ArchitecturalState,
}

impl Thread {
    fn from_stack(
        process_id: ProcessId,
        id: ThreadId,
        config: ArchitectureConfig,
        entry: InstructionAddress,
        stack: StackRegion,
        initial_sp: VirtualAddress,
    ) -> Result<Self, ThreadError> {
        if stack.config() != config {
            return Err(ThreadError::ArchitectureMismatch {
                thread_id: id,
                stack: stack.config(),
                thread: config,
            });
        }
        if !stack.contains_virtual(initial_sp) {
            return Err(ThreadError::StackOutside {
                thread_id: id,
                stack_pointer: initial_sp,
            });
        }
        let status = StatusRegister::new(Privilege::User, false).bits();
        let cpu = ArchitecturalState::new(config, entry, initial_sp, status).map_err(|source| {
            ThreadError::Cpu {
                thread_id: id,
                source,
            }
        })?;
        Ok(Self {
            process_id,
            id,
            cpu,
        })
    }

    pub fn for_process(
        id: ThreadId,
        process: &Process,
        entry: InstructionAddress,
        initial_sp: VirtualAddress,
    ) -> Result<Self, ThreadError> {
        if !process.program.contains_entry(entry) {
            return Err(ThreadError::EntryOutsideImage {
                thread_id: id,
                entry,
            });
        }
        let stack = process.stack;
        if stack.config() != process.program.config() {
            return Err(ThreadError::ArchitectureMismatch {
                thread_id: id,
                stack: stack.config(),
                thread: process.program.config(),
            });
        }
        let address = PhysicalAddress::new(initial_sp.as_u64());
        let mapped = process
            .memory
            .address_space()
            .regions()
            .iter()
            .any(|region| {
                region.start() <= address
                    && address <= region.end()
                    && region.kind() == RegionKind::Ram
                    && region.permissions() == RegionPermissions::new(true, true, false, true)
            });
        if !mapped || !stack.contains_virtual(initial_sp) {
            return Err(ThreadError::StackOutside {
                thread_id: id,
                stack_pointer: initial_sp,
            });
        }
        Self::from_stack(
            process.id,
            id,
            process.program.config(),
            entry,
            stack,
            initial_sp,
        )
    }

    pub const fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub const fn id(&self) -> ThreadId {
        self.id
    }

    pub const fn cpu(&self) -> &ArchitecturalState {
        &self.cpu
    }

    pub(crate) fn replace_cpu(&mut self, cpu: ArchitecturalState) {
        self.cpu = cpu;
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("process_id", &self.process_id)
            .field("id", &self.id)
            .field("cpu", &self.cpu)
            .finish()
    }
}

pub struct ProcessParts {
    pub id: ProcessId,
    pub state: ProcessState,
    pub exit_code: Option<u32>,
    pub memory: UserMemory,
    pub stack: StackRegion,
    pub program: ProgramImage,
    pub threads: Vec<Thread>,
    pub handles: ProcessHandles,
    pub execution_context: Option<ExecutionContextId>,
}

#[derive(Clone)]
pub struct Process {
    id: ProcessId,
    state: ProcessState,
    exit_code: Option<u32>,
    memory: UserMemory,
    stack: StackRegion,
    program: ProgramImage,
    threads: Vec<Thread>,
    handles: ProcessHandles,
    execution_context: Option<ExecutionContextId>,
}

impl Process {
    /// Puts back a state this process produced.
    ///
    /// The whole process is replaced, not patched field by field. A snapshot is a
    /// clone of this struct, and a restore that assigned only the fields it
    /// remembered would leave a process that was *almost* the one that was saved
    /// — with, say, the handles of the snapshot and the threads of whatever ran
    /// in between. Replacing the whole value is the only version that cannot
    /// forget a field, and the identity check is what keeps it from putting one
    /// process's memory into another.
    pub fn restore(&mut self, snapshot: &Self) {
        debug_assert_eq!(self.id, snapshot.id, "restoring into the wrong process");
        *self = snapshot.clone();
    }
}

impl Process {
    pub fn new(
        id: ProcessId,
        thread_id: ThreadId,
        program: ProgramImage,
    ) -> Result<Self, ProcessError> {
        let mut memory = UserMemory::new(program.config()).map_err(ProcessError::Memory)?;
        program
            .load_into(&mut memory)
            .map_err(ProcessError::Program)?;
        let stack = memory.layout().stack();
        let thread = Thread::from_stack(
            id,
            thread_id,
            program.config(),
            program.entry(),
            stack,
            stack.initial_sp(),
        )
        .map_err(ProcessError::Thread)?;
        let mut threads = Vec::new();
        threads
            .try_reserve(1)
            .map_err(ProcessError::ThreadAllocation)?;
        threads.push(thread);
        Ok(Self {
            id,
            state: ProcessState::Created,
            exit_code: None,
            memory,
            stack,
            program,
            threads,
            handles: ProcessHandles::new(),
            execution_context: None,
        })
    }

    pub const fn id(&self) -> ProcessId {
        self.id
    }

    pub const fn execution_context(&self) -> Option<ExecutionContextId> {
        self.execution_context
    }

    pub fn activate(&mut self, context: ExecutionContextId) -> Result<(), ProcessExecutionError> {
        if self.state != ProcessState::Running {
            return Err(ProcessExecutionError::NotRunning {
                process_id: self.id,
                state: self.state,
            });
        }
        if let Some(existing) = self.execution_context {
            return Err(ProcessExecutionError::AlreadyBound {
                process_id: self.id,
                context: existing,
            });
        }
        self.execution_context = Some(context);
        Ok(())
    }

    pub fn deactivate(&mut self, context: ExecutionContextId) -> Result<(), ProcessExecutionError> {
        match self.execution_context {
            None => Err(ProcessExecutionError::NotBound {
                process_id: self.id,
            }),
            Some(existing) if existing != context => Err(ProcessExecutionError::ContextMismatch {
                expected: context,
                actual: existing,
            }),
            Some(_) => {
                self.execution_context = None;
                Ok(())
            }
        }
    }

    pub const fn state(&self) -> ProcessState {
        self.state
    }

    pub const fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    pub fn mark_ready(&mut self) -> Result<(), ProcessStateError> {
        self.transition(ProcessState::Ready)
    }

    pub fn mark_running(&mut self) -> Result<(), ProcessStateError> {
        self.transition(ProcessState::Running)
    }

    pub fn preempt(&mut self) -> Result<(), ProcessStateError> {
        self.transition(ProcessState::Ready)
    }

    pub(crate) fn preempt_after_release(&mut self) {
        self.state = ProcessState::Ready;
    }

    pub fn block(&mut self) -> Result<(), ProcessStateError> {
        self.transition(ProcessState::Blocked)
    }

    pub fn exit(&mut self, code: u32) -> Result<(), ProcessStateError> {
        if self.state.is_terminal() {
            return Err(ProcessStateError::Terminal {
                process_id: self.id,
                state: self.state,
            });
        }
        self.state = ProcessState::Exited;
        self.exit_code = Some(code);
        Ok(())
    }

    pub fn fault(&mut self) -> Result<(), ProcessStateError> {
        self.transition(ProcessState::Faulted)
    }

    pub(crate) fn validate_release(&self, exit_code: Option<u32>) -> Result<(), ProcessStateError> {
        if self.state.is_terminal()
            && !(exit_code.is_some()
                && self.state == ProcessState::Exited
                && self.exit_code == exit_code)
        {
            return Err(ProcessStateError::Terminal {
                process_id: self.id,
                state: self.state,
            });
        }
        Ok(())
    }

    pub(crate) fn apply_release(&mut self, exit_code: Option<u32>) {
        if let Some(code) = exit_code {
            if self.state != ProcessState::Exited {
                self.state = ProcessState::Exited;
                self.exit_code = Some(code);
            }
        } else if !self.state.is_terminal() {
            self.state = ProcessState::Faulted;
        }
    }

    pub(crate) fn clear_execution_context(&mut self) {
        self.execution_context = None;
    }

    fn transition(&mut self, next: ProcessState) -> Result<(), ProcessStateError> {
        if self.state.is_terminal() {
            return Err(ProcessStateError::Terminal {
                process_id: self.id,
                state: self.state,
            });
        }
        let valid = matches!(
            (self.state, next),
            (
                ProcessState::Created,
                ProcessState::Ready | ProcessState::Faulted | ProcessState::Exited
            ) | (
                ProcessState::Ready,
                ProcessState::Running
                    | ProcessState::Blocked
                    | ProcessState::Faulted
                    | ProcessState::Exited
            ) | (
                ProcessState::Running,
                ProcessState::Ready
                    | ProcessState::Blocked
                    | ProcessState::Faulted
                    | ProcessState::Exited
            ) | (
                ProcessState::Blocked,
                ProcessState::Ready | ProcessState::Faulted | ProcessState::Exited
            )
        );
        if valid {
            self.state = next;
            Ok(())
        } else {
            Err(ProcessStateError::InvalidTransition {
                process_id: self.id,
                from: self.state,
                to: next,
            })
        }
    }

    pub const fn memory(&self) -> &UserMemory {
        &self.memory
    }

    pub(crate) fn memory_mut(&mut self) -> &mut UserMemory {
        &mut self.memory
    }

    pub fn memory_context(&mut self) -> Result<UserMemoryContext<'_>, UserMemoryError> {
        let thread_id = self.primary_thread().id;
        self.memory_context_for_thread(thread_id)
    }

    pub fn memory_context_for_thread(
        &mut self,
        thread_id: ThreadId,
    ) -> Result<UserMemoryContext<'_>, UserMemoryError> {
        let Some(index) = self
            .threads
            .iter()
            .position(|thread| thread.id == thread_id)
        else {
            return Err(UserMemoryError::UnknownThread(thread_id));
        };
        let process_id = self.id;
        let config = self.memory.config();
        let Self {
            state,
            exit_code,
            memory,
            stack,
            program,
            threads,
            handles,
            execution_context,
            ..
        } = self;
        let thread = &mut threads[index];
        let (layout, space) = memory.parts_mut();
        UserMemoryContext::from_process(ProcessContextParts {
            process_id,
            thread_id,
            config,
            space,
            layout,
            handles,
            state,
            exit_code,
            thread,
            program,
            stack,
            execution_context: *execution_context,
        })
    }

    pub(crate) fn memory_context_for_thread_in_space<'a>(
        &'a mut self,
        thread_id: ThreadId,
        active_space: &'a mut dyn UserSpace,
    ) -> Result<UserMemoryContext<'a>, UserMemoryError> {
        let Some(index) = self
            .threads
            .iter()
            .position(|thread| thread.id == thread_id)
        else {
            return Err(UserMemoryError::UnknownThread(thread_id));
        };
        let process_id = self.id;
        let config = self.memory.config();
        if active_space.config() != config {
            return Err(UserMemoryError::Configuration {
                expected: config,
                actual: active_space.config(),
            });
        }
        if active_space.identity() != &self.memory.identity() {
            return Err(UserMemoryError::IdentityMismatch {
                expected: self.memory.identity(),
                actual: active_space.identity().clone(),
            });
        }
        let Self {
            state,
            exit_code,
            memory,
            stack,
            program,
            threads,
            handles,
            execution_context,
            ..
        } = self;
        let thread = &mut threads[index];
        let (layout, _) = memory.parts_mut();
        UserMemoryContext::from_process(ProcessContextParts {
            process_id,
            thread_id,
            config,
            space: active_space,
            layout,
            handles,
            state,
            exit_code,
            thread,
            program,
            stack,
            execution_context: *execution_context,
        })
    }

    pub const fn stack(&self) -> StackRegion {
        self.stack
    }

    pub fn resident_bytes(&self) -> u64 {
        self.memory
            .address_space()
            .regions()
            .iter()
            .fold(0, |total, region| total.saturating_add(region.length()))
    }

    pub const fn program(&self) -> &ProgramImage {
        &self.program
    }

    pub fn threads(&self) -> &[Thread] {
        &self.threads
    }

    pub fn thread(&self, id: ThreadId) -> Option<&Thread> {
        self.threads.iter().find(|thread| thread.id == id)
    }

    pub(crate) fn save_thread_cpu(
        &mut self,
        thread_id: ThreadId,
        cpu: ArchitecturalState,
    ) -> Result<(), ThreadError> {
        if !matches!(self.state, ProcessState::Running | ProcessState::Blocked) {
            return Err(ThreadError::ProcessNotRunning {
                process_id: self.id,
                state: self.state,
            });
        }
        let Some(thread) = self
            .threads
            .iter_mut()
            .find(|thread| thread.id == thread_id)
        else {
            return Err(ThreadError::UnknownThread(thread_id));
        };
        if cpu.config() != self.program.config() {
            return Err(ThreadError::ArchitectureMismatch {
                thread_id,
                stack: self.program.config(),
                thread: cpu.config(),
            });
        }
        if cpu.privilege() != Privilege::User {
            return Err(ThreadError::InvalidPrivilege {
                thread_id,
                privilege: cpu.privilege(),
            });
        }
        validate_pc(cpu.config(), cpu.pc())
            .map_err(|source| ThreadError::Cpu { thread_id, source })?;
        validate_sp(cpu.config(), cpu.sp())
            .map_err(|source| ThreadError::Cpu { thread_id, source })?;
        thread.replace_cpu(cpu);
        Ok(())
    }

    pub(crate) fn thread_cpu(&self, thread_id: ThreadId) -> Option<ArchitecturalState> {
        self.thread(thread_id).map(|thread| thread.cpu().clone())
    }

    pub fn attach_thread(&mut self, thread: Thread) -> Result<(), ThreadError> {
        if thread.process_id != self.id {
            return Err(ThreadError::ForeignProcess {
                expected: self.id,
                actual: thread.process_id,
            });
        }
        if self.thread(thread.id).is_some() {
            return Err(ThreadError::DuplicateThread {
                process_id: self.id,
                thread_id: thread.id,
            });
        }
        self.threads
            .try_reserve(1)
            .map_err(ThreadError::ThreadAllocation)?;
        self.threads.push(thread);
        Ok(())
    }

    pub fn primary_thread(&self) -> &Thread {
        &self.threads[0]
    }

    pub fn handles(&self) -> &ProcessHandles {
        &self.handles
    }

    pub fn handles_mut(&mut self) -> &mut ProcessHandles {
        &mut self.handles
    }

    pub fn into_parts(self) -> ProcessParts {
        ProcessParts {
            id: self.id,
            state: self.state,
            exit_code: self.exit_code,
            memory: self.memory,
            stack: self.stack,
            program: self.program,
            threads: self.threads,
            handles: self.handles,
            execution_context: self.execution_context,
        }
    }
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Process")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("exit_code", &self.exit_code)
            .field("memory", &self.memory)
            .field("stack", &self.stack)
            .field("program", &self.program)
            .field("threads", &self.threads)
            .field("handles", &self.handles)
            .field("execution_context", &self.execution_context)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{FileAccess, ProcessHandles};
    use crate::FileNodeId;

    #[test]
    fn abandoned_file_reservation_does_not_consume_a_handle() {
        let mut handles = ProcessHandles::new();
        let _reservation = handles.reserve_file().unwrap();
        assert_eq!(_reservation.handle.get(), 2);
        let handle = handles
            .open_file(FileNodeId::new(1).unwrap(), FileAccess::new(true, false))
            .unwrap();
        assert_eq!(handle.get(), 2);
    }
}
