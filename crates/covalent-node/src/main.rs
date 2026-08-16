use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use covalent_core::{Engine, EngineOptions};
use covalent_node::discovery::LanDiscovery;
use covalent_node::transport::{QuicNode, TlsIdentity};
use covalent_node::{AppState, load_or_create_local_api_token, router};
use covalent_protocol::PlatformTier;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "covalent-node", version, about = "Covalent backup node")]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Runs the local API, console, and engine service.
    Serve {
        /// Socket exposed to native clients or the container network.
        #[arg(long, env = "COVALENT_LISTEN", default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
        /// QUIC peer socket. UDP may share the same port number as the HTTP TCP socket.
        #[arg(long, env = "COVALENT_PEER_LISTEN", default_value = "127.0.0.1:8787")]
        peer_listen: SocketAddr,
        /// Durable node state directory.
        #[arg(long, env = "COVALENT_DATA_DIR", default_value = ".covalent-data")]
        data_dir: PathBuf,
        /// User-visible device name.
        #[arg(long, env = "COVALENT_DEVICE_NAME", default_value = "Covalent node")]
        device_name: String,
        /// Enables local multicast discovery; false remains a supported mode.
        #[arg(long, env = "COVALENT_LAN_DISCOVERY", default_value_t = false)]
        lan_discovery: bool,
        /// Readiness tier represented by this package.
        #[arg(long, env = "COVALENT_PLATFORM_TIER", value_enum, default_value_t = Tier::Tier1)]
        platform_tier: Tier,
    },
    /// Checks an already-running node without curl.
    Healthcheck {
        /// HTTP health URL.
        #[arg(
            long,
            env = "COVALENT_HEALTH_URL",
            default_value = "http://127.0.0.1:8787/healthz"
        )]
        url: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Tier {
    Tier1,
    Tier2,
}

impl From<Tier> for PlatformTier {
    fn from(value: Tier) -> Self {
        match value {
            Tier::Tier1 => Self::Tier1,
            Tier::Tier2 => Self::Tier2,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    match Arguments::parse().command.unwrap_or(Command::Serve {
        listen: "127.0.0.1:8787".parse().expect("static socket address"),
        peer_listen: "127.0.0.1:8787".parse().expect("static socket address"),
        data_dir: PathBuf::from(".covalent-data"),
        device_name: "Covalent node".to_owned(),
        lan_discovery: false,
        platform_tier: Tier::Tier1,
    }) {
        Command::Serve {
            listen,
            peer_listen,
            data_dir,
            device_name,
            lan_discovery,
            platform_tier,
        } => {
            serve(
                listen,
                peer_listen,
                data_dir,
                device_name,
                lan_discovery,
                platform_tier.into(),
            )
            .await
        }
        Command::Healthcheck { url } => healthcheck(&url),
    }
}

async fn serve(
    listen: SocketAddr,
    peer_listen: SocketAddr,
    data_dir: PathBuf,
    device_name: String,
    lan_discovery: bool,
    platform_tier: PlatformTier,
) -> Result<()> {
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data directory {}", data_dir.display()))?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind {listen}"))?;
    let mut engine_options = EngineOptions::new(&data_dir);
    engine_options.initial_device_name = device_name;
    engine_options.initial_lan_discovery_enabled = lan_discovery;
    let engine = Arc::new(Engine::open(engine_options).context("open Covalent engine")?);
    let token = load_or_create_local_api_token(data_dir.join("local-api-token"))
        .context("load local API token")?;
    let state = AppState::new(Arc::clone(&engine), platform_tier, token)
        .context("create local API state")?;
    let tls_identity =
        TlsIdentity::load_or_create(data_dir.join("tls")).context("load QUIC identity")?;
    let discovery_enabled = engine
        .config()
        .context("load persisted discovery preference")?
        .lan_discovery_enabled;
    let quic_node =
        QuicNode::bind(peer_listen, engine, &tls_identity).context("bind QUIC peer endpoint")?;
    let peer_address = quic_node
        .local_addr()
        .context("inspect QUIC peer endpoint")?;
    let state = state
        .with_peer_port(peer_address.port())
        .with_transport_certificate(tls_identity.certificate_der().to_vec())
        .with_provider_state(data_dir.join("provider-connections.json"))
        .context("load remembered provider connections")?;
    let quic_task = tokio::spawn(quic_node.run());
    let discovery = LanDiscovery::start(discovery_enabled, peer_address.port())
        .context("start LAN discovery")?;
    info!(%listen, %peer_address, data_dir = %data_dir.display(), "Covalent node ready");
    let result = axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve local API");
    quic_task.abort();
    discovery.stop();
    result
}

async fn shutdown_signal() {
    let control_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "could not install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => tracing::error!(%error, "could not install terminate handler"),
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = control_c => {},
        () = terminate => {},
    }
    info!("shutdown requested");
}

fn healthcheck(url: &str) -> Result<()> {
    let without_scheme = url
        .strip_prefix("http://")
        .context("healthcheck supports explicit http:// URLs only")?;
    let (authority, path) = without_scheme
        .split_once('/')
        .unwrap_or((without_scheme, ""));
    let mut addresses = authority
        .to_socket_addrs()
        .with_context(|| format!("resolve health authority {authority}"))?;
    let address = addresses
        .next()
        .context("health authority resolved no address")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .with_context(|| format!("connect to {authority}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(
        stream,
        "GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    if !response.starts_with("HTTP/1.1 200") {
        bail!("node healthcheck returned a non-200 response");
    }
    Ok(())
}
