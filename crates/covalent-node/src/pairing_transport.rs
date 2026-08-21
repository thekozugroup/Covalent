//! Pairing-only QUIC path for discovery-mediated network pairing.
//!
//! Two devices that have never met cannot use the authenticated storage
//! transport, because that path admits only already-authorized peers. This
//! module carries [`NetworkPairingWireRequest`] envelopes over a separate ALPN
//! on the same advertised QUIC endpoint instead.
//!
//! Nothing here is trusted on the strength of TLS alone. The dialing side
//! deliberately accepts an unknown self-signed certificate and then binds the
//! exact bytes it observed into [`NetworkPairingManager::register_outgoing`],
//! which refuses any invitation whose signed transport binding does not name
//! the same address and the same certificate. Every request is signed by the
//! requester's own device identity and consumed exactly once through
//! [`NetworkPairingManager::verify_and_consume_wire_request`], so freshness,
//! replay, and operation binding are enforced before dispatch. Mutual consent
//! still requires a human comparing the short authentication string on both
//! devices; this path only moves the signed exchange between them.

use std::collections::{BTreeMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use covalent_core::{CoreError, Engine, PairingSession};
use covalent_protocol::TransportBinding;
use quinn::{ClientConfig, Endpoint};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use tokio::sync::Semaphore;

use crate::network_pairing::{
    NETWORK_PAIRING_SCHEMA_VERSION, NetworkPairingManager, NetworkPairingWireOperation,
    NetworkPairingWireRequest, NetworkPairingWireResponse, rate_limit_bucket_key,
    validate_pairing_route,
};
use crate::transport::{
    PAIRING_ALPN, map_quic_connection_error, read_frame, transport_limits, write_frame,
};

/// Pairing envelopes carry one signed invitation or exchange, never stored objects.
const MAX_PAIRING_FRAME_BYTES: usize = 256 * 1_024;
/// A pairing dial is a foreground user action on a local network; fail fast.
const PAIRING_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const PAIRING_REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Ceiling on one `Submit`-driven probe.
///
/// Deliberately far shorter than [`PAIRING_CONNECT_TIMEOUT`], which covers a
/// dial the local user asked for and may be aimed at a device that needs waking.
/// A probe is not that: its target has, by the source check in
/// [`NetworkPairingService::submit`], just completed a QUIC handshake with this
/// node from the same address, so it is awake and one hop away. The long timeout
/// was the per-attempt cost of using this node as a port prober.
const PAIRING_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Probes one source may start inside [`PROBE_BUDGET_WINDOW_MS`].
///
/// A legitimate pairing spends exactly one, on the first `Submit` of an
/// exchange; the remainder covers retries after a transient failure.
const MAX_PROBES_PER_SOURCE: usize = 4;
const PROBE_BUDGET_WINDOW_MS: u64 = 60 * 1_000;
/// Probes in flight across the whole node. Bounds the sockets and tasks the
/// pairing path can be made to hold open at once, without queueing — a queue
/// would let an attacker delay a real pairing instead of being refused.
const MAX_CONCURRENT_PROBES: usize = 4;
/// How long a failed probe is remembered, so hammering one dead address costs
/// one dial rather than one per attempt. Short enough that a peer whose listener
/// was briefly not ready recovers without operator involvement.
const PROBE_FAILURE_CACHE_MS: u64 = 10 * 1_000;
/// Ceilings on the guard's own memory. Both maps drain on their own; these stop
/// a burst from many sources growing them without bound in the meantime.
const MAX_PROBE_BUCKETS: usize = 256;
const MAX_PROBE_FAILURE_ENTRIES: usize = 256;
/// Bounds one connection so a single source cannot pin pairing capacity open.
const MAX_PAIRING_STREAMS_PER_CONNECTION: usize = 8;
/// Certificate ceiling shared with every other transport-binding validation.
const MAX_CERTIFICATE_BYTES: usize = 64 * 1_024;

/// Bounds the reflection and port-probe capability the `Submit` path exposes.
///
/// A `Submit` makes this node dial an address the sender named. The route check
/// confines that to private networks, and the source check in
/// [`NetworkPairingService::submit`] confines it to the sender's own address —
/// but a caller can still ask for arbitrary *ports* there, and each attempt used
/// to cost eight seconds of this node's time. This is what makes each attempt
/// cheap for the node, rare for the caller, and free to repeat.
#[derive(Debug)]
struct ProbeGuard {
    inflight: Semaphore,
    budget: Mutex<ProbeBudget>,
}

#[derive(Debug, Default)]
struct ProbeBudget {
    /// Probe start times per rate-limiting bucket, oldest first.
    attempts: BTreeMap<String, VecDeque<u64>>,
    /// Addresses whose last probe failed, with the instant the memory expires.
    ///
    /// Only failures are cached, and the asymmetry is deliberate: a remembered
    /// failure can refuse a pairing but can never make one succeed, whereas
    /// remembering a certificate would let a byte string captured from one host
    /// satisfy a live binding check against whatever holds that address later.
    failures: BTreeMap<SocketAddr, u64>,
}

impl ProbeGuard {
    fn new() -> Self {
        Self {
            inflight: Semaphore::new(MAX_CONCURRENT_PROBES),
            budget: Mutex::new(ProbeBudget::default()),
        }
    }

    /// Charges one probe of `address` to `bucket`, or refuses it.
    fn admit(&self, bucket: &str, address: SocketAddr, now_unix_ms: u64) -> Result<(), CoreError> {
        let mut budget = self.budget.lock().map_err(|_| CoreError::Synchronization)?;
        let window_start = now_unix_ms.saturating_sub(PROBE_BUDGET_WINDOW_MS);
        budget
            .failures
            .retain(|_, expires_at| *expires_at > now_unix_ms);
        for attempts in budget.attempts.values_mut() {
            while attempts.front().is_some_and(|at| *at < window_start) {
                attempts.pop_front();
            }
        }
        budget.attempts.retain(|_, attempts| !attempts.is_empty());

        if budget.failures.contains_key(&address) {
            return Err(CoreError::InvitationUnavailable);
        }
        if budget
            .attempts
            .get(bucket)
            .is_some_and(|attempts| attempts.len() >= MAX_PROBES_PER_SOURCE)
        {
            return Err(CoreError::ResourceLimit("pairing probes"));
        }
        if budget.attempts.len() >= MAX_PROBE_BUCKETS && !budget.attempts.contains_key(bucket) {
            // Evict the least recently active bucket rather than refusing a new
            // one. The evicted source has not probed inside the window it would
            // have been charged against, so its budget was about to lapse
            // anyway, and refusing instead would let a spread of sources deny
            // pairing to everyone else.
            let stalest = budget
                .attempts
                .iter()
                .filter_map(|(key, attempts)| attempts.back().map(|at| (*at, key.clone())))
                .min();
            if let Some((_, key)) = stalest {
                budget.attempts.remove(&key);
            }
        }
        budget
            .attempts
            .entry(bucket.to_owned())
            .or_default()
            .push_back(now_unix_ms);
        Ok(())
    }

    fn record_failure(&self, address: SocketAddr, now_unix_ms: u64) {
        let Ok(mut budget) = self.budget.lock() else {
            return;
        };
        if budget.failures.len() >= MAX_PROBE_FAILURE_ENTRIES
            && !budget.failures.contains_key(&address)
        {
            let soonest = budget
                .failures
                .iter()
                .map(|(key, expires_at)| (*expires_at, *key))
                .min();
            if let Some((_, key)) = soonest {
                budget.failures.remove(&key);
            }
        }
        budget
            .failures
            .insert(address, now_unix_ms.saturating_add(PROBE_FAILURE_CACHE_MS));
    }

    fn clear_failure(&self, address: SocketAddr) {
        if let Ok(mut budget) = self.budget.lock() {
            budget.failures.remove(&address);
        }
    }
}

/// Handles pairing-only requests arriving on the node's advertised QUIC endpoint.
pub struct NetworkPairingService {
    engine: Arc<Engine>,
    manager: Arc<NetworkPairingManager>,
    local_transport: Option<TransportBinding>,
    probes: ProbeGuard,
}

impl NetworkPairingService {
    /// Builds the responder half. `local_transport` is absent when the node has
    /// no concrete advertised endpoint, which leaves probing and exchange
    /// forwarding available but refuses to originate invitations.
    #[must_use]
    pub fn new(
        engine: Arc<Engine>,
        manager: Arc<NetworkPairingManager>,
        local_transport: Option<TransportBinding>,
    ) -> Self {
        Self {
            engine,
            manager,
            local_transport,
            probes: ProbeGuard::new(),
        }
    }

    fn now_unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
            .unwrap_or(0)
    }

    /// Verifies one signed request and produces its response, or a stable failure.
    async fn dispatch(
        &self,
        request: &NetworkPairingWireRequest,
        source: IpAddr,
    ) -> NetworkPairingWireResponse {
        let now_unix_ms = self.now_unix_ms();
        if self
            .manager
            .verify_and_consume_wire_request(request, now_unix_ms, Some(source))
            .is_err()
        {
            return failure(
                "pairing_request_rejected",
                "The pairing request was not fresh, signed, and unused.",
            );
        }
        match self.execute(request, source, now_unix_ms).await {
            Ok(response) => response,
            Err(error) => failure_for(&error),
        }
    }

    async fn execute(
        &self,
        request: &NetworkPairingWireRequest,
        source: IpAddr,
        now_unix_ms: u64,
    ) -> Result<NetworkPairingWireResponse, CoreError> {
        match &request.operation {
            NetworkPairingWireOperation::Probe => Ok(NetworkPairingWireResponse::Probe {
                minimum_protocol_version: NETWORK_PAIRING_SCHEMA_VERSION,
                maximum_protocol_version: NETWORK_PAIRING_SCHEMA_VERSION,
            }),
            NetworkPairingWireOperation::Start { .. } => {
                let local_transport = self
                    .local_transport
                    .clone()
                    .ok_or_else(|| CoreError::InvalidState("pairing endpoint".to_owned()))?;
                let invitation = self
                    .manager
                    .create_network_invitation(local_transport, now_unix_ms)?;
                Ok(NetworkPairingWireResponse::Invitation {
                    invitation: Box::new(invitation),
                })
            }
            NetworkPairingWireOperation::Submit {
                pairing_id,
                session,
            } => {
                let merged = self
                    .submit(pairing_id, session, request, source, now_unix_ms)
                    .await?;
                Ok(NetworkPairingWireResponse::Session {
                    session: Box::new(merged),
                })
            }
            NetworkPairingWireOperation::Poll { pairing_id } => {
                let session = self
                    .manager
                    .session_for_peer(pairing_id, request.requester.device_id)?;
                Ok(NetworkPairingWireResponse::Session {
                    session: Box::new(session),
                })
            }
            NetworkPairingWireOperation::Cancel { pairing_id } => {
                self.manager.remove_for_peer(
                    pairing_id,
                    request.requester.device_id,
                    now_unix_ms,
                )?;
                Ok(NetworkPairingWireResponse::Acknowledged)
            }
        }
    }

    /// Registers a first submission after independently probing the responder
    /// binding, then merges only signatures that verify against the same
    /// immutable transcript.
    ///
    /// # Why the probe is confined
    ///
    /// This is the one place an unauthenticated caller makes this node open a
    /// connection to an address of the caller's choosing, so the capability that
    /// hands out is bounded here rather than left to the dial itself:
    ///
    /// * The route check keeps it off public networks.
    /// * The address probed must be the address the `Submit` arrived from. QUIC
    ///   address validation has already proven the caller receives packets
    ///   there, so this reduces "make the node touch any host on the LAN" to
    ///   "make the node touch the caller's own host" — and a caller can reach
    ///   its own host without this node's help.
    /// * What survives that is the port: a caller may still ask for any port on
    ///   its own address and learn from the response whether something answered.
    ///   [`ProbeGuard`] is what makes that expensive to repeat and cheap for
    ///   this node to refuse.
    ///
    /// The source check has an operational edge, and it is a refusal rather than
    /// a silent downgrade: a multi-homed peer that advertises one interface but
    /// routes to this node out of another will be turned away. Pairing then
    /// fails visibly and is fixed by advertising the interface the peer actually
    /// reaches this node on, which is the safe direction for this to break.
    async fn submit(
        &self,
        pairing_id: &str,
        session: &PairingSession,
        request: &NetworkPairingWireRequest,
        source: IpAddr,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        if self.manager.item(pairing_id, now_unix_ms).is_ok() {
            // Reject a submission that names a retained request belonging to a
            // different identity before any state is touched. Note that this
            // path never probes: only a first submission does.
            self.manager
                .session_for_peer(pairing_id, request.requester.device_id)?;
            return self
                .manager
                .merge_peer_session(pairing_id, session, now_unix_ms);
        }
        if session.invitation().inviter_device_id != self.engine.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        let responder_transport = session
            .responder_transport()
            .ok_or(CoreError::AuthenticationFailed)?;
        let responder_address = responder_transport
            .address
            .parse::<SocketAddr>()
            .map_err(|_| CoreError::AuthenticationFailed)?;
        // A signed request must never steer this node at an arbitrary endpoint.
        validate_pairing_route(responder_address, false)?;
        if responder_address.ip() != source {
            return Err(CoreError::AuthenticationFailed);
        }
        let observed = self.probe(responder_address, source, now_unix_ms).await?;
        self.manager.register_incoming(
            responder_address,
            &observed,
            session.clone(),
            now_unix_ms,
        )?;
        self.manager
            .session_for_peer(pairing_id, request.requester.device_id)
    }

    /// Probes `address` under the guard's budget, cache, and concurrency cap.
    async fn probe(
        &self,
        address: SocketAddr,
        source: IpAddr,
        now_unix_ms: u64,
    ) -> Result<Vec<u8>, CoreError> {
        self.probes
            .admit(&rate_limit_bucket_key(Some(source)), address, now_unix_ms)?;
        // Refused rather than queued: waiting for a slot would let a burst of
        // probes delay a real pairing instead of being turned away.
        let _permit = self
            .probes
            .inflight
            .try_acquire()
            .map_err(|_| CoreError::ResourceLimit("pairing probes in flight"))?;
        match PairingConnection::probe(address).await {
            Ok(observed) => {
                self.probes.clear_failure(address);
                Ok(observed)
            }
            Err(error) => {
                self.probes.record_failure(address, now_unix_ms);
                Err(error)
            }
        }
    }
}

/// Serves pairing streams until the peer closes the connection or the task ends.
pub(crate) async fn serve_pairing_connection(
    connection: quinn::Connection,
    service: Arc<NetworkPairingService>,
    stream_limit: Arc<Semaphore>,
) {
    let connection_streams = Arc::new(Semaphore::new(MAX_PAIRING_STREAMS_PER_CONNECTION));
    // The address every request on this connection is attributed to. QUIC
    // address validation has already run, so it is a reachable peer rather than
    // a spoofed header.
    let source = connection.remote_address().ip();
    while let Ok(streams) = connection.accept_bi().await {
        let Ok(connection_permit) = Arc::clone(&connection_streams).try_acquire_owned() else {
            break;
        };
        let Ok(stream_permit) = Arc::clone(&stream_limit).try_acquire_owned() else {
            break;
        };
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            let _connection_permit = connection_permit;
            let _stream_permit = stream_permit;
            let _ = tokio::time::timeout(
                PAIRING_REQUEST_TIMEOUT,
                serve_pairing_stream(streams, service, source),
            )
            .await;
        });
    }
}

async fn serve_pairing_stream(
    (mut send, mut receive): (quinn::SendStream, quinn::RecvStream),
    service: Arc<NetworkPairingService>,
    source: IpAddr,
) -> Result<(), CoreError> {
    let bytes = read_frame(&mut receive, MAX_PAIRING_FRAME_BYTES).await?;
    let response = match serde_json::from_slice::<NetworkPairingWireRequest>(&bytes) {
        Ok(request) => service.dispatch(&request, source).await,
        Err(_) => failure(
            "pairing_request_invalid",
            "The pairing request does not satisfy the versioned wire contract.",
        ),
    };
    let encoded = serde_json::to_vec(&response)?;
    if encoded.len() > MAX_PAIRING_FRAME_BYTES {
        return Err(CoreError::ResourceLimit("pairing response frame"));
    }
    write_frame(&mut send, &encoded).await?;
    send.finish().map_err(|_| CoreError::AuthenticationFailed)
}

/// One dialed pairing connection plus the exact certificate the peer presented.
pub struct PairingConnection {
    _endpoint: Endpoint,
    connection: quinn::Connection,
    observed_certificate: Vec<u8>,
}

impl PairingConnection {
    /// Dials the pairing-only ALPN and records the presented certificate.
    ///
    /// The certificate is deliberately unverified against any trust anchor:
    /// this is a first contact between strangers. It becomes meaningful only
    /// once the caller binds it against the peer's signed transport binding.
    pub async fn connect(address: SocketAddr) -> Result<Self, CoreError> {
        Self::connect_within(address, PAIRING_CONNECT_TIMEOUT).await
    }

    async fn connect_within(address: SocketAddr, timeout: Duration) -> Result<Self, CoreError> {
        validate_pairing_route(address, true)?;
        let observed = Arc::new(Mutex::new(None));
        let verifier = Arc::new(RecordingVerifier {
            observed: Arc::clone(&observed),
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        });
        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        client_crypto.alpn_protocols = vec![PAIRING_ALPN.to_vec()];
        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
            .map_err(|error| CoreError::InvalidState(format!("configure QUIC client: {error}")))?;
        let mut client = ClientConfig::new(Arc::new(quic_crypto));
        client.transport_config(Arc::new(transport_limits()?));
        let bind = if address.is_ipv6() {
            SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0)
        } else {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0)
        };
        let mut endpoint = Endpoint::client(bind).map_err(|source| CoreError::Io {
            operation: "bind QUIC pairing client endpoint",
            path: PathBuf::from("<quic-pairing-client>"),
            source,
        })?;
        endpoint.set_default_client_config(client);
        let connecting = endpoint
            .connect(address, "covalent.local")
            .map_err(|error| CoreError::InvalidState(format!("start QUIC pairing: {error}")))?;
        let connection = tokio::time::timeout(timeout, connecting)
            .await
            .map_err(|_| CoreError::ResourceLimit("QUIC pairing connection timeout"))?
            .map_err(map_quic_connection_error)?;
        let observed_certificate = observed
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone()
            .ok_or(CoreError::AuthenticationFailed)?;
        if observed_certificate.is_empty() || observed_certificate.len() > MAX_CERTIFICATE_BYTES {
            return Err(CoreError::InvalidKeyMaterial);
        }
        Ok(Self {
            _endpoint: endpoint,
            connection,
            observed_certificate,
        })
    }

    /// Exact end-entity certificate bytes this connection was established against.
    #[must_use]
    pub fn observed_certificate(&self) -> &[u8] {
        &self.observed_certificate
    }

    /// Sends one signed request and reads its single framed response.
    pub async fn request(
        &self,
        request: &NetworkPairingWireRequest,
    ) -> Result<NetworkPairingWireResponse, CoreError> {
        let bytes = serde_json::to_vec(request)?;
        if bytes.len() > MAX_PAIRING_FRAME_BYTES {
            return Err(CoreError::ResourceLimit("pairing request frame"));
        }
        let exchange = async {
            let (mut send, mut receive) = self
                .connection
                .open_bi()
                .await
                .map_err(map_quic_connection_error)?;
            write_frame(&mut send, &bytes).await?;
            send.finish().map_err(|_| CoreError::AuthenticationFailed)?;
            let response = read_frame(&mut receive, MAX_PAIRING_FRAME_BYTES).await?;
            Ok::<_, CoreError>(serde_json::from_slice(&response)?)
        };
        tokio::time::timeout(PAIRING_REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| CoreError::ResourceLimit("QUIC pairing request timeout"))?
    }

    /// Observes a peer's live certificate without exchanging pairing state.
    ///
    /// Held to [`PAIRING_PROBE_TIMEOUT`] rather than the dial timeout: a probe
    /// target has just handshaked with this node, so a slow one is a probe worth
    /// abandoning, not a device worth waiting for.
    async fn probe(address: SocketAddr) -> Result<Vec<u8>, CoreError> {
        let connection = Self::connect_within(address, PAIRING_PROBE_TIMEOUT).await?;
        Ok(connection.observed_certificate)
    }
}

/// Records the presented end-entity certificate and defers all trust to the
/// signed transport binding checked by the caller.
#[derive(Debug)]
struct RecordingVerifier {
    observed: Arc<Mutex<Option<Vec<u8>>>>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for RecordingVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.is_empty() || end_entity.len() > MAX_CERTIFICATE_BYTES {
            return Err(rustls::Error::General(
                "unsupported pairing certificate".to_owned(),
            ));
        }
        let mut observed = self
            .observed
            .lock()
            .map_err(|_| rustls::Error::General("pairing certificate capture".to_owned()))?;
        *observed = Some(end_entity.to_vec());
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn failure(code: &str, message: &str) -> NetworkPairingWireResponse {
    NetworkPairingWireResponse::Failed {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

/// Maps an internal error onto a stable, non-revealing wire failure code.
fn failure_for(error: &CoreError) -> NetworkPairingWireResponse {
    match error {
        CoreError::InvitationUnavailable => failure(
            "pairing_unavailable",
            "The pairing request is unknown, expired, or already used.",
        ),
        CoreError::IdentityMismatch | CoreError::AuthenticationFailed => failure(
            "pairing_identity_mismatch",
            "The pairing exchange did not verify against the expected identity.",
        ),
        CoreError::ResourceLimit(_) => failure(
            "pairing_resource_limit",
            "The peer reached a configured pairing resource limit.",
        ),
        _ => failure(
            "pairing_unavailable_state",
            "The peer cannot accept a pairing exchange right now.",
        ),
    }
}
