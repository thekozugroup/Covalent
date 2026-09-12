//! Signed write-loss proposals, survivor freeze receipts, and authority aborts.
//!
//! These codecs authenticate bounded record shapes only. A checked proposal
//! cannot prove its survivor or losing-writer lists are exact: a future
//! validator must derive both from the prior accepted roster and its historical
//! roles. It must also reject any combined Add or key change.
//!
//! A checked freeze receipt does not prove that the signer durably paused the
//! losing writers, that its frontier is causally closed, or that it retained
//! the named operation tips. Production must create it only behind a durable
//! freeze capability and must collect one valid receipt from every exact
//! survivor before an epoch transition. A checked abort releases nothing by
//! itself; runtime may release a durable freeze only after verifying the same
//! pending proposal and current base head, then persisting the abort decision.
//!
//! All three version-1 formats use fixed-width, big-endian fields. Proposal
//! order is `COVSFP01`, version, zero flags, folder, base epoch and digest,
//! authority writer, transition nonce, canonical changes, canonical survivors,
//! canonical losing writers, and authority signature. Receipt order is
//! `COVSFR01`, version, zero flags, complete proposal digest, folder, base epoch
//! and digest, signer writer, canonical frontier, canonical exact losing tips,
//! and survivor signature. Abort order is `COVSFA01`, version, zero flags,
//! complete proposal digest, folder, base epoch and digest, authority writer,
//! and authority signature.

use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use thiserror::Error;
use uuid::Uuid;

use super::ids::{FolderId, WriterId};
use super::membership::{EpochDigest, MemberGrant, WriterCutoff};
use super::operation::ClockEntry;
use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector};

const PROPOSAL_MAGIC: &[u8; 8] = b"COVSFP01";
const RECEIPT_MAGIC: &[u8; 8] = b"COVSFR01";
const ABORT_MAGIC: &[u8; 8] = b"COVSFA01";
const WIRE_VERSION: u16 = 1;
const KNOWN_FLAGS: u8 = 0;
const DIGEST_BYTES: usize = 32;
const SIGNATURE_BYTES: usize = 64;
const CHANGE_BYTES: usize = 16 + 1;
const FRONTIER_ENTRY_BYTES: usize = 16 + 8;
const CUTOFF_ENTRY_BYTES: usize = 16 + 8 + 32;
const FIXED_PROPOSAL_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 16 + 8 + 32 + 16 + 32 + 2 + 2 + 2;
const FIXED_RECEIPT_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 32 + 16 + 8 + 32 + 16 + 2 + 2;
const FIXED_ABORT_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 32 + 16 + 8 + 32 + 16;

/// Domain prefix for authority-signed write-loss proposals.
pub const PROPOSAL_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-write-loss-proposal-signature/v1\0";
/// Domain prefix for survivor-signed freeze receipts.
pub const FREEZE_RECEIPT_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-freeze-receipt-signature/v1\0";
/// Domain prefix for authority-signed freeze aborts.
pub const FREEZE_ABORT_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-freeze-abort-signature/v1\0";
/// Maximum complete proposal, receipt, or abort bytes before parsing.
pub const MAX_SIGNED_FREEZE_RECORD_BYTES: usize = 16 * 1_024;
/// Maximum entries in each proposal or receipt collection.
pub const MAX_FREEZE_COLLECTION_ENTRIES: usize = MAX_VERSION_VECTOR_ACTORS;

/// A write-losing membership action. This codec defines no Add or key change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum WriteLossAction {
    /// Remove the writer from the next roster.
    Remove = 1,
    /// Retain the writer with read-only access.
    DowngradeToRead = 2,
}

impl WriteLossAction {
    fn from_byte(value: u8) -> Result<Self, FreezeCodecError> {
        match value {
            1 => Ok(Self::Remove),
            2 => Ok(Self::DowngradeToRead),
            _ => Err(FreezeCodecError::UnknownChangeAction),
        }
    }
}

/// One canonical proposed membership change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriteLossChange {
    writer_id: WriterId,
    action: WriteLossAction,
}

impl WriteLossChange {
    /// Creates one removal or downgrade description.
    #[must_use]
    pub const fn new(writer_id: WriterId, action: WriteLossAction) -> Self {
        Self { writer_id, action }
    }

    /// Returns the affected writer.
    #[must_use]
    pub const fn writer_id(self) -> WriterId {
        self.writer_id
    }

    /// Returns the requested membership change.
    #[must_use]
    pub const fn action(self) -> WriteLossAction {
        self.action
    }
}

/// BLAKE3 commitment to a complete canonical signed proposal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WriteLossProposalDigest([u8; DIGEST_BYTES]);

impl WriteLossProposalDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// BLAKE3 commitment to a complete canonical signed freeze receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FreezeReceiptDigest([u8; DIGEST_BYTES]);

impl FreezeReceiptDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// BLAKE3 commitment to a complete canonical signed abort.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FreezeAbortDigest([u8; DIGEST_BYTES]);

impl FreezeAbortDigest {
    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// Authority-authenticated proposal shape, not an accepted transition.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedWriteLossProposal {
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    authority_writer_id: WriterId,
    authority_key: VerifyingKey,
    transition_nonce: [u8; DIGEST_BYTES],
    changes: Vec<WriteLossChange>,
    survivor_writer_ids: Vec<WriterId>,
    losing_writer_ids: Vec<WriterId>,
    canonical_record: Vec<u8>,
    digest: WriteLossProposalDigest,
}

impl fmt::Debug for SignatureCheckedWriteLossProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedWriteLossProposal")
            .field("folder_id", &self.folder_id)
            .field("base_epoch", &self.base_epoch)
            .field("authority_writer_id", &self.authority_writer_id)
            .field("change_count", &self.changes.len())
            .field("survivor_count", &self.survivor_writer_ids.len())
            .field("losing_writer_count", &self.losing_writer_ids.len())
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedWriteLossProposal {
    /// Returns the affected folder.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the positive base membership epoch.
    #[must_use]
    pub const fn base_epoch(&self) -> u64 {
        self.base_epoch
    }

    /// Returns the exact base epoch record commitment.
    #[must_use]
    pub const fn base_epoch_digest(&self) -> EpochDigest {
        self.base_epoch_digest
    }

    /// Returns the pinned v1 membership authority writer.
    #[must_use]
    pub const fn authority_writer_id(&self) -> WriterId {
        self.authority_writer_id
    }

    /// Returns the externally generated transition nonce.
    #[must_use]
    pub const fn transition_nonce(&self) -> [u8; DIGEST_BYTES] {
        self.transition_nonce
    }

    /// Returns the sorted removal and downgrade descriptions.
    #[must_use]
    pub fn changes(&self) -> &[WriteLossChange] {
        &self.changes
    }

    /// Returns the claimed sorted survivor set for external exact validation.
    #[must_use]
    pub fn survivor_writer_ids(&self) -> &[WriterId] {
        &self.survivor_writer_ids
    }

    /// Returns the claimed sorted write-losing set for external exact validation.
    #[must_use]
    pub fn losing_writer_ids(&self) -> &[WriterId] {
        &self.losing_writer_ids
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to every proposal field and its signature.
    #[must_use]
    pub const fn digest(&self) -> WriteLossProposalDigest {
        self.digest
    }
}

/// Survivor-authenticated receipt shape, not proof of a durable freeze.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedFreezeReceipt {
    proposal_digest: WriteLossProposalDigest,
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    signer_writer_id: WriterId,
    frontier_entries: Vec<ClockEntry>,
    frontier: VersionVector,
    losing_writer_tips: Vec<WriterCutoff>,
    canonical_record: Vec<u8>,
    digest: FreezeReceiptDigest,
}

impl fmt::Debug for SignatureCheckedFreezeReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedFreezeReceipt")
            .field("proposal_digest", &self.proposal_digest)
            .field("folder_id", &self.folder_id)
            .field("base_epoch", &self.base_epoch)
            .field("signer_writer_id", &self.signer_writer_id)
            .field("frontier_count", &self.frontier_entries.len())
            .field("losing_tip_count", &self.losing_writer_tips.len())
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedFreezeReceipt {
    /// Returns the complete signed-proposal commitment.
    #[must_use]
    pub const fn proposal_digest(&self) -> WriteLossProposalDigest {
        self.proposal_digest
    }

    /// Returns the affected folder.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the proposal base epoch.
    #[must_use]
    pub const fn base_epoch(&self) -> u64 {
        self.base_epoch
    }

    /// Returns the exact proposal base epoch digest.
    #[must_use]
    pub const fn base_epoch_digest(&self) -> EpochDigest {
        self.base_epoch_digest
    }

    /// Returns the historical survivor writer that signed the receipt.
    #[must_use]
    pub const fn signer_writer_id(&self) -> WriterId {
        self.signer_writer_id
    }

    /// Returns writer-typed claimed durable frontier components.
    #[must_use]
    pub fn frontier_entries(&self) -> &[ClockEntry] {
        &self.frontier_entries
    }

    /// Returns the claimed frontier in causal-math representation.
    #[must_use]
    pub const fn frontier(&self) -> &VersionVector {
        &self.frontier
    }

    /// Returns exact sorted tips for every claimed losing writer.
    #[must_use]
    pub fn losing_writer_tips(&self) -> &[WriterCutoff] {
        &self.losing_writer_tips
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to every receipt field and its signature.
    #[must_use]
    pub const fn digest(&self) -> FreezeReceiptDigest {
        self.digest
    }
}

/// Authority-authenticated abort shape, not a durable freeze release.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedFreezeAbort {
    proposal_digest: WriteLossProposalDigest,
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    authority_writer_id: WriterId,
    canonical_record: Vec<u8>,
    digest: FreezeAbortDigest,
}

impl fmt::Debug for SignatureCheckedFreezeAbort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedFreezeAbort")
            .field("proposal_digest", &self.proposal_digest)
            .field("folder_id", &self.folder_id)
            .field("base_epoch", &self.base_epoch)
            .field("authority_writer_id", &self.authority_writer_id)
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedFreezeAbort {
    /// Returns the exact proposal being aborted.
    #[must_use]
    pub const fn proposal_digest(&self) -> WriteLossProposalDigest {
        self.proposal_digest
    }

    /// Returns the affected folder.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the proposal base epoch.
    #[must_use]
    pub const fn base_epoch(&self) -> u64 {
        self.base_epoch
    }

    /// Returns the exact proposal base epoch digest.
    #[must_use]
    pub const fn base_epoch_digest(&self) -> EpochDigest {
        self.base_epoch_digest
    }

    /// Returns the pinned authority writer.
    #[must_use]
    pub const fn authority_writer_id(&self) -> WriterId {
        self.authority_writer_id
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to every abort field and its signature.
    #[must_use]
    pub const fn digest(&self) -> FreezeAbortDigest {
        self.digest
    }
}

/// Structural, canonicality, binding, or signature failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FreezeCodecError {
    #[error("signed freeze record exceeds the record limit")]
    RecordTooLarge,
    #[error("signed freeze record length overflow")]
    LengthOverflow,
    #[error("signed freeze record is truncated")]
    Truncated,
    #[error("signed freeze record has trailing bytes")]
    TrailingBytes,
    #[error("invalid signed freeze record magic")]
    InvalidMagic,
    #[error("unsupported signed freeze record version")]
    UnsupportedVersion,
    #[error("signed freeze record contains unknown flags")]
    UnknownFlags,
    #[error("write-loss proposal base epoch must be positive")]
    ZeroBaseEpoch,
    #[error("write-loss proposal collection has too many entries")]
    TooManyEntries,
    #[error("write-loss proposal changes must be nonempty and canonical")]
    NonCanonicalChanges,
    #[error("write-loss proposal survivors must be nonempty and canonical")]
    NonCanonicalSurvivors,
    #[error("write-loss proposal losing writers must be nonempty and canonical")]
    NonCanonicalLosingWriters,
    #[error("write-loss proposal contains an unknown change action")]
    UnknownChangeAction,
    #[error("write-loss proposal changes or losing writers include the authority")]
    AuthorityChanged,
    #[error("write-loss proposal authority is absent from survivors")]
    AuthorityNotSurvivor,
    #[error("removed writer appears in survivors")]
    RemovedWriterSurvives,
    #[error("downgraded writer is absent from survivors")]
    DowngradedWriterMissingFromSurvivors,
    #[error("downgraded writer is absent from losing writers")]
    DowngradedWriterMissingFromLosing,
    #[error("losing writer is not named in proposal changes")]
    LosingWriterNotChanged,
    #[error("write-loss authority key is weak")]
    WeakAuthorityKey,
    #[error("write-loss proposal folder binding does not match")]
    FolderMismatch,
    #[error("write-loss proposal authority writer binding does not match")]
    AuthorityWriterMismatch,
    #[error("write-loss proposal authority signature is invalid")]
    InvalidAuthoritySignature,
    #[error("freeze receipt historical signer key is weak")]
    WeakSurvivorKey,
    #[error("freeze receipt signing key differs from its historical member grant")]
    SurvivorKeyMismatch,
    #[error("freeze receipt signer is not a proposal survivor")]
    SignerNotSurvivor,
    #[error("freeze receipt frontier counters must be positive")]
    ZeroFrontierCounter,
    #[error("freeze receipt frontier is not in canonical writer order")]
    NonCanonicalFrontier,
    #[error("freeze receipt tips are not in canonical writer order")]
    NonCanonicalTips,
    #[error("zero freeze-receipt tip must use the all-zero digest")]
    InvalidZeroTip,
    #[error("freeze receipt tips do not exactly cover proposal losing writers")]
    LosingTipSetMismatch,
    #[error("freeze receipt frontier does not match a losing writer tip")]
    LosingTipFrontierMismatch,
    #[error("freeze receipt proposal binding does not match")]
    ProposalDigestMismatch,
    #[error("freeze receipt folder differs from its proposal")]
    ReceiptFolderMismatch,
    #[error("freeze receipt base epoch differs from its proposal")]
    ReceiptBaseEpochMismatch,
    #[error("freeze receipt base digest differs from its proposal")]
    ReceiptBaseDigestMismatch,
    #[error("freeze receipt signer differs from its historical member grant")]
    ReceiptSignerMismatch,
    #[error("freeze receipt survivor signature is invalid")]
    InvalidSurvivorSignature,
    #[error("freeze abort proposal binding does not match")]
    AbortProposalDigestMismatch,
    #[error("freeze abort folder differs from its proposal")]
    AbortFolderMismatch,
    #[error("freeze abort base epoch differs from its proposal")]
    AbortBaseEpochMismatch,
    #[error("freeze abort base digest differs from its proposal")]
    AbortBaseDigestMismatch,
    #[error("freeze abort authority differs from its proposal")]
    AbortAuthorityMismatch,
    #[error("freeze abort signing key differs from the proposal authority")]
    AbortAuthorityKeyMismatch,
    #[error("freeze abort authority signature is invalid")]
    InvalidAbortSignature,
}

/// Encodes and authority-signs one canonical write-loss proposal.
///
/// The transition nonce and key are supplied externally. This performs no RNG,
/// roster lookup, persistence, or transition acceptance.
#[allow(clippy::too_many_arguments)]
pub fn encode_signed_write_loss_proposal(
    authority_signing_key: &SigningKey,
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    authority_writer_id: WriterId,
    transition_nonce: [u8; DIGEST_BYTES],
    changes: &[WriteLossChange],
    survivor_writer_ids: &[WriterId],
    losing_writer_ids: &[WriterId],
) -> Result<Vec<u8>, FreezeCodecError> {
    validate_counts(
        changes.len(),
        survivor_writer_ids.len(),
        losing_writer_ids.len(),
    )?;
    let record_len = proposal_record_len(
        changes.len(),
        survivor_writer_ids.len(),
        losing_writer_ids.len(),
    )?;
    check_encoded_bound(record_len)?;
    let authority_key = authority_signing_key.verifying_key();
    if authority_key.is_weak() {
        return Err(FreezeCodecError::WeakAuthorityKey);
    }
    validate_proposal_shape(
        base_epoch,
        authority_writer_id,
        changes,
        survivor_writer_ids,
        losing_writer_ids,
    )?;

    let mut record = Vec::with_capacity(record_len);
    encode_proposal_unsigned(
        &mut record,
        folder_id,
        base_epoch,
        base_epoch_digest,
        authority_writer_id,
        transition_nonce,
        changes,
        survivor_writer_ids,
        losing_writer_ids,
    );
    append_signature(
        &mut record,
        authority_signing_key,
        PROPOSAL_SIGNATURE_DOMAIN,
    );
    Ok(record)
}

/// Parses canonical proposal bytes and verifies the separately pinned authority.
pub fn decode_signature_checked_write_loss_proposal(
    record: &[u8],
    expected_folder: FolderId,
    expected_authority_writer: WriterId,
    pinned_authority_key: &VerifyingKey,
) -> Result<SignatureCheckedWriteLossProposal, FreezeCodecError> {
    check_record_bound(record)?;
    if pinned_authority_key.is_weak() {
        return Err(FreezeCodecError::WeakAuthorityKey);
    }
    let mut cursor = Cursor::new(record);
    read_prelude(&mut cursor, PROPOSAL_MAGIC)?;
    let folder_id = read_folder_id(&mut cursor)?;
    let base_epoch = cursor.take_u64()?;
    let base_epoch_digest = EpochDigest::from_bytes(cursor.take_array::<DIGEST_BYTES>()?);
    let authority_writer_id = read_writer_id(&mut cursor)?;
    let transition_nonce = cursor.take_array::<DIGEST_BYTES>()?;
    let changes = decode_changes(&mut cursor)?;
    let survivor_writer_ids = decode_writer_ids(&mut cursor)?;
    let losing_writer_ids = decode_writer_ids(&mut cursor)?;
    let unsigned_len = cursor.position();
    let signature = Signature::from_bytes(&cursor.take_array::<SIGNATURE_BYTES>()?);
    ensure_consumed(&cursor)?;

    validate_proposal_shape(
        base_epoch,
        authority_writer_id,
        &changes,
        &survivor_writer_ids,
        &losing_writer_ids,
    )?;
    verify_signature(
        pinned_authority_key,
        PROPOSAL_SIGNATURE_DOMAIN,
        &record[..unsigned_len],
        &signature,
        FreezeCodecError::InvalidAuthoritySignature,
    )?;
    if folder_id != expected_folder {
        return Err(FreezeCodecError::FolderMismatch);
    }
    if authority_writer_id != expected_authority_writer {
        return Err(FreezeCodecError::AuthorityWriterMismatch);
    }
    let canonical_record = record.to_vec();
    let digest = WriteLossProposalDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedWriteLossProposal {
        folder_id,
        base_epoch,
        base_epoch_digest,
        authority_writer_id,
        authority_key: *pinned_authority_key,
        transition_nonce,
        changes,
        survivor_writer_ids,
        losing_writer_ids,
        canonical_record,
        digest,
    })
}

/// Encodes a survivor-signed receipt bound to one checked proposal.
///
/// Production may call this only after durably freezing every losing writer and
/// retaining a causally closed authenticated frontier and exact tips.
pub fn encode_signed_freeze_receipt(
    survivor_signing_key: &SigningKey,
    proposal: &SignatureCheckedWriteLossProposal,
    historical_survivor_grant: &MemberGrant,
    frontier: &[ClockEntry],
    losing_writer_tips: &[WriterCutoff],
) -> Result<Vec<u8>, FreezeCodecError> {
    validate_counts(frontier.len(), losing_writer_tips.len(), 0)?;
    let record_len = receipt_record_len(frontier.len(), losing_writer_tips.len())?;
    check_encoded_bound(record_len)?;
    validate_survivor_key(
        proposal,
        historical_survivor_grant,
        &survivor_signing_key.verifying_key(),
    )?;
    validate_receipt_shape(proposal, frontier, losing_writer_tips)?;

    let mut record = Vec::with_capacity(record_len);
    encode_receipt_unsigned(
        &mut record,
        proposal,
        historical_survivor_grant.writer_id(),
        frontier,
        losing_writer_tips,
    );
    append_signature(
        &mut record,
        survivor_signing_key,
        FREEZE_RECEIPT_SIGNATURE_DOMAIN,
    );
    Ok(record)
}

/// Parses a survivor receipt and verifies its historical member key binding.
pub fn decode_signature_checked_freeze_receipt(
    record: &[u8],
    proposal: &SignatureCheckedWriteLossProposal,
    historical_survivor_grant: &MemberGrant,
) -> Result<SignatureCheckedFreezeReceipt, FreezeCodecError> {
    check_record_bound(record)?;
    let survivor_key = historical_survivor_grant.writer_key();
    validate_survivor_key(proposal, historical_survivor_grant, survivor_key)?;
    let mut cursor = Cursor::new(record);
    read_prelude(&mut cursor, RECEIPT_MAGIC)?;
    let proposal_digest = WriteLossProposalDigest(cursor.take_array::<DIGEST_BYTES>()?);
    let folder_id = read_folder_id(&mut cursor)?;
    let base_epoch = cursor.take_u64()?;
    let base_epoch_digest = EpochDigest::from_bytes(cursor.take_array::<DIGEST_BYTES>()?);
    let signer_writer_id = read_writer_id(&mut cursor)?;
    let frontier_entries = decode_frontier(&mut cursor)?;
    let losing_writer_tips = decode_tips(&mut cursor)?;
    let unsigned_len = cursor.position();
    let signature = Signature::from_bytes(&cursor.take_array::<SIGNATURE_BYTES>()?);
    ensure_consumed(&cursor)?;

    let frontier = validate_receipt_shape(proposal, &frontier_entries, &losing_writer_tips)?;
    verify_signature(
        survivor_key,
        FREEZE_RECEIPT_SIGNATURE_DOMAIN,
        &record[..unsigned_len],
        &signature,
        FreezeCodecError::InvalidSurvivorSignature,
    )?;
    if proposal_digest != proposal.digest() {
        return Err(FreezeCodecError::ProposalDigestMismatch);
    }
    if folder_id != proposal.folder_id() {
        return Err(FreezeCodecError::ReceiptFolderMismatch);
    }
    if base_epoch != proposal.base_epoch() {
        return Err(FreezeCodecError::ReceiptBaseEpochMismatch);
    }
    if base_epoch_digest != proposal.base_epoch_digest() {
        return Err(FreezeCodecError::ReceiptBaseDigestMismatch);
    }
    if signer_writer_id != historical_survivor_grant.writer_id() {
        return Err(FreezeCodecError::ReceiptSignerMismatch);
    }
    let canonical_record = record.to_vec();
    let digest = FreezeReceiptDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedFreezeReceipt {
        proposal_digest,
        folder_id,
        base_epoch,
        base_epoch_digest,
        signer_writer_id,
        frontier_entries,
        frontier,
        losing_writer_tips,
        canonical_record,
        digest,
    })
}

/// Encodes an authority-signed abort bound to one checked proposal.
///
/// This record alone does not release a durable freeze.
pub fn encode_signed_freeze_abort(
    authority_signing_key: &SigningKey,
    proposal: &SignatureCheckedWriteLossProposal,
) -> Result<Vec<u8>, FreezeCodecError> {
    let supplied_key = authority_signing_key.verifying_key();
    if supplied_key.is_weak() {
        return Err(FreezeCodecError::WeakAuthorityKey);
    }
    if supplied_key != proposal.authority_key {
        return Err(FreezeCodecError::AbortAuthorityKeyMismatch);
    }
    let record_len = FIXED_ABORT_UNSIGNED_BYTES
        .checked_add(SIGNATURE_BYTES)
        .ok_or(FreezeCodecError::LengthOverflow)?;
    check_encoded_bound(record_len)?;
    let mut record = Vec::with_capacity(record_len);
    encode_abort_unsigned(&mut record, proposal);
    append_signature(
        &mut record,
        authority_signing_key,
        FREEZE_ABORT_SIGNATURE_DOMAIN,
    );
    Ok(record)
}

/// Parses an abort and verifies it against the exact proposal and pinned authority.
pub fn decode_signature_checked_freeze_abort(
    record: &[u8],
    proposal: &SignatureCheckedWriteLossProposal,
    pinned_authority_key: &VerifyingKey,
) -> Result<SignatureCheckedFreezeAbort, FreezeCodecError> {
    check_record_bound(record)?;
    if pinned_authority_key.is_weak() {
        return Err(FreezeCodecError::WeakAuthorityKey);
    }
    if *pinned_authority_key != proposal.authority_key {
        return Err(FreezeCodecError::AbortAuthorityKeyMismatch);
    }
    let mut cursor = Cursor::new(record);
    read_prelude(&mut cursor, ABORT_MAGIC)?;
    let proposal_digest = WriteLossProposalDigest(cursor.take_array::<DIGEST_BYTES>()?);
    let folder_id = read_folder_id(&mut cursor)?;
    let base_epoch = cursor.take_u64()?;
    let base_epoch_digest = EpochDigest::from_bytes(cursor.take_array::<DIGEST_BYTES>()?);
    let authority_writer_id = read_writer_id(&mut cursor)?;
    let unsigned_len = cursor.position();
    let signature = Signature::from_bytes(&cursor.take_array::<SIGNATURE_BYTES>()?);
    ensure_consumed(&cursor)?;

    verify_signature(
        pinned_authority_key,
        FREEZE_ABORT_SIGNATURE_DOMAIN,
        &record[..unsigned_len],
        &signature,
        FreezeCodecError::InvalidAbortSignature,
    )?;
    if proposal_digest != proposal.digest() {
        return Err(FreezeCodecError::AbortProposalDigestMismatch);
    }
    if folder_id != proposal.folder_id() {
        return Err(FreezeCodecError::AbortFolderMismatch);
    }
    if base_epoch != proposal.base_epoch() {
        return Err(FreezeCodecError::AbortBaseEpochMismatch);
    }
    if base_epoch_digest != proposal.base_epoch_digest() {
        return Err(FreezeCodecError::AbortBaseDigestMismatch);
    }
    if authority_writer_id != proposal.authority_writer_id() {
        return Err(FreezeCodecError::AbortAuthorityMismatch);
    }
    let canonical_record = record.to_vec();
    let digest = FreezeAbortDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedFreezeAbort {
        proposal_digest,
        folder_id,
        base_epoch,
        base_epoch_digest,
        authority_writer_id,
        canonical_record,
        digest,
    })
}

fn validate_proposal_shape(
    base_epoch: u64,
    authority_writer_id: WriterId,
    changes: &[WriteLossChange],
    survivors: &[WriterId],
    losing: &[WriterId],
) -> Result<(), FreezeCodecError> {
    if base_epoch == 0 {
        return Err(FreezeCodecError::ZeroBaseEpoch);
    }
    validate_sorted_changes(changes)?;
    validate_sorted_writer_ids(survivors, FreezeCodecError::NonCanonicalSurvivors)?;
    validate_sorted_writer_ids(losing, FreezeCodecError::NonCanonicalLosingWriters)?;
    if !survivors.contains(&authority_writer_id) {
        return Err(FreezeCodecError::AuthorityNotSurvivor);
    }
    if changes
        .iter()
        .any(|change| change.writer_id == authority_writer_id)
        || losing.contains(&authority_writer_id)
    {
        return Err(FreezeCodecError::AuthorityChanged);
    }
    for change in changes {
        match change.action {
            WriteLossAction::Remove if survivors.contains(&change.writer_id) => {
                return Err(FreezeCodecError::RemovedWriterSurvives);
            }
            WriteLossAction::DowngradeToRead if !survivors.contains(&change.writer_id) => {
                return Err(FreezeCodecError::DowngradedWriterMissingFromSurvivors);
            }
            WriteLossAction::DowngradeToRead if !losing.contains(&change.writer_id) => {
                return Err(FreezeCodecError::DowngradedWriterMissingFromLosing);
            }
            WriteLossAction::Remove | WriteLossAction::DowngradeToRead => {}
        }
    }
    if losing.iter().any(|writer| {
        changes
            .binary_search_by_key(writer, |change| change.writer_id)
            .is_err()
    }) {
        return Err(FreezeCodecError::LosingWriterNotChanged);
    }
    Ok(())
}

fn validate_sorted_changes(changes: &[WriteLossChange]) -> Result<(), FreezeCodecError> {
    if changes.is_empty()
        || changes
            .windows(2)
            .any(|pair| pair[0].writer_id >= pair[1].writer_id)
    {
        return Err(FreezeCodecError::NonCanonicalChanges);
    }
    Ok(())
}

fn validate_sorted_writer_ids(
    writers: &[WriterId],
    error: FreezeCodecError,
) -> Result<(), FreezeCodecError> {
    if writers.is_empty() || writers.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(error);
    }
    Ok(())
}

fn validate_survivor_key(
    proposal: &SignatureCheckedWriteLossProposal,
    grant: &MemberGrant,
    supplied_key: &VerifyingKey,
) -> Result<(), FreezeCodecError> {
    if supplied_key.is_weak() {
        return Err(FreezeCodecError::WeakSurvivorKey);
    }
    if *supplied_key != *grant.writer_key() {
        return Err(FreezeCodecError::SurvivorKeyMismatch);
    }
    if proposal
        .survivor_writer_ids()
        .binary_search(&grant.writer_id())
        .is_err()
    {
        return Err(FreezeCodecError::SignerNotSurvivor);
    }
    Ok(())
}

fn validate_receipt_shape(
    proposal: &SignatureCheckedWriteLossProposal,
    frontier_entries: &[ClockEntry],
    tips: &[WriterCutoff],
) -> Result<VersionVector, FreezeCodecError> {
    validate_frontier(frontier_entries)?;
    validate_tips(tips)?;
    if tips.len() != proposal.losing_writer_ids().len()
        || tips
            .iter()
            .zip(proposal.losing_writer_ids())
            .any(|(tip, writer)| tip.writer_id() != *writer)
    {
        return Err(FreezeCodecError::LosingTipSetMismatch);
    }
    let frontier = VersionVector::new(
        frontier_entries
            .iter()
            .map(|entry| (entry.writer_id().into_vector_actor(), entry.counter())),
    )
    .map_err(|_| FreezeCodecError::NonCanonicalFrontier)?;
    for tip in tips {
        if frontier.counter(tip.writer_id().into_vector_actor()) != tip.counter() {
            return Err(FreezeCodecError::LosingTipFrontierMismatch);
        }
    }
    Ok(frontier)
}

fn validate_frontier(frontier: &[ClockEntry]) -> Result<(), FreezeCodecError> {
    ensure_count(frontier.len())?;
    let mut previous: Option<[u8; 16]> = None;
    for entry in frontier {
        if entry.counter() == 0 {
            return Err(FreezeCodecError::ZeroFrontierCounter);
        }
        let writer = entry.writer_id().to_bytes();
        if previous.is_some_and(|prior| prior >= writer) {
            return Err(FreezeCodecError::NonCanonicalFrontier);
        }
        previous = Some(writer);
    }
    Ok(())
}

fn validate_tips(tips: &[WriterCutoff]) -> Result<(), FreezeCodecError> {
    ensure_count(tips.len())?;
    let mut previous: Option<[u8; 16]> = None;
    for tip in tips {
        if tip.counter() == 0 && tip.operation_digest() != [0; DIGEST_BYTES] {
            return Err(FreezeCodecError::InvalidZeroTip);
        }
        let writer = tip.writer_id().to_bytes();
        if previous.is_some_and(|prior| prior >= writer) {
            return Err(FreezeCodecError::NonCanonicalTips);
        }
        previous = Some(writer);
    }
    Ok(())
}

fn validate_counts(first: usize, second: usize, third: usize) -> Result<(), FreezeCodecError> {
    ensure_count(first)?;
    ensure_count(second)?;
    ensure_count(third)
}

fn ensure_count(count: usize) -> Result<(), FreezeCodecError> {
    if count > MAX_FREEZE_COLLECTION_ENTRIES {
        return Err(FreezeCodecError::TooManyEntries);
    }
    Ok(())
}

fn proposal_record_len(
    changes: usize,
    survivors: usize,
    losing: usize,
) -> Result<usize, FreezeCodecError> {
    FIXED_PROPOSAL_UNSIGNED_BYTES
        .checked_add(
            changes
                .checked_mul(CHANGE_BYTES)
                .ok_or(FreezeCodecError::LengthOverflow)?,
        )
        .and_then(|length| length.checked_add(survivors.checked_mul(16)?))
        .and_then(|length| length.checked_add(losing.checked_mul(16)?))
        .and_then(|length| length.checked_add(SIGNATURE_BYTES))
        .ok_or(FreezeCodecError::LengthOverflow)
}

fn receipt_record_len(frontier: usize, tips: usize) -> Result<usize, FreezeCodecError> {
    FIXED_RECEIPT_UNSIGNED_BYTES
        .checked_add(
            frontier
                .checked_mul(FRONTIER_ENTRY_BYTES)
                .ok_or(FreezeCodecError::LengthOverflow)?,
        )
        .and_then(|length| length.checked_add(tips.checked_mul(CUTOFF_ENTRY_BYTES)?))
        .and_then(|length| length.checked_add(SIGNATURE_BYTES))
        .ok_or(FreezeCodecError::LengthOverflow)
}

fn check_encoded_bound(length: usize) -> Result<(), FreezeCodecError> {
    if length > MAX_SIGNED_FREEZE_RECORD_BYTES {
        return Err(FreezeCodecError::RecordTooLarge);
    }
    Ok(())
}

fn check_record_bound(record: &[u8]) -> Result<(), FreezeCodecError> {
    check_encoded_bound(record.len())
}

#[allow(clippy::too_many_arguments)]
fn encode_proposal_unsigned(
    output: &mut Vec<u8>,
    folder_id: FolderId,
    base_epoch: u64,
    base_epoch_digest: EpochDigest,
    authority_writer_id: WriterId,
    transition_nonce: [u8; DIGEST_BYTES],
    changes: &[WriteLossChange],
    survivor_writer_ids: &[WriterId],
    losing_writer_ids: &[WriterId],
) {
    write_prelude(output, PROPOSAL_MAGIC);
    output.extend_from_slice(&folder_id.to_bytes());
    output.extend_from_slice(&base_epoch.to_be_bytes());
    output.extend_from_slice(&base_epoch_digest.to_bytes());
    output.extend_from_slice(&authority_writer_id.to_bytes());
    output.extend_from_slice(&transition_nonce);
    output.extend_from_slice(&(changes.len() as u16).to_be_bytes());
    for change in changes {
        output.extend_from_slice(&change.writer_id.to_bytes());
        output.push(change.action as u8);
    }
    encode_writer_ids(output, survivor_writer_ids);
    encode_writer_ids(output, losing_writer_ids);
}

fn encode_receipt_unsigned(
    output: &mut Vec<u8>,
    proposal: &SignatureCheckedWriteLossProposal,
    signer_writer_id: WriterId,
    frontier: &[ClockEntry],
    tips: &[WriterCutoff],
) {
    write_prelude(output, RECEIPT_MAGIC);
    output.extend_from_slice(&proposal.digest().to_bytes());
    output.extend_from_slice(&proposal.folder_id().to_bytes());
    output.extend_from_slice(&proposal.base_epoch().to_be_bytes());
    output.extend_from_slice(&proposal.base_epoch_digest().to_bytes());
    output.extend_from_slice(&signer_writer_id.to_bytes());
    output.extend_from_slice(&(frontier.len() as u16).to_be_bytes());
    for entry in frontier {
        output.extend_from_slice(&entry.writer_id().to_bytes());
        output.extend_from_slice(&entry.counter().to_be_bytes());
    }
    output.extend_from_slice(&(tips.len() as u16).to_be_bytes());
    for tip in tips {
        output.extend_from_slice(&tip.writer_id().to_bytes());
        output.extend_from_slice(&tip.counter().to_be_bytes());
        output.extend_from_slice(&tip.operation_digest());
    }
}

fn encode_abort_unsigned(output: &mut Vec<u8>, proposal: &SignatureCheckedWriteLossProposal) {
    write_prelude(output, ABORT_MAGIC);
    output.extend_from_slice(&proposal.digest().to_bytes());
    output.extend_from_slice(&proposal.folder_id().to_bytes());
    output.extend_from_slice(&proposal.base_epoch().to_be_bytes());
    output.extend_from_slice(&proposal.base_epoch_digest().to_bytes());
    output.extend_from_slice(&proposal.authority_writer_id().to_bytes());
}

fn write_prelude(output: &mut Vec<u8>, magic: &[u8; 8]) {
    output.extend_from_slice(magic);
    output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
    output.push(KNOWN_FLAGS);
}

fn read_prelude(cursor: &mut Cursor<'_>, magic: &[u8; 8]) -> Result<(), FreezeCodecError> {
    if cursor.take_array::<8>()? != *magic {
        return Err(FreezeCodecError::InvalidMagic);
    }
    if cursor.take_u16()? != WIRE_VERSION {
        return Err(FreezeCodecError::UnsupportedVersion);
    }
    if cursor.take_u8()? != KNOWN_FLAGS {
        return Err(FreezeCodecError::UnknownFlags);
    }
    Ok(())
}

fn decode_changes(cursor: &mut Cursor<'_>) -> Result<Vec<WriteLossChange>, FreezeCodecError> {
    let count = usize::from(cursor.take_u16()?);
    ensure_count(count)?;
    let mut changes = Vec::with_capacity(count);
    for _ in 0..count {
        changes.push(WriteLossChange {
            writer_id: read_writer_id(cursor)?,
            action: WriteLossAction::from_byte(cursor.take_u8()?)?,
        });
    }
    Ok(changes)
}

fn decode_writer_ids(cursor: &mut Cursor<'_>) -> Result<Vec<WriterId>, FreezeCodecError> {
    let count = usize::from(cursor.take_u16()?);
    ensure_count(count)?;
    let mut writers = Vec::with_capacity(count);
    for _ in 0..count {
        writers.push(read_writer_id(cursor)?);
    }
    Ok(writers)
}

fn decode_frontier(cursor: &mut Cursor<'_>) -> Result<Vec<ClockEntry>, FreezeCodecError> {
    let count = usize::from(cursor.take_u16()?);
    ensure_count(count)?;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let writer = read_writer_id(cursor)?;
        let counter = cursor.take_u64()?;
        entries.push(
            ClockEntry::new(writer, counter).map_err(|_| FreezeCodecError::ZeroFrontierCounter)?,
        );
    }
    Ok(entries)
}

fn decode_tips(cursor: &mut Cursor<'_>) -> Result<Vec<WriterCutoff>, FreezeCodecError> {
    let count = usize::from(cursor.take_u16()?);
    ensure_count(count)?;
    let mut tips = Vec::with_capacity(count);
    for _ in 0..count {
        let writer = read_writer_id(cursor)?;
        let counter = cursor.take_u64()?;
        let digest = cursor.take_array::<DIGEST_BYTES>()?;
        tips.push(
            WriterCutoff::new(writer, counter, digest)
                .map_err(|_| FreezeCodecError::InvalidZeroTip)?,
        );
    }
    Ok(tips)
}

fn encode_writer_ids(output: &mut Vec<u8>, writers: &[WriterId]) {
    output.extend_from_slice(&(writers.len() as u16).to_be_bytes());
    for writer in writers {
        output.extend_from_slice(&writer.to_bytes());
    }
}

fn read_folder_id(cursor: &mut Cursor<'_>) -> Result<FolderId, FreezeCodecError> {
    Ok(FolderId::from_uuid(Uuid::from_bytes(
        cursor.take_array::<16>()?,
    )))
}

fn read_writer_id(cursor: &mut Cursor<'_>) -> Result<WriterId, FreezeCodecError> {
    Ok(WriterId::from_uuid(Uuid::from_bytes(
        cursor.take_array::<16>()?,
    )))
}

fn append_signature(output: &mut Vec<u8>, key: &SigningKey, domain: &[u8]) {
    let signature = key.sign(&signature_message(domain, output));
    output.extend_from_slice(&signature.to_bytes());
}

fn verify_signature(
    key: &VerifyingKey,
    domain: &[u8],
    unsigned: &[u8],
    signature: &Signature,
    error: FreezeCodecError,
) -> Result<(), FreezeCodecError> {
    key.verify_strict(&signature_message(domain, unsigned), signature)
        .map_err(|_| error)
}

fn signature_message(domain: &[u8], unsigned: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(domain.len() + unsigned.len());
    message.extend_from_slice(domain);
    message.extend_from_slice(unsigned);
    message
}

fn ensure_consumed(cursor: &Cursor<'_>) -> Result<(), FreezeCodecError> {
    if cursor.remaining() != 0 {
        return Err(FreezeCodecError::TrailingBytes);
    }
    Ok(())
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

    fn take(&mut self, length: usize) -> Result<&'a [u8], FreezeCodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(FreezeCodecError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(FreezeCodecError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], FreezeCodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| FreezeCodecError::Truncated)
    }

    fn take_u8(&mut self) -> Result<u8, FreezeCodecError> {
        Ok(self.take_array::<1>()?[0])
    }

    fn take_u16(&mut self) -> Result<u16, FreezeCodecError> {
        Ok(u16::from_be_bytes(self.take_array::<2>()?))
    }

    fn take_u64(&mut self) -> Result<u64, FreezeCodecError> {
        Ok(u64::from_be_bytes(self.take_array::<8>()?))
    }
}

#[cfg(test)]
mod tests {
    use covalent_protocol::DeviceId;

    use super::*;
    use crate::sync::membership::MemberRole;

    const PROPOSAL_FOLDER_OFFSET: usize = 11;
    const PROPOSAL_BASE_EPOCH_OFFSET: usize = 27;
    const PROPOSAL_BASE_DIGEST_OFFSET: usize = 35;
    const PROPOSAL_AUTHORITY_OFFSET: usize = 67;
    const PROPOSAL_NONCE_OFFSET: usize = 83;
    const PROPOSAL_CHANGES_COUNT_OFFSET: usize = 115;
    const PROPOSAL_CHANGES_OFFSET: usize = 117;
    const PROPOSAL_SURVIVORS_OFFSET: usize = PROPOSAL_CHANGES_OFFSET + 3 * CHANGE_BYTES + 2;
    const PROPOSAL_LOSING_OFFSET: usize = PROPOSAL_SURVIVORS_OFFSET + 2 * 16 + 2;

    const RECEIPT_PROPOSAL_DIGEST_OFFSET: usize = 11;
    const RECEIPT_FOLDER_OFFSET: usize = 43;
    const RECEIPT_BASE_EPOCH_OFFSET: usize = 59;
    const RECEIPT_BASE_DIGEST_OFFSET: usize = 67;
    const RECEIPT_SIGNER_OFFSET: usize = 99;
    const RECEIPT_FRONTIER_OFFSET: usize = 117;
    const RECEIPT_TIPS_OFFSET: usize = RECEIPT_FRONTIER_OFFSET + 2 * FRONTIER_ENTRY_BYTES + 2;

    const ABORT_PROPOSAL_DIGEST_OFFSET: usize = 11;
    const ABORT_FOLDER_OFFSET: usize = 43;
    const ABORT_BASE_EPOCH_OFFSET: usize = 59;
    const ABORT_BASE_DIGEST_OFFSET: usize = 67;
    const ABORT_AUTHORITY_OFFSET: usize = 99;

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
        let mut bytes = [0x53; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        SigningKey::from_bytes(&bytes)
    }

    fn member(writer_number: u128, key_number: u64, role: MemberRole) -> MemberGrant {
        MemberGrant::new(
            writer(writer_number),
            key(key_number).verifying_key(),
            device(1_000 + writer_number),
            key(100 + key_number).verifying_key(),
            role,
        )
        .expect("member grant")
    }

    fn changes() -> Vec<WriteLossChange> {
        vec![
            WriteLossChange::new(writer(2), WriteLossAction::DowngradeToRead),
            WriteLossChange::new(writer(3), WriteLossAction::Remove),
            WriteLossChange::new(writer(4), WriteLossAction::Remove),
        ]
    }

    fn proposal_record() -> Vec<u8> {
        encode_signed_write_loss_proposal(
            &key(1),
            folder(7),
            4,
            EpochDigest::from_bytes([8; 32]),
            writer(1),
            [9; 32],
            &changes(),
            &[writer(1), writer(2)],
            &[writer(2), writer(3)],
        )
        .expect("proposal")
    }

    fn decode_proposal(
        record: &[u8],
    ) -> Result<SignatureCheckedWriteLossProposal, FreezeCodecError> {
        decode_signature_checked_write_loss_proposal(
            record,
            folder(7),
            writer(1),
            &key(1).verifying_key(),
        )
    }

    fn checked_proposal() -> SignatureCheckedWriteLossProposal {
        decode_proposal(&proposal_record()).expect("checked proposal")
    }

    fn frontier() -> Vec<ClockEntry> {
        vec![
            ClockEntry::new(writer(1), 10).expect("clock"),
            ClockEntry::new(writer(2), 5).expect("clock"),
        ]
    }

    fn tips() -> Vec<WriterCutoff> {
        vec![
            WriterCutoff::new(writer(2), 5, [4; 32]).expect("tip"),
            WriterCutoff::new(writer(3), 0, [0; 32]).expect("zero tip"),
        ]
    }

    fn receipt_record(proposal: &SignatureCheckedWriteLossProposal) -> Vec<u8> {
        encode_signed_freeze_receipt(
            &key(1),
            proposal,
            &member(1, 1, MemberRole::ReadWrite),
            &frontier(),
            &tips(),
        )
        .expect("receipt")
    }

    fn resign(record: &mut Vec<u8>, signing_key: &SigningKey, domain: &[u8]) {
        record.truncate(record.len() - SIGNATURE_BYTES);
        append_signature(record, signing_key, domain);
    }

    #[test]
    fn proposal_round_trip_commits_complete_canonical_record() {
        let record = proposal_record();
        let checked = decode_proposal(&record).expect("valid proposal");

        assert_eq!(checked.folder_id(), folder(7));
        assert_eq!(checked.base_epoch(), 4);
        assert_eq!(
            checked.base_epoch_digest(),
            EpochDigest::from_bytes([8; 32])
        );
        assert_eq!(checked.authority_writer_id(), writer(1));
        assert_eq!(checked.transition_nonce(), [9; 32]);
        assert_eq!(checked.changes(), changes());
        assert_eq!(checked.survivor_writer_ids(), &[writer(1), writer(2)]);
        assert_eq!(checked.losing_writer_ids(), &[writer(2), writer(3)]);
        assert_eq!(checked.canonical_record(), record);
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        let debug = format!("{checked:?}");
        assert!(!debug.contains("transition_nonce"));
    }

    #[test]
    fn proposal_rejects_noncanonical_or_inconsistent_sets() {
        let encode = |changes: &[WriteLossChange], survivors: &[WriterId], losing: &[WriterId]| {
            encode_signed_write_loss_proposal(
                &key(1),
                folder(7),
                4,
                EpochDigest::from_bytes([8; 32]),
                writer(1),
                [9; 32],
                changes,
                survivors,
                losing,
            )
        };

        assert_eq!(
            encode(&[], &[writer(1)], &[writer(2)]),
            Err(FreezeCodecError::NonCanonicalChanges)
        );
        assert_eq!(
            encode(&changes(), &[], &[writer(2)]),
            Err(FreezeCodecError::NonCanonicalSurvivors)
        );
        assert_eq!(
            encode(&changes(), &[writer(1), writer(2)], &[]),
            Err(FreezeCodecError::NonCanonicalLosingWriters)
        );
        assert_eq!(
            encode(
                &[
                    WriteLossChange::new(writer(3), WriteLossAction::Remove),
                    WriteLossChange::new(writer(2), WriteLossAction::DowngradeToRead),
                ],
                &[writer(1), writer(2)],
                &[writer(2), writer(3)],
            ),
            Err(FreezeCodecError::NonCanonicalChanges)
        );
        assert_eq!(
            encode(
                &[WriteLossChange::new(writer(1), WriteLossAction::Remove)],
                &[writer(1)],
                &[writer(1)],
            ),
            Err(FreezeCodecError::AuthorityChanged)
        );
        assert_eq!(
            encode(
                &[WriteLossChange::new(writer(2), WriteLossAction::Remove)],
                &[writer(1), writer(2)],
                &[writer(2)],
            ),
            Err(FreezeCodecError::RemovedWriterSurvives)
        );
        assert_eq!(
            encode(
                &[WriteLossChange::new(
                    writer(2),
                    WriteLossAction::DowngradeToRead
                )],
                &[writer(1), writer(2)],
                &[writer(3)],
            ),
            Err(FreezeCodecError::DowngradedWriterMissingFromLosing)
        );
        assert_eq!(
            encode(
                &[WriteLossChange::new(writer(2), WriteLossAction::Remove)],
                &[writer(1)],
                &[writer(3)],
            ),
            Err(FreezeCodecError::LosingWriterNotChanged)
        );
        assert!(encode(&changes(), &[writer(1), writer(2)], &[writer(2), writer(3)]).is_ok());
        assert_eq!(
            encode_signed_write_loss_proposal(
                &key(1),
                folder(7),
                0,
                EpochDigest::from_bytes([8; 32]),
                writer(1),
                [9; 32],
                &changes(),
                &[writer(1), writer(2)],
                &[writer(2), writer(3)],
            ),
            Err(FreezeCodecError::ZeroBaseEpoch)
        );
    }

    #[test]
    fn proposal_rejects_wrong_bindings_weak_key_and_tampering() {
        let record = proposal_record();
        assert_eq!(
            decode_signature_checked_write_loss_proposal(
                &record,
                folder(8),
                writer(1),
                &key(1).verifying_key(),
            ),
            Err(FreezeCodecError::FolderMismatch)
        );
        assert_eq!(
            decode_signature_checked_write_loss_proposal(
                &record,
                folder(7),
                writer(8),
                &key(1).verifying_key(),
            ),
            Err(FreezeCodecError::AuthorityWriterMismatch)
        );
        let weak = VerifyingKey::from_bytes(&{
            let mut bytes = [0; 32];
            bytes[0] = 1;
            bytes
        })
        .expect("identity point");
        assert_eq!(
            decode_signature_checked_write_loss_proposal(&record, folder(7), writer(1), &weak),
            Err(FreezeCodecError::WeakAuthorityKey)
        );

        for offset in [
            PROPOSAL_FOLDER_OFFSET,
            PROPOSAL_BASE_EPOCH_OFFSET,
            PROPOSAL_BASE_DIGEST_OFFSET,
            PROPOSAL_AUTHORITY_OFFSET,
            PROPOSAL_NONCE_OFFSET,
            PROPOSAL_CHANGES_OFFSET,
            PROPOSAL_SURVIVORS_OFFSET,
            PROPOSAL_LOSING_OFFSET,
            record.len() - 1,
        ] {
            let mut tampered = record.clone();
            tampered[offset] ^= 1;
            assert!(decode_proposal(&tampered).is_err(), "offset {offset}");
        }

        let mut unknown_action = record;
        unknown_action[PROPOSAL_CHANGES_OFFSET + 16] = 0xff;
        assert_eq!(
            decode_proposal(&unknown_action),
            Err(FreezeCodecError::UnknownChangeAction)
        );
    }

    #[test]
    fn receipt_round_trip_accepts_exact_positive_and_zero_tips() {
        let proposal = checked_proposal();
        let record = receipt_record(&proposal);
        let grant = member(1, 1, MemberRole::ReadWrite);
        let checked = decode_signature_checked_freeze_receipt(&record, &proposal, &grant)
            .expect("valid receipt");

        assert_eq!(checked.proposal_digest(), proposal.digest());
        assert_eq!(checked.folder_id(), folder(7));
        assert_eq!(checked.base_epoch(), 4);
        assert_eq!(
            checked.base_epoch_digest(),
            EpochDigest::from_bytes([8; 32])
        );
        assert_eq!(checked.signer_writer_id(), writer(1));
        assert_eq!(checked.frontier_entries(), frontier());
        assert_eq!(checked.frontier().counter(writer(2).into_vector_actor()), 5);
        assert_eq!(checked.frontier().counter(writer(3).into_vector_actor()), 0);
        assert_eq!(checked.losing_writer_tips(), tips());
        assert_eq!(checked.canonical_record(), record);
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
    }

    #[test]
    fn receipt_requires_exact_losing_tips_and_matching_frontier() {
        let proposal = checked_proposal();
        let grant = member(1, 1, MemberRole::ReadWrite);
        let missing_tip = &tips()[..1];
        assert_eq!(
            encode_signed_freeze_receipt(&key(1), &proposal, &grant, &frontier(), missing_tip),
            Err(FreezeCodecError::LosingTipSetMismatch)
        );
        let wrong_counter = vec![
            WriterCutoff::new(writer(2), 4, [4; 32]).expect("tip"),
            WriterCutoff::new(writer(3), 0, [0; 32]).expect("tip"),
        ];
        assert_eq!(
            encode_signed_freeze_receipt(&key(1), &proposal, &grant, &frontier(), &wrong_counter,),
            Err(FreezeCodecError::LosingTipFrontierMismatch)
        );
        let unexpected_zero_actor = vec![
            ClockEntry::new(writer(1), 10).expect("clock"),
            ClockEntry::new(writer(2), 5).expect("clock"),
            ClockEntry::new(writer(3), 1).expect("clock"),
        ];
        assert_eq!(
            encode_signed_freeze_receipt(
                &key(1),
                &proposal,
                &grant,
                &unexpected_zero_actor,
                &tips(),
            ),
            Err(FreezeCodecError::LosingTipFrontierMismatch)
        );
        let noncanonical_frontier = vec![
            ClockEntry::new(writer(2), 5).expect("clock"),
            ClockEntry::new(writer(1), 10).expect("clock"),
        ];
        assert_eq!(
            encode_signed_freeze_receipt(
                &key(1),
                &proposal,
                &grant,
                &noncanonical_frontier,
                &tips(),
            ),
            Err(FreezeCodecError::NonCanonicalFrontier)
        );
        let mut noncanonical_tips = tips();
        noncanonical_tips.reverse();
        assert_eq!(
            encode_signed_freeze_receipt(
                &key(1),
                &proposal,
                &grant,
                &frontier(),
                &noncanonical_tips,
            ),
            Err(FreezeCodecError::NonCanonicalTips)
        );
    }

    #[test]
    fn downgraded_read_only_survivor_may_sign_its_freeze_receipt() {
        let proposal = checked_proposal();
        let grant = member(2, 2, MemberRole::Read);
        let record = encode_signed_freeze_receipt(&key(2), &proposal, &grant, &frontier(), &tips())
            .expect("downgraded survivor receipt");
        let checked = decode_signature_checked_freeze_receipt(&record, &proposal, &grant)
            .expect("checked downgraded survivor receipt");
        assert_eq!(checked.signer_writer_id(), writer(2));
    }

    #[test]
    fn receipt_rejects_wrong_or_non_survivor_signer() {
        let proposal = checked_proposal();
        let grant = member(1, 1, MemberRole::ReadWrite);
        assert_eq!(
            encode_signed_freeze_receipt(&key(2), &proposal, &grant, &frontier(), &tips()),
            Err(FreezeCodecError::SurvivorKeyMismatch)
        );
        assert_eq!(
            encode_signed_freeze_receipt(
                &key(3),
                &proposal,
                &member(3, 3, MemberRole::ReadWrite),
                &frontier(),
                &tips(),
            ),
            Err(FreezeCodecError::SignerNotSurvivor)
        );
    }

    #[test]
    fn receipt_rejects_each_wrong_binding_and_signature() {
        let proposal = checked_proposal();
        let grant = member(1, 1, MemberRole::ReadWrite);
        let record = receipt_record(&proposal);
        for (offset, expected) in [
            (
                RECEIPT_PROPOSAL_DIGEST_OFFSET,
                FreezeCodecError::ProposalDigestMismatch,
            ),
            (
                RECEIPT_FOLDER_OFFSET,
                FreezeCodecError::ReceiptFolderMismatch,
            ),
            (
                RECEIPT_BASE_EPOCH_OFFSET,
                FreezeCodecError::ReceiptBaseEpochMismatch,
            ),
            (
                RECEIPT_BASE_DIGEST_OFFSET,
                FreezeCodecError::ReceiptBaseDigestMismatch,
            ),
            (
                RECEIPT_SIGNER_OFFSET,
                FreezeCodecError::ReceiptSignerMismatch,
            ),
        ] {
            let mut changed = record.clone();
            changed[offset] ^= 1;
            resign(&mut changed, &key(1), FREEZE_RECEIPT_SIGNATURE_DOMAIN);
            assert_eq!(
                decode_signature_checked_freeze_receipt(&changed, &proposal, &grant),
                Err(expected)
            );
        }
        let mut signature = record;
        *signature.last_mut().expect("signature") ^= 1;
        assert_eq!(
            decode_signature_checked_freeze_receipt(&signature, &proposal, &grant),
            Err(FreezeCodecError::InvalidSurvivorSignature)
        );
        for offset in [RECEIPT_FRONTIER_OFFSET, RECEIPT_TIPS_OFFSET + 24] {
            let mut changed = receipt_record(&proposal);
            changed[offset] ^= 1;
            assert!(
                decode_signature_checked_freeze_receipt(&changed, &proposal, &grant).is_err(),
                "offset {offset}"
            );
        }
    }

    #[test]
    fn abort_round_trip_binds_exact_proposal_and_authority_key() {
        let proposal = checked_proposal();
        let record = encode_signed_freeze_abort(&key(1), &proposal).expect("abort");
        let checked =
            decode_signature_checked_freeze_abort(&record, &proposal, &key(1).verifying_key())
                .expect("checked abort");
        assert_eq!(checked.proposal_digest(), proposal.digest());
        assert_eq!(checked.folder_id(), proposal.folder_id());
        assert_eq!(checked.base_epoch(), proposal.base_epoch());
        assert_eq!(checked.base_epoch_digest(), proposal.base_epoch_digest());
        assert_eq!(
            checked.authority_writer_id(),
            proposal.authority_writer_id()
        );
        assert_eq!(checked.canonical_record(), record);
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        assert_eq!(
            encode_signed_freeze_abort(&key(9), &proposal),
            Err(FreezeCodecError::AbortAuthorityKeyMismatch)
        );
        let mut foreign_signed = record;
        resign(&mut foreign_signed, &key(9), FREEZE_ABORT_SIGNATURE_DOMAIN);
        assert_eq!(
            decode_signature_checked_freeze_abort(
                &foreign_signed,
                &proposal,
                &key(9).verifying_key(),
            ),
            Err(FreezeCodecError::AbortAuthorityKeyMismatch)
        );
    }

    #[test]
    fn abort_rejects_each_wrong_binding_signature_and_domain() {
        let proposal = checked_proposal();
        let record = encode_signed_freeze_abort(&key(1), &proposal).expect("abort");
        for (offset, expected) in [
            (
                ABORT_PROPOSAL_DIGEST_OFFSET,
                FreezeCodecError::AbortProposalDigestMismatch,
            ),
            (ABORT_FOLDER_OFFSET, FreezeCodecError::AbortFolderMismatch),
            (
                ABORT_BASE_EPOCH_OFFSET,
                FreezeCodecError::AbortBaseEpochMismatch,
            ),
            (
                ABORT_BASE_DIGEST_OFFSET,
                FreezeCodecError::AbortBaseDigestMismatch,
            ),
            (
                ABORT_AUTHORITY_OFFSET,
                FreezeCodecError::AbortAuthorityMismatch,
            ),
        ] {
            let mut changed = record.clone();
            changed[offset] ^= 1;
            resign(&mut changed, &key(1), FREEZE_ABORT_SIGNATURE_DOMAIN);
            assert_eq!(
                decode_signature_checked_freeze_abort(&changed, &proposal, &key(1).verifying_key(),),
                Err(expected)
            );
        }
        let mut wrong_domain = record;
        resign(&mut wrong_domain, &key(1), PROPOSAL_SIGNATURE_DOMAIN);
        assert_eq!(
            decode_signature_checked_freeze_abort(
                &wrong_domain,
                &proposal,
                &key(1).verifying_key(),
            ),
            Err(FreezeCodecError::InvalidAbortSignature)
        );
    }

    #[test]
    fn all_record_decoders_reject_envelope_errors_before_unbounded_work() {
        let proposal = checked_proposal();
        let grant = member(1, 1, MemberRole::ReadWrite);
        let records = [
            proposal_record(),
            receipt_record(&proposal),
            encode_signed_freeze_abort(&key(1), &proposal).expect("abort"),
        ];
        for (index, record) in records.iter().enumerate() {
            let mut magic = record.clone();
            magic[0] ^= 1;
            let mut version = record.clone();
            version[9] = 2;
            let mut flags = record.clone();
            flags[10] = 1;
            let mut trailing = record.clone();
            trailing.push(0);
            let oversized = vec![0; MAX_SIGNED_FREEZE_RECORD_BYTES + 1];
            let decode = |bytes: &[u8]| match index {
                0 => decode_proposal(bytes).map(|_| ()),
                1 => decode_signature_checked_freeze_receipt(bytes, &proposal, &grant).map(|_| ()),
                _ => {
                    decode_signature_checked_freeze_abort(bytes, &proposal, &key(1).verifying_key())
                        .map(|_| ())
                }
            };
            assert_eq!(decode(&magic), Err(FreezeCodecError::InvalidMagic));
            assert_eq!(decode(&version), Err(FreezeCodecError::UnsupportedVersion));
            assert_eq!(decode(&flags), Err(FreezeCodecError::UnknownFlags));
            assert_eq!(decode(&trailing), Err(FreezeCodecError::TrailingBytes));
            for end in 0..record.len() {
                assert!(
                    decode(&record[..end]).is_err(),
                    "record {index}, prefix {end}"
                );
            }
            assert_eq!(decode(&oversized), Err(FreezeCodecError::RecordTooLarge));
        }

        let mut hostile_count = proposal_record();
        hostile_count[PROPOSAL_CHANGES_COUNT_OFFSET..PROPOSAL_CHANGES_COUNT_OFFSET + 2]
            .copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            decode_proposal(&hostile_count),
            Err(FreezeCodecError::TooManyEntries)
        );
    }

    #[test]
    fn signatures_cannot_cross_freeze_record_domains() {
        let mut proposal_record = proposal_record();
        resign(&mut proposal_record, &key(1), FREEZE_ABORT_SIGNATURE_DOMAIN);
        assert_eq!(
            decode_proposal(&proposal_record),
            Err(FreezeCodecError::InvalidAuthoritySignature)
        );

        let proposal = checked_proposal();
        let grant = member(1, 1, MemberRole::ReadWrite);
        let mut receipt = receipt_record(&proposal);
        resign(&mut receipt, &key(1), PROPOSAL_SIGNATURE_DOMAIN);
        assert_eq!(
            decode_signature_checked_freeze_receipt(&receipt, &proposal, &grant),
            Err(FreezeCodecError::InvalidSurvivorSignature)
        );
    }

    #[test]
    fn maximum_structurally_valid_sets_remain_below_fixed_record_cap() {
        let changes: Vec<_> = (2..=128)
            .map(|id| WriteLossChange::new(writer(id), WriteLossAction::DowngradeToRead))
            .collect();
        let survivors: Vec<_> = (1..=128).map(writer).collect();
        let losing: Vec<_> = (2..=128).map(writer).collect();
        let record = encode_signed_write_loss_proposal(
            &key(1),
            folder(7),
            4,
            EpochDigest::from_bytes([8; 32]),
            writer(1),
            [9; 32],
            &changes,
            &survivors,
            &losing,
        )
        .expect("max proposal");
        assert_eq!(record.len(), 6_424);
        let proposal = decode_proposal(&record).expect("max checked proposal");

        let frontier: Vec<_> = (1..=128)
            .map(|id| ClockEntry::new(writer(id), 1).expect("clock"))
            .collect();
        let tips: Vec<_> = (2..=128)
            .map(|id| WriterCutoff::new(writer(id), 1, [id as u8; 32]).expect("tip"))
            .collect();
        let receipt = encode_signed_freeze_receipt(
            &key(1),
            &proposal,
            &member(1, 1, MemberRole::ReadWrite),
            &frontier,
            &tips,
        )
        .expect("max receipt");
        assert_eq!(receipt.len(), 10_367);
        assert!(receipt.len() < MAX_SIGNED_FREEZE_RECORD_BYTES);

        let too_many: Vec<_> = (1..=129).map(writer).collect();
        assert_eq!(
            encode_signed_write_loss_proposal(
                &key(1),
                folder(7),
                4,
                EpochDigest::from_bytes([8; 32]),
                writer(1),
                [9; 32],
                &changes,
                &too_many,
                &losing,
            ),
            Err(FreezeCodecError::TooManyEntries)
        );
    }
}
