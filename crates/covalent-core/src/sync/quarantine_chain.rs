//! Exact backward same-writer chain proofs for future quarantine reconciliation.
//!
//! This verifier authenticates canonical operation records and their bodies,
//! then follows exact signed-record digests and predecessor commitments from a
//! freeze receipt's losing-writer tip back to already-admitted local history.
//! The result proves only that bounded same-writer suffix. It does not prove
//! causal closure, membership or epoch continuity, content retention or apply,
//! a durable quarantine, receipt completeness, or authority to activate an
//! epoch. A later all-receipt reconciliation transaction must prove those
//! conditions before it promotes any operation.

use std::fmt;

use ed25519_dalek::VerifyingKey;
use thiserror::Error;

use crate::engine::{JobControl, JobState};

use super::admission::{History, HistoryQueryError};
use super::body::OperationBody;
use super::freeze::{SignatureCheckedFreezeReceipt, SignatureCheckedWriteLossProposal};
use super::ids::{FolderId, WriterId};
use super::membership::WriterCutoff;
use super::operation::{SignatureCheckedOperation, decode_signature_checked_operation};
use super::register::OpId;

/// Absolute record-count ceiling for one in-memory proof invocation.
pub const MAX_QUARANTINE_CHAIN_RECORDS: usize = 4_096;
/// Absolute raw-input byte ceiling for one in-memory proof invocation.
///
/// This bounds borrowed input bytes, not total caller or output RSS. The strict
/// operation decoder also enforces its independent per-record limit.
pub const MAX_QUARANTINE_CHAIN_INPUT_BYTES: u64 = 64 * 1_024 * 1_024;

/// Caller-selected proof limits under the fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QuarantineChainLimits {
    maximum_records: usize,
    maximum_input_bytes: u64,
}

impl QuarantineChainLimits {
    /// Creates nonzero limits no larger than the fixed implementation ceilings.
    pub fn new(
        maximum_records: usize,
        maximum_input_bytes: u64,
    ) -> Result<Self, QuarantineChainError> {
        if maximum_records == 0
            || maximum_records > MAX_QUARANTINE_CHAIN_RECORDS
            || maximum_input_bytes == 0
            || maximum_input_bytes > MAX_QUARANTINE_CHAIN_INPUT_BYTES
        {
            return Err(QuarantineChainError::InvalidLimits);
        }
        Ok(Self {
            maximum_records,
            maximum_input_bytes,
        })
    }

    /// Returns the maximum missing records accepted in one proof.
    #[must_use]
    pub const fn maximum_records(self) -> usize {
        self.maximum_records
    }

    /// Returns the maximum aggregate raw record bytes accepted in one proof.
    #[must_use]
    pub const fn maximum_input_bytes(self) -> u64 {
        self.maximum_input_bytes
    }
}

/// One exact already-admitted anchor, or the canonical zero boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WriterChainPoint {
    counter: u64,
    digest: [u8; 32],
}

impl WriterChainPoint {
    const ZERO: Self = Self {
        counter: 0,
        digest: [0; 32],
    };

    /// Returns zero for the boundary before a writer's first operation.
    #[must_use]
    pub const fn counter(self) -> u64 {
        self.counter
    }

    /// Returns the exact signed-record digest, or all zero at counter zero.
    #[must_use]
    pub const fn digest(self) -> [u8; 32] {
        self.digest
    }
}

/// One signature-checked, canonically decoded operation in a backward proof.
///
/// This still carries no membership, epoch, causal, retention, or apply grant.
pub struct ExactBackwardWriterRecord {
    operation: SignatureCheckedOperation,
    body: OperationBody,
}

impl ExactBackwardWriterRecord {
    /// Returns signature-checked operation fields and canonical record bytes.
    #[must_use]
    pub const fn operation(&self) -> &SignatureCheckedOperation {
        &self.operation
    }

    /// Returns the canonical body proven to be well-formed by this verifier.
    #[must_use]
    pub const fn body(&self) -> &OperationBody {
        &self.body
    }
}

impl fmt::Debug for ExactBackwardWriterRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactBackwardWriterRecord")
            .field("counter", &self.operation.header().counter())
            .field("record_length", &self.operation.canonical_record().len())
            .finish()
    }
}

/// A bounded exact same-writer suffix, ordered receipt-tip first.
///
/// The fields are private and this type deliberately does not implement
/// [`History`]. Its records remain non-authorizing proof material for a future
/// reconciliation validator.
pub struct ExactBackwardWriterChain {
    folder_id: FolderId,
    writer_id: WriterId,
    receipt_tip: WriterChainPoint,
    anchor: WriterChainPoint,
    records_tip_first: Vec<ExactBackwardWriterRecord>,
}

impl ExactBackwardWriterChain {
    /// Returns the folder namespace checked on every operation.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns the historical losing writer checked on every operation.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the exact losing-writer tip committed by the receipt.
    #[must_use]
    pub const fn receipt_tip(&self) -> WriterChainPoint {
        self.receipt_tip
    }

    /// Returns the exact admitted boundary reached by this proof.
    #[must_use]
    pub const fn anchor(&self) -> WriterChainPoint {
        self.anchor
    }

    /// Returns missing records in strict receipt-tip-to-anchor order.
    #[must_use]
    pub fn records_tip_first(&self) -> &[ExactBackwardWriterRecord] {
        &self.records_tip_first
    }
}

impl fmt::Debug for ExactBackwardWriterChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactBackwardWriterChain")
            .field("folder_id", &self.folder_id)
            .field("writer_id", &self.writer_id)
            .field("receipt_tip_counter", &self.receipt_tip.counter)
            .field("anchor_counter", &self.anchor.counter)
            .field("record_count", &self.records_tip_first.len())
            .finish()
    }
}

/// Fixed proof failures that never expose signed bytes, bodies, or paths.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum QuarantineChainError {
    /// Caller limits were zero or exceeded the fixed implementation ceilings.
    #[error("quarantine chain limits are invalid")]
    InvalidLimits,
    /// The supplied proof has too many records or the missing suffix is too long.
    #[error("quarantine chain exceeds its record limit")]
    TooManyRecords,
    /// Aggregate supplied raw record bytes exceed the caller limit.
    #[error("quarantine chain exceeds its byte limit")]
    TooManyBytes,
    /// The caller requested a resumable pause.
    #[error("quarantine chain verification is paused")]
    Paused,
    /// The caller cancelled verification.
    #[error("quarantine chain verification is cancelled")]
    Cancelled,
    /// The receipt is not bound to the exact supplied proposal and folder.
    #[error("quarantine receipt and proposal bindings do not match")]
    EvidenceMismatch,
    /// The selected writer is not an exact losing-writer receipt tip.
    #[error("quarantine receipt has no matching losing-writer tip")]
    MissingWriterTip,
    /// The historical writer key cannot be used for strict verification.
    #[error("quarantine writer key is invalid")]
    InvalidWriterKey,
    /// A bounded exact-history query could not complete.
    #[error("quarantine anchor history is unavailable")]
    HistoryUnavailable,
    /// History did not return a structurally valid exact admitted anchor.
    #[error("quarantine anchor history is inconsistent")]
    CorruptHistory,
    /// An older receipt tip is absent or has a different retained digest.
    #[error("quarantine receipt tip is not exact admitted history")]
    ReceiptTipFork,
    /// The supplied record count does not exactly span tip to anchor.
    #[error("quarantine chain has a gap or unused record")]
    RecordCountMismatch,
    /// A record failed strict signature, canonicality, folder, or writer checks.
    #[error("quarantine chain contains an invalid signed operation")]
    InvalidOperation,
    /// A signed operation contains a malformed or noncanonical body.
    #[error("quarantine chain contains an invalid operation body")]
    InvalidBody,
    /// A record has the wrong dot, digest, order, or predecessor commitment.
    #[error("quarantine chain does not link to its exact anchor")]
    ChainMismatch,
}

/// Verifies one exact backwards losing-writer suffix without mutating history.
///
/// `records_tip_first` must contain exactly the records absent from admitted
/// local history, starting at the receipt tip and descending by one counter. If
/// the receipt tip is already admitted, the exact retained header is checked
/// and the proof must be empty. A zero receipt tip likewise requires an empty
/// proof but makes no statement about newer local history.
#[allow(clippy::too_many_arguments)]
pub fn verify_exact_backward_writer_chain(
    history: &impl History,
    receipt: &SignatureCheckedFreezeReceipt,
    proposal: &SignatureCheckedWriteLossProposal,
    expected_folder: FolderId,
    expected_writer: WriterId,
    historical_writer_key: &VerifyingKey,
    records_tip_first: &[&[u8]],
    limits: QuarantineChainLimits,
    control: &JobControl,
) -> Result<ExactBackwardWriterChain, QuarantineChainError> {
    verify_inner(
        history,
        receipt,
        proposal,
        expected_folder,
        expected_writer,
        historical_writer_key,
        records_tip_first,
        limits,
        || check_control(control),
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_inner(
    history: &impl History,
    receipt: &SignatureCheckedFreezeReceipt,
    proposal: &SignatureCheckedWriteLossProposal,
    expected_folder: FolderId,
    expected_writer: WriterId,
    historical_writer_key: &VerifyingKey,
    records_tip_first: &[&[u8]],
    limits: QuarantineChainLimits,
    mut check: impl FnMut() -> Result<(), QuarantineChainError>,
) -> Result<ExactBackwardWriterChain, QuarantineChainError> {
    check()?;
    if records_tip_first.len() > limits.maximum_records {
        return Err(QuarantineChainError::TooManyRecords);
    }
    let mut aggregate_bytes = 0_u64;
    for record in records_tip_first {
        check()?;
        aggregate_bytes = aggregate_bytes
            .checked_add(record.len() as u64)
            .ok_or(QuarantineChainError::TooManyBytes)?;
        if aggregate_bytes > limits.maximum_input_bytes {
            return Err(QuarantineChainError::TooManyBytes);
        }
    }
    check()?;

    if receipt.proposal_digest() != proposal.digest()
        || receipt.folder_id() != expected_folder
        || proposal.folder_id() != expected_folder
        || receipt.base_epoch() != proposal.base_epoch()
        || receipt.base_epoch_digest() != proposal.base_epoch_digest()
    {
        return Err(QuarantineChainError::EvidenceMismatch);
    }
    if historical_writer_key.is_weak() {
        return Err(QuarantineChainError::InvalidWriterKey);
    }
    let cutoff = receipt
        .losing_writer_tips()
        .binary_search_by_key(&expected_writer, |candidate| candidate.writer_id())
        .ok()
        .map(|index| receipt.losing_writer_tips()[index])
        .filter(|candidate| {
            proposal
                .losing_writer_ids()
                .binary_search(&candidate.writer_id())
                .is_ok()
        })
        .ok_or(QuarantineChainError::MissingWriterTip)?;
    let receipt_tip = point_from_cutoff(cutoff);

    if receipt_tip.counter == 0 {
        if !records_tip_first.is_empty() {
            return Err(QuarantineChainError::RecordCountMismatch);
        }
        return Ok(ExactBackwardWriterChain {
            folder_id: expected_folder,
            writer_id: expected_writer,
            receipt_tip,
            anchor: WriterChainPoint::ZERO,
            records_tip_first: Vec::new(),
        });
    }

    check()?;
    let admitted_tip = history
        .author_tip(expected_writer.into_vector_actor())
        .map_err(map_history_error)?;
    if admitted_tip.is_some_and(|tip| tip.counter() >= receipt_tip.counter) {
        let admitted_receipt = exact_history_point(
            history,
            expected_writer,
            receipt_tip.counter,
            QuarantineChainError::ReceiptTipFork,
            &mut check,
        )?;
        if admitted_receipt.digest != receipt_tip.digest {
            return Err(QuarantineChainError::ReceiptTipFork);
        }
        if !records_tip_first.is_empty() {
            return Err(QuarantineChainError::RecordCountMismatch);
        }
        return Ok(ExactBackwardWriterChain {
            folder_id: expected_folder,
            writer_id: expected_writer,
            receipt_tip,
            anchor: admitted_receipt,
            records_tip_first: Vec::new(),
        });
    }

    let anchor = match admitted_tip {
        Some(tip) => {
            let point = exact_history_point(
                history,
                expected_writer,
                tip.counter(),
                QuarantineChainError::CorruptHistory,
                &mut check,
            )?;
            if point.digest != tip.digest() {
                return Err(QuarantineChainError::CorruptHistory);
            }
            point
        }
        None => WriterChainPoint::ZERO,
    };
    let missing = receipt_tip
        .counter
        .checked_sub(anchor.counter)
        .ok_or(QuarantineChainError::CorruptHistory)?;
    if missing > limits.maximum_records as u64 {
        return Err(QuarantineChainError::TooManyRecords);
    }
    if missing != records_tip_first.len() as u64 {
        return Err(QuarantineChainError::RecordCountMismatch);
    }

    let mut expected_counter = receipt_tip.counter;
    let mut expected_digest = Some(receipt_tip.digest);
    let mut checked_records = Vec::with_capacity(records_tip_first.len());
    for record in records_tip_first {
        check()?;
        let operation = decode_signature_checked_operation(
            record,
            expected_folder,
            expected_writer,
            historical_writer_key,
        )
        .map_err(|_| QuarantineChainError::InvalidOperation)?;
        let body = OperationBody::decode(operation.body())
            .map_err(|_| QuarantineChainError::InvalidBody)?;
        if operation.header().counter() != expected_counter
            || Some(operation.digest().to_bytes()) != expected_digest
        {
            return Err(QuarantineChainError::ChainMismatch);
        }
        expected_counter -= 1;
        expected_digest = operation.header().predecessor();
        checked_records.push(ExactBackwardWriterRecord { operation, body });
    }
    check()?;
    let expected_anchor_digest = (anchor.counter != 0).then_some(anchor.digest);
    if expected_counter != anchor.counter || expected_digest != expected_anchor_digest {
        return Err(QuarantineChainError::ChainMismatch);
    }

    Ok(ExactBackwardWriterChain {
        folder_id: expected_folder,
        writer_id: expected_writer,
        receipt_tip,
        anchor,
        records_tip_first: checked_records,
    })
}

fn exact_history_point(
    history: &impl History,
    writer: WriterId,
    counter: u64,
    missing: QuarantineChainError,
    check: &mut impl FnMut() -> Result<(), QuarantineChainError>,
) -> Result<WriterChainPoint, QuarantineChainError> {
    check()?;
    let id = OpId::new(writer.into_vector_actor(), counter)
        .map_err(|_| QuarantineChainError::CorruptHistory)?;
    let header = history
        .header(id)
        .map_err(map_history_error)?
        .ok_or(missing)?;
    check()?;
    if header.id() != id || header.clock().counter(id.actor()) != counter {
        return Err(QuarantineChainError::CorruptHistory);
    }
    Ok(WriterChainPoint {
        counter,
        digest: header.digest(),
    })
}

const fn point_from_cutoff(cutoff: WriterCutoff) -> WriterChainPoint {
    WriterChainPoint {
        counter: cutoff.counter(),
        digest: cutoff.operation_digest(),
    }
}

const fn map_history_error(_: HistoryQueryError) -> QuarantineChainError {
    QuarantineChainError::HistoryUnavailable
}

fn check_control(control: &JobControl) -> Result<(), QuarantineChainError> {
    match control.state() {
        JobState::Running => Ok(()),
        JobState::Paused => Err(QuarantineChainError::Paused),
        JobState::Cancelled => Err(QuarantineChainError::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use covalent_protocol::DeviceId;
    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::VersionVector;
    use crate::sync::admission::{AuthorTip, Header};
    use crate::sync::body::EntryValue;
    use crate::sync::freeze::{
        WriteLossAction, WriteLossChange, decode_signature_checked_freeze_receipt,
        decode_signature_checked_write_loss_proposal, encode_signed_freeze_receipt,
        encode_signed_write_loss_proposal,
    };
    use crate::sync::membership::{EpochDigest, MemberGrant, MemberRole};
    use crate::sync::operation::{ClockEntry, encode_signed_operation};
    use crate::sync::path::SyncPath;

    const EPOCH_DIGEST: EpochDigest = EpochDigest::from_bytes([11; 32]);

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn folder() -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(1))
    }

    fn other_folder() -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(2))
    }

    fn authority_writer() -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(10))
    }

    fn losing_writer() -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(20))
    }

    fn other_writer() -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(30))
    }

    fn authority_key() -> SigningKey {
        key(21)
    }

    fn losing_key() -> SigningKey {
        key(22)
    }

    fn authority_grant() -> MemberGrant {
        MemberGrant::new(
            authority_writer(),
            authority_key().verifying_key(),
            DeviceId::from_uuid(Uuid::from_u128(100)),
            key(23).verifying_key(),
            MemberRole::ReadWrite,
        )
        .expect("authority grant")
    }

    struct TestHistory {
        tip: Option<AuthorTip>,
        headers: BTreeMap<OpId, Header>,
        queries: Cell<usize>,
        unavailable: bool,
    }

    impl TestHistory {
        fn empty() -> Self {
            Self {
                tip: None,
                headers: BTreeMap::new(),
                queries: Cell::new(0),
                unavailable: false,
            }
        }

        fn through(records: &[Vec<u8>], count: usize) -> Self {
            let mut history = Self::empty();
            for record in records.iter().take(count) {
                let operation = decode_signature_checked_operation(
                    record,
                    folder(),
                    losing_writer(),
                    &losing_key().verifying_key(),
                )
                .expect("checked history operation");
                let header = operation.to_admission_header().expect("history header");
                history.tip = Some(
                    AuthorTip::new(header.id().counter(), header.digest()).expect("history tip"),
                );
                history.headers.insert(header.id(), header);
            }
            history
        }
    }

    impl History for TestHistory {
        fn author_tip(&self, _writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError> {
            self.queries.set(self.queries.get() + 1);
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self.tip)
        }

        fn header(&self, id: OpId) -> Result<Option<Header>, HistoryQueryError> {
            self.queries.set(self.queries.get() + 1);
            if self.unavailable {
                return Err(HistoryQueryError::Unavailable);
            }
            Ok(self.headers.get(&id).cloned())
        }
    }

    struct Fixture {
        proposal: SignatureCheckedWriteLossProposal,
        receipt: SignatureCheckedFreezeReceipt,
        records: Vec<Vec<u8>>,
    }

    fn checked_proposal() -> SignatureCheckedWriteLossProposal {
        checked_proposal_with_nonce([31; 32])
    }

    fn checked_proposal_with_nonce(nonce: [u8; 32]) -> SignatureCheckedWriteLossProposal {
        let record = encode_signed_write_loss_proposal(
            &authority_key(),
            folder(),
            1,
            EPOCH_DIGEST,
            authority_writer(),
            nonce,
            &[WriteLossChange::new(
                losing_writer(),
                WriteLossAction::Remove,
            )],
            &[authority_writer()],
            &[losing_writer()],
        )
        .expect("proposal");
        decode_signature_checked_write_loss_proposal(
            &record,
            folder(),
            authority_writer(),
            &authority_key().verifying_key(),
        )
        .expect("checked proposal")
    }

    fn operation(
        folder_id: FolderId,
        writer: WriterId,
        signing_key: &SigningKey,
        counter: u64,
        predecessor: Option<[u8; 32]>,
        body: &[u8],
    ) -> Vec<u8> {
        encode_signed_operation(
            signing_key,
            folder_id,
            writer,
            1,
            EPOCH_DIGEST.to_bytes(),
            counter,
            predecessor,
            &[ClockEntry::new(writer, counter).expect("clock")],
            body,
        )
        .expect("operation")
    }

    fn canonical_body(counter: u64) -> Vec<u8> {
        OperationBody::new(
            SyncPath::from_wire(format!("entry-{counter}")).expect("path"),
            EntryValue::Directory,
        )
        .encode()
    }

    fn operation_chain(count: u64) -> Vec<Vec<u8>> {
        let mut records = Vec::new();
        let mut predecessor = None;
        for counter in 1..=count {
            let record = operation(
                folder(),
                losing_writer(),
                &losing_key(),
                counter,
                predecessor,
                &canonical_body(counter),
            );
            let checked = decode_signature_checked_operation(
                &record,
                folder(),
                losing_writer(),
                &losing_key().verifying_key(),
            )
            .expect("checked operation");
            predecessor = Some(checked.digest().to_bytes());
            records.push(record);
        }
        records
    }

    fn receipt_for(
        proposal: &SignatureCheckedWriteLossProposal,
        tip: Option<&[u8]>,
    ) -> SignatureCheckedFreezeReceipt {
        let (frontier, cutoff) = match tip {
            Some(record) => {
                let operation = decode_signature_checked_operation(
                    record,
                    folder(),
                    losing_writer(),
                    &losing_key().verifying_key(),
                )
                .expect("checked tip");
                (
                    vec![
                        ClockEntry::new(losing_writer(), operation.header().counter())
                            .expect("frontier"),
                    ],
                    WriterCutoff::new(
                        losing_writer(),
                        operation.header().counter(),
                        operation.digest().to_bytes(),
                    )
                    .expect("cutoff"),
                )
            }
            None => (
                Vec::new(),
                WriterCutoff::new(losing_writer(), 0, [0; 32]).expect("zero cutoff"),
            ),
        };
        let record = encode_signed_freeze_receipt(
            &authority_key(),
            proposal,
            &authority_grant(),
            &frontier,
            &[cutoff],
        )
        .expect("receipt");
        decode_signature_checked_freeze_receipt(&record, proposal, &authority_grant())
            .expect("checked receipt")
    }

    fn fixture(count: u64) -> Fixture {
        let proposal = checked_proposal();
        let records = operation_chain(count);
        let receipt = receipt_for(&proposal, records.last().map(Vec::as_slice));
        Fixture {
            proposal,
            receipt,
            records,
        }
    }

    fn limits(records: usize, bytes: u64) -> QuarantineChainLimits {
        QuarantineChainLimits::new(records, bytes).expect("limits")
    }

    fn refs(records: &[Vec<u8>]) -> Vec<&[u8]> {
        records.iter().rev().map(Vec::as_slice).collect()
    }

    fn verify(
        fixture: &Fixture,
        history: &TestHistory,
        supplied: &[&[u8]],
        proof_limits: QuarantineChainLimits,
    ) -> Result<ExactBackwardWriterChain, QuarantineChainError> {
        verify_exact_backward_writer_chain(
            history,
            &fixture.receipt,
            &fixture.proposal,
            folder(),
            losing_writer(),
            &losing_key().verifying_key(),
            supplied,
            proof_limits,
            &JobControl::new(),
        )
    }

    #[test]
    fn exact_missing_suffix_links_backwards_without_mutating_history() {
        let fixture = fixture(3);
        let history = TestHistory::through(&fixture.records, 1);
        let before_tip = history.tip;
        let before_headers = history.headers.clone();
        let missing = refs(&fixture.records[1..]);
        let byte_count = missing.iter().map(|record| record.len() as u64).sum();
        let chain =
            verify(&fixture, &history, &missing, limits(2, byte_count)).expect("exact suffix");
        assert_eq!(chain.folder_id(), folder());
        assert_eq!(chain.writer_id(), losing_writer());
        assert_eq!(chain.receipt_tip().counter(), 3);
        assert_eq!(chain.anchor().counter(), 1);
        assert_eq!(chain.records_tip_first().len(), 2);
        assert_eq!(
            chain.records_tip_first()[0].operation().header().counter(),
            3
        );
        assert_eq!(
            chain.records_tip_first()[1].operation().header().counter(),
            2
        );
        assert_eq!(history.tip, before_tip);
        assert_eq!(history.headers, before_headers);
        assert!(!format!("{chain:?}").contains("entry-3"));
    }

    #[test]
    fn gaps_forward_order_unused_records_and_anchor_forks_fail() {
        let fixture = fixture(3);
        let history = TestHistory::through(&fixture.records, 1);
        let backward = refs(&fixture.records[1..]);
        let generous = limits(4, 1 << 20);
        assert_eq!(
            verify(&fixture, &history, &backward[..1], generous).expect_err("gap"),
            QuarantineChainError::RecordCountMismatch
        );
        let forward = [fixture.records[1].as_slice(), fixture.records[2].as_slice()];
        assert_eq!(
            verify(&fixture, &history, &forward, generous).expect_err("forward order"),
            QuarantineChainError::ChainMismatch
        );
        let all = refs(&fixture.records);
        assert_eq!(
            verify(&fixture, &history, &all, generous).expect_err("unused anchor record"),
            QuarantineChainError::RecordCountMismatch
        );

        let alternate_first = operation(
            folder(),
            losing_writer(),
            &losing_key(),
            1,
            None,
            &OperationBody::new(
                SyncPath::from_wire("other-anchor").expect("path"),
                EntryValue::Directory,
            )
            .encode(),
        );
        let alternate_history = TestHistory::through(&[alternate_first], 1);
        assert_eq!(
            verify(&fixture, &alternate_history, &backward, generous).expect_err("anchor fork"),
            QuarantineChainError::ChainMismatch
        );
    }

    #[test]
    fn different_fork_below_receipt_tip_is_not_accepted_by_counter() {
        let fixture = fixture(3);
        let history = TestHistory::through(&fixture.records, 1);
        let real_first = decode_signature_checked_operation(
            &fixture.records[0],
            folder(),
            losing_writer(),
            &losing_key().verifying_key(),
        )
        .expect("first");
        let fork_two = operation(
            folder(),
            losing_writer(),
            &losing_key(),
            2,
            Some(real_first.digest().to_bytes()),
            &OperationBody::new(
                SyncPath::from_wire("fork-two").expect("path"),
                EntryValue::Directory,
            )
            .encode(),
        );
        let supplied = [fixture.records[2].as_slice(), fork_two.as_slice()];
        assert_eq!(
            verify(&fixture, &history, &supplied, limits(2, 1 << 20)).expect_err("fork"),
            QuarantineChainError::ChainMismatch
        );
    }

    #[test]
    fn exact_older_receipt_tip_is_resolved_but_an_older_fork_is_rejected() {
        let proposal = checked_proposal();
        let records = operation_chain(3);
        let receipt = receipt_for(&proposal, Some(&records[1]));
        let fixture = Fixture {
            proposal,
            receipt,
            records,
        };
        let history = TestHistory::through(&fixture.records, 3);
        let chain = verify(&fixture, &history, &[], limits(1, 1)).expect("older exact tip");
        assert_eq!(chain.receipt_tip().counter(), 2);
        assert_eq!(chain.anchor().counter(), 2);
        assert!(chain.records_tip_first().is_empty());

        let first = decode_signature_checked_operation(
            &fixture.records[0],
            folder(),
            losing_writer(),
            &losing_key().verifying_key(),
        )
        .expect("first");
        let alternate_two = operation(
            folder(),
            losing_writer(),
            &losing_key(),
            2,
            Some(first.digest().to_bytes()),
            &OperationBody::new(
                SyncPath::from_wire("alternate-two").expect("path"),
                EntryValue::Directory,
            )
            .encode(),
        );
        let alternate = decode_signature_checked_operation(
            &alternate_two,
            folder(),
            losing_writer(),
            &losing_key().verifying_key(),
        )
        .expect("alternate");
        let alternate_header = alternate.to_admission_header().expect("header");
        let mut forked = TestHistory::through(&fixture.records, 3);
        forked
            .headers
            .insert(alternate_header.id(), alternate_header);
        assert_eq!(
            verify(&fixture, &forked, &[], limits(1, 1)).expect_err("older fork"),
            QuarantineChainError::ReceiptTipFork
        );
    }

    #[test]
    fn zero_receipt_requires_no_records_and_does_not_claim_empty_history() {
        let proposal = checked_proposal();
        let receipt = receipt_for(&proposal, None);
        let records = operation_chain(2);
        let fixture = Fixture {
            proposal,
            receipt,
            records,
        };
        let history = TestHistory::through(&fixture.records, 2);
        let chain = verify(&fixture, &history, &[], limits(1, 1)).expect("zero receipt");
        assert_eq!(chain.receipt_tip(), WriterChainPoint::ZERO);
        assert_eq!(chain.anchor(), WriterChainPoint::ZERO);
        assert_eq!(history.queries.get(), 0);
        assert_eq!(
            verify(
                &fixture,
                &history,
                &[fixture.records[0].as_slice()],
                limits(1, 1 << 20),
            )
            .expect_err("unused zero record"),
            QuarantineChainError::RecordCountMismatch
        );
    }

    #[test]
    fn strict_operation_bindings_signatures_and_body_are_required() {
        let fixture = fixture(1);
        let history = TestHistory::empty();
        let generous = limits(1, 1 << 20);

        let mut bad_signature = fixture.records[0].clone();
        *bad_signature.last_mut().expect("signature byte") ^= 1;
        assert_eq!(
            verify(&fixture, &history, &[&bad_signature], generous).expect_err("signature"),
            QuarantineChainError::InvalidOperation
        );

        let wrong_folder = operation(
            other_folder(),
            losing_writer(),
            &losing_key(),
            1,
            None,
            &canonical_body(1),
        );
        assert_eq!(
            verify(&fixture, &history, &[&wrong_folder], generous).expect_err("folder"),
            QuarantineChainError::InvalidOperation
        );
        let wrong_writer = operation(
            folder(),
            other_writer(),
            &losing_key(),
            1,
            None,
            &canonical_body(1),
        );
        assert_eq!(
            verify(&fixture, &history, &[&wrong_writer], generous).expect_err("writer"),
            QuarantineChainError::InvalidOperation
        );
        assert_eq!(
            verify_exact_backward_writer_chain(
                &history,
                &fixture.receipt,
                &fixture.proposal,
                folder(),
                losing_writer(),
                &key(91).verifying_key(),
                &[fixture.records[0].as_slice()],
                generous,
                &JobControl::new(),
            )
            .expect_err("wrong key"),
            QuarantineChainError::InvalidOperation
        );
        let malformed_body = operation(
            folder(),
            losing_writer(),
            &losing_key(),
            1,
            None,
            b"not-a-canonical-body",
        );
        let malformed_receipt = receipt_for(&fixture.proposal, Some(&malformed_body));
        let malformed_fixture = Fixture {
            proposal: fixture.proposal.clone(),
            receipt: malformed_receipt,
            records: vec![malformed_body],
        };
        assert_eq!(
            verify(
                &malformed_fixture,
                &history,
                &[malformed_fixture.records[0].as_slice()],
                generous,
            )
            .expect_err("body"),
            QuarantineChainError::InvalidBody
        );
    }

    #[test]
    fn exact_selected_limits_succeed_and_overflow_fails_before_decoding() {
        let fixture = fixture(2);
        let history = TestHistory::empty();
        let supplied = refs(&fixture.records);
        let bytes = supplied.iter().map(|record| record.len() as u64).sum();
        verify(&fixture, &history, &supplied, limits(2, bytes)).expect("exact limits");
        assert_eq!(
            verify(&fixture, &history, &supplied, limits(1, bytes)).expect_err("count bound"),
            QuarantineChainError::TooManyRecords
        );
        assert_eq!(
            verify(&fixture, &history, &supplied, limits(2, bytes - 1)).expect_err("byte bound"),
            QuarantineChainError::TooManyBytes
        );
        assert_eq!(
            QuarantineChainLimits::new(MAX_QUARANTINE_CHAIN_RECORDS + 1, 1),
            Err(QuarantineChainError::InvalidLimits)
        );
        assert_eq!(
            QuarantineChainLimits::new(1, MAX_QUARANTINE_CHAIN_INPUT_BYTES + 1),
            Err(QuarantineChainError::InvalidLimits)
        );
    }

    #[test]
    fn cancellation_and_pause_return_no_partial_chain() {
        let fixture = fixture(2);
        let history = TestHistory::empty();
        let supplied = refs(&fixture.records);
        let control = JobControl::new();
        control.pause();
        assert_eq!(
            verify_exact_backward_writer_chain(
                &history,
                &fixture.receipt,
                &fixture.proposal,
                folder(),
                losing_writer(),
                &losing_key().verifying_key(),
                &supplied,
                limits(2, 1 << 20),
                &control,
            )
            .expect_err("pause"),
            QuarantineChainError::Paused
        );
        control.cancel();
        assert_eq!(
            verify_exact_backward_writer_chain(
                &history,
                &fixture.receipt,
                &fixture.proposal,
                folder(),
                losing_writer(),
                &losing_key().verifying_key(),
                &supplied,
                limits(2, 1 << 20),
                &control,
            )
            .expect_err("cancel"),
            QuarantineChainError::Cancelled
        );

        let checks = Cell::new(0_u32);
        let error = verify_inner(
            &history,
            &fixture.receipt,
            &fixture.proposal,
            folder(),
            losing_writer(),
            &losing_key().verifying_key(),
            &supplied,
            limits(2, 1 << 20),
            || {
                let next = checks.get() + 1;
                checks.set(next);
                if next == 7 {
                    Err(QuarantineChainError::Cancelled)
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("mid-proof cancel");
        assert_eq!(error, QuarantineChainError::Cancelled);
        assert!(checks.get() >= 7);
    }

    #[test]
    fn evidence_and_history_failures_are_fixed() {
        let fixture = fixture(1);
        let history = TestHistory::empty();
        assert_eq!(
            verify_exact_backward_writer_chain(
                &history,
                &fixture.receipt,
                &fixture.proposal,
                other_folder(),
                losing_writer(),
                &losing_key().verifying_key(),
                &refs(&fixture.records),
                limits(1, 1 << 20),
                &JobControl::new(),
            )
            .expect_err("evidence folder"),
            QuarantineChainError::EvidenceMismatch
        );
        let other_proposal = checked_proposal_with_nonce([32; 32]);
        assert_eq!(
            verify_exact_backward_writer_chain(
                &history,
                &fixture.receipt,
                &other_proposal,
                folder(),
                losing_writer(),
                &losing_key().verifying_key(),
                &refs(&fixture.records),
                limits(1, 1 << 20),
                &JobControl::new(),
            )
            .expect_err("cross proposal"),
            QuarantineChainError::EvidenceMismatch
        );

        let mut unavailable = TestHistory::empty();
        unavailable.unavailable = true;
        assert_eq!(
            verify(
                &fixture,
                &unavailable,
                &refs(&fixture.records),
                limits(1, 1 << 20),
            )
            .expect_err("history"),
            QuarantineChainError::HistoryUnavailable
        );
    }

    #[test]
    fn corrupt_positive_anchor_is_rejected() {
        let fixture = fixture(2);
        let mut history = TestHistory::through(&fixture.records, 1);
        history.headers.clear();
        assert_eq!(
            verify(
                &fixture,
                &history,
                &[fixture.records[1].as_slice()],
                limits(1, 1 << 20),
            )
            .expect_err("missing anchor"),
            QuarantineChainError::CorruptHistory
        );

        let id = OpId::new(losing_writer().into_vector_actor(), 1).expect("id");
        history.headers.insert(
            id,
            Header::new(
                id,
                VersionVector::new([(losing_writer().into_vector_actor(), 2)]).expect("clock"),
                history.tip.expect("tip").digest(),
                None,
            ),
        );
        assert_eq!(
            verify(
                &fixture,
                &history,
                &[fixture.records[1].as_slice()],
                limits(1, 1 << 20),
            )
            .expect_err("bad anchor shape"),
            QuarantineChainError::CorruptHistory
        );
    }
}
