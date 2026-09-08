//! Reusable lifecycle for an in-process Covalent node.
//!
//! Native applications own this runtime instead of spawning the command-line
//! binary.  The runtime deliberately has no signal handling: binaries decide
//! which OS lifecycle events should request shutdown, while embedded callers
//! call [`NodeRuntime::stop`] or let the handle drop.

use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use covalent_core::{
    CoreError, Engine, EngineOptions, KeyProtector, ProviderQuotaPolicy, RecoveryUnlockKey,
};
use covalent_protocol::PlatformTier;
use tokio::sync::{Mutex, watch};
use tracing::{info, warn};
use zeroize::Zeroizing;

use crate::advertised_address;
use crate::discovery::DiscoveryController;
use crate::first_run_claim::{self, ClaimCode, FirstRunClaim};
use crate::pairing_transport::NetworkPairingService;
use crate::recovery_state::RecoveryStateStore;
use crate::transport::{QuicNode, TlsIdentity};
use crate::{
    AppState, ArchiveLimits, NodeReadyInfo, load_or_create_local_api_token, remove_node_ready_file,
    router, validate_cleartext_api_bind, write_node_ready_file,
};

#[path = "recovery_bootstrap.rs"]
mod recovery_bootstrap;
use recovery_bootstrap::persist_recovered_provider_connections;

/// Source for the local API bearer token.
///
/// An embedded caller should pass [`Self::Provided`] only when its native
/// secure-storage bridge owns the token.  Otherwise the node creates or loads
/// a private token beneath its data directory.
pub enum LocalApiTokenSource {
    /// Load or create a private durable token at `data_directory/local-api-token`.
    Persisted,
    /// Use a token supplied by a native secure-storage bridge.
    Provided(Zeroizing<String>),
}

/// Explicit owner-loss input consumed only while creating a fresh state root.
pub struct RecoveryBootstrap {
    /// Stable signed recovery kit bytes.
    pub kit: Zeroizing<Vec<u8>>,
    /// High-entropy secret held outside the lost node state.
    pub unlock: RecoveryUnlockKey,
}

const MAX_RECOVERY_KIT_FILE_BYTES: u64 = 16 * 1_024 * 1_024;

/// Loads the canonical raw kit and base64url key files without following symlinks.
///
/// Both files must be owner-only regular files on Unix. The kit file contains decoded
/// serialized kit bytes, while the key file contains exactly the printable 256-bit key.
pub fn load_recovery_bootstrap_files(
    kit_path: &Path,
    key_path: &Path,
) -> std::result::Result<RecoveryBootstrap, CoreError> {
    let kit = read_private_recovery_file(kit_path, MAX_RECOVERY_KIT_FILE_BYTES, "recovery kit")?;
    let key = read_private_recovery_file(key_path, 512, "recovery key")?;
    let key_text = std::str::from_utf8(key.as_ref()).map_err(|_| CoreError::InvalidKeyMaterial)?;
    let unlock = RecoveryUnlockKey::from_base64(key_text.trim())?;
    Ok(RecoveryBootstrap { kit, unlock })
}

fn read_private_recovery_file(
    path: &Path,
    maximum: u64,
    label: &'static str,
) -> std::result::Result<Zeroizing<Vec<u8>>, CoreError> {
    #[cfg(unix)]
    let (file, length) = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};
        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| CoreError::Io {
            operation: "open private recovery file without following links",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        let stat = fstat(&descriptor).map_err(|error| CoreError::Io {
            operation: "inspect open private recovery file",
            path: path.to_path_buf(),
            source: std::io::Error::from_raw_os_error(error.raw_os_error()),
        })?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum
        {
            return Err(CoreError::InvalidState(format!(
                "{label} must be an owner-only regular file no larger than {maximum} bytes"
            )));
        }
        (File::from(descriptor), stat.st_size as u64)
    };
    #[cfg(not(unix))]
    let (file, length) = {
        let metadata = std::fs::symlink_metadata(path).map_err(|source| CoreError::Io {
            operation: "inspect private recovery file",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
            return Err(CoreError::InvalidState(format!(
                "{label} must be a regular file no larger than {maximum} bytes"
            )));
        }
        let file = File::open(path).map_err(|source| CoreError::Io {
            operation: "open private recovery file",
            path: path.to_path_buf(),
            source,
        })?;
        (file, metadata.len())
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(length as usize));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| CoreError::Io {
            operation: "read private recovery file",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() as u64 > maximum {
        return Err(CoreError::ResourceLimit(label));
    }
    Ok(bytes)
}

/// A local API secret that can only be borrowed by the node-owning process.
///
/// This type intentionally implements neither `Clone` nor `Serialize`.  Its
/// `Debug` representation is permanently redacted.
pub struct RuntimeApiToken(Zeroizing<String>);

impl RuntimeApiToken {
    fn new(value: Zeroizing<String>) -> Self {
        Self(value)
    }

    /// Borrows the bearer token for an immediate authenticated local request.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for RuntimeApiToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RuntimeApiToken([REDACTED])")
    }
}

/// Explicit configuration for a reusable local node.
pub struct NodeRuntimeConfig {
    /// Private durable node state directory.
    pub data_directory: PathBuf,
    /// Loopback-only HTTP management socket.  Port zero requests an ephemeral port.
    pub api_address: SocketAddr,
    /// QUIC peer socket.  Port zero requests an ephemeral port.
    pub peer_address: SocketAddr,
    /// Public peer address to sign into pairing when binding an unspecified socket.
    /// A zero port is replaced with the bound peer port.
    pub advertised_peer_address: Option<SocketAddr>,
    /// Device name used for a newly-created local configuration.
    pub device_name: String,
    /// Initial LAN discovery preference for a newly-created configuration.
    pub lan_discovery_enabled: bool,
    /// Product readiness tier exposed by the local API.
    pub platform_tier: PlatformTier,
    /// Bounded archive admission policy.
    pub archive_limits: ArchiveLimits,
    /// Provider-side quota and lease policy.
    pub provider_quota_policy: ProviderQuotaPolicy,
    /// Admit remote backup storage requests. Native hosts may keep folder sync
    /// and owner backup/recovery active while their provider switch is off.
    /// Changing this value requires stopping the old runtime first.
    pub local_provider_enabled: bool,
    /// Required platform or explicitly provisioned KEK source.
    pub key_protector: Option<Arc<dyn KeyProtector>>,
    /// Optional owner-loss bootstrap for an empty state directory.
    pub recovery: Option<RecoveryBootstrap>,
    /// Local API token source.  This is never logged.
    pub api_token: LocalApiTokenSource,
    /// Optional private record for an app supervising this runtime.
    pub ready_file: Option<PathBuf>,
    /// Offer the one-shot first-run ownership claim on a node with no owner.
    ///
    /// Off by default. An embedded app provisions its own token through
    /// platform secure storage and must never expose an unauthenticated route
    /// that hands one out, so this is opt-in rather than opt-out: a new caller
    /// that forgets the field gets the safe behaviour.
    pub first_run_claim_enabled: bool,
    /// CA certificate clients should pin, delivered by a successful claim.
    ///
    /// Set when TLS is terminated by a same-host proxy with a private CA, which
    /// is the container deployment. Without it a claim still succeeds and simply
    /// carries no certificate, which is correct for a loopback-only node.
    pub tls_ca_certificate_file: Option<PathBuf>,
    /// Optional host-verified maintained sync engine package. Backup/recovery
    /// stays available if its separate installation needs repair.
    #[cfg(unix)]
    pub folder_sync: Option<crate::sync_engine::FolderSyncRuntimeConfig>,
    /// The host found a sync package but could not verify it. Keep the local
    /// API and backup runtime available with an actionable sync status.
    #[cfg(unix)]
    pub folder_sync_package_invalid: bool,
    /// The native host could not restore all saved folder capabilities.
    #[cfg(unix)]
    pub folder_sync_access_unavailable: bool,
}

impl NodeRuntimeConfig {
    /// Safe defaults for a native host with explicit socket addresses.
    #[must_use]
    pub fn new(
        data_directory: impl Into<PathBuf>,
        api_address: SocketAddr,
        peer_address: SocketAddr,
    ) -> Self {
        Self {
            data_directory: data_directory.into(),
            api_address,
            peer_address,
            advertised_peer_address: None,
            device_name: "Covalent node".to_owned(),
            lan_discovery_enabled: false,
            platform_tier: PlatformTier::Tier1,
            archive_limits: ArchiveLimits::default(),
            provider_quota_policy: ProviderQuotaPolicy::default(),
            local_provider_enabled: true,
            key_protector: None,
            recovery: None,
            api_token: LocalApiTokenSource::Persisted,
            ready_file: None,
            first_run_claim_enabled: false,
            tls_ca_certificate_file: None,
            #[cfg(unix)]
            folder_sync: None,
            #[cfg(unix)]
            folder_sync_package_invalid: false,
            #[cfg(unix)]
            folder_sync_access_unavailable: false,
        }
    }
}

/// Connection details returned only to the owning process.
///
/// The token accessor intentionally avoids `Debug`/`Display` so logging this
/// record cannot disclose local API credentials.
pub struct NodeRuntimeReadyInfo {
    api_base_url: String,
    api_address: SocketAddr,
    peer_address: SocketAddr,
    api_token: RuntimeApiToken,
}

impl NodeRuntimeReadyInfo {
    /// Bound loopback API URL.
    #[must_use]
    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    /// Bound loopback API socket, including an assigned ephemeral port.
    #[must_use]
    pub const fn api_address(&self) -> SocketAddr {
        self.api_address
    }

    /// Bound encrypted peer socket, including an assigned ephemeral port.
    #[must_use]
    pub const fn peer_address(&self) -> SocketAddr {
        self.peer_address
    }

    /// Bearer token for authenticated local API calls by the owning process.
    #[must_use]
    pub fn api_token(&self) -> &RuntimeApiToken {
        &self.api_token
    }
}

struct RuntimeControl {
    recovery: Arc<RecoveryStateStore>,
    shutdown: watch::Sender<bool>,
    completion: Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
}

/// A started local node and its owned Tokio tasks.
pub struct NodeRuntime {
    ready: NodeRuntimeReadyInfo,
    control: Arc<RuntimeControl>,
}

impl NodeRuntime {
    /// Opens state, binds loopback HTTP and QUIC endpoints, then starts serving.
    pub async fn start(configuration: NodeRuntimeConfig) -> Result<Self> {
        let NodeRuntimeConfig {
            data_directory,
            api_address: requested_api_address,
            peer_address: requested_peer_address,
            advertised_peer_address,
            device_name,
            lan_discovery_enabled,
            platform_tier,
            archive_limits,
            provider_quota_policy,
            local_provider_enabled,
            key_protector,
            recovery,
            api_token,
            ready_file,
            first_run_claim_enabled,
            tls_ca_certificate_file,
            #[cfg(unix)]
            folder_sync,
            #[cfg(unix)]
            folder_sync_package_invalid,
            #[cfg(unix)]
            folder_sync_access_unavailable,
        } = configuration;

        let key_protector =
            key_protector.ok_or_else(|| anyhow!("key protection is locked or unavailable"))?;
        std::fs::create_dir_all(&data_directory)
            .with_context(|| format!("create data directory {}", data_directory.display()))?;
        let data_directory = std::fs::canonicalize(&data_directory)
            .with_context(|| format!("canonicalize data directory {}", data_directory.display()))?;
        validate_cleartext_api_bind(requested_api_address)
            .context("validate local API transport")?;
        let listener = tokio::net::TcpListener::bind(requested_api_address)
            .await
            .with_context(|| format!("bind {requested_api_address}"))?;
        let api_address = listener
            .local_addr()
            .context("inspect local API endpoint")?;
        if !api_address.ip().is_loopback() && ready_file.is_some() {
            return Err(anyhow!(
                "an app-owned node readiness file requires a loopback API bind"
            ));
        }

        let mut engine_options = EngineOptions::new(&data_directory);
        engine_options.initial_device_name = device_name;
        engine_options.initial_lan_discovery_enabled = lan_discovery_enabled;
        engine_options.provider_quota_policy = provider_quota_policy;
        engine_options.key_protector = Some(Arc::clone(&key_protector));
        let recovered = recovery.is_some();
        let engine = Arc::new(match recovery {
            Some(recovery) => {
                Engine::recover_from_kit(engine_options, recovery.kit.as_ref(), &recovery.unlock)
                    .context("recover Covalent engine")?
            }
            None => Engine::open(engine_options).context("open Covalent engine")?,
        });
        let recovery_state = Arc::new(
            RecoveryStateStore::open(data_directory.join("recovery-state.json"))
                .context("open recovery progress")?,
        );
        // The marker is published atomically with the recovered identity. A normal
        // restart repairs this handoff if the process stopped before runtime setup.
        let bootstrap_pending = engine.config()?.recovery_bootstrap_pending
            || (recovered
                && recovery_state.status()?.phase
                    == covalent_protocol::RecoveryPhase::NotConfigured);
        if bootstrap_pending {
            persist_recovered_provider_connections(
                &engine,
                &data_directory.join("provider-connections.json"),
            )
            .context("restore signed provider transports")?;
            recovery_state
                .begin(&engine)
                .context("record pending catalog recovery")?;
            engine
                .acknowledge_recovery_bootstrap()
                .context("complete runtime recovery handoff")?;
        }
        let token_path = data_directory.join("local-api-token");
        // Commit the explicit unclaimed/claimed lifecycle before token loading
        // can create the token. Token existence is used only once, to migrate a
        // deployment that predates first-run claiming.
        let claim_startup = if first_run_claim_enabled {
            let token_already_exists = match std::fs::symlink_metadata(&token_path) {
                Ok(_) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(source) => {
                    return Err(source).with_context(|| {
                        format!("inspect local API token {}", token_path.display())
                    });
                }
            };
            let startup = first_run_claim::prepare_claim_lifecycle(
                &data_directory,
                token_already_exists,
                crate::now_unix_ms(),
            )
            .context("prepare first-run claim lifecycle")?;
            if startup != first_run_claim::ClaimStartupState::Unclaimed && !token_already_exists {
                return Err(anyhow!(
                    "claimed node is missing its persisted local API token"
                ));
            }
            Some(startup)
        } else {
            None
        };
        let api_token = match api_token {
            LocalApiTokenSource::Persisted => {
                load_or_create_local_api_token(&token_path, &data_directory, key_protector.as_ref())
                    .context("load local API token")?
            }
            LocalApiTokenSource::Provided(token) => token,
        };
        if api_token.len() < 32 || api_token.len() > 512 {
            return Err(anyhow!("invalid local API token"));
        }
        let first_run_claim = match claim_startup {
            Some(first_run_claim::ClaimStartupState::Unclaimed) => {
                arm_first_run_claim(&data_directory, tls_ca_certificate_file)
                    .context("arm first-run ownership claim")?
            }
            Some(
                first_run_claim::ClaimStartupState::Claimed
                | first_run_claim::ClaimStartupState::RecoveringReplay,
            ) => FirstRunClaim::load_replay(
                &data_directory,
                api_token.as_str(),
                crate::now_unix_ms(),
            )
            .context("recover first-run ownership claim response")?
            .map(Arc::new),
            None => None,
        };

        let tls_identity = TlsIdentity::load_or_create(
            data_directory.join("tls"),
            &data_directory,
            key_protector.as_ref(),
        )
        .context("load QUIC identity")?;
        let discovery_enabled = engine
            .config()
            .context("load persisted discovery preference")?
            .lan_discovery_enabled;
        let quic_node = QuicNode::bind(requested_peer_address, Arc::clone(&engine), &tls_identity)
            .context("bind QUIC peer endpoint")?
            .with_local_provider_enabled(local_provider_enabled);
        let peer_address = quic_node
            .local_addr()
            .context("inspect QUIC peer endpoint")?;
        let static_advertised_peer_address =
            resolve_advertised_peer_address(peer_address, advertised_peer_address)?;
        let discovery = Arc::new(
            DiscoveryController::new_with_provider(
                discovery_enabled,
                peer_address.port(),
                local_provider_enabled,
            )
            .context("start LAN discovery controller")?,
        );
        let mut state = AppState::new(Arc::clone(&engine), platform_tier, api_token.to_string())
            .context("create local API state")?
            .with_archive_limits(archive_limits)
            .context("validate archive resource limits")?
            .with_transport_certificate(tls_identity.certificate_der().to_vec())
            .with_discovery_controller(Arc::clone(&discovery))
            .with_provider_state(data_directory.join("provider-connections.json"))
            .context("load remembered provider connections")?;
        let startup_recovery = recovery_state
            .should_retry()
            .context("inspect recovery progress")?
            .then(|| (Arc::clone(&recovery_state), Arc::clone(&engine)));
        state = state.with_recovery_state(Arc::clone(&recovery_state));
        #[cfg(unix)]
        if folder_sync_access_unavailable {
            state = state.with_folder_sync(
                crate::sync_engine::FolderSyncRuntimeConfig::prepare_access_recovery(
                    &data_directory,
                    Arc::clone(&engine),
                    Arc::clone(&key_protector),
                ),
            );
        } else if folder_sync_package_invalid {
            state =
                state.with_folder_sync(crate::sync_engine::FolderSyncRuntimeState::NeedsAttention);
        } else if let Some(folder_sync) = folder_sync {
            state = state.with_folder_sync(
                folder_sync
                    .prepare(
                        &data_directory,
                        Arc::clone(&engine),
                        Arc::clone(&key_protector),
                        static_advertised_peer_address,
                    )
                    .await,
            );
        }
        if let Some(address) = static_advertised_peer_address {
            state = state.with_peer_address(address);
        }
        if let Some(claim) = first_run_claim {
            state = state.with_first_run_claim(claim);
        }
        // The pairing-only ALPN shares the advertised QUIC endpoint, so the
        // address a peer discovers is the exact address it must dial to pair.
        let pairing_service = NetworkPairingService::open(
            Arc::clone(&engine),
            state.network_pairing_manager(),
            state.local_transport_binding().ok(),
            data_directory.join("pairing-start-admissions.json"),
        )
        .context("open pairing Start admission state")?;
        let quic_node = quic_node.with_pairing_service(Arc::new(pairing_service));
        #[cfg(unix)]
        let quic_node = match &state.folder_sync {
            crate::sync_engine::FolderSyncRuntimeState::Ready(service) => {
                quic_node.with_folder_service(Arc::clone(service))
            }
            _ => quic_node,
        };

        if let Some(path) = ready_file.as_deref()
            && let Err(error) = write_node_ready_file(
                path,
                &NodeReadyInfo {
                    schema_version: 1,
                    api_base_url: format!("http://{api_address}"),
                    peer_address,
                    process_id: std::process::id(),
                },
            )
        {
            let _ = discovery.set_enabled(false);
            return Err(error).context("publish node readiness");
        }

        let (shutdown, shutdown_receiver) = watch::channel(false);
        let api_base_url = format!("http://{api_address}");
        let runtime_token = RuntimeApiToken::new(api_token);
        let task_ready_file = ready_file.clone();
        let supervisor_shutdown = shutdown.clone();
        let supervisor_recovery = Arc::clone(&recovery_state);
        let completion = tokio::spawn(async move {
            supervise_runtime(
                listener,
                state,
                quic_node,
                discovery,
                shutdown_receiver,
                supervisor_shutdown,
                SupervisorStartup {
                    ready_file: task_ready_file,
                    recovery: startup_recovery,
                    recovery_control: supervisor_recovery,
                },
            )
            .await
        });
        info!(listen = %api_address, peer_bind_address = %peer_address, ?static_advertised_peer_address, data_dir = %data_directory.display(), "Covalent node ready");

        Ok(Self {
            ready: NodeRuntimeReadyInfo {
                api_base_url,
                api_address,
                peer_address,
                api_token: runtime_token,
            },
            control: Arc::new(RuntimeControl {
                recovery: recovery_state,
                shutdown,
                completion: Mutex::new(Some(completion)),
            }),
        })
    }

    /// Private connection details for the process that started this node.
    #[must_use]
    pub fn ready_info(&self) -> &NodeRuntimeReadyInfo {
        &self.ready
    }

    /// Requests graceful HTTP shutdown and waits for all owned tasks to exit.
    ///
    /// Repeated and concurrent calls are safe. The first waiter receives the
    /// supervisor's completion result; every other waiter returns only after
    /// that shutdown and its endpoint cleanup have completed.
    pub async fn stop(&self) -> Result<()> {
        self.control.recovery.cancel();
        let _ = self.control.shutdown.send(true);
        let mut completion = self.control.completion.lock().await;
        let result = match completion.as_mut() {
            Some(completion) => completion
                .await
                .context("join node runtime")
                .and_then(|result| result),
            None => Ok(()),
        };
        *completion = None;
        result
    }
}

impl Drop for NodeRuntime {
    fn drop(&mut self) {
        self.control.recovery.cancel();
        let _ = self.control.shutdown.send(true);
    }
}

struct SupervisorStartup {
    recovery_control: Arc<RecoveryStateStore>,
    ready_file: Option<PathBuf>,
    recovery: Option<(Arc<RecoveryStateStore>, Arc<Engine>)>,
}

async fn supervise_runtime(
    listener: tokio::net::TcpListener,
    state: AppState,
    quic_node: QuicNode,
    discovery: Arc<DiscoveryController>,
    shutdown: watch::Receiver<bool>,
    shutdown_sender: watch::Sender<bool>,
    startup: SupervisorStartup,
) -> Result<()> {
    #[cfg(unix)]
    let folder_sync = state.folder_sync.clone();
    #[cfg(unix)]
    let delivery_task = match &folder_sync {
        crate::sync_engine::FolderSyncRuntimeState::Ready(service) => {
            Some(tokio::spawn(crate::sync_delivery::run(
                Arc::clone(&state.engine),
                Arc::clone(service),
                shutdown.clone(),
            )))
        }
        _ => None,
    };
    let mut http_task = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(wait_for_shutdown(shutdown))
            .await
            .context("serve local API")
    });
    let quic_shutdown = quic_node.shutdown_handle();
    let mut quic_task = tokio::spawn(quic_node.run());
    let recovery_task = startup
        .recovery
        .map(|(store, engine)| tokio::task::spawn_blocking(move || store.retry(&engine)));

    let result = tokio::select! {
        result = &mut http_task => {
            startup.recovery_control.cancel();
            let result = result.context("join local API task").and_then(|result| result);
            quic_shutdown.close();
            let quic_result = quic_task.await.context("join QUIC peer task");
            result.and(quic_result)
        },
        result = &mut quic_task => {
            startup.recovery_control.cancel();
            let quic_result = result.context("join QUIC peer task");
            let _ = shutdown_sender.send(true);
            let http_result = http_task.await.context("join local API task").and_then(|result| result);
            quic_result.and(http_result)
        }
    };
    // The serving task can also finish because it was cancelled or panicked.
    // Close is idempotent and must precede wait_idle in every exit path.
    quic_shutdown.close();
    let quic_release_result = quic_shutdown
        .wait_for_release()
        .await
        .context("release QUIC peer endpoint");

    let discovery_result = discovery.set_enabled(false).context("stop LAN discovery");
    #[cfg(unix)]
    let delivery_result = match delivery_task {
        Some(delivery_task) => {
            let _ = shutdown_sender.send(true);
            delivery_task
                .await
                .context("join folder invitation delivery")
        }
        None => Ok(()),
    };
    #[cfg(unix)]
    let folder_sync_result = stop_folder_sync(folder_sync).await;
    if let Some(task) = recovery_task {
        match task.await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => warn!(%error, "provider catalog recovery remains pending"),
            Err(error) => warn!(%error, "provider catalog recovery worker failed"),
        }
    }
    let readiness_result = match startup.ready_file {
        Some(path) => {
            remove_node_ready_file(&path, std::process::id()).context("remove node readiness")
        }
        None => Ok(()),
    };

    result?;
    quic_release_result?;
    discovery_result?;
    #[cfg(unix)]
    delivery_result?;
    #[cfg(unix)]
    folder_sync_result?;
    readiness_result
}

#[cfg(unix)]
async fn stop_folder_sync(state: crate::sync_engine::FolderSyncRuntimeState) -> Result<()> {
    use crate::sync_engine::{FolderSyncLifecycle, FolderSyncRuntimeState, FolderSyncServiceError};
    let FolderSyncRuntimeState::Ready(service) = state else {
        return Ok(());
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            match service.stop().await {
                Ok(FolderSyncLifecycle::Stopped) => return Ok(()),
                Ok(FolderSyncLifecycle::StillStopping) | Err(FolderSyncServiceError::Busy) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                _ => return Err(anyhow!("folder sync shutdown needs attention")),
            }
        }
    })
    .await
    .map_err(|_| anyhow!("folder sync worker is still stopping"))?;
    // Drop closes the exact lifeline even when shutdown failed. The dedicated
    // reaper retains installation/root leases until actual child exit.
    drop(service);
    result
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
}

const FIRST_RUN_CLAIM_GUIDANCE: [&str; 4] = [
    "  Use the Covalent CLI only (not native/web):",
    "  covalent claim --https-url <HTTPS_URL> \\",
    "    --setup-code-file <PATH> --output-dir <PATH>",
    "  The web console accepts only the resulting token.",
];

/// Mints a first-run code when this node has no owner, and prints it.
///
/// Returns `None` — silently and correctly — when the node is already owned.
/// The banner goes to stdout rather than through `tracing` deliberately: it is
/// the one message whose whole purpose is to be read by a person in a container
/// log viewer, and no log filter should be able to suppress it. The code itself
/// is dropped as soon as it is printed; only its stretched key survives.
fn arm_first_run_claim(
    data_directory: &std::path::Path,
    tls_ca_certificate_file: Option<PathBuf>,
) -> Result<Option<Arc<FirstRunClaim>>> {
    let marker_path = first_run_claim::owner_marker_path(data_directory);
    if first_run_claim::is_claimed(&marker_path) {
        return Ok(None);
    }

    let code = ClaimCode::mint();
    let claim = Arc::new(FirstRunClaim::new(
        &code,
        first_run_claim::claim_lifecycle_path(data_directory),
        marker_path,
        tls_ca_certificate_file,
        crate::now_unix_ms(),
    ));
    let minutes = first_run_claim::CLAIM_WINDOW_MS / 60_000;
    // Width is fixed and every line is padded to it, so the box does not skew
    // when a value changes length. An operator reading a Docker log is already
    // hunting through JSON noise; a clean box is what makes this findable.
    const WIDTH: usize = 54;
    let rule = "─".repeat(WIDTH);
    let row = |text: &str| println!("  │{text:<WIDTH$}│");
    println!();
    println!("  ┌{rule}┐");
    row("  Covalent setup code");
    row("");
    row(&format!("      {}", code.grouped()));
    row("");
    for line in FIRST_RUN_CLAIM_GUIDANCE {
        row(line);
    }
    row("");
    row(&format!("  Valid for {minutes} minutes, and usable once."));
    row("  Restart this container for a new code.");
    println!("  └{rule}┘");
    println!();
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    drop(code);
    Ok(Some(claim))
}

/// Determines the endpoint peers are told to dial.
///
/// Until this function existed, `advertised_peer_address` was set in exactly one
/// place in the repository — the integration test harness — so `peer_address`
/// was `None` on every real deployment and `AppState::local_transport_binding`
/// failed on all of them. `GET /api/v1/discovery` and
/// `GET /api/v1/transport/identity` answered 500, and
/// `POST /api/v1/pair/invitations` answered 400 `invalid_contract`, which is why
/// the failure read as a schema drift for sixty CI runs rather than as the
/// missing configuration it was. The integration test passed throughout because
/// it set the field production never set.
///
/// Auto-detection is therefore the default rather than an opt-in, and a node
/// that cannot determine a usable address refuses to advertise one at all. See
/// [`crate::advertised_address`] for why advertising a wrong address is worse
/// than advertising none.
fn resolve_advertised_peer_address(
    bound_address: SocketAddr,
    configured_address: Option<SocketAddr>,
) -> Result<Option<SocketAddr>> {
    if configured_address.is_some_and(|address| address.ip().is_unspecified()) {
        return Err(anyhow!(
            "an advertised QUIC peer address must not be unspecified"
        ));
    }
    let observed = advertised_address::observed_interface_addresses();
    let in_container = advertised_address::running_in_container();
    match advertised_address::resolve_advertised_endpoint(
        bound_address,
        configured_address,
        &observed,
        in_container,
    ) {
        Ok(address) if address.port() == 0 => {
            Err(anyhow!("advertised QUIC peer address must have a port"))
        }
        Ok(address) => Ok(Some(address)),
        Err(refusal) => {
            // Not a startup failure. The node still serves backups, restores and
            // the local console; only device-to-device pairing is unavailable,
            // and the routes that need the endpoint say so specifically. Warning
            // loudly here means the operator sees the remedy in the container
            // log at the moment it becomes relevant.
            tracing::warn!(
                guidance = %refusal.operator_guidance(),
                "no advertised peer endpoint; pairing with other devices is unavailable"
            );
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Duration;

    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use covalent_core::{Engine, EngineOptions, RecoveryUnlockKey, StaticKeyProtector};
    use covalent_protocol::{PeerRole, TransportBinding};
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use zeroize::Zeroizing;

    use super::{NodeRuntime, NodeRuntimeConfig, RecoveryBootstrap};

    fn loopback_zero() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    fn test_configuration(directory: &TempDir) -> NodeRuntimeConfig {
        let mut configuration =
            NodeRuntimeConfig::new(directory.path(), loopback_zero(), loopback_zero());
        configuration.key_protector = Some(Arc::new(
            StaticKeyProtector::new(1, [0xa1; 32]).expect("test protector"),
        ));
        configuration
    }

    async fn start_runtime(directory: &TempDir) -> NodeRuntime {
        NodeRuntime::start(test_configuration(directory))
            .await
            .expect("start runtime")
    }

    #[tokio::test]
    async fn disabled_provider_keeps_owner_api_and_folder_status_available() {
        let directory = tempfile::tempdir().unwrap();
        let mut configuration = test_configuration(&directory);
        configuration.local_provider_enabled = false;
        let runtime = NodeRuntime::start(configuration).await.unwrap();
        let ready = runtime.ready_info();
        for path in [
            "/api/v1/status",
            "/api/v1/backups",
            "/api/v1/recovery/status",
        ] {
            let response = request(ready.api_address(), &format!(
                "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                ready.api_token().expose(),
            )).await;
            assert!(response.starts_with("HTTP/1.1 200"), "{path}: {response}");
        }
        #[cfg(unix)]
        assert_eq!(sync_status(&runtime).await["schemaVersion"], 1);
        runtime.stop().await.unwrap();
    }

    async fn request(address: SocketAddr, request: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect local API");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write request");
        stream.flush().await.expect("flush request");
        let mut response = String::new();
        stream
            .read_to_string(&mut response)
            .await
            .expect("read response");
        response
    }

    #[cfg(unix)]
    fn sync_configuration(directory: &TempDir) -> NodeRuntimeConfig {
        use crate::sync_engine::{FolderSyncRuntimeConfig, VerifiedEngineExecutable};
        use std::os::unix::fs::PermissionsExt as _;
        let mut configuration = test_configuration(directory);
        let executable = directory.path().join("unused-folder-helper");
        let bytes = b"#!/bin/sh\nexit 99\n";
        fs::write(&executable, bytes).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = fs::canonicalize(executable).unwrap();
        configuration.folder_sync = Some(FolderSyncRuntimeConfig {
            guardian: VerifiedEngineExecutable::open(&executable, Sha256::digest(bytes).into())
                .unwrap(),
            worker: VerifiedEngineExecutable::open(&executable, Sha256::digest(bytes).into())
                .unwrap(),
            runtime_parent: fs::canonicalize(directory.path()).unwrap(),
            listener: "127.0.0.1:43871".parse().unwrap(),
            advertised_address: Some("127.0.0.1:43871".parse().unwrap()),
        });
        configuration
    }

    #[cfg(unix)]
    async fn sync_status(runtime: &NodeRuntime) -> serde_json::Value {
        let ready = runtime.ready_info();
        let response = request(ready.api_address(), &format!(
            "GET /api/v1/sync/status HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
            ready.api_token().expose(),
        )).await;
        assert!(response.contains(" 200 "), "{response}");
        serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unavailable_folder_engine_preserves_backup_and_never_creates_sync_state() {
        for (access_unavailable, expected_issue) in
            [(false, "installation"), (true, "folderAccess")]
        {
            let directory = TempDir::new().unwrap();
            let mut configuration = sync_configuration(&directory);
            // Host failures win even with a contradictory verified package.
            configuration.folder_sync_package_invalid = true;
            configuration.folder_sync_access_unavailable = access_unavailable;
            let runtime = NodeRuntime::start(configuration).await.unwrap();
            let status = sync_status(&runtime).await;
            assert_eq!(status["availability"], "needsAttention");
            assert_eq!(status["issue"], expected_issue);
            assert!(!directory.path().join("folder-sync").exists());
            let health = request(
                runtime.ready_info().api_address(),
                "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await;
            assert!(health.contains(" 200 "));
            runtime.stop().await.unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn access_unavailable_opens_only_existing_authenticated_journal() {
        let directory = TempDir::new().unwrap();
        let runtime = NodeRuntime::start(sync_configuration(&directory))
            .await
            .unwrap();
        runtime.stop().await.unwrap();
        drop(runtime);

        let mut configuration = sync_configuration(&directory);
        configuration.folder_sync_access_unavailable = true;
        let runtime = NodeRuntime::start(configuration).await.unwrap();
        let status = sync_status(&runtime).await;
        assert_eq!(status["availability"], "needsAttention");
        assert_eq!(status["issue"], "folderAccess");
        assert_eq!(status["shares"], serde_json::json!([]));
        let body = r#"{"offerId":"00000000-0000-0000-0000-000000000001","selectedRoot":"/"}"#;
        let unauthenticated = request(
            runtime.ready_info().api_address(),
            &format!(
                "POST /api/v1/sync/repair HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ),
        )
        .await;
        assert!(unauthenticated.contains(" 401 "));
        let denied = request(
            runtime.ready_info().api_address(),
            &format!(
                "POST /api/v1/sync/repair HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                runtime.ready_info().api_token().expose(),
                body.len(),
            ),
        )
        .await;
        assert!(denied.contains(" 409 "));
        runtime.stop().await.unwrap();
        drop(runtime);

        fs::write(
            directory.path().join("folder-sync/folder-sharing.v1"),
            b"damaged",
        )
        .unwrap();
        let mut configuration = sync_configuration(&directory);
        configuration.folder_sync_access_unavailable = true;
        let runtime = NodeRuntime::start(configuration).await.unwrap();
        let status = sync_status(&runtime).await;
        assert_eq!(status["availability"], "needsAttention");
        assert_eq!(status["issue"], "installation");
        runtime.stop().await.unwrap();

        let initialization = TempDir::new().unwrap();
        let runtime = NodeRuntime::start(sync_configuration(&initialization))
            .await
            .unwrap();
        runtime.stop().await.unwrap();
        drop(runtime);
        fs::remove_file(initialization.path().join("folder-sync/folder-sharing.v1")).unwrap();
        let mut configuration = sync_configuration(&initialization);
        configuration.folder_sync_access_unavailable = true;
        let runtime = NodeRuntime::start(configuration).await.unwrap();
        let status = sync_status(&runtime).await;
        assert_eq!(status["issue"], "folderAccess");
        assert_eq!(status["shares"], serde_json::json!([]));
        assert!(
            !initialization
                .path()
                .join("folder-sync/folder-sharing.v1")
                .exists()
        );
        runtime.stop().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn packaged_folder_sync_bootstraps_once_and_damage_preserves_backup_runtime() {
        let directory = TempDir::new().unwrap();
        let runtime = NodeRuntime::start(sync_configuration(&directory))
            .await
            .unwrap();
        let denied = request(
            runtime.ready_info().api_address(),
            "GET /api/v1/sync/status HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(denied.contains(" 401 "));
        let first = sync_status(&runtime).await;
        assert_eq!(first["availability"], "available");
        assert_eq!(first["lifecycle"], "stopped");
        assert_eq!(first["healthFreshness"], "neverObserved");
        assert_eq!(first["connectionFreshness"], "neverObserved");
        assert_eq!(first["shares"], serde_json::json!([]));
        let root = directory.path().join("folder-sync");
        let identity = fs::read(root.join("engine-identity.v1")).unwrap();
        runtime.stop().await.unwrap();
        drop(runtime);

        let runtime = NodeRuntime::start(sync_configuration(&directory))
            .await
            .unwrap();
        assert_eq!(sync_status(&runtime).await, first);
        runtime.stop().await.unwrap();
        drop(runtime);
        assert_eq!(fs::read(root.join("engine-identity.v1")).unwrap(), identity);

        // Missing consent state must not turn into an empty writable default.
        fs::remove_file(root.join("folder-sharing.v1")).unwrap();
        let runtime = NodeRuntime::start(sync_configuration(&directory))
            .await
            .unwrap();
        let unavailable = sync_status(&runtime).await;
        assert_eq!(unavailable["availability"], "needsAttention");
        assert_eq!(unavailable["issue"], "installation");
        assert!(!root.join("folder-sharing.v1").exists());
        assert_eq!(fs::read(root.join("engine-identity.v1")).unwrap(), identity);
        let health = request(
            runtime.ready_info().api_address(),
            "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(health.contains(" 200 "));
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn starts_on_ephemeral_ports_and_serves_health_and_authenticated_api() {
        let directory = TempDir::new().expect("temp directory");
        let runtime = start_runtime(&directory).await;
        let ready = runtime.ready_info();
        assert_eq!(ready.api_address().ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(ready.api_address().port(), 0);
        assert_ne!(ready.peer_address().port(), 0);
        assert!(ready.api_base_url().starts_with("http://127.0.0.1:"));

        let health = request(
            ready.api_address(),
            "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(health.contains(" 200 "), "{health}");
        let denied = request(
            ready.api_address(),
            "POST /api/v1/config/export HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(denied.contains(" 401 "), "{denied}");
        let authorized = request(
            ready.api_address(),
            &format!(
                "POST /api/v1/config/export HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ready.api_token().expose()
            ),
        )
        .await;
        assert!(authorized.contains(" 200 "), "{authorized}");
        runtime.stop().await.expect("stop runtime");
    }

    #[tokio::test]
    async fn stock_container_wildcard_peer_bind_starts_without_manual_advertise_address() {
        let directory = TempDir::new().expect("temp directory");
        let mut configuration = test_configuration(&directory);
        configuration.peer_address = "0.0.0.0:0".parse().expect("wildcard peer address");
        let runtime = NodeRuntime::start(configuration)
            .await
            .expect("stock Docker/Unraid peer bind starts");
        assert!(runtime.ready_info().peer_address().ip().is_unspecified());
        assert_ne!(runtime.ready_info().peer_address().port(), 0);
        runtime.stop().await.expect("stop wildcard node");
    }

    /// DELIBERATE CHANGE OF EXPECTATION, not a relaxed assertion.
    ///
    /// This test previously asserted that a wildcard bind with no configured
    /// address resolves to `None`. That was the behaviour, and the behaviour was
    /// the bug: `None` is what left `AppState::peer_address` unset on every real
    /// deployment, so `transport/identity` and `discovery` answered 500 and
    /// `pair/invitations` answered `invalid_contract`. Pinning it kept the
    /// defect green. A wildcard bind must now auto-detect.
    #[test]
    fn wildcard_peer_bind_auto_detects_an_advertised_endpoint() {
        use crate::advertised_address as selection;
        let expected = selection::select_advertised_address(
            &selection::observed_interface_addresses(),
            selection::running_in_container(),
        )
        .ok()
        .map(|address| SocketAddr::new(address, 8787));
        assert_eq!(
            super::resolve_advertised_peer_address(
                "0.0.0.0:8787".parse().expect("bind address"),
                None,
            )
            .expect("wildcard bind is valid"),
            expected,
            "a wildcard bind must advertise what selection chose, and refuse only when \
             selection refuses"
        );
        assert_eq!(
            super::resolve_advertised_peer_address(
                "0.0.0.0:8787".parse().expect("bind address"),
                Some("192.0.2.10:0".parse().expect("advertised address")),
            )
            .expect("concrete advertised endpoint"),
            Some("192.0.2.10:8787".parse().expect("expected endpoint")),
        );
    }

    #[tokio::test]
    async fn stop_is_idempotent_and_reopen_keeps_private_state() {
        let directory = TempDir::new().expect("temp directory");
        let runtime = start_runtime(&directory).await;
        let initial_token = runtime.ready_info().api_token().expose().to_owned();
        runtime.stop().await.expect("first stop");
        runtime.stop().await.expect("second stop");

        let reopened = start_runtime(&directory).await;
        assert_eq!(reopened.ready_info().api_token().expose(), initial_token);
        reopened.stop().await.expect("stop reopened runtime");
    }

    #[tokio::test]
    async fn stop_releases_quic_port_before_fixed_address_restart() {
        let directory = TempDir::new().expect("temp directory");
        let runtime = start_runtime(&directory).await;
        let peer_address = runtime.ready_info().peer_address();
        runtime.stop().await.expect("stop first runtime");

        let mut configuration = test_configuration(&directory);
        configuration.peer_address = peer_address;
        let restarted = NodeRuntime::start(configuration)
            .await
            .expect("restart immediately on released QUIC address");
        assert_eq!(restarted.ready_info().peer_address(), peer_address);
        restarted.stop().await.expect("stop restarted runtime");
    }

    #[tokio::test]
    async fn caller_provided_api_token_is_never_persisted() {
        let directory = TempDir::new().expect("temp directory");
        let provided = "caller-provided-api-token-with-at-least-thirty-two-bytes";
        let mut configuration = test_configuration(&directory);
        configuration.api_token =
            super::LocalApiTokenSource::Provided(zeroize::Zeroizing::new(provided.to_owned()));
        let runtime = NodeRuntime::start(configuration)
            .await
            .expect("start with provided token");
        assert_eq!(runtime.ready_info().api_token().expose(), provided);
        assert!(!directory.path().join("local-api-token").exists());
        runtime.stop().await.expect("stop runtime");
    }

    #[tokio::test]
    async fn drop_interrupts_runtime_and_removes_readiness_without_leaking_ports() {
        let directory = TempDir::new().expect("temp directory");
        let ready_file = directory.path().join("runtime-ready.json");
        let mut configuration = test_configuration(&directory);
        configuration.ready_file = Some(ready_file.clone());
        let runtime = NodeRuntime::start(configuration)
            .await
            .expect("start runtime");
        let api_address = runtime.ready_info().api_address();
        assert!(ready_file.exists());
        drop(runtime);

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !ready_file.exists() && tokio::net::TcpListener::bind(api_address).await.is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("drop cleanup");
    }

    #[test]
    fn token_debug_is_permanently_redacted() {
        let secret = "this-local-token-is-long-enough-to-authenticate".to_owned();
        let token = super::RuntimeApiToken::new(zeroize::Zeroizing::new(secret.clone()));
        let rendered = format!("{token:?}");
        assert!(rendered.contains("REDACTED"));
        assert!(!rendered.contains(&secret));
    }

    #[test]
    fn first_run_banner_directs_claiming_only_to_capable_clients() {
        let guidance = super::FIRST_RUN_CLAIM_GUIDANCE.join("\n");
        assert!(guidance.contains("Covalent CLI only (not native/web)"));
        assert!(guidance.contains("covalent claim --https-url <HTTPS_URL>"));
        assert!(guidance.contains("--setup-code-file <PATH> --output-dir <PATH>"));
        assert!(guidance.contains("web console accepts only the resulting token"));
        assert!(!guidance.contains("native app to claim"));
    }

    #[tokio::test]
    async fn startup_without_an_injected_protector_fails_locked_before_writing_state() {
        let parent = TempDir::new().expect("temp directory");
        let directory = parent.path().join("missing-state");
        let error = match NodeRuntime::start(NodeRuntimeConfig::new(
            &directory,
            loopback_zero(),
            loopback_zero(),
        ))
        .await
        {
            Ok(_) => panic!("unprotected startup must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("key protection is locked"));
        assert!(!directory.exists(), "locked startup must not create state");
    }

    #[tokio::test]
    async fn recovery_bootstrap_recreates_identity_and_activates_kit_provider_transports() {
        let root = TempDir::new().expect("root");
        let owner_path = root.path().join("lost-owner");
        let provider_path = root.path().join("provider");
        let recovered_path = root.path().join("recovered-owner");
        let protector =
            || Arc::new(StaticKeyProtector::new(1, [0xa1; 32]).expect("test protector"));
        let named_options = |path: &std::path::Path, name: &str| {
            let mut options = EngineOptions::new(path).with_key_protector(protector());
            options.initial_device_name = name.to_owned();
            options
        };
        let owner = Engine::open(named_options(&owner_path, "Recovered owner")).expect("owner");
        let provider =
            Engine::open(named_options(&provider_path, "Recovery provider")).expect("provider");
        let binding =
            |engine: &Engine, name: &str, address: &str, certificate: &[u8]| TransportBinding {
                peer_id: engine.device_id(),
                display_name: name.to_owned(),
                address: address.to_owned(),
                certificate_der: URL_SAFE_NO_PAD.encode(certificate),
                certificate_fingerprint: Sha256::digest(certificate)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            };
        let owner_binding = binding(&owner, "Recovered owner", "127.0.0.1:41001", b"owner cert");
        let provider_binding = binding(
            &provider,
            "Recovery provider",
            "127.0.0.1:41002",
            b"provider cert",
        );
        let invitation = owner
            .pairing_manager()
            .create_invitation_with_transport(1_000, 60_000, Vec::new(), owner_binding)
            .expect("invitation");
        let mut session = provider
            .accept_pairing_with_transport(
                invitation,
                provider_binding,
                BTreeSet::from([PeerRole::StorageProvider]),
                BTreeSet::from([PeerRole::BackupReader, PeerRole::BackupWriter]),
                2_000,
            )
            .expect("accept pairing");
        let code = session.authentication_string().to_string();
        provider
            .confirm_pairing_as_responder(&mut session, &code, 2_000)
            .expect("provider confirmation");
        owner
            .confirm_pairing_as_inviter(&mut session, &code, 2_000)
            .expect("owner confirmation");
        owner
            .finalize_pairing_as_inviter(&session, 2_000)
            .expect("owner provider grant");
        provider
            .finalize_pairing_as_responder(&session, 2_000)
            .expect("provider owner grant");
        let owner_id = owner.device_id();
        let provider_id = provider.device_id();
        let unlock = RecoveryUnlockKey::generate();
        let kit = owner.export_recovery_kit(&unlock).expect("recovery kit");
        drop(owner);
        fs::remove_dir_all(&owner_path).expect("destroy owner state");

        let mut configuration =
            NodeRuntimeConfig::new(&recovered_path, loopback_zero(), loopback_zero());
        configuration.key_protector = Some(protector());
        configuration.recovery = Some(RecoveryBootstrap {
            kit: Zeroizing::new(kit),
            unlock,
        });
        let runtime = NodeRuntime::start(configuration)
            .await
            .expect("recover node runtime");
        let recovered_identity: serde_json::Value = serde_json::from_slice(
            &fs::read(recovered_path.join("identity.json")).expect("identity"),
        )
        .expect("identity JSON");
        assert_eq!(recovered_identity["deviceId"], owner_id.to_string());
        assert_eq!(recovered_identity["schemaVersion"], 2);
        assert!(recovered_identity.get("privateKey").is_none());
        let providers: serde_json::Value = serde_json::from_slice(
            &fs::read(recovered_path.join("provider-connections.json"))
                .expect("provider connections"),
        )
        .expect("provider JSON");
        assert!(
            providers["providers"]
                .get(provider_id.to_string())
                .is_some()
        );
        let tls: serde_json::Value = serde_json::from_slice(
            &fs::read(recovered_path.join("tls/identity.json")).expect("TLS identity"),
        )
        .expect("TLS JSON");
        assert_eq!(tls["schemaVersion"], 2);
        assert!(tls.get("privateKeyDer").is_none());
        assert!(tls.get("protectedPrivateKey").is_some());
        let recovery_status = request(
            runtime.ready_info().api_address(),
            &format!(
                "GET /api/v1/recovery/status HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                runtime.ready_info().api_token().expose()
            ),
        )
        .await;
        assert!(recovery_status.contains(" 200 "), "{recovery_status}");
        assert!(recovery_status.contains("\"phase\":"), "{recovery_status}");
        runtime.stop().await.expect("stop recovered runtime");
        let durable_recovery: serde_json::Value = serde_json::from_slice(
            &fs::read(recovered_path.join("recovery-state.json")).expect("recovery state"),
        )
        .expect("recovery JSON");
        assert_eq!(durable_recovery["schemaVersion"], 1);
        // Immediate shutdown can cancel recovery before the unavailable
        // provider has been contacted. Both states must remain retryable;
        // neither may be reported as completed or lose the provider roster.
        assert!(matches!(
            durable_recovery["status"]["phase"].as_str(),
            Some("pending" | "blocked")
        ));
        assert!(
            crate::recovery_state::RecoveryStateStore::open(
                recovered_path.join("recovery-state.json")
            )
            .expect("reopen durable recovery progress")
            .should_retry()
            .expect("interrupted recovery stays retryable")
        );
        assert_eq!(
            durable_recovery["status"]["configuredProviderIds"],
            serde_json::json!([provider_id])
        );
        let encoded = serde_json::to_string(&durable_recovery).expect("serialize status");
        assert!(!encoded.contains("recoveryKey"));
        assert!(!encoded.contains("recoveryKit"));
    }
}
