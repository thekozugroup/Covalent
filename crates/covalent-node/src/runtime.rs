//! Reusable lifecycle for an in-process Covalent node.
//!
//! Native applications own this runtime instead of spawning the command-line
//! binary.  The runtime deliberately has no signal handling: binaries decide
//! which OS lifecycle events should request shutdown, while embedded callers
//! call [`NodeRuntime::stop`] or let the handle drop.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use covalent_core::{Engine, EngineOptions, KeyProtector, ProviderQuotaPolicy, RecoveryUnlockKey};
use covalent_protocol::PlatformTier;
use tokio::sync::{Mutex, watch};
use tracing::info;
use zeroize::Zeroizing;

use crate::advertised_address;
use crate::discovery::DiscoveryController;
use crate::first_run_claim::{self, ClaimCode, FirstRunClaim};
use crate::pairing_transport::NetworkPairingService;
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
    pub kit: Vec<u8>,
    /// High-entropy secret held outside the lost node state.
    pub unlock: RecoveryUnlockKey,
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
            key_protector: None,
            recovery: None,
            api_token: LocalApiTokenSource::Persisted,
            ready_file: None,
            first_run_claim_enabled: false,
            tls_ca_certificate_file: None,
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
            key_protector,
            recovery,
            api_token,
            ready_file,
            first_run_claim_enabled,
            tls_ca_certificate_file,
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
                Engine::recover_from_kit(engine_options, &recovery.kit, &recovery.unlock)
                    .context("recover Covalent engine")?
            }
            None => Engine::open(engine_options).context("open Covalent engine")?,
        });
        if recovered {
            persist_recovered_provider_connections(
                &engine,
                &data_directory.join("provider-connections.json"),
            )
            .context("restore signed provider transports")?;
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
            .context("bind QUIC peer endpoint")?;
        let peer_address = quic_node
            .local_addr()
            .context("inspect QUIC peer endpoint")?;
        let static_advertised_peer_address =
            resolve_advertised_peer_address(peer_address, advertised_peer_address)?;
        let discovery = Arc::new(
            DiscoveryController::new(discovery_enabled, peer_address.port())
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
        let completion = tokio::spawn(async move {
            supervise_runtime(
                listener,
                state,
                quic_node,
                discovery,
                shutdown_receiver,
                supervisor_shutdown,
                task_ready_file,
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
    /// Repeated calls are safe.  The first caller owns the completion result;
    /// later calls observe that shutdown has already been requested.
    pub async fn stop(&self) -> Result<()> {
        let _ = self.control.shutdown.send(true);
        let completion = self.control.completion.lock().await.take();
        match completion {
            Some(completion) => completion.await.context("join node runtime")?,
            None => Ok(()),
        }
    }
}

impl Drop for NodeRuntime {
    fn drop(&mut self) {
        let _ = self.control.shutdown.send(true);
    }
}

async fn supervise_runtime(
    listener: tokio::net::TcpListener,
    state: AppState,
    quic_node: QuicNode,
    discovery: Arc<DiscoveryController>,
    shutdown: watch::Receiver<bool>,
    shutdown_sender: watch::Sender<bool>,
    ready_file: Option<PathBuf>,
) -> Result<()> {
    let mut http_task = tokio::spawn(async move {
        axum::serve(listener, router(state))
            .with_graceful_shutdown(wait_for_shutdown(shutdown))
            .await
            .context("serve local API")
    });
    let quic_shutdown = quic_node.shutdown_handle();
    let mut quic_task = tokio::spawn(quic_node.run());

    let result = tokio::select! {
        result = &mut http_task => {
            let result = result.context("join local API task")?;
            quic_shutdown.close();
            quic_task.await.context("join QUIC peer task")?;
            result
        },
        result = &mut quic_task => {
            result.context("join QUIC peer task")?;
            let _ = shutdown_sender.send(true);
            http_task.await.context("join local API task")?
        }
    };

    let discovery_result = discovery.set_enabled(false).context("stop LAN discovery");
    let readiness_result = match ready_file {
        Some(path) => {
            remove_node_ready_file(&path, std::process::id()).context("remove node readiness")
        }
        None => Ok(()),
    };

    result?;
    discovery_result?;
    readiness_result
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
        configuration.recovery = Some(RecoveryBootstrap { kit, unlock });
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
        runtime.stop().await.expect("stop recovered runtime");
    }
}
