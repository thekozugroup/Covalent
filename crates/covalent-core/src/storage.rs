#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::OwnedFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
const MAX_PROVIDER_LEASE_LEDGER_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_ACTIVE_PROVIDER_LEASES: usize = 256;
const MAX_ACTIVE_PROVIDER_LEASES_PER_PEER: usize = 32;
const MAX_RETAINED_PROVIDER_LEASES: usize = 256;
const MAX_RETAINED_PROVIDER_LEASES_PER_PEER: usize = 32;
const MAX_RETAINED_PROVIDER_LEASE_SCOPES: usize = 4_096;
const MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER: usize = 256;
const MAX_PROVIDER_LEASE_STATE_FILES: usize =
    MAX_ACTIVE_PROVIDER_LEASES + MAX_RETAINED_PROVIDER_LEASES;
const MAX_PROVIDER_LEASE_STATE_FILES_PER_PEER: usize =
    MAX_ACTIVE_PROVIDER_LEASES_PER_PEER + MAX_RETAINED_PROVIDER_LEASES_PER_PEER;
const MAX_PROVIDER_UPLOAD_RECEIPT_BYTES: usize = 64 * 1_024;
const MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER: usize = 256;
const MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK: usize =
    MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER + 1;
const MAX_PROVIDER_UPLOAD_RECEIPT_PEERS: usize = MAX_RETAINED_PROVIDER_LEASE_SCOPES;
const MAX_LEGACY_PROVIDER_UPLOAD_RECEIPTS: usize = 257;
const MAX_PROVIDER_UPLOAD_JOURNALS: usize = 1_024;
const MAX_LOCAL_WRITE_JOURNALS: usize = 1_024;
const MAX_PROVIDER_CAPSULE_STAGING_PEERS: usize = MAX_RETAINED_PROVIDER_LEASE_SCOPES;
const MAX_PROVIDER_CAPSULE_STAGING_BACKUPS: usize = MAX_RETAINED_PROVIDER_LEASE_SCOPES;
const MAX_PROVIDER_CAPSULE_STAGING_LEASES: usize = 1_024;
const MAX_PROVIDER_WRITE_BATCH_RECORDS: usize = 64;
const MAX_PROVIDER_WRITE_BATCH_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_LOCAL_WRITE_BATCH_RECORDS: usize = 64;
const MAX_LOCAL_WRITE_BATCH_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_RECOVERY_CAPSULE_SEGMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_RECOVERY_CAPSULE_SEGMENTS: u32 = 128;
const RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION: u16 = 1;
const RECOVERY_CAPSULE_PAGE_ROOT_SCHEMA_VERSION: u16 = 2;
const MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES: usize = 8 * 1_024;
const MAX_RECOVERY_CAPSULE_PAGE_STATE_BYTES: usize = 1_024;
const MAX_RECOVERY_CAPSULE_PAGE_MARKER_BYTES: usize = 2 * 1_024;
const RECOVERY_CAPSULE_UPLOAD_ATTEMPT_SCHEMA_VERSION: u16 = 1;
const MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS: usize = 256;
const MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES: usize = 16 * 1_024;
const RECOVERY_CAPSULE_LEASE_INTENT_SCHEMA_VERSION: u16 = 1;
const MAX_RECOVERY_CAPSULE_LEASE_INTENTS: usize = 256;
const MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES: usize = 16 * 1_024;
const PROVIDER_WRITE_LEASE_INTENT_SCHEMA_VERSION: u16 = 1;
const MAX_PROVIDER_WRITE_LEASE_INTENTS: usize = 256;
const MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES: usize = 8 * 1_024;
#[cfg(test)]
thread_local! {
    /// Arms exactly one provider-upload abort, scoped to the thread that armed it.
    /// A process-global failpoint would let tests running in parallel consume each
    /// other's arming, so the boundary is deliberately thread-local.
    static PROVIDER_UPLOAD_FAILPOINT: Cell<u8> = const { Cell::new(0) };
    static LOCAL_WRITE_BATCH_FAILPOINT: Cell<u8> = const { Cell::new(0) };
    static PROVIDER_LEASE_COMPACTION_FAILPOINT: Cell<u8> = const { Cell::new(0) };
    static PROVIDER_UPLOAD_RECEIPT_FAILPOINT: Cell<u8> = const { Cell::new(0) };
    static RECOVERY_CAPSULE_PAGE_READS: Cell<usize> = const { Cell::new(0) };
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

#[cfg(test)]
fn local_write_batch_failpoint(boundary: u8) -> Result<(), CoreError> {
    LOCAL_WRITE_BATCH_FAILPOINT.with(|armed| {
        if armed.get() == boundary {
            armed.set(0);
            return Err(CoreError::InvalidState(format!(
                "local write batch failpoint {boundary}"
            )));
        }
        Ok(())
    })
}

#[cfg(test)]
fn provider_lease_compaction_failpoint(boundary: u8) -> Result<(), CoreError> {
    PROVIDER_LEASE_COMPACTION_FAILPOINT.with(|armed| {
        if armed.get() == boundary {
            armed.set(0);
            return Err(CoreError::InvalidState(format!(
                "provider lease compaction failpoint {boundary}"
            )));
        }
        Ok(())
    })
}

#[cfg(test)]
fn provider_upload_receipt_failpoint(boundary: u8) -> Result<(), CoreError> {
    PROVIDER_UPLOAD_RECEIPT_FAILPOINT.with(|armed| {
        if armed.get() == boundary {
            armed.set(0);
            return Err(CoreError::InvalidState(format!(
                "provider upload receipt failpoint {boundary}"
            )));
        }
        Ok(())
    })
}

#[cfg(not(test))]
const fn provider_upload_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

#[cfg(not(test))]
const fn local_write_batch_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

#[cfg(not(test))]
const fn provider_lease_compaction_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

#[cfg(not(test))]
const fn provider_upload_receipt_failpoint(_boundary: u8) -> Result<(), CoreError> {
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
    #[serde(default)]
    deferred_reference_sync: BTreeSet<String>,
    #[serde(default)]
    staged_capsule_upload: Option<StagedCapsuleUpload>,
    cancelled: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderLeaseUsage {
    consumed_new_bytes: u64,
    consumed_new_objects: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderPeerLeaseUsage {
    backups: BTreeMap<BackupId, ProviderLeaseUsage>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderLeaseCompaction {
    peer_device_id: DeviceId,
    backup_id: BackupId,
    lease_id: String,
    state_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderLeaseLedger {
    schema_version: u16,
    peers: BTreeMap<DeviceId, ProviderPeerLeaseUsage>,
    pending_compactions: Vec<ProviderLeaseCompaction>,
}

impl Default for ProviderLeaseLedger {
    fn default() -> Self {
        Self {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            peers: BTreeMap::new(),
            pending_compactions: Vec::new(),
        }
    }
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
    #[serde(default)]
    deferred_reference_candidate: bool,
    started_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderUploadBatchJournal {
    schema_version: u16,
    journal_id: String,
    uploads: Vec<ProviderUploadJournal>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalWriteBatchEntry {
    locator: String,
    record_bytes: u64,
    record_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LocalWriteBatchJournal {
    schema_version: u16,
    journal_id: String,
    entries: Vec<LocalWriteBatchEntry>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StagedCapsuleUpload {
    upload: RecoveryCapsuleUpload,
    expected_new_object: bool,
    #[serde(default)]
    committed_created: Option<bool>,
    #[serde(default)]
    completed_at_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderUploadReceipt {
    schema_version: u16,
    lease: StorageLease,
    upload_id: String,
    created: bool,
    completed_at_unix_ms: u64,
}

/// Signed capacity facts returned by the provider handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCapacity {
    pub available_bytes: u64,
    pub allocated_bytes: u64,
    pub quota_bytes: u64,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryCapsuleUploadAttemptPhase {
    LeaseAcquired,
    Uploading { next_segment: u32 },
    CommitPending,
    CommitAccepted,
}

/// Private owner-side identity needed to resume or reconcile one remote capsule upload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryCapsuleUploadAttempt {
    pub schema_version: u16,
    pub provider_device_id: DeviceId,
    pub backup_id: BackupId,
    pub snapshot_id: String,
    pub capsule_digest: String,
    pub total_bytes: u64,
    pub total_segments: u32,
    pub lease: StorageLease,
    pub upload_id: String,
    pub phase: RecoveryCapsuleUploadAttemptPhase,
}

impl RecoveryCapsuleUploadAttempt {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: String,
        capsule_digest: String,
        total_bytes: u64,
        total_segments: u32,
        lease: StorageLease,
        upload_id: String,
    ) -> Self {
        Self {
            schema_version: RECOVERY_CAPSULE_UPLOAD_ATTEMPT_SCHEMA_VERSION,
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
            total_bytes,
            total_segments,
            lease,
            upload_id,
            phase: RecoveryCapsuleUploadAttemptPhase::LeaseAcquired,
        }
    }
}

/// Private owner-side intent persisted before asking a provider to reserve quota.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryCapsuleLeaseIntent {
    pub schema_version: u16,
    pub provider_device_id: DeviceId,
    pub backup_id: BackupId,
    pub snapshot_id: String,
    pub capsule_digest: String,
    pub total_bytes: u64,
    pub total_segments: u32,
    pub acquisition_id: String,
    pub upload_id: String,
}

impl RecoveryCapsuleLeaseIntent {
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: String,
        capsule_digest: String,
        total_bytes: u64,
        total_segments: u32,
        acquisition_id: String,
        upload_id: String,
    ) -> Self {
        Self {
            schema_version: RECOVERY_CAPSULE_LEASE_INTENT_SCHEMA_VERSION,
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
            total_bytes,
            total_segments,
            acquisition_id,
            upload_id,
        }
    }
}

/// Private owner-side reservation identity retained for the full remote write lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderWriteLeaseIntent {
    pub schema_version: u16,
    pub provider_device_id: DeviceId,
    pub backup_id: BackupId,
    pub maximum_new_bytes: u64,
    pub maximum_new_objects: u64,
    pub acquisition_id: String,
}

impl ProviderWriteLeaseIntent {
    #[must_use]
    pub const fn new(
        provider_device_id: DeviceId,
        backup_id: BackupId,
        maximum_new_bytes: u64,
        maximum_new_objects: u64,
        acquisition_id: String,
    ) -> Self {
        Self {
            schema_version: PROVIDER_WRITE_LEASE_INTENT_SCHEMA_VERSION,
            provider_device_id,
            backup_id,
            maximum_new_bytes,
            maximum_new_objects,
            acquisition_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsulePageState {
    schema_version: u16,
    next_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsulePageEntry {
    schema_version: u16,
    descriptor: RecoveryCapsuleDescriptor,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsulePageMarker {
    schema_version: u16,
    descriptor_digest: String,
    all_sequence: u64,
    backup_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsulePageSchema {
    schema_version: u16,
    #[serde(default)]
    generation: String,
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
    #[cfg(unix)]
    root_descriptor: Arc<OwnedFd>,
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

#[cfg(unix)]
struct AnchoredDirectory {
    descriptor: OwnedFd,
    path: PathBuf,
}

#[cfg(not(unix))]
struct AnchoredDirectory {
    path: PathBuf,
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
            "recovery-capsule-pages",
            "recovery-upload-attempts",
            "recovery-upload-intents",
            "provider-write-intents",
            "provider-leases",
            "provider-object-refs",
            "provider-upload-journal",
            "local-write-journal",
            "provider-upload-receipts",
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
        #[cfg(unix)]
        let root_descriptor = {
            use rustix::fs::{Mode, OFlags, open};
            Arc::new(
                open(
                    &root,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(|error| CoreError::Io {
                    operation: "open chunk store root handle",
                    path: root.clone(),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                })?,
            )
        };
        let store = Self {
            root,
            #[cfg(unix)]
            root_descriptor,
            maximum_chunk_size,
            transaction_lock: Arc::new(Mutex::new(())),
            snapshot_generation: Arc::new(AtomicU64::new(0)),
            provider_quota_policy: Arc::new(provider_quota_policy),
        };
        store.migrate_legacy_recovery_capsules()?;
        store.recover_local_write_batch_journals()?;
        store.recover_provider_upload_receipts()?;
        store.recover_provider_upload_journals()?;
        store.recover_recovery_capsule_page_index()?;
        store.validate_recovery_capsule_upload_attempts()?;
        store.validate_recovery_capsule_lease_intents()?;
        store.validate_provider_write_lease_intents()?;
        store.recover_provider_lease_references()?;
        let now_unix_ms = provider_wall_clock_unix_ms()?;
        store.recover_recovery_capsule_uploads(now_unix_ms)?;
        store.compact_provider_leases(now_unix_ms)?;
        Ok(store)
    }

    fn migrate_legacy_recovery_capsules(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let root = self.root.join("recovery-capsules");
        ensure_private_directory(&root.join("by-owner"))?;
        let mut backups_seen = 0_u64;
        for backup_entry in fs::read_dir(&root).map_err(|source| CoreError::Io {
            operation: "read legacy recovery capsule root",
            path: root.clone(),
            source,
        })? {
            let backup_entry = backup_entry.map_err(|source| CoreError::Io {
                operation: "read legacy recovery capsule entry",
                path: root.clone(),
                source,
            })?;
            if backup_entry.file_name() == "by-owner" {
                continue;
            }
            backups_seen = backups_seen
                .checked_add(1)
                .ok_or(CoreError::ResourceLimit("legacy recovery capsule backups"))?;
            if backups_seen > MAX_RETAINED_PROVIDER_LEASE_SCOPES as u64 {
                return Err(CoreError::ResourceLimit("legacy recovery capsule backups"));
            }
            let backup_path = backup_entry.path();
            let metadata = fs::symlink_metadata(&backup_path).map_err(|source| CoreError::Io {
                operation: "inspect legacy recovery capsule backup",
                path: backup_path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::AuthenticationFailed);
            }
            let backup_id = BackupId::from_str(&backup_entry.file_name().to_string_lossy())
                .map_err(|_| CoreError::AuthenticationFailed)?;
            let mut capsules_seen = 0_u64;
            for capsule_entry in fs::read_dir(&backup_path).map_err(|source| CoreError::Io {
                operation: "read legacy recovery capsule backup",
                path: backup_path.clone(),
                source,
            })? {
                let capsule_entry = capsule_entry.map_err(|source| CoreError::Io {
                    operation: "read legacy recovery capsule",
                    path: backup_path.clone(),
                    source,
                })?;
                capsules_seen = capsules_seen
                    .checked_add(1)
                    .ok_or(CoreError::ResourceLimit(
                        "legacy recovery capsules per backup",
                    ))?;
                if capsules_seen > self.provider_quota_policy.maximum_backup_objects {
                    return Err(CoreError::ResourceLimit(
                        "legacy recovery capsules per backup",
                    ));
                }
                let source_path = capsule_entry.path();
                let bytes = read_private_regular_file_bounded(
                    &source_path,
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "read legacy recovery capsule",
                )?;
                let capsule: RecoveryCapsule = serde_json::from_slice(&bytes)?;
                if capsule.backup_id != backup_id
                    || source_path.extension().and_then(|value| value.to_str()) != Some("json")
                    || source_path.file_stem().and_then(|value| value.to_str())
                        != Some(capsule.snapshot_id.as_str())
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                let destination = self.recovery_capsule_path(
                    capsule.signer_device_id,
                    capsule.backup_id,
                    &capsule.snapshot_id,
                )?;
                let parent = destination.parent().ok_or_else(|| {
                    CoreError::InvalidState("migrated recovery capsule has no parent".to_owned())
                })?;
                ensure_private_directory(parent)?;
                match fs::symlink_metadata(&destination) {
                    Ok(existing) if existing.is_file() && !existing.file_type().is_symlink() => {
                        if read_private_regular_file_bounded(
                            &destination,
                            MAX_RECOVERY_CAPSULE_BYTES as u64,
                            "read migrated recovery capsule incumbent",
                        )? != bytes
                        {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        fs::remove_file(&source_path).map_err(|source| CoreError::Io {
                            operation: "remove duplicate legacy recovery capsule",
                            path: source_path.clone(),
                            source,
                        })?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        fs::rename(&source_path, &destination).map_err(|source| CoreError::Io {
                            operation: "migrate legacy recovery capsule",
                            path: source_path.clone(),
                            source,
                        })?;
                        sync_directory(parent)?;
                    }
                    _ => return Err(CoreError::AuthenticationFailed),
                }
            }
            sync_directory(&backup_path)?;
            remove_empty_directory(&backup_path, &root)?;
        }
        sync_directory(&root)
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

    fn anchored_directory(
        &self,
        base: &str,
        components: &[String],
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        if !valid_single_path_component(base)
            || components
                .iter()
                .any(|component| !valid_single_path_component(component))
        {
            return Err(CoreError::AuthenticationFailed);
        }

        #[cfg(unix)]
        {
            use rustix::fs::{FileType, Mode, OFlags, fchmod, fstat, mkdirat, openat};

            let root_stat = fstat(&*self.root_descriptor).map_err(|error| CoreError::Io {
                operation,
                path: self.root.clone(),
                source: std::io::Error::from_raw_os_error(error.raw_os_error()),
            })?;
            let mut descriptor =
                rustix::io::dup(&*self.root_descriptor).map_err(|error| CoreError::Io {
                    operation,
                    path: self.root.clone(),
                    source: std::io::Error::from_raw_os_error(error.raw_os_error()),
                })?;
            let mut path = self.root.clone();
            for component in std::iter::once(base).chain(components.iter().map(String::as_str)) {
                path.push(component);
                let open_directory = || {
                    openat(
                        &descriptor,
                        component,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                        Mode::empty(),
                    )
                };
                let next = match open_directory() {
                    Ok(next) => next,
                    Err(rustix::io::Errno::NOENT) if create => {
                        match mkdirat(&descriptor, component, Mode::from_raw_mode(0o700)) {
                            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                            Err(error) => {
                                return Err(anchored_io_error(operation, &path, error));
                            }
                        }
                        open_directory()
                            .map_err(|error| anchored_io_error(operation, &path, error))?
                    }
                    Err(rustix::io::Errno::NOENT) => return Ok(None),
                    Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    Err(error) => return Err(anchored_io_error(operation, &path, error)),
                };
                let mut stat =
                    fstat(&next).map_err(|error| anchored_io_error(operation, &path, error))?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                    || stat.st_uid != root_stat.st_uid
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                // v0.1 created a few nested metadata directories through
                // `create_dir_all`, so an ordinary umask could leave an
                // intermediate owner directory 0755 even though its leaf was
                // private. Migration is allowed to tighten an already opened,
                // same-owner, no-follow directory; normal read-only traversal
                // still rejects permissive modes instead of mutating them.
                if stat.st_mode & 0o077 != 0 {
                    if !create {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    fchmod(&next, Mode::from_raw_mode(0o700))
                        .map_err(|error| anchored_io_error(operation, &path, error))?;
                    stat =
                        fstat(&next).map_err(|error| anchored_io_error(operation, &path, error))?;
                    if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                        || stat.st_uid != root_stat.st_uid
                        || stat.st_mode & 0o077 != 0
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                }
                descriptor = next;
            }
            Ok(Some(AnchoredDirectory { descriptor, path }))
        }

        #[cfg(not(unix))]
        {
            let mut path = self.root.join(base);
            for component in components {
                path.push(component);
            }
            if create {
                ensure_private_directory(&path)?;
            } else {
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                    Ok(_) => return Err(CoreError::AuthenticationFailed),
                    Err(source) => {
                        return Err(CoreError::Io {
                            operation,
                            path,
                            source,
                        });
                    }
                }
            }
            Ok(Some(AnchoredDirectory { path }))
        }
    }

    fn anchored_directory_for_store_path(
        &self,
        path: &Path,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let mut components = relative.components();
        let base = match components.next() {
            Some(std::path::Component::Normal(value)) => {
                value.to_str().ok_or(CoreError::AuthenticationFailed)?
            }
            _ => return Err(CoreError::AuthenticationFailed),
        };
        let remaining = components
            .map(|component| match component {
                std::path::Component::Normal(value) => value
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(CoreError::AuthenticationFailed),
                _ => Err(CoreError::AuthenticationFailed),
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.anchored_directory(base, &remaining, create, operation)
    }

    fn anchored_directory_entries_bounded(
        &self,
        directory: &AnchoredDirectory,
        maximum: usize,
        resource: &'static str,
    ) -> Result<Vec<std::ffi::OsString>, CoreError> {
        #[cfg(unix)]
        {
            let mut entries = Vec::new();
            let mut reader =
                rustix::fs::Dir::read_from(&directory.descriptor).map_err(|error| {
                    anchored_io_error("read anchored storage directory", &directory.path, error)
                })?;
            for entry in &mut reader {
                let entry = entry.map_err(|error| {
                    anchored_io_error("read anchored storage directory", &directory.path, error)
                })?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                entries.push(std::ffi::OsString::from_vec(name.to_vec()));
                if entries.len() > maximum {
                    return Err(CoreError::ResourceLimit(resource));
                }
            }
            entries.sort();
            Ok(entries)
        }

        #[cfg(not(unix))]
        {
            let entries = read_directory_sorted_bounded(&directory.path, maximum, resource)?;
            Ok(entries.into_iter().map(|entry| entry.file_name()).collect())
        }
    }

    fn open_anchored_private_file_bounded(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        maximum_bytes: u64,
        expected_bytes: Option<u64>,
        operation: &'static str,
    ) -> Result<Option<(fs::File, u64)>, CoreError> {
        if !valid_single_path_component(name) {
            return Err(CoreError::AuthenticationFailed);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

            let descriptor = match openat(
                &directory.descriptor,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            ) {
                Ok(descriptor) => descriptor,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                    return Err(CoreError::AuthenticationFailed);
                }
                Err(error) => {
                    return Err(anchored_io_error(
                        operation,
                        &directory.path.join(name),
                        error,
                    ));
                }
            };
            let stat = fstat(&descriptor)
                .map_err(|error| anchored_io_error(operation, &directory.path.join(name), error))?;
            let parent_stat = fstat(&directory.descriptor)
                .map_err(|error| anchored_io_error(operation, &directory.path, error))?;
            let length = u64::try_from(stat.st_size)
                .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                || stat.st_uid != parent_stat.st_uid
                || stat.st_mode & 0o077 != 0
                || length == 0
                || length > maximum_bytes
                || expected_bytes.is_some_and(|expected| expected != length)
            {
                return Err(CoreError::AuthenticationFailed);
            }
            Ok(Some((fs::File::from(descriptor), length)))
        }

        #[cfg(not(unix))]
        {
            let path = directory.path.join(name);
            match open_private_regular_file_bounded(&path, maximum_bytes, expected_bytes, operation)
            {
                Ok(value) => Ok(Some(value)),
                Err(CoreError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            }
        }
    }

    fn read_anchored_private_file_bounded(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        maximum_bytes: u64,
        operation: &'static str,
    ) -> Result<Option<Vec<u8>>, CoreError> {
        let Some((mut file, original_length)) = self.open_anchored_private_file_bounded(
            directory,
            name,
            maximum_bytes,
            None,
            operation,
        )?
        else {
            return Ok(None);
        };
        let capacity = usize::try_from(original_length)
            .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
        (&mut file)
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|source| CoreError::Io {
                operation,
                path: directory.path.join(name),
                source,
            })?;
        let final_length = file
            .metadata()
            .map_err(|source| CoreError::Io {
                operation,
                path: directory.path.join(name),
                source,
            })?
            .len();
        if bytes.len() as u64 != original_length || final_length != original_length {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(bytes))
    }

    fn hash_anchored_private_file_bounded(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        maximum_bytes: u64,
        operation: &'static str,
    ) -> Result<Option<(u64, String)>, CoreError> {
        let Some((mut file, original_length)) = self.open_anchored_private_file_bounded(
            directory,
            name,
            maximum_bytes,
            None,
            operation,
        )?
        else {
            return Ok(None);
        };
        let mut hasher = blake3::Hasher::new();
        let mut total = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(|source| CoreError::Io {
                operation,
                path: directory.path.join(name),
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
        if total != original_length
            || file
                .metadata()
                .map_err(|source| CoreError::Io {
                    operation,
                    path: directory.path.join(name),
                    source,
                })?
                .len()
                != original_length
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some((total, hasher.finalize().to_hex().to_string())))
    }

    fn write_anchored_atomic(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        bytes: &[u8],
        no_clobber: bool,
        operation: &'static str,
    ) -> Result<bool, CoreError> {
        if !valid_single_path_component(name) {
            return Err(CoreError::AuthenticationFailed);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, Mode, OFlags, fsync, linkat, openat, renameat, unlinkat};

            let temporary_name = format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple());
            let descriptor = openat(
                &directory.descriptor,
                temporary_name.as_str(),
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| {
                anchored_io_error(operation, &directory.path.join(&temporary_name), error)
            })?;
            let mut temporary = fs::File::from(descriptor);
            if let Err(source) = temporary
                .write_all(bytes)
                .and_then(|()| temporary.flush())
                .and_then(|()| temporary.sync_all())
            {
                let _ = unlinkat(
                    &directory.descriptor,
                    temporary_name.as_str(),
                    AtFlags::empty(),
                );
                return Err(CoreError::Io {
                    operation,
                    path: directory.path.join(&temporary_name),
                    source,
                });
            }
            drop(temporary);
            let created = if no_clobber {
                match linkat(
                    &directory.descriptor,
                    temporary_name.as_str(),
                    &directory.descriptor,
                    name,
                    AtFlags::empty(),
                ) {
                    Ok(()) => true,
                    Err(rustix::io::Errno::EXIST) => false,
                    Err(error) => {
                        let _ = unlinkat(
                            &directory.descriptor,
                            temporary_name.as_str(),
                            AtFlags::empty(),
                        );
                        return Err(anchored_io_error(
                            operation,
                            &directory.path.join(name),
                            error,
                        ));
                    }
                }
            } else {
                renameat(
                    &directory.descriptor,
                    temporary_name.as_str(),
                    &directory.descriptor,
                    name,
                )
                .map_err(|error| anchored_io_error(operation, &directory.path.join(name), error))?;
                true
            };
            if no_clobber || !created {
                match unlinkat(
                    &directory.descriptor,
                    temporary_name.as_str(),
                    AtFlags::empty(),
                ) {
                    Ok(()) | Err(rustix::io::Errno::NOENT) => {}
                    Err(error) => {
                        return Err(anchored_io_error(
                            operation,
                            &directory.path.join(&temporary_name),
                            error,
                        ));
                    }
                }
            }
            fsync(&directory.descriptor)
                .map_err(|error| anchored_io_error(operation, &directory.path, error))?;
            Ok(created)
        }

        #[cfg(not(unix))]
        {
            let path = directory.path.join(name);
            if no_clobber {
                write_atomic_noclobber(&path, bytes, true)
            } else {
                write_atomic(&path, bytes, true)?;
                Ok(true)
            }
        }
    }

    fn remove_anchored_file(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        operation: &'static str,
    ) -> Result<bool, CoreError> {
        if !valid_single_path_component(name) {
            return Err(CoreError::AuthenticationFailed);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, fsync, unlinkat};
            match unlinkat(&directory.descriptor, name, AtFlags::empty()) {
                Ok(()) => {
                    fsync(&directory.descriptor)
                        .map_err(|error| anchored_io_error(operation, &directory.path, error))?;
                    Ok(true)
                }
                Err(rustix::io::Errno::NOENT) => Ok(false),
                Err(rustix::io::Errno::LOOP | rustix::io::Errno::ISDIR) => {
                    Err(CoreError::AuthenticationFailed)
                }
                Err(error) => Err(anchored_io_error(
                    operation,
                    &directory.path.join(name),
                    error,
                )),
            }
        }
        #[cfg(not(unix))]
        {
            let path = directory.path.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {
                    sync_directory(&directory.path)?;
                    Ok(true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(source) => Err(CoreError::Io {
                    operation,
                    path,
                    source,
                }),
            }
        }
    }

    fn remove_anchored_empty_directory(
        &self,
        parent: &AnchoredDirectory,
        name: &str,
        operation: &'static str,
    ) -> Result<bool, CoreError> {
        if !valid_single_path_component(name) {
            return Err(CoreError::AuthenticationFailed);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, fsync, unlinkat};
            match unlinkat(&parent.descriptor, name, AtFlags::REMOVEDIR) {
                Ok(()) => {
                    fsync(&parent.descriptor)
                        .map_err(|error| anchored_io_error(operation, &parent.path, error))?;
                    Ok(true)
                }
                Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTEMPTY) => Ok(false),
                Err(rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR) => {
                    Err(CoreError::AuthenticationFailed)
                }
                Err(error) => Err(anchored_io_error(operation, &parent.path.join(name), error)),
            }
        }
        #[cfg(not(unix))]
        {
            let path = parent.path.join(name);
            match fs::remove_dir(&path) {
                Ok(()) => {
                    sync_directory(&parent.path)?;
                    Ok(true)
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
                    ) =>
                {
                    Ok(false)
                }
                Err(source) => Err(CoreError::Io {
                    operation,
                    path,
                    source,
                }),
            }
        }
    }

    fn create_anchored_private_file(
        &self,
        directory: &AnchoredDirectory,
        name: &str,
        operation: &'static str,
    ) -> Result<fs::File, CoreError> {
        if !valid_single_path_component(name) {
            return Err(CoreError::AuthenticationFailed);
        }
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let descriptor = openat(
                &directory.descriptor,
                name,
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| anchored_io_error(operation, &directory.path.join(name), error))?;
            Ok(fs::File::from(descriptor))
        }
        #[cfg(not(unix))]
        {
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(directory.path.join(name))
                .map_err(|source| CoreError::Io {
                    operation,
                    path: directory.path.join(name),
                    source,
                })
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn copy_anchored_private_file_noclobber(
        &self,
        source: &mut fs::File,
        source_path: &Path,
        expected_bytes: u64,
        expected_digest: &str,
        destination: &AnchoredDirectory,
        name: &str,
        operation: &'static str,
    ) -> Result<bool, CoreError> {
        if !valid_single_path_component(name) || validate_hex_locator(expected_digest).is_err() {
            return Err(CoreError::AuthenticationFailed);
        }
        source
            .seek(SeekFrom::Start(0))
            .map_err(|source| CoreError::Io {
                operation,
                path: source_path.to_path_buf(),
                source,
            })?;
        #[cfg(unix)]
        {
            use rustix::fs::{AtFlags, Mode, OFlags, fsync, linkat, openat, unlinkat};

            let temporary_name = format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple());
            let descriptor = openat(
                &destination.descriptor,
                temporary_name.as_str(),
                OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|error| {
                anchored_io_error(operation, &destination.path.join(&temporary_name), error)
            })?;
            let mut temporary = fs::File::from(descriptor);
            let copy_result = copy_file_exact_with_digest(
                source,
                &mut temporary,
                expected_bytes,
                expected_digest,
                source_path,
                operation,
            )
            .and_then(|()| {
                temporary.flush().map_err(|source| CoreError::Io {
                    operation,
                    path: destination.path.join(&temporary_name),
                    source,
                })?;
                temporary.sync_all().map_err(|source| CoreError::Io {
                    operation,
                    path: destination.path.join(&temporary_name),
                    source,
                })
            });
            drop(temporary);
            if let Err(error) = copy_result {
                let _ = unlinkat(
                    &destination.descriptor,
                    temporary_name.as_str(),
                    AtFlags::empty(),
                );
                return Err(error);
            }
            let created = match linkat(
                &destination.descriptor,
                temporary_name.as_str(),
                &destination.descriptor,
                name,
                AtFlags::empty(),
            ) {
                Ok(()) => true,
                Err(rustix::io::Errno::EXIST) => false,
                Err(error) => {
                    let _ = unlinkat(
                        &destination.descriptor,
                        temporary_name.as_str(),
                        AtFlags::empty(),
                    );
                    return Err(anchored_io_error(
                        operation,
                        &destination.path.join(name),
                        error,
                    ));
                }
            };
            unlinkat(
                &destination.descriptor,
                temporary_name.as_str(),
                AtFlags::empty(),
            )
            .map_err(|error| {
                anchored_io_error(operation, &destination.path.join(&temporary_name), error)
            })?;
            fsync(&destination.descriptor)
                .map_err(|error| anchored_io_error(operation, &destination.path, error))?;
            Ok(created)
        }

        #[cfg(not(unix))]
        {
            let temporary_path = destination
                .path
                .join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
            let mut temporary = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)
                .map_err(|source| CoreError::Io {
                    operation,
                    path: temporary_path.clone(),
                    source,
                })?;
            copy_file_exact_with_digest(
                source,
                &mut temporary,
                expected_bytes,
                expected_digest,
                source_path,
                operation,
            )?;
            temporary.flush().map_err(|source| CoreError::Io {
                operation,
                path: temporary_path.clone(),
                source,
            })?;
            temporary.sync_all().map_err(|source| CoreError::Io {
                operation,
                path: temporary_path.clone(),
                source,
            })?;
            drop(temporary);
            let destination_path = destination.path.join(name);
            let created = match fs::hard_link(&temporary_path, &destination_path) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
                Err(source) => {
                    let _ = fs::remove_file(&temporary_path);
                    return Err(CoreError::Io {
                        operation,
                        path: destination_path,
                        source,
                    });
                }
            };
            fs::remove_file(&temporary_path).map_err(|source| CoreError::Io {
                operation,
                path: temporary_path,
                source,
            })?;
            sync_directory(&destination.path)?;
            Ok(created)
        }
    }

    /// Returns current physical capacity and durable active reservations.
    pub fn provider_capacity(&self, now_unix_ms: u64) -> Result<ProviderCapacity, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.provider_capacity_locked(now_unix_ms, None, None)
    }

    /// Cancels and durably compacts one exact provider lease. Repeating a
    /// cancellation after compaction is harmless because the authenticated
    /// caller already supplies the full signed lease identity.
    pub fn cancel_provider_lease(
        &self,
        lease: &StorageLease,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.recover_provider_upload_journals_locked()?;
        let path = self.provider_lease_path(lease)?;
        let mut state = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.cleanup_recovery_capsule_upload_locked(lease)?;
                self.compact_provider_leases_locked(now_unix_ms)?;
                return Ok(());
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect cancelled provider lease",
                    path,
                    source,
                });
            }
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                serde_json::from_slice::<ProviderLeaseState>(&read_bounded(
                    &path,
                    MAX_PROVIDER_LEASE_STATE_BYTES,
                )?)?
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
        };
        if state.schema_version != PROVIDER_LEASE_SCHEMA_VERSION || state.lease != *lease {
            return Err(CoreError::AuthenticationFailed);
        }
        if !state.cancelled {
            state.cancelled = true;
            self.persist_provider_lease_locked(&state)?;
        }
        self.discard_recovery_capsule_staging_locked(&mut state, now_unix_ms)?;
        self.compact_provider_leases_locked(now_unix_ms)
    }

    /// Removes one terminal segmented-upload receipt only after the authenticated
    /// writer explicitly confirms that it received the commit result. Missing
    /// receipts are idempotent; a mismatched durable receipt fails closed.
    pub fn acknowledge_recovery_capsule_upload(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let Some(receipt) = self.load_provider_upload_receipt_locked(lease, upload_id)? else {
            return Ok(());
        };
        let root = self.root.join("provider-upload-receipts");
        let scoped_path = self.provider_upload_receipt_path(lease, upload_id)?;
        let legacy_path = self.legacy_provider_upload_receipt_path(lease, upload_id)?;
        let mut synchronized = BTreeSet::new();
        for path in [scoped_path, legacy_path] {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    let incumbent: ProviderUploadReceipt =
                        serde_json::from_slice(&read_private_regular_file_bounded(
                            &path,
                            MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                            "read acknowledged provider upload receipt",
                        )?)?;
                    if incumbent != receipt {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    fs::remove_file(&path).map_err(|source| CoreError::Io {
                        operation: "acknowledge provider upload receipt",
                        path: path.clone(),
                        source,
                    })?;
                    synchronized.insert(
                        path.parent()
                            .ok_or_else(|| {
                                CoreError::InvalidState(
                                    "provider receipt path has no parent".to_owned(),
                                )
                            })?
                            .to_path_buf(),
                    );
                }
                Ok(_) => return Err(CoreError::AuthenticationFailed),
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "inspect acknowledged provider upload receipt",
                        path,
                        source,
                    });
                }
            }
        }
        for directory in synchronized {
            sync_directory(&directory)?;
            if directory != root {
                remove_empty_directory(&directory, &root)?;
            }
        }
        sync_directory(&root)
    }

    /// Fails before acquiring a remote lease when the private owner-side attempt
    /// journal cannot admit another independently recoverable upload.
    pub fn ensure_recovery_capsule_upload_attempt_capacity(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let attempt_name = self.recovery_capsule_upload_attempt_name(
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
        )?;
        let intent_name = self.recovery_capsule_lease_intent_name(
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
        )?;
        let attempts_directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let intents_directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                false,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let attempts = self.anchored_directory_entries_bounded(
            &attempts_directory,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
            "recovery capsule upload attempts",
        )?;
        let intents = self.anchored_directory_entries_bounded(
            &intents_directory,
            MAX_RECOVERY_CAPSULE_LEASE_INTENTS,
            "recovery capsule lease intents",
        )?;
        if attempts.iter().any(|entry| entry == attempt_name.as_str())
            || intents.iter().any(|entry| entry == intent_name.as_str())
            || attempts.len().saturating_add(intents.len()) < MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS
        {
            Ok(())
        } else {
            Err(CoreError::ResourceLimit(
                "recovery capsule upload attempt backpressure",
            ))
        }
    }

    /// Loads one exact private owner-side upload attempt without following links.
    pub fn load_recovery_capsule_upload_attempt(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<Option<RecoveryCapsuleUploadAttempt>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self.recovery_capsule_upload_attempt_name(
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
        )?;
        let directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES as u64,
            "read recovery capsule upload attempt",
        )?
        else {
            return Ok(None);
        };
        let attempt: RecoveryCapsuleUploadAttempt = serde_json::from_slice(&bytes)?;
        if !valid_recovery_capsule_upload_attempt(&attempt)
            || attempt.provider_device_id != provider_device_id
            || attempt.backup_id != backup_id
            || attempt.snapshot_id != snapshot_id
            || attempt.capsule_digest != capsule_digest
            || name
                != self
                    .recovery_capsule_upload_attempt_path_for_value(&attempt)?
                    .file_name()
                    .and_then(|value| value.to_str())
                    .ok_or(CoreError::AuthenticationFailed)?
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(attempt))
    }

    /// Lists a provider's bounded private attempts for restart reconciliation.
    pub fn recovery_capsule_upload_attempts_for_provider(
        &self,
        provider_device_id: DeviceId,
    ) -> Result<Vec<RecoveryCapsuleUploadAttempt>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut attempts = Vec::new();
        for name in self.anchored_directory_entries_bounded(
            &directory,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
            "recovery capsule upload attempts",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &directory,
                    name,
                    MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES as u64,
                    "read recovery capsule upload attempt",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let attempt: RecoveryCapsuleUploadAttempt = serde_json::from_slice(&bytes)?;
            if !valid_recovery_capsule_upload_attempt(&attempt)
                || self
                    .recovery_capsule_upload_attempt_path_for_value(&attempt)?
                    .file_name()
                    .and_then(|value| value.to_str())
                    != Some(name)
            {
                return Err(CoreError::AuthenticationFailed);
            }
            if attempt.provider_device_id == provider_device_id {
                attempts.push(attempt);
            }
        }
        Ok(attempts)
    }

    /// Persists a monotonic phase transition before the corresponding network action.
    pub fn persist_recovery_capsule_upload_attempt(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        if !valid_recovery_capsule_upload_attempt(attempt) {
            return Err(CoreError::AuthenticationFailed);
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self
            .recovery_capsule_upload_attempt_path_for_value(attempt)?
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?
            .to_owned();
        let directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                true,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let incumbent = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES as u64,
            "read recovery capsule upload attempt",
        )?;
        match incumbent {
            Some(bytes) => {
                let incumbent: RecoveryCapsuleUploadAttempt = serde_json::from_slice(&bytes)?;
                if !valid_recovery_capsule_upload_attempt(&incumbent) {
                    return Err(CoreError::AuthenticationFailed);
                }
                if !valid_recovery_capsule_upload_attempt_transition(&incumbent, attempt) {
                    return Err(CoreError::AuthenticationFailed);
                }
            }
            None => {
                let entries = self.anchored_directory_entries_bounded(
                    &directory,
                    MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
                    "recovery capsule upload attempts",
                )?;
                if entries.len() >= MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS {
                    return Err(CoreError::ResourceLimit(
                        "recovery capsule upload attempt backpressure",
                    ));
                }
            }
        }
        self.write_anchored_atomic(
            &directory,
            &name,
            &serde_json::to_vec_pretty(attempt)?,
            false,
            "persist recovery capsule upload attempt",
        )?;
        Ok(())
    }

    /// Clears only the exact phase the authenticated client has reconciled.
    pub fn complete_recovery_capsule_upload_attempt(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self
            .recovery_capsule_upload_attempt_path_for_value(attempt)?
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?
            .to_owned();
        let directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES as u64,
            "read completed recovery capsule upload attempt",
        )?
        else {
            return Ok(());
        };
        let incumbent: RecoveryCapsuleUploadAttempt = serde_json::from_slice(&bytes)?;
        if incumbent != *attempt {
            return Err(CoreError::AuthenticationFailed);
        }
        self.remove_anchored_file(
            &directory,
            &name,
            "complete recovery capsule upload attempt",
        )?;
        Ok(())
    }

    /// Loads one exact acquisition intent persisted before a provider lease request.
    pub fn load_recovery_capsule_lease_intent(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<Option<RecoveryCapsuleLeaseIntent>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self.recovery_capsule_lease_intent_name(
            provider_device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
        )?;
        let directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                false,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES as u64,
            "read recovery capsule lease intent",
        )?
        else {
            return Ok(None);
        };
        let intent: RecoveryCapsuleLeaseIntent = serde_json::from_slice(&bytes)?;
        if !valid_recovery_capsule_lease_intent(&intent)
            || intent.provider_device_id != provider_device_id
            || intent.backup_id != backup_id
            || intent.snapshot_id != snapshot_id
            || intent.capsule_digest != capsule_digest
            || self
                .recovery_capsule_lease_intent_path_for_value(&intent)?
                .file_name()
                .and_then(|value| value.to_str())
                != Some(name.as_str())
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(intent))
    }

    /// Lists a provider's bounded pre-lease intents for restart reconciliation.
    pub fn recovery_capsule_lease_intents_for_provider(
        &self,
        provider_device_id: DeviceId,
    ) -> Result<Vec<RecoveryCapsuleLeaseIntent>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                false,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut intents = Vec::new();
        for name in self.anchored_directory_entries_bounded(
            &directory,
            MAX_RECOVERY_CAPSULE_LEASE_INTENTS,
            "recovery capsule lease intents",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &directory,
                    name,
                    MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES as u64,
                    "read recovery capsule lease intent",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let intent: RecoveryCapsuleLeaseIntent = serde_json::from_slice(&bytes)?;
            if !valid_recovery_capsule_lease_intent(&intent)
                || self
                    .recovery_capsule_lease_intent_path_for_value(&intent)?
                    .file_name()
                    .and_then(|value| value.to_str())
                    != Some(name)
            {
                return Err(CoreError::AuthenticationFailed);
            }
            if intent.provider_device_id == provider_device_id {
                intents.push(intent);
            }
        }
        Ok(intents)
    }

    /// Publishes an immutable exact lease request before any remote reservation exists.
    pub fn persist_recovery_capsule_lease_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<(), CoreError> {
        if !valid_recovery_capsule_lease_intent(intent) {
            return Err(CoreError::AuthenticationFailed);
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self
            .recovery_capsule_lease_intent_path_for_value(intent)?
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?
            .to_owned();
        let directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                true,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        match self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES as u64,
            "read recovery capsule lease intent",
        )? {
            Some(bytes) => {
                let incumbent: RecoveryCapsuleLeaseIntent = serde_json::from_slice(&bytes)?;
                if !valid_recovery_capsule_lease_intent(&incumbent) {
                    return Err(CoreError::AuthenticationFailed);
                }
                if incumbent == *intent {
                    return Ok(());
                }
                return Err(CoreError::AuthenticationFailed);
            }
            None => {
                let attempts_directory = self
                    .anchored_directory(
                        "recovery-upload-attempts",
                        &[],
                        false,
                        "open recovery capsule upload attempts",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?;
                let attempts = self.anchored_directory_entries_bounded(
                    &attempts_directory,
                    MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
                    "recovery capsule upload attempts",
                )?;
                let intents = self.anchored_directory_entries_bounded(
                    &directory,
                    MAX_RECOVERY_CAPSULE_LEASE_INTENTS,
                    "recovery capsule lease intents",
                )?;
                if attempts.len().saturating_add(intents.len())
                    >= MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS
                {
                    return Err(CoreError::ResourceLimit(
                        "recovery capsule upload attempt backpressure",
                    ));
                }
            }
        }
        if !self.write_anchored_atomic(
            &directory,
            &name,
            &serde_json::to_vec_pretty(intent)?,
            true,
            "persist recovery capsule lease intent",
        )? {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }

    /// Removes only the exact durable intent after its lease is journaled or cancelled.
    pub fn complete_recovery_capsule_lease_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = self
            .recovery_capsule_lease_intent_path_for_value(intent)?
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?
            .to_owned();
        let directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                false,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES as u64,
            "read completed recovery capsule lease intent",
        )?
        else {
            return Ok(());
        };
        let incumbent: RecoveryCapsuleLeaseIntent = serde_json::from_slice(&bytes)?;
        if incumbent != *intent {
            return Err(CoreError::AuthenticationFailed);
        }
        self.remove_anchored_file(&directory, &name, "complete recovery capsule lease intent")?;
        Ok(())
    }

    /// Loads one exact ordinary provider-write reservation retained across restart.
    pub fn load_provider_write_lease_intent(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
    ) -> Result<Option<ProviderWriteLeaseIntent>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = provider_write_lease_intent_name(provider_device_id, backup_id);
        let directory = self
            .anchored_directory(
                "provider-write-intents",
                &[],
                false,
                "open provider write lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES as u64,
            "read provider write lease intent",
        )?
        else {
            return Ok(None);
        };
        let intent: ProviderWriteLeaseIntent = serde_json::from_slice(&bytes)?;
        if !valid_provider_write_lease_intent(&intent)
            || intent.provider_device_id != provider_device_id
            || intent.backup_id != backup_id
            || provider_write_lease_intent_name(intent.provider_device_id, intent.backup_id) != name
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(intent))
    }

    /// Lists a provider's bounded ordinary write reservations for restart cleanup.
    pub fn provider_write_lease_intents_for_provider(
        &self,
        provider_device_id: DeviceId,
    ) -> Result<Vec<ProviderWriteLeaseIntent>, CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let directory = self
            .anchored_directory(
                "provider-write-intents",
                &[],
                false,
                "open provider write lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut intents = Vec::new();
        for name in self.anchored_directory_entries_bounded(
            &directory,
            MAX_PROVIDER_WRITE_LEASE_INTENTS,
            "provider write lease intents",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &directory,
                    name,
                    MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES as u64,
                    "read provider write lease intent",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let intent: ProviderWriteLeaseIntent = serde_json::from_slice(&bytes)?;
            if !valid_provider_write_lease_intent(&intent)
                || provider_write_lease_intent_name(intent.provider_device_id, intent.backup_id)
                    != name
            {
                return Err(CoreError::AuthenticationFailed);
            }
            if intent.provider_device_id == provider_device_id {
                intents.push(intent);
            }
        }
        Ok(intents)
    }

    /// Persists one acquisition identity before the provider may reserve quota.
    pub fn persist_provider_write_lease_intent(
        &self,
        intent: &ProviderWriteLeaseIntent,
    ) -> Result<(), CoreError> {
        if !valid_provider_write_lease_intent(intent) {
            return Err(CoreError::AuthenticationFailed);
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = provider_write_lease_intent_name(intent.provider_device_id, intent.backup_id);
        let directory = self
            .anchored_directory(
                "provider-write-intents",
                &[],
                true,
                "open provider write lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        match self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES as u64,
            "read provider write lease intent",
        )? {
            Some(bytes) => {
                let incumbent: ProviderWriteLeaseIntent = serde_json::from_slice(&bytes)?;
                if incumbent == *intent && valid_provider_write_lease_intent(&incumbent) {
                    return Ok(());
                }
                return Err(CoreError::AuthenticationFailed);
            }
            None => {
                if self
                    .anchored_directory_entries_bounded(
                        &directory,
                        MAX_PROVIDER_WRITE_LEASE_INTENTS,
                        "provider write lease intents",
                    )?
                    .len()
                    >= MAX_PROVIDER_WRITE_LEASE_INTENTS
                {
                    return Err(CoreError::ResourceLimit(
                        "provider write lease intent backpressure",
                    ));
                }
            }
        }
        if !self.write_anchored_atomic(
            &directory,
            &name,
            &serde_json::to_vec_pretty(intent)?,
            true,
            "persist provider write lease intent",
        )? {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }

    /// Clears only an exact intent after its lease has been durably cancelled.
    pub fn complete_provider_write_lease_intent(
        &self,
        intent: &ProviderWriteLeaseIntent,
    ) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let name = provider_write_lease_intent_name(intent.provider_device_id, intent.backup_id);
        let directory = self
            .anchored_directory(
                "provider-write-intents",
                &[],
                false,
                "open provider write lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let Some(bytes) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES as u64,
            "read completed provider write lease intent",
        )?
        else {
            return Ok(());
        };
        let incumbent: ProviderWriteLeaseIntent = serde_json::from_slice(&bytes)?;
        if incumbent != *intent || !valid_provider_write_lease_intent(&incumbent) {
            return Err(CoreError::AuthenticationFailed);
        }
        self.remove_anchored_file(&directory, &name, "complete provider write lease intent")?;
        Ok(())
    }

    /// Persists one signed provider-issued reservation before any remote bytes are accepted.
    pub fn reserve_provider_lease(&self, lease: &StorageLease) -> Result<(), CoreError> {
        if !valid_provider_lease_shape(lease, self.provider_quota_policy.maximum_lease_lifetime_ms)
        {
            return Err(CoreError::InvalidState("invalid storage lease".to_owned()));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
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
        self.reserve_new_provider_lease_locked(lease, &path)
    }

    /// Reserves or returns the exact incumbent for one deterministic acquisition id.
    pub fn reserve_provider_lease_idempotent(
        &self,
        lease: &StorageLease,
    ) -> Result<StorageLease, CoreError> {
        if !valid_provider_lease_shape(lease, self.provider_quota_policy.maximum_lease_lifetime_ms)
        {
            return Err(CoreError::InvalidState("invalid storage lease".to_owned()));
        }
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let path = self.provider_lease_path(lease)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                let existing: ProviderLeaseState =
                    serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
                let incumbent = &existing.lease;
                if !valid_provider_lease_shape(
                    incumbent,
                    self.provider_quota_policy.maximum_lease_lifetime_ms,
                ) || incumbent.schema_version != lease.schema_version
                    || incumbent.lease_id != lease.lease_id
                    || incumbent.peer_device_id != lease.peer_device_id
                    || incumbent.provider_device_id != lease.provider_device_id
                    || incumbent.backup_id != lease.backup_id
                    || incumbent.max_new_bytes != lease.max_new_bytes
                    || incumbent.max_new_objects != lease.max_new_objects
                    || incumbent.expires_at_unix_ms - incumbent.issued_at_unix_ms
                        != lease.expires_at_unix_ms - lease.issued_at_unix_ms
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                Ok(incumbent.clone())
            }
            Ok(_) => Err(CoreError::AuthenticationFailed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.reserve_new_provider_lease_locked(lease, &path)?;
                Ok(lease.clone())
            }
            Err(source) => Err(CoreError::Io {
                operation: "inspect idempotent provider lease",
                path,
                source,
            }),
        }
    }

    fn reserve_new_provider_lease_locked(
        &self,
        lease: &StorageLease,
        path: &Path,
    ) -> Result<(), CoreError> {
        self.compact_provider_leases_locked(lease.issued_at_unix_ms)?;
        self.ensure_provider_lease_admission_locked(lease.issued_at_unix_ms, lease.peer_device_id)?;
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
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("provider lease path has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        write_json_atomic(
            path,
            &ProviderLeaseState {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease: lease.clone(),
                consumed_new_bytes: 0,
                consumed_new_objects: 0,
                objects: std::collections::BTreeMap::new(),
                deferred_reference_sync: BTreeSet::new(),
                staged_capsule_upload: None,
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

    /// Atomically validates and accounts for one bounded batch of immutable
    /// remote chunk writes under the exact persisted backup lease.
    pub fn put_provider_records_leased<R: AsRef<[u8]>>(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        records: &[(String, R)],
        now_unix_ms: u64,
    ) -> Result<Vec<bool>, CoreError> {
        if records.is_empty() || records.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS {
            return Err(CoreError::ResourceLimit("provider write batch"));
        }
        let records = records
            .iter()
            .map(|(locator, record)| (locator.clone(), record.as_ref()))
            .collect::<Vec<_>>();
        let mut total_bytes = 0_usize;
        let mut unique = BTreeSet::new();
        for (locator, record) in &records {
            validate_hex_locator(locator)?;
            validate_record_bounds(record, self.maximum_chunk_size)?;
            if !unique.insert(locator) {
                return Err(CoreError::AuthenticationFailed);
            }
            total_bytes = total_bytes
                .checked_add(record.len())
                .ok_or(CoreError::ResourceLimit("provider write batch"))?;
        }
        if total_bytes > MAX_PROVIDER_WRITE_BATCH_BYTES {
            return Err(CoreError::ResourceLimit("provider write batch"));
        }

        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let mut created = vec![false; records.len()];
        let mut journals = Vec::new();
        let mut new_bytes = 0_u64;
        let mut new_objects = 0_u64;

        for (index, (locator, record)) in records.iter().enumerate() {
            let object_key = format!("chunk:{locator}");
            let path = self.chunk_path(locator)?;
            if let Some(length) = state.objects.get(&object_key) {
                if *length != record.len() as u64
                    || read_bounded(&path, self.maximum_chunk_size + provider_record_overhead())?
                        != *record
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                continue;
            }
            let parent = path
                .parent()
                .ok_or_else(|| CoreError::InvalidState("chunk path has no parent".to_owned()))?;
            ensure_private_directory(parent)?;
            let expected_new_object = match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink()
                        || !metadata.is_file()
                        || read_bounded(
                            &path,
                            self.maximum_chunk_size + provider_record_overhead(),
                        )? != *record
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    new_bytes = new_bytes
                        .checked_add(record.len() as u64)
                        .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
                    new_objects = new_objects
                        .checked_add(1)
                        .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
                    true
                }
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "inspect leased provider chunk batch",
                        path,
                        source,
                    });
                }
            };
            created[index] = expected_new_object;
            journals.push((
                index,
                ProviderUploadJournal {
                    schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                    journal_id: uuid::Uuid::new_v4().to_string(),
                    lease: lease.clone(),
                    object_key,
                    object: ProviderUploadKind::Chunk {
                        locator: locator.clone(),
                    },
                    record_bytes: record.len() as u64,
                    record_digest: blake3::hash(record).to_hex().to_string(),
                    recovery_capsule_descriptor: None,
                    expected_new_object,
                    deferred_reference_candidate: expected_new_object
                        && matches!(
                            fs::symlink_metadata(self.provider_object_reference_path(locator)?),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound
                        ),
                    started_at_unix_ms: now_unix_ms,
                },
            ));
        }
        self.ensure_lease_consumption_locked(&state, new_bytes, new_objects)?;
        if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
            operation: "inspect provider free space",
            path: self.root.clone(),
            source,
        })? < new_bytes.saturating_add(self.provider_quota_policy.free_space_reserve_bytes)
        {
            return Err(CoreError::ResourceLimit("provider free-space reserve"));
        }
        if journals.is_empty() {
            return Ok(created);
        }

        let batch = ProviderUploadBatchJournal {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            journal_id: uuid::Uuid::new_v4().to_string(),
            uploads: journals
                .iter()
                .map(|(_, journal)| journal.clone())
                .collect(),
        };
        write_json_atomic(
            &self.provider_upload_batch_journal_path(&batch),
            &batch,
            true,
        )?;

        let write_error = Mutex::new(None);
        std::thread::scope(|scope| {
            for (index, journal) in &journals {
                let (locator, record) = &records[*index];
                let write_error = &write_error;
                scope.spawn(move || {
                    if !journal.expected_new_object {
                        return;
                    }
                    let result = self.chunk_path(locator).and_then(|path| {
                        let committed = write_atomic_noclobber(&path, record, false)?;
                        if committed
                            || read_bounded(
                                &path,
                                self.maximum_chunk_size + provider_record_overhead(),
                            )? == *record
                        {
                            Ok(())
                        } else {
                            Err(CoreError::AuthenticationFailed)
                        }
                    });
                    if let Err(error) = result
                        && let Ok(mut first) = write_error.lock()
                        && first.is_none()
                    {
                        *first = Some(error);
                    }
                });
            }
        });
        if let Some(error) = write_error
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?
        {
            self.reconcile_provider_upload_batch_locked(&batch)?;
            return Err(error);
        }

        let reference_error = Mutex::new(None);
        let deferred_references = Mutex::new(BTreeSet::new());
        std::thread::scope(|scope| {
            for (index, journal) in &journals {
                let (locator, record) = &records[*index];
                let reference_error = &reference_error;
                let deferred_references = &deferred_references;
                scope.spawn(move || {
                    let result = if journal.deferred_reference_candidate {
                        self.add_provider_object_reference_deferred_locked(
                            locator,
                            record.len() as u64,
                            peer_device_id,
                            backup_id,
                        )
                    } else {
                        self.add_provider_object_reference_locked(
                            locator,
                            record.len() as u64,
                            peer_device_id,
                            backup_id,
                        )
                        .map(|()| false)
                    };
                    match result {
                        Ok(true) => {
                            if let Ok(mut values) = deferred_references.lock() {
                                values.insert(locator.clone());
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            if let Ok(mut first) = reference_error.lock()
                                && first.is_none()
                            {
                                *first = Some(error);
                            }
                        }
                    }
                });
            }
        });
        if let Some(error) = reference_error
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?
        {
            self.reconcile_provider_upload_batch_locked(&batch)?;
            return Err(error);
        }

        state.consumed_new_bytes = state
            .consumed_new_bytes
            .checked_add(new_bytes)
            .ok_or(CoreError::ResourceLimit("provider lease bytes"))?;
        state.consumed_new_objects = state
            .consumed_new_objects
            .checked_add(new_objects)
            .ok_or(CoreError::ResourceLimit("provider lease objects"))?;
        for (_, journal) in &journals {
            state
                .objects
                .insert(journal.object_key.clone(), journal.record_bytes);
        }
        state.deferred_reference_sync.extend(
            deferred_references
                .into_inner()
                .map_err(|_| CoreError::Synchronization)?,
        );
        self.persist_provider_lease_locked(&state)?;
        self.complete_provider_upload_batch_journal_locked(&batch)?;
        Ok(created)
    }

    /// Deduplicates and durably writes one encrypted chunk.
    ///
    /// Returns `true` when a new record was committed and `false` when an exact
    /// durable copy already existed.
    pub fn put(&self, chunk: &EncryptedChunk) -> Result<bool, CoreError> {
        self.put_provider_record(&chunk.opaque_locator, &chunk.encode_provider_record())
    }

    /// Commits one bounded backup-only batch while preserving the ordinary
    /// single-record `put` durability boundary for every other caller.
    pub(crate) fn put_backup_batch(
        &self,
        chunks: &[&EncryptedChunk],
    ) -> Result<Vec<bool>, CoreError> {
        if chunks.is_empty() || chunks.len() > MAX_LOCAL_WRITE_BATCH_RECORDS {
            return Err(CoreError::ResourceLimit("local write batch"));
        }
        let mut unique = BTreeMap::<String, (Vec<u8>, Vec<usize>)>::new();
        let mut total_bytes = 0_usize;
        for (index, chunk) in chunks.iter().enumerate() {
            validate_hex_locator(&chunk.opaque_locator)?;
            let record = chunk.encode_provider_record();
            validate_record_bounds(&record, self.maximum_chunk_size)?;
            match unique.entry(chunk.opaque_locator.clone()) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    total_bytes = total_bytes
                        .checked_add(record.len())
                        .ok_or(CoreError::ResourceLimit("local write batch"))?;
                    entry.insert((record, vec![index]));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if entry.get().0 != record {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    entry.get_mut().1.push(index);
                }
            }
        }
        if total_bytes > MAX_LOCAL_WRITE_BATCH_BYTES {
            return Err(CoreError::ResourceLimit("local write batch"));
        }

        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let mut results = vec![false; chunks.len()];
        let mut pending = Vec::new();
        for (locator, (record, indices)) in &unique {
            let path = self.chunk_path(locator)?;
            let parent = path
                .parent()
                .ok_or_else(|| CoreError::InvalidState("chunk path has no parent".to_owned()))?;
            ensure_private_directory(parent)?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink()
                        || !metadata.is_file()
                        || read_bounded(
                            &path,
                            self.maximum_chunk_size + provider_record_overhead(),
                        )? != *record
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    pending.push((locator.clone(), record, indices[0]));
                }
                Err(source) => {
                    return Err(CoreError::Io {
                        operation: "inspect local write batch chunk",
                        path,
                        source,
                    });
                }
            }
        }
        if pending.is_empty() {
            return Ok(results);
        }
        let journal = LocalWriteBatchJournal {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            journal_id: uuid::Uuid::new_v4().to_string(),
            entries: pending
                .iter()
                .map(|(locator, record, _)| LocalWriteBatchEntry {
                    locator: locator.clone(),
                    record_bytes: record.len() as u64,
                    record_digest: blake3::hash(record).to_hex().to_string(),
                })
                .collect(),
        };
        write_json_atomic(
            &self.local_write_batch_journal_path(&journal),
            &journal,
            true,
        )?;
        local_write_batch_failpoint(1)?;

        let writes = Mutex::new(BTreeMap::<String, bool>::new());
        let write_error = Mutex::new(None);
        std::thread::scope(|scope| {
            for (locator, record, _) in &pending {
                let writes = &writes;
                let write_error = &write_error;
                scope.spawn(move || {
                    let result = self.chunk_path(locator).and_then(|path| {
                        let created = write_atomic_noclobber(&path, record, false)?;
                        if !created
                            && read_bounded(
                                &path,
                                self.maximum_chunk_size + provider_record_overhead(),
                            )? != **record
                        {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        Ok(created)
                    });
                    match result {
                        Ok(created) => {
                            if let Ok(mut values) = writes.lock() {
                                values.insert(locator.clone(), created);
                            }
                        }
                        Err(error) => {
                            if let Ok(mut first) = write_error.lock()
                                && first.is_none()
                            {
                                *first = Some(error);
                            }
                        }
                    }
                });
            }
        });
        if let Some(error) = write_error
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?
        {
            self.reconcile_local_write_batch_locked(&journal)?;
            return Err(error);
        }
        local_write_batch_failpoint(2)?;
        let writes = writes
            .into_inner()
            .map_err(|_| CoreError::Synchronization)?;
        for (locator, _, first_index) in &pending {
            results[*first_index] = writes.get(locator).copied().unwrap_or(false);
        }
        self.complete_local_write_batch_journal_locked(&journal)?;
        Ok(results)
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
            let reference: ProviderObjectReference =
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)
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
        let directory = self
            .recovery_capsule_directory(
                capsule.signer_device_id,
                capsule.backup_id,
                true,
                "open recovery capsule directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let name = format!("{}.json", capsule.snapshot_id);
        if self.write_anchored_atomic(
            &directory,
            &name,
            &bytes,
            true,
            "persist immutable recovery capsule",
        )? {
            return Ok(true);
        }
        let existing = self
            .read_anchored_private_file_bounded(
                &directory,
                &name,
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                "read immutable recovery capsule",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
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
        if capsule.backup_id != backup_id || capsule.signer_device_id != peer_device_id {
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
        let object_key =
            provider_capsule_object_key(peer_device_id, backup_id, &capsule.snapshot_id);
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let directory = self
            .recovery_capsule_directory(
                peer_device_id,
                backup_id,
                true,
                "open leased recovery capsule directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let name = format!("{}.json", capsule.snapshot_id);
        if let Some(length) = state.objects.get(&object_key) {
            if *length != bytes.len() as u64 {
                return Err(CoreError::AuthenticationFailed);
            }
            return if self
                .read_anchored_private_file_bounded(
                    &directory,
                    &name,
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "read leased recovery capsule",
                )?
                .is_some_and(|incumbent| incumbent == bytes)
            {
                Ok(false)
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        }
        let expected_new_object = match self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_BYTES as u64,
            "read leased recovery capsule incumbent",
        )? {
            Some(incumbent) => {
                if incumbent != bytes {
                    return Err(CoreError::AuthenticationFailed);
                }
                false
            }
            None => {
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
        let created = expected_new_object
            && self.write_anchored_atomic(
                &directory,
                &name,
                &bytes,
                true,
                "persist leased recovery capsule",
            )?;
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
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let requested = RecoveryCapsuleUpload {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            upload_id: upload_id.to_owned(),
            lease: lease.clone(),
            total_bytes,
            total_segments,
            capsule_digest: capsule_digest.to_owned(),
            descriptor: Some(descriptor.clone()),
            created_at_unix_ms: now_unix_ms,
        };
        if let Some(staged) = &state.staged_capsule_upload {
            if !same_recovery_capsule_upload_request(&staged.upload, &requested) {
                return Err(CoreError::AuthenticationFailed);
            }
            return self.ensure_recovery_capsule_upload_directory_locked(&staged.upload);
        }

        self.ensure_provider_upload_receipt_capacity_locked(peer_device_id)?;
        self.ensure_lease_consumption_locked(&state, total_bytes, 1)?;
        let required_peak_bytes = total_bytes
            .checked_mul(2)
            .and_then(|value| {
                value.checked_add(self.provider_quota_policy.free_space_reserve_bytes)
            })
            .ok_or(CoreError::ResourceLimit("recovery capsule staging"))?;
        if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
            operation: "inspect recovery capsule staging free space",
            path: self.root.clone(),
            source,
        })? < required_peak_bytes
        {
            return Err(CoreError::ResourceLimit("provider free-space reserve"));
        }

        // An older interrupted upload has no durable reservation and therefore cannot be
        // resumed safely. Remove it before publishing the exact new reservation.
        self.cleanup_recovery_capsule_upload_locked(lease)?;
        let object_key =
            provider_capsule_object_key(peer_device_id, backup_id, &descriptor.snapshot_id);
        let capsule_directory = self
            .recovery_capsule_directory(
                peer_device_id,
                backup_id,
                true,
                "open staged recovery capsule destination",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let capsule_name = format!("{}.json", descriptor.snapshot_id);
        let expected_new_object = if let Some(length) = state.objects.get(&object_key) {
            if *length != total_bytes
                || self.hash_anchored_private_file_bounded(
                    &capsule_directory,
                    &capsule_name,
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "verify staged recovery capsule incumbent",
                )? != Some((total_bytes, capsule_digest.to_owned()))
            {
                return Err(CoreError::AuthenticationFailed);
            }
            false
        } else {
            match self.hash_anchored_private_file_bounded(
                &capsule_directory,
                &capsule_name,
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                "verify staged recovery capsule incumbent",
            )? {
                Some(incumbent) => {
                    if incumbent != (total_bytes, capsule_digest.to_owned()) {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    false
                }
                None => true,
            }
        };
        state.staged_capsule_upload = Some(StagedCapsuleUpload {
            upload: requested,
            expected_new_object,
            committed_created: None,
            completed_at_unix_ms: None,
        });
        self.persist_provider_lease_locked(&state)?;
        self.ensure_recovery_capsule_upload_directory_locked(
            &state
                .staged_capsule_upload
                .as_ref()
                .ok_or(CoreError::AuthenticationFailed)?
                .upload,
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
        let state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let metadata = self.load_recovery_capsule_upload_locked(lease, upload_id)?;
        let staged = state
            .staged_capsule_upload
            .as_ref()
            .ok_or(CoreError::AuthenticationFailed)?;
        let segment_offset = u64::from(index)
            .checked_mul(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64)
            .ok_or(CoreError::ResourceLimit("recovery capsule segment"))?;
        let expected_length = metadata
            .total_bytes
            .saturating_sub(segment_offset)
            .min(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64);
        if metadata.lease != *lease
            || staged.upload != metadata
            || staged.committed_created.is_some()
            || index >= metadata.total_segments
            || expected_length == 0
            || segment.len() as u64 != expected_length
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let directory = self
            .recovery_capsule_segments_directory(lease, false, "open recovery capsule segments")?
            .ok_or(CoreError::AuthenticationFailed)?;
        let name = format!("{index:08}.bin");
        if let Some(incumbent) = self.read_anchored_private_file_bounded(
            &directory,
            &name,
            MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64,
            "read duplicate recovery capsule segment",
        )? {
            return if incumbent == segment {
                Ok(())
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        }
        let required_remaining = metadata
            .total_bytes
            .checked_add(segment.len() as u64)
            .and_then(|value| {
                value.checked_add(self.provider_quota_policy.free_space_reserve_bytes)
            })
            .ok_or(CoreError::ResourceLimit("recovery capsule staging"))?;
        if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
            operation: "inspect recovery capsule segment free space",
            path: self.root.clone(),
            source,
        })? < required_remaining
        {
            return Err(CoreError::ResourceLimit("provider free-space reserve"));
        }
        if self.write_anchored_atomic(
            &directory,
            &name,
            segment,
            true,
            "persist recovery capsule segment",
        )? {
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
        // A prior commit may have made the final capsule durable before its
        // lease accounting and receipt were persisted. Reconcile that truth
        // before classifying an incumbent file as a duplicate.
        self.recover_provider_upload_journals_locked()?;
        if let Some(receipt) = self.load_provider_upload_receipt_locked(lease, upload_id)? {
            if receipt.lease.peer_device_id != peer_device_id
                || receipt.lease.backup_id != backup_id
            {
                return Err(CoreError::AuthenticationFailed);
            }
            self.cleanup_recovery_capsule_upload_locked(lease)?;
            self.clear_staged_capsule_upload_locked(lease, upload_id)?;
            return Ok(receipt.created);
        }
        let mut state =
            self.load_active_provider_lease_locked(peer_device_id, backup_id, lease, now_unix_ms)?;
        let metadata = self.load_recovery_capsule_upload_locked(lease, upload_id)?;
        let staged = state
            .staged_capsule_upload
            .clone()
            .ok_or(CoreError::AuthenticationFailed)?;
        let descriptor = metadata
            .descriptor
            .clone()
            .ok_or(CoreError::AuthenticationFailed)?;
        if staged.upload != metadata
            || descriptor.backup_id != backup_id
            || descriptor.signer_device_id != peer_device_id
            || descriptor.total_bytes != metadata.total_bytes
            || descriptor.capsule_digest != metadata.capsule_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        if let Some(created) = staged.committed_created {
            let object_key = normalize_provider_capsule_object_key(&mut state, &descriptor)?;
            let capsule_directory = self
                .recovery_capsule_directory(
                    peer_device_id,
                    backup_id,
                    false,
                    "open committed staged recovery capsule directory",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            if state.objects.get(&object_key) != Some(&metadata.total_bytes)
                || self.hash_anchored_private_file_bounded(
                    &capsule_directory,
                    &format!("{}.json", descriptor.snapshot_id),
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "verify committed staged recovery capsule",
                )? != Some((metadata.total_bytes, metadata.capsule_digest.clone()))
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let completed_at_unix_ms = staged
                .completed_at_unix_ms
                .unwrap_or(staged.upload.created_at_unix_ms);
            self.persist_provider_upload_receipt_locked(
                lease,
                upload_id,
                created,
                completed_at_unix_ms,
            )?;
            self.cleanup_recovery_capsule_upload_locked(lease)?;
            state.staged_capsule_upload = None;
            self.persist_provider_lease_locked(&state)?;
            return Ok(created);
        }
        let directory = self
            .recovery_capsule_upload_directory(
                lease,
                false,
                "open recovery capsule assembly directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let segments_directory = self
            .recovery_capsule_segments_directory(
                lease,
                false,
                "open recovery capsule segment directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        self.remove_anchored_file(
            &directory,
            "assembled.tmp",
            "replace recovery capsule assembly",
        )?;
        let assembled_path = directory.path.join("assembled.tmp");
        let required_assembly_space = metadata
            .total_bytes
            .checked_add(self.provider_quota_policy.free_space_reserve_bytes)
            .ok_or(CoreError::ResourceLimit("recovery capsule assembly"))?;
        if fs2::available_space(&self.root).map_err(|source| CoreError::Io {
            operation: "inspect recovery capsule assembly free space",
            path: self.root.clone(),
            source,
        })? < required_assembly_space
        {
            return Err(CoreError::ResourceLimit("provider free-space reserve"));
        }
        let mut assembled = self.create_anchored_private_file(
            &directory,
            "assembled.tmp",
            "create recovery capsule assembly",
        )?;
        let mut hasher = blake3::Hasher::new();
        let mut total_written = 0_u64;
        let mut buffer = vec![0_u8; 1024 * 1024];
        for index in 0..metadata.total_segments {
            let segment_name = format!("{index:08}.bin");
            let segment_path = segments_directory.path.join(&segment_name);
            let expected_length = (metadata.total_bytes - total_written)
                .min(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64);
            let (mut segment, _) = self
                .open_anchored_private_file_bounded(
                    &segments_directory,
                    &segment_name,
                    MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64,
                    Some(expected_length),
                    "open recovery capsule segment",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
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
        let (assembled_for_parse, assembled_length) = self
            .open_anchored_private_file_bounded(
                &directory,
                "assembled.tmp",
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                Some(metadata.total_bytes),
                "open recovery capsule assembly",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let capsule_header = parse_recovery_capsule_header_file(
            assembled_for_parse,
            assembled_length,
            &assembled_path,
        )?;
        if capsule_header.backup_id != descriptor.backup_id
            || capsule_header.backup_id != backup_id
            || capsule_header.snapshot_id != descriptor.snapshot_id
            || capsule_header.key_epoch != descriptor.key_epoch
            || capsule_header.committed_at_unix_ms != descriptor.committed_at_unix_ms
            || capsule_header.signer_device_id != descriptor.signer_device_id
            || capsule_header.signer_device_id != peer_device_id
            || capsule_header.capsule_digest != metadata.capsule_digest
            || lease.backup_id != capsule_header.backup_id
            || lease.peer_device_id != capsule_header.signer_device_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let object_key =
            provider_capsule_object_key(peer_device_id, backup_id, &descriptor.snapshot_id);
        let capsule_directory = self
            .recovery_capsule_directory(
                peer_device_id,
                backup_id,
                true,
                "open committed recovery capsule directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let capsule_name = format!("{}.json", descriptor.snapshot_id);
        let already_accounted = match state.objects.get(&object_key) {
            Some(length) if *length == metadata.total_bytes => true,
            Some(_) => return Err(CoreError::AuthenticationFailed),
            None => false,
        };
        let expected_new_object = match self.hash_anchored_private_file_bounded(
            &capsule_directory,
            &capsule_name,
            MAX_RECOVERY_CAPSULE_BYTES as u64,
            "hash existing recovery capsule",
        )? {
            Some((existing_bytes, existing_digest)) => {
                if existing_bytes != metadata.total_bytes
                    || existing_digest != metadata.capsule_digest
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                false
            }
            None => true,
        };
        if already_accounted && expected_new_object {
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
            let (mut source, _) = self
                .open_anchored_private_file_bounded(
                    &directory,
                    "assembled.tmp",
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    Some(metadata.total_bytes),
                    "open committed recovery capsule assembly",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            if !self.copy_anchored_private_file_noclobber(
                &mut source,
                &assembled_path,
                metadata.total_bytes,
                &metadata.capsule_digest,
                &capsule_directory,
                &capsule_name,
                "commit recovery capsule assembly",
            )? {
                return Err(CoreError::AuthenticationFailed);
            }
            self.remove_anchored_file(
                &directory,
                "assembled.tmp",
                "remove committed recovery capsule assembly",
            )?;
            true
        } else {
            self.remove_anchored_file(
                &directory,
                "assembled.tmp",
                "discard duplicate recovery capsule assembly",
            )?;
            false
        };
        provider_upload_failpoint(2)?;
        if created && !already_accounted {
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
        let receipt_created = if already_accounted {
            staged.expected_new_object
        } else {
            created
        };
        state
            .staged_capsule_upload
            .as_mut()
            .ok_or(CoreError::AuthenticationFailed)?
            .committed_created = Some(receipt_created);
        state
            .staged_capsule_upload
            .as_mut()
            .ok_or(CoreError::AuthenticationFailed)?
            .completed_at_unix_ms = Some(now_unix_ms);
        self.persist_provider_lease_locked(&state)?;
        self.complete_provider_upload_journal_locked(&journal)?;
        self.persist_provider_upload_receipt_locked(
            lease,
            upload_id,
            receipt_created,
            now_unix_ms,
        )?;
        self.cleanup_recovery_capsule_upload_locked(lease)?;
        state.staged_capsule_upload = None;
        self.persist_provider_lease_locked(&state)?;
        drop(guard);
        Ok(receipt_created)
    }

    /// Lists every bounded capsule available to an authenticated recovery principal.
    pub fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
        let root = self.root.join("recovery-capsules/by-owner");
        let mut capsules = Vec::new();
        for owner_entry in read_directory_sorted(&root)? {
            let owner_device_id = DeviceId::from_str(&owner_entry.file_name().to_string_lossy())
                .map_err(|_| CoreError::AuthenticationFailed)?;
            let owner_path = owner_entry.path();
            let owner_metadata =
                fs::symlink_metadata(&owner_path).map_err(|source| CoreError::Io {
                    operation: "inspect recovery capsule owner directory",
                    path: owner_path.clone(),
                    source,
                })?;
            if owner_metadata.file_type().is_symlink() || !owner_metadata.is_dir() {
                return Err(CoreError::AuthenticationFailed);
            }
            for backup_entry in read_directory_sorted(&owner_path)? {
                let backup_id = BackupId::from_str(&backup_entry.file_name().to_string_lossy())
                    .map_err(|_| CoreError::AuthenticationFailed)?;
                let backup_path = backup_entry.path();
                let metadata =
                    fs::symlink_metadata(&backup_path).map_err(|source| CoreError::Io {
                        operation: "inspect recovery capsule backup directory",
                        path: backup_path.clone(),
                        source,
                    })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CoreError::InvalidState(
                        "unexpected recovery capsule entry".to_owned(),
                    ));
                }
                for entry in read_directory_sorted(&backup_path)? {
                    let path = entry.path();
                    let capsule: RecoveryCapsule =
                        serde_json::from_slice(&read_private_regular_file_bounded(
                            &path,
                            MAX_RECOVERY_CAPSULE_BYTES as u64,
                            "read listed recovery capsule",
                        )?)?;
                    if capsule.signer_device_id != owner_device_id
                        || capsule.backup_id != backup_id
                        || path.extension().and_then(|value| value.to_str()) != Some("json")
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
        }
        capsules.sort_by(|left, right| {
            (left.signer_device_id, left.backup_id, &left.snapshot_id).cmp(&(
                right.signer_device_id,
                right.backup_id,
                &right.snapshot_id,
            ))
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
        self.list_recovery_capsule_descriptors_for_owner_before(
            owner_device_id,
            backup_id,
            cursor,
            limit,
            None,
        )
    }

    /// Lists a bounded page and checks the caller's deadline before every disk record.
    pub fn list_recovery_capsule_descriptors_for_owner_with_deadline(
        &self,
        owner_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
        deadline: Instant,
    ) -> Result<(Vec<RecoveryCapsuleDescriptor>, Option<String>), CoreError> {
        self.list_recovery_capsule_descriptors_for_owner_before(
            owner_device_id,
            backup_id,
            cursor,
            limit,
            Some(deadline),
        )
    }

    fn list_recovery_capsule_descriptors_for_owner_before(
        &self,
        owner_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
        deadline: Option<Instant>,
    ) -> Result<(Vec<RecoveryCapsuleDescriptor>, Option<String>), CoreError> {
        if !(1..=128).contains(&limit) {
            return Err(CoreError::InvalidState(
                "invalid recovery capsule descriptor page".to_owned(),
            ));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(CoreError::ResourceLimit("QUIC operation timeout"));
        }
        let _guard = match deadline {
            None => self
                .transaction_lock
                .lock()
                .map_err(|_| CoreError::Synchronization)?,
            Some(deadline) => loop {
                match self.transaction_lock.try_lock() {
                    Ok(guard) => break guard,
                    Err(TryLockError::Poisoned(_)) => {
                        return Err(CoreError::Synchronization);
                    }
                    Err(TryLockError::WouldBlock) => {
                        let now = Instant::now();
                        if now >= deadline {
                            return Err(CoreError::ResourceLimit("QUIC operation timeout"));
                        }
                        std::thread::park_timeout(
                            deadline
                                .saturating_duration_since(now)
                                .min(Duration::from_millis(1)),
                        );
                    }
                }
            },
        };
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(CoreError::ResourceLimit("QUIC operation timeout"));
        }
        let generation = self.recovery_capsule_page_generation_locked()?;
        let feed = self.recovery_capsule_page_feed_directory(owner_device_id, backup_id);
        let feed_directory = self.anchored_directory_for_store_path(
            &feed,
            false,
            "open recovery capsule descriptor page feed",
        )?;
        if feed_directory.is_none() {
            let descriptor_scope = backup_id.map_or_else(
                || {
                    self.root
                        .join("recovery-capsule-index")
                        .join(owner_device_id.to_string())
                },
                |backup_id| {
                    self.root
                        .join("recovery-capsule-index")
                        .join(owner_device_id.to_string())
                        .join(backup_id.to_string())
                },
            );
            return match self.anchored_directory_for_store_path(
                &descriptor_scope,
                false,
                "open recovery capsule descriptor scope",
            )? {
                None => Ok((Vec::new(), None)),
                Some(_) => Err(CoreError::AuthenticationFailed),
            };
        }
        let maximum_entries = backup_id
            .map_or(self.provider_quota_policy.maximum_peer_objects, |_| {
                self.provider_quota_policy.maximum_backup_objects
            });
        let state = self.load_recovery_capsule_page_state_locked(&feed, maximum_entries)?;
        let start_sequence = match cursor {
            Some(cursor) => {
                parse_recovery_capsule_page_cursor(cursor, owner_device_id, backup_id, &generation)?
                    .checked_add(1)
                    .ok_or(CoreError::ResourceLimit("recovery capsule cursor"))?
            }
            None => 0,
        };
        if start_sequence > state.next_sequence {
            return Err(CoreError::AuthenticationFailed);
        }
        let requested_entries = usize::from(limit).saturating_add(1);
        let available_entries = state.next_sequence.saturating_sub(start_sequence);
        let entries_to_read =
            requested_entries.min(usize::try_from(available_entries).unwrap_or(usize::MAX));
        let entries_directory = if entries_to_read == 0 {
            None
        } else {
            Some(
                self.anchored_directory_for_store_path(
                    &feed.join("entries"),
                    false,
                    "open recovery capsule page entries",
                )?
                .ok_or(CoreError::AuthenticationFailed)?,
            )
        };
        let mut descriptors = Vec::with_capacity(entries_to_read);
        for offset in 0..entries_to_read {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(CoreError::ResourceLimit("QUIC operation timeout"));
            }
            let sequence = start_sequence
                .checked_add(offset as u64)
                .ok_or(CoreError::ResourceLimit("recovery capsule page sequence"))?;
            #[cfg(test)]
            RECOVERY_CAPSULE_PAGE_READS.with(|reads| reads.set(reads.get().saturating_add(1)));
            let entry: RecoveryCapsulePageEntry = serde_json::from_slice(
                &self
                    .read_anchored_private_file_bounded(
                        entries_directory
                            .as_ref()
                            .ok_or(CoreError::AuthenticationFailed)?,
                        &format!("{sequence:020}.json"),
                        MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
                        "read recovery capsule page entry",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?,
            )?;
            if entry.schema_version != RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION
                || !valid_recovery_capsule_descriptor(&entry.descriptor)
                || entry.descriptor.signer_device_id != owner_device_id
                || backup_id.is_some_and(|backup_id| entry.descriptor.backup_id != backup_id)
            {
                return Err(CoreError::AuthenticationFailed);
            }
            descriptors.push(entry.descriptor);
        }
        let has_more = descriptors.len() > usize::from(limit);
        descriptors.truncate(usize::from(limit));
        let next_cursor = has_more.then(|| {
            let sequence = start_sequence + descriptors.len().saturating_sub(1) as u64;
            recovery_capsule_page_cursor(owner_device_id, backup_id, &generation, sequence)
        });
        Ok((descriptors, next_cursor))
    }

    /// Reads one bounded byte range after resolving an owner-scoped descriptor.
    pub fn recovery_capsule_is_committed_for_owner(
        &self,
        owner_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        total_bytes: u64,
        capsule_digest: &str,
    ) -> Result<bool, CoreError> {
        if total_bytes == 0
            || total_bytes > MAX_RECOVERY_CAPSULE_BYTES as u64
            || validate_hex_locator(capsule_digest).is_err()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        validate_snapshot_id(snapshot_id)?;
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let Some(descriptor_directory) = self.recovery_capsule_descriptor_directory(
            owner_device_id,
            backup_id,
            false,
            "open exact recovery capsule descriptor directory",
        )?
        else {
            return Ok(false);
        };
        let descriptor_name = format!("{snapshot_id}.json");
        let Some(descriptor_bytes) = self.read_anchored_private_file_bounded(
            &descriptor_directory,
            &descriptor_name,
            MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
            "read exact recovery capsule descriptor",
        )?
        else {
            return Ok(false);
        };
        let descriptor: RecoveryCapsuleDescriptor = serde_json::from_slice(&descriptor_bytes)?;
        if descriptor.signer_device_id != owner_device_id
            || descriptor.backup_id != backup_id
            || descriptor.snapshot_id != snapshot_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        if descriptor.total_bytes != total_bytes || descriptor.capsule_digest != capsule_digest {
            return Ok(false);
        }
        let Some(capsule_directory) = self.recovery_capsule_directory(
            owner_device_id,
            backup_id,
            false,
            "open exact committed recovery capsule directory",
        )?
        else {
            return Ok(false);
        };
        Ok(self.hash_anchored_private_file_bounded(
            &capsule_directory,
            &format!("{snapshot_id}.json"),
            MAX_RECOVERY_CAPSULE_BYTES as u64,
            "verify exact committed recovery capsule",
        )? == Some((total_bytes, capsule_digest.to_owned())))
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
        validate_snapshot_id(snapshot_id)?;
        let descriptor_directory = self
            .recovery_capsule_descriptor_directory(
                owner_device_id,
                backup_id,
                false,
                "open recovery capsule segment descriptor directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let descriptor: RecoveryCapsuleDescriptor = serde_json::from_slice(
            &self
                .read_anchored_private_file_bounded(
                    &descriptor_directory,
                    &format!("{snapshot_id}.json"),
                    MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
                    "read recovery capsule segment descriptor",
                )?
                .ok_or(CoreError::AuthenticationFailed)?,
        )?;
        if descriptor.signer_device_id != owner_device_id
            || descriptor.backup_id != backup_id
            || descriptor.snapshot_id != snapshot_id
            || offset > descriptor.total_bytes
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let capsule_directory = self
            .recovery_capsule_directory(
                owner_device_id,
                backup_id,
                false,
                "open recovery capsule segment directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let capsule_name = format!("{snapshot_id}.json");
        let (mut file, original_length) = self
            .open_anchored_private_file_bounded(
                &capsule_directory,
                &capsule_name,
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                Some(descriptor.total_bytes),
                "open recovery capsule segment",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        use std::io::{Read as _, Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(offset))
            .map_err(|source| CoreError::Io {
                operation: "seek recovery capsule segment",
                path: capsule_directory.path.join(&capsule_name),
                source,
            })?;
        let remaining = descriptor.total_bytes.saturating_sub(offset);
        let length = remaining.min(maximum_bytes as u64) as usize;
        let mut bytes = vec![0_u8; length];
        file.read_exact(&mut bytes)
            .map_err(|source| CoreError::Io {
                operation: "read recovery capsule segment",
                path: capsule_directory.path.join(&capsule_name),
                source,
            })?;
        if file
            .metadata()
            .map_err(|source| CoreError::Io {
                operation: "verify recovery capsule segment",
                path: capsule_directory.path.join(&capsule_name),
                source,
            })?
            .len()
            != original_length
        {
            return Err(CoreError::AuthenticationFailed);
        }
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
        owner_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<PathBuf, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        Ok(self
            .root
            .join("recovery-capsules")
            .join("by-owner")
            .join(owner_device_id.to_string())
            .join(backup_id.to_string())
            .join(format!("{snapshot_id}.json")))
    }

    fn recovery_capsule_directory(
        &self,
        owner_device_id: DeviceId,
        backup_id: BackupId,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        self.anchored_directory(
            "recovery-capsules",
            &[
                "by-owner".to_owned(),
                owner_device_id.to_string(),
                backup_id.to_string(),
            ],
            create,
            operation,
        )
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

    fn recovery_capsule_descriptor_directory(
        &self,
        owner_device_id: DeviceId,
        backup_id: BackupId,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        self.anchored_directory(
            "recovery-capsule-index",
            &[owner_device_id.to_string(), backup_id.to_string()],
            create,
            operation,
        )
    }

    fn recovery_capsule_upload_attempt_path(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<PathBuf, CoreError> {
        Ok(self.root.join("recovery-upload-attempts").join(
            self.recovery_capsule_upload_attempt_name(
                provider_device_id,
                backup_id,
                snapshot_id,
                capsule_digest,
            )?,
        ))
    }

    fn recovery_capsule_upload_attempt_name(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<String, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        if validate_hex_locator(capsule_digest).is_err() {
            return Err(CoreError::AuthenticationFailed);
        }
        let key = blake3::hash(
            format!("{provider_device_id}:{backup_id}:{snapshot_id}:{capsule_digest}").as_bytes(),
        );
        Ok(format!("{}.json", key.to_hex()))
    }

    fn recovery_capsule_upload_attempt_path_for_value(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<PathBuf, CoreError> {
        self.recovery_capsule_upload_attempt_path(
            attempt.provider_device_id,
            attempt.backup_id,
            &attempt.snapshot_id,
            &attempt.capsule_digest,
        )
    }

    fn recovery_capsule_lease_intent_path(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<PathBuf, CoreError> {
        Ok(self.root.join("recovery-upload-intents").join(
            self.recovery_capsule_lease_intent_name(
                provider_device_id,
                backup_id,
                snapshot_id,
                capsule_digest,
            )?,
        ))
    }

    fn recovery_capsule_lease_intent_name(
        &self,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<String, CoreError> {
        validate_snapshot_id(snapshot_id)?;
        validate_hex_locator(capsule_digest)?;
        let key = blake3::hash(
            format!("{provider_device_id}:{backup_id}:{snapshot_id}:{capsule_digest}").as_bytes(),
        );
        Ok(format!("{}.json", key.to_hex()))
    }

    fn recovery_capsule_lease_intent_path_for_value(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<PathBuf, CoreError> {
        self.recovery_capsule_lease_intent_path(
            intent.provider_device_id,
            intent.backup_id,
            &intent.snapshot_id,
            &intent.capsule_digest,
        )
    }

    fn validate_recovery_capsule_upload_attempts(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        for name in self.anchored_directory_entries_bounded(
            &directory,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
            "recovery capsule upload attempts",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &directory,
                    name,
                    MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPT_BYTES as u64,
                    "read recovery capsule upload attempt",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let attempt: RecoveryCapsuleUploadAttempt = serde_json::from_slice(&bytes)?;
            if !valid_recovery_capsule_upload_attempt(&attempt)
                || self
                    .recovery_capsule_upload_attempt_path_for_value(&attempt)?
                    .file_name()
                    .and_then(|value| value.to_str())
                    != Some(name)
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    fn validate_recovery_capsule_lease_intents(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let intents_directory = self
            .anchored_directory(
                "recovery-upload-intents",
                &[],
                false,
                "open recovery capsule lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        for name in self.anchored_directory_entries_bounded(
            &intents_directory,
            MAX_RECOVERY_CAPSULE_LEASE_INTENTS,
            "recovery capsule lease intents",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &intents_directory,
                    name,
                    MAX_RECOVERY_CAPSULE_LEASE_INTENT_BYTES as u64,
                    "read recovery capsule lease intent",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let intent: RecoveryCapsuleLeaseIntent = serde_json::from_slice(&bytes)?;
            if !valid_recovery_capsule_lease_intent(&intent)
                || self
                    .recovery_capsule_lease_intent_path_for_value(&intent)?
                    .file_name()
                    .and_then(|value| value.to_str())
                    != Some(name)
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        let attempts_directory = self
            .anchored_directory(
                "recovery-upload-attempts",
                &[],
                false,
                "open recovery capsule upload attempts",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let attempts = self.anchored_directory_entries_bounded(
            &attempts_directory,
            MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS,
            "recovery capsule upload attempts",
        )?;
        let intents = self.anchored_directory_entries_bounded(
            &intents_directory,
            MAX_RECOVERY_CAPSULE_LEASE_INTENTS,
            "recovery capsule lease intents",
        )?;
        if attempts.len().saturating_add(intents.len()) > MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS {
            return Err(CoreError::ResourceLimit(
                "recovery capsule upload attempt backpressure",
            ));
        }
        Ok(())
    }

    fn validate_provider_write_lease_intents(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let directory = self
            .anchored_directory(
                "provider-write-intents",
                &[],
                false,
                "open provider write lease intents",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        for name in self.anchored_directory_entries_bounded(
            &directory,
            MAX_PROVIDER_WRITE_LEASE_INTENTS,
            "provider write lease intents",
        )? {
            let name = name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let bytes = self
                .read_anchored_private_file_bounded(
                    &directory,
                    name,
                    MAX_PROVIDER_WRITE_LEASE_INTENT_BYTES as u64,
                    "read provider write lease intent",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let intent: ProviderWriteLeaseIntent = serde_json::from_slice(&bytes)?;
            if !valid_provider_write_lease_intent(&intent)
                || provider_write_lease_intent_name(intent.provider_device_id, intent.backup_id)
                    != name
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        Ok(())
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
        if !valid_recovery_capsule_descriptor(descriptor) {
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
        let directory = self
            .anchored_directory_for_store_path(
                parent,
                true,
                "open recovery capsule descriptor directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?;
        match self.read_anchored_private_file_bounded(
            &directory,
            name,
            MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
            "read recovery capsule descriptor incumbent",
        )? {
            Some(bytes) => {
                let incumbent: RecoveryCapsuleDescriptor = serde_json::from_slice(&bytes)?;
                if incumbent != *descriptor {
                    return Err(CoreError::AuthenticationFailed);
                }
            }
            None => {
                if !self.write_anchored_atomic(
                    &directory,
                    name,
                    &serde_json::to_vec_pretty(descriptor)?,
                    true,
                    "persist recovery capsule descriptor",
                )? {
                    return Err(CoreError::AuthenticationFailed);
                }
            }
        }
        self.ensure_recovery_capsule_descriptor_page_entries_locked(descriptor)
    }

    fn recovery_capsule_page_feed_directory(
        &self,
        owner_device_id: DeviceId,
        backup_id: Option<BackupId>,
    ) -> PathBuf {
        let owner = self
            .root
            .join("recovery-capsule-pages")
            .join(owner_device_id.to_string());
        backup_id.map_or_else(
            || owner.join("all"),
            |backup_id| owner.join("backups").join(backup_id.to_string()),
        )
    }

    fn recovery_capsule_page_marker_path(
        &self,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<PathBuf, CoreError> {
        validate_snapshot_id(&descriptor.snapshot_id)?;
        Ok(self
            .root
            .join("recovery-capsule-pages")
            .join(descriptor.signer_device_id.to_string())
            .join("markers")
            .join(descriptor.backup_id.to_string())
            .join(format!("{}.json", descriptor.snapshot_id)))
    }

    #[cfg(test)]
    fn recovery_capsule_page_entry_path(feed: &Path, sequence: u64) -> PathBuf {
        feed.join("entries").join(format!("{sequence:020}.json"))
    }

    fn load_recovery_capsule_page_state_locked(
        &self,
        feed: &Path,
        maximum_entries: u64,
    ) -> Result<RecoveryCapsulePageState, CoreError> {
        let Some(directory) =
            self.anchored_directory_for_store_path(feed, false, "open recovery capsule page feed")?
        else {
            return Ok(RecoveryCapsulePageState {
                schema_version: RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION,
                next_sequence: 0,
            });
        };
        let state = match self.read_anchored_private_file_bounded(
            &directory,
            "state.json",
            MAX_RECOVERY_CAPSULE_PAGE_STATE_BYTES as u64,
            "read recovery capsule page state",
        )? {
            None => RecoveryCapsulePageState {
                schema_version: RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION,
                next_sequence: 0,
            },
            Some(bytes) => serde_json::from_slice(&bytes)?,
        };
        if state.schema_version != RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION
            || state.next_sequence > maximum_entries
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(state)
    }

    fn persist_recovery_capsule_page_state_locked(
        &self,
        feed: &Path,
        state: &RecoveryCapsulePageState,
        maximum_entries: u64,
    ) -> Result<(), CoreError> {
        if state.schema_version != RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION
            || state.next_sequence > maximum_entries
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let directory = self
            .anchored_directory_for_store_path(feed, true, "open recovery capsule page feed")?
            .ok_or(CoreError::AuthenticationFailed)?;
        self.anchored_directory_for_store_path(
            &feed.join("entries"),
            true,
            "open recovery capsule page entries",
        )?
        .ok_or(CoreError::AuthenticationFailed)?;
        self.write_anchored_atomic(
            &directory,
            "state.json",
            &serde_json::to_vec_pretty(state)?,
            false,
            "persist recovery capsule page state",
        )?;
        Ok(())
    }

    fn ensure_recovery_capsule_page_entry_locked(
        &self,
        feed: &Path,
        sequence: u64,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<(), CoreError> {
        let entries = self
            .anchored_directory_for_store_path(
                &feed.join("entries"),
                true,
                "open recovery capsule page entries",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let name = format!("{sequence:020}.json");
        let requested = RecoveryCapsulePageEntry {
            schema_version: RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION,
            descriptor: descriptor.clone(),
        };
        match self.read_anchored_private_file_bounded(
            &entries,
            &name,
            MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
            "read recovery capsule page entry incumbent",
        )? {
            Some(bytes) => {
                let incumbent: RecoveryCapsulePageEntry = serde_json::from_slice(&bytes)?;
                if incumbent != requested {
                    return Err(CoreError::AuthenticationFailed);
                }
                Ok(())
            }
            None => {
                if self.write_anchored_atomic(
                    &entries,
                    &name,
                    &serde_json::to_vec_pretty(&requested)?,
                    true,
                    "persist recovery capsule page entry",
                )? {
                    Ok(())
                } else {
                    Err(CoreError::AuthenticationFailed)
                }
            }
        }
    }

    fn ensure_recovery_capsule_descriptor_page_entries_locked(
        &self,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<(), CoreError> {
        let all_feed = self.recovery_capsule_page_feed_directory(descriptor.signer_device_id, None);
        let backup_feed = self.recovery_capsule_page_feed_directory(
            descriptor.signer_device_id,
            Some(descriptor.backup_id),
        );
        let marker_path = self.recovery_capsule_page_marker_path(descriptor)?;
        let marker_parent = marker_path.parent().ok_or_else(|| {
            CoreError::InvalidState("recovery capsule page marker has no parent".to_owned())
        })?;
        let marker_directory = self.anchored_directory_for_store_path(
            marker_parent,
            false,
            "open recovery capsule page markers",
        )?;
        let marker_name = marker_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or(CoreError::AuthenticationFailed)?;
        let descriptor_digest = blake3::hash(&serde_json::to_vec(descriptor)?)
            .to_hex()
            .to_string();
        let marker_bytes = marker_directory.as_ref().map_or(Ok(None), |directory| {
            self.read_anchored_private_file_bounded(
                directory,
                marker_name,
                MAX_RECOVERY_CAPSULE_PAGE_MARKER_BYTES as u64,
                "read recovery capsule page marker",
            )
        })?;
        let marker = match marker_bytes {
            Some(bytes) => {
                let marker: RecoveryCapsulePageMarker = serde_json::from_slice(&bytes)?;
                if marker.schema_version != RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION
                    || marker.descriptor_digest != descriptor_digest
                    || marker.all_sequence >= self.provider_quota_policy.maximum_peer_objects
                    || marker.backup_sequence >= self.provider_quota_policy.maximum_backup_objects
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                marker
            }
            None => {
                let all_state = self.load_recovery_capsule_page_state_locked(
                    &all_feed,
                    self.provider_quota_policy.maximum_peer_objects,
                )?;
                let backup_state = self.load_recovery_capsule_page_state_locked(
                    &backup_feed,
                    self.provider_quota_policy.maximum_backup_objects,
                )?;
                if all_state.next_sequence >= self.provider_quota_policy.maximum_peer_objects
                    || backup_state.next_sequence
                        >= self.provider_quota_policy.maximum_backup_objects
                {
                    return Err(CoreError::ResourceLimit(
                        "recovery capsule descriptor page index",
                    ));
                }
                let marker = RecoveryCapsulePageMarker {
                    schema_version: RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION,
                    descriptor_digest,
                    all_sequence: all_state.next_sequence,
                    backup_sequence: backup_state.next_sequence,
                };
                self.ensure_recovery_capsule_page_entry_locked(
                    &all_feed,
                    marker.all_sequence,
                    descriptor,
                )?;
                self.ensure_recovery_capsule_page_entry_locked(
                    &backup_feed,
                    marker.backup_sequence,
                    descriptor,
                )?;
                let marker_directory = self
                    .anchored_directory_for_store_path(
                        marker_parent,
                        true,
                        "open recovery capsule page markers",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?;
                if !self.write_anchored_atomic(
                    &marker_directory,
                    marker_name,
                    &serde_json::to_vec_pretty(&marker)?,
                    true,
                    "persist recovery capsule page marker",
                )? {
                    return Err(CoreError::AuthenticationFailed);
                }
                marker
            }
        };
        self.ensure_recovery_capsule_page_entry_locked(&all_feed, marker.all_sequence, descriptor)?;
        self.ensure_recovery_capsule_page_entry_locked(
            &backup_feed,
            marker.backup_sequence,
            descriptor,
        )?;
        let mut all_state = self.load_recovery_capsule_page_state_locked(
            &all_feed,
            self.provider_quota_policy.maximum_peer_objects,
        )?;
        let mut backup_state = self.load_recovery_capsule_page_state_locked(
            &backup_feed,
            self.provider_quota_policy.maximum_backup_objects,
        )?;
        let all_next = marker
            .all_sequence
            .checked_add(1)
            .ok_or(CoreError::ResourceLimit("recovery capsule page sequence"))?;
        let backup_next = marker
            .backup_sequence
            .checked_add(1)
            .ok_or(CoreError::ResourceLimit("recovery capsule page sequence"))?;
        if all_state.next_sequence < marker.all_sequence
            || backup_state.next_sequence < marker.backup_sequence
        {
            return Err(CoreError::AuthenticationFailed);
        }
        if all_state.next_sequence < all_next {
            all_state.next_sequence = all_next;
            self.persist_recovery_capsule_page_state_locked(
                &all_feed,
                &all_state,
                self.provider_quota_policy.maximum_peer_objects,
            )?;
        }
        if backup_state.next_sequence < backup_next {
            backup_state.next_sequence = backup_next;
            self.persist_recovery_capsule_page_state_locked(
                &backup_feed,
                &backup_state,
                self.provider_quota_policy.maximum_backup_objects,
            )?;
        }
        Ok(())
    }

    fn recover_recovery_capsule_page_index(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let page_root = self.root.join("recovery-capsule-pages");
        let page_directory = self
            .anchored_directory_for_store_path(&page_root, true, "open recovery capsule page root")?
            .ok_or(CoreError::AuthenticationFailed)?;
        if let Some(bytes) = self.read_anchored_private_file_bounded(
            &page_directory,
            "schema.json",
            MAX_RECOVERY_CAPSULE_PAGE_STATE_BYTES as u64,
            "read recovery capsule page schema",
        )? {
            let schema: RecoveryCapsulePageSchema = serde_json::from_slice(&bytes)?;
            if schema.schema_version == RECOVERY_CAPSULE_PAGE_ROOT_SCHEMA_VERSION
                && valid_recovery_capsule_page_generation(&schema.generation)
            {
                return Ok(());
            }
            if schema.schema_version == RECOVERY_CAPSULE_PAGE_SCHEMA_VERSION
                && schema.generation.is_empty()
            {
                self.write_anchored_atomic(
                    &page_directory,
                    "schema.json",
                    &serde_json::to_vec_pretty(&RecoveryCapsulePageSchema {
                        schema_version: RECOVERY_CAPSULE_PAGE_ROOT_SCHEMA_VERSION,
                        generation: uuid::Uuid::new_v4().simple().to_string(),
                    })?,
                    false,
                    "upgrade recovery capsule page schema",
                )?;
                return Ok(());
            }
            return Err(CoreError::AuthenticationFailed);
        }

        // One-time schema-v1 migration is explicitly quota-bounded. Normal page
        // requests never enumerate directories; they direct-open limit + 1 sequence files.
        let index_root = self.root.join("recovery-capsule-index");
        let index_directory = self
            .anchored_directory_for_store_path(
                &index_root,
                true,
                "open recovery capsule index root",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut total_descriptors = 0_u64;
        let mut owner_count = 0_usize;
        for owner_name in self.anchored_directory_entries_bounded(
            &index_directory,
            MAX_RETAINED_PROVIDER_LEASE_SCOPES,
            "recovery capsule index owners",
        )? {
            owner_count = owner_count.saturating_add(1);
            if owner_count > MAX_RETAINED_PROVIDER_LEASE_SCOPES {
                return Err(CoreError::ResourceLimit("recovery capsule index owners"));
            }
            let owner_name = owner_name.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let owner_device_id =
                DeviceId::from_str(owner_name).map_err(|_| CoreError::AuthenticationFailed)?;
            let owner_path = index_root.join(owner_name);
            let owner_directory = self
                .anchored_directory_for_store_path(
                    &owner_path,
                    true,
                    "open recovery capsule index owner",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let mut owner_descriptors = 0_u64;
            let mut backup_count = 0_usize;
            for backup_name in self.anchored_directory_entries_bounded(
                &owner_directory,
                MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER,
                "recovery capsule index backups",
            )? {
                backup_count = backup_count.saturating_add(1);
                if backup_count > MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER {
                    return Err(CoreError::ResourceLimit("recovery capsule index backups"));
                }
                let backup_name = backup_name
                    .to_str()
                    .ok_or(CoreError::AuthenticationFailed)?;
                let backup_id =
                    BackupId::from_str(backup_name).map_err(|_| CoreError::AuthenticationFailed)?;
                let backup_path = owner_path.join(backup_name);
                let backup_directory = self
                    .anchored_directory_for_store_path(
                        &backup_path,
                        true,
                        "open recovery capsule index backup",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?;
                let maximum_backup_descriptors =
                    usize::try_from(self.provider_quota_policy.maximum_backup_objects)
                        .unwrap_or(usize::MAX);
                let mut backup_descriptors = 0_u64;
                for descriptor_name in self.anchored_directory_entries_bounded(
                    &backup_directory,
                    maximum_backup_descriptors,
                    "recovery capsule descriptor migration",
                )? {
                    backup_descriptors = backup_descriptors.saturating_add(1);
                    owner_descriptors = owner_descriptors.saturating_add(1);
                    total_descriptors = total_descriptors.saturating_add(1);
                    if backup_descriptors > self.provider_quota_policy.maximum_backup_objects
                        || owner_descriptors > self.provider_quota_policy.maximum_peer_objects
                        || total_descriptors > self.provider_quota_policy.maximum_total_objects
                    {
                        return Err(CoreError::ResourceLimit(
                            "recovery capsule descriptor migration",
                        ));
                    }
                    let descriptor_name = descriptor_name
                        .to_str()
                        .ok_or(CoreError::AuthenticationFailed)?;
                    if !descriptor_name.ends_with(".json") {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    let descriptor: RecoveryCapsuleDescriptor = serde_json::from_slice(
                        &self
                            .read_anchored_private_file_bounded(
                                &backup_directory,
                                descriptor_name,
                                MAX_RECOVERY_CAPSULE_DESCRIPTOR_BYTES as u64,
                                "read recovery capsule descriptor migration entry",
                            )?
                            .ok_or(CoreError::AuthenticationFailed)?,
                    )?;
                    if !valid_recovery_capsule_descriptor(&descriptor)
                        || descriptor.signer_device_id != owner_device_id
                        || descriptor.backup_id != backup_id
                        || descriptor_name.strip_suffix(".json")
                            != Some(descriptor.snapshot_id.as_str())
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    self.ensure_recovery_capsule_descriptor_page_entries_locked(&descriptor)?;
                }
            }
        }
        self.write_anchored_atomic(
            &page_directory,
            "schema.json",
            &serde_json::to_vec_pretty(&RecoveryCapsulePageSchema {
                schema_version: RECOVERY_CAPSULE_PAGE_ROOT_SCHEMA_VERSION,
                generation: uuid::Uuid::new_v4().simple().to_string(),
            })?,
            false,
            "persist recovery capsule page schema",
        )?;
        Ok(())
    }

    fn recovery_capsule_page_generation_locked(&self) -> Result<String, CoreError> {
        let directory = self
            .anchored_directory(
                "recovery-capsule-pages",
                &[],
                false,
                "open recovery capsule page root",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let schema: RecoveryCapsulePageSchema = serde_json::from_slice(
            &self
                .read_anchored_private_file_bounded(
                    &directory,
                    "schema.json",
                    MAX_RECOVERY_CAPSULE_PAGE_STATE_BYTES as u64,
                    "read recovery capsule page generation",
                )?
                .ok_or(CoreError::AuthenticationFailed)?,
        )?;
        if schema.schema_version != RECOVERY_CAPSULE_PAGE_ROOT_SCHEMA_VERSION
            || !valid_recovery_capsule_page_generation(&schema.generation)
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(schema.generation)
    }

    fn provider_lease_path(&self, lease: &StorageLease) -> Result<PathBuf, CoreError> {
        if lease.lease_id.is_empty() || lease.lease_id.len() > 128 {
            return Err(CoreError::InvalidState(
                "invalid storage lease id".to_owned(),
            ));
        }
        Ok(self.provider_lease_path_parts(lease.peer_device_id, lease.backup_id, &lease.lease_id))
    }

    fn provider_lease_path_parts(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease_id: &str,
    ) -> PathBuf {
        self.root
            .join("provider-leases")
            .join(peer_device_id.to_string())
            .join(backup_id.to_string())
            .join(format!("{lease_id}.json"))
    }

    fn provider_lease_ledger_path(&self) -> PathBuf {
        self.root.join("provider-lease-ledger.json")
    }

    fn recovery_capsule_upload_path(&self, lease: &StorageLease) -> PathBuf {
        self.root
            .join("provider-capsule-uploads")
            .join(lease.peer_device_id.to_string())
            .join(lease.backup_id.to_string())
            .join(&lease.lease_id)
    }

    fn recovery_capsule_upload_directory(
        &self,
        lease: &StorageLease,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        self.recovery_capsule_upload_directory_parts(
            lease.peer_device_id,
            lease.backup_id,
            &lease.lease_id,
            create,
            operation,
        )
    }

    fn recovery_capsule_upload_directory_parts(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease_id: &str,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        if !valid_single_path_component(lease_id) {
            return Err(CoreError::AuthenticationFailed);
        }
        self.anchored_directory(
            "provider-capsule-uploads",
            &[
                peer_device_id.to_string(),
                backup_id.to_string(),
                lease_id.to_owned(),
            ],
            create,
            operation,
        )
    }

    fn recovery_capsule_segments_directory(
        &self,
        lease: &StorageLease,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        self.recovery_capsule_segments_directory_parts(
            lease.peer_device_id,
            lease.backup_id,
            &lease.lease_id,
            create,
            operation,
        )
    }

    fn recovery_capsule_segments_directory_parts(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease_id: &str,
        create: bool,
        operation: &'static str,
    ) -> Result<Option<AnchoredDirectory>, CoreError> {
        if !valid_single_path_component(lease_id) {
            return Err(CoreError::AuthenticationFailed);
        }
        self.anchored_directory(
            "provider-capsule-uploads",
            &[
                peer_device_id.to_string(),
                backup_id.to_string(),
                lease_id.to_owned(),
                "segments".to_owned(),
            ],
            create,
            operation,
        )
    }

    fn ensure_recovery_capsule_upload_directory_locked(
        &self,
        upload: &RecoveryCapsuleUpload,
    ) -> Result<(), CoreError> {
        let directory = self
            .recovery_capsule_upload_directory(&upload.lease, true, "open recovery capsule upload")?
            .ok_or(CoreError::AuthenticationFailed)?;
        self.recovery_capsule_segments_directory(
            &upload.lease,
            true,
            "open recovery capsule segments",
        )?
        .ok_or(CoreError::AuthenticationFailed)?;
        match self.read_anchored_private_file_bounded(
            &directory,
            "metadata.json",
            MAX_PROVIDER_LEASE_STATE_BYTES as u64,
            "read recovery capsule upload metadata",
        )? {
            Some(bytes) => {
                let incumbent: RecoveryCapsuleUpload = serde_json::from_slice(&bytes)?;
                if incumbent != *upload {
                    return Err(CoreError::AuthenticationFailed);
                }
                Ok(())
            }
            None => {
                if self.write_anchored_atomic(
                    &directory,
                    "metadata.json",
                    &serde_json::to_vec_pretty(upload)?,
                    true,
                    "persist recovery capsule upload metadata",
                )? {
                    Ok(())
                } else {
                    Err(CoreError::AuthenticationFailed)
                }
            }
        }
    }

    fn cleanup_recovery_capsule_upload_locked(
        &self,
        lease: &StorageLease,
    ) -> Result<(), CoreError> {
        self.cleanup_recovery_capsule_upload_parts_locked(
            lease.peer_device_id,
            lease.backup_id,
            &lease.lease_id,
        )
    }

    fn cleanup_recovery_capsule_upload_parts_locked(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease_id: &str,
    ) -> Result<(), CoreError> {
        let Some(directory) = self.recovery_capsule_upload_directory_parts(
            peer_device_id,
            backup_id,
            lease_id,
            true,
            "open completed recovery capsule upload",
        )?
        else {
            return Ok(());
        };
        let entries = self.anchored_directory_entries_bounded(
            &directory,
            3,
            "recovery capsule upload entries",
        )?;
        for entry in entries {
            let name = entry.to_str().ok_or(CoreError::AuthenticationFailed)?;
            match name {
                "metadata.json" | "assembled.tmp" => {
                    self.remove_anchored_file(
                        &directory,
                        name,
                        "remove recovery capsule upload file",
                    )?;
                }
                "segments" => {
                    let segments = self
                        .recovery_capsule_segments_directory_parts(
                            peer_device_id,
                            backup_id,
                            lease_id,
                            true,
                            "open completed recovery capsule segments",
                        )?
                        .ok_or(CoreError::AuthenticationFailed)?;
                    for segment in self.anchored_directory_entries_bounded(
                        &segments,
                        MAX_RECOVERY_CAPSULE_SEGMENTS as usize,
                        "recovery capsule segments",
                    )? {
                        let segment = segment.to_str().ok_or(CoreError::AuthenticationFailed)?;
                        if segment.len() != 12
                            || !segment.ends_with(".bin")
                            || !segment.as_bytes()[..8].iter().all(u8::is_ascii_digit)
                        {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        self.remove_anchored_file(
                            &segments,
                            segment,
                            "remove recovery capsule segment",
                        )?;
                    }
                    if !self.remove_anchored_empty_directory(
                        &directory,
                        "segments",
                        "remove recovery capsule segment directory",
                    )? {
                        return Err(CoreError::AuthenticationFailed);
                    }
                }
                _ => return Err(CoreError::AuthenticationFailed),
            }
        }
        if !self
            .anchored_directory_entries_bounded(&directory, 0, "recovery capsule upload entries")?
            .is_empty()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let backup_directory = self
            .anchored_directory(
                "provider-capsule-uploads",
                &[peer_device_id.to_string(), backup_id.to_string()],
                false,
                "open recovery capsule upload backup directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        if !self.remove_anchored_empty_directory(
            &backup_directory,
            lease_id,
            "remove recovery capsule upload directory",
        )? {
            return Err(CoreError::AuthenticationFailed);
        }
        let peer_directory = self
            .anchored_directory(
                "provider-capsule-uploads",
                &[peer_device_id.to_string()],
                false,
                "open recovery capsule upload peer directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let _ = self.remove_anchored_empty_directory(
            &peer_directory,
            &backup_id.to_string(),
            "remove empty recovery capsule upload backup directory",
        )?;
        let root = self
            .anchored_directory(
                "provider-capsule-uploads",
                &[],
                false,
                "open recovery capsule upload root",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let _ = self.remove_anchored_empty_directory(
            &root,
            &peer_device_id.to_string(),
            "remove empty recovery capsule upload peer directory",
        )?;
        Ok(())
    }

    fn clear_staged_capsule_upload_locked(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<(), CoreError> {
        let path = self.provider_lease_path(lease)?;
        let mut state: ProviderLeaseState = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            _ => return Err(CoreError::AuthenticationFailed),
        };
        if state.lease != *lease {
            return Err(CoreError::AuthenticationFailed);
        }
        match &state.staged_capsule_upload {
            Some(staged) if staged.upload.upload_id == upload_id => {
                state.staged_capsule_upload = None;
                self.persist_provider_lease_locked(&state)
            }
            Some(_) => Err(CoreError::AuthenticationFailed),
            None => Ok(()),
        }
    }

    fn discard_recovery_capsule_staging_locked(
        &self,
        state: &mut ProviderLeaseState,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        if self.finalize_committed_recovery_capsule_staging_locked(state, now_unix_ms)? {
            return Ok(());
        }
        self.cleanup_recovery_capsule_upload_locked(&state.lease)?;
        if state.staged_capsule_upload.take().is_some() {
            self.persist_provider_lease_locked(state)?;
        }
        Ok(())
    }

    fn finalize_committed_recovery_capsule_staging_locked(
        &self,
        state: &mut ProviderLeaseState,
        _now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        let Some(staged) = state.staged_capsule_upload.clone() else {
            return Ok(false);
        };
        let Some(created) = staged.committed_created else {
            return Ok(false);
        };
        let descriptor = staged
            .upload
            .descriptor
            .clone()
            .ok_or(CoreError::AuthenticationFailed)?;
        let object_key = normalize_provider_capsule_object_key(state, &descriptor)?;
        let capsule_directory = self
            .recovery_capsule_directory(
                state.lease.peer_device_id,
                descriptor.backup_id,
                false,
                "open committed recovery capsule during staging recovery",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        if state.objects.get(&object_key) != Some(&staged.upload.total_bytes)
            || self.hash_anchored_private_file_bounded(
                &capsule_directory,
                &format!("{}.json", descriptor.snapshot_id),
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                "recover committed staged recovery capsule",
            )? != Some((
                staged.upload.total_bytes,
                staged.upload.capsule_digest.clone(),
            ))
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let completed_at_unix_ms = match self
            .load_provider_upload_receipt_locked(&state.lease, &staged.upload.upload_id)?
        {
            Some(receipt) if receipt.created == created => receipt.completed_at_unix_ms,
            Some(_) => return Err(CoreError::AuthenticationFailed),
            None => staged
                .completed_at_unix_ms
                .unwrap_or(staged.upload.created_at_unix_ms),
        };
        self.persist_provider_upload_receipt_locked(
            &state.lease,
            &staged.upload.upload_id,
            created,
            completed_at_unix_ms,
        )?;
        self.cleanup_recovery_capsule_upload_locked(&state.lease)?;
        state.staged_capsule_upload = None;
        self.persist_provider_lease_locked(state)?;
        Ok(true)
    }

    fn provider_upload_receipt_path(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<PathBuf, CoreError> {
        validate_upload_id(upload_id)?;
        let key = blake3::hash(
            format!(
                "{}:{}:{}:{upload_id}",
                lease.peer_device_id, lease.backup_id, lease.lease_id
            )
            .as_bytes(),
        );
        Ok(self
            .root
            .join("provider-upload-receipts")
            .join(lease.peer_device_id.to_string())
            .join(format!("{}.json", key.to_hex())))
    }

    fn legacy_provider_upload_receipt_path(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<PathBuf, CoreError> {
        validate_upload_id(upload_id)?;
        let key = blake3::hash(
            format!(
                "{}:{}:{}:{upload_id}",
                lease.peer_device_id, lease.backup_id, lease.lease_id
            )
            .as_bytes(),
        );
        Ok(self
            .root
            .join("provider-upload-receipts")
            .join(format!("{}.json", key.to_hex())))
    }

    fn load_provider_upload_receipt_locked(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<Option<ProviderUploadReceipt>, CoreError> {
        let scoped_path = self.provider_upload_receipt_path(lease, upload_id)?;
        let legacy_path = self.legacy_provider_upload_receipt_path(lease, upload_id)?;
        let (path, metadata) = match fs::symlink_metadata(&scoped_path) {
            Ok(metadata) => (scoped_path, metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::symlink_metadata(&legacy_path) {
                    Ok(metadata) => (legacy_path, metadata),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(source) => {
                        return Err(CoreError::Io {
                            operation: "inspect legacy provider upload receipt",
                            path: legacy_path,
                            source,
                        });
                    }
                }
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider upload receipt",
                    path: scoped_path,
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let receipt: ProviderUploadReceipt =
            serde_json::from_slice(&read_private_regular_file_bounded(
                &path,
                MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                "read provider upload receipt",
            )?)?;
        if !self.valid_provider_upload_receipt(&receipt)
            || receipt.lease != *lease
            || receipt.upload_id != upload_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(receipt))
    }

    fn valid_provider_upload_receipt(&self, receipt: &ProviderUploadReceipt) -> bool {
        receipt.schema_version == PROVIDER_LEASE_SCHEMA_VERSION
            && valid_provider_lease_shape(
                &receipt.lease,
                self.provider_quota_policy.maximum_lease_lifetime_ms,
            )
            && validate_upload_id(&receipt.upload_id).is_ok()
            && receipt.completed_at_unix_ms >= receipt.lease.issued_at_unix_ms
    }

    fn persist_provider_upload_receipt_locked(
        &self,
        lease: &StorageLease,
        upload_id: &str,
        created: bool,
        completed_at_unix_ms: u64,
    ) -> Result<(), CoreError> {
        let path = self.provider_upload_receipt_path(lease, upload_id)?;
        let parent = self.ensure_provider_upload_receipt_peer_locked(lease.peer_device_id)?;
        if path.parent() != Some(parent.as_path()) {
            return Err(CoreError::AuthenticationFailed);
        }
        let receipt = ProviderUploadReceipt {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease: lease.clone(),
            upload_id: upload_id.to_owned(),
            created,
            completed_at_unix_ms,
        };
        if !self.valid_provider_upload_receipt(&receipt) {
            return Err(CoreError::AuthenticationFailed);
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                let incumbent: ProviderUploadReceipt =
                    serde_json::from_slice(&read_private_regular_file_bounded(
                        &path,
                        MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                        "read provider upload receipt incumbent",
                    )?)?;
                return if incumbent == receipt {
                    Ok(())
                } else {
                    Err(CoreError::AuthenticationFailed)
                };
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider upload receipt",
                    path,
                    source,
                });
            }
        }
        let existing_receipts = read_directory_sorted_bounded(
            &parent,
            MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK,
            "provider upload receipts per peer",
        )?
        .len();
        if existing_receipts >= MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER {
            return Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure",
            ));
        }
        write_json_atomic(&path, &receipt, true)?;
        provider_upload_receipt_failpoint(1)?;
        Ok(())
    }

    fn ensure_provider_upload_receipt_capacity_locked(
        &self,
        peer_device_id: DeviceId,
    ) -> Result<(), CoreError> {
        let receipt_directory = self
            .root
            .join("provider-upload-receipts")
            .join(peer_device_id.to_string());
        let receipts = match fs::symlink_metadata(&receipt_directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                read_directory_sorted_bounded(
                    &receipt_directory,
                    MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK,
                    "provider upload receipts per peer",
                )?
                .len()
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider receipt peer directory",
                    path: receipt_directory,
                    source,
                });
            }
        };
        let staged = self
            .list_provider_lease_state_files_locked()?
            .into_iter()
            .filter(|(_, state)| {
                state.lease.peer_device_id == peer_device_id
                    && state.staged_capsule_upload.is_some()
            })
            .count();
        if receipts.saturating_add(staged) >= MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER {
            return Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure",
            ));
        }
        Ok(())
    }

    fn ensure_provider_upload_receipt_peer_locked(
        &self,
        peer_device_id: DeviceId,
    ) -> Result<PathBuf, CoreError> {
        let root = self.root.join("provider-upload-receipts");
        let directory = root.join(peer_device_id.to_string());
        match fs::symlink_metadata(&directory) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                return Ok(directory);
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider receipt peer directory",
                    path: directory,
                    source,
                });
            }
        }
        let mut peer_directories = 0_usize;
        for entry in read_directory_sorted_bounded(
            &root,
            MAX_PROVIDER_UPLOAD_RECEIPT_PEERS + MAX_LEGACY_PROVIDER_UPLOAD_RECEIPTS,
            "provider upload receipt root",
        )? {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect provider upload receipt root entry",
                path: path.clone(),
                source,
            })?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                peer_directories += 1;
            } else if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        if peer_directories >= MAX_PROVIDER_UPLOAD_RECEIPT_PEERS {
            return Err(CoreError::ResourceLimit("provider upload receipt peers"));
        }
        ensure_private_directory(&directory)?;
        Ok(directory)
    }

    fn validate_provider_upload_receipts_locked(
        &self,
        peer_device_id: DeviceId,
    ) -> Result<(), CoreError> {
        let directory = self
            .root
            .join("provider-upload-receipts")
            .join(peer_device_id.to_string());
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider receipt peer directory",
                    path: directory,
                    source,
                });
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
        }
        for entry in read_directory_sorted_bounded(
            &directory,
            MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK,
            "provider upload receipts per peer",
        )? {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect provider upload receipt",
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
                || metadata.len() == 0
                || metadata.len() > MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let receipt: ProviderUploadReceipt =
                serde_json::from_slice(&read_private_regular_file_bounded(
                    &path,
                    MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                    "validate provider upload receipt",
                )?)?;
            if !self.valid_provider_upload_receipt(&receipt)
                || receipt.lease.peer_device_id != peer_device_id
                || path != self.provider_upload_receipt_path(&receipt.lease, &receipt.upload_id)?
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    fn recover_provider_upload_receipts(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let root = self.root.join("provider-upload-receipts");
        let entries = read_directory_sorted_bounded(
            &root,
            MAX_PROVIDER_UPLOAD_RECEIPT_PEERS + MAX_LEGACY_PROVIDER_UPLOAD_RECEIPTS,
            "provider upload receipt root",
        )?;
        let mut peers = BTreeSet::new();
        let mut legacy_count = 0_usize;
        let mut migrated_legacy = false;
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect provider upload receipt root entry",
                path: path.clone(),
                source,
            })?;
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && path.extension().and_then(|value| value.to_str()) == Some("json")
            {
                legacy_count += 1;
                if legacy_count > MAX_LEGACY_PROVIDER_UPLOAD_RECEIPTS {
                    return Err(CoreError::ResourceLimit("legacy provider upload receipts"));
                }
                let receipt: ProviderUploadReceipt =
                    serde_json::from_slice(&read_private_regular_file_bounded(
                        &path,
                        MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                        "read legacy provider upload receipt",
                    )?)?;
                if !self.valid_provider_upload_receipt(&receipt)
                    || path
                        != self.legacy_provider_upload_receipt_path(
                            &receipt.lease,
                            &receipt.upload_id,
                        )?
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                let scoped_path =
                    self.provider_upload_receipt_path(&receipt.lease, &receipt.upload_id)?;
                let parent =
                    self.ensure_provider_upload_receipt_peer_locked(receipt.lease.peer_device_id)?;
                if scoped_path.parent() != Some(parent.as_path()) {
                    return Err(CoreError::AuthenticationFailed);
                }
                match fs::symlink_metadata(&scoped_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        write_json_atomic(&scoped_path, &receipt, true)?;
                    }
                    Ok(existing) if existing.is_file() && !existing.file_type().is_symlink() => {
                        let incumbent: ProviderUploadReceipt =
                            serde_json::from_slice(&read_private_regular_file_bounded(
                                &scoped_path,
                                MAX_PROVIDER_UPLOAD_RECEIPT_BYTES as u64,
                                "read migrated provider upload receipt",
                            )?)?;
                        if incumbent != receipt {
                            return Err(CoreError::AuthenticationFailed);
                        }
                    }
                    _ => return Err(CoreError::AuthenticationFailed),
                }
                fs::remove_file(&path).map_err(|source| CoreError::Io {
                    operation: "migrate legacy provider upload receipt",
                    path: path.clone(),
                    source,
                })?;
                migrated_legacy = true;
                peers.insert(receipt.lease.peer_device_id);
            } else if metadata.is_dir() && !metadata.file_type().is_symlink() {
                let peer_device_id = DeviceId::from_str(&entry.file_name().to_string_lossy())
                    .map_err(|_| CoreError::AuthenticationFailed)?;
                peers.insert(peer_device_id);
            } else {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        if peers.len() > MAX_PROVIDER_UPLOAD_RECEIPT_PEERS {
            return Err(CoreError::ResourceLimit("provider upload receipt peers"));
        }
        for peer_device_id in peers {
            self.validate_provider_upload_receipts_locked(peer_device_id)?;
        }
        if migrated_legacy {
            sync_directory(&root)?;
        }
        Ok(())
    }

    fn load_recovery_capsule_upload_locked(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<RecoveryCapsuleUpload, CoreError> {
        let directory = self
            .recovery_capsule_upload_directory(
                lease,
                false,
                "open recovery capsule upload metadata directory",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let metadata: RecoveryCapsuleUpload = serde_json::from_slice(
            &self
                .read_anchored_private_file_bounded(
                    &directory,
                    "metadata.json",
                    MAX_PROVIDER_LEASE_STATE_BYTES as u64,
                    "read recovery capsule upload metadata",
                )?
                .ok_or(CoreError::AuthenticationFailed)?,
        )?;
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
        let (staged_bytes, staged_objects) = state
            .staged_capsule_upload
            .as_ref()
            .filter(|staged| staged.committed_created.is_none())
            .map_or((0, 0), |staged| (staged.upload.total_bytes, 1));
        if state
            .consumed_new_bytes
            .checked_add(staged_bytes)
            .and_then(|value| value.checked_add(bytes))
            .is_none_or(|value| value > state.lease.max_new_bytes)
            || state
                .consumed_new_objects
                .checked_add(staged_objects)
                .and_then(|value| value.checked_add(objects))
                .is_none_or(|value| value > state.lease.max_new_objects)
        {
            return Err(CoreError::ResourceLimit("provider lease quota"));
        }
        Ok(())
    }

    fn persist_provider_lease_locked(&self, state: &ProviderLeaseState) -> Result<(), CoreError> {
        write_json_atomic(&self.provider_lease_path(&state.lease)?, state, true)
    }

    fn local_write_batch_journal_path(&self, journal: &LocalWriteBatchJournal) -> PathBuf {
        self.root
            .join("local-write-journal")
            .join(format!("{}.json", journal.journal_id))
    }

    fn complete_local_write_batch_journal_locked(
        &self,
        journal: &LocalWriteBatchJournal,
    ) -> Result<(), CoreError> {
        let path = self.local_write_batch_journal_path(journal);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(self.root.join("local-write-journal").as_path()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CoreError::Io {
                operation: "complete local write batch journal",
                path,
                source,
            }),
        }
    }

    fn reconcile_local_write_batch_locked(
        &self,
        journal: &LocalWriteBatchJournal,
    ) -> Result<(), CoreError> {
        if journal.schema_version != SNAPSHOT_SCHEMA_VERSION
            || journal.entries.is_empty()
            || journal.entries.len() > MAX_LOCAL_WRITE_BATCH_RECORDS
            || journal
                .entries
                .iter()
                .map(|entry| &entry.locator)
                .collect::<BTreeSet<_>>()
                .len()
                != journal.entries.len()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let total_bytes = journal.entries.iter().try_fold(0_u64, |total, entry| {
            validate_hex_locator(&entry.locator)?;
            if entry.record_bytes < provider_record_overhead() as u64
                || entry.record_bytes
                    > (self.maximum_chunk_size + provider_record_overhead()) as u64
                || entry.record_digest.len() != 64
                || entry
                    .record_digest
                    .bytes()
                    .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
            {
                return Err(CoreError::AuthenticationFailed);
            }
            total
                .checked_add(entry.record_bytes)
                .ok_or(CoreError::ResourceLimit("local write batch"))
        })?;
        if total_bytes > MAX_LOCAL_WRITE_BATCH_BYTES as u64 {
            return Err(CoreError::ResourceLimit("local write batch"));
        }

        let mut present = Vec::new();
        for entry in &journal.entries {
            let path = self.chunk_path(&entry.locator)?;
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    let (record_bytes, record_digest) = hash_file_bounded(
                        &path,
                        (self.maximum_chunk_size + provider_record_overhead()) as u64,
                        "reconcile local write batch",
                    )?;
                    if record_bytes != entry.record_bytes || record_digest != entry.record_digest {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    present.push(path);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(CoreError::AuthenticationFailed),
            }
        }
        if present.len() != journal.entries.len() {
            let mut parents = BTreeSet::new();
            for path in present {
                let parent = path.parent().ok_or_else(|| {
                    CoreError::InvalidState("chunk path has no parent".to_owned())
                })?;
                parents.insert(parent.to_path_buf());
                fs::remove_file(&path).map_err(|source| CoreError::Io {
                    operation: "roll back partial local write batch",
                    path,
                    source,
                })?;
            }
            for parent in parents {
                sync_directory(&parent)?;
            }
        }
        self.complete_local_write_batch_journal_locked(journal)
    }

    fn recover_local_write_batch_journals(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        for entry in read_directory_sorted_bounded(
            &self.root.join("local-write-journal"),
            MAX_LOCAL_WRITE_JOURNALS,
            "local write journals",
        )? {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect local write batch journal",
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let journal: LocalWriteBatchJournal =
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
            if path != self.local_write_batch_journal_path(&journal) {
                return Err(CoreError::AuthenticationFailed);
            }
            self.reconcile_local_write_batch_locked(&journal)?;
        }
        Ok(())
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
            deferred_reference_candidate: false,
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

    fn provider_upload_batch_journal_path(&self, journal: &ProviderUploadBatchJournal) -> PathBuf {
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

    fn complete_provider_upload_batch_journal_locked(
        &self,
        journal: &ProviderUploadBatchJournal,
    ) -> Result<(), CoreError> {
        let path = self.provider_upload_batch_journal_path(journal);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(self.root.join("provider-upload-journal").as_path()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CoreError::Io {
                operation: "complete provider upload batch journal",
                path,
                source,
            }),
        }
    }

    fn reconcile_provider_upload_locked(
        &self,
        journal: &ProviderUploadJournal,
    ) -> Result<(), CoreError> {
        if journal.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || journal.started_at_unix_ms > journal.lease.expires_at_unix_ms
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let incumbent = match &journal.object {
            ProviderUploadKind::Chunk { locator } => {
                let object_path = self.chunk_path(locator)?;
                match fs::symlink_metadata(&object_path) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        Some(hash_file_bounded(
                            &object_path,
                            (self.maximum_chunk_size + provider_record_overhead()) as u64,
                            "reconcile provider upload",
                        )?)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    _ => return Err(CoreError::AuthenticationFailed),
                }
            }
            ProviderUploadKind::RecoveryCapsule { snapshot_id } => {
                let directory = match self.recovery_capsule_directory(
                    journal.lease.peer_device_id,
                    journal.lease.backup_id,
                    false,
                    "open recovery capsule during upload reconciliation",
                )? {
                    Some(directory) => directory,
                    None => return Ok(()),
                };
                self.hash_anchored_private_file_bounded(
                    &directory,
                    &format!("{snapshot_id}.json"),
                    MAX_RECOVERY_CAPSULE_BYTES as u64,
                    "reconcile provider recovery capsule upload",
                )?
            }
        };
        let Some((record_bytes, record_digest)) = incumbent else {
            return Ok(());
        };
        if record_bytes != journal.record_bytes || record_digest != journal.record_digest {
            return Err(CoreError::AuthenticationFailed);
        }
        let lease_path = self.provider_lease_path(&journal.lease)?;
        let mut state: ProviderLeaseState =
            serde_json::from_slice(&read_bounded(&lease_path, MAX_PROVIDER_LEASE_STATE_BYTES)?)?;
        if state.lease != journal.lease || state.cancelled || !valid_staged_capsule_upload(&state) {
            return Err(CoreError::AuthenticationFailed);
        }
        let matching_staged_capsule = match (
            &journal.object,
            &journal.recovery_capsule_descriptor,
            &state.staged_capsule_upload,
        ) {
            (
                ProviderUploadKind::RecoveryCapsule { snapshot_id },
                Some(journal_descriptor),
                Some(staged),
            ) if staged.upload.total_bytes == journal.record_bytes
                && staged.upload.capsule_digest == journal.record_digest
                && staged.upload.descriptor.as_ref() == Some(journal_descriptor)
                && journal_descriptor.snapshot_id == *snapshot_id =>
            {
                if staged.expected_new_object != journal.expected_new_object {
                    return Err(CoreError::AuthenticationFailed);
                }
                true
            }
            _ => false,
        };
        let effective_object_key = match (&journal.object, &journal.recovery_capsule_descriptor) {
            (ProviderUploadKind::Chunk { .. }, None) => journal.object_key.clone(),
            (ProviderUploadKind::RecoveryCapsule { snapshot_id }, Some(descriptor)) => {
                let canonical = provider_capsule_object_key(
                    journal.lease.peer_device_id,
                    journal.lease.backup_id,
                    snapshot_id,
                );
                let legacy =
                    legacy_provider_capsule_object_key(journal.lease.backup_id, snapshot_id);
                if journal.object_key != canonical && journal.object_key != legacy {
                    return Err(CoreError::AuthenticationFailed);
                }
                normalize_provider_capsule_object_key(&mut state, descriptor)?
            }
            _ => return Err(CoreError::AuthenticationFailed),
        };
        if let Some(record_bytes) = state.objects.get(&effective_object_key) {
            if *record_bytes != journal.record_bytes {
                return Err(CoreError::AuthenticationFailed);
            }
            if matching_staged_capsule {
                let staged = state
                    .staged_capsule_upload
                    .as_mut()
                    .ok_or(CoreError::AuthenticationFailed)?;
                if staged.committed_created.is_none() {
                    staged.committed_created = Some(staged.expected_new_object);
                    staged.completed_at_unix_ms = Some(journal.started_at_unix_ms);
                    self.persist_provider_lease_locked(&state)?;
                }
            }
            return Ok(());
        }
        if journal.expected_new_object {
            // A segmented capsule reserves its exact bytes and object durably at begin.
            // Journal recovery converts that reservation into consumed allocation instead
            // of admitting the same object a second time.
            if !matching_staged_capsule {
                self.ensure_lease_consumption_locked(&state, journal.record_bytes, 1)?;
            }
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
            let reference_result = self.add_provider_object_reference_locked(
                locator,
                journal.record_bytes,
                journal.lease.peer_device_id,
                journal.lease.backup_id,
            );
            if let Err(error) = reference_result {
                if !journal.deferred_reference_candidate {
                    return Err(error);
                }
                let path = self.provider_object_reference_path(locator)?;
                let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                    operation: "inspect interrupted deferred provider reference",
                    path: path.clone(),
                    source,
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(error);
                }
                fs::remove_file(&path).map_err(|source| CoreError::Io {
                    operation: "remove interrupted deferred provider reference",
                    path: path.clone(),
                    source,
                })?;
                self.add_provider_object_reference_locked(
                    locator,
                    journal.record_bytes,
                    journal.lease.peer_device_id,
                    journal.lease.backup_id,
                )?;
            }
        } else {
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
            .insert(effective_object_key, journal.record_bytes);
        if matching_staged_capsule {
            let staged = state
                .staged_capsule_upload
                .as_mut()
                .ok_or(CoreError::AuthenticationFailed)?;
            staged.committed_created = Some(staged.expected_new_object);
            staged.completed_at_unix_ms = Some(journal.started_at_unix_ms);
        }
        self.persist_provider_lease_locked(&state)
    }

    fn reconcile_provider_upload_batch_locked(
        &self,
        batch: &ProviderUploadBatchJournal,
    ) -> Result<(), CoreError> {
        if batch.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || batch.uploads.is_empty()
            || batch.uploads.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS
        {
            return Err(CoreError::AuthenticationFailed);
        }
        for journal in &batch.uploads {
            self.reconcile_provider_upload_locked(journal)?;
        }
        self.complete_provider_upload_batch_journal_locked(batch)
    }

    fn recover_provider_upload_journals(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.recover_provider_upload_journals_locked()
    }

    fn recover_provider_upload_journals_locked(&self) -> Result<(), CoreError> {
        for entry in read_directory_sorted_bounded(
            &self.root.join("provider-upload-journal"),
            MAX_PROVIDER_UPLOAD_JOURNALS,
            "provider upload journals",
        )? {
            let path = entry.path();
            let bytes = read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?;
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            if value.get("uploads").is_some() {
                let batch: ProviderUploadBatchJournal = serde_json::from_value(value)?;
                self.reconcile_provider_upload_batch_locked(&batch)?;
                continue;
            }
            let journal: ProviderUploadJournal = serde_json::from_value(value)?;
            self.reconcile_provider_upload_locked(&journal)?;
            fs::remove_file(&path).map_err(|source| CoreError::Io {
                operation: "reconcile provider upload journal",
                path: path.clone(),
                source,
            })?;
        }
        sync_directory(self.root.join("provider-upload-journal").as_path())
    }

    fn recover_recovery_capsule_uploads(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let mut expected = BTreeSet::new();
        for (_, mut state) in self.list_provider_lease_state_files_locked()? {
            let Some(staged) = state.staged_capsule_upload.clone() else {
                continue;
            };
            if self.finalize_committed_recovery_capsule_staging_locked(&mut state, now_unix_ms)? {
                continue;
            }
            if state.cancelled || state.lease.expires_at_unix_ms <= now_unix_ms {
                self.discard_recovery_capsule_staging_locked(&mut state, now_unix_ms)?;
                continue;
            }
            let directory = self.recovery_capsule_upload_path(&state.lease);
            self.ensure_recovery_capsule_upload_directory_locked(&staged.upload)?;
            self.validate_recovery_capsule_upload_directory_locked(&staged.upload)?;
            expected.insert(directory);
        }
        self.cleanup_orphan_recovery_capsule_uploads_locked(&expected)
    }

    fn validate_recovery_capsule_upload_directory_locked(
        &self,
        upload: &RecoveryCapsuleUpload,
    ) -> Result<(), CoreError> {
        let incumbent =
            self.load_recovery_capsule_upload_locked(&upload.lease, &upload.upload_id)?;
        if incumbent != *upload {
            return Err(CoreError::AuthenticationFailed);
        }
        let directory = self
            .recovery_capsule_upload_directory(
                &upload.lease,
                false,
                "open recovery capsule upload for validation",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        for entry in self.anchored_directory_entries_bounded(
            &directory,
            3,
            "recovery capsule upload entries",
        )? {
            let name = entry.to_str().ok_or(CoreError::AuthenticationFailed)?;
            match name {
                "metadata.json" => {
                    self.open_anchored_private_file_bounded(
                        &directory,
                        name,
                        MAX_PROVIDER_LEASE_STATE_BYTES as u64,
                        None,
                        "validate recovery capsule upload metadata",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?;
                }
                "segments" => {
                    let segments = self
                        .recovery_capsule_segments_directory(
                            &upload.lease,
                            false,
                            "open recovery capsule segments for validation",
                        )?
                        .ok_or(CoreError::AuthenticationFailed)?;
                    for segment in self.anchored_directory_entries_bounded(
                        &segments,
                        upload.total_segments as usize,
                        "recovery capsule upload segments",
                    )? {
                        let segment = segment.to_str().ok_or(CoreError::AuthenticationFailed)?;
                        let Some(index) = segment
                            .strip_suffix(".bin")
                            .filter(|value| value.len() == 8)
                            .and_then(|value| value.parse::<u32>().ok())
                        else {
                            return Err(CoreError::AuthenticationFailed);
                        };
                        let offset = u64::from(index)
                            .checked_mul(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64)
                            .ok_or(CoreError::ResourceLimit("recovery capsule segment"))?;
                        let expected_length = upload
                            .total_bytes
                            .saturating_sub(offset)
                            .min(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64);
                        if index >= upload.total_segments || expected_length == 0 {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        self.open_anchored_private_file_bounded(
                            &segments,
                            segment,
                            MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64,
                            Some(expected_length),
                            "validate recovery capsule segment",
                        )?
                        .ok_or(CoreError::AuthenticationFailed)?;
                    }
                }
                // Assembly is derived entirely from the authenticated immutable
                // segments. It is never restart truth, so discard it rather than
                // trusting a partial file left at an arbitrary write boundary.
                "assembled.tmp" => {
                    self.remove_anchored_file(
                        &directory,
                        name,
                        "discard interrupted recovery capsule assembly",
                    )?;
                }
                _ => return Err(CoreError::AuthenticationFailed),
            }
        }
        Ok(())
    }

    fn cleanup_orphan_recovery_capsule_uploads_locked(
        &self,
        expected: &BTreeSet<PathBuf>,
    ) -> Result<(), CoreError> {
        let root = self
            .anchored_directory(
                "provider-capsule-uploads",
                &[],
                false,
                "open provider capsule upload root",
            )?
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut backup_count = 0_usize;
        let mut lease_count = 0_usize;
        let mut orphans = Vec::new();
        for peer in self.anchored_directory_entries_bounded(
            &root,
            MAX_PROVIDER_CAPSULE_STAGING_PEERS,
            "provider capsule staging peers",
        )? {
            let peer = peer.to_str().ok_or(CoreError::AuthenticationFailed)?;
            let peer_device_id =
                DeviceId::from_str(peer).map_err(|_| CoreError::AuthenticationFailed)?;
            let peer_directory = self
                .anchored_directory(
                    "provider-capsule-uploads",
                    &[peer.to_owned()],
                    true,
                    "open provider capsule upload peer",
                )?
                .ok_or(CoreError::AuthenticationFailed)?;
            let remaining_backups =
                MAX_PROVIDER_CAPSULE_STAGING_BACKUPS.saturating_sub(backup_count);
            for backup in self.anchored_directory_entries_bounded(
                &peer_directory,
                remaining_backups,
                "provider capsule staging backups",
            )? {
                backup_count += 1;
                let backup = backup.to_str().ok_or(CoreError::AuthenticationFailed)?;
                let backup_id =
                    BackupId::from_str(backup).map_err(|_| CoreError::AuthenticationFailed)?;
                let backup_directory = self
                    .anchored_directory(
                        "provider-capsule-uploads",
                        &[peer.to_owned(), backup.to_owned()],
                        true,
                        "open provider capsule upload backup",
                    )?
                    .ok_or(CoreError::AuthenticationFailed)?;
                let remaining_leases =
                    MAX_PROVIDER_CAPSULE_STAGING_LEASES.saturating_sub(lease_count);
                for lease in self.anchored_directory_entries_bounded(
                    &backup_directory,
                    remaining_leases,
                    "provider capsule staging leases",
                )? {
                    lease_count += 1;
                    let lease_id = lease.to_str().ok_or(CoreError::AuthenticationFailed)?;
                    validate_upload_id(lease_id)?;
                    let lease_path = backup_directory.path.join(lease_id);
                    if !expected.contains(&lease_path) {
                        orphans.push((peer_device_id, backup_id, lease_id.to_owned()));
                    }
                }
            }
        }
        for (peer_device_id, backup_id, lease_id) in orphans {
            self.cleanup_recovery_capsule_upload_parts_locked(
                peer_device_id,
                backup_id,
                &lease_id,
            )?;
        }
        Ok(())
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
        self.add_provider_object_reference_with_durability_locked(
            locator,
            record_bytes,
            peer_device_id,
            backup_id,
            true,
        )
        .map(|_| ())
    }

    fn add_provider_object_reference_deferred_locked(
        &self,
        locator: &str,
        record_bytes: u64,
        peer_device_id: DeviceId,
        backup_id: BackupId,
    ) -> Result<bool, CoreError> {
        self.add_provider_object_reference_with_durability_locked(
            locator,
            record_bytes,
            peer_device_id,
            backup_id,
            false,
        )
    }

    fn add_provider_object_reference_with_durability_locked(
        &self,
        locator: &str,
        record_bytes: u64,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        sync_parent: bool,
    ) -> Result<bool, CoreError> {
        let path = self.provider_object_reference_path(locator)?;
        let mut reference_was_absent = false;
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                reference_was_absent = true;
                ProviderObjectReference {
                    schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                    locator: locator.to_owned(),
                    record_bytes,
                    owners: BTreeSet::new(),
                }
            }
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
        if !reference.owners.insert(ProviderObjectOwner {
            peer_device_id,
            backup_id,
        }) {
            return Ok(false);
        }
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("provider object reference has no parent".to_owned())
        })?;
        ensure_private_directory(parent)?;
        if sync_parent || !reference_was_absent {
            write_json_atomic(&path, &reference, true)?;
            Ok(false)
        } else {
            write_json_atomic_deferred_sync(&path, &reference)?;
            Ok(true)
        }
    }

    fn ensure_provider_lease_references_durable_locked(
        &self,
        state: &mut ProviderLeaseState,
    ) -> Result<(), CoreError> {
        let mut parents = BTreeSet::new();
        for locator in &state.deferred_reference_sync {
            let object_key = format!("chunk:{locator}");
            let record_bytes = state
                .objects
                .get(&object_key)
                .copied()
                .ok_or(CoreError::AuthenticationFailed)?;
            let path = self.provider_object_reference_path(locator)?;
            if let Err(error) = self.add_provider_object_reference_locked(
                locator,
                record_bytes,
                state.lease.peer_device_id,
                state.lease.backup_id,
            ) {
                let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                    operation: "inspect deferred provider reference during recovery",
                    path: path.clone(),
                    source,
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(error);
                }
                fs::remove_file(&path).map_err(|source| CoreError::Io {
                    operation: "remove incomplete deferred provider reference",
                    path: path.clone(),
                    source,
                })?;
                self.add_provider_object_reference_locked(
                    locator,
                    record_bytes,
                    state.lease.peer_device_id,
                    state.lease.backup_id,
                )?;
            }
            fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(|source| CoreError::Io {
                    operation: "sync deferred provider reference",
                    path: path.clone(),
                    source,
                })?;
            let parent = path.parent().ok_or_else(|| {
                CoreError::InvalidState("provider object reference has no parent".to_owned())
            })?;
            parents.insert(parent.to_path_buf());
        }
        for parent in parents {
            sync_directory(&parent)?;
        }
        if !state.deferred_reference_sync.is_empty() {
            state.deferred_reference_sync.clear();
            self.persist_provider_lease_locked(state)?;
        }
        Ok(())
    }

    fn recover_provider_lease_references(&self) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        for (_, mut state) in self.list_provider_lease_state_files_locked()? {
            self.ensure_provider_lease_references_durable_locked(&mut state)?;
        }
        Ok(())
    }

    fn list_provider_lease_state_files_locked(
        &self,
    ) -> Result<Vec<(PathBuf, ProviderLeaseState)>, CoreError> {
        let root = self.root.join("provider-leases");
        let peers = read_directory_sorted_bounded(
            &root,
            MAX_RETAINED_PROVIDER_LEASE_SCOPES,
            "provider lease peer directories",
        )?;
        let mut states = Vec::new();
        let mut states_per_peer = BTreeMap::<DeviceId, usize>::new();
        for peer in peers {
            let peer_path = peer.path();
            let metadata = fs::symlink_metadata(&peer_path).map_err(|source| CoreError::Io {
                operation: "inspect provider lease peer directory",
                path: peer_path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CoreError::InvalidState(
                    "invalid provider lease peer directory".to_owned(),
                ));
            }
            let peer_device_id = DeviceId::from_str(&peer.file_name().to_string_lossy())
                .map_err(|_| CoreError::AuthenticationFailed)?;
            let backups = read_directory_sorted_bounded(
                &peer_path,
                MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER,
                "provider lease backup directories",
            )?;
            for backup in backups {
                let backup_path = backup.path();
                let metadata =
                    fs::symlink_metadata(&backup_path).map_err(|source| CoreError::Io {
                        operation: "inspect provider lease backup directory",
                        path: backup_path.clone(),
                        source,
                    })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CoreError::InvalidState(
                        "invalid provider lease backup directory".to_owned(),
                    ));
                }
                let backup_id = BackupId::from_str(&backup.file_name().to_string_lossy())
                    .map_err(|_| CoreError::AuthenticationFailed)?;
                for entry in read_directory_sorted_bounded(
                    &backup_path,
                    MAX_PROVIDER_LEASE_STATE_FILES_PER_PEER,
                    "provider lease files per backup",
                )? {
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
                    if state.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
                        || !valid_provider_lease_shape(
                            &state.lease,
                            self.provider_quota_policy.maximum_lease_lifetime_ms,
                        )
                        || state.lease.peer_device_id != peer_device_id
                        || state.lease.backup_id != backup_id
                        || path.file_stem().and_then(|value| value.to_str())
                            != Some(state.lease.lease_id.as_str())
                        || state.consumed_new_bytes > state.lease.max_new_bytes
                        || state.consumed_new_objects > state.lease.max_new_objects
                        || state.consumed_new_objects > state.objects.len() as u64
                        || state.deferred_reference_sync.len() > state.objects.len()
                        || state.deferred_reference_sync.iter().any(|locator| {
                            validate_hex_locator(locator).is_err()
                                || !state.objects.contains_key(&format!("chunk:{locator}"))
                        })
                        || !valid_staged_capsule_upload(&state)
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    states.push((path, state));
                    let peer_states = states_per_peer.entry(peer_device_id).or_default();
                    *peer_states += 1;
                    if states.len() > MAX_PROVIDER_LEASE_STATE_FILES
                        || *peer_states > MAX_PROVIDER_LEASE_STATE_FILES_PER_PEER
                    {
                        return Err(CoreError::ResourceLimit("provider lease state files"));
                    }
                }
            }
        }
        Ok(states)
    }

    fn load_provider_lease_ledger_locked(&self) -> Result<ProviderLeaseLedger, CoreError> {
        let path = self.provider_lease_ledger_path();
        let ledger = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ProviderLeaseLedger::default()
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider lease ledger",
                    path,
                    source,
                });
            }
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() > 0
                    && metadata.len() <= MAX_PROVIDER_LEASE_LEDGER_BYTES as u64 =>
            {
                serde_json::from_slice(&read_bounded(&path, MAX_PROVIDER_LEASE_LEDGER_BYTES)?)?
            }
            Ok(_) => return Err(CoreError::AuthenticationFailed),
        };
        self.validate_provider_lease_ledger(&ledger)?;
        Ok(ledger)
    }

    fn validate_provider_lease_ledger(
        &self,
        ledger: &ProviderLeaseLedger,
    ) -> Result<(), CoreError> {
        if ledger.schema_version != PROVIDER_LEASE_SCHEMA_VERSION
            || ledger.pending_compactions.len() > MAX_PROVIDER_LEASE_STATE_FILES
            || ledger.peers.len() > MAX_RETAINED_PROVIDER_LEASE_SCOPES
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let mut scopes = 0_usize;
        for peer in ledger.peers.values() {
            if peer.backups.is_empty()
                || peer.backups.len() > MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER
            {
                return Err(CoreError::AuthenticationFailed);
            }
            scopes = scopes
                .checked_add(peer.backups.len())
                .ok_or(CoreError::ResourceLimit("provider lease ledger scopes"))?;
            for usage in peer.backups.values() {
                if usage.consumed_new_bytes == 0 && usage.consumed_new_objects == 0 {
                    return Err(CoreError::AuthenticationFailed);
                }
            }
        }
        if scopes > MAX_RETAINED_PROVIDER_LEASE_SCOPES {
            return Err(CoreError::ResourceLimit("provider lease ledger scopes"));
        }
        for pending in &ledger.pending_compactions {
            if pending.lease_id.is_empty()
                || pending.lease_id.len() > 128
                || !pending
                    .lease_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                || pending.state_digest.len() != 64
                || pending
                    .state_digest
                    .bytes()
                    .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        Ok(())
    }

    fn persist_provider_lease_ledger_locked(
        &self,
        ledger: &ProviderLeaseLedger,
    ) -> Result<(), CoreError> {
        self.validate_provider_lease_ledger(ledger)?;
        let bytes = serde_json::to_vec_pretty(ledger)?;
        if bytes.len() > MAX_PROVIDER_LEASE_LEDGER_BYTES {
            return Err(CoreError::ResourceLimit("provider lease ledger"));
        }
        write_atomic(&self.provider_lease_ledger_path(), &bytes, true)
    }

    fn finish_pending_provider_lease_compactions_locked(
        &self,
        ledger: &mut ProviderLeaseLedger,
    ) -> Result<(), CoreError> {
        if ledger.pending_compactions.is_empty() {
            return Ok(());
        }
        let mut parents = BTreeSet::new();
        for (index, pending) in ledger.pending_compactions.iter().enumerate() {
            let path = self.provider_lease_path_parts(
                pending.peer_device_id,
                pending.backup_id,
                &pending.lease_id,
            );
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    let bytes = read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?;
                    if blake3::hash(&bytes).to_hex().as_str() != pending.state_digest {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    fs::remove_file(&path).map_err(|source| CoreError::Io {
                        operation: "compact provider lease state",
                        path: path.clone(),
                        source,
                    })?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(CoreError::AuthenticationFailed),
            }
            if let Some(parent) = path.parent() {
                parents.insert(parent.to_path_buf());
            }
            if index == 0 {
                provider_lease_compaction_failpoint(2)?;
            }
        }
        provider_lease_compaction_failpoint(3)?;
        for parent in &parents {
            sync_directory(parent)?;
        }
        provider_lease_compaction_failpoint(4)?;
        for parent in parents {
            self.cleanup_empty_provider_lease_directories_locked(&parent)?;
        }
        ledger.pending_compactions.clear();
        self.persist_provider_lease_ledger_locked(ledger)
    }

    fn cleanup_empty_provider_lease_directories_locked(
        &self,
        backup_directory: &Path,
    ) -> Result<(), CoreError> {
        let Some(peer_directory) = backup_directory.parent() else {
            return Err(CoreError::InvalidState(
                "provider lease backup directory has no parent".to_owned(),
            ));
        };
        match fs::remove_dir(backup_directory) {
            Ok(()) => sync_directory(peer_directory)?,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "remove empty provider lease backup directory",
                    path: backup_directory.to_path_buf(),
                    source,
                });
            }
        }
        let lease_root = self.root.join("provider-leases");
        match fs::remove_dir(peer_directory) {
            Ok(()) => sync_directory(&lease_root),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) =>
            {
                Ok(())
            }
            Err(source) => Err(CoreError::Io {
                operation: "remove empty provider lease peer directory",
                path: peer_directory.to_path_buf(),
                source,
            }),
        }
    }

    fn compact_provider_leases(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        let _guard = self
            .transaction_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.compact_provider_leases_locked(now_unix_ms)
    }

    fn compact_provider_leases_locked(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        let mut ledger = self.load_provider_lease_ledger_locked()?;
        self.finish_pending_provider_lease_compactions_locked(&mut ledger)?;
        let states = self.list_provider_lease_state_files_locked()?;
        let mut retained_total = 0_usize;
        let mut retained_per_peer = BTreeMap::<DeviceId, usize>::new();
        let mut scope_count = ledger
            .peers
            .values()
            .map(|peer| peer.backups.len())
            .sum::<usize>();
        for (path, mut state) in states {
            if !state.cancelled && state.lease.expires_at_unix_ms > now_unix_ms {
                continue;
            }
            // Staging is temporary lease-owned data, never retained allocation. Remove it
            // durably before releasing or retaining an expired/cancelled lease reservation.
            self.discard_recovery_capsule_staging_locked(&mut state, now_unix_ms)?;
            let peer_has_scope = ledger
                .peers
                .get(&state.lease.peer_device_id)
                .is_some_and(|peer| peer.backups.contains_key(&state.lease.backup_id));
            let peer_scope_count = ledger
                .peers
                .get(&state.lease.peer_device_id)
                .map_or(0, |peer| peer.backups.len());
            let needs_scope = (state.consumed_new_bytes != 0 || state.consumed_new_objects != 0)
                && !peer_has_scope;
            if needs_scope
                && (scope_count >= MAX_RETAINED_PROVIDER_LEASE_SCOPES
                    || peer_scope_count >= MAX_RETAINED_PROVIDER_LEASE_SCOPES_PER_PEER)
            {
                retained_total += 1;
                *retained_per_peer
                    .entry(state.lease.peer_device_id)
                    .or_default() += 1;
                continue;
            }
            // Fast provider batches defer only shard-directory synchronization.
            // The signed lease state and upload journal are already durable.
            // Before the lease is compacted away, force every reference shard
            // durable so it becomes the permanent authorization and GC root.
            self.ensure_provider_lease_references_durable_locked(&mut state)?;
            if state.consumed_new_bytes != 0 || state.consumed_new_objects != 0 {
                let usage = ledger
                    .peers
                    .entry(state.lease.peer_device_id)
                    .or_default()
                    .backups
                    .entry(state.lease.backup_id)
                    .or_default();
                if needs_scope {
                    scope_count += 1;
                }
                usage.consumed_new_bytes = usage
                    .consumed_new_bytes
                    .checked_add(state.consumed_new_bytes)
                    .ok_or(CoreError::ResourceLimit("provider allocated bytes"))?;
                usage.consumed_new_objects = usage
                    .consumed_new_objects
                    .checked_add(state.consumed_new_objects)
                    .ok_or(CoreError::ResourceLimit("provider allocated objects"))?;
            }
            let bytes = read_bounded(&path, MAX_PROVIDER_LEASE_STATE_BYTES)?;
            ledger.pending_compactions.push(ProviderLeaseCompaction {
                peer_device_id: state.lease.peer_device_id,
                backup_id: state.lease.backup_id,
                lease_id: state.lease.lease_id,
                state_digest: blake3::hash(&bytes).to_hex().to_string(),
            });
        }
        if retained_total > MAX_RETAINED_PROVIDER_LEASES
            || retained_per_peer
                .values()
                .any(|count| *count > MAX_RETAINED_PROVIDER_LEASES_PER_PEER)
        {
            return Err(CoreError::ResourceLimit("retained provider leases"));
        }
        if !ledger.pending_compactions.is_empty() {
            self.persist_provider_lease_ledger_locked(&ledger)?;
            provider_lease_compaction_failpoint(1)?;
            self.finish_pending_provider_lease_compactions_locked(&mut ledger)?;
        }
        self.validate_provider_lease_inventory_locked(now_unix_ms)
    }

    fn validate_provider_lease_inventory_locked(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        let mut active_total = 0_usize;
        let mut retained_total = 0_usize;
        let mut active_per_peer = BTreeMap::<DeviceId, usize>::new();
        let mut retained_per_peer = BTreeMap::<DeviceId, usize>::new();
        for (_, state) in self.list_provider_lease_state_files_locked()? {
            let (total, per_peer) =
                if !state.cancelled && state.lease.expires_at_unix_ms > now_unix_ms {
                    (&mut active_total, &mut active_per_peer)
                } else {
                    (&mut retained_total, &mut retained_per_peer)
                };
            *total += 1;
            *per_peer.entry(state.lease.peer_device_id).or_default() += 1;
        }
        if active_total > MAX_ACTIVE_PROVIDER_LEASES
            || active_per_peer
                .values()
                .any(|count| *count > MAX_ACTIVE_PROVIDER_LEASES_PER_PEER)
            || retained_total > MAX_RETAINED_PROVIDER_LEASES
            || retained_per_peer
                .values()
                .any(|count| *count > MAX_RETAINED_PROVIDER_LEASES_PER_PEER)
        {
            return Err(CoreError::ResourceLimit("provider lease inventory"));
        }
        Ok(())
    }

    fn ensure_provider_lease_admission_locked(
        &self,
        now_unix_ms: u64,
        peer_device_id: DeviceId,
    ) -> Result<(), CoreError> {
        let mut active_total = 0_usize;
        let mut active_peer = 0_usize;
        for (_, state) in self.list_provider_lease_state_files_locked()? {
            if !state.cancelled && state.lease.expires_at_unix_ms > now_unix_ms {
                active_total += 1;
                if state.lease.peer_device_id == peer_device_id {
                    active_peer += 1;
                }
            }
        }
        if active_total >= MAX_ACTIVE_PROVIDER_LEASES
            || active_peer >= MAX_ACTIVE_PROVIDER_LEASES_PER_PEER
        {
            return Err(CoreError::ResourceLimit("active provider leases"));
        }
        Ok(())
    }

    fn provider_capacity_locked(
        &self,
        now_unix_ms: u64,
        peer_filter: Option<DeviceId>,
        backup_filter: Option<BackupId>,
    ) -> Result<ProviderCapacity, CoreError> {
        self.compact_provider_leases_locked(now_unix_ms)?;
        let ledger = self.load_provider_lease_ledger_locked()?;
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
        for (peer_device_id, peer) in &ledger.peers {
            for (backup_id, usage) in &peer.backups {
                checked_provider_counter(
                    &mut total_used_bytes,
                    usage.consumed_new_bytes,
                    "provider allocated bytes",
                )?;
                checked_provider_counter(
                    &mut total_used_objects,
                    usage.consumed_new_objects,
                    "provider allocated objects",
                )?;
                if peer_filter == Some(*peer_device_id) {
                    checked_provider_counter(
                        &mut peer_used_bytes,
                        usage.consumed_new_bytes,
                        "provider peer allocated bytes",
                    )?;
                    checked_provider_counter(
                        &mut peer_used_objects,
                        usage.consumed_new_objects,
                        "provider peer allocated objects",
                    )?;
                    if backup_filter == Some(*backup_id) {
                        checked_provider_counter(
                            &mut backup_used_bytes,
                            usage.consumed_new_bytes,
                            "provider backup allocated bytes",
                        )?;
                        checked_provider_counter(
                            &mut backup_used_objects,
                            usage.consumed_new_objects,
                            "provider backup allocated objects",
                        )?;
                    }
                }
            }
        }
        for (_, state) in self.list_provider_lease_state_files_locked()? {
            checked_provider_counter(
                &mut total_used_bytes,
                state.consumed_new_bytes,
                "provider allocated bytes",
            )?;
            checked_provider_counter(
                &mut total_used_objects,
                state.consumed_new_objects,
                "provider allocated objects",
            )?;
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
            checked_provider_counter(
                &mut total_reserved_bytes,
                reserved_bytes,
                "provider reserved bytes",
            )?;
            checked_provider_counter(
                &mut total_reserved_objects,
                reserved_objects,
                "provider reserved objects",
            )?;
            if peer_filter == Some(state.lease.peer_device_id) {
                checked_provider_counter(
                    &mut peer_used_bytes,
                    state.consumed_new_bytes,
                    "provider peer allocated bytes",
                )?;
                checked_provider_counter(
                    &mut peer_used_objects,
                    state.consumed_new_objects,
                    "provider peer allocated objects",
                )?;
                checked_provider_counter(
                    &mut peer_reserved_bytes,
                    reserved_bytes,
                    "provider peer reserved bytes",
                )?;
                checked_provider_counter(
                    &mut peer_reserved_objects,
                    reserved_objects,
                    "provider peer reserved objects",
                )?;
                if backup_filter == Some(state.lease.backup_id) {
                    checked_provider_counter(
                        &mut backup_used_bytes,
                        state.consumed_new_bytes,
                        "provider backup allocated bytes",
                    )?;
                    checked_provider_counter(
                        &mut backup_used_objects,
                        state.consumed_new_objects,
                        "provider backup allocated objects",
                    )?;
                    checked_provider_counter(
                        &mut backup_reserved_bytes,
                        reserved_bytes,
                        "provider backup reserved bytes",
                    )?;
                    checked_provider_counter(
                        &mut backup_reserved_objects,
                        reserved_objects,
                        "provider backup reserved objects",
                    )?;
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
            allocated_bytes: total_used_bytes,
            quota_bytes: total_used_bytes
                .checked_add(total_reserved_bytes)
                .and_then(|value| value.checked_add(available_bytes))
                .ok_or(CoreError::ResourceLimit("provider quota bytes"))?,
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

fn validate_upload_id(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(CoreError::InvalidState(
            "invalid provider upload id".to_owned(),
        ));
    }
    Ok(())
}

fn same_recovery_capsule_upload_request(
    incumbent: &RecoveryCapsuleUpload,
    requested: &RecoveryCapsuleUpload,
) -> bool {
    incumbent.schema_version == requested.schema_version
        && incumbent.upload_id == requested.upload_id
        && incumbent.lease == requested.lease
        && incumbent.total_bytes == requested.total_bytes
        && incumbent.total_segments == requested.total_segments
        && incumbent.capsule_digest == requested.capsule_digest
        && incumbent.descriptor == requested.descriptor
}

fn valid_provider_lease_shape(lease: &StorageLease, maximum_lifetime_ms: u64) -> bool {
    lease.schema_version == PROVIDER_LEASE_SCHEMA_VERSION
        && !lease.lease_id.is_empty()
        && lease.lease_id.len() <= 128
        && lease
            .lease_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && lease.max_new_bytes != 0
        && lease.max_new_objects != 0
        && lease.expires_at_unix_ms > lease.issued_at_unix_ms
        && lease.expires_at_unix_ms - lease.issued_at_unix_ms <= maximum_lifetime_ms
        && !lease.nonce.is_empty()
        && lease.nonce.len() <= 128
        && !lease.signature.is_empty()
}

fn valid_staged_capsule_upload(state: &ProviderLeaseState) -> bool {
    let Some(staged) = &state.staged_capsule_upload else {
        return true;
    };
    let upload = &staged.upload;
    let Some(descriptor) = &upload.descriptor else {
        return false;
    };
    let reservation_bytes = if staged.committed_created.is_none() {
        upload.total_bytes
    } else {
        0
    };
    let reservation_objects = u64::from(staged.committed_created.is_none());
    upload.schema_version == PROVIDER_LEASE_SCHEMA_VERSION
        && validate_upload_id(&upload.upload_id).is_ok()
        && upload.lease == state.lease
        && upload.total_bytes != 0
        && upload.total_bytes <= MAX_RECOVERY_CAPSULE_BYTES as u64
        && upload.total_segments != 0
        && upload.total_segments <= MAX_RECOVERY_CAPSULE_SEGMENTS
        && u64::from(upload.total_segments)
            == upload
                .total_bytes
                .div_ceil(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64)
        && validate_hex_locator(&upload.capsule_digest).is_ok()
        && upload.created_at_unix_ms >= state.lease.issued_at_unix_ms
        && upload.created_at_unix_ms < state.lease.expires_at_unix_ms
        && descriptor.backup_id == state.lease.backup_id
        && descriptor.signer_device_id == state.lease.peer_device_id
        && descriptor.total_bytes == upload.total_bytes
        && descriptor.capsule_digest == upload.capsule_digest
        && state
            .consumed_new_bytes
            .checked_add(reservation_bytes)
            .is_some_and(|value| value <= state.lease.max_new_bytes)
        && state
            .consumed_new_objects
            .checked_add(reservation_objects)
            .is_some_and(|value| value <= state.lease.max_new_objects)
        && match (staged.committed_created, staged.completed_at_unix_ms) {
            (None, None) => true,
            (Some(_), Some(completed_at_unix_ms)) => {
                completed_at_unix_ms >= state.lease.issued_at_unix_ms
                    && completed_at_unix_ms < state.lease.expires_at_unix_ms
            }
            // Schema-v1 staged uploads written before the completion timestamp was
            // introduced recover deterministically from their immutable creation time.
            (Some(_), None) => true,
            (None, Some(_)) => false,
        }
        && staged.committed_created.is_none_or(|_| {
            let canonical = provider_capsule_object_key(
                state.lease.peer_device_id,
                descriptor.backup_id,
                &descriptor.snapshot_id,
            );
            let legacy =
                legacy_provider_capsule_object_key(descriptor.backup_id, &descriptor.snapshot_id);
            state.objects.get(&canonical) == Some(&upload.total_bytes)
                || state.objects.get(&legacy) == Some(&upload.total_bytes)
        })
}

fn provider_capsule_object_key(
    owner_device_id: DeviceId,
    backup_id: BackupId,
    snapshot_id: &str,
) -> String {
    format!("capsule:{owner_device_id}:{backup_id}:{snapshot_id}")
}

fn legacy_provider_capsule_object_key(backup_id: BackupId, snapshot_id: &str) -> String {
    format!("capsule:{backup_id}:{snapshot_id}")
}

fn normalize_provider_capsule_object_key(
    state: &mut ProviderLeaseState,
    descriptor: &RecoveryCapsuleDescriptor,
) -> Result<String, CoreError> {
    let canonical = provider_capsule_object_key(
        state.lease.peer_device_id,
        descriptor.backup_id,
        &descriptor.snapshot_id,
    );
    let legacy = legacy_provider_capsule_object_key(descriptor.backup_id, &descriptor.snapshot_id);
    let canonical_length = state.objects.get(&canonical).copied();
    let legacy_length = state.objects.get(&legacy).copied();
    match (canonical_length, legacy_length) {
        (Some(left), Some(right)) if left != right => {
            return Err(CoreError::AuthenticationFailed);
        }
        (Some(_), Some(_)) => {
            state.objects.remove(&legacy);
        }
        (None, Some(length)) => {
            state.objects.remove(&legacy);
            state.objects.insert(canonical.clone(), length);
        }
        (Some(_), None) | (None, None) => {}
    }
    Ok(canonical)
}

fn valid_recovery_capsule_upload_attempt(attempt: &RecoveryCapsuleUploadAttempt) -> bool {
    let phase_valid = match attempt.phase {
        RecoveryCapsuleUploadAttemptPhase::LeaseAcquired => true,
        RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment } => {
            next_segment <= attempt.total_segments
        }
        RecoveryCapsuleUploadAttemptPhase::CommitPending
        | RecoveryCapsuleUploadAttemptPhase::CommitAccepted => true,
    };
    attempt.schema_version == RECOVERY_CAPSULE_UPLOAD_ATTEMPT_SCHEMA_VERSION
        && validate_snapshot_id(&attempt.snapshot_id).is_ok()
        && validate_hex_locator(&attempt.capsule_digest).is_ok()
        && attempt.total_bytes != 0
        && attempt.total_bytes <= MAX_RECOVERY_CAPSULE_BYTES as u64
        && attempt.total_segments != 0
        && attempt.total_segments <= MAX_RECOVERY_CAPSULE_SEGMENTS
        && u64::from(attempt.total_segments)
            == attempt
                .total_bytes
                .div_ceil(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64)
        && validate_upload_id(&attempt.upload_id).is_ok()
        && attempt.lease.provider_device_id == attempt.provider_device_id
        && attempt.lease.backup_id == attempt.backup_id
        && attempt.lease.max_new_bytes == attempt.total_bytes
        && attempt.lease.max_new_objects == 1
        && valid_provider_lease_shape(&attempt.lease, 15 * 60 * 1_000)
        && phase_valid
}

fn valid_recovery_capsule_lease_intent(intent: &RecoveryCapsuleLeaseIntent) -> bool {
    intent.schema_version == RECOVERY_CAPSULE_LEASE_INTENT_SCHEMA_VERSION
        && validate_snapshot_id(&intent.snapshot_id).is_ok()
        && validate_hex_locator(&intent.capsule_digest).is_ok()
        && intent.total_bytes != 0
        && intent.total_bytes <= MAX_RECOVERY_CAPSULE_BYTES as u64
        && intent.total_segments != 0
        && intent.total_segments <= MAX_RECOVERY_CAPSULE_SEGMENTS
        && u64::from(intent.total_segments)
            == intent
                .total_bytes
                .div_ceil(MAX_RECOVERY_CAPSULE_SEGMENT_BYTES as u64)
        && validate_upload_id(&intent.upload_id).is_ok()
        && uuid::Uuid::parse_str(&intent.acquisition_id)
            .is_ok_and(|value| value.hyphenated().to_string() == intent.acquisition_id)
}

fn valid_provider_write_lease_intent(intent: &ProviderWriteLeaseIntent) -> bool {
    intent.schema_version == PROVIDER_WRITE_LEASE_INTENT_SCHEMA_VERSION
        && intent.maximum_new_bytes != 0
        && intent.maximum_new_objects != 0
        && uuid::Uuid::parse_str(&intent.acquisition_id)
            .is_ok_and(|value| value.hyphenated().to_string() == intent.acquisition_id)
}

fn provider_write_lease_intent_name(provider_device_id: DeviceId, backup_id: BackupId) -> String {
    format!(
        "{}.json",
        blake3::hash(format!("{provider_device_id}:{backup_id}").as_bytes()).to_hex()
    )
}

fn valid_recovery_capsule_descriptor(descriptor: &RecoveryCapsuleDescriptor) -> bool {
    validate_snapshot_id(&descriptor.snapshot_id).is_ok()
        && descriptor.key_epoch != 0
        && descriptor.total_bytes != 0
        && descriptor.total_bytes <= MAX_RECOVERY_CAPSULE_BYTES as u64
        && validate_hex_locator(&descriptor.capsule_digest).is_ok()
}

fn recovery_capsule_page_cursor(
    owner_device_id: DeviceId,
    backup_id: Option<BackupId>,
    generation: &str,
    sequence: u64,
) -> String {
    let scope = blake3::hash(
        format!(
            "{owner_device_id}:{}",
            backup_id.map_or_else(|| "all".to_owned(), |backup_id| backup_id.to_string())
        )
        .as_bytes(),
    );
    format!(
        "{sequence:020}-{}-{generation}",
        &scope.to_hex().as_str()[..16]
    )
}

fn parse_recovery_capsule_page_cursor(
    cursor: &str,
    owner_device_id: DeviceId,
    backup_id: Option<BackupId>,
    generation: &str,
) -> Result<u64, CoreError> {
    let bytes = cursor.as_bytes();
    if bytes.len() != 70
        || bytes.get(20) != Some(&b'-')
        || bytes.get(37) != Some(&b'-')
        || bytes[..20].iter().any(|byte| !byte.is_ascii_digit())
    {
        return Err(CoreError::InvalidState(
            "invalid recovery capsule cursor".to_owned(),
        ));
    }
    let sequence = std::str::from_utf8(&bytes[..20])
        .map_err(|_| CoreError::InvalidState("invalid recovery capsule cursor".to_owned()))?
        .parse::<u64>()
        .map_err(|_| CoreError::InvalidState("invalid recovery capsule cursor".to_owned()))?;
    if recovery_capsule_page_cursor(owner_device_id, backup_id, generation, sequence) != cursor {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(sequence)
}

fn valid_recovery_capsule_page_generation(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_recovery_capsule_upload_attempt_transition(
    incumbent: &RecoveryCapsuleUploadAttempt,
    requested: &RecoveryCapsuleUploadAttempt,
) -> bool {
    let identity_matches = RecoveryCapsuleUploadAttempt {
        phase: incumbent.phase.clone(),
        ..requested.clone()
    } == *incumbent;
    identity_matches
        && match (&incumbent.phase, &requested.phase) {
            (
                RecoveryCapsuleUploadAttemptPhase::LeaseAcquired,
                RecoveryCapsuleUploadAttemptPhase::LeaseAcquired
                | RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment: 0 },
            ) => true,
            (
                RecoveryCapsuleUploadAttemptPhase::Uploading {
                    next_segment: incumbent_segment,
                },
                RecoveryCapsuleUploadAttemptPhase::Uploading {
                    next_segment: requested_segment,
                },
            ) => requested_segment >= incumbent_segment,
            (
                RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment },
                RecoveryCapsuleUploadAttemptPhase::CommitPending,
            ) => *next_segment == incumbent.total_segments,
            (
                RecoveryCapsuleUploadAttemptPhase::CommitPending,
                RecoveryCapsuleUploadAttemptPhase::CommitPending
                | RecoveryCapsuleUploadAttemptPhase::CommitAccepted,
            )
            | (
                RecoveryCapsuleUploadAttemptPhase::CommitAccepted,
                RecoveryCapsuleUploadAttemptPhase::CommitAccepted,
            ) => true,
            _ => false,
        }
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

fn valid_single_path_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && Path::new(value)
            .components()
            .eq(std::iter::once(std::path::Component::Normal(
                std::ffi::OsStr::new(value),
            )))
}

#[cfg(unix)]
fn anchored_io_error(operation: &'static str, path: &Path, error: rustix::io::Errno) -> CoreError {
    CoreError::Io {
        operation,
        path: path.to_path_buf(),
        source: std::io::Error::from_raw_os_error(error.raw_os_error()),
    }
}

fn recovery_capsule_cursor(capsule: &RecoveryCapsule) -> String {
    format!("{}/{}", capsule.backup_id, capsule.snapshot_id)
}

#[derive(Debug, Eq, PartialEq)]
struct ParsedRecoveryCapsuleHeader {
    backup_id: BackupId,
    snapshot_id: String,
    key_epoch: u64,
    committed_at_unix_ms: u64,
    signer_device_id: DeviceId,
    capsule_digest: String,
}

struct BoundedJsonReader<R> {
    reader: R,
    path: PathBuf,
    operation: &'static str,
    hasher: blake3::Hasher,
    consumed: u64,
}

impl<R: BufRead> BoundedJsonReader<R> {
    fn consume_buffered(&mut self, amount: usize) -> Result<(), CoreError> {
        let bytes = self.reader.fill_buf().map_err(|source| CoreError::Io {
            operation: self.operation,
            path: self.path.clone(),
            source,
        })?;
        if bytes.len() < amount {
            return Err(CoreError::AuthenticationFailed);
        }
        self.hasher.update(&bytes[..amount]);
        self.consumed = self
            .consumed
            .checked_add(amount as u64)
            .ok_or(CoreError::ResourceLimit("recovery capsule size"))?;
        self.reader.consume(amount);
        Ok(())
    }

    fn next_byte(&mut self) -> Result<Option<u8>, CoreError> {
        let byte = self
            .reader
            .fill_buf()
            .map_err(|source| CoreError::Io {
                operation: self.operation,
                path: self.path.clone(),
                source,
            })?
            .first()
            .copied();
        if byte.is_some() {
            self.consume_buffered(1)?;
        }
        Ok(byte)
    }

    fn peek_byte(&mut self) -> Result<Option<u8>, CoreError> {
        self.reader
            .fill_buf()
            .map_err(|source| CoreError::Io {
                operation: self.operation,
                path: self.path.clone(),
                source,
            })
            .map(|bytes| bytes.first().copied())
    }

    fn skip_whitespace(&mut self) -> Result<(), CoreError> {
        while self
            .peek_byte()?
            .is_some_and(|byte| matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.consume_buffered(1)?;
        }
        Ok(())
    }

    fn expect(&mut self, expected: u8) -> Result<(), CoreError> {
        if self.next_byte()? != Some(expected) {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }

    fn read_ascii_string(&mut self, maximum_bytes: usize) -> Result<String, CoreError> {
        self.expect(b'"')?;
        let mut value = Vec::with_capacity(maximum_bytes.min(128));
        loop {
            let byte = self.next_byte()?.ok_or(CoreError::AuthenticationFailed)?;
            match byte {
                b'"' => break,
                b'\\' | 0..=0x1f | 0x80..=u8::MAX => {
                    return Err(CoreError::AuthenticationFailed);
                }
                _ if value.len() == maximum_bytes => {
                    return Err(CoreError::ResourceLimit("recovery capsule header"));
                }
                _ => value.push(byte),
            }
        }
        String::from_utf8(value).map_err(|_| CoreError::AuthenticationFailed)
    }

    fn skip_string(&mut self) -> Result<(), CoreError> {
        self.expect(b'"')?;
        let mut utf8_tail = Vec::with_capacity(3);
        let mut utf8_buffer = Vec::with_capacity(64 * 1_024 + 3);
        loop {
            let bytes = self.reader.fill_buf().map_err(|source| CoreError::Io {
                operation: self.operation,
                path: self.path.clone(),
                source,
            })?;
            if bytes.is_empty() {
                return Err(CoreError::AuthenticationFailed);
            }
            let special = bytes
                .iter()
                .position(|byte| *byte == b'"' || *byte == b'\\' || *byte < 0x20);
            match special {
                None => {
                    let consumed = bytes.len();
                    validate_json_utf8_run(&bytes[..consumed], &mut utf8_tail, &mut utf8_buffer)?;
                    self.consume_buffered(consumed)?;
                }
                Some(index) => {
                    let byte = bytes[index];
                    validate_json_utf8_run(&bytes[..index], &mut utf8_tail, &mut utf8_buffer)?;
                    if !utf8_tail.is_empty() {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    self.consume_buffered(index + 1)?;
                    match byte {
                        b'"' => return Ok(()),
                        b'\\' => match self.next_byte()?.ok_or(CoreError::AuthenticationFailed)? {
                            b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {}
                            b'u' => {
                                let escaped = self.read_hex_quad()?;
                                if (0xd800..=0xdbff).contains(&escaped) {
                                    self.expect(b'\\')?;
                                    self.expect(b'u')?;
                                    if !(0xdc00..=0xdfff).contains(&self.read_hex_quad()?) {
                                        return Err(CoreError::AuthenticationFailed);
                                    }
                                } else if (0xdc00..=0xdfff).contains(&escaped) {
                                    return Err(CoreError::AuthenticationFailed);
                                }
                            }
                            _ => return Err(CoreError::AuthenticationFailed),
                        },
                        _ => return Err(CoreError::AuthenticationFailed),
                    }
                }
            }
        }
    }

    fn read_hex_quad(&mut self) -> Result<u16, CoreError> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let byte = self.next_byte()?.ok_or(CoreError::AuthenticationFailed)?;
            let digit = match byte {
                b'0'..=b'9' => u16::from(byte - b'0'),
                b'a'..=b'f' => u16::from(byte - b'a' + 10),
                b'A'..=b'F' => u16::from(byte - b'A' + 10),
                _ => return Err(CoreError::AuthenticationFailed),
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn read_u64(&mut self) -> Result<u64, CoreError> {
        let first = self
            .peek_byte()?
            .filter(u8::is_ascii_digit)
            .ok_or(CoreError::AuthenticationFailed)?;
        let mut value = 0_u64;
        let mut digits = 0_usize;
        while let Some(byte) = self.peek_byte()?.filter(u8::is_ascii_digit) {
            if first == b'0' && digits > 0 {
                return Err(CoreError::AuthenticationFailed);
            }
            self.consume_buffered(1)?;
            digits += 1;
            value = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
                .ok_or(CoreError::AuthenticationFailed)?;
        }
        Ok(value)
    }
}

fn validate_json_utf8_run(
    run: &[u8],
    tail: &mut Vec<u8>,
    buffer: &mut Vec<u8>,
) -> Result<(), CoreError> {
    buffer.clear();
    buffer.extend_from_slice(tail);
    buffer.extend_from_slice(run);
    match std::str::from_utf8(buffer) {
        Ok(_) => tail.clear(),
        Err(error) if error.error_len().is_none() => {
            let trailing = &buffer[error.valid_up_to()..];
            if trailing.len() > 3 {
                return Err(CoreError::AuthenticationFailed);
            }
            tail.clear();
            tail.extend_from_slice(trailing);
        }
        Err(_) => return Err(CoreError::AuthenticationFailed),
    }
    Ok(())
}

fn parse_recovery_capsule_header_file(
    file: fs::File,
    original_length: u64,
    path: &Path,
) -> Result<ParsedRecoveryCapsuleHeader, CoreError> {
    const HEADER_FIELD_COUNT: usize = 10;
    if original_length == 0 || original_length > MAX_RECOVERY_CAPSULE_BYTES as u64 {
        return Err(CoreError::AuthenticationFailed);
    }
    let mut parser = BoundedJsonReader {
        reader: BufReader::with_capacity(64 * 1_024, file),
        path: path.to_path_buf(),
        operation: "parse recovery capsule header",
        hasher: blake3::Hasher::new(),
        consumed: 0,
    };
    let mut seen = BTreeSet::new();
    let mut backup_id = None;
    let mut snapshot_id = None;
    let mut key_epoch = None;
    let mut committed_at_unix_ms = None;
    let mut signer_device_id = None;

    parser.skip_whitespace()?;
    parser.expect(b'{')?;
    parser.skip_whitespace()?;
    if parser.peek_byte()? == Some(b'}') {
        return Err(CoreError::AuthenticationFailed);
    }
    loop {
        let key = parser.read_ascii_string(32)?;
        if !seen.insert(key.clone()) {
            return Err(CoreError::AuthenticationFailed);
        }
        parser.skip_whitespace()?;
        parser.expect(b':')?;
        parser.skip_whitespace()?;
        match key.as_str() {
            "schemaVersion" => {
                u16::try_from(parser.read_u64()?).map_err(|_| CoreError::AuthenticationFailed)?;
            }
            "cipherSuite" | "nonce" | "ciphertext" | "signature" => {
                parser.skip_string()?;
            }
            "backupId" => {
                backup_id = Some(
                    BackupId::from_str(&parser.read_ascii_string(64)?)
                        .map_err(|_| CoreError::AuthenticationFailed)?,
                );
            }
            "snapshotId" => {
                let value = parser.read_ascii_string(128)?;
                validate_snapshot_id(&value).map_err(|_| CoreError::AuthenticationFailed)?;
                snapshot_id = Some(value);
            }
            "keyEpoch" => key_epoch = Some(parser.read_u64()?),
            "committedAtUnixMs" => committed_at_unix_ms = Some(parser.read_u64()?),
            "signerDeviceId" => {
                signer_device_id = Some(
                    DeviceId::from_str(&parser.read_ascii_string(64)?)
                        .map_err(|_| CoreError::AuthenticationFailed)?,
                );
            }
            _ => return Err(CoreError::AuthenticationFailed),
        }
        parser.skip_whitespace()?;
        match parser.next_byte()? {
            Some(b',') => parser.skip_whitespace()?,
            Some(b'}') => break,
            _ => return Err(CoreError::AuthenticationFailed),
        }
    }
    parser.skip_whitespace()?;
    let capsule_digest = parser.hasher.finalize().to_hex().to_string();
    if parser.peek_byte()?.is_some()
        || seen.len() != HEADER_FIELD_COUNT
        || parser.consumed != original_length
        || parser
            .reader
            .get_ref()
            .metadata()
            .map_err(|source| CoreError::Io {
                operation: "verify parsed recovery capsule",
                path: path.to_path_buf(),
                source,
            })?
            .len()
            != original_length
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(ParsedRecoveryCapsuleHeader {
        backup_id: backup_id.ok_or(CoreError::AuthenticationFailed)?,
        snapshot_id: snapshot_id.ok_or(CoreError::AuthenticationFailed)?,
        key_epoch: key_epoch.ok_or(CoreError::AuthenticationFailed)?,
        committed_at_unix_ms: committed_at_unix_ms.ok_or(CoreError::AuthenticationFailed)?,
        signer_device_id: signer_device_id.ok_or(CoreError::AuthenticationFailed)?,
        capsule_digest,
    })
}

fn read_private_regular_file_bounded(
    path: &Path,
    maximum_bytes: u64,
    operation: &'static str,
) -> Result<Vec<u8>, CoreError> {
    let (mut file, original_length) =
        open_private_regular_file_bounded(path, maximum_bytes, None, operation)?;
    let capacity = usize::try_from(original_length)
        .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
    (&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
    let final_length = file
        .metadata()
        .map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if bytes.len() as u64 != original_length || final_length != original_length {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(bytes)
}

fn hash_file_bounded(
    path: &Path,
    maximum_bytes: u64,
    operation: &'static str,
) -> Result<(u64, String), CoreError> {
    let (mut file, original_length) =
        open_private_regular_file_bounded(path, maximum_bytes, None, operation)?;
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
    let final_length = file
        .metadata()
        .map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if total != original_length || final_length != original_length {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok((total, hasher.finalize().to_hex().to_string()))
}

fn copy_file_exact_with_digest(
    source: &mut fs::File,
    destination: &mut fs::File,
    expected_bytes: u64,
    expected_digest: &str,
    source_path: &Path,
    operation: &'static str,
) -> Result<(), CoreError> {
    let mut total = 0_u64;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = source.read(&mut buffer).map_err(|source| CoreError::Io {
            operation,
            path: source_path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(CoreError::ResourceLimit("recovery capsule size"))?;
        if total > expected_bytes {
            return Err(CoreError::AuthenticationFailed);
        }
        hasher.update(&buffer[..read]);
        destination
            .write_all(&buffer[..read])
            .map_err(|source| CoreError::Io {
                operation,
                path: source_path.to_path_buf(),
                source,
            })?;
    }
    if total != expected_bytes
        || hasher.finalize().to_hex().as_str() != expected_digest
        || source
            .metadata()
            .map_err(|source| CoreError::Io {
                operation,
                path: source_path.to_path_buf(),
                source,
            })?
            .len()
            != expected_bytes
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn open_private_regular_file_bounded(
    path: &Path,
    maximum_bytes: u64,
    expected_bytes: Option<u64>,
    operation: &'static str,
) -> Result<(fs::File, u64), CoreError> {
    #[cfg(unix)]
    let (file, length) = {
        use std::os::unix::fs::MetadataExt as _;

        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        let stat = fstat(&descriptor).map_err(|error| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        let parent = path.parent().ok_or_else(|| {
            CoreError::InvalidState("private file has no parent directory".to_owned())
        })?;
        let parent_owner = fs::metadata(parent)
            .map_err(|source| CoreError::Io {
                operation,
                path: parent.to_path_buf(),
                source,
            })?
            .uid();
        let length = u64::try_from(stat.st_size)
            .map_err(|_| CoreError::ResourceLimit("persisted file size"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != parent_owner
            || stat.st_mode & 0o077 != 0
        {
            return Err(CoreError::AuthenticationFailed);
        }
        (fs::File::from(descriptor), length)
    };
    #[cfg(not(unix))]
    let (file, length) = {
        let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CoreError::AuthenticationFailed);
        }
        let file = fs::File::open(path).map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        let handle_metadata = file.metadata().map_err(|source| CoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        })?;
        if !handle_metadata.is_file() || handle_metadata.len() != metadata.len() {
            return Err(CoreError::AuthenticationFailed);
        }
        (file, handle_metadata.len())
    };
    if length == 0
        || length > maximum_bytes
        || expected_bytes.is_some_and(|expected| expected != length)
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok((file, length))
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

fn read_directory_sorted_bounded(
    path: &Path,
    maximum: usize,
    resource: &'static str,
) -> Result<Vec<fs::DirEntry>, CoreError> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).map_err(|source| CoreError::Io {
        operation: "read bounded store directory",
        path: path.to_path_buf(),
        source,
    })? {
        entries.push(entry.map_err(|source| CoreError::Io {
            operation: "read bounded store directory entry",
            path: path.to_path_buf(),
            source,
        })?);
        if entries.len() > maximum {
            return Err(CoreError::ResourceLimit(resource));
        }
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn provider_wall_clock_unix_ms() -> Result<u64, CoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CoreError::InvalidState("system clock predates Unix epoch".to_owned()))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| CoreError::ResourceLimit("provider lease clock"))
}

fn checked_provider_counter(
    total: &mut u64,
    value: u64,
    resource: &'static str,
) -> Result<(), CoreError> {
    *total = total
        .checked_add(value)
        .ok_or(CoreError::ResourceLimit(resource))?;
    Ok(())
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
        if metadata.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                CoreError::Io {
                    operation: "protect private storage directory",
                    path: path.to_path_buf(),
                    source,
                }
            })?;
        }
    }
    Ok(())
}

fn remove_empty_directory(path: &Path, parent: &Path) -> Result<(), CoreError> {
    match fs::remove_dir(path) {
        Ok(()) => sync_directory(parent),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(source) => Err(CoreError::Io {
            operation: "remove empty provider staging directory",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json_atomic_deferred_sync(path: &Path, value: &impl Serialize) -> Result<(), CoreError> {
    let parent = path.parent().ok_or_else(|| {
        CoreError::InvalidState("provider object reference has no parent".to_owned())
    })?;
    let bytes = serde_json::to_vec_pretty(value)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "create deferred provider reference staging file",
            path: parent.to_path_buf(),
            source,
        })?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.flush())
        .map_err(|source| CoreError::Io {
            operation: "write deferred provider reference staging file",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect deferred provider reference staging file",
                path: path.to_path_buf(),
                source,
            })?;
    }
    temporary.persist(path).map_err(|error| CoreError::Io {
        operation: "commit deferred provider reference",
        path: path.to_path_buf(),
        source: error.error,
    })?;
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

    fn test_provider_lease(
        peer_device_id: DeviceId,
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        issued_at_unix_ms: u64,
        expires_at_unix_ms: u64,
    ) -> StorageLease {
        StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id: DeviceId::new(),
            backup_id,
            max_new_bytes,
            max_new_objects,
            issued_at_unix_ms,
            expires_at_unix_ms,
            nonce: uuid::Uuid::new_v4().to_string(),
            signature: "test-signature".to_owned(),
        }
    }

    fn test_capsule_descriptor(
        peer_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        total_bytes: u64,
        capsule_digest: String,
        committed_at_unix_ms: u64,
    ) -> RecoveryCapsuleDescriptor {
        RecoveryCapsuleDescriptor {
            backup_id,
            snapshot_id: snapshot_id.to_owned(),
            key_epoch: 1,
            committed_at_unix_ms,
            signer_device_id: peer_device_id,
            total_bytes,
            capsule_digest,
        }
    }

    fn read_test_provider_lease_state(
        store: &ChunkStore,
        lease: &StorageLease,
    ) -> ProviderLeaseState {
        serde_json::from_slice(
            &read_bounded(
                &store
                    .provider_lease_path(lease)
                    .expect("provider lease path"),
                MAX_PROVIDER_LEASE_STATE_BYTES,
            )
            .expect("provider lease state"),
        )
        .expect("provider lease json")
    }

    fn test_recovery_capsule(
        peer_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        committed_at_unix_ms: u64,
    ) -> RecoveryCapsule {
        RecoveryCapsule {
            schema_version: 1,
            cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
            backup_id,
            snapshot_id: snapshot_id.to_owned(),
            key_epoch: 1,
            committed_at_unix_ms,
            nonce: "opaque".to_owned(),
            ciphertext: "opaque".to_owned(),
            signer_device_id: peer_device_id,
            signature: "opaque".to_owned(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn stage_test_recovery_capsule(
        store: &ChunkStore,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        lease: &StorageLease,
        upload_id: &str,
        capsule: &RecoveryCapsule,
        descriptor: &RecoveryCapsuleDescriptor,
        now_unix_ms: u64,
    ) -> Vec<u8> {
        let bytes = serde_json::to_vec(capsule).expect("capsule bytes");
        let digest = blake3::hash(&bytes).to_hex().to_string();
        assert_eq!(descriptor.total_bytes, bytes.len() as u64);
        assert_eq!(descriptor.capsule_digest, digest);
        store.reserve_provider_lease(lease).expect("lease");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                lease,
                upload_id,
                bytes.len() as u64,
                1,
                &digest,
                descriptor,
                now_unix_ms,
            )
            .expect("begin capsule upload");
        store
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                lease,
                upload_id,
                0,
                &bytes,
                &digest,
                now_unix_ms + 1,
            )
            .expect("stage capsule segment");
        bytes
    }

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
            let issued_at_unix_ms = provider_wall_clock_unix_ms().expect("clock");
            let lease = StorageLease {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease_id: uuid::Uuid::new_v4().to_string(),
                peer_device_id,
                provider_device_id,
                backup_id,
                max_new_bytes: record.len() as u64,
                max_new_objects: 1,
                issued_at_unix_ms,
                expires_at_unix_ms: issued_at_unix_ms + 60_000,
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
                        issued_at_unix_ms + 1,
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
                    issued_at_unix_ms + 2,
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
    fn local_write_batch_recovery_rolls_back_partial_and_retains_complete_batches() {
        for boundary in 1_u8..=2 {
            let directory = tempdir().expect("temporary store");
            let backup_id = BackupId::new();
            let key = BackupKey::generate();
            let chunks = [b"local-first".as_slice(), b"local-second".as_slice()]
                .into_iter()
                .map(|plaintext| key.encrypt_chunk(backup_id, 1, plaintext).expect("chunk"))
                .collect::<Vec<_>>();
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            LOCAL_WRITE_BATCH_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .put_backup_batch(&chunks.iter().collect::<Vec<_>>())
                    .is_err()
            );
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("reopen");
            assert!(
                read_directory_sorted(&reopened.root.join("local-write-journal"))
                    .expect("journals")
                    .is_empty()
            );
            let expected_created = boundary == 1;
            assert_eq!(
                reopened
                    .put_backup_batch(&chunks.iter().collect::<Vec<_>>())
                    .expect("retry"),
                vec![expected_created, expected_created]
            );
        }

        let directory = tempdir().expect("partial store");
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let chunks = [b"partial-first".as_slice(), b"partial-second".as_slice()]
            .into_iter()
            .map(|plaintext| key.encrypt_chunk(backup_id, 1, plaintext).expect("chunk"))
            .collect::<Vec<_>>();
        let records = chunks
            .iter()
            .map(EncryptedChunk::encode_provider_record)
            .collect::<Vec<_>>();
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let journal = LocalWriteBatchJournal {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            journal_id: uuid::Uuid::new_v4().to_string(),
            entries: chunks
                .iter()
                .zip(&records)
                .map(|(chunk, record)| LocalWriteBatchEntry {
                    locator: chunk.opaque_locator.clone(),
                    record_bytes: record.len() as u64,
                    record_digest: blake3::hash(record).to_hex().to_string(),
                })
                .collect(),
        };
        write_json_atomic(
            &store.local_write_batch_journal_path(&journal),
            &journal,
            true,
        )
        .expect("journal");
        write_atomic_noclobber(
            &store.chunk_path(&chunks[0].opaque_locator).expect("path"),
            &records[0],
            false,
        )
        .expect("partial file");
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("recover partial");
        assert!(
            chunks
                .iter()
                .all(|chunk| !reopened.contains(&chunk.opaque_locator).expect("contains"))
        );
        assert!(
            read_directory_sorted(&reopened.root.join("local-write-journal"))
                .expect("journals")
                .is_empty()
        );
    }

    #[test]
    fn provider_write_batch_prevalidates_scope_and_accounts_exactly_once() {
        let directory = tempdir().expect("temporary store");
        let peer_device_id = DeviceId::new();
        let provider_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let chunks = [b"batch-first".as_slice(), b"batch-second".as_slice()]
            .into_iter()
            .map(|plaintext| key.encrypt_chunk(backup_id, 1, plaintext).expect("chunk"))
            .collect::<Vec<_>>();
        let records = chunks
            .iter()
            .map(|chunk| (chunk.opaque_locator.clone(), chunk.encode_provider_record()))
            .collect::<Vec<_>>();
        let total_bytes = records.iter().map(|(_, record)| record.len() as u64).sum();
        let lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id,
            backup_id,
            max_new_bytes: total_bytes,
            max_new_objects: records.len() as u64,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "test-nonce".to_owned(),
            signature: "test-signature".to_owned(),
        };
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        store.reserve_provider_lease(&lease).expect("lease");

        let mut invalid = records.clone();
        invalid[1].1 = b"not a provider record".to_vec();
        assert!(
            store
                .put_provider_records_leased(peer_device_id, backup_id, &lease, &invalid, 200,)
                .is_err()
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| !store.contains(&chunk.opaque_locator).expect("contains"))
        );
        assert!(
            store
                .put_provider_records_leased(
                    peer_device_id,
                    BackupId::new(),
                    &lease,
                    &records,
                    200,
                )
                .is_err()
        );
        assert_eq!(
            store
                .put_provider_records_leased(peer_device_id, backup_id, &lease, &records, 200,)
                .expect("batch"),
            vec![true, true]
        );
        assert_eq!(
            store
                .put_provider_records_leased(peer_device_id, backup_id, &lease, &records, 300,)
                .expect("idempotent retry"),
            vec![false, false]
        );
        let state: ProviderLeaseState = serde_json::from_slice(
            &read_bounded(
                &store.provider_lease_path(&lease).expect("lease path"),
                MAX_PROVIDER_LEASE_STATE_BYTES,
            )
            .expect("lease state"),
        )
        .expect("lease json");
        assert_eq!(state.consumed_new_bytes, total_bytes);
        assert_eq!(state.consumed_new_objects, 2);
        assert!(
            read_directory_sorted(&store.root.join("provider-upload-journal"))
                .expect("journals")
                .is_empty()
        );
        let lost_reference = store
            .provider_object_reference_path(&records[0].0)
            .expect("reference path");
        fs::remove_file(&lost_reference).expect("simulate an unsynced reference lost at crash");
        sync_directory(lost_reference.parent().expect("reference parent"))
            .expect("persist simulated loss");
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart recovery");
        reopened
            .authorize_provider_record_batch(
                peer_device_id,
                backup_id,
                &records
                    .iter()
                    .map(|(locator, _)| locator.clone())
                    .collect::<Vec<_>>(),
            )
            .expect("lease state rebuilds a lost deferred reference before compaction");
        assert!(lost_reference.is_file());
    }

    #[test]
    fn provider_lease_compaction_recovers_every_boundary_with_exact_accounting() {
        for boundary in 1_u8..=4 {
            let directory = tempdir().expect("temporary store");
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let encrypted = BackupKey::generate()
                .encrypt_chunk(
                    backup_id,
                    1,
                    format!("lease-boundary-{boundary}").as_bytes(),
                )
                .expect("chunk");
            let record = encrypted.encode_provider_record();
            let lease = StorageLease {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease_id: uuid::Uuid::new_v4().to_string(),
                peer_device_id,
                provider_device_id: DeviceId::new(),
                backup_id,
                max_new_bytes: record.len() as u64 + 1_024,
                max_new_objects: 2,
                issued_at_unix_ms: 100,
                expires_at_unix_ms: 1_000,
                nonce: "lease-compaction".to_owned(),
                signature: "test-signature".to_owned(),
            };
            store.reserve_provider_lease(&lease).expect("lease");
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
                    .expect("leased write")
            );
            PROVIDER_LEASE_COMPACTION_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(store.provider_capacity(1_001).is_err());
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("recover");
            let capacity = reopened.provider_capacity(1_001).expect("capacity");
            assert_eq!(capacity.allocated_bytes, record.len() as u64);
            assert_eq!(capacity.reserved_bytes, 0);
            assert_eq!(capacity.reserved_objects, 0);
            assert!(!reopened.provider_lease_path(&lease).expect("path").exists());
            let ledger = reopened
                .load_provider_lease_ledger_locked()
                .expect("ledger");
            assert!(ledger.pending_compactions.is_empty());
            assert_eq!(
                ledger.peers[&peer_device_id].backups[&backup_id],
                ProviderLeaseUsage {
                    consumed_new_bytes: record.len() as u64,
                    consumed_new_objects: 1,
                }
            );
            drop(reopened);
            let second = ChunkStore::open(directory.path(), 1_048_576).expect("second reopen");
            assert_eq!(
                second
                    .provider_capacity(1_001)
                    .expect("stable capacity")
                    .allocated_bytes,
                record.len() as u64
            );
        }
    }

    #[test]
    fn provider_lease_admission_is_globally_and_per_peer_bounded_across_restart() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let expires = now + 10 * 60_000;
        let peers = (0..(MAX_ACTIVE_PROVIDER_LEASES / MAX_ACTIVE_PROVIDER_LEASES_PER_PEER))
            .map(|_| DeviceId::new())
            .collect::<Vec<_>>();
        let lease = |peer_device_id: DeviceId| StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id: DeviceId::new(),
            backup_id: BackupId::new(),
            max_new_bytes: 1,
            max_new_objects: 1,
            issued_at_unix_ms: now,
            expires_at_unix_ms: expires,
            nonce: "lease-flood".to_owned(),
            signature: "test-signature".to_owned(),
        };
        for peer_device_id in &peers {
            for _ in 0..MAX_ACTIVE_PROVIDER_LEASES_PER_PEER {
                store
                    .reserve_provider_lease(&lease(*peer_device_id))
                    .expect("bounded active lease");
            }
        }
        assert!(matches!(
            store.reserve_provider_lease(&lease(peers[0])),
            Err(CoreError::ResourceLimit("active provider leases"))
        ));
        assert!(matches!(
            store.reserve_provider_lease(&lease(DeviceId::new())),
            Err(CoreError::ResourceLimit("active provider leases"))
        ));
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("reopen");
        assert!(matches!(
            reopened.reserve_provider_lease(&lease(DeviceId::new())),
            Err(CoreError::ResourceLimit("active provider leases"))
        ));
        let expired_capacity = reopened
            .provider_capacity(expires)
            .expect("compact expired flood");
        assert_eq!(expired_capacity.allocated_bytes, 0);
        assert_eq!(expired_capacity.reserved_bytes, 0);
        assert_eq!(expired_capacity.reserved_objects, 0);
        assert!(
            read_directory_sorted(&reopened.root.join("provider-leases"))
                .expect("lease root")
                .is_empty()
        );
        drop(reopened);
        let second = ChunkStore::open(directory.path(), 1_048_576).expect("second reopen");
        let mut replacement = lease(DeviceId::new());
        replacement.issued_at_unix_ms = expires;
        replacement.expires_at_unix_ms = expires + 60_000;
        second
            .reserve_provider_lease(&replacement)
            .expect("admission after durable expiry compaction");
    }

    #[test]
    fn cancelled_provider_lease_compacts_and_releases_only_unused_reservation() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let peer_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let encrypted = BackupKey::generate()
            .encrypt_chunk(backup_id, 1, b"cancelled lease")
            .expect("chunk");
        let record = encrypted.encode_provider_record();
        let lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id: DeviceId::new(),
            backup_id,
            max_new_bytes: record.len() as u64 + 4_096,
            max_new_objects: 4,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "cancelled-lease".to_owned(),
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
            .expect("write");
        assert_eq!(
            store.provider_capacity(300).expect("active").reserved_bytes,
            4_096
        );
        store
            .cancel_provider_lease(&lease, 300)
            .expect("cancel and compact");
        store
            .cancel_provider_lease(&lease, 300)
            .expect("idempotent cancel");
        let capacity = store.provider_capacity(300).expect("cancelled capacity");
        assert_eq!(capacity.allocated_bytes, record.len() as u64);
        assert_eq!(capacity.reserved_bytes, 0);
        assert_eq!(capacity.reserved_objects, 0);
        drop(store);
        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("reopen");
        assert_eq!(
            reopened
                .provider_capacity(300)
                .expect("reopened capacity")
                .allocated_bytes,
            record.len() as u64
        );
    }

    #[test]
    fn staged_capsule_reservation_is_exact_and_segment_retries_do_not_grow() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let first_segment = vec![0x5a; MAX_RECOVERY_CAPSULE_SEGMENT_BYTES];
        let final_segment = vec![0xa5; 257];
        let total_bytes = (first_segment.len() + final_segment.len()) as u64;
        let mut hasher = blake3::Hasher::new();
        hasher.update(&first_segment);
        hasher.update(&final_segment);
        let digest = hasher.finalize().to_hex().to_string();
        let descriptor = test_capsule_descriptor(
            peer_device_id,
            backup_id,
            "staged-quota",
            total_bytes,
            digest.clone(),
            now,
        );
        let lease =
            test_provider_lease(peer_device_id, backup_id, total_bytes, 1, now, now + 60_000);
        store.reserve_provider_lease(&lease).expect("lease");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                total_bytes,
                2,
                &digest,
                &descriptor,
                now + 1,
            )
            .expect("begin");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                total_bytes,
                2,
                &digest,
                &descriptor,
                now + 2,
            )
            .expect("idempotent begin");
        assert!(matches!(
            store.begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                "conflicting-upload",
                total_bytes,
                2,
                &digest,
                &descriptor,
                now + 2,
            ),
            Err(CoreError::AuthenticationFailed)
        ));

        let state = read_test_provider_lease_state(&store, &lease);
        assert_eq!(state.consumed_new_bytes, 0);
        assert_eq!(state.consumed_new_objects, 0);
        assert_eq!(
            state
                .staged_capsule_upload
                .as_ref()
                .expect("durable staged reservation")
                .upload
                .total_bytes,
            total_bytes
        );
        let capacity = store.provider_capacity(now + 2).expect("capacity");
        assert_eq!(capacity.allocated_bytes, 0);
        assert_eq!(capacity.reserved_bytes, total_bytes);
        assert_eq!(capacity.reserved_objects, 1);

        let blocked_chunk = BackupKey::generate()
            .encrypt_chunk(backup_id, 1, b"must not bypass staged quota")
            .expect("chunk");
        assert!(matches!(
            store.put_provider_record_leased(
                peer_device_id,
                backup_id,
                &lease,
                &blocked_chunk.opaque_locator,
                &blocked_chunk.encode_provider_record(),
                now + 3,
            ),
            Err(CoreError::ResourceLimit("provider lease quota"))
        ));

        let first_digest = blake3::hash(&first_segment).to_hex().to_string();
        store
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                0,
                &first_segment,
                &first_digest,
                now + 4,
            )
            .expect("first segment");
        let first_path = store
            .recovery_capsule_upload_path(&lease)
            .join("segments/00000000.bin");
        assert_eq!(
            fs::metadata(&first_path)
                .expect("first segment metadata")
                .len(),
            first_segment.len() as u64
        );
        store
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                0,
                &first_segment,
                &first_digest,
                now + 5,
            )
            .expect("identical segment retry");
        assert_eq!(
            fs::metadata(&first_path)
                .expect("retried segment metadata")
                .len(),
            first_segment.len() as u64
        );
        let mut conflicting_segment = first_segment.clone();
        conflicting_segment[0] ^= 0xff;
        assert!(matches!(
            store.put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                0,
                &conflicting_segment,
                blake3::hash(&conflicting_segment).to_hex().as_str(),
                now + 6,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert_eq!(
            blake3::hash(&read_bounded(&first_path, first_segment.len()).expect("incumbent"))
                .to_hex()
                .as_str(),
            first_digest
        );

        let short_final = &final_segment[..final_segment.len() - 1];
        assert!(matches!(
            store.put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                1,
                short_final,
                blake3::hash(short_final).to_hex().as_str(),
                now + 7,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        let final_path = store
            .recovery_capsule_upload_path(&lease)
            .join("segments/00000001.bin");
        assert!(!final_path.exists());
        store
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "staged-quota",
                1,
                &final_segment,
                blake3::hash(&final_segment).to_hex().as_str(),
                now + 8,
            )
            .expect("exact final segment");
        assert_eq!(
            store
                .provider_capacity(now + 9)
                .expect("stable reservation")
                .reserved_bytes,
            total_bytes
        );
    }

    #[test]
    fn abandoned_capsule_staging_cleans_on_cancel_expiry_and_restart() {
        let directory = tempdir().expect("temporary store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let payload = vec![0x3c; 64 * 1_024];
        let total_bytes = payload.len() as u64;
        let digest = blake3::hash(&payload).to_hex().to_string();
        let peer_device_id = DeviceId::new();
        let quota = ProviderQuotaPolicy {
            maximum_total_bytes: total_bytes,
            maximum_peer_bytes: total_bytes,
            maximum_backup_bytes: total_bytes,
            maximum_total_objects: 1,
            maximum_peer_objects: 1,
            maximum_backup_objects: 1,
            free_space_reserve_bytes: 0,
            ..ProviderQuotaPolicy::default()
        };
        let store =
            ChunkStore::open_with_provider_quotas(directory.path(), 1_048_576, quota.clone())
                .expect("store");
        let backup_id = BackupId::new();
        let lease =
            test_provider_lease(peer_device_id, backup_id, total_bytes, 1, now, now + 60_000);
        let descriptor = test_capsule_descriptor(
            peer_device_id,
            backup_id,
            "restart-staging",
            total_bytes,
            digest.clone(),
            now,
        );
        store.reserve_provider_lease(&lease).expect("lease");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                "restart-staging",
                total_bytes,
                1,
                &digest,
                &descriptor,
                now + 1,
            )
            .expect("begin");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                "restart-staging",
                total_bytes,
                1,
                &digest,
                &descriptor,
                now + 2,
            )
            .expect("idempotent begin before simulated crash");
        let upload_path = store.recovery_capsule_upload_path(&lease);
        fs::remove_dir_all(&upload_path)
            .expect("simulate crash before staging directory durability");
        sync_directory(upload_path.parent().expect("staging backup directory"))
            .expect("persist simulated directory loss");
        drop(store);

        let reopened =
            ChunkStore::open_with_provider_quotas(directory.path(), 1_048_576, quota.clone())
                .expect("active restart");
        assert!(upload_path.join("metadata.json").is_file());
        reopened
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                "restart-staging",
                0,
                &payload,
                &digest,
                now + 3,
            )
            .expect("segment after reservation-first recovery");
        drop(reopened);

        let reopened =
            ChunkStore::open_with_provider_quotas(directory.path(), 1_048_576, quota.clone())
                .expect("restart with staged segment");
        assert!(upload_path.join("segments/00000000.bin").is_file());
        let blocked_backup_id = BackupId::new();
        let blocked_lease = test_provider_lease(
            peer_device_id,
            blocked_backup_id,
            total_bytes,
            1,
            now,
            now + 60_000,
        );
        assert!(matches!(
            reopened.reserve_provider_lease(&blocked_lease),
            Err(CoreError::ResourceLimit("provider storage quota"))
        ));
        let expired = reopened
            .provider_capacity(lease.expires_at_unix_ms)
            .expect("expire and compact");
        assert_eq!(expired.allocated_bytes, 0);
        assert_eq!(expired.reserved_bytes, 0);
        assert_eq!(expired.reserved_objects, 0);
        assert!(!upload_path.exists());
        assert!(
            read_directory_sorted(&reopened.root.join("provider-capsule-uploads"))
                .expect("staging root")
                .is_empty()
        );
        let mut replacement = blocked_lease;
        replacement.issued_at_unix_ms = lease.expires_at_unix_ms;
        replacement.expires_at_unix_ms = replacement.issued_at_unix_ms + 60_000;
        reopened
            .reserve_provider_lease(&replacement)
            .expect("quota is reusable only after staging cleanup");
        reopened
            .cancel_provider_lease(&replacement, replacement.issued_at_unix_ms + 1)
            .expect("cancel replacement");

        let orphan_path = reopened
            .root
            .join("provider-capsule-uploads")
            .join(DeviceId::new().to_string())
            .join(BackupId::new().to_string())
            .join(uuid::Uuid::new_v4().to_string())
            .join("segments");
        fs::create_dir_all(&orphan_path).expect("seed legacy orphan");
        fs::write(orphan_path.join("00000000.bin"), &payload).expect("seed orphan bytes");
        drop(reopened);

        let cleaned = ChunkStore::open_with_provider_quotas(directory.path(), 1_048_576, quota)
            .expect("startup orphan cleanup");
        assert!(
            read_directory_sorted(&cleaned.root.join("provider-capsule-uploads"))
                .expect("cleaned staging root")
                .is_empty()
        );
    }

    #[test]
    fn repeated_cancelled_capsule_uploads_leave_no_staging_growth() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();
        for index in 0_u8..8 {
            let payload = vec![index; 32 * 1_024];
            let total_bytes = payload.len() as u64;
            let digest = blake3::hash(&payload).to_hex().to_string();
            let backup_id = BackupId::new();
            let lease =
                test_provider_lease(peer_device_id, backup_id, total_bytes, 1, now, now + 60_000);
            let upload_id = format!("cancelled-{index}");
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                total_bytes,
                digest.clone(),
                now,
            );
            store.reserve_provider_lease(&lease).expect("lease");
            store
                .begin_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    &upload_id,
                    total_bytes,
                    1,
                    &digest,
                    &descriptor,
                    now + 1,
                )
                .expect("begin");
            store
                .put_recovery_capsule_segment(
                    peer_device_id,
                    backup_id,
                    &lease,
                    &upload_id,
                    0,
                    &payload,
                    &digest,
                    now + 2,
                )
                .expect("segment");
            store
                .cancel_provider_lease(&lease, now + 3)
                .expect("cancel");
            assert!(!store.recovery_capsule_upload_path(&lease).exists());
            assert!(
                read_directory_sorted(&store.root.join("provider-capsule-uploads"))
                    .expect("bounded staging root")
                    .is_empty()
            );
        }
        let capacity = store.provider_capacity(now + 4).expect("capacity");
        assert_eq!(capacity.allocated_bytes, 0);
        assert_eq!(capacity.reserved_bytes, 0);
        assert_eq!(capacity.reserved_objects, 0);
    }

    #[test]
    fn segmented_capsule_commit_recovers_every_journal_boundary_exactly_once() {
        for boundary in 1_u8..=3 {
            let directory = tempdir().expect("temporary store");
            let now = provider_wall_clock_unix_ms().expect("clock");
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let upload_id = format!("segmented-boundary-{boundary}");
            let capsule = RecoveryCapsule {
                schema_version: 1,
                cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
                backup_id,
                snapshot_id: upload_id.clone(),
                key_epoch: 1,
                committed_at_unix_ms: now,
                nonce: "opaque".to_owned(),
                ciphertext: "opaque".to_owned(),
                signer_device_id: peer_device_id,
                signature: "opaque".to_owned(),
            };
            let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                now,
                now + 60_000,
            );
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                bytes.len() as u64,
                digest.clone(),
                now,
            );
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            store.reserve_provider_lease(&lease).expect("lease");
            store
                .begin_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    &upload_id,
                    bytes.len() as u64,
                    1,
                    &digest,
                    &descriptor,
                    now + 1,
                )
                .expect("begin");
            store
                .put_recovery_capsule_segment(
                    peer_device_id,
                    backup_id,
                    &lease,
                    &upload_id,
                    0,
                    &bytes,
                    &digest,
                    now + 2,
                )
                .expect("segment");
            PROVIDER_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 3,
                    )
                    .is_err()
            );
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart");
            assert!(
                reopened
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 4,
                    )
                    .expect("retry after crash")
            );
            assert!(
                reopened
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 5,
                    )
                    .expect("idempotent terminal retry")
            );
            assert_eq!(
                reopened.list_recovery_capsules().expect("capsule"),
                vec![capsule]
            );
            assert!(!reopened.recovery_capsule_upload_path(&lease).exists());
            assert!(
                read_directory_sorted(&reopened.root.join("provider-upload-journal"))
                    .expect("journals")
                    .is_empty()
            );
            let state = read_test_provider_lease_state(&reopened, &lease);
            assert_eq!(state.consumed_new_bytes, bytes.len() as u64);
            assert_eq!(state.consumed_new_objects, 1);
            assert!(state.staged_capsule_upload.is_none());
        }
    }

    #[test]
    fn same_process_segmented_commit_retry_reconciles_post_rename_journal() {
        for boundary in 2_u8..=3 {
            let directory = tempdir().expect("temporary store");
            let now = provider_wall_clock_unix_ms().expect("clock");
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let upload_id = format!("same-process-boundary-{boundary}");
            let capsule = test_recovery_capsule(peer_device_id, backup_id, &upload_id, now);
            let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                bytes.len() as u64,
                digest,
                now,
            );
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                now,
                now + 60_000,
            );
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            stage_test_recovery_capsule(
                &store,
                peer_device_id,
                backup_id,
                &lease,
                &upload_id,
                &capsule,
                &descriptor,
                now + 1,
            );

            PROVIDER_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 3,
                    )
                    .is_err()
            );
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 4,
                    )
                    .expect("same-process exact retry")
            );
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 5,
                    )
                    .expect("terminal receipt retry")
            );

            let state = read_test_provider_lease_state(&store, &lease);
            assert_eq!(state.consumed_new_bytes, bytes.len() as u64);
            assert_eq!(state.consumed_new_objects, 1);
            assert!(state.staged_capsule_upload.is_none());
            let capacity = store.provider_capacity(now + 6).expect("capacity");
            assert_eq!(capacity.allocated_bytes, bytes.len() as u64);
            assert_eq!(capacity.reserved_bytes, 0);
            assert_eq!(capacity.reserved_objects, 0);
            assert!(!store.recovery_capsule_upload_path(&lease).exists());
            assert!(
                read_directory_sorted(&store.root.join("provider-upload-journal"))
                    .expect("journals")
                    .is_empty()
            );
        }
    }

    #[test]
    fn committed_segmented_capsule_recovers_receipt_before_expired_lease_compaction() {
        for boundary in 2_u8..=3 {
            let directory = tempdir().expect("temporary store");
            let wall_clock = provider_wall_clock_unix_ms().expect("clock");
            let issued_at = wall_clock - 120_000;
            let expires_at = wall_clock - 60_000;
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let upload_id = format!("expired-boundary-{boundary}");
            let capsule =
                test_recovery_capsule(peer_device_id, backup_id, &upload_id, issued_at + 1);
            let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                bytes.len() as u64,
                digest,
                issued_at + 1,
            );
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                issued_at,
                expires_at,
            );
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            stage_test_recovery_capsule(
                &store,
                peer_device_id,
                backup_id,
                &lease,
                &upload_id,
                &capsule,
                &descriptor,
                issued_at + 2,
            );
            PROVIDER_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        issued_at + 4,
                    )
                    .is_err()
            );
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576)
                .expect("recover committed upload before expiry cleanup");
            let capacity = reopened.provider_capacity(wall_clock).expect("capacity");
            assert_eq!(capacity.allocated_bytes, bytes.len() as u64);
            assert_eq!(capacity.reserved_bytes, 0);
            assert_eq!(capacity.reserved_objects, 0);
            assert!(
                !reopened
                    .provider_lease_path(&lease)
                    .expect("lease path")
                    .exists()
            );
            assert!(!reopened.recovery_capsule_upload_path(&lease).exists());
            assert!(
                read_directory_sorted(&reopened.root.join("provider-upload-journal"))
                    .expect("journals")
                    .is_empty()
            );
            assert!(
                reopened
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        wall_clock,
                    )
                    .expect("exact retry after expired restart")
            );
            drop(reopened);

            let second = ChunkStore::open(directory.path(), 1_048_576).expect("second restart");
            assert!(
                second
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        wall_clock + 1,
                    )
                    .expect("stable terminal receipt")
            );
            assert_eq!(
                second
                    .provider_capacity(wall_clock + 1)
                    .expect("stable capacity")
                    .allocated_bytes,
                bytes.len() as u64
            );
        }
    }

    #[test]
    fn cancelling_interrupted_segmented_commits_reconciles_durable_truth() {
        for boundary in 1_u8..=3 {
            let directory = tempdir().expect("temporary store");
            let now = provider_wall_clock_unix_ms().expect("clock");
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let upload_id = format!("cancel-boundary-{boundary}");
            let capsule = test_recovery_capsule(peer_device_id, backup_id, &upload_id, now);
            let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                bytes.len() as u64,
                digest,
                now,
            );
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                now,
                now + 60_000,
            );
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            stage_test_recovery_capsule(
                &store,
                peer_device_id,
                backup_id,
                &lease,
                &upload_id,
                &capsule,
                &descriptor,
                now + 1,
            );
            PROVIDER_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 3,
                    )
                    .is_err()
            );
            store
                .cancel_provider_lease(&lease, now + 4)
                .expect("cancel reconciles journal first");
            let capacity = store.provider_capacity(now + 5).expect("capacity");
            assert_eq!(
                capacity.allocated_bytes,
                if boundary == 1 { 0 } else { bytes.len() as u64 }
            );
            assert_eq!(capacity.reserved_bytes, 0);
            assert_eq!(capacity.reserved_objects, 0);
            assert!(!store.recovery_capsule_upload_path(&lease).exists());
            assert!(
                read_directory_sorted(&store.root.join("provider-upload-journal"))
                    .expect("journals")
                    .is_empty()
            );
            if boundary == 1 {
                assert!(
                    store
                        .commit_recovery_capsule_upload(
                            peer_device_id,
                            backup_id,
                            &lease,
                            &upload_id,
                            now + 6,
                        )
                        .is_err()
                );
            } else {
                assert!(
                    store
                        .commit_recovery_capsule_upload(
                            peer_device_id,
                            backup_id,
                            &lease,
                            &upload_id,
                            now + 6,
                        )
                        .expect("committed retry receipt")
                );
            }
            drop(store);

            let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart");
            assert_eq!(
                reopened
                    .provider_capacity(now + 7)
                    .expect("restart capacity")
                    .allocated_bytes,
                if boundary == 1 { 0 } else { bytes.len() as u64 }
            );
            assert_eq!(
                reopened.list_recovery_capsules().expect("capsules"),
                if boundary == 1 {
                    Vec::new()
                } else {
                    vec![capsule]
                }
            );
        }
    }

    #[test]
    fn segmented_capsule_descriptor_must_match_authenticated_payload_header() {
        for field in [
            "backupId",
            "snapshotId",
            "keyEpoch",
            "committedAtUnixMs",
            "signerDeviceId",
        ] {
            let directory = tempdir().expect("temporary store");
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            let now = provider_wall_clock_unix_ms().expect("clock");
            let peer_device_id = DeviceId::new();
            let backup_id = BackupId::new();
            let upload_id = format!("header-{}", field.to_ascii_lowercase());
            let mut capsule = test_recovery_capsule(peer_device_id, backup_id, &upload_id, now);
            match field {
                "backupId" => capsule.backup_id = BackupId::new(),
                "snapshotId" => capsule.snapshot_id = "different-snapshot".to_owned(),
                "keyEpoch" => capsule.key_epoch += 1,
                "committedAtUnixMs" => capsule.committed_at_unix_ms += 1,
                "signerDeviceId" => capsule.signer_device_id = DeviceId::new(),
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                &upload_id,
                bytes.len() as u64,
                digest,
                now,
            );
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                now,
                now + 60_000,
            );
            stage_test_recovery_capsule(
                &store,
                peer_device_id,
                backup_id,
                &lease,
                &upload_id,
                &capsule,
                &descriptor,
                now + 1,
            );
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        &upload_id,
                        now + 3,
                    )
                    .is_err(),
                "descriptor field {field} must match payload"
            );
            assert!(
                !store
                    .recovery_capsule_path(peer_device_id, backup_id, &descriptor.snapshot_id,)
                    .expect("final path")
                    .exists()
            );
            assert!(store.list_recovery_capsules().expect("capsules").is_empty());
            let state = read_test_provider_lease_state(&store, &lease);
            assert_eq!(state.consumed_new_bytes, 0);
            assert_eq!(state.consumed_new_objects, 0);
            assert!(state.staged_capsule_upload.is_some());
            assert_eq!(
                store
                    .provider_capacity(now + 4)
                    .expect("reserved capacity")
                    .reserved_bytes,
                bytes.len() as u64
            );
            store
                .cancel_provider_lease(&lease, now + 5)
                .expect("cancel rejected payload");
            let capacity = store.provider_capacity(now + 6).expect("released capacity");
            assert_eq!(capacity.allocated_bytes, 0);
            assert_eq!(capacity.reserved_bytes, 0);
            assert!(
                store
                    .list_recovery_capsule_descriptors_for_owner(
                        peer_device_id,
                        Some(backup_id),
                        None,
                        128,
                    )
                    .expect("healthy owner index")
                    .0
                    .is_empty()
            );
        }
    }

    #[test]
    fn segmented_capsule_parser_rejects_invalid_utf8_and_surrogate_without_growth() {
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let capsule = test_recovery_capsule(peer_device_id, backup_id, "malformed-json", now);
        let canonical = serde_json::to_vec(&capsule).expect("capsule bytes");
        let ciphertext = b"\"ciphertext\":\"opaque\"";
        let offset = canonical
            .windows(ciphertext.len())
            .position(|window| window == ciphertext)
            .expect("ciphertext field")
            + b"\"ciphertext\":\"".len();
        let mut invalid_utf8 = canonical.clone();
        invalid_utf8[offset] = 0xff;
        let mut lone_surrogate = canonical;
        lone_surrogate.splice(offset..offset + "opaque".len(), b"\\uD800".iter().copied());

        for (case, bytes) in [
            ("invalid-utf8", invalid_utf8),
            ("lone-surrogate", lone_surrogate),
        ] {
            let directory = tempdir().expect("temporary store");
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
            let digest = blake3::hash(&bytes).to_hex().to_string();
            let descriptor = test_capsule_descriptor(
                peer_device_id,
                backup_id,
                "malformed-json",
                bytes.len() as u64,
                digest.clone(),
                now,
            );
            let lease = test_provider_lease(
                peer_device_id,
                backup_id,
                bytes.len() as u64,
                1,
                now,
                now + 60_000,
            );
            store.reserve_provider_lease(&lease).expect("lease");
            store
                .begin_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    case,
                    bytes.len() as u64,
                    1,
                    &digest,
                    &descriptor,
                    now + 1,
                )
                .expect("begin malformed upload");
            store
                .put_recovery_capsule_segment(
                    peer_device_id,
                    backup_id,
                    &lease,
                    case,
                    0,
                    &bytes,
                    &digest,
                    now + 2,
                )
                .expect("stage malformed JSON");
            assert!(
                store
                    .commit_recovery_capsule_upload(
                        peer_device_id,
                        backup_id,
                        &lease,
                        case,
                        now + 3,
                    )
                    .is_err(),
                "{case} must fail closed"
            );
            assert!(
                store
                    .list_recovery_capsules()
                    .expect("healthy list")
                    .is_empty()
            );
            let capacity = store.provider_capacity(now + 4).expect("capacity");
            assert_eq!(capacity.allocated_bytes, 0);
            assert_eq!(capacity.reserved_bytes, bytes.len() as u64);
            store
                .cancel_provider_lease(&lease, now + 5)
                .expect("cancel malformed staging");
            assert_eq!(
                store
                    .provider_capacity(now + 6)
                    .expect("released capacity")
                    .reserved_bytes,
                0
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn recovery_capsule_reads_reject_staged_and_committed_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();

        let staged_backup_id = BackupId::new();
        let staged_upload_id = "staged-symlink";
        let staged_capsule =
            test_recovery_capsule(peer_device_id, staged_backup_id, staged_upload_id, now);
        let staged_bytes = serde_json::to_vec(&staged_capsule).expect("capsule bytes");
        let staged_digest = blake3::hash(&staged_bytes).to_hex().to_string();
        let staged_descriptor = test_capsule_descriptor(
            peer_device_id,
            staged_backup_id,
            staged_upload_id,
            staged_bytes.len() as u64,
            staged_digest,
            now,
        );
        let staged_lease = test_provider_lease(
            peer_device_id,
            staged_backup_id,
            staged_bytes.len() as u64,
            1,
            now,
            now + 60_000,
        );
        stage_test_recovery_capsule(
            &store,
            peer_device_id,
            staged_backup_id,
            &staged_lease,
            staged_upload_id,
            &staged_capsule,
            &staged_descriptor,
            now + 1,
        );
        let staged_path = store
            .recovery_capsule_upload_path(&staged_lease)
            .join("segments/00000000.bin");
        let outside_segment = directory.path().join("outside-segment.bin");
        fs::write(&outside_segment, &staged_bytes).expect("outside segment");
        fs::remove_file(&staged_path).expect("remove staged segment");
        symlink(&outside_segment, &staged_path).expect("substitute staged symlink");
        assert!(
            store
                .commit_recovery_capsule_upload(
                    peer_device_id,
                    staged_backup_id,
                    &staged_lease,
                    staged_upload_id,
                    now + 3,
                )
                .is_err()
        );
        assert_eq!(
            fs::read(&outside_segment).expect("outside bytes"),
            staged_bytes
        );
        store
            .cancel_provider_lease(&staged_lease, now + 4)
            .expect("cancel staged symlink");

        let committed_backup_id = BackupId::new();
        let committed_upload_id = "committed-symlink";
        let committed_capsule = test_recovery_capsule(
            peer_device_id,
            committed_backup_id,
            committed_upload_id,
            now,
        );
        let committed_bytes = serde_json::to_vec(&committed_capsule).expect("capsule bytes");
        let committed_digest = blake3::hash(&committed_bytes).to_hex().to_string();
        let committed_descriptor = test_capsule_descriptor(
            peer_device_id,
            committed_backup_id,
            committed_upload_id,
            committed_bytes.len() as u64,
            committed_digest,
            now,
        );
        let committed_lease = test_provider_lease(
            peer_device_id,
            committed_backup_id,
            committed_bytes.len() as u64,
            1,
            now,
            now + 60_000,
        );
        stage_test_recovery_capsule(
            &store,
            peer_device_id,
            committed_backup_id,
            &committed_lease,
            committed_upload_id,
            &committed_capsule,
            &committed_descriptor,
            now + 1,
        );
        assert!(
            store
                .commit_recovery_capsule_upload(
                    peer_device_id,
                    committed_backup_id,
                    &committed_lease,
                    committed_upload_id,
                    now + 3,
                )
                .expect("commit capsule")
        );
        let committed_path = store
            .recovery_capsule_path(peer_device_id, committed_backup_id, committed_upload_id)
            .expect("committed path");
        let outside_capsule = directory.path().join("outside-capsule.json");
        fs::write(&outside_capsule, &committed_bytes).expect("outside capsule");
        fs::remove_file(&committed_path).expect("remove committed capsule");
        symlink(&outside_capsule, &committed_path).expect("substitute committed symlink");
        assert!(
            store
                .read_recovery_capsule_segment_for_owner(
                    peer_device_id,
                    committed_backup_id,
                    committed_upload_id,
                    0,
                    128,
                )
                .is_err()
        );
        assert!(store.list_recovery_capsules().is_err());
        assert!(
            hash_file_bounded(
                &committed_path,
                MAX_RECOVERY_CAPSULE_BYTES as u64,
                "test committed symlink",
            )
            .is_err()
        );
    }

    #[test]
    fn recovery_capsule_commit_rejects_same_length_staged_file_replacement() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let upload_id = "same-length-file-swap";
        let capsule = test_recovery_capsule(peer_device_id, backup_id, upload_id, now);
        let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let descriptor = test_capsule_descriptor(
            peer_device_id,
            backup_id,
            upload_id,
            bytes.len() as u64,
            digest,
            now,
        );
        let lease = test_provider_lease(
            peer_device_id,
            backup_id,
            bytes.len() as u64,
            1,
            now,
            now + 60_000,
        );
        stage_test_recovery_capsule(
            &store,
            peer_device_id,
            backup_id,
            &lease,
            upload_id,
            &capsule,
            &descriptor,
            now + 1,
        );
        let segment_path = store
            .recovery_capsule_upload_path(&lease)
            .join("segments/00000000.bin");
        let mut replacement = bytes.clone();
        let replacement_middle = replacement.len() / 2;
        replacement[replacement_middle] ^= 0x01;
        fs::write(&segment_path, &replacement).expect("replace staged regular file");
        assert_eq!(
            fs::symlink_metadata(&segment_path)
                .expect("replacement metadata")
                .len(),
            bytes.len() as u64
        );
        assert!(
            store
                .commit_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    upload_id,
                    now + 3,
                )
                .is_err()
        );
        assert!(
            !store
                .recovery_capsule_path(peer_device_id, backup_id, upload_id)
                .expect("final path")
                .exists()
        );
        let state = read_test_provider_lease_state(&store, &lease);
        assert_eq!(state.consumed_new_bytes, 0);
        assert_eq!(state.consumed_new_objects, 0);
        store
            .cancel_provider_lease(&lease, now + 4)
            .expect("cancel swapped stage");
    }

    #[test]
    fn terminal_capsule_upload_receipt_survives_cleanup_and_restart() {
        let directory = tempdir().expect("temporary store");
        let peer_device_id = DeviceId::new();
        let provider_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let upload_id = "terminal-retry";
        let capsule = RecoveryCapsule {
            schema_version: 1,
            cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
            backup_id,
            snapshot_id: "terminal-receipt".to_owned(),
            key_epoch: 1,
            committed_at_unix_ms: 150,
            nonce: "opaque".to_owned(),
            ciphertext: "opaque".to_owned(),
            signer_device_id: peer_device_id,
            signature: "opaque".to_owned(),
        };
        let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id,
            provider_device_id,
            backup_id,
            max_new_bytes: bytes.len() as u64,
            max_new_objects: 1,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "test-nonce".to_owned(),
            signature: "test-signature".to_owned(),
        };
        let descriptor = RecoveryCapsuleDescriptor {
            backup_id,
            snapshot_id: capsule.snapshot_id.clone(),
            key_epoch: capsule.key_epoch,
            committed_at_unix_ms: capsule.committed_at_unix_ms,
            signer_device_id: peer_device_id,
            total_bytes: bytes.len() as u64,
            capsule_digest: digest.clone(),
        };
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        store.reserve_provider_lease(&lease).expect("lease");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                upload_id,
                bytes.len() as u64,
                1,
                &digest,
                &descriptor,
                200,
            )
            .expect("begin");
        store
            .put_recovery_capsule_segment(
                peer_device_id,
                backup_id,
                &lease,
                upload_id,
                0,
                &bytes,
                blake3::hash(&bytes).to_hex().as_str(),
                250,
            )
            .expect("segment");
        assert!(
            store
                .commit_recovery_capsule_upload(peer_device_id, backup_id, &lease, upload_id, 300,)
                .expect("first terminal commit")
        );
        assert!(!store.recovery_capsule_upload_path(&lease).exists());
        assert!(
            store
                .provider_upload_receipt_path(&lease, upload_id)
                .expect("receipt path")
                .is_file()
        );
        fs::create_dir_all(store.recovery_capsule_upload_path(&lease).join("segments"))
            .expect("simulate crash before upload cleanup");
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("reopen");
        assert!(
            reopened
                .commit_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    upload_id,
                    2_000,
                )
                .expect("idempotent terminal retry after lease expiry")
        );
        assert!(!reopened.recovery_capsule_upload_path(&lease).exists());
        assert_eq!(
            reopened.list_recovery_capsules().expect("stored capsule"),
            vec![capsule]
        );
    }

    #[test]
    fn receipt_publish_crash_reuses_durable_completion_time_after_expiry() {
        let directory = tempdir().expect("temporary store");
        let peer_device_id = DeviceId::new();
        let backup_id = BackupId::new();
        let upload_id = "receipt-publish-crash";
        let capsule = test_recovery_capsule(peer_device_id, backup_id, upload_id, 150);
        let bytes = serde_json::to_vec(&capsule).expect("capsule bytes");
        let digest = blake3::hash(&bytes).to_hex().to_string();
        let descriptor = test_capsule_descriptor(
            peer_device_id,
            backup_id,
            upload_id,
            bytes.len() as u64,
            digest,
            150,
        );
        let lease =
            test_provider_lease(peer_device_id, backup_id, bytes.len() as u64, 1, 100, 1_000);
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        stage_test_recovery_capsule(
            &store,
            peer_device_id,
            backup_id,
            &lease,
            upload_id,
            &capsule,
            &descriptor,
            200,
        );
        PROVIDER_UPLOAD_RECEIPT_FAILPOINT.with(|armed| armed.set(1));
        assert!(matches!(
            store.commit_recovery_capsule_upload(
                peer_device_id,
                backup_id,
                &lease,
                upload_id,
                300,
            ),
            Err(CoreError::InvalidState(message))
                if message == "provider upload receipt failpoint 1"
        ));
        let staged = read_test_provider_lease_state(&store, &lease)
            .staged_capsule_upload
            .expect("committed stage");
        assert_eq!(staged.committed_created, Some(true));
        assert_eq!(staged.completed_at_unix_ms, Some(300));
        assert!(
            store
                .load_provider_upload_receipt_locked(&lease, upload_id)
                .expect("receipt")
                .is_some()
        );
        let lease_path = store.provider_lease_path(&lease).expect("lease path");
        let mut legacy_state: serde_json::Value = serde_json::from_slice(
            &read_bounded(&lease_path, MAX_PROVIDER_LEASE_STATE_BYTES).expect("lease state"),
        )
        .expect("lease json");
        legacy_state
            .get_mut("stagedCapsuleUpload")
            .and_then(serde_json::Value::as_object_mut)
            .expect("staged object")
            .remove("completedAtUnixMs");
        write_json_atomic(&lease_path, &legacy_state, true).expect("legacy committed stage");
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576)
            .expect("receipt timestamp must survive later restart clock");
        let capacity = reopened
            .provider_capacity(provider_wall_clock_unix_ms().expect("clock"))
            .expect("capacity");
        assert_eq!(capacity.allocated_bytes, bytes.len() as u64);
        assert_eq!(capacity.reserved_bytes, 0);
        assert!(!reopened.recovery_capsule_upload_path(&lease).exists());
        assert!(
            reopened
                .commit_recovery_capsule_upload(
                    peer_device_id,
                    backup_id,
                    &lease,
                    upload_id,
                    2_000,
                )
                .expect("exact retry after expiry")
        );
        reopened
            .acknowledge_recovery_capsule_upload(&lease, upload_id)
            .expect("ack exact receipt");
        drop(reopened);

        let second = ChunkStore::open(directory.path(), 1_048_576).expect("second restart");
        assert_eq!(
            second
                .provider_capacity(provider_wall_clock_unix_ms().expect("clock"))
                .expect("second capacity")
                .allocated_bytes,
            bytes.len() as u64
        );
        assert_eq!(
            second.list_recovery_capsules().expect("capsule"),
            vec![capsule]
        );
    }

    #[test]
    fn terminal_upload_receipts_backpressure_without_cross_peer_or_self_eviction() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let first_peer = DeviceId::new();
        let first_lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id: first_peer,
            provider_device_id: DeviceId::new(),
            backup_id: BackupId::new(),
            max_new_bytes: 1,
            max_new_objects: 1,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "test-nonce".to_owned(),
            signature: "test-signature".to_owned(),
        };
        store
            .persist_provider_upload_receipt_locked(&first_lease, "first-unacknowledged", true, 200)
            .expect("first peer receipt");

        let second_lease = StorageLease {
            peer_device_id: DeviceId::new(),
            backup_id: BackupId::new(),
            lease_id: uuid::Uuid::new_v4().to_string(),
            ..first_lease.clone()
        };
        for index in 0..MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER {
            store
                .persist_provider_upload_receipt_locked(
                    &second_lease,
                    &format!("second-peer-{index}"),
                    true,
                    300 + index as u64,
                )
                .expect("bounded second-peer receipt");
        }
        assert!(matches!(
            store.persist_provider_upload_receipt_locked(
                &second_lease,
                "second-peer-blocked",
                true,
                1_000,
            ),
            Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure"
            ))
        ));
        assert!(
            store
                .load_provider_upload_receipt_locked(&second_lease, "second-peer-0")
                .expect("old same-peer receipt at cap")
                .is_some()
        );
        assert_eq!(
            store
                .load_provider_upload_receipt_locked(&first_lease, "first-unacknowledged")
                .expect("first receipt"),
            Some(ProviderUploadReceipt {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease: first_lease.clone(),
                upload_id: "first-unacknowledged".to_owned(),
                created: true,
                completed_at_unix_ms: 200,
            })
        );
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart");
        assert!(
            reopened
                .load_provider_upload_receipt_locked(&first_lease, "first-unacknowledged")
                .expect("old first-peer retry after restart")
                .is_some()
        );
        assert!(
            reopened
                .load_provider_upload_receipt_locked(&second_lease, "second-peer-0")
                .expect("old same-peer retry after restart")
                .is_some()
        );
        reopened
            .acknowledge_recovery_capsule_upload(&first_lease, "first-unacknowledged")
            .expect("ack first receipt");
        reopened
            .acknowledge_recovery_capsule_upload(&first_lease, "first-unacknowledged")
            .expect("idempotent ack");
        assert!(
            reopened
                .load_provider_upload_receipt_locked(&first_lease, "first-unacknowledged")
                .expect("acked receipt")
                .is_none()
        );
    }

    #[test]
    fn staged_capsule_upload_counts_against_receipt_admission_cap() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let now = provider_wall_clock_unix_ms().expect("clock");
        let peer_device_id = DeviceId::new();
        let receipt_lease =
            test_provider_lease(peer_device_id, BackupId::new(), 1, 1, now, now + 60_000);
        for index in 0..MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER - 1 {
            store
                .persist_provider_upload_receipt_locked(
                    &receipt_lease,
                    &format!("retained-{index}"),
                    true,
                    now + 1,
                )
                .expect("retained receipt");
        }

        let first_backup_id = BackupId::new();
        let first_digest = blake3::hash(b"a").to_hex().to_string();
        let first_descriptor = test_capsule_descriptor(
            peer_device_id,
            first_backup_id,
            "first-staged",
            1,
            first_digest.clone(),
            now,
        );
        let first_lease =
            test_provider_lease(peer_device_id, first_backup_id, 1, 1, now, now + 60_000);
        store
            .reserve_provider_lease(&first_lease)
            .expect("first lease");
        store
            .begin_recovery_capsule_upload(
                peer_device_id,
                first_backup_id,
                &first_lease,
                "first-staged",
                1,
                1,
                &first_digest,
                &first_descriptor,
                now + 1,
            )
            .expect("last receipt slot may be staged");

        let blocked_backup_id = BackupId::new();
        let blocked_digest = blake3::hash(b"b").to_hex().to_string();
        let blocked_descriptor = test_capsule_descriptor(
            peer_device_id,
            blocked_backup_id,
            "blocked-staged",
            1,
            blocked_digest.clone(),
            now,
        );
        let blocked_lease =
            test_provider_lease(peer_device_id, blocked_backup_id, 1, 1, now, now + 60_000);
        store
            .reserve_provider_lease(&blocked_lease)
            .expect("blocked lease");
        assert!(matches!(
            store.begin_recovery_capsule_upload(
                peer_device_id,
                blocked_backup_id,
                &blocked_lease,
                "blocked-staged",
                1,
                1,
                &blocked_digest,
                &blocked_descriptor,
                now + 2,
            ),
            Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure"
            ))
        ));
    }

    #[test]
    fn startup_preserves_bounded_legacy_overage_until_authenticated_ack() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let lease = StorageLease {
            schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
            lease_id: uuid::Uuid::new_v4().to_string(),
            peer_device_id: DeviceId::new(),
            provider_device_id: DeviceId::new(),
            backup_id: BackupId::new(),
            max_new_bytes: 1,
            max_new_objects: 1,
            issued_at_unix_ms: 100,
            expires_at_unix_ms: 1_000,
            nonce: "legacy-overage".to_owned(),
            signature: "test-signature".to_owned(),
        };
        let peer_directory = store
            .ensure_provider_upload_receipt_peer_locked(lease.peer_device_id)
            .expect("receipt peer");
        for index in 0..MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK {
            let upload_id = format!("legacy-overage-{index}");
            let receipt = ProviderUploadReceipt {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease: lease.clone(),
                upload_id: upload_id.clone(),
                created: true,
                completed_at_unix_ms: 200 + index as u64,
            };
            write_json_atomic(
                &store
                    .provider_upload_receipt_path(&lease, &upload_id)
                    .expect("receipt path"),
                &receipt,
                true,
            )
            .expect("seed bounded legacy overage");
        }
        assert_eq!(
            read_directory_sorted(&peer_directory)
                .expect("receipt directory")
                .len(),
            MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK
        );
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576)
            .expect("bounded legacy overage remains serviceable");
        assert!(
            reopened
                .load_provider_upload_receipt_locked(&lease, "legacy-overage-0")
                .expect("old exact retry")
                .is_some()
        );
        assert!(matches!(
            reopened.persist_provider_upload_receipt_locked(
                &lease,
                "blocked-while-overbound",
                true,
                2_000,
            ),
            Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure"
            ))
        ));
        reopened
            .acknowledge_recovery_capsule_upload(&lease, "legacy-overage-0")
            .expect("first authenticated ack");
        assert!(matches!(
            reopened.persist_provider_upload_receipt_locked(
                &lease,
                "blocked-at-bound",
                true,
                2_001,
            ),
            Err(CoreError::ResourceLimit(
                "provider upload receipt backpressure"
            ))
        ));
        reopened
            .acknowledge_recovery_capsule_upload(&lease, "legacy-overage-1")
            .expect("second authenticated ack");
        reopened
            .persist_provider_upload_receipt_locked(&lease, "admitted-after-acks", true, 2_002)
            .expect("receipt admission restored");
        assert_eq!(
            read_directory_sorted(&peer_directory)
                .expect("bounded receipt directory")
                .len(),
            MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER
        );
    }

    #[test]
    fn startup_enumeration_limits_fail_closed_before_unbounded_traversal() {
        let journal_directory = tempdir().expect("journal store");
        drop(ChunkStore::open(journal_directory.path(), 1_048_576).expect("initialize journal"));
        let journal_root = journal_directory.path().join("provider-upload-journal");
        for index in 0..=MAX_PROVIDER_UPLOAD_JOURNALS {
            fs::write(journal_root.join(format!("{index:08}.json")), b"{}")
                .expect("seed journal entry");
        }
        assert!(matches!(
            ChunkStore::open(journal_directory.path(), 1_048_576),
            Err(CoreError::ResourceLimit("provider upload journals"))
        ));

        let receipt_directory = tempdir().expect("receipt store");
        let receipt_store =
            ChunkStore::open(receipt_directory.path(), 1_048_576).expect("initialize receipts");
        let receipt_peer = receipt_store
            .root
            .join("provider-upload-receipts")
            .join(DeviceId::new().to_string());
        ensure_private_directory(&receipt_peer).expect("receipt peer");
        drop(receipt_store);
        for index in 0..=MAX_PROVIDER_UPLOAD_RECEIPTS_PER_PEER_ON_DISK {
            fs::write(receipt_peer.join(format!("{index:08}.json")), b"{}")
                .expect("seed receipt entry");
        }
        assert!(matches!(
            ChunkStore::open(receipt_directory.path(), 1_048_576),
            Err(CoreError::ResourceLimit(
                "provider upload receipts per peer"
            ))
        ));

        let staging_directory = tempdir().expect("staging store");
        let staging_store =
            ChunkStore::open(staging_directory.path(), 1_048_576).expect("initialize staging");
        let staging_backup = staging_store
            .root
            .join("provider-capsule-uploads")
            .join(DeviceId::new().to_string())
            .join(BackupId::new().to_string());
        drop(staging_store);
        for index in 0..=MAX_PROVIDER_CAPSULE_STAGING_LEASES {
            ensure_private_directory(&staging_backup.join(format!("lease-{index:08}")))
                .expect("seed staging lease");
        }
        let staging_result = ChunkStore::open(staging_directory.path(), 1_048_576);
        assert!(
            matches!(
                staging_result,
                Err(CoreError::ResourceLimit("provider capsule staging leases"))
            ),
            "unexpected staging inventory result: {staging_result:?}"
        );
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
    fn descriptor_pages_open_only_limit_plus_one_direct_sequence_records() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let owner = DeviceId::new();
        let first_backup = BackupId::new();
        let second_backup = BackupId::new();
        let mut expected = Vec::new();
        for index in 0..140_u64 {
            let descriptor = RecoveryCapsuleDescriptor {
                backup_id: if index % 2 == 0 {
                    first_backup
                } else {
                    second_backup
                },
                snapshot_id: format!("bounded-page-{index:04}"),
                key_epoch: 1,
                committed_at_unix_ms: index + 1,
                signer_device_id: owner,
                total_bytes: 1,
                capsule_digest: blake3::hash(&index.to_be_bytes()).to_hex().to_string(),
            };
            store
                .persist_recovery_capsule_descriptor_value_locked(&descriptor)
                .expect("persist paged descriptor");
            expected.push(descriptor);
        }

        let mut cursor = None;
        let mut actual = Vec::new();
        loop {
            RECOVERY_CAPSULE_PAGE_READS.with(|reads| reads.set(0));
            let (page, next) = store
                .list_recovery_capsule_descriptors_for_owner(owner, None, cursor.as_deref(), 17)
                .expect("bounded page");
            assert!(
                RECOVERY_CAPSULE_PAGE_READS.with(Cell::get) <= 18,
                "one page may open only limit + 1 sequence records"
            );
            actual.extend(page);
            let Some(next) = next else { break };
            assert!(cursor.as_ref().is_none_or(|previous| previous < &next));
            cursor = Some(next);
        }
        assert_eq!(actual, expected);

        let (filtered, _) = store
            .list_recovery_capsule_descriptors_for_owner(owner, Some(first_backup), None, 128)
            .expect("backup feed");
        assert_eq!(filtered.len(), 70);
        assert!(
            filtered
                .iter()
                .all(|descriptor| descriptor.backup_id == first_backup)
        );
        assert!(matches!(
            store.list_recovery_capsule_descriptors_for_owner_with_deadline(
                owner,
                None,
                None,
                1,
                Instant::now(),
            ),
            Err(CoreError::ResourceLimit("QUIC operation timeout"))
        ));
        let other_owner = DeviceId::new();
        store
            .persist_recovery_capsule_descriptor_value_locked(&RecoveryCapsuleDescriptor {
                backup_id: first_backup,
                snapshot_id: "other-owner-page".to_owned(),
                key_epoch: 1,
                committed_at_unix_ms: 1,
                signer_device_id: other_owner,
                total_bytes: 1,
                capsule_digest: blake3::hash(b"other owner page").to_hex().to_string(),
            })
            .expect("other owner descriptor");
        let (_, owner_cursor) = store
            .list_recovery_capsule_descriptors_for_owner(owner, None, None, 1)
            .expect("owner cursor");
        let owner_cursor = owner_cursor.expect("owner has more pages");
        assert!(
            store
                .list_recovery_capsule_descriptors_for_owner(
                    other_owner,
                    None,
                    Some(&owner_cursor),
                    1,
                )
                .is_err()
        );
        assert!(
            store
                .list_recovery_capsule_descriptors_for_owner(
                    owner,
                    Some(first_backup),
                    Some(&owner_cursor),
                    1,
                )
                .is_err()
        );

        let all_feed = store.recovery_capsule_page_feed_directory(owner, None);
        for index in 0..512_u64 {
            fs::write(
                all_feed.join("entries").join(format!("junk-{index:04}")),
                b"junk",
            )
            .expect("unindexed junk");
        }
        fs::write(
            ChunkStore::recovery_capsule_page_entry_path(&all_feed, 3),
            b"corrupt",
        )
        .expect("poison later page");
        RECOVERY_CAPSULE_PAGE_READS.with(|reads| reads.set(0));
        let (_, next) = store
            .list_recovery_capsule_descriptors_for_owner(owner, None, None, 2)
            .expect("first page does not scan later corruption");
        assert_eq!(RECOVERY_CAPSULE_PAGE_READS.with(Cell::get), 3);
        assert!(
            store
                .list_recovery_capsule_descriptors_for_owner(owner, None, next.as_deref(), 2,)
                .is_err(),
            "the page reaching a corrupt direct-sequence record must fail closed"
        );
    }

    #[test]
    fn descriptor_page_deadline_bounds_transaction_lock_wait() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let owner = DeviceId::new();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(20);
        let _held = store
            .transaction_lock
            .lock()
            .expect("hold transaction lock");

        assert!(matches!(
            store.list_recovery_capsule_descriptors_for_owner_with_deadline(
                owner, None, None, 1, deadline,
            ),
            Err(CoreError::ResourceLimit("QUIC operation timeout"))
        ));
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "deadline-aware lock acquisition must not wait indefinitely"
        );
    }

    #[test]
    fn legacy_descriptor_index_migrates_once_into_bounded_page_feeds() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let owner = DeviceId::new();
        let backup_id = BackupId::new();
        let descriptor = RecoveryCapsuleDescriptor {
            backup_id,
            snapshot_id: "legacy-page-entry".to_owned(),
            key_epoch: 1,
            committed_at_unix_ms: 1,
            signer_device_id: owner,
            total_bytes: 1,
            capsule_digest: blake3::hash(b"legacy page").to_hex().to_string(),
        };
        let path = store
            .recovery_capsule_descriptor_path(owner, backup_id, &descriptor.snapshot_id)
            .expect("descriptor path");
        ensure_private_directory(path.parent().expect("descriptor parent"))
            .expect("descriptor directory");
        write_json_atomic(&path, &descriptor, true).expect("legacy descriptor");
        fs::remove_dir_all(directory.path().join("recovery-capsule-pages"))
            .expect("remove modern page index");
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("migrate index");
        assert_eq!(
            reopened
                .list_recovery_capsule_descriptors_for_owner(owner, Some(backup_id), None, 1)
                .expect("migrated page")
                .0,
            vec![descriptor]
        );
        assert!(
            directory
                .path()
                .join("recovery-capsule-pages/schema.json")
                .is_file()
        );
    }

    #[test]
    fn private_upload_attempt_journal_is_monotonic_restart_bounded_and_nofollow() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let peer_device_id = DeviceId::new();
        let provider_device_id = DeviceId::new();
        let mut retained = None;
        for index in 0..MAX_RECOVERY_CAPSULE_UPLOAD_ATTEMPTS {
            let backup_id = BackupId::new();
            let digest = blake3::hash(&index.to_be_bytes()).to_hex().to_string();
            let lease = StorageLease {
                schema_version: PROVIDER_LEASE_SCHEMA_VERSION,
                lease_id: uuid::Uuid::new_v4().to_string(),
                peer_device_id,
                provider_device_id,
                backup_id,
                max_new_bytes: (MAX_RECOVERY_CAPSULE_SEGMENT_BYTES + 1) as u64,
                max_new_objects: 1,
                issued_at_unix_ms: 100,
                expires_at_unix_ms: 1_000,
                nonce: format!("attempt-{index}"),
                signature: "test-signature".to_owned(),
            };
            let mut attempt = RecoveryCapsuleUploadAttempt::new(
                provider_device_id,
                backup_id,
                format!("attempt-{index:04}"),
                digest,
                (MAX_RECOVERY_CAPSULE_SEGMENT_BYTES + 1) as u64,
                2,
                lease,
                format!("upload-{index}"),
            );
            store
                .persist_recovery_capsule_upload_attempt(&attempt)
                .expect("persist attempt");
            if index == 0 {
                attempt.phase = RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment: 0 };
                store
                    .persist_recovery_capsule_upload_attempt(&attempt)
                    .expect("begin attempt");
                attempt.phase = RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment: 1 };
                store
                    .persist_recovery_capsule_upload_attempt(&attempt)
                    .expect("advance attempt");
                let mut regressed = attempt.clone();
                regressed.phase = RecoveryCapsuleUploadAttemptPhase::LeaseAcquired;
                assert!(
                    store
                        .persist_recovery_capsule_upload_attempt(&regressed)
                        .is_err()
                );
                retained = Some(attempt);
            }
        }
        let retained = retained.expect("retained attempt");
        assert!(matches!(
            store.ensure_recovery_capsule_upload_attempt_capacity(
                provider_device_id,
                BackupId::new(),
                "blocked-at-cap",
                blake3::hash(b"blocked").to_hex().as_str(),
            ),
            Err(CoreError::ResourceLimit(
                "recovery capsule upload attempt backpressure"
            ))
        ));
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart attempts");
        assert_eq!(
            reopened
                .load_recovery_capsule_upload_attempt(
                    retained.provider_device_id,
                    retained.backup_id,
                    &retained.snapshot_id,
                    &retained.capsule_digest,
                )
                .expect("load retained"),
            Some(retained.clone())
        );
        reopened
            .complete_recovery_capsule_upload_attempt(&retained)
            .expect("complete retained");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = directory.path().join("outside-attempt.json");
            write_json_atomic(&outside, &retained, true).expect("outside attempt");
            let path = reopened
                .recovery_capsule_upload_attempt_path_for_value(&retained)
                .expect("attempt path");
            symlink(&outside, &path).expect("attempt symlink");
            drop(reopened);
            assert!(ChunkStore::open(directory.path(), 1_048_576).is_err());
        }
    }

    #[test]
    fn provider_write_lease_intent_is_exact_restart_bounded_and_nofollow() {
        let directory = tempdir().expect("temporary store");
        let store = ChunkStore::open(directory.path(), 1_048_576).expect("store");
        let retained = ProviderWriteLeaseIntent::new(
            DeviceId::new(),
            BackupId::new(),
            4_096,
            2,
            uuid::Uuid::new_v4().to_string(),
        );
        store
            .persist_provider_write_lease_intent(&retained)
            .expect("persist retained intent");
        store
            .persist_provider_write_lease_intent(&retained)
            .expect("idempotent exact intent");
        let conflicting = ProviderWriteLeaseIntent::new(
            retained.provider_device_id,
            retained.backup_id,
            retained.maximum_new_bytes,
            retained.maximum_new_objects,
            uuid::Uuid::new_v4().to_string(),
        );
        assert!(matches!(
            store.persist_provider_write_lease_intent(&conflicting),
            Err(CoreError::AuthenticationFailed)
        ));
        for _ in 1..MAX_PROVIDER_WRITE_LEASE_INTENTS {
            store
                .persist_provider_write_lease_intent(&ProviderWriteLeaseIntent::new(
                    DeviceId::new(),
                    BackupId::new(),
                    1,
                    1,
                    uuid::Uuid::new_v4().to_string(),
                ))
                .expect("bounded intent");
        }
        assert!(matches!(
            store.persist_provider_write_lease_intent(&ProviderWriteLeaseIntent::new(
                DeviceId::new(),
                BackupId::new(),
                1,
                1,
                uuid::Uuid::new_v4().to_string(),
            )),
            Err(CoreError::ResourceLimit(
                "provider write lease intent backpressure"
            ))
        ));
        drop(store);

        let reopened = ChunkStore::open(directory.path(), 1_048_576).expect("restart intents");
        assert_eq!(
            reopened
                .load_provider_write_lease_intent(retained.provider_device_id, retained.backup_id,)
                .expect("load retained intent"),
            Some(retained.clone())
        );
        assert_eq!(
            reopened
                .provider_write_lease_intents_for_provider(retained.provider_device_id)
                .expect("list retained intent"),
            vec![retained.clone()]
        );
        reopened
            .complete_provider_write_lease_intent(&retained)
            .expect("complete retained intent");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = directory.path().join("outside-write-intent.json");
            write_json_atomic(&outside, &retained, true).expect("outside intent");
            let name =
                provider_write_lease_intent_name(retained.provider_device_id, retained.backup_id);
            let path = directory.path().join("provider-write-intents").join(name);
            symlink(&outside, &path).expect("intent symlink");
            drop(reopened);
            assert!(ChunkStore::open(directory.path(), 1_048_576).is_err());
        }
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
