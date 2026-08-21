use std::collections::BTreeSet;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use covalent_protocol::{BackupId, DeviceId, PeerGrant, PeerRole, TransportBinding};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::atomic::{read_json_bounded, write_json_atomic};
use crate::{BackupKey, CoreError, DeviceIdentity, PublicIdentity, StoredSnapshot};

const RECOVERY_KIT_SCHEMA_VERSION: u16 = 1;
const RECOVERY_CAPSULE_SCHEMA_VERSION: u16 = 1;
const RECOVERY_MASTER_SCHEMA_VERSION: u16 = 1;
const RECOVERY_CIPHER_SUITE: &str = "XCHACHA20-POLY1305-HKDF-SHA256";
const RECOVERY_KIT_SIGNATURE_DOMAIN: &[u8] = b"covalent/recovery-kit/v1";
const RECOVERY_CAPSULE_SIGNATURE_DOMAIN: &[u8] = b"covalent/recovery-capsule/v1";
pub(crate) const MAX_RECOVERY_KIT_BYTES: usize = 16 * 1_024 * 1_024;
pub(crate) const MAX_RECOVERY_CAPSULE_BYTES: usize = 320 * 1_024 * 1_024;

/// High-entropy secret used only to unlock an exported stable recovery kit.
pub struct RecoveryUnlockKey(Zeroizing<[u8; 32]>);

impl RecoveryUnlockKey {
    /// Generates a printable recovery secret from the operating-system RNG.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(Zeroizing::new(bytes))
    }

    /// Imports an exact 256-bit secret from protected user input.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Returns base64url text suitable for a password manager or printed recovery record.
    #[must_use]
    pub fn expose_base64(&self) -> Zeroizing<String> {
        Zeroizing::new(URL_SAFE_NO_PAD.encode(self.0.as_ref()))
    }

    /// Parses the exact high-entropy format; human passwords are intentionally rejected.
    pub fn from_base64(value: &str) -> Result<Self, CoreError> {
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| CoreError::InvalidKeyMaterial)?,
        );
        let bytes: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        Ok(Self::from_bytes(bytes))
    }
}

pub(crate) struct RecoveryMasterKey(Zeroizing<[u8; 32]>);

impl RecoveryMasterKey {
    fn generate() -> Self {
        let mut bytes = [0_u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(Zeroizing::new(bytes))
    }

    fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    fn bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(*self.0)
    }
}

impl Clone for RecoveryMasterKey {
    fn clone(&self) -> Self {
        Self(self.bytes())
    }
}

impl std::fmt::Debug for RecoveryMasterKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RecoveryMasterKey([REDACTED])")
    }
}

/// Stable, user-exported identity and recovery-master envelope.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryKit {
    pub schema_version: u16,
    pub cipher_suite: String,
    pub owner_device_id: DeviceId,
    pub owner_public_key: String,
    pub owner_display_name: String,
    pub nonce: String,
    pub ciphertext: String,
    pub signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryKitPayload {
    device_secret: Zeroizing<String>,
    recovery_master: Zeroizing<String>,
    provider_directory: Vec<RecoveryProviderDirectoryEntry>,
}

/// Provider identity and exact signed TLS endpoint retained inside recovery envelopes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryProviderDirectoryEntry {
    pub grant: PeerGrant,
    pub transport: TransportBinding,
}

/// Per-snapshot signed encrypted catalog replicated only to selected providers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryCapsule {
    pub schema_version: u16,
    pub cipher_suite: String,
    pub backup_id: BackupId,
    pub snapshot_id: String,
    pub key_epoch: u64,
    pub committed_at_unix_ms: u64,
    pub nonce: String,
    pub ciphertext: String,
    pub signer_device_id: DeviceId,
    pub signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecoveryCapsulePayload {
    snapshot: StoredSnapshot,
    backup_display_name: String,
    backup_key: Zeroizing<String>,
    provider_directory: Vec<RecoveryProviderDirectoryEntry>,
}

pub(crate) struct OpenedRecoveryKit {
    pub identity: DeviceIdentity,
    pub master: RecoveryMasterKey,
    pub display_name: String,
    pub provider_directory: Vec<RecoveryProviderDirectoryEntry>,
}

pub(crate) struct OpenedRecoveryCapsule {
    pub snapshot: StoredSnapshot,
    pub backup_display_name: String,
    pub backup_key: BackupKey,
    pub provider_directory: Vec<RecoveryProviderDirectoryEntry>,
}

impl RecoveryKit {
    pub(crate) fn seal(
        identity: &DeviceIdentity,
        display_name: &str,
        master: &RecoveryMasterKey,
        unlock: &RecoveryUnlockKey,
        provider_directory: Vec<RecoveryProviderDirectoryEntry>,
    ) -> Result<Self, CoreError> {
        validate_display_name(display_name)?;
        validate_provider_directory(&provider_directory)?;
        let public = identity.public_identity();
        let context = kit_context(
            RECOVERY_KIT_SCHEMA_VERSION,
            public.device_id,
            &public.public_key,
            display_name,
        )?;
        let key = derive_key(unlock.0.as_ref(), &context, b"covalent/recovery-kit-key/v1")?;
        let nonce = random_nonce();
        let identity_secret = identity.recovery_secret();
        let master_bytes = master.bytes();
        let payload = RecoveryKitPayload {
            device_secret: Zeroizing::new(URL_SAFE_NO_PAD.encode(identity_secret.as_ref())),
            recovery_master: Zeroizing::new(URL_SAFE_NO_PAD.encode(master_bytes.as_ref())),
            provider_directory,
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload)?);
        let ciphertext = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_ref(),
                    aad: &context,
                },
            )
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let mut kit = Self {
            schema_version: RECOVERY_KIT_SCHEMA_VERSION,
            cipher_suite: RECOVERY_CIPHER_SUITE.to_owned(),
            owner_device_id: public.device_id,
            owner_public_key: public.public_key,
            owner_display_name: display_name.to_owned(),
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
            signature: String::new(),
        };
        kit.signature = identity.sign(RECOVERY_KIT_SIGNATURE_DOMAIN, &kit_signing_bytes(&kit)?);
        Ok(kit)
    }

    pub(crate) fn open(&self, unlock: &RecoveryUnlockKey) -> Result<OpenedRecoveryKit, CoreError> {
        self.validate_header()?;
        let owner =
            PublicIdentity::from_encoded(self.owner_device_id, self.owner_public_key.clone())?;
        owner.verify(
            RECOVERY_KIT_SIGNATURE_DOMAIN,
            &kit_signing_bytes(self)?,
            &self.signature,
        )?;
        let context = kit_context(
            self.schema_version,
            self.owner_device_id,
            &self.owner_public_key,
            &self.owner_display_name,
        )?;
        let key = derive_key(unlock.0.as_ref(), &context, b"covalent/recovery-kit-key/v1")?;
        let nonce = decode_nonce(&self.nonce)?;
        let ciphertext = decode_bounded(&self.ciphertext, MAX_RECOVERY_KIT_BYTES)?;
        let plaintext = Zeroizing::new(
            XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
                .decrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: &context,
                    },
                )
                .map_err(|_| CoreError::AuthenticationFailed)?,
        );
        let payload: RecoveryKitPayload = serde_json::from_slice(plaintext.as_ref())?;
        validate_provider_directory(&payload.provider_directory)?;
        let identity_secret = decode_secret(&payload.device_secret)?;
        let master = decode_secret(&payload.recovery_master)?;
        let identity = DeviceIdentity::from_recovery_secret(self.owner_device_id, identity_secret)?;
        if identity.public_identity() != owner {
            return Err(CoreError::IdentityMismatch);
        }
        Ok(OpenedRecoveryKit {
            identity,
            master: RecoveryMasterKey::from_bytes(master),
            display_name: self.owner_display_name.clone(),
            provider_directory: payload.provider_directory,
        })
    }

    fn validate_header(&self) -> Result<(), CoreError> {
        validate_suite(self.schema_version, &self.cipher_suite)?;
        validate_display_name(&self.owner_display_name)?;
        if self.owner_public_key.len() > 128 || self.signature.is_empty() {
            return Err(CoreError::InvalidKeyMaterial);
        }
        Ok(())
    }
}

impl RecoveryCapsule {
    pub(crate) fn seal(
        snapshot: &StoredSnapshot,
        backup_display_name: &str,
        backup_key: &BackupKey,
        master: &RecoveryMasterKey,
        identity: &DeviceIdentity,
        provider_directory: Vec<RecoveryProviderDirectoryEntry>,
    ) -> Result<Self, CoreError> {
        if snapshot.envelope.signer_device_id != identity.device_id() {
            return Err(CoreError::IdentityMismatch);
        }
        validate_backup_display_name(backup_display_name)?;
        validate_provider_directory(&provider_directory)?;
        let context = capsule_context(
            RECOVERY_CAPSULE_SCHEMA_VERSION,
            snapshot.backup_id,
            &snapshot.snapshot_id,
            snapshot.envelope.key_epoch,
            snapshot.committed_at_unix_ms,
            identity.device_id(),
        )?;
        let key = derive_key(
            master.0.as_ref(),
            &context,
            b"covalent/recovery-capsule-key/v1",
        )?;
        let nonce = random_nonce();
        let backup_key = backup_key.to_bytes();
        let payload = RecoveryCapsulePayload {
            snapshot: snapshot.clone(),
            backup_display_name: backup_display_name.to_owned(),
            backup_key: Zeroizing::new(URL_SAFE_NO_PAD.encode(backup_key.as_ref())),
            provider_directory,
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload)?);
        if plaintext.len() > MAX_RECOVERY_CAPSULE_BYTES {
            return Err(CoreError::ResourceLimit("recovery capsule"));
        }
        let ciphertext = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_ref(),
                    aad: &context,
                },
            )
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let mut capsule = Self {
            schema_version: RECOVERY_CAPSULE_SCHEMA_VERSION,
            cipher_suite: RECOVERY_CIPHER_SUITE.to_owned(),
            backup_id: snapshot.backup_id,
            snapshot_id: snapshot.snapshot_id.clone(),
            key_epoch: snapshot.envelope.key_epoch,
            committed_at_unix_ms: snapshot.committed_at_unix_ms,
            nonce: URL_SAFE_NO_PAD.encode(nonce),
            ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
            signer_device_id: identity.device_id(),
            signature: String::new(),
        };
        capsule.signature = identity.sign(
            RECOVERY_CAPSULE_SIGNATURE_DOMAIN,
            &capsule_signing_bytes(&capsule)?,
        );
        Ok(capsule)
    }

    pub(crate) fn open(
        &self,
        master: &RecoveryMasterKey,
        owner: &PublicIdentity,
    ) -> Result<OpenedRecoveryCapsule, CoreError> {
        validate_suite(self.schema_version, &self.cipher_suite)?;
        if self.signer_device_id != owner.device_id {
            return Err(CoreError::IdentityMismatch);
        }
        owner.verify(
            RECOVERY_CAPSULE_SIGNATURE_DOMAIN,
            &capsule_signing_bytes(self)?,
            &self.signature,
        )?;
        let context = capsule_context(
            self.schema_version,
            self.backup_id,
            &self.snapshot_id,
            self.key_epoch,
            self.committed_at_unix_ms,
            self.signer_device_id,
        )?;
        let key = derive_key(
            master.0.as_ref(),
            &context,
            b"covalent/recovery-capsule-key/v1",
        )?;
        let nonce = decode_nonce(&self.nonce)?;
        let ciphertext = decode_bounded(&self.ciphertext, MAX_RECOVERY_CAPSULE_BYTES)?;
        let plaintext = Zeroizing::new(
            XChaCha20Poly1305::new(Key::from_slice(key.as_ref()))
                .decrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &ciphertext,
                        aad: &context,
                    },
                )
                .map_err(|_| CoreError::AuthenticationFailed)?,
        );
        let payload: RecoveryCapsulePayload = serde_json::from_slice(plaintext.as_ref())?;
        validate_provider_directory(&payload.provider_directory)?;
        if payload.snapshot.backup_id != self.backup_id
            || payload.snapshot.snapshot_id != self.snapshot_id
            || payload.snapshot.envelope.key_epoch != self.key_epoch
            || payload.snapshot.committed_at_unix_ms != self.committed_at_unix_ms
            || payload.snapshot.envelope.signer_device_id != self.signer_device_id
        {
            return Err(CoreError::AuthenticationFailed);
        }
        let backup_key = BackupKey::from_bytes(decode_secret(&payload.backup_key)?);
        validate_backup_display_name(&payload.backup_display_name)?;
        Ok(OpenedRecoveryCapsule {
            snapshot: payload.snapshot,
            backup_display_name: payload.backup_display_name,
            backup_key,
            provider_directory: payload.provider_directory,
        })
    }

    /// Stable opaque identifier used by providers for immutable capsule storage.
    pub fn capsule_id(&self) -> Result<String, CoreError> {
        Ok(blake3::hash(&capsule_signing_bytes(self)?)
            .to_hex()
            .to_string())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedRecoveryMaster {
    schema_version: u16,
    key: Zeroizing<String>,
}

pub(crate) fn load_or_create_recovery_master(path: &Path) -> Result<RecoveryMasterKey, CoreError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(CoreError::InvalidState(
                    "recovery master path is not a regular file".to_owned(),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(CoreError::InvalidState(
                        "recovery master permissions are too broad".to_owned(),
                    ));
                }
            }
            let persisted: PersistedRecoveryMaster = read_json_bounded(path, 16 * 1_024)?;
            if persisted.schema_version != RECOVERY_MASTER_SCHEMA_VERSION {
                return Err(CoreError::InvalidState(
                    "unsupported recovery master schema".to_owned(),
                ));
            }
            Ok(RecoveryMasterKey::from_bytes(decode_secret(
                &persisted.key,
            )?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let master = RecoveryMasterKey::generate();
            persist_recovery_master(path, &master)?;
            Ok(master)
        }
        Err(source) => Err(CoreError::Io {
            operation: "inspect recovery master",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub(crate) fn persist_recovery_master(
    path: &Path,
    master: &RecoveryMasterKey,
) -> Result<(), CoreError> {
    let bytes = master.bytes();
    write_json_atomic(
        path,
        &PersistedRecoveryMaster {
            schema_version: RECOVERY_MASTER_SCHEMA_VERSION,
            key: Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_ref())),
        },
        true,
    )
}

fn random_nonce() -> [u8; 24] {
    let mut nonce = [0_u8; 24];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

fn derive_key(
    secret: &[u8],
    context: &[u8],
    label: &[u8],
) -> Result<Zeroizing<[u8; 32]>, CoreError> {
    let hkdf = Hkdf::<Sha256>::new(Some(context), secret);
    let mut output = Zeroizing::new([0_u8; 32]);
    hkdf.expand(label, output.as_mut())
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    Ok(output)
}

fn decode_secret(value: &str) -> Result<[u8; 32], CoreError> {
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(value)
            .map_err(|_| CoreError::InvalidKeyMaterial)?,
    );
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::InvalidKeyMaterial)
}

fn decode_nonce(value: &str) -> Result<[u8; 24], CoreError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    decoded
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::InvalidKeyMaterial)
}

fn decode_bounded(value: &str, maximum: usize) -> Result<Vec<u8>, CoreError> {
    if value.len() > maximum.saturating_mul(2) {
        return Err(CoreError::ResourceLimit("recovery envelope"));
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    if decoded.len() > maximum {
        return Err(CoreError::ResourceLimit("recovery envelope"));
    }
    Ok(decoded)
}

fn validate_suite(version: u16, suite: &str) -> Result<(), CoreError> {
    if version != RECOVERY_KIT_SCHEMA_VERSION || suite != RECOVERY_CIPHER_SUITE {
        return Err(CoreError::UnsupportedCipherSuite(suite.to_owned()));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), CoreError> {
    if value.trim().is_empty() || value.len() > 80 || value.chars().any(char::is_control) {
        return Err(CoreError::InvalidState(
            "invalid recovery owner display name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_backup_display_name(value: &str) -> Result<(), CoreError> {
    if value.trim().is_empty() || value.len() > 120 || value.chars().any(char::is_control) {
        return Err(CoreError::InvalidState(
            "invalid recovery backup display name".to_owned(),
        ));
    }
    Ok(())
}

fn validate_provider_directory(
    entries: &[RecoveryProviderDirectoryEntry],
) -> Result<(), CoreError> {
    if entries.len() > 128 {
        return Err(CoreError::ResourceLimit("recovery provider directory"));
    }
    let mut ids = BTreeSet::new();
    for entry in entries {
        if entry.grant.revoked
            || !entry.grant.roles.contains(&PeerRole::StorageProvider)
            || entry.transport.peer_id != entry.grant.peer_device_id
            || !ids.insert(entry.grant.peer_device_id)
        {
            return Err(CoreError::InvalidState(
                "invalid recovery provider directory".to_owned(),
            ));
        }
        let identity = PublicIdentity::from_encoded(
            entry.grant.peer_device_id,
            entry.grant.public_key.clone(),
        )?;
        crate::pairing::validate_transport_binding(
            &entry.transport,
            &identity,
            &entry.grant.display_name,
        )?;
    }
    Ok(())
}

fn kit_context(
    schema_version: u16,
    owner_device_id: DeviceId,
    owner_public_key: &str,
    owner_display_name: &str,
) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Context<'a> {
        schema_version: u16,
        cipher_suite: &'static str,
        owner_device_id: DeviceId,
        owner_public_key: &'a str,
        owner_display_name: &'a str,
    }
    Ok(serde_json::to_vec(&Context {
        schema_version,
        cipher_suite: RECOVERY_CIPHER_SUITE,
        owner_device_id,
        owner_public_key,
        owner_display_name,
    })?)
}

fn capsule_context(
    schema_version: u16,
    backup_id: BackupId,
    snapshot_id: &str,
    key_epoch: u64,
    committed_at_unix_ms: u64,
    signer_device_id: DeviceId,
) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Context<'a> {
        schema_version: u16,
        cipher_suite: &'static str,
        backup_id: BackupId,
        snapshot_id: &'a str,
        key_epoch: u64,
        committed_at_unix_ms: u64,
        signer_device_id: DeviceId,
    }
    Ok(serde_json::to_vec(&Context {
        schema_version,
        cipher_suite: RECOVERY_CIPHER_SUITE,
        backup_id,
        snapshot_id,
        key_epoch,
        committed_at_unix_ms,
        signer_device_id,
    })?)
}

fn kit_signing_bytes(kit: &RecoveryKit) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Signing<'a> {
        schema_version: u16,
        cipher_suite: &'a str,
        owner_device_id: DeviceId,
        owner_public_key: &'a str,
        owner_display_name: &'a str,
        nonce: &'a str,
        ciphertext: &'a str,
    }
    Ok(serde_json::to_vec(&Signing {
        schema_version: kit.schema_version,
        cipher_suite: &kit.cipher_suite,
        owner_device_id: kit.owner_device_id,
        owner_public_key: &kit.owner_public_key,
        owner_display_name: &kit.owner_display_name,
        nonce: &kit.nonce,
        ciphertext: &kit.ciphertext,
    })?)
}

fn capsule_signing_bytes(capsule: &RecoveryCapsule) -> Result<Vec<u8>, CoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Signing<'a> {
        schema_version: u16,
        cipher_suite: &'a str,
        backup_id: BackupId,
        snapshot_id: &'a str,
        key_epoch: u64,
        committed_at_unix_ms: u64,
        nonce: &'a str,
        ciphertext: &'a str,
        signer_device_id: DeviceId,
    }
    Ok(serde_json::to_vec(&Signing {
        schema_version: capsule.schema_version,
        cipher_suite: &capsule.cipher_suite,
        backup_id: capsule.backup_id,
        snapshot_id: &capsule.snapshot_id,
        key_epoch: capsule.key_epoch,
        committed_at_unix_ms: capsule.committed_at_unix_ms,
        nonce: &capsule.nonce,
        ciphertext: &capsule.ciphertext,
        signer_device_id: capsule.signer_device_id,
    })?)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use covalent_protocol::{ManifestEnvelope, PROTOCOL_VERSION};

    use super::*;

    #[test]
    fn kit_and_capsule_reject_wrong_keys_and_tampering() {
        let identity = DeviceIdentity::generate();
        let master = RecoveryMasterKey::generate();
        let unlock = RecoveryUnlockKey::generate();
        let kit =
            RecoveryKit::seal(&identity, "Owner Mac", &master, &unlock, Vec::new()).expect("kit");
        let opened = kit.open(&unlock).expect("open kit");
        assert_eq!(opened.identity.device_id(), identity.device_id());
        assert!(kit.open(&RecoveryUnlockKey::generate()).is_err());

        let backup_id = BackupId::new();
        let key = BackupKey::generate();
        let snapshot = StoredSnapshot::new(
            backup_id,
            "snapshot-1",
            ManifestEnvelope {
                protocol_version: PROTOCOL_VERSION,
                backup_id,
                key_epoch: 1,
                cipher_suite: "test".to_owned(),
                nonce: "test".to_owned(),
                ciphertext: "test".to_owned(),
                signer_device_id: identity.device_id(),
                signature: "test".to_owned(),
            },
            BTreeSet::new(),
            1,
        )
        .expect("snapshot");
        let capsule =
            RecoveryCapsule::seal(&snapshot, "Documents", &key, &master, &identity, Vec::new())
                .expect("capsule");
        let opened = capsule
            .open(&master, &identity.public_identity())
            .expect("open capsule");
        assert_eq!(opened.snapshot, snapshot);
        assert_eq!(opened.backup_key.to_bytes(), key.to_bytes());

        let mut tampered = capsule;
        tampered.snapshot_id.push('x');
        assert!(tampered.open(&master, &identity.public_identity()).is_err());
    }
}
