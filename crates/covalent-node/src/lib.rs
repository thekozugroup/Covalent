//! Local authenticated API, embedded accessible console, discovery, and peer transport.

pub mod discovery;
pub mod network_pairing;
pub mod runtime;
pub mod transport;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Path as AxumPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    BackupOptions, ChunkProvider, CoreError, Engine, JobControl, JobState, PairingConfirmation,
    PairingSession, PreviewAction, RestoreOptions, RestorePlan, RestorePreviewEntry, RosterCursor,
    canonical_target_inventory_digest,
};
use covalent_protocol::{
    ApiErrorBody, BackupId, BackupSummary, ConflictPolicy, DeviceId, EntryKind, NodeStatus,
    PROTOCOL_VERSION, PairingInvitation, PeerRole, PlatformTier, RelativePath, ReplicaAvailability,
    ReplicaIntent, SignedRoster, TargetInventory, TargetInventoryBinding, TargetInventoryEntry,
    TransportBinding,
};
use network_pairing::{NetworkPairingItem, NetworkPairingManager, NetworkPairingStatus};
use http_body_util::BodyExt as _;
use rand_core::{OsRng, RngCore};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::io::ReaderStream;
use walkdir::WalkDir;
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const INDEX_HTML: &str = include_str!("../../../packaging/web/index.html");
const APP_CSS: &str = include_str!("../../../packaging/web/app.css");
const APP_JS: &str = include_str!("../../../packaging/web/app.js");
const PAIRING_FLOW_JS: &str = include_str!("../../../packaging/web/pairing-flow.js");
const RESTORE_PLAN_FLOW_JS: &str = include_str!("../../../packaging/web/restore-plan-flow.js");
const MAX_LOCAL_API_BODY_BYTES: usize = 2 * 1_024 * 1_024;
const PROVIDER_CONNECTION_SCHEMA_VERSION: u16 = 1;
const MAX_PROVIDER_CONNECTION_STATE_BYTES: usize = 16 * 1_024 * 1_024;
const ARCHIVE_METADATA_HEADER: &str = "x-covalent-archive-metadata";
const ARCHIVE_RESULT_HEADER: &str = "x-covalent-restore-result";
const MAX_ARCHIVE_METADATA_BYTES: usize = 32 * 1_024;
const DEFAULT_MAX_ARCHIVE_COMPRESSED_BYTES: u64 = 64_u64 << 30;
const DEFAULT_MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 256_u64 << 30;
const DEFAULT_MAX_ARCHIVE_ENTRIES: usize = 250_000;
const DEFAULT_MAX_ARCHIVE_JOBS: usize = 256;
const DEFAULT_MAX_ARCHIVE_STAGING_BYTES: u64 = 512_u64 << 30;
const DEFAULT_MAX_RETAINED_RESULT_BYTES: u64 = 64_u64 << 30;
const DEFAULT_MAX_RETAINED_RESULTS: usize = 64;
const DEFAULT_ARCHIVE_FREE_SPACE_RESERVE_BYTES: u64 = 512_u64 << 20;
const ARCHIVE_UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const ARCHIVE_UPLOAD_MAX_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
const MIN_ARCHIVE_UPLOAD_BYTES_PER_SECOND: u64 = 16 * 1_024;
const ARCHIVE_PROCESSING_MAX_DURATION: Duration = Duration::from_secs(6 * 60 * 60);
const MIN_ARCHIVE_PROCESS_BYTES_PER_SECOND: u64 = 64 * 1_024;
const MAX_ARCHIVE_COMPRESSION_RATIO: u64 = 1_000;
const ARCHIVE_RESTORE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_LOCAL_JOBS: usize = 1_024;
const LOCAL_JOB_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_CONCURRENT_ENGINE_JOBS: usize = 1;
const MAX_RESTORE_PLANS: usize = 1_024;
const MAX_RESTORE_PLAN_BYTES: u64 = 256 * 1_024 * 1_024;
const RESTORE_PLAN_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const RESTORE_PLAN_ID_HEADER: &str = "x-covalent-restore-plan-id";
const RESTORE_PLAN_DIGEST_HEADER: &str = "x-covalent-restore-plan-digest";
const RESTORE_PLAN_ID_DOMAIN: &[u8] = b"covalent/restore-plan-id/v1";
const JOB_ACK_REQUIRED_HEADER: &str = "x-covalent-job-ack-required";
const ARCHIVE_UPLOAD_OFFSET_HEADER: &str = "x-covalent-upload-offset";
const ARCHIVE_UPLOAD_LENGTH_HEADER: &str = "x-covalent-upload-length";
const ARCHIVE_UPLOAD_DIGEST_HEADER: &str = "x-covalent-upload-digest";
const ARCHIVE_UPLOAD_SESSION_SCHEMA_VERSION: u16 = 1;
const TARGET_INVENTORY_SCHEMA_VERSION: u16 = 1;
const MAX_TARGET_INVENTORY_ENTRIES: u64 = 250_000;
const MAX_TARGET_INVENTORY_STAGING_BYTES: u64 = 256 * 1_024 * 1_024;
const TARGET_INVENTORY_OFFSET_HEADER: &str = "x-covalent-inventory-offset";

/// Configurable admission limits for streamed mobile archives.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveLimits {
    /// Maximum compressed request bytes.
    pub maximum_compressed_bytes: u64,
    /// Maximum declared expanded bytes across ZIP entries.
    pub maximum_uncompressed_bytes: u64,
    /// Maximum ZIP entries.
    pub maximum_entries: usize,
    /// Maximum durable staged archive jobs.
    pub maximum_jobs: usize,
    /// Maximum bytes across all active and retained archive staging.
    pub maximum_staging_bytes: u64,
    /// Maximum bytes across completed results waiting for acknowledgement.
    pub maximum_retained_result_bytes: u64,
    /// Maximum completed results waiting for acknowledgement.
    pub maximum_retained_results: usize,
    /// Free space that must remain after admission.
    pub free_space_reserve_bytes: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            maximum_compressed_bytes: DEFAULT_MAX_ARCHIVE_COMPRESSED_BYTES,
            maximum_uncompressed_bytes: DEFAULT_MAX_ARCHIVE_UNCOMPRESSED_BYTES,
            maximum_entries: DEFAULT_MAX_ARCHIVE_ENTRIES,
            maximum_jobs: DEFAULT_MAX_ARCHIVE_JOBS,
            maximum_staging_bytes: DEFAULT_MAX_ARCHIVE_STAGING_BYTES,
            maximum_retained_result_bytes: DEFAULT_MAX_RETAINED_RESULT_BYTES,
            maximum_retained_results: DEFAULT_MAX_RETAINED_RESULTS,
            free_space_reserve_bytes: DEFAULT_ARCHIVE_FREE_SPACE_RESERVE_BYTES,
        }
    }
}

impl ArchiveLimits {
    fn validate(self) -> Result<Self, CoreError> {
        if !(1 << 20..=1_u64 << 40).contains(&self.maximum_compressed_bytes)
            || self.maximum_uncompressed_bytes < self.maximum_compressed_bytes
            || self.maximum_uncompressed_bytes > 4_u64 << 40
            || !(1..=1_000_000).contains(&self.maximum_entries)
            || !(1..=4_096).contains(&self.maximum_jobs)
            || self.maximum_staging_bytes < self.maximum_uncompressed_bytes
            || self.maximum_staging_bytes > 8_u64 << 40
            || self.maximum_retained_result_bytes < self.maximum_compressed_bytes
            || self.maximum_retained_result_bytes > self.maximum_staging_bytes
            || !(1..=self.maximum_jobs).contains(&self.maximum_retained_results)
        {
            return Err(CoreError::InvalidState(
                "invalid archive resource limits".to_owned(),
            ));
        }
        Ok(self)
    }
}

#[derive(Default)]
struct ArchiveStagingAdmission {
    reserved_bytes: u64,
}

struct ArchiveStagingReservation {
    admission: Arc<Mutex<ArchiveStagingAdmission>>,
    bytes: u64,
}

impl Drop for ArchiveStagingReservation {
    fn drop(&mut self) {
        if let Ok(mut admission) = self.admission.lock() {
            admission.reserved_bytes = admission.reserved_bytes.saturating_sub(self.bytes);
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveUploadSession {
    schema_version: u16,
    total_length: u64,
    sha256_digest: String,
    metadata_digest: String,
}

#[cfg(unix)]
struct SafeUploadDirectory {
    descriptor: std::os::fd::OwnedFd,
    owner: u32,
}

#[cfg(unix)]
impl SafeUploadDirectory {
    fn open(path: &Path) -> Result<Self, ApiError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ApiError::internal("archive upload directory could not be opened safely"))?;
        let stat = fstat(&descriptor)
            .map_err(|_| ApiError::internal("archive upload directory could not be inspected"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory || stat.st_mode & 0o077 != 0
        {
            return Err(ApiError::internal(
                "archive upload directory is not private",
            ));
        }
        Ok(Self {
            descriptor,
            owner: stat.st_uid,
        })
    }

    fn read_private_file(
        &self,
        name: &str,
        maximum_bytes: u64,
    ) -> Result<Option<Vec<u8>>, ApiError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
        use std::io::Read as _;

        let descriptor = match openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(_) => {
                return Err(ApiError::internal(
                    "archive upload file could not be opened safely",
                ));
            }
        };
        let stat = fstat(&descriptor)
            .map_err(|_| ApiError::internal("archive upload file could not be inspected"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != self.owner
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum_bytes
        {
            return Err(ApiError::internal(
                "archive upload file is not a private regular file",
            ));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(stat.st_size).unwrap_or(0));
        File::from(descriptor)
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ApiError::internal("archive upload file could not be read"))?;
        if bytes.len() as u64 > maximum_bytes {
            return Err(ApiError::internal(
                "archive upload file exceeds its safe bound",
            ));
        }
        Ok(Some(bytes))
    }

    fn create_private_file(&self, name: &str, bytes: &[u8]) -> Result<(), ApiError> {
        use rustix::fs::{Mode, OFlags, fsync, openat};
        use std::io::Write as _;

        let descriptor = openat(
            &self.descriptor,
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| ApiError::internal("archive upload state could not be created safely"))?;
        let mut file = File::from(descriptor);
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ApiError::internal("archive upload state could not be persisted"))?;
        fsync(&self.descriptor)
            .map_err(|_| ApiError::internal("archive upload directory could not be synced"))
    }

    fn private_file_length(&self, name: &str, maximum_bytes: u64) -> Result<u64, ApiError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

        let descriptor = match openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(0),
            Err(_) => {
                return Err(ApiError::internal(
                    "archive partial upload could not be opened safely",
                ));
            }
        };
        let stat = fstat(&descriptor)
            .map_err(|_| ApiError::internal("archive partial upload could not be inspected"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != self.owner
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum_bytes
        {
            return Err(ApiError::internal(
                "archive partial upload is not a private regular file",
            ));
        }
        Ok(stat.st_size as u64)
    }

    fn open_private_reader(&self, name: &str, maximum_bytes: u64) -> Result<File, ApiError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

        let descriptor = openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| ApiError::internal("archive upload file could not be opened safely"))?;
        let stat = fstat(&descriptor)
            .map_err(|_| ApiError::internal("archive upload file could not be inspected"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != self.owner
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum_bytes
        {
            return Err(ApiError::internal(
                "archive upload file is not a private regular file",
            ));
        }
        Ok(File::from(descriptor))
    }

    fn open_partial_append(&self, name: &str) -> Result<File, ApiError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

        let descriptor = openat(
            &self.descriptor,
            name,
            OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|_| ApiError::internal("archive staging file could not be created safely"))?;
        let stat = fstat(&descriptor)
            .map_err(|_| ApiError::internal("archive staging file could not be inspected"))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != self.owner
            || stat.st_mode & 0o077 != 0
        {
            return Err(ApiError::internal(
                "archive staging file is not a private regular file",
            ));
        }
        Ok(File::from(descriptor))
    }

    fn commit(&self, temporary_name: &str, final_name: &str) -> Result<(), ApiError> {
        use rustix::fs::{RenameFlags, fsync, renameat_with};

        renameat_with(
            &self.descriptor,
            temporary_name,
            &self.descriptor,
            final_name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|_| ApiError::internal("archive staging commit failed safely"))?;
        fsync(&self.descriptor)
            .map_err(|_| ApiError::internal("archive staging commit could not be synced"))
    }

    fn remove(&self, name: &str) {
        use rustix::fs::{AtFlags, fsync, unlinkat};
        let _ = unlinkat(&self.descriptor, name, AtFlags::empty());
        let _ = fsync(&self.descriptor);
    }

    fn sync(&self) -> Result<(), ApiError> {
        rustix::fs::fsync(&self.descriptor)
            .map_err(|_| ApiError::internal("private archive directory could not be synced"))
    }
}

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchivePreparedSource {
    schema_version: u16,
    metadata_digest: String,
    upload_digest: String,
}

struct JobEntry {
    control: JobControl,
    active: bool,
    last_touched: SystemTime,
}

#[derive(Default)]
struct JobRegistry {
    entries: BTreeMap<String, JobEntry>,
}

impl JobRegistry {
    fn expired_job_ids(&self, now: SystemTime) -> Vec<String> {
        self.entries
            .iter()
            .filter(|(_, entry)| {
                !entry.active
                    && now
                        .duration_since(entry.last_touched)
                        .is_ok_and(|age| age >= LOCAL_JOB_IDLE_TTL)
            })
            .map(|(job_id, _)| job_id.clone())
            .collect()
    }
}

struct JobLease {
    registry: Arc<Mutex<JobRegistry>>,
    job_id: String,
    control: JobControl,
    settled: bool,
}

impl JobLease {
    fn control(&self) -> JobControl {
        self.control.clone()
    }

    fn preserve_for_resume(&mut self) -> Result<(), CoreError> {
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let entry = registry.entries.get_mut(&self.job_id).ok_or_else(|| {
            CoreError::InvalidState("active job disappeared from registry".to_owned())
        })?;
        entry.active = false;
        entry.last_touched = SystemTime::now();
        self.settled = true;
        Ok(())
    }

    fn finish(&mut self) -> Result<(), CoreError> {
        self.registry
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .entries
            .remove(&self.job_id);
        self.settled = true;
        Ok(())
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        // Dropping an HTTP handler while its blocking worker is still running must stop the
        // worker and leave a resumable registry entry instead of orphaning background work.
        self.control.pause();
        if let Ok(mut registry) = self.registry.lock()
            && let Some(entry) = registry.entries.get_mut(&self.job_id)
        {
            entry.active = false;
            entry.last_touched = SystemTime::now();
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderConnection {
    peer_id: DeviceId,
    address: SocketAddr,
    certificate_der: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderConnectionState {
    schema_version: u16,
    providers: BTreeMap<DeviceId, ProviderConnection>,
}

impl Default for ProviderConnectionState {
    fn default() -> Self {
        Self {
            schema_version: PROVIDER_CONNECTION_SCHEMA_VERSION,
            providers: BTreeMap::new(),
        }
    }
}

/// Stateful authenticated local API context.
#[derive(Clone)]
pub struct AppState {
    engine: Arc<Engine>,
    platform_tier: PlatformTier,
    api_token: Arc<Zeroizing<String>>,
    jobs: Arc<Mutex<JobRegistry>>,
    peer_address: Option<SocketAddr>,
    provider_connections: Arc<Mutex<BTreeMap<DeviceId, ProviderConnection>>>,
    provider_state_path: Option<Arc<PathBuf>>,
    transport_certificate: Option<Arc<Vec<u8>>>,
    network_pairing: Arc<NetworkPairingManager>,
    discovery_controller: Option<Arc<discovery::DiscoveryController>>,
    archive_limits: ArchiveLimits,
    archive_backup_root: Arc<PathBuf>,
    archive_backup_lock: Arc<Mutex<()>>,
    archive_restore_root: Arc<PathBuf>,
    archive_restore_lock: Arc<Mutex<()>>,
    archive_staging_admission: Arc<Mutex<ArchiveStagingAdmission>>,
    restore_plan_root: Arc<PathBuf>,
    restore_plan_lock: Arc<Mutex<()>>,
    engine_job_permits: Arc<Semaphore>,
}

impl AppState {
    /// Creates local state. The token must come from private durable storage.
    pub fn new(
        engine: Arc<Engine>,
        platform_tier: PlatformTier,
        api_token: String,
    ) -> Result<Self, CoreError> {
        if api_token.len() < 32 || api_token.len() > 512 {
            return Err(CoreError::InvalidKeyMaterial);
        }
        let data_directory = engine
            .store()
            .root()
            .parent()
            .ok_or_else(|| CoreError::InvalidState("engine store has no parent".to_owned()))?;
        let archive_restore_root =
            create_private_directory(data_directory.join("archive-restores"))?;
        prune_stale_archive_restore_targets(&archive_restore_root)?;
        let archive_backup_root = create_private_directory(data_directory.join("archive-backups"))?;
        prune_stale_archive_restore_targets(&archive_backup_root)?;
        let restore_plan_root = create_private_directory(data_directory.join("restore-plans"))?;
        prune_stale_private_files(
            &restore_plan_root,
            RESTORE_PLAN_MAX_AGE,
            MAX_RESTORE_PLAN_BYTES,
            valid_plan_identifier,
        )?;
        let network_pairing = Arc::new(NetworkPairingManager::open(
            Arc::clone(&engine),
            data_directory.join("network-pairing.json"),
        )?);
        Ok(Self {
            engine,
            platform_tier,
            api_token: Arc::new(Zeroizing::new(api_token)),
            jobs: Arc::new(Mutex::new(JobRegistry::default())),
            peer_address: None,
            provider_connections: Arc::new(Mutex::new(BTreeMap::new())),
            provider_state_path: None,
            transport_certificate: None,
            network_pairing,
            discovery_controller: None,
            archive_limits: ArchiveLimits::default(),
            archive_backup_root: Arc::new(archive_backup_root),
            archive_backup_lock: Arc::new(Mutex::new(())),
            archive_restore_root: Arc::new(archive_restore_root),
            archive_restore_lock: Arc::new(Mutex::new(())),
            archive_staging_admission: Arc::new(Mutex::new(ArchiveStagingAdmission::default())),
            restore_plan_root: Arc::new(restore_plan_root),
            restore_plan_lock: Arc::new(Mutex::new(())),
            engine_job_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_ENGINE_JOBS)),
        })
    }

    /// Sets the exact advertised/discovered QUIC endpoint after the daemon binds it.
    #[must_use]
    pub const fn with_peer_address(mut self, peer_address: SocketAddr) -> Self {
        self.peer_address = Some(peer_address);
        self
    }

    /// Publishes the daemon's public TLS certificate through the authenticated local API.
    #[must_use]
    pub fn with_transport_certificate(mut self, certificate_der: Vec<u8>) -> Self {
        self.transport_certificate = Some(Arc::new(certificate_der));
        self
    }

    fn local_transport_binding(&self) -> Result<TransportBinding, CoreError> {
        let certificate = self.transport_certificate.as_deref().ok_or_else(|| {
            CoreError::InvalidState("transport identity is unavailable".to_owned())
        })?;
        let display_name = self.engine.config()?.device_name;
        let address = self.peer_address.ok_or_else(|| {
            CoreError::InvalidState("advertised peer endpoint is unavailable".to_owned())
        })?;
        if address.ip().is_unspecified() || address.port() == 0 {
            return Err(CoreError::InvalidState(
                "advertised peer endpoint must be concrete".to_owned(),
            ));
        }
        Ok(TransportBinding {
            peer_id: self.engine.device_id(),
            display_name,
            address: address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(certificate),
            certificate_fingerprint: sha256_hex(certificate),
        })
    }

    /// Connects persisted discovery settings to the live network advertiser.
    #[must_use]
    pub fn with_discovery_controller(
        mut self,
        controller: Arc<discovery::DiscoveryController>,
    ) -> Self {
        self.discovery_controller = Some(controller);
        self
    }

    /// Overrides streamed archive admission limits after validating safe bounds.
    pub fn with_archive_limits(mut self, limits: ArchiveLimits) -> Result<Self, CoreError> {
        self.archive_limits = limits.validate()?;
        Ok(self)
    }

    /// Loads pinned remembered provider connections and activates them.
    pub fn with_provider_state(mut self, path: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let path = path.into();
        let mut state = match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > MAX_PROVIDER_CONNECTION_STATE_BYTES as u64
                {
                    return Err(CoreError::InvalidState(
                        "invalid provider connection state".to_owned(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(CoreError::InvalidState(
                            "provider connection state permissions are too broad".to_owned(),
                        ));
                    }
                }
                let bytes = fs::read(&path).map_err(|source| CoreError::Io {
                    operation: "read provider connection state",
                    path: path.clone(),
                    source,
                })?;
                serde_json::from_slice::<ProviderConnectionState>(&bytes)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                ProviderConnectionState::default()
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect provider connection state",
                    path,
                    source,
                });
            }
        };
        if state.schema_version != PROVIDER_CONNECTION_SCHEMA_VERSION
            || state.providers.len() > 128
            || state
                .providers
                .iter()
                .any(|(peer_id, provider)| peer_id != &provider.peer_id)
        {
            return Err(CoreError::InvalidState(
                "unsupported or excessive provider connection state".to_owned(),
            ));
        }
        state.providers.retain(|peer_id, provider| {
            let Ok(binding) = self
                .engine
                .trusted_peer_transport(*peer_id, PeerRole::StorageProvider)
            else {
                return false;
            };
            let Ok(certificate) = URL_SAFE_NO_PAD.decode(&binding.certificate_der) else {
                return false;
            };
            let Ok(address) = binding.address.parse::<SocketAddr>() else {
                return false;
            };
            if certificate.is_empty()
                || certificate.len() > 64 * 1_024
                || !valid_lowercase_digest(&binding.certificate_fingerprint)
                || sha256_hex(&certificate) != binding.certificate_fingerprint
            {
                return false;
            }
            // Persisted version-1 records did not label their fingerprint algorithm. Migrate
            // only from the signed DER retained by pairing; never trust a stored digest alone.
            provider.address = address;
            provider.certificate_der = binding.certificate_der;
            true
        });
        self.activate_provider_connections(&state.providers)?;
        *self
            .provider_connections
            .lock()
            .map_err(|_| CoreError::Synchronization)? = state.providers;
        self.provider_state_path = Some(Arc::new(path));
        self.persist_provider_connections()?;
        Ok(self)
    }

    /// Shared engine for daemon-owned peer services.
    #[must_use]
    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }

    fn prune_expired_jobs(&self) -> Result<(), ApiError> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
        for job_id in jobs.expired_job_ids(SystemTime::now()) {
            // Hold the registry lock so a retry cannot reactivate a job while its durable
            // checkpoint and staging are being evicted.
            discard_job_artifacts(self, &job_id)?;
            jobs.entries.remove(&job_id);
        }
        Ok(())
    }

    fn start_job(&self, job_id: &str) -> Result<JobLease, ApiError> {
        if !valid_job_identifier(job_id) {
            return Err(ApiError::bad_request(
                "invalid_job_id",
                "The job ID is invalid.",
            ));
        }
        self.prune_expired_jobs()?;
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
        if jobs.entries.len() >= MAX_LOCAL_JOBS && !jobs.entries.contains_key(job_id) {
            let eviction = jobs
                .entries
                .iter()
                .filter(|(_, entry)| !entry.active)
                .min_by_key(|(_, entry)| entry.last_touched)
                .map(|(job_id, _)| job_id.clone());
            let Some(eviction) = eviction else {
                return Err(ApiError::from_core(CoreError::ResourceLimit(
                    "active local jobs",
                )));
            };
            // Keep the registry locked so an inactive job cannot be resumed while its durable
            // state is evicted to make room for newer work.
            discard_job_artifacts(self, &eviction)?;
            jobs.entries.remove(&eviction);
        }
        let entry = jobs
            .entries
            .entry(job_id.to_owned())
            .or_insert_with(|| JobEntry {
                control: JobControl::new(),
                active: false,
                last_touched: SystemTime::now(),
            });
        if entry.active {
            return Err(ApiError::conflict(
                "job_active",
                "The job is already executing.",
            ));
        }
        entry.active = true;
        entry.last_touched = SystemTime::now();
        Ok(JobLease {
            registry: Arc::clone(&self.jobs),
            job_id: job_id.to_owned(),
            control: entry.control.clone(),
            settled: false,
        })
    }

    fn admit_engine_job(&self) -> Result<OwnedSemaphorePermit, ApiError> {
        Arc::clone(&self.engine_job_permits)
            .try_acquire_owned()
            .map_err(|_| {
                ApiError::too_many_requests(
                    "The node is already processing another storage job. Retry shortly.",
                )
            })
    }

    fn reserve_archive_staging(
        &self,
        additional_bytes: u64,
    ) -> Result<ArchiveStagingReservation, ApiError> {
        let mut admission = self
            .archive_staging_admission
            .lock()
            .map_err(|_| ApiError::internal("archive staging admission lock failed"))?;
        let used_bytes = archive_tree_bytes(self.archive_backup_root.as_path())?
            .checked_add(archive_tree_bytes(self.archive_restore_root.as_path())?)
            .ok_or_else(|| ApiError::payload_too_large("Archive staging size overflowed."))?;
        let admitted_bytes = used_bytes
            .checked_add(admission.reserved_bytes)
            .and_then(|bytes| bytes.checked_add(additional_bytes))
            .ok_or_else(|| ApiError::payload_too_large("Archive staging size overflowed."))?;
        if admitted_bytes > self.archive_limits.maximum_staging_bytes {
            return Err(ApiError::insufficient_storage(
                "The node archive staging byte budget is exhausted.",
            ));
        }
        let available = fs2::available_space(self.archive_backup_root.as_path())
            .map_err(|_| ApiError::internal("archive staging capacity is unavailable"))?;
        let required_available = admission
            .reserved_bytes
            .checked_add(additional_bytes)
            .and_then(|bytes| bytes.checked_add(self.archive_limits.free_space_reserve_bytes))
            .ok_or_else(|| ApiError::payload_too_large("Archive staging size overflowed."))?;
        if available < required_available {
            return Err(ApiError::insufficient_storage(
                "The node does not have enough reserved capacity for this archive.",
            ));
        }
        admission.reserved_bytes = admission
            .reserved_bytes
            .checked_add(additional_bytes)
            .ok_or_else(|| ApiError::payload_too_large("Archive staging size overflowed."))?;
        Ok(ArchiveStagingReservation {
            admission: Arc::clone(&self.archive_staging_admission),
            bytes: additional_bytes,
        })
    }

    fn activate_provider_connections(
        &self,
        configs: &BTreeMap<DeviceId, ProviderConnection>,
    ) -> Result<(), CoreError> {
        let mut providers = Vec::<Arc<dyn ChunkProvider>>::new();
        for config in configs.values() {
            let identity = self
                .engine
                .authorized_peer(config.peer_id, PeerRole::StorageProvider)?;
            let certificate = URL_SAFE_NO_PAD
                .decode(&config.certificate_der)
                .map_err(|_| CoreError::InvalidKeyMaterial)?;
            let provider = transport::QuicProvider::new(
                config.address,
                identity,
                certificate,
                Arc::clone(&self.engine),
            )?;
            providers.push(Arc::new(provider));
        }
        self.engine.set_connected_providers(providers)
    }

    fn persist_provider_connections(&self) -> Result<(), CoreError> {
        let providers = self
            .provider_connections
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        self.persist_provider_connections_value(&providers)
    }

    fn persist_provider_connections_value(
        &self,
        providers: &BTreeMap<DeviceId, ProviderConnection>,
    ) -> Result<(), CoreError> {
        let Some(path) = &self.provider_state_path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec_pretty(&ProviderConnectionState {
            schema_version: PROVIDER_CONNECTION_SCHEMA_VERSION,
            providers: providers.clone(),
        })?;
        if bytes.len() > MAX_PROVIDER_CONNECTION_STATE_BYTES {
            return Err(CoreError::ResourceLimit("provider connection state"));
        }
        persist_private_file(path, &bytes)
    }

    fn connect_provider(&self, config: ProviderConnection) -> Result<(), CoreError> {
        if config.peer_id == self.engine.device_id() || config.certificate_der.len() > 128 * 1_024 {
            return Err(CoreError::InvalidState(
                "invalid provider connection".to_owned(),
            ));
        }
        let mut configs = self
            .provider_connections
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        if configs.len() >= 128 && !configs.contains_key(&config.peer_id) {
            return Err(CoreError::ResourceLimit("connected providers"));
        }
        let previous = configs.clone();
        configs.insert(config.peer_id, config);
        if let Err(error) = self.activate_provider_connections(&configs) {
            *configs = previous;
            return Err(error);
        }
        if let Err(error) = self.persist_provider_connections_value(&configs) {
            let _ = self.activate_provider_connections(&previous);
            *configs = previous;
            return Err(error);
        }
        Ok(())
    }

    fn disconnect_provider(&self, peer_id: DeviceId) -> Result<(), CoreError> {
        let mut configs = self
            .provider_connections
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let previous = configs.clone();
        configs.remove(&peer_id);
        if let Err(error) = self.activate_provider_connections(&configs) {
            *configs = previous;
            return Err(error);
        }
        if let Err(error) = self.persist_provider_connections_value(&configs) {
            let _ = self.activate_provider_connections(&previous);
            *configs = previous;
            return Err(error);
        }
        Ok(())
    }

    fn connect_completed_network_pairing(
        &self,
        item: &NetworkPairingItem,
    ) -> Result<(), CoreError> {
        if item.state != NetworkPairingStatus::Complete {
            return Ok(());
        }
        let transport = item
            .peer_transport
            .as_ref()
            .ok_or(CoreError::AuthenticationFailed)?;
        let address = transport
            .address
            .parse::<SocketAddr>()
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let certificate = URL_SAFE_NO_PAD
            .decode(&transport.certificate_der)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        if certificate.is_empty()
            || certificate.len() > 64 * 1_024
            || sha256_hex(&certificate) != transport.certificate_fingerprint
        {
            return Err(CoreError::AuthenticationFailed);
        }
        self.connect_provider(ProviderConnection {
            peer_id: transport.peer_id,
            address,
            certificate_der: transport.certificate_der.clone(),
        })
    }
}

impl fmt::Debug for AppState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppState")
            .field("device_id", &self.engine.device_id())
            .field("platform_tier", &self.platform_tier)
            .field("api_token", &"[REDACTED]")
            .finish()
    }
}

/// Cleartext bearer APIs are confined to device loopback. Network access must terminate TLS in
/// a same-host proxy that forwards only to this loopback listener.
pub fn validate_cleartext_api_bind(address: SocketAddr) -> Result<(), CoreError> {
    if address.ip().is_loopback() {
        Ok(())
    } else {
        Err(CoreError::InvalidState(
            "cleartext bearer API must bind to loopback; terminate verified TLS in a same-host proxy"
                .to_owned(),
        ))
    }
}

/// Builds the stable versioned local API and static console router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(css))
        .route("/assets/app.js", get(javascript))
        .route("/assets/pairing-flow.js", get(pairing_flow_javascript))
        .route(
            "/assets/restore-plan-flow.js",
            get(restore_plan_flow_javascript),
        )
        .route("/healthz", get(health))
        .route("/api/v1/status", get(status))
        .route("/api/v1/transport/identity", get(transport_identity))
        .route("/api/v1/discovery", get(discovery_candidates))
        .route("/api/v1/config/export", post(config_export))
        .route("/api/v1/config/import", post(config_import))
        .route("/api/v1/pair/invitations", post(pair_invitation))
        .route("/api/v1/pair/network/pending", get(pair_network_pending))
        .route(
            "/api/v1/pair/network/{pairing_id}/confirm",
            post(pair_network_confirm),
        )
        .route(
            "/api/v1/pair/network/{pairing_id}",
            delete(pair_network_cancel),
        )
        .route("/api/v1/pair/accept", post(pair_accept))
        .route(
            "/api/v1/pair/confirm/responder",
            post(pair_confirm_responder),
        )
        .route("/api/v1/pair/confirm/inviter", post(pair_confirm_inviter))
        .route(
            "/api/v1/pair/finalize/responder",
            post(pair_finalize_responder),
        )
        .route("/api/v1/pair/finalize/inviter", post(pair_finalize_inviter))
        .route("/api/v1/peers/revoke", post(revoke_peer))
        .route("/api/v1/providers", get(list_providers))
        .route("/api/v1/providers/connect", post(connect_provider))
        .route("/api/v1/providers/disconnect", post(disconnect_provider))
        .route("/api/v1/rosters/current", get(current_roster))
        .route("/api/v1/rosters/accept", post(accept_roster))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/control", post(job_control))
        .route("/api/v1/jobs/discard", post(discard_job))
        .route("/api/v1/jobs/acknowledge", post(acknowledge_job))
        .route("/api/v1/backups", get(list_backups).post(backup))
        .route("/api/v1/backups/archive", post(backup_archive))
        .route("/api/v1/backups/verify", post(verify_backup))
        .route("/api/v1/restores/preview", post(restore_preview))
        .route("/api/v1/restores/execute", post(restore_execute))
        .route("/api/v1/restores/plans/{plan_id}", get(restore_plan_page))
        .route(
            "/api/v1/restores/archive/inventories",
            post(begin_target_inventory),
        )
        .route(
            "/api/v1/restores/archive/inventories/{inventory_id}/pages",
            post(append_target_inventory_page),
        )
        .route(
            "/api/v1/restores/archive/inventories/{inventory_id}/finalize",
            post(finalize_target_inventory),
        )
        .route(
            "/api/v1/restores/archive/preview",
            post(restore_archive_preview),
        )
        .route(
            "/api/v1/restores/archive/execute",
            post(restore_archive_execute),
        )
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_LOCAL_API_BODY_BYTES))
        .with_state(state)
}

/// Loads or creates the bearer token used by local mutation APIs.
pub fn load_or_create_local_api_token(path: impl AsRef<Path>) -> Result<String, CoreError> {
    let path = path.as_ref();
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1_024 {
                return Err(CoreError::InvalidState(
                    "invalid local API token file".to_owned(),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(CoreError::InvalidState(
                        "local API token permissions are too broad".to_owned(),
                    ));
                }
            }
            let token = fs::read_to_string(path).map_err(|source| CoreError::Io {
                operation: "read local API token",
                path: path.to_path_buf(),
                source,
            })?;
            let token = token.trim().to_owned();
            if token.len() < 32 || token.len() > 512 {
                return Err(CoreError::InvalidKeyMaterial);
            }
            Ok(token)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or_else(|| {
                CoreError::InvalidState("local API token path has no parent".to_owned())
            })?;
            fs::create_dir_all(parent).map_err(|source| CoreError::Io {
                operation: "create local API token directory",
                path: parent.to_path_buf(),
                source,
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(
                    |source| CoreError::Io {
                        operation: "protect local API token directory",
                        path: parent.to_path_buf(),
                        source,
                    },
                )?;
            }
            let mut random = [0_u8; 32];
            OsRng.fill_bytes(&mut random);
            let token = URL_SAFE_NO_PAD.encode(random);
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
                    operation: "stage local API token",
                    path: path.to_path_buf(),
                    source,
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                temporary
                    .as_file()
                    .set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|source| CoreError::Io {
                        operation: "protect local API token",
                        path: path.to_path_buf(),
                        source,
                    })?;
            }
            use std::io::Write as _;
            temporary
                .write_all(token.as_bytes())
                .and_then(|()| temporary.as_file().sync_all())
                .map_err(|source| CoreError::Io {
                    operation: "sync local API token",
                    path: path.to_path_buf(),
                    source,
                })?;
            temporary
                .persist_noclobber(path)
                .map_err(|error| CoreError::Io {
                    operation: "commit local API token",
                    path: path.to_path_buf(),
                    source: error.error,
                })?;
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|source| CoreError::Io {
                    operation: "sync local API token directory",
                    path: parent.to_path_buf(),
                    source,
                })?;
            Ok(token)
        }
        Err(source) => Err(CoreError::Io {
            operation: "inspect local API token",
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Private readiness record used by an app that owns this node process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NodeReadyInfo {
    /// Readiness contract schema.
    pub schema_version: u16,
    /// Bound loopback HTTP API base URL.
    pub api_base_url: String,
    /// Bound authenticated QUIC peer socket.
    pub peer_address: SocketAddr,
    /// Process identifier that owns this record.
    pub process_id: u32,
}

/// Atomically publishes a mode-0600 app-owned node readiness record.
pub fn write_node_ready_file(path: &Path, info: &NodeReadyInfo) -> Result<(), CoreError> {
    if info.schema_version != 1
        || !info.api_base_url.starts_with("http://127.0.0.1:")
        || info.process_id == 0
    {
        return Err(CoreError::InvalidState(
            "invalid node readiness record".to_owned(),
        ));
    }
    persist_private_file(path, &serde_json::to_vec_pretty(info)?)
}

/// Removes only a readiness record still owned by the calling node process.
pub fn remove_node_ready_file(path: &Path, process_id: u32) -> Result<(), CoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16_384 {
                return Err(CoreError::InvalidState(
                    "invalid node readiness file".to_owned(),
                ));
            }
            let bytes = fs::read(path).map_err(|source| CoreError::Io {
                operation: "read node readiness file",
                path: path.to_path_buf(),
                source,
            })?;
            let info: NodeReadyInfo = serde_json::from_slice(&bytes)?;
            if info.process_id != process_id {
                return Ok(());
            }
            fs::remove_file(path).map_err(|source| CoreError::Io {
                operation: "remove node readiness file",
                path: path.to_path_buf(),
                source,
            })?;
            if let Some(parent) = path.parent() {
                File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|source| CoreError::Io {
                        operation: "sync node readiness directory",
                        path: parent.to_path_buf(),
                        source,
                    })?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(CoreError::Io {
            operation: "inspect node readiness file",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn persist_private_file(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("private state path has no parent".to_owned()))?;
    fs::create_dir_all(parent).map_err(|source| CoreError::Io {
        operation: "create private state directory",
        path: parent.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
            CoreError::Io {
                operation: "protect private state directory",
                path: parent.to_path_buf(),
                source,
            }
        })?;
    }
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "stage private state file",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect private state file",
                path: path.to_path_buf(),
                source,
            })?;
    }
    use std::io::Write as _;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync private state file",
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| CoreError::Io {
        operation: "commit private state file",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync private state directory",
            path: parent.to_path_buf(),
            source,
        })
}

fn create_private_directory(path: PathBuf) -> Result<PathBuf, CoreError> {
    fs::create_dir_all(&path).map_err(|source| CoreError::Io {
        operation: "create private directory",
        path: path.clone(),
        source,
    })?;
    let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
        operation: "inspect private directory",
        path: path.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CoreError::InvalidState(
            "private directory is not a real directory".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            CoreError::Io {
                operation: "protect private directory",
                path: path.clone(),
                source,
            }
        })?;
    }
    fs::canonicalize(&path).map_err(|source| CoreError::Io {
        operation: "canonicalize private directory",
        path,
        source,
    })
}

fn prune_stale_archive_restore_targets(root: &Path) -> Result<(), CoreError> {
    let mut removed = false;
    let entries = fs::read_dir(root).map_err(|source| CoreError::Io {
        operation: "read archive restore staging directory",
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| CoreError::Io {
            operation: "read archive restore staging entry",
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
            operation: "inspect archive restore staging entry",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !entry.file_name().to_str().is_some_and(valid_job_identifier)
        {
            return Err(CoreError::InvalidState(
                "invalid archive restore staging entry".to_owned(),
            ));
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= ARCHIVE_RESTORE_MAX_AGE);
        if stale {
            fs::remove_dir_all(&path).map_err(|source| CoreError::Io {
                operation: "remove stale archive restore staging entry",
                path,
                source,
            })?;
            removed = true;
        }
    }
    if removed {
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| CoreError::Io {
                operation: "sync archive restore staging directory",
                path: root.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

fn prune_stale_private_files(
    root: &Path,
    maximum_age: Duration,
    maximum_size: u64,
    valid_name: fn(&str) -> bool,
) -> Result<(), CoreError> {
    let mut removed = false;
    for entry in fs::read_dir(root).map_err(|source| CoreError::Io {
        operation: "read private state directory",
        path: root.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CoreError::Io {
            operation: "read private state entry",
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| CoreError::Io {
            operation: "inspect private state entry",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > maximum_size
            || !entry.file_name().to_str().is_some_and(valid_name)
        {
            return Err(CoreError::InvalidState(
                "invalid private state entry".to_owned(),
            ));
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= maximum_age);
        if stale {
            fs::remove_file(&path).map_err(|source| CoreError::Io {
                operation: "remove stale private state entry",
                path,
                source,
            })?;
            removed = true;
        }
    }
    if removed {
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| CoreError::Io {
                operation: "sync private state directory",
                path: root.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn javascript() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
}

async fn pairing_flow_javascript() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        PAIRING_FLOW_JS,
    )
}

async fn restore_plan_flow_javascript() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        RESTORE_PLAN_FLOW_JS,
    )
}

async fn not_found(uri: Uri) -> Response {
    if uri.path().starts_with("/api/") {
        ApiError {
            status: StatusCode::NOT_FOUND,
            code: "route_not_found",
            message: "The requested versioned API route does not exist.",
            retryable: false,
            upload_offset: None,
        }
        .into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

async fn method_not_allowed() -> Response {
    ApiError {
        status: StatusCode::METHOD_NOT_ALLOWED,
        code: "method_not_allowed",
        message: "This API route does not support the requested HTTP method.",
        retryable: false,
        upload_offset: None,
    }
    .into_response()
}

struct ContractJson<T>(T);

impl<S, T> FromRequest<S> for ContractJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(request, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(ApiError::from_json_rejection)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    status: &'static str,
    protocol_version: u16,
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(Health {
            status: "ok",
            protocol_version: PROTOCOL_VERSION,
        }),
    )
}

async fn status(State(state): State<AppState>) -> Result<axum::Json<NodeStatus>, ApiError> {
    let config = state.engine.config().map_err(ApiError::from_core)?;
    Ok(axum::Json(NodeStatus {
        device_name: config.device_name,
        protocol_version: PROTOCOL_VERSION,
        lan_discovery: config.lan_discovery_enabled,
        platform_tier: state.platform_tier,
        state: "ready".to_owned(),
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TransportIdentityResponse {
    device_id: DeviceId,
    peer_port: u16,
    certificate_der: String,
    certificate_fingerprint: String,
}

async fn transport_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<TransportIdentityResponse>, ApiError> {
    authorize(&state, &headers)?;
    let certificate = state
        .transport_certificate
        .as_deref()
        .ok_or_else(|| ApiError::internal("transport identity is unavailable"))?;
    Ok(axum::Json(TransportIdentityResponse {
        device_id: state.engine.device_id(),
        peer_port: state
            .peer_address
            .ok_or_else(|| ApiError::internal("peer endpoint is unavailable"))?
            .port(),
        certificate_der: URL_SAFE_NO_PAD.encode(certificate),
        certificate_fingerprint: sha256_hex(certificate),
    }))
}

async fn discovery_candidates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<discovery::DiscoveryCandidate>>, ApiError> {
    authorize(&state, &headers)?;
    let enabled = state
        .engine
        .config()
        .map_err(ApiError::from_core)?
        .lan_discovery_enabled;
    let peer_port = state
        .peer_address
        .ok_or_else(|| ApiError::internal("peer endpoint is unavailable"))?
        .port();
    let candidates = tokio::task::spawn_blocking(move || {
        let mut candidates = discovery::LanDiscovery::browse(enabled, Duration::from_secs(1))?;
        candidates.extend(discovery::discover_tailscale_candidates(peer_port)?);
        candidates.sort_by_key(|candidate| {
            (
                candidate.source,
                candidate.endpoint,
                candidate.service_id.clone(),
            )
        });
        candidates.dedup_by(|left, right| {
            left.source == right.source
                && left.endpoint == right.endpoint
                && left.service_id == right.service_id
        });
        Ok::<_, CoreError>(candidates)
    })
    .await
    .map_err(|_| ApiError::internal("discovery worker failed"))?
    .map_err(ApiError::from_core)?;
    Ok(axum::Json(candidates))
}

async fn config_export(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let bytes = state
        .engine
        .export_settings()
        .map_err(ApiError::from_core)?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigImportRequest {
    confirmed: bool,
    settings: serde_json::Value,
}

async fn config_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ConfigImportRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    let bytes = serde_json::to_vec(&request.settings).map_err(ApiError::from_json)?;
    let previous = state
        .engine
        .export_settings()
        .map_err(ApiError::from_core)?;
    state
        .engine
        .import_settings(&bytes, request.confirmed)
        .map_err(ApiError::from_core)?;
    if let Some(controller) = &state.discovery_controller {
        let enabled = state
            .engine
            .config()
            .map_err(ApiError::from_core)?
            .lan_discovery_enabled;
        if let Err(error) = controller.set_enabled(enabled) {
            state
                .engine
                .import_settings(&previous, true)
                .map_err(ApiError::from_core)?;
            return Err(ApiError::from_core(error));
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairInvitationRequest {
    lifetime_ms: u64,
    #[serde(default)]
    endpoints: Vec<String>,
}

async fn pair_network_pending(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<NetworkPairingItem>>, ApiError> {
    authorize(&state, &headers)?;
    let items = state
        .network_pairing
        .items(now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(items))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairNetworkConfirmRequest {
    displayed_code: String,
}

async fn pair_network_confirm(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(pairing_id): AxumPath<String>,
    ContractJson(request): ContractJson<PairNetworkConfirmRequest>,
) -> Result<axum::Json<NetworkPairingItem>, ApiError> {
    authorize(&state, &headers)?;
    state
        .network_pairing
        .confirm_local(&pairing_id, &request.displayed_code, now_unix_ms())
        .map_err(ApiError::from_core)?;
    let item = state
        .network_pairing
        .item(&pairing_id, now_unix_ms())
        .map_err(ApiError::from_core)?;
    state
        .connect_completed_network_pairing(&item)
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(item))
}

async fn pair_network_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(pairing_id): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    state
        .network_pairing
        .remove(&pairing_id, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn pair_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairInvitationRequest>,
) -> Result<axum::Json<PairingInvitation>, ApiError> {
    authorize(&state, &headers)?;
    let binding = state
        .local_transport_binding()
        .map_err(ApiError::from_core)?;
    if request
        .endpoints
        .iter()
        .any(|endpoint| endpoint != &binding.address)
    {
        return Err(ApiError::bad_request(
            "pairing_endpoint_mismatch",
            "Pairing endpoints must match the node's signed transport address.",
        ));
    }
    let invitation = state
        .engine
        .pairing_manager()
        .create_invitation_with_transport(
            now_unix_ms(),
            request.lifetime_ms,
            vec![binding.address.clone()],
            binding,
        )
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(invitation))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairAcceptRequest {
    invitation: PairingInvitation,
    responder_name: String,
    #[serde(default)]
    responder_roles: BTreeSet<PeerRole>,
    #[serde(default)]
    inviter_roles: BTreeSet<PeerRole>,
}

async fn pair_accept(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairAcceptRequest>,
) -> Result<axum::Json<PairingSession>, ApiError> {
    authorize(&state, &headers)?;
    let binding = state
        .local_transport_binding()
        .map_err(ApiError::from_core)?;
    let session = state
        .engine
        .accept_pairing_with_transport(
            request.invitation,
            TransportBinding {
                display_name: request.responder_name,
                ..binding
            },
            request.responder_roles,
            request.inviter_roles,
            now_unix_ms(),
        )
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(session))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairConfirmRequest {
    session: PairingSession,
    displayed_code: String,
}

async fn pair_confirm_responder(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(mut request): ContractJson<PairConfirmRequest>,
) -> Result<axum::Json<PairingSession>, ApiError> {
    authorize(&state, &headers)?;
    state
        .engine
        .confirm_pairing_as_responder(&mut request.session, &request.displayed_code, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(request.session))
}

async fn pair_confirm_inviter(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(mut request): ContractJson<PairConfirmRequest>,
) -> Result<axum::Json<PairingSession>, ApiError> {
    authorize(&state, &headers)?;
    state
        .engine
        .confirm_pairing_as_inviter(&mut request.session, &request.displayed_code, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(request.session))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairFinalizeRequest {
    session: PairingSession,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PairFinalizeResponse {
    inviter_grant: covalent_protocol::PeerGrant,
    responder_grant: covalent_protocol::PeerGrant,
    peer_transport: Option<TransportBinding>,
}

fn finalization_response(
    confirmation: PairingConfirmation,
    local_is_inviter: bool,
) -> PairFinalizeResponse {
    let peer_grant = if local_is_inviter {
        &confirmation.responder_grant
    } else {
        &confirmation.inviter_grant
    };
    let peer_transport = if peer_grant.roles.contains(&PeerRole::StorageProvider) {
        if local_is_inviter {
            confirmation.responder_transport.clone()
        } else {
            confirmation.inviter_transport.clone()
        }
    } else {
        None
    };
    PairFinalizeResponse {
        inviter_grant: confirmation.inviter_grant,
        responder_grant: confirmation.responder_grant,
        peer_transport,
    }
}

async fn pair_finalize_responder(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairFinalizeRequest>,
) -> Result<axum::Json<PairFinalizeResponse>, ApiError> {
    authorize(&state, &headers)?;
    let confirmation = state
        .engine
        .finalize_pairing_as_responder(&request.session, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(finalization_response(confirmation, false)))
}

async fn pair_finalize_inviter(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairFinalizeRequest>,
) -> Result<axum::Json<PairFinalizeResponse>, ApiError> {
    authorize(&state, &headers)?;
    let confirmation = state
        .engine
        .finalize_pairing_as_inviter(&request.session, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(finalization_response(confirmation, true)))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JobAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobControlRequest {
    job_id: String,
    action: JobAction,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct JobControlResponse {
    job_id: String,
    state: &'static str,
    active: bool,
    last_touched_unix_ms: u64,
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<JobControlResponse>>, ApiError> {
    authorize(&state, &headers)?;
    state.prune_expired_jobs()?;
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
    let entries = jobs
        .entries
        .iter()
        .map(|(job_id, entry)| JobControlResponse {
            job_id: job_id.clone(),
            state: job_state_name(entry.control.state()),
            active: entry.active,
            last_touched_unix_ms: unix_ms(entry.last_touched),
        })
        .collect();
    Ok(axum::Json(entries))
}

async fn job_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<JobControlRequest>,
) -> Result<axum::Json<JobControlResponse>, ApiError> {
    authorize(&state, &headers)?;
    state.prune_expired_jobs()?;
    let (control, active, last_touched) = {
        let mut jobs = state
            .jobs
            .lock()
            .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
        let entry = jobs.entries.get_mut(&request.job_id).ok_or_else(|| {
            ApiError::not_found("job_not_found", "The requested job is not registered.")
        })?;
        entry.last_touched = SystemTime::now();
        (entry.control.clone(), entry.active, entry.last_touched)
    };
    match request.action {
        JobAction::Pause => control.pause(),
        JobAction::Resume => control.resume(),
        JobAction::Cancel => control.cancel(),
    }
    if matches!(request.action, JobAction::Cancel) && !active {
        state
            .jobs
            .lock()
            .map_err(|_| ApiError::from_core(CoreError::Synchronization))?
            .entries
            .remove(&request.job_id);
        discard_job_artifacts(&state, &request.job_id)?;
    }
    let state_name = job_state_name(control.state());
    Ok(axum::Json(JobControlResponse {
        job_id: request.job_id,
        state: state_name,
        active,
        last_touched_unix_ms: unix_ms(last_touched),
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct JobDiscardRequest {
    job_id: String,
}

async fn discard_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<JobDiscardRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    if !valid_job_identifier(&request.job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The job ID is invalid.",
        ));
    }
    {
        let mut jobs = state
            .jobs
            .lock()
            .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
        if jobs
            .entries
            .get(&request.job_id)
            .is_some_and(|entry| entry.active)
        {
            return Err(ApiError::conflict(
                "job_active",
                "Cancel the active job before discarding it.",
            ));
        }
        jobs.entries.remove(&request.job_id);
    }
    discard_job_artifacts(&state, &request.job_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn acknowledge_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<JobDiscardRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    if !valid_job_identifier(&request.job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The job ID is invalid.",
        ));
    }
    if state
        .jobs
        .lock()
        .map_err(|_| ApiError::from_core(CoreError::Synchronization))?
        .entries
        .get(&request.job_id)
        .is_some_and(|entry| entry.active)
    {
        return Err(ApiError::conflict(
            "job_active",
            "An active job cannot be acknowledged.",
        ));
    }
    let backup_completed = state
        .archive_backup_root
        .join(&request.job_id)
        .join("result.json")
        .is_file();
    let restore_completed = state
        .archive_restore_root
        .join(&request.job_id)
        .join("result.json")
        .is_file();
    if !backup_completed && !restore_completed {
        return Err(ApiError::conflict(
            "job_not_complete",
            "Only a retained completed archive job can be acknowledged.",
        ));
    }
    state
        .jobs
        .lock()
        .map_err(|_| ApiError::from_core(CoreError::Synchronization))?
        .entries
        .remove(&request.job_id);
    discard_job_artifacts(&state, &request.job_id)?;
    Ok(StatusCode::NO_CONTENT)
}

fn job_state_name(state: JobState) -> &'static str {
    match state {
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Cancelled => "cancelled",
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RevokePeerRequest {
    peer_id: DeviceId,
}

async fn revoke_peer(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RevokePeerRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    state
        .engine
        .revoke_peer(request.peer_id)
        .map_err(ApiError::from_core)?;
    state
        .disconnect_provider(request.peer_id)
        .map_err(ApiError::from_core)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectProviderRequest {
    peer_transport: TransportBinding,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderConnectionResponse {
    peer_id: DeviceId,
    address: SocketAddr,
    certificate_fingerprint: String,
}

async fn connect_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ConnectProviderRequest>,
) -> Result<axum::Json<ProviderConnectionResponse>, ApiError> {
    authorize(&state, &headers)?;
    let trusted = state
        .engine
        .trusted_peer_transport(request.peer_transport.peer_id, PeerRole::StorageProvider)
        .map_err(ApiError::from_core)?;
    if request.peer_transport != trusted {
        return Err(ApiError::conflict(
            "provider_binding_mismatch",
            "Provider transport does not match the mutually signed pairing binding.",
        ));
    }
    let certificate = URL_SAFE_NO_PAD
        .decode(&trusted.certificate_der)
        .map_err(|_| {
            ApiError::bad_request("invalid_certificate", "Certificate encoding is invalid.")
        })?;
    if certificate.is_empty() || certificate.len() > 64 * 1_024 {
        return Err(ApiError::bad_request(
            "invalid_certificate",
            "Certificate size is invalid.",
        ));
    }
    let fingerprint = sha256_hex(&certificate);
    if !valid_lowercase_digest(&trusted.certificate_fingerprint)
        || fingerprint != trusted.certificate_fingerprint
    {
        return Err(ApiError::conflict(
            "provider_binding_mismatch",
            "Provider certificate does not match the mutually signed pairing pin.",
        ));
    }
    let address = trusted.address.parse::<SocketAddr>().map_err(|_| {
        ApiError::bad_request(
            "invalid_provider_address",
            "Signed provider address is not a valid socket address.",
        )
    })?;
    let response = ProviderConnectionResponse {
        peer_id: trusted.peer_id,
        address,
        certificate_fingerprint: fingerprint,
    };
    state
        .connect_provider(ProviderConnection {
            peer_id: trusted.peer_id,
            address,
            certificate_der: trusted.certificate_der,
        })
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(response))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DisconnectProviderRequest {
    peer_id: DeviceId,
}

async fn disconnect_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<DisconnectProviderRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers)?;
    state
        .disconnect_provider(request.peer_id)
        .map_err(ApiError::from_core)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<ProviderConnectionResponse>>, ApiError> {
    authorize(&state, &headers)?;
    let configs = state
        .provider_connections
        .lock()
        .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
    let mut providers = Vec::with_capacity(configs.len());
    for config in configs.values() {
        let certificate = URL_SAFE_NO_PAD
            .decode(&config.certificate_der)
            .map_err(|_| ApiError::internal("stored provider certificate is invalid"))?;
        providers.push(ProviderConnectionResponse {
            peer_id: config.peer_id,
            address: config.address,
            certificate_fingerprint: sha256_hex(&certificate),
        });
    }
    Ok(axum::Json(providers))
}

async fn current_roster(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Option<SignedRoster>>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        state.engine.current_roster().map_err(ApiError::from_core)?,
    ))
}

async fn accept_roster(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(roster): ContractJson<SignedRoster>,
) -> Result<axum::Json<RosterCursor>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        state
            .engine
            .accept_peer_roster(roster)
            .map_err(ApiError::from_core)?,
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BackupRequest {
    source_root: PathBuf,
    backup_id: Option<BackupId>,
    display_name: String,
    snapshot_id: String,
    job_id: String,
    #[serde(default)]
    selected_provider_ids: Vec<DeviceId>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct BackupResponse {
    backup_id: BackupId,
    snapshot_id: String,
    entries: usize,
    bytes_read: u64,
    chunks_stored: usize,
    chunks_deduplicated: usize,
    selected_providers: usize,
    degraded_failures: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveBackupMetadata {
    protocol_version: u16,
    backup_id: Option<BackupId>,
    display_name: String,
    snapshot_id: String,
    job_id: String,
    #[serde(default)]
    selected_provider_ids: Vec<DeviceId>,
}

impl ArchiveBackupMetadata {
    fn with_source_root(self, source_root: PathBuf) -> BackupRequest {
        BackupRequest {
            source_root,
            backup_id: self.backup_id,
            display_name: self.display_name,
            snapshot_id: self.snapshot_id,
            job_id: self.job_id,
            selected_provider_ids: self.selected_provider_ids,
        }
    }
}

async fn list_backups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<Vec<BackupSummary>>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        state.engine.list_backups().map_err(ApiError::from_core)?,
    ))
}

async fn backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<BackupRequest>,
) -> Result<axum::Json<BackupResponse>, ApiError> {
    authorize(&state, &headers)?;
    let admission = state.admit_engine_job()?;
    let engine = Arc::clone(&state.engine);
    let job_id = request.job_id.clone();
    let mut lease = state.start_job(&job_id)?;
    let control = lease.control();
    let result = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let backup_id = request.backup_id.unwrap_or_default();
        let mut options = BackupOptions::new(backup_id, request.snapshot_id, request.job_id);
        options.display_name = request.display_name;
        options.created_at_unix_ms = now_unix_ms();
        options.replica_intent = ReplicaIntent::explicit(request.selected_provider_ids);
        engine
            .backup(request.source_root, &options, &control, |_| {})
            .map(|result| (backup_id, result))
    })
    .await
    .map_err(|_| ApiError::internal("backup worker failed"))?;
    match &result {
        Ok(_) => lease.finish().map_err(ApiError::from_core)?,
        Err(CoreError::Cancelled) => {
            lease.finish().map_err(ApiError::from_core)?;
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
        }
        Err(CoreError::Paused) => lease.preserve_for_resume().map_err(ApiError::from_core)?,
        Err(_) => {
            lease.finish().map_err(ApiError::from_core)?;
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
        }
    }
    let result = result.map_err(ApiError::from_core)?;
    Ok(axum::Json(BackupResponse {
        backup_id: result.0,
        snapshot_id: result.1.manifest.snapshot_id.clone(),
        entries: result.1.manifest.entries.len(),
        bytes_read: result.1.progress.bytes_read,
        chunks_stored: result.1.progress.chunks_stored,
        chunks_deduplicated: result.1.progress.chunks_deduplicated,
        selected_providers: result.1.manifest.replica_intent.selected_providers.len(),
        degraded_failures: result.1.replication.failures.len(),
    }))
}

async fn backup_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    require_archive_content_type(&headers)?;
    let metadata: ArchiveBackupMetadata = decode_archive_metadata(&headers)?;
    if metadata.protocol_version != PROTOCOL_VERSION {
        return Err(ApiError::conflict(
            "protocol_incompatible",
            "Archive metadata uses an unsupported protocol version.",
        ));
    }
    if !valid_job_identifier(&metadata.job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The backup job ID is invalid.",
        ));
    }
    let admission = state.admit_engine_job()?;
    let (job_directory, completed, existed) = prepare_archive_backup_job(&state, &metadata)?;
    if let Some(completed) = completed {
        return Ok(acknowledgement_required_json(completed));
    }
    let job_id = metadata.job_id.clone();
    let mut lease = match state.start_job(&job_id) {
        Ok(lease) => lease,
        Err(error) => {
            if !existed {
                let _ = remove_private_job_directory(
                    state.archive_backup_root.as_path(),
                    &job_directory,
                );
            }
            return Err(error);
        }
    };
    let control = lease.control();
    let source_root = job_directory.join("source");
    let metadata_digest =
        blake3::hash(&serde_json::to_vec(&metadata).map_err(ApiError::from_json)?)
            .to_hex()
            .to_string();
    let prepared_source = prepared_archive_source(&job_directory, &metadata_digest)?;
    let archive_path = if prepared_source.is_some() {
        None
    } else {
        match receive_archive(body, &headers, &state, &job_directory, &metadata_digest).await {
            Ok(path) => Some(path),
            Err(error) => {
                if job_directory.join("upload-session.json").is_file() {
                    lease.preserve_for_resume().map_err(ApiError::from_core)?;
                } else {
                    if !existed {
                        let _ = remove_private_job_directory(
                            state.archive_backup_root.as_path(),
                            &job_directory,
                        );
                    }
                    lease.finish().map_err(ApiError::from_core)?;
                }
                return Err(error);
            }
        }
    };
    let request = metadata.with_source_root(source_root.clone());
    let worker_job_directory = job_directory.clone();
    let worker_state = state.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        if prepared_source.is_none() {
            if source_root.exists() {
                fs::remove_dir_all(&source_root)
                    .map_err(|_| ApiError::internal("incomplete archive source cleanup failed"))?;
            }
            let archive_path = archive_path
                .as_deref()
                .ok_or_else(|| ApiError::internal("archive upload disappeared"))?;
            extract_backup_archive(archive_path, &source_root, &worker_state, &control)?;
            let upload_digest = archive_path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("upload-"))
                .and_then(|name| name.strip_suffix(".zip"))
                .filter(|digest| valid_lowercase_digest(digest))
                .ok_or_else(|| ApiError::internal("archive upload identity is invalid"))?;
            persist_private_file(
                &worker_job_directory.join("source-ready.json"),
                &serde_json::to_vec(&ArchivePreparedSource {
                    schema_version: 1,
                    metadata_digest: metadata_digest.clone(),
                    upload_digest: upload_digest.to_owned(),
                })
                .map_err(ApiError::from_json)?,
            )
            .map_err(ApiError::from_core)?;
            fs::remove_file(archive_path)
                .map_err(|_| ApiError::internal("consumed archive upload cleanup failed"))?;
            let _ = fs::remove_file(worker_job_directory.join("upload-session.json"));
            File::open(&worker_job_directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ApiError::internal("archive source commit could not be synced"))?;
        }
        let source_bytes = archive_tree_bytes(&source_root)?;
        let _backup_disk_reservation = worker_state.reserve_archive_staging(source_bytes)?;
        let backup_id = request.backup_id.unwrap_or_default();
        let mut options = BackupOptions::new(backup_id, request.snapshot_id, request.job_id);
        options.display_name = request.display_name;
        options.created_at_unix_ms = now_unix_ms();
        options.replica_intent = ReplicaIntent::explicit(request.selected_provider_ids);
        let result = worker_state
            .engine
            .backup(request.source_root, &options, &control, |_| {})
            .map_err(ApiError::from_core)?;
        let response = BackupResponse {
            backup_id,
            snapshot_id: result.manifest.snapshot_id.clone(),
            entries: result.manifest.entries.len(),
            bytes_read: result.progress.bytes_read,
            chunks_stored: result.progress.chunks_stored,
            chunks_deduplicated: result.progress.chunks_deduplicated,
            selected_providers: result.manifest.replica_intent.selected_providers.len(),
            degraded_failures: result.replication.failures.len(),
        };
        let response_bytes = serde_json::to_vec(&response).map_err(ApiError::from_json)?;
        let retained_bytes = u64::try_from(response_bytes.len())
            .ok()
            .and_then(|bytes| {
                bytes.checked_add(
                    fs::metadata(worker_job_directory.join("metadata.json"))
                        .ok()?
                        .len(),
                )
            })
            .ok_or_else(|| ApiError::payload_too_large("Archive result size overflowed."))?;
        ensure_retained_archive_capacity(&worker_state, 1, retained_bytes)?;
        persist_private_file(&worker_job_directory.join("result.json"), &response_bytes)
            .map_err(ApiError::from_core)?;
        compact_completed_backup_job(&worker_job_directory);
        Ok::<_, ApiError>(response)
    })
    .await
    .map_err(|_| ApiError::internal("archive backup worker failed"))?;
    match &result {
        Ok(_) => lease.finish().map_err(ApiError::from_core)?,
        Err(error) if error.code == "job_cancelled" => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
        Err(error) if error.code == "job_paused" => {
            lease.preserve_for_resume().map_err(ApiError::from_core)?;
        }
        Err(_) => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
    }
    let response = result?;
    Ok(acknowledgement_required_json(response))
}

fn acknowledgement_required_json<T: Serialize>(value: T) -> Response {
    let mut response = axum::Json(value).into_response();
    response
        .headers_mut()
        .insert(JOB_ACK_REQUIRED_HEADER, HeaderValue::from_static("true"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SnapshotRequest {
    backup_id: BackupId,
    snapshot_id: String,
    #[serde(default)]
    verify_providers: bool,
    #[serde(default)]
    repair: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyResponse {
    verified: usize,
    missing: Vec<String>,
    corrupt: Vec<String>,
    intact: bool,
    provider_availability: BTreeMap<DeviceId, ReplicaAvailability>,
}

async fn verify_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<SnapshotRequest>,
) -> Result<axum::Json<VerifyResponse>, ApiError> {
    authorize(&state, &headers)?;
    let admission = state.admit_engine_job()?;
    let engine = Arc::clone(&state.engine);
    let report = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        if request.repair {
            engine.repair_snapshot(request.backup_id, &request.snapshot_id)?;
        }
        if request.verify_providers {
            let availability =
                engine.verify_snapshot_availability(request.backup_id, &request.snapshot_id)?;
            Ok((availability.local, availability.providers))
        } else {
            engine
                .verify_snapshot(request.backup_id, &request.snapshot_id)
                .map(|local| (local, BTreeMap::new()))
        }
    })
    .await
    .map_err(|_| ApiError::internal("verify worker failed"))?
    .map_err(ApiError::from_core)?;
    Ok(axum::Json(VerifyResponse {
        verified: report.0.verified,
        missing: report.0.missing.clone(),
        corrupt: report.0.corrupt.clone(),
        intact: report.0.is_intact(),
        provider_availability: report.1,
    }))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestorePreviewRequest {
    backup_id: BackupId,
    snapshot_id: String,
    target_root: PathBuf,
    conflict_policy: ConflictPolicy,
    job_id: String,
}

async fn restore_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestorePreviewRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let admission = state.admit_engine_job()?;
    let options = RestoreOptions {
        conflict_policy: request.conflict_policy,
        selected_paths: Default::default(),
        job_id: request.job_id,
        target_inventory: None,
    };
    let engine = Arc::clone(&state.engine);
    let worker_state = state.clone();
    let plan = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let plan = engine
            .preview_restore(
                request.backup_id,
                &request.snapshot_id,
                request.target_root,
                &options,
            )
            .map_err(ApiError::from_core)?;
        persist_restore_plan(&worker_state, &plan)?;
        Ok::<_, ApiError>(plan)
    })
    .await
    .map_err(|_| ApiError::internal("restore preview worker failed"))??;
    persisted_plan_response(&state, plan)
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetInventoryUploadSession {
    schema_version: u16,
    inventory_id: String,
    job_id: String,
    root_identity: String,
    entry_count: u64,
    total_bytes: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BeginTargetInventoryRequest {
    job_id: String,
    schema_version: u16,
    root_identity: String,
    entry_count: u64,
    total_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetInventoryUploadResponse {
    inventory_id: String,
    job_id: String,
    next_offset: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TargetInventoryPageRequest {
    job_id: String,
    offset: u64,
    page_digest: String,
    entries: Vec<TargetInventoryEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalizeTargetInventoryRequest {
    job_id: String,
    entry_count: u64,
    total_bytes: u64,
    #[serde(default)]
    inventory_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TargetInventoryReference {
    inventory_id: String,
    job_id: String,
    schema_version: u16,
    root_identity: String,
    entry_count: u64,
    total_bytes: u64,
    inventory_digest: String,
}

async fn begin_target_inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<BeginTargetInventoryRequest>,
) -> Result<axum::Json<TargetInventoryUploadResponse>, ApiError> {
    authorize(&state, &headers)?;
    if !valid_job_identifier(&request.job_id)
        || request.schema_version != TARGET_INVENTORY_SCHEMA_VERSION
        || request.root_identity.trim().is_empty()
        || request.root_identity.len() > 512
        || request.root_identity.chars().any(char::is_control)
        || request.entry_count > MAX_TARGET_INVENTORY_ENTRIES
    {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Target inventory metadata is invalid or exceeds its bounded contract.",
        ));
    }
    let job_directory = create_or_open_archive_restore_job(&state, &request.job_id)?;
    let directory = SafeUploadDirectory::open(&job_directory)?;
    let mut random = [0_u8; 32];
    OsRng.fill_bytes(&mut random);
    let inventory_id = lowercase_hex(&random);
    let session = TargetInventoryUploadSession {
        schema_version: TARGET_INVENTORY_SCHEMA_VERSION,
        inventory_id: inventory_id.clone(),
        job_id: request.job_id.clone(),
        root_identity: request.root_identity,
        entry_count: request.entry_count,
        total_bytes: request.total_bytes,
    };
    directory.create_private_file(
        &target_inventory_session_name(&inventory_id),
        &serde_json::to_vec(&session).map_err(ApiError::from_json)?,
    )?;
    Ok(axum::Json(TargetInventoryUploadResponse {
        inventory_id,
        job_id: request.job_id,
        next_offset: 0,
    }))
}

async fn append_target_inventory_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(inventory_id): AxumPath<String>,
    ContractJson(request): ContractJson<TargetInventoryPageRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if !valid_lowercase_digest(&inventory_id) || !valid_job_identifier(&request.job_id) {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Target inventory upload identity is invalid.",
        ));
    }
    if request.entries.is_empty()
        || request.entries.len() as u64 > MAX_TARGET_INVENTORY_ENTRIES
        || !valid_lowercase_digest(&request.page_digest)
        || target_inventory_page_digest(&request.entries) != request.page_digest
    {
        return Err(ApiError::unprocessable(
            "target_inventory_page_mismatch",
            "Target inventory page content does not match its declared digest.",
        ));
    }
    let job_directory = existing_archive_restore_job(&state, &request.job_id)?;
    let directory = SafeUploadDirectory::open(&job_directory)?;
    let session = load_target_inventory_session(&directory, &inventory_id)?;
    if session.job_id != request.job_id {
        return Err(ApiError::conflict(
            "target_inventory_job_mismatch",
            "Target inventory upload is bound to a different restore job.",
        ));
    }
    let entries = load_target_inventory_entries(&directory, &inventory_id)?;
    let next_offset = entries.len() as u64;
    if request.offset != next_offset {
        if request.offset < next_offset {
            let start = usize::try_from(request.offset).unwrap_or(usize::MAX);
            let end = start.saturating_add(request.entries.len());
            if end <= entries.len() && entries[start..end] == request.entries {
                return Ok(inventory_upload_response(
                    &inventory_id,
                    &request.job_id,
                    next_offset,
                ));
            }
        }
        return Ok(inventory_offset_conflict(next_offset));
    }
    validate_target_inventory_page(&session, &entries, &request.entries)?;
    let serialized = serde_json::to_vec(&request.entries).map_err(ApiError::from_json)?;
    let current_length = directory.private_file_length(
        &target_inventory_entries_name(&inventory_id),
        MAX_TARGET_INVENTORY_STAGING_BYTES,
    )?;
    if current_length
        .saturating_add(serialized.len() as u64)
        .saturating_add(1)
        > MAX_TARGET_INVENTORY_STAGING_BYTES
    {
        return Err(ApiError::payload_too_large(
            "Target inventory staging exceeds its bounded byte limit.",
        ));
    }
    let _reservation = state.reserve_archive_staging(serialized.len() as u64 + 1)?;
    let mut file = directory.open_partial_append(&target_inventory_entries_name(&inventory_id))?;
    use std::io::Write as _;
    file.write_all(&serialized)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|_| ApiError::internal("target inventory page could not be persisted"))?;
    directory.sync()?;
    Ok(inventory_upload_response(
        &inventory_id,
        &request.job_id,
        next_offset + request.entries.len() as u64,
    ))
}

async fn finalize_target_inventory(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(inventory_id): AxumPath<String>,
    ContractJson(request): ContractJson<FinalizeTargetInventoryRequest>,
) -> Result<axum::Json<TargetInventoryReference>, ApiError> {
    authorize(&state, &headers)?;
    if !valid_lowercase_digest(&inventory_id) || !valid_job_identifier(&request.job_id) {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Target inventory upload identity is invalid.",
        ));
    }
    let job_directory = existing_archive_restore_job(&state, &request.job_id)?;
    let directory = SafeUploadDirectory::open(&job_directory)?;
    let final_name = target_inventory_final_name(&inventory_id);
    if let Some(bytes) =
        directory.read_private_file(&final_name, MAX_TARGET_INVENTORY_STAGING_BYTES)?
    {
        let inventory: TargetInventory =
            serde_json::from_slice(&bytes).map_err(ApiError::from_json)?;
        return Ok(axum::Json(target_inventory_reference(
            &inventory_id,
            &request.job_id,
            &inventory,
        )));
    }
    let session = load_target_inventory_session(&directory, &inventory_id)?;
    if session.job_id != request.job_id
        || session.entry_count != request.entry_count
        || session.total_bytes != request.total_bytes
    {
        return Err(ApiError::conflict(
            "target_inventory_job_mismatch",
            "Target inventory finalization does not match its immutable upload metadata.",
        ));
    }
    let entries = load_target_inventory_entries(&directory, &inventory_id)?;
    if entries.len() as u64 != session.entry_count {
        return Err(ApiError::conflict(
            "target_inventory_incomplete",
            "Target inventory pages are incomplete.",
        ));
    }
    validate_target_inventory_page(&session, &[], &entries)?;
    let digest = canonical_target_inventory_digest(&session.root_identity, &entries)
        .map_err(ApiError::from_core)?;
    if !request.inventory_digest.is_empty() && request.inventory_digest != digest {
        directory.remove(&target_inventory_session_name(&inventory_id));
        directory.remove(&target_inventory_entries_name(&inventory_id));
        return Err(ApiError::unprocessable(
            "target_inventory_digest_mismatch",
            "Target inventory content does not match its final declared digest.",
        ));
    }
    let inventory = TargetInventory {
        schema_version: TARGET_INVENTORY_SCHEMA_VERSION,
        root_identity: session.root_identity,
        entry_count: session.entry_count,
        total_bytes: session.total_bytes,
        inventory_digest: digest,
        entries,
    };
    directory.create_private_file(
        &final_name,
        &serde_json::to_vec(&inventory).map_err(ApiError::from_json)?,
    )?;
    directory.remove(&target_inventory_session_name(&inventory_id));
    directory.remove(&target_inventory_entries_name(&inventory_id));
    Ok(axum::Json(target_inventory_reference(
        &inventory_id,
        &request.job_id,
        &inventory,
    )))
}

fn target_inventory_page_digest(entries: &[TargetInventoryEntry]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"covalent/target-inventory-page/v1");
    hasher.update((entries.len() as u64).to_be_bytes());
    for entry in entries {
        let path = entry.path.as_str().as_bytes();
        hasher.update((path.len() as u64).to_be_bytes());
        hasher.update(path);
        hasher.update([match entry.kind {
            EntryKind::File => 1,
            EntryKind::Directory => 2,
        }]);
        hasher.update(entry.length.to_be_bytes());
        match entry.modified_at_unix_ms {
            Some(value) => {
                hasher.update([1]);
                hasher.update(value.to_be_bytes());
            }
            None => hasher.update([0]),
        }
        let identity = entry.identity_token.as_bytes();
        hasher.update((identity.len() as u64).to_be_bytes());
        hasher.update(identity);
    }
    lowercase_hex(&hasher.finalize())
}

fn validate_target_inventory_page(
    session: &TargetInventoryUploadSession,
    previous: &[TargetInventoryEntry],
    page: &[TargetInventoryEntry],
) -> Result<(), ApiError> {
    let mut prior = previous.last().map(|entry| &entry.path);
    let previous_total = previous
        .iter()
        .try_fold(0_u64, |total, entry| {
            total.checked_add(if entry.kind == EntryKind::File {
                entry.length
            } else {
                0
            })
        })
        .ok_or_else(|| ApiError::payload_too_large("Target inventory byte count overflowed."))?;
    let mut total = previous_total;
    for entry in page {
        if prior.is_some_and(|path| path >= &entry.path)
            || entry.identity_token.trim().is_empty()
            || entry.identity_token.len() > 512
            || entry.identity_token.chars().any(char::is_control)
            || (entry.kind == EntryKind::Directory && entry.length != 0)
        {
            return Err(ApiError::bad_request(
                "invalid_target_inventory",
                "Target inventory entries must be canonical, sorted, unique, and bounded.",
            ));
        }
        if entry.kind == EntryKind::File {
            total = total.checked_add(entry.length).ok_or_else(|| {
                ApiError::payload_too_large("Target inventory byte count overflowed.")
            })?;
        }
        prior = Some(&entry.path);
    }
    if previous.len().saturating_add(page.len()) as u64 > session.entry_count
        || total > session.total_bytes
        || (previous.len().saturating_add(page.len()) as u64 == session.entry_count
            && total != session.total_bytes)
    {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Target inventory counts do not match immutable upload metadata.",
        ));
    }
    Ok(())
}

fn load_target_inventory_entries(
    directory: &SafeUploadDirectory,
    inventory_id: &str,
) -> Result<Vec<TargetInventoryEntry>, ApiError> {
    use std::io::BufRead as _;

    let name = target_inventory_entries_name(inventory_id);
    if directory.private_file_length(&name, MAX_TARGET_INVENTORY_STAGING_BYTES)? == 0 {
        return Ok(Vec::new());
    }
    let file = directory.open_private_reader(&name, MAX_TARGET_INVENTORY_STAGING_BYTES)?;
    let mut entries = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line =
            line.map_err(|_| ApiError::internal("target inventory page could not be read"))?;
        let page: Vec<TargetInventoryEntry> =
            serde_json::from_str(&line).map_err(ApiError::from_json)?;
        entries.extend(page);
        if entries.len() as u64 > MAX_TARGET_INVENTORY_ENTRIES {
            return Err(ApiError::payload_too_large(
                "Target inventory exceeds its entry limit.",
            ));
        }
    }
    Ok(entries)
}

fn load_target_inventory_session(
    directory: &SafeUploadDirectory,
    inventory_id: &str,
) -> Result<TargetInventoryUploadSession, ApiError> {
    let bytes = directory
        .read_private_file(
            &target_inventory_session_name(inventory_id),
            MAX_ARCHIVE_METADATA_BYTES as u64,
        )?
        .ok_or_else(|| {
            ApiError::not_found(
                "target_inventory_not_found",
                "Target inventory upload is unavailable or already finalized.",
            )
        })?;
    let session: TargetInventoryUploadSession =
        serde_json::from_slice(&bytes).map_err(ApiError::from_json)?;
    if session.schema_version != TARGET_INVENTORY_SCHEMA_VERSION
        || session.inventory_id != inventory_id
    {
        return Err(ApiError::internal(
            "target inventory upload state is invalid",
        ));
    }
    Ok(session)
}

fn target_inventory_session_name(inventory_id: &str) -> String {
    format!("inventory-{inventory_id}.session.json")
}

fn target_inventory_entries_name(inventory_id: &str) -> String {
    format!("inventory-{inventory_id}.pages")
}

fn target_inventory_final_name(inventory_id: &str) -> String {
    format!("inventory-{inventory_id}.json")
}

fn target_inventory_reference(
    inventory_id: &str,
    job_id: &str,
    inventory: &TargetInventory,
) -> TargetInventoryReference {
    TargetInventoryReference {
        inventory_id: inventory_id.to_owned(),
        job_id: job_id.to_owned(),
        schema_version: inventory.schema_version,
        root_identity: inventory.root_identity.clone(),
        entry_count: inventory.entry_count,
        total_bytes: inventory.total_bytes,
        inventory_digest: inventory.inventory_digest.clone(),
    }
}

fn inventory_upload_response(inventory_id: &str, job_id: &str, next_offset: u64) -> Response {
    axum::Json(TargetInventoryUploadResponse {
        inventory_id: inventory_id.to_owned(),
        job_id: job_id.to_owned(),
        next_offset,
    })
    .into_response()
}

fn inventory_offset_conflict(next_offset: u64) -> Response {
    let mut response = ApiError::conflict(
        "target_inventory_offset_mismatch",
        "Target inventory page offset does not match the durable server offset.",
    )
    .into_response();
    if let Ok(value) = HeaderValue::from_str(&next_offset.to_string()) {
        response
            .headers_mut()
            .insert(TARGET_INVENTORY_OFFSET_HEADER, value);
    }
    response
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreArchivePreviewRequest {
    backup_id: BackupId,
    snapshot_id: String,
    conflict_policy: ConflictPolicy,
    job_id: String,
    #[serde(default)]
    target_inventory: Option<TargetInventory>,
    #[serde(default)]
    target_inventory_id: Option<String>,
}

async fn restore_archive_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(mut request): ContractJson<RestoreArchivePreviewRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if request.target_inventory.is_some() && request.target_inventory_id.is_some() {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Use either an inline target inventory or one finalized inventory reference.",
        ));
    }
    if let Some(inventory_id) = request.target_inventory_id.as_deref() {
        request.target_inventory = Some(load_finalized_target_inventory(
            &state,
            &request.job_id,
            inventory_id,
        )?);
    }
    if let Some(inventory) = &mut request.target_inventory {
        let canonical_digest =
            canonical_target_inventory_digest(&inventory.root_identity, &inventory.entries)
                .map_err(ApiError::from_core)?;
        if !inventory.inventory_digest.is_empty() && inventory.inventory_digest != canonical_digest
        {
            return Err(ApiError::unprocessable(
                "target_inventory_digest_mismatch",
                "Target inventory content does not match its declared digest.",
            ));
        }
        inventory.inventory_digest = canonical_digest;
    }
    if request.conflict_policy != ConflictPolicy::Fail && request.target_inventory.is_none() {
        return Err(ApiError::bad_request(
            "target_inventory_required",
            "Skip, replace, and rename restores require a bounded client target inventory.",
        ));
    }
    if let Some(plan) = find_restore_plan_by_job(&state, &request.job_id)? {
        let requested_inventory = request.target_inventory.as_ref().map(|inventory| {
            (
                inventory.root_identity.as_str(),
                inventory.entry_count,
                inventory.total_bytes,
                inventory.inventory_digest.as_str(),
            )
        });
        let persisted_inventory = plan.target_inventory.as_ref().map(|inventory| {
            (
                inventory.root_identity.as_str(),
                inventory.entry_count,
                inventory.total_bytes,
                inventory.inventory_digest.as_str(),
            )
        });
        if plan.backup_id != request.backup_id
            || plan.snapshot_id != request.snapshot_id
            || plan.conflict_policy != request.conflict_policy
            || persisted_inventory != requested_inventory
            || validate_archive_restore_plan(&state, &plan).is_err()
        {
            return Err(ApiError::conflict(
                "job_conflict",
                "This archive restore job ID is bound to a different preview.",
            ));
        }
        return persisted_plan_response(&state, plan);
    }
    let admission = state.admit_engine_job()?;
    let target_root = create_archive_restore_target(&state, &request.job_id)?;
    let options = RestoreOptions {
        conflict_policy: request.conflict_policy,
        selected_paths: Default::default(),
        job_id: request.job_id,
        target_inventory: request.target_inventory,
    };
    let engine = Arc::clone(&state.engine);
    let worker_target_root = target_root.clone();
    let worker_state = state.clone();
    let outcome = match tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let outcome = engine
            .preview_restore(
                request.backup_id,
                &request.snapshot_id,
                &worker_target_root,
                &options,
            )
            .map_err(ApiError::from_core);
        match outcome {
            Ok(plan) => {
                persist_restore_plan(&worker_state, &plan)?;
                Ok(plan)
            }
            Err(error) => {
                if let Some(job_directory) = worker_target_root.parent() {
                    let _ = remove_private_job_directory(
                        worker_state.archive_restore_root.as_path(),
                        job_directory,
                    );
                }
                Err(error)
            }
        }
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => {
            if let Some(job_directory) = target_root.parent() {
                let _ = remove_private_job_directory(
                    state.archive_restore_root.as_path(),
                    job_directory,
                );
            }
            return Err(ApiError::internal("archive restore preview worker failed"));
        }
    };
    match outcome {
        Ok(plan) => persisted_plan_response(&state, plan),
        Err(error) => Err(error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreExecuteRequest {
    #[serde(default)]
    plan: Option<RestorePlan>,
    #[serde(default)]
    plan_id: Option<String>,
}

impl RestoreExecuteRequest {
    fn resolve(self, state: &AppState) -> Result<(String, RestorePlan), ApiError> {
        match (self.plan, self.plan_id) {
            (Some(plan), None) => {
                let plan_id = persist_restore_plan(state, &plan)?;
                Ok((plan_id, plan))
            }
            (None, Some(plan_id)) => {
                let plan = load_restore_plan(state, &plan_id)?;
                Ok((plan_id, plan))
            }
            _ => Err(ApiError::bad_request(
                "invalid_restore_execute_request",
                "Provide exactly one signed plan or durable plan ID.",
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestorePlanPageQuery {
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default = "default_restore_plan_page_size")]
    limit: usize,
}

const fn default_restore_plan_page_size() -> usize {
    100
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestorePlanPage {
    plan_id: String,
    backup_id: BackupId,
    snapshot_id: String,
    authorized_root: String,
    manifest_digest: String,
    conflict_policy: ConflictPolicy,
    job_id: String,
    plan_digest: String,
    signer_device_id: DeviceId,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_inventory: Option<TargetInventoryBinding>,
    entry_offset: usize,
    total_entries: usize,
    entries: Vec<RestorePreviewEntry>,
    next_cursor: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestorePlanReference {
    plan_id: String,
    plan_digest: String,
    backup_id: BackupId,
    snapshot_id: String,
    authorized_root: String,
    manifest_digest: String,
    conflict_policy: ConflictPolicy,
    job_id: String,
    signer_device_id: DeviceId,
    signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_inventory: Option<TargetInventoryBinding>,
    total_entries: usize,
}

async fn restore_plan_page(
    State(state): State<AppState>,
    AxumPath(plan_id): AxumPath<String>,
    Query(query): Query<RestorePlanPageQuery>,
    headers: HeaderMap,
) -> Result<axum::Json<RestorePlanPage>, ApiError> {
    authorize(&state, &headers)?;
    if query.limit == 0 || query.limit > 1_000 {
        return Err(ApiError::bad_request(
            "invalid_page_limit",
            "Restore plan pages must contain between 1 and 1,000 entries.",
        ));
    }
    let plan = load_restore_plan(&state, &plan_id)?;
    let cursor = match query.cursor.as_deref() {
        Some(cursor) if !cursor.is_empty() && cursor.bytes().all(|byte| byte.is_ascii_digit()) => {
            cursor.parse::<usize>().map_err(|_| {
                ApiError::bad_request("invalid_page_cursor", "The restore plan cursor is invalid.")
            })?
        }
        None => 0,
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_page_cursor",
                "The restore plan cursor is invalid.",
            ));
        }
    };
    if cursor > plan.entries.len() {
        return Err(ApiError::bad_request(
            "invalid_page_cursor",
            "The restore plan cursor is outside the plan.",
        ));
    }
    let end = cursor.saturating_add(query.limit).min(plan.entries.len());
    let next_cursor = (end < plan.entries.len()).then(|| end.to_string());
    Ok(axum::Json(RestorePlanPage {
        plan_id,
        backup_id: plan.backup_id,
        snapshot_id: plan.snapshot_id,
        authorized_root: plan.authorized_root,
        manifest_digest: plan.manifest_digest,
        conflict_policy: plan.conflict_policy,
        job_id: plan.job_id,
        plan_digest: plan.plan_digest,
        signer_device_id: plan.signer_device_id,
        signature: plan.signature,
        target_inventory: plan.target_inventory,
        entry_offset: cursor,
        total_entries: plan.entries.len(),
        entries: plan.entries[cursor..end].to_vec(),
        next_cursor,
    }))
}

fn persisted_plan_response(state: &AppState, plan: RestorePlan) -> Result<Response, ApiError> {
    let plan_id = persist_restore_plan(state, &plan)?;
    let summary = RestorePlanReference {
        plan_id: plan_id.clone(),
        plan_digest: plan.plan_digest.clone(),
        backup_id: plan.backup_id,
        snapshot_id: plan.snapshot_id,
        authorized_root: plan.authorized_root,
        manifest_digest: plan.manifest_digest,
        conflict_policy: plan.conflict_policy,
        job_id: plan.job_id,
        signer_device_id: plan.signer_device_id,
        signature: plan.signature,
        target_inventory: plan.target_inventory,
        total_entries: plan.entries.len(),
    };
    let mut response = axum::Json(summary).into_response();
    insert_restore_plan_headers(&mut response, &plan_id, &plan.plan_digest)?;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn insert_restore_plan_headers(
    response: &mut Response,
    plan_id: &str,
    plan_digest: &str,
) -> Result<(), ApiError> {
    response.headers_mut().insert(
        RESTORE_PLAN_ID_HEADER,
        HeaderValue::from_str(plan_id)
            .map_err(|_| ApiError::internal("restore plan ID header is invalid"))?,
    );
    response.headers_mut().insert(
        RESTORE_PLAN_DIGEST_HEADER,
        HeaderValue::from_str(plan_digest)
            .map_err(|_| ApiError::internal("restore plan digest header is invalid"))?,
    );
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreResponse {
    files_restored: usize,
    directories_created: usize,
    files_skipped: usize,
    bytes_written: u64,
    rejected_provider_copies: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArchiveRestoreCompletion {
    plan_id: String,
    plan_digest: String,
    result: RestoreResponse,
}

async fn restore_execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestoreExecuteRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let (plan_id, plan) = request.resolve(&state)?;
    let plan_digest = plan.plan_digest.clone();
    let admission = state.admit_engine_job()?;
    let engine = Arc::clone(&state.engine);
    let job_id = plan.job_id.clone();
    let mut lease = state.start_job(&job_id)?;
    let control = lease.control();
    let report = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        engine.restore(&plan, &control)
    })
    .await
    .map_err(|_| ApiError::internal("restore worker failed"))?;
    match &report {
        Ok(_) => lease.finish().map_err(ApiError::from_core)?,
        Err(CoreError::Cancelled) => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
        Err(CoreError::Paused) => lease.preserve_for_resume().map_err(ApiError::from_core)?,
        Err(_) => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
    }
    let report = report.map_err(ApiError::from_core)?;
    let mut response = axum::Json(RestoreResponse {
        files_restored: report.files_restored,
        directories_created: report.directories_created,
        files_skipped: report.files_skipped,
        bytes_written: report.bytes_written,
        rejected_provider_copies: report.rejected_provider_copies.len(),
    })
    .into_response();
    insert_restore_plan_headers(&mut response, &plan_id, &plan_digest)?;
    Ok(response)
}

async fn restore_archive_execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestoreExecuteRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let (plan_id, plan) = request.resolve(&state)?;
    let plan_digest = plan.plan_digest.clone();
    if plan.target_inventory.is_none()
        && (plan.conflict_policy != ConflictPolicy::Fail
            || plan.entries.iter().any(|entry| {
                !matches!(
                    (entry.kind, entry.action),
                    (EntryKind::Directory, PreviewAction::CreateDirectory)
                        | (EntryKind::File, PreviewAction::CreateFile)
                )
            }))
    {
        return Err(ApiError::bad_request(
            "invalid_streamed_restore_plan",
            "A streamed restore plan may only create content in an empty destination.",
        ));
    }
    if let Some(response) = completed_archive_restore_response(&plan_id, &plan).await? {
        return Ok(response);
    }
    let target_root = validate_archive_restore_plan(&state, &plan)?;
    let admission = state.admit_engine_job()?;
    let job_directory = target_root
        .parent()
        .ok_or_else(|| ApiError::internal("archive restore job has no parent"))?
        .to_path_buf();
    let result_archive_path = job_directory.join("result.zip");
    let result_json_path = job_directory.join("result.json");
    let worker_result_archive_path = result_archive_path.clone();
    let worker_result_json_path = result_json_path.clone();
    let worker_plan_id = plan_id.clone();
    let worker_state = state.clone();
    let job_id = plan.job_id.clone();
    let mut lease = state.start_job(&job_id)?;
    let control = lease.control();
    let outcome = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let manifest = worker_state
            .engine
            .load_manifest(plan.backup_id, &plan.snapshot_id)
            .map_err(ApiError::from_core)?;
        let expected_restore_bytes = manifest.entries.iter().try_fold(0_u64, |total, entry| {
            total
                .checked_add(if entry.kind == EntryKind::File {
                    entry.length
                } else {
                    0
                })
                .ok_or_else(|| ApiError::payload_too_large("Restore staging size overflowed."))
        })?;
        let peak_growth = expected_restore_bytes
            .checked_mul(2)
            .ok_or_else(|| ApiError::payload_too_large("Restore staging size overflowed."))?;
        let _staging_reservation = worker_state.reserve_archive_staging(peak_growth)?;
        prepare_external_restore_staging(&target_root, &plan)?;
        let report = worker_state
            .engine
            .restore(&plan, &control)
            .map_err(ApiError::from_core)?;
        let response = RestoreResponse {
            files_restored: report.files_restored,
            directories_created: report.directories_created,
            files_skipped: report.files_skipped,
            bytes_written: report.bytes_written,
            rejected_provider_copies: report.rejected_provider_copies.len(),
        };
        let length = zip_restore_directory(
            &target_root,
            &worker_result_archive_path,
            &control,
            worker_state.archive_limits,
            &plan,
        )?;
        let completion = ArchiveRestoreCompletion {
            plan_id: worker_plan_id,
            plan_digest: plan.plan_digest.clone(),
            result: response.clone(),
        };
        let completion_bytes = serde_json::to_vec(&completion).map_err(ApiError::from_json)?;
        ensure_retained_archive_capacity(
            &worker_state,
            1,
            length.saturating_add(
                u64::try_from(completion_bytes.len())
                    .map_err(|_| ApiError::payload_too_large("Restore result size overflowed."))?,
            ),
        )?;
        persist_private_file(&worker_result_json_path, &completion_bytes)
            .map_err(ApiError::from_core)?;
        compact_completed_restore_job(&target_root);
        Ok::<_, ApiError>((length, response))
    })
    .await
    .map_err(|_| ApiError::internal("archive restore worker failed"))?;
    match &outcome {
        Ok(_) => lease.finish().map_err(ApiError::from_core)?,
        Err(error) if error.code == "job_cancelled" => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
        Err(error) if error.code == "job_paused" => {
            lease.preserve_for_resume().map_err(ApiError::from_core)?;
        }
        Err(_) => {
            lease.finish().map_err(ApiError::from_core)?;
            discard_job_artifacts(&state, &job_id)?;
        }
    }
    let (length, result) = outcome?;
    archive_restore_response(
        &result_archive_path,
        length,
        &result,
        &plan_id,
        &plan_digest,
    )
    .await
}

fn require_archive_content_type(headers: &HeaderMap) -> Result<(), ApiError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if matches!(
        content_type,
        Some("application/zip" | "application/vnd.covalent.backup+zip")
    ) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_content_type",
            "A streamed Covalent ZIP archive is required.",
        ))
    }
}

fn decode_archive_metadata<T: for<'de> Deserialize<'de>>(
    headers: &HeaderMap,
) -> Result<T, ApiError> {
    let encoded = headers
        .get(ARCHIVE_METADATA_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            ApiError::bad_request(
                "archive_metadata_required",
                "Versioned archive metadata is required.",
            )
        })?;
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
        ApiError::bad_request(
            "invalid_archive_metadata",
            "Archive metadata encoding is invalid.",
        )
    })?;
    if bytes.len() > MAX_ARCHIVE_METADATA_BYTES {
        return Err(ApiError::bad_request(
            "invalid_archive_metadata",
            "Archive metadata exceeds the contract limit.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(ApiError::from_json)
}

fn prepare_archive_backup_job(
    state: &AppState,
    metadata: &ArchiveBackupMetadata,
) -> Result<(PathBuf, Option<BackupResponse>, bool), ApiError> {
    let _guard = state
        .archive_backup_lock
        .lock()
        .map_err(|_| ApiError::internal("archive backup staging lock failed"))?;
    prune_stale_archive_restore_targets(state.archive_backup_root.as_path())
        .map_err(ApiError::from_core)?;
    let job_directory = state.archive_backup_root.join(&metadata.job_id);
    let metadata_bytes = serde_json::to_vec(metadata).map_err(ApiError::from_json)?;
    let existed = match fs::symlink_metadata(&job_directory) {
        Ok(existing) => {
            if existing.file_type().is_symlink() || !existing.is_dir() {
                return Err(ApiError::internal("archive backup staging is invalid"));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ensure_retained_archive_capacity(state, 1, 0)?;
            let count = fs::read_dir(state.archive_backup_root.as_path())
                .map_err(|_| ApiError::internal("archive backup staging could not be inspected"))?
                .count();
            if count >= state.archive_limits.maximum_jobs
                && !evict_oldest_incomplete_archive_job(state, &state.archive_backup_root)?
            {
                return Err(ApiError::payload_too_large(
                    "Too many archive jobs are waiting for completion or acknowledgement.",
                ));
            }
            fs::create_dir(&job_directory)
                .map_err(|_| ApiError::internal("archive backup staging could not be created"))?;
            create_private_directory(job_directory.clone()).map_err(ApiError::from_core)?;
            false
        }
        Err(_) => {
            return Err(ApiError::internal(
                "archive backup staging could not be inspected",
            ));
        }
    };
    let stored_metadata_path = job_directory.join("metadata.json");
    if existed {
        let stored = fs::read(&stored_metadata_path)
            .map_err(|_| ApiError::internal("archive backup metadata is unavailable"))?;
        if stored != metadata_bytes {
            return Err(ApiError::conflict(
                "job_conflict",
                "This archive job ID is bound to different metadata.",
            ));
        }
    } else {
        persist_private_file(&stored_metadata_path, &metadata_bytes)
            .map_err(ApiError::from_core)?;
    }
    let result_path = job_directory.join("result.json");
    let completed = match fs::read(&result_path) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).map_err(ApiError::from_json)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => {
            return Err(ApiError::internal(
                "archive backup result could not be read",
            ));
        }
    };
    Ok((job_directory, completed, existed))
}

async fn receive_archive(
    body: Body,
    headers: &HeaderMap,
    state: &AppState,
    job_directory: &Path,
    metadata_digest: &str,
) -> Result<PathBuf, ApiError> {
    receive_archive_inner(body, headers, state, job_directory, metadata_digest).await
}

async fn receive_archive_inner(
    mut body: Body,
    headers: &HeaderMap,
    state: &AppState,
    job_directory: &Path,
    metadata_digest: &str,
) -> Result<PathBuf, ApiError> {
    let upload_directory = SafeUploadDirectory::open(job_directory)?;
    let offset = required_archive_u64_header(headers, ARCHIVE_UPLOAD_OFFSET_HEADER, true)?;
    let total_length = required_archive_u64_header(headers, ARCHIVE_UPLOAD_LENGTH_HEADER, false)?;
    let expected_digest = headers
        .get(ARCHIVE_UPLOAD_DIGEST_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_lowercase_digest(value))
        .ok_or_else(|| {
            ApiError::bad_request(
                "invalid_upload_digest",
                "Archive upload digest must be a lowercase SHA-256 digest.",
            )
        })?
        .to_owned();
    if total_length > state.archive_limits.maximum_compressed_bytes {
        return Err(ApiError::payload_too_large(
            "Archive exceeds the streamed transfer limit.",
        ));
    }
    if offset > total_length {
        return Err(ApiError::upload_offset(
            StatusCode::CONFLICT,
            "upload_offset_mismatch",
            "Archive upload offset is outside the declared archive length.",
            0,
            true,
        ));
    }
    let request_length = optional_archive_u64_header(headers, header::CONTENT_LENGTH.as_str())?;
    if request_length.is_some_and(|length| length > total_length - offset) {
        return Err(ApiError::payload_too_large(
            "Archive request body exceeds the declared remaining length.",
        ));
    }

    let session = ArchiveUploadSession {
        schema_version: ARCHIVE_UPLOAD_SESSION_SCHEMA_VERSION,
        total_length,
        sha256_digest: expected_digest.clone(),
        metadata_digest: metadata_digest.to_owned(),
    };
    let session_name = "upload-session.json";
    let temporary_name = "upload.part";
    match upload_directory.read_private_file(session_name, MAX_ARCHIVE_METADATA_BYTES as u64)? {
        Some(bytes) => {
            let stored: ArchiveUploadSession =
                serde_json::from_slice(&bytes).map_err(ApiError::from_json)?;
            if stored != session {
                let durable_offset = upload_directory.private_file_length(
                    temporary_name,
                    state.archive_limits.maximum_compressed_bytes,
                )?;
                return Err(ApiError::upload_offset(
                    StatusCode::CONFLICT,
                    "upload_identity_mismatch",
                    "This archive job is bound to different metadata, length, or content digest.",
                    durable_offset,
                    false,
                ));
            }
        }
        None if offset == 0 => {
            upload_directory.create_private_file(
                session_name,
                &serde_json::to_vec(&session).map_err(ApiError::from_json)?,
            )?;
        }
        None => {
            return Err(ApiError::upload_offset(
                StatusCode::CONFLICT,
                "upload_offset_mismatch",
                "No resumable archive upload exists for the requested offset.",
                0,
                true,
            ));
        }
    }
    let final_name = format!("upload-{expected_digest}.zip");
    let final_length = upload_directory
        .private_file_length(&final_name, state.archive_limits.maximum_compressed_bytes)?;
    if final_length != 0 {
        if final_length == total_length {
            return Ok(job_directory.join(final_name));
        }
        return Err(ApiError::internal(
            "durable archive upload length is invalid",
        ));
    }
    let durable_offset = upload_directory.private_file_length(
        temporary_name,
        state.archive_limits.maximum_compressed_bytes,
    )?;
    if offset != durable_offset {
        return Err(ApiError::upload_offset(
            StatusCode::CONFLICT,
            "upload_offset_mismatch",
            "Archive upload offset does not match the durable server offset.",
            durable_offset,
            true,
        ));
    }
    let remaining = total_length - durable_offset;
    let _reservation = state.reserve_archive_staging(remaining)?;
    let mut hasher = Sha256::new();
    if durable_offset > 0 {
        let mut prefix = tokio::fs::File::from_std(upload_directory.open_private_reader(
            temporary_name,
            state.archive_limits.maximum_compressed_bytes,
        )?);
        let mut buffer = [0_u8; 64 * 1_024];
        loop {
            let read = prefix
                .read(&mut buffer)
                .await
                .map_err(|_| ApiError::internal("archive partial upload could not be read"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    let mut archive =
        tokio::fs::File::from_std(upload_directory.open_partial_append(temporary_name)?);
    let mut received = durable_offset;
    let mut request_received = 0_u64;
    let started = Instant::now();
    loop {
        if started.elapsed() > ARCHIVE_UPLOAD_MAX_DURATION {
            archive
                .sync_all()
                .await
                .map_err(|_| ApiError::internal("archive staging sync failed"))?;
            return Err(ApiError::upload_offset(
                StatusCode::CONFLICT,
                "upload_incomplete",
                "Archive upload reached the request duration limit and can be resumed.",
                received,
                true,
            ));
        }
        let next = match tokio::time::timeout(ARCHIVE_UPLOAD_IDLE_TIMEOUT, body.frame()).await {
            Ok(next) => next,
            Err(_) => {
                archive
                    .sync_all()
                    .await
                    .map_err(|_| ApiError::internal("archive staging sync failed"))?;
                return Err(ApiError::upload_offset(
                    StatusCode::CONFLICT,
                    "upload_incomplete",
                    "Archive upload stopped making progress and can be resumed.",
                    received,
                    true,
                ));
            }
        };
        let Some(frame) = next else {
            break;
        };
        let frame = match frame {
            Ok(frame) => frame,
            Err(_) => {
                archive
                    .sync_all()
                    .await
                    .map_err(|_| ApiError::internal("archive staging sync failed"))?;
                return Err(ApiError::upload_offset(
                    StatusCode::CONFLICT,
                    "upload_incomplete",
                    "The interrupted archive upload can be resumed.",
                    received,
                    true,
                ));
            }
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        received = received
            .checked_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        request_received = request_received
            .checked_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        if received > total_length {
            drop(archive);
            upload_directory.remove(temporary_name);
            upload_directory.remove(session_name);
            return Err(ApiError::payload_too_large(
                "Archive request body exceeds the declared total length.",
            ));
        }
        if started.elapsed() >= Duration::from_secs(60)
            && request_received / started.elapsed().as_secs().max(1)
                < MIN_ARCHIVE_UPLOAD_BYTES_PER_SECOND
        {
            archive
                .sync_all()
                .await
                .map_err(|_| ApiError::internal("archive staging sync failed"))?;
            return Err(ApiError::upload_offset(
                StatusCode::CONFLICT,
                "upload_incomplete",
                "Archive upload remained below the minimum transfer rate and can be resumed.",
                received,
                true,
            ));
        }
        hasher.update(&data);
        archive
            .write_all(&data)
            .await
            .map_err(|_| ApiError::internal("archive staging write failed"))?;
    }
    archive
        .sync_all()
        .await
        .map_err(|_| ApiError::internal("archive staging sync failed"))?;
    drop(archive);
    if request_length.is_some_and(|length| length != request_received) || received < total_length {
        return Err(ApiError::upload_offset(
            StatusCode::CONFLICT,
            "upload_incomplete",
            "Archive upload is incomplete and can be resumed from the durable offset.",
            received,
            true,
        ));
    }
    let digest = lowercase_hex(&hasher.finalize());
    if digest != expected_digest {
        upload_directory.remove(temporary_name);
        upload_directory.remove(session_name);
        return Err(ApiError::unprocessable(
            "archive_digest_mismatch",
            "Archive content did not match its declared SHA-256 digest.",
        ));
    }
    let final_name = format!("upload-{digest}.zip");
    upload_directory.commit(temporary_name, &final_name)?;
    Ok(job_directory.join(final_name))
}

fn required_archive_u64_header(
    headers: &HeaderMap,
    name: &'static str,
    allow_zero: bool,
) -> Result<u64, ApiError> {
    let value = optional_archive_u64_header(headers, name)?.ok_or_else(|| {
        ApiError::bad_request(
            "archive_upload_headers_required",
            "Archive upload offset, total length, and digest headers are required.",
        )
    })?;
    if !allow_zero && value == 0 {
        return Err(ApiError::bad_request(
            "invalid_upload_length",
            "Archive upload length must be greater than zero.",
        ));
    }
    Ok(value)
}

fn optional_archive_u64_header(headers: &HeaderMap, name: &str) -> Result<Option<u64>, ApiError> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "invalid_upload_offset",
                        "Archive upload byte headers must contain unsigned decimal integers.",
                    )
                })
        })
        .transpose()
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn prepared_archive_source(
    job_directory: &Path,
    metadata_digest: &str,
) -> Result<Option<ArchivePreparedSource>, ApiError> {
    let marker_path = job_directory.join("source-ready.json");
    let bytes = match fs::read(&marker_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(ApiError::internal(
                "archive source marker could not be read",
            ));
        }
    };
    let marker_metadata = fs::symlink_metadata(&marker_path)
        .map_err(|_| ApiError::internal("archive source marker could not be inspected"))?;
    let source_root = job_directory.join("source");
    let source_metadata = fs::symlink_metadata(&source_root)
        .map_err(|_| ApiError::internal("prepared archive source is unavailable"))?;
    if marker_metadata.file_type().is_symlink()
        || !marker_metadata.is_file()
        || marker_metadata.len() > MAX_ARCHIVE_METADATA_BYTES as u64
        || source_metadata.file_type().is_symlink()
        || !source_metadata.is_dir()
    {
        return Err(ApiError::internal("prepared archive source is invalid"));
    }
    let marker: ArchivePreparedSource =
        serde_json::from_slice(&bytes).map_err(ApiError::from_json)?;
    if marker.schema_version != 1
        || marker.metadata_digest != metadata_digest
        || !valid_lowercase_digest(&marker.upload_digest)
    {
        return Err(ApiError::conflict(
            "job_conflict",
            "Prepared archive source is bound to different job metadata.",
        ));
    }
    Ok(Some(marker))
}

fn compact_completed_backup_job(job_directory: &Path) {
    for path in [
        job_directory.join("source"),
        job_directory.join("source-ready.json"),
        job_directory.join("upload-session.json"),
        job_directory.join("upload.part"),
    ] {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                if let Err(error) = fs::remove_dir_all(&path) {
                    tracing::warn!(path = %path.display(), %error, "completed archive source cleanup deferred");
                }
            }
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if let Err(error) = fs::remove_file(&path) {
                    tracing::warn!(path = %path.display(), %error, "completed archive artifact cleanup deferred");
                }
            }
            Ok(_) => {
                tracing::warn!(path = %path.display(), "completed archive artifact was not a private regular entry")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "completed archive artifact inspection failed")
            }
        }
    }
    if let Ok(entries) = fs::read_dir(job_directory) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name
                .to_str()
                .is_some_and(|name| name.starts_with("upload-") && name.ends_with(".zip"))
                && let Err(error) = fs::remove_file(entry.path())
            {
                tracing::warn!(path = %entry.path().display(), %error, "completed archive upload cleanup deferred");
            }
        }
    }
    if let Err(error) = File::open(job_directory).and_then(|directory| directory.sync_all()) {
        tracing::warn!(path = %job_directory.display(), %error, "completed archive compaction sync deferred");
    }
}

fn compact_completed_restore_job(target_root: &Path) {
    match fs::symlink_metadata(target_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if let Err(error) = fs::remove_dir_all(target_root) {
                tracing::warn!(path = %target_root.display(), %error, "completed restore target cleanup deferred");
            }
        }
        Ok(_) => {
            tracing::warn!(path = %target_root.display(), "completed restore target was not a private directory")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %target_root.display(), %error, "completed restore target inspection failed")
        }
    }
    if let Some(job_directory) = target_root.parent()
        && let Err(error) = File::open(job_directory).and_then(|directory| directory.sync_all())
    {
        tracing::warn!(path = %job_directory.display(), %error, "completed restore compaction sync deferred");
    }
}

fn archive_tree_bytes(root: &Path) -> Result<u64, ApiError> {
    let mut total = 0_u64;
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|_| ApiError::internal("archive staging could not be walked"))?;
        if entry.path() == root {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| ApiError::internal("archive staging entry could not be inspected"))?;
        if metadata.file_type().is_symlink() {
            return Err(ApiError::internal(
                "archive staging unexpectedly contained a symbolic link",
            ));
        }
        if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| ApiError::payload_too_large("Archive staging size overflowed."))?;
        } else if !metadata.is_dir() {
            return Err(ApiError::internal(
                "archive staging contained an unsupported entry",
            ));
        }
    }
    Ok(total)
}

fn retained_archive_usage(state: &AppState) -> Result<(usize, u64), ApiError> {
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    for root in [
        state.archive_backup_root.as_path(),
        state.archive_restore_root.as_path(),
    ] {
        for entry in fs::read_dir(root)
            .map_err(|_| ApiError::internal("archive result store could not be inspected"))?
        {
            let entry = entry
                .map_err(|_| ApiError::internal("archive result entry could not be inspected"))?;
            let path = entry.path();
            if !path.join("result.json").is_file() {
                continue;
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| ApiError::payload_too_large("Archive result count overflowed."))?;
            bytes = bytes
                .checked_add(archive_tree_bytes(&path)?)
                .ok_or_else(|| ApiError::payload_too_large("Archive result size overflowed."))?;
        }
    }
    Ok((count, bytes))
}

fn ensure_retained_archive_capacity(
    state: &AppState,
    additional_results: usize,
    additional_bytes: u64,
) -> Result<(), ApiError> {
    let (count, bytes) = retained_archive_usage(state)?;
    if count.saturating_add(additional_results) > state.archive_limits.maximum_retained_results
        || bytes.saturating_add(additional_bytes)
            > state.archive_limits.maximum_retained_result_bytes
    {
        return Err(ApiError::insufficient_storage(
            "Acknowledge completed archive jobs before retaining another result.",
        ));
    }
    Ok(())
}

fn ensure_archive_capacity(
    path: &Path,
    required_bytes: u64,
    reserve_bytes: u64,
) -> Result<(), ApiError> {
    let available = fs2::available_space(path)
        .map_err(|_| ApiError::internal("archive staging capacity is unavailable"))?;
    if available < required_bytes.saturating_add(reserve_bytes) {
        return Err(ApiError {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code: "insufficient_storage",
            message: "The node does not have enough reserved capacity for this archive.",
            retryable: true,
            upload_offset: None,
        });
    }
    Ok(())
}

fn remove_private_job_directory(root: &Path, job_directory: &Path) -> Result<(), ApiError> {
    if job_directory.parent() != Some(root) {
        return Err(ApiError::internal("archive job escaped its staging root"));
    }
    match fs::symlink_metadata(job_directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(job_directory)
                .map_err(|_| ApiError::internal("archive job cleanup failed"))?;
            File::open(root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ApiError::internal("archive job cleanup could not be synced"))
        }
        Ok(_) => Err(ApiError::internal("archive job staging is invalid")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(ApiError::internal(
            "archive job staging could not be inspected",
        )),
    }
}

fn extract_backup_archive(
    archive_path: &Path,
    source_root: &Path,
    state: &AppState,
    control: &JobControl,
) -> Result<(), ApiError> {
    let limits = state.archive_limits;
    let started = Instant::now();
    let file = File::open(archive_path)
        .map_err(|_| ApiError::internal("archive staging file could not be opened"))?;
    let mut archive = ZipArchive::new(file).map_err(|_| {
        ApiError::bad_request("invalid_archive", "The streamed ZIP archive is invalid.")
    })?;
    if archive.len() > limits.maximum_entries {
        return Err(ApiError::payload_too_large(
            "Archive contains too many entries.",
        ));
    }
    let mut declared_expanded = 0_u64;
    let mut declared_compressed = 0_u64;
    for index in 0..archive.len() {
        check_archive_control(control, started)?;
        let entry = archive.by_index(index).map_err(|_| {
            ApiError::bad_request("invalid_archive", "An archive entry is invalid.")
        })?;
        declared_expanded = declared_expanded
            .checked_add(entry.size())
            .ok_or_else(|| ApiError::payload_too_large("Archive expansion size overflowed."))?;
        declared_compressed = declared_compressed
            .checked_add(entry.compressed_size())
            .ok_or_else(|| ApiError::payload_too_large("Archive compressed size overflowed."))?;
        if declared_expanded > limits.maximum_uncompressed_bytes {
            return Err(ApiError::payload_too_large(
                "Archive expands beyond the configured limit.",
            ));
        }
    }
    if declared_expanded
        > declared_compressed
            .max(1)
            .saturating_mul(MAX_ARCHIVE_COMPRESSION_RATIO)
    {
        return Err(ApiError::payload_too_large(
            "Archive compression ratio exceeds the configured limit.",
        ));
    }
    let _extraction_reservation = state.reserve_archive_staging(declared_expanded)?;
    drop(archive);
    create_private_directory(source_root.to_path_buf()).map_err(ApiError::from_core)?;
    let file = File::open(archive_path)
        .map_err(|_| ApiError::internal("archive staging file could not be reopened"))?;
    let mut archive = ZipArchive::new(file).map_err(|_| {
        ApiError::bad_request("invalid_archive", "The streamed ZIP archive is invalid.")
    })?;
    let mut seen = BTreeSet::new();
    let mut total_processed = 0_u64;
    for index in 0..archive.len() {
        check_archive_progress(control, started, total_processed)?;
        let mut entry = archive.by_index(index).map_err(|_| {
            ApiError::bad_request("invalid_archive", "An archive entry is invalid.")
        })?;
        if entry.is_symlink() || (!entry.is_file() && !entry.is_dir()) {
            return Err(ApiError::bad_request(
                "invalid_archive_entry",
                "Archive entries must be regular files or directories.",
            ));
        }
        let raw_name = std::str::from_utf8(entry.name_raw()).map_err(|_| {
            ApiError::bad_request(
                "invalid_archive_entry",
                "Archive entry names must be UTF-8.",
            )
        })?;
        let canonical_name = if entry.is_dir() {
            raw_name.strip_suffix('/').unwrap_or(raw_name)
        } else {
            raw_name
        };
        let relative = RelativePath::new(canonical_name.to_owned()).map_err(|_| {
            ApiError::bad_request(
                "invalid_archive_entry",
                "Archive entry paths must be safe relative paths.",
            )
        })?;
        if !seen.insert(relative.clone()) {
            return Err(ApiError::bad_request(
                "duplicate_archive_entry",
                "Archive entry paths must be unique.",
            ));
        }
        let destination = relative
            .components()
            .fold(source_root.to_path_buf(), |path, component| {
                path.join(component)
            });
        if entry.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|_| ApiError::internal("archive directory could not be staged"))?;
            continue;
        }
        let parent = destination.parent().ok_or_else(|| {
            ApiError::bad_request("invalid_archive_entry", "Archive file has no parent.")
        })?;
        fs::create_dir_all(parent)
            .map_err(|_| ApiError::internal("archive parent could not be staged"))?;
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|_| {
                ApiError::bad_request(
                    "duplicate_archive_entry",
                    "Archive files may not replace staged entries.",
                )
            })?;
        let copied = copy_archive_bytes(
            &mut entry,
            &mut output,
            control,
            started,
            &mut total_processed,
            limits.maximum_uncompressed_bytes,
            true,
        )?;
        if copied != entry.size() {
            return Err(ApiError::bad_request(
                "invalid_archive",
                "Archive entry length did not match its metadata.",
            ));
        }
        output
            .sync_all()
            .map_err(|_| ApiError::internal("archive file sync failed"))?;
    }
    Ok(())
}

fn check_archive_control(control: &JobControl, started: Instant) -> Result<(), ApiError> {
    match control.state() {
        JobState::Running => {}
        JobState::Paused => return Err(ApiError::from_core(CoreError::Paused)),
        JobState::Cancelled => return Err(ApiError::from_core(CoreError::Cancelled)),
    }
    if started.elapsed() > ARCHIVE_PROCESSING_MAX_DURATION {
        return Err(ApiError {
            status: StatusCode::REQUEST_TIMEOUT,
            code: "archive_processing_timeout",
            message: "Archive processing exceeded the maximum duration.",
            retryable: true,
            upload_offset: None,
        });
    }
    Ok(())
}

fn check_archive_progress(
    control: &JobControl,
    started: Instant,
    processed: u64,
) -> Result<(), ApiError> {
    check_archive_control(control, started)?;
    if started.elapsed() >= Duration::from_secs(60)
        && processed / started.elapsed().as_secs().max(1) < MIN_ARCHIVE_PROCESS_BYTES_PER_SECOND
    {
        return Err(ApiError::bad_request(
            "archive_processing_too_slow",
            "Archive processing remained below the minimum safe rate.",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_archive_bytes<R: std::io::Read, W: std::io::Write>(
    input: &mut R,
    output: &mut W,
    control: &JobControl,
    started: Instant,
    total_processed: &mut u64,
    maximum_bytes: u64,
    untrusted_input: bool,
) -> Result<u64, ApiError> {
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1_024];
    loop {
        check_archive_progress(control, started, *total_processed)?;
        let read = input.read(&mut buffer).map_err(|_| {
            if untrusted_input {
                ApiError::bad_request("invalid_archive", "Archive content failed validation.")
            } else {
                ApiError::internal("restored file could not be read")
            }
        })?;
        if read == 0 {
            return Ok(copied);
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        *total_processed = total_processed
            .checked_add(read as u64)
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        if *total_processed > maximum_bytes {
            return Err(ApiError::payload_too_large(
                "Archive content exceeds the configured limit.",
            ));
        }
        output.write_all(&buffer[..read]).map_err(|_| {
            if untrusted_input {
                ApiError::internal("archive staging write failed")
            } else {
                ApiError::internal("restore archive write failed")
            }
        })?;
    }
}

fn valid_job_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_plan_identifier(value: &str) -> bool {
    value
        .strip_suffix(".json")
        .is_some_and(valid_lowercase_digest)
}

fn valid_lowercase_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn restore_plan_identifier(plan_digest: &str) -> Result<String, ApiError> {
    if !valid_lowercase_digest(plan_digest) {
        return Err(ApiError::internal("restore plan digest is invalid"));
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(RESTORE_PLAN_ID_DOMAIN);
    hasher.update(&[0]);
    hasher.update(plan_digest.as_bytes());
    Ok(hasher.finalize().to_hex().to_string())
}

fn persist_restore_plan(state: &AppState, plan: &RestorePlan) -> Result<String, ApiError> {
    let plan_id = restore_plan_identifier(&plan.plan_digest)?;
    let _guard = state
        .restore_plan_lock
        .lock()
        .map_err(|_| ApiError::internal("restore plan lock failed"))?;
    prune_stale_private_files(
        state.restore_plan_root.as_path(),
        RESTORE_PLAN_MAX_AGE,
        MAX_RESTORE_PLAN_BYTES,
        valid_plan_identifier,
    )
    .map_err(ApiError::from_core)?;
    let path = state.restore_plan_root.join(format!("{plan_id}.json"));
    for entry in fs::read_dir(state.restore_plan_root.as_path())
        .map_err(|_| ApiError::internal("restore plan store could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("restore plan entry is invalid"))?;
        if entry.path() == path {
            continue;
        }
        let existing: RestorePlan = serde_json::from_slice(
            &fs::read(entry.path())
                .map_err(|_| ApiError::internal("restore plan could not be read"))?,
        )
        .map_err(ApiError::from_json)?;
        if existing.job_id == plan.job_id {
            return Err(ApiError::conflict(
                "job_conflict",
                "This job ID is already bound to a different signed restore plan.",
            ));
        }
    }
    if !path.exists()
        && fs::read_dir(state.restore_plan_root.as_path())
            .map_err(|_| ApiError::internal("restore plan store could not be inspected"))?
            .count()
            >= MAX_RESTORE_PLANS
    {
        return Err(ApiError::payload_too_large(
            "Too many restore plans are waiting for execution.",
        ));
    }
    let bytes = serde_json::to_vec(plan).map_err(ApiError::from_json)?;
    if bytes.len() as u64 > MAX_RESTORE_PLAN_BYTES {
        return Err(ApiError::payload_too_large(
            "The restore plan exceeds the durable plan limit.",
        ));
    }
    if path.exists() {
        let existing =
            fs::read(&path).map_err(|_| ApiError::internal("restore plan could not be read"))?;
        if existing == bytes {
            return Ok(plan_id);
        }
        return Err(ApiError::internal(
            "restore plan ID is bound to different content",
        ));
    }
    persist_private_file(&path, &bytes).map_err(ApiError::from_core)?;
    Ok(plan_id)
}

fn load_restore_plan(state: &AppState, plan_id: &str) -> Result<RestorePlan, ApiError> {
    if !valid_lowercase_digest(plan_id) {
        return Err(ApiError::bad_request(
            "invalid_restore_plan_id",
            "The restore plan ID is invalid.",
        ));
    }
    let path = state.restore_plan_root.join(format!("{plan_id}.json"));
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(
                "restore_plan_not_found",
                "The restore plan is unavailable or expired.",
            )
        } else {
            ApiError::internal("restore plan could not be inspected")
        }
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_RESTORE_PLAN_BYTES
    {
        return Err(ApiError::internal("stored restore plan is invalid"));
    }
    let plan: RestorePlan = serde_json::from_slice(
        &fs::read(&path).map_err(|_| ApiError::internal("restore plan could not be read"))?,
    )
    .map_err(ApiError::from_json)?;
    if restore_plan_identifier(&plan.plan_digest)? != plan_id {
        return Err(ApiError::internal("stored restore plan ID mismatch"));
    }
    Ok(plan)
}

fn find_restore_plan_by_job(
    state: &AppState,
    job_id: &str,
) -> Result<Option<RestorePlan>, ApiError> {
    let _guard = state
        .restore_plan_lock
        .lock()
        .map_err(|_| ApiError::internal("restore plan lock failed"))?;
    let mut found = None;
    for entry in fs::read_dir(state.restore_plan_root.as_path())
        .map_err(|_| ApiError::internal("restore plan store could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("restore plan entry is invalid"))?;
        let metadata = entry
            .metadata()
            .map_err(|_| ApiError::internal("restore plan entry could not be inspected"))?;
        if !metadata.is_file()
            || metadata.len() > MAX_RESTORE_PLAN_BYTES
            || !entry
                .file_name()
                .to_str()
                .is_some_and(valid_plan_identifier)
        {
            return Err(ApiError::internal("restore plan entry is invalid"));
        }
        let plan: RestorePlan = serde_json::from_slice(
            &fs::read(entry.path())
                .map_err(|_| ApiError::internal("restore plan could not be read"))?,
        )
        .map_err(ApiError::from_json)?;
        if plan.job_id == job_id {
            if found.is_some() {
                return Err(ApiError::internal(
                    "multiple restore plans are bound to one job",
                ));
            }
            found = Some(plan);
        }
    }
    Ok(found)
}

async fn completed_archive_restore_response(
    plan_id: &str,
    plan: &RestorePlan,
) -> Result<Option<Response>, ApiError> {
    let target = PathBuf::from(&plan.authorized_root);
    let job_directory = target
        .parent()
        .ok_or_else(|| ApiError::internal("archive restore job has no parent"))?;
    let archive_path = job_directory.join("result.zip");
    let result_path = job_directory.join("result.json");
    let completion = match fs::symlink_metadata(&result_path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && metadata.len() <= MAX_ARCHIVE_METADATA_BYTES as u64 =>
        {
            serde_json::from_slice::<ArchiveRestoreCompletion>(
                &fs::read(&result_path)
                    .map_err(|_| ApiError::internal("restore result could not be read"))?,
            )
            .map_err(ApiError::from_json)?
        }
        Ok(_) => return Err(ApiError::internal("retained restore result is invalid")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if archive_path.exists() {
                fs::remove_file(&archive_path)
                    .map_err(|_| ApiError::internal("incomplete restore archive cleanup failed"))?;
            }
            return Ok(None);
        }
        Err(_) => return Err(ApiError::internal("restore result could not be read")),
    };
    if completion.plan_id != plan_id || completion.plan_digest != plan.plan_digest {
        return Err(ApiError::conflict(
            "restore_plan_mismatch",
            "The retained restore result belongs to a different signed plan.",
        ));
    }
    let archive_metadata = fs::symlink_metadata(&archive_path)
        .map_err(|_| ApiError::internal("retained restore archive is unavailable"))?;
    if !archive_metadata.is_file() || archive_metadata.file_type().is_symlink() {
        return Err(ApiError::internal("retained restore archive is invalid"));
    }
    let length = archive_metadata.len();
    Ok(Some(
        archive_restore_response(
            &archive_path,
            length,
            &completion.result,
            &completion.plan_id,
            &completion.plan_digest,
        )
        .await?,
    ))
}

async fn archive_restore_response(
    archive_path: &Path,
    length: u64,
    result: &RestoreResponse,
    plan_id: &str,
    plan_digest: &str,
) -> Result<Response, ApiError> {
    let file = tokio::fs::File::open(archive_path)
        .await
        .map_err(|_| ApiError::internal("retained restore archive could not be opened"))?;
    let encoded_result =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(result).map_err(ApiError::from_json)?);
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/vnd.covalent.restore+zip"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"covalent-restore.zip\""),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string())
            .map_err(|_| ApiError::internal("archive length header is invalid"))?,
    );
    response.headers_mut().insert(
        ARCHIVE_RESULT_HEADER,
        HeaderValue::from_str(&encoded_result)
            .map_err(|_| ApiError::internal("archive result header is invalid"))?,
    );
    response
        .headers_mut()
        .insert(JOB_ACK_REQUIRED_HEADER, HeaderValue::from_static("true"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    insert_restore_plan_headers(&mut response, plan_id, plan_digest)?;
    Ok(response)
}

fn discard_job_artifacts(state: &AppState, job_id: &str) -> Result<(), ApiError> {
    state
        .engine
        .discard_job_checkpoint(job_id)
        .map_err(ApiError::from_core)?;
    let archive_backup = state.archive_backup_root.join(job_id);
    remove_private_job_directory(state.archive_backup_root.as_path(), &archive_backup)?;
    let archive_target = state.archive_restore_root.join(job_id);
    match fs::symlink_metadata(&archive_target) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(&archive_target)
                .map_err(|_| ApiError::internal("archive job could not be discarded"))?;
        }
        Ok(_) => return Err(ApiError::internal("archive job staging is invalid")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ApiError::internal("archive job could not be inspected")),
    }
    let _guard = state
        .restore_plan_lock
        .lock()
        .map_err(|_| ApiError::internal("restore plan lock failed"))?;
    let mut removed_plan = false;
    for entry in fs::read_dir(state.restore_plan_root.as_path())
        .map_err(|_| ApiError::internal("restore plan store could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("restore plan entry is invalid"))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| ApiError::internal("restore plan entry could not be inspected"))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_RESTORE_PLAN_BYTES
            || !entry
                .file_name()
                .to_str()
                .is_some_and(valid_plan_identifier)
        {
            return Err(ApiError::internal("restore plan entry is invalid"));
        }
        let plan: RestorePlan = serde_json::from_slice(
            &fs::read(&path).map_err(|_| ApiError::internal("restore plan could not be read"))?,
        )
        .map_err(ApiError::from_json)?;
        if plan.job_id == job_id {
            fs::remove_file(&path)
                .map_err(|_| ApiError::internal("restore plan could not be discarded"))?;
            removed_plan = true;
        }
    }
    if removed_plan {
        File::open(state.restore_plan_root.as_path())
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ApiError::internal("restore plan discard could not be synced"))?;
    }
    Ok(())
}

fn evict_oldest_incomplete_archive_job(state: &AppState, root: &Path) -> Result<bool, ApiError> {
    let mut jobs = state
        .jobs
        .lock()
        .map_err(|_| ApiError::from_core(CoreError::Synchronization))?;
    let mut eviction = None::<(SystemTime, String)>;
    for entry in fs::read_dir(root)
        .map_err(|_| ApiError::internal("archive staging could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("archive staging entry is invalid"))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| ApiError::internal("archive staging entry could not be inspected"))?;
        let job_id = entry
            .file_name()
            .to_str()
            .filter(|job_id| valid_job_identifier(job_id))
            .ok_or_else(|| ApiError::internal("archive staging entry is invalid"))?
            .to_owned();
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || path.join("result.json").exists()
            || job_has_inventory_upload_session(&path)?
            || jobs.entries.get(&job_id).is_some_and(|job| job.active)
        {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        if eviction
            .as_ref()
            .is_none_or(|(oldest, _)| modified < *oldest)
        {
            eviction = Some((modified, job_id));
        }
    }
    let Some((_, job_id)) = eviction else {
        return Ok(false);
    };
    discard_job_artifacts(state, &job_id)?;
    jobs.entries.remove(&job_id);
    Ok(true)
}

fn job_has_inventory_upload_session(job_directory: &Path) -> Result<bool, ApiError> {
    for entry in fs::read_dir(job_directory)
        .map_err(|_| ApiError::internal("archive job could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("archive job entry is invalid"))?;
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("inventory-") && name.ends_with(".session.json"))
        {
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ApiError::internal("inventory session could not be inspected"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ApiError::internal("inventory session is invalid"));
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn create_archive_restore_target(state: &AppState, job_id: &str) -> Result<PathBuf, ApiError> {
    if !valid_job_identifier(job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The restore job ID is invalid.",
        ));
    }
    let _guard = state
        .archive_restore_lock
        .lock()
        .map_err(|_| ApiError::internal("archive restore staging lock failed"))?;
    let job_directory = state.archive_restore_root.join(job_id);
    let job_directory = match fs::symlink_metadata(&job_directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => job_directory,
        Ok(_) => {
            return Err(ApiError::internal(
                "archive restore job directory is invalid",
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_archive_restore_job_unlocked(state, job_id)?
        }
        Err(_) => {
            return Err(ApiError::internal(
                "archive restore staging could not be inspected",
            ));
        }
    };
    let target = job_directory.join("target");
    match fs::create_dir(&target) {
        Ok(()) => create_private_directory(target).map_err(ApiError::from_core),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(ApiError::conflict(
            "job_conflict",
            "This archive restore job ID already has a preview target.",
        )),
        Err(_) => Err(ApiError::internal(
            "archive restore target could not be created",
        )),
    }
}

fn create_or_open_archive_restore_job(state: &AppState, job_id: &str) -> Result<PathBuf, ApiError> {
    if !valid_job_identifier(job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The restore job ID is invalid.",
        ));
    }
    let _guard = state
        .archive_restore_lock
        .lock()
        .map_err(|_| ApiError::internal("archive restore staging lock failed"))?;
    let path = state.archive_restore_root.join(job_id);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(path),
        Ok(_) => Err(ApiError::internal(
            "archive restore job directory is invalid",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create_archive_restore_job_unlocked(state, job_id)
        }
        Err(_) => Err(ApiError::internal(
            "archive restore staging could not be inspected",
        )),
    }
}

fn existing_archive_restore_job(state: &AppState, job_id: &str) -> Result<PathBuf, ApiError> {
    let path = state.archive_restore_root.join(job_id);
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found(
                "target_inventory_not_found",
                "Target inventory upload is unavailable or expired.",
            )
        } else {
            ApiError::internal("archive restore job directory could not be inspected")
        }
    })?;
    if path.parent() != Some(state.archive_restore_root.as_path())
        || metadata.file_type().is_symlink()
        || !metadata.is_dir()
    {
        return Err(ApiError::internal(
            "archive restore job directory is invalid",
        ));
    }
    Ok(path)
}

fn create_archive_restore_job_unlocked(
    state: &AppState,
    job_id: &str,
) -> Result<PathBuf, ApiError> {
    ensure_retained_archive_capacity(state, 1, 0)?;
    let target_count = fs::read_dir(state.archive_restore_root.as_path())
        .map_err(|_| ApiError::internal("archive restore staging could not be inspected"))?
        .count();
    if target_count >= state.archive_limits.maximum_jobs
        && !evict_oldest_incomplete_archive_job(state, &state.archive_restore_root)?
    {
        return Err(ApiError::payload_too_large(
            "Too many archive restore previews are waiting for execution.",
        ));
    }
    let path = state.archive_restore_root.join(job_id);
    fs::create_dir(&path)
        .map_err(|_| ApiError::internal("archive restore job directory could not be created"))?;
    create_private_directory(path).map_err(ApiError::from_core)
}

fn load_finalized_target_inventory(
    state: &AppState,
    job_id: &str,
    inventory_id: &str,
) -> Result<TargetInventory, ApiError> {
    if !valid_lowercase_digest(inventory_id) {
        return Err(ApiError::bad_request(
            "invalid_target_inventory",
            "Target inventory reference is invalid.",
        ));
    }
    let directory = SafeUploadDirectory::open(&existing_archive_restore_job(state, job_id)?)?;
    let bytes = directory
        .read_private_file(
            &target_inventory_final_name(inventory_id),
            MAX_TARGET_INVENTORY_STAGING_BYTES,
        )?
        .ok_or_else(|| {
            ApiError::not_found(
                "target_inventory_not_found",
                "Finalized target inventory is unavailable or expired.",
            )
        })?;
    let inventory: TargetInventory = serde_json::from_slice(&bytes).map_err(ApiError::from_json)?;
    let digest = canonical_target_inventory_digest(&inventory.root_identity, &inventory.entries)
        .map_err(ApiError::from_core)?;
    if inventory.schema_version != TARGET_INVENTORY_SCHEMA_VERSION
        || inventory.entry_count != inventory.entries.len() as u64
        || inventory.inventory_digest != digest
    {
        return Err(ApiError::internal("finalized target inventory is invalid"));
    }
    Ok(inventory)
}

fn validate_archive_restore_plan(
    state: &AppState,
    plan: &RestorePlan,
) -> Result<PathBuf, ApiError> {
    if !valid_job_identifier(&plan.job_id) {
        return Err(ApiError::bad_request(
            "invalid_job_id",
            "The restore job ID is invalid.",
        ));
    }
    let target = fs::canonicalize(&plan.authorized_root).map_err(|_| {
        ApiError::bad_request(
            "restore_plan_mismatch",
            "The archive restore staging root is unavailable.",
        )
    })?;
    let job_directory = target
        .parent()
        .ok_or_else(|| ApiError::internal("archive restore target has no job directory"))?;
    if job_directory.parent() != Some(state.archive_restore_root.as_path())
        || job_directory.file_name().and_then(|name| name.to_str()) != Some(plan.job_id.as_str())
        || target.file_name().and_then(|name| name.to_str()) != Some("target")
    {
        return Err(ApiError::bad_request(
            "restore_plan_mismatch",
            "Only an archive restore preview can be streamed to a document provider.",
        ));
    }
    Ok(target)
}

fn prepare_external_restore_staging(root: &Path, plan: &RestorePlan) -> Result<(), ApiError> {
    if plan.target_inventory.is_none() {
        return Ok(());
    }
    for entry in &plan.entries {
        let destination = entry
            .destination_path
            .components()
            .fold(root.to_path_buf(), |path, component| path.join(component));
        match entry.action {
            PreviewAction::KeepDirectory => {
                fs::create_dir_all(&destination).map_err(|_| {
                    ApiError::internal("external restore directory adapter could not be staged")
                })?;
            }
            PreviewAction::SkipFile | PreviewAction::ReplaceFile => {
                let parent = destination.parent().ok_or_else(|| {
                    ApiError::internal("external restore file adapter has no parent")
                })?;
                fs::create_dir_all(parent).map_err(|_| {
                    ApiError::internal("external restore parent adapter could not be staged")
                })?;
                let file = File::options()
                    .write(true)
                    .create_new(true)
                    .open(&destination)
                    .map_err(|_| {
                        ApiError::internal("external restore file adapter could not be staged")
                    })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(fs::Permissions::from_mode(0o600))
                        .map_err(|_| {
                            ApiError::internal("external restore adapter could not be protected")
                        })?;
                }
                file.sync_all().map_err(|_| {
                    ApiError::internal("external restore adapter could not be synced")
                })?;
            }
            PreviewAction::CreateFile
            | PreviewAction::CreateDirectory
            | PreviewAction::RenameFile => {}
        }
    }
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ApiError::internal("external restore adapter could not be committed"))
}

fn zip_restore_directory(
    root: &Path,
    result_path: &Path,
    control: &JobControl,
    limits: ArchiveLimits,
    plan: &RestorePlan,
) -> Result<u64, ApiError> {
    let planned_actions: BTreeMap<_, _> = plan
        .entries
        .iter()
        .map(|entry| (entry.destination_path.clone(), entry.action))
        .collect();
    let started = Instant::now();
    let mut entry_count = 0_usize;
    let mut total_uncompressed = 0_u64;
    for entry in WalkDir::new(root).follow_links(false) {
        check_archive_control(control, started)?;
        let entry =
            entry.map_err(|_| ApiError::internal("restored entry could not be inspected"))?;
        if entry.path() == root {
            continue;
        }
        entry_count = entry_count.saturating_add(1);
        if entry_count > limits.maximum_entries || entry.file_type().is_symlink() {
            return Err(ApiError::payload_too_large(
                "Restored content exceeds archive output limits.",
            ));
        }
        if entry.file_type().is_file() {
            total_uncompressed = total_uncompressed
                .checked_add(
                    entry
                        .metadata()
                        .map_err(|_| ApiError::internal("restored file metadata is unavailable"))?
                        .len(),
                )
                .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
            if total_uncompressed > limits.maximum_uncompressed_bytes {
                return Err(ApiError::payload_too_large(
                    "Restored content exceeds the archive output limit.",
                ));
            }
        } else if !entry.file_type().is_dir() {
            return Err(ApiError::internal(
                "restore staging contained an unsupported entry",
            ));
        }
    }
    ensure_archive_capacity(root, total_uncompressed, limits.free_space_reserve_bytes)?;
    let temporary_path = result_path.with_extension("part");
    let archive = File::create(&temporary_path)
        .map_err(|_| ApiError::internal("restore archive file could not be created"))?;
    let mut writer = ZipWriter::new(archive);
    let file_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(true)
        .unix_permissions(0o600);
    let directory_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o700);
    let entries = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    let mut total_processed = 0_u64;
    for entry in entries {
        check_archive_progress(control, started, total_processed)?;
        let entry =
            entry.map_err(|_| ApiError::internal("restored entry could not be inspected"))?;
        if entry.path() == root {
            continue;
        }
        if entry.file_type().is_symlink() {
            return Err(ApiError::internal(
                "restore staging unexpectedly contained a symbolic link",
            ));
        }
        let relative_path = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| ApiError::internal("restore staging path escaped its root"))?;
        let relative_string = relative_path
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| ApiError::internal("restore path was not UTF-8"))?
            .join("/");
        let relative = RelativePath::new(relative_string)
            .map_err(|_| ApiError::internal("restore path was not canonical"))?;
        let action = planned_actions
            .get(&relative)
            .copied()
            .ok_or_else(|| ApiError::internal("restore staging contained an unplanned entry"))?;
        if matches!(
            action,
            PreviewAction::SkipFile | PreviewAction::KeepDirectory
        ) {
            continue;
        }
        if entry.file_type().is_dir() {
            writer
                .add_directory(format!("{relative}/"), directory_options)
                .map_err(|_| ApiError::internal("restore directory could not be archived"))?;
        } else if entry.file_type().is_file() {
            writer
                .start_file(relative.to_string(), file_options)
                .map_err(|_| ApiError::internal("restore file could not be archived"))?;
            let mut input = File::open(entry.path())
                .map_err(|_| ApiError::internal("restored file could not be opened"))?;
            copy_archive_bytes(
                &mut input,
                &mut writer,
                control,
                started,
                &mut total_processed,
                limits.maximum_uncompressed_bytes,
                false,
            )?;
        } else {
            return Err(ApiError::internal(
                "restore staging contained an unsupported entry",
            ));
        }
    }
    check_archive_control(control, started)?;
    let archive = writer
        .finish()
        .map_err(|_| ApiError::internal("restore archive could not be finalized"))?;
    archive
        .sync_all()
        .map_err(|_| ApiError::internal("restore archive could not be synced"))?;
    let length = archive
        .metadata()
        .map_err(|_| ApiError::internal("restore archive size is unavailable"))?
        .len();
    if length > limits.maximum_compressed_bytes {
        return Err(ApiError::payload_too_large(
            "Restore archive exceeds the compressed output limit.",
        ));
    }
    drop(archive);
    check_archive_control(control, started)?;
    fs::rename(&temporary_path, result_path)
        .map_err(|_| ApiError::internal("restore archive could not be committed"))?;
    let parent = result_path
        .parent()
        .ok_or_else(|| ApiError::internal("restore archive has no parent"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ApiError::internal("restore archive commit could not be synced"))?;
    Ok(length)
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if constant_time_equal(supplied.as_bytes(), state.api_token.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::UNAUTHORIZED,
            code: "authentication_required",
            message: "A valid local API token is required.",
            retryable: false,
            upload_offset: None,
        })
    }
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let maximum = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..maximum {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn now_unix_ms() -> u64 {
    unix_ms(SystemTime::now())
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
    retryable: bool,
    upload_offset: Option<u64>,
}

impl ApiError {
    fn from_core(error: CoreError) -> Self {
        match error {
            CoreError::InvalidAuthorizedRoot(_) => Self::bad_request(
                "invalid_authorized_root",
                "The selected source or destination is not an accessible directory.",
            ),
            CoreError::SymlinkTraversal(_)
            | CoreError::NonDirectoryAncestor(_)
            | CoreError::EscapedAuthorizedRoot(_) => Self::bad_request(
                "unsafe_restore_path",
                "The restore path cannot be confined beneath the authorized destination.",
            ),
            CoreError::SettingsImportNotConfirmed | CoreError::PairingNotConfirmed => Self {
                status: StatusCode::CONFLICT,
                code: "confirmation_required",
                message: "Explicit local confirmation is required.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::Paused => Self {
                status: StatusCode::CONFLICT,
                code: "job_paused",
                message: "The job is paused and can be resumed with the same job ID.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::Cancelled => Self {
                status: StatusCode::CONFLICT,
                code: "job_cancelled",
                message: "The job was cancelled and its checkpoint was discarded.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::RestoreConflict(_) => Self {
                status: StatusCode::CONFLICT,
                code: "restore_conflict",
                message: "The restore preview found a destination conflict.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::RestorePlanMismatch => Self::conflict(
                "restore_plan_mismatch",
                "The restore plan changed after preview. Preview the restore again.",
            ),
            CoreError::InvitationUnavailable => Self {
                status: StatusCode::GONE,
                code: "invitation_unavailable",
                message: "The pairing invitation is invalid, expired, or already used.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::ProtocolNegotiationFailed => Self::conflict(
                "protocol_incompatible",
                "The devices do not share a supported protocol version.",
            ),
            CoreError::SourceChanged(_) => Self {
                status: StatusCode::CONFLICT,
                code: "source_changed",
                message: "The source changed while it was being backed up. Retry after writes stop.",
                retryable: true,
                upload_offset: None,
            },
            CoreError::UnsupportedSourceEntry(_) | CoreError::SourcePermissionDenied(_) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "source_unreadable",
                message: "The source contains an unsupported or unreadable entry.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "backup_corrupt",
                message: "Backup data failed authenticated integrity verification.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::MissingChunk(_) | CoreError::ProvidersExhausted(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "backup_unavailable",
                message: "No intact authorized copy is currently available.",
                retryable: true,
                upload_offset: None,
            },
            CoreError::ResourceLimit(_) | CoreError::SettingsTooLarge => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "resource_limit",
                message: "The request exceeded a configured resource limit.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::PeerRevoked
            | CoreError::UnselectedProvider
            | CoreError::IdentityMismatch => Self {
                status: StatusCode::FORBIDDEN,
                code: "not_authorized",
                message: "The requested peer or provider is not authorized.",
                retryable: false,
                upload_offset: None,
            },
            CoreError::InvalidKeyMaterial
            | CoreError::UnsupportedCipherSuite(_)
            | CoreError::InvalidState(_)
            | CoreError::InvalidLocator
            | CoreError::Contract(_)
            | CoreError::Json(_) => Self::bad_request(
                "invalid_contract",
                "The request does not satisfy the versioned protocol contract.",
            ),
            CoreError::StateLocked => Self {
                status: StatusCode::LOCKED,
                code: "node_state_locked",
                message: "Another Covalent process currently owns this node state.",
                retryable: true,
                upload_offset: None,
            },
            _ => Self::internal("The local engine could not complete the request."),
        }
    }

    fn from_json_rejection(error: JsonRejection) -> Self {
        match error.status() {
            StatusCode::PAYLOAD_TOO_LARGE => {
                Self::payload_too_large("The JSON request exceeds the 2 MiB API limit.")
            }
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                code: "invalid_content_type",
                message: "JSON requests require Content-Type: application/json.",
                retryable: false,
                upload_offset: None,
            },
            _ => Self::from_json_contract(),
        }
    }

    const fn from_json_contract() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: "The request does not match the versioned contract.",
            retryable: false,
            upload_offset: None,
        }
    }

    fn from_json(_error: serde_json::Error) -> Self {
        Self::from_json_contract()
    }

    const fn bad_request(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message,
            retryable: false,
            upload_offset: None,
        }
    }

    const fn conflict(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message,
            retryable: false,
            upload_offset: None,
        }
    }

    const fn not_found(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message,
            retryable: false,
            upload_offset: None,
        }
    }

    const fn payload_too_large(message: &'static str) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "resource_limit",
            message,
            retryable: false,
            upload_offset: None,
        }
    }

    const fn too_many_requests(message: &'static str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "node_busy",
            message,
            retryable: true,
            upload_offset: None,
        }
    }

    const fn internal(message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message,
            retryable: true,
            upload_offset: None,
        }
    }

    const fn insufficient_storage(message: &'static str) -> Self {
        Self {
            status: StatusCode::INSUFFICIENT_STORAGE,
            code: "insufficient_storage",
            message,
            retryable: true,
            upload_offset: None,
        }
    }

    const fn unprocessable(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message,
            retryable: false,
            upload_offset: None,
        }
    }

    const fn upload_offset(
        status: StatusCode,
        code: &'static str,
        message: &'static str,
        upload_offset: u64,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            code,
            message,
            retryable,
            upload_offset: Some(upload_offset),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry_after = self.status == StatusCode::TOO_MANY_REQUESTS;
        let mut response = (
            self.status,
            axum::Json(ApiErrorBody {
                protocol_version: PROTOCOL_VERSION,
                code: self.code.to_owned(),
                message: self.message.to_owned(),
                retryable: self.retryable,
            }),
        )
            .into_response();
        if retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if let Some(offset) = self.upload_offset
            && let Ok(value) = HeaderValue::from_str(&offset.to_string())
        {
            response
                .headers_mut()
                .insert(ARCHIVE_UPLOAD_OFFSET_HEADER, value);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Read as _, Write as _};

    use axum::body::Body;
    use axum::http::Request;
    use covalent_core::EngineOptions;
    use http_body_util::BodyExt;
    use tempfile::TempDir;
    use tower::ServiceExt;

    use super::*;

    const TEST_TOKEN: &str = "test-local-api-token-with-at-least-32-bytes";

    fn test_state(directory: &TempDir) -> AppState {
        let engine =
            Arc::new(Engine::open(EngineOptions::new(directory.path())).expect("test engine"));
        AppState::new(engine, PlatformTier::Tier1, TEST_TOKEN.to_owned()).expect("state")
    }

    fn upload_sha256(bytes: &[u8]) -> String {
        lowercase_hex(&Sha256::digest(bytes))
    }

    async fn assert_contract_error(
        response: Response,
        status: StatusCode,
        code: &str,
        retryable: bool,
    ) {
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("error body")
            .to_bytes();
        let body: ApiErrorBody = serde_json::from_slice(&bytes).expect("versioned API error");
        assert_eq!(body.protocol_version, PROTOCOL_VERSION);
        assert_eq!(body.code, code);
        assert_eq!(body.retryable, retryable);
        assert!(!body.message.is_empty());
    }

    #[test]
    fn app_owned_readiness_record_is_private_atomic_and_pid_scoped() {
        let directory = TempDir::new().expect("directory");
        let path = directory.path().join("ready/node-ready.json");
        let info = NodeReadyInfo {
            schema_version: 1,
            api_base_url: "http://127.0.0.1:49152".to_owned(),
            peer_address: "0.0.0.0:49153".parse().expect("peer address"),
            process_id: 42,
        };
        write_node_ready_file(&path, &info).expect("write ready record");
        let decoded: NodeReadyInfo =
            serde_json::from_slice(&fs::read(&path).expect("read ready record"))
                .expect("decode ready record");
        assert_eq!(decoded, info);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).expect("metadata").permissions().mode() & 0o777,
                0o600
            );
        }
        remove_node_ready_file(&path, 43).expect("ignore another owner");
        assert!(path.exists());
        remove_node_ready_file(&path, 42).expect("remove matching owner");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn every_router_rejection_uses_the_versioned_error_contract() {
        let directory = TempDir::new().expect("directory");
        let app = router(test_state(&directory));
        let invalid_json = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/import")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("invalid JSON response");
        assert_contract_error(invalid_json, StatusCode::BAD_REQUEST, "invalid_json", false).await;

        let wrong_content_type = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/import")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("content type response");
        assert_contract_error(
            wrong_content_type,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "invalid_content_type",
            false,
        )
        .await;

        let oversized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/import")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(vec![b' '; MAX_LOCAL_API_BODY_BYTES + 1]))
                    .expect("request"),
            )
            .await
            .expect("oversized response");
        assert_contract_error(
            oversized,
            StatusCode::PAYLOAD_TOO_LARGE,
            "resource_limit",
            false,
        )
        .await;

        let wrong_method = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/config/export")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("wrong method response");
        assert_contract_error(
            wrong_method,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            false,
        )
        .await;

        let unknown_route = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-real")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("unknown route response");
        assert_contract_error(
            unknown_route,
            StatusCode::NOT_FOUND,
            "route_not_found",
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn health_is_machine_readable() {
        let directory = TempDir::new().expect("directory");
        let response = router(test_state(&directory))
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn console_has_accessible_landmarks() {
        let directory = TempDir::new().expect("directory");
        let response = router(test_state(&directory))
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let html = String::from_utf8(bytes.to_vec()).expect("UTF-8");
        assert!(html.contains("<main"));
        assert!(html.contains("aria-live"));
    }

    #[tokio::test]
    async fn console_scripts_are_served_as_non_stale_javascript() {
        let directory = TempDir::new().expect("directory");
        let app = router(test_state(&directory));
        for path in [
            "/assets/app.js",
            "/assets/pairing-flow.js",
            "/assets/restore-plan-flow.js",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/javascript; charset=utf-8",
                "{path}"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-cache",
                "{path}"
            );
            assert!(
                !response
                    .into_body()
                    .collect()
                    .await
                    .expect("script body")
                    .to_bytes()
                    .is_empty(),
                "{path}"
            );
        }
    }

    #[test]
    fn cleartext_bearer_api_is_loopback_only() {
        assert!(validate_cleartext_api_bind("127.0.0.1:8787".parse().expect("loopback")).is_ok());
        assert!(validate_cleartext_api_bind("[::1]:8787".parse().expect("IPv6 loopback")).is_ok());
        assert!(validate_cleartext_api_bind("0.0.0.0:8787".parse().expect("wildcard")).is_err());
        assert!(validate_cleartext_api_bind("192.0.2.1:8787".parse().expect("network")).is_err());
    }

    #[tokio::test]
    async fn mutation_api_requires_bearer_token() {
        let directory = TempDir::new().expect("directory");
        let app = router(test_state(&directory));
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/export")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let authorized = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/config/export")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(authorized.status(), StatusCode::OK);
        assert_eq!(
            authorized.headers()[header::CONTENT_TYPE],
            "application/json"
        );
    }

    #[tokio::test]
    async fn settings_import_reconfigures_live_discovery_without_restart() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let engine = state.engine();
        let controller = Arc::new(
            discovery::DiscoveryController::new(false, 4433).expect("discovery controller"),
        );
        let app = router(state.with_discovery_controller(Arc::clone(&controller)));

        for enabled in [true, false] {
            let mut settings: serde_json::Value =
                serde_json::from_slice(&engine.export_settings().expect("export settings"))
                    .expect("settings JSON");
            settings["lanDiscoveryEnabled"] = enabled.into();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/config/import")
                        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({"confirmed": true, "settings": settings})
                                .to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("settings import response");
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert_eq!(
                controller.is_active().expect("live discovery state"),
                enabled
            );
            assert_eq!(
                engine
                    .config()
                    .expect("persisted settings")
                    .lan_discovery_enabled,
                enabled
            );
        }
    }

    #[test]
    fn failed_provider_activation_does_not_mutate_connection_state() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let result = state.connect_provider(ProviderConnection {
            peer_id: DeviceId::new(),
            address: "127.0.0.1:8787".parse().expect("address"),
            certificate_der: URL_SAFE_NO_PAD.encode([0_u8; 32]),
        });
        assert!(matches!(result, Err(CoreError::IdentityMismatch)));
        assert!(
            state
                .provider_connections
                .lock()
                .expect("connections")
                .is_empty()
        );
    }

    #[test]
    fn abandoned_job_lease_pauses_and_ttl_evicts_staging() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let job_id = "abandoned-job";
        let staging = state.archive_backup_root.join(job_id);
        fs::create_dir(&staging).expect("staging directory");
        fs::write(staging.join("upload.part"), b"partial").expect("partial upload");

        let lease = state.start_job(job_id).expect("start job");
        drop(lease);
        {
            let mut jobs = state.jobs.lock().expect("jobs");
            let entry = jobs.entries.get_mut(job_id).expect("retained job");
            assert!(!entry.active);
            assert_eq!(entry.control.state(), JobState::Paused);
            entry.last_touched = UNIX_EPOCH;
        }

        state.prune_expired_jobs().expect("prune expired job");
        assert!(state.jobs.lock().expect("jobs").entries.is_empty());
        assert!(!staging.exists());
    }

    #[test]
    fn full_registry_evicts_the_oldest_inactive_resumable_job() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        for index in 0..=MAX_LOCAL_JOBS {
            let lease = state
                .start_job(&format!("resumable-{index:04}"))
                .expect("admit resumable job");
            drop(lease);
        }
        let jobs = state.jobs.lock().expect("jobs");
        assert_eq!(jobs.entries.len(), MAX_LOCAL_JOBS);
        assert!(!jobs.entries.contains_key("resumable-0000"));
        assert!(
            jobs.entries
                .contains_key(&format!("resumable-{MAX_LOCAL_JOBS:04}"))
        );
    }

    #[tokio::test]
    async fn resumable_jobs_can_be_listed_and_explicitly_discarded() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let lease = state.start_job("paused-job").expect("start job");
        drop(lease);
        let app = router(state.clone());

        let list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/jobs")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("list jobs");
        assert_eq!(list.status(), StatusCode::OK);
        let jobs: serde_json::Value = serde_json::from_slice(
            &list
                .into_body()
                .collect()
                .await
                .expect("list body")
                .to_bytes(),
        )
        .expect("jobs JSON");
        assert_eq!(jobs[0]["jobId"], "paused-job");
        assert_eq!(jobs[0]["state"], "paused");
        assert_eq!(jobs[0]["active"], false);

        let discard = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs/discard")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"jobId":"paused-job"}"#))
                    .expect("request"),
            )
            .await
            .expect("discard job");
        assert_eq!(discard.status(), StatusCode::NO_CONTENT);
        assert!(state.jobs.lock().expect("jobs").entries.is_empty());

        let unknown = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/jobs/control")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"jobId":"not-registered","action":"pause"}"#))
                    .expect("request"),
            )
            .await
            .expect("unknown control");
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn terminal_failures_do_not_exhaust_the_job_registry() {
        let directory = TempDir::new().expect("directory");
        let valid_source = TempDir::new().expect("valid source");
        fs::write(valid_source.path().join("content.txt"), b"still admitted")
            .expect("source content");
        let state = test_state(&directory);
        let jobs = Arc::clone(&state.jobs);
        let app = router(state);
        for index in 0..=MAX_LOCAL_JOBS {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/backups")
                        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({
                                "sourceRoot": directory.path().join("missing"),
                                "displayName": "Invalid source",
                                "snapshotId": format!("failed-snapshot-{index}"),
                                "jobId": format!("failed-job-{index}"),
                                "selectedProviderIds": []
                            })
                            .to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("terminal response");
            assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        }
        assert!(jobs.lock().expect("jobs").entries.is_empty());

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sourceRoot": valid_source.path(),
                            "displayName": "Valid source",
                            "snapshotId": "valid-after-terminal-failures",
                            "jobId": "valid-after-terminal-failures",
                            "selectedProviderIds": []
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("valid response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn concurrent_storage_jobs_are_backpressured_with_retry_guidance() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let _active_job = state.admit_engine_job().expect("first admission");
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sourceRoot": directory.path(),
                            "displayName": "Backpressured",
                            "snapshotId": "backpressured-snapshot",
                            "jobId": "backpressured-job",
                            "selectedProviderIds": []
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.headers()[header::RETRY_AFTER], "1");
        assert_contract_error(response, StatusCode::TOO_MANY_REQUESTS, "node_busy", true).await;
    }

    #[test]
    fn archive_capacity_evicts_incomplete_jobs_but_retains_acknowledgeable_results() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory)
            .with_archive_limits(ArchiveLimits {
                maximum_jobs: 1,
                maximum_retained_results: 1,
                ..ArchiveLimits::default()
            })
            .expect("archive limits");
        let metadata = |job_id: &str| ArchiveBackupMetadata {
            protocol_version: PROTOCOL_VERSION,
            backup_id: None,
            display_name: "Capacity".to_owned(),
            snapshot_id: format!("{job_id}-snapshot"),
            job_id: job_id.to_owned(),
            selected_provider_ids: Vec::new(),
        };

        let first = prepare_archive_backup_job(&state, &metadata("first-job"))
            .expect("first staging")
            .0;
        fs::write(first.join("upload.part"), b"partial").expect("partial upload");
        let second = prepare_archive_backup_job(&state, &metadata("second-job"))
            .expect("evict first staging")
            .0;
        assert!(!first.exists());
        assert!(second.exists());

        fs::write(
            second.join("result.json"),
            b"retained until acknowledgement",
        )
        .expect("retained result marker");
        let error = prepare_archive_backup_job(&state, &metadata("third-job"))
            .expect_err("completed result must not be evicted");
        assert_eq!(error.status, StatusCode::INSUFFICIENT_STORAGE);
        assert!(second.exists());
    }

    #[tokio::test]
    async fn archive_upload_size_is_rejected_before_body_consumption() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory)
            .with_archive_limits(ArchiveLimits {
                maximum_compressed_bytes: 1 << 20,
                maximum_uncompressed_bytes: 1 << 20,
                free_space_reserve_bytes: 0,
                ..ArchiveLimits::default()
            })
            .expect("archive limits");
        let jobs = Arc::clone(&state.jobs);
        let archive_root = Arc::clone(&state.archive_backup_root);
        let metadata = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "displayName": "Oversized",
                "snapshotId": "oversized-snapshot",
                "jobId": "oversized-job",
                "selectedProviderIds": []
            }))
            .expect("metadata"),
        );
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, metadata)
                    .header(ARCHIVE_UPLOAD_OFFSET_HEADER, "0")
                    .header(
                        ARCHIVE_UPLOAD_LENGTH_HEADER,
                        ((1_u64 << 20) + 1).to_string(),
                    )
                    .header(ARCHIVE_UPLOAD_DIGEST_HEADER, "00".repeat(32))
                    .header(header::CONTENT_LENGTH, ((1_u64 << 20) + 1).to_string())
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_contract_error(
            response,
            StatusCode::PAYLOAD_TOO_LARGE,
            "resource_limit",
            false,
        )
        .await;
        assert!(jobs.lock().expect("jobs").entries.is_empty());
        assert_eq!(
            fs::read_dir(archive_root.as_path())
                .expect("staging")
                .count(),
            0
        );
    }

    #[tokio::test]
    async fn authenticated_api_backs_up_verifies_and_restores_deleted_source() {
        let directory = TempDir::new().expect("directory");
        let source = directory.path().join("source");
        let restore = directory.path().join("restore");
        fs::create_dir_all(source.join("nested/empty")).expect("source directories");
        fs::create_dir_all(&restore).expect("restore directory");
        fs::write(
            source.join("nested/data.bin"),
            b"api vertical slice\0payload",
        )
        .expect("source file");

        let app = router(test_state(&directory));
        let backup_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sourceRoot": source,
                            "displayName": "API E2E",
                            "snapshotId": "api-snapshot-1",
                            "jobId": "api-backup-job",
                            "selectedProviderIds": []
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("backup response");
        assert_eq!(backup_response.status(), StatusCode::OK);
        let backup_json: serde_json::Value = serde_json::from_slice(
            &backup_response
                .into_body()
                .collect()
                .await
                .expect("backup body")
                .to_bytes(),
        )
        .expect("backup JSON");
        let backup_id = backup_json["backupId"].clone();
        assert_eq!(backup_json["snapshotId"], "api-snapshot-1");

        let verify_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/verify")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "backupId": backup_id,
                            "snapshotId": "api-snapshot-1",
                            "verifyProviders": false,
                            "repair": false
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("verify response");
        assert_eq!(verify_response.status(), StatusCode::OK);
        let verify_json: serde_json::Value = serde_json::from_slice(
            &verify_response
                .into_body()
                .collect()
                .await
                .expect("verify body")
                .to_bytes(),
        )
        .expect("verify JSON");
        assert_eq!(verify_json["intact"], true);

        fs::remove_dir_all(&source).expect("delete source");
        let preview_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/preview")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "backupId": backup_id,
                            "snapshotId": "api-snapshot-1",
                            "targetRoot": restore,
                            "conflictPolicy": "fail",
                            "jobId": "api-restore-job"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("preview response");
        let preview_status = preview_response.status();
        let plan_id = preview_response.headers()[RESTORE_PLAN_ID_HEADER]
            .to_str()
            .expect("plan ID")
            .to_owned();
        let preview_bytes = preview_response
            .into_body()
            .collect()
            .await
            .expect("preview body")
            .to_bytes();
        assert_eq!(
            preview_status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&preview_bytes)
        );
        let plan: serde_json::Value = serde_json::from_slice(&preview_bytes).expect("preview JSON");
        let plan_digest = plan["planDigest"].as_str().expect("plan digest").to_owned();
        assert_eq!(plan["planId"], plan_id);
        assert_ne!(plan_digest, plan_id);
        assert!(plan.get("entries").is_none());
        assert!(plan["totalEntries"].as_u64().expect("entry count") > 1);

        let page_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/restores/plans/{plan_id}?limit=1"))
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("plan page response");
        assert_eq!(page_response.status(), StatusCode::OK);
        let page: serde_json::Value = serde_json::from_slice(
            &page_response
                .into_body()
                .collect()
                .await
                .expect("plan page")
                .to_bytes(),
        )
        .expect("plan page JSON");
        assert_eq!(page["planId"], plan_id);
        assert_eq!(page["planDigest"], plan_digest);
        assert_eq!(page["entryOffset"], 0);
        assert_eq!(page["entries"].as_array().expect("entries").len(), 1);
        let next_cursor = page["nextCursor"]
            .as_str()
            .expect("opaque next cursor")
            .to_owned();

        let second_page = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/v1/restores/plans/{plan_id}?cursor={next_cursor}&limit=1"
                    ))
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("second plan page response");
        assert_eq!(second_page.status(), StatusCode::OK);

        let restore_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "planId": plan_id.clone() }).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("restore response");
        assert_eq!(restore_response.status(), StatusCode::OK);
        assert_eq!(
            restore_response.headers()[RESTORE_PLAN_ID_HEADER],
            plan_id.as_str()
        );
        assert_eq!(
            restore_response.headers()[RESTORE_PLAN_DIGEST_HEADER],
            plan_digest.as_str()
        );
        assert_eq!(
            fs::read(restore.join("nested/data.bin")).expect("restored file"),
            b"api vertical slice\0payload"
        );
        assert!(restore.join("nested/empty").is_dir());
    }

    #[tokio::test]
    async fn interrupted_archive_upload_resumes_from_fsynced_offset_and_compacts_staging() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory)
            .with_archive_limits(ArchiveLimits {
                maximum_compressed_bytes: 8 << 20,
                maximum_uncompressed_bytes: 16 << 20,
                maximum_staging_bytes: 40 << 20,
                maximum_retained_result_bytes: 8 << 20,
                maximum_retained_results: 4,
                free_space_reserve_bytes: 0,
                ..ArchiveLimits::default()
            })
            .expect("archive limits");
        let archive_root = Arc::clone(&state.archive_backup_root);
        let app = router(state);
        let payload = vec![0xa5_u8; 4 * 1_024 * 1_024];
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "payload.bin",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("archive entry");
        writer.write_all(&payload).expect("archive payload");
        let archive = writer.finish().expect("archive").into_inner();
        let digest = upload_sha256(&archive);
        let split = archive.len() / 3;
        let metadata = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "displayName": "Interrupted upload",
                "snapshotId": "interrupted-snapshot",
                "jobId": "interrupted-upload-job",
                "selectedProviderIds": []
            }))
            .expect("metadata"),
        );

        let interrupted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, &metadata)
                    .header(ARCHIVE_UPLOAD_OFFSET_HEADER, "0")
                    .header(ARCHIVE_UPLOAD_LENGTH_HEADER, archive.len().to_string())
                    .header(ARCHIVE_UPLOAD_DIGEST_HEADER, &digest)
                    .body(Body::from(archive[..split].to_vec()))
                    .expect("partial request"),
            )
            .await
            .expect("partial response");
        assert_eq!(interrupted.status(), StatusCode::CONFLICT);
        assert_eq!(
            interrupted.headers()[ARCHIVE_UPLOAD_OFFSET_HEADER],
            split.to_string()
        );
        let partial_usage = archive_tree_bytes(archive_root.as_path()).expect("partial usage");
        assert!(partial_usage >= u64::try_from(split).expect("split length"));
        assert!(partial_usage < u64::try_from(split).expect("split length") + 64 * 1_024);
        assert_contract_error(interrupted, StatusCode::CONFLICT, "upload_incomplete", true).await;

        let wrong_identity = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, &metadata)
                    .header(ARCHIVE_UPLOAD_OFFSET_HEADER, split.to_string())
                    .header(ARCHIVE_UPLOAD_LENGTH_HEADER, archive.len().to_string())
                    .header(ARCHIVE_UPLOAD_DIGEST_HEADER, "11".repeat(32))
                    .body(Body::from(archive[split..].to_vec()))
                    .expect("wrong identity request"),
            )
            .await
            .expect("wrong identity response");
        assert_eq!(wrong_identity.status(), StatusCode::CONFLICT);
        assert_eq!(
            wrong_identity.headers()[ARCHIVE_UPLOAD_OFFSET_HEADER],
            split.to_string()
        );
        assert_contract_error(
            wrong_identity,
            StatusCode::CONFLICT,
            "upload_identity_mismatch",
            false,
        )
        .await;

        let completed = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, metadata)
                    .header(ARCHIVE_UPLOAD_OFFSET_HEADER, split.to_string())
                    .header(ARCHIVE_UPLOAD_LENGTH_HEADER, archive.len().to_string())
                    .header(ARCHIVE_UPLOAD_DIGEST_HEADER, digest)
                    .body(Body::from(archive[split..].to_vec()))
                    .expect("resume request"),
            )
            .await
            .expect("resume response");
        assert_eq!(completed.status(), StatusCode::OK);
        assert_eq!(completed.headers()[JOB_ACK_REQUIRED_HEADER], "true");
        let job_directory = archive_root.join("interrupted-upload-job");
        assert!(job_directory.join("result.json").is_file());
        assert!(!job_directory.join("upload.part").exists());
        assert!(!job_directory.join("upload-session.json").exists());
        assert!(!job_directory.join("source").exists());
        assert!(
            archive_tree_bytes(&job_directory).expect("retained usage") < 64 * 1_024,
            "completed backup retains only bounded metadata and result"
        );
    }

    #[tokio::test]
    async fn streamed_archive_bridge_never_requires_a_daemon_visible_android_path() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let archive_root = state.archive_restore_root.clone();
        let archive_backup_root = state.archive_backup_root.clone();
        let app = router(state);
        let expected = vec![0x5a_u8; 3 * 1_024 * 1_024];
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        archive
            .add_directory("Documents/", SimpleFileOptions::default())
            .expect("directory entry");
        archive
            .start_file(
                "Documents/large.bin",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("file entry");
        archive.write_all(&expected).expect("archive payload");
        let archive = archive.finish().expect("archive").into_inner();
        assert!(archive.len() > MAX_LOCAL_API_BODY_BYTES);

        let metadata = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "displayName": "Android SAF E2E",
                "snapshotId": "android-saf-snapshot-1",
                "jobId": "android-saf-backup-job",
                "selectedProviderIds": []
            }))
            .expect("metadata"),
        );
        let upload_digest = upload_sha256(&archive);
        let backup_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, metadata.clone())
                    .header(ARCHIVE_UPLOAD_OFFSET_HEADER, "0")
                    .header(ARCHIVE_UPLOAD_LENGTH_HEADER, archive.len().to_string())
                    .header(ARCHIVE_UPLOAD_DIGEST_HEADER, &upload_digest)
                    .body(Body::from(archive.clone()))
                    .expect("request"),
            )
            .await
            .expect("backup response");
        let status = backup_response.status();
        assert_eq!(backup_response.headers()[JOB_ACK_REQUIRED_HEADER], "true");
        let backup_bytes = backup_response
            .into_body()
            .collect()
            .await
            .expect("backup body")
            .to_bytes();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&backup_bytes)
        );
        let backup: serde_json::Value = serde_json::from_slice(&backup_bytes).expect("backup JSON");

        let backup_retry = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, metadata)
                    .body(Body::from(archive))
                    .expect("retry request"),
            )
            .await
            .expect("backup retry response");
        assert_eq!(backup_retry.status(), StatusCode::OK);
        assert_eq!(backup_retry.headers()[JOB_ACK_REQUIRED_HEADER], "true");
        assert_eq!(
            backup_retry
                .into_body()
                .collect()
                .await
                .expect("backup retry body")
                .to_bytes(),
            backup_bytes
        );

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/backups")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("list response");
        assert_eq!(list_response.status(), StatusCode::OK);
        let listed: serde_json::Value = serde_json::from_slice(
            &list_response
                .into_body()
                .collect()
                .await
                .expect("list body")
                .to_bytes(),
        )
        .expect("list JSON");
        assert_eq!(listed[0]["latestSnapshotId"], "android-saf-snapshot-1");
        assert_eq!(listed[0]["snapshotCount"], 1);

        let preview_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/archive/preview")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "backupId": backup["backupId"],
                            "snapshotId": "android-saf-snapshot-1",
                            "conflictPolicy": "fail",
                            "jobId": "android-saf-restore-job"
                        })
                        .to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("preview response");
        assert_eq!(preview_response.status(), StatusCode::OK);
        let plan_id = preview_response.headers()[RESTORE_PLAN_ID_HEADER]
            .to_str()
            .expect("plan ID")
            .to_owned();
        let plan: serde_json::Value = serde_json::from_slice(
            &preview_response
                .into_body()
                .collect()
                .await
                .expect("preview body")
                .to_bytes(),
        )
        .expect("preview JSON");
        let plan_digest = plan["planDigest"].as_str().expect("plan digest").to_owned();
        assert_eq!(plan["planId"], plan_id);
        assert_ne!(plan_digest, plan_id);
        assert!(
            !plan["authorizedRoot"]
                .as_str()
                .expect("authorized root")
                .starts_with("content://")
        );

        let restore_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/archive/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "planId": plan_id.clone() }).to_string(),
                    ))
                    .expect("request"),
            )
            .await
            .expect("restore response");
        assert_eq!(restore_response.status(), StatusCode::OK);
        assert!(
            restore_response
                .headers()
                .contains_key(ARCHIVE_RESULT_HEADER)
        );
        assert_eq!(restore_response.headers()[JOB_ACK_REQUIRED_HEADER], "true");
        assert_eq!(
            restore_response.headers()[RESTORE_PLAN_ID_HEADER],
            plan_id.as_str()
        );
        assert_eq!(
            restore_response.headers()[RESTORE_PLAN_DIGEST_HEADER],
            plan_digest.as_str()
        );
        let restored_archive = restore_response
            .into_body()
            .collect()
            .await
            .expect("restore archive")
            .to_bytes();

        let restore_retry = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/archive/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "planId": plan_id.clone() }).to_string(),
                    ))
                    .expect("retry request"),
            )
            .await
            .expect("restore retry response");
        assert_eq!(restore_retry.status(), StatusCode::OK);
        assert_eq!(restore_retry.headers()[JOB_ACK_REQUIRED_HEADER], "true");
        assert_eq!(
            restore_retry.headers()[RESTORE_PLAN_ID_HEADER],
            plan_id.as_str()
        );
        assert_eq!(
            restore_retry.headers()[RESTORE_PLAN_DIGEST_HEADER],
            plan_digest.as_str()
        );
        let restored_archive_retry = restore_retry
            .into_body()
            .collect()
            .await
            .expect("restore retry archive")
            .to_bytes();
        assert_eq!(restored_archive_retry, restored_archive);

        let mut restored = ZipArchive::new(Cursor::new(restored_archive)).expect("restore ZIP");
        let mut file = restored
            .by_name("Documents/large.bin")
            .expect("restored entry");
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).expect("restored content");
        assert_eq!(actual, expected);
        assert!(
            archive_root
                .join("android-saf-restore-job/result.zip")
                .is_file()
        );
        assert!(
            archive_backup_root
                .join("android-saf-backup-job/result.json")
                .is_file()
        );

        for job_id in ["android-saf-restore-job", "android-saf-backup-job"] {
            let acknowledge = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/jobs/acknowledge")
                        .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(
                            serde_json::json!({ "jobId": job_id }).to_string(),
                        ))
                        .expect("request"),
                )
                .await
                .expect("acknowledge response");
            assert_eq!(acknowledge.status(), StatusCode::NO_CONTENT);
        }
        assert!(!archive_root.join("android-saf-restore-job").exists());
        assert!(!archive_backup_root.join("android-saf-backup-job").exists());
    }

    #[tokio::test]
    async fn paged_target_inventory_is_ordered_crash_durable_and_exceeds_json_limit() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TEST_TOKEN}")).expect("authorization"),
        );
        let total_entries = MAX_TARGET_INVENTORY_ENTRIES;
        let started = begin_target_inventory(
            State(state.clone()),
            headers.clone(),
            ContractJson(BeginTargetInventoryRequest {
                job_id: "large-inventory-job".to_owned(),
                schema_version: TARGET_INVENTORY_SCHEMA_VERSION,
                root_identity: "powerbox-root-dev-1-ino-2".to_owned(),
                entry_count: total_entries,
                total_bytes: 0,
            }),
        )
        .await
        .expect("begin inventory")
        .0;
        let inventory_id = started.inventory_id;

        let first_entry = TargetInventoryEntry {
            path: RelativePath::new("entry-000000".to_owned()).expect("path"),
            kind: EntryKind::File,
            length: 0,
            modified_at_unix_ms: Some(1),
            identity_token: "dev=1;ino=1".to_owned(),
        };
        let out_of_order = append_target_inventory_page(
            State(state.clone()),
            headers.clone(),
            AxumPath(inventory_id.clone()),
            ContractJson(TargetInventoryPageRequest {
                job_id: "large-inventory-job".to_owned(),
                offset: 1,
                page_digest: target_inventory_page_digest(std::slice::from_ref(&first_entry)),
                entries: vec![first_entry.clone()],
            }),
        )
        .await
        .expect("offset response");
        assert_eq!(out_of_order.status(), StatusCode::CONFLICT);
        assert_eq!(out_of_order.headers()[TARGET_INVENTORY_OFFSET_HEADER], "0");

        let tampered = match append_target_inventory_page(
            State(state.clone()),
            headers.clone(),
            AxumPath(inventory_id.clone()),
            ContractJson(TargetInventoryPageRequest {
                job_id: "large-inventory-job".to_owned(),
                offset: 0,
                page_digest: "0".repeat(64),
                entries: vec![first_entry],
            }),
        )
        .await
        {
            Ok(_) => panic!("tampered target inventory page was accepted"),
            Err(error) => error,
        };
        assert_eq!(tampered.code, "target_inventory_page_mismatch");

        // Simulate a daemon process boundary after the durable upload session is fsynced.
        drop(state);
        let state = test_state(&directory);
        let page_size = 10_000_u64;
        let mut offset = 0_u64;
        while offset < total_entries {
            let end = (offset + page_size).min(total_entries);
            let entries: Vec<_> = (offset..end)
                .map(|index| TargetInventoryEntry {
                    path: RelativePath::new(format!("entry-{index:06}")).expect("inventory path"),
                    kind: EntryKind::File,
                    length: 0,
                    modified_at_unix_ms: Some(index),
                    identity_token: format!("dev=1;ino={index}"),
                })
                .collect();
            let response = append_target_inventory_page(
                State(state.clone()),
                headers.clone(),
                AxumPath(inventory_id.clone()),
                ContractJson(TargetInventoryPageRequest {
                    job_id: "large-inventory-job".to_owned(),
                    offset,
                    page_digest: target_inventory_page_digest(&entries),
                    entries,
                }),
            )
            .await
            .expect("append inventory page");
            assert_eq!(response.status(), StatusCode::OK);
            offset = end;
        }
        let job_directory = state.archive_restore_root.join("large-inventory-job");
        assert!(
            fs::metadata(job_directory.join(target_inventory_entries_name(&inventory_id)))
                .expect("inventory pages")
                .len()
                > MAX_LOCAL_API_BODY_BYTES as u64
        );
        let finalized = finalize_target_inventory(
            State(state),
            headers,
            AxumPath(inventory_id.clone()),
            ContractJson(FinalizeTargetInventoryRequest {
                job_id: "large-inventory-job".to_owned(),
                entry_count: total_entries,
                total_bytes: 0,
                inventory_digest: String::new(),
            }),
        )
        .await
        .expect("finalize inventory")
        .0;
        assert_eq!(finalized.inventory_id, inventory_id);
        assert_eq!(finalized.entry_count, total_entries);
        assert!(valid_lowercase_digest(&finalized.inventory_digest));
        assert!(
            !job_directory
                .join(target_inventory_session_name(&inventory_id))
                .exists()
        );
        assert!(
            !job_directory
                .join(target_inventory_entries_name(&inventory_id))
                .exists()
        );
        assert!(
            job_directory
                .join(target_inventory_final_name(&inventory_id))
                .is_file()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn archive_upload_rejects_session_part_and_final_symlink_substitution() {
        use std::os::unix::fs::symlink;

        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory);
        let payload = b"safe archive bytes";
        let digest = upload_sha256(payload);
        let mut headers = HeaderMap::new();
        headers.insert(ARCHIVE_UPLOAD_OFFSET_HEADER, HeaderValue::from_static("0"));
        headers.insert(
            ARCHIVE_UPLOAD_LENGTH_HEADER,
            HeaderValue::from_str(&payload.len().to_string()).expect("length"),
        );
        headers.insert(
            ARCHIVE_UPLOAD_DIGEST_HEADER,
            HeaderValue::from_str(&digest).expect("digest"),
        );
        let outside = directory.path().join("outside");
        fs::write(&outside, b"must remain unchanged").expect("outside");

        for (index, artifact) in [
            "upload-session.json".to_owned(),
            "upload.part".to_owned(),
            format!("upload-{digest}.zip"),
        ]
        .into_iter()
        .enumerate()
        {
            let job_directory = state
                .archive_backup_root
                .join(format!("symlink-job-{index}"));
            create_private_directory(job_directory.clone()).expect("job directory");
            symlink(&outside, job_directory.join(artifact)).expect("adversarial symlink");
            let result = receive_archive(
                Body::from(payload.as_slice()),
                &headers,
                &state,
                &job_directory,
                "metadata-digest",
            )
            .await;
            let error = match result {
                Ok(_) => panic!("symlink substitution was accepted"),
                Err(error) => error,
            };
            assert_eq!(error.code, "internal_error");
            assert_eq!(
                fs::read(&outside).expect("outside content"),
                b"must remain unchanged"
            );
        }
    }

    #[tokio::test]
    async fn target_inventory_sessions_enforce_job_quota_expiry_and_terminal_cleanup() {
        let directory = TempDir::new().expect("directory");
        let state = test_state(&directory)
            .with_archive_limits(ArchiveLimits {
                maximum_jobs: 1,
                maximum_retained_results: 1,
                free_space_reserve_bytes: 0,
                ..ArchiveLimits::default()
            })
            .expect("limits");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TEST_TOKEN}")).expect("authorization"),
        );
        let started = begin_target_inventory(
            State(state.clone()),
            headers.clone(),
            ContractJson(BeginTargetInventoryRequest {
                job_id: "inventory-owner".to_owned(),
                schema_version: 1,
                root_identity: "root-owner".to_owned(),
                entry_count: 1,
                total_bytes: 1,
            }),
        )
        .await
        .expect("begin")
        .0;
        let quota = begin_target_inventory(
            State(state.clone()),
            headers.clone(),
            ContractJson(BeginTargetInventoryRequest {
                job_id: "inventory-second".to_owned(),
                schema_version: 1,
                root_identity: "root-second".to_owned(),
                entry_count: 0,
                total_bytes: 0,
            }),
        )
        .await;
        assert!(quota.is_err(), "inventory job quota must fail closed");

        let entry = TargetInventoryEntry {
            path: RelativePath::new("file.txt".to_owned()).expect("path"),
            kind: EntryKind::File,
            length: 1,
            modified_at_unix_ms: None,
            identity_token: "dev=1;ino=2".to_owned(),
        };
        let wrong_job = append_target_inventory_page(
            State(state.clone()),
            headers.clone(),
            AxumPath(started.inventory_id.clone()),
            ContractJson(TargetInventoryPageRequest {
                job_id: "inventory-second".to_owned(),
                offset: 0,
                page_digest: target_inventory_page_digest(std::slice::from_ref(&entry)),
                entries: vec![entry.clone()],
            }),
        )
        .await;
        assert!(
            wrong_job.is_err(),
            "inventory ID must be isolated to its job directory"
        );
        append_target_inventory_page(
            State(state.clone()),
            headers.clone(),
            AxumPath(started.inventory_id.clone()),
            ContractJson(TargetInventoryPageRequest {
                job_id: "inventory-owner".to_owned(),
                offset: 0,
                page_digest: target_inventory_page_digest(std::slice::from_ref(&entry)),
                entries: vec![entry],
            }),
        )
        .await
        .expect("page");
        let terminal = finalize_target_inventory(
            State(state.clone()),
            headers,
            AxumPath(started.inventory_id.clone()),
            ContractJson(FinalizeTargetInventoryRequest {
                job_id: "inventory-owner".to_owned(),
                entry_count: 1,
                total_bytes: 1,
                inventory_digest: "0".repeat(64),
            }),
        )
        .await;
        assert!(terminal.is_err(), "wrong final digest must be rejected");
        let job = state.archive_restore_root.join("inventory-owner");
        assert!(
            !job.join(target_inventory_session_name(&started.inventory_id))
                .exists()
        );
        assert!(
            !job.join(target_inventory_entries_name(&started.inventory_id))
                .exists()
        );

        let handle = File::open(&job).expect("job directory");
        handle
            .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .expect("age inventory job");
        prune_stale_archive_restore_targets(state.archive_restore_root.as_path())
            .expect("prune expired inventory");
        assert!(!job.exists());
    }
}
