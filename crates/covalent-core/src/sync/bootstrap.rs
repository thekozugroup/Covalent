//! Canonical bootstrap permits and candidate receipts for future folder sync.
//!
//! A [`SignatureCheckedBootstrapPermit`] only authenticates authority-signed
//! permission to perform bounded, read-only bootstrap. Its requested final role
//! grants no membership, write, acknowledgement, or policy capability. Runtime
//! code must enforce the current epoch head, expiration, nonce single use,
//! authenticated transport, complete history/content transfer, quotas, and a
//! durable staged apply before permitting receipt signing.
//! The authority workflow must likewise prove that `required_frontier` is
//! causally closed before signing; this byte codec cannot establish history.
//!
//! A [`SignatureCheckedBootstrapReceipt`] authenticates the candidate's claim
//! that it applied at least the permit's required causal frontier. It does not
//! prove that bytes were physically fetched or applied. A future membership Add
//! validator must retain its applied frontier as the minimum causal context for
//! that writer's later operations. A removed writer never re-enters under its
//! old ID; regaining access requires a fresh writer and bootstrap.
//!
//! Both version-1 formats use fixed-width big-endian fields. The permit order is
//! `COVSBP01`, version, zero flags, folder, authority writer, base epoch and
//! digest, candidate member grant, nonce, expiry, canonical required frontier,
//! and authority signature. The receipt order is `COVSBR01`, version, zero
//! flags, complete signed-permit digest, folder, base epoch and digest,
//! candidate writer, canonical applied frontier, root state commitment, and
//! candidate signature.

use std::fmt;

use covalent_protocol::DeviceId;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use thiserror::Error;
use uuid::Uuid;

use super::ids::{FolderId, WriterId};
use super::membership::{EpochDigest, MemberGrant, MemberRole};
use super::operation::ClockEntry;
use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector, VersionVectorOrder};

const PERMIT_MAGIC: &[u8; 8] = b"COVSBP01";
const RECEIPT_MAGIC: &[u8; 8] = b"COVSBR01";
const WIRE_VERSION: u16 = 1;
const KNOWN_FLAGS: u8 = 0;
const SIGNATURE_BYTES: usize = 64;
const DIGEST_BYTES: usize = 32;
const MEMBER_GRANT_BYTES: usize = 16 + 32 + 16 + 32 + 1;
const FRONTIER_ENTRY_BYTES: usize = 16 + 8;
const FIXED_PERMIT_UNSIGNED_BYTES: usize =
    8 + 2 + 1 + 16 + 16 + 8 + 32 + MEMBER_GRANT_BYTES + 32 + 8 + 2;
const FIXED_RECEIPT_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 32 + 16 + 8 + 32 + 16 + 2 + 32;

/// Domain prefix for authority-signed bootstrap permits.
pub const PERMIT_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-bootstrap-permit-signature/v1\0";
/// Domain prefix for candidate-signed bootstrap receipts.
pub const RECEIPT_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-bootstrap-receipt-signature/v1\0";
/// Maximum complete permit or receipt size, checked before parsing.
pub const MAX_SIGNED_BOOTSTRAP_RECORD_BYTES: usize = 4 * 1_024;

/// BLAKE3 commitment to a complete canonical signed permit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BootstrapPermitDigest([u8; DIGEST_BYTES]);

impl BootstrapPermitDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// BLAKE3 commitment to a complete canonical signed candidate receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct BootstrapReceiptDigest([u8; DIGEST_BYTES]);

impl BootstrapReceiptDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// Authority-authenticated bootstrap fields, not an accepted capability.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedBootstrapPermit {
    folder_id: FolderId,
    authority_writer_id: WriterId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    candidate: MemberGrant,
    nonce: [u8; DIGEST_BYTES],
    expires_at_unix_ms: u64,
    required_frontier_entries: Vec<ClockEntry>,
    required_frontier: VersionVector,
    canonical_record: Vec<u8>,
    digest: BootstrapPermitDigest,
}

impl fmt::Debug for SignatureCheckedBootstrapPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedBootstrapPermit")
            .field("folder_id", &self.folder_id)
            .field("authority_writer_id", &self.authority_writer_id)
            .field("base_epoch", &self.base_epoch)
            .field("candidate_writer_id", &self.candidate.writer_id())
            .field("requested_final_role", &self.candidate.role())
            .field("expires_at_unix_ms", &self.expires_at_unix_ms)
            .field(
                "required_frontier_count",
                &self.required_frontier_entries.len(),
            )
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedBootstrapPermit {
    /// Returns the folder being bootstrapped.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the authority writer expected from the pinned genesis.
    #[must_use]
    pub const fn authority_writer_id(&self) -> WriterId {
        self.authority_writer_id
    }

    /// Returns the positive base membership epoch.
    #[must_use]
    pub const fn base_epoch(&self) -> u64 {
        self.base_epoch
    }

    /// Returns the exact base membership record commitment.
    #[must_use]
    pub const fn base_epoch_digest(&self) -> EpochDigest {
        self.base_epoch_digest
    }

    /// Returns the candidate's requested eventual membership binding.
    ///
    /// Its role is descriptive until a later accepted membership Add.
    #[must_use]
    pub const fn candidate(&self) -> &MemberGrant {
        &self.candidate
    }

    /// Returns the externally generated nonce that future runtime code must
    /// durably enforce as single use.
    #[must_use]
    pub const fn nonce(&self) -> [u8; DIGEST_BYTES] {
        self.nonce
    }

    /// Returns the positive expiry timestamp. The codec does not read a clock.
    #[must_use]
    pub const fn expires_at_unix_ms(&self) -> u64 {
        self.expires_at_unix_ms
    }

    /// Returns writer-typed required frontier components.
    #[must_use]
    pub fn required_frontier_entries(&self) -> &[ClockEntry] {
        &self.required_frontier_entries
    }

    /// Returns the required frontier in the causal-math representation.
    #[must_use]
    pub const fn required_frontier(&self) -> &VersionVector {
        &self.required_frontier
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to all permit fields and its signature.
    #[must_use]
    pub const fn digest(&self) -> BootstrapPermitDigest {
        self.digest
    }
}

/// Candidate-authenticated receipt fields, not an accepted membership Add.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedBootstrapReceipt {
    permit_digest: BootstrapPermitDigest,
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    candidate_writer_id: WriterId,
    applied_frontier_entries: Vec<ClockEntry>,
    applied_frontier: VersionVector,
    root_state_commitment: [u8; DIGEST_BYTES],
    canonical_record: Vec<u8>,
    digest: BootstrapReceiptDigest,
}

impl fmt::Debug for SignatureCheckedBootstrapReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedBootstrapReceipt")
            .field("permit_digest", &self.permit_digest)
            .field("folder_id", &self.folder_id)
            .field("base_epoch", &self.base_epoch)
            .field("candidate_writer_id", &self.candidate_writer_id)
            .field(
                "applied_frontier_count",
                &self.applied_frontier_entries.len(),
            )
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedBootstrapReceipt {
    /// Returns the exact complete signed-permit commitment.
    #[must_use]
    pub const fn permit_digest(&self) -> BootstrapPermitDigest {
        self.permit_digest
    }

    /// Returns the folder copied from the exact permit.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the base epoch copied from the exact permit.
    #[must_use]
    pub const fn base_epoch(&self) -> u64 {
        self.base_epoch
    }

    /// Returns the exact base epoch record commitment.
    #[must_use]
    pub const fn base_epoch_digest(&self) -> EpochDigest {
        self.base_epoch_digest
    }

    /// Returns the candidate writer that signed the receipt.
    #[must_use]
    pub const fn candidate_writer_id(&self) -> WriterId {
        self.candidate_writer_id
    }

    /// Returns writer-typed applied frontier components.
    #[must_use]
    pub fn applied_frontier_entries(&self) -> &[ClockEntry] {
        &self.applied_frontier_entries
    }

    /// Returns the claimed applied frontier in causal-math representation.
    #[must_use]
    pub const fn applied_frontier(&self) -> &VersionVector {
        &self.applied_frontier
    }

    /// Returns the opaque commitment to the applied root inventory and content.
    #[must_use]
    pub const fn root_state_commitment(&self) -> [u8; DIGEST_BYTES] {
        self.root_state_commitment
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to all receipt fields and its signature.
    #[must_use]
    pub const fn digest(&self) -> BootstrapReceiptDigest {
        self.digest
    }
}

/// Structural, canonicality, binding, or signature failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BootstrapCodecError {
    /// A complete record exceeds the pre-parse limit.
    #[error("signed bootstrap record exceeds the record limit")]
    RecordTooLarge,
    /// Length arithmetic could not be represented.
    #[error("signed bootstrap record length overflow")]
    LengthOverflow,
    /// A record ended before a complete field could be read.
    #[error("signed bootstrap record is truncated")]
    Truncated,
    /// Bytes followed the one canonical record.
    #[error("signed bootstrap record has trailing bytes")]
    TrailingBytes,
    /// The record magic does not match its bootstrap record kind.
    #[error("invalid signed bootstrap record magic")]
    InvalidMagic,
    /// The wire version is unsupported.
    #[error("unsupported signed bootstrap record version")]
    UnsupportedVersion,
    /// A reserved flag bit was set.
    #[error("signed bootstrap record contains unknown flags")]
    UnknownFlags,
    /// The membership base epoch is zero.
    #[error("bootstrap base epoch must be positive")]
    ZeroBaseEpoch,
    /// The permit expiry timestamp is zero.
    #[error("bootstrap permit expiry must be positive")]
    ZeroExpiry,
    /// A frontier exceeds the 128-actor bound.
    #[error("bootstrap frontier has too many actors")]
    TooManyActors,
    /// A frontier counter used zero rather than omission.
    #[error("bootstrap frontier counters must be positive")]
    ZeroFrontierCounter,
    /// Frontier actors were duplicated or out of canonical order.
    #[error("bootstrap frontier is not in canonical writer order")]
    NonCanonicalFrontier,
    /// A candidate writer key is malformed.
    #[error("bootstrap candidate writer key is invalid")]
    InvalidWriterKey,
    /// A candidate writer key is weak.
    #[error("bootstrap candidate writer key is weak")]
    WeakWriterKey,
    /// A candidate transport key is malformed.
    #[error("bootstrap candidate transport key is invalid")]
    InvalidTransportKey,
    /// A candidate transport key is weak.
    #[error("bootstrap candidate transport key is weak")]
    WeakTransportKey,
    /// A candidate reused one key for writer and transport identities.
    #[error("bootstrap candidate reuses a writer or transport key")]
    ReusedCandidateKey,
    /// A member role byte is unknown.
    #[error("bootstrap candidate role is unknown")]
    UnknownRole,
    /// The typed transport device ID could not be represented as UUID bytes.
    #[error("bootstrap candidate transport device identifier is invalid")]
    InvalidTransportDeviceId,
    /// Candidate and authority writer IDs are identical.
    #[error("bootstrap candidate writer must differ from the authority")]
    CandidateWriterIsAuthority,
    /// A candidate key is the pinned authority writer key.
    #[error("bootstrap candidate key must differ from the authority key")]
    CandidateKeyReusesAuthority,
    /// The separately pinned authority key is weak.
    #[error("bootstrap authority key is weak")]
    WeakAuthorityKey,
    /// A permit belongs to another expected folder.
    #[error("bootstrap permit folder binding does not match")]
    FolderMismatch,
    /// A permit names another expected authority writer.
    #[error("bootstrap permit authority writer binding does not match")]
    AuthorityWriterMismatch,
    /// Strict authority signature verification failed.
    #[error("bootstrap permit authority signature is invalid")]
    InvalidAuthoritySignature,
    /// The separately pinned candidate key is weak.
    #[error("bootstrap candidate receipt key is weak")]
    WeakCandidateKey,
    /// The candidate key differs from the exact permit binding.
    #[error("bootstrap candidate receipt key does not match its permit")]
    CandidateKeyMismatch,
    /// Strict candidate signature verification failed.
    #[error("bootstrap candidate receipt signature is invalid")]
    InvalidCandidateSignature,
    /// The receipt names another signed permit.
    #[error("bootstrap receipt permit binding does not match")]
    PermitDigestMismatch,
    /// The receipt folder differs from the permit.
    #[error("bootstrap receipt folder differs from its permit")]
    ReceiptFolderMismatch,
    /// The receipt base epoch number differs from the permit.
    #[error("bootstrap receipt base epoch differs from its permit")]
    ReceiptBaseEpochMismatch,
    /// The receipt base epoch digest differs from the permit.
    #[error("bootstrap receipt base epoch digest differs from its permit")]
    ReceiptBaseDigestMismatch,
    /// The receipt writer differs from the permit candidate.
    #[error("bootstrap receipt writer differs from its permit")]
    ReceiptCandidateMismatch,
    /// The claimed applied frontier omits required causal history.
    #[error("bootstrap applied frontier does not dominate its required frontier")]
    InsufficientAppliedFrontier,
}

/// Encodes and authority-signs one bounded read-only bootstrap permit.
///
/// The nonce and authority key are supplied by the caller. This function does
/// no entropy, persistence, clock evaluation, or capability activation.
#[allow(clippy::too_many_arguments)]
pub fn encode_signed_bootstrap_permit(
    authority_signing_key: &SigningKey,
    folder_id: FolderId,
    authority_writer_id: WriterId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    candidate: &MemberGrant,
    nonce: [u8; DIGEST_BYTES],
    expires_at_unix_ms: u64,
    required_frontier: &[ClockEntry],
) -> Result<Vec<u8>, BootstrapCodecError> {
    validate_frontier_bound(required_frontier.len())?;
    let record_len = permit_record_len(required_frontier.len())?;
    if record_len > MAX_SIGNED_BOOTSTRAP_RECORD_BYTES {
        return Err(BootstrapCodecError::RecordTooLarge);
    }
    let authority_key = authority_signing_key.verifying_key();
    if authority_key.is_weak() {
        return Err(BootstrapCodecError::WeakAuthorityKey);
    }
    validate_permit_shape(
        authority_writer_id,
        &authority_key,
        base_epoch,
        candidate,
        expires_at_unix_ms,
        required_frontier,
    )?;

    let mut record = Vec::with_capacity(record_len);
    encode_permit_unsigned(
        &mut record,
        folder_id,
        authority_writer_id,
        base_epoch,
        base_epoch_digest,
        candidate,
        nonce,
        expires_at_unix_ms,
        required_frontier,
    )?;
    let signature =
        authority_signing_key.sign(&signature_message(PERMIT_SIGNATURE_DOMAIN, &record));
    record.extend_from_slice(&signature.to_bytes());
    Ok(record)
}

/// Parses canonical permit bytes and verifies the pinned authority signature.
///
/// The result is still subject to runtime current-head, expiry, single-use,
/// transport, transfer, and durable-apply checks.
pub fn decode_signature_checked_bootstrap_permit(
    record: &[u8],
    expected_folder: FolderId,
    expected_authority_writer: WriterId,
    pinned_authority_key: &VerifyingKey,
) -> Result<SignatureCheckedBootstrapPermit, BootstrapCodecError> {
    check_record_bound(record)?;
    if pinned_authority_key.is_weak() {
        return Err(BootstrapCodecError::WeakAuthorityKey);
    }
    let mut cursor = Cursor::new(record);
    read_prelude(&mut cursor, PERMIT_MAGIC)?;
    let folder_id = FolderId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let authority_writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let base_epoch = cursor.take_u64()?;
    let base_epoch_digest = EpochDigest::from_bytes(cursor.take_array::<DIGEST_BYTES>()?);
    let candidate = decode_member_grant(&mut cursor)?;
    let nonce = cursor.take_array::<DIGEST_BYTES>()?;
    let expires_at_unix_ms = cursor.take_u64()?;
    let required_frontier_entries = decode_frontier(&mut cursor)?;
    let unsigned_len = cursor.position();
    let signature = Signature::from_bytes(&cursor.take_array::<SIGNATURE_BYTES>()?);
    if cursor.remaining() != 0 {
        return Err(BootstrapCodecError::TrailingBytes);
    }

    validate_permit_shape(
        authority_writer_id,
        pinned_authority_key,
        base_epoch,
        &candidate,
        expires_at_unix_ms,
        &required_frontier_entries,
    )?;
    pinned_authority_key
        .verify_strict(
            &signature_message(PERMIT_SIGNATURE_DOMAIN, &record[..unsigned_len]),
            &signature,
        )
        .map_err(|_| BootstrapCodecError::InvalidAuthoritySignature)?;
    if folder_id != expected_folder {
        return Err(BootstrapCodecError::FolderMismatch);
    }
    if authority_writer_id != expected_authority_writer {
        return Err(BootstrapCodecError::AuthorityWriterMismatch);
    }
    let required_frontier = frontier_vector(&required_frontier_entries)?;
    let canonical_record = record.to_vec();
    let digest = BootstrapPermitDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedBootstrapPermit {
        folder_id,
        authority_writer_id,
        base_epoch,
        base_epoch_digest,
        candidate,
        nonce,
        expires_at_unix_ms,
        required_frontier_entries,
        required_frontier,
        canonical_record,
        digest,
    })
}

/// Encodes and candidate-signs a receipt bound to one exact verified permit.
///
/// Production code must call this only from the private durable capability that
/// proves full history/content transfer and staged root application completed.
pub fn encode_signed_bootstrap_receipt(
    candidate_signing_key: &SigningKey,
    permit: &SignatureCheckedBootstrapPermit,
    applied_frontier: &[ClockEntry],
    root_state_commitment: [u8; DIGEST_BYTES],
) -> Result<Vec<u8>, BootstrapCodecError> {
    validate_frontier_bound(applied_frontier.len())?;
    let record_len = receipt_record_len(applied_frontier.len())?;
    if record_len > MAX_SIGNED_BOOTSTRAP_RECORD_BYTES {
        return Err(BootstrapCodecError::RecordTooLarge);
    }
    let candidate_key = candidate_signing_key.verifying_key();
    validate_candidate_key(permit, &candidate_key)?;
    validate_frontier(applied_frontier)?;
    let applied = frontier_vector(applied_frontier)?;
    require_frontier_dominance(&applied, permit.required_frontier())?;

    let mut record = Vec::with_capacity(record_len);
    encode_receipt_unsigned(&mut record, permit, applied_frontier, root_state_commitment);
    let signature =
        candidate_signing_key.sign(&signature_message(RECEIPT_SIGNATURE_DOMAIN, &record));
    record.extend_from_slice(&signature.to_bytes());
    Ok(record)
}

/// Parses a receipt, verifies its pinned candidate key, and binds it to the
/// exact verified permit.
///
/// Signature and frontier dominance do not prove physical application.
pub fn decode_signature_checked_bootstrap_receipt(
    record: &[u8],
    permit: &SignatureCheckedBootstrapPermit,
    pinned_candidate_key: &VerifyingKey,
) -> Result<SignatureCheckedBootstrapReceipt, BootstrapCodecError> {
    check_record_bound(record)?;
    validate_candidate_key(permit, pinned_candidate_key)?;
    let mut cursor = Cursor::new(record);
    read_prelude(&mut cursor, RECEIPT_MAGIC)?;
    let permit_digest = BootstrapPermitDigest(cursor.take_array::<DIGEST_BYTES>()?);
    let folder_id = FolderId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let base_epoch = cursor.take_u64()?;
    let base_epoch_digest = EpochDigest::from_bytes(cursor.take_array::<DIGEST_BYTES>()?);
    let candidate_writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let applied_frontier_entries = decode_frontier(&mut cursor)?;
    let root_state_commitment = cursor.take_array::<DIGEST_BYTES>()?;
    let unsigned_len = cursor.position();
    let signature = Signature::from_bytes(&cursor.take_array::<SIGNATURE_BYTES>()?);
    if cursor.remaining() != 0 {
        return Err(BootstrapCodecError::TrailingBytes);
    }

    validate_frontier(&applied_frontier_entries)?;
    pinned_candidate_key
        .verify_strict(
            &signature_message(RECEIPT_SIGNATURE_DOMAIN, &record[..unsigned_len]),
            &signature,
        )
        .map_err(|_| BootstrapCodecError::InvalidCandidateSignature)?;
    if permit_digest != permit.digest() {
        return Err(BootstrapCodecError::PermitDigestMismatch);
    }
    if folder_id != permit.folder_id() {
        return Err(BootstrapCodecError::ReceiptFolderMismatch);
    }
    if base_epoch != permit.base_epoch() {
        return Err(BootstrapCodecError::ReceiptBaseEpochMismatch);
    }
    if base_epoch_digest != permit.base_epoch_digest() {
        return Err(BootstrapCodecError::ReceiptBaseDigestMismatch);
    }
    if candidate_writer_id != permit.candidate().writer_id() {
        return Err(BootstrapCodecError::ReceiptCandidateMismatch);
    }
    let applied_frontier = frontier_vector(&applied_frontier_entries)?;
    require_frontier_dominance(&applied_frontier, permit.required_frontier())?;
    let canonical_record = record.to_vec();
    let digest = BootstrapReceiptDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedBootstrapReceipt {
        permit_digest,
        folder_id,
        base_epoch,
        base_epoch_digest,
        candidate_writer_id,
        applied_frontier_entries,
        applied_frontier,
        root_state_commitment,
        canonical_record,
        digest,
    })
}

fn check_record_bound(record: &[u8]) -> Result<(), BootstrapCodecError> {
    if record.len() > MAX_SIGNED_BOOTSTRAP_RECORD_BYTES {
        return Err(BootstrapCodecError::RecordTooLarge);
    }
    Ok(())
}

fn validate_permit_shape(
    authority_writer_id: WriterId,
    authority_key: &VerifyingKey,
    base_epoch: u64,
    candidate: &MemberGrant,
    expires_at_unix_ms: u64,
    required_frontier: &[ClockEntry],
) -> Result<(), BootstrapCodecError> {
    if base_epoch == 0 {
        return Err(BootstrapCodecError::ZeroBaseEpoch);
    }
    if expires_at_unix_ms == 0 {
        return Err(BootstrapCodecError::ZeroExpiry);
    }
    if candidate.writer_id() == authority_writer_id {
        return Err(BootstrapCodecError::CandidateWriterIsAuthority);
    }
    if candidate.writer_key().to_bytes() == authority_key.to_bytes()
        || candidate.transport_key().to_bytes() == authority_key.to_bytes()
    {
        return Err(BootstrapCodecError::CandidateKeyReusesAuthority);
    }
    validate_frontier(required_frontier)
}

fn validate_candidate_key(
    permit: &SignatureCheckedBootstrapPermit,
    candidate_key: &VerifyingKey,
) -> Result<(), BootstrapCodecError> {
    if candidate_key.is_weak() {
        return Err(BootstrapCodecError::WeakCandidateKey);
    }
    if *candidate_key != *permit.candidate().writer_key() {
        return Err(BootstrapCodecError::CandidateKeyMismatch);
    }
    Ok(())
}

fn validate_frontier_bound(count: usize) -> Result<(), BootstrapCodecError> {
    if count > MAX_VERSION_VECTOR_ACTORS {
        return Err(BootstrapCodecError::TooManyActors);
    }
    Ok(())
}

fn validate_frontier(frontier: &[ClockEntry]) -> Result<(), BootstrapCodecError> {
    validate_frontier_bound(frontier.len())?;
    let mut previous: Option<[u8; 16]> = None;
    for entry in frontier {
        if entry.counter() == 0 {
            return Err(BootstrapCodecError::ZeroFrontierCounter);
        }
        let writer = entry.writer_id().to_bytes();
        if previous.is_some_and(|prior| prior >= writer) {
            return Err(BootstrapCodecError::NonCanonicalFrontier);
        }
        previous = Some(writer);
    }
    Ok(())
}

fn frontier_vector(frontier: &[ClockEntry]) -> Result<VersionVector, BootstrapCodecError> {
    VersionVector::new(
        frontier
            .iter()
            .map(|entry| (entry.writer_id().into_vector_actor(), entry.counter())),
    )
    .map_err(|_| BootstrapCodecError::NonCanonicalFrontier)
}

fn require_frontier_dominance(
    applied: &VersionVector,
    required: &VersionVector,
) -> Result<(), BootstrapCodecError> {
    match applied.compare(required) {
        VersionVectorOrder::Equal | VersionVectorOrder::After => Ok(()),
        VersionVectorOrder::Before | VersionVectorOrder::Concurrent => {
            Err(BootstrapCodecError::InsufficientAppliedFrontier)
        }
    }
}

fn permit_record_len(frontier_count: usize) -> Result<usize, BootstrapCodecError> {
    FIXED_PERMIT_UNSIGNED_BYTES
        .checked_add(
            frontier_count
                .checked_mul(FRONTIER_ENTRY_BYTES)
                .ok_or(BootstrapCodecError::LengthOverflow)?,
        )
        .and_then(|length| length.checked_add(SIGNATURE_BYTES))
        .ok_or(BootstrapCodecError::LengthOverflow)
}

fn receipt_record_len(frontier_count: usize) -> Result<usize, BootstrapCodecError> {
    FIXED_RECEIPT_UNSIGNED_BYTES
        .checked_add(
            frontier_count
                .checked_mul(FRONTIER_ENTRY_BYTES)
                .ok_or(BootstrapCodecError::LengthOverflow)?,
        )
        .and_then(|length| length.checked_add(SIGNATURE_BYTES))
        .ok_or(BootstrapCodecError::LengthOverflow)
}

#[allow(clippy::too_many_arguments)]
fn encode_permit_unsigned(
    output: &mut Vec<u8>,
    folder_id: FolderId,
    authority_writer_id: WriterId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    candidate: &MemberGrant,
    nonce: [u8; DIGEST_BYTES],
    expires_at_unix_ms: u64,
    required_frontier: &[ClockEntry],
) -> Result<(), BootstrapCodecError> {
    output.extend_from_slice(PERMIT_MAGIC);
    output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
    output.push(KNOWN_FLAGS);
    output.extend_from_slice(&folder_id.to_bytes());
    output.extend_from_slice(&authority_writer_id.to_bytes());
    output.extend_from_slice(&base_epoch.to_be_bytes());
    output.extend_from_slice(&base_epoch_digest.to_bytes());
    encode_member_grant(output, candidate)?;
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&expires_at_unix_ms.to_be_bytes());
    encode_frontier(output, required_frontier);
    Ok(())
}

fn encode_receipt_unsigned(
    output: &mut Vec<u8>,
    permit: &SignatureCheckedBootstrapPermit,
    applied_frontier: &[ClockEntry],
    root_state_commitment: [u8; DIGEST_BYTES],
) {
    output.extend_from_slice(RECEIPT_MAGIC);
    output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
    output.push(KNOWN_FLAGS);
    output.extend_from_slice(&permit.digest().to_bytes());
    output.extend_from_slice(&permit.folder_id().to_bytes());
    output.extend_from_slice(&permit.base_epoch().to_be_bytes());
    output.extend_from_slice(&permit.base_epoch_digest().to_bytes());
    output.extend_from_slice(&permit.candidate().writer_id().to_bytes());
    encode_frontier(output, applied_frontier);
    output.extend_from_slice(&root_state_commitment);
}

fn encode_member_grant(
    output: &mut Vec<u8>,
    candidate: &MemberGrant,
) -> Result<(), BootstrapCodecError> {
    output.extend_from_slice(&candidate.writer_id().to_bytes());
    output.extend_from_slice(&candidate.writer_key().to_bytes());
    output.extend_from_slice(&transport_device_id_bytes(candidate.transport_device_id())?);
    output.extend_from_slice(&candidate.transport_key().to_bytes());
    output.push(candidate.role() as u8);
    Ok(())
}

fn decode_member_grant(cursor: &mut Cursor<'_>) -> Result<MemberGrant, BootstrapCodecError> {
    let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let writer_key_bytes = cursor.take_array::<32>()?;
    let writer_key = VerifyingKey::from_bytes(&writer_key_bytes)
        .map_err(|_| BootstrapCodecError::InvalidWriterKey)?;
    if writer_key.is_weak() {
        return Err(BootstrapCodecError::WeakWriterKey);
    }
    let transport_device_id = DeviceId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let transport_key_bytes = cursor.take_array::<32>()?;
    let transport_key = VerifyingKey::from_bytes(&transport_key_bytes)
        .map_err(|_| BootstrapCodecError::InvalidTransportKey)?;
    if transport_key.is_weak() {
        return Err(BootstrapCodecError::WeakTransportKey);
    }
    let role = match cursor.take_u8()? {
        1 => MemberRole::Read,
        2 => MemberRole::ReadWrite,
        _ => return Err(BootstrapCodecError::UnknownRole),
    };
    MemberGrant::new(
        writer_id,
        writer_key,
        transport_device_id,
        transport_key,
        role,
    )
    .map_err(|error| match error {
        super::membership::MembershipCodecError::WeakWriterKey => {
            BootstrapCodecError::WeakWriterKey
        }
        super::membership::MembershipCodecError::WeakTransportKey => {
            BootstrapCodecError::WeakTransportKey
        }
        _ => BootstrapCodecError::ReusedCandidateKey,
    })
}

fn encode_frontier(output: &mut Vec<u8>, frontier: &[ClockEntry]) {
    output.extend_from_slice(&(frontier.len() as u16).to_be_bytes());
    for entry in frontier {
        output.extend_from_slice(&entry.writer_id().to_bytes());
        output.extend_from_slice(&entry.counter().to_be_bytes());
    }
}

fn decode_frontier(cursor: &mut Cursor<'_>) -> Result<Vec<ClockEntry>, BootstrapCodecError> {
    let count = usize::from(cursor.take_u16()?);
    validate_frontier_bound(count)?;
    let mut frontier = Vec::with_capacity(count);
    for _ in 0..count {
        let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let counter = cursor.take_u64()?;
        frontier.push(
            ClockEntry::new(writer_id, counter)
                .map_err(|_| BootstrapCodecError::ZeroFrontierCounter)?,
        );
    }
    Ok(frontier)
}

fn transport_device_id_bytes(device_id: DeviceId) -> Result<[u8; 16], BootstrapCodecError> {
    Uuid::parse_str(&device_id.to_string())
        .map(Uuid::into_bytes)
        .map_err(|_| BootstrapCodecError::InvalidTransportDeviceId)
}

fn read_prelude(
    cursor: &mut Cursor<'_>,
    expected_magic: &[u8; 8],
) -> Result<(), BootstrapCodecError> {
    if cursor.take_array::<8>()? != *expected_magic {
        return Err(BootstrapCodecError::InvalidMagic);
    }
    if cursor.take_u16()? != WIRE_VERSION {
        return Err(BootstrapCodecError::UnsupportedVersion);
    }
    if cursor.take_u8()? != KNOWN_FLAGS {
        return Err(BootstrapCodecError::UnknownFlags);
    }
    Ok(())
}

fn signature_message(domain: &[u8], unsigned_record: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + unsigned_record.len());
    message.extend_from_slice(domain);
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

    fn take(&mut self, length: usize) -> Result<&'a [u8], BootstrapCodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(BootstrapCodecError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(BootstrapCodecError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], BootstrapCodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| BootstrapCodecError::Truncated)
    }

    fn take_u8(&mut self) -> Result<u8, BootstrapCodecError> {
        Ok(self.take_array::<1>()?[0])
    }

    fn take_u16(&mut self) -> Result<u16, BootstrapCodecError> {
        Ok(u16::from_be_bytes(self.take_array::<2>()?))
    }

    fn take_u64(&mut self) -> Result<u64, BootstrapCodecError> {
        Ok(u64::from_be_bytes(self.take_array::<8>()?))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
    use uuid::Uuid;

    use super::*;

    const FLAGS_OFFSET: usize = 10;
    const PERMIT_FOLDER_OFFSET: usize = 11;
    const PERMIT_AUTHORITY_OFFSET: usize = 27;
    const PERMIT_BASE_EPOCH_OFFSET: usize = 43;
    const PERMIT_BASE_DIGEST_OFFSET: usize = 51;
    const PERMIT_CANDIDATE_OFFSET: usize = 83;
    const PERMIT_CANDIDATE_WRITER_KEY_OFFSET: usize = PERMIT_CANDIDATE_OFFSET + 16;
    const PERMIT_CANDIDATE_TRANSPORT_ID_OFFSET: usize = PERMIT_CANDIDATE_OFFSET + 48;
    const PERMIT_CANDIDATE_TRANSPORT_KEY_OFFSET: usize = PERMIT_CANDIDATE_OFFSET + 64;
    const PERMIT_CANDIDATE_ROLE_OFFSET: usize = PERMIT_CANDIDATE_OFFSET + 96;
    const PERMIT_NONCE_OFFSET: usize = PERMIT_CANDIDATE_OFFSET + MEMBER_GRANT_BYTES;
    const PERMIT_EXPIRY_OFFSET: usize = PERMIT_NONCE_OFFSET + 32;
    const PERMIT_FRONTIER_COUNT_OFFSET: usize = PERMIT_EXPIRY_OFFSET + 8;
    const PERMIT_FRONTIER_OFFSET: usize = PERMIT_FRONTIER_COUNT_OFFSET + 2;

    const RECEIPT_PERMIT_DIGEST_OFFSET: usize = 11;
    const RECEIPT_FOLDER_OFFSET: usize = 43;
    const RECEIPT_BASE_EPOCH_OFFSET: usize = 59;
    const RECEIPT_BASE_DIGEST_OFFSET: usize = 67;
    const RECEIPT_CANDIDATE_OFFSET: usize = 99;
    const RECEIPT_FRONTIER_COUNT_OFFSET: usize = 115;
    const RECEIPT_FRONTIER_OFFSET: usize = 117;

    fn folder(number: u128) -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(number))
    }

    fn writer(number: u128) -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(number))
    }

    fn device(number: u128) -> DeviceId {
        DeviceId::from_uuid(Uuid::from_u128(number))
    }

    fn key(number: u64) -> SigningKey {
        let mut bytes = [0x31; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        SigningKey::from_bytes(&bytes)
    }

    fn candidate(role: MemberRole) -> MemberGrant {
        MemberGrant::new(
            writer(2),
            key(3).verifying_key(),
            device(102),
            key(4).verifying_key(),
            role,
        )
        .expect("candidate")
    }

    fn required_frontier() -> Vec<ClockEntry> {
        vec![
            ClockEntry::new(writer(1), 5).expect("frontier"),
            ClockEntry::new(writer(3), 2).expect("frontier"),
        ]
    }

    fn permit_record(nonce: [u8; 32], expiry: u64) -> Vec<u8> {
        encode_signed_bootstrap_permit(
            &key(1),
            folder(7),
            writer(1),
            4,
            EpochDigest::from_bytes([8; 32]),
            &candidate(MemberRole::ReadWrite),
            nonce,
            expiry,
            &required_frontier(),
        )
        .expect("permit")
    }

    fn checked_permit() -> SignatureCheckedBootstrapPermit {
        decode_permit(&permit_record([9; 32], 1_000)).expect("checked permit")
    }

    fn decode_permit(
        record: &[u8],
    ) -> Result<SignatureCheckedBootstrapPermit, BootstrapCodecError> {
        decode_signature_checked_bootstrap_permit(
            record,
            folder(7),
            writer(1),
            &key(1).verifying_key(),
        )
    }

    fn receipt_record(
        permit: &SignatureCheckedBootstrapPermit,
        frontier: &[ClockEntry],
    ) -> Vec<u8> {
        encode_signed_bootstrap_receipt(&key(3), permit, frontier, [6; 32]).expect("receipt")
    }

    fn decode_receipt(
        record: &[u8],
        permit: &SignatureCheckedBootstrapPermit,
    ) -> Result<SignatureCheckedBootstrapReceipt, BootstrapCodecError> {
        decode_signature_checked_bootstrap_receipt(record, permit, &key(3).verifying_key())
    }

    fn unsigned(record: &[u8]) -> Vec<u8> {
        record[..record.len() - SIGNATURE_BYTES].to_vec()
    }

    fn resign(mut record: Vec<u8>, signing_key: &SigningKey, domain: &[u8]) -> Vec<u8> {
        let signature = signing_key.sign(&signature_message(domain, &record));
        record.extend_from_slice(&signature.to_bytes());
        record
    }

    #[test]
    fn permit_round_trip_commits_exact_bindings_without_granting_final_role() {
        let record = permit_record([9; 32], 1);
        let checked = decode_permit(&record).expect("checked permit");

        assert_eq!(checked.canonical_record(), record);
        assert_eq!(checked.folder_id(), folder(7));
        assert_eq!(checked.authority_writer_id(), writer(1));
        assert_eq!(checked.base_epoch(), 4);
        assert_eq!(checked.base_epoch_digest().to_bytes(), [8; 32]);
        assert_eq!(checked.candidate().writer_id(), writer(2));
        assert_eq!(checked.candidate().role(), MemberRole::ReadWrite);
        assert_eq!(checked.nonce(), [9; 32]);
        assert_eq!(checked.expires_at_unix_ms(), 1);
        assert_eq!(checked.required_frontier_entries(), required_frontier());
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        let debug = format!("{checked:?}");
        assert!(!debug.contains("nonce"));
        assert!(!debug.contains("canonical_record"));
    }

    #[test]
    fn receipt_round_trip_commits_permit_applied_frontier_and_root_state() {
        let permit = checked_permit();
        let applied = [
            ClockEntry::new(writer(1), 6).expect("frontier"),
            ClockEntry::new(writer(3), 2).expect("frontier"),
            ClockEntry::new(writer(4), 1).expect("frontier"),
        ];
        let record = receipt_record(&permit, &applied);
        let checked = decode_receipt(&record, &permit).expect("checked receipt");

        assert_eq!(checked.canonical_record(), record);
        assert_eq!(checked.permit_digest(), permit.digest());
        assert_eq!(checked.folder_id(), folder(7));
        assert_eq!(checked.base_epoch(), 4);
        assert_eq!(checked.base_epoch_digest().to_bytes(), [8; 32]);
        assert_eq!(checked.candidate_writer_id(), writer(2));
        assert_eq!(checked.applied_frontier_entries(), applied);
        assert_eq!(checked.root_state_commitment(), [6; 32]);
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        assert!(!format!("{checked:?}").contains("root_state_commitment"));
    }

    #[test]
    fn permit_candidate_is_distinct_from_authority_identity_and_key() {
        let authority_writer_candidate = MemberGrant::new(
            writer(1),
            key(3).verifying_key(),
            device(102),
            key(4).verifying_key(),
            MemberRole::Read,
        )
        .expect("candidate");
        let encode = |candidate: &MemberGrant| {
            encode_signed_bootstrap_permit(
                &key(1),
                folder(7),
                writer(1),
                4,
                EpochDigest::from_bytes([8; 32]),
                candidate,
                [9; 32],
                1,
                &[],
            )
        };
        assert_eq!(
            encode(&authority_writer_candidate),
            Err(BootstrapCodecError::CandidateWriterIsAuthority)
        );
        let authority_writer_key_candidate = MemberGrant::new(
            writer(2),
            key(1).verifying_key(),
            device(102),
            key(4).verifying_key(),
            MemberRole::Read,
        )
        .expect("candidate");
        assert_eq!(
            encode(&authority_writer_key_candidate),
            Err(BootstrapCodecError::CandidateKeyReusesAuthority)
        );
        let authority_transport_key_candidate = MemberGrant::new(
            writer(2),
            key(3).verifying_key(),
            device(102),
            key(1).verifying_key(),
            MemberRole::Read,
        )
        .expect("candidate");
        assert_eq!(
            encode(&authority_transport_key_candidate),
            Err(BootstrapCodecError::CandidateKeyReusesAuthority)
        );
    }

    #[test]
    fn permit_expected_folder_authority_and_key_are_enforced() {
        let record = permit_record([9; 32], 1_000);
        let authority_key = key(1).verifying_key();
        assert_eq!(
            decode_signature_checked_bootstrap_permit(
                &record,
                folder(8),
                writer(1),
                &authority_key
            ),
            Err(BootstrapCodecError::FolderMismatch)
        );
        assert_eq!(
            decode_signature_checked_bootstrap_permit(
                &record,
                folder(7),
                writer(8),
                &authority_key
            ),
            Err(BootstrapCodecError::AuthorityWriterMismatch)
        );
        assert_eq!(
            decode_signature_checked_bootstrap_permit(
                &record,
                folder(7),
                writer(1),
                &key(8).verifying_key()
            ),
            Err(BootstrapCodecError::InvalidAuthoritySignature)
        );
    }

    #[test]
    fn permit_requires_positive_base_and_expiry_without_reading_a_clock() {
        let candidate = candidate(MemberRole::Read);
        let encode = |base_epoch, expiry| {
            encode_signed_bootstrap_permit(
                &key(1),
                folder(7),
                writer(1),
                base_epoch,
                EpochDigest::from_bytes([8; 32]),
                &candidate,
                [9; 32],
                expiry,
                &[],
            )
        };
        assert_eq!(encode(0, 1), Err(BootstrapCodecError::ZeroBaseEpoch));
        assert_eq!(encode(1, 0), Err(BootstrapCodecError::ZeroExpiry));
        let historical_positive_expiry = encode(1, 1).expect("shape-valid permit");
        assert!(decode_permit(&historical_positive_expiry).is_ok());
    }

    #[test]
    fn permit_frontier_is_sorted_unique_positive_and_bounded() {
        let encode = |frontier: &[ClockEntry]| {
            encode_signed_bootstrap_permit(
                &key(1),
                folder(7),
                writer(1),
                4,
                EpochDigest::from_bytes([8; 32]),
                &candidate(MemberRole::Read),
                [9; 32],
                1,
                frontier,
            )
        };
        assert_eq!(
            encode(&[
                ClockEntry::new(writer(3), 1).expect("frontier"),
                ClockEntry::new(writer(1), 1).expect("frontier")
            ]),
            Err(BootstrapCodecError::NonCanonicalFrontier)
        );
        assert_eq!(
            encode(&[
                ClockEntry::new(writer(1), 1).expect("frontier"),
                ClockEntry::new(writer(1), 2).expect("frontier")
            ]),
            Err(BootstrapCodecError::NonCanonicalFrontier)
        );
        let too_many = (1..=129_u128)
            .map(|number| ClockEntry::new(writer(number), 1).expect("frontier"))
            .collect::<Vec<_>>();
        assert_eq!(encode(&too_many), Err(BootstrapCodecError::TooManyActors));

        let record = permit_record([9; 32], 1_000);
        let mut zero = unsigned(&record);
        zero[PERMIT_FRONTIER_OFFSET + 16..PERMIT_FRONTIER_OFFSET + 24].fill(0);
        assert_eq!(
            decode_permit(&resign(zero, &key(1), PERMIT_SIGNATURE_DOMAIN)),
            Err(BootstrapCodecError::ZeroFrontierCounter)
        );
    }

    #[test]
    fn receipt_rejects_frontiers_that_omit_required_history() {
        let permit = checked_permit();
        let before = [
            ClockEntry::new(writer(1), 4).expect("frontier"),
            ClockEntry::new(writer(3), 2).expect("frontier"),
        ];
        assert_eq!(
            encode_signed_bootstrap_receipt(&key(3), &permit, &before, [6; 32]),
            Err(BootstrapCodecError::InsufficientAppliedFrontier)
        );
        let concurrent = [
            ClockEntry::new(writer(1), 6).expect("frontier"),
            ClockEntry::new(writer(3), 1).expect("frontier"),
        ];
        assert_eq!(
            encode_signed_bootstrap_receipt(&key(3), &permit, &concurrent, [6; 32]),
            Err(BootstrapCodecError::InsufficientAppliedFrontier)
        );

        let valid = receipt_record(&permit, &required_frontier());
        let mut insufficient = unsigned(&valid);
        insufficient[RECEIPT_FRONTIER_OFFSET + 16..RECEIPT_FRONTIER_OFFSET + 24]
            .copy_from_slice(&4_u64.to_be_bytes());
        let insufficient = resign(insufficient, &key(3), RECEIPT_SIGNATURE_DOMAIN);
        assert_eq!(
            decode_receipt(&insufficient, &permit),
            Err(BootstrapCodecError::InsufficientAppliedFrontier)
        );
    }

    #[test]
    fn receipt_is_bound_to_exact_permit_candidate_and_base_head() {
        let permit = checked_permit();
        let receipt = receipt_record(&permit, &required_frontier());
        let other_permit_record = permit_record([10; 32], 1_000);
        let other_permit = decode_permit(&other_permit_record).expect("other permit");
        assert_eq!(
            decode_receipt(&receipt, &other_permit),
            Err(BootstrapCodecError::PermitDigestMismatch)
        );
        assert_eq!(
            decode_signature_checked_bootstrap_receipt(&receipt, &permit, &key(4).verifying_key()),
            Err(BootstrapCodecError::CandidateKeyMismatch)
        );

        for (offset, expected) in [
            (
                RECEIPT_PERMIT_DIGEST_OFFSET,
                BootstrapCodecError::PermitDigestMismatch,
            ),
            (
                RECEIPT_FOLDER_OFFSET,
                BootstrapCodecError::ReceiptFolderMismatch,
            ),
            (
                RECEIPT_BASE_EPOCH_OFFSET,
                BootstrapCodecError::ReceiptBaseEpochMismatch,
            ),
            (
                RECEIPT_BASE_DIGEST_OFFSET,
                BootstrapCodecError::ReceiptBaseDigestMismatch,
            ),
            (
                RECEIPT_CANDIDATE_OFFSET,
                BootstrapCodecError::ReceiptCandidateMismatch,
            ),
        ] {
            let mut changed = unsigned(&receipt);
            changed[offset] ^= 1;
            let changed = resign(changed, &key(3), RECEIPT_SIGNATURE_DOMAIN);
            assert_eq!(decode_receipt(&changed, &permit), Err(expected));
        }
    }

    #[test]
    fn every_permit_field_and_signature_is_committed() {
        let record = permit_record([9; 32], 1_000);
        let offsets = [
            0,
            8,
            FLAGS_OFFSET,
            PERMIT_FOLDER_OFFSET,
            PERMIT_AUTHORITY_OFFSET,
            PERMIT_BASE_EPOCH_OFFSET,
            PERMIT_BASE_DIGEST_OFFSET,
            PERMIT_CANDIDATE_OFFSET,
            PERMIT_CANDIDATE_WRITER_KEY_OFFSET,
            PERMIT_CANDIDATE_TRANSPORT_ID_OFFSET,
            PERMIT_CANDIDATE_TRANSPORT_KEY_OFFSET,
            PERMIT_CANDIDATE_ROLE_OFFSET,
            PERMIT_NONCE_OFFSET,
            PERMIT_EXPIRY_OFFSET,
            PERMIT_FRONTIER_COUNT_OFFSET + 1,
            PERMIT_FRONTIER_OFFSET,
            PERMIT_FRONTIER_OFFSET + 16,
            record.len() - 1,
        ];
        for offset in offsets {
            let mut tampered = record.clone();
            tampered[offset] ^= 0x80;
            assert!(
                decode_permit(&tampered).is_err(),
                "permit tamper at {offset} was accepted"
            );
        }
    }

    #[test]
    fn every_receipt_field_and_signature_is_committed() {
        let permit = checked_permit();
        let record = receipt_record(&permit, &required_frontier());
        let root_offset = RECEIPT_FRONTIER_OFFSET + (2 * FRONTIER_ENTRY_BYTES);
        let offsets = [
            0,
            8,
            FLAGS_OFFSET,
            RECEIPT_PERMIT_DIGEST_OFFSET,
            RECEIPT_FOLDER_OFFSET,
            RECEIPT_BASE_EPOCH_OFFSET,
            RECEIPT_BASE_DIGEST_OFFSET,
            RECEIPT_CANDIDATE_OFFSET,
            RECEIPT_FRONTIER_COUNT_OFFSET + 1,
            RECEIPT_FRONTIER_OFFSET,
            RECEIPT_FRONTIER_OFFSET + 16,
            root_offset,
            record.len() - 1,
        ];
        for offset in offsets {
            let mut tampered = record.clone();
            tampered[offset] ^= 0x80;
            assert!(
                decode_receipt(&tampered, &permit).is_err(),
                "receipt tamper at {offset} was accepted"
            );
        }
    }

    #[test]
    fn maximum_frontiers_fit_their_defensible_four_kib_caps() {
        let frontier = (1..=128_u128)
            .map(|number| ClockEntry::new(writer(number + 10), 1).expect("frontier"))
            .collect::<Vec<_>>();
        let permit_record = encode_signed_bootstrap_permit(
            &key(1),
            folder(7),
            writer(1),
            4,
            EpochDigest::from_bytes([8; 32]),
            &candidate(MemberRole::Read),
            [9; 32],
            1,
            &frontier,
        )
        .expect("max permit");
        assert_eq!(permit_record.len(), 3_358);
        assert!(permit_record.len() < MAX_SIGNED_BOOTSTRAP_RECORD_BYTES);
        let permit = decode_permit(&permit_record).expect("max permit checks");
        let receipt_record = receipt_record(&permit, &frontier);
        assert_eq!(receipt_record.len(), 3_285);
        assert!(receipt_record.len() < MAX_SIGNED_BOOTSTRAP_RECORD_BYTES);
        assert_eq!(
            decode_receipt(&receipt_record, &permit)
                .expect("max receipt checks")
                .applied_frontier_entries()
                .len(),
            128
        );
    }

    #[test]
    fn malformed_versions_flags_keys_bounds_and_framing_fail_closed() {
        let permit = permit_record([9; 32], 1_000);
        for end in 0..permit.len() {
            assert!(
                decode_permit(&permit[..end]).is_err(),
                "permit prefix {end} was accepted"
            );
        }
        let mut trailing = permit.clone();
        trailing.push(0);
        assert_eq!(
            decode_permit(&trailing),
            Err(BootstrapCodecError::TrailingBytes)
        );
        assert_eq!(
            decode_permit(&vec![0; MAX_SIGNED_BOOTSTRAP_RECORD_BYTES + 1]),
            Err(BootstrapCodecError::RecordTooLarge)
        );
        let mut version = permit.clone();
        version[8..10].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            decode_permit(&version),
            Err(BootstrapCodecError::UnsupportedVersion)
        );
        let mut flags = permit.clone();
        flags[FLAGS_OFFSET] = 1;
        assert_eq!(
            decode_permit(&flags),
            Err(BootstrapCodecError::UnknownFlags)
        );
        let mut weak_key = unsigned(&permit);
        let mut identity = [0; 32];
        identity[0] = 1;
        weak_key[PERMIT_CANDIDATE_WRITER_KEY_OFFSET..PERMIT_CANDIDATE_WRITER_KEY_OFFSET + 32]
            .copy_from_slice(&identity);
        assert_eq!(
            decode_permit(&resign(weak_key, &key(1), PERMIT_SIGNATURE_DOMAIN)),
            Err(BootstrapCodecError::WeakWriterKey)
        );
        let weak = VerifyingKey::from_bytes(&identity).expect("identity point");
        assert_eq!(
            decode_signature_checked_bootstrap_permit(&permit, folder(7), writer(1), &weak),
            Err(BootstrapCodecError::WeakAuthorityKey)
        );

        let checked_permit = checked_permit();
        let receipt = receipt_record(&checked_permit, &required_frontier());
        for end in 0..receipt.len() {
            assert!(
                decode_receipt(&receipt[..end], &checked_permit).is_err(),
                "receipt prefix {end} was accepted"
            );
        }
        let mut receipt_trailing = receipt;
        receipt_trailing.push(0);
        assert_eq!(
            decode_receipt(&receipt_trailing, &checked_permit),
            Err(BootstrapCodecError::TrailingBytes)
        );
        assert_eq!(
            decode_receipt(
                &vec![0; MAX_SIGNED_BOOTSTRAP_RECORD_BYTES + 1],
                &checked_permit
            ),
            Err(BootstrapCodecError::RecordTooLarge)
        );
        assert_eq!(
            decode_signature_checked_bootstrap_receipt(&receipt_trailing, &checked_permit, &weak),
            Err(BootstrapCodecError::WeakCandidateKey)
        );
    }

    #[test]
    fn signature_domains_cannot_be_crossed() {
        let permit = permit_record([9; 32], 1_000);
        let permit_unsigned = unsigned(&permit);
        assert_eq!(
            decode_permit(&resign(permit_unsigned, &key(1), RECEIPT_SIGNATURE_DOMAIN)),
            Err(BootstrapCodecError::InvalidAuthoritySignature)
        );

        let checked = checked_permit();
        let receipt = receipt_record(&checked, &required_frontier());
        let receipt_unsigned = unsigned(&receipt);
        assert_eq!(
            decode_receipt(
                &resign(receipt_unsigned, &key(3), PERMIT_SIGNATURE_DOMAIN),
                &checked
            ),
            Err(BootstrapCodecError::InvalidCandidateSignature)
        );
    }

    #[test]
    fn adversarial_bounded_inputs_do_not_panic() {
        let authority_key = key(1).verifying_key();
        let permit = checked_permit();
        for length in (0..=MAX_SIGNED_BOOTSTRAP_RECORD_BYTES).step_by(67) {
            let sample = vec![(length as u8).wrapping_mul(19); length];
            let permit_result = std::panic::catch_unwind(|| {
                decode_signature_checked_bootstrap_permit(
                    &sample,
                    folder(7),
                    writer(1),
                    &authority_key,
                )
            });
            assert!(permit_result.is_ok(), "permit parser panicked at {length}");
            let receipt_result = std::panic::catch_unwind(|| {
                decode_signature_checked_bootstrap_receipt(
                    &sample,
                    &permit,
                    &key(3).verifying_key(),
                )
            });
            assert!(
                receipt_result.is_ok(),
                "receipt parser panicked at {length}"
            );
        }
    }
}
