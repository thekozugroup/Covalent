use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use covalent_core::{CoreError, KeyEncryptionKey, KeyProtector, StaticKeyProtector};
use covalent_node::ArchiveLimits;
use covalent_node::runtime::{
    LocalApiTokenSource, NodeRuntime, NodeRuntimeConfig, RecoveryBootstrap,
    load_recovery_bootstrap_files,
};
use covalent_protocol::PlatformTier;
use rand_core::{OsRng, RngCore};
use tracing::info;
use tracing_subscriber::EnvFilter;
use zeroize::{Zeroize, Zeroizing};

/// Inherited secret payload schemas (all integers are unsigned big-endian):
///
/// * `CVKEK001 || current:u32 || count:u16 || count*(version:u32 || kek:[u8;32])`
/// * `CVSEC002 || current:u32 || count:u16 || token_len:u16 || entries || token`
///
/// V1 remains accepted as KEK-only compatibility and uses the protected
/// persisted token. V2 requires a 32..=512 byte visible-ASCII token and keeps
/// it in memory through `LocalApiTokenSource::Provided`; it is never persisted.
const PIPE_KEY_V1_MAGIC: &[u8; 8] = b"CVKEK001";
const PIPE_SECRET_V2_MAGIC: &[u8; 8] = b"CVSEC002";
const PIPE_SECRET_V3_MAGIC: &[u8; 8] = b"CVSEC003";
const PIPE_KEY_LENGTH: usize = 32;
const PIPE_KEY_MAXIMUM_COUNT: usize = 16;
const PIPE_KEY_V1_HEADER_LENGTH: usize = PIPE_KEY_V1_MAGIC.len() + 4 + 2;
const PIPE_SECRET_V2_HEADER_LENGTH: usize = PIPE_SECRET_V2_MAGIC.len() + 4 + 2 + 2;
const PIPE_SECRET_V3_HEADER_LENGTH: usize = PIPE_SECRET_V3_MAGIC.len() + 4 + 2 + 2 + 4 + 2;
const PIPE_KEY_ENTRY_LENGTH: usize = 4 + PIPE_KEY_LENGTH;
const PIPE_API_TOKEN_MINIMUM_LENGTH: usize = 32;
const PIPE_API_TOKEN_MAXIMUM_LENGTH: usize = 512;
const PIPE_RECOVERY_KIT_MAXIMUM_LENGTH: usize = 16 * 1_024 * 1_024;
const PIPE_RECOVERY_KEY_LENGTH: usize = 43;
const PIPE_SECRET_MAXIMUM_LENGTH: usize = PIPE_SECRET_V3_HEADER_LENGTH
    + (PIPE_KEY_MAXIMUM_COUNT * PIPE_KEY_ENTRY_LENGTH)
    + PIPE_API_TOKEN_MAXIMUM_LENGTH
    + PIPE_RECOVERY_KIT_MAXIMUM_LENGTH
    + PIPE_RECOVERY_KEY_LENGTH;

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
        /// Address other devices dial to reach this node, when it cannot be detected.
        ///
        /// Left unset, the node picks a private LAN address from its own
        /// interfaces. That fails in exactly one common case: a container on a
        /// bridge network, where the only visible address belongs to the bridge
        /// and the peer port is published on the host instead. Unraid's default
        /// is bridge networking, so this is the ordinary path there rather than
        /// an exotic one. A zero port inherits the bound peer port.
        #[arg(long, env = "COVALENT_ADVERTISED_PEER_ADDRESS")]
        advertised_peer_address: Option<SocketAddr>,
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
        /// CA certificate a claiming client should pin, when a proxy terminates TLS.
        #[arg(long, env = "COVALENT_TLS_CA_FILE")]
        tls_ca_file: Option<PathBuf>,
        /// Key protection source and version.
        #[command(flatten)]
        key_protection: Box<KeyProtectionArguments>,
        /// Streamed archive admission and capacity limits.
        ///
        /// Boxed so this variant does not dwarf `Healthcheck`: eight tuning
        /// numbers is most of the subcommand's footprint and none of it is on a
        /// hot path.
        #[command(flatten)]
        archive_limits: Box<ArchiveLimitArguments>,
    },
    /// Restores a lost owner identity into a fresh state root, then keeps serving for claim.
    Recover {
        /// Owner-readable raw decoded `.covalent-recovery` file exported by the old owner.
        #[arg(long, value_name = "PATH")]
        recovery_kit_file: Option<PathBuf>,
        /// Owner-readable `.covalent-recovery-key` file containing the 43-character key.
        #[arg(long, value_name = "PATH")]
        recovery_key_file: Option<PathBuf>,
        /// Loopback-only cleartext management socket.
        #[arg(long, env = "COVALENT_LISTEN", default_value = "127.0.0.1:8787")]
        listen: SocketAddr,
        /// QUIC peer socket.
        #[arg(long, env = "COVALENT_PEER_LISTEN", default_value = "127.0.0.1:8787")]
        peer_listen: SocketAddr,
        /// Address other devices dial to reach this node.
        #[arg(long, env = "COVALENT_ADVERTISED_PEER_ADDRESS")]
        advertised_peer_address: Option<SocketAddr>,
        /// Fresh durable node state directory. Existing identity state is refused.
        #[arg(long, env = "COVALENT_DATA_DIR", default_value = ".covalent-data")]
        data_dir: PathBuf,
        /// User-visible fallback name; the signed kit's identity configuration wins.
        #[arg(long, env = "COVALENT_DEVICE_NAME", default_value = "Covalent node")]
        device_name: String,
        /// Initial fallback discovery preference.
        #[arg(long, env = "COVALENT_LAN_DISCOVERY", default_value_t = false)]
        lan_discovery: bool,
        /// Readiness tier represented by this package.
        #[arg(long, env = "COVALENT_PLATFORM_TIER", value_enum, default_value_t = Tier::Tier1)]
        platform_tier: Tier,
        /// CA certificate delivered by the first-run ownership claim.
        #[arg(long, env = "COVALENT_TLS_CA_FILE")]
        tls_ca_file: Option<PathBuf>,
        /// Optional private readiness JSON for an app that owns this process.
        #[arg(long, env = "COVALENT_READY_FILE")]
        ready_file: Option<PathBuf>,
        /// Key protection source and version for the new local state.
        #[command(flatten)]
        key_protection: Box<KeyProtectionArguments>,
        /// Streamed archive admission and capacity limits.
        #[command(flatten)]
        archive_limits: Box<ArchiveLimitArguments>,
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
    /// Creates one owner-readable KEK file without replacing an existing key.
    ProvisionKey {
        /// Destination for the base64url 32-byte KEK. The file is created as 0600.
        #[arg(long, value_name = "PATH")]
        key_file: PathBuf,
        /// Version to configure with COVALENT_KEY_ENCRYPTION_KEY_VERSION.
        #[arg(long, default_value_t = 1)]
        key_version: u32,
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

#[derive(Clone, Debug, ClapArgs)]
struct KeyProtectionArguments {
    /// Owner-readable base64url 32-byte KEK file. Required for a headless node.
    #[arg(long, env = "COVALENT_KEY_ENCRYPTION_KEY_FILE", value_name = "PATH")]
    key_encryption_key_file: Option<PathBuf>,
    /// Version recorded with newly wrapped state. Keep this value and its key together.
    #[arg(long, env = "COVALENT_KEY_ENCRYPTION_KEY_VERSION", default_value_t = 1)]
    key_encryption_key_version: u32,
    /// Reads a bounded binary KEK hierarchy from standard input, then closes it.
    /// Intended for a supervising native app using an inherited anonymous pipe.
    #[arg(long, default_value_t = false)]
    key_encryption_key_stdin: bool,
    /// Owner-only caller-supplied API token file for harnesses and host supervisors.
    /// The path may appear in argv; the token bytes never do and are not persisted.
    #[arg(long, value_name = "PATH")]
    api_token_file: Option<PathBuf>,
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
    advertised_peer_address: Option<SocketAddr>,
    data_dir: PathBuf,
    device_name: String,
    lan_discovery: bool,
    platform_tier: PlatformTier,
    ready_file: Option<PathBuf>,
    tls_ca_file: Option<PathBuf>,
    key_encryption_key_file: Option<PathBuf>,
    key_encryption_key_version: u32,
    key_encryption_key_stdin: bool,
    api_token_file: Option<PathBuf>,
    archive_limits: ArchiveLimits,
    recovery_required: bool,
    recovery_files: Option<(PathBuf, PathBuf)>,
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
        advertised_peer_address: None,
        data_dir: PathBuf::from(".covalent-data"),
        device_name: "Covalent node".to_owned(),
        lan_discovery: false,
        platform_tier: Tier::Tier1,
        ready_file: None,
        tls_ca_file: None,
        key_protection: Box::new(KeyProtectionArguments {
            key_encryption_key_file: None,
            key_encryption_key_version: 1,
            key_encryption_key_stdin: false,
            api_token_file: None,
        }),
        archive_limits: Box::new(ArchiveLimitArguments {
            archive_max_compressed_bytes: 64_u64 << 30,
            archive_max_uncompressed_bytes: 256_u64 << 30,
            archive_max_entries: 250_000,
            archive_max_jobs: 256,
            archive_max_staging_bytes: 512_u64 << 30,
            archive_max_retained_result_bytes: 64_u64 << 30,
            archive_max_retained_results: 64,
            archive_free_space_reserve_bytes: 512_u64 << 20,
        }),
    }) {
        Command::Serve {
            listen,
            peer_listen,
            advertised_peer_address,
            data_dir,
            device_name,
            lan_discovery,
            platform_tier,
            ready_file,
            tls_ca_file,
            key_protection,
            archive_limits,
        } => {
            let KeyProtectionArguments {
                key_encryption_key_file,
                key_encryption_key_version,
                key_encryption_key_stdin,
                api_token_file,
            } = *key_protection;
            serve(ServeConfiguration {
                listen,
                peer_listen,
                advertised_peer_address,
                data_dir,
                device_name,
                lan_discovery,
                platform_tier: platform_tier.into(),
                ready_file,
                tls_ca_file,
                key_encryption_key_file,
                key_encryption_key_version,
                key_encryption_key_stdin,
                api_token_file,
                archive_limits: (*archive_limits).into(),
                recovery_required: false,
                recovery_files: None,
            })
            .await
        }
        Command::Recover {
            recovery_kit_file,
            recovery_key_file,
            listen,
            peer_listen,
            advertised_peer_address,
            data_dir,
            device_name,
            lan_discovery,
            platform_tier,
            tls_ca_file,
            ready_file,
            key_protection,
            archive_limits,
        } => {
            let KeyProtectionArguments {
                key_encryption_key_file,
                key_encryption_key_version,
                key_encryption_key_stdin,
                api_token_file,
            } = *key_protection;
            let recovery_files = match (recovery_kit_file, recovery_key_file) {
                (Some(kit), Some(key)) => Some((kit, key)),
                (None, None) => None,
                _ => bail!(
                    "provide both --recovery-kit-file and --recovery-key-file, or neither when CVSEC003 is supplied through --key-encryption-key-stdin"
                ),
            };
            serve(ServeConfiguration {
                listen,
                peer_listen,
                advertised_peer_address,
                data_dir,
                device_name,
                lan_discovery,
                platform_tier: platform_tier.into(),
                ready_file,
                tls_ca_file,
                key_encryption_key_file,
                key_encryption_key_version,
                key_encryption_key_stdin,
                api_token_file,
                archive_limits: (*archive_limits).into(),
                recovery_required: true,
                recovery_files,
            })
            .await
        }
        Command::Healthcheck { url } => healthcheck(&url),
        Command::ProvisionKey {
            key_file,
            key_version,
        } => provision_key(&key_file, key_version),
    }
}

async fn serve(configuration: ServeConfiguration) -> Result<()> {
    let ServeConfiguration {
        listen,
        peer_listen,
        advertised_peer_address,
        data_dir,
        device_name,
        lan_discovery,
        platform_tier,
        ready_file,
        tls_ca_file,
        key_encryption_key_file,
        key_encryption_key_version,
        key_encryption_key_stdin,
        api_token_file,
        archive_limits,
        recovery_required,
        recovery_files,
    } = configuration;
    let mut runtime_configuration = NodeRuntimeConfig::new(data_dir, listen, peer_listen);
    runtime_configuration.advertised_peer_address = advertised_peer_address;
    runtime_configuration.device_name = device_name;
    runtime_configuration.lan_discovery_enabled = lan_discovery;
    runtime_configuration.platform_tier = platform_tier;
    runtime_configuration.archive_limits = archive_limits;
    let mut inherited = headless_secrets(
        key_encryption_key_file,
        key_encryption_key_version,
        key_encryption_key_stdin,
        api_token_file,
    )?;
    runtime_configuration.recovery =
        select_recovery_bootstrap(recovery_required, recovery_files, inherited.recovery.take())?;
    runtime_configuration.key_protector = Some(inherited.key_protector);
    let api_token_was_provided = inherited.api_token.is_some();
    runtime_configuration.api_token = inherited.api_token.map_or(
        LocalApiTokenSource::Persisted,
        LocalApiTokenSource::Provided,
    );
    // A supervising app provisions its own token through platform secure
    // storage, so the unauthenticated claim route exists only for the standalone
    // daemon -- which is the headless container this whole flow is for.
    runtime_configuration.first_run_claim_enabled = ready_file.is_none() && !api_token_was_provided;
    runtime_configuration.tls_ca_certificate_file = tls_ca_file;
    runtime_configuration.ready_file = ready_file;
    let runtime = NodeRuntime::start(runtime_configuration).await?;
    shutdown_signal().await;
    runtime.stop().await
}

struct HeadlessSecrets {
    key_protector: Arc<dyn KeyProtector>,
    api_token: Option<Zeroizing<String>>,
    recovery: Option<RecoveryBootstrap>,
}

fn headless_secrets(
    key_file: Option<PathBuf>,
    key_version: u32,
    read_stdin: bool,
    api_token_file: Option<PathBuf>,
) -> Result<HeadlessSecrets> {
    if read_stdin {
        if key_file.is_some() {
            bail!("choose either a KEK file or the inherited KEK pipe, not both");
        }
        let mut serialized = Zeroizing::new(Vec::with_capacity(PIPE_SECRET_MAXIMUM_LENGTH));
        std::io::stdin()
            .lock()
            .take((PIPE_SECRET_MAXIMUM_LENGTH + 1) as u64)
            .read_to_end(&mut serialized)
            .context("read secret payload from inherited pipe")?;
        let PipeSecrets {
            protector,
            mut api_token,
            recovery,
        } = ProvisionedKeyProtector::from_pipe_bytes(serialized.as_ref())?;
        if api_token.is_some() && api_token_file.is_some() {
            bail!("choose either the inherited v2 API token or --api-token-file, not both");
        }
        if let Some(path) = api_token_file.as_ref() {
            api_token = Some(read_api_token_file(path)?);
        }
        return Ok(HeadlessSecrets {
            key_protector: Arc::new(protector),
            api_token,
            recovery,
        });
    }
    let path = key_file.context(
        "key protection is locked: set COVALENT_KEY_ENCRYPTION_KEY_FILE to a provisioned owner-readable key file; run covalent-node provision-key --key-file <path> first",
    )?;
    if key_version == 0 {
        bail!("COVALENT_KEY_ENCRYPTION_KEY_VERSION must be greater than zero");
    }
    let encoded = read_kek_file(&path)?;
    let api_token = api_token_file
        .as_ref()
        .map(read_api_token_file)
        .transpose()?;
    Ok(HeadlessSecrets {
        key_protector: Arc::new(StaticKeyProtector::from_base64(key_version, &encoded)?),
        api_token,
        recovery: None,
    })
}

fn select_recovery_bootstrap(
    required: bool,
    files: Option<(PathBuf, PathBuf)>,
    inherited: Option<RecoveryBootstrap>,
) -> Result<Option<RecoveryBootstrap>> {
    match (required, files, inherited) {
        (false, None, None) => Ok(None),
        (false, _, Some(_)) => bail!("CVSEC003 is accepted only by the recover command"),
        (true, Some(_), Some(_)) => bail!("choose either recovery files or CVSEC003, not both"),
        (true, Some((kit, key)), None) => load_recovery_bootstrap_files(&kit, &key)
            .map(Some)
            .map_err(Into::into),
        (true, None, Some(recovery)) => Ok(Some(recovery)),
        (true, None, None) => bail!(
            "recover requires both recovery files or CVSEC003 through --key-encryption-key-stdin"
        ),
        (false, Some(_), None) => bail!("recovery files are accepted only by the recover command"),
    }
}

struct PipeSecrets {
    protector: ProvisionedKeyProtector,
    api_token: Option<Zeroizing<String>>,
    recovery: Option<RecoveryBootstrap>,
}

struct ProvisionedKeyProtector {
    current_version: u32,
    keys: BTreeMap<u32, Zeroizing<[u8; PIPE_KEY_LENGTH]>>,
}

impl ProvisionedKeyProtector {
    fn from_pipe_bytes(serialized: &[u8]) -> Result<PipeSecrets, CoreError> {
        if serialized.len() < PIPE_KEY_V1_HEADER_LENGTH
            || serialized.len() > PIPE_SECRET_MAXIMUM_LENGTH
        {
            return Err(CoreError::InvalidKeyMaterial);
        }
        let (header_length, token_length, kit_length, recovery_key_length, is_recovery) =
            if serialized.starts_with(PIPE_KEY_V1_MAGIC) {
                (PIPE_KEY_V1_HEADER_LENGTH, 0, 0, 0, false)
            } else if serialized.starts_with(PIPE_SECRET_V2_MAGIC) {
                let token_length =
                    usize::from(read_u16(serialized, PIPE_SECRET_V2_MAGIC.len() + 6)?);
                if !(PIPE_API_TOKEN_MINIMUM_LENGTH..=PIPE_API_TOKEN_MAXIMUM_LENGTH)
                    .contains(&token_length)
                {
                    return Err(CoreError::InvalidKeyMaterial);
                }
                (PIPE_SECRET_V2_HEADER_LENGTH, token_length, 0, 0, false)
            } else if serialized.starts_with(PIPE_SECRET_V3_MAGIC) {
                let token_length = usize::from(read_u16(serialized, 14)?);
                let kit_length = usize::try_from(read_u32(serialized, 16)?)
                    .map_err(|_| CoreError::InvalidKeyMaterial)?;
                let recovery_key_length = usize::from(read_u16(serialized, 20)?);
                if !(PIPE_API_TOKEN_MINIMUM_LENGTH..=PIPE_API_TOKEN_MAXIMUM_LENGTH)
                    .contains(&token_length)
                    || !(1..=PIPE_RECOVERY_KIT_MAXIMUM_LENGTH).contains(&kit_length)
                    || recovery_key_length != PIPE_RECOVERY_KEY_LENGTH
                {
                    return Err(CoreError::InvalidKeyMaterial);
                }
                (
                    PIPE_SECRET_V3_HEADER_LENGTH,
                    token_length,
                    kit_length,
                    recovery_key_length,
                    true,
                )
            } else {
                return Err(CoreError::InvalidKeyMaterial);
            };
        let current_version = read_u32(serialized, 8)?;
        let count = usize::from(read_u16(serialized, 12)?);
        let expected_length = header_length
            .checked_add(
                count
                    .checked_mul(PIPE_KEY_ENTRY_LENGTH)
                    .ok_or(CoreError::InvalidKeyMaterial)?,
            )
            .and_then(|length| length.checked_add(token_length))
            .and_then(|length| length.checked_add(kit_length))
            .and_then(|length| length.checked_add(recovery_key_length))
            .ok_or(CoreError::InvalidKeyMaterial)?;
        if current_version == 0
            || count == 0
            || count > PIPE_KEY_MAXIMUM_COUNT
            || serialized.len() != expected_length
        {
            return Err(CoreError::InvalidKeyMaterial);
        }

        let mut keys = BTreeMap::new();
        let mut previous_version = 0;
        for index in 0..count {
            let offset = header_length + (index * PIPE_KEY_ENTRY_LENGTH);
            let version = read_u32(serialized, offset)?;
            if version == 0 || version <= previous_version {
                return Err(CoreError::InvalidKeyMaterial);
            }
            previous_version = version;
            let mut key = [0_u8; PIPE_KEY_LENGTH];
            key.copy_from_slice(&serialized[(offset + 4)..(offset + PIPE_KEY_ENTRY_LENGTH)]);
            keys.insert(version, Zeroizing::new(key));
            key.zeroize();
        }
        if !keys.contains_key(&current_version) {
            return Err(CoreError::InvalidKeyMaterial);
        }
        let entries_end = header_length + (count * PIPE_KEY_ENTRY_LENGTH);
        let token_end = entries_end + token_length;
        let kit_end = token_end + kit_length;
        let api_token = if token_length == 0 {
            None
        } else {
            let token = std::str::from_utf8(&serialized[entries_end..token_end])
                .map_err(|_| CoreError::InvalidKeyMaterial)?;
            if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(CoreError::InvalidKeyMaterial);
            }
            Some(Zeroizing::new(token.to_owned()))
        };
        let recovery = if is_recovery {
            let recovery_key = std::str::from_utf8(&serialized[kit_end..expected_length])
                .map_err(|_| CoreError::InvalidKeyMaterial)?;
            Some(RecoveryBootstrap {
                kit: Zeroizing::new(serialized[token_end..kit_end].to_vec()),
                unlock: covalent_core::RecoveryUnlockKey::from_base64(recovery_key)?,
            })
        } else {
            None
        };
        Ok(PipeSecrets {
            protector: Self {
                current_version,
                keys,
            },
            api_token,
            recovery,
        })
    }
}

impl KeyProtector for ProvisionedKeyProtector {
    fn current_key_version(&self) -> Result<u32, CoreError> {
        Ok(self.current_version)
    }

    fn key_encryption_key(&self, key_version: u32) -> Result<KeyEncryptionKey, CoreError> {
        let stored = self
            .keys
            .get(&key_version)
            .ok_or(CoreError::KeyVersionUnavailable(key_version))?;
        let mut key = [0_u8; PIPE_KEY_LENGTH];
        key.copy_from_slice(stored.as_ref());
        let result = KeyEncryptionKey::from_bytes(key);
        key.zeroize();
        Ok(result)
    }
}

impl fmt::Debug for ProvisionedKeyProtector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProvisionedKeyProtector")
            .field("current_version", &self.current_version)
            .field("available_versions", &self.keys.keys())
            .field("keys", &"[REDACTED]")
            .finish()
    }
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, CoreError> {
    let encoded: [u8; 4] = bytes
        .get(offset..offset + 4)
        .ok_or(CoreError::InvalidKeyMaterial)?
        .try_into()
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    Ok(u32::from_be_bytes(encoded))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, CoreError> {
    let encoded: [u8; 2] = bytes
        .get(offset..offset + 2)
        .ok_or(CoreError::InvalidKeyMaterial)?
        .try_into()
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    Ok(u16::from_be_bytes(encoded))
}

fn read_kek_file(path: &PathBuf) -> Result<Zeroizing<String>> {
    let bytes = read_private_regular_file(path, 512, "KEK")?;
    Ok(Zeroizing::new(
        std::str::from_utf8(bytes.as_ref())
            .context("KEK file is not UTF-8 base64url")?
            .trim()
            .to_owned(),
    ))
}

fn read_api_token_file(path: &PathBuf) -> Result<Zeroizing<String>> {
    let bytes =
        read_private_regular_file(path, PIPE_API_TOKEN_MAXIMUM_LENGTH as u64 + 1, "API token")?;
    let text = std::str::from_utf8(bytes.as_ref()).context("API token file is not UTF-8")?;
    let token = text.trim();
    if !(PIPE_API_TOKEN_MINIMUM_LENGTH..=PIPE_API_TOKEN_MAXIMUM_LENGTH).contains(&token.len())
        || !token.bytes().all(|byte| byte.is_ascii_graphic())
    {
        bail!("API token file must contain 32 to 512 visible ASCII bytes");
    }
    Ok(Zeroizing::new(token.to_owned()))
}

#[cfg(test)]
thread_local! {
    static PROVISION_KEY_FAILPOINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn provision_key_failpoint() -> Result<()> {
    PROVISION_KEY_FAILPOINT.with(|failpoint| {
        if failpoint.replace(false) {
            bail!("provision-key failpoint after durable staging")
        }
        Ok(())
    })
}

#[cfg(not(test))]
const fn provision_key_failpoint() -> Result<()> {
    Ok(())
}

fn provision_key(path: &PathBuf, key_version: u32) -> Result<()> {
    if key_version == 0 {
        bail!("key version must be greater than zero");
    }
    let parent = path
        .parent()
        .context("KEK file path must have a parent directory")?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .with_context(|| format!("inspect KEK parent directory {}", parent.display()))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!("KEK parent must be an existing directory, not a symlink");
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => bail!(
            "KEK file {} already exists; it was not replaced",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect KEK target {}", path.display()));
        }
    }
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    let encoded = Zeroizing::new(URL_SAFE_NO_PAD.encode(key));
    key.zeroize();
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create private KEK staging file")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .context("protect KEK staging file")?;
    }
    temporary
        .write_all(encoded.as_bytes())
        .and_then(|()| temporary.write_all(b"\n"))
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .with_context(|| format!("durably stage KEK file {}", path.display()))?;
    provision_key_failpoint()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "atomically publish KEK file {} without replacing it",
                path.display()
            )
        })?;
    sync_parent_directory(parent).context("sync KEK parent directory")?;
    println!(
        "Created owner-readable KEK file {}. Start with COVALENT_KEY_ENCRYPTION_KEY_FILE={} and COVALENT_KEY_ENCRYPTION_KEY_VERSION={key_version}. Keep this key and version unchanged for this state directory; this release has no automatic KEK rotation.",
        path.display(),
        path.display(),
    );
    Ok(())
}

fn read_private_regular_file(
    path: &PathBuf,
    maximum: u64,
    label: &str,
) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    let (file, length) = {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};

        let descriptor = open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .with_context(|| {
            format!(
                "open {label} file {} without following links",
                path.display()
            )
        })?;
        let stat = fstat(&descriptor)
            .with_context(|| format!("inspect open {label} file {}", path.display()))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_mode & 0o077 != 0
            || stat.st_size < 0
            || stat.st_size as u64 > maximum
        {
            bail!("{label} file must be an owner-only regular file no larger than {maximum} bytes");
        }
        (File::from(descriptor), stat.st_size as u64)
    };
    #[cfg(not(unix))]
    let (file, length) = {
        let metadata = std::fs::symlink_metadata(path)
            .with_context(|| format!("inspect {label} file {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
            bail!("{label} file must be a regular file no larger than {maximum} bytes");
        }
        let file =
            File::open(path).with_context(|| format!("open {label} file {}", path.display()))?;
        (file, metadata.len())
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(length as usize));
    file.take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label} file {}", path.display()))?;
    if bytes.len() as u64 > maximum {
        bail!("{label} file exceeds the {maximum} byte limit");
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_parent_directory(path: &std::path::Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_: &std::path::Path) -> Result<()> {
    Ok(())
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use covalent_core::{
        CoreError, Engine, EngineOptions, KeyProtector as _, RecoveryUnlockKey, StaticKeyProtector,
    };

    use super::{
        Arguments, Command, PIPE_KEY_V1_MAGIC, PIPE_RECOVERY_KIT_MAXIMUM_LENGTH,
        PIPE_SECRET_V2_MAGIC, PIPE_SECRET_V3_MAGIC, PROVISION_KEY_FAILPOINT, PipeSecrets,
        ProvisionedKeyProtector, provision_key, read_kek_file, select_recovery_bootstrap,
    };
    use clap::Parser as _;

    fn serialized_hierarchy(current: u32, entries: &[(u32, u8)]) -> Vec<u8> {
        let mut bytes = PIPE_KEY_V1_MAGIC.to_vec();
        bytes.extend_from_slice(&current.to_be_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        for (version, fill) in entries {
            bytes.extend_from_slice(&version.to_be_bytes());
            bytes.extend_from_slice(&[*fill; 32]);
        }
        bytes
    }

    fn serialized_secrets(current: u32, entries: &[(u32, u8)], token: &str) -> Vec<u8> {
        let mut bytes = PIPE_SECRET_V2_MAGIC.to_vec();
        bytes.extend_from_slice(&current.to_be_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(token.len() as u16).to_be_bytes());
        for (version, fill) in entries {
            bytes.extend_from_slice(&version.to_be_bytes());
            bytes.extend_from_slice(&[*fill; 32]);
        }
        bytes.extend_from_slice(token.as_bytes());
        bytes
    }

    fn serialized_recovery_secrets(
        current: u32,
        entries: &[(u32, u8)],
        token: &str,
        kit: &[u8],
        recovery_key: &str,
    ) -> Vec<u8> {
        let mut bytes = PIPE_SECRET_V3_MAGIC.to_vec();
        bytes.extend_from_slice(&current.to_be_bytes());
        bytes.extend_from_slice(&(entries.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(token.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&(kit.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&(recovery_key.len() as u16).to_be_bytes());
        for (version, fill) in entries {
            bytes.extend_from_slice(&version.to_be_bytes());
            bytes.extend_from_slice(&[*fill; 32]);
        }
        bytes.extend_from_slice(token.as_bytes());
        bytes.extend_from_slice(kit);
        bytes.extend_from_slice(recovery_key.as_bytes());
        bytes
    }

    #[test]
    fn inherited_pipe_hierarchy_supplies_current_and_historical_versions() {
        let PipeSecrets {
            protector,
            api_token: token,
            recovery,
        } = ProvisionedKeyProtector::from_pipe_bytes(&serialized_hierarchy(
            3,
            &[(1, 0x11), (3, 0x33)],
        ))
        .expect("valid hierarchy");

        assert!(token.is_none(), "v1 is explicitly KEK-only");
        assert!(recovery.is_none());
        assert_eq!(protector.current_key_version().unwrap(), 3);
        protector
            .key_encryption_key(1)
            .expect("historical key remains available");
        protector
            .key_encryption_key(3)
            .expect("current key remains available");
        assert!(matches!(
            protector.key_encryption_key(2),
            Err(CoreError::KeyVersionUnavailable(2))
        ));
        let debug = format!("{protector:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("11111111"));
        assert!(!debug.contains("33333333"));
    }

    #[test]
    fn inherited_pipe_hierarchy_rejects_corrupt_or_downgraded_records() {
        let cases = [
            Vec::new(),
            serialized_hierarchy(0, &[(1, 0x11)]),
            serialized_hierarchy(2, &[(1, 0x11)]),
            serialized_hierarchy(1, &[(1, 0x11), (1, 0x22)]),
        ];
        for bytes in cases {
            assert!(matches!(
                ProvisionedKeyProtector::from_pipe_bytes(&bytes),
                Err(CoreError::InvalidKeyMaterial)
            ));
        }
    }

    #[test]
    fn inherited_v2_payload_supplies_an_in_memory_api_token() {
        let expected = "provided-local-api-token-with-at-least-thirty-two-bytes";
        let PipeSecrets {
            protector,
            api_token: token,
            recovery,
        } = ProvisionedKeyProtector::from_pipe_bytes(&serialized_secrets(
            4,
            &[(2, 0x22), (4, 0x44)],
            expected,
        ))
        .expect("valid v2 payload");

        assert_eq!(protector.current_key_version().expect("version"), 4);
        assert_eq!(token.as_ref().map(|token| token.as_str()), Some(expected));
        assert!(recovery.is_none());
    }

    #[test]
    fn inherited_v2_payload_rejects_missing_short_or_appended_tokens() {
        let valid = serialized_secrets(
            1,
            &[(1, 0x11)],
            "provided-local-api-token-with-at-least-thirty-two-bytes",
        );
        let mut missing = valid.clone();
        missing.truncate(missing.len() - 1);
        let short = serialized_secrets(1, &[(1, 0x11)], "short");
        let mut appended = valid;
        appended.push(b'x');

        for bytes in [missing, short, appended] {
            assert!(matches!(
                ProvisionedKeyProtector::from_pipe_bytes(&bytes),
                Err(CoreError::InvalidKeyMaterial)
            ));
        }
    }

    #[test]
    fn inherited_v3_payload_is_exact_bounded_and_recovery_only() {
        let token = "provided-local-api-token-with-at-least-thirty-two-bytes";
        let recovery_key = RecoveryUnlockKey::from_bytes([0x71; 32]).expose_base64();
        let valid = serialized_recovery_secrets(
            4,
            &[(2, 0x22), (4, 0x44)],
            token,
            b"raw signed encrypted kit",
            &recovery_key,
        );
        let PipeSecrets {
            protector,
            api_token,
            recovery,
        } = ProvisionedKeyProtector::from_pipe_bytes(&valid).expect("valid v3 payload");
        assert_eq!(protector.current_key_version().expect("version"), 4);
        assert_eq!(api_token.as_ref().map(|value| value.as_str()), Some(token));
        let recovery = recovery.expect("recovery input");
        assert_eq!(recovery.kit.as_slice(), b"raw signed encrypted kit");
        assert!(select_recovery_bootstrap(false, None, Some(recovery)).is_err());
        let mixed = ProvisionedKeyProtector::from_pipe_bytes(&valid)
            .expect("valid v3 again")
            .recovery;
        assert!(
            select_recovery_bootstrap(true, Some(("kit".into(), "key".into())), mixed,).is_err()
        );

        let mut trailing = valid.clone();
        trailing.push(0);
        let missing_kit = serialized_recovery_secrets(1, &[(1, 0x11)], token, b"", &recovery_key);
        let missing_token = serialized_recovery_secrets(1, &[(1, 0x11)], "", b"kit", &recovery_key);
        let invalid_key = serialized_recovery_secrets(
            1,
            &[(1, 0x11)],
            token,
            b"kit",
            "!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!",
        );
        let duplicate_versions =
            serialized_recovery_secrets(1, &[(1, 0x11), (1, 0x22)], token, b"kit", &recovery_key);
        let mut oversized = valid;
        oversized[16..20].copy_from_slice(
            &(u32::try_from(PIPE_RECOVERY_KIT_MAXIMUM_LENGTH).expect("u32") + 1).to_be_bytes(),
        );
        for bytes in [
            trailing,
            missing_kit,
            missing_token,
            invalid_key,
            duplicate_versions,
            oversized,
        ] {
            assert!(matches!(
                ProvisionedKeyProtector::from_pipe_bytes(&bytes),
                Err(CoreError::InvalidKeyMaterial)
            ));
        }
    }

    #[tokio::test]
    async fn inherited_v3_pipe_recovers_a_fresh_serving_runtime_without_secret_files() {
        let root = tempfile::TempDir::new().expect("root");
        let owner_path = root.path().join("lost-owner");
        let recovered_path = root.path().join("recovered-owner");
        let owner = Engine::open(EngineOptions::new(&owner_path).with_key_protector(Arc::new(
            StaticKeyProtector::new(1, [0x31; 32]).expect("owner protector"),
        )))
        .expect("owner");
        let owner_id = owner.device_id();
        let unlock = RecoveryUnlockKey::generate();
        let kit = owner.export_recovery_kit(&unlock).expect("recovery kit");
        let recovery_key = unlock.expose_base64();
        drop(owner);
        std::fs::remove_dir_all(&owner_path).expect("lose owner state");

        let token = "provided-local-api-token-with-at-least-thirty-two-bytes";
        let payload = serialized_recovery_secrets(7, &[(7, 0x77)], token, &kit, &recovery_key);
        let PipeSecrets {
            protector,
            api_token,
            recovery,
        } = ProvisionedKeyProtector::from_pipe_bytes(&payload).expect("parse v3");
        let recovery = select_recovery_bootstrap(true, None, recovery)
            .expect("recovery selection")
            .expect("recovery input");
        let ready_file = root.path().join("recovered-ready.json");
        let mut configuration = covalent_node::runtime::NodeRuntimeConfig::new(
            &recovered_path,
            "127.0.0.1:0".parse().expect("API address"),
            "127.0.0.1:0".parse().expect("peer address"),
        );
        configuration.key_protector = Some(Arc::new(protector));
        configuration.api_token =
            covalent_node::runtime::LocalApiTokenSource::Provided(api_token.expect("API token"));
        configuration.recovery = Some(recovery);
        configuration.ready_file = Some(ready_file.clone());
        let runtime = covalent_node::runtime::NodeRuntime::start(configuration)
            .await
            .expect("recovered runtime serves");
        assert!(
            ready_file.exists(),
            "app-owned recovery publishes readiness"
        );
        runtime.stop().await.expect("stop runtime");
        let identity: serde_json::Value = serde_json::from_slice(
            &std::fs::read(recovered_path.join("identity.json")).expect("identity"),
        )
        .expect("identity JSON");
        assert_eq!(identity["deviceId"], owner_id.to_string());
        assert!(!recovered_path.join("local-api-token").exists());
    }

    #[test]
    fn mac_supervisor_flag_carries_no_secret_or_path() {
        let arguments =
            Arguments::try_parse_from(["covalent-node", "serve", "--key-encryption-key-stdin"])
                .expect("parse inherited pipe flag");
        let Some(Command::Serve { key_protection, .. }) = arguments.command else {
            panic!("serve command");
        };
        assert!(key_protection.key_encryption_key_stdin);
        assert!(key_protection.key_encryption_key_file.is_none());
        assert!(key_protection.api_token_file.is_none());
    }

    #[test]
    fn headless_token_file_flag_carries_only_a_path() {
        let arguments = Arguments::try_parse_from([
            "covalent-node",
            "serve",
            "--key-encryption-key-file",
            "/run/secrets/node-kek",
            "--api-token-file",
            "/run/secrets/node-token",
        ])
        .expect("parse private token path");
        let Some(Command::Serve { key_protection, .. }) = arguments.command else {
            panic!("serve command");
        };
        assert_eq!(
            key_protection.api_token_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/node-token"))
        );
        assert!(!format!("{key_protection:?}").contains("local-api-token-with"));
    }

    #[test]
    fn recover_accepts_only_private_file_paths_for_recovery_material() {
        let arguments = Arguments::try_parse_from([
            "covalent-node",
            "recover",
            "--recovery-kit-file",
            "/run/secrets/covalent-kit",
            "--recovery-key-file",
            "/run/secrets/covalent-recovery-key",
            "--key-encryption-key-file",
            "/run/secrets/node-kek",
        ])
        .expect("parse recovery files");
        let Some(Command::Recover {
            recovery_kit_file,
            recovery_key_file,
            ..
        }) = arguments.command
        else {
            panic!("recover command");
        };
        assert_eq!(
            recovery_kit_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/covalent-kit"))
        );
        assert_eq!(
            recovery_key_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/covalent-recovery-key"))
        );
        assert!(
            Arguments::try_parse_from([
                "covalent-node",
                "recover",
                "--recovery-key",
                "a-secret-must-not-be-accepted",
            ])
            .is_err()
        );

        let pipe_arguments = Arguments::try_parse_from([
            "covalent-node",
            "recover",
            "--key-encryption-key-stdin",
            "--ready-file",
            "/tmp/covalent-ready.json",
        ])
        .expect("parse inherited recovery pipe");
        let Some(Command::Recover {
            recovery_kit_file,
            recovery_key_file,
            ready_file,
            key_protection,
            ..
        }) = pipe_arguments.command
        else {
            panic!("recover pipe command");
        };
        assert!(recovery_kit_file.is_none());
        assert!(recovery_key_file.is_none());
        assert!(key_protection.key_encryption_key_stdin);
        assert_eq!(
            ready_file.as_deref(),
            Some(std::path::Path::new("/tmp/covalent-ready.json"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn provision_key_is_private_durable_and_never_replaces_an_incumbent() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::TempDir::new().expect("directory");
        let path = directory.path().join("node.kek");
        provision_key(&path, 7).expect("provision key");
        let first = std::fs::read(&path).expect("persisted KEK");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("KEK metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(read_kek_file(&path).expect("read KEK").trim().len(), 43);

        assert!(provision_key(&path, 7).is_err());
        assert_eq!(std::fs::read(path).expect("incumbent unchanged"), first);
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("KEK directory")
                .count(),
            1,
            "failed retries must not leave partial staging files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn provision_key_restart_cleans_a_pre_publish_partial() {
        let directory = tempfile::TempDir::new().expect("directory");
        let path = directory.path().join("node.kek");
        PROVISION_KEY_FAILPOINT.with(|failpoint| failpoint.set(true));
        assert!(provision_key(&path, 1).is_err());
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_dir(directory.path())
                .expect("staging directory")
                .count(),
            0,
            "failed provisioning must remove its staging file"
        );

        provision_key(&path, 1).expect("restart provisions a complete key");
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn kek_reader_refuses_symlinks() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::TempDir::new().expect("directory");
        let target = directory.path().join("actual.kek");
        let link = directory.path().join("linked.kek");
        std::fs::write(&target, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n").expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("protect target");
        symlink(&target, &link).expect("symlink");

        assert!(read_kek_file(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_material_reader_refuses_symlinks_and_broad_permissions() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::TempDir::new().expect("directory");
        let kit = directory.path().join("owner.covalent-recovery");
        let target = directory.path().join("recovery.key");
        let link = directory.path().join("recovery-link.key");
        std::fs::write(&kit, b"encrypted kit").expect("kit");
        std::fs::set_permissions(&kit, std::fs::Permissions::from_mode(0o600))
            .expect("protect kit");
        std::fs::write(&target, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n").expect("target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("broad permissions");
        assert!(super::load_recovery_bootstrap_files(&kit, &target).is_err());
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("private permissions");
        symlink(&target, &link).expect("symlink");
        assert!(super::load_recovery_bootstrap_files(&kit, &link).is_err());
    }
}
