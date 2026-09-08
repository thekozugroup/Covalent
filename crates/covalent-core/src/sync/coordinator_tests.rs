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
