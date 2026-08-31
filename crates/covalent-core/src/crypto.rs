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
    pub fn from_bytes(mut bytes: [u8; KEY_LENGTH]) -> Self {
        let key = Self(Zeroizing::new(bytes));
        bytes.zeroize();
        key
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

    pub(crate) fn expected_chunk_locator(
        &self,
        backup_id: BackupId,
        key_epoch: u64,
        plaintext_digest: &str,
    ) -> Result<String, CoreError> {
        if key_epoch == 0 {
            return Err(CoreError::AuthenticationFailed);
        }
        let digest = blake3::Hash::from_hex(plaintext_digest)
            .map_err(|_| CoreError::CorruptChunk(plaintext_digest.to_owned()))?;
        let locator_key = self.derive(
            backup_id.to_string().as_bytes(),
            b"covalent/chunk-locator/v1",
        )?;
        Ok(chunk_locator(&locator_key, key_epoch, digest.as_bytes())
            .to_hex()
            .to_string())
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

    /// Forges a record whose AEAD associated data is exactly the one an honest
    /// encryptor would use for `claimed`, but whose sealed plaintext is `actual`.
    /// Key, nonce, locator, declared digest and declared length all stay consistent
    /// with `claimed`, so every check before the AEAD passes and the AEAD itself
    /// verifies. Only the independent post-decrypt length and digest recomputation
    /// can tell the two plaintexts apart.
    fn forge_substituted_plaintext(
        key: &BackupKey,
        backup_id: BackupId,
        key_epoch: u64,
        claimed: &[u8],
        actual: &[u8],
    ) -> EncryptedChunk {
        let claimed_length = u32::try_from(claimed.len()).expect("claimed length");
        let digest = blake3::hash(claimed);
        let context = chunk_context(backup_id, key_epoch, digest.as_bytes(), claimed_length);
        let encryption_key = key
            .derive(&context, b"covalent/chunk-encryption/v1")
            .expect("encryption key");
        let nonce_material = key
            .derive(&context, b"covalent/chunk-nonce/v1")
            .expect("nonce material");
        let locator_key = key
            .derive(
                backup_id.to_string().as_bytes(),
                b"covalent/chunk-locator/v1",
            )
            .expect("locator key");
        let mut nonce = [0_u8; 24];
        nonce.copy_from_slice(&nonce_material[..24]);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(encryption_key.as_ref()));
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: actual,
                    aad: &context,
                },
            )
            .expect("forged ciphertext");
        EncryptedChunk {
            key_epoch,
            plaintext_digest: digest.to_hex().to_string(),
            opaque_locator: chunk_locator(&locator_key, key_epoch, digest.as_bytes())
                .to_hex()
                .to_string(),
            plaintext_length: claimed_length,
            nonce,
            ciphertext,
        }
    }

    #[test]
    fn a_record_that_lies_about_its_own_digest_is_refused() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let honest = key
            .encrypt_chunk(backup, 4, b"honest chunk")
            .expect("encrypt");
        let elsewhere = key
            .encrypt_chunk(backup, 4, b"a different chunk")
            .expect("encrypt");
        assert_ne!(honest.plaintext_digest, elsewhere.plaintext_digest);

        // The record is authentic in every respect except the digest it declares
        // about itself. The declared digest is deliberately not part of the AEAD
        // associated data, so nothing else in the pipeline can notice the lie:
        // without the explicit comparison this decrypts cleanly.
        let mut lying = key
            .encrypt_chunk(backup, 4, b"honest chunk")
            .expect("encrypt");
        assert_eq!(lying.opaque_locator, honest.opaque_locator);
        lying.plaintext_digest = elsewhere.plaintext_digest.clone();
        assert!(matches!(
            key.decrypt_chunk(backup, &honest.plaintext_digest, &lying),
            Err(CoreError::CorruptChunk(_))
        ));

        // Asking for the digest the record now claims does not rescue it either.
        assert!(matches!(
            key.decrypt_chunk(backup, &elsewhere.plaintext_digest, &lying),
            Err(CoreError::CorruptChunk(_) | CoreError::AuthenticationFailed)
        ));

        // A syntactically invalid declared digest is refused rather than parsed.
        let mut malformed = key
            .encrypt_chunk(backup, 4, b"honest chunk")
            .expect("encrypt");
        malformed.plaintext_digest = "not a digest".to_owned();
        assert!(matches!(
            key.decrypt_chunk(backup, &honest.plaintext_digest, &malformed),
            Err(CoreError::CorruptChunk(_))
        ));
    }

    #[test]
    fn a_chunk_served_under_a_foreign_locator_is_refused() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let elsewhere = key
            .encrypt_chunk(backup, 9, b"a different chunk")
            .expect("encrypt");

        // A storage provider that files an otherwise authentic record under some
        // other chunk's locator must be refused. The locator is keyed and derived,
        // so it is checked rather than trusted; it is not part of the AEAD
        // associated data, so the binding check is the only thing that can reject
        // this. Every case below leaves the record decryptable on purpose.
        let mut swapped = key
            .encrypt_chunk(backup, 9, b"requested chunk")
            .expect("encrypt");
        assert_ne!(swapped.opaque_locator, elsewhere.opaque_locator);
        swapped.opaque_locator = elsewhere.opaque_locator.clone();
        assert!(
            validate_hex_locator(&swapped.opaque_locator).is_ok(),
            "the swapped locator stays well formed, so only the binding can reject it"
        );
        assert!(matches!(
            key.decrypt_chunk(backup, &swapped.plaintext_digest, &swapped),
            Err(CoreError::CorruptChunk(_))
        ));

        // A locator computed under a different backup identity.
        let mut cross_backup = key
            .encrypt_chunk(backup, 9, b"requested chunk")
            .expect("encrypt");
        cross_backup.opaque_locator = key
            .encrypt_chunk(BackupId::new(), 9, b"requested chunk")
            .expect("encrypt")
            .opaque_locator;
        assert!(matches!(
            key.decrypt_chunk(backup, &cross_backup.plaintext_digest, &cross_backup),
            Err(CoreError::CorruptChunk(_))
        ));

        // A locator computed under a different key epoch.
        let mut cross_epoch = key
            .encrypt_chunk(backup, 9, b"requested chunk")
            .expect("encrypt");
        cross_epoch.opaque_locator = key
            .encrypt_chunk(backup, 10, b"requested chunk")
            .expect("encrypt")
            .opaque_locator;
        assert!(matches!(
            key.decrypt_chunk(backup, &cross_epoch.plaintext_digest, &cross_epoch),
            Err(CoreError::CorruptChunk(_))
        ));

        // The unmodified record still decrypts, so the rejections above are the
        // binding check firing and not some unrelated breakage.
        let intact = key
            .encrypt_chunk(backup, 9, b"requested chunk")
            .expect("encrypt");
        assert_eq!(
            key.decrypt_chunk(backup, &intact.plaintext_digest, &intact)
                .expect("intact record")
                .as_slice(),
            b"requested chunk"
        );
    }

    #[test]
    fn a_substituted_plaintext_that_survives_the_aead_is_still_refused() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let claimed: &[u8] = b"the exact bytes the caller asked for";
        let expected_digest = blake3::hash(claimed).to_hex().to_string();

        // The forge helper reproduces a genuine record when it substitutes nothing,
        // which proves the rejections below come from the substitution alone.
        let honest = forge_substituted_plaintext(&key, backup, 5, claimed, claimed);
        assert_eq!(
            key.decrypt_chunk(backup, &expected_digest, &honest)
                .expect("unsubstituted forge decrypts")
                .as_slice(),
            claimed
        );

        let mut extended = claimed.to_vec();
        extended.push(b'!');
        let mut same_length = claimed.to_vec();
        same_length[0] ^= 0x20;
        assert_eq!(same_length.len(), claimed.len());

        for (label, actual) in [
            ("truncated", &claimed[..claimed.len() - 1]),
            ("emptied", &claimed[..0]),
            ("extended", extended.as_slice()),
            // Same length, different content: the length comparison alone cannot
            // catch this, so the independent digest recomputation must.
            ("same length, different content", same_length.as_slice()),
        ] {
            let wire = forge_substituted_plaintext(&key, backup, 5, claimed, actual);
            assert!(
                matches!(
                    key.decrypt_chunk(backup, &expected_digest, &wire),
                    Err(CoreError::CorruptChunk(_))
                ),
                "a {label} plaintext was returned to the caller as the requested chunk"
            );
        }
    }

    #[test]
    fn a_malformed_locator_is_rejected_as_malformed_before_any_key_derivation() {
        let key = BackupKey::generate();
        let backup = BackupId::new();
        let honest = key.encrypt_chunk(backup, 6, b"chunk").expect("encrypt");

        // Provider-supplied locators are untrusted input. A locator that is not a
        // lowercase 64-character hex string is refused as malformed rather than
        // being carried into the key schedule and reported as chunk corruption.
        for (label, locator) in [
            ("too short", "00".repeat(31)),
            ("too long", "00".repeat(33)),
            ("uppercase", "AB".repeat(32)),
            ("not hex", "zz".repeat(32)),
            ("empty", String::new()),
        ] {
            let mut wire = key.encrypt_chunk(backup, 6, b"chunk").expect("encrypt");
            wire.opaque_locator = locator;
            assert!(
                matches!(
                    key.decrypt_chunk(backup, &honest.plaintext_digest, &wire),
                    Err(CoreError::InvalidLocator)
                ),
                "a {label} locator was not rejected as malformed"
            );
        }
    }

    #[test]
    fn the_reserved_zero_key_epoch_is_refused_on_both_sides() {
        let key = BackupKey::generate();
        let backup = BackupId::new();

        // The honest encryptor never emits epoch zero.
        assert!(matches!(
            key.encrypt_chunk(backup, 0, b"chunk"),
            Err(CoreError::ResourceLimit(_))
        ));
        assert!(matches!(
            key.expected_chunk_locator(backup, 0, blake3::hash(b"chunk").to_hex().as_ref()),
            Err(CoreError::AuthenticationFailed)
        ));

        // So a record that claims epoch zero came from somewhere else. It is
        // internally consistent here - locator, nonce, AEAD and digest all agree -
        // which is exactly why the reserved-epoch gate has to be the thing that
        // rejects it.
        let claimed: &[u8] = b"a chunk minted at the reserved epoch";
        let expected_digest = blake3::hash(claimed).to_hex().to_string();
        let wire = forge_substituted_plaintext(&key, backup, 0, claimed, claimed);
        assert_eq!(wire.key_epoch, 0);
        assert!(matches!(
            key.decrypt_chunk(backup, &expected_digest, &wire),
            Err(CoreError::CorruptChunk(_))
        ));

        // The same forge at a legitimate epoch decrypts, so the rejection above is
        // the reserved-epoch gate and not a broken forge.
        let legitimate = forge_substituted_plaintext(&key, backup, 1, claimed, claimed);
        assert_eq!(
            key.decrypt_chunk(backup, &expected_digest, &legitimate)
                .expect("epoch one decrypts")
                .as_slice(),
            claimed
        );
    }
}
