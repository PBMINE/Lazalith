#![no_std]

mod error;
mod records;
mod syscall;

pub use error::{AbiError, WordValueOutOfRange};
pub use records::{
    AbiFileKind, DIRECTORY_NAME_CAPACITY, DIRECTORY_RECORD_SIZE, DISPLAY_RECORD_SIZE,
    DirectoryRecord, DisplayRecord, EXIT_STATUS_RECORD_SIZE, ExitStatusRecord, FILE_STAT_SIZE,
    FileHandle, FilePermissions, FileStat, INPUT_EVENT_RECORD_SIZE, IO_RESULT_SIZE,
    InputEventRecord, IoResult, MEMORY_ALLOCATION_SIZE, MemoryAllocation, OPEN_ALL, OPEN_CREATE,
    OPEN_READ, OPEN_TRUNCATE, OPEN_WRITE, OpenFlags, ProcessExitReason, ProcessHandle, SeekOrigin,
    valid_open_flags,
};
pub use syscall::{
    ABI_SYSCALLS, ABI_VERSION, MAX_ARGUMENT_BYTES, MAX_ARGUMENT_COUNT, MAX_ARGUMENT_TOTAL_BYTES,
    MAX_PATH_BYTES, SYSCALL_ARGUMENT_COUNT, SYSCALL_FIRST_ARGUMENT_REGISTER,
    SYSCALL_NUMBER_REGISTER, SYSCALL_RESERVED_REGISTER, Syscall, SyscallArguments, SyscallError,
    SyscallStatus, TaggedOutcome, abi_syscall, validate_range, validate_reserved_register,
};
