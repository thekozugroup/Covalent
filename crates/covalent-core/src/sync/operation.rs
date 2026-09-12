//! Canonical, bounded signatures for future folder operation headers.
//!
//! This codec authenticates exact bytes under one supplied per-install Ed25519
//! key. A [`SignatureCheckedOperation`] is not admitted: callers must still
//! validate the signed writer-key binding, exact membership epoch record and
//! revocation, complete causal history, dot uniqueness, durable counter
//! allocation, and body/path semantics before appending or applying it.
//!
//! Version 1 uses this exact big-endian wire order: `COVSOP01` magic, `u16`
//! version, `u8` flags, 16-byte folder UUID, 16-byte writer UUID, `u64`
//! membership epoch, 32-byte membership-epoch-record digest, `u64`
//! folder-global author counter, optional 32-byte predecessor digest, `u16`
//! clock count, sorted (`writer UUID`, `u64` counter) clock components, `u32`
//! opaque body length, body bytes, and a 64-byte Ed25519 signature. Only flag
//! bit zero exists and means that the predecessor digest is present. The
//! signature covers every preceding wire byte prefixed by [`SIGNATURE_DOMAIN`].

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use thiserror::Error;
use uuid::Uuid;

use super::admission;
use super::ids::{FolderId, WriterId};
use super::register::{OpId, RegisterError};
use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector};

const MAGIC: &[u8; 8] = b"COVSOP01";
const WIRE_VERSION: u16 = 1;
const PREDECESSOR_PRESENT: u8 = 1;
const KNOWN_FLAGS: u8 = PREDECESSOR_PRESENT;
const SIGNATURE_BYTES: usize = 64;
const DIGEST_BYTES: usize = 32;
const CLOCK_ENTRY_BYTES: usize = 24;
const FIXED_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 16 + 16 + 8 + 32 + 8 + 2 + 4;

/// Domain prefix signed before the canonical unsigned record.
///
/// The terminating NUL makes concatenation with the record unambiguous and
/// separates these signatures from Covalent backup, pairing, and recovery.
pub const SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-operation-signature/v1\0";
/// Maximum accepted opaque operation body.
pub const MAX_OPERATION_BODY_BYTES: usize = 16 * 1_024;
/// Maximum accepted complete signed record, checked before parsing.
pub const MAX_SIGNED_OPERATION_BYTES: usize = 24 * 1_024;

/// A BLAKE3 commitment to a complete canonical signed record.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct OperationDigest([u8; DIGEST_BYTES]);

impl OperationDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// One positive component of a canonical causal clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockEntry {
    writer_id: WriterId,
    counter: u64,
}

impl ClockEntry {
    /// Builds one component. Full canonical ordering is checked by the codec.
    pub fn new(writer_id: WriterId, counter: u64) -> Result<Self, OperationCodecError> {
        if counter == 0 {
            return Err(OperationCodecError::ZeroClockCounter);
        }
        Ok(Self { writer_id, counter })
    }

    /// Returns the causal actor.
    #[must_use]
    pub const fn writer_id(self) -> WriterId {
        self.writer_id
    }

    /// Returns the positive observed counter.
    #[must_use]
    pub const fn counter(self) -> u64 {
        self.counter
    }
}

/// Authenticated operation metadata, before membership or history admission.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationHeader {
    folder_id: FolderId,
    writer_id: WriterId,
    membership_epoch: u64,
    membership_epoch_digest: [u8; DIGEST_BYTES],
    counter: u64,
    predecessor: Option<[u8; DIGEST_BYTES]>,
    clock: VersionVector,
}

impl OperationHeader {
    /// Returns the folder committed by the signature.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the distinct per-install writer committed by the signature.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the nonzero claimed membership epoch number.
    #[must_use]
    pub const fn membership_epoch(&self) -> u64 {
        self.membership_epoch
    }

    /// Returns the exact membership epoch record committed by the signature.
    #[must_use]
    pub const fn membership_epoch_digest(&self) -> [u8; DIGEST_BYTES] {
        self.membership_epoch_digest
    }

    /// Returns the nonzero folder-global author counter.
    #[must_use]
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    /// Returns the preceding same-author signed-record digest.
    #[must_use]
    pub const fn predecessor(&self) -> Option<[u8; DIGEST_BYTES]> {
        self.predecessor
    }

    /// Returns the complete causal clock committed by the signature.
    #[must_use]
    pub const fn clock(&self) -> &VersionVector {
        &self.clock
    }
}

/// A canonical operation whose bytes passed strict Ed25519 verification.
///
/// This name deliberately does not imply membership, epoch, causal, replay,
/// durability, or application-level authorization. The body remains opaque.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedOperation {
    header: OperationHeader,
    clock_entries: Vec<ClockEntry>,
    body: Vec<u8>,
    canonical_record: Vec<u8>,
    digest: OperationDigest,
}

impl fmt::Debug for SignatureCheckedOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedOperation")
            .field("header", &self.header)
            .field("clock_entry_count", &self.clock_entries.len())
            .field("body_length", &self.body.len())
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedOperation {
    /// Returns authenticated header fields that still require admission.
    #[must_use]
    pub const fn header(&self) -> &OperationHeader {
        &self.header
    }

    /// Returns canonical writer-typed clock components.
    #[must_use]
    pub fn clock_entries(&self) -> &[ClockEntry] {
        &self.clock_entries
    }

    /// Returns the authenticated opaque body without interpreting it.
    #[must_use]
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to the complete record, including signature.
    #[must_use]
    pub const fn digest(&self) -> OperationDigest {
        self.digest
    }

    /// Converts causal fields for the separate history admission check.
    ///
    /// The caller must first authorize this exact folder, writer key, epoch
    /// number, and epoch digest, and must supply history scoped to this folder.
    /// Conversion intentionally omits authorization fields and does not prove
    /// any of those preconditions.
    pub fn to_admission_header(&self) -> Result<admission::Header, RegisterError> {
        let id = OpId::new(
            self.header.writer_id.into_vector_actor(),
            self.header.counter,
        )?;
        Ok(admission::Header::new(
            id,
            self.header.clock.clone(),
            self.digest.0,
            self.header.predecessor,
        ))
    }
}

/// Structural, canonicality, binding, or signature failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OperationCodecError {
    /// The complete record exceeds its pre-parse limit.
    #[error("signed operation exceeds the record limit")]
    RecordTooLarge,
    /// The opaque body exceeds its independent limit.
    #[error("signed operation body exceeds the body limit")]
    BodyTooLarge,
    /// Length arithmetic could not be represented.
    #[error("signed operation length overflow")]
    LengthOverflow,
    /// The record ended before a complete field could be read.
    #[error("signed operation is truncated")]
    Truncated,
    /// Bytes followed the one canonical record.
    #[error("signed operation has trailing bytes")]
    TrailingBytes,
    /// The wire magic is not the operation codec domain.
    #[error("invalid signed-operation magic")]
    InvalidMagic,
    /// The wire version is not supported.
    #[error("unsupported signed-operation version")]
    UnsupportedVersion,
    /// A reserved flag bit was set.
    #[error("signed operation contains unknown flags")]
    UnknownFlags,
    /// Epoch zero is not an authorized membership epoch.
    #[error("signed operation membership epoch must be nonzero")]
    ZeroEpoch,
    /// Author counter zero is not an operation dot.
    #[error("signed operation author counter must be nonzero")]
    ZeroCounter,
    /// Counter one or a later counter used the wrong predecessor shape.
    #[error("signed operation predecessor does not match its author counter")]
    InvalidPredecessor,
    /// A causal component used zero rather than omission.
    #[error("signed operation clock counters must be positive")]
    ZeroClockCounter,
    /// More than 128 clock actors were supplied.
    #[error("signed operation clock has too many actors")]
    TooManyActors,
    /// Clock actors were duplicated or not in ascending UUID-byte order.
    #[error("signed operation clock is not in canonical actor order")]
    NonCanonicalClock,
    /// The author's clock component does not equal the operation counter.
    #[error("signed operation dot does not match its causal clock")]
    DotClockMismatch,
    /// The authenticated record belongs to another expected folder.
    #[error("signed operation folder binding does not match")]
    FolderMismatch,
    /// The authenticated record names another expected writer.
    #[error("signed operation writer binding does not match")]
    WriterMismatch,
    /// The supplied verification key is weak.
    #[error("signed operation writer key is weak")]
    WeakWriterKey,
    /// Strict Ed25519 verification failed.
    #[error("signed operation signature is invalid")]
    InvalidSignature,
}

/// Encodes and signs one canonical operation.
///
/// Entropy, key generation, writer-key binding, durable counter allocation,
/// and all body semantics belong to the caller. Bounds and causal shape are
/// checked before body or clock bytes are cloned into the result.
#[allow(clippy::too_many_arguments)]
pub fn encode_signed_operation(
    signing_key: &SigningKey,
    folder_id: FolderId,
    writer_id: WriterId,
    membership_epoch: u64,
    membership_epoch_digest: [u8; DIGEST_BYTES],
    counter: u64,
    predecessor: Option<[u8; DIGEST_BYTES]>,
    clock: &[ClockEntry],
    body: &[u8],
) -> Result<Vec<u8>, OperationCodecError> {
    validate_shape(
        writer_id,
        membership_epoch,
        counter,
        predecessor,
        clock,
        body.len(),
    )?;
    if signing_key.verifying_key().is_weak() {
        return Err(OperationCodecError::WeakWriterKey);
    }
    let unsigned_len = encoded_unsigned_len(predecessor.is_some(), clock.len(), body.len())?;
    let record_len = unsigned_len
        .checked_add(SIGNATURE_BYTES)
        .ok_or(OperationCodecError::LengthOverflow)?;
    if record_len > MAX_SIGNED_OPERATION_BYTES {
        return Err(OperationCodecError::RecordTooLarge);
    }

    let mut record = Vec::with_capacity(record_len);
    encode_unsigned(
        &mut record,
        folder_id,
        writer_id,
        membership_epoch,
        membership_epoch_digest,
        counter,
        predecessor,
        clock,
        body,
    );
    let signature = signing_key.sign(&signature_message(&record));
    record.extend_from_slice(&signature.to_bytes());
    Ok(record)
}

/// Parses canonical bytes and strictly verifies their expected bindings.
///
/// The fixed record bound is enforced before any parse-time allocation.
pub fn decode_signature_checked_operation(
    record: &[u8],
    expected_folder: FolderId,
    expected_writer: WriterId,
    writer_key: &VerifyingKey,
) -> Result<SignatureCheckedOperation, OperationCodecError> {
    if record.len() > MAX_SIGNED_OPERATION_BYTES {
        return Err(OperationCodecError::RecordTooLarge);
    }
    if writer_key.is_weak() {
        return Err(OperationCodecError::WeakWriterKey);
    }

    let mut cursor = Cursor::new(record);
    if cursor.take_array::<8>()? != *MAGIC {
        return Err(OperationCodecError::InvalidMagic);
    }
    if cursor.take_u16()? != WIRE_VERSION {
        return Err(OperationCodecError::UnsupportedVersion);
    }
    let flags = cursor.take_u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(OperationCodecError::UnknownFlags);
    }
    let folder_id = FolderId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let membership_epoch = cursor.take_u64()?;
    let membership_epoch_digest = cursor.take_array::<DIGEST_BYTES>()?;
    let counter = cursor.take_u64()?;
    let predecessor = if flags & PREDECESSOR_PRESENT != 0 {
        Some(cursor.take_array::<DIGEST_BYTES>()?)
    } else {
        None
    };
    let clock_count = usize::from(cursor.take_u16()?);
    if clock_count > MAX_VERSION_VECTOR_ACTORS {
        return Err(OperationCodecError::TooManyActors);
    }
    let mut clock_entries = Vec::with_capacity(clock_count);
    for _ in 0..clock_count {
        let actor = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let component_counter = cursor.take_u64()?;
        clock_entries.push(ClockEntry {
            writer_id: actor,
            counter: component_counter,
        });
    }
    let body_len =
        usize::try_from(cursor.take_u32()?).map_err(|_| OperationCodecError::LengthOverflow)?;
    if body_len > MAX_OPERATION_BODY_BYTES {
        return Err(OperationCodecError::BodyTooLarge);
    }
    let body = cursor.take(body_len)?;
    let unsigned_len = cursor.position();
    let signature_bytes = cursor.take_array::<SIGNATURE_BYTES>()?;
    if cursor.remaining() != 0 {
        return Err(OperationCodecError::TrailingBytes);
    }

    validate_shape(
        writer_id,
        membership_epoch,
        counter,
        predecessor,
        &clock_entries,
        body.len(),
    )?;
    let signature = Signature::from_bytes(&signature_bytes);
    writer_key
        .verify_strict(&signature_message(&record[..unsigned_len]), &signature)
        .map_err(|_| OperationCodecError::InvalidSignature)?;
    if folder_id != expected_folder {
        return Err(OperationCodecError::FolderMismatch);
    }
    if writer_id != expected_writer {
        return Err(OperationCodecError::WriterMismatch);
    }

    let clock = VersionVector::new(
        clock_entries
            .iter()
            .map(|entry| (entry.writer_id.into_vector_actor(), entry.counter)),
    )
    .map_err(|_| OperationCodecError::NonCanonicalClock)?;
    let canonical_record = record.to_vec();
    let digest = OperationDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedOperation {
        header: OperationHeader {
            folder_id,
            writer_id,
            membership_epoch,
            membership_epoch_digest,
            counter,
            predecessor,
            clock,
        },
        clock_entries,
        body: body.to_vec(),
        canonical_record,
        digest,
    })
}

fn validate_shape(
    writer_id: WriterId,
    membership_epoch: u64,
    counter: u64,
    predecessor: Option<[u8; DIGEST_BYTES]>,
    clock: &[ClockEntry],
    body_len: usize,
) -> Result<(), OperationCodecError> {
    if body_len > MAX_OPERATION_BODY_BYTES {
        return Err(OperationCodecError::BodyTooLarge);
    }
    if clock.len() > MAX_VERSION_VECTOR_ACTORS {
        return Err(OperationCodecError::TooManyActors);
    }
    if membership_epoch == 0 {
        return Err(OperationCodecError::ZeroEpoch);
    }
    if counter == 0 {
        return Err(OperationCodecError::ZeroCounter);
    }
    if (counter == 1) != predecessor.is_none() {
        return Err(OperationCodecError::InvalidPredecessor);
    }

    let mut previous: Option<[u8; 16]> = None;
    let mut writer_counter = 0;
    for entry in clock {
        if entry.counter == 0 {
            return Err(OperationCodecError::ZeroClockCounter);
        }
        let actor_bytes = entry.writer_id.to_bytes();
        if previous.is_some_and(|prior| prior >= actor_bytes) {
            return Err(OperationCodecError::NonCanonicalClock);
        }
        previous = Some(actor_bytes);
        if entry.writer_id == writer_id {
            writer_counter = entry.counter;
        }
    }
    if writer_counter != counter {
        return Err(OperationCodecError::DotClockMismatch);
    }
    let _ = encoded_unsigned_len(predecessor.is_some(), clock.len(), body_len)?;
    Ok(())
}

fn encoded_unsigned_len(
    has_predecessor: bool,
    clock_count: usize,
    body_len: usize,
) -> Result<usize, OperationCodecError> {
    let clock_bytes = clock_count
        .checked_mul(CLOCK_ENTRY_BYTES)
        .ok_or(OperationCodecError::LengthOverflow)?;
    FIXED_UNSIGNED_BYTES
        .checked_add(if has_predecessor { DIGEST_BYTES } else { 0 })
        .and_then(|length| length.checked_add(clock_bytes))
        .and_then(|length| length.checked_add(body_len))
        .ok_or(OperationCodecError::LengthOverflow)
}

#[allow(clippy::too_many_arguments)]
fn encode_unsigned(
    output: &mut Vec<u8>,
    folder_id: FolderId,
    writer_id: WriterId,
    membership_epoch: u64,
    membership_epoch_digest: [u8; DIGEST_BYTES],
    counter: u64,
    predecessor: Option<[u8; DIGEST_BYTES]>,
    clock: &[ClockEntry],
    body: &[u8],
) {
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
    output.push(u8::from(predecessor.is_some()));
    output.extend_from_slice(&folder_id.to_bytes());
    output.extend_from_slice(&writer_id.to_bytes());
    output.extend_from_slice(&membership_epoch.to_be_bytes());
    output.extend_from_slice(&membership_epoch_digest);
    output.extend_from_slice(&counter.to_be_bytes());
    if let Some(digest) = predecessor {
        output.extend_from_slice(&digest);
    }
    output.extend_from_slice(&(clock.len() as u16).to_be_bytes());
    for entry in clock {
        output.extend_from_slice(&entry.writer_id.to_bytes());
        output.extend_from_slice(&entry.counter.to_be_bytes());
    }
    output.extend_from_slice(&(body.len() as u32).to_be_bytes());
    output.extend_from_slice(body);
}

fn signature_message(unsigned_record: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(SIGNATURE_DOMAIN.len() + unsigned_record.len());
    message.extend_from_slice(SIGNATURE_DOMAIN);
    message.extend_from_slice(unsigned_record);
    message
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn position(&self) -> usize {
        self.position
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], OperationCodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(OperationCodecError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(OperationCodecError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], OperationCodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| OperationCodecError::Truncated)
    }

    fn take_u8(&mut self) -> Result<u8, OperationCodecError> {
        Ok(self.take_array::<1>()?[0])
    }

    fn take_u16(&mut self) -> Result<u16, OperationCodecError> {
        Ok(u16::from_be_bytes(self.take_array::<2>()?))
    }

    fn take_u32(&mut self) -> Result<u32, OperationCodecError> {
        Ok(u32::from_be_bytes(self.take_array::<4>()?))
    }

    fn take_u64(&mut self) -> Result<u64, OperationCodecError> {
        Ok(u64::from_be_bytes(self.take_array::<8>()?))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
    use uuid::Uuid;

    use super::*;

    const FLAGS_OFFSET: usize = 10;
    const FOLDER_OFFSET: usize = 11;
    const WRITER_OFFSET: usize = FOLDER_OFFSET + 16;
    const EPOCH_OFFSET: usize = WRITER_OFFSET + 16;
    const EPOCH_DIGEST_OFFSET: usize = EPOCH_OFFSET + 8;
    const COUNTER_OFFSET: usize = EPOCH_DIGEST_OFFSET + DIGEST_BYTES;
    const CLOCK_COUNT_OFFSET_FIRST: usize = COUNTER_OFFSET + 8;
    const CLOCK_ENTRY_OFFSET_FIRST: usize = CLOCK_COUNT_OFFSET_FIRST + 2;
    const BODY_LENGTH_OFFSET_FIRST: usize = CLOCK_ENTRY_OFFSET_FIRST + CLOCK_ENTRY_BYTES;
    const BODY_OFFSET_FIRST: usize = BODY_LENGTH_OFFSET_FIRST + 4;

    fn folder(number: u128) -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(number))
    }

    fn writer(number: u128) -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(number))
    }

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn first_record() -> Vec<u8> {
        encode_signed_operation(
            &key(7),
            folder(1),
            writer(2),
            3,
            [9; DIGEST_BYTES],
            1,
            None,
            &[ClockEntry::new(writer(2), 1).expect("clock")],
            b"opaque body",
        )
        .expect("record")
    }

    #[allow(clippy::too_many_arguments)]
    fn raw_record(
        signing_key: &SigningKey,
        folder_id: FolderId,
        writer_id: WriterId,
        epoch: u64,
        epoch_digest: [u8; DIGEST_BYTES],
        counter: u64,
        predecessor: Option<[u8; DIGEST_BYTES]>,
        clock: &[ClockEntry],
        body: &[u8],
    ) -> Vec<u8> {
        let mut unsigned = Vec::new();
        encode_unsigned(
            &mut unsigned,
            folder_id,
            writer_id,
            epoch,
            epoch_digest,
            counter,
            predecessor,
            clock,
            body,
        );
        let signature = signing_key.sign(&signature_message(&unsigned));
        unsigned.extend_from_slice(&signature.to_bytes());
        unsigned
    }

    fn decode(record: &[u8]) -> Result<SignatureCheckedOperation, OperationCodecError> {
        decode_signature_checked_operation(record, folder(1), writer(2), &key(7).verifying_key())
    }

    #[test]
    fn signed_round_trip_is_canonical_and_converts_to_admission_header() {
        let record = first_record();
        let checked = decode(&record).expect("verified record");

        assert_eq!(checked.canonical_record(), record);
        assert_eq!(checked.body(), b"opaque body");
        assert_eq!(checked.header().folder_id(), folder(1));
        assert_eq!(checked.header().writer_id(), writer(2));
        assert_eq!(checked.header().membership_epoch(), 3);
        assert_eq!(checked.header().membership_epoch_digest(), [9; 32]);
        assert_eq!(checked.header().counter(), 1);
        assert_eq!(checked.header().predecessor(), None);
        assert_eq!(checked.clock_entries()[0].writer_id(), writer(2));
        assert_eq!(checked.clock_entries()[0].counter(), 1);
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        let header = checked.to_admission_header().expect("header");
        assert_eq!(header.digest(), checked.digest().to_bytes());
        let debug = format!("{checked:?}");
        assert!(!debug.contains("opaque body"));
    }

    #[test]
    fn expected_bindings_and_writer_key_are_enforced() {
        let record = first_record();
        let verification_key = key(7).verifying_key();
        assert_eq!(
            decode_signature_checked_operation(&record, folder(9), writer(2), &verification_key),
            Err(OperationCodecError::FolderMismatch)
        );
        assert_eq!(
            decode_signature_checked_operation(&record, folder(1), writer(9), &verification_key),
            Err(OperationCodecError::WriterMismatch)
        );
        assert_eq!(
            decode_signature_checked_operation(
                &record,
                folder(1),
                writer(2),
                &key(8).verifying_key()
            ),
            Err(OperationCodecError::InvalidSignature)
        );
    }

    #[test]
    fn every_wire_field_and_signature_is_committed() {
        let record = first_record();
        let offsets = [
            0,
            8,
            FLAGS_OFFSET,
            FOLDER_OFFSET,
            WRITER_OFFSET,
            EPOCH_OFFSET,
            EPOCH_DIGEST_OFFSET,
            COUNTER_OFFSET,
            CLOCK_COUNT_OFFSET_FIRST + 1,
            CLOCK_ENTRY_OFFSET_FIRST,
            CLOCK_ENTRY_OFFSET_FIRST + 16,
            BODY_LENGTH_OFFSET_FIRST + 3,
            BODY_OFFSET_FIRST,
            record.len() - 1,
        ];
        for offset in offsets {
            let mut tampered = record.clone();
            tampered[offset] ^= 0x80;
            assert!(
                decode(&tampered).is_err(),
                "tamper at offset {offset} was accepted"
            );
        }
    }

    #[test]
    fn predecessor_is_signed_and_first_counter_rule_rejects_gaps() {
        let record = encode_signed_operation(
            &key(7),
            folder(1),
            writer(2),
            3,
            [9; DIGEST_BYTES],
            2,
            Some([4; DIGEST_BYTES]),
            &[ClockEntry::new(writer(2), 2).expect("clock")],
            b"body",
        )
        .expect("record");
        let mut tampered = record;
        tampered[COUNTER_OFFSET + 8] ^= 1;
        assert_eq!(
            decode(&tampered),
            Err(OperationCodecError::InvalidSignature)
        );

        let counter_one_with_predecessor = raw_record(
            &key(7),
            folder(1),
            writer(2),
            3,
            [9; 32],
            1,
            Some([0; 32]),
            &[ClockEntry::new(writer(2), 1).expect("clock")],
            b"",
        );
        assert_eq!(
            decode(&counter_one_with_predecessor),
            Err(OperationCodecError::InvalidPredecessor)
        );
        let later_without_predecessor = raw_record(
            &key(7),
            folder(1),
            writer(2),
            3,
            [9; 32],
            2,
            None,
            &[ClockEntry::new(writer(2), 2).expect("clock")],
            b"",
        );
        assert_eq!(
            decode(&later_without_predecessor),
            Err(OperationCodecError::InvalidPredecessor)
        );
    }

    #[test]
    fn trailing_oversized_and_every_truncated_prefix_fail_closed() {
        let record = first_record();
        for end in 0..record.len() {
            assert!(decode(&record[..end]).is_err(), "prefix {end} was accepted");
        }
        let mut trailing = record;
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(OperationCodecError::TrailingBytes));
        assert_eq!(
            decode(&vec![0; MAX_SIGNED_OPERATION_BYTES + 1]),
            Err(OperationCodecError::RecordTooLarge)
        );
        assert_eq!(
            encode_signed_operation(
                &key(7),
                folder(1),
                writer(2),
                3,
                [9; 32],
                1,
                None,
                &[ClockEntry::new(writer(2), 1).expect("clock")],
                &vec![0; MAX_OPERATION_BODY_BYTES + 1]
            ),
            Err(OperationCodecError::BodyTooLarge)
        );
        let too_many = (0..=MAX_VERSION_VECTOR_ACTORS)
            .map(|number| ClockEntry::new(writer(number as u128 + 1), 1).expect("clock"))
            .collect::<Vec<_>>();
        assert_eq!(
            encode_signed_operation(
                &key(7),
                folder(1),
                writer(1),
                3,
                [9; 32],
                1,
                None,
                &too_many,
                b""
            ),
            Err(OperationCodecError::TooManyActors)
        );
    }

    #[test]
    fn zero_epoch_counter_clock_and_dot_mismatch_are_rejected() {
        let valid_clock = [ClockEntry::new(writer(2), 1).expect("clock")];
        let zero_epoch = raw_record(
            &key(7),
            folder(1),
            writer(2),
            0,
            [9; 32],
            1,
            None,
            &valid_clock,
            b"",
        );
        assert_eq!(decode(&zero_epoch), Err(OperationCodecError::ZeroEpoch));
        let zero_counter = raw_record(
            &key(7),
            folder(1),
            writer(2),
            1,
            [9; 32],
            0,
            None,
            &valid_clock,
            b"",
        );
        assert_eq!(decode(&zero_counter), Err(OperationCodecError::ZeroCounter));
        let dot_mismatch = raw_record(
            &key(7),
            folder(1),
            writer(2),
            1,
            [9; 32],
            1,
            None,
            &[ClockEntry::new(writer(2), 2).expect("clock")],
            b"",
        );
        assert_eq!(
            decode(&dot_mismatch),
            Err(OperationCodecError::DotClockMismatch)
        );
        assert_eq!(
            ClockEntry::new(writer(2), 0),
            Err(OperationCodecError::ZeroClockCounter)
        );
    }

    #[test]
    fn hostile_clock_count_order_duplicates_and_zero_are_rejected() {
        let mut excessive = first_record();
        excessive[CLOCK_COUNT_OFFSET_FIRST..CLOCK_COUNT_OFFSET_FIRST + 2]
            .copy_from_slice(&129_u16.to_be_bytes());
        assert_eq!(decode(&excessive), Err(OperationCodecError::TooManyActors));

        for (clock, expected) in [
            (
                vec![
                    ClockEntry::new(writer(3), 1).expect("clock"),
                    ClockEntry::new(writer(2), 1).expect("clock"),
                ],
                OperationCodecError::NonCanonicalClock,
            ),
            (
                vec![
                    ClockEntry::new(writer(2), 1).expect("clock"),
                    ClockEntry::new(writer(2), 1).expect("clock"),
                ],
                OperationCodecError::NonCanonicalClock,
            ),
        ] {
            let record = raw_record(
                &key(7),
                folder(1),
                writer(2),
                1,
                [9; 32],
                1,
                None,
                &clock,
                b"",
            );
            assert_eq!(decode(&record), Err(expected));
        }

        let zero_clock = raw_record(
            &key(7),
            folder(1),
            writer(2),
            1,
            [9; 32],
            1,
            None,
            &[ClockEntry {
                writer_id: writer(2),
                counter: 0,
            }],
            b"",
        );
        assert_eq!(
            decode(&zero_clock),
            Err(OperationCodecError::ZeroClockCounter)
        );
    }

    #[test]
    fn maximum_body_and_clock_round_trip_and_oversized_wire_body_is_rejected() {
        let clock = (1..=MAX_VERSION_VECTOR_ACTORS)
            .map(|number| ClockEntry::new(writer(number as u128), 2).expect("clock"))
            .collect::<Vec<_>>();
        let body = vec![0x5a; MAX_OPERATION_BODY_BYTES];
        let record = encode_signed_operation(
            &key(7),
            folder(1),
            writer(2),
            3,
            [9; 32],
            2,
            Some([4; 32]),
            &clock,
            &body,
        )
        .expect("maximum valid record");
        assert!(record.len() <= MAX_SIGNED_OPERATION_BYTES);
        let checked = decode(&record).expect("maximum record verifies");
        assert_eq!(checked.clock_entries().len(), MAX_VERSION_VECTOR_ACTORS);
        assert_eq!(checked.body(), body);
        assert_eq!(checked.canonical_record(), record);

        let mut excessive = first_record();
        excessive[BODY_LENGTH_OFFSET_FIRST..BODY_LENGTH_OFFSET_FIRST + 4]
            .copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(decode(&excessive), Err(OperationCodecError::BodyTooLarge));
    }

    #[test]
    fn unknown_version_flags_and_weak_keys_are_rejected() {
        let mut version = first_record();
        version[8..10].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            decode(&version),
            Err(OperationCodecError::UnsupportedVersion)
        );
        let mut flags = first_record();
        flags[FLAGS_OFFSET] = 0x80;
        assert_eq!(decode(&flags), Err(OperationCodecError::UnknownFlags));

        let mut identity_bytes = [0_u8; 32];
        identity_bytes[0] = 1;
        let weak = VerifyingKey::from_bytes(&identity_bytes).expect("identity point");
        assert!(weak.is_weak());
        assert_eq!(
            decode_signature_checked_operation(&first_record(), folder(1), writer(2), &weak),
            Err(OperationCodecError::WeakWriterKey)
        );
    }

    #[test]
    fn signature_domain_is_required() {
        let record = first_record();
        let unsigned = &record[..record.len() - SIGNATURE_BYTES];
        let mut wrong_domain = unsigned.to_vec();
        wrong_domain.extend_from_slice(&key(7).sign(unsigned).to_bytes());
        assert_eq!(
            decode(&wrong_domain),
            Err(OperationCodecError::InvalidSignature)
        );
    }

    #[test]
    fn adversarial_bounded_inputs_do_not_panic() {
        let verification_key = key(7).verifying_key();
        for length in (0..=MAX_SIGNED_OPERATION_BYTES).step_by(257) {
            let sample = vec![(length as u8).wrapping_mul(31); length];
            let result = std::panic::catch_unwind(|| {
                decode_signature_checked_operation(&sample, folder(1), writer(2), &verification_key)
            });
            assert!(result.is_ok(), "parser panicked for length {length}");
            assert!(result.expect("no panic").is_err());
        }
    }
}
