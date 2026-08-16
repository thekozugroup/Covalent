//! Authenticated, pinned-certificate QUIC encrypted-object transport.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{ChunkProvider, CoreError, Engine, ProviderHealth, PublicIdentity};
use covalent_protocol::{DeviceId, MAX_FRAME_BYTES, PROTOCOL_VERSION, PeerRole, SignedRoster};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig, VarInt};
use rand_core::{OsRng, RngCore};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const ALPN: &[u8] = b"covalent/1";
const TRANSPORT_SIGNATURE_DOMAIN: &[u8] = b"covalent/authenticated-quic/v1";
const MAX_REPLAY_NONCES_PER_PEER: usize = 4_096;
const MAX_REQUEST_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);
const MAX_PROVIDER_RECORD_BYTES: usize = 8 * 1_024 * 1_024 + 128;
const MAX_REQUESTS_PER_PEER_WINDOW: u32 = 512;
const REQUEST_RATE_WINDOW: Duration = Duration::from_secs(60);

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
        let certificate_path = directory.join("certificate.der");
        let key_path = directory.join("private-key.der");
        match (
            fs::symlink_metadata(&certificate_path),
            fs::symlink_metadata(&key_path),
        ) {
            (Ok(certificate), Ok(key)) => {
                if certificate.file_type().is_symlink()
                    || key.file_type().is_symlink()
                    || !certificate.is_file()
                    || !key.is_file()
                    || certificate.len() > 64 * 1_024
                    || key.len() > 64 * 1_024
                {
                    return Err(CoreError::InvalidState(
                        "invalid QUIC identity files".to_owned(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if key.permissions().mode() & 0o077 != 0 {
                        return Err(CoreError::InvalidState(
                            "QUIC private key permissions are too broad".to_owned(),
                        ));
                    }
                }
                Ok(Self {
                    certificate_der: fs::read(&certificate_path).map_err(|source| {
                        CoreError::Io {
                            operation: "read QUIC certificate",
                            path: certificate_path,
                            source,
                        }
                    })?,
                    private_key_der: Zeroizing::new(fs::read(&key_path).map_err(|source| {
                        CoreError::Io {
                            operation: "read QUIC private key",
                            path: key_path,
                            source,
                        }
                    })?),
                })
            }
            (Err(certificate_error), Err(key_error))
                if certificate_error.kind() == std::io::ErrorKind::NotFound
                    && key_error.kind() == std::io::ErrorKind::NotFound =>
            {
                let generated = rcgen::generate_simple_self_signed(vec!["covalent.local".into()])
                    .map_err(|error| {
                    CoreError::InvalidState(format!("generate QUIC certificate: {error}"))
                })?;
                let identity = Self {
                    certificate_der: generated.cert.der().to_vec(),
                    private_key_der: Zeroizing::new(generated.signing_key.serialize_der()),
                };
                persist_private(&certificate_path, &identity.certificate_der, false)?;
                persist_private(&key_path, identity.private_key_der.as_ref(), true)?;
                Ok(identity)
            }
            _ => Err(CoreError::InvalidState(
                "incomplete QUIC identity files".to_owned(),
            )),
        }
    }

    /// DER certificate used in pairing and pinning.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    /// BLAKE3 pin bound into application identity transcripts.
    #[must_use]
    pub fn certificate_fingerprint(&self) -> String {
        blake3::hash(&self.certificate_der).to_hex().to_string()
    }

    fn server_config(&self) -> Result<ServerConfig, CoreError> {
        let certificate = CertificateDer::from(self.certificate_der.clone());
        let key = PrivatePkcs8KeyDer::from(self.private_key_der.to_vec());
        let mut crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate], key.into())
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC TLS: {error}")))?;
        crypto.alpn_protocols = vec![ALPN.to_vec()];
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
            let engine = Arc::clone(&self.engine);
            let fingerprint = self.certificate_fingerprint.clone();
            let replay_window = Arc::clone(&self.replay_window);
            let rate_limiter = Arc::clone(&self.rate_limiter);
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                loop {
                    let streams = match connection.accept_bi().await {
                        Ok(streams) => streams,
                        Err(_) => break,
                    };
                    let engine = Arc::clone(&engine);
                    let fingerprint = fingerprint.clone();
                    let replay_window = Arc::clone(&replay_window);
                    let rate_limiter = Arc::clone(&rate_limiter);
                    tokio::spawn(async move {
                        let _ = handle_stream(
                            streams,
                            engine,
                            &fingerprint,
                            replay_window,
                            rate_limiter,
                        )
                        .await;
                    });
                }
            });
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
        })
    }

    fn request(&self, operation: Operation) -> Result<ResponsePayload, CoreError> {
        quic_runtime()?.block_on(self.request_async(operation))
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
            .map_err(|error| {
                CoreError::InvalidState(format!("complete QUIC connection: {error}"))
            })?;
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
        let operation_digest = blake3::hash(&operation_bytes).to_hex().to_string();
        let mut nonce = [0_u8; 24];
        OsRng.fill_bytes(&mut nonce);
        let nonce = URL_SAFE_NO_PAD.encode(nonce);
        let expected_certificate_fingerprint =
            blake3::hash(&self.remote_certificate).to_hex().to_string();
        let mut hello = ClientHello {
            device_id: local_engine.device_id(),
            minimum_protocol_version: PROTOCOL_VERSION,
            maximum_protocol_version: PROTOCOL_VERSION,
            issued_at_unix_ms: current_unix_ms()?,
            nonce,
            expected_certificate_fingerprint,
            operation_digest,
            signature: String::new(),
        };
        hello.signature = local_engine.sign_transport_transcript(&client_hello_bytes(&hello)?);
        let request = WireRequest { hello, operation };
        let request_bytes = serde_json::to_vec(&request)?;
        if request_bytes.len() > MAX_FRAME_BYTES {
            return Err(CoreError::ResourceLimit("QUIC request frame"));
        }

        let connection = self.connection().await?;
        let (mut send, mut receive) = connection
            .open_bi()
            .await
            .map_err(|error| CoreError::InvalidState(format!("open QUIC stream: {error}")))?;
        send.write_all(&request_bytes)
            .await
            .map_err(|error| CoreError::InvalidState(format!("write QUIC request: {error}")))?;
        send.finish()
            .map_err(|error| CoreError::InvalidState(format!("finish QUIC request: {error}")))?;
        let response_bytes = receive
            .read_to_end(MAX_FRAME_BYTES)
            .await
            .map_err(|error| CoreError::InvalidState(format!("read QUIC response: {error}")))?;
        let response: WireResponse = serde_json::from_slice(&response_bytes)?;
        let remote_certificate_fingerprint = blake3::hash(&self.remote_certificate).to_hex();
        verify_server_response(
            &request,
            &response,
            &self.remote_identity,
            remote_certificate_fingerprint.as_ref(),
        )?;
        if response.ok {
            Ok(response.payload)
        } else {
            Err(match response.error_code.as_deref() {
                Some("missing_chunk") => CoreError::MissingChunk("remote".to_owned()),
                Some("peer_revoked") => CoreError::PeerRevoked,
                Some("not_authorized") => CoreError::UnselectedProvider,
                Some("resource_limit") => CoreError::ResourceLimit("remote provider"),
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

impl fmt::Debug for QuicProvider {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuicProvider")
            .field("address", &self.address)
            .field("remote_identity", &self.remote_identity)
            .field(
                "certificate_fingerprint",
                &blake3::hash(&self.remote_certificate).to_hex().to_string(),
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

    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
        if record.len() > MAX_PROVIDER_RECORD_BYTES {
            return Err(CoreError::ResourceLimit("provider record"));
        }
        match self.request(Operation::Put {
            locator: locator.to_owned(),
            record: URL_SAFE_NO_PAD.encode(record),
        })? {
            ResponsePayload::Stored => Ok(()),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }

    fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
        match self.request(Operation::Get {
            locator: locator.to_owned(),
        })? {
            ResponsePayload::Record { record } => {
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

    fn contains(&self, locator: &str) -> Result<bool, CoreError> {
        match self.request(Operation::Contains {
            locator: locator.to_owned(),
        })? {
            ResponsePayload::Presence { present } => Ok(present),
            _ => Err(CoreError::AuthenticationFailed),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClientHello {
    device_id: DeviceId,
    minimum_protocol_version: u16,
    maximum_protocol_version: u16,
    issued_at_unix_ms: u64,
    nonce: String,
    expected_certificate_fingerprint: String,
    operation_digest: String,
    signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ServerHello {
    device_id: DeviceId,
    negotiated_protocol_version: u16,
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
    Put { locator: String, record: String },
    Get { locator: String },
    Contains { locator: String },
    GetRoster,
    SubmitRoster { roster: SignedRoster },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ResponsePayload {
    Stored,
    Record { record: String },
    Presence { present: bool },
    Roster { roster: Option<SignedRoster> },
    RosterAccepted,
    Error,
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
    windows: std::collections::BTreeMap<DeviceId, (Instant, u32)>,
}

impl PeerRateLimiter {
    fn check(&mut self, peer_id: DeviceId, now: Instant) -> bool {
        self.windows.retain(|_, (started, _)| {
            now.saturating_duration_since(*started) < REQUEST_RATE_WINDOW
        });
        let entry = self.windows.entry(peer_id).or_insert((now, 0));
        if now.saturating_duration_since(entry.0) >= REQUEST_RATE_WINDOW {
            *entry = (now, 0);
        }
        if entry.1 >= MAX_REQUESTS_PER_PEER_WINDOW {
            return false;
        }
        entry.1 += 1;
        true
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

async fn handle_stream(
    (mut send, mut receive): (quinn::SendStream, quinn::RecvStream),
    engine: Arc<Engine>,
    certificate_fingerprint: &str,
    replay_window: Arc<Mutex<ReplayWindow>>,
    rate_limiter: Arc<Mutex<PeerRateLimiter>>,
) -> Result<(), CoreError> {
    let request_bytes = receive
        .read_to_end(MAX_FRAME_BYTES)
        .await
        .map_err(|_| CoreError::ResourceLimit("QUIC request frame"))?;
    let request: WireRequest = serde_json::from_slice(&request_bytes)?;
    verify_client_request(
        &request,
        &engine,
        certificate_fingerprint,
        &replay_window,
        &rate_limiter,
    )?;
    let (ok, payload, error_code) = process_operation(&request.operation, &engine);
    let payload_digest = response_payload_digest(ok, &payload, error_code.as_deref())?;
    let mut response_nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut response_nonce);
    let mut server_hello = ServerHello {
        device_id: engine.device_id(),
        negotiated_protocol_version: PROTOCOL_VERSION,
        request_nonce: request.hello.nonce.clone(),
        response_nonce: URL_SAFE_NO_PAD.encode(response_nonce),
        certificate_fingerprint: certificate_fingerprint.to_owned(),
        response_digest: payload_digest,
        signature: String::new(),
    };
    server_hello.signature = engine.sign_transport_transcript(&server_hello_bytes(&server_hello)?);
    let response = WireResponse {
        ok,
        payload,
        error_code,
        server_hello,
    };
    let bytes = serde_json::to_vec(&response)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(CoreError::ResourceLimit("QUIC response frame"));
    }
    send.write_all(&bytes)
        .await
        .map_err(|_| CoreError::AuthenticationFailed)?;
    send.finish().map_err(|_| CoreError::AuthenticationFailed)
}

fn verify_client_request(
    request: &WireRequest,
    engine: &Engine,
    certificate_fingerprint: &str,
    replay_window: &Mutex<ReplayWindow>,
    rate_limiter: &Mutex<PeerRateLimiter>,
) -> Result<(), CoreError> {
    if request.hello.minimum_protocol_version > PROTOCOL_VERSION
        || request.hello.maximum_protocol_version < PROTOCOL_VERSION
        || request.hello.expected_certificate_fingerprint != certificate_fingerprint
        || request.hello.operation_digest
            != blake3::hash(&serde_json::to_vec(&request.operation)?)
                .to_hex()
                .to_string()
        || !matches!(
            URL_SAFE_NO_PAD.decode(&request.hello.nonce),
            Ok(nonce) if nonce.len() == 24
        )
        || current_unix_ms()?.abs_diff(request.hello.issued_at_unix_ms)
            > MAX_REQUEST_CLOCK_SKEW.as_millis() as u64
    {
        return Err(CoreError::ProtocolNegotiationFailed);
    }
    let peer = match request.operation {
        Operation::Put { .. } => {
            engine.authorized_peer(request.hello.device_id, PeerRole::BackupWriter)?
        }
        Operation::Get { .. } | Operation::Contains { .. } => {
            engine.authorized_peer(request.hello.device_id, PeerRole::BackupReader)?
        }
        Operation::GetRoster | Operation::SubmitRoster { .. } => {
            engine.trusted_peer_identity(request.hello.device_id)?
        }
    };
    peer.verify(
        TRANSPORT_SIGNATURE_DOMAIN,
        &client_hello_bytes(&request.hello)?,
        &request.hello.signature,
    )?;
    if !replay_window
        .lock()
        .map_err(|_| CoreError::Synchronization)?
        .insert_fresh(request.hello.device_id, request.hello.nonce.clone())
    {
        return Err(CoreError::AuthenticationFailed);
    }
    if !rate_limiter
        .lock()
        .map_err(|_| CoreError::Synchronization)?
        .check(request.hello.device_id, Instant::now())
    {
        return Err(CoreError::ResourceLimit("peer request rate"));
    }
    Ok(())
}

fn process_operation(
    operation: &Operation,
    engine: &Engine,
) -> (bool, ResponsePayload, Option<String>) {
    let result = match operation {
        Operation::Put { locator, record } => URL_SAFE_NO_PAD
            .decode(record)
            .map_err(|_| CoreError::AuthenticationFailed)
            .and_then(|record| {
                if record.len() > MAX_PROVIDER_RECORD_BYTES {
                    return Err(CoreError::ResourceLimit("provider record"));
                }
                engine
                    .store()
                    .put_provider_record(locator, &record)
                    .map(|_| ResponsePayload::Stored)
            }),
        Operation::Get { locator } => {
            engine
                .store()
                .get_provider_record(locator)
                .map(|record| ResponsePayload::Record {
                    record: URL_SAFE_NO_PAD.encode(record),
                })
        }
        Operation::Contains { locator } => engine
            .store()
            .contains(locator)
            .map(|present| ResponsePayload::Presence { present }),
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
    if response.server_hello.device_id != remote_identity.device_id
        || response.server_hello.negotiated_protocol_version != PROTOCOL_VERSION
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
    minimum_protocol_version: u16,
    maximum_protocol_version: u16,
    issued_at_unix_ms: u64,
    nonce: &'a str,
    expected_certificate_fingerprint: &'a str,
    operation_digest: &'a str,
}

fn client_hello_bytes(hello: &ClientHello) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&ClientHelloFields {
        device_id: hello.device_id,
        minimum_protocol_version: hello.minimum_protocol_version,
        maximum_protocol_version: hello.maximum_protocol_version,
        issued_at_unix_ms: hello.issued_at_unix_ms,
        nonce: &hello.nonce,
        expected_certificate_fingerprint: &hello.expected_certificate_fingerprint,
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
    negotiated_protocol_version: u16,
    request_nonce: &'a str,
    response_nonce: &'a str,
    certificate_fingerprint: &'a str,
    response_digest: &'a str,
}

fn server_hello_bytes(hello: &ServerHello) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&ServerHelloFields {
        device_id: hello.device_id,
        negotiated_protocol_version: hello.negotiated_protocol_version,
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
    transport.stream_receive_window(VarInt::from_u32(MAX_FRAME_BYTES as u32));
    transport.receive_window(VarInt::from_u32((MAX_FRAME_BYTES * 4) as u32));
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
        CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed => "authentication_failed",
        _ => "provider_error",
    }
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
        let chunk = key
            .encrypt_chunk(covalent_protocol::BackupId::new(), 1, b"over QUIC")
            .expect("chunk");
        let locator = chunk.opaque_locator.clone();
        let record = chunk.encode_provider_record();
        let local_roster = first
            .current_roster()
            .expect("current roster")
            .expect("issued roster");
        tokio::task::spawn_blocking(move || {
            provider.put(&locator, &record).expect("put");
            assert!(provider.contains(&locator).expect("contains"));
            assert_eq!(provider.get(&locator).expect("get"), record);
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
    fn per_peer_rate_limiter_is_bounded_and_resets() {
        let peer = DeviceId::new();
        let start = Instant::now();
        let mut limiter = PeerRateLimiter::default();
        for _ in 0..MAX_REQUESTS_PER_PEER_WINDOW {
            assert!(limiter.check(peer, start));
        }
        assert!(!limiter.check(peer, start));
        assert!(limiter.check(peer, start + REQUEST_RATE_WINDOW));
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
