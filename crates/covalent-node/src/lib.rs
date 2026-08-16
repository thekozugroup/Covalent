//! Local authenticated API, embedded accessible console, discovery, and peer transport.

pub mod discovery;
pub mod transport;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    BackupOptions, ChunkProvider, CoreError, Engine, JobControl, JobState, PairingConfirmation,
    PairingSession, RestoreOptions, RestorePlan, RosterCursor,
};
use covalent_protocol::{
    BackupId, ConflictPolicy, DeviceId, NodeStatus, PROTOCOL_VERSION, PairingInvitation, PeerRole,
    PlatformTier, ReplicaAvailability, ReplicaIntent, SignedRoster,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const INDEX_HTML: &str = include_str!("../../../packaging/web/index.html");
const APP_CSS: &str = include_str!("../../../packaging/web/app.css");
const APP_JS: &str = include_str!("../../../packaging/web/app.js");
const MAX_LOCAL_API_BODY_BYTES: usize = 2 * 1_024 * 1_024;
const PROVIDER_CONNECTION_SCHEMA_VERSION: u16 = 1;
const MAX_PROVIDER_CONNECTION_STATE_BYTES: usize = 16 * 1_024 * 1_024;

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
        Ok(Self {
            engine,
            platform_tier,
            api_token: Arc::new(Zeroizing::new(api_token)),
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
            peer_port: 8787,
            provider_connections: Arc::new(Mutex::new(BTreeMap::new())),
            provider_state_path: None,
            transport_certificate: None,
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
        .route("/api/v1/backups", post(backup))
        .route("/api/v1/backups/verify", post(verify_backup))
        .route("/api/v1/restores/preview", post(restore_preview))
        .route("/api/v1/restores/execute", post(restore_execute))
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
    axum::Json(request): axum::Json<ConfigImportRequest>,
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
    axum::Json(request): axum::Json<PairInvitationRequest>,
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
    axum::Json(request): axum::Json<PairAcceptRequest>,
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
    axum::Json(mut request): axum::Json<PairConfirmRequest>,
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
    axum::Json(mut request): axum::Json<PairConfirmRequest>,
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
    axum::Json(request): axum::Json<PairFinalizeRequest>,
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
    axum::Json(request): axum::Json<PairFinalizeRequest>,
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
    axum::Json(request): axum::Json<JobControlRequest>,
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
    axum::Json(request): axum::Json<RevokePeerRequest>,
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
    axum::Json(request): axum::Json<ConnectProviderRequest>,
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
    axum::Json(request): axum::Json<DisconnectProviderRequest>,
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
    axum::Json(roster): axum::Json<SignedRoster>,
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

async fn backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<BackupRequest>,
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
    axum::Json(request): axum::Json<SnapshotRequest>,
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
    axum::Json(request): axum::Json<RestorePreviewRequest>,
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
    axum::Json(request): axum::Json<RestoreExecuteRequest>,
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
}

impl ApiError {
    fn from_core(error: CoreError) -> Self {
        match error {
            CoreError::SettingsImportNotConfirmed | CoreError::PairingNotConfirmed => Self {
                status: StatusCode::CONFLICT,
                code: "confirmation_required",
                message: "Explicit local confirmation is required.",
            },
            CoreError::Paused => Self {
                status: StatusCode::CONFLICT,
                code: "job_paused",
                message: "The job is paused and can be resumed with the same job ID.",
            },
            CoreError::Cancelled => Self {
                status: StatusCode::CONFLICT,
                code: "job_cancelled",
                message: "The job was cancelled and its checkpoint was discarded.",
            },
            CoreError::RestoreConflict(_) => Self {
                status: StatusCode::CONFLICT,
                code: "restore_conflict",
                message: "The restore preview found a destination conflict.",
            },
            CoreError::MissingChunk(_) | CoreError::ProvidersExhausted(_) => Self {
                status: StatusCode::SERVICE_UNAVAILABLE,
                code: "backup_unavailable",
                message: "No intact authorized copy is currently available.",
            },
            CoreError::ResourceLimit(_) | CoreError::SettingsTooLarge => Self {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "resource_limit",
                message: "The request exceeded a configured resource limit.",
            },
            CoreError::PeerRevoked
            | CoreError::UnselectedProvider
            | CoreError::IdentityMismatch => Self {
                status: StatusCode::FORBIDDEN,
                code: "not_authorized",
                message: "The requested peer or provider is not authorized.",
            },
            _ => Self::internal("The local engine could not complete the request."),
        }
    }

    fn from_json(_error: serde_json::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_json",
            message: "The request does not match the versioned contract.",
        }
    }

    const fn bad_request(code: &'static str, message: &'static str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message,
        }
    }

    const fn internal(message: &'static str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal_error",
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct Body {
            code: &'static str,
            message: &'static str,
        }
        (
            self.status,
            axum::Json(Body {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

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
}
