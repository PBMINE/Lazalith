use crate::{
    ArchitecturalState, ControlStateError, CpuMemory, Privilege, StatusRegister, validate_pc,
    validate_sp,
};
use alloc::sync::Arc;
use core::num::NonZeroU64;
use lazalith_isa::ControlRegister;
use lazalith_types::{ArchitectureConfig, InstructionAddress, RegisterIndex, VirtualAddress};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u16)]
pub enum TrapCause {
    IllegalInstruction = 1,
    PrivilegeViolation = 2,
    InvalidWidth = 3,
    AddressOverflow = 4,
    Alignment = 5,
    Unmapped = 6,
    Permission = 7,
    DivideByZero = 8,
    DivisionOverflow = 9,
    InvalidControlState = 10,
    InvalidStatus = 11,
    DeviceAccess = 12,
    Syscall = 16,
    SoftwareTrap = 17,
    ExternalInterrupt = 18,
}

impl TrapCause {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExecutionContextId(NonZeroU64);

impl ExecutionContextId {
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrapSnapshot {
    architectural: ArchitecturalState,
}

impl TrapSnapshot {
    pub const fn pc(&self) -> InstructionAddress {
        self.architectural.pc()
    }

    pub const fn sp(&self) -> VirtualAddress {
        self.architectural.sp()
    }

    pub const fn status(&self) -> u64 {
        self.architectural.status().bits()
    }

    pub fn register(&self, index: RegisterIndex) -> u64 {
        self.architectural.registers().read(index)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrapAttempt {
    snapshot: TrapSnapshot,
    cause: TrapCause,
    payload: u64,
    resume_pc: InstructionAddress,
}

impl TrapAttempt {
    pub const fn snapshot(&self) -> &TrapSnapshot {
        &self.snapshot
    }

    pub const fn cause(&self) -> TrapCause {
        self.cause
    }

    pub const fn payload(&self) -> u64 {
        self.payload
    }

    pub const fn resume_pc(&self) -> InstructionAddress {
        self.resume_pc
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrapFrame {
    id: u64,
    snapshot: TrapSnapshot,
    cause: TrapCause,
    payload: u64,
    resume_pc: InstructionAddress,
    resume_sp: VirtualAddress,
    resume_status: u64,
}

impl TrapFrame {
    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn snapshot(&self) -> &TrapSnapshot {
        &self.snapshot
    }

    pub const fn cause(&self) -> TrapCause {
        self.cause
    }

    pub const fn payload(&self) -> u64 {
        self.payload
    }

    pub const fn resume_pc(&self) -> InstructionAddress {
        self.resume_pc
    }

    pub const fn resume_sp(&self) -> VirtualAddress {
        self.resume_sp
    }

    pub const fn resume_status(&self) -> u64 {
        self.resume_status
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DoubleTrap {
    second: TrapSnapshot,
    cause: TrapCause,
    payload: u64,
    resume_pc: InstructionAddress,
}

impl DoubleTrap {
    pub const fn second(&self) -> &TrapSnapshot {
        &self.second
    }

    pub const fn cause(&self) -> TrapCause {
        self.cause
    }

    pub const fn payload(&self) -> u64 {
        self.payload
    }

    pub fn attempt(&self) -> TrapAttempt {
        TrapAttempt {
            snapshot: self.second.clone(),
            cause: self.cause,
            payload: self.payload,
            resume_pc: self.resume_pc,
        }
    }

    pub const fn resume_pc(&self) -> InstructionAddress {
        self.resume_pc
    }
}

#[derive(Clone, Debug)]
struct ControllerIdentity {
    marker: Arc<u8>,
}

impl ControllerIdentity {
    fn duplicate(&self) -> Self {
        Self {
            marker: Arc::clone(&self.marker),
        }
    }
}

impl PartialEq for ControllerIdentity {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.marker, &other.marker)
    }
}

impl Eq for ControllerIdentity {}

#[derive(Debug, Eq, PartialEq)]
pub struct SyscallAdmission {
    identity: ControllerIdentity,
    execution_context: ExecutionContextId,
    frame_id: u64,
    config: ArchitectureConfig,
}

impl SyscallAdmission {
    pub const fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn execution_context(&self) -> ExecutionContextId {
        self.execution_context
    }

    fn complete(self, status: u32, payload: u32) -> SyscallCompletion {
        SyscallCompletion {
            identity: self.identity,
            execution_context: self.execution_context,
            frame_id: self.frame_id,
            config: self.config,
            status,
            payload,
        }
    }

    pub fn complete_checked(self, status: u32, payload: u32) -> Option<SyscallCompletion> {
        if !matches!(status, 0..=19) {
            return None;
        }
        Some(self.complete(status, payload))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct SyscallCompletion {
    identity: ControllerIdentity,
    execution_context: ExecutionContextId,
    frame_id: u64,
    config: ArchitectureConfig,
    status: u32,
    payload: u32,
}

impl SyscallCompletion {
    pub const fn execution_context(&self) -> ExecutionContextId {
        self.execution_context
    }

    pub const fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub const fn status(&self) -> u32 {
        self.status
    }

    pub const fn payload(&self) -> u32 {
        self.payload
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrapController {
    identity: ControllerIdentity,
    config: ArchitectureConfig,
    tvec: Option<InstructionAddress>,
    frame: Option<TrapFrame>,
    double_trap: Option<DoubleTrap>,
    failed_entry: Option<TrapAttempt>,
    next_frame_id: u64,
    syscall_admitted: bool,
    syscall_return_authorized: bool,
    execution_context: Option<ExecutionContextId>,
}

impl TrapController {
    pub fn new(config: ArchitectureConfig) -> Self {
        Self {
            identity: ControllerIdentity {
                marker: Arc::new(0),
            },
            config,
            tvec: None,
            frame: None,
            double_trap: None,
            failed_entry: None,
            next_frame_id: 0,
            syscall_admitted: false,
            syscall_return_authorized: false,
            execution_context: None,
        }
    }

    pub const fn config(&self) -> ArchitectureConfig {
        self.config
    }

    pub fn set_execution_context(&mut self, context: ExecutionContextId) {
        self.execution_context = Some(context);
    }

    pub fn clear_execution_context(&mut self, context: ExecutionContextId) -> bool {
        if self.execution_context == Some(context) {
            self.execution_context = None;
            true
        } else {
            false
        }
    }

    pub const fn execution_context(&self) -> Option<ExecutionContextId> {
        self.execution_context
    }

    pub const fn tvec(&self) -> Option<InstructionAddress> {
        self.tvec
    }

    pub const fn frame(&self) -> Option<&TrapFrame> {
        self.frame.as_ref()
    }

    pub const fn double_trap(&self) -> Option<&DoubleTrap> {
        self.double_trap.as_ref()
    }

    pub const fn failed_entry(&self) -> Option<&TrapAttempt> {
        self.failed_entry.as_ref()
    }

    pub fn is_terminal(&self) -> bool {
        self.double_trap.is_some() || self.failed_entry.is_some()
    }

    pub fn has_active_frame(&self) -> bool {
        self.frame.is_some()
    }

    pub fn take_syscall_admission(&mut self) -> Option<SyscallAdmission> {
        let frame = self.frame.as_ref()?;
        let execution_context = self.execution_context?;
        if frame.cause != TrapCause::Syscall || self.syscall_admitted {
            return None;
        }
        self.syscall_admitted = true;
        Some(SyscallAdmission {
            identity: self.identity.duplicate(),
            execution_context,
            frame_id: frame.id,
            config: self.config,
        })
    }

    pub const fn syscall_admitted(&self) -> bool {
        self.syscall_admitted
    }

    pub fn admission_matches(&self, admission: &SyscallAdmission) -> bool {
        self.identity == admission.identity
            && self.config == admission.config
            && self.execution_context == Some(admission.execution_context)
            && self.syscall_admitted
            && self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.id == admission.frame_id)
    }

    pub fn completion_matches(&self, completion: &SyscallCompletion) -> bool {
        self.identity == completion.identity
            && self.config == completion.config
            && self.execution_context == Some(completion.execution_context)
            && self.syscall_admitted
            && self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.id == completion.frame_id)
    }

    pub fn authorize_syscall_return(&mut self, completion: &SyscallCompletion) -> bool {
        if self.syscall_return_authorized || !self.completion_matches(completion) {
            return false;
        }
        self.syscall_return_authorized = true;
        true
    }

    pub fn clear_syscall_return_authorization(&mut self) {
        self.syscall_return_authorized = false;
    }

    pub fn abort_frame(&mut self) -> bool {
        if self.is_terminal() {
            return false;
        }
        let had_frame = self.frame.is_some();
        self.frame = None;
        self.syscall_admitted = false;
        self.syscall_return_authorized = false;
        had_frame
    }

    pub(crate) fn consume_syscall_return_authorization(&mut self) -> bool {
        if self
            .frame
            .as_ref()
            .is_some_and(|frame| frame.cause == TrapCause::Syscall)
        {
            if !self.syscall_return_authorized {
                return false;
            }
            self.syscall_return_authorized = false;
        }
        true
    }

    fn ensure_not_terminal(
        &self,
        operation: &'static str,
        selector: ControlRegister,
    ) -> Result<(), ControlStateError> {
        if self.is_terminal() {
            return Err(ControlStateError::InvalidControlState {
                operation,
                selector: selector.as_u8(),
            });
        }
        Ok(())
    }

    fn require_frame(
        &self,
        operation: &'static str,
        selector: ControlRegister,
    ) -> Result<&TrapFrame, ControlStateError> {
        self.frame
            .as_ref()
            .ok_or(ControlStateError::InvalidControlState {
                operation,
                selector: selector.as_u8(),
            })
    }

    fn require_frame_mut(
        &mut self,
        operation: &'static str,
        selector: ControlRegister,
    ) -> Result<&mut TrapFrame, ControlStateError> {
        self.frame
            .as_mut()
            .ok_or(ControlStateError::InvalidControlState {
                operation,
                selector: selector.as_u8(),
            })
    }

    pub fn read_control(&self, control: ControlRegister) -> Result<u64, ControlStateError> {
        self.ensure_not_terminal("read", control)?;
        match control {
            ControlRegister::Tvec => self.tvec.map(|value| value.as_u64()).ok_or(
                ControlStateError::InvalidControlState {
                    operation: "read",
                    selector: control.as_u8(),
                },
            ),
            ControlRegister::Epc => Ok(self.require_frame("read", control)?.resume_pc.as_u64()),
            ControlRegister::Esp => Ok(self.require_frame("read", control)?.resume_sp.as_u64()),
            ControlRegister::Estatus => Ok(self.require_frame("read", control)?.resume_status),
            ControlRegister::Tcause => Ok(u64::from(
                self.require_frame("read", control)?.cause.as_u16(),
            )),
            ControlRegister::Tpayload => Ok(self.require_frame("read", control)?.payload),
        }
    }

    pub fn write_control(
        &mut self,
        control: ControlRegister,
        value: u64,
    ) -> Result<(), ControlStateError> {
        self.ensure_not_terminal("write", control)?;
        match control {
            ControlRegister::Tvec => {
                let target = InstructionAddress::new(value);
                validate_pc(self.config, target)?;
                self.tvec = Some(target);
                Ok(())
            }
            ControlRegister::Epc => {
                let config = self.config;
                let frame = self.require_frame_mut("write", control)?;
                let target = InstructionAddress::new(value);
                validate_pc(config, target)?;
                frame.resume_pc = target;
                Ok(())
            }
            ControlRegister::Esp => {
                let config = self.config;
                let frame = self.require_frame_mut("write", control)?;
                let stack = VirtualAddress::new(value);
                validate_sp(config, stack)?;
                frame.resume_sp = stack;
                Ok(())
            }
            ControlRegister::Estatus => {
                let config = self.config;
                let frame = self.require_frame_mut("write", control)?;
                StatusRegister::try_from_bits(config.word_width(), value)
                    .map_err(ControlStateError::Status)?;
                frame.resume_status = value;
                Ok(())
            }
            ControlRegister::Tcause | ControlRegister::Tpayload => {
                self.require_frame("write read-only control", control)?;
                Err(ControlStateError::InvalidControlState {
                    operation: "write read-only control",
                    selector: control.as_u8(),
                })
            }
        }
    }

    pub fn return_control(
        &self,
    ) -> Result<(InstructionAddress, VirtualAddress, u64), ControlStateError> {
        self.ensure_not_terminal("return", ControlRegister::Epc)?;
        let frame = self.require_frame("return", ControlRegister::Epc)?;
        validate_pc(self.config, frame.resume_pc)?;
        validate_sp(self.config, frame.resume_sp)?;
        StatusRegister::try_from_bits(self.config.word_width(), frame.resume_status)
            .map_err(ControlStateError::Status)?;
        Ok((frame.resume_pc, frame.resume_sp, frame.resume_status))
    }

    pub(crate) fn commit_return(&mut self) {
        self.frame = None;
        self.syscall_admitted = false;
        self.syscall_return_authorized = false;
    }

    pub(crate) fn snapshot(state: &ArchitecturalState) -> TrapSnapshot {
        TrapSnapshot {
            architectural: state.clone(),
        }
    }

    pub(crate) fn record_double_trap(
        &mut self,
        state: &ArchitecturalState,
        cause: TrapCause,
        payload: u64,
        resume_pc: InstructionAddress,
    ) {
        if self.double_trap.is_some() {
            return;
        }
        self.double_trap = Some(DoubleTrap {
            second: Self::snapshot(state),
            cause,
            payload,
            resume_pc,
        });
    }

    pub(crate) fn record_failed_entry(
        &mut self,
        state: &ArchitecturalState,
        cause: TrapCause,
        payload: u64,
        resume_pc: InstructionAddress,
    ) {
        if self.failed_entry.is_some() {
            return;
        }
        self.failed_entry = Some(TrapAttempt {
            snapshot: Self::snapshot(state),
            cause,
            payload,
            resume_pc,
        });
    }

    pub(crate) fn install_frame(
        &mut self,
        snapshot: TrapSnapshot,
        cause: TrapCause,
        payload: u64,
        resume_pc: InstructionAddress,
        resume_sp: VirtualAddress,
        resume_status: u64,
    ) {
        self.next_frame_id = self.next_frame_id.wrapping_add(1);
        self.syscall_admitted = false;
        self.syscall_return_authorized = false;
        self.frame = Some(TrapFrame {
            id: self.next_frame_id,
            snapshot,
            cause,
            payload,
            resume_pc,
            resume_sp,
            resume_status,
        });
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareEntryError<E> {
    Fetch(E),
    Control(ControlStateError),
}

impl<E: core::fmt::Display> core::fmt::Display for PrepareEntryError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Fetch(source) => write!(f, "trap target fetch: {source}"),
            Self::Control(source) => source.fmt(f),
        }
    }
}

impl<E: core::error::Error + 'static> core::error::Error for PrepareEntryError<E> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Fetch(source) => Some(source),
            Self::Control(source) => Some(source),
        }
    }
}

pub(crate) fn prepare_entry_control<M: CpuMemory>(
    config: ArchitectureConfig,
    state: &ArchitecturalState,
    memory: &M,
    tvec: InstructionAddress,
) -> Result<ArchitecturalState, PrepareEntryError<M::Error>> {
    memory
        .fetch_instruction(config, tvec, Privilege::Supervisor)
        .map_err(PrepareEntryError::Fetch)?;
    let mut status = state.status();
    status.set_interrupts_enabled(false);
    status.set_privilege(Privilege::Supervisor);
    let mut candidate = state.clone();
    candidate
        .restore_control(tvec, state.sp(), status.bits())
        .map_err(PrepareEntryError::Control)?;
    Ok(candidate)
}
