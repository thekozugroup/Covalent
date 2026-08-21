#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use covalent_protocol::{BackupId, DeviceId, Manifest, ManifestEnvelope, StorageLease};
use serde::{Deserialize, Serialize};

use crate::atomic::{
    append_record_log, read_bounded, read_record_log, rewrite_record_log, sync_directory,
    sync_record_log, write_atomic, write_atomic_noclobber, write_json_atomic,
};
use crate::crypto::validate_hex_locator;
use crate::recovery::MAX_RECOVERY_CAPSULE_BYTES;
use crate::{BackupKey, CoreError, EncryptedChunk, RecoveryCapsule};

const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const MAX_SNAPSHOT_METADATA_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_CHECKPOINT_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_CHECKPOINT_LOG_BYTES: u64 = 512 * 1_024 * 1_024;
const GARBAGE_COLLECTION_BATCH_SIZE: usize = 1_024;
const RETENTION_INDEX_LINE_BYTES: u64 = 65;
const MAX_RETENTION_INDEX_PREFIX_BYTES: u64 = 64 * 1_024 * 1_024;
const PROVIDER_LEASE_SCHEMA_VERSION: u16 = 1;
const MAX_PROVIDER_LEASE_STATE_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_RECOVERY_CAPSULE_SEGMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_RECOVERY_CAPSULE_SEGMENTS: u32 = 128;
#[cfg(test)]
thread_local! {
    /// Arms exactly one provider-upload abort, scoped to the thread that armed it.
    /// A process-global failpoint would let tests running in parallel consume each
    /// other's arming, so the boundary is deliberately thread-local.
    static PROVIDER_UPLOAD_FAILPOINT: Cell<u8> = const { Cell::new(0) };
}

#[cfg(test)]
fn provider_upload_failpoint(boundary: u8) -> Result<(), CoreError> {
    PROVIDER_UPLOAD_FAILPOINT.with(|armed| {
        if armed.get() == boundary {
            armed.set(0);
            return Err(CoreError::InvalidState(format!(
                "provider upload failpoint {boundary}"
            )));
        }
        Ok(())
    })
}

#[cfg(not(test))]
const fn provider_upload_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

/// Provider-side durable admission limits for untrusted remote storage writers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderQuotaPolicy {
    pub maximum_total_bytes: u64,
    pub maximum_peer_bytes: u64,
    pub maximum_backup_bytes: u64,
    pub maximum_total_objects: u64,
    pub maximum_peer_objects: u64,
    pub maximum_backup_objects: u64,
    pub free_space_reserve_bytes: u64,
    pub maximum_lease_lifetime_ms: u64,
}

impl Default for ProviderQuotaPolicy {
    fn default() -> Self {
        Self {
            maximum_total_bytes: 8 * 1_024 * 1_024 * 1_024 * 1_024,
            maximum_peer_bytes: 4 * 1_024 * 1_024 * 1_024 * 1_024,
            maximum_backup_bytes: 2 * 1_024 * 1_024 * 1_024 * 1_024,
            maximum_total_objects: 100_000_000,
            maximum_peer_objects: 50_000_000,
            maximum_backup_objects: 10_000_000,
            free_space_reserve_bytes: 1_024 * 1_024 * 1_024,
            maximum_lease_lifetime_ms: 15 * 60 * 1_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderLeaseState {
    schema_version: u16,
    lease: StorageLease,
    consumed_new_bytes: u64,
    consumed_new_objects: u64,
    objects: std::collections::BTreeMap<String, u64>,
    cancelled: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderObjectOwner {
    peer_device_id: DeviceId,
    backup_id: BackupId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderObjectReference {
    schema_version: u16,
    locator: String,
    record_bytes: u64,
    owners: BTreeSet<ProviderObjectOwner>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProviderUploadKind {
    Chunk { locator: String },
    RecoveryCapsule { snapshot_id: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderUploadJournal {
    schema_version: u16,
    journal_id: String,
    lease: StorageLease,
    object_key: String,
    object: ProviderUploadKind,
    record_bytes: u64,
    record_digest: String,
    #[serde(default)]
    recovery_capsule_descriptor: Option<RecoveryCapsuleDescriptor>,
    expected_new_object: bool,
    started_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsuleUpload {
    schema_version: u16,
    upload_id: String,
    lease: StorageLease,
    total_bytes: u64,
    total_segments: u32,
    capsule_digest: String,
    #[serde(default)]
    descriptor: Option<RecoveryCapsuleDescriptor>,
    created_at_unix_ms: u64,
}

/// Signed capacity facts returned by the provider handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCapacity {
    pub available_bytes: u64,
    pub reserved_bytes: u64,
    pub available_objects: u64,
    pub reserved_objects: u64,
    pub free_space_reserve_bytes: u64,
}

/// Bounded authenticated metadata used to page and stream one tenant's recovery capsules.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryCapsuleDescriptor {
    pub backup_id: BackupId,
    pub snapshot_id: String,
    pub key_epoch: u64,
    pub committed_at_unix_ms: u64,
    pub signer_device_id: DeviceId,
    pub total_bytes: u64,
    pub capsule_digest: String,
}

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
    provider_quota_policy: Arc<ProviderQuotaPolicy>,
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
        Self::open_with_provider_quotas(root, maximum_chunk_size, ProviderQuotaPolicy::default())
    }

    /// Opens a store with explicit provider quotas and free-space reserve.
    pub fn open_with_provider_quotas(
        root: impl AsRef<Path>,
        maximum_chunk_size: usize,
        provider_quota_policy: ProviderQuotaPolicy,
    ) -> Result<Self, CoreError> {
        if !(4 * 1_024..=8 * 1_024 * 1_024).contains(&maximum_chunk_size) {
            return Err(CoreError::ResourceLimit("maximum stored chunk size"));
        }
        if provider_quota_policy.maximum_total_bytes == 0
            || provider_quota_policy.maximum_peer_bytes > provider_quota_policy.maximum_total_bytes
            || provider_quota_policy.maximum_backup_bytes > provider_quota_policy.maximum_peer_bytes
            || provider_quota_policy.maximum_total_objects == 0
            || provider_quota_policy.maximum_peer_objects
                > provider_quota_policy.maximum_total_objects
            || provider_quota_policy.maximum_backup_objects
                > provider_quota_policy.maximum_peer_objects
            || provider_quota_policy.maximum_lease_lifetime_ms == 0
        {
            return Err(CoreError::InvalidState(
                "invalid provider quota policy".to_owned(),
            ));
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
            "recovery-capsules",
            "recovery-capsule-index",
            "provider-leases",
            "provider-object-refs",
            "provider-upload-journal",
            "provider-capsule-uploads",
        ] {
            let path = root.join(directory);
            ensure_private_directory(&path)?;
        }
        let root = fs::canonicalize(&root).map_err(|source| CoreError::Io {
            operation: "canonicalize chunk store root",
            path: root,
            source,
        })?;
        let store = Self {
            root,
            maximum_chunk_size,
            transaction_lock: Arc::new(Mutex::new(())),
            snapshot_generation: Arc::new(AtomicU64::new(0)),
            provider_quota_policy: Arc::new(provider_quota_policy),
        };
        store.recover_provider_upload_journals()?;
        Ok(store)
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

    /// Returns current physical capacity and durable active reservations.
    pub fn provider_capacity(&self, now_unix_ms: u64) -> Result<ProviderCapacity, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.provider_capacity_locked(now_unix_ms, None, None)
    }

    /// Persists one signed provider-issued reservation before any remote bytes are accepted.
    pub fn reserve_provider_lease(&self, lease: &StorageLease) -> Result<(), CoreError> {
        if lease.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || lease.lease_id.is_empty()
            || lease.lease_id.len() > 128
            || !lease
                .lease_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || lease.max_new_bytes == 0
            || lease.max_new_objects == 0
            || lease.expires_at_unix_ms <= lease.issued_at_unix_ms
            || lease.expires_at_unix_ms - lease.issued_at_unix_ms
                > self.provider_quota_policy.maximum_lease_lifetime_ms
            || lease.nonce.len() > 128
            || lease.signature.is_empty()
        {
            return Err(CoreError::InvalidState("invalid storage lease".to_owned()));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let capacity = self.provider_capacity_locked(
            lease.issued_at_unix_ms,
            Some(lease.peer_device_id),
            Some(lease.backup_id),
        )?;
        if lease.max_new_bytes > capacity.available_bytes
            || lease.max_new_objects > capacity.available_objects
        {
            return Err(CoreError::ResourceLimit("provider storage quota"));
        }
        let path = self.provider_lease_path(lease)?;
        if path.exists() {
            let existing: ProviderLeaseState =
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
            return if existing.lease == *lease {
                Ok(())
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        }
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("provider lease path has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        write_json_atomic(
            &path,
            &ProviderLeaseState {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease: lease.clone(),
                consumed_new_bytes: 0,
                consumed_new_objects: 0,
                objects: std::collections::BTreeMap::new(),
                cancelled: false,
            },
            true,
        )
    }

    /// Atomically consumes a matching lease for one immutable remote chunk write.
    pub fn put_provider_record_leased(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        locator: &str,
        record: &[u8],
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        validate_hex_locator(locator)?;
        validate_record_bounds(record, self.maximum_chunk_size)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let object_key = format!("chunk:{locator}");
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        if let Some(length) = state.objects.get(&object_key) {
            if *length != record.len() as u64 {
                return Err(CoreError::AuthenticationFailed);
            }
            let existing = read_bounded(
                &self.chunk_path(locator)?,
                self.maximum_chunk_size + provider_record_overhead(),
            )?;
            return if existing == record {
                Ok(false)
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        }
        let path = self.chunk_path(locator)?;
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidState("chunk path has no parent".to_owned()))?;
        ensure_private_directory(parent)?;
        let expected_new_object = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "chunk path is not a regular file".to_owned(),
                    ));
                }
                if read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?
                    != record
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.ensure_lease_consumption_locked(&state, record.len() as u64, 1)?;
                if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
                    operation: "inspect provider free space",
                    path: self.root.clone(),
                    source,
                })? < (record.len() as u64)
                    .saturating_add(self.provider_quota_policy.free_space_reserve_bytes)
                {
                    return Err(CoreError::ResourceLimit("provider free-space reserve"));
                }
                true
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect leased provider chunk",
                    path,
                    source,
                });
            }
        };
        let journal = self.begin_provider_upload_journal_locked(
            lease,
            object_key.clone(),
            ProviderUploadKind::Chunk {
                locator: locator.to_owned(),
            },
            record.len() as u64,
            blake3::hash(record).to_hex().as_str(),
            None,
            expected_new_object,
            now_unix_ms,
        )?;
        provider_upload_failpoint(1)?;
        let created = expected_new_object && write_atomic_noclobber(&path, record, false)?;
        provider_upload_failpoint(2)?;
        self.add_provider_object_reference_locked(
            locator,
            record.len() as u64,
            peer_device_id,
            backup_id,
        )?;
        provider_upload_failpoint(3)?;
        if created {
            state.consumed_new_bytes = state
                .consumed_new_bytes
                .checked_add(record.len() as u64)
                .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
            state.consumed_new_objects = state
                .consumed_new_objects
                .checked_add(1)
                .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
        }
        state.objects.insert(object_key, record.len() as u64);
        self.persist_provider_lease_locked(&state)?;
        self.complete_provider_upload_journal_locked(&journal)?;
        Ok(created)
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

    /// Returns one existing opaque provider record length without loading its bytes.
    pub fn provider_record_length(&self, locator: &str) -> Result<u64, CoreError> {
        let path = self.chunk_path(locator)?;
        let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
            operation: "inspect provider record length",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > (self.maximum_chunk_size + provider_record_overhead()) as u64
        {
            return Err(CoreError::InvalidState(
                "invalid provider record length".to_owned(),
            ));
        }
        Ok(metadata.len())
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

    /// Authorizes a bounded all-or-nothing provider read for one exact tenant backup.
    pub fn authorize_provider_record_batch(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        locators: &[String],
    ) -> Result<(), CoreError> {
        if locators.is_empty() || locators.len() > 1_024 {
            return Err(CoreError::ResourceLimit("provider read batch"));
        }
        let mut unique = BTreeSet::new();
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        for locator in locators {
            validate_hex_locator(locator)?;
            if !unique.insert(locator) {
                return Err(CoreError::AuthenticationFailed);
            }
            let path = self.provider_object_reference_path(locator)?;
            let reference: ProviderObjectReference = serde_json::from_slice(&read_bounded(
                &path,
                MAX_PROVIDER_LEASE_STATE_BYTES,
            )?)
            .map_err(|_| CoreError::AuthenticationFailed)?;
            if reference.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
                || reference.locator != *locator
                || !reference.owners.contains(&ProviderObjectOwner {
                    peer_device_id,
                    backup_id,
                })
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        Ok(())
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
        self.commit_snapshot_internal(snapshot, true)
    }

    /// Commits authenticated recovery metadata before remote objects are fetched.
    pub(crate) fn commit_recovery_snapshot(
        &self,
        snapshot: &StoredSnapshot,
    ) -> Result<(), CoreError> {
        self.commit_snapshot_internal(snapshot, false)
    }

    fn commit_snapshot_internal(
        &self,
        snapshot: &StoredSnapshot,
        require_local_chunks: bool,
    ) -> Result<(), CoreError> {
        snapshot.validate()?;
        // Hold the same lock as GC from the first reachability check through the
        // metadata commit so a collector cannot delete a newly referenced chunk
        // between those two operations.
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        if require_local_chunks {
            for locator in &snapshot.chunk_locators {
                if !self.contains(locator)? {
                    return Err(CoreError::MissingChunk(locator.clone()));
                }
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

    /// Stores one immutable signed encrypted recovery catalog capsule.
    pub fn put_recovery_capsule(&self, capsule: &RecoveryCapsule) -> Result<bool, CoreError> {
        validate_snapshot_id(&capsule.snapshot_id)?;
        let bytes = serde_json::to_vec(capsule)?;
        if bytes.len() > MAX_RECOVERY_CAPSULE_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule"));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let path = self.recovery_capsule_path(capsule.backup_id, &capsule.snapshot_id)?;
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("recovery capsule path has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        if write_atomic_noclobber(&path, &bytes, false)? {
            return Ok(true);
        }
        let existing = read_bounded(&path, MAX_RECOVERY_CAPSULE_BYTES)?;
        if existing == bytes {
            Ok(false)
        } else {
            Err(CoreError::InvalidState(
                "recovery capsule identifier is already immutable".to_owned(),
            ))
        }
    }

    /// Atomically consumes a matching lease for one immutable recovery capsule.
    pub fn put_recovery_capsule_leased(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        capsule: &RecoveryCapsule,
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        if capsule.backup_id != backup_id {
            return Err(CoreError::AuthenticationFailed);
        }
        validate_snapshot_id(&capsule.snapshot_id)?;
        let bytes = serde_json::to_vec(capsule)?;
        if bytes.len() > MAX_RECOVERY_CAPSULE_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule"));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let object_key = format!("capsule:{backup_id}:{}", capsule.snapshot_id);
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let path = self.recovery_capsule_path(backup_id, &capsule.snapshot_id)?;
        if let Some(length) = state.objects.get(&object_key) {
            if *length != bytes.len() as u64 {
                return Err(CoreError::AuthenticationFailed);
            }
            return if read_bounded(&path, MAX_RECOVERY_CAPSULE_BYTES)? == bytes {
                Ok(false)
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        }
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("recovery capsule path has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        let expected_new_object = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || read_bounded(&path, MAX_RECOVERY_CAPSULE_BYTES)? != bytes
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.ensure_lease_consumption_locked(&state, bytes.len() as u64, 1)?;
                if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
                    operation: "inspect provider free space",
                    path: self.root.clone(),
                    source,
                })? < (bytes.len() as u64)
                    .saturating_add(self.provider_quota_policy.free_space_reserve_bytes)
                {
                    return Err(CoreError::ResourceLimit("provider free-space reserve"));
                }
                true
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect leased recovery capsule",
                    path,
                    source,
                });
            }
        };
        let journal = self.begin_provider_upload_journal_locked(
            lease,
            object_key.clone(),
            ProviderUploadKind::RecoveryCapsule {
                snapshot_id: capsule.snapshot_id.clone(),
            },
            bytes.len() as u64,
            blake3::hash(&bytes).to_hex().as_str(),
            Some(RecoveryCapsuleDescriptor {
                backup_id: capsule.backup_id,
                snapshot_id: capsule.snapshot_id.clone(),
                key_epoch: capsule.key_epoch,
                committed_at_unix_ms: capsule.committed_at_unix_ms,
                signer_device_id: capsule.signer_device_id,
                total_bytes: bytes.len() as u64,
                capsule_digest: blake3::hash(&bytes).to_hex().to_string(),
            }),
            expected_new_object,
            now_unix_ms,
        )?;
        provider_upload_failpoint(1)?;
        let created = expected_new_object && write_atomic_noclobber(&path, &bytes, false)?;
        provider_upload_failpoint(2)?;
        if created {
            state.consumed_new_bytes = state
                .consumed_new_bytes
                .checked_add(bytes.len() as u64)
                .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
            state.consumed_new_objects = state
                .consumed_new_objects
                .checked_add(1)
                .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
        }
        self.persist_recovery_capsule_descriptor_locked(capsule, &bytes)?;
        provider_upload_failpoint(3)?;
        state.objects.insert(object_key, bytes.len() as u64);
        self.persist_provider_lease_locked(&state)?;
        self.complete_provider_upload_journal_locked(&journal)?;
        Ok(created)
    }

    /// Starts one durable, resumable segmented capsule upload bound to an exact lease.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_recovery_capsule_upload(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        upload_id: &str,
        total_bytes: u64,
        total_segments: u32,
        capsule_digest: &str,
        descriptor: &RecoveryCapsuleDescriptor,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        if upload_id.is_empty()
            || upload_id.len() > 128
            || !upload_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || total_bytes == 0
            || total_bytes > MAX_RECOVERY_CAPSULE_BYTES as u64
            || !(1..=MAX_RECOVERY_CAPSULE_SEGMENTS).contains(&total_segments)
            || capsule_digest.len() != 64
            || capsule_digest
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
            || total_bytes > lease.max_new_bytes
            || lease.max_new_objects < 1
            || total_segments
                != u32::try_from(total_bytes.div_ceil(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64))
                    .map_err(|_| CoreError::ResourceLimit("recovery capsule segments"))?
            || descriptor.backup_id != backup_id
            || descriptor.signer_device_id != peer_device_id
            || descriptor.total_bytes != total_bytes
            || descriptor.capsule_digest != capsule_digest
        {
            return Err(CoreError::InvalidState(
                "invalid recovery capsule upload".to_owned(),
            ));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let directory = self.recovery_capsule_upload_path(lease);
        match fs::symlink_metadata(&directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CoreError::InvalidState(
                        "invalid recovery capsule upload path".to_owned(),
                    ));
                }
                let incumbent: RecoveryCapsuleUpload = serde_json::from_slice(&read_bounded(
                    &directory.join("metadata.json"),
                    MAX_PROVIDER_LEASE_STATE_BYTES,
                )?)?;
                let expected = RecoveryCapsuleUpload {
                    schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                    upload_id: upload_id.to_owned(),
                    lease: lease.clone(),
                    total_bytes,
                    total_segments,
                    capsule_digest: capsule_digest.to_owned(),
                    descriptor: Some(descriptor.clone()),
                    created_at_unix_ms: now_unix_ms,
                };
                if incumbent.upload_id == expected.upload_id
                    && incumbent.lease == expected.lease
                    && incumbent.total_bytes == expected.total_bytes
                    && incumbent.total_segments == expected.total_segments
                    && incumbent.capsule_digest == expected.capsule_digest
                    && incumbent.descriptor == expected.descriptor
                {
                    return Ok(());
                }
                return Err(CoreError::AuthenticationFailed);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovery capsule upload",
                    path: directory,
                    source,
                });
            }
        }
        ensure_private_directory(&directory)?;
        ensure_private_directory(&directory.join("segments"))?;
        write_json_atomic(
            &directory.join("metadata.json"),
            &RecoveryCapsuleUpload {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                upload_id: upload_id.to_owned(),
                lease: lease.clone(),
                total_bytes,
                total_segments,
                capsule_digest: capsule_digest.to_owned(),
                descriptor: Some(descriptor.clone()),
                created_at_unix_ms: now_unix_ms,
            },
            true,
        )
    }

    /// Stores one immutable bounded capsule segment; duplicate retries must be byte-identical.
    #[allow(clippy::too_many_arguments)]
    pub fn put_recovery_capsule_segment(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        upload_id: &str,
        index: u32,
        segment: &[u8],
        segment_digest: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        if segment.is_empty()
            || segment.len() > MAX_RECOVERY_CAPSULE_SEGMENT_BYTES
            || blake3::hash(segment).to_hex().as_str() != segment_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let metadata = self.load_recovery_capsule_upload_locked(lease, upload_id)?;
        if metadata.lease != *lease || index >= metadata.total_segments {
            return Err(CoreError::AuthenticationFailed);
        }
        let path = self
            .recovery_capsule_upload_path(lease)
            .join("segments")
            .join(format!("{index:08}.bin"));
        if write_atomic_noclobber(&path, segment, false)? {
            return Ok(());
        }
        if read_bounded(&path, MAX_RECOVERY_CAPSULE_SEGMENT_BYTES)? == segment {
            Ok(())
        } else {
            Err(CoreError::AuthenticationFailed)
        }
    }

    /// Authenticates ordered segments and commits one capsule through normal lease accounting.
    pub fn commit_recovery_capsule_upload(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        upload_id: &str,
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        let guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let metadata = self.load_recovery_capsule_upload_locked(lease, upload_id)?;
        let descriptor = metadata
            .descriptor
            .clone()
            .ok_or(CoreError::AuthenticationFailed)?;
        if descriptor.backup_id != backup_id
            || descriptor.signer_device_id != peer_device_id
            || descriptor.total_bytes != metadata.total_bytes
            || descriptor.capsule_digest != metadata.capsule_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let directory = self.recovery_capsule_upload_path(lease);
        let assembled_path = directory.join("assembled.tmp");
        match fs::symlink_metadata(&assembled_path) {
            Ok(existing) if existing.file_type().is_symlink() || !existing.is_file() => {
                return Err(CoreError::AuthenticationFailed);
            }
            Ok(_) => fs::remove_file(&assembled_path).map_err(|source| CoreError::Io {
                operation: "replace recovery capsule assembly",
                path: assembled_path.clone(),
                source,
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovery capsule assembly",
                    path: assembled_path,
                    source,
                });
            }
        }
        let mut assembled = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&assembled_path)
            .map_err(|source| CoreError::Io {
                operation: "create recovery capsule assembly",
                path: assembled_path.clone(),
                source,
            })?;
        let mut hasher = blake3::Hasher::new();
        let mut total_written = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        for index in 0..metadata.total_segments {
            let segment_path = directory.join("segments").join(format!("{index:08}.bin"));
            let segment_metadata =
                fs::symlink_metadata(&segment_path).map_err(|source| CoreError::Io {
                    operation: "inspect recovery capsule segment",
                    path: segment_path.clone(),
                    source,
                })?;
            let expected_length = (metadata.total_bytes - total_written)
                .min(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64);
            if segment_metadata.file_type().is_symlink()
                || !segment_metadata.is_file()
                || segment_metadata.len() != expected_length
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let mut segment = fs::File::open(&segment_path).map_err(|source| CoreError::Io {
                operation: "open recovery capsule segment",
                path: segment_path.clone(),
                source,
            })?;
            let mut segment_written = 0_u64;
            loop {
                let read = segment.read(&mut buffer).map_err(|source| CoreError::Io {
                    operation: "read recovery capsule segment",
                    path: segment_path.clone(),
                    source,
                })?;
                if read == 0 {
                    break;
                }
                segment_written = segment_written
                    .checked_add(read as u64)
                    .ok_or(CoreError::ResourceLimit("recovery capsule size"))?;
                total_written = total_written
                    .checked_add(read as u64)
                    .ok_or(CoreError::ResourceLimit("recovery capsule size"))?;
                if segment_written > expected_length || total_written > metadata.total_bytes {
                    return Err(CoreError::AuthenticationFailed);
                }
                hasher.update(&buffer[..read]);
                assembled
                    .write_all(&buffer[..read])
                    .map_err(|source| CoreError::Io {
                        operation: "assemble recovery capsule",
                        path: assembled_path.clone(),
                        source,
                    })?;
            }
            if segment_written != expected_length {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        assembled.flush().map_err(|source| CoreError::Io {
            operation: "flush recovery capsule assembly",
            path: assembled_path.clone(),
            source,
        })?;
        assembled.sync_all().map_err(|source| CoreError::Io {
            operation: "sync recovery capsule assembly",
            path: assembled_path.clone(),
            source,
        })?;
        drop(assembled);
        if total_written != metadata.total_bytes
            || hasher.finalize().to_hex().as_str() != metadata.capsule_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let object_key = format!("capsule:{backup_id}:{}", descriptor.snapshot_id);
        let path = self.recovery_capsule_path(backup_id, &descriptor.snapshot_id)?;
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("recovery capsule path has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        let expected_new_object = match fs::symlink_metadata(&path) {
            Ok(existing) => {
                if existing.file_type().is_symlink() || !existing.is_file() {
                    return Err(CoreError::AuthenticationFailed);
                }
                let (existing_bytes, existing_digest) = hash_file_bounded(
                    &path,
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "hash existing recovery capsule",
                )?;
                if existing_bytes != metadata.total_bytes
                    || existing_digest != metadata.capsule_digest
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                false
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.ensure_lease_consumption_locked(&state, metadata.total_bytes, 1)?;
                if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
                    operation: "inspect provider free space",
                    path: self.root.clone(),
                    source,
                })? < metadata
                    .total_bytes
                    .saturating_add(self.provider_quota_policy.free_space_reserve_bytes)
                {
                    return Err(CoreError::ResourceLimit("provider free-space reserve"));
                }
                true
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect leased recovery capsule",
                    path,
                    source,
                });
            }
        };
        if let Some(length) = state.objects.get(&object_key)
            && *length != metadata.total_bytes
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let journal = self.begin_provider_upload_journal_locked(
            lease,
            object_key.clone(),
            ProviderUploadKind::RecoveryCapsule {
                snapshot_id: descriptor.snapshot_id.clone(),
            },
            metadata.total_bytes,
            &metadata.capsule_digest,
            Some(descriptor.clone()),
            expected_new_object,
            now_unix_ms,
        )?;
        provider_upload_failpoint(1)?;
        let created = if expected_new_object {
            fs::rename(&assembled_path, &path).map_err(|source| CoreError::Io {
                operation: "commit recovery capsule assembly",
                path: path.clone(),
                source,
            })?;
            sync_directory(parent)?;
            true
        } else {
            fs::remove_file(&assembled_path).map_err(|source| CoreError::Io {
                operation: "discard duplicate recovery capsule assembly",
                path: assembled_path,
                source,
            })?;
            false
        };
        provider_upload_failpoint(2)?;
        if created {
            state.consumed_new_bytes = state
                .consumed_new_bytes
                .checked_add(metadata.total_bytes)
                .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
            state.consumed_new_objects = state
                .consumed_new_objects
                .checked_add(1)
                .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
        }
        self.persist_recovery_capsule_descriptor_value_locked(&descriptor)?;
        provider_upload_failpoint(3)?;
        state.objects.insert(object_key, metadata.total_bytes);
        self.persist_provider_lease_locked(&state)?;
        self.complete_provider_upload_journal_locked(&journal)?;
        fs::remove_dir_all(&directory).map_err(|source| CoreError::Io {
            operation: "complete recovery capsule upload",
            path: directory,
            source,
        })?;
        drop(guard);
        sync_directory(self.root.join("provider-capsule-uploads").as_path())?;
        Ok(created)
    }

    /// Lists every bounded capsule available to an authenticated recovery principal.
    pub fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
        let root = self.root.join("recovery-capsules");
        let mut capsules = Vec::new();
        for backup_entry in read_directory_sorted(&root)? {
            let backup_id = BackupId::from_str(&backup_entry.file_name().to_string_lossy())
                .map_err(|_| {
                    CoreError::InvalidState("invalid recovery backup directory".to_owned())
                })?;
            let metadata =
                fs::symlink_metadata(backup_entry.path()).map_err(|source| CoreError::Io {
                    operation: "inspect recovery capsule directory",
                    path: backup_entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::InvalidState(
                    "unexpected recovery capsule entry".to_owned(),
                ));
            }
            for entry in read_directory_sorted(&backup_entry.path())? {
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                    operation: "inspect recovery capsule",
                    path: path.clone(),
                    source,
                })?;
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("json")
                {
                    return Err(CoreError::InvalidState(
                        "unexpected recovery capsule entry".to_owned(),
                    ));
                }
                let capsule: RecoveryCapsule =
                    serde_json::from_slice(&read_bounded(&path, MAX_RECOVERY_CAPSULE_BYTES)?)?;
                if capsule.backup_id != backup_id
                    || path.file_stem().and_then(|value| value.to_str())
                        != Some(capsule.snapshot_id.as_str())
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                capsules.push(capsule);
                if capsules.len() > 1_000_000 {
                    return Err(CoreError::ResourceLimit("recovery capsule listing"));
                }
            }
        }
        capsules.sort_by(|left, right| {
            (left.backup_id, &left.snapshot_id).cmp(&(right.backup_id, &right.snapshot_id))
        });
        Ok(capsules)
    }

    /// Lists one bounded page belonging only to the authenticated owner peer.
    pub fn list_recovery_capsules_for_owner(
        &self,
        owner_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<(Vec<RecoveryCapsule>, Option<String>), CoreError> {
        if !(1..=128).contains(&limit)
            || cursor.is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
        {
            return Err(CoreError::InvalidState(
                "invalid recovery capsule page".to_owned(),
            ));
        }
        let mut capsules: Vec<_> = self
            .list_recovery_capsules()?
            .into_iter()
            .filter(|capsule| {
                capsule.signer_device_id == owner_device_id
                    && backup_id.is_none_or(|value| value == capsule.backup_id)
            })
            .filter(|capsule| {
                cursor.is_none_or(|value| recovery_capsule_cursor(capsule).as_str() > value)
            })
            .collect();
        capsules.sort_by(|left, right| {
            (left.backup_id, &left.snapshot_id).cmp(&(right.backup_id, &right.snapshot_id))
        });
        let has_more = capsules.len() > limit as usize;
        capsules.truncate(limit as usize);
        let next_cursor = has_more
            .then(|| capsules.last().map(recovery_capsule_cursor))
            .flatten();
        Ok((capsules, next_cursor))
    }

    /// Lists only small durable descriptors for one authenticated tenant.
    pub fn list_recovery_capsule_descriptors_for_owner(
        &self,
        owner_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<(Vec<RecoveryCapsuleDescriptor>, Option<String>), CoreError> {
        if !(1..=128).contains(&limit)
            || cursor.is_some_and(|value| {
                value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
            })
        {
            return Err(CoreError::InvalidState(
                "invalid recovery capsule descriptor page".to_owned(),
            ));
        }
        let root = self
            .root
            .join("recovery-capsule-index")
            .join(owner_device_id.to_string());
        let mut descriptors = Vec::new();
        for backup_entry in read_directory_sorted(&root)? {
            let directory_backup_id =
                BackupId::from_str(&backup_entry.file_name().to_string_lossy())
                    .map_err(|_| CoreError::AuthenticationFailed)?;
            if backup_id.is_some_and(|value| value != directory_backup_id) {
                continue;
            }
            for entry in read_directory_sorted(&backup_entry.path())? {
                let descriptor: RecoveryCapsuleDescriptor = serde_json::from_slice(&read_bounded(
                    &entry.path(),
                    MAX_PROVIDER_LEASE_STATE_BYTES,
                )?)?;
                if descriptor.signer_device_id != owner_device_id
                    || descriptor.backup_id != directory_backup_id
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                let descriptor_cursor =
                    format!("{}/{}", descriptor.backup_id, descriptor.snapshot_id);
                if cursor.is_none_or(|value| descriptor_cursor.as_str() > value) {
                    descriptors.push(descriptor);
                }
            }
        }
        descriptors.sort_by(|left, right| {
            (left.backup_id, &left.snapshot_id).cmp(&(right.backup_id, &right.snapshot_id))
        });
        let has_more = descriptors.len() > limit as usize;
        descriptors.truncate(limit as usize);
        let next_cursor = has_more.then(|| {
            let last = descriptors.last().expect("non-empty truncated page");
            format!("{}/{}", last.backup_id, last.snapshot_id)
        });
        Ok((descriptors, next_cursor))
    }

    /// Reads one bounded byte range after resolving an owner-scoped descriptor.
    pub fn read_recovery_capsule_segment_for_owner(
        &self,
        owner_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        offset: u64,
        maximum_bytes: u32,
    ) -> Result<(Vec<u8>, u64, String), CoreError> {
        if maximum_bytes == 0 || maximum_bytes as usize > MAX_RECOVERY_CAPSULE_SEGMENT_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule segment"));
        }
        let descriptor: RecoveryCapsuleDescriptor = serde_json::from_slice(&read_bounded(
            &self.recovery_capsule_descriptor_path(owner_device_id, backup_id, snapshot_id)?,
            MAX_PROVIDER_LEASE_STATE_BYTES,
        )?)?;
        if descriptor.signer_device_id != owner_device_id
            || descriptor.backup_id != backup_id
            || descriptor.snapshot_id != snapshot_id
            || offset > descriptor.total_bytes
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let path = self.recovery_capsule_path(backup_id, snapshot_id)?;
        let mut file = fs::File::open(&path).map_err(|source| CoreError::Io {
            operation: "open recovery capsule segment",
            path: path.clone(),
            source,
        })?;
        use std::io::{Read as _, Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| CoreError::Io {
                operation: "seek recovery capsule segment",
                path: path.clone(),
                source,
            })?;
        let remaining = descriptor.total_bytes.saturating_sub(offset);
        let length = remaining.min(maximum_bytes as u64) as usize;
        let mut bytes = vec![0_u8; length];
        file.read_exact(&mut bytes)
            .map_err(|source| CoreError::Io {
                operation: "read recovery capsule segment",
                path,
                source,
            })?;
        Ok((bytes, descriptor.total_bytes, descriptor.capsule_digest))
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
                            || self.provider_object_reference_path(locator)?.is_file()
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

    fn recovery_capsule_path(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<PathBuf, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .root
            .join("recovery-capsules")
            .join(backup_id.to_string())
            .join(format!("{snapshot_id}.json")))
    }

    fn recovery_capsule_descriptor_path(
        &self,
        owner_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<PathBuf, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .root
            .join("recovery-capsule-index")
            .join(owner_device_id.to_string())
            .join(backup_id.to_string())
            .join(format!("{snapshot_id}.json")))
    }

    fn persist_recovery_capsule_descriptor_locked(
        &self,
        capsule: &RecoveryCapsule,
        bytes: &[u8],
    ) -> Result<(), CoreError> {
        self.persist_recovery_capsule_descriptor_value_locked(&RecoveryCapsuleDescriptor {
            backup_id: capsule.backup_id,
            snapshot_id: capsule.snapshot_id.clone(),
            key_epoch: capsule.key_epoch,
            committed_at_unix_ms: capsule.committed_at_unix_ms,
            signer_device_id: capsule.signer_device_id,
            total_bytes: bytes.len() as u64,
            capsule_digest: blake3::hash(bytes).to_hex().to_string(),
        })
    }

    fn persist_recovery_capsule_descriptor_value_locked(
        &self,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<(), CoreError> {
        validate_snapshot_id(&descriptor.snapshot_id)?;
        if descriptor.total_bytes == 0
            || descriptor.total_bytes > MAX_RECOVERY_CAPSULE_BYTES as u64
            || descriptor.capsule_digest.len() != 64
            || descriptor
                .capsule_digest
                .bytes()
                .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let path = self.recovery_capsule_descriptor_path(
            descriptor.signer_device_id,
            descriptor.backup_id,
            &descriptor.snapshot_id,
        )?;
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("recovery capsule index has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        write_json_atomic(&path, descriptor, true)
    }

    fn provider_lease_path(&self, lease: &StorageLease) -> Result<PathBuf, CoreError> {
        if lease.lease_id.is_empty() || lease.lease_id.len() > 128 {
            return Err(CoreError::InvalidState(
                "invalid storage lease id".to_owned(),
            ));
        }
        Ok(self
            .root
            .join("provider-leases")
            .join(lease.peer_device_id.to_string())
            .join(lease.backup_id.to_string())
            .join(format!("{}.json", lease.lease_id)))
    }

    fn recovery_capsule_upload_path(&self, lease: &StorageLease) -> PathBuf {
        self.root
            .join("provider-capsule-uploads")
            .join(lease.peer_device_id.to_string())
            .join(lease.backup_id.to_string())
            .join(&lease.lease_id)
    }

    fn load_recovery_capsule_upload_locked(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<RecoveryCapsuleUpload, CoreError> {
        let metadata: RecoveryCapsuleUpload = serde_json::from_slice(&read_bounded(
            &self
                .recovery_capsule_upload_path(lease)
                .join("metadata.json"),
            MAX_PROVIDER_LEASE_STATE_BYTES,
        )?)?;
        if metadata.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || metadata.upload_id != upload_id
            || metadata.lease != *lease
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(metadata)
    }

    fn load_active_provider_lease_locked(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        now_unix_ms: u64,
    ) -> Result<ProviderLeaseState, CoreError> {
        if lease.peer_device_id != peer_device_id
            || lease.backup_id != backup_id
            || lease.expires_at_unix_ms <= now_unix_ms
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let path = self.provider_lease_path(lease)?;
        let state: ProviderLeaseState =
            serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
        if state.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || state.cancelled
            || state.lease != *lease
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(state)
    }

    fn ensure_lease_consumption_locked(
        &self,
        state: &ProviderLeaseState,
        bytes: u64,
        objects: u64,
    ) -> Result<(), CoreError> {
        if state
            .consumed_new_bytes
            .checked_add(bytes)
            .is_none_or(|value| value > state.lease.max_new_bytes)
            || state
                .consumed_new_objects
                .checked_add(objects)
                .is_none_or(|value| value > state.lease.max_new_objects)
        {
            return Err(CoreError::ResourceLimit("provider lease quota"));
        }
        Ok(())
    }

    fn persist_provider_lease_locked(&self, state: &ProviderLeaseState) -> Result<(), CoreError> {
        write_json_atomic(&self.provider_lease_path(&state.lease)?, state, true)
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_provider_upload_journal_locked(
        &self,
        lease: &StorageLease,
        object_key: String,
        object: ProviderUploadKind,
        record_bytes: u64,
        record_digest: &str,
        recovery_capsule_descriptor: Option<RecoveryCapsuleDescriptor>,
        expected_new_object: bool,
        started_at_unix_ms: u64,
    ) -> Result<ProviderUploadJournal, CoreError> {
        let journal = ProviderUploadJournal {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            journal_id: uuid::Uuid::new_v4().to_string(),
            lease: lease.clone(),
            object_key,
            object,
            record_bytes,
            record_digest: record_digest.to_owned(),
            recovery_capsule_descriptor,
            expected_new_object,
            started_at_unix_ms,
        };
        write_json_atomic(&self.provider_upload_journal_path(&journal), &journal, true)?;
        Ok(journal)
    }

    fn provider_upload_journal_path(&self, journal: &ProviderUploadJournal) -> PathBuf {
        self.root
            .join("provider-upload-journal")
            .join(format!("{}.json", journal.journal_id))
    }

    fn complete_provider_upload_journal_locked(
        &self,
        journal: &ProviderUploadJournal,
    ) -> Result<(), CoreError> {
        let path = self.provider_upload_journal_path(journal);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(self.root.join("provider-upload-journal").as_path()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CoreError::Io {
                operation: "complete provider upload journal",
                path,
                source,
            }),
        }
    }

    fn recover_provider_upload_journals(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        for entry in read_directory_sorted(&self.root.join("provider-upload-journal"))? {
            let path = entry.path();
            let journal: ProviderUploadJournal =
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
            if journal.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
                || journal.started_at_unix_ms > journal.lease.expires_at_unix_ms
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let object_path = match &journal.object {
                ProviderUploadKind::Chunk { locator } => self.chunk_path(locator)?,
                ProviderUploadKind::RecoveryCapsule { snapshot_id } => {
                    self.recovery_capsule_path(journal.lease.backup_id, snapshot_id)?
                }
            };
            let maximum = match &journal.object {
                ProviderUploadKind::Chunk { .. } => {
                    (self.maximum_chunk_size + provider_record_overhead()) as u64
                }
                ProviderUploadKind::RecoveryCapsule { .. } => MAX_RECOVERY_CAPSULE_BYTES as u64,
            };
            let (record_bytes, record_digest) = match fs::symlink_metadata(&object_path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    hash_file_bounded(&object_path, maximum, "reconcile provider upload")?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    fs::remove_file(&path).map_err(|source| CoreError::Io {
                        operation: "discard empty provider upload journal",
                        path: path.clone(),
                        source,
                    })?;
                    continue;
                }
                _ => return Err(CoreError::AuthenticationFailed),
            };
            if record_bytes != journal.record_bytes || record_digest != journal.record_digest {
                return Err(CoreError::AuthenticationFailed);
            }
            let lease_path = self.provider_lease_path(&journal.lease)?;
            let mut state: ProviderLeaseState = serde_json::from_slice(&read_bounded(
                &lease_path,
                MAX_PROVIDER_LEASE_STATE_BYTES,
            )?)?;
            if state.lease != journal.lease || state.cancelled {
                return Err(CoreError::AuthenticationFailed);
            }
            if !state.objects.contains_key(&journal.object_key) {
                if journal.expected_new_object {
                    self.ensure_lease_consumption_locked(&state, journal.record_bytes, 1)?;
                    state.consumed_new_bytes = state
                        .consumed_new_bytes
                        .checked_add(journal.record_bytes)
                        .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
                    state.consumed_new_objects = state
                        .consumed_new_objects
                        .checked_add(1)
                        .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
                }
                if let ProviderUploadKind::Chunk { locator } = &journal.object {
                    self.add_provider_object_reference_locked(
                        locator,
                        journal.record_bytes,
                        journal.lease.peer_device_id,
                        journal.lease.backup_id,
                    )?;
                } else if matches!(journal.object, ProviderUploadKind::RecoveryCapsule { .. }) {
                    let descriptor = journal
                        .recovery_capsule_descriptor
                        .as_ref()
                        .ok_or(CoreError::AuthenticationFailed)?;
                    if descriptor.total_bytes != record_bytes
                        || descriptor.capsule_digest != record_digest
                        || descriptor.backup_id != journal.lease.backup_id
                        || descriptor.signer_device_id != journal.lease.peer_device_id
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    self.persist_recovery_capsule_descriptor_value_locked(descriptor)?;
                }
                state
                    .objects
                    .insert(journal.object_key.clone(), journal.record_bytes);
                self.persist_provider_lease_locked(&state)?;
            }
            fs::remove_file(&path).map_err(|source| CoreError::Io {
                operation: "reconcile provider upload journal",
                path: path.clone(),
                source,
            })?;
        }
        sync_directory(self.root.join("provider-upload-journal").as_path())
    }

    fn provider_object_reference_path(&self, locator: &str) -> Result<PathBuf, CoreError> {
        validate_hex_locator(locator)?;
        Ok(self
            .root
            .join("provider-object-refs")
            .join(&locator[..2])
            .join(format!("{}.json", &locator[2..])))
    }

    fn add_provider_object_reference_locked(
        &self,
        locator: &str,
        record_bytes: u64,
        peer_device_id: DeviceId,
        backup_id: BackupId,
    ) -> Result<(), CoreError> {
        let path = self.provider_object_reference_path(locator)?;
        let mut reference = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "invalid provider object reference".to_owned(),
                    ));
                }
                serde_json::from_slice::<ProviderObjectReference>(&read_bounded(
                    &path,
                    MAX_PROVIDER_LEASE_STATE_BYTES,
                )?)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => ProviderObjectReference {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                locator: locator.to_owned(),
                record_bytes,
                owners: BTreeSet::new(),
            },
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider object reference",
                    path,
                    source,
                });
            }
        };
        if reference.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || reference.locator != locator
            || reference.record_bytes != record_bytes
        {
            return Err(CoreError::AuthenticationFailed);
        }
        reference.owners.insert(ProviderObjectOwner {
            peer_device_id,
            backup_id,
        });
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("provider object reference has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        write_json_atomic(&path, &reference, true)
    }

    fn list_provider_lease_states_locked(&self) -> Result<Vec<ProviderLeaseState>, CoreError> {
        let mut states = Vec::new();
        for peer in read_directory_sorted(&self.root.join("provider-leases"))? {
            for backup in read_directory_sorted(&peer.path())? {
                for entry in read_directory_sorted(&backup.path())? {
                    let path = entry.path();
                    let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                        operation: "inspect provider lease state",
                        path: path.clone(),
                        source,
                    })?;
                    if metadata.file_type().is_symlink()
                        || !metadata.is_file()
                        || path.extension().and_then(|value| value.to_str()) != Some("json")
                    {
                        return Err(CoreError::InvalidState(
                            "invalid provider lease state".to_owned(),
                        ));
                    }
                    let state: ProviderLeaseState = serde_json::from_slice(&read_bounded(
                        &path,
                        MAX_PROVIDER_LEASE_STATE_BYTES,
                    )?)?;
                    if state.schema_version != PROVIDER_LEASE_SCHEMA_VERSION {
                        return Err(CoreError::InvalidState(
                            "unsupported provider lease state".to_owned(),
                        ));
                    }
                    states.push(state);
                    if states.len() > 1_000_000 {
                        return Err(CoreError::ResourceLimit("provider lease states"));
                    }
                }
            }
        }
        Ok(states)
    }

    fn provider_capacity_locked(
        &self,
        now_unix_ms: u64,
        peer_filter: Option<DeviceId>,
        backup_filter: Option<BackupId>,
    ) -> Result<ProviderCapacity, CoreError> {
        let mut total_used_bytes = 0_u64;
        let mut total_used_objects = 0_u64;
        let mut total_reserved_bytes = 0_u64;
        let mut total_reserved_objects = 0_u64;
        let mut peer_used_bytes = 0_u64;
        let mut peer_used_objects = 0_u64;
        let mut peer_reserved_bytes = 0_u64;
        let mut peer_reserved_objects = 0_u64;
        let mut backup_used_bytes = 0_u64;
        let mut backup_used_objects = 0_u64;
        let mut backup_reserved_bytes = 0_u64;
        let mut backup_reserved_objects = 0_u64;
        for state in self.list_provider_lease_states_locked()? {
            total_used_bytes = total_used_bytes.saturating_add(state.consumed_new_bytes);
            total_used_objects = total_used_objects.saturating_add(state.consumed_new_objects);
            let active = !state.cancelled && state.lease.expires_at_unix_ms > now_unix_ms;
            let reserved_bytes = if active {
                state
                    .lease
                    .max_new_bytes
                    .saturating_sub(state.consumed_new_bytes)
            } else {
                0
            };
            let reserved_objects = if active {
                state
                    .lease
                    .max_new_objects
                    .saturating_sub(state.consumed_new_objects)
            } else {
                0
            };
            total_reserved_bytes = total_reserved_bytes.saturating_add(reserved_bytes);
            total_reserved_objects = total_reserved_objects.saturating_add(reserved_objects);
            if peer_filter == Some(state.lease.peer_device_id) {
                peer_used_bytes = peer_used_bytes.saturating_add(state.consumed_new_bytes);
                peer_used_objects = peer_used_objects.saturating_add(state.consumed_new_objects);
                peer_reserved_bytes = peer_reserved_bytes.saturating_add(reserved_bytes);
                peer_reserved_objects = peer_reserved_objects.saturating_add(reserved_objects);
                if backup_filter == Some(state.lease.backup_id) {
                    backup_used_bytes = backup_used_bytes.saturating_add(state.consumed_new_bytes);
                    backup_used_objects =
                        backup_used_objects.saturating_add(state.consumed_new_objects);
                    backup_reserved_bytes = backup_reserved_bytes.saturating_add(reserved_bytes);
                    backup_reserved_objects =
                        backup_reserved_objects.saturating_add(reserved_objects);
                }
            }
        }
        let disk_available = fs2::available_space(&self.root).map_err(|source| CoreError::Io {
            operation: "inspect provider free space",
            path: self.root.clone(),
            source,
        })?;
        let mut available_bytes = self
            .provider_quota_policy
            .maximum_total_bytes
            .saturating_sub(total_used_bytes)
            .saturating_sub(total_reserved_bytes)
            .min(
                disk_available
                    .saturating_sub(self.provider_quota_policy.free_space_reserve_bytes)
                    .saturating_sub(total_reserved_bytes),
            );
        let mut available_objects = self
            .provider_quota_policy
            .maximum_total_objects
            .saturating_sub(total_used_objects)
            .saturating_sub(total_reserved_objects);
        if peer_filter.is_some() {
            available_bytes = available_bytes.min(
                self.provider_quota_policy
                    .maximum_peer_bytes
                    .saturating_sub(peer_used_bytes)
                    .saturating_sub(peer_reserved_bytes),
            );
            available_objects = available_objects.min(
                self.provider_quota_policy
                    .maximum_peer_objects
                    .saturating_sub(peer_used_objects)
                    .saturating_sub(peer_reserved_objects),
            );
        }
        if backup_filter.is_some() {
            available_bytes = available_bytes.min(
                self.provider_quota_policy
                    .maximum_backup_bytes
                    .saturating_sub(backup_used_bytes)
                    .saturating_sub(backup_reserved_bytes),
            );
            available_objects = available_objects.min(
                self.provider_quota_policy
                    .maximum_backup_objects
                    .saturating_sub(backup_used_objects)
                    .saturating_sub(backup_reserved_objects),
            );
        }
        Ok(ProviderCapacity {
            available_bytes,
            reserved_bytes: total_reserved_bytes,
            available_objects,
            reserved_objects: total_reserved_objects,
            free_space_reserve_bytes: self.provider_quota_policy.free_space_reserve_bytes,
        })
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

fn recovery_capsule_cursor(capsule: &RecoveryCapsule) -> String {
    format!("{}/{}", capsule.backup_id, capsule.snapshot_id)
}

fn hash_file_bounded(
    path: &Path,
    maximum_bytes: u64,
    operation: &'static str,
) -> Result<(u64, String), CoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(CoreError::AuthenticationFailed);
    }
    let mut file = fs::File::open(path).map_err(|source| CoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(CoreError::ResourceLimit("provider object size"))?;
        if total > maximum_bytes {
            return Err(CoreError::ResourceLimit("provider object size"));
        }
        hasher.update(&buffer[..read]);
    }
    if total != metadata.len() {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
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
    fn provider_upload_journal_reconciles_every_commit_boundary_without_quota_bypass() {
        for boundary in 1_u8..=3 {
            let directory = tempdir().expect("temporary store");
            let peer_device_id = DeviceId::new();
            let provider_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let encrypted = BackupKey::generate()
                .encrypt_chunk(backup_id, 1, format!("boundary-{boundary}").as_bytes())
                .expect("chunk");
            let record = encrypted.encode_provider_record();
            let lease = StorageLease {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease_id: uuid::Uuid::new_v4().to_string(),
                peer_device_id,
                provider_device_id,
                backup_id,
                max_new_bytes: record.len() as u64,
                max_new_objects: 1,
                issued_at_unix_ms: 100,
                expires_at_unix_ms: 1_000,
                nonce: "test-nonce".to_owned(),
                signature: "test-signature".to_owned(),
            };
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            store.reserve_provider_lease(&lease).expect("lease");
            PROVIDER_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .put_provider_record_leased(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &encrypted.opaque_locator,
                        &record,
                        200,
                    )
                    .is_err()
            );
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("reopen");
            let created = reopened
                .put_provider_record_leased(
                    peer_device_id,
                    backup_id,
                    &lease,
                    &encrypted.opaque_locator,
                    &record,
                    300,
                )
                .expect("retry after recovery");
            assert_eq!(created, boundary == 1);
            assert!(
                read_directory_sorted(&reopened.root.join("provider-upload-journal"))
                    .expect("journals")
                    .is_empty()
            );
            let state: ProviderLeaseState = serde_json::from_slice(
                &read_bounded(
                    &reopened.provider_lease_path(&lease).expect("lease path"),
                    MAX_PROVIDER_LEASE_STATE_BYTES,
                )
                .expect("lease state"),
            )
            .expect("lease json");
            assert_eq!(state.consumed_new_bytes, record.len() as u64);
            assert_eq!(state.consumed_new_objects, 1);
        }
    }

    #[test]
    fn provider_read_batch_is_exactly_peer_and_backup_scoped() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let peer_device_id = DeviceId::new();
        let other_peer_device_id = DeviceId::new();
        let provider_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let other_backup_id = BackupId::new();
        let encrypted = BackupKey::generate()
            .encrypt_chunk(backup_id, 1, b"tenant-owned")
            .expect("chunk");
        let record = encrypted.encode_provider_record();
        let lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id,
            backup_id,
            max_new_bytes: record.len() as u64,
            max_new_objects: 1,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "tenant-nonce".to_owned(),
            signature: "test-signature".to_owned(),
        };
        store.reserve_provider_lease(&lease).expect("lease");
        store
            .put_provider_record_leased(
                peer_device_id,
                backup_id,
                &lease,
                &encrypted.opaque_locator,
                &record,
                200,
            )
            .expect("put");
        let locators = vec![encrypted.opaque_locator];
        assert!(
            store
                .authorize_provider_record_batch(peer_device_id, backup_id, &locators)
                .is_ok()
        );
        assert!(
            store
                .authorize_provider_record_batch(other_peer_device_id, backup_id, &locators)
                .is_err()
        );
        assert!(
            store
                .authorize_provider_record_batch(peer_device_id, other_backup_id, &locators)
                .is_err()
        );
        assert!(
            store
                .authorize_provider_record_batch(
                    peer_device_id,
                    backup_id,
                    &[locators[0].clone(), locators[0].clone()],
                )
                .is_err()
        );
    }

    #[test]
    fn recovery_capsule_pages_do_not_leak_cross_tenant_metadata() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let first_owner = DeviceId::new();
        let second_owner = DeviceId::new();
        let first_backup = BackupId::new();
        let second_backup = BackupId::new();
        let capsule = |owner, backup_id, snapshot_id: &str| RecoveryCapsule {
            schema_version: 1,
            cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
            backup_id,
            snapshot_id: snapshot_id.to_owned(),
            key_epoch: 1,
            committed_at_unix_ms: 1,
            nonce: "opaque".to_owned(),
            ciphertext: "opaque".to_owned(),
            signer_device_id: owner,
            signature: "opaque".to_owned(),
        };
        store
            .put_recovery_capsule(&capsule(first_owner, first_backup, "first"))
            .expect("first capsule");
        store
            .put_recovery_capsule(&capsule(second_owner, second_backup, "private-second"))
            .expect("second capsule");

        let (visible, cursor) = store
            .list_recovery_capsules_for_owner(first_owner, None, None, 128)
            .expect("owner page");
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].backup_id, first_backup);
        assert_eq!(visible[0].snapshot_id, "first");
        assert!(cursor.is_none());
        let encoded = serde_json::to_string(&visible).expect("json");
        assert!(!encoded.contains("private-second"));
        assert!(!encoded.contains(&second_backup.to_string()));
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
