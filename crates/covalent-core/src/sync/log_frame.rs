//! Bounded authenticated framing for future private synchronization logs.
//!
//! This codec establishes local confidentiality and detects malformed or
//! incomplete physical frames. It does not parse event plaintext, authorize a
//! member, admit causal history, acknowledge a peer, or repair a log.

use std::fmt;
use std::num::NonZeroU64;

use chacha20poly1305::aead::{AeadInPlace as _, KeyInit as _};
use chacha20poly1305::{Key, Tag, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore as _};
use thiserror::Error;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use super::ids::FolderId;
use super::membership::MAX_SIGNED_EPOCH_BYTES;

/// Magic at the start of each sync log file, outside individual frames.
pub const SYNC_LOG_FILE_MAGIC: &[u8; 8] = b"CVSLOG01";
/// Maximum plaintext accepted by the common folder-event and apply codec.
///
/// The extra byte permits a future event discriminant around the largest
/// canonical membership record. Callers must enforce each event type's tighter
/// bound, including the 24 KiB signed-operation bound.
pub const MAX_LOG_PLAINTEXT_BYTES: usize = MAX_SIGNED_EPOCH_BYTES + 1;

const FRAME_MAGIC: &[u8; 4] = b"CVSF";
const COMMIT_MAGIC: &[u8; 4] = b"CVSC";
const FRAME_VERSION: u16 = 1;
const HEADER_BYTES: usize = 4 + 2 + 1 + 8 + 4 + 4;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const FOOTER_BYTES: usize = 4 + 8 + 32;
const CIPHERTEXT_OFFSET: usize = HEADER_BYTES + NONCE_BYTES;
const MAX_CIPHERTEXT_BYTES: usize = MAX_LOG_PLAINTEXT_BYTES + TAG_BYTES;
/// Maximum encoded size of one frame, excluding the file magic.
pub const MAX_ENCODED_LOG_FRAME_BYTES: usize =
    HEADER_BYTES + NONCE_BYTES + MAX_CIPHERTEXT_BYTES + FOOTER_BYTES;

const DIGEST_DOMAIN: &[u8] = b"covalent/sync-log-frame/v1\0";
const AAD_DOMAIN: &[u8] = b"covalent/sync-log-aad/v1\0";
const AAD_BYTES: usize = AAD_DOMAIN.len() + SYNC_LOG_FILE_MAGIC.len() + 16 + 16 + 16 + HEADER_BYTES;

/// The private log receiving one encrypted frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum LogFileKind {
    /// Membership evidence and folder operations in one serialized event log.
    FolderEvents = 1,
    /// Transactional local apply receipts.
    Apply = 2,
}

/// Immutable context cryptographically bound to every encrypted frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LogBinding {
    folder_id: FolderId,
    installation_id: Uuid,
    generation_id: Uuid,
    file_kind: LogFileKind,
}

impl LogBinding {
    /// Constructs a binding from identities allocated by the higher layer.
    #[must_use]
    pub const fn new(
        folder_id: FolderId,
        installation_id: Uuid,
        generation_id: Uuid,
        file_kind: LogFileKind,
    ) -> Self {
        Self {
            folder_id,
            installation_id,
            generation_id,
            file_kind,
        }
    }

    /// Returns the bound folder.
    #[must_use]
    pub const fn folder_id(self) -> FolderId {
        self.folder_id
    }

    /// Returns the bound local installation UUID.
    #[must_use]
    pub const fn installation_id(self) -> Uuid {
        self.installation_id
    }

    /// Returns the bound log-generation UUID.
    #[must_use]
    pub const fn generation_id(self) -> Uuid {
        self.generation_id
    }

    /// Returns the bound file kind.
    #[must_use]
    pub const fn file_kind(self) -> LogFileKind {
        self.file_kind
    }
}

/// A positive, monotonically assigned frame position.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LogOrdinal(NonZeroU64);

impl LogOrdinal {
    /// Validates a positive ordinal.
    pub fn new(value: u64) -> Result<Self, LogFrameError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(LogFrameError::ZeroOrdinal)
    }

    /// Returns the numeric ordinal.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A local 256-bit frame-encryption key that zeroizes on drop.
pub struct LogFrameKey(Zeroizing<[u8; 32]>);

impl LogFrameKey {
    /// Imports a key obtained from protected local state.
    #[must_use]
    pub fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let key = Self(Zeroizing::new(bytes));
        bytes.zeroize();
        key
    }
}

impl fmt::Debug for LogFrameKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LogFrameKey([REDACTED])")
    }
}

/// One encrypted frame ready for a bounded append.
pub struct EncodedLogFrame(Vec<u8>);

impl EncodedLogFrame {
    /// Borrows the complete encoded frame.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for EncodedLogFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedLogFrame")
            .field("length", &self.0.len())
            .finish_non_exhaustive()
    }
}

/// One authenticated plaintext frame.
pub struct DecodedLogFrame {
    consumed: usize,
    plaintext: Zeroizing<Vec<u8>>,
}

impl DecodedLogFrame {
    /// Returns the encoded byte count consumed from the input prefix.
    #[must_use]
    pub const fn consumed(&self) -> usize {
        self.consumed
    }

    /// Borrows authenticated bytes for immediate higher-layer parsing.
    #[must_use]
    pub fn plaintext(&self) -> &[u8] {
        self.plaintext.as_slice()
    }
}

impl fmt::Debug for DecodedLogFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DecodedLogFrame")
            .field("consumed", &self.consumed)
            .field("plaintext_length", &self.plaintext.len())
            .finish_non_exhaustive()
    }
}

/// Result of parsing exactly one frame from an input prefix.
pub enum ParseOutcome {
    /// One committed frame was authenticated and decrypted.
    Complete(DecodedLogFrame),
    /// The bytes seen so far form a valid physical prefix of a frame.
    Incomplete {
        /// Minimum additional bytes needed to reach the next validation point.
        minimum_additional: usize,
    },
}

impl fmt::Debug for ParseOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Complete(frame) => formatter.debug_tuple("Complete").field(frame).finish(),
            Self::Incomplete { minimum_additional } => formatter
                .debug_struct("Incomplete")
                .field("minimum_additional", minimum_additional)
                .finish(),
        }
    }
}

/// A fixed, non-sensitive frame construction or validation failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LogFrameError {
    /// Ordinal zero is outside the log sequence.
    #[error("sync log frame ordinal must be positive")]
    ZeroOrdinal,
    /// Plaintext exceeds the common frame limit.
    #[error("sync log frame plaintext exceeds its size limit")]
    PlaintextTooLarge,
    /// An encoded length or offset was not representable.
    #[error("sync log frame length is invalid")]
    InvalidLength,
    /// A bounded result allocation failed.
    #[error("sync log frame allocation failed")]
    AllocationFailed,
    /// The operating-system entropy source failed.
    #[error("sync log frame entropy is unavailable")]
    EntropyUnavailable,
    /// The authenticated cipher rejected an operation.
    #[error("sync log frame encryption failed")]
    EncryptionFailed,
    /// The frame header magic is invalid.
    #[error("sync log frame magic is invalid")]
    InvalidFrameMagic,
    /// The frame uses an unsupported version.
    #[error("sync log frame version is unsupported")]
    UnsupportedVersion,
    /// The frame belongs to another private log kind.
    #[error("sync log frame kind does not match")]
    UnexpectedFileKind,
    /// The frame is not the expected next ordinal.
    #[error("sync log frame ordinal does not match")]
    UnexpectedOrdinal,
    /// The duplicated ciphertext-length fields disagree.
    #[error("sync log frame length complement is invalid")]
    InvalidLengthComplement,
    /// The commit footer magic is invalid.
    #[error("sync log frame commit magic is invalid")]
    InvalidCommitMagic,
    /// The commit footer repeats a different ordinal.
    #[error("sync log frame commit ordinal does not match")]
    InvalidCommitOrdinal,
    /// The complete frame digest is invalid.
    #[error("sync log frame commit digest is invalid")]
    InvalidCommitDigest,
    /// The key or binding did not authenticate the ciphertext.
    #[error("sync log frame authentication failed")]
    AuthenticationFailed,
}

/// Encrypts and commits one bounded plaintext frame in memory.
pub fn encode_frame(
    binding: &LogBinding,
    ordinal: LogOrdinal,
    key: &LogFrameKey,
    plaintext: &[u8],
) -> Result<EncodedLogFrame, LogFrameError> {
    encode_with_nonce_source(binding, ordinal, key, plaintext, &mut OsNonceSource)
}

/// Parses one frame without accepting any trailing bytes as part of it.
///
/// `Incomplete` means only that the supplied physical bytes are a valid prefix.
/// A durable-log caller decides whether an actual EOF is repairable while
/// holding its lock. Complete malformed or unauthenticated frames are errors.
pub fn parse_one(
    input: &[u8],
    expected_binding: &LogBinding,
    expected_ordinal: LogOrdinal,
    key: &LogFrameKey,
) -> Result<ParseOutcome, LogFrameError> {
    if let Some(outcome) = expect_prefix(input, 0, FRAME_MAGIC, LogFrameError::InvalidFrameMagic)? {
        return Ok(outcome);
    }
    if let Some(outcome) = expect_prefix(
        input,
        FRAME_MAGIC.len(),
        &FRAME_VERSION.to_be_bytes(),
        LogFrameError::UnsupportedVersion,
    )? {
        return Ok(outcome);
    }
    if let Some(outcome) = expect_prefix(
        input,
        FRAME_MAGIC.len() + 2,
        &[expected_binding.file_kind as u8],
        LogFrameError::UnexpectedFileKind,
    )? {
        return Ok(outcome);
    }
    if let Some(outcome) = expect_prefix(
        input,
        FRAME_MAGIC.len() + 2 + 1,
        &expected_ordinal.get().to_be_bytes(),
        LogFrameError::UnexpectedOrdinal,
    )? {
        return Ok(outcome);
    }

    let length_offset = FRAME_MAGIC.len() + 2 + 1 + 8;
    let length_end = checked_add(length_offset, 4)?;
    if input.len() < length_end {
        return Ok(incomplete(length_end - input.len()));
    }
    let ciphertext_length = u32::from_be_bytes(
        input[length_offset..length_end]
            .try_into()
            .map_err(|_| LogFrameError::InvalidLength)?,
    );
    let ciphertext_length =
        usize::try_from(ciphertext_length).map_err(|_| LogFrameError::InvalidLength)?;
    if !(TAG_BYTES..=MAX_CIPHERTEXT_BYTES).contains(&ciphertext_length) {
        return Err(LogFrameError::InvalidLength);
    }

    let complement =
        !(u32::try_from(ciphertext_length).map_err(|_| LogFrameError::InvalidLength)?);
    if let Some(outcome) = expect_prefix(
        input,
        length_end,
        &complement.to_be_bytes(),
        LogFrameError::InvalidLengthComplement,
    )? {
        return Ok(outcome);
    }

    let nonce_end = checked_add(HEADER_BYTES, NONCE_BYTES)?;
    if input.len() < nonce_end {
        return Ok(incomplete(nonce_end - input.len()));
    }
    let ciphertext_end = checked_add(CIPHERTEXT_OFFSET, ciphertext_length)?;
    if input.len() < ciphertext_end {
        return Ok(incomplete(ciphertext_end - input.len()));
    }

    if let Some(outcome) = expect_prefix(
        input,
        ciphertext_end,
        COMMIT_MAGIC,
        LogFrameError::InvalidCommitMagic,
    )? {
        return Ok(outcome);
    }
    let footer_ordinal_offset = checked_add(ciphertext_end, COMMIT_MAGIC.len())?;
    if let Some(outcome) = expect_prefix(
        input,
        footer_ordinal_offset,
        &expected_ordinal.get().to_be_bytes(),
        LogFrameError::InvalidCommitOrdinal,
    )? {
        return Ok(outcome);
    }

    let digest_offset = checked_add(footer_ordinal_offset, 8)?;
    let expected_digest = frame_digest(
        &input[..HEADER_BYTES],
        &input[HEADER_BYTES..CIPHERTEXT_OFFSET],
        &input[CIPHERTEXT_OFFSET..ciphertext_end],
        &[],
    );
    if let Some(outcome) = expect_prefix(
        input,
        digest_offset,
        expected_digest.as_bytes(),
        LogFrameError::InvalidCommitDigest,
    )? {
        return Ok(outcome);
    }
    let consumed = checked_add(digest_offset, expected_digest.as_bytes().len())?;

    let body_length = ciphertext_length
        .checked_sub(TAG_BYTES)
        .ok_or(LogFrameError::InvalidLength)?;
    let tag_offset = checked_add(CIPHERTEXT_OFFSET, body_length)?;
    let mut plaintext = Zeroizing::new(Vec::new());
    plaintext
        .try_reserve_exact(body_length)
        .map_err(|_| LogFrameError::AllocationFailed)?;
    plaintext.extend_from_slice(&input[CIPHERTEXT_OFFSET..tag_offset]);
    let nonce = XNonce::from_slice(&input[HEADER_BYTES..CIPHERTEXT_OFFSET]);
    let tag = Tag::from_slice(&input[tag_offset..ciphertext_end]);
    let aad = associated_data(expected_binding, &input[..HEADER_BYTES]);
    XChaCha20Poly1305::new(Key::from_slice(key.0.as_ref()))
        .decrypt_in_place_detached(nonce, &aad, plaintext.as_mut_slice(), tag)
        .map_err(|_| LogFrameError::AuthenticationFailed)?;

    Ok(ParseOutcome::Complete(DecodedLogFrame {
        consumed,
        plaintext,
    }))
}

trait NonceSource {
    fn fill_nonce(&mut self, nonce: &mut [u8; NONCE_BYTES]) -> Result<(), LogFrameError>;
}

struct OsNonceSource;

impl NonceSource for OsNonceSource {
    fn fill_nonce(&mut self, nonce: &mut [u8; NONCE_BYTES]) -> Result<(), LogFrameError> {
        OsRng
            .try_fill_bytes(nonce)
            .map_err(|_| LogFrameError::EntropyUnavailable)
    }
}

fn encode_with_nonce_source(
    binding: &LogBinding,
    ordinal: LogOrdinal,
    key: &LogFrameKey,
    plaintext: &[u8],
    nonce_source: &mut impl NonceSource,
) -> Result<EncodedLogFrame, LogFrameError> {
    if plaintext.len() > MAX_LOG_PLAINTEXT_BYTES {
        return Err(LogFrameError::PlaintextTooLarge);
    }
    let ciphertext_length = plaintext
        .len()
        .checked_add(TAG_BYTES)
        .ok_or(LogFrameError::InvalidLength)?;
    let ciphertext_u32 =
        u32::try_from(ciphertext_length).map_err(|_| LogFrameError::InvalidLength)?;
    let header = frame_header(binding.file_kind, ordinal, ciphertext_u32);

    let mut nonce = [0_u8; NONCE_BYTES];
    nonce_source.fill_nonce(&mut nonce)?;
    let aad = associated_data(binding, &header);

    let mut ciphertext = Zeroizing::new(Vec::new());
    ciphertext
        .try_reserve_exact(plaintext.len())
        .map_err(|_| LogFrameError::AllocationFailed)?;
    ciphertext.extend_from_slice(plaintext);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key.0.as_ref()));
    let tag = cipher
        .encrypt_in_place_detached(XNonce::from_slice(&nonce), &aad, ciphertext.as_mut_slice())
        .map_err(|_| LogFrameError::EncryptionFailed)?;

    let digest = frame_digest(&header, &nonce, ciphertext.as_slice(), tag.as_slice());
    let total_length = checked_add(CIPHERTEXT_OFFSET, ciphertext_length)
        .and_then(|length| checked_add(length, FOOTER_BYTES))?;
    let mut frame = Vec::new();
    frame
        .try_reserve_exact(total_length)
        .map_err(|_| LogFrameError::AllocationFailed)?;
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(ciphertext.as_slice());
    frame.extend_from_slice(tag.as_slice());
    frame.extend_from_slice(COMMIT_MAGIC);
    frame.extend_from_slice(&ordinal.get().to_be_bytes());
    frame.extend_from_slice(digest.as_bytes());
    debug_assert_eq!(frame.len(), total_length);
    Ok(EncodedLogFrame(frame))
}

fn frame_header(
    kind: LogFileKind,
    ordinal: LogOrdinal,
    ciphertext_length: u32,
) -> [u8; HEADER_BYTES] {
    let mut header = [0_u8; HEADER_BYTES];
    let mut cursor = 0;
    append_fixed(&mut header, &mut cursor, FRAME_MAGIC);
    append_fixed(&mut header, &mut cursor, &FRAME_VERSION.to_be_bytes());
    append_fixed(&mut header, &mut cursor, &[kind as u8]);
    append_fixed(&mut header, &mut cursor, &ordinal.get().to_be_bytes());
    append_fixed(&mut header, &mut cursor, &ciphertext_length.to_be_bytes());
    append_fixed(
        &mut header,
        &mut cursor,
        &(!ciphertext_length).to_be_bytes(),
    );
    debug_assert_eq!(cursor, HEADER_BYTES);
    header
}

fn associated_data(binding: &LogBinding, header: &[u8]) -> [u8; AAD_BYTES] {
    let mut aad = [0_u8; AAD_BYTES];
    let mut cursor = 0;
    append_fixed(&mut aad, &mut cursor, AAD_DOMAIN);
    append_fixed(&mut aad, &mut cursor, SYNC_LOG_FILE_MAGIC);
    append_fixed(&mut aad, &mut cursor, &binding.folder_id.to_bytes());
    append_fixed(&mut aad, &mut cursor, binding.installation_id.as_bytes());
    append_fixed(&mut aad, &mut cursor, binding.generation_id.as_bytes());
    append_fixed(&mut aad, &mut cursor, header);
    debug_assert_eq!(cursor, AAD_BYTES);
    aad
}

fn append_fixed<const N: usize>(output: &mut [u8; N], cursor: &mut usize, value: &[u8]) {
    let end = *cursor + value.len();
    output[*cursor..end].copy_from_slice(value);
    *cursor = end;
}

fn frame_digest(header: &[u8], nonce: &[u8], ciphertext: &[u8], tag: &[u8]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(DIGEST_DOMAIN);
    hasher.update(header);
    hasher.update(nonce);
    hasher.update(ciphertext);
    hasher.update(tag);
    hasher.finalize()
}

fn checked_add(left: usize, right: usize) -> Result<usize, LogFrameError> {
    left.checked_add(right).ok_or(LogFrameError::InvalidLength)
}

fn incomplete(minimum_additional: usize) -> ParseOutcome {
    debug_assert!(minimum_additional > 0);
    ParseOutcome::Incomplete { minimum_additional }
}

fn expect_prefix(
    input: &[u8],
    offset: usize,
    expected: &[u8],
    error: LogFrameError,
) -> Result<Option<ParseOutcome>, LogFrameError> {
    let available = input.len().saturating_sub(offset).min(expected.len());
    let end = checked_add(offset, available)?;
    if input.get(offset..end) != Some(&expected[..available]) {
        return Err(error);
    }
    if available < expected.len() {
        return Ok(Some(incomplete(expected.len() - available)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(kind: LogFileKind) -> LogBinding {
        LogBinding::new(
            FolderId::from_uuid(Uuid::from_u128(1)),
            Uuid::from_u128(2),
            Uuid::from_u128(3),
            kind,
        )
    }

    fn key(byte: u8) -> LogFrameKey {
        LogFrameKey::from_bytes([byte; 32])
    }

    fn ordinal(value: u64) -> LogOrdinal {
        LogOrdinal::new(value).expect("positive ordinal")
    }

    fn encoded(plaintext: &[u8], ordinal: u64) -> EncodedLogFrame {
        encode_frame(
            &binding(LogFileKind::FolderEvents),
            self::ordinal(ordinal),
            &key(7),
            plaintext,
        )
        .expect("encode frame")
    }

    fn assert_hard_error(bytes: &[u8], expected_ordinal: u64) {
        assert!(
            parse_one(
                bytes,
                &binding(LogFileKind::FolderEvents),
                ordinal(expected_ordinal),
                &key(7)
            )
            .is_err()
        );
    }

    #[test]
    fn file_magic_and_positive_ordinal_are_fixed() {
        assert_eq!(SYNC_LOG_FILE_MAGIC, b"CVSLOG01");
        assert_eq!(LogOrdinal::new(0), Err(LogFrameError::ZeroOrdinal));
        assert_eq!(ordinal(1).get(), 1);
    }

    #[test]
    fn exact_maximum_round_trips_and_oversize_is_rejected() {
        let plaintext = vec![0xa5; MAX_LOG_PLAINTEXT_BYTES];
        let frame = encoded(&plaintext, 1);
        assert_eq!(frame.as_bytes().len(), MAX_ENCODED_LOG_FRAME_BYTES);
        let ParseOutcome::Complete(decoded) = parse_one(
            frame.as_bytes(),
            &binding(LogFileKind::FolderEvents),
            ordinal(1),
            &key(7),
        )
        .expect("parse frame") else {
            panic!("complete frame must parse");
        };
        assert_eq!(decoded.consumed(), frame.as_bytes().len());
        assert_eq!(decoded.plaintext(), plaintext);

        assert!(matches!(
            encode_frame(
                &binding(LogFileKind::FolderEvents),
                ordinal(1),
                &key(7),
                &vec![0; MAX_LOG_PLAINTEXT_BYTES + 1],
            ),
            Err(LogFrameError::PlaintextTooLarge)
        ));
    }

    #[test]
    fn oversized_declared_ciphertext_is_rejected_from_header_alone() {
        let oversized = u32::try_from(MAX_CIPHERTEXT_BYTES + 1).expect("bounded fixture");
        let header = frame_header(LogFileKind::FolderEvents, ordinal(1), oversized);
        assert!(matches!(
            parse_one(
                &header,
                &binding(LogFileKind::FolderEvents),
                ordinal(1),
                &key(7)
            ),
            Err(LogFrameError::InvalidLength)
        ));
    }

    #[test]
    fn every_truncation_across_two_frames_is_incomplete_only_at_the_tail() {
        let first = encoded(b"first private event", 1);
        let second = encoded(b"second private event", 2);
        let mut both = first.as_bytes().to_vec();
        both.extend_from_slice(second.as_bytes());

        for cut in 0..both.len() {
            let prefix = &both[..cut];
            match parse_one(
                prefix,
                &binding(LogFileKind::FolderEvents),
                ordinal(1),
                &key(7),
            )
            .expect("valid physical prefix")
            {
                ParseOutcome::Incomplete { minimum_additional } => {
                    assert!(cut < first.as_bytes().len());
                    assert!(minimum_additional > 0);
                }
                ParseOutcome::Complete(decoded) => {
                    assert!(cut >= first.as_bytes().len());
                    assert_eq!(decoded.consumed(), first.as_bytes().len());
                    assert_eq!(decoded.plaintext(), b"first private event");
                    assert!(matches!(
                        parse_one(
                            &prefix[decoded.consumed()..],
                            &binding(LogFileKind::FolderEvents),
                            ordinal(2),
                            &key(7),
                        )
                        .expect("second valid physical prefix"),
                        ParseOutcome::Incomplete { .. }
                    ));
                }
            }
        }

        let ParseOutcome::Complete(first_decoded) = parse_one(
            &both,
            &binding(LogFileKind::FolderEvents),
            ordinal(1),
            &key(7),
        )
        .expect("first complete") else {
            panic!("first must be complete");
        };
        assert!(matches!(
            parse_one(
                &both[first_decoded.consumed()..],
                &binding(LogFileKind::FolderEvents),
                ordinal(2),
                &key(7),
            )
            .expect("second complete"),
            ParseOutcome::Complete(_)
        ));
    }

    #[test]
    fn binding_ordinal_kind_and_key_are_authenticated() {
        let frame = encoded(b"bound plaintext", 1);
        let bytes = frame.as_bytes();
        let original = binding(LogFileKind::FolderEvents);

        for wrong in [
            LogBinding::new(
                FolderId::from_uuid(Uuid::from_u128(9)),
                original.installation_id(),
                original.generation_id(),
                original.file_kind(),
            ),
            LogBinding::new(
                original.folder_id(),
                Uuid::from_u128(9),
                original.generation_id(),
                original.file_kind(),
            ),
            LogBinding::new(
                original.folder_id(),
                original.installation_id(),
                Uuid::from_u128(9),
                original.file_kind(),
            ),
        ] {
            assert!(matches!(
                parse_one(bytes, &wrong, ordinal(1), &key(7)),
                Err(LogFrameError::AuthenticationFailed)
            ));
        }
        assert!(matches!(
            parse_one(bytes, &original, ordinal(1), &key(8)),
            Err(LogFrameError::AuthenticationFailed)
        ));
        assert!(matches!(
            parse_one(bytes, &original, ordinal(2), &key(7)),
            Err(LogFrameError::UnexpectedOrdinal)
        ));
        assert!(matches!(
            parse_one(bytes, &binding(LogFileKind::Apply), ordinal(1), &key(7)),
            Err(LogFrameError::UnexpectedFileKind)
        ));
    }

    #[test]
    fn tamper_in_each_structural_region_is_a_hard_error() {
        let original = encoded(b"tamper target", 1).as_bytes().to_vec();
        let ciphertext_length = b"tamper target".len() + TAG_BYTES;
        let footer_offset = CIPHERTEXT_OFFSET + ciphertext_length;
        for offset in [
            0,
            FRAME_MAGIC.len(),
            FRAME_MAGIC.len() + 2,
            FRAME_MAGIC.len() + 2 + 1,
            FRAME_MAGIC.len() + 2 + 1 + 8 + 4,
            HEADER_BYTES,
            CIPHERTEXT_OFFSET,
            footer_offset,
            footer_offset + COMMIT_MAGIC.len(),
            footer_offset + COMMIT_MAGIC.len() + 8,
        ] {
            let mut changed = original.clone();
            changed[offset] ^= 0x01;
            assert_hard_error(&changed, 1);
        }
    }

    struct FailingNonceSource;

    impl NonceSource for FailingNonceSource {
        fn fill_nonce(&mut self, _nonce: &mut [u8; NONCE_BYTES]) -> Result<(), LogFrameError> {
            Err(LogFrameError::EntropyUnavailable)
        }
    }

    #[test]
    fn entropy_failure_returns_no_encoded_frame() {
        assert!(matches!(
            encode_with_nonce_source(
                &binding(LogFileKind::FolderEvents),
                ordinal(1),
                &key(7),
                b"private plaintext",
                &mut FailingNonceSource,
            ),
            Err(LogFrameError::EntropyUnavailable)
        ));
    }

    #[test]
    fn operating_system_entropy_produces_distinct_nonces() {
        let first = encoded(b"same plaintext", 1);
        let second = encoded(b"same plaintext", 1);
        assert_ne!(
            &first.as_bytes()[HEADER_BYTES..CIPHERTEXT_OFFSET],
            &second.as_bytes()[HEADER_BYTES..CIPHERTEXT_OFFSET]
        );
    }

    #[test]
    fn debug_output_redacts_key_and_plaintext() {
        let secret = b"never print this plaintext";
        let key = key(0x5a);
        let frame = encode_frame(
            &binding(LogFileKind::FolderEvents),
            ordinal(1),
            &key,
            secret,
        )
        .expect("encode");
        let parsed = parse_one(
            frame.as_bytes(),
            &binding(LogFileKind::FolderEvents),
            ordinal(1),
            &key,
        )
        .expect("parse");
        assert!(!format!("{key:?}").contains(&"5a".repeat(8)));
        assert!(!format!("{frame:?}").contains("never print"));
        assert!(!format!("{parsed:?}").contains("never print"));
    }
}
