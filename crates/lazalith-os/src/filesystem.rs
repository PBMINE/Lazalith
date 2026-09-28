use crate::syscall::{IoHandle, ServiceOutcome, UserMemoryContext, UserMemoryError};
use crate::{HandleError, ValidatedSyscall, ValidatedSyscallKind};
use alloc::collections::TryReserveError;
use alloc::vec::Vec;
use core::{error::Error, fmt};
use lazalith_os_abi::{
    AbiError, AbiFileKind, DIRECTORY_NAME_CAPACITY, DIRECTORY_RECORD_SIZE, FilePermissions,
    FileStat, IoResult, MAX_PATH_BYTES, OpenFlags, SeekOrigin, SyscallError, TaggedOutcome,
};

pub const DEFAULT_MAX_FILE_BYTES: u64 = 1_048_576;
pub const DEFAULT_MAX_NODES: usize = 1024;
pub const DEFAULT_MAX_DIRECTORY_ENTRIES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileNodeKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileAccess {
    read: bool,
    write: bool,
}

impl FileAccess {
    pub const fn new(read: bool, write: bool) -> Self {
        Self { read, write }
    }

    pub const fn read(self) -> bool {
        self.read
    }

    pub const fn write(self) -> bool {
        self.write
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileMetadata {
    pub node: FileNodeId,
    pub kind: FileNodeKind,
    /// Capability class of the node: what a fully granted handle may do with it.
    ///
    /// This is a property of the node, not a grant. Per-handle access is
    /// enforced separately through [`FileAccess`]; the virtual filesystem has no
    /// accounts or node-level permissions, so this value is never a security
    /// decision on its own.
    pub capabilities: FilePermissions,
    pub size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    name: Vec<u8>,
    kind: FileNodeKind,
}

impl DirectoryEntry {
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    pub const fn kind(&self) -> FileNodeKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileOpen {
    pub node: FileNodeId,
    pub access: FileAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileSystemLimits {
    pub max_file_bytes: u64,
    pub max_nodes: usize,
    pub max_directory_entries: usize,
}

impl FileSystemLimits {
    pub const fn new(max_file_bytes: u64, max_nodes: usize, max_directory_entries: usize) -> Self {
        Self {
            max_file_bytes,
            max_nodes,
            max_directory_entries,
        }
    }
}

impl Default for FileSystemLimits {
    fn default() -> Self {
        Self::new(
            DEFAULT_MAX_FILE_BYTES,
            DEFAULT_MAX_NODES,
            DEFAULT_MAX_DIRECTORY_ENTRIES,
        )
    }
}

#[derive(Debug)]
pub enum FileSystemError {
    InvalidLimits,
    InvalidPath,
    EmptyPath,
    PathTooLong { length: u64, maximum: u64 },
    EmbeddedNul,
    NotFound,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    PermissionDenied,
    InvalidAccess,
    InvalidName { length: usize, maximum: usize },
    FileTooLarge { length: u64, maximum: u64 },
    OffsetOverflow,
    InvalidOffset { offset: u64, length: u64 },
    InvalidSeekOffset { offset: i64 },
    NodeLimit { maximum: usize },
    DirectoryLimit { maximum: usize },
    Allocation(TryReserveError),
    Abi(AbiError),
}

impl FileSystemError {
    pub fn syscall_error(&self) -> SyscallError {
        match self {
            Self::InvalidLimits
            | Self::InvalidPath
            | Self::EmptyPath
            | Self::EmbeddedNul
            | Self::InvalidAccess
            | Self::InvalidName { .. }
            | Self::InvalidSeekOffset { .. } => SyscallError::InvalidArgument,
            Self::PathTooLong { .. } | Self::OffsetOverflow | Self::InvalidOffset { .. } => {
                SyscallError::RangeOverflow
            }
            Self::NotFound => SyscallError::NotFound,
            Self::AlreadyExists => SyscallError::AlreadyExists,
            Self::NotDirectory => SyscallError::NotDirectory,
            Self::IsDirectory => SyscallError::IsDirectory,
            Self::PermissionDenied => SyscallError::PermissionDenied,
            Self::FileTooLarge { .. }
            | Self::NodeLimit { .. }
            | Self::DirectoryLimit { .. }
            | Self::Allocation(_) => SyscallError::ResourceExhausted,
            Self::Abi(source) => SyscallError::from(*source),
        }
    }
}

impl fmt::Display for FileSystemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid filesystem limits"),
            Self::InvalidPath => f.write_str("invalid virtual filesystem path"),
            Self::EmptyPath => f.write_str("virtual filesystem path is empty"),
            Self::PathTooLong { length, maximum } => {
                write!(f, "path length {length} exceeds {maximum}")
            }
            Self::EmbeddedNul => f.write_str("virtual filesystem path contains NUL"),
            Self::NotFound => f.write_str("virtual filesystem path was not found"),
            Self::AlreadyExists => f.write_str("virtual filesystem path already exists"),
            Self::NotDirectory => f.write_str("virtual filesystem path is not a directory"),
            Self::IsDirectory => f.write_str("virtual filesystem path is a directory"),
            Self::PermissionDenied => f.write_str("virtual filesystem permission denied"),
            Self::InvalidAccess => f.write_str("invalid virtual filesystem access mode"),
            Self::InvalidName { length, maximum } => {
                write!(
                    f,
                    "virtual filesystem entry name length {length} exceeds {maximum}"
                )
            }
            Self::FileTooLarge { length, maximum } => {
                write!(f, "file length {length} exceeds {maximum}")
            }
            Self::OffsetOverflow => f.write_str("virtual filesystem offset overflows"),
            Self::InvalidOffset { offset, length } => {
                write!(f, "file offset {offset} is outside length {length}")
            }
            Self::InvalidSeekOffset { offset } => write!(f, "invalid seek offset {offset}"),
            Self::NodeLimit { maximum } => write!(f, "filesystem node limit {maximum} reached"),
            Self::DirectoryLimit { maximum } => {
                write!(f, "directory entry limit {maximum} reached")
            }
            Self::Allocation(source) => write!(f, "filesystem allocation failed: {source}"),
            Self::Abi(source) => write!(f, "filesystem ABI error: {source}"),
        }
    }
}

impl Error for FileSystemError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Abi(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FileNodeId(core::num::NonZeroU32);

impl FileNodeId {
    pub fn new(value: u32) -> Option<Self> {
        core::num::NonZeroU32::new(value).map(Self)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }

    const fn index(self) -> usize {
        self.0.get() as usize - 1
    }
}

#[derive(Debug)]
enum FileNode {
    File { bytes: Vec<u8> },
    Directory { entries: Vec<StoredDirectoryEntry> },
}

#[derive(Debug)]
struct StoredDirectoryEntry {
    name: Vec<u8>,
    node: FileNodeId,
}

pub struct VirtualFileSystem {
    limits: FileSystemLimits,
    nodes: Vec<FileNode>,
}

impl VirtualFileSystem {
    pub fn new(limits: FileSystemLimits) -> Result<Self, FileSystemError> {
        if limits.max_file_bytes == 0
            || limits.max_nodes < 2
            || limits.max_nodes > u32::MAX as usize
            || limits.max_directory_entries == 0
        {
            return Err(FileSystemError::InvalidLimits);
        }
        let mut nodes = Vec::new();
        nodes.try_reserve(1).map_err(FileSystemError::Allocation)?;
        nodes.push(FileNode::Directory {
            entries: Vec::new(),
        });
        Ok(Self { limits, nodes })
    }

    pub fn with_defaults() -> Result<Self, FileSystemError> {
        Self::new(FileSystemLimits::default())
    }

    pub const fn limits(&self) -> FileSystemLimits {
        self.limits
    }

    pub const fn root(&self) -> FileNodeId {
        FileNodeId(core::num::NonZeroU32::new(1).unwrap())
    }

    pub fn metadata(&self, path: &[u8]) -> Result<FileMetadata, FileSystemError> {
        let node = self.resolve(path)?;
        self.metadata_node(node)
    }

    pub fn metadata_node(&self, node: FileNodeId) -> Result<FileMetadata, FileSystemError> {
        let kind = match self.node(node)? {
            FileNode::File { bytes: _ } => FileNodeKind::File,
            FileNode::Directory { .. } => FileNodeKind::Directory,
        };
        let size = match self.node(node)? {
            FileNode::File { bytes } => bytes.len() as u64,
            FileNode::Directory { entries } => entries.len() as u64,
        };
        let capabilities = match kind {
            FileNodeKind::File => {
                FilePermissions::new(FilePermissions::READ | FilePermissions::WRITE)
                    .map_err(FileSystemError::Abi)?
            }
            FileNodeKind::Directory => {
                FilePermissions::new(FilePermissions::READ | FilePermissions::EXECUTE)
                    .map_err(FileSystemError::Abi)?
            }
        };
        Ok(FileMetadata {
            node,
            kind,
            capabilities,
            size,
        })
    }

    pub fn open(&mut self, path: &[u8], flags: OpenFlags) -> Result<FileOpen, FileSystemError> {
        let read = flags.bits() & lazalith_os_abi::OPEN_READ != 0;
        let write = flags.bits() & lazalith_os_abi::OPEN_WRITE != 0;
        let create = flags.bits() & lazalith_os_abi::OPEN_CREATE != 0;
        let truncate = flags.bits() & lazalith_os_abi::OPEN_TRUNCATE != 0;
        if !read && !write {
            return Err(FileSystemError::InvalidAccess);
        }
        if (create || truncate) && !write {
            return Err(FileSystemError::InvalidAccess);
        }
        let parts = path_parts(path)?;
        let Some((last, parent_parts)) = parts.split_last() else {
            return Err(FileSystemError::IsDirectory);
        };
        let parent = self.resolve_parts(parent_parts)?;
        let existing = self.find_child(parent, last)?;
        if let Some(node) = existing {
            if !matches!(self.node(node)?, FileNode::File { .. }) {
                return Err(FileSystemError::IsDirectory);
            }
            if truncate {
                let FileNode::File { bytes } = self.node_mut(node)? else {
                    return Err(FileSystemError::IsDirectory);
                };
                bytes.clear();
            }
            return Ok(FileOpen {
                node,
                access: FileAccess::new(read, write),
            });
        }
        if !create {
            return Err(FileSystemError::NotFound);
        }
        self.ensure_node_capacity()?;
        let max_directory_entries = self.limits.max_directory_entries;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        if entries.len() >= max_directory_entries {
            return Err(FileSystemError::DirectoryLimit {
                maximum: max_directory_entries,
            });
        }
        entries
            .try_reserve(1)
            .map_err(FileSystemError::Allocation)?;
        let name = copy_name(last)?;
        let node = self.allocate_node(FileNode::File { bytes: Vec::new() })?;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.push(StoredDirectoryEntry { name, node });
        Ok(FileOpen {
            node,
            access: FileAccess::new(read, write),
        })
    }

    pub fn read_at(
        &self,
        node: FileNodeId,
        offset: u64,
        length: u64,
        access: FileAccess,
    ) -> Result<Vec<u8>, FileSystemError> {
        if !access.read {
            return Err(FileSystemError::PermissionDenied);
        }
        let FileNode::File { bytes } = self.node(node)? else {
            return Err(FileSystemError::IsDirectory);
        };
        let file_length =
            u64::try_from(bytes.len()).map_err(|_| FileSystemError::OffsetOverflow)?;
        if offset > file_length {
            return Err(FileSystemError::InvalidOffset {
                offset,
                length: file_length,
            });
        }
        let available = file_length - offset;
        let transfer = length.min(available);
        let transfer_usize =
            usize::try_from(transfer).map_err(|_| FileSystemError::OffsetOverflow)?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(transfer_usize)
            .map_err(FileSystemError::Allocation)?;
        let start = usize::try_from(offset).map_err(|_| FileSystemError::OffsetOverflow)?;
        let end = start
            .checked_add(transfer_usize)
            .ok_or(FileSystemError::OffsetOverflow)?;
        output.extend_from_slice(
            bytes
                .get(start..end)
                .ok_or(FileSystemError::OffsetOverflow)?,
        );
        Ok(output)
    }

    pub fn write_at(
        &mut self,
        node: FileNodeId,
        offset: u64,
        data: &[u8],
        access: FileAccess,
    ) -> Result<u64, FileSystemError> {
        if !access.write {
            return Err(FileSystemError::PermissionDenied);
        }
        let max_file_bytes = self.limits.max_file_bytes;
        let FileNode::File { bytes } = self.node_mut(node)? else {
            return Err(FileSystemError::IsDirectory);
        };
        let file_length =
            u64::try_from(bytes.len()).map_err(|_| FileSystemError::OffsetOverflow)?;
        if offset > file_length {
            return Err(FileSystemError::InvalidOffset {
                offset,
                length: file_length,
            });
        }
        let data_length = u64::try_from(data.len()).map_err(|_| FileSystemError::FileTooLarge {
            length: u64::MAX,
            maximum: max_file_bytes,
        })?;
        let data_end = offset
            .checked_add(data_length)
            .ok_or(FileSystemError::OffsetOverflow)?;
        let end = file_length.max(data_end);
        if end > max_file_bytes {
            return Err(FileSystemError::FileTooLarge {
                length: end,
                maximum: max_file_bytes,
            });
        }
        let start = usize::try_from(offset).map_err(|_| FileSystemError::OffsetOverflow)?;
        let overwrite = data.len().min(bytes.len().saturating_sub(start));
        let extension = data.len() - overwrite;
        if extension != 0 {
            bytes
                .try_reserve(extension)
                .map_err(FileSystemError::Allocation)?;
        }
        bytes[start..start + overwrite].copy_from_slice(&data[..overwrite]);
        if extension != 0 {
            bytes.extend_from_slice(&data[overwrite..]);
        }
        let written = data_length;
        Ok(written)
    }

    pub fn seek(
        &self,
        node: FileNodeId,
        current: u64,
        offset: i64,
        origin: SeekOrigin,
    ) -> Result<u64, FileSystemError> {
        let FileNode::File { bytes } = self.node(node)? else {
            return Err(FileSystemError::IsDirectory);
        };
        let length = bytes.len() as u64;
        if current > length {
            return Err(FileSystemError::InvalidOffset {
                offset: current,
                length,
            });
        }
        let base = match origin {
            SeekOrigin::Start => 0,
            SeekOrigin::Current => current,
            SeekOrigin::End => length,
        };
        let result = (base as i128) + (offset as i128);
        if result < 0 || result > length as i128 {
            return Err(FileSystemError::InvalidSeekOffset { offset });
        }
        Ok(result as u64)
    }

    pub fn list(&self, path: &[u8]) -> Result<Vec<DirectoryEntry>, FileSystemError> {
        let node = self.resolve(path)?;
        let FileNode::Directory { entries } = self.node(node)? else {
            return Err(FileSystemError::NotDirectory);
        };
        let mut output = Vec::new();
        output
            .try_reserve_exact(entries.len())
            .map_err(FileSystemError::Allocation)?;
        for entry in entries {
            let kind = match self.node(entry.node)? {
                FileNode::File { .. } => FileNodeKind::File,
                FileNode::Directory { .. } => FileNodeKind::Directory,
            };
            output.push(DirectoryEntry {
                name: copy_name(&entry.name)?,
                kind,
            });
        }
        output.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        Ok(output)
    }

    pub fn insert_file(&mut self, path: &[u8], data: &[u8]) -> Result<FileNodeId, FileSystemError> {
        let data_length = u64::try_from(data.len()).map_err(|_| FileSystemError::FileTooLarge {
            length: u64::MAX,
            maximum: self.limits.max_file_bytes,
        })?;
        if data_length > self.limits.max_file_bytes {
            return Err(FileSystemError::FileTooLarge {
                length: data_length,
                maximum: self.limits.max_file_bytes,
            });
        }
        let parts = path_parts(path)?;
        let Some((last, parent_parts)) = parts.split_last() else {
            return Err(FileSystemError::AlreadyExists);
        };
        let parent = self.resolve_parts(parent_parts)?;
        if self.find_child(parent, last)?.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        self.ensure_node_capacity()?;
        let max_directory_entries = self.limits.max_directory_entries;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        if entries.len() >= max_directory_entries {
            return Err(FileSystemError::DirectoryLimit {
                maximum: max_directory_entries,
            });
        }
        entries
            .try_reserve(1)
            .map_err(FileSystemError::Allocation)?;
        let name = copy_name(last)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(data.len())
            .map_err(FileSystemError::Allocation)?;
        bytes.extend_from_slice(data);
        let node = self.allocate_node(FileNode::File { bytes })?;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.push(StoredDirectoryEntry { name, node });
        Ok(node)
    }

    /// Moves a node from one name to another.
    ///
    /// A rename is a *re-link*, not a copy: the node keeps its identity, so a handle
    /// open on the old name still reads the same bytes and the same node now has a
    /// different name. That is the property that makes a rename cheap and that makes
    /// a copy the wrong operation — a copy would leave two nodes with the same
    /// contents, and the next write through one handle would not be visible through
    /// the other.
    ///
    /// Moving a directory moves the *entry*, not its contents, so a directory that
    /// moves into itself is refused rather than building a cycle nobody can walk.
    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), FileSystemError> {
        let from_parts = path_parts(from)?;
        let to_parts = path_parts(to)?;
        let Some((from_name, from_parent_parts)) = from_parts.split_last() else {
            return Err(FileSystemError::InvalidPath);
        };
        let Some((to_name, to_parent_parts)) = to_parts.split_last() else {
            return Err(FileSystemError::InvalidPath);
        };
        let from_parent = self.resolve_parts(from_parent_parts)?;
        let to_parent = self.resolve_parts(to_parent_parts)?;
        let Some(node) = self.find_child(from_parent, from_name)? else {
            return Err(FileSystemError::NotFound);
        };
        if self.find_child(to_parent, to_name)?.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        // A directory may not be moved inside itself. The question is whether the
        // *moved node* contains the *destination.s parent*, and not the other way
        // round: the destination parent is still in the tree and usually an ancestor of
        // the node being moved, so asking the other question refuses every rename.
        if self.contains_node(node, to_parent)? {
            return Err(FileSystemError::InvalidPath);
        }
        let new_name = copy_name(to_name)?;
        let max_directory_entries = self.limits.max_directory_entries;
        {
            let FileNode::Directory { entries } = self.node_mut(to_parent)? else {
                return Err(FileSystemError::NotDirectory);
            };
            if entries.len() >= max_directory_entries {
                return Err(FileSystemError::DirectoryLimit {
                    maximum: max_directory_entries,
                });
            }
            entries
                .try_reserve(1)
                .map_err(FileSystemError::Allocation)?;
        }
        // The removal and the insertion are separate because the borrow of the source
        // directory has to end before the destination can be touched, and the node id
        // is the thing that survives between them.
        let index = self
            .entry_index(from_parent, from_name)?
            .ok_or(FileSystemError::NotFound)?;
        let FileNode::Directory { entries } = self.node_mut(from_parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.remove(index);
        let FileNode::Directory { entries } = self.node_mut(to_parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.push(StoredDirectoryEntry {
            name: new_name,
            node,
        });
        Ok(())
    }

    /// Removes a file, or a directory that is empty.
    ///
    /// A directory that still has entries is refused rather than emptied, because
    /// removing a directory means "I am done with this" and emptying a directory is a
    /// different act that happens to share a name. A caller that wants the second
    /// removes the children first.
    pub fn remove(&mut self, path: &[u8]) -> Result<(), FileSystemError> {
        let parts = path_parts(path)?;
        let Some((name, parent_parts)) = parts.split_last() else {
            return Err(FileSystemError::InvalidPath);
        };
        let parent = self.resolve_parts(parent_parts)?;
        let node = self
            .find_child(parent, name)?
            .ok_or(FileSystemError::NotFound)?;
        if let FileNode::Directory { entries } = self.node(node)?
            && !entries.is_empty()
        {
            return Err(FileSystemError::IsDirectory);
        }
        let index = self
            .entry_index(parent, name)?
            .ok_or(FileSystemError::NotFound)?;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.remove(index);
        // The node itself is left allocated rather than pushed onto a free list. A
        // free list is the right thing for a filesystem that is created and destroyed
        // a million times, and this one is a guest's memory inside a process: its
        // whole lifetime is one boot.
        Ok(())
    }

    /// Sets a file's length, growing it with zeros or cutting it short.
    ///
    /// Growing with zeros rather than with whatever was there because a program that
    /// seeks past the end and reads must get zeros and not another file's bytes.
    pub fn truncate(&mut self, path: &[u8], length: u64) -> Result<(), FileSystemError> {
        let maximum = self.limits.max_file_bytes;
        if length > maximum {
            return Err(FileSystemError::FileTooLarge { length, maximum });
        }
        let node = self.metadata_node_of(path)?;
        let FileNode::File { bytes } = self.node_mut(node)? else {
            return Err(FileSystemError::IsDirectory);
        };
        let length = usize::try_from(length)
            .map_err(|_| FileSystemError::FileTooLarge { length, maximum })?;
        if length > bytes.len() {
            let additional = length - bytes.len();
            bytes
                .try_reserve_exact(additional)
                .map_err(FileSystemError::Allocation)?;
            bytes.resize(length, 0);
        } else {
            bytes.truncate(length);
        }
        Ok(())
    }

    /// Every path under `path`, in a deterministic order.
    ///
    /// `path` itself is included, and its children follow it in sorted order, so two
    /// walks of the same tree produce the same list — which is what makes a walk usable
    /// in a test and in a tool that compares two trees.
    ///
    /// The list is bounded by the node limit the filesystem was built with, because it
    /// is bounded by the filesystem, and a walk that can produce more entries than
    /// there are nodes is a walk that can exhaust memory.
    pub fn walk(&self, path: &[u8]) -> Result<Vec<Vec<u8>>, FileSystemError> {
        let start = self.metadata_node_of(path)?;
        let mut found = Vec::new();
        // The walk carries the *path* it is at rather than a bare prefix, so the root
        // reports itself as `/` and a subtree reports itself as what it was asked
        // for. A prefix that started empty would report the root as `""`, which is not
        // a path anything can open.
        let mut here = path.to_vec();
        if here.is_empty() {
            here.push(b'/');
        }
        self.walk_from(start, &mut here, &mut found)?;
        Ok(found)
    }

    /// The walk itself, on borrowed state so it can recurse without cloning nodes.
    fn walk_from(
        &self,
        node: FileNodeId,
        path: &mut Vec<u8>,
        found: &mut Vec<Vec<u8>>,
    ) -> Result<(), FileSystemError> {
        if path.len() > usize::try_from(MAX_PATH_BYTES).unwrap_or(usize::MAX) {
            return Err(FileSystemError::PathTooLong {
                length: u64::try_from(path.len()).unwrap_or(u64::MAX),
                maximum: MAX_PATH_BYTES,
            });
        }
        found.try_reserve(1).map_err(FileSystemError::Allocation)?;
        found.push(path.clone());
        let FileNode::Directory { entries } = self.node(node)? else {
            return Ok(());
        };
        let mut sorted: Vec<&StoredDirectoryEntry> = entries.iter().collect();
        sorted.sort_by(|left, right| left.name.cmp(&right.name));
        for entry in sorted {
            let length = path.len();
            if path.last() != Some(&b'/') {
                path.push(b'/');
            }
            path.extend_from_slice(&entry.name);
            self.walk_from(entry.node, path, found)?;
            path.truncate(length);
        }
        Ok(())
    }

    /// The node a path names, without asking for metadata.
    fn metadata_node_of(&self, path: &[u8]) -> Result<FileNodeId, FileSystemError> {
        let parts = path_parts(path)?;
        if parts.is_empty() {
            return Ok(self.root());
        }
        let mut current = self.root();
        for part in &parts {
            current = self
                .find_child(current, part)?
                .ok_or(FileSystemError::NotFound)?;
        }
        Ok(current)
    }

    /// Whether a directory contains a node, at any depth.
    fn contains_node(
        &self,
        directory: FileNodeId,
        node: FileNodeId,
    ) -> Result<bool, FileSystemError> {
        if directory == node {
            return Ok(true);
        }
        let FileNode::Directory { entries } = self.node(directory)? else {
            return Ok(false);
        };
        for entry in entries {
            if entry.node == node {
                return Ok(true);
            }
            if self.contains_node(entry.node, node)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Where a name sits in a directory's entries.
    fn entry_index(
        &self,
        directory: FileNodeId,
        name: &[u8],
    ) -> Result<Option<usize>, FileSystemError> {
        let FileNode::Directory { entries } = self.node(directory)? else {
            return Ok(None);
        };
        Ok(entries.iter().position(|entry| entry.name == name))
    }

    pub fn insert_directory(&mut self, path: &[u8]) -> Result<FileNodeId, FileSystemError> {
        let parts = path_parts(path)?;
        let Some((last, parent_parts)) = parts.split_last() else {
            return Ok(self.root());
        };
        let parent = self.resolve_parts(parent_parts)?;
        if self.find_child(parent, last)?.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        self.ensure_node_capacity()?;
        let max_directory_entries = self.limits.max_directory_entries;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        if entries.len() >= max_directory_entries {
            return Err(FileSystemError::DirectoryLimit {
                maximum: max_directory_entries,
            });
        }
        entries
            .try_reserve(1)
            .map_err(FileSystemError::Allocation)?;
        let name = copy_name(last)?;
        let node = self.allocate_node(FileNode::Directory {
            entries: Vec::new(),
        })?;
        let FileNode::Directory { entries } = self.node_mut(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        entries.push(StoredDirectoryEntry { name, node });
        Ok(node)
    }

    fn resolve(&self, path: &[u8]) -> Result<FileNodeId, FileSystemError> {
        let parts = path_parts(path)?;
        self.resolve_parts(&parts)
    }

    fn resolve_parts(&self, parts: &[&[u8]]) -> Result<FileNodeId, FileSystemError> {
        let mut current = self.root();
        for part in parts {
            let FileNode::Directory { entries } = self.node(current)? else {
                return Err(FileSystemError::NotDirectory);
            };
            let Some(entry) = entries.iter().find(|entry| entry.name == *part) else {
                return Err(FileSystemError::NotFound);
            };
            current = entry.node;
        }
        Ok(current)
    }

    fn find_child(
        &self,
        parent: FileNodeId,
        name: &[u8],
    ) -> Result<Option<FileNodeId>, FileSystemError> {
        let FileNode::Directory { entries } = self.node(parent)? else {
            return Err(FileSystemError::NotDirectory);
        };
        Ok(entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.node))
    }

    fn node(&self, node: FileNodeId) -> Result<&FileNode, FileSystemError> {
        self.nodes
            .get(node.index())
            .ok_or(FileSystemError::NotFound)
    }

    fn node_mut(&mut self, node: FileNodeId) -> Result<&mut FileNode, FileSystemError> {
        self.nodes
            .get_mut(node.index())
            .ok_or(FileSystemError::NotFound)
    }

    fn ensure_node_capacity(&self) -> Result<(), FileSystemError> {
        if self.nodes.len() >= self.limits.max_nodes {
            return Err(FileSystemError::NodeLimit {
                maximum: self.limits.max_nodes,
            });
        }
        Ok(())
    }

    fn allocate_node(&mut self, node: FileNode) -> Result<FileNodeId, FileSystemError> {
        self.nodes
            .try_reserve(1)
            .map_err(FileSystemError::Allocation)?;
        self.nodes.push(node);
        let value = u32::try_from(self.nodes.len()).map_err(|_| FileSystemError::NodeLimit {
            maximum: self.limits.max_nodes,
        })?;
        FileNodeId::new(value).ok_or(FileSystemError::NodeLimit {
            maximum: self.limits.max_nodes,
        })
    }
}

fn copy_name(name: &[u8]) -> Result<Vec<u8>, FileSystemError> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(name.len())
        .map_err(FileSystemError::Allocation)?;
    copy.extend_from_slice(name);
    Ok(copy)
}

fn path_parts(path: &[u8]) -> Result<Vec<&[u8]>, FileSystemError> {
    let length = u64::try_from(path.len()).map_err(|_| FileSystemError::PathTooLong {
        length: u64::MAX,
        maximum: MAX_PATH_BYTES,
    })?;
    if length == 0 {
        return Err(FileSystemError::EmptyPath);
    }
    if length > MAX_PATH_BYTES {
        return Err(FileSystemError::PathTooLong {
            length,
            maximum: MAX_PATH_BYTES,
        });
    }
    if path.contains(&0) {
        return Err(FileSystemError::EmbeddedNul);
    }
    if path[0] != b'/' {
        return Err(FileSystemError::InvalidPath);
    }
    if path.len() == 1 {
        return Ok(Vec::new());
    }
    if path[path.len() - 1] == b'/' {
        return Err(FileSystemError::InvalidPath);
    }
    let mut parts = Vec::new();
    parts
        .try_reserve(path.len())
        .map_err(FileSystemError::Allocation)?;
    for part in path[1..].split(|byte| *byte == b'/') {
        if part.is_empty() || part == b"." || part == b".." {
            return Err(FileSystemError::InvalidPath);
        }
        if part.len() > DIRECTORY_NAME_CAPACITY {
            return Err(FileSystemError::InvalidName {
                length: part.len(),
                maximum: DIRECTORY_NAME_CAPACITY,
            });
        }
        parts.push(part);
    }
    Ok(parts)
}

pub struct FileSystemService {
    filesystem: VirtualFileSystem,
}

impl FileSystemService {
    pub const fn new(filesystem: VirtualFileSystem) -> Self {
        Self { filesystem }
    }

    pub const fn filesystem(&self) -> &VirtualFileSystem {
        &self.filesystem
    }

    pub fn filesystem_mut(&mut self) -> &mut VirtualFileSystem {
        &mut self.filesystem
    }

    fn invoke_open(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        path: lazalith_types::VirtualAddress,
        path_length: u64,
        flags: OpenFlags,
    ) -> ServiceOutcome {
        let path = match read_path(memory, path, path_length) {
            Ok(path) => path,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let prepared = match memory.handles().reserve_file() {
            Ok(prepared) => prepared,
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        };
        let opened = match self.filesystem.open(&path, flags) {
            Ok(opened) => opened,
            Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
        };
        let handle = prepared.handle;
        if let Err(error) =
            memory
                .handles()
                .commit_prepared_file(prepared, opened.node, opened.access)
        {
            return ServiceOutcome::Return(handle_error_outcome(error));
        }
        ServiceOutcome::Return(TaggedOutcome::success(handle.get()))
    }

    fn invoke_close(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        handle: lazalith_os_abi::FileHandle,
    ) -> ServiceOutcome {
        ServiceOutcome::Return(match memory.handles().close_file(handle) {
            Ok(()) => TaggedOutcome::success(0),
            Err(error) => handle_error_outcome(error),
        })
    }

    fn invoke_read(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        handle: IoHandle,
        buffer: lazalith_types::VirtualAddress,
        length: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let Some(handle) = handle.file() else {
            return ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::NotSupported, 0));
        };
        let file = match memory.handles().file(handle).copied() {
            Ok(file) => file,
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        };
        if !file.access().read {
            return ServiceOutcome::Return(TaggedOutcome::failure(
                SyscallError::PermissionDenied,
                0,
            ));
        }
        let data = match self
            .filesystem
            .read_at(file.node(), file.offset(), length, file.access())
        {
            Ok(data) => data,
            Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
        };
        if let Err(error) = memory.write_bytes(buffer, &data) {
            return ServiceOutcome::Return(memory_error_outcome(error));
        }
        let transferred = u64::try_from(data.len())
            .map_err(|_| TaggedOutcome::failure(SyscallError::ResourceExhausted, 0));
        let transferred = match transferred {
            Ok(transferred) => transferred,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let io = match IoResult::new(
            memory.config(),
            transferred,
            lazalith_os_abi::SyscallStatus::Ok,
        ) {
            Ok(io) => io,
            Err(error) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::from(error),
                    0,
                ));
            }
        };
        if let Err(error) = memory.write_bytes(result, &io.encode()) {
            return ServiceOutcome::Return(memory_error_outcome(error));
        }
        let next_offset = file
            .offset()
            .checked_add(transferred)
            .ok_or_else(|| TaggedOutcome::failure(SyscallError::RangeOverflow, 0));
        let next_offset = match next_offset {
            Ok(offset) => offset,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        match memory.handles().file_mut(handle) {
            Ok(open_file) => open_file.set_offset(next_offset),
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        }
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }

    fn invoke_write(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        handle: IoHandle,
        buffer: lazalith_types::VirtualAddress,
        length: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let Some(handle) = handle.file() else {
            return ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::NotSupported, 0));
        };
        let file = match memory.handles().file(handle).copied() {
            Ok(file) => file,
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        };
        if !file.access().write {
            return ServiceOutcome::Return(TaggedOutcome::failure(
                SyscallError::PermissionDenied,
                0,
            ));
        }
        let data = match read_bytes(memory, buffer, length) {
            Ok(data) => data,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let transferred =
            match self
                .filesystem
                .write_at(file.node(), file.offset(), &data, file.access())
            {
                Ok(transferred) => transferred,
                Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
            };
        let io = match IoResult::new(
            memory.config(),
            transferred,
            lazalith_os_abi::SyscallStatus::Ok,
        ) {
            Ok(io) => io,
            Err(error) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::from(error),
                    0,
                ));
            }
        };
        if let Err(error) = memory.write_bytes(result, &io.encode()) {
            return ServiceOutcome::Return(memory_error_outcome(error));
        }
        let next_offset = file
            .offset()
            .checked_add(transferred)
            .ok_or_else(|| TaggedOutcome::failure(SyscallError::RangeOverflow, 0));
        let next_offset = match next_offset {
            Ok(offset) => offset,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        match memory.handles().file_mut(handle) {
            Ok(open_file) => open_file.set_offset(next_offset),
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        }
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }

    fn invoke_seek(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        handle: lazalith_os_abi::FileHandle,
        offset: i64,
        origin: SeekOrigin,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let file = match memory.handles().file(handle).copied() {
            Ok(file) => file,
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        };
        let offset = match self
            .filesystem
            .seek(file.node(), file.offset(), offset, origin)
        {
            Ok(offset) => offset,
            Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
        };
        if let Err(error) = memory.write_bytes(result, &offset.to_le_bytes()) {
            return ServiceOutcome::Return(memory_error_outcome(error));
        }
        match memory.handles().file_mut(handle) {
            Ok(open_file) => open_file.set_offset(offset),
            Err(error) => return ServiceOutcome::Return(handle_error_outcome(error)),
        }
        ServiceOutcome::Return(TaggedOutcome::success(0))
    }

    fn invoke_stat(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        path: lazalith_types::VirtualAddress,
        path_length: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let path = match read_path(memory, path, path_length) {
            Ok(path) => path,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let metadata = match self.filesystem.metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
        };
        let kind = match metadata.kind {
            super::filesystem::FileNodeKind::File => AbiFileKind::File,
            super::filesystem::FileNodeKind::Directory => AbiFileKind::Directory,
        };
        let stat = match FileStat::new(memory.config(), kind, metadata.capabilities, metadata.size)
        {
            Ok(stat) => stat,
            Err(error) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::from(error),
                    0,
                ));
            }
        };
        match memory.write_bytes(result, &stat.encode()) {
            Ok(()) => ServiceOutcome::Return(TaggedOutcome::success(0)),
            Err(error) => ServiceOutcome::Return(memory_error_outcome(error)),
        }
    }

    fn invoke_list_directory(
        &mut self,
        memory: &mut UserMemoryContext<'_>,
        path: lazalith_types::VirtualAddress,
        path_length: u64,
        records: lazalith_types::VirtualAddress,
        capacity: u64,
        result: lazalith_types::VirtualAddress,
    ) -> ServiceOutcome {
        let path = match read_path(memory, path, path_length) {
            Ok(path) => path,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let entries = match self.filesystem.list(&path) {
            Ok(entries) => entries,
            Err(error) => return ServiceOutcome::Return(filesystem_error_outcome(error)),
        };
        let capacity = match usize::try_from(capacity) {
            Ok(capacity) => capacity,
            Err(_) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::ResourceExhausted,
                    0,
                ));
            }
        };
        let count = entries.len().min(capacity / DIRECTORY_RECORD_SIZE);
        let output_length = count
            .checked_mul(DIRECTORY_RECORD_SIZE)
            .ok_or_else(|| TaggedOutcome::failure(SyscallError::ResourceExhausted, 0));
        let output_length = match output_length {
            Ok(length) => length,
            Err(outcome) => return ServiceOutcome::Return(outcome),
        };
        let mut output = Vec::new();
        if let Err(error) = output.try_reserve_exact(output_length) {
            return ServiceOutcome::Return(filesystem_error_outcome(FileSystemError::Allocation(
                error,
            )));
        }
        for entry in entries.iter().take(count) {
            let kind = match entry.kind() {
                super::filesystem::FileNodeKind::File => AbiFileKind::File,
                super::filesystem::FileNodeKind::Directory => AbiFileKind::Directory,
            };
            let record = match lazalith_os_abi::DirectoryRecord::new(entry.name(), kind) {
                Ok(record) => record,
                Err(error) => {
                    return ServiceOutcome::Return(TaggedOutcome::failure(
                        SyscallError::from(error),
                        0,
                    ));
                }
            };
            output.extend_from_slice(&record.encode());
        }
        if let Err(error) = memory.write_bytes(records, &output) {
            return ServiceOutcome::Return(memory_error_outcome(error));
        }
        let io = match IoResult::new(
            memory.config(),
            count as u64,
            lazalith_os_abi::SyscallStatus::Ok,
        ) {
            Ok(io) => io,
            Err(error) => {
                return ServiceOutcome::Return(TaggedOutcome::failure(
                    SyscallError::from(error),
                    0,
                ));
            }
        };
        match memory.write_bytes(result, &io.encode()) {
            Ok(()) => ServiceOutcome::Return(TaggedOutcome::success(0)),
            Err(error) => ServiceOutcome::Return(memory_error_outcome(error)),
        }
    }
}

impl crate::KernelService for FileSystemService {
    fn invoke(
        &mut self,
        syscall: &ValidatedSyscall,
        memory: &mut UserMemoryContext<'_>,
    ) -> ServiceOutcome {
        match syscall.kind() {
            ValidatedSyscallKind::Open {
                path,
                path_length,
                flags,
            } => self.invoke_open(memory, path, path_length, flags),
            ValidatedSyscallKind::Close { handle } => self.invoke_close(memory, handle),
            ValidatedSyscallKind::Read {
                handle,
                buffer,
                length,
                result,
            } => self.invoke_read(memory, handle, buffer, length, result),
            ValidatedSyscallKind::Write {
                handle,
                buffer,
                length,
                result,
            } => self.invoke_write(memory, handle, buffer, length, result),
            ValidatedSyscallKind::Seek {
                handle,
                offset,
                origin,
                result,
            } => self.invoke_seek(memory, handle, offset, origin, result),
            ValidatedSyscallKind::Stat {
                path,
                path_length,
                result,
            } => self.invoke_stat(memory, path, path_length, result),
            ValidatedSyscallKind::ListDirectory {
                path,
                path_length,
                records,
                capacity,
                result,
            } => self.invoke_list_directory(memory, path, path_length, records, capacity, result),
            _ => ServiceOutcome::Return(TaggedOutcome::failure(SyscallError::NotSupported, 0)),
        }
    }
}

fn read_path(
    memory: &mut UserMemoryContext<'_>,
    path: lazalith_types::VirtualAddress,
    length: u64,
) -> Result<Vec<u8>, TaggedOutcome> {
    read_bytes(memory, path, length)
}

fn read_bytes(
    memory: &mut UserMemoryContext<'_>,
    address: lazalith_types::VirtualAddress,
    length: u64,
) -> Result<Vec<u8>, TaggedOutcome> {
    let length = usize::try_from(length)
        .map_err(|_| TaggedOutcome::failure(SyscallError::ResourceExhausted, 0))?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(length)
        .map_err(|_| TaggedOutcome::failure(SyscallError::ResourceExhausted, 0))?;
    output.resize(length, 0);
    memory
        .read_bytes(address, &mut output)
        .map_err(memory_error_outcome)?;
    Ok(output)
}

fn filesystem_error_outcome(error: FileSystemError) -> TaggedOutcome {
    TaggedOutcome::failure(error.syscall_error(), 0)
}

fn handle_error_outcome(error: HandleError) -> TaggedOutcome {
    let status = match error {
        HandleError::NotFound { .. } => SyscallError::InvalidHandle,
        HandleError::Duplicate { .. } | HandleError::Exhausted => SyscallError::ResourceExhausted,
        HandleError::Allocation(_) => SyscallError::ResourceExhausted,
    };
    TaggedOutcome::failure(status, 0)
}

fn memory_error_outcome(error: UserMemoryError) -> TaggedOutcome {
    TaggedOutcome::failure(error.syscall_error(), 0)
}
