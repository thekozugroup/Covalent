//! Descriptor-relative create and observation application of admitted folder operations.
//!
//! Every user-folder mutation follows a committed encrypted apply intent. This
//! slice creates absent files and directories or durably verifies an
//! existing file or directory. It never replaces, removes, chmods, or rewrites
//! an incumbent during adoption.

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::Arc;

use rand_core::{OsRng, RngCore as _};
use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, RawMode, RenameFlags, fchmod, fstat, fsync, mkdirat,
    open, openat, renameat_with, statat,
};
use thiserror::Error;

use crate::AuthorizedRoot;
use crate::engine::JobControl;

use super::apply_machine::{ApplyMachine, ApplyMachineError, ApplyMachineLimits, ApplyTerminal};
use super::apply_record::{
    ApplyAction, ApplyApplied, ApplyConflict, ApplyConflictReason as ConflictReason, ApplyIntent,
    ApplyRecord, ApplyRecordError, ApplyStageReady, ApplyTransactionId, ApplyUnsupported,
    ApplyUnsupportedReason as UnsupportedReason, EntryIdentity, ExpectedTarget, OperationBinding,
    StageName,
};
use super::body::{EntryValue, FileContent, OperationBody};
use super::content_store::{ContentStoreError, SyncContentStore};
use super::event_log::{DurableEventLog, EventLogError, EventLogLimits};
use super::log_frame::{LogBinding, LogFileKind, LogFrameKey};
use super::path::SyncPath;
use super::publication::{DurableFolderLog, PublicationError};
use super::register::OpId;
use super::state_dir::{PrivateStateDir, PrivateStateLock, StateDirError, StateKey};

const FILE_MODE: RawMode = 0o600;
const EXECUTABLE_FILE_MODE: RawMode = 0o700;
const DIRECTORY_MODE: RawMode = 0o700;
const STREAM_BUFFER_BYTES: usize = 64 * 1_024;
const MAX_DIRECTORY_ENTRIES: usize = 1_000_000;
const MAX_DIRECTORY_NAME_BYTES: u64 = 256 * 1_024 * 1_024;
const MAX_REVALIDATION_BYTES: u64 = 1_u64 << 40;

/// Bounded target inspection and replay limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApplyLimits {
    pub machine: ApplyMachineLimits,
    pub maximum_directory_entries: usize,
    pub maximum_directory_name_bytes: u64,
    pub maximum_revalidation_bytes: u64,
}

/// A durable create or adoption attempt's current visible state.
///
/// Adoption proves local filesystem state only. It does not promise that the
/// installation has retained content that it can serve to another member.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    Applied,
    Conflict,
    Unsupported,
    Pending,
}

/// Fixed apply errors retain no path, key, content, or server-controlled text.
#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("apply configuration is invalid")]
    InvalidConfiguration,
    #[error("folder operation is not admitted by the durable event log")]
    UnadmittedOperation,
    #[error("folder operation is not the sole active value for its path")]
    ProjectionConflict,
    #[error("apply target has a portable-name collision")]
    NameCollision,
    #[error("apply target has a missing or unsafe parent")]
    UnsafeParent,
    #[error("apply target or stage changed during the transaction")]
    Changed,
    #[error("apply target inspection exceeded its configured bound")]
    ResourceLimit,
    #[error("apply transaction is pending explicit reconciliation")]
    Pending,
    #[error("apply operation was interrupted")]
    Interrupted,
    #[error("apply filesystem operation failed during {operation} (errno {errno:?})")]
    Io {
        operation: &'static str,
        errno: Option<i32>,
    },
    #[error(transparent)]
    State(#[from] StateDirError),
    #[error(transparent)]
    Log(#[from] EventLogError),
    #[error(transparent)]
    Machine(#[from] ApplyMachineError),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Content(#[from] ContentStoreError),
    #[error(transparent)]
    Record(#[from] ApplyRecordError),
}

struct UserRoot {
    canonical: PathBuf,
    descriptor: OwnedFd,
    identity: EntryIdentity,
}

struct TargetParent {
    descriptor: OwnedFd,
    identity: EntryIdentity,
    name: String,
}

#[derive(Clone, Copy)]
struct ObservedFile {
    identity: EntryIdentity,
    permission_bits: u16,
}

struct OpenedAdoptionFile {
    file: File,
    before: rustix::fs::Stat,
    observed: ObservedFile,
}

struct OpenedAdoptionDirectory {
    descriptor: OwnedFd,
    identity: EntryIdentity,
}

type AdoptionIoHook = (AdoptionIoPoint, Box<dyn FnOnce() -> Result<(), ApplyError>>);

#[derive(Default)]
struct AdoptionVerifyHooks {
    after_sync: Option<Box<dyn FnOnce(EntryIdentity)>>,
    io: Option<AdoptionIoHook>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AdoptionIoPoint {
    ReadFile,
    SyncFile,
    InspectFile,
    SyncDirectory,
    InspectDirectory,
    SyncParent,
}

#[derive(Clone, Copy)]
enum FileModeRequirement {
    Staged,
    Adoptable,
    Exact(u16),
}

/// A lifetime-locked private apply journal and anchored authorized user root.
pub struct DurableFolderApplier {
    log: DurableEventLog<ApplyMachine>,
    binding: LogBinding,
    limits: ApplyLimits,
    poisoned: bool,
    failpoint: Option<ApplyFailpoint>,
    #[cfg(test)]
    mutation_hook: Option<(ApplyMutationPoint, Box<dyn FnOnce()>)>,
    #[cfg(test)]
    adoption_sync_hook: Option<Box<dyn FnOnce(EntryIdentity)>>,
    #[cfg(test)]
    adoption_io_hook: Option<AdoptionIoHook>,
    root: UserRoot,
    outer: Arc<PrivateStateDir>,
    outer_lock: Arc<PrivateStateLock>,
}

impl std::fmt::Debug for DurableFolderApplier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DurableFolderApplier")
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl DurableFolderApplier {
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        outer: Arc<PrivateStateDir>,
        outer_lock: Arc<PrivateStateLock>,
        apply_dir: &StateKey,
        log_file: &StateKey,
        binding: LogBinding,
        key: LogFrameKey,
        log_limits: EventLogLimits,
        limits: ApplyLimits,
        root: &AuthorizedRoot,
        events: &DurableFolderLog,
    ) -> Result<Self, ApplyError> {
        validate_configuration(&outer, &outer_lock, binding, limits, root, events)?;
        let user_root = UserRoot::open(root)?;
        let directory = outer.open_or_create_child(apply_dir)?;
        let machine = ApplyMachine::new(limits.machine)?;
        let log = DurableEventLog::create(&directory, log_file, binding, key, log_limits, machine)?;
        Ok(Self {
            outer,
            outer_lock,
            root: user_root,
            log,
            binding,
            limits,
            poisoned: false,
            failpoint: None,
            #[cfg(test)]
            mutation_hook: None,
            #[cfg(test)]
            adoption_sync_hook: None,
            #[cfg(test)]
            adoption_io_hook: None,
        })
    }

    /// Replays the private journal with pause/cancel checks between bounded
    /// frames, then checks every apply-specific history/filesystem visit. A
    /// started valid incomplete-tail repair finishes its sync before stopping;
    /// individual filesystem calls and state transitions are not preempted.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        outer: Arc<PrivateStateDir>,
        outer_lock: Arc<PrivateStateLock>,
        apply_dir: &StateKey,
        log_file: &StateKey,
        binding: LogBinding,
        key: LogFrameKey,
        log_limits: EventLogLimits,
        limits: ApplyLimits,
        root: &AuthorizedRoot,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<Self, ApplyError> {
        check_control(control)?;
        validate_configuration(&outer, &outer_lock, binding, limits, root, events)?;
        let user_root = UserRoot::open(root)?;
        let directory = outer.open_child(apply_dir)?;
        let machine = ApplyMachine::new(limits.machine)?;
        let log = DurableEventLog::open_with_control(
            &directory, log_file, binding, key, log_limits, machine, control,
        )?;
        let applier = Self {
            outer,
            outer_lock,
            root: user_root,
            log,
            binding,
            limits,
            poisoned: false,
            failpoint: None,
            #[cfg(test)]
            mutation_hook: None,
            #[cfg(test)]
            adoption_sync_hook: None,
            #[cfg(test)]
            adoption_io_hook: None,
        };
        applier.validate_replayed_history(events, control)?;
        applier.validate_replayed_filesystem(events, control)?;
        Ok(applier)
    }

    pub fn apply(
        &mut self,
        events: &DurableFolderLog,
        store: &SyncContentStore,
        id: OpId,
        exact_event: &[u8],
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        check_control(control)?;
        self.require_usable(events, control)?;
        let accepted = events
            .machine()?
            .accepted_operation(id, exact_event)
            .map_err(|_| ApplyError::UnadmittedOperation)?;
        let body = accepted.body();
        require_active(events, id, body)?;
        if let Some(outcome) = self.existing_outcome(id)? {
            return Ok(outcome);
        }
        if let Some(transaction) = self.log.machine()?.pending_operation(id) {
            return self.reconcile_transaction(transaction, events, store, control);
        }
        let operation = OperationBinding::new(id, accepted.digest());
        match body.value() {
            EntryValue::Tombstone => {
                let unsupported = ApplyUnsupported::new(
                    ApplyTransactionId::random()?,
                    operation,
                    body.path().clone(),
                    UnsupportedReason::Tombstone,
                );
                self.append(ApplyRecord::Unsupported(unsupported))?;
                Ok(ApplyOutcome::Unsupported)
            }
            EntryValue::File(content) => {
                let receipt = store.verify_retained(content, control)?;
                if receipt.domain().folder_id() != self.binding.folder_id()
                    || receipt.domain().installation_id() != self.binding.installation_id()
                    || receipt.domain().generation_id() != self.binding.generation_id()
                {
                    return Err(ApplyError::InvalidConfiguration);
                }
                self.begin_create(operation, body, Some(content), events, store, control)
            }
            EntryValue::Directory => {
                self.begin_create(operation, body, None, events, store, control)
            }
        }
    }

    /// Durably adopts a matching incumbent file or directory without creating,
    /// replacing, deleting, chmodding, or otherwise rewriting its contents.
    pub fn adopt_existing(
        &mut self,
        events: &DurableFolderLog,
        id: OpId,
        exact_event: &[u8],
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        check_control(control)?;
        self.require_usable(events, control)?;
        let accepted = events
            .machine()?
            .accepted_operation(id, exact_event)
            .map_err(|_| ApplyError::UnadmittedOperation)?;
        let body = accepted.body();
        require_active(events, id, body)?;
        if let Some(outcome) = self.existing_outcome(id)? {
            return Ok(outcome);
        }
        if let Some(transaction) = self.log.machine()?.pending_operation(id) {
            let action = self
                .log
                .machine()?
                .transaction(transaction)
                .ok_or(ApplyError::Changed)?
                .intent()
                .action();
            return match action {
                ApplyAction::EnsureExisting | ApplyAction::AdoptExisting => {
                    self.finish_existing(transaction, events, control)
                }
                ApplyAction::Create => Err(ApplyError::Pending),
            };
        }
        let operation = OperationBinding::new(id, accepted.digest());
        match body.value() {
            EntryValue::File(content) => {
                self.begin_adopt_file(operation, body, content, events, control)
            }
            EntryValue::Directory => self.begin_ensure_directory(operation, body, events, control),
            EntryValue::Tombstone => Err(ApplyError::ProjectionConflict),
        }
    }

    /// Revalidates every incomplete journal transaction and completes only
    /// stages whose exact identity was durably recorded by `StageReady`.
    pub fn reconcile(
        &mut self,
        events: &DurableFolderLog,
        store: &SyncContentStore,
        control: &JobControl,
    ) -> Result<Vec<ApplyOutcome>, ApplyError> {
        check_control(control)?;
        self.require_usable(events, control)?;
        let mut pending = Vec::new();
        for transaction in self.log.machine()?.pending_transactions() {
            pending
                .try_reserve(1)
                .map_err(|_| ApplyError::ResourceLimit)?;
            pending.push(transaction);
        }
        let mut outcomes = Vec::new();
        outcomes
            .try_reserve_exact(pending.len())
            .map_err(|_| ApplyError::ResourceLimit)?;
        for transaction in pending {
            check_control(control)?;
            outcomes.push(self.reconcile_transaction(transaction, events, store, control)?);
        }
        Ok(outcomes)
    }

    fn begin_create(
        &mut self,
        operation: OperationBinding,
        body: &OperationBody,
        file: Option<FileContent>,
        events: &DurableFolderLog,
        store: &SyncContentStore,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        let parent = self.root.open_parent(body.path(), self.limits, control)?;
        parent.reject_name_collision(body.path(), self.limits, control)?;
        let target = parent.target_identity()?;
        if let Some(identity) = target {
            if body.value() == EntryValue::Directory && parent.target_is_directory(identity)? {
                return self.begin_ensure_directory(operation, body, events, control);
            }
            return self.record_standalone_conflict(
                operation,
                body.path(),
                ConflictReason::TargetExists,
                Some(identity),
            );
        }

        let transaction = ApplyTransactionId::random()?;
        let mut random = [0_u8; 16];
        OsRng
            .try_fill_bytes(&mut random)
            .map_err(|_| ApplyError::ResourceLimit)?;
        let stage_name = StageName::from_random_bytes(random);
        let intent = ApplyIntent::new(
            transaction,
            operation,
            self.root.identity,
            body.path().clone(),
            body.value(),
            ApplyAction::Create,
            ExpectedTarget::Absent,
            Some(stage_name.clone()),
        )?;
        let intent_digest = self.append(ApplyRecord::Intent(intent))?;
        self.fail_if(ApplyFailpoint::IntentCommitted)?;
        self.run_mutation_hook(ApplyMutationPoint::BeforeStageCreate);
        self.root
            .revalidate_parent(body.path(), parent.identity, self.limits, control)?;
        let stage_identity = match file {
            Some(content) => parent.create_file_stage(&stage_name, content, store, control),
            None => parent.create_directory_stage(&stage_name, control),
        }?;
        self.fail_if(ApplyFailpoint::StageDurable)?;
        let stage = ApplyStageReady::new(
            transaction,
            intent_digest,
            operation,
            stage_identity,
            body.value(),
        )?;
        self.append(ApplyRecord::StageReady(stage))?;
        self.fail_if(ApplyFailpoint::StageReadyCommitted)?;
        self.finish_create(transaction, events, control)
    }

    fn begin_ensure_directory(
        &mut self,
        operation: OperationBinding,
        body: &OperationBody,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        let parent = self.root.open_parent(body.path(), self.limits, control)?;
        parent.reject_name_collision(body.path(), self.limits, control)?;
        let Some(identity) = parent.target_identity()? else {
            return self.record_standalone_conflict(
                operation,
                body.path(),
                ConflictReason::TargetChanged,
                None,
            );
        };
        if !parent.target_is_directory(identity)? {
            return self.record_standalone_conflict(
                operation,
                body.path(),
                ConflictReason::TargetExists,
                Some(identity),
            );
        }
        let transaction = ApplyTransactionId::random()?;
        let intent = ApplyIntent::new(
            transaction,
            operation,
            self.root.identity,
            body.path().clone(),
            EntryValue::Directory,
            ApplyAction::EnsureExisting,
            ExpectedTarget::Directory(identity),
            None,
        )?;
        self.append(ApplyRecord::Intent(intent))?;
        self.fail_if(ApplyFailpoint::IntentCommitted)?;
        self.finish_existing(transaction, events, control)
    }

    fn begin_adopt_file(
        &mut self,
        operation: OperationBinding,
        body: &OperationBody,
        content: FileContent,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        if content.byte_length() > self.limits.maximum_revalidation_bytes {
            return Err(ApplyError::ResourceLimit);
        }
        let parent = self.root.open_parent(body.path(), self.limits, control)?;
        parent.reject_name_collision(body.path(), self.limits, control)?;
        let observed = match parent.inspect_file(
            parent.name.as_str(),
            None,
            content,
            FileModeRequirement::Adoptable,
            control,
        ) {
            Ok(observed) => observed,
            Err(ApplyError::Interrupted) => return Err(ApplyError::Interrupted),
            Err(ApplyError::ResourceLimit) => return Err(ApplyError::ResourceLimit),
            Err(ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent) => {
                return self.record_standalone_conflict(
                    operation,
                    body.path(),
                    ConflictReason::ContentMismatch,
                    parent.target_identity()?,
                );
            }
            Err(error) => return Err(error),
        };
        let transaction = ApplyTransactionId::random()?;
        let intent = ApplyIntent::new(
            transaction,
            operation,
            self.root.identity,
            body.path().clone(),
            EntryValue::File(content),
            ApplyAction::AdoptExisting,
            ExpectedTarget::File {
                identity: observed.identity,
                permission_bits: observed.permission_bits,
            },
            None,
        )?;
        self.append(ApplyRecord::Intent(intent))?;
        self.fail_if(ApplyFailpoint::IntentCommitted)?;
        self.run_mutation_hook(ApplyMutationPoint::BeforeAdoptionVerification);
        self.finish_existing(transaction, events, control)
    }

    fn reconcile_transaction(
        &mut self,
        transaction: ApplyTransactionId,
        events: &DurableFolderLog,
        store: &SyncContentStore,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        let (operation, path, desired, action, stage_name, has_stage_ready, intent_digest) = {
            let retained = self
                .log
                .machine()?
                .transaction(transaction)
                .ok_or(ApplyError::Changed)?;
            let intent = retained.intent();
            if intent.root() != self.root.identity {
                return Err(ApplyError::Changed);
            }
            (
                intent.operation(),
                intent.path().clone(),
                intent.desired(),
                intent.action(),
                intent.stage_name().cloned(),
                retained.stage_ready().is_some(),
                retained.intent_digest(),
            )
        };
        let accepted = events
            .machine()?
            .accepted_operation_by_digest(operation.id(), operation.digest_bytes())
            .map_err(|_| ApplyError::UnadmittedOperation)?;
        require_active(events, accepted.id(), accepted.body())?;
        if accepted.body().path() != &path || accepted.body().value() != desired {
            return Err(ApplyError::Changed);
        }
        if matches!(
            action,
            ApplyAction::EnsureExisting | ApplyAction::AdoptExisting
        ) {
            return self.finish_existing(transaction, events, control);
        }
        if !has_stage_ready && action == ApplyAction::Create {
            let stage_name = stage_name.ok_or(ApplyError::Changed)?;
            let parent = self.root.open_parent(&path, self.limits, control)?;
            if let Some(target) = parent.target_identity()? {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::TargetExists,
                    Some(target),
                );
            }
            if parent.named_identity(stage_name.as_str())?.is_some() {
                // Without a committed StageReady, neither the random spelling
                // nor its contents prove this process created the incumbent.
                return Ok(ApplyOutcome::Pending);
            }
            if let EntryValue::File(content) = desired {
                let receipt = store.verify_retained(content, control)?;
                if receipt.domain().folder_id() != self.binding.folder_id()
                    || receipt.domain().installation_id() != self.binding.installation_id()
                    || receipt.domain().generation_id() != self.binding.generation_id()
                {
                    return Err(ApplyError::InvalidConfiguration);
                }
            }
            self.root
                .revalidate_parent(&path, parent.identity, self.limits, control)?;
            let stage_identity = match desired {
                EntryValue::File(content) => {
                    parent.create_file_stage(&stage_name, content, store, control)?
                }
                EntryValue::Directory => parent.create_directory_stage(&stage_name, control)?,
                EntryValue::Tombstone => return Err(ApplyError::Changed),
            };
            let ready = ApplyStageReady::new(
                transaction,
                intent_digest,
                operation,
                stage_identity,
                desired,
            )?;
            self.append(ApplyRecord::StageReady(ready))?;
        }
        self.finish_create(transaction, events, control)
    }

    fn finish_existing(
        &mut self,
        transaction: ApplyTransactionId,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        check_control(control)?;
        let (intent_digest, operation, root, path, desired, action, expected) = {
            let retained = self
                .log
                .machine()?
                .transaction(transaction)
                .ok_or(ApplyError::Changed)?;
            let intent = retained.intent();
            (
                retained.intent_digest(),
                intent.operation(),
                intent.root(),
                intent.path().clone(),
                intent.desired(),
                intent.action(),
                intent.expected_target(),
            )
        };
        if root != self.root.identity {
            return Err(ApplyError::Changed);
        }
        let parent = match self.root.open_parent(&path, self.limits, control) {
            Ok(parent) => parent,
            Err(ApplyError::Changed) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::RootChanged,
                    None,
                );
            }
            Err(ApplyError::UnsafeParent) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::ParentChanged,
                    None,
                );
            }
            Err(ApplyError::NameCollision) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::PortableNameCollision,
                    None,
                );
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = parent.reject_name_collision(&path, self.limits, control) {
            if matches!(error, ApplyError::NameCollision) {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::PortableNameCollision,
                    None,
                );
            }
            return Err(error);
        }
        let target_identity = match (action, expected, desired) {
            (
                ApplyAction::EnsureExisting,
                ExpectedTarget::Directory(expected),
                EntryValue::Directory,
            ) => {
                let opened = match parent.open_adoption_directory(expected) {
                    Ok(opened) => opened,
                    Err(
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent,
                    ) => {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    Err(error) => return Err(error),
                };
                self.run_mutation_hook(ApplyMutationPoint::BeforeAdoptionDescriptorSync);
                let hooks = self.take_adoption_verify_hooks();
                if let Err(error) = opened.sync_and_revalidate(&parent, control, hooks) {
                    if matches!(
                        error,
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent
                    ) {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    return Err(error);
                }
                self.run_mutation_hook(ApplyMutationPoint::BeforeAdoptionReceipt);
                if let Err(error) = self.revalidate_existing_target(
                    &path,
                    parent.identity,
                    expected,
                    desired,
                    None,
                    control,
                ) {
                    if matches!(
                        error,
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent
                    ) {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    return Err(error);
                }
                expected
            }
            (
                ApplyAction::AdoptExisting,
                ExpectedTarget::File {
                    identity,
                    permission_bits,
                },
                EntryValue::File(content),
            ) => {
                if content.byte_length() > self.limits.maximum_revalidation_bytes {
                    return Err(ApplyError::ResourceLimit);
                }
                let opened = match parent.open_adoption_file(
                    parent.name.as_str(),
                    Some(identity),
                    content,
                    FileModeRequirement::Exact(permission_bits),
                ) {
                    Ok(opened) => opened,
                    Err(
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent,
                    ) => {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    Err(error) => return Err(error),
                };
                self.run_mutation_hook(ApplyMutationPoint::BeforeAdoptionDescriptorSync);
                let hooks = self.take_adoption_verify_hooks();
                if let Err(error) =
                    opened.verify(&parent, parent.name.as_str(), content, control, true, hooks)
                {
                    if matches!(
                        error,
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent
                    ) {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    return Err(error);
                }
                self.run_mutation_hook(ApplyMutationPoint::BeforeAdoptionReceipt);
                if let Err(error) = self.revalidate_existing_target(
                    &path,
                    parent.identity,
                    identity,
                    desired,
                    Some(permission_bits),
                    control,
                ) {
                    if matches!(
                        error,
                        ApplyError::Changed | ApplyError::NameCollision | ApplyError::UnsafeParent
                    ) {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::TargetChanged,
                            parent.target_identity()?,
                        );
                    }
                    return Err(error);
                }
                identity
            }
            _ => return Err(ApplyError::Changed),
        };
        require_bound_active(events, operation, &path, desired)?;
        check_control(control)?;
        let applied = ApplyApplied::new(
            transaction,
            intent_digest,
            operation,
            target_identity,
            desired,
        )?;
        self.append(ApplyRecord::Applied(applied))?;
        Ok(ApplyOutcome::Applied)
    }

    fn finish_create(
        &mut self,
        transaction: ApplyTransactionId,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<ApplyOutcome, ApplyError> {
        check_control(control)?;
        let (intent_digest, operation, path, desired, action, stage_name, stage_identity) = {
            let machine = self.log.machine()?;
            let transaction_state = machine
                .transaction(transaction)
                .ok_or(ApplyError::Changed)?;
            let intent = transaction_state.intent();
            (
                transaction_state.intent_digest(),
                intent.operation(),
                intent.path().clone(),
                intent.desired(),
                intent.action(),
                intent.stage_name().cloned(),
                transaction_state
                    .stage_ready()
                    .map(|stage| stage.stage_identity()),
            )
        };
        if self
            .log
            .machine()?
            .transaction(transaction)
            .ok_or(ApplyError::Changed)?
            .intent()
            .root()
            != self.root.identity
        {
            return Err(ApplyError::Changed);
        }
        if action != ApplyAction::Create {
            return Err(ApplyError::Changed);
        }
        let stage_identity = stage_identity.ok_or(ApplyError::Pending)?;
        let stage_name = stage_name.ok_or(ApplyError::Changed)?;
        let parent = match self.root.open_parent(&path, self.limits, control) {
            Ok(parent) => parent,
            Err(ApplyError::Changed) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::RootChanged,
                    None,
                );
            }
            Err(ApplyError::UnsafeParent) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::ParentChanged,
                    None,
                );
            }
            Err(ApplyError::NameCollision) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::PortableNameCollision,
                    None,
                );
            }
            Err(error) => return Err(error),
        };
        match parent.target_identity()? {
            Some(target) if target == stage_identity => {}
            Some(target) => {
                return self.conflict(
                    transaction,
                    intent_digest,
                    operation,
                    &path,
                    ConflictReason::TargetExists,
                    Some(target),
                );
            }
            None => {
                match parent.named_identity(stage_name.as_str())? {
                    None => {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::StageMissing,
                            None,
                        );
                    }
                    Some(current) if current != stage_identity => {
                        return self.conflict(
                            transaction,
                            intent_digest,
                            operation,
                            &path,
                            ConflictReason::StageChanged,
                            Some(current),
                        );
                    }
                    Some(_) => {}
                }
                if let Err(error) =
                    parent.validate_stage(&stage_name, stage_identity, desired, control)
                {
                    if matches!(error, ApplyError::Interrupted) {
                        return Err(error);
                    }
                    return self.conflict(
                        transaction,
                        intent_digest,
                        operation,
                        &path,
                        ConflictReason::StageChanged,
                        Some(stage_identity),
                    );
                }
                self.run_mutation_hook(ApplyMutationPoint::BeforePromotion);
                self.root
                    .revalidate_parent(&path, parent.identity, self.limits, control)?;
                if let Err(error) =
                    parent.validate_stage(&stage_name, stage_identity, desired, control)
                {
                    if matches!(error, ApplyError::Interrupted | ApplyError::ResourceLimit) {
                        return Err(error);
                    }
                    return self.conflict(
                        transaction,
                        intent_digest,
                        operation,
                        &path,
                        ConflictReason::StageChanged,
                        parent.named_identity(stage_name.as_str())?,
                    );
                }
                self.run_mutation_hook(ApplyMutationPoint::AfterPromotionValidation);
                // The rename primitive is not an inode-conditioned CAS. The
                // validation above closes deterministic caller seams; the
                // post-rename check below treats a syscall-window
                // substitution as a preserved durable conflict.
                renameat_with(
                    &parent.descriptor,
                    stage_name.as_str(),
                    &parent.descriptor,
                    parent.name.as_str(),
                    RenameFlags::NOREPLACE,
                )
                .map_err(|error| os_error("promote apply stage", error))?;
                self.fail_if(ApplyFailpoint::Promoted)?;
            }
        }
        if let Err(error) = parent.validate_target(stage_identity, desired, control) {
            if matches!(error, ApplyError::Interrupted | ApplyError::ResourceLimit) {
                return Err(error);
            }
            return self.conflict(
                transaction,
                intent_digest,
                operation,
                &path,
                ConflictReason::FinalChanged,
                parent.target_identity()?,
            );
        }
        parent.sync_target_and_parent(desired)?;
        self.revalidate_promoted_target(&path, parent.identity, stage_identity, desired, control)?;
        self.fail_if(ApplyFailpoint::TargetSynced)?;
        require_bound_active(events, operation, &path, desired)?;
        check_control(control)?;
        let applied = ApplyApplied::new(
            transaction,
            intent_digest,
            operation,
            stage_identity,
            desired,
        )?;
        self.append(ApplyRecord::Applied(applied))?;
        Ok(ApplyOutcome::Applied)
    }

    fn revalidate_existing_target(
        &self,
        path: &SyncPath,
        parent_identity: EntryIdentity,
        target_identity: EntryIdentity,
        desired: EntryValue,
        permission_bits: Option<u16>,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        let current_parent = self.root.open_parent(path, self.limits, control)?;
        if current_parent.identity != parent_identity {
            return Err(ApplyError::Changed);
        }
        current_parent.reject_name_collision(path, self.limits, control)?;
        match permission_bits {
            Some(permission_bits) => current_parent.validate_adopted_target(
                target_identity,
                desired,
                permission_bits,
                control,
            ),
            None => current_parent.validate_target(target_identity, desired, control),
        }
    }

    fn append(
        &mut self,
        record: ApplyRecord,
    ) -> Result<super::apply_record::ApplyRecordDigest, ApplyError> {
        let digest = record.digest();
        let encoded = record.encode();
        if let Err(error) = self.log.append(encoded.as_bytes()) {
            self.poisoned = matches!(
                &error,
                EventLogError::Changed | EventLogError::Poisoned | EventLogError::State(_)
            );
            return Err(error.into());
        }
        Ok(digest)
    }

    fn record_standalone_conflict(
        &mut self,
        operation: OperationBinding,
        path: &SyncPath,
        reason: ConflictReason,
        observed: Option<EntryIdentity>,
    ) -> Result<ApplyOutcome, ApplyError> {
        let transaction = ApplyTransactionId::random()?;
        let conflict =
            ApplyConflict::new(transaction, None, operation, path.clone(), reason, observed);
        self.append(ApplyRecord::Conflict(conflict))?;
        Ok(ApplyOutcome::Conflict)
    }

    fn conflict(
        &mut self,
        transaction: ApplyTransactionId,
        intent: super::apply_record::ApplyRecordDigest,
        operation: OperationBinding,
        path: &SyncPath,
        reason: ConflictReason,
        observed: Option<EntryIdentity>,
    ) -> Result<ApplyOutcome, ApplyError> {
        let conflict = ApplyConflict::new(
            transaction,
            Some(intent),
            operation,
            path.clone(),
            reason,
            observed,
        );
        self.append(ApplyRecord::Conflict(conflict))?;
        Ok(ApplyOutcome::Conflict)
    }

    fn existing_outcome(&self, id: OpId) -> Result<Option<ApplyOutcome>, ApplyError> {
        Ok(match self.log.machine()?.operation_terminal(id) {
            Some(ApplyTerminal::Applied(_)) => Some(ApplyOutcome::Applied),
            Some(ApplyTerminal::Conflict(_)) => Some(ApplyOutcome::Conflict),
            Some(ApplyTerminal::Unsupported(_)) => Some(ApplyOutcome::Unsupported),
            None => None,
        })
    }

    fn validate_replayed_history(
        &self,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        self.log.machine()?.visit_transactions(|transaction| {
            check_control(control)?;
            if transaction.intent().root() != self.root.identity {
                return Err(ApplyError::Changed);
            }
            Ok(())
        })?;
        self.log
            .machine()?
            .visit_operations(|operation, path, desired| {
                check_control(control)?;
                let accepted = events
                    .machine()?
                    .accepted_operation_by_digest(operation.id(), operation.digest_bytes())
                    .map_err(|_| ApplyError::UnadmittedOperation)?;
                if accepted.body().path() != path
                    || desired.is_some_and(|value| accepted.body().value() != value)
                {
                    return Err(ApplyError::UnadmittedOperation);
                }
                Ok(())
            })
    }

    fn validate_replayed_filesystem(
        &self,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        let mut revalidation_bytes = 0_u64;
        self.log.machine()?.visit_transactions(|transaction| {
            check_control(control)?;
            let Some(ApplyTerminal::Applied(applied)) = transaction.terminal() else {
                return Ok(());
            };
            let accepted = events
                .machine()?
                .accepted_operation_by_digest(
                    transaction.intent().operation().id(),
                    transaction.intent().operation().digest_bytes(),
                )
                .map_err(|_| ApplyError::UnadmittedOperation)?;
            match require_active(events, accepted.id(), accepted.body()) {
                Ok(()) => {
                    if let EntryValue::File(content) = applied.desired() {
                        revalidation_bytes = revalidation_bytes
                            .checked_add(content.byte_length())
                            .ok_or(ApplyError::ResourceLimit)?;
                    }
                }
                Err(ApplyError::ProjectionConflict) => {}
                Err(error) => return Err(error),
            }
            Ok(())
        })?;
        if revalidation_bytes > self.limits.maximum_revalidation_bytes {
            return Err(ApplyError::ResourceLimit);
        }
        self.log.machine()?.visit_transactions(|transaction| {
            check_control(control)?;
            let Some(ApplyTerminal::Applied(applied)) = transaction.terminal() else {
                return Ok(());
            };
            let accepted = events
                .machine()?
                .accepted_operation_by_digest(
                    transaction.intent().operation().id(),
                    transaction.intent().operation().digest_bytes(),
                )
                .map_err(|_| ApplyError::UnadmittedOperation)?;
            match require_active(events, accepted.id(), accepted.body()) {
                Ok(()) => {}
                Err(ApplyError::ProjectionConflict) => return Ok(()),
                Err(error) => return Err(error),
            }
            let parent =
                self.root
                    .open_parent(transaction.intent().path(), self.limits, control)?;
            match (
                transaction.intent().action(),
                transaction.intent().expected_target(),
            ) {
                (
                    ApplyAction::AdoptExisting,
                    ExpectedTarget::File {
                        permission_bits, ..
                    },
                ) => parent.validate_adopted_target(
                    applied.target_identity(),
                    applied.desired(),
                    permission_bits,
                    control,
                )?,
                _ => {
                    parent.validate_target(applied.target_identity(), applied.desired(), control)?
                }
            }
            Ok(())
        })
    }

    fn require_usable(
        &self,
        events: &DurableFolderLog,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        if self.poisoned {
            return Err(ApplyError::Pending);
        }
        self.outer.sync(&self.outer_lock)?;
        require_matching_binding(self.binding, events.binding()?)?;
        self.root.revalidate()?;
        let _ = self.log.machine()?;
        self.validate_replayed_filesystem(events, control)?;
        Ok(())
    }

    fn revalidate_promoted_target(
        &self,
        path: &SyncPath,
        parent_identity: EntryIdentity,
        target_identity: EntryIdentity,
        desired: EntryValue,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        let current_parent = self.root.open_parent(path, self.limits, control)?;
        if current_parent.identity != parent_identity {
            return Err(ApplyError::Changed);
        }
        current_parent.reject_name_collision(path, self.limits, control)?;
        current_parent.validate_target(target_identity, desired, control)
    }

    fn fail_if(&mut self, point: ApplyFailpoint) -> Result<(), ApplyError> {
        if self.failpoint == Some(point) {
            self.failpoint = None;
            return Err(ApplyError::Pending);
        }
        Ok(())
    }

    fn run_mutation_hook(&mut self, point: ApplyMutationPoint) {
        #[cfg(not(test))]
        let _ = point;
        #[cfg(test)]
        if self
            .mutation_hook
            .as_ref()
            .is_some_and(|(selected, _)| *selected == point)
        {
            let (_, hook) = self.mutation_hook.take().expect("checked mutation hook");
            hook();
        }
    }

    fn take_adoption_verify_hooks(&mut self) -> AdoptionVerifyHooks {
        #[cfg(not(test))]
        {
            AdoptionVerifyHooks::default()
        }
        #[cfg(test)]
        {
            AdoptionVerifyHooks {
                after_sync: self.adoption_sync_hook.take(),
                io: self.adoption_io_hook.take(),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyFailpoint {
    IntentCommitted,
    StageDurable,
    StageReadyCommitted,
    Promoted,
    TargetSynced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApplyMutationPoint {
    BeforeStageCreate,
    BeforePromotion,
    AfterPromotionValidation,
    BeforeAdoptionVerification,
    BeforeAdoptionDescriptorSync,
    BeforeAdoptionReceipt,
}

fn validate_configuration(
    outer: &PrivateStateDir,
    lock: &PrivateStateLock,
    binding: LogBinding,
    limits: ApplyLimits,
    root: &AuthorizedRoot,
    events: &DurableFolderLog,
) -> Result<(), ApplyError> {
    if limits.maximum_directory_entries == 0
        || limits.maximum_directory_entries > MAX_DIRECTORY_ENTRIES
        || limits.maximum_directory_name_bytes == 0
        || limits.maximum_directory_name_bytes > MAX_DIRECTORY_NAME_BYTES
        || limits.maximum_revalidation_bytes > MAX_REVALIDATION_BYTES
    {
        return Err(ApplyError::InvalidConfiguration);
    }
    outer.sync(lock)?;
    require_matching_binding(binding, events.binding()?)?;
    let opened = UserRoot::open(root)?;
    opened.revalidate()?;
    Ok(())
}

fn require_matching_binding(apply: LogBinding, events: LogBinding) -> Result<(), ApplyError> {
    if apply.file_kind() != LogFileKind::Apply
        || events.file_kind() != LogFileKind::FolderEvents
        || apply.folder_id() != events.folder_id()
        || apply.installation_id() != events.installation_id()
        || apply.generation_id() != events.generation_id()
    {
        return Err(ApplyError::InvalidConfiguration);
    }
    Ok(())
}

fn require_active(
    events: &DurableFolderLog,
    id: OpId,
    body: &OperationBody,
) -> Result<(), ApplyError> {
    let machine = events.machine()?;
    let register = machine
        .register(body.path())
        .ok_or(ApplyError::ProjectionConflict)?;
    if register.active_count() != 1 {
        return Err(ApplyError::ProjectionConflict);
    }
    let active = register
        .active()
        .next()
        .ok_or(ApplyError::ProjectionConflict)?;
    if active.id() != id || *active.value() != body.value() {
        return Err(ApplyError::ProjectionConflict);
    }
    let components = body.path().components().collect::<Vec<_>>();
    let mut prefix = String::new();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let ancestor = SyncPath::from_wire(&prefix).map_err(|_| ApplyError::ProjectionConflict)?;
        if let Some(register) = machine.register(&ancestor)
            && (register.active_count() != 1
                || register
                    .active()
                    .next()
                    .is_none_or(|entry| *entry.value() != EntryValue::Directory))
        {
            return Err(ApplyError::ProjectionConflict);
        }
    }
    if matches!(body.value(), EntryValue::File(_)) && machine.has_live_descendants(body.path()) {
        return Err(ApplyError::ProjectionConflict);
    }
    Ok(())
}

fn require_bound_active(
    events: &DurableFolderLog,
    operation: OperationBinding,
    path: &SyncPath,
    desired: EntryValue,
) -> Result<(), ApplyError> {
    let accepted = events
        .machine()?
        .accepted_operation_by_digest(operation.id(), operation.digest_bytes())
        .map_err(|_| ApplyError::UnadmittedOperation)?;
    if accepted.body().path() != path || accepted.body().value() != desired {
        return Err(ApplyError::Changed);
    }
    require_active(events, accepted.id(), accepted.body())
}

impl UserRoot {
    fn open(root: &AuthorizedRoot) -> Result<Self, ApplyError> {
        let descriptor = open(
            root.canonical_path(),
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| os_error("open authorized apply root", error))?;
        let stat =
            fstat(&descriptor).map_err(|error| os_error("inspect authorized apply root", error))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(ApplyError::InvalidConfiguration);
        }
        Ok(Self {
            canonical: root.canonical_path().to_path_buf(),
            descriptor,
            identity: identity(&stat),
        })
    }

    fn revalidate(&self) -> Result<(), ApplyError> {
        let current = open(
            &self.canonical,
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| os_error("reopen authorized apply root", error))?;
        let stat =
            fstat(&current).map_err(|error| os_error("reinspect authorized apply root", error))?;
        if identity(&stat) != self.identity {
            return Err(ApplyError::Changed);
        }
        Ok(())
    }

    fn open_parent(
        &self,
        path: &SyncPath,
        limits: ApplyLimits,
        control: &JobControl,
    ) -> Result<TargetParent, ApplyError> {
        check_control(control)?;
        self.revalidate()?;
        let mut components = path.components().peekable();
        let mut current = openat(
            &self.descriptor,
            ".",
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| os_error("duplicate authorized apply root", error))?;
        let mut final_name = None;
        while let Some(component) = components.next() {
            check_control(control)?;
            if components.peek().is_none() {
                reject_component_collision(&current, component, limits, control)?;
                final_name = Some(component.to_owned());
                break;
            }
            reject_component_collision(&current, component, limits, control)?;
            current = openat(
                &current,
                component,
                OFlags::RDONLY
                    | OFlags::DIRECTORY
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|error| match error {
                rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP => {
                    ApplyError::UnsafeParent
                }
                other => os_error("open apply parent", other),
            })?;
        }
        let name = final_name.ok_or(ApplyError::UnsafeParent)?;
        let stat = fstat(&current).map_err(|error| os_error("inspect apply parent", error))?;
        Ok(TargetParent {
            descriptor: current,
            identity: identity(&stat),
            name,
        })
    }

    fn revalidate_parent(
        &self,
        path: &SyncPath,
        expected: EntryIdentity,
        limits: ApplyLimits,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        let current = self.open_parent(path, limits, control)?;
        if current.identity != expected {
            return Err(ApplyError::Changed);
        }
        Ok(())
    }
}

fn reject_component_collision(
    directory: &OwnedFd,
    name: &str,
    limits: ApplyLimits,
    control: &JobControl,
) -> Result<(), ApplyError> {
    let target = SyncPath::from_wire(name)
        .map_err(|_| ApplyError::InvalidConfiguration)?
        .portable_collision_key();
    let mut entries = 0_usize;
    let mut name_bytes = 0_u64;
    let mut reader =
        Dir::read_from(directory).map_err(|error| os_error("enumerate apply parent", error))?;
    for entry in &mut reader {
        check_control(control)?;
        let entry = entry.map_err(|error| os_error("read apply parent entry", error))?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        entries = entries.checked_add(1).ok_or(ApplyError::ResourceLimit)?;
        name_bytes = name_bytes
            .checked_add(raw.len() as u64)
            .ok_or(ApplyError::ResourceLimit)?;
        if entries > limits.maximum_directory_entries
            || name_bytes > limits.maximum_directory_name_bytes
        {
            return Err(ApplyError::ResourceLimit);
        }
        let spelling = std::str::from_utf8(raw).map_err(|_| ApplyError::NameCollision)?;
        let local = SyncPath::from_local(spelling).map_err(|_| ApplyError::NameCollision)?;
        if spelling != name && local.portable_collision_key() == target {
            return Err(ApplyError::NameCollision);
        }
    }
    Ok(())
}

impl TargetParent {
    fn reject_name_collision(
        &self,
        path: &SyncPath,
        limits: ApplyLimits,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        reject_component_collision(&self.descriptor, &self.name, limits, control)?;
        let _ = path;
        self.revalidate()
    }

    fn revalidate(&self) -> Result<(), ApplyError> {
        let stat =
            fstat(&self.descriptor).map_err(|error| os_error("reinspect apply parent", error))?;
        if identity(&stat) != self.identity {
            return Err(ApplyError::Changed);
        }
        Ok(())
    }

    fn target_identity(&self) -> Result<Option<EntryIdentity>, ApplyError> {
        self.named_identity(&self.name)
    }

    fn named_identity(&self, name: &str) -> Result<Option<EntryIdentity>, ApplyError> {
        match statat(&self.descriptor, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(identity(&stat))),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(os_error("inspect apply target", error)),
        }
    }

    fn target_is_directory(&self, expected: EntryIdentity) -> Result<bool, ApplyError> {
        let stat = statat(
            &self.descriptor,
            self.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|error| os_error("inspect apply directory", error))?;
        Ok(identity(&stat) == expected
            && FileType::from_raw_mode(stat.st_mode) == FileType::Directory)
    }

    fn open_adoption_directory(
        &self,
        expected: EntryIdentity,
    ) -> Result<OpenedAdoptionDirectory, ApplyError> {
        let fd = openat(
            &self.descriptor,
            self.name.as_str(),
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP => {
                ApplyError::Changed
            }
            other => os_error("open apply directory", other),
        })?;
        let stat = fstat(&fd).map_err(|error| os_error("inspect apply directory handle", error))?;
        if identity(&stat) != expected
            || FileType::from_raw_mode(stat.st_mode) != FileType::Directory
        {
            return Err(ApplyError::Changed);
        }
        Ok(OpenedAdoptionDirectory {
            descriptor: fd,
            identity: expected,
        })
    }

    fn create_directory_stage(
        &self,
        name: &StageName,
        control: &JobControl,
    ) -> Result<EntryIdentity, ApplyError> {
        check_control(control)?;
        mkdirat(
            &self.descriptor,
            name.as_str(),
            Mode::from_raw_mode(DIRECTORY_MODE),
        )
        .map_err(|error| os_error("create apply directory stage", error))?;
        let fd = openat(
            &self.descriptor,
            name.as_str(),
            OFlags::RDONLY
                | OFlags::DIRECTORY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| os_error("open apply directory stage", error))?;
        fchmod(&fd, Mode::from_raw_mode(DIRECTORY_MODE))
            .map_err(|error| os_error("set apply directory stage mode", error))?;
        fsync(&fd).map_err(|error| os_error("sync apply directory stage", error))?;
        fsync(&self.descriptor).map_err(|error| os_error("sync apply stage parent", error))?;
        Ok(identity(&fstat(&fd).map_err(|error| {
            os_error("inspect apply directory stage", error)
        })?))
    }

    fn create_file_stage(
        &self,
        name: &StageName,
        content: FileContent,
        store: &SyncContentStore,
        control: &JobControl,
    ) -> Result<EntryIdentity, ApplyError> {
        let fd = openat(
            &self.descriptor,
            name.as_str(),
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::from_raw_mode(FILE_MODE),
        )
        .map_err(|error| os_error("create apply file stage", error))?;
        let mut file = File::from(fd);
        let manifest = store.read_manifest(content)?;
        let mut hasher = blake3::Hasher::new();
        let mut length = 0_u64;
        for descriptor in manifest.chunks() {
            control.check().map_err(|_| ApplyError::Pending)?;
            let chunk = store.read_chunk(*descriptor, control)?;
            length = length
                .checked_add(chunk.len() as u64)
                .ok_or(ApplyError::ResourceLimit)?;
            if length > content.byte_length() {
                return Err(ApplyError::Changed);
            }
            file.write_all(&chunk)
                .map_err(|error| std_io_error("write apply stage", error))?;
            hasher.update(&chunk);
        }
        if length != content.byte_length()
            || hasher.finalize().as_bytes() != &content.digest().to_bytes()
        {
            return Err(ApplyError::Changed);
        }
        let mode = if content.executable() {
            EXECUTABLE_FILE_MODE
        } else {
            FILE_MODE
        };
        fchmod(&file, Mode::from_raw_mode(mode))
            .map_err(|error| os_error("set apply stage mode", error))?;
        fsync(&file).map_err(|error| os_error("sync apply file stage", error))?;
        let stat = fstat(&file).map_err(|error| os_error("inspect apply file stage", error))?;
        let identity = identity(&stat);
        drop(file);
        self.validate_stage(name, identity, EntryValue::File(content), control)?;
        fsync(&self.descriptor).map_err(|error| os_error("sync apply stage parent", error))?;
        Ok(identity)
    }

    fn validate_stage(
        &self,
        name: &StageName,
        expected: EntryIdentity,
        desired: EntryValue,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        self.validate_named(
            name.as_str(),
            expected,
            desired,
            FileModeRequirement::Staged,
            control,
        )
    }

    fn validate_target(
        &self,
        expected: EntryIdentity,
        desired: EntryValue,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        self.validate_named(
            self.name.as_str(),
            expected,
            desired,
            FileModeRequirement::Staged,
            control,
        )
    }

    fn validate_adopted_target(
        &self,
        expected: EntryIdentity,
        desired: EntryValue,
        permission_bits: u16,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        self.validate_named(
            self.name.as_str(),
            expected,
            desired,
            FileModeRequirement::Exact(permission_bits),
            control,
        )
    }

    fn validate_named(
        &self,
        name: &str,
        expected: EntryIdentity,
        desired: EntryValue,
        file_mode: FileModeRequirement,
        control: &JobControl,
    ) -> Result<(), ApplyError> {
        check_control(control)?;
        match desired {
            EntryValue::Directory => {
                let fd = openat(
                    &self.descriptor,
                    name,
                    OFlags::RDONLY
                        | OFlags::DIRECTORY
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC
                        | OFlags::NONBLOCK,
                    Mode::empty(),
                )
                .map_err(|error| match error {
                    rustix::io::Errno::NOENT
                    | rustix::io::Errno::NOTDIR
                    | rustix::io::Errno::LOOP => ApplyError::Changed,
                    other => os_error("open applied directory", other),
                })?;
                let stat =
                    fstat(&fd).map_err(|error| os_error("inspect applied directory", error))?;
                if identity(&stat) != expected {
                    return Err(ApplyError::Changed);
                }
                let named =
                    statat(&self.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|error| {
                        match error {
                            rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => {
                                ApplyError::Changed
                            }
                            other => os_error("reinspect applied directory entry", other),
                        }
                    })?;
                if identity(&named) != expected
                    || FileType::from_raw_mode(named.st_mode) != FileType::Directory
                {
                    return Err(ApplyError::Changed);
                }
            }
            EntryValue::File(content) => {
                self.inspect_file(name, Some(expected), content, file_mode, control)?;
            }
            EntryValue::Tombstone => return Err(ApplyError::InvalidConfiguration),
        }
        Ok(())
    }

    fn inspect_file(
        &self,
        name: &str,
        expected: Option<EntryIdentity>,
        content: FileContent,
        mode_requirement: FileModeRequirement,
        control: &JobControl,
    ) -> Result<ObservedFile, ApplyError> {
        check_control(control)?;
        self.open_adoption_file(name, expected, content, mode_requirement)?
            .verify(
                self,
                name,
                content,
                control,
                false,
                AdoptionVerifyHooks::default(),
            )
    }

    fn open_adoption_file(
        &self,
        name: &str,
        expected: Option<EntryIdentity>,
        content: FileContent,
        mode_requirement: FileModeRequirement,
    ) -> Result<OpenedAdoptionFile, ApplyError> {
        let fd = openat(
            &self.descriptor,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP => {
                ApplyError::Changed
            }
            other => os_error("open applied file", other),
        })?;
        let stat = fstat(&fd).map_err(|error| os_error("inspect applied file", error))?;
        let observed_identity = identity(&stat);
        let permission_bits = adoption_permission_bits(stat.st_mode);
        if expected.is_some_and(|expected| expected != observed_identity)
            || FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_size < 0
            || stat.st_size as u64 != content.byte_length()
            || !mode_requirement.accepts(permission_bits, content.executable())
        {
            return Err(ApplyError::Changed);
        }
        Ok(OpenedAdoptionFile {
            file: File::from(fd),
            before: stat,
            observed: ObservedFile {
                identity: observed_identity,
                permission_bits,
            },
        })
    }

    fn sync_target_and_parent(&self, desired: EntryValue) -> Result<(), ApplyError> {
        let flags = match desired {
            EntryValue::Directory => OFlags::RDONLY | OFlags::DIRECTORY,
            EntryValue::File(_) => OFlags::RDONLY,
            EntryValue::Tombstone => return Err(ApplyError::InvalidConfiguration),
        } | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK;
        let fd = openat(&self.descriptor, self.name.as_str(), flags, Mode::empty())
            .map_err(|error| os_error("open applied target for sync", error))?;
        fsync(&fd).map_err(|error| os_error("sync applied target", error))?;
        fsync(&self.descriptor).map_err(|error| os_error("sync applied parent", error))
    }
}

impl OpenedAdoptionFile {
    fn verify(
        mut self,
        parent: &TargetParent,
        name: &str,
        content: FileContent,
        control: &JobControl,
        sync: bool,
        mut hooks: AdoptionVerifyHooks,
    ) -> Result<ObservedFile, ApplyError> {
        let mut hasher = blake3::Hasher::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; STREAM_BUFFER_BYTES];
        loop {
            check_control(control)?;
            run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::ReadFile)?;
            let read = self
                .file
                .read(&mut buffer)
                .map_err(|error| std_io_error("read applied file", error))?;
            if read == 0 {
                break;
            }
            total = total.checked_add(read as u64).ok_or(ApplyError::Changed)?;
            if total > content.byte_length() {
                return Err(ApplyError::Changed);
            }
            hasher.update(&buffer[..read]);
        }
        if total != content.byte_length()
            || hasher.finalize().as_bytes() != &content.digest().to_bytes()
        {
            return Err(ApplyError::Changed);
        }
        check_control(control)?;
        if sync {
            run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::SyncFile)?;
            fsync(&self.file).map_err(|error| os_error("sync adopted file", error))?;
            if let Some(hook) = hooks.after_sync.take() {
                hook(self.observed.identity);
            }
            check_control(control)?;
        }
        run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::InspectFile)?;
        let after =
            fstat(&self.file).map_err(|error| os_error("reinspect applied file handle", error))?;
        let named = statat(&parent.descriptor, name, AtFlags::SYMLINK_NOFOLLOW).map_err(
            |error| match error {
                rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => ApplyError::Changed,
                other => os_error("reinspect applied file entry", other),
            },
        )?;
        if !same_file_fingerprint(&self.before, &after)
            || !same_file_fingerprint(&after, &named)
            || identity(&named) != self.observed.identity
            || FileType::from_raw_mode(named.st_mode) != FileType::RegularFile
            || named.st_nlink != 1
        {
            return Err(ApplyError::Changed);
        }
        if sync {
            run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::SyncParent)?;
            fsync(&parent.descriptor)
                .map_err(|error| os_error("sync adopted file parent", error))?;
            check_control(control)?;
        }
        Ok(self.observed)
    }
}

impl OpenedAdoptionDirectory {
    fn sync_and_revalidate(
        self,
        parent: &TargetParent,
        control: &JobControl,
        mut hooks: AdoptionVerifyHooks,
    ) -> Result<(), ApplyError> {
        check_control(control)?;
        run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::SyncDirectory)?;
        fsync(&self.descriptor).map_err(|error| os_error("sync adopted directory", error))?;
        if let Some(hook) = hooks.after_sync.take() {
            hook(self.identity);
        }
        check_control(control)?;
        run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::InspectDirectory)?;
        let after = fstat(&self.descriptor)
            .map_err(|error| os_error("reinspect adopted directory", error))?;
        let named = statat(
            &parent.descriptor,
            parent.name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR => ApplyError::Changed,
            other => os_error("reinspect adopted directory entry", other),
        })?;
        if identity(&after) != self.identity
            || identity(&named) != self.identity
            || FileType::from_raw_mode(after.st_mode) != FileType::Directory
            || FileType::from_raw_mode(named.st_mode) != FileType::Directory
        {
            return Err(ApplyError::Changed);
        }
        run_adoption_io_hook(&mut hooks.io, AdoptionIoPoint::SyncParent)?;
        fsync(&parent.descriptor)
            .map_err(|error| os_error("sync adopted directory parent", error))?;
        check_control(control)
    }
}

fn run_adoption_io_hook(
    hook: &mut Option<AdoptionIoHook>,
    point: AdoptionIoPoint,
) -> Result<(), ApplyError> {
    if hook
        .as_ref()
        .is_some_and(|(selected, _)| *selected == point)
    {
        let (_, run) = hook.take().expect("checked adoption I/O hook");
        run()?;
    }
    Ok(())
}

fn same_file_fingerprint(left: &rustix::fs::Stat, right: &rustix::fs::Stat) -> bool {
    left.st_dev == right.st_dev
        && left.st_ino == right.st_ino
        && left.st_mode == right.st_mode
        && left.st_nlink == right.st_nlink
        && left.st_size == right.st_size
        && left.st_mtime == right.st_mtime
        && left.st_mtime_nsec == right.st_mtime_nsec
        && left.st_ctime == right.st_ctime
        && left.st_ctime_nsec == right.st_ctime_nsec
}

impl FileModeRequirement {
    fn accepts(self, permission_bits: u16, executable: bool) -> bool {
        match self {
            Self::Staged => permission_bits == if executable { 0o700 } else { 0o600 },
            Self::Adoptable => {
                permission_bits <= 0o777 && (permission_bits & 0o111 != 0) == executable
            }
            Self::Exact(expected) => {
                expected <= 0o777
                    && permission_bits == expected
                    && (permission_bits & 0o111 != 0) == executable
            }
        }
    }
}

#[allow(clippy::unnecessary_cast)]
fn identity(stat: &rustix::fs::Stat) -> EntryIdentity {
    EntryIdentity::new(stat.st_dev as u64, stat.st_ino as u64)
}

fn check_control(control: &JobControl) -> Result<(), ApplyError> {
    control.check().map_err(|_| ApplyError::Interrupted)
}

#[allow(clippy::unnecessary_cast)]
fn adoption_permission_bits(mode: rustix::fs::RawMode) -> u16 {
    (mode as u32 & 0o7777) as u16
}

fn os_error(operation: &'static str, error: rustix::io::Errno) -> ApplyError {
    ApplyError::Io {
        operation,
        errno: Some(error.raw_os_error()),
    }
}

fn std_io_error(operation: &'static str, error: std::io::Error) -> ApplyError {
    ApplyError::Io {
        operation,
        errno: error.raw_os_error(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Cursor;
    use std::os::unix::ffi::OsStringExt as _;
    use std::os::unix::fs::symlink;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use covalent_protocol::DeviceId;
    use ed25519_dalek::SigningKey;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::sync::body::ContentDigest;
    use crate::sync::content_crypto::{SyncContentCrypto, SyncContentDomain};
    use crate::sync::content_store::ContentStoreLimits;
    use crate::sync::event::{EventEnvelope, EventKind};
    use crate::sync::ids::{FolderId, WriterId};
    use crate::sync::machine::{FolderEventMachine, FolderMachineConfig, FolderMachineLimits};
    use crate::sync::membership::{MemberGrant, MemberRole, encode_signed_epoch};
    use crate::sync::publication::PublishedOperation;

    fn folder() -> FolderId {
        FolderId::from_uuid(Uuid::from_u128(0xa1))
    }

    fn writer() -> WriterId {
        WriterId::from_uuid(Uuid::from_u128(0xa2))
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[0xa3; 32])
    }

    fn folder_machine() -> FolderEventMachine {
        FolderEventMachine::new(FolderMachineConfig {
            folder_id: folder(),
            authority_writer_id: writer(),
            pinned_authority_key: signing_key().verifying_key(),
            local_writer_id: writer(),
            limits: FolderMachineLimits {
                maximum_index_bytes: 1 << 20,
                maximum_operations: 128,
                maximum_paths: 128,
                maximum_membership_epochs: 128,
                maximum_pending_evidence_bytes: 1 << 18,
                maximum_pending_evidence_records: 128,
            },
        })
        .unwrap()
    }

    fn event_binding() -> LogBinding {
        LogBinding::new(
            folder(),
            Uuid::from_u128(0xa4),
            Uuid::from_u128(0xa5),
            LogFileKind::FolderEvents,
        )
    }

    fn apply_binding() -> LogBinding {
        let binding = event_binding();
        LogBinding::new(
            binding.folder_id(),
            binding.installation_id(),
            binding.generation_id(),
            LogFileKind::Apply,
        )
    }

    fn log_limits() -> EventLogLimits {
        EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records: 128,
        }
    }

    fn apply_limits() -> ApplyLimits {
        ApplyLimits {
            machine: ApplyMachineLimits {
                maximum_records: 128,
                maximum_retained_bytes: 1 << 20,
            },
            maximum_directory_entries: 128,
            maximum_directory_name_bytes: 1 << 16,
            maximum_revalidation_bytes: 1 << 20,
        }
    }

    fn genesis() -> Vec<u8> {
        let grant = MemberGrant::new(
            writer(),
            signing_key().verifying_key(),
            DeviceId::from_uuid(Uuid::from_u128(0xa6)),
            SigningKey::from_bytes(&[0xa7; 32]).verifying_key(),
            MemberRole::ReadWrite,
        )
        .unwrap();
        let signed = encode_signed_epoch(
            &signing_key(),
            folder(),
            1,
            writer(),
            None,
            &[grant],
            None,
            &[],
            &[],
            &[],
            &[],
        )
        .unwrap();
        EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &signed)
            .unwrap()
            .encode()
            .unwrap()
            .as_bytes()
            .to_vec()
    }

    struct Fixture {
        state: TempDir,
        user: TempDir,
        outer: Arc<PrivateStateDir>,
        outer_lock: Arc<PrivateStateLock>,
        events: DurableFolderLog,
        store: SyncContentStore,
    }

    impl Fixture {
        fn new() -> Self {
            let state = tempfile::tempdir().unwrap();
            fs::set_permissions(state.path(), fs::Permissions::from_mode(0o700)).unwrap();
            let user = tempfile::tempdir().unwrap();
            let outer = Arc::new(PrivateStateDir::open_root(state.path()).unwrap());
            let outer_lock = Arc::new(outer.try_lock().unwrap());
            let event_dir = outer
                .open_or_create_child(&StateKey::new("events").unwrap())
                .unwrap();
            let event_log = DurableEventLog::create(
                &event_dir,
                &StateKey::new("events.v1").unwrap(),
                event_binding(),
                LogFrameKey::from_bytes([0xa8; 32]),
                log_limits(),
                folder_machine(),
            )
            .unwrap();
            let mut events = DurableFolderLog::new(event_log, writer(), signing_key()).unwrap();
            events.ingest(&genesis()).unwrap();

            let content_dir = outer
                .open_or_create_child(&StateKey::new("content").unwrap())
                .unwrap();
            let domain = SyncContentDomain::new(
                folder(),
                event_binding().installation_id(),
                event_binding().generation_id(),
            );
            let store = SyncContentStore::open(
                content_dir,
                SyncContentCrypto::from_bytes(domain, [0xa9; 32]).unwrap(),
                ContentStoreLimits {
                    maximum_stored_bytes: 1 << 20,
                    maximum_objects: 128,
                },
            )
            .unwrap();
            Self {
                state,
                user,
                outer,
                outer_lock,
                events,
                store,
            }
        }

        fn applier(&self) -> DurableFolderApplier {
            DurableFolderApplier::create(
                Arc::clone(&self.outer),
                Arc::clone(&self.outer_lock),
                &StateKey::new("apply").unwrap(),
                &StateKey::new("apply.v1").unwrap(),
                apply_binding(),
                LogFrameKey::from_bytes([0xaa; 32]),
                log_limits(),
                apply_limits(),
                &AuthorizedRoot::open(self.user.path()).unwrap(),
                &self.events,
            )
            .unwrap()
        }

        fn reopen_applier(&self) -> DurableFolderApplier {
            DurableFolderApplier::open(
                Arc::clone(&self.outer),
                Arc::clone(&self.outer_lock),
                &StateKey::new("apply").unwrap(),
                &StateKey::new("apply.v1").unwrap(),
                apply_binding(),
                LogFrameKey::from_bytes([0xaa; 32]),
                log_limits(),
                apply_limits(),
                &AuthorizedRoot::open(self.user.path()).unwrap(),
                &self.events,
                &JobControl::new(),
            )
            .unwrap()
        }
    }

    fn retain_and_publish_existing_file(
        fixture: &mut Fixture,
        path: &str,
        bytes: &[u8],
        mode: u32,
    ) -> PublishedOperation {
        let target = fixture.user.path().join(path);
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(mode)).unwrap();
        let executable = mode & 0o111 != 0;
        let content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
            bytes.len() as u64,
            executable,
        )
        .unwrap();
        let receipt = fixture
            .store
            .retain_stream(content, Cursor::new(bytes), &JobControl::new())
            .unwrap();
        fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire(path).unwrap(),
                    EntryValue::File(content),
                ),
                &receipt,
            )
            .unwrap()
    }

    #[test]
    fn adoption_preserves_ordinary_file_modes_and_is_idempotent() {
        let mut fixture = Fixture::new();
        let plain = retain_and_publish_existing_file(&mut fixture, "plain.txt", b"plain", 0o644);
        let executable = retain_and_publish_existing_file(&mut fixture, "tool.sh", b"tool", 0o755);
        let mut applier = fixture.applier();
        for (operation, path, mode) in [
            (&plain, "plain.txt", 0o644),
            (&executable, "tool.sh", 0o755),
        ] {
            let before = fs::metadata(fixture.user.path().join(path)).unwrap();
            assert_eq!(
                applier
                    .adopt_existing(
                        &fixture.events,
                        operation.id(),
                        operation.event_bytes(),
                        &JobControl::new(),
                    )
                    .unwrap(),
                ApplyOutcome::Applied
            );
            let after = fs::metadata(fixture.user.path().join(path)).unwrap();
            assert_eq!(before.ino(), after.ino());
            assert_eq!(after.permissions().mode() & 0o7777, mode);
            let revision = applier.log.machine().unwrap().revision();
            assert_eq!(
                applier
                    .adopt_existing(
                        &fixture.events,
                        operation.id(),
                        operation.event_bytes(),
                        &JobControl::new(),
                    )
                    .unwrap(),
                ApplyOutcome::Applied
            );
            assert_eq!(applier.log.machine().unwrap().revision(), revision);
        }
        drop(applier);
        let reopened = fixture.reopen_applier();
        assert_eq!(
            reopened.existing_outcome(plain.id()).unwrap(),
            Some(ApplyOutcome::Applied)
        );
        assert_eq!(
            reopened.existing_outcome(executable.id()).unwrap(),
            Some(ApplyOutcome::Applied)
        );
    }

    #[test]
    fn directory_adoption_verifies_only_and_never_creates_an_absent_target() {
        let mut fixture = Fixture::new();
        fs::create_dir(fixture.user.path().join("present-dir")).unwrap();
        fs::set_permissions(
            fixture.user.path().join("present-dir"),
            fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        let present = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("present-dir").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let absent = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("absent-dir").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    present.id(),
                    present.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(
            fs::metadata(fixture.user.path().join("present-dir"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o750
        );
        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    absent.id(),
                    absent.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert!(!fixture.user.path().join("absent-dir").exists());
    }

    #[test]
    fn interrupted_directory_adoption_preserves_a_replaced_entry_as_conflict() {
        let mut fixture = Fixture::new();
        let target = fixture.user.path().join("pending-dir");
        let preserved = fixture.user.path().join("preserved-dir");
        let outside = tempfile::tempdir().unwrap();
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        let operation = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("pending-dir").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        applier.failpoint = Some(ApplyFailpoint::IntentCommitted);
        assert!(matches!(
            applier.adopt_existing(
                &fixture.events,
                operation.id(),
                operation.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::Pending)
        ));
        fs::rename(&target, &preserved).unwrap();
        symlink(outside.path(), &target).unwrap();

        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert!(preserved.is_dir());
        assert!(
            fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(outside.path().is_dir());
        drop(applier);
        let reopened = fixture.reopen_applier();
        assert_eq!(
            reopened.existing_outcome(operation.id()).unwrap(),
            Some(ApplyOutcome::Conflict)
        );
    }

    #[test]
    fn adoption_rejects_final_symlinks_and_special_permission_bits() {
        let mut fixture = Fixture::new();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"outside").unwrap();
        symlink(outside.path(), fixture.user.path().join("linked-file")).unwrap();
        let linked_content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(b"outside").as_bytes()),
            7,
            false,
        )
        .unwrap();
        let linked_receipt = fixture
            .store
            .retain_stream(linked_content, Cursor::new(b"outside"), &JobControl::new())
            .unwrap();
        let linked = fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire("linked-file").unwrap(),
                    EntryValue::File(linked_content),
                ),
                &linked_receipt,
            )
            .unwrap();
        let special =
            retain_and_publish_existing_file(&mut fixture, "special-file", b"special", 0o4755);
        let mut applier = fixture.applier();
        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    linked.id(),
                    linked.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert!(
            fs::symlink_metadata(fixture.user.path().join("linked-file"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(outside.path()).unwrap(), b"outside");
        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    special.id(),
                    special.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert_eq!(
            fs::metadata(fixture.user.path().join("special-file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o4755
        );
    }

    #[test]
    fn adoption_byte_mode_and_inode_races_become_durable_conflicts() {
        #[derive(Clone, Copy)]
        enum Race {
            Bytes,
            Mode,
            Inode,
        }
        for (point, race) in [
            (ApplyMutationPoint::BeforeAdoptionVerification, Race::Bytes),
            (ApplyMutationPoint::BeforeAdoptionReceipt, Race::Mode),
            (ApplyMutationPoint::BeforeAdoptionReceipt, Race::Inode),
        ] {
            let mut fixture = Fixture::new();
            let operation =
                retain_and_publish_existing_file(&mut fixture, "raced", b"original", 0o644);
            let target = fixture.user.path().join("raced");
            let original_inode = fs::metadata(&target).unwrap().ino();
            let hook_target = target.clone();
            let mut applier = fixture.applier();
            applier.mutation_hook = Some((
                point,
                Box::new(move || match race {
                    Race::Bytes => fs::write(&hook_target, b"mutated!").unwrap(),
                    Race::Mode => {
                        fs::set_permissions(&hook_target, fs::Permissions::from_mode(0o600))
                            .unwrap()
                    }
                    Race::Inode => {
                        fs::remove_file(&hook_target).unwrap();
                        fs::write(&hook_target, b"original").unwrap();
                        fs::set_permissions(&hook_target, fs::Permissions::from_mode(0o644))
                            .unwrap();
                    }
                }),
            ));
            assert_eq!(
                applier
                    .adopt_existing(
                        &fixture.events,
                        operation.id(),
                        operation.event_bytes(),
                        &JobControl::new(),
                    )
                    .unwrap(),
                ApplyOutcome::Conflict
            );
            assert!(matches!(
                applier
                    .log
                    .machine()
                    .unwrap()
                    .operation_terminal(operation.id()),
                Some(ApplyTerminal::Conflict(_))
            ));
            if matches!(race, Race::Inode) {
                assert_ne!(fs::metadata(target).unwrap().ino(), original_inode);
            }
            drop(applier);
            let reopened = fixture.reopen_applier();
            assert_eq!(
                reopened.existing_outcome(operation.id()).unwrap(),
                Some(ApplyOutcome::Conflict)
            );
        }
    }

    #[test]
    fn adoption_syncs_the_exact_verified_inode_across_an_a_b_a_name_swap() {
        let mut fixture = Fixture::new();
        let operation =
            retain_and_publish_existing_file(&mut fixture, "sync-race", b"stable", 0o644);
        let target = fixture.user.path().join("sync-race");
        let preserved_a = fixture.user.path().join("preserved-a");
        let preserved_b = fixture.user.path().join("preserved-b");
        let metadata = fs::metadata(&target).unwrap();
        let expected = EntryIdentity::new(metadata.dev(), metadata.ino());

        let before_target = target.clone();
        let before_a = preserved_a.clone();
        let mut applier = fixture.applier();
        applier.mutation_hook = Some((
            ApplyMutationPoint::BeforeAdoptionDescriptorSync,
            Box::new(move || {
                fs::rename(&before_target, &before_a).unwrap();
                fs::write(&before_target, b"stable").unwrap();
                fs::set_permissions(&before_target, fs::Permissions::from_mode(0o644)).unwrap();
            }),
        ));
        let synchronized = Arc::new(std::sync::Mutex::new(None));
        let observed = Arc::clone(&synchronized);
        let after_target = target.clone();
        let after_a = preserved_a.clone();
        let after_b = preserved_b.clone();
        applier.adoption_sync_hook = Some(Box::new(move |identity| {
            *observed.lock().unwrap() = Some(identity);
            fs::rename(&after_target, &after_b).unwrap();
            fs::rename(&after_a, &after_target).unwrap();
        }));

        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert_eq!(*synchronized.lock().unwrap(), Some(expected));
        assert_eq!(fs::read(&target).unwrap(), b"stable");
        assert_eq!(fs::read(&preserved_b).unwrap(), b"stable");
        assert_ne!(
            fs::metadata(&target).unwrap().ino(),
            fs::metadata(&preserved_b).unwrap().ino()
        );
        assert!(matches!(
            applier
                .log
                .machine()
                .unwrap()
                .operation_terminal(operation.id()),
            Some(ApplyTerminal::Conflict(_))
        ));
    }

    #[test]
    fn cancellation_after_adoption_sync_cannot_commit_applied() {
        let mut fixture = Fixture::new();
        let operation =
            retain_and_publish_existing_file(&mut fixture, "cancel-late", b"stable", 0o644);
        let control = Arc::new(JobControl::new());
        let cancel = Arc::clone(&control);
        let mut applier = fixture.applier();
        applier.mutation_hook = Some((
            ApplyMutationPoint::BeforeAdoptionReceipt,
            Box::new(move || cancel.cancel()),
        ));

        assert!(matches!(
            applier.adopt_existing(
                &fixture.events,
                operation.id(),
                operation.event_bytes(),
                &control,
            ),
            Err(ApplyError::Interrupted)
        ));
        assert_eq!(applier.log.machine().unwrap().revision(), 1);
        assert!(
            applier
                .log
                .machine()
                .unwrap()
                .operation_terminal(operation.id())
                .is_none()
        );
        assert_eq!(
            applier
                .adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(applier.log.machine().unwrap().revision(), 2);
    }

    #[test]
    fn transient_file_adoption_io_failures_leave_the_intent_replayable() {
        for point in [
            AdoptionIoPoint::ReadFile,
            AdoptionIoPoint::SyncFile,
            AdoptionIoPoint::InspectFile,
            AdoptionIoPoint::SyncParent,
        ] {
            let mut fixture = Fixture::new();
            let operation =
                retain_and_publish_existing_file(&mut fixture, "io-file", b"stable", 0o644);
            let mut applier = fixture.applier();
            applier.adoption_io_hook = Some((
                point,
                Box::new(|| {
                    Err(ApplyError::Io {
                        operation: "injected adoption I/O",
                        errno: Some(5),
                    })
                }),
            ));

            assert!(matches!(
                applier.adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                ),
                Err(ApplyError::Io { .. })
            ));
            assert_eq!(applier.log.machine().unwrap().revision(), 1);
            assert!(
                applier
                    .log
                    .machine()
                    .unwrap()
                    .operation_terminal(operation.id())
                    .is_none()
            );
            assert_eq!(
                applier
                    .adopt_existing(
                        &fixture.events,
                        operation.id(),
                        operation.event_bytes(),
                        &JobControl::new(),
                    )
                    .unwrap(),
                ApplyOutcome::Applied
            );
            assert_eq!(applier.log.machine().unwrap().revision(), 2);
        }
    }

    #[test]
    fn transient_directory_adoption_io_failures_leave_the_intent_replayable() {
        for point in [
            AdoptionIoPoint::SyncDirectory,
            AdoptionIoPoint::InspectDirectory,
            AdoptionIoPoint::SyncParent,
        ] {
            let mut fixture = Fixture::new();
            fs::create_dir(fixture.user.path().join("io-directory")).unwrap();
            fs::set_permissions(
                fixture.user.path().join("io-directory"),
                fs::Permissions::from_mode(0o750),
            )
            .unwrap();
            let operation = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("io-directory").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let mut applier = fixture.applier();
            applier.adoption_io_hook = Some((
                point,
                Box::new(|| {
                    Err(ApplyError::Io {
                        operation: "injected adoption I/O",
                        errno: Some(5),
                    })
                }),
            ));

            assert!(matches!(
                applier.adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                ),
                Err(ApplyError::Io { .. })
            ));
            assert_eq!(applier.log.machine().unwrap().revision(), 1);
            assert_eq!(
                applier
                    .adopt_existing(
                        &fixture.events,
                        operation.id(),
                        operation.event_bytes(),
                        &JobControl::new(),
                    )
                    .unwrap(),
                ApplyOutcome::Applied
            );
            assert_eq!(applier.log.machine().unwrap().revision(), 2);
            assert_eq!(
                fs::metadata(fixture.user.path().join("io-directory"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o7777,
                0o750
            );
        }
    }

    #[test]
    fn interrupted_adoption_replays_once_and_rejects_wrong_root() {
        let mut fixture = Fixture::new();
        let operation =
            retain_and_publish_existing_file(&mut fixture, "pending-adopt", b"stable", 0o644);
        let mut applier = fixture.applier();
        applier.failpoint = Some(ApplyFailpoint::IntentCommitted);
        assert!(matches!(
            applier.adopt_existing(
                &fixture.events,
                operation.id(),
                operation.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::Pending)
        ));
        assert_eq!(applier.log.machine().unwrap().revision(), 1);
        drop(applier);

        let wrong_root = tempfile::tempdir().unwrap();
        assert!(matches!(
            DurableFolderApplier::open(
                Arc::clone(&fixture.outer),
                Arc::clone(&fixture.outer_lock),
                &StateKey::new("apply").unwrap(),
                &StateKey::new("apply.v1").unwrap(),
                apply_binding(),
                LogFrameKey::from_bytes([0xaa; 32]),
                log_limits(),
                apply_limits(),
                &AuthorizedRoot::open(wrong_root.path()).unwrap(),
                &fixture.events,
                &JobControl::new(),
            ),
            Err(ApplyError::Changed)
        ));

        let mut reopened = fixture.reopen_applier();
        let cancelled = JobControl::new();
        cancelled.cancel();
        assert!(matches!(
            reopened.adopt_existing(
                &fixture.events,
                operation.id(),
                operation.event_bytes(),
                &cancelled,
            ),
            Err(ApplyError::Interrupted)
        ));
        assert_eq!(reopened.log.machine().unwrap().revision(), 1);
        assert_eq!(
            reopened
                .adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(reopened.log.machine().unwrap().revision(), 2);
        let revision = reopened.log.machine().unwrap().revision();
        assert_eq!(
            reopened
                .adopt_existing(
                    &fixture.events,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(reopened.log.machine().unwrap().revision(), revision);
    }

    #[test]
    fn superseded_pending_adoption_never_becomes_applied() {
        let mut fixture = Fixture::new();
        let first = retain_and_publish_existing_file(&mut fixture, "superseded", b"stable", 0o644);
        let mut applier = fixture.applier();
        applier.failpoint = Some(ApplyFailpoint::IntentCommitted);
        assert!(matches!(
            applier.adopt_existing(
                &fixture.events,
                first.id(),
                first.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::Pending)
        ));
        let content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(b"stable").as_bytes()),
            6,
            false,
        )
        .unwrap();
        let receipt = fixture
            .store
            .verify_retained(content, &JobControl::new())
            .unwrap();
        let newer = fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire("superseded").unwrap(),
                    EntryValue::File(content),
                ),
                &receipt,
            )
            .unwrap();
        assert!(matches!(
            applier.adopt_existing(
                &fixture.events,
                first.id(),
                first.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::ProjectionConflict)
        ));
        assert_eq!(applier.log.machine().unwrap().revision(), 1);
        assert_eq!(
            fs::read(fixture.user.path().join("superseded")).unwrap(),
            b"stable"
        );
        assert_ne!(newer.id(), first.id());
    }

    #[test]
    fn promotion_window_substitution_is_preserved_as_a_durable_conflict() {
        let mut fixture = Fixture::new();
        let bytes = b"desired";
        let content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
            bytes.len() as u64,
            false,
        )
        .unwrap();
        let receipt = fixture
            .store
            .retain_stream(content, Cursor::new(bytes), &JobControl::new())
            .unwrap();
        let operation = fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire("promotion-race").unwrap(),
                    EntryValue::File(content),
                ),
                &receipt,
            )
            .unwrap();
        let root = fixture.user.path().to_path_buf();
        let mut applier = fixture.applier();
        applier.mutation_hook = Some((
            ApplyMutationPoint::AfterPromotionValidation,
            Box::new(move || {
                let stage = fs::read_dir(&root)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(".covalent-stage-"))
                    })
                    .unwrap();
                fs::remove_file(&stage).unwrap();
                fs::write(&stage, b"unknown replacement").unwrap();
            }),
        ));
        assert_eq!(
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert_eq!(
            fs::read(fixture.user.path().join("promotion-race")).unwrap(),
            b"unknown replacement"
        );
        let Some(ApplyTerminal::Conflict(conflict)) = applier
            .log
            .machine()
            .unwrap()
            .operation_terminal(operation.id())
        else {
            panic!("substitution must be terminal")
        };
        assert_eq!(conflict.reason(), ConflictReason::FinalChanged);
        drop(applier);
        let reopened = fixture.reopen_applier();
        assert_eq!(
            reopened.existing_outcome(operation.id()).unwrap(),
            Some(ApplyOutcome::Conflict)
        );
    }

    #[test]
    fn real_directory_and_retained_file_apply_without_clobber() {
        let mut fixture = Fixture::new();
        let directory = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("docs").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let bytes = b"private apply content";
        let content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
            bytes.len() as u64,
            false,
        )
        .unwrap();
        let receipt = fixture
            .store
            .retain_stream(content, Cursor::new(bytes), &JobControl::new())
            .unwrap();
        let file = fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire("docs/note.txt").unwrap(),
                    EntryValue::File(content),
                ),
                &receipt,
            )
            .unwrap();
        let mut applier = fixture.applier();
        assert_eq!(
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    directory.id(),
                    directory.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    file.id(),
                    file.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert_eq!(
            fs::read(fixture.user.path().join("docs/note.txt")).unwrap(),
            bytes
        );
        let encrypted_log = fs::read(fixture.state.path().join("apply/apply.v1")).unwrap();
        assert!(
            !encrypted_log
                .windows(b"docs/note.txt".len())
                .any(|part| part == b"docs/note.txt")
        );
        assert!(!encrypted_log.windows(bytes.len()).any(|part| part == bytes));
        assert!(!format!("{applier:?}").contains("note.txt"));
        drop(applier);

        let reopened = fixture.reopen_applier();
        assert_eq!(
            reopened.existing_outcome(file.id()).unwrap(),
            Some(ApplyOutcome::Applied)
        );
    }

    #[test]
    fn incumbent_and_portable_collision_are_preserved() {
        let mut fixture = Fixture::new();
        fs::write(fixture.user.path().join("occupied"), b"sentinel").unwrap();
        let occupied = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("occupied").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        assert_eq!(
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    occupied.id(),
                    occupied.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Conflict
        );
        assert_eq!(
            fs::read(fixture.user.path().join("occupied")).unwrap(),
            b"sentinel"
        );

        fs::create_dir(fixture.user.path().join("cafe\u{301}")).unwrap();
        let colliding = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("caf\u{e9}").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                colliding.id(),
                colliding.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::NameCollision)
        ));
        assert!(fixture.user.path().join("cafe\u{301}").is_dir());
        assert_eq!(fs::read_dir(fixture.user.path()).unwrap().count(), 2);
    }

    #[test]
    fn retry_uses_the_same_pending_intent_at_every_create_boundary() {
        for point in [
            ApplyFailpoint::IntentCommitted,
            ApplyFailpoint::StageDurable,
            ApplyFailpoint::StageReadyCommitted,
            ApplyFailpoint::Promoted,
            ApplyFailpoint::TargetSynced,
        ] {
            let mut fixture = Fixture::new();
            let operation = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("retry").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let mut applier = fixture.applier();
            applier.failpoint = Some(point);
            assert!(matches!(
                applier.apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new()
                ),
                Err(ApplyError::Pending)
            ));
            let existing = applier
                .log
                .machine()
                .unwrap()
                .pending_operation(operation.id())
                .unwrap();
            let before = applier.log.committed_records().unwrap();
            let outcome = applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap();
            if point == ApplyFailpoint::StageDurable {
                assert_eq!(outcome, ApplyOutcome::Pending);
                assert_eq!(applier.log.committed_records().unwrap(), before);
                assert_eq!(
                    applier
                        .log
                        .machine()
                        .unwrap()
                        .pending_operation(operation.id()),
                    Some(existing)
                );
                assert!(!fixture.user.path().join("retry").exists());
                assert_eq!(fs::read_dir(fixture.user.path()).unwrap().count(), 1);
            } else {
                assert_eq!(outcome, ApplyOutcome::Applied);
                assert_eq!(applier.log.committed_records().unwrap(), 3);
                assert!(
                    applier
                        .log
                        .machine()
                        .unwrap()
                        .transaction(existing)
                        .unwrap()
                        .terminal()
                        .is_some()
                );
                assert!(fixture.user.path().join("retry").is_dir());
                assert_eq!(
                    applier
                        .apply(
                            &fixture.events,
                            &fixture.store,
                            operation.id(),
                            operation.event_bytes(),
                            &JobControl::new()
                        )
                        .unwrap(),
                    ApplyOutcome::Applied
                );
                assert_eq!(applier.log.committed_records().unwrap(), 3);
            }
        }
    }

    #[test]
    fn pending_intent_cannot_reopen_or_restage_in_a_different_authorized_root() {
        let mut fixture = Fixture::new();
        let operation = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("original-root-only").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        applier.failpoint = Some(ApplyFailpoint::IntentCommitted);
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                operation.id(),
                operation.event_bytes(),
                &JobControl::new()
            ),
            Err(ApplyError::Pending)
        ));
        drop(applier);
        let different = tempfile::tempdir().unwrap();
        fs::write(different.path().join("keep.txt"), b"unrelated local data").unwrap();
        let log_path = fixture.state.path().join("apply/apply.v1");
        let before = fs::read(&log_path).unwrap();
        let reopened = DurableFolderApplier::open(
            Arc::clone(&fixture.outer),
            Arc::clone(&fixture.outer_lock),
            &StateKey::new("apply").unwrap(),
            &StateKey::new("apply.v1").unwrap(),
            apply_binding(),
            LogFrameKey::from_bytes([0xaa; 32]),
            log_limits(),
            apply_limits(),
            &AuthorizedRoot::open(different.path()).unwrap(),
            &fixture.events,
            &JobControl::new(),
        );
        assert!(matches!(reopened, Err(ApplyError::Changed)));
        assert_eq!(fs::read(&log_path).unwrap(), before);
        assert_eq!(fs::read_dir(different.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read(different.path().join("keep.txt")).unwrap(),
            b"unrelated local data"
        );
        assert_eq!(fs::read_dir(fixture.user.path()).unwrap().count(), 0);
        let mut reopened = fixture.reopen_applier();
        assert_eq!(
            reopened
                .apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new()
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        assert!(fixture.user.path().join("original-root-only").is_dir());
    }

    #[test]
    fn durable_boundaries_reopen_without_clobbering_unproven_stages() {
        for point in [
            ApplyFailpoint::IntentCommitted,
            ApplyFailpoint::StageDurable,
            ApplyFailpoint::StageReadyCommitted,
            ApplyFailpoint::Promoted,
            ApplyFailpoint::TargetSynced,
        ] {
            let mut fixture = Fixture::new();
            let operation = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("boundary").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let mut applier = fixture.applier();
            applier.failpoint = Some(point);
            assert!(matches!(
                applier.apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                ),
                Err(ApplyError::Pending)
            ));
            drop(applier);

            let mut reopened = fixture.reopen_applier();
            let outcomes = reopened
                .reconcile(&fixture.events, &fixture.store, &JobControl::new())
                .unwrap();
            let expected = if point == ApplyFailpoint::StageDurable {
                ApplyOutcome::Pending
            } else {
                ApplyOutcome::Applied
            };
            assert_eq!(outcomes, vec![expected]);
            if point == ApplyFailpoint::StageDurable {
                assert_eq!(fs::read_dir(fixture.user.path()).unwrap().count(), 1);
                assert!(!fixture.user.path().join("boundary").exists());
            }
        }
    }

    #[test]
    fn symlink_parent_and_replaced_root_cannot_redirect_a_create() {
        let mut fixture = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), fixture.user.path().join("link")).unwrap();
        let through_link = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("link/escaped").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                through_link.id(),
                through_link.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::UnsafeParent)
        ));
        assert!(!outside.path().join("escaped").exists());

        let at_root = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("root-target").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let original = fixture.user.path().to_path_buf();
        let moved = original.with_extension("moved-apply-root");
        fs::rename(&original, &moved).unwrap();
        fs::create_dir(&original).unwrap();
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                at_root.id(),
                at_root.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::Changed)
        ));
        assert!(!original.join("root-target").exists());
        assert!(!moved.join("root-target").exists());
        fs::remove_dir(&original).unwrap();
        fs::rename(&moved, &original).unwrap();
    }

    #[test]
    fn terminal_reuse_requires_exact_event_and_current_projection() {
        let mut fixture = Fixture::new();
        let first = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("terminal").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        assert_eq!(
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    first.id(),
                    first.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap(),
            ApplyOutcome::Applied
        );
        let mut wrong = first.event_bytes().to_vec();
        let last = wrong.len() - 1;
        wrong[last] ^= 1;
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                first.id(),
                &wrong,
                &JobControl::new(),
            ),
            Err(ApplyError::UnadmittedOperation)
        ));
        let _newer = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("terminal").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                first.id(),
                first.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::ProjectionConflict)
        ));
    }

    #[test]
    fn invalid_local_names_fail_closed_before_create() {
        let spellings = [
            (std::ffi::OsString::from_vec(vec![0xff, b'x']), true),
            (std::ffi::OsString::from("bad\nname"), false),
        ];
        for (spelling, may_be_rejected_by_filesystem) in spellings {
            let mut fixture = Fixture::new();
            let incumbent = fixture.user.path().join(spelling);
            if let Err(error) = File::create(&incumbent) {
                // macOS filesystems can reject invalid UTF-8 byte sequences
                // before the applier can observe them. Linux accepts this
                // fixture and exercises the fail-closed branch; the control
                // character spelling below is portable and always exercises
                // the same canonical-name rejection.
                if may_be_rejected_by_filesystem {
                    continue;
                }
                panic!("failed to create invalid-name fixture: {error}");
            }
            let operation = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("wanted").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let mut applier = fixture.applier();
            assert!(matches!(
                applier.apply(
                    &fixture.events,
                    &fixture.store,
                    operation.id(),
                    operation.event_bytes(),
                    &JobControl::new(),
                ),
                Err(ApplyError::NameCollision)
            ));
            assert!(!fixture.user.path().join("wanted").exists());
        }
    }

    #[test]
    fn named_parent_swap_is_detected_before_each_mutation_boundary() {
        for point in [
            ApplyMutationPoint::BeforeStageCreate,
            ApplyMutationPoint::BeforePromotion,
        ] {
            let mut fixture = Fixture::new();
            let parent_operation = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("parent").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let mut applier = fixture.applier();
            applier
                .apply(
                    &fixture.events,
                    &fixture.store,
                    parent_operation.id(),
                    parent_operation.event_bytes(),
                    &JobControl::new(),
                )
                .unwrap();
            let child = fixture
                .events
                .publish_local(&OperationBody::new(
                    SyncPath::from_wire("parent/child").unwrap(),
                    EntryValue::Directory,
                ))
                .unwrap();
            let root = fixture.user.path().to_path_buf();
            applier.mutation_hook = Some((
                point,
                Box::new(move || {
                    fs::rename(root.join("parent"), root.join("parent-moved")).unwrap();
                    fs::create_dir(root.join("parent")).unwrap();
                }),
            ));
            assert!(matches!(
                applier.apply(
                    &fixture.events,
                    &fixture.store,
                    child.id(),
                    child.event_bytes(),
                    &JobControl::new(),
                ),
                Err(ApplyError::Changed)
            ));
            assert!(!fixture.user.path().join("parent/child").exists());
            assert!(!fixture.user.path().join("parent-moved/child").exists());
            if point == ApplyMutationPoint::BeforePromotion {
                assert_eq!(
                    fs::read_dir(fixture.user.path().join("parent-moved"))
                        .unwrap()
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn replaced_ready_stage_becomes_durable_conflict_and_is_preserved() {
        let mut fixture = Fixture::new();
        let operation = fixture
            .events
            .publish_local(&OperationBody::new(
                SyncPath::from_wire("stage-target").unwrap(),
                EntryValue::Directory,
            ))
            .unwrap();
        let mut applier = fixture.applier();
        applier.failpoint = Some(ApplyFailpoint::StageReadyCommitted);
        assert!(matches!(
            applier.apply(
                &fixture.events,
                &fixture.store,
                operation.id(),
                operation.event_bytes(),
                &JobControl::new(),
            ),
            Err(ApplyError::Pending)
        ));
        drop(applier);
        let stage = fs::read_dir(fixture.user.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::remove_dir(&stage).unwrap();
        fs::write(&stage, b"unrelated replacement").unwrap();

        let mut reopened = fixture.reopen_applier();
        assert_eq!(
            reopened
                .reconcile(&fixture.events, &fixture.store, &JobControl::new())
                .unwrap(),
            vec![ApplyOutcome::Conflict]
        );
        assert_eq!(fs::read(stage).unwrap(), b"unrelated replacement");
        drop(reopened);
        let reopened = fixture.reopen_applier();
        assert_eq!(
            reopened.existing_outcome(operation.id()).unwrap(),
            Some(ApplyOutcome::Conflict)
        );
    }

    #[test]
    fn cancelled_reopen_never_starts_historical_file_revalidation() {
        let mut fixture = Fixture::new();
        let bytes = vec![0x5a; 256 * 1_024];
        let content = FileContent::new(
            ContentDigest::from_bytes(*blake3::hash(&bytes).as_bytes()),
            bytes.len() as u64,
            false,
        )
        .unwrap();
        let receipt = fixture
            .store
            .retain_stream(content, Cursor::new(&bytes), &JobControl::new())
            .unwrap();
        let operation = fixture
            .events
            .publish_retained(
                &OperationBody::new(
                    SyncPath::from_wire("cancelled-file").unwrap(),
                    EntryValue::File(content),
                ),
                &receipt,
            )
            .unwrap();
        let mut applier = fixture.applier();
        applier
            .apply(
                &fixture.events,
                &fixture.store,
                operation.id(),
                operation.event_bytes(),
                &JobControl::new(),
            )
            .unwrap();
        drop(applier);
        let control = JobControl::new();
        control.cancel();
        assert!(matches!(
            DurableFolderApplier::open(
                Arc::clone(&fixture.outer),
                Arc::clone(&fixture.outer_lock),
                &StateKey::new("apply").unwrap(),
                &StateKey::new("apply.v1").unwrap(),
                apply_binding(),
                LogFrameKey::from_bytes([0xaa; 32]),
                log_limits(),
                apply_limits(),
                &AuthorizedRoot::open(fixture.user.path()).unwrap(),
                &fixture.events,
                &control,
            ),
            Err(ApplyError::Interrupted)
        ));
    }
}
