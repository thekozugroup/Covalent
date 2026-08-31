use std::fmt;
use std::fs;
use std::io::Write;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_protocol::DeviceId;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::CoreError;
use crate::atomic::{read_json_bounded, sync_directory, write_atomic_noclobber, write_json_atomic};
use crate::{KeyProtector, WrappedSecret, state_secret_context};

const LEGACY_IDENTITY_SCHEMA_VERSION: u16 = 1;
const PROTECTED_IDENTITY_SCHEMA_VERSION: u16 = 2;
const MAX_IDENTITY_FILE_BYTES: usize = 16 * 1_024;
const IDENTITY_SECRET_PURPOSE: &str = "device-identity";

/// Public device identity safe to share during pairing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublicIdentity {
    /// Stable device identifier.
    pub device_id: DeviceId,
    /// Base64url Ed25519 verification key.
    pub public_key: String,
    /// Short BLAKE3 fingerprint for explicit comparison and storage.
    pub fingerprint: String,
}

impl PublicIdentity {
    /// Builds a validated public identity from an encoded Ed25519 key.
    pub fn from_encoded(device_id: DeviceId, public_key: String) -> Result<Self, CoreError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&public_key)
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key =
            VerifyingKey::from_bytes(&key_bytes).map_err(|_| CoreError::InvalidKeyMaterial)?;
        Ok(Self {
            device_id,
            public_key,
            fingerprint: public_fingerprint(&key),
        })
    }

    /// Parses and validates the encoded verification key and fingerprint.
    pub fn verifying_key(&self) -> Result<VerifyingKey, CoreError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(&self.public_key)
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key_bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let key =
            VerifyingKey::from_bytes(&key_bytes).map_err(|_| CoreError::InvalidKeyMaterial)?;
        if public_fingerprint(&key) != self.fingerprint {
            return Err(CoreError::IdentityMismatch);
        }
        Ok(key)
    }

    /// Verifies a domain-separated signed message.
    pub fn verify(&self, domain: &[u8], message: &[u8], signature: &str) -> Result<(), CoreError> {
        let key = self.verifying_key()?;
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| CoreError::AuthenticationFailed)?;
        let signature =
            Signature::from_slice(&signature_bytes).map_err(|_| CoreError::AuthenticationFailed)?;
        key.verify(&signing_message(domain, message), &signature)
            .map_err(|_| CoreError::AuthenticationFailed)
    }
}

/// Locally generated long-lived Ed25519 device identity.
pub struct DeviceIdentity {
    device_id: DeviceId,
    signing_key: SigningKey,
}

impl DeviceIdentity {
    /// Generates a fresh identity from the operating-system random source.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            device_id: DeviceId::new(),
            signing_key: SigningKey::generate(&mut OsRng),
        }
    }

    pub(crate) fn from_recovery_secret(
        device_id: DeviceId,
        mut secret: [u8; 32],
    ) -> Result<Self, CoreError> {
        let signing_key = SigningKey::from_bytes(&secret);
        secret.zeroize();
        let identity = Self {
            device_id,
            signing_key,
        };
        identity.public_identity().verifying_key()?;
        Ok(identity)
    }

    pub(crate) fn recovery_secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.signing_key.to_bytes())
    }

    pub(crate) fn persist_recovered_protected(
        &self,
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<(), CoreError> {
        self.persist_protected(path, state_root, protector)
    }

    /// Loads or creates an identity whose private material is KEK-wrapped.
    pub(crate) fn load_or_create_protected(
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, CoreError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_identity_metadata(&metadata)?;
                Self::load_protected_or_migrate(path, state_root, protector)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
                if identity.persist_protected_new(path, state_root, protector)? {
                    Ok(identity)
                } else {
                    Self::load_protected_or_migrate(path, state_root, protector)
                }
            }
            Err(source) => Err(CoreError::Io {
                operation: "inspect protected device identity",
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Loads an existing identity or creates one with crash-safe, private persistence.
    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(CoreError::InvalidState(
                        "identity path must be a regular file".to_owned(),
                    ));
                }
                Self::load(path)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let identity = Self::generate();
                match identity.persist_new(path) {
                    Ok(()) => Ok(identity),
                    Err(CoreError::Io { source, .. })
                        if source.kind() == std::io::ErrorKind::AlreadyExists =>
                    {
                        Self::load(path)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(source) => Err(CoreError::Io {
                operation: "inspect device identity",
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Loads and validates a private identity record.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CoreError> {
        let path = path.as_ref();
        let metadata = fs::symlink_metadata(path).map_err(|source| CoreError::Io {
            operation: "inspect private identity file",
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CoreError::InvalidState(
                "identity path must be a regular file".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(CoreError::InvalidState(
                    "private identity permissions are too broad".to_owned(),
                ));
            }
        }
        let persisted: PersistedIdentity = read_json_bounded(path, MAX_IDENTITY_FILE_BYTES)?;
        if persisted.schema_version != LEGACY_IDENTITY_SCHEMA_VERSION {
            return Err(CoreError::InvalidState(
                "unsupported identity schema".to_owned(),
            ));
        }
        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(&persisted.private_key)
                .map_err(|_| CoreError::InvalidKeyMaterial)?,
        );
        let mut bytes: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        let signing_key = SigningKey::from_bytes(&bytes);
        bytes.zeroize();
        let identity = Self {
            device_id: persisted.device_id,
            signing_key,
        };
        if identity.public_identity().public_key != persisted.public_key {
            return Err(CoreError::IdentityMismatch);
        }
        Ok(identity)
    }

    /// Stable device identifier.
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    /// Public pairing representation.
    #[must_use]
    pub fn public_identity(&self) -> PublicIdentity {
        let verifying_key = self.signing_key.verifying_key();
        PublicIdentity {
            device_id: self.device_id,
            public_key: URL_SAFE_NO_PAD.encode(verifying_key.as_bytes()),
            fingerprint: public_fingerprint(&verifying_key),
        }
    }

    /// Signs a domain-separated protocol transcript.
    #[must_use]
    pub fn sign(&self, domain: &[u8], message: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(
            self.signing_key
                .sign(&signing_message(domain, message))
                .to_bytes(),
        )
    }

    /// Verifies a signature made by this identity.
    pub fn verify_own(
        &self,
        domain: &[u8],
        message: &[u8],
        signature: &str,
    ) -> Result<(), CoreError> {
        self.public_identity().verify(domain, message, signature)
    }

    fn persist_new(&self, path: &Path) -> Result<(), CoreError> {
        let parent = path
            .parent()
            .ok_or_else(|| CoreError::InvalidState("identity path has no parent".to_owned()))?;
        fs::create_dir_all(parent).map_err(|source| CoreError::Io {
            operation: "create identity directory",
            path: parent.to_path_buf(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|source| {
                CoreError::Io {
                    operation: "protect identity directory",
                    path: parent.to_path_buf(),
                    source,
                }
            })?;
        }
        let private_bytes = Zeroizing::new(self.signing_key.to_bytes());
        let persisted = PersistedIdentity {
            schema_version: LEGACY_IDENTITY_SCHEMA_VERSION,
            device_id: self.device_id,
            public_key: self.public_identity().public_key,
            private_key: Zeroizing::new(URL_SAFE_NO_PAD.encode(private_bytes.as_ref())),
        };
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(&persisted)?);
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| CoreError::Io {
                operation: "stage device identity",
                path: path.to_path_buf(),
                source,
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| CoreError::Io {
                    operation: "protect staged device identity",
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        temporary
            .write_all(bytes.as_ref())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| CoreError::Io {
                operation: "sync staged device identity",
                path: path.to_path_buf(),
                source,
            })?;
        temporary
            .persist_noclobber(path)
            .map_err(|error| CoreError::Io {
                operation: "commit device identity",
                path: path.to_path_buf(),
                source: error.error,
            })?;
        sync_directory(parent)
    }

    fn load_protected_or_migrate(
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<Self, CoreError> {
        let value: serde_json::Value = read_json_bounded(path, MAX_IDENTITY_FILE_BYTES)?;
        let schema_version = value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| CoreError::InvalidState("identity schema is missing".to_owned()))?;
        match schema_version {
            _ if schema_version == u64::from(PROTECTED_IDENTITY_SCHEMA_VERSION) => {
                let persisted: ProtectedIdentity = serde_json::from_value(value)?;
                let context = state_secret_context(state_root, "identity.json");
                let plaintext = persisted.protected_private_key.open(
                    protector,
                    IDENTITY_SECRET_PURPOSE,
                    &context,
                )?;
                let payload: ProtectedIdentityPayload = serde_json::from_slice(&plaintext)?;
                if payload.device_id != persisted.device_id {
                    return Err(CoreError::AuthenticationFailed);
                }
                let identity =
                    identity_from_encoded_secret(payload.device_id, &payload.private_key)?;
                if identity.public_identity().public_key != persisted.public_key {
                    return Err(CoreError::IdentityMismatch);
                }
                Ok(identity)
            }
            _ if schema_version == u64::from(LEGACY_IDENTITY_SCHEMA_VERSION) => {
                let identity = Self::load(path)?;
                identity.persist_protected(path, state_root, protector)?;
                Ok(identity)
            }
            _ => Err(CoreError::InvalidState(
                "unsupported identity schema".to_owned(),
            )),
        }
    }

    fn protected_record(
        &self,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<ProtectedIdentity, CoreError> {
        let secret = Zeroizing::new(self.signing_key.to_bytes());
        let payload = ProtectedIdentityPayload {
            device_id: self.device_id,
            private_key: Zeroizing::new(URL_SAFE_NO_PAD.encode(secret.as_ref())),
        };
        let plaintext = Zeroizing::new(serde_json::to_vec(&payload)?);
        let context = state_secret_context(state_root, "identity.json");
        Ok(ProtectedIdentity {
            schema_version: PROTECTED_IDENTITY_SCHEMA_VERSION,
            device_id: self.device_id,
            public_key: self.public_identity().public_key,
            protected_private_key: WrappedSecret::protect(
                protector,
                IDENTITY_SECRET_PURPOSE,
                &context,
                plaintext,
            )?,
        })
    }

    fn persist_protected(
        &self,
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<(), CoreError> {
        write_json_atomic(path, &self.protected_record(state_root, protector)?, true)
    }

    fn persist_protected_new(
        &self,
        path: &Path,
        state_root: &Path,
        protector: &dyn KeyProtector,
    ) -> Result<bool, CoreError> {
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(
            &self.protected_record(state_root, protector)?,
        )?);
        write_atomic_noclobber(path, &bytes, true)
    }
}

fn identity_from_encoded_secret(
    device_id: DeviceId,
    encoded: &str,
) -> Result<DeviceIdentity, CoreError> {
    let decoded = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| CoreError::InvalidKeyMaterial)?,
    );
    let mut bytes: [u8; 32] = decoded
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    let identity = DeviceIdentity::from_recovery_secret(device_id, bytes)?;
    bytes.zeroize();
    Ok(identity)
}

fn validate_identity_metadata(metadata: &fs::Metadata) -> Result<(), CoreError> {
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CoreError::InvalidState(
            "identity path must be a regular file".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(CoreError::InvalidState(
                "private identity permissions are too broad".to_owned(),
            ));
        }
    }
    Ok(())
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .field("signing_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedIdentity {
    schema_version: u16,
    device_id: DeviceId,
    public_key: String,
    private_key: Zeroizing<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtectedIdentity {
    schema_version: u16,
    device_id: DeviceId,
    public_key: String,
    protected_private_key: WrappedSecret,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProtectedIdentityPayload {
    device_id: DeviceId,
    private_key: Zeroizing<String>,
}

fn signing_message(domain: &[u8], message: &[u8]) -> Vec<u8> {
    let mut signed = Vec::with_capacity(24 + domain.len() + message.len());
    signed.extend_from_slice(b"covalent/signature/v1\0");
    signed.extend_from_slice(&(domain.len() as u64).to_be_bytes());
    signed.extend_from_slice(domain);
    signed.extend_from_slice(message);
    signed
}

fn public_fingerprint(key: &VerifyingKey) -> String {
    blake3::hash(key.as_bytes()).to_hex()[..20].to_owned()
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn persisted_identity_is_stable_and_signatures_are_bound() {
        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("identity.json");
        let first = DeviceIdentity::load_or_create(&path).expect("create identity");
        let signature = first.sign(b"test", b"message");
        first
            .public_identity()
            .verify(b"test", b"message", &signature)
            .expect("valid signature");
        assert!(
            first
                .public_identity()
                .verify(b"other", b"message", &signature)
                .is_err()
        );
        let second = DeviceIdentity::load_or_create(&path).expect("load identity");
        assert_eq!(first.device_id(), second.device_id());
        assert_eq!(first.public_identity(), second.public_identity());
    }

    #[cfg(unix)]
    #[test]
    fn private_identity_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempdir().expect("temporary directory");
        let path = directory.path().join("state/identity.json");
        DeviceIdentity::load_or_create(&path).expect("identity");
        assert_eq!(
            fs::metadata(path).expect("metadata").permissions().mode() & 0o777,
            0o600
        );
    }
}
