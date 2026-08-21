use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use covalent_node::ArchiveLimits;
use covalent_node::runtime::{LocalApiTokenSource, NodeRuntime, NodeRuntimeConfig};
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
        /// Loopback-only cleartext management socket; network access must use a local TLS proxy.
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
        /// Optional private readiness JSON for an app that owns this process.
        #[arg(long, env = "COVALENT_READY_FILE")]
        ready_file: Option<PathBuf>,
        /// Streamed archive admission and capacity limits.
        #[command(flatten)]
        archive_limits: ArchiveLimitArguments,
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

#[derive(Clone, Copy, Debug, ClapArgs)]
struct ArchiveLimitArguments {
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_MAX_COMPRESSED_BYTES",
        default_value_t = 64_u64 << 30
    )]
    archive_max_compressed_bytes: u64,
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_MAX_UNCOMPRESSED_BYTES",
        default_value_t = 256_u64 << 30
    )]
    archive_max_uncompressed_bytes: u64,
    #[arg(long, env = "COVALENT_ARCHIVE_MAX_ENTRIES", default_value_t = 250_000)]
    archive_max_entries: usize,
    #[arg(long, env = "COVALENT_ARCHIVE_MAX_JOBS", default_value_t = 256)]
    archive_max_jobs: usize,
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_MAX_STAGING_BYTES",
        default_value_t = 512_u64 << 30
    )]
    archive_max_staging_bytes: u64,
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_MAX_RETAINED_RESULT_BYTES",
        default_value_t = 64_u64 << 30
    )]
    archive_max_retained_result_bytes: u64,
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_MAX_RETAINED_RESULTS",
        default_value_t = 64
    )]
    archive_max_retained_results: usize,
    #[arg(
        long,
        env = "COVALENT_ARCHIVE_FREE_SPACE_RESERVE_BYTES",
        default_value_t = 512_u64 << 20
    )]
    archive_free_space_reserve_bytes: u64,
}

impl From<ArchiveLimitArguments> for ArchiveLimits {
    fn from(value: ArchiveLimitArguments) -> Self {
        Self {
            maximum_compressed_bytes: value.archive_max_compressed_bytes,
            maximum_uncompressed_bytes: value.archive_max_uncompressed_bytes,
            maximum_entries: value.archive_max_entries,
            maximum_jobs: value.archive_max_jobs,
            maximum_staging_bytes: value.archive_max_staging_bytes,
            maximum_retained_result_bytes: value.archive_max_retained_result_bytes,
            maximum_retained_results: value.archive_max_retained_results,
            free_space_reserve_bytes: value.archive_free_space_reserve_bytes,
        }
    }
}

impl From<Tier> for PlatformTier {
    fn from(value: Tier) -> Self {
        match value {
            Tier::Tier1 => Self::Tier1,
            Tier::Tier2 => Self::Tier2,
        }
    }
}

struct ServeConfiguration {
    listen: SocketAddr,
    peer_listen: SocketAddr,
    data_dir: PathBuf,
    device_name: String,
    lan_discovery: bool,
    platform_tier: PlatformTier,
    ready_file: Option<PathBuf>,
    archive_limits: ArchiveLimits,
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
        ready_file: None,
        archive_limits: ArchiveLimitArguments {
            archive_max_compressed_bytes: 64_u64 << 30,
            archive_max_uncompressed_bytes: 256_u64 << 30,
            archive_max_entries: 250_000,
            archive_max_jobs: 256,
            archive_max_staging_bytes: 512_u64 << 30,
            archive_max_retained_result_bytes: 64_u64 << 30,
            archive_max_retained_results: 64,
            archive_free_space_reserve_bytes: 512_u64 << 20,
        },
    }) {
        Command::Serve {
            listen,
            peer_listen,
            data_dir,
            device_name,
            lan_discovery,
            platform_tier,
            ready_file,
            archive_limits,
        } => {
            serve(ServeConfiguration {
                listen,
                peer_listen,
                data_dir,
                device_name,
                lan_discovery,
                platform_tier: platform_tier.into(),
                ready_file,
                archive_limits: archive_limits.into(),
            })
            .await
        }
        Command::Healthcheck { url } => healthcheck(&url),
    }
}

async fn serve(configuration: ServeConfiguration) -> Result<()> {
    let ServeConfiguration {
        listen,
        peer_listen,
        data_dir,
        device_name,
        lan_discovery,
        platform_tier,
        ready_file,
        archive_limits,
    } = configuration;
    let mut runtime_configuration = NodeRuntimeConfig::new(data_dir, listen, peer_listen);
    runtime_configuration.device_name = device_name;
    runtime_configuration.lan_discovery_enabled = lan_discovery;
    runtime_configuration.platform_tier = platform_tier;
    runtime_configuration.archive_limits = archive_limits;
    runtime_configuration.api_token = LocalApiTokenSource::Persisted;
    runtime_configuration.ready_file = ready_file;
    let runtime = NodeRuntime::start(runtime_configuration).await?;
    shutdown_signal().await;
    runtime.stop().await
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
