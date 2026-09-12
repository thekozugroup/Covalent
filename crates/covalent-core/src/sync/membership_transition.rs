//! Pure validation of one complete folder-membership transition.
//!
//! This module reparses both epoch records under one explicit folder, authority
//! writer, and pinned authority key, then validates the exact roster delta and
//! its bootstrap or all-survivor freeze evidence. Success returns a plan tied
//! to the exact base and next record digests. It does not accept, persist, or
//! activate membership.
//!
//! Bootstrap permit expiry and single use are deliberately absent from
//! deterministic historical replay. The live read-only bootstrap service and
//! the future authority signing transaction must enforce both before signing a
//! new epoch. There is no boolean or caller-supplied timestamp bypass here.
//! All causal checks must use one immutable, already-admitted same-folder
//! history snapshot. Archive queries likewise refer to the exact already-
//! accepted base head; an unknown or unaccepted head is an error. Record and
//! evidence validation alone does not prove that a pending freeze was durably
//! persisted or authorize epoch activation. A surviving installation must also
//! check its local admitted history against the resulting cutoff plan.

use std::fmt;

use ed25519_dalek::VerifyingKey;
use thiserror::Error;

use super::admission::{History, HistoryQueryError};
use super::bootstrap::{
    BootstrapReceiptDigest, decode_signature_checked_bootstrap_permit,
    decode_signature_checked_bootstrap_receipt,
};
use super::freeze::{
    SignatureCheckedWriteLossProposal, WriteLossAction, WriteLossChange,
    decode_signature_checked_freeze_receipt, decode_signature_checked_write_loss_proposal,
};
use super::frontier::check_closed_frontier;
use super::ids::{FolderId, WriterId};
use super::membership::{
    EpochDigest, MemberGrant, MemberRole, SignatureCheckedEpoch, WriterCutoff,
    decode_signature_checked_epoch,
};
use super::register::OpId;
use super::{MAX_VERSION_VECTOR_ACTORS, VersionVector};

/// Maximum combined raw epoch and evidence bytes checked before any decoding.
pub const MAX_MEMBERSHIP_TRANSITION_EVIDENCE_BYTES: usize = 3 * 1_024 * 1_024;
/// Maximum evidence records of either kind.
pub const MAX_MEMBERSHIP_TRANSITION_EVIDENCE_RECORDS: usize = MAX_VERSION_VECTOR_ACTORS;

/// Result of an exact membership-archive lookup at one accepted base head.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriterLifetime {
    /// This writer ID has never appeared in the retained membership chain.
    NeverSeen,
    /// This writer is active at the exact queried base head.
    Active,
    /// This writer appeared earlier and was removed.
    Removed,
}

/// Historical ownership of one Ed25519 writer verification key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoricalWriterKeyAssignment {
    /// The key has never been assigned in the retained membership chain.
    NeverAssigned,
    /// The key was assigned to this writer ID, active or removed.
    AssignedTo(WriterId),
}

/// Bounded failure from an exact accepted membership archive.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MembershipArchiveQueryError {
    /// The archive could not complete the bounded lookup.
    #[error("membership archive lookup is unavailable")]
    Unavailable,
    /// The folder/base digest is not one exact already-accepted archive head.
    #[error("membership archive does not recognize the exact accepted base head")]
    UnknownBaseHead,
}

/// Exact lifetime queries against a retained membership chain.
///
/// Every method is scoped to the same folder and exact already-accepted base
/// head supplied by the validator. Implementations must not answer from a
/// newer, older, unverified, or different-folder snapshot. The retained count
/// includes active and removed writer IDs; removal never frees an actor slot.
pub trait MembershipArchive {
    /// Returns every distinct writer ID retained through `base`.
    fn retained_writer_count(
        &self,
        folder: FolderId,
        base: EpochDigest,
    ) -> Result<usize, MembershipArchiveQueryError>;

    /// Returns the exact lifetime state of `writer` through `base`.
    fn writer_lifetime(
        &self,
        folder: FolderId,
        base: EpochDigest,
        writer: WriterId,
    ) -> Result<WriterLifetime, MembershipArchiveQueryError>;

    /// Returns any historical assignment of `key` through `base`.
    fn writer_key_assignment(
        &self,
        folder: FolderId,
        base: EpochDigest,
        key: &VerifyingKey,
    ) -> Result<HistoricalWriterKeyAssignment, MembershipArchiveQueryError>;
}

/// Exact raw permit and receipt records for one Add or Read-to-ReadWrite change.
#[derive(Clone, Copy)]
pub struct BootstrapTransitionEvidence<'a> {
    writer_id: WriterId,
    permit_record: &'a [u8],
    receipt_record: &'a [u8],
}

impl fmt::Debug for BootstrapTransitionEvidence<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootstrapTransitionEvidence")
            .field("writer_id", &self.writer_id)
            .field("permit_length", &self.permit_record.len())
            .field("receipt_length", &self.receipt_record.len())
            .finish()
    }
}

impl<'a> BootstrapTransitionEvidence<'a> {
    /// Binds raw evidence to the candidate writer used for bounded routing.
    #[must_use]
    pub const fn new(
        writer_id: WriterId,
        permit_record: &'a [u8],
        receipt_record: &'a [u8],
    ) -> Self {
        Self {
            writer_id,
            permit_record,
            receipt_record,
        }
    }
}

/// One signer-routed raw all-survivor freeze receipt.
#[derive(Clone, Copy)]
pub struct FreezeReceiptEvidence<'a> {
    signer_writer_id: WriterId,
    record: &'a [u8],
}

impl fmt::Debug for FreezeReceiptEvidence<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FreezeReceiptEvidence")
            .field("signer_writer_id", &self.signer_writer_id)
            .field("record_length", &self.record.len())
            .finish()
    }
}

impl<'a> FreezeReceiptEvidence<'a> {
    /// Binds a raw receipt to its claimed signer for one-pass roster lookup.
    #[must_use]
    pub const fn new(signer_writer_id: WriterId, record: &'a [u8]) -> Self {
        Self {
            signer_writer_id,
            record,
        }
    }
}

/// Raw authority proposal plus exactly one receipt per claimed survivor.
#[derive(Clone, Copy, Debug)]
pub struct WriteLossTransitionEvidence<'a> {
    proposal_record: &'a [u8],
    receipts: &'a [FreezeReceiptEvidence<'a>],
}

impl<'a> WriteLossTransitionEvidence<'a> {
    /// Creates bounded raw write-loss evidence for later strict validation.
    #[must_use]
    pub const fn new(proposal_record: &'a [u8], receipts: &'a [FreezeReceiptEvidence<'a>]) -> Self {
        Self {
            proposal_record,
            receipts,
        }
    }
}

/// Evidence mode for one complete roster transition.
#[derive(Clone, Copy, Debug)]
pub enum MembershipTransitionEvidence<'a> {
    /// No Add, upgrade, or loss of write permission.
    Ordinary,
    /// One exact permit/receipt pair per Add or Read-to-ReadWrite upgrade.
    Bootstrap(&'a [BootstrapTransitionEvidence<'a>]),
    /// One exact proposal plus one exact receipt per survivor.
    WriteLoss(WriteLossTransitionEvidence<'a>),
}

/// A role-only change for one surviving writer binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedRoleChange {
    writer_id: WriterId,
    previous: MemberRole,
    next: MemberRole,
}

impl ValidatedRoleChange {
    /// Returns the surviving writer whose role changed.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the role in the exact base roster.
    #[must_use]
    pub const fn previous(&self) -> MemberRole {
        self.previous
    }

    /// Returns the role in the exact next roster.
    #[must_use]
    pub const fn next(&self) -> MemberRole {
        self.next
    }
}

/// Minimum causal context retained for one newly writable or added writer.
#[derive(Clone, Eq, PartialEq)]
pub struct BootstrapMinimumContext {
    writer_id: WriterId,
    receipt_digest: BootstrapReceiptDigest,
    applied_frontier: VersionVector,
}

impl fmt::Debug for BootstrapMinimumContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BootstrapMinimumContext")
            .field("writer_id", &self.writer_id)
            .field("receipt_digest", &self.receipt_digest)
            .field("frontier_actor_count", &self.applied_frontier.actor_count())
            .finish()
    }
}

impl BootstrapMinimumContext {
    /// Returns the writer constrained by this publication floor.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the exact bootstrap receipt committed by the next epoch.
    #[must_use]
    pub const fn receipt_digest(&self) -> BootstrapReceiptDigest {
        self.receipt_digest
    }

    /// Returns the minimum context required on later writer publications.
    #[must_use]
    pub const fn applied_frontier(&self) -> &VersionVector {
        &self.applied_frontier
    }
}

/// Fully checked write-loss evidence retained for the later commit transaction.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedWriteLoss {
    proposal: SignatureCheckedWriteLossProposal,
    accepted_frontier: VersionVector,
    cutoffs: Vec<WriterCutoff>,
    freeze_receipt_digests: Vec<[u8; 32]>,
}

impl fmt::Debug for ValidatedWriteLoss {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedWriteLoss")
            .field("proposal_digest", &self.proposal.digest())
            .field(
                "frontier_actor_count",
                &self.accepted_frontier.actor_count(),
            )
            .field("cutoff_count", &self.cutoffs.len())
            .field("receipt_count", &self.freeze_receipt_digests.len())
            .finish()
    }
}

impl ValidatedWriteLoss {
    /// Returns the exact signature-checked proposal.
    #[must_use]
    pub const fn proposal(&self) -> &SignatureCheckedWriteLossProposal {
        &self.proposal
    }

    /// Returns the joined, causally closed survivor frontier.
    #[must_use]
    pub const fn accepted_frontier(&self) -> &VersionVector {
        &self.accepted_frontier
    }

    /// Returns exact terminal tips for every write-losing writer.
    #[must_use]
    pub fn cutoffs(&self) -> &[WriterCutoff] {
        &self.cutoffs
    }

    /// Returns sorted commitments to every exact survivor receipt.
    #[must_use]
    pub fn freeze_receipt_digests(&self) -> &[[u8; 32]] {
        &self.freeze_receipt_digests
    }
}

/// Pure validated transition plan, not accepted or durable membership.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedMembershipTransition {
    base_digest: EpochDigest,
    next: SignatureCheckedEpoch,
    added: Vec<MemberGrant>,
    removed: Vec<MemberGrant>,
    downgraded: Vec<ValidatedRoleChange>,
    upgraded: Vec<ValidatedRoleChange>,
    bootstrap_minimum_contexts: Vec<BootstrapMinimumContext>,
    write_loss: Option<ValidatedWriteLoss>,
}

impl fmt::Debug for ValidatedMembershipTransition {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedMembershipTransition")
            .field("base_digest", &self.base_digest)
            .field("next_digest", &self.next.digest())
            .field("added_count", &self.added.len())
            .field("removed_count", &self.removed.len())
            .field("downgraded_count", &self.downgraded.len())
            .field("upgraded_count", &self.upgraded.len())
            .field("bootstrap_count", &self.bootstrap_minimum_contexts.len())
            .field("has_write_loss", &self.write_loss.is_some())
            .finish()
    }
}

impl ValidatedMembershipTransition {
    /// Returns the exact already-accepted base record digest.
    #[must_use]
    pub const fn base_digest(&self) -> EpochDigest {
        self.base_digest
    }

    /// Returns the reverified next epoch for a future durable commit.
    #[must_use]
    pub const fn next_epoch(&self) -> &SignatureCheckedEpoch {
        &self.next
    }

    /// Returns exact new member grants.
    #[must_use]
    pub fn added(&self) -> &[MemberGrant] {
        &self.added
    }

    /// Returns exact removed base grants.
    #[must_use]
    pub fn removed(&self) -> &[MemberGrant] {
        &self.removed
    }

    /// Returns exact ReadWrite-to-Read changes.
    #[must_use]
    pub fn downgraded(&self) -> &[ValidatedRoleChange] {
        &self.downgraded
    }

    /// Returns exact Read-to-ReadWrite changes.
    #[must_use]
    pub fn upgraded(&self) -> &[ValidatedRoleChange] {
        &self.upgraded
    }

    /// Returns publication floors for every bootstrap-receipt writer.
    #[must_use]
    pub fn bootstrap_minimum_contexts(&self) -> &[BootstrapMinimumContext] {
        &self.bootstrap_minimum_contexts
    }

    /// Returns checked all-survivor evidence when write permission was lost.
    #[must_use]
    pub const fn write_loss(&self) -> Option<&ValidatedWriteLoss> {
        self.write_loss.as_ref()
    }
}

/// Fixed, redacted reason a transition cannot become a commit plan.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MembershipTransitionError {
    #[error("membership transition evidence has too many records")]
    TooManyEvidenceRecords,
    #[error("membership transition evidence length overflow")]
    EvidenceLengthOverflow,
    #[error("membership transition evidence exceeds the total byte limit")]
    EvidenceTooLarge,
    #[error("base membership epoch record is invalid")]
    InvalidBaseEpoch,
    #[error("next membership epoch record is invalid")]
    InvalidNextEpoch,
    #[error("next membership epoch number does not immediately follow base")]
    NonSequentialEpoch,
    #[error("next membership epoch does not commit the exact base record")]
    WrongPreviousEpoch,
    #[error("membership archive lookup failed")]
    ArchiveUnavailable,
    #[error("membership archive does not recognize the exact accepted base")]
    ArchiveUnknownBase,
    #[error("membership archive retained writer count is inconsistent")]
    InvalidRetainedWriterCount,
    #[error("base member is not active in the exact membership archive")]
    BaseMemberNotActive,
    #[error("base member key ownership is inconsistent with the archive")]
    BaseMemberKeyMismatch,
    #[error("removed writer identifier cannot be added again")]
    ReusedWriterId,
    #[error("historical writer key cannot be assigned to a new writer")]
    ReusedHistoricalWriterKey,
    #[error("membership transition exceeds the retained writer limit")]
    RetainedWriterLimit,
    #[error("surviving member changed an identity or key binding")]
    SurvivingBindingChanged,
    #[error("membership transition has no roster change")]
    NoRosterChange,
    #[error("membership transition is missing bootstrap evidence")]
    MissingBootstrapEvidence,
    #[error("membership transition has unexpected bootstrap evidence")]
    UnexpectedBootstrapEvidence,
    #[error("bootstrap evidence writer routing is not canonical")]
    NonCanonicalBootstrapEvidence,
    #[error("bootstrap permit record is invalid")]
    InvalidBootstrapPermit,
    #[error("bootstrap receipt record is invalid")]
    InvalidBootstrapReceipt,
    #[error("bootstrap evidence does not match the exact base or next grant")]
    BootstrapBindingMismatch,
    #[error("bootstrap frontier is not closed over admitted history")]
    BootstrapFrontierNotClosed,
    #[error("next epoch bootstrap receipt commitments are not exact")]
    BootstrapReceiptSetMismatch,
    #[error("membership transition is missing write-loss evidence")]
    MissingWriteLossEvidence,
    #[error("membership transition has unexpected write-loss evidence")]
    UnexpectedWriteLossEvidence,
    #[error("write-loss transition cannot add or upgrade a writer")]
    WriteLossCombinedWithBootstrap,
    #[error("write-loss proposal record is invalid")]
    InvalidWriteLossProposal,
    #[error("write-loss proposal does not exactly describe the roster transition")]
    WriteLossProposalMismatch,
    #[error("freeze receipt routing is not canonical or complete")]
    InvalidFreezeReceiptSet,
    #[error("freeze receipt record is invalid")]
    InvalidFreezeReceipt,
    #[error("freeze receipt frontier is not closed over admitted history")]
    FreezeFrontierNotClosed,
    #[error("freeze evidence frontiers exceed the actor limit")]
    FreezeFrontierTooLarge,
    #[error("freeze tip history lookup failed")]
    HistoryUnavailable,
    #[error("freeze tip operation is absent from admitted history")]
    MissingTipOperation,
    #[error("freeze tip history returned another operation")]
    WrongTipOperation,
    #[error("freeze tip digest differs from admitted history")]
    WrongTipDigest,
    #[error("next epoch accepted frontier differs from survivor receipts")]
    AcceptedFrontierMismatch,
    #[error("next epoch cutoffs differ from survivor receipt history")]
    CutoffMismatch,
    #[error("next epoch freeze receipt commitments are not exact")]
    FreezeReceiptSetMismatch,
    #[error("next epoch contains transition proof fields not justified by evidence")]
    UnexpectedTransitionProof,
    #[error("local writer does not survive the proposed membership transition")]
    LocalWriterNotSurvivor,
    #[error("local admitted history lookup is unavailable")]
    LocalHistoryUnavailable,
    #[error("local admitted history extends beyond an agreed write cutoff")]
    LocalHistoryBeyondCutoff,
    #[error("local admitted history tip disagrees with the agreed write cutoff")]
    LocalHistoryTipMismatch,
}

/// Revalidates one exact next-epoch transition without mutating any state.
#[allow(clippy::too_many_arguments)]
pub fn validate_membership_transition(
    base_record: &[u8],
    next_record: &[u8],
    expected_folder: FolderId,
    expected_authority_writer: WriterId,
    pinned_authority_key: &VerifyingKey,
    archive: &impl MembershipArchive,
    history: &impl History,
    evidence: MembershipTransitionEvidence<'_>,
) -> Result<ValidatedMembershipTransition, MembershipTransitionError> {
    check_evidence_bounds(base_record, next_record, evidence)?;
    let base = decode_signature_checked_epoch(
        base_record,
        expected_folder,
        expected_authority_writer,
        pinned_authority_key,
    )
    .map_err(|_| MembershipTransitionError::InvalidBaseEpoch)?;
    let next = decode_signature_checked_epoch(
        next_record,
        expected_folder,
        expected_authority_writer,
        pinned_authority_key,
    )
    .map_err(|_| MembershipTransitionError::InvalidNextEpoch)?;
    if base.epoch().checked_add(1) != Some(next.epoch()) {
        return Err(MembershipTransitionError::NonSequentialEpoch);
    }
    if next.previous_epoch_digest() != Some(base.digest()) {
        return Err(MembershipTransitionError::WrongPreviousEpoch);
    }

    let retained_writer_count = validate_archive_base(archive, &base)?;
    let delta = derive_roster_delta(base.roster(), next.roster())?;
    if delta.is_empty() {
        return Err(MembershipTransitionError::NoRosterChange);
    }
    validate_new_writer_history(archive, &base, retained_writer_count, &delta.added)?;

    let (bootstrap_minimum_contexts, write_loss) = match evidence {
        MembershipTransitionEvidence::Ordinary => {
            if !delta.bootstrap_writers().is_empty() {
                return Err(MembershipTransitionError::MissingBootstrapEvidence);
            }
            if !delta.losing_writer_ids().is_empty() {
                return Err(MembershipTransitionError::MissingWriteLossEvidence);
            }
            require_no_transition_proof(&next)?;
            (Vec::new(), None)
        }
        MembershipTransitionEvidence::Bootstrap(records) => {
            if !delta.losing_writer_ids().is_empty() {
                return Err(MembershipTransitionError::WriteLossCombinedWithBootstrap);
            }
            let contexts = validate_bootstrap_evidence(
                &base,
                &next,
                &delta,
                expected_folder,
                expected_authority_writer,
                pinned_authority_key,
                history,
                records,
            )?;
            (contexts, None)
        }
        MembershipTransitionEvidence::WriteLoss(raw) => {
            if !delta.bootstrap_writers().is_empty() {
                return Err(MembershipTransitionError::WriteLossCombinedWithBootstrap);
            }
            if delta.losing_writer_ids().is_empty() {
                return Err(MembershipTransitionError::UnexpectedWriteLossEvidence);
            }
            let checked = validate_write_loss_evidence(
                &base,
                &next,
                &delta,
                expected_folder,
                expected_authority_writer,
                pinned_authority_key,
                history,
                raw,
            )?;
            (Vec::new(), Some(checked))
        }
    };

    Ok(ValidatedMembershipTransition {
        base_digest: base.digest(),
        next,
        added: delta.added,
        removed: delta.removed,
        downgraded: delta.downgraded,
        upgraded: delta.upgraded,
        bootstrap_minimum_contexts,
        write_loss,
    })
}

/// Checks whether a surviving installation may activate a validated plan.
///
/// The history must be the same folder's immutable admitted snapshot used by
/// the surrounding acceptance transaction. A local tip below a cutoff can be
/// fetched later. A tip at the cutoff must have the exact committed digest; a
/// tip above it proves this survivor has history outside the freeze agreement.
/// Removed local writers use a separate stop-and-retain-files path and must not
/// call this as permission to discard their unshared state. Success performs no
/// persistence and does not by itself activate the epoch.
pub fn check_survivor_history_compatibility(
    history: &impl History,
    transition: &ValidatedMembershipTransition,
    local_writer: WriterId,
) -> Result<(), MembershipTransitionError> {
    if member_by_writer(transition.next_epoch().roster(), local_writer).is_none() {
        return Err(MembershipTransitionError::LocalWriterNotSurvivor);
    }
    let Some(write_loss) = transition.write_loss() else {
        return Ok(());
    };
    for cutoff in write_loss.cutoffs() {
        let Some(tip) = history
            .author_tip(cutoff.writer_id().into_vector_actor())
            .map_err(|_| MembershipTransitionError::LocalHistoryUnavailable)?
        else {
            continue;
        };
        match tip.counter().cmp(&cutoff.counter()) {
            std::cmp::Ordering::Greater => {
                return Err(MembershipTransitionError::LocalHistoryBeyondCutoff);
            }
            std::cmp::Ordering::Equal if tip.digest() != cutoff.operation_digest() => {
                return Err(MembershipTransitionError::LocalHistoryTipMismatch);
            }
            std::cmp::Ordering::Less | std::cmp::Ordering::Equal => {}
        }
    }
    Ok(())
}

struct RosterDelta {
    added: Vec<MemberGrant>,
    removed: Vec<MemberGrant>,
    downgraded: Vec<ValidatedRoleChange>,
    upgraded: Vec<ValidatedRoleChange>,
}

impl RosterDelta {
    fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.downgraded.is_empty()
            && self.upgraded.is_empty()
    }

    fn bootstrap_writers(&self) -> Vec<WriterId> {
        let mut writers: Vec<_> = self.added.iter().map(MemberGrant::writer_id).collect();
        writers.extend(self.upgraded.iter().map(ValidatedRoleChange::writer_id));
        writers.sort_unstable();
        writers
    }

    fn losing_writer_ids(&self) -> Vec<WriterId> {
        let mut writers = Vec::new();
        for grant in &self.removed {
            if grant.role() == MemberRole::ReadWrite {
                writers.push(grant.writer_id());
            }
        }
        writers.extend(self.downgraded.iter().map(ValidatedRoleChange::writer_id));
        writers.sort_unstable();
        writers
    }
}

fn derive_roster_delta(
    base: &[MemberGrant],
    next: &[MemberGrant],
) -> Result<RosterDelta, MembershipTransitionError> {
    let mut delta = RosterDelta {
        added: Vec::new(),
        removed: Vec::new(),
        downgraded: Vec::new(),
        upgraded: Vec::new(),
    };
    let (mut left, mut right) = (0, 0);
    while left < base.len() || right < next.len() {
        match (base.get(left), next.get(right)) {
            (Some(old), Some(new)) if old.writer_id() == new.writer_id() => {
                if old.writer_key() != new.writer_key()
                    || old.transport_device_id() != new.transport_device_id()
                    || old.transport_key() != new.transport_key()
                {
                    return Err(MembershipTransitionError::SurvivingBindingChanged);
                }
                match (old.role(), new.role()) {
                    (MemberRole::ReadWrite, MemberRole::Read) => {
                        delta.downgraded.push(ValidatedRoleChange {
                            writer_id: old.writer_id(),
                            previous: old.role(),
                            next: new.role(),
                        });
                    }
                    (MemberRole::Read, MemberRole::ReadWrite) => {
                        delta.upgraded.push(ValidatedRoleChange {
                            writer_id: old.writer_id(),
                            previous: old.role(),
                            next: new.role(),
                        });
                    }
                    (MemberRole::Read, MemberRole::Read)
                    | (MemberRole::ReadWrite, MemberRole::ReadWrite) => {}
                }
                left += 1;
                right += 1;
            }
            (Some(old), Some(new)) if old.writer_id() < new.writer_id() => {
                delta.removed.push(old.clone());
                left += 1;
            }
            (Some(_), Some(new)) => {
                delta.added.push(new.clone());
                right += 1;
            }
            (Some(old), None) => {
                delta.removed.push(old.clone());
                left += 1;
            }
            (None, Some(new)) => {
                delta.added.push(new.clone());
                right += 1;
            }
            (None, None) => break,
        }
    }
    Ok(delta)
}

fn validate_archive_base(
    archive: &impl MembershipArchive,
    base: &SignatureCheckedEpoch,
) -> Result<usize, MembershipTransitionError> {
    let count = archive
        .retained_writer_count(base.folder_id(), base.digest())
        .map_err(map_archive_error)?;
    if count < base.roster().len() {
        return Err(MembershipTransitionError::InvalidRetainedWriterCount);
    }
    if count > MAX_VERSION_VECTOR_ACTORS {
        return Err(MembershipTransitionError::RetainedWriterLimit);
    }
    for member in base.roster() {
        if archive
            .writer_lifetime(base.folder_id(), base.digest(), member.writer_id())
            .map_err(map_archive_error)?
            != WriterLifetime::Active
        {
            return Err(MembershipTransitionError::BaseMemberNotActive);
        }
        if archive
            .writer_key_assignment(base.folder_id(), base.digest(), member.writer_key())
            .map_err(map_archive_error)?
            != HistoricalWriterKeyAssignment::AssignedTo(member.writer_id())
        {
            return Err(MembershipTransitionError::BaseMemberKeyMismatch);
        }
    }
    Ok(count)
}

fn validate_new_writer_history(
    archive: &impl MembershipArchive,
    base: &SignatureCheckedEpoch,
    retained: usize,
    added: &[MemberGrant],
) -> Result<(), MembershipTransitionError> {
    if retained
        .checked_add(added.len())
        .is_none_or(|count| count > MAX_VERSION_VECTOR_ACTORS)
    {
        return Err(MembershipTransitionError::RetainedWriterLimit);
    }
    for member in added {
        if archive
            .writer_lifetime(base.folder_id(), base.digest(), member.writer_id())
            .map_err(map_archive_error)?
            != WriterLifetime::NeverSeen
        {
            return Err(MembershipTransitionError::ReusedWriterId);
        }
        if archive
            .writer_key_assignment(base.folder_id(), base.digest(), member.writer_key())
            .map_err(map_archive_error)?
            != HistoricalWriterKeyAssignment::NeverAssigned
        {
            return Err(MembershipTransitionError::ReusedHistoricalWriterKey);
        }
    }
    Ok(())
}

const fn map_archive_error(error: MembershipArchiveQueryError) -> MembershipTransitionError {
    match error {
        MembershipArchiveQueryError::Unavailable => MembershipTransitionError::ArchiveUnavailable,
        MembershipArchiveQueryError::UnknownBaseHead => {
            MembershipTransitionError::ArchiveUnknownBase
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_bootstrap_evidence(
    base: &SignatureCheckedEpoch,
    next: &SignatureCheckedEpoch,
    delta: &RosterDelta,
    folder: FolderId,
    authority_writer: WriterId,
    authority_key: &VerifyingKey,
    history: &impl History,
    records: &[BootstrapTransitionEvidence<'_>],
) -> Result<Vec<BootstrapMinimumContext>, MembershipTransitionError> {
    let expected_writers = delta.bootstrap_writers();
    if expected_writers.is_empty() {
        return Err(MembershipTransitionError::UnexpectedBootstrapEvidence);
    }
    if records.len() != expected_writers.len() {
        return Err(MembershipTransitionError::MissingBootstrapEvidence);
    }
    require_no_write_loss_proof(next)?;
    let mut routed: Vec<_> = records.iter().collect();
    routed.sort_unstable_by_key(|record| record.writer_id);
    if routed
        .windows(2)
        .any(|pair| pair[0].writer_id >= pair[1].writer_id)
        || routed
            .iter()
            .zip(&expected_writers)
            .any(|(record, expected)| record.writer_id != *expected)
    {
        return Err(MembershipTransitionError::NonCanonicalBootstrapEvidence);
    }

    let mut contexts = Vec::with_capacity(routed.len());
    let mut digests = Vec::with_capacity(routed.len());
    for raw in routed {
        let next_grant = member_by_writer(next.roster(), raw.writer_id)
            .ok_or(MembershipTransitionError::BootstrapBindingMismatch)?;
        let permit = decode_signature_checked_bootstrap_permit(
            raw.permit_record,
            folder,
            authority_writer,
            authority_key,
        )
        .map_err(|_| MembershipTransitionError::InvalidBootstrapPermit)?;
        if permit.base_epoch() != base.epoch()
            || permit.base_epoch_digest() != base.digest()
            || permit.candidate() != next_grant
        {
            return Err(MembershipTransitionError::BootstrapBindingMismatch);
        }
        let receipt = decode_signature_checked_bootstrap_receipt(
            raw.receipt_record,
            &permit,
            permit.candidate().writer_key(),
        )
        .map_err(|_| MembershipTransitionError::InvalidBootstrapReceipt)?;
        check_closed_frontier(history, permit.required_frontier())
            .map_err(|_| MembershipTransitionError::BootstrapFrontierNotClosed)?;
        check_closed_frontier(history, receipt.applied_frontier())
            .map_err(|_| MembershipTransitionError::BootstrapFrontierNotClosed)?;
        digests.push(receipt.digest().to_bytes());
        contexts.push(BootstrapMinimumContext {
            writer_id: raw.writer_id,
            receipt_digest: receipt.digest(),
            applied_frontier: receipt.applied_frontier().clone(),
        });
    }
    digests.sort_unstable();
    if digests != next.bootstrap_receipt_digests() {
        return Err(MembershipTransitionError::BootstrapReceiptSetMismatch);
    }
    Ok(contexts)
}

#[allow(clippy::too_many_arguments)]
fn validate_write_loss_evidence(
    base: &SignatureCheckedEpoch,
    next: &SignatureCheckedEpoch,
    delta: &RosterDelta,
    folder: FolderId,
    authority_writer: WriterId,
    authority_key: &VerifyingKey,
    history: &impl History,
    raw: WriteLossTransitionEvidence<'_>,
) -> Result<ValidatedWriteLoss, MembershipTransitionError> {
    if !next.bootstrap_receipt_digests().is_empty() {
        return Err(MembershipTransitionError::WriteLossCombinedWithBootstrap);
    }
    let proposal = decode_signature_checked_write_loss_proposal(
        raw.proposal_record,
        folder,
        authority_writer,
        authority_key,
    )
    .map_err(|_| MembershipTransitionError::InvalidWriteLossProposal)?;
    let expected_changes = exact_proposal_changes(delta);
    let expected_survivors: Vec<_> = next.roster().iter().map(MemberGrant::writer_id).collect();
    let expected_losing = delta.losing_writer_ids();
    if proposal.base_epoch() != base.epoch()
        || proposal.base_epoch_digest() != base.digest()
        || proposal.changes() != expected_changes
        || proposal.survivor_writer_ids() != expected_survivors
        || proposal.losing_writer_ids() != expected_losing
        || next.proposal_digest() != Some(proposal.digest().to_bytes())
    {
        return Err(MembershipTransitionError::WriteLossProposalMismatch);
    }
    if raw.receipts.len() != expected_survivors.len() {
        return Err(MembershipTransitionError::InvalidFreezeReceiptSet);
    }
    let mut receipts: Vec<_> = raw.receipts.iter().collect();
    receipts.sort_unstable_by_key(|receipt| receipt.signer_writer_id);
    if receipts
        .windows(2)
        .any(|pair| pair[0].signer_writer_id >= pair[1].signer_writer_id)
        || receipts
            .iter()
            .zip(&expected_survivors)
            .any(|(receipt, expected)| receipt.signer_writer_id != *expected)
    {
        return Err(MembershipTransitionError::InvalidFreezeReceiptSet);
    }

    let mut joined = VersionVector::empty();
    let mut receipt_digests = Vec::with_capacity(receipts.len());
    for raw_receipt in receipts {
        let grant = member_by_writer(base.roster(), raw_receipt.signer_writer_id)
            .ok_or(MembershipTransitionError::InvalidFreezeReceiptSet)?;
        let receipt = decode_signature_checked_freeze_receipt(raw_receipt.record, &proposal, grant)
            .map_err(|_| MembershipTransitionError::InvalidFreezeReceipt)?;
        check_closed_frontier(history, receipt.frontier())
            .map_err(|_| MembershipTransitionError::FreezeFrontierNotClosed)?;
        for tip in receipt.losing_writer_tips() {
            check_tip(history, *tip)?;
        }
        joined = joined
            .join(receipt.frontier())
            .map_err(|_| MembershipTransitionError::FreezeFrontierTooLarge)?;
        receipt_digests.push(receipt.digest().to_bytes());
    }
    check_closed_frontier(history, &joined)
        .map_err(|_| MembershipTransitionError::FreezeFrontierNotClosed)?;
    if next.accepted_frontier() != &joined {
        return Err(MembershipTransitionError::AcceptedFrontierMismatch);
    }
    let cutoffs = cutoffs_from_join(history, &joined, &expected_losing)?;
    if next.cutoffs() != cutoffs {
        return Err(MembershipTransitionError::CutoffMismatch);
    }
    receipt_digests.sort_unstable();
    if next.freeze_receipt_digests() != receipt_digests {
        return Err(MembershipTransitionError::FreezeReceiptSetMismatch);
    }
    Ok(ValidatedWriteLoss {
        proposal,
        accepted_frontier: joined,
        cutoffs,
        freeze_receipt_digests: receipt_digests,
    })
}

fn exact_proposal_changes(delta: &RosterDelta) -> Vec<WriteLossChange> {
    let mut changes: Vec<_> = delta
        .removed
        .iter()
        .map(|grant| WriteLossChange::new(grant.writer_id(), WriteLossAction::Remove))
        .collect();
    changes.extend(
        delta.downgraded.iter().map(|change| {
            WriteLossChange::new(change.writer_id(), WriteLossAction::DowngradeToRead)
        }),
    );
    changes.sort_unstable_by_key(|change| change.writer_id());
    changes
}

fn check_tip(
    history: &impl History,
    cutoff: WriterCutoff,
) -> Result<(), MembershipTransitionError> {
    if cutoff.counter() == 0 {
        return Ok(());
    }
    let id = OpId::new(cutoff.writer_id().into_vector_actor(), cutoff.counter())
        .expect("positive cutoff counter");
    let header = history
        .header(id)
        .map_err(map_history_error)?
        .ok_or(MembershipTransitionError::MissingTipOperation)?;
    if header.id() != id {
        return Err(MembershipTransitionError::WrongTipOperation);
    }
    if header.digest() != cutoff.operation_digest() {
        return Err(MembershipTransitionError::WrongTipDigest);
    }
    Ok(())
}

fn cutoffs_from_join(
    history: &impl History,
    joined: &VersionVector,
    losing: &[WriterId],
) -> Result<Vec<WriterCutoff>, MembershipTransitionError> {
    losing
        .iter()
        .map(|writer| {
            let actor = writer.into_vector_actor();
            let counter = joined.counter(actor);
            let digest = if counter == 0 {
                [0; 32]
            } else {
                let id = OpId::new(actor, counter).expect("positive joined counter");
                let header = history
                    .header(id)
                    .map_err(map_history_error)?
                    .ok_or(MembershipTransitionError::MissingTipOperation)?;
                if header.id() != id {
                    return Err(MembershipTransitionError::WrongTipOperation);
                }
                header.digest()
            };
            WriterCutoff::new(*writer, counter, digest)
                .map_err(|_| MembershipTransitionError::CutoffMismatch)
        })
        .collect()
}

const fn map_history_error(_: HistoryQueryError) -> MembershipTransitionError {
    MembershipTransitionError::HistoryUnavailable
}

fn require_no_transition_proof(
    next: &SignatureCheckedEpoch,
) -> Result<(), MembershipTransitionError> {
    if next.proposal_digest().is_some()
        || next.accepted_frontier().actor_count() != 0
        || !next.cutoffs().is_empty()
        || !next.freeze_receipt_digests().is_empty()
        || !next.bootstrap_receipt_digests().is_empty()
    {
        return Err(MembershipTransitionError::UnexpectedTransitionProof);
    }
    Ok(())
}

fn require_no_write_loss_proof(
    next: &SignatureCheckedEpoch,
) -> Result<(), MembershipTransitionError> {
    if next.proposal_digest().is_some()
        || next.accepted_frontier().actor_count() != 0
        || !next.cutoffs().is_empty()
        || !next.freeze_receipt_digests().is_empty()
    {
        return Err(MembershipTransitionError::UnexpectedWriteLossEvidence);
    }
    Ok(())
}

fn member_by_writer(roster: &[MemberGrant], writer: WriterId) -> Option<&MemberGrant> {
    roster
        .binary_search_by_key(&writer, MemberGrant::writer_id)
        .ok()
        .map(|index| &roster[index])
}

fn check_evidence_bounds(
    base_record: &[u8],
    next_record: &[u8],
    evidence: MembershipTransitionEvidence<'_>,
) -> Result<(), MembershipTransitionError> {
    let mut total = base_record
        .len()
        .checked_add(next_record.len())
        .ok_or(MembershipTransitionError::EvidenceLengthOverflow)?;
    match evidence {
        MembershipTransitionEvidence::Ordinary => {}
        MembershipTransitionEvidence::Bootstrap(records) => {
            check_evidence_count(records.len())?;
            for record in records {
                total = total
                    .checked_add(record.permit_record.len())
                    .and_then(|value| value.checked_add(record.receipt_record.len()))
                    .ok_or(MembershipTransitionError::EvidenceLengthOverflow)?;
            }
        }
        MembershipTransitionEvidence::WriteLoss(raw) => {
            check_evidence_count(raw.receipts.len())?;
            total = total
                .checked_add(raw.proposal_record.len())
                .ok_or(MembershipTransitionError::EvidenceLengthOverflow)?;
            for receipt in raw.receipts {
                total = total
                    .checked_add(receipt.record.len())
                    .ok_or(MembershipTransitionError::EvidenceLengthOverflow)?;
            }
        }
    }
    if total > MAX_MEMBERSHIP_TRANSITION_EVIDENCE_BYTES {
        return Err(MembershipTransitionError::EvidenceTooLarge);
    }
    Ok(())
}

fn check_evidence_count(count: usize) -> Result<(), MembershipTransitionError> {
    if count > MAX_MEMBERSHIP_TRANSITION_EVIDENCE_RECORDS {
        return Err(MembershipTransitionError::TooManyEvidenceRecords);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use covalent_protocol::DeviceId;
    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::admission::{self, Admission, AuthorTip, Header};
    use crate::sync::bootstrap::{
        decode_signature_checked_bootstrap_permit, encode_signed_bootstrap_permit,
        encode_signed_bootstrap_receipt,
    };
    use crate::sync::freeze::{
        decode_signature_checked_freeze_receipt, decode_signature_checked_write_loss_proposal,
        encode_signed_freeze_receipt, encode_signed_write_loss_proposal,
    };
    use crate::sync::membership::encode_signed_epoch;
    use crate::sync::operation::ClockEntry;

    #[derive(Default)]
    struct TestArchive {
        expected_folder: Option<FolderId>,
        expected_base: Option<EpochDigest>,
        retained_count: usize,
        lifetimes: BTreeMap<WriterId, WriterLifetime>,
        key_assignments: BTreeMap<[u8; 32], HistoricalWriterKeyAssignment>,
        failure: Option<MembershipArchiveQueryError>,
    }

    impl TestArchive {
        fn for_base(record: &[u8]) -> Self {
            let base = decode_epoch(record).expect("base epoch");
            let mut archive = Self {
                expected_folder: Some(base.folder_id()),
                expected_base: Some(base.digest()),
                retained_count: base.roster().len(),
                ..Self::default()
            };
            for grant in base.roster() {
                archive
                    .lifetimes
                    .insert(grant.writer_id(), WriterLifetime::Active);
                archive.key_assignments.insert(
                    grant.writer_key().to_bytes(),
                    HistoricalWriterKeyAssignment::AssignedTo(grant.writer_id()),
                );
            }
            archive
        }

        fn check_head(
            &self,
            folder: FolderId,
            base: EpochDigest,
        ) -> Result<(), MembershipArchiveQueryError> {
            if let Some(error) = self.failure {
                return Err(error);
            }
            if self.expected_folder != Some(folder) || self.expected_base != Some(base) {
                return Err(MembershipArchiveQueryError::UnknownBaseHead);
            }
            Ok(())
        }
    }

    impl MembershipArchive for TestArchive {
        fn retained_writer_count(
            &self,
            folder: FolderId,
            base: EpochDigest,
        ) -> Result<usize, MembershipArchiveQueryError> {
            self.check_head(folder, base)?;
            Ok(self.retained_count)
        }

        fn writer_lifetime(
            &self,
            folder: FolderId,
            base: EpochDigest,
            writer: WriterId,
        ) -> Result<WriterLifetime, MembershipArchiveQueryError> {
            self.check_head(folder, base)?;
            Ok(self
                .lifetimes
                .get(&writer)
                .copied()
                .unwrap_or(WriterLifetime::NeverSeen))
        }

        fn writer_key_assignment(
            &self,
            folder: FolderId,
            base: EpochDigest,
            key: &VerifyingKey,
        ) -> Result<HistoricalWriterKeyAssignment, MembershipArchiveQueryError> {
            self.check_head(folder, base)?;
            Ok(self
                .key_assignments
                .get(&key.to_bytes())
                .copied()
                .unwrap_or(HistoricalWriterKeyAssignment::NeverAssigned))
        }
    }

    #[derive(Default)]
    struct TestHistory {
        headers: BTreeMap<OpId, Header>,
        tips: BTreeMap<DeviceId, AuthorTip>,
        unavailable: bool,
    }

    impl History for TestHistory {
        fn author_tip(&self, writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError> {
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self.tips.get(&writer).copied())
        }

        fn header(&self, id: OpId) -> Result<Option<Header>, HistoryQueryError> {
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self.headers.get(&id).cloned())
        }
    }

    fn folder() -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(7))
    }

    fn writer(number: u128) -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(number))
    }

    fn device(number: u128) -> DeviceId {
        DeviceId::from_uuid(Uuid::from_u128(1_000 + number))
    }

    fn key(number: u64) -> SigningKey {
        let mut bytes = [0x6d; 32];
        bytes[24..].copy_from_slice(&number.to_be_bytes());
        SigningKey::from_bytes(&bytes)
    }

    fn member(number: u128, role: MemberRole) -> MemberGrant {
        MemberGrant::new(
            writer(number),
            key(number as u64).verifying_key(),
            device(number),
            key(100 + number as u64).verifying_key(),
            role,
        )
        .expect("member")
    }

    #[allow(clippy::too_many_arguments)]
    fn epoch_record(
        epoch: u64,
        previous: Option<EpochDigest>,
        roster: &[MemberGrant],
        proposal: Option<[u8; 32]>,
        frontier: &[ClockEntry],
        cutoffs: &[WriterCutoff],
        freeze_receipts: &[[u8; 32]],
        bootstrap_receipts: &[[u8; 32]],
    ) -> Vec<u8> {
        encode_signed_epoch(
            &key(1),
            folder(),
            epoch,
            writer(1),
            previous,
            roster,
            proposal,
            frontier,
            cutoffs,
            freeze_receipts,
            bootstrap_receipts,
        )
        .expect("epoch")
    }

    fn genesis() -> Vec<u8> {
        epoch_record(
            1,
            None,
            &[member(1, MemberRole::ReadWrite)],
            None,
            &[],
            &[],
            &[],
            &[],
        )
    }

    fn decode_epoch(record: &[u8]) -> Result<SignatureCheckedEpoch, ()> {
        decode_signature_checked_epoch(record, folder(), writer(1), &key(1).verifying_key())
            .map_err(|_| ())
    }

    fn bootstrap_records(
        base: &SignatureCheckedEpoch,
        candidate: &MemberGrant,
    ) -> (Vec<u8>, Vec<u8>) {
        let permit = encode_signed_bootstrap_permit(
            &key(1),
            folder(),
            writer(1),
            base.epoch(),
            base.digest(),
            candidate,
            [9; 32],
            1,
            &[],
        )
        .expect("permit");
        let checked = decode_signature_checked_bootstrap_permit(
            &permit,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked permit");
        let receipt = encode_signed_bootstrap_receipt(
            &key(candidate.writer_id().to_bytes()[15].into()),
            &checked,
            &[],
            [6; 32],
        )
        .expect("receipt");
        (permit, receipt)
    }

    fn validate<'a>(
        base: &[u8],
        next: &[u8],
        archive: &TestArchive,
        history: &TestHistory,
        evidence: MembershipTransitionEvidence<'a>,
    ) -> Result<ValidatedMembershipTransition, MembershipTransitionError> {
        validate_membership_transition(
            base,
            next,
            folder(),
            writer(1),
            &key(1).verifying_key(),
            archive,
            history,
            evidence,
        )
    }

    fn add_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, TestArchive) {
        let base_record = genesis();
        let base = decode_epoch(&base_record).expect("base");
        let candidate = member(2, MemberRole::ReadWrite);
        let (permit, receipt) = bootstrap_records(&base, &candidate);
        let next_record = epoch_record(
            2,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite), candidate],
            None,
            &[],
            &[],
            &[],
            &[*blake3::hash(&receipt).as_bytes()],
        );
        let archive = TestArchive::for_base(&base_record);
        (base_record, next_record, permit, receipt, archive)
    }

    #[test]
    fn add_replays_expired_permit_deterministically_and_returns_publication_floor() {
        let (base, next, permit, receipt, archive) = add_fixture();
        let records = [BootstrapTransitionEvidence::new(
            writer(2),
            &permit,
            &receipt,
        )];
        let plan = validate(
            &base,
            &next,
            &archive,
            &TestHistory::default(),
            MembershipTransitionEvidence::Bootstrap(&records),
        )
        .expect("valid Add");

        assert_eq!(plan.base_digest(), decode_epoch(&base).unwrap().digest());
        assert_eq!(plan.next_epoch().canonical_record(), next);
        assert_eq!(plan.added(), &[member(2, MemberRole::ReadWrite)]);
        assert!(plan.removed().is_empty());
        assert_eq!(plan.bootstrap_minimum_contexts().len(), 1);
        assert_eq!(plan.bootstrap_minimum_contexts()[0].writer_id(), writer(2));
        assert_eq!(
            plan.bootstrap_minimum_contexts()[0].applied_frontier(),
            &VersionVector::empty()
        );
    }

    #[test]
    fn read_to_write_upgrade_requires_fresh_exact_bootstrap() {
        let base_record = epoch_record(
            2,
            Some(EpochDigest::from_bytes([2; 32])),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::Read),
            ],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let base = decode_epoch(&base_record).expect("base");
        let upgraded = member(2, MemberRole::ReadWrite);
        let (permit, receipt) = bootstrap_records(&base, &upgraded);
        let next = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite), upgraded],
            None,
            &[],
            &[],
            &[],
            &[*blake3::hash(&receipt).as_bytes()],
        );
        let archive = TestArchive::for_base(&base_record);
        assert_eq!(
            validate(
                &base_record,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Ordinary,
            ),
            Err(MembershipTransitionError::MissingBootstrapEvidence)
        );
        let evidence = [BootstrapTransitionEvidence::new(
            writer(2),
            &permit,
            &receipt,
        )];
        let plan = validate(
            &base_record,
            &next,
            &archive,
            &TestHistory::default(),
            MembershipTransitionEvidence::Bootstrap(&evidence),
        )
        .expect("valid upgrade");
        assert_eq!(plan.upgraded().len(), 1);
    }

    #[test]
    fn bootstrap_requires_closed_frontiers_and_exact_receipt_commitment() {
        let base_record = genesis();
        let base = decode_epoch(&base_record).expect("base");
        let candidate = member(2, MemberRole::ReadWrite);
        let required = [ClockEntry::new(writer(1), 1).expect("frontier")];
        let permit = encode_signed_bootstrap_permit(
            &key(1),
            folder(),
            writer(1),
            base.epoch(),
            base.digest(),
            &candidate,
            [9; 32],
            1,
            &required,
        )
        .expect("permit");
        let checked_permit = decode_signature_checked_bootstrap_permit(
            &permit,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked permit");
        let receipt = encode_signed_bootstrap_receipt(&key(2), &checked_permit, &required, [6; 32])
            .expect("receipt");
        let next = epoch_record(
            2,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite), candidate],
            None,
            &[],
            &[],
            &[],
            &[*blake3::hash(&receipt).as_bytes()],
        );
        let archive = TestArchive::for_base(&base_record);
        let evidence = [BootstrapTransitionEvidence::new(
            writer(2),
            &permit,
            &receipt,
        )];
        assert_eq!(
            validate(
                &base_record,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::BootstrapFrontierNotClosed)
        );

        let actor = writer(1).into_vector_actor();
        let id = OpId::new(actor, 1).unwrap();
        let history = TestHistory {
            headers: BTreeMap::from([(
                id,
                Header::new(id, VersionVector::new([(actor, 1)]).unwrap(), [4; 32], None),
            )]),
            tips: BTreeMap::new(),
            unavailable: false,
        };
        assert!(
            validate(
                &base_record,
                &next,
                &archive,
                &history,
                MembershipTransitionEvidence::Bootstrap(&evidence),
            )
            .is_ok()
        );

        let wrong_commitment = epoch_record(
            2,
            Some(base.digest()),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::ReadWrite),
            ],
            None,
            &[],
            &[],
            &[],
            &[[0x55; 32]],
        );
        assert_eq!(
            validate(
                &base_record,
                &wrong_commitment,
                &archive,
                &history,
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::BootstrapReceiptSetMismatch)
        );
    }

    #[test]
    fn read_only_removal_needs_no_freeze_evidence() {
        let base_record = epoch_record(
            2,
            Some(EpochDigest::from_bytes([2; 32])),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::Read),
            ],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let base = decode_epoch(&base_record).expect("base");
        let next = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite)],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let plan = validate(
            &base_record,
            &next,
            &TestArchive::for_base(&base_record),
            &TestHistory::default(),
            MembershipTransitionEvidence::Ordinary,
        )
        .expect("read removal");
        assert_eq!(plan.removed(), &[member(2, MemberRole::Read)]);
        assert!(plan.write_loss().is_none());
    }

    fn write_loss_fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, TestArchive, TestHistory) {
        let base_record = epoch_record(
            2,
            Some(EpochDigest::from_bytes([2; 32])),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::ReadWrite),
            ],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let base = decode_epoch(&base_record).expect("base");
        let proposal_record = encode_signed_write_loss_proposal(
            &key(1),
            folder(),
            base.epoch(),
            base.digest(),
            writer(1),
            [7; 32],
            &[WriteLossChange::new(writer(2), WriteLossAction::Remove)],
            &[writer(1)],
            &[writer(2)],
        )
        .expect("proposal");
        let proposal = decode_signature_checked_write_loss_proposal(
            &proposal_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked proposal");
        let frontier = [ClockEntry::new(writer(2), 1).expect("clock")];
        let tips = [WriterCutoff::new(writer(2), 1, [5; 32]).expect("tip")];
        let receipt = encode_signed_freeze_receipt(
            &key(1),
            &proposal,
            &member(1, MemberRole::ReadWrite),
            &frontier,
            &tips,
        )
        .expect("receipt");
        let next_record = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite)],
            Some(proposal.digest().to_bytes()),
            &frontier,
            &tips,
            &[*blake3::hash(&receipt).as_bytes()],
            &[],
        );
        let actor = writer(2).into_vector_actor();
        let id = OpId::new(actor, 1).expect("id");
        let history = TestHistory {
            headers: BTreeMap::from([(
                id,
                Header::new(
                    id,
                    VersionVector::new([(actor, 1)]).expect("vector"),
                    [5; 32],
                    None,
                ),
            )]),
            tips: BTreeMap::new(),
            unavailable: false,
        };
        (
            base_record.clone(),
            next_record,
            proposal_record,
            receipt,
            TestArchive::for_base(&base_record),
            history,
        )
    }

    struct MultiSurvivorWriteLossFixture {
        base_record: Vec<u8>,
        next_record: Vec<u8>,
        proposal_record: Vec<u8>,
        authority_receipt: Vec<u8>,
        downgraded_receipt: Vec<u8>,
        joined_frontier: Vec<ClockEntry>,
        exact_cutoffs: Vec<WriterCutoff>,
        receipt_digests: Vec<[u8; 32]>,
        archive: TestArchive,
        history: TestHistory,
    }

    fn append_admitted_header(
        history: &mut TestHistory,
        writer_id: WriterId,
        clock: VersionVector,
        digest: [u8; 32],
    ) {
        let actor = writer_id.into_vector_actor();
        let counter = clock.counter(actor);
        let id = OpId::new(actor, counter).expect("positive author clock");
        let predecessor = history.tips.get(&actor).copied().map(AuthorTip::digest);
        let header = Header::new(id, clock, digest, predecessor);
        assert_eq!(
            admission::check(history, &header),
            Ok(Admission::Admissible)
        );
        history
            .tips
            .insert(actor, AuthorTip::new(counter, digest).expect("tip"));
        history.headers.insert(id, header);
    }

    fn multi_survivor_write_loss_fixture() -> MultiSurvivorWriteLossFixture {
        let authority = member(1, MemberRole::ReadWrite);
        let downgraded_base = member(2, MemberRole::ReadWrite);
        let losing = member(3, MemberRole::ReadWrite);
        let downgraded_next = member(2, MemberRole::Read);
        let base_record = epoch_record(
            2,
            Some(EpochDigest::from_bytes([2; 32])),
            &[authority.clone(), downgraded_base.clone(), losing],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        let base = decode_epoch(&base_record).expect("base epoch");
        let changes = [
            WriteLossChange::new(writer(2), WriteLossAction::DowngradeToRead),
            WriteLossChange::new(writer(3), WriteLossAction::Remove),
        ];
        let proposal_record = encode_signed_write_loss_proposal(
            &key(1),
            folder(),
            base.epoch(),
            base.digest(),
            writer(1),
            [0x71; 32],
            &changes,
            &[writer(1), writer(2)],
            &[writer(2), writer(3)],
        )
        .expect("proposal");
        let proposal = decode_signature_checked_write_loss_proposal(
            &proposal_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked proposal");

        let authority_actor = writer(1).into_vector_actor();
        let downgraded_actor = writer(2).into_vector_actor();
        let losing_actor = writer(3).into_vector_actor();
        let mut history = TestHistory::default();
        append_admitted_header(
            &mut history,
            writer(3),
            VersionVector::new([(losing_actor, 1)]).expect("loss operation one"),
            [0x31; 32],
        );
        append_admitted_header(
            &mut history,
            writer(1),
            VersionVector::new([(authority_actor, 1), (losing_actor, 1)])
                .expect("authority operation"),
            [0x11; 32],
        );
        append_admitted_header(
            &mut history,
            writer(1),
            VersionVector::new([(authority_actor, 2), (losing_actor, 1)])
                .expect("authority operation two"),
            [0x12; 32],
        );
        append_admitted_header(
            &mut history,
            writer(2),
            VersionVector::new([
                (authority_actor, 1),
                (downgraded_actor, 1),
                (losing_actor, 1),
            ])
            .expect("downgraded operation"),
            [0x21; 32],
        );
        append_admitted_header(
            &mut history,
            writer(3),
            VersionVector::new([
                (authority_actor, 1),
                (downgraded_actor, 1),
                (losing_actor, 2),
            ])
            .expect("loss operation two"),
            [0x32; 32],
        );

        let authority_frontier = [
            ClockEntry::new(writer(1), 2).expect("authority frontier"),
            ClockEntry::new(writer(3), 1).expect("loss frontier one"),
        ];
        let authority_tips = [
            WriterCutoff::new(writer(2), 0, [0; 32]).expect("zero downgraded tip"),
            WriterCutoff::new(writer(3), 1, [0x31; 32]).expect("first loss tip"),
        ];
        let authority_receipt = encode_signed_freeze_receipt(
            &key(1),
            &proposal,
            &authority,
            &authority_frontier,
            &authority_tips,
        )
        .expect("authority receipt");
        let downgraded_frontier = [
            ClockEntry::new(writer(1), 1).expect("downgraded authority"),
            ClockEntry::new(writer(2), 1).expect("downgraded writer"),
            ClockEntry::new(writer(3), 2).expect("downgraded loss"),
        ];
        let joined_frontier = vec![
            ClockEntry::new(writer(1), 2).expect("joined authority"),
            ClockEntry::new(writer(2), 1).expect("joined downgraded"),
            ClockEntry::new(writer(3), 2).expect("joined loss"),
        ];
        let exact_cutoffs = vec![
            WriterCutoff::new(writer(2), 1, [0x21; 32]).expect("downgraded cutoff"),
            WriterCutoff::new(writer(3), 2, [0x32; 32]).expect("maximum loss cutoff"),
        ];
        let downgraded_receipt = encode_signed_freeze_receipt(
            &key(2),
            &proposal,
            &downgraded_base,
            &downgraded_frontier,
            &exact_cutoffs,
        )
        .expect("downgraded survivor receipt");
        let mut receipt_digests = vec![
            *blake3::hash(&authority_receipt).as_bytes(),
            *blake3::hash(&downgraded_receipt).as_bytes(),
        ];
        receipt_digests.sort_unstable();
        let next_record = epoch_record(
            3,
            Some(base.digest()),
            &[authority, downgraded_next],
            Some(proposal.digest().to_bytes()),
            &joined_frontier,
            &exact_cutoffs,
            &receipt_digests,
            &[],
        );
        MultiSurvivorWriteLossFixture {
            base_record: base_record.clone(),
            next_record,
            proposal_record,
            authority_receipt,
            downgraded_receipt,
            joined_frontier,
            exact_cutoffs,
            receipt_digests,
            archive: TestArchive::for_base(&base_record),
            history,
        }
    }

    #[test]
    fn write_loss_requires_exact_all_survivor_receipts_and_history() {
        let (base, next, proposal, receipt, archive, history) = write_loss_fixture();
        let receipts = [FreezeReceiptEvidence::new(writer(1), &receipt)];
        let raw = WriteLossTransitionEvidence::new(&proposal, &receipts);
        let plan = validate(
            &base,
            &next,
            &archive,
            &history,
            MembershipTransitionEvidence::WriteLoss(raw),
        )
        .expect("write loss");
        let checked = plan.write_loss().expect("write loss plan");
        assert_eq!(
            checked
                .accepted_frontier()
                .counter(writer(2).into_vector_actor()),
            1
        );
        assert_eq!(checked.cutoffs()[0].operation_digest(), [5; 32]);

        let missing = WriteLossTransitionEvidence::new(&proposal, &[]);
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(missing),
            ),
            Err(MembershipTransitionError::InvalidFreezeReceiptSet)
        );
    }

    #[test]
    fn multi_survivor_write_loss_joins_frontiers_and_requires_downgraded_signer() {
        let fixture = multi_survivor_write_loss_fixture();
        let proposal = decode_signature_checked_write_loss_proposal(
            &fixture.proposal_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("proposal");
        let authority_receipt = decode_signature_checked_freeze_receipt(
            &fixture.authority_receipt,
            &proposal,
            &member(1, MemberRole::ReadWrite),
        )
        .expect("authority receipt");
        let downgraded_receipt = decode_signature_checked_freeze_receipt(
            &fixture.downgraded_receipt,
            &proposal,
            &member(2, MemberRole::ReadWrite),
        )
        .expect("downgraded survivor receipt");
        assert_eq!(authority_receipt.losing_writer_tips()[1].counter(), 1);
        assert_eq!(downgraded_receipt.losing_writer_tips()[1].counter(), 2);
        assert_eq!(
            authority_receipt
                .frontier()
                .compare(downgraded_receipt.frontier()),
            crate::sync::VersionVectorOrder::Concurrent
        );

        let receipts = [
            FreezeReceiptEvidence::new(writer(1), &fixture.authority_receipt),
            FreezeReceiptEvidence::new(writer(2), &fixture.downgraded_receipt),
        ];
        let plan = validate(
            &fixture.base_record,
            &fixture.next_record,
            &fixture.archive,
            &fixture.history,
            MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                &fixture.proposal_record,
                &receipts,
            )),
        )
        .expect("complete multi-survivor freeze");
        let checked = plan.write_loss().expect("write-loss plan");
        assert_eq!(
            checked.accepted_frontier(),
            &VersionVector::new(
                fixture
                    .joined_frontier
                    .iter()
                    .map(|entry| (entry.writer_id().into_vector_actor(), entry.counter())),
            )
            .expect("joined frontier")
        );
        assert_eq!(checked.cutoffs(), fixture.exact_cutoffs);
        assert_eq!(checked.freeze_receipt_digests(), fixture.receipt_digests);

        let missing_downgraded = [FreezeReceiptEvidence::new(
            writer(1),
            &fixture.authority_receipt,
        )];
        assert_eq!(
            validate(
                &fixture.base_record,
                &fixture.next_record,
                &fixture.archive,
                &fixture.history,
                MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                    &fixture.proposal_record,
                    &missing_downgraded,
                )),
            ),
            Err(MembershipTransitionError::InvalidFreezeReceiptSet)
        );

        let duplicate_authority = [
            FreezeReceiptEvidence::new(writer(1), &fixture.authority_receipt),
            FreezeReceiptEvidence::new(writer(1), &fixture.authority_receipt),
        ];
        assert_eq!(
            validate(
                &fixture.base_record,
                &fixture.next_record,
                &fixture.archive,
                &fixture.history,
                MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                    &fixture.proposal_record,
                    &duplicate_authority,
                )),
            ),
            Err(MembershipTransitionError::InvalidFreezeReceiptSet)
        );
    }

    #[test]
    fn multi_survivor_write_loss_rejects_stale_receipt_and_maximum_tip_digest_mismatch() {
        let fixture = multi_survivor_write_loss_fixture();
        let base = decode_epoch(&fixture.base_record).expect("base");
        let stale_record = encode_signed_write_loss_proposal(
            &key(1),
            folder(),
            base.epoch(),
            base.digest(),
            writer(1),
            [0x72; 32],
            &[
                WriteLossChange::new(writer(2), WriteLossAction::DowngradeToRead),
                WriteLossChange::new(writer(3), WriteLossAction::Remove),
            ],
            &[writer(1), writer(2)],
            &[writer(2), writer(3)],
        )
        .expect("different proposal");
        let stale_proposal = decode_signature_checked_write_loss_proposal(
            &stale_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("checked stale proposal");
        let stale_receipt = encode_signed_freeze_receipt(
            &key(2),
            &stale_proposal,
            &member(2, MemberRole::ReadWrite),
            &fixture.joined_frontier,
            &fixture.exact_cutoffs,
        )
        .expect("stale receipt");
        let mismatched_proposal_receipts = [
            FreezeReceiptEvidence::new(writer(1), &fixture.authority_receipt),
            FreezeReceiptEvidence::new(writer(2), &stale_receipt),
        ];
        assert_eq!(
            validate(
                &fixture.base_record,
                &fixture.next_record,
                &fixture.archive,
                &fixture.history,
                MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                    &fixture.proposal_record,
                    &mismatched_proposal_receipts,
                )),
            ),
            Err(MembershipTransitionError::InvalidFreezeReceipt)
        );

        let proposal = decode_signature_checked_write_loss_proposal(
            &fixture.proposal_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .expect("proposal");
        let mut wrong_cutoffs = fixture.exact_cutoffs.clone();
        wrong_cutoffs[1] = WriterCutoff::new(writer(3), 2, [0x33; 32]).expect("wrong maximum");
        let wrong_next = epoch_record(
            3,
            Some(base.digest()),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::Read),
            ],
            Some(proposal.digest().to_bytes()),
            &fixture.joined_frontier,
            &wrong_cutoffs,
            &fixture.receipt_digests,
            &[],
        );
        let valid_receipts = [
            FreezeReceiptEvidence::new(writer(1), &fixture.authority_receipt),
            FreezeReceiptEvidence::new(writer(2), &fixture.downgraded_receipt),
        ];
        assert_eq!(
            validate(
                &fixture.base_record,
                &wrong_next,
                &fixture.archive,
                &fixture.history,
                MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                    &fixture.proposal_record,
                    &valid_receipts,
                )),
            ),
            Err(MembershipTransitionError::CutoffMismatch)
        );
    }

    #[test]
    fn write_loss_rejects_missing_or_wrong_tip_history() {
        let (base, next, proposal, receipt, archive, mut history) = write_loss_fixture();
        let receipts = [FreezeReceiptEvidence::new(writer(1), &receipt)];
        let raw = WriteLossTransitionEvidence::new(&proposal, &receipts);
        history.headers.clear();
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(raw),
            ),
            Err(MembershipTransitionError::FreezeFrontierNotClosed)
        );

        let (_, _, _, _, _, mut history) = write_loss_fixture();
        history
            .headers
            .values_mut()
            .next()
            .unwrap()
            .clone_from(&Header::new(
                OpId::new(writer(2).into_vector_actor(), 1).unwrap(),
                VersionVector::new([(writer(2).into_vector_actor(), 1)]).unwrap(),
                [6; 32],
                None,
            ));
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(raw),
            ),
            Err(MembershipTransitionError::WrongTipDigest)
        );
    }

    #[test]
    fn write_loss_next_epoch_must_commit_exact_join_cutoff_and_receipt_set() {
        let (base_record, _, proposal_record, receipt, archive, history) = write_loss_fixture();
        let base = decode_epoch(&base_record).unwrap();
        let proposal = decode_signature_checked_write_loss_proposal(
            &proposal_record,
            folder(),
            writer(1),
            &key(1).verifying_key(),
        )
        .unwrap();
        let receipts = [FreezeReceiptEvidence::new(writer(1), &receipt)];
        let raw = WriteLossTransitionEvidence::new(&proposal_record, &receipts);
        let exact_frontier = [ClockEntry::new(writer(2), 1).unwrap()];
        let exact_cutoff = [WriterCutoff::new(writer(2), 1, [5; 32]).unwrap()];
        let receipt_digests = [*blake3::hash(&receipt).as_bytes()];

        let wrong_frontier = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite)],
            Some(proposal.digest().to_bytes()),
            &[ClockEntry::new(writer(2), 2).unwrap()],
            &[WriterCutoff::new(writer(2), 2, [5; 32]).unwrap()],
            &receipt_digests,
            &[],
        );
        assert_eq!(
            validate(
                &base_record,
                &wrong_frontier,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(raw),
            ),
            Err(MembershipTransitionError::AcceptedFrontierMismatch)
        );

        let wrong_cutoff = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite)],
            Some(proposal.digest().to_bytes()),
            &exact_frontier,
            &[WriterCutoff::new(writer(2), 1, [6; 32]).unwrap()],
            &receipt_digests,
            &[],
        );
        assert_eq!(
            validate(
                &base_record,
                &wrong_cutoff,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(raw),
            ),
            Err(MembershipTransitionError::CutoffMismatch)
        );

        let wrong_receipt_set = epoch_record(
            3,
            Some(base.digest()),
            &[member(1, MemberRole::ReadWrite)],
            Some(proposal.digest().to_bytes()),
            &exact_frontier,
            &exact_cutoff,
            &[[0x44; 32]],
            &[],
        );
        assert_eq!(
            validate(
                &base_record,
                &wrong_receipt_set,
                &archive,
                &history,
                MembershipTransitionEvidence::WriteLoss(raw),
            ),
            Err(MembershipTransitionError::FreezeReceiptSetMismatch)
        );
    }

    #[test]
    fn survivor_activation_rejects_local_history_beyond_agreed_cutoff() {
        let (base, next, proposal, receipt, archive, history) = write_loss_fixture();
        let receipts = [FreezeReceiptEvidence::new(writer(1), &receipt)];
        let plan = validate(
            &base,
            &next,
            &archive,
            &history,
            MembershipTransitionEvidence::WriteLoss(WriteLossTransitionEvidence::new(
                &proposal, &receipts,
            )),
        )
        .expect("validated plan");
        let losing_actor = writer(2).into_vector_actor();

        let mut compatible = history;
        compatible
            .tips
            .insert(losing_actor, AuthorTip::new(1, [5; 32]).unwrap());
        assert_eq!(
            check_survivor_history_compatibility(&compatible, &plan, writer(1)),
            Ok(())
        );

        compatible
            .tips
            .insert(losing_actor, AuthorTip::new(1, [6; 32]).unwrap());
        assert_eq!(
            check_survivor_history_compatibility(&compatible, &plan, writer(1)),
            Err(MembershipTransitionError::LocalHistoryTipMismatch)
        );

        compatible
            .tips
            .insert(losing_actor, AuthorTip::new(2, [7; 32]).unwrap());
        let second_id = OpId::new(losing_actor, 2).unwrap();
        compatible.headers.insert(
            second_id,
            Header::new(
                second_id,
                VersionVector::new([(losing_actor, 2)]).unwrap(),
                [7; 32],
                Some([5; 32]),
            ),
        );
        assert_eq!(
            check_survivor_history_compatibility(&compatible, &plan, writer(1)),
            Err(MembershipTransitionError::LocalHistoryBeyondCutoff)
        );
        assert_eq!(
            check_survivor_history_compatibility(&compatible, &plan, writer(2)),
            Err(MembershipTransitionError::LocalWriterNotSurvivor)
        );

        compatible.unavailable = true;
        assert_eq!(
            check_survivor_history_compatibility(&compatible, &plan, writer(1)),
            Err(MembershipTransitionError::LocalHistoryUnavailable)
        );
    }

    #[test]
    fn archive_rejects_unknown_head_removed_id_old_key_and_actor_exhaustion() {
        let (base, next, permit, receipt, mut archive) = add_fixture();
        let evidence = [BootstrapTransitionEvidence::new(
            writer(2),
            &permit,
            &receipt,
        )];
        archive.failure = Some(MembershipArchiveQueryError::UnknownBaseHead);
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::ArchiveUnknownBase)
        );

        let mut archive = TestArchive::for_base(&base);
        archive.retained_count = 0;
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::InvalidRetainedWriterCount)
        );

        let mut archive = TestArchive::for_base(&base);
        archive.lifetimes.insert(writer(2), WriterLifetime::Removed);
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::ReusedWriterId)
        );

        let mut archive = TestArchive::for_base(&base);
        archive.key_assignments.insert(
            key(2).verifying_key().to_bytes(),
            HistoricalWriterKeyAssignment::AssignedTo(writer(99)),
        );
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::ReusedHistoricalWriterKey)
        );

        let mut archive = TestArchive::for_base(&base);
        archive.retained_count = 128;
        assert_eq!(
            validate(
                &base,
                &next,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&evidence),
            ),
            Err(MembershipTransitionError::RetainedWriterLimit)
        );
    }

    #[test]
    fn exact_chain_bindings_and_surviving_keys_are_rechecked() {
        let (base, _, _, _, archive) = add_fixture();
        let checked = decode_epoch(&base).unwrap();
        let no_op = epoch_record(
            2,
            Some(checked.digest()),
            &[member(1, MemberRole::ReadWrite)],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            validate(
                &base,
                &no_op,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Ordinary,
            ),
            Err(MembershipTransitionError::NoRosterChange)
        );

        let changed_authority_transport = MemberGrant::new(
            writer(1),
            key(1).verifying_key(),
            device(88),
            key(188).verifying_key(),
            MemberRole::ReadWrite,
        )
        .unwrap();
        let changed = epoch_record(
            2,
            Some(checked.digest()),
            &[changed_authority_transport],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            validate(
                &base,
                &changed,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Ordinary,
            ),
            Err(MembershipTransitionError::SurvivingBindingChanged)
        );

        let wrong_previous = epoch_record(
            2,
            Some(EpochDigest::from_bytes([3; 32])),
            &[
                member(1, MemberRole::ReadWrite),
                member(2, MemberRole::Read),
            ],
            None,
            &[],
            &[],
            &[],
            &[],
        );
        assert_eq!(
            validate(
                &base,
                &wrong_previous,
                &archive,
                &TestHistory::default(),
                MembershipTransitionEvidence::Ordinary,
            ),
            Err(MembershipTransitionError::WrongPreviousEpoch)
        );
    }

    #[test]
    fn evidence_bounds_are_checked_before_record_decoding() {
        let oversized = vec![0; MAX_MEMBERSHIP_TRANSITION_EVIDENCE_BYTES + 1];
        assert_eq!(
            validate(
                &oversized,
                &[],
                &TestArchive::default(),
                &TestHistory::default(),
                MembershipTransitionEvidence::Ordinary,
            ),
            Err(MembershipTransitionError::EvidenceTooLarge)
        );

        let records = vec![BootstrapTransitionEvidence::new(writer(2), &[], &[]); 129];
        assert_eq!(
            validate(
                &[],
                &[],
                &TestArchive::default(),
                &TestHistory::default(),
                MembershipTransitionEvidence::Bootstrap(&records),
            ),
            Err(MembershipTransitionError::TooManyEvidenceRecords)
        );
    }
}
