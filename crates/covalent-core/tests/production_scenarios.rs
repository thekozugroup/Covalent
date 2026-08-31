use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use covalent_core::{
    BackupOptions, ChunkProvider, ChunkStore, CoreError, DeviceIdentity, Engine, EngineOptions,
    JobControl, ProviderHealth, RestoreOptions, StaticKeyProtector, StoreProvider,
};
use covalent_protocol::{
    BackupId, ConflictPolicy, PeerGrant, PeerRole, ReplicaAvailability, ReplicaIntent,
};
use tempfile::tempdir;

fn engine_options(path: impl Into<PathBuf>) -> EngineOptions {
    EngineOptions::new(path).with_key_protector(Arc::new(
        StaticKeyProtector::new(1, [0x61; 32]).expect("test protector"),
    ))
}

struct PausingProvider {
    id: covalent_protocol::DeviceId,
    store: ChunkStore,
    control: JobControl,
    paused_once: AtomicBool,
}

impl ChunkProvider for PausingProvider {
    fn device_id(&self) -> covalent_protocol::DeviceId {
        self.id
    }

    fn health(&self) -> ProviderHealth {
        ProviderHealth::Online
    }

    fn put(&self, locator: &str, record: &[u8]) -> Result<(), CoreError> {
        self.store.put_provider_record(locator, record).map(|_| ())
    }

    fn get(&self, locator: &str) -> Result<Vec<u8>, CoreError> {
        let record = self.store.get_provider_record(locator)?;
        if !self.paused_once.swap(true, Ordering::AcqRel) {
            self.control.pause();
        }
        Ok(record)
    }

    fn contains(&self, locator: &str) -> Result<bool, CoreError> {
        self.store.contains(locator)
    }
}

fn trust_storage_provider(engine: &Engine, identity: &covalent_core::PublicIdentity, name: &str) {
    engine
        .trust_peer(PeerGrant {
            peer_device_id: identity.device_id,
            public_key: identity.public_key.clone(),
            display_name: name.to_owned(),
            roles: BTreeSet::from([PeerRole::StorageProvider]),
            confirmed_at_unix_ms: 1,
            revoked: false,
        })
        .expect("trust provider");
}

fn provider(id: covalent_protocol::DeviceId, store: &ChunkStore) -> Arc<dyn ChunkProvider> {
    Arc::new(StoreProvider::new(id, store.clone()))
}

fn record_path(store: &ChunkStore, locator: &str) -> PathBuf {
    store
        .root()
        .join("chunks")
        .join(&locator[..2])
        .join(&locator[2..])
}

fn flip_record_bit(store: &ChunkStore, locator: &str) {
    let path = record_path(store, locator);
    let mut record = fs::read(&path).expect("read provider record");
    let last = record.last_mut().expect("non-empty provider record");
    *last ^= 0x40;
    fs::write(path, record).expect("corrupt provider record");
}

fn remove_records(store: &ChunkStore, locators: &BTreeSet<String>) {
    for locator in locators {
        fs::remove_file(record_path(store, locator)).expect("remove local provider record");
    }
}

fn assert_sparse_content(path: &Path) {
    let mut file = File::open(path).expect("open restored sparse file");
    assert_eq!(file.metadata().expect("metadata").len(), 8 * 1_024 * 1_024);
    let mut bytes = [0_u8; 6];
    file.seek(SeekFrom::Start(2 * 1_024 * 1_024))
        .expect("seek first sparse island");
    file.read_exact(&mut bytes)
        .expect("read first sparse island");
    assert_eq!(&bytes, b"island");
    let mut tail = [0_u8; 4];
    file.seek(SeekFrom::Start(7 * 1_024 * 1_024))
        .expect("seek second sparse island");
    file.read_exact(&mut tail)
        .expect("read second sparse island");
    assert_eq!(&tail, b"tail");
}

#[test]
fn multi_node_backup_resume_corruption_repair_restore_and_revocation() {
    let node_data = tempdir().expect("node data");
    let provider_one_data = tempdir().expect("provider one data");
    let provider_two_data = tempdir().expect("provider two data");
    let unselected_data = tempdir().expect("unselected provider data");
    let source = tempdir().expect("source");
    fs::create_dir_all(source.path().join("nested/empty")).expect("source directories");
    let repeated: Vec<_> = (0..700_000).map(|index| (index % 251) as u8).collect();
    fs::write(source.path().join("nested/first.bin"), &repeated).expect("first source file");
    fs::write(source.path().join("nested/second.bin"), &repeated).expect("second source file");
    fs::write(source.path().join("root.txt"), b"root content").expect("root source file");
    let mut sparse = File::create(source.path().join("sparse.bin")).expect("sparse source file");
    sparse.set_len(8 * 1_024 * 1_024).expect("size sparse file");
    sparse
        .seek(SeekFrom::Start(2 * 1_024 * 1_024))
        .expect("seek sparse source");
    sparse.write_all(b"island").expect("write sparse island");
    sparse
        .seek(SeekFrom::Start(7 * 1_024 * 1_024))
        .expect("seek sparse tail");
    sparse.write_all(b"tail").expect("write sparse tail");
    sparse.sync_all().expect("sync sparse source");
    drop(sparse);

    let engine = Engine::open(engine_options(node_data.path())).expect("engine");
    let one_identity = DeviceIdentity::generate().public_identity();
    let two_identity = DeviceIdentity::generate().public_identity();
    let unselected_identity = DeviceIdentity::generate().public_identity();
    trust_storage_provider(&engine, &one_identity, "Provider one");
    trust_storage_provider(&engine, &two_identity, "Provider two");
    trust_storage_provider(&engine, &unselected_identity, "Unselected provider");
    let store_one = ChunkStore::open(provider_one_data.path(), 1_048_576).expect("provider one");
    let store_two = ChunkStore::open(provider_two_data.path(), 1_048_576).expect("provider two");
    let store_unselected =
        ChunkStore::open(unselected_data.path(), 1_048_576).expect("unselected provider");
    engine
        .set_connected_providers(vec![
            provider(one_identity.device_id, &store_one),
            provider(two_identity.device_id, &store_two),
            provider(unselected_identity.device_id, &store_unselected),
        ])
        .expect("connect providers");

    let backup_id = BackupId::new();
    let mut first_options = BackupOptions::new(backup_id, "0001", "backup-one");
    first_options.display_name = "Production scenario".to_owned();
    first_options.created_at_unix_ms = 1;
    first_options.replica_intent =
        ReplicaIntent::explicit([one_identity.device_id, two_identity.device_id]);
    let first = engine
        .backup(source.path(), &first_options, &JobControl::new(), |_| {})
        .expect("first backup");
    assert!(
        first
            .replication
            .is_complete(first.stored_snapshot.chunk_locators.len())
    );
    assert!(first.progress.chunks_deduplicated > 0);
    for locator in &first.stored_snapshot.chunk_locators {
        assert!(
            !store_unselected
                .contains(locator)
                .expect("unselected lookup")
        );
    }

    let resume_backup_id = BackupId::new();
    let resume_control = JobControl::new();
    let mut resume_options = BackupOptions::new(resume_backup_id, "0001", "backup-resume");
    resume_options.display_name = "Resume scenario".to_owned();
    let mut pause_requested = false;
    let paused = engine.backup(
        source.path(),
        &resume_options,
        &resume_control,
        |progress| {
            if !pause_requested && progress.entries_completed > 0 {
                pause_requested = true;
                resume_control.pause();
            }
        },
    );
    assert!(matches!(paused, Err(CoreError::Paused)));
    assert!(pause_requested);
    assert!(
        engine
            .garbage_collect()
            .expect("deferred garbage collection")
            .deferred_active_jobs
    );
    resume_control.resume();
    engine
        .backup(source.path(), &resume_options, &resume_control, |_| {})
        .expect("resumed backup");
    assert!(
        !engine
            .store()
            .has_checkpoint("backup-resume")
            .expect("backup checkpoint")
    );

    let mut second_options = first_options.clone();
    second_options.snapshot_id = "0002".to_owned();
    second_options.job_id = "backup-two".to_owned();
    second_options.key_epoch = 2;
    second_options.created_at_unix_ms = 2;
    let second = engine
        .backup(source.path(), &second_options, &JobControl::new(), |_| {})
        .expect("rotated backup");
    assert!(
        first
            .stored_snapshot
            .chunk_locators
            .is_disjoint(&second.stored_snapshot.chunk_locators)
    );
    let second_retry = engine
        .backup(source.path(), &second_options, &JobControl::new(), |_| {})
        .expect("completed backup retry");
    assert_eq!(second_retry, second);

    let availability = engine
        .verify_snapshot_availability(backup_id, "0002")
        .expect("complete availability");
    assert!(availability.local.is_intact());
    assert_eq!(
        availability.providers.get(&one_identity.device_id),
        Some(&ReplicaAvailability::Complete)
    );
    assert_eq!(
        availability.providers.get(&two_identity.device_id),
        Some(&ReplicaAvailability::Complete)
    );

    let interrupted_restore = tempdir().expect("interrupted restore");
    let restore_control = JobControl::new();
    let plan = engine
        .preview_restore(
            backup_id,
            "0002",
            interrupted_restore.path(),
            &RestoreOptions::all("restore-resume"),
        )
        .expect("restore preview");
    remove_records(engine.store(), &second.stored_snapshot.chunk_locators);
    let pausing_provider = Arc::new(PausingProvider {
        id: one_identity.device_id,
        store: store_one.clone(),
        control: restore_control.clone(),
        paused_once: AtomicBool::new(false),
    });
    engine
        .set_connected_providers(vec![pausing_provider as Arc<dyn ChunkProvider>])
        .expect("connect pausing provider");
    assert!(matches!(
        engine.restore(&plan, &restore_control),
        Err(CoreError::Paused)
    ));
    assert!(
        engine
            .store()
            .has_checkpoint("restore-resume")
            .expect("restore checkpoint")
    );
    restore_control.resume();
    engine
        .restore(&plan, &restore_control)
        .expect("resumed restore");
    assert_eq!(
        fs::read(interrupted_restore.path().join("nested/first.bin"))
            .expect("resumed restored file"),
        repeated
    );
    assert!(interrupted_restore.path().join("nested/empty").is_dir());
    assert_sparse_content(&interrupted_restore.path().join("sparse.bin"));

    engine
        .set_connected_providers(vec![
            provider(one_identity.device_id, &store_one),
            provider(two_identity.device_id, &store_two),
            provider(unselected_identity.device_id, &store_unselected),
        ])
        .expect("reconnect providers");
    assert!(
        engine
            .repair_snapshot(backup_id, "0002")
            .expect("repair missing local records")
            .is_intact()
    );
    let corrupt_locator = second
        .stored_snapshot
        .chunk_locators
        .iter()
        .next()
        .expect("snapshot has chunks")
        .clone();
    flip_record_bit(engine.store(), &corrupt_locator);
    flip_record_bit(&store_one, &corrupt_locator);
    let corrupt = engine
        .verify_snapshot_availability(backup_id, "0002")
        .expect("corrupt availability");
    assert_eq!(corrupt.local.corrupt, vec![corrupt_locator.clone()]);
    assert_eq!(
        corrupt.providers.get(&one_identity.device_id),
        Some(&ReplicaAvailability::Corrupt)
    );
    assert_eq!(
        corrupt.providers.get(&two_identity.device_id),
        Some(&ReplicaAvailability::Complete)
    );
    assert!(
        engine
            .repair_snapshot(backup_id, "0002")
            .expect("authenticated repair")
            .is_intact()
    );

    let source_path = source.path().to_path_buf();
    drop(source);
    assert!(!source_path.exists());
    let final_restore = tempdir().expect("final restore");
    let final_plan = engine
        .preview_restore(
            backup_id,
            "0002",
            final_restore.path(),
            &RestoreOptions::all("restore-final"),
        )
        .expect("final preview");
    engine
        .restore(&final_plan, &JobControl::new())
        .expect("final restore");
    assert_eq!(
        fs::read(final_restore.path().join("nested/second.bin")).expect("final restored file"),
        repeated
    );
    assert_sparse_content(&final_restore.path().join("sparse.bin"));

    fs::write(final_restore.path().join("root.txt"), b"modified").expect("modify conflict");
    let mut replace_options = RestoreOptions::all("restore-replace");
    replace_options.conflict_policy = ConflictPolicy::Replace;
    let replace_plan = engine
        .preview_restore(backup_id, "0002", final_restore.path(), &replace_options)
        .expect("replace preview");
    engine
        .restore(&replace_plan, &JobControl::new())
        .expect("replace restore");
    assert_eq!(
        fs::read(final_restore.path().join("root.txt")).expect("replaced file"),
        b"root content"
    );

    engine
        .revoke_peer(two_identity.device_id)
        .expect("revoke provider");
    let revoked = engine
        .verify_snapshot_availability(backup_id, "0002")
        .expect("revoked availability");
    assert_eq!(
        revoked.providers.get(&two_identity.device_id),
        Some(&ReplicaAvailability::Revoked)
    );
    assert!(matches!(
        engine.set_connected_providers(vec![provider(two_identity.device_id, &store_two)]),
        Err(CoreError::PeerRevoked)
    ));

    drop(engine);
    let reopened = Engine::open(engine_options(node_data.path())).expect("reopen engine");
    assert!(
        reopened
            .verify_snapshot(backup_id, "0002")
            .expect("verify after restart")
            .is_intact()
    );
    assert!(
        reopened
            .config()
            .expect("config after restart")
            .trusted_peers
            .get(&two_identity.device_id)
            .expect("revocation tombstone")
            .revoked
    );
}

#[test]
fn durable_engine_state_has_an_exclusive_process_owner() {
    let data = tempdir().expect("engine data");
    let first = Engine::open(engine_options(data.path())).expect("first engine");
    assert!(matches!(
        Engine::open(engine_options(data.path())),
        Err(CoreError::StateLocked)
    ));
    drop(first);
    Engine::open(engine_options(data.path())).expect("engine after lock release");
}
