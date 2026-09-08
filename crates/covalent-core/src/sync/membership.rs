//! Canonical signed membership-epoch records for direct folder synchronization.
//!
//! This codec authenticates one bounded epoch snapshot under a separately
//! pinned authority key. A [`SignatureCheckedEpoch`] is not an authorized or
//! accepted transition. A future validator must still verify a contiguous
//! epoch chain, exact prior digest, roster changes, bootstrap receipts,
//! survivor freeze receipts, proposal binding, causal closure, and durable
//! persistence before replacing a membership head or serving folder data.
//!
//! Version 1 uses this exact big-endian order: `COVSEP01` magic, `u16` version,
//! `u8` flags, folder UUID, positive `u64` epoch, authority writer UUID,
//! optional previous signed-epoch digest, roster count and fixed roster entries,
//! optional write-loss proposal digest, accepted-frontier count and entries,
//! cutoff count and entries, freeze-receipt count and digests,
//! bootstrap-receipt count and digests, then the authority signature. Roster,
//! frontier, and cutoff entries sort by writer UUID bytes; digest lists sort by
//! digest bytes. All counts are `u16`, all integers are big-endian, and keys,
//! UUIDs, and digests have fixed widths.

use std::collections::BTreeSet;
use std::fmt;

use covalent_protocol::DeviceId;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use thiserror::Error;
use uuid::Uuid;

use super::ids::{FolderId, WriterId};
use super::operation::ClockEntry;
use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector};

const MAGIC: &[u8; 8] = b"COVSEP01";
const WIRE_VERSION: u16 = 1;
const PREVIOUS_PRESENT: u8 = 1;
const WRITE_LOSS_PROOF_PRESENT: u8 = 2;
const KNOWN_FLAGS: u8 = PREVIOUS_PRESENT | WRITE_LOSS_PROOF_PRESENT;
const SIGNATURE_BYTES: usize = 64;
const DIGEST_BYTES: usize = 32;
const ROSTER_ENTRY_BYTES: usize = 16 + 32 + 16 + 32 + 1;
const FRONTIER_ENTRY_BYTES: usize = 16 + 8;
const CUTOFF_ENTRY_BYTES: usize = 16 + 8 + 32;
const FIXED_UNSIGNED_BYTES: usize = 8 + 2 + 1 + 16 + 8 + 16 + 2 + 2 + 2 + 2 + 2;

/// Domain prefix signed before a canonical unsigned epoch record.
pub const EPOCH_SIGNATURE_DOMAIN: &[u8] = b"covalent/sync-membership-epoch-signature/v1\0";
/// Maximum roster members, frontier actors, cutoffs, and each receipt list.
pub const MAX_EPOCH_COLLECTION_ENTRIES: usize = MAX_VERSION_VECTOR_ACTORS;
/// Maximum complete signed epoch record, checked before parsing.
pub const MAX_SIGNED_EPOCH_BYTES: usize = 32 * 1_024;

/// BLAKE3 commitment to one complete canonical signed epoch record.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EpochDigest([u8; DIGEST_BYTES]);

impl EpochDigest {
    /// Constructs a reference to an already retained exact signed epoch.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    /// Returns the fixed digest bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; DIGEST_BYTES] {
        self.0
    }
}

/// A folder member's data capability. Membership authority is separate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MemberRole {
    /// May receive state and content from an authorized peer.
    Read = 1,
    /// May receive data and publish signed operations.
    ReadWrite = 2,
}

impl MemberRole {
    fn from_byte(value: u8) -> Result<Self, MembershipCodecError> {
        match value {
            1 => Ok(Self::Read),
            2 => Ok(Self::ReadWrite),
            _ => Err(MembershipCodecError::UnknownRole),
        }
    }
}

/// One authority-signed writer and paired transport binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberGrant {
    writer_id: WriterId,
    writer_key: VerifyingKey,
    transport_device_id: DeviceId,
    transport_key: VerifyingKey,
    role: MemberRole,
}

impl MemberGrant {
    /// Builds a structurally valid member binding from public keys.
    ///
    /// Record construction separately enforces roster-wide uniqueness and the
    /// pinned authority constraint.
    pub fn new(
        writer_id: WriterId,
        writer_key: VerifyingKey,
        transport_device_id: DeviceId,
        transport_key: VerifyingKey,
        role: MemberRole,
    ) -> Result<Self, MembershipCodecError> {
        if writer_key.is_weak() {
            return Err(MembershipCodecError::WeakWriterKey);
        }
        if transport_key.is_weak() {
            return Err(MembershipCodecError::WeakTransportKey);
        }
        if writer_key.to_bytes() == transport_key.to_bytes() {
            return Err(MembershipCodecError::ReusedRosterKey);
        }
        Ok(Self {
            writer_id,
            writer_key,
            transport_device_id,
            transport_key,
            role,
        })
    }

    /// Returns the distinct per-install writer identifier.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the operation-verification key bound to the writer.
    #[must_use]
    pub const fn writer_key(&self) -> &VerifyingKey {
        &self.writer_key
    }

    /// Returns the exact paired transport device identity.
    #[must_use]
    pub const fn transport_device_id(&self) -> DeviceId {
        self.transport_device_id
    }

    /// Returns the paired transport verification key.
    #[must_use]
    pub const fn transport_key(&self) -> &VerifyingKey {
        &self.transport_key
    }

    /// Returns the member's data capability.
    #[must_use]
    pub const fn role(&self) -> MemberRole {
        self.role
    }
}

/// Exact terminal operation tip for a removed or downgraded writer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterCutoff {
    writer_id: WriterId,
    counter: u64,
    operation_digest: [u8; DIGEST_BYTES],
}

impl WriterCutoff {
    /// Builds one canonical cutoff.
    ///
    /// A writer with no operation uses counter zero and the all-zero digest.
    /// Positive counters accept any digest bytes; digest semantics belong to
    /// the transition validator and retained operation history.
    pub fn new(
        writer_id: WriterId,
        counter: u64,
        operation_digest: [u8; DIGEST_BYTES],
    ) -> Result<Self, MembershipCodecError> {
        if counter == 0 && operation_digest != [0; DIGEST_BYTES] {
            return Err(MembershipCodecError::InvalidZeroCutoff);
        }
        Ok(Self {
            writer_id,
            counter,
            operation_digest,
        })
    }

    /// Returns the losing writer.
    #[must_use]
    pub const fn writer_id(self) -> WriterId {
        self.writer_id
    }

    /// Returns zero for no retained operation, otherwise the exact tip counter.
    #[must_use]
    pub const fn counter(self) -> u64 {
        self.counter
    }

    /// Returns the exact tip digest, or all zero at counter zero.
    #[must_use]
    pub const fn operation_digest(self) -> [u8; DIGEST_BYTES] {
        self.operation_digest
    }
}

/// A canonical epoch whose bytes passed strict authority-signature checking.
///
/// This value does not prove that the authority was entitled to sign a
/// transition, that evidence digests resolve to valid receipts, or that the
/// record is the next durable epoch.
#[derive(Clone, Eq, PartialEq)]
pub struct SignatureCheckedEpoch {
    folder_id: FolderId,
    epoch: u64,
    authority_writer_id: WriterId,
    previous_epoch_digest: Option<EpochDigest>,
    roster: Vec<MemberGrant>,
    proposal_digest: Option<[u8; DIGEST_BYTES]>,
    accepted_frontier_entries: Vec<ClockEntry>,
    accepted_frontier: VersionVector,
    cutoffs: Vec<WriterCutoff>,
    freeze_receipt_digests: Vec<[u8; DIGEST_BYTES]>,
    bootstrap_receipt_digests: Vec<[u8; DIGEST_BYTES]>,
    canonical_record: Vec<u8>,
    digest: EpochDigest,
}

impl fmt::Debug for SignatureCheckedEpoch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SignatureCheckedEpoch")
            .field("folder_id", &self.folder_id)
            .field("epoch", &self.epoch)
            .field("authority_writer_id", &self.authority_writer_id)
            .field("previous_epoch_digest", &self.previous_epoch_digest)
            .field("roster_count", &self.roster.len())
            .field(
                "accepted_frontier_count",
                &self.accepted_frontier_entries.len(),
            )
            .field("cutoff_count", &self.cutoffs.len())
            .field("freeze_receipt_count", &self.freeze_receipt_digests.len())
            .field(
                "bootstrap_receipt_count",
                &self.bootstrap_receipt_digests.len(),
            )
            .field("record_length", &self.canonical_record.len())
            .field("digest", &self.digest)
            .finish()
    }
}

impl SignatureCheckedEpoch {
    /// Returns the folder committed by the signature.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the positive claimed epoch number.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Returns the authority writer pinned by the genesis workflow.
    #[must_use]
    pub const fn authority_writer_id(&self) -> WriterId {
        self.authority_writer_id
    }

    /// Returns the exact prior signed-record commitment, absent only at genesis.
    #[must_use]
    pub const fn previous_epoch_digest(&self) -> Option<EpochDigest> {
        self.previous_epoch_digest
    }

    /// Returns the full sorted membership snapshot.
    #[must_use]
    pub fn roster(&self) -> &[MemberGrant] {
        &self.roster
    }

    /// Returns the write-loss proposal commitment when that proof is present.
    #[must_use]
    pub const fn proposal_digest(&self) -> Option<[u8; DIGEST_BYTES]> {
        self.proposal_digest
    }

    /// Returns writer-typed components of the accepted cutoff frontier.
    #[must_use]
    pub fn accepted_frontier_entries(&self) -> &[ClockEntry] {
        &self.accepted_frontier_entries
    }

    /// Returns the accepted cutoff frontier in the causal-math representation.
    #[must_use]
    pub const fn accepted_frontier(&self) -> &VersionVector {
        &self.accepted_frontier
    }

    /// Returns sorted losing-writer tips for a write-loss transition.
    #[must_use]
    pub fn cutoffs(&self) -> &[WriterCutoff] {
        &self.cutoffs
    }

    /// Returns the complete sorted freeze-receipt commitment list.
    #[must_use]
    pub fn freeze_receipt_digests(&self) -> &[[u8; DIGEST_BYTES]] {
        &self.freeze_receipt_digests
    }

    /// Returns the complete sorted bootstrap-receipt commitment list.
    #[must_use]
    pub fn bootstrap_receipt_digests(&self) -> &[[u8; DIGEST_BYTES]] {
        &self.bootstrap_receipt_digests
    }

    /// Returns the exact canonical signed record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        &self.canonical_record
    }

    /// Returns the commitment to the complete record, including its signature.
    #[must_use]
    pub const fn digest(&self) -> EpochDigest {
        self.digest
    }
}

/// Structural, canonicality, binding, or signature failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MembershipCodecError {
    /// The record exceeds its pre-parse limit.
    #[error("signed membership epoch exceeds the record limit")]
    RecordTooLarge,
    /// Length arithmetic could not be represented.
    #[error("signed membership epoch length overflow")]
    LengthOverflow,
    /// The record ended before a complete field could be read.
    #[error("signed membership epoch is truncated")]
    Truncated,
    /// Bytes followed the one canonical record.
    #[error("signed membership epoch has trailing bytes")]
    TrailingBytes,
    /// The wire magic is not the membership-epoch domain.
    #[error("invalid signed membership-epoch magic")]
    InvalidMagic,
    /// The wire version is not supported.
    #[error("unsupported signed membership-epoch version")]
    UnsupportedVersion,
    /// A reserved flag bit was set.
    #[error("signed membership epoch contains unknown flags")]
    UnknownFlags,
    /// Epoch zero is not valid.
    #[error("membership epoch must be positive")]
    ZeroEpoch,
    /// Genesis or a later epoch used the wrong predecessor shape.
    #[error("membership epoch predecessor does not match its epoch number")]
    InvalidPredecessor,
    /// Genesis contained members other than its pinned authority.
    #[error("membership genesis roster must contain only its authority")]
    InvalidGenesisRoster,
    /// One collection exceeds the 128-entry bound.
    #[error("membership epoch collection has too many entries")]
    TooManyEntries,
    /// The roster is empty or does not contain its named authority.
    #[error("membership epoch authority is absent from its roster")]
    MissingAuthority,
    /// The named authority is not a surviving read-write participant.
    #[error("membership epoch authority must remain read-write")]
    AuthorityNotReadWrite,
    /// The authority roster key differs from the separately pinned key.
    #[error("membership epoch authority key does not match the pinned key")]
    AuthorityKeyMismatch,
    /// A roster writer key is not a valid Ed25519 point.
    #[error("membership epoch contains an invalid writer key")]
    InvalidWriterKey,
    /// A roster writer key is weak.
    #[error("membership epoch contains a weak writer key")]
    WeakWriterKey,
    /// A transport key is not a valid Ed25519 point.
    #[error("membership epoch contains an invalid transport key")]
    InvalidTransportKey,
    /// A transport key is weak.
    #[error("membership epoch contains a weak transport key")]
    WeakTransportKey,
    /// The separately pinned authority key is weak.
    #[error("membership epoch authority key is weak")]
    WeakAuthorityKey,
    /// A key was reused across writer or transport bindings.
    #[error("membership epoch reuses a roster key")]
    ReusedRosterKey,
    /// A transport device identity was reused by multiple members.
    #[error("membership epoch reuses a transport device identity")]
    ReusedTransportIdentity,
    /// Roster writer IDs were duplicated or out of canonical order.
    #[error("membership epoch roster is not in canonical writer order")]
    NonCanonicalRoster,
    /// A role byte is unknown.
    #[error("membership epoch contains an unknown member role")]
    UnknownRole,
    /// A typed transport device ID could not be represented as UUID bytes.
    #[error("membership epoch contains an invalid transport device identifier")]
    InvalidTransportDeviceId,
    /// A frontier counter used zero rather than omission.
    #[error("membership epoch frontier counters must be positive")]
    ZeroFrontierCounter,
    /// Frontier actors were duplicated or out of canonical order.
    #[error("membership epoch frontier is not in canonical writer order")]
    NonCanonicalFrontier,
    /// Cutoff writers were duplicated or out of canonical order.
    #[error("membership epoch cutoffs are not in canonical writer order")]
    NonCanonicalCutoffs,
    /// A zero cutoff counter did not use the reserved all-zero digest.
    #[error("zero membership cutoff must use the all-zero digest")]
    InvalidZeroCutoff,
    /// A receipt digest list was duplicated or out of canonical order.
    #[error("membership epoch receipt digests are not canonical")]
    NonCanonicalReceipts,
    /// Proposal, frontier, cutoff, and freeze-receipt fields disagree.
    #[error("membership epoch write-loss proof has an invalid structural shape")]
    InvalidWriteLossProof,
    /// Bootstrap and write-loss evidence were combined in one epoch.
    #[error("membership epoch cannot combine bootstrap and write-loss evidence")]
    MixedTransitionEvidence,
    /// The record belongs to another expected folder.
    #[error("membership epoch folder binding does not match")]
    FolderMismatch,
    /// The record names another expected pinned authority writer.
    #[error("membership epoch authority writer binding does not match")]
    AuthorityWriterMismatch,
    /// Strict Ed25519 verification failed.
    #[error("membership epoch authority signature is invalid")]
    InvalidSignature,
}

/// Encodes and authority-signs one canonical epoch snapshot.
///
/// The caller supplies an already generated key and canonical arrays. This
/// function performs no entropy, persistence, transition acceptance, or
/// membership authorization. Bounds are checked before record allocation.
#[allow(clippy::too_many_arguments)]
pub fn encode_signed_epoch(
    authority_signing_key: &SigningKey,
    folder_id: FolderId,
    epoch: u64,
    authority_writer_id: WriterId,
    previous_epoch_digest: Option<EpochDigest>,
    roster: &[MemberGrant],
    proposal_digest: Option<[u8; DIGEST_BYTES]>,
    accepted_frontier: &[ClockEntry],
    cutoffs: &[WriterCutoff],
    freeze_receipt_digests: &[[u8; DIGEST_BYTES]],
    bootstrap_receipt_digests: &[[u8; DIGEST_BYTES]],
) -> Result<Vec<u8>, MembershipCodecError> {
    validate_counts(
        roster.len(),
        accepted_frontier.len(),
        cutoffs.len(),
        freeze_receipt_digests.len(),
        bootstrap_receipt_digests.len(),
    )?;
    let unsigned_len = encoded_unsigned_len(
        previous_epoch_digest.is_some(),
        proposal_digest.is_some(),
        roster.len(),
        accepted_frontier.len(),
        cutoffs.len(),
        freeze_receipt_digests.len(),
        bootstrap_receipt_digests.len(),
    )?;
    let record_len = unsigned_len
        .checked_add(SIGNATURE_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    if record_len > MAX_SIGNED_EPOCH_BYTES {
        return Err(MembershipCodecError::RecordTooLarge);
    }

    let authority_key = authority_signing_key.verifying_key();
    if authority_key.is_weak() {
        return Err(MembershipCodecError::WeakAuthorityKey);
    }
    validate_epoch_shape(
        epoch,
        previous_epoch_digest,
        roster.len(),
        proposal_digest,
        accepted_frontier,
        cutoffs,
        freeze_receipt_digests,
        bootstrap_receipt_digests,
    )?;
    validate_roster(roster)?;
    validate_authority_member(roster, authority_writer_id, &authority_key)?;
    validate_frontier(accepted_frontier)?;
    validate_cutoffs(cutoffs)?;
    validate_digest_list(freeze_receipt_digests)?;
    validate_digest_list(bootstrap_receipt_digests)?;

    let mut record = Vec::with_capacity(record_len);
    encode_unsigned(
        &mut record,
        folder_id,
        epoch,
        authority_writer_id,
        previous_epoch_digest,
        roster,
        proposal_digest,
        accepted_frontier,
        cutoffs,
        freeze_receipt_digests,
        bootstrap_receipt_digests,
    )?;
    let signature = authority_signing_key.sign(&signature_message(&record));
    record.extend_from_slice(&signature.to_bytes());
    Ok(record)
}

/// Parses canonical bytes and strictly verifies the separately pinned authority.
///
/// The record length cap is checked before any parse-time allocation. The
/// returned value still requires complete external transition validation.
pub fn decode_signature_checked_epoch(
    record: &[u8],
    expected_folder: FolderId,
    expected_authority_writer: WriterId,
    pinned_authority_key: &VerifyingKey,
) -> Result<SignatureCheckedEpoch, MembershipCodecError> {
    if record.len() > MAX_SIGNED_EPOCH_BYTES {
        return Err(MembershipCodecError::RecordTooLarge);
    }
    if pinned_authority_key.is_weak() {
        return Err(MembershipCodecError::WeakAuthorityKey);
    }

    let mut cursor = Cursor::new(record);
    if cursor.take_array::<8>()? != *MAGIC {
        return Err(MembershipCodecError::InvalidMagic);
    }
    if cursor.take_u16()? != WIRE_VERSION {
        return Err(MembershipCodecError::UnsupportedVersion);
    }
    let flags = cursor.take_u8()?;
    if flags & !KNOWN_FLAGS != 0 {
        return Err(MembershipCodecError::UnknownFlags);
    }
    let folder_id = FolderId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let epoch = cursor.take_u64()?;
    let authority_writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
    let previous_epoch_digest = if flags & PREVIOUS_PRESENT != 0 {
        Some(EpochDigest::from_bytes(
            cursor.take_array::<DIGEST_BYTES>()?,
        ))
    } else {
        None
    };

    let roster_count = usize::from(cursor.take_u16()?);
    ensure_count(roster_count)?;
    let mut roster = Vec::with_capacity(roster_count);
    for _ in 0..roster_count {
        let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let writer_key = parse_writer_key(cursor.take_array::<32>()?)?;
        let transport_device_id = DeviceId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let transport_key = parse_transport_key(cursor.take_array::<32>()?)?;
        let role = MemberRole::from_byte(cursor.take_u8()?)?;
        roster.push(MemberGrant::new(
            writer_id,
            writer_key,
            transport_device_id,
            transport_key,
            role,
        )?);
    }

    let proposal_digest = if flags & WRITE_LOSS_PROOF_PRESENT != 0 {
        Some(cursor.take_array::<DIGEST_BYTES>()?)
    } else {
        None
    };
    let frontier_count = usize::from(cursor.take_u16()?);
    ensure_count(frontier_count)?;
    let mut accepted_frontier_entries = Vec::with_capacity(frontier_count);
    for _ in 0..frontier_count {
        let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let counter = cursor.take_u64()?;
        let entry = ClockEntry::new(writer_id, counter)
            .map_err(|_| MembershipCodecError::ZeroFrontierCounter)?;
        accepted_frontier_entries.push(entry);
    }

    let cutoff_count = usize::from(cursor.take_u16()?);
    ensure_count(cutoff_count)?;
    let mut cutoffs = Vec::with_capacity(cutoff_count);
    for _ in 0..cutoff_count {
        let writer_id = WriterId::from_uuid(Uuid::from_bytes(cursor.take_array::<16>()?));
        let counter = cursor.take_u64()?;
        let digest = cursor.take_array::<DIGEST_BYTES>()?;
        cutoffs.push(WriterCutoff::new(writer_id, counter, digest)?);
    }

    let freeze_count = usize::from(cursor.take_u16()?);
    ensure_count(freeze_count)?;
    let mut freeze_receipt_digests = Vec::with_capacity(freeze_count);
    for _ in 0..freeze_count {
        freeze_receipt_digests.push(cursor.take_array::<DIGEST_BYTES>()?);
    }
    let bootstrap_count = usize::from(cursor.take_u16()?);
    ensure_count(bootstrap_count)?;
    let mut bootstrap_receipt_digests = Vec::with_capacity(bootstrap_count);
    for _ in 0..bootstrap_count {
        bootstrap_receipt_digests.push(cursor.take_array::<DIGEST_BYTES>()?);
    }

    let unsigned_len = cursor.position();
    let signature_bytes = cursor.take_array::<SIGNATURE_BYTES>()?;
    if cursor.remaining() != 0 {
        return Err(MembershipCodecError::TrailingBytes);
    }

    validate_epoch_shape(
        epoch,
        previous_epoch_digest,
        roster.len(),
        proposal_digest,
        &accepted_frontier_entries,
        &cutoffs,
        &freeze_receipt_digests,
        &bootstrap_receipt_digests,
    )?;
    validate_roster(&roster)?;
    validate_frontier(&accepted_frontier_entries)?;
    validate_cutoffs(&cutoffs)?;
    validate_digest_list(&freeze_receipt_digests)?;
    validate_digest_list(&bootstrap_receipt_digests)?;
    let signature = Signature::from_bytes(&signature_bytes);
    pinned_authority_key
        .verify_strict(&signature_message(&record[..unsigned_len]), &signature)
        .map_err(|_| MembershipCodecError::InvalidSignature)?;
    if folder_id != expected_folder {
        return Err(MembershipCodecError::FolderMismatch);
    }
    if authority_writer_id != expected_authority_writer {
        return Err(MembershipCodecError::AuthorityWriterMismatch);
    }
    validate_authority_member(&roster, authority_writer_id, pinned_authority_key)?;

    let accepted_frontier = VersionVector::new(
        accepted_frontier_entries
            .iter()
            .map(|entry| (entry.writer_id().into_vector_actor(), entry.counter())),
    )
    .map_err(|_| MembershipCodecError::NonCanonicalFrontier)?;
    let canonical_record = record.to_vec();
    let digest = EpochDigest(*blake3::hash(&canonical_record).as_bytes());
    Ok(SignatureCheckedEpoch {
        folder_id,
        epoch,
        authority_writer_id,
        previous_epoch_digest,
        roster,
        proposal_digest,
        accepted_frontier_entries,
        accepted_frontier,
        cutoffs,
        freeze_receipt_digests,
        bootstrap_receipt_digests,
        canonical_record,
        digest,
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_epoch_shape(
    epoch: u64,
    previous_epoch_digest: Option<EpochDigest>,
    roster_count: usize,
    proposal_digest: Option<[u8; DIGEST_BYTES]>,
    accepted_frontier: &[ClockEntry],
    cutoffs: &[WriterCutoff],
    freeze_receipt_digests: &[[u8; DIGEST_BYTES]],
    bootstrap_receipt_digests: &[[u8; DIGEST_BYTES]],
) -> Result<(), MembershipCodecError> {
    if epoch == 0 {
        return Err(MembershipCodecError::ZeroEpoch);
    }
    if (epoch == 1) != previous_epoch_digest.is_none() {
        return Err(MembershipCodecError::InvalidPredecessor);
    }
    if epoch == 1 {
        if roster_count != 1 {
            return Err(MembershipCodecError::InvalidGenesisRoster);
        }
        if proposal_digest.is_some()
            || !accepted_frontier.is_empty()
            || !cutoffs.is_empty()
            || !freeze_receipt_digests.is_empty()
            || !bootstrap_receipt_digests.is_empty()
        {
            return Err(MembershipCodecError::InvalidWriteLossProof);
        }
        return Ok(());
    }

    let has_write_loss = proposal_digest.is_some();
    if has_write_loss {
        if cutoffs.is_empty() || freeze_receipt_digests.is_empty() {
            return Err(MembershipCodecError::InvalidWriteLossProof);
        }
        if !bootstrap_receipt_digests.is_empty() {
            return Err(MembershipCodecError::MixedTransitionEvidence);
        }
    } else if !accepted_frontier.is_empty()
        || !cutoffs.is_empty()
        || !freeze_receipt_digests.is_empty()
    {
        return Err(MembershipCodecError::InvalidWriteLossProof);
    }
    Ok(())
}

fn validate_counts(
    roster_count: usize,
    frontier_count: usize,
    cutoff_count: usize,
    freeze_count: usize,
    bootstrap_count: usize,
) -> Result<(), MembershipCodecError> {
    for count in [
        roster_count,
        frontier_count,
        cutoff_count,
        freeze_count,
        bootstrap_count,
    ] {
        ensure_count(count)?;
    }
    Ok(())
}

fn ensure_count(count: usize) -> Result<(), MembershipCodecError> {
    if count > MAX_EPOCH_COLLECTION_ENTRIES {
        return Err(MembershipCodecError::TooManyEntries);
    }
    Ok(())
}

fn validate_roster(roster: &[MemberGrant]) -> Result<(), MembershipCodecError> {
    if roster.is_empty() {
        return Err(MembershipCodecError::MissingAuthority);
    }
    let mut previous: Option<[u8; 16]> = None;
    let mut transport_ids = BTreeSet::new();
    let mut all_keys = BTreeSet::new();
    for member in roster {
        if member.writer_key.is_weak() {
            return Err(MembershipCodecError::WeakWriterKey);
        }
        if member.transport_key.is_weak() {
            return Err(MembershipCodecError::WeakTransportKey);
        }
        let writer_bytes = member.writer_id.to_bytes();
        if previous.is_some_and(|prior| prior >= writer_bytes) {
            return Err(MembershipCodecError::NonCanonicalRoster);
        }
        previous = Some(writer_bytes);
        if !transport_ids.insert(member.transport_device_id) {
            return Err(MembershipCodecError::ReusedTransportIdentity);
        }
        if !all_keys.insert(member.writer_key.to_bytes())
            || !all_keys.insert(member.transport_key.to_bytes())
        {
            return Err(MembershipCodecError::ReusedRosterKey);
        }
    }
    Ok(())
}

fn validate_authority_member(
    roster: &[MemberGrant],
    authority_writer_id: WriterId,
    authority_key: &VerifyingKey,
) -> Result<(), MembershipCodecError> {
    let authority = roster
        .iter()
        .find(|member| member.writer_id == authority_writer_id)
        .ok_or(MembershipCodecError::MissingAuthority)?;
    if authority.role != MemberRole::ReadWrite {
        return Err(MembershipCodecError::AuthorityNotReadWrite);
    }
    if authority.writer_key != *authority_key {
        return Err(MembershipCodecError::AuthorityKeyMismatch);
    }
    Ok(())
}

fn validate_frontier(frontier: &[ClockEntry]) -> Result<(), MembershipCodecError> {
    let mut previous: Option<[u8; 16]> = None;
    for entry in frontier {
        if entry.counter() == 0 {
            return Err(MembershipCodecError::ZeroFrontierCounter);
        }
        let writer_bytes = entry.writer_id().to_bytes();
        if previous.is_some_and(|prior| prior >= writer_bytes) {
            return Err(MembershipCodecError::NonCanonicalFrontier);
        }
        previous = Some(writer_bytes);
    }
    Ok(())
}

fn validate_cutoffs(cutoffs: &[WriterCutoff]) -> Result<(), MembershipCodecError> {
    let mut previous: Option<[u8; 16]> = None;
    for cutoff in cutoffs {
        if cutoff.counter == 0 && cutoff.operation_digest != [0; DIGEST_BYTES] {
            return Err(MembershipCodecError::InvalidZeroCutoff);
        }
        let writer_bytes = cutoff.writer_id.to_bytes();
        if previous.is_some_and(|prior| prior >= writer_bytes) {
            return Err(MembershipCodecError::NonCanonicalCutoffs);
        }
        previous = Some(writer_bytes);
    }
    Ok(())
}

fn validate_digest_list(digests: &[[u8; DIGEST_BYTES]]) -> Result<(), MembershipCodecError> {
    if digests.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(MembershipCodecError::NonCanonicalReceipts);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encoded_unsigned_len(
    has_previous: bool,
    has_proposal: bool,
    roster_count: usize,
    frontier_count: usize,
    cutoff_count: usize,
    freeze_count: usize,
    bootstrap_count: usize,
) -> Result<usize, MembershipCodecError> {
    let roster_bytes = roster_count
        .checked_mul(ROSTER_ENTRY_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    let frontier_bytes = frontier_count
        .checked_mul(FRONTIER_ENTRY_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    let cutoff_bytes = cutoff_count
        .checked_mul(CUTOFF_ENTRY_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    let freeze_bytes = freeze_count
        .checked_mul(DIGEST_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    let bootstrap_bytes = bootstrap_count
        .checked_mul(DIGEST_BYTES)
        .ok_or(MembershipCodecError::LengthOverflow)?;
    FIXED_UNSIGNED_BYTES
        .checked_add(if has_previous { DIGEST_BYTES } else { 0 })
        .and_then(|length| length.checked_add(roster_bytes))
        .and_then(|length| length.checked_add(if has_proposal { DIGEST_BYTES } else { 0 }))
        .and_then(|length| length.checked_add(frontier_bytes))
        .and_then(|length| length.checked_add(cutoff_bytes))
        .and_then(|length| length.checked_add(freeze_bytes))
        .and_then(|length| length.checked_add(bootstrap_bytes))
        .ok_or(MembershipCodecError::LengthOverflow)
}

#[allow(clippy::too_many_arguments)]
fn encode_unsigned(
    output: &mut Vec<u8>,
    folder_id: FolderId,
    epoch: u64,
    authority_writer_id: WriterId,
    previous_epoch_digest: Option<EpochDigest>,
    roster: &[MemberGrant],
    proposal_digest: Option<[u8; DIGEST_BYTES]>,
    accepted_frontier: &[ClockEntry],
    cutoffs: &[WriterCutoff],
    freeze_receipt_digests: &[[u8; DIGEST_BYTES]],
    bootstrap_receipt_digests: &[[u8; DIGEST_BYTES]],
) -> Result<(), MembershipCodecError> {
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&WIRE_VERSION.to_be_bytes());
    let mut flags = 0;
    if previous_epoch_digest.is_some() {
        flags |= PREVIOUS_PRESENT;
    }
    if proposal_digest.is_some() {
        flags |= WRITE_LOSS_PROOF_PRESENT;
    }
    output.push(flags);
    output.extend_from_slice(&folder_id.to_bytes());
    output.extend_from_slice(&epoch.to_be_bytes());
    output.extend_from_slice(&authority_writer_id.to_bytes());
    if let Some(previous) = previous_epoch_digest {
        output.extend_from_slice(&previous.to_bytes());
    }
    output.extend_from_slice(&(roster.len() as u16).to_be_bytes());
    for member in roster {
        output.extend_from_slice(&member.writer_id.to_bytes());
        output.extend_from_slice(&member.writer_key.to_bytes());
        output.extend_from_slice(&transport_device_id_bytes(member.transport_device_id)?);
        output.extend_from_slice(&member.transport_key.to_bytes());
        output.push(member.role as u8);
    }
    if let Some(proposal) = proposal_digest {
        output.extend_from_slice(&proposal);
    }
    output.extend_from_slice(&(accepted_frontier.len() as u16).to_be_bytes());
    for entry in accepted_frontier {
        output.extend_from_slice(&entry.writer_id().to_bytes());
        output.extend_from_slice(&entry.counter().to_be_bytes());
    }
    output.extend_from_slice(&(cutoffs.len() as u16).to_be_bytes());
    for cutoff in cutoffs {
        output.extend_from_slice(&cutoff.writer_id.to_bytes());
        output.extend_from_slice(&cutoff.counter.to_be_bytes());
        output.extend_from_slice(&cutoff.operation_digest);
    }
    output.extend_from_slice(&(freeze_receipt_digests.len() as u16).to_be_bytes());
    for digest in freeze_receipt_digests {
        output.extend_from_slice(digest);
    }
    output.extend_from_slice(&(bootstrap_receipt_digests.len() as u16).to_be_bytes());
    for digest in bootstrap_receipt_digests {
        output.extend_from_slice(digest);
    }
    Ok(())
}

fn transport_device_id_bytes(device_id: DeviceId) -> Result<[u8; 16], MembershipCodecError> {
    Uuid::parse_str(&device_id.to_string())
        .map(Uuid::into_bytes)
        .map_err(|_| MembershipCodecError::InvalidTransportDeviceId)
}

fn parse_writer_key(bytes: [u8; 32]) -> Result<VerifyingKey, MembershipCodecError> {
    let key =
        VerifyingKey::from_bytes(&bytes).map_err(|_| MembershipCodecError::InvalidWriterKey)?;
    if key.is_weak() {
        return Err(MembershipCodecError::WeakWriterKey);
    }
    Ok(key)
}

fn parse_transport_key(bytes: [u8; 32]) -> Result<VerifyingKey, MembershipCodecError> {
    let key =
        VerifyingKey::from_bytes(&bytes).map_err(|_| MembershipCodecError::InvalidTransportKey)?;
    if key.is_weak() {
        return Err(MembershipCodecError::WeakTransportKey);
    }
    Ok(key)
}

fn signature_message(unsigned_record: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(EPOCH_SIGNATURE_DOMAIN.len() + unsigned_record.len());
    message.extend_from_slice(EPOCH_SIGNATURE_DOMAIN);
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

    fn take(&mut self, length: usize) -> Result<&'a [u8], MembershipCodecError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(MembershipCodecError::LengthOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(MembershipCodecError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], MembershipCodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| MembershipCodecError::Truncated)
    }

    fn take_u8(&mut self) -> Result<u8, MembershipCodecError> {
        Ok(self.take_array::<1>()?[0])
    }

    fn take_u16(&mut self) -> Result<u16, MembershipCodecError> {
        Ok(u16::from_be_bytes(self.take_array::<2>()?))
    }

    fn take_u64(&mut self) -> Result<u64, MembershipCodecError> {
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
    const EPOCH_OFFSET: usize = FOLDER_OFFSET + 16;
    const AUTHORITY_OFFSET: usize = EPOCH_OFFSET + 8;
    const GENESIS_ROSTER_COUNT_OFFSET: usize = AUTHORITY_OFFSET + 16;
    const GENESIS_ROSTER_ENTRY_OFFSET: usize = GENESIS_ROSTER_COUNT_OFFSET + 2;
    const WRITER_KEY_IN_ENTRY: usize = 16;
    const TRANSPORT_ID_IN_ENTRY: usize = 16 + 32;
    const TRANSPORT_KEY_IN_ENTRY: usize = 16 + 32 + 16;
    const ROLE_IN_ENTRY: usize = ROSTER_ENTRY_BYTES - 1;

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
        let mut bytes = [0x42; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        SigningKey::from_bytes(&bytes)
    }

    fn member(
        writer_number: u128,
        writer_key_number: u64,
        device_number: u128,
        transport_key_number: u64,
        role: MemberRole,
    ) -> MemberGrant {
        MemberGrant::new(
            writer(writer_number),
            key(writer_key_number).verifying_key(),
            device(device_number),
            key(transport_key_number).verifying_key(),
            role,
        )
        .expect("member")
    }

    fn genesis_record() -> Vec<u8> {
        encode_signed_epoch(
            &key(1),
            folder(7),
            1,
            writer(1),
            None,
            &[member(1, 1, 101, 2, MemberRole::ReadWrite)],
            None,
            &[],
            &[],
            &[],
            &[],
        )
        .expect("genesis")
    }

    fn decode(record: &[u8]) -> Result<SignatureCheckedEpoch, MembershipCodecError> {
        decode_signature_checked_epoch(record, folder(7), writer(1), &key(1).verifying_key())
    }

    fn resign(mut unsigned: Vec<u8>) -> Vec<u8> {
        let signature = key(1).sign(&signature_message(&unsigned));
        unsigned.extend_from_slice(&signature.to_bytes());
        unsigned
    }

    fn unsigned(record: &[u8]) -> Vec<u8> {
        record[..record.len() - SIGNATURE_BYTES].to_vec()
    }

    fn digest(number: u32) -> [u8; DIGEST_BYTES] {
        let mut value = [0; DIGEST_BYTES];
        value[DIGEST_BYTES - 4..].copy_from_slice(&number.to_be_bytes());
        value
    }

    #[test]
    fn genesis_round_trip_is_canonical_bounded_and_only_signature_checked() {
        let record = genesis_record();
        let checked = decode(&record).expect("checked genesis");

        assert_eq!(checked.canonical_record(), record);
        assert_eq!(checked.folder_id(), folder(7));
        assert_eq!(checked.epoch(), 1);
        assert_eq!(checked.authority_writer_id(), writer(1));
        assert_eq!(checked.previous_epoch_digest(), None);
        assert_eq!(checked.roster().len(), 1);
        assert_eq!(checked.roster()[0].writer_id(), writer(1));
        assert_eq!(checked.roster()[0].transport_device_id(), device(101));
        assert_eq!(checked.roster()[0].role(), MemberRole::ReadWrite);
        assert!(checked.accepted_frontier_entries().is_empty());
        assert_eq!(checked.accepted_frontier().actor_count(), 0);
        assert!(checked.cutoffs().is_empty());
        assert!(checked.freeze_receipt_digests().is_empty());
        assert!(checked.bootstrap_receipt_digests().is_empty());
        assert_eq!(
            checked.digest().to_bytes(),
            *blake3::hash(&record).as_bytes()
        );
        assert!(!format!("{checked:?}").contains("canonical_record"));
    }

    #[test]
    fn write_loss_proof_and_zero_tip_round_trip() {
        let roster = [
            member(1, 1, 101, 2, MemberRole::ReadWrite),
            member(2, 3, 102, 4, MemberRole::Read),
        ];
        let frontier = [
            ClockEntry::new(writer(1), 4).expect("frontier"),
            ClockEntry::new(writer(2), 2).expect("frontier"),
        ];
        let cutoffs = [
            WriterCutoff::new(writer(2), 0, [0; 32]).expect("zero tip"),
            WriterCutoff::new(writer(3), 7, [0; 32]).expect("positive zero digest allowed"),
        ];
        let record = encode_signed_epoch(
            &key(1),
            folder(7),
            2,
            writer(1),
            Some(EpochDigest::from_bytes([8; 32])),
            &roster,
            Some([9; 32]),
            &frontier,
            &cutoffs,
            &[digest(1), digest(2)],
            &[],
        )
        .expect("write-loss epoch");
        let checked = decode(&record).expect("checked epoch");

        assert_eq!(checked.epoch(), 2);
        assert_eq!(
            checked.previous_epoch_digest().expect("prior").to_bytes(),
            [8; 32]
        );
        assert_eq!(checked.proposal_digest(), Some([9; 32]));
        assert_eq!(checked.accepted_frontier_entries(), frontier);
        assert_eq!(checked.cutoffs(), cutoffs);
        assert_eq!(checked.freeze_receipt_digests(), &[digest(1), digest(2)]);
        assert!(checked.bootstrap_receipt_digests().is_empty());
    }

    #[test]
    fn bootstrap_receipts_are_separate_from_write_loss_evidence() {
        let roster = [
            member(1, 1, 101, 2, MemberRole::ReadWrite),
            member(2, 3, 102, 4, MemberRole::Read),
        ];
        let record = encode_signed_epoch(
            &key(1),
            folder(7),
            2,
            writer(1),
            Some(EpochDigest::from_bytes([8; 32])),
            &roster,
            None,
            &[],
            &[],
            &[],
            &[digest(3)],
        )
        .expect("bootstrap epoch");
        assert_eq!(
            decode(&record)
                .expect("checked epoch")
                .bootstrap_receipt_digests(),
            &[digest(3)]
        );
        let mut tampered = record;
        let bootstrap_digest_offset =
            AUTHORITY_OFFSET + 16 + DIGEST_BYTES + 2 + (2 * ROSTER_ENTRY_BYTES) + 2 + 2 + 2 + 2;
        tampered[bootstrap_digest_offset] ^= 1;
        assert_eq!(
            decode(&tampered),
            Err(MembershipCodecError::InvalidSignature)
        );

        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                2,
                writer(1),
                Some(EpochDigest::from_bytes([8; 32])),
                &roster,
                Some([9; 32]),
                &[],
                &[WriterCutoff::new(writer(2), 0, [0; 32]).expect("cutoff")],
                &[digest(1)],
                &[digest(2)]
            ),
            Err(MembershipCodecError::MixedTransitionEvidence)
        );
    }

    #[test]
    fn expected_folder_authority_and_pinned_key_are_enforced() {
        let record = genesis_record();
        let authority_key = key(1).verifying_key();
        assert_eq!(
            decode_signature_checked_epoch(&record, folder(8), writer(1), &authority_key),
            Err(MembershipCodecError::FolderMismatch)
        );
        assert_eq!(
            decode_signature_checked_epoch(&record, folder(7), writer(9), &authority_key),
            Err(MembershipCodecError::AuthorityWriterMismatch)
        );
        assert_eq!(
            decode_signature_checked_epoch(&record, folder(7), writer(1), &key(9).verifying_key()),
            Err(MembershipCodecError::InvalidSignature)
        );
    }

    #[test]
    fn authority_must_survive_read_write_with_the_pinned_writer_key() {
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                1,
                writer(1),
                None,
                &[member(1, 1, 101, 2, MemberRole::Read)],
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::AuthorityNotReadWrite)
        );
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                1,
                writer(1),
                None,
                &[member(2, 3, 101, 4, MemberRole::ReadWrite)],
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::MissingAuthority)
        );
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                1,
                writer(1),
                None,
                &[member(1, 3, 101, 4, MemberRole::ReadWrite)],
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::AuthorityKeyMismatch)
        );
    }

    #[test]
    fn roster_order_transport_identity_and_all_key_reuse_are_rejected() {
        let authority = member(1, 1, 101, 2, MemberRole::ReadWrite);
        let second = member(2, 3, 102, 4, MemberRole::Read);
        let args = |roster: &[MemberGrant]| {
            encode_signed_epoch(
                &key(1),
                folder(7),
                2,
                writer(1),
                Some(EpochDigest::from_bytes([8; 32])),
                roster,
                None,
                &[],
                &[],
                &[],
                &[],
            )
        };
        assert_eq!(
            args(&[second.clone(), authority.clone()]),
            Err(MembershipCodecError::NonCanonicalRoster)
        );
        assert_eq!(
            args(&[authority.clone(), authority.clone()]),
            Err(MembershipCodecError::NonCanonicalRoster)
        );
        let reused_transport = MemberGrant::new(
            writer(2),
            key(3).verifying_key(),
            device(101),
            key(4).verifying_key(),
            MemberRole::Read,
        )
        .expect("member");
        assert_eq!(
            args(&[authority.clone(), reused_transport]),
            Err(MembershipCodecError::ReusedTransportIdentity)
        );
        let reused_key = MemberGrant::new(
            writer(2),
            key(3).verifying_key(),
            device(102),
            key(2).verifying_key(),
            MemberRole::Read,
        )
        .expect("individually valid member");
        assert_eq!(
            args(&[authority, reused_key]),
            Err(MembershipCodecError::ReusedRosterKey)
        );
    }

    #[test]
    fn frontier_cutoff_and_receipt_lists_require_canonical_order() {
        let roster = [member(1, 1, 101, 2, MemberRole::ReadWrite)];
        let encode = |frontier: &[ClockEntry], cutoffs: &[WriterCutoff], receipts: &[[u8; 32]]| {
            encode_signed_epoch(
                &key(1),
                folder(7),
                2,
                writer(1),
                Some(EpochDigest::from_bytes([8; 32])),
                &roster,
                Some([9; 32]),
                frontier,
                cutoffs,
                receipts,
                &[],
            )
        };
        let valid_cutoff = WriterCutoff::new(writer(3), 0, [0; 32]).expect("cutoff");
        assert_eq!(
            encode(
                &[
                    ClockEntry::new(writer(2), 1).expect("frontier"),
                    ClockEntry::new(writer(1), 1).expect("frontier")
                ],
                &[valid_cutoff],
                &[digest(1)]
            ),
            Err(MembershipCodecError::NonCanonicalFrontier)
        );
        assert_eq!(
            encode(
                &[
                    ClockEntry::new(writer(1), 1).expect("frontier"),
                    ClockEntry::new(writer(1), 2).expect("frontier")
                ],
                &[valid_cutoff],
                &[digest(1)]
            ),
            Err(MembershipCodecError::NonCanonicalFrontier)
        );
        assert_eq!(
            encode(
                &[],
                &[
                    WriterCutoff::new(writer(3), 0, [0; 32]).expect("cutoff"),
                    WriterCutoff::new(writer(2), 0, [0; 32]).expect("cutoff")
                ],
                &[digest(1)]
            ),
            Err(MembershipCodecError::NonCanonicalCutoffs)
        );
        assert_eq!(
            encode(&[], &[valid_cutoff], &[digest(2), digest(1)]),
            Err(MembershipCodecError::NonCanonicalReceipts)
        );
        assert_eq!(
            encode(&[], &[valid_cutoff], &[digest(1), digest(1)]),
            Err(MembershipCodecError::NonCanonicalReceipts)
        );
        assert_eq!(
            WriterCutoff::new(writer(3), 0, [1; 32]),
            Err(MembershipCodecError::InvalidZeroCutoff)
        );

        let valid = encode(
            &[ClockEntry::new(writer(1), 1).expect("frontier")],
            &[valid_cutoff],
            &[digest(1)],
        )
        .expect("valid proof");
        let roster_offset = AUTHORITY_OFFSET + 16 + DIGEST_BYTES + 2;
        let proposal_offset = roster_offset + ROSTER_ENTRY_BYTES;
        let frontier_counter_offset = proposal_offset + DIGEST_BYTES + 2 + 16;
        let mut zero_frontier = unsigned(&valid);
        zero_frontier[frontier_counter_offset..frontier_counter_offset + 8].fill(0);
        assert_eq!(
            decode(&resign(zero_frontier)),
            Err(MembershipCodecError::ZeroFrontierCounter)
        );
    }

    #[test]
    fn genesis_and_later_epoch_shapes_fail_closed() {
        let authority = member(1, 1, 101, 2, MemberRole::ReadWrite);
        let second = member(2, 3, 102, 4, MemberRole::Read);
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                0,
                writer(1),
                None,
                std::slice::from_ref(&authority),
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::ZeroEpoch)
        );
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                1,
                writer(1),
                Some(EpochDigest::from_bytes([8; 32])),
                std::slice::from_ref(&authority),
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::InvalidPredecessor)
        );
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                2,
                writer(1),
                None,
                std::slice::from_ref(&authority),
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::InvalidPredecessor)
        );
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                1,
                writer(1),
                None,
                &[authority, second],
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::InvalidGenesisRoster)
        );
    }

    #[test]
    fn every_epoch_field_and_authority_signature_is_committed() {
        let roster = [member(1, 1, 101, 2, MemberRole::ReadWrite)];
        let record = encode_signed_epoch(
            &key(1),
            folder(7),
            2,
            writer(1),
            Some(EpochDigest::from_bytes([8; 32])),
            &roster,
            Some([9; 32]),
            &[ClockEntry::new(writer(1), 3).expect("frontier")],
            &[WriterCutoff::new(writer(2), 2, [6; 32]).expect("cutoff")],
            &[digest(1)],
            &[],
        )
        .expect("epoch");
        let roster_offset = AUTHORITY_OFFSET + 16 + DIGEST_BYTES + 2;
        let proposal_offset = roster_offset + ROSTER_ENTRY_BYTES;
        let frontier_offset = proposal_offset + DIGEST_BYTES + 2;
        let cutoff_offset = frontier_offset + FRONTIER_ENTRY_BYTES + 2;
        let freeze_offset = cutoff_offset + CUTOFF_ENTRY_BYTES + 2;
        let offsets = [
            0,
            8,
            FLAGS_OFFSET,
            FOLDER_OFFSET,
            EPOCH_OFFSET,
            AUTHORITY_OFFSET,
            AUTHORITY_OFFSET + 16,
            roster_offset,
            roster_offset + WRITER_KEY_IN_ENTRY,
            roster_offset + TRANSPORT_ID_IN_ENTRY,
            roster_offset + TRANSPORT_KEY_IN_ENTRY,
            roster_offset + ROLE_IN_ENTRY,
            proposal_offset,
            frontier_offset,
            frontier_offset + 16,
            cutoff_offset,
            cutoff_offset + 16,
            cutoff_offset + 24,
            freeze_offset,
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
    fn unknown_wire_values_weak_keys_truncation_trailing_and_oversize_reject() {
        let record = genesis_record();
        for end in 0..record.len() {
            assert!(decode(&record[..end]).is_err(), "prefix {end} was accepted");
        }
        let mut trailing = record.clone();
        trailing.push(0);
        assert_eq!(decode(&trailing), Err(MembershipCodecError::TrailingBytes));
        assert_eq!(
            decode(&vec![0; MAX_SIGNED_EPOCH_BYTES + 1]),
            Err(MembershipCodecError::RecordTooLarge)
        );

        let mut version = record.clone();
        version[8..10].copy_from_slice(&2_u16.to_be_bytes());
        assert_eq!(
            decode(&version),
            Err(MembershipCodecError::UnsupportedVersion)
        );
        let mut flags = record.clone();
        flags[FLAGS_OFFSET] = 0x80;
        assert_eq!(decode(&flags), Err(MembershipCodecError::UnknownFlags));
        let mut role = unsigned(&record);
        role[GENESIS_ROSTER_ENTRY_OFFSET + ROLE_IN_ENTRY] = 3;
        assert_eq!(
            decode(&resign(role)),
            Err(MembershipCodecError::UnknownRole)
        );
        let mut weak_writer = unsigned(&record);
        let mut identity = [0; 32];
        identity[0] = 1;
        weak_writer[GENESIS_ROSTER_ENTRY_OFFSET + WRITER_KEY_IN_ENTRY
            ..GENESIS_ROSTER_ENTRY_OFFSET + WRITER_KEY_IN_ENTRY + 32]
            .copy_from_slice(&identity);
        assert_eq!(
            decode(&resign(weak_writer)),
            Err(MembershipCodecError::WeakWriterKey)
        );
        let weak = VerifyingKey::from_bytes(&identity).expect("identity point");
        assert!(weak.is_weak());
        assert_eq!(
            decode_signature_checked_epoch(&record, folder(7), writer(1), &weak),
            Err(MembershipCodecError::WeakAuthorityKey)
        );
    }

    #[test]
    fn hostile_count_is_rejected_before_collection_allocation() {
        let mut record = genesis_record();
        record[GENESIS_ROSTER_COUNT_OFFSET..GENESIS_ROSTER_COUNT_OFFSET + 2]
            .copy_from_slice(&129_u16.to_be_bytes());
        assert_eq!(decode(&record), Err(MembershipCodecError::TooManyEntries));

        let too_many = vec![member(1, 1, 101, 2, MemberRole::ReadWrite); 129];
        assert_eq!(
            encode_signed_epoch(
                &key(1),
                folder(7),
                2,
                writer(1),
                Some(EpochDigest::from_bytes([8; 32])),
                &too_many,
                None,
                &[],
                &[],
                &[],
                &[]
            ),
            Err(MembershipCodecError::TooManyEntries)
        );
    }

    #[test]
    fn maximum_write_loss_record_needs_more_than_24k_and_fits_32k() {
        let roster = (0..128_u64)
            .map(|index| {
                member(
                    u128::from(index) + 1,
                    index * 2 + 1,
                    u128::from(index) + 1_000,
                    index * 2 + 2,
                    MemberRole::ReadWrite,
                )
            })
            .collect::<Vec<_>>();
        let frontier = (1..=128_u128)
            .map(|index| ClockEntry::new(writer(index), 1).expect("frontier"))
            .collect::<Vec<_>>();
        let cutoffs = (1..=128_u128)
            .map(|index| WriterCutoff::new(writer(index), 0, [0; 32]).expect("cutoff"))
            .collect::<Vec<_>>();
        let receipts = (1..=128_u32).map(digest).collect::<Vec<_>>();
        let record = encode_signed_epoch(
            &key(1),
            folder(7),
            2,
            writer(1),
            Some(EpochDigest::from_bytes([8; 32])),
            &roster,
            Some([9; 32]),
            &frontier,
            &cutoffs,
            &receipts,
            &[],
        )
        .expect("maximum epoch");

        assert_eq!(record.len(), 26_941);
        assert!(record.len() > 24 * 1_024);
        assert!(record.len() < MAX_SIGNED_EPOCH_BYTES);
        let checked = decode(&record).expect("maximum epoch verifies");
        assert_eq!(checked.roster().len(), 128);
        assert_eq!(checked.accepted_frontier_entries().len(), 128);
        assert_eq!(checked.cutoffs().len(), 128);
        assert_eq!(checked.freeze_receipt_digests().len(), 128);
    }

    #[test]
    fn membership_signature_domain_is_required() {
        let record = genesis_record();
        let unsigned = &record[..record.len() - SIGNATURE_BYTES];
        let mut wrong_domain = unsigned.to_vec();
        wrong_domain.extend_from_slice(&key(1).sign(unsigned).to_bytes());
        assert_eq!(
            decode(&wrong_domain),
            Err(MembershipCodecError::InvalidSignature)
        );
    }

    #[test]
    fn adversarial_bounded_inputs_do_not_panic() {
        let authority_key = key(1).verifying_key();
        for length in (0..=MAX_SIGNED_EPOCH_BYTES).step_by(263) {
            let sample = vec![(length as u8).wrapping_mul(17); length];
            let result = std::panic::catch_unwind(|| {
                decode_signature_checked_epoch(&sample, folder(7), writer(1), &authority_key)
            });
            assert!(result.is_ok(), "parser panicked for length {length}");
            assert!(result.expect("no panic").is_err());
        }
    }
}
