use std::collections::BTreeSet;
use std::fs;
use std::sync::Arc;

use covalent_core::{
    BackupOptions, ChunkProvider, Engine, EngineOptions, JobControl, RecoveryUnlockKey,
    RestoreOptions, StaticKeyProtector, StoreProvider,
};
use covalent_protocol::{BackupId, PeerGrant, PeerRole, ReplicaIntent};
use tempfile::tempdir;

fn engine_options(path: impl Into<std::path::PathBuf>) -> EngineOptions {
    EngineOptions::new(path).with_key_protector(Arc::new(
        StaticKeyProtector::new(1, [0x71; 32]).expect("test protector"),
    ))
}

fn storage_grant(engine: &Engine, name: &str) -> PeerGrant {
    PeerGrant {
        peer_device_id: engine.device_id(),
        public_key: engine.public_identity().public_key,
        display_name: name.to_owned(),
        roles: BTreeSet::from([PeerRole::StorageProvider]),
        confirmed_at_unix_ms: 1,
        revoked: false,
    }
}

#[test]
fn stale_stable_kit_restores_latest_snapshot_after_complete_owner_loss() {
    let root = tempdir().expect("temporary root");
    let owner_path = root.path().join("owner");
    let provider_path = root.path().join("provider");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir_all(&source).expect("source");
    fs::create_dir_all(&target).expect("target");

    let provider = Engine::open(engine_options(&provider_path)).expect("provider");
    let owner = Engine::open(engine_options(&owner_path)).expect("owner");
    let original_owner_id = owner.device_id();
    owner
        .trust_peer(storage_grant(&provider, "Provider NAS"))
        .expect("trust provider");
    let provider_store = Arc::new(StoreProvider::new(
        provider.device_id(),
        provider.store().clone(),
    )) as Arc<dyn ChunkProvider>;
    owner
        .set_connected_providers(vec![Arc::clone(&provider_store)])
        .expect("connect provider");

    let unlock = RecoveryUnlockKey::generate();
    let stale_kit = owner.export_recovery_kit(&unlock).expect("stable kit");
    let backup_id = BackupId::new();
    fs::write(source.join("document.txt"), b"first snapshot").expect("first source");
    let mut first = BackupOptions::new(backup_id, "snapshot-0001", "backup-job-1");
    first.display_name = "Documents".to_owned();
    first.replica_intent = ReplicaIntent::explicit([provider.device_id()]);
    first.created_at_unix_ms = 1;
    owner
        .backup(&source, &first, &JobControl::new(), |_| {})
        .expect("first backup");

    fs::write(source.join("document.txt"), b"latest snapshot survives").expect("latest source");
    let mut latest = BackupOptions::new(backup_id, "snapshot-0002", "backup-job-2");
    latest.display_name = "Documents".to_owned();
    latest.replica_intent = ReplicaIntent::explicit([provider.device_id()]);
    latest.created_at_unix_ms = 2;
    let latest_result = owner
        .backup(&source, &latest, &JobControl::new(), |_| {})
        .expect("latest backup");
    assert!(latest_result.replication.is_complete(1));
    assert_eq!(
        provider
            .store()
            .list_recovery_capsules()
            .expect("capsules")
            .len(),
        2
    );

    drop(owner);
    fs::remove_dir_all(&owner_path).expect("destroy complete owner state");
    let recovered = Engine::recover_from_kit(engine_options(&owner_path), &stale_kit, &unlock)
        .expect("recover stable owner identity");
    assert_eq!(recovered.device_id(), original_owner_id);
    recovered
        .trust_peer(storage_grant(&provider, "Provider NAS"))
        .expect("retrust discovered provider");
    recovered
        .set_connected_providers(vec![provider_store])
        .expect("connect recovered provider");
    let imported = recovered
        .import_recovery_catalogs()
        .expect("import latest provider catalogs");
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].snapshot_id, "snapshot-0002");

    let options = RestoreOptions::all("restore-job");
    let plan = recovered
        .preview_restore(backup_id, "snapshot-0002", &target, &options)
        .expect("preview recovered snapshot");
    recovered
        .restore(&plan, &JobControl::new())
        .expect("restore exclusively from provider");
    assert_eq!(
        fs::read(target.join("document.txt")).expect("restored file"),
        b"latest snapshot survives"
    );
}
