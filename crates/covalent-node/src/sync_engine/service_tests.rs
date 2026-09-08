use super::service::{TestBackend, TestScanBehavior, TestStopBehavior};
use super::*;
use crate::transport::TlsIdentity;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{Engine, EngineOptions, KeyProtector, StaticKeyProtector};
use covalent_protocol::{FolderShareCommit, PeerRole, TransportBinding};
use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

const QUIET_HEALTH: Duration = Duration::from_secs(3600);

struct Device {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    engine: Arc<Engine>,
    installation: Arc<EngineInstallation>,
    protector: Arc<dyn KeyProtector>,
    address: std::net::SocketAddr,
}

impl Device {
    fn new(name: &str, port: u16) -> Self {
        let temporary = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = std::fs::canonicalize(temporary.path()).unwrap();
        let protector: Arc<dyn KeyProtector> =
            Arc::new(StaticKeyProtector::new(1, [67; 32]).unwrap());
        let engine = Arc::new(
            Engine::open(EngineOptions {
                initial_device_name: name.into(),
                ..EngineOptions::new(root.join("node")).with_key_protector(Arc::clone(&protector))
            })
            .unwrap(),
        );
        let sync = root.join("sync");
        std::fs::create_dir(&sync).unwrap();
        std::fs::set_permissions(&sync, std::fs::Permissions::from_mode(0o700)).unwrap();
        let installation = Arc::new(EngineInstallation::create(&sync, protector.as_ref()).unwrap());
        std::fs::create_dir(root.join("files")).unwrap();
        Self {
            _temporary: temporary,
            root,
            engine,
            installation,
            protector,
            address: ([127, 0, 0, 1], port).into(),
        }
    }

    fn journal(&self) -> FolderSharingJournal {
        FolderSharingJournal::create(
            Arc::clone(&self.engine),
            Arc::clone(&self.installation),
            Arc::clone(&self.protector),
            self.address,
            self.address,
        )
        .unwrap()
    }

    fn reopen(&self) -> FolderSharingJournal {
        FolderSharingJournal::open(
            Arc::clone(&self.engine),
            Arc::clone(&self.installation),
            Arc::clone(&self.protector),
        )
        .unwrap()
    }

    fn files(&self) -> PathBuf {
        self.root.join("files")
    }

    fn transport(&self) -> TransportBinding {
        let tls =
            TlsIdentity::load_or_create(self.root.join("tls"), &self.root, self.protector.as_ref())
                .unwrap();
        TransportBinding {
            peer_id: self.engine.device_id(),
            display_name: self.engine.config().unwrap().device_name,
            address: self.address.to_string(),
            certificate_der: URL_SAFE_NO_PAD.encode(tls.certificate_der()),
            certificate_fingerprint: tls.certificate_fingerprint(),
        }
    }

    fn service(
        &self,
        journal: FolderSharingJournal,
        backend: Arc<TestBackend>,
    ) -> FolderSyncService {
        FolderSyncService::new_for_test(journal, Arc::clone(&self.engine), backend, QUIET_HEALTH)
    }
}

fn pair(first: &Device, second: &Device) {
    let invitation = first
        .engine
        .pairing_manager()
        .create_invitation_with_transport(
            1000,
            60_000,
            vec![first.address.to_string()],
            first.transport(),
        )
        .unwrap();
    let roles = BTreeSet::from([PeerRole::BackupReader]);
    let mut session = second
        .engine
        .accept_pairing_with_transport(invitation, second.transport(), roles.clone(), roles, 1001)
        .unwrap();
    let code = session.authentication_string().as_str().to_owned();
    second
        .engine
        .confirm_pairing_as_responder(&mut session, &code, 1002)
        .unwrap();
    first
        .engine
        .confirm_pairing_as_inviter(&mut session, &code, 1003)
        .unwrap();
    first
        .engine
        .finalize_pairing_as_inviter(&session, 1004)
        .unwrap();
    second
        .engine
        .finalize_pairing_as_responder(&session, 1004)
        .unwrap();
}

async fn make_ready(
    first: &Device,
    second: &Device,
    source: &FolderSyncService,
    target: &FolderSyncService,
) -> (Uuid, FolderShareCommit) {
    let folder = Uuid::new_v4();
    let offer = source
        .offer(
            second.engine.device_id(),
            folder,
            "Documents",
            &first.files(),
            2000,
        )
        .await
        .unwrap()
        .into_value();
    target.receive_offer(offer.clone(), 2001).await.unwrap();
    let acceptance = target
        .accept(offer.offer_id, &second.files(), 2002)
        .await
        .unwrap()
        .into_value();
    let commit = source
        .receive_acceptance(offer.offer_id, acceptance, 2003)
        .await
        .unwrap()
        .into_value();
    target
        .receive_commit(offer.offer_id, commit.clone())
        .await
        .unwrap();
    (offer.offer_id, commit)
}

#[tokio::test]
async fn empty_consent_never_launches_a_helper() {
    let device = Device::new("Mac", 44211);
    let backend = Arc::new(TestBackend::default());
    let service = device.service(device.journal(), Arc::clone(&backend));

    assert_eq!(service.start().await.unwrap(), FolderSyncLifecycle::Stopped);
    assert_eq!(
        service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    let snapshot = backend.snapshot();
    assert_eq!(snapshot.launches, 0);
    assert_eq!(snapshot.active, 0);
}

#[tokio::test]
async fn signed_consent_launches_only_after_both_durable_decisions() {
    let first = Device::new("Mac", 44221);
    let second = Device::new("Docker", 44222);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), Arc::clone(&second_backend));

    let folder = Uuid::new_v4();
    let offer = first_service
        .offer(
            second.engine.device_id(),
            folder,
            "Photos",
            &first.files(),
            3000,
        )
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_offer(offer.clone(), 3001)
        .await
        .unwrap();
    let acceptance = second_service
        .accept(offer.offer_id, &second.files(), 3002)
        .await
        .unwrap()
        .into_value();
    assert_eq!(first_backend.snapshot().launches, 0);
    assert_eq!(second_backend.snapshot().launches, 0);

    let commit = first_service
        .receive_acceptance(offer.offer_id, acceptance, 3003)
        .await
        .unwrap();
    assert_eq!(commit.lifecycle(), FolderSyncLifecycle::Running);
    assert_eq!(first_backend.snapshot().launched_folder_counts, vec![1]);
    assert_eq!(second_backend.snapshot().launches, 0);

    let commit_record = commit.into_value();
    let target = second_service
        .receive_commit(offer.offer_id, commit_record.clone())
        .await
        .unwrap();
    assert_eq!(target.lifecycle(), FolderSyncLifecycle::Running);
    assert_eq!(second_backend.snapshot().launched_folder_counts, vec![1]);

    let before_replay = second_backend.snapshot();
    second_service
        .receive_commit(offer.offer_id, commit_record)
        .await
        .unwrap();
    let after_replay = second_backend.snapshot();
    assert_eq!(after_replay.launches, before_replay.launches);
    assert_eq!(after_replay.close_calls, before_replay.close_calls);

    let before_pending = first_backend.snapshot();
    let another_root = first.root.join("another-files");
    std::fs::create_dir(&another_root).unwrap();
    first_service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Another folder",
            &another_root,
            3010,
        )
        .await
        .unwrap();
    let after_pending = first_backend.snapshot();
    assert_eq!(after_pending.launches, before_pending.launches);
    assert_eq!(after_pending.close_calls, before_pending.close_calls);
}

#[tokio::test]
async fn pending_initial_scan_is_network_inert_and_stop_cancels_it() {
    let first = Device::new("Mac", 44223);
    let second = Device::new("Docker", 44224);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let scan_gate = Arc::new(Notify::new());
    first_backend.push_scan(TestScanBehavior::Wait(Arc::clone(&scan_gate)));
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::InitialScanning
    );
    let pending = first_backend.snapshot();
    assert_eq!(pending.scan_calls, 1);
    assert_eq!(pending.promotions, 0);
    assert_eq!(pending.active, 1);

    assert_eq!(
        first_service.stop().await.unwrap(),
        FolderSyncLifecycle::Stopped
    );
    let stopped = first_backend.snapshot();
    assert_eq!(stopped.promotions, 0);
    assert_eq!(stopped.active, 0);
    assert_eq!(stopped.close_calls, 1);
    assert_eq!(stopped.stop_calls, 1);
    drop(scan_gate);
}

#[tokio::test]
async fn desired_state_change_reaps_pending_scan_and_rescans_before_promotion() {
    let first = Device::new("Mac", 44235);
    let second = Device::new("Docker", 44236);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let scan_gate = Arc::new(Notify::new());
    first_backend.push_scan(TestScanBehavior::Wait(Arc::clone(&scan_gate)));
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    let first_other = first.root.join("other-files");
    let second_other = second.root.join("other-files");
    std::fs::create_dir(&first_other).unwrap();
    std::fs::create_dir(&second_other).unwrap();
    let offer = first_service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Other documents",
            &first_other,
            4000,
        )
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_offer(offer.clone(), 4001)
        .await
        .unwrap();
    let acceptance = second_service
        .accept(offer.offer_id, &second_other, 4002)
        .await
        .unwrap()
        .into_value();
    let commit = first_service
        .receive_acceptance(offer.offer_id, acceptance, 4003)
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_commit(offer.offer_id, commit)
        .await
        .unwrap();
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::InitialScanning
    );

    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Running
    );
    let restarted = first_backend.snapshot();
    assert_eq!(restarted.launches, 2);
    assert_eq!(restarted.scan_calls, 2);
    assert_eq!(restarted.promotions, 1);
    assert_eq!(restarted.close_calls, 1);
    assert_eq!(restarted.stop_calls, 1);
    assert_eq!(restarted.active, 1);
    assert_eq!(restarted.launched_folder_counts, vec![1, 2]);
    drop(scan_gate);
}

#[tokio::test]
async fn failed_initial_scan_never_promotes_and_needs_attention() {
    let first = Device::new("Mac", 44225);
    let second = Device::new("Docker", 44226);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    first_backend.push_scan(TestScanBehavior::Fail);
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan)
    );
    let failed = first_backend.snapshot();
    assert_eq!(failed.scan_calls, 1);
    assert_eq!(failed.promotions, 0);
    assert_eq!(failed.active, 0);
    assert_eq!(failed.close_calls, 1);
    assert_eq!(failed.stop_calls, 1);
}

#[tokio::test]
async fn failed_promotion_closes_scanned_worker_before_attention() {
    let first = Device::new("Mac", 44233);
    let second = Device::new("Docker", 44234);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    first_backend.fail_next_promotion();
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan)
    );
    let failed = first_backend.snapshot();
    assert_eq!(failed.scan_calls, 1);
    assert_eq!(failed.promotions, 1);
    assert_eq!(failed.active, 0);
    assert_eq!(failed.close_calls, 1);
    assert_eq!(failed.stop_calls, 1);
}

#[tokio::test]
async fn every_restart_scans_before_one_exact_promotion() {
    let first = Device::new("Mac", 44227);
    let second = Device::new("Docker", 44228);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    make_ready(&first, &second, &first_service, &second_service).await;
    let first_run = first_backend.snapshot();
    assert_eq!(first_run.scan_calls, 1);
    assert_eq!(first_run.promotions, 1);
    assert_eq!(
        first_service.stop().await.unwrap(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(
        first_service.start().await.unwrap(),
        FolderSyncLifecycle::Running
    );
    let restarted = first_backend.snapshot();
    assert_eq!(restarted.scan_calls, 2);
    assert_eq!(restarted.promotions, 2);
    assert_eq!(restarted.maximum_active, 1);
}

#[tokio::test]
async fn dropping_service_aborts_pending_scan_and_closes_owned_worker() {
    let first = Device::new("Mac", 44229);
    let second = Device::new("Docker", 44230);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    first_backend.push_scan(TestScanBehavior::Wait(Arc::new(Notify::new())));
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(first_backend.snapshot().promotions, 0);
    drop(first_service);
    let dropped = first_backend.snapshot();
    assert_eq!(dropped.active, 0);
    assert_eq!(dropped.close_calls, 1);
    assert_eq!(dropped.promotions, 0);
}

#[tokio::test]
async fn post_commit_launch_failure_returns_the_durable_signature() {
    let first = Device::new("Mac", 44231);
    let second = Device::new("Docker", 44232);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);

    let offer = first_service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Work",
            &first.files(),
            4000,
        )
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_offer(offer.clone(), 4001)
        .await
        .unwrap();
    let accepted = second_service
        .accept(offer.offer_id, &second.files(), 4002)
        .await
        .unwrap()
        .into_value();
    first_backend.fail_next_launch();
    let committed = first_service
        .receive_acceptance(offer.offer_id, accepted, 4003)
        .await
        .unwrap();
    assert_eq!(
        committed.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerLaunch)
    );
    let signature = committed.into_value();
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].phase,
        SharingPhase::Ready
    );
    assert_eq!(
        first_service.start().await.unwrap(),
        FolderSyncLifecycle::Running
    );
    assert_eq!(first_backend.snapshot().launches, 1);
    assert!(!signature.signature.is_empty());
}

#[tokio::test]
async fn still_stopping_blocks_journal_mutation_and_parallel_launch() {
    let first = Device::new("Mac", 44241);
    let second = Device::new("Docker", 44242);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;

    first_backend.push_stop(TestStopBehavior::StillStopping);
    let committed = first_service.pause(offer_id, true).await.unwrap();
    assert_eq!(committed.lifecycle(), FolderSyncLifecycle::StillStopping);
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].phase,
        SharingPhase::Paused
    );
    // status retried the retained exact stop receiver. The same decision can be
    // retried without ever launching alongside the old session.
    let paused = first_service.pause(offer_id, true).await.unwrap();
    assert_eq!(paused.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].phase,
        SharingPhase::Paused
    );
    let snapshot = first_backend.snapshot();
    assert_eq!(snapshot.maximum_active, 1);
    assert_eq!(snapshot.launches, 1);
}

#[tokio::test]
async fn stop_failure_after_commit_keeps_the_value_and_needs_attention() {
    let first = Device::new("Mac", 44243);
    let second = Device::new("Docker", 44244);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;

    first_backend.push_stop(TestStopBehavior::Fail);
    let committed = first_service.pause(offer_id, true).await.unwrap();
    assert_eq!(
        committed.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerStop)
    );
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].phase,
        SharingPhase::Paused
    );
    assert_eq!(first_backend.snapshot().launches, 1);
}

#[tokio::test]
async fn cancelled_stop_has_already_closed_the_exact_lifeline() {
    let first = Device::new("Mac", 44251);
    let second = Device::new("Docker", 44252);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = Arc::new(first.service(first.journal(), Arc::clone(&first_backend)));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;

    let gate = Arc::new(Notify::new());
    first_backend.push_stop(TestStopBehavior::Wait(Arc::clone(&gate)));
    let service = Arc::clone(&first_service);
    let task = tokio::spawn(async move { service.pause(offer_id, true).await });
    for _ in 0..100 {
        if first_backend.snapshot().stop_calls > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    let during = first_backend.snapshot();
    assert_eq!(during.close_calls, 1);
    assert_eq!(during.stop_calls, 1);
    task.abort();
    let _ = task.await;
    assert_eq!(
        first_service.cached_lifecycle_for_test(),
        FolderSyncLifecycle::StillStopping
    );

    // The journal decision preceded reconfiguration and remains durable. A
    // retry reaps the retained session without a second live worker.
    let retried = first_service.pause(offer_id, true).await.unwrap();
    assert_eq!(retried.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(first_backend.snapshot().maximum_active, 1);
}

#[tokio::test]
async fn settings_are_revalidated_after_waiting_for_old_worker_exit() {
    let first = Device::new("Mac", 44253);
    let second = Device::new("Docker", 44254);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = Arc::new(first.service(first.journal(), Arc::clone(&first_backend)));
    let second_service = second.service(second.journal(), second_backend);
    let (first_offer, _) = make_ready(&first, &second, &first_service, &second_service).await;

    let source_root = first.root.join("second-source");
    let target_root = second.root.join("second-target");
    std::fs::create_dir(&source_root).unwrap();
    std::fs::create_dir(&target_root).unwrap();
    let offer = first_service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Second",
            &source_root,
            5000,
        )
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_offer(offer.clone(), 5001)
        .await
        .unwrap();
    let acceptance = second_service
        .accept(offer.offer_id, &target_root, 5002)
        .await
        .unwrap()
        .into_value();
    let commit = first_service
        .receive_acceptance(offer.offer_id, acceptance, 5003)
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_commit(offer.offer_id, commit)
        .await
        .unwrap();

    let before = first_backend.snapshot();
    let gate = Arc::new(Notify::new());
    first_backend.push_stop(TestStopBehavior::Wait(Arc::clone(&gate)));
    let service = Arc::clone(&first_service);
    let mutation = tokio::spawn(async move { service.pause(first_offer, true).await });
    for _ in 0..100 {
        if first_backend.snapshot().stop_calls > before.stop_calls {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(first_backend.snapshot().stop_calls > before.stop_calls);
    std::fs::rename(&source_root, first.root.join("second-source-moved")).unwrap();
    gate.notify_one();
    let committed = mutation.await.unwrap().unwrap();
    assert_eq!(
        committed.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal)
    );
    let after = first_backend.snapshot();
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.active, 0);
}

#[tokio::test]
async fn health_failure_closes_and_reaps_before_reporting_attention() {
    let first = Device::new("Mac", 44261);
    let second = Device::new("Docker", 44262);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    let observed = FolderHealth {
        folder: Uuid::new_v4(),
        lifecycle: FolderLifecycle::Idle,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 0,
        remaining_bytes: 0,
        scan_pull_error_count: 0,
        reported_error_rows: 0,
        status_error: false,
        watch_error: false,
    };
    first_backend.set_health_observation(vec![observed.clone()]);
    let healthy = first_service.status().await.unwrap();
    assert_eq!(healthy.health_freshness(), FolderHealthFreshness::Fresh);
    assert_eq!(healthy.folder_health(), std::slice::from_ref(&observed));
    first_backend.set_health_failure(true);
    let status = first_service.status().await.unwrap();
    assert_eq!(
        status.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerHealth)
    );
    assert_eq!(status.health_freshness(), FolderHealthFreshness::Stale);
    assert_eq!(status.folder_health(), &[observed]);
    let backend = first_backend.snapshot();
    assert_eq!(backend.active, 0);
    assert_eq!(backend.close_calls, 1);
    assert_eq!(backend.stop_calls, 1);
    assert_eq!(backend.health_calls, 2);
    let outgoing = first_service.outbound_records().await.unwrap();
    assert!(!outgoing.value().is_empty());
    assert_eq!(
        outgoing.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerHealth)
    );
    assert_eq!(first_backend.snapshot().launches, 1);
}

#[tokio::test]
async fn reported_folder_error_is_retained_and_reaps_worker() {
    let first = Device::new("Mac", 44275);
    let second = Device::new("Docker", 44276);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    let observed = FolderHealth {
        folder: Uuid::new_v4(),
        lifecycle: FolderLifecycle::Error,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 1,
        remaining_bytes: 7,
        scan_pull_error_count: 1,
        reported_error_rows: 1,
        status_error: true,
        watch_error: false,
    };
    first_backend.set_health_observation(vec![observed.clone()]);
    let status = first_service.status().await.unwrap();
    assert_eq!(
        status.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerHealth)
    );
    assert_eq!(status.health_freshness(), FolderHealthFreshness::Stale);
    assert_eq!(status.folder_health(), &[observed]);
    assert_eq!(first_backend.snapshot().active, 0);
}

#[tokio::test]
async fn owned_health_task_detects_failure_without_a_status_request() {
    let first = Device::new("Mac", 44269);
    let second = Device::new("Docker", 44270);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = FolderSyncService::new_for_test(
        first.journal(),
        Arc::clone(&first.engine),
        Arc::clone(&first_backend),
        Duration::from_millis(1),
    );
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    let before = first_backend.snapshot().health_calls;
    first_backend.set_health_failure(true);
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            first_backend.wait_for_health().await;
            if first_backend.snapshot().health_calls > before {
                break;
            }
        }
        first_backend.wait_for_stop().await;
    })
    .await
    .unwrap();
    assert_eq!(
        first_service.cached_lifecycle_for_test(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::WorkerHealth)
    );
}

#[tokio::test]
async fn invalid_and_replayed_records_cannot_restart_a_healthy_worker() {
    let first = Device::new("Mac", 44263);
    let second = Device::new("Docker", 44264);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let before = first_backend.snapshot();

    assert_eq!(
        first_service.remove(Uuid::new_v4()).await.unwrap_err(),
        FolderSyncServiceError::Journal
    );
    assert_eq!(
        first_service.start().await.unwrap(),
        FolderSyncLifecycle::Running
    );
    let after = first_backend.snapshot();
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.close_calls, before.close_calls);
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].offer_id,
        offer_id
    );
}

#[tokio::test]
async fn freshness_only_revision_on_pending_limit_does_not_restart_worker() {
    let first = Device::new("Mac", 44273);
    let second = Device::new("Docker", 44274);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    for index in 0_u64..16 {
        let root = second.root.join(format!("pending-{index}"));
        std::fs::create_dir(&root).unwrap();
        let offer = second_service
            .offer(
                first.engine.device_id(),
                Uuid::new_v4(),
                "Pending",
                &root,
                100_000_000 + index,
            )
            .await
            .unwrap()
            .into_value();
        first_service
            .receive_offer(offer, 100_000_000 + index)
            .await
            .unwrap();
    }
    let before = first_backend.snapshot();
    let refused_root = second.root.join("pending-refused");
    std::fs::create_dir(&refused_root).unwrap();
    let refused = second_service
        .offer(
            first.engine.device_id(),
            Uuid::new_v4(),
            "Refused",
            &refused_root,
            100_000_129,
        )
        .await
        .unwrap()
        .into_value();
    assert_eq!(
        first_service
            .receive_offer(refused, 100_000_129)
            .await
            .unwrap_err(),
        FolderSyncServiceError::Journal
    );
    let after = first_backend.snapshot();
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.close_calls, before.close_calls);
    assert_eq!(after.active, 1);
}

#[tokio::test]
async fn out_of_band_revocation_reconciles_and_reaps_stale_membership() {
    let first = Device::new("Mac", 44265);
    let second = Device::new("Docker", 44266);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    first.engine.revoke_peer(second.engine.device_id()).unwrap();
    let status = first_service.status().await.unwrap();
    assert_eq!(status.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(status.shares()[0].phase, SharingPhase::Removed);
    let backend = first_backend.snapshot();
    assert_eq!(backend.active, 0);
    assert_eq!(backend.close_calls, 1);
    assert_eq!(backend.maximum_active, 1);
}

#[tokio::test]
async fn lost_root_capability_reaps_worker_and_requires_attention() {
    let first = Device::new("Mac", 44267);
    let second = Device::new("Docker", 44268);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    std::fs::rename(first.files(), first.root.join("moved-files")).unwrap();
    assert_eq!(
        first_service.start().await.unwrap_err(),
        FolderSyncServiceError::Journal
    );
    let status = first_service.status().await.unwrap();
    assert_eq!(
        status.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::Journal)
    );
    assert_eq!(first_backend.snapshot().active, 0);
}

#[tokio::test]
async fn peer_revocation_tombstones_before_core_revoke_and_stays_stopped() {
    let first = Device::new("Mac", 44271);
    let second = Device::new("Docker", 44272);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;

    let revoked = first_service
        .revoke_peer(second.engine.device_id())
        .await
        .unwrap();
    assert!(revoked.value().is_some());
    assert_eq!(revoked.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(
        first_service.status().await.unwrap().shares()[0].phase,
        SharingPhase::Removed
    );
    assert!(
        first
            .engine
            .config()
            .unwrap()
            .trusted_peers
            .get(&second.engine.device_id())
            .unwrap()
            .revoked
    );
    assert_eq!(first_backend.snapshot().maximum_active, 1);
}

#[tokio::test]
async fn drop_closes_the_lifeline_without_a_process_lookup() {
    let first = Device::new("Mac", 44281);
    let second = Device::new("Docker", 44282);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &first_service, &second_service).await;
    assert_eq!(first_backend.snapshot().active, 1);

    drop(first_service);
    let snapshot = first_backend.snapshot();
    assert_eq!(snapshot.active, 0);
    assert_eq!(snapshot.close_calls, 1);
}

#[test]
fn public_failures_and_debug_are_redacted() {
    for error in [
        FolderSyncServiceError::Busy,
        FolderSyncServiceError::InvalidConfiguration,
        FolderSyncServiceError::Journal,
        FolderSyncServiceError::WorkerLaunch,
        FolderSyncServiceError::WorkerStop,
        FolderSyncServiceError::WorkerStillStopping,
    ] {
        let text = format!("{error:?} {error}");
        assert!(!text.contains('/'));
        assert!(!text.contains("key"));
    }
}

#[test]
fn reopen_helper_confirms_test_state_is_durable() {
    let device = Device::new("Mac", 44291);
    let journal = device.journal();
    drop(journal);
    assert!(device.reopen().summaries().unwrap().is_empty());
}
