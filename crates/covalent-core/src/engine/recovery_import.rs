//! Bounded streaming selection of authenticated recovery catalogs.
use super::*;
use crate::RecoveryCatalogSink;
use crate::replication::{ProviderHealth, error_category};

// Serialized retained capsules plus one in-flight capsule and charged index entries.
// Decryption and parsing add bounded per-capsule working memory; this is not an RSS limit.
const MAX_LIVE_CATALOG_BYTES: u64 = 384 * 1_024 * 1_024;
const MAX_CATALOGS: usize = 1_000_000;
const EVIDENCE_BYTES: u64 = 512;
const CANDIDATE_OVERHEAD: u64 = 16 * 1_024;

type Candidate = (RecoveryCapsule, BTreeSet<DeviceId>);

pub(super) struct CatalogSelection {
    pub latest: BTreeMap<BackupId, Candidate>,
    pub queried: BTreeSet<DeviceId>,
    pub failures: Vec<ProviderFailure>,
    pub blocked: bool,
}

struct Accumulator<'a> {
    engine: &'a Engine,
    control: &'a JobControl,
    latest: BTreeMap<BackupId, Candidate>,
    retained_sizes: BTreeMap<BackupId, u64>,
    verified: BTreeMap<(BackupId, String), [u8; 32]>,
    unusable: BTreeMap<BackupId, (u64, String)>,
    failures: Vec<ProviderFailure>,
    live_bytes: u64,
    maximum_bytes: u64,
    count: usize,
    fatal: bool,
}

impl CatalogSelection {
    pub(super) fn collect(
        engine: &Engine,
        scheduler: &ReplicationScheduler,
        control: &JobControl,
    ) -> Result<Self, CoreError> {
        Self::collect_with_limit(engine, scheduler, control, MAX_LIVE_CATALOG_BYTES)
    }

    fn collect_with_limit(
        engine: &Engine,
        scheduler: &ReplicationScheduler,
        control: &JobControl,
        maximum_bytes: u64,
    ) -> Result<Self, CoreError> {
        let mut accumulator = Accumulator {
            engine,
            control,
            latest: BTreeMap::new(),
            retained_sizes: BTreeMap::new(),
            verified: BTreeMap::new(),
            unusable: BTreeMap::new(),
            failures: Vec::new(),
            live_bytes: 0,
            maximum_bytes,
            count: 0,
            fatal: false,
        };
        let mut queried = BTreeSet::new();
        for (provider_id, provider) in scheduler.recovery_providers() {
            control.check()?;
            if provider.health() != ProviderHealth::Online {
                accumulator.failure(provider_id, None, "recovery_catalog_provider_offline");
                continue;
            }
            let mut sink = ProviderSink {
                accumulator: &mut accumulator,
                provider_id,
                reserved: None,
            };
            let result = provider.visit_recovery_capsules(control, &mut sink);
            // A failed fetch may not consume its reservation. It never becomes retained state.
            let unfinished = sink.reserved.take().is_some();
            control.check()?;
            match result {
                Ok(()) if !accumulator.fatal && !unfinished => {
                    queried.insert(provider_id);
                }
                Ok(()) => return Err(CoreError::ResourceLimit("recovery catalog selection")),
                Err(error) if accumulator.fatal => return Err(error),
                Err(error) => accumulator.failure(
                    provider_id,
                    None,
                    &format!("recovery_catalog_{}", error_category(&error)),
                ),
            }
        }
        accumulator.failures.sort_by(|a, b| {
            (a.provider_id, &a.locator, &a.reason).cmp(&(b.provider_id, &b.locator, &b.reason))
        });
        accumulator.failures.dedup();
        let blocked = (accumulator.latest.is_empty() && !accumulator.failures.is_empty())
            || accumulator.unusable.iter().any(|(backup, evidence)| {
                accumulator.latest.get(backup).is_none_or(|(candidate, _)| {
                    (evidence.0, evidence.1.as_str())
                        >= (
                            candidate.committed_at_unix_ms,
                            candidate.snapshot_id.as_str(),
                        )
                })
            });
        Ok(Self {
            latest: accumulator.latest,
            queried,
            failures: accumulator.failures,
            blocked,
        })
    }
}

impl Accumulator<'_> {
    fn ensure_budget(&mut self, additional: u64) -> Result<(), CoreError> {
        if additional > self.maximum_bytes.saturating_sub(self.live_bytes) {
            self.fatal = true;
            return Err(CoreError::ResourceLimit("recovery catalog selection"));
        }
        Ok(())
    }

    fn failure(&mut self, provider_id: DeviceId, snapshot: Option<String>, reason: &str) {
        retain_recovery_failure(
            &mut self.failures,
            ProviderFailure {
                provider_id,
                locator: snapshot,
                reason: reason.to_owned(),
            },
        );
    }

    fn rejected(
        &mut self,
        provider: DeviceId,
        capsule: &RecoveryCapsule,
        signed: bool,
    ) -> Result<(), CoreError> {
        self.failure(
            provider,
            Some(capsule.snapshot_id.clone()),
            "recovery_catalog_authentication_failed",
        );
        if signed {
            if !self.unusable.contains_key(&capsule.backup_id) {
                self.ensure_budget(EVIDENCE_BYTES)?;
                self.live_bytes += EVIDENCE_BYTES;
            }
            retain_latest_recovery_evidence(&mut self.unusable, capsule);
        }
        Ok(())
    }

    fn accept(
        &mut self,
        provider: DeviceId,
        capsule: RecoveryCapsule,
        serialized_bytes: u64,
    ) -> Result<(), CoreError> {
        self.control.check()?;
        let identity = self.engine.identity.public_identity();
        if capsule.verify_signature(&identity).is_err() {
            return self.rejected(provider, &capsule, false);
        }
        let opened = capsule
            .open(&self.engine.recovery_master, &identity)
            .and_then(|opened| {
                let manifest =
                    decrypt_manifest(&opened.snapshot.envelope, &opened.backup_key, &identity)?;
                Ok((opened, manifest))
            });
        self.control.check()?;
        let (opened, manifest) = match opened {
            Ok(value) => value,
            Err(_) => return self.rejected(provider, &capsule, true),
        };
        let provider_ids: BTreeSet<_> = opened
            .provider_directory
            .iter()
            .map(|entry| entry.grant.peer_device_id)
            .collect();
        if (!provider_ids.is_empty() && provider_ids != manifest.replica_intent.selected_providers)
            || manifest.backup_id != capsule.backup_id
            || manifest.snapshot_id != opened.snapshot.snapshot_id
            || manifest.created_at_unix_ms != opened.snapshot.committed_at_unix_ms
            || !manifest_locators_match(&manifest, &opened.snapshot.chunk_locators)
        {
            return self.rejected(provider, &capsule, true);
        }
        if !provider_ids.is_empty() && !provider_ids.contains(&provider) {
            return self.rejected(provider, &capsule, false);
        }
        // Plaintext manifest and content key are discarded before retaining the candidate.
        drop(manifest);
        drop(opened);
        let mut digest = blake3::Hasher::new();
        serde_json::to_writer(&mut digest, &capsule)?;
        let digest = *digest.finalize().as_bytes();
        let key = (capsule.backup_id, capsule.snapshot_id.clone());
        if let Some(previous) = self.verified.get(&key) {
            if previous != &digest {
                self.fatal = true;
                return Err(CoreError::AuthenticationFailed);
            }
        } else {
            self.ensure_budget(EVIDENCE_BYTES)?;
            self.live_bytes += EVIDENCE_BYTES;
            self.verified.insert(key, digest);
        }
        if let Some((current, providers)) = self.latest.get_mut(&capsule.backup_id) {
            if current.snapshot_id == capsule.snapshot_id {
                providers.insert(provider);
                return Ok(());
            }
            if (current.committed_at_unix_ms, &current.snapshot_id)
                > (capsule.committed_at_unix_ms, &capsule.snapshot_id)
            {
                return Ok(());
            }
        } else {
            if self.latest.len() == MAX_REMEMBERED_BACKUPS {
                self.fatal = true;
                return Err(CoreError::ResourceLimit("recovered backups"));
            }
            self.ensure_budget(CANDIDATE_OVERHEAD)?;
            self.live_bytes += CANDIDATE_OVERHEAD;
        }
        let previous_size = self
            .retained_sizes
            .insert(capsule.backup_id, serialized_bytes)
            .unwrap_or(0);
        self.live_bytes -= previous_size;
        self.ensure_budget(serialized_bytes)?;
        self.live_bytes += serialized_bytes;
        self.latest
            .insert(capsule.backup_id, (capsule, BTreeSet::from([provider])));
        Ok(())
    }
}

struct ProviderSink<'a, 'b> {
    accumulator: &'a mut Accumulator<'b>,
    provider_id: DeviceId,
    reserved: Option<u64>,
}

impl RecoveryCatalogSink for ProviderSink<'_, '_> {
    fn reserve(&mut self, serialized_bytes: u64) -> Result<(), CoreError> {
        self.accumulator.control.check()?;
        if self.reserved.is_some()
            || self.accumulator.count == MAX_CATALOGS
            || serialized_bytes == 0
        {
            self.accumulator.fatal = true;
            return Err(CoreError::ResourceLimit("recovery catalog listing"));
        }
        self.accumulator
            .ensure_budget(serialized_bytes.saturating_add(EVIDENCE_BYTES + CANDIDATE_OVERHEAD))?;
        self.accumulator.count += 1;
        self.reserved = Some(serialized_bytes);
        Ok(())
    }

    fn accept(&mut self, capsule: RecoveryCapsule) -> Result<(), CoreError> {
        let Some(size) = self.reserved.take() else {
            self.accumulator.fatal = true;
            return Err(CoreError::InvalidState(
                "recovery catalog was not reserved".to_owned(),
            ));
        };
        self.accumulator.accept(self.provider_id, capsule, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use tempfile::TempDir;

    struct Fixture {
        _root: TempDir,
        engine: Engine,
        capsules: Vec<RecoveryCapsule>,
        backup_id: BackupId,
    }

    fn fixture(count: usize) -> Fixture {
        let root = TempDir::new().expect("root");
        let engine = Engine::open(
            EngineOptions::new(root.path().join("owner")).with_key_protector(Arc::new(
                crate::StaticKeyProtector::new(1, [0x65; 32]).expect("protector"),
            )),
        )
        .expect("engine");
        let source = root.path().join("source");
        fs::create_dir(&source).expect("source");
        fs::write(source.join("data.txt"), b"streamed recovery evidence").expect("source data");
        let backup_id = BackupId::new();
        let mut capsules = Vec::new();
        for index in 0..count {
            let mut options = BackupOptions::new(
                backup_id,
                format!("snapshot-{index:04}"),
                format!("job-{index:04}"),
            );
            options.created_at_unix_ms = index as u64;
            let result = engine
                .backup(&source, &options, &JobControl::new(), |_| {})
                .expect("backup");
            capsules.push(
                RecoveryCapsule::seal(
                    &result.stored_snapshot,
                    "Recovery fixture",
                    &engine.load_backup_key(backup_id).expect("key"),
                    &engine.recovery_master,
                    &engine.identity,
                    Vec::new(),
                )
                .expect("capsule"),
            );
            engine
                .acknowledge_backup_result(&options.job_id)
                .expect("acknowledge");
        }
        Fixture {
            _root: root,
            engine,
            capsules,
            backup_id,
        }
    }

    struct StreamingProvider {
        id: DeviceId,
        capsules: Vec<RecoveryCapsule>,
        cancel_after_first: bool,
    }
    impl ChunkProvider for StreamingProvider {
        fn device_id(&self) -> DeviceId {
            self.id
        }
        fn health(&self) -> ProviderHealth {
            ProviderHealth::Online
        }
        fn put(&self, _: &str, _: &[u8]) -> Result<(), CoreError> {
            unreachable!()
        }
        fn get(&self, _: &str) -> Result<Vec<u8>, CoreError> {
            unreachable!()
        }
        fn contains(&self, _: &str) -> Result<bool, CoreError> {
            unreachable!()
        }
        fn list_recovery_capsules(&self) -> Result<Vec<RecoveryCapsule>, CoreError> {
            panic!("engine must use streaming visitor")
        }
        fn visit_recovery_capsules(
            &self,
            control: &JobControl,
            sink: &mut dyn RecoveryCatalogSink,
        ) -> Result<(), CoreError> {
            for (index, capsule) in self.capsules.iter().enumerate() {
                control.check()?;
                sink.reserve(serde_json::to_vec(capsule)?.len() as u64)?;
                sink.accept(capsule.clone())?;
                if index == 0 && self.cancel_after_first {
                    control.cancel();
                }
            }
            Ok(())
        }
    }

    fn scheduler(capsules: Vec<RecoveryCapsule>, cancel_after_first: bool) -> ReplicationScheduler {
        ReplicationScheduler::new(
            vec![Arc::new(StreamingProvider {
                id: DeviceId::new(),
                capsules,
                cancel_after_first,
            }) as Arc<dyn ChunkProvider>],
            1,
        )
        .expect("scheduler")
    }

    #[test]
    fn streaming_history_releases_old_capsules_and_keeps_latest_under_small_budget() {
        let fixture = fixture(32);
        let sizes: Vec<_> = fixture
            .capsules
            .iter()
            .map(|capsule| serde_json::to_vec(capsule).expect("serialize").len() as u64)
            .collect();
        let budget =
            sizes.iter().max().expect("size") * 2 + EVIDENCE_BYTES * 33 + CANDIDATE_OVERHEAD * 2;
        assert!(
            sizes.iter().sum::<u64>() > budget,
            "full history cannot fit in test budget"
        );
        for newest_first in [false, true] {
            let mut capsules = fixture.capsules.clone();
            if newest_first {
                capsules.reverse();
            }
            let selection = CatalogSelection::collect_with_limit(
                &fixture.engine,
                &scheduler(capsules, false),
                &JobControl::new(),
                budget,
            )
            .expect("stream history in either order");
            assert!(!selection.blocked);
            assert_eq!(selection.latest.len(), 1);
            assert_eq!(
                selection.latest[&fixture.backup_id].0.snapshot_id,
                "snapshot-0031"
            );
        }
        let before = fixture.engine.config().expect("config");
        assert!(matches!(
            CatalogSelection::collect_with_limit(
                &fixture.engine,
                &scheduler(fixture.capsules.clone(), false),
                &JobControl::new(),
                1
            ),
            Err(CoreError::ResourceLimit(_))
        ));
        assert_eq!(
            before,
            fixture.engine.config().expect("unchanged durable config")
        );
    }

    #[test]
    fn discarded_history_still_rejects_conflicting_authenticated_duplicate() {
        let fixture = fixture(2);
        let first = fixture.capsules[0]
            .open(
                &fixture.engine.recovery_master,
                &fixture.engine.public_identity(),
            )
            .expect("open first");
        let conflicting = RecoveryCapsule::seal(
            &first.snapshot,
            "Changed authenticated name",
            &first.backup_key,
            &fixture.engine.recovery_master,
            &fixture.engine.identity,
            Vec::new(),
        )
        .expect("conflicting signed capsule");
        let mut capsules = fixture.capsules.clone();
        capsules.push(conflicting);
        assert!(matches!(
            CatalogSelection::collect(
                &fixture.engine,
                &scheduler(capsules, false),
                &JobControl::new()
            ),
            Err(CoreError::AuthenticationFailed)
        ));
    }

    #[test]
    fn cancellation_mid_stream_discards_selection_and_releases_engine_lock() {
        let fixture = fixture(2);
        let before = fixture.engine.config().expect("config");
        let control = JobControl::new();
        assert!(matches!(
            CatalogSelection::collect(
                &fixture.engine,
                &scheduler(fixture.capsules.clone(), true),
                &control
            ),
            Err(CoreError::Cancelled)
        ));
        assert_eq!(before, fixture.engine.config().expect("config"));
        assert!(fixture.engine.backup_lock.try_lock().is_ok());
    }

    #[test]
    fn recovery_cancel_does_not_wait_for_busy_backup_lock() {
        let fixture = fixture(1);
        let engine = Arc::new(fixture.engine);
        let guard = engine.backup_lock.lock().expect("hold engine lock");
        let control = JobControl::new();
        let worker_control = control.clone();
        let worker_engine = Arc::clone(&engine);
        let (sent, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            sent.send(worker_engine.import_recovery_catalogs_report_controlled(&worker_control))
                .expect("result");
        });
        control.cancel();
        assert!(matches!(
            received
                .recv_timeout(Duration::from_secs(1))
                .expect("bounded cancellation"),
            Err(CoreError::Cancelled)
        ));
        drop(guard);
        worker.join().expect("worker");
    }

    #[test]
    fn recovery_retry_preserves_a_newer_durable_local_snapshot() {
        let fixture = fixture(2);
        *fixture.engine.scheduler.lock().expect("scheduler") =
            scheduler(vec![fixture.capsules[0].clone()], false);
        let report = fixture
            .engine
            .import_recovery_catalogs_report()
            .expect("conservative result");
        assert!(report.blocked && report.newer_snapshot_may_exist);
        assert!(report.recovered_backups.is_empty());
        assert_eq!(
            fixture.engine.config().expect("config").remembered_backups[&fixture.backup_id]
                .latest_snapshot_id
                .as_deref(),
            Some("snapshot-0001")
        );
        assert!(
            report
                .failures
                .iter()
                .any(|failure| failure.reason == "recovery_catalog_older_than_local")
        );
    }
    #[test]
    fn interrupted_import_watermark_survives_restart_before_remembered_publication() {
        for boundary in 1..=2 {
            let fixture = fixture(2);
            let recovery_path = fixture._root.path().join("recovered");
            let unlock = RecoveryUnlockKey::generate();
            let kit = fixture.engine.export_recovery_kit(&unlock).expect("kit");
            let options = || {
                EngineOptions::new(&recovery_path).with_key_protector(Arc::new(
                    crate::StaticKeyProtector::new(1, [0x66; 32]).expect("protector"),
                ))
            };
            let recovered =
                Engine::recover_from_kit(options(), &kit, &unlock).expect("recover identity");
            *recovered.scheduler.lock().expect("scheduler") =
                scheduler(vec![fixture.capsules[1].clone()], false);
            RECOVERY_IMPORT_FAILPOINT.with(|failpoint| failpoint.set(boundary));
            assert!(matches!(
                recovered.import_recovery_catalogs_report(),
                Err(CoreError::Cancelled)
            ));
            assert!(
                recovered
                    .config()
                    .expect("config")
                    .remembered_backups
                    .is_empty()
            );
            drop(recovered);
            let reopened = Engine::open(options()).expect("restart after interrupted import");
            *reopened.scheduler.lock().expect("scheduler") =
                scheduler(vec![fixture.capsules[0].clone()], false);
            let older = reopened
                .import_recovery_catalogs_report()
                .expect("older provider response");
            assert!(older.blocked && older.newer_snapshot_may_exist);
            assert!(
                reopened
                    .config()
                    .expect("config")
                    .remembered_backups
                    .is_empty()
            );
            *reopened.scheduler.lock().expect("scheduler") =
                scheduler(vec![fixture.capsules[1].clone()], false);
            let resumed = reopened
                .import_recovery_catalogs_report()
                .expect("retry exact newer snapshot");
            assert!(!resumed.blocked);
            assert_eq!(resumed.recovered_backups[0].snapshot_id, "snapshot-0001");
        }
    }

    #[test]
    fn recovery_watermarks_cannot_publish_config_larger_than_its_read_limit() {
        let mut config = NodeConfig::new("Configuration size regression", false).expect("config");
        for _ in 0..MAX_REMEMBERED_BACKUPS {
            config.recovery_snapshot_cursors.insert(
                BackupId::new(),
                RecoverySnapshotCursor {
                    committed_at_unix_ms: 1,
                    snapshot_id: "a".repeat(128),
                },
            );
        }
        assert!(matches!(
            config.validate(),
            Err(CoreError::ResourceLimit("node configuration"))
        ));
    }
}
