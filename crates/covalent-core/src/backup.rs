use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use covalent_protocol::{
    BackupId, ChunkReference, EntryKind, EntryMetadata, Manifest, ManifestEntry, PROTOCOL_VERSION,
    RelativePath, ReplicaIntent, SparseExtent,
};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::engine::JobControl;
use crate::{
    BackupKey, ChunkStore, ChunkingConfig, ContentDefinedChunker, CoreError, ReplicationReport,
    StoredSnapshot,
};

const LEGACY_BACKUP_CHECKPOINT_SCHEMA_VERSION: u16 = 2;
const BACKUP_WAL_SCHEMA_VERSION: u16 = 3;
const BACKUP_WAL_COMPACTION_STALE_RECORDS: usize = 4_096;
const BACKUP_WAL_SYNC_INTERVAL: usize = 64;

/// Source-link handling. Covalent never follows source symlinks.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymlinkPolicy {
    /// Fail the backup so omitted content is visible.
    Reject,
    /// Explicitly omit symlinks and report their count in progress.
    Skip,
}

/// Compiled slash-separated source exclusion patterns.
#[derive(Clone, Debug)]
pub struct ExclusionRules {
    patterns: Vec<String>,
    matcher: GlobSet,
}

impl ExclusionRules {
    /// Compiles a bounded set of gitignore-style glob patterns.
    pub fn new(patterns: Vec<String>) -> Result<Self, CoreError> {
        if patterns.len() > 1_024 || patterns.iter().any(|pattern| pattern.len() > 1_024) {
            return Err(CoreError::ResourceLimit("backup exclusion patterns"));
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            let glob = GlobBuilder::new(pattern)
                .literal_separator(true)
                .backslash_escape(false)
                .build()
                .map_err(|error| {
                    CoreError::InvalidState(format!("invalid exclusion pattern: {error}"))
                })?;
            builder.add(glob);
        }
        let matcher = builder
            .build()
            .map_err(|error| CoreError::InvalidState(format!("invalid exclusion set: {error}")))?;
        Ok(Self { patterns, matcher })
    }

    /// Empty exclusion set.
    #[must_use]
    pub fn none() -> Self {
        Self::new(Vec::new()).expect("empty glob set is valid")
    }

    /// Original patterns for stable resume fingerprinting.
    #[must_use]
    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    fn matches(&self, path: &RelativePath) -> bool {
        self.matcher.is_match(path.as_str())
    }
}

impl Default for ExclusionRules {
    fn default() -> Self {
        Self::none()
    }
}

/// Immutable backup job options.
#[derive(Clone, Debug)]
pub struct BackupOptions {
    /// Stable logical backup identifier.
    pub backup_id: BackupId,
    /// User-visible remembered backup name.
    pub display_name: String,
    /// Validated, monotonic snapshot identifier supplied by the caller.
    pub snapshot_id: String,
    /// Active content-key epoch.
    pub key_epoch: u64,
    /// Exact provider set explicitly selected by the user.
    pub replica_intent: ReplicaIntent,
    /// Streaming content-defined chunk limits.
    pub chunking: ChunkingConfig,
    /// Explicit source omissions.
    pub exclusions: ExclusionRules,
    /// Visible source symlink policy.
    pub symlink_policy: SymlinkPolicy,
    /// Stable resumable job identifier.
    pub job_id: String,
    /// Wall-clock creation time stored in the manifest.
    pub created_at_unix_ms: u64,
}

impl BackupOptions {
    /// Creates safe defaults with no automatic replicas and no exclusions.
    #[must_use]
    pub fn new(
        backup_id: BackupId,
        snapshot_id: impl Into<String>,
        job_id: impl Into<String>,
    ) -> Self {
        Self {
            backup_id,
            display_name: "Backup".to_owned(),
            snapshot_id: snapshot_id.into(),
            key_epoch: 1,
            replica_intent: ReplicaIntent::default(),
            chunking: ChunkingConfig::default(),
            exclusions: ExclusionRules::default(),
            symlink_policy: SymlinkPolicy::Reject,
            job_id: job_id.into(),
            created_at_unix_ms: 0,
        }
    }
}

/// Monotonic backup progress suitable for local APIs and native clients.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupProgress {
    /// Manifest entries fully completed and checkpointed.
    pub entries_completed: usize,
    /// Plaintext bytes read from stable file handles.
    pub bytes_read: u64,
    /// Newly committed encrypted chunks.
    pub chunks_stored: usize,
    /// Existing encrypted chunks reused by keyed locator.
    pub chunks_deduplicated: usize,
    /// Symlinks explicitly skipped under the selected policy.
    pub symlinks_skipped: usize,
    /// Entry currently being processed.
    pub current_path: Option<RelativePath>,
}

/// Final locally durable backup result.
#[derive(Clone, Debug)]
pub struct BackupResult {
    /// Decrypted manifest retained by the authorized owner.
    pub manifest: Manifest,
    /// Signed encrypted snapshot committed to the local store.
    pub stored_snapshot: StoredSnapshot,
    /// Final progress counters.
    pub progress: BackupProgress,
    /// Explicit provider acknowledgements and visible degraded state.
    pub replication: ReplicationReport,
}

pub(crate) struct ScannedBackup {
    pub manifest: Manifest,
    pub chunk_locators: BTreeSet<String>,
    pub progress: BackupProgress,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupCheckpoint {
    schema_version: u16,
    options_digest: String,
    source_canonical: String,
    entries: Vec<ManifestEntry>,
    fingerprints: BTreeMap<RelativePath, SourceFingerprint>,
    chunk_locators: BTreeSet<String>,
    progress: BackupProgress,
    #[serde(skip)]
    record_count: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum BackupWalRecord {
    Header {
        schema_version: u16,
        options_digest: String,
        source_canonical: String,
    },
    Entry {
        entry: Box<ManifestEntry>,
        fingerprint: SourceFingerprint,
        progress: BackupProgress,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceFingerprint {
    kind: EntryKind,
    length: u64,
    modified_at_unix_ms: Option<u64>,
    unix_device: Option<u64>,
    unix_inode: Option<u64>,
    unix_change_time_seconds: Option<i64>,
    unix_change_time_nanoseconds: Option<i64>,
    unix_mode: Option<u32>,
}

pub(crate) fn scan_source(
    source_root: &Path,
    options: &BackupOptions,
    key: &BackupKey,
    store: &ChunkStore,
    control: &JobControl,
    progress_callback: &mut dyn FnMut(&BackupProgress),
) -> Result<ScannedBackup, CoreError> {
    validate_options(options, store)?;
    let source_metadata = fs::symlink_metadata(source_root).map_err(|source| CoreError::Io {
        operation: "inspect backup source root",
        path: source_root.to_path_buf(),
        source,
    })?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(CoreError::InvalidAuthorizedRoot(source_root.to_path_buf()));
    }
    let canonical = fs::canonicalize(source_root).map_err(|source| CoreError::Io {
        operation: "canonicalize backup source root",
        path: source_root.to_path_buf(),
        source,
    })?;
    let canonical_string = canonical
        .to_str()
        .ok_or_else(|| CoreError::UnsupportedSourceEntry(canonical.clone()))?
        .to_owned();
    let stable_root = StableSourceRoot::open(&canonical)?;
    if !stable_root.matches_metadata(&source_metadata)? {
        return Err(CoreError::SourceChanged(source_root.to_path_buf()));
    }
    let options_digest = options_digest(options)?;
    let (mut checkpoint, resumed) =
        load_checkpoint(store, options, &canonical_string, &options_digest)?;
    let reusable: BTreeMap<_, _> = checkpoint
        .entries
        .iter()
        .cloned()
        .map(|entry| (entry.path.clone(), entry))
        .collect();
    let reusable_fingerprints = checkpoint.fingerprints.clone();
    checkpoint.entries.clear();
    checkpoint.fingerprints.clear();
    checkpoint.chunk_locators.clear();
    checkpoint.progress.current_path = None;
    checkpoint.progress.entries_completed = 0;
    checkpoint.progress.symlinks_skipped = 0;
    if !resumed {
        initialize_checkpoint_log(store, options, &mut checkpoint)?;
    }

    let mut walker = WalkDir::new(&canonical)
        .follow_links(false)
        .same_file_system(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(next) = walker.next() {
        check_control_with_durable_checkpoint(control, store, &options.job_id)?;
        let entry = next.map_err(map_walk_error)?;
        if entry.depth() == 0 {
            continue;
        }
        let relative = relative_path(&canonical, entry.path())?;
        if options.exclusions.matches(&relative) {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }
        checkpoint.progress.current_path = Some(relative.clone());
        progress_callback(&checkpoint.progress);

        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|source| map_source_io(entry.path(), source))?;
        if metadata.file_type().is_symlink() {
            match options.symlink_policy {
                SymlinkPolicy::Reject => {
                    return Err(CoreError::UnsupportedSourceEntry(
                        entry.path().to_path_buf(),
                    ));
                }
                SymlinkPolicy::Skip => {
                    checkpoint.progress.symlinks_skipped += 1;
                    continue;
                }
            }
        }

        if let (Some(existing), Some(fingerprint)) = (
            reusable.get(&relative),
            reusable_fingerprints.get(&relative),
        ) && reusable_entry_matches(existing, fingerprint, &metadata)
        {
            checkpoint.chunk_locators.extend(
                existing
                    .chunks
                    .iter()
                    .map(|reference| reference.opaque_locator.clone()),
            );
            checkpoint.entries.push(existing.clone());
            checkpoint.fingerprints.insert(
                relative.clone(),
                source_fingerprint(&metadata, existing.kind),
            );
            checkpoint.progress.entries_completed += 1;
            progress_callback(&checkpoint.progress);
            continue;
        }

        let (manifest_entry, fingerprint) = if metadata.is_dir() {
            let stable = stable_root.open_entry(&relative, true)?;
            let stable_metadata = stable
                .metadata()
                .map_err(|source| map_source_io(entry.path(), source))?;
            if !same_file_identity(&metadata, &stable_metadata) {
                return Err(CoreError::SourceChanged(entry.path().to_path_buf()));
            }
            let manifest_entry = ManifestEntry {
                path: relative,
                kind: EntryKind::Directory,
                length: 0,
                chunks: Vec::new(),
                metadata: portable_metadata(&stable_metadata, false),
                sparse_extents: Vec::new(),
            };
            let fingerprint = source_fingerprint(&stable_metadata, EntryKind::Directory);
            (manifest_entry, fingerprint)
        } else if metadata.is_file() {
            process_file(
                &stable_root,
                entry.path(),
                relative,
                &metadata,
                options,
                key,
                store,
                control,
                &mut checkpoint,
                progress_callback,
            )?
        } else {
            return Err(CoreError::UnsupportedSourceEntry(
                entry.path().to_path_buf(),
            ));
        };
        checkpoint.chunk_locators.extend(
            manifest_entry
                .chunks
                .iter()
                .map(|reference| reference.opaque_locator.clone()),
        );
        checkpoint.entries.push(manifest_entry);
        checkpoint.fingerprints.insert(
            checkpoint
                .entries
                .last()
                .ok_or_else(|| CoreError::InvalidState("missing backup entry".to_owned()))?
                .path
                .clone(),
            fingerprint,
        );
        checkpoint.progress.entries_completed += 1;
        checkpoint.progress.current_path = None;
        append_checkpoint_entry(store, options, &mut checkpoint)?;
        progress_callback(&checkpoint.progress);
    }

    store.sync_checkpoint_records(&options.job_id)?;
    check_control_with_durable_checkpoint(control, store, &options.job_id)?;
    if !stable_root.path_is_unchanged()? {
        return Err(CoreError::SourceChanged(canonical));
    }

    checkpoint
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = Manifest {
        protocol_version: PROTOCOL_VERSION,
        backup_id: options.backup_id,
        snapshot_id: options.snapshot_id.clone(),
        created_at_unix_ms: options.created_at_unix_ms,
        replica_intent: options.replica_intent.clone(),
        entries: checkpoint.entries,
        provider_acknowledgements: BTreeMap::new(),
    };
    manifest.validate()?;
    Ok(ScannedBackup {
        manifest,
        chunk_locators: checkpoint.chunk_locators,
        progress: checkpoint.progress,
    })
}

#[allow(clippy::too_many_arguments)]
fn process_file(
    stable_root: &StableSourceRoot,
    path: &Path,
    relative: RelativePath,
    initial_metadata: &fs::Metadata,
    options: &BackupOptions,
    key: &BackupKey,
    store: &ChunkStore,
    control: &JobControl,
    checkpoint: &mut BackupCheckpoint,
    progress_callback: &mut dyn FnMut(&BackupProgress),
) -> Result<(ManifestEntry, SourceFingerprint), CoreError> {
    let mut file = stable_root.open_entry(&relative, false)?;
    let opened_metadata = file
        .metadata()
        .map_err(|source| map_source_io(path, source))?;
    if !same_file_identity(initial_metadata, &opened_metadata) {
        return Err(CoreError::SourceChanged(path.to_path_buf()));
    }

    let length = opened_metadata.len();
    let discovered_extents = discover_sparse_extents(&file, length)?;
    let is_sparse = discovered_extents
        .as_ref()
        .is_some_and(|extents| extents.iter().map(|(_, length)| *length).sum::<u64>() < length);
    let extents = discovered_extents.unwrap_or_else(|| {
        if length == 0 {
            Vec::new()
        } else {
            vec![(0, length)]
        }
    });
    let mut references = Vec::new();
    let mut sparse_extents = Vec::new();

    for (offset, extent_length) in extents {
        check_control_with_durable_checkpoint(control, store, &options.job_id)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| map_source_io(path, source))?;
        let mut limited = (&mut file).take(extent_length);
        let mut chunker = ContentDefinedChunker::new(&mut limited, options.chunking);
        let mut processed = 0_u64;
        while let Some(bytes) = chunker
            .next_chunk()
            .map_err(|source| map_source_io(path, source))?
        {
            check_control_with_durable_checkpoint(control, store, &options.job_id)?;
            processed = processed.saturating_add(bytes.len() as u64);
            checkpoint.progress.bytes_read = checkpoint
                .progress
                .bytes_read
                .saturating_add(bytes.len() as u64);
            let encrypted = key.encrypt_chunk(options.backup_id, options.key_epoch, &bytes)?;
            if store.put(&encrypted)? {
                checkpoint.progress.chunks_stored += 1;
            } else {
                checkpoint.progress.chunks_deduplicated += 1;
            }
            references.push(ChunkReference {
                plaintext_digest: encrypted.plaintext_digest.clone(),
                opaque_locator: encrypted.opaque_locator.clone(),
                plaintext_length: encrypted.plaintext_length,
                ciphertext_length: encrypted.ciphertext_length(),
            });
            checkpoint.chunk_locators.insert(encrypted.opaque_locator);
            progress_callback(&checkpoint.progress);
        }
        if processed != extent_length {
            return Err(CoreError::SourceChanged(path.to_path_buf()));
        }
        if is_sparse && extent_length > 0 {
            sparse_extents.push(SparseExtent {
                offset,
                length: extent_length,
            });
        }
    }

    let final_open_metadata = file
        .metadata()
        .map_err(|source| map_source_io(path, source))?;
    let final_path_metadata =
        fs::symlink_metadata(path).map_err(|source| map_source_io(path, source))?;
    if final_path_metadata.file_type().is_symlink()
        || !same_file_identity(&opened_metadata, &final_open_metadata)
        || !same_file_identity(&opened_metadata, &final_path_metadata)
        || final_open_metadata.len() != length
        || source_fingerprint(&opened_metadata, EntryKind::File)
            != source_fingerprint(&final_open_metadata, EntryKind::File)
        || source_fingerprint(&final_open_metadata, EntryKind::File)
            != source_fingerprint(&final_path_metadata, EntryKind::File)
    {
        return Err(CoreError::SourceChanged(path.to_path_buf()));
    }

    let fingerprint = source_fingerprint(&final_open_metadata, EntryKind::File);
    Ok((
        ManifestEntry {
            path: relative,
            kind: EntryKind::File,
            length,
            chunks: references,
            metadata: portable_metadata(&opened_metadata, is_sparse),
            sparse_extents,
        },
        fingerprint,
    ))
}

fn validate_options(options: &BackupOptions, store: &ChunkStore) -> Result<(), CoreError> {
    if options.key_epoch == 0
        || options.display_name.trim().is_empty()
        || options.display_name.len() > 120
        || options.display_name.chars().any(char::is_control)
        || !options.chunking.is_valid()
        || options.chunking.maximum_size > store.maximum_chunk_size()
        || options.job_id.is_empty()
        || options.job_id.len() > 128
        || !options
            .job_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || options.snapshot_id.is_empty()
        || options.snapshot_id.len() > 128
        || !options
            .snapshot_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidState("invalid backup options".to_owned()));
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OptionsFingerprint<'a> {
    backup_id: BackupId,
    display_name: &'a str,
    snapshot_id: &'a str,
    key_epoch: u64,
    replica_intent: &'a ReplicaIntent,
    minimum_chunk_size: usize,
    average_chunk_size: usize,
    maximum_chunk_size: usize,
    exclusions: &'a [String],
    symlink_policy: SymlinkPolicy,
}

fn options_digest(options: &BackupOptions) -> Result<String, CoreError> {
    Ok(blake3::hash(&serde_json::to_vec(&OptionsFingerprint {
        backup_id: options.backup_id,
        display_name: &options.display_name,
        snapshot_id: &options.snapshot_id,
        key_epoch: options.key_epoch,
        replica_intent: &options.replica_intent,
        minimum_chunk_size: options.chunking.minimum_size,
        average_chunk_size: options.chunking.average_size,
        maximum_chunk_size: options.chunking.maximum_size,
        exclusions: options.exclusions.patterns(),
        symlink_policy: options.symlink_policy,
    })?)
    .to_hex()
    .to_string())
}

fn load_checkpoint(
    store: &ChunkStore,
    options: &BackupOptions,
    source_canonical: &str,
    options_digest: &str,
) -> Result<(BackupCheckpoint, bool), CoreError> {
    if let Some(records) = store.read_checkpoint_records(&options.job_id)? {
        return replay_checkpoint_log(store, options, source_canonical, options_digest, records)
            .map(|checkpoint| (checkpoint, true));
    }
    if let Some(bytes) = store.read_checkpoint(&options.job_id)? {
        let checkpoint: BackupCheckpoint = serde_json::from_slice(&bytes)?;
        if checkpoint.schema_version != LEGACY_BACKUP_CHECKPOINT_SCHEMA_VERSION
            || checkpoint.options_digest != options_digest
            || checkpoint.source_canonical != source_canonical
        {
            return Err(CoreError::InvalidState(
                "backup checkpoint does not match this job".to_owned(),
            ));
        }
        let mut checkpoint = checkpoint;
        migrate_legacy_checkpoint(store, options, &mut checkpoint)?;
        return Ok((checkpoint, true));
    }
    Ok((
        BackupCheckpoint {
            schema_version: LEGACY_BACKUP_CHECKPOINT_SCHEMA_VERSION,
            options_digest: options_digest.to_owned(),
            source_canonical: source_canonical.to_owned(),
            entries: Vec::new(),
            fingerprints: BTreeMap::new(),
            chunk_locators: BTreeSet::new(),
            progress: BackupProgress::default(),
            record_count: 0,
        },
        false,
    ))
}

fn initialize_checkpoint_log(
    store: &ChunkStore,
    options: &BackupOptions,
    checkpoint: &mut BackupCheckpoint,
) -> Result<(), CoreError> {
    let record = BackupWalRecord::Header {
        schema_version: BACKUP_WAL_SCHEMA_VERSION,
        options_digest: checkpoint.options_digest.clone(),
        source_canonical: checkpoint.source_canonical.clone(),
    };
    store.append_checkpoint_record(&options.job_id, &serde_json::to_vec(&record)?)?;
    checkpoint.record_count = 1;
    Ok(())
}

fn append_checkpoint_entry(
    store: &ChunkStore,
    options: &BackupOptions,
    checkpoint: &mut BackupCheckpoint,
) -> Result<(), CoreError> {
    let entry = checkpoint
        .entries
        .last()
        .ok_or_else(|| CoreError::InvalidState("missing completed backup entry".to_owned()))?;
    let fingerprint = checkpoint
        .fingerprints
        .get(&entry.path)
        .ok_or_else(|| CoreError::InvalidState("missing source fingerprint".to_owned()))?;
    let record = BackupWalRecord::Entry {
        entry: Box::new(entry.clone()),
        fingerprint: fingerprint.clone(),
        progress: checkpoint.progress.clone(),
    };
    let next_record_count = checkpoint.record_count.saturating_add(1);
    store.append_checkpoint_record_buffered(
        &options.job_id,
        &serde_json::to_vec(&record)?,
        next_record_count.is_multiple_of(BACKUP_WAL_SYNC_INTERVAL),
    )?;
    checkpoint.record_count = next_record_count;
    if backup_checkpoint_needs_compaction(checkpoint) {
        compact_checkpoint_log(store, options, checkpoint)?;
        checkpoint.record_count = checkpoint.entries.len().saturating_add(1);
    }
    Ok(())
}

fn check_control_with_durable_checkpoint(
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

fn replay_checkpoint_log(
    store: &ChunkStore,
    options: &BackupOptions,
    source_canonical: &str,
    options_digest: &str,
    records: Vec<Vec<u8>>,
) -> Result<BackupCheckpoint, CoreError> {
    let mut records = records.into_iter();
    let header: BackupWalRecord = serde_json::from_slice(
        &records
            .next()
            .ok_or_else(|| CoreError::InvalidState("empty backup checkpoint log".to_owned()))?,
    )?;
    match header {
        BackupWalRecord::Header {
            schema_version,
            options_digest: persisted_options,
            source_canonical: persisted_source,
        } if schema_version == BACKUP_WAL_SCHEMA_VERSION
            && persisted_options == options_digest
            && persisted_source == source_canonical => {}
        _ => {
            return Err(CoreError::InvalidState(
                "backup checkpoint does not match this job".to_owned(),
            ));
        }
    }
    let mut entries = BTreeMap::new();
    let mut fingerprints = BTreeMap::new();
    let mut progress = BackupProgress::default();
    let mut record_count = 1_usize;
    for bytes in records {
        match serde_json::from_slice(&bytes)? {
            BackupWalRecord::Entry {
                entry,
                fingerprint,
                progress: recorded_progress,
            } => {
                fingerprints.insert(entry.path.clone(), fingerprint);
                entries.insert(entry.path.clone(), *entry);
                progress = recorded_progress;
                record_count = record_count.saturating_add(1);
            }
            BackupWalRecord::Header { .. } => {
                return Err(CoreError::InvalidState(
                    "duplicate backup checkpoint header".to_owned(),
                ));
            }
        }
    }
    let entries: Vec<_> = entries.into_values().collect();
    let chunk_locators = entries
        .iter()
        .flat_map(|entry| &entry.chunks)
        .map(|reference| reference.opaque_locator.clone())
        .collect();
    let checkpoint = BackupCheckpoint {
        schema_version: LEGACY_BACKUP_CHECKPOINT_SCHEMA_VERSION,
        options_digest: options_digest.to_owned(),
        source_canonical: source_canonical.to_owned(),
        entries,
        fingerprints,
        chunk_locators,
        progress,
        record_count,
    };
    if backup_checkpoint_needs_compaction(&checkpoint) {
        compact_checkpoint_log(store, options, &checkpoint)?;
    }
    Ok(checkpoint)
}

fn backup_checkpoint_needs_compaction(checkpoint: &BackupCheckpoint) -> bool {
    let live_records = checkpoint.entries.len().saturating_add(1);
    let stale_records = checkpoint.record_count.saturating_sub(live_records);
    stale_records
        > BACKUP_WAL_COMPACTION_STALE_RECORDS.max(checkpoint.entries.len().saturating_div(2))
}

fn migrate_legacy_checkpoint(
    store: &ChunkStore,
    options: &BackupOptions,
    checkpoint: &mut BackupCheckpoint,
) -> Result<(), CoreError> {
    checkpoint.record_count = checkpoint.entries.len().saturating_add(1);
    compact_checkpoint_log(store, options, checkpoint)
}

fn compact_checkpoint_log(
    store: &ChunkStore,
    options: &BackupOptions,
    checkpoint: &BackupCheckpoint,
) -> Result<(), CoreError> {
    let mut records = Vec::with_capacity(checkpoint.entries.len().saturating_add(1));
    records.push(serde_json::to_vec(&BackupWalRecord::Header {
        schema_version: BACKUP_WAL_SCHEMA_VERSION,
        options_digest: checkpoint.options_digest.clone(),
        source_canonical: checkpoint.source_canonical.clone(),
    })?);
    for entry in &checkpoint.entries {
        let fingerprint = checkpoint
            .fingerprints
            .get(&entry.path)
            .ok_or_else(|| CoreError::InvalidState("missing source fingerprint".to_owned()))?;
        records.push(serde_json::to_vec(&BackupWalRecord::Entry {
            entry: Box::new(entry.clone()),
            fingerprint: fingerprint.clone(),
            progress: checkpoint.progress.clone(),
        })?);
    }
    store.replace_checkpoint_records(&options.job_id, &records)
}

fn relative_path(root: &Path, path: &Path) -> Result<RelativePath, CoreError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CoreError::EscapedAuthorizedRoot(path.to_path_buf()))?;
    let mut components = Vec::new();
    for component in relative.components() {
        let value = component
            .as_os_str()
            .to_str()
            .ok_or_else(|| CoreError::UnsupportedSourceEntry(path.to_path_buf()))?;
        components.push(value);
    }
    RelativePath::new(components.join("/")).map_err(CoreError::from)
}

fn reusable_entry_matches(
    entry: &ManifestEntry,
    fingerprint: &SourceFingerprint,
    metadata: &fs::Metadata,
) -> bool {
    let kind_matches = match entry.kind {
        EntryKind::Directory => metadata.is_dir(),
        EntryKind::File => metadata.is_file(),
    };
    kind_matches && *fingerprint == source_fingerprint(metadata, entry.kind)
}

fn source_fingerprint(metadata: &fs::Metadata, kind: EntryKind) -> SourceFingerprint {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        SourceFingerprint {
            kind,
            length: metadata.len(),
            modified_at_unix_ms: modified_millis(metadata),
            unix_device: Some(metadata.dev()),
            unix_inode: Some(metadata.ino()),
            unix_change_time_seconds: Some(metadata.ctime()),
            unix_change_time_nanoseconds: Some(metadata.ctime_nsec()),
            unix_mode: Some(metadata.mode()),
        }
    }
    #[cfg(not(unix))]
    {
        SourceFingerprint {
            kind,
            length: metadata.len(),
            modified_at_unix_ms: modified_millis(metadata),
            unix_device: None,
            unix_inode: None,
            unix_change_time_seconds: None,
            unix_change_time_nanoseconds: None,
            unix_mode: None,
        }
    }
}

fn modified_millis(metadata: &fs::Metadata) -> Option<u64> {
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn portable_metadata(metadata: &fs::Metadata, sparse: bool) -> EntryMetadata {
    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o7777)
    };
    #[cfg(not(unix))]
    let unix_mode = None;

    EntryMetadata {
        modified_at_unix_ms: modified_millis(metadata),
        unix_mode,
        sparse,
    }
}

fn map_walk_error(error: walkdir::Error) -> CoreError {
    let path = error
        .path()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("<source>"));
    if error
        .io_error()
        .is_some_and(|value| value.kind() == std::io::ErrorKind::PermissionDenied)
    {
        CoreError::SourcePermissionDenied(path)
    } else {
        CoreError::InvalidState(format!(
            "source traversal failed at {}: {error}",
            path.display()
        ))
    }
}

fn map_source_io(path: &Path, source: std::io::Error) -> CoreError {
    if source.kind() == std::io::ErrorKind::PermissionDenied {
        CoreError::SourcePermissionDenied(path.to_path_buf())
    } else {
        CoreError::Io {
            operation: "read stable backup source",
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(unix)]
struct StableSourceRoot {
    canonical: PathBuf,
    descriptor: std::os::fd::OwnedFd,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl StableSourceRoot {
    fn open(canonical: &Path) -> Result<Self, CoreError> {
        use rustix::fs::{Mode, OFlags, fstat, open};

        let descriptor = open(
            canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| source_os_error("open stable source root", canonical, error))?;
        let stat = fstat(&descriptor)
            .map_err(|error| source_os_error("inspect stable source root", canonical, error))?;
        Ok(Self {
            canonical: canonical.to_path_buf(),
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
            descriptor,
        })
    }

    fn matches_metadata(&self, metadata: &fs::Metadata) -> Result<bool, CoreError> {
        use std::os::unix::fs::MetadataExt;
        Ok(metadata.dev() == self.device && metadata.ino() == self.inode)
    }

    fn open_entry(&self, relative: &RelativePath, directory: bool) -> Result<File, CoreError> {
        use rustix::fs::{Mode, OFlags, openat};

        let mut current = rustix::io::dup(&self.descriptor).map_err(|error| {
            source_os_error("duplicate stable source root", &self.canonical, error)
        })?;
        let components: Vec<_> = relative.components().collect();
        for (index, component) in components.iter().enumerate() {
            let final_component = index + 1 == components.len();
            let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
            if !final_component || directory {
                flags |= OFlags::DIRECTORY;
            }
            current = openat(&current, *component, flags, Mode::empty()).map_err(|error| {
                let path = self.canonical.join(relative.as_str());
                if matches!(error, rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) {
                    CoreError::SourceChanged(path)
                } else {
                    source_os_error("open stable source entry", &path, error)
                }
            })?;
        }
        Ok(File::from(current))
    }

    fn path_is_unchanged(&self) -> Result<bool, CoreError> {
        use rustix::fs::{Mode, OFlags, fstat, open};

        let reopened = match open(
            &self.canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                return Ok(false);
            }
            Err(error) => {
                return Err(source_os_error(
                    "reopen stable source root",
                    &self.canonical,
                    error,
                ));
            }
        };
        let stat = fstat(&reopened).map_err(|error| {
            source_os_error("recheck stable source root", &self.canonical, error)
        })?;
        Ok(stat.st_dev as u64 == self.device && stat.st_ino as u64 == self.inode)
    }
}

#[cfg(unix)]
fn source_os_error(operation: &'static str, path: &Path, error: rustix::io::Errno) -> CoreError {
    let source = std::io::Error::from_raw_os_error(error.raw_os_error());
    if error == rustix::io::Errno::ACCESS || error == rustix::io::Errno::PERM {
        CoreError::SourcePermissionDenied(path.to_path_buf())
    } else {
        CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

#[cfg(not(unix))]
struct StableSourceRoot {
    canonical: PathBuf,
}

#[cfg(not(unix))]
impl StableSourceRoot {
    fn open(canonical: &Path) -> Result<Self, CoreError> {
        Ok(Self {
            canonical: canonical.to_path_buf(),
        })
    }

    fn matches_metadata(&self, metadata: &fs::Metadata) -> Result<bool, CoreError> {
        let current = fs::metadata(&self.canonical)
            .map_err(|source| map_source_io(&self.canonical, source))?;
        Ok(same_file_identity(metadata, &current))
    }

    fn open_entry(&self, relative: &RelativePath, directory: bool) -> Result<File, CoreError> {
        let path = self.canonical.join(relative.as_str());
        let canonical = fs::canonicalize(&path).map_err(|source| map_source_io(&path, source))?;
        if !canonical.starts_with(&self.canonical) {
            return Err(CoreError::SourceChanged(path));
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| map_source_io(&path, source))?;
        if metadata.file_type().is_symlink() || (directory && !metadata.is_dir()) {
            return Err(CoreError::SourceChanged(path));
        }
        File::open(&path).map_err(|source| map_source_io(&path, source))
    }

    fn path_is_unchanged(&self) -> Result<bool, CoreError> {
        Ok(fs::canonicalize(&self.canonical).is_ok_and(|current| current == self.canonical))
    }
}

#[cfg(unix)]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len() && modified_millis(left) == modified_millis(right)
}

#[cfg(unix)]
fn discover_sparse_extents(file: &File, length: u64) -> Result<Option<Vec<(u64, u64)>>, CoreError> {
    use rustix::fs::{SeekFrom as RustixSeekFrom, seek};
    use rustix::io::Errno;

    if length == 0 {
        return Ok(Some(Vec::new()));
    }
    let mut extents = Vec::new();
    let mut position = 0_u64;
    while position < length {
        let data = match seek(file, RustixSeekFrom::Data(position)) {
            Ok(value) => value,
            Err(Errno::NXIO) => break,
            Err(Errno::INVAL | Errno::NOTSUP) => return Ok(None),
            Err(error) => {
                return Err(CoreError::Io {
                    operation: "discover sparse data extent",
                    path: PathBuf::from("<open source>"),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        };
        if data >= length {
            break;
        }
        let hole = match seek(file, RustixSeekFrom::Hole(data)) {
            Ok(value) => value.min(length),
            Err(Errno::NXIO) => length,
            Err(Errno::INVAL | Errno::NOTSUP) => return Ok(None),
            Err(error) => {
                return Err(CoreError::Io {
                    operation: "discover sparse hole extent",
                    path: PathBuf::from("<open source>"),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                });
            }
        };
        if hole <= data {
            return Ok(None);
        }
        extents.push((data, hole - data));
        position = hole;
    }
    Ok(Some(extents))
}

#[cfg(not(unix))]
fn discover_sparse_extents(
    _file: &File,
    _length: u64,
) -> Result<Option<Vec<(u64, u64)>>, CoreError> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::JobControl;

    use super::*;

    #[test]
    fn traversal_preserves_nested_empty_dirs_and_deduplicates() {
        let source = tempdir().expect("source");
        fs::create_dir_all(source.path().join("nested/empty")).expect("directories");
        fs::write(source.path().join("nested/a.txt"), b"same").expect("file");
        fs::write(source.path().join("nested/b.txt"), b"same").expect("file");
        let data = tempdir().expect("data");
        let store = ChunkStore::open(data.path(), 1_048_576).expect("store");
        let mut options = BackupOptions::new(BackupId::new(), "snapshot-1", "job-1");
        options.chunking = ChunkingConfig::new(4_096, 8_192, 16_384).expect("config");
        let mut observed = BackupProgress::default();
        let scanned = scan_source(
            source.path(),
            &options,
            &BackupKey::generate(),
            &store,
            &JobControl::new(),
            &mut |progress| observed = progress.clone(),
        )
        .expect("scan");
        assert!(scanned.manifest.entries.iter().any(|entry| {
            entry.path.as_str() == "nested/empty" && entry.kind == EntryKind::Directory
        }));
        let files: Vec<_> = scanned
            .manifest
            .entries
            .iter()
            .filter(|entry| entry.kind == EntryKind::File)
            .collect();
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[0].chunks[0].opaque_locator,
            files[1].chunks[0].opaque_locator
        );
        assert_eq!(observed.entries_completed, scanned.manifest.entries.len());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let source = tempdir().expect("source");
        let outside = tempdir().expect("outside");
        fs::write(outside.path().join("secret"), b"secret").expect("secret");
        symlink(outside.path().join("secret"), source.path().join("link")).expect("link");
        let data = tempdir().expect("data");
        let store = ChunkStore::open(data.path(), 1_048_576).expect("store");
        let options = BackupOptions::new(BackupId::new(), "snapshot", "job");
        assert!(matches!(
            scan_source(
                source.path(),
                &options,
                &BackupKey::generate(),
                &store,
                &JobControl::new(),
                &mut |_| {}
            ),
            Err(CoreError::UnsupportedSourceEntry(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn source_path_swap_during_read_fails_closed() {
        use std::os::unix::fs::symlink;

        let source = tempdir().expect("source");
        let outside = tempdir().expect("outside");
        let source_path = source.path().join("victim.bin");
        let displaced_path = source.path().join("original.bin");
        let outside_path = outside.path().join("secret.bin");
        fs::write(&source_path, vec![0x5a; 512 * 1_024]).expect("source file");
        fs::write(&outside_path, b"must never be followed").expect("outside file");
        let data = tempdir().expect("data");
        let store = ChunkStore::open(data.path(), 1_048_576).expect("store");
        let options = BackupOptions::new(BackupId::new(), "snapshot", "swap-job");
        let mut swapped = false;
        let result = scan_source(
            source.path(),
            &options,
            &BackupKey::generate(),
            &store,
            &JobControl::new(),
            &mut |progress| {
                if !swapped && progress.bytes_read > 0 {
                    fs::rename(&source_path, &displaced_path).expect("displace source");
                    symlink(&outside_path, &source_path).expect("swap symlink");
                    swapped = true;
                }
            },
        );
        assert!(swapped);
        assert!(matches!(result, Err(CoreError::SourceChanged(_))));
    }

    #[test]
    fn exclusion_rules_skip_whole_subtrees() {
        let rules =
            ExclusionRules::new(vec!["private/**".to_owned(), "*.tmp".to_owned()]).expect("rules");
        assert!(rules.matches(&RelativePath::new("private/key").expect("path")));
        assert!(rules.matches(&RelativePath::new("cache.tmp").expect("path")));
        assert!(!rules.matches(&RelativePath::new("public/key").expect("path")));
    }
}
