//! Canonical bounded local manifests for immutable sync file content.
//!
//! A manifest shapes and binds an ordered sequence of plaintext chunks to an
//! already signed FileContent. It neither proves that chunks exist nor verifies
//! their bytes, authorizes a transfer, pins a cache entry, or applies executable
//! metadata. A content store must retain and pin verified bytes before an
//! operation becomes durable; uncertain appends retain that pin until replay
//! proves the operation absent. Accepted-history references are not evicted in
//! version 1.

use std::fmt;

use thiserror::Error;
use zeroize::Zeroizing;

use super::body::{ContentDigest, FileContent};

const MAGIC: &[u8; 8] = b"COVSCM01";
const WIRE_VERSION: u16 = 1;
const KNOWN_FLAGS: u8 = 0;
const HEADER_BYTES: usize = MAGIC.len() + 2 + 1 + 32 + 8 + 4;
const CHUNK_BYTES: usize = 32 + 4;
/// Maximum number of ordered chunk descriptors in one file manifest.
pub const MAX_SYNC_CONTENT_CHUNKS: usize = 262_144;
/// Maximum plaintext length of one described chunk.
pub const MAX_SYNC_CONTENT_CHUNK_BYTES: usize = 4 * 1_024 * 1_024;
/// Maximum whole-file plaintext length described by one manifest.
pub const MAX_SYNC_CONTENT_FILE_BYTES: u64 = 1_u64 << 40;
/// Maximum canonical manifest record bytes accepted before parsing.
pub const MAX_SYNC_CONTENT_MANIFEST_BYTES: usize = 10 * 1_024 * 1_024;

/// A plaintext BLAKE3 digest for exactly one chunk.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ChunkDigest([u8; 32]);

impl ChunkDigest {
    /// Builds a claimed chunk commitment. The cache still verifies its bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed chunk commitment bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for ChunkDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ChunkDigest([redacted])")
    }
}

/// One ordered plaintext chunk commitment.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ChunkDescriptor {
    digest: ChunkDigest,
    byte_length: u32,
}

impl ChunkDescriptor {
    /// Creates a bounded, nonempty chunk descriptor.
    pub fn new(digest: ChunkDigest, byte_length: u32) -> Result<Self, ManifestError> {
        if byte_length == 0 {
            return Err(ManifestError::ZeroChunkLength);
        }
        if byte_length > MAX_SYNC_CONTENT_CHUNK_BYTES as u32 {
            return Err(ManifestError::ChunkTooLarge);
        }
        Ok(Self {
            digest,
            byte_length,
        })
    }

    /// Returns the chunk plaintext commitment.
    #[must_use]
    pub const fn digest(self) -> ChunkDigest {
        self.digest
    }

    /// Returns the exact plaintext length.
    #[must_use]
    pub const fn byte_length(self) -> u32 {
        self.byte_length
    }
}

impl fmt::Debug for ChunkDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChunkDescriptor")
            .field("byte_length", &self.byte_length)
            .finish_non_exhaustive()
    }
}

/// Canonical local metadata for one immutable file's ordered plaintext chunks.
///
/// Executable mode is intentionally absent: it remains signed path-operation
/// metadata, so equal file bytes and length can reuse the same content cache
/// entry across chmod operations.
#[derive(Clone, Eq, PartialEq)]
pub struct ContentManifest {
    whole_digest: ContentDigest,
    byte_length: u64,
    chunks: Vec<ChunkDescriptor>,
}

impl ContentManifest {
    /// Validates a bounded manifest without reading chunk bytes.
    pub fn new(
        whole_digest: ContentDigest,
        byte_length: u64,
        chunks: Vec<ChunkDescriptor>,
    ) -> Result<Self, ManifestError> {
        validate_parts(whole_digest, byte_length, &chunks)?;
        Ok(Self {
            whole_digest,
            byte_length,
            chunks,
        })
    }

    /// Returns the expected whole-file plaintext commitment.
    #[must_use]
    pub const fn whole_digest(&self) -> ContentDigest {
        self.whole_digest
    }

    /// Returns the exact whole-file plaintext length.
    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    /// Returns file-order chunk descriptors; duplicates are significant.
    #[must_use]
    pub fn chunks(&self) -> &[ChunkDescriptor] {
        &self.chunks
    }

    /// Encodes the one canonical big-endian record using redacted owned bytes.
    ///
    /// Wire order: COVSCM01, u16 version, u8 flags, whole digest[32], u64 file
    /// length, u32 chunk count, then ordered digest[32]/u32 length pairs.
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, ManifestError> {
        let encoded_length = encoded_length(self.chunks.len())?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(encoded_length)
            .map_err(|_| ManifestError::AllocationFailed)?;
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
        output.push(KNOWN_FLAGS);
        output.extend_from_slice(&self.whole_digest.to_bytes());
        output.extend_from_slice(&self.byte_length.to_be_bytes());
        output.extend_from_slice(&(self.chunks.len() as u32).to_be_bytes());
        for chunk in &self.chunks {
            output.extend_from_slice(&chunk.digest.to_bytes());
            output.extend_from_slice(&chunk.byte_length.to_be_bytes());
        }
        Ok(Zeroizing::new(output))
    }

    /// Decodes a manifest that must match an already signed file reference.
    ///
    /// expected.executable() is deliberately ignored because executable mode is
    /// operation metadata rather than cache content identity. This is only
    /// shape/binding validation: callers must still verify stored or received
    /// chunk bytes, their order, and the whole-file digest before use.
    pub fn decode(expected: FileContent, bytes: &[u8]) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_SYNC_CONTENT_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let header = bytes.get(..HEADER_BYTES).ok_or(ManifestError::Truncated)?;
        if header[..MAGIC.len()] != *MAGIC {
            return Err(ManifestError::InvalidVersion);
        }
        let version = u16::from_be_bytes([header[8], header[9]]);
        if version != WIRE_VERSION {
            return Err(ManifestError::InvalidVersion);
        }
        if header[10] != KNOWN_FLAGS {
            return Err(ManifestError::InvalidFlags);
        }
        let whole_digest = ContentDigest::from_bytes(
            header[11..43]
                .try_into()
                .map_err(|_| ManifestError::Truncated)?,
        );
        let byte_length = u64::from_be_bytes(
            header[43..51]
                .try_into()
                .map_err(|_| ManifestError::Truncated)?,
        );
        let chunk_count = usize::try_from(u32::from_be_bytes(
            header[51..55]
                .try_into()
                .map_err(|_| ManifestError::Truncated)?,
        ))
        .map_err(|_| ManifestError::TooManyChunks)?;
        if chunk_count > MAX_SYNC_CONTENT_CHUNKS {
            return Err(ManifestError::TooManyChunks);
        }
        let expected_length = encoded_length(chunk_count)?;
        if bytes.len() < expected_length {
            return Err(ManifestError::Truncated);
        }
        if bytes.len() != expected_length {
            return Err(ManifestError::TrailingBytes);
        }
        if whole_digest != expected.digest() || byte_length != expected.byte_length() {
            return Err(ManifestError::BindingMismatch);
        }

        let mut chunks = Vec::new();
        chunks
            .try_reserve_exact(chunk_count)
            .map_err(|_| ManifestError::AllocationFailed)?;
        let mut cursor = HEADER_BYTES;
        for _ in 0..chunk_count {
            let digest = ChunkDigest::from_bytes(
                bytes[cursor..cursor + 32]
                    .try_into()
                    .map_err(|_| ManifestError::Truncated)?,
            );
            let length = u32::from_be_bytes(
                bytes[cursor + 32..cursor + CHUNK_BYTES]
                    .try_into()
                    .map_err(|_| ManifestError::Truncated)?,
            );
            chunks.push(ChunkDescriptor::new(digest, length)?);
            cursor += CHUNK_BYTES;
        }
        Self::new(whole_digest, byte_length, chunks)
    }
}

impl fmt::Debug for ContentManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContentManifest")
            .field("byte_length", &self.byte_length)
            .field("chunk_count", &self.chunks.len())
            .finish_non_exhaustive()
    }
}

fn encoded_length(chunk_count: usize) -> Result<usize, ManifestError> {
    if chunk_count > MAX_SYNC_CONTENT_CHUNKS {
        return Err(ManifestError::TooManyChunks);
    }
    let length = HEADER_BYTES
        .checked_add(
            chunk_count
                .checked_mul(CHUNK_BYTES)
                .ok_or(ManifestError::TooLarge)?,
        )
        .ok_or(ManifestError::TooLarge)?;
    if length > MAX_SYNC_CONTENT_MANIFEST_BYTES {
        return Err(ManifestError::TooLarge);
    }
    Ok(length)
}

fn validate_parts(
    whole_digest: ContentDigest,
    byte_length: u64,
    chunks: &[ChunkDescriptor],
) -> Result<(), ManifestError> {
    if byte_length > MAX_SYNC_CONTENT_FILE_BYTES {
        return Err(ManifestError::FileTooLarge);
    }
    encoded_length(chunks.len())?;
    let empty_digest = ContentDigest::from_bytes(*blake3::hash(b"").as_bytes());
    if byte_length == 0 {
        if !chunks.is_empty() || whole_digest != empty_digest {
            return Err(ManifestError::InvalidEmptyFile);
        }
        return Ok(());
    }
    if chunks.is_empty() {
        return Err(ManifestError::MissingChunks);
    }
    let total = chunks.iter().try_fold(0_u64, |total, chunk| {
        total
            .checked_add(u64::from(chunk.byte_length))
            .ok_or(ManifestError::LengthMismatch)
    })?;
    if total != byte_length {
        return Err(ManifestError::LengthMismatch);
    }
    Ok(())
}

/// Fixed, redacted failures for malformed local content metadata.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ManifestError {
    /// The record cannot fit the configured metadata bound.
    #[error("sync content manifest exceeds its byte limit")]
    TooLarge,
    /// The record ended before its declared canonical fields.
    #[error("sync content manifest is truncated")]
    Truncated,
    /// Extra bytes prevent a unique canonical representation.
    #[error("sync content manifest has trailing bytes")]
    TrailingBytes,
    /// The magic or version is not supported.
    #[error("invalid sync content manifest version")]
    InvalidVersion,
    /// A future flag bit was present.
    #[error("invalid sync content manifest flags")]
    InvalidFlags,
    /// More ordered chunks were declared than the fixed bound allows.
    #[error("sync content manifest has too many chunks")]
    TooManyChunks,
    /// A chunk length was zero.
    #[error("sync content manifest chunk length must be positive")]
    ZeroChunkLength,
    /// A chunk length exceeded the fixed streaming bound.
    #[error("sync content manifest chunk exceeds its byte limit")]
    ChunkTooLarge,
    /// The whole-file length exceeded the fixed file bound.
    #[error("sync content manifest file exceeds its byte limit")]
    FileTooLarge,
    /// The empty-file form is not exactly empty BLAKE3 with zero chunks.
    #[error("invalid empty sync content manifest")]
    InvalidEmptyFile,
    /// A nonempty file omitted ordered chunk descriptors.
    #[error("nonempty sync content manifest has no chunks")]
    MissingChunks,
    /// Ordered chunk lengths do not equal the whole-file commitment length.
    #[error("sync content manifest chunk lengths do not match file length")]
    LengthMismatch,
    /// The local record disagreed with the caller's signed file reference.
    #[error("sync content manifest does not match expected content")]
    BindingMismatch,
    /// Bounded metadata allocation was unavailable.
    #[error("sync content manifest allocation failed")]
    AllocationFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(bytes: &[u8]) -> ChunkDigest {
        ChunkDigest::from_bytes(*blake3::hash(bytes).as_bytes())
    }

    fn file(bytes: &[u8], executable: bool) -> FileContent {
        FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
            bytes.len() as u64,
            executable,
        )
        .expect("file")
    }

    fn descriptor(bytes: &[u8]) -> ChunkDescriptor {
        ChunkDescriptor::new(digest(bytes), bytes.len() as u32).expect("descriptor")
    }

    fn manifest(bytes: &[u8]) -> (FileContent, ContentManifest) {
        let first = &bytes[..3];
        let second = &bytes[3..];
        let expected = file(bytes, true);
        let value = ContentManifest::new(
            expected.digest(),
            expected.byte_length(),
            vec![descriptor(first), descriptor(second)],
        )
        .expect("manifest");
        (expected, value)
    }

    #[test]
    fn real_chunked_content_round_trips_and_ignores_executable_binding() {
        let bytes = b"abcdefghi";
        let (expected, original) = manifest(bytes);
        let encoded = original.encode().expect("encode");
        let different_mode = file(bytes, false);
        let decoded = ContentManifest::decode(different_mode, &encoded).expect("decode");
        assert_eq!(decoded, original);
        assert_eq!(expected.digest(), decoded.whole_digest());
        assert_eq!(expected.byte_length(), decoded.byte_length());
        assert_eq!(encoded.len(), HEADER_BYTES + 2 * CHUNK_BYTES);
    }

    #[test]
    fn repeated_chunks_preserve_file_order() {
        let first = descriptor(b"same");
        let middle = descriptor(b"middle");
        let expected = file(b"samemiddlesame", false);
        let manifest = ContentManifest::new(
            expected.digest(),
            expected.byte_length(),
            vec![first, middle, first],
        )
        .expect("manifest");
        assert_eq!(manifest.chunks(), &[first, middle, first]);
        assert_eq!(
            ContentManifest::decode(expected, &manifest.encode().expect("encode"))
                .expect("decode")
                .chunks(),
            &[first, middle, first]
        );
    }

    #[test]
    fn exact_empty_form_is_required() {
        let expected = file(b"", false);
        let empty = ContentManifest::new(expected.digest(), 0, Vec::new()).expect("empty");
        assert_eq!(
            ContentManifest::decode(expected, &empty.encode().expect("encode")),
            Ok(empty)
        );
        assert_eq!(
            ContentManifest::new(ContentDigest::from_bytes([9; 32]), 0, Vec::new()),
            Err(ManifestError::InvalidEmptyFile)
        );
        assert_eq!(
            ContentManifest::new(expected.digest(), 0, vec![descriptor(b"x")]),
            Err(ManifestError::InvalidEmptyFile)
        );
    }

    #[test]
    fn wrong_binding_and_shape_are_rejected() {
        let bytes = b"abcdefghi";
        let (expected, original) = manifest(bytes);
        let wrong = file(b"abcdefg", false);
        assert_eq!(
            ContentManifest::decode(wrong, &original.encode().expect("encode")),
            Err(ManifestError::BindingMismatch)
        );
        assert_eq!(
            ContentManifest::new(expected.digest(), expected.byte_length(), Vec::new()),
            Err(ManifestError::MissingChunks)
        );
        assert_eq!(
            ChunkDescriptor::new(digest(b"x"), 0),
            Err(ManifestError::ZeroChunkLength)
        );
        assert_eq!(
            ChunkDescriptor::new(digest(b"x"), (MAX_SYNC_CONTENT_CHUNK_BYTES as u32) + 1,),
            Err(ManifestError::ChunkTooLarge)
        );
        assert_eq!(
            ContentManifest::new(expected.digest(), u64::MAX, vec![descriptor(b"x")]),
            Err(ManifestError::FileTooLarge)
        );
        assert_eq!(
            ContentManifest::new(
                expected.digest(),
                expected.byte_length() + 1,
                vec![descriptor(&bytes[..3]), descriptor(&bytes[3..])],
            ),
            Err(ManifestError::LengthMismatch)
        );
    }

    #[test]
    fn decoder_rejects_all_prefixes_and_trailing_or_hostile_metadata() {
        let (expected, original) = manifest(b"abcdefghi");
        let encoded = original.encode().expect("encode");
        for end in 0..encoded.len() {
            assert!(
                ContentManifest::decode(expected, &encoded[..end]).is_err(),
                "accepted prefix {end}"
            );
        }
        let mut trailing = encoded.to_vec();
        trailing.push(0);
        assert_eq!(
            ContentManifest::decode(expected, &trailing),
            Err(ManifestError::TrailingBytes)
        );
        let mut flags = encoded.to_vec();
        flags[10] = 1;
        assert_eq!(
            ContentManifest::decode(expected, &flags),
            Err(ManifestError::InvalidFlags)
        );
        let mut count = encoded.to_vec();
        count[51..55].copy_from_slice(&((MAX_SYNC_CONTENT_CHUNKS as u32) + 1).to_be_bytes());
        assert_eq!(
            ContentManifest::decode(expected, &count),
            Err(ManifestError::TooManyChunks)
        );
        let too_large = vec![0_u8; MAX_SYNC_CONTENT_MANIFEST_BYTES + 1];
        assert_eq!(
            ContentManifest::decode(expected, &too_large),
            Err(ManifestError::TooLarge)
        );
    }

    #[test]
    fn maximum_metadata_is_valid_without_allocating_file_bytes() {
        let chunk = ChunkDescriptor::new(
            ChunkDigest::from_bytes([7; 32]),
            MAX_SYNC_CONTENT_CHUNK_BYTES as u32,
        )
        .expect("maximum chunk");
        let chunks = vec![chunk; MAX_SYNC_CONTENT_CHUNKS];
        let manifest = ContentManifest::new(
            ContentDigest::from_bytes([8; 32]),
            MAX_SYNC_CONTENT_FILE_BYTES,
            chunks,
        )
        .expect("maximum manifest");
        let encoded = manifest.encode().expect("encode");
        assert!(encoded.len() <= MAX_SYNC_CONTENT_MANIFEST_BYTES);
        let expected = FileContent::new(
            ContentDigest::from_bytes([8; 32]),
            MAX_SYNC_CONTENT_FILE_BYTES,
            true,
        )
        .expect("expected");
        assert_eq!(
            ContentManifest::decode(expected, &encoded)
                .expect("decode")
                .chunks()
                .len(),
            MAX_SYNC_CONTENT_CHUNKS
        );
        let mut excessive = encoded.to_vec();
        excessive.push(0);
        assert_eq!(
            ContentManifest::decode(expected, &excessive),
            Err(ManifestError::TrailingBytes)
        );
    }
}
