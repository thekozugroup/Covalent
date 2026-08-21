use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_protocol::{
    PROTOCOL_VERSION, PairingInvitation, PeerGrant, PeerRole, TransportBinding,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::atomic::{read_json_bounded, write_json_atomic};
use crate::{CoreError, DeviceIdentity, PublicIdentity};

const INVITATION_SIGNATURE_DOMAIN: &[u8] = b"covalent/pairing-invitation/v1";
const ACCEPTANCE_SIGNATURE_DOMAIN: &[u8] = b"covalent/pairing-acceptance/v1";
const RESPONDER_CONFIRMATION_DOMAIN: &[u8] = b"covalent/pairing-responder-confirmation/v1";
const INVITER_CONFIRMATION_DOMAIN: &[u8] = b"covalent/pairing-inviter-confirmation/v1";
const PAIRING_STATE_SCHEMA_VERSION: u16 = 1;
const MAX_PAIRING_STATE_BYTES: usize = 1_048_576;
const MAX_PENDING_INVITATIONS: usize = 32;
const MAX_INVITATION_LIFETIME_MS: u64 = 15 * 60 * 1_000;

/// Human-comparable authentication string derived from the secret-bound transcript.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShortAuthenticationString(String);

impl ShortAuthenticationString {
    /// Display representation grouped to reduce comparison mistakes.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ShortAuthenticationString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Transferable pairing exchange. Role requests and both explicit user
/// confirmations are cryptographically bound to the same invitation transcript.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingSession {
    invitation: PairingInvitation,
    responder_device_id: covalent_protocol::DeviceId,
    responder_public_key: String,
    responder_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    responder_transport: Option<TransportBinding>,
    responder_roles: BTreeSet<PeerRole>,
    inviter_roles: BTreeSet<PeerRole>,
    authentication_string: ShortAuthenticationString,
    responder_acceptance_signature: String,
    responder_confirmation_signature: Option<String>,
    inviter_confirmation_signature: Option<String>,
}

impl Drop for PairingSession {
    fn drop(&mut self) {
        self.invitation.invitation_secret.zeroize();
    }
}

impl PairingSession {
    /// Begins acceptance with no roles. Prefer `accept_with_roles` for a useful grant.
    pub fn accept(
        invitation: PairingInvitation,
        responder: &DeviceIdentity,
        responder_name: impl Into<String>,
        now_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        Self::accept_with_roles(
            invitation,
            responder,
            responder_name,
            BTreeSet::new(),
            BTreeSet::new(),
            now_unix_ms,
        )
    }

    /// Begins acceptance after validating the invitation and binding exact roles.
    pub fn accept_with_roles(
        invitation: PairingInvitation,
        responder: &DeviceIdentity,
        responder_name: impl Into<String>,
        responder_roles: BTreeSet<PeerRole>,
        inviter_roles: BTreeSet<PeerRole>,
        now_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        Self::accept_inner(
            invitation,
            responder,
            responder_name.into(),
            None,
            responder_roles,
            inviter_roles,
            now_unix_ms,
        )
    }

    /// Begins acceptance and binds the responder's exact TLS endpoint into the transcript.
    pub fn accept_with_transport(
        invitation: PairingInvitation,
        responder: &DeviceIdentity,
        responder_transport: TransportBinding,
        responder_roles: BTreeSet<PeerRole>,
        inviter_roles: BTreeSet<PeerRole>,
        now_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        let responder_name = responder_transport.display_name.clone();
        Self::accept_inner(
            invitation,
            responder,
            responder_name,
            Some(responder_transport),
            responder_roles,
            inviter_roles,
            now_unix_ms,
        )
    }

    fn accept_inner(
        invitation: PairingInvitation,
        responder: &DeviceIdentity,
        responder_name: String,
        responder_transport: Option<TransportBinding>,
        responder_roles: BTreeSet<PeerRole>,
        inviter_roles: BTreeSet<PeerRole>,
        now_unix_ms: u64,
    ) -> Result<Self, CoreError> {
        validate_invitation(&invitation, now_unix_ms)?;
        let inviter = invitation_identity(&invitation)?;
        inviter.verify(
            INVITATION_SIGNATURE_DOMAIN,
            &invitation_signing_bytes(&invitation)?,
            &invitation.signature,
        )?;
        if inviter.device_id == responder.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        validate_display_name(&responder_name)?;
        let responder_public = responder.public_identity();
        if let Some(binding) = responder_transport.as_ref() {
            validate_transport_binding(binding, &responder_public, &responder_name)?;
        }
        let transcript = acceptance_transcript(
            &invitation,
            &responder_public,
            &responder_name,
            responder_transport.as_ref(),
            &responder_roles,
            &inviter_roles,
        )?;
        let secret = decode_invitation_secret(&invitation)?;
        let authentication_string = authentication_string(&secret, &transcript);
        let responder_acceptance_signature =
            responder.sign(ACCEPTANCE_SIGNATURE_DOMAIN, &transcript);
        Ok(Self {
            invitation,
            responder_device_id: responder_public.device_id,
            responder_public_key: responder_public.public_key,
            responder_name,
            responder_transport,
            responder_roles,
            inviter_roles,
            authentication_string,
            responder_acceptance_signature,
            responder_confirmation_signature: None,
            inviter_confirmation_signature: None,
        })
    }

    /// Authentication string that must match on both physical devices.
    #[must_use]
    pub const fn authentication_string(&self) -> &ShortAuthenticationString {
        &self.authentication_string
    }

    /// Signed invitation carried by this exchange.
    #[must_use]
    pub const fn invitation(&self) -> &PairingInvitation {
        &self.invitation
    }

    /// Responder identity bound into the signed acceptance transcript.
    #[must_use]
    pub const fn responder_device_id(&self) -> covalent_protocol::DeviceId {
        self.responder_device_id
    }

    /// Responder display name bound into the signed acceptance transcript.
    #[must_use]
    pub fn responder_name(&self) -> &str {
        &self.responder_name
    }

    /// Responder transport identity bound into the signed acceptance transcript.
    #[must_use]
    pub const fn responder_transport(&self) -> Option<&TransportBinding> {
        self.responder_transport.as_ref()
    }

    /// Exact responder roles bound into the signed acceptance transcript.
    #[must_use]
    pub const fn responder_roles(&self) -> &BTreeSet<PeerRole> {
        &self.responder_roles
    }

    /// Exact inviter roles bound into the signed acceptance transcript.
    #[must_use]
    pub const fn inviter_roles(&self) -> &BTreeSet<PeerRole> {
        &self.inviter_roles
    }

    /// Whether the responder's local consent signature is present.
    #[must_use]
    pub const fn responder_is_confirmed(&self) -> bool {
        self.responder_confirmation_signature.is_some()
    }

    /// Whether the inviter's local consent signature is present.
    #[must_use]
    pub const fn inviter_is_confirmed(&self) -> bool {
        self.inviter_confirmation_signature.is_some()
    }

    /// Merges only independently verifiable consent signatures from the same immutable transcript.
    pub fn merge_confirmations_from(
        &mut self,
        peer: &Self,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.validate(now_unix_ms)?;
        peer.validate(now_unix_ms)?;
        let transcript = self.transcript()?;
        if transcript != peer.transcript()?
            || self.authentication_string != peer.authentication_string
            || self.invitation.invitation_id != peer.invitation.invitation_id
        {
            return Err(CoreError::IdentityMismatch);
        }
        let responder = self.responder_identity()?;
        let inviter = self.inviter_identity()?;
        merge_confirmation_signature(
            &mut self.responder_confirmation_signature,
            peer.responder_confirmation_signature.as_deref(),
            &responder,
            RESPONDER_CONFIRMATION_DOMAIN,
            &transcript,
        )?;
        merge_confirmation_signature(
            &mut self.inviter_confirmation_signature,
            peer.inviter_confirmation_signature.as_deref(),
            &inviter,
            INVITER_CONFIRMATION_DOMAIN,
            &transcript,
        )
    }

    /// Verifies the invitation, acceptance, roles, transport bindings, and authentication string.
    pub fn validate_exchange(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        self.validate(now_unix_ms)
    }

    /// Responder records an explicit comparison and approval with its identity key.
    pub fn confirm_responder(
        &mut self,
        displayed: &str,
        responder: &DeviceIdentity,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        self.validate(now_unix_ms)?;
        if displayed != self.authentication_string.as_str()
            || responder.device_id() != self.responder_device_id
        {
            return Err(CoreError::IdentityMismatch);
        }
        self.responder_confirmation_signature =
            Some(responder.sign(RESPONDER_CONFIRMATION_DOMAIN, &self.transcript()?));
        Ok(())
    }

    /// Whether both physical-device confirmations are present and verifiable.
    pub fn is_mutually_confirmed(&self, now_unix_ms: u64) -> bool {
        self.verify_confirmations(now_unix_ms).is_ok()
    }

    /// Responder verifies the returned inviter approval and derives the same grants.
    pub fn finalize_for_responder(
        &self,
        responder: &DeviceIdentity,
        now_unix_ms: u64,
    ) -> Result<PairingConfirmation, CoreError> {
        if responder.device_id() != self.responder_device_id {
            return Err(CoreError::IdentityMismatch);
        }
        self.verify_confirmations(now_unix_ms)?;
        self.confirmation(now_unix_ms)
    }

    fn inviter_identity(&self) -> Result<PublicIdentity, CoreError> {
        invitation_identity(&self.invitation)
    }

    fn responder_identity(&self) -> Result<PublicIdentity, CoreError> {
        PublicIdentity::from_encoded(self.responder_device_id, self.responder_public_key.clone())
    }

    fn transcript(&self) -> Result<Vec<u8>, CoreError> {
        acceptance_transcript(
            &self.invitation,
            &self.responder_identity()?,
            &self.responder_name,
            self.responder_transport.as_ref(),
            &self.responder_roles,
            &self.inviter_roles,
        )
    }

    fn validate(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        validate_invitation(&self.invitation, now_unix_ms)?;
        validate_display_name(&self.responder_name)?;
        let inviter = self.inviter_identity()?;
        inviter.verify(
            INVITATION_SIGNATURE_DOMAIN,
            &invitation_signing_bytes(&self.invitation)?,
            &self.invitation.signature,
        )?;
        let responder = self.responder_identity()?;
        if let Some(binding) = self.invitation.transport_binding.as_ref() {
            validate_transport_binding(binding, &inviter, &self.invitation.inviter_device_name)?;
        }
        if let Some(binding) = self.responder_transport.as_ref() {
            validate_transport_binding(binding, &responder, &self.responder_name)?;
        }
        let transcript = self.transcript()?;
        responder.verify(
            ACCEPTANCE_SIGNATURE_DOMAIN,
            &transcript,
            &self.responder_acceptance_signature,
        )?;
        let secret = decode_invitation_secret(&self.invitation)?;
        let expected = authentication_string(&secret, &transcript);
        if expected != self.authentication_string {
            return Err(CoreError::IdentityMismatch);
        }
        Ok(())
    }

    fn verify_confirmations(&self, now_unix_ms: u64) -> Result<(), CoreError> {
        self.validate(now_unix_ms)?;
        let transcript = self.transcript()?;
        self.responder_identity()?.verify(
            RESPONDER_CONFIRMATION_DOMAIN,
            &transcript,
            self.responder_confirmation_signature
                .as_deref()
                .ok_or(CoreError::PairingNotConfirmed)?,
        )?;
        self.inviter_identity()?.verify(
            INVITER_CONFIRMATION_DOMAIN,
            &transcript,
            self.inviter_confirmation_signature
                .as_deref()
                .ok_or(CoreError::PairingNotConfirmed)?,
        )
    }

    fn confirmation(&self, now_unix_ms: u64) -> Result<PairingConfirmation, CoreError> {
        Ok(PairingConfirmation {
            inviter_grant: PeerGrant {
                peer_device_id: self.responder_device_id,
                public_key: self.responder_public_key.clone(),
                display_name: self.responder_name.clone(),
                roles: self.responder_roles.clone(),
                confirmed_at_unix_ms: now_unix_ms,
                revoked: false,
            },
            responder_grant: PeerGrant {
                peer_device_id: self.invitation.inviter_device_id,
                public_key: self.invitation.inviter_public_key.clone(),
                display_name: self.invitation.inviter_device_name.clone(),
                roles: self.inviter_roles.clone(),
                confirmed_at_unix_ms: now_unix_ms,
                revoked: false,
            },
            inviter_transport: self.invitation.transport_binding.clone(),
            responder_transport: self.responder_transport.clone(),
        })
    }
}

fn merge_confirmation_signature(
    destination: &mut Option<String>,
    source: Option<&str>,
    signer: &PublicIdentity,
    domain: &[u8],
    transcript: &[u8],
) -> Result<(), CoreError> {
    let Some(source) = source else {
        return Ok(());
    };
    signer.verify(domain, transcript, source)?;
    if destination
        .as_deref()
        .is_some_and(|incumbent| incumbent != source)
    {
        return Err(CoreError::IdentityMismatch);
    }
    *destination = Some(source.to_owned());
    Ok(())
}

impl fmt::Debug for PairingSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingSession")
            .field("invitation_id", &self.invitation.invitation_id)
            .field("inviter_device_id", &self.invitation.inviter_device_id)
            .field("responder_device_id", &self.responder_device_id)
            .field(
                "responder_confirmed",
                &self.responder_confirmation_signature.is_some(),
            )
            .field(
                "inviter_confirmed",
                &self.inviter_confirmation_signature.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// Which of the two roles a device played in a pairing.
///
/// Side-dependent selections take this rather than a bare `bool`, because the
/// two grant fields below are named for the side that *stores* them while the
/// two transport fields are named for the side they *describe*. A boolean
/// parameter carries none of that to a call site, and reading the fields as if
/// both naming schemes matched is what shipped
/// `POST /api/v1/pair/finalize/inviter` answering `peerTransport: null` for a
/// storage provider the inviter had just paired.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingSide {
    /// Created and signed the invitation.
    Inviter,
    /// Accepted the invitation.
    Responder,
}

/// Pair of exact role-scoped grants created after mutual confirmation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingConfirmation {
    /// Grant stored by the inviter, describing the responder it just paired.
    pub inviter_grant: PeerGrant,
    /// Grant stored by the responder, describing the inviter it just paired.
    pub responder_grant: PeerGrant,
    /// Inviter TLS endpoint authenticated by the complete pairing transcript.
    pub inviter_transport: Option<TransportBinding>,
    /// Responder TLS endpoint authenticated by the complete pairing transcript.
    pub responder_transport: Option<TransportBinding>,
}

impl PairingConfirmation {
    /// Grant describing the far side, as seen by the device that played `local`.
    ///
    /// The grants are named for their holder, so the grant a device needs in
    /// order to answer "who did I just pair with?" is the one named after its
    /// own side. Note this runs in the opposite direction to
    /// [`Self::peer_transport`]; every caller should use these two accessors
    /// rather than reach for the fields and pick a side by hand.
    #[must_use]
    pub fn peer_grant(&self, local: PairingSide) -> &PeerGrant {
        match local {
            PairingSide::Inviter => &self.inviter_grant,
            PairingSide::Responder => &self.responder_grant,
        }
    }

    /// Signed transport binding of the far side, as seen by `local`.
    ///
    /// The bindings are named for the device they describe, so here the device
    /// takes the one named after the *other* side.
    #[must_use]
    pub fn peer_transport(&self, local: PairingSide) -> Option<&TransportBinding> {
        match local {
            PairingSide::Inviter => self.responder_transport.as_ref(),
            PairingSide::Responder => self.inviter_transport.as_ref(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PendingInvitation {
    expires_at_unix_ms: u64,
    secret_commitment: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedPairingState {
    schema_version: u16,
    pending: BTreeMap<String, PendingInvitation>,
    consumed_until: BTreeMap<String, u64>,
}

impl Default for PersistedPairingState {
    fn default() -> Self {
        Self {
            schema_version: PAIRING_STATE_SCHEMA_VERSION,
            pending: BTreeMap::new(),
            consumed_until: BTreeMap::new(),
        }
    }
}

/// Invitation lifecycle manager owned by one device identity.
pub struct PairingManager {
    identity: Arc<DeviceIdentity>,
    device_name: Mutex<String>,
    state_path: Option<PathBuf>,
    state: Mutex<PersistedPairingState>,
}

impl PairingManager {
    /// Creates an in-memory manager, primarily for isolated callers and tests.
    #[must_use]
    pub fn new(identity: Arc<DeviceIdentity>, device_name: impl Into<String>) -> Self {
        Self {
            identity,
            device_name: Mutex::new(device_name.into()),
            state_path: None,
            state: Mutex::new(PersistedPairingState::default()),
        }
    }

    /// Opens crash-safe invitation lifecycle state for real daemon and CLI workflows.
    pub fn open(
        identity: Arc<DeviceIdentity>,
        device_name: impl Into<String>,
        state_path: impl Into<PathBuf>,
    ) -> Result<Self, CoreError> {
        let device_name = device_name.into();
        validate_display_name(&device_name)?;
        let state_path = state_path.into();
        let state = match std::fs::symlink_metadata(&state_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "pairing state is not a regular file".to_owned(),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        return Err(CoreError::InvalidState(
                            "pairing state permissions are too broad".to_owned(),
                        ));
                    }
                }
                let state: PersistedPairingState =
                    read_json_bounded(&state_path, MAX_PAIRING_STATE_BYTES)?;
                if state.schema_version != PAIRING_STATE_SCHEMA_VERSION
                    || state.pending.len() > MAX_PENDING_INVITATIONS
                    || state.consumed_until.len() > 4_096
                    || state.pending.iter().any(|(id, pending)| {
                        !valid_invitation_id(id)
                            || pending.expires_at_unix_ms == 0
                            || !valid_lower_hex_digest(&pending.secret_commitment)
                            || state.consumed_until.contains_key(id)
                    })
                    || state
                        .consumed_until
                        .iter()
                        .any(|(id, expiry)| !valid_invitation_id(id) || *expiry == 0)
                {
                    return Err(CoreError::InvalidState(
                        "unsupported or excessive pairing state".to_owned(),
                    ));
                }
                state
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                PersistedPairingState::default()
            }
            Err(source) => {
                return Err(CoreError::Io {
                    operation: "inspect pairing state",
                    path: state_path,
                    source,
                });
            }
        };
        Ok(Self {
            identity,
            device_name: Mutex::new(device_name),
            state_path: Some(state_path),
            state: Mutex::new(state),
        })
    }

    /// Creates a bounded, expiring, single-use invitation.
    pub fn create_invitation(
        &self,
        now_unix_ms: u64,
        lifetime_ms: u64,
        endpoints: Vec<String>,
    ) -> Result<PairingInvitation, CoreError> {
        self.create_invitation_inner(now_unix_ms, lifetime_ms, endpoints, None)
    }

    /// Creates an invitation that signs the inviter's exact TLS transport identity.
    pub fn create_invitation_with_transport(
        &self,
        now_unix_ms: u64,
        lifetime_ms: u64,
        endpoints: Vec<String>,
        transport_binding: TransportBinding,
    ) -> Result<PairingInvitation, CoreError> {
        self.create_invitation_inner(now_unix_ms, lifetime_ms, endpoints, Some(transport_binding))
    }

    fn create_invitation_inner(
        &self,
        now_unix_ms: u64,
        lifetime_ms: u64,
        endpoints: Vec<String>,
        transport_binding: Option<TransportBinding>,
    ) -> Result<PairingInvitation, CoreError> {
        let device_name = self
            .device_name
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        validate_display_name(&device_name)?;
        if let Some(binding) = transport_binding.as_ref() {
            validate_transport_binding(binding, &self.identity.public_identity(), &device_name)?;
        }
        if lifetime_ms == 0
            || lifetime_ms > MAX_INVITATION_LIFETIME_MS
            || endpoints.len() > 16
            || endpoints
                .iter()
                .any(|value| value.is_empty() || value.len() > 512)
        {
            return Err(CoreError::ResourceLimit("pairing invitation"));
        }
        let expires_at_unix_ms = now_unix_ms
            .checked_add(lifetime_ms)
            .ok_or(CoreError::ResourceLimit("pairing invitation expiry"))?;
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        prune_state(&mut state, now_unix_ms);
        if state.pending.len() >= MAX_PENDING_INVITATIONS {
            return Err(CoreError::ResourceLimit("pending pairing invitations"));
        }
        let mut id_bytes = [0_u8; 16];
        let mut secret = Zeroizing::new([0_u8; 32]);
        OsRng.fill_bytes(&mut id_bytes);
        OsRng.fill_bytes(secret.as_mut());
        let invitation_id = URL_SAFE_NO_PAD.encode(id_bytes);
        let public = self.identity.public_identity();
        let invitation_secret = URL_SAFE_NO_PAD.encode(secret.as_ref());
        let invitation_secret_commitment = blake3::hash(secret.as_ref()).to_hex().to_string();
        let mut invitation = PairingInvitation {
            protocol_version: PROTOCOL_VERSION,
            minimum_protocol_version: PROTOCOL_VERSION,
            inviter_device_id: public.device_id,
            inviter_public_key: public.public_key,
            inviter_device_name: device_name,
            invitation_id: invitation_id.clone(),
            invitation_secret,
            invitation_secret_commitment: invitation_secret_commitment.clone(),
            expires_at_unix_ms,
            endpoints,
            transport_binding,
            signature: String::new(),
        };
        invitation.signature = self.identity.sign(
            INVITATION_SIGNATURE_DOMAIN,
            &invitation_signing_bytes(&invitation)?,
        );
        state.pending.insert(
            invitation_id,
            PendingInvitation {
                expires_at_unix_ms,
                secret_commitment: invitation_secret_commitment,
            },
        );
        self.persist_state(&state)?;
        Ok(invitation)
    }

    /// Updates the user-visible name bound into subsequently issued invitations.
    pub fn update_device_name(&self, device_name: impl Into<String>) -> Result<(), CoreError> {
        let device_name = device_name.into();
        validate_display_name(&device_name)?;
        *self
            .device_name
            .lock()
            .map_err(|_| CoreError::Synchronization)? = device_name;
        Ok(())
    }

    /// Cancels one pending invitation and durably rejects its replay until the original expiry.
    pub fn cancel_invitation(
        &self,
        invitation_id: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        if !valid_invitation_id(invitation_id) {
            return Err(CoreError::InvitationUnavailable);
        }
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        prune_state(&mut state, now_unix_ms);
        let pending = state
            .pending
            .remove(invitation_id)
            .ok_or(CoreError::InvitationUnavailable)?;
        state
            .consumed_until
            .insert(invitation_id.to_owned(), pending.expires_at_unix_ms);
        self.persist_state(&state)
    }

    /// Inviter verifies the live exchange and signs its explicit local approval.
    pub fn confirm_inviter(
        &self,
        session: &mut PairingSession,
        displayed: &str,
        now_unix_ms: u64,
    ) -> Result<(), CoreError> {
        session.validate(now_unix_ms)?;
        if displayed != session.authentication_string.as_str()
            || session.invitation.inviter_device_id != self.identity.device_id()
        {
            return Err(CoreError::IdentityMismatch);
        }
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        prune_state(&mut state, now_unix_ms);
        validate_pending(&state, session, now_unix_ms)?;
        session.responder_identity()?.verify(
            RESPONDER_CONFIRMATION_DOMAIN,
            &session.transcript()?,
            session
                .responder_confirmation_signature
                .as_deref()
                .ok_or(CoreError::PairingNotConfirmed)?,
        )?;
        session.inviter_confirmation_signature = Some(
            self.identity
                .sign(INVITER_CONFIRMATION_DOMAIN, &session.transcript()?),
        );
        self.persist_state(&state)
    }

    /// Consumes the invitation and emits exact role-scoped grants.
    pub fn finalize(
        &self,
        session: &PairingSession,
        now_unix_ms: u64,
    ) -> Result<PairingConfirmation, CoreError> {
        session.verify_confirmations(now_unix_ms)?;
        if session.invitation.inviter_device_id != self.identity.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        let mut state = self.state.lock().map_err(|_| CoreError::Synchronization)?;
        prune_state(&mut state, now_unix_ms);
        validate_pending(&state, session, now_unix_ms)?;
        state.pending.remove(&session.invitation.invitation_id);
        state.consumed_until.insert(
            session.invitation.invitation_id.clone(),
            session.invitation.expires_at_unix_ms,
        );
        self.persist_state(&state)?;
        session.confirmation(now_unix_ms)
    }

    fn persist_state(&self, state: &PersistedPairingState) -> Result<(), CoreError> {
        if let Some(path) = &self.state_path {
            write_json_atomic(path, state, true)?;
        }
        Ok(())
    }
}

fn prune_state(state: &mut PersistedPairingState, now_unix_ms: u64) {
    state
        .pending
        .retain(|_, pending| pending.expires_at_unix_ms > now_unix_ms);
    state
        .consumed_until
        .retain(|_, expires_at| *expires_at > now_unix_ms);
}

fn validate_pending(
    state: &PersistedPairingState,
    session: &PairingSession,
    now_unix_ms: u64,
) -> Result<(), CoreError> {
    if state
        .consumed_until
        .contains_key(&session.invitation.invitation_id)
    {
        return Err(CoreError::InvitationUnavailable);
    }
    let pending = state
        .pending
        .get(&session.invitation.invitation_id)
        .ok_or(CoreError::InvitationUnavailable)?;
    if pending.expires_at_unix_ms <= now_unix_ms
        || pending.expires_at_unix_ms != session.invitation.expires_at_unix_ms
        || pending.secret_commitment != session.invitation.invitation_secret_commitment
    {
        return Err(CoreError::InvitationUnavailable);
    }
    Ok(())
}

fn validate_invitation(invitation: &PairingInvitation, now_unix_ms: u64) -> Result<(), CoreError> {
    if invitation.protocol_version != PROTOCOL_VERSION
        || invitation.minimum_protocol_version > PROTOCOL_VERSION
        || invitation.expires_at_unix_ms <= now_unix_ms
        || invitation.invitation_id.len() > 128
        || URL_SAFE_NO_PAD
            .decode(&invitation.invitation_id)
            .map_or(true, |bytes| bytes.len() != 16)
        || invitation.endpoints.len() > 16
        || invitation
            .endpoints
            .iter()
            .any(|endpoint| endpoint.is_empty() || endpoint.len() > 512)
        || invitation.signature.is_empty()
    {
        return Err(CoreError::InvitationUnavailable);
    }
    validate_display_name(&invitation.inviter_device_name)?;
    let secret = decode_invitation_secret(invitation)?;
    if blake3::hash(secret.as_ref()).to_hex().as_str() != invitation.invitation_secret_commitment {
        return Err(CoreError::InvitationUnavailable);
    }
    Ok(())
}

fn valid_invitation_id(value: &str) -> bool {
    value.len() <= 128
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|bytes| bytes.len() == 16)
}

fn valid_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn validate_display_name(value: &str) -> Result<(), CoreError> {
    if value.trim().is_empty() || value.len() > 80 || value.chars().any(char::is_control) {
        return Err(CoreError::InvalidState(
            "invalid pairing display name".to_owned(),
        ));
    }
    Ok(())
}

fn invitation_identity(invitation: &PairingInvitation) -> Result<PublicIdentity, CoreError> {
    PublicIdentity::from_encoded(
        invitation.inviter_device_id,
        invitation.inviter_public_key.clone(),
    )
}

fn decode_invitation_secret(
    invitation: &PairingInvitation,
) -> Result<Zeroizing<[u8; 32]>, CoreError> {
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(&invitation.invitation_secret)
            .map_err(|_| CoreError::InvitationUnavailable)?,
    );
    let bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::InvitationUnavailable)?;
    Ok(Zeroizing::new(bytes))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvitationSigningFields<'a> {
    protocol_version: u16,
    minimum_protocol_version: u16,
    inviter_device_id: covalent_protocol::DeviceId,
    inviter_public_key: &'a str,
    inviter_device_name: &'a str,
    invitation_id: &'a str,
    invitation_secret: &'a str,
    invitation_secret_commitment: &'a str,
    expires_at_unix_ms: u64,
    endpoints: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_binding: Option<&'a TransportBinding>,
}

fn invitation_signing_bytes(invitation: &PairingInvitation) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&InvitationSigningFields {
        protocol_version: invitation.protocol_version,
        minimum_protocol_version: invitation.minimum_protocol_version,
        inviter_device_id: invitation.inviter_device_id,
        inviter_public_key: &invitation.inviter_public_key,
        inviter_device_name: &invitation.inviter_device_name,
        invitation_id: &invitation.invitation_id,
        invitation_secret: &invitation.invitation_secret,
        invitation_secret_commitment: &invitation.invitation_secret_commitment,
        expires_at_unix_ms: invitation.expires_at_unix_ms,
        endpoints: &invitation.endpoints,
        transport_binding: invitation.transport_binding.as_ref(),
    })?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AcceptanceTranscript<'a> {
    invitation_id: &'a str,
    invitation_commitment: &'a str,
    inviter_device_id: covalent_protocol::DeviceId,
    inviter_public_key: &'a str,
    inviter_device_name: &'a str,
    responder_device_id: covalent_protocol::DeviceId,
    responder_public_key: &'a str,
    responder_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    responder_transport: Option<&'a TransportBinding>,
    responder_roles: &'a BTreeSet<PeerRole>,
    inviter_roles: &'a BTreeSet<PeerRole>,
    protocol_version: u16,
}

fn acceptance_transcript(
    invitation: &PairingInvitation,
    responder: &PublicIdentity,
    responder_name: &str,
    responder_transport: Option<&TransportBinding>,
    responder_roles: &BTreeSet<PeerRole>,
    inviter_roles: &BTreeSet<PeerRole>,
) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&AcceptanceTranscript {
        invitation_id: &invitation.invitation_id,
        invitation_commitment: &invitation.invitation_secret_commitment,
        inviter_device_id: invitation.inviter_device_id,
        inviter_public_key: &invitation.inviter_public_key,
        inviter_device_name: &invitation.inviter_device_name,
        responder_device_id: responder.device_id,
        responder_public_key: &responder.public_key,
        responder_name,
        responder_transport,
        responder_roles,
        inviter_roles,
        protocol_version: PROTOCOL_VERSION,
    })?)
}

pub(crate) fn validate_transport_binding(
    binding: &TransportBinding,
    identity: &PublicIdentity,
    expected_name: &str,
) -> Result<(), CoreError> {
    use std::net::SocketAddr;

    if binding.peer_id != identity.device_id
        || binding.display_name != expected_name
        || binding.address.len() > 128
        || binding.certificate_der.len() > 128 * 1_024
        || binding.certificate_fingerprint.len() != 64
    {
        return Err(CoreError::IdentityMismatch);
    }
    let address: SocketAddr = binding
        .address
        .parse()
        .map_err(|_| CoreError::InvalidState("invalid paired transport address".to_owned()))?;
    if address.port() == 0 || address.to_string() != binding.address {
        return Err(CoreError::InvalidState(
            "paired transport address is not canonical".to_owned(),
        ));
    }
    let certificate = URL_SAFE_NO_PAD
        .decode(&binding.certificate_der)
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    if certificate.is_empty() || certificate.len() > 64 * 1_024 {
        return Err(CoreError::InvalidKeyMaterial);
    }
    let expected = Sha256::digest(certificate)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if expected != binding.certificate_fingerprint
        || binding
            .certificate_fingerprint
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        return Err(CoreError::IdentityMismatch);
    }
    Ok(())
}

fn authentication_string(secret: &[u8; 32], transcript: &[u8]) -> ShortAuthenticationString {
    let digest = blake3::keyed_hash(secret, transcript);
    let bytes = digest.as_bytes();
    let mut groups = Vec::with_capacity(4);
    for pair in bytes[..8].chunks_exact(2) {
        let value = u16::from_be_bytes([pair[0], pair[1]]) % 10_000;
        groups.push(format!("{value:04}"));
    }
    ShortAuthenticationString(groups.join("-"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn pairing_requires_signed_confirmations_and_is_single_use() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let responder = DeviceIdentity::generate();
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, vec!["127.0.0.1:4433".to_owned()])
            .expect("invitation");
        let mut session = PairingSession::accept_with_roles(
            invitation,
            &responder,
            "NAS",
            BTreeSet::from([PeerRole::StorageProvider]),
            BTreeSet::from([PeerRole::BackupReader]),
            2_000,
        )
        .expect("accept invitation");
        let displayed = session.authentication_string().to_string();
        assert!(matches!(
            manager.confirm_inviter(&mut session, &displayed, 2_000),
            Err(CoreError::PairingNotConfirmed)
        ));
        session
            .confirm_responder(&displayed, &responder, 2_000)
            .expect("responder confirmation");
        manager
            .confirm_inviter(&mut session, &displayed, 2_000)
            .expect("inviter confirmation");
        let confirmation = manager.finalize(&session, 2_000).expect("finalize");
        assert_eq!(
            confirmation.inviter_grant.peer_device_id,
            responder.device_id()
        );
        assert!(session.finalize_for_responder(&responder, 2_000).is_ok());
        assert!(matches!(
            manager.finalize(&session, 2_000),
            Err(CoreError::InvitationUnavailable)
        ));
    }

    #[test]
    fn pending_invitations_survive_restart_and_tampering_fails() {
        let directory = tempdir().expect("directory");
        let identity_path = directory.path().join("identity.json");
        let identity = Arc::new(DeviceIdentity::load_or_create(&identity_path).expect("identity"));
        let state_path = directory.path().join("pairing.json");
        let manager =
            PairingManager::open(Arc::clone(&identity), "Mac", &state_path).expect("manager");
        let invitation = manager
            .create_invitation(10, 100, Vec::new())
            .expect("invitation");
        drop(manager);
        let manager = PairingManager::open(identity, "Mac", state_path).expect("reopen");
        let responder = DeviceIdentity::generate();
        let mut session =
            PairingSession::accept(invitation.clone(), &responder, "NAS", 20).expect("accept");
        let displayed = session.authentication_string().to_string();
        session
            .confirm_responder(&displayed, &responder, 20)
            .expect("confirm responder");
        manager
            .confirm_inviter(&mut session, &displayed, 20)
            .expect("confirm inviter after restart");

        let mut tampered = invitation;
        tampered.inviter_device_name = "Attacker".to_owned();
        assert!(PairingSession::accept(tampered, &responder, "NAS", 20).is_err());
        assert!(
            PairingSession::accept(session.invitation.clone(), &responder, "NAS", 111,).is_err()
        );
    }

    /// Produces a code of exactly the same shape and length as `displayed` with a
    /// single digit changed, so only an exact comparison can reject it.
    fn near_miss_code(displayed: &str) -> String {
        let mut bytes = displayed.as_bytes().to_vec();
        for byte in &mut bytes {
            if byte.is_ascii_digit() {
                *byte = if *byte == b'0' { b'1' } else { b'0' };
                break;
            }
        }
        String::from_utf8(bytes).expect("ascii authentication string")
    }

    #[test]
    fn a_wrong_authentication_string_is_refused_by_both_devices() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let responder = DeviceIdentity::generate();
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");
        let mut session = PairingSession::accept(invitation, &responder, "NAS", 2_000)
            .expect("accept invitation");

        let displayed = session.authentication_string().to_string();
        let near_miss = near_miss_code(&displayed);
        assert_ne!(near_miss, displayed, "the near miss must actually differ");
        assert_eq!(near_miss.len(), displayed.len());

        // This is the entire defence against a machine in the middle: the human
        // reads a code off the other device and it does not match.
        for wrong in [
            near_miss.as_str(),
            "",
            "0000-0000-0000-0000-0000",
            &displayed[..displayed.len() - 1],
            &format!("{displayed}0"),
            &displayed.replace('-', ""),
        ] {
            if wrong == displayed {
                continue;
            }
            assert!(
                matches!(
                    session.confirm_responder(wrong, &responder, 2_000),
                    Err(CoreError::IdentityMismatch)
                ),
                "responder accepted {wrong:?} instead of {displayed:?}"
            );
            assert!(
                !session.responder_is_confirmed(),
                "a refused comparison left a consent signature behind for {wrong:?}"
            );
            assert!(
                matches!(
                    manager.confirm_inviter(&mut session, wrong, 2_000),
                    Err(CoreError::IdentityMismatch)
                ),
                "inviter accepted {wrong:?} instead of {displayed:?}"
            );
            assert!(!session.inviter_is_confirmed());
        }

        // The code the user actually sees is the only one that is accepted, so the
        // rejections above are the comparison firing and not a broken session.
        session
            .confirm_responder(&displayed, &responder, 2_000)
            .expect("the displayed code is accepted");
        assert!(session.responder_is_confirmed());
        manager
            .confirm_inviter(&mut session, &displayed, 2_000)
            .expect("the displayed code is accepted");
        assert!(session.inviter_is_confirmed());
    }

    #[test]
    fn the_authentication_string_is_derived_from_the_secret_and_the_transcript() {
        // Both inputs must move the output, or the code carries no information
        // about the exchange the two humans are comparing.
        let transcript: &[u8] = b"acceptance transcript";
        let other_transcript: &[u8] = b"acceptance transcripu";
        let secret = [7_u8; 32];
        let other_secret = [8_u8; 32];
        let base = authentication_string(&secret, transcript);
        assert_ne!(
            base,
            authentication_string(&other_secret, transcript),
            "a different invitation secret must produce a different code"
        );
        assert_ne!(
            base,
            authentication_string(&secret, other_transcript),
            "a different transcript must produce a different code"
        );
        assert_eq!(
            base,
            authentication_string(&secret, transcript),
            "the derivation must stay deterministic"
        );
        let groups: Vec<&str> = base.as_str().split('-').collect();
        assert_eq!(groups.len(), 4, "unexpected code shape: {base}");
        assert!(
            groups
                .iter()
                .all(|group| group.len() == 4 && group.bytes().all(|byte| byte.is_ascii_digit())),
            "unexpected code shape: {base}"
        );

        // End to end, independent pairings must not display the same code.
        let mut seen = BTreeSet::new();
        for index in 0..8_u64 {
            let manager = PairingManager::new(Arc::new(DeviceIdentity::generate()), "Home Mac");
            let invitation = manager
                .create_invitation(1_000, 60_000, Vec::new())
                .expect("invitation");
            let session =
                PairingSession::accept(invitation, &DeviceIdentity::generate(), "NAS", 2_000)
                    .expect("accept invitation");
            assert!(
                seen.insert(session.authentication_string().to_string()),
                "pairing {index} repeated an authentication string"
            );
        }

        // Holding the invitation fixed, changing anything the transcript covers
        // must change the code the two humans compare.
        let manager = PairingManager::new(Arc::new(DeviceIdentity::generate()), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");
        let first = PairingSession::accept(
            invitation.clone(),
            &DeviceIdentity::generate(),
            "NAS",
            2_000,
        )
        .expect("accept invitation");
        let other_responder = PairingSession::accept(
            invitation.clone(),
            &DeviceIdentity::generate(),
            "NAS",
            2_000,
        )
        .expect("accept invitation");
        assert_ne!(
            first.authentication_string(),
            other_responder.authentication_string(),
            "a different responder identity must change the displayed code"
        );
        let renamed = PairingSession::accept_with_roles(
            invitation.clone(),
            &DeviceIdentity::generate(),
            "Attacker NAS",
            BTreeSet::from([PeerRole::StorageProvider]),
            BTreeSet::from([PeerRole::BackupReader]),
            2_000,
        )
        .expect("accept invitation");
        assert_ne!(
            first.authentication_string(),
            renamed.authentication_string(),
            "a different responder name must change the displayed code"
        );
        let rerolled = PairingSession::accept_with_roles(
            invitation,
            &DeviceIdentity::generate(),
            "Attacker NAS",
            BTreeSet::from([PeerRole::BackupReader]),
            BTreeSet::from([PeerRole::StorageProvider]),
            2_000,
        )
        .expect("accept invitation");
        assert_ne!(
            renamed.authentication_string(),
            rerolled.authentication_string(),
            "different requested roles must change the displayed code"
        );
    }

    #[test]
    fn confirmations_are_verified_against_the_signed_transcript() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let responder = DeviceIdentity::generate();
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");
        let mut session = PairingSession::accept(invitation, &responder, "NAS", 2_000)
            .expect("accept invitation");
        let displayed = session.authentication_string().to_string();

        assert!(
            !session.is_mutually_confirmed(2_000),
            "an unconfirmed session must not be mutually confirmed"
        );
        assert!(matches!(
            session.finalize_for_responder(&responder, 2_000),
            Err(CoreError::PairingNotConfirmed)
        ));
        assert!(matches!(
            manager.finalize(&session, 2_000),
            Err(CoreError::PairingNotConfirmed)
        ));

        session
            .confirm_responder(&displayed, &responder, 2_000)
            .expect("responder confirmation");
        assert!(
            !session.is_mutually_confirmed(2_000),
            "one signature out of two must not be mutual confirmation"
        );
        assert!(matches!(
            session.finalize_for_responder(&responder, 2_000),
            Err(CoreError::PairingNotConfirmed)
        ));

        manager
            .confirm_inviter(&mut session, &displayed, 2_000)
            .expect("inviter confirmation");
        assert!(session.is_mutually_confirmed(2_000));

        let transcript = session.transcript().expect("transcript");
        let genuine_responder = session
            .responder_confirmation_signature
            .clone()
            .expect("responder signature");
        let genuine_inviter = session
            .inviter_confirmation_signature
            .clone()
            .expect("inviter signature");
        let mut other_transcript = transcript.clone();
        other_transcript.push(0x00);

        // Each substitution below is a well formed signature that a permissive
        // check would wave through. None of them is consent to this exchange.
        for (label, forged) in [
            (
                "signed over a different transcript",
                responder.sign(RESPONDER_CONFIRMATION_DOMAIN, &other_transcript),
            ),
            (
                "signed by a stranger",
                DeviceIdentity::generate().sign(RESPONDER_CONFIRMATION_DOMAIN, &transcript),
            ),
            (
                "signed under the inviter domain",
                responder.sign(INVITER_CONFIRMATION_DOMAIN, &transcript),
            ),
            ("replayed from the inviter", genuine_inviter.clone()),
        ] {
            session.responder_confirmation_signature = Some(forged);
            assert!(
                !session.is_mutually_confirmed(2_000),
                "a responder confirmation {label} was accepted"
            );
            assert!(matches!(
                session.finalize_for_responder(&responder, 2_000),
                Err(CoreError::AuthenticationFailed | CoreError::PairingNotConfirmed)
            ));
        }
        session.responder_confirmation_signature = None;
        assert!(!session.is_mutually_confirmed(2_000));

        session.responder_confirmation_signature = Some(genuine_responder);
        assert!(session.is_mutually_confirmed(2_000));

        for (label, forged) in [
            (
                "signed over a different transcript",
                inviter.sign(INVITER_CONFIRMATION_DOMAIN, &other_transcript),
            ),
            (
                "signed by a stranger",
                DeviceIdentity::generate().sign(INVITER_CONFIRMATION_DOMAIN, &transcript),
            ),
            (
                "signed under the responder domain",
                inviter.sign(RESPONDER_CONFIRMATION_DOMAIN, &transcript),
            ),
        ] {
            session.inviter_confirmation_signature = Some(forged);
            assert!(
                !session.is_mutually_confirmed(2_000),
                "an inviter confirmation {label} was accepted"
            );
            assert!(matches!(
                session.finalize_for_responder(&responder, 2_000),
                Err(CoreError::AuthenticationFailed | CoreError::PairingNotConfirmed)
            ));
            assert!(manager.finalize(&session, 2_000).is_err());
        }
        session.inviter_confirmation_signature = None;
        assert!(!session.is_mutually_confirmed(2_000));

        // Restoring both genuine signatures confirms the session again, so the
        // rejections above are the verification firing and not a broken session.
        session.inviter_confirmation_signature = Some(genuine_inviter);
        assert!(session.is_mutually_confirmed(2_000));
        assert!(session.finalize_for_responder(&responder, 2_000).is_ok());
    }

    #[test]
    fn a_session_carrying_a_substituted_authentication_string_is_refused() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let responder = DeviceIdentity::generate();
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");
        let mut session = PairingSession::accept(invitation, &responder, "NAS", 2_000)
            .expect("accept invitation");
        let genuine = session.authentication_string().clone();
        session.validate_exchange(2_000).expect("genuine session");

        // A session is transferable and deserializable, so the code it carries is
        // untrusted until it is rederived from the invitation secret and the signed
        // transcript. Otherwise whoever hands over the session blob chooses the
        // digits both humans read off their screens, and the comparison they
        // perform proves nothing.
        let attacker_chosen = ShortAuthenticationString("1234-5678-1234-5678".to_owned());
        assert_ne!(attacker_chosen, genuine);
        session.authentication_string = attacker_chosen.clone();

        assert!(matches!(
            session.validate_exchange(2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(matches!(
            session.confirm_responder(attacker_chosen.as_str(), &responder, 2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(!session.responder_is_confirmed());
        assert!(matches!(
            manager.confirm_inviter(&mut session, attacker_chosen.as_str(), 2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(!session.inviter_is_confirmed());
        assert!(!session.is_mutually_confirmed(2_000));

        // Restoring the derived code makes the session usable again.
        session.authentication_string = genuine.clone();
        session.validate_exchange(2_000).expect("restored session");
        session
            .confirm_responder(genuine.as_str(), &responder, 2_000)
            .expect("responder confirmation");
    }

    #[test]
    fn only_the_named_devices_can_record_their_own_consent() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let responder = DeviceIdentity::generate();
        let stranger = DeviceIdentity::generate();
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");
        let mut session = PairingSession::accept(invitation.clone(), &responder, "NAS", 2_000)
            .expect("accept invitation");
        let displayed = session.authentication_string().to_string();

        // Knowing the code is not enough: consent is recorded with the key of the
        // device the transcript names, so a third device holding the same code
        // cannot sign the responder's approval.
        assert!(matches!(
            session.confirm_responder(&displayed, &stranger, 2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(
            !session.responder_is_confirmed(),
            "a stranger recorded the responder's consent"
        );
        session
            .confirm_responder(&displayed, &responder, 2_000)
            .expect("responder confirmation");

        // Likewise on the inviter side: a manager for a different device cannot
        // sign the inviter approval for someone else's invitation.
        let other_manager = PairingManager::new(Arc::new(stranger), "Someone Else");
        assert!(matches!(
            other_manager.confirm_inviter(&mut session, &displayed, 2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(
            !session.inviter_is_confirmed(),
            "a foreign manager recorded the inviter's consent"
        );
        manager
            .confirm_inviter(&mut session, &displayed, 2_000)
            .expect("inviter confirmation");
        assert!(session.is_mutually_confirmed(2_000));
    }

    #[test]
    fn confirmations_do_not_merge_across_different_exchanges() {
        let inviter = Arc::new(DeviceIdentity::generate());
        let manager = PairingManager::new(Arc::clone(&inviter), "Home Mac");
        let invitation = manager
            .create_invitation(1_000, 60_000, Vec::new())
            .expect("invitation");

        // Two exchanges under the same invitation, differing only in who the
        // responder is. Their transcripts and codes differ, so consent recorded in
        // one is not consent in the other and must not be spliced across.
        let responder = DeviceIdentity::generate();
        let other_responder = DeviceIdentity::generate();
        let mut session = PairingSession::accept(invitation.clone(), &responder, "NAS", 2_000)
            .expect("accept invitation");
        let mut other = PairingSession::accept(invitation, &other_responder, "Other NAS", 2_000)
            .expect("accept invitation");
        assert_ne!(
            session.authentication_string(),
            other.authentication_string()
        );

        assert!(matches!(
            session.merge_confirmations_from(&other, 2_000),
            Err(CoreError::IdentityMismatch)
        ));

        other
            .confirm_responder(
                &other.authentication_string().to_string(),
                &other_responder,
                2_000,
            )
            .expect("other responder confirmation");
        assert!(matches!(
            session.merge_confirmations_from(&other, 2_000),
            Err(CoreError::IdentityMismatch)
        ));
        assert!(
            !session.responder_is_confirmed(),
            "a confirmation from a different exchange was merged in"
        );
        assert!(!session.is_mutually_confirmed(2_000));

        // Merging from a genuine copy of the same exchange still works, so the
        // rejections above are the transcript gate and not a disabled merge.
        let mut copy = PairingSession::accept(session.invitation.clone(), &responder, "NAS", 2_000)
            .expect("accept invitation");
        let displayed = copy.authentication_string().to_string();
        copy.confirm_responder(&displayed, &responder, 2_000)
            .expect("responder confirmation");
        session
            .merge_confirmations_from(&copy, 2_000)
            .expect("same exchange merges");
        assert!(session.responder_is_confirmed());
    }
}
