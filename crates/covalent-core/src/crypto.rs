use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use covalent_protocol::BackupId;
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::{CoreError, digest_bytes};

const CHUNK_RECORD_MAGIC: &[u8; 4] = b"CVCH";
const CHUNK_RECORD_VERSION: u8 = 1;
const CHUNK_HEADER_LENGTH: usize = 4 + 1 + 8 + 4 + 24;
const AEAD_TAG_LENGTH: usize = 16;
const KEY_LENGTH: usize = 32;

/// Per-backup epoch secret. Memory is zeroized on drop and never serialized.
pub struct BackupKey(Zeroizing<[u8; KEY_LENGTH]>);

impl BackupKey {
    /// Generates a key from the operating-system random source.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0_u8; KEY_LENGTH];
        OsRng.fill_bytes(&mut bytes);
        Self(Zeroizing::new(bytes))
    }

    /// Imports an exact 256-bit key from a protected platform key store.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LENGTH]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Returns a temporary copy for transfer into platform key protection.
    #[must_use]
    pub fn to_bytes(&self) -> Zeroizing<[u8; KEY_LENGTH]> {
        Zeroizing::new(*self.0)
    }

    pub(crate) fn derive(
        &self,
        salt: &[u8],
        label: &[u8],
    ) -> Result<Zeroizing<[u8; 32]>, CoreError> {
        let hkdf = Hkdf::<Sha256>::new(Some(salt), self.0.as_ref());
        let mut output = Zeroizing::new([0_u8; 32]);
        hkdf.expand(label, output.as_mut())
            .map_err(|_| CoreError::InvalidKeyMaterial)?;
        Ok(output)
    }

    /// Encrypts one independently authenticated plaintext chunk.
    pub fn encrypt_chunk(
        &self,
        backup_id: BackupId,
        key_epoch: u64,
        plaintext: &[u8],
    ) -> Result<EncryptedChunk, CoreError> {
        if key_epoch == 0 || plaintext.is_empty() || plaintext.len() > u32::MAX as usize {
            return Err(CoreError::ResourceLimit("chunk plaintext size"));
        }
        let digest = blake3::hash(plaintext);
        let digest_hex = digest.to_hex().to_string();
        let context = chunk_context(
            backup_id,
            key_epoch,
            digest.as_bytes(),
            plaintext.len() as u32,
        );
        let key = self.derive(&context, b"covalent/chunk-encryption/v1")?;
        let nonce_material = self.derive(&context, b"covalent/chunk-nonce/v1")?;
        let locator_key = self.derive(
            backup_id.to_string().as_bytes(),
            b"covalent/chunk-locator/v1",
        )?;
        let opaque_locator = chunk_locator(&locator_key, key_epoch, digest.as_bytes())
            .to_hex()
            .to_string();
        let mut nonce = [0_u8; 24];
        nonce.copy_from_slice(&nonce_material[..24]);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &context,
                },
            )
            .map_err(|_| CoreError::AuthenticationFailed)?;
        Ok(EncryptedChunk {
            key_epoch,
            plaintext_digest: digest_hex,
            opaque_locator,
            plaintext_length: plaintext.len() as u32,
            nonce,
            ciphertext,
        })
    }

    /// Authenticates, decrypts, and independently verifies a chunk digest.
    pub fn decrypt_chunk(
        &self,
        backup_id: BackupId,
        expected_digest: &str,
        encrypted: &EncryptedChunk,
    ) -> Result<Zeroizing<Vec<u8>>, CoreError> {
        validate_hex_locator(&encrypted.opaque_locator)?;
        let digest = blake3::Hash::from_hex(expected_digest)
            .map_err(|_| CoreError::CorruptChunk(expected_digest.to_owned()))?;
        if encrypted.plaintext_digest != expected_digest {
            return Err(CoreError::CorruptChunk(expected_digest.to_owned()));
        }
        let context = chunk_context(
            backup_id,
            encrypted.key_epoch,
            digest.as_bytes(),
            encrypted.plaintext_length,
        );
        let key = self.derive(&context, b"covalent/chunk-encryption/v1")?;
        let locator_key = self.derive(
            backup_id.to_string().as_bytes(),
            b"covalent/chunk-locator/v1",
        )?;
        if encrypted.key_epoch == 0 {
            return Err(CoreError::CorruptChunk(expected_digest.to_owned()));
        }
        let expected_locator = chunk_locator(&locator_key, encrypted.key_epoch, digest.as_bytes())
            .to_hex()
            .to_string();
        if encrypted.opaque_locator != expected_locator {
            return Err(CoreError::CorruptChunk(expected_digest.to_owned()));
        }
        let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_ref()));
        let mut plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&encrypted.nonce),
                    Payload {
                        msg: &encrypted.ciphertext,
                        aad: &context,
                    },
                )
                .map_err(|_| CoreError::AuthenticationFailed)?,
        );
        if plaintext.len() != encrypted.plaintext_length as usize
            || digest_bytes(plaintext.as_ref()) != expected_digest
        {
            plaintext.zeroize();
            return Err(CoreError::CorruptChunk(expected_digest.to_owned()));
        }
        Ok(plaintext)
    }
}

impl Clone for BackupKey {
    fn clone(&self) -> Self {
        Self(Zeroizing::new(*self.0))
    }
}

impl fmt::Debug for BackupKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BackupKey([REDACTED])")
    }
}

/// Provider-storable encrypted chunk plus local verification metadata.
#[derive(Clone, Eq, PartialEq)]
pub struct EncryptedChunk {
    /// Content-key epoch used for encryption.
    pub key_epoch: u64,
    /// Expected plaintext digest; excluded from provider record encoding.
    pub plaintext_digest: String,
    /// Keyed provider-visible locator.
    pub opaque_locator: String,
    /// Authenticated plaintext byte count.
    pub plaintext_length: u32,
    nonce: [u8; 24],
    ciphertext: Vec<u8>,
}

impl EncryptedChunk {
    /// Authenticated ciphertext byte count, excluding local record framing.
    #[must_use]
    pub fn ciphertext_length(&self) -> u32 {
        u32::try_from(self.ciphertext.len()).unwrap_or(u32::MAX)
    }

    /// Encodes the provider record without plaintext digest or path information.
    #[must_use]
    pub fn encode_provider_record(&self) -> Vec<u8> {
        let mut record = Vec::with_capacity(CHUNK_HEADER_LENGTH + self.ciphertext.len());
        record.extend_from_slice(CHUNK_RECORD_MAGIC);
        record.push(CHUNK_RECORD_VERSION);
        record.extend_from_slice(&self.key_epoch.to_be_bytes());
        record.extend_from_slice(&self.plaintext_length.to_be_bytes());
        record.extend_from_slice(&self.nonce);
        record.extend_from_slice(&self.ciphertext);
        record
    }

    /// Decodes a bounded provider record and attaches owner-only verification metadata.
    pub fn decode_provider_record(
        opaque_locator: String,
        plaintext_digest: String,
        record: &[u8],
        maximum_chunk_size: usize,
    ) -> Result<Self, CoreError> {
        validate_hex_locator(&opaque_locator)?;
        if record.len() < CHUNK_HEADER_LENGTH + AEAD_TAG_LENGTH
            || record.len() > CHUNK_HEADER_LENGTH + maximum_chunk_size + AEAD_TAG_LENGTH
            || &record[..4] != CHUNK_RECORD_MAGIC
            || record[4] != CHUNK_RECORD_VERSION
        {
            return Err(CoreError::CorruptChunk(opaque_locator));
        }
        let key_epoch = u64::from_be_bytes(
            record[5..13]
                .try_into()
                .map_err(|_| CoreError::CorruptChunk(opaque_locator.clone()))?,
        );
        let plaintext_length = u32::from_be_bytes(
            record[13..17]
                .try_into()
                .map_err(|_| CoreError::CorruptChunk(opaque_locator.clone()))?,
        );
        if key_epoch == 0
            || plaintext_length == 0
            || plaintext_length as usize > maximum_chunk_size
            || record.len() != CHUNK_HEADER_LENGTH + plaintext_length as usize + AEAD_TAG_LENGTH
        {
            return Err(CoreError::CorruptChunk(opaque_locator));
        }
        let mut nonce = [0_u8; 24];
        nonce.copy_from_slice(&record[17..41]);
        Ok(Self {
            key_epoch,
            plaintext_digest,
            opaque_locator,
            plaintext_length,
            nonce,
            ciphertext: record[CHUNK_HEADER_LENGTH..].to_vec(),
        })
    }
}

impl fmt::Debug for EncryptedChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedChunk")
            .field("key_epoch", &self.key_epoch)
            .field("plaintext_digest", &self.plaintext_digest)
            .field("opaque_locator", &self.opaque_locator)
            .field("plaintext_length", &self.plaintext_length)
            .field("ciphertext_length", &self.ciphertext.len())
            .finish_non_exhaustive()
    }
}

fn chunk_context(
    backup_id: BackupId,
    key_epoch: u64,
    digest: &[u8; 32],
    plaintext_length: u32,
) -> Vec<u8> {
    let mut context = Vec::with_capacity(32 + 16 + 8 + 32 + 4);
    context.extend_from_slice(b"covalent/chunk-record/v1\0");
    context.extend_from_slice(backup_id.to_string().as_bytes());
    context.extend_from_slice(&key_epoch.to_be_bytes());
    context.extend_from_slice(digest);
    context.extend_from_slice(&plaintext_length.to_be_bytes());
    context
}

fn chunk_locator(locator_key: &[u8; 32], key_epoch: u64, digest: &[u8; 32]) -> blake3::Hash {
    let mut input = [0_u8; 40];
    input[..8].copy_from_slice(&key_epoch.to_be_bytes());
    input[8..].copy_from_slice(digest);
    blake3::keyed_hash(locator_key, &input)
}

pub(crate) fn validate_hex_locator(locator: &str) -> Result<(), CoreError> {
    if locator.len() != 64
        || !locator
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(CoreError::InvalidLocator);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn chunk_round_trip_and_dedup_locator() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let first = key
            .encrypt_chunk(backup, 1, b"same plaintext")
            .expect("encrypt");
        let second = key
            .encrypt_chunk(backup, 1, b"same plaintext")
            .expect("encrypt");
        assert_eq!(first.opaque_locator, second.opaque_locator);
        assert_eq!(first.nonce, second.nonce);
        assert_eq!(
            first.encode_provider_record(),
            second.encode_provider_record()
        );
        let decoded = EncryptedChunk::decode_provider_record(
            first.opaque_locator.clone(),
            first.plaintext_digest.clone(),
            &first.encode_provider_record(),
            1_048_576,
        )
        .expect("decode");
        assert_eq!(
            key.decrypt_chunk(backup, &first.plaintext_digest, &decoded)
                .expect("decrypt")
                .as_slice(),
            b"same plaintext"
        );
    }

    #[test]
    fn key_epochs_use_distinct_locators_and_preserve_old_snapshots() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let first = key.encrypt_chunk(backup, 1, b"same").expect("first");
        let rotated = key.encrypt_chunk(backup, 2, b"same").expect("rotated");
        assert_ne!(first.opaque_locator, rotated.opaque_locator);
        assert_eq!(
            key.decrypt_chunk(backup, &first.plaintext_digest, &first)
                .expect("old epoch")
                .as_slice(),
            b"same"
        );
        assert_eq!(
            key.decrypt_chunk(backup, &rotated.plaintext_digest, &rotated)
                .expect("new epoch")
                .as_slice(),
            b"same"
        );
    }

    #[test]
    fn corruption_and_wrong_key_fail_closed() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let encrypted = key.encrypt_chunk(backup, 7, b"secret").expect("encrypt");
        let mut record = encrypted.encode_provider_record();
        let last = record.last_mut().expect("ciphertext");
        *last ^= 0x80;
        let corrupt = EncryptedChunk::decode_provider_record(
            encrypted.opaque_locator.clone(),
            encrypted.plaintext_digest.clone(),
            &record,
            1_048_576,
        )
        .expect("framing remains valid");
        assert!(matches!(
            key.decrypt_chunk(backup, &encrypted.plaintext_digest, &corrupt),
            Err(CoreError::AuthenticationFailed)
        ));
        assert!(matches!(
            BackupKey::generate().decrypt_chunk(backup, &encrypted.plaintext_digest, &encrypted),
            Err(CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed)
        ));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn arbitrary_chunks_round_trip_and_any_ciphertext_flip_fails(
            plaintext in prop::collection::vec(any::<u8>(), 1..131_072),
            epoch in 1_u64..u64::MAX,
        ) {
            let key = BackupKey::from_bytes([0x5a; 32]);
            let backup = BackupId::from_uuid(uuid::Uuid::from_u128(7));
            let encrypted = key.encrypt_chunk(backup, epoch, &plaintext)?;
            let mut record = encrypted.encode_provider_record();
            let decoded = EncryptedChunk::decode_provider_record(
                encrypted.opaque_locator.clone(),
                encrypted.plaintext_digest.clone(),
                &record,
                131_072,
            )?;
            let decrypted =
                key.decrypt_chunk(backup, &encrypted.plaintext_digest, &decoded)?;
            prop_assert_eq!(
                decrypted.as_slice(),
                plaintext.as_slice(),
            );

            let last = record.len() - 1;
            record[last] ^= 1;
            let tampered = EncryptedChunk::decode_provider_record(
                encrypted.opaque_locator.clone(),
                encrypted.plaintext_digest.clone(),
                &record,
                131_072,
            )?;
            prop_assert!(key.decrypt_chunk(backup, &encrypted.plaintext_digest, &tampered).is_err());
        }
    }
}
