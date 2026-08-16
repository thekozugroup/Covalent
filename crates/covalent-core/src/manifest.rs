use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use covalent_protocol::{Manifest, ManifestEnvelope, PROTOCOL_VERSION, PeerGrant, SignedRoster};
use rand_core::{OsRng, RngCore};
use serde::Serialize;
use zeroize::Zeroizing;

use crate::{BackupKey, CoreError, DeviceIdentity, PublicIdentity};

const MANIFEST_CIPHER_SUITE: &str = "XCHACHA20-POLY1305+ED25519+BLAKE3-HKDF-SHA256-v1";
const MANIFEST_SIGNATURE_DOMAIN: &[u8] = b"covalent/manifest-envelope/v1";
const ROSTER_SIGNATURE_DOMAIN: &[u8] = b"covalent/signed-roster/v1";
const MAX_ENCRYPTED_MANIFEST_BYTES: usize = 256 * 1_024 * 1_024;

/// Validates, encrypts, and signs a versioned manifest.
pub fn encrypt_manifest(
    manifest: &Manifest,
    key_epoch: u64,
    key: &BackupKey,
    signer: &DeviceIdentity,
) -> Result<ManifestEnvelope, CoreError> {
    manifest.validate()?;
    if key_epoch == 0 {
        return Err(CoreError::InvalidState(
            "manifest key epoch must be positive".to_owned(),
        ));
    }
    let plaintext = Zeroizing::new(serde_json::to_vec(manifest)?);
    if plaintext.len() > MAX_ENCRYPTED_MANIFEST_BYTES {
        return Err(CoreError::ResourceLimit("encrypted manifest"));
    }
    let context = manifest_context(manifest.backup_id, key_epoch);
    let encryption_key = key.derive(&context, b"covalent/manifest-encryption/v1")?;
    let mut nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(encryption_key.as_ref()));
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_ref(),
                aad: &context,
            },
        )
        .map_err(|_| CoreError::AuthenticationFailed)?;
    let mut envelope = ManifestEnvelope {
        protocol_version: PROTOCOL_VERSION,
        backup_id: manifest.backup_id,
        key_epoch,
        cipher_suite: MANIFEST_CIPHER_SUITE.to_owned(),
        nonce: URL_SAFE_NO_PAD.encode(nonce),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
        signer_device_id: signer.device_id(),
        signature: String::new(),
    };
    envelope.signature = signer.sign(
        MANIFEST_SIGNATURE_DOMAIN,
        &manifest_signing_bytes(&envelope)?,
    );
    Ok(envelope)
}

/// Verifies the signer, authenticates, decrypts, and validates a manifest.
pub fn decrypt_manifest(
    envelope: &ManifestEnvelope,
    key: &BackupKey,
    signer: &PublicIdentity,
) -> Result<Manifest, CoreError> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(covalent_protocol::ContractError::UnsupportedProtocol(
            envelope.protocol_version,
        )
        .into());
    }
    if envelope.key_epoch == 0 {
        return Err(CoreError::AuthenticationFailed);
    }
    if envelope.cipher_suite != MANIFEST_CIPHER_SUITE {
        return Err(CoreError::UnsupportedCipherSuite(
            envelope.cipher_suite.clone(),
        ));
    }
    if envelope.signer_device_id != signer.device_id {
        return Err(CoreError::IdentityMismatch);
    }
    signer.verify(
        MANIFEST_SIGNATURE_DOMAIN,
        &manifest_signing_bytes(envelope)?,
        &envelope.signature,
    )?;
    let nonce = URL_SAFE_NO_PAD
        .decode(&envelope.nonce)
        .map_err(|_| CoreError::AuthenticationFailed)?;
    let nonce: [u8; 24] = nonce
        .try_into()
        .map_err(|_| CoreError::AuthenticationFailed)?;
    if envelope.ciphertext.len() > MAX_ENCRYPTED_MANIFEST_BYTES.saturating_mul(2) {
        return Err(CoreError::ResourceLimit("encrypted manifest"));
    }
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&envelope.ciphertext)
        .map_err(|_| CoreError::AuthenticationFailed)?;
    if ciphertext.len() > MAX_ENCRYPTED_MANIFEST_BYTES + 16 {
        return Err(CoreError::ResourceLimit("encrypted manifest"));
    }
    let context = manifest_context(envelope.backup_id, envelope.key_epoch);
    let encryption_key = key.derive(&context, b"covalent/manifest-encryption/v1")?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(encryption_key.as_ref()));
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &context,
                },
            )
            .map_err(|_| CoreError::AuthenticationFailed)?,
    );
    let manifest: Manifest = serde_json::from_slice(plaintext.as_ref())?;
    manifest.validate()?;
    if manifest.backup_id != envelope.backup_id {
        return Err(CoreError::AuthenticationFailed);
    }
    Ok(manifest)
}

/// Builder for deterministic signed roster epochs.
#[derive(Clone, Debug)]
pub struct SignedRosterBuilder {
    epoch: u64,
    previous_digest: String,
    grants: Vec<PeerGrant>,
}

impl SignedRosterBuilder {
    /// Begins one monotonic roster epoch.
    #[must_use]
    pub fn new(epoch: u64, previous_digest: impl Into<String>) -> Self {
        Self {
            epoch,
            previous_digest: previous_digest.into(),
            grants: Vec::new(),
        }
    }

    /// Adds or replaces one peer tombstone/grant.
    #[must_use]
    pub fn grant(mut self, grant: PeerGrant) -> Self {
        self.grants
            .retain(|existing| existing.peer_device_id != grant.peer_device_id);
        self.grants.push(grant);
        self
    }

    /// Sorts, validates, and signs the complete roster.
    pub fn sign(mut self, signer: &DeviceIdentity) -> Result<SignedRoster, CoreError> {
        if self.epoch == 0
            || (self.epoch == 1 && !self.previous_digest.is_empty())
            || (self.epoch > 1
                && (self.previous_digest.len() != 64
                    || self
                        .previous_digest
                        .bytes()
                        .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())))
            || self.grants.len() > 128
        {
            return Err(CoreError::InvalidState(
                "invalid roster epoch, chain, or grant count".to_owned(),
            ));
        }
        self.grants.sort_by_key(|grant| grant.peer_device_id);
        if self.grants.iter().any(|grant| {
            grant.display_name.trim().is_empty()
                || grant.display_name.len() > 80
                || grant.display_name.chars().any(char::is_control)
                || PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
                    .is_err()
        }) {
            return Err(CoreError::InvalidState("invalid roster grant".to_owned()));
        }
        let mut roster = SignedRoster {
            protocol_version: PROTOCOL_VERSION,
            epoch: self.epoch,
            previous_digest: self.previous_digest,
            grants: self.grants,
            signer_device_id: signer.device_id(),
            signature: String::new(),
        };
        roster.signature = signer.sign(ROSTER_SIGNATURE_DOMAIN, &roster_signing_bytes(&roster)?);
        Ok(roster)
    }
}

/// Verifies signature, monotonicity, chain link, ordering, and duplicate freedom.
pub fn verify_roster(
    roster: &SignedRoster,
    signer: &PublicIdentity,
    high_water_epoch: u64,
    expected_previous_digest: &str,
) -> Result<(), CoreError> {
    if roster.protocol_version != PROTOCOL_VERSION
        || roster.epoch != high_water_epoch.saturating_add(1)
        || roster.previous_digest != expected_previous_digest
        || roster.signer_device_id != signer.device_id
    {
        return Err(CoreError::InvalidState(
            "roster rollback, fork, or signer mismatch".to_owned(),
        ));
    }
    if roster.grants.len() > 128
        || roster.grants.iter().any(|grant| {
            grant.display_name.trim().is_empty()
                || grant.display_name.len() > 80
                || grant.display_name.chars().any(char::is_control)
                || PublicIdentity::from_encoded(grant.peer_device_id, grant.public_key.clone())
                    .is_err()
        })
    {
        return Err(CoreError::InvalidState("invalid roster grants".to_owned()));
    }
    if roster
        .grants
        .windows(2)
        .any(|pair| pair[0].peer_device_id >= pair[1].peer_device_id)
    {
        return Err(CoreError::InvalidState(
            "roster grants are not unique and sorted".to_owned(),
        ));
    }
    signer.verify(
        ROSTER_SIGNATURE_DOMAIN,
        &roster_signing_bytes(roster)?,
        &roster.signature,
    )
}

/// Stable digest used to chain the next accepted roster.
pub fn roster_digest(roster: &SignedRoster) -> Result<String, CoreError> {
    Ok(blake3::hash(&serde_json::to_vec(roster)?)
        .to_hex()
        .to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestSigningFields<'a> {
    protocol_version: u16,
    backup_id: covalent_protocol::BackupId,
    key_epoch: u64,
    cipher_suite: &'a str,
    nonce: &'a str,
    ciphertext: &'a str,
    signer_device_id: covalent_protocol::DeviceId,
}

fn manifest_signing_bytes(envelope: &ManifestEnvelope) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&ManifestSigningFields {
        protocol_version: envelope.protocol_version,
        backup_id: envelope.backup_id,
        key_epoch: envelope.key_epoch,
        cipher_suite: &envelope.cipher_suite,
        nonce: &envelope.nonce,
        ciphertext: &envelope.ciphertext,
        signer_device_id: envelope.signer_device_id,
    })?)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RosterSigningFields<'a> {
    protocol_version: u16,
    epoch: u64,
    previous_digest: &'a str,
    grants: &'a [PeerGrant],
    signer_device_id: covalent_protocol::DeviceId,
}

fn roster_signing_bytes(roster: &SignedRoster) -> Result<Vec<u8>, CoreError> {
    Ok(serde_json::to_vec(&RosterSigningFields {
        protocol_version: roster.protocol_version,
        epoch: roster.epoch,
        previous_digest: &roster.previous_digest,
        grants: &roster.grants,
        signer_device_id: roster.signer_device_id,
    })?)
}

fn manifest_context(backup_id: covalent_protocol::BackupId, key_epoch: u64) -> Vec<u8> {
    let mut context = Vec::with_capacity(64);
    context.extend_from_slice(b"covalent/manifest-record/v1\0");
    context.extend_from_slice(backup_id.to_string().as_bytes());
    context.extend_from_slice(&key_epoch.to_be_bytes());
    context
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use covalent_protocol::{BackupId, Manifest, ReplicaIntent};

    use super::*;

    fn empty_manifest(backup_id: BackupId) -> Manifest {
        Manifest {
            protocol_version: PROTOCOL_VERSION,
            backup_id,
            snapshot_id: "snapshot-1".to_owned(),
            created_at_unix_ms: 1,
            replica_intent: ReplicaIntent::default(),
            entries: Vec::new(),
            provider_acknowledgements: BTreeMap::new(),
        }
    }

    #[test]
    fn signed_encrypted_manifest_round_trip_and_tamper_rejection() {
        let identity = DeviceIdentity::generate();
        let key = BackupKey::generate();
        let manifest = empty_manifest(BackupId::new());
        assert!(encrypt_manifest(&manifest, 0, &key, &identity).is_err());
        let envelope = encrypt_manifest(&manifest, 4, &key, &identity).expect("encrypt");
        assert_eq!(
            decrypt_manifest(&envelope, &key, &identity.public_identity()).expect("decrypt"),
            manifest
        );
        let mut tampered = envelope;
        tampered.ciphertext.push('A');
        assert!(decrypt_manifest(&tampered, &key, &identity.public_identity()).is_err());
    }

    #[test]
    fn roster_rejects_rollback_and_fork() {
        let identity = DeviceIdentity::generate();
        let previous = "a".repeat(64);
        let roster = SignedRosterBuilder::new(2, &previous)
            .sign(&identity)
            .expect("roster");
        verify_roster(&roster, &identity.public_identity(), 1, &previous).expect("valid");
        assert!(verify_roster(&roster, &identity.public_identity(), 2, &previous).is_err());
        assert!(verify_roster(&roster, &identity.public_identity(), 1, "fork").is_err());
    }
}
