//! Descriptor-anchored capabilities for private synchronization state.
//!
//! These types are limited to application-owned local state. They never accept
//! a user [`super::path::SyncPath`], expose an unanchored child path, or make an
//! advisory lock a security boundary. Cooperative sync writers must retain one
//! [`PrivateStateLock`] before creating or mutating files.

use std::fs::File;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::FileExt as _;
use std::path::Path;

use fs2::FileExt as _;
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RawMode, RenameFlags, fchmod, fstat, fsync, mkdirat,
    open, openat, renameat_with, statat,
};
use thiserror::Error;

const MAX_STORAGE_KEY_BYTES: usize = 64;
const WRITER_LOCK_KEY: &str = "writer.lock";
const PRIVATE_DIRECTORY_MODE: RawMode = 0o700;
const PRIVATE_FILE_MODE: RawMode = 0o600;
/// Maximum memory allocated by one private-state read operation.
pub const MAX_PRIVATE_STATE_READ_BYTES: u64 = 16 * 1_024 * 1_024;

/// A single internal state entry name, never a synchronized user path.
///
/// Keys are one to 64 ASCII bytes and match
/// `[a-z0-9][a-z0-9._-]{0,63}`. This admits UUIDs and fixed names such as
/// `binding.v1`, while excluding separators, dot components, hidden names, and
/// the module-owned `writer.lock` name.
pub struct StateKey(String);

impl StateKey {
    /// Validates an internal storage key before making a system call.
    pub fn new(value: &str) -> Result<Self, StateDirError> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes.len() > MAX_STORAGE_KEY_BYTES
            || value == WRITER_LOCK_KEY
            || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
            || !bytes.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(StateDirError::InvalidKey);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns an internal storage component, never a synchronized user path.
    ///
    /// Callers must not include this value in diagnostics or user-visible text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One opaque observed private directory-entry identity.
///
/// The fields deliberately remain inaccessible: this value can only be
/// obtained from a complete PrivateStateInventory.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct PrivateStateEntryIdentity {
    device: u64,
    inode: u64,
}

/// The safe type of one observed private directory entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivateStateEntryKind {
    /// An owner-only, singly linked regular file.
    RegularFile,
    /// An owner-only private child directory. Inventory does not recurse into it.
    Directory,
    /// The fixed private writer lock entry, not a caller-selected StateKey.
    WriterLock,
}

/// One inventory name. writer.lock is deliberately distinct from StateKey.
pub enum PrivateStateInventoryName {
    /// A caller-selectable validated internal storage key.
    State(StateKey),
    /// The fixed writer lock name.
    WriterLock,
}

impl PrivateStateInventoryName {
    /// Returns the validated state key when this is not the fixed writer lock.
    #[must_use]
    pub const fn state_key(&self) -> Option<&StateKey> {
        match self {
            Self::State(key) => Some(key),
            Self::WriterLock => None,
        }
    }
}

/// One complete bounded private-directory observation.
pub struct PrivateStateInventoryEntry {
    name: PrivateStateInventoryName,
    identity: PrivateStateEntryIdentity,
    kind: PrivateStateEntryKind,
    byte_length: u64,
}

impl PrivateStateInventoryEntry {
    /// Returns the entry name, including the distinct writer-lock case.
    #[must_use]
    pub const fn name(&self) -> &PrivateStateInventoryName {
        &self.name
    }

    /// Returns the observed safe entry type.
    #[must_use]
    pub const fn kind(&self) -> PrivateStateEntryKind {
        self.kind
    }

    /// Returns the observed metadata size. Directories are not recursively sized.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Returns an opaque entry identity for future exact-entry operations.
    #[must_use]
    pub const fn identity(&self) -> PrivateStateEntryIdentity {
        self.identity
    }
}

/// A complete bounded, descriptor-relative private directory inventory.
///
/// This is best-effort observation under the matching cooperative exclusive
/// lock, not an atomic filesystem snapshot. A failure returns no partial
/// inventory. Both writer.lock and every accepted entry name count toward
/// total_key_bytes; only regular files and the writer lock count toward
/// total_file_bytes.
pub struct PrivateStateInventory {
    entries: Vec<PrivateStateInventoryEntry>,
    total_key_bytes: u64,
    total_file_bytes: u64,
}

impl PrivateStateInventory {
    /// Returns every validated direct entry in this complete observation.
    #[must_use]
    pub fn entries(&self) -> &[PrivateStateInventoryEntry] {
        &self.entries
    }

    /// Returns direct-entry name bytes, including writer.lock.
    #[must_use]
    pub const fn total_key_bytes(&self) -> u64 {
        self.total_key_bytes
    }

    /// Returns direct regular-file bytes, including the writer lock.
    #[must_use]
    pub const fn total_file_bytes(&self) -> u64 {
        self.total_file_bytes
    }
}

/// Result of a private no-replace promotion.
pub enum PrivateStatePromotion {
    /// The source was atomically renamed and this is its new validated capability.
    Promoted(PrivateStateFile),
    /// The destination already existed; both validated capabilities remain available.
    Existing {
        /// The unchanged source capability.
        source: PrivateStateFile,
        /// The validated incumbent destination capability.
        destination: PrivateStateFile,
    },
}

/// A redacted private-state capability failure.
///
/// I/O variants retain only a static operation, [`ErrorKind`], and errno. They
/// never retain a caller path, storage key, or arbitrary OS error message.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum StateDirError {
    /// An internal key is outside the fixed ASCII grammar.
    #[error("invalid private state key")]
    InvalidKey,
    /// An opened directory is not private, owned, or a real directory.
    #[error("unsafe private state directory")]
    UnsafeDirectory,
    /// An opened file is not private, owned, regular, or singly linked.
    #[error("unsafe private state file")]
    UnsafeFile,
    /// A file exceeds the caller's explicit bound.
    #[error("private state file exceeds its size limit")]
    FileTooLarge,
    /// One requested read exceeds the independent memory bound.
    #[error("private state read exceeds its memory limit")]
    ReadTooLarge,
    /// A positional read range cannot be represented.
    #[error("invalid private state read range")]
    InvalidReadRange,
    /// A complete-file read observed a changing file.
    #[error("private state file changed during read")]
    FileChanged,
    /// A bounded result buffer could not be reserved.
    #[error("private state read allocation failed")]
    AllocationFailed,
    /// Immutable creation found an incumbent and left it untouched.
    #[error("private state entry already exists")]
    AlreadyExists,
    /// A direct inventory entry was not a permitted private regular file or directory.
    #[error("unsafe private state directory entry")]
    UnsafeEntry,
    /// A complete inventory exceeded its caller-supplied entry or key-byte bound.
    #[error("private state directory inventory exceeds its limit")]
    InventoryLimit,
    /// A previously observed entry or the enumerated directory changed.
    #[error("private state directory changed during inventory")]
    InventoryChanged,
    /// Another cooperative writer holds the advisory lock.
    #[error("private state is locked")]
    Locked,
    /// A mutation guard belongs to another anchored directory.
    #[error("private state lock belongs to another directory")]
    WrongLock,
    /// The held lock is no longer the directory's current lock entry.
    #[error("private state lock entry was replaced")]
    LockReplaced,
    /// An opened file is no longer the directory's current entry.
    #[error("private state file entry was replaced")]
    EntryReplaced,
    /// Rename may have completed but required parent durability confirmation failed.
    #[error("private state promotion outcome is uncertain")]
    UncertainPromotion,
    /// A system call failed. No path or storage key is retained.
    #[error("private state I/O failed during {operation} ({kind:?}, errno {raw_os_error:?})")]
    Io {
        /// Fixed operation name from this module.
        operation: &'static str,
        /// Portable error category.
        kind: ErrorKind,
        /// Raw errno when supplied by the operating system.
        raw_os_error: Option<i32>,
    },
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl From<FileIdentity> for PrivateStateEntryIdentity {
    fn from(identity: FileIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
        }
    }
}

impl From<DirectoryIdentity> for PrivateStateEntryIdentity {
    fn from(identity: DirectoryIdentity) -> Self {
        Self {
            device: identity.device,
            inode: identity.inode,
        }
    }
}

/// A retained descriptor for one private, application-owned state directory.
///
/// Child operations stay relative to this descriptor. If the caller-provided
/// root is renamed or replaced later, this capability continues to address the
/// originally admitted directory rather than following the replacement.
pub struct PrivateStateDir {
    descriptor: std::os::fd::OwnedFd,
    identity: DirectoryIdentity,
}

impl PrivateStateDir {
    /// Anchors an existing final-component-symlink-free `0700` directory owned
    /// by the process effective user.
    ///
    /// This admits an existing root but does not create or sync the root's own
    /// directory entry. Its caller remains responsible for that lifecycle.
    pub fn open_root(path: &Path) -> Result<Self, StateDirError> {
        let descriptor = open(path, directory_open_flags(), Mode::empty())
            .map_err(|error| directory_open_error("open private state root", error))?;
        Self::from_descriptor(descriptor)
    }

    /// Opens one existing private child directory relative to this anchor.
    pub fn open_child(&self, key: &StateKey) -> Result<Self, StateDirError> {
        self.revalidate()?;
        let descriptor = openat(
            &self.descriptor,
            key.as_str(),
            directory_open_flags(),
            Mode::empty(),
        )
        .map_err(|error| directory_open_error("open private state child", error))?;
        Self::from_descriptor(descriptor)
    }

    /// Opens or durably ensures one private child directory relative to this
    /// anchor. An unsafe incumbent is rejected without changing its mode. Every
    /// successful ensure syncs both the admitted child and its parent, including
    /// when another cooperative creator won the creation race.
    pub fn open_or_create_child(&self, key: &StateKey) -> Result<Self, StateDirError> {
        self.open_or_create_child_with_sync(key, |child, parent| {
            fsync(&child.descriptor)
                .map_err(|error| io_error("sync private state child", error))?;
            fsync(&parent.descriptor).map_err(|error| io_error("sync private state parent", error))
        })
    }

    fn open_or_create_child_with_sync(
        &self,
        key: &StateKey,
        sync: impl FnOnce(&Self, &Self) -> Result<(), StateDirError>,
    ) -> Result<Self, StateDirError> {
        self.revalidate()?;
        let child = match self.open_child_descriptor(key) {
            Ok(descriptor) => Self::from_descriptor(descriptor),
            Err(error) if error == rustix::io::Errno::NOENT => {
                match mkdirat(
                    &self.descriptor,
                    key.as_str(),
                    Mode::from_raw_mode(PRIVATE_DIRECTORY_MODE),
                ) {
                    Ok(()) => {}
                    Err(error) if error == rustix::io::Errno::EXIST => {}
                    Err(error) => return Err(io_error("create private state child", error)),
                }
                let descriptor = self.open_child_descriptor(key).map_err(|error| {
                    directory_open_error("open created private state child", error)
                })?;
                Self::from_descriptor(descriptor)
            }
            Err(error) => Err(directory_open_error("open private state child", error)),
        }?;
        sync(&child, self)?;
        Ok(child)
    }

    /// Opens and validates one existing private regular file for bounded reads
    /// and future lock-guarded append or tail recovery.
    pub fn open_file(
        &self,
        key: &StateKey,
        maximum_bytes: u64,
    ) -> Result<PrivateStateFile, StateDirError> {
        self.revalidate()?;
        let file = self.open_existing_file(key)?;
        PrivateStateFile::new(file, &self.descriptor, self.identity, key, maximum_bytes)
    }

    /// Creates one immutable-named private file without replacing an incumbent.
    ///
    /// The lock must belong to this directory. Success means file and parent
    /// directory are synced. Any error after exclusive creation is reported and
    /// leaves that uncertain incumbent in place for explicit recovery; a later
    /// call returns [`StateDirError::AlreadyExists`] rather than reusing it.
    pub fn create_new_file(
        &self,
        lock: &PrivateStateLock,
        key: &StateKey,
        contents: &[u8],
        maximum_bytes: u64,
    ) -> Result<PrivateStateFile, StateDirError> {
        self.require_lock(lock)?;
        if u64::try_from(contents.len()).map_or(true, |length| length > maximum_bytes) {
            return Err(StateDirError::FileTooLarge);
        }
        let descriptor = openat(
            &self.descriptor,
            key.as_str(),
            file_open_flags() | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(PRIVATE_FILE_MODE),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                StateDirError::AlreadyExists
            } else {
                file_open_error("create private state file", error)
            }
        })?;
        fchmod(&descriptor, Mode::from_raw_mode(PRIVATE_FILE_MODE))
            .map_err(|error| io_error("protect private state file", error))?;
        let mut file = File::from(descriptor);
        validate_file(&file)?;
        file.write_all(contents)
            .map_err(|error| std_io_error("write private state file", error))?;
        file.sync_all()
            .map_err(|error| std_io_error("sync private state file", error))?;
        fsync(&self.descriptor).map_err(|error| io_error("sync private state parent", error))?;
        PrivateStateFile::new(file, &self.descriptor, self.identity, key, maximum_bytes)
    }

    /// Takes a nonblocking exclusive advisory lock on a dedicated private file.
    ///
    /// The guard owns its independently opened descriptor. A second open in the
    /// same process and a separate process both contend under `fs2`'s supported
    /// Unix `flock` behavior. This is cooperative serialization, not a defense
    /// against another same-user process that ignores the lock.
    pub fn try_lock(&self) -> Result<PrivateStateLock, StateDirError> {
        self.revalidate()?;
        let key = StateKey(WRITER_LOCK_KEY.to_owned());
        let file = match self.open_existing_file_raw(&key) {
            Ok(file) => file,
            Err(error) if error == rustix::io::Errno::NOENT => match self.create_lock_file(&key) {
                Ok(file) => file,
                Err(StateDirError::AlreadyExists) => self.open_existing_file(&key)?,
                Err(error) => return Err(error),
            },
            Err(error) => return Err(file_open_error("open private state lock", error)),
        };
        validate_lock_file(&file)?;
        file.try_lock_exclusive().map_err(|error| {
            if error.kind() == fs2::lock_contended_error().kind() {
                StateDirError::Locked
            } else {
                std_io_error("lock private state", error)
            }
        })?;
        let file_identity = validate_lock_file(&file)?;
        let directory_descriptor = rustix::io::fcntl_dupfd_cloexec(&self.descriptor, 3)
            .map_err(|error| io_error("retain private state lock directory", error))?;
        Ok(PrivateStateLock {
            file,
            directory: self.identity,
            directory_descriptor,
            key: WRITER_LOCK_KEY,
            file_identity,
        })
    }

    /// Syncs this anchored directory while holding its current writer lock.
    ///
    /// Future recovery paths use this after opening an existing entry whose
    /// directory insertion may have survived without a prior parent sync.
    pub fn sync(&self, lock: &PrivateStateLock) -> Result<(), StateDirError> {
        self.require_lock(lock)?;
        fsync(&self.descriptor).map_err(|error| io_error("sync private state directory", error))
    }

    /// Returns a complete bounded direct-entry inventory under this writer lock.
    ///
    /// The iterator and every returned entry are descriptor-relative and
    /// revalidated. This is best-effort under the supplied cooperative exclusive
    /// lock, not an atomic filesystem snapshot and not a recursive walk.
    pub fn inventory(
        &self,
        lock: &PrivateStateLock,
        maximum_entries: usize,
        maximum_key_bytes: u64,
    ) -> Result<PrivateStateInventory, StateDirError> {
        self.inventory_with_check(lock, maximum_entries, maximum_key_bytes, || {})
    }

    fn inventory_with_check(
        &self,
        lock: &PrivateStateLock,
        maximum_entries: usize,
        maximum_key_bytes: u64,
        before_revalidation: impl FnOnce(),
    ) -> Result<PrivateStateInventory, StateDirError> {
        self.require_lock(lock)?;
        let initial_directory = fstat(&self.descriptor)
            .map_err(|error| io_error("inspect inventory directory", error))?;
        let mut entries = Vec::new();
        let mut total_key_bytes = 0_u64;
        let mut total_file_bytes = 0_u64;
        let mut reader = Dir::read_from(&self.descriptor)
            .map_err(|error| io_error("read private state directory", error))?;
        for result in &mut reader {
            let entry =
                result.map_err(|error| io_error("read private state directory entry", error))?;
            let raw_name = entry.file_name().to_bytes();
            if matches!(raw_name, b"." | b"..") {
                continue;
            }
            let spelling = std::str::from_utf8(raw_name).map_err(|_| StateDirError::UnsafeEntry)?;
            let key_bytes =
                u64::try_from(raw_name.len()).map_err(|_| StateDirError::InventoryLimit)?;
            if entries.len() == maximum_entries
                || total_key_bytes
                    .checked_add(key_bytes)
                    .is_none_or(|total| total > maximum_key_bytes)
            {
                return Err(StateDirError::InventoryLimit);
            }

            let stat = statat(&self.descriptor, spelling, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|error| io_error("inspect private state directory entry", error))?;
            let identity = PrivateStateEntryIdentity {
                device: stat.st_dev as u64,
                inode: stat.st_ino as u64,
            };
            let (name, kind, byte_length) = match FileType::from_raw_mode(stat.st_mode) {
                FileType::RegularFile => {
                    let file = openat(&self.descriptor, spelling, file_open_flags(), Mode::empty())
                        .map(File::from)
                        .map_err(|error| {
                            file_open_error("open private state inventory file", error)
                        })?;
                    let current = if spelling == WRITER_LOCK_KEY {
                        validate_lock_file(&file)?
                    } else {
                        validate_file(&file)?
                    };
                    if PrivateStateEntryIdentity::from(current) != identity {
                        return Err(StateDirError::EntryReplaced);
                    }
                    let name = if spelling == WRITER_LOCK_KEY {
                        PrivateStateInventoryName::WriterLock
                    } else {
                        PrivateStateInventoryName::State(
                            StateKey::new(spelling).map_err(|_| StateDirError::UnsafeEntry)?,
                        )
                    };
                    let kind = if spelling == WRITER_LOCK_KEY {
                        PrivateStateEntryKind::WriterLock
                    } else {
                        PrivateStateEntryKind::RegularFile
                    };
                    let length =
                        u64::try_from(stat.st_size).map_err(|_| StateDirError::UnsafeEntry)?;
                    (name, kind, length)
                }
                FileType::Directory => {
                    if spelling == WRITER_LOCK_KEY {
                        return Err(StateDirError::UnsafeEntry);
                    }
                    let directory = openat(
                        &self.descriptor,
                        spelling,
                        directory_open_flags(),
                        Mode::empty(),
                    )
                    .map_err(|error| {
                        directory_open_error("open private state inventory directory", error)
                    })?;
                    let current = validate_directory(&directory)?;
                    if PrivateStateEntryIdentity::from(current) != identity {
                        return Err(StateDirError::EntryReplaced);
                    }
                    let length =
                        u64::try_from(stat.st_size).map_err(|_| StateDirError::UnsafeEntry)?;
                    (
                        PrivateStateInventoryName::State(
                            StateKey::new(spelling).map_err(|_| StateDirError::UnsafeEntry)?,
                        ),
                        PrivateStateEntryKind::Directory,
                        length,
                    )
                }
                _ => return Err(StateDirError::UnsafeEntry),
            };
            entries
                .try_reserve(1)
                .map_err(|_| StateDirError::AllocationFailed)?;
            total_key_bytes = total_key_bytes
                .checked_add(key_bytes)
                .ok_or(StateDirError::InventoryLimit)?;
            if matches!(
                kind,
                PrivateStateEntryKind::RegularFile | PrivateStateEntryKind::WriterLock
            ) {
                total_file_bytes = total_file_bytes
                    .checked_add(byte_length)
                    .ok_or(StateDirError::InventoryLimit)?;
            }
            entries.push(PrivateStateInventoryEntry {
                name,
                identity,
                kind,
                byte_length,
            });
        }
        before_revalidation();
        for entry in &entries {
            self.revalidate_inventory_entry(entry)?;
        }
        let final_directory = fstat(&self.descriptor)
            .map_err(|error| io_error("reinspect inventory directory", error))?;
        if initial_directory.st_size != final_directory.st_size
            || initial_directory.st_mtime != final_directory.st_mtime
            || initial_directory.st_mtime_nsec != final_directory.st_mtime_nsec
            || initial_directory.st_ctime != final_directory.st_ctime
            || initial_directory.st_ctime_nsec != final_directory.st_ctime_nsec
        {
            return Err(StateDirError::InventoryChanged);
        }
        self.require_lock(lock)?;
        Ok(PrivateStateInventory {
            entries,
            total_key_bytes,
            total_file_bytes,
        })
    }

    fn revalidate_inventory_entry(
        &self,
        entry: &PrivateStateInventoryEntry,
    ) -> Result<(), StateDirError> {
        let spelling = match entry.name() {
            PrivateStateInventoryName::State(key) => key.as_str(),
            PrivateStateInventoryName::WriterLock => WRITER_LOCK_KEY,
        };
        let (identity, byte_length) = match entry.kind() {
            PrivateStateEntryKind::RegularFile | PrivateStateEntryKind::WriterLock => {
                let file = openat(&self.descriptor, spelling, file_open_flags(), Mode::empty())
                    .map(File::from)
                    .map_err(|error| file_open_error("reopen private inventory file", error))?;
                let identity = if entry.kind() == PrivateStateEntryKind::WriterLock {
                    validate_lock_file(&file)?
                } else {
                    validate_file(&file)?
                };
                let length = file
                    .metadata()
                    .map_err(|error| std_io_error("reinspect private inventory file", error))?
                    .len();
                (PrivateStateEntryIdentity::from(identity), length)
            }
            PrivateStateEntryKind::Directory => {
                let descriptor = openat(
                    &self.descriptor,
                    spelling,
                    directory_open_flags(),
                    Mode::empty(),
                )
                .map_err(|error| {
                    directory_open_error("reopen private inventory directory", error)
                })?;
                let identity = validate_directory(&descriptor)?;
                let stat = fstat(&descriptor)
                    .map_err(|error| io_error("reinspect private inventory child", error))?;
                let length = u64::try_from(stat.st_size).map_err(|_| StateDirError::UnsafeEntry)?;
                (PrivateStateEntryIdentity::from(identity), length)
            }
        };
        if identity != entry.identity() || byte_length != entry.byte_length() {
            return Err(StateDirError::InventoryChanged);
        }
        Ok(())
    }

    /// Atomically promotes one current private source file without replacement.
    ///
    /// The source and destination locks must each match their own anchored
    /// directories. On an existing safe destination both capabilities are
    /// returned unchanged. A post-rename failure is deliberately uncertain:
    /// callers must re-open and re-inventory rather than treating it as durable.
    pub fn promote_new_file(
        &self,
        destination_lock: &PrivateStateLock,
        source_lock: &PrivateStateLock,
        source: PrivateStateFile,
        destination: &StateKey,
    ) -> Result<PrivateStatePromotion, StateDirError> {
        self.promote_new_file_with_sync(
            destination_lock,
            source_lock,
            source,
            destination,
            |source_directory, destination_directory| {
                // Make the new name durable before committing removal of the
                // old name when the directories have independent sync order.
                fsync(destination_directory).map_err(|error| {
                    io_error("sync promoted private state destination parent", error)
                })?;
                fsync(source_directory)
                    .map_err(|error| io_error("sync promoted private state source parent", error))
            },
        )
    }

    fn promote_new_file_with_sync(
        &self,
        destination_lock: &PrivateStateLock,
        source_lock: &PrivateStateLock,
        source: PrivateStateFile,
        destination: &StateKey,
        sync_parents: impl FnOnce(
            &std::os::fd::OwnedFd,
            &std::os::fd::OwnedFd,
        ) -> Result<(), StateDirError>,
    ) -> Result<PrivateStatePromotion, StateDirError> {
        self.require_lock(destination_lock)?;
        source.require_lock(source_lock)?;
        if source.directory == self.identity && source.key == destination.as_str() {
            let incumbent = self.open_file(destination, source.maximum_bytes)?;
            return Ok(PrivateStatePromotion::Existing {
                source,
                destination: incumbent,
            });
        }
        source.sync_all(source_lock)?;
        self.require_lock(destination_lock)?;
        source.require_lock(source_lock)?;
        match renameat_with(
            &source.directory_descriptor,
            source.key.as_str(),
            &self.descriptor,
            destination.as_str(),
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(error) if error == rustix::io::Errno::EXIST => {
                let incumbent = self.open_file(destination, source.maximum_bytes)?;
                return Ok(PrivateStatePromotion::Existing {
                    source,
                    destination: incumbent,
                });
            }
            Err(error) => {
                return Err(io_error(
                    "promote private state file without replacement",
                    error,
                ));
            }
        }

        if source
            .require_parent_lock_after_promotion(source_lock)
            .and_then(|()| self.require_lock(destination_lock))
            .and_then(|()| sync_parents(&source.directory_descriptor, &self.descriptor))
            .is_err()
        {
            return Err(StateDirError::UncertainPromotion);
        }
        self.require_lock(destination_lock)
            .map_err(|_| StateDirError::UncertainPromotion)?;
        let promoted = self
            .open_file(destination, source.maximum_bytes)
            .map_err(|_| StateDirError::UncertainPromotion)?;
        if promoted.file_identity != source.file_identity {
            return Err(StateDirError::UncertainPromotion);
        }
        Ok(PrivateStatePromotion::Promoted(promoted))
    }

    fn from_descriptor(descriptor: std::os::fd::OwnedFd) -> Result<Self, StateDirError> {
        let identity = validate_directory(&descriptor)?;
        Ok(Self {
            descriptor,
            identity,
        })
    }

    fn revalidate(&self) -> Result<(), StateDirError> {
        let current = validate_directory(&self.descriptor)?;
        if current != self.identity {
            return Err(StateDirError::UnsafeDirectory);
        }
        Ok(())
    }

    fn open_child_descriptor(
        &self,
        key: &StateKey,
    ) -> Result<std::os::fd::OwnedFd, rustix::io::Errno> {
        openat(
            &self.descriptor,
            key.as_str(),
            directory_open_flags(),
            Mode::empty(),
        )
    }

    fn open_existing_file(&self, key: &StateKey) -> Result<File, StateDirError> {
        let file = self
            .open_existing_file_raw(key)
            .map_err(|error| file_open_error("open private state file", error))?;
        validate_file(&file)?;
        Ok(file)
    }

    fn open_existing_file_raw(&self, key: &StateKey) -> Result<File, rustix::io::Errno> {
        openat(
            &self.descriptor,
            key.as_str(),
            file_open_flags(),
            Mode::empty(),
        )
        .map(File::from)
    }

    fn create_lock_file(&self, key: &StateKey) -> Result<File, StateDirError> {
        let descriptor = openat(
            &self.descriptor,
            key.as_str(),
            file_open_flags() | OFlags::CREATE | OFlags::EXCL,
            Mode::from_raw_mode(PRIVATE_FILE_MODE),
        )
        .map_err(|error| {
            if error == rustix::io::Errno::EXIST {
                StateDirError::AlreadyExists
            } else {
                file_open_error("create private state lock", error)
            }
        })?;
        fchmod(&descriptor, Mode::from_raw_mode(PRIVATE_FILE_MODE))
            .map_err(|error| io_error("protect private state lock", error))?;
        let file = File::from(descriptor);
        validate_file(&file)?;
        file.sync_all()
            .map_err(|error| std_io_error("sync private state lock", error))?;
        fsync(&self.descriptor).map_err(|error| io_error("sync private state parent", error))?;
        Ok(file)
    }

    fn require_lock(&self, lock: &PrivateStateLock) -> Result<(), StateDirError> {
        self.revalidate()?;
        if lock.directory != self.identity {
            return Err(StateDirError::WrongLock);
        }
        lock.validate()
    }
}

/// A bounded private regular-file capability tied to its anchored directory.
pub struct PrivateStateFile {
    file: File,
    directory: DirectoryIdentity,
    directory_descriptor: std::os::fd::OwnedFd,
    key: String,
    file_identity: FileIdentity,
    maximum_bytes: u64,
}

impl PrivateStateFile {
    fn new(
        file: File,
        directory_descriptor: &std::os::fd::OwnedFd,
        directory: DirectoryIdentity,
        key: &StateKey,
        maximum_bytes: u64,
    ) -> Result<Self, StateDirError> {
        let file_identity = validate_file(&file)?;
        let directory_descriptor = rustix::io::fcntl_dupfd_cloexec(directory_descriptor, 3)
            .map_err(|error| io_error("retain private state file directory", error))?;
        let value = Self {
            file,
            directory,
            directory_descriptor,
            key: key.as_str().to_owned(),
            file_identity,
            maximum_bytes,
        };
        value.len()?;
        Ok(value)
    }

    /// Returns the current validated length when it is within the bound.
    pub fn len(&self) -> Result<u64, StateDirError> {
        self.validate_current_entry()?;
        let length = self
            .file
            .metadata()
            .map_err(|error| std_io_error("inspect private state file", error))?
            .len();
        if length > self.maximum_bytes {
            return Err(StateDirError::FileTooLarge);
        }
        Ok(length)
    }

    /// Returns whether the current validated file is empty.
    pub fn is_empty(&self) -> Result<bool, StateDirError> {
        self.len().map(|length| length == 0)
    }

    /// Reads the complete file when it also fits the independent 16 MiB memory
    /// bound. Larger files remain available through [`Self::read_range`].
    pub fn read_all(&self) -> Result<Vec<u8>, StateDirError> {
        let length = self.len()?;
        if length > MAX_PRIVATE_STATE_READ_BYTES {
            return Err(StateDirError::ReadTooLarge);
        }
        let bytes = self.read_range(0, length)?;
        if bytes.len() as u64 != length || self.len()? != length {
            return Err(StateDirError::FileChanged);
        }
        Ok(bytes)
    }

    /// Reads at most `maximum_read_bytes` from `offset` without changing the
    /// descriptor cursor.
    ///
    /// A request beyond EOF returns an empty vector. The request must be at most
    /// 16 MiB and `offset + maximum_read_bytes` must be representable even when
    /// the current file is shorter. File growth beyond the file's disk bound is
    /// rejected before returning.
    pub fn read_range(
        &self,
        offset: u64,
        maximum_read_bytes: u64,
    ) -> Result<Vec<u8>, StateDirError> {
        if maximum_read_bytes > MAX_PRIVATE_STATE_READ_BYTES {
            return Err(StateDirError::ReadTooLarge);
        }
        offset
            .checked_add(maximum_read_bytes)
            .ok_or(StateDirError::InvalidReadRange)?;
        let file_length = self.len()?;
        let read_length = file_length.saturating_sub(offset).min(maximum_read_bytes);
        let capacity = usize::try_from(read_length).map_err(|_| StateDirError::ReadTooLarge)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| StateDirError::AllocationFailed)?;
        let mut position = offset;
        let mut buffer = [0_u8; 8 * 1_024];
        while bytes.len() < capacity {
            let requested = buffer.len().min(capacity - bytes.len());
            let count = loop {
                match self.file.read_at(&mut buffer[..requested], position) {
                    Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                    result => break result,
                }
            }
            .map_err(|error| std_io_error("read private state file", error))?;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            position = position
                .checked_add(count as u64)
                .ok_or(StateDirError::InvalidReadRange)?;
        }
        self.len()?;
        Ok(bytes)
    }

    /// Appends bytes after validating the matching directory lock and bound.
    /// A write error may represent a partial append; callers must recover their
    /// higher-level framing before retrying.
    pub fn append(&mut self, lock: &PrivateStateLock, bytes: &[u8]) -> Result<(), StateDirError> {
        self.require_lock(lock)?;
        let current = self.len()?;
        let additional = u64::try_from(bytes.len()).map_err(|_| StateDirError::FileTooLarge)?;
        if current
            .checked_add(additional)
            .is_none_or(|length| length > self.maximum_bytes)
        {
            return Err(StateDirError::FileTooLarge);
        }
        self.file
            .write_all(bytes)
            .map_err(|error| std_io_error("append private state file", error))?;
        self.len().map(|_| ())
    }

    /// Truncates only an existing tail, under the matching directory lock.
    pub fn truncate_tail(
        &self,
        lock: &PrivateStateLock,
        new_length: u64,
    ) -> Result<(), StateDirError> {
        self.require_lock(lock)?;
        let current = self.len()?;
        if new_length > current {
            return Err(StateDirError::FileTooLarge);
        }
        rustix::fs::ftruncate(&self.file, new_length)
            .map_err(|error| io_error("truncate private state file tail", error))
    }

    /// Flushes file contents and metadata to stable storage.
    pub fn sync_all(&self, lock: &PrivateStateLock) -> Result<(), StateDirError> {
        self.require_lock(lock)?;
        self.file
            .sync_all()
            .map_err(|error| std_io_error("sync private state file", error))
    }

    fn require_lock(&self, lock: &PrivateStateLock) -> Result<(), StateDirError> {
        if lock.directory != self.directory {
            return Err(StateDirError::WrongLock);
        }
        lock.validate()?;
        self.validate_current_entry()
    }

    fn require_parent_lock_after_promotion(
        &self,
        lock: &PrivateStateLock,
    ) -> Result<(), StateDirError> {
        if lock.directory != self.directory {
            return Err(StateDirError::WrongLock);
        }
        lock.validate()?;
        let directory = validate_directory(&self.directory_descriptor)?;
        if directory != self.directory {
            return Err(StateDirError::EntryReplaced);
        }
        Ok(())
    }

    fn validate_current_entry(&self) -> Result<(), StateDirError> {
        let directory = validate_directory(&self.directory_descriptor)?;
        if directory != self.directory {
            return Err(StateDirError::EntryReplaced);
        }
        let held_identity = validate_file(&self.file)?;
        if held_identity != self.file_identity {
            return Err(StateDirError::EntryReplaced);
        }
        let current = openat(
            &self.directory_descriptor,
            self.key.as_str(),
            file_open_flags(),
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|error| {
            if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR) {
                StateDirError::UnsafeFile
            } else if error == rustix::io::Errno::NOENT {
                StateDirError::EntryReplaced
            } else {
                io_error("reopen private state file entry", error)
            }
        })?;
        let current_identity = validate_file(&current)?;
        if current_identity != self.file_identity {
            return Err(StateDirError::EntryReplaced);
        }
        Ok(())
    }
}

/// Lifetime capability proving a cooperative writer holds one folder lock.
pub struct PrivateStateLock {
    file: File,
    directory: DirectoryIdentity,
    directory_descriptor: std::os::fd::OwnedFd,
    key: &'static str,
    file_identity: FileIdentity,
}

impl PrivateStateLock {
    /// Verifies that the held lock is still the anchored directory's current,
    /// safe lock entry.
    ///
    /// This detects observed unlink or replacement races before a cooperative
    /// transaction. It does not make advisory locking protect against a hostile
    /// same-user process that ignores the lock or races after this check.
    pub fn validate(&self) -> Result<(), StateDirError> {
        let directory = validate_directory(&self.directory_descriptor)
            .map_err(|_| StateDirError::LockReplaced)?;
        if directory != self.directory || validate_lock_file(&self.file) != Ok(self.file_identity) {
            return Err(StateDirError::LockReplaced);
        }
        let current = openat(
            &self.directory_descriptor,
            self.key,
            file_open_flags(),
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| StateDirError::LockReplaced)?;
        if validate_lock_file(&current) != Ok(self.file_identity) {
            return Err(StateDirError::LockReplaced);
        }
        Ok(())
    }
}

impl Drop for PrivateStateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn directory_open_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
}

fn file_open_flags() -> OFlags {
    OFlags::RDWR
        | OFlags::APPEND
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | OFlags::NONBLOCK
        | OFlags::NOCTTY
}

fn validate_directory(
    descriptor: &std::os::fd::OwnedFd,
) -> Result<DirectoryIdentity, StateDirError> {
    let stat =
        fstat(descriptor).map_err(|error| io_error("inspect private state directory", error))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != PRIVATE_DIRECTORY_MODE
    {
        return Err(StateDirError::UnsafeDirectory);
    }
    Ok(DirectoryIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    })
}

fn validate_file(file: &File) -> Result<FileIdentity, StateDirError> {
    let stat = fstat(file).map_err(|error| io_error("inspect private state file", error))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_uid != rustix::process::geteuid().as_raw()
        || stat.st_mode & 0o7777 != PRIVATE_FILE_MODE
        || stat.st_nlink != 1
    {
        return Err(StateDirError::UnsafeFile);
    }
    Ok(FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    })
}

fn validate_lock_file(file: &File) -> Result<FileIdentity, StateDirError> {
    let identity = validate_file(file)?;
    if file
        .metadata()
        .map_err(|error| std_io_error("inspect private state lock", error))?
        .len()
        != 0
    {
        return Err(StateDirError::UnsafeFile);
    }
    Ok(identity)
}

fn directory_open_error(operation: &'static str, error: rustix::io::Errno) -> StateDirError {
    if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
        StateDirError::UnsafeDirectory
    } else {
        io_error(operation, error)
    }
}

fn file_open_error(operation: &'static str, error: rustix::io::Errno) -> StateDirError {
    if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR) {
        StateDirError::UnsafeFile
    } else {
        io_error(operation, error)
    }
}

fn io_error(operation: &'static str, error: rustix::io::Errno) -> StateDirError {
    let error = std::io::Error::from_raw_os_error(error.raw_os_error());
    std_io_error(operation, error)
}

fn std_io_error(operation: &'static str, error: std::io::Error) -> StateDirError {
    StateDirError::Io {
        operation,
        kind: error.kind(),
        raw_os_error: error.raw_os_error(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};
    use std::process::{Command, ExitStatus};
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;

    const CHILD_ROOT_ENV: &str = "COVALENT_STATE_DIR_CHILD_ROOT";
    const LOCK_CHILD_TEST: &str = "sync::state_dir::tests::lock_probe_child";
    const FIFO_CHILD_TEST: &str = "sync::state_dir::tests::fifo_probe_child";

    fn key(value: &str) -> StateKey {
        StateKey::new(value).expect("valid test state key")
    }

    fn private_root() -> (TempDir, std::path::PathBuf, PrivateStateDir) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let path = temporary.path().join("state");
        fs::create_dir(&path).expect("create state root");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("protect state root");
        let state = PrivateStateDir::open_root(&path).expect("open state root");
        (temporary, path, state)
    }

    fn spawn_probe(test_name: &str, root: &Path, expect_unlocked: bool) -> std::process::Child {
        let mut command = Command::new(std::env::current_exe().expect("current test binary"));
        command
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_ROOT_ENV, root)
            .env_remove("COVALENT_EXPECT_UNLOCKED");
        if expect_unlocked {
            command.env("COVALENT_EXPECT_UNLOCKED", "1");
        }
        command.spawn().expect("spawn probe test")
    }

    fn wait_bounded(mut child: std::process::Child) -> ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().expect("poll probe") {
                return status;
            }
            if Instant::now() >= deadline {
                child.kill().expect("kill stuck probe");
                let _ = child.wait();
                panic!("private state probe exceeded its deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn storage_keys_reject_paths_and_noncanonical_ascii() {
        for invalid in [
            "",
            ".",
            "..",
            ".hidden",
            "Upper",
            "two/parts",
            "two\\parts",
            "é",
            "writer.lock",
        ] {
            assert!(matches!(
                StateKey::new(invalid),
                Err(StateDirError::InvalidKey)
            ));
        }
        assert!(matches!(
            StateKey::new(&"a".repeat(MAX_STORAGE_KEY_BYTES + 1)),
            Err(StateDirError::InvalidKey)
        ));
        StateKey::new("550e8400-e29b-41d4-a716-446655440000").expect("UUID key");
        StateKey::new("binding.v1").expect("fixed binding key");
    }

    #[test]
    fn root_and_existing_entries_must_already_be_private() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("state");
        fs::create_dir(&root).expect("create root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).expect("broaden root");
        assert!(matches!(
            PrivateStateDir::open_root(&root),
            Err(StateDirError::UnsafeDirectory)
        ));

        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("protect root");
        let state = PrivateStateDir::open_root(&root).expect("open root");
        let child = root.join("existing");
        fs::create_dir(&child).expect("create child");
        fs::set_permissions(&child, fs::Permissions::from_mode(0o755)).expect("broaden child");
        assert!(matches!(
            state.open_child(&key("existing")),
            Err(StateDirError::UnsafeDirectory)
        ));

        let file = root.join("record.v1");
        fs::write(&file, b"record").expect("write file");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("broaden file");
        assert!(matches!(
            state.open_file(&key("record.v1"), 64),
            Err(StateDirError::UnsafeFile)
        ));
        assert_eq!(
            fs::metadata(&file).expect("metadata").permissions().mode() & 0o777,
            0o644,
            "opening an unsafe incumbent must not chmod it"
        );
    }

    #[test]
    fn nofollow_and_single_link_checks_reject_aliases() {
        let (temporary, root, state) = private_root();
        symlink(&root, temporary.path().join("rootlink")).expect("link root");
        assert!(matches!(
            PrivateStateDir::open_root(&temporary.path().join("rootlink")),
            Err(StateDirError::UnsafeDirectory)
        ));
        let real_child = root.join("realchild");
        fs::create_dir(&real_child).expect("create child");
        fs::set_permissions(&real_child, fs::Permissions::from_mode(0o700)).expect("protect child");
        symlink(&real_child, root.join("childlink")).expect("link child");
        assert!(matches!(
            state.open_child(&key("childlink")),
            Err(StateDirError::UnsafeDirectory)
        ));

        let target = root.join("target.v1");
        fs::write(&target, b"target").expect("write target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("protect target");
        symlink(&target, root.join("filelink.v1")).expect("link file");
        assert!(matches!(
            state.open_file(&key("filelink.v1"), 64),
            Err(StateDirError::UnsafeFile)
        ));

        fs::hard_link(&target, root.join("hardlink.v1")).expect("hard link");
        assert!(matches!(
            state.open_file(&key("hardlink.v1"), 64),
            Err(StateDirError::UnsafeFile)
        ));
        assert!(matches!(
            state.open_file(&key("target.v1"), 64),
            Err(StateDirError::UnsafeFile)
        ));
    }

    #[test]
    fn descriptor_anchor_does_not_follow_replaced_root() {
        let (_temporary, root, state) = private_root();
        let moved = root.with_file_name("moved-state");
        fs::rename(&root, &moved).expect("rename admitted root");
        fs::create_dir(&root).expect("create replacement root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("protect replacement");

        let lock = state.try_lock().expect("lock old root");
        state
            .create_new_file(&lock, &key("binding.v1"), b"anchored", 64)
            .expect("create through retained descriptor");
        assert_eq!(
            fs::read(moved.join("binding.v1")).expect("old root file"),
            b"anchored"
        );
        assert!(!root.join("binding.v1").exists());
    }

    #[test]
    fn durable_child_ensure_accepts_an_existing_private_directory() {
        let (_temporary, _root, state) = private_root();
        let first = state
            .open_or_create_child(&key("folder-state"))
            .expect("create child");
        let second = state
            .open_or_create_child(&key("folder-state"))
            .expect("durably ensure existing child");
        assert!(first.identity == second.identity);
    }

    #[test]
    fn durable_child_ensure_syncs_an_existing_private_directory() {
        let (_temporary, _root, state) = private_root();
        state
            .open_or_create_child(&key("folder-state"))
            .expect("create child");

        let result =
            state.open_or_create_child_with_sync(&key("folder-state"), |_child, _parent| {
                Err(StateDirError::Io {
                    operation: "test existing-child sync",
                    kind: ErrorKind::Other,
                    raw_os_error: None,
                })
            });
        assert!(matches!(
            result,
            Err(StateDirError::Io {
                operation: "test existing-child sync",
                ..
            })
        ));
    }

    #[test]
    fn create_new_collision_preserves_incumbent() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        state
            .create_new_file(&lock, &key("binding.v1"), b"first", 64)
            .expect("first create");
        assert!(matches!(
            state.create_new_file(&lock, &key("binding.v1"), b"second", 64),
            Err(StateDirError::AlreadyExists)
        ));
        assert_eq!(
            fs::read(root.join("binding.v1")).expect("incumbent"),
            b"first"
        );
    }

    #[test]
    fn reads_and_appends_enforce_growth_bound() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        let mut file = state
            .create_new_file(&lock, &key("operations.v1"), b"abc", 4)
            .expect("create bounded file");
        assert_eq!(file.read_all().expect("bounded read"), b"abc");
        assert!(matches!(
            file.append(&lock, b"de"),
            Err(StateDirError::FileTooLarge)
        ));
        assert_eq!(
            fs::read(root.join("operations.v1")).expect("unchanged"),
            b"abc"
        );

        let mut external = fs::OpenOptions::new()
            .append(true)
            .open(root.join("operations.v1"))
            .expect("external append");
        external.write_all(b"de").expect("grow file");
        assert!(matches!(file.len(), Err(StateDirError::FileTooLarge)));
        assert!(matches!(file.read_all(), Err(StateDirError::FileTooLarge)));
    }

    #[test]
    fn replaced_file_entry_invalidates_retained_capability() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        let mut old = state
            .create_new_file(&lock, &key("operations.v1"), b"old", 64)
            .expect("create original");
        fs::rename(root.join("operations.v1"), root.join("retired.v1")).expect("rename original");
        let replacement = state
            .create_new_file(&lock, &key("operations.v1"), b"new", 64)
            .expect("create replacement");

        assert!(matches!(
            old.append(&lock, b"-append"),
            Err(StateDirError::EntryReplaced)
        ));
        assert!(matches!(
            old.sync_all(&lock),
            Err(StateDirError::EntryReplaced)
        ));
        assert!(matches!(old.read_all(), Err(StateDirError::EntryReplaced)));
        assert_eq!(replacement.read_all().expect("replacement"), b"new");
        assert_eq!(
            fs::read(root.join("operations.v1")).expect("current"),
            b"new"
        );
        assert_eq!(fs::read(root.join("retired.v1")).expect("retired"), b"old");
    }

    #[test]
    fn complete_read_rejects_sparse_file_above_memory_cap() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        let file = state
            .create_new_file(
                &lock,
                &key("large.v1"),
                b"",
                MAX_PRIVATE_STATE_READ_BYTES + 1,
            )
            .expect("create sparse file");
        fs::OpenOptions::new()
            .write(true)
            .open(root.join("large.v1"))
            .expect("open sparse file")
            .set_len(MAX_PRIVATE_STATE_READ_BYTES + 1)
            .expect("grow sparse file");
        assert!(matches!(file.read_all(), Err(StateDirError::ReadTooLarge)));
    }

    #[test]
    fn positional_reads_are_bounded_and_check_range_arithmetic() {
        let (_temporary, _root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        let file = state
            .create_new_file(&lock, &key("operations.v1"), b"abcdef", 64)
            .expect("create file");
        assert_eq!(file.read_range(2, 3).expect("middle range"), b"cde");
        assert_eq!(file.read_range(5, 1).expect("last byte"), b"f");
        assert!(file.read_range(6, 4).expect("EOF range").is_empty());
        assert!(matches!(
            file.read_range(u64::MAX, 1),
            Err(StateDirError::InvalidReadRange)
        ));
        assert!(matches!(
            file.read_range(0, MAX_PRIVATE_STATE_READ_BYTES + 1),
            Err(StateDirError::ReadTooLarge)
        ));
    }

    #[test]
    fn wrong_directory_lock_cannot_mutate_file() {
        let (_temporary, _root, state) = private_root();
        let left = state
            .open_or_create_child(&key("left"))
            .expect("left child");
        let right = state
            .open_or_create_child(&key("right"))
            .expect("right child");
        let left_lock = left.try_lock().expect("left lock");
        let right_lock = right.try_lock().expect("right lock");
        let mut file = left
            .create_new_file(&left_lock, &key("operations.v1"), b"a", 16)
            .expect("left file");
        assert!(matches!(
            file.append(&right_lock, b"b"),
            Err(StateDirError::WrongLock)
        ));
        assert!(matches!(
            left.sync(&right_lock),
            Err(StateDirError::WrongLock)
        ));
        left.sync(&left_lock).expect("sync with matching lock");
    }

    #[test]
    fn replacing_lock_entry_invalidates_old_guard() {
        let (_temporary, root, state) = private_root();
        let old = state.try_lock().expect("old lock");
        fs::rename(root.join("writer.lock"), root.join("retired.lock")).expect("replace lock name");
        let replacement = state.try_lock().expect("replacement lock on new inode");
        assert!(matches!(old.validate(), Err(StateDirError::LockReplaced)));
        replacement.validate().expect("replacement remains current");
    }

    #[test]
    fn same_process_lock_excludes_duplicate_and_releases() {
        let (_temporary, _root, state) = private_root();
        let first = state.try_lock().expect("first lock");
        assert!(matches!(state.try_lock(), Err(StateDirError::Locked)));
        drop(first);
        state.try_lock().expect("released lock");
    }

    #[test]
    fn lock_file_never_accepts_a_hard_link() {
        let (_temporary, root, state) = private_root();
        drop(state.try_lock().expect("create lock"));
        fs::hard_link(root.join(WRITER_LOCK_KEY), root.join("lock-alias")).expect("hard-link lock");
        assert!(matches!(state.try_lock(), Err(StateDirError::UnsafeFile)));
    }

    #[test]
    fn separate_process_lock_excludes_and_releases() {
        let (_temporary, root, state) = private_root();
        let first = state.try_lock().expect("parent lock");
        assert!(wait_bounded(spawn_probe(LOCK_CHILD_TEST, &root, false)).success());
        drop(first);
        assert!(wait_bounded(spawn_probe(LOCK_CHILD_TEST, &root, true)).success());
    }

    #[test]
    fn lock_probe_child() {
        let Some(root) = std::env::var_os(CHILD_ROOT_ENV) else {
            return;
        };
        let state = PrivateStateDir::open_root(Path::new(&root)).expect("child opens root");
        let outcome = state.try_lock();
        let expect_locked = std::env::var_os("COVALENT_EXPECT_UNLOCKED").is_none();
        if expect_locked {
            assert!(matches!(outcome, Err(StateDirError::Locked)));
        } else {
            outcome.expect("child acquires released lock");
        }
    }

    #[test]
    fn fifo_is_rejected_without_a_blocking_open() {
        let (_temporary, root, _state) = private_root();
        assert!(
            Command::new("mkfifo")
                .arg(root.join("pipe.v1"))
                .status()
                .expect("run mkfifo")
                .success()
        );
        assert!(wait_bounded(spawn_probe(FIFO_CHILD_TEST, &root, false)).success());
    }

    #[test]
    fn fifo_probe_child() {
        let Some(root) = std::env::var_os(CHILD_ROOT_ENV) else {
            return;
        };
        let state = PrivateStateDir::open_root(Path::new(&root)).expect("child opens root");
        assert!(matches!(
            state.open_file(&key("pipe.v1"), 64),
            Err(StateDirError::UnsafeFile)
        ));
    }

    #[test]
    fn inventory_is_complete_bounded_and_keeps_writer_lock_distinct() {
        let (_temporary, _root, state) = private_root();
        let child = state
            .open_or_create_child(&key("nested"))
            .expect("child directory");
        let lock = state.try_lock().expect("lock");
        state
            .create_new_file(&lock, &key("record.v1"), b"record", 64)
            .expect("record");
        let inventory = state.inventory(&lock, 3, 64).expect("inventory");
        assert_eq!(inventory.entries().len(), 3);
        assert_eq!(
            inventory
                .entries()
                .iter()
                .filter(|entry| entry.kind() == PrivateStateEntryKind::WriterLock)
                .count(),
            1
        );
        assert!(inventory.entries().iter().any(|entry| {
            entry.name().state_key().is_some_and(|entry_key| {
                entry_key.as_str() == "record.v1"
                    && entry.kind() == PrivateStateEntryKind::RegularFile
                    && entry.byte_length() == 6
            })
        }));
        assert!(inventory.entries().iter().any(|entry| {
            entry.name().state_key().is_some_and(|entry_key| {
                entry_key.as_str() == "nested" && entry.kind() == PrivateStateEntryKind::Directory
            })
        }));
        assert!(inventory.total_key_bytes() >= u64::try_from(WRITER_LOCK_KEY.len()).unwrap());
        assert!(inventory.total_file_bytes() >= 6);
        assert!(matches!(
            state.inventory(&lock, 2, 64),
            Err(StateDirError::InventoryLimit)
        ));
        assert!(matches!(
            state.inventory(&lock, 3, 1),
            Err(StateDirError::InventoryLimit)
        ));
        child.try_lock().expect("independent child remains safe");
    }

    #[test]
    fn inventory_rejects_unsafe_entries_without_blocking_on_fifo() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        symlink(root.join("missing"), root.join("link.v1")).expect("symlink");
        assert!(matches!(
            state.inventory(&lock, 8, 256),
            Err(StateDirError::UnsafeEntry)
        ));
        fs::remove_file(root.join("link.v1")).expect("remove symlink");
        assert!(
            Command::new("mkfifo")
                .arg(root.join("pipe.v1"))
                .status()
                .expect("run mkfifo")
                .success()
        );
        assert!(matches!(
            state.inventory(&lock, 8, 256),
            Err(StateDirError::UnsafeEntry)
        ));
    }

    #[test]
    fn inventory_rejects_hard_linked_private_file() {
        let (_temporary, root, state) = private_root();
        let lock = state.try_lock().expect("lock");
        state
            .create_new_file(&lock, &key("record.v1"), b"record", 64)
            .expect("record");
        fs::hard_link(root.join("record.v1"), root.join("alias.v1")).expect("hard link");
        assert!(matches!(
            state.inventory(&lock, 8, 256),
            Err(StateDirError::UnsafeFile)
        ));
    }

    fn two_private_children(
        state: &PrivateStateDir,
    ) -> (
        PrivateStateDir,
        PrivateStateDir,
        PrivateStateLock,
        PrivateStateLock,
    ) {
        let source = state
            .open_or_create_child(&key("source"))
            .expect("source child");
        let destination = state
            .open_or_create_child(&key("destination"))
            .expect("destination child");
        let source_lock = source.try_lock().expect("source lock");
        let destination_lock = destination.try_lock().expect("destination lock");
        (source, destination, source_lock, destination_lock)
    }

    #[test]
    fn promotion_preserves_single_link_and_reopens_destination() {
        let (_temporary, root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"bytes", 64)
            .expect("source");
        let promoted = destination_dir
            .promote_new_file(&destination_lock, &source_lock, source, &key("final.v1"))
            .expect("promote");
        let PrivateStatePromotion::Promoted(destination) = promoted else {
            panic!("promotion unexpectedly found an incumbent");
        };
        assert_eq!(destination.read_all().expect("destination bytes"), b"bytes");
        assert!(!root.join("source/staged.v1").exists());
        assert_eq!(
            fs::metadata(root.join("destination/final.v1"))
                .expect("destination metadata")
                .nlink(),
            1
        );
        assert_eq!(
            destination_dir
                .open_file(&key("final.v1"), 64)
                .expect("reopen")
                .read_all()
                .expect("reopened bytes"),
            b"bytes"
        );
    }

    #[test]
    fn promotion_collision_preserves_both_files() {
        let (_temporary, _root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"source", 64)
            .expect("source");
        destination_dir
            .create_new_file(&destination_lock, &key("final.v1"), b"incumbent", 64)
            .expect("incumbent");
        let outcome = destination_dir
            .promote_new_file(&destination_lock, &source_lock, source, &key("final.v1"))
            .expect("existing outcome");
        let PrivateStatePromotion::Existing {
            source,
            destination,
        } = outcome
        else {
            panic!("collision unexpectedly promoted");
        };
        assert_eq!(source.read_all().expect("source remains"), b"source");
        assert_eq!(
            destination.read_all().expect("incumbent remains"),
            b"incumbent"
        );
    }

    #[test]
    fn promotion_rejects_wrong_or_replaced_source_lock_without_mutation() {
        let (_temporary, root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"source", 64)
            .expect("source");
        assert!(matches!(
            destination_dir.promote_new_file(
                &destination_lock,
                &destination_lock,
                source,
                &key("final.v1"),
            ),
            Err(StateDirError::WrongLock)
        ));
        assert!(root.join("source/staged.v1").exists());
        let source = source_dir
            .open_file(&key("staged.v1"), 64)
            .expect("source reopen");
        fs::rename(
            root.join("source/staged.v1"),
            root.join("source/retired.v1"),
        )
        .expect("replace source");
        source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"replacement", 64)
            .expect("replacement");
        assert!(matches!(
            destination_dir.promote_new_file(
                &destination_lock,
                &source_lock,
                source,
                &key("final.v1"),
            ),
            Err(StateDirError::EntryReplaced)
        ));
        assert!(!root.join("destination/final.v1").exists());
    }

    #[test]
    fn promotion_rejects_unsafe_destination_and_same_key_returns_existing() {
        let (_temporary, root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"source", 64)
            .expect("source");
        let target = root.join("destination/target.v1");
        fs::write(&target, b"target").expect("target");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("protect target");
        symlink(&target, root.join("destination/final.v1")).expect("destination symlink");
        assert!(matches!(
            destination_dir.promote_new_file(
                &destination_lock,
                &source_lock,
                source,
                &key("final.v1"),
            ),
            Err(StateDirError::UnsafeFile)
        ));
        assert!(root.join("source/staged.v1").exists());
        fs::remove_file(root.join("destination/final.v1")).expect("remove symlink");
        fs::hard_link(&target, root.join("destination/final.v1")).expect("destination hard link");
        let source = source_dir
            .open_file(&key("staged.v1"), 64)
            .expect("source for hard-link destination");
        assert!(matches!(
            destination_dir.promote_new_file(
                &destination_lock,
                &source_lock,
                source,
                &key("final.v1"),
            ),
            Err(StateDirError::UnsafeFile)
        ));
        assert!(root.join("source/staged.v1").exists());
        fs::remove_file(root.join("destination/final.v1")).expect("remove hard link");
        let same = source_dir
            .open_file(&key("staged.v1"), 64)
            .expect("source for same key");
        let outcome = source_dir
            .promote_new_file(&source_lock, &source_lock, same, &key("staged.v1"))
            .expect("same key existing");
        assert!(matches!(outcome, PrivateStatePromotion::Existing { .. }));
    }

    #[test]
    fn promotion_parent_sync_failure_is_uncertain_but_keeps_bytes() {
        let (_temporary, root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"bytes", 64)
            .expect("source");
        assert!(matches!(
            destination_dir.promote_new_file_with_sync(
                &destination_lock,
                &source_lock,
                source,
                &key("final.v1"),
                |_source, _destination| Err(StateDirError::Io {
                    operation: "test promotion parent sync",
                    kind: ErrorKind::Other,
                    raw_os_error: None,
                }),
            ),
            Err(StateDirError::UncertainPromotion)
        ));
        assert!(!root.join("source/staged.v1").exists());
        assert_eq!(
            destination_dir
                .open_file(&key("final.v1"), 64)
                .expect("reopen uncertain destination")
                .read_all()
                .expect("bytes"),
            b"bytes"
        );
    }

    #[test]
    fn promotion_never_returns_an_unrelated_post_rename_replacement() {
        let (_temporary, root, state) = private_root();
        let (source_dir, destination_dir, source_lock, destination_lock) =
            two_private_children(&state);
        let source = source_dir
            .create_new_file(&source_lock, &key("staged.v1"), b"original", 64)
            .expect("source");
        let outcome = destination_dir.promote_new_file_with_sync(
            &destination_lock,
            &source_lock,
            source,
            &key("final.v1"),
            |source_directory, destination_directory| {
                fs::rename(
                    root.join("destination/final.v1"),
                    root.join("destination/displaced.v1"),
                )
                .expect("retain original");
                fs::write(root.join("destination/final.v1"), b"replacement").expect("replacement");
                fs::set_permissions(
                    root.join("destination/final.v1"),
                    fs::Permissions::from_mode(0o600),
                )
                .expect("private replacement");
                fsync(source_directory).expect("sync source");
                fsync(destination_directory).expect("sync destination");
                Ok(())
            },
        );
        assert!(matches!(outcome, Err(StateDirError::UncertainPromotion)));
        assert_eq!(
            fs::read(root.join("destination/displaced.v1")).unwrap(),
            b"original"
        );
        assert_eq!(
            fs::read(root.join("destination/final.v1")).unwrap(),
            b"replacement"
        );
    }

    #[test]
    fn inventory_rejects_growth_replacement_and_late_new_entries() {
        for change in 0..3 {
            let (_temporary, root, state) = private_root();
            let lock = state.try_lock().expect("lock");
            state
                .create_new_file(&lock, &key("record.v1"), b"record", 64)
                .expect("record");
            let result = state.inventory_with_check(&lock, 4, 256, || match change {
                0 => fs::OpenOptions::new()
                    .append(true)
                    .open(root.join("record.v1"))
                    .unwrap()
                    .write_all(b"growth")
                    .unwrap(),
                1 => {
                    fs::rename(root.join("record.v1"), root.join("displaced.v1")).unwrap();
                    fs::write(root.join("record.v1"), b"record").unwrap();
                    fs::set_permissions(root.join("record.v1"), fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
                _ => {
                    fs::write(root.join("late.v1"), b"late").unwrap();
                    fs::set_permissions(root.join("late.v1"), fs::Permissions::from_mode(0o600))
                        .unwrap();
                }
            });
            assert!(matches!(result, Err(StateDirError::InventoryChanged)));
        }
    }

    #[test]
    fn errors_do_not_retain_paths_or_keys() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let secret_path = temporary.path().join("secret-token-123");
        let error = PrivateStateDir::open_root(&secret_path)
            .err()
            .expect("missing root");
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret-token-123"));
        let error = StateKey::new("secret/token").err().expect("invalid key");
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("secret/token"));
    }
}
