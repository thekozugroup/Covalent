use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use covalent_protocol::{BackupId, Manifest, ManifestEnvelope};
use serde::{Deserialize, Serialize};

use crate::atomic::{
    append_record_log, read_bounded, read_record_log, rewrite_record_log, sync_directory,
    sync_record_log, write_atomic, write_atomic_noclobber, write_json_atomic,
};
use crate::crypto::validate_hex_locator;
use crate::{BackupKey, CoreError, EncryptedChunk};

const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const MAX_SNAPSHOT_METADATA_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_CHECKPOINT_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_CHECKPOINT_LOG_BYTES: u64 = 512 * 1_024 * 1_024;
const GARBAGE_COLLECTION_BATCH_SIZE: usize = 1_024;
const RETENTION_INDEX_LINE_BYTES: u64 = 65;
const MAX_RETENTION_INDEX_PREFIX_BYTES: u64 = 64 * 1_024 * 1_024;

/// Durably committed encrypted snapshot metadata and its retention roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoredSnapshot {
    /// Persisted metadata schema.
    pub schema_version: u16,
    /// Logical backup identifier.
    pub backup_id: BackupId,
    /// Validated snapshot identifier.
    pub snapshot_id: String,
    /// Signed and encrypted manifest envelope.
    pub envelope: ManifestEnvelope,
    /// Exact local encrypted objects retained by this snapshot.
    pub chunk_locators: BTreeSet<String>,
    /// Commit time for status display.
    pub committed_at_unix_ms: u64,
}

impl StoredSnapshot {
    /// Creates validated retention metadata.
    pub fn new(
        backup_id: BackupId,
        snapshot_id: impl Into<String>,
        envelope: ManifestEnvelope,
        chunk_locators: BTreeSet<String>,
        committed_at_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        let snapshot_id = snapshot_id.into();
        validate_snapshot_id(&snapshot_id)?;
        if envelope.backup_id != backup_id {
            return Err(CoreError::InvalidState(
                "snapshot envelope backup mismatch".to_owned(),
            ));
        }
        for locator in &chunk_locators {
            validate_hex_locator(locator)?;
        }
        Ok(Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            backup_id,
            snapshot_id,
            envelope,
            chunk_locators,
            committed_at_unix_ms,
        })
    }

    fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != SNAPSHOT_SCHEMA_VERSION
            || self.envelope.backup_id != self.backup_id
        {
            return Err(CoreError::InvalidState(
                "unsupported or inconsistent snapshot metadata".to_owned(),
            ));
        }
        validate_snapshot_id(&self.snapshot_id)?;
        for locator in &self.chunk_locators {
            validate_hex_locator(locator)?;
        }
        Ok(())
    }
}

/// Result of retention-safe garbage collection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GarbageCollectionReport {
    /// Objects retained by at least one committed snapshot.
    pub retained: usize,
    /// Unreferenced objects removed after metadata was fully validated.
    pub removed: usize,
    /// Bytes reclaimed.
    pub reclaimed_bytes: u64,
    /// True when an active resumable job conservatively deferred all deletion.
    pub deferred_active_jobs: bool,
}

/// Per-manifest authenticated verification result.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IntegrityReport {
    /// Authenticated chunks checked successfully.
    pub verified: usize,
    /// Missing opaque locators.
    pub missing: Vec<String>,
    /// Present but malformed, unauthenticated, or digest-mismatched locators.
    pub corrupt: Vec<String>,
}

impl IntegrityReport {
    /// True only when every referenced object was present and valid.
    #[must_use]
    pub fn is_intact(&self) -> bool {
        self.missing.is_empty() && self.corrupt.is_empty()
    }
}

/// Crash-safe local encrypted object and metadata store.
#[derive(Clone, Debug)]
pub struct ChunkStore {
    root: PathBuf,
    maximum_chunk_size: usize,
    transaction_lock: Arc<Mutex<()>>,
    snapshot_generation: Arc<AtomicU64>,
}

pub(crate) struct RetentionIndexBuilder {
    directory: tempfile::TempDir,
    writers: std::collections::BTreeMap<u8, BufWriter<fs::File>>,
    expected_snapshot_generation: u64,
}

pub(crate) struct RetentionIndex {
    directory: tempfile::TempDir,
    expected_snapshot_generation: u64,
    unique_locators: usize,
}

impl ChunkStore {
    /// Opens or creates a store rooted at an explicitly configured data directory.
    pub fn open(root: impl AsRef<Path>, maximum_chunk_size: usize) -> Result<Self, CoreError> {
        if !(4 * 1_024..=8 * 1_024 * 1_024).contains(&maximum_chunk_size) {
            return Err(CoreError::ResourceLimit("maximum stored chunk size"));
        }
        let root = root.as_ref().to_path_buf();
        ensure_private_directory(&root)?;
        for directory in [
            "chunks",
            "snapshots",
            "jobs",
            "quarantine",
            "trash",
            "gc-work",
        ] {
            let path = root.join(directory);
            ensure_private_directory(&path)?;
        }
        let root = fs::canonicalize(&root).map_err(|source| CoreError::Io {
            operation: "canonicalize chunk store root",
            path: root,
            source,
        })?;
        Ok(Self {
            root,
            maximum_chunk_size,
            transaction_lock: Arc::new(Mutex::new(())),
            snapshot_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Store root for diagnostics only.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Maximum accepted plaintext chunk size.
    #[must_use]
    pub const fn maximum_chunk_size(&self) -> usize {
        self.maximum_chunk_size
    }

    /// Deduplicates and durably writes one encrypted chunk.
    ///
    /// Returns `true` when a new record was committed and `false` when an exact
    /// durable copy already existed.
    pub fn put(&self, chunk: &EncryptedChunk) -> Result<bool, CoreError> {
        self.put_provider_record(&chunk.opaque_locator, &chunk.encode_provider_record())
    }

    /// Stores an opaque provider record without learning its plaintext digest.
    pub fn put_provider_record(&self, locator: &str, record: &[u8]) -> Result<bool, CoreError> {
        validate_hex_locator(locator)?;
        validate_record_bounds(record, self.maximum_chunk_size)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let path = self.chunk_path(locator)?;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidState("chunk path has no parent".to_owned()))?;
        ensure_private_directory(parent)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "chunk path is not a regular file".to_owned(),
                    ));
                }
                let existing =
                    read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?;
                if existing == record {
                    return Ok(false);
                }
                return Err(CoreError::InvalidState(
                    "immutable chunk locator already contains different bytes".to_owned(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect encrypted chunk",
                    path,
                    source,
                });
            }
        }
        if write_atomic_noclobber(&path, record, false)? {
            return Ok(true);
        }
        let existing = read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?;
        if existing == record {
            Ok(false)
        } else {
            Err(CoreError::InvalidState(
                "immutable chunk locator already contains different bytes".to_owned(),
            ))
        }
    }

    /// Reads one bounded opaque provider record.
    pub fn get_provider_record(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
        validate_hex_locator(locator)?;
        let path = self.chunk_path(locator)?;
        match read_bounded(&path, self.maximum_chunk_size + provider_record_overhead()) {
            Ok(record) => {
                validate_record_bounds(&record, self.maximum_chunk_size)?;
                Ok(record)
            }
            Err(CoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Err(CoreError::MissingChunk(locator.to_owned()))
            }
            Err(error) => Err(error),
        }
    }

    /// Returns whether a regular record exists for one valid locator.
    pub fn contains(&self, locator: &str) -> Result<bool, CoreError> {
        validate_hex_locator(locator)?;
        let path = self.chunk_path(locator)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => Ok(metadata.is_file() && !metadata.file_type().is_symlink()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(CoreError::Io {
                operation: "inspect encrypted chunk",
                path,
                source,
            }),
        }
    }

    /// Makes a signed snapshot visible only after all local retention roots are durable.
    pub fn commit_snapshot(&self, snapshot: &StoredSnapshot) -> Result<(), CoreError> {
        snapshot.validate()?;
        // Hold the same lock as GC from the first reachability check through the
        // metadata commit so a collector cannot delete a newly referenced chunk
        // between those two operations.
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        for locator in &snapshot.chunk_locators {
            if !self.contains(locator)? {
                return Err(CoreError::MissingChunk(locator.clone()));
            }
        }
        let path = self.snapshot_path(snapshot.backup_id, &snapshot.snapshot_id)?;
        if let Some(parent) = path.parent() {
            ensure_private_directory(parent)?;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "snapshot metadata path is not a regular file".to_owned(),
                    ));
                }
                let existing = read_snapshot_metadata(&path)?;
                existing.validate()?;
                return if &existing == snapshot {
                    Ok(())
                } else {
                    Err(CoreError::InvalidState(
                        "snapshot identifier is already immutably committed".to_owned(),
                    ))
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect immutable snapshot metadata",
                    path,
                    source,
                });
            }
        }
        if !write_atomic_noclobber(&path, &serde_json::to_vec_pretty(snapshot)?, false)? {
            let incumbent = read_snapshot_metadata(&path)?;
            self.snapshot_generation.fetch_add(1, Ordering::Release);
            return if &incumbent == snapshot {
                Ok(())
            } else {
                Err(CoreError::InvalidState(
                    "snapshot identifier is already immutably committed".to_owned(),
                ))
            };
        }
        self.snapshot_generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// Loads and validates one committed snapshot.
    pub(crate) fn load_snapshot(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<StoredSnapshot, CoreError> {
        let path = self.snapshot_path(backup_id, snapshot_id)?;
        let snapshot = read_snapshot_metadata(&path)?;
        snapshot.validate()?;
        if snapshot.backup_id != backup_id || snapshot.snapshot_id != snapshot_id {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(snapshot)
    }

    /// Lists committed snapshots after validating every metadata record.
    pub(crate) fn list_snapshots(&self) -> Result<Vec<StoredSnapshot>, CoreError> {
        let mut snapshots = Vec::new();
        for (backup_id, snapshot_id) in self.list_snapshot_ids()? {
            snapshots.push(self.load_snapshot(backup_id, &snapshot_id)?);
        }
        Ok(snapshots)
    }

    pub(crate) fn list_snapshot_ids(&self) -> Result<Vec<(BackupId, String)>, CoreError> {
        let mut snapshots = Vec::new();
        let root = self.root.join("snapshots");
        for backup_entry in read_directory_sorted(&root)? {
            let directory_backup_id =
                BackupId::from_str(&backup_entry.file_name().to_string_lossy()).map_err(|_| {
                    CoreError::InvalidState("invalid snapshot backup directory".to_owned())
                })?;
            let metadata =
                fs::symlink_metadata(backup_entry.path()).map_err(|source| CoreError::Io {
                    operation: "inspect snapshot backup directory",
                    path: backup_entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::InvalidState(
                    "unexpected snapshot metadata entry".to_owned(),
                ));
            }
            for snapshot_entry in read_directory_sorted(&backup_entry.path())? {
                let path = snapshot_entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                    operation: "inspect snapshot metadata",
                    path: path.clone(),
                    source,
                })?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("json")
                {
                    return Err(CoreError::InvalidState(
                        "unexpected snapshot metadata entry".to_owned(),
                    ));
                }
                let path_snapshot_id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| {
                        CoreError::InvalidState("invalid snapshot metadata name".to_owned())
                    })?;
                validate_snapshot_id(path_snapshot_id)?;
                snapshots.push((directory_backup_id, path_snapshot_id.to_owned()));
            }
        }
        Ok(snapshots)
    }

    /// Removes one snapshot metadata root. Chunks remain until explicit garbage collection.
    pub fn delete_snapshot(&self, backup_id: BackupId, snapshot_id: &str) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let path = self.snapshot_path(backup_id, snapshot_id)?;
        fs::remove_file(&path).map_err(|source| CoreError::Io {
            operation: "delete snapshot metadata",
            path: path.clone(),
            source,
        })?;
        if let Some(parent) = path.parent() {
            sync_directory(parent)?;
        }
        self.snapshot_generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub(crate) fn snapshot_generation(&self) -> u64 {
        self.snapshot_generation.load(Ordering::Acquire)
    }

    pub(crate) fn begin_retention_index(
        &self,
        expected_snapshot_generation: u64,
    ) -> Result<RetentionIndexBuilder, CoreError> {
        let directory = tempfile::Builder::new()
            .prefix("gc-")
            .tempdir_in(self.root.join("gc-work"))
            .map_err(|source| CoreError::Io {
                operation: "create garbage collection reachability index",
                path: self.root.join("gc-work"),
                source,
            })?;
        Ok(RetentionIndexBuilder {
            directory,
            writers: std::collections::BTreeMap::new(),
            expected_snapshot_generation,
        })
    }

    /// Deletes only chunks unreferenced by a caller-authenticated, generation-bound root index.
    ///
    /// Snapshot authentication intentionally happens outside the storage transaction lock. Each
    /// bounded deletion batch rechecks the generation and active-job barrier, so backup commits and
    /// new resumable jobs cannot race a long collection pass.
    pub(crate) fn garbage_collect_authenticated(
        &self,
        index: &RetentionIndex,
    ) -> Result<GarbageCollectionReport, CoreError> {
        let mut report = GarbageCollectionReport {
            retained: index.unique_locators,
            ..GarbageCollectionReport::default()
        };
        let mut retained_prefix = None;
        let mut retained = Vec::new();
        for shard in self.list_chunk_shards()? {
            let prefix = shard
                .file_name()
                .and_then(|value| value.to_str())
                .and_then(|value| value.as_bytes().first().copied())
                .ok_or_else(|| CoreError::InvalidState("invalid chunk shard".to_owned()))?;
            if retained_prefix != Some(prefix) {
                retained = read_retention_index_prefix(index.directory.path(), prefix)?;
                retained_prefix = Some(prefix);
            }
            let chunks = self.list_chunk_files_in_shard(&shard)?;
            for batch in chunks.chunks(GARBAGE_COLLECTION_BATCH_SIZE) {
                let mut staged = Vec::new();
                {
                    let _guard = self
                        .transaction_lock
                        .lock()
                        .map_err(|_| CoreError::Synchronization)?;
                    if self.snapshot_generation() != index.expected_snapshot_generation {
                        return Err(CoreError::InvalidState(
                            "snapshot set changed during garbage collection".to_owned(),
                        ));
                    }
                    if !read_directory_sorted(&self.root.join("jobs"))?.is_empty() {
                        report.deferred_active_jobs = true;
                        return Ok(report);
                    }
                    for (locator, path, size) in batch {
                        if retained
                            .binary_search_by(|candidate| candidate.as_str().cmp(locator))
                            .is_ok()
                        {
                            continue;
                        }
                        let trash = self
                            .root
                            .join("trash")
                            .join(format!("{locator}-{}", uuid::Uuid::new_v4()));
                        match fs::rename(path, &trash) {
                            Ok(()) => staged.push((trash, *size)),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(source) => {
                                return Err(CoreError::Io {
                                    operation: "stage unreferenced chunk deletion",
                                    path: path.clone(),
                                    source,
                                });
                            }
                        }
                    }
                    if !staged.is_empty() {
                        sync_directory(&shard)?;
                        sync_directory(self.root.join("trash").as_path())?;
                    }
                }
                let staged_any = !staged.is_empty();
                for (trash, size) in staged {
                    fs::remove_file(&trash).map_err(|source| CoreError::Io {
                        operation: "delete unreferenced chunk",
                        path: trash,
                        source,
                    })?;
                    report.removed += 1;
                    report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(size);
                }
                if staged_any {
                    sync_directory(self.root.join("trash").as_path())?;
                }
            }
        }
        Ok(report)
    }

    /// Authenticates and digest-checks every chunk referenced by a decrypted manifest.
    pub fn verify_manifest(
        &self,
        manifest: &Manifest,
        key: &BackupKey,
    ) -> Result<IntegrityReport, CoreError> {
        manifest.validate()?;
        let mut report = IntegrityReport::default();
        let mut checked = BTreeSet::new();
        for reference in manifest.entries.iter().flat_map(|entry| &entry.chunks) {
            if !checked.insert(reference.opaque_locator.clone()) {
                continue;
            }
            let record = match self.get_provider_record(&reference.opaque_locator) {
                Ok(record) => record,
                Err(CoreError::MissingChunk(_)) => {
                    report.missing.push(reference.opaque_locator.clone());
                    continue;
                }
                Err(_) => {
                    report.corrupt.push(reference.opaque_locator.clone());
                    continue;
                }
            };
            let encrypted = match EncryptedChunk::decode_provider_record(
                reference.opaque_locator.clone(),
                reference.plaintext_digest.clone(),
                &record,
                self.maximum_chunk_size,
            ) {
                Ok(encrypted) => encrypted,
                Err(_) => {
                    report.corrupt.push(reference.opaque_locator.clone());
                    continue;
                }
            };
            if encrypted.ciphertext_length() != reference.ciphertext_length
                || encrypted.plaintext_length != reference.plaintext_length
                || key
                    .decrypt_chunk(manifest.backup_id, &reference.plaintext_digest, &encrypted)
                    .is_err()
            {
                report.corrupt.push(reference.opaque_locator.clone());
            } else {
                report.verified += 1;
            }
        }
        report.missing.sort();
        report.corrupt.sort();
        Ok(report)
    }

    /// Replaces a known-corrupt record only after authenticating an intact candidate.
    pub fn repair_record(
        &self,
        manifest: &Manifest,
        key: &BackupKey,
        locator: &str,
        candidate: &[u8],
    ) -> Result<(), CoreError> {
        let reference = manifest
            .entries
            .iter()
            .flat_map(|entry| &entry.chunks)
            .find(|reference| reference.opaque_locator == locator)
            .ok_or_else(|| CoreError::MissingChunk(locator.to_owned()))?;
        let encrypted = EncryptedChunk::decode_provider_record(
            locator.to_owned(),
            reference.plaintext_digest.clone(),
            candidate,
            self.maximum_chunk_size,
        )?;
        if encrypted.plaintext_length != reference.plaintext_length
            || encrypted.ciphertext_length() != reference.ciphertext_length
        {
            return Err(CoreError::CorruptChunk(locator.to_owned()));
        }
        key.decrypt_chunk(manifest.backup_id, &reference.plaintext_digest, &encrypted)?;
        self.replace_provider_record_verified(
            manifest.backup_id,
            reference,
            key,
            locator,
            candidate,
        )?;
        Ok(())
    }

    /// Atomically persists bounded resumable job state.
    pub fn write_checkpoint(&self, job_id: &str, bytes: &[u8]) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(CoreError::ResourceLimit("job checkpoint"));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        write_atomic(
            &self.root.join("jobs").join(format!("{job_id}.json")),
            bytes,
            true,
        )
    }

    /// Loads bounded resumable job state.
    pub fn read_checkpoint(&self, job_id: &str) -> Result<Option<Vec<u8>>, CoreError> {
        validate_job_id(job_id)?;
        let path = self.root.join("jobs").join(format!("{job_id}.json"));
        match read_bounded(&path, MAX_CHECKPOINT_BYTES) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(CoreError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn append_checkpoint_record(
        &self,
        job_id: &str,
        record: &[u8],
    ) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        self.append_checkpoint_record_with_durability(job_id, record, true)
    }

    pub(crate) fn append_checkpoint_record_buffered(
        &self,
        job_id: &str,
        record: &[u8],
        durable: bool,
    ) -> Result<(), CoreError> {
        self.append_checkpoint_record_with_durability(job_id, record, durable)
    }

    fn append_checkpoint_record_with_durability(
        &self,
        job_id: &str,
        record: &[u8],
        durable: bool,
    ) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        append_record_log(
            &self.checkpoint_log_path(job_id),
            record,
            MAX_CHECKPOINT_BYTES,
            MAX_CHECKPOINT_LOG_BYTES,
            true,
            durable,
        )
    }

    pub(crate) fn sync_checkpoint_records(&self, job_id: &str) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        sync_record_log(&self.checkpoint_log_path(job_id))
    }

    pub(crate) fn read_checkpoint_records(
        &self,
        job_id: &str,
    ) -> Result<Option<Vec<Vec<u8>>>, CoreError> {
        validate_job_id(job_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        read_record_log(
            &self.checkpoint_log_path(job_id),
            MAX_CHECKPOINT_BYTES,
            MAX_CHECKPOINT_LOG_BYTES,
        )
    }

    pub(crate) fn replace_checkpoint_records(
        &self,
        job_id: &str,
        records: &[Vec<u8>],
    ) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        rewrite_record_log(
            &self.checkpoint_log_path(job_id),
            records,
            MAX_CHECKPOINT_BYTES,
            MAX_CHECKPOINT_LOG_BYTES,
            true,
        )
    }

    /// Reports either legacy or append-only resumable state without loading it.
    pub fn has_checkpoint(&self, job_id: &str) -> Result<bool, CoreError> {
        validate_job_id(job_id)?;
        for path in [
            self.root.join("jobs").join(format!("{job_id}.json")),
            self.checkpoint_log_path(job_id),
        ] {
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        return Err(CoreError::InvalidState(
                            "checkpoint path is not a regular file".to_owned(),
                        ));
                    }
                    return Ok(true);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "inspect job checkpoint",
                        path,
                        source,
                    });
                }
            }
        }
        Ok(false)
    }

    /// Clears a completed job checkpoint durably.
    pub fn remove_checkpoint(&self, job_id: &str) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let mut removed = false;
        for path in [
            self.root.join("jobs").join(format!("{job_id}.json")),
            self.checkpoint_log_path(job_id),
        ] {
            match fs::remove_file(&path) {
                Ok(()) => removed = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "remove job checkpoint",
                        path,
                        source,
                    });
                }
            }
        }
        if removed {
            sync_directory(self.root.join("jobs").as_path())
        } else {
            Ok(())
        }
    }

    fn checkpoint_log_path(&self, job_id: &str) -> PathBuf {
        self.root.join("jobs").join(format!("{job_id}.wal"))
    }

    fn chunk_path(&self, locator: &str) -> Result<PathBuf, CoreError> {
        validate_hex_locator(locator)?;
        Ok(self
            .root
            .join("chunks")
            .join(&locator[..2])
            .join(&locator[2..]))
    }

    fn snapshot_path(&self, backup_id: BackupId, snapshot_id: &str) -> Result<PathBuf, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .root
            .join("snapshots")
            .join(backup_id.to_string())
            .join(format!("{snapshot_id}.json")))
    }

    fn quarantine_locked(&self, locator: &str, path: &Path) -> Result<(), CoreError> {
        let quarantine = self
            .root
            .join("quarantine")
            .join(format!("{locator}-{}", uuid::Uuid::new_v4()));
        fs::rename(path, &quarantine).map_err(|source| CoreError::Io {
            operation: "quarantine conflicting chunk",
            path: path.to_path_buf(),
            source,
        })?;
        sync_directory(self.root.join("quarantine").as_path())
    }

    fn replace_provider_record_verified(
        &self,
        backup_id: BackupId,
        reference: &covalent_protocol::ChunkReference,
        key: &BackupKey,
        locator: &str,
        record: &[u8],
    ) -> Result<(), CoreError> {
        validate_hex_locator(locator)?;
        validate_record_bounds(record, self.maximum_chunk_size)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let path = self.chunk_path(locator)?;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidState("chunk path has no parent".to_owned()))?;
        ensure_private_directory(parent)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "chunk path is not a regular file".to_owned(),
                    ));
                }
                let existing =
                    read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?;
                if provider_record_authenticates(
                    &existing,
                    locator,
                    reference,
                    backup_id,
                    key,
                    self.maximum_chunk_size,
                ) {
                    return Ok(());
                }
                self.quarantine_locked(locator, &path)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect repair target",
                    path,
                    source,
                });
            }
        }
        if write_atomic_noclobber(&path, record, false)? {
            return Ok(());
        }
        let incumbent = read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?;
        if provider_record_authenticates(
            &incumbent,
            locator,
            reference,
            backup_id,
            key,
            self.maximum_chunk_size,
        ) {
            Ok(())
        } else {
            Err(CoreError::InvalidState(
                "repair raced a conflicting immutable chunk write".to_owned(),
            ))
        }
    }

    fn list_chunk_shards(&self) -> Result<Vec<PathBuf>, CoreError> {
        let mut shards = Vec::new();
        for shard in read_directory_sorted(&self.root.join("chunks"))? {
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if shard_name.len() != 2
                || !shard_name
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(CoreError::InvalidState("invalid chunk shard".to_owned()));
            }
            let metadata = fs::symlink_metadata(shard.path()).map_err(|source| CoreError::Io {
                operation: "inspect chunk shard",
                path: shard.path(),
                source,
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::InvalidState("invalid chunk shard".to_owned()));
            }
            shards.push(shard.path());
        }
        Ok(shards)
    }

    fn list_chunk_files_in_shard(
        &self,
        shard: &Path,
    ) -> Result<Vec<(String, PathBuf, u64)>, CoreError> {
        let shard_name = shard
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| CoreError::InvalidState("invalid chunk shard".to_owned()))?;
        let mut chunks = Vec::new();
        for entry in read_directory_sorted(shard)? {
            let suffix = entry.file_name().to_string_lossy().into_owned();
            let locator = format!("{shard_name}{suffix}");
            validate_hex_locator(&locator)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect chunk record",
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(CoreError::InvalidState("invalid chunk record".to_owned()));
            }
            chunks.push((locator, path, metadata.len()));
        }
        Ok(chunks)
    }
}

impl RetentionIndexBuilder {
    pub(crate) fn add_locators<'a>(
        &mut self,
        locators: impl IntoIterator<Item = &'a String>,
    ) -> Result<(), CoreError> {
        for locator in locators {
            validate_hex_locator(locator)?;
            let prefix = locator.as_bytes()[0];
            if !self.writers.contains_key(&prefix) {
                let path = self
                    .directory
                    .path()
                    .join(format!("{}.idx", prefix as char));
                let file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .map_err(|source| CoreError::Io {
                        operation: "create retention index shard",
                        path,
                        source,
                    })?;
                self.writers.insert(prefix, BufWriter::new(file));
            }
            let writer = self
                .writers
                .get_mut(&prefix)
                .ok_or(CoreError::Synchronization)?;
            writer
                .write_all(locator.as_bytes())
                .and_then(|()| writer.write_all(b"\n"))
                .map_err(|source| CoreError::Io {
                    operation: "append authenticated retention locator",
                    path: self
                        .directory
                        .path()
                        .join(format!("{}.idx", prefix as char)),
                    source,
                })?;
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<RetentionIndex, CoreError> {
        for (prefix, writer) in &mut self.writers {
            writer.flush().map_err(|source| CoreError::Io {
                operation: "flush retention index shard",
                path: self
                    .directory
                    .path()
                    .join(format!("{}.idx", *prefix as char)),
                source,
            })?;
            writer
                .get_ref()
                .sync_all()
                .map_err(|source| CoreError::Io {
                    operation: "sync retention index shard",
                    path: self
                        .directory
                        .path()
                        .join(format!("{}.idx", *prefix as char)),
                    source,
                })?;
        }
        self.writers.clear();
        sync_directory(self.directory.path())?;
        let mut unique_locators = 0_usize;
        for prefix in b'0'..=b'9' {
            unique_locators = unique_locators.saturating_add(compact_retention_index_prefix(
                self.directory.path(),
                prefix,
            )?);
        }
        for prefix in b'a'..=b'f' {
            unique_locators = unique_locators.saturating_add(compact_retention_index_prefix(
                self.directory.path(),
                prefix,
            )?);
        }
        Ok(RetentionIndex {
            directory: self.directory,
            expected_snapshot_generation: self.expected_snapshot_generation,
            unique_locators,
        })
    }
}

fn compact_retention_index_prefix(directory: &Path, prefix: u8) -> Result<usize, CoreError> {
    let mut locators = read_retention_index_prefix(directory, prefix)?;
    if locators.is_empty() {
        return Ok(0);
    }
    locators.sort_unstable();
    locators.dedup();
    let mut bytes = Vec::with_capacity(locators.len().saturating_mul(65));
    for locator in &locators {
        bytes.extend_from_slice(locator.as_bytes());
        bytes.push(b'\n');
    }
    write_atomic(
        &directory.join(format!("{}.idx", prefix as char)),
        &bytes,
        true,
    )?;
    Ok(locators.len())
}

fn read_retention_index_prefix(directory: &Path, prefix: u8) -> Result<Vec<String>, CoreError> {
    let path = directory.join(format!("{}.idx", prefix as char));
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(CoreError::Io {
                operation: "inspect retention index shard",
                path,
                source,
            });
        }
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_RETENTION_INDEX_PREFIX_BYTES
        || !metadata.len().is_multiple_of(RETENTION_INDEX_LINE_BYTES)
    {
        return Err(CoreError::InvalidState(
            "invalid retention index shard".to_owned(),
        ));
    }
    let file = fs::File::open(&path).map_err(|source| CoreError::Io {
        operation: "open retention index shard",
        path: path.clone(),
        source,
    })?;
    let mut locators = Vec::with_capacity(
        usize::try_from(metadata.len() / RETENTION_INDEX_LINE_BYTES)
            .map_err(|_| CoreError::ResourceLimit("retention index locators"))?,
    );
    for line in BufReader::new(file).split(b'\n') {
        let line = line.map_err(|source| CoreError::Io {
            operation: "read retention index shard",
            path: path.clone(),
            source,
        })?;
        if line.is_empty() {
            continue;
        }
        let locator = String::from_utf8(line)
            .map_err(|_| CoreError::InvalidState("invalid retention index locator".to_owned()))?;
        validate_hex_locator(&locator)?;
        if locator.as_bytes()[0] != prefix {
            return Err(CoreError::InvalidState(
                "mis-sharded retention index locator".to_owned(),
            ));
        }
        locators.push(locator);
    }
    Ok(locators)
}

fn validate_snapshot_id(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidState("invalid snapshot id".to_owned()));
    }
    Ok(())
}

fn read_snapshot_metadata(path: &Path) -> Result<StoredSnapshot, CoreError> {
    let bytes = read_bounded(path, MAX_SNAPSHOT_METADATA_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let snapshot = if schema_version == u64::from(SNAPSHOT_SCHEMA_VERSION) {
        serde_json::from_value(value)?
    } else if schema_version == 0 {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct LegacySnapshot {
            #[serde(default)]
            schema_version: u16,
            backup_id: BackupId,
            snapshot_id: String,
            envelope: ManifestEnvelope,
            chunk_locators: BTreeSet<String>,
            committed_at_unix_ms: u64,
        }
        let legacy: LegacySnapshot = serde_json::from_value(value)?;
        if legacy.schema_version != 0 {
            return Err(CoreError::InvalidState(
                "unsupported snapshot metadata schema".to_owned(),
            ));
        }
        StoredSnapshot::new(
            legacy.backup_id,
            legacy.snapshot_id,
            legacy.envelope,
            legacy.chunk_locators,
            legacy.committed_at_unix_ms,
        )?
    } else {
        return Err(CoreError::InvalidState(
            "unsupported snapshot metadata schema".to_owned(),
        ));
    };
    snapshot.validate()?;
    if schema_version == 0 {
        write_json_atomic(path, &snapshot, false)?;
    }
    Ok(snapshot)
}

fn validate_job_id(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidState("invalid job id".to_owned()));
    }
    Ok(())
}

const fn provider_record_overhead() -> usize {
    4 + 1 + 8 + 4 + 24 + 16
}

fn provider_record_authenticates(
    record: &[u8],
    locator: &str,
    reference: &covalent_protocol::ChunkReference,
    backup_id: BackupId,
    key: &BackupKey,
    maximum_chunk_size: usize,
) -> bool {
    EncryptedChunk::decode_provider_record(
        locator.to_owned(),
        reference.plaintext_digest.clone(),
        record,
        maximum_chunk_size,
    )
    .ok()
    .filter(|encrypted| {
        encrypted.plaintext_length == reference.plaintext_length
            && encrypted.ciphertext_length() == reference.ciphertext_length
    })
    .is_some_and(|encrypted| {
        key.decrypt_chunk(backup_id, &reference.plaintext_digest, &encrypted)
            .is_ok()
    })
}

fn validate_record_bounds(record: &[u8], maximum_chunk_size: usize) -> Result<(), CoreError> {
    if record.len() < provider_record_overhead()
        || record.len() > maximum_chunk_size + provider_record_overhead()
        || record.get(..4) != Some(b"CVCH")
        || record.get(4) != Some(&1)
    {
        return Err(CoreError::CorruptChunk("provider record".to_owned()));
    }
    let plaintext_length = u32::from_be_bytes(
        record[13..17]
            .try_into()
            .map_err(|_| CoreError::CorruptChunk("provider record".to_owned()))?,
    ) as usize;
    if plaintext_length == 0
        || plaintext_length > maximum_chunk_size
        || record.len() != 4 + 1 + 8 + 4 + 24 + plaintext_length + 16
    {
        return Err(CoreError::CorruptChunk("provider record".to_owned()));
    }
    Ok(())
}

fn read_directory_sorted(path: &Path) -> Result<Vec<fs::DirEntry>, CoreError> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(|source| CoreError::Io {
            operation: "read store directory",
            path: path.to_path_buf(),
            source,
        })?
        .collect::<Result<_, _>>()
        .map_err(|source| CoreError::Io {
            operation: "read store directory entry",
            path: path.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn ensure_private_directory(path: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(path).map_err(|source| CoreError::Io {
        operation: "create private storage directory",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
        operation: "inspect private storage directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CoreError::InvalidState(
            "storage directory is not a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            CoreError::Io {
                operation: "protect private storage directory",
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use covalent_protocol::{
        ChunkReference, EntryKind, EntryMetadata, Manifest, ManifestEntry, PROTOCOL_VERSION,
        RelativePath, ReplicaIntent,
    };
    use tempfile::tempdir;

    use crate::{DeviceIdentity, encrypt_manifest};

    use super::*;

    #[test]
    fn dedup_snapshot_commit_and_retention_safe_gc() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let key = BackupKey::generate();
        let backup_id = BackupId::new();
        let first = key.encrypt_chunk(backup_id, 1, b"first").expect("chunk");
        let second = key.encrypt_chunk(backup_id, 1, b"second").expect("chunk");
        assert!(store.put(&first).expect("first put"));
        assert!(!store.put(&first).expect("dedup put"));
        assert!(store.put(&second).expect("second put"));

        let manifest = Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "snapshot-1".to_owned(),
            created_at_unix_ms: 1,
            replica_intent: ReplicaIntent::default(),
            entries: Vec::new(),
            provider_acknowledgements: BTreeMap::new(),
        };
        let envelope =
            encrypt_manifest(&manifest, 1, &key, &DeviceIdentity::generate()).expect("envelope");
        store
            .commit_snapshot(
                &StoredSnapshot::new(
                    backup_id,
                    "snapshot-1",
                    envelope,
                    BTreeSet::from([first.opaque_locator.clone()]),
                    1,
                )
                .expect("snapshot"),
            )
            .expect("commit");
        let generation = store.snapshot_generation();
        let mut index = store.begin_retention_index(generation).expect("index");
        index
            .add_locators([&first.opaque_locator, &first.opaque_locator])
            .expect("retain locator");
        let index = index.finish().expect("finish index");
        let report = store.garbage_collect_authenticated(&index).expect("gc");
        assert_eq!(report.retained, 1);
        assert_eq!(report.removed, 1);
        assert!(store.contains(&first.opaque_locator).expect("contains"));
        assert!(!store.contains(&second.opaque_locator).expect("contains"));
    }

    #[test]
    fn malformed_record_is_rejected_before_write() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        assert!(
            store
                .put_provider_record(&"0".repeat(64), b"not a record")
                .is_err()
        );
    }

    #[test]
    fn conflicting_provider_put_is_rejected_without_replacing_incumbent() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let key = BackupKey::generate();
        let chunk = key
            .encrypt_chunk(BackupId::new(), 1, b"immutable")
            .expect("chunk");
        let original = chunk.encode_provider_record();
        store
            .put_provider_record(&chunk.opaque_locator, &original)
            .expect("initial put");
        let mut conflict = original.clone();
        *conflict.last_mut().expect("ciphertext") ^= 0x40;

        assert!(
            store
                .put_provider_record(&chunk.opaque_locator, &conflict)
                .is_err()
        );
        assert_eq!(
            store
                .get_provider_record(&chunk.opaque_locator)
                .expect("incumbent"),
            original
        );
    }

    #[test]
    fn repair_preserves_an_authenticated_immutable_incumbent() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let key = BackupKey::generate();
        let backup_id = BackupId::new();
        let incumbent = key
            .encrypt_chunk(backup_id, 1, b"same authenticated plaintext")
            .expect("incumbent");
        let candidate = key
            .encrypt_chunk(backup_id, 1, b"same authenticated plaintext")
            .expect("candidate");
        let incumbent_record = incumbent.encode_provider_record();
        let candidate_record = candidate.encode_provider_record();
        assert_eq!(incumbent.opaque_locator, candidate.opaque_locator);
        assert_eq!(incumbent_record, candidate_record);
        store.put(&incumbent).expect("store incumbent");
        let manifest = Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "repair-immutability".to_owned(),
            created_at_unix_ms: 1,
            replica_intent: ReplicaIntent::default(),
            entries: vec![ManifestEntry {
                path: RelativePath::new("file").expect("path"),
                kind: EntryKind::File,
                length: incumbent.plaintext_length.into(),
                chunks: vec![ChunkReference {
                    plaintext_digest: incumbent.plaintext_digest.clone(),
                    opaque_locator: incumbent.opaque_locator.clone(),
                    plaintext_length: incumbent.plaintext_length,
                    ciphertext_length: incumbent.ciphertext_length(),
                }],
                metadata: EntryMetadata::default(),
                sparse_extents: Vec::new(),
            }],
            provider_acknowledgements: BTreeMap::new(),
        };

        store
            .repair_record(
                &manifest,
                &key,
                &incumbent.opaque_locator,
                &candidate_record,
            )
            .expect("authenticated incumbent is already healthy");
        assert_eq!(
            store
                .get_provider_record(&incumbent.opaque_locator)
                .expect("incumbent remains"),
            incumbent_record
        );
    }

    #[test]
    fn changed_snapshot_generation_blocks_gc_before_any_deletion() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let key = BackupKey::generate();
        let backup_id = BackupId::new();
        let retained = key.encrypt_chunk(backup_id, 1, b"retained").expect("chunk");
        let orphan = key.encrypt_chunk(backup_id, 1, b"orphan").expect("chunk");
        store.put(&retained).expect("retained put");
        store.put(&orphan).expect("orphan put");
        let manifest = Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "snapshot-corrupt".to_owned(),
            created_at_unix_ms: 1,
            replica_intent: ReplicaIntent::default(),
            entries: Vec::new(),
            provider_acknowledgements: BTreeMap::new(),
        };
        let snapshot = StoredSnapshot::new(
            backup_id,
            "snapshot-corrupt",
            encrypt_manifest(&manifest, 1, &key, &DeviceIdentity::generate()).expect("envelope"),
            BTreeSet::from([retained.opaque_locator]),
            1,
        )
        .expect("snapshot");
        store.commit_snapshot(&snapshot).expect("commit");
        let stale_generation = store.snapshot_generation().saturating_sub(1);
        let index = store
            .begin_retention_index(stale_generation)
            .expect("index")
            .finish()
            .expect("finish index");
        assert!(store.garbage_collect_authenticated(&index).is_err());
        assert!(
            store
                .contains(&orphan.opaque_locator)
                .expect("orphan retained")
        );
    }

    #[test]
    fn schema_zero_snapshot_metadata_migrates_in_place() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let backup_id = BackupId::new();
        let manifest = Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "legacy-snapshot".to_owned(),
            created_at_unix_ms: 7,
            replica_intent: ReplicaIntent::default(),
            entries: Vec::new(),
            provider_acknowledgements: BTreeMap::new(),
        };
        let snapshot = StoredSnapshot::new(
            backup_id,
            "legacy-snapshot",
            encrypt_manifest(
                &manifest,
                1,
                &BackupKey::generate(),
                &DeviceIdentity::generate(),
            )
            .expect("envelope"),
            BTreeSet::new(),
            7,
        )
        .expect("snapshot");
        let path = store
            .snapshot_path(backup_id, "legacy-snapshot")
            .expect("path");
        ensure_private_directory(path.parent().expect("parent")).expect("snapshot directory");
        let mut legacy = serde_json::to_value(&snapshot).expect("serialize");
        legacy
            .as_object_mut()
            .expect("object")
            .remove("schemaVersion");
        write_json_atomic(&path, &legacy, false).expect("legacy metadata");

        let migrated = store
            .load_snapshot(backup_id, "legacy-snapshot")
            .expect("migrate");
        assert_eq!(migrated, snapshot);
        let persisted: serde_json::Value = serde_json::from_slice(
            &read_bounded(&path, MAX_SNAPSHOT_METADATA_BYTES).expect("persisted"),
        )
        .expect("json");
        assert_eq!(persisted["schemaVersion"], SNAPSHOT_SCHEMA_VERSION);
    }
}
