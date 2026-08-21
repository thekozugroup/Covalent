//! Durable user-consent state for discovery-mediated network pairing.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
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
/// Ceiling on the shared nonce table one source may occupy at once.
///
/// Sized at an eighth of the table so no single source can crowd out the rest,
/// and well above what a chatty legitimate peer needs: a pairing that polls its
/// peer twice a second for the whole skew window lands near six hundred, and a
/// source that does exceed this recycles its own oldest slot instead of being
/// refused. See [`admit_request_nonce`] for why that recycling is safe.
const MAX_CONSUMED_NONCES_PER_SOURCE: usize = 512;
/// Floor under which a bucket is never an eviction candidate, whatever the fair
/// share works out to. A real pairing consumes a handful of nonces, so a bucket
/// this small is a peer doing nothing wrong and its replay protection stays
/// untouchable no matter how many sources appear. See [`source_nonce_budget`].
const MIN_FAIR_NONCE_SHARE_PER_SOURCE: usize = 16;
/// Bucket label for a request with no attributable network source.
pub(crate) const LOCAL_RATE_LIMIT_BUCKET: &str = "local";

/// Reads the durable replay floor back as the value admission is checked against.
///
/// The durable floor and the effective floor are deliberately two numbers. The
/// durable one only ever increases, so a clock stepping backwards can never
/// erase the high-water mark a previous run established. The effective one may
/// be clamped downwards *for this process only*, and the clamped value is never
/// written back, because a floor that has run away into the future refuses every
/// request the skew check would otherwise accept and bricks pairing permanently.
///
/// The clamp fires only when the stored floor is more than one skew window ahead
/// of now, and that condition is what makes it free rather than a concession:
///
/// * If the stored floor is at most `now + SKEW`, no clamp happens at all and
///   admission is checked against the full durable high-water mark. This covers
///   every backwards step smaller than one skew window — precisely the range in
///   which a request captured before the restart could still be inside the skew
///   window and therefore still replayable. The bar is not lowered by a
///   microsecond there.
/// * If the stored floor is further ahead than `now + SKEW`, then every request
///   is already refused by the skew check alone (`issued_at <= now + SKEW` and
///   `issued_at > floor >= now + SKEW` cannot both hold), so *nothing* was being
///   admitted and nothing can be lost by reading the floor back as `now`.
///
/// So the clamp never admits a request the unclamped durable floor would have
/// refused. It only converts "refuses everything forever" into "refuses
/// everything issued before this process started", which is the same guarantee
/// every healthy start already provides.
const fn effective_request_floor(durable_floor_unix_ms: u64, now_unix_ms: u64) -> u64 {
    if durable_floor_unix_ms > now_unix_ms.saturating_add(NETWORK_PAIRING_REQUEST_SKEW_MS) {
        now_unix_ms
    } else {
        durable_floor_unix_ms
    }
}

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

/// The half of pairing state a crash must not lose.
///
/// Membership here is decided by one question: can this be rebuilt or safely
/// discarded after a restart?
///
/// * `items` cannot. Each one records a human comparing a short authentication
///   string on two screens and pressing confirm. Nothing on the wire can
///   reconstruct that decision, so losing it silently un-pairs devices or forces
///   a person to repeat the ceremony. It is durable.
/// * `request_floor_unix_ms` cannot, and is the price of making the nonce table
///   volatile — see [`NetworkPairingManager::open_at`]. It is one integer.
///
/// The consumed-nonce table used to live here and no longer does. It is pure
/// replay suppression for requests that expire within
/// `NETWORK_PAIRING_REQUEST_SKEW_MS`, it is refilled by traffic rather than by
/// consent, and every unauthenticated wire request grew it — which made an
/// entire ~650 KB file rewrite plus two fsyncs the standing cost of one pairing
/// packet. It now lives in [`VolatileNonceTable`], and the replay guarantee it
/// provided across a restart is carried by the floor instead.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedNetworkPairingState {
    schema_version: u16,
    items: BTreeMap<String, PersistedNetworkPairingItem>,
    /// Wire requests must be issued strictly after this instant.
    ///
    /// Advanced to the current clock reading on every start, so no request that
    /// existed before this process did can be admitted. That is what replaces
    /// per-nonce durability: the pre-restart nonces are gone, but so is every
    /// request they were protecting against.
    #[serde(default)]
    request_floor_unix_ms: u64,
    /// Read from files written before the nonce table became volatile, then
    /// dropped. Present only so an upgrade over a `deny_unknown_fields` schema
    /// does not reject a state file it wrote itself; never written again.
    #[serde(default, skip_serializing)]
    consumed_request_nonces: BTreeMap<String, u64>,
    /// Companion of `consumed_request_nonces`, accepted and dropped identically.
    #[serde(default, skip_serializing)]
    consumed_request_nonce_sources: BTreeMap<String, String>,
}

impl Default for PersistedNetworkPairingState {
    fn default() -> Self {
        Self {
            schema_version: NETWORK_PAIRING_SCHEMA_VERSION,
            items: BTreeMap::new(),
            request_floor_unix_ms: 0,
            consumed_request_nonces: BTreeMap::new(),
            consumed_request_nonce_sources: BTreeMap::new(),
        }
    }
}

/// Replay suppression for the lifetime of one process, and nothing else.
///
/// Deliberately never written to disk. Every entry expires inside one skew
/// window, and a restart cannot leave a stale one behind because a restart
/// leaves none at all — the floor in [`PersistedNetworkPairingState`] refuses
/// anything old enough to have been protected by an entry this table lost.
#[derive(Debug, Default)]
struct VolatileNonceTable {
    nonces: BTreeMap<String, u64>,
    /// Bucket that admitted each nonce, keyed identically to `nonces`.
    /// Attribution is what makes source-fair eviction possible.
    sources: BTreeMap<String, String>,
}

/// Crash-safe coordinator for the mutually confirmed pairing transcript.
pub struct NetworkPairingManager {
    engine: Arc<Engine>,
    state_path: PathBuf,
    state: Mutex<PersistedNetworkPairingState>,
    /// Held separately from `state` so admitting a nonce never touches the
    /// durable file and never contends with a consent transition. No path takes
    /// both locks, so the split introduces no ordering hazard.
    nonces: Mutex<VolatileNonceTable>,
    request_floor_unix_ms: u64,
}

impl NetworkPairingManager {
    /// Opens retained pairing requests against the system clock.
    pub fn open(engine: Arc<Engine>, state_path: PathBuf) -> Result<Self, CoreError> {
        Self::open_at(engine, state_path, system_now_unix_ms())
    }

    /// Opens retained pairing requests at an explicit clock reading.
    ///
    /// # The replay floor
    ///
    /// Making the nonce table volatile would, on its own, be a replay hole, and
    /// the hole is worth spelling out because it is the whole reason this
    /// parameter exists. A request stays inside the skew window for
    /// `NETWORK_PAIRING_REQUEST_SKEW_MS` either side of its stamp. Consume its
    /// nonce, crash a second later, and restart with an empty table, and the
    /// captured request is admissible all over again for the rest of that
    /// window. Nonces were persisted precisely to stop that.
    ///
    /// The floor closes the same window without per-request durability. Every
    /// start records the current instant and refuses any request not issued
    /// strictly after it. A request that existed before this process started is
    /// therefore dead on arrival, whether or not its nonce survived — which is a
    /// strictly stronger statement than the old table made, since the table only
    /// remembered requests this node had actually seen, while the floor also
    /// covers ones captured from another peer's traffic.
    ///
    /// Two details keep it honest, and they are two *separate* numbers on
    /// purpose — folding them into one expression is what previously let a
    /// backwards clock lower the durable bar by a whole skew window and write
    /// the lowered value straight back to disk:
    ///
    /// * The stored value only ever moves forward (`max` against the previous
    ///   floor), so a clock stepping backwards — NTP correction, a device with
    ///   no battery-backed clock — cannot lower the bar. Nothing writes a
    ///   smaller number than the one already on disk, and `floor_advanced` is a
    ///   strict increase, so a backwards clock performs no write at all.
    /// * The value admission is checked against is derived from the stored one
    ///   by [`effective_request_floor`], which clamps only in the region where
    ///   the stored floor already refuses everything anyway. That clamp is never
    ///   persisted, and it never admits a request the unclamped floor would have
    ///   refused; see that function for the argument.
    ///
    /// The cost is stated rather than hidden: for the first moments after a
    /// restart, a peer whose clock trails this node's is refused until its own
    /// stamps pass the floor. It resigns each request with a fresh stamp, so the
    /// condition clears on its own within the peers' clock offset, and refusing
    /// briefly is the safe direction to fail.
    pub fn open_at(
        engine: Arc<Engine>,
        state_path: PathBuf,
        now_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        let mut state = match std::fs::symlink_metadata(&state_path) {
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
        // Nonces retained by an older build are volatile now. Drop them before
        // validation so a file written by that build neither has to satisfy
        // rules that no longer apply nor keeps memory alive for a table this
        // process rebuilds from zero.
        state.consumed_request_nonces = BTreeMap::new();
        state.consumed_request_nonce_sources = BTreeMap::new();
        validate_state(&state, engine.device_id())?;

        // The durable value moves in exactly one direction. Nothing below is
        // allowed to write a smaller number than the one already on disk.
        let durable_floor_unix_ms = state.request_floor_unix_ms.max(now_unix_ms);
        let floor_advanced = durable_floor_unix_ms > state.request_floor_unix_ms;
        state.request_floor_unix_ms = durable_floor_unix_ms;
        let request_floor_unix_ms = effective_request_floor(durable_floor_unix_ms, now_unix_ms);

        let manager = Self {
            engine,
            state_path,
            state: Mutex::new(state),
            nonces: Mutex::new(VolatileNonceTable::default()),
            request_floor_unix_ms,
        };
        if floor_advanced {
            // The one unavoidable durable write per process start, and the
            // anchor every later request is checked against. It must land
            // before a single request is served, so it is not deferred.
            let state = manager
                .state
                .lock()
                .map_err(|_| CoreError::Synchronization)?;
            manager.persist_locked(&state)?;
        }
        Ok(manager)
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

    /// Verifies freshness/signature and consumes a request nonce before dispatch.
    ///
    /// `source` is the address the request actually arrived from, already proven
    /// reachable by QUIC address validation before this node spent a signature
    /// verification on it. It selects the rate-limiting bucket only; every
    /// freshness, signature, operation-binding and replay check below is
    /// identical for every caller and is never scoped to a bucket.
    ///
    /// Consumption writes nothing to disk. Single-use is enforced against the
    /// in-memory table for the life of this process, and requests predating the
    /// process are refused outright by the replay floor established in
    /// [`Self::open_at`], which together cover the same ground the persisted
    /// nonce table used to — at zero writes per request instead of a full state
    /// file and two fsyncs.
    pub fn verify_and_consume_wire_request(
        &self,
        request: &NetworkPairingWireRequest,
        now_unix_ms: u64,
        source: Option<IpAddr>,
    ) -> Result<(), CoreError> {
        if request.schema_version != NETWORK_PAIRING_SCHEMA_VERSION
            || request.issued_at_unix_ms <= self.request_floor_unix_ms
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
        let expires_at_unix_ms = request
            .issued_at_unix_ms
            .saturating_add(NETWORK_PAIRING_REQUEST_SKEW_MS);
        let mut nonces = self.nonces.lock().map_err(|_| CoreError::Synchronization)?;
        admit_request_nonce(
            &mut nonces,
            nonce_key,
            expires_at_unix_ms,
            &rate_limit_bucket_key(source),
            now_unix_ms,
        )
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

/// Names the rate-limiting bucket one request address belongs to.
///
/// IPv4 addresses are individually scarce on the local networks pairing runs on,
/// so each one funds its own budget. A single IPv6 host is routinely handed a
/// whole /64, so the prefix — not the address — is the unit an attacker cannot
/// cheaply multiply; budgeting per address there would hand one host billions of
/// free buckets.
///
/// Shared with the pairing probe budget rather than duplicated there, so both
/// limits agree on what counts as one source and an attacker cannot escape one
/// by exploiting a looser definition in the other.
pub(crate) fn rate_limit_bucket_key(source: Option<IpAddr>) -> String {
    match source {
        Some(IpAddr::V4(address)) => address.to_string(),
        Some(IpAddr::V6(address)) => {
            let mut prefix = [0_u8; 16];
            prefix[..8].copy_from_slice(&address.octets()[..8]);
            format!("{}/64", Ipv6Addr::from(prefix))
        }
        None => LOCAL_RATE_LIMIT_BUCKET.to_owned(),
    }
}

fn system_now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Admits one freshly verified request nonce under a source-partitioned budget.
///
/// # Eviction versus replay protection
///
/// This is the crux of the source-keyed limiting, so the reasoning is written
/// down rather than implied.
///
/// Replay protection is global and stays global. The same signed request can be
/// replayed from *any* address, so partitioning the uniqueness *lookup* by
/// source would be a replay hole outright: an attacker would capture a request,
/// send it from a second address whose partition is empty, and be admitted. The
/// `contains_key` check below therefore runs against the whole table, exactly as
/// it did before this function existed, and is reached by every caller before
/// any budget is consulted.
///
/// Only *eviction* is partitioned, under a single invariant: **an entry is
/// evicted only from a bucket that is at or above its own per-source budget, and
/// the soonest-expiring entry in that bucket goes first.** That budget is a fair
/// share of the table rather than a fixed ceiling — see [`source_nonce_budget`]
/// for why a fixed one denied service at nine sources — but it is floored at
/// [`MIN_FAIR_NONCE_SHARE_PER_SOURCE`], and the invariant is stated against the
/// floor: a bucket at or below the minimum share is never an eviction candidate,
/// however many other sources appear. Two consequences make that safe:
///
/// * A source evicting its own entry gains nothing. It holds the signing key for
///   those requests and can mint a fresh nonce whenever it likes, so recovering
///   the ability to replay a request it already consumed is not a new
///   capability — it is a slower way to do what it could already do.
/// * A bucket below its budget is untouchable by anyone. A peer behaving
///   normally — a handful of nonces per pairing, far under the sixteen-entry
///   minimum share — can never have an entry dropped by another source, so its
///   replay protection is byte-for-byte what it was before this change.
///
/// The residual cost is stated plainly rather than hidden, with the real number
/// rather than a flattering one: when the table is full and *every* bucket sits
/// below its fair share, admission is refused instead of evicting from a
/// well-behaved peer. With a 4096-entry table and a 16-entry minimum share that
/// takes **274 distinct address-validated sources**, each holding fewer than
/// sixteen unexpired nonces, and
/// `a_bucket_within_the_minimum_fair_share_is_never_evicted_even_under_global_pressure`
/// pins that threshold so the claim cannot rot again. The alternative would
/// trade a throughput problem for a replay hole in a peer that did nothing
/// wrong. Denial is the safer failure, so denial is what happens.
///
/// The table is process-local, so it starts empty rather than restored: a
/// restart during a flood now begins with nothing to evict at all, and the
/// requests that flood was replaying are refused by the replay floor.
fn admit_request_nonce(
    table: &mut VolatileNonceTable,
    nonce_key: String,
    expires_at_unix_ms: u64,
    source: &str,
    now_unix_ms: u64,
) -> Result<(), CoreError> {
    let VolatileNonceTable {
        nonces,
        sources: buckets,
    } = table;
    nonces.retain(|_, expires_at| *expires_at > now_unix_ms);
    buckets.retain(|key, _| nonces.contains_key(key));

    // Global single-consumption, checked first and never scoped to a bucket.
    if nonces.contains_key(&nonce_key) {
        return Err(CoreError::AuthenticationFailed);
    }

    let budget = source_nonce_budget(buckets);
    if bucket_len(buckets, source) >= budget {
        evict_soonest_expiring(nonces, buckets, source);
    }
    if nonces.len() >= MAX_CONSUMED_REQUEST_NONCES {
        let crowder = noisiest_bucket_at_budget(buckets, budget)
            .ok_or(CoreError::ResourceLimit("pairing request nonces"))?;
        evict_soonest_expiring(nonces, buckets, &crowder);
    }
    if nonces.len() >= MAX_CONSUMED_REQUEST_NONCES {
        return Err(CoreError::ResourceLimit("pairing request nonces"));
    }
    nonces.insert(nonce_key.clone(), expires_at_unix_ms);
    buckets.insert(nonce_key, source.to_owned());
    Ok(())
}

/// The share of the table one source may hold before its own entries become
/// eviction candidates.
///
/// A fixed ceiling is what made this budget misbehave. At 512 entries against a
/// 4096-entry table, nine buckets of roughly 455 could fill the table without a
/// single one reaching the ceiling, leaving no legal eviction candidate and
/// refusing admission to everyone — the nine incumbents included. Nine ordinary
/// LAN peers is not a flood, and denying them was not a considered tradeoff.
///
/// The share therefore narrows as sources appear: every bucket may hold up to
/// `MAX_CONSUMED_REQUEST_NONCES / bucket_count`, capped at the original ceiling
/// so one source can never take more of the table than it could before, and
/// floored at [`MIN_FAIR_NONCE_SHARE_PER_SOURCE`] so a peer holding a real
/// pairing's worth of nonces is never an eviction candidate at all.
///
/// The eviction invariant is unchanged in substance — an entry is only ever
/// evicted from a bucket at or above its own budget — and the floor is what
/// keeps it meaningful: a bucket at or below the minimum share is untouchable
/// no matter how many other sources appear. The residual denial is still real
/// but now needs the number the rationale above claims: with a 4096-entry table
/// and a 16-entry floor it takes 274 distinct address-validated sources, each
/// holding fewer than 16 unexpired nonces, to leave no candidate and refuse.
fn source_nonce_budget(buckets: &BTreeMap<String, String>) -> usize {
    let sources: BTreeSet<&str> = buckets.values().map(String::as_str).collect();
    let fair_share = MAX_CONSUMED_REQUEST_NONCES
        .checked_div(sources.len())
        .unwrap_or(MAX_CONSUMED_NONCES_PER_SOURCE);
    fair_share.clamp(
        MIN_FAIR_NONCE_SHARE_PER_SOURCE,
        MAX_CONSUMED_NONCES_PER_SOURCE,
    )
}

fn bucket_len(buckets: &BTreeMap<String, String>, source: &str) -> usize {
    buckets
        .values()
        .filter(|bucket| bucket.as_str() == source)
        .count()
}

/// Drops one bucket's soonest-expiring entry, so the slot recovered is the one
/// whose replay window had least left to run.
fn evict_soonest_expiring(
    nonces: &mut BTreeMap<String, u64>,
    buckets: &mut BTreeMap<String, String>,
    source: &str,
) {
    let victim = buckets
        .iter()
        .filter(|(_, bucket)| bucket.as_str() == source)
        .filter_map(|(key, _)| nonces.get(key).map(|expires_at| (*expires_at, key.clone())))
        .min();
    if let Some((_, key)) = victim {
        nonces.remove(&key);
        buckets.remove(&key);
    }
}

/// The widest bucket, considered only once it has spent its own budget. Buckets
/// under budget are never eviction candidates, which is what keeps a quiet
/// peer's replay protection intact under someone else's flood. `budget` is the
/// fair share computed by [`source_nonce_budget`] for the table as it stands.
fn noisiest_bucket_at_budget(buckets: &BTreeMap<String, String>, budget: usize) -> Option<String> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for bucket in buckets.values() {
        *counts.entry(bucket.as_str()).or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|&(_, count)| count >= budget)
        .max_by_key(|&(source, count)| (count, source))
        .map(|(source, _)| source.to_owned())
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
    // The consumed-nonce maps are not checked here: `open_at` empties them
    // before this runs, because they are volatile state that no longer round
    // trips through the file at all.
    if state.schema_version != NETWORK_PAIRING_SCHEMA_VERSION
        || state.items.len() > MAX_NETWORK_PAIRING_ITEMS
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
    use std::path::Path;

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
        let now = 1_700_000_000_000_u64;
        // Opened one millisecond before the requests below are issued, which is
        // what a node serving them looks like: the replay floor is behind them.
        let manager = NetworkPairingManager::open_at(
            Arc::clone(&engine),
            dir.path().join("network-pairing.json"),
            now - 1,
        )
        .expect("manager");

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
            .verify_and_consume_wire_request(
                &request,
                now,
                Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20))),
            )
            .expect("first verification");
        assert!(
            manager
                .verify_and_consume_wire_request(
                    &request,
                    now,
                    Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)))
                )
                .is_err(),
            "consumed request nonce must not be replayable"
        );
        assert!(
            manager
                .verify_and_consume_wire_request(
                    &request,
                    now,
                    Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 7))),
                )
                .is_err(),
            "source-keyed budgets must not scope replay protection to one source"
        );

        // A second request outside the accepted clock skew is rejected.
        let stale = manager
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign stale");
        assert!(
            manager
                .verify_and_consume_wire_request(
                    &stale,
                    now + NETWORK_PAIRING_REQUEST_SKEW_MS + 1,
                    Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20))),
                )
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
                .verify_and_consume_wire_request(
                    &tampered,
                    now,
                    Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)))
                )
                .is_err(),
            "operation is covered by the request signature"
        );
    }

    /// Identity of the committed state file, or `None` while it does not exist.
    ///
    /// `persist_private_file` stages into a fresh temporary file and renames it
    /// over the target, so every durable commit installs a new inode. Counting
    /// inode changes therefore counts real commits as the filesystem saw them,
    /// which a counter maintained by the code under test could not honestly do.
    fn state_file_identity(path: &std::path::Path) -> Option<(u64, u64)> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(path)
            .ok()
            .map(|metadata| (metadata.ino(), metadata.len()))
    }

    /// Counts the durable commits `body` causes.
    fn durable_commits(path: &std::path::Path, body: impl FnOnce()) -> usize {
        let before = state_file_identity(path);
        let mut commits = 0;
        let mut last = before;
        let observe = |last: &mut Option<(u64, u64)>, commits: &mut usize| {
            let now = state_file_identity(path);
            if now != *last {
                *commits += 1;
                *last = now;
            }
        };
        body();
        observe(&mut last, &mut commits);
        commits
    }

    #[test]
    fn a_flood_of_wire_requests_no_longer_rewrites_the_state_file() {
        let dir = tempdir().expect("dir");
        let engine = Arc::new(Engine::open(EngineOptions::new(dir.path())).expect("engine"));
        let path = dir.path().join("network-pairing.json");
        let now = 1_700_000_000_000_u64;
        let manager = NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now - 1)
            .expect("manager");

        // Opening advanced the replay floor, which is the one durable write a
        // process start is allowed to make.
        let opened = state_file_identity(&path).expect("floor is committed at open");

        const REQUESTS: usize = 256;
        let source = Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 99)));
        let commits = durable_commits(&path, || {
            for _ in 0..REQUESTS {
                let request = manager
                    .sign_wire_request(NetworkPairingWireOperation::Probe, now)
                    .expect("sign");
                manager
                    .verify_and_consume_wire_request(&request, now, source)
                    .expect("fresh request is admitted");
            }
        });

        assert_eq!(
            commits, 0,
            "{REQUESTS} accepted wire requests must not commit the state file even once"
        );
        assert_eq!(
            state_file_identity(&path),
            Some(opened),
            "neither the file's identity nor its length may move under a flood"
        );
    }

    #[test]
    fn a_restart_keeps_consent_durable_and_closes_the_replay_window_it_opens() {
        let dir = tempdir().expect("dir");
        let engine = Arc::new(Engine::open(EngineOptions::new(dir.path())).expect("engine"));
        let path = dir.path().join("network-pairing.json");
        let now = 1_700_000_000_000_u64;
        let manager = NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now - 1)
            .expect("manager");

        let captured = manager
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign");
        manager
            .verify_and_consume_wire_request(&captured, now, None)
            .expect("first use is admitted");

        // Crash and restart one second later, well inside the skew window that
        // still makes the captured request look fresh on its face.
        drop(manager);
        let restart = now + 1_000;
        let reopened = NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), restart)
            .expect("reopen");

        // The nonce table is gone, so this is exactly the replay the old
        // persisted table existed to refuse. The floor refuses it instead.
        assert!(
            reopened
                .verify_and_consume_wire_request(&captured, restart, None)
                .is_err(),
            "a request consumed before the restart must stay dead after it"
        );
        // Not merely because its own nonce was remembered: a request this node
        // never saw, captured from someone else's traffic before the restart,
        // is refused too.
        let unseen = reopened
            .sign_wire_request(NetworkPairingWireOperation::Probe, now)
            .expect("sign unseen");
        assert!(
            reopened
                .verify_and_consume_wire_request(&unseen, restart, None)
                .is_err(),
            "the floor must refuse anything issued before this process started"
        );
        // And pairing still works: a request issued after the restart is served.
        let fresh = reopened
            .sign_wire_request(NetworkPairingWireOperation::Probe, restart + 1)
            .expect("sign fresh");
        reopened
            .verify_and_consume_wire_request(&fresh, restart + 1, None)
            .expect("a request issued after the restart must be admitted");
    }

    #[test]
    fn the_replay_floor_never_moves_backwards_but_recovers_from_a_forward_glitch() {
        let dir = tempdir().expect("dir");
        let engine = Arc::new(Engine::open(EngineOptions::new(dir.path())).expect("engine"));
        let path = dir.path().join("network-pairing.json");
        let now = 1_700_000_000_000_u64;

        NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now).expect("first");
        // A clock stepping backwards must not lower the bar, or every request
        // the earlier run already consumed becomes admissible again.
        let stepped_back =
            NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now - 60_000)
                .expect("reopen with a backwards clock");
        assert_eq!(
            stepped_back.request_floor_unix_ms, now,
            "the floor is monotonic across restarts"
        );
        drop(stepped_back);

        // A single forward glitch must not brick pairing once the clock is
        // corrected: the stored floor is read back clamped to one skew window
        // ahead, which is already beyond anything the skew check would accept.
        let glitch = now + 400 * 24 * 60 * 60 * 1_000;
        NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), glitch)
            .expect("reopen with a glitched clock");
        let corrected =
            NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now + 1_000)
                .expect("reopen with a corrected clock");
        assert_eq!(
            corrected.request_floor_unix_ms,
            now + 1_000,
            "a glitched floor is clamped back to the present so pairing is not bricked"
        );
        assert_eq!(
            durable_request_floor(&path),
            glitch,
            "the clamp is an admission-time reading only; the durable high-water mark is untouched"
        );
    }

    /// Reads the floor as it exists on disk, which is the only value a later
    /// process start can inherit. Asserting the in-memory effective floor alone
    /// cannot see a durable regression, which is how the original guard passed
    /// while the durable value was being lowered underneath it.
    fn durable_request_floor(path: &Path) -> u64 {
        let bytes = std::fs::read(path).expect("read persisted pairing state");
        serde_json::from_slice::<PersistedNetworkPairingState>(&bytes)
            .expect("parse persisted pairing state")
            .request_floor_unix_ms
    }

    #[test]
    fn a_clock_stepped_back_beyond_the_skew_window_cannot_lower_the_durable_floor() {
        let dir = tempdir().expect("dir");
        let engine = Arc::new(Engine::open(EngineOptions::new(dir.path())).expect("engine"));
        let path = dir.path().join("network-pairing.json");
        let now = 1_700_000_000_000_u64;

        NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), now).expect("first");
        assert_eq!(durable_request_floor(&path), now);

        // Sixty seconds back sits inside the skew window, which is the one
        // region where a read-back clamp against `now + SKEW` cannot bite. Ten
        // minutes back is outside it, and that is where the clamp used to
        // overrule the monotonic `max` and write the lowered value to disk.
        let stepped_back = now - 10 * 60 * 1_000;
        drop(
            NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), stepped_back)
                .expect("reopen with a backwards clock"),
        );
        assert_eq!(
            durable_request_floor(&path),
            now,
            "a backwards clock must never write a lowered floor to disk"
        );

        // The consequence that matters, not just the stored number: after the
        // clock partially corrects, a request stamped below the preserved
        // high-water mark is still refused. With the lowered floor on disk this
        // request is comfortably inside the admissible window.
        let partially_corrected = now - 200_000;
        let restored =
            NetworkPairingManager::open_at(Arc::clone(&engine), path.clone(), partially_corrected)
                .expect("reopen with a partially corrected clock");
        assert_eq!(
            restored.request_floor_unix_ms, now,
            "the preserved high-water mark is what admission is checked against"
        );
        let stale = restored
            .sign_wire_request(NetworkPairingWireOperation::Probe, now - 100_000)
            .expect("sign stale");
        assert!(
            restored
                .verify_and_consume_wire_request(&stale, partially_corrected, None)
                .is_err(),
            "a request stamped below the preserved floor must stay refused"
        );

        // Refusing everything is not the goal: a request stamped above the
        // preserved floor and inside the skew window is still served.
        let fresh = restored
            .sign_wire_request(NetworkPairingWireOperation::Probe, now + 1)
            .expect("sign fresh");
        restored
            .verify_and_consume_wire_request(&fresh, partially_corrected, None)
            .expect("a request above the preserved floor must still be admitted");
    }

    fn fresh_nonce_state() -> (VolatileNonceTable, u64, u64) {
        let now = 1_700_000_000_000_u64;
        (
            VolatileNonceTable::default(),
            now,
            now + NETWORK_PAIRING_REQUEST_SKEW_MS,
        )
    }

    #[test]
    fn one_source_flooding_the_nonce_table_cannot_deny_another_source() {
        let (mut state, now, expiry) = fresh_nonce_state();

        // A quiet peer consumes one nonce, the way a real pairing does.
        admit_request_nonce(
            &mut state,
            "quiet:0".to_owned(),
            expiry,
            "192.168.1.20",
            now,
        )
        .expect("quiet peer admitted");

        // The flood runs twice past the whole table's capacity. Before
        // source-keyed budgets this filled the shared table and every later
        // request — from anyone — was refused for the length of the skew window.
        for index in 0..MAX_CONSUMED_REQUEST_NONCES * 2 {
            admit_request_nonce(
                &mut state,
                format!("flood:{index}"),
                expiry,
                "192.168.1.99",
                now,
            )
            .expect("a flooding source is absorbed by its own budget, never refused");
            assert!(
                state.nonces.len() <= MAX_CONSUMED_REQUEST_NONCES,
                "the table must stay inside its global bound"
            );
        }

        // The flood is pinned to its own budget rather than the whole table.
        assert_eq!(
            bucket_len(&state.sources, "192.168.1.99"),
            MAX_CONSUMED_NONCES_PER_SOURCE,
            "one source must never hold more than its per-source budget"
        );
        assert!(
            state.nonces.len() <= MAX_CONSUMED_NONCES_PER_SOURCE + 1,
            "a single flooding source must leave the rest of the table free"
        );

        // The quiet peer's nonce survived every one of those admissions, so its
        // replay protection is exactly what it was before the flood started.
        assert!(
            state.nonces.contains_key("quiet:0"),
            "a flood must never evict a quiet peer's unexpired nonce"
        );

        // And a third source pairs normally while the flood is still at budget.
        for index in 0..64 {
            admit_request_nonce(
                &mut state,
                format!("legit:{index}"),
                expiry,
                "192.168.1.30",
                now,
            )
            .expect("a legitimate peer still pairs during someone else's flood");
        }
        assert!(state.nonces.contains_key("quiet:0"));
    }

    #[test]
    fn replay_protection_stays_global_across_buckets_and_over_budget_sources() {
        let (mut state, now, expiry) = fresh_nonce_state();

        admit_request_nonce(&mut state, "peer:one".to_owned(), expiry, "10.0.0.5", now)
            .expect("first consumption");

        // The same nonce replayed from a different source, whose bucket is
        // empty, must still be refused: partitioning the uniqueness lookup would
        // be a replay hole, so only eviction is partitioned.
        assert!(
            admit_request_nonce(&mut state, "peer:one".to_owned(), expiry, "10.0.0.6", now)
                .is_err(),
            "a consumed nonce must stay dead no matter which source replays it"
        );
        assert_eq!(
            state.sources.get("peer:one"),
            Some(&"10.0.0.5".to_owned()),
            "a refused replay must not re-attribute the incumbent entry"
        );

        // Driving one source past its budget evicts only that source's own
        // entries, so the other bucket's replay protection is untouched.
        for index in 0..MAX_CONSUMED_NONCES_PER_SOURCE * 2 {
            admit_request_nonce(
                &mut state,
                format!("noisy:{index}"),
                expiry,
                "10.0.0.6",
                now,
            )
            .expect("self-eviction keeps the noisy source served");
        }
        assert!(
            admit_request_nonce(&mut state, "peer:one".to_owned(), expiry, "10.0.0.6", now)
                .is_err(),
            "an over-budget source must not be able to reopen someone else's nonce"
        );
    }

    #[test]
    fn nine_moderately_busy_sources_cannot_deny_everyone_including_themselves() {
        let (mut state, now, expiry) = fresh_nonce_state();

        // Nine buckets of roughly 455 entries fill the whole 4096-entry table
        // while every one of them stays under the fixed 512-entry ceiling. That
        // is not a flood; it is nine ordinary LAN peers. Under a fixed
        // per-source budget no bucket was ever an eviction candidate, so the
        // table locked solid and refused *every* source — the nine incumbents
        // included. The fair share makes each of them evictable from its own
        // over-share, which is the only reason admission survives here.
        let sources = MAX_CONSUMED_REQUEST_NONCES / MAX_CONSUMED_NONCES_PER_SOURCE + 1;
        assert_eq!(sources, 9, "the arithmetic this test pins has not moved");
        for index in 0..MAX_CONSUMED_REQUEST_NONCES {
            admit_request_nonce(
                &mut state,
                format!("spread:{index}"),
                expiry,
                &format!("10.1.0.{}", index % sources),
                now,
            )
            .expect("filling the table below every fixed ceiling");
        }
        // Each bucket self-caps at its fair share of 455, so nine of them hold
        // 4095 and the table never reaches the wedged state at all. Under the
        // fixed 512-entry ceiling the same nine filled all 4096 slots with no
        // bucket at budget, and that is the configuration that refused
        // everyone.
        let fair_share = MAX_CONSUMED_REQUEST_NONCES / sources;
        assert_eq!(fair_share, 455);
        assert_eq!(state.nonces.len(), fair_share * sources);
        for bucket in 0..sources {
            assert_eq!(
                bucket_len(&state.sources, &format!("10.1.0.{bucket}")),
                fair_share,
                "every busy source is held to its share instead of racing to the ceiling"
            );
        }

        admit_request_nonce(&mut state, "newcomer:0".to_owned(), expiry, "10.2.0.1", now)
            .expect("a tenth source must not be refused by nine merely busy ones");
        assert_eq!(
            state.sources.get("newcomer:0"),
            Some(&"10.2.0.1".to_owned()),
            "the admitted entry is attributed to the source that earned it"
        );

        // And the nine incumbents are still served too — under the fixed budget
        // they had locked themselves out along with everybody else.
        for bucket in 0..sources {
            admit_request_nonce(
                &mut state,
                format!("incumbent-again:{bucket}"),
                expiry,
                &format!("10.1.0.{bucket}"),
                now,
            )
            .expect("an incumbent must not be denied by the table it helped fill");
        }
        assert!(
            state.nonces.len() <= MAX_CONSUMED_REQUEST_NONCES,
            "the global cap is never exceeded"
        );
    }

    #[test]
    fn a_bucket_within_the_minimum_fair_share_is_never_evicted_even_under_global_pressure() {
        let (mut state, now, expiry) = fresh_nonce_state();

        // A quiet peer holding a real pairing's worth of nonces, sized just
        // under the floor below which no bucket is ever an eviction candidate.
        let quiet = MIN_FAIR_NONCE_SHARE_PER_SOURCE - 1;
        for index in 0..quiet {
            admit_request_nonce(
                &mut state,
                format!("quiet:{index}"),
                expiry,
                "10.0.0.5",
                now,
            )
            .expect("a quiet peer is admitted");
        }

        // Now spread the rest of the table across enough distinct sources that
        // every bucket lands under the minimum share. This is the widest flood
        // the fair share still refuses rather than serving by evicting a
        // well-behaved peer, and it takes hundreds of address-validated sources
        // to reach — the number the rationale comment now states outright.
        let remaining = MAX_CONSUMED_REQUEST_NONCES - quiet;
        let sources = remaining.div_ceil(quiet);
        assert_eq!(
            sources + 1,
            274,
            "the refusal threshold the rationale comment states, pinned"
        );
        for index in 0..remaining {
            admit_request_nonce(
                &mut state,
                format!("spread:{index}"),
                expiry,
                &format!("10.1.{}.{}", index % sources / 256, index % sources % 256),
                now,
            )
            .expect("filling the table under every fair share");
        }
        assert_eq!(state.nonces.len(), MAX_CONSUMED_REQUEST_NONCES);

        // This is the tradeoff, asserted rather than assumed: with no bucket
        // over its fair share there is no safe eviction candidate, so admission
        // is refused instead of dropping a well-behaved peer's unexpired nonce.
        assert!(
            matches!(
                admit_request_nonce(&mut state, "newcomer:0".to_owned(), expiry, "10.2.0.1", now),
                Err(CoreError::ResourceLimit(_))
            ),
            "a full table of within-share buckets must refuse, never evict"
        );
        assert_eq!(
            state.nonces.len(),
            MAX_CONSUMED_REQUEST_NONCES,
            "a refused admission must not have evicted anything"
        );
        assert_eq!(
            bucket_len(&state.sources, "10.0.0.5"),
            quiet,
            "the quiet peer's replay protection is byte-for-byte intact"
        );

        // Once the window passes, the table drains and pairing recovers.
        let later = expiry + 1;
        admit_request_nonce(
            &mut state,
            "newcomer:0".to_owned(),
            later + NETWORK_PAIRING_REQUEST_SKEW_MS,
            "10.2.0.1",
            later,
        )
        .expect("expired nonces are pruned and capacity returns");
        assert_eq!(state.nonces.len(), 1);
        assert_eq!(state.sources.len(), 1);
    }

    #[test]
    fn rate_limit_buckets_group_ipv6_by_prefix_and_ipv4_by_address() {
        let first = rate_limit_bucket_key(Some("2001:db8::1".parse().expect("ipv6")));
        let second = rate_limit_bucket_key(Some("2001:db8::dead:beef".parse().expect("ipv6")));
        assert_eq!(
            first, second,
            "one IPv6 host holds a whole /64, so the prefix funds one budget"
        );
        assert_ne!(
            first,
            rate_limit_bucket_key(Some("2001:db9::1".parse().expect("ipv6"))),
            "a different /64 is a different budget"
        );
        assert_ne!(
            rate_limit_bucket_key(Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 20)))),
            rate_limit_bucket_key(Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 21)))),
            "IPv4 addresses are scarce enough to budget individually"
        );
        assert_eq!(rate_limit_bucket_key(None), LOCAL_RATE_LIMIT_BUCKET);
    }
}
