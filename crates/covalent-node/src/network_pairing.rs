//! Durable user-consent state for discovery-mediated network pairing.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use covalent_core::PublicIdentity;
use covalent_core::{CoreError, Engine, PairingSession};
use covalent_protocol::{DeviceId, PairingInvitation, PeerRole, TransportBinding};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::persist_private_file;

/// Wire schema version negotiated by the pairing-only QUIC probe.
pub const NETWORK_PAIRING_SCHEMA_VERSION: u16 = 1;
const MAX_NETWORK_PAIRING_STATE_BYTES: usize = 8 * 1_024 * 1_024;
const MAX_NETWORK_PAIRING_ITEMS: usize = 64;
const PAIRING_ROLE_COUNT: usize = 3;
const MAX_RESOLVED_CANDIDATES: usize = 8;
const PAIRING_RESOLUTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const NETWORK_PAIRING_REQUEST_DOMAIN: &[u8] = b"covalent/network-pairing-request/v1";
const NETWORK_PAIRING_REQUEST_SKEW_MS: u64 = 5 * 60 * 1_000;
const MAX_CONSUMED_REQUEST_NONCES: usize = 4_096;

/// Resolves one explicit host:port candidate with bounded DNS and safe-route defaults.
pub async fn resolve_pairing_candidate(
    candidate: &str,
    allow_public_route: bool,
) -> Result<Vec<SocketAddr>, CoreError> {
    if candidate.is_empty()
        || candidate.len() > 512
        || candidate.contains("//")
        || candidate.chars().any(char::is_control)
    {
        return Err(CoreError::InvalidState(
            "invalid pairing candidate".to_owned(),
        ));
    }
    let resolved = tokio::time::timeout(
        PAIRING_RESOLUTION_TIMEOUT,
        tokio::net::lookup_host(candidate),
    )
    .await
    .map_err(|_| CoreError::InvalidState("pairing candidate resolution timed out".to_owned()))?
    .map_err(|error| {
        CoreError::InvalidState(format!("resolve pairing candidate failed: {error}"))
    })?;
    let mut addresses = BTreeSet::new();
    for address in resolved {
        validate_resolved_candidate(address, allow_public_route)?;
        addresses.insert(address);
        if addresses.len() > MAX_RESOLVED_CANDIDATES {
            return Err(CoreError::ResourceLimit("pairing candidate addresses"));
        }
    }
    if addresses.is_empty() {
        return Err(CoreError::InvalidState(
            "pairing candidate resolved no addresses".to_owned(),
        ));
    }
    Ok(addresses.into_iter().collect())
}

/// Rejects a pairing endpoint whose route is unsafe for an unauthenticated exchange.
///
/// Applied to every address this node dials for pairing, including the responder
/// binding named inside a remote `Submit`, so a signed request cannot steer this
/// node at an arbitrary public endpoint.
pub fn validate_pairing_route(
    address: SocketAddr,
    allow_public_route: bool,
) -> Result<(), CoreError> {
    validate_resolved_candidate(address, allow_public_route)
}

/// Whether a pairing request was initiated here or arrived from a remote node.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPairingDirection {
    Incoming,
    Outgoing,
}

/// Stable local state exposed to native polling clients.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPairingStatus {
    AwaitingLocalConfirmation,
    AwaitingPeerConfirmation,
    Complete,
    Failed,
}

/// Secret-free item returned by the authenticated local API.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkPairingItem {
    pub pairing_id: String,
    pub direction: NetworkPairingDirection,
    pub peer_name: String,
    pub authentication_string: String,
    pub expires_at_unix_ms: u64,
    pub state: NetworkPairingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer_transport: Option<TransportBinding>,
}

/// Authenticated operation carried only over the pairing-only QUIC path.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPairingWireOperation {
    Probe,
    Start {
        responder_transport: TransportBinding,
    },
    Submit {
        pairing_id: String,
        session: Box<PairingSession>,
    },
    Poll {
        pairing_id: String,
    },
    Cancel {
        pairing_id: String,
    },
}

/// Fresh identity-signed request; request IDs remain replay-protected across restart.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NetworkPairingWireRequest {
    pub schema_version: u16,
    pub request_id: String,
    pub issued_at_unix_ms: u64,
    pub requester: PublicIdentity,
    pub operation: NetworkPairingWireOperation,
    pub signature: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPairingWireResponse {
    Probe {
        minimum_protocol_version: u16,
        maximum_protocol_version: u16,
    },
    Invitation {
        invitation: Box<PairingInvitation>,
    },
    Session {
        session: Box<PairingSession>,
    },
    Acknowledged,
    Failed {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedNetworkPairingItem {
    pairing_id: String,
    direction: NetworkPairingDirection,
    peer_device_id: DeviceId,
    peer_name: String,
    candidate_address: SocketAddr,
    observed_certificate_fingerprint: String,
    expires_at_unix_ms: u64,
    state: NetworkPairingStatus,
    local_confirmed: bool,
    session: PairingSession,
    failure_code: Option<String>,
    failure_message: Option<String>,
    peer_transport: Option<TransportBinding>,
}

impl PersistedNetworkPairingItem {
    fn view(&self) -> NetworkPairingItem {
        NetworkPairingItem {
            pairing_id: self.pairing_id.clone(),
            direction: self.direction,
            peer_name: self.peer_name.clone(),
            authentication_string: self.session.authentication_string().as_str().to_owned(),
            expires_at_unix_ms: self.expires_at_unix_ms,
            state: self.state,
            failure_code: self.failure_code.clone(),
            failure_message: self.failure_message.clone(),
            peer_transport: self.peer_transport.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedNetworkPairingState {
    schema_version: u16,
    items: BTreeMap<String, PersistedNetworkPairingItem>,
    #[serde(default)]
    consumed_request_nonces: BTreeMap<String, u64>,
}

impl Default for PersistedNetworkPairingState {
    fn default() -> Self {
        Self {
            schema_version: NETWORK_PAIRING_SCHEMA_VERSION,
            items: BTreeMap::new(),
            consumed_request_nonces: BTreeMap::new(),
        }
    }
}

/// Crash-safe coordinator for the mutually confirmed pairing transcript.
pub struct NetworkPairingManager {
    engine: Arc<Engine>,
    state_path: PathBuf,
    state: Mutex<PersistedNetworkPairingState>,
}

impl NetworkPairingManager {
    /// Opens retained pairing requests. Active requests resume after restart.
    pub fn open(engine: Arc<Engine>, state_path: PathBuf) -> Result<Self, CoreError> {
        let state = match std::fs::symlink_metadata(&state_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || !metadata.is_file()
                    || metadata.len() > MAX_NETWORK_PAIRING_STATE_BYTES as u64
                {
                    return Err(CoreError::InvalidState(
                        "invalid network pairing state".to_owned(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(CoreError::InvalidState(
                            "network pairing state permissions are too broad".to_owned(),
                        ));
                    }
                }
                let bytes = std::fs::read(&state_path).map_err(|source| CoreError::Io {
                    operation: "read network pairing state",
                    path: state_path.clone(),
                    source,
                })?;
                serde_json::from_slice(&bytes)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedNetworkPairingState::default()
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect network pairing state",
                    path: state_path,
                    source,
                });
            }
        };
        validate_state(&state, engine.device_id())?;
        Ok(Self {
            engine,
            state_path,
            state: Mutex::new(state),
        })
    }

    /// Creates a concrete local binding from a live QUIC interface address and TLS identity.
    pub fn local_transport_binding(
        &self,
        address: SocketAddr,
        certificate_der: &[u8],
    ) -> Result<TransportBinding, CoreError> {
        validate_concrete_address(address)?;
        if certificate_der.is_empty() || certificate_der.len() > 64 * 1_024 {
            return Err(CoreError::InvalidKeyMaterial);
        }
        Ok(TransportBinding {
            peer_id: self.engine.device_id(),
            display_name: self.engine.config()?.device_name,
            address: address.to_string(),
            certificate_der: base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                certificate_der,
            ),
            certificate_fingerprint: sha256_hex(certificate_der),
        })
    }

    /// Issues the server half of one short-lived network exchange at a live observed endpoint.
    pub fn create_network_invitation(
        &self,
        local_transport: TransportBinding,
        now_unix_ms: u64,
    ) -> Result<PairingInvitation, CoreError> {
        validate_transport_binding_shape(&local_transport)?;
        if local_transport.peer_id != self.engine.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        self.engine
            .pairing_manager()
            .create_invitation_with_transport(
                now_unix_ms,
                5 * 60 * 1_000,
                vec![local_transport.address.clone()],
                local_transport,
            )
    }

    /// Signs one fresh pairing-only request without exposing the node identity key.
    pub fn sign_wire_request(
        &self,
        operation: NetworkPairingWireOperation,
        now_unix_ms: u64,
    ) -> Result<NetworkPairingWireRequest, CoreError> {
        let mut nonce = [0_u8; 16];
        OsRng.fill_bytes(&mut nonce);
        let mut request = NetworkPairingWireRequest {
            schema_version: NETWORK_PAIRING_SCHEMA_VERSION,
            request_id: base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                nonce,
            ),
            issued_at_unix_ms: now_unix_ms,
            requester: self.engine.public_identity(),
            operation,
            signature: String::new(),
        };
        request.signature = self.engine.sign_transport_transcript_with_domain(
            NETWORK_PAIRING_REQUEST_DOMAIN,
            &wire_request_signing_bytes(&request)?,
        );
        Ok(request)
    }

    /// Verifies freshness/signature and durably consumes a request nonce before dispatch.
    pub fn verify_and_consume_wire_request(
        &self,
        request: &NetworkPairingWireRequest,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        if request.schema_version != NETWORK_PAIRING_SCHEMA_VERSION
            || now_unix_ms.abs_diff(request.issued_at_unix_ms) > NETWORK_PAIRING_REQUEST_SKEW_MS
            || request.signature.is_empty()
            || base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                &request.request_id,
            )
            .ok()
            .is_none_or(|nonce| nonce.len() != 16)
        {
            return Err(CoreError::AuthenticationFailed);
        }
        request.requester.verify(
            NETWORK_PAIRING_REQUEST_DOMAIN,
            &wire_request_signing_bytes(request)?,
            &request.signature,
        )?;
        match &request.operation {
            NetworkPairingWireOperation::Probe => {}
            NetworkPairingWireOperation::Start {
                responder_transport,
            } => {
                if responder_transport.peer_id != request.requester.device_id {
                    return Err(CoreError::AuthenticationFailed);
                }
                validate_transport_binding_shape(responder_transport)?;
            }
            NetworkPairingWireOperation::Submit { session, .. } => {
                if session.responder_device_id() != request.requester.device_id {
                    return Err(CoreError::AuthenticationFailed);
                }
                session.validate_exchange(now_unix_ms)?;
                validate_session_roles(session)?;
            }
            NetworkPairingWireOperation::Poll { .. }
            | NetworkPairingWireOperation::Cancel { .. } => {}
        }
        let nonce_key = format!("{}:{}", request.requester.device_id, request.request_id);
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        state
            .consumed_request_nonces
            .retain(|_, expires_at| *expires_at > now_unix_ms);
        if state.consumed_request_nonces.contains_key(&nonce_key)
            || state.consumed_request_nonces.len() >= MAX_CONSUMED_REQUEST_NONCES
        {
            return Err(CoreError::AuthenticationFailed);
        }
        state.consumed_request_nonces.insert(
            nonce_key,
            request
                .issued_at_unix_ms
                .saturating_add(NETWORK_PAIRING_REQUEST_SKEW_MS),
        );
        self.persist_locked(&state)
    }

    /// Registers an outgoing exchange only after the signed invitation matches the probed peer.
    pub fn register_outgoing(
        &self,
        candidate_address: SocketAddr,
        observed_certificate: &[u8],
        invitation: PairingInvitation,
        local_transport: TransportBinding,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        validate_concrete_address(candidate_address)?;
        let peer_transport = invitation
            .transport_binding
            .as_ref()
            .ok_or(CoreError::AuthenticationFailed)?;
        validate_observed_binding(peer_transport, candidate_address, observed_certificate)?;
        let observed_certificate_fingerprint = peer_transport.certificate_fingerprint.clone();
        let roles = network_pairing_roles();
        let session = self.engine.accept_pairing_with_transport(
            invitation,
            local_transport,
            roles.clone(),
            roles,
            now_unix_ms,
        )?;
        validate_session_roles(&session)?;
        let item = PersistedNetworkPairingItem {
            pairing_id: session.invitation().invitation_id.clone(),
            direction: NetworkPairingDirection::Outgoing,
            peer_device_id: session.invitation().inviter_device_id,
            peer_name: session.invitation().inviter_device_name.clone(),
            candidate_address,
            observed_certificate_fingerprint,
            expires_at_unix_ms: session.invitation().expires_at_unix_ms,
            state: NetworkPairingStatus::AwaitingLocalConfirmation,
            local_confirmed: false,
            session: session.clone(),
            failure_code: None,
            failure_message: None,
            peer_transport: None,
        };
        self.insert(item)?;
        Ok(session)
    }

    /// Registers an incoming accepted session after independently probing the responder binding.
    pub fn register_incoming(
        &self,
        candidate_address: SocketAddr,
        observed_certificate: &[u8],
        session: PairingSession,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        session.validate_exchange(now_unix_ms)?;
        validate_session_roles(&session)?;
        if session.invitation().inviter_device_id != self.engine.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        let peer_transport = session
            .responder_transport()
            .ok_or(CoreError::AuthenticationFailed)?;
        validate_observed_binding(peer_transport, candidate_address, observed_certificate)?;
        self.insert(PersistedNetworkPairingItem {
            pairing_id: session.invitation().invitation_id.clone(),
            direction: NetworkPairingDirection::Incoming,
            peer_device_id: session.responder_device_id(),
            peer_name: session.responder_name().to_owned(),
            candidate_address,
            observed_certificate_fingerprint: peer_transport.certificate_fingerprint.clone(),
            expires_at_unix_ms: session.invitation().expires_at_unix_ms,
            state: NetworkPairingStatus::AwaitingLocalConfirmation,
            local_confirmed: false,
            session,
            failure_code: None,
            failure_message: None,
            peer_transport: None,
        })
    }

    /// Records this device's explicit SAS approval and returns the newly signed session to send.
    pub fn confirm_local(
        &self,
        pairing_id: &str,
        displayed_code: &str,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        expire_items(&mut state, now_unix_ms);
        let item = state
            .items
            .get_mut(pairing_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        if matches!(
            item.state,
            NetworkPairingStatus::Complete | NetworkPairingStatus::Failed
        ) {
            return Err(CoreError::InvalidState(
                "network pairing is not confirmable".to_owned(),
            ));
        }
        if displayed_code != item.session.authentication_string().as_str() {
            return Err(CoreError::IdentityMismatch);
        }
        item.local_confirmed = true;
        match item.direction {
            NetworkPairingDirection::Outgoing => self.engine.confirm_pairing_as_responder(
                &mut item.session,
                displayed_code,
                now_unix_ms,
            )?,
            NetworkPairingDirection::Incoming => {
                if item.session.responder_is_confirmed() {
                    self.engine.confirm_pairing_as_inviter(
                        &mut item.session,
                        displayed_code,
                        now_unix_ms,
                    )?;
                    finalize_item(&self.engine, item, now_unix_ms)?;
                }
            }
        }
        if item.state != NetworkPairingStatus::Complete {
            item.state = NetworkPairingStatus::AwaitingPeerConfirmation;
        }
        let session = item.session.clone();
        self.persist_locked(&state)?;
        Ok(session)
    }

    /// Authenticates and merges a remote update, finalizing automatically when consent is mutual.
    pub fn merge_peer_session(
        &self,
        pairing_id: &str,
        peer_session: &PairingSession,
        now_unix_ms: u64,
    ) -> Result<PairingSession, CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        expire_items(&mut state, now_unix_ms);
        let item = state
            .items
            .get_mut(pairing_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        if item.state == NetworkPairingStatus::Failed {
            return Err(CoreError::InvitationUnavailable);
        }
        item.session
            .merge_confirmations_from(peer_session, now_unix_ms)?;
        if item.state == NetworkPairingStatus::Complete {
            let session = item.session.clone();
            self.persist_locked(&state)?;
            return Ok(session);
        }
        if item.local_confirmed {
            match item.direction {
                NetworkPairingDirection::Incoming if item.session.responder_is_confirmed() => {
                    if !item.session.inviter_is_confirmed() {
                        let code = item.session.authentication_string().as_str().to_owned();
                        self.engine.confirm_pairing_as_inviter(
                            &mut item.session,
                            &code,
                            now_unix_ms,
                        )?;
                    }
                    finalize_item(&self.engine, item, now_unix_ms)?;
                }
                NetworkPairingDirection::Outgoing if item.session.inviter_is_confirmed() => {
                    finalize_item(&self.engine, item, now_unix_ms)?;
                }
                _ => {}
            }
        }
        if item.state != NetworkPairingStatus::Complete {
            item.state = if item.local_confirmed {
                NetworkPairingStatus::AwaitingPeerConfirmation
            } else {
                NetworkPairingStatus::AwaitingLocalConfirmation
            };
        }
        let session = item.session.clone();
        self.persist_locked(&state)?;
        Ok(session)
    }

    /// Returns all retained requests, including completed items until explicit acknowledgement.
    pub fn items(&self, now_unix_ms: u64) -> Result<Vec<NetworkPairingItem>, CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        if expire_items(&mut state, now_unix_ms) {
            self.persist_locked(&state)?;
        }
        Ok(state
            .items
            .values()
            .map(PersistedNetworkPairingItem::view)
            .collect())
    }

    /// Returns one retained request after applying expiry transitions.
    pub fn item(
        &self,
        pairing_id: &str,
        now_unix_ms: u64,
    ) -> Result<NetworkPairingItem, CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        if expire_items(&mut state, now_unix_ms) {
            self.persist_locked(&state)?;
        }
        state
            .items
            .get(pairing_id)
            .map(PersistedNetworkPairingItem::view)
            .ok_or(CoreError::InvitationUnavailable)
    }

    /// Exact numeric endpoint retained for a remote sync request.
    pub fn candidate_address(&self, pairing_id: &str) -> Result<SocketAddr, CoreError> {
        self.state
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .items
            .get(pairing_id)
            .map(|item| item.candidate_address)
            .ok_or(CoreError::InvitationUnavailable)
    }

    /// Returns the latest signed session only to its exact remote identity.
    pub fn session_for_peer(
        &self,
        pairing_id: &str,
        peer_device_id: DeviceId,
    ) -> Result<PairingSession, CoreError> {
        let state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        let item = state
            .items
            .get(pairing_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        if item.peer_device_id != peer_device_id {
            return Err(CoreError::AuthenticationFailed);
        }
        Ok(item.session.clone())
    }

    /// Removes a remote request only when the signed caller matches its exact peer identity.
    pub fn remove_for_peer(
        &self,
        pairing_id: &str,
        peer_device_id: DeviceId,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        {
            let state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
            if state
                .items
                .get(pairing_id)
                .is_none_or(|item| item.peer_device_id != peer_device_id)
            {
                return Err(CoreError::AuthenticationFailed);
            }
        }
        self.remove(pairing_id, now_unix_ms)
    }

    /// Deletes an item after sending remote cancellation/acknowledgement.
    pub fn remove(&self, pairing_id: &str, now_unix_ms: u64) -> Result<(), CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        let item = state
            .items
            .remove(pairing_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        if item.direction == NetworkPairingDirection::Incoming
            && item.state != NetworkPairingStatus::Complete
        {
            self.engine
                .pairing_manager()
                .cancel_invitation(pairing_id, now_unix_ms)?;
        }
        self.persist_locked(&state)
    }

    /// Retains a stable failed item so polling cannot miss terminal failure.
    pub fn fail(&self, pairing_id: &str, code: &str, message: &str) -> Result<(), CoreError> {
        if code.is_empty()
            || code.len() > 80
            || !code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            || message.is_empty()
            || message.len() > 256
        {
            return Err(CoreError::InvalidState(
                "invalid network pairing failure".to_owned(),
            ));
        }
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        let item = state
            .items
            .get_mut(pairing_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        if item.state != NetworkPairingStatus::Complete {
            item.state = NetworkPairingStatus::Failed;
            item.failure_code = Some(code.to_owned());
            item.failure_message = Some(message.to_owned());
        }
        self.persist_locked(&state)
    }

    fn insert(&self, item: PersistedNetworkPairingItem) -> Result<(), CoreError> {
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        if state.items.len() >= MAX_NETWORK_PAIRING_ITEMS
            && !state.items.contains_key(&item.pairing_id)
        {
            return Err(CoreError::ResourceLimit("network pairing requests"));
        }
        if let Some(incumbent) = state.items.get(&item.pairing_id) {
            if incumbent.peer_device_id == item.peer_device_id
                && incumbent.peer_name == item.peer_name
                && incumbent.candidate_address == item.candidate_address
                && incumbent.observed_certificate_fingerprint
                    == item.observed_certificate_fingerprint
            {
                let mut verified = incumbent.session.clone();
                verified.merge_confirmations_from(
                    &item.session,
                    item.expires_at_unix_ms.saturating_sub(1),
                )?;
                return Ok(());
            }
            return Err(CoreError::AuthenticationFailed);
        }
        state.items.insert(item.pairing_id.clone(), item);
        self.persist_locked(&state)
    }

    fn persist_locked(&self, state: &PersistedNetworkPairingState) -> Result<(), CoreError> {
        let bytes = serde_json::to_vec_pretty(state)?;
        if bytes.len() > MAX_NETWORK_PAIRING_STATE_BYTES {
            return Err(CoreError::ResourceLimit("network pairing state"));
        }
        persist_private_file(&self.state_path, &bytes)
    }
}

fn network_pairing_roles() -> BTreeSet<PeerRole> {
    BTreeSet::from([
        PeerRole::StorageProvider,
        PeerRole::BackupReader,
        PeerRole::BackupWriter,
    ])
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRequestSigningFields<'a> {
    schema_version: u16,
    request_id: &'a str,
    issued_at_unix_ms: u64,
    requester: &'a PublicIdentity,
    operation: &'a NetworkPairingWireOperation,
}

fn wire_request_signing_bytes(request: &NetworkPairingWireRequest) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&WireRequestSigningFields {
        schema_version: request.schema_version,
        request_id: &request.request_id,
        issued_at_unix_ms: request.issued_at_unix_ms,
        requester: &request.requester,
        operation: &request.operation,
    })?)
}

fn validate_session_roles(session: &PairingSession) -> Result<(), CoreError> {
    let expected = network_pairing_roles();
    if session.responder_roles().len() != PAIRING_ROLE_COUNT
        || session.inviter_roles().len() != PAIRING_ROLE_COUNT
        || session.responder_roles() != &expected
        || session.inviter_roles() != &expected
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn validate_concrete_address(address: SocketAddr) -> Result<(), CoreError> {
    if address.ip().is_unspecified() || address.port() == 0 {
        return Err(CoreError::InvalidState(
            "pairing endpoint must be concrete".to_owned(),
        ));
    }
    Ok(())
}

fn validate_transport_binding_shape(binding: &TransportBinding) -> Result<(), CoreError> {
    let address = binding
        .address
        .parse::<SocketAddr>()
        .map_err(|_| CoreError::AuthenticationFailed)?;
    validate_concrete_address(address)?;
    let certificate = base64::Engine::decode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &binding.certificate_der,
    )
    .map_err(|_| CoreError::AuthenticationFailed)?;
    if binding.display_name.trim().is_empty()
        || binding.display_name.len() > 80
        || certificate.is_empty()
        || certificate.len() > 64 * 1_024
        || !valid_lowercase_digest(&binding.certificate_fingerprint)
        || sha256_hex(&certificate) != binding.certificate_fingerprint
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn validate_resolved_candidate(
    address: SocketAddr,
    allow_public_route: bool,
) -> Result<(), CoreError> {
    validate_concrete_address(address)?;
    let ip = address.ip();
    if ip.is_multicast() || ip.is_unspecified() {
        return Err(CoreError::InvalidState(
            "unsafe pairing candidate route".to_owned(),
        ));
    }
    let private_or_tailnet = match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || (ip.segments()[0] & 0xfe00) == 0xfc00
                || (ip.segments()[0] & 0xffc0) == 0xfe80
        }
    };
    if !private_or_tailnet && !allow_public_route {
        return Err(CoreError::InvalidState(
            "public pairing route requires advanced consent".to_owned(),
        ));
    }
    Ok(())
}

fn validate_observed_binding(
    binding: &TransportBinding,
    candidate_address: SocketAddr,
    observed_certificate: &[u8],
) -> Result<(), CoreError> {
    validate_concrete_address(candidate_address)?;
    let address = binding
        .address
        .parse::<SocketAddr>()
        .map_err(|_| CoreError::AuthenticationFailed)?;
    if address != candidate_address
        || observed_certificate.is_empty()
        || observed_certificate.len() > 64 * 1_024
        || binding.certificate_der
            != base64::Engine::encode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                observed_certificate,
            )
        || binding.certificate_fingerprint != sha256_hex(observed_certificate)
    {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(())
}

fn finalize_item(
    engine: &Engine,
    item: &mut PersistedNetworkPairingItem,
    now_unix_ms: u64,
) -> Result<(), CoreError> {
    let confirmation = match item.direction {
        NetworkPairingDirection::Incoming => {
            engine.finalize_pairing_as_inviter(&item.session, now_unix_ms)?
        }
        NetworkPairingDirection::Outgoing => {
            engine.finalize_pairing_as_responder(&item.session, now_unix_ms)?
        }
    };
    let peer_transport = match item.direction {
        NetworkPairingDirection::Incoming => confirmation.responder_transport,
        NetworkPairingDirection::Outgoing => confirmation.inviter_transport,
    }
    .ok_or(CoreError::AuthenticationFailed)?;
    if peer_transport.peer_id != item.peer_device_id
        || peer_transport.address.parse::<SocketAddr>().ok() != Some(item.candidate_address)
        || peer_transport.certificate_fingerprint != item.observed_certificate_fingerprint
    {
        return Err(CoreError::AuthenticationFailed);
    }
    item.peer_transport = Some(peer_transport);
    item.state = NetworkPairingStatus::Complete;
    item.failure_code = None;
    item.failure_message = None;
    Ok(())
}

fn validate_state(
    state: &PersistedNetworkPairingState,
    local_device_id: DeviceId,
) -> Result<(), CoreError> {
    if state.schema_version != NETWORK_PAIRING_SCHEMA_VERSION
        || state.items.len() > MAX_NETWORK_PAIRING_ITEMS
        || state.consumed_request_nonces.len() > MAX_CONSUMED_REQUEST_NONCES
        || state.consumed_request_nonces.iter().any(|(key, expiry)| {
            *expiry == 0
                || key.len() > 256
                || key
                    .rsplit_once(':')
                    .and_then(|(_, request_id)| {
                        base64::Engine::decode(
                            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                            request_id,
                        )
                        .ok()
                    })
                    .is_none_or(|nonce| nonce.len() != 16)
        })
    {
        return Err(CoreError::InvalidState(
            "unsupported network pairing state".to_owned(),
        ));
    }
    for (pairing_id, item) in &state.items {
        let direction_matches = match item.direction {
            NetworkPairingDirection::Incoming => {
                item.session.invitation().inviter_device_id == local_device_id
                    && item.session.responder_device_id() == item.peer_device_id
                    && item.session.responder_name() == item.peer_name
            }
            NetworkPairingDirection::Outgoing => {
                item.session.invitation().inviter_device_id == item.peer_device_id
                    && item.session.invitation().inviter_device_name == item.peer_name
                    && item.session.responder_device_id() == local_device_id
            }
        };
        let failure_matches = match item.state {
            NetworkPairingStatus::Failed => {
                item.failure_code.as_ref().is_some_and(|code| {
                    !code.is_empty()
                        && code.len() <= 80
                        && code
                            .bytes()
                            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                }) && item
                    .failure_message
                    .as_ref()
                    .is_some_and(|message| !message.is_empty() && message.len() <= 256)
            }
            _ => item.failure_code.is_none() && item.failure_message.is_none(),
        };
        let completion_matches = if item.state == NetworkPairingStatus::Complete {
            let expected = match item.direction {
                NetworkPairingDirection::Incoming => item.session.responder_transport(),
                NetworkPairingDirection::Outgoing => {
                    item.session.invitation().transport_binding.as_ref()
                }
            };
            item.local_confirmed
                && item
                    .session
                    .is_mutually_confirmed(item.expires_at_unix_ms.saturating_sub(1))
                && item.peer_transport.as_ref() == expected
                && item.peer_transport.as_ref().is_some_and(|transport| {
                    transport.peer_id == item.peer_device_id
                        && transport.display_name == item.peer_name
                        && transport.address.parse::<SocketAddr>().ok()
                            == Some(item.candidate_address)
                        && transport.certificate_fingerprint
                            == item.observed_certificate_fingerprint
                })
        } else {
            item.peer_transport.is_none()
        };
        if pairing_id != &item.pairing_id
            || pairing_id != &item.session.invitation().invitation_id
            || item.peer_device_id == local_device_id
            || item.expires_at_unix_ms != item.session.invitation().expires_at_unix_ms
            || item.peer_name.trim().is_empty()
            || item.peer_name.len() > 80
            || validate_concrete_address(item.candidate_address).is_err()
            || !valid_lowercase_digest(&item.observed_certificate_fingerprint)
            || validate_session_roles(&item.session).is_err()
            || item
                .session
                .validate_exchange(item.expires_at_unix_ms.saturating_sub(1))
                .is_err()
            || !direction_matches
            || !failure_matches
            || !completion_matches
            || (item.state == NetworkPairingStatus::AwaitingLocalConfirmation
                && item.local_confirmed)
            || (item.state == NetworkPairingStatus::AwaitingPeerConfirmation
                && !item.local_confirmed)
        {
            return Err(CoreError::InvalidState(
                "invalid retained network pairing item".to_owned(),
            ));
        }
    }
    Ok(())
}

fn valid_lowercase_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn expire_items(state: &mut PersistedNetworkPairingState, now_unix_ms: u64) -> bool {
    let mut changed = false;
    for item in state.items.values_mut() {
        if item.expires_at_unix_ms <= now_unix_ms
            && !matches!(
                item.state,
                NetworkPairingStatus::Complete | NetworkPairingStatus::Failed
            )
        {
            item.state = NetworkPairingStatus::Failed;
            item.failure_code = Some("pairing_expired".to_owned());
            item.failure_message = Some("The pairing request expired.".to_owned());
            changed = true;
        }
    }
    changed
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use covalent_core::EngineOptions;
    use tempfile::tempdir;

    use super::*;
    use crate::transport::TlsIdentity;

    #[test]
    fn candidate_route_policy_accepts_lan_tailnet_and_ipv6_but_gates_public() {
        for candidate in [
            "192.168.1.20:8787",
            "100.100.20.30:8787",
            "[fd7a:115c:a1e0::10]:8787",
            "[fe80::10]:8787",
        ] {
            assert!(
                validate_resolved_candidate(candidate.parse().expect("address"), false).is_ok(),
                "{candidate}"
            );
        }
        let public: SocketAddr = "8.8.8.8:8787".parse().expect("public");
        assert!(validate_resolved_candidate(public, false).is_err());
        assert!(validate_resolved_candidate(public, true).is_ok());
        assert!(
            validate_resolved_candidate("0.0.0.0:8787".parse().expect("unspecified"), true)
                .is_err()
        );
    }

    /// A transport binding only validates against the engine's own configured
    /// device name, so tests must name the engine rather than the binding alone.
    fn named_engine(directory: &std::path::Path, name: &str) -> Arc<Engine> {
        Arc::new(
            Engine::open(EngineOptions {
                initial_device_name: name.to_owned(),
                ..EngineOptions::new(directory)
            })
            .expect("engine"),
        )
    }

    fn binding(
        engine: &Engine,
        tls: &TlsIdentity,
        address: SocketAddr,
        name: &str,
    ) -> TransportBinding {
        TransportBinding {
            peer_id: engine.device_id(),
            display_name: name.to_owned(),
            address: address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(tls.certificate_der()),
            certificate_fingerprint: tls.certificate_fingerprint(),
        }
    }

    #[test]
    fn mutual_consent_survives_restart_and_retains_complete_item() {
        let first_dir = tempdir().expect("first");
        let second_dir = tempdir().expect("second");
        let first = named_engine(first_dir.path(), "First");
        let second = named_engine(second_dir.path(), "Second");
        let first_tls = TlsIdentity::load_or_create(first_dir.path().join("tls")).expect("TLS");
        let second_tls = TlsIdentity::load_or_create(second_dir.path().join("tls")).expect("TLS");
        let first_address: SocketAddr = "127.0.0.1:41001".parse().expect("address");
        let second_address: SocketAddr = "127.0.0.1:41002".parse().expect("address");
        let first_binding = binding(&first, &first_tls, first_address, "First");
        let second_binding = binding(&second, &second_tls, second_address, "Second");
        let invitation = second
            .pairing_manager()
            .create_invitation_with_transport(
                1_000,
                60_000,
                vec![second_address.to_string()],
                second_binding,
            )
            .expect("invitation");
        let first_state = first_dir.path().join("network-pairing.json");
        let second_state = second_dir.path().join("network-pairing.json");
        let first_manager = NetworkPairingManager::open(Arc::clone(&first), first_state.clone())
            .expect("first manager");
        let second_manager = NetworkPairingManager::open(Arc::clone(&second), second_state.clone())
            .expect("second manager");
        let session = first_manager
            .register_outgoing(
                second_address,
                second_tls.certificate_der(),
                invitation,
                first_binding,
                1_001,
            )
            .expect("outgoing");
        let pairing_id = session.invitation().invitation_id.clone();
        second_manager
            .register_incoming(first_address, first_tls.certificate_der(), session, 1_002)
            .expect("incoming");
        let code = first_manager.items(1_003).expect("items")[0]
            .authentication_string
            .clone();
        let responder_update = first_manager
            .confirm_local(&pairing_id, &code, 1_004)
            .expect("responder confirm");
        second_manager
            .merge_peer_session(&pairing_id, &responder_update, 1_005)
            .expect("merge responder");
        let inviter_update = second_manager
            .confirm_local(&pairing_id, &code, 1_006)
            .expect("inviter confirm");
        first_manager
            .merge_peer_session(&pairing_id, &inviter_update, 1_007)
            .expect("merge inviter");
        assert_eq!(
            first_manager.items(1_008).expect("items")[0].state,
            NetworkPairingStatus::Complete
        );
        assert_eq!(
            second_manager.items(1_008).expect("items")[0].state,
            NetworkPairingStatus::Complete
        );
        drop(first_manager);
        drop(second_manager);
        let reopened_first =
            NetworkPairingManager::open(Arc::clone(&first), first_state).expect("reopen first");
        let reopened_second =
            NetworkPairingManager::open(Arc::clone(&second), second_state).expect("reopen second");
        assert_eq!(
            reopened_first.items(1_009).expect("items")[0].state,
            NetworkPairingStatus::Complete
        );
        assert_eq!(
            reopened_second.items(1_009).expect("items")[0].state,
            NetworkPairingStatus::Complete
        );
    }

    #[test]
    fn tampered_retained_fingerprint_is_rejected() {
        let first_dir = tempdir().expect("first");
        let second_dir = tempdir().expect("second");
        let first = named_engine(first_dir.path(), "First");
        let second = named_engine(second_dir.path(), "Second");
        let first_tls = TlsIdentity::load_or_create(first_dir.path().join("tls")).expect("TLS");
        let second_tls = TlsIdentity::load_or_create(second_dir.path().join("tls")).expect("TLS");
        let first_address: SocketAddr = "127.0.0.1:42001".parse().expect("address");
        let second_address: SocketAddr = "127.0.0.1:42002".parse().expect("address");
        let invitation = second
            .pairing_manager()
            .create_invitation_with_transport(
                1_000,
                60_000,
                vec![second_address.to_string()],
                binding(&second, &second_tls, second_address, "Second"),
            )
            .expect("invitation");
        let state_path = first_dir.path().join("network-pairing.json");
        let manager =
            NetworkPairingManager::open(Arc::clone(&first), state_path.clone()).expect("manager");
        manager
            .register_outgoing(
                second_address,
                second_tls.certificate_der(),
                invitation,
                binding(&first, &first_tls, first_address, "First"),
                1_001,
            )
            .expect("outgoing");
        drop(manager);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).expect("read")).expect("JSON");
        let item = value
            .get_mut("items")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|items| items.values_mut().next())
            .expect("item");
        item["observedCertificateFingerprint"] = serde_json::Value::String("A".repeat(64));
        std::fs::write(
            &state_path,
            serde_json::to_vec_pretty(&value).expect("encode"),
        )
        .expect("tamper");
        assert!(NetworkPairingManager::open(first, state_path).is_err());
    }

    #[test]
    fn signed_wire_request_verifies_once_and_rejects_replay_skew_and_tampering() {
        let dir = tempdir().expect("dir");
        let engine = Arc::new(Engine::open(EngineOptions::new(dir.path())).expect("engine"));
        let manager = NetworkPairingManager::open(
            Arc::clone(&engine),
            dir.path().join("network-pairing.json"),
        )
        .expect("manager");
        let now = 1_700_000_000_000_u64;

        let request = manager
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign");
        assert_eq!(request.schema_version, NETWORK_PAIRING_SCHEMA_VERSION);
        assert_eq!(request.requester.device_id, engine.device_id());
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(&request.request_id)
                .expect("nonce")
                .len(),
            16
        );

        // A fresh, correctly signed request is accepted exactly once.
        manager
            .verify_and_consume_wire_request(&request, now)
            .expect("first verification");
        assert!(
            manager
                .verify_and_consume_wire_request(&request, now)
                .is_err(),
            "consumed request nonce must not be replayable"
        );

        // A second request outside the accepted clock skew is rejected.
        let stale = manager
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign stale");
        assert!(
            manager
                .verify_and_consume_wire_request(&stale, now + NETWORK_PAIRING_REQUEST_SKEW_MS + 1)
                .is_err(),
            "request outside the skew window must be rejected"
        );

        // Swapping the operation under a valid signature is rejected.
        let mut tampered = manager
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign tampered");
        tampered.operation = NetworkPairingWireOperation::Cancel {
            pairing_id: "attacker".to_owned(),
        };
        assert!(
            manager
                .verify_and_consume_wire_request(&tampered, now)
                .is_err(),
            "operation is covered by the request signature"
        );
    }
}
