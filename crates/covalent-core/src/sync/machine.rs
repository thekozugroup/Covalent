//! Replay-derived admission state for one private folder event log.
//!
//! This slice accepts a canonical membership chain containing bootstrap-backed
//! Add/upgrade and ordinary read-only removal transitions, plus operations
//! authorized by its retained history. Write-loss transitions and reconciliation
//! remain fail closed. Successful preparation is not a peer acknowledgement and
//! does not mutate files in the synchronized folder.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use covalent_protocol::DeviceId;
use ed25519_dalek::VerifyingKey;
use thiserror::Error;

use super::admission::{self, Admission, AuthorTip, History, HistoryQueryError};
use super::body::{EntryValue, OperationBody};
use super::bootstrap::{
    SignatureCheckedBootstrapPermit, SignatureCheckedBootstrapReceipt,
    decode_signature_checked_bootstrap_permit, decode_signature_checked_bootstrap_receipt,
};
use super::event::{EventEnvelope, EventKind};
use super::event_log::{EventMachine, EventValidationError, PreparedEvent};
use super::frontier::{FrontierError, check_closed_frontier};
use super::ids::{FolderId, WriterId};
use super::membership::{
    EpochDigest, MemberGrant, MemberRole, SignatureCheckedEpoch, decode_signature_checked_epoch,
};
use super::membership_transition::{
    BootstrapTransitionEvidence, HistoricalWriterKeyAssignment, MembershipArchive,
    MembershipArchiveQueryError, MembershipTransitionEvidence, WriterLifetime,
    check_survivor_history_compatibility, validate_membership_transition,
};
use super::operation::{ClockEntry, decode_signature_checked_operation};
use super::path::SyncPath;
use super::register::{CausalRegister, OpId, RegisterEntry};
use super::{VersionVector, VersionVectorOrder};

// These deterministic charges include retained payload bytes separately, then
// reserve a complete sparsely occupied BTree node plus fixed object/container
// slack for every logical entry. They intentionally exceed the corresponding
// stored Rust types and current 64-bit std BTree node allocations rather than
// relying on favorable node occupancy. Any representation change that exceeds
// a charge must raise that charge before release.
const MACHINE_INDEX_BYTES: u64 = 16 * 1_024;
const EPOCH_INDEX_BYTES: u64 = 4 * 1_024;
const MEMBER_INDEX_BYTES: u64 = 1_024;
const OPERATION_INDEX_BYTES: u64 = 4 * 1_024;
const AUTHOR_INDEX_BYTES: u64 = 2 * 1_024;
const PATH_INDEX_BYTES: u64 = 1_024;
const REGISTER_ENTRY_BYTES: u64 = 4 * 1_024;
const CLOCK_COMPONENT_INDEX_BYTES: u64 = 1_024;
const EPOCH_PROOF_REFERENCE_BYTES: u64 = 1_024;
const EVIDENCE_INDEX_BYTES: u64 = 4 * 1_024;
const PENDING_REFERENCE_INDEX_BYTES: u64 = 1_024;
const WRITER_TIMELINE_INDEX_BYTES: u64 = 2 * 1_024;

/// Independent bounds for replay-derived in-memory indexes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FolderMachineLimits {
    /// Maximum deterministic charge for all retained machine indexes.
    pub maximum_index_bytes: u64,
    /// Maximum fully admitted operation records retained for exact history.
    pub maximum_operations: u64,
    /// Maximum distinct canonical paths retained by the causal registers.
    pub maximum_paths: u64,
    /// Maximum exact accepted membership epochs retained for replay.
    pub maximum_membership_epochs: u64,
    /// Maximum canonical bytes referenced by actionable bootstrap evidence.
    pub maximum_pending_evidence_bytes: u64,
    /// Maximum actionable permit and receipt references.
    pub maximum_pending_evidence_records: u64,
}

/// Immutable namespace, authority, local identity, and quota configuration.
#[derive(Clone)]
pub struct FolderMachineConfig {
    /// Folder namespace authenticated by every accepted record.
    pub folder_id: FolderId,
    /// Writer identifier of the separately pinned membership authority.
    pub authority_writer_id: WriterId,
    /// Separately pinned membership-authority verification key.
    pub pinned_authority_key: VerifyingKey,
    /// This installation's writer identity; it may be absent before bootstrap.
    pub local_writer_id: WriterId,
    /// In-memory derived-index bounds.
    pub limits: FolderMachineLimits,
}

impl fmt::Debug for FolderMachineConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FolderMachineConfig")
            .field("folder_id", &self.folder_id)
            .field("authority_writer_id", &self.authority_writer_id)
            .field("local_writer_id", &self.local_writer_id)
            .field("limits", &self.limits)
            .field("pinned_authority_key", &"[redacted]")
            .finish()
    }
}

/// Fixed, redacted machine failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FolderEventError {
    /// A configured limit cannot contain any useful accepted state.
    #[error("folder machine limits are invalid")]
    InvalidLimits,
    /// The separately pinned membership authority key is weak.
    #[error("folder machine authority key is invalid")]
    InvalidAuthorityKey,
    /// The event envelope, signature, namespace, body, or causal shape failed.
    #[error("folder event is invalid")]
    InvalidEvent,
    /// This event kind is not accepted by the current durable machine slice.
    #[error("folder event kind is unsupported")]
    UnsupportedEvent,
    /// An ordinary operation arrived before a genesis epoch was accepted.
    #[error("folder membership genesis is unavailable")]
    MissingGenesis,
    /// The operation is not authorized by the exact accepted membership head.
    #[error("folder operation membership is not authorized")]
    UnauthorizedOperation,
    /// A retained operation identity was presented with different bytes.
    #[error("folder event conflicts with retained history")]
    Equivocation,
    /// One or more causally required operations have not arrived yet.
    #[error("folder operation history is incomplete")]
    MissingHistory,
    /// An exact permit or receipt needed by a transition is not actionable.
    #[error("folder membership evidence is incomplete")]
    MissingEvidence,
    /// The configured operation, path, or index bound would be exceeded.
    #[error("folder machine resource limit exceeded")]
    ResourceLimit,
    /// Another accepted event invalidated this prepared transition.
    #[error("folder prepared transition is stale")]
    StalePrepared,
    /// This installation cannot publish for the requested identity and key.
    #[error("folder publication is not authorized")]
    PublicationDenied,
    /// The local folder-global writer counter cannot advance.
    #[error("folder publication counter is exhausted")]
    CounterOverflow,
}

/// A borrowing view of the exact committed membership head.
#[derive(Clone, Copy)]
pub struct AcceptedMembershipHeadRef<'a> {
    epoch: &'a SignatureCheckedEpoch,
}

impl AcceptedMembershipHeadRef<'_> {
    /// Returns the positive accepted membership epoch.
    #[must_use]
    pub const fn epoch(&self) -> u64 {
        self.epoch.epoch()
    }

    /// Returns the digest of the exact accepted canonical epoch record.
    #[must_use]
    pub const fn digest(&self) -> EpochDigest {
        self.epoch.digest()
    }

    /// Returns the exact accepted roster in canonical writer order.
    #[must_use]
    pub fn roster(&self) -> &[MemberGrant] {
        self.epoch.roster()
    }

    /// Returns the exact authority-signed canonical epoch record.
    #[must_use]
    pub fn canonical_record(&self) -> &[u8] {
        self.epoch.canonical_record()
    }
}

impl fmt::Debug for AcceptedMembershipHeadRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptedMembershipHeadRef")
            .field("epoch", &self.epoch.epoch())
            .field("digest", &self.epoch.digest())
            .finish()
    }
}

/// Owned canonical inputs for a future locked local signing transaction.
///
/// This contains no private key and is not a reservation. The durable append
/// transaction must re-run `publication_context` immediately before signing.
pub struct PublicationContext {
    folder_id: FolderId,
    writer_id: WriterId,
    membership_epoch: u64,
    membership_epoch_digest: [u8; 32],
    counter: u64,
    predecessor: Option<[u8; 32]>,
    clock_entries: Vec<ClockEntry>,
}

impl PublicationContext {
    /// Returns the authenticated folder namespace.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.folder_id
    }

    /// Returns this installation's exact writer identity.
    #[must_use]
    pub const fn writer_id(&self) -> WriterId {
        self.writer_id
    }

    /// Returns the exact current membership epoch.
    #[must_use]
    pub const fn membership_epoch(&self) -> u64 {
        self.membership_epoch
    }

    /// Returns the digest of the exact current membership record.
    #[must_use]
    pub const fn membership_epoch_digest(&self) -> [u8; 32] {
        self.membership_epoch_digest
    }

    /// Returns the next folder-global local writer counter.
    #[must_use]
    pub const fn counter(&self) -> u64 {
        self.counter
    }

    /// Returns the exact predecessor commitment for a later operation.
    #[must_use]
    pub const fn predecessor(&self) -> Option<[u8; 32]> {
        self.predecessor
    }

    /// Returns the next full folder-global clock in canonical writer order.
    #[must_use]
    pub fn clock_entries(&self) -> &[ClockEntry] {
        &self.clock_entries
    }
}

impl fmt::Debug for PublicationContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationContext")
            .field("folder_id", &self.folder_id)
            .field("writer_id", &self.writer_id)
            .field("membership_epoch", &self.membership_epoch)
            .field("counter", &self.counter)
            .field("has_predecessor", &self.predecessor.is_some())
            .field("clock_entry_count", &self.clock_entries.len())
            .finish()
    }
}

struct AcceptedOperation {
    header: admission::Header,
    canonical_record: Box<[u8]>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PermitIdentity {
    base: EpochDigest,
    candidate: WriterId,
    nonce: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ReceiptIdentity {
    permit_digest: [u8; 32],
    candidate: WriterId,
}

struct RetainedPermit {
    checked: SignatureCheckedBootstrapPermit,
}

struct RetainedReceipt {
    checked: SignatureCheckedBootstrapReceipt,
}

#[derive(Clone)]
struct WriterTimeline {
    first_epoch: u64,
    removed_epoch: Option<u64>,
    writer_key: VerifyingKey,
    writable_since: Option<u64>,
    minimum_context: VersionVector,
}

struct PendingPermitRef {
    identity: PermitIdentity,
    base: EpochDigest,
}

struct PendingReceiptRef {
    identity: ReceiptIdentity,
    base: EpochDigest,
}

enum PreparedChange {
    Genesis {
        epoch: SignatureCheckedEpoch,
        timeline: WriterTimeline,
        index_bytes: u64,
    },
    Operation {
        id: OpId,
        accepted: AcceptedOperation,
        writer_id: WriterId,
        tip: AuthorTip,
        frontier: VersionVector,
        path: SyncPath,
        register: CausalRegister<EntryValue>,
        index_bytes: u64,
    },
    BootstrapPermit {
        identity: PermitIdentity,
        digest: [u8; 32],
        retained: RetainedPermit,
        index_bytes: u64,
        pending_bytes: u64,
        pending_records: u64,
    },
    BootstrapReceipt {
        identity: ReceiptIdentity,
        digest: [u8; 32],
        retained: RetainedReceipt,
        index_bytes: u64,
        pending_bytes: u64,
        pending_records: u64,
    },
    MembershipTransition {
        epoch: SignatureCheckedEpoch,
        timeline_updates: Vec<(WriterId, WriterTimeline)>,
        index_bytes: u64,
        retired_index_bytes: u64,
    },
}

/// Opaque non-mutating preflight result bound to one exact machine revision.
pub struct PreparedFolderEvent {
    machine_token: Arc<()>,
    expected_revision: u64,
    change: PreparedChange,
}

impl fmt::Debug for PreparedFolderEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self.change {
            PreparedChange::Genesis { .. } => "genesis",
            PreparedChange::Operation { .. } => "operation",
            PreparedChange::BootstrapPermit { .. } => "bootstrap-permit",
            PreparedChange::BootstrapReceipt { .. } => "bootstrap-receipt",
            PreparedChange::MembershipTransition { .. } => "membership-transition",
        };
        formatter
            .debug_struct("PreparedFolderEvent")
            .field("expected_revision", &self.expected_revision)
            .field("kind", &kind)
            .finish()
    }
}

/// Deterministic replay-derived accepted state for one folder.
pub struct FolderEventMachine {
    config: FolderMachineConfig,
    machine_token: Arc<()>,
    revision: u64,
    current_epoch_digest: Option<EpochDigest>,
    epochs: BTreeMap<EpochDigest, SignatureCheckedEpoch>,
    epoch_digests: BTreeMap<u64, EpochDigest>,
    writer_timelines: BTreeMap<WriterId, WriterTimeline>,
    operations: BTreeMap<OpId, AcceptedOperation>,
    tips: BTreeMap<WriterId, AuthorTip>,
    frontier: VersionVector,
    registers: BTreeMap<SyncPath, CausalRegister<EntryValue>>,
    permits: BTreeMap<[u8; 32], RetainedPermit>,
    permit_identities: BTreeMap<PermitIdentity, [u8; 32]>,
    receipts: BTreeMap<[u8; 32], RetainedReceipt>,
    receipt_identities: BTreeMap<ReceiptIdentity, [u8; 32]>,
    pending_permits: BTreeMap<[u8; 32], PendingPermitRef>,
    pending_receipts: BTreeMap<[u8; 32], PendingReceiptRef>,
    pending_evidence_bytes: u64,
    pending_evidence_records: u64,
    index_bytes: u64,
}

impl FolderEventMachine {
    /// Creates empty replay state after validating fixed configuration.
    pub fn new(config: FolderMachineConfig) -> Result<Self, FolderEventError> {
        if config.limits.maximum_index_bytes < MACHINE_INDEX_BYTES
            || config.limits.maximum_operations == 0
            || config.limits.maximum_paths == 0
            || config.limits.maximum_membership_epochs == 0
            || config.limits.maximum_pending_evidence_bytes == 0
            || config.limits.maximum_pending_evidence_records == 0
        {
            return Err(FolderEventError::InvalidLimits);
        }
        if config.pinned_authority_key.is_weak() {
            return Err(FolderEventError::InvalidAuthorityKey);
        }
        Ok(Self {
            config,
            machine_token: Arc::new(()),
            revision: 0,
            current_epoch_digest: None,
            epochs: BTreeMap::new(),
            epoch_digests: BTreeMap::new(),
            writer_timelines: BTreeMap::new(),
            operations: BTreeMap::new(),
            tips: BTreeMap::new(),
            frontier: VersionVector::empty(),
            registers: BTreeMap::new(),
            permits: BTreeMap::new(),
            permit_identities: BTreeMap::new(),
            receipts: BTreeMap::new(),
            receipt_identities: BTreeMap::new(),
            pending_permits: BTreeMap::new(),
            pending_receipts: BTreeMap::new(),
            pending_evidence_bytes: 0,
            pending_evidence_records: 0,
            index_bytes: MACHINE_INDEX_BYTES,
        })
    }

    /// Returns the configured folder namespace.
    #[must_use]
    pub const fn folder_id(&self) -> FolderId {
        self.config.folder_id
    }

    /// Returns the exact committed membership head, when genesis exists.
    #[must_use]
    pub fn current_head(&self) -> Option<AcceptedMembershipHeadRef<'_>> {
        self.current_epoch()
            .map(|epoch| AcceptedMembershipHeadRef { epoch })
    }

    /// Returns the join of every fully admitted operation clock.
    #[must_use]
    pub const fn current_frontier(&self) -> &VersionVector {
        &self.frontier
    }

    /// Returns only active values derived from fully admitted operations.
    #[must_use]
    pub fn register(&self, path: &SyncPath) -> Option<&CausalRegister<EntryValue>> {
        self.registers.get(path)
    }

    /// Derives bounded signing inputs from current accepted state.
    ///
    /// The caller must hold the durable log transaction and call this again
    /// immediately before signing; the returned value reserves no counter.
    pub fn publication_context(
        &self,
        writer: WriterId,
        key: &VerifyingKey,
    ) -> Result<PublicationContext, FolderEventError> {
        if writer != self.config.local_writer_id {
            return Err(FolderEventError::PublicationDenied);
        }
        let epoch = self
            .current_epoch()
            .ok_or(FolderEventError::PublicationDenied)?;
        let grant = member_by_writer(epoch.roster(), writer)
            .filter(|grant| grant.role() == MemberRole::ReadWrite)
            .filter(|grant| grant.writer_key() == key)
            .ok_or(FolderEventError::PublicationDenied)?;
        let _ = grant;

        let prior_tip = self.tips.get(&writer).copied();
        let counter = match prior_tip {
            Some(tip) => tip
                .counter()
                .checked_add(1)
                .ok_or(FolderEventError::CounterOverflow)?,
            None => 1,
        };
        let (next_frontier, checked_counter) = self
            .frontier
            .checked_advance(writer.into_vector_actor())
            .map_err(|_| FolderEventError::CounterOverflow)?;
        if checked_counter != counter {
            return Err(FolderEventError::InvalidEvent);
        }
        let mut clock_entries = Vec::with_capacity(next_frontier.actor_count());
        for (actor, tip) in &self.tips {
            let component = if *actor == writer {
                counter
            } else {
                tip.counter()
            };
            clock_entries.push(
                ClockEntry::new(*actor, component).map_err(|_| FolderEventError::InvalidEvent)?,
            );
        }
        if !self.tips.contains_key(&writer) {
            clock_entries.push(
                ClockEntry::new(writer, counter).map_err(|_| FolderEventError::InvalidEvent)?,
            );
            clock_entries.sort_unstable_by_key(|entry| entry.writer_id());
        }
        Ok(PublicationContext {
            folder_id: self.config.folder_id,
            writer_id: writer,
            membership_epoch: epoch.epoch(),
            membership_epoch_digest: epoch.digest().to_bytes(),
            counter,
            predecessor: prior_tip.map(AuthorTip::digest),
            clock_entries,
        })
    }

    /// Performs a non-mutating preflight while preserving a concrete fixed
    /// failure for local runtime policy and diagnostics.
    ///
    /// Only [`EventMachine::commit`] or durable-log append may commit the
    /// returned opaque value. It remains bound to this exact machine revision.
    pub fn prepare_event(
        &self,
        event: &[u8],
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let envelope = EventEnvelope::parse(event).map_err(|_| FolderEventError::InvalidEvent)?;
        match envelope.kind() {
            EventKind::MembershipEpoch => self.prepare_membership_epoch(&envelope),
            EventKind::Operation => self.prepare_operation(&envelope),
            EventKind::BootstrapPermit => self.prepare_bootstrap_permit(&envelope),
            EventKind::BootstrapReceipt => self.prepare_bootstrap_receipt(&envelope),
            EventKind::WriteLossProposal | EventKind::FreezeReceipt | EventKind::FreezeAbort => {
                Err(FolderEventError::UnsupportedEvent)
            }
        }
    }

    fn prepare_membership_epoch(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        if self.current_epoch_digest.is_none() {
            self.prepare_genesis(envelope)
        } else {
            self.prepare_transition(envelope)
        }
    }

    fn prepare_genesis(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let epoch = decode_signature_checked_epoch(
            envelope.record(),
            self.config.folder_id,
            self.config.authority_writer_id,
            &self.config.pinned_authority_key,
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        if epoch.epoch() != 1 {
            return Err(FolderEventError::InvalidEvent);
        }
        if let Some(existing) = self.epoch_digests.get(&epoch.epoch()) {
            let retained = self
                .epochs
                .get(existing)
                .ok_or(FolderEventError::InvalidEvent)?;
            return if retained.canonical_record() == envelope.record() {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::Equivocation)
            };
        }
        self.require_revision_capacity()?;
        let grant = epoch
            .roster()
            .first()
            .ok_or(FolderEventError::InvalidEvent)?;
        let timeline = WriterTimeline {
            first_epoch: 1,
            removed_epoch: None,
            writer_key: *grant.writer_key(),
            writable_since: Some(1),
            minimum_context: VersionVector::empty(),
        };
        let index_bytes = self
            .index_bytes
            .checked_add(EPOCH_INDEX_BYTES)
            .and_then(|bytes| bytes.checked_add(envelope.record().len() as u64))
            .and_then(|bytes| {
                (epoch.roster().len() as u64)
                    .checked_mul(MEMBER_INDEX_BYTES)
                    .and_then(|charge| bytes.checked_add(charge))
            })
            .and_then(|bytes| bytes.checked_add(WRITER_TIMELINE_INDEX_BYTES))
            .ok_or(FolderEventError::ResourceLimit)?;
        self.require_index_limit(index_bytes)?;
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::Genesis {
                epoch,
                timeline,
                index_bytes,
            },
        }))
    }

    fn prepare_operation(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let current_epoch = self
            .current_epoch()
            .ok_or(FolderEventError::MissingGenesis)?;
        let writer = envelope
            .untrusted_operation_writer_hint()
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let routing_grant = self
            .writer_timelines
            .get(&writer)
            .ok_or(FolderEventError::UnauthorizedOperation)?;
        let operation = decode_signature_checked_operation(
            envelope.record(),
            self.config.folder_id,
            writer,
            &routing_grant.writer_key,
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        self.authorize_operation(&operation, current_epoch)?;
        let header = operation
            .to_admission_header()
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let id = header.id();
        if let Some(existing) = self.operations.get(&id) {
            return if existing.canonical_record.as_ref() == envelope.record() {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::Equivocation)
            };
        }
        self.require_revision_capacity()?;
        let admission = admission::check(self, &header).map_err(|error| match error {
            admission::AdmissionError::MissingAuthorPredecessor
            | admission::AdmissionError::UnknownDependency => FolderEventError::MissingHistory,
            admission::AdmissionError::Equivocation => FolderEventError::Equivocation,
            _ => FolderEventError::InvalidEvent,
        })?;
        match admission {
            Admission::Admissible => {}
            Admission::Duplicate => return Err(FolderEventError::InvalidEvent),
        }
        let body =
            OperationBody::decode(operation.body()).map_err(|_| FolderEventError::InvalidEvent)?;
        let entry = RegisterEntry::from_full_clock(id, header.clock().clone(), body.value())
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let previous_register = self.registers.get(body.path());
        let register = match previous_register {
            Some(existing) => existing.merge_entry(entry),
            None => CausalRegister::empty().merge_entry(entry),
        }
        .map_err(|_| FolderEventError::InvalidEvent)?;
        let frontier = self
            .frontier
            .join(header.clock())
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let tip = AuthorTip::new(id.counter(), header.digest())
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let operation_count = (self.operations.len() as u64)
            .checked_add(1)
            .ok_or(FolderEventError::ResourceLimit)?;
        if operation_count > self.config.limits.maximum_operations {
            return Err(FolderEventError::ResourceLimit);
        }
        let is_new_path = previous_register.is_none();
        if is_new_path
            && (self.registers.len() as u64)
                .checked_add(1)
                .ok_or(FolderEventError::ResourceLimit)?
                > self.config.limits.maximum_paths
        {
            return Err(FolderEventError::ResourceLimit);
        }
        let index_bytes = self.operation_index_bytes(
            envelope.record().len(),
            operation.clock_entries().len(),
            writer,
            body.path(),
            previous_register,
            &register,
        )?;
        let accepted = AcceptedOperation {
            header,
            canonical_record: envelope.record().into(),
        };
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::Operation {
                id,
                accepted,
                writer_id: writer,
                tip,
                frontier,
                path: body.path().clone(),
                register,
                index_bytes,
            },
        }))
    }

    fn prepare_bootstrap_permit(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let permit = decode_signature_checked_bootstrap_permit(
            envelope.record(),
            self.config.folder_id,
            self.config.authority_writer_id,
            &self.config.pinned_authority_key,
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        let identity = PermitIdentity {
            base: permit.base_epoch_digest(),
            candidate: permit.candidate().writer_id(),
            nonce: permit.nonce(),
        };
        let digest = permit.digest().to_bytes();
        if let Some(existing_digest) = self.permit_identities.get(&identity) {
            let retained = self
                .permits
                .get(existing_digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            return if retained.checked.canonical_record() == envelope.record()
                && *existing_digest == digest
            {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::Equivocation)
            };
        }
        if self.permits.contains_key(&digest) {
            return Err(FolderEventError::Equivocation);
        }
        let current = self
            .current_epoch()
            .ok_or(FolderEventError::MissingGenesis)?;
        if permit.base_epoch() != current.epoch() || permit.base_epoch_digest() != current.digest()
        {
            return Err(FolderEventError::UnauthorizedOperation);
        }
        self.validate_permit_candidate(&permit)?;
        map_frontier_result(check_closed_frontier(self, permit.required_frontier()))?;
        self.require_revision_capacity()?;
        let (index_bytes, pending_bytes, pending_records) = self.evidence_resource_delta(
            envelope.record().len(),
            permit.required_frontier().actor_count(),
        )?;
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::BootstrapPermit {
                identity,
                digest,
                retained: RetainedPermit { checked: permit },
                index_bytes,
                pending_bytes,
                pending_records,
            },
        }))
    }

    fn prepare_bootstrap_receipt(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let hint = envelope
            .untrusted_bootstrap_receipt_hint()
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let identity = ReceiptIdentity {
            permit_digest: hint.permit_digest().to_bytes(),
            candidate: hint.candidate_writer_id(),
        };
        let retained_permit = self
            .permits
            .get(&identity.permit_digest)
            .ok_or(FolderEventError::MissingEvidence)?;
        let receipt = decode_signature_checked_bootstrap_receipt(
            envelope.record(),
            &retained_permit.checked,
            retained_permit.checked.candidate().writer_key(),
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        if receipt.candidate_writer_id() != identity.candidate
            || receipt.permit_digest().to_bytes() != identity.permit_digest
        {
            return Err(FolderEventError::InvalidEvent);
        }
        if let Some(existing_digest) = self.receipt_identities.get(&identity) {
            let retained = self
                .receipts
                .get(existing_digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            return if retained.checked.canonical_record() == envelope.record()
                && *existing_digest == receipt.digest().to_bytes()
            {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::Equivocation)
            };
        }
        let pending_permit = self
            .pending_permits
            .get(&identity.permit_digest)
            .ok_or(FolderEventError::MissingEvidence)?;
        if pending_permit.identity.candidate != identity.candidate {
            return Err(FolderEventError::InvalidEvent);
        }
        map_frontier_result(check_closed_frontier(self, receipt.applied_frontier()))?;
        let digest = receipt.digest().to_bytes();
        if self.receipts.contains_key(&digest) {
            return Err(FolderEventError::Equivocation);
        }
        self.require_revision_capacity()?;
        let (index_bytes, pending_bytes, pending_records) = self.evidence_resource_delta(
            envelope.record().len(),
            receipt.applied_frontier().actor_count(),
        )?;
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::BootstrapReceipt {
                identity,
                digest,
                retained: RetainedReceipt { checked: receipt },
                index_bytes,
                pending_bytes,
                pending_records,
            },
        }))
    }

    fn prepare_transition(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let current = self
            .current_epoch()
            .ok_or(FolderEventError::MissingGenesis)?;
        let candidate = decode_signature_checked_epoch(
            envelope.record(),
            self.config.folder_id,
            self.config.authority_writer_id,
            &self.config.pinned_authority_key,
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        if let Some(existing_digest) = self.epoch_digests.get(&candidate.epoch()) {
            let retained = self
                .epochs
                .get(existing_digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            return if retained.canonical_record() == envelope.record() {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::Equivocation)
            };
        }
        if candidate.proposal_digest().is_some()
            || !candidate.cutoffs().is_empty()
            || !candidate.freeze_receipt_digests().is_empty()
            || has_read_write_loss(current.roster(), candidate.roster())
        {
            return Err(FolderEventError::UnsupportedEvent);
        }
        self.require_revision_capacity()?;
        let mut raw_bootstrap = Vec::with_capacity(candidate.bootstrap_receipt_digests().len());
        for receipt_digest in candidate.bootstrap_receipt_digests() {
            let pending_receipt = self
                .pending_receipts
                .get(receipt_digest)
                .ok_or(FolderEventError::MissingEvidence)?;
            let receipt = self
                .receipts
                .get(receipt_digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            let permit_digest = pending_receipt.identity.permit_digest;
            if !self.pending_permits.contains_key(&permit_digest) {
                return Err(FolderEventError::MissingEvidence);
            }
            let permit = self
                .permits
                .get(&permit_digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            raw_bootstrap.push(BootstrapTransitionEvidence::new(
                pending_receipt.identity.candidate,
                permit.checked.canonical_record(),
                receipt.checked.canonical_record(),
            ));
        }
        let evidence = if raw_bootstrap.is_empty() {
            MembershipTransitionEvidence::Ordinary
        } else {
            MembershipTransitionEvidence::Bootstrap(&raw_bootstrap)
        };
        let plan = validate_membership_transition(
            current.canonical_record(),
            envelope.record(),
            self.config.folder_id,
            self.config.authority_writer_id,
            &self.config.pinned_authority_key,
            self,
            self,
            evidence,
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        if plan.write_loss().is_some() || !plan.downgraded().is_empty() {
            return Err(FolderEventError::UnsupportedEvent);
        }
        if member_by_writer(plan.next_epoch().roster(), self.config.local_writer_id).is_some() {
            check_survivor_history_compatibility(self, &plan, self.config.local_writer_id)
                .map_err(|_| FolderEventError::InvalidEvent)?;
        }
        let timeline_updates = self.prepare_timeline_updates(&plan)?;
        let retired_index_bytes = self.pending_reference_index_bytes()?;
        let index_bytes =
            self.transition_index_bytes(plan.next_epoch(), &timeline_updates, retired_index_bytes)?;
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::MembershipTransition {
                epoch: plan.next_epoch().clone(),
                timeline_updates,
                index_bytes,
                retired_index_bytes,
            },
        }))
    }

    fn current_epoch(&self) -> Option<&SignatureCheckedEpoch> {
        self.current_epoch_digest
            .as_ref()
            .and_then(|digest| self.epochs.get(digest))
    }

    fn validate_permit_candidate(
        &self,
        permit: &SignatureCheckedBootstrapPermit,
    ) -> Result<(), FolderEventError> {
        let current = self
            .current_epoch()
            .ok_or(FolderEventError::MissingGenesis)?;
        let candidate = permit.candidate();
        match member_by_writer(current.roster(), candidate.writer_id()) {
            Some(existing) => {
                if existing.role() != MemberRole::Read
                    || candidate.role() != MemberRole::ReadWrite
                    || existing.writer_key() != candidate.writer_key()
                    || existing.transport_device_id() != candidate.transport_device_id()
                    || existing.transport_key() != candidate.transport_key()
                {
                    return Err(FolderEventError::InvalidEvent);
                }
            }
            None => {
                if self.writer_timelines.contains_key(&candidate.writer_id())
                    || self.writer_timelines.values().any(|timeline| {
                        timeline.writer_key.to_bytes() == candidate.writer_key().to_bytes()
                    })
                {
                    return Err(FolderEventError::InvalidEvent);
                }
            }
        }
        Ok(())
    }

    fn authorize_operation(
        &self,
        operation: &super::operation::SignatureCheckedOperation,
        current: &SignatureCheckedEpoch,
    ) -> Result<(), FolderEventError> {
        let header = operation.header();
        let claimed_digest = EpochDigest::from_bytes(header.membership_epoch_digest());
        let claimed = self
            .epochs
            .get(&claimed_digest)
            .filter(|epoch| epoch.epoch() == header.membership_epoch())
            .ok_or(FolderEventError::UnauthorizedOperation)?;
        let writer = header.writer_id();
        let timeline = self
            .writer_timelines
            .get(&writer)
            .ok_or(FolderEventError::UnauthorizedOperation)?;
        for (_, digest) in self.epoch_digests.range(claimed.epoch()..=current.epoch()) {
            let epoch = self
                .epochs
                .get(digest)
                .ok_or(FolderEventError::InvalidEvent)?;
            let grant = member_by_writer(epoch.roster(), writer)
                .filter(|grant| grant.role() == MemberRole::ReadWrite)
                .filter(|grant| grant.writer_key() == &timeline.writer_key)
                .ok_or(FolderEventError::UnauthorizedOperation)?;
            let _ = grant;
        }
        if timeline
            .writable_since
            .is_none_or(|start| claimed.epoch() < start)
            || !matches!(
                header.clock().compare(&timeline.minimum_context),
                VersionVectorOrder::Equal | VersionVectorOrder::After
            )
        {
            return Err(FolderEventError::UnauthorizedOperation);
        }
        Ok(())
    }

    fn evidence_resource_delta(
        &self,
        record_bytes: usize,
        frontier_components: usize,
    ) -> Result<(u64, u64, u64), FolderEventError> {
        let record_bytes = record_bytes as u64;
        let pending_bytes = self
            .pending_evidence_bytes
            .checked_add(record_bytes)
            .ok_or(FolderEventError::ResourceLimit)?;
        let pending_records = self
            .pending_evidence_records
            .checked_add(1)
            .ok_or(FolderEventError::ResourceLimit)?;
        if pending_bytes > self.config.limits.maximum_pending_evidence_bytes
            || pending_records > self.config.limits.maximum_pending_evidence_records
        {
            return Err(FolderEventError::ResourceLimit);
        }
        let index_bytes = self
            .index_bytes
            .checked_add(EVIDENCE_INDEX_BYTES)
            .and_then(|bytes| bytes.checked_add(PENDING_REFERENCE_INDEX_BYTES))
            .and_then(|bytes| bytes.checked_add(record_bytes))
            .and_then(|bytes| {
                (frontier_components as u64)
                    .checked_mul(CLOCK_COMPONENT_INDEX_BYTES)
                    .and_then(|charge| bytes.checked_add(charge))
            })
            .ok_or(FolderEventError::ResourceLimit)?;
        self.require_index_limit(index_bytes)?;
        Ok((index_bytes, pending_bytes, pending_records))
    }

    fn prepare_timeline_updates(
        &self,
        plan: &super::membership_transition::ValidatedMembershipTransition,
    ) -> Result<Vec<(WriterId, WriterTimeline)>, FolderEventError> {
        let mut contexts = BTreeMap::new();
        for context in plan.bootstrap_minimum_contexts() {
            if contexts
                .insert(context.writer_id(), context.applied_frontier().clone())
                .is_some()
            {
                return Err(FolderEventError::InvalidEvent);
            }
        }
        let next_epoch = plan.next_epoch().epoch();
        let mut updates = Vec::new();
        for added in plan.added() {
            let minimum_context = contexts
                .remove(&added.writer_id())
                .ok_or(FolderEventError::InvalidEvent)?;
            updates.push((
                added.writer_id(),
                WriterTimeline {
                    first_epoch: next_epoch,
                    removed_epoch: None,
                    writer_key: *added.writer_key(),
                    writable_since: (added.role() == MemberRole::ReadWrite).then_some(next_epoch),
                    minimum_context: if added.role() == MemberRole::ReadWrite {
                        minimum_context
                    } else {
                        VersionVector::empty()
                    },
                },
            ));
        }
        for upgraded in plan.upgraded() {
            let writer = upgraded.writer_id();
            let mut timeline = self
                .writer_timelines
                .get(&writer)
                .cloned()
                .ok_or(FolderEventError::InvalidEvent)?;
            timeline.writable_since = Some(next_epoch);
            timeline.minimum_context = contexts
                .remove(&writer)
                .ok_or(FolderEventError::InvalidEvent)?;
            updates.push((writer, timeline));
        }
        for removed in plan.removed() {
            if removed.role() != MemberRole::Read {
                return Err(FolderEventError::UnsupportedEvent);
            }
            let mut timeline = self
                .writer_timelines
                .get(&removed.writer_id())
                .cloned()
                .ok_or(FolderEventError::InvalidEvent)?;
            timeline.removed_epoch = Some(next_epoch);
            updates.push((removed.writer_id(), timeline));
        }
        if !contexts.is_empty() {
            return Err(FolderEventError::InvalidEvent);
        }
        updates.sort_unstable_by_key(|(writer, _)| *writer);
        if updates.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(FolderEventError::InvalidEvent);
        }
        Ok(updates)
    }

    fn pending_reference_index_bytes(&self) -> Result<u64, FolderEventError> {
        self.pending_evidence_records
            .checked_mul(PENDING_REFERENCE_INDEX_BYTES)
            .ok_or(FolderEventError::ResourceLimit)
    }

    fn transition_index_bytes(
        &self,
        epoch: &SignatureCheckedEpoch,
        updates: &[(WriterId, WriterTimeline)],
        retired_index_bytes: u64,
    ) -> Result<u64, FolderEventError> {
        let epoch_count = (self.epochs.len() as u64)
            .checked_add(1)
            .ok_or(FolderEventError::ResourceLimit)?;
        if epoch_count > self.config.limits.maximum_membership_epochs {
            return Err(FolderEventError::ResourceLimit);
        }
        let mut bytes = self
            .index_bytes
            .checked_sub(retired_index_bytes)
            .and_then(|value| value.checked_add(EPOCH_INDEX_BYTES))
            .and_then(|value| value.checked_add(epoch.canonical_record().len() as u64))
            .and_then(|value| {
                (epoch.roster().len() as u64)
                    .checked_mul(MEMBER_INDEX_BYTES)
                    .and_then(|charge| value.checked_add(charge))
            })
            .and_then(|value| {
                (epoch.bootstrap_receipt_digests().len() as u64)
                    .checked_mul(EPOCH_PROOF_REFERENCE_BYTES)
                    .and_then(|charge| value.checked_add(charge))
            })
            .ok_or(FolderEventError::ResourceLimit)?;
        for (writer, timeline) in updates {
            if !self.writer_timelines.contains_key(writer) {
                bytes = bytes
                    .checked_add(WRITER_TIMELINE_INDEX_BYTES)
                    .ok_or(FolderEventError::ResourceLimit)?;
            }
            bytes = bytes
                .checked_add(
                    (timeline.minimum_context.actor_count() as u64)
                        .checked_mul(CLOCK_COMPONENT_INDEX_BYTES)
                        .ok_or(FolderEventError::ResourceLimit)?,
                )
                .ok_or(FolderEventError::ResourceLimit)?;
        }
        self.require_index_limit(bytes)?;
        Ok(bytes)
    }

    #[allow(clippy::too_many_arguments)]
    fn operation_index_bytes(
        &self,
        record_bytes: usize,
        clock_components: usize,
        writer: WriterId,
        path: &SyncPath,
        previous_register: Option<&CausalRegister<EntryValue>>,
        next_register: &CausalRegister<EntryValue>,
    ) -> Result<u64, FolderEventError> {
        let old_register = previous_register
            .map(register_index_bytes)
            .transpose()?
            .unwrap_or(0);
        let new_register = register_index_bytes(next_register)?;
        let mut bytes = self
            .index_bytes
            .checked_sub(old_register)
            .and_then(|value| value.checked_add(new_register))
            .and_then(|value| value.checked_add(OPERATION_INDEX_BYTES))
            .and_then(|value| value.checked_add(record_bytes as u64))
            .and_then(|value| {
                (clock_components as u64)
                    .checked_mul(CLOCK_COMPONENT_INDEX_BYTES)
                    .and_then(|charge| value.checked_add(charge))
            })
            .ok_or(FolderEventError::ResourceLimit)?;
        if !self.tips.contains_key(&writer) {
            bytes = bytes
                .checked_add(AUTHOR_INDEX_BYTES)
                .ok_or(FolderEventError::ResourceLimit)?;
        }
        if previous_register.is_none() {
            bytes = bytes
                .checked_add(PATH_INDEX_BYTES)
                .and_then(|value| value.checked_add(path.as_str().len() as u64))
                .ok_or(FolderEventError::ResourceLimit)?;
        }
        self.require_index_limit(bytes)?;
        Ok(bytes)
    }

    fn require_index_limit(&self, bytes: u64) -> Result<(), FolderEventError> {
        if bytes > self.config.limits.maximum_index_bytes {
            return Err(FolderEventError::ResourceLimit);
        }
        Ok(())
    }

    fn require_revision_capacity(&self) -> Result<(), FolderEventError> {
        self.revision
            .checked_add(1)
            .map(|_| ())
            .ok_or(FolderEventError::ResourceLimit)
    }

    fn commit_checked(&mut self, prepared: PreparedFolderEvent) -> Result<(), FolderEventError> {
        if !Arc::ptr_eq(&prepared.machine_token, &self.machine_token)
            || prepared.expected_revision != self.revision
        {
            return Err(FolderEventError::StalePrepared);
        }
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(FolderEventError::ResourceLimit)?;
        match prepared.change {
            PreparedChange::Genesis {
                epoch,
                timeline,
                index_bytes,
            } => {
                if self.current_epoch_digest.is_some() {
                    return Err(FolderEventError::StalePrepared);
                }
                let digest = epoch.digest();
                let epoch_number = epoch.epoch();
                let writer = epoch
                    .roster()
                    .first()
                    .ok_or(FolderEventError::InvalidEvent)?
                    .writer_id();
                self.epochs.insert(digest, epoch);
                self.epoch_digests.insert(epoch_number, digest);
                self.writer_timelines.insert(writer, timeline);
                self.current_epoch_digest = Some(digest);
                self.index_bytes = index_bytes;
            }
            PreparedChange::Operation {
                id,
                accepted,
                writer_id,
                tip,
                frontier,
                path,
                register,
                index_bytes,
            } => {
                if self.operations.contains_key(&id) {
                    return Err(FolderEventError::StalePrepared);
                }
                self.operations.insert(id, accepted);
                self.tips.insert(writer_id, tip);
                self.frontier = frontier;
                self.registers.insert(path, register);
                self.index_bytes = index_bytes;
            }
            PreparedChange::BootstrapPermit {
                identity,
                digest,
                retained,
                index_bytes,
                pending_bytes,
                pending_records,
            } => {
                if self.permit_identities.contains_key(&identity)
                    || self.permits.contains_key(&digest)
                {
                    return Err(FolderEventError::StalePrepared);
                }
                self.permit_identities.insert(identity, digest);
                self.permits.insert(digest, retained);
                self.pending_permits.insert(
                    digest,
                    PendingPermitRef {
                        identity,
                        base: identity.base,
                    },
                );
                self.pending_evidence_bytes = pending_bytes;
                self.pending_evidence_records = pending_records;
                self.index_bytes = index_bytes;
            }
            PreparedChange::BootstrapReceipt {
                identity,
                digest,
                retained,
                index_bytes,
                pending_bytes,
                pending_records,
            } => {
                if self.receipt_identities.contains_key(&identity)
                    || self.receipts.contains_key(&digest)
                {
                    return Err(FolderEventError::StalePrepared);
                }
                let base = retained.checked.base_epoch_digest();
                self.receipt_identities.insert(identity, digest);
                self.receipts.insert(digest, retained);
                self.pending_receipts
                    .insert(digest, PendingReceiptRef { identity, base });
                self.pending_evidence_bytes = pending_bytes;
                self.pending_evidence_records = pending_records;
                self.index_bytes = index_bytes;
            }
            PreparedChange::MembershipTransition {
                epoch,
                timeline_updates,
                index_bytes,
                retired_index_bytes,
            } => {
                let current = self
                    .current_epoch_digest
                    .ok_or(FolderEventError::StalePrepared)?;
                if epoch.previous_epoch_digest() != Some(current) {
                    return Err(FolderEventError::StalePrepared);
                }
                let digest = epoch.digest();
                let epoch_number = epoch.epoch();
                if self.epochs.contains_key(&digest)
                    || self.epoch_digests.contains_key(&epoch_number)
                {
                    return Err(FolderEventError::StalePrepared);
                }
                if self.pending_reference_index_bytes()? != retired_index_bytes
                    || self
                        .pending_permits
                        .values()
                        .any(|pending| pending.base != current)
                    || self
                        .pending_receipts
                        .values()
                        .any(|pending| pending.base != current)
                {
                    return Err(FolderEventError::StalePrepared);
                }
                self.epochs.insert(digest, epoch);
                self.epoch_digests.insert(epoch_number, digest);
                for (writer, timeline) in timeline_updates {
                    self.writer_timelines.insert(writer, timeline);
                }
                self.current_epoch_digest = Some(digest);
                self.pending_permits.clear();
                self.pending_receipts.clear();
                self.pending_evidence_bytes = 0;
                self.pending_evidence_records = 0;
                self.index_bytes = index_bytes;
            }
        }
        self.revision = next_revision;
        Ok(())
    }
}

impl EventMachine for FolderEventMachine {
    type Prepared = PreparedFolderEvent;

    fn is_empty_for_replay(&self) -> bool {
        self.revision == 0
    }

    fn prepare(&self, event: &[u8]) -> Result<PreparedEvent<Self::Prepared>, EventValidationError> {
        self.prepare_event(event).map_err(|_| EventValidationError)
    }

    fn commit(&mut self, prepared: Self::Prepared) -> Result<(), EventValidationError> {
        self.commit_checked(prepared)
            .map_err(|_| EventValidationError)
    }
}

impl History for FolderEventMachine {
    fn author_tip(&self, writer: DeviceId) -> Result<Option<AuthorTip>, HistoryQueryError> {
        Ok(self
            .tips
            .iter()
            .find_map(|(candidate, tip)| (candidate.into_vector_actor() == writer).then_some(*tip)))
    }

    fn header(&self, id: OpId) -> Result<Option<admission::Header>, HistoryQueryError> {
        Ok(self
            .operations
            .get(&id)
            .map(|operation| operation.header.clone()))
    }
}

impl MembershipArchive for FolderEventMachine {
    fn retained_writer_count(
        &self,
        folder: FolderId,
        base: EpochDigest,
    ) -> Result<usize, MembershipArchiveQueryError> {
        let epoch = self.exact_archive_head(folder, base)?.epoch();
        Ok(self
            .writer_timelines
            .values()
            .filter(|timeline| timeline.first_epoch <= epoch)
            .count())
    }

    fn writer_lifetime(
        &self,
        folder: FolderId,
        base: EpochDigest,
        writer: WriterId,
    ) -> Result<WriterLifetime, MembershipArchiveQueryError> {
        let epoch = self.exact_archive_head(folder, base)?.epoch();
        let Some(timeline) = self.writer_timelines.get(&writer) else {
            return Ok(WriterLifetime::NeverSeen);
        };
        if timeline.first_epoch > epoch {
            return Ok(WriterLifetime::NeverSeen);
        }
        Ok(
            if timeline
                .removed_epoch
                .is_some_and(|removed| removed <= epoch)
            {
                WriterLifetime::Removed
            } else {
                WriterLifetime::Active
            },
        )
    }

    fn writer_key_assignment(
        &self,
        folder: FolderId,
        base: EpochDigest,
        key: &VerifyingKey,
    ) -> Result<HistoricalWriterKeyAssignment, MembershipArchiveQueryError> {
        let epoch = self.exact_archive_head(folder, base)?.epoch();
        Ok(self
            .writer_timelines
            .iter()
            .find(|(_, timeline)| timeline.first_epoch <= epoch && &timeline.writer_key == key)
            .map_or(
                HistoricalWriterKeyAssignment::NeverAssigned,
                |(writer, _)| HistoricalWriterKeyAssignment::AssignedTo(*writer),
            ))
    }
}

impl FolderEventMachine {
    fn exact_archive_head(
        &self,
        folder: FolderId,
        base: EpochDigest,
    ) -> Result<&SignatureCheckedEpoch, MembershipArchiveQueryError> {
        self.epochs
            .get(&base)
            .filter(|_| folder == self.config.folder_id)
            .ok_or(MembershipArchiveQueryError::UnknownBaseHead)
    }
}

fn member_by_writer(roster: &[MemberGrant], writer: WriterId) -> Option<&MemberGrant> {
    roster
        .binary_search_by_key(&writer, MemberGrant::writer_id)
        .ok()
        .map(|index| &roster[index])
}

fn has_read_write_loss(base: &[MemberGrant], next: &[MemberGrant]) -> bool {
    base.iter()
        .filter(|grant| grant.role() == MemberRole::ReadWrite)
        .any(|grant| {
            member_by_writer(next, grant.writer_id())
                .is_none_or(|next_grant| next_grant.role() != MemberRole::ReadWrite)
        })
}

fn map_frontier_result(result: Result<(), FrontierError>) -> Result<(), FolderEventError> {
    result.map_err(|error| match error {
        FrontierError::HistoryUnavailable | FrontierError::MissingOperation => {
            FolderEventError::MissingHistory
        }
        FrontierError::WrongOperation
        | FrontierError::InconsistentClock
        | FrontierError::NotClosed => FolderEventError::InvalidEvent,
    })
}

fn register_index_bytes(register: &CausalRegister<EntryValue>) -> Result<u64, FolderEventError> {
    register.active().try_fold(0_u64, |bytes, entry| {
        bytes
            .checked_add(REGISTER_ENTRY_BYTES)
            .and_then(|value| {
                (entry.clock().actor_count() as u64)
                    .checked_mul(CLOCK_COMPONENT_INDEX_BYTES)
                    .and_then(|charge| value.checked_add(charge))
            })
            .ok_or(FolderEventError::ResourceLimit)
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::body::OperationBody;
    use crate::sync::bootstrap::{
        decode_signature_checked_bootstrap_permit, encode_signed_bootstrap_permit,
        encode_signed_bootstrap_receipt,
    };
    use crate::sync::event::EventEnvelope;
    use crate::sync::event_log::{DurableEventLog, EventAppendOutcome, EventLogLimits};
    use crate::sync::log_frame::{LogBinding, LogFileKind, LogFrameKey};
    use crate::sync::membership::{MemberGrant, encode_signed_epoch};
    use crate::sync::operation::{OperationDigest, encode_signed_operation};
    use crate::sync::state_dir::{PrivateStateDir, StateKey};

    const GENEROUS_LIMITS: FolderMachineLimits = FolderMachineLimits {
        maximum_index_bytes: 16 * 1_024 * 1_024,
        maximum_operations: 1_000,
        maximum_paths: 1_000,
        maximum_membership_epochs: 128,
        maximum_pending_evidence_bytes: 256 * 1_024,
        maximum_pending_evidence_records: 128,
    };

    fn folder(number: u128) -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(number))
    }

    fn writer(number: u128) -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(number))
    }

    fn device(number: u128) -> DeviceId {
        DeviceId::from_uuid(Uuid::from_u128(number))
    }

    fn key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    struct Fixture {
        folder_id: FolderId,
        authority_writer: WriterId,
        local_writer: WriterId,
        authority_key: SigningKey,
        genesis_record: Vec<u8>,
        genesis_digest: EpochDigest,
    }

    impl Fixture {
        fn new() -> Self {
            let folder_id = folder(10);
            let authority_writer = writer(20);
            let local_writer = authority_writer;
            let authority_key = key(1);
            let grant = MemberGrant::new(
                authority_writer,
                authority_key.verifying_key(),
                device(30),
                key(2).verifying_key(),
                MemberRole::ReadWrite,
            )
            .expect("grant");
            let genesis_record = encode_signed_epoch(
                &authority_key,
                folder_id,
                1,
                authority_writer,
                None,
                &[grant],
                None,
                &[],
                &[],
                &[],
                &[],
            )
            .expect("genesis");
            let genesis_digest = decode_signature_checked_epoch(
                &genesis_record,
                folder_id,
                authority_writer,
                &authority_key.verifying_key(),
            )
            .expect("checked genesis")
            .digest();
            Self {
                folder_id,
                authority_writer,
                local_writer,
                authority_key,
                genesis_record,
                genesis_digest,
            }
        }

        fn config(&self, limits: FolderMachineLimits) -> FolderMachineConfig {
            FolderMachineConfig {
                folder_id: self.folder_id,
                authority_writer_id: self.authority_writer,
                pinned_authority_key: self.authority_key.verifying_key(),
                local_writer_id: self.local_writer,
                limits,
            }
        }

        fn machine(&self) -> FolderEventMachine {
            FolderEventMachine::new(self.config(GENEROUS_LIMITS)).expect("machine")
        }

        fn envelope(kind: EventKind, record: &[u8]) -> Vec<u8> {
            EventEnvelope::from_signed_record(kind, record)
                .expect("envelope")
                .encode()
                .expect("encoded envelope")
                .as_bytes()
                .to_vec()
        }

        fn genesis_event(&self) -> Vec<u8> {
            Self::envelope(EventKind::MembershipEpoch, &self.genesis_record)
        }

        fn operation(
            &self,
            counter: u64,
            predecessor: Option<OperationDigest>,
            clock: &[ClockEntry],
            path: &str,
            value: EntryValue,
        ) -> Vec<u8> {
            let body = OperationBody::new(SyncPath::from_wire(path).expect("path"), value).encode();
            let record = encode_signed_operation(
                &self.authority_key,
                self.folder_id,
                self.authority_writer,
                1,
                self.genesis_digest.to_bytes(),
                counter,
                predecessor.map(OperationDigest::to_bytes),
                clock,
                &body,
            )
            .expect("operation");
            Self::envelope(EventKind::Operation, &record)
        }

        #[allow(clippy::too_many_arguments)]
        fn operation_for(
            &self,
            signing_key: &SigningKey,
            writer_id: WriterId,
            epoch: u64,
            epoch_digest: EpochDigest,
            counter: u64,
            predecessor: Option<OperationDigest>,
            clock: &[ClockEntry],
            path: &str,
            value: EntryValue,
        ) -> Vec<u8> {
            let body = OperationBody::new(SyncPath::from_wire(path).expect("path"), value).encode();
            let record = encode_signed_operation(
                signing_key,
                self.folder_id,
                writer_id,
                epoch,
                epoch_digest.to_bytes(),
                counter,
                predecessor.map(OperationDigest::to_bytes),
                clock,
                &body,
            )
            .expect("operation");
            Self::envelope(EventKind::Operation, &record)
        }

        fn grant(
            &self,
            writer_id: WriterId,
            writer_key: &SigningKey,
            transport_number: u128,
            transport_key: &SigningKey,
            role: MemberRole,
        ) -> MemberGrant {
            MemberGrant::new(
                writer_id,
                writer_key.verifying_key(),
                device(transport_number),
                transport_key.verifying_key(),
                role,
            )
            .expect("member grant")
        }

        fn authority_grant(&self) -> MemberGrant {
            self.grant(
                self.authority_writer,
                &self.authority_key,
                30,
                &key(2),
                MemberRole::ReadWrite,
            )
        }

        #[allow(clippy::too_many_arguments)]
        fn bootstrap_pair(
            &self,
            base_epoch: u64,
            base_digest: EpochDigest,
            candidate: &MemberGrant,
            candidate_key: &SigningKey,
            nonce_byte: u8,
            required: &[ClockEntry],
            applied: &[ClockEntry],
        ) -> (Vec<u8>, Vec<u8>, [u8; 32]) {
            let permit_record = encode_signed_bootstrap_permit(
                &self.authority_key,
                self.folder_id,
                self.authority_writer,
                base_epoch,
                base_digest,
                candidate,
                [nonce_byte; 32],
                1,
                required,
            )
            .expect("permit");
            let permit = decode_signature_checked_bootstrap_permit(
                &permit_record,
                self.folder_id,
                self.authority_writer,
                &self.authority_key.verifying_key(),
            )
            .expect("checked permit");
            let receipt_record =
                encode_signed_bootstrap_receipt(candidate_key, &permit, applied, [8; 32])
                    .expect("receipt");
            let receipt = decode_signature_checked_bootstrap_receipt(
                &receipt_record,
                &permit,
                candidate.writer_key(),
            )
            .expect("checked receipt");
            (
                Self::envelope(EventKind::BootstrapPermit, &permit_record),
                Self::envelope(EventKind::BootstrapReceipt, &receipt_record),
                receipt.digest().to_bytes(),
            )
        }

        fn next_epoch(
            &self,
            epoch: u64,
            previous: EpochDigest,
            roster: &[MemberGrant],
            receipts: &[[u8; 32]],
        ) -> (Vec<u8>, EpochDigest) {
            let record = encode_signed_epoch(
                &self.authority_key,
                self.folder_id,
                epoch,
                self.authority_writer,
                Some(previous),
                roster,
                None,
                &[],
                &[],
                &[],
                receipts,
            )
            .expect("next epoch");
            let digest = decode_signature_checked_epoch(
                &record,
                self.folder_id,
                self.authority_writer,
                &self.authority_key.verifying_key(),
            )
            .expect("checked next epoch")
            .digest();
            (Self::envelope(EventKind::MembershipEpoch, &record), digest)
        }
    }

    fn append(machine: &mut FolderEventMachine, event: &[u8]) {
        let PreparedEvent::Append(prepared) = machine.prepare_event(event).expect("prepare") else {
            panic!("unexpected duplicate");
        };
        machine.commit_checked(prepared).expect("commit");
    }

    fn prepare_error(machine: &FolderEventMachine, event: &[u8]) -> FolderEventError {
        match machine.prepare_event(event) {
            Err(error) => error,
            Ok(_) => panic!("event unexpectedly prepared"),
        }
    }

    fn construction_error(config: FolderMachineConfig) -> FolderEventError {
        match FolderEventMachine::new(config) {
            Err(error) => error,
            Ok(_) => panic!("configuration unexpectedly accepted"),
        }
    }

    fn directory() -> EntryValue {
        EntryValue::Directory
    }

    #[test]
    fn genesis_and_folder_global_cross_path_history_are_accepted() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());

        let context = machine
            .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
            .expect("first publication context");
        assert_eq!(context.folder_id(), fixture.folder_id);
        assert_eq!(context.writer_id(), fixture.local_writer);
        assert_eq!(context.membership_epoch(), 1);
        assert_eq!(
            context.membership_epoch_digest(),
            fixture.genesis_digest.to_bytes()
        );
        assert_eq!(context.counter(), 1);
        assert_eq!(context.predecessor(), None);
        assert_eq!(context.clock_entries().len(), 1);

        let first = fixture.operation(1, None, context.clock_entries(), "a", directory());
        append(&mut machine, &first);
        let first_record = &first[1..];
        let first_checked = decode_signature_checked_operation(
            first_record,
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("first checked");

        let context = machine
            .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
            .expect("second context");
        assert_eq!(context.counter(), 2);
        assert_eq!(
            context.predecessor(),
            Some(first_checked.digest().to_bytes())
        );
        assert_eq!(context.clock_entries()[0].counter(), 2);
        let second = fixture.operation(
            2,
            Some(first_checked.digest()),
            context.clock_entries(),
            "b",
            directory(),
        );
        append(&mut machine, &second);
        let second_checked = decode_signature_checked_operation(
            &second[1..],
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("second checked");

        let context = machine
            .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
            .expect("third context");
        let third = fixture.operation(
            3,
            Some(second_checked.digest()),
            context.clock_entries(),
            "a",
            EntryValue::Tombstone,
        );
        append(&mut machine, &third);

        let path_a = SyncPath::from_wire("a").expect("path a");
        let path_b = SyncPath::from_wire("b").expect("path b");
        let active_a: Vec<_> = machine
            .register(&path_a)
            .expect("a register")
            .active()
            .collect();
        assert_eq!(active_a.len(), 1);
        assert_eq!(active_a[0].id().counter(), 3);
        assert_eq!(active_a[0].value(), &EntryValue::Tombstone);
        assert_eq!(
            machine
                .register(&path_b)
                .expect("b register")
                .active_count(),
            1
        );
        assert_eq!(
            machine
                .current_frontier()
                .counter(writer(20).into_vector_actor()),
            3
        );
        assert!(
            machine
                .header(OpId::new(writer(20).into_vector_actor(), 1).expect("id"))
                .expect("history")
                .is_some(),
            "dominated path values must not erase accepted history"
        );
        assert!(matches!(
            machine.prepare_event(&third),
            Ok(PreparedEvent::Duplicate)
        ));

        let conflicting = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "different",
            directory(),
        );
        assert_eq!(
            prepare_error(&machine, &conflicting),
            FolderEventError::Equivocation
        );
    }

    #[test]
    fn stale_prepared_operation_is_rejected_before_any_mutation() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        let clock = [ClockEntry::new(fixture.authority_writer, 1).expect("clock")];
        let first = fixture.operation(1, None, &clock, "first", directory());
        let stale = fixture.operation(1, None, &clock, "stale", directory());
        let PreparedEvent::Append(first_prepared) = machine.prepare_event(&first).expect("first")
        else {
            panic!("append");
        };
        let PreparedEvent::Append(stale_prepared) = machine.prepare_event(&stale).expect("stale")
        else {
            panic!("append");
        };
        machine
            .commit_checked(first_prepared)
            .expect("first commit");
        assert_eq!(
            machine.commit_checked(stale_prepared),
            Err(FolderEventError::StalePrepared)
        );
        assert_eq!(machine.operations.len(), 1);
        assert!(
            machine
                .register(&SyncPath::from_wire("stale").expect("path"))
                .is_none()
        );
    }

    #[test]
    fn exact_index_and_count_quotas_fail_without_mutation() {
        let fixture = Fixture::new();
        let genesis_bytes = MACHINE_INDEX_BYTES
            + EPOCH_INDEX_BYTES
            + MEMBER_INDEX_BYTES
            + WRITER_TIMELINE_INDEX_BYTES
            + fixture.genesis_record.len() as u64;
        let exact_genesis_limits = FolderMachineLimits {
            maximum_index_bytes: genesis_bytes,
            maximum_operations: 1,
            maximum_paths: 1,
            maximum_membership_epochs: 1,
            maximum_pending_evidence_bytes: 1,
            maximum_pending_evidence_records: 1,
        };
        let mut exact =
            FolderEventMachine::new(fixture.config(exact_genesis_limits)).expect("exact machine");
        append(&mut exact, &fixture.genesis_event());
        assert_eq!(exact.index_bytes, genesis_bytes);

        let operation = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "quota",
            directory(),
        );
        assert_eq!(
            prepare_error(&exact, &operation),
            FolderEventError::ResourceLimit
        );

        let mut generous = fixture.machine();
        append(&mut generous, &fixture.genesis_event());
        let PreparedEvent::Append(prepared) = generous.prepare_event(&operation).expect("prepare")
        else {
            panic!("append");
        };
        let exact_operation_bytes = match &prepared.change {
            PreparedChange::Operation { index_bytes, .. } => *index_bytes,
            _ => panic!("operation"),
        };
        let mut exact = FolderEventMachine::new(fixture.config(FolderMachineLimits {
            maximum_index_bytes: exact_operation_bytes,
            maximum_operations: 1,
            maximum_paths: 1,
            maximum_membership_epochs: 1,
            maximum_pending_evidence_bytes: 1,
            maximum_pending_evidence_records: 1,
        }))
        .expect("machine");
        append(&mut exact, &fixture.genesis_event());
        append(&mut exact, &operation);
        assert_eq!(exact.index_bytes, exact_operation_bytes);

        let before_revision = exact.revision;
        let second_context = exact
            .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
            .expect("context");
        let first_checked = decode_signature_checked_operation(
            &operation[1..],
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("checked");
        let second = fixture.operation(
            2,
            Some(first_checked.digest()),
            second_context.clock_entries(),
            "second",
            directory(),
        );
        assert_eq!(
            prepare_error(&exact, &second),
            FolderEventError::ResourceLimit
        );
        assert_eq!(exact.revision, before_revision);
        assert_eq!(exact.operations.len(), 1);
        assert!(
            exact
                .register(&SyncPath::from_wire("second").expect("path"))
                .is_none()
        );

        let below = FolderEventMachine::new(fixture.config(FolderMachineLimits {
            maximum_index_bytes: genesis_bytes - 1,
            maximum_operations: 1,
            maximum_paths: 1,
            maximum_membership_epochs: 1,
            maximum_pending_evidence_bytes: 1,
            maximum_pending_evidence_records: 1,
        }))
        .expect("below machine");
        assert_eq!(
            prepare_error(&below, &fixture.genesis_event()),
            FolderEventError::ResourceLimit
        );
        assert!(below.current_head().is_none());
        assert_eq!(below.revision, 0);
    }

    #[test]
    fn invalid_namespace_signature_body_and_epoch_fail_closed() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        let first_clock = [ClockEntry::new(fixture.authority_writer, 1).expect("clock")];
        let before_genesis = fixture.operation(1, None, &first_clock, "a", directory());
        assert_eq!(
            prepare_error(&machine, &before_genesis),
            FolderEventError::MissingGenesis
        );

        let foreign_grant = MemberGrant::new(
            fixture.authority_writer,
            fixture.authority_key.verifying_key(),
            device(30),
            key(2).verifying_key(),
            MemberRole::ReadWrite,
        )
        .expect("grant");
        let foreign_genesis = encode_signed_epoch(
            &fixture.authority_key,
            folder(999),
            1,
            fixture.authority_writer,
            None,
            &[foreign_grant],
            None,
            &[],
            &[],
            &[],
            &[],
        )
        .expect("foreign genesis");
        assert_eq!(
            prepare_error(
                &machine,
                &Fixture::envelope(EventKind::MembershipEpoch, &foreign_genesis),
            ),
            FolderEventError::InvalidEvent
        );
        append(&mut machine, &fixture.genesis_event());

        let invalid_body_record = encode_signed_operation(
            &fixture.authority_key,
            fixture.folder_id,
            fixture.authority_writer,
            1,
            fixture.genesis_digest.to_bytes(),
            1,
            None,
            &first_clock,
            b"not a canonical body",
        )
        .expect("signed invalid body");
        assert_eq!(
            prepare_error(
                &machine,
                &Fixture::envelope(EventKind::Operation, &invalid_body_record),
            ),
            FolderEventError::InvalidEvent
        );

        let mut bad_signature = before_genesis.clone();
        *bad_signature.last_mut().expect("signature byte") ^= 1;
        assert_eq!(
            prepare_error(&machine, &bad_signature),
            FolderEventError::InvalidEvent
        );

        let wrong_epoch_record = encode_signed_operation(
            &fixture.authority_key,
            fixture.folder_id,
            fixture.authority_writer,
            2,
            [9; 32],
            1,
            None,
            &first_clock,
            &OperationBody::new(SyncPath::from_wire("x").expect("path"), directory()).encode(),
        )
        .expect("wrong epoch operation");
        assert_eq!(
            prepare_error(
                &machine,
                &Fixture::envelope(EventKind::Operation, &wrong_epoch_record),
            ),
            FolderEventError::UnauthorizedOperation
        );

        let foreign_record = encode_signed_operation(
            &fixture.authority_key,
            folder(777),
            fixture.authority_writer,
            1,
            fixture.genesis_digest.to_bytes(),
            1,
            None,
            &first_clock,
            &OperationBody::new(SyncPath::from_wire("x").expect("path"), directory()).encode(),
        )
        .expect("foreign operation");
        assert_eq!(
            prepare_error(
                &machine,
                &Fixture::envelope(EventKind::Operation, &foreign_record),
            ),
            FolderEventError::InvalidEvent
        );
        assert!(machine.operations.is_empty());
        assert!(machine.registers.is_empty());
    }

    #[test]
    fn unsupported_write_loss_and_invalid_bootstrap_kinds_never_mutate() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        let revision = machine.revision;
        for (kind, magic, expected) in [
            (
                EventKind::BootstrapPermit,
                b"COVSBP01",
                FolderEventError::InvalidEvent,
            ),
            (
                EventKind::BootstrapReceipt,
                b"COVSBR01",
                FolderEventError::InvalidEvent,
            ),
            (
                EventKind::WriteLossProposal,
                b"COVSFP01",
                FolderEventError::UnsupportedEvent,
            ),
            (
                EventKind::FreezeReceipt,
                b"COVSFR01",
                FolderEventError::UnsupportedEvent,
            ),
            (
                EventKind::FreezeAbort,
                b"COVSFA01",
                FolderEventError::UnsupportedEvent,
            ),
        ] {
            let mut record = magic.to_vec();
            record.extend_from_slice(&1_u16.to_be_bytes());
            record.push(0);
            let event = Fixture::envelope(kind, &record);
            assert_eq!(prepare_error(&machine, &event), expected);
        }
        assert!(matches!(
            machine.prepare_event(&fixture.genesis_event()),
            Ok(PreparedEvent::Duplicate)
        ));
        assert_eq!(machine.revision, revision);
    }

    #[test]
    fn publication_and_archive_are_bound_to_exact_local_identity_and_head() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        assert_eq!(
            machine
                .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
                .expect_err("no genesis"),
            FolderEventError::PublicationDenied
        );
        append(&mut machine, &fixture.genesis_event());
        assert_eq!(machine.folder_id(), fixture.folder_id);
        let head = machine.current_head().expect("head");
        assert_eq!(head.epoch(), 1);
        assert_eq!(head.digest(), fixture.genesis_digest);
        assert_eq!(head.roster().len(), 1);
        assert_eq!(head.canonical_record(), fixture.genesis_record);
        assert_eq!(
            machine.retained_writer_count(fixture.folder_id, fixture.genesis_digest),
            Ok(1)
        );
        assert_eq!(
            machine.writer_lifetime(
                fixture.folder_id,
                fixture.genesis_digest,
                fixture.local_writer
            ),
            Ok(WriterLifetime::Active)
        );
        assert_eq!(
            machine.writer_key_assignment(
                fixture.folder_id,
                fixture.genesis_digest,
                &fixture.authority_key.verifying_key()
            ),
            Ok(HistoricalWriterKeyAssignment::AssignedTo(
                fixture.local_writer
            ))
        );
        assert_eq!(
            machine.retained_writer_count(folder(999), fixture.genesis_digest),
            Err(MembershipArchiveQueryError::UnknownBaseHead)
        );
        assert_eq!(
            machine
                .publication_context(writer(999), &fixture.authority_key.verifying_key())
                .expect_err("foreign local identity"),
            FolderEventError::PublicationDenied
        );
        assert_eq!(
            machine
                .publication_context(fixture.local_writer, &key(9).verifying_key())
                .expect_err("wrong key"),
            FolderEventError::PublicationDenied
        );
        let debug = format!("{:?}", fixture.config(GENEROUS_LIMITS));
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains(&format!("{:?}", fixture.authority_key.to_bytes())));
    }

    #[test]
    fn invalid_configuration_is_rejected_without_state() {
        let fixture = Fixture::new();
        for limits in [
            FolderMachineLimits {
                maximum_index_bytes: MACHINE_INDEX_BYTES - 1,
                ..GENEROUS_LIMITS
            },
            FolderMachineLimits {
                maximum_operations: 0,
                ..GENEROUS_LIMITS
            },
            FolderMachineLimits {
                maximum_paths: 0,
                ..GENEROUS_LIMITS
            },
            FolderMachineLimits {
                maximum_membership_epochs: 0,
                ..GENEROUS_LIMITS
            },
            FolderMachineLimits {
                maximum_pending_evidence_bytes: 0,
                ..GENEROUS_LIMITS
            },
            FolderMachineLimits {
                maximum_pending_evidence_records: 0,
                ..GENEROUS_LIMITS
            },
        ] {
            assert_eq!(
                construction_error(fixture.config(limits)),
                FolderEventError::InvalidLimits
            );
        }
        let mut weak = fixture.config(GENEROUS_LIMITS);
        let mut identity = [0_u8; 32];
        identity[0] = 1;
        weak.pinned_authority_key = VerifyingKey::from_bytes(&identity).expect("weak point");
        assert!(weak.pinned_authority_key.is_weak());
        assert_eq!(
            construction_error(weak),
            FolderEventError::InvalidAuthorityKey
        );
    }

    #[test]
    fn prepared_values_cannot_cross_machine_instances() {
        let fixture = Fixture::new();
        let source = fixture.machine();
        let PreparedEvent::Append(foreign_genesis) = source
            .prepare_event(&fixture.genesis_event())
            .expect("prepared genesis")
        else {
            panic!("append");
        };
        let mut other_folder = FolderEventMachine::new(FolderMachineConfig {
            folder_id: folder(999),
            ..fixture.config(GENEROUS_LIMITS)
        })
        .expect("other folder");
        assert_eq!(
            other_folder.commit_checked(foreign_genesis),
            Err(FolderEventError::StalePrepared)
        );
        assert!(other_folder.current_head().is_none());
        assert_eq!(other_folder.revision, 0);

        let mut left = fixture.machine();
        let mut right = fixture.machine();
        append(&mut left, &fixture.genesis_event());
        append(&mut right, &fixture.genesis_event());
        let first_clock = [ClockEntry::new(fixture.authority_writer, 1).expect("clock")];
        let left_first = fixture.operation(1, None, &first_clock, "left", directory());
        let right_first = fixture.operation(1, None, &first_clock, "right", directory());
        append(&mut left, &left_first);
        append(&mut right, &right_first);
        assert_eq!(left.revision, right.revision);

        let left_checked = decode_signature_checked_operation(
            &left_first[1..],
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("left checked");
        let left_context = left
            .publication_context(fixture.local_writer, &fixture.authority_key.verifying_key())
            .expect("left context");
        let left_second = fixture.operation(
            2,
            Some(left_checked.digest()),
            left_context.clock_entries(),
            "left-two",
            directory(),
        );
        let PreparedEvent::Append(left_prepared) =
            left.prepare_event(&left_second).expect("left prepared")
        else {
            panic!("append");
        };
        let right_revision = right.revision;
        let right_operations = right.operations.len();
        assert_eq!(
            right.commit_checked(left_prepared),
            Err(FolderEventError::StalePrepared)
        );
        assert_eq!(right.revision, right_revision);
        assert_eq!(right.operations.len(), right_operations);
        assert!(
            right
                .register(&SyncPath::from_wire("left-two").expect("path"))
                .is_none()
        );
    }

    #[test]
    fn missing_author_history_is_retryable_after_gap_is_filled() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        let first_clock = [ClockEntry::new(fixture.authority_writer, 1).expect("clock")];
        let first = fixture.operation(1, None, &first_clock, "first", directory());
        let first_checked = decode_signature_checked_operation(
            &first[1..],
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("first checked");
        let second_clock = [ClockEntry::new(fixture.authority_writer, 2).expect("clock")];
        let second = fixture.operation(
            2,
            Some(first_checked.digest()),
            &second_clock,
            "second",
            directory(),
        );
        assert_eq!(
            prepare_error(&machine, &second),
            FolderEventError::MissingHistory
        );
        assert!(machine.operations.is_empty());
        assert!(machine.registers.is_empty());
        let revision = machine.revision;

        append(&mut machine, &first);
        append(&mut machine, &second);
        assert_eq!(machine.revision, revision + 2);
        assert_eq!(machine.operations.len(), 2);
    }

    #[test]
    fn revision_exhaustion_rejects_append_preflight_but_allows_duplicate() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        machine.revision = u64::MAX;
        assert!(matches!(
            machine.prepare_event(&fixture.genesis_event()),
            Ok(PreparedEvent::Duplicate)
        ));
        let operation = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "blocked",
            directory(),
        );
        assert_eq!(
            prepare_error(&machine, &operation),
            FolderEventError::ResourceLimit
        );
        assert!(machine.operations.is_empty());
        assert!(machine.registers.is_empty());
    }

    #[test]
    fn bootstrap_add_replays_two_writer_concurrency_and_global_publication_clock() {
        let fixture = Fixture::new();
        let candidate_writer = writer(40);
        let candidate_key = key(4);
        let candidate = fixture.grant(
            candidate_writer,
            &candidate_key,
            50,
            &key(5),
            MemberRole::ReadWrite,
        );
        let mut authority_machine = fixture.machine();
        append(&mut authority_machine, &fixture.genesis_event());
        let authority_first = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "shared",
            directory(),
        );
        append(&mut authority_machine, &authority_first);
        let authority_first_checked = decode_signature_checked_operation(
            &authority_first[1..],
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("first authority operation");
        let bootstrap_frontier = [ClockEntry::new(fixture.authority_writer, 1).expect("frontier")];
        let (permit, receipt, receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &candidate,
            &candidate_key,
            7,
            &bootstrap_frontier,
            &bootstrap_frontier,
        );
        let (epoch_two, epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), candidate.clone()],
            &[receipt_digest],
        );
        assert_eq!(
            prepare_error(&authority_machine, &epoch_two),
            FolderEventError::MissingEvidence
        );
        append(&mut authority_machine, &permit);
        assert_eq!(
            prepare_error(&authority_machine, &epoch_two),
            FolderEventError::MissingEvidence
        );
        append(&mut authority_machine, &receipt);

        let mut unauthenticated_conflict = receipt.clone();
        *unauthenticated_conflict
            .last_mut()
            .expect("receipt signature byte") ^= 1;
        assert_eq!(
            prepare_error(&authority_machine, &unauthenticated_conflict),
            FolderEventError::InvalidEvent
        );
        let permit_envelope = EventEnvelope::parse(&permit).expect("permit envelope");
        let checked_permit = decode_signature_checked_bootstrap_permit(
            permit_envelope.record(),
            fixture.folder_id,
            fixture.authority_writer,
            &fixture.authority_key.verifying_key(),
        )
        .expect("checked permit");
        let signed_conflict_record = encode_signed_bootstrap_receipt(
            &candidate_key,
            &checked_permit,
            &bootstrap_frontier,
            [9; 32],
        )
        .expect("signed conflicting receipt");
        let signed_conflict =
            Fixture::envelope(EventKind::BootstrapReceipt, &signed_conflict_record);
        assert_eq!(
            prepare_error(&authority_machine, &signed_conflict),
            FolderEventError::Equivocation
        );
        append(&mut authority_machine, &epoch_two);
        assert_eq!(authority_machine.pending_evidence_records, 0);
        assert_eq!(authority_machine.pending_evidence_bytes, 0);
        assert!(matches!(
            authority_machine.prepare_event(&permit),
            Ok(PreparedEvent::Duplicate)
        ));
        assert!(matches!(
            authority_machine.prepare_event(&receipt),
            Ok(PreparedEvent::Duplicate)
        ));
        assert_eq!(authority_machine.pending_evidence_records, 0);

        let authority_context = authority_machine
            .publication_context(
                fixture.authority_writer,
                &fixture.authority_key.verifying_key(),
            )
            .expect("authority context before concurrency");
        assert_eq!(authority_context.counter(), 2);
        let authority_second = fixture.operation_for(
            &fixture.authority_key,
            fixture.authority_writer,
            2,
            epoch_two_digest,
            2,
            Some(authority_first_checked.digest()),
            authority_context.clock_entries(),
            "shared",
            EntryValue::Tombstone,
        );
        let candidate_clock = [
            ClockEntry::new(fixture.authority_writer, 1).expect("authority component"),
            ClockEntry::new(candidate_writer, 1).expect("candidate component"),
        ];
        let candidate_first = fixture.operation_for(
            &candidate_key,
            candidate_writer,
            2,
            epoch_two_digest,
            1,
            None,
            &candidate_clock,
            "shared",
            directory(),
        );
        append(&mut authority_machine, &candidate_first);
        append(&mut authority_machine, &authority_second);
        let shared = SyncPath::from_wire("shared").expect("shared path");
        let active: Vec<_> = authority_machine
            .register(&shared)
            .expect("shared register")
            .active()
            .collect();
        assert_eq!(active.len(), 2);
        assert!(
            active
                .iter()
                .any(|entry| entry.id().actor() == candidate_writer.into_vector_actor())
        );
        assert!(
            active
                .iter()
                .any(|entry| entry.id().actor() == fixture.authority_writer.into_vector_actor())
        );
        let next_authority = authority_machine
            .publication_context(
                fixture.authority_writer,
                &fixture.authority_key.verifying_key(),
            )
            .expect("joined authority context");
        assert_eq!(next_authority.counter(), 3);
        assert_eq!(next_authority.clock_entries().len(), 2);
        assert_eq!(
            next_authority
                .clock_entries()
                .iter()
                .find(|entry| entry.writer_id() == candidate_writer)
                .expect("remote component")
                .counter(),
            1
        );

        let joining_config = FolderMachineConfig {
            local_writer_id: candidate_writer,
            ..fixture.config(GENEROUS_LIMITS)
        };
        let mut joining = FolderEventMachine::new(joining_config).expect("joining machine");
        append(&mut joining, &fixture.genesis_event());
        append(&mut joining, &authority_first);
        assert_eq!(
            joining
                .publication_context(candidate_writer, &candidate_key.verifying_key())
                .err(),
            Some(FolderEventError::PublicationDenied)
        );
        for event in [
            &permit,
            &receipt,
            &epoch_two,
            &candidate_first,
            &authority_second,
        ] {
            append(&mut joining, event);
        }
        let joining_context = joining
            .publication_context(candidate_writer, &candidate_key.verifying_key())
            .expect("joined candidate context");
        assert_eq!(joining_context.counter(), 2);
        assert_eq!(joining_context.clock_entries().len(), 2);
        assert_eq!(joining.register(&shared).expect("shared").active_count(), 2);

        assert_eq!(
            joining.retained_writer_count(fixture.folder_id, fixture.genesis_digest),
            Ok(1)
        );
        assert_eq!(
            joining.writer_lifetime(fixture.folder_id, fixture.genesis_digest, candidate_writer),
            Ok(WriterLifetime::NeverSeen)
        );
        assert_eq!(
            joining.writer_key_assignment(
                fixture.folder_id,
                fixture.genesis_digest,
                &candidate_key.verifying_key()
            ),
            Ok(HistoricalWriterKeyAssignment::NeverAssigned)
        );
        assert_eq!(
            joining.retained_writer_count(fixture.folder_id, epoch_two_digest),
            Ok(2)
        );
    }

    #[test]
    fn read_member_upgrade_enforces_bootstrap_floor_and_old_epoch_continuity() {
        let fixture = Fixture::new();
        let candidate_writer = writer(40);
        let candidate_key = key(4);
        let read_grant = fixture.grant(
            candidate_writer,
            &candidate_key,
            50,
            &key(5),
            MemberRole::Read,
        );
        let mut machine = FolderEventMachine::new(FolderMachineConfig {
            local_writer_id: candidate_writer,
            ..fixture.config(GENEROUS_LIMITS)
        })
        .expect("candidate machine");
        append(&mut machine, &fixture.genesis_event());

        let (add_permit, add_receipt, add_receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &read_grant,
            &candidate_key,
            11,
            &[],
            &[],
        );
        let (epoch_two, epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), read_grant.clone()],
            &[add_receipt_digest],
        );
        append(&mut machine, &add_permit);
        append(&mut machine, &add_receipt);
        append(&mut machine, &epoch_two);
        assert_eq!(
            machine
                .publication_context(candidate_writer, &candidate_key.verifying_key())
                .err(),
            Some(FolderEventError::PublicationDenied)
        );

        let delayed_authority = fixture.operation_for(
            &fixture.authority_key,
            fixture.authority_writer,
            1,
            fixture.genesis_digest,
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "old-epoch",
            directory(),
        );
        append(&mut machine, &delayed_authority);

        let write_grant = MemberGrant::new(
            candidate_writer,
            candidate_key.verifying_key(),
            read_grant.transport_device_id(),
            *read_grant.transport_key(),
            MemberRole::ReadWrite,
        )
        .expect("upgraded grant");
        let floor = [ClockEntry::new(fixture.authority_writer, 1).expect("floor")];
        let (upgrade_permit, upgrade_receipt, upgrade_receipt_digest) = fixture.bootstrap_pair(
            2,
            epoch_two_digest,
            &write_grant,
            &candidate_key,
            12,
            &floor,
            &floor,
        );
        let (epoch_three, epoch_three_digest) = fixture.next_epoch(
            3,
            epoch_two_digest,
            &[fixture.authority_grant(), write_grant],
            &[upgrade_receipt_digest],
        );
        append(&mut machine, &upgrade_permit);
        append(&mut machine, &upgrade_receipt);
        append(&mut machine, &epoch_three);

        let context = machine
            .publication_context(candidate_writer, &candidate_key.verifying_key())
            .expect("upgraded publication context");
        assert_eq!(context.membership_epoch(), 3);
        assert_eq!(
            context.membership_epoch_digest(),
            epoch_three_digest.to_bytes()
        );
        assert_eq!(context.counter(), 1);
        assert_eq!(context.clock_entries().len(), 2);
        assert_eq!(
            context
                .clock_entries()
                .iter()
                .find(|entry| entry.writer_id() == fixture.authority_writer)
                .expect("bootstrap floor component")
                .counter(),
            1
        );

        let below_floor = fixture.operation_for(
            &candidate_key,
            candidate_writer,
            3,
            epoch_three_digest,
            1,
            None,
            &[ClockEntry::new(candidate_writer, 1).expect("self clock")],
            "below-floor",
            directory(),
        );
        assert_eq!(
            prepare_error(&machine, &below_floor),
            FolderEventError::UnauthorizedOperation
        );
        assert!(
            machine
                .register(&SyncPath::from_wire("below-floor").expect("path"))
                .is_none()
        );

        let accepted = fixture.operation_for(
            &candidate_key,
            candidate_writer,
            3,
            epoch_three_digest,
            1,
            None,
            context.clock_entries(),
            "at-floor",
            directory(),
        );
        append(&mut machine, &accepted);
        assert_eq!(machine.current_frontier().actor_count(), 2);

        let before_add = fixture.operation_for(
            &candidate_key,
            candidate_writer,
            1,
            fixture.genesis_digest,
            1,
            None,
            &[
                ClockEntry::new(fixture.authority_writer, 1).expect("authority clock"),
                ClockEntry::new(candidate_writer, 1).expect("candidate clock"),
            ],
            "pre-membership",
            directory(),
        );
        assert_eq!(
            prepare_error(&machine, &before_add),
            FolderEventError::UnauthorizedOperation
        );
    }

    #[test]
    fn ordinary_read_only_removal_is_historical_and_read_write_loss_is_unsupported() {
        let fixture = Fixture::new();
        let reader_writer = writer(40);
        let reader_key = key(4);
        let reader = fixture.grant(reader_writer, &reader_key, 50, &key(5), MemberRole::Read);
        let mut machine = FolderEventMachine::new(FolderMachineConfig {
            local_writer_id: reader_writer,
            ..fixture.config(GENEROUS_LIMITS)
        })
        .expect("reader machine");
        append(&mut machine, &fixture.genesis_event());
        let (permit, receipt, receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &reader,
            &reader_key,
            21,
            &[],
            &[],
        );
        let (epoch_two, epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), reader.clone()],
            &[receipt_digest],
        );
        append(&mut machine, &permit);
        append(&mut machine, &receipt);
        append(&mut machine, &epoch_two);

        let (epoch_three, epoch_three_digest) =
            fixture.next_epoch(3, epoch_two_digest, &[fixture.authority_grant()], &[]);
        append(&mut machine, &epoch_three);
        assert_eq!(
            machine.writer_lifetime(fixture.folder_id, epoch_two_digest, reader_writer),
            Ok(WriterLifetime::Active)
        );
        assert_eq!(
            machine.writer_lifetime(fixture.folder_id, epoch_three_digest, reader_writer),
            Ok(WriterLifetime::Removed)
        );
        assert_eq!(
            machine.writer_key_assignment(
                fixture.folder_id,
                epoch_three_digest,
                &reader_key.verifying_key()
            ),
            Ok(HistoricalWriterKeyAssignment::AssignedTo(reader_writer))
        );
        assert_eq!(
            machine.retained_writer_count(fixture.folder_id, epoch_three_digest),
            Ok(2)
        );
        assert_eq!(
            machine
                .publication_context(reader_writer, &reader_key.verifying_key())
                .err(),
            Some(FolderEventError::PublicationDenied)
        );

        let mut loss_machine = fixture.machine();
        append(&mut loss_machine, &fixture.genesis_event());
        let loss_writer = writer(60);
        let loss_key = key(6);
        let loss_grant = fixture.grant(loss_writer, &loss_key, 70, &key(7), MemberRole::ReadWrite);
        let (loss_permit, loss_receipt, loss_receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &loss_grant,
            &loss_key,
            22,
            &[],
            &[],
        );
        let (loss_epoch_two, loss_epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), loss_grant],
            &[loss_receipt_digest],
        );
        append(&mut loss_machine, &loss_permit);
        append(&mut loss_machine, &loss_receipt);
        append(&mut loss_machine, &loss_epoch_two);
        let (write_loss, _) =
            fixture.next_epoch(3, loss_epoch_two_digest, &[fixture.authority_grant()], &[]);
        let revision = loss_machine.revision;
        assert_eq!(
            prepare_error(&loss_machine, &write_loss),
            FolderEventError::UnsupportedEvent
        );
        assert_eq!(loss_machine.revision, revision);
        assert_eq!(loss_machine.current_head().expect("head").epoch(), 2);
    }

    #[test]
    fn evidence_equivocation_and_pending_quota_fail_without_mutation() {
        let fixture = Fixture::new();
        let candidate_writer = writer(40);
        let candidate_key = key(4);
        let candidate = fixture.grant(
            candidate_writer,
            &candidate_key,
            50,
            &key(5),
            MemberRole::ReadWrite,
        );
        let limits = FolderMachineLimits {
            maximum_pending_evidence_records: 1,
            ..GENEROUS_LIMITS
        };
        let mut machine = FolderEventMachine::new(fixture.config(limits)).expect("quota machine");
        append(&mut machine, &fixture.genesis_event());
        let (permit, receipt, receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &candidate,
            &candidate_key,
            31,
            &[],
            &[],
        );
        append(&mut machine, &permit);
        let revision = machine.revision;
        let index_bytes = machine.index_bytes;
        assert_eq!(machine.pending_evidence_records, 1);
        assert_eq!(
            prepare_error(&machine, &receipt),
            FolderEventError::ResourceLimit
        );
        assert_eq!(machine.revision, revision);
        assert_eq!(machine.index_bytes, index_bytes);
        assert_eq!(machine.receipts.len(), 0);
        assert_eq!(machine.pending_receipts.len(), 0);

        let conflicting_record = encode_signed_bootstrap_permit(
            &fixture.authority_key,
            fixture.folder_id,
            fixture.authority_writer,
            1,
            fixture.genesis_digest,
            &candidate,
            [31; 32],
            2,
            &[],
        )
        .expect("conflicting permit");
        let conflicting = Fixture::envelope(EventKind::BootstrapPermit, &conflicting_record);
        assert_eq!(
            prepare_error(&machine, &conflicting),
            FolderEventError::Equivocation
        );
        assert_eq!(machine.revision, revision);

        let (transition, _) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), candidate],
            &[receipt_digest],
        );
        assert_eq!(
            prepare_error(&machine, &transition),
            FolderEventError::MissingEvidence
        );
        assert_eq!(machine.revision, revision);
    }

    #[test]
    fn membership_noop_and_missing_frontier_history_fail_without_mutation() {
        let fixture = Fixture::new();
        let candidate_writer = writer(40);
        let candidate_key = key(4);
        let candidate = fixture.grant(
            candidate_writer,
            &candidate_key,
            50,
            &key(5),
            MemberRole::ReadWrite,
        );
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        let revision = machine.revision;
        let (noop, _) =
            fixture.next_epoch(2, fixture.genesis_digest, &[fixture.authority_grant()], &[]);
        assert_eq!(
            prepare_error(&machine, &noop),
            FolderEventError::InvalidEvent
        );

        let missing = [ClockEntry::new(fixture.authority_writer, 1).expect("missing frontier")];
        let (permit, _, _) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &candidate,
            &candidate_key,
            41,
            &missing,
            &missing,
        );
        assert_eq!(
            prepare_error(&machine, &permit),
            FolderEventError::MissingHistory
        );
        assert_eq!(machine.revision, revision);
        assert!(machine.permits.is_empty());
        assert!(machine.pending_permits.is_empty());

        let operation = fixture.operation(1, None, &missing, "required", directory());
        append(&mut machine, &operation);
        append(&mut machine, &permit);
        assert_eq!(machine.pending_evidence_records, 1);
    }

    #[test]
    fn multi_actor_evidence_frontier_has_an_exact_index_boundary() {
        let fixture = Fixture::new();
        let first_writer = writer(40);
        let first_key = key(4);
        let first_grant =
            fixture.grant(first_writer, &first_key, 50, &key(5), MemberRole::ReadWrite);
        let authority_operation = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "authority",
            directory(),
        );
        let authority_frontier = [ClockEntry::new(fixture.authority_writer, 1).expect("frontier")];
        let (first_permit, first_receipt, first_receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &first_grant,
            &first_key,
            61,
            &authority_frontier,
            &authority_frontier,
        );
        let (epoch_two, epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), first_grant],
            &[first_receipt_digest],
        );
        let joined_frontier = [
            ClockEntry::new(fixture.authority_writer, 1).expect("authority component"),
            ClockEntry::new(first_writer, 1).expect("first writer component"),
        ];
        let first_operation = fixture.operation_for(
            &first_key,
            first_writer,
            2,
            epoch_two_digest,
            1,
            None,
            &joined_frontier,
            "first-writer",
            directory(),
        );
        let second_writer = writer(60);
        let second_key = key(6);
        let second_grant = fixture.grant(second_writer, &second_key, 70, &key(7), MemberRole::Read);
        let (second_permit, _, _) = fixture.bootstrap_pair(
            2,
            epoch_two_digest,
            &second_grant,
            &second_key,
            62,
            &joined_frontier,
            &joined_frontier,
        );
        let base_events = [
            &fixture.genesis_event(),
            &authority_operation,
            &first_permit,
            &first_receipt,
            &epoch_two,
            &first_operation,
        ];
        let replay_base = |limits| {
            let mut machine =
                FolderEventMachine::new(fixture.config(limits)).expect("bounded machine");
            for event in base_events {
                append(&mut machine, event);
            }
            machine
        };

        let generous = replay_base(GENEROUS_LIMITS);
        let record_bytes = EventEnvelope::parse(&second_permit)
            .expect("permit envelope")
            .record()
            .len() as u64;
        let exact_index_bytes = generous
            .index_bytes
            .checked_add(EVIDENCE_INDEX_BYTES)
            .and_then(|bytes| bytes.checked_add(PENDING_REFERENCE_INDEX_BYTES))
            .and_then(|bytes| bytes.checked_add(record_bytes))
            .and_then(|bytes| bytes.checked_add(2 * CLOCK_COMPONENT_INDEX_BYTES))
            .expect("exact index charge");
        let PreparedEvent::Append(prepared) = generous
            .prepare_event(&second_permit)
            .expect("multi-actor evidence preflight")
        else {
            panic!("new permit");
        };
        assert!(matches!(
            prepared.change,
            PreparedChange::BootstrapPermit { index_bytes, .. }
                if index_bytes == exact_index_bytes
        ));

        let exact_limits = FolderMachineLimits {
            maximum_index_bytes: exact_index_bytes,
            ..GENEROUS_LIMITS
        };
        let mut exact = replay_base(exact_limits);
        append(&mut exact, &second_permit);
        assert_eq!(exact.index_bytes, exact_index_bytes);

        let below = replay_base(FolderMachineLimits {
            maximum_index_bytes: exact_index_bytes - 1,
            ..GENEROUS_LIMITS
        });
        let revision = below.revision;
        let pending = below.pending_evidence_records;
        assert_eq!(
            prepare_error(&below, &second_permit),
            FolderEventError::ResourceLimit
        );
        assert_eq!(below.revision, revision);
        assert_eq!(below.pending_evidence_records, pending);
        assert!(!below.permit_identities.values().any(|digest| {
            below
                .permits
                .get(digest)
                .is_some_and(|permit| permit.checked.candidate().writer_id() == second_writer)
        }));
    }

    #[test]
    fn encrypted_durable_log_replays_bootstrap_epoch_and_two_writer_history() {
        let fixture = Fixture::new();
        let candidate_writer = writer(40);
        let candidate_key = key(4);
        let candidate = fixture.grant(
            candidate_writer,
            &candidate_key,
            50,
            &key(5),
            MemberRole::ReadWrite,
        );
        let authority_operation = fixture.operation(
            1,
            None,
            &[ClockEntry::new(fixture.authority_writer, 1).expect("clock")],
            "durable-authority",
            directory(),
        );
        let frontier = [ClockEntry::new(fixture.authority_writer, 1).expect("frontier")];
        let (permit, receipt, receipt_digest) = fixture.bootstrap_pair(
            1,
            fixture.genesis_digest,
            &candidate,
            &candidate_key,
            51,
            &frontier,
            &frontier,
        );
        let (epoch_two, epoch_two_digest) = fixture.next_epoch(
            2,
            fixture.genesis_digest,
            &[fixture.authority_grant(), candidate],
            &[receipt_digest],
        );
        let candidate_operation = fixture.operation_for(
            &candidate_key,
            candidate_writer,
            2,
            epoch_two_digest,
            1,
            None,
            &[
                ClockEntry::new(fixture.authority_writer, 1).expect("authority component"),
                ClockEntry::new(candidate_writer, 1).expect("candidate component"),
            ],
            "durable-candidate",
            directory(),
        );

        let temp = tempfile::tempdir().expect("temporary state");
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700))
            .expect("private state mode");
        let directory = PrivateStateDir::open_root(temp.path()).expect("private state");
        let file_name = StateKey::new("events.v1").expect("state key");
        let binding = LogBinding::new(
            fixture.folder_id,
            Uuid::from_u128(101),
            Uuid::from_u128(102),
            LogFileKind::FolderEvents,
        );
        let frame_key_bytes = [103; 32];
        let limits = EventLogLimits {
            maximum_bytes: 4 * 1_024 * 1_024,
            maximum_records: 16,
        };
        let mut log = DurableEventLog::create(
            &directory,
            &file_name,
            binding,
            LogFrameKey::from_bytes(frame_key_bytes),
            limits,
            fixture.machine(),
        )
        .expect("create log");
        for (index, event) in [
            &fixture.genesis_event(),
            &authority_operation,
            &permit,
            &receipt,
            &epoch_two,
            &candidate_operation,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                log.append(event).expect("durable append"),
                EventAppendOutcome::Committed {
                    ordinal: index as u64 + 1
                }
            );
        }
        assert_eq!(log.machine().expect("machine").operations.len(), 2);
        drop(log);

        let joining_machine = FolderEventMachine::new(FolderMachineConfig {
            local_writer_id: candidate_writer,
            ..fixture.config(GENEROUS_LIMITS)
        })
        .expect("fresh joining machine");
        let replay = DurableEventLog::open(
            &directory,
            &file_name,
            binding,
            LogFrameKey::from_bytes(frame_key_bytes),
            limits,
            joining_machine,
        )
        .expect("replay log");
        assert_eq!(replay.committed_records().expect("records"), 6);
        let replayed = replay.machine().expect("replayed machine");
        assert_eq!(replayed.current_head().expect("head").epoch(), 2);
        assert_eq!(replayed.current_frontier().actor_count(), 2);
        assert_eq!(
            replayed
                .publication_context(candidate_writer, &candidate_key.verifying_key())
                .expect("candidate publication")
                .counter(),
            2
        );
        assert_eq!(replayed.pending_evidence_records, 0);
    }
}
