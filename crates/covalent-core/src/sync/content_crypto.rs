//! Bounded local encryption for immutable synchronization content.
//!
//! These records and keyed locators belong to one local folder installation and
//! key generation. They are not a peer transport format or a backup-provider
//! namespace. Encryption proves neither content retention nor file application;
//! the store must verify the ordered whole-file digest and durably retain all
//! referenced chunks before publishing an operation.

use std::fmt;

use chacha20poly1305::aead::{AeadInPlace as _, KeyInit as _};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::{OsRng, RngCore as _};
use sha2::Sha256;
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use super::body::FileContent;
use super::content_manifest::{
    ChunkDescriptor, ContentManifest, MAX_SYNC_CONTENT_CHUNK_BYTES, MAX_SYNC_CONTENT_MANIFEST_BYTES,
};
use super::ids::FolderId;

const MAGIC: &[u8; 8] = b"COVSCO01";
const VERSION: u16 = 1;
const HEADER_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const PAYLOAD_OFFSET: usize = HEADER_BYTES + NONCE_BYTES;
/// Fixed storage overhead for each encrypted chunk or manifest.
pub const SYNC_CONTENT_RECORD_OVERHEAD: usize = PAYLOAD_OFFSET + TAG_BYTES;
/// Largest encrypted chunk accepted before allocation or decryption.
pub const MAX_ENCRYPTED_SYNC_CHUNK_BYTES: usize =
    MAX_SYNC_CONTENT_CHUNK_BYTES + SYNC_CONTENT_RECORD_OVERHEAD;
/// Largest encrypted manifest accepted before allocation or decryption.
pub const MAX_ENCRYPTED_SYNC_MANIFEST_BYTES: usize =
    MAX_SYNC_CONTENT_MANIFEST_BYTES + SYNC_CONTENT_RECORD_OVERHEAD;

const AAD_DOMAIN: &[u8] = b"covalent/sync-content-object-aad/v1\0";
const LOCATOR_DOMAIN: &[u8] = b"covalent/sync-content-object-locator/v1\0";
const CHUNK_KEY_DOMAIN: &[u8] = b"covalent/sync-content-chunk-key/v1";
const MANIFEST_KEY_DOMAIN: &[u8] = b"covalent/sync-content-manifest-key/v1";
const LOCATOR_KEY_DOMAIN: &[u8] = b"covalent/sync-content-locator-key/v1";
const DOMAIN_BYTES: usize = 48;
const OBJECT_BYTES: usize = 1 + 32 + 8;
const AAD_BYTES: usize = AAD_DOMAIN.len() + DOMAIN_BYTES + OBJECT_BYTES + HEADER_BYTES;

/// Local identities allocated and persisted by the folder-state coordinator.
///
/// A restored owner creates a fresh installation/writer and key generation;
/// importing owner recovery state must not recreate an old sync domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyncContentDomain {
    folder_id: FolderId,
    installation_id: Uuid,
    generation_id: Uuid,
}

impl SyncContentDomain {
    /// Binds content keys and objects to an independently persisted generation.
    #[must_use]
    pub const fn new(folder_id: FolderId, installation_id: Uuid, generation_id: Uuid) -> Self {
        Self {
            folder_id,
            installation_id,
            generation_id,
        }
    }

    /// Returns the folder that owns these local objects.
    #[must_use]
    pub const fn folder_id(self) -> FolderId {
        self.folder_id
    }

    /// Returns the local installation identity.
    #[must_use]
    pub const fn installation_id(self) -> Uuid {
        self.installation_id
    }

    /// Returns the protected-key generation identity.
    #[must_use]
    pub const fn generation_id(self) -> Uuid {
        self.generation_id
    }

    fn encode(self) -> [u8; DOMAIN_BYTES] {
        let mut bytes = [0; DOMAIN_BYTES];
        bytes[..16].copy_from_slice(&self.folder_id.to_bytes());
        bytes[16..32].copy_from_slice(self.installation_id.as_bytes());
        bytes[32..].copy_from_slice(self.generation_id.as_bytes());
        bytes
    }
}

/// A keyed local storage name; it does not disclose a plaintext content digest.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ContentLocator([u8; 32]);

impl ContentLocator {
    /// Returns the opaque locator bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Converts the keyed locator to a bounded private storage component.
    #[cfg(unix)]
    pub fn to_state_key(
        self,
    ) -> Result<super::state_dir::StateKey, super::state_dir::StateDirError> {
        super::state_dir::StateKey::new(blake3::Hash::from(self.0).to_hex().as_str())
    }
}

impl fmt::Debug for ContentLocator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentLocator([redacted])")
    }
}

/// Ciphertext ready for immutable local storage, without a durability claim.
pub struct EncodedContentRecord(Vec<u8>);

impl EncodedContentRecord {
    /// Borrows the exact canonical encrypted record.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for EncodedContentRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedContentRecord")
            .field("length", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// Independent, zeroizing local chunk, manifest, and locator keys.
pub struct SyncContentCrypto {
    domain: SyncContentDomain,
    chunk_key: Zeroizing<[u8; 32]>,
    manifest_key: Zeroizing<[u8; 32]>,
    locator_key: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for SyncContentCrypto {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SyncContentCrypto([redacted])")
    }
}

impl SyncContentCrypto {
    /// Imports a random generation secret already protected by the platform.
    /// The input and derived keys are never serialized by this type.
    pub fn from_bytes(
        domain: SyncContentDomain,
        mut bytes: [u8; 32],
    ) -> Result<Self, ContentCryptoError> {
        let secret = Zeroizing::new(bytes);
        bytes.zeroize();
        let hkdf = Hkdf::<Sha256>::new(Some(&domain.encode()), secret.as_ref());
        let derive = |label: &[u8]| -> Result<Zeroizing<[u8; 32]>, ContentCryptoError> {
            let mut key = Zeroizing::new([0; 32]);
            hkdf.expand(label, key.as_mut())
                .map_err(|_| ContentCryptoError::InvalidKeyMaterial)?;
            Ok(key)
        };
        Ok(Self {
            domain,
            chunk_key: derive(CHUNK_KEY_DOMAIN)?,
            manifest_key: derive(MANIFEST_KEY_DOMAIN)?,
            locator_key: derive(LOCATOR_KEY_DOMAIN)?,
        })
    }

    /// Returns the exact local encryption namespace.
    #[must_use]
    pub const fn domain(&self) -> SyncContentDomain {
        self.domain
    }

    /// Locates equal chunks independently of their file or executable mode.
    #[must_use]
    pub fn chunk_locator(&self, expected: ChunkDescriptor) -> ContentLocator {
        self.locator(ObjectBinding::chunk(expected))
    }

    /// Locates file content independently of signed executable metadata.
    #[must_use]
    pub fn manifest_locator(&self, expected: FileContent) -> ContentLocator {
        self.locator(ObjectBinding::manifest(expected))
    }

    /// Verifies a plaintext chunk's commitment before encrypting it.
    pub fn seal_chunk(
        &self,
        expected: ChunkDescriptor,
        plaintext: &[u8],
    ) -> Result<EncodedContentRecord, ContentCryptoError> {
        verify_chunk(expected, plaintext)?;
        self.seal(
            ObjectBinding::chunk(expected),
            plaintext,
            &mut OsNonceSource,
        )
    }

    /// Authenticates a bounded chunk and independently checks its digest.
    pub fn open_chunk(
        &self,
        expected: ChunkDescriptor,
        record: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ContentCryptoError> {
        let plaintext = self.open(ObjectBinding::chunk(expected), record)?;
        verify_chunk(expected, &plaintext)?;
        Ok(plaintext)
    }

    /// Encrypts canonical metadata; the store still verifies all file bytes.
    pub fn seal_manifest(
        &self,
        manifest: &ContentManifest,
    ) -> Result<EncodedContentRecord, ContentCryptoError> {
        let plaintext = manifest
            .encode()
            .map_err(|_| ContentCryptoError::InvalidManifest)?;
        let expected = FileContent::new(manifest.whole_digest(), manifest.byte_length(), false)
            .map_err(|_| ContentCryptoError::InvalidManifest)?;
        self.seal(
            ObjectBinding::manifest(expected),
            &plaintext,
            &mut OsNonceSource,
        )
    }

    /// Authenticates metadata and binds its digest and length to a file reference.
    pub fn open_manifest(
        &self,
        expected: FileContent,
        record: &[u8],
    ) -> Result<ContentManifest, ContentCryptoError> {
        let plaintext = self.open(ObjectBinding::manifest(expected), record)?;
        ContentManifest::decode(expected, &plaintext)
            .map_err(|_| ContentCryptoError::InvalidManifest)
    }

    fn locator(&self, object: ObjectBinding) -> ContentLocator {
        let mut hash = blake3::Hasher::new_keyed(&self.locator_key);
        hash.update(LOCATOR_DOMAIN);
        hash.update(&self.domain.encode());
        hash.update(&object.encode());
        ContentLocator(*hash.finalize().as_bytes())
    }

    fn key(&self, kind: ObjectKind) -> &[u8; 32] {
        match kind {
            ObjectKind::Chunk => &self.chunk_key,
            ObjectKind::Manifest => &self.manifest_key,
        }
    }

    fn seal(
        &self,
        object: ObjectBinding,
        plaintext: &[u8],
        source: &mut impl NonceSource,
    ) -> Result<EncodedContentRecord, ContentCryptoError> {
        object.validate_plaintext_length(plaintext.len())?;
        let mut header = [0; HEADER_BYTES];
        header[..8].copy_from_slice(MAGIC);
        header[8..10].copy_from_slice(&VERSION.to_be_bytes());
        header[10] = object.kind as u8;
        header[12..].copy_from_slice(&(plaintext.len() as u32).to_be_bytes());
        let mut nonce = [0; NONCE_BYTES];
        source.fill(&mut nonce)?;
        let mut output = Zeroizing::new(Vec::new());
        output
            .try_reserve_exact(plaintext.len() + SYNC_CONTENT_RECORD_OVERHEAD)
            .map_err(|_| ContentCryptoError::AllocationFailed)?;
        output.extend_from_slice(&header);
        output.extend_from_slice(&nonce);
        output.extend_from_slice(plaintext);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(self.key(object.kind)));
        let tag = cipher
            .encrypt_in_place_detached(
                XNonce::from_slice(&nonce),
                &self.aad(object, &header),
                &mut output[PAYLOAD_OFFSET..],
            )
            .map_err(|_| ContentCryptoError::EncryptionFailed)?;
        output.extend_from_slice(&tag);
        Ok(EncodedContentRecord(std::mem::take(&mut *output)))
    }

    fn open(
        &self,
        object: ObjectBinding,
        record: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, ContentCryptoError> {
        if record.len() > object.kind.limit() + SYNC_CONTENT_RECORD_OVERHEAD {
            return Err(ContentCryptoError::InvalidLength);
        }
        let header: &[u8; HEADER_BYTES] = record
            .get(..HEADER_BYTES)
            .ok_or(ContentCryptoError::InvalidLength)?
            .try_into()
            .map_err(|_| ContentCryptoError::InvalidLength)?;
        if header[..8] != *MAGIC || header[8..10] != VERSION.to_be_bytes() {
            return Err(ContentCryptoError::InvalidVersion);
        }
        if header[10] != object.kind as u8 {
            return Err(ContentCryptoError::UnexpectedKind);
        }
        if header[11] != 0 {
            return Err(ContentCryptoError::InvalidFlags);
        }
        let length = u32::from_be_bytes(
            header[12..16]
                .try_into()
                .map_err(|_| ContentCryptoError::InvalidLength)?,
        ) as usize;
        object.validate_plaintext_length(length)?;
        if record.len() != length + SYNC_CONTENT_RECORD_OVERHEAD {
            return Err(ContentCryptoError::InvalidLength);
        }
        let payload_end = PAYLOAD_OFFSET + length;
        let mut plaintext = Zeroizing::new(Vec::new());
        plaintext
            .try_reserve_exact(length)
            .map_err(|_| ContentCryptoError::AllocationFailed)?;
        plaintext.extend_from_slice(&record[PAYLOAD_OFFSET..payload_end]);
        let cipher = XChaCha20Poly1305::new(Key::from_slice(self.key(object.kind)));
        cipher
            .decrypt_in_place_detached(
                XNonce::from_slice(&record[HEADER_BYTES..PAYLOAD_OFFSET]),
                &self.aad(object, header),
                &mut plaintext,
                Tag::from_slice(&record[payload_end..]),
            )
            .map_err(|_| ContentCryptoError::AuthenticationFailed)?;
        Ok(plaintext)
    }

    fn aad(&self, object: ObjectBinding, header: &[u8; HEADER_BYTES]) -> [u8; AAD_BYTES] {
        let mut aad = [0; AAD_BYTES];
        let domain_end = AAD_DOMAIN.len() + DOMAIN_BYTES;
        let object_end = domain_end + OBJECT_BYTES;
        aad[..AAD_DOMAIN.len()].copy_from_slice(AAD_DOMAIN);
        aad[AAD_DOMAIN.len()..domain_end].copy_from_slice(&self.domain.encode());
        aad[domain_end..object_end].copy_from_slice(&object.encode());
        aad[object_end..].copy_from_slice(header);
        aad
    }
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum ObjectKind {
    Chunk = 1,
    Manifest = 2,
}

impl ObjectKind {
    const fn limit(self) -> usize {
        match self {
            Self::Chunk => MAX_SYNC_CONTENT_CHUNK_BYTES,
            Self::Manifest => MAX_SYNC_CONTENT_MANIFEST_BYTES,
        }
    }
}

#[derive(Clone, Copy)]
struct ObjectBinding {
    kind: ObjectKind,
    digest: [u8; 32],
    byte_length: u64,
}

impl ObjectBinding {
    fn chunk(expected: ChunkDescriptor) -> Self {
        Self {
            kind: ObjectKind::Chunk,
            digest: expected.digest().to_bytes(),
            byte_length: u64::from(expected.byte_length()),
        }
    }
    fn manifest(expected: FileContent) -> Self {
        Self {
            kind: ObjectKind::Manifest,
            digest: expected.digest().to_bytes(),
            byte_length: expected.byte_length(),
        }
    }
    fn encode(self) -> [u8; OBJECT_BYTES] {
        let mut bytes = [0; OBJECT_BYTES];
        bytes[0] = self.kind as u8;
        bytes[1..33].copy_from_slice(&self.digest);
        bytes[33..].copy_from_slice(&self.byte_length.to_be_bytes());
        bytes
    }
    fn validate_plaintext_length(self, length: usize) -> Result<(), ContentCryptoError> {
        if length == 0
            || length > self.kind.limit()
            || (matches!(self.kind, ObjectKind::Chunk) && length as u64 != self.byte_length)
        {
            return Err(ContentCryptoError::InvalidLength);
        }
        Ok(())
    }
}

fn verify_chunk(expected: ChunkDescriptor, plaintext: &[u8]) -> Result<(), ContentCryptoError> {
    if plaintext.len() != expected.byte_length() as usize {
        return Err(ContentCryptoError::InvalidLength);
    }
    if blake3::hash(plaintext).as_bytes() != &expected.digest().to_bytes() {
        return Err(ContentCryptoError::ContentMismatch);
    }
    Ok(())
}

trait NonceSource {
    fn fill(&mut self, nonce: &mut [u8; NONCE_BYTES]) -> Result<(), ContentCryptoError>;
}
struct OsNonceSource;
impl NonceSource for OsNonceSource {
    fn fill(&mut self, nonce: &mut [u8; NONCE_BYTES]) -> Result<(), ContentCryptoError> {
        OsRng
            .try_fill_bytes(nonce)
            .map_err(|_| ContentCryptoError::EntropyUnavailable)
    }
}

/// Fixed failures that never include content, plaintext digests, or keys.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ContentCryptoError {
    /// An imported key could not derive a fixed-size subkey.
    #[error("invalid sync content key material")]
    InvalidKeyMaterial,
    /// Record lengths do not match the expected bounded object.
    #[error("invalid sync content record length")]
    InvalidLength,
    /// The record format is unknown.
    #[error("invalid sync content record version")]
    InvalidVersion,
    /// Unknown flags cannot be interpreted safely.
    #[error("invalid sync content record flags")]
    InvalidFlags,
    /// Chunk and manifest records cannot substitute for each other.
    #[error("unexpected sync content record kind")]
    UnexpectedKind,
    /// Bounded allocation was unavailable.
    #[error("sync content allocation failed")]
    AllocationFailed,
    /// A fresh encryption nonce could not be generated.
    #[error("sync content entropy is unavailable")]
    EntropyUnavailable,
    /// Encryption did not complete.
    #[error("sync content encryption failed")]
    EncryptionFailed,
    /// A key, context, or ciphertext was incorrect.
    #[error("sync content authentication failed")]
    AuthenticationFailed,
    /// Plaintext did not match the expected chunk digest.
    #[error("sync content commitment does not match")]
    ContentMismatch,
    /// Manifest shape or file binding was invalid.
    #[error("invalid sync content manifest")]
    InvalidManifest,
}

#[cfg(test)]
#[path = "content_crypto_tests.rs"]
mod tests;
