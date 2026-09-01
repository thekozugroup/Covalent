//! Authenticated, pinned-certificate QUIC encrypted-object transport.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    ChunkProvider, CoreError, Engine, JobControl, JobState, KeyProtector, ProviderHealth,
    ProviderWriteLeaseIntent, PublicIdentity, RecoveryCapsule, RecoveryCapsuleDescriptor,
    RecoveryCapsuleLeaseIntent, RecoveryCapsuleUploadAttempt, RecoveryCapsuleUploadAttemptPhase,
    WrappedSecret, state_secret_context,
};
use covalent_protocol::{BackupId, DeviceId, PeerRole, SignedRoster, StorageLease};
use quinn::{
    ClientConfig, ConnectionError, Endpoint, ServerConfig, TransportConfig, TransportErrorCode,
    VarInt,
};
use rand_core::{OsRng, RngCore};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use zeroize::Zeroizing;

use crate::pairing_transport::{NetworkPairingService, serve_pairing_connection};

/// Version of the authenticated, two-frame QUIC storage transport.
///
/// This is intentionally independent from the local HTTP/archive API version.
pub const QUIC_TRANSPORT_VERSION: u16 = 3;
const ALPN: &[u8] = b"covalent-quic/3";
/// ALPN of the pairing-only path, which carries identity-signed pairing requests
/// between devices that are not yet paired and never carries stored objects.
pub(crate) const PAIRING_ALPN: &[u8] = b"covalent-pair/1";
const TRANSPORT_SIGNATURE_DOMAIN: &[u8] = b"covalent/authenticated-quic/v3";
const TLS_ALERT_NO_APPLICATION_PROTOCOL: u8 = 0x78;
const MAX_REPLAY_NONCES_PER_PEER: usize = 4_096;
const MAX_REQUEST_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);
const MAX_PROVIDER_RECORD_BYTES: usize = 8 * 1_024 * 1_024 + 128;
const MAX_PROVIDER_READ_BATCH_RECORDS: usize = 32;
const MAX_PROVIDER_READ_BATCH_BYTES: usize = 8 * 1_024 * 1_024 + 4 * 1_024;
const MAX_PROVIDER_WRITE_BATCH_RECORDS: usize = 64;
const MAX_PROVIDER_WRITE_BATCH_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES: usize = 16 * 1_024 * 1_024;
const JOB_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const RECOVERY_CAPSULE_SEGMENT_BYTES: usize = 4 * 1_024 * 1_024;
const STORAGE_LEASE_LIFETIME_MS: u64 = 5 * 60 * 1_000;
const MAX_RECOVERY_ATTEMPT_RECONCILIATIONS_PER_CALL: usize = 8;
const MAX_WRITE_LEASE_RECONCILIATIONS_PER_CALL: usize = 8;
const MAX_RECOVERY_CAPSULE_BYTES: u64 = 320 * 1_024 * 1_024;
const MAX_HELLO_FRAME_BYTES: usize = 8 * 1_024;
const MAX_OPERATION_FRAME_BYTES: usize = 12 * 1_024 * 1_024;
const MAX_RESPONSE_FRAME_BYTES: usize = 12 * 1_024 * 1_024;
const MAX_GLOBAL_CONNECTIONS: usize = 64;
const MAX_CONNECTIONS_PER_SOURCE: usize = 8;
const MAX_GLOBAL_STREAMS: usize = 256;
/// Pairing gets its own stream pool rather than drawing on `MAX_GLOBAL_STREAMS`,
/// so unauthenticated pairing traffic can never starve authenticated storage
/// transfers on the same endpoint.
const MAX_GLOBAL_PAIRING_STREAMS: usize = 32;
const MAX_BLOCKING_OPERATIONS: usize = 16;
const CONNECTION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const PROVIDER_CAPABILITY_FRESHNESS: Duration = Duration::from_secs(5);
const PEER_REQUEST_BURST: f64 = 4_096.0;
const PEER_REQUESTS_PER_SECOND: f64 = 256.0;
const PEER_BYTE_BURST: f64 = 512.0 * 1_024.0 * 1_024.0;
const PEER_BYTES_PER_SECOND: f64 = 256.0 * 1_024.0 * 1_024.0;
const LEGACY_TLS_IDENTITY_SCHEMA_VERSION: u16 = 1;
const PROTECTED_TLS_IDENTITY_SCHEMA_VERSION: u16 = 2;
const TLS_PRIVATE_KEY_PURPOSE: &str = "quic-tls-private-key";

#[cfg(test)]
thread_local! {
    static RECOVERY_CAPSULE_UPLOAD_FAILPOINT: std::cell::Cell<u8> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg(test)]
static SERVER_RECOVERY_RESPONSE_FAILPOINTS: OnceLock<Mutex<BTreeSet<(u8, String)>>> =
    OnceLock::new();

#[cfg(test)]
fn arm_server_recovery_response_failpoint(boundary: u8, upload_id: &str) {
    SERVER_RECOVERY_RESPONSE_FAILPOINTS
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .expect("server recovery response failpoints")
        .insert((boundary, upload_id.to_owned()));
}

#[cfg(test)]
fn take_server_recovery_response_failpoint(boundary: u8, upload_id: &str) -> bool {
    SERVER_RECOVERY_RESPONSE_FAILPOINTS
        .get_or_init(|| Mutex::new(BTreeSet::new()))
        .lock()
        .is_ok_and(|mut armed| armed.remove(&(boundary, upload_id.to_owned())))
}

#[cfg(not(test))]
const fn take_server_recovery_response_failpoint(_boundary: u8, _upload_id: &str) -> bool {
    false
}

#[cfg(test)]
fn recovery_capsule_upload_failpoint(boundary: u8) -> Result<(), CoreError> {
    RECOVERY_CAPSULE_UPLOAD_FAILPOINT.with(|armed| {
        if armed.get() == boundary {
            armed.set(0);
            return Err(CoreError::InvalidState(format!(
                "recovery capsule upload failpoint {boundary}"
            )));
        }
        Ok(())
    })
}

#[cfg(not(test))]
const fn recovery_capsule_upload_failpoint(_boundary: u8) -> Result<(), CoreError> {
    Ok(())
}

fn sha256_fingerprint(certificate_der: &[u8]) -> String {
    Sha256::digest(certificate_der)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hash_and_rewind_file(file: &mut fs::File, path: &Path) -> Result<String, CoreError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| CoreError::Io {
            operation: "rewind recovery capsule spool",
            path: path.to_path_buf(),
            source,
        })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| CoreError::Io {
            operation: "hash recovery capsule spool",
            path: path.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| CoreError::Io {
            operation: "rewind recovery capsule spool",
            path: path.to_path_buf(),
            source,
        })?;
    Ok(hasher.finalize().to_hex().to_string())
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedTlsIdentity {
    schema_version: u16,
    certificate_der: String,
    protected_private_key: WrappedSecret,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyPersistedTlsIdentity {
    schema_version: u16,
    certificate_der: String,
    private_key_der: Zeroizing<String>,
}

/// Stable self-signed TLS certificate persisted independently from app identity.
pub struct TlsIdentity {
    certificate_der: Vec<u8>,
    private_key_der: Zeroizing<Vec<u8>>,
}

impl TlsIdentity {
    /// Loads or atomically creates a long-lived certificate for certificate pinning.
    pub fn load_or_create(
        directory: impl AsRef<Path>,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, CoreError> {
        let directory = directory.as_ref();
        fs::create_dir_all(directory).map_err(|source| CoreError::Io {
            operation: "create QUIC identity directory",
            path: directory.to_path_buf(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                |source| CoreError::Io {
                    operation: "protect QUIC identity directory",
                    path: directory.to_path_buf(),
                    source,
                },
            )?;
        }
        let bundle_path = directory.join("identity.json");
        if bundle_path.exists() {
            let identity = Self::load_bundle(&bundle_path, state_root, protector)?;
            Self::remove_legacy_files(directory)?;
            return Ok(identity);
        }

        let certificate_path = directory.join("certificate.der");
        let key_path = directory.join("private-key.der");
        let legacy = match (
            fs::symlink_metadata(&certificate_path),
            fs::symlink_metadata(&key_path),
        ) {
            (Ok(certificate), Ok(key)) => Some(Self::load_legacy(
                certificate_path,
                certificate,
                key_path,
                key,
            )?),
            (Err(certificate_error), Err(key_error))
                if certificate_error.kind() == std::io::ErrorKind::NotFound
                    && key_error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            // A first-run interruption may leave one legacy file. It never formed a
            // publishable identity, so recovery creates a complete atomic bundle.
            (Ok(_), Err(key_error)) if key_error.kind() == std::io::ErrorKind::NotFound => None,
            (Err(certificate_error), Ok(_))
                if certificate_error.kind() == std::io::ErrorKind::NotFound =>
            {
                None
            }
            (_, _) => {
                return Err(CoreError::InvalidState(
                    "invalid QUIC identity files".to_owned(),
                ));
            }
        };
        let identity = match legacy {
            Some(identity) => identity,
            None => Self::generate()?,
        };
        identity.persist_bundle(&bundle_path, state_root, protector)?;
        Self::remove_legacy_files(directory)?;
        Ok(identity)
    }

    fn generate() -> Result<Self, CoreError> {
        let generated =
            rcgen::generate_simple_self_signed(vec!["covalent.local".into()]).map_err(|error| {
                CoreError::InvalidState(format!("generate QUIC certificate: {error}"))
            })?;
        Ok(Self {
            certificate_der: generated.cert.der().to_vec(),
            private_key_der: Zeroizing::new(generated.signing_key.serialize_der()),
        })
    }

    fn load_bundle(
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, CoreError> {
        let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
            operation: "inspect QUIC identity bundle",
            path: path.to_path_buf(),
            source,
        })?;
        validate_private_identity_file(&metadata, 256 * 1_024)?;
        let bytes = fs::read(path).map_err(|source| CoreError::Io {
            operation: "read QUIC identity bundle",
            path: path.to_path_buf(),
            source,
        })?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let schema_version = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| CoreError::InvalidState("QUIC identity schema is missing".to_owned()))?;
        let (certificate_der, private_key_der, migrate) = if schema_version
            == u64::from(PROTECTED_TLS_IDENTITY_SCHEMA_VERSION)
        {
            let bundle: PersistedTlsIdentity = serde_json::from_value(value)?;
            let certificate_der = URL_SAFE_NO_PAD
                .decode(bundle.certificate_der)
                .map_err(|_| CoreError::InvalidKeyMaterial)?;
            let context = state_secret_context(state_root, "tls/identity.json");
            let private_key_der =
                bundle
                    .protected_private_key
                    .open(protector, TLS_PRIVATE_KEY_PURPOSE, &context)?;
            (certificate_der, private_key_der, false)
        } else if schema_version == u64::from(LEGACY_TLS_IDENTITY_SCHEMA_VERSION) {
            let bundle: LegacyPersistedTlsIdentity = serde_json::from_value(value)?;
            if bundle.schema_version != LEGACY_TLS_IDENTITY_SCHEMA_VERSION {
                return Err(CoreError::InvalidState(
                    "unsupported QUIC identity schema".to_owned(),
                ));
            }
            let certificate_der = URL_SAFE_NO_PAD
                .decode(bundle.certificate_der)
                .map_err(|_| CoreError::InvalidKeyMaterial)?;
            let private_key_der = Zeroizing::new(
                URL_SAFE_NO_PAD
                    .decode(bundle.private_key_der.as_bytes())
                    .map_err(|_| CoreError::InvalidKeyMaterial)?,
            );
            (certificate_der, private_key_der, true)
        } else {
            return Err(CoreError::InvalidState(
                "unsupported QUIC identity schema".to_owned(),
            ));
        };
        let identity = Self {
            certificate_der,
            private_key_der,
        };
        identity.validate()?;
        if migrate {
            identity.persist_bundle(path, state_root, protector)?;
        }
        Ok(identity)
    }

    fn load_legacy(
        certificate_path: PathBuf,
        certificate: fs::Metadata,
        key_path: PathBuf,
        key: fs::Metadata,
    ) -> Result<Self, CoreError> {
        validate_identity_file(&certificate, 64 * 1_024)?;
        validate_private_identity_file(&key, 64 * 1_024)?;
        let identity = Self {
            certificate_der: fs::read(&certificate_path).map_err(|source| CoreError::Io {
                operation: "read QUIC certificate",
                path: certificate_path,
                source,
            })?,
            private_key_der: Zeroizing::new(fs::read(&key_path).map_err(|source| {
                CoreError::Io {
                    operation: "read QUIC private key",
                    path: key_path,
                    source,
                }
            })?),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn persist_bundle(
        &self,
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<(), CoreError> {
        let context = state_secret_context(state_root, "tls/identity.json");
        let bytes = Zeroizing::new(serde_json::to_vec(&PersistedTlsIdentity {
            schema_version: PROTECTED_TLS_IDENTITY_SCHEMA_VERSION,
            certificate_der: URL_SAFE_NO_PAD.encode(&self.certificate_der),
            protected_private_key: WrappedSecret::protect(
                protector,
                TLS_PRIVATE_KEY_PURPOSE,
                &context,
                Zeroizing::new(self.private_key_der.to_vec()),
            )?,
        })?);
        persist_private_replace(path, &bytes, true)
    }

    fn remove_legacy_files(directory: &Path) -> Result<(), CoreError> {
        for (name, operation) in [
            ("private-key.der", "remove legacy QUIC private key"),
            ("certificate.der", "remove legacy QUIC certificate"),
        ] {
            let path = directory.join(name);
            match fs::remove_file(&path) {
                Ok(()) => sync_directory(directory)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(CoreError::Io {
                        operation,
                        path,
                        source,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), CoreError> {
        if self.certificate_der.is_empty()
            || self.certificate_der.len() > 64 * 1_024
            || self.private_key_der.is_empty()
            || self.private_key_der.len() > 64 * 1_024
        {
            return Err(CoreError::InvalidKeyMaterial);
        }
        self.server_config().map(|_| ())
    }

    /// DER certificate used in pairing and pinning.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    /// Lowercase SHA-256 pin bound into application identity transcripts.
    #[must_use]
    pub fn certificate_fingerprint(&self) -> String {
        sha256_fingerprint(&self.certificate_der)
    }

    fn server_config(&self) -> Result<ServerConfig, CoreError> {
        self.server_config_with_alpns(&[ALPN, PAIRING_ALPN])
    }

    #[cfg(test)]
    fn server_config_with_alpn(&self, alpn: &[u8]) -> Result<ServerConfig, CoreError> {
        self.server_config_with_alpns(&[alpn])
    }

    fn server_config_with_alpns(&self, alpns: &[&[u8]]) -> Result<ServerConfig, CoreError> {
        let certificate = CertificateDer::from(self.certificate_der.clone());
        let key = PrivatePkcs8KeyDer::from(self.private_key_der.to_vec());
        let mut crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key.into())
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC TLS: {error}")))?;
        crypto.alpn_protocols = alpns.iter().map(|alpn| alpn.to_vec()).collect();
        let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC crypto: {error}")))?;
        let mut config = ServerConfig::with_crypto(Arc::new(quic_crypto));
        config.transport_config(Arc::new(transport_limits()?));
        Ok(config)
    }
}

impl fmt::Debug for TlsIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsIdentity")
            .field("certificate_fingerprint", &self.certificate_fingerprint())
            .field("private_key", &"[REDACTED]")
            .finish()
    }
}

/// Running QUIC storage endpoint.
pub struct QuicNode {
    endpoint: Endpoint,
    engine: Arc<Engine>,
    certificate_fingerprint: String,
    replay_window: Arc<Mutex<ReplayWindow>>,
    rate_limiter: Arc<Mutex<PeerRateLimiter>>,
    connection_limit: Arc<Semaphore>,
    stream_limit: Arc<Semaphore>,
    pairing_stream_limit: Arc<Semaphore>,
    blocking_limit: Arc<Semaphore>,
    source_connections: Arc<Mutex<BTreeMap<IpAddr, usize>>>,
    pairing_service: Option<Arc<NetworkPairingService>>,
}

/// Close capability retained after a node enters its serving task.
pub(crate) struct QuicNodeShutdown {
    endpoint: Endpoint,
}

impl QuicNodeShutdown {
    pub(crate) fn close(&self) {
        self.endpoint
            .close(VarInt::from_u32(0), b"node shutting down");
    }
}

impl QuicNode {
    /// Binds a resource-limited QUIC/TLS 1.3 endpoint.
    pub fn bind(
        address: SocketAddr,
        engine: Arc<Engine>,
        tls_identity: &TlsIdentity,
    ) -> Result<Self, CoreError> {
        let endpoint =
            Endpoint::server(tls_identity.server_config()?, address).map_err(|source| {
                CoreError::Io {
                    operation: "bind QUIC peer endpoint",
                    path: PathBuf::from(address.to_string()),
                    source,
                }
            })?;
        Ok(Self {
            endpoint,
            engine,
            certificate_fingerprint: tls_identity.certificate_fingerprint(),
            replay_window: Arc::new(Mutex::new(ReplayWindow::default())),
            rate_limiter: Arc::new(Mutex::new(PeerRateLimiter::default())),
            connection_limit: Arc::new(Semaphore::new(MAX_GLOBAL_CONNECTIONS)),
            stream_limit: Arc::new(Semaphore::new(MAX_GLOBAL_STREAMS)),
            pairing_stream_limit: Arc::new(Semaphore::new(MAX_GLOBAL_PAIRING_STREAMS)),
            blocking_limit: Arc::new(Semaphore::new(MAX_BLOCKING_OPERATIONS)),
            source_connections: Arc::new(Mutex::new(BTreeMap::new())),
            pairing_service: None,
        })
    }

    /// Serves the pairing-only ALPN on this same endpoint, so the advertised and
    /// discovered QUIC address is the exact address a pairing peer must reach.
    #[must_use]
    pub fn with_pairing_service(mut self, service: Arc<NetworkPairingService>) -> Self {
        self.pairing_service = Some(service);
        self
    }

    /// Actual bound address, including an assigned ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr, CoreError> {
        self.endpoint.local_addr().map_err(|source| CoreError::Io {
            operation: "inspect QUIC peer endpoint",
            path: PathBuf::from("<quic>"),
            source,
        })
    }

    /// Returns the capability used by the runtime to begin graceful shutdown.
    pub(crate) fn shutdown_handle(&self) -> QuicNodeShutdown {
        QuicNodeShutdown {
            endpoint: self.endpoint.clone(),
        }
    }

    /// Accepts connections until the endpoint is closed or the task is cancelled.
    pub async fn run(self) {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let incoming = tokio::select! {
                incoming = self.endpoint.accept() => incoming,
                _ = connections.join_next(), if !connections.is_empty() => continue,
            };
            let Some(incoming) = incoming else {
                break;
            };
            if !incoming.remote_address_validated() {
                let _ = incoming.retry();
                continue;
            }
            let remote_ip = incoming.remote_address().ip();
            let Some(source_permit) = SourceConnectionPermit::try_acquire(
                Arc::clone(&self.source_connections),
                remote_ip,
            ) else {
                incoming.refuse();
                continue;
            };
            let Ok(connection_permit) = Arc::clone(&self.connection_limit).try_acquire_owned()
            else {
                incoming.refuse();
                continue;
            };
            let engine = Arc::clone(&self.engine);
            let fingerprint = self.certificate_fingerprint.clone();
            let replay_window = Arc::clone(&self.replay_window);
            let rate_limiter = Arc::clone(&self.rate_limiter);
            let stream_limit = Arc::clone(&self.stream_limit);
            let pairing_stream_limit = Arc::clone(&self.pairing_stream_limit);
            let blocking_limit = Arc::clone(&self.blocking_limit);
            let pairing_service = self.pairing_service.clone();
            connections.spawn(async move {
                let _connection_permit = connection_permit;
                let _source_permit = source_permit;
                let Ok(Ok(connection)) =
                    tokio::time::timeout(CONNECTION_HANDSHAKE_TIMEOUT, incoming).await
                else {
                    return;
                };
                if negotiated_alpn(&connection).as_deref() == Some(PAIRING_ALPN) {
                    if let Some(service) = pairing_service {
                        serve_pairing_connection(connection, service, pairing_stream_limit).await;
                    }
                    return;
                }
                serve_storage_connection(
                    connection,
                    engine,
                    fingerprint,
                    replay_window,
                    rate_limiter,
                    stream_limit,
                    blocking_limit,
                )
                .await;
            });
        }

        while connections.join_next().await.is_some() {}
    }
}

async fn serve_storage_connection(
    connection: quinn::Connection,
    engine: Arc<Engine>,
    fingerprint: String,
    replay_window: Arc<Mutex<ReplayWindow>>,
    rate_limiter: Arc<Mutex<PeerRateLimiter>>,
    stream_limit: Arc<Semaphore>,
    blocking_limit: Arc<Semaphore>,
) {
    let mut requests = tokio::task::JoinSet::new();
    loop {
        let streams = tokio::select! {
            streams = connection.accept_bi() => streams,
            _ = requests.join_next(), if !requests.is_empty() => continue,
        };
        let mut streams = match streams {
            Ok(streams) => streams,
            Err(_) => break,
        };
        let engine = Arc::clone(&engine);
        let fingerprint = fingerprint.clone();
        let replay_window = Arc::clone(&replay_window);
        let rate_limiter = Arc::clone(&rate_limiter);
        let Ok(stream_permit) = Arc::clone(&stream_limit).try_acquire_owned() else {
            streams.0.reset(VarInt::from_u32(1)).ok();
            streams.1.stop(VarInt::from_u32(1)).ok();
            continue;
        };
        let blocking_limit = Arc::clone(&blocking_limit);
        requests.spawn(async move {
            let _stream_permit = stream_permit;
            let _ = tokio::time::timeout(
                STREAM_OPERATION_TIMEOUT,
                handle_stream(
                    streams,
                    engine,
                    &fingerprint,
                    replay_window,
                    rate_limiter,
                    blocking_limit,
                ),
            )
            .await;
        });
    }

    while requests.join_next().await.is_some() {}
}

fn negotiated_alpn(connection: &quinn::Connection) -> Option<Vec<u8>> {
    connection
        .handshake_data()?
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()?
        .protocol
}

struct SourceConnectionPermit {
    counts: Arc<Mutex<BTreeMap<IpAddr, usize>>>,
    source: IpAddr,
}

impl SourceConnectionPermit {
    fn try_acquire(counts: Arc<Mutex<BTreeMap<IpAddr, usize>>>, source: IpAddr) -> Option<Self> {
        {
            let mut guard = counts.lock().ok()?;
            let count = guard.entry(source).or_default();
            if *count >= MAX_CONNECTIONS_PER_SOURCE {
                return None;
            }
            *count += 1;
        }
        Some(Self { counts, source })
    }
}

impl Drop for SourceConnectionPermit {
    fn drop(&mut self) {
        if let Ok(mut counts) = self.counts.lock()
            && let Some(count) = counts.get_mut(&self.source)
        {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.source);
            }
        }
    }
}

/// Network-backed provider authenticated against one paired identity and pinned certificate.
#[derive(Clone)]
pub struct QuicProvider {
    address: SocketAddr,
    remote_identity: PublicIdentity,
    remote_certificate: Vec<u8>,
    local_engine: Weak<Engine>,
    request_timeout: Duration,
    client_state: Arc<tokio::sync::Mutex<Option<QuicClientState>>>,
    write_leases: Arc<Mutex<BTreeMap<BackupId, StorageLease>>>,
    lease_lifecycle: Arc<Mutex<()>>,
    metrics: Arc<TransportMetricCounters>,
}

#[derive(Default)]
struct TransportMetricCounters {
    requests: AtomicU64,
    successes: AtomicU64,
    failures: AtomicU64,
    cancellations: AtomicU64,
    timeouts: AtomicU64,
    request_bytes: AtomicU64,
    response_bytes: AtomicU64,
    last_success_unix_ms: AtomicU64,
    #[cfg(test)]
    operations: Mutex<Vec<OperationType>>,
}

/// Retained counters for one connected provider transport.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderTransportMetrics {
    pub requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub cancellations: u64,
    pub timeouts: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    pub last_success_unix_ms: Option<u64>,
}

/// Fresh, nonce-bound provider capacity facts from a signed QUIC response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCapability {
    pub schema_version: u16,
    pub provider_device_id: DeviceId,
    pub reachable: bool,
    pub observed_at_unix_ms: u64,
    pub valid_until_unix_ms: u64,
    pub usable_bytes: u64,
    pub allocated_bytes: u64,
    pub quota_bytes: u64,
    pub reserved_bytes: u64,
    pub available_objects: u64,
    pub reserved_objects: u64,
    pub free_space_reserve_bytes: u64,
}

struct QuicClientState {
    _endpoint: Endpoint,
    connection: quinn::Connection,
}

impl QuicProvider {
    /// Creates a provider from explicitly paired identity and certificate material.
    pub fn new(
        address: SocketAddr,
        remote_identity: PublicIdentity,
        remote_certificate: Vec<u8>,
        local_engine: Arc<Engine>,
    ) -> Result<Self, CoreError> {
        remote_identity.verifying_key()?;
        if remote_certificate.is_empty() || remote_certificate.len() > 64 * 1_024 {
            return Err(CoreError::InvalidKeyMaterial);
        }
        Ok(Self {
            address,
            remote_identity,
            remote_certificate,
            local_engine: Arc::downgrade(&local_engine),
            request_timeout: Duration::from_secs(5),
            client_state: Arc::new(tokio::sync::Mutex::new(None)),
            write_leases: Arc::new(Mutex::new(BTreeMap::new())),
            lease_lifecycle: Arc::new(Mutex::new(())),
            metrics: Arc::new(TransportMetricCounters::default()),
        })
    }

    /// Performs a signed, nonce-bound capacity and reachability probe.
    pub fn probe_capability(&self) -> Result<ProviderCapability, CoreError> {
        let ResponsePayload::ProviderCapability { capability } =
            self.request(Operation::GetProviderCapability)?
        else {
            return Err(CoreError::AuthenticationFailed);
        };
        let now_unix_ms = current_unix_ms()?;
        if capability.schema_version != 1
            || capability.provider_device_id != self.remote_identity.device_id
            || !capability.reachable
            || capability.valid_until_unix_ms < capability.observed_at_unix_ms
            || capability
                .valid_until_unix_ms
                .saturating_sub(capability.observed_at_unix_ms)
                != PROVIDER_CAPABILITY_FRESHNESS.as_millis() as u64
            || now_unix_ms > capability.valid_until_unix_ms
            || capability
                .allocated_bytes
                .checked_add(capability.reserved_bytes)
                .and_then(|bytes| bytes.checked_add(capability.usable_bytes))
                != Some(capability.quota_bytes)
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(capability)
    }

    /// Returns counters retained across every clone of this provider client.
    #[must_use]
    pub fn metrics(&self) -> ProviderTransportMetrics {
        let last_success_unix_ms = self.metrics.last_success_unix_ms.load(Ordering::Relaxed);
        ProviderTransportMetrics {
            requests: self.metrics.requests.load(Ordering::Relaxed),
            successes: self.metrics.successes.load(Ordering::Relaxed),
            failures: self.metrics.failures.load(Ordering::Relaxed),
            cancellations: self.metrics.cancellations.load(Ordering::Relaxed),
            timeouts: self.metrics.timeouts.load(Ordering::Relaxed),
            request_bytes: self.metrics.request_bytes.load(Ordering::Relaxed),
            response_bytes: self.metrics.response_bytes.load(Ordering::Relaxed),
            last_success_unix_ms: (last_success_unix_ms != 0).then_some(last_success_unix_ms),
        }
    }

    #[cfg(test)]
    fn operation_trace(&self) -> Vec<OperationType> {
        self.metrics
            .operations
            .lock()
            .expect("transport operation trace")
            .clone()
    }

    fn request(&self, operation: Operation) -> Result<ResponsePayload, CoreError> {
        quic_runtime()?.block_on(self.request_async(operation))
    }

    fn request_controlled(
        &self,
        operation: Operation,
        control: &JobControl,
    ) -> Result<ResponsePayload, CoreError> {
        quic_runtime()?.block_on(async {
            tokio::select! {
                result = self.request_async(operation) => result,
                error = wait_for_job_stop(control) => {
                    self.metrics.cancellations.fetch_add(1, Ordering::Relaxed);
                    Err(error)
                },
            }
        })
    }

    async fn connection(&self) -> Result<quinn::Connection, CoreError> {
        let mut state = self.client_state.lock().await;
        if let Some(existing) = state.as_ref()
            && existing.connection.close_reason().is_none()
        {
            return Ok(existing.connection.clone());
        }
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(self.remote_certificate.clone()))
            .map_err(|error| CoreError::InvalidState(format!("pin QUIC certificate: {error}")))?;
        let mut client_crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![ALPN.to_vec()];
        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC client: {error}")))?;
        let mut client = ClientConfig::new(Arc::new(quic_crypto));
        client.transport_config(Arc::new(transport_limits()?));
        let mut endpoint = Endpoint::client(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))
            .map_err(|source| CoreError::Io {
                operation: "bind QUIC client endpoint",
                path: PathBuf::from("<quic-client>"),
                source,
            })?;
        endpoint.set_default_client_config(client);
        let connecting = endpoint
            .connect(self.address, "covalent.local")
            .map_err(|error| CoreError::InvalidState(format!("start QUIC connection: {error}")))?;
        let connection = tokio::time::timeout(self.request_timeout, connecting)
            .await
            .map_err(|_| CoreError::ResourceLimit("QUIC connection timeout"))?
            .map_err(map_quic_connection_error)?;
        *state = Some(QuicClientState {
            _endpoint: endpoint,
            connection: connection.clone(),
        });
        Ok(connection)
    }

    async fn request_async(&self, operation: Operation) -> Result<ResponsePayload, CoreError> {
        #[cfg(test)]
        self.metrics
            .operations
            .lock()
            .expect("transport operation trace")
            .push(operation.kind());
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        let result = self.request_async_inner(operation, None).await;
        self.record_request_result(&result);
        result
    }

    async fn request_streamed_async(
        &self,
        operation: Operation,
        records: &[(String, Vec<u8>)],
    ) -> Result<ResponsePayload, CoreError> {
        #[cfg(test)]
        self.metrics
            .operations
            .lock()
            .expect("transport operation trace")
            .push(operation.kind());
        self.metrics.requests.fetch_add(1, Ordering::Relaxed);
        let result = self.request_async_inner(operation, Some(records)).await;
        self.record_request_result(&result);
        result
    }

    fn record_request_result(&self, result: &Result<ResponsePayload, CoreError>) {
        match &result {
            Ok(_) => {
                self.metrics.successes.fetch_add(1, Ordering::Relaxed);
                if let Ok(now_unix_ms) = current_unix_ms() {
                    self.metrics
                        .last_success_unix_ms
                        .store(now_unix_ms, Ordering::Relaxed);
                }
            }
            Err(error) => {
                self.metrics.failures.fetch_add(1, Ordering::Relaxed);
                if matches!(error, CoreError::ResourceLimit(name) if name.contains("timeout")) {
                    self.metrics.timeouts.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    async fn request_async_inner(
        &self,
        operation: Operation,
        streamed_records: Option<&[(String, Vec<u8>)]>,
    ) -> Result<ResponsePayload, CoreError> {
        let local_engine = self.local_engine.upgrade().ok_or_else(|| {
            CoreError::InvalidState("local engine is no longer available".to_owned())
        })?;
        let operation_bytes = serde_json::to_vec(&operation)?;
        if operation_bytes.len() > MAX_OPERATION_FRAME_BYTES {
            return Err(CoreError::ResourceLimit("QUIC operation frame"));
        }
        let operation_digest = blake3::hash(&operation_bytes).to_hex().to_string();
        let (streamed_payload_bytes, streamed_payload_digest) =
            streamed_payload_identity(streamed_records)?;
        let mut nonce = [0_u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let nonce = URL_SAFE_NO_PAD.encode(nonce);
        let expected_certificate_fingerprint = sha256_fingerprint(&self.remote_certificate);
        let mut hello = ClientHello {
            device_id: local_engine.device_id(),
            minimum_transport_version: QUIC_TRANSPORT_VERSION,
            maximum_transport_version: QUIC_TRANSPORT_VERSION,
            issued_at_unix_ms: current_unix_ms()?,
            nonce,
            expected_certificate_fingerprint,
            operation_type: operation.kind(),
            operation_bytes: operation_bytes.len() as u64,
            operation_digest,
            streamed_payload_bytes,
            streamed_payload_digest,
            signature: String::new(),
        };
        hello.signature = local_engine.sign_transport_transcript_with_domain(
            TRANSPORT_SIGNATURE_DOMAIN,
            &client_hello_bytes(&hello)?,
        );
        let request = WireRequest { hello, operation };
        let connection = self.connection().await?;
        let (mut send, mut receive) = connection
            .open_bi()
            .await
            .map_err(|error| CoreError::InvalidState(format!("open QUIC stream: {error}")))?;
        let hello_bytes = serde_json::to_vec(&request.hello)?;
        self.metrics.request_bytes.fetch_add(
            hello_bytes
                .len()
                .saturating_add(operation_bytes.len())
                .saturating_add(streamed_payload_bytes as usize)
                .saturating_add(if streamed_records.is_some() { 12 } else { 8 }) as u64,
            Ordering::Relaxed,
        );
        let response_bytes = tokio::time::timeout(self.request_timeout, async {
            write_frame(&mut send, &hello_bytes).await?;
            write_frame(&mut send, &operation_bytes).await?;
            if let Some(records) = streamed_records {
                write_record_payload_frame(&mut send, records, streamed_payload_bytes).await?;
            }
            send.finish().map_err(|error| {
                CoreError::InvalidState(format!("finish QUIC request: {error}"))
            })?;
            read_frame(&mut receive, MAX_RESPONSE_FRAME_BYTES).await
        })
        .await
        .map_err(|_| CoreError::ResourceLimit("QUIC operation timeout"))??;
        self.metrics.response_bytes.fetch_add(
            response_bytes.len().saturating_add(4) as u64,
            Ordering::Relaxed,
        );
        let response: WireResponse = serde_json::from_slice(&response_bytes)?;
        let remote_certificate_fingerprint = sha256_fingerprint(&self.remote_certificate);
        verify_server_response(
            &request,
            &response,
            &self.remote_identity,
            &remote_certificate_fingerprint,
        )?;
        if response.ok {
            Ok(response.payload)
        } else {
            Err(match response.error_code.as_deref() {
                Some("missing_chunk") => CoreError::MissingChunk("remote".to_owned()),
                Some("peer_revoked") => CoreError::PeerRevoked,
                Some("not_authorized") => CoreError::UnselectedProvider,
                Some("resource_limit") => CoreError::ResourceLimit("remote provider"),
                Some("protocol_incompatible") => CoreError::ProtocolNegotiationFailed,
                _ => CoreError::AuthenticationFailed,
            })
        }
    }

    /// Fetches the remote peer's latest signed roster for local anti-rollback acceptance.
    pub fn fetch_roster(&self) -> Result<Option<SignedRoster>, CoreError> {
        match self.request(Operation::GetRoster)? {
            ResponsePayload::Roster { roster } => Ok(roster),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    /// Submits a signed local roster to a remembered peer without granting new trust.
    pub fn submit_roster(&self, roster: SignedRoster) -> Result<(), CoreError> {
        match self.request(Operation::SubmitRoster { roster })? {
            ResponsePayload::RosterAccepted => Ok(()),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }
}

async fn wait_for_job_stop(control: &JobControl) -> CoreError {
    loop {
        match control.state() {
            JobState::Running => tokio::time::sleep(JOB_CONTROL_POLL_INTERVAL).await,
            JobState::Paused => return CoreError::Paused,
            JobState::Cancelled => return CoreError::Cancelled,
        }
    }
}

fn decode_provider_read_batch(
    payload: ResponsePayload,
    backup_id: BackupId,
    locators: &[String],
) -> Result<Vec<Vec<u8>>, CoreError> {
    let ResponsePayload::Records {
        backup_id: response_backup_id,
        records,
    } = payload
    else {
        return Err(CoreError::AuthenticationFailed);
    };
    if response_backup_id != backup_id || records.len() != locators.len() {
        return Err(CoreError::AuthenticationFailed);
    }
    let mut total_bytes = 0_usize;
    let mut decoded = Vec::with_capacity(records.len());
    for (expected, record) in locators.iter().zip(records) {
        if record.locator != *expected {
            return Err(CoreError::AuthenticationFailed);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(record.record)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        if bytes.is_empty() || bytes.len() > MAX_PROVIDER_RECORD_BYTES {
            return Err(CoreError::ResourceLimit("provider record"));
        }
        total_bytes = total_bytes
            .checked_add(bytes.len())
            .ok_or(CoreError::ResourceLimit("provider read batch"))?;
        if total_bytes > MAX_PROVIDER_READ_BATCH_BYTES {
            return Err(CoreError::ResourceLimit("provider read batch"));
        }
        decoded.push(bytes);
    }
    Ok(decoded)
}

fn decode_provider_write_batch_ack(
    payload: ResponsePayload,
    backup_id: BackupId,
    records: &[(String, Vec<u8>)],
) -> Result<(), CoreError> {
    let ResponsePayload::StoredBatch {
        backup_id: response_backup_id,
        locators,
    } = payload
    else {
        return Err(CoreError::AuthenticationFailed);
    };
    if response_backup_id != backup_id
        || locators.len() != records.len()
        || locators
            .iter()
            .zip(records)
            .any(|(actual, (expected, _))| actual != expected)
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn quic_runtime() -> Result<&'static tokio::runtime::Runtime, CoreError> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("covalent-quic")
            .build()
            .map_err(|error| error.to_string())
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(CoreError::InvalidState(format!(
            "create shared QUIC runtime: {error}"
        ))),
    }
}

pub(crate) fn map_quic_connection_error(error: ConnectionError) -> CoreError {
    let no_application_protocol = TransportErrorCode::crypto(TLS_ALERT_NO_APPLICATION_PROTOCOL);
    if matches!(error, ConnectionError::VersionMismatch)
        || matches!(
            &error,
            ConnectionError::TransportError(error) if error.code == no_application_protocol
        )
        || matches!(
            &error,
            ConnectionError::ConnectionClosed(error)
                if error.error_code == no_application_protocol
        )
    {
        return CoreError::ProtocolNegotiationFailed;
    }
    CoreError::InvalidState(format!("complete QUIC connection: {error}"))
}

impl fmt::Debug for QuicProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuicProvider")
            .field("address", &self.address)
            .field("remote_identity", &self.remote_identity)
            .field(
                "certificate_fingerprint",
                &sha256_fingerprint(&self.remote_certificate),
            )
            .finish()
    }
}

impl ChunkProvider for QuicProvider {
    fn device_id(&self) -> DeviceId {
        self.remote_identity.device_id
    }

    fn health(&self) -> ProviderHealth {
        ProviderHealth::Online
    }

    fn begin_backup_write(
        &self,
        backup_id: BackupId,
        maximum_new_bytes: u64,
        maximum_new_objects: u64,
    ) -> Result<(), CoreError> {
        let _lifecycle = self
            .lease_lifecycle
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        self.reconcile_pending_lease_state()?;
        let intent = ProviderWriteLeaseIntent::new(
            self.remote_identity.device_id,
            backup_id,
            maximum_new_bytes,
            maximum_new_objects,
            uuid::Uuid::new_v4().to_string(),
        );
        self.local_engine()?
            .store()
            .persist_provider_write_lease_intent(&intent)?;
        let lease = self.acquire_storage_lease_for_write_intent(&intent)?;
        self.write_leases
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .insert(backup_id, lease);
        Ok(())
    }

    fn finish_backup_write(&self, backup_id: BackupId) -> Result<(), CoreError> {
        let _lifecycle = self
            .lease_lifecycle
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let local_engine = self.local_engine()?;
        let intent = local_engine
            .store()
            .load_provider_write_lease_intent(self.remote_identity.device_id, backup_id)?;
        let lease = self
            .write_leases
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .remove(&backup_id);
        let Some(intent) = intent else {
            return if lease.is_none() {
                Ok(())
            } else {
                Err(CoreError::AuthenticationFailed)
            };
        };
        let lease = match lease {
            Some(lease) => {
                if lease.lease_id != intent.acquisition_id
                    || lease.backup_id != intent.backup_id
                    || lease.provider_device_id != intent.provider_device_id
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                lease
            }
            None => self.acquire_storage_lease_for_write_intent(&intent)?,
        };
        self.cancel_storage_lease_with_retry(&lease)?;
        local_engine
            .store()
            .complete_provider_write_lease_intent(&intent)
    }

    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
        let _ = (locator, record);
        Err(CoreError::InvalidState(
            "remote provider writes require a backup scope".to_owned(),
        ))
    }

    fn put_scoped(
        &self,
        backup_id: BackupId,
        locator: &str,
        record: &[u8],
    ) -> Result<(), CoreError> {
        if record.len() > MAX_PROVIDER_RECORD_BYTES {
            return Err(CoreError::ResourceLimit("provider record"));
        }
        let lease = self
            .write_leases
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .get(&backup_id)
            .cloned()
            .ok_or_else(|| {
                CoreError::InvalidState("backup storage lease was not preflighted".to_owned())
            })?;
        match self.request(Operation::Put {
            backup_id,
            lease,
            locator: locator.to_owned(),
            record: URL_SAFE_NO_PAD.encode(record),
        })? {
            ResponsePayload::Stored => Ok(()),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    fn put_many_scoped_controlled(
        &self,
        backup_id: BackupId,
        records: &[(String, Vec<u8>)],
        control: &JobControl,
    ) -> Result<(), CoreError> {
        if records.is_empty()
            || records.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS
            || records
                .iter()
                .map(|(_, record)| record.len())
                .sum::<usize>()
                > MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES
            || records
                .iter()
                .map(|(locator, _)| locator)
                .collect::<BTreeSet<_>>()
                .len()
                != records.len()
        {
            return Err(CoreError::ResourceLimit("provider write batch"));
        }
        let lease = self
            .write_leases
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .get(&backup_id)
            .cloned()
            .ok_or_else(|| {
                CoreError::InvalidState("backup storage lease was not preflighted".to_owned())
            })?;
        let wire_records = records
            .iter()
            .map(|(locator, record)| WireProviderWriteMetadata {
                locator: locator.clone(),
                record_bytes: record.len() as u64,
                record_digest: blake3::hash(record).to_hex().to_string(),
            })
            .collect();
        let operation = Operation::PutBatchStream {
            backup_id,
            lease,
            records: wire_records,
        };
        let payload = quic_runtime()?.block_on(async {
            tokio::select! {
                result = self.request_streamed_async(operation, records) => result,
                error = wait_for_job_stop(control) => {
                    self.metrics.cancellations.fetch_add(1, Ordering::Relaxed);
                    Err(error)
                },
            }
        })?;
        decode_provider_write_batch_ack(payload, backup_id, records)
    }

    fn get(&self, _locator: &str) -> Result<Vec<u8>, CoreError> {
        Err(CoreError::AuthenticationFailed)
    }

    fn get_scoped(&self, backup_id: BackupId, locator: &str) -> Result<Vec<u8>, CoreError> {
        match self.request(Operation::GetScoped {
            backup_id,
            locator: locator.to_owned(),
        })? {
            ResponsePayload::ScopedRecord {
                backup_id: response_backup_id,
                locator: response_locator,
                record,
            } => {
                if response_backup_id != backup_id || response_locator != locator {
                    return Err(CoreError::AuthenticationFailed);
                }
                let decoded = URL_SAFE_NO_PAD
                    .decode(record)
                    .map_err(|_| CoreError::AuthenticationFailed)?;
                if decoded.len() > MAX_PROVIDER_RECORD_BYTES {
                    return Err(CoreError::ResourceLimit("provider record"));
                }
                Ok(decoded)
            }
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    fn get_many_controlled(
        &self,
        backup_id: BackupId,
        locators: &[String],
        control: &JobControl,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        if locators.is_empty()
            || locators.len() > MAX_PROVIDER_READ_BATCH_RECORDS
            || locators.iter().collect::<BTreeSet<_>>().len() != locators.len()
        {
            return Err(CoreError::ResourceLimit("provider read batch"));
        }
        decode_provider_read_batch(
            self.request_controlled(
                Operation::GetBatch {
                    backup_id,
                    locators: locators.to_vec(),
                },
                control,
            )?,
            backup_id,
            locators,
        )
    }

    fn contains(&self, _locator: &str) -> Result<bool, CoreError> {
        Err(CoreError::AuthenticationFailed)
    }

    fn contains_scoped(&self, backup_id: BackupId, locator: &str) -> Result<bool, CoreError> {
        match self.request(Operation::ContainsScoped {
            backup_id,
            locator: locator.to_owned(),
        })? {
            ResponsePayload::ScopedPresence {
                backup_id: response_backup_id,
                locator: response_locator,
                present,
            } if response_backup_id == backup_id && response_locator == locator => Ok(present),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    fn put_recovery_capsule(&self, capsule: &RecoveryCapsule) -> Result<(), CoreError> {
        self.put_recovery_capsule_scoped(capsule.backup_id, capsule)
    }

    fn put_recovery_capsule_scoped(
        &self,
        backup_id: BackupId,
        capsule: &RecoveryCapsule,
    ) -> Result<(), CoreError> {
        if backup_id != capsule.backup_id {
            return Err(CoreError::AuthenticationFailed);
        }
        let _lifecycle = self
            .lease_lifecycle
            .lock()
            .map_err(|_| CoreError::Synchronization)?;
        let mut encoded = tempfile::NamedTempFile::new().map_err(|source| CoreError::Io {
            operation: "create recovery capsule spool",
            path: std::env::temp_dir(),
            source,
        })?;
        serde_json::to_writer(encoded.as_file_mut(), capsule)?;
        encoded
            .as_file_mut()
            .flush()
            .map_err(|source| CoreError::Io {
                operation: "flush recovery capsule spool",
                path: encoded.path().to_path_buf(),
                source,
            })?;
        let total_bytes = encoded
            .as_file()
            .metadata()
            .map_err(|source| CoreError::Io {
                operation: "inspect recovery capsule spool",
                path: encoded.path().to_path_buf(),
                source,
            })?
            .len();
        if total_bytes == 0 || total_bytes > MAX_RECOVERY_CAPSULE_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule"));
        }
        let encoded_path = encoded.path().to_path_buf();
        let capsule_digest = hash_and_rewind_file(encoded.as_file_mut(), &encoded_path)?;
        let total_segments = total_bytes.div_ceil(RECOVERY_CAPSULE_SEGMENT_BYTES as u64) as u32;
        let descriptor = RecoveryCapsuleDescriptor {
            backup_id,
            snapshot_id: capsule.snapshot_id.clone(),
            key_epoch: capsule.key_epoch,
            committed_at_unix_ms: capsule.committed_at_unix_ms,
            signer_device_id: capsule.signer_device_id,
            total_bytes,
            capsule_digest: capsule_digest.clone(),
        };
        if let Some(mut attempt) = self.load_recovery_capsule_upload_attempt(
            backup_id,
            &capsule.snapshot_id,
            &capsule_digest,
        )? {
            if attempt.total_bytes != total_bytes
                || attempt.total_segments != total_segments
                || attempt.lease.peer_device_id != capsule.signer_device_id
            {
                return Err(CoreError::AuthenticationFailed);
            }
            self.complete_matching_recovery_capsule_lease_intent(&attempt)?;
            match &attempt.phase {
                RecoveryCapsuleUploadAttemptPhase::CommitPending
                | RecoveryCapsuleUploadAttemptPhase::CommitAccepted => {
                    if self.reconcile_recovery_capsule_upload_attempt(&mut attempt)? {
                        return Ok(());
                    }
                }
                RecoveryCapsuleUploadAttemptPhase::LeaseAcquired
                | RecoveryCapsuleUploadAttemptPhase::Uploading { .. } => {
                    return self
                        .resume_segmented_recovery_capsule_upload(encoded, descriptor, attempt);
                }
            }
        }
        if let Some(intent) = self.load_recovery_capsule_lease_intent(
            backup_id,
            &capsule.snapshot_id,
            &capsule_digest,
        )? {
            if intent.total_bytes != total_bytes
                || intent.total_segments != total_segments
                || intent.provider_device_id != self.remote_identity.device_id
            {
                return Err(CoreError::AuthenticationFailed);
            }
            let attempt = self.materialize_recovery_capsule_lease_intent(&intent)?;
            return self.resume_segmented_recovery_capsule_upload(encoded, descriptor, attempt);
        }
        self.reconcile_pending_lease_state()?;
        if self.recovery_capsule_is_committed(&descriptor)? {
            return Ok(());
        }
        let local_engine = self.local_engine()?;
        local_engine
            .store()
            .ensure_recovery_capsule_upload_attempt_capacity(
                self.remote_identity.device_id,
                backup_id,
                &capsule.snapshot_id,
                &capsule_digest,
            )?;
        let intent = RecoveryCapsuleLeaseIntent::new(
            self.remote_identity.device_id,
            backup_id,
            capsule.snapshot_id.clone(),
            capsule_digest,
            total_bytes,
            total_segments,
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        self.persist_recovery_capsule_lease_intent(&intent)?;
        let attempt = self.materialize_recovery_capsule_lease_intent(&intent)?;
        self.resume_segmented_recovery_capsule_upload(encoded, descriptor, attempt)
    }

    fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
        let mut all = Vec::new();
        let mut cursor = None;
        loop {
            match self.request(Operation::ListRecoveryCapsules {
                backup_id: None,
                cursor: cursor.clone(),
                limit: 128,
            })? {
                ResponsePayload::RecoveryCapsuleDescriptors {
                    descriptors,
                    next_cursor,
                } => {
                    for descriptor in descriptors {
                        all.push(self.fetch_recovery_capsule(&descriptor)?);
                    }
                    if all.len() > 1_000_000 {
                        return Err(CoreError::ResourceLimit("recovery capsule listing"));
                    }
                    let Some(next) = next_cursor else {
                        return Ok(all);
                    };
                    if cursor.as_ref().is_some_and(|previous| previous >= &next) {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    cursor = Some(next);
                }
                _ => return Err(CoreError::AuthenticationFailed),
            }
        }
    }
}

impl QuicProvider {
    fn local_engine(&self) -> Result<Arc<Engine>, CoreError> {
        self.local_engine.upgrade().ok_or_else(|| {
            CoreError::InvalidState("local engine is no longer available".to_owned())
        })
    }

    fn load_recovery_capsule_upload_attempt(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<Option<RecoveryCapsuleUploadAttempt>, CoreError> {
        let local_engine = self.local_engine()?;
        let attempt = local_engine.store().load_recovery_capsule_upload_attempt(
            self.remote_identity.device_id,
            backup_id,
            snapshot_id,
            capsule_digest,
        )?;
        if attempt
            .as_ref()
            .is_some_and(|attempt| attempt.lease.peer_device_id != local_engine.device_id())
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(attempt)
    }

    fn load_recovery_capsule_lease_intent(
        &self,
        backup_id: BackupId,
        snapshot_id: &str,
        capsule_digest: &str,
    ) -> Result<Option<RecoveryCapsuleLeaseIntent>, CoreError> {
        self.local_engine()?
            .store()
            .load_recovery_capsule_lease_intent(
                self.remote_identity.device_id,
                backup_id,
                snapshot_id,
                capsule_digest,
            )
    }

    fn persist_recovery_capsule_lease_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<(), CoreError> {
        if intent.provider_device_id != self.remote_identity.device_id {
            return Err(CoreError::AuthenticationFailed);
        }
        self.local_engine()?
            .store()
            .persist_recovery_capsule_lease_intent(intent)
    }

    fn complete_recovery_capsule_lease_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<(), CoreError> {
        if intent.provider_device_id != self.remote_identity.device_id {
            return Err(CoreError::AuthenticationFailed);
        }
        self.local_engine()?
            .store()
            .complete_recovery_capsule_lease_intent(intent)
    }

    fn persist_recovery_capsule_upload_attempt(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        let local_engine = self.local_engine()?;
        if attempt.provider_device_id != self.remote_identity.device_id
            || attempt.lease.peer_device_id != local_engine.device_id()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        local_engine
            .store()
            .persist_recovery_capsule_upload_attempt(attempt)
    }

    fn complete_recovery_capsule_upload_attempt(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        let local_engine = self.local_engine()?;
        if attempt.provider_device_id != self.remote_identity.device_id
            || attempt.lease.peer_device_id != local_engine.device_id()
        {
            return Err(CoreError::AuthenticationFailed);
        }
        local_engine
            .store()
            .complete_recovery_capsule_upload_attempt(attempt)
    }

    fn cleanup_precommit_recovery_capsule_upload_attempt(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        if !matches!(
            &attempt.phase,
            RecoveryCapsuleUploadAttemptPhase::LeaseAcquired
                | RecoveryCapsuleUploadAttemptPhase::Uploading { .. }
        ) {
            return Err(CoreError::AuthenticationFailed);
        }
        self.cancel_storage_lease_with_retry(&attempt.lease)?;
        self.complete_recovery_capsule_upload_attempt(attempt)
    }

    fn complete_matching_recovery_capsule_lease_intent(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        let Some(intent) = self.load_recovery_capsule_lease_intent(
            attempt.backup_id,
            &attempt.snapshot_id,
            &attempt.capsule_digest,
        )?
        else {
            return Ok(());
        };
        if intent.upload_id != attempt.upload_id
            || intent.acquisition_id != attempt.lease.lease_id
            || intent.total_bytes != attempt.total_bytes
            || intent.total_segments != attempt.total_segments
        {
            return Err(CoreError::AuthenticationFailed);
        }
        self.complete_recovery_capsule_lease_intent(&intent)
    }

    fn acquire_storage_lease_for_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<StorageLease, CoreError> {
        let local_engine = self.local_engine()?;
        let response = self.request(Operation::AcquireStorageLease {
            backup_id: intent.backup_id,
            max_new_bytes: intent.total_bytes,
            max_new_objects: 1,
            acquisition_id: intent.acquisition_id.clone(),
        })?;
        let ResponsePayload::StorageLease { lease } = response else {
            return Err(CoreError::AuthenticationFailed);
        };
        if lease.lease_id != intent.acquisition_id
            || lease.peer_device_id != local_engine.device_id()
            || lease.provider_device_id != intent.provider_device_id
            || lease.backup_id != intent.backup_id
            || lease.max_new_bytes != intent.total_bytes
            || lease.max_new_objects != 1
            || lease.expires_at_unix_ms <= lease.issued_at_unix_ms
            || lease.expires_at_unix_ms - lease.issued_at_unix_ms != STORAGE_LEASE_LIFETIME_MS
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(lease)
    }

    fn acquire_storage_lease_for_write_intent(
        &self,
        intent: &ProviderWriteLeaseIntent,
    ) -> Result<StorageLease, CoreError> {
        if intent.provider_device_id != self.remote_identity.device_id {
            return Err(CoreError::AuthenticationFailed);
        }
        let local_engine = self.local_engine()?;
        let response = self.request(Operation::AcquireStorageLease {
            backup_id: intent.backup_id,
            max_new_bytes: intent.maximum_new_bytes,
            max_new_objects: intent.maximum_new_objects,
            acquisition_id: intent.acquisition_id.clone(),
        })?;
        let ResponsePayload::StorageLease { lease } = response else {
            return Err(CoreError::AuthenticationFailed);
        };
        if lease.lease_id != intent.acquisition_id
            || lease.peer_device_id != local_engine.device_id()
            || lease.provider_device_id != intent.provider_device_id
            || lease.backup_id != intent.backup_id
            || lease.max_new_bytes != intent.maximum_new_bytes
            || lease.max_new_objects != intent.maximum_new_objects
            || lease.expires_at_unix_ms <= lease.issued_at_unix_ms
            || lease.expires_at_unix_ms - lease.issued_at_unix_ms != STORAGE_LEASE_LIFETIME_MS
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(lease)
    }

    fn reconcile_pending_write_lease_state(&self) -> Result<(), CoreError> {
        let local_engine = self.local_engine()?;
        let intents = local_engine
            .store()
            .provider_write_lease_intents_for_provider(self.remote_identity.device_id)?;
        for intent in intents
            .iter()
            .take(MAX_WRITE_LEASE_RECONCILIATIONS_PER_CALL)
        {
            let lease = self.acquire_storage_lease_for_write_intent(intent)?;
            self.cancel_storage_lease_with_retry(&lease)?;
            local_engine
                .store()
                .complete_provider_write_lease_intent(intent)?;
        }
        if intents.len() > MAX_WRITE_LEASE_RECONCILIATIONS_PER_CALL {
            Err(CoreError::ResourceLimit(
                "provider write lease reconciliation",
            ))
        } else {
            Ok(())
        }
    }

    fn reconcile_pending_lease_state(&self) -> Result<(), CoreError> {
        self.reconcile_pending_recovery_capsule_state()?;
        self.reconcile_pending_write_lease_state()
    }

    fn materialize_recovery_capsule_lease_intent(
        &self,
        intent: &RecoveryCapsuleLeaseIntent,
    ) -> Result<RecoveryCapsuleUploadAttempt, CoreError> {
        let lease = self.acquire_storage_lease_for_intent(intent)?;
        recovery_capsule_upload_failpoint(8)?;
        let attempt = RecoveryCapsuleUploadAttempt::new(
            intent.provider_device_id,
            intent.backup_id,
            intent.snapshot_id.clone(),
            intent.capsule_digest.clone(),
            intent.total_bytes,
            intent.total_segments,
            lease,
            intent.upload_id.clone(),
        );
        self.persist_recovery_capsule_upload_attempt(&attempt)?;
        self.complete_recovery_capsule_lease_intent(intent)?;
        Ok(attempt)
    }

    fn recovery_capsule_is_committed(
        &self,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<bool, CoreError> {
        self.recovery_capsule_identity_is_committed(&RecoveryCapsuleIdentity {
            backup_id: descriptor.backup_id,
            snapshot_id: descriptor.snapshot_id.clone(),
            signer_device_id: descriptor.signer_device_id,
            total_bytes: descriptor.total_bytes,
            capsule_digest: descriptor.capsule_digest.clone(),
        })
    }

    fn recovery_capsule_attempt_is_committed(
        &self,
        attempt: &RecoveryCapsuleUploadAttempt,
    ) -> Result<bool, CoreError> {
        self.recovery_capsule_identity_is_committed(&RecoveryCapsuleIdentity {
            backup_id: attempt.backup_id,
            snapshot_id: attempt.snapshot_id.clone(),
            signer_device_id: attempt.lease.peer_device_id,
            total_bytes: attempt.total_bytes,
            capsule_digest: attempt.capsule_digest.clone(),
        })
    }

    fn recovery_capsule_identity_is_committed(
        &self,
        identity: &RecoveryCapsuleIdentity,
    ) -> Result<bool, CoreError> {
        match self.request(Operation::QueryRecoveryCapsule {
            identity: identity.clone(),
        })? {
            ResponsePayload::RecoveryCapsuleStatus {
                identity: response_identity,
                committed,
            } if response_identity == *identity => Ok(committed),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    fn reconcile_pending_recovery_capsule_state(&self) -> Result<(), CoreError> {
        let local_engine = self.local_engine()?;
        let attempts = local_engine
            .store()
            .recovery_capsule_upload_attempts_for_provider(self.remote_identity.device_id)?;
        let intents = local_engine
            .store()
            .recovery_capsule_lease_intents_for_provider(self.remote_identity.device_id)?;
        let attempt_count = attempts.len();
        let pending = attempts.len().saturating_add(intents.len());
        for mut attempt in attempts
            .into_iter()
            .take(MAX_RECOVERY_ATTEMPT_RECONCILIATIONS_PER_CALL)
        {
            if attempt.lease.peer_device_id != local_engine.device_id() {
                return Err(CoreError::AuthenticationFailed);
            }
            match &attempt.phase {
                RecoveryCapsuleUploadAttemptPhase::LeaseAcquired
                | RecoveryCapsuleUploadAttemptPhase::Uploading { .. } => {
                    self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt)?;
                }
                RecoveryCapsuleUploadAttemptPhase::CommitPending
                | RecoveryCapsuleUploadAttemptPhase::CommitAccepted => {
                    self.reconcile_recovery_capsule_upload_attempt(&mut attempt)?;
                }
            }
        }
        let processed_attempts = attempt_count.min(MAX_RECOVERY_ATTEMPT_RECONCILIATIONS_PER_CALL);
        for intent in intents
            .into_iter()
            .take(MAX_RECOVERY_ATTEMPT_RECONCILIATIONS_PER_CALL.saturating_sub(processed_attempts))
        {
            if let Some(attempt) = self.load_recovery_capsule_upload_attempt(
                intent.backup_id,
                &intent.snapshot_id,
                &intent.capsule_digest,
            )? {
                if attempt.upload_id != intent.upload_id
                    || attempt.lease.lease_id != intent.acquisition_id
                {
                    return Err(CoreError::AuthenticationFailed);
                }
                self.complete_recovery_capsule_lease_intent(&intent)?;
                continue;
            }
            let lease = self.acquire_storage_lease_for_intent(&intent)?;
            self.cancel_storage_lease_with_retry(&lease)?;
            self.complete_recovery_capsule_lease_intent(&intent)?;
        }
        if pending > MAX_RECOVERY_ATTEMPT_RECONCILIATIONS_PER_CALL {
            Err(CoreError::ResourceLimit(
                "recovery capsule upload reconciliation",
            ))
        } else {
            Ok(())
        }
    }

    fn reconcile_recovery_capsule_upload_attempt(
        &self,
        attempt: &mut RecoveryCapsuleUploadAttempt,
    ) -> Result<bool, CoreError> {
        if matches!(
            &attempt.phase,
            RecoveryCapsuleUploadAttemptPhase::CommitPending
        ) {
            let commit = Operation::CommitRecoveryCapsuleUpload {
                backup_id: attempt.backup_id,
                lease: attempt.lease.clone(),
                upload_id: attempt.upload_id.clone(),
            };
            let first_error = self.request_stored_with_retry(commit.clone()).err();
            if let Some(first_error) = first_error {
                if self
                    .cancel_storage_lease_with_retry(&attempt.lease)
                    .is_err()
                {
                    return Err(first_error);
                }
                match self.request(commit) {
                    Ok(ResponsePayload::Stored) => {}
                    Err(CoreError::AuthenticationFailed) => {
                        if self
                            .acknowledge_recovery_capsule_upload_with_retry(
                                &attempt.lease,
                                &attempt.upload_id,
                            )
                            .is_err()
                            || self
                                .complete_recovery_capsule_upload_attempt(attempt)
                                .is_err()
                        {
                            return Err(first_error);
                        }
                        return Ok(false);
                    }
                    Ok(_) | Err(_) => return Err(first_error),
                }
            }
            attempt.phase = RecoveryCapsuleUploadAttemptPhase::CommitAccepted;
            self.persist_recovery_capsule_upload_attempt(attempt)?;
        }
        if !matches!(
            &attempt.phase,
            RecoveryCapsuleUploadAttemptPhase::CommitAccepted
        ) {
            return Err(CoreError::AuthenticationFailed);
        }
        self.cancel_storage_lease_with_retry(&attempt.lease)?;
        if let Err(error) =
            self.acknowledge_recovery_capsule_upload_with_retry(&attempt.lease, &attempt.upload_id)
            && !self.recovery_capsule_attempt_is_committed(attempt)?
        {
            return Err(error);
        }
        self.complete_recovery_capsule_upload_attempt(attempt)?;
        Ok(true)
    }

    fn resume_segmented_recovery_capsule_upload(
        &self,
        mut encoded: tempfile::NamedTempFile,
        descriptor: RecoveryCapsuleDescriptor,
        mut attempt: RecoveryCapsuleUploadAttempt,
    ) -> Result<(), CoreError> {
        let backup_id = attempt.backup_id;
        let begin = Operation::BeginRecoveryCapsuleUpload {
            backup_id,
            lease: attempt.lease.clone(),
            upload_id: attempt.upload_id.clone(),
            total_bytes: attempt.total_bytes,
            total_segments: attempt.total_segments,
            capsule_digest: attempt.capsule_digest.clone(),
            descriptor,
        };
        if let Err(error) = recovery_capsule_upload_failpoint(1) {
            let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
            return Err(error);
        }
        if let Err(error) = self.request_stored_with_retry(begin) {
            let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
            return Err(error);
        }
        let next_segment = match &attempt.phase {
            RecoveryCapsuleUploadAttemptPhase::LeaseAcquired => 0,
            RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment } => *next_segment,
            _ => return Err(CoreError::AuthenticationFailed),
        };
        let previous_attempt = attempt.clone();
        attempt.phase = RecoveryCapsuleUploadAttemptPhase::Uploading { next_segment };
        if let Err(error) = self.persist_recovery_capsule_upload_attempt(&attempt) {
            let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&previous_attempt);
            return Err(error);
        }
        if let Err(error) = recovery_capsule_upload_failpoint(2) {
            let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
            return Err(error);
        }
        encoded
            .as_file_mut()
            .seek(SeekFrom::Start(
                u64::from(next_segment) * RECOVERY_CAPSULE_SEGMENT_BYTES as u64,
            ))
            .map_err(|source| CoreError::Io {
                operation: "seek recovery capsule spool",
                path: encoded.path().to_path_buf(),
                source,
            })?;
        let mut segment = vec![0_u8; RECOVERY_CAPSULE_SEGMENT_BYTES];
        for index in next_segment..attempt.total_segments {
            let offset = u64::from(index) * RECOVERY_CAPSULE_SEGMENT_BYTES as u64;
            let length =
                (attempt.total_bytes - offset).min(RECOVERY_CAPSULE_SEGMENT_BYTES as u64) as usize;
            if let Err(source) = encoded.as_file_mut().read_exact(&mut segment[..length]) {
                let error = CoreError::Io {
                    operation: "read recovery capsule spool",
                    path: encoded.path().to_path_buf(),
                    source,
                };
                let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
                return Err(error);
            }
            let segment = &segment[..length];
            let operation = Operation::PutRecoveryCapsuleSegment {
                backup_id,
                lease: attempt.lease.clone(),
                upload_id: attempt.upload_id.clone(),
                index,
                segment: URL_SAFE_NO_PAD.encode(segment),
                segment_digest: blake3::hash(segment).to_hex().to_string(),
            };
            if let Err(error) = self.request_stored_with_retry(operation) {
                let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
                return Err(error);
            }
            let previous_attempt = attempt.clone();
            attempt.phase = RecoveryCapsuleUploadAttemptPhase::Uploading {
                next_segment: index + 1,
            };
            if let Err(error) = self.persist_recovery_capsule_upload_attempt(&attempt) {
                let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&previous_attempt);
                return Err(error);
            }
            if index == 0
                && let Err(error) = recovery_capsule_upload_failpoint(3)
            {
                let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&attempt);
                return Err(error);
            }
        }
        let previous_attempt = attempt.clone();
        attempt.phase = RecoveryCapsuleUploadAttemptPhase::CommitPending;
        if let Err(error) = self.persist_recovery_capsule_upload_attempt(&attempt) {
            let _ = self.cleanup_precommit_recovery_capsule_upload_attempt(&previous_attempt);
            return Err(error);
        }
        let commit = Operation::CommitRecoveryCapsuleUpload {
            backup_id,
            lease: attempt.lease.clone(),
            upload_id: attempt.upload_id.clone(),
        };
        if let Err(error) = self.request_stored_with_retry(commit) {
            if matches!(
                self.reconcile_recovery_capsule_upload_attempt(&mut attempt),
                Ok(true)
            ) {
                return Ok(());
            }
            return Err(error);
        }
        if let Err(error) = recovery_capsule_upload_failpoint(4) {
            if matches!(
                self.reconcile_recovery_capsule_upload_attempt(&mut attempt),
                Ok(true)
            ) {
                return Ok(());
            }
            return Err(error);
        }
        recovery_capsule_upload_failpoint(5)?;
        attempt.phase = RecoveryCapsuleUploadAttemptPhase::CommitAccepted;
        self.persist_recovery_capsule_upload_attempt(&attempt)?;
        recovery_capsule_upload_failpoint(6)?;
        self.cancel_storage_lease_with_retry(&attempt.lease)?;
        if let Err(error) =
            self.acknowledge_recovery_capsule_upload_with_retry(&attempt.lease, &attempt.upload_id)
            && !self.recovery_capsule_attempt_is_committed(&attempt)?
        {
            return Err(error);
        }
        recovery_capsule_upload_failpoint(7)?;
        self.complete_recovery_capsule_upload_attempt(&attempt)
    }

    fn request_stored_with_retry(&self, operation: Operation) -> Result<(), CoreError> {
        let first_error = match self.request(operation.clone()) {
            Ok(ResponsePayload::Stored) => return Ok(()),
            Ok(_) => CoreError::AuthenticationFailed,
            Err(error) => error,
        };
        match self.request(operation) {
            Ok(ResponsePayload::Stored) => Ok(()),
            Ok(_) | Err(_) => Err(first_error),
        }
    }

    fn cancel_storage_lease_with_retry(&self, lease: &StorageLease) -> Result<(), CoreError> {
        self.request_stored_with_retry(Operation::CancelStorageLease {
            lease: lease.clone(),
        })
    }

    fn acknowledge_recovery_capsule_upload_with_retry(
        &self,
        lease: &StorageLease,
        upload_id: &str,
    ) -> Result<(), CoreError> {
        self.request_stored_with_retry(Operation::AcknowledgeRecoveryCapsuleUpload {
            lease: lease.clone(),
            upload_id: upload_id.to_owned(),
        })
    }

    fn fetch_recovery_capsule(
        &self,
        descriptor: &RecoveryCapsuleDescriptor,
    ) -> Result<RecoveryCapsule, CoreError> {
        if descriptor.total_bytes == 0 || descriptor.total_bytes > MAX_RECOVERY_CAPSULE_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule"));
        }
        let mut spool = tempfile::NamedTempFile::new().map_err(|source| CoreError::Io {
            operation: "create recovery capsule download spool",
            path: std::env::temp_dir(),
            source,
        })?;
        let mut hasher = blake3::Hasher::new();
        let mut offset = 0_u64;
        while offset < descriptor.total_bytes {
            match self.request(Operation::GetRecoveryCapsuleSegment {
                backup_id: descriptor.backup_id,
                snapshot_id: descriptor.snapshot_id.clone(),
                offset,
                maximum_bytes: RECOVERY_CAPSULE_SEGMENT_BYTES as u32,
            })? {
                ResponsePayload::RecoveryCapsuleSegment {
                    segment,
                    total_bytes,
                    capsule_digest,
                } => {
                    if total_bytes != descriptor.total_bytes
                        || capsule_digest != descriptor.capsule_digest
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    let segment = URL_SAFE_NO_PAD
                        .decode(segment)
                        .map_err(|_| CoreError::AuthenticationFailed)?;
                    if segment.is_empty() || segment.len() > RECOVERY_CAPSULE_SEGMENT_BYTES {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    if offset
                        .checked_add(segment.len() as u64)
                        .is_none_or(|end| end > descriptor.total_bytes)
                    {
                        return Err(CoreError::AuthenticationFailed);
                    }
                    spool
                        .as_file_mut()
                        .write_all(&segment)
                        .map_err(|source| CoreError::Io {
                            operation: "write recovery capsule download spool",
                            path: spool.path().to_path_buf(),
                            source,
                        })?;
                    hasher.update(&segment);
                    offset = offset
                        .checked_add(segment.len() as u64)
                        .ok_or(CoreError::ResourceLimit("recovery capsule offset"))?;
                }
                _ => return Err(CoreError::AuthenticationFailed),
            }
        }
        if offset != descriptor.total_bytes
            || hasher.finalize().to_hex().as_str() != descriptor.capsule_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        spool
            .as_file_mut()
            .seek(SeekFrom::Start(0))
            .map_err(|source| CoreError::Io {
                operation: "rewind recovery capsule download spool",
                path: spool.path().to_path_buf(),
                source,
            })?;
        let capsule: RecoveryCapsule = serde_json::from_reader(BufReader::new(spool.as_file()))?;
        if capsule.backup_id != descriptor.backup_id
            || capsule.snapshot_id != descriptor.snapshot_id
            || capsule.signer_device_id != descriptor.signer_device_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(capsule)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientHello {
    device_id: DeviceId,
    minimum_transport_version: u16,
    maximum_transport_version: u16,
    issued_at_unix_ms: u64,
    nonce: String,
    expected_certificate_fingerprint: String,
    operation_type: OperationType,
    operation_bytes: u64,
    operation_digest: String,
    #[serde(default)]
    streamed_payload_bytes: u64,
    #[serde(default)]
    streamed_payload_digest: String,
    signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerHello {
    device_id: DeviceId,
    negotiated_transport_version: u16,
    request_nonce: String,
    response_nonce: String,
    certificate_fingerprint: String,
    response_digest: String,
    signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRequest {
    hello: ClientHello,
    operation: Operation,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsuleIdentity {
    backup_id: BackupId,
    snapshot_id: String,
    signer_device_id: DeviceId,
    total_bytes: u64,
    capsule_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireResponse {
    ok: bool,
    payload: ResponsePayload,
    error_code: Option<String>,
    server_hello: ServerHello,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    GetProviderCapability,
    AcquireStorageLease {
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        acquisition_id: String,
    },
    CancelStorageLease {
        lease: StorageLease,
    },
    Put {
        backup_id: BackupId,
        lease: StorageLease,
        locator: String,
        record: String,
    },
    PutBatch {
        backup_id: BackupId,
        lease: StorageLease,
        records: Vec<WireProviderWriteRecord>,
    },
    PutBatchStream {
        backup_id: BackupId,
        lease: StorageLease,
        records: Vec<WireProviderWriteMetadata>,
    },
    GetScoped {
        backup_id: BackupId,
        locator: String,
    },
    GetBatch {
        backup_id: BackupId,
        locators: Vec<String>,
    },
    ContainsScoped {
        backup_id: BackupId,
        locator: String,
    },
    PutRecoveryCapsule {
        backup_id: BackupId,
        lease: StorageLease,
        capsule: RecoveryCapsule,
    },
    BeginRecoveryCapsuleUpload {
        backup_id: BackupId,
        lease: StorageLease,
        upload_id: String,
        total_bytes: u64,
        total_segments: u32,
        capsule_digest: String,
        descriptor: RecoveryCapsuleDescriptor,
    },
    PutRecoveryCapsuleSegment {
        backup_id: BackupId,
        lease: StorageLease,
        upload_id: String,
        index: u32,
        segment: String,
        segment_digest: String,
    },
    CommitRecoveryCapsuleUpload {
        backup_id: BackupId,
        lease: StorageLease,
        upload_id: String,
    },
    AcknowledgeRecoveryCapsuleUpload {
        lease: StorageLease,
        upload_id: String,
    },
    QueryRecoveryCapsule {
        identity: RecoveryCapsuleIdentity,
    },
    ListRecoveryCapsules {
        backup_id: Option<BackupId>,
        cursor: Option<String>,
        limit: u16,
    },
    GetRecoveryCapsuleSegment {
        backup_id: BackupId,
        snapshot_id: String,
        offset: u64,
        maximum_bytes: u32,
    },
    GetRoster,
    SubmitRoster {
        roster: SignedRoster,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperationType {
    GetProviderCapability,
    AcquireStorageLease,
    CancelStorageLease,
    Put,
    PutBatch,
    PutBatchStream,
    GetScoped,
    GetBatch,
    ContainsScoped,
    PutRecoveryCapsule,
    BeginRecoveryCapsuleUpload,
    PutRecoveryCapsuleSegment,
    CommitRecoveryCapsuleUpload,
    AcknowledgeRecoveryCapsuleUpload,
    QueryRecoveryCapsule,
    ListRecoveryCapsules,
    GetRecoveryCapsuleSegment,
    GetRoster,
    SubmitRoster,
}

impl Operation {
    const fn kind(&self) -> OperationType {
        match self {
            Self::GetProviderCapability => OperationType::GetProviderCapability,
            Self::AcquireStorageLease { .. } => OperationType::AcquireStorageLease,
            Self::CancelStorageLease { .. } => OperationType::CancelStorageLease,
            Self::Put { .. } => OperationType::Put,
            Self::PutBatch { .. } => OperationType::PutBatch,
            Self::PutBatchStream { .. } => OperationType::PutBatchStream,
            Self::GetScoped { .. } => OperationType::GetScoped,
            Self::GetBatch { .. } => OperationType::GetBatch,
            Self::ContainsScoped { .. } => OperationType::ContainsScoped,
            Self::PutRecoveryCapsule { .. } => OperationType::PutRecoveryCapsule,
            Self::BeginRecoveryCapsuleUpload { .. } => OperationType::BeginRecoveryCapsuleUpload,
            Self::PutRecoveryCapsuleSegment { .. } => OperationType::PutRecoveryCapsuleSegment,
            Self::CommitRecoveryCapsuleUpload { .. } => OperationType::CommitRecoveryCapsuleUpload,
            Self::AcknowledgeRecoveryCapsuleUpload { .. } => {
                OperationType::AcknowledgeRecoveryCapsuleUpload
            }
            Self::QueryRecoveryCapsule { .. } => OperationType::QueryRecoveryCapsule,
            Self::ListRecoveryCapsules { .. } => OperationType::ListRecoveryCapsules,
            Self::GetRecoveryCapsuleSegment { .. } => OperationType::GetRecoveryCapsuleSegment,
            Self::GetRoster => OperationType::GetRoster,
            Self::SubmitRoster { .. } => OperationType::SubmitRoster,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ResponsePayload {
    ProviderCapability {
        capability: ProviderCapability,
    },
    StorageLease {
        lease: StorageLease,
    },
    RecoveryCapsuleStatus {
        identity: RecoveryCapsuleIdentity,
        committed: bool,
    },
    Stored,
    StoredBatch {
        backup_id: BackupId,
        locators: Vec<String>,
    },
    ScopedRecord {
        backup_id: BackupId,
        locator: String,
        record: String,
    },
    Records {
        backup_id: BackupId,
        records: Vec<WireProviderRecord>,
    },
    ScopedPresence {
        backup_id: BackupId,
        locator: String,
        present: bool,
    },
    RecoveryCapsuleDescriptors {
        descriptors: Vec<RecoveryCapsuleDescriptor>,
        next_cursor: Option<String>,
    },
    RecoveryCapsuleSegment {
        segment: String,
        total_bytes: u64,
        capsule_digest: String,
    },
    Roster {
        roster: Option<SignedRoster>,
    },
    RosterAccepted,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProviderRecord {
    locator: String,
    record: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProviderWriteRecord {
    locator: String,
    record: String,
    record_digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireProviderWriteMetadata {
    locator: String,
    record_bytes: u64,
    record_digest: String,
}

#[derive(Default)]
struct PeerReplayWindow {
    ordered: VecDeque<String>,
    members: BTreeSet<String>,
}

#[derive(Default)]
struct ReplayWindow {
    peers: std::collections::BTreeMap<DeviceId, PeerReplayWindow>,
}

#[derive(Default)]
struct PeerRateLimiter {
    peers: BTreeMap<DeviceId, PeerBudget>,
}

struct PeerBudget {
    request_tokens: f64,
    byte_tokens: f64,
    last_refill: Instant,
}

impl PeerBudget {
    fn new(now: Instant) -> Self {
        Self {
            request_tokens: PEER_REQUEST_BURST,
            byte_tokens: PEER_BYTE_BURST,
            last_refill: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.request_tokens =
            (self.request_tokens + elapsed * PEER_REQUESTS_PER_SECOND).min(PEER_REQUEST_BURST);
        self.byte_tokens =
            (self.byte_tokens + elapsed * PEER_BYTES_PER_SECOND).min(PEER_BYTE_BURST);
        self.last_refill = now;
    }
}

impl PeerRateLimiter {
    fn admission_delay(
        &mut self,
        peer_id: DeviceId,
        bytes: usize,
        charge_request: bool,
        now: Instant,
    ) -> Option<Duration> {
        self.peers.retain(|_, budget| {
            now.saturating_duration_since(budget.last_refill) < Duration::from_secs(10 * 60)
        });
        let budget = self
            .peers
            .entry(peer_id)
            .or_insert_with(|| PeerBudget::new(now));
        budget.refill(now);
        let byte_cost = bytes as f64;
        let request_delay = if charge_request && budget.request_tokens < 1.0 {
            (1.0 - budget.request_tokens) / PEER_REQUESTS_PER_SECOND
        } else {
            0.0
        };
        let byte_delay = if budget.byte_tokens < byte_cost {
            (byte_cost - budget.byte_tokens) / PEER_BYTES_PER_SECOND
        } else {
            0.0
        };
        let delay = request_delay.max(byte_delay);
        if delay > 0.0 {
            return Some(Duration::from_secs_f64(delay));
        }
        if charge_request {
            budget.request_tokens -= 1.0;
        }
        budget.byte_tokens -= byte_cost;
        None
    }
}

async fn wait_for_peer_budget(
    rate_limiter: &Mutex<PeerRateLimiter>,
    peer_id: DeviceId,
    bytes: usize,
    charge_request: bool,
) -> Result<(), CoreError> {
    if bytes as f64 > PEER_BYTE_BURST {
        return Err(CoreError::ResourceLimit("peer byte burst"));
    }
    loop {
        let delay = rate_limiter
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .admission_delay(peer_id, bytes, charge_request, Instant::now());
        let Some(delay) = delay else {
            return Ok(());
        };
        tokio::time::sleep(delay.max(Duration::from_millis(1))).await;
    }
}

fn streamed_payload_identity(
    records: Option<&[(String, Vec<u8>)]>,
) -> Result<(u64, String), CoreError> {
    let Some(records) = records else {
        return Ok((0, String::new()));
    };
    if records.is_empty() || records.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS {
        return Err(CoreError::ResourceLimit("provider streamed write batch"));
    }
    let mut bytes = 0_u64;
    let mut digest = blake3::Hasher::new();
    for (_, record) in records {
        bytes = bytes
            .checked_add(record.len() as u64)
            .ok_or(CoreError::ResourceLimit("provider streamed write batch"))?;
        digest.update(record);
    }
    if bytes == 0 || bytes > MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES as u64 {
        return Err(CoreError::ResourceLimit("provider streamed write batch"));
    }
    Ok((bytes, digest.finalize().to_hex().to_string()))
}

async fn write_record_payload_frame(
    send: &mut quinn::SendStream,
    records: &[(String, Vec<u8>)],
    total_bytes: u64,
) -> Result<(), CoreError> {
    let length = u32::try_from(total_bytes)
        .map_err(|_| CoreError::ResourceLimit("provider streamed write batch"))?;
    send.write_u32(length)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)?;
    for (_, record) in records {
        send.write_all(record)
            .await
            .map_err(|_| CoreError::AuthenticationFailed)?;
    }
    Ok(())
}

impl ReplayWindow {
    fn insert_fresh(&mut self, peer_id: DeviceId, nonce: String) -> bool {
        let peer = self.peers.entry(peer_id).or_default();
        if !peer.members.insert(nonce.clone()) {
            return false;
        }
        peer.ordered.push_back(nonce);
        while peer.ordered.len() > MAX_REPLAY_NONCES_PER_PEER {
            if let Some(expired) = peer.ordered.pop_front() {
                peer.members.remove(&expired);
            }
        }
        true
    }
}

pub(crate) async fn write_frame(
    send: &mut quinn::SendStream,
    bytes: &[u8],
) -> Result<(), CoreError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| CoreError::ResourceLimit("QUIC frame length"))?;
    send.write_u32(length)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)?;
    send.write_all(bytes)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)
}

pub(crate) async fn read_frame(
    receive: &mut quinn::RecvStream,
    maximum: usize,
) -> Result<Vec<u8>, CoreError> {
    let length = receive
        .read_u32()
        .await
        .map_err(|_| CoreError::AuthenticationFailed)? as usize;
    if length == 0 || length > maximum {
        return Err(CoreError::ResourceLimit("QUIC frame"));
    }
    let mut bytes = vec![0_u8; length];
    receive
        .read_exact(&mut bytes)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)?;
    Ok(bytes)
}

async fn handle_stream(
    (mut send, mut receive): (quinn::SendStream, quinn::RecvStream),
    engine: Arc<Engine>,
    certificate_fingerprint: &str,
    replay_window: Arc<Mutex<ReplayWindow>>,
    rate_limiter: Arc<Mutex<PeerRateLimiter>>,
    blocking_limit: Arc<Semaphore>,
) -> Result<(), CoreError> {
    let hello_bytes = read_frame(&mut receive, MAX_HELLO_FRAME_BYTES).await?;
    let hello: ClientHello = serde_json::from_slice(&hello_bytes)?;
    authenticate_client_hello(&hello, &engine, certificate_fingerprint, &replay_window)?;
    let operation_bytes = usize::try_from(hello.operation_bytes)
        .map_err(|_| CoreError::ResourceLimit("QUIC operation frame"))?;
    let streamed_payload_bytes = usize::try_from(hello.streamed_payload_bytes)
        .map_err(|_| CoreError::ResourceLimit("provider streamed write batch"))?;
    wait_for_peer_budget(
        &rate_limiter,
        hello.device_id,
        hello_bytes
            .len()
            .saturating_add(operation_bytes)
            .saturating_add(streamed_payload_bytes),
        true,
    )
    .await?;
    // Authenticate and pace first, then bound the number of streams allowed to allocate a
    // large operation/response frame. Retain the permit through the response write so an
    // authorized peer cannot multiply the frame ceiling by the global stream count.
    let permit = blocking_limit
        .acquire_owned()
        .await
        .map_err(|_| CoreError::ResourceLimit("QUIC storage workers"))?;
    let operation_bytes = read_frame(&mut receive, MAX_OPERATION_FRAME_BYTES).await?;
    if operation_bytes.len() as u64 != hello.operation_bytes
        || blake3::hash(&operation_bytes).to_hex().as_str() != hello.operation_digest
    {
        return Err(CoreError::AuthenticationFailed);
    }
    let operation: Operation = serde_json::from_slice(&operation_bytes)?;
    if operation.kind() != hello.operation_type {
        return Err(CoreError::AuthenticationFailed);
    }
    let streamed_payload = if matches!(operation, Operation::PutBatchStream { .. }) {
        let payload = read_frame(&mut receive, MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES).await?;
        if payload.len() as u64 != hello.streamed_payload_bytes
            || blake3::hash(&payload).to_hex().as_str() != hello.streamed_payload_digest
        {
            return Err(CoreError::AuthenticationFailed);
        }
        Some(payload)
    } else {
        None
    };
    let recovery_response_failpoint = match &operation {
        Operation::CommitRecoveryCapsuleUpload { upload_id, .. } => Some((1, upload_id.clone())),
        Operation::AcknowledgeRecoveryCapsuleUpload { upload_id, .. } => {
            Some((2, upload_id.clone()))
        }
        Operation::AcquireStorageLease { acquisition_id, .. } => Some((3, acquisition_id.clone())),
        _ => None,
    };
    let worker_engine = Arc::clone(&engine);
    let peer_device_id = hello.device_id;
    let (permit, (ok, payload, error_code)) = tokio::task::spawn_blocking(move || {
        (
            permit,
            process_operation_with_stream(
                &operation,
                streamed_payload.as_deref(),
                &worker_engine,
                peer_device_id,
            ),
        )
    })
    .await
    .map_err(|_| CoreError::InvalidState("QUIC storage worker failed".to_owned()))?;
    let _permit: OwnedSemaphorePermit = permit;
    if ok
        && recovery_response_failpoint.is_some_and(|(boundary, upload_id)| {
            take_server_recovery_response_failpoint(boundary, &upload_id)
        })
    {
        return Err(CoreError::InvalidState(
            "server recovery response failpoint".to_owned(),
        ));
    }
    let payload_digest = response_payload_digest(ok, &payload, error_code.as_deref())?;
    let mut response_nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut response_nonce);
    let mut server_hello = ServerHello {
        device_id: engine.device_id(),
        negotiated_transport_version: QUIC_TRANSPORT_VERSION,
        request_nonce: hello.nonce.clone(),
        response_nonce: URL_SAFE_NO_PAD.encode(response_nonce),
        certificate_fingerprint: certificate_fingerprint.to_owned(),
        response_digest: payload_digest,
        signature: String::new(),
    };
    server_hello.signature = engine.sign_transport_transcript_with_domain(
        TRANSPORT_SIGNATURE_DOMAIN,
        &server_hello_bytes(&server_hello)?,
    );
    let response = WireResponse {
        ok,
        payload,
        error_code,
        server_hello,
    };
    let bytes = serde_json::to_vec(&response)?;
    if bytes.len() > MAX_RESPONSE_FRAME_BYTES {
        return Err(CoreError::ResourceLimit("QUIC response frame"));
    }
    wait_for_peer_budget(&rate_limiter, hello.device_id, bytes.len(), false).await?;
    write_frame(&mut send, &bytes).await?;
    send.finish().map_err(|_| CoreError::AuthenticationFailed)
}

fn authenticate_client_hello(
    hello: &ClientHello,
    engine: &Engine,
    certificate_fingerprint: &str,
    replay_window: &Mutex<ReplayWindow>,
) -> Result<(), CoreError> {
    negotiate_transport_version(
        hello.minimum_transport_version,
        hello.maximum_transport_version,
    )?;
    if hello.expected_certificate_fingerprint != certificate_fingerprint
        || hello.operation_bytes == 0
        || hello.operation_bytes > MAX_OPERATION_FRAME_BYTES as u64
        || (matches!(hello.operation_type, OperationType::PutBatchStream)
            && (hello.streamed_payload_bytes == 0
                || hello.streamed_payload_bytes > MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES as u64
                || !is_lower_hex_digest(&hello.streamed_payload_digest)))
        || (!matches!(hello.operation_type, OperationType::PutBatchStream)
            && (hello.streamed_payload_bytes != 0 || !hello.streamed_payload_digest.is_empty()))
        || !matches!(
            URL_SAFE_NO_PAD.decode(&hello.nonce),
            Ok(nonce) if nonce.len() == 24
        )
        || current_unix_ms()?.abs_diff(hello.issued_at_unix_ms)
            > MAX_REQUEST_CLOCK_SKEW.as_millis() as u64
    {
        return Err(CoreError::ProtocolNegotiationFailed);
    }
    let peer = match hello.operation_type {
        OperationType::GetProviderCapability
        | OperationType::AcquireStorageLease
        | OperationType::CancelStorageLease
        | OperationType::Put
        | OperationType::PutBatch
        | OperationType::PutBatchStream
        | OperationType::PutRecoveryCapsule
        | OperationType::BeginRecoveryCapsuleUpload
        | OperationType::PutRecoveryCapsuleSegment
        | OperationType::CommitRecoveryCapsuleUpload
        | OperationType::AcknowledgeRecoveryCapsuleUpload
        | OperationType::QueryRecoveryCapsule => {
            engine.authorized_peer(hello.device_id, PeerRole::BackupWriter)?
        }
        OperationType::GetScoped
        | OperationType::GetBatch
        | OperationType::ContainsScoped
        | OperationType::ListRecoveryCapsules
        | OperationType::GetRecoveryCapsuleSegment => {
            engine.authorized_peer(hello.device_id, PeerRole::BackupReader)?
        }
        OperationType::GetRoster | OperationType::SubmitRoster => {
            engine.trusted_peer_identity(hello.device_id)?
        }
    };
    peer.verify(
        TRANSPORT_SIGNATURE_DOMAIN,
        &client_hello_bytes(hello)?,
        &hello.signature,
    )?;
    if !replay_window
        .lock()
        .map_err(|_| CoreError::Synchronization)?
        .insert_fresh(hello.device_id, hello.nonce.clone())
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn negotiate_transport_version(minimum: u16, maximum: u16) -> Result<u16, CoreError> {
    // Transport v3 changed mandatory operation fields and signed transcripts.
    // A peer advertising a range that includes an older framing is ambiguous:
    // accepting it would complete negotiation and fail only after deserializing
    // an operation. Require an exact version so incompatibility is rejected
    // before any authenticated storage mutation.
    if minimum != QUIC_TRANSPORT_VERSION || maximum != QUIC_TRANSPORT_VERSION {
        return Err(CoreError::ProtocolNegotiationFailed);
    }
    Ok(QUIC_TRANSPORT_VERSION)
}

fn validate_negotiated_transport_version(
    minimum: u16,
    maximum: u16,
    negotiated: u16,
) -> Result<(), CoreError> {
    if negotiated != negotiate_transport_version(minimum, maximum)? {
        return Err(CoreError::ProtocolNegotiationFailed);
    }
    Ok(())
}

#[cfg(test)]
fn process_operation(
    operation: &Operation,
    engine: &Engine,
    peer_device_id: DeviceId,
) -> (bool, ResponsePayload, Option<String>) {
    process_operation_with_stream(operation, None, engine, peer_device_id)
}

fn process_operation_with_stream(
    operation: &Operation,
    streamed_payload: Option<&[u8]>,
    engine: &Engine,
    peer_device_id: DeviceId,
) -> (bool, ResponsePayload, Option<String>) {
    let operation_started = Instant::now();
    let result = match operation {
        Operation::GetProviderCapability => current_unix_ms().and_then(|observed_at_unix_ms| {
            let valid_until_unix_ms = observed_at_unix_ms
                .checked_add(PROVIDER_CAPABILITY_FRESHNESS.as_millis() as u64)
                .ok_or(CoreError::ResourceLimit("provider capability timestamp"))?;
            let capacity = engine.store().provider_capacity(observed_at_unix_ms)?;
            Ok(ResponsePayload::ProviderCapability {
                capability: ProviderCapability {
                    schema_version: 1,
                    provider_device_id: engine.device_id(),
                    reachable: true,
                    observed_at_unix_ms,
                    valid_until_unix_ms,
                    usable_bytes: capacity.available_bytes,
                    allocated_bytes: capacity.allocated_bytes,
                    quota_bytes: capacity.quota_bytes,
                    reserved_bytes: capacity.reserved_bytes,
                    available_objects: capacity.available_objects,
                    reserved_objects: capacity.reserved_objects,
                    free_space_reserve_bytes: capacity.free_space_reserve_bytes,
                },
            })
        }),
        Operation::AcquireStorageLease {
            backup_id,
            max_new_bytes,
            max_new_objects,
            acquisition_id,
        } => current_unix_ms().and_then(|issued_at_unix_ms| {
            let expires_at_unix_ms = issued_at_unix_ms
                .checked_add(STORAGE_LEASE_LIFETIME_MS)
                .ok_or(CoreError::ResourceLimit("storage lease expiry"))?;
            engine
                .issue_storage_lease_idempotent(
                    peer_device_id,
                    *backup_id,
                    *max_new_bytes,
                    *max_new_objects,
                    issued_at_unix_ms,
                    expires_at_unix_ms,
                    acquisition_id,
                )
                .map(|lease| ResponsePayload::StorageLease { lease })
        }),
        Operation::CancelStorageLease { lease } => current_unix_ms().and_then(|now_unix_ms| {
            engine
                .cancel_storage_lease(peer_device_id, lease, now_unix_ms)
                .map(|()| ResponsePayload::Stored)
        }),
        Operation::Put {
            backup_id,
            lease,
            locator,
            record,
        } => URL_SAFE_NO_PAD
            .decode(record)
            .map_err(|_| CoreError::AuthenticationFailed)
            .and_then(|record| {
                if record.len() > MAX_PROVIDER_RECORD_BYTES {
                    return Err(CoreError::ResourceLimit("provider record"));
                }
                if lease.backup_id != *backup_id {
                    return Err(CoreError::AuthenticationFailed);
                }
                engine
                    .put_leased_provider_record(
                        peer_device_id,
                        lease,
                        locator,
                        &record,
                        current_unix_ms()?,
                    )
                    .map(|_| ResponsePayload::Stored)
            }),
        Operation::PutBatch {
            backup_id,
            lease,
            records,
        } => {
            if lease.backup_id != *backup_id
                || records.is_empty()
                || records.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS
                || records
                    .iter()
                    .map(|record| &record.locator)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != records.len()
            {
                Err(CoreError::AuthenticationFailed)
            } else {
                let mut total_bytes = 0_usize;
                let mut decoded = Vec::with_capacity(records.len());
                let mut validation_error = None;
                for record in records {
                    if operation_started.elapsed() >= STREAM_OPERATION_TIMEOUT {
                        validation_error = Some(CoreError::ResourceLimit("QUIC operation timeout"));
                        break;
                    }
                    let bytes = match URL_SAFE_NO_PAD.decode(&record.record) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            validation_error = Some(CoreError::AuthenticationFailed);
                            break;
                        }
                    };
                    total_bytes = match total_bytes.checked_add(bytes.len()) {
                        Some(total_bytes) => total_bytes,
                        None => {
                            validation_error =
                                Some(CoreError::ResourceLimit("provider write batch"));
                            break;
                        }
                    };
                    if bytes.is_empty()
                        || bytes.len() > MAX_PROVIDER_RECORD_BYTES
                        || total_bytes > MAX_PROVIDER_WRITE_BATCH_BYTES
                        || blake3::hash(&bytes).to_hex().as_str() != record.record_digest
                    {
                        validation_error = Some(CoreError::AuthenticationFailed);
                        break;
                    }
                    decoded.push((record.locator.clone(), bytes));
                }
                if let Some(error) = validation_error {
                    Err(error)
                } else {
                    current_unix_ms().and_then(|now_unix_ms| {
                        engine.store().put_provider_records_leased(
                            peer_device_id,
                            *backup_id,
                            lease,
                            &decoded,
                            now_unix_ms,
                        )?;
                        if operation_started.elapsed() >= STREAM_OPERATION_TIMEOUT {
                            return Err(CoreError::ResourceLimit("QUIC operation timeout"));
                        }
                        Ok(ResponsePayload::StoredBatch {
                            backup_id: *backup_id,
                            locators: decoded.into_iter().map(|(locator, _)| locator).collect(),
                        })
                    })
                }
            }
        }
        Operation::PutBatchStream {
            backup_id,
            lease,
            records,
        } => {
            let payload = streamed_payload.unwrap_or_default();
            if lease.backup_id != *backup_id
                || records.is_empty()
                || records.len() > MAX_PROVIDER_WRITE_BATCH_RECORDS
                || payload.is_empty()
                || payload.len() > MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES
                || records
                    .iter()
                    .map(|record| &record.locator)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != records.len()
            {
                Err(CoreError::AuthenticationFailed)
            } else {
                let mut cursor = 0_usize;
                let mut decoded = Vec::with_capacity(records.len());
                let mut validation_error = None;
                for record in records {
                    let Ok(record_bytes) = usize::try_from(record.record_bytes) else {
                        validation_error =
                            Some(CoreError::ResourceLimit("provider streamed write batch"));
                        break;
                    };
                    let Some(end) = cursor.checked_add(record_bytes) else {
                        validation_error =
                            Some(CoreError::ResourceLimit("provider streamed write batch"));
                        break;
                    };
                    let Some(bytes) = payload.get(cursor..end) else {
                        validation_error = Some(CoreError::AuthenticationFailed);
                        break;
                    };
                    if bytes.is_empty()
                        || bytes.len() > MAX_PROVIDER_RECORD_BYTES
                        || !is_lower_hex_digest(&record.record_digest)
                        || blake3::hash(bytes).to_hex().as_str() != record.record_digest
                    {
                        validation_error = Some(CoreError::AuthenticationFailed);
                        break;
                    }
                    decoded.push((record.locator.clone(), bytes));
                    cursor = end;
                }
                if cursor != payload.len() && validation_error.is_none() {
                    validation_error = Some(CoreError::AuthenticationFailed);
                }
                if let Some(error) = validation_error {
                    Err(error)
                } else {
                    current_unix_ms().and_then(|now_unix_ms| {
                        engine.store().put_provider_records_leased(
                            peer_device_id,
                            *backup_id,
                            lease,
                            &decoded,
                            now_unix_ms,
                        )?;
                        if operation_started.elapsed() >= STREAM_OPERATION_TIMEOUT {
                            return Err(CoreError::ResourceLimit("QUIC operation timeout"));
                        }
                        Ok(ResponsePayload::StoredBatch {
                            backup_id: *backup_id,
                            locators: decoded.into_iter().map(|(locator, _)| locator).collect(),
                        })
                    })
                }
            }
        }
        Operation::GetScoped { backup_id, locator } => engine
            .authorize_provider_read_batch(
                peer_device_id,
                *backup_id,
                std::slice::from_ref(locator),
            )
            .and_then(|()| engine.store().get_provider_record(locator))
            .map(|record| ResponsePayload::ScopedRecord {
                backup_id: *backup_id,
                locator: locator.clone(),
                record: URL_SAFE_NO_PAD.encode(record),
            }),
        Operation::GetBatch {
            backup_id,
            locators,
        } => {
            if locators.is_empty()
                || locators.len() > MAX_PROVIDER_READ_BATCH_RECORDS
                || locators.iter().collect::<BTreeSet<_>>().len() != locators.len()
            {
                Err(CoreError::ResourceLimit("provider read batch"))
            } else {
                engine
                    .authorize_provider_read_batch(peer_device_id, *backup_id, locators)
                    .and_then(|()| {
                        let mut total_bytes = 0_usize;
                        let mut records = Vec::with_capacity(locators.len());
                        for locator in locators {
                            if operation_started.elapsed() >= STREAM_OPERATION_TIMEOUT {
                                return Err(CoreError::ResourceLimit("QUIC operation timeout"));
                            }
                            let record = engine.store().get_provider_record(locator)?;
                            total_bytes = total_bytes
                                .checked_add(record.len())
                                .ok_or(CoreError::ResourceLimit("provider read batch"))?;
                            if total_bytes > MAX_PROVIDER_READ_BATCH_BYTES {
                                return Err(CoreError::ResourceLimit("provider read batch"));
                            }
                            records.push(WireProviderRecord {
                                locator: locator.clone(),
                                record: URL_SAFE_NO_PAD.encode(record),
                            });
                        }
                        Ok(ResponsePayload::Records {
                            backup_id: *backup_id,
                            records,
                        })
                    })
            }
        }
        Operation::ContainsScoped { backup_id, locator } => engine
            .authorize_provider_read_batch(
                peer_device_id,
                *backup_id,
                std::slice::from_ref(locator),
            )
            .and_then(|()| engine.store().contains(locator))
            .map(|present| ResponsePayload::ScopedPresence {
                backup_id: *backup_id,
                locator: locator.clone(),
                present,
            }),
        Operation::PutRecoveryCapsule {
            backup_id,
            lease,
            capsule,
        } => {
            if lease.backup_id != *backup_id || capsule.backup_id != *backup_id {
                Err(CoreError::AuthenticationFailed)
            } else {
                current_unix_ms().and_then(|now_unix_ms| {
                    engine
                        .put_leased_recovery_capsule(peer_device_id, lease, capsule, now_unix_ms)
                        .map(|_| ResponsePayload::Stored)
                })
            }
        }
        Operation::BeginRecoveryCapsuleUpload {
            backup_id,
            lease,
            upload_id,
            total_bytes,
            total_segments,
            capsule_digest,
            descriptor,
        } => {
            if lease.backup_id != *backup_id {
                Err(CoreError::AuthenticationFailed)
            } else {
                current_unix_ms().and_then(|now_unix_ms| {
                    engine
                        .begin_leased_recovery_capsule_upload(
                            peer_device_id,
                            lease,
                            upload_id,
                            *total_bytes,
                            *total_segments,
                            capsule_digest,
                            descriptor,
                            now_unix_ms,
                        )
                        .map(|()| ResponsePayload::Stored)
                })
            }
        }
        Operation::PutRecoveryCapsuleSegment {
            backup_id,
            lease,
            upload_id,
            index,
            segment,
            segment_digest,
        } => {
            if lease.backup_id != *backup_id {
                Err(CoreError::AuthenticationFailed)
            } else {
                URL_SAFE_NO_PAD
                    .decode(segment)
                    .map_err(|_| CoreError::AuthenticationFailed)
                    .and_then(|segment| {
                        if segment.len() > RECOVERY_CAPSULE_SEGMENT_BYTES {
                            return Err(CoreError::ResourceLimit("recovery capsule segment"));
                        }
                        engine
                            .put_leased_recovery_capsule_segment(
                                peer_device_id,
                                lease,
                                upload_id,
                                *index,
                                &segment,
                                segment_digest,
                                current_unix_ms()?,
                            )
                            .map(|()| ResponsePayload::Stored)
                    })
            }
        }
        Operation::CommitRecoveryCapsuleUpload {
            backup_id,
            lease,
            upload_id,
        } => {
            if lease.backup_id != *backup_id {
                Err(CoreError::AuthenticationFailed)
            } else {
                current_unix_ms().and_then(|now_unix_ms| {
                    engine
                        .store()
                        .commit_recovery_capsule_upload(
                            peer_device_id,
                            *backup_id,
                            lease,
                            upload_id,
                            now_unix_ms,
                        )
                        .map(|_| ResponsePayload::Stored)
                })
            }
        }
        Operation::AcknowledgeRecoveryCapsuleUpload { lease, upload_id } => engine
            .acknowledge_recovery_capsule_upload(peer_device_id, lease, upload_id)
            .map(|()| ResponsePayload::Stored),
        Operation::QueryRecoveryCapsule { identity } => engine
            .recovery_capsule_is_committed_for_peer(
                peer_device_id,
                identity.backup_id,
                &identity.snapshot_id,
                identity.total_bytes,
                &identity.capsule_digest,
            )
            .and_then(|committed| {
                if identity.signer_device_id != peer_device_id {
                    return Err(CoreError::AuthenticationFailed);
                }
                Ok(ResponsePayload::RecoveryCapsuleStatus {
                    identity: identity.clone(),
                    committed,
                })
            }),
        Operation::ListRecoveryCapsules {
            backup_id,
            cursor,
            limit,
        } => operation_started
            .checked_add(STREAM_OPERATION_TIMEOUT)
            .ok_or(CoreError::ResourceLimit("QUIC operation timeout"))
            .and_then(|deadline| {
                engine.recovery_capsule_descriptors_for_peer_with_deadline(
                    peer_device_id,
                    *backup_id,
                    cursor.as_deref(),
                    *limit,
                    deadline,
                )
            })
            .and_then(|(descriptors, next_cursor)| {
                if operation_started.elapsed() >= STREAM_OPERATION_TIMEOUT {
                    return Err(CoreError::ResourceLimit("QUIC operation timeout"));
                }
                if serde_json::to_vec(&descriptors)?.len()
                    > MAX_RESPONSE_FRAME_BYTES.saturating_sub(MAX_HELLO_FRAME_BYTES)
                {
                    return Err(CoreError::ResourceLimit("QUIC recovery capsule listing"));
                }
                Ok(ResponsePayload::RecoveryCapsuleDescriptors {
                    descriptors,
                    next_cursor,
                })
            }),
        Operation::GetRecoveryCapsuleSegment {
            backup_id,
            snapshot_id,
            offset,
            maximum_bytes,
        } => engine
            .recovery_capsule_segment_for_peer(
                peer_device_id,
                *backup_id,
                snapshot_id,
                *offset,
                *maximum_bytes,
            )
            .map(|(segment, total_bytes, capsule_digest)| {
                ResponsePayload::RecoveryCapsuleSegment {
                    segment: URL_SAFE_NO_PAD.encode(segment),
                    total_bytes,
                    capsule_digest,
                }
            }),
        Operation::GetRoster => engine
            .current_roster()
            .map(|roster| ResponsePayload::Roster { roster }),
        Operation::SubmitRoster { roster } => engine
            .accept_peer_roster(roster.clone())
            .map(|_| ResponsePayload::RosterAccepted),
    };
    match result {
        Ok(payload) => (true, payload, None),
        Err(error) => (
            false,
            ResponsePayload::Error,
            Some(error_code(&error).to_owned()),
        ),
    }
}

fn verify_server_response(
    request: &WireRequest,
    response: &WireResponse,
    remote_identity: &PublicIdentity,
    expected_certificate_fingerprint: &str,
) -> Result<(), CoreError> {
    validate_negotiated_transport_version(
        request.hello.minimum_transport_version,
        request.hello.maximum_transport_version,
        response.server_hello.negotiated_transport_version,
    )?;
    if response.server_hello.device_id != remote_identity.device_id
        || response.server_hello.request_nonce != request.hello.nonce
        || response.server_hello.certificate_fingerprint != expected_certificate_fingerprint
        || response.server_hello.response_digest
            != response_payload_digest(
                response.ok,
                &response.payload,
                response.error_code.as_deref(),
            )?
    {
        return Err(CoreError::IdentityMismatch);
    }
    remote_identity.verify(
        TRANSPORT_SIGNATURE_DOMAIN,
        &server_hello_bytes(&response.server_hello)?,
        &response.server_hello.signature,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyClientHelloFields<'a> {
    device_id: DeviceId,
    minimum_transport_version: u16,
    maximum_transport_version: u16,
    issued_at_unix_ms: u64,
    nonce: &'a str,
    expected_certificate_fingerprint: &'a str,
    operation_type: OperationType,
    operation_bytes: u64,
    operation_digest: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamedClientHelloFields<'a> {
    #[serde(flatten)]
    legacy: LegacyClientHelloFields<'a>,
    streamed_payload_bytes: u64,
    streamed_payload_digest: &'a str,
}

fn client_hello_bytes(hello: &ClientHello) -> Result<Vec<u8>, CoreError> {
    let legacy = LegacyClientHelloFields {
        device_id: hello.device_id,
        minimum_transport_version: hello.minimum_transport_version,
        maximum_transport_version: hello.maximum_transport_version,
        issued_at_unix_ms: hello.issued_at_unix_ms,
        nonce: &hello.nonce,
        expected_certificate_fingerprint: &hello.expected_certificate_fingerprint,
        operation_type: hello.operation_type,
        operation_bytes: hello.operation_bytes,
        operation_digest: &hello.operation_digest,
    };
    if matches!(hello.operation_type, OperationType::PutBatchStream) {
        Ok(serde_json::to_vec(&StreamedClientHelloFields {
            legacy,
            streamed_payload_bytes: hello.streamed_payload_bytes,
            streamed_payload_digest: &hello.streamed_payload_digest,
        })?)
    } else {
        // Preserve the v2 signing transcript byte-for-byte for every pre-existing operation.
        // The new streamed operation signs its additional raw-payload identity above.
        Ok(serde_json::to_vec(&legacy)?)
    }
}

fn current_unix_ms() -> Result<u64, CoreError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CoreError::InvalidState("system clock precedes Unix epoch".to_owned()))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| CoreError::ResourceLimit("system clock timestamp"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerHelloFields<'a> {
    device_id: DeviceId,
    negotiated_transport_version: u16,
    request_nonce: &'a str,
    response_nonce: &'a str,
    certificate_fingerprint: &'a str,
    response_digest: &'a str,
}

fn server_hello_bytes(hello: &ServerHello) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&ServerHelloFields {
        device_id: hello.device_id,
        negotiated_transport_version: hello.negotiated_transport_version,
        request_nonce: &hello.request_nonce,
        response_nonce: &hello.response_nonce,
        certificate_fingerprint: &hello.certificate_fingerprint,
        response_digest: &hello.response_digest,
    })?)
}

fn response_payload_digest(
    ok: bool,
    payload: &ResponsePayload,
    error_code: Option<&str>,
) -> Result<String, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Fields<'a> {
        ok: bool,
        payload: &'a ResponsePayload,
        error_code: Option<&'a str>,
    }
    Ok(blake3::hash(&serde_json::to_vec(&Fields {
        ok,
        payload,
        error_code,
    })?)
    .to_hex()
    .to_string())
}

pub(crate) fn transport_limits() -> Result<TransportConfig, CoreError> {
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(VarInt::from_u32(32));
    transport.max_concurrent_uni_streams(VarInt::from_u32(0));
    let stream_window = MAX_OPERATION_FRAME_BYTES.max(MAX_RESPONSE_FRAME_BYTES) as u32;
    transport.stream_receive_window(VarInt::from_u32(stream_window));
    transport.receive_window(VarInt::from_u32(stream_window.saturating_mul(4)));
    transport.max_idle_timeout(Some(
        Duration::from_secs(30)
            .try_into()
            .map_err(|_| CoreError::ResourceLimit("QUIC idle timeout"))?,
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(10)));
    Ok(transport)
}

fn error_code(error: &CoreError) -> &'static str {
    match error {
        CoreError::MissingChunk(_) => "missing_chunk",
        CoreError::PeerRevoked => "peer_revoked",
        CoreError::UnselectedProvider | CoreError::IdentityMismatch => "not_authorized",
        CoreError::ResourceLimit(_) => "resource_limit",
        CoreError::ProtocolNegotiationFailed => "protocol_incompatible",
        CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => "authentication_failed",
        _ => "provider_error",
    }
}

fn validate_identity_file(metadata: &fs::Metadata, maximum: u64) -> Result<(), CoreError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > maximum
    {
        return Err(CoreError::InvalidState(
            "invalid QUIC identity file".to_owned(),
        ));
    }
    Ok(())
}

fn validate_private_identity_file(metadata: &fs::Metadata, maximum: u64) -> Result<(), CoreError> {
    validate_identity_file(metadata, maximum)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CoreError::InvalidState(
                "QUIC private identity permissions are too broad".to_owned(),
            ));
        }
    }
    Ok(())
}

fn persist_private_replace(path: &Path, bytes: &[u8], private: bool) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("TLS identity path has no parent".to_owned()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "stage protected QUIC identity",
            path: path.to_path_buf(),
            source,
        })?;
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|source| CoreError::Io {
                operation: "protect QUIC identity file",
                path: path.to_path_buf(),
                source,
            })?;
    }
    use std::io::Write as _;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync protected QUIC identity",
            path: path.to_path_buf(),
            source,
        })?;
    temporary.persist(path).map_err(|error| CoreError::Io {
        operation: "commit protected QUIC identity",
        path: path.to_path_buf(),
        source: error.error,
    })?;
    sync_directory(parent)
}

fn sync_directory(path: &Path) -> Result<(), CoreError> {
    #[cfg(unix)]
    {
        fs::File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| CoreError::Io {
                operation: "sync QUIC identity directory",
                path: path.to_path_buf(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use covalent_core::{
        BackupKey, EngineOptions, KeyProtector, ProviderQuotaPolicy, StaticKeyProtector,
    };
    use covalent_protocol::{PeerGrant, PeerRole};
    use tempfile::tempdir;

    use super::*;

    fn test_protector() -> Arc<dyn KeyProtector> {
        Arc::new(StaticKeyProtector::new(1, [0xc1; 32]).expect("test protector"))
    }

    fn test_options(path: &Path) -> EngineOptions {
        EngineOptions::new(path).with_key_protector(test_protector())
    }

    fn test_tls(root: &Path, directory_name: &str) -> TlsIdentity {
        let protector = test_protector();
        TlsIdentity::load_or_create(root.join(directory_name), root, protector.as_ref())
            .expect("TLS")
    }

    fn trust_all(local: &Engine, remote: &Engine) {
        local
            .trust_peer(PeerGrant {
                peer_device_id: remote.device_id(),
                public_key: remote.public_identity().public_key,
                display_name: "Peer".to_owned(),
                roles: BTreeSet::from([
                    PeerRole::StorageProvider,
                    PeerRole::BackupReader,
                    PeerRole::BackupWriter,
                ]),
                confirmed_at_unix_ms: 1,
                revoked: false,
            })
            .expect("trust peer");
    }

    fn regular_files_below(path: &Path) -> usize {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Ok(metadata) if metadata.is_file() => 1,
            Ok(metadata) if metadata.is_dir() => fs::read_dir(path)
                .expect("read test directory")
                .map(|entry| regular_files_below(&entry.expect("test entry").path()))
                .sum(),
            Ok(_) => 0,
            Err(error) => panic!("inspect test directory: {error}"),
        }
    }

    #[test]
    fn provider_read_batch_rejects_partial_reordered_wrong_scope_and_oversized_responses() {
        let backup_id = BackupId::new();
        let locators = vec!["1".repeat(64), "2".repeat(64)];
        let record = |locator: &str, bytes: &[u8]| WireProviderRecord {
            locator: locator.to_owned(),
            record: URL_SAFE_NO_PAD.encode(bytes),
        };
        let payload = |backup_id, records| ResponsePayload::Records { backup_id, records };

        assert!(matches!(
            decode_provider_read_batch(
                payload(backup_id, vec![record(&locators[0], b"first")]),
                backup_id,
                &locators,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            decode_provider_read_batch(
                payload(
                    backup_id,
                    vec![
                        record(&locators[1], b"second"),
                        record(&locators[0], b"first"),
                    ],
                ),
                backup_id,
                &locators,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            decode_provider_read_batch(
                payload(
                    BackupId::new(),
                    vec![
                        record(&locators[0], b"first"),
                        record(&locators[1], b"second"),
                    ],
                ),
                backup_id,
                &locators,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        let half = MAX_PROVIDER_READ_BATCH_BYTES / 2 + 1;
        let oversized_record = vec![0_u8; half];
        assert!(matches!(
            decode_provider_read_batch(
                payload(
                    backup_id,
                    vec![
                        record(&locators[0], &oversized_record),
                        record(&locators[1], &oversized_record),
                    ],
                ),
                backup_id,
                &locators,
            ),
            Err(CoreError::ResourceLimit("provider read batch"))
        ));
    }

    #[test]
    fn provider_write_batch_rejects_partial_reordered_and_wrong_scope_acknowledgements() {
        let backup_id = BackupId::new();
        let records = vec![
            ("1".repeat(64), b"first".to_vec()),
            ("2".repeat(64), b"second".to_vec()),
        ];
        let payload = |backup_id, locators| ResponsePayload::StoredBatch {
            backup_id,
            locators,
        };
        assert!(matches!(
            decode_provider_write_batch_ack(
                payload(backup_id, vec![records[0].0.clone()]),
                backup_id,
                &records,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            decode_provider_write_batch_ack(
                payload(backup_id, vec![records[1].0.clone(), records[0].0.clone()],),
                backup_id,
                &records,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            decode_provider_write_batch_ack(
                payload(
                    BackupId::new(),
                    records.iter().map(|(locator, _)| locator.clone()).collect(),
                ),
                backup_id,
                &records,
            ),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn provider_write_batch_tamper_and_cross_scope_fail_before_partial_commit() {
        let owner_data = tempdir().expect("owner");
        let provider_data = tempdir().expect("provider");
        let owner = Engine::open(test_options(owner_data.path())).expect("owner");
        let provider = Engine::open(test_options(provider_data.path())).expect("provider");
        trust_all(&provider, &owner);
        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let chunks = [
            b"first batch record".as_slice(),
            b"second batch record".as_slice(),
        ]
        .into_iter()
        .map(|plaintext| key.encrypt_chunk(backup_id, 1, plaintext).expect("encrypt"))
        .collect::<Vec<_>>();
        let now = current_unix_ms().expect("time");
        let records = chunks
            .iter()
            .map(|chunk| chunk.encode_provider_record())
            .collect::<Vec<_>>();
        let lease = provider
            .issue_storage_lease(
                owner.device_id(),
                backup_id,
                records.iter().map(|record| record.len() as u64).sum(),
                records.len() as u64,
                now,
                now + 60_000,
            )
            .expect("lease");
        let wire_records = records
            .iter()
            .zip(&chunks)
            .map(|(record, chunk)| WireProviderWriteRecord {
                locator: chunk.opaque_locator.clone(),
                record: URL_SAFE_NO_PAD.encode(record),
                record_digest: blake3::hash(record).to_hex().to_string(),
            })
            .collect::<Vec<_>>();

        let mut tampered = wire_records.clone();
        tampered[1].record_digest = "0".repeat(64);
        let (ok, _, code) = process_operation(
            &Operation::PutBatch {
                backup_id,
                lease: lease.clone(),
                records: tampered,
            },
            &provider,
            owner.device_id(),
        );
        assert!(!ok);
        assert_eq!(code.as_deref(), Some("authentication_failed"));
        assert!(chunks.iter().all(|chunk| {
            !provider
                .store()
                .contains(&chunk.opaque_locator)
                .expect("contains")
        }));

        let payload = records.concat();
        let mut streamed_metadata = records
            .iter()
            .zip(&chunks)
            .map(|(record, chunk)| WireProviderWriteMetadata {
                locator: chunk.opaque_locator.clone(),
                record_bytes: record.len() as u64,
                record_digest: blake3::hash(record).to_hex().to_string(),
            })
            .collect::<Vec<_>>();
        streamed_metadata[1].record_digest = "0".repeat(64);
        let (ok, _, code) = process_operation_with_stream(
            &Operation::PutBatchStream {
                backup_id,
                lease: lease.clone(),
                records: streamed_metadata,
            },
            Some(&payload),
            &provider,
            owner.device_id(),
        );
        assert!(!ok);
        assert_eq!(code.as_deref(), Some("authentication_failed"));
        assert!(chunks.iter().all(|chunk| {
            !provider
                .store()
                .contains(&chunk.opaque_locator)
                .expect("contains")
        }));

        let streamed_metadata = records
            .iter()
            .zip(&chunks)
            .map(|(record, chunk)| WireProviderWriteMetadata {
                locator: chunk.opaque_locator.clone(),
                record_bytes: record.len() as u64,
                record_digest: blake3::hash(record).to_hex().to_string(),
            })
            .collect::<Vec<_>>();
        let (ok, _, code) = process_operation_with_stream(
            &Operation::PutBatchStream {
                backup_id,
                lease: lease.clone(),
                records: streamed_metadata,
            },
            Some(&payload[..payload.len() - 1]),
            &provider,
            owner.device_id(),
        );
        assert!(!ok);
        assert_eq!(code.as_deref(), Some("authentication_failed"));
        assert!(chunks.iter().all(|chunk| {
            !provider
                .store()
                .contains(&chunk.opaque_locator)
                .expect("contains")
        }));

        let (ok, _, code) = process_operation(
            &Operation::PutBatch {
                backup_id: BackupId::new(),
                lease,
                records: wire_records,
            },
            &provider,
            owner.device_id(),
        );
        assert!(!ok);
        assert_eq!(code.as_deref(), Some("authentication_failed"));
        assert!(chunks.iter().all(|chunk| {
            !provider
                .store()
                .contains(&chunk.opaque_locator)
                .expect("contains")
        }));
    }

    #[test]
    fn transport_lease_acquisition_is_bounded_and_cancel_releases_a_slot() {
        let owner_data = tempdir().expect("owner");
        let provider_data = tempdir().expect("provider");
        let owner = Engine::open(test_options(owner_data.path())).expect("owner");
        let provider = Engine::open(test_options(provider_data.path())).expect("provider");
        trust_all(&provider, &owner);
        let operation = || Operation::AcquireStorageLease {
            backup_id: BackupId::new(),
            max_new_bytes: 1,
            max_new_objects: 1,
            acquisition_id: uuid::Uuid::new_v4().to_string(),
        };
        let mut first = None;
        for index in 0..32 {
            let (ok, payload, code) = process_operation(&operation(), &provider, owner.device_id());
            assert!(ok, "lease {index} failed with {code:?}");
            let ResponsePayload::StorageLease { lease } = payload else {
                panic!("unexpected lease response");
            };
            if index == 0 {
                first = Some(lease);
            }
        }
        let (ok, _, code) = process_operation(&operation(), &provider, owner.device_id());
        assert!(!ok);
        assert_eq!(code.as_deref(), Some("resource_limit"));
        let first = first.expect("first lease");
        let (ok, payload, code) = process_operation(
            &Operation::CancelStorageLease {
                lease: first.clone(),
            },
            &provider,
            owner.device_id(),
        );
        assert!(ok, "cancel failed with {code:?}");
        assert!(matches!(payload, ResponsePayload::Stored));
        let (ok, payload, code) = process_operation(
            &Operation::CancelStorageLease { lease: first },
            &provider,
            owner.device_id(),
        );
        assert!(ok, "idempotent cancel failed with {code:?}");
        assert!(matches!(payload, ResponsePayload::Stored));
        let (ok, _, code) = process_operation(&operation(), &provider, owner.device_id());
        assert!(ok, "replacement lease failed with {code:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ordinary_write_lease_response_loss_reconciles_exactly_after_client_recreation() {
        let owner_data = tempdir().expect("owner");
        let provider_data = tempdir().expect("provider");
        let owner = Arc::new(Engine::open(test_options(owner_data.path())).expect("owner"));
        let mut remote_options = test_options(provider_data.path());
        remote_options.provider_quota_policy = ProviderQuotaPolicy {
            maximum_total_bytes: 4_096,
            maximum_peer_bytes: 4_096,
            maximum_backup_bytes: 4_096,
            maximum_total_objects: 2,
            maximum_peer_objects: 2,
            maximum_backup_objects: 2,
            free_space_reserve_bytes: 0,
            maximum_lease_lifetime_ms: 15 * 60 * 1_000,
        };
        let remote = Arc::new(Engine::open(remote_options).expect("provider"));
        trust_all(&owner, &remote);
        trust_all(&remote, &owner);
        let tls = test_tls(provider_data.path(), "tls");
        let node = QuicNode::bind(
            "127.0.0.1:0".parse().expect("address"),
            Arc::clone(&remote),
            &tls,
        )
        .expect("node");
        let address = node.local_addr().expect("local address");
        let task = tokio::spawn(node.run());
        let provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&owner),
        )
        .expect("provider");
        let backup_id = BackupId::new();
        let interrupted_intent = ProviderWriteLeaseIntent::new(
            remote.device_id(),
            backup_id,
            4_096,
            2,
            uuid::Uuid::new_v4().to_string(),
        );
        owner
            .store()
            .persist_provider_write_lease_intent(&interrupted_intent)
            .expect("persist acquisition before request");
        arm_server_recovery_response_failpoint(3, &interrupted_intent.acquisition_id);
        let attempted_intent = interrupted_intent.clone();
        let attempted_provider = provider.clone();
        let result = tokio::task::spawn_blocking(move || {
            attempted_provider.acquire_storage_lease_for_write_intent(&attempted_intent)
        })
        .await
        .expect("interrupted worker");
        assert!(result.is_err(), "the acquired lease response must be lost");
        assert_eq!(provider.metrics().requests, 1);
        assert_eq!(
            owner
                .store()
                .load_provider_write_lease_intent(remote.device_id(), backup_id)
                .expect("retained intent"),
            Some(interrupted_intent.clone())
        );
        let interrupted_capacity = remote
            .store()
            .provider_capacity(current_unix_ms().expect("time"))
            .expect("interrupted capacity");
        assert_eq!(interrupted_capacity.reserved_bytes, 4_096);
        assert_eq!(interrupted_capacity.reserved_objects, 2);
        drop(provider);
        drop(owner);

        let reopened_owner =
            Arc::new(Engine::open(test_options(owner_data.path())).expect("reopen owner"));
        let recreated_provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&reopened_owner),
        )
        .expect("recreated provider");
        let requests = tokio::task::spawn_blocking({
            let provider = recreated_provider.clone();
            move || {
                let before = provider.metrics().requests;
                provider
                    .begin_backup_write(backup_id, 4_096, 2)
                    .expect("reconcile old reservation before new acquisition");
                provider.metrics().requests - before
            }
        })
        .await
        .expect("recreated worker");
        assert_eq!(
            requests, 3,
            "recreation must reacquire and cancel the exact old lease before acquiring a new one"
        );
        let replacement_intent = reopened_owner
            .store()
            .load_provider_write_lease_intent(remote.device_id(), backup_id)
            .expect("replacement intent")
            .expect("active replacement intent");
        assert_ne!(
            replacement_intent.acquisition_id,
            interrupted_intent.acquisition_id
        );
        assert_eq!(replacement_intent.maximum_new_bytes, 4_096);
        assert_eq!(replacement_intent.maximum_new_objects, 2);
        let replacement_capacity = remote
            .store()
            .provider_capacity(current_unix_ms().expect("time"))
            .expect("replacement capacity");
        assert_eq!(replacement_capacity.reserved_bytes, 4_096);
        assert_eq!(replacement_capacity.reserved_objects, 2);

        tokio::task::spawn_blocking({
            let provider = recreated_provider.clone();
            move || provider.finish_backup_write(backup_id)
        })
        .await
        .expect("finish worker")
        .expect("finish backup write");
        assert!(
            reopened_owner
                .store()
                .load_provider_write_lease_intent(remote.device_id(), backup_id)
                .expect("completed intent")
                .is_none()
        );
        let completed_capacity = remote
            .store()
            .provider_capacity(current_unix_ms().expect("time"))
            .expect("completed capacity");
        assert_eq!(completed_capacity.reserved_bytes, 0);
        assert_eq!(completed_capacity.reserved_objects, 0);
        assert_eq!(
            regular_files_below(&owner_data.path().join("store/provider-write-intents")),
            0
        );
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_quic_provider_round_trip_and_pin_rejection() {
        let first_data = tempdir().expect("first");
        let second_data = tempdir().expect("second");
        let first = Arc::new(Engine::open(test_options(first_data.path())).expect("first"));
        let second = Arc::new(Engine::open(test_options(second_data.path())).expect("second"));
        trust_all(&first, &second);
        trust_all(&second, &first);
        let tls = test_tls(second_data.path(), "tls");
        let node = QuicNode::bind(
            "127.0.0.1:0".parse().expect("address"),
            Arc::clone(&second),
            &tls,
        )
        .expect("node");
        let address = node.local_addr().expect("local address");
        let task = tokio::spawn(node.run());
        let provider = QuicProvider::new(
            address,
            second.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&first),
        )
        .expect("provider");
        let key = BackupKey::generate();
        let backup_id = BackupId::new();
        let chunk = key
            .encrypt_chunk(backup_id, 1, b"over QUIC")
            .expect("chunk");
        let locator = chunk.opaque_locator.clone();
        let record = chunk.encode_provider_record();
        let local_roster = first
            .current_roster()
            .expect("current roster")
            .expect("issued roster");
        let cross_peer_locator = locator.clone();
        tokio::task::spawn_blocking(move || {
            assert!(provider.put(&locator, &record).is_err());
            provider
                .begin_backup_write(backup_id, record.len() as u64, 1)
                .expect("lease preflight");
            provider
                .put_scoped(backup_id, &locator, &record)
                .expect("leased put");
            assert!(
                provider
                    .contains_scoped(backup_id, &locator)
                    .expect("contains")
            );
            assert_eq!(
                provider.get_scoped(backup_id, &locator).expect("get"),
                record
            );
            let capability = provider.probe_capability().expect("provider capability");
            assert_eq!(capability.provider_device_id, provider.device_id());
            assert!(capability.reachable);
            assert_eq!(
                capability.quota_bytes,
                capability
                    .usable_bytes
                    .saturating_add(capability.allocated_bytes)
                    .saturating_add(capability.reserved_bytes)
            );
            let metrics = provider.metrics();
            assert!(metrics.requests >= 5);
            assert_eq!(metrics.failures, 0);
            assert_eq!(metrics.requests, metrics.successes + metrics.failures);
            assert!(metrics.request_bytes > 0);
            assert!(metrics.response_bytes > 0);
            assert!(metrics.last_success_unix_ms.is_some());
            assert!(provider.contains(&locator).is_err());
            assert!(provider.get(&locator).is_err());
            let other_backup = BackupId::new();
            assert!(matches!(
                provider.contains_scoped(other_backup, &locator),
                Err(CoreError::AuthenticationFailed)
            ));
            assert!(matches!(
                provider.get_scoped(other_backup, &locator),
                Err(CoreError::AuthenticationFailed)
            ));
            let remote_roster = provider
                .fetch_roster()
                .expect("fetch roster")
                .expect("remote roster");
            assert_eq!(remote_roster.signer_device_id, provider.device_id());
            provider
                .submit_roster(local_roster.clone())
                .expect("submit roster");
            assert!(provider.submit_roster(local_roster).is_err());
        })
        .await
        .expect("worker");

        let other_peer_data = tempdir().expect("other peer");
        let other_peer =
            Arc::new(Engine::open(test_options(other_peer_data.path())).expect("other peer"));
        trust_all(&second, &other_peer);
        trust_all(&other_peer, &second);
        let cross_peer_provider = QuicProvider::new(
            address,
            second.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&other_peer),
        )
        .expect("cross-peer provider");
        tokio::task::spawn_blocking(move || {
            assert!(matches!(
                cross_peer_provider.contains_scoped(backup_id, &cross_peer_locator),
                Err(CoreError::AuthenticationFailed)
            ));
            assert!(matches!(
                cross_peer_provider.get_scoped(backup_id, &cross_peer_locator),
                Err(CoreError::AuthenticationFailed)
            ));
        })
        .await
        .expect("cross-peer worker");

        let wrong_tls = test_tls(first_data.path(), "other-tls");
        assert_ne!(
            tls.certificate_fingerprint(),
            wrong_tls.certificate_fingerprint()
        );
        let wrong_pin_provider = QuicProvider::new(
            address,
            second.public_identity(),
            wrong_tls.certificate_der().to_vec(),
            Arc::clone(&first),
        )
        .expect("wrong-pin provider");
        tokio::task::spawn_blocking(move || {
            assert!(wrong_pin_provider.contains(&"0".repeat(64)).is_err());
        })
        .await
        .expect("wrong-pin worker");
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn segmented_recovery_capsule_exceeding_frame_round_trips_tenant_scoped() {
        let owner_data = tempdir().expect("owner");
        let provider_data = tempdir().expect("provider");
        let owner = Arc::new(Engine::open(test_options(owner_data.path())).expect("owner"));
        let remote = Arc::new(Engine::open(test_options(provider_data.path())).expect("provider"));
        trust_all(&owner, &remote);
        trust_all(&remote, &owner);
        let tls = test_tls(provider_data.path(), "tls");
        let node = QuicNode::bind(
            "127.0.0.1:0".parse().expect("address"),
            Arc::clone(&remote),
            &tls,
        )
        .expect("node");
        let address = node.local_addr().expect("local address");
        let task = tokio::spawn(node.run());
        let provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&owner),
        )
        .expect("provider");
        let backup_id = BackupId::new();
        let capsule = RecoveryCapsule {
            schema_version: 1,
            cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
            backup_id,
            snapshot_id: "large-capsule".to_owned(),
            key_epoch: 1,
            committed_at_unix_ms: 1,
            nonce: "opaque".to_owned(),
            ciphertext: "A".repeat(13 * 1_024 * 1_024),
            signer_device_id: owner.device_id(),
            signature: "opaque".to_owned(),
        };
        let expected = capsule.clone();
        tokio::task::spawn_blocking(move || {
            provider
                .put_recovery_capsule_scoped(backup_id, &capsule)
                .expect("segmented capsule put");
            provider
                .put_recovery_capsule_scoped(backup_id, &capsule)
                .expect("duplicate segmented capsule put");
            assert_eq!(
                provider.list_recovery_capsules().expect("streamed list"),
                vec![expected]
            );
        })
        .await
        .expect("worker");
        let capacity = remote
            .store()
            .provider_capacity(current_unix_ms().expect("time"))
            .expect("provider capacity");
        assert_eq!(capacity.reserved_bytes, 0);
        assert_eq!(capacity.reserved_objects, 0);
        assert_eq!(
            regular_files_below(&provider_data.path().join("provider-capsule-uploads")),
            0
        );
        assert_eq!(
            regular_files_below(&provider_data.path().join("provider-upload-receipts")),
            0
        );
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn segmented_recovery_failures_cancel_exact_lease_and_retry_without_growth() {
        let owner_data = tempdir().expect("owner");
        let provider_data = tempdir().expect("provider");
        let owner = Arc::new(Engine::open(test_options(owner_data.path())).expect("owner"));
        let remote = Arc::new(Engine::open(test_options(provider_data.path())).expect("provider"));
        trust_all(&owner, &remote);
        trust_all(&remote, &owner);
        let tls = test_tls(provider_data.path(), "tls");
        let node = QuicNode::bind(
            "127.0.0.1:0".parse().expect("address"),
            Arc::clone(&remote),
            &tls,
        )
        .expect("node");
        let address = node.local_addr().expect("local address");
        let task = tokio::spawn(node.run());
        let provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&owner),
        )
        .expect("provider");

        for boundary in 1_u8..=4 {
            let backup_id = BackupId::new();
            let capsule = RecoveryCapsule {
                schema_version: 1,
                cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
                backup_id,
                snapshot_id: format!("failure-boundary-{boundary}"),
                key_epoch: 1,
                committed_at_unix_ms: u64::from(boundary),
                nonce: "opaque".to_owned(),
                ciphertext: "A".repeat(5 * 1_024 * 1_024),
                signer_device_id: owner.device_id(),
                signature: "opaque".to_owned(),
            };
            let total_bytes = serde_json::to_vec(&capsule).expect("capsule bytes").len() as u64;
            let provider = provider.clone();
            let remote = Arc::clone(&remote);
            let provider_root = provider_data.path().to_path_buf();
            tokio::task::spawn_blocking(move || {
                let allocated_before_attempt = remote
                    .store()
                    .provider_capacity(current_unix_ms().expect("time"))
                    .expect("capacity before failure")
                    .allocated_bytes;
                let requests_before_attempt = provider.metrics().requests;
                let operations_before_attempt = provider.operation_trace().len();
                RECOVERY_CAPSULE_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
                let result = provider.put_recovery_capsule_scoped(backup_id, &capsule);
                if boundary == 4 {
                    result.expect("lost commit response reconciles as success");
                } else {
                    assert!(matches!(
                        result,
                        Err(CoreError::InvalidState(message))
                            if message == format!("recovery capsule upload failpoint {boundary}")
                    ));
                }
                assert_eq!(
                    provider.metrics().requests - requests_before_attempt,
                    match boundary {
                        1 => 3,
                        2 => 4,
                        3 => 5,
                        4 => 9,
                        _ => unreachable!(),
                    },
                    "one committed-state probe plus the exact lease/upload/cleanup sequence"
                );
                let expected_operations = match boundary {
                    1 => vec![
                        OperationType::QueryRecoveryCapsule,
                        OperationType::AcquireStorageLease,
                        OperationType::CancelStorageLease,
                    ],
                    2 => vec![
                        OperationType::QueryRecoveryCapsule,
                        OperationType::AcquireStorageLease,
                        OperationType::BeginRecoveryCapsuleUpload,
                        OperationType::CancelStorageLease,
                    ],
                    3 => vec![
                        OperationType::QueryRecoveryCapsule,
                        OperationType::AcquireStorageLease,
                        OperationType::BeginRecoveryCapsuleUpload,
                        OperationType::PutRecoveryCapsuleSegment,
                        OperationType::CancelStorageLease,
                    ],
                    4 => vec![
                        OperationType::QueryRecoveryCapsule,
                        OperationType::AcquireStorageLease,
                        OperationType::BeginRecoveryCapsuleUpload,
                        OperationType::PutRecoveryCapsuleSegment,
                        OperationType::PutRecoveryCapsuleSegment,
                        OperationType::CommitRecoveryCapsuleUpload,
                        OperationType::CommitRecoveryCapsuleUpload,
                        OperationType::CancelStorageLease,
                        OperationType::AcknowledgeRecoveryCapsuleUpload,
                    ],
                    _ => unreachable!(),
                };
                assert_eq!(
                    &provider.operation_trace()[operations_before_attempt..],
                    expected_operations,
                    "reconciliation must not acquire a second lease or reupload bytes"
                );
                let after_failure = remote
                    .store()
                    .provider_capacity(current_unix_ms().expect("time"))
                    .expect("capacity after failure");
                assert_eq!(after_failure.reserved_bytes, 0);
                assert_eq!(after_failure.reserved_objects, 0);
                assert_eq!(
                    regular_files_below(&provider_root.join("provider-capsule-uploads")),
                    0
                );
                assert_eq!(
                    regular_files_below(&provider_root.join("provider-upload-receipts")),
                    0
                );
                if boundary == 4 {
                    assert_eq!(
                        after_failure.allocated_bytes,
                        allocated_before_attempt + total_bytes
                    );
                    assert!(
                        remote
                            .store()
                            .list_recovery_capsules()
                            .expect("capsules")
                            .contains(&capsule)
                    );
                    return;
                }
                let allocated_before_retry = after_failure.allocated_bytes;
                provider
                    .put_recovery_capsule_scoped(backup_id, &capsule)
                    .expect("immediate retry");
                let after_retry = remote
                    .store()
                    .provider_capacity(current_unix_ms().expect("time"))
                    .expect("capacity after retry");
                assert_eq!(after_retry.reserved_bytes, 0);
                assert_eq!(after_retry.reserved_objects, 0);
                assert_eq!(
                    after_retry.allocated_bytes,
                    allocated_before_retry + total_bytes
                );
                provider
                    .put_recovery_capsule_scoped(backup_id, &capsule)
                    .expect("idempotent duplicate retry");
                let after_duplicate = remote
                    .store()
                    .provider_capacity(current_unix_ms().expect("time"))
                    .expect("capacity after duplicate");
                assert_eq!(after_duplicate.allocated_bytes, after_retry.allocated_bytes);
                assert_eq!(after_duplicate.reserved_bytes, 0);
                assert_eq!(after_duplicate.reserved_objects, 0);
                assert_eq!(
                    regular_files_below(&provider_root.join("provider-capsule-uploads")),
                    0
                );
                assert_eq!(
                    regular_files_below(&provider_root.join("provider-upload-receipts")),
                    0
                );
            })
            .await
            .expect("worker");
        }
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recreated_client_reconciles_commit_and_ack_boundaries_without_reupload() {
        for boundary in 5_u8..=7 {
            let owner_data = tempdir().expect("owner");
            let provider_data = tempdir().expect("provider");
            let owner = Arc::new(Engine::open(test_options(owner_data.path())).expect("owner"));
            let backup_id = BackupId::new();
            let capsule = RecoveryCapsule {
                schema_version: 1,
                cipher_suite: "XCHACHA20-POLY1305-HKDF-SHA256".to_owned(),
                backup_id,
                snapshot_id: format!("restart-boundary-{boundary}"),
                key_epoch: 1,
                committed_at_unix_ms: u64::from(boundary),
                nonce: "opaque".to_owned(),
                ciphertext: "A".repeat(5 * 1_024 * 1_024),
                signer_device_id: owner.device_id(),
                signature: "opaque".to_owned(),
            };
            let total_bytes = serde_json::to_vec(&capsule).expect("capsule bytes").len() as u64;
            let mut remote_options = test_options(provider_data.path());
            remote_options.provider_quota_policy = ProviderQuotaPolicy {
                maximum_total_bytes: total_bytes,
                maximum_peer_bytes: total_bytes,
                maximum_backup_bytes: total_bytes,
                maximum_total_objects: 1,
                maximum_peer_objects: 1,
                maximum_backup_objects: 1,
                free_space_reserve_bytes: 0,
                maximum_lease_lifetime_ms: 15 * 60 * 1_000,
            };
            let remote = Arc::new(Engine::open(remote_options).expect("provider"));
            trust_all(&owner, &remote);
            trust_all(&remote, &owner);
            let tls = test_tls(provider_data.path(), "tls");
            let node = QuicNode::bind(
                "127.0.0.1:0".parse().expect("address"),
                Arc::clone(&remote),
                &tls,
            )
            .expect("node");
            let address = node.local_addr().expect("local address");
            let task = tokio::spawn(node.run());
            let provider = QuicProvider::new(
                address,
                remote.public_identity(),
                tls.certificate_der().to_vec(),
                Arc::clone(&owner),
            )
            .expect("provider");
            let interrupted_capsule = capsule.clone();
            let result = tokio::task::spawn_blocking(move || {
                RECOVERY_CAPSULE_UPLOAD_FAILPOINT.with(|armed| armed.set(boundary));
                provider.put_recovery_capsule_scoped(backup_id, &interrupted_capsule)
            })
            .await
            .expect("interrupted worker");
            assert!(matches!(
                result,
                Err(CoreError::InvalidState(message))
                    if message == format!("recovery capsule upload failpoint {boundary}")
            ));
            assert_eq!(
                regular_files_below(&owner_data.path().join("store/recovery-upload-attempts")),
                1,
                "the exact lease/upload identity must survive process loss"
            );
            let interrupted_capacity = remote
                .store()
                .provider_capacity(current_unix_ms().expect("time"))
                .expect("interrupted capacity");
            assert_eq!(interrupted_capacity.allocated_bytes, total_bytes);
            assert_eq!(interrupted_capacity.reserved_bytes, 0);
            drop(owner);

            let reopened_owner =
                Arc::new(Engine::open(test_options(owner_data.path())).expect("reopen owner"));
            let provider = QuicProvider::new(
                address,
                remote.public_identity(),
                tls.certificate_der().to_vec(),
                Arc::clone(&reopened_owner),
            )
            .expect("recreated provider");
            let expected = capsule.clone();
            let requests = tokio::task::spawn_blocking(move || {
                let before = provider.metrics().requests;
                provider
                    .put_recovery_capsule_scoped(backup_id, &capsule)
                    .expect("exact restart reconciliation");
                provider.metrics().requests - before
            })
            .await
            .expect("reconciliation worker");
            assert!(
                requests <= 3,
                "restart may send only exact commit/cancel/ack, never acquire/begin/segments"
            );
            assert_eq!(
                regular_files_below(&owner_data.path().join("store/recovery-upload-attempts")),
                0
            );
            assert_eq!(
                regular_files_below(&provider_data.path().join("provider-upload-receipts")),
                0
            );
            assert_eq!(
                regular_files_below(&provider_data.path().join("provider-capsule-uploads")),
                0
            );
            let recovered_capacity = remote
                .store()
                .provider_capacity(current_unix_ms().expect("time"))
                .expect("recovered capacity");
            assert_eq!(recovered_capacity.allocated_bytes, total_bytes);
            assert_eq!(recovered_capacity.available_bytes, 0);
            assert_eq!(recovered_capacity.reserved_bytes, 0);
            assert_eq!(
                remote.store().list_recovery_capsules().expect("capsule"),
                vec![expected]
            );
            task.abort();
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn controlled_read_cancels_client_stream_and_server_tail_within_five_seconds() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(test_options(local_data.path())).expect("local"));
        let remote = Arc::new(Engine::open(test_options(remote_data.path())).expect("remote"));
        let tls = test_tls(remote_data.path(), "tls");
        let endpoint = Endpoint::server(
            tls.server_config().expect("server config"),
            "127.0.0.1:0".parse().expect("address"),
        )
        .expect("endpoint");
        let address = endpoint.local_addr().expect("local address");
        let (request_seen_send, request_seen_receive) = tokio::sync::oneshot::channel();
        let (tail_send, tail_receive) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming");
            let connection = incoming.await.expect("connection");
            let (send, mut receive) = connection.accept_bi().await.expect("stream");
            read_frame(&mut receive, MAX_HELLO_FRAME_BYTES)
                .await
                .expect("hello");
            read_frame(&mut receive, MAX_OPERATION_FRAME_BYTES)
                .await
                .expect("operation");
            request_seen_send.send(()).ok();
            let stopped = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
            tail_send.send(stopped.is_ok()).ok();
        });
        let provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&local),
        )
        .expect("provider");
        let control = JobControl::new();
        let mut worker = tokio::task::spawn_blocking({
            let provider = provider.clone();
            let control = control.clone();
            move || provider.get_many_controlled(BackupId::new(), &["0".repeat(64)], &control)
        });
        tokio::select! {
            seen = request_seen_receive => seen.expect("request signal"),
            result = &mut worker => panic!("client completed before request reached server: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(5)) => panic!("request did not reach server within five seconds"),
        }
        let started = Instant::now();
        control.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .expect("client cancellation deadline")
            .expect("client worker");
        assert!(matches!(result, Err(CoreError::Cancelled)));
        assert!(started.elapsed() <= Duration::from_secs(5));
        assert!(
            tokio::time::timeout(Duration::from_secs(5), tail_receive)
                .await
                .expect("server tail deadline")
                .expect("server tail signal")
        );
        let metrics = provider.metrics();
        assert_eq!(metrics.requests, 1);
        assert_eq!(metrics.cancellations, 1);
        server.await.expect("server");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn controlled_streamed_write_cancels_after_payload_without_retrying() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(test_options(local_data.path())).expect("local"));
        let remote = Arc::new(Engine::open(test_options(remote_data.path())).expect("remote"));
        trust_all(&remote, &local);
        let backup_id = BackupId::new();
        let now = current_unix_ms().expect("time");
        let lease = remote
            .issue_storage_lease(
                local.device_id(),
                backup_id,
                1_024 * 1_024,
                1,
                now,
                now + 60_000,
            )
            .expect("lease");
        let tls = test_tls(remote_data.path(), "tls");
        let endpoint = Endpoint::server(
            tls.server_config().expect("server config"),
            "127.0.0.1:0".parse().expect("address"),
        )
        .expect("endpoint");
        let address = endpoint.local_addr().expect("local address");
        let (payload_seen_send, payload_seen_receive) = tokio::sync::oneshot::channel();
        let (tail_send, tail_receive) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("incoming");
            let connection = incoming.await.expect("connection");
            let (send, mut receive) = connection.accept_bi().await.expect("stream");
            read_frame(&mut receive, MAX_HELLO_FRAME_BYTES)
                .await
                .expect("hello");
            read_frame(&mut receive, MAX_OPERATION_FRAME_BYTES)
                .await
                .expect("operation");
            let payload = read_frame(&mut receive, MAX_PROVIDER_STREAM_WRITE_BATCH_BYTES)
                .await
                .expect("streamed payload");
            assert_eq!(payload.len(), 1_024 * 1_024);
            payload_seen_send.send(()).ok();
            let stopped = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
            tail_send.send(stopped.is_ok()).ok();
        });
        let provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&local),
        )
        .expect("provider");
        provider
            .write_leases
            .lock()
            .expect("lease lock")
            .insert(backup_id, lease);
        let control = JobControl::new();
        let records = vec![("a".repeat(64), vec![0x5a; 1_024 * 1_024])];
        let mut worker = tokio::task::spawn_blocking({
            let provider = provider.clone();
            let control = control.clone();
            move || provider.put_many_scoped_controlled(backup_id, &records, &control)
        });
        tokio::select! {
            seen = payload_seen_receive => seen.expect("payload signal"),
            result = &mut worker => panic!("client completed before cancellation: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(5)) => panic!("payload was not streamed within five seconds"),
        }
        control.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), worker)
            .await
            .expect("client cancellation deadline")
            .expect("client worker");
        assert!(matches!(result, Err(CoreError::Cancelled)));
        assert!(
            tokio::time::timeout(Duration::from_secs(5), tail_receive)
                .await
                .expect("server tail deadline")
                .expect("server tail signal")
        );
        let metrics = provider.metrics();
        assert_eq!(metrics.requests, 1);
        assert_eq!(metrics.successes, 0);
        assert_eq!(metrics.cancellations, 1);
        server.await.expect("server");
    }

    #[test]
    fn connected_provider_does_not_form_an_engine_reference_cycle() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(test_options(local_data.path())).expect("local"));
        let remote = Arc::new(Engine::open(test_options(remote_data.path())).expect("remote"));
        trust_all(&local, &remote);
        let tls = test_tls(remote_data.path(), "tls");
        let provider = Arc::new(
            QuicProvider::new(
                "127.0.0.1:9".parse().expect("address"),
                remote.public_identity(),
                tls.certificate_der().to_vec(),
                Arc::clone(&local),
            )
            .expect("provider"),
        ) as Arc<dyn ChunkProvider>;
        local
            .set_connected_providers(vec![provider])
            .expect("connect provider");
        let weak = Arc::downgrade(&local);
        drop(local);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn tls_identity_recovers_from_partial_legacy_creation_with_one_atomic_bundle() {
        let directory = tempdir().expect("directory");
        let tls_directory = directory.path().join("tls");
        fs::create_dir_all(&tls_directory).expect("TLS directory");
        fs::write(tls_directory.join("certificate.der"), b"interrupted")
            .expect("partial legacy certificate");

        let first = test_tls(directory.path(), "tls");
        let fingerprint = first.certificate_fingerprint();
        assert!(tls_directory.join("identity.json").is_file());
        drop(first);

        let second = test_tls(directory.path(), "tls");
        assert_eq!(second.certificate_fingerprint(), fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(tls_directory.join("identity.json"))
                    .expect("bundle metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn transport_v3_rejects_v2_and_future_framing_as_protocol_incompatible() {
        assert_eq!(covalent_protocol::PROTOCOL_VERSION, 1);
        assert_eq!(QUIC_TRANSPORT_VERSION, 3);
        assert_ne!(ALPN, b"covalent/1");

        let old_client = negotiate_transport_version(2, 2).expect_err("v2 client on v3 server");
        assert!(matches!(&old_client, CoreError::ProtocolNegotiationFailed));
        assert_eq!(error_code(&old_client), "protocol_incompatible");
        for range in [(1, 3), (2, 3)] {
            assert!(matches!(
                negotiate_transport_version(range.0, range.1),
                Err(CoreError::ProtocolNegotiationFailed)
            ));
        }

        let old_server = validate_negotiated_transport_version(
            QUIC_TRANSPORT_VERSION,
            QUIC_TRANSPORT_VERSION,
            2,
        )
        .expect_err("v3 client with v2 server selection");
        assert!(matches!(&old_server, CoreError::ProtocolNegotiationFailed));
        assert_eq!(error_code(&old_server), "protocol_incompatible");

        let future_client =
            negotiate_transport_version(4, 4).expect_err("future client on v3 server");
        assert!(matches!(
            future_client,
            CoreError::ProtocolNegotiationFailed
        ));
        assert_eq!(
            negotiate_transport_version(QUIC_TRANSPORT_VERSION, QUIC_TRANSPORT_VERSION)
                .expect("current framing"),
            QUIC_TRANSPORT_VERSION
        );
    }

    #[test]
    fn non_streamed_operations_keep_the_v3_signed_hello_transcript_shape() {
        let mut hello = ClientHello {
            device_id: DeviceId::new(),
            minimum_transport_version: QUIC_TRANSPORT_VERSION,
            maximum_transport_version: QUIC_TRANSPORT_VERSION,
            issued_at_unix_ms: 42,
            nonce: "legacy-nonce".to_owned(),
            expected_certificate_fingerprint: "legacy-fingerprint".to_owned(),
            operation_type: OperationType::GetRoster,
            operation_bytes: 128,
            operation_digest: "a".repeat(64),
            streamed_payload_bytes: 0,
            streamed_payload_digest: String::new(),
            signature: String::new(),
        };
        let expected = serde_json::to_vec(&LegacyClientHelloFields {
            device_id: hello.device_id,
            minimum_transport_version: hello.minimum_transport_version,
            maximum_transport_version: hello.maximum_transport_version,
            issued_at_unix_ms: hello.issued_at_unix_ms,
            nonce: &hello.nonce,
            expected_certificate_fingerprint: &hello.expected_certificate_fingerprint,
            operation_type: hello.operation_type,
            operation_bytes: hello.operation_bytes,
            operation_digest: &hello.operation_digest,
        })
        .expect("legacy transcript");
        assert_eq!(client_hello_bytes(&hello).expect("hello bytes"), expected);

        hello.operation_type = OperationType::PutBatchStream;
        hello.streamed_payload_bytes = 1_024;
        hello.streamed_payload_digest = "b".repeat(64);
        let streamed: serde_json::Value =
            serde_json::from_slice(&client_hello_bytes(&hello).expect("streamed hello bytes"))
                .expect("streamed transcript");
        assert_eq!(streamed["streamedPayloadBytes"], 1_024);
        assert_eq!(streamed["streamedPayloadDigest"], "b".repeat(64));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_alpn_mismatch_maps_to_explicit_protocol_error() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(test_options(local_data.path())).expect("local"));
        let remote = Arc::new(Engine::open(test_options(remote_data.path())).expect("remote"));
        let tls = test_tls(remote_data.path(), "tls");
        let endpoint = Endpoint::server(
            tls.server_config_with_alpn(b"covalent/1")
                .expect("legacy server config"),
            "127.0.0.1:0".parse().expect("address"),
        )
        .expect("legacy endpoint");
        let address = endpoint.local_addr().expect("legacy address");
        let server = tokio::spawn(async move {
            if let Some(incoming) = endpoint.accept().await {
                let _ = incoming.await;
            }
        });

        let mut provider = QuicProvider::new(
            address,
            remote.public_identity(),
            tls.certificate_der().to_vec(),
            local,
        )
        .expect("provider");
        provider.request_timeout = Duration::from_secs(2);
        let error = provider
            .connection()
            .await
            .expect_err("legacy ALPN must be rejected");
        assert!(matches!(&error, CoreError::ProtocolNegotiationFailed));
        assert_eq!(error_code(&error), "protocol_incompatible");
        server.abort();
    }

    #[test]
    fn per_peer_rate_limiter_is_bounded_and_resets() {
        let peer = DeviceId::new();
        let start = Instant::now();
        let mut limiter = PeerRateLimiter::default();
        for _ in 0..PEER_REQUEST_BURST as usize {
            assert!(limiter.admission_delay(peer, 1, true, start).is_none());
        }
        assert!(limiter.admission_delay(peer, 1, true, start).is_some());
        assert!(
            limiter
                .admission_delay(peer, 1, true, start + Duration::from_secs(1))
                .is_none()
        );

        let other = DeviceId::new();
        assert!(
            limiter
                .admission_delay(other, PEER_BYTE_BURST as usize + 1, true, start)
                .is_some()
        );
        assert!(limiter.admission_delay(other, 1_024, true, start).is_none());
    }

    #[test]
    fn ten_gibibyte_transfer_is_paced_without_a_fixed_operation_ceiling() {
        const CHUNK_BYTES: usize = 256 * 1_024;
        const WIRE_CHUNK_BYTES: usize = CHUNK_BYTES * 4 / 3 + 1_024;
        const TEN_GIBIBYTES: u64 = 10 * 1_024 * 1_024 * 1_024;
        let peer = DeviceId::new();
        let start = Instant::now();
        let mut now = start;
        let mut limiter = PeerRateLimiter::default();
        for _ in 0..TEN_GIBIBYTES / CHUNK_BYTES as u64 {
            while let Some(delay) = limiter.admission_delay(peer, WIRE_CHUNK_BYTES, true, now) {
                now += delay + Duration::from_nanos(1);
            }
        }
        assert!(now.saturating_duration_since(start) < Duration::from_secs(3 * 60));
    }

    #[test]
    fn source_connection_admission_is_bounded_and_released_by_raii() {
        let counts = Arc::new(Mutex::new(BTreeMap::new()));
        let source = "192.0.2.1".parse().expect("source IP");
        let permits = (0..MAX_CONNECTIONS_PER_SOURCE)
            .map(|_| {
                SourceConnectionPermit::try_acquire(Arc::clone(&counts), source)
                    .expect("source permit")
            })
            .collect::<Vec<_>>();
        assert!(SourceConnectionPermit::try_acquire(Arc::clone(&counts), source).is_none());
        drop(permits);
        assert!(SourceConnectionPermit::try_acquire(counts, source).is_some());
    }

    #[test]
    fn replay_nonces_are_isolated_and_bounded_per_peer() {
        let first = DeviceId::new();
        let second = DeviceId::new();
        let mut window = ReplayWindow::default();
        assert!(window.insert_fresh(first, "nonce".to_owned()));
        assert!(!window.insert_fresh(first, "nonce".to_owned()));
        assert!(window.insert_fresh(second, "nonce".to_owned()));
        for index in 0..=MAX_REPLAY_NONCES_PER_PEER {
            assert!(window.insert_fresh(first, format!("nonce-{index}")));
        }
        assert_eq!(
            window
                .peers
                .get(&first)
                .expect("first replay window")
                .members
                .len(),
            MAX_REPLAY_NONCES_PER_PEER
        );
    }
}
