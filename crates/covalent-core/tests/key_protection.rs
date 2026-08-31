use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{
    BackupOptions, CoreError, DeviceIdentity, Engine, EngineOptions, JobControl, StaticKeyProtector,
};
use covalent_protocol::BackupId;
use tempfile::tempdir;

fn options(path: impl Into<PathBuf>, byte: u8) -> EngineOptions {
    EngineOptions::new(path).with_key_protector(Arc::new(
        StaticKeyProtector::new(1, [byte; 32]).expect("test protector"),
    ))
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).expect("read protected record")).expect("JSON")
}

#[test]
fn engine_refuses_locked_startup_before_creating_state() {
    let root = tempdir().expect("root");
    let state = root.path().join("locked-state");
    assert!(matches!(
        Engine::open(EngineOptions::new(&state)),
        Err(CoreError::KeyProtectionLocked)
    ));
    assert!(!state.exists());
}

#[test]
fn all_core_long_lived_secrets_are_wrapped_and_wrong_kek_fails() {
    let root = tempdir().expect("root");
    let state = root.path().join("state");
    let source = root.path().join("source");
    fs::create_dir_all(&source).expect("source");
    fs::write(source.join("file.txt"), b"protected backup").expect("source file");
    let backup_id = BackupId::new();
    let engine = Engine::open(options(&state, 0x41)).expect("engine");
    engine
        .backup(
            &source,
            &BackupOptions::new(backup_id, "0001", "protect-key"),
            &JobControl::new(),
            |_| {},
        )
        .expect("backup");
    drop(engine);

    let identity = json(&state.join("identity.json"));
    let master = json(&state.join("recovery-master.json"));
    let backup_key = json(&state.join("keys").join(format!("{backup_id}.json")));
    assert_eq!(identity["schemaVersion"], 2);
    assert_eq!(master["schemaVersion"], 2);
    assert_eq!(backup_key["schemaVersion"], 2);
    assert!(identity.get("privateKey").is_none());
    assert!(master.get("key").is_none());
    assert!(backup_key.get("key").is_none());
    assert!(identity.get("protectedPrivateKey").is_some());
    assert!(master.get("protectedKey").is_some());
    assert!(backup_key.get("protectedKey").is_some());

    assert!(matches!(
        Engine::open(options(&state, 0x42)),
        Err(CoreError::AuthenticationFailed)
    ));
    Engine::open(options(&state, 0x41)).expect("correct KEK reopens");
}

#[test]
fn plaintext_v1_identity_and_recovery_master_migrate_atomically_to_v2() {
    let root = tempdir().expect("root");
    let state = root.path().join("legacy");
    fs::create_dir_all(&state).expect("state");
    let identity_path = state.join("identity.json");
    DeviceIdentity::load_or_create(&identity_path).expect("legacy identity");
    let master_path = state.join("recovery-master.json");
    fs::write(
        &master_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "key": URL_SAFE_NO_PAD.encode([0x55_u8; 32]),
        }))
        .expect("legacy master JSON"),
    )
    .expect("legacy master");
    let backup_id = BackupId::new();
    let key_directory = state.join("keys");
    fs::create_dir_all(&key_directory).expect("legacy key directory");
    let backup_key_path = key_directory.join(format!("{backup_id}.json"));
    fs::write(
        &backup_key_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "key": URL_SAFE_NO_PAD.encode([0x56_u8; 32]),
        }))
        .expect("legacy backup key JSON"),
    )
    .expect("legacy backup key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&master_path, fs::Permissions::from_mode(0o600))
            .expect("protect legacy master");
        fs::set_permissions(&key_directory, fs::Permissions::from_mode(0o700))
            .expect("protect legacy key directory");
        fs::set_permissions(&backup_key_path, fs::Permissions::from_mode(0o600))
            .expect("protect legacy backup key");
    }

    Engine::open(options(&state, 0x43)).expect("migrate legacy state");
    let identity = json(&identity_path);
    let master = json(&master_path);
    let backup_key = json(&backup_key_path);
    assert_eq!(identity["schemaVersion"], 2);
    assert_eq!(master["schemaVersion"], 2);
    assert_eq!(backup_key["schemaVersion"], 2);
    assert!(identity.get("privateKey").is_none());
    assert!(master.get("key").is_none());
    assert!(backup_key.get("key").is_none());
}

#[test]
fn copied_volume_and_schema_downgrade_fail_closed() {
    let root = tempdir().expect("root");
    let source = root.path().join("source-state");
    let copied = root.path().join("copied-state");
    let engine = Engine::open(options(&source, 0x44)).expect("source engine");
    drop(engine);
    copy_tree(&source, &copied);
    assert!(matches!(
        Engine::open(options(&copied, 0x44)),
        Err(CoreError::AuthenticationFailed)
    ));

    let identity_path = source.join("identity.json");
    let mut identity = json(&identity_path);
    identity["schemaVersion"] = serde_json::json!(1);
    fs::write(
        &identity_path,
        serde_json::to_vec_pretty(&identity).expect("downgraded JSON"),
    )
    .expect("downgrade identity");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&identity_path, fs::Permissions::from_mode(0o600))
            .expect("protect downgraded identity");
    }
    assert!(Engine::open(options(&source, 0x44)).is_err());
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in walkdir::WalkDir::new(source) {
        let entry = entry.expect("walk state");
        let relative = entry.path().strip_prefix(source).expect("relative state");
        let target = destination.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).expect("copy directory");
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}
