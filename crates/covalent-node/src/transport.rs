//! Authenticated, pinned-certificate QUIC encrypted-object transport.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    ChunkProvider, CoreError, Engine, JobControl, JobState, ProviderHealth, PublicIdentity,
    RecoveryCapsule, RecoveryCapsuleDescriptor,
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

/// Version of the authenticated, two-frame QUIC storage transport.
///
/// This is intentionally independent from the local HTTP/archive API version.
pub const QUIC_TRANSPORT_VERSION: u16 = 2;
const ALPN: &[u8] = b"covalent-quic/2";
const TRANSPORT_SIGNATURE_DOMAIN: &[u8] = b"covalent/authenticated-quic/v2";
const TLS_ALERT_NO_APPLICATION_PROTOCOL: u8 = 0x78;
const MAX_REPLAY_NONCES_PER_PEER: usize = 4_096;
const MAX_REQUEST_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);
const MAX_PROVIDER_RECORD_BYTES: usize = 8 * 1_024 * 1_024 + 128;
const MAX_PROVIDER_READ_BATCH_RECORDS: usize = 32;
const MAX_PROVIDER_READ_BATCH_BYTES: usize = 8 * 1_024 * 1_024 + 4 * 1_024;
const JOB_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(25);
const RECOVERY_CAPSULE_SEGMENT_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_RECOVERY_CAPSULE_BYTES: u64 = 320 * 1_024 * 1_024;
const MAX_HELLO_FRAME_BYTES: usize = 8 * 1_024;
const MAX_OPERATION_FRAME_BYTES: usize = 12 * 1_024 * 1_024;
const MAX_RESPONSE_FRAME_BYTES: usize = 12 * 1_024 * 1_024;
const MAX_GLOBAL_CONNECTIONS: usize = 64;
const MAX_CONNECTIONS_PER_SOURCE: usize = 8;
const MAX_GLOBAL_STREAMS: usize = 256;
const MAX_BLOCKING_OPERATIONS: usize = 16;
const CONNECTION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const STREAM_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
const PEER_REQUEST_BURST: f64 = 4_096.0;
const PEER_REQUESTS_PER_SECOND: f64 = 256.0;
const PEER_BYTE_BURST: f64 = 512.0 * 1_024.0 * 1_024.0;
const PEER_BYTES_PER_SECOND: f64 = 256.0 * 1_024.0 * 1_024.0;
const TLS_IDENTITY_SCHEMA_VERSION: u16 = 1;

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
    private_key_der: String,
}

/// Stable self-signed TLS certificate persisted independently from app identity.
pub struct TlsIdentity {
    certificate_der: Vec<u8>,
    private_key_der: Zeroizing<Vec<u8>>,
}

impl TlsIdentity {
    /// Loads or atomically creates a long-lived certificate for certificate pinning.
    pub fn load_or_create(directory: impl AsRef<Path>) -> Result<Self, CoreError> {
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
            return Self::load_bundle(&bundle_path);
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
        identity.persist_bundle(&bundle_path)?;
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

    fn load_bundle(path: &Path) -> Result<Self, CoreError> {
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
        let bundle: PersistedTlsIdentity = serde_json::from_slice(&bytes)?;
        if bundle.schema_version != TLS_IDENTITY_SCHEMA_VERSION {
            return Err(CoreError::InvalidState(
                "unsupported QUIC identity schema".to_owned(),
            ));
        }
        let certificate_der = URL_SAFE_NO_PAD
            .decode(bundle.certificate_der)
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let private_key_der = URL_SAFE_NO_PAD
            .decode(bundle.private_key_der)
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let identity = Self {
            certificate_der,
            private_key_der: Zeroizing::new(private_key_der),
        };
        identity.validate()?;
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

    fn persist_bundle(&self, path: &Path) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec(&PersistedTlsIdentity {
            schema_version: TLS_IDENTITY_SCHEMA_VERSION,
            certificate_der: URL_SAFE_NO_PAD.encode(&self.certificate_der),
            private_key_der: URL_SAFE_NO_PAD.encode(&self.private_key_der[..]),
        })?;
        persist_private(path, &bytes, true)
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
        self.server_config_with_alpn(ALPN)
    }

    fn server_config_with_alpn(&self, alpn: &[u8]) -> Result<ServerConfig, CoreError> {
        let certificate = CertificateDer::from(self.certificate_der.clone());
        let key = PrivatePkcs8KeyDer::from(self.private_key_der.to_vec());
        let mut crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key.into())
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC TLS: {error}")))?;
        crypto.alpn_protocols = vec![alpn.to_vec()];
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
    blocking_limit: Arc<Semaphore>,
    source_connections: Arc<Mutex<BTreeMap<IpAddr, usize>>>,
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
            blocking_limit: Arc::new(Semaphore::new(MAX_BLOCKING_OPERATIONS)),
            source_connections: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }

    /// Actual bound address, including an assigned ephemeral port.
    pub fn local_addr(&self) -> Result<SocketAddr, CoreError> {
        self.endpoint.local_addr().map_err(|source| CoreError::Io {
            operation: "inspect QUIC peer endpoint",
            path: PathBuf::from("<quic>"),
            source,
        })
    }

    /// Accepts connections until the endpoint is closed or the task is cancelled.
    pub async fn run(self) {
        while let Some(incoming) = self.endpoint.accept().await {
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
            let blocking_limit = Arc::clone(&self.blocking_limit);
            tokio::spawn(async move {
                let _connection_permit = connection_permit;
                let _source_permit = source_permit;
                let Ok(Ok(connection)) =
                    tokio::time::timeout(CONNECTION_HANDSHAKE_TIMEOUT, incoming).await
                else {
                    return;
                };
                loop {
                    let mut streams = match connection.accept_bi().await {
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
                    tokio::spawn(async move {
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
            });
        }
    }
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
            request_timeout: Duration::from_secs(15),
            client_state: Arc::new(tokio::sync::Mutex::new(None)),
            write_leases: Arc::new(Mutex::new(BTreeMap::new())),
        })
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
                error = wait_for_job_stop(control) => Err(error),
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
        let local_engine = self.local_engine.upgrade().ok_or_else(|| {
            CoreError::InvalidState("local engine is no longer available".to_owned())
        })?;
        let operation_bytes = serde_json::to_vec(&operation)?;
        if operation_bytes.len() > MAX_OPERATION_FRAME_BYTES {
            return Err(CoreError::ResourceLimit("QUIC operation frame"));
        }
        let operation_digest = blake3::hash(&operation_bytes).to_hex().to_string();
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
        let response_bytes = tokio::time::timeout(self.request_timeout, async {
            write_frame(&mut send, &hello_bytes).await?;
            write_frame(&mut send, &operation_bytes).await?;
            send.finish().map_err(|error| {
                CoreError::InvalidState(format!("finish QUIC request: {error}"))
            })?;
            read_frame(&mut receive, MAX_RESPONSE_FRAME_BYTES).await
        })
        .await
        .map_err(|_| CoreError::ResourceLimit("QUIC operation timeout"))??;
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

fn map_quic_connection_error(error: ConnectionError) -> CoreError {
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
        let lease =
            self.acquire_storage_lease(backup_id, maximum_new_bytes, maximum_new_objects)?;
        self.write_leases
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .insert(backup_id, lease);
        Ok(())
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
        let lease = self.acquire_storage_lease(backup_id, total_bytes, 1)?;
        if total_bytes > RECOVERY_CAPSULE_SEGMENT_BYTES as u64 {
            let upload_id = uuid::Uuid::new_v4().to_string();
            let total_segments =
                u32::try_from(total_bytes.div_ceil(RECOVERY_CAPSULE_SEGMENT_BYTES as u64))
                    .map_err(|_| CoreError::ResourceLimit("recovery capsule segments"))?;
            let descriptor = RecoveryCapsuleDescriptor {
                backup_id,
                snapshot_id: capsule.snapshot_id.clone(),
                key_epoch: capsule.key_epoch,
                committed_at_unix_ms: capsule.committed_at_unix_ms,
                signer_device_id: capsule.signer_device_id,
                total_bytes,
                capsule_digest: capsule_digest.clone(),
            };
            match self.request(Operation::BeginRecoveryCapsuleUpload {
                backup_id,
                lease: lease.clone(),
                upload_id: upload_id.clone(),
                total_bytes,
                total_segments,
                capsule_digest,
                descriptor,
            })? {
                ResponsePayload::Stored => {}
                _ => return Err(CoreError::AuthenticationFailed),
            }
            let mut segment = vec![0_u8; RECOVERY_CAPSULE_SEGMENT_BYTES];
            for index in 0..total_segments {
                let offset = u64::from(index) * RECOVERY_CAPSULE_SEGMENT_BYTES as u64;
                let length = usize::try_from(
                    (total_bytes - offset).min(RECOVERY_CAPSULE_SEGMENT_BYTES as u64),
                )
                .map_err(|_| CoreError::ResourceLimit("recovery capsule segment"))?;
                encoded
                    .as_file_mut()
                    .read_exact(&mut segment[..length])
                    .map_err(|source| CoreError::Io {
                        operation: "read recovery capsule spool",
                        path: encoded.path().to_path_buf(),
                        source,
                    })?;
                let segment = &segment[..length];
                match self.request(Operation::PutRecoveryCapsuleSegment {
                    backup_id,
                    lease: lease.clone(),
                    upload_id: upload_id.clone(),
                    index,
                    segment: URL_SAFE_NO_PAD.encode(segment),
                    segment_digest: blake3::hash(segment).to_hex().to_string(),
                })? {
                    ResponsePayload::Stored => {}
                    _ => return Err(CoreError::AuthenticationFailed),
                }
            }
            return match self.request(Operation::CommitRecoveryCapsuleUpload {
                backup_id,
                lease,
                upload_id,
            })? {
                ResponsePayload::Stored => Ok(()),
                _ => Err(CoreError::AuthenticationFailed),
            };
        }
        match self.request(Operation::PutRecoveryCapsule {
            backup_id,
            lease,
            capsule: capsule.clone(),
        })? {
            ResponsePayload::Stored => Ok(()),
            _ => Err(CoreError::AuthenticationFailed),
        }
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

    fn acquire_storage_lease(
        &self,
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
    ) -> Result<StorageLease, CoreError> {
        let issued_at_unix_ms = current_unix_ms()?;
        let expires_at_unix_ms = issued_at_unix_ms
            .checked_add(5 * 60 * 1_000)
            .ok_or(CoreError::ResourceLimit("storage lease expiry"))?;
        match self.request(Operation::AcquireStorageLease {
            backup_id,
            max_new_bytes,
            max_new_objects,
            expires_at_unix_ms,
        })? {
            ResponsePayload::StorageLease { lease } => Ok(lease),
            _ => Err(CoreError::AuthenticationFailed),
        }
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
    AcquireStorageLease {
        backup_id: BackupId,
        max_new_bytes: u64,
        max_new_objects: u64,
        expires_at_unix_ms: u64,
    },
    Put {
        backup_id: BackupId,
        lease: StorageLease,
        locator: String,
        record: String,
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
    AcquireStorageLease,
    Put,
    GetScoped,
    GetBatch,
    ContainsScoped,
    PutRecoveryCapsule,
    BeginRecoveryCapsuleUpload,
    PutRecoveryCapsuleSegment,
    CommitRecoveryCapsuleUpload,
    ListRecoveryCapsules,
    GetRecoveryCapsuleSegment,
    GetRoster,
    SubmitRoster,
}

impl Operation {
    const fn kind(&self) -> OperationType {
        match self {
            Self::AcquireStorageLease { .. } => OperationType::AcquireStorageLease,
            Self::Put { .. } => OperationType::Put,
            Self::GetScoped { .. } => OperationType::GetScoped,
            Self::GetBatch { .. } => OperationType::GetBatch,
            Self::ContainsScoped { .. } => OperationType::ContainsScoped,
            Self::PutRecoveryCapsule { .. } => OperationType::PutRecoveryCapsule,
            Self::BeginRecoveryCapsuleUpload { .. } => OperationType::BeginRecoveryCapsuleUpload,
            Self::PutRecoveryCapsuleSegment { .. } => OperationType::PutRecoveryCapsuleSegment,
            Self::CommitRecoveryCapsuleUpload { .. } => OperationType::CommitRecoveryCapsuleUpload,
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
    StorageLease {
        lease: StorageLease,
    },
    Stored,
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

async fn write_frame(send: &mut quinn::SendStream, bytes: &[u8]) -> Result<(), CoreError> {
    let length =
        u32::try_from(bytes.len()).map_err(|_| CoreError::ResourceLimit("QUIC frame length"))?;
    send.write_u32(length)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)?;
    send.write_all(bytes)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)
}

async fn read_frame(receive: &mut quinn::RecvStream, maximum: usize) -> Result<Vec<u8>, CoreError> {
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
    wait_for_peer_budget(
        &rate_limiter,
        hello.device_id,
        hello_bytes.len().saturating_add(operation_bytes),
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
    let worker_engine = Arc::clone(&engine);
    let peer_device_id = hello.device_id;
    let (permit, (ok, payload, error_code)) = tokio::task::spawn_blocking(move || {
        (
            permit,
            process_operation(&operation, &worker_engine, peer_device_id),
        )
    })
    .await
    .map_err(|_| CoreError::InvalidState("QUIC storage worker failed".to_owned()))?;
    let _permit: OwnedSemaphorePermit = permit;
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
        OperationType::AcquireStorageLease
        | OperationType::Put
        | OperationType::PutRecoveryCapsule
        | OperationType::BeginRecoveryCapsuleUpload
        | OperationType::PutRecoveryCapsuleSegment
        | OperationType::CommitRecoveryCapsuleUpload => {
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

fn negotiate_transport_version(minimum: u16, maximum: u16) -> Result<u16, CoreError> {
    if minimum > maximum || minimum > QUIC_TRANSPORT_VERSION || maximum < QUIC_TRANSPORT_VERSION {
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

fn process_operation(
    operation: &Operation,
    engine: &Engine,
    peer_device_id: DeviceId,
) -> (bool, ResponsePayload, Option<String>) {
    let result = match operation {
        Operation::AcquireStorageLease {
            backup_id,
            max_new_bytes,
            max_new_objects,
            expires_at_unix_ms,
        } => current_unix_ms().and_then(|issued_at_unix_ms| {
            engine
                .issue_storage_lease(
                    peer_device_id,
                    *backup_id,
                    *max_new_bytes,
                    *max_new_objects,
                    issued_at_unix_ms,
                    *expires_at_unix_ms,
                )
                .map(|lease| ResponsePayload::StorageLease { lease })
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
                        .commit_leased_recovery_capsule_upload(
                            peer_device_id,
                            lease,
                            upload_id,
                            now_unix_ms,
                        )
                        .map(|_| ResponsePayload::Stored)
                })
            }
        }
        Operation::ListRecoveryCapsules {
            backup_id,
            cursor,
            limit,
        } => engine
            .recovery_capsule_descriptors_for_peer(
                peer_device_id,
                *backup_id,
                cursor.as_deref(),
                *limit,
            )
            .and_then(|(descriptors, next_cursor)| {
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
struct ClientHelloFields<'a> {
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

fn client_hello_bytes(hello: &ClientHello) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&ClientHelloFields {
        device_id: hello.device_id,
        minimum_transport_version: hello.minimum_transport_version,
        maximum_transport_version: hello.maximum_transport_version,
        issued_at_unix_ms: hello.issued_at_unix_ms,
        nonce: &hello.nonce,
        expected_certificate_fingerprint: &hello.expected_certificate_fingerprint,
        operation_type: hello.operation_type,
        operation_bytes: hello.operation_bytes,
        operation_digest: &hello.operation_digest,
    })?)
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

fn transport_limits() -> Result<TransportConfig, CoreError> {
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

fn persist_private(path: &Path, bytes: &[u8], private: bool) -> Result<(), CoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| CoreError::InvalidState("TLS identity path has no parent".to_owned()))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
            operation: "stage QUIC identity file",
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
            operation: "sync QUIC identity file",
            path: path.to_path_buf(),
            source,
        })?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| CoreError::Io {
            operation: "commit QUIC identity file",
            path: path.to_path_buf(),
            source: error.error,
        })?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| CoreError::Io {
            operation: "sync QUIC identity directory",
            path: parent.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use covalent_core::{BackupKey, EngineOptions};
    use covalent_protocol::{PeerGrant, PeerRole};
    use tempfile::tempdir;

    use super::*;

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

    #[tokio::test(flavor = "multi_thread")]
    async fn authenticated_quic_provider_round_trip_and_pin_rejection() {
        let first_data = tempdir().expect("first");
        let second_data = tempdir().expect("second");
        let first = Arc::new(Engine::open(EngineOptions::new(first_data.path())).expect("first"));
        let second =
            Arc::new(Engine::open(EngineOptions::new(second_data.path())).expect("second"));
        trust_all(&first, &second);
        trust_all(&second, &first);
        let tls = TlsIdentity::load_or_create(second_data.path().join("tls")).expect("TLS");
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
        let other_peer = Arc::new(
            Engine::open(EngineOptions::new(other_peer_data.path())).expect("other peer"),
        );
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

        let wrong_tls =
            TlsIdentity::load_or_create(first_data.path().join("other-tls")).expect("other TLS");
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
        let owner = Arc::new(Engine::open(EngineOptions::new(owner_data.path())).expect("owner"));
        let remote =
            Arc::new(Engine::open(EngineOptions::new(provider_data.path())).expect("provider"));
        trust_all(&owner, &remote);
        trust_all(&remote, &owner);
        let tls = TlsIdentity::load_or_create(provider_data.path().join("tls")).expect("TLS");
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
            assert_eq!(
                provider.list_recovery_capsules().expect("streamed list"),
                vec![expected]
            );
        })
        .await
        .expect("worker");
        task.abort();
    }

    #[test]
    fn connected_provider_does_not_form_an_engine_reference_cycle() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(EngineOptions::new(local_data.path())).expect("local"));
        let remote =
            Arc::new(Engine::open(EngineOptions::new(remote_data.path())).expect("remote"));
        trust_all(&local, &remote);
        let tls = TlsIdentity::load_or_create(remote_data.path().join("tls")).expect("TLS");
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

        let first = TlsIdentity::load_or_create(&tls_directory).expect("recover identity");
        let fingerprint = first.certificate_fingerprint();
        assert!(tls_directory.join("identity.json").is_file());
        drop(first);

        let second = TlsIdentity::load_or_create(&tls_directory).expect("reload identity");
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
    fn transport_v2_rejects_old_and_future_framing_as_protocol_incompatible() {
        assert_eq!(covalent_protocol::PROTOCOL_VERSION, 1);
        assert_eq!(QUIC_TRANSPORT_VERSION, 2);
        assert_ne!(ALPN, b"covalent/1");

        let old_client = negotiate_transport_version(1, 1).expect_err("v1 client on v2 server");
        assert!(matches!(&old_client, CoreError::ProtocolNegotiationFailed));
        assert_eq!(error_code(&old_client), "protocol_incompatible");

        let old_server = validate_negotiated_transport_version(
            QUIC_TRANSPORT_VERSION,
            QUIC_TRANSPORT_VERSION,
            1,
        )
        .expect_err("v2 client with v1 server selection");
        assert!(matches!(&old_server, CoreError::ProtocolNegotiationFailed));
        assert_eq!(error_code(&old_server), "protocol_incompatible");

        let future_client =
            negotiate_transport_version(3, 3).expect_err("future client on v2 server");
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

    #[tokio::test(flavor = "multi_thread")]
    async fn legacy_alpn_mismatch_maps_to_explicit_protocol_error() {
        let local_data = tempdir().expect("local");
        let remote_data = tempdir().expect("remote");
        let local = Arc::new(Engine::open(EngineOptions::new(local_data.path())).expect("local"));
        let remote =
            Arc::new(Engine::open(EngineOptions::new(remote_data.path())).expect("remote"));
        let tls = TlsIdentity::load_or_create(remote_data.path().join("tls")).expect("TLS");
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
