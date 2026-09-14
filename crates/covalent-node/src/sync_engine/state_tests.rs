use super::*;
use covalent_core::StaticKeyProtector;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn fixture() -> (
    tempfile::TempDir,
    Arc<EngineInstallation>,
    Arc<dyn KeyProtector>,
) {
    let directory = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let protector: Arc<dyn KeyProtector> = Arc::new(StaticKeyProtector::new(1, [37; 32]).unwrap());
    let installation = Arc::new(EngineInstallation::create(&root, protector.as_ref()).unwrap());
    (directory, installation, protector)
}

#[test]
fn restart_preserves_authenticated_payload_and_exact_revision() {
    let (_root, installation, protector) = fixture();
    let mut store = EngineStateStore::create(
        Arc::clone(&installation),
        Arc::clone(&protector),
        br#"{"shares":[]}"#,
    )
    .unwrap();
    assert_eq!(store.revision(), 1);
    let next = br#"{"shares":[{"paused":true,"label":"Documents"}]}"#;
    store.replace(next).unwrap();
    assert_eq!(store.revision(), 2);
    assert_eq!(store.payload().unwrap(), next);
    drop(store);
    let reopened = EngineStateStore::open(Arc::clone(&installation), protector).unwrap();
    assert_eq!(reopened.revision(), 2);
    assert_eq!(reopened.payload().unwrap(), next);
    let metadata = std::fs::metadata(installation.root().join(RECORD_NAME)).unwrap();
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.nlink(), 1);
    assert!(!std::fs::read_dir(installation.root()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("sharing-")
    }));
}

#[test]
fn stale_handle_cannot_overwrite_a_newer_decision() {
    let (_root, installation, protector) = fixture();
    let mut first = EngineStateStore::create(
        Arc::clone(&installation),
        Arc::clone(&protector),
        br#"{"revoked":false}"#,
    )
    .unwrap();
    let mut second =
        EngineStateStore::open(Arc::clone(&installation), Arc::clone(&protector)).unwrap();
    first.replace(br#"{"revoked":true}"#).unwrap();
    assert_eq!(
        second.payload().unwrap_err(),
        EngineStateError::StaleRevision
    );
    assert_eq!(
        second.replace(br#"{"revoked":false}"#).unwrap_err(),
        EngineStateError::StaleRevision
    );
    assert_eq!(
        EngineStateStore::open(installation, protector)
            .unwrap()
            .payload()
            .unwrap(),
        br#"{"revoked":true}"#
    );
}

#[test]
fn damaged_payload_revision_and_wrong_key_are_refused_without_repair() {
    let (_root, installation, protector) = fixture();
    let store = EngineStateStore::create(
        Arc::clone(&installation),
        Arc::clone(&protector),
        br#"{"revoked":true}"#,
    )
    .unwrap();
    let path = installation.root().join(RECORD_NAME);
    let original = std::fs::read(&path).unwrap();
    let wrong: Arc<dyn KeyProtector> = Arc::new(StaticKeyProtector::new(1, [38; 32]).unwrap());
    assert_eq!(
        EngineStateStore::open(Arc::clone(&installation), wrong).unwrap_err(),
        EngineStateError::InvalidState
    );
    for (field, value) in [
        ("payload", serde_json::json!("{\"revoked\":false}")),
        ("revision", serde_json::json!(99)),
        ("schemaVersion", serde_json::json!(2)),
    ] {
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        record[field] = value;
        let damaged = serde_json::to_vec(&record).unwrap();
        std::fs::write(&path, &damaged).unwrap();
        assert_eq!(
            EngineStateStore::open(Arc::clone(&installation), Arc::clone(&protector)).unwrap_err(),
            EngineStateError::InvalidState
        );
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
    }
    assert_eq!(
        store.payload().unwrap_err(),
        EngineStateError::StaleRevision
    );
}

#[test]
fn copied_missing_symlink_and_hardlink_records_are_not_defaults() {
    let (_one, installation, protector) = fixture();
    let (_two, other, _) = fixture();
    drop(
        EngineStateStore::create(Arc::clone(&installation), Arc::clone(&protector), b"{}").unwrap(),
    );
    let path = installation.root().join(RECORD_NAME);
    let other_path = other.root().join(RECORD_NAME);
    std::fs::copy(&path, &other_path).unwrap();
    assert_eq!(
        EngineStateStore::open(other, Arc::clone(&protector)).unwrap_err(),
        EngineStateError::InvalidState
    );
    let outside = installation.root().join("held.v1");
    std::fs::rename(&path, &outside).unwrap();
    assert_eq!(
        EngineStateStore::open(Arc::clone(&installation), Arc::clone(&protector)).unwrap_err(),
        EngineStateError::InvalidState
    );
    assert!(!path.exists());
    symlink(&outside, &path).unwrap();
    assert!(EngineStateStore::open(Arc::clone(&installation), Arc::clone(&protector)).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&outside, &path).unwrap();
    assert!(EngineStateStore::open(installation, protector).is_err());
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        std::fs::read(&path).unwrap()
    );
}

#[test]
fn interruption_at_each_publish_boundary_requires_reopen() {
    for fail_at in [WriteBoundary::StagedDurable, WriteBoundary::Renamed] {
        let (_root, installation, protector) = fixture();
        let mut store = EngineStateStore::create(
            Arc::clone(&installation),
            Arc::clone(&protector),
            b"{\"generation\":1}",
        )
        .unwrap();
        let error = store
            .replace_at_boundaries(b"{\"generation\":2}", |boundary| {
                if boundary == fail_at {
                    Err(EngineStateError::PersistenceUncertain)
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, EngineStateError::PersistenceUncertain);
        assert_eq!(store.revision(), 1);
        assert_eq!(
            store.payload().unwrap_err(),
            EngineStateError::PersistenceUncertain
        );
        assert_eq!(
            store.replace(b"{}").unwrap_err(),
            EngineStateError::PersistenceUncertain
        );
        drop(store);
        let reopened = EngineStateStore::open(installation, protector).unwrap();
        let expected = if fail_at == WriteBoundary::StagedDurable {
            1
        } else {
            2
        };
        assert_eq!(reopened.revision(), expected);
        assert_eq!(
            reopened.payload().unwrap(),
            format!("{{\"generation\":{expected}}}").as_bytes()
        );
    }
}

#[test]
fn invalid_and_oversized_input_does_not_mutate_or_poison_current_state() {
    let (_root, installation, protector) = fixture();
    let mut store = EngineStateStore::create(Arc::clone(&installation), protector, b"{}").unwrap();
    let path = installation.root().join(RECORD_NAME);
    let original = std::fs::read(&path).unwrap();
    for invalid in [
        Vec::new(),
        b"{".to_vec(),
        vec![b' '; MAX_PAYLOAD + 1],
        vec![b'['; 1024],
    ] {
        assert_eq!(
            store.replace(&invalid).unwrap_err(),
            EngineStateError::InvalidPayload
        );
        assert_eq!(store.payload().unwrap(), b"{}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[test]
fn staging_cleanup_never_unlinks_a_replaced_entry() {
    let (_root, installation, _) = fixture();
    let directory = File::open(installation.root()).unwrap();
    let staged = StagedRecord::create(directory).unwrap();
    let staged_path = installation.root().join(&staged.name);
    let moved_path = installation.root().join("moved.v1");
    std::fs::rename(&staged_path, &moved_path).unwrap();
    std::fs::write(&staged_path, b"incumbent").unwrap();
    drop(staged);
    assert_eq!(std::fs::read(staged_path).unwrap(), b"incumbent");
    assert!(moved_path.exists());
}

#[test]
fn reopen_reconciles_one_torn_stage_and_refuses_unsafe_remnants() {
    let (_root, installation, protector) = fixture();
    drop(
        EngineStateStore::create(Arc::clone(&installation), Arc::clone(&protector), b"{}").unwrap(),
    );
    let stage = installation.root().join(STAGED_NAME);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage)
        .unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .unwrap();
    std::fs::write(&stage, b"{interrupted").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let mut reopened =
        EngineStateStore::open(Arc::clone(&installation), Arc::clone(&protector)).unwrap();
    assert!(!stage.exists());
    reopened.replace(br#"{"next":true}"#).unwrap();
    assert_eq!(reopened.revision(), 2);
    drop(reopened);
    let outside = installation.root().join("outside.v1");
    std::fs::write(&outside, b"preserve").unwrap();
    symlink(&outside, &stage).unwrap();
    assert!(EngineStateStore::open(installation, protector).is_err());
    assert!(
        std::fs::symlink_metadata(stage)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"preserve");
}
