//! Versioned, metadata-bound wrapping for long-lived secrets.
//!
//! This module deliberately consumes both the KEK and plaintext. It therefore
//! never leaves an additional caller-owned copy of either secret behind.

use std::fmt;
use std::path::Path;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::CoreError;

const SCHEMA_VERSION: u8 = 1;
const CIPHER_SUITE: &str = "XCHACHA20-POLY1305+HKDF-SHA256";
const KEK_LENGTH: usize = 32;
const SALT_LENGTH: usize = 32;
const NONCE_LENGTH: usize = 24;
const AEAD_TAG_LENGTH: usize = 16;
const MAX_PURPOSE_LENGTH: usize = 128;
const MAX_CONTEXT_LENGTH: usize = 4096;
const MAX_PLAINTEXT_LENGTH: usize = 65_536;
const DOMAIN: &[u8] = b"covalent/key-envelope/v1\0";
const STATE_CONTEXT_DOMAIN: &[u8] = b"covalent/state-secret-context/v1\0";

/// Platform-owned source of versioned key-encryption keys.
///
/// Implementations bridge a platform keystore, hardware-backed key, or an
/// explicitly provisioned headless secret. The core never persists a KEK and
/// requests a fresh, owned value for each envelope operation so the returned
/// bytes can be zeroized immediately.
pub trait KeyProtector: Send + Sync {
    /// Version used for newly wrapped records.
    fn current_key_version(&self) -> Result<u32, CoreError>;

    /// Returns the exact KEK version required by an existing envelope.
    fn key_encryption_key(&self, key_version: u32) -> Result<KeyEncryptionKey, CoreError>;
}

/// Explicit in-process KEK source for tests and provisioned headless runtimes.
///
/// Native apps should implement [`KeyProtector`] with their platform secure
/// storage rather than retaining the KEK in process memory for the node's full
/// lifetime.
pub struct StaticKeyProtector {
    key_version: u32,
    key: Zeroizing<[u8; KEK_LENGTH]>,
}

impl StaticKeyProtector {
    /// Takes ownership of an explicitly provisioned KEK.
    pub fn new(key_version: u32, mut key: [u8; KEK_LENGTH]) -> Result<Self, CoreError> {
        if key_version == 0 {
            return Err(CoreError::InvalidState(
                "invalid key-protection version".to_owned(),
            ));
        }
        let protector = Self {
            key_version,
            key: Zeroizing::new(key),
        };
        key.zeroize();
        Ok(protector)
    }

    /// Imports a base64url, unpadded 256-bit KEK from explicit provisioning.
    pub fn from_base64(key_version: u32, encoded: &str) -> Result<Self, CoreError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        let decoded = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(encoded.trim())
                .map_err(|_| CoreError::InvalidKeyMaterial)?,
        );
        let mut key = [0_u8; KEK_LENGTH];
        if decoded.len() != KEK_LENGTH {
            return Err(CoreError::InvalidKeyMaterial);
        }
        key.copy_from_slice(decoded.as_ref());
        let result = Self::new(key_version, key);
        key.zeroize();
        result
    }
}

impl KeyProtector for StaticKeyProtector {
    fn current_key_version(&self) -> Result<u32, CoreError> {
        Ok(self.key_version)
    }

    fn key_encryption_key(&self, key_version: u32) -> Result<KeyEncryptionKey, CoreError> {
        if key_version != self.key_version {
            return Err(CoreError::KeyVersionUnavailable(key_version));
        }
        let mut bytes = [0_u8; KEK_LENGTH];
        bytes.copy_from_slice(self.key.as_ref());
        let kek = KeyEncryptionKey::from_bytes(bytes);
        bytes.zeroize();
        Ok(kek)
    }
}

impl fmt::Debug for StaticKeyProtector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeyProtector")
            .field("key_version", &self.key_version)
            .field("key", &"[REDACTED]")
            .finish()
    }
}

/// A non-copyable 256-bit key-encryption key, zeroized on drop.
///
/// Construct this from the protected-platform-key-store result and pass it by
/// value to [`WrappedSecret::wrap`] or [`WrappedSecret::unwrap`]. It has no
/// serialization, clone, byte-exposure, or non-redacted debug API.
pub struct KeyEncryptionKey(Zeroizing<[u8; KEK_LENGTH]>);

impl KeyEncryptionKey {
    /// Takes ownership of a 256-bit key returned by protected key storage.
    #[must_use]
    pub fn from_bytes(mut bytes: [u8; KEK_LENGTH]) -> Self {
        let key = Self(Zeroizing::new(bytes));
        bytes.zeroize();
        key
    }

    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for KeyEncryptionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KeyEncryptionKey([REDACTED])")
    }
}

/// The exact metadata which a wrapped secret is bound to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecretBinding<'a> {
    /// A stable, bounded operation label, such as `device-identity`.
    pub purpose: &'a str,
    /// A caller-chosen stable identifier for the protected record.
    pub context: &'a [u8],
    /// The explicit version of the KEK hierarchy.
    pub key_version: u32,
}

impl<'a> SecretBinding<'a> {
    /// Validates the binding before it is used in an authenticated operation.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.purpose.is_empty()
            || self.purpose.len() > MAX_PURPOSE_LENGTH
            || !self.purpose.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
            })
        {
            return Err(CoreError::InvalidState(
                "invalid wrapped-secret purpose".to_owned(),
            ));
        }
        if self.context.is_empty() || self.context.len() > MAX_CONTEXT_LENGTH {
            return Err(CoreError::InvalidState(
                "invalid wrapped-secret context".to_owned(),
            ));
        }
        if self.key_version == 0 {
            return Err(CoreError::InvalidState(
                "invalid wrapped-secret key version".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A serializable encrypted secret record. Metadata is authenticated as AAD.
///
/// `serde` rejects fields not in this schema, and [`Self::unwrap`] validates all
/// persisted lengths and versions before allocating plaintext.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrappedSecret {
    schema_version: u8,
    cipher_suite: String,
    purpose: String,
    context: Vec<u8>,
    key_version: u32,
    salt: [u8; SALT_LENGTH],
    nonce: [u8; NONCE_LENGTH],
    ciphertext: Vec<u8>,
}

impl WrappedSecret {
    /// Wraps with the current version supplied by an injected protector.
    pub fn protect(
        protector: &dyn KeyProtector,
        purpose: &str,
        context: &[u8],
        plaintext: Zeroizing<Vec<u8>>,
    ) -> Result<Self, CoreError> {
        let key_version = protector.current_key_version()?;
        let binding = SecretBinding {
            purpose,
            context,
            key_version,
        };
        let kek = protector.key_encryption_key(key_version)?;
        Self::wrap(kek, binding, plaintext)
    }

    /// Opens with the exact non-downgraded version named by this envelope.
    pub fn open(
        &self,
        protector: &dyn KeyProtector,
        purpose: &str,
        context: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, CoreError> {
        self.validate()?;
        let current = protector.current_key_version()?;
        if current < self.key_version {
            return Err(CoreError::KeyVersionUnavailable(self.key_version));
        }
        let binding = SecretBinding {
            purpose,
            context,
            key_version: self.key_version,
        };
        let kek = protector.key_encryption_key(self.key_version)?;
        self.unwrap(kek, binding)
    }

    /// Wraps a secret with a consumed 256-bit KEK and random salt and nonce.
    ///
    /// Both secret inputs are consumed. The non-copyable KEK and plaintext are
    /// zeroized on return, including on authentication or allocation failure.
    pub fn wrap(
        kek: KeyEncryptionKey,
        binding: SecretBinding<'_>,
        plaintext: Zeroizing<Vec<u8>>,
    ) -> Result<Self, CoreError> {
        binding.validate()?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_LENGTH {
            return Err(CoreError::ResourceLimit("wrapped secret plaintext size"));
        }

        let mut salt = [0_u8; SALT_LENGTH];
        let mut nonce = [0_u8; NONCE_LENGTH];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce);
        Self::wrap_with_material(kek, binding, plaintext, salt, nonce)
    }

    /// Authenticates and unwraps a secret for the exact expected binding.
    ///
    /// This consumes and zeroizes the KEK. The returned plaintext is zeroized
    /// when dropped by its owner.
    pub fn unwrap(
        &self,
        kek: KeyEncryptionKey,
        expected_binding: SecretBinding<'_>,
    ) -> Result<Zeroizing<Vec<u8>>, CoreError> {
        expected_binding.validate()?;
        self.validate()?;
        if self.purpose != expected_binding.purpose
            || self.context != expected_binding.context
            || self.key_version != expected_binding.key_version
        {
            return Err(CoreError::AuthenticationFailed);
        }

        let mut owned_kek = kek;
        let mut info = kdf_info(expected_binding, &self.salt, &self.nonce)?;
        let mut key = derive_key(&owned_kek, &self.salt, &info)?;
        let mut aad = aad(
            self.schema_version,
            expected_binding,
            &self.salt,
            &self.nonce,
        )?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let result = cipher
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.ciphertext,
                    aad: aad.as_ref(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| CoreError::AuthenticationFailed);
        drop(cipher);
        key.zeroize();
        owned_kek.zeroize();
        info.zeroize();
        aad.zeroize();
        result
    }

    /// Returns the record's purpose after strict structural validation.
    pub fn purpose(&self) -> Result<&str, CoreError> {
        self.validate()?;
        Ok(&self.purpose)
    }

    /// Returns the record's bound context after strict structural validation.
    pub fn context(&self) -> Result<&[u8], CoreError> {
        self.validate()?;
        Ok(&self.context)
    }

    /// Returns the explicit hierarchy version after strict structural validation.
    pub fn key_version(&self) -> Result<u32, CoreError> {
        self.validate()?;
        Ok(self.key_version)
    }

    /// Checks the complete persisted envelope schema before a decrypt attempt.
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(CoreError::InvalidState(
                "unsupported wrapped-secret schema".to_owned(),
            ));
        }
        if self.cipher_suite != CIPHER_SUITE {
            return Err(CoreError::UnsupportedCipherSuite(self.cipher_suite.clone()));
        }
        SecretBinding {
            purpose: &self.purpose,
            context: &self.context,
            key_version: self.key_version,
        }
        .validate()?;
        if self.ciphertext.len() < AEAD_TAG_LENGTH
            || self.ciphertext.len() > MAX_PLAINTEXT_LENGTH + AEAD_TAG_LENGTH
        {
            return Err(CoreError::InvalidState(
                "invalid wrapped-secret ciphertext length".to_owned(),
            ));
        }
        Ok(())
    }

    fn wrap_with_material(
        kek: KeyEncryptionKey,
        binding: SecretBinding<'_>,
        mut plaintext: Zeroizing<Vec<u8>>,
        salt: [u8; SALT_LENGTH],
        nonce: [u8; NONCE_LENGTH],
    ) -> Result<Self, CoreError> {
        let mut owned_kek = kek;
        let mut info = kdf_info(binding, &salt, &nonce)?;
        let mut key = derive_key(&owned_kek, &salt, &info)?;
        let mut aad = aad(SCHEMA_VERSION, binding, &salt, &nonce)?;
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_ref(),
                    aad: aad.as_ref(),
                },
            )
            .map_err(|_| CoreError::AuthenticationFailed);
        drop(cipher);
        key.zeroize();
        owned_kek.zeroize();
        info.zeroize();
        aad.zeroize();
        plaintext.zeroize();
        drop(plaintext);
        ciphertext.map(|ciphertext| Self {
            schema_version: SCHEMA_VERSION,
            cipher_suite: CIPHER_SUITE.to_owned(),
            purpose: binding.purpose.to_owned(),
            context: binding.context.to_vec(),
            key_version: binding.key_version,
            salt,
            nonce,
            ciphertext,
        })
    }
}

/// Derives a bounded, non-secret AAD context for one state-volume record.
///
/// The canonical root intentionally participates in the binding. Copying an
/// encrypted volume to another location therefore cannot silently make that
/// copy a second usable identity. Owner-loss recovery rewraps secrets for the
/// explicitly selected replacement root.
#[must_use]
pub fn state_secret_context(state_root: &Path, record_id: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(STATE_CONTEXT_DOMAIN);
    let root = state_root.as_os_str().as_encoded_bytes();
    hasher.update(&(root.len() as u64).to_be_bytes());
    hasher.update(root);
    hasher.update(&(record_id.len() as u64).to_be_bytes());
    hasher.update(record_id.as_bytes());
    *hasher.finalize().as_bytes()
}

impl fmt::Debug for WrappedSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WrappedSecret")
            .field("schema_version", &self.schema_version)
            .field("cipher_suite", &self.cipher_suite)
            .field("purpose", &self.purpose)
            .field("context_length", &self.context.len())
            .field("key_version", &self.key_version)
            .field("ciphertext_length", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

fn derive_key(
    kek: &KeyEncryptionKey,
    salt: &[u8; SALT_LENGTH],
    info: &[u8],
) -> Result<Zeroizing<[u8; KEK_LENGTH]>, CoreError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), kek.0.as_ref());
    let mut derived = Zeroizing::new([0_u8; KEK_LENGTH]);
    hkdf.expand(info, derived.as_mut())
        .map_err(|_| CoreError::InvalidKeyMaterial)?;
    Ok(derived)
}

fn kdf_info(
    binding: SecretBinding<'_>,
    salt: &[u8; SALT_LENGTH],
    nonce: &[u8; NONCE_LENGTH],
) -> Result<Zeroizing<Vec<u8>>, CoreError> {
    let mut info = Zeroizing::new(Vec::with_capacity(
        DOMAIN.len()
            + 2
            + binding.purpose.len()
            + 4
            + binding.context.len()
            + SALT_LENGTH
            + NONCE_LENGTH,
    ));
    append_binding(&mut info, binding)?;
    info.extend_from_slice(salt);
    info.extend_from_slice(nonce);
    Ok(info)
}

fn aad(
    schema_version: u8,
    binding: SecretBinding<'_>,
    salt: &[u8; SALT_LENGTH],
    nonce: &[u8; NONCE_LENGTH],
) -> Result<Zeroizing<Vec<u8>>, CoreError> {
    let mut output = Zeroizing::new(Vec::with_capacity(
        DOMAIN.len()
            + 1
            + CIPHER_SUITE.len()
            + 2
            + binding.purpose.len()
            + 4
            + binding.context.len()
            + SALT_LENGTH
            + NONCE_LENGTH,
    ));
    output.extend_from_slice(DOMAIN);
    output.push(schema_version);
    append_length_prefixed(&mut output, CIPHER_SUITE.as_bytes())?;
    append_binding(&mut output, binding)?;
    output.extend_from_slice(salt);
    output.extend_from_slice(nonce);
    Ok(output)
}

fn append_binding(output: &mut Vec<u8>, binding: SecretBinding<'_>) -> Result<(), CoreError> {
    append_length_prefixed(output, binding.purpose.as_bytes())?;
    append_length_prefixed(output, binding.context)?;
    output.extend_from_slice(&binding.key_version.to_be_bytes());
    Ok(())
}

fn append_length_prefixed(output: &mut Vec<u8>, value: &[u8]) -> Result<(), CoreError> {
    let length = u16::try_from(value.len())
        .map_err(|_| CoreError::ResourceLimit("wrapped-secret metadata size"))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protector() -> StaticKeyProtector {
        StaticKeyProtector::new(7, [42_u8; KEK_LENGTH]).unwrap()
    }

    fn binding() -> SecretBinding<'static> {
        SecretBinding {
            purpose: "device-identity",
            context: b"node:4f6b9a",
            key_version: 7,
        }
    }

    fn key() -> KeyEncryptionKey {
        KeyEncryptionKey::from_bytes([42_u8; KEK_LENGTH])
    }

    fn wrapped() -> WrappedSecret {
        WrappedSecret::wrap(
            key(),
            binding(),
            Zeroizing::new(b"secret material".to_vec()),
        )
        .unwrap()
    }

    #[test]
    fn round_trip_binds_all_metadata() {
        let record = wrapped();
        let decrypted = record.unwrap(key(), binding()).unwrap();
        assert_eq!(decrypted.as_slice(), b"secret material");
        assert_eq!(record.purpose().unwrap(), "device-identity");
        assert_eq!(record.context().unwrap(), b"node:4f6b9a");
        assert_eq!(record.key_version().unwrap(), 7);
    }

    #[test]
    fn rejects_wrong_key_and_binding() {
        let record = wrapped();
        assert!(matches!(
            record.unwrap(KeyEncryptionKey::from_bytes([9_u8; KEK_LENGTH]), binding()),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            record.unwrap(
                key(),
                SecretBinding {
                    purpose: "recovery-kit",
                    ..binding()
                }
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            record.unwrap(
                key(),
                SecretBinding {
                    context: b"node:changed",
                    ..binding()
                }
            ),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            record.unwrap(
                key(),
                SecretBinding {
                    key_version: 8,
                    ..binding()
                }
            ),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn rejects_cipher_schema_salt_and_nonce_tamper() {
        let mut record = wrapped();
        record.schema_version = 99;
        assert!(matches!(record.validate(), Err(CoreError::InvalidState(_))));

        let mut record = wrapped();
        record.cipher_suite = "AES-GCM".to_owned();
        assert!(matches!(
            record.validate(),
            Err(CoreError::UnsupportedCipherSuite(_))
        ));

        let mut record = wrapped();
        record.salt[0] ^= 1;
        assert!(matches!(
            record.unwrap(key(), binding()),
            Err(CoreError::AuthenticationFailed)
        ));

        let mut record = wrapped();
        record.nonce[0] ^= 1;
        assert!(matches!(
            record.unwrap(key(), binding()),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn rejects_ciphertext_tamper_truncation_and_oversize() {
        let mut record = wrapped();
        record.ciphertext[0] ^= 1;
        assert!(matches!(
            record.unwrap(key(), binding()),
            Err(CoreError::AuthenticationFailed)
        ));

        let mut record = wrapped();
        record.ciphertext.truncate(AEAD_TAG_LENGTH - 1);
        assert!(matches!(record.validate(), Err(CoreError::InvalidState(_))));

        let mut record = wrapped();
        record
            .ciphertext
            .resize(MAX_PLAINTEXT_LENGTH + AEAD_TAG_LENGTH + 1, 0);
        assert!(matches!(record.validate(), Err(CoreError::InvalidState(_))));
    }

    #[test]
    fn serde_rejects_unknown_fields_and_legacy_fixture_is_never_accepted() {
        let record = wrapped();
        let mut json = serde_json::to_value(&record).unwrap();
        json.as_object_mut()
            .unwrap()
            .insert("unexpected".to_owned(), serde_json::Value::Null);
        assert!(serde_json::from_value::<WrappedSecret>(json).is_err());

        for field in ["salt", "nonce"] {
            let mut malformed = serde_json::to_value(&record).unwrap();
            malformed
                .as_object_mut()
                .unwrap()
                .insert(field.to_owned(), serde_json::json!([0_u8]));
            assert!(serde_json::from_value::<WrappedSecret>(malformed).is_err());
        }

        let legacy = deterministic_legacy_v0_fixture();
        assert!(matches!(legacy.validate(), Err(CoreError::InvalidState(_))));
    }

    #[test]
    fn debug_never_exposes_key_or_plaintext() {
        let kek_debug = format!("{:?}", key());
        assert_eq!(kek_debug, "KeyEncryptionKey([REDACTED])");
        let record = wrapped();
        let record_debug = format!("{record:?}");
        assert!(!record_debug.contains("secret material"));
        assert!(!record_debug.contains("42"));
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("secret material"));
    }

    #[test]
    fn protector_open_rejects_version_downgrade_and_copied_volume_context() {
        let source = Path::new("/state/volume-a");
        let copied = Path::new("/state/volume-b");
        let source_context = state_secret_context(source, "identity.json");
        let copied_context = state_secret_context(copied, "identity.json");
        let record = WrappedSecret::protect(
            &protector(),
            "device-identity",
            &source_context,
            Zeroizing::new(b"secret material".to_vec()),
        )
        .unwrap();
        assert!(matches!(
            record.open(&protector(), "device-identity", &copied_context),
            Err(CoreError::AuthenticationFailed)
        ));

        let downgraded = StaticKeyProtector::new(6, [42_u8; KEK_LENGTH]).unwrap();
        assert!(matches!(
            record.open(&downgraded, "device-identity", &source_context),
            Err(CoreError::KeyVersionUnavailable(7))
        ));
    }

    // Migration code can use this fixed, clearly invalid v0 serialized shape as
    // a deterministic regression fixture without ever creating legacy crypto.
    fn deterministic_legacy_v0_fixture() -> WrappedSecret {
        let mut record = wrapped();
        record.schema_version = 0;
        record
    }
}
