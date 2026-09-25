use crate::syscall::{IoHandle, KernelService, ServiceOutcome, UserMemoryContext};
use crate::{FileSystemService, ValidatedSyscall, ValidatedSyscallKind};
use alloc::{collections::TryReserveError, vec::Vec};
use core::{error::Error, fmt};
use lazalith_os_abi::{IoResult, SyscallError, SyscallStatus, TaggedOutcome};

pub const DEFAULT_TERMINAL_INPUT_LIMIT: usize = 65_536;
pub const DEFAULT_TERMINAL_OUTPUT_LIMIT: usize = 1_048_576;

#[derive(Debug)]
pub enum TerminalError {
    InvalidLimits,
    InputLimit { maximum: usize },
    OutputLimit { maximum: usize },
    GenerationOverflow,
    Allocation(TryReserveError),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("terminal limits must be nonzero"),
            Self::InputLimit { maximum } => write!(f, "terminal input exceeds {maximum} bytes"),
            Self::OutputLimit { maximum } => write!(f, "terminal output exceeds {maximum} bytes"),
            Self::GenerationOverflow => f.write_str("terminal screen generation overflowed"),
            Self::Allocation(source) => write!(f, "terminal allocation failed: {source}"),
        }
    }
}

impl Error for TerminalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

pub struct VirtualTerminal {
    input: Vec<u8>,
    input_offset: usize,
    input_limit: usize,
    output: Vec<u8>,
    output_limit: usize,
    screen_generation: u64,
}

impl VirtualTerminal {
    pub fn new(input: &[u8]) -> Result<Self, TerminalError> {
        Self::with_limits(
            input,
            DEFAULT_TERMINAL_INPUT_LIMIT,
            DEFAULT_TERMINAL_OUTPUT_LIMIT,
        )
    }

    pub fn with_limits(
        input: &[u8],
        input_limit: usize,
        output_limit: usize,
    ) -> Result<Self, TerminalError> {
        if input_limit == 0 || output_limit == 0 {
            return Err(TerminalError::InvalidLimits);
        }
        if input.len() > input_limit {
            return Err(TerminalError::InputLimit {
                maximum: input_limit,
            });
        }
        let mut stored = Vec::new();
        stored
            .try_reserve_exact(input.len())
            .map_err(TerminalError::Allocation)?;
        stored.extend_from_slice(input);
        Ok(Self {
            input: stored,
            input_offset: 0,
            input_limit,
            output: Vec::new(),
            output_limit,
            screen_generation: 0,
        })
    }

    pub const fn input_limit(&self) -> usize {
        self.input_limit
    }

    pub const fn output_limit(&self) -> usize {
        self.output_limit
    }

    pub fn remaining_input(&self) -> &[u8] {
        &self.input[self.input_offset..]
    }

    pub fn output(&self) -> &[u8] {
        &self.output
    }

    pub const fn screen_generation(&self) -> u64 {
        self.screen_generation
    }

    pub fn push_input(&mut self, input: &[u8]) -> Result<(), TerminalError> {
        let end = self
            .input
            .len()
            .checked_add(input.len())
            .ok_or(TerminalError::InputLimit {
                maximum: self.input_limit,
            })?;
        if end > self.input_limit {
            return Err(TerminalError::InputLimit {
                maximum: self.input_limit,
            });
        }
        self.input
            .try_reserve(input.len())
            .map_err(TerminalError::Allocation)?;
        self.input.extend_from_slice(input);
        Ok(())
    }

    pub fn clear(&mut self) -> Result<(), TerminalError> {
        self.screen_generation = self
            .screen_generation
            .checked_add(1)
            .ok_or(TerminalError::GenerationOverflow)?;
        self.output.clear();
        Ok(())
    }

    fn prepare_input(&self, length: usize) -> Result<(usize, usize, usize), TerminalError> {
        let start = self.input_offset.min(self.input.len());
        if length == 0 {
            return Ok((start, start, start));
        }
        let available = &self.input[start..];
        let content_length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(available.len());
        if content_length > length {
            return Err(TerminalError::InputLimit {
                maximum: self.input_limit,
            });
        }
        let transfer = if content_length == 0 && available.first() == Some(&b'\n') {
            1
        } else {
            content_length
        };
        let write_end = start
            .checked_add(transfer)
            .ok_or(TerminalError::InputLimit {
                maximum: self.input_limit,
            })?;
        let consumed_end = if transfer == content_length {
            start
                .checked_add(content_length)
                .and_then(|value| {
                    if available.get(content_length) == Some(&b'\n') {
                        value.checked_add(1)
                    } else {
                        Some(value)
                    }
                })
                .ok_or(TerminalError::InputLimit {
                    maximum: self.input_limit,
                })?
        } else {
            write_end
        };
        Ok((start, write_end, consumed_end))
    }

    fn commit_input(&mut self, end: usize) {
        self.input_offset = end;
    }

    fn prepare_output(&mut self, length: usize) -> Result<(), TerminalError> {
        let end = self
            .output
            .len()
            .checked_add(length)
            .ok_or(TerminalError::OutputLimit {
                maximum: self.output_limit,
            })?;
        if end > self.output_limit {
            return Err(TerminalError::OutputLimit {
                maximum: self.output_limit,
            });
        }
        self.output
            .try_reserve(length)
            .map_err(TerminalError::Allocation)?;
        Ok(())
    }

    fn commit_output(&mut self, bytes: &[u8]) {
        self.output.extend_from_slice(bytes);
    }
}

pub struct TerminalService {
    terminal: VirtualTerminal,
    filesystem: FileSystemService,
}

impl TerminalService {
    pub const fn new(terminal: VirtualTerminal, filesystem: FileSystemService) -> Self {
        Self {
            terminal,
            filesystem,
        }
    }

    pub const fn terminal(&self) -> &VirtualTerminal {
        &self.terminal
    }

    pub fn terminal_mut(&mut self) -> &mut VirtualTerminal {
        &mut self.terminal
    }

    pub const fn filesystem(&self) -> &FileSystemService {
        &self.filesystem
    }

    pub fn filesystem_mut(&mut self) -> &mut FileSystemService {
        &mut self.filesystem
    }

    fn read_input(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        buffer: lazalith_types::VirtualAddress,
        length: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let requested = match usize::try_from(length) {
            Ok(requested) => requested,
            Err(_) => return failure(SyscallError::ResourceExhausted),
        };
        let (start, write_end, consumed_end) = match self.terminal.prepare_input(requested) {
            Ok(range) => range,
            Err(_) => return failure(SyscallError::ResourceExhausted),
        };
        let bytes = &self.terminal.input[start..write_end];
        if let Err(error) = memory.write_bytes(buffer, bytes) {
            return failure(error.syscall_error());
        }
        let transferred = u64::try_from(bytes.len()).unwrap_or(0);
        let io = match IoResult::new(memory.config(), transferred, SyscallStatus::Ok) {
            Ok(io) => io,
            Err(error) => return failure(SyscallError::from(error)),
        };
        if let Err(error) = memory.write_bytes(result, &io.encode()) {
            return failure(error.syscall_error());
        }
        self.terminal.commit_input(consumed_end);
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }

    fn write_output(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        buffer: lazalith_types::VirtualAddress,
        length: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let requested = match usize::try_from(length) {
            Ok(requested) => requested,
            Err(_) => return failure(SyscallError::ResourceExhausted),
        };
        if self
            .terminal
            .output
            .len()
            .checked_add(requested)
            .is_none_or(|end| end > self.terminal.output_limit)
        {
            return failure(SyscallError::ResourceExhausted);
        }
        let mut bytes = Vec::new();
        if bytes.try_reserve_exact(requested).is_err() {
            return failure(SyscallError::ResourceExhausted);
        }
        bytes.resize(requested, 0);
        if let Err(error) = memory.read_bytes(buffer, &mut bytes) {
            return failure(error.syscall_error());
        }
        let transferred = u64::try_from(bytes.len()).unwrap_or(0);
        let io = match IoResult::new(memory.config(), transferred, SyscallStatus::Ok) {
            Ok(io) => io,
            Err(error) => return failure(SyscallError::from(error)),
        };
        if self.terminal.prepare_output(bytes.len()).is_err() {
            return failure(SyscallError::ResourceExhausted);
        }
        if let Err(error) = memory.write_bytes(result, &io.encode()) {
            return failure(error.syscall_error());
        }
        self.terminal.commit_output(&bytes);
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }
}

impl KernelService for TerminalService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::Exit { .. } => ServiceOutcome::Exit,
            ValidatedSyscallKind::Read {
                handle: IoHandle::Input,
                buffer,
                length,
                result,
            } => self.read_input(memory, buffer, length, result),
            ValidatedSyscallKind::Write {
                handle: IoHandle::Output,
                buffer,
                length,
                result,
            } => self.write_output(memory, buffer, length, result),
            ValidatedSyscallKind::Read { handle, .. }
            | ValidatedSyscallKind::Write { handle, .. }
                if handle.file().is_none() =>
            {
                failure(SyscallError::InvalidHandle)
            }
            ValidatedSyscallKind::ClearScreen => match self.terminal.clear() {
                Ok(()) => ServiceOutcome::Return(TaggedOutcome::success(0)),
                Err(_) => failure(SyscallError::ResourceExhausted),
            },
            _ => self.filesystem.invoke(syscall, memory),
        }
    }
}

fn failure(error: SyscallError) -> ServiceOutcome {
    ServiceOutcome::Return(TaggedOutcome::failure(error, 0))
}

#[cfg(test)]
mod tests {
    use super::{TerminalError, VirtualTerminal};
    use alloc::vec;

    #[test]
    fn zero_length_input_does_not_consume_a_line() {
        let terminal = VirtualTerminal::new(b"\nhelp\n").unwrap();
        assert_eq!(terminal.prepare_input(0).unwrap(), (0, 0, 0));
        assert_eq!(terminal.remaining_input(), b"\nhelp\n");
    }

    #[test]
    fn overlong_input_is_rejected_before_fragmentation() {
        let input = vec![b'x'; 256];
        let terminal = VirtualTerminal::new(&input).unwrap();
        assert!(matches!(
            terminal.prepare_input(255),
            Err(TerminalError::InputLimit { .. })
        ));
        assert_eq!(terminal.remaining_input(), input.as_slice());
    }
}
