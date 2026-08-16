use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_protocol::{PROTOCOL_VERSION, PairingInvitation, PeerGrant, PeerRole};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
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
        let responder_name = responder_name.into();
        validate_display_name(&responder_name)?;
        let responder_public = responder.public_identity();
        let transcript = acceptance_transcript(
            &invitation,
            &responder_public,
            &responder_name,
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
        })
    }
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

/// Pair of exact role-scoped grants created after mutual confirmation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairingConfirmation {
    /// Grant stored by the inviter for the responder.
    pub inviter_grant: PeerGrant,
    /// Grant stored by the responder for the inviter.
    pub responder_grant: PeerGrant,
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
        let device_name = self
            .device_name
            .lock()
            .map_err(|_| CoreError::Synchronization)?
            .clone();
        validate_display_name(&device_name)?;
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
    responder_roles: &'a BTreeSet<PeerRole>,
    inviter_roles: &'a BTreeSet<PeerRole>,
    protocol_version: u16,
}

fn acceptance_transcript(
    invitation: &PairingInvitation,
    responder: &PublicIdentity,
    responder_name: &str,
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
        responder_roles,
        inviter_roles,
        protocol_version: PROTOCOL_VERSION,
    })?)
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
}
