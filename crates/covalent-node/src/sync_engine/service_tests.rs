use super::config::EngineDeviceId;
use super::run_observation::EngineRunObservation;
use super::service::{TestBackend, TestScanBehavior, TestStopBehavior};
use super::*;
use crate::transport::TlsIdentity;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use covalent_core::{Engine, EngineOptions, KeyProtector, StaticKeyProtector};
use covalent_protocol::{DeviceId, FolderShareCommit, PeerGrant, PeerRole, TransportBinding};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::Notify;
use uuid::Uuid;

const QUIET_HEALTH: Duration = Duration::from_secs(3600);

#[test]
fn known_destination_failure_is_reportable_for_the_admitted_index() {
    let source = EngineDeviceId::from_certificate_der(b"failed transfer source").unwrap();
    let expected = EngineIndexSnapshot {
        index_id: "0x0123456789ABCDEF".into(),
        sequence: 1,
    };
    let observed = EngineRunObservation {
        local_index: None,
        completions: BTreeMap::new(),
        failures: BTreeSet::from([source.clone()]),
    };
    assert_eq!(
        super::service::destination_observation_result(&observed, &source, &expected),
        Some(LinkRunDestinationResult::Failed)
    );
}

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

    fn grant(&self, peer: DeviceId) -> PeerGrant {
        self.engine.config().unwrap().trusted_peers[&peer].clone()
    }

    fn engine_binding(&self, direct_address: &str) -> covalent_protocol::SyncEngineBinding {
        covalent_protocol::SyncEngineBinding::new_authenticated(
            self.engine.device_id(),
            self.installation.device_id().as_str(),
            &self.installation.identity().ssh_public_key().unwrap(),
            direct_address,
        )
        .unwrap()
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
    pair_with_roles(first, second, BTreeSet::from([PeerRole::BackupReader]));
}

fn pair_with_roles(first: &Device, second: &Device, roles: BTreeSet<PeerRole>) {
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
    make_ready_folder(first, second, source, target, folder).await
}

async fn make_ready_folder(
    first: &Device,
    second: &Device,
    source: &FolderSyncService,
    target: &FolderSyncService,
    folder: Uuid,
) -> (Uuid, FolderShareCommit) {
    make_ready_folder_at(
        second,
        source,
        target,
        folder,
        &first.files(),
        &second.files(),
    )
    .await
}

async fn make_ready_folder_at(
    second: &Device,
    source: &FolderSyncService,
    target: &FolderSyncService,
    folder: Uuid,
    source_root: &std::path::Path,
    target_root: &std::path::Path,
) -> (Uuid, FolderShareCommit) {
    let offer = source
        .offer(
            second.engine.device_id(),
            folder,
            "Documents",
            source_root,
            2000,
        )
        .await
        .unwrap()
        .into_value();
    target.receive_offer(offer.clone(), 2001).await.unwrap();
    let acceptance = target
        .accept(offer.offer_id, target_root, 2002)
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
    let settings = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::SettingsCommit(commit)
                if delivery.peer_transport.peer_id == second.engine.device_id()
                    && commit.folder_id == folder =>
            {
                Some(commit)
            }
            _ => None,
        })
        .unwrap();
    target
        .receive_link_settings_commit(&settings)
        .await
        .unwrap();
    (offer.offer_id, commit)
}

#[tokio::test]
async fn unavailable_source_folder_interrupts_only_its_active_run() {
    let first = Device::new("Android source", 44291);
    let second = Device::new("Target", 44292);
    pair(&first, &second);
    let source_backend = Arc::new(TestBackend::default());
    let target_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&source_backend));
    let target = second.service(second.journal(), target_backend);
    let folders = [Uuid::new_v4(), Uuid::new_v4()];
    let source_roots = [first.root.join("source-a"), first.root.join("source-b")];
    let target_roots = [second.root.join("target-a"), second.root.join("target-b")];
    for root in source_roots.iter().chain(target_roots.iter()) {
        std::fs::create_dir(root).unwrap();
    }
    let settings = FolderLinkSettings {
        deletion_policy: covalent_protocol::FolderLinkPolicy::default(),
        paused: false,
        cadence: LinkCadence::Manual,
        android_conditions: AndroidLinkConditions::default(),
    };
    let mut revisions = BTreeMap::new();
    let mut offer_ids = BTreeMap::new();
    for ((folder, source_root), target_root) in folders
        .iter()
        .zip(source_roots.iter())
        .zip(target_roots.iter())
    {
        let offer = source
            .offer_with_settings(
                second.engine.device_id(),
                *folder,
                "Documents",
                source_root,
                2000,
                settings,
            )
            .await
            .unwrap()
            .into_value();
        offer_ids.insert(*folder, offer.offer_id);
        target.receive_offer(offer.clone(), 2001).await.unwrap();
        let acceptance = target
            .accept(offer.offer_id, target_root, 2002)
            .await
            .unwrap()
            .into_value();
        let commit = source
            .receive_acceptance(offer.offer_id, acceptance, 2003)
            .await
            .unwrap()
            .into_value();
        target.receive_commit(offer.offer_id, commit).await.unwrap();
        let settings_commit = source
            .outbound_records()
            .await
            .unwrap()
            .into_value()
            .into_iter()
            .find_map(|delivery| match delivery.record {
                FolderShareRecord::SettingsCommit(commit) if commit.folder_id == *folder => {
                    Some(commit)
                }
                _ => None,
            })
            .unwrap();
        revisions.insert(*folder, settings_commit.revision);
        target
            .receive_link_settings_commit(&settings_commit)
            .await
            .unwrap();
    }

    for folder in folders {
        let admission = source
            .request_link_run(folder, Uuid::new_v4(), 0, revisions[&folder])
            .await
            .unwrap()
            .into_value();
        assert!(matches!(admission, LinkRunAdmission::Accepted(_)));
    }
    source_backend.set_health_observation(
        folders
            .into_iter()
            .map(|folder| FolderHealth {
                folder,
                lifecycle: if folder == folders[0] {
                    FolderLifecycle::Error
                } else {
                    FolderLifecycle::Idle
                },
                access_unavailable: folder == folders[0],
                state_changed: OffsetDateTime::UNIX_EPOCH,
                remaining_files: 0,
                remaining_bytes: 0,
                scan_pull_error_count: 0,
                reported_error_rows: 0,
                status_error: false,
                watch_error: false,
            })
            .collect(),
    );
    source_backend.set_run_folder_unavailable(folders[0]);
    source_backend.set_run_observations(
        folders[1],
        vec![EngineRunObservation {
            local_index: Some(EngineIndexSnapshot {
                index_id: "0x0123456789ABCDEF".into(),
                sequence: 1,
            }),
            completions: BTreeMap::new(),
            failures: BTreeSet::new(),
        }],
    );
    source.advance_runs_for_test().await;
    source.advance_runs_for_test().await;

    let status = source.status().await.unwrap();
    let inaccessible = status
        .shares()
        .iter()
        .find(|share| share.folder_id == folders[0])
        .unwrap();
    assert_eq!(
        inaccessible.link_run.as_ref().unwrap().phase,
        Some(LinkRunPhase::Interrupted)
    );
    assert!(
        inaccessible
            .link_run
            .as_ref()
            .unwrap()
            .destinations
            .iter()
            .all(|destination| destination.result == LinkRunDestinationResult::Interrupted)
    );
    assert!(status.folder_health().iter().any(|health| {
        health.folder == folders[0]
            && health.lifecycle == FolderLifecycle::Error
            && health.access_unavailable
    }));
    let healthy = status
        .shares()
        .iter()
        .find(|share| share.folder_id == folders[1])
        .unwrap();
    assert_eq!(
        healthy.link_run.as_ref().unwrap().phase,
        Some(LinkRunPhase::Running)
    );

    let interrupted = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunCommit(commit)
                if commit.folder_id == folders[0]
                    && delivery.peer_transport.peer_id == second.engine.device_id() =>
            {
                Some(commit)
            }
            _ => None,
        })
        .expect("interrupted state must be delivered to this link's target");
    assert_eq!(
        interrupted.state.as_ref().unwrap().phase,
        LinkRunPhase::Interrupted
    );
    target.receive_link_run_commit(&interrupted).await.unwrap();
    let target_status = target.status().await.unwrap();
    assert_eq!(
        target_status
            .shares()
            .iter()
            .find(|share| share.folder_id == folders[0])
            .unwrap()
            .link_run
            .as_ref()
            .unwrap()
            .phase,
        Some(LinkRunPhase::Interrupted)
    );
    let reopened = first.reopen();
    assert_eq!(
        reopened
            .summaries()
            .unwrap()
            .into_iter()
            .find(|share| share.folder_id == folders[0])
            .unwrap()
            .link_run
            .unwrap()
            .phase,
        Some(LinkRunPhase::Interrupted)
    );

    source.remove(offer_ids[&folders[1]]).await.unwrap();
    let stopped = source.status().await.unwrap();
    assert_eq!(stopped.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(stopped.health_freshness(), FolderHealthFreshness::Stale);
    assert_eq!(stopped.folder_health().len(), 1);
    assert!(stopped.folder_health()[0].access_unavailable);

    source
        .repair_root(offer_ids[&folders[0]], &source_roots[0])
        .await
        .unwrap();
    let repaired = source.status().await.unwrap();
    assert_eq!(repaired.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(
        repaired.health_freshness(),
        FolderHealthFreshness::NeverObserved
    );
    assert!(repaired.folder_health().is_empty());
}

#[tokio::test]
async fn concurrent_expired_offer_renewals_converge_on_one_durable_replacement() {
    let first = Device::new("Mac", 44311);
    let second = Device::new("Server", 44312);
    pair(&first, &second);
    let backend = Arc::new(TestBackend::default());
    let service = Arc::new(first.service(first.journal(), Arc::clone(&backend)));
    let offer = service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Documents",
            &first.files(),
            2000,
        )
        .await
        .unwrap()
        .into_value();
    let expires = offer.expires_at_unix_ms;

    let (left, right) = tokio::join!(
        service.renew_offer(offer.offer_id, expires),
        service.renew_offer(offer.offer_id, expires)
    );
    let mut returned = Vec::new();
    for result in [left, right] {
        match result {
            Ok(committed) => returned.push(committed.into_value().offer_id),
            Err(error) => assert_eq!(error, FolderSyncServiceError::Busy),
        }
    }
    let retried = service
        .renew_offer(offer.offer_id, expires + 1)
        .await
        .unwrap()
        .into_value();
    assert!(!returned.is_empty());
    assert!(
        returned
            .iter()
            .all(|offer_id| *offer_id == retried.offer_id)
    );
    let status = service.status().await.unwrap();
    assert_eq!(status.shares().len(), 1);
    assert_eq!(status.shares()[0].offer_id, retried.offer_id);
    assert_eq!(status.shares()[0].phase, SharingPhase::Offered);
    assert_eq!(backend.snapshot().launches, 0);
}

#[tokio::test]
async fn peer_address_refresh_is_durable_without_a_folder_or_worker() {
    let first = Device::new("Mac", 44301);
    let second = Device::new("Server", 44302);
    pair(&first, &second);
    let backend = Arc::new(TestBackend::default());
    let service = first.service(first.journal(), Arc::clone(&backend));
    let expected = second.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:54302".into();
    let candidate_engine = second.engine_binding("127.0.0.1:54303");

    let committed = service
        .refresh_peer_address(
            second.engine.device_id(),
            &first.grant(second.engine.device_id()),
            &expected,
            &candidate,
            &candidate_engine,
        )
        .await
        .unwrap();
    assert_eq!(committed.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(
        service.pending_peer_address_refresh().await.unwrap(),
        Some((second.engine.device_id(), true))
    );
    service
        .finish_peer_address_refresh(second.engine.device_id())
        .await
        .unwrap();
    assert_eq!(backend.snapshot().launches, 0);
    assert_eq!(
        first
            .engine
            .config()
            .unwrap()
            .trusted_peer_transports
            .get(&second.engine.device_id())
            .unwrap(),
        &candidate
    );
    drop(service);

    let reopened = first.service(first.reopen(), Arc::clone(&backend));
    assert_eq!(
        reopened.start().await.unwrap(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(backend.snapshot().launches, 0);
}

#[tokio::test]
async fn active_address_refresh_reaps_before_core_change_and_requires_explicit_restart() {
    let first = Device::new("Mac", 44303);
    let second = Device::new("Server", 44304);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&first_backend));
    let target = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &source, &target).await;
    let before = first_backend.snapshot();
    let expected = second.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:54304".into();
    let candidate_engine = second.engine_binding("127.0.0.1:54305");

    assert_eq!(
        source
            .refresh_peer_address(
                second.engine.device_id(),
                &first.grant(second.engine.device_id()),
                &expected,
                &candidate,
                &candidate_engine,
            )
            .await
            .unwrap()
            .lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    let stopped = first_backend.snapshot();
    assert_eq!(stopped.close_calls, before.close_calls + 1);
    assert_eq!(stopped.stop_calls, before.stop_calls + 1);
    assert_eq!(stopped.launches, before.launches);
    source
        .finish_peer_address_refresh(second.engine.device_id())
        .await
        .unwrap();
    assert_eq!(source.start().await.unwrap(), FolderSyncLifecycle::Running);
    let restarted = first_backend.snapshot();
    assert_eq!(restarted.launches, before.launches + 1);
    assert_eq!(restarted.scan_calls, before.scan_calls + 1);
    assert_eq!(restarted.maximum_active, 1);
}

#[tokio::test]
async fn invalid_address_refresh_keeps_worker_and_failed_reap_keeps_core_old() {
    let first = Device::new("Mac", 44305);
    let second = Device::new("Server", 44306);
    let stranger = Device::new("Stranger", 44307);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&first_backend));
    let target = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &source, &target).await;
    let expected = second.transport();
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:54306".into();
    let wrong_owner = covalent_protocol::SyncEngineBinding::new_authenticated(
        stranger.engine.device_id(),
        second.installation.device_id().as_str(),
        &second.installation.identity().ssh_public_key().unwrap(),
        "127.0.0.1:54307",
    )
    .unwrap();
    let before = first_backend.snapshot();
    assert_eq!(
        source
            .refresh_peer_address(
                second.engine.device_id(),
                &first.grant(second.engine.device_id()),
                &expected,
                &candidate,
                &wrong_owner,
            )
            .await,
        Err(FolderSyncServiceError::Journal)
    );
    let unchanged = first_backend.snapshot();
    assert_eq!(unchanged.stop_calls, before.stop_calls);
    assert_eq!(unchanged.active, 1);

    let valid = second.engine_binding("127.0.0.1:54307");
    first_backend.push_stop(TestStopBehavior::StillStopping);
    assert_eq!(
        source
            .refresh_peer_address(
                second.engine.device_id(),
                &first.grant(second.engine.device_id()),
                &expected,
                &candidate,
                &valid,
            )
            .await,
        Err(FolderSyncServiceError::WorkerStillStopping)
    );
    assert_eq!(
        first
            .engine
            .config()
            .unwrap()
            .trusted_peer_transports
            .get(&second.engine.device_id()),
        Some(&expected)
    );
}

#[tokio::test]
async fn grant_change_while_worker_reaps_rejects_the_prepared_address_transition() {
    let first = Device::new("Mac", 44308);
    let second = Device::new("Server", 44309);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let source = Arc::new(first.service(first.journal(), Arc::clone(&first_backend)));
    let target = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &source, &target).await;
    let expected = second.transport();
    let expected_grant = first.grant(second.engine.device_id());
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:54309".into();
    let candidate_engine = second.engine_binding("127.0.0.1:54310");
    let peer_id = second.engine.device_id();
    let gate = Arc::new(Notify::new());
    first_backend.push_stop(TestStopBehavior::Wait(Arc::clone(&gate)));
    let refresh = {
        let source = Arc::clone(&source);
        let expected_grant = expected_grant.clone();
        let expected = expected.clone();
        let candidate = candidate.clone();
        tokio::spawn(async move {
            source
                .refresh_peer_address(
                    peer_id,
                    &expected_grant,
                    &expected,
                    &candidate,
                    &candidate_engine,
                )
                .await
        })
    };
    first_backend.wait_for_stop().await;
    let mut changed = expected_grant;
    changed.roles.insert(PeerRole::StorageProvider);
    first.engine.trust_peer(changed).unwrap();
    gate.notify_waiters();
    assert_eq!(refresh.await.unwrap(), Err(FolderSyncServiceError::Journal));
    assert_eq!(source.pending_peer_address_refresh().await.unwrap(), None);
    let config = first.engine.config().unwrap();
    assert_ne!(
        config
            .trusted_peer_transports
            .get(&second.engine.device_id())
            .map(|binding| binding.address.as_str()),
        Some(candidate.address.as_str())
    );
    assert_eq!(first_backend.snapshot().maximum_active, 1);
}

#[tokio::test]
async fn provider_persistence_failure_keeps_the_address_barrier_until_exact_retry() {
    let first = Device::new("Mac", 44310);
    let second = Device::new("Server", 44311);
    pair_with_roles(
        &first,
        &second,
        BTreeSet::from([PeerRole::BackupReader, PeerRole::StorageProvider]),
    );
    let provider_path = first.root.join("provider-connections.json");
    let state = crate::AppState::new(
        Arc::clone(&first.engine),
        crate::PlatformTier::Tier1,
        "provider-barrier-test-token-with-32-bytes".into(),
    )
    .unwrap()
    .with_provider_state(&provider_path)
    .unwrap();
    let expected = second.transport();
    state
        .connect_provider(crate::ProviderConnection {
            peer_id: second.engine.device_id(),
            address: expected.address.parse().unwrap(),
            certificate_der: expected.certificate_der.clone(),
        })
        .unwrap();
    let service = first.service(first.journal(), Arc::new(TestBackend::default()));
    let expected_grant = first.grant(second.engine.device_id());
    let mut candidate = expected.clone();
    candidate.address = "127.0.0.1:54311".into();
    let candidate_engine = second.engine_binding("127.0.0.1:54312");
    service
        .refresh_peer_address(
            second.engine.device_id(),
            &expected_grant,
            &expected,
            &candidate,
            &candidate_engine,
        )
        .await
        .unwrap();

    let saved = first.root.join("provider-connections.saved");
    std::fs::rename(&provider_path, &saved).unwrap();
    std::fs::create_dir(&provider_path).unwrap();
    assert!(
        state
            .finish_peer_address_provider_barrier(&service)
            .await
            .is_err()
    );
    assert!(
        service
            .pending_peer_address_refresh()
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(service.start().await, Err(FolderSyncServiceError::Journal));

    std::fs::remove_dir(&provider_path).unwrap();
    std::fs::rename(&saved, &provider_path).unwrap();
    drop(service);
    drop(state);
    let reopened_state = crate::AppState::new(
        Arc::clone(&first.engine),
        crate::PlatformTier::Tier1,
        "provider-barrier-test-token-with-32-bytes".into(),
    )
    .unwrap()
    .with_provider_state(&provider_path)
    .unwrap();
    let reopened_service = first.service(first.reopen(), Arc::new(TestBackend::default()));
    reopened_state
        .finish_peer_address_provider_barrier(&reopened_service)
        .await
        .unwrap();
    assert!(
        reopened_service
            .pending_peer_address_refresh()
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened_service.start().await.unwrap(),
        FolderSyncLifecycle::Stopped
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&provider_path).unwrap()).unwrap();
    assert_eq!(
        saved["providers"][second.engine.device_id().to_string()]["address"],
        candidate.address
    );
}

#[tokio::test]
async fn renewing_pending_consent_does_not_restart_an_unrelated_running_folder() {
    let first = Device::new("Mac", 44313);
    let second = Device::new("Server", 44314);
    let third = Device::new("Phone", 44315);
    pair(&first, &second);
    pair(&first, &third);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&first_backend));
    let target = second.service(second.journal(), second_backend);
    make_ready(&first, &second, &source, &target).await;
    let pending_root = first.root.join("pending-phone");
    std::fs::create_dir(&pending_root).unwrap();
    let pending = source
        .offer(
            third.engine.device_id(),
            Uuid::new_v4(),
            "Phone files",
            &pending_root,
            4000,
        )
        .await
        .unwrap()
        .into_value();
    let before = first_backend.snapshot();

    let renewed = source
        .renew_offer(pending.offer_id, pending.expires_at_unix_ms)
        .await
        .unwrap();

    assert_ne!(renewed.value().offer_id, pending.offer_id);
    assert_eq!(renewed.lifecycle(), FolderSyncLifecycle::Running);
    let after = first_backend.snapshot();
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.close_calls, before.close_calls);
    assert_eq!(after.stop_calls, before.stop_calls);
    assert_eq!(after.active, 1);
}

#[tokio::test]
async fn disconnect_reconnect_and_unknown_do_not_stop_a_healthy_worker() {
    let first = Device::new("Mac", 44209);
    let second = Device::new("Server", 44210);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), Arc::clone(&second_backend));
    make_ready(&first, &second, &first_service, &second_service).await;

    first_backend.set_connection_states(vec![EnginePeerConnectionState::Disconnected]);
    first_service.observe_health_for_test().await;
    let disconnected = first_service.status().await.unwrap();
    assert_eq!(disconnected.lifecycle(), FolderSyncLifecycle::Running);
    assert_eq!(
        disconnected.connection_freshness(),
        PeerConnectionFreshness::Fresh
    );
    assert_eq!(
        disconnected.peer_connection(second.engine.device_id()),
        PeerConnectionState::Disconnected
    );

    first_backend.set_connection_states(vec![EnginePeerConnectionState::Connected]);
    first_service.observe_health_for_test().await;
    let connected = first_service.status().await.unwrap();
    assert_eq!(
        connected.peer_connection(second.engine.device_id()),
        PeerConnectionState::Connected
    );

    first_backend.set_connection_failure(true);
    first_service.observe_health_for_test().await;
    let unavailable = first_service.status().await.unwrap();
    assert_eq!(unavailable.lifecycle(), FolderSyncLifecycle::Running);
    assert_eq!(
        unavailable.connection_freshness(),
        PeerConnectionFreshness::Stale
    );
    assert_eq!(
        unavailable.peer_connection(second.engine.device_id()),
        PeerConnectionState::Unknown
    );
    let calls = first_backend.snapshot();
    assert_eq!(calls.health_calls, 3);
    assert_eq!(calls.connection_calls, 3);
    assert_eq!(calls.stop_calls, 0);
    assert_eq!(calls.close_calls, 0);
}

#[tokio::test]
async fn root_repair_reaps_old_worker_then_runs_a_new_full_initial_scan() {
    let first = Device::new("Mac", 44207);
    let second = Device::new("Server", 44208);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), Arc::clone(&second_backend));
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let before = first_backend.snapshot();
    let replacement = first.root.join("replacement");
    std::fs::create_dir(&replacement).unwrap();

    let committed = first_service
        .repair_root(offer_id, &replacement)
        .await
        .unwrap();
    assert_eq!(committed.lifecycle(), FolderSyncLifecycle::Running);
    let after = first_backend.snapshot();
    assert_eq!(after.close_calls, before.close_calls + 2);
    assert_eq!(after.stop_calls, before.stop_calls + 2);
    assert_eq!(after.launches, before.launches + 2);
    assert_eq!(after.scan_calls, before.scan_calls + 1);
    assert_eq!(after.reset_calls.len(), 1);
    assert_eq!(
        after.launched_reset_gates[after.launches - 2..],
        [true, false]
    );
    assert_eq!(after.maximum_active, 1);

    drop(first_service);
    assert_eq!(
        first.reopen().desired_settings().unwrap().folders[0].root(),
        replacement
    );
}

#[tokio::test]
async fn scoped_root_reset_retains_unrelated_folder_settings_and_index_gate() {
    let first = Device::new("Mac", 44277);
    let second = Device::new("Server", 44278);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let repaired_folder = Uuid::new_v4();
    let (offer_id, _) = make_ready_folder(
        &first,
        &second,
        &first_service,
        &second_service,
        repaired_folder,
    )
    .await;
    let first_other = first.root.join("other");
    let second_other = second.root.join("other");
    std::fs::create_dir(&first_other).unwrap();
    std::fs::create_dir(&second_other).unwrap();
    let unrelated_folder = Uuid::new_v4();
    make_ready_folder_at(
        &second,
        &first_service,
        &second_service,
        unrelated_folder,
        &first_other,
        &second_other,
    )
    .await;
    let before = first_backend.snapshot();
    let replacement = first.root.join("replacement-scoped");
    std::fs::create_dir(&replacement).unwrap();

    assert_eq!(
        first_service
            .repair_root(offer_id, &replacement)
            .await
            .unwrap()
            .lifecycle(),
        FolderSyncLifecycle::Running
    );
    let after = first_backend.snapshot();
    assert_eq!(after.reset_calls, [repaired_folder]);
    assert_eq!(after.launched_folder_counts[after.launches - 2..], [2, 2]);
    assert_eq!(
        after.launched_reset_gates[after.launches - 2..],
        [true, false]
    );
    assert_eq!(after.scan_calls, before.scan_calls + 1);
    let settings = first.reopen().desired_settings().unwrap();
    assert_eq!(settings.folders.len(), 2);
    assert!(settings.folders.iter().any(|folder| {
        folder.id() == unrelated_folder && folder.root() == first_other.as_path()
    }));
}

#[tokio::test]
async fn invalid_root_repair_does_not_stop_a_healthy_worker() {
    let first = Device::new("Mac", 44213);
    let second = Device::new("Server", 44214);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let before = first_backend.snapshot();

    assert_eq!(
        first_service
            .repair_root(offer_id, &first.root.join("absent"))
            .await
            .unwrap_err(),
        FolderSyncServiceError::Journal
    );
    assert_eq!(
        first_service.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Running
    );
    let after = first_backend.snapshot();
    assert_eq!(after.close_calls, before.close_calls);
    assert_eq!(after.stop_calls, before.stop_calls);
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.active, 1);
}

#[tokio::test]
async fn multi_peer_root_repair_is_rejected_before_stopping_worker() {
    let first = Device::new("Mac", 44217);
    let second = Device::new("Server", 44218);
    let third = Device::new("Laptop", 44219);
    pair(&first, &second);
    pair(&first, &third);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let third_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let third_service = third.service(third.journal(), third_backend);
    let folder = Uuid::new_v4();
    let (offer_id, _) =
        make_ready_folder(&first, &second, &first_service, &second_service, folder).await;
    make_ready_folder(&first, &third, &first_service, &third_service, folder).await;
    let before = first_backend.snapshot();
    let replacement = first.root.join("multi-peer-replacement");
    std::fs::create_dir(&replacement).unwrap();

    assert_eq!(
        first_service
            .repair_root(offer_id, &replacement)
            .await
            .unwrap_err(),
        FolderSyncServiceError::Journal
    );
    let after = first_backend.snapshot();
    assert_eq!(after.close_calls, before.close_calls);
    assert_eq!(after.stop_calls, before.stop_calls);
    assert_eq!(after.launches, before.launches);
    assert_eq!(after.active, 1);
}

#[tokio::test]
async fn failed_index_reset_stays_durable_and_retries_before_scanning() {
    let first = Device::new("Mac", 44215);
    let second = Device::new("Server", 44216);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let before = first_backend.snapshot();
    let replacement = first.root.join("replacement-retry");
    std::fs::create_dir(&replacement).unwrap();
    first_backend.fail_next_reset();

    let committed = first_service
        .repair_root(offer_id, &replacement)
        .await
        .unwrap();
    assert_eq!(
        committed.lifecycle(),
        FolderSyncLifecycle::NeedsAttention(FolderSyncIssue::InitialScan)
    );
    let failed = first_backend.snapshot();
    assert_eq!(failed.reset_calls.len(), 1);
    assert_eq!(failed.scan_calls, before.scan_calls);
    assert_eq!(failed.active, 0);
    assert_eq!(failed.launched_reset_gates.last(), Some(&true));

    assert_eq!(
        first_service.start().await.unwrap(),
        FolderSyncLifecycle::Running
    );
    let retried = first_backend.snapshot();
    assert_eq!(retried.reset_calls.len(), 2);
    assert_eq!(retried.scan_calls, before.scan_calls + 1);
    assert_eq!(retried.maximum_active, 1);
    assert_eq!(
        retried.launched_reset_gates[retried.launches - 2..],
        [true, false]
    );
}

#[tokio::test]
async fn successful_reset_is_not_cleared_until_the_exact_worker_is_reaped() {
    let first = Device::new("Mac", 44279);
    let second = Device::new("Server", 44280);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let replacement = first.root.join("replacement-unreaped-reset");
    std::fs::create_dir(&replacement).unwrap();
    first_backend.push_stop(TestStopBehavior::Exited);
    first_backend.push_stop(TestStopBehavior::StillStopping);

    let committed = first_service
        .repair_root(offer_id, &replacement)
        .await
        .unwrap();
    assert_eq!(committed.lifecycle(), FolderSyncLifecycle::StillStopping);
    assert!(first.reopen().pending_root_reset().unwrap().is_some());
    let unreaped = first_backend.snapshot();
    assert_eq!(unreaped.active, 1);
    assert_eq!(unreaped.reset_calls.len(), 1);
    assert_eq!(unreaped.scan_calls, 1);

    assert_eq!(
        first_service.start().await.unwrap(),
        FolderSyncLifecycle::Running
    );
    let recovered = first_backend.snapshot();
    assert_eq!(recovered.active, 1);
    assert_eq!(recovered.maximum_active, 1);
    assert_eq!(recovered.reset_calls.len(), 2);
    assert_eq!(recovered.scan_calls, 2);
    assert_eq!(first.reopen().pending_root_reset().unwrap(), None);
}

#[tokio::test]
async fn root_repair_cannot_commit_while_the_old_worker_is_still_stopping() {
    let first = Device::new("Mac", 44205);
    let second = Device::new("Server", 44206);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = first.service(first.journal(), Arc::clone(&first_backend));
    let second_service = second.service(second.journal(), second_backend);
    let (offer_id, _) = make_ready(&first, &second, &first_service, &second_service).await;
    let replacement = first.root.join("replacement");
    std::fs::create_dir(&replacement).unwrap();
    first_backend.push_stop(TestStopBehavior::StillStopping);

    assert_eq!(
        first_service
            .repair_root(offer_id, &replacement)
            .await
            .unwrap_err(),
        FolderSyncServiceError::WorkerStillStopping
    );
    assert_eq!(
        first.reopen().desired_settings().unwrap().folders[0].root(),
        first.files()
    );

    let committed = first_service
        .repair_root(offer_id, &replacement)
        .await
        .unwrap();
    assert_eq!(committed.lifecycle(), FolderSyncLifecycle::Running);
    assert_eq!(
        first.reopen().desired_settings().unwrap().folders[0].root(),
        replacement
    );
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
    assert_eq!(target.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(second_backend.snapshot().launches, 0);
    let settings = first_service
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::SettingsCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    let target = second_service
        .receive_link_settings_commit(&settings)
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
        access_unavailable: false,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 0,
        remaining_bytes: 0,
        scan_pull_error_count: 0,
        reported_error_rows: 0,
        status_error: false,
        watch_error: false,
    };
    first_backend.set_health_observation(vec![observed.clone()]);
    first_service.observe_health_for_test().await;
    let healthy = first_service.status().await.unwrap();
    assert_eq!(healthy.health_freshness(), FolderHealthFreshness::Fresh);
    assert_eq!(healthy.folder_health(), std::slice::from_ref(&observed));
    first_backend.set_health_failure(true);
    first_service.observe_health_for_test().await;
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
        access_unavailable: false,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 1,
        remaining_bytes: 7,
        scan_pull_error_count: 1,
        reported_error_rows: 1,
        status_error: true,
        watch_error: false,
    };
    first_backend.set_health_observation(vec![observed.clone()]);
    first_service.observe_health_for_test().await;
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
async fn authenticated_remote_removal_restarts_each_side_once_and_keeps_unrelated_folder() {
    let first = Device::new("Mac", 44277);
    let second = Device::new("Docker", 44278);
    pair(&first, &second);
    let first_other = first.root.join("other-files");
    let second_other = second.root.join("other-files");
    std::fs::create_dir(&first_other).unwrap();
    std::fs::create_dir(&second_other).unwrap();
    std::fs::write(first.files().join("keep-source.txt"), b"source").unwrap();
    std::fs::write(second.files().join("keep-target.txt"), b"target").unwrap();
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&first_backend));
    let target = second.service(second.journal(), Arc::clone(&second_backend));
    let first_folder = Uuid::new_v4();
    let second_folder = Uuid::new_v4();
    let (removed_offer, _) = make_ready_folder_at(
        &second,
        &source,
        &target,
        first_folder,
        &first.files(),
        &second.files(),
    )
    .await;
    make_ready_folder_at(
        &second,
        &source,
        &target,
        second_folder,
        &first_other,
        &second_other,
    )
    .await;
    let source_before = first_backend.snapshot();
    let target_before = second_backend.snapshot();

    source.remove(removed_offer).await.unwrap();
    let source_after_remove = first_backend.snapshot();
    assert_eq!(source_after_remove.launches, source_before.launches + 1);
    assert_eq!(source_after_remove.launched_folder_counts.last(), Some(&1));
    source.remove(removed_offer).await.unwrap();
    assert_eq!(
        first_backend.snapshot().launches,
        source_after_remove.launches
    );
    let status = source.status().await.unwrap();
    assert!(
        status
            .shares()
            .iter()
            .any(|share| { share.offer_id == removed_offer && share.remote_removal_pending })
    );
    let notice = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::Removal(notice) => Some(notice),
            _ => None,
        })
        .unwrap();

    target.receive_removal(&notice).await.unwrap();
    let target_after = second_backend.snapshot();
    assert_eq!(target_after.launches, target_before.launches + 1);
    assert_eq!(target_after.launched_folder_counts.last(), Some(&1));
    target.receive_removal(&notice).await.unwrap();
    assert_eq!(second_backend.snapshot().launches, target_after.launches);
    assert!(
        target
            .outbound_records()
            .await
            .unwrap()
            .into_value()
            .iter()
            .all(|delivery| !matches!(delivery.record, FolderShareRecord::Removal(_)))
    );

    let launches = first_backend.snapshot().launches;
    source
        .acknowledge_removal(second.engine.device_id(), removed_offer)
        .await
        .unwrap();
    assert_eq!(first_backend.snapshot().launches, launches);
    assert!(
        !source
            .status()
            .await
            .unwrap()
            .shares()
            .iter()
            .any(|share| { share.offer_id == removed_offer && share.remote_removal_pending })
    );
    assert_eq!(
        std::fs::read(first.files().join("keep-source.txt")).unwrap(),
        b"source"
    );
    assert_eq!(
        std::fs::read(second.files().join("keep-target.txt")).unwrap(),
        b"target"
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
    first_service.observe_health_for_test().await;
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

#[tokio::test]
async fn sustained_status_polling_does_not_probe_or_block_a_second_folder_offer() {
    let first = Device::new("Mac", 44301);
    let second = Device::new("Server", 44302);
    pair(&first, &second);
    let first_backend = Arc::new(TestBackend::default());
    let second_backend = Arc::new(TestBackend::default());
    let first_service = Arc::new(first.service(first.journal(), Arc::clone(&first_backend)));
    let second_service = Arc::new(second.service(second.journal(), Arc::clone(&second_backend)));
    make_ready(&first, &second, &first_service, &second_service).await;
    first_service.observe_health_for_test().await;
    second_service.observe_health_for_test().await;
    let before = (
        first_backend.snapshot().health_calls,
        second_backend.snapshot().health_calls,
    );
    let poll_source = Arc::clone(&first_service);
    let poll_target = Arc::clone(&second_service);
    let polling = tokio::spawn(async move {
        for _ in 0..500 {
            for service in [&poll_source, &poll_target] {
                match service.status().await {
                    Ok(_) | Err(FolderSyncServiceError::Busy) => {}
                    Err(error) => panic!("unexpected polling failure: {error:?}"),
                }
            }
            tokio::task::yield_now().await;
        }
    });
    let another_root = first.root.join("another-folder");
    std::fs::create_dir(&another_root).unwrap();
    let offer = first_service
        .offer(
            second.engine.device_id(),
            Uuid::new_v4(),
            "Second folder",
            &another_root,
            4000,
        )
        .await
        .unwrap()
        .into_value();
    second_service
        .receive_offer(offer.clone(), 4001)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), polling)
        .await
        .unwrap()
        .unwrap();
    let snapshot = second_service.status().await.unwrap();
    assert_eq!(snapshot.shares().len(), 2);
    assert!(
        snapshot
            .shares()
            .iter()
            .any(|share| share.offer_id == offer.offer_id)
    );
    assert_eq!(
        (
            first_backend.snapshot().health_calls,
            second_backend.snapshot().health_calls
        ),
        before
    );
    assert_eq!(first_backend.snapshot().launches, 1);
    assert_eq!(second_backend.snapshot().launches, 1);
}

#[tokio::test]
async fn folder_offer_api_preserves_shared_pause_and_rejects_other_setting_mismatches() {
    use axum::{extract::State, http::HeaderMap};
    use serde_json::json;

    let first = Device::new("Source", 44511);
    let second = Device::new("First target", 44512);
    let third = Device::new("Second target", 44513);
    pair(&first, &second);
    pair(&first, &third);
    let backend = Arc::new(TestBackend::default());
    let source = Arc::new(first.service(first.journal(), Arc::clone(&backend)));
    let token = "paused-fanout-test-token-with-32-bytes";
    let state = crate::AppState::new(
        Arc::clone(&first.engine),
        crate::PlatformTier::Tier1,
        token.into(),
    )
    .unwrap()
    .with_folder_sync(FolderSyncRuntimeState::Ready(Arc::clone(&source)));
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let folder = Uuid::new_v4();
    let mut request = json!({
        "peerId": second.engine.device_id(), "folderId": folder,
        "label": "Photos", "selectedRoot": first.files(),
        "linkPolicy": {"propagateSourceDeletions": false, "restoreLocalDeletions": false},
        "cadence": {"mode": "manual"},
        "androidConditions": {"wifiOnly": false, "chargingOnly": false}
    });
    let offer = |request| {
        crate::sync_api::offer(
            State(state.clone()),
            headers.clone(),
            crate::ContractJson(serde_json::from_value(request).unwrap()),
        )
    };
    let _ = offer(request.clone()).await.unwrap();
    let initial = source.status().await.unwrap().shares()[0]
        .link_settings
        .clone()
        .unwrap();
    assert!(!initial.settings.paused);
    let paused = FolderLinkSettings {
        paused: true,
        ..initial.settings
    };
    source
        .request_link_settings(folder, Uuid::new_v4(), initial.revision, paused)
        .await
        .unwrap();

    request["peerId"] = json!(third.engine.device_id());
    let mut mismatched = request.clone();
    mismatched["cadence"] = json!({"mode": "continuous"});
    assert_eq!(
        offer(mismatched)
            .await
            .err()
            .expect("settings mismatch")
            .code,
        "link_settings_conflict"
    );
    assert_eq!(source.status().await.unwrap().shares().len(), 1);
    let _ = offer(request.clone()).await.unwrap();
    let snapshot = source.status().await.unwrap();
    assert_eq!(snapshot.shares().len(), 2);
    assert!(snapshot.shares().iter().all(|share| {
        share
            .link_settings
            .as_ref()
            .is_some_and(|settings| settings.settings == paused)
    }));
    assert_eq!(snapshot.lifecycle(), FolderSyncLifecycle::Stopped);
    assert_eq!(backend.snapshot().launches, 0);

    let new_root = first.root.join("another-source");
    std::fs::create_dir(&new_root).unwrap();
    let new_folder = Uuid::new_v4();
    request["folderId"] = json!(new_folder);
    request["selectedRoot"] = json!(new_root);
    let _ = offer(request).await.unwrap();
    let snapshot = source.status().await.unwrap();
    let fresh = snapshot
        .shares()
        .iter()
        .find(|share| share.folder_id == new_folder)
        .unwrap();
    assert!(!fresh.link_settings.as_ref().unwrap().settings.paused);
}

#[tokio::test]
async fn manual_batch_waits_for_fresh_scan_and_exact_completion_then_reaps_workers() {
    use super::connection::EnginePeerConnectionState;
    use super::run_observation::EngineRunObservation;
    let first = Device::new("Source", 44501);
    let second = Device::new("Target", 44502);
    pair(&first, &second);
    let source_backend = Arc::new(TestBackend::default());
    let target_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), Arc::clone(&source_backend));
    let target = second.service(second.journal(), Arc::clone(&target_backend));
    let folder = Uuid::new_v4();
    let offer = source
        .offer_with_settings(
            second.engine.device_id(),
            folder,
            "Photos",
            &first.files(),
            2000,
            FolderLinkSettings {
                deletion_policy: covalent_protocol::FolderLinkPolicy::default(),
                paused: false,
                cadence: LinkCadence::Manual,
                android_conditions: AndroidLinkConditions::default(),
            },
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
    target.receive_commit(offer.offer_id, commit).await.unwrap();
    let settings = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::SettingsCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    target
        .receive_link_settings_commit(&settings)
        .await
        .unwrap();
    for service in [&source, &target] {
        service.start().await.unwrap();
        service.advance_runs_for_test().await;
        assert_eq!(
            service.status().await.unwrap().lifecycle(),
            FolderSyncLifecycle::Stopped
        );
    }
    assert_eq!(source_backend.snapshot().launches, 0);
    assert_eq!(target_backend.snapshot().launches, 0);

    let scan = Arc::new(Notify::new());
    source_backend.push_scan(TestScanBehavior::Wait(Arc::clone(&scan)));
    let request_id = Uuid::new_v4();
    let LinkRunAdmission::Pending(request) = target
        .request_link_run(folder, request_id, 0, settings.revision)
        .await
        .unwrap()
        .into_value()
    else {
        panic!("destination must request source admission")
    };
    let preparing = source
        .receive_link_run_request(&request)
        .await
        .unwrap()
        .into_value();
    assert_eq!(
        preparing.state.as_ref().unwrap().phase,
        LinkRunPhase::Preparing
    );
    target.receive_link_run_commit(&preparing).await.unwrap();
    assert_eq!(target_backend.snapshot().launches, 0);
    assert_eq!(source_backend.snapshot().scan_calls, 1);
    let proof = EngineIndexSnapshot {
        index_id: "0x0123456789ABCDEF".into(),
        sequence: 7,
    };
    let health = FolderHealth {
        folder,
        lifecycle: FolderLifecycle::Idle,
        access_unavailable: false,
        state_changed: OffsetDateTime::UNIX_EPOCH,
        remaining_files: 3,
        remaining_bytes: 90,
        scan_pull_error_count: 0,
        reported_error_rows: 0,
        status_error: false,
        watch_error: false,
    };
    source_backend.set_health_observation(vec![health.clone()]);
    source_backend.set_run_observations(
        folder,
        vec![EngineRunObservation {
            local_index: Some(proof.clone()),
            completions: BTreeMap::new(),
            failures: BTreeSet::new(),
        }],
    );
    source.advance_runs_for_test().await;
    assert_eq!(
        source.status().await.unwrap().shares()[0]
            .link_run
            .as_ref()
            .unwrap()
            .phase,
        Some(LinkRunPhase::Preparing)
    );
    scan.notify_one();
    tokio::task::yield_now().await;
    // Different healthy samples can straddle an index update. They cannot start a batch.
    source_backend.set_run_observations(
        folder,
        vec![
            EngineRunObservation {
                local_index: Some(proof.clone()),
                completions: BTreeMap::new(),
                failures: BTreeSet::new(),
            },
            EngineRunObservation {
                local_index: Some(EngineIndexSnapshot {
                    sequence: 8,
                    ..proof.clone()
                }),
                completions: BTreeMap::new(),
                failures: BTreeSet::new(),
            },
        ],
    );
    source.observe_health_for_test().await;
    source.advance_runs_for_test().await;
    assert_eq!(
        source.status().await.unwrap().shares()[0]
            .link_run
            .as_ref()
            .unwrap()
            .phase,
        Some(LinkRunPhase::Preparing)
    );
    source_backend.set_run_observations(
        folder,
        vec![EngineRunObservation {
            local_index: Some(proof.clone()),
            completions: BTreeMap::new(),
            failures: BTreeSet::new(),
        }],
    );
    source.advance_runs_for_test().await;
    let running = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    assert_eq!(running.state.as_ref().unwrap().phase, LinkRunPhase::Running);
    assert_eq!(
        source
            .receive_link_run_request(&request)
            .await
            .unwrap()
            .into_value()
            .state,
        running.state
    );
    assert_eq!(source_backend.snapshot().launches, 1);

    target.receive_link_run_commit(&running).await.unwrap();
    target_backend.set_health_observation(vec![health]);
    target_backend.set_connection_states(vec![EnginePeerConnectionState::Connected]);
    target_backend.set_run_observations(folder, vec![EngineRunObservation::default()]);
    target.advance_runs_for_test().await;
    assert_eq!(target_backend.snapshot().active, 1);
    let mut wrong_index = proof.clone();
    wrong_index.index_id = "0x1123456789ABCDEF".into();
    target_backend.set_run_observations(
        folder,
        vec![EngineRunObservation {
            local_index: None,
            completions: BTreeMap::from([(first.installation.device_id().clone(), wrong_index)]),
            failures: BTreeSet::new(),
        }],
    );
    target.advance_runs_for_test().await;
    assert_eq!(target_backend.snapshot().active, 1);
    target_backend.set_run_observations(
        folder,
        vec![EngineRunObservation {
            local_index: None,
            completions: BTreeMap::from([(first.installation.device_id().clone(), proof.clone())]),
            failures: BTreeSet::new(),
        }],
    );
    target_backend.set_connection_states(vec![EnginePeerConnectionState::Disconnected]);
    target.advance_runs_for_test().await;
    assert_eq!(
        target_backend.snapshot().active,
        0,
        "exact durable completion stops target before report ACK"
    );
    let report = target
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunReport(report) => Some(report),
            _ => None,
        })
        .unwrap();
    assert_eq!(report.result, LinkRunDestinationResult::Succeeded);
    assert_eq!(report.source_index, proof);
    source.receive_link_run_report(&report).await.unwrap();
    assert_eq!(source_backend.snapshot().active, 0);
    let done = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::RunCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    assert_eq!(done.state.as_ref().unwrap().phase, LinkRunPhase::Succeeded);
    target.receive_link_run_commit(&done).await.unwrap();
    assert!(
        !target
            .outbound_records()
            .await
            .unwrap()
            .into_value()
            .iter()
            .any(|delivery| matches!(delivery.record, FolderShareRecord::RunReport(_)))
    );
    for service in [&source, &target] {
        service.stop().await.unwrap();
        service.advance_runs_for_test().await;
    }
    assert_eq!(source_backend.snapshot().launches, 1);
    assert_eq!(target_backend.snapshot().launches, 1);
    assert_eq!(source_backend.snapshot().maximum_active, 1);
    assert_eq!(target_backend.snapshot().maximum_active, 1);
    assert!(first.files().exists() && second.files().exists());
}

#[tokio::test]
async fn android_continuous_conditions_stop_pending_scans_and_never_override_explicit_stop() {
    let first = Device::new("Source", 44511);
    let second = Device::new("Android target", 44512);
    pair(&first, &second);
    let source_backend = Arc::new(TestBackend::default());
    let target_backend = Arc::new(TestBackend::default());
    let source = first.service(first.journal(), source_backend);
    let mut target_journal = second.journal();
    target_journal.set_android_host(true);
    let target = second.service(target_journal, Arc::clone(&target_backend));
    let folder = Uuid::new_v4();
    let offer = source
        .offer_with_settings(
            second.engine.device_id(),
            folder,
            "Photos",
            &first.files(),
            3000,
            FolderLinkSettings {
                deletion_policy: covalent_protocol::FolderLinkPolicy::default(),
                paused: false,
                cadence: LinkCadence::Continuous,
                android_conditions: AndroidLinkConditions {
                    wifi_only: true,
                    charging_only: false,
                },
            },
        )
        .await
        .unwrap()
        .into_value();
    target.receive_offer(offer.clone(), 3001).await.unwrap();
    let acceptance = target
        .accept(offer.offer_id, &second.files(), 3002)
        .await
        .unwrap()
        .into_value();
    let commit = source
        .receive_acceptance(offer.offer_id, acceptance, 3003)
        .await
        .unwrap()
        .into_value();
    target.receive_commit(offer.offer_id, commit).await.unwrap();
    let settings = source
        .outbound_records()
        .await
        .unwrap()
        .into_value()
        .into_iter()
        .find_map(|delivery| match delivery.record {
            FolderShareRecord::SettingsCommit(commit) => Some(commit),
            _ => None,
        })
        .unwrap();
    target
        .receive_link_settings_commit(&settings)
        .await
        .unwrap();
    assert_eq!(
        target.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(target_backend.snapshot().launches, 0);

    target_backend.push_scan(TestScanBehavior::Complete);
    target
        .observe_android_conditions(true, false)
        .await
        .unwrap();
    target.advance_runs_for_test().await;
    assert_eq!(
        target.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Running
    );
    assert_eq!(target_backend.snapshot().promotions, 1);
    target
        .observe_android_conditions(false, false)
        .await
        .unwrap();
    assert_eq!(
        target.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(target_backend.snapshot().active, 0);

    let scan = Arc::new(Notify::new());
    target_backend.push_scan(TestScanBehavior::Wait(scan));
    assert_eq!(
        target
            .observe_android_conditions(true, false)
            .await
            .unwrap(),
        FolderSyncLifecycle::InitialScanning
    );
    assert_eq!(target_backend.snapshot().active, 1);
    target.expire_android_conditions_for_test().await;
    target.advance_runs_for_test().await;
    let expired = target_backend.snapshot();
    assert_eq!(
        target.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(expired.active, 0);
    assert_eq!(
        expired.promotions, 1,
        "expired scan must not promote a peer"
    );

    target.stop().await.unwrap();
    target
        .observe_android_conditions(true, false)
        .await
        .unwrap();
    target.advance_runs_for_test().await;
    let stopped = target_backend.snapshot();
    assert_eq!(
        target.status().await.unwrap().lifecycle(),
        FolderSyncLifecycle::Stopped
    );
    assert_eq!(
        stopped.launches, 2,
        "conditions must not override explicit stop"
    );
    source.stop().await.unwrap();
}
