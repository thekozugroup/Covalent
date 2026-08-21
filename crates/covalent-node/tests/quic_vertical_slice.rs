use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read as _, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use covalent_core::{
    BackupOptions, ChunkProvider, Engine, EngineOptions, JobControl, RestoreOptions,
};
use covalent_node::transport::{QuicNode, QuicProvider, TlsIdentity};
use covalent_protocol::{BackupId, PeerRole, ReplicaAvailability, ReplicaIntent};
use tempfile::tempdir;

/// Executes at 64 MiB by default. `COVALENT_QUIC_SCALE_BYTES` raises that, and a
/// value outside the 8 MiB..=10 GiB envelope - or one that is not a plain byte
/// count - now fails the test instead of silently reverting to the default.
///
/// No workflow or script sets `COVALENT_QUIC_SCALE_BYTES` today, so the larger
/// runs below are operator-invoked and must not be described as release
/// validation until something actually invokes them. Capable dedicated hosts can
/// run the production ceiling with
/// `COVALENT_QUIC_SCALE_BYTES=10737418240 cargo test --locked -p covalent-node
/// --test quic_vertical_slice real_quic_scale_backup_and_restore_is_streaming_and_disk_bounded
/// -- --nocapture`.
#[tokio::test(flavor = "multi_thread")]
async fn real_quic_scale_backup_and_restore_is_streaming_and_disk_bounded() {
    let owner_data = tempdir().expect("owner data");
    let provider_data = tempdir().expect("provider data");
    let source = tempdir().expect("source");
    let restore = tempdir().expect("restore");
    fs::create_dir_all(source.path().join("nested/empty")).expect("source directories");
    // `.parse().ok().unwrap_or(64 MiB)` used to swallow a malformed value, so
    // `COVALENT_QUIC_SCALE_BYTES=1GiB` ran a 64 MiB transfer and reported ok -
    // the range assert below then inspected the already-defaulted number and
    // could never catch it. Default only when the variable is genuinely unset.
    let transfer_bytes = match std::env::var("COVALENT_QUIC_SCALE_BYTES") {
        Ok(value) => value.parse::<u64>().unwrap_or_else(|error| {
            panic!(
                "COVALENT_QUIC_SCALE_BYTES={value:?} is not a plain byte count ({error}); \
                 refusing to silently run the 64 MiB default instead"
            )
        }),
        Err(std::env::VarError::NotPresent) => 64_u64 << 20,
        Err(error) => panic!("COVALENT_QUIC_SCALE_BYTES is unreadable: {error}"),
    };
    assert!(
        (8_u64 << 20..=10_u64 << 30).contains(&transfer_bytes),
        "COVALENT_QUIC_SCALE_BYTES={transfer_bytes} is outside the 8 MiB..=10 GiB envelope"
    );
    let source_path = source.path().join("nested/data.bin");
    let mut source_file = File::create(&source_path).expect("source file");
    let mut source_digest = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1_024 * 1_024];
    let mut written = 0_u64;
    let mut block_index = 0_u64;
    while written < transfer_bytes {
        let count = usize::try_from((transfer_bytes - written).min(buffer.len() as u64))
            .expect("write count");
        let mut generator = blake3::Hasher::new_keyed(b"covalent-quic-scale-seed-v1!!!!!");
        generator.update(&block_index.to_le_bytes());
        generator.finalize_xof().fill(&mut buffer[..count]);
        source_file
            .write_all(&buffer[..count])
            .expect("source payload");
        source_digest.update(&buffer[..count]);
        written += u64::try_from(count).expect("written bytes");
        block_index += 1;
    }
    source_file.sync_all().expect("sync source");
    drop(source_file);
    let source_digest = source_digest.finalize();

    let owner = Arc::new(Engine::open(EngineOptions::new(owner_data.path())).expect("owner"));
    let provider =
        Arc::new(Engine::open(EngineOptions::new(provider_data.path())).expect("provider"));
    let invitation = owner
        .pairing_manager()
        .create_invitation(1_000, 60_000, Vec::new())
        .expect("invitation");
    let mut session = provider
        .accept_pairing(
            invitation,
            "Storage provider",
            BTreeSet::from([PeerRole::StorageProvider]),
            BTreeSet::from([PeerRole::BackupReader, PeerRole::BackupWriter]),
            2_000,
        )
        .expect("accept pairing");
    let code = session.authentication_string().to_string();
    // Feeding the session's own code straight back would assert nothing: a
    // derivation returning a constant, or a confirmation that never compared,
    // would satisfy it just as well. Prove the comparison is live before
    // relying on it, so the pairing this transport test is built on is real.
    assert_eq!(
        code.len(),
        19,
        "the short authentication string is four groups of four"
    );
    // Flip the first digit, so the wrong code differs from the right one in
    // exactly one position and the comparison cannot pass by length alone.
    let (first, rest) = code.split_at(1);
    let wrong = format!("{}{rest}", if first == "0" { "1" } else { "0" });
    assert!(
        provider
            .confirm_pairing_as_responder(&mut session, &wrong, 2_000)
            .is_err(),
        "a code that does not match must be refused"
    );
    provider
        .confirm_pairing_as_responder(&mut session, &code, 2_000)
        .expect("responder confirmation");
    owner
        .confirm_pairing_as_inviter(&mut session, &code, 2_000)
        .expect("inviter confirmation");
    owner
        .finalize_pairing_as_inviter(&session, 2_000)
        .expect("owner grant");
    provider
        .finalize_pairing_as_responder(&session, 2_000)
        .expect("provider grant");

    let tls = TlsIdentity::load_or_create(provider_data.path().join("tls")).expect("TLS");
    let node = QuicNode::bind(
        "127.0.0.1:0".parse().expect("address"),
        Arc::clone(&provider),
        &tls,
    )
    .expect("QUIC node");
    let address = node.local_addr().expect("QUIC address");
    let node_task = tokio::spawn(node.run());
    let quic_provider = Arc::new(
        QuicProvider::new(
            address,
            provider.public_identity(),
            tls.certificate_der().to_vec(),
            Arc::clone(&owner),
        )
        .expect("QUIC provider"),
    );
    owner
        .set_connected_providers(vec![Arc::clone(&quic_provider) as Arc<dyn ChunkProvider>])
        .expect("connect provider");

    let backup_id = BackupId::new();
    let mut options = BackupOptions::new(backup_id, "0001", "quic-backup");
    options.display_name = "QUIC vertical slice".to_owned();
    options.replica_intent = ReplicaIntent::explicit([provider.device_id()]);
    options.created_at_unix_ms = 3_000;
    // `resident_set_bytes().unwrap_or(0)` used to stand in for both the baseline
    // and the peak. With `ps` absent from PATH the growth computed as 0 - 0 = 0
    // and the 192 MiB ceiling below passed having measured nothing at all.
    // A ceiling that cannot be measured is not a ceiling; say so and stop.
    let baseline_rss = resident_set_bytes().expect(
        "resident set size is unreadable on this host, so the streaming RSS ceiling \
         would assert against a measurement of zero",
    );
    let rss_sampler = RssSampler::start();
    let disk_sampler = DiskSampler::start([
        owner_data.path().to_path_buf(),
        provider_data.path().to_path_buf(),
    ]);
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10 * 60);
    let backup_control = JobControl::new();
    let mut backup_task = tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        let source_path = source.path().to_path_buf();
        let control = backup_control.clone();
        move || owner.backup(source_path, &options, &control, |_| {})
    });
    let backup = tokio::select! {
        result = &mut backup_task => result.expect("backup worker").expect("backup"),
        () = tokio::time::sleep_until(deadline) => {
            backup_control.cancel();
            let _ = backup_task.await;
            panic!("real QUIC scale transfer exceeded the ten-minute gate during backup");
        }
    };
    assert!(
        backup
            .replication
            .is_complete(backup.stored_snapshot.chunk_locators.len()),
        "QUIC replication incomplete: required={} acknowledgements={:?} recovery_acknowledgements={:?} failures={:?}",
        backup.stored_snapshot.chunk_locators.len(),
        backup.replication.acknowledgements,
        backup.replication.recovery_catalog_acknowledgements,
        backup.replication.failures
    );
    let provider_chunk_root = provider.store().root().join("chunks");
    let provider_chunk_bytes = tree_file_bytes(&provider_chunk_root);
    let provider_chunk_count = tree_file_count(&provider_chunk_root);
    let minimum_chunk_count = transfer_bytes.div_ceil(1_024 * 1_024);
    assert!(
        provider_chunk_bytes >= transfer_bytes.saturating_mul(9) / 10,
        "provider stored only {provider_chunk_bytes} encrypted chunk bytes for {transfer_bytes} nonrepeating source bytes"
    );
    assert!(
        provider_chunk_count >= minimum_chunk_count,
        "provider stored only {provider_chunk_count} chunk records; expected at least {minimum_chunk_count}"
    );
    assert_eq!(
        provider_chunk_count,
        u64::try_from(backup.stored_snapshot.chunk_locators.len()).expect("locator count")
    );

    let availability = tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        move || owner.verify_snapshot_availability(backup_id, "0001")
    })
    .await
    .expect("verify worker")
    .expect("verify");
    assert_eq!(
        availability.providers.get(&provider.device_id()),
        Some(&ReplicaAvailability::Complete)
    );

    for locator in &backup.stored_snapshot.chunk_locators {
        fs::remove_file(
            owner
                .store()
                .root()
                .join("chunks")
                .join(&locator[..2])
                .join(&locator[2..]),
        )
        .expect("remove local chunk");
    }
    assert_eq!(
        tree_file_count(&owner.store().root().join("chunks")),
        0,
        "owner chunks must be absent before provider-only restore"
    );
    let source_directory = source.path().to_path_buf();
    drop(source);
    assert!(!source_directory.exists());
    let plan = owner
        .preview_restore(
            backup_id,
            "0001",
            restore.path(),
            &RestoreOptions::all("quic-restore"),
        )
        .expect("preview");
    let restore_control = JobControl::new();
    let mut restore_task = tokio::task::spawn_blocking({
        let owner = Arc::clone(&owner);
        let control = restore_control.clone();
        move || owner.restore(&plan, &control)
    });
    tokio::select! {
        result = &mut restore_task => {
            result.expect("restore worker").expect("restore");
        }
        () = tokio::time::sleep_until(deadline) => {
            restore_control.cancel();
            let _ = restore_task.await;
            panic!("real QUIC scale transfer exceeded the ten-minute gate during restore");
        }
    }
    let restored_path = restore.path().join("nested/data.bin");
    assert_eq!(
        fs::metadata(&restored_path).expect("restored file").len(),
        transfer_bytes
    );
    let mut restored = File::open(&restored_path).expect("open restored file");
    let mut restored_digest = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 1_024 * 1_024];
    loop {
        let read = restored.read(&mut buffer).expect("read restored file");
        if read == 0 {
            break;
        }
        restored_digest.update(&buffer[..read]);
    }
    assert_eq!(restored_digest.finalize(), source_digest);
    assert!(restore.path().join("nested/empty").is_dir());
    let peak_rss = rss_sampler.finish();
    let peak_disk = disk_sampler.finish();
    let rss_growth = peak_rss.saturating_sub(baseline_rss);
    let owner_disk = tree_file_bytes(owner_data.path());
    let provider_disk = tree_file_bytes(provider_data.path());
    let disk_budget = transfer_bytes.saturating_mul(3).saturating_add(32 << 20);
    assert!(
        owner_disk.saturating_add(provider_disk) <= disk_budget,
        "encrypted owner/provider storage {} exceeded {} for {} source bytes",
        owner_disk.saturating_add(provider_disk),
        disk_budget,
        transfer_bytes
    );
    assert!(
        peak_disk <= disk_budget,
        "peak encrypted owner/provider storage {peak_disk} exceeded {disk_budget} for {transfer_bytes} source bytes"
    );
    assert!(
        peak_rss >= baseline_rss,
        "peak RSS {peak_rss} is below the {baseline_rss} baseline, so the sampler \
         never observed the transfer"
    );
    assert!(
        rss_growth <= 192_u64 << 20,
        "peak RSS grew by {rss_growth} bytes for a {transfer_bytes}-byte transfer"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10 * 60),
        "real QUIC scale transfer exceeded the ten-minute gate"
    );
    eprintln!(
        "QUIC scale: architecture={} bytes={transfer_bytes} elapsed_ms={} peak_rss_bytes={peak_rss} rss_growth_bytes={rss_growth} provider_chunk_bytes={provider_chunk_bytes} provider_chunk_count={provider_chunk_count} peak_owner_provider_disk_bytes={peak_disk} final_owner_provider_disk_bytes={}",
        std::env::consts::ARCH,
        started.elapsed().as_millis(),
        owner_disk.saturating_add(provider_disk)
    );

    owner
        .revoke_peer(provider.device_id())
        .expect("revoke provider");
    let revoked = owner
        .verify_snapshot_availability(backup_id, "0001")
        .expect("revoked availability");
    assert_eq!(
        revoked.providers.get(&provider.device_id()),
        Some(&ReplicaAvailability::Revoked)
    );
    node_task.abort();
}

#[test]
fn repeated_pattern_backup_deduplicates_physical_storage() {
    let state = tempdir().expect("dedup state");
    let source = tempdir().expect("dedup source");
    let source_path = source.path().join("repeated.bin");
    let mut source_file = File::create(&source_path).expect("dedup source file");
    let pattern: Vec<_> = (0..1_024 * 1_024)
        .map(|index| (index % 239) as u8)
        .collect();
    for _ in 0..8 {
        source_file
            .write_all(&pattern)
            .expect("dedup source payload");
    }
    source_file.sync_all().expect("sync dedup source");
    drop(source_file);

    let engine = Engine::open(EngineOptions::new(state.path())).expect("dedup engine");
    let backup_id = BackupId::new();
    let options = BackupOptions::new(backup_id, "0001", "dedup-regression");
    let result = engine
        .backup(source.path(), &options, &JobControl::new(), |_| {})
        .expect("dedup backup");
    let physical = tree_file_bytes(&engine.store().root().join("chunks"));
    assert!(
        physical < 2_u64 << 20,
        "8 MiB repeated source used {physical} physical encrypted chunk bytes"
    );
    assert!(result.stored_snapshot.chunk_locators.len() < 16);
    assert!(
        engine
            .verify_snapshot(backup_id, "0001")
            .expect("verify dedup")
            .is_intact()
    );
}

fn tree_file_bytes(root: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .map(|entry| entry.expect("walk scale data"))
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.metadata().expect("scale file metadata").len())
        .try_fold(0_u64, u64::checked_add)
        .expect("scale disk usage")
}

fn tree_file_count(root: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .map(|entry| entry.expect("walk scale data"))
        .filter(|entry| entry.file_type().is_file())
        .count()
        .try_into()
        .expect("scale file count")
}

struct RssSampler {
    running: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    /// Samples the probe could not take. A silently-skipped sample is
    /// indistinguishable from a flat memory profile, so count them and refuse
    /// to report a peak that was never actually observed.
    failed_samples: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}

struct DiskSampler {
    running: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    worker: Option<thread::JoinHandle<()>>,
}

impl RssSampler {
    fn start() -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let peak = Arc::new(AtomicU64::new(
            resident_set_bytes().expect("resident set size probe"),
        ));
        let failed_samples = Arc::new(AtomicU64::new(0));
        let worker = thread::spawn({
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            let failed_samples = Arc::clone(&failed_samples);
            move || {
                while running.load(Ordering::Relaxed) {
                    match resident_set_bytes() {
                        Some(current) => {
                            peak.fetch_max(current, Ordering::Relaxed);
                        }
                        // The `if let Some(_)` this replaces dropped failed
                        // samples on the floor, so a probe that stopped working
                        // mid-run left a stale peak that still looked plausible.
                        None => {
                            failed_samples.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self {
            running,
            peak,
            failed_samples,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> u64 {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("RSS sampler");
        }
        let failed_samples = self.failed_samples.load(Ordering::Relaxed);
        assert_eq!(
            failed_samples, 0,
            "the resident-set probe failed {failed_samples} times during the transfer, \
             so the peak below is not a real measurement"
        );
        let peak = self.peak.load(Ordering::Relaxed);
        assert!(
            peak > 0,
            "the resident-set probe reported a peak of zero bytes, which no live \
             process has; the RSS ceiling would be asserting against nothing"
        );
        peak
    }
}

impl Drop for RssSampler {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl DiskSampler {
    fn start(roots: impl IntoIterator<Item = std::path::PathBuf>) -> Self {
        let roots: Vec<_> = roots.into_iter().collect();
        let running = Arc::new(AtomicBool::new(true));
        let peak = Arc::new(AtomicU64::new(sample_tree_file_bytes(&roots)));
        let worker = thread::spawn({
            let running = Arc::clone(&running);
            let peak = Arc::clone(&peak);
            move || {
                while running.load(Ordering::Relaxed) {
                    peak.fetch_max(sample_tree_file_bytes(&roots), Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Self {
            running,
            peak,
            worker: Some(worker),
        }
    }

    fn finish(mut self) -> u64 {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("disk sampler");
        }
        self.peak.load(Ordering::Relaxed)
    }
}

impl Drop for DiskSampler {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn sample_tree_file_bytes(roots: &[std::path::PathBuf]) -> u64 {
    roots
        .iter()
        .flat_map(|root| {
            walkdir::WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
        })
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| entry.metadata().ok())
        .map(|metadata| metadata.len())
        .fold(0_u64, u64::saturating_add)
}

#[cfg(target_os = "linux")]
fn resident_set_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()?;
    kib.checked_mul(1_024)
}

#[cfg(not(target_os = "linux"))]
fn resident_set_bytes() -> Option<u64> {
    let output = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()?
        .checked_mul(1_024)
}
