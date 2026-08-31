use std::collections::{BTreeMap, BTreeSet};
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_protocol::{
    BackupId, BackupSummary, DeviceId, ExportedDeviceSettings, Manifest, PairingInvitation,
    PeerGrant, PeerRole, RememberedBackup, ReplicaAvailability, ReplicaIntent, SignedRoster,
    StorageLease, TransportBinding,
};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::atomic::{read_json_bounded, sync_directory, write_atomic, write_json_atomic};
use crate::backup::{BackupProgress, options_digest, scan_source_with_chunk_sink};
use crate::manifest::{SignedRosterBuilder, roster_digest, verify_roster};
use crate::recovery::{
    MAX_RECOVERY_KIT_BYTES, RecoveryCapsule, RecoveryKit, RecoveryMasterKey,
    RecoveryProviderDirectoryEntry, RecoveryUnlockKey, load_or_create_recovery_master,
    persist_recovery_master,
};
use crate::replication::ProviderFailure;
use crate::restore::{execute_restore, preview_restore};
use crate::{
    AuthorizedRoot, BackupKey, BackupOptions, BackupResult, ChunkProvider, ChunkStore, CoreError,
    DeviceIdentity, IntegrityReport, KeyProtector, PairingConfirmation, PairingManager,
    PairingSession, ProviderQuotaPolicy, PublicIdentity, RecoveryCapsuleDescriptor,
    ReplicationScheduler, RestoreOptions, RestorePlan, RestoreReport, StoreProvider,
    StoredSnapshot, WrappedSecret, decrypt_manifest, encrypt_manifest, export_settings,
    import_settings, state_secret_context,
};

const NODE_CONFIG_SCHEMA_VERSION: u16 = 1;
const LEGACY_BACKUP_KEY_SCHEMA_VERSION: u16 = 1;
const PROTECTED_BACKUP_KEY_SCHEMA_VERSION: u16 = 2;
const MAX_NODE_CONFIG_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_BACKUP_KEY_BYTES: usize = 16 * 1_024;
const MAX_ROSTER_BYTES: usize = 1_048_576;
const ROSTER_TRANSACTION_SCHEMA_VERSION: u16 = 1;
const MAX_ROSTER_TRANSACTION_BYTES: usize = MAX_NODE_CONFIG_BYTES + MAX_ROSTER_BYTES;
const BACKUP_TRANSACTION_SCHEMA_VERSION: u16 = 1;
const MAX_BACKUP_TRANSACTION_BYTES: usize = 256 * 1_024 * 1_024;
const BACKUP_TERMINAL_RECEIPT_SCHEMA_VERSION: u16 = 1;
/// Maximum completed backup results retained until explicit durable acknowledgement.
pub const MAX_UNACKNOWLEDGED_BACKUP_RESULTS: usize = 8;
const BACKUP_TERMINAL_RECEIPT_SIGNATURE_DOMAIN: &[u8] = b"covalent/backup-terminal-receipt/v1";
const STORAGE_LEASE_SIGNATURE_DOMAIN: &[u8] = b"covalent/storage-lease/v1";
const BACKUP_KEY_SECRET_PURPOSE: &str = "backup-key";

#[cfg(test)]
thread_local! {
    static BACKUP_COMPLETION_FAILPOINT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn backup_completion_failpoint(boundary: u8) -> Result<(), CoreError> {
    BACKUP_COMPLETION_FAILPOINT.with(|failpoint| {
        if failpoint.get() == boundary {
            failpoint.set(0);
            Err(CoreError::InvalidState(format!(
                "backup completion failpoint {boundary}"
            )))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
const fn backup_completion_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

/// Persisted non-key backup state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RememberedBackupState {
    /// Export-safe descriptor.
    pub descriptor: RememberedBackup,
    /// Active local content-key epoch, without the key itself.
    pub key_epoch: u64,
    /// Latest locally committed snapshot.
    pub latest_snapshot_id: Option<String>,
    /// Exact latest explicit provider intent.
    pub replica_intent: ReplicaIntent,
}

/// Durable anti-rollback cursor for one remembered peer's signed roster chain.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RosterCursor {
    /// Highest sequential epoch accepted from this signer.
    pub epoch: u64,
    /// Digest the next epoch must name as its predecessor.
    pub digest: String,
}

/// Authenticated local and explicit-provider availability for one snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SnapshotAvailabilityReport {
    /// Local authenticated object verification.
    pub local: IntegrityReport,
    /// Availability of every provider explicitly selected for this snapshot.
    pub providers: BTreeMap<DeviceId, ReplicaAvailability>,
    /// Safe per-object failure categories for diagnostics.
    pub failures: Vec<ProviderFailure>,
}

/// One latest snapshot authenticated and imported from selected replica catalogs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveredBackup {
    pub backup_id: BackupId,
    pub snapshot_id: String,
    pub source_providers: BTreeSet<DeviceId>,
}

/// Versioned durable node configuration. Private identity and backup keys live separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeConfig {
    /// Persisted schema version.
    pub schema_version: u16,
    /// User-facing device name.
    pub device_name: String,
    /// Persistent multicast discovery preference.
    pub lan_discovery_enabled: bool,
    /// Remembered backups keyed by stable identifier.
    pub remembered_backups: BTreeMap<BackupId, RememberedBackupState>,
    /// Explicitly confirmed peer grants, including revocation tombstones.
    pub trusted_peers: BTreeMap<DeviceId, PeerGrant>,
    /// Exact peer transport pins retained only from a mutually signed pairing transcript.
    #[serde(default)]
    pub trusted_peer_transports: BTreeMap<DeviceId, TransportBinding>,
    /// Highest locally issued roster epoch.
    pub roster_epoch: u64,
    /// Digest chained by the next roster.
    pub roster_digest: String,
    /// High-water cursors for remote signed roster gossip; never implicit trust.
    #[serde(default)]
    pub peer_roster_cursors: BTreeMap<DeviceId, RosterCursor>,
}

impl NodeConfig {
    /// Creates a validated schema-v1 configuration.
    pub fn new(
        device_name: impl Into<String>,
        lan_discovery_enabled: bool,
    ) -> Result<Self, CoreError> {
        let settings =
            ExportedDeviceSettings::new(device_name.into(), lan_discovery_enabled, Vec::new())?;
        Ok(Self {
            schema_version: NODE_CONFIG_SCHEMA_VERSION,
            device_name: settings.device_name,
            lan_discovery_enabled,
            remembered_backups: BTreeMap::new(),
            trusted_peers: BTreeMap::new(),
            trusted_peer_transports: BTreeMap::new(),
            roster_epoch: 0,
            roster_digest: String::new(),
            peer_roster_cursors: BTreeMap::new(),
        })
    }

    fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != NODE_CONFIG_SCHEMA_VERSION {
            return Err(CoreError::InvalidState(
                "unsupported node config schema".to_owned(),
            ));
        }
        let remembered_backups: Vec<_> = self
            .remembered_backups
            .values()
            .map(|state| state.descriptor.clone())
            .collect();
        ExportedDeviceSettings::new(
            self.device_name.clone(),
            self.lan_discovery_enabled,
            remembered_backups,
        )?;
        for (backup_id, state) in &self.remembered_backups {
            if *backup_id != state.descriptor.backup_id
                || state.key_epoch == 0
                || state.replica_intent.selected_providers.len() > 128
                || state
                    .latest_snapshot_id
                    .as_ref()
                    .is_some_and(|value| !valid_identifier(value))
            {
                return Err(CoreError::InvalidState(
                    "inconsistent remembered backup".to_owned(),
                ));
            }
        }
        for (peer_id, grant) in &self.trusted_peers {
            if *peer_id != grant.peer_device_id
                || grant.display_name.trim().is_empty()
                || grant.display_name.len() > 80
                || grant.display_name.chars().any(char::is_control)
            {
                return Err(CoreError::InvalidState(
                    "inconsistent trusted peer".to_owned(),
                ));
            }
            PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())?;
        }
        for (peer_id, binding) in &self.trusted_peer_transports {
            let grant = self
                .trusted_peers
                .get(peer_id)
                .ok_or_else(|| CoreError::InvalidState("orphaned peer transport".to_owned()))?;
            if grant.revoked || binding.peer_id != *peer_id {
                return Err(CoreError::InvalidState(
                    "invalid trusted peer transport".to_owned(),
                ));
            }
            let identity =
                PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())?;
            crate::pairing::validate_transport_binding(binding, &identity, &grant.display_name)?;
        }
        if self.trusted_peers.len() > 128
            || self.trusted_peer_transports.len() > 128
            || self.peer_roster_cursors.len() > 128
        {
            return Err(CoreError::InvalidState(
                "excessive trusted peer state".to_owned(),
            ));
        }
        if (self.roster_epoch == 0 && !self.roster_digest.is_empty())
            || (self.roster_epoch > 0 && !valid_lower_hex_digest(&self.roster_digest))
        {
            return Err(CoreError::InvalidState(
                "invalid local roster cursor".to_owned(),
            ));
        }
        for (signer_id, cursor) in &self.peer_roster_cursors {
            if !self.trusted_peers.contains_key(signer_id)
                || cursor.epoch == 0
                || !valid_lower_hex_digest(&cursor.digest)
            {
                return Err(CoreError::InvalidState(
                    "invalid peer roster cursor".to_owned(),
                ));
            }
        }
        Ok(())
    }

    fn safe_export(&self) -> Result<ExportedDeviceSettings, CoreError> {
        let mut remembered_backups: Vec<_> = self
            .remembered_backups
            .values()
            .map(|state| state.descriptor.clone())
            .collect();
        remembered_backups.sort_by_key(|backup| backup.backup_id);
        Ok(ExportedDeviceSettings::new(
            self.device_name.clone(),
            self.lan_discovery_enabled,
            remembered_backups,
        )?)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn recovery_provider_directory(
    config: &NodeConfig,
    selected: Option<&BTreeSet<DeviceId>>,
) -> Result<Vec<RecoveryProviderDirectoryEntry>, CoreError> {
    let mut entries = Vec::new();
    for (peer_id, grant) in &config.trusted_peers {
        if grant.revoked
            || !grant.roles.contains(&PeerRole::StorageProvider)
            || selected.is_some_and(|ids| !ids.contains(peer_id))
        {
            continue;
        }
        let Some(transport) = config.trusted_peer_transports.get(peer_id) else {
            continue;
        };
        entries.push(RecoveryProviderDirectoryEntry {
            grant: grant.clone(),
            transport: transport.clone(),
        });
    }
    if entries.len() > 128 {
        return Err(CoreError::ResourceLimit("recovery provider directory"));
    }
    Ok(entries)
}

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn storage_lease_signing_bytes(lease: &StorageLease) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Fields<'a> {
        schema_version: u16,
        lease_id: &'a str,
        peer_device_id: DeviceId,
        provider_device_id: DeviceId,
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        issued_at_unix_ms: u64,
        expires_at_unix_ms: u64,
        nonce: &'a str,
    }
    Ok(serde_json::to_vec(&Fields {
        schema_version: lease.schema_version,
        lease_id: &lease.lease_id,
        peer_device_id: lease.peer_device_id,
        provider_device_id: lease.provider_device_id,
        backup_id: lease.backup_id,
        max_new_bytes: lease.max_new_bytes,
        max_new_objects: lease.max_new_objects,
        issued_at_unix_ms: lease.issued_at_unix_ms,
        expires_at_unix_ms: lease.expires_at_unix_ms,
        nonce: &lease.nonce,
    })?)
}

/// Engine construction and resource limits.
#[derive(Clone)]
pub struct EngineOptions {
    /// Durable state directory.
    pub data_directory: PathBuf,
    /// Initial name used only when no config exists.
    pub initial_device_name: String,
    /// Initial discovery preference used only when no config exists.
    pub initial_lan_discovery_enabled: bool,
    /// Hard maximum plaintext chunk size.
    pub maximum_chunk_size: usize,
    /// Hard maximum concurrent provider operations.
    pub maximum_parallel_transfers: usize,
    /// Provider byte/object quotas and real free-space reserve.
    pub provider_quota_policy: ProviderQuotaPolicy,
    /// Platform-backed KEK source required before any private state is opened.
    pub key_protector: Option<Arc<dyn KeyProtector>>,
}

impl EngineOptions {
    /// Production-safe local defaults.
    #[must_use]
    pub fn new(data_directory: impl Into<PathBuf>) -> Self {
        Self {
            data_directory: data_directory.into(),
            initial_device_name: "Covalent node".to_owned(),
            initial_lan_discovery_enabled: false,
            maximum_chunk_size: 1_024 * 1_024,
            maximum_parallel_transfers: 8,
            provider_quota_policy: ProviderQuotaPolicy::default(),
            key_protector: None,
        }
    }

    /// Injects the only source authorized to wrap or open persisted secrets.
    #[must_use]
    pub fn with_key_protector(mut self, key_protector: Arc<dyn KeyProtector>) -> Self {
        self.key_protector = Some(key_protector);
        self
    }
}

impl std::fmt::Debug for EngineOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EngineOptions")
            .field("data_directory", &self.data_directory)
            .field("initial_device_name", &self.initial_device_name)
            .field(
                "initial_lan_discovery_enabled",
                &self.initial_lan_discovery_enabled,
            )
            .field("maximum_chunk_size", &self.maximum_chunk_size)
            .field(
                "maximum_parallel_transfers",
                &self.maximum_parallel_transfers,
            )
            .field("provider_quota_policy", &self.provider_quota_policy)
            .field(
                "key_protector",
                &self.key_protector.as_ref().map(|_| "[CONFIGURED]"),
            )
            .finish()
    }
}

/// Resumable job lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum JobState {
    /// Work may proceed.
    Running = 0,
    /// Work returns a checkpoint-preserving pause.
    Paused = 1,
    /// Work must stop and remain visibly cancelled.
    Cancelled = 2,
}

/// Thread-safe pause/resume/cancel signal.
#[derive(Clone, Debug)]
pub struct JobControl {
    state: Arc<AtomicU8>,
}

impl JobControl {
    /// Starts in running state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(JobState::Running as u8)),
        }
    }

    /// Requests a resumable pause.
    pub fn pause(&self) {
        self.state.store(JobState::Paused as u8, Ordering::Release);
    }

    /// Allows a subsequent invocation to resume from its durable checkpoint.
    pub fn resume(&self) {
        self.state.store(JobState::Running as u8, Ordering::Release);
    }

    /// Requests cancellation.
    pub fn cancel(&self) {
        self.state
            .store(JobState::Cancelled as u8, Ordering::Release);
    }

    /// Current lifecycle state.
    #[must_use]
    pub fn state(&self) -> JobState {
        match self.state.load(Ordering::Acquire) {
            1 => JobState::Paused,
            2 => JobState::Cancelled,
            _ => JobState::Running,
        }
    }

    pub(crate) fn check(&self) -> Result<(), CoreError> {
        match self.state() {
            JobState::Running => Ok(()),
            JobState::Paused => Err(CoreError::Paused),
            JobState::Cancelled => Err(CoreError::Cancelled),
        }
    }
}

impl Default for JobControl {
    fn default() -> Self {
        Self::new()
    }
}

/// Stateful shared engine used by native, CLI, and daemon surfaces.
pub struct Engine {
    options: EngineOptions,
    _state_lock: File,
    identity: Arc<DeviceIdentity>,
    pairing: Arc<PairingManager>,
    store: ChunkStore,
    config_path: PathBuf,
    key_directory: PathBuf,
    config: Mutex<NodeConfig>,
    keys: Mutex<BTreeMap<BackupId, BackupKey>>,
    scheduler: Mutex<ReplicationScheduler>,
    backup_lock: Mutex<()>,
    recovery_master: RecoveryMasterKey,
    key_protector: Arc<dyn KeyProtector>,
}

fn required_key_protector(options: &EngineOptions) -> Result<Arc<dyn KeyProtector>, CoreError> {
    let protector = options
        .key_protector
        .as_ref()
        .cloned()
        .ok_or(CoreError::KeyProtectionLocked)?;
    let version = protector.current_key_version()?;
    if version == 0 {
        return Err(CoreError::KeyProtectionLocked);
    }
    drop(protector.key_encryption_key(version)?);
    Ok(protector)
}

const RECOVERY_BOOTSTRAP_JOURNAL_SCHEMA_VERSION: u16 = 1;
const RECOVERY_BOOTSTRAP_JOURNAL_FILE: &str = "recovery-bootstrap.json";
const MAX_RECOVERY_BOOTSTRAP_JOURNAL_BYTES: usize = 4 * 1_024;
const MAX_RECOVERY_BOOTSTRAP_IDENTITY_BYTES: usize = 16 * 1_024;

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryBootstrapJournal {
    schema_version: u16,
    kit_digest: [u8; 32],
    target_digest: [u8; 32],
}

#[cfg(test)]
thread_local! {
    static RECOVERY_BOOTSTRAP_FAILPOINT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn recovery_bootstrap_failpoint(boundary: u8) -> Result<(), CoreError> {
    RECOVERY_BOOTSTRAP_FAILPOINT.with(|failpoint| {
        if failpoint.get() == boundary {
            failpoint.set(0);
            Err(CoreError::InvalidState(format!(
                "recovery bootstrap failpoint {boundary}"
            )))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
const fn recovery_bootstrap_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

fn validate_recovery_target_identity(
    target_root: &Path,
    expected_device_id: DeviceId,
) -> Result<(), CoreError> {
    let identity: serde_json::Value = read_json_bounded(
        &target_root.join("identity.json"),
        MAX_RECOVERY_BOOTSTRAP_IDENTITY_BYTES,
    )?;
    let actual = identity
        .get("deviceId")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<DeviceId>().ok())
        .ok_or_else(|| {
            CoreError::InvalidState("recovery target identity is missing or invalid".to_owned())
        })?;
    if actual != expected_device_id {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

impl Engine {
    /// Opens, migrates, validates, and recovers durable state.
    pub fn open(mut options: EngineOptions) -> Result<Self, CoreError> {
        if !(1..=32).contains(&options.maximum_parallel_transfers) {
            return Err(CoreError::ResourceLimit("maximum parallel transfers"));
        }
        let key_protector = required_key_protector(&options)?;
        fs::create_dir_all(&options.data_directory).map_err(|source| CoreError::Io {
            operation: "create engine data directory",
            path: options.data_directory.clone(),
            source,
        })?;
        let data_metadata =
            fs::symlink_metadata(&options.data_directory).map_err(|source| CoreError::Io {
                operation: "inspect engine data directory",
                path: options.data_directory.clone(),
                source,
            })?;
        if data_metadata.file_type().is_symlink() || !data_metadata.is_dir() {
            return Err(CoreError::InvalidState(
                "engine data directory is not a real directory".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&options.data_directory, fs::Permissions::from_mode(0o700))
                .map_err(|source| CoreError::Io {
                    operation: "protect engine data directory",
                    path: options.data_directory.clone(),
                    source,
                })?;
        }
        options.data_directory =
            fs::canonicalize(&options.data_directory).map_err(|source| CoreError::Io {
                operation: "canonicalize engine data directory",
                path: options.data_directory.clone(),
                source,
            })?;
        let state_lock = acquire_state_lock(&options.data_directory)?;
        let identity = Arc::new(DeviceIdentity::load_or_create_protected(
            &options.data_directory.join("identity.json"),
            &options.data_directory,
            key_protector.as_ref(),
        )?);
        let recovery_master = load_or_create_recovery_master(
            &options.data_directory.join("recovery-master.json"),
            &options.data_directory,
            key_protector.as_ref(),
        )?;
        let config_path = options.data_directory.join("config.json");
        let mut config = load_or_create_config(&config_path, &options)?;
        recover_roster_transaction(
            &options.data_directory,
            &config_path,
            &mut config,
            &identity,
        )?;
        let key_directory = options.data_directory.join("keys");
        fs::create_dir_all(&key_directory).map_err(|source| CoreError::Io {
            operation: "create backup key directory",
            path: key_directory.clone(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_directory, fs::Permissions::from_mode(0o700)).map_err(
                |source| CoreError::Io {
                    operation: "protect backup key directory",
                    path: key_directory.clone(),
                    source,
                },
            )?;
        }
        migrate_backup_key_directory(
            &key_directory,
            &options.data_directory,
            key_protector.as_ref(),
        )?;
        let store = ChunkStore::open_with_provider_quotas(
            options.data_directory.join("store"),
            options.maximum_chunk_size,
            options.provider_quota_policy.clone(),
        )?;
        recover_backup_transactions(
            &options.data_directory,
            &store,
            &config_path,
            &mut config,
            &identity,
            key_protector.as_ref(),
        )?;
        let pairing = Arc::new(PairingManager::open(
            Arc::clone(&identity),
            config.device_name.clone(),
            options.data_directory.join("pairing-state.json"),
        )?);
        let local_provider = Arc::new(StoreProvider::new(identity.device_id(), store.clone()))
            as Arc<dyn ChunkProvider>;
        let scheduler =
            ReplicationScheduler::new([local_provider], options.maximum_parallel_transfers)?;
        Ok(Self {
            scheduler: Mutex::new(scheduler),
            options,
            _state_lock: state_lock,
            identity,
            pairing,
            store,
            config_path,
            key_directory,
            config: Mutex::new(config),
            keys: Mutex::new(BTreeMap::new()),
            backup_lock: Mutex::new(()),
            recovery_master,
            key_protector,
        })
    }

    /// Recreates the original authenticated recovery principal from a stable kit.
    pub fn recover_from_kit(
        mut options: EngineOptions,
        kit_bytes: &[u8],
        unlock: &RecoveryUnlockKey,
    ) -> Result<Self, CoreError> {
        let key_protector = required_key_protector(&options)?;
        if kit_bytes.len() > MAX_RECOVERY_KIT_BYTES {
            return Err(CoreError::ResourceLimit("recovery kit"));
        }
        let kit: RecoveryKit = serde_json::from_slice(kit_bytes)?;
        let opened = kit.open(unlock)?;
        let requested_root = options.data_directory.clone();
        let target_name = requested_root.file_name().ok_or_else(|| {
            CoreError::InvalidState("recovery target must name one directory".to_owned())
        })?;
        let requested_parent = requested_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(requested_parent).map_err(|source| CoreError::Io {
            operation: "create recovery target parent",
            path: requested_parent.to_path_buf(),
            source,
        })?;
        let parent = fs::canonicalize(requested_parent).map_err(|source| CoreError::Io {
            operation: "canonicalize recovery target parent",
            path: requested_parent.to_path_buf(),
            source,
        })?;
        let target_root = parent.join(target_name);
        let kit_digest = *blake3::hash(kit_bytes).as_bytes();
        let target_digest = *blake3::hash(target_root.as_os_str().as_encoded_bytes()).as_bytes();
        let journal = RecoveryBootstrapJournal {
            schema_version: RECOVERY_BOOTSTRAP_JOURNAL_SCHEMA_VERSION,
            kit_digest,
            target_digest,
        };
        let recovered_device_id = opened.identity.device_id();
        options.data_directory = target_root.clone();
        options.initial_device_name = opened.display_name.clone();

        match fs::symlink_metadata(&target_root) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CoreError::InvalidState(
                        "recovery target is not a real directory".to_owned(),
                    ));
                }
                let journal_path = target_root.join(RECOVERY_BOOTSTRAP_JOURNAL_FILE);
                if journal_path.exists() {
                    let persisted: RecoveryBootstrapJournal =
                        read_json_bounded(&journal_path, MAX_RECOVERY_BOOTSTRAP_JOURNAL_BYTES)?;
                    if persisted != journal {
                        return Err(CoreError::InvalidState(
                            "recovery target belongs to another recovery operation".to_owned(),
                        ));
                    }
                    validate_recovery_target_identity(&target_root, recovered_device_id)?;
                    fs::remove_file(&journal_path).map_err(|source| CoreError::Io {
                        operation: "complete recovered engine publish",
                        path: journal_path,
                        source,
                    })?;
                    sync_directory(&target_root)?;
                    let engine = Self::open(options)?;
                    if engine.device_id() != recovered_device_id {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    return Ok(engine);
                }
                if fs::read_dir(&target_root)
                    .map_err(|source| CoreError::Io {
                        operation: "inspect recovered engine directory",
                        path: target_root.clone(),
                        source,
                    })?
                    .next()
                    .is_some()
                {
                    validate_recovery_target_identity(&target_root, recovered_device_id)?;
                    let engine = Self::open(options)?;
                    if engine.device_id() != recovered_device_id {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    return Ok(engine);
                }
                fs::remove_dir(&target_root).map_err(|source| CoreError::Io {
                    operation: "remove empty recovery target",
                    path: target_root.clone(),
                    source,
                })?;
                sync_directory(&parent)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovery target",
                    path: target_root.clone(),
                    source,
                });
            }
        }

        let staging_id = blake3::hash(target_root.as_os_str().as_encoded_bytes()).to_hex();
        let staging_root = parent.join(format!(
            ".covalent-recovery-{}.staging",
            &staging_id.as_str()[..16]
        ));
        match fs::symlink_metadata(&staging_root) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(CoreError::InvalidState(
                        "recovery staging path is not a real directory".to_owned(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(CoreError::InvalidState(
                            "recovery staging directory permissions are too broad".to_owned(),
                        ));
                    }
                }
                let journal_path = staging_root.join(RECOVERY_BOOTSTRAP_JOURNAL_FILE);
                if !journal_path.exists() {
                    if fs::read_dir(&staging_root)
                        .map_err(|source| CoreError::Io {
                            operation: "inspect incomplete recovery staging directory",
                            path: staging_root.clone(),
                            source,
                        })?
                        .next()
                        .is_some()
                    {
                        return Err(CoreError::InvalidState(
                            "recovery staging journal is missing".to_owned(),
                        ));
                    }
                    fs::remove_dir(&staging_root).map_err(|source| CoreError::Io {
                        operation: "remove empty recovery staging directory",
                        path: staging_root.clone(),
                        source,
                    })?;
                } else {
                    let persisted: RecoveryBootstrapJournal =
                        read_json_bounded(&journal_path, MAX_RECOVERY_BOOTSTRAP_JOURNAL_BYTES)?;
                    if persisted != journal {
                        return Err(CoreError::InvalidState(
                            "recovery staging belongs to another recovery operation".to_owned(),
                        ));
                    }
                    for entry in fs::read_dir(&staging_root).map_err(|source| CoreError::Io {
                        operation: "inspect recovery staging contents",
                        path: staging_root.clone(),
                        source,
                    })? {
                        let entry = entry.map_err(|source| CoreError::Io {
                            operation: "inspect recovery staging entry",
                            path: staging_root.clone(),
                            source,
                        })?;
                        if !matches!(
                            entry.file_name().to_str(),
                            Some(
                                RECOVERY_BOOTSTRAP_JOURNAL_FILE
                                    | "identity.json"
                                    | "recovery-master.json"
                                    | "config.json"
                            )
                        ) {
                            return Err(CoreError::InvalidState(
                                "recovery staging contains an unexpected entry".to_owned(),
                            ));
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect recovery staging directory",
                    path: staging_root.clone(),
                    source,
                });
            }
        }
        if !staging_root.exists() {
            fs::create_dir(&staging_root).map_err(|source| CoreError::Io {
                operation: "create recovery staging directory",
                path: staging_root.clone(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&staging_root, fs::Permissions::from_mode(0o700)).map_err(
                    |source| CoreError::Io {
                        operation: "protect recovery staging directory",
                        path: staging_root.clone(),
                        source,
                    },
                )?;
            }
            write_json_atomic(
                &staging_root.join(RECOVERY_BOOTSTRAP_JOURNAL_FILE),
                &journal,
                true,
            )?;
        }
        recovery_bootstrap_failpoint(1)?;

        opened.identity.persist_recovered_protected(
            &staging_root.join("identity.json"),
            &target_root,
            key_protector.as_ref(),
        )?;
        recovery_bootstrap_failpoint(2)?;
        persist_recovery_master(
            &staging_root.join("recovery-master.json"),
            &target_root,
            &opened.master,
            key_protector.as_ref(),
        )?;
        let mut recovered_config = NodeConfig::new(opened.display_name.clone(), false)?;
        for entry in opened.provider_directory {
            let peer_id = entry.grant.peer_device_id;
            recovered_config.trusted_peers.insert(peer_id, entry.grant);
            recovered_config
                .trusted_peer_transports
                .insert(peer_id, entry.transport);
        }
        recovered_config.validate()?;
        write_json_atomic(&staging_root.join("config.json"), &recovered_config, true)?;
        sync_directory(&staging_root)?;
        recovery_bootstrap_failpoint(3)?;
        #[cfg(any(target_os = "linux", target_vendor = "apple", target_os = "redox"))]
        {
            use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};

            let parent_descriptor = open(
                &parent,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::empty(),
            )
            .map_err(|error| CoreError::Io {
                operation: "open recovery parent for atomic publish",
                path: parent.clone(),
                source: std::io::Error::from_raw_os_error(error.raw_os_error()),
            })?;
            renameat_with(
                &parent_descriptor,
                staging_root
                    .file_name()
                    .expect("staging path has a filename"),
                &parent_descriptor,
                target_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| CoreError::Io {
                operation: "atomically publish recovered engine directory without replacement",
                path: target_root.clone(),
                source: std::io::Error::from_raw_os_error(error.raw_os_error()),
            })?;
        }
        #[cfg(not(any(target_os = "linux", target_vendor = "apple", target_os = "redox")))]
        fs::rename(&staging_root, &target_root).map_err(|source| CoreError::Io {
            operation: "atomically publish recovered engine directory",
            path: target_root.clone(),
            source,
        })?;
        sync_directory(&parent)?;
        recovery_bootstrap_failpoint(4)?;
        let published_journal = target_root.join(RECOVERY_BOOTSTRAP_JOURNAL_FILE);
        fs::remove_file(&published_journal).map_err(|source| CoreError::Io {
            operation: "complete recovered engine publish",
            path: published_journal,
            source,
        })?;
        sync_directory(&target_root)?;
        let engine = Self::open(options)?;
        if engine.device_id() != recovered_device_id {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(engine)
    }

    /// Exports one stable kit. Future snapshot capsules remain recoverable without re-export.
    pub fn export_recovery_kit(&self, unlock: &RecoveryUnlockKey) -> Result<Vec<u8>, CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let provider_directory = recovery_provider_directory(&config, None)?;
        let kit = RecoveryKit::seal(
            &self.identity,
            &config.device_name,
            &self.recovery_master,
            unlock,
            provider_directory,
        )?;
        let bytes = serde_json::to_vec_pretty(&kit)?;
        if bytes.len() > MAX_RECOVERY_KIT_BYTES {
            return Err(CoreError::ResourceLimit("recovery kit"));
        }
        Ok(bytes)
    }

    /// Authenticates connected-provider catalogs and imports the latest snapshot per backup.
    pub fn import_recovery_catalogs(&self) -> Result<Vec<RecoveredBackup>, CoreError> {
        let scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        let mut candidates =
            BTreeMap::<(BackupId, String), (RecoveryCapsule, BTreeSet<DeviceId>)>::new();
        for (provider_id, capsule) in scheduler.recovery_capsules()? {
            let key = (capsule.backup_id, capsule.snapshot_id.clone());
            match candidates.get_mut(&key) {
                Some((incumbent, providers)) => {
                    if incumbent != &capsule {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    providers.insert(provider_id);
                }
                None => {
                    candidates.insert(key, (capsule, BTreeSet::from([provider_id])));
                }
            }
        }
        let mut latest = BTreeMap::<BackupId, (RecoveryCapsule, BTreeSet<DeviceId>)>::new();
        for ((backup_id, _), candidate) in candidates {
            let replace = latest.get(&backup_id).is_none_or(|(current, _)| {
                (candidate.0.committed_at_unix_ms, &candidate.0.snapshot_id)
                    > (current.committed_at_unix_ms, &current.snapshot_id)
            });
            if replace {
                latest.insert(backup_id, candidate);
            }
        }
        let mut recovered = Vec::with_capacity(latest.len());
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let mut candidate_config = config.clone();
        for (backup_id, (capsule, providers)) in latest {
            let opened = capsule.open(&self.recovery_master, &self.identity.public_identity())?;
            let manifest = decrypt_manifest(
                &opened.snapshot.envelope,
                &opened.backup_key,
                &self.identity.public_identity(),
            )?;
            let capsule_provider_ids: BTreeSet<_> = opened
                .provider_directory
                .iter()
                .map(|entry| entry.grant.peer_device_id)
                .collect();
            if !capsule_provider_ids.is_empty()
                && (capsule_provider_ids != manifest.replica_intent.selected_providers
                    || !providers.is_subset(&capsule_provider_ids))
            {
                return Err(CoreError::AuthenticationFailed);
            }
            if manifest.backup_id != backup_id
                || manifest.snapshot_id != opened.snapshot.snapshot_id
                || manifest.created_at_unix_ms != opened.snapshot.committed_at_unix_ms
                || !manifest_locators_match(&manifest, &opened.snapshot.chunk_locators)
            {
                return Err(CoreError::AuthenticationFailed);
            }
            persist_or_validate_backup_key_file(
                &self.key_path(backup_id),
                &self.options.data_directory,
                backup_id,
                &opened.backup_key,
                self.key_protector.as_ref(),
            )?;
            self.keys
                .lock()
                .map_err(|_| CoreError::Synchronization)?
                .insert(backup_id, opened.backup_key);
            self.store.commit_recovery_snapshot(&opened.snapshot)?;
            candidate_config.remembered_backups.insert(
                backup_id,
                RememberedBackupState {
                    descriptor: RememberedBackup {
                        backup_id,
                        name: opened.backup_display_name,
                        owner_device_id: self.device_id(),
                    },
                    key_epoch: capsule.key_epoch,
                    latest_snapshot_id: Some(capsule.snapshot_id.clone()),
                    replica_intent: manifest.replica_intent,
                },
            );
            recovered.push(RecoveredBackup {
                backup_id,
                snapshot_id: capsule.snapshot_id,
                source_providers: providers,
            });
        }
        candidate_config.validate()?;
        write_json_atomic(&self.config_path, &candidate_config, true)?;
        *config = candidate_config;
        recovered.sort_by_key(|item| item.backup_id);
        Ok(recovered)
    }

    /// Public local identity.
    #[must_use]
    pub fn public_identity(&self) -> PublicIdentity {
        self.identity.public_identity()
    }

    /// Stable local device identifier.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.identity.device_id()
    }

    /// Local encrypted store.
    #[must_use]
    pub const fn store(&self) -> &ChunkStore {
        &self.store
    }

    /// Shared invitation manager.
    #[must_use]
    pub fn pairing_manager(&self) -> Arc<PairingManager> {
        Arc::clone(&self.pairing)
    }

    /// Accepts a transferred invitation using this node's protected identity.
    pub fn accept_pairing(
        &self,
        invitation: PairingInvitation,
        responder_name: impl Into<String>,
        responder_roles: BTreeSet<PeerRole>,
        inviter_roles: BTreeSet<PeerRole>,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        PairingSession::accept_with_roles(
            invitation,
            &self.identity,
            responder_name,
            responder_roles,
            inviter_roles,
            now_unix_ms,
        )
    }

    /// Accepts an invitation while binding this node's exact TLS endpoint.
    pub fn accept_pairing_with_transport(
        &self,
        invitation: PairingInvitation,
        responder_transport: TransportBinding,
        responder_roles: BTreeSet<PeerRole>,
        inviter_roles: BTreeSet<PeerRole>,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        PairingSession::accept_with_transport(
            invitation,
            &self.identity,
            responder_transport,
            responder_roles,
            inviter_roles,
            now_unix_ms,
        )
    }

    /// Records the responder user's explicit short-code confirmation.
    pub fn confirm_pairing_as_responder(
        &self,
        session: &mut PairingSession,
        displayed: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        session.confirm_responder(displayed, &self.identity, now_unix_ms)
    }

    /// Records the inviter user's explicit short-code confirmation.
    pub fn confirm_pairing_as_inviter(
        &self,
        session: &mut PairingSession,
        displayed: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.pairing
            .confirm_inviter(session, displayed, now_unix_ms)
    }

    /// Consumes an invitation and durably trusts the confirmed responder grant.
    pub fn finalize_pairing_as_inviter(
        &self,
        session: &PairingSession,
        now_unix_ms: u64,
    ) -> Result<PairingConfirmation, CoreError> {
        let confirmation = self.pairing.finalize(session, now_unix_ms)?;
        self.trust_peer_with_transport(
            confirmation.inviter_grant.clone(),
            confirmation.responder_transport.clone(),
        )?;
        Ok(confirmation)
    }

    /// Verifies the inviter approval and durably trusts the confirmed inviter grant.
    pub fn finalize_pairing_as_responder(
        &self,
        session: &PairingSession,
        now_unix_ms: u64,
    ) -> Result<PairingConfirmation, CoreError> {
        let confirmation = session.finalize_for_responder(&self.identity, now_unix_ms)?;
        self.trust_peer_with_transport(
            confirmation.responder_grant.clone(),
            confirmation.inviter_transport.clone(),
        )?;
        Ok(confirmation)
    }

    /// Signs a transport transcript without exposing the private identity key.
    #[must_use]
    pub fn sign_transport_transcript(&self, transcript: &[u8]) -> String {
        self.sign_transport_transcript_with_domain(b"covalent/authenticated-quic/v1", transcript)
    }

    /// Signs a versioned transport transcript under an explicit protocol domain.
    #[must_use]
    pub fn sign_transport_transcript_with_domain(
        &self,
        domain: &[u8],
        transcript: &[u8],
    ) -> String {
        self.identity.sign(domain, transcript)
    }

    /// Issues and durably reserves one backup-scoped remote storage lease.
    pub fn issue_storage_lease(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        issued_at_unix_ms: u64,
        expires_at_unix_ms: u64,
    ) -> Result<StorageLease, CoreError> {
        self.issue_storage_lease_idempotent(
            peer_device_id,
            backup_id,
            max_new_bytes,
            max_new_objects,
            issued_at_unix_ms,
            expires_at_unix_ms,
            &uuid::Uuid::new_v4().to_string(),
        )
    }

    /// Issues the same signed lease for every retry of one durable acquisition identity.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_storage_lease_idempotent(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        issued_at_unix_ms: u64,
        expires_at_unix_ms: u64,
        acquisition_id: &str,
    ) -> Result<StorageLease, CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupWriter)?;
        if !matches!(
            uuid::Uuid::parse_str(acquisition_id),
            Ok(value) if value.hyphenated().to_string() == acquisition_id
        ) {
            return Err(CoreError::AuthenticationFailed);
        }
        let mut lease = StorageLease {
            schema_version: 1,
            lease_id: acquisition_id.to_owned(),
            peer_device_id,
            provider_device_id: self.device_id(),
            backup_id,
            max_new_bytes,
            max_new_objects,
            issued_at_unix_ms,
            expires_at_unix_ms,
            nonce: acquisition_id.to_owned(),
            signature: String::new(),
        };
        lease.signature = self.identity.sign(
            STORAGE_LEASE_SIGNATURE_DOMAIN,
            &storage_lease_signing_bytes(&lease)?,
        );
        self.store.reserve_provider_lease_idempotent(&lease)
    }

    /// Authenticates, cancels, and compacts one exact provider-issued lease.
    pub fn cancel_storage_lease(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.verify_storage_lease_identity(peer_device_id, lease)?;
        self.store.cancel_provider_lease(lease, now_unix_ms)
    }

    /// Verifies and atomically consumes a lease for one opaque remote chunk.
    pub fn put_leased_provider_record(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        locator: &str,
        record: &[u8],
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        self.verify_storage_lease(peer_device_id, lease, now_unix_ms)?;
        self.store.put_provider_record_leased(
            peer_device_id,
            lease.backup_id,
            lease,
            locator,
            record,
            now_unix_ms,
        )
    }

    /// Authorizes one bounded provider read batch for the exact peer and backup scope.
    pub fn authorize_provider_read_batch(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        locators: &[String],
    ) -> Result<(), CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupReader)?;
        self.store
            .authorize_provider_record_batch(peer_device_id, backup_id, locators)
    }

    /// Verifies and atomically consumes a lease for one owner-signed recovery capsule.
    pub fn put_leased_recovery_capsule(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        capsule: &RecoveryCapsule,
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        self.verify_storage_lease(peer_device_id, lease, now_unix_ms)?;
        if capsule.signer_device_id != peer_device_id || capsule.backup_id != lease.backup_id {
            return Err(CoreError::AuthenticationFailed);
        }
        self.store.put_recovery_capsule_leased(
            peer_device_id,
            lease.backup_id,
            lease,
            capsule,
            now_unix_ms,
        )
    }

    /// Starts one segmented recovery-capsule upload after verifying its lease.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_leased_recovery_capsule_upload(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        upload_id: &str,
        total_bytes: u64,
        total_segments: u32,
        capsule_digest: &str,
        descriptor: &RecoveryCapsuleDescriptor,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.verify_storage_lease(peer_device_id, lease, now_unix_ms)?;
        self.store.begin_recovery_capsule_upload(
            peer_device_id,
            lease.backup_id,
            lease,
            upload_id,
            total_bytes,
            total_segments,
            capsule_digest,
            descriptor,
            now_unix_ms,
        )
    }

    /// Persists one authenticated segment under an existing capsule upload lease.
    #[allow(clippy::too_many_arguments)]
    pub fn put_leased_recovery_capsule_segment(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        upload_id: &str,
        index: u32,
        segment: &[u8],
        segment_digest: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.verify_storage_lease(peer_device_id, lease, now_unix_ms)?;
        self.store.put_recovery_capsule_segment(
            peer_device_id,
            lease.backup_id,
            lease,
            upload_id,
            index,
            segment,
            segment_digest,
            now_unix_ms,
        )
    }

    /// Verifies and commits all ordered capsule segments through lease accounting.
    pub fn commit_leased_recovery_capsule_upload(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        upload_id: &str,
        now_unix_ms: u64,
    ) -> Result<bool, CoreError> {
        self.verify_storage_lease(peer_device_id, lease, now_unix_ms)?;
        self.store.commit_recovery_capsule_upload(
            peer_device_id,
            lease.backup_id,
            lease,
            upload_id,
            now_unix_ms,
        )
    }

    /// Authenticates an explicit acknowledgement of one terminal segmented
    /// capsule-upload result. Expired leases remain valid identities for this
    /// receipt-only operation.
    pub fn acknowledge_recovery_capsule_upload(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<(), CoreError> {
        self.verify_storage_lease_identity(peer_device_id, lease)?;
        self.store
            .acknowledge_recovery_capsule_upload(lease, upload_id)
    }

    /// Authenticates an exact terminal capsule probe without reserving new quota.
    pub fn recovery_capsule_is_committed_for_peer(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        total_bytes: u64,
        capsule_digest: &str,
    ) -> Result<bool, CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupWriter)?;
        self.store.recovery_capsule_is_committed_for_owner(
            peer_device_id,
            backup_id,
            snapshot_id,
            total_bytes,
            capsule_digest,
        )
    }

    /// Lists one owner-scoped page of bounded recovery capsule descriptors.
    pub fn recovery_capsule_descriptors_for_peer(
        &self,
        peer_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
    ) -> Result<(Vec<RecoveryCapsuleDescriptor>, Option<String>), CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupReader)?;
        self.store.list_recovery_capsule_descriptors_for_owner(
            peer_device_id,
            backup_id,
            cursor,
            limit,
        )
    }

    /// Lists one owner page while enforcing the transport worker's deadline.
    pub fn recovery_capsule_descriptors_for_peer_with_deadline(
        &self,
        peer_device_id: DeviceId,
        backup_id: Option<BackupId>,
        cursor: Option<&str>,
        limit: u16,
        deadline: Instant,
    ) -> Result<(Vec<RecoveryCapsuleDescriptor>, Option<String>), CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupReader)?;
        self.store
            .list_recovery_capsule_descriptors_for_owner_with_deadline(
                peer_device_id,
                backup_id,
                cursor,
                limit,
                deadline,
            )
    }

    /// Reads one bounded owner-scoped capsule segment.
    pub fn recovery_capsule_segment_for_peer(
        &self,
        peer_device_id: DeviceId,
        backup_id: BackupId,
        snapshot_id: &str,
        offset: u64,
        maximum_bytes: u32,
    ) -> Result<(Vec<u8>, u64, String), CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupReader)?;
        self.store.read_recovery_capsule_segment_for_owner(
            peer_device_id,
            backup_id,
            snapshot_id,
            offset,
            maximum_bytes,
        )
    }

    fn verify_storage_lease(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.verify_storage_lease_identity(peer_device_id, lease)?;
        if lease.expires_at_unix_ms <= now_unix_ms {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(())
    }

    fn verify_storage_lease_identity(
        &self,
        peer_device_id: DeviceId,
        lease: &StorageLease,
    ) -> Result<(), CoreError> {
        self.authorized_peer(peer_device_id, PeerRole::BackupWriter)?;
        if lease.schema_version != 1
            || lease.peer_device_id != peer_device_id
            || lease.provider_device_id != self.device_id()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        self.public_identity().verify(
            STORAGE_LEASE_SIGNATURE_DOMAIN,
            &storage_lease_signing_bytes(lease)?,
            &lease.signature,
        )
    }

    /// Resolves one non-revoked peer with an exact required role.
    pub fn authorized_peer(
        &self,
        peer_id: DeviceId,
        required_role: PeerRole,
    ) -> Result<PublicIdentity, CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let grant = config
            .trusted_peers
            .get(&peer_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if grant.revoked {
            return Err(CoreError::PeerRevoked);
        }
        if !grant.roles.contains(&required_role) {
            return Err(CoreError::UnselectedProvider);
        }
        PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
    }

    /// Resolves one non-revoked remembered peer without expanding its roles.
    pub fn trusted_peer_identity(&self, peer_id: DeviceId) -> Result<PublicIdentity, CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let grant = config
            .trusted_peers
            .get(&peer_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if grant.revoked {
            return Err(CoreError::PeerRevoked);
        }
        PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
    }

    /// Returns the latest locally issued signed roster, if pairing has created one.
    pub fn current_roster(&self) -> Result<Option<SignedRoster>, CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        if config.roster_epoch == 0 {
            return Ok(None);
        }
        let path = self.options.data_directory.join("roster.json");
        let roster: SignedRoster = read_json_bounded(&path, MAX_ROSTER_BYTES)?;
        validate_local_roster(&roster, &config, &self.identity)?;
        if roster_digest(&roster)? != config.roster_digest {
            return Err(CoreError::InvalidState(
                "local roster digest does not match durable cursor".to_owned(),
            ));
        }
        Ok(Some(roster))
    }

    /// Snapshot of validated non-key config.
    pub fn config(&self) -> Result<NodeConfig, CoreError> {
        Ok(self
            .config
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone())
    }

    /// Lists authoritative remembered backups with validated immutable snapshot metadata.
    pub fn list_backups(&self) -> Result<Vec<BackupSummary>, CoreError> {
        let config = self.config()?;
        let snapshots = self
            .store
            .list_snapshots()?
            .into_iter()
            .map(|snapshot| {
                let snapshot_id = snapshot.snapshot_id.clone();
                self.authenticate_snapshot(snapshot.backup_id, &snapshot_id, snapshot)
                    .map(|authenticated| authenticated.snapshot)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut by_backup = BTreeMap::<BackupId, Vec<StoredSnapshot>>::new();
        for snapshot in snapshots {
            by_backup
                .entry(snapshot.backup_id)
                .or_default()
                .push(snapshot);
        }
        let mut summaries = Vec::with_capacity(config.remembered_backups.len());
        for (backup_id, remembered) in config.remembered_backups {
            let snapshots = by_backup.remove(&backup_id).unwrap_or_default();
            let latest_snapshot = match remembered.latest_snapshot_id.as_deref() {
                Some(snapshot_id) => Some(
                    snapshots
                        .iter()
                        .find(|snapshot| snapshot.snapshot_id == snapshot_id)
                        .ok_or_else(|| {
                            CoreError::InvalidState(
                                "remembered latest snapshot metadata is missing".to_owned(),
                            )
                        })?,
                ),
                None => None,
            };
            summaries.push(BackupSummary {
                backup_id,
                name: remembered.descriptor.name,
                owner_device_id: remembered.descriptor.owner_device_id,
                latest_snapshot_id: latest_snapshot.map(|snapshot| snapshot.snapshot_id.clone()),
                latest_committed_at_unix_ms: latest_snapshot
                    .map(|snapshot| snapshot.committed_at_unix_ms),
                snapshot_count: u64::try_from(snapshots.len())
                    .map_err(|_| CoreError::ResourceLimit("snapshot count"))?,
                selected_provider_ids: remembered.replica_intent.selected_providers,
            });
        }
        summaries.sort_by(|left, right| {
            right
                .latest_committed_at_unix_ms
                .cmp(&left.latest_committed_at_unix_ms)
                .then_with(|| left.backup_id.cmp(&right.backup_id))
        });
        Ok(summaries)
    }

    /// Persists one mutually confirmed grant.
    pub fn trust_peer(&self, grant: PeerGrant) -> Result<SignedRoster, CoreError> {
        self.trust_peer_with_transport(grant, None)
    }

    fn trust_peer_with_transport(
        &self,
        grant: PeerGrant,
        transport: Option<TransportBinding>,
    ) -> Result<SignedRoster, CoreError> {
        if grant.revoked {
            return Err(CoreError::InvalidState(
                "new trust grant cannot already be revoked".to_owned(),
            ));
        }
        let identity =
            PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())?;
        if let Some(binding) = transport.as_ref() {
            crate::pairing::validate_transport_binding(binding, &identity, &grant.display_name)?;
        }
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let mut candidate = config.clone();
        if candidate.trusted_peers.len() >= 128
            && !candidate.trusted_peers.contains_key(&grant.peer_device_id)
        {
            return Err(CoreError::ResourceLimit("trusted peers"));
        }
        let peer_id = grant.peer_device_id;
        candidate.trusted_peers.insert(peer_id, grant);
        if let Some(binding) = transport {
            candidate.trusted_peer_transports.insert(peer_id, binding);
        } else {
            candidate.trusted_peer_transports.remove(&peer_id);
        }
        let roster = self.issue_roster_locked(&config, &mut candidate)?;
        *config = candidate;
        Ok(roster)
    }

    /// Returns an exact transport pin retained from a mutually signed pairing transcript.
    pub fn trusted_peer_transport(
        &self,
        peer_id: DeviceId,
        required_role: PeerRole,
    ) -> Result<TransportBinding, CoreError> {
        self.authorized_peer(peer_id, required_role)?;
        self.config
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .trusted_peer_transports
            .get(&peer_id)
            .cloned()
            .ok_or(CoreError::IdentityMismatch)
    }

    /// Verifies and durably advances one peer's sequential signed roster gossip.
    /// The roster is informational and never grants local trust automatically.
    pub fn accept_peer_roster(&self, roster: SignedRoster) -> Result<RosterCursor, CoreError> {
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let grant = config
            .trusted_peers
            .get(&roster.signer_device_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if grant.revoked {
            return Err(CoreError::PeerRevoked);
        }
        let signer = PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())?;
        let previous = config
            .peer_roster_cursors
            .get(&roster.signer_device_id)
            .cloned()
            .unwrap_or_default();
        verify_roster(&roster, &signer, previous.epoch, &previous.digest)?;
        let cursor = RosterCursor {
            epoch: roster.epoch,
            digest: roster_digest(&roster)?,
        };
        let mut candidate = config.clone();
        candidate
            .peer_roster_cursors
            .insert(roster.signer_device_id, cursor.clone());
        candidate.validate()?;
        commit_roster_transaction(
            &self.options.data_directory,
            &self.config_path,
            &config,
            PendingRosterCommit::Peer {
                schema_version: ROSTER_TRANSACTION_SCHEMA_VERSION,
                roster,
                config: candidate.clone(),
            },
            &self.identity,
        )?;
        *config = candidate;
        Ok(cursor)
    }

    /// Revokes a peer with a signed persistent tombstone.
    pub fn revoke_peer(&self, peer_id: DeviceId) -> Result<SignedRoster, CoreError> {
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let mut candidate = config.clone();
        let grant = candidate
            .trusted_peers
            .get_mut(&peer_id)
            .ok_or(CoreError::IdentityMismatch)?;
        grant.revoked = true;
        candidate.trusted_peer_transports.remove(&peer_id);
        let roster = self.issue_roster_locked(&config, &mut candidate)?;
        *config = candidate;
        drop(config);
        let mut scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        *scheduler = scheduler.without_provider(peer_id);
        Ok(roster)
    }

    /// Registers connected providers only when their storage role is explicitly trusted.
    pub fn set_connected_providers(
        &self,
        mut providers: Vec<Arc<dyn ChunkProvider>>,
    ) -> Result<(), CoreError> {
        if !providers
            .iter()
            .any(|provider| provider.device_id() == self.device_id())
        {
            providers.push(Arc::new(StoreProvider::new(
                self.device_id(),
                self.store.clone(),
            )));
        }
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        for provider in &providers {
            if provider.device_id() == self.device_id() {
                continue;
            }
            let grant = config
                .trusted_peers
                .get(&provider.device_id())
                .ok_or(CoreError::IdentityMismatch)?;
            if grant.revoked || !grant.roles.contains(&PeerRole::StorageProvider) {
                return Err(if grant.revoked {
                    CoreError::PeerRevoked
                } else {
                    CoreError::UnselectedProvider
                });
            }
        }
        drop(config);
        *self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)? =
            ReplicationScheduler::new(providers, self.options.maximum_parallel_transfers)?;
        Ok(())
    }

    /// Exports only device name, discovery preference, and remembered descriptors.
    pub fn export_settings(&self) -> Result<Vec<u8>, CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        export_settings(&config.safe_export()?)
    }

    /// Imports safe settings only after explicit confirmation; keys and trust remain untouched.
    pub fn import_settings(&self, bytes: &[u8], confirmed: bool) -> Result<(), CoreError> {
        if !confirmed {
            return Err(CoreError::SettingsImportNotConfirmed);
        }
        let imported = import_settings(bytes)?;
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let mut candidate = config.clone();
        candidate.device_name = imported.device_name;
        candidate.lan_discovery_enabled = imported.lan_discovery_enabled;
        let existing = std::mem::take(&mut candidate.remembered_backups);
        candidate.remembered_backups = imported
            .remembered_backups
            .into_iter()
            .map(|descriptor| {
                let state = existing
                    .get(&descriptor.backup_id)
                    .cloned()
                    .map(|mut state| {
                        state.descriptor = descriptor.clone();
                        state
                    })
                    .unwrap_or_else(|| RememberedBackupState {
                        descriptor: descriptor.clone(),
                        key_epoch: 1,
                        latest_snapshot_id: None,
                        replica_intent: ReplicaIntent::default(),
                    });
                (descriptor.backup_id, state)
            })
            .collect();
        candidate.validate()?;
        write_json_atomic(&self.config_path, &candidate, true)?;
        let device_name = candidate.device_name.clone();
        *config = candidate;
        drop(config);
        self.pairing.update_device_name(device_name)
    }

    /// Runs a complete local encrypted backup and explicit provider replication.
    ///
    /// A signed result remains retryable by job ID until
    /// [`Self::acknowledge_backup_result`] succeeds. Starting a ninth distinct
    /// job before acknowledging an earlier result returns
    /// `CoreError::ResourceLimit("unacknowledged backup terminal results")`
    /// before source scanning or replication begins.
    pub fn backup(
        &self,
        source_root: impl AsRef<Path>,
        options: &BackupOptions,
        control: &JobControl,
        mut progress_callback: impl FnMut(&BackupProgress),
    ) -> Result<BackupResult, CoreError> {
        let _backup_guard = self
            .backup_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let source_root = source_root.as_ref();
        let source_root_digest = blake3::hash(source_root.as_os_str().as_encoded_bytes())
            .to_hex()
            .to_string();
        let request_digest = options_digest(options)?;
        if let Some(result) = self.load_or_complete_backup_terminal_receipt(
            options,
            &request_digest,
            &source_root_digest,
        )? {
            let _ = self.repair_backup_completion(&options.job_id);
            return Ok(result);
        }
        let receipt_path = self.backup_terminal_receipt_path(&options.job_id)?;
        let receipt_directory = receipt_path.parent().ok_or_else(|| {
            CoreError::InvalidState("backup terminal receipt has no parent".to_owned())
        })?;
        ensure_private_state_directory(receipt_directory)?;
        self.ensure_backup_terminal_receipt_capacity(&receipt_path)?;
        self.validate_backup_transition(options)?;
        {
            let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
            for provider_id in &options.replica_intent.selected_providers {
                if *provider_id == self.device_id() {
                    continue;
                }
                let grant = config
                    .trusted_peers
                    .get(provider_id)
                    .ok_or(CoreError::IdentityMismatch)?;
                if grant.revoked {
                    return Err(CoreError::PeerRevoked);
                }
                if !grant.roles.contains(&PeerRole::StorageProvider) {
                    return Err(CoreError::UnselectedProvider);
                }
            }
        }
        let key = self.load_or_create_backup_key(options.backup_id)?;
        let scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        let pipeline = scheduler.start_pipeline(
            self.store.clone(),
            options.replica_intent.clone(),
            control.clone(),
            options.backup_id,
        );
        let scanned = scan_source_with_chunk_sink(
            source_root,
            options,
            &key,
            &self.store,
            control,
            &mut progress_callback,
            &mut |locators| pipeline.submit(locators),
        )?;
        control.check()?;
        let mut replication = pipeline.finish()?;
        let mut manifest = scanned.manifest;
        manifest.provider_acknowledgements = replication.acknowledgements.clone();
        manifest.validate()?;
        let envelope = encrypt_manifest(&manifest, options.key_epoch, &key, &self.identity)?;
        let stored_snapshot = StoredSnapshot::new(
            options.backup_id,
            options.snapshot_id.clone(),
            envelope,
            scanned.chunk_locators,
            options.created_at_unix_ms,
        )?;
        let recovery_provider_directory = {
            let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
            recovery_provider_directory(&config, Some(&options.replica_intent.selected_providers))?
        };
        let recovery_capsule = RecoveryCapsule::seal(
            &stored_snapshot,
            &options.display_name,
            &key,
            &self.recovery_master,
            &self.identity,
            recovery_provider_directory,
        )?;
        self.store.put_recovery_capsule(&recovery_capsule)?;
        scheduler.replicate_recovery_capsule(
            &options.replica_intent,
            &recovery_capsule,
            &mut replication,
        );
        let remembered = remembered_backup_state(options, self.device_id())?;
        let result = BackupResult {
            manifest,
            stored_snapshot: stored_snapshot.clone(),
            progress: scanned.progress,
            replication,
        };
        let transaction = PendingBackupCommit {
            schema_version: BACKUP_TRANSACTION_SCHEMA_VERSION,
            snapshot: stored_snapshot.clone(),
            remembered: remembered.clone(),
        };
        let transaction_path = self
            .options
            .data_directory
            .join("transactions")
            .join(format!("{}.json", options.job_id));
        self.persist_backup_terminal_receipt(options, request_digest, source_root_digest, &result)?;
        write_json_atomic(&transaction_path, &transaction, true)?;
        self.store.commit_snapshot(&stored_snapshot)?;
        self.remember_backup_state(options.backup_id, remembered)?;
        let _ = self.repair_backup_completion(&options.job_id);
        Ok(result)
    }

    /// Durably acknowledges delivery of one terminal backup result.
    ///
    /// Until this succeeds, the signed result remains retryable by job ID and
    /// counts against the bounded terminal-result window.
    pub fn acknowledge_backup_result(&self, job_id: &str) -> Result<(), CoreError> {
        let _backup_guard = self
            .backup_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        if self
            .read_authenticated_backup_terminal_receipt(job_id)?
            .is_none()
        {
            return Ok(());
        }
        let path = self.backup_terminal_receipt_path(job_id)?;
        fs::remove_file(&path).map_err(|source| CoreError::Io {
            operation: "acknowledge backup terminal result",
            path: path.clone(),
            source,
        })?;
        sync_directory(path.parent().ok_or_else(|| {
            CoreError::InvalidState("backup terminal receipt has no parent".to_owned())
        })?)
    }

    /// Returns the backup ID from one signed unacknowledged terminal result.
    /// Native/API callers use this to preserve a server-generated backup ID
    /// while retrying a response-lost job.
    pub fn unacknowledged_backup_id(&self, job_id: &str) -> Result<Option<BackupId>, CoreError> {
        let _backup_guard = self
            .backup_lock
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        Ok(self
            .read_authenticated_backup_terminal_receipt(job_id)?
            .map(|receipt| receipt.result.manifest.backup_id))
    }

    /// Loads, authenticates, and decrypts a committed snapshot.
    pub fn load_manifest(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<Manifest, CoreError> {
        let snapshot = self.store.load_snapshot(backup_id, snapshot_id)?;
        Ok(self
            .authenticate_snapshot(backup_id, snapshot_id, snapshot)?
            .manifest)
    }

    /// Authenticates every local chunk in one committed snapshot.
    pub fn verify_snapshot(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<IntegrityReport, CoreError> {
        let manifest = self.load_manifest(backup_id, snapshot_id)?;
        let key = self.load_backup_key(backup_id)?;
        self.store.verify_manifest(&manifest, &key)
    }

    /// Verifies local objects plus every acknowledged copy on connected selected providers.
    pub fn verify_snapshot_availability(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<SnapshotAvailabilityReport, CoreError> {
        let manifest = self.load_manifest(backup_id, snapshot_id)?;
        let key = self.load_backup_key(backup_id)?;
        let local = self.store.verify_manifest(&manifest, &key)?;
        let scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        let revoked: BTreeSet<_> = self
            .config
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .trusted_peers
            .values()
            .filter(|grant| grant.revoked)
            .map(|grant| grant.peer_device_id)
            .collect();
        let references: BTreeMap<_, _> = manifest
            .entries
            .iter()
            .flat_map(|entry| &entry.chunks)
            .map(|reference| (reference.opaque_locator.clone(), reference))
            .collect();
        let mut report = SnapshotAvailabilityReport {
            local,
            ..SnapshotAvailabilityReport::default()
        };
        let mut provider_copies = Vec::new();
        for provider_id in &manifest.replica_intent.selected_providers {
            if revoked.contains(provider_id) {
                report
                    .providers
                    .insert(*provider_id, ReplicaAvailability::Revoked);
                continue;
            }
            match scheduler.provider_health(*provider_id) {
                Some(crate::ProviderHealth::Online) => {}
                Some(crate::ProviderHealth::Corrupt) => {
                    report
                        .providers
                        .insert(*provider_id, ReplicaAvailability::Corrupt);
                    continue;
                }
                Some(crate::ProviderHealth::Offline) | None => {
                    report
                        .providers
                        .insert(*provider_id, ReplicaAvailability::Offline);
                    continue;
                }
            }
            let acknowledged = manifest
                .provider_acknowledgements
                .get(provider_id)
                .cloned()
                .unwrap_or_default();
            let availability = if acknowledged.len() == references.len() {
                ReplicaAvailability::Complete
            } else {
                ReplicaAvailability::Degraded
            };
            for locator in acknowledged {
                let reference = references
                    .get(&locator)
                    .ok_or(CoreError::RestorePlanMismatch)?;
                provider_copies.push((*provider_id, *reference));
            }
            report.providers.insert(*provider_id, availability);
        }
        for failure in
            scheduler.verify_provider_copies_parallel(&provider_copies, backup_id, &key)?
        {
            if let Some(availability) = report.providers.get_mut(&failure.provider_id) {
                match failure.reason.as_str() {
                    "corrupt_chunk" => *availability = ReplicaAvailability::Corrupt,
                    "missing_chunk" if *availability != ReplicaAvailability::Corrupt => {
                        *availability = ReplicaAvailability::Degraded;
                    }
                    _ if *availability != ReplicaAvailability::Corrupt => {
                        *availability = ReplicaAvailability::Offline;
                    }
                    _ => {}
                }
            }
            report.failures.push(failure);
        }
        report.failures.sort_by(|left, right| {
            (left.provider_id, &left.locator).cmp(&(right.provider_id, &right.locator))
        });
        Ok(report)
    }

    /// Builds a signed no-write restore preview.
    pub fn preview_restore(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
        authorized_root: impl AsRef<Path>,
        options: &RestoreOptions,
    ) -> Result<RestorePlan, CoreError> {
        let manifest = self.load_manifest(backup_id, snapshot_id)?;
        let root = AuthorizedRoot::open(authorized_root)?;
        preview_restore(&manifest, &root, options, &self.identity)
    }

    /// Executes only the exact signed preview under the same authorized root.
    pub fn restore(
        &self,
        plan: &RestorePlan,
        control: &JobControl,
    ) -> Result<RestoreReport, CoreError> {
        let manifest = self.load_manifest(plan.backup_id, &plan.snapshot_id)?;
        let key = self.load_backup_key(plan.backup_id)?;
        let root = AuthorizedRoot::open(&plan.authorized_root)?;
        let scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        execute_restore(
            &manifest,
            plan,
            &root,
            &key,
            &scheduler,
            &self.store,
            &self.identity.public_identity(),
            self.device_id(),
            control,
        )
    }

    /// Durably discards a cancelled backup or restore checkpoint.
    pub fn discard_job_checkpoint(&self, job_id: &str) -> Result<(), CoreError> {
        self.store.remove_checkpoint(job_id)
    }

    /// Repairs missing or corrupt local objects only from an authenticated authorized copy.
    pub fn repair_snapshot(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
    ) -> Result<IntegrityReport, CoreError> {
        let snapshot = self.store.load_snapshot(backup_id, snapshot_id)?;
        let authenticated = self.authenticate_snapshot(backup_id, snapshot_id, snapshot)?;
        let snapshot = authenticated.snapshot;
        let manifest = authenticated.manifest;
        let key = authenticated.key;
        let initial = self.store.verify_manifest(&manifest, &key)?;
        let targets: BTreeSet<_> = initial
            .missing
            .iter()
            .chain(&initial.corrupt)
            .cloned()
            .collect();
        if targets.is_empty() {
            return Ok(initial);
        }
        let scheduler = self
            .scheduler
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        for reference in manifest
            .entries
            .iter()
            .flat_map(|entry| &entry.chunks)
            .filter(|reference| targets.contains(&reference.opaque_locator))
        {
            let mut providers = BTreeSet::from([self.device_id()]);
            providers.extend(
                manifest
                    .provider_acknowledgements
                    .iter()
                    .filter(|(_, locators)| locators.contains(&reference.opaque_locator))
                    .map(|(provider_id, _)| *provider_id),
            );
            let fetched = scheduler.fetch_plaintext(
                reference,
                backup_id,
                &key,
                &providers,
                &JobControl::new(),
            )?;
            let repaired =
                key.encrypt_chunk(backup_id, snapshot.envelope.key_epoch, &fetched.plaintext)?;
            if repaired.opaque_locator != reference.opaque_locator {
                return Err(CoreError::CorruptChunk(reference.opaque_locator.clone()));
            }
            self.store.repair_record(
                &manifest,
                &key,
                &reference.opaque_locator,
                &repaired.encode_provider_record(),
            )?;
        }
        self.store.verify_manifest(&manifest, &key)
    }

    /// Authenticates every retention root before deleting unreferenced local objects.
    pub fn garbage_collect(&self) -> Result<crate::GarbageCollectionReport, CoreError> {
        let generation = self.store.snapshot_generation();
        let mut index = self.store.begin_retention_index(generation)?;
        for (backup_id, snapshot_id) in self.store.list_snapshot_ids()? {
            let snapshot = self.store.load_snapshot(backup_id, &snapshot_id)?;
            let authenticated = self.authenticate_snapshot(backup_id, &snapshot_id, snapshot)?;
            index.add_locators(authenticated.snapshot.chunk_locators.iter())?;
        }
        let index = index.finish()?;
        self.store.garbage_collect_authenticated(&index)
    }

    fn remember_backup_state(
        &self,
        backup_id: BackupId,
        state: RememberedBackupState,
    ) -> Result<(), CoreError> {
        let mut config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let mut candidate = config.clone();
        candidate.remembered_backups.insert(backup_id, state);
        candidate.validate()?;
        write_json_atomic(&self.config_path, &candidate, true)?;
        *config = candidate;
        Ok(())
    }

    fn backup_terminal_receipt_path(&self, job_id: &str) -> Result<PathBuf, CoreError> {
        if !valid_identifier(job_id) {
            return Err(CoreError::InvalidState(
                "invalid backup terminal receipt id".to_owned(),
            ));
        }
        Ok(self
            .options
            .data_directory
            .join("backup-results")
            .join(format!("{job_id}.json")))
    }

    fn persist_backup_terminal_receipt(
        &self,
        options: &BackupOptions,
        request_digest: String,
        source_root_digest: String,
        result: &BackupResult,
    ) -> Result<(), CoreError> {
        let path = self.backup_terminal_receipt_path(&options.job_id)?;
        let directory = path.parent().ok_or_else(|| {
            CoreError::InvalidState("backup terminal receipt has no parent".to_owned())
        })?;
        ensure_private_state_directory(directory)?;
        self.ensure_backup_terminal_receipt_capacity(&path)?;
        let mut receipt = BackupTerminalReceipt {
            schema_version: BACKUP_TERMINAL_RECEIPT_SCHEMA_VERSION,
            job_id: options.job_id.clone(),
            options_digest: request_digest,
            source_root_digest,
            result: result.clone(),
            signature: String::new(),
        };
        receipt.signature = self.identity.sign(
            BACKUP_TERMINAL_RECEIPT_SIGNATURE_DOMAIN,
            &backup_terminal_receipt_signing_bytes(&receipt)?,
        );
        write_json_atomic(&path, &receipt, true)
    }

    fn ensure_backup_terminal_receipt_capacity(&self, retained: &Path) -> Result<(), CoreError> {
        let directory = retained.parent().ok_or_else(|| {
            CoreError::InvalidState("backup terminal receipt has no parent".to_owned())
        })?;
        let mut entries = fs::read_dir(directory)
            .map_err(|source| CoreError::Io {
                operation: "read backup terminal receipts",
                path: directory.to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| CoreError::Io {
                operation: "read backup terminal receipt entry",
                path: directory.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        let mut paths = Vec::new();
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
                operation: "inspect backup terminal receipt",
                path: path.clone(),
                source,
            })?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("json")
                || !path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .is_some_and(valid_identifier)
            {
                return Err(CoreError::InvalidState(
                    "unexpected backup terminal receipt entry".to_owned(),
                ));
            }
            paths.push(path);
        }
        if paths.len() >= MAX_UNACKNOWLEDGED_BACKUP_RESULTS && !retained.exists() {
            return Err(CoreError::ResourceLimit(
                "unacknowledged backup terminal results",
            ));
        }
        Ok(())
    }

    fn load_or_complete_backup_terminal_receipt(
        &self,
        options: &BackupOptions,
        request_digest: &str,
        source_root_digest: &str,
    ) -> Result<Option<BackupResult>, CoreError> {
        let Some(receipt) = self.read_authenticated_backup_terminal_receipt(&options.job_id)?
        else {
            return Ok(None);
        };
        if receipt.options_digest != request_digest
            || receipt.source_root_digest != source_root_digest
        {
            return Err(CoreError::JobConflict);
        }
        if receipt.result.manifest.backup_id != options.backup_id
            || receipt.result.manifest.snapshot_id != options.snapshot_id
            || receipt.result.stored_snapshot.backup_id != options.backup_id
            || receipt.result.stored_snapshot.snapshot_id != options.snapshot_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        self.store
            .commit_snapshot(&receipt.result.stored_snapshot)?;

        let expected = remembered_backup_state(options, self.device_id())?;
        let should_advance = {
            let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
            match config.remembered_backups.get(&options.backup_id) {
                None => true,
                Some(previous) if previous.descriptor.owner_device_id != self.device_id() => {
                    return Err(CoreError::AuthenticationFailed);
                }
                Some(previous) => match previous.latest_snapshot_id.as_deref() {
                    None => true,
                    Some(snapshot) if snapshot < options.snapshot_id.as_str() => {
                        if previous.key_epoch > expected.key_epoch {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        true
                    }
                    Some(snapshot) if snapshot == options.snapshot_id => {
                        if previous != &expected {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        false
                    }
                    Some(_) => {
                        if previous.key_epoch < expected.key_epoch {
                            return Err(CoreError::AuthenticationFailed);
                        }
                        false
                    }
                },
            }
        };
        if should_advance {
            self.remember_backup_state(options.backup_id, expected)?;
        }
        let stored = self
            .store
            .load_snapshot(options.backup_id, &options.snapshot_id)?;
        if stored != receipt.result.stored_snapshot
            || self
                .authenticate_snapshot(options.backup_id, &options.snapshot_id, stored)?
                .manifest
                != receipt.result.manifest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(receipt.result))
    }

    fn read_authenticated_backup_terminal_receipt(
        &self,
        job_id: &str,
    ) -> Result<Option<BackupTerminalReceipt>, CoreError> {
        let path = self.backup_terminal_receipt_path(job_id)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect backup terminal receipt",
                    path,
                    source,
                });
            }
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > MAX_BACKUP_TRANSACTION_BYTES as u64 =>
            {
                return Err(CoreError::AuthenticationFailed);
            }
            Ok(_) => {}
        }
        let receipt: BackupTerminalReceipt =
            read_json_bounded(&path, MAX_BACKUP_TRANSACTION_BYTES)?;
        if receipt.schema_version != BACKUP_TERMINAL_RECEIPT_SCHEMA_VERSION {
            return Err(CoreError::AuthenticationFailed);
        }
        self.identity.public_identity().verify(
            BACKUP_TERMINAL_RECEIPT_SIGNATURE_DOMAIN,
            &backup_terminal_receipt_signing_bytes(&receipt)?,
            &receipt.signature,
        )?;
        if receipt.job_id != job_id
            || !valid_lower_hex_digest(&receipt.options_digest)
            || !valid_lower_hex_digest(&receipt.source_root_digest)
            || receipt.result.manifest.backup_id != receipt.result.stored_snapshot.backup_id
            || receipt.result.manifest.snapshot_id != receipt.result.stored_snapshot.snapshot_id
            || receipt.result.manifest.provider_acknowledgements
                != receipt.result.replication.acknowledgements
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(Some(receipt))
    }

    fn repair_backup_completion(&self, job_id: &str) -> Result<(), CoreError> {
        let transaction_path = self
            .options
            .data_directory
            .join("transactions")
            .join(format!("{job_id}.json"));
        backup_completion_failpoint(1)?;
        match fs::remove_file(&transaction_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "complete backup transaction",
                    path: transaction_path.clone(),
                    source,
                });
            }
        }
        backup_completion_failpoint(2)?;
        sync_directory(
            transaction_path
                .parent()
                .ok_or_else(|| CoreError::InvalidState("transaction has no parent".to_owned()))?,
        )?;
        backup_completion_failpoint(3)?;
        self.store.remove_checkpoint(job_id)
    }

    fn validate_backup_transition(&self, options: &BackupOptions) -> Result<(), CoreError> {
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        if let Some(previous) = config.remembered_backups.get(&options.backup_id) {
            if previous.descriptor.owner_device_id != self.device_id()
                || options.key_epoch < previous.key_epoch
                || previous
                    .latest_snapshot_id
                    .as_ref()
                    .is_some_and(|snapshot| options.snapshot_id <= *snapshot)
            {
                return Err(CoreError::InvalidState(
                    "backup snapshot or key epoch is not monotonic".to_owned(),
                ));
            }
        } else if options.key_epoch != 1 {
            return Err(CoreError::InvalidState(
                "a new backup must begin at key epoch 1".to_owned(),
            ));
        }
        Ok(())
    }

    fn load_or_create_backup_key(&self, backup_id: BackupId) -> Result<BackupKey, CoreError> {
        if let Ok(keys) = self.keys.lock() {
            if let Some(key) = keys.get(&backup_id) {
                return Ok(key.clone());
            }
        } else {
            return Err(CoreError::Synchronization);
        }
        let path = self.key_path(backup_id);
        let key = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "backup key path is not a regular file".to_owned(),
                    ));
                }
                load_backup_key_file(
                    &path,
                    &self.options.data_directory,
                    backup_id,
                    self.key_protector.as_ref(),
                )?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = BackupKey::generate();
                persist_backup_key_file(
                    &path,
                    &self.options.data_directory,
                    backup_id,
                    &key,
                    self.key_protector.as_ref(),
                )?;
                key
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect backup key",
                    path,
                    source,
                });
            }
        };
        self.keys
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .insert(backup_id, key.clone());
        Ok(key)
    }

    fn load_backup_key(&self, backup_id: BackupId) -> Result<BackupKey, CoreError> {
        if let Some(key) = self
            .keys
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .get(&backup_id)
            .cloned()
        {
            return Ok(key);
        }
        let key = load_backup_key_file(
            &self.key_path(backup_id),
            &self.options.data_directory,
            backup_id,
            self.key_protector.as_ref(),
        )?;
        self.keys
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .insert(backup_id, key.clone());
        Ok(key)
    }

    fn key_path(&self, backup_id: BackupId) -> PathBuf {
        self.key_directory.join(format!("{backup_id}.json"))
    }

    fn signer_identity(&self, signer_id: DeviceId) -> Result<PublicIdentity, CoreError> {
        if signer_id == self.device_id() {
            return Ok(self.identity.public_identity());
        }
        let config = self.config.lock().map_err(|_| CoreError::Synchronization)?;
        let grant = config
            .trusted_peers
            .get(&signer_id)
            .ok_or(CoreError::IdentityMismatch)?;
        if grant.revoked {
            return Err(CoreError::PeerRevoked);
        }
        PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
    }

    fn authenticate_snapshot(
        &self,
        requested_backup_id: BackupId,
        requested_snapshot_id: &str,
        snapshot: StoredSnapshot,
    ) -> Result<AuthenticatedSnapshot, CoreError> {
        if snapshot.backup_id != requested_backup_id
            || snapshot.snapshot_id != requested_snapshot_id
            || snapshot.envelope.backup_id != requested_backup_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let key = self.load_backup_key(requested_backup_id)?;
        let signer = self.signer_identity(snapshot.envelope.signer_device_id)?;
        let manifest = decrypt_manifest(&snapshot.envelope, &key, &signer)?;
        if manifest.backup_id != requested_backup_id
            || manifest.snapshot_id != requested_snapshot_id
            || manifest.created_at_unix_ms != snapshot.committed_at_unix_ms
            || !manifest_locators_match(&manifest, &snapshot.chunk_locators)
        {
            return Err(CoreError::AuthenticationFailed);
        }
        for reference in manifest.entries.iter().flat_map(|entry| &entry.chunks) {
            let expected = key.expected_chunk_locator(
                requested_backup_id,
                snapshot.envelope.key_epoch,
                &reference.plaintext_digest,
            )?;
            if expected != reference.opaque_locator {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        if self
            .config
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .remembered_backups
            .get(&requested_backup_id)
            .is_some_and(|remembered| snapshot.envelope.key_epoch > remembered.key_epoch)
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(AuthenticatedSnapshot {
            snapshot,
            manifest,
            key,
        })
    }

    fn issue_roster_locked(
        &self,
        previous: &NodeConfig,
        candidate: &mut NodeConfig,
    ) -> Result<SignedRoster, CoreError> {
        let next_epoch = previous
            .roster_epoch
            .checked_add(1)
            .ok_or(CoreError::ResourceLimit("roster epoch"))?;
        let mut builder = SignedRosterBuilder::new(next_epoch, previous.roster_digest.clone());
        for grant in candidate.trusted_peers.values() {
            builder = builder.grant(grant.clone());
        }
        let roster = builder.sign(&self.identity)?;
        candidate.roster_epoch = next_epoch;
        candidate.roster_digest = roster_digest(&roster)?;
        candidate.validate()?;
        commit_roster_transaction(
            &self.options.data_directory,
            &self.config_path,
            previous,
            PendingRosterCommit::Local {
                schema_version: ROSTER_TRANSACTION_SCHEMA_VERSION,
                roster: roster.clone(),
                config: candidate.clone(),
            },
            &self.identity,
        )?;
        Ok(roster)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingBackupCommit {
    schema_version: u16,
    snapshot: StoredSnapshot,
    remembered: RememberedBackupState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupTerminalReceipt {
    schema_version: u16,
    job_id: String,
    options_digest: String,
    source_root_digest: String,
    result: BackupResult,
    signature: String,
}

fn backup_terminal_receipt_signing_bytes(
    receipt: &BackupTerminalReceipt,
) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Fields<'a> {
        schema_version: u16,
        job_id: &'a str,
        options_digest: &'a str,
        source_root_digest: &'a str,
        result: &'a BackupResult,
    }
    Ok(serde_json::to_vec(&Fields {
        schema_version: receipt.schema_version,
        job_id: &receipt.job_id,
        options_digest: &receipt.options_digest,
        source_root_digest: &receipt.source_root_digest,
        result: &receipt.result,
    })?)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum PendingRosterCommit {
    Local {
        schema_version: u16,
        roster: SignedRoster,
        config: NodeConfig,
    },
    Peer {
        schema_version: u16,
        roster: SignedRoster,
        config: NodeConfig,
    },
}

struct AuthenticatedSnapshot {
    snapshot: StoredSnapshot,
    manifest: Manifest,
    key: BackupKey,
}

fn roster_transaction_path(data_directory: &Path) -> PathBuf {
    data_directory.join("roster-transaction.json")
}

fn commit_roster_transaction(
    data_directory: &Path,
    config_path: &Path,
    current: &NodeConfig,
    transaction: PendingRosterCommit,
    identity: &DeviceIdentity,
) -> Result<(), CoreError> {
    let transaction_path = roster_transaction_path(data_directory);
    if fs::symlink_metadata(&transaction_path).is_ok() {
        return Err(CoreError::InvalidState(
            "pending roster transaction requires recovery".to_owned(),
        ));
    }
    write_json_atomic(&transaction_path, &transaction, true)?;
    apply_roster_transaction(data_directory, config_path, current, &transaction, identity)?;
    fs::remove_file(&transaction_path).map_err(|source| CoreError::Io {
        operation: "complete roster transaction",
        path: transaction_path,
        source,
    })?;
    sync_directory(data_directory)
}

fn recover_roster_transaction(
    data_directory: &Path,
    config_path: &Path,
    config: &mut NodeConfig,
    identity: &DeviceIdentity,
) -> Result<(), CoreError> {
    let transaction_path = roster_transaction_path(data_directory);
    let metadata = match fs::symlink_metadata(&transaction_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(CoreError::Io {
                operation: "inspect roster transaction",
                path: transaction_path,
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CoreError::InvalidState(
            "roster transaction is not a regular file".to_owned(),
        ));
    }
    let transaction: PendingRosterCommit =
        read_json_bounded(&transaction_path, MAX_ROSTER_TRANSACTION_BYTES)?;
    apply_roster_transaction(data_directory, config_path, config, &transaction, identity)?;
    *config = transaction_config(&transaction).clone();
    fs::remove_file(&transaction_path).map_err(|source| CoreError::Io {
        operation: "complete recovered roster transaction",
        path: transaction_path,
        source,
    })?;
    sync_directory(data_directory)
}

fn apply_roster_transaction(
    data_directory: &Path,
    config_path: &Path,
    current: &NodeConfig,
    transaction: &PendingRosterCommit,
    identity: &DeviceIdentity,
) -> Result<(), CoreError> {
    let candidate = transaction_config(transaction);
    candidate.validate()?;
    match transaction {
        PendingRosterCommit::Local {
            schema_version,
            roster,
            ..
        } => {
            if *schema_version != ROSTER_TRANSACTION_SCHEMA_VERSION {
                return Err(CoreError::InvalidState(
                    "unsupported roster transaction schema".to_owned(),
                ));
            }
            validate_local_roster(roster, candidate, identity)?;
            validate_local_roster_transition(current, candidate, roster)?;
            write_json_atomic(&data_directory.join("roster.json"), roster, false)?;
        }
        PendingRosterCommit::Peer {
            schema_version,
            roster,
            ..
        } => {
            if *schema_version != ROSTER_TRANSACTION_SCHEMA_VERSION {
                return Err(CoreError::InvalidState(
                    "unsupported roster transaction schema".to_owned(),
                ));
            }
            validate_peer_roster_transition(current, candidate, roster)?;
            write_json_atomic(
                &data_directory
                    .join("peer-rosters")
                    .join(format!("{}.json", roster.signer_device_id)),
                roster,
                false,
            )?;
        }
    }
    write_json_atomic(config_path, candidate, true)
}

const fn transaction_config(transaction: &PendingRosterCommit) -> &NodeConfig {
    match transaction {
        PendingRosterCommit::Local { config, .. } | PendingRosterCommit::Peer { config, .. } => {
            config
        }
    }
}

fn validate_local_roster(
    roster: &SignedRoster,
    config: &NodeConfig,
    identity: &DeviceIdentity,
) -> Result<(), CoreError> {
    verify_roster(
        roster,
        &identity.public_identity(),
        roster.epoch.saturating_sub(1),
        &roster.previous_digest,
    )?;
    let expected_grants: Vec<_> = config.trusted_peers.values().cloned().collect();
    if roster.epoch != config.roster_epoch
        || roster.signer_device_id != identity.device_id()
        || roster_digest(roster)? != config.roster_digest
        || roster.grants != expected_grants
    {
        return Err(CoreError::InvalidState(
            "local roster does not match durable configuration".to_owned(),
        ));
    }
    Ok(())
}

fn validate_local_roster_transition(
    current: &NodeConfig,
    candidate: &NodeConfig,
    roster: &SignedRoster,
) -> Result<(), CoreError> {
    if !same_config_outside_local_roster(current, candidate) {
        return Err(CoreError::InvalidState(
            "roster transaction changes unrelated configuration".to_owned(),
        ));
    }
    if current == candidate {
        return Ok(());
    }
    if roster.epoch != current.roster_epoch.saturating_add(1)
        || roster.previous_digest != current.roster_digest
    {
        return Err(CoreError::InvalidState(
            "roster transaction is not the next local epoch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_peer_roster_transition(
    current: &NodeConfig,
    candidate: &NodeConfig,
    roster: &SignedRoster,
) -> Result<(), CoreError> {
    if !same_config_outside_peer_rosters(current, candidate) {
        return Err(CoreError::InvalidState(
            "peer roster transaction changes unrelated configuration".to_owned(),
        ));
    }
    let grant = current
        .trusted_peers
        .get(&roster.signer_device_id)
        .ok_or(CoreError::IdentityMismatch)?;
    if grant.revoked {
        return Err(CoreError::PeerRevoked);
    }
    let signer = PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())?;
    let previous = current
        .peer_roster_cursors
        .get(&roster.signer_device_id)
        .cloned()
        .unwrap_or_default();
    let expected = RosterCursor {
        epoch: roster.epoch,
        digest: roster_digest(roster)?,
    };
    if candidate.peer_roster_cursors.get(&roster.signer_device_id) != Some(&expected) {
        return Err(CoreError::InvalidState(
            "peer roster cursor does not match signed roster".to_owned(),
        ));
    }
    if current == candidate {
        verify_roster(
            roster,
            &signer,
            roster.epoch.saturating_sub(1),
            &roster.previous_digest,
        )?;
    } else {
        verify_roster(roster, &signer, previous.epoch, &previous.digest)?;
    }
    Ok(())
}

fn same_config_outside_local_roster(left: &NodeConfig, right: &NodeConfig) -> bool {
    left.schema_version == right.schema_version
        && left.device_name == right.device_name
        && left.lan_discovery_enabled == right.lan_discovery_enabled
        && left.remembered_backups == right.remembered_backups
        && left.peer_roster_cursors == right.peer_roster_cursors
}

fn same_config_outside_peer_rosters(left: &NodeConfig, right: &NodeConfig) -> bool {
    left.schema_version == right.schema_version
        && left.device_name == right.device_name
        && left.lan_discovery_enabled == right.lan_discovery_enabled
        && left.remembered_backups == right.remembered_backups
        && left.trusted_peers == right.trusted_peers
        && left.roster_epoch == right.roster_epoch
        && left.roster_digest == right.roster_digest
}

fn acquire_state_lock(data_directory: &Path) -> Result<File, CoreError> {
    let path = data_directory.join(".engine.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(CoreError::InvalidState(
                "engine lock path is not a regular file".to_owned(),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(CoreError::Io {
                operation: "inspect engine state lock",
                path,
                source,
            });
        }
    }
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags, open};

        let descriptor = open(
            &path,
            OFlags::CREATE | OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NOCTTY,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| CoreError::Io {
            operation: "open engine state lock without following links",
            path: path.clone(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        File::from(descriptor)
    };
    #[cfg(not(unix))]
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| CoreError::Io {
            operation: "open engine state lock",
            path: path.clone(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| CoreError::Io {
        operation: "inspect opened engine state lock",
        path: path.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(CoreError::InvalidState(
            "engine lock path is not a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect engine state lock",
                path: path.clone(),
                source,
            })?;
    }
    file.try_lock_exclusive().map_err(|source| {
        if source.kind() == std::io::ErrorKind::WouldBlock {
            CoreError::StateLocked
        } else {
            CoreError::Io {
                operation: "lock engine state",
                path,
                source,
            }
        }
    })?;
    Ok(file)
}

fn remembered_backup_state(
    options: &BackupOptions,
    owner_device_id: DeviceId,
) -> Result<RememberedBackupState, CoreError> {
    let descriptor = RememberedBackup {
        backup_id: options.backup_id,
        name: options.display_name.clone(),
        owner_device_id,
    };
    ExportedDeviceSettings::new("Recovery validation", false, vec![descriptor.clone()])?;
    Ok(RememberedBackupState {
        descriptor,
        key_epoch: options.key_epoch,
        latest_snapshot_id: Some(options.snapshot_id.clone()),
        replica_intent: options.replica_intent.clone(),
    })
}

fn recover_backup_transactions(
    data_directory: &Path,
    store: &ChunkStore,
    config_path: &Path,
    config: &mut NodeConfig,
    identity: &DeviceIdentity,
    protector: &dyn KeyProtector,
) -> Result<(), CoreError> {
    let directory = data_directory.join("transactions");
    ensure_private_state_directory(&directory)?;
    let mut entries = fs::read_dir(&directory)
        .map_err(|source| CoreError::Io {
            operation: "read backup transaction directory",
            path: directory.clone(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CoreError::Io {
            operation: "read backup transaction entry",
            path: directory.clone(),
            source,
        })?;
    if entries.len() > 64 {
        return Err(CoreError::ResourceLimit("pending backup transactions"));
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
            operation: "inspect backup transaction",
            path: path.clone(),
            source,
        })?;
        let stem = path.file_stem().and_then(|value| value.to_str());
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
            || !stem.is_some_and(valid_identifier)
        {
            return Err(CoreError::InvalidState(
                "unexpected backup transaction entry".to_owned(),
            ));
        }
        let transaction: PendingBackupCommit =
            read_json_bounded(&path, MAX_BACKUP_TRANSACTION_BYTES)?;
        validate_pending_backup_commit(&transaction, data_directory, identity, protector)?;

        let backup_id = transaction.snapshot.backup_id;
        let should_advance = match config.remembered_backups.get(&backup_id) {
            None => {
                if transaction.remembered.key_epoch != 1 {
                    return Err(CoreError::InvalidState(
                        "recovered backup does not begin at key epoch 1".to_owned(),
                    ));
                }
                true
            }
            Some(previous) => {
                if previous.descriptor.owner_device_id != identity.device_id()
                    || transaction.remembered.key_epoch < previous.key_epoch
                {
                    return Err(CoreError::InvalidState(
                        "recovered backup violates owner or key epoch".to_owned(),
                    ));
                }
                match previous.latest_snapshot_id.as_deref() {
                    None => true,
                    Some(previous_snapshot) => {
                        match transaction
                            .snapshot
                            .snapshot_id
                            .as_str()
                            .cmp(previous_snapshot)
                        {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Equal => {
                                if previous != &transaction.remembered {
                                    return Err(CoreError::InvalidState(
                                        "recovered backup conflicts with committed config"
                                            .to_owned(),
                                    ));
                                }
                                false
                            }
                            std::cmp::Ordering::Less => {
                                if transaction.remembered.key_epoch > previous.key_epoch {
                                    return Err(CoreError::InvalidState(
                                        "stale transaction has a future key epoch".to_owned(),
                                    ));
                                }
                                false
                            }
                        }
                    }
                }
            }
        };
        store.commit_snapshot(&transaction.snapshot)?;
        if should_advance {
            let mut candidate = config.clone();
            candidate
                .remembered_backups
                .insert(backup_id, transaction.remembered);
            candidate.validate()?;
            write_json_atomic(config_path, &candidate, true)?;
            *config = candidate;
        }
        fs::remove_file(&path).map_err(|source| CoreError::Io {
            operation: "complete recovered backup transaction",
            path: path.clone(),
            source,
        })?;
        sync_directory(&directory)?;
    }
    Ok(())
}

fn validate_pending_backup_commit(
    transaction: &PendingBackupCommit,
    data_directory: &Path,
    identity: &DeviceIdentity,
    protector: &dyn KeyProtector,
) -> Result<(), CoreError> {
    if transaction.schema_version != BACKUP_TRANSACTION_SCHEMA_VERSION
        || transaction.remembered.descriptor.backup_id != transaction.snapshot.backup_id
        || transaction.remembered.descriptor.owner_device_id != identity.device_id()
        || transaction.remembered.latest_snapshot_id.as_deref()
            != Some(transaction.snapshot.snapshot_id.as_str())
        || transaction.remembered.key_epoch != transaction.snapshot.envelope.key_epoch
        || transaction.snapshot.envelope.signer_device_id != identity.device_id()
    {
        return Err(CoreError::InvalidState(
            "inconsistent backup transaction".to_owned(),
        ));
    }
    ExportedDeviceSettings::new(
        "Recovery validation",
        false,
        vec![transaction.remembered.descriptor.clone()],
    )?;
    let key = load_backup_key_file(
        &data_directory
            .join("keys")
            .join(format!("{}.json", transaction.snapshot.backup_id)),
        data_directory,
        transaction.snapshot.backup_id,
        protector,
    )?;
    let manifest = decrypt_manifest(
        &transaction.snapshot.envelope,
        &key,
        &identity.public_identity(),
    )?;
    if manifest.backup_id != transaction.snapshot.backup_id
        || manifest.snapshot_id != transaction.snapshot.snapshot_id
        || manifest.created_at_unix_ms != transaction.snapshot.committed_at_unix_ms
        || manifest.replica_intent != transaction.remembered.replica_intent
        || !manifest_locators_match(&manifest, &transaction.snapshot.chunk_locators)
    {
        return Err(CoreError::InvalidState(
            "backup transaction does not match its authenticated manifest".to_owned(),
        ));
    }
    for reference in manifest.entries.iter().flat_map(|entry| &entry.chunks) {
        if key.expected_chunk_locator(
            manifest.backup_id,
            transaction.snapshot.envelope.key_epoch,
            &reference.plaintext_digest,
        )? != reference.opaque_locator
        {
            return Err(CoreError::InvalidState(
                "backup transaction locator uses a different key epoch".to_owned(),
            ));
        }
    }
    Ok(())
}

fn manifest_locators_match(manifest: &Manifest, expected: &BTreeSet<String>) -> bool {
    let mut locators: Vec<_> = manifest
        .entries
        .iter()
        .flat_map(|entry| &entry.chunks)
        .map(|reference| reference.opaque_locator.as_str())
        .collect();
    locators.sort_unstable();
    locators.dedup();
    locators.len() == expected.len()
        && locators
            .into_iter()
            .zip(expected.iter().map(String::as_str))
            .all(|(actual, expected)| actual == expected)
}

fn ensure_private_state_directory(path: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(path).map_err(|source| CoreError::Io {
        operation: "create private state directory",
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
        operation: "inspect private state directory",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CoreError::InvalidState(
            "private state path is not a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            CoreError::Io {
                operation: "protect private state directory",
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedBackupKey {
    schema_version: u16,
    protected_key: WrappedSecret,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyPersistedBackupKey {
    #[serde(default)]
    schema_version: u16,
    key: Zeroizing<String>,
}

fn backup_key_record_id(backup_id: BackupId) -> String {
    format!("keys/{backup_id}.json")
}

fn migrate_backup_key_directory(
    key_directory: &Path,
    state_root: &Path,
    protector: &dyn KeyProtector,
) -> Result<(), CoreError> {
    let mut entries = fs::read_dir(key_directory)
        .map_err(|source| CoreError::Io {
            operation: "read backup key directory",
            path: key_directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CoreError::Io {
            operation: "read backup key entry",
            path: key_directory.to_path_buf(),
            source,
        })?;
    if entries.len() > 4_096 {
        return Err(CoreError::ResourceLimit("persisted backup keys"));
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
            operation: "inspect backup key entry",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || path.extension().and_then(|value| value.to_str()) != Some("json")
        {
            return Err(CoreError::InvalidState(
                "unexpected backup key entry".to_owned(),
            ));
        }
        let backup_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .and_then(|value| value.parse::<BackupId>().ok())
            .ok_or_else(|| CoreError::InvalidState("invalid backup key filename".to_owned()))?;
        drop(load_backup_key_file(
            &path, state_root, backup_id, protector,
        )?);
    }
    Ok(())
}

fn persist_backup_key_file(
    path: &Path,
    state_root: &Path,
    backup_id: BackupId,
    key: &BackupKey,
    protector: &dyn KeyProtector,
) -> Result<(), CoreError> {
    let context = state_secret_context(state_root, &backup_key_record_id(backup_id));
    let key_bytes = key.to_bytes();
    let encoded = PersistedBackupKey {
        schema_version: PROTECTED_BACKUP_KEY_SCHEMA_VERSION,
        protected_key: WrappedSecret::protect(
            protector,
            BACKUP_KEY_SECRET_PURPOSE,
            &context,
            Zeroizing::new(key_bytes.as_ref().to_vec()),
        )?,
    };
    let bytes = Zeroizing::new(serde_json::to_vec_pretty(&encoded)?);
    write_atomic(path, &bytes, true)
}

fn persist_or_validate_backup_key_file(
    path: &Path,
    state_root: &Path,
    backup_id: BackupId,
    key: &BackupKey,
    protector: &dyn KeyProtector,
) -> Result<(), CoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(CoreError::InvalidState(
                    "backup key path is not a regular file".to_owned(),
                ));
            }
            let incumbent = load_backup_key_file(path, state_root, backup_id, protector)?;
            if incumbent.to_bytes().as_ref() != key.to_bytes().as_ref() {
                return Err(CoreError::AuthenticationFailed);
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            persist_backup_key_file(path, state_root, backup_id, key, protector)
        }
        Err(source) => Err(CoreError::Io {
            operation: "inspect recovered backup key",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn load_backup_key_file(
    path: &Path,
    state_root: &Path,
    backup_id: BackupId,
    protector: &dyn KeyProtector,
) -> Result<BackupKey, CoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
        operation: "inspect backup key file",
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CoreError::InvalidState(
            "backup key path is not a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CoreError::InvalidState(
                "backup key permissions are too broad".to_owned(),
            ));
        }
    }
    let persisted_bytes = crate::atomic::read_bounded(path, MAX_BACKUP_KEY_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&persisted_bytes)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if schema_version == u64::from(PROTECTED_BACKUP_KEY_SCHEMA_VERSION) {
        let persisted: PersistedBackupKey = serde_json::from_value(value)?;
        let context = state_secret_context(state_root, &backup_key_record_id(backup_id));
        let plaintext =
            persisted
                .protected_key
                .open(protector, BACKUP_KEY_SECRET_PURPOSE, &context)?;
        let mut bytes: [u8; 32] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key = BackupKey::from_bytes(bytes);
        bytes.zeroize();
        return Ok(key);
    }
    if schema_version == 0 || schema_version == u64::from(LEGACY_BACKUP_KEY_SCHEMA_VERSION) {
        let legacy: LegacyPersistedBackupKey = serde_json::from_value(value)?;
        if legacy.schema_version != 0 && legacy.schema_version != LEGACY_BACKUP_KEY_SCHEMA_VERSION {
            return Err(CoreError::InvalidState(
                "unsupported backup key schema".to_owned(),
            ));
        }
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(legacy.key.as_bytes())
                .map_err(|_| CoreError::InvalidKeyMaterial)?,
        );
        let mut bytes: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key = BackupKey::from_bytes(bytes);
        bytes.zeroize();
        persist_backup_key_file(path, state_root, backup_id, &key, protector)?;
        return Ok(key);
    }
    Err(CoreError::InvalidState(
        "unsupported backup key schema".to_owned(),
    ))
}

fn load_or_create_config(path: &Path, options: &EngineOptions) -> Result<NodeConfig, CoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(CoreError::InvalidState(
                    "config path is not a regular file".to_owned(),
                ));
            }
            let bytes = crate::atomic::read_bounded(path, MAX_NODE_CONFIG_BYTES)?;
            let config = decode_or_migrate_config(&bytes)?;
            config.validate()?;
            write_json_atomic(path, &config, true)?;
            Ok(config)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let config = NodeConfig::new(
                options.initial_device_name.clone(),
                options.initial_lan_discovery_enabled,
            )?;
            write_json_atomic(path, &config, true)?;
            Ok(config)
        }
        Err(source) => Err(CoreError::Io {
            operation: "inspect node config",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn decode_or_migrate_config(bytes: &[u8]) -> Result<NodeConfig, CoreError> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if schema_version == u64::from(NODE_CONFIG_SCHEMA_VERSION) {
        return Ok(serde_json::from_value(value)?);
    }
    if schema_version == 0 {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct LegacyConfig {
            #[serde(default)]
            schema_version: u16,
            name: String,
            #[serde(default)]
            lan_discovery: bool,
        }
        let legacy: LegacyConfig = serde_json::from_value(value)?;
        if legacy.schema_version != 0 {
            return Err(CoreError::InvalidState(
                "unsupported legacy config".to_owned(),
            ));
        }
        return NodeConfig::new(legacy.name, legacy.lan_discovery);
    }
    Err(CoreError::InvalidState(
        "unsupported node config schema".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use covalent_protocol::PeerRole;
    use tempfile::tempdir;

    use crate::replication::StoreProvider;

    use super::*;

    fn test_protector() -> Arc<dyn KeyProtector> {
        Arc::new(crate::StaticKeyProtector::new(1, [0x51; 32]).expect("test protector"))
    }

    fn test_options(path: impl Into<PathBuf>) -> EngineOptions {
        EngineOptions::new(path).with_key_protector(test_protector())
    }

    #[test]
    fn legacy_config_migrates_without_identity_material() {
        let migrated = decode_or_migrate_config(br#"{"name":"Old node","lanDiscovery":true}"#)
            .expect("migration");
        assert_eq!(migrated.schema_version, 1);
        assert_eq!(migrated.device_name, "Old node");
        assert!(migrated.lan_discovery_enabled);
    }

    #[test]
    fn legacy_backup_key_migrates_without_changing_key_material() {
        let directory = tempdir().expect("directory");
        let path = directory.path().join("legacy-key.json");
        let backup_id = BackupId::new();
        let protector = test_protector();
        let expected = BackupKey::generate();
        write_json_atomic(
            &path,
            &serde_json::json!({
                "key": URL_SAFE_NO_PAD.encode(expected.to_bytes().as_ref())
            }),
            true,
        )
        .expect("legacy key");

        let loaded = load_backup_key_file(&path, directory.path(), backup_id, protector.as_ref())
            .expect("migrate key");
        assert_eq!(loaded.to_bytes().as_ref(), expected.to_bytes().as_ref());
        let migrated: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("persisted key")).expect("json");
        assert_eq!(
            migrated["schemaVersion"],
            PROTECTED_BACKUP_KEY_SCHEMA_VERSION
        );
        assert!(migrated.get("key").is_none());
    }

    #[test]
    fn recovery_bootstrap_resumes_after_every_durable_boundary() {
        for boundary in 1_u8..=4 {
            let root = tempdir().expect("root");
            let original_path = root.path().join("lost-owner");
            let recovered_path = root.path().join("recovered-owner");
            let original = Engine::open(test_options(&original_path)).expect("original engine");
            let original_device_id = original.device_id();
            let unlock = RecoveryUnlockKey::generate();
            let kit = original.export_recovery_kit(&unlock).expect("recovery kit");
            drop(original);
            fs::remove_dir_all(&original_path).expect("remove lost state");

            RECOVERY_BOOTSTRAP_FAILPOINT.with(|failpoint| failpoint.set(boundary));
            assert!(
                Engine::recover_from_kit(test_options(&recovered_path), &kit, &unlock).is_err(),
                "boundary {boundary} must simulate a crash"
            );
            let recovered = Engine::recover_from_kit(test_options(&recovered_path), &kit, &unlock)
                .expect("restart resumes recovery");
            assert_eq!(recovered.device_id(), original_device_id);
            assert!(
                !recovered_path
                    .join(RECOVERY_BOOTSTRAP_JOURNAL_FILE)
                    .exists()
            );
            assert!(
                fs::read_dir(root.path())
                    .expect("root entries")
                    .all(|entry| !entry
                        .expect("root entry")
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".staging"))
            );

            drop(recovered);
            let reopened = Engine::recover_from_kit(test_options(&recovered_path), &kit, &unlock)
                .expect("completed recovery is idempotent");
            assert_eq!(reopened.device_id(), original_device_id);
        }
    }

    #[test]
    fn settings_import_requires_confirmation_and_preserves_trust() {
        let directory = tempdir().expect("directory");
        let engine = Engine::open(test_options(directory.path())).expect("engine");
        let peer = DeviceIdentity::generate().public_identity();
        engine
            .trust_peer(PeerGrant {
                peer_device_id: peer.device_id,
                public_key: peer.public_key,
                display_name: "Provider".to_owned(),
                roles: BTreeSet::from([PeerRole::StorageProvider]),
                confirmed_at_unix_ms: 1,
                revoked: false,
            })
            .expect("trust");
        let settings = ExportedDeviceSettings::new("Imported", true, Vec::new()).expect("settings");
        let bytes = export_settings(&settings).expect("export");
        assert!(matches!(
            engine.import_settings(&bytes, false),
            Err(CoreError::SettingsImportNotConfirmed)
        ));
        engine.import_settings(&bytes, true).expect("import");
        assert_eq!(engine.config().expect("config").trusted_peers.len(), 1);
    }

    #[test]
    fn end_to_end_backup_verify_and_restore() {
        let node_data = tempdir().expect("node data");
        let provider_data = tempdir().expect("provider data");
        let source = tempdir().expect("source");
        let restore = tempdir().expect("restore");
        fs::create_dir(source.path().join("empty")).expect("empty directory");
        fs::write(source.path().join("hello.txt"), b"hello engine").expect("source file");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let provider_identity = DeviceIdentity::generate().public_identity();
        engine
            .trust_peer(PeerGrant {
                peer_device_id: provider_identity.device_id,
                public_key: provider_identity.public_key,
                display_name: "Provider".to_owned(),
                roles: BTreeSet::from([PeerRole::StorageProvider]),
                confirmed_at_unix_ms: 1,
                revoked: false,
            })
            .expect("trust");
        let provider_store = ChunkStore::open(provider_data.path(), 1_048_576).expect("provider");
        let local_provider = Arc::new(StoreProvider::new(engine.device_id(), engine.store.clone()));
        let remote_provider = Arc::new(StoreProvider::new(
            provider_identity.device_id,
            provider_store,
        ));
        engine
            .set_connected_providers(vec![
                local_provider as Arc<dyn ChunkProvider>,
                remote_provider as Arc<dyn ChunkProvider>,
            ])
            .expect("providers");
        let backup_id = BackupId::new();
        let mut options = BackupOptions::new(backup_id, "snapshot-1", "backup-job");
        options.created_at_unix_ms = 1;
        options.replica_intent = ReplicaIntent::explicit([provider_identity.device_id]);
        let result = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("backup");
        assert!(
            result
                .replication
                .is_complete(result.stored_snapshot.chunk_locators.len())
        );
        assert!(
            engine
                .verify_snapshot(backup_id, "snapshot-1")
                .expect("verify")
                .is_intact()
        );
        let plan = engine
            .preview_restore(
                backup_id,
                "snapshot-1",
                restore.path(),
                &RestoreOptions::all("restore-job"),
            )
            .expect("preview");
        engine.restore(&plan, &JobControl::new()).expect("restore");
        assert_eq!(
            fs::read(restore.path().join("hello.txt")).expect("restored"),
            b"hello engine"
        );
        assert!(restore.path().join("empty").is_dir());
    }

    #[test]
    fn startup_recovers_an_authenticated_backup_commit_transaction() {
        let node_data = tempdir().expect("node data");
        let source = tempdir().expect("source");
        fs::write(source.path().join("recover.txt"), b"recover me").expect("source file");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let backup_id = BackupId::new();
        let mut options = BackupOptions::new(backup_id, "0001", "recovery-job");
        options.display_name = "Recovery".to_owned();
        options.created_at_unix_ms = 42;
        let result = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("backup");
        let remembered = engine
            .config()
            .expect("config")
            .remembered_backups
            .get(&backup_id)
            .expect("remembered backup")
            .clone();
        drop(engine);

        let config_path = node_data.path().join("config.json");
        let mut config: NodeConfig =
            read_json_bounded(&config_path, MAX_NODE_CONFIG_BYTES).expect("persisted config");
        config.remembered_backups.remove(&backup_id);
        write_json_atomic(&config_path, &config, true).expect("rewind config");
        fs::remove_file(
            node_data
                .path()
                .join("store/snapshots")
                .join(backup_id.to_string())
                .join("0001.json"),
        )
        .expect("remove visible snapshot");
        let transaction_path = node_data.path().join("transactions/recovery-job.json");
        write_json_atomic(
            &transaction_path,
            &PendingBackupCommit {
                schema_version: BACKUP_TRANSACTION_SCHEMA_VERSION,
                snapshot: result.stored_snapshot,
                remembered,
            },
            true,
        )
        .expect("stage recovery transaction");

        let recovered = Engine::open(test_options(node_data.path())).expect("recover engine");
        assert!(
            recovered
                .verify_snapshot(backup_id, "0001")
                .expect("recovered snapshot")
                .is_intact()
        );
        assert_eq!(
            recovered
                .config()
                .expect("recovered config")
                .remembered_backups
                .get(&backup_id)
                .and_then(|state| state.latest_snapshot_id.as_deref()),
            Some("0001")
        );
        assert!(!transaction_path.exists());
    }

    #[test]
    fn terminal_backup_receipt_survives_every_post_commit_cleanup_failure() {
        for boundary in 1_u8..=3 {
            let node_data = tempdir().expect("node data");
            let source = tempdir().expect("source");
            fs::write(
                source.path().join("terminal.txt"),
                format!("terminal boundary {boundary}"),
            )
            .expect("source file");
            let engine = Engine::open(test_options(node_data.path())).expect("engine");
            let backup_id = BackupId::new();
            let job_id = format!("terminal-boundary-{boundary}");
            let mut options = BackupOptions::new(backup_id, "0001", &job_id);
            options.display_name = "Terminal receipt".to_owned();
            options.created_at_unix_ms = u64::from(boundary);
            BACKUP_COMPLETION_FAILPOINT.with(|failpoint| failpoint.set(boundary));
            let first = engine
                .backup(source.path(), &options, &JobControl::new(), |_| {})
                .expect("terminal success despite cleanup failure");
            let receipt_path = node_data
                .path()
                .join("backup-results")
                .join(format!("{job_id}.json"));
            assert!(receipt_path.is_file());
            drop(engine);
            let source_path = source.path().to_path_buf();
            drop(source);

            let reopened = Engine::open(test_options(node_data.path())).expect("reopen");
            let retry = reopened
                .backup(&source_path, &options, &JobControl::new(), |_| {})
                .expect("receipt retry without source");
            assert_eq!(retry, first);
            let different_source = tempdir().expect("different source");
            fs::write(
                different_source.path().join("terminal.txt"),
                b"different request",
            )
            .expect("different source file");
            assert!(matches!(
                reopened.backup(
                    different_source.path(),
                    &options,
                    &JobControl::new(),
                    |_| {}
                ),
                Err(CoreError::JobConflict)
            ));
            let mut conflicting_options = options.clone();
            conflicting_options.snapshot_id = "0002".to_owned();
            assert!(matches!(
                reopened.backup(
                    &source_path,
                    &conflicting_options,
                    &JobControl::new(),
                    |_| {}
                ),
                Err(CoreError::JobConflict)
            ));
            assert_eq!(
                reopened
                    .store()
                    .list_snapshot_ids()
                    .expect("snapshots")
                    .into_iter()
                    .filter(|(id, snapshot)| *id == backup_id && snapshot == "0001")
                    .count(),
                1
            );
            assert!(receipt_path.is_file());
            assert!(
                !node_data
                    .path()
                    .join("transactions")
                    .join(format!("{job_id}.json"))
                    .exists()
            );
            assert!(
                !reopened
                    .store()
                    .has_checkpoint(&job_id)
                    .expect("checkpoint")
            );
        }
    }

    #[test]
    fn unacknowledged_terminal_backup_receipts_backpressure_without_losing_retry_truth() {
        let node_data = tempdir().expect("node data");
        let source = tempdir().expect("source");
        fs::write(source.path().join("bounded.txt"), b"bounded receipts").expect("source");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let mut first = None;
        let mut first_options = None;
        for index in 0..MAX_UNACKNOWLEDGED_BACKUP_RESULTS {
            let mut options = BackupOptions::new(
                BackupId::new(),
                "0001",
                format!("bounded-receipt-{index:02}"),
            );
            options.created_at_unix_ms = index as u64;
            let result = engine
                .backup(source.path(), &options, &JobControl::new(), |_| {})
                .expect("backup");
            if index == 0 {
                first = Some(result);
                first_options = Some(options);
            }
        }
        let mut blocked = BackupOptions::new(BackupId::new(), "0001", "bounded-receipt-blocked");
        blocked.created_at_unix_ms = MAX_UNACKNOWLEDGED_BACKUP_RESULTS as u64;
        assert!(matches!(
            engine.backup(source.path(), &blocked, &JobControl::new(), |_| {}),
            Err(CoreError::ResourceLimit(
                "unacknowledged backup terminal results"
            ))
        ));
        let receipts = fs::read_dir(node_data.path().join("backup-results"))
            .expect("receipts")
            .collect::<Result<Vec<_>, _>>()
            .expect("receipt entries");
        assert_eq!(receipts.len(), MAX_UNACKNOWLEDGED_BACKUP_RESULTS);
        drop(engine);
        let reopened = Engine::open(test_options(node_data.path())).expect("reopen");
        let first_options = first_options.expect("first options");
        assert_eq!(
            reopened
                .backup(source.path(), &first_options, &JobControl::new(), |_| {})
                .expect("oldest unacknowledged retry"),
            first.expect("first result")
        );
        reopened
            .acknowledge_backup_result(&first_options.job_id)
            .expect("acknowledge oldest result");
        reopened
            .acknowledge_backup_result(&first_options.job_id)
            .expect("idempotent acknowledgement");
        reopened
            .backup(source.path(), &blocked, &JobControl::new(), |_| {})
            .expect("capacity after acknowledgement");
        assert!(
            node_data
                .path()
                .join("backup-results/bounded-receipt-blocked.json")
                .is_file()
        );
    }

    #[test]
    fn requested_snapshot_id_is_bound_to_authenticated_manifest() {
        let node_data = tempdir().expect("node data");
        let source = tempdir().expect("source");
        fs::write(source.path().join("file"), b"version one").expect("source");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let backup_id = BackupId::new();
        let mut first = BackupOptions::new(backup_id, "0001", "snapshot-bind-one");
        first.created_at_unix_ms = 1;
        engine
            .backup(source.path(), &first, &JobControl::new(), |_| {})
            .expect("first backup");
        fs::write(source.path().join("file"), b"version two").expect("source update");
        let mut second = first.clone();
        second.snapshot_id = "0002".to_owned();
        second.job_id = "snapshot-bind-two".to_owned();
        second.created_at_unix_ms = 2;
        let second_result = engine
            .backup(source.path(), &second, &JobControl::new(), |_| {})
            .expect("second backup");
        let snapshots = node_data
            .path()
            .join("store/snapshots")
            .join(backup_id.to_string());
        let requested_path = snapshots.join("0002.json");
        fs::copy(snapshots.join("0001.json"), &requested_path).expect("replace requested snapshot");
        let mut rollback: serde_json::Value =
            serde_json::from_slice(&fs::read(&requested_path).expect("rollback metadata"))
                .expect("rollback JSON");
        rollback["snapshotId"] = serde_json::json!("0002");
        write_json_atomic(&requested_path, &rollback, false).expect("forge outer snapshot id");

        assert!(matches!(
            engine.verify_snapshot(backup_id, "0002"),
            Err(CoreError::AuthenticationFailed)
        ));

        let mut timestamp =
            serde_json::to_value(&second_result.stored_snapshot).expect("snapshot metadata");
        timestamp["committedAtUnixMs"] = serde_json::json!(3);
        write_json_atomic(&requested_path, &timestamp, false).expect("forge outer timestamp");
        assert!(matches!(
            engine.verify_snapshot(backup_id, "0002"),
            Err(CoreError::AuthenticationFailed)
        ));

        let mut key_epoch =
            serde_json::to_value(&second_result.stored_snapshot).expect("snapshot metadata");
        key_epoch["envelope"]["keyEpoch"] = serde_json::json!(2);
        write_json_atomic(&requested_path, &key_epoch, false).expect("forge key epoch");
        assert!(matches!(
            engine.verify_snapshot(backup_id, "0002"),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn authenticated_snapshot_rejects_locator_from_a_different_key_epoch() {
        let node_data = tempdir().expect("node data");
        let source = tempdir().expect("source");
        let plaintext = b"epoch-bound locator";
        fs::write(source.path().join("file"), plaintext).expect("source");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let backup_id = BackupId::new();
        let mut options = BackupOptions::new(backup_id, "epoch-one", "epoch-one-job");
        options.created_at_unix_ms = 11;
        let result = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("backup");
        let key = engine.load_backup_key(backup_id).expect("backup key");
        let wrong_epoch = key
            .encrypt_chunk(backup_id, 2, plaintext)
            .expect("wrong epoch chunk");
        engine.store().put(&wrong_epoch).expect("store wrong epoch");
        let mut manifest = result.manifest;
        manifest.snapshot_id = "epoch-mismatch".to_owned();
        manifest.provider_acknowledgements.clear();
        let reference = manifest
            .entries
            .iter_mut()
            .flat_map(|entry| &mut entry.chunks)
            .next()
            .expect("chunk reference");
        reference.opaque_locator = wrong_epoch.opaque_locator.clone();
        reference.ciphertext_length = wrong_epoch.ciphertext_length();
        let envelope = encrypt_manifest(&manifest, 1, &key, &engine.identity).expect("envelope");
        let snapshot = StoredSnapshot::new(
            backup_id,
            "epoch-mismatch",
            envelope,
            BTreeSet::from([wrong_epoch.opaque_locator]),
            options.created_at_unix_ms,
        )
        .expect("snapshot");
        engine.store().commit_snapshot(&snapshot).expect("commit");

        assert!(matches!(
            engine.verify_snapshot(backup_id, "epoch-mismatch"),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn garbage_collection_rejects_unsigned_retention_locator_mutation() {
        let node_data = tempdir().expect("node data");
        let source = tempdir().expect("source");
        fs::write(source.path().join("file"), b"retained").expect("source");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let backup_id = BackupId::new();
        let mut options = BackupOptions::new(backup_id, "0001", "gc-binding");
        options.created_at_unix_ms = 7;
        let result = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("backup");
        let retained = result
            .stored_snapshot
            .chunk_locators
            .iter()
            .next()
            .expect("retained locator")
            .clone();
        let unrelated_key = BackupKey::generate();
        let orphan = unrelated_key
            .encrypt_chunk(BackupId::new(), 1, b"orphan")
            .expect("orphan chunk");
        engine.store().put(&orphan).expect("orphan put");
        let snapshot_path = node_data
            .path()
            .join("store/snapshots")
            .join(backup_id.to_string())
            .join("0001.json");
        let mut metadata: serde_json::Value =
            serde_json::from_slice(&fs::read(&snapshot_path).expect("metadata"))
                .expect("metadata JSON");
        metadata["chunkLocators"] = serde_json::json!([orphan.opaque_locator]);
        write_json_atomic(&snapshot_path, &metadata, false).expect("mutate unsigned locator set");

        assert!(matches!(
            engine.garbage_collect(),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(
            engine
                .store()
                .contains(&retained)
                .expect("retained remains")
        );
        assert!(
            engine
                .store()
                .contains(metadata["chunkLocators"][0].as_str().expect("locator"))
                .expect("orphan remains")
        );
    }

    #[test]
    fn startup_recovers_revocation_roster_and_current_roster_is_fully_verified() {
        let node_data = tempdir().expect("node data");
        let engine = Engine::open(test_options(node_data.path())).expect("engine");
        let peer = DeviceIdentity::generate().public_identity();
        engine
            .trust_peer(PeerGrant {
                peer_device_id: peer.device_id,
                public_key: peer.public_key,
                display_name: "Revoked peer".to_owned(),
                roles: BTreeSet::from([PeerRole::StorageProvider]),
                confirmed_at_unix_ms: 1,
                revoked: false,
            })
            .expect("initial roster");
        let previous = engine.config().expect("previous config");
        let mut candidate = previous.clone();
        candidate
            .trusted_peers
            .get_mut(&peer.device_id)
            .expect("peer")
            .revoked = true;
        let mut builder = SignedRosterBuilder::new(
            previous.roster_epoch.saturating_add(1),
            previous.roster_digest.clone(),
        );
        for grant in candidate.trusted_peers.values() {
            builder = builder.grant(grant.clone());
        }
        let roster = builder.sign(&engine.identity).expect("signed revocation");
        candidate.roster_epoch = roster.epoch;
        candidate.roster_digest = roster_digest(&roster).expect("roster digest");
        let transaction_path = roster_transaction_path(node_data.path());
        write_json_atomic(
            &transaction_path,
            &PendingRosterCommit::Local {
                schema_version: ROSTER_TRANSACTION_SCHEMA_VERSION,
                roster: roster.clone(),
                config: candidate,
            },
            true,
        )
        .expect("stage roster transaction");
        drop(engine);

        let recovered = Engine::open(test_options(node_data.path())).expect("recover");
        assert!(matches!(
            recovered.authorized_peer(peer.device_id, PeerRole::StorageProvider),
            Err(CoreError::PeerRevoked)
        ));
        assert_eq!(
            recovered.current_roster().expect("current roster"),
            Some(roster.clone())
        );
        assert!(!transaction_path.exists());

        let mut tampered = roster;
        tampered.signature.push('A');
        write_json_atomic(&node_data.path().join("roster.json"), &tampered, false)
            .expect("tamper roster");
        assert!(recovered.current_roster().is_err());
    }
}
