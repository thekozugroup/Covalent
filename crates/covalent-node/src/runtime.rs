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
use covalent_core::{Engine, EngineOptions, ProviderQuotaPolicy};
use covalent_protocol::PlatformTier;
use tokio::sync::{Mutex, watch};
use tracing::info;
use zeroize::Zeroizing;

use crate::discovery::DiscoveryController;
use crate::pairing_transport::NetworkPairingService;
use crate::transport::{QuicNode, TlsIdentity};
use crate::{
    AppState, ArchiveLimits, NodeReadyInfo, load_or_create_local_api_token, remove_node_ready_file,
    router, validate_cleartext_api_bind, write_node_ready_file,
};

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
    /// Local API token source.  This is never logged.
    pub api_token: LocalApiTokenSource,
    /// Optional private record for an app supervising this runtime.
    pub ready_file: Option<PathBuf>,
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
            api_token: LocalApiTokenSource::Persisted,
            ready_file: None,
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
            api_token,
            ready_file,
        } = configuration;

        std::fs::create_dir_all(&data_directory)
            .with_context(|| format!("create data directory {}", data_directory.display()))?;
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
        let engine = Arc::new(Engine::open(engine_options).context("open Covalent engine")?);
        let api_token = match api_token {
            LocalApiTokenSource::Persisted => Zeroizing::new(
                load_or_create_local_api_token(data_directory.join("local-api-token"))
                    .context("load local API token")?,
            ),
            LocalApiTokenSource::Provided(token) => token,
        };
        if api_token.len() < 32 || api_token.len() > 512 {
            return Err(anyhow!("invalid local API token"));
        }

        let tls_identity = TlsIdentity::load_or_create(data_directory.join("tls"))
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
        // The pairing-only ALPN shares the advertised QUIC endpoint, so the
        // address a peer discovers is the exact address it must dial to pair.
        let quic_node = quic_node.with_pairing_service(Arc::new(NetworkPairingService::new(
            Arc::clone(&engine),
            state.network_pairing_manager(),
            state.local_transport_binding().ok(),
        )));

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
    let mut quic_task = tokio::spawn(quic_node.run());

    let result = tokio::select! {
        result = &mut http_task => result.context("join local API task")?,
        result = &mut quic_task => {
            result.context("join QUIC peer task")?;
            let _ = shutdown_sender.send(true);
            http_task.await.context("join local API task")?
        }
    };

    quic_task.abort();
    let _ = quic_task.await;
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

fn resolve_advertised_peer_address(
    bound_address: SocketAddr,
    configured_address: Option<SocketAddr>,
) -> Result<Option<SocketAddr>> {
    let Some(configured_address) = configured_address else {
        return Ok(None);
    };
    if configured_address.ip().is_unspecified() {
        return Err(anyhow!(
            "an advertised QUIC peer address must not be unspecified"
        ));
    }
    let port = if configured_address.port() == 0 {
        bound_address.port()
    } else {
        configured_address.port()
    };
    if port == 0 {
        return Err(anyhow!("advertised QUIC peer address must have a port"));
    }
    Ok(Some(SocketAddr::new(configured_address.ip(), port)))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::{NodeRuntime, NodeRuntimeConfig};

    fn loopback_zero() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)
    }

    async fn start_runtime(directory: &TempDir) -> NodeRuntime {
        NodeRuntime::start(NodeRuntimeConfig::new(
            directory.path(),
            loopback_zero(),
            loopback_zero(),
        ))
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
        let runtime = NodeRuntime::start(NodeRuntimeConfig::new(
            directory.path(),
            loopback_zero(),
            "0.0.0.0:0".parse().expect("wildcard peer address"),
        ))
        .await
        .expect("stock Docker/Unraid peer bind starts");
        assert!(runtime.ready_info().peer_address().ip().is_unspecified());
        assert_ne!(runtime.ready_info().peer_address().port(), 0);
        runtime.stop().await.expect("stop wildcard node");
    }

    #[test]
    fn wildcard_peer_bind_starts_without_a_static_advertised_endpoint() {
        assert_eq!(
            super::resolve_advertised_peer_address(
                "0.0.0.0:8787".parse().expect("bind address"),
                None,
            )
            .expect("wildcard bind is valid"),
            None,
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
    async fn drop_interrupts_runtime_and_removes_readiness_without_leaking_ports() {
        let directory = TempDir::new().expect("temp directory");
        let ready_file = directory.path().join("runtime-ready.json");
        let mut configuration =
            NodeRuntimeConfig::new(directory.path(), loopback_zero(), loopback_zero());
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
}
