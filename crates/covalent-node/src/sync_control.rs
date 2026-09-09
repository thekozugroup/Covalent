//! Signed folder-consent control envelopes for the dedicated QUIC ALPN.
//!
//! This contains no filesystem path and does not provide storage transport
//! fallback. Endpoint dispatch and journal mutation remain explicit host glue.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{CoreError, Engine, PublicIdentity};
use covalent_protocol::{
    DeviceId, FolderShareAcceptance, FolderShareCommit, FolderShareOffer, SyncEngineBinding,
};
use rand_core::{OsRng, RngCore as _};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

pub const FOLDER_CONTROL_ALPN: &[u8] = b"covalent-quic/4";
const SCHEMA_VERSION: u16 = 1;
const REQUEST_DOMAIN: &[u8] = b"covalent/folder-control/request/v1";
const RESPONSE_DOMAIN: &[u8] = b"covalent/folder-control/response/v1";
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_FOLDER_STREAMS: usize = 8;
const MAX_PEERS: usize = 256;
const MAX_NONCES_PER_PEER: usize = 4096;
const FRESHNESS: Duration = Duration::from_secs(5 * 60);
// A sender may be at the allowed future-skew edge when receipt occurs. Its
// request remains fresh for another full window, so replay retention is twice
// the freshness window measured from receipt.
const REPLAY_RETENTION: Duration = FRESHNESS.saturating_mul(2);

fn nil_device_id() -> DeviceId {
    DeviceId::from_uuid(uuid::Uuid::nil())
}

/// Fixed errors. Do not expose peer payloads, local paths, certificates or I/O details.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FolderControlError {
    Invalid,
    Rejected,
    Busy,
    TimedOut,
    Unavailable,
}
impl fmt::Display for FolderControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "folder control request is invalid",
            Self::Rejected => "folder control request was rejected",
            Self::Busy => "folder control is busy",
            Self::TimedOut => "folder control timed out",
            Self::Unavailable => "folder control is unavailable",
        })
    }
}
impl std::error::Error for FolderControlError {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(clippy::enum_variant_names)] // Fixed protocol operation names are part of schema v1.
pub enum FolderControlOperation {
    SendOffer(FolderShareOffer),
    SendAcceptance {
        offer_id: uuid::Uuid,
        acceptance: FolderShareAcceptance,
    },
    SendCommit {
        offer_id: uuid::Uuid,
        commit: FolderShareCommit,
    },
    SendRemoval(crate::sync_engine::FolderRemovalNotice),
    ProbeAddress {
        requester_id: DeviceId,
        target_id: DeviceId,
        candidate_address: String,
    },
}
impl FolderControlOperation {
    fn requester(&self) -> DeviceId {
        match self {
            Self::SendOffer(offer) => offer.source_device_id,
            Self::SendAcceptance { acceptance, .. } => acceptance.target_device_id,
            Self::SendCommit { commit, .. } => commit.source_device_id,
            Self::SendRemoval(notice) => notice.requester_id,
            Self::ProbeAddress { requester_id, .. } => *requester_id,
        }
    }

    // Offer carries both sides. Acceptance and commit bind their other side
    // through the retained offer the service validates before mutation.
    fn declared_target(&self) -> Option<DeviceId> {
        match self {
            Self::SendOffer(offer) => Some(offer.target_device_id),
            Self::SendAcceptance { .. } | Self::SendCommit { .. } => None,
            Self::SendRemoval(notice) => Some(notice.target_id),
            Self::ProbeAddress { target_id, .. } => Some(*target_id),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderControlRequest {
    pub schema_version: u16,
    pub requester_id: DeviceId,
    pub target_id: DeviceId,
    pub expected_server_certificate_fingerprint: String,
    pub issued_at_unix_ms: u64,
    /// Canonical base64url encoding of exactly 24 random bytes.
    pub nonce: String,
    pub operation: FolderControlOperation,
    pub operation_digest: String,
    pub signature: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub enum FolderControlPayload {
    Ack,
    Commit(FolderShareCommit),
    Busy,
    Rejected,
    NeedsAttention,
    AddressProof(SyncEngineBinding),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FolderControlResponse {
    pub schema_version: u16,
    pub request_nonce: String,
    pub request_digest: String,
    pub server_id: DeviceId,
    pub certificate_fingerprint: String,
    pub payload: FolderControlPayload,
    pub payload_digest: String,
    pub signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestTranscript<'a> {
    schema_version: u16,
    requester_id: DeviceId,
    target_id: DeviceId,
    expected_server_certificate_fingerprint: &'a str,
    issued_at_unix_ms: u64,
    nonce: &'a str,
    operation: &'a FolderControlOperation,
    operation_digest: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseTranscript<'a> {
    schema_version: u16,
    request_nonce: &'a str,
    request_digest: &'a str,
    server_id: DeviceId,
    certificate_fingerprint: &'a str,
    payload: &'a FolderControlPayload,
    payload_digest: &'a str,
}

fn digest<T: Serialize>(value: &T) -> Result<String, FolderControlError> {
    Ok(
        blake3::hash(&serde_json::to_vec(value).map_err(|_| FolderControlError::Invalid)?)
            .to_hex()
            .to_string(),
    )
}
fn now_ms() -> Result<u64, FolderControlError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| FolderControlError::Invalid)
        .and_then(|v| u64::try_from(v.as_millis()).map_err(|_| FolderControlError::Invalid))
}
fn valid_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
fn valid_nonce(value: &str) -> bool {
    match URL_SAFE_NO_PAD.decode(value) {
        Ok(decoded) => decoded.len() == 24 && URL_SAFE_NO_PAD.encode(decoded) == value,
        Err(_) => false,
    }
}
fn request_bytes(request: &FolderControlRequest) -> Result<Vec<u8>, FolderControlError> {
    serde_json::to_vec(&RequestTranscript {
        schema_version: request.schema_version,
        requester_id: request.requester_id,
        target_id: request.target_id,
        expected_server_certificate_fingerprint: &request.expected_server_certificate_fingerprint,
        issued_at_unix_ms: request.issued_at_unix_ms,
        nonce: &request.nonce,
        operation: &request.operation,
        operation_digest: &request.operation_digest,
    })
    .map_err(|_| FolderControlError::Invalid)
}
fn response_bytes(response: &FolderControlResponse) -> Result<Vec<u8>, FolderControlError> {
    serde_json::to_vec(&ResponseTranscript {
        schema_version: response.schema_version,
        request_nonce: &response.request_nonce,
        request_digest: &response.request_digest,
        server_id: response.server_id,
        certificate_fingerprint: &response.certificate_fingerprint,
        payload: &response.payload,
        payload_digest: &response.payload_digest,
    })
    .map_err(|_| FolderControlError::Invalid)
}

/// Build the only signed request form. The caller must first compare its
/// explicit retained binding with the engine's current trusted binding.
pub fn sign_request(
    engine: &Engine,
    target: DeviceId,
    expected_fingerprint: &str,
    operation: FolderControlOperation,
) -> Result<FolderControlRequest, FolderControlError> {
    if !valid_fingerprint(expected_fingerprint)
        || target == nil_device_id()
        || target == engine.device_id()
    {
        return Err(FolderControlError::Invalid);
    }
    if operation.requester() != engine.device_id()
        || operation
            .declared_target()
            .is_some_and(|declared| declared != target)
    {
        return Err(FolderControlError::Rejected);
    }
    let operation_digest = digest(&operation)?;
    let mut nonce = [0_u8; 24];
    OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| FolderControlError::Unavailable)?;
    let mut request = FolderControlRequest {
        schema_version: SCHEMA_VERSION,
        requester_id: engine.device_id(),
        target_id: target,
        expected_server_certificate_fingerprint: expected_fingerprint.to_owned(),
        issued_at_unix_ms: now_ms()?,
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        operation,
        operation_digest,
        signature: String::new(),
    };
    request.signature =
        engine.sign_transport_transcript_with_domain(REQUEST_DOMAIN, &request_bytes(&request)?);
    enforce_frame(&request)?;
    Ok(request)
}

/// Authenticate structural, freshness, digest and signature properties before
/// journal mutation. Acceptance/commit target binding is deliberately left to
/// the journal preflight because their records reference a retained offer.
pub fn verify_request(
    request: &FolderControlRequest,
    server: DeviceId,
    fingerprint: &str,
    requester: &PublicIdentity,
    replay: &mut FolderControlReplay,
    now: u64,
) -> Result<(), FolderControlError> {
    enforce_frame(request)?;
    if request.schema_version != SCHEMA_VERSION
        || request.requester_id == nil_device_id()
        || request.target_id == nil_device_id()
        || request.requester_id == request.target_id
        || request.target_id != server
        || request.requester_id != requester.device_id
        || request.expected_server_certificate_fingerprint != fingerprint
        || !valid_fingerprint(fingerprint)
        || now.abs_diff(request.issued_at_unix_ms) > FRESHNESS.as_millis() as u64
        || !valid_nonce(&request.nonce)
        || request.operation_digest != digest(&request.operation)?
    {
        return Err(FolderControlError::Rejected);
    }
    if request.operation.requester() != request.requester_id
        || request
            .operation
            .declared_target()
            .is_some_and(|declared| declared != server)
    {
        return Err(FolderControlError::Rejected);
    }
    if matches!(
        &request.operation,
        FolderControlOperation::ProbeAddress {
            candidate_address,
            ..
        } if !valid_socket_address(candidate_address)
    ) {
        return Err(FolderControlError::Rejected);
    }
    requester
        .verify(
            REQUEST_DOMAIN,
            &request_bytes(request).map_err(|_| FolderControlError::Rejected)?,
            &request.signature,
        )
        .map_err(|_| FolderControlError::Rejected)?;
    replay.insert(request.requester_id, &request.nonce, now)
}

pub fn sign_response(
    engine: &Engine,
    request: &FolderControlRequest,
    certificate_fingerprint: &str,
    payload: FolderControlPayload,
) -> Result<FolderControlResponse, FolderControlError> {
    if !valid_fingerprint(certificate_fingerprint) {
        return Err(FolderControlError::Invalid);
    }
    let mut response = FolderControlResponse {
        schema_version: SCHEMA_VERSION,
        request_nonce: request.nonce.clone(),
        request_digest: digest(request)?,
        server_id: engine.device_id(),
        certificate_fingerprint: certificate_fingerprint.to_owned(),
        payload_digest: digest(&payload)?,
        payload,
        signature: String::new(),
    };
    response.signature =
        engine.sign_transport_transcript_with_domain(RESPONSE_DOMAIN, &response_bytes(&response)?);
    enforce_frame(&response)?;
    Ok(response)
}
pub fn verify_response(
    request: &FolderControlRequest,
    response: &FolderControlResponse,
    target: &PublicIdentity,
    fingerprint: &str,
) -> Result<(), FolderControlError> {
    enforce_frame(response)?;
    if response.schema_version != SCHEMA_VERSION
        || response.server_id != target.device_id
        || response.request_nonce != request.nonce
        || response.request_digest != digest(request)?
        || response.certificate_fingerprint != fingerprint
        || response.payload_digest != digest(&response.payload)?
    {
        return Err(FolderControlError::Rejected);
    }
    target
        .verify(
            RESPONSE_DOMAIN,
            &response_bytes(response)?,
            &response.signature,
        )
        .map_err(|_| FolderControlError::Rejected)
}
fn enforce_frame<T: Serialize>(value: &T) -> Result<(), FolderControlError> {
    if serde_json::to_vec(value)
        .map_err(|_| FolderControlError::Invalid)?
        .len()
        > MAX_FRAME_BYTES
    {
        Err(FolderControlError::Invalid)
    } else {
        Ok(())
    }
}

/// Bounded replay retention. New peers are refused when 256 buckets are full;
/// nonces are never evicted merely to admit a fresh request.
#[derive(Default)]
pub struct FolderControlReplay {
    peers: BTreeMap<DeviceId, VecDeque<(String, u64)>>,
}
impl FolderControlReplay {
    pub fn insert(
        &mut self,
        peer: DeviceId,
        nonce: &str,
        now: u64,
    ) -> Result<(), FolderControlError> {
        self.peers.retain(|_, entries| {
            while entries.front().is_some_and(|(_, at)| {
                now.saturating_sub(*at) > REPLAY_RETENTION.as_millis() as u64
            }) {
                entries.pop_front();
            }
            !entries.is_empty()
        });
        if !self.peers.contains_key(&peer) && self.peers.len() >= MAX_PEERS {
            return Err(FolderControlError::Busy);
        }
        let entries = self.peers.entry(peer).or_default();
        if entries.iter().any(|(old, _)| old == nonce) || entries.len() >= MAX_NONCES_PER_PEER {
            return Err(FolderControlError::Rejected);
        }
        entries.push_back((nonce.to_owned(), now));
        Ok(())
    }
}

#[cfg(test)]
#[path = "sync_control_tests.rs"]
mod tests;

// QUIC plumbing is intentionally one-shot: each control mutation opens one
// connection, one bidirectional stream, sends one frame and receives one frame.
// It never reuses storage-v3 state or falls back to another ALPN.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(45);
const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared server admission state. Keep this beside the endpoint for its whole
/// lifetime; callers must not rebuild it per connection.
pub struct FolderControlAdmission {
    streams: std::sync::Arc<tokio::sync::Semaphore>,
    replay: std::sync::Mutex<FolderControlReplay>,
}

/// Owns exactly the client endpoint and connection for one exchange. Dropping
/// it closes only these handles, including cancellation and error paths.
struct OneShotClientConnection {
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
}
impl OneShotClientConnection {
    async fn close_and_wait(&self) {
        self.connection
            .close(0_u32.into(), b"folder control complete");
        self.endpoint
            .close(0_u32.into(), b"folder control complete");
        self.endpoint.wait_idle().await;
    }
}
impl Drop for OneShotClientConnection {
    fn drop(&mut self) {
        self.connection
            .close(0_u32.into(), b"folder control complete");
        self.endpoint
            .close(0_u32.into(), b"folder control complete");
    }
}

/// The endpoint remains owned by the host dispatcher. This guard closes only
/// the accepted one-shot connection on every error, timeout, or cancellation.
struct OneShotServerConnection(quinn::Connection);
impl Drop for OneShotServerConnection {
    fn drop(&mut self) {
        self.0.close(0_u32.into(), b"folder control complete");
    }
}
impl Default for FolderControlAdmission {
    fn default() -> Self {
        Self {
            streams: std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_FOLDER_STREAMS)),
            replay: std::sync::Mutex::new(FolderControlReplay::default()),
        }
    }
}

/// A client-owned, explicitly pinned one-shot control exchange. The caller
/// passes a retained pairing binding; the current config snapshot must equal it.
pub async fn send_folder_control(
    engine: std::sync::Arc<Engine>,
    binding: covalent_protocol::TransportBinding,
    operation: FolderControlOperation,
) -> Result<FolderControlPayload, FolderControlError> {
    send_folder_control_to(engine, binding, None, operation).await
}

/// Authenticate the retained peer at one candidate endpoint and obtain its
/// signed current engine route. The candidate is not trusted or persisted by
/// this exchange; the caller must commit it through the serialized journal.
/// The signed request binds requester, target, candidate, nonce and digest.
/// Its old route is instead the exact current grant/certificate pin checked
/// before dialing and checked again by journal preparation and core CAS after
/// the response. This permits the peer to answer from a newly reachable route
/// without treating that unauthenticated route as retained state.
pub(crate) async fn send_peer_address_probe(
    engine: std::sync::Arc<Engine>,
    binding: covalent_protocol::TransportBinding,
    candidate_address: std::net::SocketAddr,
) -> Result<SyncEngineBinding, FolderControlError> {
    if !valid_socket_address(&candidate_address.to_string()) {
        return Err(FolderControlError::Invalid);
    }
    let operation = FolderControlOperation::ProbeAddress {
        requester_id: engine.device_id(),
        target_id: binding.peer_id,
        candidate_address: candidate_address.to_string(),
    };
    let expected_peer = binding.peer_id;
    match send_folder_control_to(engine, binding, Some(candidate_address), operation).await? {
        FolderControlPayload::AddressProof(binding) => {
            binding
                .validate()
                .map_err(|_| FolderControlError::Rejected)?;
            if binding.covalent_device_id != expected_peer {
                return Err(FolderControlError::Rejected);
            }
            Ok(binding)
        }
        _ => Err(FolderControlError::Rejected),
    }
}

async fn send_folder_control_to(
    engine: std::sync::Arc<Engine>,
    binding: covalent_protocol::TransportBinding,
    address_override: Option<std::net::SocketAddr>,
    operation: FolderControlOperation,
) -> Result<FolderControlPayload, FolderControlError> {
    // This is deliberately one config read. The same observed grant supplies
    // the response signer and the exact retained endpoint/certificate pin.
    let (target, retained_address, certificate, target_identity) =
        current_binding(&engine, &binding)?;
    let address = address_override.unwrap_or(retained_address);
    let request = sign_request(&engine, target, &binding.certificate_fingerprint, operation)?;
    tokio::time::timeout(EXCHANGE_TIMEOUT, async move {
        use quinn::{ClientConfig, Endpoint};
        use rustls::pki_types::CertificateDer;
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(CertificateDer::from(certificate.clone()))
            .map_err(|_| FolderControlError::Rejected)?;
        let mut crypto = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        crypto.alpn_protocols = vec![FOLDER_CONTROL_ALPN.to_vec()];
        let quic = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|_| FolderControlError::Unavailable)?;
        let mut config = ClientConfig::new(std::sync::Arc::new(quic));
        config.transport_config(std::sync::Arc::new(
            crate::transport::transport_limits().map_err(|_| FolderControlError::Unavailable)?,
        ));
        let bind = if address.is_ipv6() {
            "[::]:0"
        } else {
            "0.0.0.0:0"
        };
        let mut endpoint =
            Endpoint::client(bind.parse().map_err(|_| FolderControlError::Unavailable)?)
                .map_err(|_| FolderControlError::Unavailable)?;
        endpoint.set_default_client_config(config);
        let connection = endpoint
            .connect(address, "covalent.local")
            .map_err(|_| FolderControlError::Unavailable)?
            .await
            .map_err(map_connection_error)?;
        let client = OneShotClientConnection {
            endpoint,
            connection,
        };
        let identity = client
            .connection
            .peer_identity()
            .ok_or(FolderControlError::Rejected)?;
        let chain = identity
            .downcast::<Vec<CertificateDer<'static>>>()
            .map_err(|_| FolderControlError::Rejected)?;
        if chain.len() != 1 || chain[0].as_ref() != certificate.as_slice() {
            return Err(FolderControlError::Rejected);
        }
        let (mut send, mut receive) = client
            .connection
            .open_bi()
            .await
            .map_err(map_connection_error)?;
        let bytes = serde_json::to_vec(&request).map_err(|_| FolderControlError::Invalid)?;
        crate::transport::write_frame(&mut send, &bytes)
            .await
            .map_err(|_| FolderControlError::Unavailable)?;
        send.finish().map_err(|_| FolderControlError::Unavailable)?;
        let response = crate::transport::read_frame(&mut receive, MAX_FRAME_BYTES)
            .await
            .map_err(|_| FolderControlError::Unavailable)?;
        let response: FolderControlResponse =
            serde_json::from_slice(&response).map_err(|_| FolderControlError::Rejected)?;
        verify_response(
            &request,
            &response,
            &target_identity,
            &binding.certificate_fingerprint,
        )?;
        client.close_and_wait().await;
        Ok(response.payload)
    })
    .await
    .map_err(|_| FolderControlError::TimedOut)?
}

/// Serve exactly one bidirectional stream after the QUIC endpoint has selected
/// `covalent-quic/4`. Root dispatch must reject every other ALPN before calling.
pub async fn serve_folder_control_connection(
    connection: quinn::Connection,
    engine: std::sync::Arc<Engine>,
    service: std::sync::Arc<crate::sync_engine::FolderSyncService>,
    server_fingerprint: String,
    admission: std::sync::Arc<FolderControlAdmission>,
) -> Result<(), FolderControlError> {
    tokio::time::timeout(EXCHANGE_TIMEOUT, async move {
        let connection = OneShotServerConnection(connection);
        // Expensive, unauthenticated peers never hold a mutation permit. The
        // host also caps connections, while this deadline bounds a partial
        // 64KiB frame held by a single accepted connection.
        let (mut send, request) = tokio::time::timeout(AUTHENTICATION_TIMEOUT, async {
            let (send, mut receive) = connection
                .0
                .accept_bi()
                .await
                .map_err(map_connection_error)?;
            let bytes = crate::transport::read_frame(&mut receive, MAX_FRAME_BYTES)
                .await
                .map_err(|_| FolderControlError::Rejected)?;
            let request: FolderControlRequest =
                serde_json::from_slice(&bytes).map_err(|_| FolderControlError::Rejected)?;
            let requester = current_identity(&engine, request.requester_id)?;
            let mut replay = admission
                .replay
                .lock()
                .map_err(|_| FolderControlError::Busy)?;
            verify_request(
                &request,
                engine.device_id(),
                &server_fingerprint,
                &requester,
                &mut replay,
                now_ms()?,
            )?;
            Ok::<_, FolderControlError>((send, request))
        })
        .await
        .map_err(|_| FolderControlError::TimedOut)??;
        let permit = admission.streams.clone().try_acquire_owned().ok();
        let payload = if permit.is_some() {
            apply_remote(service, request.operation.clone()).await
        } else {
            FolderControlPayload::Busy
        };
        let response = sign_response(&engine, &request, &server_fingerprint, payload)?;
        let response_bytes =
            serde_json::to_vec(&response).map_err(|_| FolderControlError::Invalid)?;
        crate::transport::write_frame(&mut send, &response_bytes)
            .await
            .map_err(|_| FolderControlError::Unavailable)?;
        send.finish().map_err(|_| FolderControlError::Unavailable)?;
        // Waiting for the peer to stop its receive side avoids closing the
        // connection while Quinn still has the response queued. The enclosing
        // exchange budget bounds a peer that never consumes it.
        let _ = send.stopped().await;
        drop(permit);
        Ok(())
    })
    .await
    .map_err(|_| FolderControlError::TimedOut)?
}

async fn apply_remote(
    service: std::sync::Arc<crate::sync_engine::FolderSyncService>,
    operation: FolderControlOperation,
) -> FolderControlPayload {
    match operation {
        FolderControlOperation::SendOffer(offer) => {
            let Ok(now) = now_ms() else {
                return FolderControlPayload::NeedsAttention;
            };
            match service.receive_offer(offer, now).await {
                Ok(_) => FolderControlPayload::Ack,
                Err(crate::sync_engine::FolderSyncServiceError::Busy) => FolderControlPayload::Busy,
                Err(_) => FolderControlPayload::NeedsAttention,
            }
        }
        FolderControlOperation::SendAcceptance {
            offer_id,
            acceptance,
        } => {
            let Ok(now) = now_ms() else {
                return FolderControlPayload::NeedsAttention;
            };
            match service.receive_acceptance(offer_id, acceptance, now).await {
                Ok(value) => FolderControlPayload::Commit(value.into_value()),
                Err(crate::sync_engine::FolderSyncServiceError::Busy) => FolderControlPayload::Busy,
                Err(_) => FolderControlPayload::NeedsAttention,
            }
        }
        FolderControlOperation::SendCommit { offer_id, commit } => {
            match service.receive_commit(offer_id, commit).await {
                Ok(_) => FolderControlPayload::Ack,
                Err(crate::sync_engine::FolderSyncServiceError::Busy) => FolderControlPayload::Busy,
                Err(_) => FolderControlPayload::NeedsAttention,
            }
        }
        FolderControlOperation::SendRemoval(notice) => {
            match service.receive_removal(&notice).await {
                Ok(_) => FolderControlPayload::Ack,
                Err(crate::sync_engine::FolderSyncServiceError::Busy) => FolderControlPayload::Busy,
                Err(_) => FolderControlPayload::NeedsAttention,
            }
        }
        FolderControlOperation::ProbeAddress { .. } => match service.current_binding().await {
            Ok(binding) => FolderControlPayload::AddressProof(binding),
            Err(crate::sync_engine::FolderSyncServiceError::Busy) => FolderControlPayload::Busy,
            Err(_) => FolderControlPayload::NeedsAttention,
        },
    }
}

fn current_identity(engine: &Engine, peer: DeviceId) -> Result<PublicIdentity, FolderControlError> {
    let config = engine
        .config()
        .map_err(|_| FolderControlError::Unavailable)?;
    let grant = config
        .trusted_peers
        .get(&peer)
        .ok_or(FolderControlError::Rejected)?;
    if grant.revoked
        || grant.confirmed_at_unix_ms == 0
        || grant.peer_device_id != peer
        || !config.trusted_peer_transports.contains_key(&peer)
    {
        return Err(FolderControlError::Rejected);
    }
    PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
        .map_err(|_| FolderControlError::Rejected)
}
fn current_binding(
    engine: &Engine,
    expected: &covalent_protocol::TransportBinding,
) -> Result<(DeviceId, std::net::SocketAddr, Vec<u8>, PublicIdentity), FolderControlError> {
    let config = engine
        .config()
        .map_err(|_| FolderControlError::Unavailable)?;
    let grant = config
        .trusted_peers
        .get(&expected.peer_id)
        .ok_or(FolderControlError::Rejected)?;
    if grant.revoked
        || grant.confirmed_at_unix_ms == 0
        || grant.peer_device_id != expected.peer_id
        || config.trusted_peer_transports.get(&expected.peer_id) != Some(expected)
    {
        return Err(FolderControlError::Rejected);
    }
    let address: std::net::SocketAddr = expected
        .address
        .parse()
        .map_err(|_| FolderControlError::Rejected)?;
    if !valid_socket_address(&expected.address) {
        return Err(FolderControlError::Rejected);
    }
    let certificate = URL_SAFE_NO_PAD
        .decode(&expected.certificate_der)
        .map_err(|_| FolderControlError::Rejected)?;
    if certificate.is_empty()
        || certificate.len() > 64 * 1024
        || !valid_fingerprint(&expected.certificate_fingerprint)
    {
        return Err(FolderControlError::Rejected);
    }
    let actual: String = sha2::Sha256::digest(&certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected.certificate_fingerprint {
        return Err(FolderControlError::Rejected);
    }
    let identity = PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
        .map_err(|_| FolderControlError::Rejected)?;
    Ok((expected.peer_id, address, certificate, identity))
}

fn valid_socket_address(value: &str) -> bool {
    let Some(address) = (value.len() <= 128)
        .then(|| value.parse::<std::net::SocketAddr>().ok())
        .flatten()
    else {
        return false;
    };
    let ambiguous_ipv6 = match address {
        std::net::SocketAddr::V6(address) => {
            address.ip().segments()[0] & 0xffc0 == 0xfe80
                || address.scope_id() != 0
                || address.flowinfo() != 0
        }
        std::net::SocketAddr::V4(_) => false,
    };
    address.port() != 0
        && !address.ip().is_unspecified()
        && !address.ip().is_multicast()
        && !matches!(address.ip(), std::net::IpAddr::V4(ip) if ip.is_broadcast())
        && !ambiguous_ipv6
        && address.to_string() == value
}

fn map_connection_error(error: quinn::ConnectionError) -> FolderControlError {
    match crate::transport::map_quic_connection_error(error) {
        CoreError::ProtocolNegotiationFailed => FolderControlError::Rejected,
        _ => FolderControlError::Unavailable,
    }
}
