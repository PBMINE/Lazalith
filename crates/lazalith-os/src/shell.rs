use crate::{
    FileNodeKind, FileSystemError, KernelError, LZX_MAX_FILE_SIZE, LazalithKernel, LzxError,
    LzxImage, ProcessId, ThreadId, VirtualFileSystem,
};
use alloc::{boxed::Box, collections::TryReserveError, string::String, vec::Vec};
use core::{error::Error, fmt};
use lazalith_os_abi::{OPEN_READ, OpenFlags};

pub const SHELL_PROMPT: &[u8] = b"lazos$ ";
pub const SHELL_HELP: &[u8] = b"commands: help echo ls cat run clear\n";
pub const DEFAULT_SHELL_OUTPUT_LIMIT: usize = 1_048_576;
pub const DEFAULT_SHELL_LINE_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellCommand {
    Help,
    Echo,
    Ls,
    Cat,
    Run,
    Clear,
}

impl ShellCommand {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::Echo => "echo",
            Self::Ls => "ls",
            Self::Cat => "cat",
            Self::Run => "run",
            Self::Clear => "clear",
        }
    }

    fn parse(name: &[u8]) -> Option<Self> {
        match name {
            b"help" => Some(Self::Help),
            b"echo" => Some(Self::Echo),
            b"ls" => Some(Self::Ls),
            b"cat" => Some(Self::Cat),
            b"run" => Some(Self::Run),
            b"clear" => Some(Self::Clear),
            _ => None,
        }
    }

    const fn takes_argument(self) -> bool {
        matches!(self, Self::Echo | Self::Ls | Self::Cat | Self::Run)
    }

    const fn requires_argument(self) -> bool {
        matches!(self, Self::Cat | Self::Run)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShellOutcome {
    pub command: ShellCommand,
    pub bytes_written: usize,
    pub cleared: bool,
}

#[derive(Debug)]
pub enum ShellError {
    InvalidLimits,
    LineTooLong {
        length: usize,
        maximum: usize,
    },
    EmptyLine,
    InvalidCommand,
    UnknownCommand {
        command: Vec<u8>,
    },
    MissingArgument {
        command: ShellCommand,
    },
    ExtraArguments {
        command: ShellCommand,
    },
    FileSystem(FileSystemError),
    Kernel(Box<KernelError>),
    Image(Box<LzxError>),
    NoPendingRun,
    /// A launch is already queued and has not been consumed yet. The queued path is
    /// reported so a host can explain which program must be cleared.
    PendingRun {
        path: Vec<u8>,
    },
    OutputLimit {
        maximum: usize,
    },
    GenerationOverflow,
    Allocation(TryReserveError),
}

impl fmt::Display for ShellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("shell limits must be nonzero"),
            Self::LineTooLong { length, maximum } => {
                write!(f, "shell line length {length} exceeds {maximum}")
            }
            Self::EmptyLine => f.write_str("shell line is empty"),
            Self::InvalidCommand => f.write_str("invalid shell command state"),
            Self::UnknownCommand { command } => {
                write!(f, "unknown shell command {command:?}")
            }
            Self::MissingArgument { command } => {
                write!(f, "shell command {} requires an argument", command.name())
            }
            Self::ExtraArguments { command } => {
                write!(f, "shell command {} accepts one argument", command.name())
            }
            Self::FileSystem(source) => write!(f, "shell filesystem operation failed: {source}"),
            Self::Kernel(source) => write!(f, "shell program launch failed: {source}"),
            Self::Image(source) => write!(f, "shell program image is invalid: {source}"),
            Self::NoPendingRun => f.write_str("no program launch is pending"),
            Self::PendingRun { path } => {
                write!(
                    f,
                    "a program launch is already pending for {}",
                    String::from_utf8_lossy(path)
                )
            }
            Self::OutputLimit { maximum } => write!(f, "shell output exceeds {maximum} bytes"),
            Self::GenerationOverflow => f.write_str("shell screen generation overflowed"),
            Self::Allocation(source) => write!(f, "shell allocation failed: {source}"),
        }
    }
}

impl Error for ShellError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FileSystem(source) => Some(source),
            Self::Kernel(source) => Some(source),
            Self::Image(source) => Some(source),
            Self::Allocation(source) => Some(source),
            _ => None,
        }
    }
}

pub struct HeadlessShell {
    filesystem: VirtualFileSystem,
    output: Vec<u8>,
    output_limit: usize,
    line_limit: usize,
    screen_generation: u64,
    pending_run: Option<Vec<u8>>,
}

impl HeadlessShell {
    pub fn new(filesystem: VirtualFileSystem) -> Self {
        Self {
            filesystem,
            output: Vec::new(),
            output_limit: DEFAULT_SHELL_OUTPUT_LIMIT,
            line_limit: DEFAULT_SHELL_LINE_LIMIT,
            screen_generation: 0,
            pending_run: None,
        }
    }

    pub fn with_limits(
        filesystem: VirtualFileSystem,
        output_limit: usize,
        line_limit: usize,
    ) -> Result<Self, ShellError> {
        if output_limit == 0 || line_limit == 0 {
            return Err(ShellError::InvalidLimits);
        }
        Ok(Self {
            filesystem,
            output: Vec::new(),
            output_limit,
            line_limit,
            screen_generation: 0,
            pending_run: None,
        })
    }

    pub const fn filesystem(&self) -> &VirtualFileSystem {
        &self.filesystem
    }

    pub fn filesystem_mut(&mut self) -> &mut VirtualFileSystem {
        &mut self.filesystem
    }

    pub fn output(&self) -> &[u8] {
        &self.output
    }

    pub const fn output_limit(&self) -> usize {
        self.output_limit
    }

    pub const fn line_limit(&self) -> usize {
        self.line_limit
    }

    pub const fn screen_generation(&self) -> u64 {
        self.screen_generation
    }

    pub fn schedule_run(
        &mut self,
        image: LzxImage,
        kernel: &mut LazalithKernel,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<(), ShellError> {
        kernel
            .start_image(image, process_id, thread_id)
            .map_err(|source| ShellError::Kernel(Box::new(source)))
    }

    pub fn take_pending_image(&mut self) -> Result<LzxImage, ShellError> {
        let path = self.pending_run.clone().ok_or(ShellError::NoPendingRun)?;
        let flags = OpenFlags::new(OPEN_READ)
            .map_err(|source| ShellError::FileSystem(FileSystemError::Abi(source)))?;
        let opened = self
            .filesystem
            .open(&path, flags)
            .map_err(ShellError::FileSystem)?;
        let maximum = self
            .filesystem
            .limits()
            .max_file_bytes
            .min(LZX_MAX_FILE_SIZE);
        let mut bytes = Vec::new();
        let mut offset = 0u64;
        loop {
            let chunk = self
                .filesystem
                .read_at(opened.node, offset, 4096, opened.access)
                .map_err(ShellError::FileSystem)?;
            if chunk.is_empty() {
                break;
            }
            if (bytes.len() as u64).saturating_add(chunk.len() as u64) > maximum {
                return Err(ShellError::FileSystem(FileSystemError::FileTooLarge {
                    length: bytes.len() as u64 + chunk.len() as u64,
                    maximum,
                }));
            }
            bytes
                .try_reserve(chunk.len())
                .map_err(ShellError::Allocation)?;
            bytes.extend_from_slice(&chunk);
            offset = offset
                .checked_add(chunk.len() as u64)
                .ok_or(ShellError::FileSystem(FileSystemError::OffsetOverflow))?;
        }
        let image =
            LzxImage::from_bytes(&bytes).map_err(|source| ShellError::Image(Box::new(source)))?;
        self.pending_run = None;
        Ok(image)
    }

    pub fn execute_run_command(
        &mut self,
        line: &[u8],
        kernel: &mut LazalithKernel,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<ShellOutcome, ShellError> {
        if line.len() > self.line_limit {
            return Err(ShellError::LineTooLong {
                length: line.len(),
                maximum: self.line_limit,
            });
        }
        let (command, _) = parse_line(line)?;
        if command != ShellCommand::Run {
            return Err(ShellError::InvalidCommand);
        }
        if let Some(path) = self.pending_run.clone() {
            return Err(ShellError::PendingRun { path });
        }
        let outcome = self.execute_line(line)?;
        let image = self.take_pending_image()?;
        self.schedule_run(image, kernel, process_id, thread_id)?;
        Ok(outcome)
    }

    pub fn print_prompt(&mut self) -> Result<(), ShellError> {
        self.emit(SHELL_PROMPT)
    }

    pub fn execute_line(&mut self, line: &[u8]) -> Result<ShellOutcome, ShellError> {
        if line.len() > self.line_limit {
            return Err(ShellError::LineTooLong {
                length: line.len(),
                maximum: self.line_limit,
            });
        }
        let (command, argument) = parse_line(line)?;
        if command == ShellCommand::Clear {
            self.pending_run = None;
            self.screen_generation = self
                .screen_generation
                .checked_add(1)
                .ok_or(ShellError::GenerationOverflow)?;
            self.output.clear();
            return Ok(ShellOutcome {
                command,
                bytes_written: 0,
                cleared: true,
            });
        }
        let generated = match command {
            ShellCommand::Help => copy_bytes(SHELL_HELP)?,
            ShellCommand::Echo => {
                let argument = argument.unwrap_or(&[]);
                let mut output = Vec::new();
                output
                    .try_reserve(argument.len().saturating_add(1))
                    .map_err(ShellError::Allocation)?;
                output.extend_from_slice(argument);
                output.push(b'\n');
                output
            }
            ShellCommand::Ls => {
                let entries = self
                    .filesystem
                    .list(argument.unwrap_or(b"/"))
                    .map_err(ShellError::FileSystem)?;
                let total = entries.iter().try_fold(0usize, |total, entry| {
                    total
                        .checked_add(entry.name().len())
                        .and_then(|value| value.checked_add(1))
                });
                let total = total.ok_or(ShellError::OutputLimit {
                    maximum: self.output_limit,
                })?;
                let remaining = self.output_limit.saturating_sub(self.output.len());
                if total > remaining {
                    return Err(ShellError::OutputLimit {
                        maximum: self.output_limit,
                    });
                }
                let mut output = Vec::new();
                output.try_reserve(total).map_err(ShellError::Allocation)?;
                for entry in entries {
                    output.extend_from_slice(entry.name());
                    output.push(b'\n');
                }
                output
            }
            ShellCommand::Cat => {
                let argument = argument.ok_or(ShellError::MissingArgument { command })?;
                let flags = OpenFlags::new(OPEN_READ)
                    .map_err(|source| ShellError::FileSystem(FileSystemError::Abi(source)))?;
                let opened = self
                    .filesystem
                    .open(argument, flags)
                    .map_err(ShellError::FileSystem)?;
                let mut output = Vec::new();
                let mut offset = 0;
                loop {
                    let chunk = self
                        .filesystem
                        .read_at(opened.node, offset, 4096, opened.access)
                        .map_err(ShellError::FileSystem)?;
                    if chunk.is_empty() {
                        break;
                    }
                    let length = u64::try_from(chunk.len())
                        .map_err(|_| ShellError::FileSystem(FileSystemError::OffsetOverflow))?;
                    let remaining = self
                        .output_limit
                        .checked_sub(self.output.len())
                        .and_then(|value| value.checked_sub(output.len()))
                        .ok_or(ShellError::OutputLimit {
                            maximum: self.output_limit,
                        })?;
                    if chunk.len() > remaining {
                        return Err(ShellError::OutputLimit {
                            maximum: self.output_limit,
                        });
                    }
                    output
                        .try_reserve(chunk.len())
                        .map_err(ShellError::Allocation)?;
                    output.extend_from_slice(&chunk);
                    offset = offset
                        .checked_add(length)
                        .ok_or(ShellError::FileSystem(FileSystemError::OffsetOverflow))?;
                }
                output
            }
            ShellCommand::Run => {
                let path = argument.ok_or(ShellError::MissingArgument { command })?;
                let metadata = self
                    .filesystem
                    .metadata(path)
                    .map_err(ShellError::FileSystem)?;
                if metadata.kind == FileNodeKind::Directory {
                    return Err(ShellError::FileSystem(FileSystemError::IsDirectory));
                }
                if let Some(pending) = self.pending_run.clone() {
                    return Err(ShellError::PendingRun { path: pending });
                }
                self.pending_run = Some(copy_bytes(path)?);
                return Ok(ShellOutcome {
                    command,
                    bytes_written: 0,
                    cleared: false,
                });
            }
            ShellCommand::Clear => return Err(ShellError::InvalidCommand),
        };
        let bytes_written = generated.len();
        self.emit(&generated)?;
        Ok(ShellOutcome {
            command,
            bytes_written,
            cleared: false,
        })
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<(), ShellError> {
        let end = self
            .output
            .len()
            .checked_add(bytes.len())
            .ok_or(ShellError::OutputLimit {
                maximum: self.output_limit,
            })?;
        if end > self.output_limit {
            return Err(ShellError::OutputLimit {
                maximum: self.output_limit,
            });
        }
        self.output
            .try_reserve(bytes.len())
            .map_err(ShellError::Allocation)?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }
}

fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, ShellError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len())
        .map_err(ShellError::Allocation)?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn parse_line(line: &[u8]) -> Result<(ShellCommand, Option<&[u8]>), ShellError> {
    let line = trim_ascii(line);
    if line.is_empty() {
        return Err(ShellError::EmptyLine);
    }
    let separator = line
        .iter()
        .position(|byte| matches!(byte, b' ' | b'\t'))
        .unwrap_or(line.len());
    let command = match ShellCommand::parse(&line[..separator]) {
        Some(command) => command,
        None => {
            return Err(ShellError::UnknownCommand {
                command: copy_bytes(&line[..separator])?,
            });
        }
    };
    let argument = trim_ascii(&line[separator..]);
    if !command.takes_argument() {
        if !argument.is_empty() {
            return Err(ShellError::ExtraArguments { command });
        }
        return Ok((command, None));
    }
    if argument.is_empty() {
        if command.requires_argument() {
            return Err(ShellError::MissingArgument { command });
        }
        return Ok((command, None));
    }
    if command != ShellCommand::Echo && argument.iter().any(|byte| matches!(byte, b' ' | b'\t')) {
        return Err(ShellError::ExtraArguments { command });
    }
    Ok((command, Some(argument)))
}

fn trim_ascii(mut input: &[u8]) -> &[u8] {
    while input
        .first()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        input = &input[1..];
    }
    while input
        .last()
        .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
    {
        input = &input[..input.len() - 1];
    }
    input
}
