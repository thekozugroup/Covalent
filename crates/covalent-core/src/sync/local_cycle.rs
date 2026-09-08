//! Serialized first local inventory, retention, publication, and adoption.
//!
//! One complete source inventory is classified before any mutation. Known
//! changes, deletions, concurrent values, or changed historical apply targets
//! defer every new mutation in that cycle. This preserves local observations
//! for later replacement/deletion policy and prevents a scan failure from being
//! interpreted as an up-to-date tree. No peer ingress is provided here.

use std::collections::BTreeMap;
use std::fs::File;
use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, open, openat, statat};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{AuthorizedRoot, JobControl};

use super::apply_unix::{
    ApplyError, ApplyOutcome, DurableFolderApplier, HistoricalApplicationState,
};
use super::body::{EntryValue, FileContent, OperationBody};
use super::content_store::{ContentStoreError, SyncContentStore};
use super::event::{EventEnvelope, EventKind};
use super::path::SyncPath;
use super::publication::{DurableFolderLog, PublicationError};
use super::register::OpId;
use super::source_inventory::{SourceInventoryError, SourceInventoryLimits, scan_source_inventory};

const MAX_LOCAL_CYCLE_REPORT_ENTRIES: usize = 1_000_000;

/// Bounds both the complete scan and the union with accepted missing paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalCycleLimits {
    pub inventory: SourceInventoryLimits,
    pub maximum_report_entries: usize,
    pub maximum_report_path_bytes: usize,
}

/// One path's result after a complete serialized local cycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCycleDisposition {
    /// Current bytes and the exact recorded apply identity remain valid.
    UnchangedApplied,
    /// Existing applied bytes were newly retained without republishing.
    RetainedApplied,
    /// A new local value was retained when needed, published, and adopted.
    PublishedApplied,
    /// An earlier exact accepted value was adopted without republishing.
    ResumedApplied,
    /// A new value was preserved for a later cycle because another path is dirty.
    DeferredNew,
    /// An accepted but unapplied matching value awaits a clean baseline.
    DeferredResume,
    /// Local state differs from a known accepted or historically applied value.
    PendingKnownChange,
    /// An accepted live value is absent locally; this slice never publishes deletion.
    PendingDeletion,
    /// A durable local apply conflict already exists or was observed this cycle.
    Conflict,
    /// The apply journal records a feature this local cycle cannot complete.
    Unsupported,
    /// A retryable apply transaction remains pending and must be reopened/retried.
    Pending,
}

/// A bounded canonical path and its explicit local-cycle disposition.
#[derive(Clone, Eq, PartialEq)]
pub struct LocalCycleEntry {
    path: SyncPath,
    disposition: LocalCycleDisposition,
    operation: Option<OpId>,
}

impl LocalCycleEntry {
    #[must_use]
    pub const fn path(&self) -> &SyncPath {
        &self.path
    }

    #[must_use]
    pub const fn disposition(&self) -> LocalCycleDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn operation(&self) -> Option<OpId> {
        self.operation
    }
}

impl std::fmt::Debug for LocalCycleEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalCycleEntry")
            .field("path_bytes", &self.path.as_str().len())
            .field("disposition", &self.disposition)
            .field("operation", &self.operation)
            .finish()
    }
}

/// A complete result; no partial scan or classification is returned.
pub struct LocalCycleReport {
    entries: Vec<LocalCycleEntry>,
    inventory_bytes_read: u64,
    mutations_deferred: bool,
}

impl LocalCycleReport {
    #[must_use]
    pub fn entries(&self) -> &[LocalCycleEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn inventory_bytes_read(&self) -> u64 {
        self.inventory_bytes_read
    }

    #[must_use]
    pub const fn mutations_deferred(&self) -> bool {
        self.mutations_deferred
    }
}

impl std::fmt::Debug for LocalCycleReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalCycleReport")
            .field("entry_count", &self.entries.len())
            .field("inventory_bytes_read", &self.inventory_bytes_read)
            .field("mutations_deferred", &self.mutations_deferred)
            .finish_non_exhaustive()
    }
}

/// Fixed failures contain no path, file content, record, or OS-provided text.
#[derive(Debug, Error)]
pub enum LocalCycleError {
    #[error("local cycle limits are invalid")]
    InvalidLimits,
    #[error("local cycle report exceeds its configured bound")]
    ResourceLimit,
    #[error("local source changed after its complete inventory")]
    SourceChanged,
    #[error("local source entry could not be reopened safely")]
    SourceUnavailable,
    #[error("local accepted history is inconsistent")]
    InvalidHistory,
    #[error("local cycle requires coordinator reopen before retry")]
    ReopenRequired,
    #[error("local cycle was interrupted")]
    Interrupted,
    #[error(transparent)]
    Inventory(#[from] SourceInventoryError),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Content(#[from] ContentStoreError),
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

#[derive(Clone, Copy)]
enum PlanState {
    New(EntryValue),
    Matching { id: OpId, value: EntryValue },
    Changed(Option<OpId>),
    Missing(Option<OpId>),
}

struct PlannedPath {
    path: SyncPath,
    state: PlanState,
    historical: HistoricalApplicationState,
    disposition: Option<LocalCycleDisposition>,
    operation: Option<OpId>,
}

struct ActionResult {
    disposition: LocalCycleDisposition,
    operation: Option<OpId>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_local_cycle(
    events: &mut DurableFolderLog,
    content: &mut SyncContentStore,
    applier: &mut DurableFolderApplier,
    source_root: &AuthorizedRoot,
    limits: LocalCycleLimits,
    control: &JobControl,
) -> Result<LocalCycleReport, LocalCycleError> {
    validate_limits(limits)?;
    check_control(control)?;
    let inventory = scan_source_inventory(source_root.canonical_path(), limits.inventory, control)?;
    let source = LocalSourceRoot::open(source_root)?;
    // Resolve and inspect every canonical path before any private or event-log
    // mutation. This catches portable-name ambiguity that an NFC-only source
    // inventory deliberately does not classify on its own.
    for (path, value) in inventory.entries() {
        match value {
            EntryValue::File(file) => drop(source.open_file(path, *file, limits, control)?),
            EntryValue::Directory => source.verify_directory(path, limits, control)?,
            EntryValue::Tombstone => return Err(LocalCycleError::InvalidHistory),
        }
    }
    let mut plans = BTreeMap::<SyncPath, PlannedPath>::new();
    let mut report_path_bytes = 0_usize;
    for (path, value) in inventory.entries() {
        reserve_report_path(&plans, &mut report_path_bytes, path, limits)?;
        plans.insert(
            path.clone(),
            PlannedPath {
                path: path.clone(),
                state: PlanState::New(*value),
                historical: HistoricalApplicationState::Untracked,
                disposition: None,
                operation: None,
            },
        );
    }

    events
        .machine()?
        .visit_current_registers(|path, register| -> Result<(), LocalCycleError> {
            let active = register.active().next();
            let state = if register.active_count() == 1 {
                let active = active.ok_or(LocalCycleError::InvalidHistory)?;
                match plans.get(path) {
                    Some(existing) => match existing.state {
                        PlanState::New(observed) if observed == *active.value() => {
                            PlanState::Matching {
                                id: active.id(),
                                value: observed,
                            }
                        }
                        _ => PlanState::Changed(Some(active.id())),
                    },
                    None => PlanState::Missing(Some(active.id())),
                }
            } else if plans.contains_key(path) {
                PlanState::Changed(None)
            } else {
                PlanState::Missing(None)
            };
            if let Some(existing) = plans.get_mut(path) {
                existing.state = state;
            } else {
                reserve_report_path(&plans, &mut report_path_bytes, path, limits)?;
                plans.insert(
                    path.clone(),
                    PlannedPath {
                        path: path.clone(),
                        state,
                        historical: HistoricalApplicationState::Untracked,
                        disposition: None,
                        operation: None,
                    },
                );
            }
            Ok(())
        })?;

    // Classify every path and verify every current historical Applied receipt.
    // Any dirty or unsupported known state makes this cycle observation-only.
    let mut defer_mutations =
        applier.has_pending_create(control)? || applier.has_visible_unapplied_stage(control)?;
    for plan in plans.values_mut() {
        check_control(control)?;
        match plan.state {
            PlanState::New(_) => {}
            PlanState::Matching { id, .. } => {
                plan.operation = Some(id);
                plan.historical = applier.historical_application_state(id)?;
                match plan.historical {
                    HistoricalApplicationState::Applied => {
                        match applier.revalidate_historical_application(id, &plan.path, control) {
                            Ok(()) => {
                                plan.disposition = Some(LocalCycleDisposition::UnchangedApplied);
                            }
                            Err(
                                ApplyError::Changed
                                | ApplyError::UnsafeParent
                                | ApplyError::NameCollision
                                | ApplyError::ProjectionConflict,
                            ) => {
                                plan.disposition = Some(LocalCycleDisposition::PendingKnownChange);
                                defer_mutations = true;
                            }
                            Err(error) => return Err(error.into()),
                        }
                    }
                    HistoricalApplicationState::Conflict => {
                        plan.disposition = Some(LocalCycleDisposition::Conflict);
                        defer_mutations = true;
                    }
                    HistoricalApplicationState::Unsupported => {
                        plan.disposition = Some(LocalCycleDisposition::Unsupported);
                        defer_mutations = true;
                    }
                    HistoricalApplicationState::Pending | HistoricalApplicationState::Untracked => {
                    }
                }
            }
            PlanState::Changed(_) => {
                if let PlanState::Changed(id) = plan.state {
                    plan.operation = id;
                }
                plan.disposition = Some(LocalCycleDisposition::PendingKnownChange);
                defer_mutations = true;
            }
            PlanState::Missing(_) => {
                if let PlanState::Missing(id) = plan.state {
                    plan.operation = id;
                }
                plan.disposition = Some(LocalCycleDisposition::PendingDeletion);
                defer_mutations = true;
            }
        }
    }

    if defer_mutations {
        for plan in plans.values_mut() {
            if plan.disposition.is_none() {
                plan.disposition = Some(match plan.state {
                    PlanState::New(_) => LocalCycleDisposition::DeferredNew,
                    PlanState::Matching { .. } => LocalCycleDisposition::DeferredResume,
                    PlanState::Changed(_) => LocalCycleDisposition::PendingKnownChange,
                    PlanState::Missing(_) => LocalCycleDisposition::PendingDeletion,
                });
            }
        }
        return build_report(plans, inventory.bytes_read(), true);
    }

    // This global check remains the gate for every mutation even though the
    // coordinator may have used its observation-only open path.
    applier.validate_ready(events, control)?;
    let mut runtime_deferred = false;
    for plan in plans.values_mut() {
        check_control(control)?;
        if runtime_deferred {
            plan.disposition = Some(match plan.state {
                PlanState::New(_) => LocalCycleDisposition::DeferredNew,
                PlanState::Matching { .. }
                    if plan.historical == HistoricalApplicationState::Applied =>
                {
                    LocalCycleDisposition::UnchangedApplied
                }
                PlanState::Matching { .. } => LocalCycleDisposition::DeferredResume,
                PlanState::Changed(_) => LocalCycleDisposition::PendingKnownChange,
                PlanState::Missing(_) => LocalCycleDisposition::PendingDeletion,
            });
            continue;
        }
        let result = match plan.state {
            PlanState::New(value) => publish_and_adopt(
                events, content, applier, &source, &plan.path, value, limits, control,
            )?,
            PlanState::Matching { id, value } => match plan.historical {
                HistoricalApplicationState::Applied => {
                    if let EntryValue::File(file) = value {
                        let disposition = match content.verify_retained(file, control) {
                            Ok(_) => LocalCycleDisposition::UnchangedApplied,
                            Err(ContentStoreError::MissingContent) => {
                                retain_source_file(
                                    content, &source, &plan.path, file, limits, control,
                                )?;
                                match applier
                                    .revalidate_historical_application(id, &plan.path, control)
                                {
                                    Ok(()) => LocalCycleDisposition::RetainedApplied,
                                    Err(
                                        ApplyError::Changed
                                        | ApplyError::UnsafeParent
                                        | ApplyError::NameCollision
                                        | ApplyError::ProjectionConflict,
                                    ) => LocalCycleDisposition::PendingKnownChange,
                                    Err(error) => return Err(error.into()),
                                }
                            }
                            Err(error) => return Err(error.into()),
                        };
                        ActionResult {
                            disposition,
                            operation: Some(id),
                        }
                    } else {
                        ActionResult {
                            disposition: LocalCycleDisposition::UnchangedApplied,
                            operation: Some(id),
                        }
                    }
                }
                HistoricalApplicationState::Pending | HistoricalApplicationState::Untracked => {
                    resume_adoption(
                        events, content, applier, &source, &plan.path, id, value, limits, control,
                    )?
                }
                HistoricalApplicationState::Conflict => ActionResult {
                    disposition: LocalCycleDisposition::Conflict,
                    operation: Some(id),
                },
                HistoricalApplicationState::Unsupported => ActionResult {
                    disposition: LocalCycleDisposition::Unsupported,
                    operation: Some(id),
                },
            },
            PlanState::Changed(id) => ActionResult {
                disposition: LocalCycleDisposition::PendingKnownChange,
                operation: id,
            },
            PlanState::Missing(id) => ActionResult {
                disposition: LocalCycleDisposition::PendingDeletion,
                operation: id,
            },
        };
        plan.disposition = Some(result.disposition);
        plan.operation = result.operation;
        if matches!(
            result.disposition,
            LocalCycleDisposition::Conflict
                | LocalCycleDisposition::Unsupported
                | LocalCycleDisposition::Pending
                | LocalCycleDisposition::PendingKnownChange
        ) {
            runtime_deferred = true;
        }
    }
    build_report(plans, inventory.bytes_read(), runtime_deferred)
}

#[allow(clippy::too_many_arguments)]
fn publish_and_adopt(
    events: &mut DurableFolderLog,
    content: &mut SyncContentStore,
    applier: &mut DurableFolderApplier,
    source: &LocalSourceRoot,
    path: &SyncPath,
    value: EntryValue,
    limits: LocalCycleLimits,
    control: &JobControl,
) -> Result<ActionResult, LocalCycleError> {
    let body = OperationBody::new(path.clone(), value);
    let mut receipt = None;
    if let EntryValue::File(file) = value {
        receipt = Some(retain_source_file(
            content, source, path, file, limits, control,
        )?);
    }
    // Retention can take time; recheck every previously Applied current target
    // before making a new accepted operation durable.
    applier.validate_ready(events, control)?;
    let published = match value {
        EntryValue::File(_) => events.publish_retained(
            &body,
            receipt.as_ref().ok_or(LocalCycleError::InvalidHistory)?,
        )?,
        EntryValue::Directory => events.publish_local(&body)?,
        EntryValue::Tombstone => return Err(LocalCycleError::InvalidHistory),
    };
    let id = published.id();
    Ok(ActionResult {
        disposition: map_apply_outcome(
            applier.adopt_existing(events, id, published.event_bytes(), control)?,
            LocalCycleDisposition::PublishedApplied,
        )?,
        operation: Some(id),
    })
}

#[allow(clippy::too_many_arguments)]
fn resume_adoption(
    events: &DurableFolderLog,
    content: &mut SyncContentStore,
    applier: &mut DurableFolderApplier,
    source: &LocalSourceRoot,
    path: &SyncPath,
    id: OpId,
    value: EntryValue,
    limits: LocalCycleLimits,
    control: &JobControl,
) -> Result<ActionResult, LocalCycleError> {
    if let EntryValue::File(file) = value {
        match content.verify_retained(file, control) {
            Ok(_) => {}
            Err(ContentStoreError::MissingContent) => {
                retain_source_file(content, source, path, file, limits, control)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let event = accepted_event(events, id)?;
    Ok(ActionResult {
        disposition: map_apply_outcome(
            applier.adopt_existing(events, id, &event, control)?,
            LocalCycleDisposition::ResumedApplied,
        )?,
        operation: Some(id),
    })
}

fn map_apply_outcome(
    outcome: ApplyOutcome,
    applied: LocalCycleDisposition,
) -> Result<LocalCycleDisposition, LocalCycleError> {
    Ok(match outcome {
        ApplyOutcome::Applied => applied,
        ApplyOutcome::Conflict => LocalCycleDisposition::Conflict,
        ApplyOutcome::Unsupported => LocalCycleDisposition::Unsupported,
        ApplyOutcome::Pending => LocalCycleDisposition::Pending,
    })
}

fn accepted_event(
    events: &DurableFolderLog,
    id: OpId,
) -> Result<Zeroizing<Vec<u8>>, LocalCycleError> {
    let accepted = events
        .machine()?
        .accepted_operation_by_id(id)
        .map_err(|_| LocalCycleError::InvalidHistory)?;
    let encoded =
        EventEnvelope::from_signed_record(EventKind::Operation, accepted.canonical_record())
            .and_then(|event| event.encode())
            .map_err(|_| LocalCycleError::InvalidHistory)?;
    let mut owned = Zeroizing::new(Vec::new());
    owned
        .try_reserve_exact(encoded.as_bytes().len())
        .map_err(|_| LocalCycleError::ResourceLimit)?;
    owned.extend_from_slice(encoded.as_bytes());
    Ok(owned)
}

fn retain_source_file(
    content: &mut SyncContentStore,
    source: &LocalSourceRoot,
    path: &SyncPath,
    expected: FileContent,
    limits: LocalCycleLimits,
    control: &JobControl,
) -> Result<super::content_store::VerifiedContentReceipt, LocalCycleError> {
    run_before_retention_hook();
    let mut opened = source.open_file(path, expected, limits, control)?;
    let receipt = content.retain_stream(expected, &mut opened.file, control)?;
    opened.revalidate(source, path, limits, control)?;
    Ok(receipt)
}

fn build_report(
    plans: BTreeMap<SyncPath, PlannedPath>,
    inventory_bytes_read: u64,
    mutations_deferred: bool,
) -> Result<LocalCycleReport, LocalCycleError> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(plans.len())
        .map_err(|_| LocalCycleError::ResourceLimit)?;
    for (_, plan) in plans {
        entries.push(LocalCycleEntry {
            path: plan.path,
            disposition: plan.disposition.ok_or(LocalCycleError::InvalidHistory)?,
            operation: plan.operation,
        });
    }
    Ok(LocalCycleReport {
        entries,
        inventory_bytes_read,
        mutations_deferred,
    })
}

fn validate_limits(limits: LocalCycleLimits) -> Result<(), LocalCycleError> {
    if limits.maximum_report_entries == 0
        || limits.maximum_report_entries > MAX_LOCAL_CYCLE_REPORT_ENTRIES
        || limits.maximum_report_path_bytes == 0
    {
        return Err(LocalCycleError::InvalidLimits);
    }
    Ok(())
}

fn reserve_report_path(
    plans: &BTreeMap<SyncPath, PlannedPath>,
    path_bytes: &mut usize,
    path: &SyncPath,
    limits: LocalCycleLimits,
) -> Result<(), LocalCycleError> {
    if plans.len() >= limits.maximum_report_entries {
        return Err(LocalCycleError::ResourceLimit);
    }
    *path_bytes = path_bytes
        .checked_add(path.as_str().len())
        .ok_or(LocalCycleError::ResourceLimit)?;
    if *path_bytes > limits.maximum_report_path_bytes {
        return Err(LocalCycleError::ResourceLimit);
    }
    Ok(())
}

struct LocalSourceRoot {
    canonical: std::path::PathBuf,
    descriptor: OwnedFd,
    identity: FileIdentity,
}

struct OpenedSourceFile {
    file: File,
    before: Stat,
    parent_identity: FileIdentity,
    selected_name: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl LocalSourceRoot {
    fn open(root: &AuthorizedRoot) -> Result<Self, LocalCycleError> {
        let descriptor = open(
            root.canonical_path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| LocalCycleError::SourceUnavailable)?;
        let stat = fstat(&descriptor).map_err(|_| LocalCycleError::SourceUnavailable)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(LocalCycleError::SourceUnavailable);
        }
        Ok(Self {
            canonical: root.canonical_path().to_path_buf(),
            descriptor,
            identity: file_identity(&stat),
        })
    }

    fn open_file(
        &self,
        path: &SyncPath,
        expected: FileContent,
        limits: LocalCycleLimits,
        control: &JobControl,
    ) -> Result<OpenedSourceFile, LocalCycleError> {
        let (parent, parent_identity, selected_name) = self.open_parent(path, limits, control)?;
        let descriptor = openat(
            &parent,
            selected_name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| LocalCycleError::SourceChanged)?;
        let before = fstat(&descriptor).map_err(|_| LocalCycleError::SourceUnavailable)?;
        if !valid_source_file(&before, expected) {
            return Err(LocalCycleError::SourceChanged);
        }
        Ok(OpenedSourceFile {
            file: File::from(descriptor),
            before,
            parent_identity,
            selected_name,
        })
    }

    fn verify_directory(
        &self,
        path: &SyncPath,
        limits: LocalCycleLimits,
        control: &JobControl,
    ) -> Result<(), LocalCycleError> {
        let (parent, _, selected_name) = self.open_parent(path, limits, control)?;
        let descriptor = openat(
            &parent,
            selected_name.as_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| LocalCycleError::SourceChanged)?;
        let opened = fstat(&descriptor).map_err(|_| LocalCycleError::SourceUnavailable)?;
        let named = statat(&parent, selected_name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalCycleError::SourceChanged)?;
        if FileType::from_raw_mode(opened.st_mode) != FileType::Directory
            || FileType::from_raw_mode(named.st_mode) != FileType::Directory
            || file_identity(&opened) != file_identity(&named)
        {
            return Err(LocalCycleError::SourceChanged);
        }
        Ok(())
    }

    fn open_parent(
        &self,
        path: &SyncPath,
        limits: LocalCycleLimits,
        control: &JobControl,
    ) -> Result<(OwnedFd, FileIdentity, String), LocalCycleError> {
        self.revalidate()?;
        let mut current = rustix::io::fcntl_dupfd_cloexec(&self.descriptor, 3)
            .map_err(|_| LocalCycleError::SourceUnavailable)?;
        let mut components = path.components().peekable();
        let mut selected_name = None;
        while let Some(component) = components.next() {
            check_control(control)?;
            let selected = resolve_component(&current, component, limits, control)?;
            if components.peek().is_none() {
                selected_name = Some(selected);
                break;
            }
            current = openat(
                &current,
                selected.as_str(),
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| LocalCycleError::SourceChanged)?;
        }
        let name = selected_name.ok_or(LocalCycleError::SourceChanged)?;
        let stat = fstat(&current).map_err(|_| LocalCycleError::SourceUnavailable)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::Directory {
            return Err(LocalCycleError::SourceChanged);
        }
        Ok((current, file_identity(&stat), name))
    }

    fn revalidate(&self) -> Result<(), LocalCycleError> {
        let descriptor = open(
            &self.canonical,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| LocalCycleError::SourceChanged)?;
        let stat = fstat(&descriptor).map_err(|_| LocalCycleError::SourceChanged)?;
        if file_identity(&stat) != self.identity {
            return Err(LocalCycleError::SourceChanged);
        }
        Ok(())
    }
}

impl OpenedSourceFile {
    fn revalidate(
        &self,
        root: &LocalSourceRoot,
        path: &SyncPath,
        limits: LocalCycleLimits,
        control: &JobControl,
    ) -> Result<(), LocalCycleError> {
        check_control(control)?;
        let after = fstat(&self.file).map_err(|_| LocalCycleError::SourceUnavailable)?;
        if !same_file_fingerprint(&self.before, &after) {
            return Err(LocalCycleError::SourceChanged);
        }
        let (parent, identity, selected_name) = root.open_parent(path, limits, control)?;
        if identity != self.parent_identity || selected_name != self.selected_name {
            return Err(LocalCycleError::SourceChanged);
        }
        let named = statat(&parent, selected_name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| LocalCycleError::SourceChanged)?;
        if !same_file_fingerprint(&after, &named) {
            return Err(LocalCycleError::SourceChanged);
        }
        Ok(())
    }
}

fn resolve_component(
    directory: &OwnedFd,
    canonical: &str,
    limits: LocalCycleLimits,
    control: &JobControl,
) -> Result<String, LocalCycleError> {
    let target = SyncPath::from_wire(canonical).map_err(|_| LocalCycleError::InvalidHistory)?;
    let collision_key = target.portable_collision_key();
    let mut selected = None;
    let mut entries = 0_usize;
    let mut name_bytes = 0_usize;
    let mut reader = Dir::read_from(directory).map_err(|_| LocalCycleError::SourceUnavailable)?;
    for entry in &mut reader {
        check_control(control)?;
        let entry = entry.map_err(|_| LocalCycleError::SourceUnavailable)?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        entries = entries
            .checked_add(1)
            .ok_or(LocalCycleError::ResourceLimit)?;
        name_bytes = name_bytes
            .checked_add(raw.len())
            .ok_or(LocalCycleError::ResourceLimit)?;
        if entries > limits.inventory.maximum_entries
            || name_bytes > limits.inventory.maximum_path_bytes
        {
            return Err(LocalCycleError::ResourceLimit);
        }
        let spelling = std::str::from_utf8(raw).map_err(|_| LocalCycleError::SourceUnavailable)?;
        let local =
            SyncPath::from_local(spelling).map_err(|_| LocalCycleError::SourceUnavailable)?;
        if local.portable_collision_key() == collision_key {
            if local != target || selected.is_some() {
                return Err(LocalCycleError::SourceUnavailable);
            }
            selected = Some(spelling.to_owned());
        }
    }
    selected.ok_or(LocalCycleError::SourceChanged)
}

fn valid_source_file(stat: &Stat, expected: FileContent) -> bool {
    let permission_bits = permission_bits(stat.st_mode);
    FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
        && stat.st_nlink == 1
        && stat.st_size >= 0
        && stat.st_size as u64 == expected.byte_length()
        && permission_bits <= 0o777
        && (permission_bits & 0o111 != 0) == expected.executable()
}

fn same_file_fingerprint(left: &Stat, right: &Stat) -> bool {
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

#[allow(clippy::unnecessary_cast)]
fn file_identity(stat: &Stat) -> FileIdentity {
    FileIdentity {
        device: stat.st_dev as u64,
        inode: stat.st_ino as u64,
    }
}

#[allow(clippy::unnecessary_cast)]
fn permission_bits(mode: rustix::fs::RawMode) -> u16 {
    (mode as u32 & 0o7777) as u16
}

fn check_control(control: &JobControl) -> Result<(), LocalCycleError> {
    control.check().map_err(|_| LocalCycleError::Interrupted)
}

#[cfg(test)]
thread_local! {
    static BEFORE_RETENTION_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) fn set_before_retention_hook(hook: impl FnOnce() + 'static) {
    BEFORE_RETENTION_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

fn run_before_retention_hook() {
    #[cfg(test)]
    BEFORE_RETENTION_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}
