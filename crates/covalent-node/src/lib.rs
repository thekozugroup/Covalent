//! Local authenticated API, embedded accessible console, discovery, and peer transport.

pub mod discovery;
pub mod transport;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    BackupOptions, ChunkProvider, CoreError, Engine, JobControl, JobState, PairingConfirmation,
    PairingSession, PreviewAction, RestoreOptions, RestorePlan, RosterCursor,
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
use tokio_util::io::ReaderStream;
use walkdir::WalkDir;
use zeroize::Zeroizing;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const INDEX_HTML: &str = include_str!("../../../packaging/web/index.html");
const APP_CSS: &str = include_str!("../../../packaging/web/app.css");
const APP_JS: &str = include_str!("../../../packaging/web/app.js");
const MAX_LOCAL_API_BODY_BYTES: usize = 2 * 1_024 * 1_024;
const PROVIDER_CONNECTION_SCHEMA_VERSION: u16 = 1;
const MAX_PROVIDER_CONNECTION_STATE_BYTES: usize = 16 * 1_024 * 1_024;
const ARCHIVE_METADATA_HEADER: &str = "x-covalent-archive-metadata";
const ARCHIVE_RESULT_HEADER: &str = "x-covalent-restore-result";
const MAX_ARCHIVE_METADATA_BYTES: usize = 32 * 1_024;
const MAX_ARCHIVE_COMPRESSED_BYTES: u64 = 1_u64 << 40;
const MAX_ARCHIVE_UNCOMPRESSED_BYTES: u64 = 16_u64 << 40;
const MAX_ARCHIVE_ENTRIES: usize = 1_000_000;
const MAX_ARCHIVE_RESTORE_TARGETS: usize = 1_024;
const ARCHIVE_RESTORE_MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

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
    jobs: Arc<Mutex<BTreeMap<String, JobControl>>>,
    peer_port: u16,
    provider_connections: Arc<Mutex<BTreeMap<DeviceId, ProviderConnection>>>,
    provider_state_path: Option<Arc<PathBuf>>,
    transport_certificate: Option<Arc<Vec<u8>>>,
    archive_restore_root: Arc<PathBuf>,
    archive_restore_lock: Arc<Mutex<()>>,
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
        Ok(Self {
            engine,
            platform_tier,
            api_token: Arc::new(Zeroizing::new(api_token)),
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            peer_port: 8787,
            provider_connections: Arc::new(Mutex::new(BTreeMap::new())),
            provider_state_path: None,
            transport_certificate: None,
            archive_restore_root: Arc::new(archive_restore_root),
            archive_restore_lock: Arc::new(Mutex::new(())),
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

    fn job_control(&self, job_id: &str) -> Result<JobControl, CoreError> {
        let mut jobs = self.jobs.lock().map_err(|_| CoreError::Synchronization)?;
        if jobs.len() >= 1_024 && !jobs.contains_key(job_id) {
            return Err(CoreError::ResourceLimit("active local jobs"));
        }
        Ok(jobs
            .entry(job_id.to_owned())
            .or_insert_with(JobControl::new)
            .clone())
    }

    fn finish_job(&self, job_id: &str) -> Result<(), CoreError> {
        self.jobs
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .remove(job_id);
        Ok(())
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

/// Builds the stable versioned local API and static console router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(css))
        .route("/assets/app.js", get(javascript))
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
        .route("/api/v1/jobs/control", post(job_control))
        .route("/api/v1/backups", get(list_backups).post(backup))
        .route("/api/v1/backups/archive", post(backup_archive))
        .route("/api/v1/backups/verify", post(verify_backup))
        .route("/api/v1/restores/preview", post(restore_preview))
        .route("/api/v1/restores/execute", post(restore_execute))
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

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS)
}

async fn javascript() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
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
    state
        .engine
        .import_settings(&bytes, request.confirmed)
        .map_err(ApiError::from_core)?;
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
}

async fn job_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<JobControlRequest>,
) -> Result<axum::Json<JobControlResponse>, ApiError> {
    authorize(&state, &headers)?;
    let control = state
        .job_control(&request.job_id)
        .map_err(ApiError::from_core)?;
    match request.action {
        JobAction::Pause => control.pause(),
        JobAction::Resume => control.resume(),
        JobAction::Cancel => control.cancel(),
    }
    let state_name = match control.state() {
        JobState::Running => "running",
        JobState::Paused => "paused",
        JobState::Cancelled => "cancelled",
    };
    Ok(axum::Json(JobControlResponse {
        job_id: request.job_id,
        state: state_name,
    }))
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

#[derive(Serialize)]
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

#[derive(Deserialize)]
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
    let engine = Arc::clone(&state.engine);
    let job_id = request.job_id.clone();
    let control = state.job_control(&job_id).map_err(ApiError::from_core)?;
    let result = tokio::task::spawn_blocking(move || {
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
        Ok(_) => state.finish_job(&job_id).map_err(ApiError::from_core)?,
        Err(CoreError::Cancelled) => {
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
            state.finish_job(&job_id).map_err(ApiError::from_core)?;
        }
        Err(_) => {}
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
) -> Result<axum::Json<BackupResponse>, ApiError> {
    authorize(&state, &headers)?;
    require_archive_content_type(&headers)?;
    let metadata: ArchiveBackupMetadata = decode_archive_metadata(&headers)?;
    if metadata.protocol_version != PROTOCOL_VERSION {
        return Err(ApiError::conflict(
            "protocol_incompatible",
            "Archive metadata uses an unsupported protocol version.",
        ));
    }
    let (staging, archive_path) = receive_archive(body, &headers).await?;
    let source_root = staging.path().join("source");
    let request = metadata.with_source_root(source_root.clone());
    let job_id = request.job_id.clone();
    let control = state.job_control(&job_id).map_err(ApiError::from_core)?;
    let engine = Arc::clone(&state.engine);
    let result = tokio::task::spawn_blocking(move || {
        let _staging = staging;
        extract_backup_archive(&archive_path, &source_root)?;
        let backup_id = request.backup_id.unwrap_or_default();
        let mut options = BackupOptions::new(backup_id, request.snapshot_id, request.job_id);
        options.display_name = request.display_name;
        options.created_at_unix_ms = now_unix_ms();
        options.replica_intent = ReplicaIntent::explicit(request.selected_provider_ids);
        engine
            .backup(request.source_root, &options, &control, |_| {})
            .map(|result| (backup_id, result))
            .map_err(ApiError::from_core)
    })
    .await
    .map_err(|_| ApiError::internal("archive backup worker failed"))?;
    match &result {
        Ok(_) => state.finish_job(&job_id).map_err(ApiError::from_core)?,
        Err(error) if error.code == "job_cancelled" => {
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
            state.finish_job(&job_id).map_err(ApiError::from_core)?;
        }
        Err(_) => {}
    }
    let (backup_id, result) = result?;
    Ok(axum::Json(BackupResponse {
        backup_id,
        snapshot_id: result.manifest.snapshot_id.clone(),
        entries: result.manifest.entries.len(),
        bytes_read: result.progress.bytes_read,
        chunks_stored: result.progress.chunks_stored,
        chunks_deduplicated: result.progress.chunks_deduplicated,
        selected_providers: result.manifest.replica_intent.selected_providers.len(),
        degraded_failures: result.replication.failures.len(),
    }))
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
    let engine = Arc::clone(&state.engine);
    let report = tokio::task::spawn_blocking(move || {
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
) -> Result<axum::Json<RestorePlan>, ApiError> {
    authorize(&state, &headers)?;
    let options = RestoreOptions {
        conflict_policy: request.conflict_policy,
        selected_paths: Default::default(),
        job_id: request.job_id,
    };
    let plan = state
        .engine
        .preview_restore(
            request.backup_id,
            &request.snapshot_id,
            request.target_root,
            &options,
        )
        .map_err(ApiError::from_core)?;
    Ok(axum::Json(plan))
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
) -> Result<axum::Json<RestorePlan>, ApiError> {
    authorize(&state, &headers)?;
    if request.conflict_policy != ConflictPolicy::Fail {
        return Err(ApiError::bad_request(
            "streamed_restore_requires_empty_destination",
            "Streamed restores require fail-on-conflict and an empty client-authorized destination.",
        ));
    }
    let target_root = create_archive_restore_target(&state, &request.job_id)?;
    let options = RestoreOptions {
        conflict_policy: request.conflict_policy,
        selected_paths: Default::default(),
        job_id: request.job_id,
    };
    match state.engine.preview_restore(
        request.backup_id,
        &request.snapshot_id,
        &target_root,
        &options,
    ) {
        Ok(plan) => Ok(axum::Json(plan)),
        Err(error) => {
            let _ = fs::remove_dir(&target_root);
            Err(ApiError::from_core(error))
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RestoreExecuteRequest {
    plan: RestorePlan,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreResponse {
    files_restored: usize,
    directories_created: usize,
    files_skipped: usize,
    bytes_written: u64,
    rejected_provider_copies: usize,
}

async fn restore_execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestoreExecuteRequest>,
) -> Result<axum::Json<RestoreResponse>, ApiError> {
    authorize(&state, &headers)?;
    let engine = Arc::clone(&state.engine);
    let job_id = request.plan.job_id.clone();
    let control = state.job_control(&job_id).map_err(ApiError::from_core)?;
    let report = tokio::task::spawn_blocking(move || engine.restore(&request.plan, &control))
        .await
        .map_err(|_| ApiError::internal("restore worker failed"))?;
    match &report {
        Ok(_) => state.finish_job(&job_id).map_err(ApiError::from_core)?,
        Err(CoreError::Cancelled) => {
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
            state.finish_job(&job_id).map_err(ApiError::from_core)?;
        }
        Err(_) => {}
    }
    let report = report.map_err(ApiError::from_core)?;
    Ok(axum::Json(RestoreResponse {
        files_restored: report.files_restored,
        directories_created: report.directories_created,
        files_skipped: report.files_skipped,
        bytes_written: report.bytes_written,
        rejected_provider_copies: report.rejected_provider_copies.len(),
    }))
}

async fn restore_archive_execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RestoreExecuteRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    if request.plan.conflict_policy != ConflictPolicy::Fail
        || request.plan.entries.iter().any(|entry| {
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
    let target_root = validate_archive_restore_plan(&state, &request.plan)?;
    let engine = Arc::clone(&state.engine);
    let job_id = request.plan.job_id.clone();
    let control = state.job_control(&job_id).map_err(ApiError::from_core)?;
    let plan = request.plan;
    let outcome = tokio::task::spawn_blocking(move || {
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
        let (archive, length) = zip_restore_directory(&target_root)?;
        remove_archive_restore_target(&target_root)?;
        Ok::<_, ApiError>((archive, length, response))
    })
    .await
    .map_err(|_| ApiError::internal("archive restore worker failed"))?;
    match &outcome {
        Ok(_) => state.finish_job(&job_id).map_err(ApiError::from_core)?,
        Err(error) if error.code == "job_cancelled" => {
            state
                .engine
                .discard_job_checkpoint(&job_id)
                .map_err(ApiError::from_core)?;
            state.finish_job(&job_id).map_err(ApiError::from_core)?;
        }
        Err(_) => {}
    }
    let (archive, length, result) = outcome?;
    let encoded_result =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&result).map_err(ApiError::from_json)?);
    let mut response = Response::new(Body::from_stream(ReaderStream::new(
        tokio::fs::File::from_std(archive),
    )));
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
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
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

async fn receive_archive(
    mut body: Body,
    headers: &HeaderMap,
) -> Result<(tempfile::TempDir, PathBuf), ApiError> {
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_ARCHIVE_COMPRESSED_BYTES)
    {
        return Err(ApiError::payload_too_large(
            "Archive exceeds the streamed transfer limit.",
        ));
    }
    let staging = tempfile::Builder::new()
        .prefix("covalent-saf-backup-")
        .tempdir()
        .map_err(|_| ApiError::internal("archive staging directory could not be created"))?;
    let archive_path = staging.path().join("upload.zip");
    let mut archive = tokio::fs::File::create(&archive_path)
        .await
        .map_err(|_| ApiError::internal("archive staging file could not be created"))?;
    let mut received = 0_u64;
    while let Some(frame) = body.frame().await {
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
        if received > MAX_ARCHIVE_COMPRESSED_BYTES {
            return Err(ApiError::payload_too_large(
                "Archive exceeds the streamed transfer limit.",
            ));
        }
        archive
            .write_all(&data)
            .await
            .map_err(|_| ApiError::internal("archive staging write failed"))?;
    }
    archive
        .sync_all()
        .await
        .map_err(|_| ApiError::internal("archive staging sync failed"))?;
    Ok((staging, archive_path))
}

fn extract_backup_archive(archive_path: &Path, source_root: &Path) -> Result<(), ApiError> {
    create_private_directory(source_root.to_path_buf()).map_err(ApiError::from_core)?;
    let file = File::open(archive_path)
        .map_err(|_| ApiError::internal("archive staging file could not be opened"))?;
    let mut archive = ZipArchive::new(file).map_err(|_| {
        ApiError::bad_request("invalid_archive", "The streamed ZIP archive is invalid.")
    })?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ApiError::payload_too_large(
            "Archive contains too many entries.",
        ));
    }
    let mut seen = BTreeSet::new();
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
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
        total_size = total_size
            .checked_add(entry.size())
            .ok_or_else(|| ApiError::payload_too_large("Archive size overflowed."))?;
        if total_size > MAX_ARCHIVE_UNCOMPRESSED_BYTES {
            return Err(ApiError::payload_too_large(
                "Archive expands beyond the transfer limit.",
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
        let copied = std::io::copy(&mut entry, &mut output).map_err(|_| {
            ApiError::bad_request("invalid_archive", "Archive content failed validation.")
        })?;
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

fn valid_job_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
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
    if target_count >= MAX_ARCHIVE_RESTORE_TARGETS {
        return Err(ApiError::payload_too_large(
            "Too many archive restore previews are waiting for execution.",
        ));
    }
    let target = state.archive_restore_root.join(job_id);
    match fs::create_dir(&target) {
        Ok(()) => create_private_directory(target).map_err(ApiError::from_core),
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
    if target.parent() != Some(state.archive_restore_root.as_path())
        || target.file_name().and_then(|name| name.to_str()) != Some(plan.job_id.as_str())
    {
        return Err(ApiError::bad_request(
            "restore_plan_mismatch",
            "Only an archive restore preview can be streamed to a document provider.",
        ));
    }
    Ok(target)
}

fn zip_restore_directory(root: &Path) -> Result<(File, u64), ApiError> {
    let archive = tempfile::tempfile()
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
    for entry in entries {
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
            std::io::copy(&mut input, &mut writer)
                .map_err(|_| ApiError::internal("restored file could not be archived"))?;
        } else {
            return Err(ApiError::internal(
                "restore staging contained an unsupported entry",
            ));
        }
    }
    let mut archive = writer
        .finish()
        .map_err(|_| ApiError::internal("restore archive could not be finalized"))?;
    archive
        .sync_all()
        .map_err(|_| ApiError::internal("restore archive could not be synced"))?;
    let length = archive
        .metadata()
        .map_err(|_| ApiError::internal("restore archive size is unavailable"))?
        .len();
    archive
        .seek(SeekFrom::Start(0))
        .map_err(|_| ApiError::internal("restore archive could not be rewound"))?;
    Ok((archive, length))
}

fn remove_archive_restore_target(target: &Path) -> Result<(), ApiError> {
    let parent = target
        .parent()
        .ok_or_else(|| ApiError::internal("archive restore target has no parent"))?;
    fs::remove_dir_all(target)
        .map_err(|_| ApiError::internal("archive restore staging cleanup failed"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ApiError::internal("archive restore cleanup sync failed"))
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

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

    const fn payload_too_large(message: &'static str) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "resource_limit",
            message,
            retryable: false,
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
        (
            self.status,
            axum::Json(ApiErrorBody {
                protocol_version: PROTOCOL_VERSION,
                code: self.code.to_owned(),
                message: self.message.to_owned(),
                retryable: self.retryable,
            }),
        )
            .into_response()
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

        let restore_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({ "plan": plan }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("restore response");
        assert_eq!(restore_response.status(), StatusCode::OK);
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
                    .header(ARCHIVE_METADATA_HEADER, metadata)
                    .body(Body::from(archive))
                    .expect("request"),
            )
            .await
            .expect("backup response");
        let status = backup_response.status();
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
        let plan: serde_json::Value = serde_json::from_slice(
            &preview_response
                .into_body()
                .collect()
                .await
                .expect("preview body")
                .to_bytes(),
        )
        .expect("preview JSON");
        assert!(
            !plan["authorizedRoot"]
                .as_str()
                .expect("authorized root")
                .starts_with("content://")
        );

        let restore_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/restores/archive/execute")
                    .header(header::AUTHORIZATION, format!("Bearer {TEST_TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::json!({ "plan": plan }).to_string()))
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
        let restored_archive = restore_response
            .into_body()
            .collect()
            .await
            .expect("restore archive")
            .to_bytes();
        let mut restored = ZipArchive::new(Cursor::new(restored_archive)).expect("restore ZIP");
        let mut file = restored
            .by_name("Documents/large.bin")
            .expect("restored entry");
        let mut actual = Vec::new();
        file.read_to_end(&mut actual).expect("restored content");
        assert_eq!(actual, expected);
        assert!(!archive_root.join("android-saf-restore-job").exists());
    }
}
