use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use covalent_protocol::{
    BackupId, ConflictPolicy, DeviceId, EntryKind, EntryMetadata, Manifest, ManifestEntry,
    RelativePath,
};
use serde::{Deserialize, Serialize};

use crate::engine::JobControl;
use crate::replication::ProviderFailure;
use crate::{
    AuthorizedRoot, BackupKey, ChunkStore, CoreError, DeviceIdentity, PublicIdentity,
    ReplicationScheduler,
};

const RESTORE_CHECKPOINT_SCHEMA_VERSION: u16 = 1;
const RESTORE_PLAN_SIGNATURE_DOMAIN: &[u8] = b"covalent/restore-plan/v1";

/// Immutable restore preview options.
#[derive(Clone, Debug)]
pub struct RestoreOptions {
    /// Existing-target policy.
    pub conflict_policy: ConflictPolicy,
    /// Exact selected entries; empty restores the complete manifest. Selecting a
    /// directory includes its descendants.
    pub selected_paths: BTreeSet<RelativePath>,
    /// Stable resumable job identifier.
    pub job_id: String,
}

impl RestoreOptions {
    /// Restores every manifest entry and fails on conflicts.
    #[must_use]
    pub fn all(job_id: impl Into<String>) -> Self {
        Self {
            conflict_policy: ConflictPolicy::Fail,
            selected_paths: BTreeSet::new(),
            job_id: job_id.into(),
        }
    }
}

/// Planned action visible before any write.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewAction {
    /// Create a new regular file.
    CreateFile,
    /// Create a missing directory.
    CreateDirectory,
    /// Reuse a real existing directory.
    KeepDirectory,
    /// Leave an existing regular file unchanged.
    SkipFile,
    /// Atomically replace an existing regular file.
    ReplaceFile,
    /// Create a deterministic non-conflicting sibling.
    RenameFile,
}

/// One immutable preview mapping.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestorePreviewEntry {
    /// Original manifest path.
    pub source_path: RelativePath,
    /// Final relative path under the frozen authorized root.
    pub destination_path: RelativePath,
    /// Manifest kind.
    pub kind: EntryKind,
    /// Exact action approved by preview.
    pub action: PreviewAction,
}

/// Signed immutable restore plan binding root, manifest, selection, and conflict outcomes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestorePlan {
    /// Logical backup.
    pub backup_id: BackupId,
    /// Snapshot covered by the plan.
    pub snapshot_id: String,
    /// Canonical root path selected locally by the user.
    pub authorized_root: String,
    /// Root filesystem device token where available.
    pub root_device: u64,
    /// Root inode/file-index token where available.
    pub root_inode: u64,
    /// BLAKE3 digest of the exact decrypted manifest.
    pub manifest_digest: String,
    /// Existing-target policy.
    pub conflict_policy: ConflictPolicy,
    /// Stable resumable job identifier.
    pub job_id: String,
    /// Fully resolved actions.
    pub entries: Vec<RestorePreviewEntry>,
    /// Digest over every preceding field.
    pub plan_digest: String,
    /// Device that signed the preview.
    pub signer_device_id: DeviceId,
    /// Ed25519 signature over the plan digest.
    pub signature: String,
}

/// Executed restore outcome.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestoreReport {
    /// Regular files durably committed.
    pub files_restored: usize,
    /// Directories created.
    pub directories_created: usize,
    /// Existing files intentionally skipped.
    pub files_skipped: usize,
    /// Plaintext bytes written, excluding sparse holes.
    pub bytes_written: u64,
    /// Provider failures rejected while another intact copy succeeded.
    pub rejected_provider_copies: Vec<ProviderFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreCheckpoint {
    schema_version: u16,
    plan_digest: String,
    completed: BTreeMap<RelativePath, String>,
}

pub(crate) fn preview_restore(
    manifest: &Manifest,
    authorized_root: &AuthorizedRoot,
    options: &RestoreOptions,
    signer: &DeviceIdentity,
) -> Result<RestorePlan, CoreError> {
    manifest.validate()?;
    validate_restore_options(options)?;
    let (root_device, root_inode) = filesystem_identity(authorized_root.canonical_path())?;
    let mut entries = Vec::new();
    for entry in planned_manifest_entries(manifest, &options.selected_paths)? {
        let destination = authorized_root.resolve(&entry.path)?;
        let (destination_path, action) = preview_action(
            authorized_root,
            &entry,
            &destination,
            options.conflict_policy,
        )?;
        entries.push(RestorePreviewEntry {
            source_path: entry.path,
            destination_path,
            kind: entry.kind,
            action,
        });
    }
    let authorized_root_string = authorized_root
        .canonical_path()
        .to_str()
        .ok_or_else(|| {
            CoreError::InvalidAuthorizedRoot(authorized_root.canonical_path().to_path_buf())
        })?
        .to_owned();
    let manifest_digest = blake3::hash(&serde_json::to_vec(manifest)?)
        .to_hex()
        .to_string();
    let mut plan = RestorePlan {
        backup_id: manifest.backup_id,
        snapshot_id: manifest.snapshot_id.clone(),
        authorized_root: authorized_root_string,
        root_device,
        root_inode,
        manifest_digest,
        conflict_policy: options.conflict_policy,
        job_id: options.job_id.clone(),
        entries,
        plan_digest: String::new(),
        signer_device_id: signer.device_id(),
        signature: String::new(),
    };
    plan.plan_digest = compute_plan_digest(&plan)?;
    plan.signature = signer.sign(RESTORE_PLAN_SIGNATURE_DOMAIN, plan.plan_digest.as_bytes());
    Ok(plan)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_restore(
    manifest: &Manifest,
    plan: &RestorePlan,
    authorized_root: &AuthorizedRoot,
    key: &BackupKey,
    scheduler: &ReplicationScheduler,
    store: &ChunkStore,
    signer: &PublicIdentity,
    local_device_id: DeviceId,
    control: &JobControl,
) -> Result<RestoreReport, CoreError> {
    validate_plan(manifest, plan, authorized_root, signer)?;
    let mut checkpoint = load_restore_checkpoint(store, plan)?;
    let safe_root = SafeRestoreRoot::open(authorized_root)?;
    let manifest_entries: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let mut report = RestoreReport::default();
    let mut allowed_providers = BTreeMap::<String, BTreeSet<DeviceId>>::new();
    for reference in manifest.entries.iter().flat_map(|entry| &entry.chunks) {
        allowed_providers
            .entry(reference.opaque_locator.clone())
            .or_default()
            .insert(local_device_id);
    }
    for (provider_id, locators) in &manifest.provider_acknowledgements {
        for locator in locators {
            allowed_providers
                .entry(locator.clone())
                .or_default()
                .insert(*provider_id);
        }
    }
    let mut deferred_directories = Vec::new();

    for preview in &plan.entries {
        control.check()?;
        let synthetic;
        let entry = if let Some(entry) = manifest_entries.get(&preview.source_path) {
            *entry
        } else if preview.kind == EntryKind::Directory
            && plan
                .entries
                .iter()
                .any(|candidate| is_descendant(&candidate.source_path, &preview.source_path))
        {
            synthetic = implicit_directory(preview.source_path.clone());
            &synthetic
        } else {
            return Err(CoreError::RestorePlanMismatch);
        };
        if let Some(expected_digest) = checkpoint.completed.get(&preview.destination_path) {
            if safe_root.completed_entry_matches(
                &preview.destination_path,
                preview.kind,
                expected_digest,
            )? {
                if preview.action == PreviewAction::CreateDirectory {
                    deferred_directories.push((preview.destination_path.clone(), entry.clone()));
                }
                continue;
            }
            return Err(CoreError::RestorePlanMismatch);
        }
        match preview.action {
            PreviewAction::CreateDirectory => {
                safe_root.ensure_directory(&preview.destination_path, entry)?;
                report.directories_created += 1;
                checkpoint
                    .completed
                    .insert(preview.destination_path.clone(), "directory".to_owned());
                deferred_directories.push((preview.destination_path.clone(), (*entry).clone()));
            }
            PreviewAction::KeepDirectory => {
                safe_root.verify_directory(&preview.destination_path)?;
                checkpoint
                    .completed
                    .insert(preview.destination_path.clone(), "directory".to_owned());
            }
            PreviewAction::SkipFile => {
                safe_root.verify_regular_file(&preview.destination_path)?;
                report.files_skipped += 1;
                let digest = safe_root.digest_regular_file(&preview.destination_path)?;
                checkpoint
                    .completed
                    .insert(preview.destination_path.clone(), digest);
            }
            PreviewAction::CreateFile | PreviewAction::ReplaceFile | PreviewAction::RenameFile => {
                let outcome = safe_root.write_file(
                    entry,
                    &preview.destination_path,
                    preview.action,
                    manifest.backup_id,
                    key,
                    scheduler,
                    &allowed_providers,
                    control,
                )?;
                report.files_restored += 1;
                report.bytes_written = report.bytes_written.saturating_add(outcome.bytes_written);
                report.rejected_provider_copies.extend(outcome.failures);
                checkpoint
                    .completed
                    .insert(preview.destination_path.clone(), outcome.plaintext_digest);
            }
        }
        store.write_checkpoint(&plan.job_id, &serde_json::to_vec(&checkpoint)?)?;
    }
    for (path, entry) in deferred_directories.into_iter().rev() {
        safe_root.apply_directory_metadata(&path, &entry)?;
    }
    store.remove_checkpoint(&plan.job_id)?;
    report.rejected_provider_copies.sort_by(|left, right| {
        (left.provider_id, &left.locator).cmp(&(right.provider_id, &right.locator))
    });
    Ok(report)
}

fn validate_restore_options(options: &RestoreOptions) -> Result<(), CoreError> {
    if options.job_id.is_empty()
        || options.job_id.len() > 128
        || !options
            .job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidState("invalid restore job id".to_owned()));
    }
    Ok(())
}

fn validate_plan(
    manifest: &Manifest,
    plan: &RestorePlan,
    authorized_root: &AuthorizedRoot,
    signer: &PublicIdentity,
) -> Result<(), CoreError> {
    manifest.validate()?;
    validate_restore_options(&RestoreOptions {
        conflict_policy: plan.conflict_policy,
        selected_paths: BTreeSet::new(),
        job_id: plan.job_id.clone(),
    })?;
    if plan.backup_id != manifest.backup_id
        || plan.snapshot_id != manifest.snapshot_id
        || plan.signer_device_id != signer.device_id
        || plan.authorized_root != authorized_root.canonical_path().to_string_lossy()
        || plan.manifest_digest
            != blake3::hash(&serde_json::to_vec(manifest)?)
                .to_hex()
                .to_string()
        || plan.plan_digest != compute_plan_digest(plan)?
    {
        return Err(CoreError::RestorePlanMismatch);
    }
    let (device, inode) = filesystem_identity(authorized_root.canonical_path())?;
    if (device, inode) != (plan.root_device, plan.root_inode) {
        return Err(CoreError::RestorePlanMismatch);
    }
    let manifest_entries: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|entry| (&entry.path, entry.kind))
        .collect();
    let mut sources = BTreeSet::new();
    let mut destinations = BTreeSet::new();
    for preview in &plan.entries {
        if !sources.insert(preview.source_path.clone())
            || !destinations.insert(preview.destination_path.clone())
            || manifest_entries
                .get(&preview.source_path)
                .is_some_and(|kind| *kind != preview.kind)
            || (!manifest_entries.contains_key(&preview.source_path)
                && (preview.kind != EntryKind::Directory
                    || !plan.entries.iter().any(|candidate| {
                        is_descendant(&candidate.source_path, &preview.source_path)
                    })))
            || !action_matches_kind_and_policy(preview, plan.conflict_policy)
        {
            return Err(CoreError::RestorePlanMismatch);
        }
    }
    signer.verify(
        RESTORE_PLAN_SIGNATURE_DOMAIN,
        plan.plan_digest.as_bytes(),
        &plan.signature,
    )
}

fn action_matches_kind_and_policy(preview: &RestorePreviewEntry, policy: ConflictPolicy) -> bool {
    match preview.kind {
        EntryKind::Directory => {
            preview.destination_path == preview.source_path
                && matches!(
                    preview.action,
                    PreviewAction::CreateDirectory | PreviewAction::KeepDirectory
                )
        }
        EntryKind::File => match preview.action {
            PreviewAction::CreateFile => preview.destination_path == preview.source_path,
            PreviewAction::SkipFile => {
                policy == ConflictPolicy::Skip && preview.destination_path == preview.source_path
            }
            PreviewAction::ReplaceFile => {
                policy == ConflictPolicy::Replace && preview.destination_path == preview.source_path
            }
            PreviewAction::RenameFile => {
                policy == ConflictPolicy::Rename && preview.destination_path != preview.source_path
            }
            PreviewAction::CreateDirectory | PreviewAction::KeepDirectory => false,
        },
    }
}

fn is_descendant(path: &RelativePath, ancestor: &RelativePath) -> bool {
    path.as_str()
        .strip_prefix(ancestor.as_str())
        .is_some_and(|suffix| suffix.starts_with('/'))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UnsignedPlan<'a> {
    backup_id: BackupId,
    snapshot_id: &'a str,
    authorized_root: &'a str,
    root_device: u64,
    root_inode: u64,
    manifest_digest: &'a str,
    conflict_policy: ConflictPolicy,
    job_id: &'a str,
    entries: &'a [RestorePreviewEntry],
    signer_device_id: DeviceId,
}

fn compute_plan_digest(plan: &RestorePlan) -> Result<String, CoreError> {
    Ok(blake3::hash(&serde_json::to_vec(&UnsignedPlan {
        backup_id: plan.backup_id,
        snapshot_id: &plan.snapshot_id,
        authorized_root: &plan.authorized_root,
        root_device: plan.root_device,
        root_inode: plan.root_inode,
        manifest_digest: &plan.manifest_digest,
        conflict_policy: plan.conflict_policy,
        job_id: &plan.job_id,
        entries: &plan.entries,
        signer_device_id: plan.signer_device_id,
    })?)
    .to_hex()
    .to_string())
}

fn preview_action(
    authorized_root: &AuthorizedRoot,
    entry: &ManifestEntry,
    destination: &Path,
    policy: ConflictPolicy,
) -> Result<(RelativePath, PreviewAction), CoreError> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(CoreError::SymlinkTraversal(destination.to_path_buf()));
            }
            match entry.kind {
                EntryKind::Directory if metadata.is_dir() => {
                    Ok((entry.path.clone(), PreviewAction::KeepDirectory))
                }
                EntryKind::Directory => Err(CoreError::RestoreConflict(destination.to_path_buf())),
                EntryKind::File if !metadata.is_file() => {
                    Err(CoreError::RestoreConflict(destination.to_path_buf()))
                }
                EntryKind::File => match policy {
                    ConflictPolicy::Fail => {
                        Err(CoreError::RestoreConflict(destination.to_path_buf()))
                    }
                    ConflictPolicy::Skip => Ok((entry.path.clone(), PreviewAction::SkipFile)),
                    ConflictPolicy::Replace => Ok((entry.path.clone(), PreviewAction::ReplaceFile)),
                    ConflictPolicy::Rename => {
                        let renamed = find_renamed_path(authorized_root, &entry.path)?;
                        Ok((renamed, PreviewAction::RenameFile))
                    }
                },
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((
            entry.path.clone(),
            if entry.kind == EntryKind::Directory {
                PreviewAction::CreateDirectory
            } else {
                PreviewAction::CreateFile
            },
        )),
        Err(source) => Err(CoreError::Io {
            operation: "preview restore destination",
            path: destination.to_path_buf(),
            source,
        }),
    }
}

fn find_renamed_path(
    authorized_root: &AuthorizedRoot,
    original: &RelativePath,
) -> Result<RelativePath, CoreError> {
    let (parent, file_name) = original
        .as_str()
        .rsplit_once('/')
        .map_or((None, original.as_str()), |(parent, name)| {
            (Some(parent), name)
        });
    for index in 1..=10_000 {
        let candidate = match parent {
            Some(parent) => format!("{parent}/{file_name}.covalent-restored-{index}"),
            None => format!("{file_name}.covalent-restored-{index}"),
        };
        let relative = RelativePath::new(candidate)?;
        let destination = authorized_root.resolve(&relative)?;
        match fs::symlink_metadata(&destination) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(relative),
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CoreError::SymlinkTraversal(destination));
            }
            Ok(_) => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "find renamed restore destination",
                    path: destination,
                    source,
                });
            }
        }
    }
    Err(CoreError::ResourceLimit("restore rename attempts"))
}

fn is_selected(path: &RelativePath, selected: &BTreeSet<RelativePath>) -> bool {
    selected.is_empty()
        || selected
            .iter()
            .any(|selection| path == selection || is_descendant(path, selection))
}

fn planned_manifest_entries(
    manifest: &Manifest,
    selected: &BTreeSet<RelativePath>,
) -> Result<Vec<ManifestEntry>, CoreError> {
    if !selected.is_empty()
        && selected.iter().any(|selection| {
            !manifest
                .entries
                .iter()
                .any(|entry| &entry.path == selection)
        })
    {
        return Err(CoreError::InvalidState(
            "restore selection is not present in the snapshot".to_owned(),
        ));
    }
    let mut planned = BTreeMap::new();
    for entry in &manifest.entries {
        if !is_selected(&entry.path, selected) {
            continue;
        }
        planned.insert(entry.path.clone(), entry.clone());
        let components: Vec<_> = entry.path.components().collect();
        for end in 1..components.len() {
            let parent = RelativePath::new(components[..end].join("/"))?;
            planned
                .entry(parent.clone())
                .or_insert_with(|| implicit_directory(parent));
        }
    }
    Ok(planned.into_values().collect())
}

fn implicit_directory(path: RelativePath) -> ManifestEntry {
    ManifestEntry {
        path,
        kind: EntryKind::Directory,
        length: 0,
        chunks: Vec::new(),
        metadata: EntryMetadata::default(),
        sparse_extents: Vec::new(),
    }
}

fn load_restore_checkpoint(
    store: &ChunkStore,
    plan: &RestorePlan,
) -> Result<RestoreCheckpoint, CoreError> {
    if let Some(bytes) = store.read_checkpoint(&plan.job_id)? {
        let checkpoint: RestoreCheckpoint = serde_json::from_slice(&bytes)?;
        if checkpoint.schema_version != RESTORE_CHECKPOINT_SCHEMA_VERSION
            || checkpoint.plan_digest != plan.plan_digest
        {
            return Err(CoreError::RestorePlanMismatch);
        }
        return Ok(checkpoint);
    }
    Ok(RestoreCheckpoint {
        schema_version: RESTORE_CHECKPOINT_SCHEMA_VERSION,
        plan_digest: plan.plan_digest.clone(),
        completed: BTreeMap::new(),
    })
}

fn digest_file_handle(mut file: &File, path: &Path) -> Result<String, CoreError> {
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| CoreError::Io {
            operation: "verify restored file",
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(unix)]
fn filesystem_identity(path: &Path) -> Result<(u64, u64), CoreError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path).map_err(|source| CoreError::Io {
        operation: "identify authorized root",
        path: path.to_path_buf(),
        source,
    })?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn filesystem_identity(path: &Path) -> Result<(u64, u64), CoreError> {
    let canonical = fs::canonicalize(path).map_err(|source| CoreError::Io {
        operation: "identify authorized root",
        path: path.to_path_buf(),
        source,
    })?;
    let digest = blake3::hash(canonical.to_string_lossy().as_bytes());
    Ok((
        u64::from_be_bytes(digest.as_bytes()[..8].try_into().expect("length")),
        u64::from_be_bytes(digest.as_bytes()[8..16].try_into().expect("length")),
    ))
}

struct FileWriteOutcome {
    plaintext_digest: String,
    bytes_written: u64,
    failures: Vec<ProviderFailure>,
}

#[cfg(unix)]
struct SafeRestoreRoot {
    canonical: PathBuf,
    root_descriptor: std::os::fd::OwnedFd,
    root_device: u64,
    root_inode: u64,
}

#[cfg(unix)]
impl SafeRestoreRoot {
    fn open(root: &AuthorizedRoot) -> Result<Self, CoreError> {
        use rustix::fs::{Mode, OFlags, fstat, open};
        let descriptor = open(
            root.canonical_path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            restore_os_error("open authorized root handle", root.canonical_path(), error)
        })?;
        let stat = fstat(&descriptor).map_err(|error| {
            restore_os_error(
                "inspect authorized root handle",
                root.canonical_path(),
                error,
            )
        })?;
        Ok(Self {
            canonical: root.canonical_path().to_path_buf(),
            root_device: stat.st_dev as u64,
            root_inode: stat.st_ino as u64,
            root_descriptor: descriptor,
        })
    }

    fn ensure_directory(
        &self,
        path: &RelativePath,
        entry: &ManifestEntry,
    ) -> Result<(), CoreError> {
        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        let descriptor = self.open_directory(path.components(), true)?;
        let _ = entry;
        rustix::fs::fsync(&descriptor)
            .map_err(|error| restore_os_error("sync restored directory", &self.canonical, error))
    }

    fn verify_directory(&self, path: &RelativePath) -> Result<(), CoreError> {
        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        self.open_directory(path.components(), false).map(|_| ())
    }

    fn verify_regular_file(&self, path: &RelativePath) -> Result<(), CoreError> {
        use rustix::fs::{AtFlags, FileType, statat};
        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        let (parent, name) = self.open_parent(path, false)?;
        let stat = statat(&parent, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|error| restore_os_error("inspect restore file", &self.canonical, error))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(CoreError::RestoreConflict(
                self.canonical.join(path.as_str()),
            ));
        }
        Ok(())
    }

    fn digest_regular_file(&self, path: &RelativePath) -> Result<String, CoreError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        let (parent, name) = self.open_parent(path, false)?;
        let descriptor = openat(
            &parent,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| {
            restore_os_error(
                "open restored file for verification",
                &self.canonical,
                error,
            )
        })?;
        let stat = fstat(&descriptor).map_err(|error| {
            restore_os_error("inspect restored file handle", &self.canonical, error)
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Err(CoreError::RestoreConflict(
                self.canonical.join(path.as_str()),
            ));
        }
        digest_file_handle(&File::from(descriptor), &self.canonical.join(path.as_str()))
    }

    fn completed_entry_matches(
        &self,
        path: &RelativePath,
        kind: EntryKind,
        expected_digest: &str,
    ) -> Result<bool, CoreError> {
        match kind {
            EntryKind::Directory => match self.verify_directory(path) {
                Ok(()) => Ok(expected_digest == "directory"),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            },
            EntryKind::File => match self.digest_regular_file(path) {
                Ok(digest) => Ok(digest == expected_digest),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_file(
        &self,
        entry: &ManifestEntry,
        destination: &RelativePath,
        action: PreviewAction,
        backup_id: BackupId,
        key: &BackupKey,
        scheduler: &ReplicationScheduler,
        allowed_providers: &BTreeMap<String, BTreeSet<DeviceId>>,
        control: &JobControl,
    ) -> Result<FileWriteOutcome, CoreError> {
        use rustix::fs::{
            AtFlags, FileType, Mode, OFlags, RenameFlags, fsync, openat, renameat, renameat_with,
            statat, unlinkat,
        };
        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        let (parent, final_name) = self.open_parent(destination, true)?;
        match statat(&parent, final_name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if action != PreviewAction::ReplaceFile || kind != FileType::RegularFile {
                    return Err(if kind == FileType::Symlink {
                        CoreError::SymlinkTraversal(self.canonical.join(destination.as_str()))
                    } else {
                        CoreError::RestoreConflict(self.canonical.join(destination.as_str()))
                    });
                }
            }
            Err(rustix::io::Errno::NOENT) => {
                if action == PreviewAction::ReplaceFile {
                    return Err(CoreError::RestorePlanMismatch);
                }
            }
            Err(error) => {
                return Err(restore_os_error(
                    "inspect restore destination",
                    &self.canonical,
                    error,
                ));
            }
        }
        let temporary_name = format!(".covalent-{}.tmp", uuid::Uuid::new_v4());
        let temporary_descriptor = openat(
            &parent,
            temporary_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| restore_os_error("create restore staging file", &self.canonical, error))?;
        let mut file = File::from(temporary_descriptor);
        let write_result = self.write_plaintext(
            &mut file,
            entry,
            backup_id,
            key,
            scheduler,
            allowed_providers,
            control,
        );
        let mut outcome = match write_result {
            Ok(outcome) => outcome,
            Err(error) => {
                drop(file);
                let _ = unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
                return Err(error);
            }
        };
        self.apply_file_metadata(&file, entry)?;
        file.sync_all().map_err(|source| CoreError::Io {
            operation: "sync restore staging file",
            path: self.canonical.join(destination.as_str()),
            source,
        })?;
        drop(file);

        let (reopened_parent, reopened_name) = self.open_parent(destination, false)?;
        let original_stat = rustix::fs::fstat(&parent)
            .map_err(|error| restore_os_error("inspect restore parent", &self.canonical, error))?;
        let reopened_stat = rustix::fs::fstat(&reopened_parent)
            .map_err(|error| restore_os_error("recheck restore parent", &self.canonical, error))?;
        if reopened_name != final_name
            || original_stat.st_dev != reopened_stat.st_dev
            || original_stat.st_ino != reopened_stat.st_ino
            || !self.root_is_unchanged()?
        {
            let _ = unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
            return Err(CoreError::RestorePlanMismatch);
        }

        let rename_result = if action == PreviewAction::ReplaceFile {
            renameat(
                &parent,
                temporary_name.as_str(),
                &parent,
                final_name.as_str(),
            )
        } else {
            renameat_with(
                &parent,
                temporary_name.as_str(),
                &parent,
                final_name.as_str(),
                RenameFlags::NOREPLACE,
            )
        };
        if let Err(error) = rename_result {
            let _ = unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
            return Err(restore_os_error(
                "atomically commit restored file",
                &self.canonical.join(destination.as_str()),
                error,
            ));
        }
        fsync(&parent).map_err(|error| {
            restore_os_error(
                "sync restored file parent",
                &self.canonical.join(destination.as_str()),
                error,
            )
        })?;
        outcome.failures.sort_by_key(|failure| failure.provider_id);
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_plaintext(
        &self,
        file: &mut File,
        entry: &ManifestEntry,
        backup_id: BackupId,
        key: &BackupKey,
        scheduler: &ReplicationScheduler,
        allowed_providers: &BTreeMap<String, BTreeSet<DeviceId>>,
        control: &JobControl,
    ) -> Result<FileWriteOutcome, CoreError> {
        file.set_len(entry.length).map_err(|source| CoreError::Io {
            operation: "size restore staging file",
            path: self.canonical.join(entry.path.as_str()),
            source,
        })?;
        let extents: Vec<_> = if entry.metadata.sparse {
            entry
                .sparse_extents
                .iter()
                .map(|extent| (extent.offset, extent.length))
                .collect()
        } else if entry.length == 0 {
            Vec::new()
        } else {
            vec![(0, entry.length)]
        };
        let mut reference_index = 0_usize;
        let mut full_hasher = blake3::Hasher::new();
        let zero_buffer = [0_u8; 64 * 1_024];
        let mut logical_position = 0_u64;
        let mut bytes_written = 0_u64;
        let mut failures = Vec::new();
        for (offset, length) in extents {
            hash_zeros(
                &mut full_hasher,
                offset.saturating_sub(logical_position),
                &zero_buffer,
            );
            file.seek(SeekFrom::Start(offset))
                .map_err(|source| CoreError::Io {
                    operation: "seek restore sparse extent",
                    path: self.canonical.join(entry.path.as_str()),
                    source,
                })?;
            let mut remaining = length;
            while remaining > 0 {
                control.check()?;
                let reference = entry
                    .chunks
                    .get(reference_index)
                    .ok_or(CoreError::RestorePlanMismatch)?;
                if u64::from(reference.plaintext_length) > remaining {
                    return Err(CoreError::RestorePlanMismatch);
                }
                let fetched = scheduler.fetch_plaintext(
                    reference,
                    backup_id,
                    key,
                    allowed_providers
                        .get(&reference.opaque_locator)
                        .ok_or(CoreError::RestorePlanMismatch)?,
                )?;
                let _provider_used = fetched.provider_id;
                file.write_all(fetched.plaintext.as_ref())
                    .map_err(|source| CoreError::Io {
                        operation: "write restore staging file",
                        path: self.canonical.join(entry.path.as_str()),
                        source,
                    })?;
                full_hasher.update(fetched.plaintext.as_ref());
                failures.extend(fetched.failures);
                let length = u64::from(reference.plaintext_length);
                remaining -= length;
                bytes_written = bytes_written.saturating_add(length);
                reference_index += 1;
            }
            logical_position = offset + length;
        }
        hash_zeros(
            &mut full_hasher,
            entry.length.saturating_sub(logical_position),
            &zero_buffer,
        );
        if reference_index != entry.chunks.len() {
            return Err(CoreError::RestorePlanMismatch);
        }
        Ok(FileWriteOutcome {
            plaintext_digest: full_hasher.finalize().to_hex().to_string(),
            bytes_written,
            failures,
        })
    }

    fn open_parent(
        &self,
        path: &RelativePath,
        create: bool,
    ) -> Result<(std::os::fd::OwnedFd, String), CoreError> {
        let mut components: Vec<_> = path.components().collect();
        let name = components
            .pop()
            .ok_or(CoreError::RestorePlanMismatch)?
            .to_owned();
        let directory = self.open_directory(components.into_iter(), create)?;
        Ok((directory, name))
    }

    fn open_directory<'a>(
        &self,
        components: impl Iterator<Item = &'a str>,
        create: bool,
    ) -> Result<std::os::fd::OwnedFd, CoreError> {
        use rustix::fs::{Mode, OFlags, mkdirat, openat};
        let mut current = rustix::io::dup(&self.root_descriptor)
            .map_err(|error| restore_os_error("duplicate root handle", &self.canonical, error))?;
        for component in components {
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let next = match openat(&current, component, flags, Mode::empty()) {
                Ok(descriptor) => descriptor,
                Err(rustix::io::Errno::NOENT) if create => {
                    mkdirat(
                        &current,
                        component,
                        Mode::RUSR
                            | Mode::WUSR
                            | Mode::XUSR
                            | Mode::RGRP
                            | Mode::XGRP
                            | Mode::ROTH
                            | Mode::XOTH,
                    )
                    .map_err(|error| {
                        restore_os_error("create restore directory", &self.canonical, error)
                    })?;
                    rustix::fs::fsync(&current).map_err(|error| {
                        restore_os_error("sync restore directory parent", &self.canonical, error)
                    })?;
                    openat(&current, component, flags, Mode::empty()).map_err(|error| {
                        restore_os_error("open created restore directory", &self.canonical, error)
                    })?
                }
                Err(error) => {
                    return Err(
                        if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
                            CoreError::SymlinkTraversal(self.canonical.join(component))
                        } else {
                            restore_os_error("open restore directory", &self.canonical, error)
                        },
                    );
                }
            };
            current = next;
        }
        Ok(current)
    }

    fn root_is_unchanged(&self) -> Result<bool, CoreError> {
        use rustix::fs::{Mode, OFlags, fstat, open};
        let reopened = open(
            &self.canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| restore_os_error("recheck authorized root", &self.canonical, error))?;
        let stat = fstat(&reopened).map_err(|error| {
            restore_os_error("inspect rechecked authorized root", &self.canonical, error)
        })?;
        Ok(stat.st_dev as u64 == self.root_device && stat.st_ino as u64 == self.root_inode)
    }

    fn apply_file_metadata(&self, file: &File, entry: &ManifestEntry) -> Result<(), CoreError> {
        #[cfg(unix)]
        if let Some(mode) = entry.metadata.unix_mode {
            rustix::fs::fchmod(
                file,
                rustix::fs::Mode::from_raw_mode((mode & 0o777) as rustix::fs::RawMode),
            )
            .map_err(|error| restore_os_error("set restored file mode", &self.canonical, error))?;
        }
        if let Some(milliseconds) = entry.metadata.modified_at_unix_ms {
            let seconds = i64::try_from(milliseconds / 1_000).unwrap_or(i64::MAX);
            let nanoseconds = ((milliseconds % 1_000) * 1_000_000) as u32;
            filetime::set_file_handle_times(
                file,
                None,
                Some(filetime::FileTime::from_unix_time(seconds, nanoseconds)),
            )
            .map_err(|source| CoreError::Io {
                operation: "set restored file modification time",
                path: self.canonical.join(entry.path.as_str()),
                source,
            })?;
        }
        Ok(())
    }

    fn apply_metadata_to_descriptor(
        &self,
        descriptor: &std::os::fd::OwnedFd,
        entry: &ManifestEntry,
    ) -> Result<(), CoreError> {
        if let Some(mode) = entry.metadata.unix_mode {
            rustix::fs::fchmod(
                descriptor,
                rustix::fs::Mode::from_raw_mode((mode & 0o777) as rustix::fs::RawMode),
            )
            .map_err(|error| {
                restore_os_error("set restored directory mode", &self.canonical, error)
            })?;
        }
        if let Some(milliseconds) = entry.metadata.modified_at_unix_ms {
            let duplicated = rustix::io::dup(descriptor).map_err(|error| {
                restore_os_error(
                    "duplicate restored directory handle",
                    &self.canonical,
                    error,
                )
            })?;
            let file = File::from(duplicated);
            let seconds = i64::try_from(milliseconds / 1_000).unwrap_or(i64::MAX);
            let nanoseconds = ((milliseconds % 1_000) * 1_000_000) as u32;
            filetime::set_file_handle_times(
                &file,
                None,
                Some(filetime::FileTime::from_unix_time(seconds, nanoseconds)),
            )
            .map_err(|source| CoreError::Io {
                operation: "set restored directory modification time",
                path: self.canonical.clone(),
                source,
            })?;
        }
        Ok(())
    }

    fn apply_directory_metadata(
        &self,
        path: &RelativePath,
        entry: &ManifestEntry,
    ) -> Result<(), CoreError> {
        if !self.root_is_unchanged()? {
            return Err(CoreError::RestorePlanMismatch);
        }
        let descriptor = self.open_directory(path.components(), false)?;
        self.apply_metadata_to_descriptor(&descriptor, entry)?;
        rustix::fs::fsync(&descriptor).map_err(|error| {
            restore_os_error("sync restored directory metadata", &self.canonical, error)
        })
    }
}

#[cfg(unix)]
fn restore_os_error(operation: &'static str, path: &Path, error: rustix::io::Errno) -> CoreError {
    CoreError::Io {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::from_raw_os_error(error.raw_os_error()),
    }
}

#[cfg(not(unix))]
struct SafeRestoreRoot {
    canonical: PathBuf,
}

#[cfg(not(unix))]
impl SafeRestoreRoot {
    fn open(root: &AuthorizedRoot) -> Result<Self, CoreError> {
        Ok(Self {
            canonical: root.canonical_path().to_path_buf(),
        })
    }

    fn ensure_directory(
        &self,
        path: &RelativePath,
        _entry: &ManifestEntry,
    ) -> Result<(), CoreError> {
        fs::create_dir_all(self.canonical.join(path.as_str())).map_err(|source| CoreError::Io {
            operation: "create restore directory",
            path: self.canonical.join(path.as_str()),
            source,
        })
    }

    fn verify_directory(&self, path: &RelativePath) -> Result<(), CoreError> {
        if self.canonical.join(path.as_str()).is_dir() {
            Ok(())
        } else {
            Err(CoreError::RestoreConflict(
                self.canonical.join(path.as_str()),
            ))
        }
    }

    fn verify_regular_file(&self, path: &RelativePath) -> Result<(), CoreError> {
        if self.canonical.join(path.as_str()).is_file() {
            Ok(())
        } else {
            Err(CoreError::RestoreConflict(
                self.canonical.join(path.as_str()),
            ))
        }
    }

    fn digest_regular_file(&self, path: &RelativePath) -> Result<String, CoreError> {
        let destination = self.canonical.join(path.as_str());
        let metadata = fs::symlink_metadata(&destination).map_err(|source| CoreError::Io {
            operation: "inspect restored file",
            path: destination.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CoreError::RestoreConflict(destination));
        }
        let file = File::open(&destination).map_err(|source| CoreError::Io {
            operation: "open restored file for verification",
            path: destination.clone(),
            source,
        })?;
        digest_file_handle(&file, &destination)
    }

    fn completed_entry_matches(
        &self,
        path: &RelativePath,
        kind: EntryKind,
        expected_digest: &str,
    ) -> Result<bool, CoreError> {
        match kind {
            EntryKind::Directory => {
                Ok(self.canonical.join(path.as_str()).is_dir() && expected_digest == "directory")
            }
            EntryKind::File => match self.digest_regular_file(path) {
                Ok(digest) => Ok(digest == expected_digest),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_file(
        &self,
        _entry: &ManifestEntry,
        _destination: &RelativePath,
        _action: PreviewAction,
        _backup_id: BackupId,
        _key: &BackupKey,
        _scheduler: &ReplicationScheduler,
        _allowed_providers: &BTreeMap<String, BTreeSet<DeviceId>>,
        _control: &JobControl,
    ) -> Result<FileWriteOutcome, CoreError> {
        Err(CoreError::InvalidState(
            "Windows is outside the supported platform set".to_owned(),
        ))
    }

    fn apply_directory_metadata(
        &self,
        _path: &RelativePath,
        _entry: &ManifestEntry,
    ) -> Result<(), CoreError> {
        Ok(())
    }
}

fn hash_zeros(hasher: &mut blake3::Hasher, mut length: u64, zero_buffer: &[u8]) {
    while length > 0 {
        let take = usize::try_from(length.min(zero_buffer.len() as u64)).expect("bounded");
        hasher.update(&zero_buffer[..take]);
        length -= take as u64;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use covalent_protocol::{
        ChunkReference, EntryMetadata, Manifest, ManifestEntry, PROTOCOL_VERSION, ReplicaIntent,
    };
    use tempfile::tempdir;

    use crate::ChunkProvider;
    use crate::replication::StoreProvider;

    use super::*;

    fn single_file_manifest(
        backup_id: BackupId,
        key: &BackupKey,
        store: &ChunkStore,
        provider_id: DeviceId,
    ) -> Manifest {
        let encrypted = key
            .encrypt_chunk(backup_id, 1, b"restored content")
            .expect("encrypt");
        store.put(&encrypted).expect("store");
        let ciphertext_length = encrypted.ciphertext_length();
        let locator = encrypted.opaque_locator.clone();
        Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "snapshot".to_owned(),
            created_at_unix_ms: 1,
            replica_intent: ReplicaIntent::explicit([provider_id]),
            entries: vec![ManifestEntry {
                path: RelativePath::new("nested/file.txt").expect("path"),
                kind: EntryKind::File,
                length: 16,
                chunks: vec![ChunkReference {
                    plaintext_digest: encrypted.plaintext_digest,
                    opaque_locator: locator.clone(),
                    plaintext_length: encrypted.plaintext_length,
                    ciphertext_length,
                }],
                metadata: EntryMetadata::default(),
                sparse_extents: Vec::new(),
            }],
            provider_acknowledgements: BTreeMap::from([(provider_id, BTreeSet::from([locator]))]),
        }
    }

    #[test]
    fn signed_preview_and_atomic_restore_round_trip() {
        let data = tempdir().expect("data");
        let restore = tempdir().expect("restore");
        let store = ChunkStore::open(data.path(), 1_048_576).expect("store");
        let provider_id = DeviceId::new();
        let key = BackupKey::generate();
        let manifest = single_file_manifest(BackupId::new(), &key, &store, provider_id);
        let identity = DeviceIdentity::generate();
        let root = AuthorizedRoot::open(restore.path()).expect("root");
        let plan = preview_restore(
            &manifest,
            &root,
            &RestoreOptions::all("restore-job"),
            &identity,
        )
        .expect("preview");
        let scheduler = ReplicationScheduler::new(
            [Arc::new(StoreProvider::new(provider_id, store.clone())) as Arc<dyn ChunkProvider>],
            4,
        )
        .expect("scheduler");
        let mut tampered = plan.clone();
        tampered.job_id = "attacker-job".to_owned();
        assert!(matches!(
            execute_restore(
                &manifest,
                &tampered,
                &root,
                &key,
                &scheduler,
                &store,
                &identity.public_identity(),
                provider_id,
                &JobControl::new(),
            ),
            Err(CoreError::RestorePlanMismatch)
        ));
        let report = execute_restore(
            &manifest,
            &plan,
            &root,
            &key,
            &scheduler,
            &store,
            &identity.public_identity(),
            provider_id,
            &JobControl::new(),
        )
        .expect("restore");
        assert_eq!(report.files_restored, 1);
        assert_eq!(
            fs::read(restore.path().join("nested/file.txt")).expect("restored file"),
            b"restored content"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preview_rejects_intermediate_symlink() {
        use std::os::unix::fs::symlink;
        let restore = tempdir().expect("restore");
        let outside = tempdir().expect("outside");
        symlink(outside.path(), restore.path().join("nested")).expect("symlink");
        let data = tempdir().expect("data");
        let store = ChunkStore::open(data.path(), 1_048_576).expect("store");
        let key = BackupKey::generate();
        let manifest = single_file_manifest(BackupId::new(), &key, &store, DeviceId::new());
        let identity = DeviceIdentity::generate();
        assert!(matches!(
            preview_restore(
                &manifest,
                &AuthorizedRoot::open(restore.path()).expect("root"),
                &RestoreOptions::all("job"),
                &identity
            ),
            Err(CoreError::SymlinkTraversal(_))
        ));
    }
}
