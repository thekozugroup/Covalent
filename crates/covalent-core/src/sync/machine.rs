//! Replay-derived admission state for one private folder event log.
//!
//! This first slice accepts one canonical genesis epoch followed by ordinary
//! operations authorized by that epoch. Bootstrap, membership transitions,
//! write-loss freezes, and reconciliation fail closed until their complete
//! durable state machines are implemented. Successful preparation is not a
//! peer acknowledgement and does not mutate files in the synchronized folder.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use covalent_protocol::DeviceId;
use ed25519_dalek::VerifyingKey;
use thiserror::Error;

use super::VersionVector;
use super::admission::{self, Admission, AuthorTip, History, HistoryQueryError};
use super::body::{EntryValue, OperationBody};
use super::event::{EventEnvelope, EventKind};
use super::event_log::{EventMachine, EventValidationError, PreparedEvent};
use super::ids::{FolderId, WriterId};
use super::membership::{
    EpochDigest, MemberGrant, MemberRole, SignatureCheckedEpoch, decode_signature_checked_epoch,
};
use super::membership_transition::{
    HistoricalWriterKeyAssignment, MembershipArchive, MembershipArchiveQueryError, WriterLifetime,
};
use super::operation::{ClockEntry, decode_signature_checked_operation};
use super::path::SyncPath;
use super::register::{CausalRegister, OpId, RegisterEntry};

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

/// Independent bounds for replay-derived in-memory indexes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FolderMachineLimits {
    /// Maximum deterministic charge for all retained machine indexes.
    pub maximum_index_bytes: u64,
    /// Maximum fully admitted operation records retained for exact history.
    pub maximum_operations: u64,
    /// Maximum distinct canonical paths retained by the causal registers.
    pub maximum_paths: u64,
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

enum PreparedChange {
    Genesis {
        epoch: SignatureCheckedEpoch,
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
    current_epoch: Option<SignatureCheckedEpoch>,
    operations: BTreeMap<OpId, AcceptedOperation>,
    tips: BTreeMap<WriterId, AuthorTip>,
    frontier: VersionVector,
    registers: BTreeMap<SyncPath, CausalRegister<EntryValue>>,
    index_bytes: u64,
}

impl FolderEventMachine {
    /// Creates empty replay state after validating fixed configuration.
    pub fn new(config: FolderMachineConfig) -> Result<Self, FolderEventError> {
        if config.limits.maximum_index_bytes < MACHINE_INDEX_BYTES
            || config.limits.maximum_operations == 0
            || config.limits.maximum_paths == 0
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
            current_epoch: None,
            operations: BTreeMap::new(),
            tips: BTreeMap::new(),
            frontier: VersionVector::empty(),
            registers: BTreeMap::new(),
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
        self.current_epoch
            .as_ref()
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
            .current_epoch
            .as_ref()
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
            EventKind::MembershipEpoch => self.prepare_genesis(&envelope),
            EventKind::Operation => self.prepare_operation(&envelope),
            EventKind::BootstrapPermit
            | EventKind::BootstrapReceipt
            | EventKind::WriteLossProposal
            | EventKind::FreezeReceipt
            | EventKind::FreezeAbort => Err(FolderEventError::UnsupportedEvent),
        }
    }

    fn prepare_genesis(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        if let Some(current) = &self.current_epoch {
            return if current.canonical_record() == envelope.record() {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(FolderEventError::UnsupportedEvent)
            };
        }
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
        self.require_revision_capacity()?;
        let index_bytes = self
            .index_bytes
            .checked_add(EPOCH_INDEX_BYTES)
            .and_then(|bytes| bytes.checked_add(envelope.record().len() as u64))
            .and_then(|bytes| {
                (epoch.roster().len() as u64)
                    .checked_mul(MEMBER_INDEX_BYTES)
                    .and_then(|charge| bytes.checked_add(charge))
            })
            .ok_or(FolderEventError::ResourceLimit)?;
        self.require_index_limit(index_bytes)?;
        Ok(PreparedEvent::Append(PreparedFolderEvent {
            machine_token: Arc::clone(&self.machine_token),
            expected_revision: self.revision,
            change: PreparedChange::Genesis { epoch, index_bytes },
        }))
    }

    fn prepare_operation(
        &self,
        envelope: &EventEnvelope<'_>,
    ) -> Result<PreparedEvent<PreparedFolderEvent>, FolderEventError> {
        let epoch = self
            .current_epoch
            .as_ref()
            .ok_or(FolderEventError::MissingGenesis)?;
        let writer = envelope
            .untrusted_operation_writer_hint()
            .map_err(|_| FolderEventError::InvalidEvent)?;
        let grant = member_by_writer(epoch.roster(), writer)
            .filter(|grant| grant.role() == MemberRole::ReadWrite)
            .ok_or(FolderEventError::UnauthorizedOperation)?;
        let operation = decode_signature_checked_operation(
            envelope.record(),
            self.config.folder_id,
            writer,
            grant.writer_key(),
        )
        .map_err(|_| FolderEventError::InvalidEvent)?;
        if operation.header().membership_epoch() != epoch.epoch()
            || operation.header().membership_epoch_digest() != epoch.digest().to_bytes()
        {
            return Err(FolderEventError::UnauthorizedOperation);
        }
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
            PreparedChange::Genesis { epoch, index_bytes } => {
                if self.current_epoch.is_some() {
                    return Err(FolderEventError::StalePrepared);
                }
                self.current_epoch = Some(epoch);
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
        Ok(self.exact_archive_head(folder, base)?.roster().len())
    }

    fn writer_lifetime(
        &self,
        folder: FolderId,
        base: EpochDigest,
        writer: WriterId,
    ) -> Result<WriterLifetime, MembershipArchiveQueryError> {
        let epoch = self.exact_archive_head(folder, base)?;
        Ok(if member_by_writer(epoch.roster(), writer).is_some() {
            WriterLifetime::Active
        } else {
            WriterLifetime::NeverSeen
        })
    }

    fn writer_key_assignment(
        &self,
        folder: FolderId,
        base: EpochDigest,
        key: &VerifyingKey,
    ) -> Result<HistoricalWriterKeyAssignment, MembershipArchiveQueryError> {
        let epoch = self.exact_archive_head(folder, base)?;
        Ok(epoch
            .roster()
            .iter()
            .find(|grant| grant.writer_key() == key)
            .map_or(HistoricalWriterKeyAssignment::NeverAssigned, |grant| {
                HistoricalWriterKeyAssignment::AssignedTo(grant.writer_id())
            }))
    }
}

impl FolderEventMachine {
    fn exact_archive_head(
        &self,
        folder: FolderId,
        base: EpochDigest,
    ) -> Result<&SignatureCheckedEpoch, MembershipArchiveQueryError> {
        self.current_epoch
            .as_ref()
            .filter(|epoch| folder == self.config.folder_id && epoch.digest() == base)
            .ok_or(MembershipArchiveQueryError::UnknownBaseHead)
    }
}

fn member_by_writer(roster: &[MemberGrant], writer: WriterId) -> Option<&MemberGrant> {
    roster
        .binary_search_by_key(&writer, MemberGrant::writer_id)
        .ok()
        .map(|index| &roster[index])
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
    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::body::OperationBody;
    use crate::sync::event::EventEnvelope;
    use crate::sync::membership::{MemberGrant, encode_signed_epoch};
    use crate::sync::operation::{OperationDigest, encode_signed_operation};

    const GENEROUS_LIMITS: FolderMachineLimits = FolderMachineLimits {
        maximum_index_bytes: 16 * 1_024 * 1_024,
        maximum_operations: 1_000,
        maximum_paths: 1_000,
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
            + fixture.genesis_record.len() as u64;
        let exact_genesis_limits = FolderMachineLimits {
            maximum_index_bytes: genesis_bytes,
            maximum_operations: 1,
            maximum_paths: 1,
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
            PreparedChange::Genesis { .. } => panic!("operation"),
        };
        let mut exact = FolderEventMachine::new(fixture.config(FolderMachineLimits {
            maximum_index_bytes: exact_operation_bytes,
            maximum_operations: 1,
            maximum_paths: 1,
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
    fn unsupported_evidence_kinds_and_later_epoch_never_mutate() {
        let fixture = Fixture::new();
        let mut machine = fixture.machine();
        append(&mut machine, &fixture.genesis_event());
        let revision = machine.revision;
        for (kind, magic) in [
            (EventKind::BootstrapPermit, b"COVSBP01"),
            (EventKind::BootstrapReceipt, b"COVSBR01"),
            (EventKind::WriteLossProposal, b"COVSFP01"),
            (EventKind::FreezeReceipt, b"COVSFR01"),
            (EventKind::FreezeAbort, b"COVSFA01"),
        ] {
            let mut record = magic.to_vec();
            record.extend_from_slice(&1_u16.to_be_bytes());
            record.push(0);
            let event = Fixture::envelope(kind, &record);
            assert_eq!(
                prepare_error(&machine, &event),
                FolderEventError::UnsupportedEvent
            );
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
}
