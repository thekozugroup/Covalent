//! Read-only, descriptor-anchored inventory of one local Unix source tree.
//!
//! This scanner is deliberately separate from backup scanning. It neither
//! creates operations nor proposes deletions: callers may use an inventory
//! only after this function returns a complete result. It detects ordinary
//! replacement and mutation races, but it is a best-effort live scan, not a
//! filesystem snapshot or an authorization boundary.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
use thiserror::Error;

use crate::engine::{JobControl, JobState};

use super::body::{ContentDigest, EntryValue, FileContent};
use super::path::{MAX_SYNC_PATH_BYTES, MAX_SYNC_PATH_DEPTH, SyncPath};

const HASH_BUFFER_BYTES: usize = 64 * 1024;

/// Bounds one complete local source inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceInventoryLimits {
    /// Maximum regular-file and directory entries, excluding the selected root.
    pub maximum_entries: usize,
    /// Maximum combined owned raw and canonical entry-path bytes.
    pub maximum_path_bytes: usize,
    /// Maximum plaintext bytes read while hashing regular files.
    pub maximum_read_bytes: u64,
    /// Maximum traversal depth, capped by the protocol path depth.
    pub maximum_depth: usize,
}

/// A completed, sorted read-only source inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceInventory {
    entries: BTreeMap<SyncPath, EntryValue>,
    bytes_read: u64,
}

impl SourceInventory {
    /// Canonical paths and whole-file content commitments from a completed scan.
    #[must_use]
    pub fn entries(&self) -> &BTreeMap<SyncPath, EntryValue> {
        &self.entries
    }

    /// Plaintext bytes read to produce this inventory.
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
}

/// A redacted source-inventory failure.
///
/// The error has no source path, file contents, content digest, or arbitrary
/// operating-system message. A caller can associate it with the already known
/// selected root without treating an error as an inventory.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SourceInventoryError {
    /// The selected root was not a stable real directory.
    #[error("source root is unavailable")]
    InvalidRoot,
    /// A source file or directory could not be read safely.
    #[error("source entry is unreadable")]
    Unreadable,
    /// Reading a directory cursor failed.
    #[error("source directory cursor failed")]
    Cursor,
    /// A source symlink was observed.
    #[error("source contains a symbolic link")]
    Symlink,
    /// A source entry was not a regular file or directory.
    #[error("source contains an unsupported entry")]
    Unsupported,
    /// A local source name cannot be represented by a canonical sync path.
    #[error("source contains an unsupported name")]
    InvalidName,
    /// Two local names normalize to the same canonical sync path.
    #[error("source names collide after canonicalization")]
    CanonicalCollision,
    /// An entry or root changed during the live scan.
    #[error("source changed during inventory")]
    Changed,
    /// A configured work or memory bound would be exceeded.
    #[error("source inventory exceeds a configured limit")]
    Limit,
    /// The caller paused work before a complete inventory existed.
    #[error("source inventory is paused")]
    Paused,
    /// The caller cancelled work before a complete inventory existed.
    #[error("source inventory is cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservedKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fingerprint {
    device: u64,
    inode: u64,
    length: u64,
    // Signed libc timestamps and unsigned Linux nanoseconds fit losslessly.
    modified_seconds: i128,
    modified_nanoseconds: i128,
    changed_seconds: i128,
    changed_nanoseconds: i128,
    mode: u32,
}

impl Fingerprint {
    // `rustix::fs::Stat` mirrors each target libc ABI: macOS uses narrower
    // device/mode fields while Linux and Android already use these widths.
    #[allow(clippy::unnecessary_cast)]
    fn from_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            length: u64::try_from(stat.st_size).unwrap_or(u64::MAX),
            modified_seconds: i128::from(stat.st_mtime),
            modified_nanoseconds: i128::from(stat.st_mtime_nsec),
            changed_seconds: i128::from(stat.st_ctime),
            changed_nanoseconds: i128::from(stat.st_ctime_nsec),
            mode: stat.st_mode as u32,
        }
    }
}

struct ObservedEntry {
    raw_path: String,
    fingerprint: Fingerprint,
    kind: ObservedKind,
}

struct ScanState {
    entries: BTreeMap<SyncPath, EntryValue>,
    observed: Vec<ObservedEntry>,
    path_bytes: usize,
    pending_cursor_entries: usize,
    pending_cursor_bytes: usize,
    bytes_read: u64,
}

trait ScanHooks {
    fn before_cursor(&mut self, _raw_path: &str) -> Result<(), SourceInventoryError> {
        Ok(())
    }

    fn after_file_read(&mut self, _raw_path: &str) -> Result<(), SourceInventoryError> {
        Ok(())
    }

    fn before_final_revalidation(&mut self) -> Result<(), SourceInventoryError> {
        Ok(())
    }
}

struct NoHooks;
impl ScanHooks for NoHooks {}

/// Returns a complete source inventory, or an error with no partial inventory.
///
/// This is available only on Unix. Android SAF trees require their own
/// document-provider scanner; Windows deliberately has no fallback here.
pub fn scan_source_inventory(
    source_root: &Path,
    limits: SourceInventoryLimits,
    control: &JobControl,
) -> Result<SourceInventory, SourceInventoryError> {
    scan_with_hooks(source_root, limits, control, &mut NoHooks)
}

fn scan_with_hooks(
    source_root: &Path,
    limits: SourceInventoryLimits,
    control: &JobControl,
    hooks: &mut dyn ScanHooks,
) -> Result<SourceInventory, SourceInventoryError> {
    let depth_limit = limits.maximum_depth.min(MAX_SYNC_PATH_DEPTH);
    if limits.maximum_depth == 0 || depth_limit == 0 {
        return Err(SourceInventoryError::Limit);
    }
    check_control(control)?;
    let selected =
        std::fs::symlink_metadata(source_root).map_err(|_| SourceInventoryError::InvalidRoot)?;
    if selected.file_type().is_symlink() || !selected.is_dir() {
        return Err(SourceInventoryError::InvalidRoot);
    }
    let canonical =
        std::fs::canonicalize(source_root).map_err(|_| SourceInventoryError::InvalidRoot)?;
    let root = open(
        &canonical,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| SourceInventoryError::InvalidRoot)?;
    let root_fingerprint =
        Fingerprint::from_stat(&fstat(&root).map_err(|_| SourceInventoryError::InvalidRoot)?);
    if root_fingerprint != metadata_fingerprint(&selected) {
        return Err(SourceInventoryError::Changed);
    }

    let mut state = ScanState {
        entries: BTreeMap::new(),
        observed: Vec::new(),
        path_bytes: 0,
        pending_cursor_entries: 0,
        pending_cursor_bytes: 0,
        bytes_read: 0,
    };
    let mut raw_components = Vec::new();
    scan_directory(
        &root,
        "",
        &mut raw_components,
        0,
        root_fingerprint,
        limits,
        depth_limit,
        control,
        hooks,
        &mut state,
    )?;
    hooks.before_final_revalidation()?;
    check_control(control)?;
    for observed in &state.observed {
        check_control(control)?;
        let descriptor = open_raw_entry(&root, &observed.raw_path, observed.kind)
            .map_err(|_| SourceInventoryError::Changed)?;
        let current =
            Fingerprint::from_stat(&fstat(&descriptor).map_err(|_| SourceInventoryError::Changed)?);
        if current != observed.fingerprint {
            return Err(SourceInventoryError::Changed);
        }
    }
    revalidate_root(source_root, &canonical, &root, root_fingerprint)?;
    check_control(control)?;
    Ok(SourceInventory {
        entries: state.entries,
        bytes_read: state.bytes_read,
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_directory(
    directory: &std::os::fd::OwnedFd,
    raw_path: &str,
    raw_components: &mut Vec<String>,
    depth: usize,
    before: Fingerprint,
    limits: SourceInventoryLimits,
    depth_limit: usize,
    control: &JobControl,
    hooks: &mut dyn ScanHooks,
    state: &mut ScanState,
) -> Result<(), SourceInventoryError> {
    check_control(control)?;
    hooks.before_cursor(raw_path)?;
    let mut names = Vec::new();
    let mut reader =
        rustix::fs::Dir::read_from(directory).map_err(|_| SourceInventoryError::Cursor)?;
    for entry in &mut reader {
        check_control(control)?;
        let entry = entry.map_err(|_| SourceInventoryError::Cursor)?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if bytes.len() > MAX_SYNC_PATH_BYTES {
            return Err(SourceInventoryError::Limit);
        }
        let name = std::str::from_utf8(bytes).map_err(|_| SourceInventoryError::InvalidName)?;
        reserve_cursor_name(state, name.len(), limits)?;
        names.push(name.to_owned());
    }
    names.sort_unstable();

    for name in names {
        check_control(control)?;
        release_cursor_name(state, name.len())?;
        let child_raw = join_raw(raw_path, &name)?;
        let child_depth = depth.checked_add(1).ok_or(SourceInventoryError::Limit)?;
        if child_depth > depth_limit {
            return Err(SourceInventoryError::Limit);
        }
        raw_components.push(name);
        let result = scan_child(
            directory,
            &child_raw,
            raw_components,
            child_depth,
            limits,
            depth_limit,
            control,
            hooks,
            state,
        );
        raw_components.pop();
        result?;
    }

    let after =
        Fingerprint::from_stat(&fstat(directory).map_err(|_| SourceInventoryError::Changed)?);
    if after != before {
        return Err(SourceInventoryError::Changed);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn scan_child(
    parent: &std::os::fd::OwnedFd,
    raw_path: &str,
    raw_components: &mut Vec<String>,
    depth: usize,
    limits: SourceInventoryLimits,
    depth_limit: usize,
    control: &JobControl,
    hooks: &mut dyn ScanHooks,
    state: &mut ScanState,
) -> Result<(), SourceInventoryError> {
    let name = raw_components.last().ok_or(SourceInventoryError::Changed)?;
    let stat = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_entry_open_error)?;
    let file_type = FileType::from_raw_mode(stat.st_mode);
    if file_type == FileType::Symlink {
        return Err(SourceInventoryError::Symlink);
    }
    let kind = if file_type == FileType::Directory {
        ObservedKind::Directory
    } else if file_type == FileType::RegularFile {
        ObservedKind::File
    } else {
        return Err(SourceInventoryError::Unsupported);
    };
    let before = Fingerprint::from_stat(&stat);
    let descriptor = openat(parent, name, entry_open_flags(kind), Mode::empty())
        .map_err(map_entry_open_error)?;
    let opened =
        Fingerprint::from_stat(&fstat(&descriptor).map_err(|_| SourceInventoryError::Unreadable)?);
    if opened != before {
        return Err(SourceInventoryError::Changed);
    }
    let path = SyncPath::from_local(raw_path).map_err(|_| SourceInventoryError::InvalidName)?;
    match kind {
        ObservedKind::Directory => {
            reserve_entry(state, raw_path, &path, limits)?;
            insert_entry(state, path, EntryValue::Directory)?;
            scan_directory(
                &descriptor,
                raw_path,
                raw_components,
                depth,
                before,
                limits,
                depth_limit,
                control,
                hooks,
                state,
            )?;
            state.observed.push(ObservedEntry {
                raw_path: raw_path.to_owned(),
                fingerprint: before,
                kind,
            });
        }
        ObservedKind::File => {
            if state.entries.contains_key(&path) {
                return Err(SourceInventoryError::CanonicalCollision);
            }
            // Reserve the retained raw revalidation path and canonical output
            // path before opening and hashing potentially large content.
            reserve_entry(state, raw_path, &path, limits)?;
            let value = hash_regular_file(
                &descriptor,
                parent,
                name,
                raw_path,
                before,
                limits,
                control,
                hooks,
                state,
            )?;
            insert_entry(state, path, value)?;
            state.observed.push(ObservedEntry {
                raw_path: raw_path.to_owned(),
                fingerprint: before,
                kind,
            });
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn hash_regular_file(
    descriptor: &std::os::fd::OwnedFd,
    parent: &std::os::fd::OwnedFd,
    name: &str,
    raw_path: &str,
    before: Fingerprint,
    limits: SourceInventoryLimits,
    control: &JobControl,
    hooks: &mut dyn ScanHooks,
    state: &mut ScanState,
) -> Result<EntryValue, SourceInventoryError> {
    if before.length > limits.maximum_read_bytes.saturating_sub(state.bytes_read) {
        return Err(SourceInventoryError::Limit);
    }
    let duplicate = rustix::io::fcntl_dupfd_cloexec(descriptor, 3)
        .map_err(|_| SourceInventoryError::Unreadable)?;
    let mut file = std::fs::File::from(duplicate);
    let mut remaining = before.length;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    while remaining > 0 {
        check_control(control)?;
        let desired = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = match file.read(&mut buffer[..desired]) {
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                check_control(control)?;
                continue;
            }
            Err(_) => return Err(SourceInventoryError::Unreadable),
        };
        if read == 0 {
            return Err(SourceInventoryError::Changed);
        }
        hasher.update(&buffer[..read]);
        state.bytes_read = state
            .bytes_read
            .checked_add(u64::try_from(read).map_err(|_| SourceInventoryError::Limit)?)
            .ok_or(SourceInventoryError::Limit)?;
        remaining = remaining
            .checked_sub(u64::try_from(read).map_err(|_| SourceInventoryError::Limit)?)
            .ok_or(SourceInventoryError::Changed)?;
    }
    hooks.after_file_read(raw_path)?;
    check_control(control)?;
    let after_handle =
        Fingerprint::from_stat(&fstat(&file).map_err(|_| SourceInventoryError::Changed)?);
    let after_path = Fingerprint::from_stat(
        &statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(map_entry_open_error)?,
    );
    if after_handle != before || after_path != before {
        return Err(SourceInventoryError::Changed);
    }
    let digest = ContentDigest::from_bytes(*hasher.finalize().as_bytes());
    let executable = before.mode & 0o111 != 0;
    FileContent::new(digest, before.length, executable)
        .map(EntryValue::File)
        .map_err(|_| SourceInventoryError::Changed)
}

fn reserve_entry(
    state: &mut ScanState,
    raw_path: &str,
    canonical_path: &SyncPath,
    limits: SourceInventoryLimits,
) -> Result<(), SourceInventoryError> {
    let owned_bytes = raw_path
        .len()
        .checked_add(canonical_path.as_str().len())
        .ok_or(SourceInventoryError::Limit)?;
    let entries = state
        .entries
        .len()
        .checked_add(state.pending_cursor_entries)
        .and_then(|value| value.checked_add(1))
        .ok_or(SourceInventoryError::Limit)?;
    let path_bytes = state
        .path_bytes
        .checked_add(state.pending_cursor_bytes)
        .and_then(|value| value.checked_add(owned_bytes))
        .ok_or(SourceInventoryError::Limit)?;
    if entries > limits.maximum_entries || path_bytes > limits.maximum_path_bytes {
        return Err(SourceInventoryError::Limit);
    }
    state.path_bytes = state
        .path_bytes
        .checked_add(owned_bytes)
        .ok_or(SourceInventoryError::Limit)?;
    Ok(())
}

fn reserve_cursor_name(
    state: &mut ScanState,
    name_bytes: usize,
    limits: SourceInventoryLimits,
) -> Result<(), SourceInventoryError> {
    let entries = state
        .entries
        .len()
        .checked_add(state.pending_cursor_entries)
        .and_then(|value| value.checked_add(1))
        .ok_or(SourceInventoryError::Limit)?;
    let path_bytes = state
        .path_bytes
        .checked_add(state.pending_cursor_bytes)
        .and_then(|value| value.checked_add(name_bytes))
        .ok_or(SourceInventoryError::Limit)?;
    if entries > limits.maximum_entries || path_bytes > limits.maximum_path_bytes {
        return Err(SourceInventoryError::Limit);
    }
    state.pending_cursor_entries = state
        .pending_cursor_entries
        .checked_add(1)
        .ok_or(SourceInventoryError::Limit)?;
    state.pending_cursor_bytes = state
        .pending_cursor_bytes
        .checked_add(name_bytes)
        .ok_or(SourceInventoryError::Limit)?;
    Ok(())
}

fn release_cursor_name(
    state: &mut ScanState,
    name_bytes: usize,
) -> Result<(), SourceInventoryError> {
    state.pending_cursor_entries = state
        .pending_cursor_entries
        .checked_sub(1)
        .ok_or(SourceInventoryError::Changed)?;
    state.pending_cursor_bytes = state
        .pending_cursor_bytes
        .checked_sub(name_bytes)
        .ok_or(SourceInventoryError::Changed)?;
    Ok(())
}

fn insert_entry(
    state: &mut ScanState,
    path: SyncPath,
    value: EntryValue,
) -> Result<(), SourceInventoryError> {
    if state.entries.insert(path, value).is_some() {
        return Err(SourceInventoryError::CanonicalCollision);
    }
    Ok(())
}

fn open_raw_entry(
    root: &std::os::fd::OwnedFd,
    raw_path: &str,
    kind: ObservedKind,
) -> Result<std::os::fd::OwnedFd, rustix::io::Errno> {
    let mut current = rustix::io::fcntl_dupfd_cloexec(root, 3)?;
    let mut components = raw_path.split('/').peekable();
    while let Some(component) = components.next() {
        let final_component = components.peek().is_none();
        let flags = if final_component {
            entry_open_flags(kind)
        } else {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        };
        current = openat(&current, component, flags, Mode::empty())?;
    }
    Ok(current)
}

fn entry_open_flags(kind: ObservedKind) -> OFlags {
    let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    match kind {
        ObservedKind::Directory => flags |= OFlags::DIRECTORY,
        ObservedKind::File => flags |= OFlags::NONBLOCK | OFlags::NOCTTY,
    }
    flags
}

fn revalidate_root(
    selected_root: &Path,
    canonical: &Path,
    root: &std::os::fd::OwnedFd,
    before: Fingerprint,
) -> Result<(), SourceInventoryError> {
    let selected =
        std::fs::symlink_metadata(selected_root).map_err(|_| SourceInventoryError::Changed)?;
    if selected.file_type().is_symlink()
        || !selected.is_dir()
        || metadata_fingerprint(&selected) != before
    {
        return Err(SourceInventoryError::Changed);
    }
    let current_canonical =
        std::fs::canonicalize(selected_root).map_err(|_| SourceInventoryError::Changed)?;
    if current_canonical != canonical {
        return Err(SourceInventoryError::Changed);
    }
    let reopened = open(
        canonical,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| SourceInventoryError::Changed)?;
    let reopened_fingerprint =
        Fingerprint::from_stat(&fstat(&reopened).map_err(|_| SourceInventoryError::Changed)?);
    let anchored_fingerprint =
        Fingerprint::from_stat(&fstat(root).map_err(|_| SourceInventoryError::Changed)?);
    if reopened_fingerprint != before || anchored_fingerprint != before {
        return Err(SourceInventoryError::Changed);
    }
    Ok(())
}

fn metadata_fingerprint(metadata: &std::fs::Metadata) -> Fingerprint {
    use std::os::unix::fs::MetadataExt;

    Fingerprint {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: i128::from(metadata.mtime()),
        modified_nanoseconds: i128::from(metadata.mtime_nsec()),
        changed_seconds: i128::from(metadata.ctime()),
        changed_nanoseconds: i128::from(metadata.ctime_nsec()),
        mode: metadata.mode(),
    }
}

fn join_raw(parent: &str, name: &str) -> Result<String, SourceInventoryError> {
    let length = parent
        .len()
        .checked_add(usize::from(!parent.is_empty()))
        .and_then(|value| value.checked_add(name.len()))
        .ok_or(SourceInventoryError::Limit)?;
    if length > MAX_SYNC_PATH_BYTES {
        return Err(SourceInventoryError::InvalidName);
    }
    if parent.is_empty() {
        Ok(name.to_owned())
    } else {
        Ok(format!("{parent}/{name}"))
    }
}

fn check_control(control: &JobControl) -> Result<(), SourceInventoryError> {
    match control.state() {
        JobState::Running => Ok(()),
        JobState::Paused => Err(SourceInventoryError::Paused),
        JobState::Cancelled => Err(SourceInventoryError::Cancelled),
    }
}

fn map_entry_open_error(error: rustix::io::Errno) -> SourceInventoryError {
    match error {
        rustix::io::Errno::LOOP => SourceInventoryError::Symlink,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => SourceInventoryError::Unreadable,
        rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => SourceInventoryError::Changed,
        _ => SourceInventoryError::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use super::*;

    fn limits() -> SourceInventoryLimits {
        SourceInventoryLimits {
            maximum_entries: 32,
            maximum_path_bytes: 8 * 1024,
            maximum_read_bytes: 1024 * 1024,
            maximum_depth: 8,
        }
    }

    fn scan(root: &Path) -> Result<SourceInventory, SourceInventoryError> {
        scan_source_inventory(root, limits(), &JobControl::new())
    }

    #[test]
    fn hashes_regular_files_and_keeps_empty_directories() {
        let root = TempDir::new().expect("root");
        fs::create_dir(root.path().join("empty")).expect("empty directory");
        fs::write(root.path().join("run"), b"hello").expect("file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.path().join("run"), fs::Permissions::from_mode(0o755))
                .expect("mode");
        }
        let inventory = scan(root.path()).expect("inventory");
        assert_eq!(inventory.entries().len(), 2);
        assert_eq!(inventory.bytes_read(), 5);
        assert_eq!(
            inventory
                .entries()
                .get(&SyncPath::from_wire("empty").expect("path")),
            Some(&EntryValue::Directory)
        );
        assert_eq!(
            inventory
                .entries()
                .get(&SyncPath::from_wire("run").expect("path")),
            Some(&EntryValue::File(
                FileContent::new(
                    ContentDigest::from_bytes(*blake3::hash(b"hello").as_bytes()),
                    5,
                    true,
                )
                .expect("content")
            ))
        );
    }

    #[test]
    fn rejects_symlink_and_fifo() {
        let symlink_root = TempDir::new().expect("root");
        fs::write(symlink_root.path().join("target"), b"x").expect("target");
        std::os::unix::fs::symlink("target", symlink_root.path().join("link")).expect("link");
        assert_eq!(
            scan(symlink_root.path()),
            Err(SourceInventoryError::Symlink)
        );

        let fifo_root = TempDir::new().expect("root");
        let status = std::process::Command::new("mkfifo")
            .arg(fifo_root.path().join("pipe"))
            .status()
            .expect("mkfifo");
        assert!(status.success());
        assert_eq!(
            scan(fifo_root.path()),
            Err(SourceInventoryError::Unsupported)
        );
    }

    #[test]
    fn canonicalized_local_name_collision_is_never_an_inventory_entry() {
        let mut state = ScanState {
            entries: BTreeMap::new(),
            observed: Vec::new(),
            path_bytes: 0,
            pending_cursor_entries: 0,
            pending_cursor_bytes: 0,
            bytes_read: 0,
        };
        let canonical = SyncPath::from_local("caf\u{e9}").expect("canonical local path");
        let decomposed = SyncPath::from_local("cafe\u{301}").expect("same canonical path");
        assert_eq!(canonical, decomposed);
        insert_entry(&mut state, canonical, EntryValue::Directory).expect("first spelling");
        assert_eq!(
            insert_entry(&mut state, decomposed, EntryValue::Directory),
            Err(SourceInventoryError::CanonicalCollision)
        );
    }

    #[test]
    fn limits_and_cancellation_never_return_an_inventory() {
        let root = TempDir::new().expect("root");
        fs::write(root.path().join("file"), b"1234").expect("file");
        assert_eq!(
            scan_source_inventory(
                root.path(),
                SourceInventoryLimits {
                    maximum_read_bytes: 3,
                    ..limits()
                },
                &JobControl::new(),
            ),
            Err(SourceInventoryError::Limit)
        );
        let control = JobControl::new();
        control.cancel();
        assert_eq!(
            scan_source_inventory(root.path(), limits(), &control),
            Err(SourceInventoryError::Cancelled)
        );
    }

    #[test]
    fn nested_cursor_names_share_one_global_bounded_reservation() {
        let root = TempDir::new().expect("root");
        fs::create_dir(root.path().join("deep")).expect("deep directory");
        fs::write(root.path().join("deep/a"), b"a").expect("first nested file");
        fs::write(root.path().join("deep/b"), b"b").expect("second nested file");
        fs::write(root.path().join("sibling"), b"s").expect("root sibling");
        assert_eq!(
            scan_source_inventory(
                root.path(),
                SourceInventoryLimits {
                    maximum_entries: 3,
                    ..limits()
                },
                &JobControl::new(),
            ),
            Err(SourceInventoryError::Limit)
        );
    }

    #[test]
    fn entry_reservation_keeps_retained_sibling_cursor_bytes_in_the_global_bound() {
        let mut state = ScanState {
            entries: BTreeMap::new(),
            observed: Vec::new(),
            path_bytes: 3,
            pending_cursor_entries: 1,
            pending_cursor_bytes: 3,
            bytes_read: 0,
        };
        let canonical = SyncPath::from_wire("z").expect("canonical path");
        let bounded = SourceInventoryLimits {
            maximum_entries: 3,
            maximum_path_bytes: 10,
            ..limits()
        };
        // Existing bytes + this raw/canonical pair fits alone (3 + 5), but
        // the still-owned sibling cursor name makes the real total 11.
        assert_eq!(
            reserve_entry(&mut state, "four", &canonical, bounded),
            Err(SourceInventoryError::Limit)
        );
        assert_eq!(state.path_bytes, 3);
    }

    enum HookAction {
        MutateFile(PathBuf),
        FailCursor,
        FailUnreadable,
        ReplaceRoot {
            selected: PathBuf,
            replacement: PathBuf,
            retired: PathBuf,
        },
    }

    struct Hooks(Option<HookAction>);

    impl ScanHooks for Hooks {
        fn before_cursor(&mut self, raw_path: &str) -> Result<(), SourceInventoryError> {
            if raw_path.is_empty() && matches!(self.0, Some(HookAction::FailCursor)) {
                return Err(SourceInventoryError::Cursor);
            }
            if raw_path.is_empty() && matches!(self.0, Some(HookAction::FailUnreadable)) {
                return Err(SourceInventoryError::Unreadable);
            }
            Ok(())
        }

        fn after_file_read(&mut self, _raw_path: &str) -> Result<(), SourceInventoryError> {
            let Some(HookAction::MutateFile(path)) = self.0.as_ref() else {
                return Ok(());
            };
            let path = path.clone();
            self.0 = None;
            fs::write(path, b"changed").expect("mutate file");
            Ok(())
        }

        fn before_final_revalidation(&mut self) -> Result<(), SourceInventoryError> {
            if let Some(HookAction::ReplaceRoot {
                selected,
                replacement,
                retired,
            }) = self.0.take()
            {
                fs::rename(&selected, retired).expect("retire selected root");
                fs::rename(replacement, selected).expect("replace root");
            }
            Ok(())
        }
    }

    #[test]
    fn injected_cursor_mutation_and_root_replacement_fail_without_a_result() {
        let cursor_root = TempDir::new().expect("root");
        let mut cursor_hooks = Hooks(Some(HookAction::FailCursor));
        assert_eq!(
            scan_with_hooks(
                cursor_root.path(),
                limits(),
                &JobControl::new(),
                &mut cursor_hooks
            ),
            Err(SourceInventoryError::Cursor)
        );
        let mut unreadable_hooks = Hooks(Some(HookAction::FailUnreadable));
        assert_eq!(
            scan_with_hooks(
                cursor_root.path(),
                limits(),
                &JobControl::new(),
                &mut unreadable_hooks
            ),
            Err(SourceInventoryError::Unreadable)
        );

        let mutation_root = TempDir::new().expect("root");
        let file = mutation_root.path().join("file");
        fs::write(&file, b"original").expect("file");
        let mut mutation_hooks = Hooks(Some(HookAction::MutateFile(file)));
        assert_eq!(
            scan_with_hooks(
                mutation_root.path(),
                limits(),
                &JobControl::new(),
                &mut mutation_hooks
            ),
            Err(SourceInventoryError::Changed)
        );

        let parent = TempDir::new().expect("parent");
        let selected = parent.path().join("selected");
        let replacement = parent.path().join("replacement");
        fs::create_dir(&selected).expect("selected");
        fs::write(selected.join("file"), b"old").expect("old file");
        fs::create_dir(&replacement).expect("replacement");
        fs::write(replacement.join("file"), b"new").expect("new file");
        let mut replacement_hooks = Hooks(Some(HookAction::ReplaceRoot {
            selected: selected.clone(),
            replacement,
            retired: parent.path().join("retired-selected"),
        }));
        assert_eq!(
            scan_with_hooks(
                &selected,
                limits(),
                &JobControl::new(),
                &mut replacement_hooks
            ),
            Err(SourceInventoryError::Changed)
        );
    }
}
