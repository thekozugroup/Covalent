use std::fs;
use std::os::unix::fs::PermissionsExt as _;

use ed25519_dalek::SigningKey;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::StaticKeyProtector;
use crate::sync::apply_machine::ApplyMachineLimits;
use crate::sync::apply_unix::set_after_ready_append_hook;
use crate::sync::ids::{FolderId, WriterId};
use crate::sync::local_cycle::set_before_retention_hook;
use crate::sync::local_cycle::{LocalCycleDisposition, LocalCycleError, LocalCycleLimits};
use crate::sync::path::SyncPath;
use crate::sync::source_inventory::SourceInventoryLimits;

fn folder() -> FolderId {
    FolderId::from_uuid(Uuid::from_u128(0xc001))
}

fn transport() -> (DeviceId, VerifyingKey) {
    (
        DeviceId::from_uuid(Uuid::from_u128(0xc002)),
        SigningKey::from_bytes(&[0xc3; 32]).verifying_key(),
    )
}

fn protector() -> StaticKeyProtector {
    StaticKeyProtector::new(1, [0xc4; 32]).unwrap()
}

fn private_dir() -> TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

fn limits() -> FolderCoordinatorLimits {
    FolderCoordinatorLimits {
        event_log: EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records: 128,
        },
        folder_machine: FolderMachineLimits {
            maximum_index_bytes: 1 << 20,
            maximum_operations: 128,
            maximum_paths: 128,
            maximum_membership_epochs: 128,
            maximum_pending_evidence_bytes: 1 << 18,
            maximum_pending_evidence_records: 128,
        },
        content_store: ContentStoreLimits {
            maximum_stored_bytes: 1 << 20,
            maximum_objects: 128,
        },
        apply_log: EventLogLimits {
            maximum_bytes: 1 << 20,
            maximum_records: 128,
        },
        apply: ApplyLimits {
            machine: ApplyMachineLimits {
                maximum_records: 128,
                maximum_retained_bytes: 1 << 20,
            },
            maximum_directory_entries: 128,
            maximum_directory_name_bytes: 1 << 16,
            maximum_revalidation_bytes: 1 << 20,
        },
    }
}

fn cycle_limits() -> LocalCycleLimits {
    LocalCycleLimits {
        inventory: SourceInventoryLimits {
            maximum_entries: 128,
            maximum_path_bytes: 1 << 16,
            maximum_read_bytes: 1 << 20,
            maximum_depth: 16,
        },
        maximum_report_entries: 128,
        maximum_report_path_bytes: 1 << 16,
    }
}

fn ready_coordinator(state: &TempDir, user: &TempDir) -> FolderCoordinator {
    let (device, key) = transport();
    FolderCoordinator::initialize_or_resume(
        create_installation(state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap()
}

fn reopen_coordinator(state: &TempDir, user: &TempDir) -> FolderCoordinator {
    let (device, key) = transport();
    FolderCoordinator::open_ready(
        reopen_installation(state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap()
}

fn dispositions(
    report: &crate::sync::local_cycle::LocalCycleReport,
) -> Vec<(&str, LocalCycleDisposition)> {
    report
        .entries()
        .iter()
        .map(|entry| (entry.path().as_str(), entry.disposition()))
        .collect()
}

fn create_installation(state: &TempDir) -> SyncInstallation {
    SyncInstallation::create(
        PrivateStateDir::open_root(state.path()).unwrap(),
        folder(),
        &protector(),
    )
    .unwrap()
}

fn reopen_installation(state: &TempDir) -> SyncInstallation {
    SyncInstallation::open(
        PrivateStateDir::open_root(state.path()).unwrap(),
        folder(),
        &protector(),
    )
    .unwrap()
}

#[test]
fn owner_initialization_is_ready_once_and_reopens_without_recreating_state() {
    let state = private_dir();
    let user = private_dir();
    let installation = create_installation(&state);
    let writer = installation.writer_id();
    let (device, key) = transport();
    let coordinator = FolderCoordinator::initialize_or_resume(
        installation,
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    assert_eq!(coordinator.authority().writer_id(), writer);
    assert_eq!(
        coordinator
            .events()
            .unwrap()
            .current_head()
            .unwrap()
            .epoch(),
        1
    );
    assert_eq!(coordinator.content_usage().unwrap().objects, 0);
    coordinator.validate_ready(&JobControl::new()).unwrap();
    assert!(
        SyncInstallation::open(
            PrivateStateDir::open_root(state.path()).unwrap(),
            folder(),
            &protector(),
        )
        .is_err()
    );
    drop(coordinator);

    let reopened = FolderCoordinator::open_ready(
        reopen_installation(&state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    assert_eq!(
        reopened.events().unwrap().current_head().unwrap().epoch(),
        1
    );
}

#[test]
fn remote_authority_joiner_becomes_ready_with_no_fabricated_history() {
    let state = private_dir();
    let user = private_dir();
    let remote_writer = WriterId::from_uuid(Uuid::from_u128(0xc005));
    let remote_key = SigningKey::from_bytes(&[0xc6; 32]).verifying_key();
    let authority = PinnedFolderAuthority::new(remote_writer, remote_key).unwrap();
    let (device, key) = transport();
    let coordinator = FolderCoordinator::initialize_or_resume(
        create_installation(&state),
        FolderAuthoritySelection::pinned(authority),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    assert_eq!(coordinator.authority(), authority);
    assert!(coordinator.events().unwrap().current_head().is_none());
    drop(coordinator);
    let reopened = FolderCoordinator::open_ready(
        reopen_installation(&state),
        FolderAuthoritySelection::pinned(authority),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    assert!(reopened.events().unwrap().current_head().is_none());
}

#[test]
fn ordinary_open_never_creates_missing_initialization_state() {
    let state = private_dir();
    let user = private_dir();
    let installation = create_installation(&state);
    let (device, key) = transport();
    assert!(matches!(
        FolderCoordinator::open_ready(
            installation,
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        ),
        Err(FolderCoordinatorError::NotReady)
    ));
    assert!(!state.path().join(AUTHORITY_FILE).exists());
    assert!(!state.path().join(EVENTS_DIR).exists());
}

#[test]
fn cancellation_before_setup_creates_nothing_and_releases_the_root_lock() {
    let state = private_dir();
    let user = private_dir();
    let control = JobControl::new();
    control.cancel();
    let (device, key) = transport();
    assert!(matches!(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &control,
        ),
        Err(FolderCoordinatorError::Interrupted)
    ));
    assert!(!state.path().join(AUTHORITY_FILE).exists());
    assert!(reopen_installation(&state).folder_id() == folder());
}

#[test]
fn ready_open_rejects_wrong_transport_and_root_without_mutating_them() {
    let state = private_dir();
    let user = private_dir();
    let other = private_dir();
    let (device, key) = transport();
    let ready = FolderCoordinator::initialize_or_resume(
        create_installation(&state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    drop(ready);
    let wrong_device = DeviceId::from_uuid(Uuid::from_u128(0xc099));
    assert!(matches!(
        FolderCoordinator::open_ready(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            wrong_device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        ),
        Err(FolderCoordinatorError::Authority(
            AuthorityConfigError::WrongTransport
        ))
    ));
    assert!(matches!(
        FolderCoordinator::open_ready(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(other.path()).unwrap(),
            limits(),
            &JobControl::new(),
        ),
        Err(FolderCoordinatorError::Apply(ApplyError::ReadyMismatch))
    ));
    assert_eq!(fs::read_dir(user.path()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(other.path()).unwrap().count(), 0);
}

#[test]
fn unexpected_private_entry_is_preserved_and_blocks_resume() {
    let state = private_dir();
    let user = private_dir();
    drop(create_installation(&state));
    let root = PrivateStateDir::open_root(state.path()).unwrap();
    let lock = root.try_lock().unwrap();
    root.create_new_file(
        &lock,
        &StateKey::new("unknown.v1").unwrap(),
        b"preserve",
        64,
    )
    .unwrap();
    drop(lock);
    drop(root);
    let (device, key) = transport();
    assert!(matches!(
        FolderCoordinator::initialize_or_resume(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        ),
        Err(FolderCoordinatorError::UnexpectedState)
    ));
    assert_eq!(
        fs::read(state.path().join("unknown.v1")).unwrap(),
        b"preserve"
    );
}

#[test]
fn missing_ready_is_not_ordinary_open_but_explicit_resume_restores_it() {
    let state = private_dir();
    let user = private_dir();
    let (device, key) = transport();
    drop(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .unwrap(),
    );
    let apply = state.path().join("apply/apply.v1");
    let file = fs::OpenOptions::new().write(true).open(&apply).unwrap();
    file.set_len(8).unwrap();
    file.sync_all().unwrap();
    assert!(matches!(
        FolderCoordinator::open_ready(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        ),
        Err(FolderCoordinatorError::NotReady)
    ));
    let resumed = FolderCoordinator::initialize_or_resume(
        reopen_installation(&state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    assert_eq!(resumed.events().unwrap().current_head().unwrap().epoch(), 1);
}

#[test]
fn corrupt_event_history_is_preserved_and_never_reinitialized() {
    let state = private_dir();
    let user = private_dir();
    let (device, key) = transport();
    drop(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .unwrap(),
    );
    let event_path = state.path().join("events/events.v1");
    let mut bytes = fs::read(&event_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&event_path, &bytes).unwrap();
    assert!(
        FolderCoordinator::initialize_or_resume(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .is_err()
    );
    assert_eq!(fs::read(&event_path).unwrap(), bytes);
}

#[test]
fn ordinary_open_does_not_recreate_a_missing_content_child() {
    let state = private_dir();
    let user = private_dir();
    let (device, key) = transport();
    drop(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .unwrap(),
    );
    fs::remove_file(state.path().join("content/staging/writer.lock")).unwrap();
    fs::remove_dir(state.path().join("content/staging")).unwrap();
    assert!(
        FolderCoordinator::open_ready(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .is_err()
    );
    assert!(!state.path().join("content/staging").exists());
}

#[test]
fn ready_topology_never_recreates_missing_owner_history() {
    let state = private_dir();
    let user = private_dir();
    let (device, key) = transport();
    drop(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .unwrap(),
    );
    let event_path = state.path().join("events/events.v1");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&event_path)
        .unwrap();
    file.set_len(8).unwrap();
    file.sync_all().unwrap();
    let header_only = fs::read(&event_path).unwrap();
    assert!(
        FolderCoordinator::open_ready(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .is_err()
    );
    assert_eq!(fs::read(&event_path).unwrap(), header_only);
    assert!(
        FolderCoordinator::initialize_or_resume(
            reopen_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &JobControl::new(),
        )
        .is_err()
    );
    assert_eq!(fs::read(&event_path).unwrap(), header_only);
}

#[test]
fn cancellation_after_ready_fsync_exposes_no_handle_and_later_open_succeeds() {
    let state = private_dir();
    let user = private_dir();
    let (device, key) = transport();
    let control = JobControl::new();
    let cancel = control.clone();
    set_after_ready_append_hook(move || cancel.cancel());
    assert!(matches!(
        FolderCoordinator::initialize_or_resume(
            create_installation(&state),
            FolderAuthoritySelection::local_owner(),
            device,
            key,
            &protector(),
            &AuthorizedRoot::open(user.path()).unwrap(),
            limits(),
            &control,
        ),
        Err(FolderCoordinatorError::Apply(ApplyError::Interrupted))
    ));
    let reopened = FolderCoordinator::open_ready(
        reopen_installation(&state),
        FolderAuthoritySelection::local_owner(),
        device,
        key,
        &protector(),
        &AuthorizedRoot::open(user.path()).unwrap(),
        limits(),
        &JobControl::new(),
    )
    .unwrap();
    reopened.validate_ready(&JobControl::new()).unwrap();
}

#[test]
fn local_cycle_retains_publishes_adopts_once_and_preserves_empty_directories() {
    let state = private_dir();
    let user = private_dir();
    fs::create_dir(user.path().join("empty")).unwrap();
    fs::create_dir(user.path().join("nested")).unwrap();
    fs::write(user.path().join("nested/file"), b"local bytes").unwrap();
    let mut coordinator = ready_coordinator(&state, &user);

    let first = coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert!(!first.mutations_deferred());
    assert_eq!(
        dispositions(&first),
        vec![
            ("empty", LocalCycleDisposition::PublishedApplied),
            ("nested", LocalCycleDisposition::PublishedApplied),
            ("nested/file", LocalCycleDisposition::PublishedApplied),
        ]
    );
    assert!(
        first
            .entries()
            .iter()
            .all(|entry| entry.operation().is_some())
    );
    let events_before = fs::read(state.path().join("events/events.v1")).unwrap();
    let apply_before = fs::read(state.path().join("apply/apply.v1")).unwrap();
    let usage_before = coordinator.content_usage().unwrap();

    let second = coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert_eq!(
        dispositions(&second),
        vec![
            ("empty", LocalCycleDisposition::UnchangedApplied),
            ("nested", LocalCycleDisposition::UnchangedApplied),
            ("nested/file", LocalCycleDisposition::UnchangedApplied),
        ]
    );
    assert_eq!(
        fs::read(state.path().join("events/events.v1")).unwrap(),
        events_before
    );
    assert_eq!(
        fs::read(state.path().join("apply/apply.v1")).unwrap(),
        apply_before
    );
    assert_eq!(coordinator.content_usage().unwrap(), usage_before);
}

#[test]
fn offline_edit_delete_and_new_path_reopen_as_complete_deferred_observation() {
    let state = private_dir();
    let user = private_dir();
    fs::write(user.path().join("known"), b"before").unwrap();
    fs::write(user.path().join("stable"), b"unchanged").unwrap();
    fs::create_dir(user.path().join("removed")).unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    drop(coordinator);

    fs::write(user.path().join("known"), b"after!").unwrap();
    fs::remove_dir(user.path().join("removed")).unwrap();
    fs::write(user.path().join("new"), b"preserve me").unwrap();
    let events_before = fs::read(state.path().join("events/events.v1")).unwrap();
    let apply_before = fs::read(state.path().join("apply/apply.v1")).unwrap();
    let content_before = fs::read_dir(state.path().join("content/staging"))
        .unwrap()
        .count();

    let mut reopened = reopen_coordinator(&state, &user);
    assert!(reopened.validate_ready(&JobControl::new()).is_err());
    let report = reopened
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert!(report.mutations_deferred());
    assert_eq!(
        dispositions(&report),
        vec![
            ("known", LocalCycleDisposition::PendingKnownChange),
            ("new", LocalCycleDisposition::DeferredNew),
            ("removed", LocalCycleDisposition::PendingDeletion),
            ("stable", LocalCycleDisposition::UnchangedApplied),
        ]
    );
    assert_eq!(fs::read(user.path().join("known")).unwrap(), b"after!");
    assert_eq!(fs::read(user.path().join("new")).unwrap(), b"preserve me");
    assert_eq!(
        fs::read(state.path().join("events/events.v1")).unwrap(),
        events_before
    );
    assert_eq!(
        fs::read(state.path().join("apply/apply.v1")).unwrap(),
        apply_before
    );
    assert_eq!(
        fs::read_dir(state.path().join("content/staging"))
            .unwrap()
            .count(),
        content_before
    );
}

#[test]
fn same_bytes_replaced_inode_is_dirty_historical_state() {
    let state = private_dir();
    let user = private_dir();
    let path = user.path().join("same");
    fs::write(&path, b"same bytes").unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    drop(coordinator);

    let retained_old_inode = fs::File::open(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"same bytes").unwrap();
    let mut reopened = reopen_coordinator(&state, &user);
    let report = reopened
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert_eq!(
        dispositions(&report),
        vec![("same", LocalCycleDisposition::PendingKnownChange)]
    );
    assert!(report.mutations_deferred());
    drop(retained_old_inode);
}

#[test]
fn committed_publication_without_apply_resumes_exact_operation_after_reopen() {
    use crate::sync::body::{ContentDigest, EntryValue, FileContent, OperationBody};
    use crate::sync::path::SyncPath;

    let state = private_dir();
    let user = private_dir();
    let path = user.path().join("pending");
    let bytes = b"retained before crash";
    fs::write(&path, bytes).unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    let content = FileContent::new(
        ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u64,
        false,
    )
    .unwrap();
    let receipt = coordinator
        .content
        .retain_stream(content, fs::File::open(&path).unwrap(), &JobControl::new())
        .unwrap();
    let body = OperationBody::new(
        SyncPath::from_wire("pending").unwrap(),
        EntryValue::File(content),
    );
    let published = coordinator
        .events
        .publish_retained(&body, &receipt)
        .unwrap();
    let expected_id = published.id();
    drop(published);
    let events_before = fs::read(state.path().join("events/events.v1")).unwrap();
    drop(coordinator);

    let mut reopened = reopen_coordinator(&state, &user);
    let report = reopened
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert_eq!(
        dispositions(&report),
        vec![("pending", LocalCycleDisposition::ResumedApplied)]
    );
    assert_eq!(report.entries()[0].operation(), Some(expected_id));
    assert_eq!(
        fs::read(state.path().join("events/events.v1")).unwrap(),
        events_before
    );
}

#[test]
fn canonical_unicode_source_is_resolved_but_casefold_ambiguity_mutates_nothing() {
    let state = private_dir();
    let user = private_dir();
    fs::write(user.path().join("Cafe\u{301}"), b"accent").unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    let report = coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert_eq!(report.entries()[0].path().as_str(), "Caf\u{e9}");
    drop(coordinator);

    let ambiguous_state = private_dir();
    let ambiguous_user = private_dir();
    fs::write(ambiguous_user.path().join("Stra\u{df}e"), b"one").unwrap();
    fs::write(ambiguous_user.path().join("STRASSE"), b"two").unwrap();
    let mut ambiguous = ready_coordinator(&ambiguous_state, &ambiguous_user);
    let events_before = fs::read(ambiguous_state.path().join("events/events.v1")).unwrap();
    // Default macOS volumes may collapse this casefold pair before the scanner
    // can observe two entries; case-sensitive Unix filesystems exercise it.
    if fs::read_dir(ambiguous_user.path()).unwrap().count() != 2 {
        return;
    }
    let result = ambiguous.capture_local_cycle(cycle_limits(), &JobControl::new());
    assert!(
        matches!(result, Err(LocalCycleError::SourceUnavailable)),
        "{result:?}"
    );
    assert_eq!(
        fs::read(ambiguous_state.path().join("events/events.v1")).unwrap(),
        events_before
    );
}

#[test]
fn pending_create_stage_is_reported_and_never_published_as_user_content() {
    use std::io::Cursor;

    use crate::sync::body::{ContentDigest, EntryValue, FileContent, OperationBody};
    use crate::sync::path::SyncPath;

    let state = private_dir();
    let user = private_dir();
    let mut coordinator = ready_coordinator(&state, &user);
    let bytes = b"future target";
    let content = FileContent::new(
        ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u64,
        false,
    )
    .unwrap();
    let receipt = coordinator
        .content
        .retain_stream(content, Cursor::new(bytes), &JobControl::new())
        .unwrap();
    let body = OperationBody::new(
        SyncPath::from_wire("target").unwrap(),
        EntryValue::File(content),
    );
    let published = coordinator
        .events
        .publish_retained(&body, &receipt)
        .unwrap();
    coordinator.applier.interrupt_after_stage_durable_for_test();
    assert!(matches!(
        coordinator.applier.apply(
            &coordinator.events,
            &coordinator.content,
            published.id(),
            published.event_bytes(),
            &JobControl::new(),
        ),
        Err(ApplyError::Pending)
    ));
    drop(published);
    let events_before = fs::read(state.path().join("events/events.v1")).unwrap();

    let report = coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert!(report.mutations_deferred());
    assert!(report.entries().iter().any(|entry| {
        entry.path().as_str() == "target"
            && entry.disposition() == LocalCycleDisposition::PendingDeletion
    }));
    assert!(report.entries().iter().any(|entry| {
        entry.path().as_str() != "target"
            && entry.disposition() == LocalCycleDisposition::DeferredNew
    }));
    assert_eq!(
        fs::read(state.path().join("events/events.v1")).unwrap(),
        events_before
    );
}

#[test]
fn adoption_conflict_stops_later_publications_and_returns_complete_report() {
    let state = private_dir();
    let user = private_dir();
    let first = user.path().join("a");
    fs::write(&first, b"first").unwrap();
    fs::write(user.path().join("b"), b"second").unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    coordinator
        .applier
        .mutate_before_adoption_receipt_for_test(move || {
            fs::remove_file(&first).unwrap();
            fs::write(&first, b"raced").unwrap();
        });

    let report = coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert!(report.mutations_deferred());
    assert_eq!(
        dispositions(&report),
        vec![
            ("a", LocalCycleDisposition::Conflict),
            ("b", LocalCycleDisposition::DeferredNew),
        ]
    );
    assert!(
        coordinator
            .events()
            .unwrap()
            .register(&SyncPath::from_wire("b").unwrap())
            .is_none()
    );
    assert_eq!(fs::read(user.path().join("a")).unwrap(), b"raced");
    assert_eq!(fs::read(user.path().join("b")).unwrap(), b"second");
}

#[test]
fn replacement_during_missing_content_retention_is_dirty_not_applied() {
    let state = private_dir();
    let user = private_dir();
    let path = user.path().join("tracked");
    fs::write(&path, b"same retained bytes").unwrap();
    let mut coordinator = ready_coordinator(&state, &user);
    coordinator
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    drop(coordinator);

    for directory in ["content/chunks", "content/manifests"] {
        for entry in fs::read_dir(state.path().join(directory)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() != "writer.lock" {
                fs::remove_file(entry.path()).unwrap();
            }
        }
    }
    let mut reopened = reopen_coordinator(&state, &user);
    let old_inode = fs::File::open(&path).unwrap();
    let raced = path.clone();
    set_before_retention_hook(move || {
        fs::remove_file(&raced).unwrap();
        fs::write(&raced, b"same retained bytes").unwrap();
        drop(old_inode);
    });
    let report = reopened
        .capture_local_cycle(cycle_limits(), &JobControl::new())
        .unwrap();
    assert!(report.mutations_deferred());
    assert_eq!(
        dispositions(&report),
        vec![("tracked", LocalCycleDisposition::PendingKnownChange)]
    );
}
