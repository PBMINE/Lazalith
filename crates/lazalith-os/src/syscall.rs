use crate::abi::{
    AbiError, DIRECTORY_RECORD_SIZE, DISPLAY_RECORD_SIZE, EXIT_STATUS_RECORD_SIZE, FILE_STAT_SIZE,
    FileHandle, INPUT_EVENT_RECORD_SIZE, IO_RESULT_SIZE, MAX_ARGUMENT_BYTES, MAX_ARGUMENT_COUNT,
    MAX_ARGUMENT_TOTAL_BYTES, MAX_PATH_BYTES, MEMORY_ALLOCATION_SIZE, OpenFlags, ProcessHandle,
    SeekOrigin, Syscall, SyscallArguments, SyscallError, TaggedOutcome, validate_range,
    validate_reserved_register,
};
use crate::syscall::ValidationError::Abi;
use crate::{
    ProcessHandles, ProcessId, ProcessState, ProgramImage, StackRegion, Thread, ThreadId,
    UserAllocation, UserMemoryLayout,
};
use core::{error::Error, fmt};
use lazalith_cpu::{
    ArchitecturalState, ControlStateError, ExecutionContextId, Privilege, SyscallAdmission,
    SyscallCompletion, TrapCause, TrapController,
};
pub use lazalith_memory::AddressSpaceIdentity as UserMemoryIdentity;
use lazalith_memory::{RegionKind, RegionPermissions, UserSpace};
use lazalith_types::{
    ArchitectureConfig, InstructionAddress, InvalidRegisterIndex, PhysicalAddress, RegisterIndex,
    VirtualAddress, WordWidth,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoHandle {
    Input,
    Output,
    File(FileHandle),
}

impl IoHandle {
    pub fn from_raw(raw: u32) -> Result<Self, AbiError> {
        match raw {
            0 => Ok(Self::Input),
            1 => Ok(Self::Output),
            value => FileHandle::new(value).map(Self::File),
        }
    }

    pub const fn as_raw(self) -> u32 {
        match self {
            Self::Input => 0,
            Self::Output => 1,
            Self::File(handle) => handle.get(),
        }
    }

    pub const fn file(self) -> Option<FileHandle> {
        match self {
            Self::File(handle) => Some(handle),
            Self::Input | Self::Output => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserMemoryAccess {
    Read,
    Write,
}

#[derive(Debug, Eq, PartialEq)]
pub enum UserMemoryError {
    Configuration {
        expected: ArchitectureConfig,
        actual: ArchitectureConfig,
    },
    IdentityMismatch {
        expected: UserMemoryIdentity,
        actual: UserMemoryIdentity,
    },
    Abi(AbiError),
    Unmapped {
        address: PhysicalAddress,
    },
    CrossRegion {
        address: PhysicalAddress,
        last: PhysicalAddress,
        region_start: PhysicalAddress,
        region_end: PhysicalAddress,
    },
    Permission {
        permissions: RegionPermissions,
    },
    ReadOnly {
        kind: RegionKind,
    },
    HostSize {
        length: u64,
    },
    InternalAccess {
        address: PhysicalAddress,
        operation: &'static str,
    },
    UnknownThread(ThreadId),
}

impl UserMemoryError {
    pub fn syscall_error(&self) -> SyscallError {
        match self {
            Self::Configuration { .. } | Self::IdentityMismatch { .. } => {
                SyscallError::InvalidState
            }
            Self::Abi(source) => SyscallError::from(*source),
            Self::Unmapped { .. }
            | Self::CrossRegion { .. }
            | Self::Permission { .. }
            | Self::ReadOnly { .. }
            | Self::HostSize { .. } => SyscallError::InvalidPointer,
            Self::InternalAccess { .. } => SyscallError::Internal,
            Self::UnknownThread(_) => SyscallError::InvalidState,
        }
    }
}

impl fmt::Display for UserMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration { expected, actual } => {
                write!(f, "memory uses {actual:?}, request uses {expected:?}")
            }
            Self::IdentityMismatch { expected, actual } => {
                write!(
                    f,
                    "active memory identity does not match process identity: expected {expected:?}, actual {actual:?}"
                )
            }
            Self::Abi(source) => write!(f, "invalid user memory range: {source}"),
            Self::Unmapped { address } => write!(f, "user address {address:?} is unmapped"),
            Self::CrossRegion {
                address,
                last,
                region_start,
                region_end,
            } => write!(
                f,
                "user range {address:?}..={last:?} crosses region {region_start:?}..={region_end:?}"
            ),
            Self::Permission { permissions } => {
                write!(f, "user memory permission denied: {permissions:?}")
            }
            Self::ReadOnly { kind } => write!(f, "user memory region {kind:?} is read-only"),
            Self::HostSize { length } => write!(f, "host cannot address {length} bytes"),
            Self::InternalAccess { address, operation } => {
                write!(f, "validated user memory {operation} failed at {address:?}")
            }
            Self::UnknownThread(thread_id) => {
                write!(f, "process has no thread {}", thread_id.get())
            }
        }
    }
}

impl Error for UserMemoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Abi(source) => Some(source),
            _ => None,
        }
    }
}

pub(crate) struct ProcessContextParts<'a> {
    pub(crate) process_id: ProcessId,
    pub(crate) thread_id: ThreadId,
    pub(crate) config: ArchitectureConfig,
    pub(crate) space: &'a mut dyn UserSpace,
    pub(crate) layout: &'a mut UserMemoryLayout,
    pub(crate) handles: &'a mut ProcessHandles,
    pub(crate) state: &'a mut ProcessState,
    pub(crate) exit_code: &'a mut Option<u32>,
    pub(crate) thread: &'a mut Thread,
    pub(crate) program: &'a ProgramImage,
    pub(crate) stack: &'a StackRegion,
    pub(crate) execution_context: Option<ExecutionContextId>,
}

pub struct UserMemoryContext<'a> {
    config: ArchitectureConfig,
    identity: UserMemoryIdentity,
    process_id: ProcessId,
    thread_id: ThreadId,
    space: &'a mut dyn UserSpace,
    layout: &'a mut UserMemoryLayout,
    handles: &'a mut ProcessHandles,
    state: &'a mut ProcessState,
    exit_code: &'a mut Option<u32>,
    thread: &'a mut Thread,
    program: &'a ProgramImage,
    stack: &'a StackRegion,
    execution_context: Option<ExecutionContextId>,
    in_syscall: bool,
}

impl<'a> UserMemoryContext<'a> {
    pub(crate) fn from_process(parts: ProcessContextParts<'a>) -> Result<Self, UserMemoryError> {
        let ProcessContextParts {
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
            execution_context,
        } = parts;
        if space.config() != config || layout.config() != config {
            return Err(UserMemoryError::Configuration {
                expected: config,
                actual: space.config(),
            });
        }
        let identity = space.identity().clone();
        Ok(Self {
            config,
            identity,
            process_id,
            thread_id,
            space,
            layout,
            handles,
            state,
            exit_code,
            thread,
            program,
            stack,
            execution_context,
            in_syscall: false,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub fn identity(&self) -> UserMemoryIdentity {
        self.identity.clone()
    }

    pub const fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub const fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    pub const fn execution_context(&self) -> Option<ExecutionContextId> {
        self.execution_context
    }

    pub const fn state(&self) -> ProcessState {
        *self.state
    }

    pub fn transition(&mut self, next: ProcessState) -> Result<(), crate::ProcessStateError> {
        if self.state.is_terminal() {
            return Err(crate::ProcessStateError::Terminal {
                process_id: self.process_id,
                state: *self.state,
            });
        }
        if matches!(next, ProcessState::Exited | ProcessState::Faulted) {
            return Err(crate::ProcessStateError::InvalidTransition {
                process_id: self.process_id,
                from: *self.state,
                to: next,
            });
        }
        if *self.state == ProcessState::Running && next == ProcessState::Ready {
            return Err(crate::ProcessStateError::InvalidTransition {
                process_id: self.process_id,
                from: *self.state,
                to: next,
            });
        }
        let valid = matches!(
            (*self.state, next),
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
            *self.state = next;
            Ok(())
        } else {
            Err(crate::ProcessStateError::InvalidTransition {
                process_id: self.process_id,
                from: *self.state,
                to: next,
            })
        }
    }

    pub fn exit(&mut self, code: u32) -> Result<(), crate::ProcessStateError> {
        if self.state.is_terminal() {
            return Err(crate::ProcessStateError::Terminal {
                process_id: self.process_id,
                state: *self.state,
            });
        }
        if !self.in_syscall {
            return Err(crate::ProcessStateError::NotInSyscall {
                process_id: self.process_id,
            });
        }
        *self.state = ProcessState::Exited;
        *self.exit_code = Some(code);
        Ok(())
    }

    pub fn exit_code(&self) -> Option<u32> {
        *self.exit_code
    }

    pub fn thread(&self) -> &Thread {
        self.thread
    }

    pub const fn program(&self) -> &ProgramImage {
        self.program
    }

    pub const fn stack(&self) -> &StackRegion {
        self.stack
    }

    pub fn handles(&mut self) -> &mut ProcessHandles {
        self.handles
    }

    pub fn allocate(
        &mut self,
        length: u64,
        alignment: u64,
    ) -> Result<UserAllocation, crate::MemoryError> {
        self.layout.allocate(length, alignment)
    }

    pub fn validate_range(
        &self,
        address: VirtualAddress,
        length: u64,
        alignment: u64,
        access: UserMemoryAccess,
    ) -> Result<(), UserMemoryError> {
        validate_range(self.config, address.as_u64(), length, alignment)
            .map_err(UserMemoryError::Abi)?;
        if length == 0 {
            return Ok(());
        }
        let start = PhysicalAddress::new(address.as_u64());
        let last = PhysicalAddress::new(address.as_u64().checked_add(length - 1).ok_or(
            UserMemoryError::Abi(AbiError::RangeOverflow {
                address: address.as_u64(),
                length,
            }),
        )?);
        let region = self
            .space
            .regions()
            .iter()
            .find(|region| region.start() <= start && start <= region.end())
            .ok_or(UserMemoryError::Unmapped { address: start })?;
        if last > region.end() {
            return Err(UserMemoryError::CrossRegion {
                address: start,
                last,
                region_start: region.start(),
                region_end: region.end(),
            });
        }
        if matches!(access, UserMemoryAccess::Write) && region.kind() != RegionKind::Ram {
            return Err(UserMemoryError::ReadOnly {
                kind: region.kind(),
            });
        }
        let permissions = region.permissions();
        let allowed = match access {
            UserMemoryAccess::Read => permissions.read,
            UserMemoryAccess::Write => permissions.write,
        };
        if !permissions.user || !allowed {
            return Err(UserMemoryError::Permission { permissions });
        }
        Ok(())
    }

    pub fn read_bytes(
        &mut self,
        address: VirtualAddress,
        output: &mut [u8],
    ) -> Result<(), UserMemoryError> {
        let length = u64::try_from(output.len())
            .map_err(|_| UserMemoryError::HostSize { length: u64::MAX })?;
        self.validate_range(address, length, 1, UserMemoryAccess::Read)?;
        if output.is_empty() {
            return Ok(());
        }
        self.space
            .peek_user(PhysicalAddress::new(address.as_u64()), output)
            .map_err(|_| UserMemoryError::InternalAccess {
                address: PhysicalAddress::new(address.as_u64()),
                operation: "read",
            })
    }

    pub fn write_bytes(
        &mut self,
        address: VirtualAddress,
        input: &[u8],
    ) -> Result<(), UserMemoryError> {
        let length = u64::try_from(input.len())
            .map_err(|_| UserMemoryError::HostSize { length: u64::MAX })?;
        self.validate_range(address, length, 1, UserMemoryAccess::Write)?;
        if input.is_empty() {
            return Ok(());
        }
        self.space
            .initialize_user(PhysicalAddress::new(address.as_u64()), input)
            .map_err(|_| UserMemoryError::InternalAccess {
                address: PhysicalAddress::new(address.as_u64()),
                operation: "write",
            })
    }

    pub fn read_word(
        &mut self,
        config: ArchitectureConfig,
        address: VirtualAddress,
    ) -> Result<u64, UserMemoryError> {
        if config != self.config {
            return Err(UserMemoryError::Configuration {
                expected: self.config,
                actual: config,
            });
        }
        let mut bytes = [0u8; 8];
        let size = usize::from(config.word_bytes());
        self.validate_range(
            address,
            u64::from(config.word_bytes()),
            u64::from(config.word_bytes()),
            UserMemoryAccess::Read,
        )?;
        self.read_bytes(address, &mut bytes[..size])?;
        Ok(match config.word_width() {
            WordWidth::W32 => {
                u64::from(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            WordWidth::W64 => u64::from_le_bytes(bytes),
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum SyscallRequestError {
    TerminalTrap,
    MissingFrame,
    WrongCause {
        expected: TrapCause,
        actual: TrapCause,
    },
    Configuration {
        controller: ArchitectureConfig,
        current: ArchitectureConfig,
    },
    ProcessConfiguration {
        process: ArchitectureConfig,
        current: ArchitectureConfig,
    },
    ProcessNotRunning {
        process_id: ProcessId,
        state: ProcessState,
    },
    MissingExecutionContext,
    ExecutionContextMismatch {
        expected: ExecutionContextId,
        actual: ExecutionContextId,
    },
    Privilege {
        expected: Privilege,
        actual: Privilege,
    },
    InterruptsEnabled,
    MissingTrapVector,
    WrongTrapTarget {
        expected: InstructionAddress,
        actual: InstructionAddress,
    },
    RegisterIndex(InvalidRegisterIndex),
    RegisterIndexConversion(usize),
    InvalidReturnControl(ControlStateError),
    AlreadyAdmitted,
    AdmissionFrameMismatch,
    MemoryIdentityMismatch,
    UnknownThread(ThreadId),
}

impl fmt::Display for SyscallRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TerminalTrap => f.write_str("trap controller is terminal"),
            Self::MissingFrame => f.write_str("syscall trap has no active frame"),
            Self::WrongCause { expected, actual } => {
                write!(f, "expected trap cause {expected:?}, found {actual:?}")
            }
            Self::Configuration {
                controller,
                current,
            } => write!(
                f,
                "trap controller uses {controller:?}, current CPU uses {current:?}"
            ),
            Self::ProcessConfiguration { process, current } => write!(
                f,
                "process memory uses {process:?}, current CPU uses {current:?}"
            ),
            Self::ProcessNotRunning { process_id, state } => {
                write!(f, "process {} is {state:?}, not Running", process_id.get())
            }
            Self::MissingExecutionContext => {
                f.write_str("no active execution context is bound to the process")
            }
            Self::ExecutionContextMismatch { expected, actual } => write!(
                f,
                "execution context {} does not match {}",
                actual.get(),
                expected.get()
            ),
            Self::Privilege { expected, actual } => {
                write!(
                    f,
                    "expected {expected:?} dispatch privilege, found {actual:?}"
                )
            }
            Self::InterruptsEnabled => f.write_str("syscall handler has interrupts enabled"),
            Self::MissingTrapVector => f.write_str("trap vector is not installed"),
            Self::WrongTrapTarget { expected, actual } => {
                write!(f, "expected trap target {expected:?}, found {actual:?}")
            }
            Self::RegisterIndex(source) => write!(f, "invalid syscall register: {source}"),
            Self::RegisterIndexConversion(index) => {
                write!(f, "syscall register index {index} exceeds u8")
            }
            Self::InvalidReturnControl(source) => {
                write!(f, "syscall trap has invalid return control: {source}")
            }
            Self::AlreadyAdmitted => f.write_str("syscall trap was already admitted"),
            Self::AdmissionFrameMismatch => f.write_str("admitted syscall frame changed"),
            Self::MemoryIdentityMismatch => {
                f.write_str("syscall request is bound to different user memory")
            }
            Self::UnknownThread(thread_id) => {
                write!(f, "process has no thread {}", thread_id.get())
            }
        }
    }
}

impl Error for SyscallRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RegisterIndex(source) => Some(source),
            Self::InvalidReturnControl(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct SyscallRequest {
    config: ArchitectureConfig,
    admission: lazalith_cpu::SyscallAdmission,
    process_id: ProcessId,
    thread_id: ThreadId,
    execution_context: ExecutionContextId,
    memory_identity: UserMemoryIdentity,
    resume_pc: InstructionAddress,
    number: u64,
    arguments: SyscallArguments,
    reserved: u64,
}

impl SyscallRequest {
    pub fn from_trap(
        controller: &mut TrapController,
        current: &ArchitecturalState,
        process: &crate::Process,
        thread_id: ThreadId,
        active_memory_identity: &UserMemoryIdentity,
    ) -> Result<Self, SyscallRequestError> {
        if controller.is_terminal() {
            return Err(SyscallRequestError::TerminalTrap);
        }
        if controller.config() != current.config() {
            return Err(SyscallRequestError::Configuration {
                controller: controller.config(),
                current: current.config(),
            });
        }
        if process.memory().config() != current.config() {
            return Err(SyscallRequestError::ProcessConfiguration {
                process: process.memory().config(),
                current: current.config(),
            });
        }
        if process.thread(thread_id).is_none() {
            return Err(SyscallRequestError::UnknownThread(thread_id));
        }
        if process.state() != ProcessState::Running {
            return Err(SyscallRequestError::ProcessNotRunning {
                process_id: process.id(),
                state: process.state(),
            });
        }
        let execution_context = controller
            .execution_context()
            .ok_or(SyscallRequestError::MissingExecutionContext)?;
        let process_context = process
            .execution_context()
            .ok_or(SyscallRequestError::MissingExecutionContext)?;
        if process_context != execution_context {
            return Err(SyscallRequestError::ExecutionContextMismatch {
                expected: execution_context,
                actual: process_context,
            });
        }
        let frame = controller
            .frame()
            .ok_or(SyscallRequestError::MissingFrame)?;
        if current.privilege() != Privilege::Supervisor {
            return Err(SyscallRequestError::Privilege {
                expected: Privilege::Supervisor,
                actual: current.privilege(),
            });
        }
        if current.status().interrupts_enabled() {
            return Err(SyscallRequestError::InterruptsEnabled);
        }
        if controller.tvec().is_none() {
            return Err(SyscallRequestError::MissingTrapVector);
        }
        if frame.cause() != TrapCause::Syscall {
            return Err(SyscallRequestError::WrongCause {
                expected: TrapCause::Syscall,
                actual: frame.cause(),
            });
        }
        controller
            .return_control()
            .map_err(SyscallRequestError::InvalidReturnControl)?;
        let read = |index| frame.snapshot().register(index);
        let number = read_trap_register(read, 0)?;
        let mut values = [0u64; crate::abi::SYSCALL_ARGUMENT_COUNT];
        for (slot, value) in values.iter_mut().enumerate() {
            *value = read_trap_register(read, slot + 1)?;
        }
        let reserved = read_trap_register(read, 7)?;
        let frame_resume_pc = frame.resume_pc();
        let expected_frame_id = frame.id();
        let admission = controller
            .take_syscall_admission()
            .ok_or(SyscallRequestError::AlreadyAdmitted)?;
        if admission.frame_id() != expected_frame_id {
            return Err(SyscallRequestError::AdmissionFrameMismatch);
        }
        Ok(Self {
            config: current.config(),
            admission,
            process_id: process.id(),
            thread_id,
            execution_context,
            memory_identity: active_memory_identity.clone(),
            resume_pc: frame_resume_pc,
            number,
            arguments: SyscallArguments::new(values),
            reserved,
        })
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn process_id(&self) -> ProcessId {
        self.process_id
    }

    pub const fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    pub const fn execution_context(&self) -> ExecutionContextId {
        self.execution_context
    }

    pub const fn memory_identity(&self) -> &UserMemoryIdentity {
        &self.memory_identity
    }

    pub const fn resume_pc(&self) -> InstructionAddress {
        self.resume_pc
    }

    pub const fn number(&self) -> u64 {
        self.number
    }

    pub const fn arguments(&self) -> SyscallArguments {
        self.arguments
    }

    pub const fn reserved(&self) -> u64 {
        self.reserved
    }

    pub fn is_live(&self, controller: &TrapController, current: &ArchitecturalState) -> bool {
        if controller.config() != self.config
            || self.admission.config() != self.config
            || current.config() != self.config
            || current.privilege() != Privilege::Supervisor
            || current.status().interrupts_enabled()
            || controller.tvec().is_none()
            || !controller.admission_matches(&self.admission)
            || controller.execution_context() != Some(self.execution_context)
            || controller.return_control().is_err()
        {
            return false;
        }
        let Some(frame) = controller.frame() else {
            return false;
        };
        if frame.id() != self.admission.frame_id()
            || frame.cause() != TrapCause::Syscall
            || frame.resume_pc() != self.resume_pc
        {
            return false;
        }
        let read = |index| frame.snapshot().register(index);
        let Ok(number_register) = RegisterIndex::try_from(0) else {
            return false;
        };
        let Ok(reserved_register) = RegisterIndex::try_from(7) else {
            return false;
        };
        if read(number_register) != self.number || read(reserved_register) != self.reserved {
            return false;
        }
        for index in 0..crate::abi::SYSCALL_ARGUMENT_COUNT {
            let Ok(raw_register) = u8::try_from(index + 1) else {
                return false;
            };
            let Ok(register) = RegisterIndex::try_from(raw_register) else {
                return false;
            };
            let Some(expected) = self.arguments.get(index) else {
                return false;
            };
            if read(register) != expected {
                return false;
            }
        }
        true
    }
}

fn read_trap_register(
    read: impl Fn(RegisterIndex) -> u64,
    raw_index: usize,
) -> Result<u64, SyscallRequestError> {
    let raw = u8::try_from(raw_index)
        .map_err(|_| SyscallRequestError::RegisterIndexConversion(raw_index))?;
    let index = RegisterIndex::try_from(raw).map_err(SyscallRequestError::RegisterIndex)?;
    Ok(read(index))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidatedSyscallKind {
    Exit {
        exit_code: u32,
    },
    Write {
        handle: IoHandle,
        buffer: VirtualAddress,
        length: u64,
        result: VirtualAddress,
    },
    Read {
        handle: IoHandle,
        buffer: VirtualAddress,
        length: u64,
        result: VirtualAddress,
    },
    Open {
        path: VirtualAddress,
        path_length: u64,
        flags: OpenFlags,
    },
    Close {
        handle: FileHandle,
    },
    Seek {
        handle: FileHandle,
        offset: i64,
        origin: SeekOrigin,
        result: VirtualAddress,
    },
    Stat {
        path: VirtualAddress,
        path_length: u64,
        result: VirtualAddress,
    },
    ListDirectory {
        path: VirtualAddress,
        path_length: u64,
        records: VirtualAddress,
        capacity: u64,
        result: VirtualAddress,
    },
    Time {
        result: VirtualAddress,
    },
    Sleep {
        cycles: u64,
    },
    AllocateMemory {
        length: u64,
        alignment: u64,
        result: VirtualAddress,
    },
    SpawnProcess {
        path: VirtualAddress,
        path_length: u64,
        argv: VirtualAddress,
        argc: u32,
        result: VirtualAddress,
    },
    WaitProcess {
        handle: ProcessHandle,
        result: VirtualAddress,
    },
    ClearScreen,
    /// Open a window and report the framebuffer the guest owns.
    ///
    /// The record is where the width, the height and the framebuffer's address go
    /// back to the guest. It is a fixed 24 bytes because the guest cannot allocate
    /// a struct it does not already have the bytes for, and a record whose size
    /// the ABI fixes is one a program can lay out in its own frame.
    DisplayOpen {
        width: u32,
        height: u32,
        framebuffer: VirtualAddress,
        record: VirtualAddress,
    },
    /// Present the frame the guest owns.
    DisplayPresent {
        framebuffer: VirtualAddress,
        result: VirtualAddress,
    },
    /// Drain queued input events into the caller's array.
    InputPoll {
        events: VirtualAddress,
        capacity: u32,
        result: VirtualAddress,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct ValidatedSyscall {
    kind: ValidatedSyscallKind,
}

impl ValidatedSyscall {
    pub const fn kind(&self) -> ValidatedSyscallKind {
        self.kind
    }

    const fn from_kind(kind: ValidatedSyscallKind) -> Self {
        Self { kind }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceOutcome {
    Return(TaggedOutcome),
    Exit,
}

pub trait KernelService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome;
}

#[derive(Debug, Eq, PartialEq)]
pub enum DispatchOutcome {
    Return {
        outcome: TaggedOutcome,
        completion: SyscallCompletion,
    },
    Exit {
        exit_code: u32,
    },
    Fault(SyscallError),
}

impl DispatchOutcome {
    pub fn returning_registers(&self) -> Option<[u64; 2]> {
        match self {
            Self::Return { outcome, .. } => Some(outcome.registers()),
            Self::Exit { .. } | Self::Fault(_) => None,
        }
    }

    pub fn into_completion(self) -> Option<SyscallCompletion> {
        match self {
            Self::Return { completion, .. } => Some(completion),
            Self::Exit { .. } | Self::Fault(_) => None,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ValidationError {
    Abi(AbiError),
    Memory { index: u8, source: UserMemoryError },
}

impl ValidationError {
    pub fn into_outcome(self) -> TaggedOutcome {
        match self {
            Self::Abi(source) => {
                TaggedOutcome::failure(SyscallError::from(source), abi_detail(source))
            }
            Self::Memory { index, source } => {
                TaggedOutcome::failure(source.syscall_error(), u32::from(index))
            }
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Abi(source) => write!(f, "invalid syscall argument: {source}"),
            Self::Memory { index, source } => {
                write!(f, "invalid memory for argument {index}: {source}")
            }
        }
    }
}

impl Error for ValidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Abi(source) => Some(source),
            Self::Memory { source, .. } => Some(source),
        }
    }
}

fn abi_detail(error: AbiError) -> u32 {
    match error {
        AbiError::InvalidArgument { index }
        | AbiError::InvalidPointer { index }
        | AbiError::InvalidPointerWidth { index, .. }
        | AbiError::InvalidArgumentWidth { index, .. }
        | AbiError::ResourceExhausted { index } => u32::from(index),
        AbiError::InvalidNameLength { maximum, .. } => u32::try_from(maximum).unwrap_or(u32::MAX),
        AbiError::Misaligned { alignment, .. } => u32::try_from(alignment).unwrap_or(u32::MAX),
        AbiError::UnknownSyscall(_)
        | AbiError::InvalidStatus(_)
        | AbiError::InvalidHandle(_)
        | AbiError::ReservedNonzero { .. }
        | AbiError::InvalidExitReason(_)
        | AbiError::WordValueOutOfRange(_)
        | AbiError::Width(_)
        | AbiError::RangeOverflow { .. } => 0,
    }
}

fn returning_outcome(admission: SyscallAdmission, outcome: TaggedOutcome) -> DispatchOutcome {
    match admission.complete_checked(outcome.status().as_u32(), outcome.payload()) {
        Some(completion) => DispatchOutcome::Return {
            outcome,
            completion,
        },
        None => DispatchOutcome::Fault(SyscallError::Internal),
    }
}

#[derive(Default)]
pub struct SyscallDispatcher;

impl SyscallDispatcher {
    pub const fn new() -> Self {
        Self
    }

    pub fn dispatch<S: KernelService + ?Sized>(
        &self,
        request: SyscallRequest,
        controller: &TrapController,
        current: &ArchitecturalState,
        memory: &mut UserMemoryContext<'_>,
        service: &mut S,
    ) -> DispatchOutcome {
        if !request.is_live(controller, current)
            || request.config != memory.config
            || request.process_id != memory.process_id()
            || request.thread_id != memory.thread_id()
            || memory.state() != ProcessState::Running
            || memory.execution_context() != Some(request.execution_context)
            || request.memory_identity != memory.identity()
        {
            return DispatchOutcome::Fault(SyscallError::InvalidState);
        }
        memory.in_syscall = true;
        let SyscallRequest {
            config,
            admission,
            number,
            arguments,
            reserved,
            ..
        } = request;
        let call = match Syscall::from_word(config, number) {
            Ok(call) => call,
            Err(error) => {
                let outcome = TaggedOutcome::failure(
                    SyscallError::from(error),
                    u32::try_from(number).unwrap_or(u32::MAX),
                );
                return returning_outcome(admission, outcome);
            }
        };
        if let Err(error) = validate_reserved_register(reserved) {
            let outcome = ValidationError::Abi(error).into_outcome();
            return returning_outcome(admission, outcome);
        }
        if let Err(error) = arguments.validate_required_zero(call) {
            let outcome = ValidationError::Abi(error).into_outcome();
            return returning_outcome(admission, outcome);
        }
        let call = match Self::validate(config, arguments, memory, call) {
            Ok(call) => call,
            Err(error) => {
                let outcome = error.into_outcome();
                return returning_outcome(admission, outcome);
            }
        };
        match (call.kind(), service.invoke(&call, memory)) {
            (ValidatedSyscallKind::Exit { exit_code }, ServiceOutcome::Exit) => {
                match memory.exit_code() {
                    Some(existing) if existing == exit_code => DispatchOutcome::Exit { exit_code },
                    None if memory.exit(exit_code).is_ok() => DispatchOutcome::Exit { exit_code },
                    _ => DispatchOutcome::Fault(SyscallError::Internal),
                }
            }
            (ValidatedSyscallKind::Exit { .. }, _) => {
                DispatchOutcome::Fault(SyscallError::Internal)
            }
            (_, ServiceOutcome::Exit) => DispatchOutcome::Fault(SyscallError::Internal),
            (_, ServiceOutcome::Return(outcome)) => {
                if matches!(
                    memory.state(),
                    ProcessState::Running | ProcessState::Blocked
                ) {
                    returning_outcome(admission, outcome)
                } else {
                    DispatchOutcome::Fault(SyscallError::Internal)
                }
            }
        }
    }

    fn validate(
        config: ArchitectureConfig,
        arguments: SyscallArguments,
        memory: &mut UserMemoryContext<'_>,
        call: Syscall,
    ) -> Result<ValidatedSyscall, ValidationError> {
        let word_bytes = u64::from(config.word_bytes());
        match call {
            // The exit status is a *bit pattern*, and a program that computed a
            // negative one did not do anything the ABI should refuse. A C `int`
            // is 32 bits in a 64-bit register, so `return -42` from `main`
            // arrives sign-extended to `0xffffffffffffffd6`, and reading argument
            // zero as a `u32` rejected the call — a kernel that made "report a
            // negative number" impossible, for a program that did nothing but
            // report a negative number. The status is the low 32 bits whatever
            // sign the register carries, which is also what every host does with
            // `exit(-1)`.
            Syscall::Exit => Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Exit {
                exit_code: abi(arguments.word(config, 0))? as u32,
            })),
            Syscall::Write => {
                let handle = IoHandle::from_raw(abi(arguments.u32(0))?).map_err(Abi)?;
                let buffer = abi(arguments.pointer(config, 1))?;
                let length = abi(arguments.word(config, 2))?;
                let result = abi(arguments.pointer(config, 3))?;
                validate_memory(memory, buffer, length, 1, UserMemoryAccess::Read, 1)?;
                validate_memory(
                    memory,
                    result,
                    host_length(IO_RESULT_SIZE, 3)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    3,
                )?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Write {
                    handle,
                    buffer,
                    length,
                    result,
                }))
            }
            Syscall::Read => {
                let handle = IoHandle::from_raw(abi(arguments.u32(0))?).map_err(Abi)?;
                let buffer = abi(arguments.pointer(config, 1))?;
                let length = abi(arguments.word(config, 2))?;
                let result = abi(arguments.pointer(config, 3))?;
                validate_memory(memory, buffer, length, 1, UserMemoryAccess::Write, 1)?;
                validate_memory(
                    memory,
                    result,
                    host_length(IO_RESULT_SIZE, 3)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    3,
                )?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Read {
                    handle,
                    buffer,
                    length,
                    result,
                }))
            }
            Syscall::Open => {
                let path = abi(arguments.pointer(config, 0))?;
                let path_length = abi(arguments.word(config, 1))?;
                validate_path(memory, path, path_length, 0, 1)?;
                let flags = OpenFlags::new(abi(arguments.u32(2))?).map_err(Abi)?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Open {
                    path,
                    path_length,
                    flags,
                }))
            }
            Syscall::Close => Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Close {
                handle: FileHandle::new(abi(arguments.u32(0))?).map_err(Abi)?,
            })),
            Syscall::Seek => {
                let handle = FileHandle::new(abi(arguments.u32(0))?).map_err(Abi)?;
                let offset = abi(arguments.signed_word(config, 1))?;
                let origin = SeekOrigin::try_from(abi(arguments.u32(2))?).map_err(Abi)?;
                let result = abi(arguments.pointer(config, 3))?;
                validate_memory(memory, result, 8, word_bytes, UserMemoryAccess::Write, 3)?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Seek {
                    handle,
                    offset,
                    origin,
                    result,
                }))
            }
            Syscall::Stat => {
                let path = abi(arguments.pointer(config, 0))?;
                let path_length = abi(arguments.word(config, 1))?;
                validate_path(memory, path, path_length, 0, 1)?;
                let result = abi(arguments.pointer(config, 2))?;
                validate_memory(
                    memory,
                    result,
                    host_length(FILE_STAT_SIZE, 2)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    2,
                )?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Stat {
                    path,
                    path_length,
                    result,
                }))
            }
            Syscall::ListDirectory => {
                let path = abi(arguments.pointer(config, 0))?;
                let path_length = abi(arguments.word(config, 1))?;
                validate_path(memory, path, path_length, 0, 1)?;
                let records = abi(arguments.pointer(config, 2))?;
                let capacity = abi(arguments.word(config, 3))?;
                let record_size = host_length(DIRECTORY_RECORD_SIZE, 3)?;
                if capacity % record_size != 0 {
                    return Err(Abi(AbiError::InvalidArgument { index: 3 }));
                }
                validate_memory(memory, records, capacity, 2, UserMemoryAccess::Write, 2)?;
                let result = abi(arguments.pointer(config, 4))?;
                validate_memory(
                    memory,
                    result,
                    host_length(IO_RESULT_SIZE, 4)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    4,
                )?;
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::ListDirectory {
                        path,
                        path_length,
                        records,
                        capacity,
                        result,
                    },
                ))
            }
            Syscall::Time => {
                let result = abi(arguments.pointer(config, 0))?;
                validate_memory(memory, result, 8, word_bytes, UserMemoryAccess::Write, 0)?;
                Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Time {
                    result,
                }))
            }
            Syscall::Sleep => Ok(ValidatedSyscall::from_kind(ValidatedSyscallKind::Sleep {
                cycles: abi(arguments.word(config, 0))?,
            })),
            Syscall::AllocateMemory => {
                let length = abi(arguments.word(config, 0))?;
                let alignment = abi(arguments.word(config, 1))?;
                if length == 0 {
                    return Err(Abi(AbiError::InvalidArgument { index: 0 }));
                }
                if alignment == 0 || !alignment.is_power_of_two() {
                    return Err(Abi(AbiError::InvalidArgument { index: 1 }));
                }
                validate_range(config, 0, length, alignment).map_err(Abi)?;
                let result = abi(arguments.pointer(config, 2))?;
                validate_memory(
                    memory,
                    result,
                    host_length(MEMORY_ALLOCATION_SIZE, 2)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    2,
                )?;
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::AllocateMemory {
                        length,
                        alignment,
                        result,
                    },
                ))
            }
            Syscall::SpawnProcess => {
                let path = abi(arguments.pointer(config, 0))?;
                let path_length = abi(arguments.word(config, 1))?;
                validate_path(memory, path, path_length, 0, 1)?;
                let argv = abi(arguments.pointer(config, 2))?;
                let argc = abi(arguments.u32(3))?;
                validate_arguments(memory, config, argv, argc)?;
                let result = abi(arguments.pointer(config, 4))?;
                validate_memory(memory, result, 4, 4, UserMemoryAccess::Write, 4)?;
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::SpawnProcess {
                        path,
                        path_length,
                        argv,
                        argc,
                        result,
                    },
                ))
            }
            Syscall::WaitProcess => {
                let handle = ProcessHandle::new(abi(arguments.u32(0))?).map_err(Abi)?;
                let result = abi(arguments.pointer(config, 1))?;
                validate_memory(
                    memory,
                    result,
                    host_length(EXIT_STATUS_RECORD_SIZE, 1)?,
                    4,
                    UserMemoryAccess::Write,
                    1,
                )?;
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::WaitProcess { handle, result },
                ))
            }
            Syscall::ClearScreen => Ok(ValidatedSyscall::from_kind(
                ValidatedSyscallKind::ClearScreen,
            )),
            Syscall::DisplayOpen => {
                let width = abi(arguments.u32(0))?;
                let height = abi(arguments.u32(1))?;
                let framebuffer = abi(arguments.pointer(config, 2))?;
                let record = abi(arguments.pointer(config, 3))?;
                // The record is where the window comes back, so its length is the
                // ABI's and the guest must have offered that much.
                validate_memory(
                    memory,
                    record,
                    host_length(DISPLAY_RECORD_SIZE, 3)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    3,
                )?;
                // The framebuffer is the guest's own memory, so it must be writable
                // and must hold the whole window. Validating the length here is
                // what stops a program opening a window whose framebuffer is
                // smaller than the window claims.
                let bytes = u64::from(width)
                    .checked_mul(u64::from(height))
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or(Abi(AbiError::ResourceExhausted { index: 0 }))?;
                if bytes != 0 {
                    validate_memory(memory, framebuffer, bytes, 1, UserMemoryAccess::Write, 2)?;
                }
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::DisplayOpen {
                        width,
                        height,
                        framebuffer,
                        record,
                    },
                ))
            }
            Syscall::DisplayPresent => {
                let framebuffer = abi(arguments.pointer(config, 0))?;
                let result = abi(arguments.pointer(config, 1))?;
                validate_memory(
                    memory,
                    result,
                    host_length(IO_RESULT_SIZE, 1)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    1,
                )?;
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::DisplayPresent {
                        framebuffer,
                        result,
                    },
                ))
            }
            Syscall::InputPoll => {
                let events = abi(arguments.pointer(config, 0))?;
                let capacity = abi(arguments.u32(1))?;
                let result = abi(arguments.pointer(config, 2))?;
                // The count comes back in a record rather than in the return
                // value, so the record is checked before anything is written.
                validate_memory(
                    memory,
                    result,
                    host_length(IO_RESULT_SIZE, 2)?,
                    word_bytes,
                    UserMemoryAccess::Write,
                    2,
                )?;
                // The array is the caller's and the driver writes whole records
                // into it, so the length is checked here rather than trusted. A
                // capacity of zero is a poll for "is anything pending", which
                // touches no memory at all, so there is nothing to validate.
                //
                // The product cannot overflow: `capacity` is a `u32` and a record
                // is sixteen bytes, so the longest array the call can name is
                // `u32::MAX * 16`, which fits a 64-bit word with room to spare. It
                // is written this way rather than as a checked multiply because a
                // checked multiply with an unreachable error would be a branch no
                // program can take and no test can reach.
                if capacity != 0 {
                    let bytes = u64::from(capacity) * INPUT_EVENT_RECORD_SIZE as u64;
                    validate_memory(memory, events, bytes, 1, UserMemoryAccess::Write, 0)?;
                }
                Ok(ValidatedSyscall::from_kind(
                    ValidatedSyscallKind::InputPoll {
                        events,
                        capacity,
                        result,
                    },
                ))
            }
        }
    }
}

fn abi<T>(result: Result<T, AbiError>) -> Result<T, ValidationError> {
    result.map_err(Abi)
}

fn host_length(length: usize, index: u8) -> Result<u64, ValidationError> {
    u64::try_from(length).map_err(|_| Abi(AbiError::InvalidArgument { index }))
}

fn validate_memory(
    memory: &UserMemoryContext<'_>,
    address: VirtualAddress,
    length: u64,
    alignment: u64,
    access: UserMemoryAccess,
    index: u8,
) -> Result<(), ValidationError> {
    memory
        .validate_range(address, length, alignment, access)
        .map_err(|source| ValidationError::Memory { index, source })
}

fn validate_path(
    memory: &mut UserMemoryContext<'_>,
    path: VirtualAddress,
    length: u64,
    pointer_index: u8,
    length_index: u8,
) -> Result<(), ValidationError> {
    if length == 0 || length > MAX_PATH_BYTES {
        return Err(Abi(AbiError::InvalidArgument {
            index: length_index,
        }));
    }
    validate_memory(
        memory,
        path,
        length,
        1,
        UserMemoryAccess::Read,
        pointer_index,
    )?;
    for offset in 0..length {
        let address = VirtualAddress::new(path.as_u64().checked_add(offset).ok_or(Abi(
            AbiError::RangeOverflow {
                address: path.as_u64(),
                length,
            },
        ))?);
        let mut byte = [0u8; 1];
        memory
            .read_bytes(address, &mut byte)
            .map_err(|source| ValidationError::Memory {
                index: pointer_index,
                source,
            })?;
        if byte[0] == 0 {
            return Err(Abi(AbiError::InvalidArgument {
                index: length_index,
            }));
        }
    }
    Ok(())
}

fn validate_arguments(
    memory: &mut UserMemoryContext<'_>,
    config: ArchitectureConfig,
    argv: VirtualAddress,
    argc: u32,
) -> Result<(), ValidationError> {
    if argc > MAX_ARGUMENT_COUNT {
        return Err(Abi(AbiError::InvalidArgument { index: 3 }));
    }
    let word_bytes = u64::from(config.word_bytes());
    let table_length =
        u64::from(argc)
            .checked_mul(word_bytes)
            .ok_or(Abi(AbiError::RangeOverflow {
                address: argv.as_u64(),
                length: u64::from(argc),
            }))?;
    validate_memory(
        memory,
        argv,
        table_length,
        word_bytes,
        UserMemoryAccess::Read,
        2,
    )?;
    let mut total_bytes = 0u64;
    for index in 0..u64::from(argc) {
        let entry = VirtualAddress::new(argv.as_u64().checked_add(index * word_bytes).ok_or(
            Abi(AbiError::RangeOverflow {
                address: argv.as_u64(),
                length: table_length,
            }),
        )?);
        let raw = memory
            .read_word(config, entry)
            .map_err(|source| ValidationError::Memory { index: 2, source })?;
        if raw > config.word_width().mask() {
            return Err(Abi(AbiError::InvalidPointerWidth {
                index: 2,
                source: crate::abi::WordValueOutOfRange::new(raw, config.word_width()),
            }));
        }
        let argument = VirtualAddress::new(raw);
        let mut string_length = None;
        for offset in 0..MAX_ARGUMENT_BYTES {
            let address = VirtualAddress::new(argument.as_u64().checked_add(offset).ok_or(Abi(
                AbiError::RangeOverflow {
                    address: argument.as_u64(),
                    length: offset,
                },
            ))?);
            let mut byte = [0u8; 1];
            memory
                .read_bytes(address, &mut byte)
                .map_err(|source| ValidationError::Memory { index: 2, source })?;
            if byte[0] == 0 {
                string_length = Some(offset + 1);
                break;
            }
        }
        let string_length = string_length.ok_or(Abi(AbiError::InvalidArgument { index: 2 }))?;
        memory
            .validate_range(argument, string_length, 1, UserMemoryAccess::Read)
            .map_err(|source| ValidationError::Memory { index: 2, source })?;
        total_bytes =
            total_bytes
                .checked_add(string_length)
                .ok_or(Abi(AbiError::RangeOverflow {
                    address: argument.as_u64(),
                    length: string_length,
                }))?;
        if total_bytes > MAX_ARGUMENT_TOTAL_BYTES {
            return Err(Abi(AbiError::ResourceExhausted { index: 2 }));
        }
    }
    Ok(())
}
