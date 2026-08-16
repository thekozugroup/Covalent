use std::collections::BTreeSet;
use std::fs;
use std::hint::black_box;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Instant;

use covalent_core::{
    BackupKey, BackupOptions, ChunkProvider, ChunkStore, ChunkingConfig, ContentDefinedChunker,
    CoreError, DeviceIdentity, Engine, EngineOptions, JobControl, RestoreOptions, StoreProvider,
};
use covalent_protocol::{BackupId, PeerGrant, PeerRole, ReplicaIntent};
use tempfile::tempdir;

fn main() {
    let input = deterministic_bytes(16 * 1_024 * 1_024);
    crypto_threshold(&input);
    checkpoint_scale_threshold();
    provider_restore_threshold(&input);
}

fn crypto_threshold(input: &[u8]) {
    let key = BackupKey::from_bytes([0x2d; 32]);
    let backup_id = BackupId::from_uuid(uuid::Uuid::from_u128(0x42));
    let started = Instant::now();
    let mut chunker = ContentDefinedChunker::new(Cursor::new(input), ChunkingConfig::default());
    let mut processed = 0_usize;
    let mut chunks = 0_usize;
    while let Some(chunk) = chunker.next_chunk().expect("stream chunk") {
        let encrypted = key
            .encrypt_chunk(backup_id, 1, black_box(&chunk))
            .expect("encrypt chunk");
        let plaintext = key
            .decrypt_chunk(backup_id, &encrypted.plaintext_digest, &encrypted)
            .expect("decrypt chunk");
        assert_eq!(plaintext.as_slice(), chunk.as_slice());
        processed += plaintext.len();
        chunks += 1;
    }
    assert_eq!(processed, input.len());
    assert!(chunks > 1);
    let elapsed = started.elapsed();
    let throughput = mebibytes_per_second(input.len(), elapsed.as_secs_f64());
    let minimum = environment_f64("COVALENT_MIN_CRYPTO_MIBPS", 20.0);
    assert!(
        throughput >= minimum,
        "crypto throughput {throughput:.2} MiB/s fell below {minimum:.2} MiB/s"
    );
    eprintln!(
        "engine_smoke crypto: {chunks} chunks, {processed} bytes, {throughput:.2} MiB/s (minimum {minimum:.2})"
    );
}

fn checkpoint_scale_threshold() {
    let entries = environment_usize("COVALENT_SCALE_ENTRIES", 10_000);
    assert!(
        matches!(entries, 10_000 | 100_000 | 1_000_000),
        "COVALENT_SCALE_ENTRIES must be 10000, 100000, or 1000000"
    );
    let data = tempdir().expect("scale data");
    let source = tempdir().expect("scale source");
    let restore_destination = tempdir().expect("scale restore");
    let groups = entries.div_ceil(1_001);
    let files = entries.saturating_sub(groups);
    for group in 0..groups {
        let group_path = source.path().join(format!("group-{group:04}"));
        fs::create_dir(&group_path).expect("scale group");
        let remaining = files.saturating_sub(group * 1_000).min(1_000);
        for index in 0..remaining {
            fs::File::create(group_path.join(format!("entry-{index:04}"))).expect("scale entry");
        }
    }
    let expected_manifest_entries = entries;
    let engine = Engine::open(EngineOptions::new(data.path())).expect("scale engine");
    let options = BackupOptions::new(BackupId::new(), "scale-snapshot", "scale-checkpoint");
    let control = JobControl::new();
    let pause_control = control.clone();
    let mut pause_requested = false;
    let started = Instant::now();
    let paused = engine.backup(source.path(), &options, &control, |progress| {
        if !pause_requested && progress.entries_completed >= expected_manifest_entries / 2 {
            pause_requested = true;
            pause_control.pause();
        }
    });
    assert!(matches!(paused, Err(CoreError::Paused)));
    assert!(
        engine
            .store()
            .has_checkpoint("scale-checkpoint")
            .expect("scale checkpoint")
    );
    control.resume();
    let result = engine
        .backup(source.path(), &options, &control, |_| {})
        .expect("resume scale backup");
    assert_eq!(result.manifest.entries.len(), expected_manifest_entries);
    assert!(
        !engine
            .store()
            .has_checkpoint("scale-checkpoint")
            .expect("completed checkpoint")
    );
    let plan = engine
        .preview_restore(
            options.backup_id,
            &options.snapshot_id,
            restore_destination.path(),
            &RestoreOptions::all("scale-restore"),
        )
        .expect("scale restore preview");
    let restore = engine
        .restore(&plan, &JobControl::new())
        .expect("scale restore");
    assert_eq!(restore.files_restored, files);
    assert_eq!(restore.directories_created, groups);
    assert!(
        !engine
            .store()
            .has_checkpoint("scale-restore")
            .expect("completed restore checkpoint")
    );
    let elapsed = started.elapsed().as_secs_f64();
    let default_maximum = match entries {
        10_000 => 45.0,
        100_000 => 450.0,
        1_000_000 => 4_500.0,
        _ => unreachable!(),
    };
    let maximum = environment_f64("COVALENT_MAX_SCALE_SECONDS", default_maximum);
    assert!(
        elapsed <= maximum,
        "{entries}-entry backup/restore checkpoint run took {elapsed:.2}s, above {maximum:.2}s"
    );
    eprintln!(
        "engine_smoke checkpoint: {entries} manifest entries ({files} files plus {groups} directories), backup pause/resume and restore {elapsed:.2}s (maximum {maximum:.2}s)"
    );
}

fn provider_restore_threshold(input: &[u8]) {
    let minimum = environment_f64("COVALENT_MIN_PROVIDER_RESTORE_MIBPS", 4.0);
    let mut throughputs = Vec::new();
    for provider_count in [1_usize, 2, 4] {
        let data = tempdir().expect("provider data");
        let source = tempdir().expect("provider source");
        let destination = tempdir().expect("provider destination");
        fs::write(source.path().join("payload.bin"), input).expect("provider source file");
        let engine = Engine::open(EngineOptions::new(data.path())).expect("provider engine");
        let mut provider_directories = Vec::new();
        let mut providers = Vec::<Arc<dyn ChunkProvider>>::new();
        let mut provider_ids = BTreeSet::new();
        for index in 0..provider_count {
            let identity = DeviceIdentity::generate().public_identity();
            engine
                .trust_peer(PeerGrant {
                    peer_device_id: identity.device_id,
                    public_key: identity.public_key,
                    display_name: format!("Benchmark provider {index}"),
                    roles: BTreeSet::from([PeerRole::StorageProvider]),
                    confirmed_at_unix_ms: index as u64 + 1,
                    revoked: false,
                })
                .expect("trust benchmark provider");
            let directory = tempdir().expect("provider directory");
            let store = ChunkStore::open(directory.path(), 1_048_576).expect("provider store");
            providers.push(Arc::new(StoreProvider::new(identity.device_id, store)));
            provider_ids.insert(identity.device_id);
            provider_directories.push(directory);
        }
        engine
            .set_connected_providers(providers)
            .expect("connect benchmark providers");
        let backup_id = BackupId::new();
        let mut options = BackupOptions::new(
            backup_id,
            format!("providers-{provider_count}"),
            format!("provider-backup-{provider_count}"),
        );
        options.replica_intent = ReplicaIntent::explicit(provider_ids.clone());
        let backup = engine
            .backup(source.path(), &options, &JobControl::new(), |_| {})
            .expect("provider backup");
        assert!(
            backup
                .replication
                .is_complete(backup.stored_snapshot.chunk_locators.len())
        );
        for locator in &backup.stored_snapshot.chunk_locators {
            let path = engine
                .store()
                .root()
                .join("chunks")
                .join(&locator[..2])
                .join(&locator[2..]);
            fs::remove_file(path).expect("remove local benchmark copy");
        }
        let plan = engine
            .preview_restore(
                backup_id,
                &options.snapshot_id,
                destination.path(),
                &RestoreOptions::all(format!("provider-restore-{provider_count}")),
            )
            .expect("provider preview");
        let started = Instant::now();
        let report = engine
            .restore(&plan, &JobControl::new())
            .expect("provider restore");
        let elapsed = started.elapsed().as_secs_f64();
        let throughput = mebibytes_per_second(input.len(), elapsed);
        assert_eq!(report.bytes_written as usize, input.len());
        assert_eq!(report.provider_chunks.len(), provider_count);
        assert!(
            throughput >= minimum,
            "{provider_count}-provider restore {throughput:.2} MiB/s fell below {minimum:.2} MiB/s"
        );
        assert_eq!(
            fs::read(destination.path().join("payload.bin")).expect("restored payload"),
            input
        );
        eprintln!(
            "engine_smoke providers: {provider_count} source(s), {throughput:.2} MiB/s, distribution {:?}",
            report.provider_chunks
        );
        throughputs.push(throughput);
        black_box(provider_directories);
    }
    assert!(
        throughputs[1] >= throughputs[0] * 0.35 && throughputs[2] >= throughputs[0] * 0.25,
        "provider scaling regressed catastrophically: {throughputs:?} MiB/s"
    );
}

fn deterministic_bytes(length: usize) -> Vec<u8> {
    let mut state = 0x4d59_5df4_d0f3_3173_u64;
    (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn mebibytes_per_second(bytes: usize, seconds: f64) -> f64 {
    bytes as f64 / (1_024.0 * 1_024.0) / seconds.max(f64::EPSILON)
}

fn environment_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().unwrap_or_else(|_| panic!("invalid {name}")))
        .unwrap_or(default)
}

fn environment_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .map(|value| value.parse().unwrap_or_else(|_| panic!("invalid {name}")))
        .unwrap_or(default)
}
