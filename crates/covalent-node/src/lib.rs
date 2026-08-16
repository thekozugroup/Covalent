//! Local authenticated API, embedded accessible console, discovery, and peer transport.

pub mod discovery;
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
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    BackupOptions, ChunkProvider, CoreError, Engine, JobControl, JobState, PairingConfirmation,
    PairingSession, PreviewAction, RestoreOptions, RestorePlan, RestorePreviewEntry, RosterCursor,
};
use covalent_protocol::{
    ApiErrorBody, BackupId, BackupSummary, ConflictPolicy, DeviceId, EntryKind, NodeStatus,
    PROTOCOL_VERSION, PairingInvitation, PeerRole, PlatformTier, RelativePath, ReplicaAvailability,
    ReplicaIntent, SignedRoster,
};
use http_body_util::BodyExt as _;
use rand_core::{OsRng, RngCore};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt as _;
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
        {
            return Err(CoreError::InvalidState(
                "invalid archive resource limits".to_owned(),
            ));
        }
        Ok(self)
    }
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
    peer_port: u16,
    provider_connections: Arc<Mutex<BTreeMap<DeviceId, ProviderConnection>>>,
    provider_state_path: Option<Arc<PathBuf>>,
    transport_certificate: Option<Arc<Vec<u8>>>,
    discovery_controller: Option<Arc<discovery::DiscoveryController>>,
    archive_limits: ArchiveLimits,
    archive_backup_root: Arc<PathBuf>,
    archive_backup_lock: Arc<Mutex<()>>,
    archive_restore_root: Arc<PathBuf>,
    archive_restore_lock: Arc<Mutex<()>>,
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
        Ok(Self {
            engine,
            platform_tier,
            api_token: Arc::new(Zeroizing::new(api_token)),
            jobs: Arc::new(Mutex::new(JobRegistry::default())),
            peer_port: 8787,
            provider_connections: Arc::new(Mutex::new(BTreeMap::new())),
            provider_state_path: None,
            transport_certificate: None,
            discovery_controller: None,
            archive_limits: ArchiveLimits::default(),
            archive_backup_root: Arc::new(archive_backup_root),
            archive_backup_lock: Arc::new(Mutex::new(())),
            archive_restore_root: Arc::new(archive_restore_root),
            archive_restore_lock: Arc::new(Mutex::new(())),
            restore_plan_root: Arc::new(restore_plan_root),
            restore_plan_lock: Arc::new(Mutex::new(())),
            engine_job_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_ENGINE_JOBS)),
        })
    }

    /// Overrides the advertised/discovered QUIC port after the daemon binds it.
    #[must_use]
    pub const fn with_peer_port(mut self, peer_port: u16) -> Self {
        self.peer_port = peer_port;
        self
    }

    /// Publishes the daemon's public TLS certificate through the authenticated local API.
    #[must_use]
    pub fn with_transport_certificate(mut self, certificate_der: Vec<u8>) -> Self {
        self.transport_certificate = Some(Arc::new(certificate_der));
        self
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
        state.providers.retain(|peer_id, _| {
            self.engine
                .authorized_peer(*peer_id, PeerRole::StorageProvider)
                .is_ok()
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

fn persist_private_file(path: &Path, bytes: &[u8]) -> Result<(), CoreError> {
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
        peer_port: state.peer_port,
        certificate_der: URL_SAFE_NO_PAD.encode(certificate),
        certificate_fingerprint: blake3::hash(certificate).to_hex().to_string(),
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
    let peer_port = state.peer_port;
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
    endpoints: Vec<String>,
}

async fn pair_invitation(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairInvitationRequest>,
) -> Result<axum::Json<PairingInvitation>, ApiError> {
    authorize(&state, &headers)?;
    let invitation = state
        .engine
        .pairing_manager()
        .create_invitation(now_unix_ms(), request.lifetime_ms, request.endpoints)
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
    let session = state
        .engine
        .accept_pairing(
            request.invitation,
            request.responder_name,
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

async fn pair_finalize_responder(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairFinalizeRequest>,
) -> Result<axum::Json<PairingConfirmation>, ApiError> {
    authorize(&state, &headers)?;
    let confirmation = state
        .engine
        .finalize_pairing_as_responder(&request.session, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(confirmation))
}

async fn pair_finalize_inviter(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<PairFinalizeRequest>,
) -> Result<axum::Json<PairingConfirmation>, ApiError> {
    authorize(&state, &headers)?;
    let confirmation = state
        .engine
        .finalize_pairing_as_inviter(&request.session, now_unix_ms())
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(confirmation))
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
    peer_id: DeviceId,
    address: SocketAddr,
    certificate_der: String,
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
    let certificate = URL_SAFE_NO_PAD
        .decode(&request.certificate_der)
        .map_err(|_| {
            ApiError::bad_request("invalid_certificate", "Certificate encoding is invalid.")
        })?;
    if certificate.is_empty() || certificate.len() > 64 * 1_024 {
        return Err(ApiError::bad_request(
            "invalid_certificate",
            "Certificate size is invalid.",
        ));
    }
    let response = ProviderConnectionResponse {
        peer_id: request.peer_id,
        address: request.address,
        certificate_fingerprint: blake3::hash(&certificate).to_hex().to_string(),
    };
    state
        .connect_provider(ProviderConnection {
            peer_id: request.peer_id,
            address: request.address,
            certificate_der: request.certificate_der,
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
            certificate_fingerprint: blake3::hash(&certificate).to_hex().to_string(),
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
    let archive_path =
        match receive_archive(body, &headers, &job_directory, state.archive_limits).await {
            Ok(path) => path,
            Err(error) => {
                if !existed {
                    let _ = remove_private_job_directory(
                        state.archive_backup_root.as_path(),
                        &job_directory,
                    );
                    lease.finish().map_err(ApiError::from_core)?;
                } else {
                    lease.preserve_for_resume().map_err(ApiError::from_core)?;
                }
                return Err(error);
            }
        };
    let source_root = job_directory.join("source");
    let request = metadata.with_source_root(source_root.clone());
    let worker_job_directory = job_directory.clone();
    let engine = Arc::clone(&state.engine);
    let limits = state.archive_limits;
    let result = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        if !source_root.exists() {
            extract_backup_archive(&archive_path, &source_root, limits, &control)?;
        }
        let backup_id = request.backup_id.unwrap_or_default();
        let mut options = BackupOptions::new(backup_id, request.snapshot_id, request.job_id);
        options.display_name = request.display_name;
        options.created_at_unix_ms = now_unix_ms();
        options.replica_intent = ReplicaIntent::explicit(request.selected_provider_ids);
        let result = engine
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
        persist_private_file(
            &worker_job_directory.join("result.json"),
            &serde_json::to_vec(&response).map_err(ApiError::from_json)?,
        )
        .map_err(ApiError::from_core)?;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreArchivePreviewRequest {
    backup_id: BackupId,
    snapshot_id: String,
    conflict_policy: ConflictPolicy,
    job_id: String,
}

async fn restore_archive_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestoreArchivePreviewRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if request.conflict_policy != ConflictPolicy::Fail {
        return Err(ApiError::bad_request(
            "streamed_restore_requires_empty_destination",
            "Streamed restores require fail-on-conflict and an empty client-authorized destination.",
        ));
    }
    if let Some(plan) = find_restore_plan_by_job(&state, &request.job_id)? {
        if plan.backup_id != request.backup_id
            || plan.snapshot_id != request.snapshot_id
            || plan.conflict_policy != request.conflict_policy
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
    if plan.conflict_policy != ConflictPolicy::Fail
        || plan.entries.iter().any(|entry| {
            !matches!(
                (entry.kind, entry.action),
                (EntryKind::Directory, PreviewAction::CreateDirectory)
                    | (EntryKind::File, PreviewAction::CreateFile)
            )
        })
    {
        return Err(ApiError::bad_request(
            "invalid_streamed_restore_plan",
            "A streamed restore plan may only create content in an empty destination.",
        ));
    }
    let target_root = validate_archive_restore_plan(&state, &plan)?;
    if let Some(response) = completed_archive_restore_response(&plan_id, &plan).await? {
        return Ok(response);
    }
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
    let engine = Arc::clone(&state.engine);
    let limits = state.archive_limits;
    let job_id = plan.job_id.clone();
    let mut lease = state.start_job(&job_id)?;
    let control = lease.control();
    let outcome = tokio::task::spawn_blocking(move || {
        let _admission = admission;
        let report = engine
            .restore(&plan, &control)
            .map_err(ApiError::from_core)?;
        let response = RestoreResponse {
            files_restored: report.files_restored,
            directories_created: report.directories_created,
            files_skipped: report.files_skipped,
            bytes_written: report.bytes_written,
            rejected_provider_copies: report.rejected_provider_copies.len(),
        };
        let length =
            zip_restore_directory(&target_root, &worker_result_archive_path, &control, limits)?;
        let completion = ArchiveRestoreCompletion {
            plan_id: worker_plan_id,
            plan_digest: plan.plan_digest.clone(),
            result: response.clone(),
        };
        persist_private_file(
            &worker_result_json_path,
            &serde_json::to_vec(&completion).map_err(ApiError::from_json)?,
        )
        .map_err(ApiError::from_core)?;
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
    job_directory: &Path,
    limits: ArchiveLimits,
) -> Result<PathBuf, ApiError> {
    let temporary_path = job_directory.join("upload.part");
    let result = receive_archive_inner(body, headers, job_directory, limits).await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(temporary_path).await;
    }
    result
}

async fn receive_archive_inner(
    mut body: Body,
    headers: &HeaderMap,
    job_directory: &Path,
    limits: ArchiveLimits,
) -> Result<PathBuf, ApiError> {
    let expected_length = match headers.get(header::CONTENT_LENGTH) {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|length| *length > 0)
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "invalid_content_length",
                        "Archive Content-Length is invalid.",
                    )
                })?,
        ),
        None => None,
    };
    if expected_length.is_some_and(|length| length > limits.maximum_compressed_bytes) {
        return Err(ApiError::payload_too_large(
            "Archive exceeds the streamed transfer limit.",
        ));
    }
    if let Some(length) = expected_length {
        ensure_archive_capacity(job_directory, length, limits.free_space_reserve_bytes)?;
    }
    let temporary_path = job_directory.join("upload.part");
    let mut archive = tokio::fs::File::create(&temporary_path)
        .await
        .map_err(|_| ApiError::internal("archive staging file could not be created"))?;
    let mut received = 0_u64;
    let started = Instant::now();
    let mut hasher = blake3::Hasher::new();
    loop {
        if started.elapsed() > ARCHIVE_UPLOAD_MAX_DURATION {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(ApiError::payload_too_large(
                "Archive upload exceeded the maximum duration.",
            ));
        }
        let next = tokio::time::timeout(ARCHIVE_UPLOAD_IDLE_TIMEOUT, body.frame())
            .await
            .map_err(|_| {
                ApiError::bad_request(
                    "archive_upload_stalled",
                    "Archive upload stopped making progress.",
                )
            })?;
        let Some(frame) = next else {
            break;
        };
        let frame = frame.map_err(|_| {
            ApiError::bad_request(
                "invalid_archive",
                "The streamed archive ended unexpectedly.",
            )
        })?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        received = received
            .checked_add(u64::try_from(data.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        if received > limits.maximum_compressed_bytes {
            return Err(ApiError::payload_too_large(
                "Archive exceeds the streamed transfer limit.",
            ));
        }
        if started.elapsed() >= Duration::from_secs(60)
            && received / started.elapsed().as_secs().max(1) < MIN_ARCHIVE_UPLOAD_BYTES_PER_SECOND
        {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(ApiError::bad_request(
                "archive_upload_too_slow",
                "Archive upload remained below the minimum transfer rate.",
            ));
        }
        if expected_length.is_none() {
            ensure_archive_capacity(
                job_directory,
                u64::try_from(data.len()).unwrap_or(u64::MAX),
                limits.free_space_reserve_bytes,
            )?;
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
    if received == 0 || expected_length.is_some_and(|length| length != received) {
        let _ = tokio::fs::remove_file(&temporary_path).await;
        return Err(ApiError::bad_request(
            "invalid_archive",
            "Archive length did not match the request.",
        ));
    }
    let digest = hasher.finalize().to_hex().to_string();
    let archive_path = job_directory.join(format!("upload-{digest}.zip"));
    match existing_archive_upload(job_directory, limits.maximum_compressed_bytes)? {
        Some((stored, existing_path)) if stored == digest => {
            tokio::fs::remove_file(&temporary_path)
                .await
                .map_err(|_| ApiError::internal("duplicate archive staging cleanup failed"))?;
            return Ok(existing_path);
        }
        Some(_) => {
            let _ = tokio::fs::remove_file(&temporary_path).await;
            return Err(ApiError::conflict(
                "job_conflict",
                "This archive job ID is bound to different content.",
            ));
        }
        None => {
            tokio::fs::rename(&temporary_path, &archive_path)
                .await
                .map_err(|_| ApiError::internal("archive staging commit failed"))?;
            File::open(job_directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ApiError::internal("archive staging commit could not be synced"))?;
        }
    }
    Ok(archive_path)
}

fn existing_archive_upload(
    job_directory: &Path,
    maximum_bytes: u64,
) -> Result<Option<(String, PathBuf)>, ApiError> {
    let mut found = None;
    for entry in fs::read_dir(job_directory)
        .map_err(|_| ApiError::internal("archive staging could not be inspected"))?
    {
        let entry = entry.map_err(|_| ApiError::internal("archive staging entry is invalid"))?;
        let name = entry.file_name();
        let Some(digest) = name
            .to_str()
            .and_then(|name| name.strip_prefix("upload-"))
            .and_then(|name| name.strip_suffix(".zip"))
        else {
            continue;
        };
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| ApiError::internal("archive upload could not be inspected"))?;
        if !valid_lowercase_digest(digest)
            || metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > maximum_bytes
            || found.is_some()
        {
            return Err(ApiError::internal("durable archive upload is invalid"));
        }
        found = Some((digest.to_owned(), path));
    }
    Ok(found)
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
    limits: ArchiveLimits,
    control: &JobControl,
) -> Result<(), ApiError> {
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
    ensure_archive_capacity(
        source_root
            .parent()
            .ok_or_else(|| ApiError::internal("archive source has no parent"))?,
        declared_expanded.saturating_mul(2),
        limits.free_space_reserve_bytes,
    )?;
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
    let job_directory = state.archive_restore_root.join(job_id);
    match fs::create_dir(&job_directory) {
        Ok(()) => {
            let job_directory =
                create_private_directory(job_directory).map_err(ApiError::from_core)?;
            let target = job_directory.join("target");
            fs::create_dir(&target)
                .map_err(|_| ApiError::internal("archive restore target could not be created"))?;
            create_private_directory(target).map_err(ApiError::from_core)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(ApiError::conflict(
            "job_conflict",
            "This archive restore job ID already exists.",
        )),
        Err(_) => Err(ApiError::internal(
            "archive restore staging directory could not be created",
        )),
    }
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

fn zip_restore_directory(
    root: &Path,
    result_path: &Path,
    control: &JobControl,
    limits: ArchiveLimits,
) -> Result<u64, ApiError> {
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
            },
            CoreError::Paused => Self {
                status: StatusCode::CONFLICT,
                code: "job_paused",
                message: "The job is paused and can be resumed with the same job ID.",
                retryable: false,
            },
            CoreError::Cancelled => Self {
                status: StatusCode::CONFLICT,
                code: "job_cancelled",
                message: "The job was cancelled and its checkpoint was discarded.",
                retryable: false,
            },
            CoreError::RestoreConflict(_) => Self {
                status: StatusCode::CONFLICT,
                code: "restore_conflict",
                message: "The restore preview found a destination conflict.",
                retryable: false,
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
            },
            CoreError::UnsupportedSourceEntry(_) | CoreError::SourcePermissionDenied(_) => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "source_unreadable",
                message: "The source contains an unsupported or unreadable entry.",
                retryable: false,
            },
            CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "backup_corrupt",
                message: "Backup data failed authenticated integrity verification.",
                retryable: false,
            },
            CoreError::MissingChunk(_) | CoreError::ProvidersExhausted(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "backup_unavailable",
                message: "No intact authorized copy is currently available.",
                retryable: true,
            },
            CoreError::ResourceLimit(_) | CoreError::SettingsTooLarge => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "resource_limit",
                message: "The request exceeded a configured resource limit.",
                retryable: false,
            },
            CoreError::PeerRevoked
            | CoreError::UnselectedProvider
            | CoreError::IdentityMismatch => Self {
                status: StatusCode::FORBIDDEN,
                code: "not_authorized",
                message: "The requested peer or provider is not authorized.",
                retryable: false,
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
        }
    }

    const fn conflict(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code,
            message,
            retryable: false,
        }
    }

    const fn not_found(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            code,
            message,
            retryable: false,
        }
    }

    const fn payload_too_large(message: &'static str) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "resource_limit",
            message,
            retryable: false,
        }
    }

    const fn too_many_requests(message: &'static str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "node_busy",
            message,
            retryable: true,
        }
    }

    const fn internal(message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message,
            retryable: true,
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
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
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
        let backup_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/backups/archive")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/vnd.covalent.backup+zip")
                    .header(ARCHIVE_METADATA_HEADER, metadata.clone())
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
}
