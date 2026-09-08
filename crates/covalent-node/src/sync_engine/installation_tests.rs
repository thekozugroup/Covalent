use super::*;
use covalent_core::StaticKeyProtector;
use std::os::unix::fs::{PermissionsExt as _, symlink};

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    (directory, root)
}

fn protector(value: u8) -> StaticKeyProtector {
    StaticKeyProtector::new(1, [value; 32]).unwrap()
}

#[test]
fn protected_identity_survives_restart_and_excludes_concurrent_owners() {
    let (_fixture, root) = fixture();
    let installation = EngineInstallation::create(&root, &protector(1)).unwrap();
    let id = installation.device_id().clone();
    assert!(EngineInstallation::open(&root, &protector(1)).is_err());
    let record = std::fs::read(root.join(IDENTITY_RECORD)).unwrap();
    assert!(!String::from_utf8_lossy(&record).contains("PRIVATE KEY"));
    let database = installation.database_directory().unwrap();
    assert_eq!(std::fs::metadata(database).unwrap().mode() & 0o777, 0o700);
    drop(installation);
    let reopened = EngineInstallation::open(&root, &protector(1)).unwrap();
    assert_eq!(reopened.device_id(), &id);
    assert_eq!(std::fs::read(root.join(IDENTITY_RECORD)).unwrap(), record);
}

#[test]
fn wrong_key_and_copied_state_never_create_replacement_identity() {
    let (_source_fixture, source) = fixture();
    let (_target_fixture, target) = fixture();
    drop(EngineInstallation::create(&source, &protector(1)).unwrap());
    let record = std::fs::read(source.join(IDENTITY_RECORD)).unwrap();
    for name in [IDENTITY_RECORD, "writer.lock"] {
        std::fs::copy(source.join(name), target.join(name)).unwrap();
        std::fs::set_permissions(target.join(name), std::fs::Permissions::from_mode(0o600))
            .unwrap();
    }
    assert_eq!(
        EngineInstallation::open(&source, &protector(2)).unwrap_err(),
        EngineInstallationError::InvalidIdentity
    );
    assert_eq!(
        EngineInstallation::open(&target, &protector(1)).unwrap_err(),
        EngineInstallationError::InvalidIdentity
    );
    assert_eq!(std::fs::read(source.join(IDENTITY_RECORD)).unwrap(), record);
    assert_eq!(std::fs::read(target.join(IDENTITY_RECORD)).unwrap(), record);
}

#[test]
fn absent_and_damaged_identity_cannot_be_recreated_against_database() {
    let (_fixture, root) = fixture();
    let installation = EngineInstallation::create(&root, &protector(1)).unwrap();
    installation.database_directory().unwrap();
    drop(installation);
    std::fs::write(root.join(IDENTITY_RECORD), b"damaged").unwrap();
    assert_eq!(
        EngineInstallation::open(&root, &protector(1)).unwrap_err(),
        EngineInstallationError::InvalidIdentity
    );
    assert_eq!(
        EngineInstallation::create(&root, &protector(1)).unwrap_err(),
        EngineInstallationError::NotFresh
    );
    assert_eq!(
        std::fs::read(root.join(IDENTITY_RECORD)).unwrap(),
        b"damaged"
    );
    std::fs::remove_file(root.join(IDENTITY_RECORD)).unwrap();
    assert_eq!(
        EngineInstallation::open(&root, &protector(1)).unwrap_err(),
        EngineInstallationError::InvalidIdentity
    );
    assert_eq!(
        EngineInstallation::create(&root, &protector(1)).unwrap_err(),
        EngineInstallationError::NotFresh
    );
    assert!(!root.join(IDENTITY_RECORD).exists());
}

#[test]
fn replaced_root_and_symlink_database_are_rejected_without_repair() {
    let (_fixture, parent) = fixture();
    let root = parent.join("installation");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let installation = EngineInstallation::create(&root, &protector(1)).unwrap();
    let outside = parent.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("database")).unwrap();
    assert_eq!(
        installation.database_directory().unwrap_err(),
        EngineInstallationError::Unavailable
    );
    assert!(
        std::fs::symlink_metadata(root.join("database"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::rename(&root, parent.join("old")).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        installation.revalidate().unwrap_err(),
        EngineInstallationError::Unavailable
    );
}
