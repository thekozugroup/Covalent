//! Authenticated initialization and strict reopening for one local sync folder.
//!
//! This coordinator serializes ownership of the private event, content, and
//! apply stores. Construction exposes no intermediate signing, retention, or
//! filesystem-apply capability before the first-only ApplyRootReady record is
//! durable. Matching a configured transport identity checks immutable local
//! pins; it does not prove possession by a live network session.

use std::sync::Arc;

use covalent_protocol::DeviceId;
use ed25519_dalek::VerifyingKey;
use thiserror::Error;

use crate::{AuthorizedRoot, JobControl, JobState, KeyProtector};

use super::apply_record::ApplyInitialization;
use super::apply_unix::{ApplyError, ApplyInitializationState, ApplyLimits, DurableFolderApplier};
use super::authority_config::{AuthorityConfigError, FolderAuthorityConfig, PinnedFolderAuthority};
use super::content_store::{ContentStoreError, ContentStoreLimits, SyncContentStore};
use super::event::{EventEnvelope, EventKind};
use super::event_log::{DurableEventLog, EventLogError, EventLogLimits};
use super::installation::{InstallationError, SyncInstallation, SyncInstallationParts};
use super::local_cycle::{LocalCycleError, LocalCycleLimits, LocalCycleReport, run_local_cycle};
use super::log_frame::{LogBinding, LogFileKind};
use super::machine::{FolderEventMachine, FolderMachineLimits};
use super::membership::{MemberGrant, MemberRole, encode_signed_epoch};
use super::publication::{DurableFolderLog, PublicationError};
use super::state_dir::{
    PrivateStateDir, PrivateStateEntryKind, PrivateStateInventoryName, PrivateStateLock,
    StateDirError, StateKey,
};

const AUTHORITY_FILE: &str = "authority.v1";
const INSTALLATION_FILE: &str = "installation.v1";
const EVENTS_DIR: &str = "events";
const EVENTS_FILE: &str = "events.v1";
const CONTENT_DIR: &str = "content";
const APPLY_DIR: &str = "apply";
const APPLY_FILE: &str = "apply.v1";
const INVENTORY_ENTRIES: usize = 16;
const INVENTORY_KEY_BYTES: u64 = 1_024;

/// Finite storage and replay bounds for coordinator initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FolderCoordinatorLimits {
    pub event_log: EventLogLimits,
    pub folder_machine: FolderMachineLimits,
    pub content_store: ContentStoreLimits,
    pub apply_log: EventLogLimits,
    pub apply: ApplyLimits,
}

/// Authority chosen by the authenticated setup workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FolderAuthoritySelection(Option<PinnedFolderAuthority>);

impl FolderAuthoritySelection {
    /// Uses the fresh installation's protected writer key as initial authority.
    #[must_use]
    pub const fn local_owner() -> Self {
        Self(None)
    }

    /// Uses a separately authenticated remote authority for a joining writer.
    #[must_use]
    pub const fn pinned(authority: PinnedFolderAuthority) -> Self {
        Self(Some(authority))
    }
}

/// Fixed initialization errors do not expose paths, keys, or record bytes.
#[derive(Debug, Error)]
pub enum FolderCoordinatorError {
    #[error("folder coordinator private topology is unexpected")]
    UnexpectedState,
    #[error("folder coordinator is not durably ready")]
    NotReady,
    #[error("folder coordinator authority differs from configured setup")]
    WrongAuthority,
    #[error("folder coordinator initial history is invalid")]
    InvalidInitialHistory,
    #[error("folder coordinator initialization was interrupted")]
    Interrupted,
    #[error(transparent)]
    Installation(#[from] InstallationError),
    #[error(transparent)]
    Authority(#[from] AuthorityConfigError),
    #[error(transparent)]
    State(#[from] StateDirError),
    #[error(transparent)]
    EventLog(#[from] EventLogError),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Content(#[from] ContentStoreError),
    #[error(transparent)]
    Apply(#[from] ApplyError),
}

/// A fully ready, lifetime-serialized local folder installation.
///
/// Fields are ordered so apply, content, and event child locks release before
/// the retained installation lock. This slice intentionally exposes only
/// read-only readiness state; later runtime methods must preserve this order.
pub struct FolderCoordinator {
    applier: DurableFolderApplier,
    content: SyncContentStore,
    events: DurableFolderLog,
    source_root: AuthorizedRoot,
    authority: FolderAuthorityConfig,
    outer: Arc<PrivateStateDir>,
    outer_lock: Arc<PrivateStateLock>,
}

impl std::fmt::Debug for FolderCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FolderCoordinator")
            .field("ready", &true)
            .finish_non_exhaustive()
    }
}

impl FolderCoordinator {
    /// Initializes or resumes only a recognized setup prefix.
    ///
    /// Authority creation is allowed solely in the pristine installation. A
    /// present Ready marker switches to strict ready-state validation; unknown
    /// objects, corrupt records, and pre-ready apply operations are preserved
    /// and rejected.
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_or_resume(
        installation: SyncInstallation,
        authority_selection: FolderAuthoritySelection,
        transport_device: DeviceId,
        transport_key: VerifyingKey,
        protector: &dyn KeyProtector,
        root: &AuthorizedRoot,
        limits: FolderCoordinatorLimits,
        control: &JobControl,
    ) -> Result<Self, FolderCoordinatorError> {
        check_control(control)?;
        let parts = installation.into_parts()?;
        let expected_authority = selected_authority(&parts, authority_selection)?;
        let initial_root = root_inventory(&parts)?;
        require_initial_root_prefix(&initial_root)?;
        let authority = if has_entry(
            &initial_root,
            AUTHORITY_FILE,
            PrivateStateEntryKind::RegularFile,
        ) {
            FolderAuthorityConfig::open(&parts, protector)?
        } else {
            if !is_pristine_installation(&initial_root) {
                return Err(FolderCoordinatorError::UnexpectedState);
            }
            FolderAuthorityConfig::create(
                &parts,
                expected_authority,
                transport_device,
                transport_key,
                protector,
            )?
        };
        validate_setup(
            &authority,
            expected_authority,
            transport_device,
            transport_key,
        )?;
        check_control(control)?;
        assemble(
            parts,
            authority,
            transport_device,
            transport_key,
            root,
            limits,
            control,
            OpenMode::Initialize,
        )
    }

    /// Opens only a complete topology containing a valid Ready marker.
    /// Missing state is never recreated by this entry point.
    #[allow(clippy::too_many_arguments)]
    pub fn open_ready(
        installation: SyncInstallation,
        authority_selection: FolderAuthoritySelection,
        transport_device: DeviceId,
        transport_key: VerifyingKey,
        protector: &dyn KeyProtector,
        root: &AuthorizedRoot,
        limits: FolderCoordinatorLimits,
        control: &JobControl,
    ) -> Result<Self, FolderCoordinatorError> {
        check_control(control)?;
        let parts = installation.into_parts()?;
        let expected_authority = selected_authority(&parts, authority_selection)?;
        let inventory = root_inventory(&parts)?;
        require_complete_root(&inventory)?;
        let authority = FolderAuthorityConfig::open(&parts, protector)?;
        validate_setup(
            &authority,
            expected_authority,
            transport_device,
            transport_key,
        )?;
        assemble(
            parts,
            authority,
            transport_device,
            transport_key,
            root,
            limits,
            control,
            OpenMode::Ready,
        )
    }

    /// Returns replay-derived accepted history without a mutable log handle.
    pub fn events(&self) -> Result<&FolderEventMachine, FolderCoordinatorError> {
        Ok(self.events.machine()?)
    }

    /// Returns authenticated local content usage established during open.
    pub fn content_usage(
        &self,
    ) -> Result<super::content_store::ContentStoreUsage, FolderCoordinatorError> {
        Ok(self.content.usage()?)
    }

    /// Returns the immutable authority identity authenticated during startup.
    #[must_use]
    pub const fn authority(&self) -> PinnedFolderAuthority {
        self.authority.authority()
    }

    /// Confirms that the retained child/root owners remain live.
    pub fn validate_ready(&self, control: &JobControl) -> Result<(), FolderCoordinatorError> {
        check_control(control)?;
        self.outer.sync(&self.outer_lock)?;
        self.applier.validate_ready(&self.events, control)?;
        let _ = self.content.usage()?;
        Ok(())
    }

    /// Captures one complete local-only inventory before any future peer ingress.
    ///
    /// The cycle retains and publishes safe new values only after every known
    /// applied target is still valid. Dirty or missing known paths return a
    /// complete observation-only report and defer all mutations.
    pub fn capture_local_cycle(
        &mut self,
        limits: LocalCycleLimits,
        control: &JobControl,
    ) -> Result<LocalCycleReport, LocalCycleError> {
        run_local_cycle(
            &mut self.events,
            &mut self.content,
            &mut self.applier,
            &self.source_root,
            limits,
            control,
        )
    }
}

#[derive(Clone, Copy)]
enum OpenMode {
    Initialize,
    Ready,
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    parts: SyncInstallationParts,
    authority: FolderAuthorityConfig,
    transport_device: DeviceId,
    transport_key: VerifyingKey,
    root: &AuthorizedRoot,
    limits: FolderCoordinatorLimits,
    control: &JobControl,
    mode: OpenMode,
) -> Result<FolderCoordinator, FolderCoordinatorError> {
    let SyncInstallationParts {
        directory,
        lock,
        folder_id,
        installation_id,
        generation_id,
        writer_id,
        signing_key,
        event_log_key,
        apply_log_key,
        content_crypto,
    } = parts;
    let outer = Arc::new(directory);
    let outer_lock = Arc::new(lock);
    let event_binding = LogBinding::new(
        folder_id,
        installation_id,
        generation_id,
        LogFileKind::FolderEvents,
    );
    let apply_binding = LogBinding::new(
        folder_id,
        installation_id,
        generation_id,
        LogFileKind::Apply,
    );
    let setup = *authority.setup_binding().as_bytes();
    let owner = authority.authority().writer_id() == writer_id;
    let owner_genesis = if owner {
        Some(owner_genesis_record_with_key(
            &authority,
            folder_id,
            writer_id,
            &signing_key,
            (transport_device, transport_key),
        )?)
    } else {
        None
    };
    let initialization = match owner_genesis.as_ref() {
        Some(record) => ApplyInitialization::OwnerGenesis(
            super::membership::EpochDigest::from_bytes(*blake3::hash(record).as_bytes()),
        ),
        None => ApplyInitialization::AwaitingBootstrap,
    };
    let current_root = inventory(&outer, &outer_lock)?;
    let content_already_present =
        has_entry(&current_root, CONTENT_DIR, PrivateStateEntryKind::Directory);
    let apply_already_present =
        has_entry(&current_root, APPLY_DIR, PrivateStateEntryKind::Directory);

    check_control(control)?;
    let event_directory = match mode {
        OpenMode::Initialize => outer.open_or_create_child(&key(EVENTS_DIR)?)?,
        OpenMode::Ready => outer.open_child(&key(EVENTS_DIR)?)?,
    };
    let event_child_mode =
        if matches!(mode, OpenMode::Ready) || content_already_present || apply_already_present {
            OpenMode::Ready
        } else {
            OpenMode::Initialize
        };
    let event_exists = log_entry_state(&event_directory, EVENTS_FILE, event_child_mode)?;
    let machine = FolderEventMachine::new(authority.machine_config(limits.folder_machine))
        .map_err(|_| FolderCoordinatorError::InvalidInitialHistory)?;
    let event_log = match (mode, event_exists) {
        (OpenMode::Initialize, false) => DurableEventLog::create(
            &event_directory,
            &key(EVENTS_FILE)?,
            event_binding,
            event_log_key,
            limits.event_log,
            machine,
        )?,
        (_, true) => DurableEventLog::open_with_control(
            &event_directory,
            &key(EVENTS_FILE)?,
            event_binding,
            event_log_key,
            limits.event_log,
            machine,
            control,
        )?,
        (OpenMode::Ready, false) => return Err(FolderCoordinatorError::NotReady),
    };
    let mut event_records = event_log.committed_records()?;
    let mut events = DurableFolderLog::new(event_log, writer_id, signing_key)?;
    if owner
        && event_records == 0
        && matches!(mode, OpenMode::Initialize)
        && !content_already_present
        && !apply_already_present
    {
        let record = owner_genesis
            .as_ref()
            .ok_or(FolderCoordinatorError::InvalidInitialHistory)?;
        let genesis = EventEnvelope::from_signed_record(EventKind::MembershipEpoch, record)
            .map_err(|_| FolderCoordinatorError::InvalidInitialHistory)?
            .encode()
            .map_err(|_| FolderCoordinatorError::InvalidInitialHistory)?;
        events.ingest(genesis.as_bytes())?;
        event_records = event_records
            .checked_add(1)
            .ok_or(FolderCoordinatorError::InvalidInitialHistory)?;
    } else if owner && event_records == 0 {
        return Err(FolderCoordinatorError::InvalidInitialHistory);
    }

    check_control(control)?;
    let content_directory = match mode {
        OpenMode::Initialize => outer.open_or_create_child(&key(CONTENT_DIR)?)?,
        OpenMode::Ready => outer.open_child(&key(CONTENT_DIR)?)?,
    };
    if matches!(mode, OpenMode::Initialize) && !apply_already_present {
        require_initial_content_prefix(&content_directory)?;
    }
    let content = match (mode, apply_already_present) {
        (OpenMode::Initialize, false) => {
            SyncContentStore::open(content_directory, content_crypto, limits.content_store)?
        }
        (OpenMode::Ready, _) | (OpenMode::Initialize, true) => SyncContentStore::open_existing(
            content_directory,
            content_crypto,
            limits.content_store,
        )?,
    };

    check_control(control)?;
    let apply_exists = match mode {
        OpenMode::Initialize => {
            if !has_top_directory(&outer, &outer_lock, APPLY_DIR)? {
                false
            } else {
                let directory = outer.open_child(&key(APPLY_DIR)?)?;
                log_entry_state(&directory, APPLY_FILE, mode)?
            }
        }
        OpenMode::Ready => {
            let directory = outer.open_child(&key(APPLY_DIR)?)?;
            log_entry_state(&directory, APPLY_FILE, mode)?
        }
    };

    let applier = if !apply_exists {
        if matches!(mode, OpenMode::Ready) {
            return Err(FolderCoordinatorError::NotReady);
        }
        require_initial_state(&events, &content, owner, initialization, event_records)?;
        outer.sync(&outer_lock)?;
        DurableFolderApplier::initialize_new_ready(
            Arc::clone(&outer),
            Arc::clone(&outer_lock),
            &key(APPLY_DIR)?,
            &key(APPLY_FILE)?,
            apply_binding,
            apply_log_key,
            limits.apply_log,
            limits.apply,
            setup,
            initialization,
            root,
            &events,
            control,
        )?
    } else {
        match DurableFolderApplier::open_initialization_observing(
            Arc::clone(&outer),
            Arc::clone(&outer_lock),
            &key(APPLY_DIR)?,
            &key(APPLY_FILE)?,
            apply_binding,
            apply_log_key,
            limits.apply_log,
            limits.apply,
            setup,
            initialization,
            root,
            &events,
            control,
        )? {
            ApplyInitializationState::Ready(applier) => applier,
            ApplyInitializationState::Empty(applier) => {
                if matches!(mode, OpenMode::Ready) {
                    return Err(FolderCoordinatorError::NotReady);
                }
                require_initial_state(&events, &content, owner, initialization, event_records)?;
                outer.sync(&outer_lock)?;
                applier.commit_ready(control)?
            }
        }
    };
    check_control(control)?;
    Ok(FolderCoordinator {
        applier,
        content,
        events,
        source_root: root.clone(),
        authority,
        outer,
        outer_lock,
    })
}

fn validate_setup(
    config: &FolderAuthorityConfig,
    expected: PinnedFolderAuthority,
    transport_device: DeviceId,
    transport_key: VerifyingKey,
) -> Result<(), FolderCoordinatorError> {
    if config.authority() != expected {
        return Err(FolderCoordinatorError::WrongAuthority);
    }
    config.require_local_transport(transport_device, transport_key)?;
    Ok(())
}

fn selected_authority(
    parts: &SyncInstallationParts,
    selection: FolderAuthoritySelection,
) -> Result<PinnedFolderAuthority, FolderCoordinatorError> {
    match selection.0 {
        None => PinnedFolderAuthority::new(parts.writer_id, parts.signing_key.verifying_key())
            .map_err(FolderCoordinatorError::Authority),
        Some(authority) => Ok(authority),
    }
}

fn require_initial_state(
    events: &DurableFolderLog,
    content: &SyncContentStore,
    owner: bool,
    initialization: ApplyInitialization,
    event_records: u64,
) -> Result<(), FolderCoordinatorError> {
    let machine = events.machine()?;
    match (owner, initialization, machine.current_head()) {
        (true, ApplyInitialization::OwnerGenesis(expected), Some(head))
            if head.epoch() == 1
                && head.digest() == expected
                && machine.current_frontier().actor_count() == 0
                && event_records == 1 => {}
        (false, ApplyInitialization::AwaitingBootstrap, None)
            if machine.current_frontier().actor_count() == 0 && event_records == 0 => {}
        _ => return Err(FolderCoordinatorError::InvalidInitialHistory),
    }
    let usage = content.usage()?;
    if usage.objects != 0 || usage.stored_bytes != 0 {
        return Err(FolderCoordinatorError::UnexpectedState);
    }
    Ok(())
}

fn owner_genesis_record_with_key(
    authority: &FolderAuthorityConfig,
    folder: super::ids::FolderId,
    writer: super::ids::WriterId,
    signing_key: &ed25519_dalek::SigningKey,
    transport: (DeviceId, VerifyingKey),
) -> Result<Vec<u8>, FolderCoordinatorError> {
    let grant = MemberGrant::new(
        writer,
        signing_key.verifying_key(),
        transport.0,
        transport.1,
        MemberRole::ReadWrite,
    )
    .map_err(|_| FolderCoordinatorError::InvalidInitialHistory)?;
    encode_signed_epoch(
        signing_key,
        folder,
        1,
        authority.authority().writer_id(),
        None,
        &[grant],
        None,
        &[],
        &[],
        &[],
        &[],
    )
    .map_err(|_| FolderCoordinatorError::InvalidInitialHistory)
}

fn root_inventory(
    parts: &SyncInstallationParts,
) -> Result<Vec<(String, PrivateStateEntryKind)>, FolderCoordinatorError> {
    inventory(parts.directory(), parts.root_lock())
}

fn inventory(
    directory: &PrivateStateDir,
    lock: &PrivateStateLock,
) -> Result<Vec<(String, PrivateStateEntryKind)>, FolderCoordinatorError> {
    Ok(directory
        .inventory(lock, INVENTORY_ENTRIES, INVENTORY_KEY_BYTES)?
        .entries()
        .iter()
        .map(|entry| {
            let name = match entry.name() {
                PrivateStateInventoryName::WriterLock => "writer.lock".to_owned(),
                PrivateStateInventoryName::State(key) => key.as_str().to_owned(),
            };
            (name, entry.kind())
        })
        .collect())
}

fn has_entry(
    entries: &[(String, PrivateStateEntryKind)],
    name: &str,
    kind: PrivateStateEntryKind,
) -> bool {
    entries
        .iter()
        .any(|(candidate, candidate_kind)| candidate == name && *candidate_kind == kind)
}

fn is_pristine_installation(entries: &[(String, PrivateStateEntryKind)]) -> bool {
    entries.len() == 2
        && has_entry(entries, "writer.lock", PrivateStateEntryKind::WriterLock)
        && has_entry(
            entries,
            INSTALLATION_FILE,
            PrivateStateEntryKind::RegularFile,
        )
}

fn require_initial_root_prefix(
    entries: &[(String, PrivateStateEntryKind)],
) -> Result<(), FolderCoordinatorError> {
    let allowed = [
        ("writer.lock", PrivateStateEntryKind::WriterLock),
        (INSTALLATION_FILE, PrivateStateEntryKind::RegularFile),
        (AUTHORITY_FILE, PrivateStateEntryKind::RegularFile),
        (EVENTS_DIR, PrivateStateEntryKind::Directory),
        (CONTENT_DIR, PrivateStateEntryKind::Directory),
        (APPLY_DIR, PrivateStateEntryKind::Directory),
    ];
    if entries
        .iter()
        .any(|(name, kind)| !allowed.contains(&(name.as_str(), *kind)))
        || !has_entry(entries, "writer.lock", PrivateStateEntryKind::WriterLock)
        || !has_entry(
            entries,
            INSTALLATION_FILE,
            PrivateStateEntryKind::RegularFile,
        )
        || (has_entry(entries, CONTENT_DIR, PrivateStateEntryKind::Directory)
            && !has_entry(entries, EVENTS_DIR, PrivateStateEntryKind::Directory))
        || (has_entry(entries, APPLY_DIR, PrivateStateEntryKind::Directory)
            && !has_entry(entries, CONTENT_DIR, PrivateStateEntryKind::Directory))
    {
        return Err(FolderCoordinatorError::UnexpectedState);
    }
    Ok(())
}

fn require_complete_root(
    entries: &[(String, PrivateStateEntryKind)],
) -> Result<(), FolderCoordinatorError> {
    require_initial_root_prefix(entries)?;
    if entries.len() != 6
        || !has_entry(entries, AUTHORITY_FILE, PrivateStateEntryKind::RegularFile)
        || !has_entry(entries, EVENTS_DIR, PrivateStateEntryKind::Directory)
        || !has_entry(entries, CONTENT_DIR, PrivateStateEntryKind::Directory)
        || !has_entry(entries, APPLY_DIR, PrivateStateEntryKind::Directory)
    {
        return Err(FolderCoordinatorError::NotReady);
    }
    Ok(())
}

fn has_top_directory(
    outer: &PrivateStateDir,
    lock: &PrivateStateLock,
    name: &str,
) -> Result<bool, FolderCoordinatorError> {
    Ok(has_entry(
        &inventory(outer, lock)?,
        name,
        PrivateStateEntryKind::Directory,
    ))
}

fn log_entry_state(
    directory: &PrivateStateDir,
    file: &str,
    mode: OpenMode,
) -> Result<bool, FolderCoordinatorError> {
    let lock = match mode {
        OpenMode::Initialize => directory.try_lock()?,
        OpenMode::Ready => directory.try_lock_existing()?,
    };
    let entries = inventory(directory, &lock)?;
    let file_present = has_entry(&entries, file, PrivateStateEntryKind::RegularFile);
    if entries.iter().any(|(name, kind)| {
        !((name == "writer.lock" && *kind == PrivateStateEntryKind::WriterLock)
            || (name == file && *kind == PrivateStateEntryKind::RegularFile))
    }) {
        return Err(FolderCoordinatorError::UnexpectedState);
    }
    if matches!(mode, OpenMode::Ready) && (!file_present || entries.len() != 2) {
        return Err(FolderCoordinatorError::NotReady);
    }
    Ok(file_present)
}

fn require_initial_content_prefix(
    directory: &PrivateStateDir,
) -> Result<(), FolderCoordinatorError> {
    let lock = directory.try_lock()?;
    let entries = inventory(directory, &lock)?;
    let allowed = [
        ("writer.lock", PrivateStateEntryKind::WriterLock),
        ("chunks", PrivateStateEntryKind::Directory),
        ("manifests", PrivateStateEntryKind::Directory),
        ("staging", PrivateStateEntryKind::Directory),
    ];
    if entries
        .iter()
        .any(|(name, kind)| !allowed.contains(&(name.as_str(), *kind)))
        || !has_entry(&entries, "writer.lock", PrivateStateEntryKind::WriterLock)
        || (has_entry(&entries, "manifests", PrivateStateEntryKind::Directory)
            && !has_entry(&entries, "chunks", PrivateStateEntryKind::Directory))
        || (has_entry(&entries, "staging", PrivateStateEntryKind::Directory)
            && !has_entry(&entries, "manifests", PrivateStateEntryKind::Directory))
    {
        return Err(FolderCoordinatorError::UnexpectedState);
    }
    for child in ["chunks", "manifests", "staging"] {
        if has_entry(&entries, child, PrivateStateEntryKind::Directory) {
            let child_directory = directory.open_child(&key(child)?)?;
            let child_lock = child_directory.try_lock()?;
            let child_entries = inventory(&child_directory, &child_lock)?;
            if child_entries.len() != 1
                || !has_entry(
                    &child_entries,
                    "writer.lock",
                    PrivateStateEntryKind::WriterLock,
                )
            {
                return Err(FolderCoordinatorError::UnexpectedState);
            }
        }
    }
    Ok(())
}

fn key(value: &str) -> Result<StateKey, FolderCoordinatorError> {
    StateKey::new(value).map_err(|_| FolderCoordinatorError::UnexpectedState)
}

fn check_control(control: &JobControl) -> Result<(), FolderCoordinatorError> {
    if control.state() == JobState::Running {
        Ok(())
    } else {
        Err(FolderCoordinatorError::Interrupted)
    }
}

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;
