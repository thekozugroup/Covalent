use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use covalent_protocol::{BackupId, Manifest, ManifestEnvelope};
use serde::{Deserialize, Serialize};

use crate::atomic::{
    read_bounded, read_json_bounded, sync_directory, write_atomic, write_json_atomic,
};
use crate::crypto::validate_hex_locator;
use crate::{BackupKey, CoreError, EncryptedChunk};

const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const MAX_SNAPSHOT_METADATA_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_CHECKPOINT_BYTES: usize = 256 * 1_024 * 1_024;

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
}

impl ChunkStore {
    /// Opens or creates a store rooted at an explicitly configured data directory.
    pub fn open(root: impl AsRef<Path>, maximum_chunk_size: usize) -> Result<Self, CoreError> {
        if !(4 * 1_024..=8 * 1_024 * 1_024).contains(&maximum_chunk_size) {
            return Err(CoreError::ResourceLimit("maximum stored chunk size"));
        }
        let root = root.as_ref().to_path_buf();
        ensure_private_directory(&root)?;
        for directory in ["chunks", "snapshots", "jobs", "quarantine", "trash"] {
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
                self.quarantine_locked(locator, &path)?;
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
        write_atomic(&path, record, false)?;
        Ok(true)
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
                let existing: StoredSnapshot =
                    read_json_bounded(&path, MAX_SNAPSHOT_METADATA_BYTES)?;
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
        write_json_atomic(&path, snapshot, false)
    }

    /// Loads and validates one committed snapshot.
    pub fn load_snapshot(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<StoredSnapshot, CoreError> {
        let path = self.snapshot_path(backup_id, snapshot_id)?;
        let snapshot: StoredSnapshot = read_json_bounded(&path, MAX_SNAPSHOT_METADATA_BYTES)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Lists committed snapshots after validating every metadata record.
    pub fn list_snapshots(&self) -> Result<Vec<StoredSnapshot>, CoreError> {
        let mut snapshots = Vec::new();
        let root = self.root.join("snapshots");
        for backup_entry in read_directory_sorted(&root)? {
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
                let snapshot: StoredSnapshot =
                    read_json_bounded(&path, MAX_SNAPSHOT_METADATA_BYTES)?;
                snapshot.validate()?;
                snapshots.push(snapshot);
            }
        }
        snapshots.sort_by(|left, right| {
            (left.backup_id, &left.snapshot_id).cmp(&(right.backup_id, &right.snapshot_id))
        });
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
        Ok(())
    }

    /// Deletes only chunks unreferenced by every fully validated committed snapshot.
    pub fn garbage_collect(&self) -> Result<GarbageCollectionReport, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        // Validation occurs before any mutation. Corrupt metadata therefore blocks GC.
        let retained: BTreeSet<_> = self
            .list_snapshots()?
            .into_iter()
            .flat_map(|snapshot| snapshot.chunk_locators)
            .collect();
        let mut report = GarbageCollectionReport {
            retained: retained.len(),
            ..GarbageCollectionReport::default()
        };
        if !read_directory_sorted(&self.root.join("jobs"))?.is_empty() {
            report.deferred_active_jobs = true;
            return Ok(report);
        }
        for (locator, path, size) in self.list_chunk_files()? {
            if retained.contains(&locator) {
                continue;
            }
            let trash = self
                .root
                .join("trash")
                .join(format!("{locator}-{}", uuid::Uuid::new_v4()));
            fs::rename(&path, &trash).map_err(|source| CoreError::Io {
                operation: "stage unreferenced chunk deletion",
                path: path.clone(),
                source,
            })?;
            sync_directory(self.root.join("trash").as_path())?;
            fs::remove_file(&trash).map_err(|source| CoreError::Io {
                operation: "delete unreferenced chunk",
                path: trash,
                source,
            })?;
            report.removed += 1;
            report.reclaimed_bytes = report.reclaimed_bytes.saturating_add(size);
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
        key.decrypt_chunk(manifest.backup_id, &reference.plaintext_digest, &encrypted)?;
        self.put_provider_record(locator, candidate)?;
        Ok(())
    }

    /// Atomically persists bounded resumable job state.
    pub fn write_checkpoint(&self, job_id: &str, bytes: &[u8]) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        if bytes.len() > MAX_CHECKPOINT_BYTES {
            return Err(CoreError::ResourceLimit("job checkpoint"));
        }
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

    /// Clears a completed job checkpoint durably.
    pub fn remove_checkpoint(&self, job_id: &str) -> Result<(), CoreError> {
        validate_job_id(job_id)?;
        let path = self.root.join("jobs").join(format!("{job_id}.json"));
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(self.root.join("jobs").as_path()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CoreError::Io {
                operation: "remove job checkpoint",
                path,
                source,
            }),
        }
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

    fn list_chunk_files(&self) -> Result<Vec<(String, PathBuf, u64)>, CoreError> {
        let mut chunks = Vec::new();
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
            for entry in read_directory_sorted(&shard.path())? {
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
        }
        Ok(chunks)
    }
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

    use covalent_protocol::{Manifest, PROTOCOL_VERSION, ReplicaIntent};
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
        let report = store.garbage_collect().expect("gc");
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
    fn corrupt_snapshot_metadata_blocks_gc_before_any_deletion() {
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
        let metadata_path = store
            .root()
            .join("snapshots")
            .join(backup_id.to_string())
            .join("snapshot-corrupt.json");
        fs::write(metadata_path, b"{corrupt").expect("corrupt metadata");

        assert!(store.garbage_collect().is_err());
        assert!(
            store
                .contains(&orphan.opaque_locator)
                .expect("orphan retained")
        );
    }
}
