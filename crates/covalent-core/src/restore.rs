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

const LEGACY_RESTORE_CHECKPOINT_SCHEMA_VERSION: u16 = 1;
const RESTORE_WAL_SCHEMA_VERSION: u16 = 2;
const RESTORE_WAL_COMPACTION_STALE_RECORDS: usize = 4_096;
const RESTORE_WAL_SYNC_INTERVAL: usize = 64;
const RESTORE_FILE_COMMIT_BATCH_SIZE: usize = 64;
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
    /// Authenticated chunks consumed from each provider.
    pub provider_chunks: BTreeMap<DeviceId, usize>,
}

#[derive(Clone, Debug)]
struct RestoreCheckpoint {
    plan_digest: String,
    entries: BTreeMap<RelativePath, RestoreCheckpointEntry>,
    record_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyRestoreCheckpoint {
    schema_version: u16,
    plan_digest: String,
    completed: BTreeMap<RelativePath, String>,
}

#[derive(Clone, Debug)]
enum RestoreCheckpointEntry {
    Prepared(RestoreReceipt),
    Completed(RestoreReceipt),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreReceipt {
    kind: EntryKind,
    plaintext_digest: String,
    expected_length: Option<u64>,
    expected_metadata: Option<EntryMetadata>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum RestoreWalRecord {
    Header {
        schema_version: u16,
        plan_digest: String,
    },
    Prepared {
        destination_path: RelativePath,
        receipt: RestoreReceipt,
    },
    Completed {
        destination_path: RelativePath,
        receipt: RestoreReceipt,
    },
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
    let mut checkpoint = load_restore_checkpoint(store, plan, manifest)?;
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

    let mut preview_index = 0_usize;
    while let Some(preview) = plan.entries.get(preview_index) {
        check_restore_control(control, store, &plan.job_id)?;
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
        if let Some(state) = checkpoint.entries.get(&preview.destination_path).cloned() {
            let receipt = match &state {
                RestoreCheckpointEntry::Prepared(receipt)
                | RestoreCheckpointEntry::Completed(receipt) => receipt,
            };
            if safe_root.completed_entry_matches(&preview.destination_path, receipt)? {
                if matches!(state, RestoreCheckpointEntry::Prepared(_)) {
                    append_restore_checkpoint(
                        store,
                        plan,
                        &mut checkpoint,
                        preview.destination_path.clone(),
                        RestoreCheckpointEntry::Completed(receipt.clone()),
                    )?;
                }
                if preview.action == PreviewAction::CreateDirectory {
                    deferred_directories.push((preview.destination_path.clone(), entry.clone()));
                }
                preview_index += 1;
                continue;
            }
            if matches!(state, RestoreCheckpointEntry::Completed(_)) {
                return Err(CoreError::RestorePlanMismatch);
            }
        }
        match preview.action {
            PreviewAction::CreateDirectory => {
                let receipt = directory_receipt();
                append_restore_checkpoint(
                    store,
                    plan,
                    &mut checkpoint,
                    preview.destination_path.clone(),
                    RestoreCheckpointEntry::Prepared(receipt.clone()),
                )?;
                safe_root.ensure_directory(&preview.destination_path, entry)?;
                report.directories_created += 1;
                append_restore_checkpoint(
                    store,
                    plan,
                    &mut checkpoint,
                    preview.destination_path.clone(),
                    RestoreCheckpointEntry::Completed(receipt),
                )?;
                deferred_directories.push((preview.destination_path.clone(), (*entry).clone()));
            }
            PreviewAction::KeepDirectory => {
                safe_root.verify_directory(&preview.destination_path)?;
                append_restore_checkpoint(
                    store,
                    plan,
                    &mut checkpoint,
                    preview.destination_path.clone(),
                    RestoreCheckpointEntry::Completed(directory_receipt()),
                )?;
            }
            PreviewAction::SkipFile => {
                safe_root.verify_regular_file(&preview.destination_path)?;
                report.files_skipped += 1;
                let digest = safe_root.digest_regular_file(&preview.destination_path)?;
                append_restore_checkpoint(
                    store,
                    plan,
                    &mut checkpoint,
                    preview.destination_path.clone(),
                    RestoreCheckpointEntry::Completed(RestoreReceipt {
                        kind: EntryKind::File,
                        plaintext_digest: digest,
                        expected_length: None,
                        expected_metadata: None,
                    }),
                )?;
            }
            PreviewAction::CreateFile | PreviewAction::ReplaceFile | PreviewAction::RenameFile => {
                let mut batch = vec![(entry, preview)];
                while batch.len() < RESTORE_FILE_COMMIT_BATCH_SIZE {
                    let Some(next_preview) = plan.entries.get(preview_index + batch.len()) else {
                        break;
                    };
                    if !matches!(
                        next_preview.action,
                        PreviewAction::CreateFile
                            | PreviewAction::ReplaceFile
                            | PreviewAction::RenameFile
                    ) || checkpoint
                        .entries
                        .contains_key(&next_preview.destination_path)
                    {
                        break;
                    }
                    let next_entry = manifest_entries
                        .get(&next_preview.source_path)
                        .copied()
                        .ok_or(CoreError::RestorePlanMismatch)?;
                    batch.push((next_entry, next_preview));
                }
                let outcomes = safe_root.write_files(
                    &batch,
                    manifest.backup_id,
                    key,
                    scheduler,
                    &allowed_providers,
                    control,
                    &mut |preparations| {
                        for ((entry, _), preparation) in batch.iter().zip(preparations) {
                            let receipt =
                                restored_file_receipt(entry, &preparation.outcome.plaintext_digest);
                            append_restore_checkpoint_buffered(
                                store,
                                plan,
                                &mut checkpoint,
                                preparation.destination.clone(),
                                RestoreCheckpointEntry::Prepared(receipt),
                            )?;
                        }
                        store.sync_checkpoint_records(&plan.job_id)?;
                        Ok(())
                    },
                )?;
                for ((entry, preview), outcome) in batch.iter().zip(outcomes) {
                    report.files_restored += 1;
                    report.bytes_written =
                        report.bytes_written.saturating_add(outcome.bytes_written);
                    report.rejected_provider_copies.extend(outcome.failures);
                    for (provider_id, chunks) in outcome.provider_chunks {
                        *report.provider_chunks.entry(provider_id).or_default() += chunks;
                    }
                    let receipt = restored_file_receipt(entry, &outcome.plaintext_digest);
                    append_restore_checkpoint_buffered(
                        store,
                        plan,
                        &mut checkpoint,
                        preview.destination_path.clone(),
                        RestoreCheckpointEntry::Completed(receipt),
                    )?;
                }
                preview_index += batch.len();
                continue;
            }
        }
        preview_index += 1;
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
    manifest: &Manifest,
) -> Result<RestoreCheckpoint, CoreError> {
    if let Some(records) = store.read_checkpoint_records(&plan.job_id)? {
        return replay_restore_checkpoint(store, plan, records);
    }
    if let Some(bytes) = store.read_checkpoint(&plan.job_id)? {
        let legacy: LegacyRestoreCheckpoint = serde_json::from_slice(&bytes)?;
        if legacy.schema_version != LEGACY_RESTORE_CHECKPOINT_SCHEMA_VERSION
            || legacy.plan_digest != plan.plan_digest
        {
            return Err(CoreError::RestorePlanMismatch);
        }
        let mut checkpoint = RestoreCheckpoint {
            plan_digest: plan.plan_digest.clone(),
            entries: BTreeMap::new(),
            record_count: 1,
        };
        for (destination, digest) in legacy.completed {
            let preview = plan
                .entries
                .iter()
                .find(|preview| preview.destination_path == destination)
                .ok_or(CoreError::RestorePlanMismatch)?;
            let receipt = if preview.kind == EntryKind::Directory {
                directory_receipt()
            } else if preview.action == PreviewAction::SkipFile {
                RestoreReceipt {
                    kind: EntryKind::File,
                    plaintext_digest: digest,
                    expected_length: None,
                    expected_metadata: None,
                }
            } else {
                let entry = manifest
                    .entries
                    .iter()
                    .find(|entry| entry.path == preview.source_path)
                    .ok_or(CoreError::RestorePlanMismatch)?;
                restored_file_receipt(entry, &digest)
            };
            checkpoint
                .entries
                .insert(destination, RestoreCheckpointEntry::Completed(receipt));
        }
        compact_restore_checkpoint(store, plan, &checkpoint)?;
        return Ok(checkpoint);
    }
    let checkpoint = RestoreCheckpoint {
        plan_digest: plan.plan_digest.clone(),
        entries: BTreeMap::new(),
        record_count: 1,
    };
    compact_restore_checkpoint(store, plan, &checkpoint)?;
    Ok(checkpoint)
}

fn replay_restore_checkpoint(
    store: &ChunkStore,
    plan: &RestorePlan,
    records: Vec<Vec<u8>>,
) -> Result<RestoreCheckpoint, CoreError> {
    let mut records = records.into_iter();
    let header: RestoreWalRecord = serde_json::from_slice(
        &records
            .next()
            .ok_or_else(|| CoreError::InvalidState("empty restore checkpoint log".to_owned()))?,
    )?;
    match header {
        RestoreWalRecord::Header {
            schema_version,
            plan_digest,
        } if schema_version == RESTORE_WAL_SCHEMA_VERSION && plan_digest == plan.plan_digest => {}
        _ => return Err(CoreError::RestorePlanMismatch),
    }
    let mut checkpoint = RestoreCheckpoint {
        plan_digest: plan.plan_digest.clone(),
        entries: BTreeMap::new(),
        record_count: 1,
    };
    for bytes in records {
        let (destination_path, state) = match serde_json::from_slice(&bytes)? {
            RestoreWalRecord::Prepared {
                destination_path,
                receipt,
            } => (destination_path, RestoreCheckpointEntry::Prepared(receipt)),
            RestoreWalRecord::Completed {
                destination_path,
                receipt,
            } => (destination_path, RestoreCheckpointEntry::Completed(receipt)),
            RestoreWalRecord::Header { .. } => return Err(CoreError::RestorePlanMismatch),
        };
        if !plan
            .entries
            .iter()
            .any(|preview| preview.destination_path == destination_path)
        {
            return Err(CoreError::RestorePlanMismatch);
        }
        checkpoint.entries.insert(destination_path, state);
        checkpoint.record_count = checkpoint.record_count.saturating_add(1);
    }
    if restore_checkpoint_needs_compaction(&checkpoint) {
        compact_restore_checkpoint(store, plan, &checkpoint)?;
    }
    Ok(checkpoint)
}

fn append_restore_checkpoint(
    store: &ChunkStore,
    plan: &RestorePlan,
    checkpoint: &mut RestoreCheckpoint,
    destination_path: RelativePath,
    state: RestoreCheckpointEntry,
) -> Result<(), CoreError> {
    let next_record_count = checkpoint.record_count.saturating_add(1);
    let durable = matches!(state, RestoreCheckpointEntry::Prepared(_))
        || next_record_count.is_multiple_of(RESTORE_WAL_SYNC_INTERVAL);
    append_restore_checkpoint_with_durability(
        store,
        plan,
        checkpoint,
        destination_path,
        state,
        durable,
    )
}

fn append_restore_checkpoint_buffered(
    store: &ChunkStore,
    plan: &RestorePlan,
    checkpoint: &mut RestoreCheckpoint,
    destination_path: RelativePath,
    state: RestoreCheckpointEntry,
) -> Result<(), CoreError> {
    append_restore_checkpoint_with_durability(
        store,
        plan,
        checkpoint,
        destination_path,
        state,
        false,
    )
}

fn append_restore_checkpoint_with_durability(
    store: &ChunkStore,
    plan: &RestorePlan,
    checkpoint: &mut RestoreCheckpoint,
    destination_path: RelativePath,
    state: RestoreCheckpointEntry,
    durable: bool,
) -> Result<(), CoreError> {
    if checkpoint.plan_digest != plan.plan_digest {
        return Err(CoreError::RestorePlanMismatch);
    }
    let record = match &state {
        RestoreCheckpointEntry::Prepared(receipt) => RestoreWalRecord::Prepared {
            destination_path: destination_path.clone(),
            receipt: receipt.clone(),
        },
        RestoreCheckpointEntry::Completed(receipt) => RestoreWalRecord::Completed {
            destination_path: destination_path.clone(),
            receipt: receipt.clone(),
        },
    };
    store.append_checkpoint_record_buffered(
        &plan.job_id,
        &serde_json::to_vec(&record)?,
        durable,
    )?;
    checkpoint.entries.insert(destination_path, state);
    checkpoint.record_count = checkpoint.record_count.saturating_add(1);
    if restore_checkpoint_needs_compaction(checkpoint) {
        compact_restore_checkpoint(store, plan, checkpoint)?;
        checkpoint.record_count = checkpoint.entries.len().saturating_add(1);
    }
    Ok(())
}

fn check_restore_control(
    control: &JobControl,
    store: &ChunkStore,
    job_id: &str,
) -> Result<(), CoreError> {
    match control.check() {
        Ok(()) => Ok(()),
        Err(error @ (CoreError::Paused | CoreError::Cancelled)) => {
            store.sync_checkpoint_records(job_id)?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

fn restore_checkpoint_needs_compaction(checkpoint: &RestoreCheckpoint) -> bool {
    let live_records = checkpoint.entries.len().saturating_add(1);
    let stale_records = checkpoint.record_count.saturating_sub(live_records);
    stale_records
        > RESTORE_WAL_COMPACTION_STALE_RECORDS.max(checkpoint.entries.len().saturating_div(2))
}

fn compact_restore_checkpoint(
    store: &ChunkStore,
    plan: &RestorePlan,
    checkpoint: &RestoreCheckpoint,
) -> Result<(), CoreError> {
    let mut records = Vec::with_capacity(checkpoint.entries.len().saturating_add(1));
    records.push(serde_json::to_vec(&RestoreWalRecord::Header {
        schema_version: RESTORE_WAL_SCHEMA_VERSION,
        plan_digest: plan.plan_digest.clone(),
    })?);
    for (destination_path, state) in &checkpoint.entries {
        let record = match state {
            RestoreCheckpointEntry::Prepared(receipt) => RestoreWalRecord::Prepared {
                destination_path: destination_path.clone(),
                receipt: receipt.clone(),
            },
            RestoreCheckpointEntry::Completed(receipt) => RestoreWalRecord::Completed {
                destination_path: destination_path.clone(),
                receipt: receipt.clone(),
            },
        };
        records.push(serde_json::to_vec(&record)?);
    }
    store.replace_checkpoint_records(&plan.job_id, &records)
}

fn directory_receipt() -> RestoreReceipt {
    RestoreReceipt {
        kind: EntryKind::Directory,
        plaintext_digest: "directory".to_owned(),
        expected_length: Some(0),
        expected_metadata: None,
    }
}

fn restored_file_receipt(entry: &ManifestEntry, plaintext_digest: &str) -> RestoreReceipt {
    RestoreReceipt {
        kind: EntryKind::File,
        plaintext_digest: plaintext_digest.to_owned(),
        expected_length: Some(entry.length),
        expected_metadata: Some(entry.metadata.clone()),
    }
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
    provider_chunks: BTreeMap<DeviceId, usize>,
}

struct PreparedRestoreFile<'a> {
    destination: &'a RelativePath,
    outcome: &'a FileWriteOutcome,
}

#[cfg(unix)]
struct StagedRestoreFile {
    destination: RelativePath,
    parent: std::os::fd::OwnedFd,
    final_name: String,
    temporary_name: String,
    action: PreviewAction,
    file: Option<File>,
    outcome: Option<FileWriteOutcome>,
    committed: bool,
}

#[cfg(unix)]
impl Drop for StagedRestoreFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = rustix::fs::unlinkat(
                &self.parent,
                self.temporary_name.as_str(),
                rustix::fs::AtFlags::empty(),
            );
        }
    }
}

#[cfg(unix)]
fn sync_staged_restore_files(
    staged_files: &[StagedRestoreFile],
    restore_root: &Path,
    maximum_parallelism: usize,
    control: &JobControl,
) -> Result<(), CoreError> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    if staged_files.is_empty() {
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let first_error = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..maximum_parallelism.min(staged_files.len()) {
            scope.spawn(|| {
                loop {
                    if first_error.lock().expect("restore sync lock").is_some() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(staged) = staged_files.get(index) else {
                        break;
                    };
                    let result = control.check().and_then(|()| {
                        staged
                            .file
                            .as_ref()
                            .ok_or_else(|| {
                                CoreError::InvalidState(
                                    "restore staging descriptor is unavailable".to_owned(),
                                )
                            })?
                            .sync_all()
                            .map_err(|source| CoreError::Io {
                                operation: "sync restore staging file",
                                path: restore_root.join(staged.destination.as_str()),
                                source,
                            })
                    });
                    if let Err(error) = result {
                        let mut captured = first_error.lock().expect("restore sync lock");
                        if captured.is_none() {
                            *captured = Some(error);
                        }
                        break;
                    }
                }
            });
        }
    });
    if let Some(error) = first_error.into_inner().expect("restore sync lock") {
        return Err(error);
    }
    Ok(())
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
        receipt: &RestoreReceipt,
    ) -> Result<bool, CoreError> {
        match receipt.kind {
            EntryKind::Directory => match self.verify_directory(path) {
                Ok(()) => Ok(receipt.plaintext_digest == "directory"),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            },
            EntryKind::File => match self.regular_file_matches_receipt(path, receipt) {
                Ok(matches) => Ok(matches),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            },
        }
    }

    fn regular_file_matches_receipt(
        &self,
        path: &RelativePath,
        receipt: &RestoreReceipt,
    ) -> Result<bool, CoreError> {
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
                "open restored file for reconciliation",
                &self.canonical,
                error,
            )
        })?;
        let stat = fstat(&descriptor).map_err(|error| {
            restore_os_error(
                "inspect restored file for reconciliation",
                &self.canonical,
                error,
            )
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(false);
        }
        let file = File::from(descriptor);
        let metadata = file.metadata().map_err(|source| CoreError::Io {
            operation: "inspect restored file metadata",
            path: self.canonical.join(path.as_str()),
            source,
        })?;
        if receipt
            .expected_length
            .is_some_and(|length| metadata.len() != length)
            || receipt
                .expected_metadata
                .as_ref()
                .is_some_and(|expected| !restored_metadata_matches(&metadata, expected))
        {
            return Ok(false);
        }
        Ok(
            digest_file_handle(&file, &self.canonical.join(path.as_str()))?
                == receipt.plaintext_digest,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn write_files(
        &self,
        entries: &[(&ManifestEntry, &RestorePreviewEntry)],
        backup_id: BackupId,
        key: &BackupKey,
        scheduler: &ReplicationScheduler,
        allowed_providers: &BTreeMap<String, BTreeSet<DeviceId>>,
        control: &JobControl,
        before_commit: &mut dyn FnMut(&[PreparedRestoreFile<'_>]) -> Result<(), CoreError>,
    ) -> Result<Vec<FileWriteOutcome>, CoreError> {
        use rustix::fs::{
            AtFlags, FileType, Mode, OFlags, RenameFlags, fsync, openat, renameat, renameat_with,
            statat,
        };
        let mut staged_files = Vec::with_capacity(entries.len());
        for (entry, preview) in entries {
            control.check()?;
            if !self.root_is_unchanged()? {
                return Err(CoreError::RestorePlanMismatch);
            }
            let (parent, final_name) = self.open_parent(&preview.destination_path, true)?;
            match statat(&parent, final_name.as_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => {
                    let kind = FileType::from_raw_mode(stat.st_mode);
                    if preview.action != PreviewAction::ReplaceFile || kind != FileType::RegularFile
                    {
                        return Err(if kind == FileType::Symlink {
                            CoreError::SymlinkTraversal(
                                self.canonical.join(preview.destination_path.as_str()),
                            )
                        } else {
                            CoreError::RestoreConflict(
                                self.canonical.join(preview.destination_path.as_str()),
                            )
                        });
                    }
                }
                Err(rustix::io::Errno::NOENT) => {
                    if preview.action == PreviewAction::ReplaceFile {
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
            let mut staged = StagedRestoreFile {
                destination: preview.destination_path.clone(),
                parent,
                final_name,
                temporary_name,
                action: preview.action,
                file: None,
                outcome: None,
                committed: false,
            };
            let temporary_descriptor = openat(
                &staged.parent,
                staged.temporary_name.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|error| {
                restore_os_error("create restore staging file", &self.canonical, error)
            })?;
            let mut file = File::from(temporary_descriptor);
            let outcome = self.write_plaintext(
                &mut file,
                entry,
                backup_id,
                key,
                scheduler,
                allowed_providers,
                control,
            )?;
            self.apply_file_metadata(&file, entry)?;
            staged.file = Some(file);
            staged.outcome = Some(outcome);
            staged_files.push(staged);
        }
        sync_staged_restore_files(
            &staged_files,
            &self.canonical,
            scheduler.maximum_parallelism(),
            control,
        )?;
        for staged in &mut staged_files {
            drop(staged.file.take());
        }
        let preparations: Vec<_> = staged_files
            .iter()
            .map(|staged| PreparedRestoreFile {
                destination: &staged.destination,
                outcome: staged.outcome.as_ref().expect("staged outcome"),
            })
            .collect();
        before_commit(&preparations)?;

        for staged in &mut staged_files {
            let (reopened_parent, reopened_name) = self.open_parent(&staged.destination, false)?;
            let original_stat = rustix::fs::fstat(&staged.parent).map_err(|error| {
                restore_os_error("inspect restore parent", &self.canonical, error)
            })?;
            let reopened_stat = rustix::fs::fstat(&reopened_parent).map_err(|error| {
                restore_os_error("recheck restore parent", &self.canonical, error)
            })?;
            if reopened_name != staged.final_name
                || original_stat.st_dev != reopened_stat.st_dev
                || original_stat.st_ino != reopened_stat.st_ino
                || !self.root_is_unchanged()?
            {
                return Err(CoreError::RestorePlanMismatch);
            }
            let rename_result = if staged.action == PreviewAction::ReplaceFile {
                renameat(
                    &staged.parent,
                    staged.temporary_name.as_str(),
                    &staged.parent,
                    staged.final_name.as_str(),
                )
            } else {
                renameat_with(
                    &staged.parent,
                    staged.temporary_name.as_str(),
                    &staged.parent,
                    staged.final_name.as_str(),
                    RenameFlags::NOREPLACE,
                )
            };
            if let Err(error) = rename_result {
                return Err(restore_os_error(
                    "atomically commit restored file",
                    &self.canonical.join(staged.destination.as_str()),
                    error,
                ));
            }
            staged.committed = true;
        }
        let mut synced_parents = BTreeSet::new();
        for staged in &staged_files {
            let stat = rustix::fs::fstat(&staged.parent).map_err(|error| {
                restore_os_error("inspect restored file parent", &self.canonical, error)
            })?;
            if synced_parents.insert((stat.st_dev, stat.st_ino)) {
                fsync(&staged.parent).map_err(|error| {
                    restore_os_error(
                        "sync restored file parent",
                        &self.canonical.join(staged.destination.as_str()),
                        error,
                    )
                })?;
            }
        }
        staged_files
            .iter_mut()
            .map(|staged| {
                let mut outcome = staged.outcome.take().ok_or_else(|| {
                    CoreError::InvalidState("missing staged restore outcome".to_owned())
                })?;
                outcome.failures.sort_by_key(|failure| failure.provider_id);
                Ok(outcome)
            })
            .collect()
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
        let mut provider_chunks = BTreeMap::new();
        let provider_stripe_offset = restore_provider_stripe(&entry.path);
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
                let batch_start = reference_index;
                let mut batch_remaining = remaining;
                let mut requests = Vec::with_capacity(scheduler.maximum_parallelism());
                while requests.len() < scheduler.maximum_parallelism() && batch_remaining > 0 {
                    let reference = entry
                        .chunks
                        .get(reference_index + requests.len())
                        .ok_or(CoreError::RestorePlanMismatch)?;
                    let reference_length = u64::from(reference.plaintext_length);
                    if reference_length > batch_remaining {
                        return Err(CoreError::RestorePlanMismatch);
                    }
                    requests.push((
                        reference,
                        allowed_providers
                            .get(&reference.opaque_locator)
                            .ok_or(CoreError::RestorePlanMismatch)?,
                    ));
                    batch_remaining -= reference_length;
                }
                let fetched_chunks = scheduler.fetch_plaintexts_parallel(
                    &requests,
                    backup_id,
                    key,
                    control,
                    provider_stripe_offset.wrapping_add(batch_start),
                )?;
                for ((reference, _), fetched) in requests.into_iter().zip(fetched_chunks) {
                    control.check()?;
                    *provider_chunks.entry(fetched.provider_id).or_default() += 1;
                    file.write_all(fetched.plaintext.as_ref())
                        .map_err(|source| CoreError::Io {
                            operation: "write restore staging file",
                            path: self.canonical.join(entry.path.as_str()),
                            source,
                        })?;
                    full_hasher.update(fetched.plaintext.as_ref());
                    failures.extend(fetched.failures);
                    let chunk_length = u64::from(reference.plaintext_length);
                    remaining -= chunk_length;
                    bytes_written = bytes_written.saturating_add(chunk_length);
                    reference_index += 1;
                }
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
            provider_chunks,
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
        receipt: &RestoreReceipt,
    ) -> Result<bool, CoreError> {
        match receipt.kind {
            EntryKind::Directory => Ok(self.canonical.join(path.as_str()).is_dir()
                && receipt.plaintext_digest == "directory"),
            EntryKind::File => {
                let destination = self.canonical.join(path.as_str());
                let metadata = match fs::symlink_metadata(&destination) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        metadata
                    }
                    Ok(_) => return Ok(false),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                    Err(source) => {
                        return Err(CoreError::Io {
                            operation: "inspect restored file for reconciliation",
                            path: destination,
                            source,
                        });
                    }
                };
                if receipt
                    .expected_length
                    .is_some_and(|length| metadata.len() != length)
                    || receipt
                        .expected_metadata
                        .as_ref()
                        .is_some_and(|expected| !restored_metadata_matches(&metadata, expected))
                {
                    return Ok(false);
                }
                Ok(self.digest_regular_file(path)? == receipt.plaintext_digest)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_files(
        &self,
        _entries: &[(&ManifestEntry, &RestorePreviewEntry)],
        _backup_id: BackupId,
        _key: &BackupKey,
        _scheduler: &ReplicationScheduler,
        _allowed_providers: &BTreeMap<String, BTreeSet<DeviceId>>,
        _control: &JobControl,
        _before_commit: &mut dyn FnMut(&[PreparedRestoreFile<'_>]) -> Result<(), CoreError>,
    ) -> Result<Vec<FileWriteOutcome>, CoreError> {
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

fn restore_provider_stripe(path: &RelativePath) -> usize {
    let digest = blake3::hash(path.as_str().as_bytes());
    usize::from_be_bytes(
        digest.as_bytes()[..std::mem::size_of::<usize>()]
            .try_into()
            .expect("native word slice"),
    )
}

fn hash_zeros(hasher: &mut blake3::Hasher, mut length: u64, zero_buffer: &[u8]) {
    while length > 0 {
        let take = usize::try_from(length.min(zero_buffer.len() as u64)).expect("bounded");
        hasher.update(&zero_buffer[..take]);
        length -= take as u64;
    }
}

fn restored_metadata_matches(metadata: &fs::Metadata, expected: &EntryMetadata) -> bool {
    #[cfg(unix)]
    if let Some(expected_mode) = expected.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != expected_mode & 0o777 {
            return false;
        }
    }
    if let Some(expected_millis) = expected.modified_at_unix_ms {
        let actual_millis = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());
        if actual_millis != Some(expected_millis) {
            return false;
        }
    }
    true
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

    #[test]
    fn prepared_restore_entry_reconciles_after_durable_rename_crash() {
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
            &RestoreOptions::all("restore-crash"),
            &identity,
        )
        .expect("preview");
        fs::create_dir(restore.path().join("nested")).expect("committed directory");
        fs::write(restore.path().join("nested/file.txt"), b"restored content")
            .expect("committed file");
        let file_entry = manifest.entries.first().expect("file entry");
        let file_destination = plan
            .entries
            .iter()
            .find(|preview| preview.kind == EntryKind::File)
            .expect("file preview")
            .destination_path
            .clone();
        let directory_destination = plan
            .entries
            .iter()
            .find(|preview| preview.kind == EntryKind::Directory)
            .expect("directory preview")
            .destination_path
            .clone();
        let records = vec![
            serde_json::to_vec(&RestoreWalRecord::Header {
                schema_version: RESTORE_WAL_SCHEMA_VERSION,
                plan_digest: plan.plan_digest.clone(),
            })
            .expect("header"),
            serde_json::to_vec(&RestoreWalRecord::Completed {
                destination_path: directory_destination,
                receipt: directory_receipt(),
            })
            .expect("directory receipt"),
            serde_json::to_vec(&RestoreWalRecord::Prepared {
                destination_path: file_destination,
                receipt: restored_file_receipt(
                    file_entry,
                    blake3::hash(b"restored content").to_hex().as_str(),
                ),
            })
            .expect("prepared receipt"),
        ];
        store
            .replace_checkpoint_records(&plan.job_id, &records)
            .expect("crash journal");
        let scheduler = ReplicationScheduler::new(
            [Arc::new(StoreProvider::new(provider_id, store.clone())) as Arc<dyn ChunkProvider>],
            4,
        )
        .expect("scheduler");

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
        .expect("reconcile");
        assert_eq!(report.files_restored, 0);
        assert_eq!(
            fs::read(restore.path().join("nested/file.txt")).expect("restored file"),
            b"restored content"
        );
        assert!(
            !store
                .has_checkpoint(&plan.job_id)
                .expect("checkpoint removed")
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
