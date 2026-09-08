//! Non-destructive recovery of a simple-versioner archive.
//!
//! This is deliberately a local filesystem primitive, not a proxy for
//! Syncthing's restore endpoint. The caller must already have authorized the
//! folder and obtained the selected metadata through the private engine API.
//! That metadata identifies an upstream archive name, not its contents: the
//! archive can change after listing, including where two versions share a
//! second. We pin and verify the archive locally and return a digest of the
//! bytes actually copied.

use std::fmt;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use covalent_core::{JobControl, JobState};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, fsync, open, openat, statat, unlinkat};
use rustix::io::fcntl_dupfd_cloexec;
use sha2::{Digest as _, Sha256};

/// Maximum bytes copied by one recover-as-copy operation unless the caller
/// selects a smaller bound. The bound is checked before opening the archive.
pub const MAX_RECOVERY_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_COMPONENTS: usize = 64;
const MAX_COMPONENT_BYTES: usize = 255;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_VERSION_ROWS: usize = 128;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// The only archive layout supported by this primitive. The controller must
/// compare this with the engine's *effective* folder configuration before it
/// makes recovery available. Custom versioner types and paths are rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimpleVersionerConfig {
    /// Syncthing's versioner type. It must be `simple`.
    pub kind: String,
    /// Versioner path from effective config. It must be empty, meaning the
    /// pinned upstream default `.stversions` below the folder root.
    pub path: String,
    /// Pinned retention count.
    pub keep: u32,
    /// Pinned cleanout interval.
    pub cleanout_days: u32,
}

impl SimpleVersionerConfig {
    /// The supported v2.1.3 configuration: default archive path, keep 100,
    /// and no automatic cleanout.
    pub fn pinned_default() -> Self {
        Self {
            kind: "simple".to_owned(),
            path: String::new(),
            keep: 100,
            cleanout_days: 0,
        }
    }

    fn is_pinned_default(&self) -> bool {
        self == &Self::pinned_default()
    }
}

/// One decoded row from the private engine's complete versions listing for a
/// single path. It has no digest and is not a content identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionMetadata {
    /// RFC3339 `versionTime` as returned by the engine.
    pub version_time: String,
    /// RFC3339 `modTime` as returned by the engine.
    pub mod_time: String,
    /// Byte size as returned by the engine.
    pub size: u64,
}

/// A selected and unambiguous version. Construct this only with
/// [`SelectedVersion::from_complete_listing`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedVersion {
    path: String,
    version_time: String,
    mod_time: String,
    size: u64,
}

impl SelectedVersion {
    /// Validate one selection from the complete private API listing for
    /// `path`. The function maps every retained row through the pinned v2.1.3
    /// simple-versioner filename rule and rejects a same-tag collision instead
    /// of guessing which archive the caller meant.
    pub fn from_complete_listing(
        path: &str,
        versions: &[VersionMetadata],
        selected_index: usize,
    ) -> Result<Self, RecoveryError> {
        let components = parse_relative_path(path)?;
        if versions.is_empty() || versions.len() > MAX_VERSION_ROWS {
            return Err(RecoveryError::UnsafeInput);
        }
        let selected = versions
            .get(selected_index)
            .ok_or(RecoveryError::UnsafeInput)?;
        let selected_time = parse_rfc3339(&selected.version_time)?;
        let selected_mod_time = parse_rfc3339(&selected.mod_time)?;
        if selected_time.nanoseconds != 0 || selected_mod_time.nanoseconds != 0 {
            return Err(RecoveryError::UnsafeInput);
        }
        let selected_tag = selected_time.local_tag();
        let name = components.last().ok_or(RecoveryError::UnsafeInput)?;
        let selected_archive = tagged_filename(name, &selected_tag)?;
        let mut matching_archives = 0_usize;
        for version in versions {
            let version_time = parse_rfc3339(&version.version_time)?;
            let mod_time = parse_rfc3339(&version.mod_time)?;
            if version_time.nanoseconds != 0 || mod_time.nanoseconds != 0 {
                return Err(RecoveryError::UnsafeInput);
            }
            if tagged_filename(name, &version_time.local_tag())? == selected_archive {
                matching_archives += 1;
            }
        }
        if matching_archives != 1 {
            return Err(RecoveryError::UnsafeInput);
        }
        Ok(Self {
            path: path.to_owned(),
            version_time: selected.version_time.clone(),
            mod_time: selected.mod_time.clone(),
            size: selected.size,
        })
    }
}

/// Inputs for one controller-approved local recovery.
pub struct RecoverAsCopyRequest<'a> {
    /// Existing folder root chosen by the controller, never a peer-supplied
    /// path. It is opened without following a final symlink and rechecked.
    pub folder_root: &'a Path,
    pub versioner: SimpleVersionerConfig,
    pub selected: &'a SelectedVersion,
    /// A safe basename chosen by the caller for a fresh copy in the selected
    /// file's parent directory. Existing names always fail; this primitive
    /// never chooses a replacement or truncates an incumbent file.
    pub destination_basename: &'a str,
    /// Must be nonzero and no larger than [`MAX_RECOVERY_FILE_BYTES`].
    pub maximum_bytes: u64,
    /// Cancellation is checked before creation and between every bounded read.
    pub control: &'a JobControl,
}

/// Receipt for bytes synced to a fresh local sibling. The digest is derived
/// while reading the held archive descriptor; it is not an upstream claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReceipt {
    pub destination: PathBuf,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// Fixed, redacted outcomes. They deliberately contain no filesystem path,
/// API body, credential, or operating-system error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryError {
    UnsafeInput,
    UnsupportedVersioner,
    SourceChanged,
    DestinationExists,
    Cancelled,
    IoFailure,
    /// A file or its directory may have reached durable storage, or a partial
    /// name could not be proven to be our inode. It is intentionally retained
    /// for local inspection rather than described as rolled back.
    PersistenceUncertain,
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsafeInput => "selected recovery version is unsafe",
            Self::UnsupportedVersioner => "selected recovery versioner is unsupported",
            Self::SourceChanged => "selected recovery archive changed",
            Self::DestinationExists => "recovery copy name is already in use",
            Self::Cancelled => "recovery copy was cancelled",
            Self::IoFailure => "recovery copy could not be completed",
            Self::PersistenceUncertain => "recovery copy persistence is uncertain",
        })
    }
}

impl std::error::Error for RecoveryError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObjectIdentity {
    device: u64,
    inode: u64,
    size: i64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    links: u64,
}

impl ObjectIdentity {
    fn from_stat(stat: rustix::fs::Stat) -> Self {
        Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            size: stat.st_size,
            modified_seconds: stat.st_mtime,
            modified_nanoseconds: stat.st_mtime_nsec,
            links: stat.st_nlink as u64,
        }
    }

    fn same_entry(self, other: Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

/// Copy the exact selected simple-versioner archive to an exclusive new
/// sibling. This never calls `/rest/folder/versions/restore`, never replaces
/// the current file, and never removes an archive.
pub fn recover_selected_version_as_copy(
    request: RecoverAsCopyRequest<'_>,
) -> Result<RecoveryReceipt, RecoveryError> {
    recover_with_hooks(request, &mut || Ok(()), &mut || Ok(()), &mut || {})
}

// Keeping the failure boundaries injectable makes the descriptor and cleanup
// properties testable without a privileged filesystem fault injector. It is
// private; production callers always use the no-op hooks above.
fn recover_with_hooks(
    request: RecoverAsCopyRequest<'_>,
    after_source_open: &mut dyn FnMut() -> Result<(), RecoveryError>,
    after_write: &mut dyn FnMut() -> Result<(), RecoveryError>,
    after_sync: &mut dyn FnMut(),
) -> Result<RecoveryReceipt, RecoveryError> {
    if !request.versioner.is_pinned_default() {
        return Err(RecoveryError::UnsupportedVersioner);
    }
    if request.maximum_bytes == 0 || request.maximum_bytes > MAX_RECOVERY_FILE_BYTES {
        return Err(RecoveryError::UnsafeInput);
    }
    check_control(request.control)?;

    let components = parse_relative_path(&request.selected.path)?;
    let destination = validate_component(request.destination_basename)?;
    let receipt_destination: PathBuf = components[..components.len() - 1]
        .iter()
        .fold(PathBuf::new(), |mut path, component| {
            path.push(component);
            path
        })
        .join(destination);
    let version_time = parse_rfc3339(&request.selected.version_time)?;
    let mod_time = parse_rfc3339(&request.selected.mod_time)?;
    if request.selected.size > request.maximum_bytes
        || version_time.nanoseconds != 0
        || mod_time.nanoseconds != 0
    {
        return Err(RecoveryError::UnsafeInput);
    }
    let version_tag = version_time.local_tag();
    let archive_name = tagged_filename(
        components
            .last()
            .copied()
            .ok_or(RecoveryError::UnsafeInput)?,
        &version_tag,
    )?;

    let root = open_folder_root(request.folder_root)?;
    let root_identity = directory_identity(&root)?;
    let relative_parent = &components[..components.len() - 1];
    let destination_parent = open_relative_directory(&root, relative_parent)?;
    let versions = open_directory(&root, ".stversions")?;
    let archive_parent = open_relative_directory(&versions, relative_parent)?;
    let destination_parent_identity = directory_identity(&destination_parent)?;
    let archive_parent_identity = directory_identity(&archive_parent)?;

    let source_fd = openat(
        &archive_parent,
        archive_name.as_str(),
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| RecoveryError::SourceChanged)?;
    let source_stat = fstat(&source_fd).map_err(|_| RecoveryError::SourceChanged)?;
    validate_source(&source_stat, request.selected.size, mod_time.unix_seconds)?;
    let source_identity = ObjectIdentity::from_stat(source_stat);
    let mut source = File::from(source_fd);

    after_source_open()?;
    check_control(request.control)?;
    let destination_fd = openat(
        &destination_parent,
        destination,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            RecoveryError::DestinationExists
        } else {
            RecoveryError::IoFailure
        }
    })?;
    let destination_stat =
        fstat(&destination_fd).map_err(|_| RecoveryError::PersistenceUncertain)?;
    validate_new_destination(&destination_stat).map_err(|_| RecoveryError::PersistenceUncertain)?;
    let destination_identity = ObjectIdentity::from_stat(destination_stat);
    let mut destination_file = Some(File::from(destination_fd));

    let result = copy_and_persist(
        &mut source,
        destination_file.as_mut().expect("destination is held"),
        request.folder_root,
        &root,
        root_identity,
        relative_parent,
        &destination_parent,
        destination_parent_identity,
        destination,
        destination_identity,
        &archive_parent,
        archive_parent_identity,
        archive_name.as_str(),
        source_identity,
        request.selected.size,
        request.maximum_bytes,
        request.control,
        after_write,
        after_sync,
    );

    match result {
        Ok(receipt) => Ok(RecoveryReceipt {
            destination: receipt_destination,
            size: receipt.0,
            sha256: receipt.1,
        }),
        Err(error @ RecoveryError::PersistenceUncertain) => Err(error),
        Err(error) => {
            drop(destination_file.take());
            cleanup_owned_destination(&destination_parent, destination, destination_identity)?;
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_and_persist(
    source: &mut File,
    destination: &mut File,
    root_path: &Path,
    root: &OwnedFd,
    root_identity: ObjectIdentity,
    relative_parent: &[&str],
    destination_parent: &OwnedFd,
    destination_parent_identity: ObjectIdentity,
    destination_name: &str,
    destination_identity: ObjectIdentity,
    archive_parent: &OwnedFd,
    archive_parent_identity: ObjectIdentity,
    archive_name: &str,
    source_identity: ObjectIdentity,
    selected_size: u64,
    maximum_bytes: u64,
    control: &JobControl,
    after_write: &mut dyn FnMut() -> Result<(), RecoveryError>,
    after_sync: &mut dyn FnMut(),
) -> Result<(u64, [u8; 32]), RecoveryError> {
    let mut digest = Sha256::new();
    let mut bytes_copied = 0_u64;
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    loop {
        check_control(control)?;
        let remaining = selected_size.saturating_add(1).saturating_sub(bytes_copied);
        if remaining == 0 {
            return Err(RecoveryError::SourceChanged);
        }
        let limit = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = source
            .read(&mut buffer[..limit])
            .map_err(|_| RecoveryError::IoFailure)?;
        if read == 0 {
            break;
        }
        bytes_copied = bytes_copied.saturating_add(read as u64);
        if bytes_copied > selected_size || bytes_copied > maximum_bytes {
            return Err(RecoveryError::SourceChanged);
        }
        digest.update(&buffer[..read]);
        let mut written = 0;
        while written < read {
            check_control(control)?;
            let count = destination
                .write(&buffer[written..read])
                .map_err(|_| RecoveryError::IoFailure)?;
            if count == 0 {
                return Err(RecoveryError::IoFailure);
            }
            written += count;
        }
        after_write()?;
    }
    if bytes_copied != selected_size {
        return Err(RecoveryError::SourceChanged);
    }
    check_control(control)?;
    let destination_after = file_identity(destination)?;
    if file_identity(source)? != source_identity
        || named_identity(archive_parent, archive_name)? != source_identity
        || named_identity(destination_parent, destination_name)? != destination_after
        || !all_paths_are_unchanged(
            root_path,
            root,
            root_identity,
            relative_parent,
            destination_parent_identity,
            archive_parent_identity,
        )?
    {
        return Err(RecoveryError::SourceChanged);
    }
    if !destination_after.same_entry(destination_identity)
        || destination_after.size != selected_size as i64
        || destination_after.links != 1
    {
        return Err(RecoveryError::SourceChanged);
    }

    // A failing sync has an unknown persistence outcome. Preserve the name;
    // deleting it would itself be an unsupported rollback claim.
    destination
        .sync_all()
        .map_err(|_| RecoveryError::PersistenceUncertain)?;
    fsync(destination_parent).map_err(|_| RecoveryError::PersistenceUncertain)?;
    after_sync();
    let persisted_destination = named_identity(destination_parent, destination_name)
        .map_err(|_| RecoveryError::PersistenceUncertain)?;
    let persisted_source = named_identity(archive_parent, archive_name)
        .map_err(|_| RecoveryError::PersistenceUncertain)?;
    let paths_are_unchanged = all_paths_are_unchanged(
        root_path,
        root,
        root_identity,
        relative_parent,
        destination_parent_identity,
        archive_parent_identity,
    )
    .map_err(|_| RecoveryError::PersistenceUncertain)?;
    if persisted_destination != destination_after
        || persisted_source != source_identity
        || !paths_are_unchanged
    {
        return Err(RecoveryError::PersistenceUncertain);
    }
    Ok((bytes_copied, digest.finalize().into()))
}

fn check_control(control: &JobControl) -> Result<(), RecoveryError> {
    if control.state() == JobState::Running {
        Ok(())
    } else {
        Err(RecoveryError::Cancelled)
    }
}

fn open_folder_root(path: &Path) -> Result<OwnedFd, RecoveryError> {
    if !path.is_absolute() {
        return Err(RecoveryError::UnsafeInput);
    }
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| RecoveryError::SourceChanged)
}

fn directory_identity(descriptor: &OwnedFd) -> Result<ObjectIdentity, RecoveryError> {
    let stat = fstat(descriptor).map_err(|_| RecoveryError::SourceChanged)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
        return Err(RecoveryError::SourceChanged);
    }
    Ok(ObjectIdentity::from_stat(stat))
}

fn open_directory(parent: &OwnedFd, component: &str) -> Result<OwnedFd, RecoveryError> {
    openat(
        parent,
        component,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| RecoveryError::SourceChanged)
}

fn open_relative_directory(
    parent: &OwnedFd,
    components: &[&str],
) -> Result<OwnedFd, RecoveryError> {
    let mut current = fcntl_dupfd_cloexec(parent, 0).map_err(|_| RecoveryError::SourceChanged)?;
    for component in components {
        current = open_directory(&current, component)?;
    }
    Ok(current)
}

fn validate_source(
    stat: &rustix::fs::Stat,
    expected_size: u64,
    expected_mtime: i64,
) -> Result<(), RecoveryError> {
    // Syncthing v2.1.3 `retrieveVersions` stores
    // `f.ModTime().Truncate(time.Second)` in `FileVersion.ModTime`, so the
    // private API exposes whole-second metadata. We reject fractional API
    // values at selection and compare the source's seconds here; its retained
    // nanoseconds remain part of the pre/post descriptor identity below.
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_size < 0
        || stat.st_size as u64 != expected_size
        || stat.st_mtime != expected_mtime
    {
        return Err(RecoveryError::SourceChanged);
    }
    Ok(())
}

fn validate_new_destination(stat: &rustix::fs::Stat) -> Result<(), RecoveryError> {
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
        || stat.st_nlink != 1
        || stat.st_size != 0
    {
        return Err(RecoveryError::IoFailure);
    }
    Ok(())
}

fn file_identity(file: &File) -> Result<ObjectIdentity, RecoveryError> {
    fstat(file)
        .map(ObjectIdentity::from_stat)
        .map_err(|_| RecoveryError::SourceChanged)
}

fn named_identity(parent: &OwnedFd, name: &str) -> Result<ObjectIdentity, RecoveryError> {
    statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map(ObjectIdentity::from_stat)
        .map_err(|_| RecoveryError::SourceChanged)
}

fn all_paths_are_unchanged(
    root_path: &Path,
    root: &OwnedFd,
    expected_root: ObjectIdentity,
    relative_parent: &[&str],
    expected_destination_parent: ObjectIdentity,
    expected_archive_parent: ObjectIdentity,
) -> Result<bool, RecoveryError> {
    let reopened_root = open_folder_root(root_path)?;
    if !directory_identity(root)?.same_entry(expected_root)
        || !directory_identity(&reopened_root)?.same_entry(expected_root)
    {
        return Ok(false);
    }
    let reopened_destination = open_relative_directory(&reopened_root, relative_parent)?;
    let reopened_versions = open_directory(&reopened_root, ".stversions")?;
    let reopened_archive = open_relative_directory(&reopened_versions, relative_parent)?;
    Ok(
        directory_identity(&reopened_destination)?.same_entry(expected_destination_parent)
            && directory_identity(&reopened_archive)?.same_entry(expected_archive_parent),
    )
}

fn cleanup_owned_destination(
    parent: &OwnedFd,
    name: &str,
    expected: ObjectIdentity,
) -> Result<(), RecoveryError> {
    let named = named_identity(parent, name).map_err(|_| RecoveryError::PersistenceUncertain)?;
    if !named.same_entry(expected) {
        return Err(RecoveryError::PersistenceUncertain);
    }
    unlinkat(parent, name, AtFlags::empty()).map_err(|_| RecoveryError::PersistenceUncertain)?;
    fsync(parent).map_err(|_| RecoveryError::PersistenceUncertain)
}

fn parse_relative_path(path: &str) -> Result<Vec<&str>, RecoveryError> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
    {
        return Err(RecoveryError::UnsafeInput);
    }
    let components: Vec<_> = path.split('/').collect();
    if components.len() > MAX_COMPONENTS {
        return Err(RecoveryError::UnsafeInput);
    }
    for component in &components {
        validate_component(component)?;
    }
    Ok(components)
}

fn validate_component(component: &str) -> Result<&str, RecoveryError> {
    if component.is_empty()
        || component.len() > MAX_COMPONENT_BYTES
        || component == "."
        || component == ".."
        || component.contains('/')
        || component.as_bytes().contains(&0)
        || component.chars().any(char::is_control)
        || is_reserved_private_name(component)
    {
        return Err(RecoveryError::UnsafeInput);
    }
    Ok(component)
}

fn is_reserved_private_name(component: &str) -> bool {
    matches!(component, ".stversions" | ".stfolder" | ".stignore")
        || component.starts_with(".syncthing.")
        || component.starts_with("~syncthing~")
}

fn tagged_filename(name: &str, tag: &str) -> Result<String, RecoveryError> {
    let extension_start = name.rfind('.').unwrap_or(name.len());
    let (stem, extension) = name.split_at(extension_start);
    let tagged = format!("{stem}~{tag}{extension}");
    validate_component(&tagged)?;
    Ok(tagged)
}

#[derive(Clone, Copy, Debug)]
struct Rfc3339Time {
    unix_seconds: i64,
    nanoseconds: u32,
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

impl Rfc3339Time {
    fn local_tag(self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Strict enough for Go's RFC3339 JSON times. The date/time fields are kept
/// as presented because Syncthing v2.1.3 makes the simple-versioner tag from
/// its local `versionTime`; controller code must query and recover against the
/// same local engine configuration.
fn parse_rfc3339(value: &str) -> Result<Rfc3339Time, RecoveryError> {
    if value.is_empty() || value.len() > MAX_TIMESTAMP_BYTES || !value.is_ascii() {
        return Err(RecoveryError::UnsafeInput);
    }
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return Err(RecoveryError::UnsafeInput);
    }
    let year = decimal_u16(&bytes[0..4])?;
    let month = decimal_u8(&bytes[5..7])?;
    let day = decimal_u8(&bytes[8..10])?;
    let hour = decimal_u8(&bytes[11..13])?;
    let minute = decimal_u8(&bytes[14..16])?;
    let second = decimal_u8(&bytes[17..19])?;
    if month == 0
        || month > 12
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(RecoveryError::UnsafeInput);
    }
    let mut cursor = 19;
    let mut nanoseconds = 0_u32;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == start || cursor - start > 9 {
            return Err(RecoveryError::UnsafeInput);
        }
        nanoseconds = decimal_u32(&bytes[start..cursor])?;
        for _ in cursor - start..9 {
            nanoseconds *= 10;
        }
    }
    let offset_seconds = match bytes.get(cursor) {
        Some(b'Z') if cursor + 1 == bytes.len() => 0_i64,
        Some(sign @ (b'+' | b'-')) if cursor + 6 == bytes.len() && bytes[cursor + 3] == b':' => {
            let offset_hour = decimal_u8(&bytes[cursor + 1..cursor + 3])?;
            let offset_minute = decimal_u8(&bytes[cursor + 4..cursor + 6])?;
            if offset_hour > 23 || offset_minute > 59 {
                return Err(RecoveryError::UnsafeInput);
            }
            let seconds = i64::from(offset_hour) * 3600 + i64::from(offset_minute) * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return Err(RecoveryError::UnsafeInput),
    };
    let days = days_before_year(year) + days_before_month(year, month) + i64::from(day - 1);
    let unix_seconds = days
        .checked_mul(86_400)
        .and_then(|value| {
            value.checked_add(i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second))
        })
        .and_then(|value| value.checked_sub(offset_seconds))
        .and_then(|value| value.checked_sub(719_528_i64 * 86_400))
        .ok_or(RecoveryError::UnsafeInput)?;
    Ok(Rfc3339Time {
        unix_seconds,
        nanoseconds,
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

fn decimal_u8(bytes: &[u8]) -> Result<u8, RecoveryError> {
    let mut value = 0_u8;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(RecoveryError::UnsafeInput);
        }
        value = value
            .checked_mul(10)
            .and_then(|item| item.checked_add(byte - b'0'))
            .ok_or(RecoveryError::UnsafeInput)?;
    }
    Ok(value)
}

fn decimal_u16(bytes: &[u8]) -> Result<u16, RecoveryError> {
    let mut value = 0_u16;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(RecoveryError::UnsafeInput);
        }
        value = value
            .checked_mul(10)
            .and_then(|item| item.checked_add(u16::from(byte - b'0')))
            .ok_or(RecoveryError::UnsafeInput)?;
    }
    Ok(value)
}

fn decimal_u32(bytes: &[u8]) -> Result<u32, RecoveryError> {
    let mut value = 0_u32;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err(RecoveryError::UnsafeInput);
        }
        value = value
            .checked_mul(10)
            .and_then(|item| item.checked_add(u32::from(byte - b'0')))
            .ok_or(RecoveryError::UnsafeInput)?;
    }
    Ok(value)
}

const fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

const fn days_before_year(year: u16) -> i64 {
    let year = year as i64;
    365 * year + (year - 1) / 4 - (year - 1) / 100 + (year - 1) / 400 + 1
}

const fn days_before_month(year: u16, month: u8) -> i64 {
    const DAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    DAYS[(month - 1) as usize]
        + if month > 2 && is_leap_year(year) {
            1
        } else {
            0
        }
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
