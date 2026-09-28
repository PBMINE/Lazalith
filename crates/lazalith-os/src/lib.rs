#![no_std]

extern crate alloc;

pub use lazalith_os_abi as abi;

pub mod allocator;
pub mod debug;
pub mod display;
mod error;
mod filesystem;
mod init;
pub mod input;
mod kernel;
mod lza;
mod lzx;
mod memory;
mod native_shell;
mod pool;
mod process;
mod resolve;
mod scheduler;
mod shell;
mod syscall;
mod terminal;

pub use allocator::MemoryService;
pub use error::MemoryError;
pub use filesystem::{
    DEFAULT_MAX_DIRECTORY_ENTRIES, DEFAULT_MAX_FILE_BYTES, DEFAULT_MAX_NODES, DirectoryEntry,
    FileAccess, FileMetadata, FileNodeId, FileNodeKind, FileOpen, FileSystemError,
    FileSystemLimits, FileSystemService, VirtualFileSystem,
};
pub use init::{
    INIT_CODE_LENGTH, INIT_EXIT_CODE, INIT_SYSCALL_NUMBER, InitImageError, build_init_image,
};
pub use input::InputService;
pub use kernel::{KernelError, KernelServiceOutcome, KernelStep, LazalithKernel};
pub use lza::{
    LZA_FORMAT_VERSION, LZA_MAGIC, LzaError, LzaPackage, LzaResource, PERMISSION_CONSOLE,
    PERMISSION_FILESYSTEM, PERMISSION_GRAPHICS, PERMISSION_INPUT, PackageIdentity,
    PackagePermissions, PackageVersion, check_name,
};
pub use lzx::{
    LZX_ABI_VERSION, LZX_CODE_PERMISSIONS, LZX_DATA_PERMISSIONS, LZX_FORMAT_VERSION,
    LZX_HEADER_SIZE, LZX_ISA_VERSION, LZX_MAGIC, LZX_MAX_FILE_SIZE, LZX_MAX_SECTIONS,
    LZX_SECTION_ENTRY_SIZE, LzxArchitecture, LzxError, LzxImage, LzxSection, LzxSectionKind,
};
pub use memory::{
    KERNEL_HEAP_LENGTH, KERNEL_HEAP_START, KERNEL_IMAGE_LENGTH, KERNEL_IMAGE_START,
    KERNEL_INITIAL_SP, KERNEL_STACK_LENGTH, KERNEL_STACK_START, KernelMemory, PHYSICAL_RAM_LENGTH,
    PHYSICAL_RAM_START, StackRegion, USER_CODE_LENGTH, USER_CODE_START, USER_DATA_LENGTH,
    USER_DATA_START, USER_INITIAL_SP, USER_STACK_LENGTH, USER_STACK_START, UserAllocation,
    UserMemory, UserMemoryLayout,
};
pub use native_shell::{
    NATIVE_SHELL_BSS_LENGTH, NATIVE_SHELL_BSS_OFFSET, NATIVE_SHELL_CAT_PATH,
    NATIVE_SHELL_DATA_LENGTH, NATIVE_SHELL_DATA_OFFSET, NATIVE_SHELL_DIRECTORY_CAPACITY,
    NATIVE_SHELL_DIRECTORY_OFFSET, NATIVE_SHELL_FILE_BUFFER_OFFSET, NATIVE_SHELL_FILE_CAPACITY,
    NATIVE_SHELL_HELP, NATIVE_SHELL_IO_ERROR, NATIVE_SHELL_IO_RESULT_OFFSET,
    NATIVE_SHELL_LINE_BUFFER_OFFSET, NATIVE_SHELL_LINE_CAPACITY, NATIVE_SHELL_LS_HEADER,
    NATIVE_SHELL_LS_PATH, NATIVE_SHELL_PROCESS_LAUNCH_SUPPORTED, NATIVE_SHELL_PROMPT,
    NATIVE_SHELL_REQUIRED_DATA, NATIVE_SHELL_RUN_DEFERRED, NativeShellEmissionError,
    NativeShellImageError, NativeShellLayout, NativeShellLayoutError, build_init_shell_image,
};
pub use pool::{BumpPool, MemoryBlock};
pub use process::{
    HandleError, OpenFile, Process, ProcessError, ProcessExecutionError, ProcessHandleEntry,
    ProcessHandles, ProcessId, ProcessParts, ProcessState, ProcessStateError, ProgramError,
    ProgramImage, Thread, ThreadError, ThreadId,
};
pub use resolve::{Container, ResolveError, Resolved, installable, resolve};
pub use scheduler::{
    ActiveThread, RoundRobinScheduler, SchedulerError, SchedulerRun, SchedulerStep,
};
pub use shell::{
    DEFAULT_SHELL_LINE_LIMIT, DEFAULT_SHELL_OUTPUT_LIMIT, HeadlessShell, SHELL_HELP, SHELL_PROMPT,
    ShellCommand, ShellError, ShellOutcome,
};
pub use syscall::{
    DispatchOutcome, IoHandle, KernelService, ServiceOutcome, SyscallDispatcher, SyscallRequest,
    SyscallRequestError, UserMemoryAccess, UserMemoryContext, UserMemoryError, UserMemoryIdentity,
    ValidatedSyscall, ValidatedSyscallKind, ValidationError,
};
pub use terminal::{
    DEFAULT_TERMINAL_INPUT_LIMIT, DEFAULT_TERMINAL_OUTPUT_LIMIT, TerminalError, TerminalService,
    VirtualTerminal,
};
