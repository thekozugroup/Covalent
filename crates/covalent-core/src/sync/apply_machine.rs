//! Replay state for the private create and observation filesystem-apply journal.
//!
//! This machine validates journal structure and transaction ordering. It does
//! not admit folder operations, authorize a writer, or inspect user files. The
//! applier must match every operation binding against the usable durable folder
//! event log and revalidate every recorded filesystem identity.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use super::apply_record::{
    ApplyAction, ApplyApplied, ApplyConflict, ApplyIntent, ApplyRecord, ApplyRecordDigest,
    ApplyRootReady, ApplyStageReady, ApplyTransactionId, ApplyUnsupported, ExpectedTarget,
    OperationBinding,
};
use super::body::EntryValue;
use super::event_log::{EventMachine, EventValidationError, PreparedEvent};
use super::path::SyncPath;
use super::register::OpId;

const MAX_APPLY_RECORDS: u64 = 1_000_000;
// Reserve slack for the first sparsely populated nodes in all five indexes;
// a single inserted value can allocate a complete multi-slot B-tree node.
const BASE_INDEX_CHARGE_BYTES: u64 = 32 * 1_024;
// Covers the retained canonical bytes plus map nodes, keys, phase structs and
// allocator/container slack. It is a conservative logical retained-state cap,
// not a process-RSS measurement.
const RECORD_INDEX_CHARGE_BYTES: u64 = 4 * 1_024;
// Each first Applied path retains a separate key and B-tree entry. Replacing
// its transaction pointer does not allocate another retained path key.
const BASELINE_INDEX_CHARGE_BYTES: u64 = 4 * 1_024;

/// Finite replay-state limits independent of the encrypted file quota.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplyMachineLimits {
    pub maximum_records: u64,
    pub maximum_retained_bytes: u64,
}

/// Fixed replay failures contain no journal, path, or content bytes.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ApplyMachineError {
    #[error("apply machine limits are invalid")]
    InvalidLimits,
    #[error("apply journal record is invalid")]
    InvalidRecord,
    #[error("apply journal transaction is missing an earlier phase")]
    MissingPhase,
    #[error("apply journal transaction order is invalid")]
    InvalidTransition,
    #[error("apply journal has no authenticated root readiness record")]
    NotReady,
    #[error("apply journal contains an equivocation")]
    Equivocation,
    #[error("apply replay state exceeds its configured quota")]
    QuotaExceeded,
    #[error("prepared apply transition belongs to another machine")]
    WrongMachine,
    #[error("prepared apply transition is stale")]
    StalePrepared,
}

/// Terminal result retained for one attempted admitted operation.
pub enum ApplyTerminal<'a> {
    Applied(&'a ApplyApplied),
    Conflict(&'a ApplyConflict),
    Unsupported(&'a ApplyUnsupported),
}

/// The last physical application recorded at a path, in journal commit order.
///
/// This is historical evidence only. User files may have changed afterward,
/// and a newer accepted operation may supersede this value. Callers must
/// revalidate both before using the baseline for status or a mutation.
pub struct HistoricalAppliedBaseline<'a> {
    intent: &'a ApplyIntent,
    applied: &'a ApplyApplied,
}

impl<'a> HistoricalAppliedBaseline<'a> {
    #[must_use]
    pub const fn intent(&self) -> &'a ApplyIntent {
        self.intent
    }

    #[must_use]
    pub const fn applied(&self) -> &'a ApplyApplied {
        self.applied
    }
}

impl fmt::Debug for HistoricalAppliedBaseline<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HistoricalAppliedBaseline")
            .finish_non_exhaustive()
    }
}

/// Replay-derived state for one transaction, before filesystem revalidation.
pub struct ApplyTransaction<'a> {
    intent: &'a ApplyIntent,
    intent_digest: ApplyRecordDigest,
    stage_ready: Option<&'a ApplyStageReady>,
    terminal: Option<ApplyTerminal<'a>>,
}

impl<'a> ApplyTransaction<'a> {
    #[must_use]
    pub const fn intent(&self) -> &'a ApplyIntent {
        self.intent
    }

    #[must_use]
    pub const fn stage_ready(&self) -> Option<&'a ApplyStageReady> {
        self.stage_ready
    }

    #[must_use]
    pub const fn intent_digest(&self) -> ApplyRecordDigest {
        self.intent_digest
    }

    #[must_use]
    pub const fn terminal(&self) -> Option<&ApplyTerminal<'a>> {
        self.terminal.as_ref()
    }
}

struct RetainedTransaction {
    intent: ApplyIntent,
    intent_digest: ApplyRecordDigest,
    stage_ready: Option<ApplyStageReady>,
    terminal: Option<RetainedTerminal>,
}

enum RetainedTerminal {
    Applied(ApplyApplied),
    Conflict(ApplyConflict),
}

enum OperationState {
    Transaction(ApplyTransactionId),
    Conflict(ApplyConflict),
    Unsupported(ApplyUnsupported),
}

enum Transition {
    RootReady(ApplyRootReady),
    Intent(ApplyIntent, ApplyRecordDigest),
    StageReady(ApplyStageReady),
    Applied(ApplyApplied),
    Conflict(ApplyConflict),
    Unsupported(ApplyUnsupported),
}

/// Opaque revision- and instance-bound preflight result.
pub struct PreparedApplyRecord {
    instance: Arc<()>,
    revision: u64,
    encoded: Box<[u8]>,
    digest: ApplyRecordDigest,
    charge: u64,
    transition: Transition,
}

/// Deterministic structural state reconstructed only from committed apply frames.
pub struct ApplyMachine {
    instance: Arc<()>,
    limits: ApplyMachineLimits,
    revision: u64,
    retained_bytes: u64,
    ready: Option<ApplyRootReady>,
    seen: BTreeMap<ApplyRecordDigest, Box<[u8]>>,
    transactions: BTreeMap<ApplyTransactionId, RetainedTransaction>,
    attempts: BTreeMap<ApplyTransactionId, OpId>,
    operations: BTreeMap<OpId, OperationState>,
    latest_applied: BTreeMap<SyncPath, ApplyTransactionId>,
}

impl fmt::Debug for ApplyMachine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplyMachine")
            .field("revision", &self.revision)
            .field("records", &self.seen.len())
            .field("retained_bytes", &self.retained_bytes)
            .finish_non_exhaustive()
    }
}

impl ApplyMachine {
    pub fn new(limits: ApplyMachineLimits) -> Result<Self, ApplyMachineError> {
        if limits.maximum_records == 0
            || limits.maximum_records > MAX_APPLY_RECORDS
            || limits.maximum_retained_bytes < BASE_INDEX_CHARGE_BYTES
        {
            return Err(ApplyMachineError::InvalidLimits);
        }
        Ok(Self {
            instance: Arc::new(()),
            limits,
            revision: 0,
            retained_bytes: BASE_INDEX_CHARGE_BYTES,
            ready: None,
            seen: BTreeMap::new(),
            transactions: BTreeMap::new(),
            attempts: BTreeMap::new(),
            operations: BTreeMap::new(),
            latest_applied: BTreeMap::new(),
        })
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the first-only authenticated coordinator readiness binding.
    #[must_use]
    pub const fn ready(&self) -> Option<&ApplyRootReady> {
        self.ready.as_ref()
    }

    /// Returns the latest durable Applied receipt, never a pending/conflict
    /// outcome and never inferred from the ordering of writer operation IDs.
    #[must_use]
    pub fn last_applied(&self, path: &SyncPath) -> Option<HistoricalAppliedBaseline<'_>> {
        let transaction = self.latest_applied.get(path)?;
        let retained = self.transactions.get(transaction)?;
        let Some(RetainedTerminal::Applied(applied)) = retained.terminal.as_ref() else {
            return None;
        };
        Some(HistoricalAppliedBaseline {
            intent: &retained.intent,
            applied,
        })
    }

    /// Visits historical baselines in canonical path order without collecting
    /// another index. The callback can stop early, including for cancellation.
    pub fn visit_applied_baselines<E>(
        &self,
        mut visit: impl FnMut(HistoricalAppliedBaseline<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        for path in self.latest_applied.keys() {
            visit(
                self.last_applied(path)
                    .expect("indexed applied transaction"),
            )?;
        }
        Ok(())
    }

    pub fn transaction(&self, transaction: ApplyTransactionId) -> Option<ApplyTransaction<'_>> {
        let retained = self.transactions.get(&transaction)?;
        let terminal = match retained.terminal.as_ref() {
            Some(RetainedTerminal::Applied(value)) => Some(ApplyTerminal::Applied(value)),
            Some(RetainedTerminal::Conflict(value)) => Some(ApplyTerminal::Conflict(value)),
            None => None,
        };
        Some(ApplyTransaction {
            intent: &retained.intent,
            intent_digest: retained.intent_digest,
            stage_ready: retained.stage_ready.as_ref(),
            terminal,
        })
    }

    pub fn operation_terminal(&self, operation: OpId) -> Option<ApplyTerminal<'_>> {
        match self.operations.get(&operation)? {
            OperationState::Transaction(transaction) => self.transaction(*transaction)?.terminal,
            OperationState::Conflict(value) => Some(ApplyTerminal::Conflict(value)),
            OperationState::Unsupported(value) => Some(ApplyTerminal::Unsupported(value)),
        }
    }

    /// Finds the existing unfinished attempt without allocating another intent.
    pub fn pending_operation(&self, operation: OpId) -> Option<ApplyTransactionId> {
        let OperationState::Transaction(id) = self.operations.get(&operation)? else {
            return None;
        };
        self.transactions
            .get(id)
            .filter(|transaction| transaction.terminal.is_none())
            .map(|_| *id)
    }

    pub fn pending_transactions(&self) -> impl Iterator<Item = ApplyTransactionId> + '_ {
        self.transactions
            .iter()
            .filter_map(|(id, transaction)| transaction.terminal.is_none().then_some(*id))
    }

    /// Visits each operation identity and its retained path/value without
    /// allocating a second replay index. The caller still proves admission
    /// against the usable durable folder-event log.
    pub fn visit_operations<E>(
        &self,
        mut visit: impl FnMut(
            OperationBinding,
            &super::path::SyncPath,
            Option<EntryValue>,
        ) -> Result<(), E>,
    ) -> Result<(), E> {
        for state in self.operations.values() {
            match state {
                OperationState::Transaction(transaction) => {
                    let retained = self
                        .transactions
                        .get(transaction)
                        .expect("indexed transaction");
                    visit(
                        retained.intent.operation(),
                        retained.intent.path(),
                        Some(retained.intent.desired()),
                    )?;
                }
                OperationState::Conflict(conflict) => {
                    visit(conflict.operation(), conflict.path(), None)?;
                }
                OperationState::Unsupported(unsupported) => {
                    visit(unsupported.operation(), unsupported.path(), None)?;
                }
            }
        }
        Ok(())
    }

    pub fn visit_transactions<E>(
        &self,
        mut visit: impl FnMut(ApplyTransaction<'_>) -> Result<(), E>,
    ) -> Result<(), E> {
        for transaction in self.transactions.keys() {
            visit(self.transaction(*transaction).expect("indexed transaction"))?;
        }
        Ok(())
    }

    pub fn prepare_record(
        &self,
        encoded: &[u8],
    ) -> Result<PreparedEvent<PreparedApplyRecord>, ApplyMachineError> {
        let record = ApplyRecord::decode(encoded).map_err(|_| ApplyMachineError::InvalidRecord)?;
        let digest = record.digest();
        if let Some(prior) = self.seen.get(&digest) {
            return if prior.as_ref() == encoded {
                Ok(PreparedEvent::Duplicate)
            } else {
                Err(ApplyMachineError::Equivocation)
            };
        }
        if self.revision == u64::MAX || self.seen.len() as u64 >= self.limits.maximum_records {
            return Err(ApplyMachineError::QuotaExceeded);
        }
        let encoded_len =
            u64::try_from(encoded.len()).map_err(|_| ApplyMachineError::QuotaExceeded)?;
        let dynamic_charge = owned_dynamic_charge(&record)?;
        let charge = RECORD_INDEX_CHARGE_BYTES
            .checked_add(encoded_len)
            .and_then(|total| total.checked_add(dynamic_charge))
            .and_then(|total| total.checked_add(self.baseline_charge(&record)?))
            .ok_or(ApplyMachineError::QuotaExceeded)?;
        if self
            .retained_bytes
            .checked_add(charge)
            .ok_or(ApplyMachineError::QuotaExceeded)?
            > self.limits.maximum_retained_bytes
        {
            return Err(ApplyMachineError::QuotaExceeded);
        }
        let transition = self.validate_transition(record)?;
        Ok(PreparedEvent::Append(PreparedApplyRecord {
            instance: Arc::clone(&self.instance),
            revision: self.revision,
            encoded: encoded.into(),
            digest,
            charge,
            transition,
        }))
    }

    pub fn commit_record(
        &mut self,
        prepared: PreparedApplyRecord,
    ) -> Result<(), ApplyMachineError> {
        if !Arc::ptr_eq(&self.instance, &prepared.instance) {
            return Err(ApplyMachineError::WrongMachine);
        }
        if self.revision != prepared.revision {
            return Err(ApplyMachineError::StalePrepared);
        }
        match prepared.transition {
            Transition::RootReady(ready) => self.ready = Some(ready),
            Transition::Intent(intent, digest) => {
                self.operations.insert(
                    intent.operation().id(),
                    OperationState::Transaction(intent.transaction()),
                );
                self.attempts
                    .insert(intent.transaction(), intent.operation().id());
                self.transactions.insert(
                    intent.transaction(),
                    RetainedTransaction {
                        intent,
                        intent_digest: digest,
                        stage_ready: None,
                        terminal: None,
                    },
                );
            }
            Transition::StageReady(stage) => {
                let transaction = stage.transaction();
                self.transactions
                    .get_mut(&transaction)
                    .ok_or(ApplyMachineError::MissingPhase)?
                    .stage_ready = Some(stage);
            }
            Transition::Applied(applied) => {
                let transaction = applied.transaction();
                let retained = self
                    .transactions
                    .get_mut(&transaction)
                    .ok_or(ApplyMachineError::MissingPhase)?;
                self.latest_applied
                    .insert(retained.intent.path().clone(), transaction);
                retained.terminal = Some(RetainedTerminal::Applied(applied));
            }
            Transition::Conflict(conflict) => {
                if let Some(intent_digest) = conflict.intent_digest() {
                    let retained = self
                        .transactions
                        .get_mut(&conflict.transaction())
                        .ok_or(ApplyMachineError::MissingPhase)?;
                    if retained.intent_digest != intent_digest {
                        return Err(ApplyMachineError::InvalidTransition);
                    }
                    retained.terminal = Some(RetainedTerminal::Conflict(conflict));
                } else {
                    self.attempts
                        .insert(conflict.transaction(), conflict.operation().id());
                    self.operations.insert(
                        conflict.operation().id(),
                        OperationState::Conflict(conflict),
                    );
                }
            }
            Transition::Unsupported(unsupported) => {
                self.attempts
                    .insert(unsupported.transaction(), unsupported.operation().id());
                self.operations.insert(
                    unsupported.operation().id(),
                    OperationState::Unsupported(unsupported),
                );
            }
        }
        self.seen.insert(prepared.digest, prepared.encoded);
        self.retained_bytes = self
            .retained_bytes
            .checked_add(prepared.charge)
            .ok_or(ApplyMachineError::QuotaExceeded)?;
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(ApplyMachineError::QuotaExceeded)?;
        Ok(())
    }

    fn baseline_charge(&self, record: &ApplyRecord) -> Option<u64> {
        let ApplyRecord::Applied(applied) = record else {
            return Some(0);
        };
        // A missing phase is rejected by transition validation. It cannot
        // allocate a baseline, so it contributes no additional retained charge.
        let Some(retained) = self.transactions.get(&applied.transaction()) else {
            return Some(0);
        };
        if self.latest_applied.contains_key(retained.intent.path()) {
            Some(0)
        } else {
            BASELINE_INDEX_CHARGE_BYTES
                .checked_add(u64::try_from(retained.intent.path().as_str().len()).ok()?)
        }
    }

    fn validate_transition(&self, record: ApplyRecord) -> Result<Transition, ApplyMachineError> {
        if !matches!(&record, ApplyRecord::RootReady(_)) && self.ready.is_none() {
            return Err(ApplyMachineError::NotReady);
        }
        match record {
            ApplyRecord::RootReady(ready) => {
                if self.revision != 0
                    || self.ready.is_some()
                    || !self.transactions.is_empty()
                    || !self.attempts.is_empty()
                    || !self.operations.is_empty()
                    || !self.latest_applied.is_empty()
                {
                    return Err(ApplyMachineError::InvalidTransition);
                }
                Ok(Transition::RootReady(ready))
            }
            ApplyRecord::Intent(intent) => {
                if self.transactions.contains_key(&intent.transaction())
                    || self.attempts.contains_key(&intent.transaction())
                    || self.operations.contains_key(&intent.operation().id())
                    || !intent_shape_valid(&intent)
                {
                    return Err(ApplyMachineError::Equivocation);
                }
                let digest = ApplyRecord::Intent(intent.clone()).digest();
                Ok(Transition::Intent(intent, digest))
            }
            ApplyRecord::StageReady(stage) => {
                let retained = self
                    .transactions
                    .get(&stage.transaction())
                    .ok_or(ApplyMachineError::MissingPhase)?;
                if retained.intent_digest != stage.intent_digest()
                    || retained.intent.operation() != stage.operation()
                    || retained.intent.desired() != stage.desired()
                    || retained.intent.action() != ApplyAction::Create
                    || retained.stage_ready.is_some()
                    || retained.terminal.is_some()
                {
                    return Err(ApplyMachineError::InvalidTransition);
                }
                Ok(Transition::StageReady(stage))
            }
            ApplyRecord::Applied(applied) => {
                let retained = self
                    .transactions
                    .get(&applied.transaction())
                    .ok_or(ApplyMachineError::MissingPhase)?;
                if retained.intent_digest != applied.intent_digest()
                    || retained.intent.operation() != applied.operation()
                    || retained.intent.desired() != applied.desired()
                    || retained.terminal.is_some()
                {
                    return Err(ApplyMachineError::InvalidTransition);
                }
                match retained.intent.action() {
                    ApplyAction::Create => {
                        let stage = retained
                            .stage_ready
                            .as_ref()
                            .ok_or(ApplyMachineError::MissingPhase)?;
                        if stage.stage_identity() != applied.target_identity() {
                            return Err(ApplyMachineError::InvalidTransition);
                        }
                    }
                    ApplyAction::EnsureExisting => {
                        let ExpectedTarget::Directory(expected) = retained.intent.expected_target()
                        else {
                            return Err(ApplyMachineError::InvalidTransition);
                        };
                        if retained.stage_ready.is_some() || expected != applied.target_identity() {
                            return Err(ApplyMachineError::InvalidTransition);
                        }
                    }
                    ApplyAction::AdoptExisting => {
                        let ExpectedTarget::File {
                            identity: expected, ..
                        } = retained.intent.expected_target()
                        else {
                            return Err(ApplyMachineError::InvalidTransition);
                        };
                        if retained.stage_ready.is_some() || expected != applied.target_identity() {
                            return Err(ApplyMachineError::InvalidTransition);
                        }
                    }
                }
                Ok(Transition::Applied(applied))
            }
            ApplyRecord::Conflict(conflict) => {
                if let Some(digest) = conflict.intent_digest() {
                    let retained = self
                        .transactions
                        .get(&conflict.transaction())
                        .ok_or(ApplyMachineError::MissingPhase)?;
                    if retained.intent_digest != digest
                        || retained.intent.operation() != conflict.operation()
                        || retained.intent.path() != conflict.path()
                        || retained.terminal.is_some()
                    {
                        return Err(ApplyMachineError::InvalidTransition);
                    }
                } else if self.operations.contains_key(&conflict.operation().id())
                    || self.attempts.contains_key(&conflict.transaction())
                {
                    return Err(ApplyMachineError::Equivocation);
                }
                Ok(Transition::Conflict(conflict))
            }
            ApplyRecord::Unsupported(unsupported) => {
                if self.operations.contains_key(&unsupported.operation().id())
                    || self.attempts.contains_key(&unsupported.transaction())
                {
                    return Err(ApplyMachineError::Equivocation);
                }
                Ok(Transition::Unsupported(unsupported))
            }
        }
    }
}

fn owned_dynamic_charge(record: &ApplyRecord) -> Result<u64, ApplyMachineError> {
    let bytes = match record {
        ApplyRecord::RootReady(_) => Some(0),
        ApplyRecord::Intent(value) => value
            .path()
            .as_str()
            .len()
            .checked_add(value.stage_name().map_or(0, |stage| stage.as_str().len())),
        ApplyRecord::Conflict(value) => Some(value.path().as_str().len()),
        ApplyRecord::Unsupported(value) => Some(value.path().as_str().len()),
        ApplyRecord::StageReady(_) | ApplyRecord::Applied(_) => Some(0),
    }
    .ok_or(ApplyMachineError::QuotaExceeded)?;
    u64::try_from(bytes).map_err(|_| ApplyMachineError::QuotaExceeded)
}

fn intent_shape_valid(intent: &ApplyIntent) -> bool {
    matches!(
        (
            intent.desired(),
            intent.action(),
            intent.expected_target(),
            intent.stage_name()
        ),
        (
            EntryValue::File(_) | EntryValue::Directory,
            ApplyAction::Create,
            ExpectedTarget::Absent,
            Some(_)
        ) | (
            EntryValue::Directory,
            ApplyAction::EnsureExisting,
            ExpectedTarget::Directory(_),
            None
        ) | (
            EntryValue::File(_),
            ApplyAction::AdoptExisting,
            ExpectedTarget::File { .. },
            None
        )
    )
}

impl EventMachine for ApplyMachine {
    type Prepared = PreparedApplyRecord;

    fn is_empty_for_replay(&self) -> bool {
        self.revision == 0
    }

    fn prepare(&self, event: &[u8]) -> Result<PreparedEvent<Self::Prepared>, EventValidationError> {
        self.prepare_record(event).map_err(|_| EventValidationError)
    }

    fn commit(&mut self, prepared: Self::Prepared) -> Result<(), EventValidationError> {
        self.commit_record(prepared)
            .map_err(|_| EventValidationError)
    }
}

#[cfg(test)]
mod tests {
    use covalent_protocol::DeviceId;
    use ed25519_dalek::SigningKey;
    use uuid::Uuid;

    use super::*;
    use crate::sync::apply_record::{
        ApplyInitialization, ApplyRootReady, ApplyUnsupportedReason, EntryIdentity, StageName,
    };
    use crate::sync::body::{ContentDigest, FileContent, OperationBody};
    use crate::sync::ids::{FolderId, WriterId};
    use crate::sync::operation::{
        ClockEntry, decode_signature_checked_operation, encode_signed_operation,
    };
    use crate::sync::path::SyncPath;

    fn limits(bytes: u64) -> ApplyMachineLimits {
        ApplyMachineLimits {
            maximum_records: 16,
            maximum_retained_bytes: bytes,
        }
    }

    fn operation_at(path: &SyncPath, counter: u64) -> OperationBinding {
        operation_with_writer(path, counter, Uuid::from_u128(0xb1), EntryValue::Directory)
    }

    fn operation_with_writer(
        path: &SyncPath,
        counter: u64,
        writer_uuid: Uuid,
        desired: EntryValue,
    ) -> OperationBinding {
        let writer = WriterId::from_uuid(writer_uuid);
        let key = SigningKey::from_bytes(&[0xb2; 32]);
        let body = OperationBody::new(path.clone(), desired).encode();
        let record = encode_signed_operation(
            &key,
            FolderId::from_uuid(Uuid::from_u128(0xb3)),
            writer,
            1,
            [0xb4; 32],
            counter,
            (counter > 1).then_some([0xb5; 32]),
            &[ClockEntry::new(writer, counter).unwrap()],
            &body,
        )
        .unwrap();
        let checked = decode_signature_checked_operation(
            &record,
            FolderId::from_uuid(Uuid::from_u128(0xb3)),
            writer,
            &key.verifying_key(),
        )
        .unwrap();
        OperationBinding::new(
            OpId::new(DeviceId::from_uuid(writer_uuid), counter).unwrap(),
            checked.digest(),
        )
    }

    fn intent_at(transaction: ApplyTransactionId, path: SyncPath) -> ApplyRecord {
        ApplyRecord::Intent(
            ApplyIntent::new(
                transaction,
                operation_at(&path, 1),
                EntryIdentity::new(1, 2),
                path,
                EntryValue::Directory,
                ApplyAction::Create,
                ExpectedTarget::Absent,
                Some(StageName::from_random_bytes([3; 16])),
            )
            .unwrap(),
        )
    }

    fn intent(transaction: ApplyTransactionId) -> ApplyRecord {
        intent_at(transaction, SyncPath::from_wire("machine-canary").unwrap())
    }

    fn ready_record() -> ApplyRecord {
        ApplyRecord::RootReady(ApplyRootReady::from_test_parts(
            EntryIdentity::new(91, 92),
            [93; 32],
            ApplyInitialization::OwnerGenesis(super::super::membership::EpochDigest::from_bytes(
                [94; 32],
            )),
        ))
    }

    fn ready_machine(bytes: u64) -> ApplyMachine {
        let mut machine = ApplyMachine::new(limits(bytes)).unwrap();
        let encoded = ready_record().encode();
        let PreparedEvent::Append(prepared) = machine.prepare_record(encoded.as_bytes()).unwrap()
        else {
            panic!("ready must append")
        };
        machine.commit_record(prepared).unwrap();
        machine
    }

    fn adoption_records(path: &SyncPath, tag: u8, writer: u128, mode: u16) -> [ApplyRecord; 2] {
        let desired = EntryValue::File(
            FileContent::new(ContentDigest::from_bytes([tag; 32]), u64::from(tag), false).unwrap(),
        );
        let operation = operation_with_writer(path, 1, Uuid::from_u128(writer), desired);
        let transaction = ApplyTransactionId::from_bytes([tag; 32]).unwrap();
        let identity = EntryIdentity::new(1, u64::from(tag));
        let intent = ApplyRecord::Intent(
            ApplyIntent::new(
                transaction,
                operation,
                EntryIdentity::new(91, 92),
                path.clone(),
                desired,
                ApplyAction::AdoptExisting,
                ExpectedTarget::File {
                    identity,
                    permission_bits: mode,
                },
                None,
            )
            .unwrap(),
        );
        let applied = ApplyRecord::Applied(
            ApplyApplied::new(transaction, intent.digest(), operation, identity, desired).unwrap(),
        );
        [intent, applied]
    }

    fn commit(machine: &mut ApplyMachine, record: &ApplyRecord) {
        let PreparedEvent::Append(prepared) =
            machine.prepare_record(record.encode().as_bytes()).unwrap()
        else {
            panic!("new record must append")
        };
        machine.commit_record(prepared).unwrap();
    }

    #[test]
    fn applied_baseline_uses_durable_commit_order_across_writers_and_reopen() {
        use crate::sync::event_log::{DurableEventLog, EventAppendOutcome, EventLogLimits};
        use crate::sync::log_frame::{LogBinding, LogFileKind, LogFrameKey};
        use crate::sync::state_dir::{PrivateStateDir, StateKey};
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let directory = PrivateStateDir::open_root(temp.path()).unwrap();
        let name = StateKey::new("baseline.v1").unwrap();
        let binding = LogBinding::new(
            FolderId::from_uuid(Uuid::from_u128(301)),
            Uuid::from_u128(302),
            Uuid::from_u128(303),
            LogFileKind::Apply,
        );
        let log_limits = EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records: 16,
        };
        let mut log = DurableEventLog::create(
            &directory,
            &name,
            binding,
            LogFrameKey::from_bytes([44; 32]),
            log_limits,
            ApplyMachine::new(limits(1 << 20)).unwrap(),
        )
        .unwrap();
        log.append(ready_record().encode().as_bytes()).unwrap();
        let path = SyncPath::from_wire("baseline-name-canary").unwrap();
        let first = adoption_records(&path, 41, 0x900, 0o640);
        let second = adoption_records(&path, 42, 0x100, 0o444);
        for record in &first {
            log.append(record.encode().as_bytes()).unwrap();
        }
        let first_id = log
            .machine()
            .unwrap()
            .last_applied(&path)
            .unwrap()
            .applied()
            .operation()
            .id();
        log.append(second[0].encode().as_bytes()).unwrap();
        assert_eq!(
            log.machine()
                .unwrap()
                .last_applied(&path)
                .unwrap()
                .applied()
                .operation()
                .id(),
            first_id
        );
        log.append(second[1].encode().as_bytes()).unwrap();
        let baseline = log.machine().unwrap().last_applied(&path).unwrap();
        let second_id = baseline.applied().operation().id();
        assert!(
            second_id < first_id,
            "OpId ordering must differ from commit ordering"
        );
        assert_eq!(
            baseline.intent().expected_target(),
            ExpectedTarget::File {
                identity: EntryIdentity::new(1, 42),
                permission_bits: 0o444,
            }
        );
        assert_eq!(
            baseline.applied().target_identity(),
            EntryIdentity::new(1, 42)
        );
        assert!(!format!("{baseline:?}").contains("baseline-name-canary"));
        assert_eq!(
            log.append(first[1].encode().as_bytes()).unwrap(),
            EventAppendOutcome::Duplicate
        );
        let before = log.committed_records().unwrap();
        drop(log);
        let reopened = DurableEventLog::open(
            &directory,
            &name,
            binding,
            LogFrameKey::from_bytes([44; 32]),
            log_limits,
            ApplyMachine::new(limits(1 << 20)).unwrap(),
        )
        .unwrap();
        assert_eq!(reopened.committed_records().unwrap(), before);
        let machine = reopened.machine().unwrap();
        assert_eq!(
            machine
                .last_applied(&path)
                .unwrap()
                .applied()
                .operation()
                .id(),
            second_id
        );
        assert!(
            machine.operation_terminal(first_id).is_some(),
            "older evidence is retained"
        );
        let mut visited = 0;
        machine
            .visit_applied_baselines::<()>(|baseline| {
                visited += 1;
                assert_eq!(baseline.intent().path(), &path);
                assert_eq!(baseline.applied().operation().id(), second_id);
                Ok(())
            })
            .unwrap();
        assert_eq!(visited, 1);
        assert_eq!(
            machine.visit_applied_baselines(|_| Err("stop")),
            Err("stop")
        );
    }

    #[test]
    fn created_and_ensured_directory_baselines_visit_each_path_in_canonical_order() {
        let later = SyncPath::from_wire("z-directory").unwrap();
        let earlier = SyncPath::from_wire("a-directory").unwrap();
        let mut machine = ready_machine(1 << 20);
        let create = intent_at(
            ApplyTransactionId::from_bytes([55; 32]).unwrap(),
            later.clone(),
        );
        let ApplyRecord::Intent(intent) = &create else {
            unreachable!()
        };
        let identity = EntryIdentity::new(1, 55);
        let stage = ApplyRecord::StageReady(
            ApplyStageReady::new(
                intent.transaction(),
                create.digest(),
                intent.operation(),
                identity,
                EntryValue::Directory,
            )
            .unwrap(),
        );
        let applied = ApplyRecord::Applied(
            ApplyApplied::new(
                intent.transaction(),
                create.digest(),
                intent.operation(),
                identity,
                EntryValue::Directory,
            )
            .unwrap(),
        );
        commit(&mut machine, &create);
        commit(&mut machine, &stage);
        assert!(machine.last_applied(&later).is_none());
        commit(&mut machine, &applied);
        assert_eq!(
            machine.last_applied(&later).unwrap().intent().action(),
            ApplyAction::Create
        );

        for (path, tag) in [(&earlier, 56_u8), (&later, 57_u8)] {
            let transaction = ApplyTransactionId::from_bytes([tag; 32]).unwrap();
            let operation = operation_with_writer(
                path,
                1,
                Uuid::from_u128(u128::from(tag)),
                EntryValue::Directory,
            );
            let identity = EntryIdentity::new(1, u64::from(tag));
            let ensure = ApplyRecord::Intent(
                ApplyIntent::new(
                    transaction,
                    operation,
                    EntryIdentity::new(91, 92),
                    path.clone(),
                    EntryValue::Directory,
                    ApplyAction::EnsureExisting,
                    ExpectedTarget::Directory(identity),
                    None,
                )
                .unwrap(),
            );
            let applied = ApplyRecord::Applied(
                ApplyApplied::new(
                    transaction,
                    ensure.digest(),
                    operation,
                    identity,
                    EntryValue::Directory,
                )
                .unwrap(),
            );
            commit(&mut machine, &ensure);
            commit(&mut machine, &applied);
        }
        assert_eq!(machine.latest_applied.len(), 2);
        let mut paths = Vec::new();
        machine
            .visit_applied_baselines::<()>(|baseline| {
                assert_eq!(baseline.intent().action(), ApplyAction::EnsureExisting);
                assert_eq!(baseline.applied().desired(), EntryValue::Directory);
                paths.push((
                    baseline.intent().path().clone(),
                    baseline.applied().target_identity().inode(),
                ));
                Ok(())
            })
            .unwrap();
        assert_eq!(paths, vec![(earlier, 56), (later, 57)]);
    }

    #[test]
    fn applied_baseline_reserves_new_path_charge_before_commit() {
        let path = SyncPath::from_wire(vec!["x".repeat(250); 16].join("/")).unwrap();
        let records = adoption_records(&path, 45, 0x110, 0o644);
        let mut machine = ready_machine(1 << 20);
        commit(&mut machine, &records[0]);
        let encoded = records[1].encode();
        let extra = BASELINE_INDEX_CHARGE_BYTES + path.as_str().len() as u64;
        let exact = machine.retained_bytes
            + RECORD_INDEX_CHARGE_BYTES
            + encoded.as_bytes().len() as u64
            + extra;
        machine.limits.maximum_retained_bytes = exact - 1;
        let revision = machine.revision();
        assert_eq!(
            machine.prepare_record(encoded.as_bytes()).err(),
            Some(ApplyMachineError::QuotaExceeded)
        );
        assert_eq!(machine.revision(), revision);
        assert!(machine.last_applied(&path).is_none());
        machine.limits.maximum_retained_bytes = exact;
        commit(&mut machine, &records[1]);
        assert_eq!(machine.retained_bytes, exact);

        let next = adoption_records(&path, 46, 0x120, 0o644);
        machine.limits.maximum_retained_bytes = 1 << 20;
        commit(&mut machine, &next[0]);
        assert_eq!(machine.baseline_charge(&next[1]), Some(0));
        machine.limits.maximum_retained_bytes = machine.retained_bytes
            + RECORD_INDEX_CHARGE_BYTES
            + next[1].encode().as_bytes().len() as u64;
        commit(&mut machine, &next[1]);
        assert_eq!(machine.latest_applied.len(), 1);
        assert_eq!(
            machine.retained_bytes,
            machine.limits.maximum_retained_bytes
        );
    }

    #[test]
    fn rejected_stale_and_conflict_records_cannot_advance_applied_baseline() {
        use crate::sync::apply_record::ApplyConflictReason;
        let path = SyncPath::from_wire("preserve-last-applied").unwrap();
        let first = adoption_records(&path, 47, 0x140, 0o644);
        let next = adoption_records(&path, 48, 0x150, 0o644);
        let mut machine = ready_machine(1 << 20);
        for record in &first {
            commit(&mut machine, record);
        }
        let expected = machine.last_applied(&path).unwrap().applied().operation();
        commit(&mut machine, &next[0]);
        let PreparedEvent::Append(stale) =
            machine.prepare_record(next[1].encode().as_bytes()).unwrap()
        else {
            panic!("applied must prepare")
        };
        let ApplyRecord::Intent(intent) = &next[0] else {
            unreachable!()
        };
        let conflict = ApplyRecord::Conflict(ApplyConflict::new(
            intent.transaction(),
            Some(next[0].digest()),
            intent.operation(),
            path.clone(),
            ApplyConflictReason::TargetChanged,
            Some(EntryIdentity::new(1, 99)),
        ));
        commit(&mut machine, &conflict);
        assert_eq!(
            machine.commit_record(stale),
            Err(ApplyMachineError::StalePrepared)
        );
        assert_eq!(
            machine.prepare_record(next[1].encode().as_bytes()).err(),
            Some(ApplyMachineError::InvalidTransition)
        );
        assert_eq!(
            machine.last_applied(&path).unwrap().applied().operation(),
            expected
        );
        assert!(matches!(
            machine.operation_terminal(intent.operation().id()),
            Some(ApplyTerminal::Conflict(_))
        ));
        assert_eq!(machine.latest_applied.len(), 1);
    }

    #[test]
    fn readiness_is_first_only_and_operations_cannot_precede_it() {
        let transaction = ApplyTransactionId::from_bytes([31; 32]).unwrap();
        let intent = intent(transaction).encode();
        let mut machine = ApplyMachine::new(limits(1 << 20)).unwrap();
        assert_eq!(
            machine.prepare_record(intent.as_bytes()).err(),
            Some(ApplyMachineError::NotReady)
        );
        assert_eq!(machine.revision(), 0);

        let ready = ready_record().encode();
        let PreparedEvent::Append(prepared) = machine.prepare_record(ready.as_bytes()).unwrap()
        else {
            panic!("ready must append")
        };
        machine.commit_record(prepared).unwrap();
        assert!(machine.ready().is_some());
        assert!(matches!(
            machine.prepare_record(ready.as_bytes()),
            Ok(PreparedEvent::Duplicate)
        ));
        let different = ApplyRecord::RootReady(ApplyRootReady::from_test_parts(
            EntryIdentity::new(91, 95),
            [93; 32],
            ApplyInitialization::AwaitingBootstrap,
        ))
        .encode();
        assert_eq!(
            machine.prepare_record(different.as_bytes()).err(),
            Some(ApplyMachineError::InvalidTransition)
        );
        assert_eq!(machine.revision(), 1);
    }

    #[test]
    fn prepared_values_are_exact_instance_and_revision_bound() {
        let transaction = ApplyTransactionId::from_bytes([1; 32]).unwrap();
        let record = intent(transaction).encode();
        let mut first = ready_machine(1 << 20);
        let mut second = ready_machine(1 << 20);
        let PreparedEvent::Append(prepared) = first.prepare_record(record.as_bytes()).unwrap()
        else {
            panic!("new intent must append")
        };
        assert_eq!(
            second.commit_record(prepared),
            Err(ApplyMachineError::WrongMachine)
        );
        assert_eq!(second.revision(), 1);

        let PreparedEvent::Append(stale) = first.prepare_record(record.as_bytes()).unwrap() else {
            panic!("new intent must append")
        };
        let PreparedEvent::Append(current) = first.prepare_record(record.as_bytes()).unwrap()
        else {
            panic!("new intent must append")
        };
        first.commit_record(current).unwrap();
        assert_eq!(
            first.commit_record(stale),
            Err(ApplyMachineError::StalePrepared)
        );
        assert_eq!(first.revision(), 2);
    }

    #[test]
    fn quota_and_phase_rejection_leave_state_unchanged() {
        let transaction = ApplyTransactionId::from_bytes([4; 32]).unwrap();
        let long_path = SyncPath::from_wire(vec!["x".repeat(250); 16].join("/")).unwrap();
        let record = intent_at(transaction, long_path);
        let encoded = record.encode();
        let ready = ready_record();
        let ready_encoded = ready.encode();
        let exact_charge = BASE_INDEX_CHARGE_BYTES
            + RECORD_INDEX_CHARGE_BYTES
            + ready_encoded.as_bytes().len() as u64
            + RECORD_INDEX_CHARGE_BYTES
            + encoded.as_bytes().len() as u64
            + owned_dynamic_charge(&record).unwrap();
        let below = ready_machine(exact_charge - 1);
        assert!(matches!(
            below.prepare_record(encoded.as_bytes()),
            Err(ApplyMachineError::QuotaExceeded)
        ));
        assert_eq!(below.revision(), 1);

        let exact = ready_machine(exact_charge);
        assert!(matches!(
            exact.prepare_record(encoded.as_bytes()),
            Ok(PreparedEvent::Append(_))
        ));

        let ApplyRecord::Intent(intent) = record else {
            unreachable!()
        };
        let stage = ApplyRecord::StageReady(
            ApplyStageReady::new(
                transaction,
                ApplyRecordDigest::from_bytes([9; 32]),
                intent.operation(),
                EntryIdentity::new(5, 6),
                intent.desired(),
            )
            .unwrap(),
        )
        .encode();
        let machine = ready_machine(1 << 20);
        assert!(matches!(
            machine.prepare_record(stage.as_bytes()),
            Err(ApplyMachineError::MissingPhase)
        ));
        assert_eq!(machine.revision(), 1);
    }

    #[test]
    fn transaction_identity_cannot_be_reused_across_record_families() {
        let transaction = ApplyTransactionId::from_bytes([7; 32]).unwrap();
        let path = SyncPath::from_wire("unsupported-one").unwrap();
        let first = ApplyRecord::Unsupported(ApplyUnsupported::new(
            transaction,
            operation_at(&path, 1),
            path,
            ApplyUnsupportedReason::Replace,
        ))
        .encode();
        let mut machine = ready_machine(1 << 20);
        let PreparedEvent::Append(prepared) = machine.prepare_record(first.as_bytes()).unwrap()
        else {
            panic!("new unsupported record must append")
        };
        machine.commit_record(prepared).unwrap();

        let path = SyncPath::from_wire("unsupported-two").unwrap();
        let second = ApplyRecord::Unsupported(ApplyUnsupported::new(
            transaction,
            operation_at(&path, 2),
            path,
            ApplyUnsupportedReason::Replace,
        ))
        .encode();
        assert_eq!(
            machine.prepare_record(second.as_bytes()).err(),
            Some(ApplyMachineError::Equivocation)
        );
        assert_eq!(machine.revision(), 2);
    }

    #[test]
    fn adoption_requires_exact_incumbent_identity_and_no_stage() {
        let transaction = ApplyTransactionId::from_bytes([8; 32]).unwrap();
        let path = SyncPath::from_wire("adopted-file").unwrap();
        let operation = operation_at(&path, 1);
        let desired = EntryValue::File(
            FileContent::new(ContentDigest::from_bytes([9; 32]), 12, false).unwrap(),
        );
        let identity = EntryIdentity::new(10, 11);
        let intent = ApplyRecord::Intent(
            ApplyIntent::new(
                transaction,
                operation,
                EntryIdentity::new(1, 2),
                path,
                desired,
                ApplyAction::AdoptExisting,
                ExpectedTarget::File {
                    identity,
                    permission_bits: 0o644,
                },
                None,
            )
            .unwrap(),
        );
        let intent_digest = intent.digest();
        let mut machine = ready_machine(1 << 20);
        let PreparedEvent::Append(prepared) =
            machine.prepare_record(intent.encode().as_bytes()).unwrap()
        else {
            panic!("adoption intent must append")
        };
        machine.commit_record(prepared).unwrap();

        let wrong = ApplyRecord::Applied(
            ApplyApplied::new(
                transaction,
                intent_digest,
                operation,
                EntryIdentity::new(10, 12),
                desired,
            )
            .unwrap(),
        )
        .encode();
        assert_eq!(
            machine.prepare_record(wrong.as_bytes()).err(),
            Some(ApplyMachineError::InvalidTransition)
        );
        assert_eq!(machine.revision(), 2);

        let applied = ApplyRecord::Applied(
            ApplyApplied::new(transaction, intent_digest, operation, identity, desired).unwrap(),
        )
        .encode();
        let PreparedEvent::Append(prepared) = machine.prepare_record(applied.as_bytes()).unwrap()
        else {
            panic!("matching adoption receipt must append")
        };
        machine.commit_record(prepared).unwrap();
        assert_eq!(machine.revision(), 3);
        assert!(matches!(
            machine.operation_terminal(operation.id()),
            Some(ApplyTerminal::Applied(_))
        ));
    }
}
