//! Exercises the crate-internal consuming handoff from a sibling coordinator.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use covalent_protocol::DeviceId;
use ed25519_dalek::SigningKey;
use uuid::Uuid;

use super::body::{ContentDigest, EntryValue, FileContent, OperationBody};
use super::content_store::{ContentStoreLimits, SyncContentStore};
use super::event::{EventEnvelope, EventKind};
use super::event_log::{DurableEventLog, EventLogLimits};
use super::ids::{FolderId, WriterId};
use super::installation::{SyncInstallation, SyncInstallationParts};
use super::log_frame::{LogBinding, LogFileKind, LogFrameKey};
use super::machine::{FolderEventMachine, FolderMachineConfig, FolderMachineLimits};
use super::membership::{MemberGrant, MemberRole, encode_signed_epoch};
use super::path::SyncPath;
use super::publication::DurableFolderLog;
use super::state_dir::{PrivateStateDir, PrivateStateLock, StateDirError, StateKey};
use crate::{JobControl, StaticKeyProtector};

struct Harness {
    log: DurableFolderLog,
    store: SyncContentStore,
    _apply_key: LogFrameKey,
    writer: WriterId,
    installation: Uuid,
    generation: Uuid,
    _root: Arc<PrivateStateDir>,
    // Drop child owners before releasing the installation coordinator lock.
    _root_lock: Arc<PrivateStateLock>,
}

fn key(name: &str) -> StateKey {
    StateKey::new(name).unwrap()
}
fn folder() -> FolderId {
    FolderId::from_uuid(Uuid::from_u128(71))
}
fn root(path: &std::path::Path) -> PrivateStateDir {
    PrivateStateDir::open_root(path).unwrap()
}

fn open_harness(path: &std::path::Path, create: bool) -> Harness {
    let protector = StaticKeyProtector::new(1, [75; 32]).unwrap();
    let installation = if create {
        SyncInstallation::create(root(path), folder(), &protector)
    } else {
        SyncInstallation::open(root(path), folder(), &protector)
    }
    .unwrap();
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
    } = installation.into_parts().unwrap();
    let outer = Arc::new(directory);
    let outer_lock = Arc::new(lock);
    assert!(matches!(root(path).try_lock(), Err(StateDirError::Locked)));
    outer.sync(&outer_lock).unwrap();
    let events_dir = outer.open_or_create_child(&key("events")).unwrap();
    let content_dir = outer.open_or_create_child(&key("content")).unwrap();
    let binding = LogBinding::new(
        folder_id,
        installation_id,
        generation_id,
        LogFileKind::FolderEvents,
    );
    let machine = FolderEventMachine::new(FolderMachineConfig {
        folder_id,
        authority_writer_id: writer_id,
        pinned_authority_key: signing_key.verifying_key(),
        local_writer_id: writer_id,
        limits: FolderMachineLimits {
            maximum_index_bytes: 1 << 20,
            maximum_operations: 100,
            maximum_paths: 100,
            maximum_membership_epochs: 16,
            maximum_pending_evidence_bytes: 1 << 18,
            maximum_pending_evidence_records: 128,
        },
    })
    .unwrap();
    let limits = EventLogLimits {
        maximum_bytes: 1 << 20,
        maximum_records: 100,
    };
    let event_log = if create {
        DurableEventLog::create(
            &events_dir,
            &key("events.v1"),
            binding,
            event_log_key,
            limits,
            machine,
        )
    } else {
        DurableEventLog::open(
            &events_dir,
            &key("events.v1"),
            binding,
            event_log_key,
            limits,
            machine,
        )
    }
    .unwrap();
    let genesis = if create {
        let grant = MemberGrant::new(
            writer_id,
            signing_key.verifying_key(),
            DeviceId::from_uuid(Uuid::from_u128(72)),
            SigningKey::from_bytes(&[73; 32]).verifying_key(),
            MemberRole::ReadWrite,
        )
        .unwrap();
        Some(
            encode_signed_epoch(
                &signing_key,
                folder_id,
                1,
                writer_id,
                None,
                &[grant],
                None,
                &[],
                &[],
                &[],
                &[],
            )
            .unwrap(),
        )
    } else {
        None
    };
    let mut log = DurableFolderLog::new(event_log, writer_id, signing_key).unwrap();
    if let Some(genesis) = genesis {
        let event = EventEnvelope::from_signed_record(EventKind::MembershipEpoch, &genesis)
            .unwrap()
            .encode()
            .unwrap();
        log.ingest(event.as_bytes()).unwrap();
    }
    let store = SyncContentStore::open(
        content_dir,
        content_crypto,
        ContentStoreLimits {
            maximum_stored_bytes: 1 << 20,
            maximum_objects: 100,
        },
    )
    .unwrap();
    Harness {
        log,
        store,
        _apply_key: apply_log_key,
        writer: writer_id,
        installation: installation_id,
        generation: generation_id,
        _root: outer,
        _root_lock: outer_lock,
    }
}

fn file(bytes: &[u8]) -> FileContent {
    FileContent::new(
        ContentDigest::from_bytes(*blake3::hash(bytes).as_bytes()),
        bytes.len() as u64,
        false,
    )
    .unwrap()
}

#[test]
fn consuming_installation_handoff_publishes_retained_files_and_reopens_all_owners() {
    let temporary = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let control = JobControl::new();
    let mut first = open_harness(temporary.path(), true);
    let identities = (first.writer, first.installation, first.generation);
    let original = b"installed content before first publication";
    let expected = file(original);
    let receipt = first
        .store
        .retain_stream(expected, original.as_slice(), &control)
        .unwrap();
    let body = OperationBody::new(
        SyncPath::from_wire("from-installed-keys.txt").unwrap(),
        EntryValue::File(expected),
    );
    let publication = first.log.publish_retained(&body, &receipt).unwrap();
    assert_eq!(publication.id().counter(), 1);
    let published_id = publication.id();
    let event = publication.event_bytes().to_vec();
    let charged = first.store.usage().unwrap();
    drop(first);

    let mut reopened = open_harness(temporary.path(), false);
    assert_eq!(
        (reopened.writer, reopened.installation, reopened.generation),
        identities
    );
    assert_eq!(reopened.store.usage().unwrap(), charged);
    assert!(matches!(
        root(temporary.path()).try_lock(),
        Err(StateDirError::Locked)
    ));
    let accepted = reopened
        .log
        .machine()
        .unwrap()
        .accepted_operation(published_id, &event)
        .unwrap();
    assert_eq!(accepted.body(), &body);
    let manifest = reopened.store.read_manifest(expected).unwrap();
    assert_eq!(
        reopened
            .store
            .read_chunk(manifest.chunks()[0], &control)
            .unwrap()
            .as_slice(),
        original
    );
    reopened.store.verify_retained(expected, &control).unwrap();
    let replacement = b"installed content after reopen";
    let changed = file(replacement);
    let receipt = reopened
        .store
        .retain_stream(changed, replacement.as_slice(), &control)
        .unwrap();
    let publication = reopened
        .log
        .publish_retained(
            &OperationBody::new(body.path().clone(), EntryValue::File(changed)),
            &receipt,
        )
        .unwrap();
    assert_eq!(publication.id().counter(), 2);
    // Both versions remain retained after advancing accepted file history.
    reopened.store.verify_retained(expected, &control).unwrap();
    reopened.store.verify_retained(changed, &control).unwrap();
    drop(reopened);
    let final_open = open_harness(temporary.path(), false);
    assert_eq!(
        final_open
            .log
            .machine()
            .unwrap()
            .register(body.path())
            .unwrap()
            .active()
            .next()
            .unwrap()
            .value(),
        &EntryValue::File(changed)
    );
    assert!(matches!(
        root(temporary.path()).try_lock(),
        Err(StateDirError::Locked)
    ));
    drop(final_open);
    root(temporary.path())
        .try_lock()
        .expect("outer lock released with all owners");
}
